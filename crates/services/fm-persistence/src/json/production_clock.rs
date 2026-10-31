//! Production clock definitions in `project.json` (SW-013, schema 106).
//!
//! ```text
//! "production_clocks": [
//!   {"name": "Countdown", "kind": "timer", "direction": "count_down", "preset_ns": 300000000000, "limit_ns": null, "overrun": "stop_at_target", "format": {"type": "duration", "preset": "minutes_seconds"}},
//!   {"name": "Local", "kind": "time_of_day", "zone": "EST5EDT,M3.2.0,M11.1.0", "format": {"type": "time_of_day", "preset": "hours_24"}},
//!   {"name": "Media", "kind": "media_remaining", "format": {"type": "duration", "preset": "minutes_seconds"}},
//!   {"name": "Live", "kind": "session", "session": "stream", "format": {"type": "duration", "preset": "hours_minutes_seconds"}}
//! ]
//! ```
//!
//! Definitions are written in definition order and omitted when empty. POSIX
//! zones must be in their canonical text form. Names, values, and formats
//! are validated by [`fm_types::validate_project_clocks`].

use core::fmt::Write as _;

use fm_model::Project;
use fm_types::{
    MAX_PROJECT_CLOCKS, ProjectClock, ProjectClockDirection, ProjectClockKind,
    ProjectClockOverrun, ProjectDurationFormat, ProjectSessionKind, ProjectTimeFormat,
    validate_project_clocks,
};

use super::{DecodeError, Object, Value, parse_array, syntax, unknown_enum};

/// Writes `,\n    "production_clocks": [...]`; an empty list is omitted.
pub(crate) fn write_production_clocks(output: &mut String, project: &Project) {
    let clocks = project.production_clocks();
    if clocks.is_empty() {
        return;
    }
    output.push_str(",\n    \"production_clocks\": [");
    for (index, clock) in clocks.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write_clock(output, clock);
    }
    output.push_str("\n    ]");
}

fn write_clock(output: &mut String, clock: &ProjectClock) {
    write!(output, "{{\"name\": ").expect("writing to a string cannot fail");
    write_string(output, &clock.name);
    match &clock.kind {
        ProjectClockKind::Timer {
            direction,
            preset_nanos,
            limit_nanos,
            overrun,
            format,
        } => {
            write!(
                output,
                ", \"kind\": \"timer\", \"direction\": \"{}\", \"preset_ns\": {preset_nanos}, \"limit_ns\": ",
                direction.token()
            )
            .expect("writing to a string cannot fail");
            match limit_nanos {
                Some(limit) => write!(output, "{limit}").expect("writing to a string cannot fail"),
                None => output.push_str("null"),
            }
            write!(output, ", \"overrun\": \"{}\", \"format\": ", overrun.token())
                .expect("writing to a string cannot fail");
            write_duration_format(output, *format);
        }
        ProjectClockKind::TimeOfDay { zone, format } => {
            output.push_str(", \"kind\": \"time_of_day\", \"zone\": ");
            write_string(output, zone);
            output.push_str(", \"format\": ");
            write_time_format(output, *format);
        }
        ProjectClockKind::MediaRemaining { input, format } => {
            output.push_str(", \"kind\": \"media_remaining\", \"media_input\": ");
            match input {
                Some(input) => write!(output, "{input}").expect("writing to a string cannot fail"),
                None => output.push_str("null"),
            }
            output.push_str(", \"format\": ");
            write_duration_format(output, *format);
        }
        ProjectClockKind::Session { kind, format } => {
            write!(
                output,
                ", \"kind\": \"session\", \"session\": \"{}\", \"format\": ",
                kind.token()
            )
            .expect("writing to a string cannot fail");
            write_duration_format(output, *format);
        }
    }
    output.push('}');
}

fn write_string(output: &mut String, value: &str) {
    output.push('"');
    super::escape_string(output, value);
    output.push('"');
}

fn write_duration_format(output: &mut String, format: ProjectDurationFormat) {
    match format {
        ProjectDurationFormat::HoursMinutesSecondsFrames {
            numerator,
            denominator,
        } => write!(
            output,
            "{{\"type\": \"duration\", \"preset\": \"hours_minutes_seconds_frames\", \"numerator\": {numerator}, \"denominator\": {denominator}}}"
        )
        .expect("writing to a string cannot fail"),
        format => write!(
            output,
            "{{\"type\": \"duration\", \"preset\": \"{}\"}}",
            format.token()
        )
        .expect("writing to a string cannot fail"),
    }
}

fn write_time_format(output: &mut String, format: ProjectTimeFormat) {
    write!(
        output,
        "{{\"type\": \"time_of_day\", \"preset\": \"{}\"}}",
        format.token()
    )
    .expect("writing to a string cannot fail");
}

/// Parses `"production_clocks"` from the project object; an absent list is
/// empty.
pub(crate) fn parse_production_clocks(
    object: &mut Object,
) -> Result<Vec<ProjectClock>, DecodeError> {
    let Some(value) = object.take_optional("production_clocks") else {
        return Ok(Vec::new());
    };
    let clocks = parse_array(value, "production_clocks", parse_clock)?;
    if clocks.len() > MAX_PROJECT_CLOCKS {
        return Err(syntax(format!(
            "production_clocks has {}, maximum is {MAX_PROJECT_CLOCKS}",
            clocks.len()
        )));
    }
    validate_project_clocks(&clocks).map_err(|error| syntax(error.to_string()))?;
    Ok(clocks)
}

fn parse_clock(value: Value) -> Result<ProjectClock, DecodeError> {
    let mut clock = Object::new(value, "production clock")?;
    let name = clock.string("name")?;
    let kind = match clock.string("kind")?.as_str() {
        "timer" => {
            let direction_text = clock.string("direction")?;
            let direction = ProjectClockDirection::from_token(&direction_text)
                .ok_or_else(|| unknown_enum("clock direction", &direction_text))?;
            let preset_nanos = clock.i64("preset_ns")?;
            let limit_nanos = match clock.take("limit_ns")? {
                Value::Null => None,
                Value::Number(value) => Some(
                    i64::try_from(value)
                        .map_err(|_| syntax("clock limit_ns must fit a signed 64-bit integer"))?,
                ),
                Value::NegativeNumber(value) => Some(
                    i64::try_from(value)
                        .map_err(|_| syntax("clock limit_ns must fit a signed 64-bit integer"))?
                        .saturating_neg(),
                ),
                _ => return Err(syntax("clock limit_ns must be a number or null")),
            };
            let overrun_text = clock.string("overrun")?;
            let overrun = ProjectClockOverrun::from_token(&overrun_text)
                .ok_or_else(|| unknown_enum("clock overrun", &overrun_text))?;
            let format = parse_duration_format(clock.take("format")?)?;
            ProjectClockKind::Timer {
                direction,
                preset_nanos,
                limit_nanos,
                overrun,
                format,
            }
        }
        "time_of_day" => {
            let text = clock.string("zone")?;
            let zone = fm_clock::production::TimeZone::parse_posix(&text)
                .map_err(|error| syntax(error.to_string()))?;
            if zone.to_string() != text {
                return Err(syntax(format!(
                    "zone `{text}` is not in its canonical POSIX form `{zone}`"
                )));
            }
            let format = parse_time_format(clock.take("format")?)?;
            ProjectClockKind::TimeOfDay { zone: text, format }
        }
        "media_remaining" => {
            let input = match clock.take("media_input")? {
                Value::Null => None,
                Value::Number(value) => Some(fm_types::InputId::new(
                    core::num::NonZeroU128::new(value)
                        .ok_or_else(|| syntax("media_input must be a nonzero id"))?,
                )),
                _ => return Err(syntax("media_input must be a nonzero id or null")),
            };
            ProjectClockKind::MediaRemaining {
                input,
                format: parse_duration_format(clock.take("format")?)?,
            }
        }
        "session" => {
            let session_text = clock.string("session")?;
            let kind = ProjectSessionKind::from_token(&session_text)
                .ok_or_else(|| unknown_enum("session kind", &session_text))?;
            ProjectClockKind::Session {
                kind,
                format: parse_duration_format(clock.take("format")?)?,
            }
        }
        value => return Err(unknown_enum("clock kind", value)),
    };
    clock.finish()?;
    Ok(ProjectClock { name, kind })
}

fn parse_duration_format(value: Value) -> Result<ProjectDurationFormat, DecodeError> {
    let mut format = Object::new(value, "clock format")?;
    if format.string("type")? != "duration" {
        return Err(syntax("this clock needs a duration format"));
    }
    let preset = format.string("preset")?;
    let parsed = match preset.as_str() {
        "hours_minutes_seconds_frames" => ProjectDurationFormat::HoursMinutesSecondsFrames {
            numerator: format.u32("numerator")?,
            denominator: format.u32("denominator")?,
        },
        value => ProjectDurationFormat::from_token(value)
            .ok_or_else(|| unknown_enum("duration preset", value))?,
    };
    format.finish()?;
    Ok(parsed)
}

fn parse_time_format(value: Value) -> Result<ProjectTimeFormat, DecodeError> {
    let mut format = Object::new(value, "clock format")?;
    if format.string("type")? != "time_of_day" {
        return Err(syntax("this clock needs a time_of_day format"));
    }
    let preset = format.string("preset")?;
    let parsed =
        ProjectTimeFormat::from_token(&preset).ok_or_else(|| unknown_enum("time preset", &preset))?;
    format.finish()?;
    Ok(parsed)
}
