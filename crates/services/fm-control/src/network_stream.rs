//! Network stream inputs between the engine and the wire (Protocol 2.46,
//! IN-015; 2.49, IN-016/IN-017).
//!
//! Everything this module reports carries the redacted URL only, and masks
//! the RTMP stream key and SRT passphrase.

use std::collections::BTreeMap;

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, EventPayload, NetworkStreamConfig, NetworkStreamEntry, NetworkStreamSettings,
    NetworkStreamStatus, SecretEdit, StreamUrl, WireInputId,
};
use fm_types::InputId;

/// An `input_stream_set` edit's fields besides its input.
pub(super) struct StreamEdit<'a> {
    pub url: Option<&'a StreamUrl>,
    pub settings: &'a NetworkStreamSettings,
    pub stream_id: Option<&'a str>,
    pub stream_key: &'a SecretEdit,
    pub passphrase: &'a SecretEdit,
}

/// The engine command for `input_stream_set`: the new URL, or the input's
/// current one when the edit keeps it, and the stream key and passphrase
/// the edit keeps, clears, or replaces (IN-016, IN-017). An input that is
/// not a network stream keeps an empty URL, which the engine refuses with
/// the reason.
pub(super) fn engine_command(
    show: &ShowState,
    input: WireInputId,
    edit: &StreamEdit<'_>,
) -> EngineCommand {
    let input = input.to_domain();
    let current = show.network_stream(input);
    let url = edit.url.map_or_else(
        || current.map(|config| config.url.clone()).unwrap_or_default(),
        |url| url.as_str().to_owned(),
    );
    EngineCommand::SetNetworkStream {
        input,
        config: Box::new(NetworkStreamConfig {
            url,
            settings: *edit.settings,
            stream_id: edit.stream_id.map(str::to_owned),
            stream_key: edit
                .stream_key
                .apply(current.and_then(|config| config.stream_key.as_deref())),
            passphrase: edit
                .passphrase
                .apply(current.and_then(|config| config.passphrase.as_deref())),
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
        stream_id: config.stream_id.clone(),
        stream_key: SecretEdit::exact(config.stream_key.as_deref()),
        passphrase: SecretEdit::exact(config.passphrase.as_deref()),
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
