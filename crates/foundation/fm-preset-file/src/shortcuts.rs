//! Shortcut sets shared between projects and consoles: `.fmshortcuts`
//! files (RC-004).
//!
//! ```text
//! {"schema_version": 67, "kind": "shortcut_set", "name": "Studio A",
//!  "shortcuts": [
//!    {"name": "Intro", "enabled": true,
//!     "key": {"key": "F9", "command": false, "shift": true, "alt": false},
//!     "osc": "intro", "action": "actions:overlay_on.1.4/record.1.1"},
//!    {"name": "Open", "enabled": true, "key": null, "osc": null,
//!     "action": "cue:1:go:-"}]}
//! ```
//!
//! A set holds 1 to [`MAX_SHORTCUTS`] shortcuts without their project ids,
//! with unique names, keys, and OSC names. `action` is written in the
//! protocol's shortcut action notation
//! ([`fm_protocol::shortcut_action_text`]); the inputs, cue lists, and
//! scripts it names are the exporting project's ids and are checked when
//! the shortcut is fired, like any shortcut's.
//!
//! [`plan_shortcut_import`] merges a set into a project's shortcuts: a
//! shortcut whose name the project already uses replaces that shortcut's
//! definition (keeping its id), every other one is added under a fresh id,
//! and an identical one is left alone. The merged set must still fit the
//! project and keep keys and OSC names unique; otherwise nothing is
//! imported. The plan orders its edits so every intermediate project is
//! valid too, since a console sends them one `shortcut_set` at a time.

use core::fmt::{self, Write as _};
use std::path::Path;

use fm_protocol::{
    MAX_SHORTCUTS, ShortcutSpec, ShortcutStatus, WireShortcutKey, parse_shortcut_action_text,
    shortcut_action_text, validate_shortcut_spec,
};

use crate::json::{Object, Value, escape_string, parse_array, parse_optional, syntax};
use crate::{PresetFileError, check_name, header, read_text, root_within, write_atomically};

/// File extension of a shortcut set.
pub const SHORTCUT_SET_EXTENSION: &str = "fmshortcuts";
/// Longest shortcut set name, in bytes.
pub const MAX_SHORTCUT_SET_NAME_BYTES: usize = 128;
/// Largest `.fmshortcuts` document, in bytes: 64 shortcuts of 16 actions
/// and 512 bytes of title text each, escaped, fit with room to spare.
pub const MAX_SHORTCUT_SET_FILE_BYTES: u64 = 1024 * 1024;

const KIND: &str = "shortcut_set";

/// A shortcut set as shared in a `.fmshortcuts` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutSetFile {
    pub name: String,
    pub shortcuts: Vec<ShortcutSpec>,
}

fn invalid(message: impl Into<String>) -> PresetFileError {
    PresetFileError::Invalid(message.into())
}

/// Checks a set: 1 to [`MAX_SHORTCUTS`] valid shortcuts with unique names,
/// keys, and OSC names.
fn check_set(set: &ShortcutSetFile) -> Result<(), PresetFileError> {
    check_name(&set.name, MAX_SHORTCUT_SET_NAME_BYTES)?;
    if set.shortcuts.is_empty() || set.shortcuts.len() > MAX_SHORTCUTS {
        return Err(invalid(format!(
            "a shortcut set holds 1 to {MAX_SHORTCUTS} shortcuts, not {}",
            set.shortcuts.len()
        )));
    }
    for (index, shortcut) in set.shortcuts.iter().enumerate() {
        validate_shortcut_spec(shortcut)
            .map_err(|error| invalid(format!("shortcut {:?}: {error}", shortcut.name)))?;
        let earlier = &set.shortcuts[..index];
        if earlier.iter().any(|other| other.name == shortcut.name) {
            return Err(invalid(format!(
                "shortcut name {:?} is used twice",
                shortcut.name
            )));
        }
        if let Some(key) = &shortcut.key
            && let Some(other) = earlier.iter().find(|other| other.key.as_ref() == Some(key))
        {
            return Err(invalid(format!(
                "shortcuts {:?} and {:?} share a key",
                other.name, shortcut.name
            )));
        }
        if let Some(osc) = &shortcut.osc
            && let Some(other) = earlier.iter().find(|other| other.osc.as_ref() == Some(osc))
        {
            return Err(invalid(format!(
                "shortcuts {:?} and {:?} share an OSC name",
                other.name, shortcut.name
            )));
        }
    }
    Ok(())
}

/// Encodes a `.fmshortcuts` document.
///
/// # Errors
///
/// [`PresetFileError::Invalid`] for a bad set name or an empty, oversized,
/// invalid, or self-conflicting set.
pub fn encode_shortcut_set_file(set: &ShortcutSetFile) -> Result<String, PresetFileError> {
    check_set(set)?;
    let mut output = String::new();
    header(&mut output, KIND, &set.name);
    output.push_str("\"shortcuts\": [");
    for (index, shortcut) in set.shortcuts.iter().enumerate() {
        output.push_str(if index == 0 { "\n  " } else { ",\n  " });
        write_shortcut(&mut output, shortcut);
    }
    output.push_str("\n]}\n");
    Ok(output)
}

fn write_shortcut(output: &mut String, shortcut: &ShortcutSpec) {
    output.push_str("{\"name\": \"");
    escape_string(output, &shortcut.name);
    write!(output, "\", \"enabled\": {}, \"key\": ", shortcut.enabled)
        .expect("writing to a string");
    match &shortcut.key {
        Some(key) => {
            output.push_str("{\"key\": \"");
            escape_string(output, &key.key);
            write!(
                output,
                "\", \"command\": {}, \"shift\": {}, \"alt\": {}}}",
                key.command, key.shift, key.alt
            )
            .expect("writing to a string");
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
    output.push_str(", \"action\": \"");
    escape_string(output, &shortcut_action_text(&shortcut.action));
    output.push_str("\"}");
}

/// Decodes a `.fmshortcuts` document.
///
/// # Errors
///
/// [`PresetFileError`] for an oversized, malformed, wrong-schema,
/// wrong-kind, or invalid document.
pub fn decode_shortcut_set_file(source: &str) -> Result<ShortcutSetFile, PresetFileError> {
    let mut root = root_within(source, KIND, MAX_SHORTCUT_SET_FILE_BYTES)?;
    let name = root.string("name")?;
    let shortcuts = parse_array(root.take("shortcuts")?, "shortcuts", parse_shortcut)?;
    root.finish()?;
    let set = ShortcutSetFile { name, shortcuts };
    check_set(&set)?;
    Ok(set)
}

fn parse_shortcut(value: Value) -> Result<ShortcutSpec, PresetFileError> {
    let mut object = Object::new(value, "shortcut")?;
    let name = object.string("name")?;
    let enabled = object.boolean("enabled")?;
    let key = parse_optional(object.take("key")?, parse_key)?;
    let osc = parse_optional(object.take("osc")?, |value| match value {
        Value::String(name) => Ok(name),
        _ => Err(syntax("shortcut osc must be a string or null")),
    })?;
    let action = object.string("action")?;
    object.finish()?;
    let action = parse_shortcut_action_text(&action)
        .map_err(|error| invalid(format!("shortcut {name:?} action: {error}")))?;
    Ok(ShortcutSpec {
        name,
        enabled,
        key,
        osc,
        action,
    })
}

fn parse_key(value: Value) -> Result<WireShortcutKey, PresetFileError> {
    let mut object = Object::new(value, "shortcut key")?;
    let key = WireShortcutKey {
        key: object.string("key")?,
        command: object.boolean("command")?,
        shift: object.boolean("shift")?,
        alt: object.boolean("alt")?,
    };
    object.finish()?;
    Ok(key)
}

/// Writes a `.fmshortcuts` file atomically.
///
/// # Errors
///
/// [`PresetFileError`] when the set does not encode or the file cannot be
/// written.
pub fn write_shortcut_set_file(path: &Path, set: &ShortcutSetFile) -> Result<(), PresetFileError> {
    let document = encode_shortcut_set_file(set)?;
    write_atomically(path, document.as_bytes())?;
    Ok(())
}

/// Reads and decodes a `.fmshortcuts` file.
///
/// # Errors
///
/// [`PresetFileError`] when the file cannot be read or does not decode.
pub fn read_shortcut_set_file(path: &Path) -> Result<ShortcutSetFile, PresetFileError> {
    decode_shortcut_set_file(&read_text(path, MAX_SHORTCUT_SET_FILE_BYTES)?)
}

/// Why a shortcut set cannot be merged into a project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutImportError {
    /// The merged project would hold `total` shortcuts, over
    /// [`MAX_SHORTCUTS`].
    TooMany { total: usize },
    /// Imported shortcut `imported` takes the key project shortcut
    /// `existing` keeps.
    KeyTaken { imported: String, existing: String },
    /// Imported shortcut `imported` takes the OSC name project shortcut
    /// `existing` keeps.
    OscTaken { imported: String, existing: String },
    /// The replacements swap keys or OSC names among themselves, which
    /// one-at-a-time edits cannot reach.
    Swap { first: String, second: String },
    /// No shortcut id is free.
    NoFreeId,
}

impl fmt::Display for ShortcutImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany { total } => write!(
                formatter,
                "the project would hold {total} shortcuts; the maximum is {MAX_SHORTCUTS}"
            ),
            Self::KeyTaken { imported, existing } => write!(
                formatter,
                "imported shortcut {imported:?} uses the key of shortcut {existing:?}"
            ),
            Self::OscTaken { imported, existing } => write!(
                formatter,
                "imported shortcut {imported:?} uses the OSC name of shortcut {existing:?}"
            ),
            Self::Swap { first, second } => write!(
                formatter,
                "shortcuts {first:?} and {second:?} swap a key or OSC name; \
                 clear one of them first"
            ),
            Self::NoFreeId => formatter.write_str("no shortcut id is free"),
        }
    }
}

impl std::error::Error for ShortcutImportError {}

/// The edits that merge a shortcut set into a project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortcutImportPlan {
    /// Every shortcut to set, in an order whose every step leaves the
    /// project's keys and OSC names unique.
    pub sets: Vec<ShortcutStatus>,
    /// Shortcuts the set adds.
    pub added: usize,
    /// Project shortcuts (matched by name) the set redefines.
    pub replaced: usize,
    /// Project shortcuts the set already matches exactly.
    pub unchanged: usize,
}

/// Whether `candidate` keeps a key or OSC name `other` already has.
fn clashes(candidate: &ShortcutSpec, other: &ShortcutSpec) -> Option<bool> {
    if candidate.key.is_some() && candidate.key == other.key {
        Some(true)
    } else if candidate.osc.is_some() && candidate.osc == other.osc {
        Some(false)
    } else {
        None
    }
}

/// The first shortcut of `shortcuts` (other than `id`) that `candidate`
/// clashes with, and whether over a key (else an OSC name).
fn first_clash<'a>(
    shortcuts: &'a [ShortcutStatus],
    id: u64,
    candidate: &ShortcutSpec,
) -> Option<(&'a ShortcutStatus, bool)> {
    shortcuts
        .iter()
        .filter(|other| other.id != id)
        .find_map(|other| clashes(candidate, &other.spec).map(|key| (other, key)))
}

/// A fresh id after `taken`: one past the largest, else the lowest free.
fn fresh_id(taken: &[u64]) -> Option<u64> {
    taken
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .or_else(|| (1..=u64::MAX).find(|id| !taken.contains(id)))
}

/// Plans the merge of `imported` (a decoded set) into `existing` (the
/// project's shortcuts, in project order): see the module documentation.
///
/// # Errors
///
/// [`ShortcutImportError`] when the merged project would break a bound or
/// cannot be reached one edit at a time; nothing is to be imported then.
pub fn plan_shortcut_import(
    existing: &[ShortcutStatus],
    imported: &[ShortcutSpec],
) -> Result<ShortcutImportPlan, ShortcutImportError> {
    let mut plan = ShortcutImportPlan::default();
    let mut pending = Vec::new();
    let mut ids: Vec<u64> = existing.iter().map(|shortcut| shortcut.id).collect();
    for spec in imported {
        match existing
            .iter()
            .find(|shortcut| shortcut.spec.name == spec.name)
        {
            Some(found) if found.spec == *spec => plan.unchanged += 1,
            Some(found) => {
                plan.replaced += 1;
                pending.push(ShortcutStatus {
                    id: found.id,
                    spec: spec.clone(),
                });
            }
            None => {
                let id = fresh_id(&ids).ok_or(ShortcutImportError::NoFreeId)?;
                ids.push(id);
                plan.added += 1;
                pending.push(ShortcutStatus {
                    id,
                    spec: spec.clone(),
                });
            }
        }
    }
    let total = existing.len() + plan.added;
    if total > MAX_SHORTCUTS {
        return Err(ShortcutImportError::TooMany { total });
    }
    // The merged project: every pending edit applied at once.
    let mut merged: Vec<ShortcutStatus> = existing.to_vec();
    for edit in &pending {
        match merged.iter_mut().find(|shortcut| shortcut.id == edit.id) {
            Some(shortcut) => shortcut.spec = edit.spec.clone(),
            None => merged.push(edit.clone()),
        }
    }
    for edit in &pending {
        if let Some((other, key)) = first_clash(&merged, edit.id, &edit.spec) {
            let imported = edit.spec.name.clone();
            let existing = other.spec.name.clone();
            return Err(if key {
                ShortcutImportError::KeyTaken { imported, existing }
            } else {
                ShortcutImportError::OscTaken { imported, existing }
            });
        }
    }
    // One edit at a time: each next edit is one that clashes with nothing
    // the project holds by then.
    let mut current: Vec<ShortcutStatus> = existing.to_vec();
    while !pending.is_empty() {
        let Some(index) = pending
            .iter()
            .position(|edit| first_clash(&current, edit.id, &edit.spec).is_none())
        else {
            let first = &pending[0];
            let second = first_clash(&current, first.id, &first.spec)
                .map(|(other, _)| other.spec.name.clone())
                .unwrap_or_default();
            return Err(ShortcutImportError::Swap {
                first: first.spec.name.clone(),
                second,
            });
        };
        let edit = pending.remove(index);
        match current.iter_mut().find(|shortcut| shortcut.id == edit.id) {
            Some(shortcut) => shortcut.spec = edit.spec.clone(),
            None => current.push(edit.clone()),
        }
        plan.sets.push(edit);
    }
    Ok(plan)
}
