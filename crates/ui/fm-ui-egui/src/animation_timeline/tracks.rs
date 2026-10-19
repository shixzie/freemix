//! The time ruler and the rows: Simple-mode element bars and Advanced-mode
//! property rows with keys, plus every pointer gesture on them.
//!
//! One click-and-drag sense covers the ruler and rows. What the press landed
//! on decides the gesture: the ruler scrubs the playhead, the phase-end grip
//! stretches the phase, the selection strip's ends retime the selection, a
//! key moves the selection, an element bar moves (body) or retimes (ends)
//! that element, and empty row space box-selects (Advanced) or scrubs
//! (Simple). Each drag goes through the core's `begin_drag`/`update_drag`/
//! `commit_drag`, so a whole drag is one undo step.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};
use fm_animation::{AnimationPreset, Easing, KeyRef, KeySelection, SnapKind, TrackKey};
use fm_ui_model::animation_editor::{
    AnimDocument, DragKind, DragStart, EditorMode, Modifiers, Pointer, RetimeHandle, StaggerPlan,
    TargetKind, TimelineView,
};

use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::{AnimationTimelineState, Notice, PresetRequest, text};
use crate::gating::Hint;
use crate::theme::{
    ACCENT_FILL, AMBER, ERROR, accent_color, border_color, border_strong_color, faded, legible,
    muted_color, raised_color, text_color, text_dim_color, widget_color,
};
use fm_ui_model::animation_editor::PresetSource;

/// Height of the time ruler.
pub const RULER_HEIGHT: f32 = 26.0;
/// Height of an Advanced-mode property row.
pub const ADVANCED_ROW_HEIGHT: f32 = 22.0;
/// Height of a Simple-mode element row.
pub const SIMPLE_ROW_HEIGHT: f32 = 30.0;
/// Width of the label column in Advanced mode.
pub const ADVANCED_LABEL_WIDTH: f32 = 170.0;
/// Width of the label column in Simple mode (name plus preset picker).
pub const SIMPLE_LABEL_WIDTH: f32 = 220.0;
/// Pointer distance, in points, that still hits a key or handle.
pub const HIT_RADIUS: f32 = 6.0;
/// Height of the selection strip at the bottom of the ruler.
const STRIP_HEIGHT: f32 = 7.0;
/// Half-size of a key glyph.
const KEY_RADIUS: f32 = 5.0;

/// Where the ruler and rows were laid out this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackGeometry {
    /// Label column plus ruler and rows.
    pub rect: Rect,
    /// The time ruler (track columns only).
    pub ruler: Rect,
    /// Left edge of the track columns: frame `scroll_frame` is drawn here.
    pub track_left: f32,
    /// Top of the first row.
    pub rows_top: f32,
    pub row_height: f32,
    /// Number of rows drawn.
    pub rows: usize,
}

impl TrackGeometry {
    /// Screen x of `frame`.
    #[must_use]
    pub fn x_of(&self, view: &TimelineView, frame: u32) -> f32 {
        self.track_left + view.x_of_frame(frame)
    }

    /// Screen y of the middle of `row`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Row counts are small.
    pub fn row_center(&self, row: usize) -> f32 {
        self.rows_top + (row as f32 + 0.5) * self.row_height
    }

    /// Width of the track columns.
    #[must_use]
    pub fn track_width(&self) -> f32 {
        (self.rect.right() - self.track_left).max(0.0)
    }

    /// The timeline pointer (frame and clamped row) at screen `position`.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )] // Clamped first; row counts are small.
    pub fn pointer(&self, view: &TimelineView, position: Pos2) -> Pointer {
        let row = ((position.y - self.rows_top) / self.row_height)
            .floor()
            .clamp(0.0, self.rows.saturating_sub(1) as f32) as usize;
        Pointer::new(view.frame_at_x(position.x - self.track_left), row)
    }

    fn row_rect(&self, row: usize) -> Rect {
        let top = self.row_center(row) - self.row_height / 2.0;
        Rect::from_min_max(
            pos2(self.track_left, top),
            pos2(self.rect.right(), top + self.row_height),
        )
    }

    pub(crate) fn label_rect(&self, row: usize) -> Rect {
        let top = self.row_center(row) - self.row_height / 2.0;
        Rect::from_min_max(
            pos2(self.rect.left(), top),
            pos2(self.track_left - 4.0, top + self.row_height),
        )
    }
}

/// What a pointer position is over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Hit<T> {
    Ruler,
    PhaseEnd,
    Strip(RetimeHandle),
    Key(KeyRef<T>),
    Bar { target: T, part: BarPart },
    Empty { row: usize },
    Outside,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BarPart {
    Start,
    Body,
    End,
}

/// A row: an element (Simple) or one of its properties (Advanced).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Row<T> {
    Target(T),
    Track(TrackKey<T>),
}

pub(super) fn rows<T: Copy + Ord>(document: &AnimDocument<T>) -> Vec<Row<T>> {
    match document.mode() {
        EditorMode::Advanced => document.rows().into_iter().map(Row::Track).collect(),
        EditorMode::Simple => {
            let mut targets: Vec<T> = document
                .targets()
                .iter()
                .map(|outline| outline.target)
                .collect();
            for (target, _, _) in document.target_spans() {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
            targets.into_iter().map(Row::Target).collect()
        }
    }
}

/// The keys of `target` in the shown phase.
#[must_use]
pub fn target_keys<T: Copy + Ord>(document: &AnimDocument<T>, target: T) -> KeySelection<T> {
    document
        .clip()
        .tracks()
        .iter()
        .filter(|track| track.key.target == target && track.key.phase == document.phase())
        .flat_map(|track| {
            track.keys.iter().map(move |key| KeyRef {
                track: track.key,
                frame: key.frame,
            })
        })
        .collect()
}

/// An element's name: its outline's (an unnamed outline is "Element N" by
/// its outline position), or "Element N" numbering the animated elements
/// without an outline in id order.
pub(super) fn target_name<T: Copy + Ord>(
    l: Locale,
    document: &AnimDocument<T>,
    target: T,
) -> String {
    if let Some(index) = document
        .targets()
        .iter()
        .position(|outline| outline.target == target && outline.name.trim().is_empty())
    {
        return l.format(Msg::AnimElementNumbered, &[("n", Arg::from(index + 1))]);
    }
    document.target(target).map_or_else(
        || {
            let mut unknown: Vec<T> = document
                .clip()
                .tracks()
                .iter()
                .map(|track| track.key.target)
                .filter(|candidate| document.target(*candidate).is_none())
                .collect();
            unknown.sort_unstable();
            unknown.dedup();
            unknown
                .iter()
                .position(|candidate| *candidate == target)
                .map_or_else(
                    || l.text(Msg::AnimElement).to_owned(),
                    |index| l.format(Msg::AnimElementNumbered, &[("n", Arg::from(index + 1))]),
                )
        },
        |outline| outline.name.clone(),
    )
}

/// The localized word for an outline kind.
pub(crate) fn kind_name(l: Locale, kind: TargetKind) -> &'static str {
    l.text(match kind {
        TargetKind::Text => Msg::AnimKindText,
        TargetKind::Shape => Msg::AnimKindShape,
        TargetKind::Image => Msg::AnimKindImage,
        TargetKind::Video => Msg::AnimKindVideo,
        TargetKind::Group => Msg::AnimKindGroup,
        TargetKind::Other => Msg::AnimKindOther,
    })
}

/// Width a kind icon takes before an element name.
const KIND_ICON_WIDTH: f32 = 16.0;

/// A small painted icon for an element kind, centred on `center`: a "T"
/// for text, a square for shapes, a framed mountain for images, a play
/// triangle for video, stacked squares for groups, and a dot otherwise.
pub(crate) fn paint_kind_icon(painter: &Painter, center: Pos2, kind: TargetKind, color: Color32) {
    let stroke = Stroke::new(1.2, color);
    let half = 5.0;
    let frame = Rect::from_center_size(center, vec2(half * 2.0, half * 2.0));
    match kind {
        TargetKind::Text => {
            painter.line_segment([frame.left_top(), frame.right_top()], stroke);
            painter.line_segment([frame.center_top(), frame.center_bottom()], stroke);
        }
        TargetKind::Shape => {
            painter.rect_filled(frame.shrink(1.0), CornerRadius::same(1), faded(color, 0.5));
            painter.rect_stroke(
                frame.shrink(1.0),
                CornerRadius::same(1),
                stroke,
                StrokeKind::Inside,
            );
        }
        TargetKind::Image => {
            painter.rect_stroke(frame, CornerRadius::same(1), stroke, StrokeKind::Inside);
            painter.add(Shape::line(
                vec![
                    pos2(frame.left() + 1.5, frame.bottom() - 2.0),
                    pos2(frame.left() + 4.0, frame.center().y),
                    pos2(frame.left() + 6.0, frame.bottom() - 3.5),
                    pos2(frame.right() - 1.5, frame.top() + 3.0),
                ],
                stroke,
            ));
        }
        TargetKind::Video => {
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(frame.left() + 1.0, frame.top()),
                    pos2(frame.right(), center.y),
                    pos2(frame.left() + 1.0, frame.bottom()),
                ],
                faded(color, 0.5),
                stroke,
            ));
        }
        TargetKind::Group => {
            let back = Rect::from_min_size(frame.min, vec2(7.0, 7.0));
            let front = Rect::from_min_size(frame.min + vec2(3.0, 3.0), vec2(7.0, 7.0));
            painter.rect_stroke(back, CornerRadius::ZERO, stroke, StrokeKind::Inside);
            painter.rect_filled(front, CornerRadius::ZERO, raised_color());
            painter.rect_stroke(front, CornerRadius::ZERO, stroke, StrokeKind::Inside);
        }
        TargetKind::Other => {
            painter.circle_stroke(center, 3.5, stroke);
        }
    }
}

/// `name`'s hover text with the element's kind on a second line, when the
/// document has an outline for it.
fn with_kind<T: Copy + Ord>(
    l: Locale,
    document: &AnimDocument<T>,
    target: T,
    text: String,
) -> String {
    match document.target(target) {
        Some(outline) => format!(
            "{text}\n{}",
            l.format(
                Msg::AnimTargetKindHover,
                &[("kind", Arg::Str(kind_name(l, outline.kind)))],
            )
        ),
        None => text,
    }
}

pub(super) fn modifiers(ui: &Ui) -> Modifiers {
    ui.input(|input| Modifiers {
        shift: input.modifiers.shift,
        alt: input.modifiers.alt,
        ctrl: input.modifiers.command || input.modifiers.ctrl,
    })
}

/// Lays out, draws, and handles input for the ruler and rows.
pub(super) fn draw<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> TrackGeometry {
    let rows = rows(document);
    let simple = document.mode() == EditorMode::Simple;
    let (label_width, row_height) = if simple {
        (SIMPLE_LABEL_WIDTH, SIMPLE_ROW_HEIGHT)
    } else {
        (ADVANCED_LABEL_WIDTH, ADVANCED_ROW_HEIGHT)
    };
    let shown_rows = rows.len().max(1);
    #[allow(clippy::cast_precision_loss)] // Row counts are small.
    let height = RULER_HEIGHT + shown_rows as f32 * row_height + 2.0;
    let width = ui.available_width().max(label_width + 120.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let track_left = rect.left() + label_width;
    let geometry = TrackGeometry {
        rect,
        ruler: Rect::from_min_max(
            pos2(track_left, rect.top()),
            pos2(rect.right(), rect.top() + RULER_HEIGHT),
        ),
        track_left,
        rows_top: rect.top() + RULER_HEIGHT,
        row_height,
        rows: rows.len(),
    };
    let area = Rect::from_min_max(geometry.ruler.min, rect.max);
    let response = ui.interact(area, ui.id().with("anim-tracks"), Sense::click_and_drag());

    scroll_and_zoom(ui, document, &geometry, &response);
    handle_pointer(ui, document, state, &geometry, &rows, &response);

    let painter = ui.painter_at(rect);
    paint_background(&painter, document, &geometry, &rows, state);
    paint_ruler(&painter, document, &geometry);
    labels(ui, l, document, state, &geometry, &rows);
    let track_painter = painter.with_clip_rect(area);
    for (index, row) in rows.iter().enumerate() {
        match *row {
            Row::Track(track) => paint_track(&track_painter, document, &geometry, index, track),
            Row::Target(target) => {
                paint_bar(&track_painter, l, document, &geometry, index, target);
            }
        }
    }
    if rows.is_empty() {
        painter.text(
            geometry.row_rect(0).center(),
            Align2::CENTER_CENTER,
            l.text(if simple {
                Msg::AnimNoElements
            } else {
                Msg::AnimNothingAnimated
            }),
            FontId::new(legible(11.0), FontFamily::Proportional),
            muted_color(),
        );
    }
    if let Some(plan) = &state.stagger_preview {
        paint_stagger_preview(&track_painter, document, &geometry, &rows, plan);
    }
    paint_overlays(&track_painter, l, document, &geometry, &response);

    // Help for what is under the pointer, or for the whole area when the
    // pointer is elsewhere (a forced tooltip, or a touch press).
    if document.drag().is_none() {
        let hover = response
            .hover_pos()
            .and_then(|position| {
                hover_text(
                    l,
                    document,
                    &rows,
                    hit(document, &geometry, &rows, position),
                )
            })
            .unwrap_or_else(|| l.text(Msg::AnimTracksHover).to_owned());
        response.on_hover_text(hover);
    }
    geometry
}

/// Ctrl+wheel (or pinch) zooms about the pointer; horizontal scrolling
/// (Shift+wheel, sideways swipes) pans in time.
fn scroll_and_zoom<T: Copy + Ord>(
    ui: &Ui,
    document: &mut AnimDocument<T>,
    geometry: &TrackGeometry,
    response: &Response,
) {
    let Some(position) = response.hover_pos() else {
        return;
    };
    let (zoom, scroll) = ui.input(|input| (input.zoom_delta(), input.smooth_scroll_delta));
    let view = document.view_mut();
    if (zoom - 1.0).abs() > f32::EPSILON {
        view.zoom_about(1.0 / zoom, position.x - geometry.track_left);
    }
    if scroll.x.abs() > 0.0 {
        view.scroll_by_px(-scroll.x);
        ui.ctx()
            .input_mut(|input| input.smooth_scroll_delta.x = 0.0);
    }
}

fn hit<T: Copy + Ord>(
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
    position: Pos2,
) -> Hit<T> {
    let view = document.view();
    if position.x < geometry.track_left || !geometry.rect.contains(position) {
        return Hit::Outside;
    }
    if position.y < geometry.ruler.bottom() {
        let end_x = geometry.x_of(view, document.phase_frames());
        if (position.x - end_x).abs() <= HIT_RADIUS {
            return Hit::PhaseEnd;
        }
        if document.mode() == EditorMode::Advanced
            && position.y >= geometry.ruler.bottom() - STRIP_HEIGHT - 2.0
            && let (Some(first), Some(last)) = (
                document.selection().first_frame(),
                document.selection().last_frame(),
            )
            && last > first
        {
            if (position.x - geometry.x_of(view, first)).abs() <= HIT_RADIUS {
                return Hit::Strip(RetimeHandle::SelectionStart);
            }
            if (position.x - geometry.x_of(view, last)).abs() <= HIT_RADIUS {
                return Hit::Strip(RetimeHandle::SelectionEnd);
            }
        }
        return Hit::Ruler;
    }
    let pointer = geometry.pointer(view, position);
    let Some(row) = rows.get(pointer.row) else {
        return Hit::Empty { row: pointer.row };
    };
    match *row {
        Row::Track(track) => {
            let nearest = document.clip().track(track).and_then(|keys| {
                keys.keys
                    .iter()
                    .map(|key| {
                        (
                            key.frame,
                            (geometry.x_of(view, key.frame) - position.x).abs(),
                        )
                    })
                    .filter(|(_, distance)| *distance <= HIT_RADIUS)
                    .min_by(|left, right| left.1.total_cmp(&right.1))
            });
            nearest.map_or(Hit::Empty { row: pointer.row }, |(frame, _)| {
                Hit::Key(KeyRef { track, frame })
            })
        }
        Row::Target(target) => {
            let Some((_, first, last)) = document
                .target_spans()
                .into_iter()
                .find(|span| span.0 == target)
            else {
                return Hit::Empty { row: pointer.row };
            };
            let (start, end) = bar_span(geometry, view, first, last);
            if (position.x - end).abs() <= HIT_RADIUS {
                Hit::Bar {
                    target,
                    part: BarPart::End,
                }
            } else if (position.x - start).abs() <= HIT_RADIUS && last > first {
                Hit::Bar {
                    target,
                    part: BarPart::Start,
                }
            } else if position.x > start && position.x < end {
                Hit::Bar {
                    target,
                    part: BarPart::Body,
                }
            } else {
                Hit::Empty { row: pointer.row }
            }
        }
    }
}

/// Screen x range of an element bar; at least two hit radii wide so both
/// ends stay grabbable.
fn bar_span(geometry: &TrackGeometry, view: &TimelineView, first: u32, last: u32) -> (f32, f32) {
    let start = geometry.x_of(view, first);
    let end = geometry
        .x_of(view, last)
        .max(start + 2.0 * HIT_RADIUS + 2.0);
    (start, end)
}

fn handle_pointer<T: Copy + Ord>(
    ui: &Ui,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
    response: &Response,
) {
    let held = modifiers(ui);
    if response.drag_started()
        && document.drag().is_none()
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
    {
        let pointer = geometry.pointer(document.view(), origin);
        let start = match hit(document, geometry, rows, origin) {
            Hit::Ruler => Some(DragStart::Playhead),
            Hit::PhaseEnd => Some(DragStart::Retime(RetimeHandle::PhaseEnd)),
            Hit::Strip(handle) => Some(DragStart::Retime(handle)),
            Hit::Key(key) => {
                state.active_row = Some(key.track);
                Some(DragStart::Key(key))
            }
            Hit::Bar { target, part } => {
                let keys = target_keys(document, target);
                let first = keys.iter().next();
                document.set_selection(&keys);
                match (part, first) {
                    (BarPart::Body, Some(first)) => Some(DragStart::Key(first)),
                    (BarPart::Start, _) => Some(DragStart::Retime(RetimeHandle::SelectionStart)),
                    (BarPart::End, _) => Some(DragStart::Retime(RetimeHandle::SelectionEnd)),
                    (BarPart::Body, None) => None,
                }
            }
            Hit::Empty { row } => match rows.get(row) {
                Some(Row::Track(track)) => {
                    state.active_row = Some(*track);
                    Some(DragStart::SelectionBox)
                }
                _ if document.mode() == EditorMode::Simple => Some(DragStart::Playhead),
                _ => Some(DragStart::SelectionBox),
            },
            Hit::Outside => None,
        };
        // A lock keeps scrubbing and box selection but no edit drags.
        let start = match (state.lock_reason, start) {
            (Some(reason), Some(DragStart::Key(_) | DragStart::Retime(_))) => {
                let locale = state.locale();
                state.notice = Some(Notice::hint(locale.tr(reason)));
                None
            }
            (_, start) => start,
        };
        // Key drags move by the pointer's travel from the press frame, so the
        // key follows the pointer without jumping by the hit slop.
        if let Some(start) = start {
            match document.begin_drag(start, pointer, held) {
                Ok(()) => state.notice = None,
                Err(error) => state.show_error(&error),
            }
        }
    }
    // A curve handle drag belongs to the inspector.
    if document
        .drag()
        .is_some_and(fm_ui_model::animation_editor::DragSession::is_curve)
    {
        return;
    }
    if document.drag().is_some() {
        if let Some(position) = ui.input(|input| input.pointer.interact_pos()) {
            let _ = document.update_drag(geometry.pointer(document.view(), position), held);
        }
        let released = response.drag_stopped() || !ui.input(|input| input.pointer.any_down());
        if released {
            match document.commit_drag() {
                Ok(outcome) => {
                    if let Some(error) = outcome.rejected {
                        state.show_error(&error);
                    }
                }
                Err(error) => state.show_error(&error),
            }
        }
        return;
    }
    if response.clicked()
        && let Some(position) = response.interact_pointer_pos()
    {
        click(document, state, geometry, rows, position, held);
    }
}

fn click<T: Copy + Ord>(
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
    position: Pos2,
    held: Modifiers,
) {
    match hit(document, geometry, rows, position) {
        Hit::Ruler | Hit::PhaseEnd => {
            let frame = geometry.pointer(document.view(), position).frame;
            document.set_playhead(frame);
        }
        Hit::Key(key) => {
            state.active_row = Some(key.track);
            if let Err(error) = document.click_key(key, held) {
                state.show_error(&error);
            }
        }
        Hit::Bar { target, .. } => {
            let keys = target_keys(document, target);
            document.set_selection(&keys);
        }
        Hit::Empty { row } => {
            if let Some(Row::Track(track)) = rows.get(row) {
                state.active_row = Some(*track);
            }
            document.click_empty(held);
        }
        Hit::Strip(_) | Hit::Outside => {}
    }
}

pub(super) fn hover_text<T: Copy + Ord>(
    l: Locale,
    document: &AnimDocument<T>,
    rows: &[Row<T>],
    hit: Hit<T>,
) -> Option<String> {
    // The Shift advice follows the Snapping toggle, which Shift inverts.
    let snapping = document.view().snapping();
    let pick = |on: Msg, off: Msg| if snapping { on } else { off };
    Some(match hit {
        Hit::Ruler => l
            .text(pick(Msg::AnimRulerHover, Msg::AnimRulerHoverSnapOff))
            .to_owned(),
        Hit::PhaseEnd => l.format(
            Msg::AnimPhaseEndHover,
            &[(
                "length",
                Arg::Str(&crate::l10n::frames(l, document.phase_frames())),
            )],
        ),
        Hit::Strip(_) => l
            .text(pick(Msg::AnimStripHover, Msg::AnimStripHoverSnapOff))
            .to_owned(),
        Hit::Key(key) => {
            let found = document.clip().key(key.track, key.frame)?;
            let info = l.format(
                Msg::AnimKeyInfo,
                &[
                    (
                        "target",
                        Arg::Str(&target_name(l, document, key.track.target)),
                    ),
                    (
                        "property",
                        Arg::Str(text::property_name(l, key.track.property)),
                    ),
                    ("frame", Arg::from(key.frame)),
                    (
                        "value",
                        Arg::Str(&text::format_value(l, key.track.property, found.value)),
                    ),
                    ("easing", Arg::Str(text::easing_name(l, found.easing))),
                    ("shape", Arg::Str(text::key_shape_name(l, found.easing))),
                ],
            );
            format!(
                "{info}\n{}",
                l.text(pick(Msg::AnimKeyDragHelp, Msg::AnimKeyDragHelpSnapOff))
            )
        }
        Hit::Bar { target, part } => {
            let name = target_name(l, document, target);
            let msg = match part {
                BarPart::Body => Msg::AnimBarBodyHover,
                BarPart::End | BarPart::Start => {
                    pick(Msg::AnimBarEndHover, Msg::AnimBarEndHoverSnapOff)
                }
            };
            l.format(msg, &[("name", Arg::Str(&name))])
        }
        Hit::Empty { row } => match rows.get(row) {
            Some(Row::Track(_)) => l.text(Msg::AnimRowEmptyHover).to_owned(),
            Some(Row::Target(target)) => l.format(
                Msg::AnimTargetEmptyHover,
                &[("name", Arg::Str(&target_name(l, document, *target)))],
            ),
            None => return None,
        },
        Hit::Outside => return None,
    })
}

/// Row stripes, the region past the phase end, and the active row marker.
fn paint_background<T: Copy + Ord>(
    painter: &Painter,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
    state: &AnimationTimelineState<T>,
) {
    painter.rect_filled(
        geometry.rect,
        CornerRadius::same(3),
        crate::theme::MONITOR_BLACK,
    );
    for row in 0..rows.len().max(1) {
        let stripe = if row % 2 == 0 {
            raised_color()
        } else {
            widget_color()
        };
        painter.rect_filled(geometry.row_rect(row), CornerRadius::ZERO, stripe);
        if let Some(Row::Track(track)) = rows.get(row)
            && state.active_row == Some(*track)
        {
            painter.rect_filled(
                geometry.row_rect(row).union(geometry.label_rect(row)),
                CornerRadius::ZERO,
                ACCENT_FILL.gamma_multiply(0.6),
            );
        }
    }
    let end = geometry.x_of(document.view(), document.phase_frames());
    if end < geometry.rect.right() {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(end.max(geometry.track_left), geometry.rect.top()),
                geometry.rect.max,
            ),
            CornerRadius::ZERO,
            Color32::from_black_alpha(110),
        );
    }
    painter.line_segment(
        [
            pos2(geometry.track_left, geometry.rect.top()),
            pos2(geometry.track_left, geometry.rect.bottom()),
        ],
        Stroke::new(1.0, border_strong_color()),
    );
}

/// Frames between labelled ruler ticks: the smallest frame-rate-friendly
/// step at least 64 points apart.
///
/// Below a second the step divides the (nominal) rate exactly, so every
/// second boundary carries a label and labels read evenly (`0:10`, `0:20`,
/// `1:00` at 30 fps; `0:05`, `0:10`, ... `1:00` at 25 fps). From a second up
/// it is a whole number of seconds: 1, 2, 5, 10, 15, 30 s, then 1, 2, 5, 10
/// minutes.
#[must_use]
pub fn ruler_step(frames_per_pixel: f32, fps: u32) -> u32 {
    let fps = fps.max(1);
    let mut candidates: Vec<u32> = [1, 2, 3, 5, 6, 10, 12, 15, 20, 30]
        .into_iter()
        .filter(|step| *step < fps && fps.is_multiple_of(*step))
        .collect();
    candidates.extend(
        [1, 2, 5, 10, 15, 30, 60, 120, 300, 600]
            .iter()
            .map(|seconds| seconds * fps),
    );
    #[allow(clippy::cast_precision_loss)] // Small steps.
    candidates
        .into_iter()
        .find(|step| *step as f32 / frames_per_pixel >= 64.0)
        .unwrap_or(600 * fps)
}

/// Frames between the unlabelled ticks inside one ruler `step`: the
/// coarsest division into at most six parts that stays frame-rate
/// friendly (whole seconds inside a multi-second step, an exact divisor of
/// the rate inside a second).
#[must_use]
pub fn ruler_minor_step(step: u32, fps: u32) -> u32 {
    let fps = fps.max(1);
    let step = step.max(1);
    let finest = |whole: u32| {
        (1..=whole)
            .find(|part| whole.is_multiple_of(*part) && whole / part <= 6)
            .unwrap_or(whole)
    };
    if step > fps && step.is_multiple_of(fps) {
        finest(step / fps) * fps
    } else {
        finest(step)
    }
}

/// A ruler label: `S:FF` under a minute, `M:SS:FF` above.
#[must_use]
pub fn ruler_label(frame: u32, fps: u32) -> String {
    let fps = fps.max(1);
    let seconds = frame / fps;
    let frames = frame % fps;
    if seconds >= 60 {
        format!("{}:{:02}:{frames:02}", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}:{frames:02}")
    }
}

fn nominal_fps<T: Copy + Ord>(document: &AnimDocument<T>) -> u32 {
    let rate = document.clip().timebase().rate();
    rate.numerator().div_ceil(rate.denominator()).max(1)
}

fn paint_ruler<T: Copy + Ord>(
    painter: &Painter,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
) {
    let ruler = geometry.ruler;
    painter.rect_filled(ruler, CornerRadius::ZERO, raised_color());
    painter.line_segment(
        [ruler.left_bottom(), ruler.right_bottom()],
        Stroke::new(1.0, border_color()),
    );
    let view = document.view();
    let fps = nominal_fps(document);
    let step = ruler_step(view.frames_per_pixel(), fps);
    let minor = ruler_minor_step(step, fps);
    #[allow(clippy::cast_precision_loss)]
    let minor_visible = minor as f32 / view.frames_per_pixel() >= 5.0;
    let (first, last) = view.visible_range(geometry.track_width());
    let painter = painter.with_clip_rect(ruler);
    let font = FontId::new(legible(9.5), FontFamily::Monospace);
    let mut frame = first - first % minor;
    while frame <= last {
        let x = geometry.x_of(view, frame);
        if frame.is_multiple_of(step) {
            painter.line_segment(
                [pos2(x, ruler.bottom() - 10.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, text_dim_color()),
            );
            painter.text(
                pos2(x + 3.0, ruler.top() + 2.0),
                Align2::LEFT_TOP,
                ruler_label(frame, fps),
                font.clone(),
                text_dim_color(),
            );
        } else if minor_visible {
            painter.line_segment(
                [pos2(x, ruler.bottom() - 4.0), pos2(x, ruler.bottom())],
                Stroke::new(1.0, faded(text_dim_color(), 0.6)),
            );
        }
        frame += minor;
    }
    // Phase-end grip.
    let end = geometry.x_of(view, document.phase_frames());
    let grip = Rect::from_center_size(pos2(end, ruler.center().y), vec2(5.0, ruler.height() - 6.0));
    painter.rect_filled(grip, CornerRadius::same(2), AMBER.gamma_multiply(0.8));
    // Selection strip with retime ends (Advanced).
    if document.mode() == EditorMode::Advanced
        && let (Some(first), Some(last)) = (
            document.selection().first_frame(),
            document.selection().last_frame(),
        )
        && last > first
    {
        let strip = Rect::from_min_max(
            pos2(geometry.x_of(view, first), ruler.bottom() - STRIP_HEIGHT),
            pos2(geometry.x_of(view, last), ruler.bottom() - 1.0),
        );
        painter.rect_filled(
            strip,
            CornerRadius::same(2),
            accent_color().gamma_multiply(0.35),
        );
        for x in [strip.left(), strip.right()] {
            painter.rect_filled(
                Rect::from_center_size(pos2(x, strip.center().y), vec2(4.0, STRIP_HEIGHT + 3.0)),
                CornerRadius::same(1),
                accent_color(),
            );
        }
    }
}

/// Row labels: element names with a preset picker (Simple) or target and
/// property names that make the row active on click (Advanced).
#[allow(clippy::too_many_lines)] // Both row kinds share one label pass.
fn labels<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
) {
    let font = FontId::new(legible(11.0), FontFamily::Proportional);
    let mut previous_target = None;
    let mut preset_choice = None;
    let mut preset_hover = None;
    for (index, row) in rows.iter().enumerate() {
        let rect = geometry.label_rect(index);
        match *row {
            Row::Track(track) => {
                let response = ui
                    .interact(
                        rect,
                        ui.id().with(("anim-row-label", index)),
                        Sense::click(),
                    )
                    .on_hover_text(format!(
                        "{}\n{}",
                        l.format(
                            Msg::AnimRowLabelInfo,
                            &[
                                ("target", Arg::Str(&target_name(l, document, track.target)),),
                                ("property", Arg::Str(text::property_name(l, track.property)),),
                                ("help", Arg::Str(text::property_hover(l, track.property))),
                            ],
                        ),
                        l.text(Msg::AnimRowLabelClick)
                    ));
                if response.clicked() {
                    state.active_row = Some(track);
                }
                let painter = ui.painter_at(rect);
                let first_of_target = previous_target != Some(track.target);
                previous_target = Some(track.target);
                if first_of_target {
                    painter.text(
                        pos2(rect.left() + 6.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        crate::theme::ellipsize(&target_name(l, document, track.target), 14),
                        font.clone(),
                        text_color(),
                    );
                }
                painter.text(
                    pos2(rect.right() - 4.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    text::property_name(l, track.property),
                    font.clone(),
                    if state.active_row == Some(track) {
                        accent_color()
                    } else {
                        text_dim_color()
                    },
                );
            }
            Row::Target(target) => {
                let name = target_name(l, document, target);
                let (text_rect, picker_rect) = {
                    let split = rect.right() - 104.0;
                    (
                        Rect::from_min_max(rect.min, pos2(split - 4.0, rect.bottom())),
                        Rect::from_min_max(
                            pos2(split, rect.top() + 3.0),
                            pos2(rect.right(), rect.bottom() - 3.0),
                        ),
                    )
                };
                ui.interact(
                    text_rect,
                    ui.id().with(("anim-target-label", index)),
                    Sense::hover(),
                )
                .on_hover_text(with_kind(
                    l,
                    document,
                    target,
                    l.format(Msg::AnimTargetLabelHover, &[("name", Arg::Str(&name))]),
                ));
                let painter = ui.painter_at(text_rect);
                let text_left = match document.target(target) {
                    Some(outline) => {
                        paint_kind_icon(
                            &painter,
                            pos2(text_rect.left() + 6.0 + 5.0, text_rect.center().y),
                            outline.kind,
                            text_dim_color(),
                        );
                        text_rect.left() + 6.0 + KIND_ICON_WIDTH
                    }
                    None => text_rect.left() + 6.0,
                };
                painter.text(
                    pos2(text_left, text_rect.center().y),
                    Align2::LEFT_CENTER,
                    crate::theme::ellipsize(&name, 16),
                    font.clone(),
                    text_color(),
                );
                let lock = state.lock_reason.map(|reason| l.tr(reason));
                let outline = document.target(target).is_some();
                let outside = match state.outside_presets {
                    _ if outline => None,
                    None => Some(l.text(Msg::AnimReasonUnknownTarget)),
                    Some(Ok(())) => None,
                    Some(Err(reason)) => Some(l.tr(reason)),
                };
                let enabled = lock.is_none() && document.drag().is_none() && outside.is_none();
                let reason = if let Some(lock) = lock {
                    lock
                } else if document.drag().is_some() {
                    l.text(Msg::AnimReasonDrag)
                } else {
                    outside.unwrap_or_default()
                };
                ui.scope_builder(UiBuilder::new().max_rect(picker_rect), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let combo = egui::ComboBox::from_id_salt(("anim-preset", index))
                            .selected_text(l.text(Msg::AnimPresetPick))
                            .width(picker_rect.width() - 8.0)
                            .show_ui(ui, |ui| {
                                for preset in AnimationPreset::ALL {
                                    let preset = PresetSource::BuiltIn(preset);
                                    let PresetSource::BuiltIn(builtin) = preset else {
                                        unreachable!("a built-in preset")
                                    };
                                    let item = ui
                                        .selectable_label(false, text::preset_name(l, builtin))
                                        .on_hover_text(if document.target(target).is_some() {
                                            l.format(
                                                Msg::AnimPresetItemHover,
                                                &[(
                                                    "help",
                                                    Arg::Str(text::preset_hover(l, builtin)),
                                                )],
                                            )
                                        } else {
                                            text::preset_hover(l, builtin).to_owned()
                                        });
                                    if item.hovered() {
                                        preset_hover = Some((target, preset.clone()));
                                    }
                                    if item.clicked() {
                                        preset_choice = Some((target, preset));
                                    }
                                }
                                // The project's own presets saved from this
                                // phase that animate something here.
                                let mine: Vec<_> = state
                                    .user_presets
                                    .iter()
                                    .filter(|item| {
                                        item.phase == document.phase()
                                            && item.motion.properties().any(|property| {
                                                state.properties.contains(&property)
                                            })
                                    })
                                    .collect();
                                if !mine.is_empty() {
                                    ui.separator();
                                    ui.label(crate::theme::section_label(
                                        l.text(Msg::AnimMyPresets),
                                    ));
                                }
                                for item in mine {
                                    let help = super::library::user_preset_hover(l, item);
                                    let response = ui
                                        .selectable_label(
                                            false,
                                            crate::theme::ellipsize(&item.name, 22),
                                        )
                                        .on_hover_text(if document.target(target).is_some() {
                                            l.format(
                                                Msg::AnimPresetItemHover,
                                                &[("help", Arg::Str(&help))],
                                            )
                                        } else {
                                            help
                                        });
                                    if response.hovered() {
                                        preset_hover = Some((target, item.source()));
                                    }
                                    if response.clicked() {
                                        preset_choice = Some((target, item.source()));
                                    }
                                }
                            });
                        combo.response.hint_in(
                            l,
                            &l.format(Msg::AnimPresetPickHover, &[("name", Arg::Str(&name))]),
                            reason,
                        );
                    });
                });
            }
        }
    }
    // A preset runs over the phase; into an empty phase a built-in brings
    // half a second and a user preset its own length.
    let duration = |preset: &PresetSource| {
        if document.phase_frames() > 0 {
            document.phase_frames()
        } else {
            match preset {
                PresetSource::BuiltIn(_) => (nominal_fps(document) / 2).max(1),
                PresetSource::User { motion, .. } => {
                    motion.frames_at(document.clip().timebase()).max(1)
                }
            }
        }
    };
    // Only presets that would apply here are previewed.
    state.preset_hover =
        preset_hover
            .filter(|_| preset_choice.is_none())
            .map(|(target, preset)| PresetRequest {
                target,
                phase: document.phase(),
                frames: duration(&preset),
                preset,
            });
    if let Some((target, preset)) = preset_choice {
        let frames = duration(&preset);
        if document.target(target).is_none() {
            state.preset_requests.push(PresetRequest {
                target,
                phase: document.phase(),
                preset,
                frames,
            });
            return;
        }
        let phase = document.phase();
        match document.apply_preset_source(target, phase, &preset, frames, state.properties) {
            Ok(()) => state.notice = None,
            Err(error) => state.notice = Some(Notice::error(l, &error)),
        }
    }
}

/// One property row: span lines between keys and the key glyphs.
fn paint_track<T: Copy + Ord>(
    painter: &Painter,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    row: usize,
    track: TrackKey<T>,
) {
    let Some(keys) = document.clip().track(track) else {
        return;
    };
    let view = document.view();
    let y = geometry.row_center(row);
    for pair in keys.keys.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let hold = matches!(from.easing, Easing::Hold | Easing::Step);
        let color = if hold {
            faded(text_dim_color(), 0.35)
        } else {
            faded(accent_color(), 0.55)
        };
        painter.line_segment(
            [
                pos2(geometry.x_of(view, from.frame), y),
                pos2(geometry.x_of(view, to.frame), y),
            ],
            Stroke::new(if hold { 1.0 } else { 2.0 }, color),
        );
    }
    for key in &keys.keys {
        let selected = document.selection().contains(track, key.frame);
        paint_key(
            painter,
            pos2(geometry.x_of(view, key.frame), y),
            key.easing,
            selected,
        );
    }
}

/// A key glyph: diamond (linear), square (hold), or circle (eased).
pub(crate) fn paint_key(painter: &Painter, center: Pos2, easing: Easing, selected: bool) {
    let fill = if selected {
        accent_color()
    } else {
        text_dim_color()
    };
    let stroke = Stroke::new(
        1.0,
        if selected {
            Color32::WHITE
        } else {
            crate::theme::MONITOR_BLACK
        },
    );
    let r = KEY_RADIUS;
    match easing {
        Easing::Linear => {
            painter.add(Shape::convex_polygon(
                vec![
                    center + vec2(0.0, -r),
                    center + vec2(r, 0.0),
                    center + vec2(0.0, r),
                    center + vec2(-r, 0.0),
                ],
                fill,
                stroke,
            ));
        }
        Easing::Hold | Easing::Step => {
            painter.rect(
                Rect::from_center_size(center, Vec2::splat(2.0 * r - 2.0)),
                CornerRadius::ZERO,
                fill,
                stroke,
                StrokeKind::Middle,
            );
        }
        _ => {
            painter.circle(center, r - 0.5, fill, stroke);
        }
    }
}

/// One element bar with grips at both ends.
fn paint_bar<T: Copy + Ord>(
    painter: &Painter,
    l: Locale,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    row: usize,
    target: T,
) {
    let row_rect = geometry.row_rect(row);
    let Some((_, first, last)) = document
        .target_spans()
        .into_iter()
        .find(|span| span.0 == target)
    else {
        painter.text(
            pos2(geometry.track_left + 8.0, row_rect.center().y),
            Align2::LEFT_CENTER,
            l.text(Msg::AnimBarNoAnimation),
            FontId::new(legible(10.0), FontFamily::Proportional),
            muted_color(),
        );
        return;
    };
    let view = document.view();
    let (start, end) = bar_span(geometry, view, first, last);
    let bar = Rect::from_min_max(
        pos2(start, row_rect.top() + 5.0),
        pos2(end, row_rect.bottom() - 5.0),
    );
    let selected = document
        .selection()
        .iter()
        .any(|key| key.track.target == target);
    painter.rect(
        bar,
        CornerRadius::same(4),
        ACCENT_FILL,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                accent_color()
            } else {
                accent_color().gamma_multiply(0.6)
            },
        ),
        StrokeKind::Inside,
    );
    for x in [bar.left() + 3.0, bar.right() - 3.0] {
        painter.rect_filled(
            Rect::from_center_size(pos2(x, bar.center().y), vec2(4.0, bar.height() - 6.0)),
            CornerRadius::same(1),
            accent_color(),
        );
    }
    let text = crate::l10n::frames(l, last - first);
    if bar.width() > 56.0 {
        painter.text(
            bar.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::new(legible(10.0), FontFamily::Monospace),
            text_color(),
        );
    }
    // Key ticks inside the bar show where the motion changes.
    for key in target_keys(document, target).iter() {
        let x = geometry.x_of(view, key.frame);
        painter.line_segment(
            [pos2(x, bar.bottom() - 4.0), pos2(x, bar.bottom() - 1.0)],
            Stroke::new(1.0, text_dim_color()),
        );
    }
}

/// The Stagger menu's Apply ghosted over the rows: each moved element's
/// keys (Advanced) or bar (Simple) where they would land, and the new phase
/// end when the phase would grow.
fn paint_stagger_preview<T: Copy + Ord>(
    painter: &Painter,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    rows: &[Row<T>],
    plan: &StaggerPlan<T>,
) {
    let view = document.view();
    let ghost = AMBER.gamma_multiply(0.85);
    let moved = |target: T| -> Option<i64> {
        plan.moves
            .iter()
            .find(|(moved, delta)| *moved == target && *delta != 0)
            .map(|(_, delta)| *delta)
    };
    // Frames of the plan stay within the phase limit, so they fit.
    let shifted = |frame: u32, delta: i64| u32::try_from(i64::from(frame) + delta).unwrap_or(frame);
    for (index, row) in rows.iter().enumerate() {
        match *row {
            Row::Track(track) => {
                let (Some(delta), Some(keys)) = (moved(track.target), document.clip().track(track))
                else {
                    continue;
                };
                let y = geometry.row_center(index);
                if let (Some(first), Some(last)) = (keys.keys.first(), keys.keys.last()) {
                    painter.add(Shape::dashed_line(
                        &[
                            pos2(geometry.x_of(view, shifted(first.frame, delta)), y),
                            pos2(geometry.x_of(view, shifted(last.frame, delta)), y),
                        ],
                        Stroke::new(1.0, ghost),
                        4.0,
                        3.0,
                    ));
                }
                for key in &keys.keys {
                    let center = pos2(geometry.x_of(view, shifted(key.frame, delta)), y);
                    let r = KEY_RADIUS;
                    painter.add(Shape::closed_line(
                        vec![
                            center + vec2(0.0, -r),
                            center + vec2(r, 0.0),
                            center + vec2(0.0, r),
                            center + vec2(-r, 0.0),
                        ],
                        Stroke::new(1.5, ghost),
                    ));
                }
            }
            Row::Target(target) => {
                let Some(delta) = moved(target) else {
                    continue;
                };
                let Some((_, first, last)) = document
                    .target_spans()
                    .into_iter()
                    .find(|span| span.0 == target)
                else {
                    continue;
                };
                let row_rect = geometry.row_rect(index);
                let (start, end) =
                    bar_span(geometry, view, shifted(first, delta), shifted(last, delta));
                painter.rect(
                    Rect::from_min_max(
                        pos2(start, row_rect.top() + 5.0),
                        pos2(end, row_rect.bottom() - 5.0),
                    ),
                    CornerRadius::same(4),
                    AMBER.gamma_multiply(0.12),
                    Stroke::new(1.5, ghost),
                    StrokeKind::Inside,
                );
            }
        }
    }
    if plan.last_frame > u64::from(document.phase_frames()) {
        let end = u32::try_from(plan.last_frame).unwrap_or(u32::MAX);
        dashed_line(
            painter,
            geometry.x_of(view, end),
            geometry.rows_top,
            geometry.rect.bottom(),
            ghost,
        );
    }
}

/// Box selection, snapping guide, playhead, and a refused-drag message.
fn paint_overlays<T: Copy + Ord>(
    painter: &Painter,
    l: Locale,
    document: &AnimDocument<T>,
    geometry: &TrackGeometry,
    response: &Response,
) {
    let view = document.view();
    let top = geometry.rect.top();
    let bottom = geometry.rect.bottom();
    if let Some(drag) = document.drag() {
        if let Some(((low, high), (row_low, row_high))) = drag.box_range() {
            let rect = Rect::from_min_max(
                pos2(
                    geometry.x_of(view, low) - KEY_RADIUS,
                    geometry.row_center(row_low) - geometry.row_height / 2.0,
                ),
                pos2(
                    geometry.x_of(view, high) + KEY_RADIUS,
                    geometry.row_center(row_high) + geometry.row_height / 2.0,
                ),
            );
            painter.rect(
                rect,
                CornerRadius::same(2),
                accent_color().gamma_multiply(0.12),
                Stroke::new(1.0, accent_color()),
                StrokeKind::Inside,
            );
        }
        if let Some(snap) = drag.snap() {
            let x = geometry.x_of(view, snap.frame);
            dashed_line(painter, x, top, bottom, AMBER);
            let label = l.text(match snap.kind {
                SnapKind::Playhead => Msg::AnimSnapPlayhead,
                SnapKind::Key => Msg::AnimSnapKey,
                SnapKind::PhaseBound => Msg::AnimSnapPhaseEdge,
                SnapKind::WholeSecond => Msg::AnimSnapSecond,
            });
            painter.text(
                pos2(x + 4.0, geometry.ruler.bottom() + 2.0),
                Align2::LEFT_TOP,
                label,
                FontId::new(legible(9.5), FontFamily::Proportional),
                AMBER,
            );
        }
        if let Some(error) = drag.rejected()
            && let Some(position) = response.interact_pointer_pos()
        {
            let galley = painter.layout(
                text::describe_error(l, error),
                FontId::new(legible(10.5), FontFamily::Proportional),
                ERROR,
                280.0,
            );
            let at = position + vec2(12.0, -galley.size().y - 8.0);
            let rect = Rect::from_min_size(at, galley.size()).expand(4.0);
            painter.rect_filled(rect, CornerRadius::same(3), Color32::from_black_alpha(220));
            painter.galley(at, galley, ERROR);
        }
        if matches!(drag.kind(), DragKind::Retime { .. }) {
            painter.text(
                pos2(geometry.track_left + 6.0, bottom - 4.0),
                Align2::LEFT_BOTTOM,
                l.text(if view.snapping() {
                    Msg::AnimRetimeModifiers
                } else {
                    Msg::AnimRetimeModifiersSnapOff
                }),
                FontId::new(legible(9.5), FontFamily::Proportional),
                text_dim_color(),
            );
        }
    }
    let x = geometry.x_of(view, document.playhead());
    painter.line_segment(
        [pos2(x, top), pos2(x, bottom)],
        Stroke::new(1.5, text_color()),
    );
    let head = [pos2(x - 5.0, top), pos2(x + 5.0, top), pos2(x, top + 8.0)];
    painter.add(Shape::convex_polygon(
        head.to_vec(),
        accent_color(),
        Stroke::NONE,
    ));
}

fn dashed_line(painter: &Painter, x: f32, top: f32, bottom: f32, color: Color32) {
    painter.add(Shape::dashed_line(
        &[pos2(x, top), pos2(x, bottom)],
        Stroke::new(1.0, color),
        4.0,
        3.0,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_steps_stay_readable_at_every_zoom() {
        // Four pixels per frame at 25 fps: labels every 20 frames would be
        // 80 px, but a sub-second step must divide the rate (1 or 5 frames
        // at 25 fps), so the next is one second.
        assert_eq!(ruler_step(0.25, 25), 25);
        assert_eq!(ruler_step(1.0 / 64.0, 25), 1);
        assert_eq!(ruler_step(1.0 / 8.0, 25), 25);
        assert_eq!(ruler_step(1.0 / 16.0, 25), 5);
        assert_eq!(ruler_step(4.0, 25), 25 * 15);
        assert_eq!(ruler_label(0, 25), "0:00");
        assert_eq!(ruler_label(37, 25), "1:12");
        assert_eq!(ruler_label(25 * 61 + 3, 25), "1:01:03");
    }

    /// Every labelled tick at every zoom and common rate falls on a
    /// frame-rate-friendly position: a sub-second step divides the rate, so
    /// each second boundary is labelled and the frame part of a label is
    /// always a multiple of the step (30 fps never reads `1:05` after
    /// `0:20`); a longer step is whole seconds.
    #[test]
    fn ruler_labels_align_to_the_frame_rate() {
        for fps in [24_u32, 25, 30, 50, 60] {
            let mut previous = 0;
            for zoom in 1..=400 {
                #[allow(clippy::cast_precision_loss)]
                let frames_per_pixel = zoom as f32 / 64.0;
                let step = ruler_step(frames_per_pixel, fps);
                assert!(step >= previous, "{fps} fps: steps grow with zoom");
                previous = step;
                if step < fps {
                    assert_eq!(fps % step, 0, "{fps} fps: {step} divides the rate");
                } else {
                    assert_eq!(step % fps, 0, "{fps} fps: {step} is whole seconds");
                }
                let minor = ruler_minor_step(step, fps);
                assert_eq!(step % minor, 0, "{fps} fps: minor {minor} divides {step}");
                assert!(step / minor <= 6);
                if minor >= fps {
                    assert_eq!(minor % fps, 0);
                }
                let labels: Vec<_> = (0..4).map(|tick| ruler_label(tick * step, fps)).collect();
                for (tick, label) in (0_u32..).zip(&labels) {
                    let frame = tick * step;
                    let frames = label.rsplit(':').next().unwrap().parse::<u32>().unwrap();
                    assert_eq!(frames, frame % fps, "{fps} fps label {label}");
                    assert_eq!(frames % step.min(fps), 0, "{fps} fps label {label}");
                }
            }
        }
        // The reported case: a 10-frame step at 30 fps reads 0:00, 0:10,
        // 0:20, 1:00, and 25 fps never takes a 10-frame step.
        let at_30: Vec<_> = (0..6).map(|tick| ruler_label(tick * 10, 30)).collect();
        assert_eq!(at_30, ["0:00", "0:10", "0:20", "1:00", "1:10", "1:20"]);
        assert_eq!(ruler_step(1.0 / 6.0, 30), 15);
        assert!((1..=400).all(|zoom| {
            #[allow(clippy::cast_precision_loss)]
            let step = ruler_step(zoom as f32 / 64.0, 25);
            step != 10 && step != 2
        }));
        assert_eq!(ruler_minor_step(25, 25), 5);
        assert_eq!(ruler_minor_step(30, 30), 5);
        assert_eq!(ruler_minor_step(10, 30), 2);
        assert_eq!(ruler_minor_step(60, 30), 30);
        assert_eq!(ruler_minor_step(30 * 30, 30), 150);
    }
}
