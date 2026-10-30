//! Strip and Master audio plugin chains in `project.json` (schema 91,
//! AU-006).
//!
//! Every entry of `input_audio_strips` carries `"plugins"`, and the project
//! carries `"master_audio_plugins"`: `null` for an empty chain, otherwise
//! the ordered slots:
//!
//! ```text
//! [{"plugin": "freemix.reference.trim", "bypassed": false,
//!   "latency_frames": 0, "state": "AP8BAg"},
//!  {"plugin": "freemix.reference.gain", "bypassed": true,
//!   "latency_frames": 12, "state": ""}]
//! ```
//!
//! Every field is required, `plugin` is `[A-Za-z0-9._-]` and unique in the
//! chain, `latency_frames` is at most the plugin API bound, and `state` is
//! canonical unpadded base64url carrying at most 64 KiB. An empty chain is
//! written as `null` only, so the encoding is canonical.

use core::fmt::Write as _;

use fm_types::audio_plugins::{
    MAX_PLUGIN_CHAIN_SLOTS, PluginChainState, PluginSlotState, plugin_id_is_valid,
};

use super::{DecodeError, Object, escape_string, parse_array, parse_optional, syntax};

/// Writes `null` for an empty chain or the slot array.
pub(crate) fn write_plugin_chain(output: &mut String, chain: &PluginChainState) {
    if chain.is_empty() {
        output.push_str("null");
        return;
    }
    output.push('[');
    for (index, slot) in chain.slots().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str("{\"plugin\": \"");
        escape_string(output, slot.plugin());
        write!(
            output,
            "\", \"bypassed\": {}, \"latency_frames\": {}, \"state\": \"{}\"}}",
            slot.is_bypassed(),
            slot.latency_frames(),
            slot.state_code()
        )
        .expect("writing to a string cannot fail");
    }
    output.push(']');
}

/// Parses [`write_plugin_chain`] strictly.
pub(crate) fn parse_plugin_chain(
    value: super::Value,
    context: &'static str,
) -> Result<PluginChainState, DecodeError> {
    let Some(slots) = parse_optional(value, |value| {
        parse_array(value, context, |value| {
            let mut slot = Object::new(value, context)?;
            let plugin = slot.string("plugin")?;
            if !plugin_id_is_valid(&plugin) {
                return Err(syntax(format!("{context}: invalid plugin identifier")));
            }
            let mut state = PluginSlotState::new(plugin);
            state.set_bypassed(slot.boolean("bypassed")?);
            state
                .set_latency(slot.u32("latency_frames")?)
                .map_err(|error| syntax(format!("{context}: {error}")))?;
            state
                .set_state_code(&slot.string("state")?)
                .map_err(|error| syntax(format!("{context}: {error}")))?;
            slot.finish()?;
            Ok(state)
        })
    })?
    else {
        return Ok(PluginChainState::new());
    };
    if slots.is_empty() {
        return Err(syntax(format!("{context} writes an empty chain as null")));
    }
    if slots.len() > MAX_PLUGIN_CHAIN_SLOTS {
        return Err(syntax(format!(
            "{context} holds more than {MAX_PLUGIN_CHAIN_SLOTS} slots"
        )));
    }
    PluginChainState::from_slots(slots).map_err(|error| syntax(format!("{context}: {error}")))
}
