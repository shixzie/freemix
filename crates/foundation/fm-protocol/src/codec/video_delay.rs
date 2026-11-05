//! Per-input video delay codec (Protocol 2.160, IN-022).

use crate::{CodecError, MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES, WireInputVideoDelay};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Encodes a `set_input_video_delay` command's input and frames.
pub(super) fn encode_entry(
    entry: WireInputVideoDelay,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if entry.frames > MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES {
        return Err(invalid("frames", entry.frames.to_string()));
    }
    push("input", entry.input.to_string())?;
    push("frames", entry.frames.to_string())
}

/// Decodes a `set_input_video_delay` command's input and frames.
pub(super) fn decode_entry(
    fields: &mut dyn FieldSource,
) -> Result<WireInputVideoDelay, CodecError> {
    let input = fields.required("input")?;
    let input = parse_canonical_input(&input).ok_or_else(|| invalid("input", &input))?;
    let frames = fields.required("frames")?;
    let frames = parse_canonical_u64(&frames)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|frames| *frames <= MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES)
        .ok_or_else(|| invalid("frames", &frames))?;
    Ok(WireInputVideoDelay { input, frames })
}

/// Encodes a snapshot's `input_video_delays`.
pub(super) fn encode_entries(entries: &[WireInputVideoDelay]) -> Result<String, CodecError> {
    let mut previous = None;
    entries
        .iter()
        .map(|entry| {
            if previous.is_some_and(|previous| previous >= entry.input) {
                return Err(invalid("input_video_delays", entry.input.to_string()));
            }
            previous = Some(entry.input);
            if entry.frames > MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES {
                return Err(invalid("input_video_delays", entry.frames.to_string()));
            }
            Ok(format!("{}:{}", entry.input, entry.frames))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(","))
}

/// Decodes a snapshot's `input_video_delays`.
pub(super) fn decode_entries(value: &str) -> Result<Vec<WireInputVideoDelay>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let mut previous = None;
    value
        .split(',')
        .map(|entry| {
            let (input, frames) = entry
                .split_once(':')
                .ok_or_else(|| invalid("input_video_delays", entry))?;
            let input =
                parse_canonical_input(input).ok_or_else(|| invalid("input_video_delays", entry))?;
            if previous.is_some_and(|previous| previous >= input) {
                return Err(invalid("input_video_delays", entry));
            }
            previous = Some(input);
            let frames = parse_canonical_u64(frames)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|frames| *frames <= MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES)
                .ok_or_else(|| invalid("input_video_delays", entry))?;
            Ok(WireInputVideoDelay { input, frames })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> crate::WireInputId {
        crate::WireInputId::new(NonZeroU128::new(value).unwrap())
    }

    #[test]
    fn entries_round_trip_and_are_strict() {
        let entries = [
            WireInputVideoDelay {
                input: input(7),
                frames: 5,
            },
            WireInputVideoDelay {
                input: input(9),
                frames: MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES,
            },
        ];
        let encoded = encode_entries(&entries).unwrap();
        assert_eq!(encoded, "7:5,9:600");
        assert_eq!(decode_entries(&encoded).unwrap(), entries);
        for broken in ["7", "7:5,7:6", "7:601", "0:1", "7:-1", "7:5,9:600:1"] {
            assert!(decode_entries(broken).is_err(), "accepted {broken}");
        }
    }
}
