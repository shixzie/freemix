//! Image and slideshow inputs in `project.json` (schema 60, IN-008,
//! IN-009).
//!
//! ```text
//! {"type": "image", "asset_uri": "asset://logo.png", "fit": "fit|fill|stretch|1:1"}
//! {"type": "slideshow",
//!  "source": {"folder": "asset://photos"} | {"items": [{"asset_uri": "asset://a.jpg",
//!                                                       "caption": null | "..."}]},
//!  "slide_frames": 150, "transition": "cut|fade", "transition_frames": 30,
//!  "ken_burns": false, "order": "ordered" | {"random": 42}, "loop": true,
//!  "auto_advance": true, "fit": "fit|fill|stretch|1:1"}
//! ```
//!
//! Every field is required; bounds are those of
//! [`fm_types::still::ImageInputConfig::validate`] and
//! [`fm_types::still::SlideshowConfig::validate`].

use core::fmt::Write as _;

use fm_model::InputKind;
use fm_types::OutputScaling;
use fm_types::still::{
    ImageInputConfig, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem,
    SlideshowSource,
};

use super::{DecodeError, Object, Value, parse_array, syntax, unknown_enum};

fn write_string(output: &mut String, value: &str) {
    output.push('"');
    super::escape_string(output, value);
    output.push('"');
}

/// Writes `{"type": "image", ...}`.
pub(crate) fn write_image_input(output: &mut String, config: &ImageInputConfig) {
    output.push_str("{\"type\": \"image\", \"asset_uri\": ");
    write_string(output, &config.asset_uri);
    write!(output, ", \"fit\": \"{}\"}}", config.fit.as_str())
        .expect("writing to a string cannot fail");
}

/// Writes `{"type": "slideshow", ...}`.
pub(crate) fn write_slideshow_input(output: &mut String, config: &SlideshowConfig) {
    output.push_str("{\"type\": \"slideshow\", \"source\": ");
    match &config.source {
        SlideshowSource::Folder { asset_uri } => {
            output.push_str("{\"folder\": ");
            write_string(output, asset_uri);
            output.push('}');
        }
        SlideshowSource::List(items) => {
            output.push_str("{\"items\": [");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                output.push_str("{\"asset_uri\": ");
                write_string(output, &item.asset_uri);
                output.push_str(", \"caption\": ");
                match &item.caption {
                    Some(caption) => write_string(output, caption),
                    None => output.push_str("null"),
                }
                output.push('}');
            }
            output.push_str("]}");
        }
    }
    let order = match config.order {
        SlideOrder::Ordered => "\"ordered\"".to_owned(),
        SlideOrder::Random { seed } => format!("{{\"random\": {seed}}}"),
    };
    write!(
        output,
        ", \"slide_frames\": {}, \"transition\": \"{}\", \"transition_frames\": {}, \
         \"ken_burns\": {}, \"order\": {order}, \"loop\": {}, \"auto_advance\": {}, \
         \"fit\": \"{}\"}}",
        config.slide_frames,
        config.transition.token(),
        config.transition_frames,
        config.ken_burns,
        config.loop_show,
        config.auto_advance,
        config.fit.as_str(),
    )
    .expect("writing to a string cannot fail");
}

fn parse_fit(object: &mut Object) -> Result<OutputScaling, DecodeError> {
    let token = object.string("fit")?;
    match token.as_str() {
        "fit" | "fill" | "stretch" | "1:1" => {
            token.parse().map_err(|_| unknown_enum("fit", &token))
        }
        _ => Err(unknown_enum("fit", &token)),
    }
}

/// Parses the fields of an `image` input kind after its `type`.
pub(crate) fn parse_image_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let config = ImageInputConfig::new(object.string("asset_uri")?, parse_fit(object)?);
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(InputKind::Image(config))
}

fn parse_item(value: Value) -> Result<SlideshowItem, DecodeError> {
    let mut object = Object::new(value, "slideshow item")?;
    let asset_uri = object.string("asset_uri")?;
    let caption = match object.take("caption")? {
        Value::Null => None,
        Value::String(caption) => Some(caption),
        _ => return Err(syntax("field `caption` must be a string or null")),
    };
    object.finish()?;
    Ok(SlideshowItem { asset_uri, caption })
}

fn parse_source(value: Value) -> Result<SlideshowSource, DecodeError> {
    let mut object = Object::new(value, "slideshow source")?;
    let source = if let Ok(folder) = object.take("folder") {
        let Value::String(asset_uri) = folder else {
            return Err(syntax("field `folder` must be a string"));
        };
        SlideshowSource::Folder { asset_uri }
    } else {
        SlideshowSource::List(parse_array(
            object.take("items")?,
            "slideshow items",
            parse_item,
        )?)
    };
    object.finish()?;
    Ok(source)
}

fn parse_order(value: Value) -> Result<SlideOrder, DecodeError> {
    match value {
        Value::String(token) if token == "ordered" => Ok(SlideOrder::Ordered),
        Value::String(token) => Err(unknown_enum("slideshow order", &token)),
        value => {
            let mut object = Object::new(value, "slideshow order")?;
            let seed = object.u64("random")?;
            object.finish()?;
            Ok(SlideOrder::Random { seed })
        }
    }
}

/// Parses the fields of a `slideshow` input kind after its `type`.
pub(crate) fn parse_slideshow_input(object: &mut Object) -> Result<InputKind, DecodeError> {
    let source = parse_source(object.take("source")?)?;
    let slide_frames = object.u32("slide_frames")?;
    let transition_token = object.string("transition")?;
    let transition = SlideTransitionStyle::from_token(&transition_token)
        .ok_or_else(|| unknown_enum("slide transition", &transition_token))?;
    let config = SlideshowConfig {
        source,
        slide_frames,
        transition,
        transition_frames: object.u32("transition_frames")?,
        ken_burns: object.boolean("ken_burns")?,
        order: parse_order(object.take("order")?)?,
        loop_show: object.boolean("loop")?,
        auto_advance: object.boolean("auto_advance")?,
        fit: parse_fit(object)?,
    };
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(InputKind::Slideshow(config))
}
