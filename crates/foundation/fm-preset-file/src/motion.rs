//! The strict JSON body of one saved animation motion, shared by the
//! project's `animation_presets` and `.fmpreset` files, and the animation
//! names shared with the `.fmtitle` clip encoding.
//!
//! ```text
//! "phase": "in", "timebase": "30/1", "frames": 15, "width": 400,
//! "height": 100, "tracks": [
//!   {"property": "position_x",
//!    "keys": [{"frame": 0, "value": -400, "easing": "cubic-out"},
//!             {"frame": 15, "value": 0, "easing": "linear"}]}]
//! ```
//!
//! A body is one phase of one target ([`PhaseMotion`]): the phase it was
//! saved from, its timebase (a lowest-terms `"numerator/denominator"`), its
//! length in frames, the box its crop keys were measured against
//! (`width` x `height`, both nonzero), and its tracks in property order
//! without repeats. Properties and easings use the title template names
//! (`color` keys are `"#rrggbbaa"` strings; `"step"` easing is accepted
//! beside the preset ids and `{"cubic_bezier": [x1, y1, x2, y2]}`). Every
//! `fm_animation` bound applies, and a motion holds at most
//! [`fm_animation::MAX_MOTION_KEYS`] keys.

use std::fmt::Write as _;

use fm_animation::{
    Clip, CubicBezier, Easing, EasingPreset, Keyframe, MotionTarget, Phase, PhaseMotion, Property,
    Rgba, Timebase, Track, TrackKey, Value as Animated,
};
use fm_types::FrameRate;

use crate::json::{
    Object, SyntaxError, Value, parse_array, parse_hex_rgba, syntax, unknown_enum, write_hex_rgba,
};

/// The document name of a phase.
#[must_use]
pub const fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::In => "in",
        Phase::Out => "out",
    }
}

/// The phase named `name`.
///
/// # Errors
///
/// [`SyntaxError`] for any other name.
pub fn parse_phase(name: &str) -> Result<Phase, SyntaxError> {
    match name {
        "in" => Ok(Phase::In),
        "out" => Ok(Phase::Out),
        value => Err(unknown_enum("animation phase", value)),
    }
}

/// The document name of an animated property.
#[must_use]
pub const fn property_name(property: Property) -> &'static str {
    match property {
        Property::PositionX => "position_x",
        Property::PositionY => "position_y",
        Property::ScaleX => "scale_x",
        Property::ScaleY => "scale_y",
        Property::Rotation => "rotation",
        Property::Opacity => "opacity",
        Property::CropLeft => "crop_left",
        Property::CropTop => "crop_top",
        Property::CropRight => "crop_right",
        Property::CropBottom => "crop_bottom",
        Property::Color => "color",
        Property::MattePoint1X => "matte_point_1_x",
        Property::MattePoint1Y => "matte_point_1_y",
        Property::MattePoint2X => "matte_point_2_x",
        Property::MattePoint2Y => "matte_point_2_y",
        Property::MattePoint3X => "matte_point_3_x",
        Property::MattePoint3Y => "matte_point_3_y",
        Property::MattePoint4X => "matte_point_4_x",
        Property::MattePoint4Y => "matte_point_4_y",
        Property::MattePoint5X => "matte_point_5_x",
        Property::MattePoint5Y => "matte_point_5_y",
        Property::MattePoint6X => "matte_point_6_x",
        Property::MattePoint6Y => "matte_point_6_y",
        Property::MattePoint7X => "matte_point_7_x",
        Property::MattePoint7Y => "matte_point_7_y",
        Property::MattePoint8X => "matte_point_8_x",
        Property::MattePoint8Y => "matte_point_8_y",
    }
}

/// The property named `name`.
///
/// # Errors
///
/// [`SyntaxError`] for any other name.
pub fn parse_property(name: &str) -> Result<Property, SyntaxError> {
    Property::ALL
        .into_iter()
        .find(|property| property_name(*property) == name)
        .ok_or_else(|| unknown_enum("animated property", name))
}

/// Parses exactly `numerator/denominator` in lowest terms.
///
/// # Errors
///
/// [`SyntaxError`] for anything else.
pub fn parse_timebase(text: &str) -> Result<Timebase, SyntaxError> {
    let invalid = || {
        syntax(format!(
            "timebase `{text}` is not a lowest-terms `N/D` rate"
        ))
    };
    let (numerator, denominator) = text.split_once('/').ok_or_else(invalid)?;
    let number = |digits: &str| -> Result<u32, SyntaxError> {
        if digits.is_empty()
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return Err(invalid());
        }
        digits.parse().map_err(|_| invalid())
    };
    let (numerator, denominator) = (number(numerator)?, number(denominator)?);
    let rate = FrameRate::new(numerator, denominator).map_err(|_| invalid())?;
    if rate.numerator() != numerator || rate.denominator() != denominator {
        return Err(invalid());
    }
    Ok(Timebase::new(rate))
}

/// Parses an easing: a preset id string or `{"cubic_bezier": [x1, y1, x2,
/// y2]}`. (`"step"` is not a preset id; callers that accept it check first.)
///
/// # Errors
///
/// [`SyntaxError`] for an unknown id or an invalid curve.
pub fn parse_easing(value: Value) -> Result<Easing, SyntaxError> {
    match value {
        Value::String(id) => EasingPreset::from_id(&id)
            .map(EasingPreset::easing)
            .ok_or_else(|| unknown_enum("easing", &id)),
        value => {
            let mut object = Object::new(value, "easing")?;
            let Value::Array(points) = object.take("cubic_bezier")? else {
                return Err(syntax("cubic_bezier must be an array of four integers"));
            };
            object.finish()?;
            let [x1, y1, x2, y2] = <[Value; 4]>::try_from(points)
                .map_err(|_| syntax("cubic_bezier must hold exactly four integers"))?;
            let unsigned = |value: Value| match value {
                Value::Number(number) => {
                    u32::try_from(number).map_err(|_| syntax("cubic_bezier x is out of range"))
                }
                _ => Err(syntax("cubic_bezier x must be an unsigned integer")),
            };
            let signed = |value: Value| match value {
                Value::Number(number) => {
                    i32::try_from(number).map_err(|_| syntax("cubic_bezier y is out of range"))
                }
                Value::NegativeNumber(number) => i128::try_from(number)
                    .ok()
                    .and_then(|number| i32::try_from(-number).ok())
                    .ok_or_else(|| syntax("cubic_bezier y is out of range")),
                _ => Err(syntax("cubic_bezier y must be an integer")),
            };
            CubicBezier::new(unsigned(x1)?, signed(y1)?, unsigned(x2)?, signed(y2)?)
                .map(Easing::CubicBezier)
                .map_err(|error| syntax(error.to_string()))
        }
    }
}

/// Writes a motion key's easing: `"step"`, a preset id, or a bezier.
fn write_motion_easing(output: &mut String, easing: Easing) {
    match easing {
        Easing::Step => output.push_str("\"step\""),
        Easing::CubicBezier(curve) => {
            let (x1, y1, x2, y2) = curve.control_points();
            write!(output, "{{\"cubic_bezier\": [{x1}, {y1}, {x2}, {y2}]}}")
                .expect("writing to a string cannot fail");
        }
        other => {
            let preset = EasingPreset::from_easing(other)
                .expect("every easing but step and a bezier has a stable preset id");
            write!(output, "\"{}\"", preset.id()).expect("writing to a string cannot fail");
        }
    }
}

/// Writes the body fields without braces: `"phase": ..., "timebase": ...,
/// ..., "tracks": [...]`.
pub fn write_body(output: &mut String, phase: Phase, motion: &PhaseMotion) {
    let rate = motion.timebase().rate();
    write!(
        output,
        "\"phase\": \"{}\", \"timebase\": \"{}/{}\", \"frames\": {}, \"width\": {}, \
         \"height\": {}, \"tracks\": [",
        phase_name(phase),
        rate.numerator(),
        rate.denominator(),
        motion.frames(),
        motion.width(),
        motion.height()
    )
    .expect("writing to a string");
    for (index, track) in motion.clip().tracks().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"property\": \"{}\", \"keys\": [",
            property_name(track.key.property)
        )
        .expect("writing to a string");
        for (index, key) in track.keys.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            write!(output, "{{\"frame\": {}, \"value\": ", key.frame).expect("writing to a string");
            match key.value {
                Animated::Scalar(value) => {
                    write!(output, "{value}").expect("writing to a string");
                }
                Animated::Color(color) => {
                    output.push('"');
                    write_hex_rgba(output, [color.r, color.g, color.b, color.a]);
                    output.push('"');
                }
            }
            output.push_str(", \"easing\": ");
            write_motion_easing(output, key.easing);
            output.push('}');
        }
        output.push_str("]}");
    }
    output.push(']');
}

/// Parses the body fields from `object` (see [`write_body`]); the caller
/// finishes the object.
///
/// # Errors
///
/// [`SyntaxError`] for a missing, unknown, or mistyped field, unsorted or
/// repeated tracks, or a motion `fm_animation` refuses.
pub fn parse_body(object: &mut Object) -> Result<(Phase, PhaseMotion), SyntaxError> {
    let phase = parse_phase(&object.string("phase")?)?;
    let timebase = parse_timebase(&object.string("timebase")?)?;
    let frames = object.u32("frames")?;
    let width = object.u32("width")?;
    let height = object.u32("height")?;
    let tracks = parse_array(
        object.take("tracks")?,
        "animation preset tracks",
        parse_track,
    )?;
    if tracks.windows(2).any(|pair| pair[0].key >= pair[1].key) {
        return Err(syntax(
            "animation preset tracks must be sorted by property without repeats",
        ));
    }
    let clip = Clip::from_tracks(timebase, frames, 0, tracks)
        .map_err(|error| syntax(format!("animation preset: {error}")))?;
    let motion = PhaseMotion::new(clip, width, height)
        .map_err(|error| syntax(format!("animation preset: {error}")))?;
    Ok((phase, motion))
}

fn parse_track(value: Value) -> Result<Track<MotionTarget>, SyntaxError> {
    let mut object = Object::new(value, "animation preset track")?;
    let property = parse_property(&object.string("property")?)?;
    let keys = parse_array(object.take("keys")?, "animation preset keys", |value| {
        let mut key = Object::new(value, "animation preset key")?;
        let frame = key.u32("frame")?;
        let value = if property == Property::Color {
            let [r, g, b, a] = parse_hex_rgba(&key.string("value")?)?;
            Animated::Color(Rgba::new(r, g, b, a))
        } else {
            Animated::Scalar(key.i64("value")?)
        };
        let easing = match key.take("easing")? {
            Value::String(id) if id == "step" => Easing::Step,
            other => parse_easing(other)?,
        };
        key.finish()?;
        Ok::<_, SyntaxError>(Keyframe::new(frame, value, easing))
    })?;
    object.finish()?;
    Ok(Track::new(
        TrackKey::new(MotionTarget, Phase::In, property),
        keys,
    ))
}
