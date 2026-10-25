//! Replay recording settings in `project.json` (OR-012, schema 82).
//!
//! ```text
//! "replay": {"sources": [3, 5], "retention_seconds": 600,
//!            "storage_root": "/srv/replay"}
//! ```
//!
//! `sources` are the recorded inputs in angle order, `retention_seconds` is
//! how much of each the server keeps, and `storage_root` is `null` (the
//! server chooses) or an absolute directory. The recorded segment files and
//! the replay events are runtime state and never written here. Unknown,
//! repeated, or Replay sources, a retention out of range, and an unusable
//! root are reported by project validation. A Replay input is
//! `{"type": "replay"}`.

use core::fmt::Write as _;

use fm_types::InputId;
use fm_types::replay::ReplaySettings;

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

/// Writes `,\n    "replay": {...}`.
pub(crate) fn write_replay(output: &mut String, project: &fm_model::Project) {
    let settings = project.replay();
    output.push_str(",\n    \"replay\": {\"sources\": [");
    for (index, source) in settings.sources.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        write!(output, "{source}").expect("writing to a string cannot fail");
    }
    write!(
        output,
        "], \"retention_seconds\": {}, \"storage_root\": ",
        settings.retention_seconds
    )
    .expect("writing to a string cannot fail");
    match &settings.storage_root {
        Some(root) => {
            output.push('"');
            escape_string(output, root);
            output.push('"');
        }
        None => output.push_str("null"),
    }
    output.push('}');
}

/// Parses `"replay"` from the project object.
pub(crate) fn parse_replay(object: &mut Object) -> Result<ReplaySettings, DecodeError> {
    let mut replay = Object::new(object.take("replay")?, "replay")?;
    let sources = parse_array(replay.take("sources")?, "replay sources", |value| {
        let Value::Number(value) = value else {
            return Err(syntax("replay sources must be input ids"));
        };
        core::num::NonZeroU128::new(value)
            .map(InputId::new)
            .ok_or_else(|| syntax("replay sources must be nonzero"))
    })?;
    let retention_seconds = replay.u32("retention_seconds")?;
    let storage_root = parse_optional(replay.take("storage_root")?, |value| {
        let Value::String(root) = value else {
            return Err(syntax("replay storage_root must be a string or null"));
        };
        Ok(root)
    })?;
    replay.finish()?;
    Ok(ReplaySettings {
        sources,
        retention_seconds,
        storage_root,
    })
}
