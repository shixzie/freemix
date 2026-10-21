//! Drift-corrected ingest of a live source that delivers timed blocks
//! (AU-004).
//!
//! The daemon's `ffmpeg`-backed receivers (network streams, V4L2/ALSA
//! capture, system audio, screen capture) push raw sample bursts into a
//! [`LiveSourceClock`] themselves. A source behind the `fm-io-api`
//! contract instead hands over [`AudioBlock`]s and pictures, each with its
//! [`MediaTiming`] on the source's own clock domain (a native capture
//! adapter such as `fm-io-macos`'s audio source, a browser input, a
//! WebRTC guest). A [`LiveBlockSource`] is the adapter between the two:
//!
//! * each block is placed on the connection's sample timeline by its
//!   sequence number and presentation time. A block whose sequence follows
//!   the previous one is contiguous whatever its timestamp says, so
//!   host-stamped jitter and the slow divergence of a host clock from the
//!   device crystal never become gaps. A skipped sequence (blocks lost on
//!   the way) is placed by the time elapsed since the previous block's
//!   end: a short gap is concealed with silence and a short overlap
//!   trimmed, both on the converter's declick rules, and a larger jump is
//!   spliced. A block flagged [`MediaFlags::DISCONTINUITY`], or a sequence
//!   that goes backwards (the source restarted its counter), is spliced
//!   with declick ramps;
//! * a block on a different clock domain is a different device: the
//!   connection restarts and the drift estimate is forgotten;
//! * pictures queue beside the audio (bounded; the oldest drops on
//!   overflow) and are shown one per engine frame as the clock's
//!   [`PictureAction`] says, so they follow the drift-corrected audio
//!   exactly as a receiver's do; the queue goes live once it holds the
//!   latency and stalls after [`PICTURE_STALL_FRAMES`] late frames;
//! * [`LiveBlockSource::live_telemetry`] is the same
//!   [`LiveSourceTelemetry`] a receiver reports, so the source's loop
//!   reaches `source_sync_status`, Studio's `SYNC` lamps, the Web Source
//!   sync section, and `remote-diagnostics` unchanged.
//!
//! All storage except the picture payloads is allocated in
//! [`LiveBlockSource::new`]; given identical pushes and pulls the output
//! is bit-identical.

use std::collections::VecDeque;
use std::fmt;

use fm_frame::{AudioBlock, ClockDomainId, MediaFlags, MediaTiming};

use crate::live_source::{
    LiveSourceClock, LiveSourceConfig, LiveSourceTelemetry, MIN_LIVE_LATENCY_FRAMES, PictureAction,
};
use crate::source_sync::{PullReport, PushReport, SourceSyncError};

/// Late engine frames in a row after which the pictures count as stalled
/// (their audio keeps playing; the clock stops following them until the
/// queue holds the latency again).
pub const PICTURE_STALL_FRAMES: usize = 2;
/// Fewest pictures a source may queue.
pub const MIN_PICTURE_CAPACITY: usize = 2;
/// Most pictures a source may queue.
pub const MAX_PICTURE_CAPACITY: usize = 64;
/// Source sample index of the first sample of a connection: far enough
/// from zero that an overlap trimmed off the first block cannot underflow.
const TIMELINE_BASE: u64 = 1 << 40;

/// Construction parameters of one [`LiveBlockSource`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveBlockConfig {
    live: LiveSourceConfig,
    picture_capacity: usize,
}

impl LiveBlockConfig {
    /// A block source converted by `live`, queueing at most four times its
    /// latency in pictures (within [`MIN_PICTURE_CAPACITY`] and
    /// [`MAX_PICTURE_CAPACITY`]).
    #[must_use]
    pub fn new(live: LiveSourceConfig) -> Self {
        let latency = live.latency_frames().max(MIN_LIVE_LATENCY_FRAMES);
        Self {
            live,
            picture_capacity: latency
                .saturating_mul(4)
                .clamp(MIN_PICTURE_CAPACITY, MAX_PICTURE_CAPACITY),
        }
    }

    /// Bounds the picture queue to `capacity` (clamped to
    /// [`MIN_PICTURE_CAPACITY`]..=[`MAX_PICTURE_CAPACITY`]).
    #[must_use]
    pub fn with_picture_capacity(mut self, capacity: usize) -> Self {
        self.picture_capacity = capacity.clamp(MIN_PICTURE_CAPACITY, MAX_PICTURE_CAPACITY);
        self
    }

    #[must_use]
    pub const fn live(&self) -> LiveSourceConfig {
        self.live
    }

    #[must_use]
    pub const fn picture_capacity(&self) -> usize {
        self.picture_capacity
    }

    /// Pictures queued before the pictures go live: the latency, at most
    /// the capacity.
    #[must_use]
    pub fn picture_latency(&self) -> usize {
        self.live
            .latency_frames()
            .max(MIN_LIVE_LATENCY_FRAMES)
            .min(self.picture_capacity)
    }
}

/// Why a block was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiveBlockError {
    /// The block's sample rate is not the source's.
    SampleRateMismatch { expected: u32, actual: u32 },
    /// The block's channel count is not the source's.
    ChannelCountMismatch { expected: usize, actual: usize },
    /// The converter refused the block or the pull.
    Sync(SourceSyncError),
}

impl fmt::Display for LiveBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SampleRateMismatch { expected, actual } => write!(
                formatter,
                "audio block at {actual} Hz on a {expected} Hz live source"
            ),
            Self::ChannelCountMismatch { expected, actual } => write!(
                formatter,
                "audio block with {actual} channels on a {expected}-channel live source"
            ),
            Self::Sync(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for LiveBlockError {}

impl From<SourceSyncError> for LiveBlockError {
    fn from(error: SourceSyncError) -> Self {
        Self::Sync(error)
    }
}

/// How one block was placed on the connection's sample timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockPlacement {
    /// The first block of a connection.
    Anchored,
    /// The next sequence number: contiguous with the previous block.
    Contiguous,
    /// Blocks were lost: placed by the time since the previous block's
    /// end (the converter conceals, trims, or splices as the distance
    /// says).
    Timed { missed_blocks: u64 },
    /// Flagged discontinuous, or the sequence went backwards: spliced.
    Discontinuous,
    /// A different clock domain: the connection restarted and the drift
    /// estimate was forgotten.
    NewClockDomain,
}

/// Result of one [`LiveBlockSource::push_audio`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockPush {
    pub placement: BlockPlacement,
    pub report: PushReport,
}

/// What the picture queue did on one pull.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PictureShown {
    /// The pictures are not live (none yet, or stalled): nothing new.
    None,
    /// A new picture is current (after dropping `skipped` extra ones).
    New { skipped: usize },
    /// The current picture is shown again (the pictures lead the audio).
    Held,
    /// No picture was queued in time: the current one is shown again.
    Late,
}

/// Result of one [`LiveBlockSource::pull`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockPull {
    pub audio: PullReport,
    pub picture: PictureShown,
}

/// Adapter counters beyond the clock's own telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LiveBlockCounters {
    /// Audio blocks accepted.
    pub blocks: u64,
    /// Sequence numbers skipped between accepted blocks.
    pub missed_blocks: u64,
    /// Blocks spliced as discontinuous (flagged, or a backwards sequence).
    pub discontinuities: u64,
    /// Clock domain changes (each restarts the connection).
    pub clock_domain_changes: u64,
    /// Pictures dropped because the queue was full.
    pub picture_overflows: u64,
    /// Engine frames whose picture was late.
    pub late_pictures: u64,
    /// Times the pictures stalled.
    pub picture_stalls: u64,
}

/// The adapter's full telemetry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveBlockTelemetry {
    pub live: LiveSourceTelemetry,
    pub counters: LiveBlockCounters,
}

/// Where the connection's timeline stands after the last accepted block.
#[derive(Clone, Copy, Debug)]
struct Timeline {
    domain: ClockDomainId,
    sequence: u64,
    /// Presentation time of the end of the last block, in nanoseconds.
    end_nanos: i128,
    /// Source sample index just after the last block.
    next_index: u64,
}

/// One live source delivering timed [`AudioBlock`]s and pictures `P`,
/// drift-corrected into the engine clock with its pictures following the
/// audio.
#[derive(Debug)]
pub struct LiveBlockSource<P> {
    config: LiveBlockConfig,
    clock: LiveSourceClock,
    timeline: Option<Timeline>,
    pictures: VecDeque<P>,
    current: Option<P>,
    pictures_live: bool,
    late_run: usize,
    counters: LiveBlockCounters,
}

impl<P> LiveBlockSource<P> {
    /// Validates `config` and preallocates the converter and the picture
    /// queue.
    ///
    /// # Errors
    ///
    /// Returns the converter's configuration error.
    pub fn new(config: LiveBlockConfig) -> Result<Self, SourceSyncError> {
        Ok(Self {
            clock: LiveSourceClock::new(config.live)?,
            pictures: VecDeque::with_capacity(config.picture_capacity),
            config,
            timeline: None,
            current: None,
            pictures_live: false,
            late_run: 0,
            counters: LiveBlockCounters::default(),
        })
    }

    #[must_use]
    pub const fn config(&self) -> &LiveBlockConfig {
        &self.config
    }

    #[must_use]
    pub const fn clock(&self) -> &LiveSourceClock {
        &self.clock
    }

    /// A new connection to the same device: drops buffered audio and
    /// pictures and re-primes, keeping the drift estimate.
    pub fn restart(&mut self) {
        self.clock.restart();
        self.timeline = None;
        self.pictures.clear();
        self.pictures_live = false;
        self.late_run = 0;
    }

    /// Pushes the next audio block, which arrived at `at` seconds (the
    /// epoch shared with [`Self::pull`]).
    ///
    /// # Errors
    ///
    /// Refuses a block whose rate or channel count is not the source's
    /// (nothing changes), or returns the converter's error.
    pub fn push_audio(&mut self, block: &AudioBlock, at: f64) -> Result<BlockPush, LiveBlockError> {
        let live = self.config.live;
        if block.sample_rate() != live.sample_rate() {
            return Err(LiveBlockError::SampleRateMismatch {
                expected: live.sample_rate().hertz(),
                actual: block.sample_rate().hertz(),
            });
        }
        if block.planes().len() != live.channels() {
            return Err(LiveBlockError::ChannelCountMismatch {
                expected: live.channels(),
                actual: block.planes().len(),
            });
        }
        let timing = block.timing();
        let (placement, first_sample, discontinuity) = self.place(timing);
        let mut planes: [&[f32]; crate::MAX_CHANNELS] = [&[]; crate::MAX_CHANNELS];
        for (slot, plane) in planes.iter_mut().zip(block.planes()) {
            *slot = plane;
        }
        let report =
            self.clock
                .push_at(first_sample, &planes[..live.channels()], at, discontinuity)?;
        let samples = u64::try_from(block.sample_count()).unwrap_or(u64::MAX);
        self.timeline = Some(Timeline {
            domain: timing.clock_domain(),
            sequence: timing.sequence().get(),
            end_nanos: end_nanos(timing),
            next_index: first_sample.saturating_add(samples),
        });
        self.counters.blocks = self.counters.blocks.saturating_add(1);
        match placement {
            BlockPlacement::Timed { missed_blocks } => {
                self.counters.missed_blocks =
                    self.counters.missed_blocks.saturating_add(missed_blocks);
            }
            BlockPlacement::Discontinuous => {
                self.counters.discontinuities = self.counters.discontinuities.saturating_add(1);
            }
            BlockPlacement::NewClockDomain => {
                self.counters.clock_domain_changes =
                    self.counters.clock_domain_changes.saturating_add(1);
            }
            BlockPlacement::Anchored | BlockPlacement::Contiguous => {}
        }
        Ok(BlockPush { placement, report })
    }

    /// Places a block with `timing`: its placement, the source sample index
    /// of its first sample, and whether it is pushed as a discontinuity.
    fn place(&mut self, timing: MediaTiming) -> (BlockPlacement, u64, bool) {
        let Some(previous) = self.timeline else {
            return (BlockPlacement::Anchored, TIMELINE_BASE, false);
        };
        if timing.clock_domain() != previous.domain {
            self.restart();
            self.clock.forget_drift();
            return (BlockPlacement::NewClockDomain, TIMELINE_BASE, false);
        }
        let sequence = timing.sequence().get();
        let flagged = timing.flags().contains(MediaFlags::DISCONTINUITY);
        if flagged || sequence <= previous.sequence {
            return (BlockPlacement::Discontinuous, previous.next_index, true);
        }
        let missed = sequence - previous.sequence - 1;
        if missed == 0 {
            return (BlockPlacement::Contiguous, previous.next_index, false);
        }
        // Lost blocks: the distance from the previous block's end.
        let delta = i128::from(timing.presentation_timestamp().as_nanos()) - previous.end_nanos;
        let rate = i128::from(self.config.live.sample_rate().hertz());
        let samples = rounded_div(delta.saturating_mul(rate), 1_000_000_000);
        let index = i128::from(previous.next_index).saturating_add(samples);
        let index = u64::try_from(index.max(0)).unwrap_or(u64::MAX);
        (
            BlockPlacement::Timed {
                missed_blocks: missed,
            },
            index,
            false,
        )
    }

    /// Queues the next picture; on a full queue the oldest is dropped.
    pub fn push_picture(&mut self, picture: P) {
        if self.pictures.len() >= self.config.picture_capacity {
            self.pictures.pop_front();
            self.counters.picture_overflows = self.counters.picture_overflows.saturating_add(1);
        }
        self.pictures.push_back(picture);
        if !self.pictures_live && self.pictures.len() >= self.config.picture_latency() {
            self.pictures_live = true;
            self.late_run = 0;
        }
    }

    /// Renders this engine frame's audio into `output` (one plane per
    /// channel, the frame's sample count each), pulled at `at` seconds, and
    /// advances the pictures as the clock says; [`Self::current_picture`]
    /// is then the picture to show.
    ///
    /// # Errors
    ///
    /// Returns the converter's output-shape error; on error nothing changes.
    pub fn pull(
        &mut self,
        output: &mut [&mut [f32]],
        at: f64,
    ) -> Result<BlockPull, LiveBlockError> {
        let queued = self.pictures_live.then_some(self.pictures.len());
        let pull = self.clock.pull(output, queued, at)?;
        let picture = if self.pictures_live {
            self.advance_pictures(pull.picture)
        } else {
            PictureShown::None
        };
        Ok(BlockPull {
            audio: pull.audio,
            picture,
        })
    }

    fn advance_pictures(&mut self, action: PictureAction) -> PictureShown {
        let skip = match action {
            PictureAction::Hold if !self.pictures.is_empty() => {
                self.late_run = 0;
                return PictureShown::Held;
            }
            PictureAction::Hold => 0,
            PictureAction::Advance { skip } => skip,
        };
        let skipped = skip.min(self.pictures.len().saturating_sub(1));
        self.pictures.drain(..skipped);
        if let Some(picture) = self.pictures.pop_front() {
            self.current = Some(picture);
            self.late_run = 0;
            return PictureShown::New { skipped };
        }
        self.counters.late_pictures = self.counters.late_pictures.saturating_add(1);
        self.late_run += 1;
        if self.late_run >= PICTURE_STALL_FRAMES {
            self.pictures_live = false;
            self.counters.picture_stalls = self.counters.picture_stalls.saturating_add(1);
        }
        PictureShown::Late
    }

    /// The picture to show (the last one taken), if any.
    #[must_use]
    pub const fn current_picture(&self) -> Option<&P> {
        self.current.as_ref()
    }

    /// Pictures waiting in the queue.
    #[must_use]
    pub fn queued_pictures(&self) -> usize {
        self.pictures.len()
    }

    /// Whether the clock follows the pictures (they are queued and not
    /// stalled).
    #[must_use]
    pub const fn pictures_live(&self) -> bool {
        self.pictures_live
    }

    /// The loop's telemetry, as a receiver reports it.
    #[must_use]
    pub fn live_telemetry(&self) -> LiveSourceTelemetry {
        self.clock.telemetry()
    }

    /// The loop's telemetry and the adapter's counters. Never allocates.
    #[must_use]
    pub fn telemetry(&self) -> LiveBlockTelemetry {
        LiveBlockTelemetry {
            live: self.clock.telemetry(),
            counters: self.counters,
        }
    }
}

/// Presentation end of a block, in nanoseconds.
fn end_nanos(timing: MediaTiming) -> i128 {
    i128::from(timing.presentation_timestamp().as_nanos())
        .saturating_add(i128::from(timing.duration().as_nanos()))
}

/// `numerator / denominator` rounded half away from zero.
fn rounded_div(numerator: i128, denominator: i128) -> i128 {
    let half = denominator / 2;
    if numerator >= 0 {
        numerator.saturating_add(half) / denominator
    } else {
        numerator.saturating_sub(half) / denominator
    }
}
