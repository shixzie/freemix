//! System audio input value types (AU-011).
//!
//! A system audio input hears a local audio capture source: on Linux, a
//! `PulseAudio` (or `PipeWire` `pipewire-pulse`) source such as a sink's
//! `.monitor` (what applications play) or `@DEFAULT_MONITOR@`, or an ALSA
//! capture device such as a `snd-aloop` loopback (`hw:Loopback,1`). The
//! operator names the device, or picks it from the [`SystemAudioInventory`]
//! the server lists (Protocol 2.68). The input is
//! audio-only: it has no picture and its Master strip carries what the
//! device hears.
//!
//! The runtime side reports each input's [`SystemAudioStatus`]: its
//! [`SystemAudioState`] (opening, live, reopening with the attempt and the
//! delay before it, or failed), the reopen count, and the last error.

use std::fmt;

/// Most system audio inputs one show holds.
pub const MAX_SYSTEM_AUDIO_INPUTS: usize = 8;
/// The longest device name, in bytes. Device names are printable ASCII.
pub const MAX_SYSTEM_AUDIO_DEVICE_BYTES: usize = 128;
/// The longest status error text, in bytes.
pub const MAX_SYSTEM_AUDIO_ERROR_BYTES: usize = 160;

/// The audio system a system audio input opens its device through.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemAudioBackend {
    /// A `PulseAudio` source (`PipeWire` serves the same API through
    /// `pipewire-pulse`), such as `<sink>.monitor` or `@DEFAULT_MONITOR@`.
    #[default]
    Pulse,
    /// An ALSA capture device, such as `hw:Loopback,1` or `default`.
    Alsa,
}

impl SystemAudioBackend {
    pub const ALL: [Self; 2] = [Self::Pulse, Self::Alsa];

    /// The backend's token: `pulse` or `alsa`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Pulse => "pulse",
            Self::Alsa => "alsa",
        }
    }

    /// The backend a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|backend| backend.token() == token)
    }
}

/// One system audio input's device.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SystemAudioConfig {
    pub backend: SystemAudioBackend,
    /// The device (`PulseAudio` source or ALSA PCM) name.
    pub device: String,
}

impl SystemAudioConfig {
    #[must_use]
    pub fn new(backend: SystemAudioBackend, device: impl Into<String>) -> Self {
        Self {
            backend,
            device: device.into(),
        }
    }

    /// Checks the device name.
    ///
    /// # Errors
    ///
    /// Returns [`SystemAudioError::InvalidDevice`] for an empty or too long
    /// name, one that is not printable ASCII, or one that starts with `-`
    /// or a space.
    pub fn validate(&self) -> Result<(), SystemAudioError> {
        validate_system_audio_device(&self.device)
    }
}

/// Checks a system audio device name: 1 to
/// [`MAX_SYSTEM_AUDIO_DEVICE_BYTES`] printable ASCII characters (spaces
/// allowed inside), not starting with `-` or a space and not ending with a
/// space.
///
/// # Errors
///
/// Returns [`SystemAudioError::InvalidDevice`] when the name breaks a rule.
pub fn validate_system_audio_device(device: &str) -> Result<(), SystemAudioError> {
    let valid = !device.is_empty()
        && device.len() <= MAX_SYSTEM_AUDIO_DEVICE_BYTES
        && device.bytes().all(|byte| (0x20..0x7f).contains(&byte))
        && !device.starts_with(['-', ' '])
        && !device.ends_with(' ');
    if valid {
        Ok(())
    } else {
        Err(SystemAudioError::InvalidDevice)
    }
}

/// Why a system audio configuration is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SystemAudioError {
    /// The device name is empty, too long, not printable ASCII, or starts
    /// with `-` or a space.
    InvalidDevice,
}

impl SystemAudioError {
    /// The stable code of the error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidDevice => "invalid_device",
        }
    }

    /// The field the error is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidDevice => "device",
        }
    }
}

impl fmt::Display for SystemAudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDevice => {
                "a system audio device name is 1 to 128 printable characters and does not start \
                 with '-' or a space"
            }
        })
    }
}

impl std::error::Error for SystemAudioError {}

/// A system audio input's capture state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SystemAudioState {
    /// Nothing captures the device: the input is closed, or the daemon does
    /// not realize system audio inputs.
    #[default]
    Idle,
    /// The device is being opened.
    Opening,
    /// The device delivers audio.
    Live,
    /// The device stopped delivering or failed to open: waiting
    /// `next_in_ms` before reopen attempt `attempt` (one-based).
    Reopening { attempt: u32, next_in_ms: u32 },
    /// The device cannot be captured here (its backend is unavailable on
    /// this platform); it is not reopened.
    Failed,
}

impl SystemAudioState {
    /// The state's token: `idle`, `opening`, `live`, `reopening`, or
    /// `failed`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Opening => "opening",
            Self::Live => "live",
            Self::Reopening { .. } => "reopening",
            Self::Failed => "failed",
        }
    }
}

/// A system audio input's capture status.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SystemAudioStatus {
    pub state: SystemAudioState,
    /// Reopen attempts made since the input opened.
    pub reopens: u32,
    /// The last error, bounded to [`MAX_SYSTEM_AUDIO_ERROR_BYTES`].
    pub last_error: Option<String>,
}

/// `text` trimmed to one line of at most [`MAX_SYSTEM_AUDIO_ERROR_BYTES`]
/// bytes, control characters replaced by spaces.
#[must_use]
pub fn bound_system_audio_error(text: &str) -> String {
    let mut bounded: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if bounded.len() > MAX_SYSTEM_AUDIO_ERROR_BYTES {
        let mut end = MAX_SYSTEM_AUDIO_ERROR_BYTES;
        while !bounded.is_char_boundary(end) {
            end -= 1;
        }
        bounded.truncate(end);
    }
    bounded
}

/// Most devices one system audio device list carries (Protocol 2.68).
pub const MAX_SYSTEM_AUDIO_DEVICES: usize = 48;
/// The longest listed device description, in bytes.
pub const MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES: usize = 96;
/// The most channels a listed device reports.
pub const MAX_SYSTEM_AUDIO_DEVICE_CHANNELS: u8 = 64;

/// The device name each audio system calls its default: `PulseAudio`'s
/// monitor of the default output (what the server plays), and ALSA's
/// `default` PCM.
#[must_use]
pub const fn default_system_audio_device(backend: SystemAudioBackend) -> &'static str {
    match backend {
        SystemAudioBackend::Pulse => "@DEFAULT_MONITOR@",
        SystemAudioBackend::Alsa => "default",
    }
}

/// `text` as a listed device description: one trimmed line of at most
/// [`MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES`], control characters replaced by
/// spaces.
#[must_use]
pub fn bound_system_audio_description(text: &str) -> String {
    let mut bounded: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if bounded.len() > MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES {
        let mut end = MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES;
        while !bounded.is_char_boundary(end) {
            end -= 1;
        }
        bounded.truncate(end);
        bounded.truncate(bounded.trim_end().len());
    }
    bounded
}

/// One capture device the server's audio system offers (Protocol 2.68).
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SystemAudioDevice {
    pub backend: SystemAudioBackend,
    /// The name a system audio input opens it by (see
    /// [`validate_system_audio_device`]).
    pub device: String,
    /// What the audio system calls it, for people (may be empty).
    pub description: String,
    /// Its channel count, when the audio system reports one.
    pub channels: Option<u8>,
    /// The audio system's default capture device.
    pub default: bool,
    /// It hears an output (a `PulseAudio` sink's monitor) rather than an
    /// input.
    pub monitor: bool,
}

impl SystemAudioDevice {
    /// Whether the device can travel as listed: a valid name, a bounded
    /// description, and a channel count from 1 to
    /// [`MAX_SYSTEM_AUDIO_DEVICE_CHANNELS`] when there is one.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        validate_system_audio_device(&self.device).is_ok()
            && bound_system_audio_description(&self.description) == self.description
            && self
                .channels
                .is_none_or(|channels| (1..=MAX_SYSTEM_AUDIO_DEVICE_CHANNELS).contains(&channels))
    }
}

/// Why one audio system could not be listed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SystemAudioBackendError {
    pub backend: SystemAudioBackend,
    /// One line of 1 to [`MAX_SYSTEM_AUDIO_ERROR_BYTES`].
    pub error: String,
}

/// The capture devices a server's audio systems offer (Protocol 2.68).
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct SystemAudioInventory {
    /// The server captures system audio at all; when false nothing is
    /// listed.
    pub supported: bool,
    /// The devices, ascending by audio system and then name, each named
    /// once; at most one default per audio system.
    pub devices: Vec<SystemAudioDevice>,
    /// The audio systems that could not be listed, ascending, each once.
    pub errors: Vec<SystemAudioBackendError>,
}

impl SystemAudioInventory {
    /// The inventory of a server that does not capture system audio.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            supported: false,
            devices: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// The inventory bounded to what one record carries: descriptions and
    /// errors bounded, unusable devices dropped, devices sorted with the
    /// first of equal names kept, at most one default per audio system (the
    /// first), at most [`MAX_SYSTEM_AUDIO_DEVICES`] devices, and one error
    /// per audio system. An unsupported inventory lists nothing.
    #[must_use]
    pub fn fitted(&self) -> Self {
        if !self.supported {
            return Self::unsupported();
        }
        let mut devices: Vec<SystemAudioDevice> = self
            .devices
            .iter()
            .map(|device| SystemAudioDevice {
                description: bound_system_audio_description(&device.description),
                channels: device
                    .channels
                    .filter(|channels| (1..=MAX_SYSTEM_AUDIO_DEVICE_CHANNELS).contains(channels)),
                ..device.clone()
            })
            .filter(SystemAudioDevice::is_valid)
            .collect();
        devices.sort_by(|left, right| {
            (left.backend, &left.device).cmp(&(right.backend, &right.device))
        });
        devices.dedup_by(|later, earlier| {
            later.backend == earlier.backend && later.device == earlier.device
        });
        devices.truncate(MAX_SYSTEM_AUDIO_DEVICES);
        let mut defaults = Vec::new();
        for device in &mut devices {
            if device.default {
                if defaults.contains(&device.backend) {
                    device.default = false;
                } else {
                    defaults.push(device.backend);
                }
            }
        }
        let mut errors: Vec<SystemAudioBackendError> = self
            .errors
            .iter()
            .map(|error| SystemAudioBackendError {
                backend: error.backend,
                error: bound_system_audio_error(&error.error),
            })
            .filter(|error| !error.error.is_empty())
            .collect();
        errors.sort_by_key(|error| error.backend);
        errors.dedup_by_key(|error| error.backend);
        Self {
            supported: true,
            devices,
            errors,
        }
    }

    /// The listed devices of one audio system.
    pub fn devices_of(
        &self,
        backend: SystemAudioBackend,
    ) -> impl Iterator<Item = &SystemAudioDevice> {
        self.devices
            .iter()
            .filter(move |device| device.backend == backend)
    }

    /// Why one audio system could not be listed, if it could not.
    #[must_use]
    pub fn error_of(&self, backend: SystemAudioBackend) -> Option<&str> {
        self.errors
            .iter()
            .find(|error| error.backend == backend)
            .map(|error| error.error.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_are_bounded_printable_ascii() {
        for device in [
            "@DEFAULT_MONITOR@",
            "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
            "hw:Loopback,1",
            "plughw:CARD=Loopback,DEV=1",
            "default",
            "Desk mic",
        ] {
            assert_eq!(validate_system_audio_device(device), Ok(()), "{device}");
        }
        let long = "a".repeat(MAX_SYSTEM_AUDIO_DEVICE_BYTES + 1);
        for device in ["", "-i", " lead", "trail ", "tab\there", "ä", long.as_str()] {
            assert_eq!(
                validate_system_audio_device(device),
                Err(SystemAudioError::InvalidDevice),
                "{device}"
            );
        }
        assert_eq!(
            validate_system_audio_device(&"a".repeat(MAX_SYSTEM_AUDIO_DEVICE_BYTES)),
            Ok(())
        );
    }

    #[test]
    fn tokens_round_trip() {
        for backend in SystemAudioBackend::ALL {
            assert_eq!(
                SystemAudioBackend::from_token(backend.token()),
                Some(backend)
            );
        }
        assert_eq!(SystemAudioBackend::from_token("jack"), None);
        assert_eq!(
            SystemAudioState::Reopening {
                attempt: 2,
                next_in_ms: 10
            }
            .token(),
            "reopening"
        );
    }

    #[test]
    fn error_text_is_one_bounded_line() {
        assert_eq!(bound_system_audio_error(" a\nb "), "a b");
        let long = "é".repeat(MAX_SYSTEM_AUDIO_ERROR_BYTES);
        let bounded = bound_system_audio_error(&long);
        assert!(bounded.len() <= MAX_SYSTEM_AUDIO_ERROR_BYTES);
        assert!(bounded.len() >= MAX_SYSTEM_AUDIO_ERROR_BYTES - 1);
    }

    fn listed(backend: SystemAudioBackend, device: &str, default: bool) -> SystemAudioDevice {
        SystemAudioDevice {
            backend,
            device: device.into(),
            description: format!(" {device}\n"),
            channels: Some(2),
            default,
            monitor: false,
        }
    }

    #[test]
    fn a_fitted_inventory_is_sorted_bounded_and_has_one_default_per_backend() {
        let mut inventory = SystemAudioInventory {
            supported: true,
            devices: vec![
                listed(SystemAudioBackend::Alsa, "plughw:B,0", true),
                listed(SystemAudioBackend::Pulse, "z.monitor", false),
                listed(SystemAudioBackend::Alsa, "plughw:A,0", true),
                listed(SystemAudioBackend::Pulse, "a", true),
                listed(SystemAudioBackend::Pulse, "a", false),
                listed(SystemAudioBackend::Pulse, "-bad", false),
            ],
            errors: vec![
                SystemAudioBackendError {
                    backend: SystemAudioBackend::Pulse,
                    error: "pactl\nfailed".into(),
                },
                SystemAudioBackendError {
                    backend: SystemAudioBackend::Pulse,
                    error: "again".into(),
                },
                SystemAudioBackendError {
                    backend: SystemAudioBackend::Alsa,
                    error: " ".into(),
                },
            ],
        };
        inventory.devices[0].channels = Some(0);
        inventory.devices[1].description = "é".repeat(MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES);
        let fitted = inventory.fitted();
        let names: Vec<(SystemAudioBackend, &str, bool)> = fitted
            .devices
            .iter()
            .map(|device| (device.backend, device.device.as_str(), device.default))
            .collect();
        assert_eq!(
            names,
            [
                (SystemAudioBackend::Pulse, "a", true),
                (SystemAudioBackend::Pulse, "z.monitor", false),
                (SystemAudioBackend::Alsa, "plughw:A,0", true),
                (SystemAudioBackend::Alsa, "plughw:B,0", false),
            ]
        );
        assert!(fitted.devices.iter().all(SystemAudioDevice::is_valid));
        assert_eq!(fitted.devices[0].description, "a");
        assert_eq!(fitted.devices[3].channels, None);
        assert!(fitted.devices[1].description.len() <= MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES);
        assert_eq!(
            fitted.error_of(SystemAudioBackend::Pulse),
            Some("pactl failed")
        );
        assert_eq!(fitted.error_of(SystemAudioBackend::Alsa), None);
        assert_eq!(fitted.devices_of(SystemAudioBackend::Alsa).count(), 2);
        assert_eq!(fitted.fitted(), fitted);
        assert_eq!(
            SystemAudioInventory {
                supported: false,
                ..inventory
            }
            .fitted(),
            SystemAudioInventory::unsupported()
        );
    }
}
