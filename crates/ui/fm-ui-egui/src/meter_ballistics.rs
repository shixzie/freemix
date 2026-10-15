//! Pure meter ballistics: peak-hold markers that decay and clip lamps that
//! latch until the operator clears them.
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
/// Peaks at or above this level (milli-dBFS) light the clip lamp.
pub(crate) const CLIP_THRESHOLD_MDB: i32 = -100;
/// Floor below which a held marker is considered gone (matches the meter
/// scale floor of -60 dBFS).
pub(crate) const FLOOR_MDB: i32 = -60_000;
/// Meters with no new meter message for longer than this are stale: they
/// keep the last sample, dimmed and marked `STALE`, like monitor frames.
pub(crate) const METER_STALE_AFTER_MS: u32 = 1_000;
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
    /// The clip lamp is latched until the operator clears it.
    pub(crate) clip_latched: bool,
    /// Latest observed peak; a released marker never falls below it.
    pub(crate) live_mdb: i32,
}

impl Default for ChannelBallistics {
    fn default() -> Self {
        Self {
            held_mdb: FLOOR_MDB,
            hold_remaining_ms: 0,
            clip_latched: false,
            live_mdb: FLOOR_MDB,
        }
    }
}

impl ChannelBallistics {
    /// Advances by `dt_ms` and folds in a newly received peak sample, if any.
    ///
    /// Without a new sample the state only ages: a repeated stale sample
    /// does not re-arm a cleared clip lamp.
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
        if let Some(peak_mdb) = sample_mdb {
            self.live_mdb = peak_mdb;
            if peak_mdb >= self.held_mdb {
                self.held_mdb = peak_mdb;
                self.hold_remaining_ms = PEAK_HOLD_MS;
            }
            if peak_mdb >= CLIP_THRESHOLD_MDB {
                self.clip_latched = true;
            }
        }
        self.held_mdb = self.held_mdb.max(self.live_mdb);
    }

    /// Whether the clip lamp is lit.
    pub(crate) const fn clipping(self) -> bool {
        self.clip_latched
    }

    /// Whether the marker still moves over time without new input. A latched
    /// clip lamp is static until cleared.
    pub(crate) const fn animating(self) -> bool {
        self.held_mdb > self.live_mdb
    }
}

/// Which meter a ballistic state belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MeterKey {
    Input(InputId),
    /// A VCA group strip (1-4): the loudest of its member strips.
    Group(u8),
    Master,
    /// The monitor (headphone) bus (AU-001).
    Monitor,
}

/// Ballistic state for every visible meter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MeterBallistics {
    meters: Vec<(MeterKey, Vec<ChannelBallistics>)>,
    /// Sequence of the meter message whose peaks were last folded in.
    last_sequence: Option<u64>,
    /// Whether the current frame carries a meter message not yet folded in.
    fresh: bool,
    /// Whether the current frame has any meter message at all.
    has_sample: bool,
    /// Frame time since the latest new meter message arrived.
    since_message_ms: u32,
}

impl MeterBallistics {
    /// Starts a frame with the sequence of the latest meter message, if any,
    /// `dt_ms` after the previous frame.
    pub(crate) fn begin_frame(&mut self, sequence: Option<u64>, dt_ms: u32) {
        self.fresh = sequence.is_some() && sequence != self.last_sequence;
        self.has_sample = sequence.is_some();
        if self.fresh {
            self.since_message_ms = 0;
        } else {
            self.since_message_ms = self.since_message_ms.saturating_add(dt_ms);
        }
        if sequence.is_some() {
            self.last_sequence = sequence;
        }
    }

    /// Whether the shown meter message is older than
    /// [`METER_STALE_AFTER_MS`]; never without a message.
    pub(crate) const fn stale(&self) -> bool {
        self.has_sample && self.since_message_ms > METER_STALE_AFTER_MS
    }

    /// Frame time since the latest new meter message, for the `STALE` label.
    pub(crate) const fn message_age_ms(&self) -> u32 {
        self.since_message_ms
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

    /// Clears the latched clip lamps of one meter.
    pub(crate) fn clear_clip(&mut self, key: MeterKey) {
        for (_, states) in self
            .meters
            .iter_mut()
            .filter(|(existing, _)| *existing == key)
        {
            for state in states {
                state.clip_latched = false;
            }
        }
    }

    /// Clears every latched clip lamp.
    pub(crate) fn clear_all_clips(&mut self) {
        for (_, states) in &mut self.meters {
            for state in states {
                state.clip_latched = false;
            }
        }
    }

    /// Number of meters with a latched clip lamp.
    pub(crate) fn clipped_meters(&self) -> usize {
        self.meters
            .iter()
            .filter(|(_, states)| states.iter().any(|state| state.clip_latched))
            .count()
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

/// The highest held peak across a meter's channels, in milli-dBFS.
pub(crate) fn held_peak_mdb(states: &[ChannelBallistics]) -> Option<i32> {
    states.iter().map(|state| state.held_mdb).max()
}

/// Peak-hold readout text: `--` without a sample, `-inf` at the floor,
/// `CLIP` while latched, else the held level to one decimal (`-12.4`).
pub(crate) fn peak_readout(states: &[ChannelBallistics]) -> String {
    if states.iter().any(|state| state.clip_latched) {
        return "CLIP".to_owned();
    }
    match held_peak_mdb(states) {
        None => "--".to_owned(),
        Some(mdb) if mdb <= FLOOR_MDB => "-inf".to_owned(),
        Some(mdb) => {
            // Tenths of a dB, rounded half away from zero.
            #[allow(clippy::cast_possible_truncation)] // bounded milli-dB
            let tenths = (f64::from(mdb) / 100.0).round() as i32;
            let sign = match tenths.cmp(&0) {
                core::cmp::Ordering::Greater => "+",
                core::cmp::Ordering::Less => "-",
                core::cmp::Ordering::Equal => "",
            };
            format!("{sign}{}.{}", tenths.abs() / 10, tenths.abs() % 10)
        }
    }
}

/// Converts an `egui` frame time in seconds to whole milliseconds.
pub(crate) fn frame_dt_ms(dt_seconds: f32) -> u32 {
    if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
    let ms = (dt_seconds * 1_000.0).round().min(f32::from(u16::MAX)) as u32;
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
    fn clip_lamp_latches_until_cleared() {
        let mut state = ChannelBallistics::default();
        state.step(Some(-1_000), 16);
        assert!(!state.clipping());
        state.step(Some(0), 16);
        assert!(state.clipping());
        // Time and quieter samples never release the lamp.
        for _ in 0..100 {
            state.step(Some(-40_000), 250);
        }
        assert!(state.clipping());
        // A latched lamp is static: it needs no animation repaints.
        assert!(!state.animating());
    }

    #[test]
    fn clearing_releases_one_meter_or_all() {
        let input = InputId::new(NonZeroU128::new(3).unwrap());
        let mut meters = MeterBallistics::default();
        meters.begin_frame(Some(1), 16);
        meters.update(MeterKey::Input(input), Some(&[channel(1_000_000)]), 16);
        meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 16);
        assert_eq!(meters.clipped_meters(), 2);
        meters.clear_clip(MeterKey::Input(input));
        assert_eq!(meters.clipped_meters(), 1);
        // The same message on a later frame does not re-arm the cleared lamp.
        meters.begin_frame(Some(1), 16);
        assert!(
            !meters.update(MeterKey::Input(input), Some(&[channel(1_000_000)]), 16)[0].clipping()
        );
        meters.clear_all_clips();
        assert_eq!(meters.clipped_meters(), 0);
        // A new clipping message latches again.
        meters.begin_frame(Some(2), 16);
        assert!(meters.update(MeterKey::Master, Some(&[channel(1_000_000)]), 16)[0].clipping());
    }

    #[test]
    fn peak_readout_shows_the_highest_hold_or_clip() {
        assert_eq!(peak_readout(&[]), "--");
        let mut low = ChannelBallistics::default();
        assert_eq!(peak_readout(&[low]), "-inf");
        low.step(Some(-12_449), 16);
        let mut high = ChannelBallistics::default();
        high.step(Some(-6_050), 16);
        assert_eq!(held_peak_mdb(&[low, high]), Some(-6_050));
        assert_eq!(peak_readout(&[low, high]), "-6.1");
        assert_eq!(peak_readout(&[low]), "-12.4");
        let mut quiet = ChannelBallistics::default();
        quiet.step(Some(-140), 16);
        assert_eq!(peak_readout(&[quiet]), "-0.1");
        let mut hot = ChannelBallistics::default();
        hot.step(Some(-250), 16);
        assert_eq!(peak_readout(&[hot]), "-0.3");
        hot.step(Some(0), 16);
        assert_eq!(peak_readout(&[low, hot]), "CLIP");
    }

    #[test]
    fn meters_without_samples_drop_their_state() {
        let input = InputId::new(NonZeroU128::new(3).unwrap());
        let mut meters = MeterBallistics::default();
        meters.begin_frame(Some(1), 16);
        let states = meters.update(
            MeterKey::Input(input),
            Some(&[channel(1_000_000), channel(10)]),
            16,
        );
        assert_eq!(states.len(), 2);
        assert!(states[0].clipping());
        assert!(!states[1].clipping());
        meters.update(MeterKey::Master, Some(&[channel(500_000)]), 16);
        assert_eq!(meters.clipped_meters(), 1);
        assert_eq!(meters.update(MeterKey::Input(input), None, 16), &[]);
        assert_eq!(meters.clipped_meters(), 0);
        meters.retain(|key| key != MeterKey::Master);
        assert!(!meters.animating());
    }

    #[test]
    fn only_new_meter_messages_feed_peaks() {
        let mut meters = MeterBallistics::default();
        meters.begin_frame(Some(7), 16);
        let held = meters.update(MeterKey::Master, Some(&[channel(500_000)]), 16)[0].held_mdb;
        assert_eq!(held, -6_021);
        // The same message on later frames only ages the marker.
        for _ in 0..12 {
            meters.begin_frame(Some(7), 16);
            meters.update(MeterKey::Master, Some(&[channel(500_000)]), 250);
        }
        meters.begin_frame(Some(7), 16);
        let aged = meters.update(MeterKey::Master, Some(&[channel(500_000)]), 16)[0];
        // The live peak floors the marker at the level last seen.
        assert_eq!(aged.held_mdb, -6_021);
        assert_eq!(aged.hold_remaining_ms, 0);
        meters.begin_frame(Some(8), 16);
        let fresh = meters.update(MeterKey::Master, Some(&[channel(500_000)]), 16)[0];
        assert_eq!(fresh.hold_remaining_ms, PEAK_HOLD_MS);
    }

    #[test]
    fn meters_go_stale_without_new_messages() {
        let mut meters = MeterBallistics::default();
        meters.begin_frame(None, 16);
        assert!(!meters.stale(), "no message is not stale, just unavailable");
        meters.begin_frame(Some(1), 16);
        assert!(!meters.stale());
        assert_eq!(meters.message_age_ms(), 0);
        // The same message keeps aging by frame time.
        for _ in 0..4 {
            meters.begin_frame(Some(1), 250);
        }
        assert_eq!(meters.message_age_ms(), 1_000);
        assert!(!meters.stale(), "exactly one second is still current");
        meters.begin_frame(Some(1), 1);
        assert!(meters.stale());
        // A new message is current again.
        meters.begin_frame(Some(2), 16);
        assert!(!meters.stale());
        // Losing the message (reconnect) leaves nothing to mark stale.
        meters.begin_frame(Some(2), 5_000);
        assert!(meters.stale());
        meters.begin_frame(None, 16);
        assert!(!meters.stale());
    }

    #[test]
    fn frame_time_converts_to_bounded_milliseconds() {
        assert_eq!(frame_dt_ms(0.016_6), 17);
        assert_eq!(frame_dt_ms(-1.0), 0);
        assert_eq!(frame_dt_ms(f32::NAN), 0);
        assert_eq!(frame_dt_ms(1_000.0), u32::from(u16::MAX));
    }
}
