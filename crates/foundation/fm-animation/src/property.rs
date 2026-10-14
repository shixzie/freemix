//! Animatable properties and how each layers onto a target's static value.

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
    pub const ALL: [Self; 11] = [
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
            Self::Color => Layering::Override,
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
    /// [`Self::Color`] overrides, so it has none.
    #[must_use]
    pub const fn identity(self) -> Option<Value> {
        match self {
            Self::ScaleX | Self::ScaleY => Some(Value::Scalar(SCALE_IDENTITY_BP)),
            Self::Opacity => Some(Value::Scalar(OPACITY_IDENTITY)),
            Self::Color => None,
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
