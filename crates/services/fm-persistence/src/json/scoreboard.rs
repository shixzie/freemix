//! Title scoreboard documents in `project.json` (GX-003, schema 106).
//!
//! ```text
//! "scoreboards": [
//!   {"input": 7, "document": "freemix-scoreboard 1\nrules 99 4 24000\n..."}
//! ]
//! ```
//!
//! Each document is the strict, versioned scoreboard text form; it is
//! required to be canonical, so an import cannot smuggle a different byte
//! form of the same state. Entries are written in strictly ascending input
//! order. Whether an entry names a title input is reported by project
//! validation.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use fm_model::Project;
use fm_types::InputId;

use super::{DecodeError, Object, escape_string, parse_array, syntax};

/// Writes `,\n    "scoreboards": [...]`; an empty map is omitted.
pub(crate) fn write_scoreboards(output: &mut String, project: &Project) {
    let documents = project.scoreboard_documents();
    if documents.is_empty() {
        return;
    }
    output.push_str(",\n    \"scoreboards\": [");
    for (index, (input, document)) in documents.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"input\": {input}, \"document\": \"").expect("writing to a string cannot fail");
        escape_string(output, document);
        output.push_str("\"}");
    }
    output.push_str("\n    ]");
}

/// Parses `"scoreboards"` from the project object; an absent map is empty.
pub(crate) fn parse_scoreboards(
    object: &mut Object,
) -> Result<BTreeMap<InputId, String>, DecodeError> {
    let Some(value) = object.take_optional("scoreboards") else {
        return Ok(BTreeMap::new());
    };
    let entries = parse_array(value, "scoreboards", |value| {
        let mut entry = Object::new(value, "scoreboard")?;
        let input = InputId::new(entry.nonzero_u128("input")?);
        let document = entry.string("document")?;
        entry.finish()?;
        let snapshot = fm_titles::ScoreboardSnapshot::from_document(&document)
            .map_err(|error| syntax(format!("scoreboard document: {error}")))?;
        if snapshot.to_document() != document {
            return Err(syntax("scoreboard document is not canonical"));
        }
        Ok((input, document))
    })?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "scoreboards entries must be in strictly ascending input order",
        ));
    }
    Ok(entries.into_iter().collect())
}
