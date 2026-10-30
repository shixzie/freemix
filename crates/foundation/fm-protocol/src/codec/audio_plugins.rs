//! `audio_plugin_status_request` / `audio_plugin_status` (2.129, AU-006): one
//! target's ordered plugin slots and the identifiers the daemon installs.

use crate::{AudioPluginStatusMessage, AudioPluginStatusRequest, CodecError, PluginChainSlot};

use super::audio_mixer::{decode_plugin_slots, decode_target, encode_plugin_slots, encode_target};
use super::media::FieldSource;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Encodes the fields of an `audio_plugin_status` answer.
pub(super) fn encode_status(
    message: &AudioPluginStatusMessage,
    push: &mut dyn FnMut(&'static str, String) -> Result<(), CodecError>,
) -> Result<(), CodecError> {
    push("target", encode_target(message.target))?;
    let available = message.available.join(",");
    for plugin in &message.available {
        if !PluginChainSlot::plugin_id_is_valid(plugin) {
            return Err(invalid("available", plugin.clone()));
        }
    }
    push("available", available)?;
    push("plugins", encode_plugin_slots(&message.slots)?)
}

/// Decodes `audio_plugin_status`'s fields after the server identity and
/// request id.
pub(super) fn decode_status(
    server: crate::ServerIdentity,
    request_id: String,
    fields: &mut dyn FieldSource,
) -> Result<AudioPluginStatusMessage, CodecError> {
    let target = decode_target("target", &fields.required("target")?)?;
    let available = fields.required("available")?;
    let available: Vec<String> = if available.is_empty() {
        Vec::new()
    } else {
        available.split(',').map(str::to_owned).collect()
    };
    let mut seen = std::collections::BTreeSet::new();
    for plugin in &available {
        if !PluginChainSlot::plugin_id_is_valid(plugin) || !seen.insert(plugin.as_str()) {
            return Err(invalid("available", plugin.clone()));
        }
    }
    Ok(AudioPluginStatusMessage {
        server,
        request_id,
        target,
        available,
        slots: decode_plugin_slots(&fields.required("plugins")?)?,
    })
}

pub(super) fn encode_request(
    message: &AudioPluginStatusRequest,
    push: &mut dyn FnMut(&'static str, String) -> Result<(), CodecError>,
) -> Result<(), CodecError> {
    push("target", encode_target(message.target))
}

pub(super) fn decode_request(
    request_id: String,
    fields: &mut dyn FieldSource,
) -> Result<AudioPluginStatusRequest, CodecError> {
    Ok(AudioPluginStatusRequest {
        request_id,
        target: decode_target("target", &fields.required("target")?)?,
    })
}
