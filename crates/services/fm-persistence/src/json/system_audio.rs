//! A system audio input in `project.json` (schema 52, AU-011).
//!
//! ```text
//! {"type": "system_audio", "backend": "pulse|alsa", "device": "<name>"}
//! ```
//!
//! Both fields are required. `device` is 1 to 128 printable ASCII
//! characters ([`fm_types::system_audio::validate_system_audio_device`]).

use fm_model::InputKind;
use fm_types::system_audio::{SystemAudioBackend, SystemAudioConfig};

use super::{DecodeError, Object, syntax, unknown_enum};

/// Writes `{"type": "system_audio", "backend": ..., "device": ...}`.
pub(crate) fn write_system_audio_input(output: &mut String, config: &SystemAudioConfig) {
    output.push_str("{\"type\": \"system_audio\", \"backend\": \"");
    output.push_str(config.backend.token());
    output.push_str("\", \"device\": \"");
    super::escape_string(output, &config.device);
    output.push_str("\"}");
}

/// Parses the fields of a `system_audio` input kind after its `type`.
pub(crate) fn parse_system_audio_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let token = object.string("backend")?;
    let backend = SystemAudioBackend::from_token(&token)
        .ok_or_else(|| unknown_enum("system audio backend", &token))?;
    let config = SystemAudioConfig::new(backend, object.string("device")?);
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(InputKind::SystemAudio(config))
}
