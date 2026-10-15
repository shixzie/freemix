//! Deterministic fixed-point trigonometry for rotation renderers.
//!
//! Titles (`fm-titles`), scene posing (`fm-model`), and the compositor
//! (`fm-compositor`) rotate by the same integer sine and cosine, so every
//! renderer turns a picture identically on every host.

/// A full turn in millidegrees, the unit of [`crate::Property::Rotation`].
pub const FULL_TURN_MILLIDEGREES: i64 = 360_000;

/// Q30 fixed-point `1.0`.
pub const TRIG_ONE: i64 = 1 << 30;

/// `round(pi * 2^60)`.
const PI_Q60: i128 = 3_622_009_729_038_561_421;

/// `(sin, cos)` of `millidegrees` in Q30, deterministic on every host.
///
/// The angle is reduced to a quadrant and a remainder `r < 90` degrees;
/// multiples of 90 degrees are exact (`0` and `±2^30`). Otherwise both are
/// Taylor series of `x = r * pi / 180` evaluated in Q60 `i128` to the
/// `x^25`/`x^24` term (truncation below `2^-80` for `x <= pi/2`) and rounded
/// once, half up, to Q30, so each is within `2^-31 + 2^-55` of the real
/// value.
#[must_use]
pub fn sin_cos_q30(millidegrees: i64) -> (i64, i64) {
    let angle = millidegrees.rem_euclid(FULL_TURN_MILLIDEGREES);
    let quadrant = angle / 90_000;
    let remainder = angle % 90_000;
    let (sin, cos) = if remainder == 0 {
        (0, TRIG_ONE)
    } else {
        let one = 1_i128 << 60;
        let x = i128::from(remainder) * PI_Q60 / 180_000;
        let square = (x * x) >> 60;
        let (mut sin, mut sin_term) = (x, x);
        let (mut cos, mut cos_term) = (one, one);
        for k in 1..=12_i128 {
            sin_term = -((sin_term * square) >> 60) / ((2 * k) * (2 * k + 1));
            cos_term = -((cos_term * square) >> 60) / ((2 * k - 1) * (2 * k));
            sin += sin_term;
            cos += cos_term;
        }
        let round =
            |value: i128| -> i64 { i64::try_from((value + (1 << 29)) >> 30).unwrap_or(TRIG_ONE) };
        (round(sin), round(cos))
    };
    match quadrant {
        0 => (sin, cos),
        1 => (cos, -sin),
        2 => (-sin, -cos),
        _ => (-cos, sin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_are_exact_and_others_are_close() {
        assert_eq!(sin_cos_q30(0), (0, TRIG_ONE));
        assert_eq!(sin_cos_q30(90_000), (TRIG_ONE, 0));
        assert_eq!(sin_cos_q30(-90_000), (-TRIG_ONE, 0));
        assert_eq!(sin_cos_q30(180_000), (0, -TRIG_ONE));
        for millidegrees in [1, 30_000, 45_000, 123_456, -77_777] {
            let (sin, cos) = sin_cos_q30(millidegrees);
            #[allow(clippy::cast_precision_loss)]
            let radians = (millidegrees as f64 / 1000.0).to_radians();
            #[allow(clippy::cast_precision_loss)]
            let scale = TRIG_ONE as f64;
            #[allow(clippy::cast_precision_loss)]
            {
                assert!((sin as f64 / scale - radians.sin()).abs() < 1e-9);
                assert!((cos as f64 / scale - radians.cos()).abs() < 1e-9);
            }
        }
    }
}
