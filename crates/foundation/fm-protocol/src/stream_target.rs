//! Configured stream destinations on the wire (Protocols 2.52, 2.54, and
//! 2.122, OR-006, OR-009).
//!
//! A destination's authored configuration travels without its stream key:
//! [`StreamTargetEntry::has_key`] says only whether one is stored, and an
//! edit ([`crate::CommandPayload::SetStreamTarget`]) keeps, clears, or
//! replaces it through a [`crate::SecretEdit`], exactly like a network
//! input's secrets. Since 2.54 an SRT destination also carries its send
//! latency and an optional encryption passphrase, which is a secret handled
//! exactly like the key ([`StreamTargetEntry::has_passphrase`]). No
//! snapshot, event, status line, or error ever carries a key or passphrase.
//!
//! The wire restates the authoring bounds of `fm_model::StreamTarget` (which
//! this foundation crate cannot depend on) so that an out-of-range value is
//! refused at the codec instead of at the engine; the engine still validates
//! every edit against the model.

use crate::{WireOutputId, WireStreamTargetId};

/// Most configured destinations one show may carry.
pub const MAX_STREAM_TARGETS: usize = 5;
/// Longest destination name in bytes.
pub const MAX_STREAM_TARGET_NAME_BYTES: usize = 128;
/// Longest scheme-relative endpoint in bytes.
pub const MAX_STREAM_TARGET_ENDPOINT_BYTES: usize = 1_024;
/// Longest stream key in bytes.
pub const MAX_STREAM_TARGET_KEY_BYTES: usize = 512;
/// Lowest authored video bitrate in kilobits per second.
pub const MIN_STREAM_TARGET_BITRATE_KBPS: u32 = 1_000;
/// Highest authored video bitrate in kilobits per second.
pub const MAX_STREAM_TARGET_BITRATE_KBPS: u32 = 100_000;
/// Shortest authored reconnect delay in milliseconds.
pub const MIN_STREAM_RECONNECT_DELAY_MS: u32 = 100;
/// Longest authored reconnect delay in milliseconds.
pub const MAX_STREAM_RECONNECT_DELAY_MS: u32 = 300_000;
/// Most authored reconnect attempts before a destination gives up.
pub const MAX_STREAM_RECONNECT_ATTEMPTS: u32 = 10_000;
/// Most rungs an authored HLS ladder carries.
pub const MAX_STREAM_TARGET_LADDER_RUNGS: u8 = 8;
/// Lowest SRT send latency in milliseconds (2.54).
pub const MIN_STREAM_TARGET_SRT_LATENCY_MS: u32 = 20;
/// Highest SRT send latency in milliseconds (2.54).
pub const MAX_STREAM_TARGET_SRT_LATENCY_MS: u32 = 8_000;
/// The SRT send latency a new SRT destination starts with (2.54).
pub const DEFAULT_STREAM_TARGET_SRT_LATENCY_MS: u32 = 120;
/// Shortest SRT passphrase in bytes (2.54).
pub const MIN_STREAM_TARGET_PASSPHRASE_BYTES: usize = 10;
/// Longest SRT passphrase in bytes (2.54).
pub const MAX_STREAM_TARGET_PASSPHRASE_BYTES: usize = 79;

/// The transport a destination publishes over.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StreamTargetProtocol {
    Rtmp,
    Rtmps,
    Srt,
    /// Local HLS segments under an absolute directory; carries no key.
    Hls,
    /// An Open Media Transport sender (`omt://host:port`); carries no key.
    Omt,
}

impl StreamTargetProtocol {
    pub const ALL: [Self; 5] = [Self::Rtmp, Self::Rtmps, Self::Srt, Self::Hls, Self::Omt];

    /// The wire token, which is also the URL scheme.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Rtmps => "rtmps",
            Self::Srt => "srt",
            Self::Hls => "hls",
            Self::Omt => "omt",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|protocol| protocol.token() == token)
    }

    /// Whether the protocol requires a stream key (every network protocol).
    #[must_use]
    pub const fn carries_key(self) -> bool {
        matches!(self, Self::Rtmp | Self::Rtmps | Self::Srt)
    }
}

/// How a destination reconnects: the first retry waits `initial_delay_ms`,
/// each later one doubles up to `max_delay_ms`, and after `max_attempts`
/// undelivered attempts the destination gives up (`None` retries forever).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StreamReconnect {
    pub initial_delay_ms: u32,
    pub max_delay_ms: u32,
    pub max_attempts: Option<u32>,
}

impl StreamReconnect {
    /// The default policy: 250 ms doubling to 2 s, giving up after 40.
    pub const DEFAULT: Self = Self {
        initial_delay_ms: 250,
        max_delay_ms: 2_000,
        max_attempts: Some(40),
    };

    /// Whether every bound holds.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        const fn delay(ms: u32) -> bool {
            ms >= MIN_STREAM_RECONNECT_DELAY_MS && ms <= MAX_STREAM_RECONNECT_DELAY_MS
        }
        delay(self.initial_delay_ms)
            && delay(self.max_delay_ms)
            && self.max_delay_ms >= self.initial_delay_ms
            && match self.max_attempts {
                Some(attempts) => attempts >= 1 && attempts <= MAX_STREAM_RECONNECT_ATTEMPTS,
                None => true,
            }
    }
}

impl Default for StreamReconnect {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One destination's authored configuration, without its stream key.
///
/// `endpoint` is scheme-relative: `host[:port]/application[/path]` for
/// RTMP/RTMPS, `host[:port]` for SRT, an absolute directory for HLS, and
/// the listening `host:port` for OMT.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StreamTargetConfig {
    pub name: String,
    pub protocol: StreamTargetProtocol,
    pub endpoint: String,
    pub backup_endpoint: Option<String>,
    pub output: WireOutputId,
    /// Whether the destination resumes its desired running state when the
    /// engine starts (`reconcile`), or always starts stopped (`stopped`).
    pub reconcile_on_startup: bool,
    pub video_bitrate_kbps: u32,
    pub reconnect: StreamReconnect,
    /// The SRT send latency in milliseconds: `Some` exactly for SRT (2.54).
    pub srt_latency_ms: Option<u32>,
}

fn endpoint_is_valid(endpoint: &str) -> bool {
    !endpoint.is_empty()
        && endpoint.len() <= MAX_STREAM_TARGET_ENDPOINT_BYTES
        && endpoint.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

impl StreamTargetConfig {
    /// Whether every wire bound holds (the engine validates the rest).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.len() <= MAX_STREAM_TARGET_NAME_BYTES
            && endpoint_is_valid(&self.endpoint)
            && self
                .backup_endpoint
                .as_deref()
                .is_none_or(endpoint_is_valid)
            && (MIN_STREAM_TARGET_BITRATE_KBPS..=MAX_STREAM_TARGET_BITRATE_KBPS)
                .contains(&self.video_bitrate_kbps)
            && self.reconnect.is_valid()
            && match self.srt_latency_ms {
                Some(latency) => {
                    self.protocol == StreamTargetProtocol::Srt
                        && (MIN_STREAM_TARGET_SRT_LATENCY_MS..=MAX_STREAM_TARGET_SRT_LATENCY_MS)
                            .contains(&latency)
                }
                None => self.protocol != StreamTargetProtocol::Srt,
            }
    }

    /// The destination URL without its key, as an operator may see it.
    #[must_use]
    pub fn display_url(&self) -> String {
        format!("{}://{}", self.protocol.token(), self.endpoint)
    }
}

/// Whether `key` is acceptable stream-key text on the wire: printable ASCII
/// without whitespace, within [`MAX_STREAM_TARGET_KEY_BYTES`].
#[must_use]
pub fn stream_key_is_valid(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_STREAM_TARGET_KEY_BYTES
        && key.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

/// Whether `passphrase` is acceptable SRT passphrase text on the wire:
/// printable ASCII without whitespace, [`MIN_STREAM_TARGET_PASSPHRASE_BYTES`]
/// to [`MAX_STREAM_TARGET_PASSPHRASE_BYTES`] (2.54).
#[must_use]
pub fn srt_passphrase_is_valid(passphrase: &str) -> bool {
    (MIN_STREAM_TARGET_PASSPHRASE_BYTES..=MAX_STREAM_TARGET_PASSPHRASE_BYTES)
        .contains(&passphrase.len())
        && passphrase.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

/// One configured destination as clients see it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StreamTargetEntry {
    pub target: WireStreamTargetId,
    pub config: StreamTargetConfig,
    /// Whether a stream key is stored; the key itself never travels.
    pub has_key: bool,
    /// Rungs of the authored HLS ladder, `0` without one. A ladder is
    /// authored offline; an edit keeps it while the protocol stays HLS.
    pub ladder_rungs: u8,
    /// Whether an SRT passphrase is stored; the passphrase never travels
    /// (2.54).
    pub has_passphrase: bool,
}
