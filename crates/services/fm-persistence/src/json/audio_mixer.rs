//! Strip groups, the solo mode, the monitor output, and Fade-to-Black output
//! selection in `project.json` (schemas 39, 42, and 50, AU-001 and SW-004).
//!
//! ```text
//! "audio_groups": [
//!   {"name": "Group 1", "gain_milli_db": 0, "muted": false, "soloed": false, "parent": 2},
//!   {"name": "Hosts", "gain_milli_db": -6000, "muted": true, "soloed": false, "parent": null},
//!   {"name": "Group 3", "gain_milli_db": 0, "muted": false, "soloed": true, "parent": null},
//!   {"name": "Group 4", "gain_milli_db": 0, "muted": false, "soloed": false, "parent": null}
//! ],
//! "aux_buses": [
//!   {"name": "Bus A", "gain_milli_db": 0, "muted": false},
//!   ... seven buses, A first ...
//! ],
//! "output_aux_buses": [{"output": 4, "bus": "B"}],
//! "audio_solo_mode": "pfl",
//! "audio_monitor_output": {"device": "default", "gain_milli_db": -6000, "muted": false},
//! "fade_to_black_excluded_outputs": [5],
//! "fade_to_black_output_durations": [{"output": 3, "duration_frames": 90}],
//! "fade_to_black_curve": "ease"
//! ```
//!
//! Exactly [`AUDIO_GROUP_COUNT`] groups, group 1 first; `parent` is a group
//! number or `null`, and parents never form a cycle (checked by project
//! validation). Exactly [`AUX_BUS_COUNT`] `aux_buses` (schema 90, AU-002),
//! bus A first, each named like a group; `output_aux_buses` lists the
//! outputs that hear an aux bus (`A` through `G`) instead of Master,
//! strictly ascending by output. Every strip in `input_audio_strips` carries
//! `buses`: `M` when it feeds Master, then the letter of each aux bus it
//! feeds (`MAC`), or `-` for none. `audio_solo_mode` is `sip`, `pfl`, or
//! `afl`.
//! `audio_monitor_output` (schema 50) names the local audio output device the
//! monitor bus plays on (`null` for meter-only; an unusable name is reported
//! by project validation) and its own level and mute. Every strip in
//! `input_audio_strips` also carries `pan_basis_points`, `label` (empty for
//! none), and `group` (a group number or `null`). Excluded outputs are
//! strictly ascending output ids; every other output fades. Outputs in
//! `fade_to_black_output_durations` (schema 45) fade over their own
//! duration (1 through 3600 frames), strictly ascending by output;
//! `fade_to_black_curve` is `linear` or `ease`.

use core::fmt::Write as _;
use std::collections::BTreeSet;

use fm_model::{
    AUDIO_GROUP_COUNT, AUX_BUS_COUNT, AudioGroup, AudioGroupNumber, AudioGroups,
    AudioMonitorOutput, AudioSoloMode, AuxBus, AuxBusNumber, AuxBuses, InputGainMilliDb,
};
use fm_types::OutputId;

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

/// Writes `,\n    "audio_groups": [...]` and the FTB output exclusions.
pub(crate) fn write_audio_mixer(output: &mut String, project: &fm_model::Project) {
    output.push_str(",\n    \"audio_groups\": [");
    for (index, (_, group)) in project.audio_groups().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        output.push_str("{\"name\": \"");
        escape_string(output, &group.name);
        write!(
            output,
            "\", \"gain_milli_db\": {}, \"muted\": {}, \"soloed\": {}, \"parent\": ",
            group.gain.get(),
            group.muted,
            group.soloed
        )
        .expect("writing to a string cannot fail");
        match group.parent {
            Some(parent) => write!(output, "{}}}", parent.get()),
            None => write!(output, "null}}"),
        }
        .expect("writing to a string cannot fail");
    }
    output.push_str("\n    ],\n    \"aux_buses\": [");
    for (index, (_, bus)) in project.aux_buses().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        output.push_str("{\"name\": \"");
        escape_string(output, &bus.name);
        write!(
            output,
            "\", \"gain_milli_db\": {}, \"muted\": {}}}",
            bus.gain.get(),
            bus.muted
        )
        .expect("writing to a string cannot fail");
    }
    output.push_str("\n    ],\n    \"output_aux_buses\": [");
    for (index, (id, bus)) in project.output_aux_buses().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"output\": {id}, \"bus\": \"{}\"}}",
            bus.letter()
        )
        .expect("writing to a string cannot fail");
    }
    write!(
        output,
        "],\n    \"audio_solo_mode\": \"{}\"",
        project.audio_solo_mode().token()
    )
    .expect("writing to a string cannot fail");
    let monitor = project.audio_monitor_output();
    output.push_str(",\n    \"audio_monitor_output\": {\"device\": ");
    match &monitor.device {
        Some(device) => {
            output.push('"');
            escape_string(output, device);
            output.push('"');
        }
        None => output.push_str("null"),
    }
    write!(
        output,
        ", \"gain_milli_db\": {}, \"muted\": {}}}",
        monitor.gain.get(),
        monitor.muted
    )
    .expect("writing to a string cannot fail");
    output.push_str(",\n    \"master_audio_dsp\": ");
    super::strip_dsp::write_strip_dsp(output, project.master_audio_dsp());
    output.push_str(",\n    \"fade_to_black_excluded_outputs\": [");
    for (index, id) in project.fade_to_black_excluded_outputs().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{id}").expect("writing to a string cannot fail");
    }
    output.push_str("],\n    \"fade_to_black_output_durations\": [");
    for (index, (id, duration)) in project.fade_to_black_output_durations().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"output\": {id}, \"duration_frames\": {duration}}}"
        )
        .expect("writing to a string cannot fail");
    }
    write!(
        output,
        "],\n    \"fade_to_black_curve\": \"{}\"",
        project.fade_to_black_curve().token()
    )
    .expect("writing to a string cannot fail");
}

/// Parses `"fade_to_black_output_durations"` from the project object.
pub(crate) fn parse_fade_to_black_output_durations(
    object: &mut Object,
) -> Result<Vec<(OutputId, u32)>, DecodeError> {
    let entries = parse_array(
        object.take("fade_to_black_output_durations")?,
        "fade_to_black_output_durations",
        |value| {
            let mut entry = Object::new(value, "fade-to-black output duration")?;
            let output = OutputId::new(entry.nonzero_u128("output")?);
            let duration = entry.u32("duration_frames")?;
            entry.finish()?;
            Ok((output, duration))
        },
    )?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "fade_to_black_output_durations must be strictly ascending by output",
        ));
    }
    Ok(entries)
}

/// Parses `"fade_to_black_curve"` from the project object.
pub(crate) fn parse_fade_to_black_curve(
    object: &mut Object,
) -> Result<fm_types::FadeToBlackCurve, DecodeError> {
    let token = object.string("fade_to_black_curve")?;
    fm_types::FadeToBlackCurve::from_token(&token)
        .ok_or_else(|| syntax("fade_to_black_curve must be \"linear\" or \"ease\""))
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
        let soloed = object.boolean("soloed")?;
        let parent = parse_optional(object.take("parent")?, |value| {
            let Value::Number(number) = value else {
                return Err(syntax("audio group parent must be a number or null"));
            };
            u8::try_from(number)
                .ok()
                .and_then(AudioGroupNumber::new)
                .ok_or_else(|| {
                    syntax(format!(
                        "audio group parent must be 1 through {AUDIO_GROUP_COUNT}"
                    ))
                })
        })?;
        object.finish()?;
        Ok(AudioGroup {
            name,
            gain,
            muted,
            soloed,
            parent,
        })
    })?;
    let groups: [AudioGroup; AUDIO_GROUP_COUNT] = groups.try_into().map_err(|_| {
        syntax(format!(
            "audio_groups must hold exactly {AUDIO_GROUP_COUNT} groups"
        ))
    })?;
    Ok(AudioGroups::new(groups))
}

/// Parses `"aux_buses"` from the project object.
pub(crate) fn parse_aux_buses(object: &mut Object) -> Result<AuxBuses, DecodeError> {
    let buses = parse_array(object.take("aux_buses")?, "aux_buses", |value| {
        let mut object = Object::new(value, "aux bus")?;
        let name = object.string("name")?;
        let gain = InputGainMilliDb::new(object.i32("gain_milli_db")?).ok_or_else(|| {
            syntax(format!(
                "aux bus gain_milli_db must be between {} and {}",
                InputGainMilliDb::MIN,
                InputGainMilliDb::MAX
            ))
        })?;
        let muted = object.boolean("muted")?;
        object.finish()?;
        Ok(AuxBus { name, gain, muted })
    })?;
    let buses: [AuxBus; AUX_BUS_COUNT] = buses
        .try_into()
        .map_err(|_| syntax(format!("aux_buses must hold exactly {AUX_BUS_COUNT} buses")))?;
    Ok(AuxBuses::new(buses))
}

/// Parses `"output_aux_buses"` from the project object.
pub(crate) fn parse_output_aux_buses(
    object: &mut Object,
) -> Result<std::collections::BTreeMap<OutputId, AuxBusNumber>, DecodeError> {
    let entries = parse_array(
        object.take("output_aux_buses")?,
        "output_aux_buses",
        |value| {
            let mut entry = Object::new(value, "output aux bus")?;
            let output = OutputId::new(entry.nonzero_u128("output")?);
            let letter = entry.string("bus")?;
            let mut letters = letter.chars();
            let bus = match (letters.next(), letters.next()) {
                (Some(letter), None) => AuxBusNumber::from_letter(letter),
                _ => None,
            }
            .ok_or_else(|| syntax("output aux bus must be a letter A through G"))?;
            entry.finish()?;
            Ok((output, bus))
        },
    )?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "output_aux_buses must be strictly ascending by output",
        ));
    }
    Ok(entries.into_iter().collect())
}

/// Parses `"audio_solo_mode"` from the project object.
pub(crate) fn parse_audio_solo_mode(object: &mut Object) -> Result<AudioSoloMode, DecodeError> {
    let token = object.string("audio_solo_mode")?;
    AudioSoloMode::from_token(&token)
        .ok_or_else(|| syntax("audio_solo_mode must be \"sip\", \"pfl\", or \"afl\""))
}

/// Parses `"audio_monitor_output"` from the project object.
pub(crate) fn parse_audio_monitor_output(
    object: &mut Object,
) -> Result<AudioMonitorOutput, DecodeError> {
    let mut monitor = Object::new(object.take("audio_monitor_output")?, "audio monitor output")?;
    let device = parse_optional(monitor.take("device")?, |value| {
        let Value::String(device) = value else {
            return Err(syntax("audio monitor device must be a string or null"));
        };
        Ok(device)
    })?;
    let gain = InputGainMilliDb::new(monitor.i32("gain_milli_db")?).ok_or_else(|| {
        syntax(format!(
            "audio monitor gain_milli_db must be between {} and {}",
            InputGainMilliDb::MIN,
            InputGainMilliDb::MAX
        ))
    })?;
    let muted = monitor.boolean("muted")?;
    monitor.finish()?;
    Ok(AudioMonitorOutput {
        device,
        gain,
        muted,
    })
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
