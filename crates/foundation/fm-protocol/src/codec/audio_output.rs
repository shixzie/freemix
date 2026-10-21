//! Audio output device list codec (Protocol 2.102, AU-001): `supported`,
//! an optional `error`, and `devices`; see
//! [`crate::AudioOutputDevicesMessage`].

use core::fmt::Write as _;

use fm_types::audio_output::{
    AudioOutputDevice, AudioOutputInventory, MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES,
    MAX_AUDIO_OUTPUT_DEVICE_BYTES, MAX_AUDIO_OUTPUT_DEVICES, MAX_AUDIO_OUTPUT_ERROR_BYTES,
};
use fm_types::system_audio::bound_system_audio_error;

use crate::CodecError;

use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

const DEVICES: &str = "devices";
const ERROR: &str = "error";
const SUPPORTED: &str = "supported";

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Checks an inventory against the exact contract.
fn check_inventory(inventory: &AudioOutputInventory) -> Result<(), CodecError> {
    if !inventory.supported && (!inventory.devices.is_empty() || inventory.error.is_some()) {
        return Err(invalid(SUPPORTED, "0"));
    }
    if inventory.devices.len() > MAX_AUDIO_OUTPUT_DEVICES {
        return Err(CodecError::TooManyItems(DEVICES));
    }
    let mut defaults = 0;
    for (index, device) in inventory.devices.iter().enumerate() {
        if !device.is_valid() || (index > 0 && inventory.devices[index - 1].device >= device.device)
        {
            return Err(invalid(DEVICES, device.device.clone()));
        }
        defaults += usize::from(device.default);
    }
    if defaults > 1 {
        return Err(invalid(DEVICES, "two defaults"));
    }
    if let Some(error) = &inventory.error
        && (error.is_empty()
            || error.len() > MAX_AUDIO_OUTPUT_ERROR_BYTES
            || bound_system_audio_error(error) != *error)
    {
        return Err(invalid(ERROR, error.clone()));
    }
    Ok(())
}

/// Encodes an inventory as `supported`, an optional `error`, and
/// `devices`.
pub(super) fn encode_inventory(
    inventory: &AudioOutputInventory,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_inventory(inventory)?;
    push(SUPPORTED, u8::from(inventory.supported).to_string())?;
    if let Some(error) = &inventory.error {
        push(ERROR, error.clone())?;
    }
    let mut devices = String::new();
    for device in &inventory.devices {
        if !devices.is_empty() {
            devices.push(',');
        }
        write!(
            devices,
            "{}~{}~{}~{}:{}{}",
            u8::from(device.default),
            device
                .channels
                .map_or_else(|| "-".to_owned(), |channels| channels.to_string()),
            device.device.len(),
            device.description.len(),
            device.device,
            device.description,
        )
        .expect("writing to a string cannot fail");
    }
    push(DEVICES, devices)
}

fn text_length(text: &str, max: usize) -> Option<usize> {
    parse_canonical_u64(text)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= max)
}

/// Decodes an inventory, the exact inverse of [`encode_inventory`].
pub(super) fn decode_inventory(
    fields: &mut dyn FieldSource,
) -> Result<AudioOutputInventory, CodecError> {
    let supported = match fields.required(SUPPORTED)?.as_str() {
        "0" => false,
        "1" => true,
        other => return Err(invalid(SUPPORTED, other)),
    };
    let error = fields.optional(ERROR);
    let devices_text = fields.required(DEVICES)?;
    let mut devices = Vec::new();
    let mut rest = devices_text.as_str();
    while !rest.is_empty() {
        if !devices.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(DEVICES, devices_text.clone()))?;
        }
        if devices.len() == MAX_AUDIO_OUTPUT_DEVICES {
            return Err(CodecError::TooManyItems(DEVICES));
        }
        let (device, after) =
            split_device(rest).ok_or_else(|| invalid(DEVICES, devices_text.clone()))?;
        devices.push(device);
        rest = after;
    }
    let inventory = AudioOutputInventory {
        supported,
        devices,
        error,
    };
    check_inventory(&inventory)?;
    Ok(inventory)
}

/// Splits one device off the front of `text`.
fn split_device(text: &str) -> Option<(AudioOutputDevice, &str)> {
    let (default, after) = text.split_once('~')?;
    let default = match default {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let (channels, after) = after.split_once('~')?;
    let channels = if channels == "-" {
        None
    } else {
        Some(parse_canonical_u64(channels).and_then(|channels| u8::try_from(channels).ok())?)
    };
    let (device_length, after) = after.split_once('~')?;
    let device_length = text_length(device_length, MAX_AUDIO_OUTPUT_DEVICE_BYTES)?;
    let (description_length, after) = after.split_once(':')?;
    let description_length = text_length(description_length, MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES)?;
    let device = after.get(..device_length)?;
    let after = &after[device_length..];
    let description = after.get(..description_length)?;
    Some((
        AudioOutputDevice {
            device: device.to_owned(),
            description: description.to_owned(),
            channels,
            default,
        },
        &after[description_length..],
    ))
}
