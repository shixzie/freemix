//! Named project scripts (RC-011, schema 54): bounded `FreeMix` Script
//! sources an operator runs on demand.
//!
//! The project stores each script's name and source text exactly as
//! written; a source need not parse (a draft with a syntax error may be
//! saved, and running it reports the error with its line and column). Only
//! the structural bounds are checked here: the script count, unique ids,
//! the name, and the source size. What a script may do is decided by
//! `fm-script` and the engine when it runs.
//!
//! A script with `wait` spans frames (schema 56): the project saves the
//! one run waiting ([`ScriptRunState`]) with its interpreter state, so a
//! saved or restarted show resumes it on its due frame, or the fault the
//! last run ended with.

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
    /// The run slot does not fit the project's scripts (see
    /// [`validate_script_run`]).
    InvalidRun,
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
            Self::InvalidRun => formatter.write_str("the script run does not fit the scripts"),
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

/// Whether `source` parses and holds a `wait` (in any branch or loop): a
/// run of it may span frames, so no cue step may run it (schema 56). A
/// source that does not parse never waits (its runs fail at once).
#[must_use]
pub fn script_waits(source: &str) -> bool {
    fm_script::parse(source).is_ok_and(|program| program.waits())
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

/// Longest one run's `wait`s may add up to, in seconds (schema 56).
pub const MAX_SCRIPT_WAIT_SECONDS: u32 = 600;
/// Most `wait`s one run may pass (schema 56).
pub const MAX_SCRIPT_WAITS: u32 = fm_script::DEFAULT_WAITS;

/// The frames [`MAX_SCRIPT_WAIT_SECONDS`] last at `rate`, rounded up.
#[must_use]
pub fn script_wait_frames(rate: fm_types::FrameRate) -> u32 {
    let numerator = u128::from(MAX_SCRIPT_WAIT_SECONDS) * u128::from(rate.numerator());
    u32::try_from(numerator.div_ceil(u128::from(rate.denominator()))).unwrap_or(u32::MAX)
}

/// The budgets of one run of a project script at `rate`: the `fm-script`
/// defaults, with the `wait`s bounded to [`MAX_SCRIPT_WAITS`] adding up to
/// at most [`MAX_SCRIPT_WAIT_SECONDS`] of frames.
#[must_use]
pub fn script_limits(rate: fm_types::FrameRate) -> fm_script::Limits {
    fm_script::Limits {
        waits: MAX_SCRIPT_WAITS,
        wait_frames: script_wait_frames(rate),
        ..fm_script::Limits::default()
    }
}

/// A run waiting at a `wait` (schema 56): what it resumes and when.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptRun {
    /// The script running.
    pub script: u64,
    /// The frame the run started on.
    pub started_frame: u64,
    /// The frame the run resumes on.
    pub due_frame: u64,
    /// The interpreter state to resume.
    pub state: fm_script::Suspended,
}

/// Why the part of a run due on `frame` failed (schema 56): nothing of
/// that frame was applied and the run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptFault {
    pub script: u64,
    pub frame: u64,
    pub failure: fm_script::FailureKind,
    /// Where the run stopped (1-based).
    pub at: fm_script::Span,
}

/// The show's one script run slot (schema 56): the run waiting, or the
/// fault the last one ended with. Never both.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScriptRunState {
    pub running: Option<ScriptRun>,
    pub fault: Option<ScriptFault>,
}

impl ScriptRunState {
    /// The script running or faulted, if any.
    #[must_use]
    pub fn script(&self) -> Option<u64> {
        self.running
            .as_ref()
            .map(|run| run.script)
            .or(self.fault.map(|fault| fault.script))
    }
}

/// Checks the run slot against the project's `scripts` at `rate`: a run
/// names a script whose source parses and accepts the saved state under
/// [`script_limits`], and resumes no earlier than its waits allow; a fault
/// names a script and a position. A run and a fault are never both set.
///
/// # Errors
///
/// Returns [`ScriptError::InvalidRun`].
pub fn validate_script_run(
    state: &ScriptRunState,
    scripts: &[Script],
    rate: fm_types::FrameRate,
) -> Result<(), ScriptError> {
    let known = |script: u64| scripts.iter().find(|found| found.id == script);
    if let Some(run) = &state.running {
        let valid = state.fault.is_none()
            && known(run.script).is_some_and(|script| {
                fm_script::parse(&script.source)
                    .is_ok_and(|program| program.accepts(&run.state, &script_limits(rate)))
            })
            && run
                .started_frame
                .checked_add(u64::from(run.state.wait_frames))
                .is_some_and(|earliest| run.due_frame >= earliest);
        if !valid {
            return Err(ScriptError::InvalidRun);
        }
    }
    if let Some(fault) = &state.fault
        && (known(fault.script).is_none() || fault.at.line == 0 || fault.at.column == 0)
    {
        return Err(ScriptError::InvalidRun);
    }
    Ok(())
}
