//! The custom curve editor (GX-011): a compact bezier graph for the
//! selected keys' easing, and bezier handles drawn on the value graph.
//!
//! The inspector shows the editor whenever every selected key shares a
//! custom curve ("Custom curve…" in the easing picker converts the current
//! easing into one): a unit square (time left to right, value bottom to
//! top) with two draggable handles that may overshoot within the engine's
//! limits, snapping onto quarters and thirds (shift moves freely), a row
//! of shapes to start from, the four numbers as fields, and a dot that
//! loops along the curve to preview its pace.
//!
//! On the value graph, every selected key that starts a segment on the
//! active row shows its curve's handles in the graph's own frame (time and
//! value), After Effects style: dragging one shapes that key's easing
//! (influence sideways, speed by slope). Every handle drag is one
//! [`AnimDocument::begin_curve_drag`] session, so one undo step.

use egui::{
    Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Painter, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};
use fm_animation::{CubicBezier, Easing, KeyRef};
use fm_ui_model::animation_editor::curve::{
    bezier_point, handle_point, sample_curve, unit_controls, with_control,
};
use fm_ui_model::animation_editor::{
    AnimDocument, CurveControl, CurveHandle, CurvePreset, CurveSnap, CurveTarget, EditorMode,
    MAX_CURVE_Y, MIN_CURVE_Y, Segment, curve_seed,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::{AnimationTimelineState, text};
use crate::gating::Hint;
use crate::theme::{
    AMBER, accent_color, border_color, field_label, legible, muted_color, small_button, text_color,
    text_dim_color,
};

/// Size of the editor's graph.
pub const CURVE_EDITOR_SIZE: egui::Vec2 = egui::Vec2::new(132.0, 132.0);
/// Radius of a drawn handle; the grab area is a little larger.
const HANDLE_RADIUS: f32 = 4.5;
const HANDLE_GRAB: f32 = 14.0;
/// Snap distance in screen pixels.
const SNAP_PX: f64 = 6.0;
/// One preview loop: travel, then a short rest at the end.
const PREVIEW_TRAVEL: f64 = 1.2;
const PREVIEW_PERIOD: f64 = 1.6;
/// Smallest on-screen value span of a graph segment before handles use a
/// nominal height (a flat segment has none).
const MIN_SEGMENT_HEIGHT: f32 = 8.0;

/// Where the editor's parts were drawn on the last frame, for callers and
/// tests.
#[derive(Clone, Debug, Default)]
pub struct CurveLayout {
    /// The editor graph, when shown.
    pub editor: Option<CurveFrame>,
    /// The numeric fields.
    pub fields: Vec<(CurveControl, Rect)>,
    /// The "start from" shapes.
    pub presets: Vec<(CurvePreset, Rect)>,
    /// The easing picker (Advanced mode).
    pub easing_picker: Option<Rect>,
    /// The easing picker's "Custom curve…" item, while the picker is open.
    pub custom_item: Option<Rect>,
    /// Handles on the value graph: the segment's first key frame, the
    /// handle, and its centre.
    pub graph_handles: Vec<(u32, CurveHandle, Pos2)>,
}

/// Maps unit curve coordinates to the screen: `x` over the rect's width,
/// `y` from `low` (bottom) to `high` (top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveFrame {
    pub rect: Rect,
    pub low: f64,
    pub high: f64,
}

impl CurveFrame {
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // Screen coordinates.
    pub fn to_screen(&self, [x, y]: [f64; 2]) -> Pos2 {
        pos2(
            self.rect.left() + (x as f32) * self.rect.width(),
            self.rect.bottom()
                - ((y - self.low) / (self.high - self.low)) as f32 * self.rect.height(),
        )
    }

    #[must_use]
    pub fn to_unit(&self, position: Pos2) -> [f64; 2] {
        [
            f64::from((position.x - self.rect.left()) / self.rect.width()),
            self.low
                + f64::from((self.rect.bottom() - position.y) / self.rect.height())
                    * (self.high - self.low),
        ]
    }

    /// The snap distance in unit coordinates.
    fn snap_threshold(&self) -> f64 {
        SNAP_PX / f64::from(self.rect.width().min(self.rect.height()).max(1.0))
            * (self.high - self.low).max(1.0)
    }
}

/// The value range the editor shows for `curve`: the unit square plus a
/// margin, widened to keep both handles in view.
fn editor_range(curve: CubicBezier) -> (f64, f64) {
    let [_, y1, _, y2] = unit_controls(curve);
    (
        (y1.min(y2).min(-0.2) - 0.1).max(MIN_CURVE_Y - 0.05),
        (y1.max(y2).max(1.2) + 0.1).min(MAX_CURVE_Y + 0.05),
    )
}

/// The shared custom curve of the selection, if every selected key has
/// the same [`Easing::CubicBezier`].
fn shared_curve<T: Copy + Ord>(document: &AnimDocument<T>) -> Option<CubicBezier> {
    let mut easings = document
        .selection()
        .iter()
        .filter_map(|key| document.clip().key(key.track, key.frame))
        .map(|key| key.easing);
    let first = easings.next()?;
    match first {
        Easing::CubicBezier(curve) if easings.all(|easing| easing == first) => Some(curve),
        _ => None,
    }
}

/// "Custom curve…" in the easing picker: converts the selected keys'
/// easing into an editable curve (one undo step) so the editor opens.
pub(super) fn custom_item<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    let current = shared_curve(document).is_some();
    let response = ui
        .selectable_label(current, l.text(Msg::AnimEaseCustomEllipsis))
        .on_hover_text(l.text(Msg::AnimEaseCustomHover));
    state.curve_layout.custom_item = Some(response.rect);
    if response.clicked() && !current {
        let result = document
            .curve(CurveTarget::Selection)
            .and_then(|curve| document.set_curve(CurveTarget::Selection, curve));
        if let Err(error) = result {
            state.show_error(&error);
        }
    }
}

/// Why curve controls are disabled, if they are.
fn disabled_reason<T: Copy + Ord>(
    l: Locale,
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
) -> Option<&'static str> {
    if let Some(lock) = state.lock_reason {
        Some(l.tr(lock))
    } else if document.drag().is_some_and(|drag| !drag.is_curve()) {
        Some(l.text(Msg::AnimReasonDrag))
    } else {
        None
    }
}

/// The custom curve editor for the selection, when it shares one.
pub(super) fn draw_editor<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    // A drag on the value graph turns a named easing into a custom curve
    // live; the editor keeps its visibility until the drag ends so the
    // graph under the pointer does not jump.
    let graph_drag = state.graph_range.is_some() && document.curve_drag().is_some();
    let was_shown = state.curve_layout.editor.is_some();
    state.curve_layout.editor = None;
    state.curve_layout.fields.clear();
    state.curve_layout.presets.clear();
    let stored = match shared_curve(document) {
        Some(_) if graph_drag && !was_shown => None,
        Some(curve) => Some(curve),
        None if graph_drag && was_shown => document.curve_drag().map(|(_, curve, _)| curve),
        None => None,
    };
    let Some(stored) = stored else {
        state.curve_edit = None;
        return;
    };
    let reason = disabled_reason(l, document, state);
    let enabled = reason.is_none();
    let reason = reason.unwrap_or("");
    // A field being typed or dragged previews its number before it is
    // committed.
    let shown = state.curve_edit.map_or(stored, |(control, value)| {
        with_control(stored, control, value)
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(enabled, |ui| {
            graph(ui, l, document, state, shown, reason);
        });
        ui.vertical(|ui| {
            ui.label(field_label(l.text(Msg::AnimEaseCustom)))
                .on_hover_text(l.text(Msg::AnimCurveEditorHover));
            ui.add_enabled_ui(enabled, |ui| {
                presets_row(ui, l, document, state, reason);
                fields(ui, l, document, state, stored, reason);
            });
            preview_bar(ui, l, shown);
        });
    });
}

/// The editor graph with its two handles and the preview dot.
fn graph<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    shown: CubicBezier,
    reason: &str,
) {
    let (outer, background) = ui.allocate_exact_size(CURVE_EDITOR_SIZE, Sense::click());
    let dragging = document.curve_drag();
    if dragging.is_none() {
        state.curve_view = None;
    }
    let (low, high) = state.curve_view.unwrap_or_else(|| editor_range(shown));
    let frame = CurveFrame {
        rect: outer.shrink2(vec2(10.0, 6.0)),
        low,
        high,
    };
    state.curve_layout.editor = Some(frame);
    let held = super::tracks::modifiers(ui);

    let mut snapped = CurveSnap::NONE;
    for handle in CurveHandle::ALL {
        let current = dragging.map_or(shown, |(_, curve, _)| curve);
        let center = frame.to_screen(handle_point(current, handle));
        let id = ui.id().with(("anim-curve-handle", handle_index(handle)));
        let response = ui.interact(
            Rect::from_center_size(center, egui::Vec2::splat(HANDLE_GRAB)),
            id,
            Sense::drag(),
        );
        if response.drag_started() && document.drag().is_none() {
            state.curve_edit = None;
            match document.begin_curve_drag(CurveTarget::Selection, handle) {
                Ok(start) => {
                    state.curve_view = Some(editor_range(start));
                    state.notice = None;
                }
                Err(error) => state.show_error(&error),
            }
        }
        if response.dragged()
            && document
                .curve_drag()
                .is_some_and(|(dragged, ..)| dragged == handle)
            && let Some(position) = response.interact_pointer_pos()
        {
            let frame = CurveFrame {
                low: state.curve_view.map_or(low, |range| range.0),
                high: state.curve_view.map_or(high, |range| range.1),
                ..frame
            };
            if let Ok(snap) =
                document.update_curve_drag(frame.to_unit(position), held, frame.snap_threshold())
            {
                snapped = snap;
            }
        }
        if response.drag_stopped() {
            finish_curve_drag(document, state);
        }
        let [x, y] = handle_point(current, handle);
        response.hint_in(l, &handle_hover(l, handle, x, y), reason);
    }

    let curve = document.curve_drag().map_or(shown, |(_, curve, _)| curve);
    let painter = ui.painter_at(outer);
    paint_editor(
        &painter,
        frame,
        curve,
        snapped,
        ui.input(|input| input.time),
    );
    background.hint_in(l, l.text(Msg::AnimCurveEditorHover), reason);
    ui.ctx().request_repaint();
}

const fn handle_index(handle: CurveHandle) -> u8 {
    match handle {
        CurveHandle::Out => 0,
        CurveHandle::In => 1,
    }
}

/// Commits the curve drag in progress (one undo step).
pub(super) fn finish_curve_drag<T: Copy + Ord>(
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    if document.curve_drag().is_some() {
        match document.commit_drag() {
            Ok(outcome) => {
                if let Some(error) = outcome.rejected {
                    state.show_error(&error);
                }
            }
            Err(error) => state.show_error(&error),
        }
    }
    state.curve_view = None;
    state.graph_range = None;
}

fn handle_hover(l: Locale, handle: CurveHandle, x: f64, y: f64) -> String {
    let msg = match handle {
        CurveHandle::Out => Msg::AnimCurveHandleOutHover,
        CurveHandle::In => Msg::AnimCurveHandleInHover,
    };
    l.format(
        msg,
        &[
            ("x", Arg::Str(&l.decimal(x, 2))),
            ("y", Arg::Str(&l.decimal(y, 2))),
        ],
    )
}

/// Paints the editor graph: grid, the eased curve, handles, snap guides,
/// and the preview dot.
fn paint_editor(
    painter: &Painter,
    frame: CurveFrame,
    curve: CubicBezier,
    snap: CurveSnap,
    time: f64,
) {
    let outer = painter.clip_rect();
    painter.rect_filled(outer, CornerRadius::same(3), crate::theme::MONITOR_BLACK);
    painter.rect_stroke(
        outer,
        CornerRadius::same(3),
        Stroke::new(1.0, border_color()),
        StrokeKind::Inside,
    );
    let square = Rect::from_two_pos(frame.to_screen([0.0, 0.0]), frame.to_screen([1.0, 1.0]));
    for quarter in [0.25_f32, 0.5, 0.75] {
        let x = square.left() + square.width() * quarter;
        let y = square.bottom() - square.height() * quarter;
        let faint = Stroke::new(1.0, border_color().gamma_multiply(0.5));
        painter.line_segment([pos2(x, square.top()), pos2(x, square.bottom())], faint);
        painter.line_segment([pos2(square.left(), y), pos2(square.right(), y)], faint);
    }
    painter.rect_stroke(
        square,
        CornerRadius::ZERO,
        Stroke::new(1.0, border_color()),
        StrokeKind::Middle,
    );
    painter.line_segment(
        [square.left_bottom(), square.right_top()],
        Stroke::new(1.0, muted_color().gamma_multiply(0.5)),
    );
    if let Some(x) = snap.unit_x() {
        let x = frame.to_screen([x, 0.0]).x;
        dashed(painter, [pos2(x, outer.top()), pos2(x, outer.bottom())]);
    }
    if let Some(y) = snap.unit_y() {
        let y = frame.to_screen([0.0, y]).y;
        dashed(painter, [pos2(outer.left(), y), pos2(outer.right(), y)]);
    }
    let points: Vec<Pos2> = (0..=48)
        .map(|step| frame.to_screen(bezier_point(curve, f64::from(step) / 48.0)))
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent_color())));
    for handle in CurveHandle::ALL {
        let anchor = frame.to_screen(handle.anchor());
        let center = frame.to_screen(handle_point(curve, handle));
        painter.line_segment([anchor, center], Stroke::new(1.0, text_dim_color()));
        painter.circle(
            center,
            HANDLE_RADIUS,
            accent_color(),
            Stroke::new(1.5, Color32::WHITE),
        );
    }
    let progress = preview_progress(time);
    let dot = frame.to_screen([progress, sample_curve(curve, progress)]);
    painter.circle_filled(dot, 3.5, AMBER);
}

fn dashed(painter: &Painter, points: [Pos2; 2]) {
    painter.add(Shape::dashed_line(
        &points,
        Stroke::new(1.0, AMBER),
        3.0,
        3.0,
    ));
}

/// Progress `0..=1` of the looping preview at clock `time`.
fn preview_progress(time: f64) -> f64 {
    (time.rem_euclid(PREVIEW_PERIOD) / PREVIEW_TRAVEL).min(1.0)
}

/// The "start from" shapes.
fn presets_row<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    reason: &str,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(text_dim(l.text(Msg::AnimCurveStartFrom)));
        for preset in CurvePreset::ALL {
            let response = ui
                .add(small_button(text::curve_preset_name(l, preset)))
                .hint_in(
                    l,
                    &l.format(
                        Msg::AnimCurvePresetHover,
                        &[("help", Arg::Str(text::curve_preset_hover(l, preset)))],
                    ),
                    reason,
                );
            state.curve_layout.presets.push((preset, response.rect));
            if response.clicked() {
                state.curve_edit = None;
                if let Err(error) = document.apply_curve_preset(CurveTarget::Selection, preset) {
                    state.show_error(&error);
                }
            }
        }
    });
}

fn text_dim(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(legible(10.0))
        .color(text_dim_color())
}

/// The four numbers; a typed or dragged number is committed (one undo step)
/// once the field lets go.
fn fields<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    stored: CubicBezier,
    reason: &str,
) {
    let mut commit = None;
    egui::Grid::new("anim-curve-fields")
        .spacing(vec2(4.0, 2.0))
        .show(ui, |ui| {
            for (index, control) in CurveControl::ALL.into_iter().enumerate() {
                ui.label(text_dim(control_label(control)));
                let mut value = match state.curve_edit {
                    Some((edited, value)) if edited == control => value,
                    _ => control.value(stored),
                };
                let (low, high) = control.range();
                let response = ui
                    .add(
                        DragValue::new(&mut value)
                            .range(low..=high)
                            .speed(0.005)
                            .min_decimals(2)
                            .max_decimals(3),
                    )
                    .hint_in(l, text::curve_control_hover(l, control), reason);
                state.curve_layout.fields.push((control, response.rect));
                if response.changed() {
                    state.curve_edit = Some((control, value));
                }
                let editing = response.dragged() || response.has_focus();
                if !editing
                    && let Some((edited, value)) = state.curve_edit
                    && edited == control
                {
                    commit = Some((control, value));
                }
                if index % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    if let Some((control, value)) = commit {
        state.curve_edit = None;
        if (value - control.value(stored)).abs() > f64::EPSILON
            && let Err(error) = document.set_curve_control(CurveTarget::Selection, control, value)
        {
            state.show_error(&error);
        }
    }
}

/// Notation of a curve number; the same in every language.
const fn control_label(control: CurveControl) -> &'static str {
    match control {
        CurveControl::X1 => "x1",
        CurveControl::Y1 => "y1",
        CurveControl::X2 => "x2",
        CurveControl::Y2 => "y2",
    }
}

/// A bar with a dot moving at the curve's pace (its eased value).
fn preview_bar(ui: &mut Ui, l: Locale, curve: CubicBezier) {
    let (rect, response) = ui.allocate_exact_size(vec2(150.0, 12.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let track = rect.shrink2(vec2(20.0, 4.0));
    painter.rect_filled(track, CornerRadius::same(2), crate::theme::raised_color());
    for end in [track.left(), track.right()] {
        painter.line_segment(
            [pos2(end, rect.top()), pos2(end, rect.bottom())],
            Stroke::new(1.0, border_color()),
        );
    }
    let eased = sample_curve(curve, preview_progress(ui.input(|input| input.time)));
    #[allow(clippy::cast_possible_truncation)] // Screen coordinates.
    let x = track.left() + track.width() * eased.clamp(-0.6, 1.6) as f32;
    painter.circle_filled(pos2(x, track.center().y), 4.0, AMBER);
    response.on_hover_text(l.text(Msg::AnimCurvePreviewHover));
}

/// Scale of the value graph a segment's handles are drawn in.
#[derive(Clone, Copy, Debug)]
pub(super) struct GraphScale {
    pub plot: Rect,
    /// Value at the bottom and top of the drawn range.
    pub low: f64,
    pub high: f64,
    pub top: f32,
    pub bottom: f32,
}

impl GraphScale {
    #[allow(clippy::cast_possible_truncation)] // Screen coordinates.
    fn y_of(&self, value: f64) -> f32 {
        self.bottom
            - ((value - self.low) / (self.high - self.low)) as f32 * (self.bottom - self.top)
    }
}

/// The selected segments of `row` whose handles the value graph shows.
fn shown_segments<T: Copy + Ord>(
    document: &AnimDocument<T>,
    row: fm_animation::TrackKey<T>,
) -> Vec<Segment<T>> {
    document
        .selection()
        .iter()
        .filter(|key| key.track == row)
        .filter_map(|key| document.segment(key))
        .filter(|segment| {
            !matches!(segment.easing, Easing::Hold | Easing::Step)
                && matches!(segment.start_value, fm_animation::Value::Scalar(_))
        })
        .collect()
}

/// The values the value graph's handles sit at, so its range keeps them in
/// view.
#[allow(clippy::cast_precision_loss)] // Display only.
pub(super) fn handle_values<T: Copy + Ord>(
    document: &AnimDocument<T>,
    row: fm_animation::TrackKey<T>,
) -> Vec<f64> {
    shown_segments(document, row)
        .into_iter()
        .flat_map(|segment| {
            let (fm_animation::Value::Scalar(from), fm_animation::Value::Scalar(to)) =
                (segment.start_value, segment.end_value)
            else {
                return Vec::new();
            };
            let [_, y1, _, y2] = unit_controls(curve_seed(segment.easing));
            let (from, span) = (from as f64, (to - from) as f64);
            vec![from + y1 * span, from + y2 * span]
        })
        .collect()
}

/// The handles of each selected segment on the value graph's `row`, with
/// their drags. `x_of` maps a frame to the screen.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn graph_handles<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    row: fm_animation::TrackKey<T>,
    scale: GraphScale,
    x_of: &dyn Fn(u32) -> f32,
    painter: &Painter,
) {
    state.curve_layout.graph_handles.clear();
    if document.mode() != EditorMode::Advanced {
        return;
    }
    let reason = disabled_reason(l, document, state);
    let enabled = reason.is_none();
    let reason = reason.unwrap_or("");
    let held = super::tracks::modifiers(ui);
    let segments = shown_segments(document, row);
    let rate = document.clip().timebase().rate();
    let fps = f64::from(rate.numerator()) / f64::from(rate.denominator());
    let (unit, divisor) = text::property_unit(row.property);
    for segment in segments {
        let (fm_animation::Value::Scalar(from), fm_animation::Value::Scalar(to)) =
            (segment.start_value, segment.end_value)
        else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        let start = pos2(x_of(segment.key.frame), scale.y_of(from as f64));
        #[allow(clippy::cast_precision_loss)]
        let mut end = pos2(x_of(segment.end_frame), scale.y_of(to as f64));
        if (end.y - start.y).abs() < MIN_SEGMENT_HEIGHT {
            end.y = start.y - scale.plot.height() * 0.35;
        }
        let to_screen = |[u, v]: [f64; 2]| {
            #[allow(clippy::cast_possible_truncation)]
            pos2(
                start.x + (u as f32) * (end.x - start.x),
                start.y + (v as f32) * (end.y - start.y),
            )
        };
        let to_unit = |position: Pos2| {
            [
                f64::from((position.x - start.x) / (end.x - start.x).max(1.0)),
                f64::from((position.y - start.y) / (end.y - start.y)),
            ]
        };
        let threshold = SNAP_PX
            / f64::from(
                (end.x - start.x)
                    .abs()
                    .min((end.y - start.y).abs())
                    .max(1.0),
            );
        let key = segment.key;
        let dragging_this = |document: &AnimDocument<T>| {
            document.drag().is_some_and(|drag| match drag.kind() {
                fm_ui_model::animation_editor::DragKind::Curve { keys, .. } => {
                    keys.len() == 1 && keys.contains(key.track, key.frame)
                }
                _ => false,
            })
        };
        for handle in CurveHandle::ALL {
            let curve = match document.curve_drag() {
                Some((_, curve, _)) if dragging_this(document) => curve,
                _ => curve_seed(segment.easing),
            };
            let center = to_screen(handle_point(curve, handle));
            let id = ui
                .id()
                .with(("anim-graph-handle", key.frame, handle_index(handle)));
            // A disabled child keeps the handle's help with the reason.
            let grab = Rect::from_center_size(center, egui::Vec2::splat(HANDLE_GRAB));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grab));
            if !enabled {
                child.disable();
            }
            let response = child.interact(grab, id, Sense::drag());
            if enabled && response.drag_started() && document.drag().is_none() {
                match document.begin_curve_drag(CurveTarget::Segment(key), handle) {
                    Ok(_) => {
                        state.graph_range = Some((scale.low, scale.high));
                        state.notice = None;
                    }
                    Err(error) => state.show_error(&error),
                }
            }
            if response.dragged()
                && dragging_this(document)
                && document
                    .curve_drag()
                    .is_some_and(|(dragged, ..)| dragged == handle)
                && let Some(position) = response.interact_pointer_pos()
            {
                let _ = document.update_curve_drag(to_unit(position), held, threshold);
            }
            if response.drag_stopped() {
                finish_curve_drag(document, state);
            }
            let curve = match document.curve_drag() {
                Some((_, curve, _)) if dragging_this(document) => curve,
                _ => curve,
            };
            let center = to_screen(handle_point(curve, handle));
            let anchor = match handle {
                CurveHandle::Out => start,
                CurveHandle::In => to_screen([1.0, 1.0]),
            };
            painter.line_segment(
                [anchor, center],
                Stroke::new(1.0, text_color().gamma_multiply(0.6)),
            );
            painter.circle(
                center,
                HANDLE_RADIUS,
                if response.hovered() || response.dragged() {
                    AMBER
                } else {
                    accent_color()
                },
                Stroke::new(1.5, Color32::WHITE),
            );
            state
                .curve_layout
                .graph_handles
                .push((key.frame, handle, center));
            let influence = Segment::<T>::influence(curve, handle) * 100.0;
            let speed = segment.speed_per_frame(curve, handle).map_or_else(
                || l.text(Msg::AnimCurveSpeedInstant).to_owned(),
                |per_frame| {
                    l.format(
                        Msg::AnimCurveSpeedPerSecond,
                        &[(
                            "value",
                            Arg::Str(&text::format_number(l, per_frame * fps / divisor, 1, unit)),
                        )],
                    )
                },
            );
            let msg = match handle {
                CurveHandle::Out => Msg::AnimGraphHandleOutHover,
                CurveHandle::In => Msg::AnimGraphHandleInHover,
            };
            response.hint_in(
                l,
                &l.format(
                    msg,
                    &[
                        ("from", Arg::from(key.frame)),
                        ("to", Arg::from(segment.end_frame)),
                        ("influence", Arg::Str(&l.decimal(influence, 0))),
                        ("speed", Arg::Str(&speed)),
                    ],
                ),
                reason,
            );
        }
    }
}

/// Painted label for a snapped graph-handle drag (unused axes skipped).
pub(super) fn paint_graph_snap(painter: &Painter, l: Locale, plot: Rect) {
    painter.text(
        pos2(plot.left() + 6.0, plot.bottom() - 4.0),
        Align2::LEFT_BOTTOM,
        l.text(Msg::AnimCurveShiftHint),
        FontId::new(legible(9.5), FontFamily::Proportional),
        text_dim_color(),
    );
}

/// The segment's first key under a graph click, for selecting it.
pub(super) fn segment_key_at<T: Copy + Ord>(
    document: &AnimDocument<T>,
    row: fm_animation::TrackKey<T>,
    frame: u32,
) -> Option<KeyRef<T>> {
    document.segment_at(row, frame).map(|segment| segment.key)
}
