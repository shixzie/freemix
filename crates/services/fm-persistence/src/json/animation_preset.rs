//! The animation preset library in `project.json` (schema 34, GX-011), and
//! the preset body shared with `.fmpreset` files.
//!
//! ```text
//! "animation_presets": [
//!   {"id": 1, "name": "Swoosh", "phase": "in", "timebase": "30/1",
//!    "frames": 15, "width": 400, "height": 100, "tracks": [
//!     {"property": "position_x",
//!      "keys": [{"frame": 0, "value": -400, "easing": "cubic-out"},
//!               {"frame": 15, "value": 0, "easing": "linear"}]}]}]
//! ```
//!
//! A preset is one phase of one target ([`fm_model::fm_animation::PhaseMotion`]):
//! its timebase (a lowest-terms `"numerator/denominator"`), its length in
//! frames, the box its crop keys were measured against (`width` x `height`,
//! both nonzero), and its tracks in property order without repeats.
//! Properties and easings are the title template names (`color` keys are
//! `"#rrggbbaa"` strings; `"step"` easing is accepted beside the preset
//! ids and `{"cubic_bezier": [...]}`). Every `fm_animation` bound applies,
//! and a preset holds at most [`fm_model::fm_animation::MAX_MOTION_KEYS`]
//! keys.

use core::{fmt::Write as _, num::NonZeroU32};

use fm_model::{
    AnimationPresetId, AnimationPresetLibrary, UserAnimationPreset,
    fm_animation::{Phase, PhaseMotion},
};
use fm_preset_file::json::Object as SharedObject;

use super::{DecodeError, Object, escape_string, parse_array, syntax};

/// Writes `,\n    "animation_presets": [...]` after the grading fields.
pub(crate) fn write_animation_presets(output: &mut String, library: &AnimationPresetLibrary) {
    output.push_str(",\n    \"animation_presets\": [");
    for (index, preset) in library.presets().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"id\": {}, \"name\": \"", preset.id).expect("writing to a string");
        escape_string(output, &preset.name);
        output.push_str("\", ");
        write_body(output, preset.phase, &preset.motion);
        output.push('}');
    }
    output.push_str(if library.is_empty() { "]" } else { "\n    ]" });
}

/// Writes the preset body (every field but `id` and `name`) without braces:
/// `"phase": ..., "timebase": ..., ..., "tracks": [...]`.
pub(crate) fn write_body(output: &mut String, phase: Phase, motion: &PhaseMotion) {
    fm_preset_file::motion::write_body(output, phase, motion);
}

/// Parses `"animation_presets"` from the project object. Ids and names are
/// checked by project validation.
pub(crate) fn parse_animation_presets(
    object: &mut Object,
) -> Result<AnimationPresetLibrary, DecodeError> {
    let presets = parse_array(
        object.take("animation_presets")?,
        "animation_presets",
        |value| {
            // The shared object reader, so the body parses exactly as a
            // `.fmpreset` file's does.
            let mut object = SharedObject::new(value, "animation preset")?;
            let id = NonZeroU32::new(object.u32("id")?)
                .map(AnimationPresetId::new)
                .ok_or_else(|| syntax("animation preset id must be nonzero"))?;
            let name = object.string("name")?;
            let (phase, motion) = fm_preset_file::motion::parse_body(&mut object)?;
            object.finish()?;
            Ok(UserAnimationPreset {
                id,
                name,
                phase,
                motion,
            })
        },
    )?;
    Ok(AnimationPresetLibrary::from_presets_unchecked(presets))
}
