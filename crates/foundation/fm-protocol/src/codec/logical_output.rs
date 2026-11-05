//! Logical-output codec (Protocol 2.157, SW-012).
//!
//! * A configuration is `slot`, `output`, `source`, `overlays`, and `audio`.
//! * `logical_outputs` is a comma-separated list of
//!   `<slot>~<output>~<source>~<overlays>~<audio>`, strictly ascending by
//!   slot.
//! * `logical_output_changed` is `slot`, `configured` (`0|1`), then the
//!   configuration's other four fields when configured.

use core::num::NonZeroU128;

use crate::{
    CodecError, MAX_WIRE_LOGICAL_MIX, MAX_WIRE_LOGICAL_MULTIVIEW_CELLS, MAX_WIRE_LOGICAL_OUTPUTS,
    WireLogicalAudioMap, WireLogicalOutputCell, WireLogicalOutputConfig, WireLogicalOutputOverlays,
    WireLogicalOutputSlot, WireLogicalOutputSource,
};
use fm_types::BusId;

use super::media::{FieldSource, Push};
use super::value::{
    escape_bounded, parse_canonical_input, parse_canonical_output, parse_canonical_u64, unescape,
};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_slot(field: &'static str, value: &str) -> Result<WireLogicalOutputSlot, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u8::try_from(number).ok())
        .and_then(WireLogicalOutputSlot::new)
        .ok_or_else(|| invalid(field, value))
}

fn parse_mix(field: &'static str, value: &str) -> Result<u8, CodecError> {
    let mix = parse_canonical_u64(value)
        .and_then(|number| u8::try_from(number).ok())
        .filter(|mix| (1..=MAX_WIRE_LOGICAL_MIX).contains(mix))
        .ok_or_else(|| invalid(field, value))?;
    Ok(mix)
}

fn parse_bus(field: &'static str, value: &str) -> Result<BusId, CodecError> {
    let number = value
        .parse::<u128>()
        .ok()
        .filter(|number| *number > 0 && (number.to_string() == value))
        .and_then(NonZeroU128::new)
        .ok_or_else(|| invalid(field, value))?;
    Ok(BusId::new(number))
}

fn source_token(source: &WireLogicalOutputSource) -> String {
    match source {
        WireLogicalOutputSource::Program => "program".to_owned(),
        WireLogicalOutputSource::Preview => "preview".to_owned(),
        WireLogicalOutputSource::Input(input) => format!("input:{input}"),
        WireLogicalOutputSource::Mix(mix) => format!("mix:{mix}"),
        WireLogicalOutputSource::MultiView(cells) => format!(
            "multiview:{}",
            cells
                .iter()
                .map(|cell| cell_token(*cell))
                .collect::<Vec<_>>()
                .join("/")
        ),
    }
}

fn cell_token(cell: WireLogicalOutputCell) -> String {
    match cell {
        WireLogicalOutputCell::Program => "program".to_owned(),
        WireLogicalOutputCell::Preview => "preview".to_owned(),
        WireLogicalOutputCell::Input(input) => format!("input:{input}"),
        WireLogicalOutputCell::Mix(mix) => format!("mix:{mix}"),
        WireLogicalOutputCell::Output(slot) => format!("output:{slot}"),
    }
}

fn parse_source(field: &'static str, value: &str) -> Result<WireLogicalOutputSource, CodecError> {
    if value == "program" {
        return Ok(WireLogicalOutputSource::Program);
    }
    if value == "preview" {
        return Ok(WireLogicalOutputSource::Preview);
    }
    if let Some(input) = value.strip_prefix("input:") {
        return parse_canonical_input(input)
            .map(WireLogicalOutputSource::Input)
            .ok_or_else(|| invalid(field, value));
    }
    if let Some(mix) = value.strip_prefix("mix:") {
        return parse_mix(field, mix).map(WireLogicalOutputSource::Mix);
    }
    if let Some(cells) = value.strip_prefix("multiview:") {
        if cells.is_empty() {
            return Err(invalid(field, value));
        }
        let cells = cells
            .split('/')
            .map(|cell| parse_cell(field, cell))
            .collect::<Result<Vec<_>, _>>()?;
        if cells.len() > MAX_WIRE_LOGICAL_MULTIVIEW_CELLS {
            return Err(CodecError::TooManyItems("multiview"));
        }
        return Ok(WireLogicalOutputSource::MultiView(cells));
    }
    Err(invalid(field, value))
}

fn parse_cell(field: &'static str, value: &str) -> Result<WireLogicalOutputCell, CodecError> {
    if value == "program" {
        return Ok(WireLogicalOutputCell::Program);
    }
    if value == "preview" {
        return Ok(WireLogicalOutputCell::Preview);
    }
    if let Some(input) = value.strip_prefix("input:") {
        return parse_canonical_input(input)
            .map(WireLogicalOutputCell::Input)
            .ok_or_else(|| invalid(field, value));
    }
    if let Some(mix) = value.strip_prefix("mix:") {
        return parse_mix(field, mix).map(WireLogicalOutputCell::Mix);
    }
    if let Some(slot) = value.strip_prefix("output:") {
        return parse_slot(field, slot).map(WireLogicalOutputCell::Output);
    }
    Err(invalid(field, value))
}

fn overlays_token(overlays: &WireLogicalOutputOverlays) -> String {
    match overlays {
        WireLogicalOutputOverlays::All => "all".to_owned(),
        WireLogicalOutputOverlays::Clean => "clean".to_owned(),
        WireLogicalOutputOverlays::Routed => "routed".to_owned(),
        WireLogicalOutputOverlays::Channels(channels) => format!(
            "channels:{}",
            channels
                .iter()
                .map(|channel| channel.number().to_string())
                .collect::<Vec<_>>()
                .join("/")
        ),
    }
}

fn parse_overlays(
    field: &'static str,
    value: &str,
) -> Result<WireLogicalOutputOverlays, CodecError> {
    if value == "all" {
        return Ok(WireLogicalOutputOverlays::All);
    }
    if value == "clean" {
        return Ok(WireLogicalOutputOverlays::Clean);
    }
    if value == "routed" {
        return Ok(WireLogicalOutputOverlays::Routed);
    }
    let Some(channels) = value.strip_prefix("channels:") else {
        return Err(invalid(field, value));
    };
    if channels.is_empty() {
        return Ok(WireLogicalOutputOverlays::Channels(Vec::new()));
    }
    let mut parsed = Vec::new();
    for channel in channels.split('/') {
        let number = parse_canonical_u64(channel)
            .and_then(|number| u8::try_from(number).ok())
            .and_then(crate::WireOverlayChannelId::new)
            .ok_or_else(|| invalid(field, value))?;
        if parsed
            .last()
            .is_some_and(|previous: &crate::WireOverlayChannelId| {
                previous.number() >= number.number()
            })
        {
            return Err(invalid(field, value));
        }
        parsed.push(number);
    }
    Ok(WireLogicalOutputOverlays::Channels(parsed))
}

fn audio_token(audio: WireLogicalAudioMap) -> String {
    match audio {
        WireLogicalAudioMap::Master => "master".to_owned(),
        WireLogicalAudioMap::FollowVideo => "follow_video".to_owned(),
        WireLogicalAudioMap::Bus(bus) => format!("bus:{bus}"),
    }
}

fn parse_audio(field: &'static str, value: &str) -> Result<WireLogicalAudioMap, CodecError> {
    match value {
        "master" => Ok(WireLogicalAudioMap::Master),
        "follow_video" => Ok(WireLogicalAudioMap::FollowVideo),
        value => value
            .strip_prefix("bus:")
            .ok_or_else(|| invalid(field, value))
            .and_then(|bus| parse_bus(field, bus))
            .map(WireLogicalAudioMap::Bus),
    }
}

fn push_config_fields(
    config: &WireLogicalOutputConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("output", config.output.to_string())?;
    push("source", escape_bounded(&source_token(&config.source))?)?;
    push(
        "overlays",
        escape_bounded(&overlays_token(&config.overlays))?,
    )?;
    push("audio", escape_bounded(&audio_token(config.audio))?)
}

fn push_config(config: &WireLogicalOutputConfig, push: &mut Push<'_>) -> Result<(), CodecError> {
    push("slot", config.slot.to_string())?;
    push_config_fields(config, push)
}

fn parse_config_fields(
    fields: &mut dyn FieldSource,
    slot: WireLogicalOutputSlot,
) -> Result<WireLogicalOutputConfig, CodecError> {
    let output = fields.required("output")?;
    let output = parse_canonical_output(&output).ok_or_else(|| invalid("output", output))?;
    let source = fields.required("source")?;
    let overlays = fields.required("overlays")?;
    let audio = fields.required("audio")?;
    Ok(WireLogicalOutputConfig {
        slot,
        output,
        source: parse_source("source", &unescape(&source)?)?,
        overlays: parse_overlays("overlays", &unescape(&overlays)?)?,
        audio: parse_audio("audio", &unescape(&audio)?)?,
    })
}

fn parse_config(fields: &mut dyn FieldSource) -> Result<WireLogicalOutputConfig, CodecError> {
    let slot = parse_slot("slot", &fields.required("slot")?)?;
    parse_config_fields(fields, slot)
}

/// Encodes a configuration's five fields.
pub(super) fn encode_config(
    config: &WireLogicalOutputConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push_config(config, push)
}

/// Decodes a configuration's five fields.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<WireLogicalOutputConfig, CodecError> {
    parse_config(fields)
}

/// Decodes a `slot` field.
pub(super) fn decode_slot(
    fields: &mut dyn FieldSource,
) -> Result<WireLogicalOutputSlot, CodecError> {
    parse_slot("slot", &fields.required("slot")?)
}

/// Encodes `logical_output_changed` after `event`.
pub(super) fn encode_changed(
    slot: WireLogicalOutputSlot,
    config: Option<&WireLogicalOutputConfig>,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("slot", slot.to_string())?;
    push("configured", u8::from(config.is_some()).to_string())?;
    match config {
        Some(config) => {
            if config.slot != slot {
                return Err(invalid("slot", slot.to_string()));
            }
            push_config_fields(config, push)
        }
        None => Ok(()),
    }
}

/// Decodes `logical_output_changed` after `event`.
pub(super) fn decode_changed(
    fields: &mut dyn FieldSource,
) -> Result<(WireLogicalOutputSlot, Option<WireLogicalOutputConfig>), CodecError> {
    let slot = parse_slot("slot", &fields.required("slot")?)?;
    let configured = match fields.required("configured")?.as_str() {
        "0" => false,
        "1" => true,
        value => return Err(invalid("configured", value)),
    };
    if !configured {
        return Ok((slot, None));
    }
    Ok((slot, Some(parse_config_fields(fields, slot)?)))
}

/// Encodes a snapshot's `logical_outputs`.
pub(super) fn encode_entries(entries: &[WireLogicalOutputConfig]) -> Result<String, CodecError> {
    if entries.len() > MAX_WIRE_LOGICAL_OUTPUTS {
        return Err(CodecError::TooManyItems("logical_outputs"));
    }
    let mut previous = None;
    entries
        .iter()
        .map(|config| {
            if previous.is_some_and(|previous| previous >= config.slot) {
                return Err(invalid("logical_outputs", config.slot.to_string()));
            }
            previous = Some(config.slot);
            Ok(format!(
                "{}~{}~{}~{}~{}",
                config.slot,
                config.output,
                escape_bounded(&source_token(&config.source))?,
                escape_bounded(&overlays_token(&config.overlays))?,
                escape_bounded(&audio_token(config.audio))?
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(","))
}

/// Decodes a snapshot's `logical_outputs`.
pub(super) fn decode_entries(value: &str) -> Result<Vec<WireLogicalOutputConfig>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries = value.split(',').collect::<Vec<_>>();
    if entries.len() > MAX_WIRE_LOGICAL_OUTPUTS {
        return Err(CodecError::TooManyItems("logical_outputs"));
    }
    let mut previous = None;
    entries
        .into_iter()
        .map(|entry| {
            let [slot, output, source, overlays, audio] = entry.split('~').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("logical_outputs", entry));
            };
            let slot = parse_slot("logical_outputs", slot)?;
            if previous.is_some_and(|previous| previous >= slot) {
                return Err(invalid("logical_outputs", entry));
            }
            previous = Some(slot);
            Ok(WireLogicalOutputConfig {
                slot,
                output: parse_canonical_output(output)
                    .ok_or_else(|| invalid("logical_outputs", output))?,
                source: parse_source("logical_outputs", &unescape(source)?)?,
                overlays: parse_overlays("logical_outputs", &unescape(overlays)?)?,
                audio: parse_audio("logical_outputs", &unescape(audio)?)?,
            })
        })
        .collect()
}
