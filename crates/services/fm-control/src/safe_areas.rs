//! Operator safe-area guides between the engine and the wire (SW-014).

use fm_engine::{EngineCommand, EngineEvent};
use fm_model::SafeAreaGuideSelection;
use fm_protocol::{CommandPayload, EventPayload, WireSafeAreaGuides};

/// The wire form of a model selection.
pub(super) const fn wire(guides: SafeAreaGuideSelection) -> WireSafeAreaGuides {
    WireSafeAreaGuides {
        action: guides.action,
        title: guides.title,
        center_4_3: guides.center_4_3,
        center_16_9: guides.center_16_9,
        center_9_16: guides.center_9_16,
    }
}

/// The model form of a wire selection.
pub(super) const fn model(guides: WireSafeAreaGuides) -> SafeAreaGuideSelection {
    SafeAreaGuideSelection {
        action: guides.action,
        title: guides.title,
        center_4_3: guides.center_4_3,
        center_16_9: guides.center_16_9,
        center_9_16: guides.center_9_16,
    }
}

/// The engine command of a safe-area payload.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    let CommandPayload::SetSafeAreaGuides { guides } = payload else {
        return None;
    };
    Some(EngineCommand::SetSafeAreaGuides {
        guides: model(*guides),
    })
}

/// The durable event an accepted safe-area edit publishes, including through
/// undo and redo.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::SafeAreaGuidesChanged { guides } = event else {
        return None;
    };
    Some(EventPayload::SafeAreaGuidesChanged {
        guides: wire(*guides),
    })
}

/// The wire payload that reproduces `command`, for an undo or redo event.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetSafeAreaGuides { guides } = command else {
        return None;
    };
    Some(CommandPayload::SetSafeAreaGuides {
        guides: wire(*guides),
    })
}
