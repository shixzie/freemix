//! Secondary mixes in `project.json` (IN-023, schema 108).
//!
//! ```text
//! "secondary_mixes": [
//!   {"number": 2, "name": "Clean", "mode": "preview_program",
//!    "program": {"input": 3}, "preview": {"input": 4}},
//!   {"number": 3, "name": "Feed", "mode": "output_only",
//!    "program": {"mix": 2}}
//! ]
//! ```
//!
//! Entries are strictly ascending by number, numbered 2 through 16. `program`
//! and `preview` are `{"input": <id>}` or `{"mix": <1..=16>}`. `preview` is
//! present exactly when `mode` is `preview_program`; an `output_only` mix has
//! no `preview`. Project validation reports missing inputs, unknown mixes,
//! equal Program and Preview, and cycles.

use core::{fmt::Write as _, num::NonZeroU128};

use fm_model::{
    MAX_MIX_NUMBER, MAX_SECONDARY_MIXES, MixNumber, SecondaryMixConfig, SecondaryMixMode,
    SecondaryMixSource,
};
use fm_types::InputId;

use super::{DecodeError, Object, Value, parse_array, syntax};

/// Writes `,\n    "secondary_mixes": [...]` when any mix is configured.
pub(crate) fn write_secondary_mixes(output: &mut String, project: &fm_model::Project) {
    if project.secondary_mixes().is_empty() {
        return;
    }
    output.push_str(",\n    \"secondary_mixes\": [");
    for (index, config) in project.secondary_mixes().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(
            output,
            "{{\"number\": {}, \"name\": ",
            config.number.number()
        )
        .expect("writing to a string cannot fail");
        write_name(output, &config.name);
        match config.mode {
            SecondaryMixMode::PreviewProgram => output.push_str(", \"mode\": \"preview_program\""),
            SecondaryMixMode::OutputOnly => output.push_str(", \"mode\": \"output_only\""),
        }
        output.push_str(", \"program\": ");
        write_source(output, config.program);
        if let Some(preview) = config.preview {
            output.push_str(", \"preview\": ");
            write_source(output, preview);
        }
        output.push('}');
    }
    output.push_str("\n    ]");
}

fn write_name(output: &mut String, name: &str) {
    output.push('"');
    for character in name.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a string cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

fn write_source(output: &mut String, source: SecondaryMixSource) {
    match source {
        SecondaryMixSource::Input(input) => {
            write!(output, "{{\"input\": {input}}}").expect("writing to a string cannot fail");
        }
        SecondaryMixSource::Mix(number) => {
            write!(output, "{{\"mix\": {}}}", number.number())
                .expect("writing to a string cannot fail");
        }
    }
}

/// Parses `secondary_mixes`, defaulting to none when the key is absent.
pub(crate) fn parse_secondary_mixes(
    object: &mut Object,
) -> Result<Vec<SecondaryMixConfig>, DecodeError> {
    let Some(value) = object.take_optional("secondary_mixes") else {
        return Ok(Vec::new());
    };
    let entries = parse_array(value, "secondary_mixes", parse_entry)?;
    if entries.len() > MAX_SECONDARY_MIXES {
        return Err(syntax(format!(
            "secondary_mixes must hold at most {MAX_SECONDARY_MIXES} entries"
        )));
    }
    if entries
        .windows(2)
        .any(|pair| pair[0].number >= pair[1].number)
    {
        return Err(syntax(
            "secondary_mixes must be strictly ascending by number",
        ));
    }
    Ok(entries)
}

fn parse_entry(value: Value) -> Result<SecondaryMixConfig, DecodeError> {
    let mut object = Object::new(value, "secondary mix")?;
    let number = MixNumber::new(object.u8("number")?).ok_or_else(|| {
        syntax(format!(
            "secondary mix number must be 1 through {MAX_MIX_NUMBER}"
        ))
    })?;
    if number.is_main() {
        return Err(syntax("secondary_mixes cannot configure Main"));
    }
    let name = object.string("name")?;
    let mode = match object.string("mode")?.as_str() {
        "preview_program" => SecondaryMixMode::PreviewProgram,
        "output_only" => SecondaryMixMode::OutputOnly,
        other => return Err(super::unknown_enum("secondary mix mode", other)),
    };
    let program = parse_source(object.take("program")?)?;
    let preview = object
        .take_optional("preview")
        .map(parse_source)
        .transpose()?;
    object.finish()?;
    Ok(SecondaryMixConfig {
        number,
        name,
        mode,
        program,
        preview,
    })
}

fn parse_source(value: Value) -> Result<SecondaryMixSource, DecodeError> {
    let mut object = Object::new(value, "secondary mix source")?;
    let source = if let Some(input) = object.take_optional("input") {
        SecondaryMixSource::Input(InputId::new(nonzero(&input, "secondary mix input")?))
    } else if let Some(mix) = object.take_optional("mix") {
        let number = u8_value(&mix, "secondary mix source mix")?;
        SecondaryMixSource::Mix(MixNumber::new(number).ok_or_else(|| {
            syntax(format!(
                "secondary mix source mix must be 1 through {MAX_MIX_NUMBER}"
            ))
        })?)
    } else {
        return Err(syntax("secondary mix source needs input or mix"));
    };
    object.finish()?;
    Ok(source)
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
