//! The PTZ preset virtual-input document in `project.json` (IN-028,
//! schema 106).
//!
//! ```text
//! "ptz_presets": "freemix-ptz-presets 1\ncount 2\n..."
//! ```
//!
//! The document is the strict, versioned form defined by
//! `fm_ptz::PresetVirtualInputs`; it is required to be canonical. An absent
//! field means the show defines no preset virtual inputs.

use fm_model::Project;

use super::{DecodeError, Object, Value, escape_string, syntax};

/// Writes `,\n    "ptz_presets": "..."`; an absent document is omitted.
pub(crate) fn write_ptz_presets(output: &mut String, project: &Project) {
    let Some(document) = project.ptz_presets_document() else {
        return;
    };
    output.push_str(",\n    \"ptz_presets\": \"");
    escape_string(output, document);
    output.push('"');
}

/// Parses `"ptz_presets"` from the project object.
pub(crate) fn parse_ptz_presets(object: &mut Object) -> Result<Option<String>, DecodeError> {
    let Some(value) = object.take_optional("ptz_presets") else {
        return Ok(None);
    };
    let Value::String(document) = value else {
        return Err(syntax("ptz_presets must be a string"));
    };
    let inputs = fm_ptz::PresetVirtualInputs::from_document(&document)
        .map_err(|error| syntax(format!("ptz_presets: {error}")))?;
    if inputs.to_document() != document {
        return Err(syntax("ptz_presets document is not canonical"));
    }
    Ok(Some(document))
}
