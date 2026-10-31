//! The meter values one taken audio interval exposes, converted once for
//! both daemons' wire records.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// The meter values one taken interval exposes, from either daemon's audio
/// plane, so the native and simulated paths share one wire conversion.
pub(crate) struct AudioMeterView<'a> {
    frame: u64,
    start_sample: u64,
    end_sample: u64,
    channels: usize,
    master: &'a [fm_audio::ChannelMeter],
    inputs: &'a [InputId],
    input_meters: &'a [fm_audio::ChannelMeter],
    monitor: &'a [fm_audio::ChannelMeter],
    monitor_solo: bool,
    dynamics: &'a [(Option<InputId>, fm_audio::DspDynamicsReading)],
    loudness: &'a [(Option<InputId>, fm_audio::LoudnessReading)],
    aux: &'a [fm_audio::ChannelMeter],
}

#[cfg(feature = "native-media")]
impl<'a> From<&'a native_audio_realizer::OwnedAudioMeters> for AudioMeterView<'a> {
    fn from(meters: &'a native_audio_realizer::OwnedAudioMeters) -> Self {
        Self {
            frame: meters.frame,
            start_sample: meters.start_sample,
            end_sample: meters.end_sample,
            channels: meters.channels,
            master: &meters.master,
            inputs: &meters.inputs,
            input_meters: &meters.input_meters,
            monitor: &meters.monitor,
            monitor_solo: meters.monitor_solo,
            dynamics: &meters.dynamics,
            loudness: &meters.loudness,
            aux: &meters.aux,
        }
    }
}

#[cfg(feature = "native-media")]
impl<'a, 'b: 'a> From<&'a NativeAudioMeters<'b>> for AudioMeterView<'a> {
    fn from(meters: &'a NativeAudioMeters<'b>) -> Self {
        Self {
            frame: meters.frame,
            start_sample: meters.start_sample,
            end_sample: meters.end_sample,
            channels: meters.channels,
            master: meters.master,
            inputs: meters.inputs,
            input_meters: meters.input_meters,
            monitor: meters.monitor,
            monitor_solo: meters.monitor_solo,
            dynamics: meters.dynamics,
            loudness: meters.loudness,
            aux: meters.aux,
        }
    }
}

impl<'a, 'b: 'a> From<&'a fm_sim::SimulatedAudioMeters<'b>> for AudioMeterView<'a> {
    fn from(meters: &'a fm_sim::SimulatedAudioMeters<'b>) -> Self {
        Self {
            frame: meters.frame,
            start_sample: meters.start_sample,
            end_sample: meters.end_sample,
            channels: meters.channels,
            master: meters.master,
            inputs: meters.inputs,
            input_meters: meters.input_meters,
            monitor: meters.monitor,
            monitor_solo: meters.monitor_solo,
            dynamics: meters.dynamics,
            loudness: meters.loudness,
            aux: &[],
        }
    }
}

pub(crate) fn audio_meters_message(
    server: &ServerIdentity,
    sequence: u64,
    meters: &AudioMeterView<'_>,
    output: fm_protocol::AudioMonitorOutputState,
) -> AudioMetersMessage {
    AudioMetersMessage {
        server: server.clone(),
        sequence,
        frame: meters.frame,
        start_sample: meters.start_sample,
        end_sample: meters.end_sample,
        master: meters
            .master
            .iter()
            .copied()
            .map(protocol_audio_meter)
            .collect(),
        inputs: meters
            .inputs
            .iter()
            .zip(meters.input_meters.chunks_exact(meters.channels))
            .map(|(&input, channels)| InputAudioMeters {
                input: WireInputId::from_domain(input),
                channels: channels.iter().copied().map(protocol_audio_meter).collect(),
            })
            .collect(),
        monitor: Some(fm_protocol::AudioMonitorMeters {
            solo: meters.monitor_solo,
            output,
            channels: meters
                .monitor
                .iter()
                .copied()
                .map(protocol_audio_meter)
                .collect(),
        }),
        dynamics: meters
            .dynamics
            .iter()
            .map(|&(input, reading)| fm_protocol::AudioDynamicsMeter {
                target: input.map_or(fm_protocol::AudioDspTarget::Master, |input| {
                    fm_protocol::AudioDspTarget::Input(WireInputId::from_domain(input))
                }),
                gate_millidb: reading.gate_db.map(reduction_millidb),
                expander_millidb: reading.expander_db.map(reduction_millidb),
                compressor_millidb: reading.compressor_db.map(reduction_millidb),
                limiter_millidb: reading.limiter_db.map(reduction_millidb),
            })
            .collect(),
        aux: meters
            .aux
            .chunks_exact(meters.channels)
            .enumerate()
            .filter_map(|(index, channels)| {
                Some(fm_protocol::AuxBusMeters {
                    bus: fm_types::audio_bus::AuxBusNumber::new(
                        u8::try_from(index.checked_add(1)?).ok()?,
                    )?,
                    channels: channels.iter().copied().map(protocol_audio_meter).collect(),
                })
            })
            .collect(),
        loudness: meters
            .loudness
            .iter()
            .map(|&(input, reading)| fm_protocol::AudioLoudnessMeter {
                target: input.map_or(fm_protocol::AudioDspTarget::Master, |input| {
                    fm_protocol::AudioDspTarget::Input(WireInputId::from_domain(input))
                }),
                momentary_millilufs: reading.momentary_lufs.and_then(loudness_milli),
                short_term_millilufs: reading.short_term_lufs.and_then(loudness_milli),
                integrated_millilufs: reading.integrated_lufs.and_then(loudness_milli),
                true_peak_millidbtp: reading.true_peak_dbtp.and_then(loudness_milli),
                max_true_peak_millidbtp: reading.max_true_peak_dbtp.and_then(loudness_milli),
                loudness_range_millilu: reading.loudness_range_lu.map(loudness_range_millilu),
            })
            .filter(|meter| {
                meter.values().iter().any(Option::is_some) || meter.loudness_range_millilu.is_some()
            })
            .collect(),
    }
}

/// A loudness (LUFS) or true-peak (dBTP) value in thousandths for the
/// wire: `None` below the wire floor, clamped at its ceiling (AU-005).
// Values are clamped to the wire bounds before the cast; truncation is not
// reachable.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn loudness_milli(value: f64) -> Option<i32> {
    let milli = (value * 1_000.0).round();
    (milli.is_finite() && milli >= f64::from(fm_protocol::MIN_LOUDNESS_MILLI))
        .then(|| milli.min(f64::from(fm_protocol::MAX_LOUDNESS_MILLI)) as i32)
}

/// A loudness range in milli-LU for the wire, clamped to its bounds
/// (AU-005).
// Values are clamped to the wire bounds before the cast; truncation is not
// reachable.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn loudness_range_millilu(lu: f64) -> i32 {
    let milli = (lu * 1_000.0).round();
    if milli.is_finite() {
        milli.clamp(0.0, f64::from(fm_protocol::MAX_LOUDNESS_RANGE_MILLILU)) as i32
    } else {
        0
    }
}

/// A DSP stage's gain reduction in positive millidecibels, bounded for the
/// wire (AU-005).
// Reductions are clamped to [0, MAX_DYNAMICS_REDUCTION_MILLIDB] before the
// cast; truncation is not reachable.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn reduction_millidb(db: f32) -> u32 {
    (f64::from(db.max(0.0)) * 1_000.0)
        .round()
        .min(f64::from(fm_protocol::MAX_DYNAMICS_REDUCTION_MILLIDB)) as u32
}

pub(crate) fn protocol_audio_meter(meter: fm_audio::ChannelMeter) -> AudioMeterChannel {
    let peak_millionths = meter_level_millionths(meter.peak);
    AudioMeterChannel {
        peak_millionths,
        rms_millionths: meter_level_millionths(meter.rms).min(peak_millionths),
    }
}

// Levels are clamped to [0, u32::MAX] before the cast; truncation is not reachable.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn meter_level_millionths(level: f32) -> u32 {
    (f64::from(level.max(0.0)) * f64::from(AUDIO_METER_LEVEL_SCALE))
        .round()
        .min(f64::from(u32::MAX)) as u32
}
