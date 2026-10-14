//! Bounded, exact-integer CPU reference video effects.
//!
//! Every effect is a pure RGBA8 frame transform. Source pixels are read
//! through [`ImageFrame::pixel`], which respects the source stride, and the
//! output is always tightly packed at `width * 4` bytes per row. Color effects
//! never touch alpha, [`ReferenceEffect::Flip`] moves whole pixels including
//! alpha, and all arithmetic is exact integer math so repeated executions over
//! the same input are byte-identical.

use core::fmt;

use fm_video::{FrameError, ImageFrame, Rgba8};

use crate::{CompositionPlan, Effect};

/// Descriptor name of [`ReferenceEffect::Levels`].
const LEVELS_NAME: &str = "levels";
/// Descriptor name of [`ReferenceEffect::BrightnessContrast`].
const BRIGHTNESS_CONTRAST_NAME: &str = "brightness_contrast";
/// Descriptor name of [`ReferenceEffect::Saturation`].
const SATURATION_NAME: &str = "saturation";
/// Descriptor name of [`ReferenceEffect::Sharpen`].
const SHARPEN_NAME: &str = "sharpen";
/// Descriptor name of [`ReferenceEffect::Flip`].
const FLIP_NAME: &str = "flip";

/// Longest descriptor name echoed in a [`ReferenceEffectError`] message.
///
/// Matches [`CompositionPlan::MAX_EFFECT_NAME_BYTES`], the bound already
/// enforced when a scene compiles into a plan.
const MAX_REPORTED_NAME_BYTES: usize = CompositionPlan::MAX_EFFECT_NAME_BYTES;

/// A bounded CPU reference effect parsed from a backend-neutral [`Effect`].
///
/// Each variant documents the descriptor name and parameter schema accepted by
/// [`ReferenceEffect::from_descriptor`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceEffect {
    /// Black/white level mapping. Descriptor name `"levels"`, parameters
    /// `[black, white]`, both `0..=255` with `black < white`.
    ///
    /// With `span = white - black`, every RGB channel is
    /// `((input - black) * 255 + span / 2) / span`, clamped to `0..=255`.
    /// Truncating integer division after adding `span / 2` rounds half away
    /// from zero; inputs at or below `black` clamp to `0`. Alpha is untouched.
    Levels { black: u8, white: u8 },
    /// Brightness/contrast adjustment. Descriptor name
    /// `"brightness_contrast"`, parameters `[brightness, contrast]`, both
    /// `-100..=100`.
    ///
    /// Every RGB channel computes `value = (input - 128) * (100 + contrast)`
    /// and `output = round_div_100(value) + 128 + brightness`, clamped to
    /// `0..=255`. `round_div_100` rounds `value / 100` to the nearest integer
    /// with halves away from zero. Alpha is untouched.
    BrightnessContrast { brightness: i16, contrast: i16 },
    /// Saturation adjustment. Descriptor name `"saturation"`, parameter
    /// `[amount]`, `-100..=100`.
    ///
    /// Every pixel computes BT.601 luma in `i32` as
    /// `y = (77 * r + 150 * g + 29 * b + 128) >> 8`, then each channel becomes
    /// `y + round_div_100((channel - y) * (100 + amount))`, clamped to
    /// `0..=255`. `amount == -100` is exact grayscale, `amount == 0` is the
    /// exact identity. Alpha is untouched.
    Saturation { amount: i16 },
    /// Unsharp mask in per-mille. Descriptor name `"sharpen"`, parameter
    /// `[amount]`, `0..=1000`.
    ///
    /// Every channel uses edge-clamped (replicated border) neighbors:
    /// `blur = (4 * c + up + down + left + right) / 8` with truncating integer
    /// division, then `output = c + round_1000(amount * (c - blur))`, clamped
    /// to `0..=255`. `round_1000` rounds `value / 1000` to the nearest integer
    /// with halves away from zero. `amount == 0` is the exact identity. Alpha
    /// is untouched.
    Sharpen { amount: u16 },
    /// Whole-frame mirror. Descriptor name `"flip"`, parameters
    /// `[horizontal, vertical]`, each exactly `0` or `1`.
    ///
    /// Pixels including alpha move to their mirrored coordinates;
    /// `false, false` is the exact identity.
    Flip { horizontal: bool, vertical: bool },
}

impl ReferenceEffect {
    /// Returns the descriptor name this effect parses from.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Levels { .. } => LEVELS_NAME,
            Self::BrightnessContrast { .. } => BRIGHTNESS_CONTRAST_NAME,
            Self::Saturation { .. } => SATURATION_NAME,
            Self::Sharpen { .. } => SHARPEN_NAME,
            Self::Flip { .. } => FLIP_NAME,
        }
    }

    /// Parses a backend-neutral descriptor into a bounded reference effect.
    ///
    /// The parameterless `"passthrough"` descriptor is not a reference effect;
    /// the CPU executor handles it as a no-op before parsing.
    ///
    /// # Errors
    ///
    /// Returns [`ReferenceEffectError::UnknownEffect`] for an unrecognized
    /// name, [`ReferenceEffectError::WrongParameterCount`] for a known name
    /// with the wrong number of parameters, and
    /// [`ReferenceEffectError::ParameterOutOfRange`] or
    /// [`ReferenceEffectError::InvalidLevels`] for a known name with a
    /// malformed parameter.
    pub fn from_descriptor(effect: &Effect) -> Result<Self, ReferenceEffectError> {
        let name = effect.name();
        let parameters = effect.parameters();
        match name {
            LEVELS_NAME => parse_levels(name, parameters),
            BRIGHTNESS_CONTRAST_NAME => parse_brightness_contrast(name, parameters),
            SATURATION_NAME => parse_saturation(name, parameters),
            SHARPEN_NAME => parse_sharpen(name, parameters),
            FLIP_NAME => parse_flip(name, parameters),
            _ => Err(ReferenceEffectError::UnknownEffect {
                name: name.to_owned(),
            }),
        }
    }
}

/// Reasons a backend-neutral [`Effect`] does not parse into a
/// [`ReferenceEffect`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceEffectError {
    /// The descriptor name is not a known reference effect.
    UnknownEffect { name: String },
    /// The descriptor name is known but the parameter count differs.
    WrongParameterCount {
        name: String,
        expected: usize,
        actual: usize,
    },
    /// A parameter falls outside its documented inclusive range.
    ParameterOutOfRange {
        name: String,
        parameter: usize,
        value: i32,
        minimum: i32,
        maximum: i32,
    },
    /// `levels` requires `black < white`.
    InvalidLevels { black: u8, white: u8 },
}

impl fmt::Display for ReferenceEffectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEffect { name } => {
                formatter.write_str("unknown reference effect \"")?;
                write_bounded_name(formatter, name)?;
                formatter.write_str("\"")
            }
            Self::WrongParameterCount {
                name,
                expected,
                actual,
            } => {
                formatter.write_str("reference effect \"")?;
                write_bounded_name(formatter, name)?;
                write!(formatter, "\" expects {expected} parameters, got {actual}")
            }
            Self::ParameterOutOfRange {
                name,
                parameter,
                value,
                minimum,
                maximum,
            } => {
                formatter.write_str("reference effect \"")?;
                write_bounded_name(formatter, name)?;
                write!(
                    formatter,
                    "\" parameter {parameter} value {value} is outside {minimum}..={maximum}"
                )
            }
            Self::InvalidLevels { black, white } => write!(
                formatter,
                "reference effect \"{LEVELS_NAME}\" requires black < white, got black {black} and \
                 white {white}"
            ),
        }
    }
}

impl std::error::Error for ReferenceEffectError {}

/// Applies one parsed reference effect, returning a tightly packed frame.
///
/// The output stride is always `width * 4`, matching the CPU executor's crop
/// and key/mask stages. Color effects leave alpha untouched; `flip` moves whole
/// pixels including alpha.
///
/// # Errors
///
/// Returns [`FrameError`] when the source layout cannot be addressed or the
/// output would exceed the bounded frame layout.
pub fn apply_reference_effect(
    frame: &ImageFrame,
    effect: ReferenceEffect,
) -> Result<ImageFrame, FrameError> {
    match effect {
        ReferenceEffect::Levels { black, white } => map_pixels(frame, |_, _, pixel| {
            Ok(Rgba8::new(
                levels_channel(pixel.r, black, white),
                levels_channel(pixel.g, black, white),
                levels_channel(pixel.b, black, white),
                pixel.a,
            ))
        }),
        ReferenceEffect::BrightnessContrast {
            brightness,
            contrast,
        } => map_pixels(frame, |_, _, pixel| {
            Ok(Rgba8::new(
                brightness_contrast_channel(pixel.r, brightness, contrast),
                brightness_contrast_channel(pixel.g, brightness, contrast),
                brightness_contrast_channel(pixel.b, brightness, contrast),
                pixel.a,
            ))
        }),
        ReferenceEffect::Saturation { amount } => {
            map_pixels(frame, |_, _, pixel| Ok(saturation_pixel(pixel, amount)))
        }
        ReferenceEffect::Sharpen { amount } => map_pixels(frame, |x, y, pixel| {
            Ok(sharpen_pixel(frame, x, y, pixel, amount))
        }),
        ReferenceEffect::Flip {
            horizontal,
            vertical,
        } => map_pixels(frame, |x, y, _| {
            let source_x = if horizontal { frame.width() - 1 - x } else { x };
            let source_y = if vertical { frame.height() - 1 - y } else { y };
            frame
                .pixel(source_x, source_y)
                .ok_or(FrameError::LayoutOverflow)
        }),
    }
}

fn parse_levels(name: &str, parameters: &[i32]) -> Result<ReferenceEffect, ReferenceEffectError> {
    expect_parameter_count(name, parameters, 2)?;
    let black = u8_parameter(name, 0, parameters[0], 0, u8::MAX)?;
    let white = u8_parameter(name, 1, parameters[1], 0, u8::MAX)?;
    if black >= white {
        return Err(ReferenceEffectError::InvalidLevels { black, white });
    }
    Ok(ReferenceEffect::Levels { black, white })
}

fn parse_brightness_contrast(
    name: &str,
    parameters: &[i32],
) -> Result<ReferenceEffect, ReferenceEffectError> {
    expect_parameter_count(name, parameters, 2)?;
    let brightness = i16_parameter(name, 0, parameters[0], -100, 100)?;
    let contrast = i16_parameter(name, 1, parameters[1], -100, 100)?;
    Ok(ReferenceEffect::BrightnessContrast {
        brightness,
        contrast,
    })
}

fn parse_saturation(
    name: &str,
    parameters: &[i32],
) -> Result<ReferenceEffect, ReferenceEffectError> {
    expect_parameter_count(name, parameters, 1)?;
    let amount = i16_parameter(name, 0, parameters[0], -100, 100)?;
    Ok(ReferenceEffect::Saturation { amount })
}

fn parse_sharpen(name: &str, parameters: &[i32]) -> Result<ReferenceEffect, ReferenceEffectError> {
    expect_parameter_count(name, parameters, 1)?;
    let amount = u16_parameter(name, 0, parameters[0], 0, 1000)?;
    Ok(ReferenceEffect::Sharpen { amount })
}

fn parse_flip(name: &str, parameters: &[i32]) -> Result<ReferenceEffect, ReferenceEffectError> {
    expect_parameter_count(name, parameters, 2)?;
    let horizontal = bool_parameter(name, 0, parameters[0])?;
    let vertical = bool_parameter(name, 1, parameters[1])?;
    Ok(ReferenceEffect::Flip {
        horizontal,
        vertical,
    })
}

fn expect_parameter_count(
    name: &str,
    parameters: &[i32],
    expected: usize,
) -> Result<(), ReferenceEffectError> {
    if parameters.len() == expected {
        Ok(())
    } else {
        Err(ReferenceEffectError::WrongParameterCount {
            name: name.to_owned(),
            expected,
            actual: parameters.len(),
        })
    }
}

fn bool_parameter(name: &str, parameter: usize, value: i32) -> Result<bool, ReferenceEffectError> {
    Ok(u8_parameter(name, parameter, value, 0, 1)? == 1)
}

fn u8_parameter(
    name: &str,
    parameter: usize,
    value: i32,
    minimum: u8,
    maximum: u8,
) -> Result<u8, ReferenceEffectError> {
    u8::try_from(value)
        .ok()
        .filter(|converted| (minimum..=maximum).contains(converted))
        .ok_or_else(|| {
            out_of_range(
                name,
                parameter,
                value,
                i32::from(minimum),
                i32::from(maximum),
            )
        })
}

fn u16_parameter(
    name: &str,
    parameter: usize,
    value: i32,
    minimum: u16,
    maximum: u16,
) -> Result<u16, ReferenceEffectError> {
    u16::try_from(value)
        .ok()
        .filter(|converted| (minimum..=maximum).contains(converted))
        .ok_or_else(|| {
            out_of_range(
                name,
                parameter,
                value,
                i32::from(minimum),
                i32::from(maximum),
            )
        })
}

fn i16_parameter(
    name: &str,
    parameter: usize,
    value: i32,
    minimum: i16,
    maximum: i16,
) -> Result<i16, ReferenceEffectError> {
    i16::try_from(value)
        .ok()
        .filter(|converted| (minimum..=maximum).contains(converted))
        .ok_or_else(|| {
            out_of_range(
                name,
                parameter,
                value,
                i32::from(minimum),
                i32::from(maximum),
            )
        })
}

fn out_of_range(
    name: &str,
    parameter: usize,
    value: i32,
    minimum: i32,
    maximum: i32,
) -> ReferenceEffectError {
    ReferenceEffectError::ParameterOutOfRange {
        name: name.to_owned(),
        parameter,
        value,
        minimum,
        maximum,
    }
}

fn write_bounded_name(formatter: &mut fmt::Formatter<'_>, name: &str) -> fmt::Result {
    let mut end = name.len().min(MAX_REPORTED_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    formatter.write_str(&name[..end])?;
    if end < name.len() {
        formatter.write_str("...")?;
    }
    Ok(())
}

/// Rounds `value / 100` to the nearest integer, rounding halves away from
/// zero. The remainder form avoids negating `i32::MIN`.
const fn round_div_100(value: i32) -> i32 {
    let quotient = value / 100;
    let remainder = value % 100;
    if remainder >= 50 {
        quotient + 1
    } else if remainder <= -50 {
        quotient - 1
    } else {
        quotient
    }
}

/// Rounds `value / 1000` to the nearest integer, rounding halves away from
/// zero. The remainder form avoids negating `i32::MIN`.
const fn round_1000(value: i32) -> i32 {
    let quotient = value / 1000;
    let remainder = value % 1000;
    if remainder >= 500 {
        quotient + 1
    } else if remainder <= -500 {
        quotient - 1
    } else {
        quotient
    }
}

/// Builds a tightly packed output frame by transforming every source pixel.
///
/// Edge-clamped effects resolve neighbors through the source frame itself, so
/// callers only have to map the visited coordinate.
fn map_pixels<F>(frame: &ImageFrame, mut transform: F) -> Result<ImageFrame, FrameError>
where
    F: FnMut(u32, u32, Rgba8) -> Result<Rgba8, FrameError>,
{
    let mut bytes = Vec::with_capacity(frame.pixels().len());
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let pixel = frame.pixel(x, y).ok_or(FrameError::LayoutOverflow)?;
            bytes.extend_from_slice(&transform(x, y, pixel)?.to_bytes());
        }
    }
    ImageFrame::new(
        frame.width(),
        frame.height(),
        frame.width() as usize * 4,
        bytes,
    )
}

/// Maps one channel. `black < white` is a documented invariant of
/// [`ReferenceEffect::Levels`]; a directly constructed invalid pair returns the
/// input unchanged instead of dividing by zero.
fn levels_channel(input: u8, black: u8, white: u8) -> u8 {
    let span = i32::from(white) - i32::from(black);
    if span <= 0 {
        return input;
    }
    let numerator = (i32::from(input) - i32::from(black)) * 255 + span / 2;
    u8::try_from((numerator / span).clamp(0, 255)).unwrap_or(u8::MAX)
}

fn brightness_contrast_channel(input: u8, brightness: i16, contrast: i16) -> u8 {
    let value = (i32::from(input) - 128) * (100 + i32::from(contrast));
    let output = round_div_100(value) + 128 + i32::from(brightness);
    u8::try_from(output.clamp(0, 255)).unwrap_or(u8::MAX)
}

fn saturation_pixel(pixel: Rgba8, amount: i16) -> Rgba8 {
    let luma =
        (77 * i32::from(pixel.r) + 150 * i32::from(pixel.g) + 29 * i32::from(pixel.b) + 128) >> 8;
    let scale = 100 + i32::from(amount);
    Rgba8::new(
        saturation_channel(pixel.r, luma, scale),
        saturation_channel(pixel.g, luma, scale),
        saturation_channel(pixel.b, luma, scale),
        pixel.a,
    )
}

fn saturation_channel(channel: u8, luma: i32, scale: i32) -> u8 {
    let output = luma + round_div_100((i32::from(channel) - luma) * scale);
    u8::try_from(output.clamp(0, 255)).unwrap_or(u8::MAX)
}

fn sharpen_pixel(frame: &ImageFrame, x: u32, y: u32, pixel: Rgba8, amount: u16) -> Rgba8 {
    let up = y.saturating_sub(1);
    let down = (y + 1).min(frame.height() - 1);
    let left = x.saturating_sub(1);
    let right = (x + 1).min(frame.width() - 1);
    let up = frame.pixel(x, up).unwrap_or(pixel);
    let down = frame.pixel(x, down).unwrap_or(pixel);
    let left = frame.pixel(left, y).unwrap_or(pixel);
    let right = frame.pixel(right, y).unwrap_or(pixel);
    Rgba8::new(
        sharpen_channel(pixel.r, up.r, down.r, left.r, right.r, amount),
        sharpen_channel(pixel.g, up.g, down.g, left.g, right.g, amount),
        sharpen_channel(pixel.b, up.b, down.b, left.b, right.b, amount),
        pixel.a,
    )
}

fn sharpen_channel(center: u8, up: u8, down: u8, left: u8, right: u8, amount: u16) -> u8 {
    let blur = (4 * i32::from(center)
        + i32::from(up)
        + i32::from(down)
        + i32::from(left)
        + i32::from(right))
        / 8;
    let adjustment = round_1000(i32::from(amount) * (i32::from(center) - blur));
    u8::try_from((i32::from(center) + adjustment).clamp(0, 255)).unwrap_or(u8::MAX)
}
