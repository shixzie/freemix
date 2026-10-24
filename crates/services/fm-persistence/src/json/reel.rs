//! Highlight reels in `project.json` (OR-013, schema 83).
//!
//! ```text
//! "reels": [{"id": 1, "name": "Goals", "clips": [
//!   {"event": 2, "angle": 1, "trim_in": 0, "trim_out": 0, "speed": 50}
//! ]}]
//! ```
//!
//! Reels are written in display order and their clips in play order. Each
//! clip names a replay event, a recorded angle, trims before and after the
//! event marks, and a speed in percent. An empty reel list is omitted.
//! Unknown fields are refused; repeated ids, unusable names, angles that
//! cannot be recorded, unsupported speeds, and out-of-range trims are also
//! reported by project validation.

use core::fmt::Write as _;

use fm_types::InputId;
use fm_types::reel::{ReelClip, ReelSpec};

use super::{DecodeError, Object, Value, escape_string, parse_array, syntax};

/// Writes `,\n    "reels": [...]`; an empty list is omitted.
pub(crate) fn write_reels(output: &mut String, project: &fm_model::Project) {
    let reels = project.reels();
    if reels.is_empty() {
        return;
    }
    output.push_str(",\n    \"reels\": [");
    for (index, reel) in reels.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        write!(output, "{{\"id\": {}, \"name\": \"", reel.id)
            .expect("writing to a string cannot fail");
        escape_string(output, &reel.name);
        output.push_str("\", \"clips\": [");
        for (clip_index, clip) in reel.clips.iter().enumerate() {
            if clip_index > 0 {
                output.push_str(", ");
            }
            write!(
                output,
                "{{\"event\": {}, \"angle\": {}, \"trim_in\": {}, \"trim_out\": {}, \
                 \"speed\": {}}}",
                clip.event,
                clip.angle,
                clip.trim_in_frames,
                clip.trim_out_frames,
                clip.speed_percent
            )
            .expect("writing to a string cannot fail");
        }
        output.push_str("]}");
    }
    output.push(']');
}

/// Parses `"reels"` from the project object; an absent list is empty.
pub(crate) fn parse_reels(object: &mut Object) -> Result<Vec<ReelSpec>, DecodeError> {
    let Some(value) = object.take_optional("reels") else {
        return Ok(Vec::new());
    };
    let reels = parse_array(value, "reels", parse_reel)?;
    for (index, reel) in reels.iter().enumerate() {
        if reels[..index].iter().any(|previous| previous.id == reel.id) {
            return Err(syntax("reels must not repeat a reel id"));
        }
    }
    Ok(reels)
}

fn parse_reel(value: Value) -> Result<ReelSpec, DecodeError> {
    let mut reel = Object::new(value, "reel")?;
    let id = reel.u64("id")?;
    let name = reel.string("name")?;
    let clips = parse_array(reel.take("clips")?, "reel clips", parse_clip)?;
    reel.finish()?;
    Ok(ReelSpec { id, name, clips })
}

fn parse_clip(value: Value) -> Result<ReelClip, DecodeError> {
    let mut clip = Object::new(value, "reel clip")?;
    let event = clip.u32("event")?;
    let angle = InputId::new(clip.nonzero_u128("angle")?);
    let trim_in_frames = clip.u32("trim_in")?;
    let trim_out_frames = clip.u32("trim_out")?;
    let speed_percent = clip.u8("speed")?;
    clip.finish()?;
    Ok(ReelClip {
        event,
        angle,
        trim_in_frames,
        trim_out_frames,
        speed_percent,
    })
}
