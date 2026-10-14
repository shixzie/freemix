//! Bounded raw audio/video recording through one direct `FFmpeg` child.
//!
//! The recorder authenticates independent loopback HTTP input streams with
//! random bearer paths. This prevents accidental or unauthenticated local
//! connections. It is not a defence against a local attacker: `/proc/<pid>/cmdline`
//! is world readable unless the operating system is configured otherwise, so
//! any local user can recover the paths while the child runs. Confining that is
//! an OS-level concern (`hidepid`, a container, or a dedicated user) and lies
//! outside this crate's isolation boundary. The
//! recorder owns only the direct child and does not create a process group.
//! Normal stop and cleanup paths are deadline-bounded. If the operating system
//! cannot create the final fallback cleanup thread, ownership is retained and
//! joined synchronously; only that thread-exhaustion fallback may exceed the
//! configured deadline. An optional linear-PCM WAV sidecar is written by the
//! same child, either to a path created exclusively before any child starts
//! (which the child reopens directly) or, with [`Recorder::start_with_sinks`],
//! to a [`RecordSink`] such as a [`ResilientWavOutput`].
//!
//! A WAV sink receives the child's second output through a third loopback
//! listener authenticated by its own random path: `FFmpeg` streams the WAV as
//! the raw body of one `PUT` request (`-method PUT -chunked_post 0`), which
//! needs no extra inherited file descriptor and works on every platform the
//! HTTP inputs already work on. Without seekable output `FFmpeg` leaves the
//! RIFF and data sizes as placeholders; the sink owns fixing them (the
//! resilient sink patches them on close, and crash recovery recomputes them).

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_frame::{AudioBlock, SequenceNumber};
use fm_types::{Channel, ChannelLayout, FrameRate, SampleRate, Timecode, VideoDimensions};

use crate::{Executable, UnavailableReason};

pub use crate::record_sink::{
    RecordSink, ResilientOutput, ResilientOutputState, ResilientOutputStatus, ResilientWavOutput,
    ResilientWavOutputState, ResilientWavOutputStatus,
};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_HTTP_HEADER_BYTES: usize = 8 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const TOKEN_BYTES: usize = 32;
/// `FFmpeg`'s floor for `-probesize`. Both raw inputs are fully specified on the
/// command line, so no input byte needs to be inspected to describe them.
pub(crate) const PROBE_SIZE: &str = "32";

/// Lowest accepted [`RecordConfig::video_bitrate_kbps`], in kbps.
pub const MIN_VIDEO_BITRATE_KBPS: u32 = 1_000;
/// Highest accepted [`RecordConfig::video_bitrate_kbps`], in kbps.
pub const MAX_VIDEO_BITRATE_KBPS: u32 = 100_000;
/// Lowest accepted [`RecordConfig::audio_bitrate_kbps`], in kbps.
pub const MIN_AUDIO_BITRATE_KBPS: u32 = 8;
/// Highest accepted [`RecordConfig::audio_bitrate_kbps`], in kbps.
pub const MAX_AUDIO_BITRATE_KBPS: u32 = 1_024;

/// Validated raw input format for one recording.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordFormat {
    dimensions: VideoDimensions,
    frame_rate: FrameRate,
    sample_rate: SampleRate,
    channel_layout: ChannelLayout,
    ffmpeg_channel_layout: &'static str,
    /// For each ffmpeg channel position, the source plane index; `None` when
    /// the `fm-types` order already equals ffmpeg's.
    ffmpeg_plane_order: Option<&'static [usize]>,
    first_sequence: SequenceNumber,
    rgba_bytes: usize,
}

impl RecordFormat {
    /// Creates a format compatible with tightly packed RGBA, yuv420p, and
    /// nonempty per-frame [`AudioBlock`] spans.
    ///
    /// # Errors
    ///
    /// Returns a typed dimension, cadence, layout, or byte-count error.
    pub fn new(
        width: u32,
        height: u32,
        frame_rate: FrameRate,
        sample_rate: SampleRate,
        channel_layout: ChannelLayout,
        first_sequence: SequenceNumber,
    ) -> Result<Self, FormatError> {
        let dimensions = VideoDimensions::new(width, height).ok_or(FormatError::ZeroDimension)?;
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(FormatError::OddYuv420Dimension);
        }
        if sample_rate.hertz() > AudioBlock::MAX_SAMPLE_RATE_HZ {
            return Err(FormatError::SampleRateTooHigh);
        }
        let cadence = u128::from(sample_rate.hertz()) * u128::from(frame_rate.denominator());
        let rate = u128::from(frame_rate.numerator());
        if cadence < rate {
            return Err(FormatError::EmptyAudioSpan);
        }
        if cadence.div_ceil(rate) > AudioBlock::MAX_SAMPLES_PER_CHANNEL as u128 {
            return Err(FormatError::AudioSpanTooLarge);
        }
        let rgba_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(FormatError::ByteCountOverflow)?;
        let FfmpegLayout {
            name: ffmpeg_channel_layout,
            plane_order: ffmpeg_plane_order,
        } = ffmpeg_layout(&channel_layout).ok_or(FormatError::UnsupportedChannelLayout)?;
        Ok(Self {
            dimensions,
            frame_rate,
            sample_rate,
            channel_layout,
            ffmpeg_channel_layout,
            ffmpeg_plane_order,
            first_sequence,
            rgba_bytes,
        })
    }

    #[must_use]
    pub const fn dimensions(&self) -> VideoDimensions {
        self.dimensions
    }

    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    #[must_use]
    pub const fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    #[must_use]
    pub const fn channel_layout(&self) -> &ChannelLayout {
        &self.channel_layout
    }

    #[must_use]
    pub const fn first_sequence(&self) -> SequenceNumber {
        self.first_sequence
    }

    #[must_use]
    pub const fn rgba_bytes_per_frame(&self) -> usize {
        self.rgba_bytes
    }

    fn expected_samples(&self, sequence: SequenceNumber) -> Result<usize, FrameError> {
        if sequence < self.first_sequence {
            return Err(FrameError::SequenceBeforeOrigin);
        }
        let start = sample_boundary(self, sequence.get())?;
        let end = sample_boundary(
            self,
            sequence
                .get()
                .checked_add(1)
                .ok_or(FrameError::TimingOverflow)?,
        )?;
        usize::try_from(end - start).map_err(|_| FrameError::TimingOverflow)
    }

    fn expected_timing(&self, sequence: SequenceNumber) -> Result<ExpectedAudioTiming, FrameError> {
        if sequence < self.first_sequence {
            return Err(FrameError::SequenceBeforeOrigin);
        }
        let start_sample = sample_boundary(self, sequence.get())?;
        let end_sample = sample_boundary(
            self,
            sequence
                .get()
                .checked_add(1)
                .ok_or(FrameError::TimingOverflow)?,
        )?;
        let sample_rate = u128::from(self.sample_rate.hertz());
        let start_nanos = start_sample
            .checked_mul(1_000_000_000)
            .map(|value| value / sample_rate)
            .ok_or(FrameError::TimingOverflow)?;
        let end_nanos = end_sample
            .checked_mul(1_000_000_000)
            .map(|value| value / sample_rate)
            .ok_or(FrameError::TimingOverflow)?;
        Ok(ExpectedAudioTiming {
            start_sample,
            start_nanos: i64::try_from(start_nanos).map_err(|_| FrameError::TimingOverflow)?,
            duration_nanos: u64::try_from(end_nanos - start_nanos)
                .map_err(|_| FrameError::TimingOverflow)?,
        })
    }

    /// The largest interleaved audio span one pair of this format can carry.
    pub(crate) fn maximum_audio_bytes(&self) -> Option<usize> {
        let samples = (u128::from(self.sample_rate.hertz())
            * u128::from(self.frame_rate.denominator()))
        .div_ceil(u128::from(self.frame_rate.numerator()));
        samples
            .checked_mul(self.channel_layout.channels().len() as u128)?
            .checked_mul(4)
            .and_then(|value| usize::try_from(value).ok())
    }

    /// The largest value [`PairedFrame::retained_bytes`] can report for a pair
    /// of this format, derived from the same accounting that method uses so a
    /// sink's byte budget cannot drift away from what a pair actually charges.
    pub(crate) fn maximum_pair_bytes(&self) -> Option<usize> {
        self.rgba_bytes
            .checked_add(self.maximum_audio_bytes()?)?
            .checked_add(pair_accounting_overhead(
                self.channel_layout.channels().len(),
            ))
    }
}

struct ExpectedAudioTiming {
    start_sample: u128,
    start_nanos: i64,
    duration_nanos: u64,
}

fn sample_boundary(format: &RecordFormat, frames: u64) -> Result<u128, FrameError> {
    u128::from(frames)
        .checked_mul(u128::from(format.sample_rate.hertz()))
        .and_then(|value| value.checked_mul(u128::from(format.frame_rate.denominator())))
        .map(|value| value / u128::from(format.frame_rate.numerator()))
        .ok_or(FrameError::TimingOverflow)
}

/// Format validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatError {
    ZeroDimension,
    OddYuv420Dimension,
    ByteCountOverflow,
    UnsupportedChannelLayout,
    SampleRateTooHigh,
    EmptyAudioSpan,
    AudioSpanTooLarge,
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid recording format: {self:?}")
    }
}

impl std::error::Error for FormatError {}

/// Bounded resource and cancellation policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordLimits {
    pub max_outstanding_pairs: usize,
    pub max_retained_bytes: usize,
    pub connect_timeout: Duration,
    pub no_progress_timeout: Duration,
    pub stop_timeout: Duration,
    pub kill_timeout: Duration,
    pub max_stderr_bytes: usize,
}

impl Default for RecordLimits {
    fn default() -> Self {
        Self {
            max_outstanding_pairs: 2,
            max_retained_bytes: 256 * 1024 * 1024,
            connect_timeout: Duration::from_secs(5),
            no_progress_timeout: Duration::from_secs(2),
            stop_timeout: Duration::from_secs(30),
            kill_timeout: Duration::from_secs(2),
            max_stderr_bytes: MAX_STDERR_BYTES,
        }
    }
}

/// Recorder startup configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordConfig {
    pub ffmpeg: Executable,
    pub format: RecordFormat,
    pub limits: RecordLimits,
    /// Names the exclusive linear-PCM WAV sidecar the child writes directly.
    ///
    /// The path must not already exist: [`Recorder::start`] creates it with a
    /// create-new open before any child starts and `FFmpeg` reopens the same
    /// path itself, so nothing there is ever overwritten. Parent directories
    /// are never created. The sidecar is only guaranteed finalized after a
    /// clean stop; a killed child may leave it empty or truncated.
    ///
    /// Must be `None` when the WAV goes to a sink through
    /// [`Recorder::start_with_sinks`].
    pub wav: Option<PathBuf>,
    /// Optional constant video bitrate in kbps, within
    /// [`MIN_VIDEO_BITRATE_KBPS`]`..=`[`MAX_VIDEO_BITRATE_KBPS`].
    ///
    /// `Some` adds `-b:v {kbps}k -maxrate {kbps}k -bufsize {2*kbps}k` to the
    /// `libx264` encoder arguments. `None` keeps the encoder's own default
    /// bitrate control and therefore today's output byte for byte.
    pub video_bitrate_kbps: Option<u32>,
    /// Optional audio bitrate in kbps, within
    /// [`MIN_AUDIO_BITRATE_KBPS`]`..=`[`MAX_AUDIO_BITRATE_KBPS`].
    ///
    /// `Some` adds `-b:a {kbps}k` to the AAC encoder arguments. `None` keeps
    /// the encoder's own default bitrate and therefore today's output.
    pub audio_bitrate_kbps: Option<u32>,
    /// Optional start timecode for the segment, in the project's nominal
    /// frame-rate domain.
    ///
    /// `Some` adds `-timecode {hours:02}:{minutes:02}:{seconds:02}:{frames:02}`
    /// to the MP4 output, immediately before `-f mp4 pipe:1`. `None` keeps the
    /// output byte for byte as it was before timecodes existed.
    pub timecode: Option<Timecode>,
}

impl RecordConfig {
    #[must_use]
    pub fn new(format: RecordFormat) -> Self {
        Self {
            ffmpeg: Executable::SearchPath,
            format,
            limits: RecordLimits::default(),
            wav: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            timecode: None,
        }
    }
}

/// Limits validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitsError {
    ZeroOutstandingPairs,
    ZeroRetainedBytes,
    ZeroTimeout,
    ZeroStderrBytes,
    StderrTooLarge,
    TimeoutOverflow,
    ByteCountOverflow,
    RetainedBytesTooSmall { required: usize, maximum: usize },
}

impl fmt::Display for LimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid recording limits: {self:?}")
    }
}

impl std::error::Error for LimitsError {}

fn validate_limits(format: &RecordFormat, limits: RecordLimits) -> Result<(), LimitsError> {
    if limits.max_outstanding_pairs == 0 {
        return Err(LimitsError::ZeroOutstandingPairs);
    }
    if limits.max_retained_bytes == 0 {
        return Err(LimitsError::ZeroRetainedBytes);
    }
    if limits.connect_timeout.is_zero()
        || limits.no_progress_timeout.is_zero()
        || limits.stop_timeout.is_zero()
        || limits.kill_timeout.is_zero()
    {
        return Err(LimitsError::ZeroTimeout);
    }
    if limits.max_stderr_bytes == 0 {
        return Err(LimitsError::ZeroStderrBytes);
    }
    if limits.max_stderr_bytes > MAX_STDERR_BYTES {
        return Err(LimitsError::StderrTooLarge);
    }
    let now = Instant::now();
    for timeout in [
        limits.connect_timeout,
        limits.no_progress_timeout,
        limits.stop_timeout,
        limits.kill_timeout,
    ] {
        if now.checked_add(timeout).is_none() {
            return Err(LimitsError::TimeoutOverflow);
        }
    }
    let required = format
        .maximum_pair_bytes()
        .and_then(|bytes| bytes.checked_mul(limits.max_outstanding_pairs))
        .ok_or(LimitsError::ByteCountOverflow)?;
    if required > limits.max_retained_bytes {
        return Err(LimitsError::RetainedBytesTooSmall {
            required,
            maximum: limits.max_retained_bytes,
        });
    }
    Ok(())
}

/// Refuses configured encoder bitrates outside their accepted envelopes before
/// any output, listener, or child exists.
fn validate_bitrates(config: &RecordConfig) -> Result<(), StartError> {
    if config
        .video_bitrate_kbps
        .is_some_and(|kbps| !(MIN_VIDEO_BITRATE_KBPS..=MAX_VIDEO_BITRATE_KBPS).contains(&kbps))
    {
        return Err(StartError::complete(StartErrorKind::InvalidBitrate));
    }
    if config
        .audio_bitrate_kbps
        .is_some_and(|kbps| !(MIN_AUDIO_BITRATE_KBPS..=MAX_AUDIO_BITRATE_KBPS).contains(&kbps))
    {
        return Err(StartError::complete(StartErrorKind::InvalidBitrate));
    }
    Ok(())
}

/// One owned validated video frame and matching sample-major audio span.
#[derive(Debug)]
pub struct PairedFrame {
    format: RecordFormat,
    sequence: SequenceNumber,
    rgba: Vec<u8>,
    audio_f32le: Vec<u8>,
}

impl PairedFrame {
    /// Validates and converts one paired engine frame.
    ///
    /// Audio boundaries are absolute engine-sequence boundaries:
    /// `floor(sequence * sample_rate * rate_denominator / rate_numerator)`.
    /// `first_sequence` is a lower bound, not a cadence-rebasing origin. The
    /// audio original timestamp must exactly represent the absolute start
    /// sample, and normalized PTS/duration must equal the floored nanosecond
    /// representations of the same sample endpoints.
    ///
    /// # Errors
    ///
    /// Rejects mismatched bytes, sequence, audio metadata, rational sample
    /// span, arithmetic overflow, or checked allocation failure.
    pub fn new(
        format: &RecordFormat,
        sequence: SequenceNumber,
        rgba: Vec<u8>,
        audio: AudioBlock,
    ) -> Result<Self, FrameError> {
        if rgba.len() != format.rgba_bytes {
            return Err(FrameError::RgbaLength {
                expected: format.rgba_bytes,
                actual: rgba.len(),
            });
        }
        if audio.timing().sequence() != sequence {
            return Err(FrameError::AudioSequence {
                expected: sequence,
                actual: audio.timing().sequence(),
            });
        }
        if audio.sample_rate() != format.sample_rate {
            return Err(FrameError::SampleRate);
        }
        if audio.channel_layout() != &format.channel_layout {
            return Err(FrameError::ChannelLayout);
        }
        let expected = format.expected_samples(sequence)?;
        if audio.sample_count() != expected {
            return Err(FrameError::SampleSpan {
                expected,
                actual: audio.sample_count(),
            });
        }
        let expected_timing = format.expected_timing(sequence)?;
        if !original_timestamp_matches(format, &audio, expected_timing.start_sample)? {
            return Err(FrameError::AudioOriginalTimestamp);
        }
        let actual_start = audio.timing().presentation_timestamp().as_nanos();
        if actual_start != expected_timing.start_nanos {
            return Err(FrameError::AudioPresentationTimestamp {
                expected: expected_timing.start_nanos,
                actual: actual_start,
            });
        }
        let actual_duration = audio.timing().duration().as_nanos();
        if actual_duration != expected_timing.duration_nanos {
            return Err(FrameError::AudioDuration {
                expected: expected_timing.duration_nanos,
                actual: actual_duration,
            });
        }
        let audio_f32le = interleave_f32le(&audio, format.ffmpeg_plane_order)?;
        drop(audio);
        // `retained_bytes` charges capacity; spare caller capacity would
        // exceed the `maximum_pair_bytes` budget limits were validated with.
        let mut rgba = rgba;
        rgba.shrink_to_fit();
        Ok(Self {
            format: format.clone(),
            sequence,
            rgba,
            audio_f32le,
        })
    }

    #[must_use]
    pub const fn sequence(&self) -> SequenceNumber {
        self.sequence
    }

    /// The exact format this pair was validated against.
    ///
    /// Sinks must compare this whole value rather than payload byte counts: a
    /// transposed frame and a relayout of the same audio both preserve the byte
    /// counts while describing completely different media.
    #[must_use]
    pub const fn format(&self) -> &RecordFormat {
        &self.format
    }

    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.rgba
            .capacity()
            .saturating_add(self.audio_f32le.capacity())
            .saturating_add(pair_accounting_overhead(
                self.format.channel_layout.channels().len(),
            ))
    }

    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn audio_f32le(&self) -> &[u8] {
        &self.audio_f32le
    }
}

/// Paired-frame validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    RgbaLength {
        expected: usize,
        actual: usize,
    },
    AudioSequence {
        expected: SequenceNumber,
        actual: SequenceNumber,
    },
    SampleRate,
    ChannelLayout,
    SequenceBeforeOrigin,
    SampleSpan {
        expected: usize,
        actual: usize,
    },
    AudioOriginalTimestamp,
    AudioPresentationTimestamp {
        expected: i64,
        actual: i64,
    },
    AudioDuration {
        expected: u64,
        actual: u64,
    },
    TimingOverflow,
    ByteCountOverflow,
    AllocationFailed,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid paired recording frame: {self:?}")
    }
}

impl std::error::Error for FrameError {}

fn original_timestamp_matches(
    format: &RecordFormat,
    audio: &AudioBlock,
    expected_start_sample: u128,
) -> Result<bool, FrameError> {
    let original = audio.timing().original_timestamp();
    let ticks = original.timestamp().ticks();
    let ticks = u128::try_from(ticks).map_err(|_| FrameError::AudioOriginalTimestamp)?;
    let time_base = original.time_base();
    let actual = ticks
        .checked_mul(u128::from(time_base.numerator()))
        .and_then(|value| value.checked_mul(u128::from(format.sample_rate.hertz())))
        .ok_or(FrameError::TimingOverflow)?;
    let expected = expected_start_sample
        .checked_mul(u128::from(time_base.denominator()))
        .ok_or(FrameError::TimingOverflow)?;
    Ok(actual == expected)
}

/// Interleaves planes as `f32le`, in ffmpeg channel order when `plane_order`
/// is given (one source plane index per ffmpeg channel position).
fn interleave_f32le(
    audio: &AudioBlock,
    plane_order: Option<&'static [usize]>,
) -> Result<Vec<u8>, FrameError> {
    let byte_count = audio
        .sample_count()
        .checked_mul(audio.channel_layout().channels().len())
        .and_then(|samples| samples.checked_mul(size_of::<f32>()))
        .ok_or(FrameError::ByteCountOverflow)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(byte_count)
        .map_err(|_| FrameError::AllocationFailed)?;
    let planes = audio.planes();
    for sample in 0..audio.sample_count() {
        match plane_order {
            Some(order) => {
                for index in order {
                    output.extend_from_slice(&planes[*index][sample].to_le_bytes());
                }
            }
            None => {
                for plane in planes {
                    output.extend_from_slice(&plane[sample].to_le_bytes());
                }
            }
        }
    }
    Ok(output)
}

/// An ffmpeg channel layout descriptor for the raw `f32le` audio input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FfmpegLayout {
    name: &'static str,
    plane_order: Option<&'static [usize]>,
}

impl FfmpegLayout {
    const fn native(name: &'static str) -> Self {
        Self {
            name,
            plane_order: None,
        }
    }
}

/// `{n}C`: ffmpeg's unspecified-order layout of `n` channels.
const UNSPECIFIED_LAYOUTS: [&str; ChannelLayout::MAX_CHANNELS] = [
    "1C", "2C", "3C", "4C", "5C", "6C", "7C", "8C", "9C", "10C", "11C", "12C", "13C", "14C", "15C",
    "16C",
];

/// SMPTE/ITU 7.1 (`L R C LFE Ls Rs Lrs Rrs`) planes in ffmpeg `7.1` order
/// (`FL FR FC LFE BL BR SL SR`).
const SMPTE_7_1_TO_FFMPEG: [usize; 8] = [0, 1, 2, 3, 6, 7, 4, 5];

/// Maps a semantic layout to the ffmpeg layout that names exactly the same
/// speakers.
///
/// `LeftSurround`/`RightSurround` are ffmpeg side channels (`SL`/`SR`) and
/// `LeftRearSurround`/`RightRearSurround` are ffmpeg back channels
/// (`BL`/`BR`). ffmpeg's plain `quad`, `5.0`, and `5.1` use `BL`/`BR`, so they
/// carry rear labels, while the `(side)` variants carry `Ls`/`Rs`. ffmpeg's
/// `7.1` is `FL FR FC LFE BL BR SL SR`: that order is accepted directly, and
/// the SMPTE/ITU order `L R C LFE Ls Rs Lrs Rrs` (the `fm-audio` 7.1 catalogue
/// order) is accepted and reordered while interleaving. A layout made only of
/// discrete channels is passed in its own order as `{n}C`, ffmpeg's
/// unspecified-order layout, so no speaker positions are invented. Mixed
/// named/discrete and other layouts are refused.
fn ffmpeg_layout(layout: &ChannelLayout) -> Option<FfmpegLayout> {
    use Channel::{
        Center as C, Left as L, LeftRearSurround as Lrs, LeftSurround as Ls, LowFrequency as Lfe,
        Right as R, RightRearSurround as Rrs, RightSurround as Rs,
    };
    let channels = layout.channels();
    if channels
        .iter()
        .all(|channel| matches!(channel, Channel::Discrete(_)))
    {
        return Some(FfmpegLayout::native(
            UNSPECIFIED_LAYOUTS[channels.len() - 1],
        ));
    }
    let name = match channels {
        [Channel::Mono] => "mono",
        [L, R] => "stereo",
        [L, R, Lfe] => "2.1",
        [L, R, C] => "3.0",
        [L, R, C, Lfe] => "3.1",
        [L, R, Lrs, Rrs] => "quad",
        [L, R, Ls, Rs] => "quad(side)",
        [L, R, C, Lrs, Rrs] => "5.0",
        [L, R, C, Ls, Rs] => "5.0(side)",
        [L, R, C, Lfe, Lrs, Rrs] => "5.1",
        [L, R, C, Lfe, Ls, Rs] => "5.1(side)",
        [L, R, C, Lrs, Rrs, Ls, Rs] => "7.0",
        [L, R, C, Lfe, Lrs, Rrs, Ls, Rs] => "7.1",
        [L, R, C, Lfe, Ls, Rs, Lrs, Rrs] => {
            return Some(FfmpegLayout {
                name: "7.1",
                plane_order: Some(&SMPTE_7_1_TO_FFMPEG),
            });
        }
        _ => return None,
    };
    Some(FfmpegLayout::native(name))
}

/// Raw input involved in a typed failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaInput {
    Video,
    Audio,
}

/// Whether all fallback cleanup was observed before a bounded return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupStatus {
    Complete,
    /// Background reaping, worker completion, or output finalization may still
    /// be running. The frozen report will not change.
    Unconfirmed,
}

/// Path-free startup failure category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartErrorKind {
    InvalidLimits(LimitsError),
    /// A configured video or audio bitrate is outside its documented envelope.
    InvalidBitrate,
    InvalidExecutable,
    ToolUnavailable(UnavailableReason),
    Randomness,
    ThreadSpawn(io::ErrorKind),
    OutputMetadata(io::ErrorKind),
    OutputNotEmpty {
        bytes: u64,
    },
    OutputPosition(io::ErrorKind),
    WavOutput(io::ErrorKind),
    Bind {
        input: MediaInput,
        kind: io::ErrorKind,
    },
    Spawn(io::ErrorKind),
    MissingPipe,
    ConnectTimeout {
        input: MediaInput,
    },
    Connect {
        input: MediaInput,
        kind: io::ErrorKind,
    },
    EarlyExit {
        status: Option<i32>,
        stderr: String,
    },
}

/// Startup failure plus an explicit bounded-cleanup result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartError {
    pub kind: StartErrorKind,
    pub cleanup: CleanupStatus,
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "FFmpeg recorder startup failed: {:?} ({:?} cleanup)",
            self.kind, self.cleanup
        )
    }
}

impl std::error::Error for StartError {}

impl StartError {
    const fn complete(kind: StartErrorKind) -> Self {
        Self {
            kind,
            cleanup: CleanupStatus::Complete,
        }
    }
}

/// First terminal runtime failure. This value is sticky.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordFailure {
    Cancelled,
    ChildExited {
        status: Option<i32>,
    },
    ConnectTimeout(MediaInput),
    Connect {
        input: MediaInput,
        kind: io::ErrorKind,
    },
    VideoWrite(io::ErrorKind),
    AudioWrite(io::ErrorKind),
    OutputRead(io::ErrorKind),
    OutputWrite(io::ErrorKind),
    OutputFlush(io::ErrorKind),
    OutputSync(io::ErrorKind),
    /// The WAV sink refused bytes, failed to flush or finish, the child's
    /// WAV upload broke off, or the child never delivered its WAV output.
    WavOutput(io::ErrorKind),
    StderrRead(io::ErrorKind),
    DispatcherClosed(MediaInput),
    WorkerPanicked,
    StopTimedOut,
    KillTimedOut,
    CleanupUnconfirmed,
}

/// Recorder lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderState {
    Starting,
    Recording,
    Stopping,
    Stopped,
    Failed,
}

/// Stable point-in-time telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordTelemetry {
    pub state: RecorderState,
    pub failure: Option<RecordFailure>,
    pub accepted_pairs: u64,
    pub completed_pairs: u64,
    pub failed_pairs: u64,
    pub outstanding_pairs: usize,
    pub retained_bytes: usize,
    pub output_bytes: u64,
    /// Bytes written to the WAV sidecar, measured once from the sidecar's
    /// metadata when [`Recorder::stop`] freezes the report, or, for a WAV
    /// sink, the stream bytes the sink accepted. Zero while a recorder is
    /// running, when no sidecar is configured, or when the metadata cannot be
    /// read.
    ///
    /// The sidecar is only guaranteed finalized after a clean stop; a killed
    /// child may leave it empty or truncated.
    pub wav_bytes: u64,
    pub stderr_tail: String,
    pub stderr_truncated: bool,
}

/// Why a nonblocking enqueue was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnqueueRejection {
    QueueFull,
    RetainedByteLimit,
    FormatMismatch,
    Sequence {
        expected: SequenceNumber,
        actual: SequenceNumber,
    },
    SequenceExhausted,
    Stopping,
    Stopped,
    Failed(RecordFailure),
}

/// Nonblocking rejection retaining ownership of the pair.
#[derive(Debug)]
pub struct EnqueueError {
    pub reason: EnqueueRejection,
    pub frame: Box<PairedFrame>,
}

impl fmt::Display for EnqueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "recording enqueue rejected: {:?}", self.reason)
    }
}

impl std::error::Error for EnqueueError {}

impl EnqueueError {
    #[must_use]
    pub fn into_frame(self) -> PairedFrame {
        *self.frame
    }
}

/// How output finalization was observed before stop returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFinalization {
    Synced,
    Failed(io::ErrorKind),
    /// The output worker owns the file and may still flush or sync it in the
    /// explicitly reported fallback cleanup.
    Unconfirmed,
}

/// How stop completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopOutcome {
    Clean,
    Failed,
    Killed,
    KillTimedOut,
}

/// Frozen, idempotent stop result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopReport {
    pub outcome: StopOutcome,
    pub exit_status: Option<i32>,
    pub output: OutputFinalization,
    pub cleanup: CleanupStatus,
    /// Bytes in the WAV sidecar at stop; the same value as
    /// [`RecordTelemetry::wav_bytes`].
    pub wav_bytes: u64,
    pub telemetry: RecordTelemetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownMode {
    Drain,
    Cancel,
}

#[derive(Clone, Copy)]
struct Shutdown {
    mode: ShutdownMode,
    deadline: Instant,
}

#[derive(Clone, Copy)]
struct ChildResult {
    success: bool,
    code: Option<i32>,
}

struct Shared {
    data: Mutex<SharedData>,
    kill_requested: AtomicBool,
    max_stderr: usize,
    redactions: Vec<String>,
    /// Stream bytes a WAV sink accepted.
    wav_sink_bytes: AtomicU64,
}

struct SharedData {
    state: RecorderState,
    failure: Option<RecordFailure>,
    frozen: bool,
    next_sequence: SequenceNumber,
    accepted: u64,
    completed: u64,
    failed_pairs: u64,
    outstanding: usize,
    retained_bytes: usize,
    output_bytes: u64,
    wav_bytes: u64,
    stderr: VecDeque<u8>,
    stderr_truncated: bool,
    child: Option<ChildResult>,
    shutdown: Option<Shutdown>,
}

impl Shared {
    fn new(format: &RecordFormat, max_stderr: usize, redactions: impl Into<Vec<String>>) -> Self {
        Self {
            data: Mutex::new(SharedData {
                state: RecorderState::Starting,
                failure: None,
                frozen: false,
                next_sequence: format.first_sequence,
                accepted: 0,
                completed: 0,
                failed_pairs: 0,
                outstanding: 0,
                retained_bytes: 0,
                output_bytes: 0,
                wav_bytes: 0,
                stderr: VecDeque::with_capacity(max_stderr.min(8 * 1024)),
                stderr_truncated: false,
                child: None,
                shutdown: None,
            }),
            kill_requested: AtomicBool::new(false),
            max_stderr,
            redactions: redactions.into(),
            wav_sink_bytes: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SharedData> {
        self.data.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fail(&self, failure: RecordFailure) {
        let mut data = self.lock();
        fail_locked(&mut data, failure);
    }

    fn append_stderr(&self, bytes: &[u8]) {
        let mut data = self.lock();
        if data.frozen {
            return;
        }
        for &byte in bytes {
            if data.stderr.len() == self.max_stderr {
                data.stderr.pop_front();
                data.stderr_truncated = true;
            }
            data.stderr.push_back(byte);
        }
    }

    fn add_output_bytes(&self, bytes: usize) {
        let mut data = self.lock();
        if !data.frozen {
            data.output_bytes = data.output_bytes.saturating_add(bytes as u64);
        }
    }

    fn record_child(&self, status: ExitStatus) {
        let mut data = self.lock();
        if data.child.is_some() {
            return;
        }
        if self.kill_requested.load(Ordering::Acquire) {
            if data.shutdown.is_none() {
                data.shutdown = Some(Shutdown {
                    mode: ShutdownMode::Cancel,
                    deadline: Instant::now(),
                });
            }
            fail_locked(&mut data, RecordFailure::Cancelled);
        }
        let result = ChildResult {
            success: status.success(),
            code: status.code(),
        };
        data.child = Some(result);
        if data.shutdown.is_none() && (!result.success || data.state == RecorderState::Recording) {
            fail_locked(
                &mut data,
                RecordFailure::ChildExited {
                    status: result.code,
                },
            );
        }
    }

    fn snapshot(&self) -> RecordTelemetry {
        telemetry_locked(&self.lock(), &self.redactions)
    }
}

fn fail_locked(data: &mut SharedData, failure: RecordFailure) {
    if !data.frozen && data.failure.is_none() {
        data.failure = Some(failure);
        data.state = RecorderState::Failed;
    }
}

fn telemetry_locked(data: &SharedData, redactions: &[String]) -> RecordTelemetry {
    let stderr = data.stderr.iter().copied().collect::<Vec<_>>();
    let mut stderr_tail = clean_tail(&stderr);
    for token in redactions.iter().filter(|token| !token.is_empty()) {
        stderr_tail = stderr_tail.replace(token, "<input-token>");
    }
    RecordTelemetry {
        state: data.state,
        failure: data.failure.clone(),
        accepted_pairs: data.accepted,
        completed_pairs: data.completed,
        failed_pairs: data.failed_pairs,
        outstanding_pairs: data.outstanding,
        retained_bytes: data.retained_bytes,
        output_bytes: data.output_bytes,
        wav_bytes: data.wav_bytes,
        stderr_tail,
        stderr_truncated: data.stderr_truncated,
    }
}

fn clean_tail(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    text.retain(|character| character == '\n' || character == '\t' || !character.is_control());
    text.trim().to_owned()
}

struct Completion {
    shared: Arc<Shared>,
    bytes: usize,
    remaining: AtomicUsize,
    successful: AtomicBool,
}

impl Completion {
    fn finish_part(&self, successful: bool) {
        if !successful {
            self.successful.store(false, Ordering::Release);
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut data = self.shared.lock();
            if data.frozen {
                return;
            }
            data.outstanding = data.outstanding.saturating_sub(1);
            data.retained_bytes = data.retained_bytes.saturating_sub(self.bytes);
            if self.successful.load(Ordering::Acquire) {
                data.completed = data.completed.saturating_add(1);
            } else {
                data.failed_pairs = data.failed_pairs.saturating_add(1);
            }
        }
    }
}

struct WriteJob {
    bytes: Vec<u8>,
    completion: Option<Arc<Completion>>,
}

impl WriteJob {
    fn complete(mut self, successful: bool) {
        if let Some(completion) = self.completion.take() {
            completion.finish_part(successful);
        }
    }
}

impl Drop for WriteJob {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.finish_part(false);
        }
    }
}

struct PairJob {
    frame: PairedFrame,
    completion: Arc<Completion>,
}

/// Private per-pair bookkeeping charged on top of the two payload allocations.
/// Derived from the types that actually hold a queued pair so that adding a
/// field to [`PairedFrame`] or [`Completion`] cannot silently invalidate the
/// byte budget the limits were validated against.
fn pair_accounting_overhead(channels: usize) -> usize {
    size_of::<PairJob>()
        .saturating_add(size_of::<Completion>())
        .saturating_add(2 * size_of::<AtomicUsize>())
        .saturating_add(channels.saturating_mul(size_of::<Channel>()))
        .saturating_add(4 * size_of::<usize>())
}

struct Endpoint {
    listener: TcpListener,
    path: String,
    input: MediaInput,
}

struct OutputResult {
    finalization: OutputFinalization,
}

enum CleanupWorker {
    Unit(JoinHandle<()>),
    Output(JoinHandle<OutputResult>),
}

/// Long-lived bounded raw A/V recorder.
pub struct Recorder {
    format: RecordFormat,
    limits: RecordLimits,
    wav: Option<PathBuf>,
    /// The WAV goes to a sink; telemetry counts the bytes it accepted.
    wav_sink: bool,
    sender: Option<mpsc::SyncSender<PairJob>>,
    shared: Arc<Shared>,
    child: Arc<Mutex<Child>>,
    workers: Vec<JoinHandle<()>>,
    output_worker: Option<JoinHandle<OutputResult>>,
    monitor: Option<JoinHandle<()>>,
    report: Option<StopReport>,
}

impl Recorder {
    /// Consumes an empty output file, binds authenticated loopback HTTP inputs,
    /// starts `FFmpeg`, and validates its first input request.
    ///
    /// The file must be empty. Its position is normalized to zero before any
    /// process starts, and no other public handle is retained by this API.
    ///
    /// When [`RecordConfig::wav`] names a path, that path must not exist: it is
    /// created exclusively before any child starts and the same child reopens
    /// it to write linear PCM directly. A failed start removes it again.
    ///
    /// # Errors
    ///
    /// Returns a typed path-free error. [`StartError::cleanup`] explicitly says
    /// whether fallback output finalization or child reaping may continue.
    pub fn start(output: File, config: RecordConfig) -> Result<Self, StartError> {
        Self::start_output(PendingOutput::File(output), None, config)
    }

    /// Like [`Self::start`], but copies the muxed fragmented MP4 stream into
    /// `sink` instead of a file. Use a [`ResilientOutput`] for a
    /// fault-tolerant, multi-directory, crash-recoverable recording session.
    ///
    /// The sink receives nothing before `FFmpeg` produces output, and
    /// [`RecordSink::finish`] runs exactly once when the output ends. A start
    /// that fails before the output worker exists drops the sink.
    ///
    /// # Errors
    ///
    /// The same typed startup failures as [`Self::start`], except the
    /// empty-file checks, which a sink does not need.
    pub fn start_with_sink(
        sink: Box<dyn RecordSink>,
        config: RecordConfig,
    ) -> Result<Self, StartError> {
        Self::start_output(PendingOutput::Sink(sink), None, config)
    }

    /// Like [`Self::start_with_sink`], and additionally copies the child's
    /// linear-PCM WAV output into `wav` instead of a sidecar path. Use a
    /// [`ResilientWavOutput`] for a fault-tolerant, multi-directory,
    /// crash-recoverable WAV session.
    ///
    /// The WAV arrives over an authenticated loopback `PUT` whose raw body is
    /// the WAV stream byte for byte, with placeholder RIFF and data sizes.
    /// [`RecordSink::finish`] runs exactly once when that stream ends (or
    /// when the child exits without ever delivering it), also after a sink
    /// error. A sink failure, a broken upload, or a missing upload is
    /// reported as [`RecordFailure::WavOutput`]. A start that fails before
    /// the WAV worker exists drops `wav`.
    ///
    /// # Errors
    ///
    /// The same typed startup failures as [`Self::start_with_sink`], plus
    /// [`StartErrorKind::WavOutput`] with [`io::ErrorKind::InvalidInput`]
    /// when [`RecordConfig::wav`] also names a path, or with the listener
    /// error when the WAV listener cannot be bound.
    pub fn start_with_sinks(
        output: Box<dyn RecordSink>,
        wav: Box<dyn RecordSink>,
        config: RecordConfig,
    ) -> Result<Self, StartError> {
        Self::start_output(PendingOutput::Sink(output), Some(wav), config)
    }

    #[allow(clippy::too_many_lines)]
    fn start_output(
        output: PendingOutput,
        wav_sink: Option<Box<dyn RecordSink>>,
        config: RecordConfig,
    ) -> Result<Self, StartError> {
        validate_limits(&config.format, config.limits)
            .map_err(|error| StartError::complete(StartErrorKind::InvalidLimits(error)))?;
        validate_bitrates(&config)?;
        if wav_sink.is_some() && config.wav.is_some() {
            return Err(StartError::complete(StartErrorKind::WavOutput(
                io::ErrorKind::InvalidInput,
            )));
        }
        let executable = recorder_executable(config.ffmpeg)?;
        let output: Box<dyn RecordSink> = match output {
            PendingOutput::File(file) => Box::new(prepare_output(file)?),
            PendingOutput::Sink(sink) => sink,
        };
        prepare_wav(config.wav.as_deref())?;
        // Every later startup failure removes the sidecar created above, so
        // a retry is not refused as `AlreadyExists`.
        let mut created_wav = CreatedWav(config.wav.clone());
        let video_token = random_token()?;
        let audio_token = random_token()?;
        let video_listener = bind_listener(MediaInput::Video)?;
        let audio_listener = bind_listener(MediaInput::Audio)?;
        let video_address = listener_address(&video_listener, MediaInput::Video)?;
        let audio_address = listener_address(&audio_listener, MediaInput::Audio)?;
        let wav_upload = match wav_sink {
            Some(sink) => {
                let token = random_token()?;
                let (listener, address) = bind_wav_listener()?;
                Some((sink, listener, address, token))
            }
            None => None,
        };
        let wav_destination = match (&wav_upload, config.wav.as_deref()) {
            (Some((_, _, address, token)), _) => Some(WavDestination::Upload {
                address: *address,
                token,
            }),
            (None, Some(path)) => Some(WavDestination::File(path)),
            (None, None) => None,
        };
        let args = command_args(
            &config.format,
            video_address,
            audio_address,
            &video_token,
            &audio_token,
            wav_destination,
            config.video_bitrate_kbps,
            config.audio_bitrate_kbps,
            config.timecode,
        );
        let mut redactions = vec![video_token.clone(), audio_token.clone()];
        if let Some((_, _, _, token)) = &wav_upload {
            redactions.push(token.clone());
        }
        let shared = Arc::new(Shared::new(
            &config.format,
            config.limits.max_stderr_bytes,
            redactions,
        ));
        let (monitor_sender, monitor) = spawn_monitor_waiter()?;
        let mut command = command(&executable, &args);
        let child = match command.spawn() {
            Ok(child) => Arc::new(Mutex::new(child)),
            Err(error) => {
                drop(monitor_sender);
                let _ = monitor.join();
                return Err(StartError::complete(spawn_error(&error)));
            }
        };
        if monitor_sender
            .send((Arc::clone(&child), Arc::clone(&shared)))
            .is_err()
        {
            let cleanup = terminate_shared_without_monitor(&child, config.limits.kill_timeout);
            return Err(StartError {
                kind: StartErrorKind::ThreadSpawn(io::ErrorKind::Other),
                cleanup,
            });
        }
        drop(monitor_sender);
        let (stdout, stderr) = {
            let mut child_guard = child.lock().unwrap_or_else(PoisonError::into_inner);
            (child_guard.stdout.take(), child_guard.stderr.take())
        };
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            return Err(cleanup_missing_pipe(
                &child,
                &shared,
                monitor,
                config.limits.kill_timeout,
            ));
        };

        let output_worker = match spawn_output(stdout, output, Arc::clone(&shared)) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(cleanup_startup(
                    StartErrorKind::ThreadSpawn(error.kind()),
                    &child,
                    &shared,
                    Vec::new(),
                    None,
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        };
        let stderr_worker = match spawn_stderr(stderr, Arc::clone(&shared)) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(cleanup_startup(
                    StartErrorKind::ThreadSpawn(error.kind()),
                    &child,
                    &shared,
                    Vec::new(),
                    Some(output_worker),
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        };
        let mut workers = vec![stderr_worker];
        let wav_sink = wav_upload.is_some();
        if let Some((sink, listener, _, token)) = wav_upload {
            let endpoint = WavEndpoint {
                listener,
                path: format!("/{token}"),
            };
            match spawn_wav_output(endpoint, sink, config.limits, Arc::clone(&shared)) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    return Err(cleanup_startup(
                        StartErrorKind::ThreadSpawn(error.kind()),
                        &child,
                        &shared,
                        workers,
                        Some(output_worker),
                        monitor,
                        config.limits.kill_timeout,
                    ));
                }
            }
        }
        let deadline = Instant::now() + config.limits.connect_timeout;
        let mut video = Endpoint {
            listener: video_listener,
            path: format!("/{video_token}"),
            input: MediaInput::Video,
        };
        let mut audio = Endpoint {
            listener: audio_listener,
            path: format!("/{audio_token}"),
            input: MediaInput::Audio,
        };
        let first = wait_for_first_http(
            &mut video,
            &mut audio,
            deadline,
            config.limits.no_progress_timeout,
            &shared,
        );
        let (first_input, first_stream) = match first {
            Ok(value) => value,
            Err(kind) => {
                return Err(cleanup_startup(
                    kind,
                    &child,
                    &shared,
                    workers,
                    Some(output_worker),
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        };

        {
            let mut data = shared.lock();
            if let Some(failure) = &data.failure {
                let kind = match failure {
                    RecordFailure::ChildExited { status } => StartErrorKind::EarlyExit {
                        status: *status,
                        stderr: telemetry_locked(&data, &shared.redactions).stderr_tail,
                    },
                    _ => StartErrorKind::EarlyExit {
                        status: data.child.and_then(|child| child.code),
                        stderr: telemetry_locked(&data, &shared.redactions).stderr_tail,
                    },
                };
                drop(data);
                return Err(cleanup_startup(
                    kind,
                    &child,
                    &shared,
                    workers,
                    Some(output_worker),
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
            data.state = RecorderState::Recording;
        }

        let (pair_sender, pair_receiver) = mpsc::sync_channel(config.limits.max_outstanding_pairs);
        let (video_sender, video_receiver) =
            mpsc::sync_channel(config.limits.max_outstanding_pairs);
        let (audio_sender, audio_receiver) =
            mpsc::sync_channel(config.limits.max_outstanding_pairs);
        let final_media_worker = match first_input {
            MediaInput::Video => {
                let video_worker =
                    match spawn_writer(video, first_stream, video_receiver, Arc::clone(&shared)) {
                        Ok(worker) => worker,
                        Err(error) => {
                            return Err(cleanup_startup(
                                StartErrorKind::ThreadSpawn(error.kind()),
                                &child,
                                &shared,
                                workers,
                                Some(output_worker),
                                monitor,
                                config.limits.kill_timeout,
                            ));
                        }
                    };
                workers.push(video_worker);
                match spawn_pending_writer(
                    audio,
                    audio_receiver,
                    config.limits,
                    Arc::clone(&shared),
                ) {
                    Ok(worker) => worker,
                    Err(error) => {
                        return Err(cleanup_startup(
                            StartErrorKind::ThreadSpawn(error.kind()),
                            &child,
                            &shared,
                            workers,
                            Some(output_worker),
                            monitor,
                            config.limits.kill_timeout,
                        ));
                    }
                }
            }
            MediaInput::Audio => {
                let video_worker = match spawn_pending_writer(
                    video,
                    video_receiver,
                    config.limits,
                    Arc::clone(&shared),
                ) {
                    Ok(worker) => worker,
                    Err(error) => {
                        return Err(cleanup_startup(
                            StartErrorKind::ThreadSpawn(error.kind()),
                            &child,
                            &shared,
                            workers,
                            Some(output_worker),
                            monitor,
                            config.limits.kill_timeout,
                        ));
                    }
                };
                workers.push(video_worker);
                match spawn_writer(audio, first_stream, audio_receiver, Arc::clone(&shared)) {
                    Ok(worker) => worker,
                    Err(error) => {
                        return Err(cleanup_startup(
                            StartErrorKind::ThreadSpawn(error.kind()),
                            &child,
                            &shared,
                            workers,
                            Some(output_worker),
                            monitor,
                            config.limits.kill_timeout,
                        ));
                    }
                }
            }
        };
        workers.push(final_media_worker);
        let dispatcher = match spawn_dispatcher(
            pair_receiver,
            video_sender,
            audio_sender,
            Arc::clone(&shared),
        ) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(cleanup_startup(
                    StartErrorKind::ThreadSpawn(error.kind()),
                    &child,
                    &shared,
                    workers,
                    Some(output_worker),
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        };
        workers.push(dispatcher);
        created_wav.0 = None;
        Ok(Self {
            format: config.format,
            limits: config.limits,
            wav: config.wav,
            wav_sink,
            sender: Some(pair_sender),
            shared,
            child,
            workers,
            output_worker: Some(output_worker),
            monitor: Some(monitor),
            report: None,
        })
    }

    /// Attempts to accept an entire pair without blocking.
    ///
    /// Admission, sticky failure observation, pair/byte reservation, queueing,
    /// and sequence commit linearize under one shared state lock.
    ///
    /// # Errors
    ///
    /// Returns the original frame on every rejection.
    pub fn enqueue(&mut self, frame: PairedFrame) -> Result<(), EnqueueError> {
        let mut data = self.shared.lock();
        let reject = |reason, frame| EnqueueError {
            reason,
            frame: Box::new(frame),
        };
        if self.shared.kill_requested.load(Ordering::Acquire) {
            fail_locked(&mut data, RecordFailure::Cancelled);
        }
        if let Some(failure) = &data.failure {
            return Err(reject(EnqueueRejection::Failed(failure.clone()), frame));
        }
        match data.state {
            RecorderState::Recording => {}
            RecorderState::Starting | RecorderState::Stopping => {
                return Err(reject(EnqueueRejection::Stopping, frame));
            }
            RecorderState::Stopped => return Err(reject(EnqueueRejection::Stopped, frame)),
            RecorderState::Failed => {
                return Err(reject(
                    EnqueueRejection::Failed(RecordFailure::CleanupUnconfirmed),
                    frame,
                ));
            }
        }
        if frame.format != self.format {
            return Err(reject(EnqueueRejection::FormatMismatch, frame));
        }
        if frame.sequence != data.next_sequence {
            return Err(reject(
                EnqueueRejection::Sequence {
                    expected: data.next_sequence,
                    actual: frame.sequence,
                },
                frame,
            ));
        }
        let Some(next) = data.next_sequence.checked_next() else {
            return Err(reject(EnqueueRejection::SequenceExhausted, frame));
        };
        let bytes = frame.retained_bytes();
        if data.outstanding >= self.limits.max_outstanding_pairs {
            return Err(reject(EnqueueRejection::QueueFull, frame));
        }
        if data
            .retained_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.limits.max_retained_bytes)
        {
            return Err(reject(EnqueueRejection::RetainedByteLimit, frame));
        }
        let completion = Arc::new(Completion {
            shared: Arc::clone(&self.shared),
            bytes,
            remaining: AtomicUsize::new(2),
            successful: AtomicBool::new(true),
        });
        let Some(sender) = self.sender.as_ref() else {
            return Err(reject(EnqueueRejection::Stopping, frame));
        };
        match sender.try_send(PairJob { frame, completion }) {
            Ok(()) => {
                data.next_sequence = next;
                data.outstanding += 1;
                data.retained_bytes += bytes;
                data.accepted = data.accepted.saturating_add(1);
                Ok(())
            }
            Err(mpsc::TrySendError::Full(job)) => {
                Err(reject(EnqueueRejection::QueueFull, job.frame))
            }
            Err(mpsc::TrySendError::Disconnected(job)) => {
                let failure = RecordFailure::DispatcherClosed(MediaInput::Video);
                fail_locked(&mut data, failure.clone());
                Err(reject(EnqueueRejection::Failed(failure), job.frame))
            }
        }
    }

    #[must_use]
    pub fn telemetry(&self) -> RecordTelemetry {
        self.report
            .as_ref()
            .map_or_else(|| self.shared.snapshot(), |report| report.telemetry.clone())
    }

    /// Drains accepted frames and finalizes output within the stop and kill
    /// budgets. Repeated calls return the same frozen report.
    #[must_use]
    pub fn stop(&mut self) -> StopReport {
        self.stop_inner(ShutdownMode::Drain)
    }

    /// Atomically rejects future enqueue attempts, closes the accepted-pair
    /// sender, and requests direct-child termination without waiting for any
    /// worker, child status, output flush, or output sync.
    pub fn request_cancel(&mut self) {
        if self.report.is_some() {
            return;
        }
        self.shared.kill_requested.store(true, Ordering::Release);
        self.sender.take();
        match self.shared.data.try_lock() {
            Ok(mut data) => {
                if !data
                    .shutdown
                    .is_some_and(|shutdown| shutdown.mode == ShutdownMode::Cancel)
                {
                    data.shutdown = Some(Shutdown {
                        mode: ShutdownMode::Cancel,
                        deadline: Instant::now() + self.limits.kill_timeout,
                    });
                    fail_locked(&mut data, RecordFailure::Cancelled);
                }
            }
            Err(TryLockError::Poisoned(error)) => {
                let mut data = error.into_inner();
                data.shutdown = Some(Shutdown {
                    mode: ShutdownMode::Cancel,
                    deadline: Instant::now() + self.limits.kill_timeout,
                });
                fail_locked(&mut data, RecordFailure::Cancelled);
            }
            Err(TryLockError::WouldBlock) => {}
        }
        try_kill_child(&self.child);
    }

    /// Kills the direct child immediately and finalizes the partial prefix when
    /// the output worker can do so within the kill budget.
    #[must_use]
    pub fn cancel(&mut self) -> StopReport {
        self.request_cancel();
        self.stop_inner(ShutdownMode::Cancel)
    }

    #[allow(clippy::too_many_lines)]
    fn stop_inner(&mut self, mode: ShutdownMode) -> StopReport {
        if let Some(report) = &self.report {
            return report.clone();
        }
        let started = Instant::now();
        let drain_deadline = started + self.limits.stop_timeout;
        let mode = {
            let mut data = self.shared.lock();
            let mode = if mode == ShutdownMode::Cancel
                || self.shared.kill_requested.load(Ordering::Acquire)
                || data
                    .shutdown
                    .is_some_and(|shutdown| shutdown.mode == ShutdownMode::Cancel)
            {
                ShutdownMode::Cancel
            } else {
                ShutdownMode::Drain
            };
            if data.state != RecorderState::Failed {
                data.state = RecorderState::Stopping;
            }
            if data.shutdown.is_none() {
                data.shutdown = Some(Shutdown {
                    mode,
                    deadline: drain_deadline,
                });
            }
            if mode == ShutdownMode::Cancel {
                fail_locked(&mut data, RecordFailure::Cancelled);
            }
            mode
        };
        self.sender.take();

        let mut killed = mode == ShutdownMode::Cancel;
        let cleanup_deadline = if mode == ShutdownMode::Cancel {
            request_kill(&self.child);
            started + self.limits.kill_timeout
        } else {
            loop {
                poll_child(&self.child, &self.shared);
                let data = self.shared.lock();
                let exited = data.child.is_some();
                let failed = data.failure.is_some();
                drop(data);
                if exited {
                    break drain_deadline;
                }
                if failed || Instant::now() >= drain_deadline {
                    if !failed {
                        self.shared.fail(RecordFailure::StopTimedOut);
                    }
                    killed = true;
                    let kill_deadline = Instant::now() + self.limits.kill_timeout;
                    {
                        let mut data = self.shared.lock();
                        data.shutdown = Some(Shutdown {
                            mode: ShutdownMode::Cancel,
                            deadline: kill_deadline,
                        });
                    }
                    request_kill(&self.child);
                    break kill_deadline;
                }
                thread::sleep(POLL_INTERVAL);
            }
        };

        wait_until(cleanup_deadline, || {
            poll_child(&self.child, &self.shared);
            self.all_workers_finished()
        });
        poll_child(&self.child, &self.shared);
        let child_result = self.shared.lock().child;
        if child_result.is_none() {
            self.shared.fail(RecordFailure::KillTimedOut);
        } else if child_result.is_some_and(|child| !child.success) && !killed {
            self.shared.fail(RecordFailure::ChildExited {
                status: child_result.and_then(|child| child.code),
            });
        }

        let mut unfinished = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.is_finished() {
                if worker.join().is_err() {
                    self.shared.fail(RecordFailure::WorkerPanicked);
                }
            } else {
                unfinished.push(CleanupWorker::Unit(worker));
            }
        }
        let output = if let Some(worker) = self.output_worker.take() {
            if worker.is_finished() {
                if let Ok(result) = worker.join() {
                    result.finalization
                } else {
                    self.shared.fail(RecordFailure::WorkerPanicked);
                    OutputFinalization::Failed(io::ErrorKind::Other)
                }
            } else {
                unfinished.push(CleanupWorker::Output(worker));
                OutputFinalization::Unconfirmed
            }
        } else {
            OutputFinalization::Unconfirmed
        };
        if let Some(monitor) = self.monitor.take() {
            if monitor.is_finished() {
                if monitor.join().is_err() {
                    self.shared.fail(RecordFailure::WorkerPanicked);
                }
            } else {
                unfinished.push(CleanupWorker::Unit(monitor));
            }
        }
        if output == OutputFinalization::Unconfirmed || !unfinished.is_empty() {
            self.shared.fail(RecordFailure::CleanupUnconfirmed);
        }
        let wav_bytes = if self.wav_sink {
            self.shared.wav_sink_bytes.load(Ordering::Acquire)
        } else {
            wav_sidecar_bytes(self.wav.as_deref())
        };
        let (telemetry, final_child) = {
            let mut data = self.shared.lock();
            data.wav_bytes = wav_bytes;
            let cleanup_complete = unfinished.is_empty() && data.child.is_some();
            if data.failure.is_none()
                && data.child.is_some_and(|child| child.success)
                && output == OutputFinalization::Synced
                && cleanup_complete
            {
                data.state = RecorderState::Stopped;
            } else {
                data.state = RecorderState::Failed;
            }
            data.frozen = true;
            (telemetry_locked(&data, &self.shared.redactions), data.child)
        };
        let cleanup = if unfinished.is_empty() && final_child.is_some() {
            CleanupStatus::Complete
        } else {
            spawn_fallback_cleanup(unfinished);
            CleanupStatus::Unconfirmed
        };
        let outcome = if final_child.is_none() {
            StopOutcome::KillTimedOut
        } else if killed {
            StopOutcome::Killed
        } else if telemetry.failure.is_some() {
            StopOutcome::Failed
        } else {
            StopOutcome::Clean
        };
        let report = StopReport {
            outcome,
            exit_status: final_child.and_then(|child| child.code),
            output,
            cleanup,
            wav_bytes: telemetry.wav_bytes,
            telemetry,
        };
        self.report = Some(report.clone());
        report
    }

    fn all_workers_finished(&self) -> bool {
        self.workers.iter().all(JoinHandle::is_finished)
            && self
                .output_worker
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            && self.monitor.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Output handed to [`Recorder::start_output`].
enum PendingOutput {
    File(File),
    Sink(Box<dyn RecordSink>),
}

fn prepare_output(mut output: File) -> Result<File, StartError> {
    let bytes = output
        .metadata()
        .map_err(|error| StartError::complete(StartErrorKind::OutputMetadata(error.kind())))?
        .len();
    if bytes != 0 {
        return Err(StartError::complete(StartErrorKind::OutputNotEmpty {
            bytes,
        }));
    }
    output
        .seek(SeekFrom::Start(0))
        .map_err(|error| StartError::complete(StartErrorKind::OutputPosition(error.kind())))?;
    Ok(output)
}

/// Proves exclusive ownership of the optional WAV sidecar before any child
/// starts. `FFmpeg` reopens the path itself, so the create-new handle is closed
/// immediately. Missing parent directories are never created.
fn prepare_wav(path: Option<&Path>) -> Result<(), StartError> {
    let Some(path) = path else {
        return Ok(());
    };
    let probe = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| StartError::complete(StartErrorKind::WavOutput(error.kind())))?;
    drop(probe);
    Ok(())
}

/// Removes a sidecar created by [`prepare_wav`] when startup fails.
struct CreatedWav(Option<PathBuf>);

impl Drop for CreatedWav {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Reads the sidecar length once the child has stopped writing. A killed child
/// may leave the sidecar empty or truncated; it is only guaranteed finalized
/// after a clean stop.
fn wav_sidecar_bytes(path: Option<&Path>) -> u64 {
    path.and_then(|path| fs::metadata(path).ok())
        .map_or(0, |metadata| metadata.len())
}

fn random_token() -> Result<String, StartError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| StartError::complete(StartErrorKind::Randomness))?;
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        use fmt::Write as _;
        write!(token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn bind_listener(input: MediaInput) -> Result<TcpListener, StartError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|error| {
        StartError::complete(StartErrorKind::Bind {
            input,
            kind: error.kind(),
        })
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        StartError::complete(StartErrorKind::Bind {
            input,
            kind: error.kind(),
        })
    })?;
    Ok(listener)
}

fn listener_address(listener: &TcpListener, input: MediaInput) -> Result<SocketAddr, StartError> {
    listener.local_addr().map_err(|error| {
        StartError::complete(StartErrorKind::Bind {
            input,
            kind: error.kind(),
        })
    })
}

fn wait_for_first_http(
    video: &mut Endpoint,
    audio: &mut Endpoint,
    deadline: Instant,
    no_progress: Duration,
    shared: &Shared,
) -> Result<(MediaInput, TcpStream), StartErrorKind> {
    while Instant::now() < deadline {
        for endpoint in [&mut *video, &mut *audio] {
            if let Some(stream) = try_accept_http(endpoint, deadline, no_progress)? {
                return Ok((endpoint.input, stream));
            }
        }
        let data = shared.lock();
        if let Some(child) = data.child {
            return Err(StartErrorKind::EarlyExit {
                status: child.code,
                stderr: telemetry_locked(&data, &shared.redactions).stderr_tail,
            });
        }
        drop(data);
        thread::sleep(POLL_INTERVAL);
    }
    Err(StartErrorKind::ConnectTimeout {
        input: MediaInput::Video,
    })
}

fn try_accept_http(
    endpoint: &Endpoint,
    deadline: Instant,
    no_progress: Duration,
) -> Result<Option<TcpStream>, StartErrorKind> {
    let (mut stream, peer) = match endpoint.listener.accept() {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => {
            return Err(StartErrorKind::Connect {
                input: endpoint.input,
                kind: error.kind(),
            });
        }
    };
    if !peer.ip().is_loopback() {
        return Ok(None);
    }
    configure_http_stream(&stream, deadline, no_progress).map_err(|error| {
        StartErrorKind::Connect {
            input: endpoint.input,
            kind: error.kind(),
        }
    })?;
    match read_http_request(&mut stream, deadline, no_progress) {
        Ok(request) if request_path(&request) == Some(endpoint.path.as_str()) => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                )
                .map_err(|error| StartErrorKind::Connect {
                    input: endpoint.input,
                    kind: error.kind(),
                })?;
            stream
                .set_read_timeout(None)
                .and_then(|()| stream.set_write_timeout(Some(no_progress)))
                .map_err(|error| StartErrorKind::Connect {
                    input: endpoint.input,
                    kind: error.kind(),
                })?;
            Ok(Some(stream))
        }
        Ok(_) => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            Ok(None)
        }
        Err(HttpRequestError::TooLarge) => {
            let _ = stream.write_all(
                b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            Ok(None)
        }
        Err(HttpRequestError::Timeout) => Err(StartErrorKind::ConnectTimeout {
            input: endpoint.input,
        }),
        Err(HttpRequestError::Io(error)) => {
            if Instant::now() >= deadline {
                Err(StartErrorKind::ConnectTimeout {
                    input: endpoint.input,
                })
            } else {
                let _ = error;
                Ok(None)
            }
        }
    }
}

fn configure_http_stream(
    stream: &TcpStream,
    deadline: Instant,
    no_progress: Duration,
) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = remaining.min(no_progress).max(Duration::from_millis(1));
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))
}

#[derive(Debug)]
enum HttpRequestError {
    TooLarge,
    Timeout,
    Io(io::ErrorKind),
}

fn read_http_request(
    stream: &mut TcpStream,
    deadline: Instant,
    no_progress: Duration,
) -> Result<Vec<u8>, HttpRequestError> {
    let mut request = Vec::with_capacity(512);
    let mut buffer = [0_u8; 512];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(HttpRequestError::Timeout);
        }
        let timeout = deadline
            .saturating_duration_since(now)
            .min(no_progress)
            .max(Duration::from_millis(1));
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| HttpRequestError::Io(error.kind()))?;
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            // A signal must not end the request; retry through the
            // top-of-loop deadline check.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) && Instant::now() >= deadline =>
            {
                return Err(HttpRequestError::Timeout);
            }
            Err(error) => return Err(HttpRequestError::Io(error.kind())),
        };
        if count == 0 {
            return Err(HttpRequestError::Io(io::ErrorKind::UnexpectedEof));
        }
        if request.len().saturating_add(count) > MAX_HTTP_HEADER_BYTES {
            return Err(HttpRequestError::TooLarge);
        }
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
    }
}

fn request_path(request: &[u8]) -> Option<&str> {
    request_target(request, "GET")
}

fn request_target<'a>(request: &'a [u8], method: &str) -> Option<&'a str> {
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(verb), Some(path), Some("HTTP/1.1" | "HTTP/1.0"), None) if verb == method => {
            Some(path)
        }
        _ => None,
    }
}

/// Whether the request head carries a header named `name` (ASCII
/// case-insensitive).
fn has_header(head: &[u8], name: &str) -> bool {
    head.split(|byte| *byte == b'\n').skip(1).any(|line| {
        line.iter()
            .position(|byte| *byte == b':')
            .is_some_and(|colon| {
                line[..colon]
                    .trim_ascii()
                    .eq_ignore_ascii_case(name.as_bytes())
            })
    })
}

fn spawn_pending_writer(
    endpoint: Endpoint,
    receiver: mpsc::Receiver<WriteJob>,
    limits: RecordLimits,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("fm-ffmpeg-{:?}-http-writer", endpoint.input))
        .spawn(move || {
            if let Ok(first) = receiver.recv() {
                let deadline = Instant::now() + limits.connect_timeout;
                match accept_http_until(&endpoint, deadline, limits.no_progress_timeout, &shared) {
                    Ok(stream) => {
                        write_jobs(stream, Some(first), receiver, endpoint.input, &shared);
                    }
                    Err(failure) => {
                        shared.fail(failure);
                        drop(first);
                    }
                }
            } else {
                let shutdown = shared.lock().shutdown;
                if let Some(Shutdown {
                    mode: ShutdownMode::Drain,
                    deadline,
                }) = shutdown
                    && let Ok(stream) =
                        accept_http_until(&endpoint, deadline, limits.no_progress_timeout, &shared)
                {
                    drop(stream);
                }
            }
        })
}

fn accept_http_until(
    endpoint: &Endpoint,
    deadline: Instant,
    no_progress: Duration,
    shared: &Shared,
) -> Result<TcpStream, RecordFailure> {
    while Instant::now() < deadline {
        if shared.kill_requested.load(Ordering::Acquire)
            || shared
                .lock()
                .shutdown
                .is_some_and(|shutdown| shutdown.mode == ShutdownMode::Cancel)
        {
            return Err(RecordFailure::Cancelled);
        }
        match try_accept_http(endpoint, deadline, no_progress) {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(StartErrorKind::Connect { kind, .. }) => {
                return Err(RecordFailure::Connect {
                    input: endpoint.input,
                    kind,
                });
            }
            Err(_) => return Err(RecordFailure::ConnectTimeout(endpoint.input)),
        }
    }
    Err(RecordFailure::ConnectTimeout(endpoint.input))
}

fn spawn_writer(
    endpoint: Endpoint,
    stream: TcpStream,
    receiver: mpsc::Receiver<WriteJob>,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("fm-ffmpeg-{:?}-writer", endpoint.input))
        .spawn(move || {
            write_jobs(stream, None, receiver, endpoint.input, &shared);
            drop(endpoint);
        })
}

fn write_jobs(
    mut stream: TcpStream,
    first: Option<WriteJob>,
    receiver: mpsc::Receiver<WriteJob>,
    input: MediaInput,
    shared: &Shared,
) {
    for job in first.into_iter().chain(receiver) {
        let result = stream.write_all(&job.bytes);
        let successful = result.is_ok();
        job.complete(successful);
        if let Err(error) = result {
            shared.fail(match input {
                MediaInput::Video => RecordFailure::VideoWrite(error.kind()),
                MediaInput::Audio => RecordFailure::AudioWrite(error.kind()),
            });
            return;
        }
    }
}

fn spawn_dispatcher(
    receiver: mpsc::Receiver<PairJob>,
    video: mpsc::SyncSender<WriteJob>,
    audio: mpsc::SyncSender<WriteJob>,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-pair-dispatcher".to_owned())
        .spawn(move || {
            for pair in receiver {
                let PairedFrame {
                    rgba, audio_f32le, ..
                } = pair.frame;
                let video_job = WriteJob {
                    bytes: rgba,
                    completion: Some(Arc::clone(&pair.completion)),
                };
                let audio_job = WriteJob {
                    bytes: audio_f32le,
                    completion: Some(pair.completion),
                };
                if let Err(error) = video.send(video_job) {
                    error.0.complete(false);
                    audio_job.complete(false);
                    shared.fail(RecordFailure::DispatcherClosed(MediaInput::Video));
                    continue;
                }
                if let Err(error) = audio.send(audio_job) {
                    error.0.complete(false);
                    shared.fail(RecordFailure::DispatcherClosed(MediaInput::Audio));
                }
            }
        })
}

fn spawn_output(
    mut stdout: impl Read + Send + 'static,
    mut output: impl RecordSink + 'static,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<OutputResult>> {
    thread::Builder::new()
        .name("fm-ffmpeg-output".to_owned())
        .spawn(move || {
            let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
            let mut first_error = None;
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) if first_error.is_none() => {
                        if let Err(error) = output.write_all(&buffer[..count]) {
                            first_error = Some(error.kind());
                            shared.fail(RecordFailure::OutputWrite(error.kind()));
                        } else {
                            shared.add_output_bytes(count);
                        }
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        first_error.get_or_insert(error.kind());
                        shared.fail(RecordFailure::OutputRead(error.kind()));
                        break;
                    }
                }
            }
            let mut finalization_error = None;
            if let Err(error) = output.flush() {
                finalization_error = Some(error.kind());
                shared.fail(RecordFailure::OutputFlush(error.kind()));
            }
            if let Err(error) = output.finish() {
                finalization_error.get_or_insert(error.kind());
                shared.fail(RecordFailure::OutputSync(error.kind()));
            }
            let finalization = match first_error.or(finalization_error) {
                None => OutputFinalization::Synced,
                Some(kind) => OutputFinalization::Failed(kind),
            };
            OutputResult { finalization }
        })
}

fn spawn_stderr(
    mut stderr: impl Read + Send + 'static,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-stderr".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => shared.append_stderr(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        shared.fail(RecordFailure::StderrRead(error.kind()));
                        return;
                    }
                }
            }
        })
}

/// The WAV upload listener and its authenticated path.
struct WavEndpoint {
    listener: TcpListener,
    path: String,
}

fn bind_wav_listener() -> Result<(TcpListener, SocketAddr), StartError> {
    let fail = |error: io::Error| StartError::complete(StartErrorKind::WavOutput(error.kind()));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(fail)?;
    listener.set_nonblocking(true).map_err(fail)?;
    let address = listener.local_addr().map_err(fail)?;
    Ok((listener, address))
}

/// Copies the child's WAV upload into `sink` with the same contract as the
/// MP4 output worker: every body chunk is written until the first error,
/// later bytes are drained and dropped so the child never blocks, and the
/// sink is flushed and finished exactly once when the upload ends or the
/// child exits without one.
fn spawn_wav_output(
    endpoint: WavEndpoint,
    mut sink: Box<dyn RecordSink>,
    limits: RecordLimits,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-wav-output".to_owned())
        .spawn(move || {
            let mut failed = false;
            let mut fail = |kind: io::ErrorKind| {
                if !failed {
                    failed = true;
                    shared.fail(RecordFailure::WavOutput(kind));
                }
            };
            match accept_wav_upload(&endpoint, limits, &shared) {
                Ok(Some((stream, body))) => {
                    copy_wav_upload(stream, &body, &mut sink, &shared, &mut fail);
                }
                Ok(None) => {
                    if !cancelling(&shared) {
                        fail(io::ErrorKind::NotConnected);
                    }
                }
                Err(kind) => fail(kind),
            }
            if let Err(error) = sink.flush() {
                fail(error.kind());
            }
            if let Err(error) = sink.finish() {
                fail(error.kind());
            }
        })
}

fn cancelling(shared: &Shared) -> bool {
    shared.kill_requested.load(Ordering::Acquire)
        || shared
            .lock()
            .shutdown
            .is_some_and(|shutdown| shutdown.mode == ShutdownMode::Cancel)
}

/// Waits for the child's authenticated `PUT` of the WAV output and returns
/// the stream plus any body bytes read with the request head. Returns
/// `None` once the child exited or the recorder is cancelling without an
/// upload having arrived. Unauthenticated or malformed requests are refused
/// and waiting continues.
fn accept_wav_upload(
    endpoint: &WavEndpoint,
    limits: RecordLimits,
    shared: &Shared,
) -> Result<Option<(TcpStream, Vec<u8>)>, io::ErrorKind> {
    loop {
        let (mut stream, peer) = match endpoint.listener.accept() {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if cancelling(shared) || shared.lock().child.is_some() {
                    return Ok(None);
                }
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.kind()),
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let deadline = Instant::now() + limits.connect_timeout;
        if configure_http_stream(&stream, deadline, limits.no_progress_timeout).is_err() {
            continue;
        }
        let request = match read_http_request(&mut stream, deadline, limits.no_progress_timeout) {
            Ok(request) => request,
            Err(HttpRequestError::TooLarge) => {
                let _ = stream.write_all(
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            Err(_) => continue,
        };
        let Some(head_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        else {
            continue;
        };
        if request_target(&request, "PUT") != Some(endpoint.path.as_str()) {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        }
        // `-chunked_post 0` makes the body the raw WAV stream. A framed body
        // would corrupt the copy, so it is refused rather than forwarded.
        if has_header(&request[..head_end], "transfer-encoding") {
            let _ = stream.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Err(io::ErrorKind::InvalidData);
        }
        stream
            .set_read_timeout(None)
            .and_then(|()| stream.set_write_timeout(Some(limits.no_progress_timeout)))
            .map_err(|error| error.kind())?;
        return Ok(Some((stream, request[head_end..].to_vec())));
    }
}

fn copy_wav_upload(
    mut stream: TcpStream,
    body: &[u8],
    sink: &mut dyn RecordSink,
    shared: &Shared,
    fail: &mut impl FnMut(io::ErrorKind),
) {
    let mut healthy = true;
    let mut deliver = |bytes: &[u8], fail: &mut dyn FnMut(io::ErrorKind)| {
        if !healthy || bytes.is_empty() {
            return;
        }
        match sink.write_all(bytes) {
            Ok(()) => {
                shared
                    .wav_sink_bytes
                    .fetch_add(bytes.len() as u64, Ordering::AcqRel);
            }
            Err(error) => {
                healthy = false;
                fail(error.kind());
            }
        }
    };
    deliver(body, fail);
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => deliver(&buffer[..count], fail),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                fail(error.kind());
                return;
            }
        }
    }
}

type MonitorTarget = (Arc<Mutex<Child>>, Arc<Shared>);

fn spawn_monitor_waiter() -> Result<(mpsc::SyncSender<MonitorTarget>, JoinHandle<()>), StartError> {
    let (sender, receiver) = mpsc::sync_channel::<MonitorTarget>(1);
    let monitor = thread::Builder::new()
        .name("fm-ffmpeg-child-reaper".to_owned())
        .spawn(move || {
            let Ok((child, shared)) = receiver.recv() else {
                return;
            };
            loop {
                if shared.kill_requested.load(Ordering::Acquire) {
                    try_kill_child(&child);
                }
                if let Some(status) = child_status(&child) {
                    shared.record_child(status);
                    return;
                }
                thread::sleep(POLL_INTERVAL);
            }
        })
        .map_err(|error| StartError::complete(StartErrorKind::ThreadSpawn(error.kind())))?;
    Ok((sender, monitor))
}

fn child_status(child: &Arc<Mutex<Child>>) -> Option<ExitStatus> {
    match child.try_lock() {
        Ok(mut child) => child.try_wait().ok().flatten(),
        Err(TryLockError::Poisoned(error)) => error.into_inner().try_wait().ok().flatten(),
        Err(TryLockError::WouldBlock) => None,
    }
}

fn poll_child(child: &Arc<Mutex<Child>>, shared: &Shared) {
    if let Some(status) = child_status(child) {
        shared.record_child(status);
    }
}

fn request_kill(child: &Arc<Mutex<Child>>) {
    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = child.kill();
}

fn try_kill_child(child: &Arc<Mutex<Child>>) {
    match child.try_lock() {
        Ok(mut child) => {
            let _ = child.kill();
        }
        Err(TryLockError::Poisoned(error)) => {
            let _ = error.into_inner().kill();
        }
        Err(TryLockError::WouldBlock) => {}
    }
}

fn cleanup_missing_pipe(
    child: &Arc<Mutex<Child>>,
    shared: &Arc<Shared>,
    monitor: JoinHandle<()>,
    timeout: Duration,
) -> StartError {
    request_kill(child);
    let deadline = Instant::now() + timeout;
    wait_until(deadline, || {
        poll_child(child, shared);
        monitor.is_finished()
    });
    let cleanup = if monitor.is_finished() {
        let _ = monitor.join();
        CleanupStatus::Complete
    } else {
        shared.lock().frozen = true;
        spawn_fallback_cleanup(vec![CleanupWorker::Unit(monitor)]);
        CleanupStatus::Unconfirmed
    };
    StartError {
        kind: StartErrorKind::MissingPipe,
        cleanup,
    }
}

fn terminate_shared_without_monitor(child: &Arc<Mutex<Child>>, timeout: Duration) -> CleanupStatus {
    request_kill(child);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child_status(child).is_some() {
            return CleanupStatus::Complete;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let holder = Arc::new(Mutex::new(Some(Arc::clone(child))));
    let background = Arc::clone(&holder);
    if thread::Builder::new()
        .name("fm-ffmpeg-emergency-reaper".to_owned())
        .spawn(move || {
            let child = background
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(child) = child {
                let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = child.wait();
            }
        })
        .is_err()
        && let Some(child) = holder.lock().unwrap_or_else(PoisonError::into_inner).take()
    {
        // Thread exhaustion is the sole case where correctness takes priority
        // over the normal bounded-return guarantee.
        let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = child.wait();
    }
    CleanupStatus::Unconfirmed
}

#[cfg(test)]
fn terminate_unshared(mut child: Child, timeout: Duration) -> CleanupStatus {
    let _ = child.kill();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return CleanupStatus::Complete;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let holder = Arc::new(Mutex::new(Some(child)));
    let background = Arc::clone(&holder);
    if thread::Builder::new()
        .name("fm-ffmpeg-fallback-reaper".to_owned())
        .spawn(move || {
            if let Some(mut child) = background
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                let _ = child.wait();
            }
        })
        .is_err()
        && let Some(mut child) = holder.lock().unwrap_or_else(PoisonError::into_inner).take()
    {
        let _ = child.wait();
    }
    CleanupStatus::Unconfirmed
}

fn cleanup_startup(
    kind: StartErrorKind,
    child: &Arc<Mutex<Child>>,
    shared: &Arc<Shared>,
    mut workers: Vec<JoinHandle<()>>,
    output: Option<JoinHandle<OutputResult>>,
    monitor: JoinHandle<()>,
    timeout: Duration,
) -> StartError {
    {
        let mut data = shared.lock();
        data.shutdown = Some(Shutdown {
            mode: ShutdownMode::Cancel,
            deadline: Instant::now() + timeout,
        });
    }
    request_kill(child);
    let deadline = Instant::now() + timeout;
    wait_until(deadline, || {
        poll_child(child, shared);
        workers.iter().all(JoinHandle::is_finished)
            && output.as_ref().is_none_or(JoinHandle::is_finished)
            && monitor.is_finished()
    });
    let mut unfinished = Vec::new();
    for worker in workers.drain(..) {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            unfinished.push(CleanupWorker::Unit(worker));
        }
    }
    if let Some(output) = output {
        if output.is_finished() {
            let _ = output.join();
        } else {
            unfinished.push(CleanupWorker::Output(output));
        }
    }
    if monitor.is_finished() {
        let _ = monitor.join();
    } else {
        unfinished.push(CleanupWorker::Unit(monitor));
    }
    let child_done = shared.lock().child.is_some();
    let cleanup = if unfinished.is_empty() && child_done {
        CleanupStatus::Complete
    } else {
        {
            let mut data = shared.lock();
            data.frozen = true;
        }
        spawn_fallback_cleanup(unfinished);
        CleanupStatus::Unconfirmed
    };
    let kind = match kind {
        StartErrorKind::EarlyExit { status, .. } => StartErrorKind::EarlyExit {
            status,
            stderr: shared.snapshot().stderr_tail,
        },
        kind => kind,
    };
    StartError { kind, cleanup }
}

fn wait_until(deadline: Instant, mut complete: impl FnMut() -> bool) {
    while Instant::now() < deadline {
        if complete() {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Starts unbounded joining after a bounded public return. If the OS cannot
/// create this thread, joins synchronously as the last-resort ownership path;
/// only that thread-exhaustion case may exceed the configured deadline.
fn spawn_fallback_cleanup(workers: Vec<CleanupWorker>) {
    if workers.is_empty() {
        return;
    }
    let holder = Arc::new(Mutex::new(Some(workers)));
    let background = Arc::clone(&holder);
    if thread::Builder::new()
        .name("fm-ffmpeg-fallback-cleanup".to_owned())
        .spawn(move || {
            if let Some(workers) = background
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                join_cleanup_workers(workers);
            }
        })
        .is_err()
        && let Some(workers) = holder.lock().unwrap_or_else(PoisonError::into_inner).take()
    {
        join_cleanup_workers(workers);
    }
}

fn join_cleanup_workers(workers: Vec<CleanupWorker>) {
    for worker in workers {
        match worker {
            CleanupWorker::Unit(worker) => {
                let _ = worker.join();
            }
            CleanupWorker::Output(worker) => {
                let _ = worker.join();
            }
        }
    }
}

fn recorder_executable(executable: Executable) -> Result<OsString, StartError> {
    match executable {
        Executable::SearchPath => Ok(OsString::from("ffmpeg")),
        Executable::Explicit(path) if path.is_absolute() => {
            let path = fs::canonicalize(path).map_err(|error| {
                StartError::complete(match error.kind() {
                    io::ErrorKind::NotFound => {
                        StartErrorKind::ToolUnavailable(UnavailableReason::Missing)
                    }
                    io::ErrorKind::PermissionDenied => {
                        StartErrorKind::ToolUnavailable(UnavailableReason::PermissionDenied)
                    }
                    _ => StartErrorKind::InvalidExecutable,
                })
            })?;
            if path.is_file() {
                Ok(path.into_os_string())
            } else {
                Err(StartError::complete(StartErrorKind::InvalidExecutable))
            }
        }
        Executable::Explicit(_) => Err(StartError::complete(StartErrorKind::InvalidExecutable)),
    }
}

fn spawn_error(error: &io::Error) -> StartErrorKind {
    match error.kind() {
        io::ErrorKind::NotFound => StartErrorKind::ToolUnavailable(UnavailableReason::Missing),
        io::ErrorKind::PermissionDenied => {
            StartErrorKind::ToolUnavailable(UnavailableReason::PermissionDenied)
        }
        kind => StartErrorKind::Spawn(kind),
    }
}

fn command(executable: &OsStr, args: &[OsString]) -> Command {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for name in retained_environment_names() {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("LC_ALL", "C");
    command
}

fn retained_environment_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["PATH", "SystemRoot", "WINDIR", "PATHEXT"]
    }
    #[cfg(not(windows))]
    {
        &["PATH"]
    }
}

/// Where the child writes its linear-PCM WAV output.
#[derive(Clone, Copy, Debug)]
enum WavDestination<'a> {
    /// A sidecar path the child opens itself.
    File(&'a Path),
    /// The raw body of one `PUT` to the authenticated WAV listener.
    Upload { address: SocketAddr, token: &'a str },
}

/// Appends the optional WAV output: linear PCM from the same child, after
/// the MP4 output so every video option stays attached to `pipe:1`.
fn push_wav_output_args(args: &mut Vec<OsString>, wav: Option<WavDestination<'_>>) {
    match wav {
        None => {}
        Some(WavDestination::File(path)) => {
            // A second output for the same child: linear PCM written directly
            // to the sidecar path. The MP4 output above stays first on
            // `pipe:1`, and every video option stays attached to it.
            args.extend(
                ["-map", "1:a:0", "-c:a", "pcm_s16le", "-f", "wav", "-y"]
                    .into_iter()
                    .map(OsString::from),
            );
            // `file:` keeps a relative name such as `take:2.wav`, `pipe:1`, or
            // `-a.wav` a literal path instead of a protocol, pipe, or option.
            let mut output = OsString::from("file:");
            output.push(path.as_os_str());
            args.push(output);
        }
        Some(WavDestination::Upload { address, token }) => {
            // The same second output, streamed as the raw body of one `PUT`
            // (`-chunked_post 0`: no chunk framing, the body ends when the
            // child closes the connection) to the authenticated listener.
            args.extend(
                [
                    "-map",
                    "1:a:0",
                    "-c:a",
                    "pcm_s16le",
                    "-f",
                    "wav",
                    "-protocol_whitelist",
                    "http,tcp",
                    "-method",
                    "PUT",
                    "-chunked_post",
                    "0",
                ]
                .into_iter()
                .map(OsString::from),
            );
            args.push(OsString::from(format!("http://{address}/{token}")));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn command_args(
    format: &RecordFormat,
    video_address: SocketAddr,
    audio_address: SocketAddr,
    video_token: &str,
    audio_token: &str,
    wav: Option<WavDestination<'_>>,
    video_bitrate_kbps: Option<u32>,
    audio_bitrate_kbps: Option<u32>,
    timecode: Option<Timecode>,
) -> Vec<OsString> {
    let dimensions = format.dimensions;
    let rate = format.frame_rate;
    let gop = u64::from(rate.numerator()).div_ceil(u64::from(rate.denominator()));
    let mut values = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "warning".to_owned(),
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pixel_format".to_owned(),
        "rgba".to_owned(),
        "-video_size".to_owned(),
        format!("{}x{}", dimensions.width(), dimensions.height()),
        "-framerate".to_owned(),
        format!("{}/{}", rate.numerator(), rate.denominator()),
        // Both raw inputs are fully described above, so there is nothing to
        // probe. Without this the child buys `analyzeduration` (5s by default)
        // worth of one input before it drains the other, which deadlocks the
        // pair writers at any real frame size.
        "-probesize".to_owned(),
        PROBE_SIZE.to_owned(),
        "-analyzeduration".to_owned(),
        "0".to_owned(),
        "-protocol_whitelist".to_owned(),
        "http,tcp".to_owned(),
        "-i".to_owned(),
        format!("http://{video_address}/{video_token}"),
        "-f".to_owned(),
        "f32le".to_owned(),
        "-ar".to_owned(),
        format.sample_rate.hertz().to_string(),
        "-ac".to_owned(),
        format.channel_layout.channels().len().to_string(),
        "-channel_layout".to_owned(),
        format.ffmpeg_channel_layout.to_owned(),
        "-probesize".to_owned(),
        PROBE_SIZE.to_owned(),
        "-analyzeduration".to_owned(),
        "0".to_owned(),
        "-protocol_whitelist".to_owned(),
        "http,tcp".to_owned(),
        "-i".to_owned(),
        format!("http://{audio_address}/{audio_token}"),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-map".to_owned(),
        "1:a:0".to_owned(),
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
    ];
    if let Some(kbps) = video_bitrate_kbps {
        // `-maxrate` plus a two-second `-bufsize` bounds the instantaneous rate
        // around the same target; without both, `-b:v` alone is only a hint.
        values.extend([
            "-b:v".to_owned(),
            format!("{kbps}k"),
            "-maxrate".to_owned(),
            format!("{kbps}k"),
            "-bufsize".to_owned(),
            format!("{}k", kbps.saturating_mul(2)),
        ]);
    }
    values.extend([
        "-g".to_owned(),
        gop.to_string(),
        "-keyint_min".to_owned(),
        gop.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-force_key_frames".to_owned(),
        "expr:gte(t,n_forced*1)".to_owned(),
        "-c:a".to_owned(),
        "aac".to_owned(),
    ]);
    if let Some(kbps) = audio_bitrate_kbps {
        values.extend(["-b:a".to_owned(), format!("{kbps}k")]);
    }
    values.extend([
        "-movflags".to_owned(),
        "+empty_moov+default_base_moof+frag_keyframe".to_owned(),
        "-flush_packets".to_owned(),
        "1".to_owned(),
    ]);
    if let Some(timecode) = timecode {
        let (hours, minutes, seconds, frames) = timecode.components();
        values.extend([
            "-timecode".to_owned(),
            format!("{hours:02}:{minutes:02}:{seconds:02}:{frames:02}"),
        ]);
    }
    values.extend(["-f".to_owned(), "mp4".to_owned(), "pipe:1".to_owned()]);
    let mut args = values.into_iter().map(OsString::from).collect::<Vec<_>>();
    push_wav_output_args(&mut args, wav);
    args
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_frame::{
        ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp,
        OriginalTimestamp, TimeBase,
    };
    use tempfile::tempdir;

    use super::*;

    struct DelayedEof {
        delay: Duration,
        bytes: Option<Vec<u8>>,
    }

    impl Read for DelayedEof {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            thread::sleep(self.delay);
            let Some(bytes) = self.bytes.take() else {
                return Ok(0);
            };
            let count = bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&bytes[..count]);
            Ok(count)
        }
    }

    fn format() -> RecordFormat {
        RecordFormat::new(
            2,
            2,
            FrameRate::new(60_000, 1_001).unwrap(),
            SampleRate::new(48_000).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(10),
        )
        .unwrap()
    }

    fn audio(format: &RecordFormat, sequence: SequenceNumber, samples: usize) -> AudioBlock {
        let expected = format.expected_timing(sequence).unwrap();
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(i64::try_from(expected.start_sample).unwrap()),
                TimeBase::new(1, format.sample_rate().hertz()).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(expected.start_nanos),
            NormalizedDuration::from_nanos(expected.duration_nanos).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            sequence,
        )
        .unwrap();
        AudioBlock::new(
            timing,
            format.sample_rate(),
            format.channel_layout().clone(),
            vec![vec![1.0; samples], vec![-1.0; samples]],
        )
        .unwrap()
    }

    fn paired(format: &RecordFormat, sequence: u64) -> PairedFrame {
        let sequence = SequenceNumber::new(sequence);
        let samples = format.expected_samples(sequence).unwrap();
        PairedFrame::new(
            format,
            sequence,
            vec![0; format.rgba_bytes_per_frame()],
            audio(format, sequence, samples),
        )
        .unwrap()
    }

    fn helper_child(mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "process::tests::runner_helper",
                "--nocapture",
            ])
            .env("FM_RUNNER_HELPER", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn fake_recorder(
        format: &RecordFormat,
        limits: RecordLimits,
        shared: Arc<Shared>,
        sender: mpsc::SyncSender<PairJob>,
    ) -> Recorder {
        Recorder {
            format: format.clone(),
            limits,
            wav: None,
            wav_sink: false,
            sender: Some(sender),
            shared,
            child: Arc::new(Mutex::new(helper_child("sleep"))),
            workers: Vec::new(),
            output_worker: None,
            monitor: None,
            report: None,
        }
    }

    fn suppress_fake_drop(recorder: &mut Recorder) {
        recorder.report = Some(StopReport {
            outcome: StopOutcome::Failed,
            exit_status: None,
            output: OutputFinalization::Unconfirmed,
            cleanup: CleanupStatus::Unconfirmed,
            wav_bytes: 0,
            telemetry: recorder.telemetry(),
        });
        request_kill(&recorder.child);
        let deadline = Instant::now() + Duration::from_secs(1);
        while child_status(&recorder.child).is_none() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn rational_spans_layouts_and_interleave_are_exact() {
        let format = format();
        let spans = (10..16)
            .map(|sequence| {
                format
                    .expected_samples(SequenceNumber::new(sequence))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(spans, [800, 801, 801, 801, 801, 800]);
        let layouts = [
            (vec![Channel::Mono], "mono"),
            (vec![Channel::Left, Channel::Right], "stereo"),
            (
                vec![Channel::Left, Channel::Right, Channel::LowFrequency],
                "2.1",
            ),
            (vec![Channel::Left, Channel::Right, Channel::Center], "3.0"),
            (
                vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::Center,
                    Channel::LowFrequency,
                ],
                "3.1",
            ),
            (
                vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::LeftSurround,
                    Channel::RightSurround,
                ],
                "quad(side)",
            ),
            (
                vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::Center,
                    Channel::LeftSurround,
                    Channel::RightSurround,
                ],
                "5.0(side)",
            ),
            (
                vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::Center,
                    Channel::LowFrequency,
                    Channel::LeftSurround,
                    Channel::RightSurround,
                ],
                "5.1(side)",
            ),
        ];
        for (channels, expected) in layouts {
            assert_eq!(
                ffmpeg_layout(&ChannelLayout::new(channels).unwrap()),
                Some(FfmpegLayout::native(expected))
            );
        }

        let tiny = RecordFormat::new(
            2,
            2,
            FrameRate::new(2, 1).unwrap(),
            SampleRate::new(4).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap();
        let frame = PairedFrame::new(
            &tiny,
            SequenceNumber::new(0),
            vec![0; 16],
            audio(&tiny, SequenceNumber::new(0), 2),
        )
        .unwrap();
        let expected = [1.0_f32, -1.0, 1.0, -1.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(frame.audio_f32le(), expected);
    }

    #[test]
    fn rear_surround_and_discrete_layouts_name_exact_ffmpeg_speakers() {
        use fm_types::DiscreteChannel;
        let (l, r, c, lfe) = (
            Channel::Left,
            Channel::Right,
            Channel::Center,
            Channel::LowFrequency,
        );
        let (ls, rs, lrs, rrs) = (
            Channel::LeftSurround,
            Channel::RightSurround,
            Channel::LeftRearSurround,
            Channel::RightRearSurround,
        );
        // ffmpeg plain quad/5.0/5.1 are BL/BR, i.e. rear surrounds.
        for (channels, expected) in [
            (vec![l, r, lrs, rrs], "quad"),
            (vec![l, r, c, lrs, rrs], "5.0"),
            (vec![l, r, c, lfe, lrs, rrs], "5.1"),
            (vec![l, r, c, lrs, rrs, ls, rs], "7.0"),
            // ffmpeg-native 7.1 order: FL FR FC LFE BL BR SL SR.
            (vec![l, r, c, lfe, lrs, rrs, ls, rs], "7.1"),
        ] {
            assert_eq!(
                ffmpeg_layout(&ChannelLayout::new(channels).unwrap()),
                Some(FfmpegLayout::native(expected))
            );
        }
        // SMPTE/ITU 7.1 is reordered to ffmpeg's 7.1 while interleaving.
        let smpte = ChannelLayout::new(vec![l, r, c, lfe, ls, rs, lrs, rrs]).unwrap();
        let mapped = ffmpeg_layout(&smpte).unwrap();
        assert_eq!(mapped.name, "7.1");
        let block = AudioBlock::new(
            MediaTiming::new(
                OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, 48_000).unwrap()),
                NormalizedTimestamp::from_nanos(0),
                NormalizedDuration::from_nanos(41_666).unwrap(),
                ClockDomainId::new(NonZeroU128::new(1).unwrap()),
                SequenceNumber::new(0),
            )
            .unwrap(),
            SampleRate::new(48_000).unwrap(),
            smpte,
            (0..8_u8)
                .map(|channel| vec![f32::from(channel), f32::from(channel) + 10.0])
                .collect(),
        )
        .unwrap();
        let interleaved = interleave_f32le(&block, mapped.plane_order).unwrap();
        let samples = interleaved
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect::<Vec<_>>();
        assert_eq!(
            samples,
            [
                0.0, 1.0, 2.0, 3.0, 6.0, 7.0, 4.0, 5.0, //
                10.0, 11.0, 12.0, 13.0, 16.0, 17.0, 14.0, 15.0,
            ]
        );

        // Discrete-only layouts keep their order as ffmpeg unspecified-order
        // layouts; mixing discrete and named channels is refused.
        let discrete = |number| Channel::Discrete(DiscreteChannel::new(number).unwrap());
        assert_eq!(
            ffmpeg_layout(&ChannelLayout::new((1..=16).map(discrete).collect()).unwrap()),
            Some(FfmpegLayout::native("16C"))
        );
        assert_eq!(
            ffmpeg_layout(&ChannelLayout::new(vec![discrete(3), discrete(1)]).unwrap()),
            Some(FfmpegLayout::native("2C"))
        );
        assert_eq!(
            ffmpeg_layout(&ChannelLayout::new(vec![l, r, discrete(3)]).unwrap()),
            None
        );
        assert_eq!(
            ffmpeg_layout(&ChannelLayout::new(vec![l, r, lrs]).unwrap()),
            None
        );
    }

    #[test]
    fn absolute_sequence_cadence_and_audio_timing_are_required() {
        let format = RecordFormat::new(
            2,
            2,
            FrameRate::new(60_000, 1_001).unwrap(),
            SampleRate::new(48_000).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(11),
        )
        .unwrap();
        let sequence = SequenceNumber::new(11);
        let expected = format.expected_timing(sequence).unwrap();
        assert_eq!(expected.start_sample, 8_808);
        assert_eq!(format.expected_samples(sequence), Ok(801));
        assert_eq!(
            format.expected_samples(SequenceNumber::new(10)),
            Err(FrameError::SequenceBeforeOrigin)
        );

        let make_audio = |timing| {
            AudioBlock::new(
                timing,
                format.sample_rate(),
                format.channel_layout().clone(),
                vec![vec![0.0; 801], vec![0.0; 801]],
            )
            .unwrap()
        };
        let original_time_base = TimeBase::new(1, format.sample_rate().hertz()).unwrap();
        let timing = |original_sample, pts, duration| {
            MediaTiming::new(
                OriginalTimestamp::new(MediaTimestamp::new(original_sample), original_time_base),
                NormalizedTimestamp::from_nanos(pts),
                NormalizedDuration::from_nanos(duration).unwrap(),
                ClockDomainId::new(NonZeroU128::new(1).unwrap()),
                sequence,
            )
            .unwrap()
        };
        let rgba = || vec![0; format.rgba_bytes_per_frame()];
        assert_eq!(
            PairedFrame::new(
                &format,
                sequence,
                rgba(),
                make_audio(timing(
                    i64::try_from(expected.start_sample).unwrap() + 1,
                    expected.start_nanos,
                    expected.duration_nanos,
                )),
            )
            .unwrap_err(),
            FrameError::AudioOriginalTimestamp
        );
        assert!(matches!(
            PairedFrame::new(
                &format,
                sequence,
                rgba(),
                make_audio(timing(
                    i64::try_from(expected.start_sample).unwrap(),
                    expected.start_nanos + 1,
                    expected.duration_nanos,
                )),
            ),
            Err(FrameError::AudioPresentationTimestamp { .. })
        ));
        assert!(matches!(
            PairedFrame::new(
                &format,
                sequence,
                rgba(),
                make_audio(timing(
                    i64::try_from(expected.start_sample).unwrap(),
                    expected.start_nanos,
                    expected.duration_nanos + 1,
                )),
            ),
            Err(FrameError::AudioDuration { .. })
        ));
    }

    #[test]
    fn command_uses_tokenized_http_and_standard_fragment_flags() {
        let args = command_args(
            &format(),
            "127.0.0.1:1000".parse().unwrap(),
            "127.0.0.1:1001".parse().unwrap(),
            "video-secret",
            "audio-secret",
            None,
            None,
            None,
            None,
        );
        let args = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(args.contains(&"http://127.0.0.1:1000/video-secret".into()));
        assert!(args.contains(&"http://127.0.0.1:1001/audio-secret".into()));
        assert_eq!(args.iter().filter(|value| **value == "http,tcp").count(), 2);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-movflags", "+empty_moov+default_base_moof+frag_keyframe"])
        );
        assert_eq!(args.last().map(AsRef::as_ref), Some("pipe:1"));
    }

    #[test]
    fn command_is_byte_identical_without_a_wav_sidecar_and_exact_with_one() {
        let args = |wav: Option<WavDestination<'_>>| {
            command_args(
                &format(),
                "127.0.0.1:1000".parse().unwrap(),
                "127.0.0.1:1001".parse().unwrap(),
                "video-secret",
                "audio-secret",
                wav,
                None,
                None,
                None,
            )
        };
        let strings = |args: &[OsString]| {
            args.iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let expected_base = [
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "warning",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgba",
            "-video_size",
            "2x2",
            "-framerate",
            "60000/1001",
            "-probesize",
            PROBE_SIZE,
            "-analyzeduration",
            "0",
            "-protocol_whitelist",
            "http,tcp",
            "-i",
            "http://127.0.0.1:1000/video-secret",
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-channel_layout",
            "stereo",
            "-probesize",
            PROBE_SIZE,
            "-analyzeduration",
            "0",
            "-protocol_whitelist",
            "http,tcp",
            "-i",
            "http://127.0.0.1:1001/audio-secret",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
            "-force_key_frames",
            "expr:gte(t,n_forced*1)",
            "-c:a",
            "aac",
            "-movflags",
            "+empty_moov+default_base_moof+frag_keyframe",
            "-flush_packets",
            "1",
            "-f",
            "mp4",
            "pipe:1",
        ];
        assert_eq!(strings(&args(None)), expected_base);

        let wav = Path::new("/tmp/freemix-sidecar.wav");
        let mut expected_wav = strings(&args(None));
        expected_wav
            .extend(["-map", "1:a:0", "-c:a", "pcm_s16le", "-f", "wav", "-y"].map(str::to_owned));
        expected_wav.push(format!("file:{}", wav.to_str().unwrap()));
        assert_eq!(
            strings(&args(Some(WavDestination::File(wav)))),
            expected_wav
        );
    }

    #[test]
    fn command_streams_a_wav_sink_output_as_one_raw_put() {
        let args = |wav: Option<WavDestination<'_>>| {
            command_args(
                &format(),
                "127.0.0.1:1000".parse().unwrap(),
                "127.0.0.1:1001".parse().unwrap(),
                "video-secret",
                "audio-secret",
                wav,
                None,
                None,
                None,
            )
        };
        let strings = |args: &[OsString]| {
            args.iter()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        // A WAV sink receives the same output as the raw body of one
        // authenticated `PUT`.
        let mut expected_upload = strings(&args(None));
        expected_upload.extend(
            [
                "-map",
                "1:a:0",
                "-c:a",
                "pcm_s16le",
                "-f",
                "wav",
                "-protocol_whitelist",
                "http,tcp",
                "-method",
                "PUT",
                "-chunked_post",
                "0",
                "http://127.0.0.1:1002/wav-secret",
            ]
            .map(str::to_owned),
        );
        assert_eq!(
            strings(&args(Some(WavDestination::Upload {
                address: "127.0.0.1:1002".parse().unwrap(),
                token: "wav-secret",
            }))),
            expected_upload
        );
    }

    #[test]
    fn wav_upload_requests_need_the_put_method_and_a_raw_body() {
        let put =
            b"PUT /token HTTP/1.1\r\nUser-Agent: Lavf\r\nTransfer-Encoding: chunked\r\n\r\nRIFF";
        assert_eq!(request_target(put, "PUT"), Some("/token"));
        assert_eq!(request_target(put, "GET"), None);
        assert_eq!(request_path(put), None);
        let head_end = put
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert!(has_header(&put[..head_end], "transfer-encoding"));
        assert!(has_header(&put[..head_end], "TRANSFER-ENCODING"));
        assert!(!has_header(&put[..head_end], "content-length"));
        // The request line is never mistaken for a header.
        assert!(!has_header(
            b"PUT /transfer-encoding: HTTP/1.1\r\n\r\n",
            "put /transfer-encoding"
        ));
    }

    #[test]
    fn wav_sidecar_argument_is_a_literal_file_path() {
        // Without the `file:` protocol prefix, FFmpeg reads `take:2.wav` as an
        // unknown `take` protocol, `pipe:1` as the MP4 stdout pipe, and a
        // leading `-` as an option.
        for path in ["take:2.wav", "pipe:1", "-sidecar.wav"] {
            let args = command_args(
                &format(),
                "127.0.0.1:1000".parse().unwrap(),
                "127.0.0.1:1001".parse().unwrap(),
                "video-secret",
                "audio-secret",
                Some(WavDestination::File(Path::new(path))),
                None,
                None,
                None,
            );
            assert_eq!(
                args.last().unwrap(),
                &OsString::from(format!("file:{path}"))
            );
        }
    }

    #[test]
    fn bitrate_arguments_are_exact_and_only_when_configured() {
        let args = |video: Option<u32>, audio: Option<u32>| {
            command_args(
                &format(),
                "127.0.0.1:1000".parse().unwrap(),
                "127.0.0.1:1001".parse().unwrap(),
                "video-secret",
                "audio-secret",
                None,
                video,
                audio,
                None,
            )
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
        };

        let unconfigured = args(None, None);
        assert!(
            !unconfigured.iter().any(|argument| matches!(
                argument.as_str(),
                "-b:v" | "-maxrate" | "-bufsize" | "-b:a"
            )),
            "{unconfigured:?}"
        );

        let configured = args(Some(2_000), Some(96));
        let video_position = configured
            .iter()
            .position(|argument| argument == "yuv420p")
            .expect("the yuv420p pixel format is present");
        assert_eq!(
            configured[video_position + 1..video_position + 8]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "-b:v", "2000k", "-maxrate", "2000k", "-bufsize", "4000k", "-g"
            ]
        );
        let audio_position = configured
            .iter()
            .position(|argument| argument == "aac")
            .expect("the AAC encoder argument is present");
        assert_eq!(
            configured[audio_position + 1..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "-b:a",
                "96k",
                "-movflags",
                "+empty_moov+default_base_moof+frag_keyframe",
                "-flush_packets",
                "1",
                "-f",
                "mp4",
                "pipe:1"
            ]
        );

        let low = args(Some(MIN_VIDEO_BITRATE_KBPS), Some(MIN_AUDIO_BITRATE_KBPS));
        assert!(low.windows(2).any(|pair| pair == ["-b:v", "1000k"]));
        assert!(low.windows(2).any(|pair| pair == ["-b:a", "8k"]));
        let high = args(Some(MAX_VIDEO_BITRATE_KBPS), Some(MAX_AUDIO_BITRATE_KBPS));
        assert!(high.windows(2).any(|pair| pair == ["-b:v", "100000k"]));
        assert!(high.windows(2).any(|pair| pair == ["-bufsize", "200000k"]));
        assert!(high.windows(2).any(|pair| pair == ["-b:a", "1024k"]));
    }

    #[test]
    fn timecode_argument_is_exact_and_only_when_configured() {
        let args = |timecode: Option<Timecode>| {
            command_args(
                &format(),
                "127.0.0.1:1000".parse().unwrap(),
                "127.0.0.1:1001".parse().unwrap(),
                "video-secret",
                "audio-secret",
                None,
                None,
                None,
                timecode,
            )
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
        };

        let unconfigured = args(None);
        assert!(
            !unconfigured.iter().any(|argument| argument == "-timecode"),
            "{unconfigured:?}"
        );

        let zero = args(Some(Timecode::new(0, 0, 0, 0, 30).unwrap()));
        assert_eq!(
            zero[zero.len() - 5..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["-timecode", "00:00:00:00", "-f", "mp4", "pipe:1"]
        );

        let configured = args(Some(Timecode::new(1, 2, 3, 4, 30).unwrap()));
        assert_eq!(
            configured[configured.len() - 7..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "-flush_packets",
                "1",
                "-timecode",
                "01:02:03:04",
                "-f",
                "mp4",
                "pipe:1"
            ]
        );
        assert_eq!(
            configured
                .iter()
                .filter(|argument| *argument == "-timecode")
                .count(),
            1
        );
        assert!(RecordConfig::new(format()).timecode.is_none());
    }

    #[test]
    fn bitrate_validation_envelope_is_inclusive() {
        let mut config = RecordConfig::new(format());
        for (video, audio) in [
            (None, None),
            (Some(MIN_VIDEO_BITRATE_KBPS), Some(MIN_AUDIO_BITRATE_KBPS)),
            (Some(MAX_VIDEO_BITRATE_KBPS), Some(MAX_AUDIO_BITRATE_KBPS)),
        ] {
            config.video_bitrate_kbps = video;
            config.audio_bitrate_kbps = audio;
            assert_eq!(validate_bitrates(&config), Ok(()), "{video:?}/{audio:?}");
        }
        for (video, audio) in [
            (Some(MIN_VIDEO_BITRATE_KBPS - 1), None),
            (Some(MAX_VIDEO_BITRATE_KBPS + 1), None),
            (None, Some(MIN_AUDIO_BITRATE_KBPS - 1)),
            (None, Some(MAX_AUDIO_BITRATE_KBPS + 1)),
        ] {
            config.video_bitrate_kbps = video;
            config.audio_bitrate_kbps = audio;
            assert_eq!(
                validate_bitrates(&config).unwrap_err().kind,
                StartErrorKind::InvalidBitrate,
                "{video:?}/{audio:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_refuses_out_of_range_bitrates_without_spawning_a_child() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let marker = directory.path().join("child-ran");
        let stub = directory.path().join("ffmpeg-stub");
        fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        let mut permissions = fs::metadata(&stub).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions).unwrap();

        for (index, (video, audio)) in [
            (Some(999), None),
            (Some(100_001), None),
            (None, Some(7)),
            (None, Some(1_025)),
        ]
        .into_iter()
        .enumerate()
        {
            let mut config = RecordConfig::new(format());
            config.ffmpeg = Executable::Explicit(stub.clone());
            config.video_bitrate_kbps = video;
            config.audio_bitrate_kbps = audio;
            let output =
                File::create(directory.path().join(format!("output-{index}.mp4"))).unwrap();
            let Err(error) = Recorder::start(output, config) else {
                panic!("start accepted the out-of-range bitrate {video:?}/{audio:?}");
            };
            assert_eq!(error.kind, StartErrorKind::InvalidBitrate);
            assert_eq!(error.cleanup, CleanupStatus::Complete);
        }
        assert!(
            !marker.exists(),
            "start spawned the child before refusing an out-of-range bitrate"
        );
    }

    #[test]
    fn token_rejection_and_header_pressure_do_not_consume_endpoint() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = Endpoint {
            listener,
            path: "/right".to_owned(),
            input: MediaInput::Video,
        };
        let attacker = thread::spawn(move || {
            let mut wrong = TcpStream::connect(address).unwrap();
            wrong
                .write_all(b"GET /wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut oversized = TcpStream::connect(address).unwrap();
            let _ = oversized.write_all(&vec![b'x'; MAX_HTTP_HEADER_BYTES + 1]);
            let mut right = TcpStream::connect(address).unwrap();
            right
                .write_all(b"GET /right HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = [0_u8; 15];
            right.read_exact(&mut response).unwrap();
            assert_eq!(&response, b"HTTP/1.1 200 OK");
        });
        let shared = Shared::new(&format(), 64, [String::new(), String::new()]);
        let stream = accept_http_until(
            &endpoint,
            Instant::now() + Duration::from_secs(2),
            Duration::from_millis(100),
            &shared,
        )
        .unwrap();
        drop(stream);
        attacker.join().unwrap();
    }

    #[test]
    fn slowloris_cannot_extend_absolute_header_deadline() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        // A dripping peer below the header terminator is read directly: the
        // accept loop cannot make this deterministic because a read that
        // stalls just before the deadline is classified as a retryable
        // connection, not as the deadline itself.
        let (dripping_sender, dripping_receiver) = mpsc::channel();
        let attacker = thread::Builder::new()
            .name("slowloris-test".to_owned())
            .spawn(move || {
                let mut stream = TcpStream::connect(address).unwrap();
                for _ in 0..200 {
                    if stream.write_all(b"x").is_err() {
                        return;
                    }
                    let _ = dripping_sender.send(());
                    thread::sleep(Duration::from_millis(5));
                }
            })
            .unwrap();
        let connect_deadline = Instant::now() + Duration::from_secs(5);
        let (mut server, _) = loop {
            match listener.accept() {
                Ok(value) => break value,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < connect_deadline,
                        "slowloris client never connected"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => panic!("slowloris listener failed: {error}"),
            }
        };
        dripping_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("slowloris client sent its first byte");
        // The no-progress limit is deliberately longer than the absolute
        // deadline: a dripping peer must be cut off by the absolute deadline,
        // not by an unrelated per-read stall.
        let started = Instant::now();
        let result = read_http_request(
            &mut server,
            started + Duration::from_millis(80),
            Duration::from_secs(1),
        );
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "slowloris extended the absolute header deadline: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                result,
                Err(HttpRequestError::Timeout
                    | HttpRequestError::Io(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
            ),
            "unexpected slowloris result: {result:?}"
        );
        drop(server);
        attacker.join().unwrap();
    }

    #[test]
    fn output_must_be_empty_and_owned() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("output");
        fs::write(&path, b"not empty").unwrap();
        let error =
            prepare_output(File::options().read(true).write(true).open(path).unwrap()).unwrap_err();
        assert_eq!(error.kind, StartErrorKind::OutputNotEmpty { bytes: 9 });
    }

    #[cfg(unix)]
    #[test]
    fn start_refuses_an_existing_wav_sidecar_without_spawning_a_child() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let marker = directory.path().join("child-ran");
        let stub = directory.path().join("ffmpeg-stub");
        fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        let mut permissions = fs::metadata(&stub).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions).unwrap();

        let wav = directory.path().join("sidecar.wav");
        fs::write(&wav, b"already here").unwrap();
        let mut config = RecordConfig::new(format());
        config.ffmpeg = Executable::Explicit(stub);
        config.wav = Some(wav.clone());
        let output = File::create(directory.path().join("output.mp4")).unwrap();
        let Err(error) = Recorder::start(output, config) else {
            panic!("start accepted an existing WAV sidecar");
        };
        assert_eq!(
            error.kind,
            StartErrorKind::WavOutput(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(error.cleanup, CleanupStatus::Complete);
        assert_eq!(fs::read(&wav).unwrap(), b"already here");
        assert!(
            !marker.exists(),
            "start spawned the child before refusing the existing sidecar"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_start_removes_the_wav_sidecar_it_created() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let stub = directory.path().join("ffmpeg-stub");
        fs::write(&stub, "#!/bin/sh\nexit 3\n").unwrap();
        let mut permissions = fs::metadata(&stub).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions).unwrap();

        let wav = directory.path().join("sidecar.wav");
        let mut config = RecordConfig::new(format());
        config.ffmpeg = Executable::Explicit(stub);
        config.wav = Some(wav.clone());
        let output = File::create(directory.path().join("output.mp4")).unwrap();
        let Err(error) = Recorder::start(output, config.clone()) else {
            panic!("start accepted a child that exits immediately");
        };
        assert!(
            !matches!(error.kind, StartErrorKind::WavOutput(_)),
            "{error:?}"
        );
        assert!(
            !wav.exists(),
            "a failed start left its exclusive sidecar behind"
        );

        // A retry is therefore not refused as `AlreadyExists`.
        let output = File::create(directory.path().join("output-retry.mp4")).unwrap();
        let Err(error) = Recorder::start(output, config) else {
            panic!("start accepted a child that exits immediately");
        };
        assert!(
            !matches!(error.kind, StartErrorKind::WavOutput(_)),
            "{error:?}"
        );
    }

    #[test]
    fn over_allocated_rgba_is_charged_within_the_pair_budget() {
        let format = format();
        let sequence = SequenceNumber::new(10);
        let samples = format.expected_samples(sequence).unwrap();
        let mut rgba = Vec::with_capacity(format.rgba_bytes_per_frame() * 64);
        rgba.resize(format.rgba_bytes_per_frame(), 0);
        let frame =
            PairedFrame::new(&format, sequence, rgba, audio(&format, sequence, samples)).unwrap();
        assert!(frame.retained_bytes() <= format.maximum_pair_bytes().unwrap());
    }

    #[test]
    fn output_and_stderr_workers_retry_interrupted_reads() {
        struct InterruptOnce {
            interrupted: bool,
            bytes: Option<Vec<u8>>,
        }

        impl Read for InterruptOnce {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                let Some(bytes) = self.bytes.take() else {
                    return Ok(0);
                };
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
        }

        let format = format();
        let shared = Arc::new(Shared::new(&format, 1024, [String::new(), String::new()]));
        let directory = tempdir().unwrap();
        let path = directory.path().join("interrupted-output");
        let result = spawn_output(
            InterruptOnce {
                interrupted: false,
                bytes: Some(b"media".to_vec()),
            },
            File::create(&path).unwrap(),
            Arc::clone(&shared),
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(result.finalization, OutputFinalization::Synced);
        assert_eq!(fs::read(&path).unwrap(), b"media");
        spawn_stderr(
            InterruptOnce {
                interrupted: false,
                bytes: Some(b"late stderr".to_vec()),
            },
            Arc::clone(&shared),
        )
        .unwrap()
        .join()
        .unwrap();
        let snapshot = shared.snapshot();
        assert!(snapshot.failure.is_none(), "{:?}", snapshot.failure);
    }

    #[test]
    fn output_worker_drives_a_sink_and_finishes_it_exactly_once() {
        #[derive(Default)]
        struct Calls {
            written: Vec<u8>,
            writes: usize,
            flushes: usize,
            finishes: usize,
        }

        struct Recording {
            calls: Arc<Mutex<Calls>>,
            fail_after: usize,
        }

        impl RecordSink for Recording {
            fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
                let mut calls = self.calls.lock().unwrap();
                calls.writes += 1;
                if calls.writes > self.fail_after {
                    return Err(io::Error::from(io::ErrorKind::StorageFull));
                }
                calls.written.extend_from_slice(bytes);
                Ok(())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.calls.lock().unwrap().flushes += 1;
                Ok(())
            }

            fn finish(&mut self) -> io::Result<()> {
                self.calls.lock().unwrap().finishes += 1;
                Ok(())
            }
        }

        struct Chunks(VecDeque<Vec<u8>>);

        impl Read for Chunks {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let Some(chunk) = self.0.pop_front() else {
                    return Ok(0);
                };
                buffer[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }

        let format = format();
        let chunks = || {
            Chunks(VecDeque::from([
                b"ab".to_vec(),
                b"cd".to_vec(),
                b"ef".to_vec(),
            ]))
        };

        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        let calls = Arc::new(Mutex::new(Calls::default()));
        let sink: Box<dyn RecordSink> = Box::new(Recording {
            calls: Arc::clone(&calls),
            fail_after: usize::MAX,
        });
        let result = spawn_output(chunks(), sink, Arc::clone(&shared))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(result.finalization, OutputFinalization::Synced);
        assert_eq!(shared.snapshot().output_bytes, 6);
        assert!(shared.snapshot().failure.is_none());
        let seen = calls.lock().unwrap();
        assert_eq!(
            (
                seen.written.as_slice(),
                seen.writes,
                seen.flushes,
                seen.finishes
            ),
            (&b"abcdef"[..], 3, 1, 1)
        );
        drop(seen);

        // A failed write is sticky: no later chunk reaches the sink, and the
        // sink is still finished once so it can close its session.
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        let calls = Arc::new(Mutex::new(Calls::default()));
        let result = spawn_output(
            chunks(),
            Recording {
                calls: Arc::clone(&calls),
                fail_after: 1,
            },
            Arc::clone(&shared),
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(
            result.finalization,
            OutputFinalization::Failed(io::ErrorKind::StorageFull)
        );
        assert_eq!(
            shared.snapshot().failure,
            Some(RecordFailure::OutputWrite(io::ErrorKind::StorageFull))
        );
        let seen = calls.lock().unwrap();
        assert_eq!(
            (seen.written.as_slice(), seen.writes, seen.finishes),
            (&b"ab"[..], 2, 1)
        );
    }

    #[test]
    fn wav_worker_authenticates_the_upload_and_copies_it_byte_exactly_once() {
        #[derive(Default)]
        struct Calls {
            written: Vec<u8>,
            writes: usize,
            flushes: usize,
            finishes: usize,
        }

        struct Capture {
            calls: Arc<Mutex<Calls>>,
            fail_after: usize,
        }

        impl RecordSink for Capture {
            fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
                let mut calls = self.calls.lock().unwrap();
                calls.writes += 1;
                if calls.writes > self.fail_after {
                    return Err(io::Error::from(io::ErrorKind::StorageFull));
                }
                calls.written.extend_from_slice(bytes);
                Ok(())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.calls.lock().unwrap().flushes += 1;
                Ok(())
            }

            fn finish(&mut self) -> io::Result<()> {
                self.calls.lock().unwrap().finishes += 1;
                Ok(())
            }
        }

        let run = |fail_after: usize| {
            let format = format();
            let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
            let calls = Arc::new(Mutex::new(Calls::default()));
            let (listener, address) = bind_wav_listener().unwrap();
            let worker = spawn_wav_output(
                WavEndpoint {
                    listener,
                    path: "/wav-token".to_owned(),
                },
                Box::new(Capture {
                    calls: Arc::clone(&calls),
                    fail_after,
                }),
                RecordLimits::default(),
                Arc::clone(&shared),
            )
            .unwrap();

            // A wrong token and a GET are refused; waiting continues.
            for request in [
                &b"PUT /other HTTP/1.1\r\n\r\nRIFF"[..],
                &b"GET /wav-token HTTP/1.1\r\n\r\n"[..],
            ] {
                let mut client = TcpStream::connect(address).unwrap();
                client.write_all(request).unwrap();
                let mut response = Vec::new();
                client.read_to_end(&mut response).unwrap();
                assert!(response.starts_with(b"HTTP/1.1 404"), "{response:?}");
            }

            let mut body = Vec::new();
            for index in 0_u32..50_000 {
                body.extend_from_slice(&index.to_le_bytes());
            }
            let mut client = TcpStream::connect(address).unwrap();
            // Body bytes arrive in the same packet as the request head.
            let mut first = b"PUT /wav-token HTTP/1.1\r\nConnection: close\r\n\r\n".to_vec();
            first.extend_from_slice(&body[..10]);
            client.write_all(&first).unwrap();
            for chunk in body[10..].chunks(7_777) {
                client.write_all(chunk).unwrap();
            }
            drop(client);
            worker.join().unwrap();
            let calls = Arc::try_unwrap(calls).ok().unwrap().into_inner().unwrap();
            (shared, calls, body)
        };

        let (shared, calls, body) = run(usize::MAX);
        assert!(
            calls.written == body,
            "the sink sees the body byte for byte"
        );
        assert_eq!((calls.flushes, calls.finishes), (1, 1));
        assert_eq!(
            shared.wav_sink_bytes.load(Ordering::Acquire),
            body.len() as u64
        );
        assert!(shared.snapshot().failure.is_none());

        // A sink failure is sticky and typed; the rest of the upload is
        // drained, and the sink is still finished once.
        let (shared, calls, body) = run(1);
        assert!(!calls.written.is_empty() && calls.written.len() < body.len());
        assert!(body.starts_with(&calls.written));
        assert_eq!(
            calls.writes, 2,
            "nothing reaches the sink after its failure"
        );
        assert_eq!((calls.flushes, calls.finishes), (1, 1));
        assert_eq!(
            shared.snapshot().failure,
            Some(RecordFailure::WavOutput(io::ErrorKind::StorageFull))
        );
    }

    #[test]
    fn wav_worker_finishes_its_sink_when_the_child_exits_without_an_upload() {
        struct Finished(Arc<AtomicUsize>);

        impl RecordSink for Finished {
            fn write_all(&mut self, _bytes: &[u8]) -> io::Result<()> {
                Ok(())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }

            fn finish(&mut self) -> io::Result<()> {
                self.0.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        }

        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        let finishes = Arc::new(AtomicUsize::new(0));
        let (listener, _) = bind_wav_listener().unwrap();
        let worker = spawn_wav_output(
            WavEndpoint {
                listener,
                path: "/wav-token".to_owned(),
            },
            Box::new(Finished(Arc::clone(&finishes))),
            RecordLimits::default(),
            Arc::clone(&shared),
        )
        .unwrap();
        shared.lock().child = Some(ChildResult {
            success: true,
            code: Some(0),
        });
        worker.join().unwrap();
        assert_eq!(finishes.load(Ordering::Acquire), 1);
        assert_eq!(
            shared.snapshot().failure,
            Some(RecordFailure::WavOutput(io::ErrorKind::NotConnected))
        );
    }

    #[test]
    fn resilient_output_refuses_a_non_fragmented_stream_and_closes_its_session() {
        use fm_record::{
            FileStorage, RecordingSession, ResilientWriterConfig, SessionConfig, SessionManifest,
            SessionState, UnknownSpace,
        };

        let format = format();
        let directory = tempdir().unwrap();
        let config = SessionConfig::new("garbage", directory.path())
            .unwrap()
            .with_bare_first_segment();
        let session = RecordingSession::start(config, FileStorage::new(UnknownSpace)).unwrap();
        let output = ResilientOutput::new(session, ResilientWriterConfig::default());
        let status = output.status();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        // A box header that is not `ftyp`: not a relocatable fragmented MP4.
        let mut garbage = vec![0, 0, 0, 16];
        garbage.extend_from_slice(b"mdat");
        garbage.extend_from_slice(&[0; 8]);
        let result = spawn_output(io::Cursor::new(garbage), output, Arc::clone(&shared))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(
            shared.snapshot().failure,
            Some(RecordFailure::OutputWrite(io::ErrorKind::InvalidData))
        );
        assert_eq!(
            result.finalization,
            OutputFinalization::Failed(io::ErrorKind::InvalidData)
        );
        let state = status.get();
        assert!(state.finished);
        assert!(state.error.is_some(), "{state:?}");
        assert!(state.segments.is_empty(), "nothing playable was written");
        assert!(!directory.path().join("garbage.mp4").exists());
        let manifest = SessionManifest::parse(
            &fs::read_to_string(directory.path().join("garbage.fmsession")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.state, SessionState::Stopped);
    }

    #[test]
    fn a_dropped_resilient_output_still_closes_its_session() {
        use fm_record::{
            FileStorage, RecordingSession, ResilientWriterConfig, SessionConfig, SessionManifest,
            SessionState, UnknownSpace,
        };

        let directory = tempdir().unwrap();
        let config = SessionConfig::new("unused", directory.path()).unwrap();
        let session = RecordingSession::start(config, FileStorage::new(UnknownSpace)).unwrap();
        let output = ResilientOutput::new(session, ResilientWriterConfig::default());
        let status = output.status();
        assert!(!status.get().finished);
        drop(output);
        assert!(status.get().finished);
        assert_eq!(status.get().snapshot.state, SessionState::Stopped);
        let manifest = SessionManifest::parse(
            &fs::read_to_string(directory.path().join("unused.fmsession")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.state, SessionState::Stopped);
    }

    #[test]
    fn wav_bytes_default_to_zero_and_measure_a_configured_sidecar() {
        let format = format();
        let directory = tempdir().unwrap();

        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = fake_recorder(&format, RecordLimits::default(), shared, sender);
        let report = recorder.cancel();
        assert_eq!(report.wav_bytes, 0);
        assert_eq!(report.telemetry.wav_bytes, 0);
        drop(recorder);
        drop(receiver);

        let wav = directory.path().join("sidecar.wav");
        fs::write(&wav, vec![7_u8; 100]).unwrap();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = fake_recorder(&format, RecordLimits::default(), shared, sender);
        recorder.wav = Some(wav);
        let report = recorder.cancel();
        assert_eq!(report.wav_bytes, 100);
        assert_eq!(report.telemetry.wav_bytes, 100);
        assert_eq!(recorder.telemetry(), report.telemetry);
        drop(recorder);
        drop(receiver);
    }

    #[test]
    fn paired_frame_rejects_length_sequence_and_sample_span() {
        let format = format();
        let sequence = SequenceNumber::new(10);
        assert!(matches!(
            PairedFrame::new(
                &format,
                sequence,
                vec![0; 15],
                audio(&format, sequence, 800)
            ),
            Err(FrameError::RgbaLength {
                expected: 16,
                actual: 15
            })
        ));
        assert!(matches!(
            PairedFrame::new(
                &format,
                sequence,
                vec![0; 16],
                audio(&format, SequenceNumber::new(11), 800)
            ),
            Err(FrameError::AudioSequence { .. })
        ));
        assert_eq!(
            PairedFrame::new(
                &format,
                sequence,
                vec![0; 16],
                audio(&format, sequence, 799)
            )
            .unwrap_err(),
            FrameError::SampleSpan {
                expected: 800,
                actual: 799
            }
        );
    }

    #[test]
    fn queue_full_returns_ownership_without_advancing_sequence() {
        let format = format();
        let limits = RecordLimits {
            max_outstanding_pairs: 1,
            ..RecordLimits::default()
        };
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = fake_recorder(&format, limits, shared, sender);
        recorder.enqueue(paired(&format, 10)).unwrap();
        let error = recorder.enqueue(paired(&format, 11)).unwrap_err();
        assert_eq!(error.reason, EnqueueRejection::QueueFull);
        assert_eq!(error.into_frame().sequence(), SequenceNumber::new(11));
        assert_eq!(
            recorder.shared.lock().next_sequence,
            SequenceNumber::new(11)
        );
        suppress_fake_drop(&mut recorder);
        drop(recorder);
        drop(receiver);
    }

    #[test]
    fn enqueue_and_failure_linearize_under_one_lock() {
        let format = format();
        let limits = RecordLimits::default();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let (sender, receiver) = mpsc::sync_channel(2);
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut recorder = Recorder {
            format: format.clone(),
            limits,
            wav: None,
            wav_sink: false,
            sender: Some(sender),
            shared: Arc::clone(&shared),
            child: Arc::new(Mutex::new(child)),
            workers: Vec::new(),
            output_worker: None,
            monitor: None,
            report: None,
        };
        let guard = shared.lock();
        let failing = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.fail(RecordFailure::OutputWrite(io::ErrorKind::Other)))
        };
        drop(guard);
        failing.join().unwrap();
        let error = recorder.enqueue(paired(&format, 10)).unwrap_err();
        assert_eq!(
            error.reason,
            EnqueueRejection::Failed(RecordFailure::OutputWrite(io::ErrorKind::Other))
        );
        let telemetry = recorder.telemetry();
        recorder.report = Some(StopReport {
            outcome: StopOutcome::Failed,
            exit_status: None,
            output: OutputFinalization::Unconfirmed,
            cleanup: CleanupStatus::Unconfirmed,
            wav_bytes: 0,
            telemetry,
        });
        drop(recorder);
        drop(receiver);
    }

    #[test]
    fn accepted_pair_can_linearize_before_sticky_failure() {
        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut recorder = fake_recorder(
            &format,
            RecordLimits::default(),
            Arc::clone(&shared),
            sender,
        );
        recorder.enqueue(paired(&format, 10)).unwrap();
        shared.fail(RecordFailure::OutputWrite(io::ErrorKind::Other));
        let error = recorder.enqueue(paired(&format, 11)).unwrap_err();
        assert!(matches!(error.reason, EnqueueRejection::Failed(_)));
        assert_eq!(recorder.telemetry().accepted_pairs, 1);
        suppress_fake_drop(&mut recorder);
        drop(recorder);
        drop(receiver);
    }

    #[test]
    fn frozen_telemetry_ignores_stale_worker_writes() {
        let shared = Shared::new(&format(), 8, ["secret".to_owned(), String::new()]);
        {
            let mut data = shared.lock();
            data.state = RecorderState::Stopped;
            data.frozen = true;
        }
        let before = shared.snapshot();
        shared.fail(RecordFailure::VideoWrite(io::ErrorKind::BrokenPipe));
        shared.append_stderr(b"secret late stderr");
        shared.add_output_bytes(100);
        assert_eq!(shared.snapshot(), before);
    }

    #[test]
    fn limits_and_environment_are_bounded() {
        let format = format();
        let required = format.maximum_pair_bytes().unwrap() * 2;
        assert!(matches!(
            validate_limits(
                &format,
                RecordLimits {
                    max_retained_bytes: required - 1,
                    ..RecordLimits::default()
                }
            ),
            Err(LimitsError::RetainedBytesTooSmall { .. })
        ));
        assert!(!retained_environment_names().contains(&"FFREPORT"));
    }

    #[test]
    fn second_input_timeout_starts_only_after_a_job() {
        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = Endpoint {
            listener,
            path: "/idle".to_owned(),
            input: MediaInput::Audio,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = spawn_pending_writer(
            endpoint,
            receiver,
            RecordLimits {
                connect_timeout: Duration::from_millis(20),
                ..RecordLimits::default()
            },
            Arc::clone(&shared),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(shared.snapshot().failure, None);
        {
            let mut data = shared.lock();
            data.shutdown = Some(Shutdown {
                mode: ShutdownMode::Cancel,
                deadline: Instant::now() + Duration::from_millis(20),
            });
        }
        drop(sender);
        worker.join().unwrap();
        assert_eq!(shared.snapshot().failure, None);
    }

    #[test]
    fn stderr_flood_and_output_failure_remain_bounded_and_sticky() {
        let format = format();
        let shared = Arc::new(Shared::new(
            &format,
            8,
            ["secret".to_owned(), String::new()],
        ));
        spawn_stderr(
            io::Cursor::new(vec![b'x'; 1024 * 1024]),
            Arc::clone(&shared),
        )
        .unwrap()
        .join()
        .unwrap();
        let telemetry = shared.snapshot();
        assert_eq!(telemetry.stderr_tail, "xxxxxxxx");
        assert!(telemetry.stderr_truncated);

        let directory = tempdir().unwrap();
        let path = directory.path().join("read-only");
        fs::write(&path, b"").unwrap();
        let result = spawn_output(
            io::Cursor::new(vec![1_u8; 128]),
            File::open(path).unwrap(),
            Arc::clone(&shared),
        )
        .unwrap()
        .join()
        .unwrap();
        let Some(RecordFailure::OutputWrite(kind)) = shared.snapshot().failure else {
            panic!("expected sticky output write failure");
        };
        assert_eq!(result.finalization, OutputFinalization::Failed(kind));
    }

    #[test]
    fn complete_startup_cleanup_captures_final_early_exit_stderr() {
        let format = format();
        let shared = Arc::new(Shared::new(&format, 1024, [String::new(), String::new()]));
        let child = Arc::new(Mutex::new(helper_child("sleep")));
        let (monitor_sender, monitor) = spawn_monitor_waiter().unwrap();
        monitor_sender
            .send((Arc::clone(&child), Arc::clone(&shared)))
            .unwrap();
        drop(monitor_sender);
        let stderr = spawn_stderr(
            DelayedEof {
                delay: Duration::from_millis(20),
                bytes: Some(b"final child stderr".to_vec()),
            },
            Arc::clone(&shared),
        )
        .unwrap();
        let directory = tempdir().unwrap();
        let output = spawn_output(
            io::Cursor::new(Vec::<u8>::new()),
            File::create(directory.path().join("early-output")).unwrap(),
            Arc::clone(&shared),
        )
        .unwrap();
        let error = cleanup_startup(
            StartErrorKind::EarlyExit {
                status: Some(1),
                stderr: "stale".to_owned(),
            },
            &child,
            &shared,
            vec![stderr],
            Some(output),
            monitor,
            Duration::from_secs(1),
        );
        assert_eq!(error.cleanup, CleanupStatus::Complete);
        assert_eq!(
            error.kind,
            StartErrorKind::EarlyExit {
                status: Some(1),
                stderr: "final child stderr".to_owned()
            }
        );
    }

    #[test]
    fn stop_freezes_an_unconfirmed_slow_output_report() {
        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let child = Arc::new(Mutex::new(helper_child("sleep")));
        let (monitor_sender, monitor) = spawn_monitor_waiter().unwrap();
        monitor_sender
            .send((Arc::clone(&child), Arc::clone(&shared)))
            .unwrap();
        drop(monitor_sender);
        let directory = tempdir().unwrap();
        let output_worker = spawn_output(
            DelayedEof {
                delay: Duration::from_millis(250),
                bytes: None,
            },
            File::create(directory.path().join("slow-output")).unwrap(),
            Arc::clone(&shared),
        )
        .unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = Recorder {
            format,
            limits: RecordLimits {
                stop_timeout: Duration::from_millis(20),
                kill_timeout: Duration::from_millis(40),
                ..RecordLimits::default()
            },
            wav: None,
            wav_sink: false,
            sender: Some(sender),
            shared,
            child,
            workers: Vec::new(),
            output_worker: Some(output_worker),
            monitor: Some(monitor),
            report: None,
        };
        let started = Instant::now();
        let report = recorder.stop();
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(report.output, OutputFinalization::Unconfirmed);
        assert_eq!(report.cleanup, CleanupStatus::Unconfirmed);
        assert_eq!(report.telemetry.state, RecorderState::Failed);
        let frozen = report.telemetry.clone();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(recorder.telemetry(), frozen);
        assert_eq!(recorder.stop(), report);
        drop(receiver);
    }

    #[test]
    fn request_cancel_is_immediate_and_later_cleanup_is_idempotent() {
        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        shared.lock().state = RecorderState::Recording;
        let child = Arc::new(Mutex::new(helper_child("sleep")));
        let (monitor_sender, monitor) = spawn_monitor_waiter().unwrap();
        monitor_sender
            .send((Arc::clone(&child), Arc::clone(&shared)))
            .unwrap();
        drop(monitor_sender);
        let directory = tempdir().unwrap();
        let output_worker = spawn_output(
            DelayedEof {
                delay: Duration::from_millis(50),
                bytes: None,
            },
            File::create(directory.path().join("cancel-output")).unwrap(),
            Arc::clone(&shared),
        )
        .unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut recorder = Recorder {
            format: format.clone(),
            limits: RecordLimits {
                kill_timeout: Duration::from_secs(1),
                ..RecordLimits::default()
            },
            wav: None,
            wav_sink: false,
            sender: Some(sender),
            shared,
            child,
            workers: Vec::new(),
            output_worker: Some(output_worker),
            monitor: Some(monitor),
            report: None,
        };
        let child = Arc::clone(&recorder.child);
        let shared = Arc::clone(&recorder.shared);
        let child_guard = child.lock().unwrap();
        let shared_guard = shared.lock();
        let started = Instant::now();
        recorder.request_cancel();
        assert!(started.elapsed() < Duration::from_millis(100));
        recorder.request_cancel();
        drop(shared_guard);
        assert_eq!(
            recorder.enqueue(paired(&format, 10)).unwrap_err().reason,
            EnqueueRejection::Failed(RecordFailure::Cancelled)
        );
        drop(child_guard);

        let report = recorder.stop();
        assert_eq!(report.outcome, StopOutcome::Killed, "{report:?}");
        assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
        assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
        assert_eq!(report.telemetry.failure, Some(RecordFailure::Cancelled));
        assert_eq!(recorder.stop(), report);
        assert_eq!(recorder.cancel(), report);
        assert_eq!(recorder.telemetry(), report.telemetry);
        drop(receiver);
    }

    #[test]
    fn blocked_writer_and_fallback_reap_return_boundedly() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let _peer = TcpStream::connect(address).unwrap();
        let (stream, _) = loop {
            match listener.accept() {
                Ok(value) => break value,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let format = format();
        let shared = Arc::new(Shared::new(&format, 64, [String::new(), String::new()]));
        let completion = Arc::new(Completion {
            shared: Arc::clone(&shared),
            bytes: 16 * 1024 * 1024,
            remaining: AtomicUsize::new(1),
            successful: AtomicBool::new(true),
        });
        let (sender, receiver) = mpsc::sync_channel(1);
        let endpoint = Endpoint {
            listener,
            path: "/unused".to_owned(),
            input: MediaInput::Video,
        };
        let worker = spawn_writer(endpoint, stream, receiver, Arc::clone(&shared));
        let worker = worker.unwrap();
        sender
            .send(WriteJob {
                bytes: vec![0; 16 * 1024 * 1024],
                completion: Some(completion),
            })
            .unwrap();
        drop(sender);
        worker.join().unwrap();
        assert!(matches!(
            shared.snapshot().failure,
            Some(RecordFailure::VideoWrite(
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ))
        ));

        let started = Instant::now();
        assert_eq!(
            terminate_unshared(helper_child("sleep"), Duration::from_nanos(1)),
            CleanupStatus::Unconfirmed
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
