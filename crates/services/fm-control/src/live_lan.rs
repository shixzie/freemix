//! The `LiveLAN` viewer stream between the engine and the wire (Protocol
//! 2.57, OR-017).
//!
//! Everything this module reports carries the configuration without its
//! PIN: only whether one is stored.

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, EventPayload, LiveLanConfig, LiveLanEntry, LiveLanPin, LiveLanStatus,
    SecretEdit,
};

/// The engine command for a wire `LiveLAN` edit, if `payload` is one. The PIN
/// edit resolves against the stored PIN; the codec already refused a PIN it
/// cannot store.
pub(super) fn engine_command(show: &ShowState, payload: &CommandPayload) -> Option<EngineCommand> {
    let CommandPayload::SetLiveLan { config, pin } = payload else {
        return None;
    };
    let stored = show.live_lan().pin.clone();
    let pin = match pin {
        SecretEdit::Keep => stored,
        SecretEdit::Clear => None,
        // The codec refuses an unusable PIN; one that reaches here in
        // process keeps the stored PIN rather than silently opening access.
        SecretEdit::Set(pin) => LiveLanPin::parse(pin).ok().or(stored),
    };
    Some(EngineCommand::SetLiveLan(Box::new(LiveLanConfig {
        enabled: config.enabled,
        source: config.source,
        max_viewers: config.max_viewers,
        pin,
    })))
}

/// The wire payload of an undoable `LiveLAN` edit, with its exact PIN for the
/// journal and history.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetLiveLan(config) = command else {
        return None;
    };
    Some(CommandPayload::SetLiveLan {
        config: LiveLanEntry::from_config(config),
        pin: SecretEdit::exact(config.pin.as_ref().map(LiveLanPin::expose_secret)),
    })
}

/// The durable event an accepted `LiveLAN` edit publishes, without its PIN.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::LiveLanChanged(config) = event else {
        return None;
    };
    Some(EventPayload::LiveLanChanged(LiveLanEntry::from_config(
        config,
    )))
}

/// The configuration as clients see it.
pub(super) fn wire_entry(show: &ShowState) -> LiveLanEntry {
    LiveLanEntry::from_config(show.live_lan())
}

/// A reported status as clients see it (see
/// [`LiveLanStatus::for_config`]).
pub(super) fn wire_status(show: &ShowState, status: &LiveLanStatus) -> LiveLanStatus {
    status.for_config(show.live_lan().enabled)
}
