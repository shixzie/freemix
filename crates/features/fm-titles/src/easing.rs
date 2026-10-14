//! Fixed-point easing curves for designer timelines.
//!
//! Every curve maps a progress value in Q16 fixed point (`0..=EASE_ONE`,
//! where [`EASE_ONE`] is `1.0`) to an eased value in Q16. Evaluation is pure
//! integer arithmetic, so results are bit-identical on every host.
//!
//! # Precision
//!
//! * [`Easing::Linear`] and [`Easing::Step`] are exact.
//! * The quadratic and cubic polynomials are evaluated exactly in wide
//!   integers and rounded once, half up, to Q16: the result is within
//!   `2^-17` of the real curve. The `Out` variants are defined as
//!   `EASE_ONE - In(EASE_ONE - p)` and the `InOut` variants are point
//!   symmetric about the midpoint, both exactly.
//! * [`Easing::CubicBezier`] finds, by integer bisection, the smallest Q16
//!   parameter `t*` whose exact `x(t*)` is at least the progress, then
//!   evaluates `y(t*)` exactly and rounds once to Q16. The true parameter lies
//!   in `(t* - 2^-16, t*]` and `|y'(t)| <= 9` over the permitted control
//!   range, so the result is within `9.5 * 2^-16` (under `1.5e-4`) of the real
//!   curve. Bisection is fixed at 17 steps.

use core::fmt;

/// Q16 fixed-point `1.0`.
pub const EASE_ONE: u32 = 1 << 16;
/// Smallest permitted cubic-bezier control `y`, `-1.0` in Q16.
pub const MIN_BEZIER_Y: i32 = -(1 << 16);
/// Largest permitted cubic-bezier control `y`, `2.0` in Q16.
pub const MAX_BEZIER_Y: i32 = 2 << 16;

const ONE: u64 = 1 << 16;

/// A CSS-style cubic bezier from `(0, 0)` through `(x1, y1)` and `(x2, y2)` to
/// `(1, 1)`, with Q16 control points. `x1` and `x2` lie in `[0, 1]`, which
/// keeps `x(t)` monotonic; `y1` and `y2` lie in `[-1, 2]` to allow overshoot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CubicBezier {
    x1: u32,
    y1: i32,
    x2: u32,
    y2: i32,
}

/// Why a cubic bezier was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EasingError {
    /// A control `x` lies outside `0..=EASE_ONE`.
    ControlXOutOfRange(u32),
    /// A control `y` lies outside `MIN_BEZIER_Y..=MAX_BEZIER_Y`.
    ControlYOutOfRange(i32),
}

impl fmt::Display for EasingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ControlXOutOfRange(value) => {
                write!(formatter, "bezier control x {value} is outside 0..=65536")
            }
            Self::ControlYOutOfRange(value) => {
                write!(
                    formatter,
                    "bezier control y {value} is outside -65536..=131072"
                )
            }
        }
    }
}

impl std::error::Error for EasingError {}

impl CubicBezier {
    /// Creates a curve from Q16 control points.
    ///
    /// # Errors
    ///
    /// Returns [`EasingError`] when a control point is out of range.
    pub const fn new(x1: u32, y1: i32, x2: u32, y2: i32) -> Result<Self, EasingError> {
        if x1 > EASE_ONE {
            return Err(EasingError::ControlXOutOfRange(x1));
        }
        if x2 > EASE_ONE {
            return Err(EasingError::ControlXOutOfRange(x2));
        }
        if y1 < MIN_BEZIER_Y || y1 > MAX_BEZIER_Y {
            return Err(EasingError::ControlYOutOfRange(y1));
        }
        if y2 < MIN_BEZIER_Y || y2 > MAX_BEZIER_Y {
            return Err(EasingError::ControlYOutOfRange(y2));
        }
        Ok(Self { x1, y1, x2, y2 })
    }

    /// The Q16 control points as `(x1, y1, x2, y2)`.
    #[must_use]
    pub const fn control_points(self) -> (u32, i32, u32, i32) {
        (self.x1, self.y1, self.x2, self.y2)
    }

    /// `x(t)` scaled by `2^64` (Q64) for a Q16 parameter `t`; exact.
    fn x_q64(self, t: u64) -> u128 {
        let t = u128::from(t);
        let u = u128::from(ONE) - t;
        let one = u128::from(ONE);
        3 * u * u * t * u128::from(self.x1) + 3 * u * t * t * u128::from(self.x2) + t * t * t * one
    }

    /// `y(t)` scaled by `2^64` (Q64) for a Q16 parameter `t`; exact.
    fn y_q64(self, t: u64) -> i128 {
        let t = i128::from(t);
        let one = i128::from(ONE);
        let u = one - t;
        3 * u * u * t * i128::from(self.y1) + 3 * u * t * t * i128::from(self.y2) + t * t * t * one
    }

    fn apply(self, progress: u64) -> i32 {
        let target = u128::from(progress) << 48;
        // Smallest t in [0, ONE] with x(t) >= progress. x(ONE) == ONE^4 >=
        // target, so the search always terminates inside the range.
        let (mut low, mut high) = (0_u64, ONE);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.x_q64(middle) >= target {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        let y = self.y_q64(low);
        let rounded = (y + (1_i128 << 47)).div_euclid(1_i128 << 48);
        i32::try_from(rounded).unwrap_or(if rounded < 0 { i32::MIN } else { i32::MAX })
    }
}

/// An easing curve applied to the interval from one keyframe to the next.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Easing {
    #[default]
    Linear,
    /// Holds the starting value for the whole interval and jumps to the next
    /// keyframe's value exactly at that keyframe (CSS `step-end`).
    Step,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    CubicBezier(CubicBezier),
}

impl Easing {
    /// Maps Q16 progress to a Q16 eased value. Progress above [`EASE_ONE`] is
    /// clamped. Only [`Self::CubicBezier`] can leave `0..=EASE_ONE`.
    #[must_use]
    pub fn apply(self, progress: u32) -> i32 {
        let p = u64::from(progress.min(EASE_ONE));
        let value = match self {
            Self::Linear => p,
            Self::Step => {
                if p >= ONE {
                    ONE
                } else {
                    0
                }
            }
            Self::QuadIn => quad_in(p),
            Self::QuadOut => ONE - quad_in(ONE - p),
            Self::QuadInOut => {
                if 2 * p < ONE {
                    round_div(2 * p * p, ONE)
                } else {
                    let q = ONE - p;
                    ONE - round_div(2 * q * q, ONE)
                }
            }
            Self::CubicIn => cubic_in(p),
            Self::CubicOut => ONE - cubic_in(ONE - p),
            Self::CubicInOut => {
                if 2 * p < ONE {
                    round_div(4 * p * p * p, ONE * ONE)
                } else {
                    let q = ONE - p;
                    ONE - round_div(4 * q * q * q, ONE * ONE)
                }
            }
            Self::CubicBezier(curve) => return curve.apply(p),
        };
        // Every polynomial branch stays inside 0..=ONE.
        i32::try_from(value).unwrap_or(i32::MAX)
    }
}

fn quad_in(p: u64) -> u64 {
    round_div(p * p, ONE)
}

fn cubic_in(p: u64) -> u64 {
    round_div(p * p * p, ONE * ONE)
}

const fn round_div(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator / 2) / denominator
}

/// Q16 progress `floor(elapsed * EASE_ONE / duration)`; [`EASE_ONE`] when
/// `elapsed >= duration` or `duration == 0`.
#[must_use]
pub fn progress_q16(elapsed: u64, duration: u64) -> u32 {
    if duration == 0 || elapsed >= duration {
        return EASE_ONE;
    }
    let value = u128::from(elapsed) * u128::from(ONE) / u128::from(duration);
    u32::try_from(value).unwrap_or(EASE_ONE)
}

/// `from + trunc((to - from) * eased / EASE_ONE)`, saturating at the `i64`
/// range. `eased` is the output of [`Easing::apply`].
#[must_use]
pub fn ease_between(from: i64, to: i64, eased: i32) -> i64 {
    let difference = i128::from(to) - i128::from(from);
    let offset = difference * i128::from(eased) / i128::from(ONE);
    let value = i128::from(from) + offset;
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}
