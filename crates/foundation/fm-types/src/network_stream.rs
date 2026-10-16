//! Network stream input value types (IN-015).
//!
//! A network stream input receives one RTSP, RTP, UDP, TCP, or HTTP(S)
//! MPEG-TS stream, or an HLS playlist, by URL. Its
//! [`NetworkStreamSettings`] bound the receive jitter buffer, the reconnect
//! policy (exponential backoff between a minimum and maximum delay, and
//! whether to give up after a number of attempts), the no-data timeout, and
//! what the input shows while the stream stalls.
//!
//! A URL may carry credentials (`rtsp://user:password@host/path`) or secret
//! query parameters (`?token=...`). [`redact_stream_url`] masks both, and
//! everything that reports a URL to an operator (snapshots, events, logs,
//! status text) reports only its redacted form.
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
}

impl StreamProtocol {
    /// Every protocol, in token order.
    pub const ALL: [Self; 6] = [
        Self::Rtsp,
        Self::Rtp,
        Self::Udp,
        Self::Tcp,
        Self::Http,
        Self::Hls,
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
}

impl Default for NetworkStreamSettings {
    fn default() -> Self {
        Self {
            transport: RtspTransport::Auto,
            jitter_buffer_ms: DEFAULT_JITTER_BUFFER_MS,
            reconnect: ReconnectPolicy::default(),
            timeout_ms: 5_000,
            stall: StallSlate::Freeze,
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

/// A network stream input's source: its URL and settings.
///
/// `Debug` shows only the redacted URL.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct NetworkStreamConfig {
    /// The stream URL, credentials included. Report it only through
    /// [`Self::redacted_url`].
    pub url: String,
    pub settings: NetworkStreamSettings,
}

impl fmt::Debug for NetworkStreamConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkStreamConfig")
            .field("url", &self.redacted_url())
            .field("settings", &self.settings)
            .finish()
    }
}

impl NetworkStreamConfig {
    /// A stream at `url` with default settings.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            settings: NetworkStreamSettings::default(),
        }
    }

    /// Validates the URL and settings, and that a transport hint is only
    /// given for RTSP.
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
        Ok(protocol)
    }

    /// The URL with its credentials and secret query values masked.
    #[must_use]
    pub fn redacted_url(&self) -> String {
        redact_stream_url(&self.url)
    }

    /// This configuration with its URL redacted, for reporting.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            url: self.redacted_url(),
            settings: self.settings,
        }
    }
}

/// Why a network stream configuration is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NetworkStreamError {
    /// The URL is empty, too long, not ASCII, holds spaces or control
    /// characters, or has no host.
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
}

impl NetworkStreamError {
    pub const ALL: [Self; 7] = [
        Self::InvalidUrl,
        Self::UnsupportedScheme,
        Self::TransportNotRtsp,
        Self::JitterBuffer,
        Self::Backoff,
        Self::GiveUp,
        Self::Timeout,
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
        }
    }

    /// The field the error is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidUrl | Self::UnsupportedScheme => "url",
            Self::TransportNotRtsp => "transport",
            Self::JitterBuffer => "jitter_ms",
            Self::Backoff => "backoff",
            Self::GiveUp => "give_up",
            Self::Timeout => "timeout_ms",
        }
    }
}

impl fmt::Display for NetworkStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => {
                "the stream URL is empty, too long, not ASCII, contains spaces, or has no host"
            }
            Self::UnsupportedScheme => {
                "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls)"
            }
            Self::TransportNotRtsp => "a transport hint applies only to RTSP streams",
            Self::JitterBuffer => "the jitter buffer must be 40 to 5000 ms",
            Self::Backoff => {
                "reconnect delays must be 100 to 60000 ms, the minimum at most the maximum"
            }
            Self::GiveUp => "give up after 1 to 10000 attempts, or never",
            Self::Timeout => "the timeout must be 500 to 60000 ms",
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
        "rtsp" | "rtsps" | "rtp" | "udp" | "tcp" | "http" | "https"
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
        StreamProtocol::Tcp | StreamProtocol::Udp | StreamProtocol::Rtp
    ) && port.is_none()
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
    let Some((_, rest)) = split_scheme(url) else {
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
    match before_fragment.split_once('?') {
        Some((path, query)) => {
            redacted.push_str(path);
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
        None => redacted.push_str(before_fragment),
    }
    if let Some(fragment) = fragment {
        redacted.push('#');
        redacted.push_str(fragment);
    }
    redacted
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
    if let Some((_, rest)) = split_scheme(url) {
        let (authority, tail) = split_authority(rest);
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
    /// The state's token: `idle`, `connecting`, `buffering`, `live`,
    /// `stalled`, `reconnecting`, or `failed`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
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
        ] {
            assert_eq!(validate_stream_url(url), Ok(protocol), "{url}");
        }
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
            "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls)"
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
}
