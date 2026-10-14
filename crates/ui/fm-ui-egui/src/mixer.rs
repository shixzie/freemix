//! Audio mixer: vertical input channel strips plus a Master meter strip.
//!
//! Meters display the latest lossy daemon sample only; with no sample the
//! meter wells stay unlit and the header reports meters as unavailable.

use egui::{
    Color32, CornerRadius, DragValue, Rect, RichText, ScrollArea, Sense, Slider, Stroke,
    StrokeKind, Ui, Vec2, pos2, style::HandleShape,
};
use fm_protocol::AudioMeterChannel;
use fm_ui_model::{ClientView, InputAudioStripStatus};

use crate::monitors::label_for_input;
use crate::theme::{
    ACCENT, ACCENT_FILL, AMBER, AMBER_FILL, BORDER, BORDER_STRONG, GRAPHITE_RAISED, MONITOR_BLACK,
    MUTED, PREVIEW, PROGRAM, PROGRAM_FILL, TEXT, TEXT_DIM, WIDGET, mono, section_header,
    tally_color,
};
use crate::{
    InputAudioStripUpdate, StudioConnectionStatus, StudioIntent, StudioUiState, tally_state,
};

/// Width of one channel strip.
pub(crate) const STRIP_WIDTH: f32 = 82.0;
const STRIP_GAP: f32 = 6.0;
const MIN_FADER_HEIGHT: f32 = 140.0;
const MAX_FADER_HEIGHT: f32 = 300.0;
/// Vertical space a strip needs besides its fader and meters.
const STRIP_CHROME_HEIGHT: f32 = 196.0;
/// Lowest level shown on the meter scale.
const METER_FLOOR_DB: f32 = -60.0;
/// Meter segment boundaries: green below, yellow between, red above.
const METER_YELLOW_DB: f32 = -18.0;
const METER_RED_DB: f32 = -6.0;
const METER_SEGMENTS: usize = 30;

/// Total width the mixer wants for `strips` input strips plus Master.
#[allow(clippy::cast_precision_loss)] // strip counts are tiny
pub(crate) fn mixer_width(strips: usize) -> f32 {
    (strips as f32 + 1.0) * (STRIP_WIDTH + 2.0 + STRIP_GAP) + 34.0
}

/// Converts a linear meter value in millionths of full scale to dBFS.
pub(crate) fn meter_db(millionths: u32) -> f32 {
    if millionths == 0 {
        return f32::NEG_INFINITY;
    }
    #[allow(clippy::cast_possible_truncation)] // dB values are small
    let db = (20.0 * (f64::from(millionths) / 1_000_000.0).log10()) as f32;
    db
}

/// Maps a dBFS level onto the meter's 0..=1 travel.
pub(crate) fn meter_fraction(db: f32) -> f32 {
    if db.is_nan() || db <= METER_FLOOR_DB {
        0.0
    } else {
        ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
    }
}

/// Color of the meter segment whose lower edge sits at `db`.
pub(crate) fn meter_segment_color(db: f32) -> Color32 {
    if db >= METER_RED_DB {
        PROGRAM
    } else if db >= METER_YELLOW_DB {
        Color32::from_rgb(232, 200, 40)
    } else {
        PREVIEW
    }
}

/// Formats a stereo balance in basis points as `C`, `L 25`, or `R 25` percent.
pub(crate) fn format_balance(basis_points: f64) -> String {
    #[allow(clippy::cast_possible_truncation)] // the range is bounded to +/-10000
    let percent = (basis_points / 100.0).round() as i64;
    match percent {
        0 => "C".to_owned(),
        value if value < 0 => format!("L{}", -value),
        value => format!("R{value}"),
    }
}

/// Parses `C`, `L25`, `R25`, or a signed percent back to basis points.
pub(crate) fn parse_balance(text: &str) -> Option<f64> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    if upper == "C" {
        return Some(0.0);
    }
    let (sign, digits) = if let Some(rest) = upper.strip_prefix('L') {
        (-1.0, rest)
    } else if let Some(rest) = upper.strip_prefix('R') {
        (1.0, rest)
    } else {
        (1.0, upper.as_str())
    };
    digits
        .trim()
        .parse::<f64>()
        .ok()
        .map(|percent| sign * percent * 100.0)
}

/// Pure placeholder decision for the mixer header meter notice.
const fn meters_unavailable(state: &StudioUiState) -> bool {
    state.audio_meters.is_none()
        && matches!(state.connection_status, StudioConnectionStatus::Ready)
        && state.view.is_some()
}

pub(crate) fn draw_mixer(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let fader_height =
        (ui.available_height() - STRIP_CHROME_HEIGHT).clamp(MIN_FADER_HEIGHT, MAX_FADER_HEIGHT);
    let enabled = state.connection_status.controls_enabled()
        && state.can_control_audio
        && state.view.is_some();
    section_header(ui, "Audio mixer", |ui| {
        if meters_unavailable(state) {
            ui.label(RichText::new("METERS UNAVAILABLE").size(10.0).color(AMBER))
                .on_hover_text("No audio meter sample has been received from the daemon");
        } else if state.audio_meters.is_some() {
            ui.label(RichText::new("PEAK · RMS").size(10.0).color(MUTED));
        }
    });
    let Some(view) = &state.view else {
        ui.label(
            RichText::new("INPUT AUDIO UNAVAILABLE")
                .size(11.0)
                .color(MUTED),
        );
        return;
    };
    // Master stays pinned at the right edge; input strips scroll when the
    // mixer is narrower than all strips.
    let master_width = STRIP_WIDTH + 2.0 + STRIP_GAP;
    let inputs_width = (ui.available_width() - master_width).max(STRIP_WIDTH);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = STRIP_GAP;
        ui.allocate_ui_with_layout(
            Vec2::new(inputs_width, ui.available_height()),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                ScrollArea::horizontal()
                    .id_salt("studio-mixer")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = STRIP_GAP;
                            for status in &view.input_audio_strips {
                                let channels = state.audio_meters.as_ref().and_then(|meters| {
                                    meters
                                        .inputs
                                        .iter()
                                        .find(|meter| meter.input.to_domain() == status.input)
                                        .map(|meter| meter.channels.as_slice())
                                });
                                draw_input_strip(
                                    ui,
                                    view,
                                    status,
                                    channels,
                                    enabled,
                                    fader_height,
                                    intents,
                                );
                            }
                        });
                    });
            },
        );
        draw_master_strip(
            ui,
            state
                .audio_meters
                .as_ref()
                .map(|meters| meters.master.as_slice()),
            fader_height,
        );
    });
}

fn strip_frame(ui: &mut Ui, accent: Color32, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(GRAPHITE_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 6))
        .show(ui, |ui| {
            ui.set_width(STRIP_WIDTH - 10.0);
            // Compact numerics so every entry fits the fixed strip width.
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::new(10.5, egui::FontFamily::Monospace),
            );
            ui.spacing_mut().button_padding.x = 4.0;
            let top = ui.max_rect().min;
            ui.painter().rect_filled(
                Rect::from_min_size(top - Vec2::new(5.0, 6.0), Vec2::new(STRIP_WIDTH, 2.0)),
                CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                accent,
            );
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 4.0);
                add_contents(ui);
            });
        });
}

#[allow(clippy::too_many_lines)] // one immediate-mode region
fn draw_input_strip(
    ui: &mut Ui,
    view: &ClientView,
    status: &InputAudioStripStatus,
    channels: Option<&[AudioMeterChannel]>,
    enabled: bool,
    fader_height: f32,
    intents: &mut Vec<StudioIntent>,
) {
    let name = label_for_input(view, status.input);
    let tally = tally_state(status.input, view.switcher);
    let accent = if tally.is_none() {
        BORDER_STRONG
    } else {
        tally_color(tally)
    };
    let mut gain_millidb = status.gain_millidb;
    let mut balance_basis_points = status.balance_basis_points;
    let mut muted = status.muted;
    let mut soloed = status.soloed;
    let mut follow_video = status.follow_video;
    let mut delay_samples = status.delay_samples;
    let mut gain_changed = false;
    let mut balance_changed = false;
    let mut mute_changed = false;
    let mut solo_changed = false;
    let mut follow_changed = false;
    let mut delay_changed = false;
    strip_frame(ui, accent, |ui| {
        ui.add(egui::Label::new(RichText::new(&name).size(11.0).strong().color(TEXT)).truncate())
            .on_hover_text(&name);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            draw_meter(ui, channels, fader_height);
            let fader_changed = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = fader_height;
                    ui.spacing_mut().interact_size.x = 26.0;
                    ui.spacing_mut().slider_rail_height = 4.0;
                    ui.visuals_mut().selection.bg_fill = ACCENT.gamma_multiply(0.55);
                    ui.add_enabled(
                        enabled,
                        Slider::new(&mut gain_millidb, -96_000..=24_000)
                            .vertical()
                            .step_by(100.0)
                            .show_value(false)
                            .trailing_fill(true)
                            .handle_shape(HandleShape::Rect { aspect_ratio: 0.5 }),
                    )
                    .on_hover_text("Input gain fader")
                    .changed()
                })
                .inner;
            gain_changed |= fader_changed;
        });
        ui.vertical_centered_justified(|ui| {
            let entry_changed = ui
                .add_enabled(
                    enabled,
                    DragValue::new(&mut gain_millidb)
                        .range(-96_000..=24_000)
                        .speed(100.0)
                        .custom_formatter(|value, _| format!("{:+.1} dB", value / 1000.0))
                        .custom_parser(|text| {
                            text.trim()
                                .trim_end_matches("dB")
                                .trim_end_matches("db")
                                .trim()
                                .parse::<f64>()
                                .ok()
                                .map(|db| db * 1000.0)
                        }),
                )
                .on_hover_text("Input gain in decibels (stored in one-thousandths of a dB)")
                .changed();
            gain_changed |= entry_changed;
            balance_changed = ui
                .add_enabled(
                    enabled,
                    DragValue::new(&mut balance_basis_points)
                        .range(-10_000..=10_000)
                        .speed(100.0)
                        .prefix("BAL ")
                        .custom_formatter(|value, _| format_balance(value))
                        .custom_parser(parse_balance),
                )
                .on_hover_text("Stereo balance: L100 full left, C center, R100 full right")
                .changed();
            delay_changed = ui
                .add_enabled(
                    enabled,
                    DragValue::new(&mut delay_samples)
                        .range(0..=48_000)
                        .speed(48.0)
                        .prefix("DLY ")
                        .suffix(" smp"),
                )
                .on_hover_text("Input delay in samples at the 48 kHz Master sample rate")
                .changed();
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let half = (STRIP_WIDTH - 13.0) / 2.0;
            mute_changed = toggle(ui, enabled, &mut muted, "M", PROGRAM, PROGRAM_FILL, half)
                .on_hover_text("Mute this input")
                .changed();
            solo_changed = toggle(ui, enabled, &mut soloed, "S", AMBER, AMBER_FILL, half)
                .on_hover_text("Solo this input")
                .changed();
        });
        follow_changed = toggle(
            ui,
            enabled,
            &mut follow_video,
            "AFV",
            ACCENT,
            ACCENT_FILL,
            STRIP_WIDTH - 10.0,
        )
        .on_hover_text("Audio follows video: open only while the input is on Program")
        .changed();
    });
    let update = InputAudioStripUpdate {
        gain_millidb: gain_changed.then_some(gain_millidb),
        balance_basis_points: balance_changed.then_some(balance_basis_points),
        muted: mute_changed.then_some(muted),
        soloed: solo_changed.then_some(soloed),
        follow_video: follow_changed.then_some(follow_video),
        delay_samples: delay_changed.then_some(delay_samples),
    };
    if update != InputAudioStripUpdate::default() {
        intents.push(StudioIntent::SetInputAudioStrip {
            input: status.input,
            update,
        });
    }
}

/// A latching strip key; `changed()` reports a click that flipped `value`.
fn toggle(
    ui: &mut Ui,
    enabled: bool,
    value: &mut bool,
    label: &str,
    color: Color32,
    fill: Color32,
    width: f32,
) -> egui::Response {
    let on = *value;
    let button = egui::Button::new(RichText::new(label).size(11.0).strong().color(if on {
        Color32::WHITE
    } else {
        TEXT_DIM
    }))
    .fill(if on { fill } else { WIDGET })
    .stroke(Stroke::new(1.0, if on { color } else { BORDER }))
    .min_size(Vec2::new(width, 20.0));
    let mut response = ui.add_enabled(enabled, button);
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response
}

fn draw_master_strip(ui: &mut Ui, channels: Option<&[AudioMeterChannel]>, fader_height: f32) {
    strip_frame(ui, ACCENT, |ui| {
        ui.label(RichText::new("MASTER").size(11.0).strong().color(TEXT));
        ui.horizontal_top(|ui| {
            draw_meter(ui, channels, fader_height);
            draw_meter_scale(ui, fader_height);
        });
        let peak = channels
            .and_then(|channels| channels.iter().map(|channel| channel.peak_millionths).max());
        ui.vertical_centered(|ui| {
            ui.label(
                mono(peak.map_or_else(
                    || "-- dB".to_owned(),
                    |peak| {
                        let db = meter_db(peak);
                        if db.is_finite() {
                            format!("{db:+.1} dB")
                        } else {
                            "-inf dB".to_owned()
                        }
                    },
                ))
                .size(11.0)
                .color(TEXT_DIM),
            )
            .on_hover_text("Latest Master peak");
            ui.add(
                egui::Label::new(RichText::new("PRE-STINGER/ FTB").size(8.5).color(MUTED)).wrap(),
            )
            .on_hover_text("Master meters are taken before Stinger and Fade-to-Black");
        });
    });
}

/// Draws one segmented meter well with a bar per channel: RMS as the lit
/// segments and peak as a hold line. Without channels the well stays unlit.
fn draw_meter(ui: &mut Ui, channels: Option<&[AudioMeterChannel]>, height: f32) {
    let bars = channels.map_or(2, |channels| channels.len().clamp(1, 2));
    #[allow(clippy::cast_precision_loss)] // one or two bars
    let width = bars as f32 * 7.0 + 4.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), MONITOR_BLACK);
    let inner = rect.shrink(2.0);
    #[allow(clippy::cast_precision_loss)]
    let segment_height = inner.height() / METER_SEGMENTS as f32;
    for bar in 0..bars {
        #[allow(clippy::cast_precision_loss)]
        let x = inner.min.x + bar as f32 * 7.0;
        let channel = channels.and_then(|channels| channels.get(bar));
        let rms = channel.map_or(0.0, |channel| {
            meter_fraction(meter_db(channel.rms_millionths))
        });
        let peak = channel.map_or(0.0, |channel| {
            meter_fraction(meter_db(channel.peak_millionths))
        });
        for segment in 0..METER_SEGMENTS {
            #[allow(clippy::cast_precision_loss)]
            let low = segment as f32 / METER_SEGMENTS as f32;
            let db = METER_FLOOR_DB + low * -METER_FLOOR_DB;
            let color = meter_segment_color(db);
            #[allow(clippy::cast_precision_loss)]
            let y_bottom = inner.max.y - segment as f32 * segment_height;
            let segment_rect = Rect::from_min_max(
                pos2(x, y_bottom - segment_height + 1.0),
                pos2(x + 5.0, y_bottom),
            );
            let lit = channel.is_some() && low < rms;
            painter.rect_filled(
                segment_rect,
                CornerRadius::ZERO,
                if lit {
                    color
                } else {
                    color.gamma_multiply(0.12)
                },
            );
        }
        if channel.is_some() && peak > 0.0 {
            let y = inner.max.y - peak * inner.height();
            painter.line_segment(
                [pos2(x, y), pos2(x + 5.0, y)],
                Stroke::new(
                    2.0,
                    meter_segment_color(METER_FLOOR_DB + peak * -METER_FLOOR_DB),
                ),
            );
        }
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    if let Some(channels) = channels {
        let text = channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                format!(
                    "{} P {:.3} R {:.3}",
                    index + 1,
                    f64::from(channel.peak_millionths) / 1_000_000.0,
                    f64::from(channel.rms_millionths) / 1_000_000.0
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        response.on_hover_text(text);
    } else {
        response.on_hover_text("No meter sample");
    }
}

fn draw_meter_scale(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(22.0, height), Sense::hover());
    let painter = ui.painter_at(rect.expand2(Vec2::new(0.0, 6.0)));
    for db in [0.0_f32, -6.0, -12.0, -18.0, -30.0, -48.0, -60.0] {
        let y = rect.max.y - 2.0 - meter_fraction(db) * (rect.height() - 4.0);
        painter.text(
            pos2(rect.min.x + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{}", db.abs()),
            egui::FontId::new(8.5, egui::FontFamily::Monospace),
            MUTED,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_math_maps_full_scale_and_floor() {
        assert!((meter_db(1_000_000) - 0.0).abs() < 1e-4);
        assert!((meter_db(500_000) + 6.0206).abs() < 1e-3);
        assert!(meter_db(0).is_infinite());
        assert!((meter_fraction(0.0) - 1.0).abs() < f32::EPSILON);
        assert!(meter_fraction(f32::NEG_INFINITY).abs() < f32::EPSILON);
        assert!(meter_fraction(-90.0).abs() < f32::EPSILON);
        assert!((meter_fraction(-30.0) - 0.5).abs() < 1e-6);
        assert!((meter_fraction(6.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn meter_segments_are_green_yellow_red() {
        assert_eq!(meter_segment_color(-40.0), PREVIEW);
        assert_eq!(meter_segment_color(-12.0), Color32::from_rgb(232, 200, 40));
        assert_eq!(meter_segment_color(-3.0), PROGRAM);
    }

    #[test]
    fn balance_formatting_round_trips() {
        assert_eq!(format_balance(0.0), "C");
        assert_eq!(format_balance(-2_500.0), "L25");
        assert_eq!(format_balance(10_000.0), "R100");
        assert_eq!(parse_balance("c"), Some(0.0));
        assert_eq!(parse_balance("L25"), Some(-2_500.0));
        assert_eq!(parse_balance(" r 40 "), Some(4_000.0));
        assert_eq!(parse_balance("-10"), Some(-1_000.0));
        assert_eq!(parse_balance("left"), None);
    }

    #[test]
    fn meter_notice_requires_ready_view_without_sample() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert!(!meters_unavailable(&state));
        let state = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert!(!meters_unavailable(&state));
    }
}
