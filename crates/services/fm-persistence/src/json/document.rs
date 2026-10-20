//! A document input in `project.json` (IN-014).
//!
//! ```text
//! {"type": "document", "asset_uri": "asset://<key>", "page": <n>}
//! ```
//!
//! Both fields are required. `asset_uri` is a strict project asset URI
//! naming a `.pdf`, `.pptx`, `.odp`, or `.key` file, and `page` is the
//! zero-based page on screen, below [`fm_types::paged::MAX_PAGED_PAGES`].

use fm_model::{DocumentInput, InputKind};

use super::{DecodeError, Object, syntax};

/// Writes `{"type": "document", "asset_uri": ..., "page": ...}`.
pub(crate) fn write_document_input(output: &mut String, document: &DocumentInput) {
    output.push_str("{\"type\": \"document\", \"asset_uri\": \"");
    super::escape_string(output, &document.asset_uri);
    output.push_str("\", \"page\": ");
    output.push_str(&document.page.to_string());
    output.push('}');
}

/// Parses the fields of a `document` input kind after its `type`.
pub(crate) fn parse_document_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let asset_uri = object.string("asset_uri")?;
    let page = u32::try_from(object.u64("page")?)
        .ok()
        .filter(|page| *page < fm_types::paged::MAX_PAGED_PAGES)
        .ok_or_else(|| syntax("document page is out of range"))?;
    let source = fm_model::InputSourceSpec::Document {
        asset_uri: asset_uri.clone(),
    };
    source
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(InputKind::Document(DocumentInput::new(asset_uri, page)))
}
