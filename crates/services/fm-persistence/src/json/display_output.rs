//! Fullscreen display outputs in `project.json` (schema 40, OR-011).
//!
//! ```text
//! "display_outputs": [
//!   {"slot": 1, "display": {"id": "cg:69733382", "name": "DELL U2720Q"},
//!    "source": "program", "mode": "current", "scaling": "fit", "hdr": "off"},
//!   {"slot": 2, "display": {"index": 1}, "source": "output:5",
//!    "mode": {"width": 1920, "height": 1080, "refresh_millihertz": 60000},
//!    "scaling": "1:1", "hdr": "auto"}
//! ]
//! ```
//!
//! At most two entries, strictly ascending by slot (`1..=2`). `display`
//! holds exactly one of `id` (optionally with `name`, the fallback when the
//! platform re-numbers the display), `name`, or `index`. `source` is
//! `program`, `preview`, or `output:<id>`; `mode` is `"current"` or an exact
//! raster with a refresh in millihertz (zero when unspecified); `scaling` is
//! `fit`, `fill`, `stretch`, or `1:1`; `hdr` is `off` or `auto`. Unknown
//! source outputs and two slots claiming one display are reported by
//! project validation.

use core::fmt::Write as _;

use fm_types::{
    OutputScaling,
    display::{
        DisplayMode, DisplayOutputConfig, DisplayOutputSlot, DisplayOutputSource, DisplaySelector,
        HdrRequest, MAX_DISPLAY_OUTPUTS, ModeRequest,
    },
};

use super::{DecodeError, Object, Value, escape_string, parse_array, syntax};

/// Writes `,\n    "display_outputs": [...]`.
pub(crate) fn write_display_outputs(output: &mut String, project: &fm_model::Project) {
    output.push_str(",\n    \"display_outputs\": [");
    for (index, (slot, config)) in project.display_outputs().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"slot\": {}, \"display\": {{", slot.get())
            .expect("writing to a string cannot fail");
        let mut first = true;
        let mut field = |output: &mut String, key: &str| {
            if !first {
                output.push_str(", ");
            }
            first = false;
            write!(output, "\"{key}\": ").expect("writing to a string cannot fail");
        };
        if let Some(id) = config.display.id() {
            field(output, "id");
            output.push('"');
            escape_string(output, id.as_str());
            output.push('"');
        }
        if let Some(name) = config.display.name() {
            field(output, "name");
            output.push('"');
            escape_string(output, name);
            output.push('"');
        }
        if let Some(index) = config.display.index() {
            field(output, "index");
            write!(output, "{index}").expect("writing to a string cannot fail");
        }
        output.push_str("}, \"source\": \"");
        escape_string(output, &config.source.to_string());
        output.push_str("\", \"mode\": ");
        match config.mode {
            ModeRequest::Current => output.push_str("\"current\""),
            ModeRequest::Exact(mode) => write!(
                output,
                "{{\"width\": {}, \"height\": {}, \"refresh_millihertz\": {}}}",
                mode.width, mode.height, mode.refresh_millihertz
            )
            .expect("writing to a string cannot fail"),
        }
        write!(
            output,
            ", \"scaling\": \"{}\", \"hdr\": \"{}\"}}",
            config.scaling, config.hdr
        )
        .expect("writing to a string cannot fail");
    }
    if project.display_outputs().is_empty() {
        output.push(']');
    } else {
        output.push_str("\n    ]");
    }
}

/// Parses `"display_outputs"` from the project object.
pub(crate) fn parse_display_outputs(
    object: &mut Object,
) -> Result<Vec<(DisplayOutputSlot, DisplayOutputConfig)>, DecodeError> {
    let entries = parse_array(
        object.take("display_outputs")?,
        "display_outputs",
        parse_entry,
    )?;
    if entries.len() > MAX_DISPLAY_OUTPUTS {
        return Err(syntax(format!(
            "display_outputs must hold at most {MAX_DISPLAY_OUTPUTS} entries"
        )));
    }
    if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(syntax("display_outputs must be strictly ascending by slot"));
    }
    Ok(entries)
}

fn parse_entry(value: Value) -> Result<(DisplayOutputSlot, DisplayOutputConfig), DecodeError> {
    let mut object = Object::new(value, "display output")?;
    let slot = DisplayOutputSlot::new(object.u8("slot")?).ok_or_else(|| {
        syntax(format!(
            "display output slot must be 1 through {MAX_DISPLAY_OUTPUTS}"
        ))
    })?;
    let display = parse_display(object.take("display")?)?;
    let source = object.string("source")?;
    let source = source
        .parse::<DisplayOutputSource>()
        .map_err(|error| syntax(error.to_string()))?;
    let mode = match object.take("mode")? {
        Value::String(mode) if mode == "current" => ModeRequest::Current,
        value @ Value::Object(_) => {
            let mut mode = Object::new(value, "display output mode")?;
            let parsed = DisplayMode::new(
                mode.u32("width")?,
                mode.u32("height")?,
                mode.u32("refresh_millihertz")?,
            );
            mode.finish()?;
            ModeRequest::Exact(parsed)
        }
        _ => {
            return Err(syntax(
                "display output mode must be \"current\" or an exact mode object",
            ));
        }
    };
    let scaling = object.string("scaling")?;
    let scaling = scaling
        .parse::<OutputScaling>()
        .map_err(|error| syntax(error.to_string()))?;
    let hdr = match object.string("hdr")?.as_str() {
        "off" => HdrRequest::Off,
        "auto" => HdrRequest::Auto,
        other => return Err(super::unknown_enum("hdr", other)),
    };
    object.finish()?;
    Ok((
        slot,
        DisplayOutputConfig {
            display,
            source,
            mode,
            scaling,
            hdr,
        },
    ))
}

fn parse_display(value: Value) -> Result<DisplaySelector, DecodeError> {
    let Value::Object(mut fields) = value else {
        return Err(syntax("display output display must be an object"));
    };
    let mut text = |key: &str| -> Result<Option<String>, DecodeError> {
        match fields.remove(key) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(syntax(format!(
                "display output display.{key} must be a string"
            ))),
        }
    };
    let id = text("id")?;
    let name = text("name")?;
    let index = match fields.remove("index") {
        None => None,
        Some(Value::Number(index)) => Some(
            usize::try_from(index)
                .map_err(|_| syntax("display output display.index is out of range"))?,
        ),
        Some(_) => {
            return Err(syntax(
                "display output display.index must be an unsigned integer",
            ));
        }
    };
    if let Some(key) = fields.keys().next() {
        return Err(syntax(format!(
            "unknown field `{key}` in display output display"
        )));
    }
    // Text bounds and the index range are refused here; conflicts with the
    // other slot are reported by project validation.
    DisplaySelector::from_parts(id.as_deref(), name.as_deref(), index)
        .map_err(|error| syntax(error.to_string()))
}
