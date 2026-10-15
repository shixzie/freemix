//! Input categories and lifecycle in `project.json` (schema 37, IN-026).
//!
//! ```text
//! "input_categories": [{"id": 1, "name": "Cameras"}],
//! "input_lifecycle": [
//!   {"input": 3, "category": 1 | null, "color": "red" | null,
//!    "paused": false, "closed": true}
//! ]
//! ```
//!
//! Categories keep their display order. Lifecycle entries are written in
//! ascending input order and only for inputs that differ from the default;
//! an all-default entry is refused. Bounds and references are reported by
//! project validation.

use core::fmt::Write as _;

use fm_model::{InputCategory, InputCategoryId, InputColorLabel, InputLifecycle, Project};
use fm_types::InputId;

use super::{DecodeError, Object, Value, parse_array, parse_optional, syntax, unknown_enum};

/// Writes `,\n    "input_categories": [...],\n    "input_lifecycle": [...]`.
pub(crate) fn write_input_lifecycle(output: &mut String, project: &Project) {
    output.push_str(",\n    \"input_categories\": [");
    for (index, category) in project.input_categories().iter().enumerate() {
        output.push_str(if index == 0 { "" } else { ", " });
        write!(output, "{{\"id\": {}, \"name\": \"", category.id)
            .expect("writing to a string cannot fail");
        super::escape_string(output, &category.name);
        output.push_str("\"}");
    }
    output.push_str("],\n    \"input_lifecycle\": [");
    for (index, (input, lifecycle)) in project.input_lifecycles().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"input\": {input}, \"category\": ")
            .expect("writing to a string cannot fail");
        match lifecycle.category {
            Some(category) => write!(output, "{category}"),
            None => write!(output, "null"),
        }
        .expect("writing to a string cannot fail");
        output.push_str(", \"color\": ");
        match lifecycle.color {
            Some(color) => write!(output, "\"{}\"", color.token()),
            None => write!(output, "null"),
        }
        .expect("writing to a string cannot fail");
        write!(
            output,
            ", \"paused\": {}, \"closed\": {}}}",
            lifecycle.paused, lifecycle.closed
        )
        .expect("writing to a string cannot fail");
    }
    if !project.input_lifecycles().is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

/// The parsed category list and lifecycle entries.
pub(crate) struct ParsedInputLifecycle {
    pub(crate) categories: Vec<InputCategory>,
    pub(crate) entries: Vec<(InputId, InputLifecycle)>,
}

fn category_id(value: u32) -> Result<InputCategoryId, DecodeError> {
    InputCategoryId::new(value).ok_or_else(|| syntax("input category id must be nonzero"))
}

/// Parses `"input_categories"` and `"input_lifecycle"` from the project
/// object.
pub(crate) fn parse_input_lifecycle(
    object: &mut Object,
) -> Result<ParsedInputLifecycle, DecodeError> {
    let categories = parse_array(
        object.take("input_categories")?,
        "input_categories",
        |value| {
            let mut category = Object::new(value, "input category")?;
            let id = category_id(category.u32("id")?)?;
            let name = category.string("name")?;
            category.finish()?;
            Ok(InputCategory::new(id, name))
        },
    )?;
    let entries = parse_array(
        object.take("input_lifecycle")?,
        "input_lifecycle",
        |value| {
            let mut entry = Object::new(value, "input lifecycle")?;
            let input = InputId::new(entry.nonzero_u128("input")?);
            let category = parse_optional(entry.take("category")?, |value| {
                let Value::Number(number) = value else {
                    return Err(syntax(
                        "field `category` must be an unsigned integer or null",
                    ));
                };
                category_id(
                    u32::try_from(number).map_err(|_| syntax("field `category` exceeds u32"))?,
                )
            })?;
            let color = parse_optional(entry.take("color")?, |value| {
                let Value::String(token) = value else {
                    return Err(syntax("field `color` must be a string or null"));
                };
                InputColorLabel::from_token(&token)
                    .ok_or_else(|| unknown_enum("input color", &token))
            })?;
            let lifecycle = InputLifecycle {
                category,
                color,
                paused: entry.boolean("paused")?,
                closed: entry.boolean("closed")?,
            };
            entry.finish()?;
            if lifecycle.is_default() {
                return Err(syntax(format!(
                    "input_lifecycle entry for input {input} holds only defaults"
                )));
            }
            Ok((input, lifecycle))
        },
    )?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "input_lifecycle entries must be in strictly ascending input order",
        ));
    }
    Ok(ParsedInputLifecycle {
        categories,
        entries,
    })
}
