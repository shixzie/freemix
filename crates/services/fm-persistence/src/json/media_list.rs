//! Media list inputs in `project.json` (IN-006).
//!
//! ```text
//! "media_lists": [
//!   {"input": 7, "active": 2, "cycle": 0, "auto_next": true,
//!    "auto_first": false, "loop": false, "shuffle_seed": 42 | null,
//!    "items": [{"id": 1, "uri": "asset://media/a.mp4", "title": "Intro" | null,
//!               "mark_in": 0, "mark_out": 250 | null}]}
//! ]
//! ```
//!
//! Lists are written in strictly ascending input order, items in authored
//! order. Bounds, the active item, and whether the input plays it are
//! reported by project validation.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use fm_model::{MediaList, MediaListItem, MediaListOptions, Project};
use fm_types::InputId;

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

fn write_optional_number(output: &mut String, value: Option<u64>) {
    match value {
        Some(value) => write!(output, "{value}"),
        None => write!(output, "null"),
    }
    .expect("writing to a string cannot fail");
}

/// Writes `,\n    "media_lists": [...]`.
pub(crate) fn write_media_lists(output: &mut String, project: &Project) {
    output.push_str(",\n    \"media_lists\": [");
    for (index, (input, list)) in project.media_lists().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(
            output,
            "{{\"input\": {input}, \"active\": {}, \"cycle\": {}, \"auto_next\": {}, \
             \"auto_first\": {}, \"loop\": {}, \"shuffle_seed\": ",
            list.active,
            list.cycle,
            list.options.auto_next,
            list.options.auto_first,
            list.options.loop_list
        )
        .expect("writing to a string cannot fail");
        write_optional_number(output, list.options.shuffle_seed);
        output.push_str(", \"items\": [");
        for (position, item) in list.items.iter().enumerate() {
            if position != 0 {
                output.push_str(", ");
            }
            write!(output, "{{\"id\": {}, \"uri\": \"", item.id)
                .expect("writing to a string cannot fail");
            escape_string(output, &item.asset_uri);
            output.push_str("\", \"title\": ");
            match &item.title {
                Some(title) => {
                    output.push('"');
                    escape_string(output, title);
                    output.push('"');
                }
                None => output.push_str("null"),
            }
            write!(output, ", \"mark_in\": {}, \"mark_out\": ", item.mark_in)
                .expect("writing to a string cannot fail");
            write_optional_number(output, item.mark_out);
            output.push('}');
        }
        output.push_str("]}");
    }
    if !project.media_lists().is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

fn optional_u64(value: Value, field: &'static str) -> Result<Option<u64>, DecodeError> {
    parse_optional(value, |value| {
        let Value::Number(number) = value else {
            return Err(syntax(format!(
                "field `{field}` must be an unsigned integer or null"
            )));
        };
        u64::try_from(number).map_err(|_| syntax(format!("field `{field}` exceeds u64")))
    })
}

fn parse_item(value: Value) -> Result<MediaListItem, DecodeError> {
    let mut object = Object::new(value, "media list item")?;
    let id = object.u32("id")?;
    let asset_uri = object.string("uri")?;
    let title = parse_optional(object.take("title")?, |value| {
        let Value::String(title) = value else {
            return Err(syntax("field `title` must be a string or null"));
        };
        Ok(title)
    })?;
    let mark_in = object.u64("mark_in")?;
    let mark_out = optional_u64(object.take("mark_out")?, "mark_out")?;
    object.finish()?;
    Ok(MediaListItem {
        id,
        asset_uri,
        title,
        mark_in,
        mark_out,
    })
}

/// Parses `"media_lists"` from the project object.
pub(crate) fn parse_media_lists(
    object: &mut Object,
) -> Result<BTreeMap<InputId, MediaList>, DecodeError> {
    let lists = parse_array(object.take("media_lists")?, "media_lists", |value| {
        let mut entry = Object::new(value, "media list")?;
        let input = InputId::new(entry.nonzero_u128("input")?);
        let active = entry.u32("active")?;
        let cycle = entry.u64("cycle")?;
        let options = MediaListOptions {
            auto_next: entry.boolean("auto_next")?,
            auto_first: entry.boolean("auto_first")?,
            loop_list: entry.boolean("loop")?,
            shuffle_seed: optional_u64(entry.take("shuffle_seed")?, "shuffle_seed")?,
        };
        let items = parse_array(entry.take("items")?, "items", parse_item)?;
        entry.finish()?;
        Ok((
            input,
            MediaList {
                items,
                options,
                active,
                cycle,
            },
        ))
    })?;
    if lists.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "media_lists entries must be in strictly ascending input order",
        ));
    }
    Ok(lists.into_iter().collect())
}
