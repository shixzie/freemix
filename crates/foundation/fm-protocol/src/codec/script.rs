//! Script codec (Protocol 2.58, RC-011).
//!
//! `script` is a nonzero canonical id, `name` the script name (validated by
//! [`crate::script_name_is_valid`]), and `source` the lowercase hex of the
//! source's UTF-8 bytes, so newlines and separators never reach the record.
//! A `script_state` or `script_changed` whose source is withheld from the
//! session carries `source=hidden` (Protocol 2.104); `script_set` never
//! does.

use crate::{
    CodecError, HIDDEN_SCRIPT_SOURCE, MAX_SCRIPT_RUNS, MAX_SCRIPTS, ScriptRunStatus, ScriptSpec,
    ScriptStatus, script_name_is_valid, script_source_is_valid,
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

/// Encodes a whole script; a withheld source is written as
/// [`HIDDEN_SCRIPT_SOURCE`] (Protocol 2.104).
pub(super) fn encode_status(status: &ScriptStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    if !status.source_hidden {
        return encode_spec(status.id, &status.spec, push);
    }
    validate_script_id(status.id)?;
    validate_spec(&status.spec)?;
    if !status.spec.source.is_empty() {
        return Err(invalid("source", "a withheld source is empty"));
    }
    push("script", status.id.to_string())?;
    push("name", status.spec.name.clone())?;
    push("source", HIDDEN_SCRIPT_SOURCE.to_owned())
}

/// Decodes a whole script, its source possibly withheld.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<ScriptStatus, CodecError> {
    let id = decode_script_id(fields)?;
    let name = fields.required("name")?;
    let source = fields.required("source")?;
    let source_hidden = source == HIDDEN_SCRIPT_SOURCE;
    let spec = ScriptSpec {
        name,
        source: if source_hidden {
            String::new()
        } else {
            unhex("source", &source)?
        },
    };
    validate_spec(&spec)?;
    Ok(ScriptStatus {
        id,
        spec,
        source_hidden,
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

/// Encodes the `runs` field of `script_stop` (Protocol 2.146): 1 to
/// [`MAX_SCRIPT_RUNS`] nonzero ids, ascending, comma separated. An empty
/// list names every run and writes no field.
pub(super) fn encode_run_list(runs: &[u64]) -> Result<String, CodecError> {
    if !run_list_is_valid(runs) {
        return Err(invalid("runs", format!("{runs:?}")));
    }
    Ok(runs
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(","))
}

/// Decodes the optional `runs` field of `script_stop` (see
/// [`encode_run_list`]); absent means every run.
pub(super) fn decode_run_list(fields: &mut dyn FieldSource) -> Result<Vec<u64>, CodecError> {
    let Some(value) = fields.optional("runs") else {
        return Ok(Vec::new());
    };
    value
        .split(',')
        .map(parse_canonical_u64)
        .collect::<Option<Vec<u64>>>()
        .filter(|runs| run_list_is_valid(runs))
        .ok_or_else(|| invalid("runs", value))
}

fn run_list_is_valid(runs: &[u64]) -> bool {
    (1..=MAX_SCRIPT_RUNS).contains(&runs.len())
        && runs.first().is_some_and(|first| *first != 0)
        && runs.windows(2).all(|pair| pair[0] < pair[1])
}

/// Encodes the scripts of `script_runs_stopped` (Protocol 2.69): 1 to
/// [`MAX_SCRIPTS`] nonzero ids, ascending, comma separated.
pub(super) fn encode_script_list(scripts: &[u64]) -> Result<String, CodecError> {
    if !script_list_is_valid(scripts) {
        return Err(invalid("scripts", format!("{scripts:?}")));
    }
    Ok(scripts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(","))
}

/// Decodes the scripts of `script_runs_stopped` (see
/// [`encode_script_list`]).
pub(super) fn decode_script_list(value: &str) -> Result<Vec<u64>, CodecError> {
    let scripts = value
        .split(',')
        .map(parse_canonical_u64)
        .collect::<Option<Vec<u64>>>()
        .filter(|scripts| script_list_is_valid(scripts));
    scripts.ok_or_else(|| invalid("scripts", value))
}

fn script_list_is_valid(scripts: &[u64]) -> bool {
    (1..=MAX_SCRIPTS).contains(&scripts.len())
        && scripts.first().is_some_and(|first| *first != 0)
        && scripts.windows(2).all(|pair| pair[0] < pair[1])
}

/// Encodes a run status (Protocol 2.62; the run id and the wait's line
/// since Protocol 2.69): `idle`,
/// `waiting:<run>:<started>:<due>:<waits>:<wait frames>:<line>`, or
/// `faulted:<run>:<frame>:<failure>:<line>:<column>:<reason>:<message>`
/// (the detail since Protocol 2.63, see
/// [`super::refusal::encode_fault_detail`]).
pub(super) fn encode_run(run: &ScriptRunStatus) -> Result<String, CodecError> {
    Ok(match run {
        ScriptRunStatus::Idle => "idle".to_owned(),
        ScriptRunStatus::Waiting {
            run: id,
            started_frame,
            due_frame,
            waits,
            wait_frames,
            line,
        } => {
            if *id == 0
                || due_frame <= started_frame
                || *waits == 0
                || *wait_frames == 0
                || *line == 0
            {
                return Err(invalid("run", format!("{run:?}")));
            }
            format!("waiting:{id}:{started_frame}:{due_frame}:{waits}:{wait_frames}:{line}")
        }
        ScriptRunStatus::Faulted {
            run: id,
            frame,
            failure,
            line,
            column,
            detail,
        } => {
            if *id == 0 || *line == 0 || *column == 0 {
                return Err(invalid("run", format!("{run:?}")));
            }
            format!(
                "faulted:{id}:{frame}:{}:{line}:{column}:{}",
                failure.token(),
                super::refusal::encode_fault_detail("run", detail, ':')?
            )
        }
    })
}

/// Decodes a run status (see [`encode_run`]).
pub(super) fn decode_run(value: &str) -> Result<ScriptRunStatus, CodecError> {
    let number = |token: &str| parse_canonical_u64(token);
    let id = |token: &str| parse_canonical_u64(token).filter(|value| *value != 0);
    let small = |token: &str| {
        parse_canonical_u64(token)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value != 0)
    };
    let parts: Vec<&str> = value.split(':').collect();
    let run = match parts.as_slice() {
        ["idle"] => Some(ScriptRunStatus::Idle),
        ["waiting", run, started, due, waits, frames, line] => id(run)
            .zip(number(started))
            .zip(number(due))
            .zip(small(waits))
            .zip(small(frames))
            .zip(small(line))
            .filter(|(((((_, started), due), _), _), _)| due > started)
            .map(
                |(((((run, started_frame), due_frame), waits), wait_frames), line)| {
                    ScriptRunStatus::Waiting {
                        run,
                        started_frame,
                        due_frame,
                        waits,
                        wait_frames,
                        line,
                    }
                },
            ),
        [
            "faulted",
            run,
            frame,
            failure,
            line,
            column,
            reason,
            message,
        ] => {
            let detail = super::refusal::decode_fault_detail("run", reason, message)?;
            id(run)
                .zip(number(frame))
                .zip(fm_command::ScriptFailureKind::from_token(failure))
                .zip(small(line))
                .zip(small(column))
                .map(
                    |((((run, frame), failure), line), column)| ScriptRunStatus::Faulted {
                        run,
                        frame,
                        failure,
                        line,
                        column,
                        detail,
                    },
                )
        }
        _ => None,
    };
    run.ok_or_else(|| invalid("run", value))
}
