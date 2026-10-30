//! Network stream input value types (IN-015, IN-016, IN-017).
//!
//! A network stream input receives one RTSP, RTP, UDP, TCP, or HTTP(S)
//! MPEG-TS stream, an HLS playlist, an RTMP(S) stream (IN-016), or an SRT
//! stream (IN-017), by URL. RTMP and SRT inputs connect as a caller (pull)
//! or listen for a publisher on a local port ([`StreamMode`]); SRT also
//! meets its peer in rendezvous mode, and carries an optional AES
//! passphrase, stream ID, and receive latency. Its
//! [`NetworkStreamSettings`] bound the receive jitter buffer, the reconnect
//! policy (exponential backoff between a minimum and maximum delay, and
//! whether to give up after a number of attempts), the no-data timeout, and
//! what the input shows while the stream stalls.
//!
//! A URL may carry credentials (`rtsp://user:password@host/path`), secret
//! query parameters (`?token=...`), or an RTMP stream key
//! (`rtmp://host/app/<key>`). [`redact_stream_url`] masks all three, and
//! everything that reports a URL to an operator (snapshots, events, logs,
//! status text) reports only its redacted form. The separate RTMP stream key
//! and SRT passphrase are secrets too: a report carries [`REDACTED`] in
//! their place ([`NetworkStreamConfig::redacted`]).
//!
//! The runtime side reports each stream's [`NetworkStreamStatus`]: its
//! [`NetworkStreamState`] (connecting, buffering, live, stalled,
//! reconnecting with the attempt and the delay before it, or failed), the
//! received bitrate, jitter-buffer fill, dropped and late frames, reconnect
//! count, and the last (redacted) error.

use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

/// The longest stream URL, in bytes. URLs are ASCII.
pub const MAX_STREAM_URL_BYTES: usize = 512;
/// The most network stream inputs one project holds.
pub const MAX_NETWORK_STREAM_INPUTS: usize = 16;
/// The shortest jitter buffer, in milliseconds.
pub const MIN_JITTER_BUFFER_MS: u32 = 40;
/// The longest jitter buffer, in milliseconds.
pub const MAX_JITTER_BUFFER_MS: u32 = 5_000;
/// The default jitter buffer, in milliseconds.
pub const DEFAULT_JITTER_BUFFER_MS: u32 = 300;
/// The shortest reconnect delay, in milliseconds.
pub const MIN_RECONNECT_BACKOFF_MS: u32 = 100;
/// The longest reconnect delay, in milliseconds.
pub const MAX_RECONNECT_BACKOFF_MS: u32 = 60_000;
/// The most reconnect attempts a give-up policy may allow.
pub const MAX_RECONNECT_ATTEMPTS: u32 = 10_000;
/// The shortest no-data timeout, in milliseconds.
pub const MIN_STREAM_TIMEOUT_MS: u32 = 500;
/// The longest no-data timeout, in milliseconds.
pub const MAX_STREAM_TIMEOUT_MS: u32 = 60_000;
/// The longest reported error text, in bytes.
pub const MAX_STREAM_ERROR_BYTES: usize = 160;
/// The shortest SRT receive latency, in milliseconds.
pub const MIN_SRT_LATENCY_MS: u32 = 20;
/// The longest SRT receive latency, in milliseconds.
pub const MAX_SRT_LATENCY_MS: u32 = 8_000;
/// The default SRT receive latency, in milliseconds (libsrt's default).
pub const DEFAULT_SRT_LATENCY_MS: u32 = 120;
/// The shortest SRT passphrase, in bytes.
pub const MIN_SRT_PASSPHRASE_BYTES: usize = 10;
/// The longest SRT passphrase, in bytes.
pub const MAX_SRT_PASSPHRASE_BYTES: usize = 79;
/// The longest SRT stream ID, in bytes.
pub const MAX_SRT_STREAM_ID_BYTES: usize = 512;
/// The longest RTMP stream key, in bytes.
pub const MAX_STREAM_KEY_BYTES: usize = 256;
/// The lowest port a listening input may bind (no privileged ports).
pub const MIN_LISTEN_PORT: u16 = 1024;
/// The port an `rtmp://` URL without one names.
pub const DEFAULT_RTMP_PORT: u16 = 1935;
/// The most source audio channels a stream input distinguishes.
pub const MAX_STREAM_AUDIO_CHANNELS: u16 = 8;

/// What masks a secret in a redacted URL.
pub const REDACTED: &str = "***";

/// Query parameter names whose values are secrets.
const SECRET_QUERY_KEYS: [&str; 10] = [
    "password",
    "passwd",
    "pass",
    "pwd",
    "passphrase",
    "token",
    "key",
    "secret",
    "auth",
    "signature",
];

/// The transport protocol a stream URL names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamProtocol {
    /// `rtsp://` or `rtsps://`.
    Rtsp,
    /// `rtp://`.
    Rtp,
    /// `udp://` (MPEG-TS, unicast or multicast).
    Udp,
    /// `tcp://` (MPEG-TS).
    Tcp,
    /// `http://` or `https://` (MPEG-TS or another progressive stream).
    Http,
    /// An `http://` or `https://` URL whose path ends in `.m3u8`.
    Hls,
    /// `rtmp://` or `rtmps://` (IN-016).
    Rtmp,
    /// `srt://` (IN-017).
    Srt,
    /// `omt://host:port`: an Open Media Transport sender (IN-019).
    Omt,
}

impl StreamProtocol {
    /// Every protocol, in token order.
    pub const ALL: [Self; 9] = [
        Self::Rtsp,
        Self::Rtp,
        Self::Udp,
        Self::Tcp,
        Self::Http,
        Self::Hls,
        Self::Rtmp,
        Self::Srt,
        Self::Omt,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Rtsp => "rtsp",
            Self::Rtp => "rtp",
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Http => "http",
            Self::Hls => "hls",
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
            Self::Omt => "omt",
        }
    }

    /// Whether a stalled source of this protocol can be re-read from where
    /// it stopped at more than real time (a playlist or a progressive HTTP
    /// body), so the receiver paces it to its own timestamps.
    #[must_use]
    pub const fn is_paced(self) -> bool {
        matches!(self, Self::Http | Self::Hls)
    }
}

/// The RTSP transport hint.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RtspTransport {
    /// Let the receiver negotiate (UDP first, then TCP).
    #[default]
    Auto,
    /// Interleave RTP over the RTSP TCP connection.
    Tcp,
    /// RTP over UDP.
    Udp,
}

impl RtspTransport {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Tcp, Self::Udp];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// How an RTMP or SRT input meets its peer (IN-016, IN-017).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamMode {
    /// Connect to the URL's host and pull the stream.
    #[default]
    Caller,
    /// Listen on the URL's local address and port for a publisher (an RTMP
    /// push from an encoder, or an SRT caller).
    Listener,
    /// SRT only: both peers connect to each other on the same port.
    Rendezvous,
}

impl StreamMode {
    pub const ALL: [Self; 3] = [Self::Caller, Self::Listener, Self::Rendezvous];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Caller => "caller",
            Self::Listener => "listener",
            Self::Rendezvous => "rendezvous",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }

    /// Whether `protocol` connects in this mode: every protocol is a
    /// caller, RTMP and SRT may listen, and only SRT meets in rendezvous.
    #[must_use]
    pub const fn supported_by(self, protocol: StreamProtocol) -> bool {
        match self {
            Self::Caller => true,
            Self::Listener => matches!(protocol, StreamProtocol::Rtmp | StreamProtocol::Srt),
            Self::Rendezvous => matches!(protocol, StreamProtocol::Srt),
        }
    }

    /// Whether the input waits for its peer rather than dialling it.
    #[must_use]
    pub const fn waits_for_peer(self) -> bool {
        matches!(self, Self::Listener | Self::Rendezvous)
    }
}

/// The AES key length an SRT passphrase derives (IN-017).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SrtKeyLength {
    /// AES-128 (16-byte key).
    #[default]
    Aes128,
    /// AES-192 (24-byte key).
    Aes192,
    /// AES-256 (32-byte key).
    Aes256,
}

impl SrtKeyLength {
    pub const ALL: [Self; 3] = [Self::Aes128, Self::Aes192, Self::Aes256];

    /// The key length in bytes (`pbkeylen`): 16, 24, or 32.
    #[must_use]
    pub const fn bytes(self) -> u8 {
        match self {
            Self::Aes128 => 16,
            Self::Aes192 => 24,
            Self::Aes256 => 32,
        }
    }

    /// The token: `16`, `24`, or `32`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Aes128 => "16",
            Self::Aes192 => "24",
            Self::Aes256 => "32",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Which source audio channels an input hears (IN-017): a downmix of every
/// channel, or one stereo pair of a multichannel source of up to
/// [`MAX_STREAM_AUDIO_CHANNELS`], so split stereo pairs reach a mixer strip
/// one pair per input.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AudioChannels {
    /// Downmix every source channel to the program layout.
    #[default]
    Mix,
    /// Source channels 1 and 2.
    Pair12,
    /// Source channels 3 and 4.
    Pair34,
    /// Source channels 5 and 6.
    Pair56,
    /// Source channels 7 and 8.
    Pair78,
}

impl AudioChannels {
    pub const ALL: [Self; 5] = [
        Self::Mix,
        Self::Pair12,
        Self::Pair34,
        Self::Pair56,
        Self::Pair78,
    ];

    /// The token: `mix`, `1-2`, `3-4`, `5-6`, or `7-8`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Mix => "mix",
            Self::Pair12 => "1-2",
            Self::Pair34 => "3-4",
            Self::Pair56 => "5-6",
            Self::Pair78 => "7-8",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }

    /// The zero-based source channel of the pair's left side, or `None`
    /// for a downmix.
    #[must_use]
    pub const fn first_channel(self) -> Option<u16> {
        match self {
            Self::Mix => None,
            Self::Pair12 => Some(0),
            Self::Pair34 => Some(2),
            Self::Pair56 => Some(4),
            Self::Pair78 => Some(6),
        }
    }
}

/// The transport a listening input binds its port on.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ListenTransport {
    Tcp,
    Udp,
}

/// The local port a listening input binds: two inputs may not share one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ListenEndpoint {
    pub transport: ListenTransport,
    pub port: u16,
}

impl fmt::Display for ListenEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = match self.transport {
            ListenTransport::Tcp => "TCP",
            ListenTransport::Udp => "UDP",
        };
        write!(formatter, "{transport} port {}", self.port)
    }
}

/// What a stalled input shows until its stream returns.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StallSlate {
    /// Hold the last received picture.
    #[default]
    Freeze,
    /// Show opaque black.
    Black,
}

impl StallSlate {
    pub const ALL: [Self; 2] = [Self::Freeze, Self::Black];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Freeze => "freeze",
            Self::Black => "black",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// When the receiver stops reconnecting.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum GiveUp {
    /// Reconnect for as long as the input is open.
    #[default]
    Never,
    /// Fail after this many consecutive unsuccessful attempts.
    After(NonZeroU32),
}

impl GiveUp {
    /// `never`, or the attempt count.
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Never => "never".to_owned(),
            Self::After(attempts) => attempts.to_string(),
        }
    }

    /// Parses [`Self::token`]; a count must be canonical decimal.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        if token == "never" {
            return Some(Self::Never);
        }
        if token.starts_with('0') || !token.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        token
            .parse()
            .ok()
            .and_then(NonZeroU32::new)
            .map(Self::After)
    }
}

/// How the receiver reconnects a stream that dropped or never connected.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReconnectPolicy {
    /// The delay before the first reconnect attempt.
    pub backoff_min_ms: u32,
    /// The longest delay between attempts; each delay doubles up to it.
    pub backoff_max_ms: u32,
    pub give_up: GiveUp,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            backoff_min_ms: 500,
            backoff_max_ms: 10_000,
            give_up: GiveUp::Never,
        }
    }
}

impl ReconnectPolicy {
    /// The delay before reconnect attempt `attempt` (one-based): the
    /// minimum doubled `attempt - 1` times, capped at the maximum.
    #[must_use]
    pub fn delay(self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(31);
        let delay = u64::from(self.backoff_min_ms)
            .saturating_mul(1_u64 << shift)
            .min(u64::from(self.backoff_max_ms));
        Duration::from_millis(delay)
    }

    /// Whether attempt `attempt` (one-based) may still be made.
    #[must_use]
    pub fn allows(self, attempt: u32) -> bool {
        match self.give_up {
            GiveUp::Never => true,
            GiveUp::After(attempts) => attempt <= attempts.get(),
        }
    }
}

/// Everything about a network stream input but its URL.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NetworkStreamSettings {
    /// RTSP transport; must be [`RtspTransport::Auto`] for other protocols.
    pub transport: RtspTransport,
    /// How much media the receiver buffers before it goes live, in
    /// milliseconds. The buffer holds at most twice this.
    pub jitter_buffer_ms: u32,
    pub reconnect: ReconnectPolicy,
    /// How long the receiver waits for data before it drops the connection
    /// and reconnects, in milliseconds.
    pub timeout_ms: u32,
    /// What the input shows while it stalls.
    pub stall: StallSlate,
    /// RTMP and SRT: caller, listener, or (SRT only) rendezvous. Must be
    /// [`StreamMode::Caller`] for other protocols.
    pub mode: StreamMode,
    /// SRT receive latency, in milliseconds; other protocols ignore it.
    pub latency_ms: u32,
    /// The AES key length an SRT passphrase derives; ignored without one.
    pub key_length: SrtKeyLength,
    /// Which source audio channels the input hears.
    pub audio: AudioChannels,
}

impl Default for NetworkStreamSettings {
    fn default() -> Self {
        Self {
            transport: RtspTransport::Auto,
            jitter_buffer_ms: DEFAULT_JITTER_BUFFER_MS,
            reconnect: ReconnectPolicy::default(),
            timeout_ms: 5_000,
            stall: StallSlate::Freeze,
            mode: StreamMode::Caller,
            latency_ms: DEFAULT_SRT_LATENCY_MS,
            key_length: SrtKeyLength::Aes128,
            audio: AudioChannels::Mix,
        }
    }
}

impl NetworkStreamSettings {
    /// Validates every bound.
    ///
    /// # Errors
    ///
    /// Returns the first [`NetworkStreamError`] found.
    pub fn validate(&self) -> Result<(), NetworkStreamError> {
        if !(MIN_JITTER_BUFFER_MS..=MAX_JITTER_BUFFER_MS).contains(&self.jitter_buffer_ms) {
            return Err(NetworkStreamError::JitterBuffer);
        }
        let backoff = MIN_RECONNECT_BACKOFF_MS..=MAX_RECONNECT_BACKOFF_MS;
        if !backoff.contains(&self.reconnect.backoff_min_ms)
            || !backoff.contains(&self.reconnect.backoff_max_ms)
            || self.reconnect.backoff_min_ms > self.reconnect.backoff_max_ms
        {
            return Err(NetworkStreamError::Backoff);
        }
        if let GiveUp::After(attempts) = self.reconnect.give_up
            && attempts.get() > MAX_RECONNECT_ATTEMPTS
        {
            return Err(NetworkStreamError::GiveUp);
        }
        if !(MIN_STREAM_TIMEOUT_MS..=MAX_STREAM_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(NetworkStreamError::Timeout);
        }
        if !(MIN_SRT_LATENCY_MS..=MAX_SRT_LATENCY_MS).contains(&self.latency_ms) {
            return Err(NetworkStreamError::Latency);
        }
        Ok(())
    }
}

/// A stream URL in transit (a new URL in an edit). `Debug` shows only its
/// redacted form.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct StreamUrl(String);

impl StreamUrl {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }

    /// The URL, credentials included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for StreamUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StreamUrl")
            .field(&redact_stream_url(&self.0))
            .finish()
    }
}

/// A network stream input's source: its URL, settings, and the RTMP and
/// SRT connection fields.
///
/// `Debug` shows only the redacted URL and masks the stream key and the
/// passphrase.
#[derive(Clone, Default, Eq, Hash, PartialEq)]
pub struct NetworkStreamConfig {
    /// The stream URL, credentials included. Report it only through
    /// [`Self::redacted_url`].
    pub url: String,
    pub settings: NetworkStreamSettings,
    /// SRT stream ID passed to the peer (`streamid`); not a secret.
    pub stream_id: Option<String>,
    /// RTMP stream key (a secret): appended to the URL's application path
    /// when calling, and required of a publisher when listening.
    pub stream_key: Option<String>,
    /// SRT AES passphrase (a secret), [`MIN_SRT_PASSPHRASE_BYTES`] to
    /// [`MAX_SRT_PASSPHRASE_BYTES`] bytes.
    pub passphrase: Option<String>,
}

impl fmt::Debug for NetworkStreamConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = self.redacted();
        formatter
            .debug_struct("NetworkStreamConfig")
            .field("url", &redacted.url)
            .field("settings", &self.settings)
            .field("stream_id", &self.stream_id)
            .field("stream_key", &redacted.stream_key)
            .field("passphrase", &redacted.passphrase)
            .finish()
    }
}

impl NetworkStreamConfig {
    /// A stream at `url` with default settings.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// Validates the URL, settings, and connection fields: a transport
    /// hint only for RTSP, a mode the protocol supports, a listening port
    /// of at least [`MIN_LISTEN_PORT`], a stream key only for RTMP (and
    /// then a URL naming just the application), and a passphrase and stream
    /// ID only for SRT.
    ///
    /// # Errors
    ///
    /// Returns the first [`NetworkStreamError`] found.
    pub fn validate(&self) -> Result<StreamProtocol, NetworkStreamError> {
        let protocol = validate_stream_url(&self.url)?;
        self.settings.validate()?;
        if protocol != StreamProtocol::Rtsp && self.settings.transport != RtspTransport::Auto {
            return Err(NetworkStreamError::TransportNotRtsp);
        }
        let mode = self.settings.mode;
        if !mode.supported_by(protocol)
            || (protocol == StreamProtocol::Rtmp
                && mode == StreamMode::Listener
                && scheme_of(&self.url).as_deref() == Some("rtmps"))
        {
            return Err(NetworkStreamError::ModeUnsupported);
        }
        if mode.waits_for_peer()
            && listen_port(&self.url, protocol).is_none_or(|port| port < MIN_LISTEN_PORT)
        {
            return Err(NetworkStreamError::ListenPort);
        }
        if let Some(key) = &self.stream_key
            && (protocol != StreamProtocol::Rtmp
                || key.is_empty()
                || key.len() > MAX_STREAM_KEY_BYTES
                || key == REDACTED
                || !key.bytes().all(|byte| byte.is_ascii_graphic())
                || rtmp_path_segments(&self.url) != 1)
        {
            return Err(NetworkStreamError::StreamKey);
        }
        if let Some(passphrase) = &self.passphrase
            && (protocol != StreamProtocol::Srt
                || !(MIN_SRT_PASSPHRASE_BYTES..=MAX_SRT_PASSPHRASE_BYTES)
                    .contains(&passphrase.len())
                || !passphrase.bytes().all(|byte| (b' '..=b'~').contains(&byte)))
        {
            return Err(NetworkStreamError::Passphrase);
        }
        if let Some(stream_id) = &self.stream_id
            && (protocol != StreamProtocol::Srt
                || stream_id.is_empty()
                || stream_id.len() > MAX_SRT_STREAM_ID_BYTES
                || !stream_id.bytes().all(|byte| (b' '..=b'~').contains(&byte)))
        {
            return Err(NetworkStreamError::StreamId);
        }
        Ok(protocol)
    }

    /// Validates a reported (redacted) configuration: as
    /// [`Self::validate`], except that a stream key or passphrase that is
    /// set must read exactly [`REDACTED`].
    ///
    /// # Errors
    ///
    /// Returns the first [`NetworkStreamError`] found.
    pub fn validate_redacted(&self) -> Result<StreamProtocol, NetworkStreamError> {
        let mut probe = self.clone();
        for (secret, placeholder, error) in [
            (&mut probe.stream_key, "k", NetworkStreamError::StreamKey),
            (
                &mut probe.passphrase,
                "redacted-passphrase",
                NetworkStreamError::Passphrase,
            ),
        ] {
            if let Some(value) = secret {
                if value != REDACTED {
                    return Err(error);
                }
                placeholder.clone_into(value);
            }
        }
        probe.validate()
    }

    /// The URL with its credentials, secret query values, and RTMP stream
    /// key path masked.
    #[must_use]
    pub fn redacted_url(&self) -> String {
        redact_stream_url(&self.url)
    }

    /// This configuration with its URL redacted and its stream key and
    /// passphrase replaced by [`REDACTED`], for reporting.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            url: self.redacted_url(),
            settings: self.settings,
            stream_id: self.stream_id.clone(),
            stream_key: self.stream_key.as_ref().map(|_| REDACTED.to_owned()),
            passphrase: self.passphrase.as_ref().map(|_| REDACTED.to_owned()),
        }
    }

    /// The URL the receiver opens: an RTMP stream key is appended to the
    /// application path, and an SRT stream ID travels percent-encoded as the
    /// `streamid` query parameter (`ffmpeg` decodes it there; its command
    /// line has no input option of that name).
    #[must_use]
    pub fn connect_url(&self) -> String {
        if let Some(key) = &self.stream_key {
            return format!("{}/{key}", self.url.trim_end_matches('/'));
        }
        match &self.stream_id {
            Some(stream_id) => {
                use fmt::Write as _;
                let mut url = format!("{}?streamid=", self.url.trim_end_matches('/'));
                for byte in stream_id.bytes() {
                    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                        url.push(char::from(byte));
                    } else {
                        let _ = write!(url, "%{byte:02X}");
                    }
                }
                url
            }
            None => self.url.clone(),
        }
    }

    /// Every secret this configuration holds (URL secrets, the stream key,
    /// and the passphrase), longest first, for scrubbing free text.
    #[must_use]
    pub fn secrets(&self) -> Vec<String> {
        let mut secrets = stream_url_secrets(&self.connect_url());
        secrets.extend(self.stream_key.iter().cloned());
        secrets.extend(self.passphrase.iter().cloned());
        secrets.retain(|secret| !secret.is_empty());
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        secrets
    }

    /// `text` with the URLs and every secret masked, bounded as
    /// [`scrub_stream_text`] bounds it.
    #[must_use]
    pub fn scrub(&self, text: &str) -> String {
        let connect = self.connect_url();
        let mut scrubbed = text.replace(&connect, &redact_stream_url(&connect));
        for secret in self.secrets() {
            scrubbed = scrubbed.replace(&secret, REDACTED);
        }
        scrub_stream_text(&scrubbed, &self.url)
    }

    /// The local port this input binds while it waits for its peer (an RTMP
    /// or SRT listener, or an SRT rendezvous), or `None` for an input that
    /// dials out. A UDP/RTP receiver is not counted: several may share a
    /// (multicast) port, as IN-015 allows.
    #[must_use]
    pub fn listen_endpoint(&self) -> Option<ListenEndpoint> {
        let protocol = validate_stream_url(&self.url).ok()?;
        let port = listen_port(&self.url, protocol)?;
        let transport = match (protocol, self.settings.mode) {
            (StreamProtocol::Rtmp, StreamMode::Listener) => ListenTransport::Tcp,
            (StreamProtocol::Srt, StreamMode::Listener | StreamMode::Rendezvous) => {
                ListenTransport::Udp
            }
            _ => return None,
        };
        Some(ListenEndpoint { transport, port })
    }
}

/// How an edit changes a secret (the RTMP stream key or SRT passphrase):
/// keep the stored one (which a client cannot read back), clear it, or set
/// a new one. `Debug` masks a new secret.
#[derive(Clone, Default, Eq, Hash, PartialEq)]
pub enum SecretEdit {
    #[default]
    Keep,
    Clear,
    Set(String),
}

impl fmt::Debug for SecretEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Clear => formatter.write_str("Clear"),
            Self::Set(_) => write!(formatter, "Set({REDACTED:?})"),
        }
    }
}

impl SecretEdit {
    /// The secret after this edit, given the stored one.
    #[must_use]
    pub fn apply(&self, current: Option<&str>) -> Option<String> {
        match self {
            Self::Keep => current.map(str::to_owned),
            Self::Clear => None,
            Self::Set(secret) => Some(secret.clone()),
        }
    }

    /// The edit that stores exactly `secret`.
    #[must_use]
    pub fn exact(secret: Option<&str>) -> Self {
        secret.map_or(Self::Clear, |secret| Self::Set(secret.to_owned()))
    }
}

/// The lowercase scheme of `url`.
fn scheme_of(url: &str) -> Option<String> {
    split_scheme(url).map(|(scheme, _)| scheme)
}

/// The port of `url`, or the protocol's default (RTMP's 1935).
fn listen_port(url: &str, protocol: StreamProtocol) -> Option<u16> {
    let (scheme, rest) = split_scheme(url)?;
    let (authority, _) = split_authority(rest);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (_, port) = split_host_port(host_port)?;
    port.or(match (protocol, scheme.as_str()) {
        (StreamProtocol::Rtmp, "rtmp") => Some(DEFAULT_RTMP_PORT),
        (StreamProtocol::Rtmp, _) => Some(443),
        _ => None,
    })
}

/// The non-empty path segments of an RTMP URL (`/app` is one, `/app/key`
/// two).
fn rtmp_path_segments(url: &str) -> usize {
    let Some((_, rest)) = split_scheme(url) else {
        return 0;
    };
    let (_, tail) = split_authority(rest);
    let path = tail.split(['?', '#']).next().unwrap_or_default();
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
}

/// Why a network stream configuration is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NetworkStreamError {
    /// The URL is empty, too long, not ASCII, holds spaces or control
    /// characters, has no host, misses a required port or RTMP application,
    /// or is an SRT URL with a query.
    InvalidUrl,
    /// The URL's scheme is not a supported stream protocol.
    UnsupportedScheme,
    /// A transport hint was given for a protocol other than RTSP.
    TransportNotRtsp,
    /// The jitter buffer is out of range.
    JitterBuffer,
    /// A backoff delay is out of range, or the minimum exceeds the maximum.
    Backoff,
    /// The give-up attempt count is out of range.
    GiveUp,
    /// The timeout is out of range.
    Timeout,
    /// The protocol cannot connect in the chosen mode (a listener other
    /// than RTMP or SRT, an RTMPS listener, or a rendezvous other than SRT).
    ModeUnsupported,
    /// A listening input names no port, or one below [`MIN_LISTEN_PORT`].
    ListenPort,
    /// The SRT latency is out of range.
    Latency,
    /// A stream key was given for a protocol other than RTMP, is empty, too
    /// long, or not printable ASCII, or the URL already names a stream path.
    StreamKey,
    /// A passphrase was given for a protocol other than SRT, or is not 10 to
    /// 79 printable ASCII characters.
    Passphrase,
    /// A stream ID was given for a protocol other than SRT, or is empty, too
    /// long, or not printable ASCII.
    StreamId,
}

impl NetworkStreamError {
    pub const ALL: [Self; 13] = [
        Self::InvalidUrl,
        Self::UnsupportedScheme,
        Self::TransportNotRtsp,
        Self::JitterBuffer,
        Self::Backoff,
        Self::GiveUp,
        Self::Timeout,
        Self::ModeUnsupported,
        Self::ListenPort,
        Self::Latency,
        Self::StreamKey,
        Self::Passphrase,
        Self::StreamId,
    ];

    /// The stable code of the error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_url",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::TransportNotRtsp => "transport_not_rtsp",
            Self::JitterBuffer => "jitter_buffer_out_of_range",
            Self::Backoff => "backoff_out_of_range",
            Self::GiveUp => "give_up_out_of_range",
            Self::Timeout => "timeout_out_of_range",
            Self::ModeUnsupported => "mode_unsupported",
            Self::ListenPort => "listen_port_invalid",
            Self::Latency => "latency_out_of_range",
            Self::StreamKey => "invalid_stream_key",
            Self::Passphrase => "invalid_passphrase",
            Self::StreamId => "invalid_stream_id",
        }
    }

    /// The field the error is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidUrl | Self::UnsupportedScheme | Self::ListenPort => "url",
            Self::TransportNotRtsp => "transport",
            Self::JitterBuffer => "jitter_ms",
            Self::Backoff => "backoff",
            Self::GiveUp => "give_up",
            Self::Timeout => "timeout_ms",
            Self::ModeUnsupported => "mode",
            Self::Latency => "latency_ms",
            Self::StreamKey => "stream_key",
            Self::Passphrase => "passphrase",
            Self::StreamId => "stream_id",
        }
    }
}

impl fmt::Display for NetworkStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => {
                "the stream URL is empty, too long, not ASCII, contains spaces, has no host, \
                 or misses its port or RTMP application"
            }
            Self::UnsupportedScheme => {
                "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls, rtmp, rtmps, \
                 srt)"
            }
            Self::TransportNotRtsp => "a transport hint applies only to RTSP streams",
            Self::JitterBuffer => "the jitter buffer must be 40 to 5000 ms",
            Self::Backoff => {
                "reconnect delays must be 100 to 60000 ms, the minimum at most the maximum"
            }
            Self::GiveUp => "give up after 1 to 10000 attempts, or never",
            Self::Timeout => "the timeout must be 500 to 60000 ms",
            Self::ModeUnsupported => {
                "only RTMP and SRT may listen (RTMPS may not), and only SRT meets in rendezvous"
            }
            Self::ListenPort => "a listening input needs a port from 1024 to 65535",
            Self::Latency => "the SRT latency must be 20 to 8000 ms",
            Self::StreamKey => {
                "a stream key is 1 to 256 printable characters, for RTMP only, with a URL \
                 naming just the application"
            }
            Self::Passphrase => "an SRT passphrase is 10 to 79 printable characters, for SRT only",
            Self::StreamId => "an SRT stream ID is 1 to 512 printable characters, for SRT only",
        })
    }
}

impl std::error::Error for NetworkStreamError {}

/// Splits `url` into its lowercase scheme and the rest after `://`.
fn split_scheme(url: &str) -> Option<(String, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), rest))
}

/// The authority (`[userinfo@]host[:port]`) and the remainder of `rest`.
fn split_authority(rest: &str) -> (&str, &str) {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest.split_at(end)
}

/// Validates a stream URL and returns the protocol it names.
///
/// A URL is ASCII without spaces or control characters, at most
/// [`MAX_STREAM_URL_BYTES`] long (its redacted form too), with a supported scheme and a host. UDP and
/// RTP URLs may leave the host empty (`udp://@:1234`, `udp://:1234`) to
/// listen on every interface, but must then name a port.
///
/// # Errors
///
/// Returns [`NetworkStreamError::InvalidUrl`] or
/// [`NetworkStreamError::UnsupportedScheme`].
pub fn validate_stream_url(url: &str) -> Result<StreamProtocol, NetworkStreamError> {
    if url.is_empty()
        || url.len() > MAX_STREAM_URL_BYTES
        || !url.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(NetworkStreamError::InvalidUrl);
    }
    let (scheme, rest) = split_scheme(url).ok_or(NetworkStreamError::InvalidUrl)?;
    // The scheme is judged first: `file:///x` names no host, but what is
    // wrong with it is the scheme.
    if !matches!(
        scheme.as_str(),
        "rtsp"
            | "rtsps"
            | "rtp"
            | "udp"
            | "tcp"
            | "http"
            | "https"
            | "rtmp"
            | "rtmps"
            | "srt"
            | "omt"
    ) {
        return Err(NetworkStreamError::UnsupportedScheme);
    }
    let (authority, path) = split_authority(rest);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = split_host_port(host_port).ok_or(NetworkStreamError::InvalidUrl)?;
    let listening = matches!(scheme.as_str(), "udp" | "rtp");
    if host.is_empty() && !(listening && port.is_some()) {
        return Err(NetworkStreamError::InvalidUrl);
    }
    let protocol = match scheme.as_str() {
        "rtsp" | "rtsps" => StreamProtocol::Rtsp,
        "rtp" => StreamProtocol::Rtp,
        "udp" => StreamProtocol::Udp,
        "tcp" => StreamProtocol::Tcp,
        "rtmp" | "rtmps" => StreamProtocol::Rtmp,
        "srt" => StreamProtocol::Srt,
        "omt" => StreamProtocol::Omt,
        "http" | "https" => {
            let path = path.split(['?', '#']).next().unwrap_or_default();
            if path.to_ascii_lowercase().ends_with(".m3u8") {
                StreamProtocol::Hls
            } else {
                StreamProtocol::Http
            }
        }
        _ => return Err(NetworkStreamError::UnsupportedScheme),
    };
    if matches!(
        protocol,
        StreamProtocol::Tcp
            | StreamProtocol::Udp
            | StreamProtocol::Rtp
            | StreamProtocol::Srt
            | StreamProtocol::Omt
    ) && port.is_none()
    {
        return Err(NetworkStreamError::InvalidUrl);
    }
    // An SRT URL's options are the input's own fields (mode, passphrase,
    // stream ID, latency), never query parameters that could contradict
    // them; an RTMP URL names at least its application.
    if (protocol == StreamProtocol::Srt && !path.is_empty() && path != "/")
        || (protocol == StreamProtocol::Omt && !path.is_empty() && path != "/")
        || (protocol == StreamProtocol::Rtmp && rtmp_path_segments(url) == 0)
    {
        return Err(NetworkStreamError::InvalidUrl);
    }
    // Masked secrets may be longer than the secrets: the redacted URL every
    // report carries must fit the same bound.
    if redact_stream_url(url).len() > MAX_STREAM_URL_BYTES {
        return Err(NetworkStreamError::InvalidUrl);
    }
    Ok(protocol)
}

/// The `host[:port]` authority of an `omt://` URL, or `None` for any other
/// URL. The port is required.
#[must_use]
pub fn omt_authority(url: &str) -> Option<String> {
    if validate_stream_url(url).ok()? != StreamProtocol::Omt {
        return None;
    }
    let (_, rest) = split_scheme(url)?;
    let (authority, _) = split_authority(rest);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(host_port.to_owned())
}

/// Splits `host[:port]` (a bracketed IPv6 host allowed), checking the port.
fn split_host_port(value: &str) -> Option<(&str, Option<u16>)> {
    let (host, port) = if let Some(bracketed) = value.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':')?)),
        }
    } else {
        match value.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (value, None),
        }
    };
    if host.contains(['@', '[', ']']) {
        return None;
    }
    let port = match port {
        None => None,
        Some(port) => {
            if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(port.parse::<u16>().ok().filter(|port| *port != 0)?)
        }
    };
    let host = host.strip_prefix('@').unwrap_or(host);
    Some((host, port))
}

/// `url` with its userinfo (user and password) replaced by [`REDACTED`],
/// and the value of every secret query parameter (`password`, `token`,
/// `key`, ...) replaced by [`REDACTED`]. A URL without secrets is returned
/// unchanged. Text that is not a URL is returned as [`REDACTED`] when it
/// contains `@`, and unchanged otherwise.
#[must_use]
pub fn redact_stream_url(url: &str) -> String {
    let Some((scheme, rest)) = split_scheme(url) else {
        return if url.contains('@') {
            REDACTED.to_owned()
        } else {
            url.to_owned()
        };
    };
    let scheme_end = url.len() - rest.len();
    let (authority, tail) = split_authority(rest);
    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..scheme_end]);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) if !userinfo.is_empty() => {
            redacted.push_str(REDACTED);
            redacted.push('@');
            redacted.push_str(host);
        }
        _ => redacted.push_str(authority),
    }
    let (before_fragment, fragment) = match tail.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (tail, None),
    };
    let rtmp = matches!(scheme.as_str(), "rtmp" | "rtmps");
    let push_path = |redacted: &mut String, path: &str| match rtmp_key_path(path).filter(|_| rtmp) {
        Some((app, _)) => {
            redacted.push_str(app);
            redacted.push('/');
            redacted.push_str(REDACTED);
        }
        None => redacted.push_str(path),
    };
    match before_fragment.split_once('?') {
        Some((path, query)) => {
            push_path(&mut redacted, path);
            redacted.push('?');
            let parts = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((key, _)) if is_secret_key(key) => format!("{key}={REDACTED}"),
                    _ => pair.to_owned(),
                })
                .collect::<Vec<_>>();
            redacted.push_str(&parts.join("&"));
        }
        None => push_path(&mut redacted, before_fragment),
    }
    if let Some(fragment) = fragment {
        redacted.push('#');
        redacted.push_str(fragment);
    }
    redacted
}

/// An RTMP path's application (`/app`) and the stream path after it (the
/// stream key), or `None` when the path names no stream.
fn rtmp_key_path(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.strip_prefix('/')?;
    let (app, key) = trimmed.split_once('/')?;
    if app.is_empty() || key.trim_matches('/').is_empty() {
        return None;
    }
    Some((&path[..=app.len()], key))
}

fn is_secret_key(key: &str) -> bool {
    // A server percent-decodes the key: `%74oken` names `token`.
    let key = percent_decode(key).to_ascii_lowercase();
    SECRET_QUERY_KEYS
        .iter()
        .any(|secret| key == *secret || key.ends_with(&format!("_{secret}")))
}

/// `text` with each `%XX` escape decoded; an invalid escape, or one that
/// decodes to a byte that is not ASCII, is kept as written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| core::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            .filter(u8::is_ascii);
        if let Some(byte) = escaped {
            decoded.push(char::from(byte));
            index += 3;
        } else {
            decoded.push(char::from(bytes[index]));
            index += 1;
        }
    }
    decoded
}

/// Every secret substring of `url` (its userinfo, the password alone, and
/// each secret query value), longest first, for scrubbing free text such as
/// tool diagnostics that may echo them.
#[must_use]
pub fn stream_url_secrets(url: &str) -> Vec<String> {
    let mut secrets = Vec::new();
    if let Some((scheme, rest)) = split_scheme(url) {
        let (authority, tail) = split_authority(rest);
        if matches!(scheme.as_str(), "rtmp" | "rtmps")
            && let Some((_, key)) = rtmp_key_path(tail.split(['?', '#']).next().unwrap_or_default())
        {
            secrets.push(key.to_owned());
            secrets.extend(key.split('/').map(str::to_owned));
        }
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            secrets.push(userinfo.to_owned());
            if let Some((user, password)) = userinfo.split_once(':') {
                secrets.push(password.to_owned());
                secrets.push(user.to_owned());
            } else {
                secrets.push(userinfo.to_owned());
            }
        }
        if let Some((_, query)) = tail.split('#').next().unwrap_or_default().split_once('?') {
            for pair in query.split('&') {
                if let Some((key, value)) = pair.split_once('=')
                    && is_secret_key(key)
                {
                    secrets.push(value.to_owned());
                }
            }
        }
    }
    // A tool may echo a secret percent-decoded.
    let decoded = secrets
        .iter()
        .map(|secret| percent_decode(secret))
        .collect::<Vec<_>>();
    secrets.extend(decoded);
    secrets.retain(|secret| !secret.is_empty());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    secrets
}

/// `text` with the full URL and each of its secrets replaced, bounded to
/// [`MAX_STREAM_ERROR_BYTES`] on a character boundary, with control
/// characters replaced by spaces.
#[must_use]
pub fn scrub_stream_text(text: &str, url: &str) -> String {
    let mut scrubbed = text.replace(url, &redact_stream_url(url));
    for secret in stream_url_secrets(url) {
        scrubbed = scrubbed.replace(&secret, REDACTED);
    }
    let scrubbed: String = scrubbed
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut scrubbed = scrubbed.trim().to_owned();
    if scrubbed.len() > MAX_STREAM_ERROR_BYTES {
        let mut end = MAX_STREAM_ERROR_BYTES;
        while !scrubbed.is_char_boundary(end) {
            end -= 1;
        }
        scrubbed.truncate(end);
    }
    scrubbed
}

/// A network stream's receive state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum NetworkStreamState {
    /// Nothing receives the stream: the input is closed, or the daemon does
    /// not realize network inputs.
    #[default]
    Idle,
    /// The first connection is being made.
    Connecting,
    /// A listener (or SRT rendezvous) waits for its peer to connect
    /// (IN-016, IN-017).
    Listening,
    /// Connected; filling the jitter buffer before going live.
    Buffering,
    /// Playing from the jitter buffer.
    Live,
    /// Connected but no media arrives; the input holds its stall slate.
    Stalled,
    /// Waiting `next_in_ms` before reconnect attempt `attempt` (one-based).
    Reconnecting { attempt: u32, next_in_ms: u32 },
    /// The reconnect policy gave up.
    Failed,
}

impl NetworkStreamState {
    /// The state's token: `idle`, `connecting`, `listening`, `buffering`,
    /// `live`, `stalled`, `reconnecting`, or `failed`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Listening => "listening",
            Self::Buffering => "buffering",
            Self::Live => "live",
            Self::Stalled => "stalled",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Failed => "failed",
        }
    }
}

/// A network stream's receive status.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct NetworkStreamStatus {
    pub state: NetworkStreamState,
    /// Received media bitrate over about the last second, in bits per
    /// second.
    pub bitrate_bps: u64,
    /// Media buffered ahead of the program, in milliseconds.
    pub buffer_ms: u32,
    /// Frames discarded because the jitter buffer was full.
    pub dropped_frames: u64,
    /// Program frames that found the buffer empty while connected.
    pub late_frames: u64,
    /// Reconnect attempts made since the input opened.
    pub reconnects: u32,
    /// Audio channels the source's first audio stream carries (up to
    /// [`MAX_STREAM_AUDIO_CHANNELS`] are told apart), or 0 when unknown.
    pub source_channels: u16,
    /// The last connection error, redacted and bounded to
    /// [`MAX_STREAM_ERROR_BYTES`].
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_name_their_protocol() {
        for (url, protocol) in [
            ("rtsp://cam.local:554/stream1", StreamProtocol::Rtsp),
            ("RTSPS://cam.local/s", StreamProtocol::Rtsp),
            ("rtp://239.0.0.1:5004", StreamProtocol::Rtp),
            ("udp://@:1234", StreamProtocol::Udp),
            ("udp://239.1.1.1:1234?pkt_size=1316", StreamProtocol::Udp),
            ("tcp://10.0.0.2:9000", StreamProtocol::Tcp),
            ("http://host/live.ts", StreamProtocol::Http),
            ("https://cdn.example/a/index.M3U8?x=1", StreamProtocol::Hls),
            ("http://[::1]:8080/live.m3u8", StreamProtocol::Hls),
            ("omt://192.168.1.5:6500", StreamProtocol::Omt),
            ("OMT://studio.local:6500/", StreamProtocol::Omt),
        ] {
            assert_eq!(validate_stream_url(url), Ok(protocol), "{url}");
        }
    }

    #[test]
    fn omt_urls_name_their_authority() {
        assert_eq!(
            omt_authority("omt://127.0.0.1:6500"),
            Some("127.0.0.1:6500".to_owned())
        );
        assert_eq!(
            omt_authority("omt://[::1]:6500"),
            Some("[::1]:6500".to_owned())
        );
        assert_eq!(omt_authority("omt://host"), None);
        assert_eq!(omt_authority("rtsp://host:554"), None);
        let config = NetworkStreamConfig::new("omt://host:6500");
        assert_eq!(config.validate(), Ok(StreamProtocol::Omt));
        let mut listener = config.clone();
        listener.settings.mode = StreamMode::Listener;
        assert_eq!(
            listener.validate(),
            Err(NetworkStreamError::ModeUnsupported)
        );
        let mut passphrase = config;
        passphrase.passphrase = Some("abcdefghijk".to_owned());
        assert_eq!(passphrase.validate(), Err(NetworkStreamError::Passphrase));
    }

    #[test]
    fn bad_urls_are_refused() {
        for url in [
            "",
            "rtsp://",
            "rtsp:// cam/",
            "cam.local/stream",
            "udp://host",
            "tcp://host",
            "udp://:0",
            "udp://:99999",
            "rtsp://cam:abc/",
            "rtsp://cäm/",
        ] {
            assert_eq!(
                validate_stream_url(url),
                Err(NetworkStreamError::InvalidUrl),
                "{url}"
            );
        }
        for url in ["ftp://host/x", "file:///etc/passwd", "concat:/a|/b"] {
            assert_eq!(
                validate_stream_url(url),
                Err(if url.contains("://") {
                    NetworkStreamError::UnsupportedScheme
                } else {
                    NetworkStreamError::InvalidUrl
                }),
                "{url}"
            );
        }
        assert_eq!(
            NetworkStreamError::UnsupportedScheme.to_string(),
            "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls, rtmp, rtmps, srt)"
        );
        let long = format!("http://h/{}", "a".repeat(MAX_STREAM_URL_BYTES));
        assert_eq!(
            validate_stream_url(&long),
            Err(NetworkStreamError::InvalidUrl)
        );
    }

    #[test]
    fn percent_encoded_secret_keys_are_redacted() {
        // A server decodes `%74oken` to `token`: its value is just as secret.
        for url in [
            "http://cdn/live.ts?%74oken=s3cr3t",
            "http://cdn/live.ts?TO%4Ben=s3cr3t",
            "http://cdn/live.ts?stream%5Fkey=s3cr3t",
        ] {
            let redacted = redact_stream_url(url);
            assert!(!redacted.contains("s3cr3t"), "{redacted}");
            assert!(
                !scrub_stream_text(&format!("error: s3cr3t {url}"), url).contains("s3cr3t"),
                "{url}"
            );
        }
    }

    #[test]
    fn every_valid_url_redacts_to_a_valid_url() {
        // Short secrets grow when masked: a URL at the length bound whose
        // redacted form overflows it could not be reported in a snapshot.
        let pad = |url: &str| {
            let fill = MAX_STREAM_URL_BYTES - url.len();
            format!("{url}{}", "a".repeat(fill))
        };
        for url in [
            pad("rtsp://u@cam/"),
            pad("http://cam/live.ts?key=k&token=t&x="),
            "rtsp://u:p@cam/s".to_owned(),
        ] {
            if validate_stream_url(&url).is_ok() {
                let redacted = redact_stream_url(&url);
                assert!(
                    validate_stream_url(&redacted).is_ok(),
                    "{url} redacts to the invalid {redacted}"
                );
            }
        }
        assert_eq!(
            validate_stream_url(&pad("rtsp://u@cam/")),
            Err(NetworkStreamError::InvalidUrl)
        );
    }

    #[test]
    fn redaction_masks_credentials_and_secret_query_values() {
        assert_eq!(
            redact_stream_url("rtsp://admin:hunter2@cam.local:554/s?token=abc&x=1#f"),
            "rtsp://***@cam.local:554/s?token=***&x=1#f"
        );
        assert_eq!(
            redact_stream_url("http://host/live.ts?stream_key=k1"),
            "http://host/live.ts?stream_key=***"
        );
        assert_eq!(redact_stream_url("udp://@:1234"), "udp://@:1234");
        assert_eq!(redact_stream_url("not a url"), "not a url");
        assert_eq!(redact_stream_url("user@host"), REDACTED);
        let config = NetworkStreamConfig::new("rtsp://u:p4ss@h/s");
        assert!(!format!("{config:?}").contains("p4ss"));
        assert_eq!(config.redacted().url, "rtsp://***@h/s");
    }

    #[test]
    fn scrubbing_removes_every_secret() {
        let url = "rtsp://admin:hunter2@cam/s?auth=zz9";
        let text = format!("{url}: 401 Unauthorized for admin (hunter2) zz9\n");
        let encoded = "rtsp://admin:hunt%65r2@cam/s";
        assert!(!scrub_stream_text("auth hunter2 failed", encoded).contains("hunter2"));
        let scrubbed = scrub_stream_text(&text, url);
        for secret in ["hunter2", "admin", "zz9"] {
            assert!(!scrubbed.contains(secret), "{scrubbed}");
        }
        assert!(
            scrubbed.starts_with("rtsp://***@cam/s?auth=***"),
            "{scrubbed}"
        );
        assert!(scrub_stream_text(&"é".repeat(200), url).len() <= MAX_STREAM_ERROR_BYTES);
    }

    #[test]
    fn settings_are_bounded() {
        let mut config = NetworkStreamConfig::new("udp://@:1234");
        assert_eq!(config.validate(), Ok(StreamProtocol::Udp));
        config.settings.transport = RtspTransport::Tcp;
        assert_eq!(config.validate(), Err(NetworkStreamError::TransportNotRtsp));
        config.settings.transport = RtspTransport::Auto;
        config.settings.jitter_buffer_ms = MAX_JITTER_BUFFER_MS + 1;
        assert_eq!(config.validate(), Err(NetworkStreamError::JitterBuffer));
        config.settings.jitter_buffer_ms = MIN_JITTER_BUFFER_MS;
        config.settings.reconnect.backoff_min_ms = 20_000;
        assert_eq!(config.validate(), Err(NetworkStreamError::Backoff));
        config.settings.reconnect.backoff_min_ms = 100;
        config.settings.reconnect.give_up =
            GiveUp::After(NonZeroU32::new(MAX_RECONNECT_ATTEMPTS + 1).unwrap());
        assert_eq!(config.validate(), Err(NetworkStreamError::GiveUp));
        config.settings.reconnect.give_up = GiveUp::Never;
        config.settings.timeout_ms = 100;
        assert_eq!(config.validate(), Err(NetworkStreamError::Timeout));
    }

    #[test]
    fn backoff_doubles_to_its_cap_and_give_up_counts_attempts() {
        let policy = ReconnectPolicy {
            backoff_min_ms: 250,
            backoff_max_ms: 2_000,
            give_up: GiveUp::After(NonZeroU32::new(3).unwrap()),
        };
        let delays: Vec<_> = (1..=6).map(|n| policy.delay(n).as_millis()).collect();
        assert_eq!(delays, [250, 500, 1_000, 2_000, 2_000, 2_000]);
        assert_eq!(policy.delay(u32::MAX).as_millis(), 2_000);
        assert!(policy.allows(3));
        assert!(!policy.allows(4));
        assert_eq!(GiveUp::from_token("never"), Some(GiveUp::Never));
        assert_eq!(GiveUp::from_token("03"), None);
        assert_eq!(GiveUp::from_token("0"), None);
        assert_eq!(GiveUp::from_token("7").unwrap().token(), "7");
    }

    #[test]
    fn rtmp_and_srt_urls_name_their_protocol() {
        for (url, protocol) in [
            ("rtmp://ingest.test/live", StreamProtocol::Rtmp),
            ("RTMPS://ingest.test:443/app/key", StreamProtocol::Rtmp),
            ("srt://10.0.0.5:9000", StreamProtocol::Srt),
            ("srt://0.0.0.0:9000/", StreamProtocol::Srt),
        ] {
            assert_eq!(validate_stream_url(url), Ok(protocol), "{url}");
        }
        for url in [
            "rtmp://ingest.test",
            "rtmp://ingest.test/",
            "srt://host",
            "srt://host:9000?passphrase=abcdefghijk",
            "srt://host:9000/path",
        ] {
            assert_eq!(
                validate_stream_url(url),
                Err(NetworkStreamError::InvalidUrl),
                "{url}"
            );
        }
    }

    #[test]
    fn modes_belong_to_their_protocols_and_listeners_need_a_free_port() {
        let mut config = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
        config.settings.mode = StreamMode::Listener;
        assert_eq!(config.validate(), Ok(StreamProtocol::Rtmp));
        assert_eq!(
            config.listen_endpoint(),
            Some(ListenEndpoint {
                transport: ListenTransport::Tcp,
                port: 1935
            })
        );
        config.url = "rtmp://0.0.0.0/live".into();
        assert_eq!(config.listen_endpoint().map(|end| end.port), Some(1935));
        config.url = "rtmp://0.0.0.0:80/live".into();
        assert_eq!(config.validate(), Err(NetworkStreamError::ListenPort));
        config.url = "rtmps://0.0.0.0:1936/live".into();
        assert_eq!(config.validate(), Err(NetworkStreamError::ModeUnsupported));
        config.url = "rtsp://cam/s".into();
        assert_eq!(config.validate(), Err(NetworkStreamError::ModeUnsupported));
        config.settings.mode = StreamMode::Rendezvous;
        config.url = "rtmp://h/live".into();
        assert_eq!(config.validate(), Err(NetworkStreamError::ModeUnsupported));
        config.url = "srt://203.0.113.9:9000".into();
        assert_eq!(config.validate(), Ok(StreamProtocol::Srt));
        assert_eq!(
            config.listen_endpoint(),
            Some(ListenEndpoint {
                transport: ListenTransport::Udp,
                port: 9000
            })
        );
        config.settings.mode = StreamMode::Caller;
        assert_eq!(config.listen_endpoint(), None);
        assert_eq!(
            NetworkStreamConfig::new("udp://@:1234").listen_endpoint(),
            None
        );
        config.settings.latency_ms = MAX_SRT_LATENCY_MS + 1;
        assert_eq!(config.validate(), Err(NetworkStreamError::Latency));
    }

    #[test]
    fn secrets_belong_to_their_protocols_and_are_bounded() {
        let mut rtmp = NetworkStreamConfig::new("rtmp://ingest.test/live");
        rtmp.stream_key = Some("live_123-abc".into());
        assert_eq!(rtmp.validate(), Ok(StreamProtocol::Rtmp));
        assert_eq!(rtmp.connect_url(), "rtmp://ingest.test/live/live_123-abc");
        let mut with_id = NetworkStreamConfig::new("srt://enc.test:9000/");
        with_id.stream_id = Some("#!::r=live/a b,m=request".into());
        assert_eq!(
            with_id.connect_url(),
            "srt://enc.test:9000?streamid=%23%21%3A%3Ar%3Dlive%2Fa%20b%2Cm%3Drequest"
        );
        for bad in [
            "",
            "has space",
            REDACTED,
            &"k".repeat(MAX_STREAM_KEY_BYTES + 1),
        ] {
            rtmp.stream_key = Some(bad.to_owned());
            assert_eq!(rtmp.validate(), Err(NetworkStreamError::StreamKey), "{bad}");
        }
        rtmp.stream_key = Some("k".into());
        rtmp.url = "rtmp://ingest.test/live/other".into();
        assert_eq!(rtmp.validate(), Err(NetworkStreamError::StreamKey));
        rtmp.stream_key = None;
        rtmp.passphrase = Some("abcdefghijk".into());
        assert_eq!(rtmp.validate(), Err(NetworkStreamError::Passphrase));

        let mut srt = NetworkStreamConfig::new("srt://enc.test:9000");
        srt.passphrase = Some("0123456789".into());
        srt.stream_id = Some("#!::r=live/cam1,m=request".into());
        srt.settings.key_length = SrtKeyLength::Aes256;
        assert_eq!(srt.validate(), Ok(StreamProtocol::Srt));
        srt.passphrase = Some("p".repeat(MAX_SRT_PASSPHRASE_BYTES));
        assert_eq!(srt.validate(), Ok(StreamProtocol::Srt));
        for bad in ["short", &"p".repeat(MAX_SRT_PASSPHRASE_BYTES + 1)] {
            srt.passphrase = Some(bad.to_owned());
            assert_eq!(srt.validate(), Err(NetworkStreamError::Passphrase));
        }
        srt.passphrase = None;
        srt.stream_id = Some("s".repeat(MAX_SRT_STREAM_ID_BYTES + 1));
        assert_eq!(srt.validate(), Err(NetworkStreamError::StreamId));
        srt.stream_id = None;
        srt.stream_key = Some("k".into());
        assert_eq!(srt.validate(), Err(NetworkStreamError::StreamKey));
    }

    #[test]
    fn stream_keys_and_passphrases_are_redacted_everywhere() {
        assert_eq!(
            redact_stream_url("rtmp://ingest.test/live/sk_live_9f8e7d"),
            "rtmp://ingest.test/live/***"
        );
        assert_eq!(
            redact_stream_url("rtmps://u:p@ingest.test:443/app/a/b?x=1"),
            "rtmps://***@ingest.test:443/app/***?x=1"
        );
        assert_eq!(
            redact_stream_url("rtmp://ingest.test/live"),
            "rtmp://ingest.test/live"
        );
        let mut rtmp = NetworkStreamConfig::new("rtmp://ingest.test/live");
        rtmp.stream_key = Some("sk_live_9f8e7d".into());
        let mut srt = NetworkStreamConfig::new("srt://enc.test:9000");
        srt.passphrase = Some("correct horse battery".into());
        for (config, secret) in [(&rtmp, "sk_live_9f8e7d"), (&srt, "correct horse battery")] {
            assert!(!format!("{config:?}").contains(secret));
            let redacted = config.redacted();
            assert!(!format!("{redacted:?}").contains(secret));
            assert_eq!(redacted.validate_redacted(), config.validate());
            assert!(redacted.validate().is_err());
            let echoed = format!("failed to open {} with {secret}", config.connect_url());
            assert!(!config.scrub(&echoed).contains(secret), "{echoed}");
        }
        let mut forged = rtmp.redacted();
        forged.stream_key = Some("real".into());
        assert_eq!(
            forged.validate_redacted(),
            Err(NetworkStreamError::StreamKey)
        );
        assert_eq!(
            format!("{:?}", SecretEdit::Set("hunter2".into())),
            "Set(\"***\")"
        );
        assert_eq!(SecretEdit::Keep.apply(Some("a")), Some("a".into()));
        assert_eq!(SecretEdit::Clear.apply(Some("a")), None);
        let caller = NetworkStreamConfig::new("rtmp://ingest.test/live/sk_1");
        assert!(!caller.scrub("Unexpected stream sk_1").contains("sk_1"));
    }

    #[test]
    fn audio_pairs_name_their_source_channels() {
        assert_eq!(AudioChannels::Mix.first_channel(), None);
        assert_eq!(AudioChannels::Pair78.first_channel(), Some(6));
        for audio in AudioChannels::ALL {
            assert_eq!(AudioChannels::from_token(audio.token()), Some(audio));
        }
        for length in SrtKeyLength::ALL {
            assert_eq!(SrtKeyLength::from_token(length.token()), Some(length));
            assert_eq!(length.token(), length.bytes().to_string());
        }
    }
}
