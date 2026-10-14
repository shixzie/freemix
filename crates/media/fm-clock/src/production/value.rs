use core::fmt;

use crate::ClockDuration;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// A signed production-clock value in nanoseconds.
///
/// Values are bounded to `±(100_000 h - 1 ns)` so every arithmetic step and
/// every display fits comfortably in `i64`/`i128` and hours never exceed five
/// digits.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClockValue(i64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockValueError {
    /// The value is outside `[ClockValue::MIN, ClockValue::MAX]`.
    OutOfRange,
    /// The text is not a valid clock value. The payload names the problem.
    Syntax(&'static str),
}

impl fmt::Display for ClockValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange => formatter.write_str("clock value is outside the supported range"),
            Self::Syntax(reason) => write!(formatter, "invalid clock value: {reason}"),
        }
    }
}

impl std::error::Error for ClockValueError {}

impl ClockValue {
    pub const ZERO: Self = Self(0);
    /// Largest supported value: 99999:59:59.999999999.
    pub const MAX: Self = Self(100_000 * 3_600 * NANOS_PER_SECOND - 1);
    /// Smallest supported value: the negation of [`ClockValue::MAX`].
    pub const MIN: Self = Self(-Self::MAX.0);

    /// Creates a value from signed nanoseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ClockValueError::OutOfRange`] outside the supported range.
    pub const fn from_nanos(nanos: i64) -> Result<Self, ClockValueError> {
        if nanos < Self::MIN.0 || nanos > Self::MAX.0 {
            Err(ClockValueError::OutOfRange)
        } else {
            Ok(Self(nanos))
        }
    }

    /// Creates a value from signed milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ClockValueError::OutOfRange`] outside the supported range.
    pub const fn from_millis(millis: i64) -> Result<Self, ClockValueError> {
        match millis.checked_mul(NANOS_PER_MILLI) {
            Some(nanos) => Self::from_nanos(nanos),
            None => Err(ClockValueError::OutOfRange),
        }
    }

    /// Creates a value from signed whole seconds.
    ///
    /// # Errors
    ///
    /// Returns [`ClockValueError::OutOfRange`] outside the supported range.
    pub const fn from_secs(seconds: i64) -> Result<Self, ClockValueError> {
        match seconds.checked_mul(NANOS_PER_SECOND) {
            Some(nanos) => Self::from_nanos(nanos),
            None => Err(ClockValueError::OutOfRange),
        }
    }

    /// Creates a non-negative value from a monotonic duration.
    ///
    /// # Errors
    ///
    /// Returns [`ClockValueError::OutOfRange`] above [`ClockValue::MAX`].
    pub fn from_duration(duration: ClockDuration) -> Result<Self, ClockValueError> {
        i64::try_from(duration.as_nanos())
            .map_err(|_| ClockValueError::OutOfRange)
            .and_then(Self::from_nanos)
    }

    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// Whole milliseconds, truncated toward zero.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0 / NANOS_PER_MILLI
    }

    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Adds two values, saturating at the supported range.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        // Both operands are within ±MAX, so the raw sum cannot overflow i64.
        Self::clamp_raw(self.0 + other.0)
    }

    /// Subtracts two values, saturating at the supported range.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self::clamp_raw(self.0 - other.0)
    }

    pub(crate) const fn clamp_raw(nanos: i64) -> Self {
        if nanos > Self::MAX.0 {
            Self::MAX
        } else if nanos < Self::MIN.0 {
            Self::MIN
        } else {
            Self(nanos)
        }
    }

    /// Converts a monotonic duration, saturating at [`ClockValue::MAX`].
    pub(crate) fn saturating_from_duration(duration: ClockDuration) -> Self {
        Self::from_duration(duration).unwrap_or(Self::MAX)
    }
}

/// Parses the strict text form of a clock value.
///
/// Accepted forms, with an optional leading `-`:
///
/// - `S` — whole seconds, any number of digits (`90` is 90 seconds);
/// - `M:SS` — minutes (unbounded) and two-digit seconds below 60;
/// - `H:MM:SS` — hours (unbounded), two-digit minutes and seconds below 60.
///
/// The last component may carry a fraction of 1 to 9 digits after `.`.
/// No whitespace, signs inside components, or empty components are accepted.
///
/// # Errors
///
/// Returns [`ClockValueError::Syntax`] for malformed text and
/// [`ClockValueError::OutOfRange`] for values beyond [`ClockValue::MAX`].
pub fn parse_clock_value(text: &str) -> Result<ClockValue, ClockValueError> {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if body.is_empty() {
        return Err(ClockValueError::Syntax("empty value"));
    }
    let (whole, fraction) = match body.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (body, None),
    };
    let parts: Vec<&str> = whole.split(':').collect();
    if parts.len() > 3 {
        return Err(ClockValueError::Syntax("too many components"));
    }
    let mut seconds: i64 = 0;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(ClockValueError::Syntax("empty component"));
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ClockValueError::Syntax("components must be decimal digits"));
        }
        let leading = index == 0;
        if !leading && part.len() != 2 {
            return Err(ClockValueError::Syntax(
                "minutes and seconds after a colon must have two digits",
            ));
        }
        if leading && part.len() > 12 {
            return Err(ClockValueError::OutOfRange);
        }
        let value: i64 = part
            .parse()
            .map_err(|_| ClockValueError::Syntax("components must be decimal digits"))?;
        if !leading && value >= 60 {
            return Err(ClockValueError::Syntax(
                "minutes and seconds after a colon must be below 60",
            ));
        }
        seconds = seconds
            .checked_mul(60)
            .and_then(|scaled| scaled.checked_add(value))
            .ok_or(ClockValueError::OutOfRange)?;
    }
    let mut nanos = seconds
        .checked_mul(NANOS_PER_SECOND)
        .ok_or(ClockValueError::OutOfRange)?;
    if let Some(fraction) = fraction {
        if fraction.is_empty() || fraction.len() > 9 {
            return Err(ClockValueError::Syntax(
                "a fraction must have between one and nine digits",
            ));
        }
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ClockValueError::Syntax("fraction must be decimal digits"));
        }
        let mut fraction_nanos: i64 = 0;
        for index in 0..9 {
            let digit = fraction
                .as_bytes()
                .get(index)
                .map_or(0, |byte| i64::from(byte - b'0'));
            fraction_nanos = fraction_nanos * 10 + digit;
        }
        nanos = nanos
            .checked_add(fraction_nanos)
            .ok_or(ClockValueError::OutOfRange)?;
    }
    ClockValue::from_nanos(if negative { -nanos } else { nanos })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_is_symmetric_and_enforced() {
        assert_eq!(ClockValue::MIN.as_nanos(), -ClockValue::MAX.as_nanos());
        assert!(ClockValue::from_nanos(ClockValue::MAX.as_nanos()).is_ok());
        assert_eq!(
            ClockValue::from_nanos(ClockValue::MAX.as_nanos() + 1),
            Err(ClockValueError::OutOfRange)
        );
        assert_eq!(
            ClockValue::from_secs(i64::MAX),
            Err(ClockValueError::OutOfRange)
        );
        assert_eq!(
            ClockValue::MAX.saturating_add(ClockValue::MAX),
            ClockValue::MAX
        );
        assert_eq!(
            ClockValue::MIN.saturating_sub(ClockValue::MAX),
            ClockValue::MIN
        );
    }

    #[test]
    fn millis_truncate_toward_zero() {
        assert_eq!(ClockValue::from_nanos(-1_999_999).unwrap().as_millis(), -1);
        assert_eq!(ClockValue::from_nanos(1_999_999).unwrap().as_millis(), 1);
    }
}
