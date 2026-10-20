//! Screenshot aid: a synthetic audio meter feed for the simulated daemon.
//!
//! Compiled only with the `capture-meter-signal` feature and active only
//! when `FREEMIX_STUDIO_CAPTURE_METERS=1` is set, so neither a normal build
//! nor the test suite ever shows levels the daemon did not send. The
//! simulated daemon publishes no meters; documentation captures use this to
//! show the meter bridge lit. The signal is a deterministic function of the
//! time since first use: each input strip sits at its own level with slow
//! movement, input 4 peaks into 0 dBFS every few seconds (latching its clip
//! lamp), and Master follows the loudest strip. Each strip and Master also
//! reads a loudness and true peak near its level, and a chain with a running
//! dynamics stage reports some gain reduction.

use std::sync::OnceLock;
use std::time::Instant;

use fm_protocol::{
    AudioDspTarget, AudioDynamicsMeter, AudioLoudnessMeter, AudioMeterChannel, AudioMetersMessage,
    InputAudioMeters, ServerIdentity, WireInputId,
};
use fm_types::strip_dsp::{StripDsp, StripDspTarget};
use fm_ui_egui::StudioUiState;

/// The environment switch for the synthetic feed.
pub const CAPTURE_METERS_ENV: &str = "FREEMIX_STUDIO_CAPTURE_METERS";

/// Engine id marking a synthetic message.
const CAPTURE_ENGINE: &str = "studio-capture-signal";

/// Meter message cadence of the synthetic feed, in milliseconds.
const MESSAGE_INTERVAL_MS: u128 = 50;

/// Resting RMS level per strip position, in dBFS.
const BASE_RMS_DB: [f64; 7] = [-21.0, -15.0, -30.0, -11.0, -26.0, -42.0, -17.0];

fn start() -> Option<Instant> {
    static START: OnceLock<Option<Instant>> = OnceLock::new();
    *START.get_or_init(|| {
        (std::env::var(CAPTURE_METERS_ENV).as_deref() == Ok("1")).then(Instant::now)
    })
}

fn linear(db: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=1e6
    let millionths = (10_f64.powf(db.min(0.0) / 20.0) * 1_000_000.0).round() as u32;
    millionths
}

fn channel(rms_db: f64, peak_db: f64) -> AudioMeterChannel {
    AudioMeterChannel {
        peak_millionths: linear(peak_db),
        rms_millionths: linear(rms_db),
    }
}

/// The synthetic (RMS, peak) of strip `index` at `seconds`, per channel.
#[allow(clippy::cast_precision_loss)] // small indices
fn strip_levels(index: usize, seconds: f64) -> [(f64, f64); 2] {
    let base = BASE_RMS_DB[index % BASE_RMS_DB.len()];
    let phase = index as f64 * 1.7;
    let movement = 3.5 * (seconds * 0.9 + phase).sin() + 1.5 * (seconds * 2.3 + phase).sin();
    let crest = 7.0 + 2.0 * (seconds * 1.3 + phase).sin().abs();
    let mut levels = [0, 1].map(|side| {
        let rms = base + movement - f64::from(side) * 1.2;
        (rms, rms + crest)
    });
    // Strip 4 hits full scale for a moment every six seconds.
    if index == 3 && seconds % 6.0 < 0.15 {
        levels[0].1 = 0.0;
    }
    levels
}

/// Milli-units of a decibel or LUFS value.
#[allow(clippy::cast_possible_truncation)] // within the loudness range
fn milli(value: f64) -> i32 {
    (value * 1000.0).round() as i32
}

/// A loudness reading for a bus resting at `base` dBFS RMS and now at
/// `rms`, with the interval's `peak`.
fn loudness(target: AudioDspTarget, base: f64, rms: f64, peak: f64) -> AudioLoudnessMeter {
    AudioLoudnessMeter {
        target,
        momentary_millilufs: Some(milli(rms + 1.0)),
        short_term_millilufs: Some(milli(rms.midpoint(base) + 1.0)),
        integrated_millilufs: Some(milli(base + 1.5)),
        true_peak_millidbtp: Some(milli((peak + 0.3).min(1.0))),
        max_true_peak_millidbtp: Some(milli((peak + 0.3).max(base + 12.0).min(1.0))),
        loudness_range_millilu: Some(milli((rms - base).abs().clamp(4.0, 20.0))),
    }
}

/// The gain reduction of `dsp`'s running dynamics stages at `rms` dBFS, or
/// `None` while it has none (or is bypassed).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=40 dB
fn dynamics(target: AudioDspTarget, dsp: &StripDsp, rms: f64) -> Option<AudioDynamicsMeter> {
    let reduction = |enabled: bool, db: f64| enabled.then(|| (db.clamp(0.0, 40.0) * 1000.0) as u32);
    let meter = AudioDynamicsMeter {
        target,
        gate_millidb: reduction(dsp.gate.enabled, 0.0),
        expander_millidb: reduction(dsp.expander.enabled, (-rms).midpoint(-30.0)),
        compressor_millidb: reduction(dsp.compressor.enabled, (rms + 24.0) * 0.6),
        limiter_millidb: reduction(dsp.limiter.enabled, rms.midpoint(8.0)),
    };
    let running = meter.gate_millidb.is_some()
        || meter.expander_millidb.is_some()
        || meter.compressor_millidb.is_some()
        || meter.limiter_millidb.is_some();
    (!dsp.bypass && running).then_some(meter)
}

/// Fills `state.audio_meters` with the synthetic feed when it is switched on
/// and the daemon sent no meters for a synchronized view.
pub fn inject(state: &mut StudioUiState) {
    let Some(start) = start() else {
        return;
    };
    // Daemon meters always win; a feed this module produced is refreshed.
    if state
        .audio_meters
        .as_ref()
        .is_some_and(|meters| meters.server.engine_id != CAPTURE_ENGINE)
    {
        return;
    }
    let Some(view) = &state.view else {
        return;
    };
    let elapsed = start.elapsed();
    let seconds = elapsed.as_secs_f64();
    let sequence = u64::try_from(elapsed.as_millis() / MESSAGE_INTERVAL_MS).unwrap_or(u64::MAX);
    let inputs: Vec<InputAudioMeters> = view
        .input_audio_strips
        .iter()
        .enumerate()
        .map(|(index, strip)| InputAudioMeters {
            input: WireInputId::from_domain(strip.input),
            channels: strip_levels(index, seconds)
                .iter()
                .map(|(rms, peak)| channel(*rms, *peak))
                .collect(),
        })
        .collect();
    let loudest = (0..view.input_audio_strips.len())
        .map(|index| strip_levels(index, seconds)[0].0)
        .fold(-60.0_f64, f64::max);
    let master_rms = (loudest + 2.0).min(-6.0);
    let master_peak = (master_rms + 8.0).min(-1.0);
    let mut loudness_meters = Vec::new();
    let mut dynamics_meters = Vec::new();
    for (index, strip) in view.input_audio_strips.iter().enumerate() {
        let target = AudioDspTarget::Input(WireInputId::from_domain(strip.input));
        let [(rms, peak), _] = strip_levels(index, seconds);
        let base = BASE_RMS_DB[index % BASE_RMS_DB.len()];
        loudness_meters.push(loudness(target, base, rms, peak));
        if let Some(project) = &state.project_state {
            let dsp = project.audio_dsp(StripDspTarget::Input(strip.input));
            dynamics_meters.extend(dynamics(target, &dsp, rms));
        }
    }
    loudness_meters.push(loudness(
        AudioDspTarget::Master,
        -16.0,
        master_rms,
        master_peak,
    ));
    if let Some(project) = &state.project_state {
        let dsp = project.audio_dsp(StripDspTarget::Master);
        dynamics_meters.extend(dynamics(AudioDspTarget::Master, &dsp, master_rms));
    }
    state.audio_meters = Some(AudioMetersMessage {
        server: ServerIdentity {
            engine_id: CAPTURE_ENGINE.to_owned(),
            project_id: "capture".to_owned(),
            state_epoch: 0,
            log_id: "capture".to_owned(),
        },
        sequence,
        frame: sequence,
        start_sample: 0,
        end_sample: 0,
        master: vec![
            channel(master_rms, master_peak),
            channel(master_rms - 1.0, (master_rms + 7.0).min(-1.5)),
        ],
        inputs,
        monitor: None,
        dynamics: dynamics_meters,
        loudness: loudness_meters,
    });
}
