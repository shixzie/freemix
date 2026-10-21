//! The server's audio output devices (AU-001, Protocol 2.102).
//!
//! The headphone (monitor) output plays the monitor bus on one local audio
//! output device. An [`AudioOutputInventory`] lists the devices the server
//! can open there, so an operator picks one instead of typing its name.

use crate::system_audio::{bound_system_audio_description, bound_system_audio_error};

/// Most devices one output device list carries.
pub const MAX_AUDIO_OUTPUT_DEVICES: usize = 48;
/// The longest output device name, in bytes: the ALSA device bound a
/// monitor output device obeys.
pub const MAX_AUDIO_OUTPUT_DEVICE_BYTES: usize = crate::capture::MAX_ALSA_DEVICE_BYTES;
/// The longest listed device description, in bytes.
pub const MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES: usize =
    crate::system_audio::MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES;
/// The most channels a listed device reports.
pub const MAX_AUDIO_OUTPUT_CHANNELS: u8 = 64;
/// The longest listing error, in bytes.
pub const MAX_AUDIO_OUTPUT_ERROR_BYTES: usize = crate::system_audio::MAX_SYSTEM_AUDIO_ERROR_BYTES;

/// Whether `device` can name an output device: 1 to
/// [`MAX_AUDIO_OUTPUT_DEVICE_BYTES`] bytes of printable ASCII without
/// spaces, not starting with `-` (the monitor output device rule).
#[must_use]
pub fn audio_output_device_is_valid(device: &str) -> bool {
    !device.is_empty()
        && device.len() <= MAX_AUDIO_OUTPUT_DEVICE_BYTES
        && !device.starts_with('-')
        && device.bytes().all(|byte| byte.is_ascii_graphic())
}

/// One output device the server can play the monitor bus on.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AudioOutputDevice {
    /// The stable name the monitor output opens it by (an ALSA name such
    /// as `default` or `plughw:CARD=Headset,DEV=0`): it names the card by
    /// its id, so it survives the card being plugged into another port.
    pub device: String,
    /// What the audio system calls it, for people (may be empty).
    pub description: String,
    /// Its playback channel count, when the audio system reports one.
    pub channels: Option<u8>,
    /// The audio system's default output.
    pub default: bool,
}

impl AudioOutputDevice {
    /// Whether the device can travel as listed: a valid name, a bounded
    /// description, and a channel count from 1 to
    /// [`MAX_AUDIO_OUTPUT_CHANNELS`] when there is one.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        audio_output_device_is_valid(&self.device)
            && bound_system_audio_description(&self.description) == self.description
            && self
                .channels
                .is_none_or(|channels| (1..=MAX_AUDIO_OUTPUT_CHANNELS).contains(&channels))
    }
}

/// The output devices a server offers its headphone output.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AudioOutputInventory {
    /// The server plays the monitor bus on a device at all; when false
    /// nothing is listed.
    pub supported: bool,
    /// The devices, strictly ascending by name; at most one default.
    pub devices: Vec<AudioOutputDevice>,
    /// Why the devices could not be listed, if they could not: one line of
    /// 1 to [`MAX_AUDIO_OUTPUT_ERROR_BYTES`].
    pub error: Option<String>,
}

impl AudioOutputInventory {
    /// The inventory of a server without a headphone output device.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            supported: false,
            devices: Vec::new(),
            error: None,
        }
    }

    /// The inventory bounded to what one record carries: descriptions and
    /// the error bounded, unusable devices dropped, devices sorted with the
    /// first of equal names kept, at most one default (the first), and at
    /// most [`MAX_AUDIO_OUTPUT_DEVICES`] devices. An unsupported inventory
    /// lists nothing.
    #[must_use]
    pub fn fitted(&self) -> Self {
        if !self.supported {
            return Self::unsupported();
        }
        let mut devices: Vec<AudioOutputDevice> = self
            .devices
            .iter()
            .map(|device| AudioOutputDevice {
                description: bound_system_audio_description(&device.description),
                channels: device
                    .channels
                    .filter(|channels| (1..=MAX_AUDIO_OUTPUT_CHANNELS).contains(channels)),
                ..device.clone()
            })
            .filter(AudioOutputDevice::is_valid)
            .collect();
        devices.sort_by(|left, right| left.device.cmp(&right.device));
        devices.dedup_by(|later, earlier| later.device == earlier.device);
        devices.truncate(MAX_AUDIO_OUTPUT_DEVICES);
        let mut seen_default = false;
        for device in &mut devices {
            if device.default {
                device.default = !seen_default;
                seen_default = true;
            }
        }
        let error = self
            .error
            .as_deref()
            .map(bound_system_audio_error)
            .filter(|error| !error.is_empty());
        Self {
            supported: true,
            devices,
            error,
        }
    }

    /// The listed device named `device`, if it is listed.
    #[must_use]
    pub fn device(&self, device: &str) -> Option<&AudioOutputDevice> {
        self.devices.iter().find(|listed| listed.device == device)
    }

    /// The deterministic list a simulated server offers: the ALSA default,
    /// a stereo headset, and an eight-channel interface.
    #[must_use]
    pub fn simulated() -> Self {
        let device = |device: &str, description: &str, channels, default| AudioOutputDevice {
            device: device.into(),
            description: description.into(),
            channels,
            default,
        };
        Self {
            supported: true,
            devices: vec![
                device("default", "Default output", None, true),
                device(
                    "plughw:CARD=Headset,DEV=0",
                    "USB Headset: USB Audio",
                    Some(2),
                    false,
                ),
                device(
                    "plughw:CARD=Interface,DEV=0",
                    "Studio Interface: Monitor Out",
                    Some(8),
                    false,
                ),
            ],
            error: None,
        }
        .fitted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_follow_the_monitor_device_rule() {
        for device in ["default", "plughw:CARD=Headset,DEV=0", "hw:0,0", "null"] {
            assert!(audio_output_device_is_valid(device), "{device}");
        }
        let long = "a".repeat(MAX_AUDIO_OUTPUT_DEVICE_BYTES + 1);
        for device in ["", "-y", "with space", "ä", long.as_str()] {
            assert!(!audio_output_device_is_valid(device), "{device}");
        }
    }

    #[test]
    fn fitting_sorts_bounds_and_keeps_one_default() {
        let device = |device: &str, default| AudioOutputDevice {
            device: device.into(),
            description: "  Two\nlines ".into(),
            channels: Some(0),
            default,
        };
        let inventory = AudioOutputInventory {
            supported: true,
            devices: vec![
                device("plughw:CARD=B,DEV=0", true),
                device("default", true),
                device("bad name", false),
                device("default", false),
            ],
            error: Some(" \n ".into()),
        }
        .fitted();
        let names: Vec<&str> = inventory
            .devices
            .iter()
            .map(|device| device.device.as_str())
            .collect();
        assert_eq!(names, ["default", "plughw:CARD=B,DEV=0"]);
        assert!(inventory.devices[0].default);
        assert!(!inventory.devices[1].default);
        assert_eq!(inventory.devices[0].description, "Two lines");
        assert_eq!(inventory.devices[0].channels, None);
        assert_eq!(inventory.error, None);
        assert!(inventory.devices.iter().all(AudioOutputDevice::is_valid));
        assert_eq!(
            AudioOutputInventory {
                supported: false,
                devices: vec![device("default", true)],
                error: Some("x".into()),
            }
            .fitted(),
            AudioOutputInventory::unsupported()
        );
    }

    #[test]
    fn the_simulated_list_is_fitted_and_names_a_default() {
        let simulated = AudioOutputInventory::simulated();
        assert_eq!(simulated, simulated.fitted());
        assert_eq!(simulated.devices.len(), 3);
        assert_eq!(
            simulated.device("default").map(|device| device.default),
            Some(true)
        );
        assert_eq!(
            simulated
                .device("plughw:CARD=Interface,DEV=0")
                .and_then(|device| device.channels),
            Some(8)
        );
    }
}
