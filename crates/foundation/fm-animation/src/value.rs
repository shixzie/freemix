//! Keyframe values and their exact interpolation.

use crate::easing::{EASE_ONE, ease_between};

/// Straight (not premultiplied) 8-bit RGBA.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// A keyframe value: a scalar for every property but color.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Value {
    Scalar(i64),
    Color(Rgba),
}

/// A value an eased interval can blend between.
pub trait Animatable: Copy {
    /// The value `eased` (Q16, possibly outside `0..=EASE_ONE`) of the way
    /// from `from` to `to`.
    #[must_use]
    fn ease(from: Self, to: Self, eased: i32) -> Self;
}

impl Animatable for i64 {
    /// [`ease_between`]: truncates toward zero, saturating at `i64`.
    fn ease(from: Self, to: Self, eased: i32) -> Self {
        ease_between(from, to, eased)
    }
}

impl Animatable for Rgba {
    /// Per channel `from + (to - from) * eased`, rounded once, half up, and
    /// clamped to `0..=255` (an overshooting curve can leave the range).
    fn ease(from: Self, to: Self, eased: i32) -> Self {
        Self {
            r: ease_channel(from.r, to.r, eased),
            g: ease_channel(from.g, to.g, eased),
            b: ease_channel(from.b, to.b, eased),
            a: ease_channel(from.a, to.a, eased),
        }
    }
}

fn ease_channel(from: u8, to: u8, eased: i32) -> u8 {
    let one = i64::from(EASE_ONE);
    let start = i64::from(from) * one;
    let difference = i64::from(to) - i64::from(from);
    let value = (start + difference * i64::from(eased) + one / 2).div_euclid(one);
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

impl Animatable for Value {
    /// Blends like kinds; mismatched kinds hold `from`.
    fn ease(from: Self, to: Self, eased: i32) -> Self {
        match (from, to) {
            (Self::Scalar(from), Self::Scalar(to)) => Self::Scalar(i64::ease(from, to, eased)),
            (Self::Color(from), Self::Color(to)) => Self::Color(Rgba::ease(from, to, eased)),
            (from, _) => from,
        }
    }
}
