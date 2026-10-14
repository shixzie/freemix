//! Deterministic number and date/time formatting for title text fields.
//!
//! Numbers are formatted from exact [`Decimal`] values with integer
//! arithmetic only. Date/time values come from ISO-8601 text or Unix seconds
//! and are rendered with a caller-supplied fixed UTC offset; there is no
//! system clock, locale, or time-zone database.

use std::{fmt, str::FromStr};

use crate::{DataValue, Decimal, ValueType};

/// A formatter configuration is invalid or a value could not be formatted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    DecimalsOutOfRange {
        limit: u32,
        actual: u32,
    },
    InvalidSeparator(char),
    SeparatorConflict(char),
    AffixTooLong {
        limit: usize,
        actual: usize,
    },
    AffixControlCharacter,
    PatternTooLong {
        limit: usize,
        actual: usize,
    },
    TooManyDirectives {
        limit: usize,
    },
    UnknownDirective(char),
    DanglingPercent,
    OffsetOutOfRange {
        minutes: i32,
    },
    NotANumber(String),
    UnsupportedValue {
        actual: ValueType,
    },
    NumberOutOfRange,
    InvalidDateTime(String),
    /// ISO-8601 text without a UTC offset and no naive offset configured.
    MissingOffset(String),
    DateOutOfRange,
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecimalsOutOfRange { limit, actual } => {
                write!(formatter, "{actual} decimals exceed the limit of {limit}")
            }
            Self::InvalidSeparator(separator) => {
                write!(formatter, "`{separator}` cannot be a separator")
            }
            Self::SeparatorConflict(separator) => write!(
                formatter,
                "`{separator}` is both the thousands and decimal separator"
            ),
            Self::AffixTooLong { limit, actual } => {
                write!(formatter, "affix is {actual} bytes; limit is {limit}")
            }
            Self::AffixControlCharacter => {
                formatter.write_str("affix contains a control character")
            }
            Self::PatternTooLong { limit, actual } => {
                write!(formatter, "pattern is {actual} bytes; limit is {limit}")
            }
            Self::TooManyDirectives { limit } => {
                write!(formatter, "pattern exceeds {limit} directives")
            }
            Self::UnknownDirective(directive) => {
                write!(formatter, "`%{directive}` is not a supported directive")
            }
            Self::DanglingPercent => formatter.write_str("pattern ends with a lone `%`"),
            Self::OffsetOutOfRange { minutes } => {
                write!(formatter, "UTC offset of {minutes} minutes is out of range")
            }
            Self::NotANumber(text) => write!(formatter, "`{text}` is not a number"),
            Self::UnsupportedValue { actual } => {
                write!(formatter, "cannot format a {actual:?} value")
            }
            Self::NumberOutOfRange => formatter.write_str("number is out of range"),
            Self::InvalidDateTime(text) => {
                write!(formatter, "`{text}` is not a supported ISO-8601 date/time")
            }
            Self::MissingOffset(text) => {
                write!(
                    formatter,
                    "`{text}` has no UTC offset and none is configured"
                )
            }
            Self::DateOutOfRange => formatter.write_str("date is outside years 0000-9999"),
        }
    }
}

impl std::error::Error for FormatError {}

// ---------------------------------------------------------------- numbers

/// How digits beyond the configured decimals are removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DecimalRounding {
    #[default]
    HalfAwayFromZero,
    /// Banker's rounding: ties go to the even neighbour.
    HalfEven,
    /// Rounds toward zero.
    Truncate,
}

/// How the sign is rendered. Zero (after rounding) is never signed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SignStyle {
    /// `-` for negatives only.
    #[default]
    NegativeOnly,
    /// `+` for positives and `-` for negatives.
    Always,
    /// Negatives wrapped in parentheses, including prefix and suffix.
    Parentheses,
}

/// Fixed-decimal number formatting with exact rounding.
///
/// Output layout: `sign prefix digits suffix` (or `(prefix digits suffix)`
/// for [`SignStyle::Parentheses`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumberFormat {
    pub decimals: u32,
    pub rounding: DecimalRounding,
    pub thousands_separator: Option<char>,
    pub decimal_separator: char,
    pub sign: SignStyle,
    pub prefix: String,
    pub suffix: String,
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self {
            decimals: 0,
            rounding: DecimalRounding::HalfAwayFromZero,
            thousands_separator: None,
            decimal_separator: '.',
            sign: SignStyle::NegativeOnly,
            prefix: String::new(),
            suffix: String::new(),
        }
    }
}

impl NumberFormat {
    pub const MAX_DECIMALS: u32 = Decimal::MAX_SCALE;
    pub const MAX_AFFIX_BYTES: usize = 64;

    #[must_use]
    pub fn fixed(decimals: u32) -> Self {
        Self {
            decimals,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn with_rounding(mut self, rounding: DecimalRounding) -> Self {
        self.rounding = rounding;
        self
    }

    #[must_use]
    pub const fn with_thousands(mut self, separator: char) -> Self {
        self.thousands_separator = Some(separator);
        self
    }

    #[must_use]
    pub const fn with_decimal_separator(mut self, separator: char) -> Self {
        self.decimal_separator = separator;
        self
    }

    #[must_use]
    pub const fn with_sign(mut self, sign: SignStyle) -> Self {
        self.sign = sign;
        self
    }

    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    #[must_use]
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = suffix.into();
        self
    }

    /// Checks decimals, separators (not digits, signs, parentheses, or
    /// control characters, and distinct), and bounded affixes.
    ///
    /// # Errors
    ///
    /// Returns the first configuration [`FormatError`].
    pub fn validate(&self) -> Result<(), FormatError> {
        if self.decimals > Self::MAX_DECIMALS {
            return Err(FormatError::DecimalsOutOfRange {
                limit: Self::MAX_DECIMALS,
                actual: self.decimals,
            });
        }
        let bad_separator =
            |c: char| c.is_ascii_digit() || c.is_control() || matches!(c, '-' | '+' | '(' | ')');
        if bad_separator(self.decimal_separator) {
            return Err(FormatError::InvalidSeparator(self.decimal_separator));
        }
        if let Some(separator) = self.thousands_separator {
            if bad_separator(separator) {
                return Err(FormatError::InvalidSeparator(separator));
            }
            if separator == self.decimal_separator {
                return Err(FormatError::SeparatorConflict(separator));
            }
        }
        for affix in [&self.prefix, &self.suffix] {
            if affix.len() > Self::MAX_AFFIX_BYTES {
                return Err(FormatError::AffixTooLong {
                    limit: Self::MAX_AFFIX_BYTES,
                    actual: affix.len(),
                });
            }
            if affix.chars().any(char::is_control) {
                return Err(FormatError::AffixControlCharacter);
            }
        }
        Ok(())
    }

    /// Formats an exact decimal.
    ///
    /// # Errors
    ///
    /// Returns a configuration error from [`Self::validate`].
    pub fn format_decimal(&self, value: Decimal) -> Result<String, FormatError> {
        self.validate()?;
        let scaled = rescale(value, self.decimals, self.rounding);
        let negative = scaled < 0;
        let magnitude = scaled.unsigned_abs();
        let width = self.decimals as usize + 1;
        let digits = format!("{magnitude:0width$}");
        let (whole, fraction) = digits.split_at(digits.len() - self.decimals as usize);

        let mut body = String::with_capacity(digits.len() * 2 + 8);
        body.push_str(&self.prefix);
        let whole_len = whole.len();
        for (index, digit) in whole.chars().enumerate() {
            if index > 0
                && (whole_len - index) % 3 == 0
                && let Some(separator) = self.thousands_separator
            {
                body.push(separator);
            }
            body.push(digit);
        }
        if !fraction.is_empty() {
            body.push(self.decimal_separator);
            body.push_str(fraction);
        }
        body.push_str(&self.suffix);

        Ok(match (self.sign, negative, scaled > 0) {
            (SignStyle::Parentheses, true, _) => format!("({body})"),
            (_, true, _) => format!("-{body}"),
            (SignStyle::Always, false, true) => format!("+{body}"),
            _ => body,
        })
    }

    /// Formats an integer, decimal, or numeric string (optional sign and
    /// surrounding ASCII whitespace).
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::NotANumber`] or [`FormatError::UnsupportedValue`]
    /// for non-numeric input, or a configuration error.
    pub fn format_value(&self, value: &DataValue) -> Result<String, FormatError> {
        self.format_decimal(numeric_value(value)?)
    }
}

/// Reads an exact number from an integer, decimal, or numeric string.
pub(crate) fn numeric_value(value: &DataValue) -> Result<Decimal, FormatError> {
    match value {
        DataValue::Integer(number) => Ok(Decimal::from_integer(*number)),
        DataValue::Decimal(decimal) => Ok(*decimal),
        DataValue::String(text) => {
            let trimmed = text.trim_ascii();
            let invalid = || FormatError::NotANumber(text.clone());
            let unsigned = match trimmed.strip_prefix('+') {
                Some(rest) if rest.starts_with('-') => return Err(invalid()),
                Some(rest) => rest,
                None => trimmed,
            };
            Decimal::from_str(unsigned).map_err(|_| invalid())
        }
        other => Err(FormatError::UnsupportedValue {
            actual: other.value_type(),
        }),
    }
}

/// Returns `value * 10^decimals` rounded to an integer. Fits in `i128`
/// because both the coefficient and the scale factor are bounded.
fn rescale(value: Decimal, decimals: u32, rounding: DecimalRounding) -> i128 {
    let coefficient = i128::from(value.coefficient());
    let scale = value.scale();
    if decimals >= scale {
        return coefficient * 10_i128.pow(decimals - scale);
    }
    let divisor = 10_i128.pow(scale - decimals);
    let quotient = coefficient / divisor;
    let twice_remainder = (coefficient % divisor).abs() * 2;
    let away = quotient + coefficient.signum();
    let round_away = match rounding {
        DecimalRounding::Truncate => false,
        DecimalRounding::HalfAwayFromZero => twice_remainder >= divisor,
        DecimalRounding::HalfEven => {
            twice_remainder > divisor || (twice_remainder == divisor && quotient % 2 != 0)
        }
    };
    if round_away { away } else { quotient }
}

// ---------------------------------------------------------------- date/time

/// A fixed offset from UTC with minute precision, bounded to +/-18:00.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct UtcOffset {
    minutes: i32,
}

impl UtcOffset {
    pub const UTC: Self = Self { minutes: 0 };
    pub const MAX_MINUTES: i32 = 18 * 60;

    /// # Errors
    ///
    /// Returns [`FormatError::OffsetOutOfRange`] beyond +/-18:00.
    pub const fn from_minutes(minutes: i32) -> Result<Self, FormatError> {
        if minutes.unsigned_abs() > Self::MAX_MINUTES.unsigned_abs() {
            return Err(FormatError::OffsetOutOfRange { minutes });
        }
        Ok(Self { minutes })
    }

    /// # Errors
    ///
    /// Returns [`FormatError::OffsetOutOfRange`] beyond +/-18:00 or when
    /// `minutes` exceeds 59.
    pub fn from_hours_minutes(hours: i32, minutes: u32) -> Result<Self, FormatError> {
        let minutes = i32::try_from(minutes)
            .ok()
            .filter(|minutes| *minutes < 60)
            .ok_or(FormatError::OffsetOutOfRange { minutes: i32::MAX })?;
        // Widen before scaling: `hours.abs() * 60` overflows `i32` (and
        // `i32::MIN.abs()` panics) long before the range check could reject it.
        let total = i64::from(hours.unsigned_abs()) * 60 + i64::from(minutes);
        let total = if hours < 0 { -total } else { total };
        let total = i32::try_from(total).map_err(|_| FormatError::OffsetOutOfRange {
            minutes: if hours < 0 { i32::MIN } else { i32::MAX },
        })?;
        Self::from_minutes(total)
    }

    #[must_use]
    pub const fn minutes(self) -> i32 {
        self.minutes
    }

    fn render(self, colon: bool) -> String {
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let absolute = self.minutes.unsigned_abs();
        let (hours, minutes) = (absolute / 60, absolute % 60);
        if colon {
            format!("{sign}{hours:02}:{minutes:02}")
        } else {
            format!("{sign}{hours:02}{minutes:02}")
        }
    }
}

/// An instant as Unix seconds plus non-negative nanoseconds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanos: u32,
}

/// Which value shapes a date/time binding accepts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DateInput {
    /// Strings as ISO-8601; integers and decimals as Unix seconds.
    #[default]
    Auto,
    /// ISO-8601 strings only.
    Iso8601,
    /// Unix seconds from integers, decimals, or numeric strings.
    UnixSeconds,
}

impl Timestamp {
    /// Earliest supported instant: 0000-01-01T00:00:00Z.
    pub const MIN_SECONDS: i64 = -62_167_219_200;
    /// Latest supported instant: 9999-12-31T23:59:59Z.
    pub const MAX_SECONDS: i64 = 253_402_300_799;

    /// Converts Unix seconds (fraction kept to nanoseconds, floor-truncated).
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::DateOutOfRange`] outside years 0000-9999 UTC.
    pub fn from_unix(value: Decimal) -> Result<Self, FormatError> {
        let coefficient = i128::from(value.coefficient());
        let divisor = 10_i128.pow(value.scale());
        let seconds = coefficient.div_euclid(divisor);
        let remainder = coefficient.rem_euclid(divisor);
        let nanos = if value.scale() <= 9 {
            remainder * 10_i128.pow(9 - value.scale())
        } else {
            remainder / 10_i128.pow(value.scale() - 9)
        };
        let seconds = i64::try_from(seconds).map_err(|_| FormatError::DateOutOfRange)?;
        if !(Self::MIN_SECONDS..=Self::MAX_SECONDS).contains(&seconds) {
            return Err(FormatError::DateOutOfRange);
        }
        Ok(Self {
            seconds,
            nanos: u32::try_from(nanos).map_err(|_| FormatError::DateOutOfRange)?,
        })
    }

    /// Parses `YYYY-MM-DD`, or `YYYY-MM-DD[T| ]HH:MM[:SS[.fraction]]`
    /// followed by `Z`, `±HH`, `±HHMM`, or `±HH:MM`. Text without an offset
    /// (including date-only text) is interpreted at `naive_offset`.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::InvalidDateTime`], [`FormatError::MissingOffset`],
    /// or [`FormatError::DateOutOfRange`].
    pub fn parse_iso8601(text: &str, naive_offset: Option<UtcOffset>) -> Result<Self, FormatError> {
        let invalid = || FormatError::InvalidDateTime(text.to_owned());
        let input = text.trim_ascii();
        let bytes = input.as_bytes();
        let number = |range: std::ops::Range<usize>| -> Result<u32, FormatError> {
            let slice = bytes.get(range).ok_or_else(invalid)?;
            if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
                return Err(invalid());
            }
            slice
                .iter()
                .try_fold(0_u32, |acc, digit| {
                    acc.checked_mul(10)?.checked_add(u32::from(digit - b'0'))
                })
                .ok_or_else(invalid)
        };
        if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
            return Err(invalid());
        }
        let year = number(0..4)?;
        let month = number(5..7)?;
        let day = number(8..10)?;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return Err(invalid());
        }
        let (mut hour, mut minute, mut second, mut nanos) = (0, 0, 0, 0_u32);
        let mut cursor = 10;
        if cursor < bytes.len() && matches!(bytes[cursor], b'T' | b't' | b' ') {
            hour = number(cursor + 1..cursor + 3)?;
            if bytes.get(cursor + 3) != Some(&b':') {
                return Err(invalid());
            }
            minute = number(cursor + 4..cursor + 6)?;
            cursor += 6;
            if bytes.get(cursor) == Some(&b':') {
                second = number(cursor + 1..cursor + 3)?;
                cursor += 3;
                if bytes.get(cursor) == Some(&b'.') {
                    let start = cursor + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    let digits = end - start;
                    if digits == 0 || digits > 9 {
                        return Err(invalid());
                    }
                    let exponent = u32::try_from(9 - digits).map_err(|_| invalid())?;
                    nanos = number(start..end)? * 10_u32.pow(exponent);
                    cursor = end;
                }
            }
            if hour > 23 || minute > 59 || second > 59 {
                return Err(invalid());
            }
        }
        let offset = match bytes.get(cursor) {
            None => naive_offset.ok_or_else(|| FormatError::MissingOffset(text.to_owned()))?,
            Some(b'Z' | b'z') if cursor + 1 == bytes.len() && cursor > 10 => UtcOffset::UTC,
            Some(sign @ (b'+' | b'-')) if cursor > 10 => {
                let rest = &bytes[cursor + 1..];
                let hours = number(cursor + 1..cursor + 3)?;
                let minutes = match rest.len() {
                    2 => 0,
                    4 => number(cursor + 3..cursor + 5)?,
                    5 if rest[2] == b':' => number(cursor + 4..cursor + 6)?,
                    _ => return Err(invalid()),
                };
                if minutes > 59 {
                    return Err(invalid());
                }
                let total = i32::try_from(hours * 60 + minutes).map_err(|_| invalid())?;
                UtcOffset::from_minutes(if *sign == b'-' { -total } else { total })
                    .map_err(|_| invalid())?
            }
            Some(_) => return Err(invalid()),
        };
        let days = days_from_civil(i64::from(year), month, day);
        let local = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
        let seconds = local - i64::from(offset.minutes) * 60;
        if !(Self::MIN_SECONDS..=Self::MAX_SECONDS).contains(&seconds) {
            return Err(FormatError::DateOutOfRange);
        }
        Ok(Self { seconds, nanos })
    }
}

/// One element of a validated date/time pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DateToken {
    Literal(String),
    /// A single-character directive after `%`.
    Directive(char),
}

/// A validated strftime-like pattern.
///
/// Supported directives: `%Y` (4-digit year), `%y` (2-digit year), `%m`,
/// `%d`, `%e` (space-padded day), `%j` (day of year), `%H`, `%I`, `%M`,
/// `%S`, `%L` (milliseconds), `%p` (AM/PM), `%b`/`%B` (English month),
/// `%a`/`%A` (English weekday), `%z` (`+hhmm`), `%Z` (`+hh:mm`), and `%%`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatePattern {
    source: String,
    tokens: Vec<DateToken>,
}

const DIRECTIVES: &str = "YymdejHIMSLpbBaAzZ";
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl DatePattern {
    pub const MAX_BYTES: usize = 128;
    pub const MAX_DIRECTIVES: usize = 32;

    /// # Errors
    ///
    /// Returns [`FormatError::PatternTooLong`], [`FormatError::TooManyDirectives`],
    /// [`FormatError::UnknownDirective`], or [`FormatError::DanglingPercent`].
    pub fn parse(source: &str) -> Result<Self, FormatError> {
        if source.len() > Self::MAX_BYTES {
            return Err(FormatError::PatternTooLong {
                limit: Self::MAX_BYTES,
                actual: source.len(),
            });
        }
        let mut tokens = Vec::new();
        let mut literal = String::new();
        let mut directives = 0;
        let mut chars = source.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                literal.push(c);
                continue;
            }
            match chars.next() {
                None => return Err(FormatError::DanglingPercent),
                Some('%') => literal.push('%'),
                Some(directive) if DIRECTIVES.contains(directive) => {
                    directives += 1;
                    if directives > Self::MAX_DIRECTIVES {
                        return Err(FormatError::TooManyDirectives {
                            limit: Self::MAX_DIRECTIVES,
                        });
                    }
                    if !literal.is_empty() {
                        tokens.push(DateToken::Literal(std::mem::take(&mut literal)));
                    }
                    tokens.push(DateToken::Directive(directive));
                }
                Some(other) => return Err(FormatError::UnknownDirective(other)),
            }
        }
        if !literal.is_empty() {
            tokens.push(DateToken::Literal(literal));
        }
        Ok(Self {
            source: source.to_owned(),
            tokens,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Renders `instant` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::DateOutOfRange`] when the local date leaves
    /// years 0000-9999.
    pub fn render(&self, instant: Timestamp, offset: UtcOffset) -> Result<String, FormatError> {
        let local = instant.seconds + i64::from(offset.minutes) * 60;
        let days = local.div_euclid(86_400);
        let second_of_day = local.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        if !(0..=9999).contains(&year) {
            return Err(FormatError::DateOutOfRange);
        }
        let hour = second_of_day / 3600;
        let minute = second_of_day % 3600 / 60;
        let second = second_of_day % 60;
        let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);
        let month_index = usize::try_from(month - 1).unwrap_or(0);
        let mut out = String::with_capacity(self.source.len() + 16);
        for token in &self.tokens {
            match token {
                DateToken::Literal(text) => out.push_str(text),
                DateToken::Directive(directive) => {
                    let piece = match directive {
                        'Y' => format!("{year:04}"),
                        'y' => format!("{:02}", year % 100),
                        'm' => format!("{month:02}"),
                        'd' => format!("{day:02}"),
                        'e' => format!("{day:>2}"),
                        'j' => format!("{:03}", days - days_from_civil(year, 1, 1) + 1),
                        'H' => format!("{hour:02}"),
                        'I' => format!("{:02}", (hour + 11) % 12 + 1),
                        'M' => format!("{minute:02}"),
                        'S' => format!("{second:02}"),
                        'L' => format!("{:03}", instant.nanos / 1_000_000),
                        'p' => (if hour < 12 { "AM" } else { "PM" }).to_owned(),
                        'b' => MONTHS[month_index][..3].to_owned(),
                        'B' => MONTHS[month_index].to_owned(),
                        'a' => WEEKDAYS[weekday][..3].to_owned(),
                        'A' => WEEKDAYS[weekday].to_owned(),
                        'z' => offset.render(false),
                        'Z' => offset.render(true),
                        _ => unreachable!("directives are validated by DatePattern::parse"),
                    };
                    out.push_str(&piece);
                }
            }
        }
        Ok(out)
    }
}

impl FromStr for DatePattern {
    type Err = FormatError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::parse(source)
    }
}

/// Date/time formatting: input shape, naive-input offset, output offset, and pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateTimeFormat {
    pub pattern: DatePattern,
    pub input: DateInput,
    /// Offset applied to ISO-8601 text that carries none; `None` rejects it.
    pub naive_offset: Option<UtcOffset>,
    /// Offset at which the instant is rendered.
    pub output_offset: UtcOffset,
}

impl DateTimeFormat {
    /// UTC output, [`DateInput::Auto`], and naive ISO text rejected.
    #[must_use]
    pub const fn new(pattern: DatePattern) -> Self {
        Self {
            pattern,
            input: DateInput::Auto,
            naive_offset: None,
            output_offset: UtcOffset::UTC,
        }
    }

    #[must_use]
    pub const fn with_input(mut self, input: DateInput) -> Self {
        self.input = input;
        self
    }

    #[must_use]
    pub const fn with_naive_offset(mut self, offset: UtcOffset) -> Self {
        self.naive_offset = Some(offset);
        self
    }

    #[must_use]
    pub const fn with_output_offset(mut self, offset: UtcOffset) -> Self {
        self.output_offset = offset;
        self
    }

    /// Reads an instant from `value` according to [`Self::input`].
    ///
    /// # Errors
    ///
    /// Returns a [`FormatError`] for unsupported shapes or invalid input.
    pub fn instant(&self, value: &DataValue) -> Result<Timestamp, FormatError> {
        match (self.input, value) {
            (DateInput::Auto | DateInput::Iso8601, DataValue::String(text)) => {
                Timestamp::parse_iso8601(text, self.naive_offset)
            }
            (DateInput::Auto, DataValue::Integer(_) | DataValue::Decimal(_))
            | (
                DateInput::UnixSeconds,
                DataValue::Integer(_) | DataValue::Decimal(_) | DataValue::String(_),
            ) => Timestamp::from_unix(numeric_value(value)?),
            (_, other) => Err(FormatError::UnsupportedValue {
                actual: other.value_type(),
            }),
        }
    }

    /// Parses and renders `value`.
    ///
    /// # Errors
    ///
    /// Returns a [`FormatError`] for unsupported or out-of-range input.
    pub fn format_value(&self, value: &DataValue) -> Result<String, FormatError> {
        self.pattern
            .render(self.instant(value)?, self.output_offset)
    }
}

const fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let (month, day) = (i64::from(month), i64::from(day));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Proleptic Gregorian `(year, month, day)` for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}
