//! Advanced-mode inspector: easing for the selected keys, the active row's
//! value at the playhead, an "animate property" menu, and a value graph of
//! the active row aligned with the timeline above it.

use egui::{
    Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Painter, Pos2, Rect, Sense,
    Shape, Stroke, Ui, pos2, vec2,
};
use fm_animation::{EASE_ONE, Easing, EasingPreset, Property, TrackKey, Value};
use fm_ui_model::animation_editor::{AnimDocument, phase_state};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::{AnimationTimelineState, TrackGeometry, text};
use crate::gating::Hint;
use crate::theme::{
    AMBER, accent_color, border_color, field_label, legible, muted_color, raised_color, text_color,
    text_dim_color,
};

/// Height of the value graph.
pub const CURVE_HEIGHT: f32 = 96.0;

pub(super) fn draw<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    geometry: &TrackGeometry,
) {
    // The active row follows the shown phase and must still exist: its
    // element in the design, or (without an outline) its track.
    if let Some(row) = state.active_row
        && (row.phase != document.phase()
            || (document.target(row.target).is_none() && document.clip().track(row).is_none()))
    {
        state.active_row = None;
    }
    ui.horizontal(|ui| {
        easing_picker(ui, l, document, state);
        ui.separator();
        value_field(ui, l, document, state);
        ui.separator();
        add_property_menu(ui, l, document, state);
    });
    curve(ui, l, document, state, geometry);
}

/// The easing shared by every selected key, `None` when mixed or empty.
fn selected_easing<T: Copy + Ord>(document: &AnimDocument<T>) -> Option<Easing> {
    let mut easings = document
        .selection()
        .iter()
        .filter_map(|key| document.clip().key(key.track, key.frame))
        .map(|key| key.easing);
    let first = easings.next()?;
    easings.all(|easing| easing == first).then_some(first)
}

fn easing_picker<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    ui.label(field_label(l.text(Msg::AnimEasing)));
    let current = selected_easing(document);
    let empty = document.selection().is_empty();
    let selected_text = match (current, empty) {
        (_, true) => "—".to_owned(),
        (Some(easing), false) => text::easing_name(l, easing).to_owned(),
        (None, false) => l.text(Msg::AnimEaseMixed).to_owned(),
    };
    let lock = state.lock_reason.map(|reason| l.tr(reason));
    let enabled = !empty && document.drag().is_none() && lock.is_none();
    let reason = if let Some(lock) = lock {
        lock
    } else if empty {
        l.text(Msg::AnimReasonNoSelection)
    } else {
        l.text(Msg::AnimReasonDrag)
    };
    let mut chosen = None;
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt("anim-easing")
                .selected_text(selected_text)
                .width(150.0)
                .show_ui(ui, |ui| {
                    for preset in EasingPreset::ALL {
                        let response = ui
                            .horizontal(|ui| {
                                easing_thumbnail(ui, preset.easing(), vec2(28.0, 16.0));
                                ui.selectable_label(
                                    current == Some(preset.easing()),
                                    text::easing_preset_name(l, preset),
                                )
                            })
                            .inner
                            .on_hover_text(text::easing_preset_hover(l, preset));
                        if response.clicked() {
                            chosen = Some(preset.easing());
                        }
                    }
                })
                .response
        })
        .inner;
    response.hint_in(l, l.text(Msg::AnimEasingHover), reason);
    if let Some(easing) = current.filter(|_| !empty) {
        easing_thumbnail(ui, easing, vec2(36.0, 22.0)).on_hover_text(l.format(
            Msg::AnimEasingShapeHover,
            &[("easing", Arg::Str(text::easing_name(l, easing)))],
        ));
    }
    if let Some(easing) = chosen
        && let Err(error) = document.set_easing(easing)
    {
        state.show_error(&error);
    }
}

/// A small plot of an easing curve.
fn easing_thumbnail(ui: &mut Ui, easing: Easing, size: egui::Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), raised_color());
    let inner = rect.shrink(2.0);
    let steps = 24_u32;
    let points: Vec<Pos2> = (0..=steps)
        .map(|step| {
            let progress = step * EASE_ONE / steps;
            #[allow(clippy::cast_precision_loss)] // Q16 values.
            let eased = easing.apply(progress) as f32 / EASE_ONE as f32;
            #[allow(clippy::cast_precision_loss)]
            let x = inner.left() + inner.width() * step as f32 / steps as f32;
            // -0.25..=1.25 so overshooting curves stay visible.
            let y = inner.bottom() - inner.height() * (eased + 0.25) / 1.5;
            pos2(x, y)
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(1.5, accent_color())));
    response
}

fn value_field<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    ui.label(field_label(l.text(Msg::AnimValue)));
    let Some(row) = state.active_row else {
        ui.add_enabled(false, egui::Button::new("—")).hint_in(
            l,
            l.text(Msg::AnimValueNoRowHover),
            l.text(Msg::AnimReasonNoValueRow),
        );
        return;
    };
    let current = document.value_at_playhead(row.target, row.property);
    let Ok(value) = current else {
        return;
    };
    match value {
        Value::Color(color) => {
            let (rect, response) = ui.allocate_exact_size(vec2(40.0, 18.0), Sense::hover());
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(2),
                Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a),
            );
            response.on_hover_text(l.format(
                Msg::AnimColourAtPlayheadHover,
                &[(
                    "value",
                    Arg::Str(&text::format_value(l, row.property, value)),
                )],
            ));
        }
        Value::Scalar(raw) => {
            let (unit, divisor) = text::property_unit(row.property);
            #[allow(clippy::cast_precision_loss)] // Display only.
            let mut shown = state.value_edit.unwrap_or(raw as f64 / divisor);
            let range = row.property.range().map_or(
                (f64::MIN, f64::MAX),
                #[allow(clippy::cast_precision_loss)]
                |(low, high)| (low as f64 / divisor, high as f64 / divisor),
            );
            let lock = state.lock_reason.map(|reason| l.tr(reason));
            let response = ui
                .add_enabled(
                    document.drag().is_none() && lock.is_none(),
                    DragValue::new(&mut shown)
                        .range(range.0..=range.1)
                        .speed(if divisor > 10.0 { 0.5 } else { 1.0 })
                        .max_decimals(1)
                        .suffix(unit),
                )
                .hint_in(
                    l,
                    &l.format(
                        Msg::AnimValueHover,
                        &[("property", Arg::Str(text::property_name(l, row.property)))],
                    ),
                    lock.unwrap_or(l.text(Msg::AnimReasonDrag)),
                );
            if response.changed() {
                state.value_edit = Some(shown);
            }
            if !(response.dragged() || response.has_focus())
                && let Some(edited) = state.value_edit.take()
                && lock.is_none()
            {
                #[allow(clippy::cast_possible_truncation)] // Clamped by the range above.
                let stored = (edited * divisor).round() as i64;
                if stored != raw
                    && let Err(error) = document.set_value_at_playhead(
                        row.target,
                        row.property,
                        row.property.clamp(Value::Scalar(stored)),
                    )
                {
                    state.show_error(&error);
                }
            }
        }
    }
}

fn add_property_menu<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    let phase = document.phase();
    let targets: Vec<(T, String)> = document
        .targets()
        .iter()
        .map(|outline| (outline.target, outline.name.clone()))
        .collect();
    let mut chosen = None;
    let lock = state.lock_reason.map(|reason| l.tr(reason));
    let enabled = document.drag().is_none() && !targets.is_empty() && lock.is_none();
    let reason = if let Some(lock) = lock {
        lock
    } else if targets.is_empty() {
        l.text(Msg::AnimReasonNoTargets)
    } else {
        l.text(Msg::AnimReasonDrag)
    };
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ui.menu_button(l.text(Msg::AnimAnimateProperty), |ui| {
                for (target, name) in &targets {
                    let submenu = ui.menu_button(name.as_str(), |ui| {
                        for property in Property::ALL {
                            let animated = document
                                .clip()
                                .track(TrackKey::new(*target, phase, property))
                                .is_some();
                            let response = ui
                                .add_enabled(
                                    !animated,
                                    egui::Button::new(text::property_name(l, property)),
                                )
                                .hint_in(
                                    l,
                                    text::property_hover(l, property),
                                    l.text(Msg::AnimReasonAlreadyAnimated),
                                );
                            if response.clicked() {
                                chosen = Some((*target, property));
                                ui.close();
                            }
                        }
                    });
                    submenu.response.on_hover_text(
                        l.format(Msg::AnimAnimateTargetHover, &[("name", Arg::Str(name))]),
                    );
                }
            })
            .response
        })
        .inner;
    response.hint_in(l, l.text(Msg::AnimAnimatePropertyHover), reason);
    if let Some((target, property)) = chosen {
        match document.key_at_playhead(target, property) {
            Ok(key) => state.active_row = Some(key.track),
            Err(error) => state.show_error(&error),
        }
    }
}

/// The active row's value over the shown phase, frame-aligned with the
/// timeline (scalar rows as a line, colour rows as a strip).
#[allow(clippy::too_many_lines)] // One painter pass over the graph.
fn curve<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
    geometry: &TrackGeometry,
) {
    let width = geometry.rect.width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, CURVE_HEIGHT), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(
            rect.left() + (geometry.track_left - geometry.rect.left()),
            rect.top(),
        ),
        rect.max,
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), crate::theme::MONITOR_BLACK);
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(1.0, border_color()),
        egui::StrokeKind::Inside,
    );
    let small = FontId::new(legible(10.0), FontFamily::Proportional);
    let Some(row) = state.active_row else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            l.text(Msg::AnimGraphEmpty),
            small,
            muted_color(),
        );
        response.on_hover_text(l.text(Msg::AnimGraphEmptyHover));
        return;
    };
    painter.text(
        pos2(rect.left() + 6.0, rect.top() + 4.0),
        Align2::LEFT_TOP,
        text::property_name(l, row.property),
        FontId::new(legible(11.0), FontFamily::Proportional),
        text_color(),
    );
    let view = document.view();
    let (first, last) = view.visible_range(geometry.track_width());
    let last = last.min(document.phase_frames());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Small positive.
    let stride = (view.frames_per_pixel() * 2.0).ceil().max(1.0) as u32;
    let phase = document.phase();
    let sample = |frame: u32| {
        document
            .clip()
            .sample(row.target, row.property, phase_state(phase, frame))
    };
    let frames: Vec<u32> = (first..=last)
        .step_by(stride as usize)
        .chain(core::iter::once(last))
        .collect();
    let plot_painter = painter.with_clip_rect(plot);
    let x_of = |frame: u32| plot.left() + view.x_of_frame(frame);
    let keys = document
        .clip()
        .track(row)
        .map(|track| track.keys.clone())
        .unwrap_or_default();

    if row.property == Property::Color {
        for pair in frames.windows(2) {
            if let Some(Value::Color(color)) = sample(pair[0]) {
                plot_painter.rect_filled(
                    Rect::from_min_max(
                        pos2(x_of(pair[0]), plot.top() + 24.0),
                        pos2(x_of(pair[1]) + 1.0, plot.bottom() - 24.0),
                    ),
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a),
                );
            }
        }
    } else {
        let values: Vec<(u32, f64)> = frames
            .iter()
            .filter_map(|frame| match sample(*frame) {
                #[allow(clippy::cast_precision_loss)]
                Some(Value::Scalar(value)) => Some((*frame, value as f64)),
                _ => None,
            })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let key_values = keys.iter().filter_map(|key| match key.value {
            Value::Scalar(value) => Some(value as f64),
            Value::Color(_) => None,
        });
        let (low, high) = values
            .iter()
            .map(|(_, value)| *value)
            .chain(key_values)
            .fold((f64::MAX, f64::MIN), |(low, high), value| {
                (low.min(value), high.max(value))
            });
        if low <= high {
            let (low, high) = if (high - low).abs() < f64::EPSILON {
                (low - 1.0, high + 1.0)
            } else {
                (low, high)
            };
            let (top, bottom) = (plot.top() + 10.0, plot.bottom() - 10.0);
            #[allow(clippy::cast_possible_truncation)] // Screen coordinates.
            let y_of = |value: f64| bottom - ((value - low) / (high - low)) as f32 * (bottom - top);
            let (unit, divisor) = text::property_unit(row.property);
            for (value, align) in [(high, Align2::LEFT_TOP), (low, Align2::LEFT_BOTTOM)] {
                painter.text(
                    pos2(
                        rect.left() + 6.0,
                        if align == Align2::LEFT_TOP {
                            top + 12.0
                        } else {
                            bottom
                        },
                    ),
                    align,
                    text::format_number(l, value / divisor, 1, unit),
                    small.clone(),
                    text_dim_color(),
                );
            }
            let points: Vec<Pos2> = values
                .iter()
                .map(|(frame, value)| pos2(x_of(*frame), y_of(*value)))
                .collect();
            plot_painter.add(Shape::line(points, Stroke::new(1.5, accent_color())));
            for key in &keys {
                if let Value::Scalar(value) = key.value {
                    #[allow(clippy::cast_precision_loss)]
                    let center = pos2(x_of(key.frame), y_of(value as f64));
                    let selected = document.selection().contains(row, key.frame);
                    super::tracks::paint_key(&plot_painter, center, key.easing, selected);
                }
            }
        }
    }
    let playhead = x_of(document.playhead());
    plot_painter.line_segment(
        [pos2(playhead, plot.top()), pos2(playhead, plot.bottom())],
        Stroke::new(1.0, text_color().gamma_multiply(0.7)),
    );
    let end = x_of(document.phase_frames());
    plot_painter.line_segment(
        [pos2(end, plot.top()), pos2(end, plot.bottom())],
        Stroke::new(1.0, AMBER.gamma_multiply(0.6)),
    );
    hover_readout(&painter, &response, l, document, row, plot);
    response.on_hover_text(l.format(
        Msg::AnimGraphHover,
        &[("property", Arg::Str(text::property_name(l, row.property)))],
    ));
}

/// A value readout at the pointer's frame.
fn hover_readout<T: Copy + Ord>(
    painter: &Painter,
    response: &egui::Response,
    l: Locale,
    document: &AnimDocument<T>,
    row: TrackKey<T>,
    plot: Rect,
) {
    let Some(position) = response
        .hover_pos()
        .filter(|position| plot.contains(*position))
    else {
        return;
    };
    let frame = document
        .view()
        .frame_at_x(position.x - plot.left())
        .min(document.phase_frames());
    let value = document
        .clip()
        .sample(
            row.target,
            row.property,
            phase_state(document.phase(), frame),
        )
        .or_else(|| row.property.identity());
    if let Some(value) = value {
        painter.text(
            pos2(plot.right() - 6.0, plot.top() + 4.0),
            Align2::RIGHT_TOP,
            l.format(
                Msg::AnimGraphReadout,
                &[
                    ("frame", Arg::from(frame)),
                    (
                        "value",
                        Arg::Str(&text::format_value(l, row.property, value)),
                    ),
                ],
            ),
            FontId::new(legible(10.0), FontFamily::Monospace),
            text_color(),
        );
    }
}
