//! Animatable properties and how each layers onto a target's static value.

use fm_types::{GARBAGE_MATTE_UNIT, MAX_GARBAGE_MATTE_POINTS};

use crate::value::{Rgba, Value};

/// Basis points meaning a scale factor of `1.0`.
pub const SCALE_IDENTITY_BP: i64 = 10_000;
/// Largest scale, `10.0`, in basis points.
pub const MAX_SCALE_BP: i64 = 100_000;
/// Fully opaque animated opacity: the identity factor.
pub const OPACITY_IDENTITY: i64 = 255;

/// One animatable property of a target.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Property {
    /// Pixels added to the static x.
    PositionX,
    /// Pixels added to the static y.
    PositionY,
    /// Basis points (`0..=100_000`, `10_000` is `1.0`) multiplied into the
    /// static width.
    ScaleX,
    /// Basis points multiplied into the static height.
    ScaleY,
    /// Millidegrees added to the static rotation.
    Rotation,
    /// `0..=255` multiplied into the static opacity (`255` is identity).
    Opacity,
    /// Pixels added to the static left crop.
    CropLeft,
    /// Pixels added to the static top crop.
    CropTop,
    /// Pixels added to the static right crop.
    CropRight,
    /// Pixels added to the static bottom crop.
    CropBottom,
    /// Straight RGBA that overrides the static color.
    Color,
    /// The x coordinate of garbage matte point 1, normalized to the cropped
    /// layer ([`GARBAGE_MATTE_UNIT`] is its right edge), overriding the
    /// static point. The same pattern repeats through
    /// [`Self::MattePoint8Y`]; a layer without a matte, or a matte with fewer
    /// points, ignores tracks for missing points.
    MattePoint1X,
    /// The y coordinate of garbage matte point 1.
    MattePoint1Y,
    /// The x coordinate of garbage matte point 2.
    MattePoint2X,
    /// The y coordinate of garbage matte point 2.
    MattePoint2Y,
    /// The x coordinate of garbage matte point 3.
    MattePoint3X,
    /// The y coordinate of garbage matte point 3.
    MattePoint3Y,
    /// The x coordinate of garbage matte point 4.
    MattePoint4X,
    /// The y coordinate of garbage matte point 4.
    MattePoint4Y,
    /// The x coordinate of garbage matte point 5.
    MattePoint5X,
    /// The y coordinate of garbage matte point 5.
    MattePoint5Y,
    /// The x coordinate of garbage matte point 6.
    MattePoint6X,
    /// The y coordinate of garbage matte point 6.
    MattePoint6Y,
    /// The x coordinate of garbage matte point 7.
    MattePoint7X,
    /// The y coordinate of garbage matte point 7.
    MattePoint7Y,
    /// The x coordinate of garbage matte point 8.
    MattePoint8X,
    /// The y coordinate of garbage matte point 8.
    MattePoint8Y,
}

/// How an animated value combines with the static value beneath it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layering {
    /// `static + animated`.
    Offset,
    /// `static * animated / identity`, rounded once, half up.
    Factor,
    /// The animated value replaces the static one.
    Override,
}

impl Property {
    pub const ALL: [Self; 11 + MAX_GARBAGE_MATTE_POINTS * 2] = [
        Self::PositionX,
        Self::PositionY,
        Self::ScaleX,
        Self::ScaleY,
        Self::Rotation,
        Self::Opacity,
        Self::CropLeft,
        Self::CropTop,
        Self::CropRight,
        Self::CropBottom,
        Self::Color,
        Self::MattePoint1X,
        Self::MattePoint1Y,
        Self::MattePoint2X,
        Self::MattePoint2Y,
        Self::MattePoint3X,
        Self::MattePoint3Y,
        Self::MattePoint4X,
        Self::MattePoint4Y,
        Self::MattePoint5X,
        Self::MattePoint5Y,
        Self::MattePoint6X,
        Self::MattePoint6Y,
        Self::MattePoint7X,
        Self::MattePoint7Y,
        Self::MattePoint8X,
        Self::MattePoint8Y,
    ];

    /// Stable id for UI and wire use.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::PositionX => "position-x",
            Self::PositionY => "position-y",
            Self::ScaleX => "scale-x",
            Self::ScaleY => "scale-y",
            Self::Rotation => "rotation",
            Self::Opacity => "opacity",
            Self::CropLeft => "crop-left",
            Self::CropTop => "crop-top",
            Self::CropRight => "crop-right",
            Self::CropBottom => "crop-bottom",
            Self::Color => "color",
            Self::MattePoint1X => "matte-point-1-x",
            Self::MattePoint1Y => "matte-point-1-y",
            Self::MattePoint2X => "matte-point-2-x",
            Self::MattePoint2Y => "matte-point-2-y",
            Self::MattePoint3X => "matte-point-3-x",
            Self::MattePoint3Y => "matte-point-3-y",
            Self::MattePoint4X => "matte-point-4-x",
            Self::MattePoint4Y => "matte-point-4-y",
            Self::MattePoint5X => "matte-point-5-x",
            Self::MattePoint5Y => "matte-point-5-y",
            Self::MattePoint6X => "matte-point-6-x",
            Self::MattePoint6Y => "matte-point-6-y",
            Self::MattePoint7X => "matte-point-7-x",
            Self::MattePoint7Y => "matte-point-7-y",
            Self::MattePoint8X => "matte-point-8-x",
            Self::MattePoint8Y => "matte-point-8-y",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|property| property.id() == id)
    }

    #[must_use]
    pub const fn layering(self) -> Layering {
        match self {
            Self::ScaleX | Self::ScaleY | Self::Opacity => Layering::Factor,
            Self::Color
            | Self::MattePoint1X
            | Self::MattePoint1Y
            | Self::MattePoint2X
            | Self::MattePoint2Y
            | Self::MattePoint3X
            | Self::MattePoint3Y
            | Self::MattePoint4X
            | Self::MattePoint4Y
            | Self::MattePoint5X
            | Self::MattePoint5Y
            | Self::MattePoint6X
            | Self::MattePoint6Y
            | Self::MattePoint7X
            | Self::MattePoint7Y
            | Self::MattePoint8X
            | Self::MattePoint8Y => Layering::Override,
            Self::PositionX
            | Self::PositionY
            | Self::Rotation
            | Self::CropLeft
            | Self::CropTop
            | Self::CropRight
            | Self::CropBottom => Layering::Offset,
        }
    }

    /// The animated value that leaves the static value unchanged, if any.
    /// [`Self::Color`] and the matte point coordinates override, so they have
    /// none.
    #[must_use]
    pub const fn identity(self) -> Option<Value> {
        match self {
            Self::ScaleX | Self::ScaleY => Some(Value::Scalar(SCALE_IDENTITY_BP)),
            Self::Opacity => Some(Value::Scalar(OPACITY_IDENTITY)),
            Self::Color
            | Self::MattePoint1X
            | Self::MattePoint1Y
            | Self::MattePoint2X
            | Self::MattePoint2Y
            | Self::MattePoint3X
            | Self::MattePoint3Y
            | Self::MattePoint4X
            | Self::MattePoint4Y
            | Self::MattePoint5X
            | Self::MattePoint5Y
            | Self::MattePoint6X
            | Self::MattePoint6Y
            | Self::MattePoint7X
            | Self::MattePoint7Y
            | Self::MattePoint8X
            | Self::MattePoint8Y => None,
            _ => Some(Value::Scalar(0)),
        }
    }

    /// Inclusive scalar range a keyframe value must lie in; `None` for
    /// [`Self::Color`].
    #[must_use]
    pub const fn range(self) -> Option<(i64, i64)> {
        match self {
            Self::PositionX | Self::PositionY | Self::Rotation => {
                Some((i32::MIN as i64, i32::MAX as i64))
            }
            Self::ScaleX | Self::ScaleY => Some((0, MAX_SCALE_BP)),
            Self::Opacity => Some((0, OPACITY_IDENTITY)),
            Self::CropLeft | Self::CropTop | Self::CropRight | Self::CropBottom => {
                Some((0, i32::MAX as i64))
            }
            Self::MattePoint1X
            | Self::MattePoint1Y
            | Self::MattePoint2X
            | Self::MattePoint2Y
            | Self::MattePoint3X
            | Self::MattePoint3Y
            | Self::MattePoint4X
            | Self::MattePoint4Y
            | Self::MattePoint5X
            | Self::MattePoint5Y
            | Self::MattePoint6X
            | Self::MattePoint6Y
            | Self::MattePoint7X
            | Self::MattePoint7Y
            | Self::MattePoint8X
            | Self::MattePoint8Y => Some((0, GARBAGE_MATTE_UNIT as i64)),
            Self::Color => None,
        }
    }

    /// Whether `value` is the right kind and in range for this property.
    #[must_use]
    pub fn accepts(self, value: Value) -> bool {
        match (self.range(), value) {
            (Some((low, high)), Value::Scalar(scalar)) => (low..=high).contains(&scalar),
            (None, Value::Color(_)) => true,
            _ => false,
        }
    }

    /// The garbage matte coordinate this property overrides: point
    /// `index / 2`, x when even and y when odd (both in normalized
    /// `0..=1000` units); `None` for every other property.
    #[must_use]
    pub const fn matte_index(self) -> Option<usize> {
        match self {
            Self::MattePoint1X => Some(0),
            Self::MattePoint1Y => Some(1),
            Self::MattePoint2X => Some(2),
            Self::MattePoint2Y => Some(3),
            Self::MattePoint3X => Some(4),
            Self::MattePoint3Y => Some(5),
            Self::MattePoint4X => Some(6),
            Self::MattePoint4Y => Some(7),
            Self::MattePoint5X => Some(8),
            Self::MattePoint5Y => Some(9),
            Self::MattePoint6X => Some(10),
            Self::MattePoint6Y => Some(11),
            Self::MattePoint7X => Some(12),
            Self::MattePoint7Y => Some(13),
            Self::MattePoint8X => Some(14),
            Self::MattePoint8Y => Some(15),
            _ => None,
        }
    }

    /// Clamps a sampled value into this property's range. Overshooting curves
    /// (Back, bezier) can leave it between keyframes.
    #[must_use]
    pub fn clamp(self, value: Value) -> Value {
        match (self.range(), value) {
            (Some((low, high)), Value::Scalar(scalar)) => Value::Scalar(scalar.clamp(low, high)),
            _ => value,
        }
    }

    /// Layers a sampled value onto a scalar static value. Offsets add and
    /// saturate; factors multiply and round once, half up. A color sample or
    /// [`Self::Color`] leaves `base` unchanged.
    #[must_use]
    pub fn layer_scalar(self, base: i64, sampled: Value) -> i64 {
        let Value::Scalar(value) = sampled else {
            return base;
        };
        match self.layering() {
            Layering::Offset => base.saturating_add(value),
            Layering::Factor => {
                let identity = if matches!(self, Self::Opacity) {
                    OPACITY_IDENTITY
                } else {
                    SCALE_IDENTITY_BP
                };
                let product = i128::from(base) * i128::from(value);
                let identity = i128::from(identity);
                let rounded = (product + identity / 2).div_euclid(identity);
                i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX })
            }
            Layering::Override => base,
        }
    }

    /// Layers a sampled color over a static color: a color sample replaces
    /// it, anything else keeps it.
    #[must_use]
    pub const fn layer_color(self, base: Rgba, sampled: Value) -> Rgba {
        match (self, sampled) {
            (Self::Color, Value::Color(color)) => color,
            _ => base,
        }
    }
}
