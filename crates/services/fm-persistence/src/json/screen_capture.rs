//! A screen capture input in `project.json` (IN-011).
//!
//! ```text
//! {"type": "screen_capture",
//!  "target": {"kind": "screen", "index": 0}
//!          | {"kind": "window", "app": "XTerm", "title": "build",
//!             "title_match": "exact|contains|any", "window_id": null | 41943047},
//!  "crop": {"left": 0, "top": 0, "right": 0, "bottom": 0},
//!  "cursor": true, "fps": 30, "stall": "freeze|black"}
//! ```
//!
//! Every field is required; bounds are those of
//! [`fm_types::screen_capture::ScreenCaptureConfig::validate`].

use core::fmt::Write as _;

use fm_model::InputKind;
use fm_types::network_stream::StallSlate;
use fm_types::screen_capture::{
    ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureTarget, TitleMatch, WindowRule,
};

use super::{DecodeError, Object, Value, syntax, unknown_enum};

/// Writes `{"type": "screen_capture", ...}`.
pub(crate) fn write_screen_capture_input(output: &mut String, config: &ScreenCaptureConfig) {
    output.push_str("{\"type\": \"screen_capture\", \"target\": ");
    match &config.target {
        ScreenCaptureTarget::Screen { index } => {
            write!(output, "{{\"kind\": \"screen\", \"index\": {index}}}")
                .expect("writing to a string cannot fail");
        }
        ScreenCaptureTarget::Window(rule) => {
            output.push_str("{\"kind\": \"window\", \"app\": \"");
            super::escape_string(output, &rule.app);
            output.push_str("\", \"title\": \"");
            super::escape_string(output, &rule.title);
            write!(
                output,
                "\", \"title_match\": \"{}\", \"window_id\": ",
                rule.title_match.token()
            )
            .expect("writing to a string cannot fail");
            match rule.window_id {
                Some(id) => write!(output, "{id}").expect("writing to a string cannot fail"),
                None => output.push_str("null"),
            }
            output.push('}');
        }
    }
    let crop = config.crop;
    write!(
        output,
        ", \"crop\": {{\"left\": {}, \"top\": {}, \"right\": {}, \"bottom\": {}}}, \
         \"cursor\": {}, \"fps\": {}, \"stall\": \"{}\"}}",
        crop.left,
        crop.top,
        crop.right,
        crop.bottom,
        config.cursor,
        config.fps,
        config.slate.token(),
    )
    .expect("writing to a string cannot fail");
}

/// Parses the fields of a `screen_capture` input kind after its `type`.
pub(crate) fn parse_screen_capture_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let mut target = Object::new(object.take("target")?, "screen capture target")?;
    let target = match target.string("kind")?.as_str() {
        "screen" => {
            let index = target.u16("index")?;
            target.finish()?;
            ScreenCaptureTarget::Screen { index }
        }
        "window" => {
            let app = target.string("app")?;
            let title = target.string("title")?;
            let title_match = {
                let token = target.string("title_match")?;
                TitleMatch::from_token(&token)
                    .ok_or_else(|| unknown_enum("screen capture title match", &token))?
            };
            let window_id = match target.take("window_id")? {
                Value::Null => None,
                Value::Number(id) => {
                    Some(u64::try_from(id).map_err(|_| syntax("field `window_id` exceeds u64"))?)
                }
                _ => return Err(syntax("field `window_id` must be null or an integer")),
            };
            target.finish()?;
            ScreenCaptureTarget::Window(WindowRule {
                app,
                title,
                title_match,
                window_id,
            })
        }
        other => return Err(unknown_enum("screen capture target", other)),
    };
    let mut crop = Object::new(object.take("crop")?, "screen capture crop")?;
    let crop_value = ScreenCaptureCrop {
        left: crop.u16("left")?,
        top: crop.u16("top")?,
        right: crop.u16("right")?,
        bottom: crop.u16("bottom")?,
    };
    crop.finish()?;
    let cursor = object.boolean("cursor")?;
    let fps = object.u8("fps")?;
    let slate = {
        let token = object.string("stall")?;
        StallSlate::from_token(&token)
            .ok_or_else(|| unknown_enum("screen capture stall", &token))?
    };
    let config = ScreenCaptureConfig {
        target,
        crop: crop_value,
        cursor,
        fps,
        slate,
    };
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(InputKind::ScreenCapture(config))
}
