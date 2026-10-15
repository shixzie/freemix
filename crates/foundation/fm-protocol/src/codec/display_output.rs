//! Display-output codec (Protocol 2.42, OR-011).
//!
//! * A configuration is the fields `display` (`id:<id>`,
//!   `id:<id>|name:<name>`, `name:<name>`, or `index:<n>`), `source`
//!   (`program`, `preview`, or `output:<id>`), `mode` (`current` or
//!   `<w>x<h>@<millihertz>`), `scaling` (`fit`, `fill`, `stretch`, `1:1`),
//!   and `hdr` (`off`, `auto`).
//! * `display_outputs` is a comma-separated list of
//!   `<slot>~<display>~<source>~<mode>~<scaling>~<hdr>`, strictly ascending
//!   by slot.
//! * `display_output_states` is a comma-separated list of
//!   `<slot>~waiting~<reason>` or
//!   `<slot>~active~<id>~<name>~<mode>~<fallback 0|1>~<hdr state>`,
//!   strictly ascending by slot.
//! * `displays` is a comma-separated list of
//!   `<id>~<name>~<x>~<y>~<current mode>~<modes ;-separated>~<hdr>`.
//!
//! Text inside a list entry is percent-escaped before it is joined.

use crate::display_output::invalid;
use crate::{
    CodecError, DisplayId, DisplayInventory, DisplayInventoryEntry, DisplayMode,
    DisplayOutputConfig, DisplayOutputEntry, DisplayOutputSlot, DisplayOutputSource,
    DisplayOutputStatus, DisplaySelector, HdrCapability, HdrRequest, HdrState,
    MAX_DISPLAY_INVENTORY_ERROR_BYTES, MAX_DISPLAY_OUTPUTS, MAX_INVENTORY_DISPLAY_MODES,
    MAX_INVENTORY_DISPLAYS, ModeRequest, WaitReason, WireActiveDisplay, WireDisplayOutputState,
};
use fm_types::OutputScaling;

use super::media::{FieldSource, Push};
use super::value::{escape_bounded, parse_canonical_u64, unescape};

fn parse_slot(field: &'static str, value: &str) -> Result<DisplayOutputSlot, CodecError> {
    parse_canonical_u64(value)
        .and_then(|slot| u8::try_from(slot).ok())
        .and_then(DisplayOutputSlot::new)
        .ok_or_else(|| invalid(field, value))
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

/// A selector whose own fields are usable: the text bounds hold and the
/// index is in range.
fn check_selector(field: &'static str, selector: &DisplaySelector) -> Result<(), CodecError> {
    let canonical = DisplaySelector::from_parts(
        selector.id().map(DisplayId::as_str),
        selector.name(),
        selector.index(),
    );
    if canonical.as_ref() == Ok(selector) {
        Ok(())
    } else {
        Err(invalid(field, selector.to_string()))
    }
}

fn parse_selector(field: &'static str, value: &str) -> Result<DisplaySelector, CodecError> {
    let selector = value
        .parse::<DisplaySelector>()
        .map_err(|_| invalid(field, value))?;
    // Only the explicit forms are canonical on the wire (no bare index).
    if selector.to_string() != value {
        return Err(invalid(field, value));
    }
    check_selector(field, &selector)?;
    Ok(selector)
}

fn encode_mode(field: &'static str, mode: DisplayMode) -> Result<String, CodecError> {
    if mode.is_valid() {
        Ok(mode.to_string())
    } else {
        Err(invalid(field, mode.to_string()))
    }
}

/// Parses a canonical `<w>x<h>@<millihertz>` mode.
fn parse_mode(field: &'static str, value: &str) -> Result<DisplayMode, CodecError> {
    let mode = value
        .parse::<DisplayMode>()
        .map_err(|_| invalid(field, value))?;
    if mode.is_valid() && mode.to_string() == value {
        Ok(mode)
    } else {
        Err(invalid(field, value))
    }
}

fn encode_mode_request(field: &'static str, mode: ModeRequest) -> Result<String, CodecError> {
    match mode {
        ModeRequest::Current => Ok("current".to_owned()),
        ModeRequest::Exact(mode) => encode_mode(field, mode),
    }
}

fn parse_mode_request(field: &'static str, value: &str) -> Result<ModeRequest, CodecError> {
    if value == "current" {
        Ok(ModeRequest::Current)
    } else {
        parse_mode(field, value).map(ModeRequest::Exact)
    }
}

fn parse_source(field: &'static str, value: &str) -> Result<DisplayOutputSource, CodecError> {
    let source = value
        .parse::<DisplayOutputSource>()
        .map_err(|_| invalid(field, value))?;
    if source.to_string() == value {
        Ok(source)
    } else {
        Err(invalid(field, value))
    }
}

fn parse_scaling(field: &'static str, value: &str) -> Result<OutputScaling, CodecError> {
    value
        .parse::<OutputScaling>()
        .map_err(|_| invalid(field, value))
}

fn parse_hdr_request(field: &'static str, value: &str) -> Result<HdrRequest, CodecError> {
    match value {
        "off" => Ok(HdrRequest::Off),
        "auto" => Ok(HdrRequest::Auto),
        _ => Err(invalid(field, value)),
    }
}

/// Encodes a configuration as its five fields.
pub(super) fn encode_config(
    config: &DisplayOutputConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_selector("display", &config.display)?;
    push("display", config.display.to_string())?;
    push("source", config.source.to_string())?;
    push("mode", encode_mode_request("mode", config.mode)?)?;
    push("scaling", config.scaling.to_string())?;
    push("hdr", config.hdr.to_string())
}

/// Decodes a configuration's five fields.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<DisplayOutputConfig, CodecError> {
    Ok(DisplayOutputConfig {
        display: parse_selector("display", &fields.required("display")?)?,
        source: parse_source("source", &fields.required("source")?)?,
        mode: parse_mode_request("mode", &fields.required("mode")?)?,
        scaling: parse_scaling("scaling", &fields.required("scaling")?)?,
        hdr: parse_hdr_request("hdr", &fields.required("hdr")?)?,
    })
}

/// Decodes a `slot` field.
pub(super) fn decode_slot(fields: &mut dyn FieldSource) -> Result<DisplayOutputSlot, CodecError> {
    parse_slot("slot", &fields.required("slot")?)
}

/// Encodes `display_output_changed`'s fields after `event`.
pub(super) fn encode_changed(
    slot: DisplayOutputSlot,
    config: Option<&DisplayOutputConfig>,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("slot", slot.to_string())?;
    push("configured", u8::from(config.is_some()).to_string())?;
    match config {
        Some(config) => encode_config(config, push),
        None => Ok(()),
    }
}

/// Decodes `display_output_changed`'s fields.
pub(super) fn decode_changed(
    fields: &mut dyn FieldSource,
) -> Result<(DisplayOutputSlot, Option<DisplayOutputConfig>), CodecError> {
    let slot = decode_slot(fields)?;
    let configured = parse_flag("configured", &fields.required("configured")?)?;
    let config = if configured {
        Some(decode_config(fields)?)
    } else {
        None
    };
    Ok((slot, config))
}

fn check_slots<'a>(
    field: &'static str,
    slots: impl Iterator<Item = &'a DisplayOutputSlot>,
) -> Result<(), CodecError> {
    let mut previous = None;
    for slot in slots {
        if previous.is_some_and(|previous| previous >= *slot) {
            return Err(invalid(field, slot.to_string()));
        }
        previous = Some(*slot);
    }
    Ok(())
}

/// Encodes a snapshot's `display_outputs`.
pub(super) fn encode_entries(entries: &[DisplayOutputEntry]) -> Result<String, CodecError> {
    if entries.len() > MAX_DISPLAY_OUTPUTS {
        return Err(CodecError::TooManyItems("display_outputs"));
    }
    check_slots("display_outputs", entries.iter().map(|entry| &entry.slot))?;
    entries
        .iter()
        .map(|entry| {
            let config = &entry.config;
            check_selector("display_outputs", &config.display)?;
            Ok(format!(
                "{}~{}~{}~{}~{}~{}",
                entry.slot,
                escape_bounded(&config.display.to_string())?,
                escape_bounded(&config.source.to_string())?,
                encode_mode_request("display_outputs", config.mode)?,
                escape_bounded(config.scaling.as_str())?,
                config.hdr
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(","))
}

/// Decodes a snapshot's `display_outputs`.
pub(super) fn decode_entries(value: &str) -> Result<Vec<DisplayOutputEntry>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_DISPLAY_OUTPUTS + 1).count() > MAX_DISPLAY_OUTPUTS {
        return Err(CodecError::TooManyItems("display_outputs"));
    }
    let entries = value
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('~').collect();
            let [slot, display, source, mode, scaling, hdr] = parts.as_slice() else {
                return Err(invalid("display_outputs", entry));
            };
            Ok(DisplayOutputEntry {
                slot: parse_slot("display_outputs", slot)?,
                config: DisplayOutputConfig {
                    display: parse_selector("display_outputs", &unescape(display)?)?,
                    source: parse_source("display_outputs", &unescape(source)?)?,
                    mode: parse_mode_request("display_outputs", mode)?,
                    scaling: parse_scaling("display_outputs", &unescape(scaling)?)?,
                    hdr: parse_hdr_request("display_outputs", hdr)?,
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_slots("display_outputs", entries.iter().map(|entry| &entry.slot))?;
    Ok(entries)
}

fn check_display_text(field: &'static str, text: &str) -> Result<(), CodecError> {
    fm_types::display::validate_display_text(text).map_err(|_| invalid(field, text))
}

fn encode_state(
    field: &'static str,
    state: &WireDisplayOutputState,
    separator: char,
) -> Result<String, CodecError> {
    Ok(match state {
        WireDisplayOutputState::Waiting(reason) => format!("waiting{separator}{}", reason.as_str()),
        WireDisplayOutputState::Active(active) => {
            check_display_text(field, active.id.as_str())?;
            check_display_text(field, &active.name)?;
            format!(
                "active{separator}{}{separator}{}{separator}{}{separator}{}{separator}{}",
                escape_bounded(active.id.as_str())?,
                escape_bounded(&active.name)?,
                encode_mode(field, active.mode)?,
                u8::from(active.mode_fallback),
                active.hdr.as_str()
            )
        }
    })
}

fn decode_state(field: &'static str, parts: &[&str]) -> Result<WireDisplayOutputState, CodecError> {
    let describe = || parts.join("~");
    match parts {
        ["waiting", reason] => reason
            .parse::<WaitReason>()
            .map(WireDisplayOutputState::Waiting)
            .map_err(|_| invalid(field, describe())),
        ["active", id, name, mode, fallback, hdr] => {
            let id = unescape(id)?;
            let name = unescape(name)?;
            check_display_text(field, &name)?;
            Ok(WireDisplayOutputState::Active(WireActiveDisplay {
                id: DisplayId::new(id).map_err(|_| invalid(field, describe()))?,
                name,
                mode: parse_mode(field, mode)?,
                mode_fallback: parse_flag(field, fallback)?,
                hdr: hdr
                    .parse::<HdrState>()
                    .map_err(|_| invalid(field, describe()))?,
            }))
        }
        _ => Err(invalid(field, describe())),
    }
}

/// Encodes a snapshot's `display_output_states`.
pub(super) fn encode_states(states: &[DisplayOutputStatus]) -> Result<String, CodecError> {
    if states.len() > MAX_DISPLAY_OUTPUTS {
        return Err(CodecError::TooManyItems("display_output_states"));
    }
    check_slots(
        "display_output_states",
        states.iter().map(|status| &status.slot),
    )?;
    states
        .iter()
        .map(|status| {
            Ok(format!(
                "{}~{}",
                status.slot,
                encode_state("display_output_states", &status.state, '~')?
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|states| states.join(","))
}

/// Decodes a snapshot's `display_output_states`.
pub(super) fn decode_states(value: &str) -> Result<Vec<DisplayOutputStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_DISPLAY_OUTPUTS + 1).count() > MAX_DISPLAY_OUTPUTS {
        return Err(CodecError::TooManyItems("display_output_states"));
    }
    let states = value
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('~').collect();
            let Some((slot, state)) = parts.split_first() else {
                return Err(invalid("display_output_states", entry));
            };
            Ok(DisplayOutputStatus {
                slot: parse_slot("display_output_states", slot)?,
                state: decode_state("display_output_states", state)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_slots(
        "display_output_states",
        states.iter().map(|status| &status.slot),
    )?;
    Ok(states)
}

/// Encodes a `display_output_status` runtime event's fields after `event`.
pub(super) fn encode_status(
    status: &DisplayOutputStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("slot", status.slot.to_string())?;
    match &status.state {
        WireDisplayOutputState::Waiting(reason) => {
            push("state", "waiting".to_owned())?;
            push("reason", reason.as_str().to_owned())
        }
        WireDisplayOutputState::Active(active) => {
            check_display_text("display_id", active.id.as_str())?;
            check_display_text("display_name", &active.name)?;
            push("state", "active".to_owned())?;
            push("display_id", active.id.as_str().to_owned())?;
            push("display_name", active.name.clone())?;
            push("mode", encode_mode("mode", active.mode)?)?;
            push("mode_fallback", u8::from(active.mode_fallback).to_string())?;
            push("hdr", active.hdr.as_str().to_owned())
        }
    }
}

/// Decodes a `display_output_status` runtime event's fields.
pub(super) fn decode_status(
    fields: &mut dyn FieldSource,
) -> Result<DisplayOutputStatus, CodecError> {
    let slot = decode_slot(fields)?;
    let state = fields.required("state")?;
    let state = match state.as_str() {
        "waiting" => {
            let reason = fields.required("reason")?;
            WireDisplayOutputState::Waiting(
                reason
                    .parse::<WaitReason>()
                    .map_err(|_| invalid("reason", reason))?,
            )
        }
        "active" => {
            let id = fields.required("display_id")?;
            let name = fields.required("display_name")?;
            check_display_text("display_name", &name)?;
            let hdr = fields.required("hdr")?;
            WireDisplayOutputState::Active(WireActiveDisplay {
                id: DisplayId::new(id.clone()).map_err(|_| invalid("display_id", id))?,
                name,
                mode: parse_mode("mode", &fields.required("mode")?)?,
                mode_fallback: parse_flag("mode_fallback", &fields.required("mode_fallback")?)?,
                hdr: hdr.parse::<HdrState>().map_err(|_| invalid("hdr", hdr))?,
            })
        }
        _ => return Err(invalid("state", state)),
    };
    Ok(DisplayOutputStatus { slot, state })
}

fn encode_display(display: &DisplayInventoryEntry) -> Result<String, CodecError> {
    check_display_text("displays", display.id.as_str())?;
    check_display_text("displays", &display.name)?;
    if display.modes.len() > MAX_INVENTORY_DISPLAY_MODES {
        return Err(CodecError::TooManyItems("displays"));
    }
    let modes = display
        .modes
        .iter()
        .map(|mode| encode_mode("displays", *mode))
        .collect::<Result<Vec<_>, _>>()?
        .join(";");
    Ok(format!(
        "{}~{}~{}~{}~{}~{}~{}",
        escape_bounded(display.id.as_str())?,
        escape_bounded(&display.name)?,
        display.x,
        display.y,
        encode_mode("displays", display.current_mode)?,
        modes,
        display.hdr.as_str()
    ))
}

fn parse_coordinate(value: &str) -> Result<i32, CodecError> {
    value
        .parse::<i32>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .ok_or_else(|| invalid("displays", value))
}

fn decode_display(entry: &str) -> Result<DisplayInventoryEntry, CodecError> {
    let parts: Vec<&str> = entry.split('~').collect();
    let [id, name, x, y, current, modes, hdr] = parts.as_slice() else {
        return Err(invalid("displays", entry));
    };
    let id = unescape(id)?;
    let name = unescape(name)?;
    check_display_text("displays", &name)?;
    let modes = if modes.is_empty() {
        Vec::new()
    } else {
        if modes
            .split(';')
            .take(MAX_INVENTORY_DISPLAY_MODES + 1)
            .count()
            > MAX_INVENTORY_DISPLAY_MODES
        {
            return Err(CodecError::TooManyItems("displays"));
        }
        modes
            .split(';')
            .map(|mode| parse_mode("displays", mode))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(DisplayInventoryEntry {
        id: DisplayId::new(id).map_err(|_| invalid("displays", entry))?,
        name,
        x: parse_coordinate(x)?,
        y: parse_coordinate(y)?,
        current_mode: parse_mode("displays", current)?,
        modes,
        hdr: hdr
            .parse::<HdrCapability>()
            .map_err(|_| invalid("displays", entry))?,
    })
}

fn check_error(error: &str) -> Result<(), CodecError> {
    if error.is_empty()
        || error.len() > MAX_DISPLAY_INVENTORY_ERROR_BYTES
        || error.chars().any(char::is_control)
    {
        Err(invalid("error", error))
    } else {
        Ok(())
    }
}

/// Encodes an inventory as `presenter`, optional `error`, and `displays`.
pub(super) fn encode_inventory(
    inventory: &DisplayInventory,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if inventory.displays.len() > MAX_INVENTORY_DISPLAYS {
        return Err(CodecError::TooManyItems("displays"));
    }
    push("presenter", u8::from(inventory.presenter).to_string())?;
    if let Some(error) = &inventory.error {
        check_error(error)?;
        push("error", error.clone())?;
    }
    let displays = inventory
        .displays
        .iter()
        .map(encode_display)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    push("displays", displays)
}

/// Decodes an inventory's `presenter`, optional `error`, and `displays`.
pub(super) fn decode_inventory(
    fields: &mut dyn FieldSource,
) -> Result<DisplayInventory, CodecError> {
    let presenter = parse_flag("presenter", &fields.required("presenter")?)?;
    let error = fields.optional("error");
    if let Some(error) = &error {
        check_error(error)?;
    }
    let displays = fields.required("displays")?;
    let displays = if displays.is_empty() {
        Vec::new()
    } else {
        if displays.split(',').take(MAX_INVENTORY_DISPLAYS + 1).count() > MAX_INVENTORY_DISPLAYS {
            return Err(CodecError::TooManyItems("displays"));
        }
        displays
            .split(',')
            .map(decode_display)
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(DisplayInventory {
        presenter,
        error,
        displays,
    })
}
