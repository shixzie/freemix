//! The project's user shortcuts in `project.json` (RC-004, schema 63; a
//! key's `then` and `scope` since schema 70; mouse buttons and pop-out
//! window scopes since schema 75).
//!
//! ```text
//! "shortcuts": [
//!   {"id": 1, "name": "Intro", "enabled": true,
//!    "key": {"key": "F9", "command": false, "shift": true, "alt": false,
//!            "then": null, "scope": null},
//!    "osc": "intro",
//!    "action": {"type": "actions", "actions": [
//!      {"type": "overlay_on", "channel": 1, "input": 4},
//!      {"type": "record", "recorder": 1, "start": true}]}},
//!   {"id": 2, "name": "Open", "enabled": true, "key": null, "osc": null,
//!    "action": {"type": "cue", "list": 1, "operation": "go", "cue": null}},
//!   {"id": 3, "name": "Scores", "enabled": false, "key": null, "osc": "scores",
//!    "action": {"type": "script", "script": 2}}
//! ]
//! ```
//!
//! A chord's `then` is its second stroke, `{"key", "command", "shift",
//! "alt"}`; `scope` is `null`, the console panel the key answers in, or that
//! panel's pop-out window (`"mixer-window"`). A `key` may name a mouse
//! button (`MouseMiddle`, `MouseBack`, `MouseForward`), a single stroke
//! whose `then` is `null`.
//!
//! An `actions` action holds cue actions in the cue list form (see
//! [`super::cue`]). A `cue` action's `operation` is `go` (with `cue`, a cue
//! id or `null` for the standby cue), `stop`, `back`, `skip`, or `reset`
//! (with no `cue`). Bounds, duplicate ids, keys, and OSC names are reported
//! by project validation.

use core::fmt::Write as _;

use fm_model::{Shortcut, ShortcutAction, ShortcutCueOperation, ShortcutKey};

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

/// Writes `,\n    "shortcuts": [...]`.
pub(crate) fn write_shortcuts(output: &mut String, shortcuts: &[Shortcut]) {
    output.push_str(",\n    \"shortcuts\": [");
    for (index, shortcut) in shortcuts.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write_shortcut(output, shortcut);
    }
    if !shortcuts.is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

fn write_shortcut(output: &mut String, shortcut: &Shortcut) {
    write!(output, "{{\"id\": {}, \"name\": \"", shortcut.id)
        .expect("writing to a string cannot fail");
    escape_string(output, &shortcut.name);
    write!(output, "\", \"enabled\": {}, \"key\": ", shortcut.enabled)
        .expect("writing to a string cannot fail");
    match &shortcut.key {
        Some(key) => {
            write_stroke(output, key);
            output.push_str(", \"then\": ");
            match &key.then {
                Some(then) => {
                    write_stroke(output, then);
                    output.push('}');
                }
                None => output.push_str("null"),
            }
            output.push_str(", \"scope\": ");
            match &key.scope {
                Some(scope) => {
                    output.push('"');
                    escape_string(output, scope);
                    output.push('"');
                }
                None => output.push_str("null"),
            }
            output.push('}');
        }
        None => output.push_str("null"),
    }
    output.push_str(", \"osc\": ");
    match &shortcut.osc {
        Some(name) => {
            output.push('"');
            escape_string(output, name);
            output.push('"');
        }
        None => output.push_str("null"),
    }
    output.push_str(", \"action\": ");
    match &shortcut.action {
        ShortcutAction::Actions(actions) => {
            output.push_str("{\"type\": \"actions\", \"actions\": [");
            for (index, action) in actions.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                super::cue::write_action(output, action);
            }
            output.push_str("]}");
        }
        ShortcutAction::Cue { list, operation } => {
            write!(
                output,
                "{{\"type\": \"cue\", \"list\": {list}, \"operation\": "
            )
            .expect("writing to a string cannot fail");
            match operation {
                ShortcutCueOperation::Go { cue } => {
                    let cue = cue.map_or_else(|| "null".to_owned(), |cue| cue.to_string());
                    write!(output, "\"go\", \"cue\": {cue}}}")
                }
                ShortcutCueOperation::Stop => write!(output, "\"stop\"}}"),
                ShortcutCueOperation::Back => write!(output, "\"back\"}}"),
                ShortcutCueOperation::Skip => write!(output, "\"skip\"}}"),
                ShortcutCueOperation::Reset => write!(output, "\"reset\"}}"),
            }
            .expect("writing to a string cannot fail");
        }
        ShortcutAction::Script { script } => {
            write!(output, "{{\"type\": \"script\", \"script\": {script}}}")
                .expect("writing to a string cannot fail");
        }
    }
    output.push('}');
}

/// Parses `"shortcuts"` from the project object.
pub(crate) fn parse_shortcuts(object: &mut Object) -> Result<Vec<Shortcut>, DecodeError> {
    parse_array(object.take("shortcuts")?, "shortcuts", parse_shortcut)
}

fn parse_shortcut(value: Value) -> Result<Shortcut, DecodeError> {
    let mut object = Object::new(value, "shortcut")?;
    let id = object.u64("id")?;
    let name = object.string("name")?;
    let enabled = object.boolean("enabled")?;
    let key = parse_optional(object.take("key")?, parse_key)?;
    let osc = parse_optional(object.take("osc")?, |value| match value {
        Value::String(name) => Ok(name),
        _ => Err(syntax("shortcut osc must be a string or null")),
    })?;
    let action = parse_action(object.take("action")?)?;
    object.finish()?;
    Ok(Shortcut {
        id,
        name,
        enabled,
        key,
        osc,
        action,
    })
}

/// Writes `{"key": .., "command": .., "shift": .., "alt": ..` (open).
fn write_stroke(output: &mut String, key: &ShortcutKey) {
    output.push_str("{\"key\": \"");
    escape_string(output, &key.key);
    write!(
        output,
        "\", \"command\": {}, \"shift\": {}, \"alt\": {}",
        key.command, key.shift, key.alt
    )
    .expect("writing to a string cannot fail");
}

fn parse_stroke(object: &mut Object) -> Result<ShortcutKey, DecodeError> {
    Ok(ShortcutKey {
        key: object.string("key")?,
        command: object.boolean("command")?,
        shift: object.boolean("shift")?,
        alt: object.boolean("alt")?,
        then: None,
        scope: None,
    })
}

fn parse_key(value: Value) -> Result<ShortcutKey, DecodeError> {
    let mut object = Object::new(value, "shortcut key")?;
    let mut key = parse_stroke(&mut object)?;
    key.then = parse_optional(object.take("then")?, |value| {
        let mut object = Object::new(value, "shortcut key stroke")?;
        let stroke = parse_stroke(&mut object)?;
        object.finish()?;
        Ok(Box::new(stroke))
    })?;
    key.scope = parse_optional(object.take("scope")?, |value| match value {
        Value::String(scope) => Ok(scope),
        _ => Err(syntax("shortcut key scope must be a string or null")),
    })?;
    object.finish()?;
    Ok(key)
}

fn parse_action(value: Value) -> Result<ShortcutAction, DecodeError> {
    let mut object = Object::new(value, "shortcut action")?;
    let action = match object.string("type")?.as_str() {
        "actions" => ShortcutAction::Actions(parse_array(
            object.take("actions")?,
            "shortcut actions",
            super::cue::parse_action,
        )?),
        "cue" => {
            let list = object.u64("list")?;
            let operation = match object.string("operation")?.as_str() {
                "go" => ShortcutCueOperation::Go {
                    cue: parse_optional(object.take("cue")?, |value| match value {
                        Value::Number(cue) => u64::try_from(cue)
                            .map_err(|_| syntax("shortcut cue id must fit 64 bits")),
                        _ => Err(syntax("shortcut cue must be a cue id or null")),
                    })?,
                },
                "stop" => ShortcutCueOperation::Stop,
                "back" => ShortcutCueOperation::Back,
                "skip" => ShortcutCueOperation::Skip,
                "reset" => ShortcutCueOperation::Reset,
                value => return Err(super::unknown_enum("shortcut cue operation", value)),
            };
            ShortcutAction::Cue { list, operation }
        }
        "script" => ShortcutAction::Script {
            script: object.u64("script")?,
        },
        value => return Err(super::unknown_enum("shortcut action", value)),
    };
    object.finish()?;
    Ok(action)
}
