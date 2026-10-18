//! Configured RTMP/RTMPS/SRT/HLS streaming destinations.
//!
//! A [`StreamTarget`] is the authored, persisted half of going live: it names
//! an ingest server or a local output directory, carries the service's stream
//! key when the protocol has one, and says which
//! [`Output`](crate::Output) supplies its video and audio. It is not a running
//! stream. Nothing here starts, stops, reconnects, or observes a sink.
//!
//! An HLS destination may also carry an authored multi-bitrate
//! [`StreamLadder`]: [`MIN_STREAM_LADDER_RUNGS`] through
//! [`MAX_STREAM_LADDER_RUNGS`] rungs, each an even width and height with a
//! video bitrate that strictly increases up the list while the dimensions
//! never shrink. The ladder is authored intent like the rest of a destination;
//! nothing here muxes or serves a rendition. RTMP/RTMPS/SRT publish one
//! rendition and refuse a ladder.
//!
//! # The stream key is a secret
//!
//! [`StreamKey`] never renders its value. `Debug` shows `StreamKey(****)`, it
//! has no `Display`, and no error type in this module carries key or endpoint
//! text, so a rejected destination cannot leak one through a message. The only
//! way to read the secret is [`StreamKey::expose_secret`], which is named to be
//! obvious in review and in `grep`. An HLS destination writes to a local
//! directory and has no key at all: [`StreamTarget::key`] returns `None`.
//!
//! The key *is* written to `project.json` in the clear. That is the existing
//! trust model of a `.freemix` bundle: the manifest is plaintext JSON on disk
//! and every other authored field is stored the same way. Anyone who can read
//! the bundle can read the key, so the bundle must be protected like a
//! credential. This module makes the weaker promise that the key does not
//! escape the bundle: not into logs, not into `Debug` output, not into an
//! error, not into a status or inventory line.
//!
//! # Validation
//!
//! The rules mirror `fm_codec_ffmpeg::StreamDestination`, which is what
//! actually opens the socket, because a destination that this crate accepts and
//! the sink then refuses is a show that fails at air time instead of at
//! authoring time. `fm-model` is a foundation crate and cannot depend on the
//! `io` layer, so the rules are restated here against the same contract:
//! `rtmp`/`rtmps`/`srt`/`hls` only, printable ASCII, no `user:password@host`
//! userinfo, a stream key of at least [`MIN_STREAM_KEY_BYTES`] for the
//! protocols that carry one, an absolute local directory for `hls`, and a
//! total URL that stays inside the sink's bound. The composed URL is
//! [`StreamTarget::expose_url`]. An HLS ladder is held to the same standard as
//! the destination that carries it: rung count, rung bitrates, even
//! dimensions inside [`MIN_STREAM_LADDER_DIMENSION`] through
//! [`MAX_STREAM_LADDER_WIDTH`] and [`MAX_STREAM_LADDER_HEIGHT`], strictly
//! increasing bitrates, and nondecreasing dimensions.

use core::{fmt, num::NonZeroU128};

use fm_types::OutputId;

use crate::StartupPolicy;

/// Longest operator-facing destination name, matching the input-name bound.
pub const MAX_STREAM_TARGET_NAME_BYTES: usize = 128;

/// Longest scheme-relative endpoint, as `host[:port]/application/path` for
/// RTMP/RTMPS, bare `host[:port]` for SRT, or an absolute local directory path
/// for HLS.
pub const MAX_STREAM_ENDPOINT_BYTES: usize = 1_024;

/// Most stream destinations one project may carry (OR-006), matching the
/// switcher's stream inventory and the shared output set.
pub const MAX_STREAM_TARGETS: usize = 5;

/// Shortest stream key that can be substituted out of captured child output
/// without mangling unrelated text. Matches the sink's minimum.
pub const MIN_STREAM_KEY_BYTES: usize = 4;

/// Longest stream key. `MAX_STREAM_ENDPOINT_BYTES` plus this plus the longest
/// scheme and the separator stays inside the sink's 2 KiB URL bound.
pub const MAX_STREAM_KEY_BYTES: usize = 512;

/// What a redacted stream key renders as, byte for byte as the sink renders it.
pub const REDACTED_STREAM_KEY: &str = "****";

/// Lowest authored video bitrate for one destination, in kilobits per second.
pub const MIN_VIDEO_BITRATE_KBPS: u32 = 1_000;

/// Highest authored video bitrate for one destination, in kilobits per second.
pub const MAX_VIDEO_BITRATE_KBPS: u32 = 100_000;

/// Video bitrate assumed when a destination is authored without naming one.
pub const DEFAULT_VIDEO_BITRATE_KBPS: u32 = 4_500;

/// Fewest rungs an authored HLS ladder may carry. One rendition is not a
/// ladder, and the group needs a lower rung to fall back from.
pub const MIN_STREAM_LADDER_RUNGS: usize = 2;

/// Most rungs an authored HLS ladder may carry.
pub const MAX_STREAM_LADDER_RUNGS: usize = 8;

/// Smallest authored ladder width or height, in pixels.
pub const MIN_STREAM_LADDER_DIMENSION: u32 = 16;

/// Widest authored ladder rendition: 8K UHD width, in pixels.
pub const MAX_STREAM_LADDER_WIDTH: u32 = 7_680;

/// Tallest authored ladder rendition: 8K UHD height, in pixels.
pub const MAX_STREAM_LADDER_HEIGHT: u32 = 4_320;

/// Stable identity of one configured streaming destination.
///
/// Domain ids normally live in `fm-types`; this one lives here because the
/// streaming destination model is the only thing that refers to it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamTargetId(NonZeroU128);

impl StreamTargetId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

impl From<NonZeroU128> for StreamTargetId {
    fn from(value: NonZeroU128) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for StreamTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The transport a destination is published over.
///
/// RTMP/RTMPS/SRT publish to a remote ingest and carry a stream key; HLS
/// writes segments under a local directory and carries none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamProtocol {
    Rtmp,
    Rtmps,
    Srt,
    Hls,
}

impl StreamProtocol {
    /// The URL scheme, without `://`.
    #[must_use]
    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Rtmps => "rtmps",
            Self::Srt => "srt",
            Self::Hls => "hls",
        }
    }
}

impl fmt::Display for StreamProtocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.scheme())
    }
}

/// Why an ingest endpoint was refused.
///
/// No variant carries endpoint text, so an error may be logged or surfaced to
/// an operator without repeating what was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamEndpointError {
    Empty,
    TooLong,
    /// The endpoint contains whitespace, control, or non-ASCII bytes.
    InvalidCharacter,
    /// Only `rtmp://`, `rtmps://`, `srt://`, and `hls://` are accepted.
    UnsupportedScheme,
    /// A scheme appeared inside the scheme-relative endpoint text.
    EmbeddedScheme,
    MissingHost,
    /// `user:password@host` credentials are refused; they cannot be redacted
    /// as reliably as a trailing stream key.
    EmbeddedCredentials,
    /// The endpoint has no `/application` path, so appending the stream key
    /// would produce a URL with no redactable final segment.
    MissingApplicationPath,
    /// An SRT endpoint carries an application path, but SRT names only a
    /// `host[:port]`; per-session identity belongs in the stream id.
    PathNotAllowed,
    /// An HLS endpoint names a directory and must start with `/`.
    PathNotAbsolute,
    /// An HLS path segment was `.` or `..`, which could escape the authored
    /// directory.
    PathTraversal,
    /// A `//` or a trailing `/` left a path segment empty, or an HLS path was
    /// only `/`.
    EmptyPathSegment,
    /// `?` and `#` belong to the stream key, which is where services put
    /// per-session tokens, not to the endpoint.
    QueryOrFragment,
}

impl fmt::Display for StreamEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "stream endpoint must not be empty",
            Self::TooLong => "stream endpoint is too long",
            Self::InvalidCharacter => "stream endpoint must be printable ASCII without whitespace",
            Self::UnsupportedScheme => {
                "stream endpoint must use rtmp://, rtmps://, srt:// or hls://"
            }
            Self::EmbeddedScheme => "stream endpoint must not repeat the URL scheme",
            Self::MissingHost => "stream endpoint must name a host",
            Self::EmbeddedCredentials => "stream endpoint must not embed user:password credentials",
            Self::MissingApplicationPath => "stream endpoint must include an application path",
            Self::PathNotAllowed => "stream endpoint must be host[:port] only without a path",
            Self::PathNotAbsolute => "stream endpoint path must be absolute",
            Self::PathTraversal => {
                "stream endpoint path must not contain `.` or `..` path segments"
            }
            Self::EmptyPathSegment => "stream endpoint must not contain an empty path segment",
            Self::QueryOrFragment => {
                "stream endpoint must not contain a query or fragment; put tokens in the key"
            }
        })
    }
}

impl std::error::Error for StreamEndpointError {}

/// One validated ingest location, stored without its scheme.
///
/// The text is `host[:port]/application[/path]` for RTMP/RTMPS, bare
/// `host[:port]` for SRT, and an absolute directory path for HLS. The scheme
/// lives on [`StreamTarget::protocol`] so that it is not stored twice and
/// cannot disagree with itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamEndpoint {
    text: String,
}

impl StreamEndpoint {
    /// Validates a scheme-relative `host[:port]/application` endpoint under
    /// the strict RTMP-style rules, which require an application path.
    ///
    /// # Errors
    ///
    /// Returns a typed [`StreamEndpointError`]. No variant carries URL text.
    pub fn parse(text: &str) -> Result<Self, StreamEndpointError> {
        Self::parse_for(StreamProtocol::Rtmp, text)
    }

    /// Validates a scheme-relative endpoint for one [`StreamProtocol`].
    ///
    /// RTMP/RTMPS accept `host[:port]/application[/path]`. SRT accepts only
    /// `host[:port]`: it has no application path, and per-session identity is
    /// carried by the stream id tail instead. HLS accepts an absolute local
    /// directory path that is exactly what follows `hls://` in a full URL: at
    /// least one non-empty segment, with no `.` or `..` segment.
    ///
    /// # Errors
    ///
    /// Returns a typed [`StreamEndpointError`]. No variant carries URL text.
    pub fn parse_for(protocol: StreamProtocol, text: &str) -> Result<Self, StreamEndpointError> {
        if text.is_empty() {
            return Err(StreamEndpointError::Empty);
        }
        if text.len() > MAX_STREAM_ENDPOINT_BYTES {
            return Err(StreamEndpointError::TooLong);
        }
        if !text.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(StreamEndpointError::InvalidCharacter);
        }
        if text.contains("://") {
            return Err(StreamEndpointError::EmbeddedScheme);
        }
        if text.contains('?') || text.contains('#') {
            return Err(StreamEndpointError::QueryOrFragment);
        }
        match protocol {
            StreamProtocol::Rtmp | StreamProtocol::Rtmps => {
                let (authority, path) = text
                    .split_once('/')
                    .ok_or(StreamEndpointError::MissingApplicationPath)?;
                if authority.is_empty() {
                    return Err(StreamEndpointError::MissingHost);
                }
                if authority.contains('@') {
                    return Err(StreamEndpointError::EmbeddedCredentials);
                }
                if path.split('/').any(str::is_empty) {
                    return Err(StreamEndpointError::EmptyPathSegment);
                }
            }
            StreamProtocol::Srt => {
                if text.contains('/') {
                    return Err(StreamEndpointError::PathNotAllowed);
                }
                if text.split(':').next().is_none_or(str::is_empty) {
                    return Err(StreamEndpointError::MissingHost);
                }
                if text.contains('@') {
                    return Err(StreamEndpointError::EmbeddedCredentials);
                }
            }
            StreamProtocol::Hls => {
                let Some(path) = text.strip_prefix('/') else {
                    return Err(StreamEndpointError::PathNotAbsolute);
                };
                if path
                    .split('/')
                    .any(|segment| segment == "." || segment == "..")
                {
                    return Err(StreamEndpointError::PathTraversal);
                }
                if path.split('/').any(str::is_empty) {
                    return Err(StreamEndpointError::EmptyPathSegment);
                }
            }
        }
        Ok(Self {
            text: text.to_owned(),
        })
    }

    /// Splits and validates a full `rtmp://host/application`,
    /// `rtmps://host/application`, `srt://host[:port]`, or
    /// `hls:///local/directory` URL.
    ///
    /// The URL must not already carry a stream key or stream id: the tail is
    /// a separate authored field so that it can be redacted independently.
    /// HLS has no tail, so what follows `hls://` is the path itself.
    ///
    /// # Errors
    ///
    /// Returns a typed [`StreamEndpointError`]. No variant carries URL text.
    pub fn parse_url(url: &str) -> Result<(StreamProtocol, Self), StreamEndpointError> {
        let (protocol, rest) = if let Some(rest) = url.strip_prefix("rtmps://") {
            (StreamProtocol::Rtmps, rest)
        } else if let Some(rest) = url.strip_prefix("rtmp://") {
            (StreamProtocol::Rtmp, rest)
        } else if let Some(rest) = url.strip_prefix("srt://") {
            (StreamProtocol::Srt, rest)
        } else if let Some(rest) = url.strip_prefix("hls://") {
            (StreamProtocol::Hls, rest)
        } else if url.is_empty() {
            return Err(StreamEndpointError::Empty);
        } else {
            return Err(StreamEndpointError::UnsupportedScheme);
        };
        Ok((protocol, Self::parse_for(protocol, rest)?))
    }

    /// The scheme-relative endpoint text: `host[:port]/application[/path]`
    /// for RTMP/RTMPS, bare `host[:port]` for SRT, and an absolute directory
    /// path for HLS.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Why a stream key was refused.
///
/// No variant carries key text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamKeyError {
    Empty,
    /// Shorter than [`MIN_STREAM_KEY_BYTES`], so the sink could not scrub it
    /// out of captured output without mangling unrelated text.
    TooShort,
    TooLong,
    /// The key contains whitespace, control, or non-ASCII bytes.
    InvalidCharacter,
    /// The key is the final URL segment and cannot contain a separator.
    PathSeparator,
}

impl fmt::Display for StreamKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "stream key must not be empty",
            Self::TooShort => "stream key is too short to be redacted safely",
            Self::TooLong => "stream key is too long",
            Self::InvalidCharacter => "stream key must be printable ASCII without whitespace",
            Self::PathSeparator => "stream key must not contain `/`",
        })
    }
}

impl std::error::Error for StreamKeyError {}

/// Why an authored video bitrate was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamVideoBitrateError {
    /// Below [`MIN_VIDEO_BITRATE_KBPS`], which no ingest accepts as a real
    /// encode target.
    TooLow,
    /// Above [`MAX_VIDEO_BITRATE_KBPS`].
    TooHigh,
}

impl fmt::Display for StreamVideoBitrateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLow => write!(
                formatter,
                "stream video bitrate must be at least {MIN_VIDEO_BITRATE_KBPS} kbps"
            ),
            Self::TooHigh => write!(
                formatter,
                "stream video bitrate must not exceed {MAX_VIDEO_BITRATE_KBPS} kbps"
            ),
        }
    }
}

impl std::error::Error for StreamVideoBitrateError {}

/// Shortest authored first reconnect delay, in milliseconds.
pub const MIN_STREAM_RECONNECT_DELAY_MS: u32 = 100;

/// Longest authored reconnect delay (first or ceiling), in milliseconds: five
/// minutes.
pub const MAX_STREAM_RECONNECT_DELAY_MS: u32 = 300_000;

/// Most reconnect attempts a destination may be authored to make before it
/// gives up. `None` (no limit) is authored separately.
pub const MAX_STREAM_RECONNECT_ATTEMPTS: u32 = 10_000;

/// How a destination reconnects after its ingest drops or refuses it
/// (OR-006, schema 49).
///
/// The first retry waits [`StreamReconnectPolicy::initial_delay_ms`]; each
/// later one doubles the wait up to [`StreamReconnectPolicy::max_delay_ms`].
/// After [`StreamReconnectPolicy::max_attempts`] consecutive attempts that
/// never delivered media the destination gives up and latches failed until an
/// operator retries, stops, or restarts it. `None` retries forever. An attempt
/// that delivers media resets the budget.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StreamReconnectPolicy {
    initial_delay_ms: u32,
    max_delay_ms: u32,
    max_attempts: Option<u32>,
}

/// Why an authored reconnect policy was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamReconnectError {
    /// A delay is below [`MIN_STREAM_RECONNECT_DELAY_MS`] or above
    /// [`MAX_STREAM_RECONNECT_DELAY_MS`].
    DelayOutOfRange,
    /// The ceiling is below the first delay.
    MaxBelowInitial,
    /// The attempt budget is zero or above [`MAX_STREAM_RECONNECT_ATTEMPTS`].
    AttemptsOutOfRange,
}

impl fmt::Display for StreamReconnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DelayOutOfRange => write!(
                formatter,
                "stream reconnect delays must be between {MIN_STREAM_RECONNECT_DELAY_MS} and \
                 {MAX_STREAM_RECONNECT_DELAY_MS} ms"
            ),
            Self::MaxBelowInitial => formatter
                .write_str("stream reconnect maximum delay must not be below the first delay"),
            Self::AttemptsOutOfRange => write!(
                formatter,
                "stream reconnect attempts must be between 1 and {MAX_STREAM_RECONNECT_ATTEMPTS}"
            ),
        }
    }
}

impl std::error::Error for StreamReconnectError {}

impl StreamReconnectPolicy {
    /// The policy a destination gets when none is authored: first retry
    /// after 250 ms, doubling to at most 2 s, giving up after 40 attempts.
    pub const DEFAULT: Self = Self {
        initial_delay_ms: 250,
        max_delay_ms: 2_000,
        max_attempts: Some(40),
    };

    /// Validates one authored policy.
    ///
    /// # Errors
    ///
    /// Returns [`StreamReconnectError`] when a delay is out of range, the
    /// ceiling is below the first delay, or the attempt budget is zero or too
    /// large.
    pub const fn new(
        initial_delay_ms: u32,
        max_delay_ms: u32,
        max_attempts: Option<u32>,
    ) -> Result<Self, StreamReconnectError> {
        if initial_delay_ms < MIN_STREAM_RECONNECT_DELAY_MS
            || initial_delay_ms > MAX_STREAM_RECONNECT_DELAY_MS
            || max_delay_ms < MIN_STREAM_RECONNECT_DELAY_MS
            || max_delay_ms > MAX_STREAM_RECONNECT_DELAY_MS
        {
            return Err(StreamReconnectError::DelayOutOfRange);
        }
        if max_delay_ms < initial_delay_ms {
            return Err(StreamReconnectError::MaxBelowInitial);
        }
        if let Some(attempts) = max_attempts
            && (attempts == 0 || attempts > MAX_STREAM_RECONNECT_ATTEMPTS)
        {
            return Err(StreamReconnectError::AttemptsOutOfRange);
        }
        Ok(Self {
            initial_delay_ms,
            max_delay_ms,
            max_attempts,
        })
    }

    /// The wait before the first reconnect attempt, in milliseconds.
    #[must_use]
    pub const fn initial_delay_ms(self) -> u32 {
        self.initial_delay_ms
    }

    /// The longest wait between attempts, in milliseconds.
    #[must_use]
    pub const fn max_delay_ms(self) -> u32 {
        self.max_delay_ms
    }

    /// How many consecutive undelivered attempts before giving up; `None`
    /// retries forever.
    #[must_use]
    pub const fn max_attempts(self) -> Option<u32> {
        self.max_attempts
    }
}

impl Default for StreamReconnectPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why an authored HLS ladder was refused.
///
/// No variant carries a rejected value, so an error may be logged or surfaced
/// to an operator without repeating what was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamLadderError {
    /// Fewer than [`MIN_STREAM_LADDER_RUNGS`] rungs.
    TooFewRungs,
    /// More than [`MAX_STREAM_LADDER_RUNGS`] rungs.
    TooManyRungs,
    /// A rung's video bitrate is below [`MIN_VIDEO_BITRATE_KBPS`].
    BitrateTooLow,
    /// A rung's video bitrate is above [`MAX_VIDEO_BITRATE_KBPS`].
    BitrateTooHigh,
    /// A rung's width or height is below [`MIN_STREAM_LADDER_DIMENSION`].
    DimensionTooSmall,
    /// A rung's width is above [`MAX_STREAM_LADDER_WIDTH`] or its height is
    /// above [`MAX_STREAM_LADDER_HEIGHT`].
    DimensionTooLarge,
    /// A rung's width or height is not even, so no chroma layout divides it.
    OddDimension,
    /// A rung's video bitrate does not exceed the previous rung's.
    BitratesNotIncreasing,
    /// A rung's width or height is smaller than the previous rung's.
    DimensionsDecrease,
}

impl fmt::Display for StreamLadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewRungs => write!(
                formatter,
                "stream ladder must have at least {MIN_STREAM_LADDER_RUNGS} rungs"
            ),
            Self::TooManyRungs => write!(
                formatter,
                "stream ladder must not have more than {MAX_STREAM_LADDER_RUNGS} rungs"
            ),
            Self::BitrateTooLow => write!(
                formatter,
                "stream ladder rung video bitrate must be at least {MIN_VIDEO_BITRATE_KBPS} kbps"
            ),
            Self::BitrateTooHigh => write!(
                formatter,
                "stream ladder rung video bitrate must not exceed {MAX_VIDEO_BITRATE_KBPS} kbps"
            ),
            Self::DimensionTooSmall => write!(
                formatter,
                "stream ladder rung width and height must be at least {MIN_STREAM_LADDER_DIMENSION}"
            ),
            Self::DimensionTooLarge => write!(
                formatter,
                "stream ladder rung must not exceed {MAX_STREAM_LADDER_WIDTH} by \
                 {MAX_STREAM_LADDER_HEIGHT}"
            ),
            Self::OddDimension => {
                formatter.write_str("stream ladder rung width and height must be even")
            }
            Self::BitratesNotIncreasing => {
                formatter.write_str("stream ladder rung video bitrates must strictly increase")
            }
            Self::DimensionsDecrease => {
                formatter.write_str("stream ladder rung width and height must not decrease")
            }
        }
    }
}

impl std::error::Error for StreamLadderError {}

/// One authored ABR rendition inside a [`StreamLadder`].
///
/// The dimensions are the rendition's own encoded size, not the program's, and
/// the bitrate is that rendition's video bitrate in kilobits per second.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLadderRung {
    video_bitrate_kbps: u32,
    width: u32,
    height: u32,
}

impl StreamLadderRung {
    /// Validates one rung's video bitrate and dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`StreamLadderError::BitrateTooLow`] or
    /// [`StreamLadderError::BitrateTooHigh`] when the bitrate falls outside
    /// [`MIN_VIDEO_BITRATE_KBPS`]..=[`MAX_VIDEO_BITRATE_KBPS`],
    /// [`StreamLadderError::DimensionTooSmall`] when width or height is below
    /// [`MIN_STREAM_LADDER_DIMENSION`],
    /// [`StreamLadderError::DimensionTooLarge`] when width exceeds
    /// [`MAX_STREAM_LADDER_WIDTH`] or height exceeds
    /// [`MAX_STREAM_LADDER_HEIGHT`], and [`StreamLadderError::OddDimension`]
    /// when either dimension is not even.
    pub fn new(
        video_bitrate_kbps: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, StreamLadderError> {
        validate_video_bitrate(video_bitrate_kbps).map_err(|error| match error {
            StreamVideoBitrateError::TooLow => StreamLadderError::BitrateTooLow,
            StreamVideoBitrateError::TooHigh => StreamLadderError::BitrateTooHigh,
        })?;
        if width < MIN_STREAM_LADDER_DIMENSION || height < MIN_STREAM_LADDER_DIMENSION {
            return Err(StreamLadderError::DimensionTooSmall);
        }
        if width > MAX_STREAM_LADDER_WIDTH || height > MAX_STREAM_LADDER_HEIGHT {
            return Err(StreamLadderError::DimensionTooLarge);
        }
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(StreamLadderError::OddDimension);
        }
        Ok(Self {
            video_bitrate_kbps,
            width,
            height,
        })
    }

    /// The rendition's video bitrate, in kilobits per second.
    #[must_use]
    pub const fn video_bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps
    }

    /// The rendition's width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The rendition's height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// Renders dimensions and bitrate as `{width}x{height}@{video_bitrate_kbps}`,
/// for example `640x360@800`.
impl fmt::Display for StreamLadderRung {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}x{}@{}",
            self.width, self.height, self.video_bitrate_kbps
        )
    }
}

/// One authored multi-bitrate HLS ladder, ordered lowest rendition first.
///
/// The list order is the contract: video bitrates strictly increase from the
/// first rung to the last and dimensions never shrink, so a sink can walk the
/// rungs from the lowest fallback to the top rendition. A ladder is authored
/// intent and only HLS carries one; see [`StreamTarget::with_ladder`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamLadder {
    rungs: Vec<StreamLadderRung>,
}

impl StreamLadder {
    /// Validates rung count and list order.
    ///
    /// Each rung is already validated by [`StreamLadderRung::new`]; this checks
    /// what only the list can: a count between [`MIN_STREAM_LADDER_RUNGS`] and
    /// [`MAX_STREAM_LADDER_RUNGS`], strictly increasing video bitrates, and
    /// nondecreasing width and height.
    ///
    /// # Errors
    ///
    /// Returns [`StreamLadderError::TooFewRungs`] or
    /// [`StreamLadderError::TooManyRungs`] for a count outside the bound,
    /// [`StreamLadderError::BitratesNotIncreasing`] when a rung's video
    /// bitrate does not exceed the previous rung's, and
    /// [`StreamLadderError::DimensionsDecrease`] when either dimension shrinks
    /// from one rung to the next.
    pub fn new(rungs: Vec<StreamLadderRung>) -> Result<Self, StreamLadderError> {
        if rungs.len() < MIN_STREAM_LADDER_RUNGS {
            return Err(StreamLadderError::TooFewRungs);
        }
        if rungs.len() > MAX_STREAM_LADDER_RUNGS {
            return Err(StreamLadderError::TooManyRungs);
        }
        for (lower, higher) in rungs.iter().zip(rungs.iter().skip(1)) {
            if higher.video_bitrate_kbps <= lower.video_bitrate_kbps {
                return Err(StreamLadderError::BitratesNotIncreasing);
            }
            if higher.width < lower.width || higher.height < lower.height {
                return Err(StreamLadderError::DimensionsDecrease);
            }
        }
        Ok(Self { rungs })
    }

    /// The rungs, from the lowest rendition to the top one.
    #[must_use]
    pub fn rungs(&self) -> &[StreamLadderRung] {
        &self.rungs
    }

    /// The top rung's video bitrate, in kilobits per second.
    ///
    /// This is the destination's primary/grouping bitrate: the value a sink
    /// advertises and groups the ladder's renditions under.
    ///
    /// # Panics
    ///
    /// Panics if the ladder has no rungs; unreachable because
    /// [`StreamLadder::new`] refuses fewer than [`MIN_STREAM_LADDER_RUNGS`]
    /// rungs and the rungs cannot be mutated afterwards.
    #[must_use]
    pub fn top_bitrate_kbps(&self) -> u32 {
        self.rungs
            .last()
            .expect("a validated ladder always has at least one rung")
            .video_bitrate_kbps
    }
}

/// Renders the rungs comma-joined from lowest to highest, for example
/// `640x360@800,1280x720@1500`.
impl fmt::Display for StreamLadder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rungs = self.rungs.iter();
        if let Some(rung) = rungs.next() {
            write!(formatter, "{rung}")?;
        }
        for rung in rungs {
            write!(formatter, ",{rung}")?;
        }
        Ok(())
    }
}

/// One validated stream key.
///
/// The value is never rendered. There is no `Display`, `Debug` prints
/// `StreamKey(****)`, and reading the secret requires the deliberately
/// conspicuous [`StreamKey::expose_secret`].
#[derive(Clone, Eq, PartialEq)]
pub struct StreamKey {
    secret: String,
}

impl StreamKey {
    /// Validates length and character set.
    ///
    /// # Errors
    ///
    /// Returns a typed [`StreamKeyError`]. No variant carries key text.
    pub fn parse(secret: &str) -> Result<Self, StreamKeyError> {
        if secret.is_empty() {
            return Err(StreamKeyError::Empty);
        }
        if !secret.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(StreamKeyError::InvalidCharacter);
        }
        if secret.contains('/') {
            return Err(StreamKeyError::PathSeparator);
        }
        if secret.len() < MIN_STREAM_KEY_BYTES {
            return Err(StreamKeyError::TooShort);
        }
        if secret.len() > MAX_STREAM_KEY_BYTES {
            return Err(StreamKeyError::TooLong);
        }
        Ok(Self {
            secret: secret.to_owned(),
        })
    }

    /// Returns the secret in the clear.
    ///
    /// Call this only to compose the URL handed to a sink or to write the
    /// bundle manifest. It must not reach a log, a status line, or an error.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for StreamKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StreamKey")
            .field(&REDACTED_STREAM_KEY)
            .finish()
    }
}

/// Lowest authored SRT send latency, in milliseconds (schema 51).
pub const MIN_STREAM_SRT_LATENCY_MS: u32 = fm_types::network_stream::MIN_SRT_LATENCY_MS;

/// Highest authored SRT send latency, in milliseconds (schema 51).
pub const MAX_STREAM_SRT_LATENCY_MS: u32 = fm_types::network_stream::MAX_SRT_LATENCY_MS;

/// SRT latency an SRT destination gets when none is authored: libsrt's own
/// default of 120 ms.
pub const DEFAULT_STREAM_SRT_LATENCY_MS: u32 = fm_types::network_stream::DEFAULT_SRT_LATENCY_MS;

/// Shortest SRT passphrase, in bytes, as libsrt requires.
pub const MIN_STREAM_SRT_PASSPHRASE_BYTES: usize =
    fm_types::network_stream::MIN_SRT_PASSPHRASE_BYTES;

/// Longest SRT passphrase, in bytes, as libsrt requires.
pub const MAX_STREAM_SRT_PASSPHRASE_BYTES: usize =
    fm_types::network_stream::MAX_SRT_PASSPHRASE_BYTES;

/// Why an SRT passphrase was refused. No variant carries passphrase text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrtPassphraseError {
    /// Shorter than [`MIN_STREAM_SRT_PASSPHRASE_BYTES`] or longer than
    /// [`MAX_STREAM_SRT_PASSPHRASE_BYTES`].
    Length,
    /// Not printable ASCII without whitespace.
    InvalidCharacter,
}

impl fmt::Display for SrtPassphraseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length => write!(
                formatter,
                "SRT passphrase must be {MIN_STREAM_SRT_PASSPHRASE_BYTES} to \
                 {MAX_STREAM_SRT_PASSPHRASE_BYTES} bytes"
            ),
            Self::InvalidCharacter => {
                formatter.write_str("SRT passphrase must be printable ASCII without whitespace")
            }
        }
    }
}

impl std::error::Error for SrtPassphraseError {}

/// One validated SRT encryption passphrase (schema 51).
///
/// A secret exactly like [`StreamKey`]: no `Display`, `Debug` prints
/// `SrtPassphrase("****")`, and only [`SrtPassphrase::expose_secret`] reads it.
#[derive(Clone, Eq, PartialEq)]
pub struct SrtPassphrase {
    secret: String,
}

impl SrtPassphrase {
    /// Validates length and character set.
    ///
    /// # Errors
    ///
    /// Returns a typed [`SrtPassphraseError`]. No variant carries the text.
    pub fn parse(secret: &str) -> Result<Self, SrtPassphraseError> {
        if !(MIN_STREAM_SRT_PASSPHRASE_BYTES..=MAX_STREAM_SRT_PASSPHRASE_BYTES)
            .contains(&secret.len())
        {
            return Err(SrtPassphraseError::Length);
        }
        if !secret.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(SrtPassphraseError::InvalidCharacter);
        }
        Ok(Self {
            secret: secret.to_owned(),
        })
    }

    /// Returns the secret in the clear.
    ///
    /// Call this only to hand the passphrase to a sink or to write the bundle
    /// manifest. It must not reach a log, a status line, or an error.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for SrtPassphrase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SrtPassphrase")
            .field(&REDACTED_STREAM_KEY)
            .finish()
    }
}

/// Why a streaming destination was refused at construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamTargetError {
    EmptyName,
    NameTooLong,
    /// RTMP/RTMPS/SRT carry per-session identity and require a stream key.
    MissingStreamKey,
    /// HLS writes to a local directory and must not carry a stream key.
    UnexpectedStreamKey,
    /// HLS has no failover endpoint; local segmenting cannot use one.
    BackupNotAllowed,
    /// RTMP/RTMPS/SRT publish one rendition, so a multi-bitrate ladder has no
    /// meaning there.
    LadderNotAllowed,
    /// Scheme, endpoint, key and separators together exceed what the sink
    /// accepts as one URL.
    UrlTooLong,
    /// An SRT key travels as the single `?streamid=` query value, so `&` or
    /// `?` inside it would start another parameter or query and the URL could
    /// never be opened.
    InvalidSrtStreamId,
    /// The endpoint does not satisfy the destination protocol's endpoint
    /// rules, for example an RTMP `host/application` endpoint on SRT or HLS.
    EndpointProtocolMismatch,
    /// An SRT latency or passphrase on a protocol other than SRT.
    SrtOptionsNotAllowed,
    /// An SRT latency outside [`MIN_STREAM_SRT_LATENCY_MS`] through
    /// [`MAX_STREAM_SRT_LATENCY_MS`].
    SrtLatencyOutOfRange,
}

impl fmt::Display for StreamTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("stream destination name must not be empty"),
            Self::NameTooLong => write!(
                formatter,
                "stream destination name must not exceed {MAX_STREAM_TARGET_NAME_BYTES} bytes"
            ),
            Self::MissingStreamKey => {
                formatter.write_str("stream destination protocol requires a stream key")
            }
            Self::UnexpectedStreamKey => {
                formatter.write_str("stream destination protocol does not accept a stream key")
            }
            Self::BackupNotAllowed => formatter
                .write_str("stream destination protocol does not support a backup endpoint"),
            Self::LadderNotAllowed => formatter
                .write_str("stream destination protocol does not support a multi-bitrate ladder"),
            Self::UrlTooLong => write!(
                formatter,
                "stream destination URL must not exceed {} bytes",
                StreamTarget::MAX_URL_BYTES
            ),
            Self::InvalidSrtStreamId => {
                formatter.write_str("SRT stream id must not contain `&` or `?` query separators")
            }
            Self::EndpointProtocolMismatch => formatter
                .write_str("stream endpoint does not match the stream destination protocol"),
            Self::SrtOptionsNotAllowed => formatter
                .write_str("only an SRT stream destination carries a latency or passphrase"),
            Self::SrtLatencyOutOfRange => write!(
                formatter,
                "SRT latency must be {MIN_STREAM_SRT_LATENCY_MS} to {MAX_STREAM_SRT_LATENCY_MS} ms"
            ),
        }
    }
}

impl std::error::Error for StreamTargetError {}

/// One configured streaming destination.
///
/// Every field is validated at construction and the id is stable across
/// updates, so a destination can be re-authored without a downstream reference
/// to it changing meaning. See the [module docs](self) for how the stream key
/// is handled.
#[derive(Clone, Eq, PartialEq)]
pub struct StreamTarget {
    id: StreamTargetId,
    name: String,
    protocol: StreamProtocol,
    endpoint: StreamEndpoint,
    backup_endpoint: Option<StreamEndpoint>,
    key: Option<StreamKey>,
    startup: StartupPolicy,
    running: bool,
    video_bitrate_kbps: u32,
    ladder: Option<StreamLadder>,
    reconnect: StreamReconnectPolicy,
    /// SRT send latency in milliseconds; `Some` exactly for SRT.
    srt_latency_ms: Option<u32>,
    /// SRT encryption passphrase; only SRT may carry one.
    srt_passphrase: Option<SrtPassphrase>,
    output: OutputId,
}

impl StreamTarget {
    /// The sink's bound on a whole destination URL.
    pub const MAX_URL_BYTES: usize = 2 * 1_024;

    /// Builds a destination that starts [`StartupPolicy::Stopped`] with no
    /// backup endpoint.
    ///
    /// `key` is `Some` for RTMP/RTMPS/SRT, which carry per-session identity,
    /// and `None` for HLS, which writes to a local directory with no secret.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError`] when the name is blank or oversized, the
    /// protocol/key pairing is wrong, an SRT key contains a `&` or `?` query
    /// separator, the endpoint fails the protocol's endpoint rules, or the
    /// composed URL would exceed what the sink accepts.
    pub fn new(
        id: StreamTargetId,
        name: String,
        protocol: StreamProtocol,
        endpoint: StreamEndpoint,
        key: Option<StreamKey>,
        output: OutputId,
    ) -> Result<Self, StreamTargetError> {
        if name.trim().is_empty() {
            return Err(StreamTargetError::EmptyName);
        }
        if name.len() > MAX_STREAM_TARGET_NAME_BYTES {
            return Err(StreamTargetError::NameTooLong);
        }
        if protocol == StreamProtocol::Hls && key.is_some() {
            return Err(StreamTargetError::UnexpectedStreamKey);
        }
        if protocol != StreamProtocol::Hls && key.is_none() {
            return Err(StreamTargetError::MissingStreamKey);
        }
        if protocol == StreamProtocol::Srt
            && key
                .as_ref()
                .is_some_and(|key| key.expose_secret().contains(['&', '?']))
        {
            return Err(StreamTargetError::InvalidSrtStreamId);
        }
        check_endpoint_protocol(protocol, &endpoint)?;
        if url_bytes(protocol, &endpoint, key.as_ref()) > Self::MAX_URL_BYTES {
            return Err(StreamTargetError::UrlTooLong);
        }
        Ok(Self {
            id,
            name,
            protocol,
            endpoint,
            backup_endpoint: None,
            key,
            startup: StartupPolicy::Stopped,
            running: false,
            video_bitrate_kbps: DEFAULT_VIDEO_BITRATE_KBPS,
            ladder: None,
            reconnect: StreamReconnectPolicy::DEFAULT,
            srt_latency_ms: (protocol == StreamProtocol::Srt)
                .then_some(DEFAULT_STREAM_SRT_LATENCY_MS),
            srt_passphrase: None,
            output,
        })
    }

    /// Builder-style setter for the SRT send latency, in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError::SrtOptionsNotAllowed`] on a protocol other
    /// than SRT and [`StreamTargetError::SrtLatencyOutOfRange`] outside
    /// [`MIN_STREAM_SRT_LATENCY_MS`]..=[`MAX_STREAM_SRT_LATENCY_MS`].
    pub fn with_srt_latency_ms(mut self, latency_ms: u32) -> Result<Self, StreamTargetError> {
        if self.protocol != StreamProtocol::Srt {
            return Err(StreamTargetError::SrtOptionsNotAllowed);
        }
        if !(MIN_STREAM_SRT_LATENCY_MS..=MAX_STREAM_SRT_LATENCY_MS).contains(&latency_ms) {
            return Err(StreamTargetError::SrtLatencyOutOfRange);
        }
        self.srt_latency_ms = Some(latency_ms);
        Ok(self)
    }

    /// Attaches or clears the SRT encryption passphrase.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError::SrtOptionsNotAllowed`] when a passphrase
    /// is set on a protocol other than SRT.
    pub fn with_srt_passphrase(
        mut self,
        passphrase: Option<SrtPassphrase>,
    ) -> Result<Self, StreamTargetError> {
        if passphrase.is_some() && self.protocol != StreamProtocol::Srt {
            return Err(StreamTargetError::SrtOptionsNotAllowed);
        }
        self.srt_passphrase = passphrase;
        Ok(self)
    }

    /// Attaches or clears the failover endpoint, which uses the same protocol
    /// and the same key as the primary.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError::BackupNotAllowed`] for HLS, which has no
    /// failover endpoint, [`StreamTargetError::EndpointProtocolMismatch`] when
    /// the endpoint fails the protocol's endpoint rules, and
    /// [`StreamTargetError::UrlTooLong`] when the
    /// composed backup URL would exceed what the sink accepts.
    pub fn with_backup_endpoint(
        mut self,
        backup_endpoint: Option<StreamEndpoint>,
    ) -> Result<Self, StreamTargetError> {
        if let Some(endpoint) = &backup_endpoint {
            if self.protocol == StreamProtocol::Hls {
                return Err(StreamTargetError::BackupNotAllowed);
            }
            check_endpoint_protocol(self.protocol, endpoint)?;
            if url_bytes(self.protocol, endpoint, self.key.as_ref()) > Self::MAX_URL_BYTES {
                return Err(StreamTargetError::UrlTooLong);
            }
        }
        self.backup_endpoint = backup_endpoint;
        Ok(self)
    }

    /// Attaches or clears the authored multi-bitrate ladder.
    ///
    /// Only HLS publishes more than one rendition, so `Some` on any other
    /// protocol is refused. Setting a ladder also sets
    /// [`StreamTarget::video_bitrate_kbps`] to [`StreamLadder::top_bitrate_kbps`],
    /// the top rung's bitrate: that is the destination's primary/grouping
    /// bitrate, the value a sink advertises and groups the renditions under.
    /// Passing `None` clears the ladder and leaves the base bitrate untouched.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError::LadderNotAllowed`] when a ladder is set on
    /// a protocol other than `hls`.
    pub fn with_ladder(mut self, ladder: Option<StreamLadder>) -> Result<Self, StreamTargetError> {
        if let Some(ladder) = ladder {
            if self.protocol != StreamProtocol::Hls {
                return Err(StreamTargetError::LadderNotAllowed);
            }
            self.video_bitrate_kbps = ladder.top_bitrate_kbps();
            self.ladder = Some(ladder);
        } else {
            self.ladder = None;
        }
        Ok(self)
    }

    /// Builder-style setter for the authored reconnect policy.
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: StreamReconnectPolicy) -> Self {
        self.reconnect = reconnect;
        self
    }

    #[must_use]
    pub fn with_startup(mut self, startup: StartupPolicy) -> Self {
        self.startup = startup;
        self
    }

    /// Builder-style setter for the desired running state.
    #[must_use]
    pub fn set_running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// Builder-style setter for the authored video bitrate, in kilobits per
    /// second.
    ///
    /// # Errors
    ///
    /// Returns [`StreamVideoBitrateError`] when the bitrate falls outside
    /// [`MIN_VIDEO_BITRATE_KBPS`]..=[`MAX_VIDEO_BITRATE_KBPS`].
    pub fn with_video_bitrate(
        mut self,
        video_bitrate_kbps: u32,
    ) -> Result<Self, StreamVideoBitrateError> {
        validate_video_bitrate(video_bitrate_kbps)?;
        self.video_bitrate_kbps = video_bitrate_kbps;
        Ok(self)
    }

    #[must_use]
    pub const fn id(&self) -> StreamTargetId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn protocol(&self) -> StreamProtocol {
        self.protocol
    }

    #[must_use]
    pub const fn endpoint(&self) -> &StreamEndpoint {
        &self.endpoint
    }

    #[must_use]
    pub const fn backup_endpoint(&self) -> Option<&StreamEndpoint> {
        self.backup_endpoint.as_ref()
    }

    /// The stream key, when the protocol carries one. Reading its value still
    /// requires [`StreamKey::expose_secret`].
    #[must_use]
    pub const fn key(&self) -> Option<&StreamKey> {
        self.key.as_ref()
    }

    #[must_use]
    pub const fn startup(&self) -> StartupPolicy {
        self.startup
    }

    /// Whether the destination should be running once the engine reconciles
    /// desired state. This is authored intent, not a live stream status.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// The authored video bitrate, in kilobits per second.
    ///
    /// When the destination carries a ladder this is the top rung's bitrate,
    /// the primary/grouping value the sink advertises.
    #[must_use]
    pub const fn video_bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps
    }

    /// The authored multi-bitrate ladder, when this HLS destination has one.
    #[must_use]
    pub const fn ladder(&self) -> Option<&StreamLadder> {
        self.ladder.as_ref()
    }

    /// How this destination reconnects after its ingest drops it.
    #[must_use]
    pub const fn reconnect(&self) -> StreamReconnectPolicy {
        self.reconnect
    }

    /// The SRT send latency in milliseconds, `Some` exactly for SRT.
    #[must_use]
    pub const fn srt_latency_ms(&self) -> Option<u32> {
        self.srt_latency_ms
    }

    /// The SRT encryption passphrase, when one is stored. Reading its value
    /// still requires [`SrtPassphrase::expose_secret`].
    #[must_use]
    pub const fn srt_passphrase(&self) -> Option<&SrtPassphrase> {
        self.srt_passphrase.as_ref()
    }

    /// The output this destination takes video and audio from.
    #[must_use]
    pub const fn output(&self) -> OutputId {
        self.output
    }

    /// Renames in place, preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`StreamTargetError`] when the name is blank or oversized.
    pub fn rename(&mut self, name: String) -> Result<(), StreamTargetError> {
        if name.trim().is_empty() {
            return Err(StreamTargetError::EmptyName);
        }
        if name.len() > MAX_STREAM_TARGET_NAME_BYTES {
            return Err(StreamTargetError::NameTooLong);
        }
        self.name = name;
        Ok(())
    }

    /// The full destination URL, stream key included.
    ///
    /// This is what a sink is handed. It must not reach a log or a status
    /// line; use [`StreamTarget::redacted_url`] for anything an operator or a
    /// file might see. HLS composes its local output directory and has no key
    /// segment.
    #[must_use]
    pub fn expose_url(&self) -> String {
        compose_url(
            self.protocol,
            &self.endpoint,
            self.key.as_ref().map(StreamKey::expose_secret),
        )
    }

    /// The full failover URL, stream key included, when one is configured.
    #[must_use]
    pub fn expose_backup_url(&self) -> Option<String> {
        self.backup_endpoint.as_ref().map(|endpoint| {
            compose_url(
                self.protocol,
                endpoint,
                self.key.as_ref().map(StreamKey::expose_secret),
            )
        })
    }

    /// The destination URL with the stream key replaced by `****`.
    ///
    /// HLS has no key, so its URL is the same as [`StreamTarget::expose_url`].
    #[must_use]
    pub fn redacted_url(&self) -> String {
        compose_url(
            self.protocol,
            &self.endpoint,
            self.key.as_ref().map(|_| REDACTED_STREAM_KEY),
        )
    }

    /// The failover URL with the stream key replaced by `****`.
    #[must_use]
    pub fn redacted_backup_url(&self) -> Option<String> {
        self.backup_endpoint.as_ref().map(|endpoint| {
            compose_url(
                self.protocol,
                endpoint,
                self.key.as_ref().map(|_| REDACTED_STREAM_KEY),
            )
        })
    }
}

/// Renders the redacted URLs instead of the raw protocol, endpoint and key.
///
/// The omitted fields are exactly the ones the redacted URLs already
/// summarise, and one of them is the secret, so this impl is deliberately not
/// exhaustive. The authored ladder is rendered verbatim: it holds no secret.
impl fmt::Debug for StreamTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamTarget")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.redacted_url())
            .field("backup_url", &self.redacted_backup_url())
            .field("ladder", &self.ladder)
            .field("startup", &self.startup)
            .field("reconnect", &self.reconnect)
            .field("srt_latency_ms", &self.srt_latency_ms)
            .field("srt_passphrase", &self.srt_passphrase)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for StreamTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted_url())
    }
}

/// Renders one destination URL from its parts.
///
/// `tail` is the stream key or its redacted placeholder for RTMP/RTMPS/SRT,
/// and `None` for HLS, whose local output path carries no secret. The
/// constructor pairs each protocol with exactly one shape and no setter can
/// change either half, so a mismatch is unreachable.
fn compose_url(protocol: StreamProtocol, endpoint: &StreamEndpoint, tail: Option<&str>) -> String {
    match (protocol, tail) {
        (StreamProtocol::Rtmp | StreamProtocol::Rtmps, Some(tail)) => {
            format!("{}://{}/{tail}", protocol.scheme(), endpoint.as_str())
        }
        (StreamProtocol::Srt, Some(tail)) => {
            format!(
                "{}://{}?streamid={tail}",
                protocol.scheme(),
                endpoint.as_str()
            )
        }
        (StreamProtocol::Hls, None) => format!("{}://{}", protocol.scheme(), endpoint.as_str()),
        _ => unreachable!("stream protocol and stream key presence are paired at construction"),
    }
}

/// A [`StreamEndpoint`] does not record which protocol's rules it was
/// validated under, so a destination re-checks it against its own protocol.
fn check_endpoint_protocol(
    protocol: StreamProtocol,
    endpoint: &StreamEndpoint,
) -> Result<(), StreamTargetError> {
    StreamEndpoint::parse_for(protocol, endpoint.as_str())
        .map(drop)
        .map_err(|_| StreamTargetError::EndpointProtocolMismatch)
}

fn url_bytes(
    protocol: StreamProtocol,
    endpoint: &StreamEndpoint,
    key: Option<&StreamKey>,
) -> usize {
    let separator = match protocol {
        StreamProtocol::Rtmp | StreamProtocol::Rtmps => 1,
        StreamProtocol::Srt => "?streamid=".len(),
        StreamProtocol::Hls => 0,
    };
    protocol.scheme().len()
        + "://".len()
        + endpoint.as_str().len()
        + separator
        + key.map_or(0, |key| key.expose_secret().len())
}

fn validate_video_bitrate(video_bitrate_kbps: u32) -> Result<(), StreamVideoBitrateError> {
    if video_bitrate_kbps < MIN_VIDEO_BITRATE_KBPS {
        Err(StreamVideoBitrateError::TooLow)
    } else if video_bitrate_kbps > MAX_VIDEO_BITRATE_KBPS {
        Err(StreamVideoBitrateError::TooHigh)
    } else {
        Ok(())
    }
}
