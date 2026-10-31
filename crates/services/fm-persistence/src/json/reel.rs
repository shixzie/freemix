//! Highlight reels in `project.json` (OR-013, schema 83; dissolves since
//! schema 84, music beds since schema 86).
//!
//! ```text
//! "reels": [{"id": 1, "name": "Goals", "clips": [
//!   {"event": 2, "angle": 1, "trim_in": 0, "trim_out": 0, "speed": 50, "dissolve": 0},
//!   {"event": 3, "angle": 1, "trim_in": 0, "trim_out": 0, "speed": 100, "dissolve": 15}
//! ], "music": {"path": "/srv/music/bed.wav", "level": 80, "duck": 25}}]
//! ```
//!
//! Reels are written in display order and their clips in play order. Each
//! clip names a replay event, a recorded angle, trims before and after the
//! event marks, a speed in percent, and the frames it dissolves in from the
//! clip before (0 for a cut). A reel without a music bed omits `music`; a
//! bed names its file and its level and ducked level in percent. An empty
//! reel list is omitted. Unknown fields
//! are refused; repeated ids, unusable names, angles that cannot be
//! recorded, unsupported speeds, out-of-range trims, and a dissolving first
//! clip or overlong dissolve are also reported by project validation.

use core::fmt::Write as _;

use fm_types::InputId;
use fm_types::reel::{ReelClip, ReelMusic, ReelSpec};

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
                 \"speed\": {}, \"reverse\": {}, \"dissolve\": {}}}",
                clip.event,
                clip.angle,
                clip.trim_in_frames,
                clip.trim_out_frames,
                clip.speed_percent,
                clip.reverse,
                clip.dissolve_frames
            )
            .expect("writing to a string cannot fail");
        }
        output.push(']');
        if let Some(music) = &reel.music {
            output.push_str(", \"music\": {\"path\": \"");
            escape_string(output, &music.path);
            write!(
                output,
                "\", \"level\": {}, \"duck\": {}}}",
                music.level_percent, music.duck_percent
            )
            .expect("writing to a string cannot fail");
        }
        output.push('}');
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
    let music = reel.take_optional("music").map(parse_music).transpose()?;
    reel.finish()?;
    Ok(ReelSpec {
        id,
        name,
        clips,
        music,
    })
}

fn parse_music(value: Value) -> Result<ReelMusic, DecodeError> {
    let mut music = Object::new(value, "reel music")?;
    let path = music.string("path")?;
    let level_percent = music.u8("level")?;
    let duck_percent = music.u8("duck")?;
    music.finish()?;
    Ok(ReelMusic {
        path,
        level_percent,
        duck_percent,
    })
}

fn parse_clip(value: Value) -> Result<ReelClip, DecodeError> {
    let mut clip = Object::new(value, "reel clip")?;
    let event = clip.u32("event")?;
    let angle = InputId::new(clip.nonzero_u128("angle")?);
    let trim_in_frames = clip.u32("trim_in")?;
    let trim_out_frames = clip.u32("trim_out")?;
    let speed_percent = clip.u8("speed")?;
    let reverse = clip.boolean("reverse")?;
    let dissolve_frames = clip.u32("dissolve")?;
    clip.finish()?;
    Ok(ReelClip {
        event,
        angle,
        trim_in_frames,
        trim_out_frames,
        speed_percent,
        reverse,
        dissolve_frames,
    })
}
