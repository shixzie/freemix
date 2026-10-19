//! System audio inputs between the engine and the wire (Protocol 2.55,
//! AU-011).

use std::collections::BTreeMap;

use fm_engine::ShowState;
use fm_protocol::{SystemAudioEntry, SystemAudioStatus, WireInputId};
use fm_types::InputId;

/// Every system audio input, ascending, with its device and latest status
/// (without error text). An input nothing has reported on is idle.
pub(super) fn wire_entries(
    show: &ShowState,
    states: &BTreeMap<InputId, SystemAudioStatus>,
) -> Vec<SystemAudioEntry> {
    show.system_audio_inputs()
        .map(|(input, config)| SystemAudioEntry {
            input: WireInputId::from_domain(input),
            config: config.clone(),
            status: states
                .get(&input)
                .map(|status| SystemAudioStatus {
                    last_error: None,
                    ..status.clone()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// `status` bounded for the wire: its error one line of at most
/// [`fm_protocol::MAX_SYSTEM_AUDIO_ERROR_BYTES`] bytes, and an empty error
/// dropped.
pub(super) fn wire_status(status: &SystemAudioStatus) -> SystemAudioStatus {
    SystemAudioStatus {
        last_error: status
            .last_error
            .as_deref()
            .map(fm_protocol::bound_system_audio_error)
            .filter(|error| !error.is_empty()),
        ..status.clone()
    }
}
