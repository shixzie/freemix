use core::{cmp::Ordering, fmt};

use crate::PlanError;

/// An exact, always-reduced rational number with a positive denominator.
///
/// All virtual-camera and layer-stack arithmetic uses this type so that every
/// frame plan is bit-for-bit reproducible across platforms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Rational {
    numer: i128,
    denom: i128,
}

impl Rational {
    pub const ZERO: Self = Self { numer: 0, denom: 1 };
    pub const ONE: Self = Self { numer: 1, denom: 1 };

    /// Builds a reduced rational.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ZeroDenominator`] when `denom` is zero and
    /// [`PlanError::ArithmeticOverflow`] when normalizing the sign overflows.
    pub fn new(numer: i128, denom: i128) -> Result<Self, PlanError> {
        if denom == 0 {
            return Err(PlanError::ZeroDenominator);
        }
        let (numer, denom) = if denom < 0 {
            (
                numer.checked_neg().ok_or(PlanError::ArithmeticOverflow)?,
                denom.checked_neg().ok_or(PlanError::ArithmeticOverflow)?,
            )
        } else {
            (numer, denom)
        };
        let divisor = gcd(numer.unsigned_abs(), denom.unsigned_abs());
        let divisor = i128::try_from(divisor).map_err(|_| PlanError::ArithmeticOverflow)?;
        Ok(Self {
            numer: numer / divisor,
            denom: denom / divisor,
        })
    }

    #[must_use]
    pub const fn integer(value: i64) -> Self {
        Self {
            numer: value as i128,
            denom: 1,
        }
    }

    /// `value / 1000`, the crate's integer control-point unit.
    #[must_use]
    pub fn permille(value: i64) -> Self {
        Self::new(i128::from(value), 1000).unwrap_or(Self::ZERO)
    }

    #[must_use]
    pub const fn numer(self) -> i128 {
        self.numer
    }

    #[must_use]
    pub const fn denom(self) -> i128 {
        self.denom
    }

    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on `i128` overflow.
    pub fn checked_add(self, other: Self) -> Result<Self, PlanError> {
        let left = mul(self.numer, other.denom)?;
        let right = mul(other.numer, self.denom)?;
        Self::new(
            left.checked_add(right)
                .ok_or(PlanError::ArithmeticOverflow)?,
            mul(self.denom, other.denom)?,
        )
    }

    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on `i128` overflow.
    pub fn checked_sub(self, other: Self) -> Result<Self, PlanError> {
        self.checked_add(other.checked_neg()?)
    }

    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on `i128` overflow.
    pub fn checked_mul(self, other: Self) -> Result<Self, PlanError> {
        Self::new(mul(self.numer, other.numer)?, mul(self.denom, other.denom)?)
    }

    /// # Errors
    ///
    /// Returns [`PlanError::ZeroDenominator`] when dividing by zero and
    /// [`PlanError::ArithmeticOverflow`] on `i128` overflow.
    pub fn checked_div(self, other: Self) -> Result<Self, PlanError> {
        Self::new(mul(self.numer, other.denom)?, mul(self.denom, other.numer)?)
    }

    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] for `i128::MIN` numerators.
    pub fn checked_neg(self) -> Result<Self, PlanError> {
        Ok(Self {
            numer: self
                .numer
                .checked_neg()
                .ok_or(PlanError::ArithmeticOverflow)?,
            denom: self.denom,
        })
    }

    /// Largest integer not greater than the value.
    #[must_use]
    pub const fn floor(self) -> i128 {
        self.numer.div_euclid(self.denom)
    }

    /// Nearest integer, with exact halves rounded toward positive infinity.
    #[must_use]
    pub const fn round_half_up(self) -> i128 {
        // floor((2n + d) / 2d); bounded inputs keep 2n + d far from overflow.
        (2 * self.numer + self.denom).div_euclid(2 * self.denom)
    }

    /// Rounds onto the grid `k / denom` using [`Self::round_half_up`].
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ZeroDenominator`] or [`PlanError::ArithmeticOverflow`].
    pub fn quantize(self, denom: i128) -> Result<Self, PlanError> {
        let scaled = self.checked_mul(Self::new(denom, 1)?)?;
        Self::new(scaled.round_half_up(), denom)
    }

    #[must_use]
    pub fn clamp_to(self, min: Self, max: Self) -> Self {
        if self < min {
            min
        } else if self > max {
            max
        } else {
            self
        }
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        // Denominators are positive, so cross-multiplication preserves order.
        // Values produced by this crate stay far below the overflow bound.
        (self.numer * other.denom).cmp(&(other.numer * self.denom))
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.denom == 1 {
            write!(formatter, "{}", self.numer)
        } else {
            write!(formatter, "{}/{}", self.numer, self.denom)
        }
    }
}

fn mul(left: i128, right: i128) -> Result<i128, PlanError> {
    left.checked_mul(right).ok_or(PlanError::ArithmeticOverflow)
}

const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left == 0 { 1 } else { left }
}
