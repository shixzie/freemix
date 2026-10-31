//! Title preview-state documents in `project.json` (GX-005, schema 106).
//!
//! ```text
//! "previews": [
//!   {"input": 7, "document": "freemix-data-preview 1\npolicy require_complete\n..."}
//! ]
//! ```
//!
//! Each document is the strict, versioned staged-binding text form; it is
//! required to be canonical. Entries are written in strictly ascending
//! input order. Whether an entry names a title input is reported by project
//! validation.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use fm_model::Project;
use fm_types::InputId;

use super::{DecodeError, Object, escape_string, parse_array, syntax};

/// Writes `,\n    "previews": [...]`; an empty map is omitted.
pub(crate) fn write_previews(output: &mut String, project: &Project) {
    let documents = project.preview_documents();
    if documents.is_empty() {
        return;
    }
    output.push_str(",\n    \"previews\": [");
    for (index, (input, document)) in documents.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"input\": {input}, \"document\": \"").expect("writing to a string cannot fail");
        escape_string(output, document);
        output.push_str("\"}");
    }
    output.push_str("\n    ]");
}

/// Parses `"previews"` from the project object; an absent map is empty.
pub(crate) fn parse_previews(
    object: &mut Object,
) -> Result<BTreeMap<InputId, String>, DecodeError> {
    let Some(value) = object.take_optional("previews") else {
        return Ok(BTreeMap::new());
    };
    let entries = parse_array(value, "previews", |value| {
        let mut entry = Object::new(value, "preview")?;
        let input = InputId::new(entry.nonzero_u128("input")?);
        let document = entry.string("document")?;
        entry.finish()?;
        let snapshot = fm_data::PreviewSnapshot::from_document(&document)
            .map_err(|error| syntax(format!("preview document: {error}")))?;
        if snapshot.to_document() != document {
            return Err(syntax("preview document is not canonical"));
        }
        Ok((input, document))
    })?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "previews entries must be in strictly ascending input order",
        ));
    }
    Ok(entries.into_iter().collect())
}
