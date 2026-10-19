//! System audio input codec (Protocol 2.55, AU-011).
//!
//! * Config fields: `backend` (`pulse|alsa`) and `device`, validated on
//!   both sides.
//! * Status fields: `state`, `attempt` and `next_in_ms` (only for
//!   `reopening`), `reopens`, and an optional `last_error`.
//! * A snapshot's `system_audio_inputs` is a comma-separated, strictly
//!   ascending list of `<input>~<backend>~<state>~<attempt>~<next_in_ms>~
//!   <reopens>~<length>:<device>`: the device is written raw after its
//!   canonical byte length, so the line encoding escapes it exactly once.
//! * Protocol 2.68 device lists (`supported`, `errors`, `devices`) write
//!   their texts the same way; see [`crate::SystemAudioDevicesMessage`].

use core::fmt::Write as _;

use fm_types::system_audio::{
    MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES, MAX_SYSTEM_AUDIO_DEVICE_BYTES, MAX_SYSTEM_AUDIO_DEVICES,
    MAX_SYSTEM_AUDIO_ERROR_BYTES, MAX_SYSTEM_AUDIO_INPUTS, SystemAudioBackend,
    SystemAudioBackendError, SystemAudioConfig, SystemAudioDevice, SystemAudioInventory,
    SystemAudioState, SystemAudioStatus,
};

use crate::{CodecError, SystemAudioEntry, WireInputId};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

const ENTRIES: &str = "system_audio_inputs";

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn canonical<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| T::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

fn backend(field: &'static str, value: &str) -> Result<SystemAudioBackend, CodecError> {
    SystemAudioBackend::from_token(value).ok_or_else(|| invalid(field, value))
}

fn check_config(config: &SystemAudioConfig) -> Result<(), CodecError> {
    config
        .validate()
        .map_err(|error| invalid(error.field(), error.code()))
}

/// Encodes a config: `backend` and `device`.
pub(super) fn encode_config(
    config: &SystemAudioConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_config(config)?;
    push("backend", config.backend.token().to_owned())?;
    push("device", config.device.clone())
}

/// Decodes a config.
pub(super) fn decode_config(fields: &mut dyn FieldSource) -> Result<SystemAudioConfig, CodecError> {
    let config = SystemAudioConfig {
        backend: backend("backend", &fields.required("backend")?)?,
        device: fields.required("device")?,
    };
    check_config(&config)?;
    Ok(config)
}

fn state_parts(state: SystemAudioState) -> (&'static str, u32, u32) {
    match state {
        SystemAudioState::Reopening {
            attempt,
            next_in_ms,
        } => ("reopening", attempt, next_in_ms),
        other => (other.token(), 0, 0),
    }
}

fn parse_state(
    field: &'static str,
    token: &str,
    attempt: u32,
    next_in_ms: u32,
) -> Result<SystemAudioState, CodecError> {
    let state = match token {
        "idle" => SystemAudioState::Idle,
        "opening" => SystemAudioState::Opening,
        "live" => SystemAudioState::Live,
        "failed" => SystemAudioState::Failed,
        "reopening" if attempt > 0 => {
            return Ok(SystemAudioState::Reopening {
                attempt,
                next_in_ms,
            });
        }
        _ => return Err(invalid(field, token)),
    };
    if attempt != 0 || next_in_ms != 0 {
        return Err(invalid(field, token));
    }
    Ok(state)
}

fn check_error(error: Option<&str>) -> Result<(), CodecError> {
    match error {
        Some(error)
            if error.is_empty()
                || error.len() > MAX_SYSTEM_AUDIO_ERROR_BYTES
                || error.chars().any(char::is_control) =>
        {
            Err(invalid("last_error", error))
        }
        _ => Ok(()),
    }
}

/// Encodes a `system_audio_status` runtime event's fields after `event`.
pub(super) fn encode_status(
    input: WireInputId,
    status: &SystemAudioStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_error(status.last_error.as_deref())?;
    let (state, attempt, next_in_ms) = state_parts(status.state);
    push("input", input.to_string())?;
    push("state", state.to_owned())?;
    if state == "reopening" {
        if attempt == 0 {
            return Err(invalid("attempt", "0"));
        }
        push("attempt", attempt.to_string())?;
        push("next_in_ms", next_in_ms.to_string())?;
    }
    push("reopens", status.reopens.to_string())?;
    if let Some(error) = &status.last_error {
        push("last_error", error.clone())?;
    }
    Ok(())
}

/// Decodes a `system_audio_status` runtime event's fields.
pub(super) fn decode_status(
    fields: &mut dyn FieldSource,
) -> Result<(WireInputId, SystemAudioStatus), CodecError> {
    let input_text = fields.required("input")?;
    let input = parse_canonical_input(&input_text).ok_or_else(|| invalid("input", input_text))?;
    let state = fields.required("state")?;
    let (attempt, next_in_ms) = if state == "reopening" {
        (
            canonical("attempt", &fields.required("attempt")?)?,
            canonical("next_in_ms", &fields.required("next_in_ms")?)?,
        )
    } else {
        (0, 0)
    };
    let status = SystemAudioStatus {
        state: parse_state("state", &state, attempt, next_in_ms)?,
        reopens: canonical("reopens", &fields.required("reopens")?)?,
        last_error: fields.optional("last_error"),
    };
    check_error(status.last_error.as_deref())?;
    Ok((input, status))
}

/// Encodes a snapshot's `system_audio_inputs`.
pub(super) fn encode_entries(entries: &[SystemAudioEntry]) -> Result<String, CodecError> {
    if entries.len() > MAX_SYSTEM_AUDIO_INPUTS {
        return Err(CodecError::TooManyItems(ENTRIES));
    }
    let mut previous = None;
    let mut encoded = String::new();
    for entry in entries {
        if previous.is_some_and(|previous| previous >= entry.input.get()) {
            return Err(invalid(ENTRIES, entry.input.to_string()));
        }
        previous = Some(entry.input.get());
        check_config(&entry.config)?;
        let (state, attempt, next_in_ms) = state_parts(entry.status.state);
        if state == "reopening" && attempt == 0 {
            return Err(invalid(ENTRIES, "reopening attempt 0"));
        }
        // Error text travels only in `system_audio_status`.
        if entry.status.last_error.is_some() {
            return Err(invalid(ENTRIES, "last_error"));
        }
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{}~{}~{}~{}~{}~{}:{}",
            entry.input,
            entry.config.backend.token(),
            state,
            attempt,
            next_in_ms,
            entry.status.reopens,
            entry.config.device.len(),
            entry.config.device,
        )
        .expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Decodes a snapshot's `system_audio_inputs`, the exact inverse of
/// [`encode_entries`].
pub(super) fn decode_entries(value: &str) -> Result<Vec<SystemAudioEntry>, CodecError> {
    let mut entries: Vec<SystemAudioEntry> = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if !entries.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(ENTRIES, value))?;
        }
        if entries.len() == MAX_SYSTEM_AUDIO_INPUTS {
            return Err(CodecError::TooManyItems(ENTRIES));
        }
        let (parts, device, after) = split_entry(rest).ok_or_else(|| invalid(ENTRIES, rest))?;
        let [input, backend_token, state, attempt, next_in_ms, reopens] = parts;
        let input = parse_canonical_input(input).ok_or_else(|| invalid(ENTRIES, rest))?;
        if entries
            .last()
            .is_some_and(|previous| previous.input.get() >= input.get())
        {
            return Err(invalid(ENTRIES, rest));
        }
        let config = SystemAudioConfig {
            backend: backend(ENTRIES, backend_token)?,
            device: device.to_owned(),
        };
        check_config(&config)?;
        let status = SystemAudioStatus {
            state: parse_state(
                ENTRIES,
                state,
                canonical(ENTRIES, attempt)?,
                canonical(ENTRIES, next_in_ms)?,
            )?,
            reopens: canonical(ENTRIES, reopens)?,
            last_error: None,
        };
        rest = after;
        entries.push(SystemAudioEntry {
            input,
            config,
            status,
        });
    }
    Ok(entries)
}

/// Splits one snapshot entry off the front of `text`: its six `~`-ended
/// parts, its device, and what follows the entry.
fn split_entry(text: &str) -> Option<([&str; 6], &str, &str)> {
    let mut parts = [""; 6];
    let mut after = text;
    for part in &mut parts {
        let (value, tail) = after.split_once('~')?;
        *part = value;
        after = tail;
    }
    let (length, after) = after.split_once(':')?;
    let length = parse_canonical_u64(length)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= MAX_SYSTEM_AUDIO_DEVICE_BYTES)?;
    let device = after.get(..length)?;
    Some((parts, device, &after[length..]))
}

const DEVICES: &str = "devices";
const ERRORS: &str = "errors";

/// Checks a device inventory against the exact contract (see
/// [`crate::SystemAudioDevicesMessage`]).
fn check_inventory(inventory: &SystemAudioInventory) -> Result<(), CodecError> {
    if !inventory.supported && (!inventory.devices.is_empty() || !inventory.errors.is_empty()) {
        return Err(invalid("supported", "0"));
    }
    if inventory.devices.len() > MAX_SYSTEM_AUDIO_DEVICES {
        return Err(CodecError::TooManyItems(DEVICES));
    }
    let mut defaults = Vec::new();
    for (index, device) in inventory.devices.iter().enumerate() {
        if !device.is_valid() {
            return Err(invalid(DEVICES, device.device.clone()));
        }
        if index > 0 {
            let previous = &inventory.devices[index - 1];
            if (previous.backend, &previous.device) >= (device.backend, &device.device) {
                return Err(invalid(DEVICES, device.device.clone()));
            }
        }
        if device.default {
            if defaults.contains(&device.backend) {
                return Err(invalid(DEVICES, "two defaults"));
            }
            defaults.push(device.backend);
        }
    }
    for (index, error) in inventory.errors.iter().enumerate() {
        if index > 0 && inventory.errors[index - 1].backend >= error.backend {
            return Err(invalid(ERRORS, error.backend.token()));
        }
        if error.error.is_empty()
            || error.error.len() > MAX_SYSTEM_AUDIO_ERROR_BYTES
            || error.error.chars().any(char::is_control)
        {
            return Err(invalid(ERRORS, error.error.clone()));
        }
    }
    Ok(())
}

/// Encodes a device inventory as `supported`, `errors`, and `devices`.
pub(super) fn encode_inventory(
    inventory: &SystemAudioInventory,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_inventory(inventory)?;
    push("supported", u8::from(inventory.supported).to_string())?;
    let mut errors = String::new();
    for error in &inventory.errors {
        if !errors.is_empty() {
            errors.push(',');
        }
        write!(
            errors,
            "{}~{}:{}",
            error.backend.token(),
            error.error.len(),
            error.error
        )
        .expect("writing to a string cannot fail");
    }
    push(ERRORS, errors)?;
    let mut devices = String::new();
    for device in &inventory.devices {
        if !devices.is_empty() {
            devices.push(',');
        }
        write!(
            devices,
            "{}~{}~{}~{}~{}~{}:{}{}",
            device.backend.token(),
            u8::from(device.default),
            u8::from(device.monitor),
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

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn text_length(text: &str, max: usize) -> Option<usize> {
    parse_canonical_u64(text)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= max)
}

/// Decodes a device inventory, the exact inverse of [`encode_inventory`].
pub(super) fn decode_inventory(
    fields: &mut dyn FieldSource,
) -> Result<SystemAudioInventory, CodecError> {
    let supported = flag("supported", &fields.required("supported")?)?;
    let errors_text = fields.required(ERRORS)?;
    let mut errors = Vec::new();
    let mut rest = errors_text.as_str();
    while !rest.is_empty() {
        if !errors.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(ERRORS, errors_text.clone()))?;
        }
        if errors.len() == SystemAudioBackend::ALL.len() {
            return Err(CodecError::TooManyItems(ERRORS));
        }
        let (error, after) =
            split_error(rest).ok_or_else(|| invalid(ERRORS, errors_text.clone()))?;
        errors.push(error);
        rest = after;
    }
    let devices_text = fields.required(DEVICES)?;
    let mut devices = Vec::new();
    let mut rest = devices_text.as_str();
    while !rest.is_empty() {
        if !devices.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(DEVICES, devices_text.clone()))?;
        }
        if devices.len() == MAX_SYSTEM_AUDIO_DEVICES {
            return Err(CodecError::TooManyItems(DEVICES));
        }
        let (device, after) = split_device(rest)?;
        devices.push(device);
        rest = after;
    }
    let inventory = SystemAudioInventory {
        supported,
        devices,
        errors,
    };
    check_inventory(&inventory)?;
    Ok(inventory)
}

/// Splits one `<backend>~<length>:<text>` error off the front of `text`.
fn split_error(text: &str) -> Option<(SystemAudioBackendError, &str)> {
    let (backend, after) = text.split_once('~')?;
    let backend = SystemAudioBackend::from_token(backend)?;
    let (size, after) = after.split_once(':')?;
    let size = text_length(size, MAX_SYSTEM_AUDIO_ERROR_BYTES)?;
    let error = after.get(..size)?.to_owned();
    Some((SystemAudioBackendError { backend, error }, &after[size..]))
}

/// Splits one device off the front of `text`.
fn split_device(text: &str) -> Result<(SystemAudioDevice, &str), CodecError> {
    let bad = || invalid(DEVICES, text);
    let mut parts = [""; 5];
    let mut after = text;
    for part in &mut parts {
        let (value, tail) = after.split_once('~').ok_or_else(bad)?;
        *part = value;
        after = tail;
    }
    let [backend_token, default, monitor, channels, device_length] = parts;
    let (description_length, after) = after.split_once(':').ok_or_else(bad)?;
    let device_length =
        text_length(device_length, MAX_SYSTEM_AUDIO_DEVICE_BYTES).ok_or_else(bad)?;
    let description_length =
        text_length(description_length, MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES).ok_or_else(bad)?;
    let device = after.get(..device_length).ok_or_else(bad)?;
    let after = &after[device_length..];
    let description = after.get(..description_length).ok_or_else(bad)?;
    let channels = if channels == "-" {
        None
    } else {
        Some(canonical::<u8>(DEVICES, channels)?)
    };
    Ok((
        SystemAudioDevice {
            backend: backend(DEVICES, backend_token)?,
            device: device.to_owned(),
            description: description.to_owned(),
            channels,
            default: flag(DEVICES, default)?,
            monitor: flag(DEVICES, monitor)?,
        },
        &after[description_length..],
    ))
}
