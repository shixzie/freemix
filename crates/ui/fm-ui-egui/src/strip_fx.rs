//! Strip and Master FX (AU-005): the console's per-strip processing window.
//!
//! The `FX` key on an input strip (or on Master) opens one floating window
//! for that chain: a chain bypass, presets, copy and paste between strips,
//! and a reset in the header; an EQ response curve with a draggable point
//! per band (frequency left and right, gain up and down, the wheel for the
//! width) and a draggable high-pass corner; the polarity invert; each
//! band's switch, shape, frequency, gain, and Q; and the gate, expander,
//! compressor, and limiter with their switches, settings, and
//! gain-reduction meters fed by `audio_meters`.
//!
//! Every edit goes through [`StripDspBinding`]: a drag sends the whole
//! chain every few frames under one gesture token, so the server folds it
//! into one undo step, and the window shows the draft until the authority
//! reports it.

use std::time::Instant;

use egui::{
    Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_types::strip_dsp::{
    EqBandKind, MAX_COMPRESSOR_KNEE_MILLIDB, MAX_COMPRESSOR_MAKEUP_MILLIDB,
    MAX_COMPRESSOR_RATIO_MILLI, MAX_EQ_FREQUENCY_HZ, MAX_EQ_GAIN_MILLIDB, MAX_EQ_Q_MILLI,
    MAX_EXPANDER_RANGE_MILLIDB, MAX_EXPANDER_RATIO_MILLI, MAX_GATE_HOLD_US, MAX_GATE_RANGE_MILLIDB,
    MAX_GATE_RELEASE_US, MAX_HIGH_PASS_HZ, MAX_TIME_US, MIN_COMPRESSOR_RATIO_MILLI,
    MIN_COMPRESSOR_THRESHOLD_MILLIDB, MIN_EQ_FREQUENCY_HZ, MIN_EQ_Q_MILLI,
    MIN_EXPANDER_RATIO_MILLI, MIN_EXPANDER_THRESHOLD_MILLIDB, MIN_GATE_THRESHOLD_MILLIDB,
    MIN_HIGH_PASS_HZ, MIN_LIMITER_CEILING_MILLIDB, MIN_TIME_US, StripDsp, StripDspPreset,
    StripDspTarget,
};
use fm_ui_model::strip_dsp::{DRAG_SEND_INTERVAL, StripDspBinding, response_db};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, MONITOR_BLACK, PROGRAM, accent_color, border_color, muted_color, small_button,
    text_color, text_dim_color,
};
use crate::{StudioIntent, StudioShell, StudioUiState};

/// Width of the FX window's contents.
const FX_WIDTH: f32 = 560.0;
/// Height of the EQ response display.
const CURVE_HEIGHT: f32 = 180.0;
/// The response display spans +-this many dB.
const CURVE_RANGE_DB: f64 = 24.0;
/// The response display's lowest and highest frequencies.
const CURVE_LOW_HZ: f64 = 20.0;
const CURVE_HIGH_HZ: f64 = 20_000.0;
/// The sample rate the response curve is drawn at (the Master rate).
const CURVE_SAMPLE_RATE_HZ: f64 = 48_000.0;
/// Radius of a band's point on the response display.
const HANDLE_RADIUS: f32 = 9.0;
/// Full scale of a gain-reduction meter, in dB.
const GR_FULL_SCALE_DB: f32 = 24.0;
/// The band colours, band 1 first.
const BAND_COLORS: [Color32; 5] = [
    Color32::from_rgb(230, 126, 72),
    Color32::from_rgb(226, 196, 74),
    Color32::from_rgb(96, 196, 120),
    Color32::from_rgb(82, 168, 232),
    Color32::from_rgb(178, 124, 232),
];
/// The presets in menu order.
const PRESETS: [StripDspPreset; 5] = [
    StripDspPreset::Flat,
    StripDspPreset::Voice,
    StripDspPreset::Podcast,
    StripDspPreset::Music,
    StripDspPreset::Master,
];

/// The FX window's local state: the chain it edits and the copied chain.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StripFxPanel {
    binding: Option<StripDspBinding>,
    clipboard: Option<StripDsp>,
}

impl StripFxPanel {
    /// Opens the window on `target`'s chain (keeping a draft of the same
    /// chain).
    pub(crate) fn open(&mut self, target: StripDspTarget) {
        if self
            .binding
            .as_ref()
            .is_none_or(|binding| binding.target() != target)
        {
            self.binding = Some(StripDspBinding::new(target));
        }
    }

    /// The chain the window edits, while open.
    #[must_use]
    pub(crate) fn target(&self) -> Option<StripDspTarget> {
        self.binding.as_ref().map(StripDspBinding::target)
    }

    /// Copies `chain` for a later paste.
    pub(crate) const fn copy(&mut self, chain: StripDsp) {
        self.clipboard = Some(chain);
    }

    /// The copied chain, if any.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn clipboard(&self) -> Option<&StripDsp> {
        self.clipboard.as_ref()
    }
}

/// The name of a preset in the console language.
fn preset_name(l: Locale, preset: StripDspPreset) -> &'static str {
    l.text(match preset {
        StripDspPreset::Flat => Msg::FxPresetFlat,
        StripDspPreset::Voice => Msg::FxPresetVoice,
        StripDspPreset::Podcast => Msg::FxPresetPodcast,
        StripDspPreset::Music => Msg::FxPresetMusic,
        StripDspPreset::Master => Msg::FxPresetMaster,
    })
}

fn kind_name(l: Locale, kind: EqBandKind) -> &'static str {
    l.text(match kind {
        EqBandKind::LowShelf => Msg::FxKindLowShelf,
        EqBandKind::Peak => Msg::FxKindPeak,
        EqBandKind::HighShelf => Msg::FxKindHighShelf,
    })
}

/// The strip name the window title shows.
fn target_name(l: Locale, state: &StudioUiState, target: StripDspTarget) -> String {
    match target {
        StripDspTarget::Master => l.text(Msg::FxMasterName).to_owned(),
        StripDspTarget::Input(input) => state.view.as_ref().map_or_else(
            || input.get().to_string(),
            |view| {
                let label = view.audio_label(input);
                if label.is_empty() {
                    crate::monitors::label_for_input(l, view, input)
                } else {
                    label.to_owned()
                }
            },
        ),
    }
}

/// Whether `target` still names a chain of the replicated show.
fn target_exists(state: &StudioUiState, target: StripDspTarget) -> bool {
    match target {
        StripDspTarget::Master => state.view.is_some(),
        StripDspTarget::Input(input) => state
            .view
            .as_ref()
            .is_some_and(|view| view.inputs.contains(&input)),
    }
}

/// The latest gain reduction `audio_meters` reports for `target`:
/// gate, expander, compressor, and limiter, in millidecibels.
fn reductions(state: &StudioUiState, target: StripDspTarget) -> [Option<u32>; 4] {
    let wire = fm_ui_model::strip_dsp::wire_target(target);
    state
        .audio_meters
        .as_ref()
        .and_then(|meters| meters.dynamics.iter().find(|meter| meter.target == wire))
        .map_or([None; 4], |meter| {
            [
                meter.gate_millidb,
                meter.expander_millidb,
                meter.compressor_millidb,
                meter.limiter_millidb,
            ]
        })
}

/// What the header keys asked for this frame.
#[derive(Default)]
struct HeaderOutcome {
    close: bool,
    copy: bool,
}

impl StudioShell {
    /// Draws the FX window while one chain is open, and sends its edits.
    pub(crate) fn draw_strip_fx_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(target) = self.strip_fx.target() else {
            return;
        };
        let Some(project) = state.project_state.as_ref() else {
            return;
        };
        if !target_exists(state, target) {
            self.strip_fx.binding = None;
            return;
        }
        let l = self.locale;
        let authority = project.audio_dsp(target);
        let enabled = state.connection_status.controls_enabled()
            && state.can_control_audio
            && state.view.is_some();
        let reason = unavailable_reason(state, Capability::Audio);
        let area = crate::workspace::tool_area(self, context);
        let title = l.format(
            Msg::FxWindowTitle,
            &[("strip", Arg::Str(&target_name(l, state, target)))],
        );
        let gr = reductions(state, target);
        let loudness = crate::mixer::loudness_for(state, target);
        let clipboard = self.strip_fx.clipboard;
        let Some(binding) = self.strip_fx.binding.as_mut() else {
            return;
        };
        let shown = binding.shown(&authority);
        let pending = binding.is_pending();
        let mut chain = shown;
        let mut header = HeaderOutcome::default();
        egui::Window::new(title.clone())
            .id(egui::Id::new("studio-strip-fx"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .constrain_to(area)
            .pivot(Align2::RIGHT_BOTTOM)
            .default_pos(pos2(area.right() - 8.0, area.bottom() - 8.0))
            .frame(crate::workspace::tool_window_frame())
            .show(context, |ui| {
                let (close, ()) = crate::workspace::floating_window_frame(
                    ui,
                    l,
                    "strip-fx",
                    &title,
                    l.text(Msg::FxCloseHover),
                    |ui| {
                        ui.set_width(FX_WIDTH);
                        header = draw_header(ui, &mut chain, (enabled, reason), pending, clipboard);
                        ui.add_space(4.0);
                        draw_curve(ui, &mut chain, (enabled, reason));
                        ui.add_space(6.0);
                        draw_bands(ui, &mut chain, (enabled, reason));
                        ui.separator();
                        draw_dynamics(ui, &mut chain, (enabled, reason), gr);
                        ui.separator();
                        draw_loudness(ui, loudness.as_ref());
                    },
                );
                header.close |= close;
            });
        if header.copy {
            self.strip_fx.copy(shown);
            context.copy_text(shown.code());
        }
        let Some(binding) = self.strip_fx.binding.as_mut() else {
            return;
        };
        if enabled && chain != shown {
            binding.edit(chain);
        }
        let dragging = context.input(|input| input.pointer.any_down());
        if let Some(operation) = binding.poll(&authority, dragging, Instant::now()) {
            intents.push(StudioIntent::AudioMixer(operation));
        }
        if binding.is_pending() {
            context.request_repaint_after(DRAG_SEND_INTERVAL);
        }
        if header.close {
            self.strip_fx.binding = None;
        }
    }
}

/// The chain keys: bypass, preset, copy, paste, reset, and the pending
/// mark while an edit is on its way.
fn draw_header(
    ui: &mut Ui,
    chain: &mut StripDsp,
    (enabled, reason): (bool, &str),
    pending: bool,
    clipboard: Option<StripDsp>,
) -> HeaderOutcome {
    let l = l10n::locale(ui.ctx());
    let mut outcome = HeaderOutcome::default();
    ui.horizontal(|ui| {
        let mut bypass = chain.bypass;
        if crate::mixer::led_key(ui, enabled, &mut bypass, l.text(Msg::FxBypass), AMBER, 72.0)
            .hint(l.text(Msg::FxBypassHover), reason)
            .changed()
        {
            chain.bypass = bypass;
        }
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt("strip-fx-preset")
                .selected_text(l.text(Msg::FxPresetLabel))
                .width(120.0)
                .show_ui(ui, |ui| {
                    for preset in PRESETS {
                        let name = preset_name(l, preset);
                        if ui
                            .selectable_label(false, name)
                            .on_hover_text(
                                l.format(Msg::FxPresetOptionHover, &[("preset", Arg::Str(name))]),
                            )
                            .clicked()
                        {
                            *chain = preset.chain();
                        }
                    }
                })
                .response
                .hint(l.text(Msg::FxPresetHover), reason);
        });
        outcome.copy = ui
            .add(small_button(l.text(Msg::FxCopy)))
            .on_hover_text(l.text(Msg::FxCopyHover))
            .clicked();
        let paste_reason = if enabled {
            Msg::FxReasonNothingCopied.en()
        } else {
            reason
        };
        if ui
            .add_enabled(
                enabled && clipboard.is_some(),
                small_button(l.text(Msg::FxPaste)),
            )
            .hint(l.text(Msg::FxPasteHover), paste_reason)
            .clicked()
            && let Some(copied) = clipboard
        {
            *chain = copied;
        }
        if ui
            .add_enabled(enabled, small_button(l.text(Msg::FxReset)))
            .hint(l.text(Msg::FxResetHover), reason)
            .clicked()
        {
            *chain = StripDsp::DEFAULT;
        }
        if pending {
            ui.label(
                RichText::new(l.text(Msg::Pending))
                    .size(crate::theme::legible(10.0))
                    .color(AMBER),
            )
            .on_hover_text(l.text(Msg::FxPendingHover));
        }
    });
    outcome
}

/// The x position of `frequency_hz` on the response display.
fn x_of(rect: Rect, frequency_hz: f64) -> f32 {
    let fraction = (frequency_hz / CURVE_LOW_HZ).ln() / (CURVE_HIGH_HZ / CURVE_LOW_HZ).ln();
    #[allow(clippy::cast_possible_truncation)] // a fraction of the display
    let fraction = fraction.clamp(0.0, 1.0) as f32;
    rect.left() + rect.width() * fraction
}

/// The frequency at `x` on the response display.
fn frequency_at(rect: Rect, x: f32) -> f64 {
    let fraction = f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    CURVE_LOW_HZ * (CURVE_HIGH_HZ / CURVE_LOW_HZ).powf(fraction)
}

/// The y position of `db` on the response display.
fn y_of(rect: Rect, db: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)] // a fraction of the display
    let fraction = (db / CURVE_RANGE_DB).clamp(-1.0, 1.0) as f32;
    rect.center().y - fraction * rect.height() / 2.0
}

/// The gain at `y` on the response display, in millidecibels.
fn millidb_at(rect: Rect, y: f32) -> i32 {
    let fraction = f64::from(((rect.center().y - y) / (rect.height() / 2.0)).clamp(-1.0, 1.0));
    #[allow(clippy::cast_possible_truncation)] // bounded by the display range
    let millidb = (fraction * CURVE_RANGE_DB * 10.0).round() as i32 * 100;
    millidb.clamp(-MAX_EQ_GAIN_MILLIDB, MAX_EQ_GAIN_MILLIDB)
}

/// A frequency rounded to whole hertz within `range`.
fn whole_hz(frequency_hz: f64, (low, high): (u32, u32)) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped below
    let hz = frequency_hz.round().clamp(f64::from(low), f64::from(high)) as u32;
    hz
}

/// The EQ response display with its draggable band and high-pass points.
#[allow(clippy::too_many_lines)] // one painter pass and its handles
fn draw_curve(ui: &mut Ui, chain: &mut StripDsp, (enabled, reason): (bool, &str)) {
    let l = l10n::locale(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), CURVE_HEIGHT),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(4), MONITOR_BLACK);
    let grid = Stroke::new(1.0, border_color().gamma_multiply(0.6));
    let label_font = FontId::new(9.0, FontFamily::Proportional);
    for (hz, label) in [
        (50.0, "50"),
        (100.0, "100"),
        (200.0, "200"),
        (500.0, "500"),
        (1_000.0, "1k"),
        (2_000.0, "2k"),
        (5_000.0, "5k"),
        (10_000.0, "10k"),
    ] {
        let x = x_of(rect, hz);
        painter.vline(x, rect.top()..=rect.bottom(), grid);
        painter.text(
            pos2(x + 2.0, rect.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            label,
            label_font.clone(),
            text_dim_color(),
        );
    }
    for db in [-18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0] {
        let y = y_of(rect, db);
        let stroke = if db == 0.0 {
            Stroke::new(1.0, border_color())
        } else {
            grid
        };
        painter.hline(rect.left()..=rect.right(), y, stroke);
        painter.text(
            pos2(rect.left() + 2.0, y - 1.0),
            Align2::LEFT_BOTTOM,
            if db == 0.0 {
                "0".to_owned()
            } else {
                format!("{db:+}")
            },
            label_font.clone(),
            text_dim_color(),
        );
    }
    let live = !chain.bypass && (chain.eq.enabled || chain.high_pass.enabled);
    let curve_color = if live {
        accent_color()
    } else {
        muted_color().gamma_multiply(0.7)
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a pixel count
    let steps = rect.width().max(2.0) as usize;
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|step| {
            #[allow(clippy::cast_precision_loss)] // a pixel count
            let x = rect.left() + rect.width() * step as f32 / steps as f32;
            let db = response_db(chain, frequency_at(rect, x), CURVE_SAMPLE_RATE_HZ);
            pos2(x, y_of(rect, db))
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.0, curve_color)));

    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let hover = l.text(Msg::FxCurveHover);
    // The high-pass corner rides the 0 dB line.
    let hp_center = pos2(
        x_of(rect, f64::from(chain.high_pass.frequency_hz)),
        y_of(rect, -CURVE_RANGE_DB * 0.75),
    );
    let response = ui
        .interact(
            Rect::from_center_size(hp_center, Vec2::splat(HANDLE_RADIUS * 2.4)),
            ui.id().with("strip-fx-hp"),
            sense,
        )
        .hint(hover, reason);
    if let Some(pointer) = response.interact_pointer_pos()
        && response.dragged()
    {
        chain.high_pass.frequency_hz = whole_hz(
            frequency_at(rect, pointer.x),
            (MIN_HIGH_PASS_HZ, MAX_HIGH_PASS_HZ),
        );
        chain.high_pass.enabled = true;
    }
    paint_handle(
        &painter,
        hp_center,
        "HP",
        text_color(),
        chain.high_pass.enabled,
        response.hovered() || response.dragged(),
    );
    for (index, color) in BAND_COLORS.iter().copied().enumerate() {
        let band = chain.eq.bands[index];
        let center = pos2(
            x_of(rect, f64::from(band.frequency_hz)),
            y_of(rect, f64::from(band.gain_millidb) / 1_000.0),
        );
        let response = ui
            .interact(
                Rect::from_center_size(center, Vec2::splat(HANDLE_RADIUS * 2.4)),
                ui.id().with(("strip-fx-band", index)),
                sense,
            )
            .hint(hover, reason);
        let band = &mut chain.eq.bands[index];
        if let Some(pointer) = response.interact_pointer_pos()
            && response.dragged()
        {
            band.frequency_hz = whole_hz(
                frequency_at(rect, pointer.x),
                (MIN_EQ_FREQUENCY_HZ, MAX_EQ_FREQUENCY_HZ),
            );
            band.gain_millidb = millidb_at(rect, pointer.y);
            band.enabled = true;
            chain.eq.enabled = true;
        }
        if enabled && response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let q = f64::from(band.q_milli) * f64::from((scroll * 0.004).exp());
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
                let q = q
                    .round()
                    .clamp(f64::from(MIN_EQ_Q_MILLI), f64::from(MAX_EQ_Q_MILLI))
                    as u32;
                band.q_milli = q;
                band.enabled = true;
                chain.eq.enabled = true;
            }
        }
        paint_handle(
            &painter,
            center,
            &(index + 1).to_string(),
            color,
            band.enabled && chain.eq.enabled,
            response.hovered() || response.dragged(),
        );
    }
}

fn paint_handle(
    painter: &egui::Painter,
    center: egui::Pos2,
    label: &str,
    color: Color32,
    on: bool,
    hot: bool,
) {
    let fill = if on {
        color.gamma_multiply(0.85)
    } else {
        color.gamma_multiply(0.25)
    };
    painter.circle_filled(center, HANDLE_RADIUS, fill);
    painter.circle_stroke(
        center,
        HANDLE_RADIUS,
        Stroke::new(if hot { 2.0 } else { 1.0 }, color),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        label,
        FontId::new(9.5, FontFamily::Proportional),
        if on { Color32::BLACK } else { text_dim_color() },
    );
}

/// A drag entry over a scaled integer: `value / scale` shown with
/// `places` decimals, `prefix`, and `unit`, typed back the same way.
#[allow(clippy::too_many_arguments)] // one entry's look and bounds
fn scaled_entry<T: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut T,
    range: core::ops::RangeInclusive<T>,
    (scale, places, speed): (f64, usize, f64),
    (prefix, unit): (&'static str, &'static str),
    (enabled, hover, reason): (bool, &str, &str),
) -> bool {
    let l = l10n::locale(ui.ctx());
    ui.add_enabled(
        enabled,
        DragValue::new(value)
            .range(range)
            .speed(speed)
            .custom_formatter(move |value, _| {
                let number = l.decimal(value / scale, places);
                // A ratio reads 3.0:1, a measure 3.0 dB.
                if unit.starts_with(':') {
                    format!("{prefix}{number}{unit}")
                } else {
                    format!("{prefix}{number} {unit}")
                }
            })
            .custom_parser(move |text| {
                l.parse_decimal(
                    text.trim()
                        .trim_start_matches(prefix.trim())
                        .trim()
                        .trim_end_matches(unit)
                        .trim(),
                )
                .map(|number| number * scale)
            }),
    )
    .hint(hover, reason)
    .changed()
}

fn millidb_entry(
    ui: &mut Ui,
    value: &mut i32,
    range: core::ops::RangeInclusive<i32>,
    prefix: &'static str,
    gate: (bool, &str, &str),
) -> bool {
    scaled_entry(ui, value, range, (1_000.0, 1, 100.0), (prefix, "dB"), gate)
}

fn millidb_amount_entry(
    ui: &mut Ui,
    value: &mut u32,
    max: u32,
    prefix: &'static str,
    gate: (bool, &str, &str),
) -> bool {
    scaled_entry(
        ui,
        value,
        0..=max,
        (1_000.0, 1, 100.0),
        (prefix, "dB"),
        gate,
    )
}

fn time_entry(
    ui: &mut Ui,
    value: &mut u32,
    max: u32,
    prefix: &'static str,
    gate: (bool, &str, &str),
) -> bool {
    scaled_entry(
        ui,
        value,
        MIN_TIME_US.min(max)..=max,
        (1_000.0, 1, 100.0),
        (prefix, "ms"),
        gate,
    )
}

/// The polarity invert, the high-pass corner, and each band's switch,
/// shape, frequency, gain, and width.
fn draw_bands(ui: &mut Ui, chain: &mut StripDsp, (enabled, reason): (bool, &str)) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        polarity_key(ui, &mut chain.invert_polarity, (enabled, reason));
        ui.add_space(8.0);
        stage_key(
            ui,
            &mut chain.high_pass.enabled,
            Msg::FxStageHighPass,
            (enabled, reason),
        );
        scaled_entry(
            ui,
            &mut chain.high_pass.frequency_hz,
            MIN_HIGH_PASS_HZ..=MAX_HIGH_PASS_HZ,
            (1.0, 0, 1.0),
            ("", "Hz"),
            (enabled, l.text(Msg::FxHighPassHover), reason),
        );
    });
    ui.add_space(4.0);
    stage_key(ui, &mut chain.eq.enabled, Msg::FxStageEq, (enabled, reason));
    egui::Grid::new("strip-fx-bands")
        .num_columns(5)
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            // Equal entry widths keep the frequency, gain, and Q columns
            // straight whatever their values.
            ui.spacing_mut().interact_size.x = 84.0;
            for (index, color) in BAND_COLORS.iter().copied().enumerate() {
                let band_name = (index + 1).to_string();
                let band = &mut chain.eq.bands[index];
                let mut on = band.enabled;
                if crate::mixer::led_key(ui, enabled, &mut on, &band_name, color, 30.0)
                    .hint(
                        &l.format(Msg::FxBandHover, &[("band", Arg::Str(&band_name))]),
                        reason,
                    )
                    .changed()
                {
                    band.enabled = on;
                }
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt(("strip-fx-kind", index))
                        .selected_text(kind_name(l, band.kind))
                        .width(124.0)
                        .show_ui(ui, |ui| {
                            for kind in [
                                EqBandKind::LowShelf,
                                EqBandKind::Peak,
                                EqBandKind::HighShelf,
                            ] {
                                let name = kind_name(l, kind);
                                ui.selectable_value(&mut band.kind, kind, name)
                                    .on_hover_text(l.format(
                                        Msg::FxKindOptionHover,
                                        &[("kind", Arg::Str(name))],
                                    ));
                            }
                        })
                        .response
                        .hint(
                            &l.format(Msg::FxBandKindHover, &[("band", Arg::Str(&band_name))]),
                            reason,
                        );
                });
                scaled_entry(
                    ui,
                    &mut band.frequency_hz,
                    MIN_EQ_FREQUENCY_HZ..=MAX_EQ_FREQUENCY_HZ,
                    (1.0, 0, 10.0),
                    ("", "Hz"),
                    (enabled, l.text(Msg::FxFrequencyHover), reason),
                );
                millidb_entry(
                    ui,
                    &mut band.gain_millidb,
                    -MAX_EQ_GAIN_MILLIDB..=MAX_EQ_GAIN_MILLIDB,
                    "",
                    (enabled, l.text(Msg::FxBandGainHover), reason),
                );
                scaled_entry(
                    ui,
                    &mut band.q_milli,
                    MIN_EQ_Q_MILLI..=MAX_EQ_Q_MILLI,
                    (1_000.0, 2, 10.0),
                    ("Q ", ""),
                    (enabled, l.text(Msg::FxQHover), reason),
                );
                ui.end_row();
            }
        });
}

/// The polarity invert key, amber while the strip is inverted.
fn polarity_key(ui: &mut Ui, inverted: &mut bool, (enabled, reason): (bool, &str)) {
    let l = l10n::locale(ui.ctx());
    let mut value = *inverted;
    if crate::mixer::led_key(
        ui,
        enabled,
        &mut value,
        l.text(Msg::FxPolarity),
        AMBER,
        96.0,
    )
    .hint(
        l.text(if value {
            Msg::FxPolarityOnHover
        } else {
            Msg::FxPolarityOffHover
        }),
        reason,
    )
    .changed()
    {
        *inverted = value;
    }
}

/// A stage's on/off key.
fn stage_key(ui: &mut Ui, on: &mut bool, stage: Msg, (enabled, reason): (bool, &str)) {
    let l = l10n::locale(ui.ctx());
    let name = l.text(stage);
    let hover = l.format(
        if *on {
            Msg::FxStageOnHover
        } else {
            Msg::FxStageOffHover
        },
        &[("stage", Arg::Str(name))],
    );
    let mut value = *on;
    if crate::mixer::led_key(
        ui,
        enabled,
        &mut value,
        &name.to_uppercase(),
        accent_color(),
        96.0,
    )
    .hint(&hover, reason)
    .changed()
    {
        *on = value;
    }
}

/// A horizontal gain-reduction meter: the reduction grows from the left
/// in amber, red past 12 dB.
/// A gain reduction readout with one decimal: `-3.5 dB`, and `0.0 dB` (not
/// `-0.0 dB`) while the stage reduces less than 0.05 dB.
fn gr_readout(l: Locale, reduction_millidb: u32) -> String {
    let tenths = reduction_millidb.saturating_add(50) / 100;
    let value = l.decimal(f64::from(tenths) / 10.0, 1);
    if tenths == 0 {
        format!("{value} dB")
    } else {
        format!("-{value} dB")
    }
}

/// Room for the `GR` legend at the left of a gain reduction meter.
const GR_LEGEND_WIDTH: f32 = 20.0;

fn gr_meter(ui: &mut Ui, stage: Msg, reduction_millidb: Option<u32>) {
    let l = l10n::locale(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(150.0, 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), MONITOR_BLACK);
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, border_color()),
        StrokeKind::Inside,
    );
    // The legend names the meter, so an idle one is not a bare box.
    painter.text(
        rect.left_center() + Vec2::new(5.0, 0.0),
        Align2::LEFT_CENTER,
        "GR",
        FontId::new(8.5, FontFamily::Proportional),
        muted_color(),
    );
    let stage_name = l.text(stage);
    match reduction_millidb {
        Some(reduction) => {
            #[allow(clippy::cast_precision_loss)] // bounded by the wire
            let db = reduction as f32 / 1_000.0;
            let fraction = (db / GR_FULL_SCALE_DB).clamp(0.0, 1.0);
            // The bar starts after the legend, so the reduction never hides it.
            let bar = Rect::from_min_max(pos2(rect.min.x + GR_LEGEND_WIDTH, rect.min.y), rect.max);
            let fill =
                Rect::from_min_size(bar.min, Vec2::new(bar.width() * fraction, bar.height()));
            painter.rect_filled(
                fill.shrink(1.0),
                CornerRadius::same(1),
                if db > 12.0 { PROGRAM } else { AMBER },
            );
            let readout = gr_readout(l, reduction);
            painter.text(
                rect.right_center() - Vec2::new(4.0, 0.0),
                Align2::RIGHT_CENTER,
                &readout,
                FontId::new(9.5, FontFamily::Monospace),
                text_color(),
            );
            response.on_hover_text(l.format(
                Msg::FxGrHover,
                &[("stage", Arg::Str(stage_name)), ("db", Arg::Str(&readout))],
            ));
        }
        None => {
            response
                .on_hover_text(l.format(Msg::FxGrIdleHover, &[("stage", Arg::Str(stage_name))]));
        }
    }
}

/// The gate, expander, compressor, and limiter rows with their meters.
#[allow(clippy::too_many_lines)] // one row per stage setting
fn draw_dynamics(
    ui: &mut Ui,
    chain: &mut StripDsp,
    (enabled, reason): (bool, &str),
    [gate_gr, expander_gr, compressor_gr, limiter_gr]: [Option<u32>; 4],
) {
    let l = l10n::locale(ui.ctx());
    let gate = |hover: Msg| (enabled, l.text(hover), reason);
    ui.horizontal(|ui| {
        stage_key(
            ui,
            &mut chain.gate.enabled,
            Msg::FxStageGate,
            (enabled, reason),
        );
        gr_meter(ui, Msg::FxStageGate, gate_gr);
    });
    ui.horizontal_wrapped(|ui| {
        millidb_entry(
            ui,
            &mut chain.gate.threshold_millidb,
            MIN_GATE_THRESHOLD_MILLIDB..=0,
            "THR ",
            gate(Msg::FxGateThresholdHover),
        );
        millidb_amount_entry(
            ui,
            &mut chain.gate.range_millidb,
            MAX_GATE_RANGE_MILLIDB,
            "RNG ",
            gate(Msg::FxGateRangeHover),
        );
        time_entry(
            ui,
            &mut chain.gate.attack_us,
            MAX_TIME_US,
            "ATK ",
            gate(Msg::FxAttackHover),
        );
        scaled_entry(
            ui,
            &mut chain.gate.hold_us,
            0..=MAX_GATE_HOLD_US,
            (1_000.0, 0, 100.0),
            ("HOLD ", "ms"),
            gate(Msg::FxHoldHover),
        );
        time_entry(
            ui,
            &mut chain.gate.release_us,
            MAX_GATE_RELEASE_US,
            "REL ",
            gate(Msg::FxReleaseHover),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        stage_key(
            ui,
            &mut chain.expander.enabled,
            Msg::FxStageExpander,
            (enabled, reason),
        );
        gr_meter(ui, Msg::FxStageExpander, expander_gr);
    });
    ui.horizontal_wrapped(|ui| {
        millidb_entry(
            ui,
            &mut chain.expander.threshold_millidb,
            MIN_EXPANDER_THRESHOLD_MILLIDB..=0,
            "THR ",
            gate(Msg::FxExpThresholdHover),
        );
        scaled_entry(
            ui,
            &mut chain.expander.ratio_milli,
            MIN_EXPANDER_RATIO_MILLI..=MAX_EXPANDER_RATIO_MILLI,
            (1_000.0, 1, 50.0),
            ("RATIO ", ":1"),
            gate(Msg::FxExpRatioHover),
        );
        time_entry(
            ui,
            &mut chain.expander.attack_us,
            MAX_TIME_US,
            "ATK ",
            gate(Msg::FxAttackHover),
        );
        time_entry(
            ui,
            &mut chain.expander.release_us,
            MAX_GATE_RELEASE_US,
            "REL ",
            gate(Msg::FxReleaseHover),
        );
        millidb_amount_entry(
            ui,
            &mut chain.expander.range_millidb,
            MAX_EXPANDER_RANGE_MILLIDB,
            "RNG ",
            gate(Msg::FxExpRangeHover),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        stage_key(
            ui,
            &mut chain.compressor.enabled,
            Msg::FxStageCompressor,
            (enabled, reason),
        );
        gr_meter(ui, Msg::FxStageCompressor, compressor_gr);
    });
    ui.horizontal_wrapped(|ui| {
        millidb_entry(
            ui,
            &mut chain.compressor.threshold_millidb,
            MIN_COMPRESSOR_THRESHOLD_MILLIDB..=0,
            "THR ",
            gate(Msg::FxCompThresholdHover),
        );
        scaled_entry(
            ui,
            &mut chain.compressor.ratio_milli,
            MIN_COMPRESSOR_RATIO_MILLI..=MAX_COMPRESSOR_RATIO_MILLI,
            (1_000.0, 1, 50.0),
            ("RATIO ", ":1"),
            gate(Msg::FxRatioHover),
        );
        time_entry(
            ui,
            &mut chain.compressor.attack_us,
            MAX_TIME_US,
            "ATK ",
            gate(Msg::FxAttackHover),
        );
        time_entry(
            ui,
            &mut chain.compressor.release_us,
            MAX_TIME_US,
            "REL ",
            gate(Msg::FxReleaseHover),
        );
        millidb_amount_entry(
            ui,
            &mut chain.compressor.knee_millidb,
            MAX_COMPRESSOR_KNEE_MILLIDB,
            "KNEE ",
            gate(Msg::FxKneeHover),
        );
        millidb_amount_entry(
            ui,
            &mut chain.compressor.makeup_millidb,
            MAX_COMPRESSOR_MAKEUP_MILLIDB,
            "MAKEUP ",
            gate(Msg::FxMakeupHover),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        stage_key(
            ui,
            &mut chain.limiter.enabled,
            Msg::FxStageLimiter,
            (enabled, reason),
        );
        gr_meter(ui, Msg::FxStageLimiter, limiter_gr);
        millidb_entry(
            ui,
            &mut chain.limiter.ceiling_millidb,
            MIN_LIMITER_CEILING_MILLIDB..=0,
            "CEIL ",
            gate(Msg::FxCeilingHover),
        );
        time_entry(
            ui,
            &mut chain.limiter.release_us,
            MAX_TIME_US,
            "REL ",
            gate(Msg::FxReleaseHover),
        );
    });
}

/// The chain's BS.1770 loudness and true peak (AU-005), as `audio_meters`
/// last reported it: one readout box per value, the scale and unit dim
/// beside a bright figure.
fn draw_loudness(ui: &mut Ui, meter: Option<&fm_protocol::AudioLoudnessMeter>) {
    let l = l10n::locale(ui.ctx());
    let row = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
    ui.with_layout(row, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
        ui.label(
            RichText::new(l.text(Msg::FxLoudnessLabel).to_uppercase())
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        )
        .on_hover_text(crate::mixer::loudness_hover(l, meter, false));
        let value = |pick: fn(&fm_protocol::AudioLoudnessMeter) -> Option<i32>| {
            crate::mixer::format_loudness(l, meter.and_then(pick))
        };
        for (scale, text, unit, hover) in [
            (
                "M",
                value(|meter| meter.momentary_millilufs),
                "LUFS",
                Msg::FxLoudnessMomentaryHover,
            ),
            (
                "S",
                value(|meter| meter.short_term_millilufs),
                "LUFS",
                Msg::FxLoudnessShortTermHover,
            ),
            (
                "I",
                value(|meter| meter.integrated_millilufs),
                "LUFS",
                Msg::FxLoudnessIntegratedHover,
            ),
            (
                "TP",
                format!(
                    "{} / {}",
                    value(|meter| meter.true_peak_millidbtp),
                    value(|meter| meter.max_true_peak_millidbtp)
                ),
                "dBTP",
                Msg::FxTruePeakHover,
            ),
        ] {
            loudness_readout(ui, scale, &text, unit).on_hover_text(l.text(hover));
        }
    });
}

/// One loudness readout box: `scale` and `unit` dim around the bright
/// `value`.
fn loudness_readout(ui: &mut Ui, scale: &str, value: &str, unit: &str) -> egui::Response {
    let dim = |text: &str| {
        RichText::new(text)
            .monospace()
            .size(crate::theme::legible(9.5))
            .color(muted_color())
    };
    egui::Frame::new()
        .fill(MONITOR_BLACK)
        .stroke(Stroke::new(1.0, border_color()))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(dim(scale));
                ui.label(
                    RichText::new(value)
                        .monospace()
                        .size(crate::theme::legible(10.5))
                        .color(text_color()),
                );
                ui.label(dim(unit));
            });
        })
        .response
        .interact(Sense::hover())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_response_display_maps_frequency_and_gain_both_ways() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), Vec2::new(500.0, 180.0));
        for hz in [20.0, 100.0, 1_000.0, 20_000.0] {
            let back = frequency_at(rect, x_of(rect, hz));
            assert!((back - hz).abs() / hz < 1e-3, "{hz} -> {back}");
        }
        assert!((x_of(rect, 20.0) - rect.left()).abs() < 1e-3);
        assert!((x_of(rect, 20_000.0) - rect.right()).abs() < 1e-3);
        assert_eq!(millidb_at(rect, y_of(rect, 0.0)), 0);
        assert_eq!(millidb_at(rect, y_of(rect, 6.0)), 6_000);
        assert_eq!(millidb_at(rect, y_of(rect, -12.5)), -12_500);
        assert_eq!(millidb_at(rect, rect.top() - 50.0), 24_000);
        assert_eq!(whole_hz(5.0, (20, 2_000)), 20);
        assert_eq!(whole_hz(99.6, (20, 2_000)), 100);
    }

    fn frame(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
        events: Vec<egui::Event>,
    ) -> Vec<StudioIntent> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                Vec2::new(1600.0, 1000.0),
            )),
            events,
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-strip-fx-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = StudioShell::without_thumbnail_subscription(shell.draw(&mut ui, state));
        let _ = context.end_pass();
        intents
    }

    fn input(value: u128) -> fm_types::InputId {
        fm_types::InputId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    fn strip_sets(intents: &[StudioIntent]) -> Vec<(StripDsp, Option<fm_protocol::GradeGesture>)> {
        intents
            .iter()
            .filter_map(|intent| match intent {
                StudioIntent::AudioMixer(fm_protocol::AudioMixerOperation::SetStripDsp {
                    target,
                    chain,
                    gesture,
                }) => {
                    assert_eq!(
                        *target,
                        fm_protocol::AudioDspTarget::Input(fm_protocol::WireInputId::from_domain(
                            input(1)
                        ))
                    );
                    Some((**chain, *gesture))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_window_sends_edits_and_folds_a_drag_into_one_gesture() {
        let state = crate::hover_audit::bank_fixture();
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let target = StripDspTarget::Input(input(1));
        shell.strip_fx.open(target);
        // Open and idle: nothing to send.
        assert_eq!(
            strip_sets(&frame(&context, &mut shell, &state, Vec::new())),
            Vec::new()
        );
        assert_eq!(shell.strip_fx.target(), Some(target));

        // A click-sized edit (no pointer held) is sent at once, alone.
        let voice = StripDspPreset::Voice.chain();
        shell.strip_fx.binding.as_mut().unwrap().edit(voice);
        let sent = strip_sets(&frame(&context, &mut shell, &state, Vec::new()));
        assert_eq!(sent, vec![(voice, None)]);
        // The draft shows until the authority reports it; nothing repeats.
        assert_eq!(
            strip_sets(&frame(&context, &mut shell, &state, Vec::new())),
            Vec::new()
        );

        // While the pointer is held the edits of one drag share a gesture.
        let press = |pressed| egui::Event::PointerButton {
            pos: pos2(5.0, 990.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut music = StripDspPreset::Music.chain();
        shell.strip_fx.binding.as_mut().unwrap().edit(music);
        let first = strip_sets(&frame(&context, &mut shell, &state, vec![press(true)]));
        let [(chain, Some(gesture))] = first.as_slice() else {
            panic!("{first:?}");
        };
        assert_eq!(*chain, music);
        assert!(!gesture.continues);
        music.compressor.ratio_milli = 9_000;
        shell.strip_fx.binding.as_mut().unwrap().edit(music);
        let released = strip_sets(&frame(&context, &mut shell, &state, vec![press(false)]));
        assert_eq!(
            released,
            vec![(
                music,
                Some(fm_protocol::GradeGesture {
                    token: gesture.token,
                    continues: true
                })
            )]
        );
    }

    #[test]
    fn the_window_closes_when_its_input_leaves_the_show() {
        let mut state = crate::hover_audit::bank_fixture();
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        shell.strip_fx.open(StripDspTarget::Input(input(1)));
        frame(&context, &mut shell, &state, Vec::new());
        assert!(shell.strip_fx.target().is_some());
        if let Some(view) = state.view.as_mut() {
            view.inputs.retain(|candidate| *candidate != input(1));
        }
        frame(&context, &mut shell, &state, Vec::new());
        assert_eq!(shell.strip_fx.target(), None);
    }

    #[test]
    fn opening_another_chain_starts_a_fresh_binding() {
        let input = fm_types::InputId::new(core::num::NonZeroU128::new(2).unwrap());
        let mut panel = StripFxPanel::default();
        assert_eq!(panel.target(), None);
        panel.open(StripDspTarget::Input(input));
        assert_eq!(panel.target(), Some(StripDspTarget::Input(input)));
        panel.open(StripDspTarget::Master);
        assert_eq!(panel.target(), Some(StripDspTarget::Master));
        assert_eq!(panel.clipboard(), None);
    }
}
