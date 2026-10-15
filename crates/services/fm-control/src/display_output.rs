//! Fullscreen display outputs between the engine and the wire (Protocol
//! 2.42, OR-011).

use std::collections::BTreeMap;

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, DisplayOutputEntry, DisplayOutputSlot, DisplayOutputStatus, EventPayload,
    WaitReason, WireDisplayOutputState,
};

/// The engine command for a wire display-output edit, if `payload` is one.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    match payload {
        CommandPayload::SetDisplayOutput { slot, config } => {
            Some(EngineCommand::SetDisplayOutput {
                slot: *slot,
                config: Some(config.clone()),
            })
        }
        CommandPayload::ClearDisplayOutput { slot } => Some(EngineCommand::SetDisplayOutput {
            slot: *slot,
            config: None,
        }),
        _ => None,
    }
}

/// The wire payload of an undoable display-output edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetDisplayOutput { slot, config } = command else {
        return None;
    };
    Some(match config {
        Some(config) => CommandPayload::SetDisplayOutput {
            slot: *slot,
            config: config.clone(),
        },
        None => CommandPayload::ClearDisplayOutput { slot: *slot },
    })
}

/// The durable event an accepted display-output edit publishes, if `event`
/// is one.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::DisplayOutputChanged { slot, config } = event else {
        return None;
    };
    Some(EventPayload::DisplayOutputChanged {
        slot: *slot,
        config: config.clone(),
    })
}

/// Every configured slot, ascending.
pub(super) fn wire_entries(show: &ShowState) -> Vec<DisplayOutputEntry> {
    show.display_outputs()
        .iter()
        .map(|(slot, config)| DisplayOutputEntry {
            slot: *slot,
            config: config.clone(),
        })
        .collect()
}

/// Every configured slot's runtime state, ascending. A slot no presenter
/// has reported on waits for one.
pub(super) fn wire_states(
    show: &ShowState,
    states: &BTreeMap<DisplayOutputSlot, WireDisplayOutputState>,
) -> Vec<DisplayOutputStatus> {
    show.display_outputs()
        .keys()
        .map(|slot| DisplayOutputStatus {
            slot: *slot,
            state: states
                .get(slot)
                .cloned()
                .unwrap_or(WireDisplayOutputState::Waiting(WaitReason::NoPresenter)),
        })
        .collect()
}
