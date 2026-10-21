//! Shortcut codec (Protocol 2.72, RC-004).
//!
//! * `shortcut` is a nonzero canonical id; `name` the shortcut name
//!   (validated by [`crate::shortcut_name_is_valid`]); `enabled` `0` or `1`.
//! * `key` is `-` for none or `<stroke>[+<stroke>][@<scope>]` (the second
//!   stroke and the scope since 2.83), each stroke
//!   `<key>.<command 0|1>.<shift 0|1>.<alt 0|1>` with `key` 1 to 16 ASCII
//!   letters and digits, and `scope` one of [`crate::SHORTCUT_SCOPES`],
//!   a panel's pop-out window (2.93), or a tool window (2.106, see
//!   [`crate::shortcut_scope_is_valid`]).
//! * `osc` is `-` for none or the OSC name (1 to 32 lowercase ASCII
//!   letters, digits, `-`, and `_`).
//! * `action` is `actions:<tokens>` (1 to 16 `/`-separated cue action
//!   tokens, see [`super::cue`], without `wait` or `delay`, at most one
//!   taking Program, at most 512 bytes of title text),
//!   `cue:<list>:go:<cue|->`, `cue:<list>:<stop|back|skip|reset>`, or
//!   `script:<script>`, every id nonzero canonical decimal.
//! * `shortcut_fire` carries an optional `value` (Protocol 2.99, RC-004),
//!   canonical decimal `0..=1000`: the firing control's position, per mille.
//! * `shortcut_fired` carries `shortcut`, `frame`, `actions`, and the
//!   desired state fields of `cue_run_changed`.

use crate::{
    CodecError, MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_TEXT_BYTES, MAX_SHORTCUTS, ShortcutSpec,
    ShortcutStatus, WireCueAction, WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
    shortcut_key_is_valid, shortcut_name_is_valid, shortcut_osc_is_valid,
};

use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Refuses shortcut id zero.
pub(super) fn validate_shortcut_id(shortcut: u64) -> Result<(), CodecError> {
    if shortcut == 0 {
        Err(invalid("shortcut", "0"))
    } else {
        Ok(())
    }
}

/// Decodes the optional `value` of a `shortcut_fire` (Protocol 2.99).
pub(super) fn decode_fire_value(
    fields: &mut dyn FieldSource,
) -> Result<Option<fm_types::FireValue>, CodecError> {
    fields
        .optional("value")
        .map(|value| {
            parse_canonical_u64(&value)
                .and_then(|parsed| u16::try_from(parsed).ok())
                .and_then(fm_types::FireValue::new)
                .ok_or_else(|| invalid("value", value))
        })
        .transpose()
}

/// Decodes a nonzero `shortcut` field.
pub(super) fn decode_shortcut_id(fields: &mut dyn FieldSource) -> Result<u64, CodecError> {
    let value = fields.required("shortcut")?;
    parse_canonical_u64(&value)
        .filter(|shortcut| *shortcut != 0)
        .ok_or_else(|| invalid("shortcut", value))
}

fn nonzero(value: &str) -> Option<u64> {
    parse_canonical_u64(value).filter(|id| *id != 0)
}

/// Checks a shortcut action's structural bounds.
fn validate_action(action: &WireShortcutAction) -> Result<(), CodecError> {
    match action {
        WireShortcutAction::Actions(actions) => {
            if actions.is_empty() || actions.len() > MAX_SHORTCUT_ACTIONS {
                return Err(CodecError::TooManyItems("action"));
            }
            let mut takes = 0_usize;
            let mut text = 0_usize;
            for action in actions {
                if matches!(
                    action,
                    WireCueAction::Wait { .. } | WireCueAction::Delay { .. }
                ) || !super::cue::action_is_valid(action)
                {
                    return Err(invalid("action", format!("{action:?}")));
                }
                if matches!(
                    action,
                    WireCueAction::Take { .. } | WireCueAction::PlaylistStart
                ) {
                    takes += 1;
                }
                if let WireCueAction::TitleText { text: value, .. } = action {
                    text += value.len();
                }
            }
            if takes > 1 {
                return Err(invalid("action", "takes Program more than once"));
            }
            if text > MAX_SHORTCUT_TEXT_BYTES {
                return Err(CodecError::TooManyItems("action"));
            }
            Ok(())
        }
        WireShortcutAction::Cue { list, operation } => {
            if *list == 0 || matches!(operation, WireShortcutCueOperation::Go { cue: Some(0) }) {
                return Err(invalid("action", "0"));
            }
            Ok(())
        }
        WireShortcutAction::Script { script } => {
            if *script == 0 {
                return Err(invalid("action", "0"));
            }
            Ok(())
        }
    }
}

/// Checks a shortcut definition's structural bounds.
///
/// # Errors
///
/// Refuses an invalid name, key, OSC name, or action.
pub fn validate_shortcut_spec(spec: &ShortcutSpec) -> Result<(), CodecError> {
    if !shortcut_name_is_valid(&spec.name) {
        return Err(invalid("name", spec.name.clone()));
    }
    if let Some(key) = &spec.key
        && !shortcut_key_is_valid(key)
    {
        return Err(invalid("key", key.key.clone()));
    }
    if let Some(osc) = &spec.osc
        && !shortcut_osc_is_valid(osc)
    {
        return Err(invalid("osc", osc.clone()));
    }
    validate_action(&spec.action)
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn parse_flag(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn encode_stroke(key: &WireShortcutKey) -> String {
    format!(
        "{}.{}.{}.{}",
        key.key,
        flag(key.command),
        flag(key.shift),
        flag(key.alt)
    )
}

fn encode_key(key: Option<&WireShortcutKey>) -> String {
    key.map_or_else(
        || "-".to_owned(),
        |key| {
            let mut text = encode_stroke(key);
            if let Some(then) = &key.then {
                text.push('+');
                text.push_str(&encode_stroke(then));
            }
            if let Some(scope) = &key.scope {
                text.push('@');
                text.push_str(scope);
            }
            text
        },
    )
}

fn decode_stroke(stroke: &str) -> Option<WireShortcutKey> {
    let parts: Vec<&str> = stroke.split('.').collect();
    let [key, command, shift, alt] = parts.as_slice() else {
        return None;
    };
    Some(WireShortcutKey::stroke(
        *key,
        parse_flag(command)?,
        parse_flag(shift)?,
        parse_flag(alt)?,
    ))
}

fn decode_key(value: &str) -> Result<Option<WireShortcutKey>, CodecError> {
    if value == "-" {
        return Ok(None);
    }
    let bad = || invalid("key", value);
    let (strokes, scope) = match value.split_once('@') {
        Some((strokes, scope)) => (strokes, Some(scope.to_owned())),
        None => (value, None),
    };
    let (first, then) = match strokes.split_once('+') {
        Some((first, then)) => (first, Some(Box::new(decode_stroke(then).ok_or_else(bad)?))),
        None => (strokes, None),
    };
    let mut key = decode_stroke(first).ok_or_else(bad)?;
    key.then = then;
    key.scope = scope;
    if !shortcut_key_is_valid(&key) {
        return Err(bad());
    }
    Ok(Some(key))
}

/// A shortcut action in the `action` field's notation (`actions:<tokens>`,
/// `cue:<list>:go:<cue|->`, `cue:<list>:<stop|back|skip|reset>`, or
/// `script:<script>`), which shortcut set files reuse.
#[must_use]
pub fn shortcut_action_text(action: &WireShortcutAction) -> String {
    encode_action(action)
}

/// Parses a shortcut action written by [`shortcut_action_text`], checking
/// its structural bounds.
///
/// # Errors
///
/// Refuses unknown notation and an action out of bounds.
pub fn parse_shortcut_action_text(text: &str) -> Result<WireShortcutAction, CodecError> {
    let action = decode_action(text)?;
    validate_action(&action)?;
    Ok(action)
}

fn encode_action(action: &WireShortcutAction) -> String {
    match action {
        WireShortcutAction::Actions(actions) => format!(
            "actions:{}",
            actions
                .iter()
                .map(super::cue::cue_action_token)
                .collect::<Vec<_>>()
                .join("/")
        ),
        WireShortcutAction::Cue { list, operation } => match operation {
            WireShortcutCueOperation::Go { cue } => format!(
                "cue:{list}:go:{}",
                cue.map_or_else(|| "-".to_owned(), |cue| cue.to_string())
            ),
            WireShortcutCueOperation::Stop => format!("cue:{list}:stop"),
            WireShortcutCueOperation::Back => format!("cue:{list}:back"),
            WireShortcutCueOperation::Skip => format!("cue:{list}:skip"),
            WireShortcutCueOperation::Reset => format!("cue:{list}:reset"),
        },
        WireShortcutAction::Script { script } => format!("script:{script}"),
    }
}

fn decode_action(value: &str) -> Result<WireShortcutAction, CodecError> {
    let bad = || invalid("action", value);
    if let Some(tokens) = value.strip_prefix("actions:") {
        let actions = tokens
            .split('/')
            .map(|token| super::cue::parse_cue_action_token(token).ok_or_else(bad))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(WireShortcutAction::Actions(actions));
    }
    if let Some(script) = value.strip_prefix("script:") {
        return Ok(WireShortcutAction::Script {
            script: nonzero(script).ok_or_else(bad)?,
        });
    }
    let Some(cue) = value.strip_prefix("cue:") else {
        return Err(bad());
    };
    let parts: Vec<&str> = cue.split(':').collect();
    let (list, operation) = match parts.as_slice() {
        [list, "go", "-"] => (list, WireShortcutCueOperation::Go { cue: None }),
        [list, "go", cue] => (
            list,
            WireShortcutCueOperation::Go {
                cue: Some(nonzero(cue).ok_or_else(bad)?),
            },
        ),
        [list, "stop"] => (list, WireShortcutCueOperation::Stop),
        [list, "back"] => (list, WireShortcutCueOperation::Back),
        [list, "skip"] => (list, WireShortcutCueOperation::Skip),
        [list, "reset"] => (list, WireShortcutCueOperation::Reset),
        _ => return Err(bad()),
    };
    Ok(WireShortcutAction::Cue {
        list: nonzero(list).ok_or_else(bad)?,
        operation,
    })
}

/// Encodes `shortcut`, `name`, `enabled`, `key`, `osc`, and `action`.
pub(super) fn encode_spec(
    shortcut: u64,
    spec: &ShortcutSpec,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    validate_shortcut_id(shortcut)?;
    validate_shortcut_spec(spec)?;
    push("shortcut", shortcut.to_string())?;
    push("name", spec.name.clone())?;
    push("enabled", flag(spec.enabled).to_owned())?;
    push("key", encode_key(spec.key.as_ref()))?;
    push("osc", spec.osc.clone().unwrap_or_else(|| "-".to_owned()))?;
    push("action", encode_action(&spec.action))
}

/// Decodes `name`, `enabled`, `key`, `osc`, and `action`.
pub(super) fn decode_spec(fields: &mut dyn FieldSource) -> Result<ShortcutSpec, CodecError> {
    let name = fields.required("name")?;
    let enabled = fields.required("enabled")?;
    let enabled = parse_flag(&enabled).ok_or_else(|| invalid("enabled", enabled))?;
    let key = decode_key(&fields.required("key")?)?;
    let osc = fields.required("osc")?;
    let osc = (osc != "-").then_some(osc);
    let action = decode_action(&fields.required("action")?)?;
    let spec = ShortcutSpec {
        name,
        enabled,
        key,
        osc,
        action,
    };
    validate_shortcut_spec(&spec)?;
    Ok(spec)
}

/// Encodes a whole shortcut.
pub(super) fn encode_status(
    status: &ShortcutStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    encode_spec(status.id, &status.spec, push)
}

/// Decodes a whole shortcut.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<ShortcutStatus, CodecError> {
    Ok(ShortcutStatus {
        id: decode_shortcut_id(fields)?,
        spec: decode_spec(fields)?,
    })
}

/// Validates a snapshot's `shortcut_states` count.
pub(super) fn parse_state_count(value: &str) -> Result<u16, CodecError> {
    parse_canonical_u64(value)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| usize::from(*count) <= MAX_SHORTCUTS)
        .ok_or_else(|| invalid("shortcut_states", value))
}

/// Whether `count` is a valid `shortcut_states` count.
pub(super) fn state_count_is_valid(count: u16) -> bool {
    usize::from(count) <= MAX_SHORTCUTS
}
