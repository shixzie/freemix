//! Logical output routing in `project.json` (SW-012, schema 107).
//!
//! ```text
//! "logical_outputs": [
//!   {"slot": 1, "output": 7, "source": "program",
//!    "overlays": "all", "audio": "master"},
//!   {"slot": 2, "output": 8, "source": {"input": 3},
//!    "overlays": {"channels": [1, 3]}, "audio": {"bus": 7}},
//!   {"slot": 3, "output": 9,
//!    "source": {"multiview": ["program", {"input": 3}, {"mix": 2}, {"output": 1}]},
//!    "overlays": "routed", "audio": "follow_video"}
//! ]
//! ```
//!
//! Entries are strictly ascending by slot. `source` is `program`, `preview`,
//! `{"input": <id>}`, `{"mix": <1..=16>}`, or `{"multiview": [cells]}`;
//! a cell is `program`, `preview`, `{"input": <id>}`, `{"mix": <1..=16>}`, or
//! `{"output": <slot>}`. `overlays` is `all`, `clean`, `routed`, or
//! `{"channels": [1..=8]}`. `audio` is `master`, `follow_video`, or
//! `{"bus": <id>}`. Every reference, duplicate, and multiview cycle is
//! reported by project validation.

use core::{fmt::Write as _, num::NonZeroU128};

use fm_model::{
    LogicalAudioMap, LogicalMultiViewCell, LogicalOutputConfig, LogicalOutputSlot,
    LogicalOutputSource, LogicalOverlayChannels, LogicalOverlayPolicy, MAX_LOGICAL_OUTPUTS,
};
use fm_types::{BusId, InputId, OutputId};

use super::{DecodeError, Object, Value, parse_array, syntax};

/// Writes `,\n    "logical_outputs": [...]` when any slot is configured.
pub(crate) fn write_logical_outputs(output: &mut String, project: &fm_model::Project) {
    if project.logical_outputs().is_empty() {
        return;
    }
    output.push_str(",\n    \"logical_outputs\": [");
    for (index, config) in project.logical_outputs().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(
            output,
            "{{\"slot\": {}, \"output\": {}, \"source\": ",
            config.slot.number(),
            config.output
        )
        .expect("writing to a string cannot fail");
        write_source(output, &config.source);
        output.push_str(", \"overlays\": ");
        match config.overlays {
            LogicalOverlayPolicy::All => output.push_str("\"all\""),
            LogicalOverlayPolicy::Clean => output.push_str("\"clean\""),
            LogicalOverlayPolicy::Routed => output.push_str("\"routed\""),
            LogicalOverlayPolicy::Channels(channels) => {
                output.push_str("{\"channels\": [");
                for (channel_index, channel) in channels.iter().enumerate() {
                    if channel_index != 0 {
                        output.push_str(", ");
                    }
                    write!(output, "{channel}").expect("writing to a string cannot fail");
                }
                output.push_str("]}");
            }
        }
        output.push_str(", \"audio\": ");
        match config.audio {
            LogicalAudioMap::Master => output.push_str("\"master\""),
            LogicalAudioMap::FollowVideo => output.push_str("\"follow_video\""),
            LogicalAudioMap::Bus(bus) => {
                write!(output, "{{\"bus\": {bus}}}").expect("writing to a string cannot fail");
            }
        }
        output.push('}');
    }
    output.push_str("\n    ]");
}

fn write_source(output: &mut String, source: &LogicalOutputSource) {
    match source {
        LogicalOutputSource::Program => output.push_str("\"program\""),
        LogicalOutputSource::Preview => output.push_str("\"preview\""),
        LogicalOutputSource::Input(input) => {
            write!(output, "{{\"input\": {input}}}").expect("writing to a string cannot fail");
        }
        LogicalOutputSource::Mix(mix) => {
            write!(output, "{{\"mix\": {mix}}}").expect("writing to a string cannot fail");
        }
        LogicalOutputSource::MultiView(cells) => {
            output.push_str("{\"multiview\": [");
            for (index, cell) in cells.iter().enumerate() {
                if index != 0 {
                    output.push_str(", ");
                }
                write_cell(output, *cell);
            }
            output.push_str("]}");
        }
    }
}

fn write_cell(output: &mut String, cell: LogicalMultiViewCell) {
    match cell {
        LogicalMultiViewCell::Program => output.push_str("\"program\""),
        LogicalMultiViewCell::Preview => output.push_str("\"preview\""),
        LogicalMultiViewCell::Input(input) => {
            write!(output, "{{\"input\": {input}}}").expect("writing to a string cannot fail");
        }
        LogicalMultiViewCell::Mix(mix) => {
            write!(output, "{{\"mix\": {mix}}}").expect("writing to a string cannot fail");
        }
        LogicalMultiViewCell::Output(slot) => {
            write!(output, "{{\"output\": {}}}", slot.number())
                .expect("writing to a string cannot fail");
        }
    }
}

/// Parses `"logical_outputs"`; an absent section is empty.
pub(crate) fn parse_logical_outputs(
    object: &mut Object,
) -> Result<Vec<LogicalOutputConfig>, DecodeError> {
    let Some(value) = object.take_optional("logical_outputs") else {
        return Ok(Vec::new());
    };
    let entries = parse_array(value, "logical_outputs", parse_entry)?;
    if entries.len() > MAX_LOGICAL_OUTPUTS {
        return Err(syntax(format!(
            "logical_outputs must hold at most {MAX_LOGICAL_OUTPUTS} entries"
        )));
    }
    if entries.windows(2).any(|pair| pair[0].slot >= pair[1].slot) {
        return Err(syntax("logical_outputs must be strictly ascending by slot"));
    }
    Ok(entries)
}

fn parse_entry(value: Value) -> Result<LogicalOutputConfig, DecodeError> {
    let mut object = Object::new(value, "logical output")?;
    let slot = LogicalOutputSlot::new(object.u8("slot")?).ok_or_else(|| {
        syntax(format!(
            "logical output slot must be 1 through {MAX_LOGICAL_OUTPUTS}"
        ))
    })?;
    let output = OutputId::new(object.nonzero_u128("output")?);
    let source = parse_source(object.take("source")?)?;
    let overlays = parse_overlays(object.take("overlays")?)?;
    let audio = parse_audio(object.take("audio")?)?;
    object.finish()?;
    Ok(LogicalOutputConfig {
        slot,
        output,
        source,
        overlays,
        audio,
    })
}

fn parse_source(value: Value) -> Result<LogicalOutputSource, DecodeError> {
    match value {
        Value::String(source) => match source.as_str() {
            "program" => Ok(LogicalOutputSource::Program),
            "preview" => Ok(LogicalOutputSource::Preview),
            other => Err(super::unknown_enum("logical output source", other)),
        },
        value @ Value::Object(_) => {
            let mut object = Object::new(value, "logical output source")?;
            let source = if let Some(input) = object.take_optional("input") {
                LogicalOutputSource::Input(InputId::new(nonzero(&input, "logical output input")?))
            } else if let Some(mix) = object.take_optional("mix") {
                LogicalOutputSource::Mix(mix_number(mix)?)
            } else if let Some(cells) = object.take_optional("multiview") {
                LogicalOutputSource::MultiView(parse_array(cells, "multiview", parse_cell)?)
            } else {
                return Err(syntax(
                    "logical output source needs input, mix, or multiview",
                ));
            };
            object.finish()?;
            Ok(source)
        }
        _ => Err(syntax(
            "logical output source must be a token or one of input, mix, multiview",
        )),
    }
}

fn parse_cell(value: Value) -> Result<LogicalMultiViewCell, DecodeError> {
    match value {
        Value::String(cell) => match cell.as_str() {
            "program" => Ok(LogicalMultiViewCell::Program),
            "preview" => Ok(LogicalMultiViewCell::Preview),
            other => Err(super::unknown_enum("multiview cell", other)),
        },
        value @ Value::Object(_) => {
            let mut object = Object::new(value, "multiview cell")?;
            let cell = if let Some(input) = object.take_optional("input") {
                LogicalMultiViewCell::Input(InputId::new(nonzero(&input, "multiview input")?))
            } else if let Some(mix) = object.take_optional("mix") {
                LogicalMultiViewCell::Mix(mix_number(mix)?)
            } else if let Some(slot) = object.take_optional("output") {
                let slot = u8_value(&slot, "multiview output")?;
                LogicalMultiViewCell::Output(LogicalOutputSlot::new(slot).ok_or_else(|| {
                    syntax(format!(
                        "multiview output must be 1 through {MAX_LOGICAL_OUTPUTS}"
                    ))
                })?)
            } else {
                return Err(syntax("multiview cell needs input, mix, or output"));
            };
            object.finish()?;
            Ok(cell)
        }
        _ => Err(syntax(
            "multiview cell must be a token or one of input, mix, output",
        )),
    }
}

fn parse_overlays(value: Value) -> Result<LogicalOverlayPolicy, DecodeError> {
    match value {
        Value::String(overlays) => match overlays.as_str() {
            "all" => Ok(LogicalOverlayPolicy::All),
            "clean" => Ok(LogicalOverlayPolicy::Clean),
            "routed" => Ok(LogicalOverlayPolicy::Routed),
            other => Err(super::unknown_enum("logical output overlays", other)),
        },
        value @ Value::Object(_) => {
            let mut object = Object::new(value, "logical output overlays")?;
            let channels = parse_array(object.take("channels")?, "overlay channels", |value| {
                u8_value(&value, "overlay channel")
            })?;
            object.finish()?;
            LogicalOverlayChannels::from_channels(channels)
                .map(LogicalOverlayPolicy::Channels)
                .map_err(|error| syntax(error.to_string()))
        }
        _ => Err(syntax(
            "logical output overlays must be a token or a channels object",
        )),
    }
}

fn parse_audio(value: Value) -> Result<LogicalAudioMap, DecodeError> {
    match value {
        Value::String(audio) => match audio.as_str() {
            "master" => Ok(LogicalAudioMap::Master),
            "follow_video" => Ok(LogicalAudioMap::FollowVideo),
            other => Err(super::unknown_enum("logical output audio", other)),
        },
        value @ Value::Object(_) => {
            let mut object = Object::new(value, "logical output audio")?;
            let bus = BusId::new(object.nonzero_u128("bus")?);
            object.finish()?;
            Ok(LogicalAudioMap::Bus(bus))
        }
        _ => Err(syntax(
            "logical output audio must be a token or a bus object",
        )),
    }
}

fn mix_number(value: Value) -> Result<u8, DecodeError> {
    let mix = u8_value(&value, "mix")?;
    if !(1..=fm_model::MAX_LOGICAL_MIX_NUMBER).contains(&mix) {
        return Err(syntax(format!(
            "mix must be 1 through {}",
            fm_model::MAX_LOGICAL_MIX_NUMBER
        )));
    }
    Ok(mix)
}

fn u8_value(value: &Value, context: &str) -> Result<u8, DecodeError> {
    match value {
        Value::Number(number) => {
            u8::try_from(*number).map_err(|_| syntax(format!("{context} must fit in a byte")))
        }
        _ => Err(syntax(format!("{context} must be a number"))),
    }
}

fn nonzero(value: &Value, context: &str) -> Result<NonZeroU128, DecodeError> {
    let Value::Number(number) = value else {
        return Err(syntax(format!("{context} must be a number")));
    };
    NonZeroU128::new(*number).ok_or_else(|| syntax(format!("{context} must be nonzero")))
}
