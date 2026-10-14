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
        // floor(n/d + 1/2) without forming `2n + d`, which overflows for large
        // numerators. With `n = q*d + r` and `0 <= r < d`, the fractional part
        // `r/d` rounds up exactly when `r >= d - r`. A non-zero remainder
        // implies `d >= 2`, so `q + 1` cannot overflow.
        let quotient = self.numer.div_euclid(self.denom);
        let remainder = self.numer.rem_euclid(self.denom);
        if remainder != 0 && remainder >= self.denom - remainder {
            quotient + 1
        } else {
            quotient
        }
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
        // Cross-multiplication can overflow `i128` for large public inputs, so
        // compare integer parts first and then the fractional parts through
        // their continued-fraction expansion. Every intermediate stays within
        // `[0, denom)` and the loop terminates like Euclid's algorithm.
        let floor_order = self.floor().cmp(&other.floor());
        if floor_order != Ordering::Equal {
            return floor_order;
        }
        compare_fractions(
            self.numer.rem_euclid(self.denom),
            self.denom,
            other.numer.rem_euclid(other.denom),
            other.denom,
        )
    }
}

/// Compares `left_numer / left_denom` with `right_numer / right_denom` where
/// both fractions lie in `[0, 1)` and both denominators are positive.
fn compare_fractions(
    mut left_numer: i128,
    mut left_denom: i128,
    mut right_numer: i128,
    mut right_denom: i128,
) -> Ordering {
    // `reversed` tracks how many reciprocals have been taken: each one flips
    // the order of the remaining comparison.
    let mut reversed = false;
    loop {
        let order = match (left_numer == 0, right_numer == 0) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => {
                // a/b < c/d  <=>  b/a > d/c; compare the reciprocals' integer
                // parts, then recurse on their fractional parts.
                let left_whole = left_denom / left_numer;
                let right_whole = right_denom / right_numer;
                if left_whole == right_whole {
                    let next_left = (left_denom % left_numer, left_numer);
                    let next_right = (right_denom % right_numer, right_numer);
                    (left_numer, left_denom) = next_left;
                    (right_numer, right_denom) = next_right;
                    reversed = !reversed;
                    continue;
                }
                left_whole.cmp(&right_whole).reverse()
            }
        };
        return if reversed { order.reverse() } else { order };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_rounding_do_not_overflow_for_extreme_values() {
        let max = Rational::new(i128::MAX, 1).unwrap();
        let min = Rational::new(i128::MIN, 1).unwrap();
        let half = Rational::new(1, 2).unwrap();
        let near_max = Rational::new(i128::MAX, i128::MAX - 1).unwrap();
        let near_one = Rational::new(i128::MAX - 1, i128::MAX).unwrap();

        assert!(max > half);
        assert!(min < half);
        assert!(min < max);
        assert!(near_max > Rational::ONE);
        assert!(near_one < Rational::ONE);
        assert!(near_one < near_max);
        assert_eq!(max.cmp(&max), Ordering::Equal);
        assert_eq!(near_one.cmp(&near_one), Ordering::Equal);

        assert_eq!(max.round_half_up(), i128::MAX);
        assert_eq!(min.round_half_up(), i128::MIN);
        assert_eq!(
            Rational::new(i128::MAX, 2).unwrap().round_half_up(),
            i128::MAX / 2 + 1
        );
        assert_eq!(
            Rational::new(i128::MIN + 1, 2).unwrap().round_half_up(),
            i128::MIN / 2 + 1
        );
        assert_eq!(near_one.round_half_up(), 1);
        assert_eq!(near_max.round_half_up(), 1);
    }

    #[test]
    fn ordering_matches_cross_multiplication_for_small_values() {
        let mut values = Vec::new();
        for numer in -13..=13 {
            for denom in 1..=7 {
                values.push(Rational::new(numer, denom).unwrap());
            }
        }
        for left in &values {
            for right in &values {
                let expected = (left.numer * right.denom).cmp(&(right.numer * left.denom));
                assert_eq!(left.cmp(right), expected, "{left} vs {right}");
            }
            let expected = (2 * left.numer + left.denom).div_euclid(2 * left.denom);
            assert_eq!(left.round_half_up(), expected, "{left}");
        }
    }
}
