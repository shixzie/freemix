//! The server's audio output devices (Protocol 2.102, AU-001).
//!
//! The headphone output plays the monitor bus on one local output device
//! the operator chooses (`audio_mixer` `set_monitor_device`). Protocol 2.102
//! lists the devices the server can open there, so an operator picks one
//! rather than type it. A client sends `audio_output_devices_request`
//! (`request_id`) and receives `audio_output_devices` with the same
//! `request_id`: `supported` (`0|1`, whether the server plays the monitor
//! bus on a device at all), an optional `error` (why the devices could not
//! be listed), and `devices` ([`AudioOutputInventory`]). The request needs
//! the `edit_project` permission, as choosing the device does. The server
//! lists off its frame loop and reuses a list younger than two seconds, so
//! it may answer only a peer's newest request.
//!
//! * `error`, when present, is one line of 1 to
//!   [`MAX_AUDIO_OUTPUT_ERROR_BYTES`] bytes.
//! * `devices` is a comma-separated list, strictly ascending by device
//!   name, of `<default>~<channels>~<device length>~<description length>:
//!   <device><description>` with a `0|1` default flag and `channels` either
//!   `-` or 1 to [`MAX_AUDIO_OUTPUT_CHANNELS`]; at most one default and
//!   [`MAX_AUDIO_OUTPUT_DEVICES`] devices. The device is a valid monitor
//!   output device name and the texts are written raw after their
//!   canonical byte lengths.
//!
//! A server that plays no device answers `supported=0` with nothing listed.

pub use fm_types::audio_output::{
    AudioOutputDevice, AudioOutputInventory, MAX_AUDIO_OUTPUT_CHANNELS,
    MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES, MAX_AUDIO_OUTPUT_DEVICE_BYTES, MAX_AUDIO_OUTPUT_DEVICES,
    MAX_AUDIO_OUTPUT_ERROR_BYTES, audio_output_device_is_valid,
};

/// A client's request for the server's audio output devices (Protocol
/// 2.102).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioOutputDevicesRequest {
    pub request_id: String,
}

/// The server's audio output devices, answering an
/// [`AudioOutputDevicesRequest`] with the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioOutputDevicesMessage {
    pub request_id: String,
    pub inventory: AudioOutputInventory,
}
