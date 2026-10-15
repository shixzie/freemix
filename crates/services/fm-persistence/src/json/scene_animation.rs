//! Scene layer animations in `project.json` (schema 32, GX-011).
//!
//! Every scene carries `"animation"`, the title template animation document
//! keyed by layer instead of element:
//!
//! ```json
//! "animation": {"timebase": "30/1", "revision": 3, "in_frames": 15,
//!               "out_frames": 10, "tracks": [
//!   {"layer": 2, "phase": "in", "property": "position_x",
//!    "keys": [{"frame": 0, "value": -400, "easing": "cubic-out"},
//!             {"frame": 15, "value": 0, "easing": "linear"}]}]}
//! ```
//!
//! The timebase is a lowest-terms `"numerator/denominator"`; tracks appear in
//! clip order (layer, then `in` before `out`, then property) without
//! repeats. Properties are the title template names except `color`, which
//! scene layers do not animate. Easing is a stable `fm_animation` preset id,
//! `"step"`, or `{"cubic_bezier": [x1, y1, x2, y2]}` with Q16 control points.
//! Every `fm_animation` limit applies, and every track must name a layer of
//! its scene (checked by project validation).

use core::fmt::Write as _;

use fm_model::{
    LayerId, SceneAnimation, SceneClip,
    fm_animation::{Clip, Easing, Keyframe, Phase, Property, Track, TrackKey, Value as Animated},
    scene_animates,
};

use super::{DecodeError, Object, Value, parse_array, syntax, unknown_enum};
use crate::title_template::{
    parse_easing, parse_timebase, phase_name, property_name, write_easing,
};

/// Writes `"animation": {...}` at `indent` spaces (the key's indentation).
pub(crate) fn write_scene_animation(
    output: &mut String,
    animation: &SceneAnimation,
    indent: usize,
) {
    let clip = &animation.clip;
    let rate = clip.timebase().rate();
    let pad = " ".repeat(indent);
    write!(
        output,
        "{pad}\"animation\": {{\"timebase\": \"{}/{}\", \"revision\": {}, \"in_frames\": {}, \"out_frames\": {}, \"tracks\": [",
        rate.numerator(),
        rate.denominator(),
        animation.revision,
        clip.in_frames(),
        clip.out_frames()
    )
    .expect("writing to a string cannot fail");
    for (index, track) in clip.tracks().iter().enumerate() {
        output.push_str(if index == 0 { "\n" } else { ",\n" });
        write!(
            output,
            "{pad}  {{\"layer\": {}, \"phase\": \"{}\", \"property\": \"{}\", \"keys\": [",
            track.key.target,
            phase_name(track.key.phase),
            property_name(track.key.property)
        )
        .expect("writing to a string cannot fail");
        for (index, key) in track.keys.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            write!(output, "{{\"frame\": {}, \"value\": ", key.frame)
                .expect("writing to a string cannot fail");
            // Scene clips hold scalars only (no colour property).
            if let Animated::Scalar(value) = key.value {
                write!(output, "{value}").expect("writing to a string cannot fail");
            } else {
                output.push('0');
            }
            output.push_str(", \"easing\": ");
            if key.easing == Easing::Step {
                output.push_str("\"step\"");
            } else {
                // Every easing but Step has a stable id or is a bezier.
                write_easing(output, key.easing).expect("only step easing has no stable id");
            }
            output.push('}');
        }
        output.push_str("]}");
    }
    if clip.tracks().is_empty() {
        output.push_str("]}");
    } else {
        write!(output, "\n{pad}]}}").expect("writing to a string cannot fail");
    }
}

/// Parses one scene's `"animation"` object. Layer references are checked by
/// project validation.
pub(crate) fn parse_scene_animation(value: Value) -> Result<SceneAnimation, DecodeError> {
    let mut object = Object::new(value, "scene animation")?;
    let timebase = parse_timebase(&object.string("timebase")?)?;
    let revision = object.u64("revision")?;
    let in_frames = object.u32("in_frames")?;
    let out_frames = object.u32("out_frames")?;
    let tracks = parse_array(
        object.take("tracks")?,
        "scene animation tracks",
        parse_track,
    )?;
    object.finish()?;
    if tracks.windows(2).any(|pair| pair[0].key >= pair[1].key) {
        return Err(syntax(
            "scene animation tracks must be sorted by layer, phase, and property without repeats",
        ));
    }
    let clip: SceneClip = Clip::from_tracks(timebase, in_frames, out_frames, tracks)
        .map_err(|error| syntax(format!("scene animation: {error}")))?;
    Ok(SceneAnimation::new(clip, revision))
}

fn parse_track(value: Value) -> Result<Track<LayerId>, DecodeError> {
    let mut object = Object::new(value, "scene animation track")?;
    let layer = object.u32("layer")?;
    let layer =
        LayerId::new(layer).ok_or_else(|| syntax("scene animation layer must be nonzero"))?;
    let phase = match object.string("phase")?.as_str() {
        "in" => Phase::In,
        "out" => Phase::Out,
        value => return Err(unknown_enum("animation phase", value)),
    };
    let property = {
        let name = object.string("property")?;
        Property::ALL
            .into_iter()
            .filter(|property| scene_animates(*property))
            .find(|property| property_name(*property) == name)
            .ok_or_else(|| unknown_enum("scene animated property", &name))?
    };
    let keys = parse_array(object.take("keys")?, "scene animation keys", parse_key)?;
    object.finish()?;
    Ok(Track::new(TrackKey::new(layer, phase, property), keys))
}

fn parse_key(value: Value) -> Result<Keyframe, DecodeError> {
    let mut object = Object::new(value, "scene animation key")?;
    let frame = object.u32("frame")?;
    let value = Animated::Scalar(object.i64("value")?);
    let easing = match object.take("easing")? {
        Value::String(id) if id == "step" => Easing::Step,
        other => parse_easing(other)?,
    };
    object.finish()?;
    Ok(Keyframe::new(frame, value, easing))
}
