//! Per-input freeze between the engine and the wire (SW-016).

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{CommandPayload, EventPayload, WireInputId};

/// The engine command of an input-freeze payload.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    let CommandPayload::SetInputFreeze { input, frozen } = payload else {
        return None;
    };
    Some(EngineCommand::SetInputFreeze {
        input: input.to_domain(),
        frozen: *frozen,
    })
}

/// The durable event of a freeze change.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::InputFreezeChanged { input, frozen } = event else {
        return None;
    };
    Some(EventPayload::InputFreezeChanged {
        input: WireInputId::from_domain(*input),
        frozen: *frozen,
    })
}

/// Every frozen input in ascending order.
pub(super) fn wire_entries(show: &ShowState) -> Vec<WireInputId> {
    show.frozen_inputs().map(WireInputId::from_domain).collect()
}
