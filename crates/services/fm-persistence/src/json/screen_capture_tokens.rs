//! Screen capture inputs' desktop restore tokens in `project.json` (IN-011,
//! schema 72).
//!
//! ```text
//! "screen_capture_restore_tokens": [
//!   {"input": 7, "token": "3a7c6b52-1f0e-4f43-9a51-2d1c0f2e8b11"}
//! ]
//! ```
//!
//! A Wayland desktop's screen sharing dialog grants a restore token with
//! each share; the server keeps the newest one beside its input, so a
//! reopened project shares the same screen or window again without asking.
//! Entries are written in strictly ascending input order. Whether each
//! names a screen capture input and holds a usable token is reported by
//! project validation.

use core::fmt::Write as _;

use fm_model::Project;
use fm_types::InputId;

use super::{DecodeError, Object, escape_string, parse_array, syntax};

/// Writes `,\n    "screen_capture_restore_tokens": [...]`.
pub(crate) fn write_screen_capture_restore_tokens(output: &mut String, project: &Project) {
    output.push_str(",\n    \"screen_capture_restore_tokens\": [");
    let tokens = project.screen_capture_restore_tokens();
    for (index, (input, token)) in tokens.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"input\": {input}, \"token\": \"")
            .expect("writing to a string cannot fail");
        escape_string(output, token);
        output.push_str("\"}");
    }
    if !tokens.is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

/// Parses `"screen_capture_restore_tokens"` from the project object.
pub(crate) fn parse_screen_capture_restore_tokens(
    object: &mut Object,
) -> Result<Vec<(InputId, String)>, DecodeError> {
    let tokens = parse_array(
        object.take("screen_capture_restore_tokens")?,
        "screen_capture_restore_tokens",
        |value| {
            let mut entry = Object::new(value, "screen capture restore token")?;
            let input = InputId::new(entry.nonzero_u128("input")?);
            let token = entry.string("token")?;
            entry.finish()?;
            Ok((input, token))
        },
    )?;
    if tokens.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "screen_capture_restore_tokens must be in strictly ascending input order",
        ));
    }
    Ok(tokens)
}
