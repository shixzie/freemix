//! Audio mixer: vertical input channel strips plus a Master meter strip.
//!
//! Meters display the latest lossy daemon sample only (per-channel peak and
//! RMS); with no sample the meter wells stay unlit and the header reports
//! meters as unavailable. Peak hold and the clip latch are Studio-local
//! ballistics over those samples; a clip lamp stays lit until clicked. When
//! no new meter message has arrived for over a second the meters keep the
//! last sample dimmed, each readout reads `STALE`, and the header carries a
//! `STALE` badge with the message age, as stale monitor frames do.
//!
//! Each strip carries a pan knob (placing a mono source with the -3 dB
//! constant-power law) separate from the stereo balance trim, an optional
//! operator label shown on its name plate, and an optional VCA group
//! (AU-001). Every group with members gets a group strip between the input
//! strips and Master: its fader adds to each member's gain and its mute
//! silences them, while each member keeps its own fader. The group strip's
//! meter is the loudest of its members' post-fader input meters. A group
//! strip also solos its members and, in the details, nests the group inside
//! a parent group (which then also governs every member); a parent group
//! with only nested groups gets a strip too.
//!
//! The `MON` strip beside Master meters the monitor (headphone) bus and
//! chooses the solo mode: SIP gates Program to the soloed strips, while PFL
//! and AFL leave Program alone and route the soloed strips to the monitor
//! bus before or after their faders. While nothing is soloed in PFL or AFL
//! the monitor bus follows Master.

use egui::{
    Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Key, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{AudioLoudnessMeter, AudioMeterChannel, SourceSyncSample};
use fm_types::strip_dsp::StripDspTarget;
use fm_ui_model::{ClientView, InputAudioStripStatus};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::meter_ballistics::{
    ChannelBallistics, MeterBallistics, MeterKey, held_peak_mdb, peak_readout,
};
use crate::monitors::label_for_input;
use crate::theme::{
    AMBER, MONITOR_BLACK, PREVIEW, PROGRAM, accent_color, border_color, border_strong_color,
    caption, graphite_color, muted_color, raised_color, section_label, small_button, tally_color,
    text_color, text_dim_color, widget_color, widget_hover_color,
};
use crate::{
    InputAudioStripUpdate, StudioConnectionStatus, StudioIntent, StudioUiState, TallyState,
    tally_state,
};

/// Width of the narrowest (compact) channel strip.
pub(crate) const STRIP_WIDTH: f32 = 82.0;
/// Width of the widest channel strip: wider strips would only add empty
/// space between the meter and the fader.
const MAX_STRIP_WIDTH: f32 = 150.0;
/// Horizontal inner margins of a strip frame.
const STRIP_FRAME_WIDTH: f32 = 10.0;
const STRIP_GAP: f32 = 6.0;
const MIN_FADER_HEIGHT: f32 = 120.0;
const MAX_FADER_HEIGHT: f32 = 1_000.0;
/// From this meter and fader height up, the scales carry the fine marks.
const FINE_SCALE_HEIGHT: f32 = 360.0;
/// Room kept free beside the strips for the rule before the pinned
/// Monitor and Master strips and for rounding.
const MIXER_SIDE_RESERVE: f32 = 8.0;
/// Room kept free under the strips, so a panel that fits them never
/// scrolls.
const MIXER_BOTTOM_RESERVE: f32 = 16.0;
/// Vertical space a strip needs besides its fader and meters.
const STRIP_CHROME_HEIGHT: f32 = 322.0;
/// Height of the label, group, gain, balance, and delay entry rows inside
/// the chrome.
const ENTRY_ROWS_HEIGHT: f32 = 124.0;
/// Height of the pan knob row.
const PAN_ROW_HEIGHT: f32 = 24.0;
/// Longest strip label or group name, in UTF-8 bytes.
const MAX_LABEL_BYTES: usize = fm_protocol::MAX_AUDIO_LABEL_BYTES;
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
/// The finer meter scale of a tall meter: every 3 dB near full scale.
pub(crate) const FINE_METER_TICKS_DB: [f32; 13] = [
    0.0, -3.0, -6.0, -9.0, -12.0, -15.0, -18.0, -21.0, -24.0, -30.0, -36.0, -48.0, -60.0,
];
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
/// The finer fader scale of a tall fader.
pub(crate) const FINE_FADER_TICKS: [(i32, &str); 13] = [
    (20_000, "+20"),
    (10_000, "+10"),
    (5_000, "+5"),
    (0, "0"),
    (-5_000, "5"),
    (-10_000, "10"),
    (-15_000, "15"),
    (-20_000, "20"),
    (-30_000, "30"),
    (-40_000, "40"),
    (-50_000, "50"),
    (-60_000, "60"),
    (-96_000, "∞"),
];
/// Gain step of the fader and its keyboard nudges, in milli-dB.
const FADER_STEP_MDB: i32 = 100;
const FADER_KEY_STEP_MDB: i32 = 500;
/// The fader cap of a compact strip; wider strips get a larger cap.
const FADER_CAP: Vec2 = Vec2::new(26.0, 14.0);

/// The groups that have member strips, directly or through a nested
/// group, ascending, with those members.
pub(crate) fn active_groups(view: &ClientView) -> Vec<(u8, Vec<fm_types::InputId>)> {
    group_members(&view.audio_mixer, &view.input_audio_strips)
}

/// [`active_groups`] over a mixer status and its strips.
pub(crate) fn group_members(
    mixer: &fm_protocol::AudioMixerStatus,
    strips: &[InputAudioStripStatus],
) -> Vec<(u8, Vec<fm_types::InputId>)> {
    (1..=4_u8)
        .filter_map(|group| {
            let members = strips
                .iter()
                .filter(|strip| mixer.chain(strip.group).any(|member| member == group))
                .map(|strip| strip.input)
                .collect::<Vec<_>>();
            (!members.is_empty()).then_some((group, members))
        })
        .collect()
}

/// The parents `group` may take without a cycle, ascending.
pub(crate) fn parent_choices(mixer: &fm_protocol::AudioMixerStatus, group: u8) -> Vec<u8> {
    (1..=4_u8)
        .filter(|&parent| parent != group && mixer.parent_is_acyclic(group, Some(parent)))
        .collect()
}

/// The display name of `group` in `view`.
pub(crate) fn group_name(view: &ClientView, group: u8) -> &str {
    view.audio_mixer
        .groups
        .get(usize::from(group).saturating_sub(1))
        .map_or("", |status| status.name.as_str())
}

/// The longest prefix of `text` within [`MAX_LABEL_BYTES`] bytes, cut at a
/// character boundary.
pub(crate) fn bounded_label(text: &str) -> &str {
    let mut end = text.len().min(MAX_LABEL_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Total width the mixer wants for `strips` input strips plus Master.
#[allow(clippy::cast_precision_loss)] // strip counts are tiny
pub(crate) fn mixer_width(strips: usize) -> f32 {
    (strips as f32 + 1.0) * strip_pitch(STRIP_WIDTH) + 34.0
}

/// Distance from one strip's left edge to the next one's, for strips
/// `width` wide (the frame's stroke adds 2 points).
const fn strip_pitch(width: f32) -> f32 {
    width + 2.0 + STRIP_GAP
}
/// Width of the fade where the strip area's edge cuts through a strip.
const SCROLL_FADE_WIDTH: f32 = 28.0;

/// The size of every strip in a mixer panel: the strips share the panel's
/// width (wider up to [`MAX_STRIP_WIDTH`], never narrower than the compact
/// [`STRIP_WIDTH`]) and their meters and faders take its height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StripLayout {
    /// Outer width of each input, group, Monitor, and Master strip.
    pub(crate) width: f32,
    /// Height of each strip's meter and fader.
    pub(crate) fader_height: f32,
}

impl StripLayout {
    /// Content width inside a strip frame.
    pub(crate) const fn inner_width(self) -> f32 {
        self.width - STRIP_FRAME_WIDTH
    }

    /// Distance from one strip's left edge to the next one's.
    pub(crate) const fn pitch(self) -> f32 {
        strip_pitch(self.width)
    }

    /// How far the strip has grown from compact (0) to the widest (1).
    fn growth(self) -> f32 {
        ((self.width - STRIP_WIDTH) / (MAX_STRIP_WIDTH - STRIP_WIDTH)).clamp(0.0, 1.0)
    }

    /// Width of one meter bar: 5 points on a compact strip, up to 10 on
    /// the widest.
    pub(crate) fn bar_width(self) -> f32 {
        5.0f32.mul_add(self.growth(), 5.0).round()
    }

    /// The fader cap: 26x14 on a compact strip, up to 40x18 on the widest.
    pub(crate) fn fader_cap(self) -> Vec2 {
        let growth = self.growth();
        Vec2::new(
            14.0f32.mul_add(growth, FADER_CAP.x).round(),
            4.0f32.mul_add(growth, FADER_CAP.y).round(),
        )
    }

    /// Whether the meter and fader are tall enough for the fine scales.
    pub(crate) fn fine_scale(self) -> bool {
        self.fader_height >= FINE_SCALE_HEIGHT
    }

    /// The meter's scale marks.
    pub(crate) fn meter_ticks(self) -> &'static [f32] {
        if self.fine_scale() {
            &FINE_METER_TICKS_DB
        } else {
            &METER_TICKS_DB
        }
    }

    /// The fader's scale marks.
    pub(crate) fn fader_ticks(self) -> &'static [(i32, &'static str)] {
        if self.fine_scale() {
            &FINE_FADER_TICKS
        } else {
            &FADER_TICKS
        }
    }
}

/// Lays out `strips` input and group strips plus the pinned Monitor and
/// Master strips in a `panel` (the mixer's whole area, header included).
/// `details`: the typed entries show and take height from the faders.
#[allow(clippy::cast_precision_loss)] // strip counts are tiny
pub(crate) fn strip_layout(panel: Vec2, strips: usize, details: bool) -> StripLayout {
    let chrome = if details {
        STRIP_CHROME_HEIGHT
    } else {
        STRIP_CHROME_HEIGHT - ENTRY_ROWS_HEIGHT
    };
    let fader_height = (panel.y - chrome - MIXER_BOTTOM_RESERVE)
        .clamp(MIN_FADER_HEIGHT, MAX_FADER_HEIGHT)
        .floor();
    // `n` strips at pitch `p` take `n * p - gap`; with the two pinned
    // strips beside the rule that is `(strips + 2) * p`.
    let count = strips as f32 + 2.0;
    let pitch = (panel.x - MIXER_SIDE_RESERVE) / count;
    let width = (pitch - 2.0 - STRIP_GAP)
        .floor()
        .clamp(STRIP_WIDTH, MAX_STRIP_WIDTH);
    StripLayout {
        width,
        fader_height,
    }
}

/// Width of the scrolling strip area inside `room` for `strips` strips at
/// `pitch`: all the room when every strip fits, else only as many whole
/// strips as fit, so the pinned Monitor and Master strips never sit beside
/// a sliver.
#[allow(clippy::cast_precision_loss)] // strip counts are tiny
pub(crate) fn whole_strips_width(room: f32, strips: usize, pitch: f32) -> f32 {
    let content = strips as f32 * pitch - STRIP_GAP;
    if content <= room {
        return room;
    }
    let whole = ((room + STRIP_GAP) / pitch).floor().max(1.0);
    (whole * pitch - STRIP_GAP).min(room)
}

/// Fades the strip area's left or right edge into the panel where it cuts
/// through a strip, so a partly scrolled strip reads as "more this way"
/// instead of a clipped sliver.
fn paint_scroll_edges(
    painter: &egui::Painter,
    viewport: Rect,
    (offset, content): (f32, f32),
    pitch: f32,
) {
    // A strip spans the first `pitch - STRIP_GAP` points of each pitch; an
    // edge at content position `x` cuts one when it falls inside.
    let cuts = |x: f32| {
        let into = x.rem_euclid(pitch);
        into > 1.0 && into < pitch - STRIP_GAP - 1.0
    };
    let fill = crate::theme::panel_color();
    let width = SCROLL_FADE_WIDTH.min(viewport.width() / 4.0);
    if offset > 0.5 && cuts(offset) {
        let band = Rect::from_min_size(viewport.min, Vec2::new(width, viewport.height()));
        paint_fade(painter, band, fill, true);
    }
    if offset + viewport.width() < content - 0.5 && cuts(offset + viewport.width()) {
        let band = Rect::from_min_max(
            egui::pos2(viewport.max.x - width, viewport.min.y),
            viewport.max,
        );
        paint_fade(painter, band, fill, false);
    }
}

/// A horizontal gradient over `band`, opaque `fill` at its left edge when
/// `opaque_left`, else at its right edge, transparent at the other.
fn paint_fade(painter: &egui::Painter, band: Rect, fill: Color32, opaque_left: bool) {
    let (left, right) = if opaque_left {
        (fill, Color32::TRANSPARENT)
    } else {
        (Color32::TRANSPARENT, fill)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(band.left_top(), left);
    mesh.colored_vertex(band.right_top(), right);
    mesh.colored_vertex(band.right_bottom(), right);
    mesh.colored_vertex(band.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
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
/// Draws the mixer. `details` is the progressive-disclosure flag for the
/// strips' typed gain, balance, and delay entries; the header key flips it.
pub(crate) fn draw_mixer(
    ui: &mut Ui,
    state: &StudioUiState,
    ballistics: &mut MeterBallistics,
    (dt_ms, details, fx): (u32, &mut bool, &mut Option<StripDspTarget>),
    bus_mixer: &mut bool,
    intents: &mut Vec<StudioIntent>,
) {
    // The strips share the whole panel; hidden entries give their height
    // to the faders.
    let panel = ui.available_size();
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
    crate::theme::panel_header(ui, |ui| {
        if state.view.is_some() {
            crate::theme::disclosure(
                ui,
                details,
                l.text(Msg::MixerDetailsKey),
                (
                    l.text(Msg::DisclosureShowMixer),
                    l.text(Msg::DisclosureHideMixer),
                ),
            );
        }
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
            // The key gives way first in a narrow mixer: it shows whole or
            // not at all, rather than run under the title or end in a cut
            // word.
            let legend = RichText::new(l.text(Msg::MeterLegend))
                .size(crate::theme::legible(10.0))
                .color(muted_color());
            let legend_width = egui::WidgetText::from(legend.clone())
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::FontSelection::Default,
                )
                .size()
                .x;
            if ui.available_width() >= legend_width + 4.0 {
                ui.add(
                    egui::Label::new(legend)
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .sense(Sense::hover()),
                )
                .on_hover_text(format!(
                    "{}\n{}",
                    l.text(Msg::MeterLegend),
                    l.text(Msg::MeterLegendHover)
                ));
            }
        }
        // Added last: the header lays out from the right, so the BUSES key
        // sits at the left edge and never takes the meter legend's room.
        if state.view.is_some()
            && ui
                .add(small_button(l.text(Msg::BusMixerKey)))
                .hint(
                    l.text(Msg::BusMixerKeyHover),
                    Msg::ReasonNotSynchronized.en(),
                )
                .clicked()
        {
            *bus_mixer = true;
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
        MeterKey::Master | MeterKey::Monitor => true,
        MeterKey::Group(group) => active_groups(view)
            .iter()
            .any(|(active, _)| *active == group),
        MeterKey::Input(input) => view
            .input_audio_strips
            .iter()
            .any(|strip| strip.input == input),
    });
    let audio_reason = unavailable_reason(state, Capability::Audio);
    // Monitor and Master stay pinned at the right edge; input strips scroll
    // when the mixer is narrower than all strips.
    let strip_count = view.input_audio_strips.len() + active_groups(view).len();
    let mut layout = strip_layout(panel, strip_count, *details);
    // Live source clock keys (AU-004) take a row from every fader, so the
    // strips stay level.
    // A screen capture input's audio state badge (IN-011) shares the row.
    let sync_row = crate::source_sync_ui::any_strip(state)
        || view.input_audio_strips.iter().any(|strip| {
            view.screen_capture_input(strip.input)
                .is_some_and(crate::screen_capture_ui::shows_audio_badge)
        });
    if sync_row {
        layout.fader_height =
            (layout.fader_height - crate::source_sync_ui::SYNC_ROW_HEIGHT).max(MIN_FADER_HEIGHT);
    }
    let master_width = 2.0 * layout.pitch();
    let inputs_width = (ui.available_width() - master_width).max(layout.width);
    let viewport_width = whole_strips_width(inputs_width, strip_count, layout.pitch());
    let mut clear = Vec::new();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = STRIP_GAP;
        let strips = ui.allocate_ui_with_layout(
            Vec2::new(inputs_width, ui.available_height()),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                let scrolled = ScrollArea::horizontal()
                    .id_salt("studio-mixer")
                    .auto_shrink([false, true])
                    .max_width(viewport_width)
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
                                let target = StripDspTarget::Input(status.input);
                                let sync = if sync_row {
                                    SyncRow::Row {
                                        sync: crate::source_sync_ui::sample_for(
                                            state,
                                            status.input,
                                        ),
                                        audio: view.screen_capture_input(status.input).filter(
                                            |entry| {
                                                crate::screen_capture_ui::shows_audio_badge(entry)
                                            },
                                        ),
                                    }
                                } else {
                                    SyncRow::Absent
                                };
                                if draw_input_strip(
                                    ui,
                                    (view, fx_active(state, target), &mut *fx),
                                    (status, sync, loudness_for(state, target)),
                                    MeterFeed {
                                        channels,
                                        holds: &holds,
                                        stale: meters_stale,
                                    },
                                    StripGate {
                                        enabled,
                                        reason: audio_reason,
                                        details: *details,
                                    },
                                    layout,
                                    intents,
                                ) {
                                    clear.push(key);
                                }
                            }
                            for (group, members) in active_groups(view) {
                                let channels = state
                                    .audio_meters
                                    .as_ref()
                                    .and_then(|meters| group_meter(&meters.inputs, &members));
                                let key = MeterKey::Group(group);
                                let holds =
                                    ballistics.update(key, channels.as_deref(), dt_ms).to_vec();
                                if draw_group_strip(
                                    ui,
                                    view,
                                    (group, members.len()),
                                    MeterFeed {
                                        channels: channels.as_deref(),
                                        holds: &holds,
                                        stale: meters_stale,
                                    },
                                    StripGate {
                                        enabled,
                                        reason: audio_reason,
                                        details: *details,
                                    },
                                    layout,
                                    intents,
                                ) {
                                    clear.push(key);
                                }
                            }
                        });
                    });
                paint_scroll_edges(
                    ui.painter(),
                    scrolled.inner_rect,
                    (scrolled.state.offset.x, scrolled.content_size.x),
                    layout.pitch(),
                );
            },
        );
        // The input strips start at the left; a rule sets the pinned
        // Monitor and Master strips apart however much room is spare.
        let rect = strips.response.rect;
        ui.painter().vline(
            rect.right() + STRIP_GAP / 2.0,
            rect.top()..=rect.bottom(),
            Stroke::new(1.0, border_color()),
        );
        let monitor = state
            .audio_meters
            .as_ref()
            .and_then(|meters| meters.monitor.as_ref());
        let monitor_channels = monitor.map(|monitor| monitor.channels.as_slice());
        let holds = ballistics
            .update(MeterKey::Monitor, monitor_channels, dt_ms)
            .to_vec();
        // The headphone device list is answered only to a session that may
        // edit the project (Protocol 2.102), so only such a session asks.
        let list_outputs = enabled && state.can_edit_project;
        if draw_monitor_strip(
            ui,
            view,
            (
                MeterFeed {
                    channels: monitor_channels,
                    holds: &holds,
                    stale: meters_stale,
                },
                monitor.is_some_and(|monitor| monitor.solo),
                monitor.map(|monitor| monitor.output),
                list_outputs,
            ),
            StripGate {
                enabled,
                reason: audio_reason,
                details: *details,
            },
            layout,
            intents,
        ) {
            clear.push(MeterKey::Monitor);
        }
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
            layout,
            (fx_active(state, StripDspTarget::Master), &mut *fx),
            loudness_for(state, StripDspTarget::Master),
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

/// The latest BS.1770 loudness `audio_meters` reports for `target`
/// (AU-005).
pub(crate) fn loudness_for(
    state: &StudioUiState,
    target: StripDspTarget,
) -> Option<AudioLoudnessMeter> {
    let wire = fm_ui_model::strip_dsp::wire_target(target);
    state
        .audio_meters
        .as_ref()?
        .loudness
        .iter()
        .find(|meter| meter.target == wire)
        .copied()
}

/// A milli-LUFS or milli-dBTP value with one decimal, or `--` for none.
pub(crate) fn format_loudness(l: Locale, value: Option<i32>) -> String {
    value.map_or_else(
        || "--".to_owned(),
        |value| l.decimal(f64::from(value) / 1_000.0, 1),
    )
}

/// The loudness hover text: every value of `meter`, or why there is none.
pub(crate) fn loudness_hover(
    l: Locale,
    meter: Option<&AudioLoudnessMeter>,
    master: bool,
) -> String {
    let Some(meter) = meter else {
        return l.text(Msg::LoudnessIdleHover).to_owned();
    };
    l.format(
        if master {
            Msg::LoudnessMasterHover
        } else {
            Msg::LoudnessStripHover
        },
        &[
            (
                "momentary",
                Arg::Str(&format_loudness(l, meter.momentary_millilufs)),
            ),
            (
                "short",
                Arg::Str(&format_loudness(l, meter.short_term_millilufs)),
            ),
            (
                "integrated",
                Arg::Str(&format_loudness(l, meter.integrated_millilufs)),
            ),
            (
                "peak",
                Arg::Str(&format_loudness(l, meter.true_peak_millidbtp)),
            ),
            (
                "max",
                Arg::Str(&format_loudness(l, meter.max_true_peak_millidbtp)),
            ),
            (
                "range",
                Arg::Str(&format_loudness(l, meter.loudness_range_millilu)),
            ),
        ],
    )
}

/// A strip's loudness readout (AU-005): Master shows its integrated
/// loudness and a strip its short-term loudness (momentary until 3 s have
/// been heard), in LUFS; amber past a true peak of -1 dBTP. The hover
/// lists every value.
fn loudness_box(ui: &mut Ui, meter: Option<&AudioLoudnessMeter>, master: bool, stale: bool) {
    let l = l10n::locale(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), READOUT_HEIGHT),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(2), MONITOR_BLACK);
        painter.rect_stroke(
            rect,
            CornerRadius::same(2),
            Stroke::new(1.0, border_color()),
            StrokeKind::Inside,
        );
        let shown = meter.and_then(|meter| {
            if master {
                meter
                    .integrated_millilufs
                    .map(|value| ("I", value))
                    .or(meter.short_term_millilufs.map(|value| ("S", value)))
            } else {
                meter.short_term_millilufs.map(|value| ("S", value))
            }
            .or(meter.momentary_millilufs.map(|value| ("M", value)))
        });
        let hot = meter
            .and_then(|meter| meter.true_peak_millidbtp)
            .is_some_and(|peak| peak > -1_000);
        let color = if shown.is_none() || stale {
            muted_color()
        } else if hot {
            AMBER
        } else {
            text_color()
        };
        if let Some((scale, value)) = shown {
            // The scale and unit dim at the left, the figure at the right;
            // the unit is dropped when the strip is too narrow for both.
            let figure = painter.layout_no_wrap(
                format_loudness(l, Some(value)),
                FontId::new(10.0, FontFamily::Monospace),
                color,
            );
            let legend_font = FontId::new(crate::theme::legible(8.5), FontFamily::Proportional);
            let mut legend =
                painter.layout_no_wrap(format!("{scale} LUFS"), legend_font.clone(), muted_color());
            if legend.size().x + figure.size().x + 12.0 > rect.width() {
                legend = painter.layout_no_wrap(scale.to_owned(), legend_font, muted_color());
            }
            painter.galley(
                pos2(rect.left() + 4.0, rect.center().y - legend.size().y / 2.0),
                legend,
                muted_color(),
            );
            painter.galley(
                pos2(
                    rect.right() - 4.0 - figure.size().x,
                    rect.center().y - figure.size().y / 2.0,
                ),
                figure,
                color,
            );
        } else {
            let galley = crate::theme::elided(
                painter,
                "LUFS --",
                FontId::new(10.0, FontFamily::Monospace),
                color,
                rect.width() - 4.0,
            );
            painter.galley(rect.center() - galley.size() / 2.0, galley, color);
        }
    }
    response.on_hover_text(loudness_hover(l, meter, master));
}

/// Whether `target`'s DSP chain processes audio now (AU-005): its `FX` key
/// is lit.
fn fx_active(state: &StudioUiState, target: StripDspTarget) -> bool {
    state
        .project_state
        .as_ref()
        .is_some_and(|project| project.audio_dsp(target).is_active())
}

/// The `FX` key that opens a strip's or Master's FX window (AU-005): lit
/// while the chain processes audio. Opening the window is local, so the key
/// is always enabled.
fn fx_key(ui: &mut Ui, active: bool, hover: &str, width: f32) -> bool {
    let l = l10n::locale(ui.ctx());
    let mut lit = active;
    led_key(
        ui,
        true,
        &mut lit,
        l.text(Msg::FxKey),
        accent_color(),
        width,
    )
    .on_hover_text(hover)
    .clicked()
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

/// An input strip's live source clock row (AU-004).
#[derive(Clone, Copy)]
enum SyncRow<'a> {
    /// No strip shows a clock or capture audio row.
    Absent,
    /// The row: this input's loop and its screen capture audio state
    /// (IN-011), either or both absent (a blank row keeps strips level).
    Row {
        sync: Option<&'a SourceSyncSample>,
        audio: Option<&'a fm_protocol::ScreenCaptureEntry>,
    },
}

/// Whether strip controls are enabled, and why not.
#[derive(Clone, Copy)]
struct StripGate {
    enabled: bool,
    reason: &'static str,
    /// Whether the typed gain, balance, and delay entries show.
    details: bool,
}

fn strip_frame(ui: &mut Ui, accent: Color32, width: f32, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(raised_color())
        .stroke(Stroke::new(crate::theme::line_width(), border_color()))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 6))
        .show(ui, |ui| {
            ui.set_width(width - STRIP_FRAME_WIDTH);
            // Compact numerics so every entry fits the fixed strip width.
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::new(10.5, egui::FontFamily::Monospace),
            );
            ui.spacing_mut().button_padding.x = 4.0;
            let top = ui.max_rect().min;
            ui.painter().rect_filled(
                Rect::from_min_size(top - Vec2::new(5.0, 6.0), Vec2::new(width, 2.0)),
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
/// and up to two lines of name, ending in an ellipsis; `hover` names the
/// strip in full.
fn name_plate(ui: &mut Ui, ordinal: Option<usize>, name: &str, hover: &str, tally: TallyState) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), NAME_PLATE_HEIGHT),
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
    response.on_hover_text(hover);
}

/// A compact pan knob with its readout: drag sideways or up and down to
/// move it, double-click to center, Left/Right nudge 5 % while focused.
/// `changed()` reports a new position.
fn pan_knob(ui: &mut Ui, pan: &mut i32, enabled: bool, width: f32) -> egui::Response {
    let l = l10n::locale(ui.ctx());
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(width, PAN_ROW_HEIGHT - 4.0), sense);
    let original = *pan;
    if enabled {
        if response.dragged() {
            let delta = response.drag_delta();
            #[allow(clippy::cast_possible_truncation)] // bounded pointer deltas
            let step = ((delta.x - delta.y) * 100.0).round() as i32;
            *pan = (*pan + step).clamp(-10_000, 10_000);
        }
        if response.double_clicked() {
            *pan = 0;
        }
        if response.clicked() {
            response.request_focus();
        }
        if response.has_focus() {
            let (left, right) = ui.input(|input| {
                (
                    input.key_pressed(Key::ArrowLeft),
                    input.key_pressed(Key::ArrowRight),
                )
            });
            if left {
                *pan = (*pan - 500).max(-10_000);
            }
            if right {
                *pan = (*pan + 500).min(10_000);
            }
        }
    }
    if *pan != original {
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let dim = if enabled { 1.0 } else { 0.45 };
        let painter = ui.painter();
        let radius = rect.height() / 2.0 - 1.0;
        let center = pos2(rect.min.x + radius + 1.0, rect.center().y);
        painter.circle_filled(
            center,
            radius,
            if response.hovered() && enabled {
                widget_hover_color()
            } else {
                widget_color()
            },
        );
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(crate::theme::line_width(), border_strong_color()),
        );
        // -135° (full left) through 0° (up, center) to +135° (full right).
        #[allow(clippy::cast_precision_loss)] // bounded basis points
        let angle = (*pan as f32 / 10_000.0) * 135.0_f32.to_radians();
        let tip = center + Vec2::new(angle.sin(), -angle.cos()) * (radius - 2.0);
        painter.line_segment(
            [center, tip],
            Stroke::new(2.0, accent_color().gamma_multiply(dim)),
        );
        painter.text(
            pos2(rect.max.x, rect.center().y),
            Align2::RIGHT_CENTER,
            format!(
                "{} {}",
                l.text(Msg::PanPrefix),
                format_balance(f64::from(*pan))
            ),
            FontId::new(10.0, FontFamily::Monospace),
            text_dim_color().gamma_multiply(dim),
        );
    }
    response
}

/// A one-line label editor that commits on Enter or focus loss. Returns the
/// edited text when it differs from `current`. `id` keys the in-progress
/// text; `placeholder` shows while the label is empty.
fn label_entry(
    ui: &mut Ui,
    id: impl core::hash::Hash + core::fmt::Debug,
    current: &str,
    enabled: bool,
    (placeholder, hover, reason): (&str, &str, &str),
) -> Option<String> {
    let id = ui.make_persistent_id(id);
    let mut text = ui
        .data_mut(|data| data.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_owned());
    let response = ui
        .add_enabled(
            enabled,
            egui::TextEdit::singleline(&mut text)
                .char_limit(MAX_LABEL_BYTES)
                // The placeholder uses the label's monospace font so a
                // labelled and an unlabelled strip's rows stay level.
                .hint_text(RichText::new(placeholder).monospace())
                .font(egui::TextStyle::Monospace),
        )
        .hint(hover, reason);
    let text = bounded_label(&text).to_owned();
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, text));
        None
    } else {
        ui.data_mut(|data| data.remove::<String>(id));
        (response.lost_focus() && text != current).then_some(text)
    }
}

/// The strip's group picker. Returns the chosen group (`Some(None)`: no
/// group) when it changed.
#[allow(clippy::option_option)]
fn group_choice(
    ui: &mut Ui,
    view: &ClientView,
    status: &InputAudioStripStatus,
    enabled: bool,
    reason: &str,
) -> Option<Option<u8>> {
    let l = l10n::locale(ui.ctx());
    let mut chosen = status.group;
    let selected = chosen.map_or_else(
        || l.text(Msg::StripGroupNone).to_owned(),
        |group| group_name(view, group).to_owned(),
    );
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(("strip-group", status.input.get()))
            .selected_text(selected)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, l.text(Msg::StripGroupNone))
                    .on_hover_text(l.text(Msg::StripGroupNoneHover));
                for group in 1..=4_u8 {
                    let name = group_name(view, group);
                    ui.selectable_value(&mut chosen, Some(group), name)
                        .on_hover_text(
                            l.format(Msg::StripGroupItemHover, &[("group", Arg::Str(name))]),
                        );
                }
            })
            .response
            .hint(l.text(Msg::StripGroupHover), reason);
    });
    (chosen != status.group).then_some(chosen)
}

/// The meter of a VCA group: per channel, the loudest peak and RMS of its
/// member strips. The daemon meters each input strip after its fader, pan,
/// mute, and group gain, so this is the group's post-fader level without a
/// meter of its own on the wire. `None` while no member has a sample.
pub(crate) fn group_meter(
    inputs: &[fm_protocol::InputAudioMeters],
    members: &[fm_types::InputId],
) -> Option<Vec<AudioMeterChannel>> {
    let mut combined: Option<Vec<AudioMeterChannel>> = None;
    for meter in inputs
        .iter()
        .filter(|meter| members.contains(&meter.input.to_domain()))
    {
        let combined = combined.get_or_insert_with(Vec::new);
        if combined.len() < meter.channels.len() {
            combined.resize(meter.channels.len(), AudioMeterChannel::default());
        }
        for (into, channel) in combined.iter_mut().zip(&meter.channels) {
            into.peak_millionths = into.peak_millionths.max(channel.peak_millionths);
            into.rms_millionths = into.rms_millionths.max(channel.rms_millionths);
        }
    }
    combined
}

/// Draws one VCA group strip: name plate, peak readout, the group meter
/// beside its fader, mute and solo, and (with details) the group name and
/// parent. Returns `true` when the operator cleared its clip.
#[allow(clippy::too_many_lines)]
fn draw_group_strip(
    ui: &mut Ui,
    view: &ClientView,
    (group, members): (u8, usize),
    feed: MeterFeed<'_>,
    gate: StripGate,
    layout: StripLayout,
    intents: &mut Vec<StudioIntent>,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let StripGate {
        enabled,
        reason,
        details,
    } = gate;
    let Some(status) = view.audio_mixer.groups.get(usize::from(group) - 1) else {
        return false;
    };
    let mut gain_millidb = status.gain_millidb;
    let mut muted = status.muted;
    let mut soloed = status.soloed;
    let mut level_changed = false;
    let mut solo_changed = false;
    let mut clear_clip = false;
    strip_frame(ui, accent_color(), layout.width, |ui| {
        #[allow(clippy::cast_possible_truncation)] // at most 256 strips
        let plate_hover = l.format(
            Msg::GroupStripPlateHover,
            &[
                ("group", Arg::Str(&status.name)),
                ("count", Arg::from(members as u32)),
            ],
        );
        name_plate(ui, None, &status.name, &plate_hover, TallyState::default());
        clear_clip |= peak_box(ui, feed.holds, feed.channels.is_some(), feed.stale);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            clear_clip |= draw_meter(ui, feed, layout);
            let fader_width = ui.available_width();
            level_changed |= fader(ui, &mut gain_millidb, (enabled, muted), fader_width, layout)
                .hint(l.text(Msg::GroupFaderHover), reason)
                .changed();
        });
        let (_, _) = ui.allocate_exact_size(
            Vec2::new(layout.inner_width(), PAN_ROW_HEIGHT - 4.0),
            Sense::hover(),
        );
        if details {
            ui.vertical_centered_justified(|ui| {
                if let Some(name) = label_entry(
                    ui,
                    ("group-name", group),
                    &status.name,
                    enabled,
                    (&status.name, l.text(Msg::GroupNameHover), reason),
                )
                .filter(|name| !name.trim().is_empty())
                {
                    intents.push(StudioIntent::RenameAudioGroup { group, name });
                }
                if let Some(parent) = group_parent_choice(ui, view, group, enabled, reason) {
                    intents.push(StudioIntent::AudioMixer(
                        fm_protocol::AudioMixerOperation::SetGroupParent { group, parent },
                    ));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let half = (layout.inner_width() - 3.0) / 2.0;
            level_changed |= led_key(ui, enabled, &mut muted, "M", PROGRAM, half)
                .hint(
                    l.text(if muted {
                        Msg::GroupMuteOnHover
                    } else {
                        Msg::GroupMuteOffHover
                    }),
                    reason,
                )
                .changed();
            solo_changed = led_key(ui, enabled, &mut soloed, "S", AMBER, half)
                .hint(
                    l.text(if soloed {
                        Msg::GroupSoloOnHover
                    } else {
                        Msg::GroupSoloOffHover
                    }),
                    reason,
                )
                .changed();
        });
    });
    if level_changed {
        intents.push(StudioIntent::SetAudioGroupLevel {
            group,
            gain_millidb,
            muted,
        });
    }
    if solo_changed {
        intents.push(StudioIntent::AudioMixer(
            fm_protocol::AudioMixerOperation::SetGroupSolo { group, soloed },
        ));
    }
    clear_clip
}

/// The group's parent picker: top level or any group that keeps the groups
/// cycle-free. Returns the chosen parent (`Some(None)`: top level) when it
/// changed.
#[allow(clippy::option_option)]
fn group_parent_choice(
    ui: &mut Ui,
    view: &ClientView,
    group: u8,
    enabled: bool,
    reason: &str,
) -> Option<Option<u8>> {
    let l = l10n::locale(ui.ctx());
    let current = view
        .audio_mixer
        .groups
        .get(usize::from(group) - 1)
        .and_then(|status| status.parent);
    let mut chosen = current;
    let selected = chosen.map_or_else(
        || l.text(Msg::GroupParentNone).to_owned(),
        |parent| {
            l.format(
                Msg::GroupParentIn,
                &[("group", Arg::Str(group_name(view, parent)))],
            )
        },
    );
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(("group-parent", group))
            .selected_text(selected)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, l.text(Msg::GroupParentNone))
                    .on_hover_text(l.text(Msg::GroupParentNoneHover));
                for parent in parent_choices(&view.audio_mixer, group) {
                    let name = group_name(view, parent);
                    ui.selectable_value(&mut chosen, Some(parent), name)
                        .on_hover_text(
                            l.format(Msg::GroupParentItemHover, &[("group", Arg::Str(name))]),
                        );
                }
            })
            .response
            .hint(l.text(Msg::GroupParentHover), reason);
    });
    (chosen != current).then_some(chosen)
}

/// Draws the monitor-bus strip: what the bus carries, its meter, the
/// solo-mode keys, and the headphone output (device, state, level, and
/// mute). `output` is the device state the server last metered, if any;
/// `list_outputs` whether this session may list the server's output
/// devices. Returns `true` when the operator cleared its clip.
#[allow(clippy::too_many_lines)]
fn draw_monitor_strip(
    ui: &mut Ui,
    view: &ClientView,
    (feed, solo, output, list_outputs): (
        MeterFeed<'_>,
        bool,
        Option<fm_protocol::AudioMonitorOutputState>,
        bool,
    ),
    gate: StripGate,
    layout: StripLayout,
    intents: &mut Vec<StudioIntent>,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let StripGate {
        enabled, reason, ..
    } = gate;
    let mut clear_clip = false;
    let mode = view.audio_mixer.solo_mode;
    let accent = if solo { AMBER } else { accent_color() };
    strip_frame(ui, accent, layout.width, |ui| {
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(layout.inner_width(), NAME_PLATE_HEIGHT),
            Sense::hover(),
        );
        ui.painter().text(
            rect.left_top(),
            Align2::LEFT_TOP,
            l.text(Msg::MonitorStrip),
            FontId::new(11.0, FontFamily::Proportional),
            text_color(),
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(0.0, 15.0),
            Align2::LEFT_TOP,
            l.text(if solo {
                Msg::MonitorSoloListen
            } else {
                Msg::MonitorFollowsMaster
            }),
            FontId::new(8.5, FontFamily::Proportional),
            if solo { AMBER } else { muted_color() },
        );
        response.on_hover_text(l.text(Msg::MonitorStripHover));
        clear_clip |= peak_box(ui, feed.holds, feed.channels.is_some(), feed.stale);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // The mode keys take the rest of the meter's height.
            clear_clip |= draw_meter(ui, feed, layout);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let width = ui.available_width();
                for (choice, label, hover) in [
                    (
                        fm_protocol::AudioSoloMode::InPlace,
                        Msg::SoloModeSip,
                        Msg::SoloModeSipHover,
                    ),
                    (
                        fm_protocol::AudioSoloMode::Pfl,
                        Msg::SoloModePfl,
                        Msg::SoloModePflHover,
                    ),
                    (
                        fm_protocol::AudioSoloMode::Afl,
                        Msg::SoloModeAfl,
                        Msg::SoloModeAflHover,
                    ),
                ] {
                    let mut on = mode == choice;
                    if led_key(ui, enabled, &mut on, l.text(label), AMBER, width)
                        .hint(
                            &format!("{}\n{}", l.text(Msg::SoloModeHover), l.text(hover)),
                            reason,
                        )
                        .changed()
                        && mode != choice
                    {
                        intents.push(StudioIntent::AudioMixer(
                            fm_protocol::AudioMixerOperation::SetSoloMode { mode: choice },
                        ));
                    }
                }
            });
        });
        draw_monitor_output(ui, view, output, (enabled, reason, list_outputs), intents);
    });
    clear_clip
}

/// The monitor strip's headphone output: the device picker (or field), the
/// device state, and the feed's own level and mute (AU-001, Protocols 2.53
/// and 2.102).
fn draw_monitor_output(
    ui: &mut Ui,
    view: &ClientView,
    output: Option<fm_protocol::AudioMonitorOutputState>,
    (enabled, reason, list_outputs): (bool, &str, bool),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let monitor = &view.audio_mixer.monitor;
    ui.vertical_centered_justified(|ui| {
        monitor_device_control(ui, view, (enabled, reason, list_outputs), intents);
    });
    let (text, hover, color) = match (monitor.device.as_deref(), output) {
        (None, _) => (
            l.text(Msg::MonitorNoDevice),
            l.text(Msg::MonitorNoDeviceHover).to_owned(),
            muted_color(),
        ),
        (Some(device), Some(fm_protocol::AudioMonitorOutputState::Playing)) => (
            l.text(Msg::MonitorOutputPlaying),
            l.format(
                Msg::MonitorOutputPlayingHover,
                &[("device", Arg::Str(device))],
            ),
            accent_color(),
        ),
        (Some(device), Some(fm_protocol::AudioMonitorOutputState::Failed)) => (
            l.text(Msg::MonitorOutputFailed),
            l.format(
                Msg::MonitorOutputFailedHover,
                &[("device", Arg::Str(device))],
            ),
            PROGRAM,
        ),
        (Some(device), Some(fm_protocol::AudioMonitorOutputState::Off) | None) => (
            l.text(Msg::MonitorOutputIdle),
            l.format(Msg::MonitorOutputIdleHover, &[("device", Arg::Str(device))]),
            muted_color(),
        ),
    };
    ui.vertical_centered(|ui| {
        let size = fitted_font_size(ui, text, crate::theme::legible(8.5), ui.available_width());
        let state =
            ui.add(egui::Label::new(RichText::new(text).size(size).color(color)).truncate());
        // Beside the state, growing upwards from its bottom edge (else
        // above it), so the help never covers the level and mute keys
        // under it.
        let mut tooltip = egui::Tooltip::for_enabled(&state);
        tooltip.popup = tooltip
            .popup
            .align(egui::RectAlign::LEFT_END)
            .align_alternatives(&[egui::RectAlign::RIGHT_END, egui::RectAlign::TOP_START]);
        tooltip.show(|ui| {
            ui.set_max_width(ui.spacing().tooltip_width);
            ui.label(hover);
        });
    });
    let mut gain_millidb = monitor.gain_millidb;
    let mut muted = monitor.muted;
    let mut changed = false;
    ui.vertical_centered_justified(|ui| {
        changed |= ui
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
            .hint(l.text(Msg::MonitorLevelHover), reason)
            .changed();
    });
    let width = ui.available_width();
    changed |= led_key(ui, enabled, &mut muted, "M", PROGRAM, width)
        .hint(
            l.text(if muted {
                Msg::MonitorMuteOnHover
            } else {
                Msg::MonitorMuteOffHover
            }),
            reason,
        )
        .changed();
    if changed {
        intents.push(StudioIntent::AudioMixer(
            fm_protocol::AudioMixerOperation::SetMonitorLevel {
                gain_millidb,
                muted,
            },
        ));
    }
}

/// How often, in seconds, a visible monitor strip asks the server for its
/// output devices again, so a device plugged in or out shows up.
const OUTPUT_LIST_REFRESH_SECONDS: f64 = 3.0;

/// One listed output device as the picker shows it: its description (or
/// name), then its channel count and whether it is the default.
fn output_device_label(l: Locale, device: &fm_protocol::AudioOutputDevice) -> String {
    let mut label = if device.description.is_empty() {
        device.device.clone()
    } else {
        device.description.clone()
    };
    if let Some(channels) = device.channels {
        label.push_str(" · ");
        label.push_str(&l.format(
            Msg::MonitorPickChannels,
            &[("count", Arg::from(u64::from(channels)))],
        ));
    }
    if device.default {
        label.push_str(" · ");
        label.push_str(l.text(Msg::MonitorPickDefault));
    }
    label
}

/// The chosen device when the server's list is known, readable, and does
/// not name it: unplugged, or not an output the server sees.
fn missing_output<'a>(
    current: Option<&'a str>,
    inventory: Option<&fm_protocol::AudioOutputInventory>,
) -> Option<&'a str> {
    let inventory = inventory?;
    current.filter(|device| {
        inventory.supported && inventory.error.is_none() && inventory.device(device).is_none()
    })
}

/// What the operator chose in the headphone device picker.
enum OutputPick {
    /// Meter only: no device.
    Off,
    /// A device (the current one again retries it).
    Device(String),
    /// Type a name the list does not show.
    Type,
}

/// The headphone device control: a picker of the server's output devices
/// (Protocol 2.102) with meter-only and a typed name as fallbacks, or the
/// device field where the server lists none (or this session may not ask).
/// A chosen device is pushed as `SetMonitorDevice`; choosing the current
/// device again retries it. While shown, the list is asked for again every
/// few seconds and whenever the picker opens, so hot-plug changes show.
fn monitor_device_control(
    ui: &mut Ui,
    view: &ClientView,
    (enabled, reason, list_outputs): (bool, &str, bool),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let current = view.audio_mixer.monitor.device.as_deref();
    let inventory = view.audio_output_inventory.as_ref();
    let id = ui.make_persistent_id("monitor-device");
    let asked_id = id.with("listed-at");
    let now = ui.input(|input| input.time);
    if list_outputs {
        let asked = ui.data(|data| data.get_temp::<f64>(asked_id));
        if asked.is_none_or(|asked| now < asked || now - asked >= OUTPUT_LIST_REFRESH_SECONDS) {
            intents.push(StudioIntent::RequestAudioOutputDevices);
            ui.data_mut(|data| data.insert_temp(asked_id, now));
        }
    }
    let choose = |intents: &mut Vec<StudioIntent>, device: Option<String>| {
        intents.push(StudioIntent::AudioMixer(
            fm_protocol::AudioMixerOperation::SetMonitorDevice { device },
        ));
    };
    let typing = ui.data(|data| data.get_temp::<bool>(id.with("editing")).unwrap_or(false));
    let listable = list_outputs && inventory.is_none_or(|inventory| inventory.supported);
    if typing || !listable {
        if let Some(text) = device_entry(
            ui,
            current.unwrap_or_default(),
            enabled,
            (
                l.text(Msg::MonitorDevicePlaceholder),
                l.text(Msg::MonitorDeviceHover),
                reason,
            ),
        ) {
            choose(intents, (!text.is_empty()).then_some(text));
        }
        return;
    }
    let missing = missing_output(current, inventory);
    let selected = match current {
        None => l.text(Msg::MonitorPickNone).to_owned(),
        Some(device) => match inventory.and_then(|inventory| inventory.device(device)) {
            Some(listed) => output_device_label(l, listed),
            None if missing.is_some() => {
                l.format(Msg::MonitorPickMissing, &[("device", Arg::Str(device))])
            }
            None => device.to_owned(),
        },
    };
    let mut pick = None;
    let combo = ui
        .add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(id.with("picker"))
                .width(ui.available_width())
                .truncate()
                .selected_text(RichText::new(selected).color(if missing.is_some() {
                    PROGRAM
                } else {
                    text_color()
                }))
                .show_ui(ui, |ui| {
                    pick = output_picker_items(ui, l, current, inventory, missing);
                })
        })
        .inner;
    let response = combo
        .response
        .hint(&output_picker_hover(l, inventory, missing), reason);
    if response.clicked() && list_outputs {
        intents.push(StudioIntent::RequestAudioOutputDevices);
        ui.data_mut(|data| data.insert_temp(asked_id, now));
    }
    match pick {
        Some(OutputPick::Off) => choose(intents, None),
        Some(OutputPick::Device(device)) => choose(intents, Some(device)),
        Some(OutputPick::Type) => {
            ui.data_mut(|data| data.insert_temp(id.with("editing"), true));
            ui.memory_mut(|memory| memory.request_focus(id.with("field")));
        }
        None => {}
    }
}

/// The headphone device picker's rows: meter-only, the listing state, the
/// listed devices, the chosen device when it is no longer listed, and
/// typing a name. Returns what was clicked.
fn output_picker_items(
    ui: &mut Ui,
    l: Locale,
    current: Option<&str>,
    inventory: Option<&fm_protocol::AudioOutputInventory>,
    missing: Option<&str>,
) -> Option<OutputPick> {
    ui.set_min_width(240.0);
    let mut pick = None;
    if ui
        .selectable_label(current.is_none(), l.text(Msg::MonitorPickNone))
        .on_hover_text(l.text(Msg::MonitorPickNoneHover))
        .clicked()
        && current.is_some()
    {
        pick = Some(OutputPick::Off);
    }
    match inventory {
        None => {
            ui.label(RichText::new(l.text(Msg::MonitorPickListing)).weak())
                .on_hover_text(l.text(Msg::MonitorPickListingHover));
        }
        Some(inventory) => {
            if let Some(error) = &inventory.error {
                ui.label(RichText::new(l.text(Msg::MonitorPickListError)).color(AMBER))
                    .on_hover_text(l.format(
                        Msg::MonitorPickListErrorHover,
                        &[("error", Arg::Str(error))],
                    ));
            }
            for device in &inventory.devices {
                let name = if device.description.is_empty() {
                    device.device.as_str()
                } else {
                    device.description.as_str()
                };
                if ui
                    .selectable_label(
                        current == Some(device.device.as_str()),
                        output_device_label(l, device),
                    )
                    .on_hover_text(l.format(
                        Msg::MonitorPickItemHover,
                        &[
                            ("name", Arg::Str(name)),
                            ("device", Arg::Str(&device.device)),
                        ],
                    ))
                    .clicked()
                {
                    pick = Some(OutputPick::Device(device.device.clone()));
                }
            }
        }
    }
    if let Some(device) = missing
        && ui
            .selectable_label(
                true,
                RichText::new(l.format(Msg::MonitorPickMissing, &[("device", Arg::Str(device))]))
                    .color(PROGRAM),
            )
            .on_hover_text(l.format(
                Msg::MonitorPickMissingHover,
                &[("device", Arg::Str(device))],
            ))
            .clicked()
    {
        pick = Some(OutputPick::Device(device.to_owned()));
    }
    if ui
        .selectable_label(false, l.text(Msg::MonitorPickTyped))
        .on_hover_text(l.text(Msg::MonitorPickTypedHover))
        .clicked()
    {
        pick = Some(OutputPick::Type);
    }
    pick
}

/// The headphone device picker's help: what it does, then whether the list
/// is still awaited or could not be read, and whether the chosen device is
/// no longer listed.
fn output_picker_hover(
    l: Locale,
    inventory: Option<&fm_protocol::AudioOutputInventory>,
    missing: Option<&str>,
) -> String {
    let mut hover = l.text(Msg::MonitorPickerHover).to_owned();
    match inventory {
        None => {
            hover.push('\n');
            hover.push_str(l.text(Msg::MonitorPickListingHover));
        }
        Some(fm_protocol::AudioOutputInventory {
            error: Some(error), ..
        }) => {
            hover.push('\n');
            hover.push_str(&l.format(
                Msg::MonitorPickListErrorHover,
                &[("error", Arg::Str(error))],
            ));
        }
        Some(_) => {}
    }
    if let Some(device) = missing {
        hover.push('\n');
        hover.push_str(&l.format(
            Msg::MonitorPickMissingHover,
            &[("device", Arg::Str(device))],
        ));
    }
    hover
}

/// The monitor device editor: commits on Enter or focus loss and returns
/// the trimmed text when it differs from `current` (empty: no device).
/// Text the server would refuse is never sent. While not being edited, a
/// set device shows as a key whose text ends in an ellipsis when it does
/// not fit the strip (an ALSA name rarely does), with the full name on
/// hover; clicking it opens the field.
fn device_entry(
    ui: &mut Ui,
    current: &str,
    enabled: bool,
    (placeholder, hover, reason): (&str, &str, &str),
) -> Option<String> {
    let id = ui.make_persistent_id("monitor-device");
    let editing_id = id.with("editing");
    let field_id = id.with("field");
    let editing = ui.data(|data| data.get_temp::<bool>(editing_id).unwrap_or(false));
    if !editing && !current.is_empty() {
        let l = l10n::locale(ui.ctx());
        let shown = ui
            .add_enabled(
                enabled,
                egui::Button::new(RichText::new(current).monospace())
                    .truncate()
                    .fill(ui.visuals().extreme_bg_color)
                    .min_size(Vec2::new(ui.available_width(), 0.0)),
            )
            .hint(
                &l.format(
                    Msg::MonitorDeviceShownHover,
                    &[("device", Arg::Str(current))],
                ),
                reason,
            );
        if shown.clicked() {
            ui.data_mut(|data| data.insert_temp(editing_id, true));
            ui.memory_mut(|memory| memory.request_focus(field_id));
        }
        return None;
    }
    let mut text = ui
        .data_mut(|data| data.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_owned());
    let response = ui
        .add_enabled(
            enabled,
            egui::TextEdit::singleline(&mut text)
                .id(field_id)
                .char_limit(fm_protocol::MAX_AUDIO_MONITOR_DEVICE_BYTES)
                .hint_text(RichText::new(placeholder).monospace())
                .font(egui::TextStyle::Monospace),
        )
        .hint(hover, reason);
    let text = text.trim().to_owned();
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, text));
        None
    } else {
        // Focus requested by the key lands on the next frame.
        let opening = ui.memory(|memory| memory.focused() == Some(field_id));
        if editing && !opening && !response.gained_focus() {
            ui.data_mut(|data| data.remove::<bool>(editing_id));
        }
        ui.data_mut(|data| data.remove::<String>(id));
        // Enter on the same device reopens it, so a failed device can be
        // retried; leaving the field unchanged sends nothing.
        let entered = ui.input(|input| input.key_pressed(egui::Key::Enter));
        let retry = entered && !text.is_empty() && text == current;
        (response.lost_focus()
            && (text != current || retry)
            && (text.is_empty() || fm_protocol::audio_monitor_device_is_valid(&text)))
        .then_some(text)
    }
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
        ui.allocate_exact_size(Vec2::new(ui.available_width(), READOUT_HEIGHT), sense);
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
/// `sync` is its live source clock row (AU-004).
#[allow(clippy::too_many_lines)] // one immediate-mode region
fn draw_input_strip(
    ui: &mut Ui,
    (view, fx_on, fx): (&ClientView, bool, &mut Option<StripDspTarget>),
    (status, sync, loudness): (
        &InputAudioStripStatus,
        SyncRow<'_>,
        Option<AudioLoudnessMeter>,
    ),
    feed: MeterFeed<'_>,
    gate: StripGate,
    layout: StripLayout,
    intents: &mut Vec<StudioIntent>,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let StripGate {
        enabled,
        reason,
        details,
    } = gate;
    let input_name = label_for_input(l, view, status.input);
    let strip_label = view.audio_label(status.input).to_owned();
    let (name, plate_hover) = if strip_label.is_empty() {
        (input_name.clone(), input_name.clone())
    } else {
        (
            strip_label.clone(),
            l.format(
                Msg::StripNamePlateHover,
                &[
                    ("label", Arg::Str(&strip_label)),
                    ("name", Arg::Str(&input_name)),
                ],
            ),
        )
    };
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
    let mut pan_basis_points = status.pan_basis_points;
    let mut pan_changed = false;
    let mut muted = status.muted;
    let mut soloed = status.soloed;
    let mut follow_video = status.follow_video;
    let mut delay_samples = status.delay_samples;
    let mut buses = status.buses;
    let mut gain_changed = false;
    let mut balance_changed = false;
    let mut mute_changed = false;
    let mut solo_changed = false;
    let mut follow_changed = false;
    let mut delay_changed = false;
    let mut clear_clip = false;
    strip_frame(ui, accent, layout.width, |ui| {
        name_plate(ui, ordinal, &name, &plate_hover, tally);
        clear_clip |= peak_box(ui, feed.holds, feed.channels.is_some(), feed.stale);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            clear_clip |= draw_meter(ui, feed, layout);
            let fader_width = ui.available_width();
            gain_changed |= fader(ui, &mut gain_millidb, (enabled, muted), fader_width, layout)
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
        pan_changed = pan_knob(ui, &mut pan_basis_points, enabled, layout.inner_width())
            .hint(l.text(Msg::PanHover), reason)
            .changed();
        if details {
            ui.vertical_centered_justified(|ui| {
                if let Some(label) = label_entry(
                    ui,
                    ("strip-label", status.input.get()),
                    &strip_label,
                    enabled,
                    (&input_name, l.text(Msg::StripLabelHover), reason),
                ) {
                    intents.push(StudioIntent::SetAudioStripLabel {
                        input: status.input,
                        label,
                    });
                }
                if let Some(group) = group_choice(ui, view, status, enabled, reason) {
                    intents.push(StudioIntent::SetAudioStripGroup {
                        input: status.input,
                        group,
                    });
                }
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
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let half = (layout.inner_width() - 3.0) / 2.0;
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
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let half = (layout.inner_width() - 3.0) / 2.0;
            follow_changed = led_key(ui, enabled, &mut follow_video, "AFV", accent_color(), half)
                .hint(l.text(Msg::AfvHover), reason)
                .changed();
            if fx_key(ui, fx_on, l.text(Msg::FxKeyHover), half) {
                *fx = Some(StripDspTarget::Input(status.input));
            }
        });
        buses = bus_keys(ui, enabled, reason, buses, layout.inner_width());
        loudness_box(ui, loudness.as_ref(), false, feed.stale);
        let row_height = crate::source_sync_ui::SYNC_ROW_HEIGHT - 4.0;
        match sync {
            SyncRow::Row {
                sync: Some(sample),
                audio: Some(entry),
            } => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let half = (layout.inner_width() - 3.0) / 2.0;
                    crate::source_sync_ui::strip_badge(ui, sample, &input_name, half);
                    crate::screen_capture_ui::audio_badge_key(
                        ui,
                        l,
                        entry,
                        Vec2::new(half, row_height),
                    );
                });
            }
            SyncRow::Row {
                sync: Some(sample),
                audio: None,
            } => {
                crate::source_sync_ui::strip_badge(ui, sample, &input_name, layout.inner_width());
            }
            SyncRow::Row {
                sync: None,
                audio: Some(entry),
            } => {
                crate::screen_capture_ui::audio_badge_key(
                    ui,
                    l,
                    entry,
                    Vec2::new(layout.inner_width(), row_height),
                );
            }
            SyncRow::Row {
                sync: None,
                audio: None,
            } => {
                ui.allocate_space(Vec2::new(layout.inner_width(), row_height));
            }
            SyncRow::Absent => {}
        }
    });
    let update = InputAudioStripUpdate {
        gain_millidb: gain_changed.then_some(gain_millidb),
        balance_basis_points: balance_changed.then_some(balance_basis_points),
        pan_basis_points: pan_changed.then_some(pan_basis_points),
        muted: mute_changed.then_some(muted),
        soloed: solo_changed.then_some(soloed),
        follow_video: follow_changed.then_some(follow_video),
        delay_samples: delay_changed.then_some(delay_samples),
        buses: (buses != status.buses).then_some(buses),
    };
    if update != InputAudioStripUpdate::default() {
        intents.push(StudioIntent::SetInputAudioStrip {
            input: status.input,
            update,
        });
    }
    clear_clip
}

/// The strip's bus keys (AU-002), like a vMix strip's: `M` feeds Master and
/// `A`–`G` feed the aux buses, four to a row. Returns the strip's buses after
/// any click.
fn bus_keys(
    ui: &mut Ui,
    enabled: bool,
    reason: &str,
    mut buses: fm_types::audio_bus::AudioBusMask,
    width: f32,
) -> fm_types::audio_bus::AudioBusMask {
    let l = l10n::locale(ui.ctx());
    let aux = fm_types::audio_bus::AuxBusNumber::ALL;
    let rows: [[Option<fm_types::audio_bus::AuxBusNumber>; 4]; 2] = [
        [None, Some(aux[0]), Some(aux[1]), Some(aux[2])],
        [Some(aux[3]), Some(aux[4]), Some(aux[5]), Some(aux[6])],
    ];
    for row in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let quarter = (width - 6.0) / 4.0;
            for bus in row {
                let (mut on, label, hover) = match bus {
                    None => (
                        buses.master(),
                        "M".to_owned(),
                        l.text(Msg::StripBusMasterHover).to_owned(),
                    ),
                    Some(bus) => (
                        buses.aux(bus),
                        bus.letter().to_string(),
                        l.format(
                            Msg::StripBusAuxHover,
                            &[("bus", Arg::Str(&bus.letter().to_string()))],
                        ),
                    ),
                };
                let color = if bus.is_none() {
                    PROGRAM
                } else {
                    accent_color()
                };
                if led_key(ui, enabled, &mut on, &label, color, quarter)
                    .hint(&hover, reason)
                    .changed()
                {
                    buses = match bus {
                        None => buses.with_master(on),
                        Some(bus) => buses.with_aux(bus, on),
                    };
                }
            }
        });
    }
    buses
}

/// A latching console key with an LED bar: lit in `color` while on, an unlit
/// LED and dim label while off. `changed()` reports a click that flipped
/// `value`.
pub(crate) fn led_key(
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
    (enabled, muted): (bool, bool),
    width: f32,
    layout: StripLayout,
) -> egui::Response {
    let height = layout.fader_height;
    let cap_size = layout.fader_cap();
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    let track_x = rect.max.x - cap_size.x / 2.0 - 1.0;
    let top = rect.min.y + cap_size.y / 2.0 + 1.0;
    let bottom = rect.max.y - cap_size.y / 2.0 - 1.0;
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
    // Labels clear the cap, so the unity mark reads beside a cap at 0 dB.
    let label_x = track_x - (cap_size.x / 2.0 + 1.0).max(13.0);
    for &(mdb, label) in layout.fader_ticks() {
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
        if (last_label_y - y).abs() >= 10.0 && label_x > rect.min.x + 6.0 {
            painter.text(
                pos2(label_x, y),
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
    let cap = Rect::from_center_size(pos2(track_x, cap_y), cap_size);
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
            pos2(cap.min.x - 3.0, cap_y - cap_size.y),
            Align2::RIGHT_CENTER,
            format_gain(l10n::locale(painter.ctx()), *gain_mdb),
            FontId::new(9.5, FontFamily::Monospace),
            text_color(),
        );
    }
    response
}

/// Draws the Master strip; returns `true` when the operator cleared its clip.
fn draw_master_strip(
    ui: &mut Ui,
    feed: MeterFeed<'_>,
    layout: StripLayout,
    (fx_on, fx): (bool, &mut Option<StripDspTarget>),
    loudness: Option<AudioLoudnessMeter>,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let mut clear_clip = false;
    let stale = feed.stale;
    strip_frame(ui, accent_color(), layout.width, |ui| {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(layout.inner_width(), NAME_PLATE_HEIGHT),
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
            clear_clip |= draw_meter(ui, feed, layout);
            draw_meter_scale(ui, layout);
        });
        ui.vertical_centered(|ui| {
            // One line: the tap label shrinks to the strip instead of
            // wrapping mid-word when high contrast enlarges small text.
            let text = l.text(Msg::MasterTap);
            let size = fitted_font_size(ui, text, crate::theme::legible(8.5), ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(text).size(size).color(muted_color())).truncate(),
            )
            .on_hover_text(l.text(Msg::MasterTapHover));
        });
        if fx_key(
            ui,
            fx_on,
            l.text(Msg::FxMasterKeyHover),
            layout.inner_width(),
        ) {
            *fx = Some(StripDspTarget::Master);
        }
        loudness_box(ui, loudness.as_ref(), true, stale);
    });
    clear_clip
}

/// The largest proportional font size up to `size` (and no smaller than
/// 7 pt) at which `text` fits `width` on one line.
fn fitted_font_size(ui: &Ui, text: &str, size: f32, width: f32) -> f32 {
    let natural = ui
        .painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::new(size, FontFamily::Proportional),
            Color32::WHITE,
        )
        .size()
        .x;
    if natural <= width || natural <= 0.0 {
        size
    } else {
        (size * width / natural).floor().max(7.0)
    }
}

/// Width of a meter well for `bars` channel bars `bar` points wide,
/// including its tick gutter.
#[allow(clippy::cast_precision_loss)] // one or two bars
pub(crate) fn meter_width(bars: usize, bar: f32) -> f32 {
    bars as f32 * (bar + 2.0) + 4.0 + METER_TICK_GUTTER
}

/// Draws one segmented meter well with a bar per channel: RMS as the lit
/// segments, the latest peak as a thin line, the held peak as a bright marker,
/// a clip lamp on top, and scale ticks in a gutter on the right. Without
/// channels the well stays unlit. Returns `true` when a latched clip lamp was
/// clicked (clear request).
#[allow(clippy::too_many_lines)] // one painter pass per meter region
fn draw_meter(ui: &mut Ui, feed: MeterFeed<'_>, layout: StripLayout) -> bool {
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
    let bar_width = layout.bar_width();
    let (outer, response) = ui.allocate_exact_size(
        Vec2::new(meter_width(bars, bar_width), layout.fader_height),
        sense,
    );
    if !ui.is_rect_visible(outer) {
        // Scrolled out of view: nothing to paint, but a clickable meter
        // still explains itself.
        let _ = response.on_hover_text(l.text(if clipped {
            Msg::MeterClickToClear
        } else {
            Msg::NoMeterSample
        }));
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
    for &db in layout.meter_ticks() {
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
        let x = inner.min.x + bar as f32 * (bar_width + 2.0);
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
                pos2(x + bar_width, y_bottom),
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
                [pos2(x, y), pos2(x + bar_width, y)],
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
                    [pos2(x - 0.5, y), pos2(x + bar_width + 0.5, y)],
                    Stroke::new(
                        2.0,
                        stale_dim(meter_segment_color(held_db).gamma_multiply(1.15), stale),
                    ),
                );
            }
        }
        // Clip lamp: dark when idle, lit red (with a glow) while latched.
        let lamp = Rect::from_min_size(pos2(x, well.min.y), Vec2::new(bar_width, CLIP_LAMP_HEIGHT));
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
pub(crate) fn labelled_meter_ticks(
    ticks: &[f32],
    top: f32,
    bottom: f32,
    spacing: f32,
) -> Vec<(f32, f32)> {
    let y_of = |db: f32| bottom - meter_fraction(db) * (bottom - top);
    let floor_y = y_of(METER_FLOOR_DB);
    let mut labels: Vec<(f32, f32)> = Vec::new();
    for &db in ticks {
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
fn draw_meter_scale(ui: &mut Ui, layout: StripLayout) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(24.0, layout.fader_height), Sense::hover());
    let painter = ui.painter_at(rect.expand2(Vec2::new(0.0, 6.0)));
    let top = rect.min.y + 2.0 + CLIP_LAMP_HEIGHT + 2.0;
    let bottom = rect.max.y - 2.0;
    for (db, y) in labelled_meter_ticks(layout.meter_ticks(), top, bottom, METER_LABEL_SPACING) {
        // Clear of the gutter's tick, so `-6` does not read as `- -6`.
        painter.text(
            pos2(rect.min.x + 4.0, y),
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

/// The Aux buses window (AU-002): each bus's name, fader, and mute, and the
/// bus each project output hears.
pub(crate) fn draw_bus_mixer_window(
    open: &mut bool,
    context: &egui::Context,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    if !*open {
        return;
    }
    let l = l10n::locale(context);
    // A floating, non-modal window: the operator keeps the console and the
    // Program monitor in reach while riding a bus fader.
    egui::Window::new(l.text(Msg::BusMixerTitle))
        .id(egui::Id::new("studio-bus-mixer"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(context, |ui| {
            ui.set_width(520.0);
            draw_bus_mixer_contents(ui, state, intents);
        });
}

#[allow(clippy::too_many_lines)] // one row per bus, then the outputs
fn draw_bus_mixer_contents(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let Some(view) = &state.view else {
        crate::theme::empty_state(
            ui,
            l.text(Msg::NoAudioState),
            l.text(Msg::NoAudioStateDetail),
        );
        return;
    };
    let ready = state.connection_status.controls_enabled() && state.view.is_some();
    let audio_enabled = ready && state.can_control_audio;
    let edit_enabled = ready && state.can_edit_project;
    let audio_reason = unavailable_reason(state, Capability::Audio);
    let edit_reason = unavailable_reason(state, Capability::EditProject);
    ui.label(caption(l.text(Msg::BusMixerNote)));
    ui.add_space(6.0);
    for (number, bus) in fm_types::audio_bus::AuxBusNumber::ALL
        .into_iter()
        .zip(&view.audio_mixer.aux_buses)
    {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(number.letter().to_string())
                    .strong()
                    .color(accent_color()),
            );
            if let Some(name) = label_entry(
                ui,
                ("aux-bus-name", number.index()),
                &bus.name,
                edit_enabled,
                (&bus.name, l.text(Msg::BusMixerNameHover), edit_reason),
            ) {
                intents.push(StudioIntent::AudioMixer(
                    fm_protocol::AudioMixerOperation::RenameAuxBus { bus: number, name },
                ));
            }
            let mut gain_millidb = bus.gain_millidb;
            let mut muted = bus.muted;
            let changed = ui
                .add_enabled(
                    audio_enabled,
                    DragValue::new(&mut gain_millidb)
                        .range(-96_000..=24_000)
                        .speed(100.0)
                        .custom_formatter(|value, _| {
                            #[allow(clippy::cast_possible_truncation)] // bounded range
                            format_gain(l, value as i32)
                        })
                        .custom_parser(|text| parse_gain(l, text)),
                )
                .hint(l.text(Msg::BusMixerFaderHover), audio_reason)
                .changed();
            let muted_changed = led_key(
                ui,
                audio_enabled,
                &mut muted,
                l.text(Msg::AuxBusMuteKey),
                PROGRAM,
                52.0,
            )
            .hint(l.text(Msg::BusMixerMuteHover), audio_reason)
            .changed();
            if changed || muted_changed {
                intents.push(StudioIntent::AudioMixer(
                    fm_protocol::AudioMixerOperation::SetAuxBusLevel {
                        bus: number,
                        gain_millidb,
                        muted,
                    },
                ));
            }
            let channels = state.audio_meters.as_ref().and_then(|meters| {
                meters
                    .aux
                    .iter()
                    .find(|aux| aux.bus == number)
                    .map(|aux| aux.channels.as_slice())
            });
            bus_meter(ui, channels, 56.0, 14.0);
        });
    }
    ui.add_space(8.0);
    ui.label(section_label(l.text(Msg::BusMixerOutputs)));
    if view.outputs.is_empty() {
        ui.label(caption(l.text(Msg::BusMixerNoOutputs)));
    }
    for output in &view.outputs {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(&output.name);
            let mut chosen = output.aux_bus;
            let selected = match chosen {
                None => l.text(Msg::BusMixerMaster).to_owned(),
                Some(bus) => l.format(
                    Msg::BusMixerBus,
                    &[("bus", Arg::Str(&bus.letter().to_string()))],
                ),
            };
            ui.add_enabled_ui(edit_enabled, |ui| {
                egui::ComboBox::from_id_salt(("bus-mixer-output", output.output))
                    .selected_text(selected)
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut chosen, None, l.text(Msg::BusMixerMaster));
                        for bus in fm_types::audio_bus::AuxBusNumber::ALL {
                            ui.selectable_value(
                                &mut chosen,
                                Some(bus),
                                l.format(
                                    Msg::BusMixerBus,
                                    &[("bus", Arg::Str(&bus.letter().to_string()))],
                                ),
                            );
                        }
                    });
            })
            .response
            .hint(l.text(Msg::BusMixerOutputHover), edit_reason);
            if chosen != output.aux_bus {
                intents.push(StudioIntent::AudioMixer(
                    fm_protocol::AudioMixerOperation::SetOutputBus {
                        output: fm_protocol::WireOutputId::from_domain(output.output),
                        bus: chosen,
                    },
                ));
            }
        });
    }
}

/// A compact two-bar meter for one aux bus, from its `audio_meters` record
/// (Protocol 2.120, AU-002): a bar per channel on a -60 dBFS to 0 dBFS
/// scale, with the peak in dBFS on hover.
fn bus_meter(
    ui: &mut Ui,
    channels: Option<&[fm_protocol::AudioMeterChannel]>,
    width: f32,
    height: f32,
) {
    let l = l10n::locale(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), graphite_color());
    let Some(channels) = channels else {
        response.on_hover_text(l.text(Msg::BusMixerNoMeterHover));
        return;
    };
    let level = |channel: &fm_protocol::AudioMeterChannel| {
        #[allow(clippy::cast_precision_loss)] // a bounded millionth count
        let amplitude = channel.peak_millionths as f32 / 1_000_000.0;
        let db = 20.0 * amplitude.max(1e-6).log10();
        (db, ((db + 60.0) / 60.0).clamp(0.0, 1.0))
    };
    let bar_width = (rect.width() - 3.0) / 2.0;
    let mut readout = Vec::new();
    for (index, channel) in channels.iter().take(2).enumerate() {
        let (db, fraction) = level(channel);
        #[allow(clippy::cast_precision_loss)] // at most two bars
        let x = rect.left() + index as f32 * (bar_width + 3.0);
        let bar = Rect::from_min_size(pos2(x, rect.top()), Vec2::new(bar_width, rect.height()));
        let fill = Rect::from_min_size(
            pos2(bar.left(), bar.bottom() - bar.height() * fraction),
            Vec2::new(bar.width(), bar.height() * fraction),
        );
        ui.painter()
            .rect_filled(fill, CornerRadius::same(1), PROGRAM.gamma_multiply(0.85));
        readout.push(format!("{} dB", l.decimal(f64::from(db), 1)));
    }
    let readout = readout.join(" · ");
    response.on_hover_text(format!("{}\n{}", readout, l.text(Msg::BusMixerMeterHover)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_meter_is_the_loudest_member_per_channel() {
        let id = |value: u128| {
            fm_types::InputId::new(core::num::NonZeroU128::new(value).expect("non-zero"))
        };
        let channel = |peak, rms| AudioMeterChannel {
            peak_millionths: peak,
            rms_millionths: rms,
        };
        let meter = |input: u128, channels: Vec<AudioMeterChannel>| fm_protocol::InputAudioMeters {
            input: fm_protocol::WireInputId::from_domain(id(input)),
            channels,
        };
        let inputs = [
            meter(1, vec![channel(500_000, 200_000), channel(100_000, 50_000)]),
            meter(2, vec![channel(300_000, 250_000)]),
            meter(
                3,
                vec![channel(900_000, 900_000), channel(900_000, 900_000)],
            ),
        ];
        assert_eq!(
            group_meter(&inputs, &[id(1), id(2)]),
            Some(vec![channel(500_000, 250_000), channel(100_000, 50_000)])
        );
        assert_eq!(group_meter(&inputs, &[id(4)]), None);
        assert_eq!(group_meter(&[], &[id(1)]), None);
    }

    #[test]
    fn nested_groups_are_active_through_their_children_and_parents_stay_acyclic() {
        let id = |value: u128| {
            fm_types::InputId::new(core::num::NonZeroU128::new(value).expect("non-zero"))
        };
        let mut mixer = fm_protocol::AudioMixerStatus::default();
        mixer.groups[0].parent = Some(2);
        let strip = |input: u128, group: Option<u8>| fm_ui_model::InputAudioStripStatus {
            input: id(input),
            gain_millidb: 0,
            balance_basis_points: 0,
            pan_basis_points: 0,
            group,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
            buses: fm_types::audio_bus::AudioBusMask::MASTER,
        };
        let strips = [strip(1, Some(1)), strip(2, None), strip(3, Some(3))];
        // Group 2 has no strips of its own but nests group 1.
        assert_eq!(
            group_members(&mixer, &strips),
            vec![(1, vec![id(1)]), (2, vec![id(1)]), (3, vec![id(3)])]
        );
        // Group 2 may not nest inside group 1, its own child.
        assert_eq!(parent_choices(&mixer, 2), vec![3, 4]);
        assert_eq!(parent_choices(&mixer, 1), vec![2, 3, 4]);
    }

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
            labelled_meter_ticks(&METER_TICKS_DB, 0.0, height, METER_LABEL_SPACING)
                .into_iter()
                .map(|(db, _)| db)
                .collect()
        };
        assert_eq!(all(600.0), METER_TICKS_DB.to_vec());
        let short = all(110.0);
        assert_eq!(short.first(), Some(&0.0));
        assert_eq!(short.last(), Some(&-60.0));
        assert!(short.len() < METER_TICKS_DB.len());
        let ys: Vec<f32> = labelled_meter_ticks(&METER_TICKS_DB, 0.0, 110.0, METER_LABEL_SPACING)
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

    #[test]
    fn strips_share_the_panel_width_up_to_the_widest_strip() {
        // A maximised mixer: six strips grow to the widest strip, with tall
        // faders, fine scales, and wide meter bars.
        let big = strip_layout(Vec2::new(1_900.0, 1_000.0), 6, false);
        assert!((big.width - MAX_STRIP_WIDTH).abs() < f32::EPSILON);
        assert!((big.fader_height - 786.0).abs() < f32::EPSILON);
        assert!(big.fine_scale());
        assert!((big.bar_width() - 10.0).abs() < f32::EPSILON);
        assert_eq!(big.fader_cap(), Vec2::new(40.0, 18.0));
        assert_eq!(big.meter_ticks(), &FINE_METER_TICKS_DB);
        assert_eq!(big.fader_ticks(), &FINE_FADER_TICKS);
        // A mid-sized dock: the strips widen to fill it exactly.
        let mid = strip_layout(Vec2::new(1_100.0, 600.0), 6, false);
        assert!((mid.width - 128.0).abs() < f32::EPSILON);
        assert!((mid.inner_width() - 118.0).abs() < f32::EPSILON);
        assert!((mid.fader_height - 386.0).abs() < f32::EPSILON);
        // A narrow dock: compact strips with the coarse scales.
        let compact = strip_layout(Vec2::new(600.0, 250.0), 6, false);
        assert!((compact.width - STRIP_WIDTH).abs() < f32::EPSILON);
        assert!((compact.fader_height - MIN_FADER_HEIGHT).abs() < f32::EPSILON);
        assert!(!compact.fine_scale());
        assert!((compact.bar_width() - 5.0).abs() < f32::EPSILON);
        assert_eq!(compact.fader_cap(), FADER_CAP);
        assert_eq!(compact.meter_ticks(), &METER_TICKS_DB);
        assert_eq!(compact.fader_ticks(), &FADER_TICKS);
        // The typed entries take their rows from the faders; a very tall
        // panel stops the faders at the longest travel.
        let details = strip_layout(Vec2::new(1_100.0, 600.0), 6, true);
        assert!((mid.fader_height - details.fader_height - ENTRY_ROWS_HEIGHT).abs() < 1.0);
        assert!((details.width - mid.width).abs() < f32::EPSILON);
        let tall = strip_layout(Vec2::new(1_100.0, 3_000.0), 6, false);
        assert!((tall.fader_height - MAX_FADER_HEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn strips_scroll_only_when_compact_strips_do_not_fit() {
        for panel_width in [400.0_f32, 700.0, 1_000.0, 1_366.0, 1_920.0, 2_560.0] {
            for strips in 0..=24 {
                let layout = strip_layout(Vec2::new(panel_width, 700.0), strips, false);
                assert!(layout.width >= STRIP_WIDTH && layout.width <= MAX_STRIP_WIDTH);
                // The pinned Monitor and Master strips sit at the right.
                let room = (panel_width - 2.0 * layout.pitch()).max(layout.width);
                let viewport = whole_strips_width(room, strips, layout.pitch());
                #[allow(clippy::cast_precision_loss)] // tiny counts
                let content = strips as f32 * layout.pitch() - STRIP_GAP;
                let scrolls = content > viewport;
                // Strips wider than compact always fit without scrolling.
                if layout.width > STRIP_WIDTH {
                    assert!(!scrolls, "{panel_width} px, {strips} strips");
                }
                // Scrolling shows only whole strips.
                if scrolls {
                    let whole = (viewport + STRIP_GAP) / layout.pitch();
                    assert!((whole - whole.round()).abs() < 1e-3, "{whole}");
                }
            }
        }
        // Twenty strips in a 1100 px dock: compact strips, eleven visible.
        let layout = strip_layout(Vec2::new(1_100.0, 600.0), 20, false);
        assert!((layout.width - STRIP_WIDTH).abs() < f32::EPSILON);
        let room = 1_100.0 - 2.0 * layout.pitch();
        assert!((whole_strips_width(room, 20, layout.pitch()) - (10.0 * 90.0 - 6.0)).abs() < 0.01);
    }

    #[test]
    fn fine_scales_keep_the_coarse_marks_in_order() {
        let positions: Vec<f32> = FINE_FADER_TICKS
            .iter()
            .map(|(mdb, _)| fader_position(*mdb))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] > pair[1]));
        assert!(FINE_METER_TICKS_DB.windows(2).all(|pair| pair[0] > pair[1]));
        for tick in FADER_TICKS {
            assert!(FINE_FADER_TICKS.contains(&tick), "{tick:?}");
        }
        for db in METER_TICKS_DB {
            assert!(FINE_METER_TICKS_DB.contains(&db), "{db}");
        }
        // A tall meter labels every fine mark; a short one keeps the ends.
        let tall = labelled_meter_ticks(&FINE_METER_TICKS_DB, 0.0, 800.0, METER_LABEL_SPACING);
        assert_eq!(tall.len(), FINE_METER_TICKS_DB.len());
        // The meter well grows with its bars.
        assert!(meter_width(2, 10.0) > meter_width(2, 5.0));
        assert!((meter_width(2, 5.0) - 22.0).abs() < f32::EPSILON);
    }
}
