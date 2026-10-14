//! Audio mixer: vertical input channel strips plus a Master meter strip.
//!
//! Meters display the latest lossy daemon sample only (per-channel peak and
//! RMS); with no sample the meter wells stay unlit and the header reports
//! meters as unavailable. Peak hold and the clip latch are Studio-local
//! ballistics over those samples; a clip lamp stays lit until clicked. When
//! no new meter message has arrived for over a second the meters keep the
//! last sample dimmed, each readout reads `STALE`, and the header carries a
//! `STALE` badge with the message age, as stale monitor frames do.

use egui::{
    Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Key, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::AudioMeterChannel;
use fm_ui_model::{ClientView, InputAudioStripStatus};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::meter_ballistics::{
    ChannelBallistics, MeterBallistics, MeterKey, held_peak_mdb, peak_readout,
};
use crate::monitors::label_for_input;
use crate::theme::{
    AMBER, MONITOR_BLACK, PREVIEW, PROGRAM, accent_color, border_color, border_strong_color,
    graphite_color, muted_color, raised_color, section_header, tally_color, text_color,
    text_dim_color, widget_color, widget_hover_color,
};
use crate::{
    InputAudioStripUpdate, StudioConnectionStatus, StudioIntent, StudioUiState, TallyState,
    tally_state,
};

/// Width of one channel strip.
pub(crate) const STRIP_WIDTH: f32 = 82.0;
/// Content width inside a strip frame.
const STRIP_INNER_WIDTH: f32 = STRIP_WIDTH - 10.0;
const STRIP_GAP: f32 = 6.0;
const MIN_FADER_HEIGHT: f32 = 120.0;
const MAX_FADER_HEIGHT: f32 = 620.0;
/// Vertical space a strip needs besides its fader and meters.
const STRIP_CHROME_HEIGHT: f32 = 224.0;
/// Fixed height of the strip name plate (two lines of name).
const NAME_PLATE_HEIGHT: f32 = 30.0;
/// Height of the peak-hold readout above the meter.
const READOUT_HEIGHT: f32 = 16.0;
/// Lowest level shown on the meter scale.
const METER_FLOOR_DB: f32 = -60.0;
/// Meter zone boundaries: green below, amber between, red above.
const METER_YELLOW_DB: f32 = -18.0;
const METER_RED_DB: f32 = -6.0;
/// Segment count bounds; the count follows the meter height so each
/// segment stays about [`METER_SEGMENT_PITCH`] tall.
const METER_MIN_SEGMENTS: usize = 24;
const METER_MAX_SEGMENTS: usize = 120;
const METER_SEGMENT_PITCH: f32 = 4.0;
/// Height of the clip lamp above each meter bar.
const CLIP_LAMP_HEIGHT: f32 = 5.0;
/// Meter scale marks in dBFS.
pub(crate) const METER_TICKS_DB: [f32; 8] = [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0];
/// The meter scale: (dBFS, travel 0..=1) breakpoints, piecewise linear in
/// dB between them. Like a broadcast programme meter the scale expands
/// toward full scale, so the working range from -24 to 0 dBFS takes most of
/// the height and the noise floor is compressed.
const METER_SCALE: [(f32, f32); 8] = [
    (-60.0, 0.0),
    (-48.0, 0.08),
    (-36.0, 0.18),
    (-24.0, 0.32),
    (-18.0, 0.44),
    (-12.0, 0.59),
    (-6.0, 0.78),
    (0.0, 1.0),
];
/// Width of the tick-mark gutter on the right of a meter well.
const METER_TICK_GUTTER: f32 = 4.0;
/// Fader travel breakpoints: (travel 0..=1, gain in milli-dB). Piecewise
/// linear in dB between them, so the useful range around unity gets most of
/// the travel, as on a console fader.
const FADER_TAPER: [(f32, i32); 8] = [
    (0.0, -96_000),
    (0.08, -60_000),
    (0.22, -40_000),
    (0.42, -20_000),
    (0.57, -10_000),
    (0.72, 0),
    (0.86, 10_000),
    (1.0, 24_000),
];
/// Fader scale marks: (gain in milli-dB, label).
pub(crate) const FADER_TICKS: [(i32, &str); 7] = [
    (10_000, "+10"),
    (0, "0"),
    (-10_000, "10"),
    (-20_000, "20"),
    (-40_000, "40"),
    (-60_000, "60"),
    (-96_000, "∞"),
];
/// Gain step of the fader and its keyboard nudges, in milli-dB.
const FADER_STEP_MDB: i32 = 100;
const FADER_KEY_STEP_MDB: i32 = 500;
const FADER_CAP: Vec2 = Vec2::new(26.0, 14.0);

/// Total width the mixer wants for `strips` input strips plus Master.
#[allow(clippy::cast_precision_loss)] // strip counts are tiny
pub(crate) fn mixer_width(strips: usize) -> f32 {
    (strips as f32 + 1.0) * (STRIP_WIDTH + 2.0 + STRIP_GAP) + 34.0
}

/// Maps a gain in milli-dB onto the fader's 0..=1 travel.
pub(crate) fn fader_position(mdb: i32) -> f32 {
    let (first, last) = (FADER_TAPER[0], FADER_TAPER[FADER_TAPER.len() - 1]);
    if mdb <= first.1 {
        return first.0;
    }
    if mdb >= last.1 {
        return last.0;
    }
    FADER_TAPER
        .windows(2)
        .find(|pair| mdb <= pair[1].1)
        .map_or(last.0, |pair| {
            let ((p0, g0), (p1, g1)) = (pair[0], pair[1]);
            #[allow(clippy::cast_precision_loss)] // milli-dB spans fit f32 exactly
            let t = (mdb - g0) as f32 / (g1 - g0) as f32;
            p0 + t * (p1 - p0)
        })
}

/// Maps a fader travel position back to a gain, snapped to the 0.1 dB step.
pub(crate) fn fader_mdb(position: f32) -> i32 {
    let position = if position.is_nan() {
        0.0
    } else {
        position.clamp(0.0, 1.0)
    };
    let pair = FADER_TAPER
        .windows(2)
        .find(|pair| position <= pair[1].0)
        .unwrap_or(&FADER_TAPER[FADER_TAPER.len() - 2..]);
    let ((p0, g0), (p1, g1)) = (pair[0], pair[1]);
    let t = f64::from((position - p0) / (p1 - p0));
    let mdb = f64::from(g0) + t * f64::from(g1 - g0);
    #[allow(clippy::cast_possible_truncation)] // bounded range
    let snapped = ((mdb / f64::from(FADER_STEP_MDB)).round() as i32) * FADER_STEP_MDB;
    snapped.clamp(FADER_TAPER[0].1, FADER_TAPER[FADER_TAPER.len() - 1].1)
}

/// Formats a fader gain for its dB entry and tooltip: `+0.0 dB`, `-inf dB`
/// at the bottom of the travel.
pub(crate) fn format_gain(l: Locale, mdb: i32) -> String {
    l.decibels(
        f64::from(mdb) / 1000.0,
        f64::from(FADER_TAPER[0].1) / 1000.0,
    )
}

/// Parses a typed gain in dB (`-6.5`, `+3 dB`, `-inf`) to milli-dB. Text that
/// is not a number is refused: `NaN` would otherwise reach the entry as 0 dB.
pub(crate) fn parse_gain(l: Locale, text: &str) -> Option<f64> {
    l.parse_decimal(text.trim().trim_end_matches("dB").trim_end_matches("db"))
        .map(|db| db * 1000.0)
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

/// Maps a dBFS level onto the meter's 0..=1 travel along [`METER_SCALE`].
pub(crate) fn meter_fraction(db: f32) -> f32 {
    if db.is_nan() || db <= METER_FLOOR_DB {
        return 0.0;
    }
    METER_SCALE
        .windows(2)
        .find(|pair| db <= pair[1].0)
        .map_or(1.0, |pair| {
            let ((d0, f0), (d1, f1)) = (pair[0], pair[1]);
            f0 + (db - d0) / (d1 - d0) * (f1 - f0)
        })
}

/// The dBFS level at a 0..=1 meter travel; the inverse of
/// [`meter_fraction`] on the scale.
pub(crate) fn meter_db_at(fraction: f32) -> f32 {
    let fraction = if fraction.is_nan() {
        0.0
    } else {
        fraction.clamp(0.0, 1.0)
    };
    METER_SCALE
        .windows(2)
        .find(|pair| fraction <= pair[1].1)
        .map_or(0.0, |pair| {
            let ((d0, f0), (d1, f1)) = (pair[0], pair[1]);
            d0 + (fraction - f0) / (f1 - f0) * (d1 - d0)
        })
}

/// How many segments a meter of `height` points shows.
pub(crate) fn meter_segments(height: f32) -> usize {
    if !height.is_finite() || height <= 0.0 {
        return METER_MIN_SEGMENTS;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positive, bounded
    let count = (height / METER_SEGMENT_PITCH).round() as usize;
    count.clamp(METER_MIN_SEGMENTS, METER_MAX_SEGMENTS)
}

/// How one meter segment is lit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SegmentLight {
    /// Below the RMS level: fully lit.
    Rms,
    /// Between the RMS level and the latest peak: dimly lit.
    Peak,
    /// Above the latest peak: only the zone tint shows.
    Unlit,
}

/// Lights the segment whose lower edge sits at `low` (meter travel), given
/// the RMS and latest-peak travel of its bar.
pub(crate) fn segment_light(low: f32, rms: f32, peak: f32) -> SegmentLight {
    if low < rms {
        SegmentLight::Rms
    } else if low < peak {
        SegmentLight::Peak
    } else {
        SegmentLight::Unlit
    }
}

/// The Master scale's label for a tick: `0`, `-6`, … (ASCII minus, as on
/// hardware meter bridges).
pub(crate) fn meter_tick_label(db: f32) -> String {
    #[allow(clippy::cast_possible_truncation)] // whole-dB ticks
    let whole = db.round() as i32;
    format!("{whole}")
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
        .filter(|percent| !percent.is_nan())
        .map(|percent| sign * percent * 100.0)
}

/// Pure placeholder decision for the mixer header meter notice.
const fn meters_unavailable(state: &StudioUiState) -> bool {
    state.audio_meters.is_none()
        && matches!(state.connection_status, StudioConnectionStatus::Ready)
        && state.view.is_some()
}

/// Header badge text for stale meters: `METERS STALE · 2.4 s`.
pub(crate) fn stale_meter_label(l: Locale, age_ms: u32) -> String {
    l.format(
        Msg::MetersStale,
        &[("age", Arg::Str(&l.seconds_tenths(u64::from(age_ms))))],
    )
}

/// Paints a meter element at its full colour, or dimmed while stale.
fn stale_dim(color: Color32, stale: bool) -> Color32 {
    if stale {
        color.gamma_multiply(0.35)
    } else {
        color
    }
}

/// Repaint cadence while meters are shown, so they go stale without a new
/// state arriving.
const METER_REPAINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Header text for the latched clip lamps, when any are lit.
pub(crate) fn clip_reset_label(l: Locale, clipped: usize) -> Option<String> {
    (clipped > 0).then(|| l.format(Msg::ResetClips, &[("count", Arg::from(clipped))]))
}

#[allow(clippy::too_many_lines)] // one immediate-mode region
pub(crate) fn draw_mixer(
    ui: &mut Ui,
    state: &StudioUiState,
    ballistics: &mut MeterBallistics,
    dt_ms: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let fader_height =
        (ui.available_height() - STRIP_CHROME_HEIGHT).clamp(MIN_FADER_HEIGHT, MAX_FADER_HEIGHT);
    let enabled = state.connection_status.controls_enabled()
        && state.can_control_audio
        && state.view.is_some();
    let mut reset_all = false;
    ballistics.begin_frame(
        state.audio_meters.as_ref().map(|meters| meters.sequence),
        dt_ms,
    );
    let meters_stale = ballistics.stale();
    let l = l10n::locale(ui.ctx());
    if state.audio_meters.is_some() {
        ui.ctx().request_repaint_after(METER_REPAINT_INTERVAL);
    }
    section_header(ui, l.text(Msg::ViewAudioMixer), |ui| {
        if meters_unavailable(state) {
            ui.label(
                RichText::new(l.text(Msg::MetersUnavailable))
                    .size(crate::theme::legible(10.0))
                    .color(AMBER),
            )
            .on_hover_text(l.text(Msg::MetersUnavailableHover));
        } else if state.audio_meters.is_some() {
            if let Some(label) = clip_reset_label(l, ballistics.clipped_meters()) {
                reset_all = ui
                    .add(
                        egui::Button::new(
                            RichText::new(label)
                                .size(crate::theme::legible(10.0))
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(PROGRAM.gamma_multiply(0.85))
                        .stroke(Stroke::new(1.0, PROGRAM))
                        .min_size(Vec2::new(0.0, 18.0)),
                    )
                    .on_hover_text(l.text(Msg::ResetClipsHover))
                    .clicked();
            }
            if meters_stale {
                stale_badge(ui, &stale_meter_label(l, ballistics.message_age_ms()));
            }
            ui.label(
                RichText::new(l.text(Msg::MeterLegend))
                    .size(crate::theme::legible(10.0))
                    .color(muted_color()),
            )
            .on_hover_text(l.text(Msg::MeterLegendHover));
        }
    });
    if reset_all {
        ballistics.clear_all_clips();
    }
    let Some(view) = &state.view else {
        ballistics.retain(|_| false);
        crate::theme::empty_state(
            ui,
            l.text(Msg::NoAudioState),
            l.text(Msg::NoAudioStateDetail),
        );
        return;
    };
    ballistics.retain(|key| match key {
        MeterKey::Master => true,
        MeterKey::Input(input) => view
            .input_audio_strips
            .iter()
            .any(|strip| strip.input == input),
    });
    let audio_reason = unavailable_reason(state, Capability::Audio);
    // Master stays pinned at the right edge; input strips scroll when the
    // mixer is narrower than all strips.
    let master_width = STRIP_WIDTH + 2.0 + STRIP_GAP;
    let inputs_width = (ui.available_width() - master_width).max(STRIP_WIDTH);
    let mut clear = Vec::new();
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
                                let key = MeterKey::Input(status.input);
                                let holds = ballistics.update(key, channels, dt_ms).to_vec();
                                if draw_input_strip(
                                    ui,
                                    view,
                                    status,
                                    MeterFeed {
                                        channels,
                                        holds: &holds,
                                        stale: meters_stale,
                                    },
                                    StripGate {
                                        enabled,
                                        reason: audio_reason,
                                    },
                                    fader_height,
                                    intents,
                                ) {
                                    clear.push(key);
                                }
                            }
                        });
                    });
            },
        );
        let master = state
            .audio_meters
            .as_ref()
            .map(|meters| meters.master.as_slice());
        let holds = ballistics.update(MeterKey::Master, master, dt_ms).to_vec();
        if draw_master_strip(
            ui,
            MeterFeed {
                channels: master,
                holds: &holds,
                stale: meters_stale,
            },
            fader_height,
        ) {
            clear.push(MeterKey::Master);
        }
    });
    for key in clear {
        ballistics.clear_clip(key);
    }
    if ballistics.animating() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// One meter's latest sample plus its ballistic state.
#[derive(Clone, Copy)]
struct MeterFeed<'a> {
    channels: Option<&'a [AudioMeterChannel]>,
    holds: &'a [ChannelBallistics],
    /// No new meter message for over a second: draw the last sample dimmed.
    stale: bool,
}

/// The muted `STALE` badge, as on a stale monitor frame.
fn stale_badge(ui: &mut Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(9.5, FontFamily::Proportional),
        Color32::BLACK,
    );
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 12.0, 18.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), muted_color());
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::BLACK);
    response.on_hover_text(l10n::locale(ui.ctx()).text(Msg::MetersStaleHover));
}

/// Whether strip controls are enabled, and why not.
#[derive(Clone, Copy)]
struct StripGate {
    enabled: bool,
    reason: &'static str,
}

fn strip_frame(ui: &mut Ui, accent: Color32, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(raised_color())
        .stroke(Stroke::new(crate::theme::line_width(), border_color()))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 6))
        .show(ui, |ui| {
            ui.set_width(STRIP_INNER_WIDTH);
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

/// The strip name plate: a key cap with the input's ordinal (tally-tinted)
/// and up to two lines of name, ending in an ellipsis; full name on hover.
fn name_plate(ui: &mut Ui, ordinal: Option<usize>, name: &str, tally: TallyState) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(STRIP_INNER_WIDTH, NAME_PLATE_HEIGHT),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let mut text_left = rect.min.x;
        if let Some(ordinal) = ordinal {
            let cap = Rect::from_min_size(rect.min + Vec2::new(0.0, 1.0), Vec2::new(16.0, 14.0));
            let tinted = !tally.is_none();
            painter.rect_filled(
                cap,
                CornerRadius::same(3),
                if tinted {
                    tally_color(tally)
                } else {
                    graphite_color()
                },
            );
            if !tinted {
                painter.rect_stroke(
                    cap,
                    CornerRadius::same(3),
                    Stroke::new(crate::theme::line_width(), border_strong_color()),
                    StrokeKind::Inside,
                );
            }
            painter.text(
                cap.center(),
                Align2::CENTER_CENTER,
                format!("{ordinal}"),
                FontId::new(9.5, FontFamily::Monospace),
                if tinted {
                    Color32::WHITE
                } else {
                    text_dim_color()
                },
            );
            text_left = cap.max.x + 4.0;
        }
        let mut job = egui::text::LayoutJob::simple(
            name.to_owned(),
            FontId::new(10.5, FontFamily::Proportional),
            text_color(),
            rect.max.x - text_left,
        );
        job.wrap.max_rows = 2;
        job.wrap.break_anywhere = false;
        let galley = painter.layout_job(job);
        painter.galley(pos2(text_left, rect.min.y), galley, text_color());
    }
    response.on_hover_text(name);
}

/// The peak-hold readout: the held level, or a red `CLIP` while latched.
/// Returns `true` when clicked while latched (clear request).
fn peak_box(ui: &mut Ui, holds: &[ChannelBallistics], has_sample: bool, stale: bool) -> bool {
    let l = l10n::locale(ui.ctx());
    let clipped = holds.iter().any(|hold| hold.clipping());
    let sense = if clipped {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(STRIP_INNER_WIDTH, READOUT_HEIGHT), sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = clipped && response.hovered();
        painter.rect_filled(
            rect,
            CornerRadius::same(2),
            if clipped {
                PROGRAM.gamma_multiply(if hovered { 1.0 } else { 0.85 })
            } else {
                MONITOR_BLACK
            },
        );
        let stale_readout = stale && has_sample && !clipped;
        painter.rect_stroke(
            rect,
            CornerRadius::same(2),
            Stroke::new(
                1.0,
                if clipped {
                    PROGRAM
                } else if stale_readout {
                    muted_color()
                } else {
                    border_color()
                },
            ),
            StrokeKind::Inside,
        );
        let text = if stale_readout {
            l.text(Msg::BadgeStale).to_owned()
        } else if has_sample {
            peak_readout(holds)
        } else {
            "--".to_owned()
        };
        let color = if clipped {
            Color32::WHITE
        } else if stale_readout {
            muted_color()
        } else {
            held_peak_mdb(holds).map_or(muted_color(), |mdb| {
                #[allow(clippy::cast_precision_loss)] // milli-dB fits f32 exactly
                let db = mdb as f32 / 1_000.0;
                if db > METER_FLOOR_DB {
                    meter_segment_color(db)
                } else {
                    muted_color()
                }
            })
        };
        let galley = crate::theme::elided(
            painter,
            &text,
            FontId::new(10.0, FontFamily::Monospace),
            color,
            rect.width() - 4.0,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    let response = if clipped {
        response.on_hover_text(l.text(Msg::PeakClippedHover))
    } else if stale && has_sample {
        response.on_hover_text(l.format(
            Msg::PeakStaleHover,
            &[("level", Arg::Str(&peak_readout(holds)))],
        ))
    } else {
        response.on_hover_text(l.text(Msg::PeakHoldHover))
    };
    clipped && response.clicked()
}

/// Draws one input strip; returns `true` when the operator cleared its clip.
#[allow(clippy::too_many_lines)] // one immediate-mode region
fn draw_input_strip(
    ui: &mut Ui,
    view: &ClientView,
    status: &InputAudioStripStatus,
    feed: MeterFeed<'_>,
    gate: StripGate,
    fader_height: f32,
    intents: &mut Vec<StudioIntent>,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let StripGate { enabled, reason } = gate;
    let name = label_for_input(l, view, status.input);
    let ordinal = view
        .inputs
        .iter()
        .position(|input| *input == status.input)
        .map(|index| index + 1);
    let tally = tally_state(status.input, view.switcher);
    let accent = if tally.is_none() {
        border_strong_color()
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
    let mut clear_clip = false;
    strip_frame(ui, accent, |ui| {
        name_plate(ui, ordinal, &name, tally);
        clear_clip |= peak_box(ui, feed.holds, feed.channels.is_some(), feed.stale);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            clear_clip |= draw_meter(ui, feed, fader_height);
            let fader_width = ui.available_width();
            gain_changed |= fader(
                ui,
                &mut gain_millidb,
                enabled,
                muted,
                fader_width,
                fader_height,
            )
            .hint(
                &with_key(
                    l,
                    l.text(Msg::FaderHover),
                    &format!("{} ±{} dB", l.text(Msg::KeyArrows), l.decimal(0.5, 1)),
                ),
                reason,
            )
            .changed();
        });
        ui.vertical_centered_justified(|ui| {
            let entry_changed = ui
                .add_enabled(
                    enabled,
                    DragValue::new(&mut gain_millidb)
                        .range(-96_000..=24_000)
                        .speed(100.0)
                        .custom_formatter(|value, _| {
                            #[allow(clippy::cast_possible_truncation)] // bounded range
                            format_gain(l, value as i32)
                        })
                        .custom_parser(|text| parse_gain(l, text)),
                )
                .hint(l.text(Msg::GainEntryHover), reason)
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
                .hint(l.text(Msg::BalanceHover), reason)
                .changed();
            delay_changed = ui
                .add_enabled(
                    enabled,
                    DragValue::new(&mut delay_samples)
                        .range(0..=48_000)
                        .speed(48.0)
                        .prefix(format!("{} ", l.text(Msg::DelayPrefix)))
                        .suffix(" smp"),
                )
                .hint(l.text(Msg::DelayHover), reason)
                .changed();
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let half = (STRIP_INNER_WIDTH - 3.0) / 2.0;
            mute_changed = led_key(ui, enabled, &mut muted, "M", PROGRAM, half)
                .hint(
                    l.text(if muted {
                        Msg::MuteOnHover
                    } else {
                        Msg::MuteOffHover
                    }),
                    reason,
                )
                .changed();
            solo_changed = led_key(ui, enabled, &mut soloed, "S", AMBER, half)
                .hint(
                    l.text(if soloed {
                        Msg::SoloOnHover
                    } else {
                        Msg::SoloOffHover
                    }),
                    reason,
                )
                .changed();
        });
        follow_changed = led_key(
            ui,
            enabled,
            &mut follow_video,
            "AFV",
            accent_color(),
            STRIP_INNER_WIDTH,
        )
        .hint(l.text(Msg::AfvHover), reason)
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
    clear_clip
}

/// A latching console key with an LED bar: lit in `color` while on, an unlit
/// LED and dim label while off. `changed()` reports a click that flipped
/// `value`.
fn led_key(
    ui: &mut Ui,
    enabled: bool,
    value: &mut bool,
    label: &str,
    color: Color32,
    width: f32,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, 22.0), sense);
    if enabled && response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let on = *value;
        let dim = if enabled { 1.0 } else { 0.45 };
        let hovered = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let painter = ui.painter();
        let base = if pressed {
            crate::theme::widget_active_color()
        } else if hovered {
            widget_hover_color()
        } else {
            widget_color()
        };
        painter.rect_filled(rect, CornerRadius::same(3), base);
        if on {
            painter.rect_filled(
                rect,
                CornerRadius::same(3),
                color.gamma_multiply(0.26 * dim),
            );
        }
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(
                crate::theme::line_width(),
                if on {
                    color.gamma_multiply(0.9 * dim)
                } else if hovered {
                    border_strong_color()
                } else {
                    border_color()
                },
            ),
            StrokeKind::Inside,
        );
        let led = Rect::from_center_size(
            pos2(rect.center().x, rect.min.y + 4.0),
            Vec2::new((width * 0.4).clamp(8.0, 18.0), 2.5),
        );
        if on {
            painter.rect_filled(
                led.expand(1.5),
                CornerRadius::same(2),
                color.gamma_multiply(0.3 * dim),
            );
            painter.rect_filled(led, CornerRadius::same(1), color.gamma_multiply(dim));
        } else {
            painter.rect_filled(led, CornerRadius::same(1), MONITOR_BLACK);
        }
        painter.text(
            rect.center() + Vec2::new(0.0, 2.5),
            Align2::CENTER_CENTER,
            label,
            FontId::new(10.5, FontFamily::Proportional),
            if on {
                Color32::WHITE.gamma_multiply(dim)
            } else {
                text_dim_color().gamma_multiply(dim)
            },
        );
    }
    response
}

/// A console fader: a slot with scale marks (unity emphasized), trailing
/// level fill, and a ridged cap with an index line. Drags move the cap
/// relative to the pointer (no jump on grab); double-click returns to 0 dB;
/// Up/Down nudge 0.5 dB while focused. `changed()` reports a new gain.
#[allow(clippy::too_many_lines)] // one painter pass per fader region
fn fader(
    ui: &mut Ui,
    gain_mdb: &mut i32,
    enabled: bool,
    muted: bool,
    width: f32,
    height: f32,
) -> egui::Response {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    let track_x = rect.max.x - FADER_CAP.x / 2.0 - 1.0;
    let top = rect.min.y + FADER_CAP.y / 2.0 + 1.0;
    let bottom = rect.max.y - FADER_CAP.y / 2.0 - 1.0;
    let travel = (bottom - top).max(1.0);
    let original = *gain_mdb;
    if enabled {
        let id = response.id.with("fader-position");
        if response.drag_started() {
            ui.data_mut(|data| data.insert_temp(id, fader_position(*gain_mdb)));
        }
        if response.dragged() {
            let position = ui.data_mut(|data| {
                let position = data.get_temp_mut_or(id, fader_position(*gain_mdb));
                *position = (*position - response.drag_delta().y / travel).clamp(0.0, 1.0);
                *position
            });
            *gain_mdb = fader_mdb(position);
        }
        if response.double_clicked() {
            *gain_mdb = 0;
        }
        if response.clicked() {
            response.request_focus();
        }
        if response.has_focus() {
            let (up, down) = ui.input(|input| {
                (
                    input.key_pressed(Key::ArrowUp),
                    input.key_pressed(Key::ArrowDown),
                )
            });
            if up {
                *gain_mdb = (*gain_mdb + FADER_KEY_STEP_MDB).min(24_000);
            }
            if down {
                *gain_mdb = (*gain_mdb - FADER_KEY_STEP_MDB).max(-96_000);
            }
        }
        if *gain_mdb != original {
            response.mark_changed();
        }
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let dim = if enabled { 1.0 } else { 0.5 };
    let painter = ui.painter_at(rect.expand(2.0));
    let y_for = |mdb: i32| bottom - fader_position(mdb) * travel;
    // Scale marks on both sides of the slot, labels on the left.
    let mut last_label_y = f32::INFINITY;
    for (mdb, label) in FADER_TICKS {
        let y = y_for(mdb);
        let unity = mdb == 0;
        let (reach, color) = if unity {
            (11.0, text_dim_color())
        } else {
            (7.0, border_strong_color())
        };
        for side in [-1.0, 1.0] {
            painter.line_segment(
                [
                    pos2(track_x + side * 4.0, y),
                    pos2(track_x + side * reach, y),
                ],
                Stroke::new(1.0, color.gamma_multiply(dim)),
            );
        }
        if (last_label_y - y).abs() >= 10.0 && track_x - 13.0 > rect.min.x + 6.0 {
            painter.text(
                pos2(track_x - 13.0, y),
                Align2::RIGHT_CENTER,
                label,
                FontId::new(8.5, FontFamily::Monospace),
                (if unity {
                    text_dim_color()
                } else {
                    muted_color()
                })
                .gamma_multiply(dim),
            );
            last_label_y = y;
        }
    }
    // The slot and its trailing level fill.
    let slot = Rect::from_min_max(pos2(track_x - 2.0, top), pos2(track_x + 2.0, bottom));
    painter.rect_filled(slot, CornerRadius::same(2), MONITOR_BLACK);
    let cap_y = y_for(*gain_mdb);
    painter.rect_filled(
        Rect::from_min_max(
            pos2(slot.min.x + 1.0, cap_y),
            pos2(slot.max.x - 1.0, bottom),
        ),
        CornerRadius::same(1),
        (if muted { muted_color() } else { accent_color() }).gamma_multiply(0.55 * dim),
    );
    painter.rect_stroke(
        slot,
        CornerRadius::same(2),
        Stroke::new(crate::theme::line_width(), border_color()),
        StrokeKind::Outside,
    );
    // The cap: shadow, two-tone body, ridges, and a bright index line.
    let cap = Rect::from_center_size(pos2(track_x, cap_y), FADER_CAP);
    painter.rect_filled(
        cap.translate(Vec2::new(0.0, 2.0)),
        CornerRadius::same(3),
        Color32::from_black_alpha(140),
    );
    let lit = enabled && (response.hovered() || response.dragged());
    let body_top = if lit {
        Color32::from_rgb(92, 100, 112)
    } else {
        Color32::from_rgb(78, 85, 96)
    };
    let body_bottom = Color32::from_rgb(46, 51, 59);
    painter.rect_filled(cap, CornerRadius::same(3), body_bottom.gamma_multiply(dim));
    painter.rect_filled(
        Rect::from_min_max(cap.min, pos2(cap.max.x, cap.center().y)),
        CornerRadius {
            nw: 3,
            ne: 3,
            sw: 0,
            se: 0,
        },
        body_top.gamma_multiply(dim),
    );
    for offset in [-4.0, -2.5, 2.5, 4.0] {
        let y = cap.center().y + offset;
        painter.line_segment(
            [pos2(cap.min.x + 4.0, y), pos2(cap.max.x - 4.0, y)],
            Stroke::new(0.75, Color32::from_black_alpha(120)),
        );
    }
    painter.line_segment(
        [
            pos2(cap.min.x + 1.5, cap.center().y),
            pos2(cap.max.x - 1.5, cap.center().y),
        ],
        Stroke::new(
            1.5,
            (if muted { PROGRAM } else { Color32::WHITE }).gamma_multiply(dim),
        ),
    );
    painter.rect_stroke(
        cap,
        CornerRadius::same(3),
        Stroke::new(
            1.0,
            if response.dragged() || response.has_focus() {
                accent_color()
            } else {
                Color32::from_black_alpha(200)
            },
        ),
        StrokeKind::Inside,
    );
    if response.dragged() {
        // Live readout beside the cap while dragging.
        painter.text(
            pos2(cap.min.x - 3.0, cap_y - FADER_CAP.y),
            Align2::RIGHT_CENTER,
            format_gain(l10n::locale(painter.ctx()), *gain_mdb),
            FontId::new(9.5, FontFamily::Monospace),
            text_color(),
        );
    }
    response
}

/// Draws the Master strip; returns `true` when the operator cleared its clip.
fn draw_master_strip(ui: &mut Ui, feed: MeterFeed<'_>, fader_height: f32) -> bool {
    let l = l10n::locale(ui.ctx());
    let mut clear_clip = false;
    strip_frame(ui, accent_color(), |ui| {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(STRIP_INNER_WIDTH, NAME_PLATE_HEIGHT),
            Sense::hover(),
        );
        ui.painter().text(
            rect.left_top(),
            Align2::LEFT_TOP,
            l.text(Msg::MasterStrip),
            FontId::new(11.0, FontFamily::Proportional),
            text_color(),
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(0.0, 15.0),
            Align2::LEFT_TOP,
            l.text(Msg::ProgramMix),
            FontId::new(8.5, FontFamily::Proportional),
            muted_color(),
        );
        clear_clip |= peak_box(ui, feed.holds, feed.channels.is_some(), feed.stale);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            clear_clip |= draw_meter(ui, feed, fader_height);
            draw_meter_scale(ui, fader_height);
        });
        ui.vertical_centered(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(l.text(Msg::MasterTap))
                        .size(crate::theme::legible(8.5))
                        .color(muted_color()),
                )
                .wrap(),
            )
            .on_hover_text(l.text(Msg::MasterTapHover));
        });
    });
    clear_clip
}

/// Width of a meter well for `bars` channel bars, including its tick gutter.
#[allow(clippy::cast_precision_loss)] // one or two bars
fn meter_width(bars: usize) -> f32 {
    bars as f32 * 7.0 + 4.0 + METER_TICK_GUTTER
}

/// Draws one segmented meter well with a bar per channel: RMS as the lit
/// segments, the latest peak as a thin line, the held peak as a bright marker,
/// a clip lamp on top, and scale ticks in a gutter on the right. Without
/// channels the well stays unlit. Returns `true` when a latched clip lamp was
/// clicked (clear request).
#[allow(clippy::too_many_lines)] // one painter pass per meter region
fn draw_meter(ui: &mut Ui, feed: MeterFeed<'_>, height: f32) -> bool {
    let l = l10n::locale(ui.ctx());
    let MeterFeed {
        channels,
        holds,
        stale,
    } = feed;
    let bars = channels.map_or(2, |channels| channels.len().clamp(1, 2));
    let clipped = channels.is_some() && holds.iter().any(|hold| hold.clipping());
    let sense = if clipped {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (outer, response) = ui.allocate_exact_size(Vec2::new(meter_width(bars), height), sense);
    if !ui.is_rect_visible(outer) {
        return false;
    }
    let rect = Rect::from_min_max(
        outer.min,
        pos2(outer.max.x - METER_TICK_GUTTER, outer.max.y),
    );
    let painter = ui.painter_at(outer);
    painter.rect_filled(rect, CornerRadius::same(2), MONITOR_BLACK);
    let well = rect.shrink(2.0);
    let inner = Rect::from_min_max(
        pos2(well.min.x, well.min.y + CLIP_LAMP_HEIGHT + 2.0),
        well.max,
    );
    // Scale ticks in the gutter, aligned with the segment scale.
    for db in METER_TICKS_DB {
        let y = inner.max.y - meter_fraction(db) * inner.height();
        let y = y.clamp(inner.min.y + 0.5, inner.max.y - 0.5);
        painter.line_segment(
            [pos2(rect.max.x + 1.0, y), pos2(outer.max.x, y)],
            Stroke::new(
                1.0,
                if db >= 0.0 {
                    PROGRAM.gamma_multiply(0.8)
                } else {
                    muted_color()
                },
            ),
        );
    }
    let segments = meter_segments(inner.height());
    #[allow(clippy::cast_precision_loss)]
    let segment_height = inner.height() / segments as f32;
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
        for segment in 0..segments {
            #[allow(clippy::cast_precision_loss)]
            let low = segment as f32 / segments as f32;
            let color = meter_segment_color(meter_db_at(low));
            #[allow(clippy::cast_precision_loss)]
            let y_bottom = inner.max.y - segment as f32 * segment_height;
            let segment_rect = Rect::from_min_max(
                pos2(x, y_bottom - segment_height + 1.0),
                pos2(x + 5.0, y_bottom),
            );
            let light = if channel.is_some() {
                segment_light(low, rms, peak)
            } else {
                SegmentLight::Unlit
            };
            painter.rect_filled(
                segment_rect,
                CornerRadius::ZERO,
                match light {
                    SegmentLight::Rms => stale_dim(color, stale),
                    SegmentLight::Peak => stale_dim(color.gamma_multiply(0.45), stale),
                    // The zone tint stays visible on an unlit meter.
                    SegmentLight::Unlit => color.gamma_multiply(0.16),
                },
            );
        }
        if channel.is_some() && peak > 0.0 {
            let y = inner.max.y - peak * inner.height();
            painter.line_segment(
                [pos2(x, y), pos2(x + 5.0, y)],
                Stroke::new(
                    1.0,
                    stale_dim(
                        meter_segment_color(meter_db_at(peak)).gamma_multiply(0.9),
                        stale,
                    ),
                ),
            );
        }
        let hold = channel.and(holds.get(bar)).copied();
        if let Some(hold) = hold {
            #[allow(clippy::cast_precision_loss)] // milli-dB fits f32 exactly
            let held_db = hold.held_mdb as f32 / 1_000.0;
            let fraction = meter_fraction(held_db);
            if fraction > 0.0 {
                let y = inner.max.y - fraction * inner.height();
                painter.line_segment(
                    [pos2(x - 0.5, y), pos2(x + 5.5, y)],
                    Stroke::new(
                        2.0,
                        stale_dim(meter_segment_color(held_db).gamma_multiply(1.15), stale),
                    ),
                );
            }
        }
        // Clip lamp: dark when idle, lit red (with a glow) while latched.
        let lamp = Rect::from_min_size(pos2(x, well.min.y), Vec2::new(5.0, CLIP_LAMP_HEIGHT));
        let clipping = hold.is_some_and(ChannelBallistics::clipping);
        if clipping {
            painter.rect_filled(
                lamp.expand(1.0),
                CornerRadius::same(2),
                PROGRAM.gamma_multiply(0.35),
            );
        }
        painter.rect_filled(
            lamp,
            CornerRadius::same(1),
            if clipping {
                PROGRAM
            } else {
                PROGRAM.gamma_multiply(0.15)
            },
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(
            1.0,
            if clipped {
                PROGRAM.gamma_multiply(0.7)
            } else {
                border_color()
            },
        ),
        StrokeKind::Inside,
    );
    let response = if let Some(channels) = channels {
        let text = channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let hold = holds.get(index).copied();
                let mut line = l.format(
                    Msg::MeterChannelHover,
                    &[
                        ("channel", Arg::Plain(index as u64 + 1)),
                        (
                            "peak",
                            Arg::Str(&format_db(l, meter_db(channel.peak_millionths))),
                        ),
                        (
                            "rms",
                            Arg::Str(&format_db(l, meter_db(channel.rms_millionths))),
                        ),
                        (
                            "hold",
                            Arg::Str(&hold.map_or_else(
                                || "--".to_owned(),
                                #[allow(clippy::cast_precision_loss)]
                                |hold| format_db(l, hold.held_mdb as f32 / 1_000.0),
                            )),
                        ),
                    ],
                );
                if hold.is_some_and(ChannelBallistics::clipping) {
                    line.push_str("  ");
                    line.push_str(l.text(Msg::MeterClipMark));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n");
        response.on_hover_text(if clipped {
            format!("{text}\n{}", l.text(Msg::MeterClickToClear))
        } else {
            text
        })
    } else {
        response.on_hover_text(l.text(Msg::NoMeterSample))
    };
    clipped && response.clicked()
}

/// Formats a dBFS level for meter tooltips.
pub(crate) fn format_db(l: Locale, db: f32) -> String {
    l.decibels(f64::from(db), f64::from(METER_FLOOR_DB))
}

/// Least vertical distance between two Master scale labels.
const METER_LABEL_SPACING: f32 = 10.0;

/// The scale ticks that get a label on a meter spanning `top..bottom`, with
/// their y positions: full scale and the floor always, and each other tick
/// only when it keeps `spacing` from the labels around it, so a short
/// compact-window meter drops crowded labels instead of overprinting them.
pub(crate) fn labelled_meter_ticks(top: f32, bottom: f32, spacing: f32) -> Vec<(f32, f32)> {
    let y_of = |db: f32| bottom - meter_fraction(db) * (bottom - top);
    let floor_y = y_of(METER_FLOOR_DB);
    let mut labels: Vec<(f32, f32)> = Vec::new();
    for db in METER_TICKS_DB {
        let y = y_of(db);
        let is_end = db >= 0.0 || db <= METER_FLOOR_DB;
        let clear =
            labels.last().is_none_or(|(_, last)| y - last >= spacing) && floor_y - y >= spacing;
        if is_end || clear {
            labels.push((db, y));
        }
    }
    labels
}

/// The Master meter's dB scale: a label beside each tick in the gutter.
fn draw_meter_scale(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(24.0, height), Sense::hover());
    let painter = ui.painter_at(rect.expand2(Vec2::new(0.0, 6.0)));
    let top = rect.min.y + 2.0 + CLIP_LAMP_HEIGHT + 2.0;
    let bottom = rect.max.y - 2.0;
    for (db, y) in labelled_meter_ticks(top, bottom, METER_LABEL_SPACING) {
        painter.text(
            pos2(rect.min.x + 1.0, y),
            egui::Align2::LEFT_CENTER,
            meter_tick_label(db),
            egui::FontId::new(8.5, egui::FontFamily::Monospace),
            if db >= 0.0 {
                PROGRAM.gamma_multiply(0.9)
            } else {
                muted_color()
            },
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
        assert!((meter_fraction(6.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn meter_scale_expands_toward_full_scale_and_inverts() {
        // Every tick sits on the travel, in order, with -18 dBFS below the
        // middle and more room per dB toward the top.
        let ticks: Vec<f32> = METER_TICKS_DB
            .iter()
            .map(|db| meter_fraction(*db))
            .collect();
        assert!(ticks.windows(2).all(|pair| pair[0] > pair[1]));
        assert!((meter_fraction(-18.0) - 0.44).abs() < 1e-6);
        assert!((meter_fraction(-21.0) - 0.38).abs() < 1e-6);
        let per_db =
            |high: f32, low: f32| (meter_fraction(high) - meter_fraction(low)) / (high - low);
        assert!(per_db(0.0, -6.0) > per_db(-6.0, -12.0));
        assert!(per_db(-24.0, -36.0) > per_db(-48.0, -60.0));
        // The inverse maps every level back.
        let mut db = -60.0_f32;
        while db <= 0.0 {
            assert!((meter_db_at(meter_fraction(db)) - db).abs() < 1e-3, "{db}");
            db += 0.5;
        }
        assert!((meter_db_at(-1.0) + 60.0).abs() < f32::EPSILON);
        assert!(meter_db_at(2.0).abs() < f32::EPSILON);
        assert!((meter_db_at(f32::NAN) + 60.0).abs() < f32::EPSILON);
    }

    #[test]
    fn meter_segments_follow_height_and_light_rms_then_peak() {
        assert_eq!(meter_segments(0.0), METER_MIN_SEGMENTS);
        assert_eq!(meter_segments(f32::NAN), METER_MIN_SEGMENTS);
        assert_eq!(meter_segments(200.0), 50);
        assert_eq!(meter_segments(5_000.0), METER_MAX_SEGMENTS);
        assert_eq!(segment_light(0.1, 0.5, 0.7), SegmentLight::Rms);
        assert_eq!(segment_light(0.5, 0.5, 0.7), SegmentLight::Peak);
        assert_eq!(segment_light(0.69, 0.5, 0.7), SegmentLight::Peak);
        assert_eq!(segment_light(0.7, 0.5, 0.7), SegmentLight::Unlit);
        // A silent bar lights nothing; a peak below RMS adds no peak band.
        assert_eq!(segment_light(0.0, 0.0, 0.0), SegmentLight::Unlit);
        assert_eq!(segment_light(0.3, 0.5, 0.2), SegmentLight::Rms);
        assert_eq!(segment_light(0.6, 0.5, 0.2), SegmentLight::Unlit);
    }

    #[test]
    fn crowded_scale_labels_are_dropped_but_the_ends_stay() {
        let all = |height: f32| -> Vec<f32> {
            labelled_meter_ticks(0.0, height, METER_LABEL_SPACING)
                .into_iter()
                .map(|(db, _)| db)
                .collect()
        };
        assert_eq!(all(600.0), METER_TICKS_DB.to_vec());
        let short = all(110.0);
        assert_eq!(short.first(), Some(&0.0));
        assert_eq!(short.last(), Some(&-60.0));
        assert!(short.len() < METER_TICKS_DB.len());
        let ys: Vec<f32> = labelled_meter_ticks(0.0, 110.0, METER_LABEL_SPACING)
            .into_iter()
            .map(|(_, y)| y)
            .collect();
        assert!(
            ys.windows(2)
                .all(|pair| pair[1] - pair[0] >= METER_LABEL_SPACING)
        );
    }

    #[test]
    fn meter_ticks_are_the_broadcast_set_with_signed_labels() {
        assert_eq!(
            METER_TICKS_DB.map(meter_tick_label),
            ["0", "-6", "-12", "-18", "-24", "-36", "-48", "-60"]
        );
        // The zones change colour exactly on the -18 and -6 ticks.
        assert_eq!(
            meter_segment_color(meter_db_at(meter_fraction(-18.01))),
            PREVIEW
        );
        assert_eq!(
            meter_segment_color(meter_db_at(meter_fraction(-17.99))),
            Color32::from_rgb(232, 200, 40)
        );
        assert_eq!(meter_segment_color(-6.0), PROGRAM);
    }

    #[test]
    fn fader_taper_is_monotonic_and_round_trips_on_the_gain_step() {
        assert!(fader_position(-96_000).abs() < f32::EPSILON);
        assert!((fader_position(24_000) - 1.0).abs() < f32::EPSILON);
        assert!((fader_position(0) - 0.72).abs() < 1e-6);
        assert!((fader_position(-15_000) - 0.495).abs() < 1e-6);
        // Out-of-range gains clamp to the travel ends.
        assert!(fader_position(-200_000).abs() < f32::EPSILON);
        assert!((fader_position(90_000) - 1.0).abs() < f32::EPSILON);
        let mut previous = -1.0;
        for mdb in (-96_000..=24_000).step_by(100) {
            let position = fader_position(mdb);
            assert!(position > previous, "{mdb}");
            previous = position;
            assert_eq!(fader_mdb(position), mdb, "{mdb}");
        }
        assert_eq!(fader_mdb(0.0), -96_000);
        assert_eq!(fader_mdb(1.0), 24_000);
        assert_eq!(fader_mdb(0.72), 0);
        assert_eq!(fader_mdb(-3.0), -96_000);
        assert_eq!(fader_mdb(f32::NAN), -96_000);
        // Unity sits in the upper third, with most travel near it.
        assert!(fader_position(-10_000) > 0.5);
    }

    #[test]
    fn fader_ticks_sit_on_the_travel_in_order() {
        let positions: Vec<f32> = FADER_TICKS
            .iter()
            .map(|(mdb, _)| fader_position(*mdb))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] > pair[1]));
        assert_eq!(FADER_TICKS[1], (0, "0"));
        let meter: Vec<f32> = METER_TICKS_DB
            .iter()
            .map(|db| meter_fraction(*db))
            .collect();
        assert!((meter[0] - 1.0).abs() < f32::EPSILON);
        assert!(meter.last().unwrap().abs() < f32::EPSILON);
    }

    #[test]
    fn gain_and_clip_labels() {
        assert_eq!(format_gain(Locale::ENGLISH, 0), "+0.0 dB");
        assert_eq!(format_gain(Locale::ENGLISH, -6_500), "-6.5 dB");
        assert_eq!(format_gain(Locale::ENGLISH, 12_000), "+12.0 dB");
        assert_eq!(format_gain(Locale::ENGLISH, -96_000), "-inf dB");
        let en = Locale::ENGLISH;
        assert_eq!(clip_reset_label(en, 0), None);
        assert_eq!(clip_reset_label(en, 1).as_deref(), Some("RESET CLIP"));
        assert_eq!(clip_reset_label(en, 3).as_deref(), Some("RESET 3 CLIPS"));
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            clip_reset_label(spanish, 2).as_deref(),
            Some("BORRAR 2 CLIPS")
        );
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
    fn typed_gain_and_balance_refuse_not_a_number() {
        // `NaN` parses as an f64; the entries would store it as 0 (unity
        // gain, centre balance) instead of refusing the text.
        assert_eq!(parse_gain(Locale::ENGLISH, "-6.5 dB"), Some(-6_500.0));
        assert_eq!(parse_gain(Locale::ENGLISH, " +3db "), Some(3_000.0));
        assert_eq!(parse_gain(Locale::ENGLISH, "-inf"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_gain(Locale::ENGLISH, "loud"), None);
        assert_eq!(parse_gain(Locale::ENGLISH, "NaN"), None);
        assert_eq!(parse_gain(Locale::ENGLISH, "nan dB"), None);
        assert_eq!(parse_balance("NaN"), None);
        assert_eq!(parse_balance("Lnan"), None);
        assert_eq!(parse_balance("R inf"), Some(f64::INFINITY));
    }

    #[test]
    fn meter_notice_requires_ready_view_without_sample() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert!(!meters_unavailable(&state));
        let state = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert!(!meters_unavailable(&state));
    }

    #[test]
    fn stale_meters_are_labelled_with_their_age_and_dimmed() {
        assert_eq!(
            stale_meter_label(Locale::ENGLISH, 1_049),
            "METERS STALE · 1.0 s"
        );
        assert_eq!(
            stale_meter_label(Locale::ENGLISH, 12_380),
            "METERS STALE · 12.3 s"
        );
        assert_eq!(
            stale_meter_label(Locale::new(fm_ui_model::i18n::Language::Spanish), 2_400),
            "MEDIDORES OBSOLETOS · 2,4 s"
        );
        assert_eq!(stale_dim(PREVIEW, false), PREVIEW);
        assert_eq!(stale_dim(PREVIEW, true), PREVIEW.gamma_multiply(0.35));
    }

    #[test]
    fn meter_tooltips_format_levels_and_floor() {
        assert_eq!(format_db(Locale::ENGLISH, -6.02), "-6.0 dB");
        assert_eq!(format_db(Locale::ENGLISH, 0.0), "+0.0 dB");
        assert_eq!(format_db(Locale::ENGLISH, f32::NEG_INFINITY), "-inf dB");
        assert_eq!(format_db(Locale::ENGLISH, -60.0), "-inf dB");
    }

    #[test]
    fn levels_use_the_locale_decimal_mark_and_parse_back() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(format_gain(spanish, -6_500), "-6,5 dB");
        assert_eq!(format_gain(spanish, -96_000), "-inf dB");
        assert_eq!(format_db(spanish, -6.02), "-6,0 dB");
        // The formatted text round-trips, and a typed `.` still works.
        assert_eq!(parse_gain(spanish, "-6,5 dB"), Some(-6_500.0));
        assert_eq!(parse_gain(spanish, "+3.5"), Some(3_500.0));
        assert_eq!(parse_gain(spanish, "-inf dB"), Some(f64::NEG_INFINITY));
        // English never reads a comma as a decimal mark.
        assert_eq!(parse_gain(Locale::ENGLISH, "-6,5 dB"), None);
    }
}
