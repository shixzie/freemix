//! Preset and custom operator safe-area guide generation.
//!
//! A safe-area guide is a free-form operator rectangle: the SMPTE action-safe
//! and title-safe boundaries, an aspect-ratio center cut (for example the 9:16
//! vertical strip of a landscape program), or a caller-defined custom guide.
//! Every preset is defined per canvas axis, so the same preset is equally
//! valid on landscape, portrait, square, and odd-sized canvases. This module
//! turns a [`SafeAreaPreset`] plus a canvas size into exact integer
//! [`SafeAreaGuide`] rectangles. Geometry is pure and deterministic: every
//! dimension is derived with truncating integer division and never floating
//! point.
//!
//! Output routing is unchanged. Guides reach a plan only when a scene is
//! compiled for [`OutputTarget::Operator`](crate::OutputTarget);
//! [`compile_scene`](crate::compile_scene) omits them from every other target.
//! A caller that wants guides on another output must route them explicitly as
//! overlay layers.

use core::fmt;

use fm_video::Rgba8;

use crate::plan::CompositionPlan;
use crate::scene::{SafeAreaGuide, Scene};

/// Maximum number of safe-area guides one compiled plan retains.
///
/// This is [`CompositionPlan::MAX_SAFE_AREAS`], the bound that
/// [`compile_scene`](crate::compile_scene) already enforces. [`Scene`] itself
/// does not bound [`Scene::push_safe_area`]; use [`push_preset_guides`] to add
/// generated guides without exceeding the plan bound.
pub const MAX_SAFE_AREA_GUIDES: usize = CompositionPlan::MAX_SAFE_AREAS;

/// Action-safe size as a percentage of each canvas dimension.
const ACTION_SAFE_PERCENT: u32 = 93;

/// Title-safe size as a percentage of each canvas dimension.
const TITLE_SAFE_PERCENT: u32 = 90;

/// Percentage denominator for the fixed presets.
const PERCENT_DENOMINATOR: u32 = 100;

/// One complete canvas dimension in basis points (1/100 of a percent).
const FULL_BASIS_POINTS: u32 = 10_000;

/// One selectable safe-area guide preset.
///
/// Every preset generates plain rectangles: no crosshairs or thirds lines.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SafeAreaPreset {
    /// A centered rectangle at 93% of each canvas dimension.
    Action,
    /// A centered rectangle at 90% of each canvas dimension.
    Title,
    /// The nested SMPTE pair: 93% action safe, then 90% title safe.
    Smpte,
    /// The largest centered rectangle with aspect ratio
    /// `aspect_width:aspect_height`, as computed by
    /// [`center_cut`](crate::center_cut). A `9:16` cut on a landscape canvas
    /// outlines exactly the source region a portrait
    /// [`FitPolicy::Fill`](crate::FitPolicy::Fill) placement keeps; a `16:9`
    /// cut on a portrait canvas outlines the band a landscape source fills
    /// under [`FitPolicy::Fit`](crate::FitPolicy::Fit).
    CenterCut {
        /// Aspect-ratio width term; must be nonzero.
        aspect_width: u16,
        /// Aspect-ratio height term; must be nonzero.
        aspect_height: u16,
    },
    /// A caller-supplied rectangle in basis points of each canvas dimension.
    ///
    /// Each component is measured in basis points (1/100 of a percent) and must
    /// be in `0..=10_000`. Each axis is `canvas * basis_points / 10_000`,
    /// truncated. The resulting rectangle must be nonempty and fully inside the
    /// canvas.
    Custom {
        /// Horizontal offset in basis points of the canvas width.
        x_basis_points: u16,
        /// Vertical offset in basis points of the canvas height.
        y_basis_points: u16,
        /// Width in basis points of the canvas width.
        width_basis_points: u16,
        /// Height in basis points of the canvas height.
        height_basis_points: u16,
    },
}

/// Generates the guides for one preset on a canvas.
///
/// `Action` and `Title` each produce one centered rectangle sized
/// `canvas * percent / 100` with truncating division (`93%` and `90%`
/// respectively) and positioned at `(canvas - size) / 2` per axis. `Smpte`
/// produces exactly two guides in that order: 93% action safe, then 90% title
/// safe. `CenterCut` produces one [`center_cut`](crate::center_cut)
/// rectangle. `Custom` uses the recorded basis points directly: each axis is
/// `canvas * basis_points / 10_000`, truncated.
///
/// Every generated rectangle is half-open: it covers `x..x + width` and
/// `y..y + height`. Results depend only on the inputs, so identical calls are
/// byte-identical, and a successful preset never contains a zero-size guide.
///
/// # Errors
///
/// Returns [`SafeAreaError::ZeroCanvasWidth`] or
/// [`SafeAreaError::ZeroCanvasHeight`] for a degenerate canvas, and
/// [`SafeAreaError::CanvasTooSmall`] when a fixed preset would truncate to a
/// zero-size guide, and [`SafeAreaError::ZeroAspect`] for a center cut with a
/// zero aspect term. A custom rectangle that truncates to zero size is rejected
/// with [`SafeAreaError::CustomGuideEmpty`]; one that does not fit inside the
/// canvas, or records basis points above 10,000, is rejected with
/// [`SafeAreaError::CustomGuideOutOfBounds`].
///
/// # Examples
///
/// ```
/// use fm_compositor::{Rgba8, SafeAreaPreset, preset_guides};
///
/// let color = Rgba8::new(255, 255, 0, 255);
/// let guides = preset_guides(1920, 1080, SafeAreaPreset::Action, color).unwrap();
/// assert_eq!(guides.len(), 1);
/// assert_eq!(
///     (guides[0].x, guides[0].y, guides[0].width, guides[0].height),
///     (67, 38, 1785, 1004)
/// );
/// ```
pub fn preset_guides(
    canvas_width: u32,
    canvas_height: u32,
    preset: SafeAreaPreset,
    color: Rgba8,
) -> Result<Vec<SafeAreaGuide>, SafeAreaError> {
    if canvas_width == 0 {
        return Err(SafeAreaError::ZeroCanvasWidth);
    }
    if canvas_height == 0 {
        return Err(SafeAreaError::ZeroCanvasHeight);
    }
    match preset {
        SafeAreaPreset::Action => Ok(vec![centered_guide(
            canvas_width,
            canvas_height,
            ACTION_SAFE_PERCENT,
            SafeAreaPreset::Action,
            color,
        )?]),
        SafeAreaPreset::Title => Ok(vec![centered_guide(
            canvas_width,
            canvas_height,
            TITLE_SAFE_PERCENT,
            SafeAreaPreset::Title,
            color,
        )?]),
        SafeAreaPreset::Smpte => Ok(vec![
            centered_guide(
                canvas_width,
                canvas_height,
                ACTION_SAFE_PERCENT,
                SafeAreaPreset::Action,
                color,
            )?,
            centered_guide(
                canvas_width,
                canvas_height,
                TITLE_SAFE_PERCENT,
                SafeAreaPreset::Title,
                color,
            )?,
        ]),
        SafeAreaPreset::CenterCut {
            aspect_width,
            aspect_height,
        } => {
            let cut = crate::fit::center_cut(
                canvas_width,
                canvas_height,
                u32::from(aspect_width),
                u32::from(aspect_height),
            )
            .map_err(|_| SafeAreaError::ZeroAspect {
                aspect_width,
                aspect_height,
            })?;
            Ok(vec![SafeAreaGuide::new(
                cut.x, cut.y, cut.width, cut.height, color,
            )])
        }
        SafeAreaPreset::Custom {
            x_basis_points,
            y_basis_points,
            width_basis_points,
            height_basis_points,
        } => Ok(vec![custom_guide(
            canvas_width,
            canvas_height,
            CustomBasisPoints {
                x: x_basis_points,
                y: y_basis_points,
                width: width_basis_points,
                height: height_basis_points,
            },
            color,
        )?]),
    }
}

/// Generates one preset and pushes every guide onto `scene`.
///
/// Returns the number of guides pushed. The capacity is preflighted against
/// [`MAX_SAFE_AREA_GUIDES`] before anything is pushed, so a rejected call
/// leaves the scene exactly as it was. The color is applied to every generated
/// guide.
///
/// # Errors
///
/// Returns the same geometry errors as [`preset_guides`], or
/// [`SafeAreaError::TooManyGuides`] when the scene already holds enough guides
/// that the generated set would exceed [`MAX_SAFE_AREA_GUIDES`].
pub fn push_preset_guides(
    scene: &mut Scene,
    preset: SafeAreaPreset,
    color: Rgba8,
) -> Result<usize, SafeAreaError> {
    let guides = preset_guides(scene.width(), scene.height(), preset, color)?;
    let existing = scene.safe_areas().len();
    if existing + guides.len() > MAX_SAFE_AREA_GUIDES {
        return Err(SafeAreaError::TooManyGuides {
            existing,
            requested: guides.len(),
            maximum: MAX_SAFE_AREA_GUIDES,
        });
    }
    let pushed = guides.len();
    for guide in guides {
        scene.push_safe_area(guide);
    }
    Ok(pushed)
}

/// Typed safe-area guide generation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeAreaError {
    /// The canvas width is zero.
    ZeroCanvasWidth,
    /// The canvas height is zero.
    ZeroCanvasHeight,
    /// A fixed preset would truncate to a zero-size guide.
    CanvasTooSmall {
        /// Rejected canvas width.
        canvas_width: u32,
        /// Rejected canvas height.
        canvas_height: u32,
        /// The preset that could not produce a nonzero guide.
        preset: SafeAreaPreset,
    },
    /// A center-cut preset has a zero aspect-ratio term.
    ZeroAspect {
        /// Requested aspect-ratio width term.
        aspect_width: u16,
        /// Requested aspect-ratio height term.
        aspect_height: u16,
    },
    /// A custom rectangle truncates to zero pixels on at least one axis.
    CustomGuideEmpty {
        /// Truncated rectangle width in pixels.
        width: u32,
        /// Truncated rectangle height in pixels.
        height: u32,
    },
    /// A custom rectangle leaves the canvas or records out-of-range basis points.
    CustomGuideOutOfBounds {
        /// Requested horizontal offset in basis points.
        x_basis_points: u16,
        /// Requested vertical offset in basis points.
        y_basis_points: u16,
        /// Requested width in basis points.
        width_basis_points: u16,
        /// Requested height in basis points.
        height_basis_points: u16,
        /// Canvas width the rectangle must fit inside.
        canvas_width: u32,
        /// Canvas height the rectangle must fit inside.
        canvas_height: u32,
    },
    /// Pushing the generated guides would exceed the plan capacity.
    TooManyGuides {
        /// Guides already present in the scene.
        existing: usize,
        /// Guides generated for the rejected call.
        requested: usize,
        /// Maximum guides the plan accepts.
        maximum: usize,
    },
}

impl fmt::Display for SafeAreaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCanvasWidth => formatter.write_str("safe-area canvas width must be nonzero"),
            Self::ZeroCanvasHeight => {
                formatter.write_str("safe-area canvas height must be nonzero")
            }
            Self::CanvasTooSmall {
                canvas_width,
                canvas_height,
                preset,
            } => write!(
                formatter,
                "canvas {canvas_width}x{canvas_height} is too small for the {preset:?} safe-area preset"
            ),
            Self::ZeroAspect {
                aspect_width,
                aspect_height,
            } => write!(
                formatter,
                "center-cut safe-area aspect {aspect_width}:{aspect_height} must be nonzero"
            ),
            Self::CustomGuideEmpty { width, height } => write!(
                formatter,
                "custom safe-area guide truncates to an empty {width}x{height} rectangle"
            ),
            Self::CustomGuideOutOfBounds {
                x_basis_points,
                y_basis_points,
                width_basis_points,
                height_basis_points,
                canvas_width,
                canvas_height,
            } => write!(
                formatter,
                "custom safe-area guide at ({x_basis_points}, {y_basis_points}) basis points sized \
                 {width_basis_points}x{height_basis_points} basis points does not fit inside the \
                 {canvas_width}x{canvas_height} canvas"
            ),
            Self::TooManyGuides {
                existing,
                requested,
                maximum,
            } => write!(
                formatter,
                "scene has {existing} safe-area guides and {requested} more were requested; \
                 maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for SafeAreaError {}

/// One custom rectangle request in canvas basis points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CustomBasisPoints {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

/// Builds one centered fixed-percentage guide.
fn centered_guide(
    canvas_width: u32,
    canvas_height: u32,
    percent: u32,
    preset: SafeAreaPreset,
    color: Rgba8,
) -> Result<SafeAreaGuide, SafeAreaError> {
    let width = scale(canvas_width, percent, PERCENT_DENOMINATOR);
    let height = scale(canvas_height, percent, PERCENT_DENOMINATOR);
    if width == 0 || height == 0 {
        return Err(SafeAreaError::CanvasTooSmall {
            canvas_width,
            canvas_height,
            preset,
        });
    }
    Ok(SafeAreaGuide::new(
        (canvas_width - width) / 2,
        (canvas_height - height) / 2,
        width,
        height,
        color,
    ))
}

/// Builds one validated custom guide.
fn custom_guide(
    canvas_width: u32,
    canvas_height: u32,
    basis_points: CustomBasisPoints,
    color: Rgba8,
) -> Result<SafeAreaGuide, SafeAreaError> {
    let out_of_bounds = SafeAreaError::CustomGuideOutOfBounds {
        x_basis_points: basis_points.x,
        y_basis_points: basis_points.y,
        width_basis_points: basis_points.width,
        height_basis_points: basis_points.height,
        canvas_width,
        canvas_height,
    };
    if u32::from(basis_points.x) > FULL_BASIS_POINTS
        || u32::from(basis_points.y) > FULL_BASIS_POINTS
        || u32::from(basis_points.width) > FULL_BASIS_POINTS
        || u32::from(basis_points.height) > FULL_BASIS_POINTS
    {
        return Err(out_of_bounds);
    }
    let x = scale(canvas_width, u32::from(basis_points.x), FULL_BASIS_POINTS);
    let y = scale(canvas_height, u32::from(basis_points.y), FULL_BASIS_POINTS);
    let width = scale(
        canvas_width,
        u32::from(basis_points.width),
        FULL_BASIS_POINTS,
    );
    let height = scale(
        canvas_height,
        u32::from(basis_points.height),
        FULL_BASIS_POINTS,
    );
    let right = u64::from(x) + u64::from(width);
    let bottom = u64::from(y) + u64::from(height);
    if right > u64::from(canvas_width) || bottom > u64::from(canvas_height) {
        return Err(out_of_bounds);
    }
    if width == 0 || height == 0 {
        return Err(SafeAreaError::CustomGuideEmpty { width, height });
    }
    Ok(SafeAreaGuide::new(x, y, width, height, color))
}

/// Exact truncating `value * numerator / denominator` without wide integers.
///
/// Writing `value` as `denominator * quotient + remainder` gives
/// `value * numerator / denominator = quotient * numerator + remainder *
/// numerator / denominator` exactly, because the quotient term is a whole
/// multiple of `denominator` and only the remainder term truncates. Both call
/// sites keep `numerator <= denominator`, so `quotient * numerator` is at most
/// `value` and `remainder * numerator` is below `denominator * denominator`;
/// neither intermediate overflows `u32`.
const fn scale(value: u32, numerator: u32, denominator: u32) -> u32 {
    (value / denominator) * numerator + (value % denominator) * numerator / denominator
}
