//! Drift-corrected ingest of one live source with its own clock (AU-004).
//!
//! A capture card, a network stream, or a sound-server device delivers audio
//! on its own crystal, which runs a few hundred ppm off the engine clock.
//! Cutting a fixed number of samples per engine frame lets the backlog grow
//! or drain until something gives (a dropped or faded block: a click, and an
//! A/V offset that grows over a long show). A [`LiveSourceClock`] instead
//! pushes the source's audio, contiguous within one connection, into a
//! [`SourceSync`] configured for live delivery:
//!
//! * buffer-fill tracking with a smoothed PI loop
//!   ([`DriftControlConfig::live`]: ±1000 ppm, 0.01 Hz) trims the
//!   windowed-sinc conversion ratio, so the backlog stays at its target
//!   latency without dropping or inserting a sample;
//! * a read-side hard resync (declicked skip back to the target) fires only
//!   when the fill is more than one target latency above the target, and an
//!   underrun (silence, then a declicked re-prime) only when the source
//!   stops delivering; both are counted in [`LiveSourceTelemetry`].
//!
//! # Time-weighted fill
//!
//! Audio arrives in bursts (an `ffmpeg` audio frame, a socket read) that
//! are not commensurate with the engine frame. The fill sampled just before
//! each pull then aliases the burst sawtooth into a slow beat (its period is
//! the burst size over the drift in samples per second: tens of seconds),
//! which a fill-driven loop cannot tell from drift and chases with ratio
//! swings of hundreds of ppm. Every push and pull therefore carries its
//! arrival time, and the loop observes the fill through a one-pole filter
//! run in continuous time over the piecewise-constant fill
//! ([`LIVE_FILL_SMOOTHING_SECONDS`]), plus half a frame so it compares with
//! the before-pull target. The burst sawtooth is attenuated by the filter at
//! its own (tens of hertz) frequency instead of being sampled.
//!
//! A jump of the fill (a new connection, a re-prime after an underrun, a
//! resync, an overrun) lands it at the target only to within one burst,
//! depending on where in a burst the cut fell. Rather than pull that
//! sub-burst offset back through the integral (moving the drift estimate
//! for minutes), the loop holds its ratio for [`LIVE_REANCHOR_SECONDS`]
//! after a jump, then re-anchors: the settled offset (bounded to half the
//! target) becomes the new zero. The drift estimate survives pauses and
//! reconnects untouched.
//!
//! # Pictures follow the audio
//!
//! The source's pictures queue beside its audio and are popped one per
//! engine frame. Their queue drifts at the same rate the audio would; each
//! pull compares the queue's latency with the audio's (both smoothed over
//! about a second, in frames: pictures queued against the time-weighted
//! audio fill over the frame's sample count) and, when the pictures run more than
//! [`AV_TOLERANCE_FRAMES`] behind the audio, drops one extra picture at the
//! source boundary, or repeats the current one when they run ahead. Audio
//! and video of a source therefore stay within a frame of each other, and
//! the picture queue cannot drift into overflow or starvation. Comparing
//! latencies rather than counts also holds for a capture card whose
//! pictures are stamped on the host clock while its audio runs on the
//! card's crystal.
//!
//! All storage is allocated in [`LiveSourceClock::new`]; given identical
//! pushes and pulls the output is bit-identical.

use fm_types::{FrameRate, SampleRate};

use crate::resampler::usize_to_f64;
use crate::source_sync::{
    DriftControlConfig, MAX_SOURCE_SYNC_CAPACITY_SAMPLES, MAX_SOURCE_SYNC_PUSH_SAMPLES, PullReport,
    SourceSync, SourceSyncConfig, SourceSyncError, SourceSyncState, SourceSyncTelemetry,
};

/// Fewest frames of audio a live source buffers: one frame is pulled at a
/// time, so less would underrun on ordinary delivery jitter.
pub const MIN_LIVE_LATENCY_FRAMES: usize = 2;
/// Fewest samples a live source buffers, above the sinc kernel's lookahead.
pub const MIN_LIVE_TARGET_SAMPLES: usize = 256;
/// How far, in frames, the smoothed picture latency may stray from the
/// audio latency before a picture is dropped or repeated. Each correction
/// moves the pictures one frame, so the A/V offset stays inside
/// `±AV_TOLERANCE_FRAMES`, and corrections cannot oscillate.
pub const AV_TOLERANCE_FRAMES: f64 = 0.75;
/// Time constant of the picture and audio latency smoothing, in seconds.
pub const AV_SMOOTHING_SECONDS: f64 = 1.0;
/// Time constant of the continuous-time fill observation, in seconds.
pub const LIVE_FILL_SMOOTHING_SECONDS: f64 = 2.0;
/// How long the ratio is held after the fill jumps (connection, re-prime,
/// resync, overrun) before the loop observes again: three filter time
/// constants, so the observation has settled on the new fill.
pub const LIVE_REANCHOR_SECONDS: f64 = 3.0 * LIVE_FILL_SMOOTHING_SECONDS;

/// Construction parameters of one [`LiveSourceClock`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveSourceConfig {
    sample_rate: SampleRate,
    output_rate: SampleRate,
    channels: usize,
    frame_rate: FrameRate,
    latency_frames: usize,
    drift_control: DriftControlConfig,
}

impl LiveSourceConfig {
    /// A source delivering `channels` of audio at `sample_rate` (the engine
    /// rate unless [`Self::with_output_rate`] says otherwise), pulled once
    /// per frame at `frame_rate`, buffering `latency_frames` frames (at
    /// least [`MIN_LIVE_LATENCY_FRAMES`]), with [`DriftControlConfig::live`].
    #[must_use]
    pub const fn new(
        sample_rate: SampleRate,
        channels: usize,
        frame_rate: FrameRate,
        latency_frames: usize,
    ) -> Self {
        Self {
            sample_rate,
            output_rate: sample_rate,
            channels,
            frame_rate,
            latency_frames,
            drift_control: DriftControlConfig::live(),
        }
    }

    #[must_use]
    pub const fn with_output_rate(mut self, rate: SampleRate) -> Self {
        self.output_rate = rate;
        self
    }

    #[must_use]
    pub const fn with_drift_control(mut self, config: DriftControlConfig) -> Self {
        self.drift_control = config;
        self
    }

    /// Output samples of one frame, rounded up.
    #[must_use]
    pub fn frame_samples(&self) -> usize {
        let numerator =
            u128::from(self.output_rate.hertz()) * u128::from(self.frame_rate.denominator());
        let samples = numerator.div_ceil(u128::from(self.frame_rate.numerator()).max(1));
        usize::try_from(samples).unwrap_or(usize::MAX)
    }

    /// The converter configuration: a target of `latency_frames` frames of
    /// input (at least two, and at least [`MIN_LIVE_TARGET_SAMPLES`]), four
    /// times that as capacity, and a read-side resync one target above the
    /// target.
    #[must_use]
    pub fn sync_config(&self) -> SourceSyncConfig {
        let input_frame = usize::try_from(
            (u128::from(self.sample_rate.hertz()) * u128::from(self.frame_rate.denominator()))
                .div_ceil(u128::from(self.frame_rate.numerator()).max(1)),
        )
        .unwrap_or(usize::MAX);
        let target = self
            .latency_frames
            .max(MIN_LIVE_LATENCY_FRAMES)
            .saturating_mul(input_frame)
            .clamp(
                MIN_LIVE_TARGET_SAMPLES,
                MAX_SOURCE_SYNC_CAPACITY_SAMPLES / 2,
            );
        let capacity = target
            .saturating_mul(4)
            .min(MAX_SOURCE_SYNC_CAPACITY_SAMPLES);
        SourceSyncConfig::new(self.sample_rate, self.channels)
            .with_output_rate(self.output_rate)
            .with_target_fill_samples(target)
            .with_capacity_samples(capacity)
            .with_drift_control(self.drift_control)
            .with_resync_threshold_samples(Some(target))
    }

    #[must_use]
    pub const fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    #[must_use]
    pub const fn output_rate(&self) -> SampleRate {
        self.output_rate
    }

    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    #[must_use]
    pub const fn latency_frames(&self) -> usize {
        self.latency_frames
    }

    #[must_use]
    pub const fn drift_control(&self) -> DriftControlConfig {
        self.drift_control
    }
}

/// What the source's picture queue does this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PictureAction {
    /// Drop `skip` extra pictures (the pictures lag the audio), then show
    /// the next one; `skip` is 0 on an ordinary frame.
    Advance { skip: usize },
    /// Keep showing the current picture (the pictures lead the audio).
    Hold,
}

/// Result of one [`LiveSourceClock::pull`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LivePull {
    pub audio: PullReport,
    pub picture: PictureAction,
}

/// Per-source drift telemetry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveSourceTelemetry {
    pub sync: SourceSyncTelemetry,
    /// Extra pictures dropped to follow the audio.
    pub picture_drops: u64,
    /// Pictures repeated to wait for the audio.
    pub picture_repeats: u64,
    /// Smoothed audio latency minus picture latency, in frames, while the
    /// pictures follow the audio.
    pub av_offset_frames: Option<f64>,
    /// Connections (or other restarts) since construction.
    pub restarts: u64,
}

impl LiveSourceTelemetry {
    /// Hard resyncs: read-side skips plus capacity overruns. Underruns
    /// (the source stopped delivering) are counted apart.
    #[must_use]
    pub const fn hard_resyncs(&self) -> u64 {
        self.sync.resyncs.saturating_add(self.sync.overruns)
    }

    /// Unread input in milliseconds of source audio.
    #[must_use]
    pub fn buffer_ms(&self) -> f64 {
        self.sync.buffer_fill_samples * 1_000.0 / f64::from(self.sync.input_rate_hz.max(1))
    }

    /// Target fill in milliseconds of source audio.
    #[must_use]
    pub fn target_ms(&self) -> f64 {
        usize_to_f64(self.sync.target_fill_samples) * 1_000.0
            / f64::from(self.sync.input_rate_hz.max(1))
    }
}

/// Smoothed latencies, in frames, of the pictures and the audio.
#[derive(Clone, Copy, Debug)]
struct Latencies {
    pictures: f64,
    audio: f64,
}

/// The fill observed through a one-pole filter in continuous time.
#[derive(Clone, Copy, Debug)]
struct FillFilter {
    /// Filtered fill, in input samples.
    value: f64,
    /// The fill since the last event.
    level: f64,
    /// Time of the last event, in seconds.
    at: f64,
}

impl FillFilter {
    /// Advances to `at` over the constant `level` since the last event.
    fn advance(&mut self, at: f64) {
        let elapsed = at - self.at;
        if elapsed > 0.0 && elapsed.is_finite() {
            let keep = (-elapsed / LIVE_FILL_SMOOTHING_SECONDS).exp();
            self.value = self.level + (self.value - self.level) * keep;
            self.at = at;
        }
    }
}

/// Counters whose change means the fill jumped (skip, overrun, re-prime).
fn fill_epoch(telemetry: &SourceSyncTelemetry) -> u64 {
    telemetry
        .resyncs
        .wrapping_add(telemetry.overruns)
        .wrapping_add(telemetry.underruns)
        .wrapping_add(telemetry.resets)
        .wrapping_add(telemetry.splices)
}

/// One live source: drift-corrected audio and the picture decisions that
/// keep its video aligned with it.
#[derive(Debug)]
pub struct LiveSourceClock {
    sync: SourceSync,
    /// Output samples per frame (nominal, for latency in frames).
    frame_samples: f64,
    /// Input samples per frame (nominal).
    input_frame_samples: f64,
    /// Weight of one frame in the latency smoothing.
    alpha: f64,
    next_index: u64,
    fill: Option<FillFilter>,
    /// The ratio is held until this time after a jump of the fill.
    hold_until: Option<f64>,
    /// Settled observation minus target captured at the last re-anchor.
    anchor_offset: f64,
    /// [`fill_epoch`] and state when the filter was last seeded.
    epoch: (u64, SourceSyncState),
    latencies: Option<Latencies>,
    picture_drops: u64,
    picture_repeats: u64,
    restarts: u64,
}

impl LiveSourceClock {
    /// Validates `config` and preallocates all storage.
    ///
    /// # Errors
    ///
    /// Returns the converter's configuration error (for example a rate
    /// outside the resampler's range).
    pub fn new(config: LiveSourceConfig) -> Result<Self, SourceSyncError> {
        let sync = SourceSync::new(config.sync_config())?;
        let fps = f64::from(config.frame_rate.numerator())
            / f64::from(config.frame_rate.denominator().max(1));
        let frames_per_tau = (fps * AV_SMOOTHING_SECONDS).max(1.0);
        let per_frame = |rate: SampleRate| f64::from(rate.hertz()) / fps.max(f64::MIN_POSITIVE);
        Ok(Self {
            sync,
            frame_samples: per_frame(config.output_rate),
            input_frame_samples: per_frame(config.sample_rate),
            alpha: 1.0 / (frames_per_tau + 1.0),
            next_index: 0,
            fill: None,
            hold_until: None,
            anchor_offset: 0.0,
            epoch: (0, SourceSyncState::Priming),
            latencies: None,
            picture_drops: 0,
            picture_repeats: 0,
            restarts: 0,
        })
    }

    #[must_use]
    pub fn sync(&self) -> &SourceSync {
        &self.sync
    }

    #[must_use]
    pub fn state(&self) -> SourceSyncState {
        self.sync.state()
    }

    /// Output samples of one frame (nominal).
    #[must_use]
    pub fn frame_samples(&self) -> f64 {
        self.frame_samples
    }

    /// A new connection to the source: drops buffered audio and re-primes,
    /// keeping the drift estimate (the source's crystal is unchanged).
    pub fn restart(&mut self) {
        self.sync.reset();
        self.next_index = 0;
        self.fill = None;
        self.latencies = None;
        self.restarts = self.restarts.saturating_add(1);
    }

    /// Forgets the drift estimate, for a different device.
    pub fn forget_drift(&mut self) {
        self.sync.reset_drift_estimate();
    }

    /// Pushes the next planar block of the connection's audio, which
    /// arrived at `at` seconds (any fixed epoch, shared with
    /// [`Self::pull`]).
    ///
    /// # Errors
    ///
    /// Returns the converter's shape or finiteness error; for a block above
    /// [`MAX_SOURCE_SYNC_PUSH_SAMPLES`], chunks before the failing one stay
    /// pushed.
    pub fn push(&mut self, planes: &[&[f32]], at: f64) -> Result<(), SourceSyncError> {
        self.observe(at);
        let result = self.push_planes(planes);
        self.settle(false, at);
        result
    }

    fn push_planes(&mut self, planes: &[&[f32]]) -> Result<(), SourceSyncError> {
        let samples = planes.first().map_or(0, |plane| plane.len());
        if samples <= MAX_SOURCE_SYNC_PUSH_SAMPLES {
            self.sync.push(self.next_index, planes)?;
            self.advance(samples);
            return Ok(());
        }
        let mut start = 0;
        while start < samples {
            let end = samples.min(start + MAX_SOURCE_SYNC_PUSH_SAMPLES);
            let chunk: Vec<&[f32]> = planes
                .iter()
                .map(|plane| plane.get(start..end).unwrap_or(plane))
                .collect();
            self.sync.push(self.next_index, &chunk)?;
            self.advance(end - start);
            start = end;
        }
        Ok(())
    }

    fn advance(&mut self, samples: usize) {
        self.next_index = self
            .next_index
            .saturating_add(u64::try_from(samples).unwrap_or(u64::MAX));
    }

    /// Advances the fill filter to `at`, over the fill since the last event.
    fn observe(&mut self, at: f64) {
        if let Some(filter) = &mut self.fill {
            filter.advance(at);
        } else {
            let level = self.sync.buffer_fill();
            self.fill = Some(FillFilter {
                value: level,
                level,
                at,
            });
        }
    }

    /// Records the fill after an event; a jump (skip, overrun, re-prime,
    /// reset) or a state change reseeds the filter at the new fill.
    ///
    /// A reseed assumes steady delivery: after a pull the mean fill over
    /// the coming frame is half a frame above the level, after a push half
    /// a frame below it. The picture latency smoothing restarts too, so the
    /// pictures catch up with a skipped or re-primed audio position at once.
    fn settle(&mut self, after_pull: bool, at: f64) {
        let level = self.sync.buffer_fill();
        let epoch = (fill_epoch(&self.sync.telemetry()), self.sync.state());
        let half_frame = self.input_frame_samples / 2.0;
        if let Some(filter) = &mut self.fill {
            filter.level = level;
            if epoch != self.epoch {
                filter.value = if after_pull {
                    level + half_frame
                } else {
                    (level - half_frame).max(0.0)
                };
                self.latencies = None;
                self.hold_until = Some(at + LIVE_REANCHOR_SECONDS);
            }
        }
        self.epoch = epoch;
    }

    /// The loop's observation: the time-weighted fill plus half a frame of
    /// input, comparable with the fill sampled just before a pull.
    fn observation(&self) -> Option<f64> {
        self.fill
            .map(|filter| filter.value + self.input_frame_samples / 2.0)
    }

    /// The observation the loop is fed at `at`: the target itself (no
    /// error, the ratio holds) while re-anchoring after a jump, then the
    /// observation less the offset captured when the hold ended.
    fn loop_observation(&mut self, at: f64) -> Option<f64> {
        let observation = self.observation()?;
        let target = usize_to_f64(self.sync.config().target_fill_samples());
        if let Some(until) = self.hold_until {
            if at < until {
                return Some(target);
            }
            self.hold_until = None;
            let bound = target / 2.0;
            self.anchor_offset = (observation - target).clamp(-bound, bound);
        }
        Some(observation - self.anchor_offset)
    }

    /// Renders this frame's audio into `output` (one plane per channel, the
    /// frame's sample count each), pulled at `at` seconds, and decides the
    /// picture queue's action.
    ///
    /// `queued_pictures` is the number of pictures queued before this
    /// frame's picture is taken, or `None` while the source's pictures are
    /// not live (connecting, buffering, stalled) or it has none: the audio
    /// is still pulled, so its buffer stays at the target, and the latency
    /// smoothing restarts when the pictures come back.
    ///
    /// # Errors
    ///
    /// Returns the converter's output-shape error; on error nothing changes.
    pub fn pull(
        &mut self,
        output: &mut [&mut [f32]],
        queued_pictures: Option<usize>,
        at: f64,
    ) -> Result<LivePull, SourceSyncError> {
        self.observe(at);
        let running = self.sync.state() == SourceSyncState::Running;
        // Until running, the converter's own (trimmed) fill is the better
        // observation for the first update.
        let observed = if running {
            self.loop_observation(at)
        } else {
            None
        };
        let audio = self.sync.pull_observed(output, observed)?;
        self.settle(true, at);
        let fill = self
            .observation()
            .unwrap_or_else(|| self.sync.buffer_fill());
        let picture = match queued_pictures {
            Some(queued) if running => self.follow(queued, fill),
            _ => {
                self.latencies = None;
                PictureAction::Advance { skip: 0 }
            }
        };
        Ok(LivePull { audio, picture })
    }

    /// Compares the smoothed picture and audio latencies and corrects the
    /// pictures by one frame when they stray beyond the tolerance.
    fn follow(&mut self, queued: usize, fill: f64) -> PictureAction {
        let pictures = usize_to_f64(queued);
        let audio = fill / self.input_frame_samples.max(f64::MIN_POSITIVE);
        let alpha = self.alpha;
        let latencies = self.latencies.get_or_insert(Latencies { pictures, audio });
        latencies.pictures += alpha * (pictures - latencies.pictures);
        latencies.audio += alpha * (audio - latencies.audio);
        let lag = latencies.pictures - latencies.audio;
        if lag > AV_TOLERANCE_FRAMES && queued >= 2 {
            // Catch up at once after a jump; one frame at a time under drift.
            let skip = (lag - AV_TOLERANCE_FRAMES)
                .ceil()
                .clamp(1.0, usize_to_f64(queued - 1));
            latencies.pictures -= skip;
            let skip = f64_to_usize(skip);
            self.picture_drops = self
                .picture_drops
                .saturating_add(u64::try_from(skip).unwrap_or(u64::MAX));
            PictureAction::Advance { skip }
        } else if lag < -AV_TOLERANCE_FRAMES && queued >= 1 {
            latencies.pictures += 1.0;
            self.picture_repeats = self.picture_repeats.saturating_add(1);
            PictureAction::Hold
        } else {
            PictureAction::Advance { skip: 0 }
        }
    }

    /// A telemetry snapshot. Never allocates.
    #[must_use]
    pub fn telemetry(&self) -> LiveSourceTelemetry {
        LiveSourceTelemetry {
            sync: self.sync.telemetry(),
            picture_drops: self.picture_drops,
            picture_repeats: self.picture_repeats,
            av_offset_frames: self
                .latencies
                .map(|latencies| latencies.audio - latencies.pictures),
            restarts: self.restarts,
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f64_to_usize(value: f64) -> usize {
    value as usize
}
