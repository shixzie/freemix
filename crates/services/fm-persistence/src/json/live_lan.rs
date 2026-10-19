//! The `LiveLAN` viewer stream in `project.json` (schema 53, OR-017).
//!
//! ```text
//! "live_lan": {"enabled": true, "source": "output:5", "max_viewers": 8,
//!              "pin": "2468"}
//! ```
//!
//! `source` is `program` or `output:<id>`, `max_viewers` is `1..=32`, and
//! `pin` is `null` or 4 to 12 ASCII digits, stored in the bundle under the
//! same trust model as a stream key. A refused PIN is never echoed. An
//! unknown source output or a viewer limit out of range is reported by
//! project validation.

use core::fmt::Write as _;

use fm_types::live_lan::{LiveLanConfig, LiveLanPin, LiveLanSource};

use super::{DecodeError, Object, Value, escape_string, parse_optional, syntax};

/// Writes `,\n    "live_lan": {...}`.
pub(crate) fn write_live_lan(output: &mut String, project: &fm_model::Project) {
    let config = project.live_lan();
    write!(
        output,
        ",\n    \"live_lan\": {{\"enabled\": {}, \"source\": \"{}\", \"max_viewers\": {}, \"pin\": ",
        config.enabled, config.source, config.max_viewers
    )
    .expect("writing to a string cannot fail");
    match &config.pin {
        Some(pin) => {
            output.push('"');
            escape_string(output, pin.expose_secret());
            output.push('"');
        }
        None => output.push_str("null"),
    }
    output.push('}');
}

/// Parses `"live_lan"` from the project object.
pub(crate) fn parse_live_lan(object: &mut Object) -> Result<LiveLanConfig, DecodeError> {
    let mut live_lan = Object::new(object.take("live_lan")?, "live_lan")?;
    let enabled = live_lan.boolean("enabled")?;
    let source = live_lan
        .string("source")?
        .parse::<LiveLanSource>()
        .map_err(|error| syntax(format!("live_lan source is invalid: {error}")))?;
    let max_viewers = live_lan.u8("max_viewers")?;
    let pin = parse_optional(live_lan.take("pin")?, |value| {
        let Value::String(pin) = value else {
            return Err(syntax("live_lan pin must be a string or null"));
        };
        LiveLanPin::parse(&pin).map_err(|error| syntax(format!("live_lan pin is invalid: {error}")))
    })?;
    live_lan.finish()?;
    Ok(LiveLanConfig {
        enabled,
        source,
        max_viewers,
        pin,
    })
}
