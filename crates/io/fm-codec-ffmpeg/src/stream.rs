//! Bounded RTMP/RTMPS/SRT live streaming and local HLS output through one
//! direct `FFmpeg` child.
//!
//! The streamer feeds one `FFmpeg` child tightly packed RGBA8 video frames and
//! sample-major interleaved `f32le` audio over two authenticated loopback HTTP
//! inputs, and muxes `-f flv` to one `rtmp://` or `rtmps://` destination,
//! `-f mpegts` to one `srt://host[:port]?streamid=KEY` destination, or
//! `-f hls` into the absolute local directory named by an
//! `hls:///absolute/path` destination — the LAN-equivalent local HLS output. An
//! HLS destination may additionally carry a multi-bitrate [`HlsLadder`]: the
//! one child then scales and encodes 2..=8 renditions of the same decoded
//! frame, and the segmenter publishes one master playlist plus one variant
//! playlist per rung. It reuses [`RecordFormat`] and [`PairedFrame`] from
//! [`crate::record`], which
//! own cadence, layout, and per-pair timing validation. The recorder's private
//! process plumbing (loopback HTTP handshake, child reaping, fallback cleanup)
//! is not reachable across module boundaries, so the equivalent bounded
//! plumbing is restated here.
//!
//! # Secret handling
//!
//! The destination carries a stream key: an RTMP path tail or an SRT
//! `streamid`. An HLS destination carries no secret at all, so its redacted
//! form is the URL itself and the key machinery must not mask any part of the
//! directory path. [`StreamDestination`] keeps the key
//! private: its `Debug` and `Display` render the redacted form only, every
//! typed error is path- and key-free, and child stderr is redacted *before* it
//! enters the bounded retention ring, so neither truncation nor a read split
//! across chunk boundaries can expose a partial key. The key still appears in
//! the child's argv, and on Linux `/proc/<pid>/cmdline` is mode 0444: unless
//! the operating system is configured to hide it, **any** local user can read
//! the key while the child runs, not merely a process running as the same user.
//! This crate cannot close that hole. Containing it is an OS-level measure
//! (`hidepid=2`, a PID namespace, or a dedicated unprivileged user) and lies
//! outside this crate's isolation boundary, exactly as for the recorder's
//! loopback input tokens.
//!
//! # Loss is explicit, and the media clock is not allowed to drift
//!
//! Admission is bounded by pair count and retained bytes. Every refusal or
//! discard is classified and counted in [`StreamTelemetry`]; nothing is lost
//! silently. Dropping or skipping a pair removes exactly one video frame and
//! its matching audio span, so audio and video stay mutually aligned. Because
//! the child derives presentation timestamps from the *count* of raw frames and
//! samples it receives, a pair that is simply omitted would also shorten the
//! muxed timeline by one frame period and leave the stream permanently behind
//! wall clock. The dispatcher therefore pads every gap in the dispatched
//! sequence: it repeats the last delivered video frame and emits the matching
//! span of silence, counted as `padded_pairs`. Residual divergence between the
//! child's muxed media clock and wall clock is reported as
//! [`StreamTelemetry::media_drift`] and, once it exceeds `no_progress_timeout`,
//! becomes a terminal [`StreamFailure::NoProgress`].
//!
//! # Explicitly out of scope for this slice
//!
//! Automatic reconnection is **not** implemented and will not be: the first
//! fatal cause is sticky, every later call reports it, and retry policy belongs
//! to the caller, which owns backoff, destination rotation, and whether a new
//! [`Streamer`] should be started at all. Also out of scope: per-output wiring
//! into the engine, hardware encoders, and TLS certificate policy (`rtmps://`
//! uses the child's default trust configuration), and SRT encryption and
//! passphrase policy (`srt://` likewise uses the child's defaults).

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_frame::SequenceNumber;
use fm_types::{Channel, ChannelLayout};

use crate::record::PROBE_SIZE;
pub use crate::record::{CleanupStatus, MediaInput, PairedFrame, RecordFormat};
use crate::{Executable, UnavailableReason};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const WRITE_POLL: Duration = Duration::from_millis(100);
/// Bounded window a failing input writer allows the reaper so the sticky cause
/// can name the child's exit rather than the broken pipe it caused.
const WRITE_ATTRIBUTION_WINDOW: Duration = Duration::from_millis(50);
const MAX_HTTP_HEADER_BYTES: usize = 8 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const STDERR_PENDING_BYTES: usize = 8 * 1024;
const PROGRESS_PENDING_BYTES: usize = 4 * 1024;
const TOKEN_BYTES: usize = 32;
const MAX_DESTINATION_BYTES: usize = 2 * 1024;
const MIN_STREAM_KEY_BYTES: usize = 4;
/// The only query parameter an SRT destination may carry.
const STREAM_ID_PARAM: &str = "streamid";
/// The only scheme that names a local directory instead of a network peer.
const HLS_SCHEME: &str = "hls://";
/// How many renditions one HLS ladder may carry: a ladder is a choice, and
/// every rung is another video encoder and audio stream inside the one child.
const MIN_LADDER_RUNGS: usize = 2;
const MAX_LADDER_RUNGS: usize = 8;
/// Bitrate envelope of one ladder rung, in kbps, matching the single-rendition
/// encoder's own bound.
const MIN_LADDER_BITRATE_KBPS: u32 = 1_000;
const MAX_LADDER_BITRATE_KBPS: u32 = 100_000;
/// Dimension envelope of one ladder rung. The lower bound is the smallest
/// macroblock-bearing frame; the upper bound is 8K UHD.
const MIN_LADDER_DIMENSION: u32 = 16;
const MAX_LADDER_WIDTH: u32 = 7_680;
const MAX_LADDER_HEIGHT: u32 = 4_320;
const REDACTED_KEY: &str = "****";
const REDACTED_TOKEN: &[u8] = b"<input-token>";
const STATS_PERIOD: &str = "0.25";
/// Backlog each input writer may hold beyond the payload it is actively
/// writing.
///
/// Zero (a rendezvous) is wrong: the dispatcher must be able to hand the child
/// the next pair's audio while the previous pair's video is still draining into
/// the socket, or the two inputs can only advance in lockstep with whichever
/// one the child happens to be reading. Anything large is also wrong: a pair
/// sitting in a writer channel is still accounted in `outstanding`, but
/// [`OverflowPolicy`] can no longer recall it, so a deep writer channel would
/// quietly move the sink's loss policy out of the caller's reach.
const WRITER_BACKLOG: usize = 1;

/// Why a destination URL was refused before any process was spawned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationError {
    Empty,
    TooLong,
    /// The URL contains whitespace, control, or non-ASCII bytes.
    InvalidCharacter,
    /// Only `rtmp://`, `rtmps://`, and `srt://` are accepted.
    UnsupportedScheme,
    MissingHost,
    /// `user:password@host` credentials are refused; they cannot be redacted
    /// as reliably as a trailing stream key.
    EmbeddedCredentials,
    /// The URL has no `/app/key` shape, so no segment can be treated as secret.
    MissingStreamKey,
    /// The stream key is too short to be substituted out of captured text
    /// without mangling unrelated output.
    StreamKeyTooShort,
    /// An `srt://` destination is not exactly `host[:port]` plus at most one
    /// `?streamid=<key>` parameter: a path component, a second `?`, an unknown
    /// parameter name, or extra parameters are all refused.
    MalformedSrtUrl,
    /// An `hls://` destination names a local directory, so the text after the
    /// scheme must start with `/`.
    PathNotAbsolute,
    /// The `hls://` path is not a plain sequence of named segments: an empty
    /// segment (`//`), a trailing `/`, or a `.` or `..` segment is refused.
    MalformedHlsPath,
}

impl fmt::Display for DestinationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid streaming destination: {self:?}")
    }
}

impl std::error::Error for DestinationError {}

/// The validated transport a destination publishes over, which selects the
/// muxer the child is driven with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationKind {
    /// `rtmp://`, muxed as FLV.
    Rtmp,
    /// `rtmps://`, muxed as FLV over the child's default TLS trust.
    Rtmps,
    /// `srt://`, muxed as MPEG-TS.
    Srt,
    /// `hls:///absolute/path`, muxed as MPEG-TS segments plus a playlist in
    /// that local directory. A local destination has no peer and no secret.
    Hls,
}

/// One validated `rtmp://`, `rtmps://`, `srt://`, or local `hls://` live
/// destination, with a redacted view.
///
/// The secret tail is never rendered. `Debug` and `Display` both produce the
/// redacted form, so embedding this value in a derived `Debug` type is safe.
/// An HLS destination has no secret: its absolute directory is
/// operator-chosen local storage and is rendered verbatim.
#[derive(Clone, Eq, PartialEq)]
pub struct StreamDestination {
    url: String,
    redacted: String,
    key_offset: usize,
    kind: DestinationKind,
}

impl StreamDestination {
    /// Validates scheme, authority, and the presence of a redactable key.
    ///
    /// For RTMP the secret is everything after the final `/`, including any
    /// query string, which is where RTMP services carry stream keys and tokens.
    /// For SRT the secret is the value of the single optional `streamid`
    /// parameter; no path component is allowed there at all. An HLS destination
    /// carries no secret: the absolute directory after `hls://` is validated
    /// and then rendered verbatim.
    ///
    /// # Errors
    ///
    /// Returns a typed [`DestinationError`]. No variant carries URL text.
    pub fn parse(url: &str) -> Result<Self, DestinationError> {
        if url.is_empty() {
            return Err(DestinationError::Empty);
        }
        if url.len() > MAX_DESTINATION_BYTES {
            return Err(DestinationError::TooLong);
        }
        if !url.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(DestinationError::InvalidCharacter);
        }
        let (kind, rest) = if let Some(rest) = url.strip_prefix("rtmp://") {
            (DestinationKind::Rtmp, rest)
        } else if let Some(rest) = url.strip_prefix("rtmps://") {
            (DestinationKind::Rtmps, rest)
        } else if let Some(rest) = url.strip_prefix("srt://") {
            (DestinationKind::Srt, rest)
        } else if let Some(rest) = url.strip_prefix(HLS_SCHEME) {
            (DestinationKind::Hls, rest)
        } else {
            return Err(DestinationError::UnsupportedScheme);
        };
        let key = match kind {
            DestinationKind::Rtmp | DestinationKind::Rtmps => Self::rtmp_key(rest)?,
            DestinationKind::Srt => Self::srt_stream_id(rest)?.unwrap_or_default(),
            DestinationKind::Hls => {
                Self::hls_directory(rest)?;
                ""
            }
        };
        let key_offset = url.len() - key.len();
        let redacted = if key.is_empty() {
            // Nothing to mask: an SRT destination without `?streamid=` has no
            // secret, so its redacted form is the URL itself.
            url.to_owned()
        } else {
            format!("{}{REDACTED_KEY}", &url[..key_offset])
        };
        Ok(Self {
            redacted,
            url: url.to_owned(),
            key_offset,
            kind,
        })
    }

    /// The RTMP secret: everything after the final `/`.
    fn rtmp_key(rest: &str) -> Result<&str, DestinationError> {
        let (authority, path) = rest
            .split_once('/')
            .ok_or(DestinationError::MissingStreamKey)?;
        if authority.is_empty() {
            return Err(DestinationError::MissingHost);
        }
        if authority.contains('@') {
            return Err(DestinationError::EmbeddedCredentials);
        }
        let key_start = path
            .rfind('/')
            .map(|index| index + 1)
            .ok_or(DestinationError::MissingStreamKey)?;
        let key = &path[key_start..];
        if key.len() < MIN_STREAM_KEY_BYTES {
            return Err(DestinationError::StreamKeyTooShort);
        }
        Ok(key)
    }

    /// The SRT secret: the value of the single optional `streamid` parameter.
    ///
    /// The body must be exactly `host[:port]` plus at most one query string of
    /// exactly `streamid=<key>`; anything else — a path, a second `?`, another
    /// parameter name, extra parameters — is refused. A `/` is refused
    /// everywhere in the body, including inside a stream id, because the SRT
    /// URL has no path component to redact out of captured output.
    fn srt_stream_id(rest: &str) -> Result<Option<&str>, DestinationError> {
        let (authority, query) = match rest.split_once('?') {
            Some((authority, query)) => {
                if query.contains('?') || query.contains('&') {
                    return Err(DestinationError::MalformedSrtUrl);
                }
                (authority, Some(query))
            }
            None => (rest, None),
        };
        if authority.is_empty() {
            return Err(DestinationError::MissingHost);
        }
        if authority.contains('@') {
            return Err(DestinationError::EmbeddedCredentials);
        }
        if authority.contains('/') || query.is_some_and(|query| query.contains('/')) {
            return Err(DestinationError::MalformedSrtUrl);
        }
        let Some(query) = query else {
            return Ok(None);
        };
        let Some((name, value)) = query.split_once('=') else {
            return Err(DestinationError::MalformedSrtUrl);
        };
        if name != STREAM_ID_PARAM {
            return Err(DestinationError::MalformedSrtUrl);
        }
        if value.len() < MIN_STREAM_KEY_BYTES {
            return Err(DestinationError::StreamKeyTooShort);
        }
        Ok(Some(value))
    }

    /// The HLS destination directory: the secret-free absolute path after the
    /// scheme.
    ///
    /// # Errors
    ///
    /// [`DestinationError::PathNotAbsolute`] when the path does not start with
    /// `/`, and [`DestinationError::MalformedHlsPath`] when it is not a plain
    /// sequence of named segments.
    fn hls_directory(rest: &str) -> Result<&str, DestinationError> {
        if !rest.starts_with('/') {
            return Err(DestinationError::PathNotAbsolute);
        }
        if !valid_local_directory(rest) {
            return Err(DestinationError::MalformedHlsPath);
        }
        Ok(rest)
    }

    /// The validated transport this destination publishes over.
    #[must_use]
    pub const fn kind(&self) -> DestinationKind {
        self.kind
    }

    /// The absolute local directory this destination publishes into, or `None`
    /// for every network destination.
    ///
    /// An HLS directory is operator-chosen local storage rather than a secret,
    /// so both this and [`StreamDestination::redacted`] render it verbatim.
    #[must_use]
    pub fn local_directory(&self) -> Option<&str> {
        match self.kind {
            DestinationKind::Hls => Some(&self.url[HLS_SCHEME.len()..]),
            _ => None,
        }
    }

    /// The destination with its stream key replaced by `****`.
    #[must_use]
    pub fn redacted(&self) -> &str {
        &self.redacted
    }

    fn key(&self) -> &str {
        &self.url[self.key_offset..]
    }
}

impl fmt::Debug for StreamDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StreamDestination")
            .field(&self.redacted)
            .finish()
    }
}

impl fmt::Display for StreamDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted)
    }
}

/// Whether `path` is an absolute local directory acceptable as an HLS output:
/// printable ASCII, at least one segment, and only plain named segments — no
/// empty segment (`//`), no trailing `/`, and no `.` or `..` segment.
///
/// This is the one rule set for HLS paths: a destination that parses here is
/// exactly a directory [`Streamer::start`] is willing to publish into.
fn valid_local_directory(path: &str) -> bool {
    if !path.starts_with('/') || path.len() < 2 {
        return false;
    }
    if !path.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        || path.ends_with('/')
        || path.contains("//")
    {
        return false;
    }
    !path[1..]
        .split('/')
        .any(|segment| segment == "." || segment == "..")
}

/// What the sink does when an accepted pair does not fit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverflowPolicy {
    /// Return the new pair to the caller, counted as a rejection.
    Reject,
    /// Discard the oldest still-queued pair and admit the new one, counted as
    /// a drop. Exactly one pair is discarded per admission.
    ///
    /// Pairs already handed to the input writers are committed to the child and
    /// cannot be recalled, so `max_outstanding_pairs` must leave room for at
    /// least one pair to remain in the queue; a shallower queue is refused at
    /// startup rather than silently behaving as [`OverflowPolicy::Reject`].
    DropOldest,
}

/// Bounded resource and deadline policy for one streaming session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLimits {
    pub max_outstanding_pairs: usize,
    pub max_retained_bytes: usize,
    /// Upper bound on how long [`Streamer::enqueue`] may wait for space.
    ///
    /// Defaults to zero, which makes enqueue fully nonblocking, and may not
    /// exceed one frame period: a live render thread must never wait on a
    /// network sink for longer than the frame it is producing, because every
    /// millisecond spent here is taken from the next frame's budget rather than
    /// from the sink's.
    pub enqueue_timeout: Duration,
    /// Budget from the first dispatched pair to the first observed muxer
    /// progress, which is the child's proof that the destination opened. It is
    /// armed by the first dispatch, not by startup, so an idle sink is never
    /// failed for a destination it has not yet had reason to open; an
    /// unresponsive destination on a sink that never sent a pair is bounded by
    /// `stop_timeout` instead.
    pub connect_timeout: Duration,
    /// How far the child's muxed media clock may fall behind wall clock, and
    /// how long the child may report nothing at all, before the sink is
    /// declared dead.
    ///
    /// This is deliberately measured against the media clock rather than
    /// against byte movement: a child that keeps encoding into its own buffers,
    /// or that muxes slower than real time, is producing bytes while the stream
    /// its viewers see is already broken.
    pub no_progress_timeout: Duration,
    pub stop_timeout: Duration,
    pub kill_timeout: Duration,
    pub max_stderr_bytes: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_outstanding_pairs: 4,
            max_retained_bytes: 256 * 1024 * 1024,
            enqueue_timeout: Duration::ZERO,
            connect_timeout: Duration::from_secs(10),
            no_progress_timeout: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(15),
            kill_timeout: Duration::from_secs(2),
            max_stderr_bytes: MAX_STDERR_BYTES,
        }
    }
}

/// Encoder settings applied to the encoded output.
///
/// `keyframe_interval_seconds` also becomes `-hls_time` for a local HLS
/// destination, so playlist segments end on the keyframe boundaries this
/// interval produces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncoderSettings {
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub keyframe_interval_seconds: u32,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            video_bitrate_kbps: 4_500,
            audio_bitrate_kbps: 128,
            keyframe_interval_seconds: 2,
        }
    }
}

/// One rung of a multi-bitrate HLS ladder: the frame size one rendition is
/// scaled to and the video bitrate it is encoded at.
///
/// Every rung of a ladder is encoded from the same decoded frame by the same
/// `FFmpeg` child, so a rung is a scale target plus a bitrate cap, not a
/// separate session. Its `Display` is the `{width}x{height}@{kbps}` form the
/// master playlist's resolutions read as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HlsLadderRung {
    pub video_bitrate_kbps: u32,
    pub width: u32,
    pub height: u32,
}

impl fmt::Display for HlsLadderRung {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}x{}@{}",
            self.width, self.height, self.video_bitrate_kbps
        )
    }
}

/// A validated multi-bitrate HLS ladder: 2..=8 renditions of one program,
/// encoded together by one `FFmpeg` child and published as one master playlist
/// plus one variant playlist and segment set per rung.
///
/// The rungs read from the thinnest rendition to the richest: bitrates strictly
/// increase, and neither width nor height ever decreases across the list, so
/// the last rung is [`HlsLadder::top_bitrate_kbps`].
///
/// Only a local HLS destination carries a ladder. Every network transport in
/// this crate publishes exactly one rendition, and [`Streamer::start`] refuses
/// a ladder configured for anything else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsLadder {
    rungs: Vec<HlsLadderRung>,
}

impl HlsLadder {
    /// Validates the rung count, each rung's own bitrate and dimensions, and
    /// the ordering across rungs, in that order.
    ///
    /// # Errors
    ///
    /// Returns the typed [`HlsLadderError`] of the first rule the list breaks.
    pub fn new(rungs: Vec<HlsLadderRung>) -> Result<Self, HlsLadderError> {
        if rungs.len() < MIN_LADDER_RUNGS {
            return Err(HlsLadderError::TooFewRungs);
        }
        if rungs.len() > MAX_LADDER_RUNGS {
            return Err(HlsLadderError::TooManyRungs);
        }
        let mut previous: Option<HlsLadderRung> = None;
        for rung in &rungs {
            if rung.video_bitrate_kbps < MIN_LADDER_BITRATE_KBPS {
                return Err(HlsLadderError::BitrateTooLow);
            }
            if rung.video_bitrate_kbps > MAX_LADDER_BITRATE_KBPS {
                return Err(HlsLadderError::BitrateTooHigh);
            }
            if rung.width < MIN_LADDER_DIMENSION || rung.height < MIN_LADDER_DIMENSION {
                return Err(HlsLadderError::DimensionTooSmall);
            }
            if rung.width > MAX_LADDER_WIDTH || rung.height > MAX_LADDER_HEIGHT {
                return Err(HlsLadderError::DimensionTooLarge);
            }
            if rung.width % 2 != 0 || rung.height % 2 != 0 {
                return Err(HlsLadderError::OddDimension);
            }
            if let Some(previous) = previous {
                if rung.video_bitrate_kbps <= previous.video_bitrate_kbps {
                    return Err(HlsLadderError::BitratesNotIncreasing);
                }
                if rung.width < previous.width || rung.height < previous.height {
                    return Err(HlsLadderError::DimensionsDecrease);
                }
            }
            previous = Some(*rung);
        }
        Ok(Self { rungs })
    }

    /// The rungs in ascending bitrate order, exactly as validated.
    #[must_use]
    pub fn rungs(&self) -> &[HlsLadderRung] {
        &self.rungs
    }

    /// The highest rung's video bitrate in kbps: the top of the ladder.
    #[must_use]
    pub fn top_bitrate_kbps(&self) -> u32 {
        self.rungs.last().map_or(0, |rung| rung.video_bitrate_kbps)
    }
}

impl fmt::Display for HlsLadder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, rung) in self.rungs.iter().enumerate() {
            if index > 0 {
                formatter.write_str(",")?;
            }
            write!(formatter, "{rung}")?;
        }
        Ok(())
    }
}

/// Why a rung list is not a usable HLS ladder. Every variant carries static
/// text and no values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlsLadderError {
    /// Fewer than two rungs: a ladder with one rendition is not a choice.
    TooFewRungs,
    /// More than eight rungs: every rung is another encoder for the one child.
    TooManyRungs,
    /// A rung bitrate below `1_000` kbps.
    BitrateTooLow,
    /// A rung bitrate above `100_000` kbps.
    BitrateTooHigh,
    /// A rung narrower or shorter than 16 pixels.
    DimensionTooSmall,
    /// A rung wider than `7_680` or taller than `4_320` pixels.
    DimensionTooLarge,
    /// A rung whose width or height is not even; 4:2:0 needs even dimensions.
    OddDimension,
    /// Bitrates must strictly increase from the first rung to the last.
    BitratesNotIncreasing,
    /// Neither width nor height may decrease from the first rung to the last.
    DimensionsDecrease,
}

impl fmt::Display for HlsLadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooFewRungs => "an HLS ladder needs at least two rungs",
            Self::TooManyRungs => "an HLS ladder carries at most eight rungs",
            Self::BitrateTooLow => "a ladder rung bitrate is below 1000 kbps",
            Self::BitrateTooHigh => "a ladder rung bitrate is above 100000 kbps",
            Self::DimensionTooSmall => "a ladder rung is smaller than 16x16",
            Self::DimensionTooLarge => "a ladder rung is larger than 7680x4320",
            Self::OddDimension => "a ladder rung has an odd dimension",
            Self::BitratesNotIncreasing => {
                "ladder bitrates must strictly increase from the first rung to the last"
            }
            Self::DimensionsDecrease => {
                "ladder dimensions must not decrease from the first rung to the last"
            }
        })
    }
}

impl std::error::Error for HlsLadderError {}

/// Streaming startup configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamConfig {
    pub ffmpeg: Executable,
    pub format: RecordFormat,
    pub destination: StreamDestination,
    pub limits: StreamLimits,
    pub overflow: OverflowPolicy,
    pub encoder: EncoderSettings,
    /// A multi-bitrate ladder, `Some` only for an HLS destination: the child
    /// encodes every rung of the one program and publishes a master playlist
    /// plus one variant playlist per rung.
    pub ladder: Option<HlsLadder>,
}

impl StreamConfig {
    #[must_use]
    pub fn new(format: RecordFormat, destination: StreamDestination) -> Self {
        Self {
            ffmpeg: Executable::SearchPath,
            format,
            destination,
            limits: StreamLimits::default(),
            overflow: OverflowPolicy::DropOldest,
            encoder: EncoderSettings::default(),
            ladder: None,
        }
    }
}

/// Limits or encoder validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitsError {
    ZeroOutstandingPairs,
    /// [`OverflowPolicy::DropOldest`] can only discard pairs still sitting in
    /// the accounted queue. A queue this shallow would put every admitted pair
    /// straight into a writer, where nothing can be recalled, so the policy
    /// would silently behave as [`OverflowPolicy::Reject`].
    OutstandingPairsTooFewToDropOldest {
        required: usize,
    },
    ZeroRetainedBytes,
    ZeroTimeout,
    EnqueueTimeoutTooLong,
    ZeroStderrBytes,
    StderrTooLarge,
    TimeoutOverflow,
    ByteCountOverflow,
    RetainedBytesTooSmall {
        required: usize,
        maximum: usize,
    },
    VideoBitrate,
    AudioBitrate,
    KeyframeInterval,
}

impl fmt::Display for LimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid streaming limits: {self:?}")
    }
}

impl std::error::Error for LimitsError {}

/// Path-, URL-, and key-free startup failure category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartErrorKind {
    InvalidLimits(LimitsError),
    InvalidExecutable,
    ToolUnavailable(UnavailableReason),
    /// FLV carries mono or stereo audio only; the MPEG-TS-based outputs
    /// additionally carry multi-channel AAC. Wider layouts are refused rather
    /// than silently downmixed.
    UnsupportedChannelLayout,
    /// The absolute directory an HLS destination publishes into is unusable:
    /// it exists as something other than a directory, or it is missing and
    /// could not be created as a single leaf. A ladder's variant directories
    /// beneath it are held to the same rule. The variant carries no path text.
    HlsDirectoryUnusable,
    /// A multi-bitrate ladder was configured for a destination that cannot
    /// carry one: only a local HLS destination encodes more than one
    /// rendition. The variant carries no path or URL text.
    LadderNotSupported,
    Randomness,
    ThreadSpawn(io::ErrorKind),
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
        /// Redacted child stderr tail.
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
            "FFmpeg streamer startup failed: {:?} ({:?} cleanup)",
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

/// First terminal runtime failure. This value is sticky: it is recorded once
/// and reported by every later call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamFailure {
    Cancelled,
    ChildExited {
        status: Option<i32>,
    },
    /// The child never opened the destination within `connect_timeout` of the
    /// first dispatched pair. The child was alive and had media to send, so the
    /// destination itself is the suspect.
    DestinationTimeout,
    /// The child never completed its HTTP request for this loopback input
    /// within `connect_timeout`, so this input could never be written. Nothing
    /// was attempted against the destination; the child is the suspect.
    InputTimeout {
        input: MediaInput,
    },
    /// The child's muxed media clock fell more than `no_progress_timeout`
    /// behind wall clock, or the child stopped reporting progress entirely.
    NoProgress,
    Connect {
        input: MediaInput,
        kind: io::ErrorKind,
    },
    Write {
        input: MediaInput,
        kind: io::ErrorKind,
    },
    ProgressRead(io::ErrorKind),
    StderrRead(io::ErrorKind),
    DispatcherClosed(MediaInput),
    WorkerPanicked,
    StopTimedOut,
    KillTimedOut,
    CleanupUnconfirmed,
}

/// Streamer lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamState {
    Starting,
    Streaming,
    Stopping,
    Stopped,
    Failed,
}

/// Observed direct-child state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildState {
    Running,
    Exited { code: Option<i32>, success: bool },
}

/// Per-reason counts of pairs the sink refused to admit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RejectionCounts {
    pub queue_full: u64,
    pub retained_byte_limit: u64,
    pub not_streaming: u64,
    pub failed: u64,
    pub format_mismatch: u64,
    pub sequence: u64,
}

impl RejectionCounts {
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.queue_full
            .saturating_add(self.retained_byte_limit)
            .saturating_add(self.not_streaming)
            .saturating_add(self.failed)
            .saturating_add(self.format_mismatch)
            .saturating_add(self.sequence)
    }
}

/// Stable point-in-time telemetry.
///
/// Accepted pairs satisfy
/// `accepted = delivered + write_failed + dropped_oldest + discarded + outstanding`.
/// `padded_pairs` is outside that identity: padding is synthesized by the sink,
/// never accepted from the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamTelemetry {
    pub state: StreamState,
    pub child: ChildState,
    pub failure: Option<StreamFailure>,
    /// Redacted destination, safe to log.
    pub destination: String,
    pub accepted_pairs: u64,
    pub delivered_pairs: u64,
    pub write_failed_pairs: u64,
    /// Admitted pairs discarded by [`OverflowPolicy::DropOldest`].
    pub dropped_oldest_pairs: u64,
    /// Admitted pairs discarded because shutdown or cancellation intervened.
    pub discarded_pairs: u64,
    /// Sequence numbers the producer skipped between accepted pairs.
    pub skipped_pairs: u64,
    /// Pairs the sink synthesized to cover a gap in the dispatched sequence:
    /// the previous video frame repeated plus the matching span of silence.
    /// These keep the child's media clock on wall clock; they are not caller
    /// media and are not counted as accepted or delivered.
    pub padded_pairs: u64,
    pub rejected: RejectionCounts,
    pub outstanding_pairs: usize,
    pub peak_outstanding_pairs: usize,
    pub retained_bytes: usize,
    pub peak_retained_bytes: usize,
    /// Bytes the child reports having handed to its output muxer
    /// (`total_size`).
    ///
    /// This is **not** a delivery receipt. The bytes counted here have been
    /// written into the child's output pipeline; how many of them have reached
    /// the destination depends on the child's muxer interleaving, its socket
    /// send buffer, and the receiver's window, none of which are observable
    /// from here. Use [`StreamTelemetry::media_drift`] to judge liveness.
    pub muxed_bytes: u64,
    pub encoded_frames: u64,
    /// The child's muxed media timestamp (`out_time_us`): how much media it has
    /// written to the output so far.
    pub muxed_media_time: Duration,
    /// How far [`StreamTelemetry::muxed_media_time`] has fallen behind wall
    /// clock since the destination opened. A healthy real-time sink holds this
    /// near zero; sustained growth means viewers are rebuffering.
    pub media_drift: Duration,
    /// Whether the destination was ever observed open.
    pub connected: bool,
    pub stderr_tail: String,
    pub stderr_truncated: bool,
}

/// Why a bounded enqueue did not admit a pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnqueueRejection {
    QueueFull,
    RetainedByteLimit,
    FormatMismatch,
    /// Sequence numbers must strictly increase; gaps are allowed and counted.
    Sequence {
        minimum: SequenceNumber,
        actual: SequenceNumber,
    },
    SequenceExhausted,
    Stopping,
    Stopped,
    Failed(StreamFailure),
}

/// Bounded rejection that returns ownership of the pair to the caller.
pub struct EnqueueError {
    pub reason: EnqueueRejection,
    pub frame: Box<PairedFrame>,
}

impl EnqueueError {
    #[must_use]
    pub fn into_frame(self) -> PairedFrame {
        *self.frame
    }
}

impl fmt::Debug for EnqueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnqueueError")
            .field("reason", &self.reason)
            .field("sequence", &self.frame.sequence())
            .finish()
    }
}

impl fmt::Display for EnqueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "streaming enqueue rejected: {:?}", self.reason)
    }
}

impl std::error::Error for EnqueueError {}

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
    pub cleanup: CleanupStatus,
    pub telemetry: StreamTelemetry,
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

/// Ordered secret substitutions applied to every captured child byte.
#[derive(Clone, Debug)]
struct Redactor {
    rules: Vec<(Vec<u8>, Vec<u8>)>,
    longest: usize,
}

impl Redactor {
    fn new(destination: &StreamDestination, tokens: [&str; 2]) -> Self {
        let mut rules = vec![
            (
                destination.url.clone().into_bytes(),
                destination.redacted.clone().into_bytes(),
            ),
            (
                destination.key().as_bytes().to_vec(),
                REDACTED_KEY.as_bytes().to_vec(),
            ),
        ];
        for token in tokens {
            rules.push((token.as_bytes().to_vec(), REDACTED_TOKEN.to_vec()));
        }
        rules.sort_by_key(|rule| std::cmp::Reverse(rule.0.len()));
        let longest = rules.first().map_or(0, |rule| rule.0.len());
        Self { rules, longest }
    }

    fn scrub(&self, buffer: &mut Vec<u8>) {
        for (needle, replacement) in &self.rules {
            replace_all(buffer, needle, replacement);
        }
    }

    fn scrub_text(&self, text: &str) -> String {
        let mut bytes = text.as_bytes().to_vec();
        self.scrub(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Bytes that must be withheld from a forced flush so that a secret split
    /// across two child writes is still reassembled before it is retained.
    const fn carry(&self) -> usize {
        self.longest.saturating_sub(1)
    }
}

fn replace_all(buffer: &mut Vec<u8>, needle: &[u8], replacement: &[u8]) {
    if needle.is_empty() || buffer.len() < needle.len() {
        return;
    }
    if !buffer.windows(needle.len()).any(|window| window == needle) {
        return;
    }
    let mut output = Vec::with_capacity(buffer.len());
    let mut index = 0;
    while index < buffer.len() {
        if buffer[index..].starts_with(needle) {
            output.extend_from_slice(replacement);
            index += needle.len();
        } else {
            output.push(buffer[index]);
            index += 1;
        }
    }
    *buffer = output;
}

struct Queued {
    frame: PairedFrame,
    bytes: usize,
}

struct Shared {
    data: Mutex<SharedData>,
    signal: Condvar,
    kill_requested: AtomicBool,
    aborting: AtomicBool,
    redactor: Redactor,
    format: RecordFormat,
    limits: StreamLimits,
    overflow: OverflowPolicy,
    destination: String,
}

struct SharedData {
    state: StreamState,
    failure: Option<StreamFailure>,
    frozen: bool,
    /// Set once a drain or cancel has been requested.
    shutdown: Option<Shutdown>,
    minimum_sequence: SequenceNumber,
    queue: VecDeque<Queued>,
    accepted: u64,
    delivered: u64,
    write_failed: u64,
    dropped_oldest: u64,
    discarded: u64,
    skipped: u64,
    padded: u64,
    rejected: RejectionCounts,
    outstanding: usize,
    peak_outstanding: usize,
    retained_bytes: usize,
    peak_retained_bytes: usize,
    muxed_bytes: u64,
    encoded_frames: u64,
    /// Latest `out_time_us` the child reported, once it has reported a usable
    /// one at all.
    out_time: Option<Duration>,
    /// Wall instant and media timestamp of the first usable `out_time_us`.
    /// Drift is measured from here, so a slow startup is not charged to the
    /// running stream.
    media_baseline: Option<(Instant, Duration)>,
    /// Smallest drift ever observed. A child that runs a constant pipeline
    /// latency behind wall clock is healthy; only growth beyond its own best
    /// alignment is a failure.
    drift_floor: Option<Duration>,
    connected: bool,
    first_dispatch: Option<Instant>,
    last_progress: Option<Instant>,
    stderr: VecDeque<u8>,
    stderr_truncated: bool,
    child: Option<ChildResult>,
}

impl SharedData {
    fn cancelling(&self) -> bool {
        self.shutdown
            .is_some_and(|shutdown| shutdown.mode == ShutdownMode::Cancel)
    }

    /// How far the child's muxed media clock has fallen behind wall clock as of
    /// `now`, using the latest sample rather than the latest report, so a child
    /// that has stopped reporting shows its drift growing in real time.
    fn media_drift(&self, now: Instant) -> Option<Duration> {
        let (wall_origin, media_origin) = self.media_baseline?;
        let advanced = self.out_time?.saturating_sub(media_origin);
        Some(
            now.saturating_duration_since(wall_origin)
                .saturating_sub(advanced),
        )
    }
}

impl Shared {
    fn new(
        format: &RecordFormat,
        limits: StreamLimits,
        overflow: OverflowPolicy,
        destination: &StreamDestination,
        redactor: Redactor,
    ) -> Self {
        Self {
            data: Mutex::new(SharedData {
                state: StreamState::Starting,
                failure: None,
                frozen: false,
                shutdown: None,
                minimum_sequence: format.first_sequence(),
                queue: VecDeque::new(),
                accepted: 0,
                delivered: 0,
                write_failed: 0,
                dropped_oldest: 0,
                discarded: 0,
                skipped: 0,
                padded: 0,
                rejected: RejectionCounts::default(),
                outstanding: 0,
                peak_outstanding: 0,
                retained_bytes: 0,
                peak_retained_bytes: 0,
                muxed_bytes: 0,
                encoded_frames: 0,
                out_time: None,
                media_baseline: None,
                drift_floor: None,
                connected: false,
                first_dispatch: None,
                last_progress: None,
                stderr: VecDeque::new(),
                stderr_truncated: false,
                child: None,
            }),
            signal: Condvar::new(),
            kill_requested: AtomicBool::new(false),
            aborting: AtomicBool::new(false),
            redactor,
            format: format.clone(),
            limits,
            overflow,
            destination: destination.redacted.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SharedData> {
        self.data.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fail(&self, failure: StreamFailure) {
        let mut data = self.lock();
        self.fail_locked(&mut data, failure);
    }

    fn fail_locked(&self, data: &mut SharedData, failure: StreamFailure) {
        if !data.frozen && data.failure.is_none() {
            data.failure = Some(failure);
            data.state = StreamState::Failed;
            self.aborting.store(true, Ordering::Release);
            self.signal.notify_all();
        }
    }

    /// Classifies a failed input write. A broken loopback input is nearly
    /// always the consequence of the child exiting, so wait briefly for the
    /// reaper before deciding which cause becomes sticky.
    fn fail_write(&self, input: MediaInput, kind: io::ErrorKind) {
        let deadline = Instant::now() + WRITE_ATTRIBUTION_WINDOW;
        let mut data = self.lock();
        while data.child.is_none() && data.failure.is_none() && Instant::now() < deadline {
            let (guard, _) = self
                .signal
                .wait_timeout(data, POLL_INTERVAL)
                .unwrap_or_else(PoisonError::into_inner);
            data = guard;
        }
        let failure = data
            .child
            .map_or(StreamFailure::Write { input, kind }, |child| {
                StreamFailure::ChildExited { status: child.code }
            });
        self.fail_locked(&mut data, failure);
    }

    fn is_aborting(&self) -> bool {
        self.aborting.load(Ordering::Acquire) || self.kill_requested.load(Ordering::Acquire)
    }

    fn append_stderr(&self, bytes: &[u8]) {
        let mut data = self.lock();
        if data.frozen {
            return;
        }
        for &byte in bytes {
            if data.stderr.len() == self.limits.max_stderr_bytes {
                data.stderr.pop_front();
                data.stderr_truncated = true;
            }
            data.stderr.push_back(byte);
        }
    }

    fn observe_progress(&self, line: &[u8]) {
        let Ok(text) = std::str::from_utf8(line) else {
            return;
        };
        let Some((key, value)) = text.trim().split_once('=') else {
            return;
        };
        let value = value.trim();
        let mut data = self.lock();
        if data.frozen {
            return;
        }
        match key.trim() {
            "total_size" => {
                if let Ok(bytes) = value.parse::<u64>() {
                    data.muxed_bytes = data.muxed_bytes.max(bytes);
                }
            }
            "frame" => {
                if let Ok(frames) = value.parse::<u64>() {
                    data.encoded_frames = data.encoded_frames.max(frames);
                }
            }
            // Reported in microseconds, and `N/A` or negative until the child
            // has actually muxed something.
            "out_time_us" => {
                if let Ok(micros) = value.parse::<i64>()
                    && let Ok(micros) = u64::try_from(micros)
                {
                    let media = Duration::from_micros(micros);
                    data.out_time = Some(data.out_time.map_or(media, |seen| seen.max(media)));
                }
            }
            // A progress block is only emitted once the child has opened every
            // output, so the first one proves the destination accepted us.
            "progress" => {
                let now = Instant::now();
                data.connected = true;
                data.last_progress = Some(now);
                if let Some(media) = data.out_time {
                    data.media_baseline.get_or_insert((now, media));
                    if let Some(drift) = data.media_drift(now) {
                        data.drift_floor =
                            Some(data.drift_floor.map_or(drift, |floor| floor.min(drift)));
                    }
                }
            }
            _ => {}
        }
    }

    /// Returns true when a deadline just became the sticky failure and the
    /// child must be torn down.
    ///
    /// The no-progress deadline runs whenever the sink is connected and
    /// streaming. It is deliberately *not* gated on there being outstanding
    /// pairs: a producer that pauses is routine, and a dead destination that
    /// happens to coincide with a paused producer is exactly the case a live
    /// operator must be told about rather than shielded from.
    fn check_deadlines(&self) -> bool {
        let mut data = self.lock();
        if data.frozen || data.failure.is_some() || data.cancelling() {
            return false;
        }
        // A drain is still streaming while pairs remain to flush. Once nothing
        // is outstanding the drain is only waiting for the child to finalize
        // and exit, which `stop_timeout` bounds.
        if data.shutdown.is_some() && data.outstanding == 0 {
            return false;
        }
        let now = Instant::now();
        let failure = if data.connected {
            let silent = data.last_progress.is_some_and(|last| {
                now.saturating_duration_since(last) > self.limits.no_progress_timeout
            });
            let diverged = match (data.media_drift(now), data.drift_floor) {
                (Some(drift), Some(floor)) => {
                    drift.saturating_sub(floor) > self.limits.no_progress_timeout
                }
                _ => false,
            };
            (silent || diverged).then_some(StreamFailure::NoProgress)
        } else {
            data.first_dispatch
                .filter(|armed| now.saturating_duration_since(*armed) > self.limits.connect_timeout)
                .map(|_| StreamFailure::DestinationTimeout)
        };
        let Some(failure) = failure else {
            return false;
        };
        self.fail_locked(&mut data, failure);
        true
    }

    fn record_child(&self, status: ExitStatus) {
        let mut data = self.lock();
        if data.child.is_some() {
            return;
        }
        let result = ChildResult {
            success: status.success(),
            code: status.code(),
        };
        data.child = Some(result);
        if self.kill_requested.load(Ordering::Acquire) {
            self.fail_locked(&mut data, StreamFailure::Cancelled);
        }
        if data.shutdown.is_none() && (!result.success || data.state == StreamState::Streaming) {
            self.fail_locked(
                &mut data,
                StreamFailure::ChildExited {
                    status: result.code,
                },
            );
        }
        self.signal.notify_all();
    }

    fn discard_queue(&self, data: &mut SharedData) {
        while let Some(queued) = data.queue.pop_front() {
            data.outstanding = data.outstanding.saturating_sub(1);
            data.retained_bytes = data.retained_bytes.saturating_sub(queued.bytes);
            data.discarded = data.discarded.saturating_add(1);
        }
        self.signal.notify_all();
    }

    fn snapshot(&self) -> StreamTelemetry {
        self.telemetry_locked(&self.lock())
    }

    fn telemetry_locked(&self, data: &SharedData) -> StreamTelemetry {
        let stderr = data.stderr.iter().copied().collect::<Vec<_>>();
        // Once the child is gone the media clock cannot advance again, so drift
        // is frozen at its last report rather than growing with wall clock.
        let clock = match data.child {
            Some(_) => data.last_progress.unwrap_or_else(Instant::now),
            None => Instant::now(),
        };
        StreamTelemetry {
            state: data.state,
            child: match data.child {
                None => ChildState::Running,
                Some(child) => ChildState::Exited {
                    code: child.code,
                    success: child.success,
                },
            },
            failure: data.failure.clone(),
            destination: self.destination.clone(),
            accepted_pairs: data.accepted,
            delivered_pairs: data.delivered,
            write_failed_pairs: data.write_failed,
            dropped_oldest_pairs: data.dropped_oldest,
            discarded_pairs: data.discarded,
            skipped_pairs: data.skipped,
            padded_pairs: data.padded,
            rejected: data.rejected,
            outstanding_pairs: data.outstanding,
            peak_outstanding_pairs: data.peak_outstanding,
            retained_bytes: data.retained_bytes,
            peak_retained_bytes: data.peak_retained_bytes,
            muxed_bytes: data.muxed_bytes,
            encoded_frames: data.encoded_frames,
            muxed_media_time: data.out_time.unwrap_or_default(),
            media_drift: data.media_drift(clock).unwrap_or_default(),
            connected: data.connected,
            // Retained stderr was redacted before storage; this is a second,
            // cheap pass in case a rule was added after a byte was retained.
            stderr_tail: self.redactor.scrub_text(&clean_tail(&stderr)),
            stderr_truncated: data.stderr_truncated,
        }
    }
}

fn clean_tail(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    text.retain(|character| character == '\n' || character == '\t' || !character.is_control());
    text.trim().to_owned()
}

/// Shared completion state for the two halves of one dispatched pair.
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
            if !data.frozen {
                data.outstanding = data.outstanding.saturating_sub(1);
                data.retained_bytes = data.retained_bytes.saturating_sub(self.bytes);
                if self.successful.load(Ordering::Acquire) {
                    data.delivered = data.delivered.saturating_add(1);
                } else {
                    data.write_failed = data.write_failed.saturating_add(1);
                }
            }
            drop(data);
            self.shared.signal.notify_all();
        }
    }
}

/// What one write consumes: a real pair's payload, or synthesized padding.
enum Payload {
    /// A pair's own video or audio bytes. Also used for padding video, where
    /// the previously delivered pair is simply repeated.
    Pair(Arc<PairedFrame>),
    /// Padding audio: `length` bytes taken from a shared zero buffer, sized for
    /// the exact sequence being padded.
    Silence { zeros: Arc<Vec<u8>>, length: usize },
}

struct WritePart {
    payload: Payload,
    input: MediaInput,
    /// `None` for padding: synthesized pairs were never admitted, so they own
    /// no share of `outstanding` or `retained_bytes`.
    completion: Option<Arc<Completion>>,
}

impl WritePart {
    fn bytes(&self) -> &[u8] {
        match (&self.payload, self.input) {
            (Payload::Pair(frame), MediaInput::Video) => frame.rgba(),
            (Payload::Pair(frame), MediaInput::Audio) => frame.audio_f32le(),
            (Payload::Silence { zeros, length }, _) => &zeros[..*length],
        }
    }

    fn complete(mut self, successful: bool) {
        if let Some(completion) = self.completion.take() {
            completion.finish_part(successful);
        }
    }
}

impl Drop for WritePart {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.finish_part(false);
        }
    }
}

struct Endpoint {
    listener: TcpListener,
    path: String,
    input: MediaInput,
}

enum CleanupWorker {
    Unit(JoinHandle<()>),
}

/// Long-lived bounded RTMP/RTMPS/SRT/HLS sink over one direct `FFmpeg` child.
pub struct Streamer {
    format: RecordFormat,
    limits: StreamLimits,
    shared: Arc<Shared>,
    child: Arc<Mutex<Child>>,
    workers: Vec<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
    report: Option<StopReport>,
}

impl Streamer {
    /// Validates the configuration, binds two authenticated loopback HTTP
    /// inputs, starts `FFmpeg`, and confirms the child's first input request.
    ///
    /// The destination scheme is checked before anything is spawned. No error
    /// returned by this function carries the stream key.
    ///
    /// # Errors
    ///
    /// Returns a typed, redacted [`StartError`]. [`StartError::cleanup`] says
    /// explicitly whether child reaping may still be running.
    #[allow(clippy::too_many_lines)]
    pub fn start(config: StreamConfig) -> Result<Self, StartError> {
        validate_limits(
            &config.format,
            config.limits,
            config.overflow,
            config.encoder,
        )
        .map_err(|error| StartError::complete(StartErrorKind::InvalidLimits(error)))?;
        // Only a local HLS destination can publish more than one rendition:
        // every network transport muxes exactly one stream into one output.
        // Refusing here keeps a ladder out of every non-HLS argument tail
        // before anything is bound or spawned.
        if config.ladder.is_some() && config.destination.kind() != DestinationKind::Hls {
            return Err(StartError::complete(StartErrorKind::LadderNotSupported));
        }
        // A local HLS destination publishes into a directory the operator
        // chose. Create its leaf before the child is spawned, so an unusable
        // path is a startup error rather than a child that dies on its first
        // mux. A ladder's variant directories are created with it: the child
        // writes `<dir>/%v/...` but never creates the variant directories.
        if let Some(directory) = config.destination.local_directory() {
            ensure_hls_directory(directory).map_err(StartError::complete)?;
            if let Some(ladder) = &config.ladder {
                for index in 0..ladder.rungs().len() {
                    ensure_hls_directory(&format!("{directory}/{index}"))
                        .map_err(StartError::complete)?;
                }
            }
        }
        let layout =
            output_channel_layout(config.destination.kind(), config.format.channel_layout())
                .ok_or_else(|| StartError::complete(StartErrorKind::UnsupportedChannelLayout))?;
        let executable = streamer_executable(config.ffmpeg.clone())?;
        let video_token = random_token()?;
        let audio_token = random_token()?;
        let video_listener = bind_listener(MediaInput::Video)?;
        let audio_listener = bind_listener(MediaInput::Audio)?;
        let video_address = listener_address(&video_listener, MediaInput::Video)?;
        let audio_address = listener_address(&audio_listener, MediaInput::Audio)?;
        let args = command_args(
            &config,
            layout,
            &Inputs {
                video_address,
                audio_address,
                video_token: &video_token,
                audio_token: &audio_token,
            },
        );
        let redactor = Redactor::new(&config.destination, [&video_token, &audio_token]);
        let shared = Arc::new(Shared::new(
            &config.format,
            config.limits,
            config.overflow,
            &config.destination,
            redactor,
        ));
        let (monitor_sender, monitor) = spawn_monitor_waiter()?;
        let child = match command(&executable, &args).spawn() {
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
            let cleanup = terminate_without_monitor(&child, config.limits.kill_timeout);
            return Err(StartError {
                kind: StartErrorKind::ThreadSpawn(io::ErrorKind::Other),
                cleanup,
            });
        }
        drop(monitor_sender);
        let (stdout, stderr) = {
            let mut guard = child.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.stdout.take(), guard.stderr.take())
        };
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            return Err(cleanup_startup(
                StartErrorKind::MissingPipe,
                &child,
                &shared,
                Vec::new(),
                monitor,
                config.limits.kill_timeout,
            ));
        };
        let mut workers = Vec::new();
        for worker in [
            spawn_progress(stdout, Arc::clone(&shared)),
            spawn_stderr(stderr, Arc::clone(&shared)),
        ] {
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    return Err(cleanup_startup(
                        StartErrorKind::ThreadSpawn(error.kind()),
                        &child,
                        &shared,
                        workers,
                        monitor,
                        config.limits.kill_timeout,
                    ));
                }
            }
        }

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
        let deadline = Instant::now() + config.limits.connect_timeout;
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
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        };
        {
            let mut data = shared.lock();
            if data.failure.is_some() || data.child.is_some() {
                let kind = StartErrorKind::EarlyExit {
                    status: data.child.and_then(|child| child.code),
                    stderr: shared.telemetry_locked(&data).stderr_tail,
                };
                drop(data);
                return Err(cleanup_startup(
                    kind,
                    &child,
                    &shared,
                    workers,
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
            data.state = StreamState::Streaming;
        }

        // A one-deep buffer, not a rendezvous: see `WRITER_BACKLOG`. Everything
        // handed to a writer is still counted in `outstanding`, so the admission
        // bound stays honest either way.
        let (video_sender, video_receiver) = mpsc::sync_channel(WRITER_BACKLOG);
        let (audio_sender, audio_receiver) = mpsc::sync_channel(WRITER_BACKLOG);
        let (connected, pending, pending_receiver) = match first_input {
            MediaInput::Video => ((video, first_stream, video_receiver), audio, audio_receiver),
            MediaInput::Audio => ((audio, first_stream, audio_receiver), video, video_receiver),
        };
        for worker in [
            spawn_writer(connected.0, connected.1, connected.2, Arc::clone(&shared)),
            spawn_pending_writer(pending, pending_receiver, Arc::clone(&shared)),
        ] {
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    return Err(cleanup_startup(
                        StartErrorKind::ThreadSpawn(error.kind()),
                        &child,
                        &shared,
                        workers,
                        monitor,
                        config.limits.kill_timeout,
                    ));
                }
            }
        }
        match spawn_dispatcher(Arc::clone(&shared), video_sender, audio_sender) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                return Err(cleanup_startup(
                    StartErrorKind::ThreadSpawn(error.kind()),
                    &child,
                    &shared,
                    workers,
                    monitor,
                    config.limits.kill_timeout,
                ));
            }
        }
        Ok(Self {
            format: config.format,
            limits: config.limits,
            shared,
            child,
            workers,
            monitor: Some(monitor),
            report: None,
        })
    }

    /// The destination with its stream key replaced by `****`.
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.shared.destination
    }

    /// Admits one pair, waiting at most `enqueue_timeout` for space.
    ///
    /// This is called from the render thread, so it is nonblocking by default
    /// and can never wait longer than one frame period. Sequence numbers must
    /// strictly increase; gaps are accepted, counted as `skipped_pairs`, and
    /// padded out to the child so the media clock keeps tracking wall clock.
    /// Every refusal is counted by reason and returns the pair to the caller.
    ///
    /// # Errors
    ///
    /// Returns the original pair with a classified [`EnqueueRejection`].
    pub fn enqueue(&mut self, frame: PairedFrame) -> Result<(), EnqueueError> {
        let deadline = Instant::now().checked_add(self.limits.enqueue_timeout);
        let bytes = frame.retained_bytes();
        let mut evicted = false;
        let mut data = self.shared.lock();
        loop {
            if self.shared.kill_requested.load(Ordering::Acquire) {
                self.shared.fail_locked(&mut data, StreamFailure::Cancelled);
            }
            if let Some(failure) = data.failure.clone() {
                data.rejected.failed = data.rejected.failed.saturating_add(1);
                return Err(reject(EnqueueRejection::Failed(failure), frame));
            }
            match data.state {
                StreamState::Streaming => {}
                StreamState::Stopped => {
                    data.rejected.not_streaming = data.rejected.not_streaming.saturating_add(1);
                    return Err(reject(EnqueueRejection::Stopped, frame));
                }
                _ => {
                    data.rejected.not_streaming = data.rejected.not_streaming.saturating_add(1);
                    return Err(reject(EnqueueRejection::Stopping, frame));
                }
            }
            if !self.matches_format(&frame) {
                data.rejected.format_mismatch = data.rejected.format_mismatch.saturating_add(1);
                return Err(reject(EnqueueRejection::FormatMismatch, frame));
            }
            let sequence = frame.sequence();
            if sequence < data.minimum_sequence {
                data.rejected.sequence = data.rejected.sequence.saturating_add(1);
                return Err(reject(
                    EnqueueRejection::Sequence {
                        minimum: data.minimum_sequence,
                        actual: sequence,
                    },
                    frame,
                ));
            }
            let Some(next) = sequence.checked_next() else {
                data.rejected.sequence = data.rejected.sequence.saturating_add(1);
                return Err(reject(EnqueueRejection::SequenceExhausted, frame));
            };
            let fits = data.outstanding < self.limits.max_outstanding_pairs
                && data
                    .retained_bytes
                    .checked_add(bytes)
                    .is_some_and(|total| total <= self.limits.max_retained_bytes);
            if fits {
                data.skipped = data
                    .skipped
                    .saturating_add(sequence.get() - data.minimum_sequence.get());
                data.minimum_sequence = next;
                data.outstanding += 1;
                data.peak_outstanding = data.peak_outstanding.max(data.outstanding);
                data.retained_bytes += bytes;
                data.peak_retained_bytes = data.peak_retained_bytes.max(data.retained_bytes);
                data.accepted = data.accepted.saturating_add(1);
                data.queue.push_back(Queued { frame, bytes });
                self.shared.signal.notify_all();
                return Ok(());
            }
            // Exactly one eviction per admission: `DropOldest` trades the
            // oldest pair for the newest one. A pair that still does not fit
            // after that is over the per-pair budget the limits were validated
            // against, and must be refused rather than allowed to drain the
            // whole queue behind it.
            if self.shared.overflow == OverflowPolicy::DropOldest
                && !evicted
                && let Some(queued) = data.queue.pop_front()
            {
                evicted = true;
                data.outstanding = data.outstanding.saturating_sub(1);
                data.retained_bytes = data.retained_bytes.saturating_sub(queued.bytes);
                data.dropped_oldest = data.dropped_oldest.saturating_add(1);
                drop(queued);
                continue;
            }
            let now = Instant::now();
            match deadline {
                Some(deadline) if now < deadline => {
                    let (guard, _) = self
                        .shared
                        .signal
                        .wait_timeout(data, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner);
                    data = guard;
                }
                _ => {
                    let reason = if data.outstanding >= self.limits.max_outstanding_pairs {
                        data.rejected.queue_full = data.rejected.queue_full.saturating_add(1);
                        EnqueueRejection::QueueFull
                    } else {
                        data.rejected.retained_byte_limit =
                            data.rejected.retained_byte_limit.saturating_add(1);
                        EnqueueRejection::RetainedByteLimit
                    };
                    return Err(reject(reason, frame));
                }
            }
        }
    }

    #[must_use]
    pub fn telemetry(&self) -> StreamTelemetry {
        self.report
            .as_ref()
            .map_or_else(|| self.shared.snapshot(), |report| report.telemetry.clone())
    }

    /// Stops accepting, flushes queued pairs within `stop_timeout`, closes the
    /// inputs, then waits for and if necessary kills and reaps the child.
    /// Repeated calls return the same frozen report.
    #[must_use]
    pub fn stop(&mut self) -> StopReport {
        self.stop_inner(ShutdownMode::Drain)
    }

    /// Immediately stops accepting, abandons queued pairs, and requests child
    /// termination without waiting for any worker or exit status.
    pub fn request_cancel(&mut self) {
        if self.report.is_some() {
            return;
        }
        self.shared.kill_requested.store(true, Ordering::Release);
        self.shared.aborting.store(true, Ordering::Release);
        match self.shared.data.try_lock() {
            Ok(mut data) => self.mark_cancelled(&mut data),
            Err(TryLockError::Poisoned(error)) => self.mark_cancelled(&mut error.into_inner()),
            Err(TryLockError::WouldBlock) => {}
        }
        self.shared.signal.notify_all();
        try_kill_child(&self.child);
    }

    /// Kills the child immediately and returns the frozen report.
    #[must_use]
    pub fn cancel(&mut self) -> StopReport {
        self.request_cancel();
        self.stop_inner(ShutdownMode::Cancel)
    }

    fn mark_cancelled(&self, data: &mut SharedData) {
        let deadline = data.shutdown.map_or_else(
            || Instant::now() + self.limits.kill_timeout,
            |shutdown| shutdown.deadline,
        );
        data.shutdown = Some(Shutdown {
            mode: ShutdownMode::Cancel,
            deadline,
        });
        self.shared.fail_locked(data, StreamFailure::Cancelled);
        self.shared.discard_queue(data);
    }

    /// Compares the whole format, not the payload byte counts.
    ///
    /// Byte counts are not a format: a 48x64 frame has exactly as many RGBA
    /// bytes as a 64x48 one and would be muxed transposed, and mono at 96 kHz
    /// has exactly as many audio bytes as stereo at 48 kHz.
    fn matches_format(&self, frame: &PairedFrame) -> bool {
        frame.format() == &self.format
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
                || data.cancelling()
                || self.shared.kill_requested.load(Ordering::Acquire)
            {
                ShutdownMode::Cancel
            } else {
                ShutdownMode::Drain
            };
            if data.state != StreamState::Failed {
                data.state = StreamState::Stopping;
            }
            if data.shutdown.is_none() {
                data.shutdown = Some(Shutdown {
                    mode,
                    deadline: drain_deadline,
                });
            }
            if mode == ShutdownMode::Cancel {
                self.mark_cancelled(&mut data);
            }
            drop(data);
            self.shared.signal.notify_all();
            mode
        };

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
                        self.shared.fail(StreamFailure::StopTimedOut);
                    }
                    killed = true;
                    let kill_deadline = Instant::now() + self.limits.kill_timeout;
                    {
                        let mut data = self.shared.lock();
                        data.shutdown = Some(Shutdown {
                            mode: ShutdownMode::Cancel,
                            deadline: kill_deadline,
                        });
                        self.shared.discard_queue(&mut data);
                    }
                    self.shared.kill_requested.store(true, Ordering::Release);
                    self.shared.aborting.store(true, Ordering::Release);
                    self.shared.signal.notify_all();
                    request_kill(&self.child);
                    break kill_deadline;
                }
                thread::sleep(POLL_INTERVAL);
            }
        };

        wait_until(cleanup_deadline, || {
            poll_child(&self.child, &self.shared);
            self.workers.iter().all(JoinHandle::is_finished)
                && self.monitor.as_ref().is_none_or(JoinHandle::is_finished)
        });
        poll_child(&self.child, &self.shared);
        let child_result = self.shared.lock().child;
        if child_result.is_none() {
            self.shared.fail(StreamFailure::KillTimedOut);
        } else if child_result.is_some_and(|child| !child.success) && !killed {
            self.shared.fail(StreamFailure::ChildExited {
                status: child_result.and_then(|child| child.code),
            });
        }

        let mut unfinished = Vec::new();
        for worker in self
            .workers
            .drain(..)
            .chain(self.monitor.take())
            .collect::<Vec<_>>()
        {
            if worker.is_finished() {
                if worker.join().is_err() {
                    self.shared.fail(StreamFailure::WorkerPanicked);
                }
            } else {
                unfinished.push(CleanupWorker::Unit(worker));
            }
        }
        if !unfinished.is_empty() {
            self.shared.fail(StreamFailure::CleanupUnconfirmed);
        }
        let (telemetry, final_child) = {
            let mut data = self.shared.lock();
            // Anything still queued was never handed to the child; count it as
            // discarded rather than leaving it reported as outstanding.
            self.shared.discard_queue(&mut data);
            if data.failure.is_none()
                && data.child.is_some_and(|child| child.success)
                && unfinished.is_empty()
            {
                data.state = StreamState::Stopped;
            } else {
                data.state = StreamState::Failed;
            }
            data.frozen = true;
            (self.shared.telemetry_locked(&data), data.child)
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
            cleanup,
            telemetry,
        };
        self.report = Some(report.clone());
        report
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn reject(reason: EnqueueRejection, frame: PairedFrame) -> EnqueueError {
    EnqueueError {
        reason,
        frame: Box::new(frame),
    }
}

fn expected_audio_bytes(format: &RecordFormat, sequence: SequenceNumber) -> Option<usize> {
    let rate = format.frame_rate();
    let hertz = u128::from(format.sample_rate().hertz());
    let boundary = |frames: u64| -> Option<u128> {
        u128::from(frames)
            .checked_mul(hertz)?
            .checked_mul(u128::from(rate.denominator()))
            .map(|value| value / u128::from(rate.numerator()))
    };
    let start = boundary(sequence.get())?;
    let end = boundary(sequence.get().checked_add(1)?)?;
    usize::try_from(end - start)
        .ok()?
        .checked_mul(format.channel_layout().channels().len())?
        .checked_mul(size_of::<f32>())
}

/// One frame period, rounded up.
fn frame_period(format: &RecordFormat) -> Duration {
    let rate = format.frame_rate();
    let nanos = u128::from(rate.denominator())
        .saturating_mul(1_000_000_000)
        .div_ceil(u128::from(rate.numerator()));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// How many pairs the dispatcher may synthesize to close one gap.
///
/// Padding restores the media clock at real time and no faster: the child
/// consumes it at the cadence it was told the input runs at. A gap wider than
/// the no-progress budget therefore cannot be closed by padding at all, because
/// the sink is already failing by the time the padding would finish, so the
/// burst is bounded there and the watchdog owns the rest.
fn maximum_padding_pairs(format: &RecordFormat, no_progress: Duration) -> u64 {
    let rate = format.frame_rate();
    let pairs = no_progress
        .as_nanos()
        .saturating_mul(u128::from(rate.numerator()))
        / u128::from(rate.denominator()).saturating_mul(1_000_000_000);
    u64::try_from(pairs).unwrap_or(u64::MAX).max(1)
}

/// Pairs that are admitted but no longer recallable: one being written, up to
/// `WRITER_BACKLOG` buffered for the writer, and one held by the dispatcher
/// while it hands the previous one over.
const COMMITTED_PAIRS: usize = WRITER_BACKLOG + 2;

fn validate_limits(
    format: &RecordFormat,
    limits: StreamLimits,
    overflow: OverflowPolicy,
    encoder: EncoderSettings,
) -> Result<(), LimitsError> {
    if limits.max_outstanding_pairs == 0 {
        return Err(LimitsError::ZeroOutstandingPairs);
    }
    if overflow == OverflowPolicy::DropOldest && limits.max_outstanding_pairs <= COMMITTED_PAIRS {
        return Err(LimitsError::OutstandingPairsTooFewToDropOldest {
            required: COMMITTED_PAIRS + 1,
        });
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
    if limits.enqueue_timeout > frame_period(format) {
        return Err(LimitsError::EnqueueTimeoutTooLong);
    }
    if limits.max_stderr_bytes == 0 {
        return Err(LimitsError::ZeroStderrBytes);
    }
    if limits.max_stderr_bytes > MAX_STDERR_BYTES {
        return Err(LimitsError::StderrTooLarge);
    }
    let now = Instant::now();
    for timeout in [
        limits.enqueue_timeout,
        limits.connect_timeout,
        limits.no_progress_timeout,
        limits.stop_timeout,
        limits.kill_timeout,
    ] {
        if now.checked_add(timeout).is_none() {
            return Err(LimitsError::TimeoutOverflow);
        }
    }
    if !(1..=100_000).contains(&encoder.video_bitrate_kbps) {
        return Err(LimitsError::VideoBitrate);
    }
    if !(8..=1_024).contains(&encoder.audio_bitrate_kbps) {
        return Err(LimitsError::AudioBitrate);
    }
    if !(1..=10).contains(&encoder.keyframe_interval_seconds) {
        return Err(LimitsError::KeyframeInterval);
    }
    // Derived from `PairedFrame`'s own accounting rather than restated here, so
    // adding a field to a pair cannot silently invalidate this budget.
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

/// FLV carries one or two audio channels; wider layouts are refused rather
/// than silently downmixed.
fn flv_channel_layout(layout: &ChannelLayout) -> Option<&'static str> {
    match layout.channels() {
        [Channel::Mono] => Some("mono"),
        [Channel::Left, Channel::Right] => Some("stereo"),
        _ => None,
    }
}

/// MPEG-TS carries AAC with more channels than FLV's stereo ceiling: the
/// standard mono, stereo, and 5.1 channel configurations are all expressible
/// exactly. Anything else is refused rather than silently downmixed or guessed
/// at a layout name the child would reject.
fn mpegts_channel_layout(layout: &ChannelLayout) -> Option<&'static str> {
    match layout.channels() {
        [Channel::Mono] => Some("mono"),
        [Channel::Left, Channel::Right] => Some("stereo"),
        [
            Channel::Left,
            Channel::Right,
            Channel::Center,
            Channel::LowFrequency,
            Channel::LeftSurround,
            Channel::RightSurround,
        ] => Some("5.1"),
        _ => None,
    }
}

/// The audio channel-layout descriptor the output muxer can carry exactly.
fn output_channel_layout(kind: DestinationKind, layout: &ChannelLayout) -> Option<&'static str> {
    match kind {
        DestinationKind::Rtmp | DestinationKind::Rtmps => flv_channel_layout(layout),
        DestinationKind::Srt | DestinationKind::Hls => mpegts_channel_layout(layout),
    }
}

fn random_token() -> Result<String, StartError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| StartError::complete(StartErrorKind::Randomness))?;
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
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
                stderr: shared.telemetry_locked(&data).stderr_tail,
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
                .and_then(|()| stream.set_write_timeout(Some(WRITE_POLL)))
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
        Err(HttpRequestError::Io) if Instant::now() >= deadline => {
            Err(StartErrorKind::ConnectTimeout {
                input: endpoint.input,
            })
        }
        Err(HttpRequestError::Io) => Ok(None),
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

enum HttpRequestError {
    TooLarge,
    Timeout,
    Io,
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
            .map_err(|_| HttpRequestError::Io)?;
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) && Instant::now() >= deadline =>
            {
                return Err(HttpRequestError::Timeout);
            }
            Err(_) => return Err(HttpRequestError::Io),
        };
        if count == 0 {
            return Err(HttpRequestError::Io);
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
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(path), Some("HTTP/1.1" | "HTTP/1.0"), None) => Some(path),
        _ => None,
    }
}

fn spawn_writer(
    endpoint: Endpoint,
    stream: TcpStream,
    receiver: mpsc::Receiver<WritePart>,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("fm-ffmpeg-stream-{:?}", endpoint.input))
        .spawn(move || {
            write_parts(stream, None, &receiver, endpoint.input, &shared);
            drop(endpoint);
        })
}

/// The child opens its second input only after the first one has produced
/// data, so that endpoint is accepted lazily, on the first part to write.
fn spawn_pending_writer(
    endpoint: Endpoint,
    receiver: mpsc::Receiver<WritePart>,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("fm-ffmpeg-stream-pending-{:?}", endpoint.input))
        .spawn(move || {
            let Ok(first) = receiver.recv() else {
                // Nothing was ever streamed: accept and close so a child that
                // is waiting on this input observes end of input promptly.
                let deadline = shared
                    .lock()
                    .shutdown
                    .map_or_else(Instant::now, |shutdown| shutdown.deadline);
                if let Ok(stream) = accept_http_until(&endpoint, deadline, &shared) {
                    drop(stream);
                }
                return;
            };
            let deadline = Instant::now() + shared.limits.connect_timeout;
            match accept_http_until(&endpoint, deadline, &shared) {
                Ok(stream) => write_parts(stream, Some(first), &receiver, endpoint.input, &shared),
                Err(failure) => {
                    shared.fail(failure);
                    first.complete(false);
                }
            }
        })
}

fn accept_http_until(
    endpoint: &Endpoint,
    deadline: Instant,
    shared: &Shared,
) -> Result<TcpStream, StreamFailure> {
    while Instant::now() < deadline {
        if shared.is_aborting() {
            return Err(StreamFailure::Cancelled);
        }
        // A child that has already exited will never open this input, so do
        // not hold shutdown open for the rest of the accept budget.
        if let Some(child) = shared.lock().child {
            return Err(StreamFailure::ChildExited { status: child.code });
        }
        match try_accept_http(endpoint, deadline, shared.limits.no_progress_timeout) {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(StartErrorKind::Connect { kind, .. }) => {
                return Err(StreamFailure::Connect {
                    input: endpoint.input,
                    kind,
                });
            }
            Err(_) => {
                return Err(StreamFailure::InputTimeout {
                    input: endpoint.input,
                });
            }
        }
    }
    Err(StreamFailure::InputTimeout {
        input: endpoint.input,
    })
}

fn write_parts(
    mut stream: TcpStream,
    first: Option<WritePart>,
    receiver: &mpsc::Receiver<WritePart>,
    input: MediaInput,
    shared: &Shared,
) {
    for part in first.into_iter().chain(receiver.iter()) {
        let successful = write_part(&mut stream, &part, input, shared);
        part.complete(successful);
        if !successful {
            return;
        }
    }
}

/// Writes one payload under the configured no-progress budget.
///
/// The socket's own timeout is kept short so cancellation is observed promptly,
/// but a child that accepts nothing is a terminal failure after
/// `no_progress_timeout`, not after `stop_timeout`: a wedged input writer would
/// otherwise hold a graceful drain open for the entire stop budget.
fn write_part(
    stream: &mut TcpStream,
    part: &WritePart,
    input: MediaInput,
    shared: &Shared,
) -> bool {
    let bytes = part.bytes();
    let mut offset = 0;
    let mut moved = Instant::now();
    while offset < bytes.len() {
        if shared.is_aborting() {
            return false;
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                shared.fail_write(input, io::ErrorKind::WriteZero);
                return false;
            }
            Ok(count) => {
                offset += count;
                moved = Instant::now();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now().saturating_duration_since(moved)
                    > shared.limits.no_progress_timeout
                {
                    shared.fail_write(input, io::ErrorKind::TimedOut);
                    return false;
                }
            }
            Err(error) => {
                shared.fail_write(input, error.kind());
                return false;
            }
        }
    }
    true
}

/// Hands one pair to both writers, and closes any gap in the dispatched
/// sequence before it so the child's media clock keeps tracking wall clock.
struct Dispatcher {
    shared: Arc<Shared>,
    video: mpsc::SyncSender<WritePart>,
    audio: mpsc::SyncSender<WritePart>,
    /// The last pair actually handed to the writers, repeated as padding video.
    previous: Option<Arc<PairedFrame>>,
    /// The sequence the next dispatched pair should carry if nothing is lost.
    expected: Option<SequenceNumber>,
    zeros: Arc<Vec<u8>>,
    maximum_padding: u64,
}

impl Dispatcher {
    /// Returns false once the dispatcher must stop.
    fn send_pair(&mut self, video: WritePart, audio: WritePart) -> bool {
        if let Err(error) = self.video.send(video) {
            error.0.complete(false);
            audio.complete(false);
            self.shared
                .fail(StreamFailure::DispatcherClosed(MediaInput::Video));
            return false;
        }
        if let Err(error) = self.audio.send(audio) {
            error.0.complete(false);
            self.shared
                .fail(StreamFailure::DispatcherClosed(MediaInput::Audio));
            return false;
        }
        true
    }

    fn pad_to(&mut self, sequence: SequenceNumber) -> bool {
        let (Some(previous), Some(expected)) = (self.previous.clone(), self.expected) else {
            return true;
        };
        let gap = sequence.get().saturating_sub(expected.get());
        for index in 0..gap.min(self.maximum_padding) {
            if self.shared.is_aborting() {
                return false;
            }
            let missing = SequenceNumber::new(expected.get().saturating_add(index));
            let Some(length) = expected_audio_bytes(&self.shared.format, missing) else {
                return true;
            };
            let repeat = WritePart {
                payload: Payload::Pair(Arc::clone(&previous)),
                input: MediaInput::Video,
                completion: None,
            };
            let silence = WritePart {
                payload: Payload::Silence {
                    zeros: Arc::clone(&self.zeros),
                    length: length.min(self.zeros.len()),
                },
                input: MediaInput::Audio,
                completion: None,
            };
            if !self.send_pair(repeat, silence) {
                return false;
            }
            let mut data = self.shared.lock();
            data.padded = data.padded.saturating_add(1);
        }
        true
    }

    fn dispatch(&mut self, queued: Queued) -> bool {
        let sequence = queued.frame.sequence();
        if !self.pad_to(sequence) {
            return false;
        }
        let completion = Arc::new(Completion {
            shared: Arc::clone(&self.shared),
            bytes: queued.bytes,
            remaining: AtomicUsize::new(2),
            successful: AtomicBool::new(true),
        });
        let frame = Arc::new(queued.frame);
        let video = WritePart {
            payload: Payload::Pair(Arc::clone(&frame)),
            input: MediaInput::Video,
            completion: Some(Arc::clone(&completion)),
        };
        let audio = WritePart {
            payload: Payload::Pair(Arc::clone(&frame)),
            input: MediaInput::Audio,
            completion: Some(completion),
        };
        self.expected = sequence.checked_next();
        self.previous = Some(frame);
        self.send_pair(video, audio)
    }
}

fn spawn_dispatcher(
    shared: Arc<Shared>,
    video: mpsc::SyncSender<WritePart>,
    audio: mpsc::SyncSender<WritePart>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-stream-dispatcher".to_owned())
        .spawn(move || {
            let zeros = vec![0_u8; shared.format.maximum_audio_bytes().unwrap_or_default()];
            let maximum_padding =
                maximum_padding_pairs(&shared.format, shared.limits.no_progress_timeout);
            let mut dispatcher = Dispatcher {
                shared: Arc::clone(&shared),
                video,
                audio,
                previous: None,
                expected: None,
                zeros: Arc::new(zeros),
                maximum_padding,
            };
            loop {
                let queued = {
                    let mut data = shared.lock();
                    loop {
                        if data.cancelling() || shared.kill_requested.load(Ordering::Acquire) {
                            shared.discard_queue(&mut data);
                            return;
                        }
                        if let Some(queued) = data.queue.pop_front() {
                            if data.first_dispatch.is_none() {
                                data.first_dispatch = Some(Instant::now());
                            }
                            break queued;
                        }
                        if data.state != StreamState::Streaming {
                            return;
                        }
                        let (guard, _) = shared
                            .signal
                            .wait_timeout(data, POLL_INTERVAL)
                            .unwrap_or_else(PoisonError::into_inner);
                        data = guard;
                    }
                };
                if !dispatcher.dispatch(queued) {
                    return;
                }
            }
        })
}

fn spawn_progress(
    mut stdout: impl Read + Send + 'static,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-stream-progress".to_owned())
        .spawn(move || {
            let mut pending = Vec::new();
            let mut buffer = [0_u8; 4 * 1024];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => {
                        pending.extend_from_slice(&buffer[..count]);
                        while let Some(index) = pending.iter().position(|&byte| byte == b'\n') {
                            let line = pending.drain(..=index).collect::<Vec<_>>();
                            shared.observe_progress(&line);
                        }
                        if pending.len() > PROGRESS_PENDING_BYTES {
                            pending.clear();
                        }
                    }
                    Err(error) => {
                        shared.fail(StreamFailure::ProgressRead(error.kind()));
                        return;
                    }
                }
            }
        })
}

/// Redacts captured stderr before it is retained, keeping back the bytes that
/// could still be the head of a secret split across two child writes.
fn spawn_stderr(
    mut stderr: impl Read + Send + 'static,
    shared: Arc<Shared>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("fm-ffmpeg-stream-stderr".to_owned())
        .spawn(move || {
            let carry = shared.redactor.carry();
            let mut pending: Vec<u8> = Vec::new();
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        pending.extend_from_slice(&buffer[..count]);
                        shared.redactor.scrub(&mut pending);
                        let flush = match pending.iter().rposition(|&byte| byte == b'\n') {
                            Some(index) => index + 1,
                            None if pending.len() > STDERR_PENDING_BYTES + carry => {
                                pending.len() - carry
                            }
                            None => 0,
                        };
                        if flush > 0 {
                            shared.append_stderr(&pending[..flush]);
                            pending.drain(..flush);
                        }
                    }
                    Err(error) => {
                        shared.fail(StreamFailure::StderrRead(error.kind()));
                        break;
                    }
                }
            }
            if !pending.is_empty() {
                shared.redactor.scrub(&mut pending);
                shared.append_stderr(&pending);
            }
        })
}

type MonitorTarget = (Arc<Mutex<Child>>, Arc<Shared>);

/// Reaps the direct child and enforces the connect and no-progress deadlines.
fn spawn_monitor_waiter() -> Result<(mpsc::SyncSender<MonitorTarget>, JoinHandle<()>), StartError> {
    let (sender, receiver) = mpsc::sync_channel::<MonitorTarget>(1);
    let monitor = thread::Builder::new()
        .name("fm-ffmpeg-stream-monitor".to_owned())
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
                if shared.check_deadlines() {
                    try_kill_child(&child);
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

fn terminate_without_monitor(child: &Arc<Mutex<Child>>, timeout: Duration) -> CleanupStatus {
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
        .name("fm-ffmpeg-stream-emergency-reaper".to_owned())
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
        // Thread exhaustion is the sole case where not orphaning the child
        // takes priority over the bounded-return guarantee.
        let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = child.wait();
    }
    CleanupStatus::Unconfirmed
}

fn cleanup_startup(
    kind: StartErrorKind,
    child: &Arc<Mutex<Child>>,
    shared: &Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
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
    shared.kill_requested.store(true, Ordering::Release);
    shared.aborting.store(true, Ordering::Release);
    shared.signal.notify_all();
    request_kill(child);
    let deadline = Instant::now() + timeout;
    let mut all = workers;
    all.push(monitor);
    wait_until(deadline, || {
        poll_child(child, shared);
        all.iter().all(JoinHandle::is_finished)
    });
    let mut unfinished = Vec::new();
    for worker in all {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            unfinished.push(CleanupWorker::Unit(worker));
        }
    }
    let child_done = shared.lock().child.is_some();
    let kind = match kind {
        StartErrorKind::EarlyExit { status, .. } => StartErrorKind::EarlyExit {
            status,
            stderr: shared.snapshot().stderr_tail,
        },
        kind => kind,
    };
    let cleanup = if unfinished.is_empty() && child_done {
        CleanupStatus::Complete
    } else {
        shared.lock().frozen = true;
        spawn_fallback_cleanup(unfinished);
        CleanupStatus::Unconfirmed
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

/// Starts unbounded joining after a bounded public return. If the operating
/// system cannot create this thread, joins synchronously as the last-resort
/// ownership path; only that case may exceed the configured deadline.
fn spawn_fallback_cleanup(workers: Vec<CleanupWorker>) {
    if workers.is_empty() {
        return;
    }
    let holder = Arc::new(Mutex::new(Some(workers)));
    let background = Arc::clone(&holder);
    if thread::Builder::new()
        .name("fm-ffmpeg-stream-fallback-cleanup".to_owned())
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
    for CleanupWorker::Unit(worker) in workers {
        let _ = worker.join();
    }
}

/// Ensures the directory one local HLS destination publishes into.
///
/// Only the leaf may be missing: an HLS destination is an operator-chosen
/// absolute path, and silently creating missing parents would materialize a
/// typo. A ladder calls this once for the destination itself and once for each
/// variant directory beneath it; the destination is created first, so every
/// variant directory is still exactly one leaf. `FFmpeg` still decides whether
/// the directory is writable; this check exists so an unusable destination is a
/// path-free startup error rather than a child that dies during its first mux.
fn ensure_hls_directory(path: &str) -> Result<(), StartErrorKind> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| StartErrorKind::HlsDirectoryUnusable)
        }
        Ok(_) | Err(_) => Err(StartErrorKind::HlsDirectoryUnusable),
    }
}

fn streamer_executable(executable: Executable) -> Result<OsString, StartError> {
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

struct Inputs<'a> {
    video_address: SocketAddr,
    audio_address: SocketAddr,
    video_token: &'a str,
    audio_token: &'a str,
}

fn command_args(config: &StreamConfig, layout: &str, inputs: &Inputs<'_>) -> Vec<OsString> {
    let format = &config.format;
    let dimensions = format.dimensions();
    let rate = format.frame_rate();
    let fps = u64::from(rate.numerator()).div_ceil(u64::from(rate.denominator()));
    let gop = fps.saturating_mul(u64::from(config.encoder.keyframe_interval_seconds));
    let mut values = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "warning".to_owned(),
        "-nostats".to_owned(),
        "-progress".to_owned(),
        "pipe:1".to_owned(),
        "-stats_period".to_owned(),
        STATS_PERIOD.to_owned(),
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pixel_format".to_owned(),
        "rgba".to_owned(),
        "-video_size".to_owned(),
        format!("{}x{}", dimensions.width(), dimensions.height()),
        "-framerate".to_owned(),
        format!("{}/{}", rate.numerator(), rate.denominator()),
        // Both raw inputs are fully described on this command line, so there is
        // nothing left to probe. Without this the child spends `analyzeduration`
        // (5s by default) collecting one input before it reads a byte of the
        // other, while the pair writers can only advance in lockstep: the sink
        // deadlocks at startup at every frame size that does not fit in a socket
        // buffer, which is every real broadcast resolution.
        "-probesize".to_owned(),
        PROBE_SIZE.to_owned(),
        "-analyzeduration".to_owned(),
        "0".to_owned(),
        "-protocol_whitelist".to_owned(),
        "http,tcp".to_owned(),
        "-i".to_owned(),
        format!("http://{}/{}", inputs.video_address, inputs.video_token),
        "-f".to_owned(),
        "f32le".to_owned(),
        "-ar".to_owned(),
        format.sample_rate().hertz().to_string(),
        "-ac".to_owned(),
        format.channel_layout().channels().len().to_string(),
        "-channel_layout".to_owned(),
        layout.to_owned(),
        "-probesize".to_owned(),
        PROBE_SIZE.to_owned(),
        "-analyzeduration".to_owned(),
        "0".to_owned(),
        "-protocol_whitelist".to_owned(),
        "http,tcp".to_owned(),
        "-i".to_owned(),
        format!("http://{}/{}", inputs.audio_address, inputs.audio_token),
    ];
    // `Streamer::start` refuses a ladder for every non-HLS destination before
    // any argument is built, so a ladder reaching here is always the local HLS
    // segmenter's multi-rendition shape.
    if let Some(ladder) = config
        .ladder
        .as_ref()
        .filter(|_| config.destination.kind() == DestinationKind::Hls)
    {
        values.extend(ladder_encoder_args(ladder, config.encoder, gop));
        values.extend(ladder_output_args(
            &config.destination.url,
            config.encoder.keyframe_interval_seconds,
            ladder.rungs().len(),
        ));
    } else {
        values.extend(single_rendition_encoder_args(config.encoder, gop));
        values.extend(output_args(
            config.destination.kind(),
            &config.destination.url,
            config.encoder.keyframe_interval_seconds,
        ));
    }
    values.into_iter().map(OsString::from).collect()
}

/// The single-rendition encoder block: one video stream and one audio stream,
/// both mapped from the pair inputs.
fn single_rendition_encoder_args(encoder: EncoderSettings, gop: u64) -> Vec<String> {
    let video_bitrate = format!("{}k", encoder.video_bitrate_kbps);
    vec![
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-map".to_owned(),
        "1:a:0".to_owned(),
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-preset".to_owned(),
        "veryfast".to_owned(),
        "-tune".to_owned(),
        "zerolatency".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
        "-b:v".to_owned(),
        video_bitrate.clone(),
        "-maxrate".to_owned(),
        video_bitrate,
        "-bufsize".to_owned(),
        format!("{}k", encoder.video_bitrate_kbps.saturating_mul(2)),
        "-g".to_owned(),
        gop.to_string(),
        "-keyint_min".to_owned(),
        gop.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-force_key_frames".to_owned(),
        format!("expr:gte(t,n_forced*{})", encoder.keyframe_interval_seconds),
        "-c:a".to_owned(),
        "aac".to_owned(),
        "-b:a".to_owned(),
        format!("{}k", encoder.audio_bitrate_kbps),
        "-flush_packets".to_owned(),
        "1".to_owned(),
    ]
}

/// The ladder encoder block: the filter graph that splits the one decoded
/// video input into one scaled stream per rung, then per rung the mapped video
/// stream with its own bitrate cap and one mapped AAC stream, and finally the
/// shared encoder flags. The child does the scaling itself, from the one
/// decoded frame it already holds; nothing is decoded twice.
fn ladder_encoder_args(ladder: &HlsLadder, encoder: EncoderSettings, gop: u64) -> Vec<String> {
    let mut values = vec![
        "-filter_complex".to_owned(),
        ladder_filter_complex(ladder.rungs()),
    ];
    for (index, rung) in ladder.rungs().iter().enumerate() {
        let bitrate = rung.video_bitrate_kbps;
        values.extend([
            "-map".to_owned(),
            format!("[s{index}]"),
            format!("-c:v:{index}"),
            "libx264".to_owned(),
            format!("-b:v:{index}"),
            format!("{bitrate}k"),
            format!("-maxrate:v:{index}"),
            format!("{bitrate}k"),
            format!("-bufsize:v:{index}"),
            format!("{}k", bitrate.saturating_mul(2)),
            "-map".to_owned(),
            "1:a:0".to_owned(),
            format!("-c:a:{index}"),
            "aac".to_owned(),
            format!("-b:a:{index}"),
            format!("{}k", encoder.audio_bitrate_kbps),
        ]);
    }
    values.extend([
        "-preset".to_owned(),
        "veryfast".to_owned(),
        "-tune".to_owned(),
        "zerolatency".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
        "-g".to_owned(),
        gop.to_string(),
        "-keyint_min".to_owned(),
        gop.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-force_key_frames".to_owned(),
        format!("expr:gte(t,n_forced*{})", encoder.keyframe_interval_seconds),
        "-flush_packets".to_owned(),
        "1".to_owned(),
    ]);
    values
}

/// The filter graph that splits the one decoded video input into one scaled
/// stream per rung:
/// `[0:v]split=N[v0]...[vN-1];[v0]scale=W0:H0[s0];...`.
fn ladder_filter_complex(rungs: &[HlsLadderRung]) -> String {
    use fmt::Write as _;
    let mut graph = format!("[0:v]split={}", rungs.len());
    let mut scales = String::new();
    for (index, rung) in rungs.iter().enumerate() {
        let _ = write!(graph, "[v{index}]");
        let _ = write!(
            scales,
            ";[v{index}]scale={}:{}[s{index}]",
            rung.width, rung.height
        );
    }
    graph.push_str(&scales);
    graph
}

/// The container each transport is muxed into: FLV for RTMP(S), MPEG-TS for
/// SRT, and the HLS segmenter for a local directory.
#[must_use]
const fn muxer_format(kind: DestinationKind) -> &'static str {
    match kind {
        DestinationKind::Rtmp | DestinationKind::Rtmps => "flv",
        DestinationKind::Srt => "mpegts",
        DestinationKind::Hls => "hls",
    }
}

/// The segmenter settings every HLS output shares: the muxer, the segment
/// duration, the bounded playlist window, and the segment flags. Only the
/// segment filename and the playlists differ between one rendition and a
/// ladder.
fn hls_segmenter_args(keyframe_interval_seconds: u32) -> Vec<String> {
    vec![
        "-y".to_owned(),
        "-f".to_owned(),
        muxer_format(DestinationKind::Hls).to_owned(),
        "-hls_time".to_owned(),
        keyframe_interval_seconds.to_string(),
        "-hls_list_size".to_owned(),
        "6".to_owned(),
        "-hls_flags".to_owned(),
        "delete_segments+independent_segments+temp_file".to_owned(),
    ]
}

/// The single-rendition output-side argument tail, from the first
/// muxer-owned flag to the destination URL. Kept pure so tests can assert
/// exactly what the child is told to mux.
///
/// `keyframe_interval_seconds` is the encoder's keyframe interval, which an
/// HLS destination uses as its segment duration so every segment starts on a
/// keyframe; other transports ignore it.
fn output_args(kind: DestinationKind, url: &str, keyframe_interval_seconds: u32) -> Vec<String> {
    let mut values = Vec::new();
    if kind == DestinationKind::Hls {
        // The destination URL names the directory; the segment template and
        // the playlist both live inside it, and the playlist is the output
        // file the child writes.
        let directory = url.strip_prefix(HLS_SCHEME).unwrap_or(url);
        values.extend(hls_segmenter_args(keyframe_interval_seconds));
        values.extend([
            "-hls_segment_filename".to_owned(),
            format!("{directory}/segment-%05d.ts"),
            format!("{directory}/index.m3u8"),
        ]);
        return values;
    }
    if kind != DestinationKind::Srt {
        // FLV-only: suppress the duration/filesize warnings a live FLV muxer
        // would otherwise print, since a live stream has neither. MPEG-TS has
        // no such flag and needs no equivalent.
        values.extend(["-flvflags".to_owned(), "no_duration_filesize".to_owned()]);
    }
    values.extend([
        "-f".to_owned(),
        muxer_format(kind).to_owned(),
        url.to_owned(),
    ]);
    values
}

/// The HLS ladder output tail: the shared segmenter settings, the per-variant
/// `%v` segment template, the master playlist name, the explicit
/// stream-to-variant map, and the per-variant output URL.
fn ladder_output_args(url: &str, keyframe_interval_seconds: u32, rung_count: usize) -> Vec<String> {
    let directory = url.strip_prefix(HLS_SCHEME).unwrap_or(url);
    let var_stream_map = (0..rung_count)
        .map(|index| format!("v:{index},a:{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut values = hls_segmenter_args(keyframe_interval_seconds);
    values.extend([
        "-hls_segment_filename".to_owned(),
        format!("{directory}/%v/segment-%05d.ts"),
        "-master_pl_name".to_owned(),
        "index.m3u8".to_owned(),
        "-var_stream_map".to_owned(),
        var_stream_map,
        format!("{directory}/%v/index.m3u8"),
    ]);
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "live_998877_SuperSecretStreamKey";

    fn destination() -> StreamDestination {
        StreamDestination::parse(&format!("rtmp://ingest.example:1935/app/{KEY}")).unwrap()
    }

    #[test]
    fn destinations_are_scheme_checked_and_never_render_their_key() {
        for rejected in [
            ("", DestinationError::Empty),
            (
                "http://host/app/key12345",
                DestinationError::UnsupportedScheme,
            ),
            ("srt://host/app/key12345", DestinationError::MalformedSrtUrl),
            ("file:///tmp/x", DestinationError::UnsupportedScheme),
            ("rtmp://", DestinationError::MissingStreamKey),
            ("rtmp:///app/key12345", DestinationError::MissingHost),
            ("rtmp://host/onlyapp", DestinationError::MissingStreamKey),
            ("rtmp://host/app/ab", DestinationError::StreamKeyTooShort),
            (
                "rtmp://user:pw@host/app/key12345",
                DestinationError::EmbeddedCredentials,
            ),
            (
                "rtmp://host/app/key 12345",
                DestinationError::InvalidCharacter,
            ),
        ] {
            assert_eq!(
                StreamDestination::parse(rejected.0),
                Err(rejected.1),
                "{}",
                rejected.0
            );
        }

        let destination = destination();
        assert_eq!(
            destination.redacted(),
            "rtmp://ingest.example:1935/app/****"
        );
        for rendered in [
            format!("{destination}"),
            format!("{destination:?}"),
            format!("{:?}", StreamConfig::new(format(), destination.clone())),
        ] {
            assert!(!rendered.contains(KEY), "{rendered}");
            assert!(rendered.contains("****"), "{rendered}");
        }
        assert_eq!(
            StreamDestination::parse("rtmps://host/app/key12345?token=abcdef")
                .unwrap()
                .redacted(),
            "rtmps://host/app/****"
        );
    }

    const SRT_KEY: &str = "SRT_live_998877_SecretStreamId";

    #[test]
    fn srt_destinations_are_shape_checked_and_mask_only_the_stream_id() {
        for rejected in [
            ("srt://", DestinationError::MissingHost),
            ("srt://?streamid=abcd1234", DestinationError::MissingHost),
            ("srt://host/app/key12345", DestinationError::MalformedSrtUrl),
            ("srt://host/", DestinationError::MalformedSrtUrl),
            (
                "srt://host/app/key12345?streamid=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://host/streamid=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            ("srt://host?streamid=", DestinationError::StreamKeyTooShort),
            (
                "srt://host?streamid=ab",
                DestinationError::StreamKeyTooShort,
            ),
            (
                "srt://host?other=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            ("srt://host?abcd1234", DestinationError::MalformedSrtUrl),
            ("srt://host?", DestinationError::MalformedSrtUrl),
            (
                "srt://host?streamid=abcd1234&mode=caller",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://host?mode=caller&streamid=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://host??streamid=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://host?streamid=abcd1234?",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://host?streamid=ab?cd1234",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://user@host?streamid=abcd1234",
                DestinationError::EmbeddedCredentials,
            ),
            (
                "srt://host:9710/x?streamid=abcd1234",
                DestinationError::MalformedSrtUrl,
            ),
            (
                "srt://ho st?streamid=abcd1234",
                DestinationError::InvalidCharacter,
            ),
        ] {
            assert_eq!(
                StreamDestination::parse(rejected.0),
                Err(rejected.1),
                "{}",
                rejected.0
            );
        }
        let too_long = format!("srt://{}?streamid={}", "h".repeat(2 * 1024), SRT_KEY);
        assert_eq!(
            StreamDestination::parse(&too_long),
            Err(DestinationError::TooLong)
        );

        // Exactly at every boundary the sink accepts.
        let minimal = StreamDestination::parse("srt://h?streamid=abcd").unwrap();
        assert_eq!(minimal.kind(), DestinationKind::Srt);
        assert_eq!(minimal.redacted(), "srt://h?streamid=****");
        // A port with no query carries no secret, so redaction is identity.
        let bare = StreamDestination::parse("srt://ingest.example:9710").unwrap();
        assert_eq!(bare.kind(), DestinationKind::Srt);
        assert_eq!(bare.redacted(), "srt://ingest.example:9710");

        let destination = StreamDestination::parse(
            "srt://ingest.example:9710?streamid=SRT_live_998877_SecretStreamId",
        )
        .unwrap();
        // Only the stream id is masked; the visible `?streamid=` shape stays.
        assert_eq!(
            destination.redacted(),
            "srt://ingest.example:9710?streamid=****"
        );
        for rendered in [
            format!("{destination}"),
            format!("{destination:?}"),
            format!("{:?}", StreamConfig::new(format(), destination.clone())),
        ] {
            assert!(!rendered.contains(SRT_KEY), "{rendered}");
            assert!(rendered.contains("****"), "{rendered}");
        }
    }

    #[test]
    fn kind_maps_scheme_to_transport_and_srt_to_mpegts_args() {
        assert_eq!(
            StreamDestination::parse("rtmp://host/app/key12345")
                .unwrap()
                .kind(),
            DestinationKind::Rtmp
        );
        assert_eq!(
            StreamDestination::parse("rtmps://host/app/key12345")
                .unwrap()
                .kind(),
            DestinationKind::Rtmps
        );
        assert_eq!(
            StreamDestination::parse("srt://host?streamid=abcd1234")
                .unwrap()
                .kind(),
            DestinationKind::Srt
        );
        assert_eq!(muxer_format(DestinationKind::Rtmp), "flv");
        assert_eq!(muxer_format(DestinationKind::Rtmps), "flv");
        assert_eq!(muxer_format(DestinationKind::Srt), "mpegts");

        let video_address: SocketAddr = "127.0.0.1:20001".parse().unwrap();
        let audio_address: SocketAddr = "127.0.0.1:20002".parse().unwrap();
        let inputs = Inputs {
            video_address,
            audio_address,
            video_token: "video-token",
            audio_token: "audio-token",
        };
        let srt = StreamConfig::new(
            format(),
            StreamDestination::parse("srt://127.0.0.1:9000?streamid=abcd1234").unwrap(),
        );
        let args = command_args(&srt, "stereo", &inputs);
        let text = args
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect::<Vec<_>>();
        let url_position = text.len() - 1;
        assert_eq!(text[url_position], "srt://127.0.0.1:9000?streamid=abcd1234");
        assert_eq!(text[url_position - 1], "mpegts");
        assert_eq!(text[url_position - 2], "-f");
        assert!(!text.contains(&"-flvflags"), "{text:?}");

        let rtmp = StreamConfig::new(
            format(),
            StreamDestination::parse("rtmp://127.0.0.1:1935/app/key12345").unwrap(),
        );
        let args = command_args(&rtmp, "stereo", &inputs);
        let text = args
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect::<Vec<_>>();
        let url_position = text.len() - 1;
        assert_eq!(text[url_position], "rtmp://127.0.0.1:1935/app/key12345");
        assert_eq!(text[url_position - 1], "flv");
        assert_eq!(text[url_position - 2], "-f");
        let flvflags = text
            .windows(2)
            .position(|window| window == ["-flvflags", "no_duration_filesize"])
            .expect("FLV output keeps -flvflags");
        assert!(flvflags < url_position - 2);
    }

    #[test]
    fn mpegts_carries_wider_audio_than_flv() {
        let surround = ChannelLayout::new(vec![
            Channel::Left,
            Channel::Right,
            Channel::Center,
            Channel::LowFrequency,
            Channel::LeftSurround,
            Channel::RightSurround,
        ])
        .unwrap();
        assert_eq!(flv_channel_layout(&surround), None);
        assert_eq!(mpegts_channel_layout(&surround), Some("5.1"));
        assert_eq!(
            output_channel_layout(DestinationKind::Srt, &surround),
            Some("5.1")
        );
        // HLS is MPEG-TS underneath and follows the same layout rules.
        assert_eq!(
            output_channel_layout(DestinationKind::Hls, &surround),
            Some("5.1")
        );
        assert_eq!(
            output_channel_layout(DestinationKind::Rtmp, &surround),
            None
        );
        assert_eq!(
            output_channel_layout(DestinationKind::Rtmps, &surround),
            None
        );
        for (kind, layout) in [
            (
                DestinationKind::Rtmp,
                flv_channel_layout(&ChannelLayout::stereo()),
            ),
            (
                DestinationKind::Rtmps,
                flv_channel_layout(&ChannelLayout::stereo()),
            ),
            (
                DestinationKind::Srt,
                mpegts_channel_layout(&ChannelLayout::stereo()),
            ),
        ] {
            assert_eq!(
                output_channel_layout(kind, &ChannelLayout::stereo()),
                layout
            );
            assert_eq!(layout, Some("stereo"));
        }
        // A duplicated-channel pseudo-layout is not a named configuration and
        // is refused rather than guessed.
        let doubled = ChannelLayout::new(vec![Channel::Mono, Channel::Mono]).unwrap();
        assert_eq!(mpegts_channel_layout(&doubled), None);
        assert_eq!(output_channel_layout(DestinationKind::Srt, &doubled), None);
    }

    fn format() -> RecordFormat {
        RecordFormat::new(
            64,
            48,
            fm_types::FrameRate::new(30, 1).unwrap(),
            fm_types::SampleRate::new(48_000).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap()
    }

    #[test]
    fn stderr_redaction_survives_chunk_splits_and_ring_truncation() {
        let destination = destination();
        let redactor = Redactor::new(&destination, ["videotoken", "audiotoken"]);
        let shared = Arc::new(Shared::new(
            &format(),
            StreamLimits {
                max_stderr_bytes: 96,
                ..StreamLimits::default()
            },
            OverflowPolicy::Reject,
            &destination,
            redactor.clone(),
        ));

        // Feed the child's real refusal message one byte at a time, which is
        // the worst case for a secret split across reads.
        let message = format!(
            "[out#0/flv @ 0x1] Error opening output rtmp://ingest.example:1935/app/{KEY}: Connection refused\nError opening output file rtmp://ingest.example:1935/app/{KEY}.\n"
        );
        let carry = redactor.carry();
        let mut pending: Vec<u8> = Vec::new();
        for byte in message.as_bytes() {
            pending.push(*byte);
            redactor.scrub(&mut pending);
            let flush = match pending.iter().rposition(|&byte| byte == b'\n') {
                Some(index) => index + 1,
                None if pending.len() > STDERR_PENDING_BYTES + carry => pending.len() - carry,
                None => 0,
            };
            if flush > 0 {
                shared.append_stderr(&pending[..flush]);
                pending.drain(..flush);
            }
            // Nothing retained may ever contain even a prefix of the key.
            let retained = shared.snapshot().stderr_tail;
            assert!(!retained.contains(&KEY[..8]), "{retained}");
        }
        assert!(pending.is_empty());

        let telemetry = shared.snapshot();
        assert!(telemetry.stderr_truncated, "{telemetry:?}");
        assert!(telemetry.stderr_tail.len() <= 96);
        assert!(!telemetry.stderr_tail.contains(&KEY[..8]));
        assert!(telemetry.stderr_tail.contains("****"));
        assert!(!telemetry.destination.contains(KEY));
    }

    #[test]
    fn limits_and_layouts_are_validated_before_any_spawn() {
        let format = format();
        let encoder = EncoderSettings::default();
        let drop_oldest = OverflowPolicy::DropOldest;
        assert_eq!(
            validate_limits(&format, StreamLimits::default(), drop_oldest, encoder),
            Ok(())
        );
        // One frame period at 30fps is 33.3ms; a render thread may not be asked
        // to wait longer than the frame it is producing.
        assert_eq!(
            validate_limits(
                &format,
                StreamLimits {
                    enqueue_timeout: Duration::from_millis(34),
                    ..StreamLimits::default()
                },
                drop_oldest,
                encoder
            ),
            Err(LimitsError::EnqueueTimeoutTooLong)
        );
        assert_eq!(
            validate_limits(
                &format,
                StreamLimits {
                    enqueue_timeout: Duration::from_millis(33),
                    ..StreamLimits::default()
                },
                drop_oldest,
                encoder
            ),
            Ok(())
        );
        // A queue with nothing recallable cannot honour `DropOldest`, but is
        // perfectly usable with `Reject`.
        assert_eq!(
            validate_limits(
                &format,
                StreamLimits {
                    max_outstanding_pairs: COMMITTED_PAIRS,
                    ..StreamLimits::default()
                },
                drop_oldest,
                encoder
            ),
            Err(LimitsError::OutstandingPairsTooFewToDropOldest {
                required: COMMITTED_PAIRS + 1
            })
        );
        assert_eq!(
            validate_limits(
                &format,
                StreamLimits {
                    max_outstanding_pairs: 1,
                    ..StreamLimits::default()
                },
                OverflowPolicy::Reject,
                encoder
            ),
            Ok(())
        );
        assert!(matches!(
            validate_limits(
                &format,
                StreamLimits {
                    max_retained_bytes: 1_024,
                    ..StreamLimits::default()
                },
                drop_oldest,
                encoder
            ),
            Err(LimitsError::RetainedBytesTooSmall { .. })
        ));
        assert_eq!(
            validate_limits(
                &format,
                StreamLimits::default(),
                drop_oldest,
                EncoderSettings {
                    video_bitrate_kbps: 0,
                    ..encoder
                }
            ),
            Err(LimitsError::VideoBitrate)
        );
        assert_eq!(flv_channel_layout(&ChannelLayout::stereo()), Some("stereo"));
        assert_eq!(
            flv_channel_layout(&ChannelLayout::new(vec![Channel::Mono]).unwrap()),
            Some("mono")
        );
        assert_eq!(
            flv_channel_layout(
                &ChannelLayout::new(vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::Center,
                    Channel::LowFrequency,
                    Channel::LeftSurround,
                    Channel::RightSurround,
                ])
                .unwrap()
            ),
            None
        );
    }

    const HLS_URL: &str = "hls:///srv/freemix/output/hls";

    #[test]
    fn hls_destinations_are_absolute_segment_paths_and_render_verbatim() {
        for (accepted, directory) in [
            (HLS_URL, "/srv/freemix/output/hls"),
            ("hls:///a", "/a"),
            ("hls:///srv/out.v1", "/srv/out.v1"),
            // A key-shaped segment is still just a path: HLS has no secret.
            (
                "hls:///srv/live_998877_SuperSecretStreamKey",
                "/srv/live_998877_SuperSecretStreamKey",
            ),
        ] {
            let destination = StreamDestination::parse(accepted).unwrap();
            assert_eq!(destination.kind(), DestinationKind::Hls);
            assert_eq!(destination.local_directory(), Some(directory));
            assert_eq!(destination.redacted(), accepted);
            assert_eq!(format!("{destination}"), accepted);
            assert_eq!(
                format!("{destination:?}"),
                format!("StreamDestination({accepted:?})")
            );
            assert!(!destination.redacted().contains("****"), "{accepted}");
        }
        for (rejected, expected) in [
            ("hls://", DestinationError::PathNotAbsolute),
            ("hls://relative/path", DestinationError::PathNotAbsolute),
            ("hls:///", DestinationError::MalformedHlsPath),
            ("hls:///trailing/", DestinationError::MalformedHlsPath),
            ("hls:///double//segment", DestinationError::MalformedHlsPath),
            ("hls:///dot/./segment", DestinationError::MalformedHlsPath),
            (
                "hls:///dotdot/../segment",
                DestinationError::MalformedHlsPath,
            ),
        ] {
            assert_eq!(
                StreamDestination::parse(rejected),
                Err(expected),
                "{rejected}"
            );
        }
        // The URL-wide character rule runs before the path rules.
        assert_eq!(
            StreamDestination::parse("hls:///srv/out put"),
            Err(DestinationError::InvalidCharacter)
        );
        // Network destinations still have no local directory.
        assert_eq!(destination().local_directory(), None);
        assert_eq!(
            StreamDestination::parse("srt://ingest.example:9710")
                .unwrap()
                .local_directory(),
            None
        );
    }

    #[test]
    fn hls_output_tail_is_exact_and_uses_the_keyframe_interval() {
        assert_eq!(muxer_format(DestinationKind::Hls), "hls");
        let expected = [
            "-y",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "6",
            "-hls_flags",
            "delete_segments+independent_segments+temp_file",
            "-hls_segment_filename",
            "/srv/freemix/output/hls/segment-%05d.ts",
            "/srv/freemix/output/hls/index.m3u8",
        ]
        .map(str::to_owned);
        assert_eq!(output_args(DestinationKind::Hls, HLS_URL, 2), expected);
        // The encoder's keyframe interval is passed through, not baked in.
        let longer = output_args(DestinationKind::Hls, HLS_URL, 5);
        let interval = longer
            .windows(2)
            .find(|window| window[0] == "-hls_time")
            .expect("an HLS tail carries -hls_time");
        assert_eq!(interval[1], "5");

        // Through `command_args` the tail is byte-for-byte what the child is
        // driven with, and no FLV-only flag leaks into it.
        let video_address: SocketAddr = "127.0.0.1:20001".parse().unwrap();
        let audio_address: SocketAddr = "127.0.0.1:20002".parse().unwrap();
        let inputs = Inputs {
            video_address,
            audio_address,
            video_token: "video-token",
            audio_token: "audio-token",
        };
        let hls = StreamConfig::new(format(), StreamDestination::parse(HLS_URL).unwrap());
        let args = command_args(&hls, "stereo", &inputs);
        let text = args
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect::<Vec<_>>();
        let expected_text = expected.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(&text[text.len() - expected_text.len()..], expected_text);
        assert!(!text.contains(&"-flvflags"), "{text:?}");
    }

    fn ladder_rung(video_bitrate_kbps: u32, width: u32, height: u32) -> HlsLadderRung {
        HlsLadderRung {
            video_bitrate_kbps,
            width,
            height,
        }
    }

    /// The two-rung ladder the argv and start tests share: thinnest first, as
    /// [`HlsLadder`] validates.
    fn ladder() -> HlsLadder {
        HlsLadder::new(vec![
            ladder_rung(1_000, 320, 180),
            ladder_rung(1_500, 640, 360),
        ])
        .unwrap()
    }

    fn inputs() -> Inputs<'static> {
        Inputs {
            video_address: "127.0.0.1:20001".parse().unwrap(),
            audio_address: "127.0.0.1:20002".parse().unwrap(),
            video_token: "video-token",
            audio_token: "audio-token",
        }
    }

    #[test]
    fn hls_ladders_validate_rung_count_bitrates_dimensions_and_order() {
        // The smallest and largest accepted values, and the Display forms.
        let bounds = HlsLadder::new(vec![
            ladder_rung(1_000, 16, 16),
            ladder_rung(100_000, 7_680, 4_320),
        ])
        .unwrap();
        assert_eq!(bounds.rungs().len(), 2);
        assert_eq!(bounds.top_bitrate_kbps(), 100_000);
        assert_eq!(format!("{}", bounds.rungs()[0]), "16x16@1000");
        assert_eq!(format!("{bounds}"), "16x16@1000,7680x4320@100000");
        assert_eq!(
            format!("{}", HlsLadderError::BitratesNotIncreasing),
            "ladder bitrates must strictly increase from the first rung to the last"
        );

        // Eight rungs are still one child; nine are not.
        let eight = (0..8_u32)
            .map(|index| ladder_rung(1_000 + index * 500, 320, 180))
            .collect::<Vec<_>>();
        assert!(HlsLadder::new(eight).is_ok());
        let nine = (0..9_u32)
            .map(|index| ladder_rung(1_000 + index * 500, 320, 180))
            .collect::<Vec<_>>();
        assert_eq!(HlsLadder::new(nine), Err(HlsLadderError::TooManyRungs));
        // A ladder is a choice, so one rung is not a ladder at all.
        assert_eq!(HlsLadder::new(Vec::new()), Err(HlsLadderError::TooFewRungs));
        assert_eq!(
            HlsLadder::new(vec![ladder_rung(1_500, 640, 360)]),
            Err(HlsLadderError::TooFewRungs)
        );

        // Each remaining rule, one case at a time.
        for (rungs, expected) in [
            (
                vec![ladder_rung(999, 320, 180), ladder_rung(1_500, 640, 360)],
                HlsLadderError::BitrateTooLow,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(100_001, 640, 360)],
                HlsLadderError::BitrateTooHigh,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_500, 8, 8)],
                HlsLadderError::DimensionTooSmall,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_500, 7_682, 360)],
                HlsLadderError::DimensionTooLarge,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_500, 640, 4_322)],
                HlsLadderError::DimensionTooLarge,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_500, 641, 360)],
                HlsLadderError::OddDimension,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_500, 320, 181)],
                HlsLadderError::OddDimension,
            ),
            (
                vec![ladder_rung(1_000, 320, 180), ladder_rung(1_000, 640, 360)],
                HlsLadderError::BitratesNotIncreasing,
            ),
            (
                vec![ladder_rung(1_500, 640, 360), ladder_rung(1_000, 320, 180)],
                HlsLadderError::BitratesNotIncreasing,
            ),
            (
                vec![ladder_rung(1_000, 640, 360), ladder_rung(1_500, 320, 360)],
                HlsLadderError::DimensionsDecrease,
            ),
            (
                vec![ladder_rung(1_000, 640, 360), ladder_rung(1_500, 640, 180)],
                HlsLadderError::DimensionsDecrease,
            ),
        ] {
            assert_eq!(HlsLadder::new(rungs), Err(expected), "{expected:?}");
        }

        // Equal frame sizes are allowed while the bitrates still climb.
        let equal = HlsLadder::new(vec![
            ladder_rung(1_500, 640, 360),
            ladder_rung(3_000, 640, 360),
        ])
        .unwrap();
        assert_eq!(equal.top_bitrate_kbps(), 3_000);
    }

    #[test]
    fn hls_ladder_argv_is_exact() {
        let mut config = StreamConfig::new(format(), StreamDestination::parse(HLS_URL).unwrap());
        config.ladder = Some(ladder());
        let args = command_args(&config, "stereo", &inputs());
        let text = args
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect::<Vec<_>>();
        let start = text
            .iter()
            .position(|argument| *argument == "-filter_complex")
            .expect("a ladder argv carries its filter graph");
        let expected = [
            "-filter_complex",
            "[0:v]split=2[v0][v1];[v0]scale=320:180[s0];[v1]scale=640:360[s1]",
            "-map",
            "[s0]",
            "-c:v:0",
            "libx264",
            "-b:v:0",
            "1000k",
            "-maxrate:v:0",
            "1000k",
            "-bufsize:v:0",
            "2000k",
            "-map",
            "1:a:0",
            "-c:a:0",
            "aac",
            "-b:a:0",
            "128k",
            "-map",
            "[s1]",
            "-c:v:1",
            "libx264",
            "-b:v:1",
            "1500k",
            "-maxrate:v:1",
            "1500k",
            "-bufsize:v:1",
            "3000k",
            "-map",
            "1:a:0",
            "-c:a:1",
            "aac",
            "-b:a:1",
            "128k",
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
            "-force_key_frames",
            "expr:gte(t,n_forced*2)",
            "-flush_packets",
            "1",
            "-y",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "6",
            "-hls_flags",
            "delete_segments+independent_segments+temp_file",
            "-hls_segment_filename",
            "/srv/freemix/output/hls/%v/segment-%05d.ts",
            "-master_pl_name",
            "index.m3u8",
            "-var_stream_map",
            "v:0,a:0 v:1,a:1",
            "/srv/freemix/output/hls/%v/index.m3u8",
        ];
        // The whole tail, exactly: no single-rendition map, no unsuffixed
        // codec flags, and no FLV-only flag may survive into a ladder.
        assert_eq!(&text[start..], expected.as_slice(), "{text:?}");
        assert!(!text.contains(&"0:v:0"), "{text:?}");
        assert!(!text.contains(&"-c:v"), "{text:?}");
        assert!(!text.contains(&"-b:v"), "{text:?}");
        assert!(!text.contains(&"-b:a"), "{text:?}");
        assert!(!text.contains(&"-flvflags"), "{text:?}");
    }

    #[test]
    fn hls_ladder_filter_graph_and_variant_map_scale_every_rung() {
        // Three rungs: one scale per rung, one variant pair per rung.
        let three = HlsLadder::new(vec![
            ladder_rung(1_000, 320, 180),
            ladder_rung(1_500, 640, 360),
            ladder_rung(3_000, 1_280, 720),
        ])
        .unwrap();
        assert_eq!(
            ladder_filter_complex(three.rungs()),
            "[0:v]split=3[v0][v1][v2];[v0]scale=320:180[s0];\
             [v1]scale=640:360[s1];[v2]scale=1280:720[s2]"
        );
        let tail = ladder_output_args(HLS_URL, 2, three.rungs().len());
        let var_stream_map = tail
            .windows(2)
            .find(|window| window[0] == "-var_stream_map")
            .expect("a ladder tail carries -var_stream_map");
        assert_eq!(var_stream_map[1], "v:0,a:0 v:1,a:1 v:2,a:2");
    }

    #[test]
    fn start_refuses_a_ladder_for_every_network_destination() {
        for url in [
            "rtmp://127.0.0.1:1/app/key12345",
            "rtmps://127.0.0.1:1/app/key12345",
            "srt://127.0.0.1:1?streamid=abcd1234",
        ] {
            let mut config = StreamConfig::new(format(), StreamDestination::parse(url).unwrap());
            config.ladder = Some(ladder());
            let Err(error) = Streamer::start(config) else {
                panic!("a ladder must be refused for {url}");
            };
            assert_eq!(error.kind, StartErrorKind::LadderNotSupported, "{error:?}");
            assert_eq!(error.cleanup, CleanupStatus::Complete, "{error:?}");
            // The refusal happens before any executable is resolved or any
            // socket is bound, so no child can exist, and its kind carries no
            // path or URL text.
            assert!(!format!("{error:?}").contains("127.0.0.1"), "{error:?}");
        }
    }

    #[test]
    fn start_refuses_a_ladder_variant_directory_occupied_by_a_file() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("hls");
        fs::create_dir(&directory).unwrap();
        // The destination itself is a directory, but the first variant
        // directory beneath it is not.
        fs::write(directory.join("0"), b"occupied").unwrap();
        let destination =
            StreamDestination::parse(&format!("hls://{}", directory.to_str().unwrap())).unwrap();
        let mut config = StreamConfig::new(format(), destination);
        config.ladder = Some(ladder());
        let Err(error) = Streamer::start(config) else {
            panic!("a file where a variant directory belongs must be refused");
        };
        assert_eq!(
            error.kind,
            StartErrorKind::HlsDirectoryUnusable,
            "{error:?}"
        );
        assert_eq!(error.cleanup, CleanupStatus::Complete, "{error:?}");
        // The check runs before any child exists and nothing was replaced.
        assert_eq!(fs::read(directory.join("0")).unwrap(), b"occupied");
    }

    #[test]
    fn non_hls_output_tails_stay_byte_identical() {
        assert_eq!(
            output_args(DestinationKind::Rtmp, "rtmp://host/app/key12345", 2),
            [
                "-flvflags",
                "no_duration_filesize",
                "-f",
                "flv",
                "rtmp://host/app/key12345",
            ]
            .map(str::to_owned)
        );
        assert_eq!(
            output_args(DestinationKind::Rtmps, "rtmps://host/app/key12345", 2),
            [
                "-flvflags",
                "no_duration_filesize",
                "-f",
                "flv",
                "rtmps://host/app/key12345",
            ]
            .map(str::to_owned)
        );
        assert_eq!(
            output_args(DestinationKind::Srt, "srt://host?streamid=abcd1234", 2),
            ["-f", "mpegts", "srt://host?streamid=abcd1234"].map(str::to_owned)
        );
    }

    #[test]
    fn hls_directories_are_created_as_one_leaf_only() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("live");
        let leaf = leaf.to_str().unwrap();
        assert_eq!(ensure_hls_directory(leaf), Ok(()));
        assert!(std::path::Path::new(leaf).is_dir());
        // An existing directory is accepted unchanged.
        assert_eq!(ensure_hls_directory(leaf), Ok(()));
        // A missing parent chain is refused rather than created.
        let nested = format!("{leaf}/deeper/leaf");
        assert_eq!(
            ensure_hls_directory(&nested),
            Err(StartErrorKind::HlsDirectoryUnusable)
        );
        assert!(!std::path::Path::new(&nested).exists());
        // A regular file at the destination is refused.
        let file = root.path().join("occupied");
        fs::write(&file, b"occupied").unwrap();
        assert_eq!(
            ensure_hls_directory(file.to_str().unwrap()),
            Err(StartErrorKind::HlsDirectoryUnusable)
        );
    }
}
