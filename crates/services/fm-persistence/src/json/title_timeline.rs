//! Title timelines in `project.json` (GX-001, schema 66).
//!
//! ```text
//! "title_timelines": [
//!   {"input": 7, "on": "air" | "preview", "since_frame": 1200}
//! ]
//! ```
//!
//! Entries are written in strictly ascending input order. Whether an entry
//! names a title input is reported by project validation; whether its frame
//! is reachable is checked when an engine restores it.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use fm_model::{Project, TitleShownOn, TitleTimeline};
use fm_types::InputId;

use super::{DecodeError, Object, parse_array, syntax};

const fn shown_token(on: TitleShownOn) -> &'static str {
    match on {
        TitleShownOn::Air => "air",
        TitleShownOn::Preview => "preview",
    }
}

/// Writes `,\n    "title_timelines": [...]`.
pub(crate) fn write_title_timelines(output: &mut String, project: &Project) {
    output.push_str(",\n    \"title_timelines\": [");
    for (index, (input, timeline)) in project.title_timelines().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(
            output,
            "{{\"input\": {input}, \"on\": \"{}\", \"since_frame\": {}}}",
            shown_token(timeline.on),
            timeline.since_frame
        )
        .expect("writing to a string cannot fail");
    }
    if !project.title_timelines().is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

/// Parses `"title_timelines"` from the project object.
pub(crate) fn parse_title_timelines(
    object: &mut Object,
) -> Result<BTreeMap<InputId, TitleTimeline>, DecodeError> {
    let entries = parse_array(
        object.take("title_timelines")?,
        "title_timelines",
        |value| {
            let mut entry = Object::new(value, "title timeline")?;
            let input = InputId::new(entry.nonzero_u128("input")?);
            let on = match entry.string("on")?.as_str() {
                "air" => TitleShownOn::Air,
                "preview" => TitleShownOn::Preview,
                _ => return Err(syntax("title timeline `on` must be \"air\" or \"preview\"")),
            };
            let since_frame = entry.u64("since_frame")?;
            entry.finish()?;
            Ok((input, TitleTimeline { on, since_frame }))
        },
    )?;
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax(
            "title_timelines entries must be in strictly ascending input order",
        ));
    }
    Ok(entries.into_iter().collect())
}
