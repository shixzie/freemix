//! PTZ preset virtual inputs over the control protocol (IN-028).
//!
//! A client asks for the show's preset virtual inputs with a
//! [`PtzPresetsRequest`] and receives a [`PtzPresetsResponse`]. The preset
//! list travels as one canonical string field so the record codec stays a
//! single line: entries are `;`-separated and each entry is
//! `camera_hex,preset,enabled,thumbnail_hex_or_dash,name_hex`.
//! Every text field is hex-encoded, so names and thumbnails cannot collide
//! with the separators.

use core::fmt;

use crate::{CodecError, ProtocolVersion};

/// Most preset virtual inputs one response may carry.
pub const MAX_PTZ_PRESETS: usize = 256;

/// One preset virtual input as the daemon exposes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtzPresetStatus {
    /// The camera the preset belongs to.
    pub camera: String,
    /// The camera preset slot.
    pub preset: u16,
    /// The operator-visible name.
    pub name: String,
    /// An opaque thumbnail reference (asset URI or host path).
    pub thumbnail: Option<String>,
    /// Whether the input is selectable.
    pub enabled: bool,
}

/// A client's request for the show's preset virtual inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtzPresetsRequest {
    pub protocol: ProtocolVersion,
    pub request_id: String,
}

/// The daemon's answer to a [`PtzPresetsRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtzPresetsResponse {
    pub protocol: ProtocolVersion,
    pub request_id: String,
    /// Entries in ascending camera and preset order.
    pub presets: Vec<PtzPresetStatus>,
}

/// Encodes preset statuses as the response's canonical field.
///
/// # Errors
///
/// Returns [`CodecError::InvalidField`] past [`MAX_PTZ_PRESETS`].
pub fn encode_presets(presets: &[PtzPresetStatus]) -> Result<String, CodecError> {
    if presets.len() > MAX_PTZ_PRESETS {
        return Err(CodecError::InvalidField {
            field: "presets",
            value: format!("{} entries", presets.len()),
        });
    }
    let mut output = String::new();
    for (index, preset) in presets.iter().enumerate() {
        if index > 0 {
            output.push(';');
        }
        output.push_str(&hex(preset.camera.as_bytes()));
        output.push(',');
        output.push_str(&preset.preset.to_string());
        output.push(',');
        output.push(if preset.enabled { '1' } else { '0' });
        output.push(',');
        match &preset.thumbnail {
            Some(thumbnail) => output.push_str(&hex(thumbnail.as_bytes())),
            None => output.push('-'),
        }
        output.push(',');
        output.push_str(&hex(preset.name.as_bytes()));
    }
    if output.len() > crate::MAX_FIELD_VALUE_BYTES {
        return Err(CodecError::FieldValueTooLong);
    }
    Ok(output)
}

/// Decodes the response's canonical preset field.
///
/// # Errors
///
/// Returns [`CodecError::InvalidField`] for a malformed entry or more than
/// [`MAX_PTZ_PRESETS`] entries.
pub fn decode_presets(text: &str) -> Result<Vec<PtzPresetStatus>, CodecError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let invalid = || CodecError::InvalidField {
        field: "presets",
        value: text.to_owned(),
    };
    let mut presets = Vec::new();
    for entry in text.split(';') {
        let mut fields = entry.split(',');
        let camera = decode_hex(fields.next().ok_or_else(invalid)?).ok_or_else(invalid)?;
        let preset = fields.next().ok_or_else(invalid)?;
        if preset.is_empty()
            || (preset != "0" && preset.starts_with('0'))
            || !preset.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        let preset = preset.parse::<u16>().map_err(|_| invalid())?;
        let enabled = match fields.next().ok_or_else(invalid)? {
            "1" => true,
            "0" => false,
            _ => return Err(invalid()),
        };
        let thumbnail = match fields.next().ok_or_else(invalid)? {
            "-" => None,
            value => Some(decode_hex(value).ok_or_else(invalid)?),
        };
        let name = decode_hex(fields.next().ok_or_else(invalid)?).ok_or_else(invalid)?;
        if fields.next().is_some() || camera.is_empty() || name.is_empty() {
            return Err(invalid());
        }
        presets.push(PtzPresetStatus {
            camera,
            preset,
            name,
            thumbnail,
            enabled,
        });
        if presets.len() > MAX_PTZ_PRESETS {
            return Err(invalid());
        }
    }
    Ok(presets)
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn decode_hex(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let pair = core::str::from_utf8(pair).ok()?;
        bytes.push(u8::from_str_radix(pair, 16).ok()?);
    }
    String::from_utf8(bytes).ok()
}

impl fmt::Display for PtzPresetStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} preset {} `{}` ({})",
            self.camera,
            self.preset,
            self.name,
            if self.enabled { "enabled" } else { "disabled" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(
        camera: &str,
        slot: u16,
        name: &str,
        thumbnail: Option<&str>,
        enabled: bool,
    ) -> PtzPresetStatus {
        PtzPresetStatus {
            camera: camera.to_owned(),
            preset: slot,
            name: name.to_owned(),
            thumbnail: thumbnail.map(str::to_owned),
            enabled,
        }
    }

    #[test]
    fn presets_round_trip_and_are_canonical() {
        let presets = vec![
            preset("cam-a", 3, "Wide", Some("asset://thumbs/wide.png"), true),
            preset("cam-b", 0, "Close", None, false),
        ];
        let text = encode_presets(&presets).unwrap();
        assert_eq!(
            text,
            "63616d2d61,3,1,61737365743a2f2f7468756d62732f776964652e706e67,57696465;\
             63616d2d62,0,0,-,436c6f7365"
        );
        assert_eq!(decode_presets(&text).unwrap(), presets);
        assert_eq!(
            encode_presets(&decode_presets(&text).unwrap()).unwrap(),
            text
        );
        assert_eq!(decode_presets("").unwrap(), Vec::new());
    }

    #[test]
    fn presets_are_strict() {
        for text in [
            "zz,1,1,-,6e616d65",
            "63616d,1,2,-,6e616d65",
            "63616d,01,1,-,6e616d65",
            "63616d,+1,1,-,6e616d65",
            "63616d,1,1,-,",
            "63616d,1,1,-,6e616d65,extra",
            "63616d,1,1,zz,6e616d65",
            ";",
        ] {
            assert!(decode_presets(text).is_err(), "accepted {text:?}");
        }
        let too_many = vec![preset("cam", 1, "x", None, true); MAX_PTZ_PRESETS + 1];
        assert!(encode_presets(&too_many).is_err());
    }
}
