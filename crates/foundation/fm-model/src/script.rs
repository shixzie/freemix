//! Named project scripts (RC-011, schema 54): bounded `FreeMix` Script
//! sources an operator runs on demand.
//!
//! The project stores each script's name and source text exactly as
//! written; a source need not parse (a draft with a syntax error may be
//! saved, and running it reports the error with its line and column). Only
//! the structural bounds are checked here: the script count, unique ids,
//! the name, and the source size. What a script may do is decided by
//! `fm-script` and the engine when it runs.

use core::fmt;

/// Most scripts one project holds.
pub const MAX_SCRIPTS: usize = 16;
/// Longest script name, in UTF-8 bytes.
pub const MAX_SCRIPT_NAME_BYTES: usize = 64;
/// Longest script source, in UTF-8 bytes.
pub const MAX_SCRIPT_SOURCE_BYTES: usize = fm_script::MAX_SOURCE_BYTES;

/// One named script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
    /// Stable identity, nonzero and unique within the project.
    pub id: u64,
    pub name: String,
    pub source: String,
}

/// Why a script or the project's script set is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptError {
    /// The id is zero.
    InvalidId,
    /// The name is blank, longer than [`MAX_SCRIPT_NAME_BYTES`], or holds a
    /// control character.
    InvalidName,
    /// The source is longer than [`MAX_SCRIPT_SOURCE_BYTES`] or holds a
    /// control character other than a newline, tab, or carriage return.
    InvalidSource,
    TooManyScripts,
    DuplicateId(u64),
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("script id must not be zero"),
            Self::InvalidName => write!(
                formatter,
                "script name must be 1 to {MAX_SCRIPT_NAME_BYTES} bytes, not blank, without \
                 control characters"
            ),
            Self::InvalidSource => write!(
                formatter,
                "script source must be at most {MAX_SCRIPT_SOURCE_BYTES} bytes of text"
            ),
            Self::TooManyScripts => {
                write!(formatter, "a project holds at most {MAX_SCRIPTS} scripts")
            }
            Self::DuplicateId(id) => write!(formatter, "script {id} is duplicated"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// Whether `name` is a valid script name.
#[must_use]
pub fn script_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_SCRIPT_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// Whether `source` is within the stored source bounds.
#[must_use]
pub fn script_source_is_valid(source: &str) -> bool {
    source.len() <= MAX_SCRIPT_SOURCE_BYTES
        && !source
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
}

/// Checks one script's name and source.
///
/// # Errors
///
/// Returns the first [`ScriptError`].
pub fn validate_script_definition(name: &str, source: &str) -> Result<(), ScriptError> {
    if !script_name_is_valid(name) {
        return Err(ScriptError::InvalidName);
    }
    if !script_source_is_valid(source) {
        return Err(ScriptError::InvalidSource);
    }
    Ok(())
}

/// Checks the project's scripts: at most [`MAX_SCRIPTS`], nonzero unique
/// ids, each definition valid.
///
/// # Errors
///
/// Returns the first [`ScriptError`].
pub fn validate_scripts(scripts: &[Script]) -> Result<(), ScriptError> {
    if scripts.len() > MAX_SCRIPTS {
        return Err(ScriptError::TooManyScripts);
    }
    for (index, script) in scripts.iter().enumerate() {
        if script.id == 0 {
            return Err(ScriptError::InvalidId);
        }
        if scripts[..index].iter().any(|other| other.id == script.id) {
            return Err(ScriptError::DuplicateId(script.id));
        }
        validate_script_definition(&script.name, &script.source)?;
    }
    Ok(())
}
