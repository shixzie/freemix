//! Script codec (Protocol 2.58, RC-011).
//!
//! `script` is a nonzero canonical id, `name` the script name (validated by
//! [`crate::script_name_is_valid`]), and `source` the lowercase hex of the
//! source's UTF-8 bytes, so newlines and separators never reach the record.

use crate::{
    CodecError, MAX_SCRIPTS, ScriptRunStatus, ScriptSpec, ScriptStatus, script_name_is_valid,
    script_source_is_valid,
};

use super::cue::{hex, unhex};
use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Refuses script id zero.
pub(super) fn validate_script_id(script: u64) -> Result<(), CodecError> {
    if script == 0 {
        Err(invalid("script", "0"))
    } else {
        Ok(())
    }
}

/// Decodes a nonzero `script` field.
pub(super) fn decode_script_id(fields: &mut dyn FieldSource) -> Result<u64, CodecError> {
    let value = fields.required("script")?;
    parse_canonical_u64(&value)
        .filter(|script| *script != 0)
        .ok_or_else(|| invalid("script", value))
}

fn validate_spec(spec: &ScriptSpec) -> Result<(), CodecError> {
    if !script_name_is_valid(&spec.name) {
        return Err(invalid("name", spec.name.clone()));
    }
    if !script_source_is_valid(&spec.source) {
        return Err(invalid("source", format!("{} bytes", spec.source.len())));
    }
    Ok(())
}

/// Encodes `script`, `name`, and `source`.
pub(super) fn encode_spec(
    script: u64,
    spec: &ScriptSpec,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    validate_script_id(script)?;
    validate_spec(spec)?;
    push("script", script.to_string())?;
    push("name", spec.name.clone())?;
    push("source", hex(&spec.source))
}

/// Decodes `name` and `source`.
pub(super) fn decode_spec(fields: &mut dyn FieldSource) -> Result<ScriptSpec, CodecError> {
    let spec = ScriptSpec {
        name: fields.required("name")?,
        source: unhex("source", &fields.required("source")?)?,
    };
    validate_spec(&spec)?;
    Ok(spec)
}

/// Encodes a whole script.
pub(super) fn encode_status(status: &ScriptStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    encode_spec(status.id, &status.spec, push)
}

/// Decodes a whole script.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<ScriptStatus, CodecError> {
    Ok(ScriptStatus {
        id: decode_script_id(fields)?,
        spec: decode_spec(fields)?,
    })
}

/// Validates a snapshot's `script_states` count.
pub(super) fn parse_state_count(value: &str) -> Result<u16, CodecError> {
    parse_canonical_u64(value)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| usize::from(*count) <= MAX_SCRIPTS)
        .ok_or_else(|| invalid("script_states", value))
}

/// Whether `count` is a valid `script_states` count.
pub(super) fn state_count_is_valid(count: u16) -> bool {
    usize::from(count) <= MAX_SCRIPTS
}

/// Encodes a run status (Protocol 2.62): `idle`,
/// `waiting:<started>:<due>:<waits>:<wait frames>`, or
/// `faulted:<frame>:<failure>:<line>:<column>`.
pub(super) fn encode_run(run: ScriptRunStatus) -> Result<String, CodecError> {
    Ok(match run {
        ScriptRunStatus::Idle => "idle".to_owned(),
        ScriptRunStatus::Waiting {
            started_frame,
            due_frame,
            waits,
            wait_frames,
        } => {
            if due_frame <= started_frame || waits == 0 || wait_frames == 0 {
                return Err(invalid("run", format!("{run:?}")));
            }
            format!("waiting:{started_frame}:{due_frame}:{waits}:{wait_frames}")
        }
        ScriptRunStatus::Faulted {
            frame,
            failure,
            line,
            column,
        } => {
            if line == 0 || column == 0 {
                return Err(invalid("run", format!("{run:?}")));
            }
            format!("faulted:{frame}:{}:{line}:{column}", failure.token())
        }
    })
}

/// Decodes a run status (see [`encode_run`]).
pub(super) fn decode_run(value: &str) -> Result<ScriptRunStatus, CodecError> {
    let number = |token: &str| parse_canonical_u64(token);
    let small = |token: &str| {
        parse_canonical_u64(token)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value != 0)
    };
    let parts: Vec<&str> = value.split(':').collect();
    let run = match parts.as_slice() {
        ["idle"] => Some(ScriptRunStatus::Idle),
        ["waiting", started, due, waits, frames] => number(started)
            .zip(number(due))
            .zip(small(waits))
            .zip(small(frames))
            .filter(|(((started, due), _), _)| due > started)
            .map(
                |(((started_frame, due_frame), waits), wait_frames)| ScriptRunStatus::Waiting {
                    started_frame,
                    due_frame,
                    waits,
                    wait_frames,
                },
            ),
        ["faulted", frame, failure, line, column] => number(frame)
            .zip(fm_command::ScriptFailureKind::from_token(failure))
            .zip(small(line))
            .zip(small(column))
            .map(
                |(((frame, failure), line), column)| ScriptRunStatus::Faulted {
                    frame,
                    failure,
                    line,
                    column,
                },
            ),
        _ => None,
    };
    run.ok_or_else(|| invalid("run", value))
}
