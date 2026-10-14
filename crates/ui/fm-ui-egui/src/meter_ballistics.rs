//! Pure meter ballistics: peak-hold markers that decay and latched clip lamps.
//!
//! The state advances only by the frame time passed in (`dt_ms`); it never
//! reads a clock. Levels are integer milli-dBFS so the shell stays `Eq`.

use fm_protocol::AudioMeterChannel;
use fm_types::InputId;

/// How long a new peak is held before it starts to fall.
pub(crate) const PEAK_HOLD_MS: u32 = 1_500;
/// Fall rate of a released peak-hold marker in milli-dB per millisecond
/// (20 dB/s).
pub(crate) const PEAK_DECAY_MDB_PER_MS: i32 = 20;
/// How long a clip lamp stays lit after the last clipping sample.
pub(crate) const CLIP_HOLD_MS: u32 = 3_000;
/// Peaks at or above this level (milli-dBFS) light the clip lamp.
pub(crate) const CLIP_THRESHOLD_MDB: i32 = -100;
/// Floor below which a held marker is considered gone (matches the meter
/// scale floor of -60 dBFS).
pub(crate) const FLOOR_MDB: i32 = -60_000;
/// Longest frame step applied at once, so a stalled frame cannot skip the
/// whole hold window in one jump.
const MAX_STEP_MS: u32 = 250;

/// Converts a linear meter value in millionths of full scale to milli-dBFS,
/// clamped to the meter floor.
pub(crate) fn millionths_to_mdb(millionths: u32) -> i32 {
    if millionths == 0 {
        return FLOOR_MDB;
    }
    let db = 20.0 * (f64::from(millionths) / 1_000_000.0).log10();
    #[allow(clippy::cast_possible_truncation)] // clamped to a small range first
    let mdb = (db * 1_000.0).round().clamp(f64::from(FLOOR_MDB), 24_000.0) as i32;
    mdb
}

/// Ballistic state of one meter channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChannelBallistics {
    /// Held peak level in milli-dBFS.
    pub(crate) held_mdb: i32,
    /// Remaining hold time before the marker starts to fall.
    pub(crate) hold_remaining_ms: u32,
    /// Remaining time the clip lamp stays lit.
    pub(crate) clip_remaining_ms: u32,
    /// Latest observed peak; a released marker never falls below it.
    pub(crate) live_mdb: i32,
}

impl Default for ChannelBallistics {
    fn default() -> Self {
        Self {
            held_mdb: FLOOR_MDB,
            hold_remaining_ms: 0,
            clip_remaining_ms: 0,
            live_mdb: FLOOR_MDB,
        }
    }
}

impl ChannelBallistics {
    /// Advances by `dt_ms` and folds in a newly received peak sample, if any.
    ///
    /// Without a new sample the state only ages: the clip lamp is not re-armed
    /// by a repeated stale sample.
    pub(crate) fn step(&mut self, sample_mdb: Option<i32>, dt_ms: u32) {
        let dt_ms = dt_ms.min(MAX_STEP_MS);
        // Age the existing hold first, then let a louder sample capture it.
        if self.hold_remaining_ms >= dt_ms {
            self.hold_remaining_ms -= dt_ms;
        } else {
            let falling_ms = dt_ms - self.hold_remaining_ms;
            self.hold_remaining_ms = 0;
            let fall =
                PEAK_DECAY_MDB_PER_MS.saturating_mul(i32::try_from(falling_ms).unwrap_or(i32::MAX));
            self.held_mdb = self.held_mdb.saturating_sub(fall).max(FLOOR_MDB);
        }
        self.clip_remaining_ms = self.clip_remaining_ms.saturating_sub(dt_ms);
        if let Some(peak_mdb) = sample_mdb {
            self.live_mdb = peak_mdb;
            if peak_mdb >= self.held_mdb {
                self.held_mdb = peak_mdb;
                self.hold_remaining_ms = PEAK_HOLD_MS;
            }
            if peak_mdb >= CLIP_THRESHOLD_MDB {
                self.clip_remaining_ms = CLIP_HOLD_MS;
            }
        }
        self.held_mdb = self.held_mdb.max(self.live_mdb);
    }

    /// Whether the clip lamp is lit.
    pub(crate) const fn clipping(self) -> bool {
        self.clip_remaining_ms > 0
    }

    /// Whether the marker or lamp still changes over time without new input.
    pub(crate) const fn animating(self) -> bool {
        self.clip_remaining_ms > 0 || self.held_mdb > self.live_mdb
    }
}

/// Which meter a ballistic state belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MeterKey {
    Input(InputId),
    Master,
}

/// Ballistic state for every visible meter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MeterBallistics {
    meters: Vec<(MeterKey, Vec<ChannelBallistics>)>,
    /// Sequence of the meter message whose peaks were last folded in.
    last_sequence: Option<u64>,
    /// Whether the current frame carries a meter message not yet folded in.
    fresh: bool,
}

impl MeterBallistics {
    /// Starts a frame with the sequence of the latest meter message, if any.
    pub(crate) fn begin_frame(&mut self, sequence: Option<u64>) {
        self.fresh = sequence.is_some() && sequence != self.last_sequence;
        if sequence.is_some() {
            self.last_sequence = sequence;
        }
    }

    /// Advances one meter by `dt_ms` with its current channel samples.
    ///
    /// With no sample the meter's ballistic state is dropped so an unlit meter
    /// never shows a stale held peak or clip lamp.
    pub(crate) fn update(
        &mut self,
        key: MeterKey,
        channels: Option<&[AudioMeterChannel]>,
        dt_ms: u32,
    ) -> &[ChannelBallistics] {
        let Some(channels) = channels else {
            self.meters.retain(|(existing, _)| *existing != key);
            return &[];
        };
        let index = if let Some(index) = self
            .meters
            .iter()
            .position(|(existing, _)| *existing == key)
        {
            index
        } else {
            self.meters.push((key, Vec::new()));
            self.meters.len() - 1
        };
        let states = &mut self.meters[index].1;
        states.resize(channels.len(), ChannelBallistics::default());
        let fresh = self.fresh;
        for (state, channel) in states.iter_mut().zip(channels) {
            state.step(
                fresh.then(|| millionths_to_mdb(channel.peak_millionths)),
                dt_ms,
            );
        }
        &self.meters[index].1
    }

    /// Drops meters whose keys are no longer shown.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(MeterKey) -> bool) {
        self.meters.retain(|(key, _)| keep(*key));
    }

    /// Whether any meter needs further repaints to finish its animation.
    pub(crate) fn animating(&self) -> bool {
        self.meters
            .iter()
            .any(|(_, states)| states.iter().any(|state| state.animating()))
    }
}

/// Converts an `egui` frame time in seconds to whole milliseconds.
pub(crate) fn frame_dt_ms(stable_dt_seconds: f32) -> u32 {
    if !stable_dt_seconds.is_finite() || stable_dt_seconds <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
    let ms = (stable_dt_seconds * 1_000.0)
        .round()
        .min(f32::from(u16::MAX)) as u32;
    ms
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn channel(peak: u32) -> AudioMeterChannel {
        AudioMeterChannel {
            peak_millionths: peak,
            rms_millionths: peak / 2,
        }
    }

    #[test]
    fn decibel_conversion_clamps_to_floor() {
        assert_eq!(millionths_to_mdb(1_000_000), 0);
        assert_eq!(millionths_to_mdb(0), FLOOR_MDB);
        assert_eq!(millionths_to_mdb(1), FLOOR_MDB);
        assert_eq!(millionths_to_mdb(500_000), -6_021);
    }

    #[test]
    fn peak_is_held_then_decays_at_the_configured_rate() {
        let mut state = ChannelBallistics::default();
        state.step(Some(-6_000), 16);
        assert_eq!(state.held_mdb, -6_000);
        assert_eq!(state.hold_remaining_ms, PEAK_HOLD_MS);
        // Quieter samples do not move the marker during the hold window.
        for _ in 0..6 {
            state.step(Some(-30_000), 250);
        }
        assert_eq!(state.held_mdb, -6_000);
        assert_eq!(state.hold_remaining_ms, 0);
        // After the hold, 100 ms falls 2 dB.
        state.step(Some(-30_000), 100);
        assert_eq!(state.held_mdb, -8_000);
        // Frames without a new sample keep aging the marker.
        state.step(None, 50);
        assert_eq!(state.held_mdb, -9_000);
        // A louder sample recaptures the hold.
        state.step(Some(-3_000), 16);
        assert_eq!(state.held_mdb, -3_000);
        assert_eq!(state.hold_remaining_ms, PEAK_HOLD_MS);
    }

    #[test]
    fn marker_falls_to_the_floor_but_never_below_the_live_peak() {
        let mut state = ChannelBallistics::default();
        state.step(Some(0), 0);
        for _ in 0..40 {
            state.step(Some(-20_000), 250);
        }
        assert_eq!(state.held_mdb, -20_000);
        assert!(!state.animating());
        for _ in 0..40 {
            state.step(Some(FLOOR_MDB), 250);
        }
        assert_eq!(state.held_mdb, FLOOR_MDB);
        assert!(!state.animating());
    }

    #[test]
    fn large_frame_steps_are_bounded() {
        let mut state = ChannelBallistics::default();
        state.step(Some(-6_000), 0);
        state.step(Some(FLOOR_MDB), 10_000);
        assert_eq!(state.hold_remaining_ms, PEAK_HOLD_MS - 250);
        assert_eq!(state.held_mdb, -6_000);
    }

    #[test]
    fn clip_lamp_latches_for_the_hold_time() {
        let mut state = ChannelBallistics::default();
        state.step(Some(-1_000), 16);
        assert!(!state.clipping());
        state.step(Some(0), 16);
        assert!(state.clipping());
        // A stale clipping sample does not re-arm the lamp.
        for _ in 0..11 {
            state.step(None, 250);
        }
        assert!(state.clipping());
        state.step(None, 250);
        assert!(!state.clipping());
    }

    #[test]
    fn meters_without_samples_drop_their_state() {
        let input = InputId::new(NonZeroU128::new(3).unwrap());
        let mut meters = MeterBallistics::default();
        meters.begin_frame(Some(1));
        let states = meters.update(
            MeterKey::Input(input),
            Some(&[channel(1_000_000), channel(10)]),
            16,
        );
        assert_eq!(states.len(), 2);
        assert!(states[0].clipping());
        assert!(!states[1].clipping());
        meters.update(MeterKey::Master, Some(&[channel(500_000)]), 16);
        assert!(meters.animating());
        assert_eq!(meters.update(MeterKey::Input(input), None, 16), &[]);
        meters.retain(|key| key != MeterKey::Master);
        assert!(!meters.animating());
    }

    #[test]
    fn only_new_meter_messages_feed_peaks() {
        let mut meters = MeterBallistics::default();
        meters.begin_frame(Some(7));
        assert!(meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 16)[0].clipping());
        // The same message on later frames only ages the lamp.
        for _ in 0..12 {
            meters.begin_frame(Some(7));
            meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 250);
        }
        meters.begin_frame(Some(7));
        assert!(!meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 16)[0].clipping());
        meters.begin_frame(Some(8));
        assert!(meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 16)[0].clipping());
    }

    #[test]
    fn frame_time_converts_to_bounded_milliseconds() {
        assert_eq!(frame_dt_ms(0.016_6), 17);
        assert_eq!(frame_dt_ms(-1.0), 0);
        assert_eq!(frame_dt_ms(f32::NAN), 0);
        assert_eq!(frame_dt_ms(1_000.0), u32::from(u16::MAX));
    }
}
