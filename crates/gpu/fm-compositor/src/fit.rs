//! Exact integer placement of a source picture inside an arbitrary canvas.
//!
//! A canvas may be landscape, portrait, square, or any other bounded size, and
//! a source may have any aspect ratio. [`place_source`] turns a source size, an
//! optional source region, a target rectangle, a quarter-turn rotation, and a
//! [`FitPolicy`] into the crop and [`Transform`] of one [`SourceLayer`]. Every
//! value is derived with integer arithmetic only; no floating point is used,
//! so identical requests always produce identical layers on every platform.
//!
//! [`center_cut`] is the shared primitive: the largest rectangle with a given
//! aspect ratio centered inside another rectangle. [`FitPolicy::Fill`] crops
//! the source with it, [`FitPolicy::Fit`] sizes the placed picture with it, and
//! [`SafeAreaPreset::CenterCut`](crate::SafeAreaPreset::CenterCut) draws it as
//! an operator guide, so a 9:16 guide on a landscape operator view outlines
//! exactly the source region a portrait `Fill` placement keeps.

use core::fmt;

use fm_video::{CropRect, Rotation, Transform};

use crate::scene::{Scene, SourceId, SourceLayer};

/// How a source picture is sized inside its target rectangle.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FitPolicy {
    /// Keep the source aspect ratio and show the whole picture: the largest
    /// centered picture that fits inside the target, leaving letterbox or
    /// pillarbox bars of the scene background. Also known as "contain".
    #[default]
    Fit,
    /// Keep the source aspect ratio and cover the whole target: the source is
    /// cropped to the target aspect ratio around its center. Also known as
    /// "cover" or "center cut".
    Fill,
    /// Scale each axis independently to exactly the target size, distorting a
    /// source whose aspect ratio differs from the target.
    Stretch,
}

/// A target rectangle in canvas pixels. Bounds are half-open: the rectangle
/// covers `x..x + width` and `y..y + height`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CanvasRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CanvasRect {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The whole `width`x`height` canvas.
    #[must_use]
    pub const fn full(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }
}

/// The crop and transform that place one source inside its target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePlacement {
    crop: Option<CropRect>,
    transform: Transform,
}

impl SourcePlacement {
    /// The source-space crop the layer must apply, or `None` for the whole
    /// source frame.
    #[must_use]
    pub const fn crop(&self) -> Option<CropRect> {
        self.crop
    }

    /// The layer transform: `scale_width`x`scale_height` is the pre-rotation
    /// picture size and the translation is the top-left of the rotated bounds.
    #[must_use]
    pub const fn transform(&self) -> Transform {
        self.transform
    }

    /// The placed picture's canvas footprint after rotation, as
    /// `(x, y, width, height)`. The footprint always lies inside the target.
    #[must_use]
    pub const fn bounds(&self) -> (i32, i32, u32, u32) {
        let (width, height) = rotated_size(
            self.transform.scale_width,
            self.transform.scale_height,
            self.transform.rotation,
        );
        (
            self.transform.translation_x,
            self.transform.translation_y,
            width,
            height,
        )
    }

    /// A layer drawing `source` with this placement. Further layer options
    /// (mask, key, opacity, effects) can be chained on the result; a mask is
    /// authored in post-crop source space, which is [`Self::crop`] when set.
    #[must_use]
    pub const fn layer(&self, source: SourceId, z: i32) -> SourceLayer {
        let layer = SourceLayer::new(source, z, self.transform);
        match self.crop {
            Some(crop) => layer.with_crop(crop),
            None => layer,
        }
    }
}

/// The largest rectangle with aspect ratio `aspect_width:aspect_height`
/// centered inside a `width`x`height` rectangle, relative to its top-left.
///
/// When the rectangle is wider than the aspect ratio the full height is kept
/// and the width is `height * aspect_width / aspect_height`; when it is
/// narrower the full width is kept and the height is
/// `width * aspect_height / aspect_width`; an equal ratio returns the whole
/// rectangle. The derived side is rounded to the nearest integer with exact
/// halves rounded up, is at least one pixel, and never exceeds the rectangle.
/// The offset on the derived axis is `(size - derived) / 2`, truncated, so an
/// odd leftover pixel lands on the right or bottom edge.
///
/// # Errors
///
/// Returns [`FitError::ZeroDimension`] when any argument is zero.
///
/// # Examples
///
/// ```
/// use fm_compositor::{CropRect, center_cut};
///
/// // The 9:16 vertical cut of a 1920x1080 frame.
/// assert_eq!(center_cut(1920, 1080, 9, 16), Ok(CropRect::new(656, 0, 608, 1080)));
/// // The 16:9 band of a 1080x1920 portrait canvas.
/// assert_eq!(center_cut(1080, 1920, 16, 9), Ok(CropRect::new(0, 656, 1080, 608)));
/// ```
pub fn center_cut(
    width: u32,
    height: u32,
    aspect_width: u32,
    aspect_height: u32,
) -> Result<CropRect, FitError> {
    if width == 0 || height == 0 || aspect_width == 0 || aspect_height == 0 {
        return Err(FitError::ZeroDimension);
    }
    let wide = u128::from(width) * u128::from(aspect_height);
    let tall = u128::from(height) * u128::from(aspect_width);
    let (cut_width, cut_height) = match wide.cmp(&tall) {
        core::cmp::Ordering::Greater => (
            rounded_ratio(height, aspect_width, aspect_height, width),
            height,
        ),
        core::cmp::Ordering::Less => (
            width,
            rounded_ratio(width, aspect_height, aspect_width, height),
        ),
        core::cmp::Ordering::Equal => (width, height),
    };
    Ok(CropRect::new(
        (width - cut_width) / 2,
        (height - cut_height) / 2,
        cut_width,
        cut_height,
    ))
}

/// Places a `source_width`x`source_height` source inside `target` by `policy`.
///
/// `region` optionally restricts the source to a rectangle (an authored crop,
/// such as the vertical strip of a landscape camera); the policy then applies
/// to that region. `rotation` turns the picture clockwise by quarter turns, so
/// a landscape source rotated by 90 or 270 degrees fills a portrait target
/// without cropping.
///
/// All geometry is computed in source orientation: the target is first turned
/// back by the rotation (width and height swap for quarter turns), then
///
/// - [`FitPolicy::Stretch`] scales the region to the turned target exactly;
/// - [`FitPolicy::Fit`] scales the region to the [`center_cut`] of the turned
///   target with the region's aspect ratio;
/// - [`FitPolicy::Fill`] crops the region to its [`center_cut`] with the turned
///   target's aspect ratio and scales that crop to the turned target.
///
/// The rotated picture is centered in `target` with truncating division, so an
/// odd leftover pixel of a letterbox or pillarbox lands on the right or bottom.
/// A `Fill` crop is centered in source space; after a quarter turn its odd
/// leftover source pixel therefore appears on whichever output edge that
/// source edge rotates to. The returned crop is `None` only when the whole
/// source frame is used.
///
/// # Errors
///
/// Returns a typed error for a zero or oversized source or target, a region
/// that is empty or outside the source, or a target whose far edge does not
/// fit a scene transform.
pub fn place_source(
    source_width: u32,
    source_height: u32,
    region: Option<CropRect>,
    target: CanvasRect,
    rotation: Rotation,
    policy: FitPolicy,
) -> Result<SourcePlacement, FitError> {
    if source_width == 0 || source_height == 0 {
        return Err(FitError::ZeroDimension);
    }
    if source_width > Scene::MAX_WIDTH || source_height > Scene::MAX_HEIGHT {
        return Err(FitError::SourceTooLarge {
            width: source_width,
            height: source_height,
        });
    }
    let region = match region {
        Some(rect) => {
            let inside = rect.width > 0
                && rect.height > 0
                && rect
                    .x
                    .checked_add(rect.width)
                    .is_some_and(|right| right <= source_width)
                && rect
                    .y
                    .checked_add(rect.height)
                    .is_some_and(|bottom| bottom <= source_height);
            if !inside {
                return Err(FitError::RegionOutsideSource {
                    region: rect,
                    source_width,
                    source_height,
                });
            }
            rect
        }
        None => CropRect::new(0, 0, source_width, source_height),
    };
    if target.width == 0 || target.height == 0 {
        return Err(FitError::ZeroDimension);
    }
    if target.width > Scene::MAX_WIDTH || target.height > Scene::MAX_HEIGHT {
        return Err(FitError::TargetTooLarge { target });
    }
    let representable = |origin: u32, size: u32| {
        origin
            .checked_add(size)
            .is_some_and(|edge| i32::try_from(edge).is_ok())
    };
    if !representable(target.x, target.width) || !representable(target.y, target.height) {
        return Err(FitError::TargetOutOfRange { target });
    }

    let (turned_width, turned_height) = rotated_size(target.width, target.height, rotation);
    let (crop, scale_width, scale_height) = match policy {
        FitPolicy::Stretch => (region, turned_width, turned_height),
        FitPolicy::Fit => {
            let picture = center_cut(turned_width, turned_height, region.width, region.height)?;
            (region, picture.width, picture.height)
        }
        FitPolicy::Fill => {
            let cut = center_cut(region.width, region.height, turned_width, turned_height)?;
            (
                CropRect::new(region.x + cut.x, region.y + cut.y, cut.width, cut.height),
                turned_width,
                turned_height,
            )
        }
    };
    let (placed_width, placed_height) = rotated_size(scale_width, scale_height, rotation);
    // The target edges were checked to fit `i32`, and the placed picture lies
    // inside the target, so both translations are representable.
    let translation_x = i32::try_from(target.x + (target.width - placed_width) / 2)
        .map_err(|_| FitError::TargetOutOfRange { target })?;
    let translation_y = i32::try_from(target.y + (target.height - placed_height) / 2)
        .map_err(|_| FitError::TargetOutOfRange { target })?;
    let whole_source = crop == CropRect::new(0, 0, source_width, source_height);
    Ok(SourcePlacement {
        crop: (!whole_source).then_some(crop),
        transform: Transform::new(
            translation_x,
            translation_y,
            scale_width,
            scale_height,
            rotation,
        ),
    })
}

/// Typed placement errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FitError {
    /// A source, target, or aspect dimension is zero.
    ZeroDimension,
    /// The source exceeds the scene dimension bounds.
    SourceTooLarge { width: u32, height: u32 },
    /// The source region is empty or does not lie inside the source.
    RegionOutsideSource {
        region: CropRect,
        source_width: u32,
        source_height: u32,
    },
    /// The target exceeds the scene dimension bounds.
    TargetTooLarge { target: CanvasRect },
    /// The target's far edge cannot be expressed as a scene translation.
    TargetOutOfRange { target: CanvasRect },
}

impl fmt::Display for FitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension => formatter.write_str("placement dimensions must be nonzero"),
            Self::SourceTooLarge { width, height } => write!(
                formatter,
                "source {width}x{height} exceeds {}x{}",
                Scene::MAX_WIDTH,
                Scene::MAX_HEIGHT
            ),
            Self::RegionOutsideSource {
                region,
                source_width,
                source_height,
            } => write!(
                formatter,
                "source region {}x{} at ({}, {}) is empty or outside the \
                 {source_width}x{source_height} source",
                region.width, region.height, region.x, region.y
            ),
            Self::TargetTooLarge { target } => write!(
                formatter,
                "target {}x{} exceeds {}x{}",
                target.width,
                target.height,
                Scene::MAX_WIDTH,
                Scene::MAX_HEIGHT
            ),
            Self::TargetOutOfRange { target } => write!(
                formatter,
                "target {}x{} at ({}, {}) cannot be positioned in a scene transform",
                target.width, target.height, target.x, target.y
            ),
        }
    }
}

impl std::error::Error for FitError {}

/// `value * numerator / denominator` rounded half up, clamped to
/// `1..=maximum`. Callers guarantee the exact quotient is at most `maximum`.
fn rounded_ratio(value: u32, numerator: u32, denominator: u32, maximum: u32) -> u32 {
    let numerator = u128::from(value) * u128::from(numerator);
    let denominator = u128::from(denominator);
    let rounded = (2 * numerator + denominator) / (2 * denominator);
    u32::try_from(rounded).map_or(maximum, |rounded| rounded.clamp(1, maximum))
}

const fn rotated_size(width: u32, height: u32, rotation: Rotation) -> (u32, u32) {
    match rotation {
        Rotation::Deg0 | Rotation::Deg180 => (width, height),
        Rotation::Deg90 | Rotation::Deg270 => (height, width),
    }
}
