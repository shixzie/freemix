//! A capture device input in `project.json` (schema 44, IN-001).
//!
//! ```text
//! {"type": "device", "stable_key": "v4l2:usb-0000:00:14.0-1#C920",
//!  "capture": {"format": "1920x1080@30/1:yuyv:p" | null,
//!              "audio": "none|paired|alsa:<device>", "loss": "hold|black"}}
//! ```
//!
//! Every field is required. `format` is a
//! [`fm_types::capture::CaptureFormat`] token, or `null` to let the runtime
//! pick the device format nearest the program; `audio` is a
//! [`fm_types::capture::CaptureAudio`] token. Bounds are those of
//! [`fm_types::capture::CaptureSettings::validate`] and
//! [`fm_types::capture::validate_stable_key`].

use fm_model::InputKind;
use fm_types::capture::{
    CaptureAudio, CaptureFormat, CaptureLossSlate, CaptureSettings, validate_stable_key,
};

use super::{DecodeError, Object, Value, syntax, unknown_enum};

/// Writes `{"type": "device", "stable_key": ..., "capture": {...}}`.
pub(crate) fn write_device_input(output: &mut String, stable_key: &str, capture: &CaptureSettings) {
    output.push_str("{\"type\": \"device\", \"stable_key\": \"");
    super::escape_string(output, stable_key);
    output.push_str("\", \"capture\": {\"format\": ");
    match &capture.format {
        Some(format) => {
            output.push('"');
            output.push_str(&format.token());
            output.push('"');
        }
        None => output.push_str("null"),
    }
    output.push_str(", \"audio\": \"");
    super::escape_string(output, &capture.audio.token());
    output.push_str("\", \"loss\": \"");
    output.push_str(capture.loss.token());
    output.push_str("\"}}");
}

/// Parses the fields of a `device` input kind after its `type`.
pub(crate) fn parse_device_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let stable_key = object.string("stable_key")?;
    validate_stable_key(&stable_key).map_err(|error| syntax(error.to_string()))?;
    let mut capture = Object::new(object.take("capture")?, "device capture")?;
    let format = match capture.take("format")? {
        Value::Null => None,
        Value::String(token) => {
            Some(CaptureFormat::from_token(&token).map_err(|error| syntax(error.to_string()))?)
        }
        _ => return Err(syntax("field `format` must be a string or null")),
    };
    let audio = CaptureAudio::from_token(&capture.string("audio")?)
        .map_err(|error| syntax(error.to_string()))?;
    let loss = {
        let token = capture.string("loss")?;
        CaptureLossSlate::from_token(&token)
            .ok_or_else(|| unknown_enum("capture loss slate", &token))?
    };
    capture.finish()?;
    Ok(InputKind::Device {
        stable_key,
        capture: CaptureSettings {
            format,
            audio,
            loss,
        },
    })
}
