//! The colour panel's painted controls: grade sliders, colour wheels, the
//! curve editor, and tabs. Each is one egui widget with hover help, a
//! relative drag (Shift for fine control) that is one undo step, and a
//! double-click reset.

use egui::{
    Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Response, RichText, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};
use fm_grade::{CurveChannel, HueCurveKind, Wheel, WheelKind};
use fm_ui_model::grade_editor::{
    CurveTarget, FINE_FACTOR, GradeDocument, GradeParam, GradeSection, ValueDrag, WheelDrag,
    puck_offset, wheel_color, wheel_mut,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::{ColorPanelState, PanelDrag, text};
use crate::gating::Hint;
use crate::theme::{
    ACCENT_FILL, accent_color, border_color, border_strong_color, legible, line_width, muted_color,
    text_color, text_dim_color, widget_color,
};

/// Row height of a slider.
pub const SLIDER_HEIGHT: f32 = 22.0;
/// Curve points are this many points in radius on screen.
const POINT_RADIUS: f32 = 4.5;
/// Pointer distance, in points, that grabs a curve point.
const GRAB_RADIUS: f32 = 8.0;

/// Widths of a slider's label and value columns.
#[derive(Clone, Copy, Debug)]
pub struct SliderLayout {
    pub label: f32,
    pub value: f32,
}

impl SliderLayout {
    pub const FULL: Self = Self {
        label: 84.0,
        value: 64.0,
    };
    pub const COMPACT: Self = Self {
        label: 0.0,
        value: 44.0,
    };
}

/// Shift held: fine adjustment.
fn fine(ui: &Ui) -> bool {
    ui.input(|input| input.modifiers.shift)
}

/// A double-click, which resets a control. A quick click elsewhere just
/// before makes egui report the second click as a triple click, which
/// resets too.
pub(super) fn reset_click(response: &Response) -> bool {
    response.double_clicked() || response.triple_clicked()
}

/// Ends any drag in progress, committing its gesture.
pub(super) fn finish_drag(document: &mut GradeDocument, state: &mut ColorPanelState) {
    state.drag = None;
    if document.gesture_active() {
        let _ = document.commit_gesture();
    }
}

/// A grade slider: name, track, and value in one row. `disabled` gives the
/// reason the control is unavailable.
pub(super) fn slider(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    param: GradeParam,
    (width, layout): (f32, SliderLayout),
    disabled: Option<&str>,
) -> Response {
    let disabled = state.lock_reason.or(disabled);
    let enabled = disabled.is_none();
    let value = param.get(document.grade());
    let name = text::param_name(l, param);
    // A disabled `Ui` makes a disabled response that still explains itself.
    let (rect, response) = ui
        .add_enabled_ui(enabled, |ui| {
            ui.allocate_exact_size(vec2(width, SLIDER_HEIGHT), Sense::click_and_drag())
        })
        .inner;
    response.widget_info(|| WidgetInfo::slider(enabled, f64::from(value), name));
    let response = response.hint_in(
        l,
        &text::slider_hover(l, param, value),
        disabled.unwrap_or(""),
    );
    let track = Rect::from_min_max(
        pos2(rect.min.x + layout.label, rect.min.y + 4.0),
        pos2(rect.max.x - layout.value - 4.0, rect.max.y - 4.0),
    );

    if enabled {
        let shift = fine(ui);
        // Anchor at the press, not where the drag was recognized, so the
        // first few pixels of motion count.
        if response.drag_started()
            && let Some(pointer) = ui.input(|input| input.pointer.press_origin())
            && document.begin_gesture().is_ok()
        {
            state.drag = Some(PanelDrag::Slider(
                param,
                ValueDrag::new(value, param.range(), track.width(), pointer.x, shift),
            ));
        }
        if response.dragged()
            && let Some(PanelDrag::Slider(dragged, drag)) = &mut state.drag
            && *dragged == param
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let next = drag.update(pointer.x, shift);
            let _ = document.update_gesture(|grade| {
                param.set(grade, next);
                Ok(())
            });
        }
        if response.drag_stopped() {
            finish_drag(document, state);
        }
        if reset_click(&response)
            && let Err(error) = document.reset_param(param)
        {
            state.show_error(&error);
        }
    }

    if ui.is_rect_visible(rect) {
        let value = param.get(document.grade());
        let painter = ui.painter();
        let dim = if enabled { 1.0 } else { 0.45 };
        if layout.label > 0.0 {
            painter.text(
                pos2(rect.min.x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                FontId::proportional(legible(11.0)),
                text_dim_color().gamma_multiply(dim),
            );
        }
        paint_track(
            painter,
            track,
            param,
            value,
            response.hovered() || response.dragged(),
            dim,
        );
        painter.text(
            pos2(rect.max.x, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            text::format_param(l, param, value),
            FontId::monospace(legible(10.5)),
            text_color().gamma_multiply(dim),
        );
    }
    response
}

/// The track: an optional gradient, a fill from the default to the value,
/// and the thumb.
#[allow(clippy::cast_precision_loss)] // Grade ranges are tiny.
fn paint_track(
    painter: &egui::Painter,
    track: Rect,
    param: GradeParam,
    value: i32,
    hot: bool,
    dim: f32,
) {
    let (low, high) = param.range();
    let fraction = |value: i32| (value - low) as f32 / (high - low).max(1) as f32;
    let x_of = |value: i32| track.min.x + fraction(value) * track.width();
    let bar = Rect::from_center_size(track.center(), vec2(track.width(), 6.0));
    match gradient(param) {
        Some(stops) => paint_gradient(painter, bar, &stops, dim),
        None => {
            painter.rect_filled(bar, CornerRadius::same(3), widget_color());
        }
    }
    painter.rect_stroke(
        bar,
        CornerRadius::same(3),
        Stroke::new(line_width(), border_strong_color()),
        StrokeKind::Outside,
    );
    let start = x_of(param.default_value());
    let end = x_of(value);
    let fill = Rect::from_x_y_ranges(start.min(end)..=start.max(end), bar.y_range());
    painter.rect_filled(
        fill.shrink2(vec2(0.0, 1.5)),
        CornerRadius::same(2),
        accent_color().gamma_multiply(0.85 * dim),
    );
    // Default tick.
    painter.line_segment(
        [pos2(start, bar.min.y - 3.0), pos2(start, bar.max.y + 3.0)],
        Stroke::new(1.0, muted_color()),
    );
    let thumb = pos2(end, bar.center().y);
    painter.circle(
        thumb,
        if hot { 6.5 } else { 5.5 },
        text_color().gamma_multiply(dim),
        Stroke::new(1.5, Color32::from_gray(20)),
    );
}

/// Gradient stops that show what a slider does to the picture.
fn gradient(param: GradeParam) -> Option<Vec<Color32>> {
    let rgb = Color32::from_rgb;
    Some(match param {
        GradeParam::Temperature => vec![rgb(70, 130, 230), rgb(150, 150, 150), rgb(235, 165, 60)],
        GradeParam::Tint => vec![rgb(70, 190, 90), rgb(150, 150, 150), rgb(210, 80, 200)],
        GradeParam::Saturation => vec![rgb(140, 140, 140), rgb(210, 110, 70), rgb(255, 60, 30)],
        GradeParam::Exposure | GradeParam::WheelMaster(_) => {
            vec![rgb(20, 20, 20), rgb(128, 128, 128), rgb(235, 235, 235)]
        }
        _ => return None,
    })
}

#[allow(clippy::cast_precision_loss)] // A handful of stops.
fn paint_gradient(painter: &egui::Painter, rect: Rect, stops: &[Color32], dim: f32) {
    let mut mesh = Mesh::default();
    let last = stops.len().saturating_sub(1).max(1) as f32;
    for (index, color) in stops.iter().enumerate() {
        let x = rect.min.x + rect.width() * index as f32 / last;
        let color = color.gamma_multiply(dim);
        mesh.colored_vertex(pos2(x, rect.min.y), color);
        mesh.colored_vertex(pos2(x, rect.max.y), color);
        if index > 0 {
            let base = u32::try_from(index * 2).unwrap_or(0);
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// One colour wheel with its name, reset key, and master slider. Returns
/// the wheel's rect.
pub(super) fn wheel(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    kind: WheelKind,
    size: f32,
) -> Rect {
    let name = text::wheel_name(l, kind);
    let mut rect = Rect::NOTHING;
    ui.allocate_ui(vec2(size, size + 60.0), |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.set_width(size);
                ui.label(
                    RichText::new(name)
                        .size(legible(11.0))
                        .strong()
                        .color(text_color()),
                )
                .on_hover_text(text::wheel_hover(l, kind));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let section = GradeSection::Wheel(kind);
                    let neutral = section.is_default(document.grade());
                    let reset = ui
                        .add_enabled(
                            !neutral && state.lock_reason.is_none(),
                            crate::theme::small_button("↺"),
                        )
                        .hint_in(
                            l,
                            &l.format(Msg::GradeWheelResetHover, &[("wheel", Arg::Str(name))]),
                            state
                                .lock_reason
                                .unwrap_or_else(|| l.text(Msg::GradeReasonDefault)),
                        );
                    if reset.clicked()
                        && let Err(error) = document.reset_section(section)
                    {
                        state.show_error(&error);
                    }
                });
            });
            rect = wheel_disc(ui, l, document, state, kind, size);
            slider(
                ui,
                l,
                document,
                state,
                GradeParam::WheelMaster(kind),
                (size, SliderLayout::COMPACT),
                None,
            );
        });
    });
    rect
}

fn wheel_disc(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    kind: WheelKind,
    size: f32,
) -> Rect {
    let lock = state.lock_reason;
    let (rect, response) = ui
        .add_enabled_ui(lock.is_none(), |ui| {
            ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag())
        })
        .inner;
    let current = document.grade().wheels.get(kind);
    let name = text::wheel_name(l, kind);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, lock.is_none(), name));
    let response = response.hint_in(
        l,
        &format!(
            "{}\n{}",
            text::wheel_hover(l, kind),
            text::wheel_help(l, current.x, current.y)
        ),
        lock.unwrap_or(""),
    );
    let radius = size / 2.0 - 3.0;
    let shift = fine(ui);
    if response.drag_started()
        && let Some(pointer) = ui.input(|input| input.pointer.press_origin())
        && document.begin_gesture().is_ok()
    {
        state.drag = Some(PanelDrag::Wheel(
            kind,
            WheelDrag::new(current, [pointer.x, pointer.y], radius, shift),
        ));
    }
    if response.dragged()
        && let Some(PanelDrag::Wheel(dragged, drag)) = &mut state.drag
        && *dragged == kind
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let (x, y) = drag.update([pointer.x, pointer.y], shift);
        let _ = document.update_gesture(|grade| {
            let wheel = wheel_mut(grade, kind);
            wheel.x = x;
            wheel.y = y;
            Ok(())
        });
    }
    if response.drag_stopped() {
        finish_drag(document, state);
    }
    if reset_click(&response) {
        let master = document.grade().wheels.get(kind).master;
        if let Err(error) = document.set_wheel(kind, Wheel::new(0, 0, master)) {
            state.show_error(&error);
        }
    }
    if ui.is_rect_visible(rect) {
        paint_wheel(
            ui.painter(),
            rect.center(),
            radius,
            document.grade().wheels.get(kind),
            response.hovered() || response.dragged(),
        );
    }
    rect
}

#[allow(clippy::cast_precision_loss)] // Small loop counters.
fn paint_wheel(painter: &egui::Painter, centre: Pos2, radius: f32, wheel: Wheel, hot: bool) {
    const RINGS: usize = 5;
    const SEGMENTS: usize = 72;
    let color = |offset: [f32; 2]| {
        let [r, g, b] = wheel_color(offset);
        Color32::from_rgb(r, g, b)
    };
    let mut mesh = Mesh::default();
    mesh.colored_vertex(centre, color([0.0, 0.0]));
    for ring in 1..=RINGS {
        let fraction = ring as f32 / RINGS as f32;
        for segment in 0..SEGMENTS {
            let angle = segment as f32 / SEGMENTS as f32 * core::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            mesh.colored_vertex(
                centre + vec2(cos, -sin) * radius * fraction,
                color([cos * fraction, sin * fraction]),
            );
        }
    }
    let index = |ring: usize, segment: usize| -> u32 {
        u32::try_from(1 + (ring - 1) * SEGMENTS + segment % SEGMENTS).unwrap_or(0)
    };
    for segment in 0..SEGMENTS {
        mesh.add_triangle(0, index(1, segment), index(1, segment + 1));
        for ring in 2..=RINGS {
            let (a, b) = (index(ring - 1, segment), index(ring - 1, segment + 1));
            let (c, d) = (index(ring, segment), index(ring, segment + 1));
            mesh.add_triangle(a, c, d);
            mesh.add_triangle(a, d, b);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(
        centre,
        radius,
        Stroke::new(
            1.5,
            if hot {
                accent_color()
            } else {
                border_strong_color()
            },
        ),
    );
    let guide = Stroke::new(1.0, Color32::from_black_alpha(70));
    painter.line_segment(
        [centre - vec2(radius, 0.0), centre + vec2(radius, 0.0)],
        guide,
    );
    painter.line_segment(
        [centre - vec2(0.0, radius), centre + vec2(0.0, radius)],
        guide,
    );
    painter.circle_stroke(centre, radius * 0.5, guide);
    let [u, v] = puck_offset(wheel);
    let puck = centre + vec2(u, -v) * radius;
    painter.line_segment(
        [centre, puck],
        Stroke::new(1.5, Color32::from_black_alpha(150)),
    );
    painter.circle(
        puck,
        if hot { 6.0 } else { 5.0 },
        Color32::WHITE,
        Stroke::new(1.5, Color32::from_gray(25)),
    );
}

/// A relative drag of one curve point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CurveDrag {
    target: CurveTarget,
    index: usize,
    anchor: [f32; 2],
    anchor_pointer: Pos2,
    fine: bool,
    last: [f32; 2],
}

/// The curve editor's graph for `target`; returns its rect.
#[allow(clippy::too_many_lines)] // One interaction table and its painting.
pub(super) fn curve_editor(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    target: CurveTarget,
    size: Vec2,
) -> Rect {
    let lock = state.lock_reason;
    let (rect, response) = ui
        .add_enabled_ui(lock.is_none(), |ui| {
            ui.allocate_exact_size(size, Sense::click_and_drag())
        })
        .inner;
    let name = text::curve_name(l, target);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, lock.is_none(), name));
    let response = response.hint_in(
        l,
        &format!(
            "{}\n{}",
            text::curve_hover(l, target),
            l.text(Msg::GradeCurveCanvasHover)
        ),
        lock.unwrap_or(""),
    );
    let inner = rect.shrink2(vec2(8.0, 8.0));
    let to_unit = |pos: Pos2| {
        [
            ((pos.x - inner.min.x) / inner.width()).clamp(0.0, 1.0),
            ((inner.max.y - pos.y) / inner.height()).clamp(0.0, 1.0),
        ]
    };
    let to_screen = |p: [f32; 2]| {
        pos2(
            inner.min.x + p[0] * inner.width(),
            inner.max.y - p[1] * inner.height(),
        )
    };
    let tolerance = [GRAB_RADIUS / inner.width(), GRAB_RADIUS / inner.height()];
    let shift = fine(ui);

    if response.drag_started()
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
        && document.begin_gesture().is_ok()
    {
        let at = to_unit(origin);
        let hit = target.hit(document.grade(), at, tolerance);
        let index = match hit {
            Some(index) => Ok(index),
            None => document.update_gesture(|grade| target.add_point(grade, at)),
        };
        match index {
            Ok(index) => {
                let anchor = target.points(document.grade())[index];
                state.drag = Some(PanelDrag::Curve(CurveDrag {
                    target,
                    index,
                    anchor,
                    anchor_pointer: origin,
                    fine: shift,
                    last: anchor,
                }));
            }
            Err(error) => {
                let _ = document.cancel_gesture();
                state.show_error(&error);
            }
        }
    }
    if response.dragged()
        && let Some(PanelDrag::Curve(drag)) = &mut state.drag
        && drag.target == target
        && let Some(pointer) = response.interact_pointer_pos()
    {
        if drag.fine != shift {
            drag.anchor = drag.last;
            drag.anchor_pointer = pointer;
            drag.fine = shift;
        }
        let scale = if shift { FINE_FACTOR } else { 1.0 };
        let delta = pointer - drag.anchor_pointer;
        let to = [
            (drag.anchor[0] + delta.x / inner.width() * scale).clamp(0.0, 1.0),
            (drag.anchor[1] - delta.y / inner.height() * scale).clamp(0.0, 1.0),
        ];
        drag.last = to;
        let index = drag.index;
        let _ = document.update_gesture(|grade| target.move_point(grade, index, to));
    }
    if response.drag_stopped() {
        finish_drag(document, state);
    }
    let pointer = response.interact_pointer_pos().map(to_unit);
    let hit = pointer.and_then(|at| target.hit(document.grade(), at, tolerance));
    if response.clicked()
        && hit.is_none()
        && let Some(at) = pointer
        && let Err(error) = document.add_curve_point(target, at)
    {
        state.show_error(&error);
    }
    if (response.double_clicked() || response.secondary_clicked())
        && let Some(index) = hit
        && let Err(error) = document.delete_curve_point(target, index)
    {
        state.show_error(&error);
    }

    if ui.is_rect_visible(rect) {
        let canvas = ui.painter_at(rect);
        let grade = document.grade();
        canvas.rect_filled(
            rect,
            CornerRadius::same(crate::theme::RADIUS),
            Color32::from_gray(14),
        );
        canvas.rect_stroke(
            rect,
            CornerRadius::same(crate::theme::RADIUS),
            Stroke::new(line_width(), border_color()),
            StrokeKind::Inside,
        );
        let grid = Stroke::new(1.0, Color32::from_gray(38));
        for step in 1..4_u8 {
            let fraction = f32::from(step) / 4.0;
            let x = inner.min.x + inner.width() * fraction;
            let y = inner.min.y + inner.height() * fraction;
            canvas.line_segment([pos2(x, inner.min.y), pos2(x, inner.max.y)], grid);
            canvas.line_segment([pos2(inner.min.x, y), pos2(inner.max.x, y)], grid);
        }
        let reference = Stroke::new(1.0, Color32::from_gray(70));
        match target {
            CurveTarget::Rgb(_) => {
                canvas.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], reference);
            }
            CurveTarget::Hue(kind) => {
                paint_axis_strip(&canvas, inner, kind);
                canvas.line_segment([to_screen([0.0, 0.5]), to_screen([1.0, 0.5])], reference);
            }
        }
        let samples = target.sample(grade, 129);
        let line: Vec<Pos2> = (0..=128_u8)
            .zip(&samples)
            .map(|(index, &y)| to_screen([f32::from(index) / 128.0, y]))
            .collect();
        let color = curve_color(target);
        canvas.add(Shape::line(line, Stroke::new(2.0, color)));
        let hovered = response
            .hover_pos()
            .and_then(|pos| target.hit(grade, to_unit(pos), tolerance));
        let dragged = match state.drag {
            Some(PanelDrag::Curve(drag)) if drag.target == target => Some(drag.index),
            _ => None,
        };
        for (index, point) in target.points(grade).into_iter().enumerate() {
            let hot = Some(index) == hovered || Some(index) == dragged;
            canvas.circle(
                to_screen(point),
                if hot {
                    POINT_RADIUS + 1.5
                } else {
                    POINT_RADIUS
                },
                if hot { Color32::WHITE } else { color },
                Stroke::new(1.5, Color32::from_gray(15)),
            );
        }
        if target.is_empty(grade) {
            canvas.text(
                inner.center_top() + vec2(0.0, 10.0),
                egui::Align2::CENTER_TOP,
                l.text(Msg::GradeCurveEmpty),
                FontId::proportional(legible(10.5)),
                muted_color(),
            );
        }
    }
    rect
}

/// A strip along the bottom showing what the horizontal axis means: the
/// hue circle, or black to white for the luma curve.
fn paint_axis_strip(painter: &egui::Painter, inner: Rect, kind: HueCurveKind) {
    let strip = Rect::from_min_max(pos2(inner.min.x, inner.max.y - 6.0), inner.max);
    let colors: Vec<Color32> = (0..=36_u8)
        .map(|step| match kind {
            HueCurveKind::LumaVsSaturation => Color32::from_gray(step * 7),
            _ => egui::ecolor::Hsva::new(f32::from(step) / 36.0, 0.75, 0.85, 1.0).into(),
        })
        .collect();
    paint_gradient(painter, strip, &colors, 1.0);
}

/// The drawing colour of a curve.
pub(super) fn curve_color(target: CurveTarget) -> Color32 {
    match target {
        CurveTarget::Rgb(CurveChannel::Master) => Color32::from_gray(225),
        CurveTarget::Rgb(CurveChannel::Red) => Color32::from_rgb(240, 80, 80),
        CurveTarget::Rgb(CurveChannel::Green) => Color32::from_rgb(90, 210, 100),
        CurveTarget::Rgb(CurveChannel::Blue) => Color32::from_rgb(90, 140, 250),
        CurveTarget::Hue(_) => Color32::from_rgb(235, 200, 90),
    }
}

/// A tab: highlighted while `selected`, with hover help.
pub(super) fn tab(ui: &mut Ui, selected: bool, label: &str, hover: &str) -> Response {
    let button = egui::Button::new(RichText::new(label).size(legible(10.5)).strong().color(
        if selected {
            text_color()
        } else {
            text_dim_color()
        },
    ))
    .fill(if selected {
        ACCENT_FILL
    } else {
        widget_color()
    })
    .stroke(Stroke::new(
        line_width(),
        if selected {
            accent_color().gamma_multiply(0.7)
        } else {
            border_strong_color()
        },
    ))
    .min_size(vec2(0.0, 20.0));
    ui.add(button).on_hover_text(hover)
}
