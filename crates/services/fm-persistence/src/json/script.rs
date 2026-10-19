//! The project's scripts in `project.json` (schema 54, RC-011).
//!
//! ```text
//! "scripts": [
//!   {"id": 1, "name": "Open", "source": "preview 2\ntransition fade, 25"}
//! ]
//! ```
//!
//! The source is stored verbatim (it may hold a syntax error); bounds and
//! duplicate ids are reported by project validation.
//!
//! The show's script run slot follows (schema 56): the run waiting at a
//! `wait` with its interpreter state, or the fault the last run ended with.
//!
//! ```text
//! "script_run": {"running": {"script": 1, "started_frame": 40,
//!   "due_frame": 65, "path": [{"statement": 2, "branch": 1},
//!   {"statement": 0, "branch": 0}], "variables": [{"name": "n", "int": 3},
//!   {"name": "label", "string": "Live"}, {"name": "on", "bool": true}],
//!   "instructions": 18, "actions": 2, "waits": 1, "wait_frames": 25},
//!  "fault": null}
//! ```
//!
//! A fault is `{"script", "frame", "failure", "line", "column"}`, `failure`
//! one of `syntax`, `runtime`, `refused`, `instructions`, `actions`,
//! `variables`, `string_bytes`, `waits`, and `wait_frames`. Whether a run
//! fits its script is reported by project validation.

use core::fmt::Write as _;

use fm_model::{Script, ScriptFault, ScriptRun, ScriptRunState};
use fm_script::{Budget, FailureKind, ResumePoint, ScriptValue, Span, Suspended};

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

/// Writes `,\n    "scripts": [...]`.
pub(crate) fn write_scripts(output: &mut String, scripts: &[Script]) {
    output.push_str(",\n    \"scripts\": [");
    for (index, script) in scripts.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"id\": {}, \"name\": \"", script.id)
            .expect("writing to a string cannot fail");
        escape_string(output, &script.name);
        output.push_str("\", \"source\": \"");
        escape_string(output, &script.source);
        output.push_str("\"}");
    }
    if !scripts.is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

/// Parses `"scripts"` from the project object.
pub(crate) fn parse_scripts(object: &mut Object) -> Result<Vec<Script>, DecodeError> {
    parse_array(object.take("scripts")?, "scripts", parse_script)
}

fn parse_script(value: Value) -> Result<Script, DecodeError> {
    let mut object = Object::new(value, "script")?;
    let id = object.u64("id")?;
    let name = object.string("name")?;
    let source = object.string("source")?;
    object.finish()?;
    Ok(Script { id, name, source })
}

/// The stable token of a run failure.
const fn failure_token(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Syntax => "syntax",
        FailureKind::Runtime => "runtime",
        FailureKind::Refused => "refused",
        FailureKind::Budget(budget) => budget.token(),
    }
}

fn parse_failure(token: &str) -> Result<FailureKind, DecodeError> {
    Ok(match token {
        "syntax" => FailureKind::Syntax,
        "runtime" => FailureKind::Runtime,
        "refused" => FailureKind::Refused,
        "instructions" => FailureKind::Budget(Budget::Instructions),
        "actions" => FailureKind::Budget(Budget::Actions),
        "variables" => FailureKind::Budget(Budget::Variables),
        "string_bytes" => FailureKind::Budget(Budget::StringBytes),
        "waits" => FailureKind::Budget(Budget::Waits),
        "wait_frames" => FailureKind::Budget(Budget::WaitFrames),
        value => return Err(super::unknown_enum("script failure", value)),
    })
}

/// Writes `,\n    "script_run": {...}` (schema 56).
pub(crate) fn write_script_run(output: &mut String, state: &ScriptRunState) {
    output.push_str(",\n    \"script_run\": {\"running\": ");
    match &state.running {
        None => output.push_str("null"),
        Some(run) => write_run(output, run),
    }
    output.push_str(", \"fault\": ");
    match &state.fault {
        None => output.push_str("null"),
        Some(fault) => write!(
            output,
            "{{\"script\": {}, \"frame\": {}, \"failure\": \"{}\", \"line\": {}, \"column\": {}}}",
            fault.script,
            fault.frame,
            failure_token(fault.failure),
            fault.at.line,
            fault.at.column
        )
        .expect("writing to a string cannot fail"),
    }
    output.push('}');
}

fn write_run(output: &mut String, run: &ScriptRun) {
    write!(
        output,
        "{{\"script\": {}, \"started_frame\": {}, \"due_frame\": {}, \"path\": [",
        run.script, run.started_frame, run.due_frame
    )
    .expect("writing to a string cannot fail");
    for (index, point) in run.state.path.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"statement\": {}, \"branch\": {}}}",
            point.statement, point.branch
        )
        .expect("writing to a string cannot fail");
    }
    output.push_str("], \"variables\": [");
    for (index, (name, value)) in run.state.variables.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str("{\"name\": \"");
        escape_string(output, name);
        match value {
            ScriptValue::Int(value) => {
                write!(output, "\", \"int\": {value}}}").expect("writing to a string cannot fail");
            }
            ScriptValue::Bool(value) => {
                write!(output, "\", \"bool\": {value}}}").expect("writing to a string cannot fail");
            }
            ScriptValue::Str(value) => {
                output.push_str("\", \"string\": \"");
                escape_string(output, value);
                output.push_str("\"}");
            }
        }
    }
    write!(
        output,
        "], \"instructions\": {}, \"actions\": {}, \"waits\": {}, \"wait_frames\": {}}}",
        run.state.instructions, run.state.actions, run.state.waits, run.state.wait_frames
    )
    .expect("writing to a string cannot fail");
}

/// Parses `"script_run"` from the project object (schema 56).
pub(crate) fn parse_script_run(object: &mut Object) -> Result<ScriptRunState, DecodeError> {
    let mut object = Object::new(object.take("script_run")?, "script run")?;
    let running = parse_optional(object.take("running")?, parse_run)?;
    let fault = parse_optional(object.take("fault")?, |value| {
        let mut fault = Object::new(value, "script fault")?;
        let parsed = ScriptFault {
            script: fault.u64("script")?,
            frame: fault.u64("frame")?,
            failure: parse_failure(&fault.string("failure")?)?,
            at: Span::new(fault.u32("line")?, fault.u32("column")?),
        };
        fault.finish()?;
        Ok(parsed)
    })?;
    object.finish()?;
    Ok(ScriptRunState { running, fault })
}

fn parse_run(value: Value) -> Result<ScriptRun, DecodeError> {
    let mut object = Object::new(value, "script running")?;
    let script = object.u64("script")?;
    let started_frame = object.u64("started_frame")?;
    let due_frame = object.u64("due_frame")?;
    let path = parse_array(object.take("path")?, "script path", |value| {
        let mut point = Object::new(value, "script path point")?;
        let parsed = ResumePoint {
            statement: point.u32("statement")?,
            branch: point.u64("branch")?,
        };
        point.finish()?;
        Ok(parsed)
    })?;
    let variables = parse_array(object.take("variables")?, "script variables", |value| {
        let Value::Object(fields) = &value else {
            return Err(syntax("script variable must be an object"));
        };
        let kind = ["int", "bool", "string"]
            .into_iter()
            .find(|kind| fields.contains_key(*kind))
            .ok_or_else(|| syntax("script variable needs int, bool, or string"))?;
        let mut variable = Object::new(value, "script variable")?;
        let name = variable.string("name")?;
        let parsed = match kind {
            "int" => ScriptValue::Int(variable.i64("int")?),
            "bool" => ScriptValue::Bool(variable.boolean("bool")?),
            _ => ScriptValue::Str(variable.string("string")?),
        };
        variable.finish()?;
        Ok((name, parsed))
    })?;
    let state = Suspended {
        path,
        variables,
        instructions: object.u32("instructions")?,
        actions: object.u32("actions")?,
        waits: object.u32("waits")?,
        wait_frames: object.u32("wait_frames")?,
    };
    object.finish()?;
    Ok(ScriptRun {
        script,
        started_frame,
        due_frame,
        state,
    })
}
