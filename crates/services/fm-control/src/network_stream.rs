//! Network stream inputs between the engine and the wire (Protocol 2.46,
//! IN-015).
//!
//! Everything this module reports carries the redacted URL only.

use std::collections::BTreeMap;

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, EventPayload, NetworkStreamConfig, NetworkStreamEntry, NetworkStreamSettings,
    NetworkStreamStatus, StreamUrl, WireInputId,
};
use fm_types::InputId;

/// The engine command for `input_stream_set`: the new URL, or the input's
/// current one when the edit keeps it. An input that is not a network
/// stream keeps an empty URL, which the engine refuses with the reason.
pub(super) fn engine_command(
    show: &ShowState,
    input: WireInputId,
    url: Option<&StreamUrl>,
    settings: &NetworkStreamSettings,
) -> EngineCommand {
    let input = input.to_domain();
    let url = url.map_or_else(
        || {
            show.network_stream(input)
                .map(|config| config.url.clone())
                .unwrap_or_default()
        },
        |url| url.as_str().to_owned(),
    );
    EngineCommand::SetNetworkStream {
        input,
        config: Box::new(NetworkStreamConfig {
            url,
            settings: *settings,
        }),
    }
}

/// The wire payload of an undoable network stream edit (its exact URL,
/// for the journal and history).
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetNetworkStream { input, config } = command else {
        return None;
    };
    Some(CommandPayload::SetNetworkStream {
        input: WireInputId::from_domain(*input),
        url: Some(StreamUrl::new(config.url.clone())),
        settings: config.settings,
    })
}

/// The durable event an accepted network stream edit publishes, with its
/// URL redacted.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::NetworkStreamChanged { input, config } = event else {
        return None;
    };
    Some(EventPayload::NetworkStreamChanged {
        input: WireInputId::from_domain(*input),
        config: Box::new(config.redacted()),
    })
}

/// Every network stream input, ascending, with its redacted config and
/// latest status (without error text). An input nothing has reported on is
/// idle.
pub(super) fn wire_entries(
    show: &ShowState,
    states: &BTreeMap<InputId, NetworkStreamStatus>,
) -> Vec<NetworkStreamEntry> {
    show.network_streams()
        .map(|(input, config)| NetworkStreamEntry {
            input: WireInputId::from_domain(input),
            config: config.redacted(),
            status: states
                .get(&input)
                .map(|status| NetworkStreamStatus {
                    last_error: None,
                    ..status.clone()
                })
                .unwrap_or_default(),
        })
        .collect()
}
