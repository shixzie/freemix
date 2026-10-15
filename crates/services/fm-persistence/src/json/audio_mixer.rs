//! Strip groups and Fade-to-Black output selection in `project.json`
//! (schema 39, AU-001 and SW-004).
//!
//! ```text
//! "audio_groups": [
//!   {"name": "Group 1", "gain_milli_db": 0, "muted": false},
//!   {"name": "Hosts", "gain_milli_db": -6000, "muted": true},
//!   {"name": "Group 3", "gain_milli_db": 0, "muted": false},
//!   {"name": "Group 4", "gain_milli_db": 0, "muted": false}
//! ],
//! "fade_to_black_excluded_outputs": [5]
//! ```
//!
//! Exactly [`AUDIO_GROUP_COUNT`] groups, group 1 first. Every strip in
//! `input_audio_strips` also carries `pan_basis_points`, `label` (empty for
//! none), and `group` (a group number or `null`). Excluded outputs are
//! strictly ascending output ids; every other output fades.

use core::fmt::Write as _;
use std::collections::BTreeSet;

use fm_model::{AUDIO_GROUP_COUNT, AudioGroup, AudioGroups, InputGainMilliDb};
use fm_types::OutputId;

use super::{DecodeError, Object, Value, escape_string, parse_array, syntax};

/// Writes `,\n    "audio_groups": [...]` and the FTB output exclusions.
pub(crate) fn write_audio_mixer(output: &mut String, project: &fm_model::Project) {
    output.push_str(",\n    \"audio_groups\": [");
    for (index, (_, group)) in project.audio_groups().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        output.push_str("{\"name\": \"");
        escape_string(output, &group.name);
        write!(
            output,
            "\", \"gain_milli_db\": {}, \"muted\": {}}}",
            group.gain.get(),
            group.muted
        )
        .expect("writing to a string cannot fail");
    }
    output.push_str("\n    ],\n    \"fade_to_black_excluded_outputs\": [");
    for (index, id) in project.fade_to_black_excluded_outputs().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{id}").expect("writing to a string cannot fail");
    }
    output.push(']');
}

/// Parses `"audio_groups"` from the project object.
pub(crate) fn parse_audio_groups(object: &mut Object) -> Result<AudioGroups, DecodeError> {
    let groups = parse_array(object.take("audio_groups")?, "audio_groups", |value| {
        let mut object = Object::new(value, "audio group")?;
        let name = object.string("name")?;
        let gain = object.i32("gain_milli_db")?;
        let gain = InputGainMilliDb::new(gain).ok_or_else(|| {
            syntax(format!(
                "audio group gain_milli_db must be between {} and {}",
                InputGainMilliDb::MIN,
                InputGainMilliDb::MAX
            ))
        })?;
        let muted = object.boolean("muted")?;
        object.finish()?;
        Ok(AudioGroup { name, gain, muted })
    })?;
    let groups: [AudioGroup; AUDIO_GROUP_COUNT] = groups.try_into().map_err(|_| {
        syntax(format!(
            "audio_groups must hold exactly {AUDIO_GROUP_COUNT} groups"
        ))
    })?;
    Ok(AudioGroups::new(groups))
}

/// Parses `"fade_to_black_excluded_outputs"` from the project object.
pub(crate) fn parse_fade_to_black_excluded_outputs(
    object: &mut Object,
) -> Result<BTreeSet<OutputId>, DecodeError> {
    let ids = parse_array(
        object.take("fade_to_black_excluded_outputs")?,
        "fade_to_black_excluded_outputs",
        |value| {
            let Value::Number(value) = value else {
                return Err(syntax(
                    "fade_to_black_excluded_outputs entries must be output ids",
                ));
            };
            std::num::NonZeroU128::new(value)
                .map(OutputId::new)
                .ok_or_else(|| syntax("fade_to_black_excluded_outputs entries must be nonzero"))
        },
    )?;
    if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(syntax(
            "fade_to_black_excluded_outputs must be strictly ascending",
        ));
    }
    Ok(ids.into_iter().collect())
}
