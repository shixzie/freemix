//! Operator safe-area guide selection in `project.json` (SW-014, schema 112).
//!
//! ```text
//! "safe_area_guides": "action,title,9:16"
//! ```
//!
//! The value is the canonical token list: `-` for none, else comma-separated
//! `action`, `title`, `4:3`, `16:9`, and `9:16` in that order. An absent key
//! selects title safe, the console's historical default.

use fm_model::SafeAreaGuideSelection;

use super::{DecodeError, Object, Value, syntax};

/// Writes `"safe_area_guides"` after the preceding project field.
pub(crate) fn write_safe_area_guides(output: &mut String, project: &fm_model::Project) {
    use std::fmt::Write as _;
    let _ = write!(
        output,
        ",\n    \"safe_area_guides\": \"{}\"",
        project.safe_area_guides().tokens()
    );
}

/// Parses `safe_area_guides`, defaulting to title safe when the key is
/// absent.
pub(crate) fn parse_safe_area_guides(
    object: &mut Object,
) -> Result<SafeAreaGuideSelection, DecodeError> {
    let Some(value) = object.take_optional("safe_area_guides") else {
        return Ok(SafeAreaGuideSelection::default());
    };
    let Value::String(text) = value else {
        return Err(syntax("safe_area_guides must be a string"));
    };
    SafeAreaGuideSelection::from_tokens(&text).ok_or_else(|| {
        syntax(
            "safe_area_guides must be \"-\" or a comma-separated subset of action, title, 4:3, 16:9, 9:16",
        )
    })
}
