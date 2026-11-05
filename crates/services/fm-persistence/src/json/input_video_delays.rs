//! Per-input video delays in `project.json` (IN-022, schema 110).
//!
//! ```text
//! "input_video_delays": [
//!   {"input": 7, "frames": 5},
//!   {"input": 9, "frames": 30}
//! ]
//! ```
//!
//! Entries are strictly ascending by input and hold at most
//! [`fm_model::MAX_INPUT_VIDEO_DELAY_FRAMES`]. Project validation reports
//! unknown inputs and oversized delays.

use core::fmt::Write as _;

use fm_model::{MAX_INPUT_VIDEO_DELAY_FRAMES, Project};
use fm_types::InputId;

use super::{DecodeError, Object, parse_array, syntax};

/// Writes `,\n    "input_video_delays": [...]` when any input is delayed.
pub(crate) fn write_input_video_delays(output: &mut String, project: &Project) {
    if project.input_video_delays().is_empty() {
        return;
    }
    output.push_str(",\n    \"input_video_delays\": [");
    for (index, (input, frames)) in project.input_video_delays().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"input\": {input}, \"frames\": {frames}}}")
            .expect("writing to a string cannot fail");
    }
    output.push_str("\n    ]");
}

/// Parses `input_video_delays`, defaulting to none when the key is absent.
pub(crate) fn parse_input_video_delays(
    object: &mut Object,
) -> Result<Vec<(InputId, u32)>, DecodeError> {
    let Some(value) = object.take_optional("input_video_delays") else {
        return Ok(Vec::new());
    };
    let delays = parse_array(value, "input_video_delays", |value| {
        let mut entry = Object::new(value, "input video delay")?;
        let input = InputId::new(entry.nonzero_u128("input")?);
        let frames = entry.u32("frames")?;
        entry.finish()?;
        Ok((input, frames))
    })?;
    if delays.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "input_video_delays must be in strictly ascending input order",
        ));
    }
    let Some((input, frames)) = delays
        .iter()
        .find(|(_, frames)| *frames > MAX_INPUT_VIDEO_DELAY_FRAMES)
    else {
        return Ok(delays);
    };
    Err(syntax(format!(
        "input {input} video delay {frames} exceeds {MAX_INPUT_VIDEO_DELAY_FRAMES} frames"
    )))
}
