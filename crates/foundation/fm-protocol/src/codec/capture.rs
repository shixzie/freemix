//! Capture input codec (Protocol 2.47, IN-001).
//!
//! * Settings fields: `format` (a canonical [`CaptureFormat`] token or `-`),
//!   `audio` (a [`CaptureAudio`] token), `loss` (`hold|black`). A config adds
//!   `device`, the stable key. Each is validated on both sides.
//! * Status fields: `state`, `reason` (or `-`), `device` (or `-`), `format`
//!   (or `-`), `audio` (`0|1`), `frames`, `losses`, `recoveries`,
//!   `renegotiations`, and `error` (or `-`). A present text is 1 to its bound
//!   of bytes without control characters, and never `-` itself.
//! * List texts (stable keys, device texts, audio tokens) are escaped
//!   minimally: `%`, `~`, `,`, and `;` become `%25`, `%7E`, `%2C`, and
//!   `%3B`, and a text that is exactly `-` becomes `%2D`; decoding accepts
//!   only that canonical form.
//! * A snapshot's `capture_inputs` is a comma-separated, strictly ascending
//!   list of `<input>~<key>~<format|->~<audio>~<loss>~<state>~<reason|->~
//!   <device|->~<status format|->~<audio 0|1>~<frames>~<losses>~
//!   <recoveries>~<renegotiations>`.
//! * An inventory is `backend` (token or `-`), `error` (or `-`), and
//!   `devices`: a comma-separated list, strictly ascending by key, of
//!   `<key>~<name>~<path>~<driver>~<bus>~<audio|->~<format>;<format>;...`.

use core::fmt::Write as _;

use fm_types::capture::{
    CaptureAudio, CaptureDevice, CaptureFormat, CaptureInputConfig, CaptureInventory,
    CaptureLossReason, CaptureLossSlate, CaptureSettings, CaptureSignalState, CaptureStatus,
    MAX_CAPTURE_DEVICE_FORMATS, MAX_CAPTURE_DEVICES, MAX_CAPTURE_ERROR_BYTES, MAX_CAPTURE_INPUTS,
    MAX_CAPTURE_TEXT_BYTES, validate_stable_key,
};

use crate::{CaptureInputEntry, CodecError, WireInputId};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64, unescape};

/// The placeholder of an absent value.
const NONE: &str = "-";

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

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

/// Escapes a list text minimally (see the module documentation).
fn escape_text(text: &str) -> String {
    if text == NONE {
        return "%2D".to_owned();
    }
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '%' => escaped.push_str("%25"),
            '~' => escaped.push_str("%7E"),
            ',' => escaped.push_str("%2C"),
            ';' => escaped.push_str("%3B"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Unescapes a list text, accepting only the canonical escaping.
fn unescape_text(field: &'static str, text: &str) -> Result<String, CodecError> {
    let decoded = unescape(text)?;
    if escape_text(&decoded) == text {
        Ok(decoded)
    } else {
        Err(invalid(field, text))
    }
}

/// A present wire text: 1 to `max` bytes without control characters, and
/// not the absent placeholder.
fn check_text(field: &'static str, text: &str, max: usize) -> Result<(), CodecError> {
    if text.is_empty() || text == NONE || text.len() > max || text.chars().any(char::is_control) {
        Err(invalid(field, text))
    } else {
        Ok(())
    }
}

/// A device text in an inventory: at most [`MAX_CAPTURE_TEXT_BYTES`] bytes
/// without control characters (`required` ones are not empty).
fn check_device_text(text: &str, required: bool) -> Result<(), CodecError> {
    if (required && text.is_empty())
        || text.len() > MAX_CAPTURE_TEXT_BYTES
        || text.chars().any(char::is_control)
    {
        Err(invalid("devices", text))
    } else {
        Ok(())
    }
}

fn check_key(field: &'static str, key: &str) -> Result<(), CodecError> {
    validate_stable_key(key).map_err(|_| invalid(field, key))
}

fn check_settings(field: &'static str, settings: &CaptureSettings) -> Result<(), CodecError> {
    settings
        .validate()
        .map_err(|error| invalid(field, error.to_string()))
}

fn encode_format(format: Option<&CaptureFormat>) -> String {
    format.map_or_else(|| NONE.to_owned(), CaptureFormat::token)
}

fn decode_format(field: &'static str, value: &str) -> Result<Option<CaptureFormat>, CodecError> {
    if value == NONE {
        Ok(None)
    } else {
        CaptureFormat::from_token(value)
            .map(Some)
            .map_err(|_| invalid(field, value))
    }
}

fn decode_audio(field: &'static str, value: &str) -> Result<CaptureAudio, CodecError> {
    CaptureAudio::from_token(value).map_err(|_| invalid(field, value))
}

fn decode_loss(field: &'static str, value: &str) -> Result<CaptureLossSlate, CodecError> {
    CaptureLossSlate::from_token(value).ok_or_else(|| invalid(field, value))
}

/// Encodes the settings fields: `format`, `audio`, `loss`.
pub(super) fn encode_settings(
    settings: &CaptureSettings,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_settings("capture", settings)?;
    push("format", encode_format(settings.format.as_ref()))?;
    push("audio", settings.audio.token())?;
    push("loss", settings.loss.token().to_owned())
}

/// Decodes the settings fields.
pub(super) fn decode_settings(fields: &mut dyn FieldSource) -> Result<CaptureSettings, CodecError> {
    let settings = CaptureSettings {
        format: decode_format("format", &fields.required("format")?)?,
        audio: decode_audio("audio", &fields.required("audio")?)?,
        loss: decode_loss("loss", &fields.required("loss")?)?,
    };
    check_settings("capture", &settings)?;
    Ok(settings)
}

/// Encodes a whole config: `device` and the settings fields.
pub(super) fn encode_config(
    config: &CaptureInputConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_key("device", &config.stable_key)?;
    push("device", config.stable_key.clone())?;
    encode_settings(&config.settings, push)
}

/// Decodes a whole config.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<CaptureInputConfig, CodecError> {
    let stable_key = fields.required("device")?;
    check_key("device", &stable_key)?;
    Ok(CaptureInputConfig {
        stable_key,
        settings: decode_settings(fields)?,
    })
}

fn parse_state(field: &'static str, value: &str) -> Result<CaptureSignalState, CodecError> {
    CaptureSignalState::from_token(value).ok_or_else(|| invalid(field, value))
}

fn parse_reason(field: &'static str, value: &str) -> Result<Option<CaptureLossReason>, CodecError> {
    if value == NONE {
        Ok(None)
    } else {
        CaptureLossReason::from_token(value)
            .map(Some)
            .ok_or_else(|| invalid(field, value))
    }
}

fn optional_text(
    field: &'static str,
    value: String,
    max: usize,
) -> Result<Option<String>, CodecError> {
    if value == NONE {
        return Ok(None);
    }
    check_text(field, &value, max)?;
    Ok(Some(value))
}

/// Encodes a `capture_status` runtime event's fields after `event`.
pub(super) fn encode_status(
    input: WireInputId,
    status: &CaptureStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if let Some(device) = &status.device {
        check_text("device", device, MAX_CAPTURE_TEXT_BYTES)?;
    }
    if let Some(error) = &status.last_error {
        check_text("error", error, MAX_CAPTURE_ERROR_BYTES)?;
    }
    if let Some(format) = &status.format {
        format
            .validate()
            .map_err(|_| invalid("format", format.token()))?;
    }
    push("input", input.to_string())?;
    push("state", status.state.token().to_owned())?;
    push(
        "reason",
        status
            .reason
            .map_or(NONE, CaptureLossReason::token)
            .to_owned(),
    )?;
    push(
        "device",
        status.device.clone().unwrap_or_else(|| NONE.to_owned()),
    )?;
    push("format", encode_format(status.format.as_ref()))?;
    push("audio", u8::from(status.audio).to_string())?;
    push("frames", status.frames.to_string())?;
    push("losses", status.losses.to_string())?;
    push("recoveries", status.recoveries.to_string())?;
    push("renegotiations", status.renegotiations.to_string())?;
    push(
        "error",
        status.last_error.clone().unwrap_or_else(|| NONE.to_owned()),
    )
}

/// Decodes a `capture_status` runtime event's fields.
pub(super) fn decode_status(
    fields: &mut dyn FieldSource,
) -> Result<(WireInputId, CaptureStatus), CodecError> {
    let input_text = fields.required("input")?;
    let input = parse_canonical_input(&input_text).ok_or_else(|| invalid("input", input_text))?;
    let status = CaptureStatus {
        state: parse_state("state", &fields.required("state")?)?,
        reason: parse_reason("reason", &fields.required("reason")?)?,
        device: optional_text("device", fields.required("device")?, MAX_CAPTURE_TEXT_BYTES)?,
        format: decode_format("format", &fields.required("format")?)?,
        audio: flag("audio", &fields.required("audio")?)?,
        frames: canonical("frames", &fields.required("frames")?)?,
        losses: canonical("losses", &fields.required("losses")?)?,
        recoveries: canonical("recoveries", &fields.required("recoveries")?)?,
        renegotiations: canonical("renegotiations", &fields.required("renegotiations")?)?,
        last_error: optional_text("error", fields.required("error")?, MAX_CAPTURE_ERROR_BYTES)?,
    };
    Ok((input, status))
}

/// Encodes a snapshot's `capture_inputs`.
pub(super) fn encode_entries(entries: &[CaptureInputEntry]) -> Result<String, CodecError> {
    const FIELD: &str = "capture_inputs";
    if entries.len() > MAX_CAPTURE_INPUTS {
        return Err(CodecError::TooManyItems(FIELD));
    }
    let mut previous = None;
    let mut encoded = String::new();
    for entry in entries {
        if previous.is_some_and(|previous| previous >= entry.input.get()) {
            return Err(invalid(FIELD, entry.input.to_string()));
        }
        previous = Some(entry.input.get());
        check_key(FIELD, &entry.config.stable_key)?;
        check_settings(FIELD, &entry.config.settings)?;
        let settings = &entry.config.settings;
        let status = &entry.status;
        if status.last_error.is_some() {
            return Err(invalid(FIELD, "a snapshot carries no capture error text"));
        }
        if let Some(device) = &status.device {
            check_text(FIELD, device, MAX_CAPTURE_TEXT_BYTES)?;
        }
        if let Some(format) = &status.format {
            format
                .validate()
                .map_err(|_| invalid(FIELD, format.token()))?;
        }
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}",
            entry.input,
            escape_text(&entry.config.stable_key),
            encode_format(settings.format.as_ref()),
            escape_text(&settings.audio.token()),
            settings.loss.token(),
            status.state.token(),
            status.reason.map_or(NONE, CaptureLossReason::token),
            status
                .device
                .as_deref()
                .map_or_else(|| NONE.to_owned(), escape_text),
            encode_format(status.format.as_ref()),
            u8::from(status.audio),
            status.frames,
            status.losses,
            status.recoveries,
            status.renegotiations,
        )
        .expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Decodes a snapshot's `capture_inputs`, the exact inverse of
/// [`encode_entries`].
pub(super) fn decode_entries(value: &str) -> Result<Vec<CaptureInputEntry>, CodecError> {
    const FIELD: &str = "capture_inputs";
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_CAPTURE_INPUTS + 1).count() > MAX_CAPTURE_INPUTS {
        return Err(CodecError::TooManyItems(FIELD));
    }
    let mut entries: Vec<CaptureInputEntry> = Vec::new();
    for item in value.split(',') {
        let parts: Vec<&str> = item.split('~').collect();
        let [
            input,
            key,
            format,
            audio,
            loss,
            state,
            reason,
            device,
            status_format,
            status_audio,
            frames,
            losses,
            recoveries,
            renegotiations,
        ] = parts.as_slice()
        else {
            return Err(invalid(FIELD, item));
        };
        let input = parse_canonical_input(input).ok_or_else(|| invalid(FIELD, item))?;
        if entries
            .last()
            .is_some_and(|previous| previous.input.get() >= input.get())
        {
            return Err(invalid(FIELD, item));
        }
        let stable_key = unescape_text(FIELD, key)?;
        check_key(FIELD, &stable_key)?;
        let settings = CaptureSettings {
            format: decode_format(FIELD, format)?,
            audio: decode_audio(FIELD, &unescape_text(FIELD, audio)?)?,
            loss: decode_loss(FIELD, loss)?,
        };
        check_settings(FIELD, &settings)?;
        let device = if *device == NONE {
            None
        } else {
            let device = unescape_text(FIELD, device)?;
            check_text(FIELD, &device, MAX_CAPTURE_TEXT_BYTES)?;
            Some(device)
        };
        let status = CaptureStatus {
            state: parse_state(FIELD, state)?,
            reason: parse_reason(FIELD, reason)?,
            device,
            format: decode_format(FIELD, status_format)?,
            audio: flag(FIELD, status_audio)?,
            frames: canonical(FIELD, frames)?,
            losses: canonical(FIELD, losses)?,
            recoveries: canonical(FIELD, recoveries)?,
            renegotiations: canonical(FIELD, renegotiations)?,
            last_error: None,
        };
        entries.push(CaptureInputEntry {
            input,
            config: CaptureInputConfig {
                stable_key,
                settings,
            },
            status,
        });
    }
    Ok(entries)
}

fn check_device(device: &CaptureDevice) -> Result<(), CodecError> {
    check_key("devices", &device.stable_key)?;
    check_device_text(&device.name, true)?;
    check_device_text(&device.path, true)?;
    check_device_text(&device.driver, false)?;
    check_device_text(&device.bus, false)?;
    if let Some(audio) = &device.audio {
        check_device_text(audio, true)?;
    }
    if device.formats.len() > MAX_CAPTURE_DEVICE_FORMATS {
        return Err(CodecError::TooManyItems("devices"));
    }
    for format in &device.formats {
        format
            .validate()
            .map_err(|_| invalid("devices", format.token()))?;
    }
    Ok(())
}

fn encode_device(device: &CaptureDevice) -> Result<String, CodecError> {
    check_device(device)?;
    let formats = device
        .formats
        .iter()
        .map(CaptureFormat::token)
        .collect::<Vec<_>>()
        .join(";");
    Ok(format!(
        "{}~{}~{}~{}~{}~{}~{}",
        escape_text(&device.stable_key),
        escape_text(&device.name),
        escape_text(&device.path),
        escape_text(&device.driver),
        escape_text(&device.bus),
        device
            .audio
            .as_deref()
            .map_or_else(|| NONE.to_owned(), escape_text),
        formats,
    ))
}

fn decode_device(entry: &str) -> Result<CaptureDevice, CodecError> {
    let parts: Vec<&str> = entry.split('~').collect();
    let [key, name, path, driver, bus, audio, formats] = parts.as_slice() else {
        return Err(invalid("devices", entry));
    };
    let formats = if formats.is_empty() {
        Vec::new()
    } else {
        if formats
            .split(';')
            .take(MAX_CAPTURE_DEVICE_FORMATS + 1)
            .count()
            > MAX_CAPTURE_DEVICE_FORMATS
        {
            return Err(CodecError::TooManyItems("devices"));
        }
        formats
            .split(';')
            .map(|format| CaptureFormat::from_token(format).map_err(|_| invalid("devices", format)))
            .collect::<Result<Vec<_>, _>>()?
    };
    let device = CaptureDevice {
        stable_key: unescape_text("devices", key)?,
        name: unescape_text("devices", name)?,
        path: unescape_text("devices", path)?,
        driver: unescape_text("devices", driver)?,
        bus: unescape_text("devices", bus)?,
        audio: if *audio == NONE {
            None
        } else {
            Some(unescape_text("devices", audio)?)
        },
        formats,
    };
    check_device(&device)?;
    Ok(device)
}

/// Encodes an inventory as `backend`, `error`, and `devices`.
pub(super) fn encode_inventory(
    inventory: &CaptureInventory,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if inventory.devices.len() > MAX_CAPTURE_DEVICES {
        return Err(CodecError::TooManyItems("devices"));
    }
    if inventory
        .devices
        .windows(2)
        .any(|pair| pair[0].stable_key >= pair[1].stable_key)
    {
        return Err(invalid("devices", "devices out of stable-key order"));
    }
    match &inventory.backend {
        Some(backend) if !crate::is_capture_backend_token(backend) => {
            return Err(invalid("backend", backend.clone()));
        }
        backend => push(
            "backend",
            backend.clone().unwrap_or_else(|| NONE.to_owned()),
        )?,
    }
    if let Some(error) = &inventory.error {
        check_text("error", error, MAX_CAPTURE_ERROR_BYTES)?;
    }
    push(
        "error",
        inventory.error.clone().unwrap_or_else(|| NONE.to_owned()),
    )?;
    let devices = inventory
        .devices
        .iter()
        .map(encode_device)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    push("devices", devices)
}

/// Decodes an inventory's `backend`, `error`, and `devices`.
pub(super) fn decode_inventory(
    fields: &mut dyn FieldSource,
) -> Result<CaptureInventory, CodecError> {
    let backend = fields.required("backend")?;
    let backend = if backend == NONE {
        None
    } else if crate::is_capture_backend_token(&backend) {
        Some(backend)
    } else {
        return Err(invalid("backend", backend));
    };
    let error = optional_text("error", fields.required("error")?, MAX_CAPTURE_ERROR_BYTES)?;
    let devices = fields.required("devices")?;
    let devices = if devices.is_empty() {
        Vec::new()
    } else {
        if devices.split(',').take(MAX_CAPTURE_DEVICES + 1).count() > MAX_CAPTURE_DEVICES {
            return Err(CodecError::TooManyItems("devices"));
        }
        devices
            .split(',')
            .map(decode_device)
            .collect::<Result<Vec<_>, _>>()?
    };
    if devices
        .windows(2)
        .any(|pair| pair[0].stable_key >= pair[1].stable_key)
    {
        return Err(invalid("devices", "devices out of stable-key order"));
    }
    Ok(CaptureInventory {
        backend,
        error,
        devices,
    })
}
