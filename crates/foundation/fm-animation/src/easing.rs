//! Fixed-point easing curves for designer timelines.
//!
//! Every curve maps a progress value in Q16 fixed point (`0..=EASE_ONE`,
//! where [`EASE_ONE`] is `1.0`) to an eased value in Q16. Evaluation is pure
//! integer arithmetic, so results are bit-identical on every host.
//!
//! # Precision
//!
//! * [`Easing::Linear`], [`Easing::Step`], and [`Easing::Hold`] are exact.
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
//! * The Back curves (`c1 = 1.70158`, `c2 = c1 * 1.525`, the conventional
//!   constants, held as exact decimal rationals) and the Bounce curves
//!   (`n1 = 7.5625 = (11/4)^2`, `d1 = 2.75`, so every branch is a dyadic
//!   square) are evaluated as exact rationals in wide integers and rounded
//!   once, half up: within `2^-17` of the real curve. Back overshoots to
//!   about `-0.1` / `1.1`, inside `[-1, 2]`; Bounce touches `1.0` exactly at
//!   each join, so it is continuous there.
//! * The Sine curves evaluate `1 - cos(x)` by its Taylor series to the
//!   `x^22` term in Q60 integers with `x <= pi/2` (truncation below `2^-63`,
//!   accumulated intermediate flooring below `2^-55`) and round once, half up,
//!   to Q16: within `2^-17 + 2^-50` of the real curve.
//! * Every `Out` variant is `EASE_ONE - In(EASE_ONE - p)` and every `InOut`
//!   variant is point symmetric about the midpoint, both exactly, so all of
//!   them hit `0` and [`EASE_ONE`] exactly at the endpoints.

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
    /// Identical to [`Self::Step`]: holds the starting value until the next
    /// keyframe. The editor's name for a hold key.
    Hold,
    SineIn,
    SineOut,
    SineInOut,
    /// Anticipates below `0` before rising to `1`.
    BackIn,
    /// Overshoots above `1` before settling.
    BackOut,
    BackInOut,
    BounceIn,
    BounceOut,
    BounceInOut,
}

impl Easing {
    /// Maps Q16 progress to a Q16 eased value. Progress above [`EASE_ONE`] is
    /// clamped. Only [`Self::CubicBezier`] and the Back curves can leave
    /// `0..=EASE_ONE`.
    #[must_use]
    pub fn apply(self, progress: u32) -> i32 {
        let p = u64::from(progress.min(EASE_ONE));
        let value = match self {
            Self::Linear => p,
            Self::Step | Self::Hold => {
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
            Self::SineIn => sine_in(p),
            Self::SineOut => ONE - sine_in(ONE - p),
            Self::SineInOut => {
                if 2 * p < ONE {
                    sine_in_out_low(p)
                } else {
                    ONE - sine_in_out_low(ONE - p)
                }
            }
            Self::BackIn => return back_in(p),
            Self::BackOut => return ONE_I32 - back_in(ONE - p),
            Self::BackInOut => {
                return if 2 * p < ONE {
                    back_in_out_low(p)
                } else {
                    ONE_I32 - back_in_out_low(ONE - p)
                };
            }
            Self::BounceIn => ONE - bounce_out(ONE - p),
            Self::BounceOut => bounce_out(p),
            Self::BounceInOut => {
                if 2 * p < ONE {
                    bounce_in_out_low(p)
                } else {
                    ONE - bounce_in_out_low(ONE - p)
                }
            }
        };
        // Every polynomial branch stays inside 0..=ONE.
        i32::try_from(value).unwrap_or(i32::MAX)
    }
}

const ONE_I32: i32 = 1 << 16;

/// `pi / 2` in Q60, rounded to nearest.
const HALF_PI_Q60: u128 = 1_811_004_864_519_280_711;
const Q60: u32 = 60;

/// `1 - cos(x)` in Q60 for a Q60 angle `0 <= x <= pi/2`, by the alternating
/// Taylor series `x^2/2! - x^4/4! + ...` through the `x^22` term.
fn one_minus_cos_q60(angle: u128) -> u128 {
    let square = (angle * angle) >> Q60;
    let mut term = square / 2;
    let mut sum = i128::try_from(term).unwrap_or(i128::MAX);
    for k in 2..=11_u128 {
        term = ((term * square) >> Q60) / ((2 * k - 1) * (2 * k));
        let signed = i128::try_from(term).unwrap_or(i128::MAX);
        if k % 2 == 0 {
            sum -= signed;
        } else {
            sum += signed;
        }
    }
    u128::try_from(sum).unwrap_or(0)
}

/// Rounds a non-negative Q60 value to Q16 after dividing by `2^extra`.
fn round_q60(value: u128, extra: u32) -> u64 {
    let shift = Q60 - 16 + extra;
    u64::try_from((value + (1 << (shift - 1))) >> shift).unwrap_or(ONE)
}

/// `1 - cos(pi/2 * p)`.
fn sine_in(p: u64) -> u64 {
    let angle = HALF_PI_Q60 * u128::from(p) / u128::from(ONE);
    round_q60(one_minus_cos_q60(angle), 0).min(ONE)
}

/// `(1 - cos(pi * p)) / 2` for `p < 1/2`.
fn sine_in_out_low(p: u64) -> u64 {
    let angle = HALF_PI_Q60 * u128::from(2 * p) / u128::from(ONE);
    round_q60(one_minus_cos_q60(angle), 1)
}

/// `c1 = 1.70158` as `BACK_C1 / BACK_DENOMINATOR`.
const BACK_C1: i128 = 170_158;
const BACK_DENOMINATOR: i128 = 100_000;
/// `c2 = c1 * 1.525 = 2.5949095` as `BACK_C2 / BACK_C2_DENOMINATOR`.
const BACK_C2: i128 = 25_949_095;
const BACK_C2_DENOMINATOR: i128 = 10_000_000;

/// `round_half_up(numerator / denominator)` for a positive denominator.
fn round_signed(numerator: i128, denominator: i128) -> i32 {
    let rounded = (numerator + denominator / 2).div_euclid(denominator);
    i32::try_from(rounded).unwrap_or(if rounded < 0 { i32::MIN } else { i32::MAX })
}

/// `(c1 + 1) p^3 - c1 p^2`, exact then rounded once.
fn back_in(p: u64) -> i32 {
    let p = i128::from(p);
    let one = i128::from(ONE);
    // Q16 result = p^2 * ((c1 + 1) p - c1 * ONE) / (DEN * ONE^2).
    let numerator = p * p * ((BACK_C1 + BACK_DENOMINATOR) * p - BACK_C1 * one);
    round_signed(numerator, BACK_DENOMINATOR * one * one)
}

/// `(2p)^2 ((c2 + 1) 2p - c2) / 2` for `p < 1/2`, exact then rounded once.
fn back_in_out_low(p: u64) -> i32 {
    let u = i128::from(2 * p);
    let one = i128::from(ONE);
    let numerator = u * u * ((BACK_C2 + BACK_C2_DENOMINATOR) * u - BACK_C2 * one);
    round_signed(numerator, 2 * BACK_C2_DENOMINATOR * one * one)
}

/// Bounce-out scaled by `2^36` for Q16 progress `p`: with `d1 = 11/4` each
/// branch is `(11 p / 2^18 - a)^2 + c`, so `(11 p - a 2^18)^2 + c 2^36` is
/// exact.
fn bounce_out_q36(p: u64) -> u64 {
    let eleven_p = 11 * p;
    let (a, c) = if eleven_p < 4 * ONE {
        (0, 0)
    } else if eleven_p < 8 * ONE {
        // a = 3/2, c = 3/4
        (3 << 17, 3 << 34)
    } else if eleven_p < 10 * ONE {
        // a = 9/4, c = 15/16
        (9 << 16, 15 << 32)
    } else {
        // a = 21/8, c = 63/64
        (21 << 15, 63 << 30)
    };
    let distance = eleven_p.abs_diff(a);
    distance * distance + c
}

fn bounce_out(p: u64) -> u64 {
    (bounce_out_q36(p) + (1 << 19)) >> 20
}

/// `(1 - bounce_out(1 - 2p)) / 2` for `p < 1/2`, rounded once.
fn bounce_in_out_low(p: u64) -> u64 {
    let value = (1_u64 << 36) - bounce_out_q36(ONE - 2 * p);
    (value + (1 << 20)) >> 21
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

/// A named easing the editor offers, with a stable string id used by the UI
/// and on the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EasingPreset {
    Linear,
    Hold,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    SineIn,
    SineOut,
    SineInOut,
    BackIn,
    BackOut,
    BackInOut,
    BounceIn,
    BounceOut,
    BounceInOut,
}

impl EasingPreset {
    /// Every preset, in menu order.
    pub const ALL: [Self; 17] = [
        Self::Linear,
        Self::Hold,
        Self::QuadIn,
        Self::QuadOut,
        Self::QuadInOut,
        Self::CubicIn,
        Self::CubicOut,
        Self::CubicInOut,
        Self::SineIn,
        Self::SineOut,
        Self::SineInOut,
        Self::BackIn,
        Self::BackOut,
        Self::BackInOut,
        Self::BounceIn,
        Self::BounceOut,
        Self::BounceInOut,
    ];

    /// The stable id. Never renamed once shipped.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Hold => "hold",
            Self::QuadIn => "quad-in",
            Self::QuadOut => "quad-out",
            Self::QuadInOut => "quad-in-out",
            Self::CubicIn => "cubic-in",
            Self::CubicOut => "cubic-out",
            Self::CubicInOut => "cubic-in-out",
            Self::SineIn => "sine-in",
            Self::SineOut => "sine-out",
            Self::SineInOut => "sine-in-out",
            Self::BackIn => "back-in",
            Self::BackOut => "back-out",
            Self::BackInOut => "back-in-out",
            Self::BounceIn => "bounce-in",
            Self::BounceOut => "bounce-out",
            Self::BounceInOut => "bounce-in-out",
        }
    }

    /// The preset with this id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    /// The exact curve this preset names.
    #[must_use]
    pub const fn easing(self) -> Easing {
        match self {
            Self::Linear => Easing::Linear,
            Self::Hold => Easing::Hold,
            Self::QuadIn => Easing::QuadIn,
            Self::QuadOut => Easing::QuadOut,
            Self::QuadInOut => Easing::QuadInOut,
            Self::CubicIn => Easing::CubicIn,
            Self::CubicOut => Easing::CubicOut,
            Self::CubicInOut => Easing::CubicInOut,
            Self::SineIn => Easing::SineIn,
            Self::SineOut => Easing::SineOut,
            Self::SineInOut => Easing::SineInOut,
            Self::BackIn => Easing::BackIn,
            Self::BackOut => Easing::BackOut,
            Self::BackInOut => Easing::BackInOut,
            Self::BounceIn => Easing::BounceIn,
            Self::BounceOut => Easing::BounceOut,
            Self::BounceInOut => Easing::BounceInOut,
        }
    }

    /// The preset naming `easing`; [`Easing::Step`] reads as [`Self::Hold`].
    /// A custom [`Easing::CubicBezier`] has none.
    #[must_use]
    pub fn from_easing(easing: Easing) -> Option<Self> {
        match easing {
            Easing::Step => Some(Self::Hold),
            Easing::CubicBezier(_) => None,
            other => Self::ALL
                .into_iter()
                .find(|preset| preset.easing() == other),
        }
    }

    /// A cubic bezier approximating this preset, used to seed the curve
    /// editor. The preset itself stays exact until the user edits the curve.
    /// Linear's controls sit on the diagonal at thirds, so `x(t) == y(t)` and
    /// the seed is linear within one Q16 step; the polynomial and Sine seeds are
    /// the conventional CSS approximations; Back seeds overshoot like the
    /// real curve; a bezier cannot bounce, so Bounce seeds are the closest
    /// single-arc shape. [`Self::Hold`] has no bezier form.
    #[must_use]
    pub fn to_bezier(self) -> Option<CubicBezier> {
        let thousandths = match self {
            Self::Hold => return None,
            Self::Linear => return CubicBezier::new(21_845, 21_845, 43_691, 43_691).ok(),
            Self::QuadIn => (110, 0, 500, 0),
            Self::QuadOut => (500, 1_000, 890, 1_000),
            Self::QuadInOut => (450, 0, 550, 1_000),
            Self::CubicIn => (320, 0, 670, 0),
            Self::CubicOut => (330, 1_000, 680, 1_000),
            Self::CubicInOut => (650, 0, 350, 1_000),
            Self::SineIn => (120, 0, 390, 0),
            Self::SineOut => (610, 1_000, 880, 1_000),
            Self::SineInOut => (370, 0, 630, 1_000),
            Self::BackIn => (360, 0, 660, -560),
            Self::BackOut => (340, 1_560, 640, 1_000),
            Self::BackInOut => (680, -600, 320, 1_600),
            Self::BounceIn => (700, 0, 840, 0),
            Self::BounceOut => (160, 1_000, 300, 1_000),
            Self::BounceInOut => (700, 0, 300, 1_000),
        };
        let (x1, y1, x2, y2) = thousandths;
        CubicBezier::new(
            thousandths_to_q16_x(x1),
            thousandths_to_q16_y(y1),
            thousandths_to_q16_x(x2),
            thousandths_to_q16_y(y2),
        )
        .ok()
    }
}

/// `round_half_up(value / 1000)` in Q16.
fn thousandths_to_q16_y(value: i32) -> i32 {
    let scaled = i64::from(value) * (1 << 16);
    i32::try_from((scaled + 500).div_euclid(1_000)).unwrap_or(0)
}

fn thousandths_to_q16_x(value: u32) -> u32 {
    let scaled = u64::from(value) * (1 << 16);
    u32::try_from((scaled + 500) / 1_000).unwrap_or(0)
}
