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
//!
//! Protocol 2.68 lists the server's capture devices so an operator can
//! pick one rather than type it. A client sends
//! `system_audio_devices_request` (`request_id`) and receives
//! `system_audio_devices` with the same `request_id`: `supported` (`0|1`,
//! whether the server captures system audio at all), `errors`, and
//! `devices` ([`SystemAudioInventory`]). The server lists its devices off
//! its frame loop and reuses a list younger than two seconds, so it may
//! answer only a peer's newest request.
//!
//! * `errors` is a comma-separated list, strictly ascending by audio
//!   system, of `<backend>~<length>:<text>`: an audio system that could
//!   not be listed and why.
//! * `devices` is a comma-separated list, strictly ascending by audio
//!   system and then device name, of
//!   `<backend>~<default>~<monitor>~<channels>~<device length>~
//!   <description length>:<device><description>` with `0|1` flags and
//!   `channels` either `-` or 1 to [`MAX_SYSTEM_AUDIO_DEVICE_CHANNELS`]; at
//!   most one default per audio system and [`MAX_SYSTEM_AUDIO_DEVICES`]
//!   devices. The texts are written raw after their canonical byte
//!   lengths.
//!
//! A server that does not capture system audio answers `supported=0` with
//! empty lists.

pub use fm_types::system_audio::{
    MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES, MAX_SYSTEM_AUDIO_DEVICE_BYTES,
    MAX_SYSTEM_AUDIO_DEVICE_CHANNELS, MAX_SYSTEM_AUDIO_DEVICES, MAX_SYSTEM_AUDIO_ERROR_BYTES,
    MAX_SYSTEM_AUDIO_INPUTS, SystemAudioBackend, SystemAudioBackendError, SystemAudioConfig,
    SystemAudioDevice, SystemAudioError, SystemAudioInventory, SystemAudioState, SystemAudioStatus,
    bound_system_audio_description, bound_system_audio_error, default_system_audio_device,
    validate_system_audio_device,
};

use crate::WireInputId;

/// A client's request for the server's system audio capture devices
/// (Protocol 2.68).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemAudioDevicesRequest {
    pub request_id: String,
}

/// The server's system audio capture devices, answering a
/// [`SystemAudioDevicesRequest`] with the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemAudioDevicesMessage {
    pub request_id: String,
    pub inventory: SystemAudioInventory,
}

/// One system audio input in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemAudioEntry {
    pub input: WireInputId,
    pub config: SystemAudioConfig,
    /// The latest status; a snapshot carries no error text.
    pub status: SystemAudioStatus,
}
