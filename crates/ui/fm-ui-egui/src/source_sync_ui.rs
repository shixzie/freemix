//! Live source clock drift correction in the console (`AU-004`).
//!
//! A daemon that ingests live sources (capture inputs, network streams,
//! system audio) reports each one's drift-correcting loop as
//! `source_sync_status` (Protocol 2.76). The console shows it twice:
//!
//! - a `SYNC` lamp in the status bar's health strip: the largest drift, or
//!   the number of resyncs, underruns, and overruns once any source has had
//!   one, amber while a source primes or recovers;
//! - a `SYNC` key on the mixer strip of every input with a loop: its drift,
//!   with the ratio, buffer, and recovery counts one click away.

use egui::{Button, RichText, Ui, Vec2};
use fm_protocol::{SourceClockState, SourceSyncKind, SourceSyncSample, WireInputId};
use fm_types::InputId;

use crate::StudioUiState;
use crate::health::HealthItem;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{Tone, legible};

/// Height of the strip row the `SYNC` key takes (an empty row on strips
/// without a loop, so the faders stay level).
pub(crate) const SYNC_ROW_HEIGHT: f32 = 20.0;

/// The latest sample of `input`'s loop, if the daemon reports one.
pub(crate) fn sample_for(state: &StudioUiState, input: InputId) -> Option<&SourceSyncSample> {
    let wire = WireInputId::from_domain(input);
    state
        .source_sync
        .as_ref()?
        .sources
        .iter()
        .find(|sample| sample.input == wire)
}

/// Whether any mixer strip has a loop to show.
pub(crate) fn any_strip(state: &StudioUiState) -> bool {
    state.view.as_ref().is_some_and(|view| {
        view.input_audio_strips
            .iter()
            .any(|strip| sample_for(state, strip.input).is_some())
    })
}

/// Green while running clean; amber while priming or recovering, or once
/// the source has had a resync, underrun, or overrun.
pub(crate) const fn tone(sample: &SourceSyncSample) -> Tone {
    match sample.state {
        SourceClockState::Running if sample.faults() == 0 => Tone::Ready,
        _ => Tone::Pending,
    }
}

const fn kind_word(kind: SourceSyncKind) -> Msg {
    match kind {
        SourceSyncKind::Capture => Msg::SyncKindCapture,
        SourceSyncKind::Stream => Msg::SyncKindStream,
        SourceSyncKind::SystemAudio => Msg::SyncKindSystemAudio,
        SourceSyncKind::ScreenCapture => Msg::SyncKindScreenCapture,
    }
}

const fn state_word(state: SourceClockState) -> Msg {
    match state {
        SourceClockState::Priming => Msg::SyncStatePriming,
        SourceClockState::Running => Msg::SyncStateRunning,
        SourceClockState::Recovering => Msg::SyncStateRecovering,
    }
}

/// A signed ppm value with one decimal: `+12.3`.
pub(crate) fn ppm(l: Locale, value: f64) -> String {
    l.signed_decimal(value, 1)
}

/// The resampling ratio with six decimals: `0.918762`.
pub(crate) fn ratio(l: Locale, sample: &SourceSyncSample) -> String {
    l.decimal(sample.resampling_ratio(), 6)
}

fn milliseconds(l: Locale, value: f64) -> String {
    l.decimal(value, 1)
}

/// The badge text: `SYNC +12.3`.
pub(crate) fn badge_text(l: Locale, sample: &SourceSyncSample) -> String {
    l.format(
        Msg::HealthSync,
        &[("drift", Arg::Str(&ppm(l, sample.drift_ppm())))],
    )
}

/// One line per detail: drift and correction, ratio, buffer, recoveries,
/// and pictures.
pub(crate) fn detail_lines(l: Locale, sample: &SourceSyncSample) -> [String; 5] {
    [
        l.format(
            Msg::SyncDetailDrift,
            &[
                ("drift", Arg::Str(&ppm(l, sample.drift_ppm()))),
                ("correction", Arg::Str(&ppm(l, sample.correction_ppm()))),
            ],
        ),
        l.format(
            Msg::SyncDetailRatio,
            &[
                ("ratio", Arg::Str(&ratio(l, sample))),
                ("input", Arg::Count(u64::from(sample.input_rate_hz))),
                ("output", Arg::Count(u64::from(sample.output_rate_hz))),
            ],
        ),
        l.format(
            Msg::SyncDetailBuffer,
            &[
                ("buffer", Arg::Str(&milliseconds(l, sample.buffer_ms()))),
                ("target", Arg::Str(&milliseconds(l, sample.target_ms()))),
            ],
        ),
        l.format(
            Msg::SyncDetailFaults,
            &[
                ("resyncs", Arg::Count(sample.resyncs)),
                ("underruns", Arg::Count(sample.underruns)),
                ("overruns", Arg::Count(sample.overruns)),
            ],
        ),
        l.format(
            Msg::SyncDetailPictures,
            &[
                ("drops", Arg::Count(sample.picture_drops)),
                ("repeats", Arg::Count(sample.picture_repeats)),
                ("restarts", Arg::Count(sample.restarts)),
            ],
        ),
    ]
}

/// The badge's help: what the loop is, for which source, and the click.
pub(crate) fn badge_hover(l: Locale, sample: &SourceSyncSample, name: &str) -> String {
    l.format(
        Msg::SyncBadgeHover,
        &[
            ("name", Arg::Str(name)),
            ("kind", Arg::Str(l.text(kind_word(sample.kind)))),
            ("state", Arg::Str(l.text(state_word(sample.state)))),
        ],
    )
}

/// One source's summary line.
pub(crate) fn source_line(l: Locale, sample: &SourceSyncSample, name: &str) -> String {
    l.format(
        Msg::SyncSourceLine,
        &[
            ("name", Arg::Str(name)),
            ("drift", Arg::Str(&ppm(l, sample.drift_ppm()))),
            ("correction", Arg::Str(&ppm(l, sample.correction_ppm()))),
            ("ratio", Arg::Str(&ratio(l, sample))),
            ("buffer", Arg::Str(&milliseconds(l, sample.buffer_ms()))),
            ("target", Arg::Str(&milliseconds(l, sample.target_ms()))),
        ],
    )
}

fn source_name(l: Locale, state: &StudioUiState, input: WireInputId) -> String {
    let input = input.to_domain();
    state.view.as_ref().map_or_else(
        || crate::monitors::fallback_input_name(l, input),
        |view| crate::monitors::label_for_input(l, view, input),
    )
}

/// The health strip's `SYNC` lamp, when the daemon reports any live source.
/// It names the largest drift, or the fault count once there is one; its
/// help adds the source with the largest drift.
pub(crate) fn health_item(l: Locale, state: &StudioUiState) -> Option<HealthItem> {
    let sources = &state.source_sync.as_ref()?.sources;
    let worst = sources
        .iter()
        .max_by_key(|sample| sample.drift_mppm.unsigned_abs())?;
    let faults = sources
        .iter()
        .map(SourceSyncSample::faults)
        .fold(0, u64::saturating_add);
    let drift = ppm(l, worst.drift_ppm());
    let tone = if sources.iter().all(|sample| tone(sample) == Tone::Ready) {
        Tone::Ready
    } else {
        Tone::Pending
    };
    let label = if faults > 0 {
        l.format(Msg::HealthSyncFaults, &[("count", Arg::Count(faults))])
    } else {
        l.format(Msg::HealthSync, &[("drift", Arg::Str(&drift))])
    };
    let summary = l.format(
        Msg::HealthSyncHover,
        &[
            ("sources", Arg::from(sources.len())),
            ("drift", Arg::Str(&drift)),
            ("faults", Arg::Count(faults)),
        ],
    );
    let worst_line = source_line(l, worst, &source_name(l, state, worst.input));
    Some(HealthItem {
        label,
        tone,
        hover: format!("{summary}\n{worst_line}"),
    })
}

/// The strip's `SYNC` key: the drift in the loop's tone; a click opens the
/// ratio, buffer, and recovery counts.
pub(crate) fn strip_badge(ui: &mut Ui, sample: &SourceSyncSample, name: &str, width: f32) {
    let l = l10n::locale(ui.ctx());
    let response = ui
        .add(
            Button::new(
                RichText::new(badge_text(l, sample))
                    .size(legible(9.0))
                    .monospace()
                    .color(tone(sample).color()),
            )
            .min_size(Vec2::new(width, SYNC_ROW_HEIGHT - 4.0)),
        )
        .on_hover_text(badge_hover(l, sample, name));
    // Each detail stays on one line: the popup grows to the longest, so a
    // count never wraps away from its label.
    egui::Popup::menu(&response).show(|ui| {
        ui.label(RichText::new(name).strong());
        ui.separator();
        for line in detail_lines(l, sample) {
            ui.add(egui::Label::new(RichText::new(line).size(legible(10.0))).extend());
        }
    });
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_ui_model::i18n::Language;

    use super::*;
    use crate::StudioConnectionStatus;

    fn sample(id: u128) -> SourceSyncSample {
        SourceSyncSample {
            input: WireInputId::new(NonZeroU128::new(id).unwrap()),
            kind: SourceSyncKind::Stream,
            state: SourceClockState::Running,
            input_rate_hz: 44_100,
            output_rate_hz: 48_000,
            drift_mppm: -12_345,
            correction_mppm: 12_001,
            buffer_us: 81_250,
            target_us: 80_000,
            resyncs: 0,
            underruns: 0,
            overruns: 0,
            picture_drops: 4,
            picture_repeats: 0,
            restarts: 1,
        }
    }

    fn state(sources: Vec<SourceSyncSample>) -> StudioUiState {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.source_sync = Some(fm_protocol::SourceSyncStatusMessage {
            server: fm_protocol::ServerIdentity {
                engine_id: "engine".into(),
                project_id: "project".into(),
                state_epoch: 1,
                log_id: "log".into(),
            },
            sequence: 1,
            sources,
        });
        state
    }

    #[test]
    fn details_read_in_either_language() {
        let en = Locale::new(Language::English);
        let es = Locale::new(Language::Spanish);
        let sample = sample(7);
        assert_eq!(badge_text(en, &sample), "SYNC -12.3");
        assert_eq!(badge_text(es, &sample), "SINC -12,3");
        assert_eq!(
            detail_lines(en, &sample),
            [
                "Drift -12.3 ppm, correction +12.0 ppm".to_owned(),
                "Resampling ratio 0.918761 (44,100 Hz into 48,000 Hz)".to_owned(),
                "Buffer 81.2 ms, target 80.0 ms".to_owned(),
                "0 resyncs · 0 underruns · 0 overruns".to_owned(),
                "4 pictures dropped · 0 repeated · reconnections: 1".to_owned(),
            ]
        );
        assert!(detail_lines(es, &sample)[1].starts_with("Relación de remuestreo 0,918761"));
        assert!(badge_hover(es, &sample, "Cámara").contains("Cámara (emisión de red, en marcha)"));
    }

    #[test]
    fn the_health_lamp_names_the_largest_drift_then_the_faults() {
        let en = Locale::new(Language::English);
        assert!(health_item(en, &StudioUiState::new(StudioConnectionStatus::Ready)).is_none());
        assert!(health_item(en, &state(Vec::new())).is_none());
        let mut small = sample(3);
        small.drift_mppm = 2_000;
        let item = health_item(en, &state(vec![small, sample(7)])).unwrap();
        assert_eq!(item.label, "SYNC -12.3");
        assert_eq!(item.tone, Tone::Ready);
        assert!(item.hover.contains("2 drift-corrected"), "{}", item.hover);
        assert!(
            item.hover.contains("\nInput 7: drift -12.3 ppm"),
            "{}",
            item.hover
        );
        let mut faulted = sample(7);
        faulted.underruns = 2;
        faulted.state = SourceClockState::Recovering;
        let item = health_item(en, &state(vec![small, faulted])).unwrap();
        assert_eq!(item.label, "SYNC 2 FAULTS");
        assert_eq!(item.tone, Tone::Pending);
        let mut priming = sample(7);
        priming.state = SourceClockState::Priming;
        assert_eq!(tone(&priming), Tone::Pending);
    }
}
