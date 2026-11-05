//! Secondary-mix codec (Protocol 2.158, IN-023).
//!
//! * A configuration is `number`, `name`, `mode`, `program`, and `preview`.
//! * `secondary_mixes` is a comma-separated list of
//!   `<number>~<name>~<mode>~<program>~<preview>`, strictly ascending by
//!   number; `preview` is `-` for an output-only mix.
//! * `secondary_mix_changed` is `number`, `configured` (`0|1`), then the
//!   configuration's other four fields when configured.

use crate::{
    CodecError, MAX_WIRE_MIX_NUMBER, MAX_WIRE_SECONDARY_MIXES, WireMixNumber,
    WireSecondaryMixConfig, WireSecondaryMixMode, WireSecondaryMixSource,
};

use super::media::{FieldSource, Push};
use super::value::{escape_bounded, parse_canonical_input, parse_canonical_u64, unescape};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_number(field: &'static str, value: &str) -> Result<WireMixNumber, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u8::try_from(number).ok())
        .and_then(WireMixNumber::new)
        .ok_or_else(|| invalid(field, value))
}

fn source_token(source: WireSecondaryMixSource) -> String {
    match source {
        WireSecondaryMixSource::Input(input) => format!("input:{input}"),
        WireSecondaryMixSource::Mix(number) => format!("mix:{number}"),
    }
}

fn parse_source(field: &'static str, value: &str) -> Result<WireSecondaryMixSource, CodecError> {
    if let Some(input) = value.strip_prefix("input:") {
        return parse_canonical_input(input)
            .map(WireSecondaryMixSource::Input)
            .ok_or_else(|| invalid(field, value));
    }
    if let Some(mix) = value.strip_prefix("mix:") {
        return parse_number(field, mix).map(WireSecondaryMixSource::Mix);
    }
    Err(invalid(field, value))
}

fn mode_token(mode: WireSecondaryMixMode) -> &'static str {
    match mode {
        WireSecondaryMixMode::PreviewProgram => "preview_program",
        WireSecondaryMixMode::OutputOnly => "output_only",
    }
}

fn parse_mode(field: &'static str, value: &str) -> Result<WireSecondaryMixMode, CodecError> {
    match value {
        "preview_program" => Ok(WireSecondaryMixMode::PreviewProgram),
        "output_only" => Ok(WireSecondaryMixMode::OutputOnly),
        _ => Err(invalid(field, value)),
    }
}

fn push_config_fields(
    config: &WireSecondaryMixConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if !config.is_consistent() {
        return Err(invalid("preview", config.number.to_string()));
    }
    push("name", escape_bounded(&config.name)?)?;
    push("mode", mode_token(config.mode))?;
    push(
        "program",
        escape_bounded(&source_token(config.program))?,
    )?;
    if let Some(preview) = config.preview {
        push("preview", escape_bounded(&source_token(preview))?)?;
    }
    Ok(())
}

fn parse_config_fields(
    fields: &mut dyn FieldSource,
    number: WireMixNumber,
) -> Result<WireSecondaryMixConfig, CodecError> {
    let name = unescape(&fields.required("name")?)?;
    let mode = parse_mode("mode", &fields.required("mode")?)?;
    let program = parse_source("program", &unescape(&fields.required("program")?)?)?;
    let preview = match fields.optional("preview") {
        Some(preview) => Some(parse_source("preview", &unescape(&preview)?)?),
        None => None,
    };
    Ok(WireSecondaryMixConfig {
        number,
        name,
        mode,
        program,
        preview,
    })
}

/// Encodes a configuration's fields.
pub(super) fn encode_config(
    config: &WireSecondaryMixConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("number", config.number.to_string())?;
    push_config_fields(config, push)
}

/// Decodes a configuration's fields.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<WireSecondaryMixConfig, CodecError> {
    let number = parse_number("number", &fields.required("number")?)?;
    parse_config_fields(fields, number)
}

/// Decodes a `number` field.
pub(super) fn decode_number(
    fields: &mut dyn FieldSource,
) -> Result<WireMixNumber, CodecError> {
    parse_number("number", &fields.required("number")?)
}

/// Encodes `secondary_mix_changed` after `event`.
pub(super) fn encode_changed(
    number: WireMixNumber,
    config: Option<&WireSecondaryMixConfig>,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("number", number.to_string())?;
    push("configured", u8::from(config.is_some()).to_string())?;
    match config {
        Some(config) => {
            if config.number != number {
                return Err(invalid("number", number.to_string()));
            }
            push_config_fields(config, push)
        }
        None => Ok(()),
    }
}

/// Decodes `secondary_mix_changed` after `event`.
pub(super) fn decode_changed(
    fields: &mut dyn FieldSource,
) -> Result<(WireMixNumber, Option<WireSecondaryMixConfig>), CodecError> {
    let number = parse_number("number", &fields.required("number")?)?;
    let configured = match fields.required("configured")?.as_str() {
        "0" => false,
        "1" => true,
        value => return Err(invalid("configured", value)),
    };
    if !configured {
        return Ok((number, None));
    }
    Ok((number, Some(parse_config_fields(fields, number)?)))
}

/// Encodes a snapshot's `secondary_mixes`.
pub(super) fn encode_entries(entries: &[WireSecondaryMixConfig]) -> Result<String, CodecError> {
    if entries.len() > MAX_WIRE_SECONDARY_MIXES {
        return Err(CodecError::TooManyItems("secondary_mixes"));
    }
    let mut previous = None;
    entries
        .iter()
        .map(|config| {
            if previous.is_some_and(|previous| previous >= config.number) {
                return Err(invalid("secondary_mixes", config.number.to_string()));
            }
            previous = Some(config.number);
            if !config.is_consistent() {
                return Err(invalid("secondary_mixes", config.number.to_string()));
            }
            let preview = config
                .preview
                .map_or_else(|| "-".to_owned(), source_token);
            Ok(format!(
                "{}~{}~{}~{}~{}",
                config.number,
                escape_bounded(&config.name)?,
                mode_token(config.mode),
                escape_bounded(&source_token(config.program))?,
                escape_bounded(&preview)?
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(","))
}

/// Decodes a snapshot's `secondary_mixes`.
pub(super) fn decode_entries(value: &str) -> Result<Vec<WireSecondaryMixConfig>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries = value.split(',').collect::<Vec<_>>();
    if entries.len() > MAX_WIRE_SECONDARY_MIXES {
        return Err(CodecError::TooManyItems("secondary_mixes"));
    }
    let mut previous = None;
    entries
        .into_iter()
        .map(|entry| {
            let [number, name, mode, program, preview] = entry.split('~').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("secondary_mixes", entry));
            };
            let number = parse_number("secondary_mixes", number)?;
            if previous.is_some_and(|previous| previous >= number) {
                return Err(invalid("secondary_mixes", entry));
            }
            previous = Some(number);
            if number.is_main() {
                return Err(invalid("secondary_mixes", entry));
            }
            let mode = parse_mode("secondary_mixes", mode)?;
            let preview = if preview == "-" {
                None
            } else {
                Some(parse_source("secondary_mixes", &unescape(preview)?)?)
            };
            let config = WireSecondaryMixConfig {
                number,
                name: unescape(name)?,
                mode,
                program: parse_source("secondary_mixes", &unescape(program)?)?,
                preview,
            };
            if !config.is_consistent() {
                return Err(invalid("secondary_mixes", entry));
            }
            Ok(config)
        })
        .collect()
}

const _: () = {
    let _ = MAX_WIRE_MIX_NUMBER;
};
