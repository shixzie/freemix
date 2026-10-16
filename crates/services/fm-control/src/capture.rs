//! Capture inputs between the engine and the wire (Protocol 2.47, IN-001).

use std::collections::BTreeMap;

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CaptureInputConfig, CaptureInputEntry, CaptureStatus, CommandPayload, EventPayload, WireInputId,
};
use fm_types::InputId;

/// The engine command for `input_capture_set`.
pub(super) fn engine_command(
    input: WireInputId,
    settings: &fm_protocol::CaptureSettings,
) -> EngineCommand {
    EngineCommand::SetCaptureSettings {
        input: input.to_domain(),
        settings: Box::new(settings.clone()),
    }
}

/// The wire payload of an undoable capture settings edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetCaptureSettings { input, settings } = command else {
        return None;
    };
    Some(CommandPayload::SetCaptureSettings {
        input: WireInputId::from_domain(*input),
        settings: settings.clone(),
    })
}

/// The durable event an accepted capture settings edit publishes.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::CaptureSettingsChanged { input, settings } = event else {
        return None;
    };
    Some(EventPayload::CaptureSettingsChanged {
        input: WireInputId::from_domain(*input),
        settings: settings.clone(),
    })
}

/// Every capture input, ascending, with its stable key, settings, and
/// latest status (without error text). An input nothing has reported on is
/// idle.
pub(super) fn wire_entries(
    show: &ShowState,
    states: &BTreeMap<InputId, CaptureStatus>,
) -> Vec<CaptureInputEntry> {
    show.capture_inputs()
        .map(|(input, stable_key, settings)| CaptureInputEntry {
            input: WireInputId::from_domain(input),
            config: CaptureInputConfig {
                stable_key: stable_key.to_owned(),
                settings: settings.clone(),
            },
            status: states
                .get(&input)
                .map(|status| CaptureStatus {
                    last_error: None,
                    ..status.clone()
                })
                .unwrap_or_default(),
        })
        .collect()
}
