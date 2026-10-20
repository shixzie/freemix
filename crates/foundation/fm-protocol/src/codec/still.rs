//! Image and slideshow input codec (IN-008, IN-009).
//!
//! * Image fields: `uri` and `fit` (`fit|fill|stretch|1:1`).
//! * Slideshow settings, one field: `<slide frames>~<cut|fade>~<transition
//!   frames>~<ken burns 0|1>~<ordered|random.<seed>>~<loop 0|1>~<auto
//!   advance 0|1>~<fit>~<source>`, where the source is
//!   `folder~<length>:<uri>` or `list~<item>,<item>...` and an item is
//!   `<length>:<uri>~-` or `<length>:<uri>~<length>:<caption>`. Texts are
//!   written raw after their canonical byte length, so the line encoding
//!   escapes them exactly once.
//! * Replacement sources: `image:<fit>:<uri>` and `slideshow:<settings>`.
//! * Page status fields: `page`, `count`, `generation`, `label`.
//! * Navigation fields: `action` (`next|previous|first|last|goto`) and,
//!   for `goto` only, `page`.
//! * Listed slides: comma-separated items as above.

use core::fmt::Write as _;

use fm_types::OutputScaling;

use crate::{
    CodecError, ImageInputConfig, MAX_SLIDESHOW_SLIDES_BYTES, MAX_SLIDESHOW_SLIDES_PAGE,
    PagedNavigation, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem,
    SlideshowPage, SlideshowPageStatus, SlideshowSlide, SlideshowSource, StillInputError,
    WireStillSource,
};

use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn still_error(error: StillInputError) -> CodecError {
    invalid(error.field(), error.code())
}

fn canonical<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| T::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

/// The fit token of a scaling policy.
pub(super) const fn fit_token(fit: OutputScaling) -> &'static str {
    fit.as_str()
}

/// The scaling policy of a fit token.
pub(super) fn parse_fit(field: &'static str, value: &str) -> Result<OutputScaling, CodecError> {
    match value {
        "fit" => Ok(OutputScaling::Fit),
        "fill" => Ok(OutputScaling::Fill),
        "stretch" => Ok(OutputScaling::Stretch),
        "1:1" => Ok(OutputScaling::OneToOne),
        _ => Err(invalid(field, value)),
    }
}

fn push_text(output: &mut String, text: &str) {
    write!(output, "{}:{text}", text.len()).expect("writing to a string cannot fail");
}

/// Splits `<length>:<text>` off the front of `rest`.
fn take_text<'a>(field: &'static str, rest: &'a str) -> Result<(&'a str, &'a str), CodecError> {
    let (length, after) = rest.split_once(':').ok_or_else(|| invalid(field, rest))?;
    let length: usize = canonical(field, length)?;
    if length > after.len() || !after.is_char_boundary(length) {
        return Err(invalid(field, rest));
    }
    Ok(after.split_at(length))
}

/// Encodes slideshow settings as one text.
pub(super) fn encode_slideshow(config: &SlideshowConfig) -> Result<String, CodecError> {
    config.validate_request().map_err(still_error)?;
    let mut output = String::new();
    let order = match config.order {
        SlideOrder::Ordered => "ordered".to_owned(),
        SlideOrder::Random { seed } => format!("random.{seed}"),
    };
    write!(
        output,
        "{}~{}~{}~{}~{order}~{}~{}~{}~",
        config.slide_frames,
        config.transition.token(),
        config.transition_frames,
        u8::from(config.ken_burns),
        u8::from(config.loop_show),
        u8::from(config.auto_advance),
        fit_token(config.fit),
    )
    .expect("writing to a string cannot fail");
    match &config.source {
        SlideshowSource::Folder { asset_uri } => {
            output.push_str("folder~");
            push_text(&mut output, asset_uri);
        }
        SlideshowSource::List(items) => {
            output.push_str("list~");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                push_item(&mut output, &item.asset_uri, item.caption.as_deref());
            }
        }
    }
    Ok(output)
}

fn push_item(output: &mut String, uri: &str, caption: Option<&str>) {
    push_text(output, uri);
    output.push('~');
    match caption {
        Some(caption) => push_text(output, caption),
        None => output.push('-'),
    }
}

/// Splits one `<uri>~<caption|->` item off the front of `rest`.
fn take_item<'a>(
    field: &'static str,
    rest: &'a str,
) -> Result<(String, Option<String>, &'a str), CodecError> {
    let (uri, after) = take_text(field, rest)?;
    let after = after
        .strip_prefix('~')
        .ok_or_else(|| invalid(field, rest))?;
    let (caption, after) = if let Some(after) = after.strip_prefix('-') {
        (None, after)
    } else {
        let (caption, after) = take_text(field, after)?;
        (Some(caption.to_owned()), after)
    };
    Ok((uri.to_owned(), caption, after))
}

/// Decodes slideshow settings.
pub(super) fn decode_slideshow(
    field: &'static str,
    value: &str,
) -> Result<SlideshowConfig, CodecError> {
    let mut parts = value.splitn(9, '~');
    let mut next = || parts.next().ok_or_else(|| invalid(field, value));
    let slide_frames = canonical(field, next()?)?;
    let transition =
        SlideTransitionStyle::from_token(next()?).ok_or_else(|| invalid(field, value))?;
    let transition_frames = canonical(field, next()?)?;
    let ken_burns = flag(field, next()?)?;
    let order = match next()? {
        "ordered" => SlideOrder::Ordered,
        other => SlideOrder::Random {
            seed: canonical(
                field,
                other
                    .strip_prefix("random.")
                    .ok_or_else(|| invalid(field, value))?,
            )?,
        },
    };
    let loop_show = flag(field, next()?)?;
    let auto_advance = flag(field, next()?)?;
    let fit = parse_fit(field, next()?)?;
    let source = next()?;
    let source = if let Some(rest) = source.strip_prefix("folder~") {
        let (uri, after) = take_text(field, rest)?;
        if !after.is_empty() {
            return Err(invalid(field, value));
        }
        SlideshowSource::Folder {
            asset_uri: uri.to_owned(),
        }
    } else if let Some(mut rest) = source.strip_prefix("list~") {
        let mut items = Vec::new();
        loop {
            if items.len() >= crate::MAX_SLIDESHOW_ITEMS {
                return Err(invalid(field, "too_many_items"));
            }
            let (asset_uri, caption, after) = take_item(field, rest)?;
            items.push(SlideshowItem { asset_uri, caption });
            match after.strip_prefix(',') {
                Some(after) => rest = after,
                None if after.is_empty() => break,
                None => return Err(invalid(field, value)),
            }
        }
        SlideshowSource::List(items)
    } else {
        return Err(invalid(field, value));
    };
    let config = SlideshowConfig {
        source,
        slide_frames,
        transition,
        transition_frames,
        ken_burns,
        order,
        loop_show,
        auto_advance,
        fit,
    };
    config.validate_request().map_err(still_error)?;
    Ok(config)
}

/// Encodes an image's `uri` and `fit`.
pub(super) fn encode_image(
    config: &ImageInputConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    config.validate_request().map_err(still_error)?;
    push("uri", config.asset_uri.clone())?;
    push("fit", fit_token(config.fit).to_owned())
}

/// Decodes an image's `uri` and `fit`.
pub(super) fn decode_image(fields: &mut dyn FieldSource) -> Result<ImageInputConfig, CodecError> {
    let config = ImageInputConfig::new(
        fields.required("uri")?,
        parse_fit("fit", &fields.required("fit")?)?,
    );
    config.validate_request().map_err(still_error)?;
    Ok(config)
}

/// Encodes an image (`kind=image`, `uri`, `fit`) or a slideshow
/// (`kind=slideshow`, `slideshow`).
pub(super) fn encode_source(
    source: &WireStillSource,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match source {
        WireStillSource::Image(config) => {
            push("kind", "image".to_owned())?;
            encode_image(config, push)
        }
        WireStillSource::Slideshow(config) => {
            push("kind", "slideshow".to_owned())?;
            push("slideshow", encode_slideshow(config)?)
        }
    }
}

/// Decodes the source fields after a `kind` of `image` or `slideshow`.
pub(super) fn decode_source(
    kind: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<WireStillSource>, CodecError> {
    Ok(Some(match kind {
        "image" => WireStillSource::Image(decode_image(fields)?),
        "slideshow" => WireStillSource::Slideshow(decode_slideshow(
            "slideshow",
            &fields.required("slideshow")?,
        )?),
        _ => return Ok(None),
    }))
}

/// As [`decode_source`], for a record the server sends: its pictures are
/// project assets, never the host paths a client's request may name.
pub(super) fn decode_project_source(
    kind: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<WireStillSource>, CodecError> {
    let source = decode_source(kind, fields)?;
    if let Some(source) = &source {
        source.validate().map_err(still_error)?;
    }
    Ok(source)
}

/// The replacement source token of an image or slideshow.
pub(super) fn encode_source_token(source: &WireStillSource) -> Result<String, CodecError> {
    source
        .validate_request()
        .map_err(|error| invalid("source", error.code()))?;
    Ok(match source {
        WireStillSource::Image(config) => {
            format!("image:{}:{}", fit_token(config.fit), config.asset_uri)
        }
        WireStillSource::Slideshow(config) => format!("slideshow:{}", encode_slideshow(config)?),
    })
}

/// Decodes `image:<fit>:<uri>` or `slideshow:<settings>`; `None` for any
/// other source.
pub(super) fn decode_source_token(value: &str) -> Result<Option<WireStillSource>, CodecError> {
    if let Some(rest) = value.strip_prefix("image:") {
        let (fit, uri) = if let Some(uri) = rest.strip_prefix("1:1:") {
            (OutputScaling::OneToOne, uri)
        } else {
            let (fit, uri) = rest
                .split_once(':')
                .ok_or_else(|| invalid("source", value))?;
            (parse_fit("source", fit)?, uri)
        };
        let config = ImageInputConfig::new(uri, fit);
        config
            .validate_request()
            .map_err(|error| invalid("source", error.code()))?;
        return Ok(Some(WireStillSource::Image(config)));
    }
    if let Some(rest) = value.strip_prefix("slideshow:") {
        return Ok(Some(WireStillSource::Slideshow(decode_slideshow(
            "source", rest,
        )?)));
    }
    Ok(None)
}

/// Encodes `page`, `count`, `listing` (the listing generation), and `label`.
pub(super) fn encode_page_status(
    status: &SlideshowPageStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if !status.is_valid() {
        return Err(invalid("page", status.page.index.to_string()));
    }
    push("page", status.page.index.to_string())?;
    push("count", status.page.count.to_string())?;
    push("listing", status.generation.to_string())?;
    push("label", status.label.clone())
}

/// Decodes `page`, `count`, `listing` (the listing generation), and `label`.
pub(super) fn decode_page_status(
    fields: &mut dyn FieldSource,
) -> Result<SlideshowPageStatus, CodecError> {
    let status = SlideshowPageStatus {
        page: SlideshowPage {
            index: canonical("page", &fields.required("page")?)?,
            count: canonical("count", &fields.required("count")?)?,
        },
        generation: canonical("listing", &fields.required("listing")?)?,
        label: fields.required("label")?,
    };
    if !status.is_valid() {
        return Err(invalid("page", status.page.index.to_string()));
    }
    Ok(status)
}

/// Encodes `action` and, for `goto`, `page`.
pub(super) fn encode_navigation(
    navigation: PagedNavigation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("action", navigation.token().to_owned())?;
    if let PagedNavigation::Goto(page) = navigation {
        if PagedNavigation::from_token("goto", Some(page)).is_none() {
            return Err(invalid("page", page.to_string()));
        }
        push("page", page.to_string())?;
    }
    Ok(())
}

/// Decodes `action` and, for `goto`, `page`.
pub(super) fn decode_navigation(
    fields: &mut dyn FieldSource,
) -> Result<PagedNavigation, CodecError> {
    let action = fields.required("action")?;
    let page = if action == "goto" {
        Some(canonical("page", &fields.required("page")?)?)
    } else {
        None
    };
    PagedNavigation::from_token(&action, page).ok_or_else(|| invalid("action", action))
}

/// Validates a request window.
pub(super) fn check_window(offset: u32, limit: u32) -> Result<(), CodecError> {
    if !(1..=MAX_SLIDESHOW_SLIDES_PAGE).contains(&limit) {
        return Err(invalid("limit", limit.to_string()));
    }
    if offset as usize >= crate::MAX_SLIDESHOW_SLIDES {
        return Err(invalid("offset", offset.to_string()));
    }
    Ok(())
}

fn check_slide(slide: &SlideshowSlide) -> Result<(), CodecError> {
    let caption_ok = slide.caption.as_deref().is_none_or(|caption| {
        !caption.is_empty()
            && caption.len() <= crate::MAX_SLIDE_CAPTION_BYTES
            && !caption.chars().any(char::is_control)
    });
    if !fm_types::still::asset_uri_is_valid(&slide.asset_uri, crate::MAX_IMAGE_URI_BYTES)
        || !caption_ok
    {
        return Err(invalid("slides", slide.asset_uri.clone()));
    }
    Ok(())
}

/// Encodes listed slides.
pub(super) fn encode_slides(slides: &[SlideshowSlide]) -> Result<String, CodecError> {
    if slides.len() > MAX_SLIDESHOW_SLIDES_PAGE as usize {
        return Err(invalid("slides", slides.len().to_string()));
    }
    let mut output = String::new();
    let mut bytes = 0usize;
    for (index, slide) in slides.iter().enumerate() {
        check_slide(slide)?;
        bytes += slide.asset_uri.len() + slide.caption.as_ref().map_or(0, String::len);
        if index > 0 {
            output.push(',');
        }
        push_item(&mut output, &slide.asset_uri, slide.caption.as_deref());
    }
    if bytes > MAX_SLIDESHOW_SLIDES_BYTES {
        return Err(invalid("slides", bytes.to_string()));
    }
    Ok(output)
}

/// Decodes listed slides.
pub(super) fn decode_slides(value: &str) -> Result<Vec<SlideshowSlide>, CodecError> {
    let mut slides = Vec::new();
    if value.is_empty() {
        return Ok(slides);
    }
    let mut rest = value;
    let mut bytes = 0usize;
    loop {
        if slides.len() >= MAX_SLIDESHOW_SLIDES_PAGE as usize {
            return Err(invalid("slides", "too_many_slides"));
        }
        let (asset_uri, caption, after) = take_item("slides", rest)?;
        let slide = SlideshowSlide { asset_uri, caption };
        check_slide(&slide)?;
        bytes += slide.asset_uri.len() + slide.caption.as_ref().map_or(0, String::len);
        if bytes > MAX_SLIDESHOW_SLIDES_BYTES {
            return Err(invalid("slides", bytes.to_string()));
        }
        slides.push(slide);
        match after.strip_prefix(',') {
            Some(after) => rest = after,
            None if after.is_empty() => break,
            None => return Err(invalid("slides", value)),
        }
    }
    Ok(slides)
}
