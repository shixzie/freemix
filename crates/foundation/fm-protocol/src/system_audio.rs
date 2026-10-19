//! System audio inputs (Protocol 2.55, AU-011).
//!
//! A system audio input hears a local audio capture source named by the
//! operator: a `PulseAudio`/`PipeWire` source (such as a sink's `.monitor`) or
//! an ALSA capture device. It is audio-only.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::AddInput`] | `input_add` | `input`, `name`, `kind=system_audio`, `backend` (`pulse\|alsa`), `device` | project edit |
//!
//! The add is an undoable project edit that honours the project padlock;
//! `event=input_added` carries `kind=system_audio`, `backend`, and
//! `device`. A daemon that cannot capture system audio on its platform
//! refuses the add with the typed reason `runtime_unsupported`.
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=system_audio_status` reports one input's [`SystemAudioStatus`]
//! whenever its state changes: `input`, `state`
//! (`idle|opening|live|reopening|failed`), `attempt` and `next_in_ms`
//! (only for `reopening`), `reopens`, and an optional `last_error`. A
//! snapshot carries `system_audio_inputs`: every system audio input,
//! ascending, with its config and latest status (without the error text).

pub use fm_types::system_audio::{
    MAX_SYSTEM_AUDIO_DEVICE_BYTES, MAX_SYSTEM_AUDIO_ERROR_BYTES, MAX_SYSTEM_AUDIO_INPUTS,
    SystemAudioBackend, SystemAudioConfig, SystemAudioError, SystemAudioState, SystemAudioStatus,
    bound_system_audio_error, validate_system_audio_device,
};

use crate::WireInputId;

/// One system audio input in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemAudioEntry {
    pub input: WireInputId,
    pub config: SystemAudioConfig,
    /// The latest status; a snapshot carries no error text.
    pub status: SystemAudioStatus,
}
