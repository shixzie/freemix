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

use core::fmt::Write as _;

use fm_model::Script;

use super::{DecodeError, Object, Value, escape_string, parse_array};

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
