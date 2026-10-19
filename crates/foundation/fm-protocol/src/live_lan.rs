//! `LiveLAN` viewer stream (Protocol 2.57, OR-017).
//!
//! `LiveLAN` lets people on the local network watch Program, or one chosen
//! project output, in a browser: the server encodes the picture as a
//! low-latency HLS stream and serves a viewer page next to it, optionally
//! behind an access PIN.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::SetLiveLan`] | `live_lan_set` | `enabled` (`0\|1`), `source` (`program\|output:<id>`), `max_viewers` (`1..=32`), optional `pin` | project edit |
//!
//! The edit is padlocked and undoable. `pin` is a secret edit: absent keeps
//! the stored PIN, empty clears it, `***` is refused, and a set PIN is 4 to
//! 12 ASCII digits. It is announced by the durable `event=live_lan_changed`
//! carrying `enabled`, `source`, `max_viewers`, and `has_pin` (`0|1`).
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=live_lan_status` reports the [`LiveLanStatus`] whenever it
//! changes: `state` (`off|unavailable|starting|live|failed`), `url` (the
//! viewer page while it is served, otherwise empty), `viewers`, and
//! `egress_kbps`. A snapshot carries `live_lan`
//! (`<enabled>~<source>~<max_viewers>~<has_pin>`) and `live_lan_status`
//! (`<state>~<viewers>~<egress_kbps>~<url>`).
//!
//! No command echo, event, snapshot, or refusal ever carries a stored PIN.

pub use fm_types::live_lan::{
    DEFAULT_LIVE_LAN_VIEWERS, LiveLanConfig, LiveLanPin, LiveLanPinError, LiveLanSource,
    LiveLanState, LiveLanStatus, MAX_LIVE_LAN_PIN_DIGITS, MAX_LIVE_LAN_URL_BYTES,
    MAX_LIVE_LAN_VIEWERS, MIN_LIVE_LAN_PIN_DIGITS, MIN_LIVE_LAN_VIEWERS, live_lan_pin_is_valid,
    live_lan_url_is_valid,
};

/// The `LiveLAN` configuration as clients see it: whether a PIN is stored,
/// never the PIN itself.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LiveLanEntry {
    pub enabled: bool,
    pub source: LiveLanSource,
    pub max_viewers: u8,
    pub has_pin: bool,
}

impl Default for LiveLanEntry {
    fn default() -> Self {
        Self::from_config(&LiveLanConfig::default())
    }
}

impl LiveLanEntry {
    /// The client view of `config`.
    #[must_use]
    pub fn from_config(config: &LiveLanConfig) -> Self {
        Self {
            enabled: config.enabled,
            source: config.source,
            max_viewers: config.max_viewers,
            has_pin: config.pin.is_some(),
        }
    }
}
