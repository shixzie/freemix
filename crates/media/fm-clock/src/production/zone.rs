//! Explicit time-zone descriptions without a time-zone database.
//!
//! A [`TimeZone`] is either a fixed UTC offset or a standard offset plus one
//! annual daylight-saving rule, exactly the information a POSIX `TZ` string
//! carries (`EST5EDT,M3.2.0,M11.1.0`, `CET-1CEST,M3.5.0,M10.5.0/3`). This
//! covers every zone whose *current* rules are a single annual pair of
//! transitions; it cannot reproduce historical rule changes, which require
//! the IANA database.

use core::fmt;

use super::civil::{
    LocalDateTime, SECONDS_PER_DAY, UtcInstant, days_from_civil, days_in_month, is_leap_year,
    weekday_from_days,
};

const MAX_ABBREVIATION_LEN: usize = 16;
/// POSIX limits offsets to 24 hours; minutes and seconds may be added.
const MAX_OFFSET_SECONDS: i32 = 24 * 3_600 + 59 * 60 + 59;
/// RFC 8536 extends the transition time range to ±167 hours.
const MAX_TRANSITION_SECONDS: i32 = 167 * 3_600 + 59 * 60 + 59;
const DEFAULT_TRANSITION_SECONDS: i32 = 2 * 3_600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneError {
    /// The POSIX `TZ` text is malformed. The payload names the problem.
    Syntax(&'static str),
    /// Abbreviations are 1 to 16 ASCII letters, digits, `+` or `-`; parsed
    /// POSIX abbreviations must also be at least three characters long.
    InvalidAbbreviation,
    /// A UTC offset exceeds ±24:59:59.
    OffsetOutOfRange,
    /// A transition time exceeds ±167:59:59.
    TransitionTimeOutOfRange,
    /// A transition date component is out of range.
    InvalidTransitionDate,
    /// A daylight-saving abbreviation was given without transition rules.
    MissingRule,
    /// The daylight-saving offset equals the standard offset.
    DegenerateDaylightOffset,
}

impl fmt::Display for ZoneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(reason) => write!(formatter, "invalid time zone: {reason}"),
            Self::InvalidAbbreviation => formatter.write_str("invalid time zone abbreviation"),
            Self::OffsetOutOfRange => formatter.write_str("UTC offset exceeds 24:59:59"),
            Self::TransitionTimeOutOfRange => {
                formatter.write_str("transition time exceeds 167:59:59")
            }
            Self::InvalidTransitionDate => formatter.write_str("invalid transition date"),
            Self::MissingRule => {
                formatter.write_str("daylight saving time requires explicit transition rules")
            }
            Self::DegenerateDaylightOffset => {
                formatter.write_str("daylight offset must differ from the standard offset")
            }
        }
    }
}

impl std::error::Error for ZoneError {}

/// The local date on which a daylight-saving transition happens.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TransitionDate {
    /// `Mm.w.d`: weekday `d` (0 = Sunday) of week `w` (1..=5, 5 = last) of
    /// month `m`.
    MonthWeekDay { month: u8, week: u8, weekday: u8 },
    /// `Jn`: day 1..=365, never counting February 29.
    JulianNoLeap(u16),
    /// `n`: zero-based day 0..=365, counting February 29 in leap years.
    ZeroBased(u16),
}

impl TransitionDate {
    fn validate(self) -> Result<Self, ZoneError> {
        let valid = match self {
            Self::MonthWeekDay {
                month,
                week,
                weekday,
            } => (1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6,
            Self::JulianNoLeap(day) => (1..=365).contains(&day),
            Self::ZeroBased(day) => day <= 365,
        };
        if valid {
            Ok(self)
        } else {
            Err(ZoneError::InvalidTransitionDate)
        }
    }

    /// Days since 1970-01-01 of this date in `year`.
    fn days_in_year(self, year: i32) -> i64 {
        let january_first = days_from_civil(year, 1, 1);
        match self {
            Self::MonthWeekDay {
                month,
                week,
                weekday,
            } => {
                let first = days_from_civil(year, month, 1);
                let first_weekday = i64::from(weekday_from_days(first).index_from_sunday());
                let mut day = 1
                    + (i64::from(weekday) - first_weekday).rem_euclid(7)
                    + (i64::from(week) - 1) * 7;
                let length = i64::from(days_in_month(year, month));
                while day > length {
                    day -= 7;
                }
                first + day - 1
            }
            Self::JulianNoLeap(day) => {
                let leap_skip = i64::from(is_leap_year(year) && day >= 60);
                january_first + i64::from(day) - 1 + leap_skip
            }
            Self::ZeroBased(day) => january_first + i64::from(day),
        }
    }
}

/// The daylight-saving half of a [`TimeZone`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DaylightRule {
    pub abbreviation: String,
    /// Offset east of UTC while daylight-saving time is in effect.
    pub utc_offset_seconds: i32,
    pub start: TransitionDate,
    /// Local standard time of day at which daylight time starts.
    pub start_time_seconds: i32,
    pub end: TransitionDate,
    /// Local daylight time of day at which daylight time ends.
    pub end_time_seconds: i32,
}

/// A fixed offset or a standard offset plus one annual daylight rule.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TimeZone {
    abbreviation: String,
    utc_offset_seconds: i32,
    daylight: Option<DaylightRule>,
}

/// The offset in effect at one instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneResolution<'a> {
    pub utc_offset_seconds: i32,
    pub abbreviation: &'a str,
    pub is_dst: bool,
}

impl TimeZone {
    /// Coordinated Universal Time.
    #[must_use]
    pub fn utc() -> Self {
        Self {
            abbreviation: "UTC".to_owned(),
            utc_offset_seconds: 0,
            daylight: None,
        }
    }

    /// A zone with a fixed offset east of UTC.
    ///
    /// # Errors
    ///
    /// Returns [`ZoneError::InvalidAbbreviation`] or
    /// [`ZoneError::OffsetOutOfRange`].
    pub fn fixed(abbreviation: &str, utc_offset_seconds: i32) -> Result<Self, ZoneError> {
        Self::new(abbreviation, utc_offset_seconds, None)
    }

    /// A zone with a standard offset and an optional daylight rule.
    ///
    /// # Errors
    ///
    /// Returns a [`ZoneError`] when an abbreviation, offset, transition time,
    /// or transition date is invalid, or when the daylight offset equals the
    /// standard offset.
    pub fn new(
        abbreviation: &str,
        utc_offset_seconds: i32,
        daylight: Option<DaylightRule>,
    ) -> Result<Self, ZoneError> {
        validate_abbreviation(abbreviation, 1)?;
        validate_offset(utc_offset_seconds)?;
        if let Some(rule) = &daylight {
            validate_abbreviation(&rule.abbreviation, 1)?;
            validate_offset(rule.utc_offset_seconds)?;
            if rule.utc_offset_seconds == utc_offset_seconds {
                return Err(ZoneError::DegenerateDaylightOffset);
            }
            rule.start.validate()?;
            rule.end.validate()?;
            validate_transition_time(rule.start_time_seconds)?;
            validate_transition_time(rule.end_time_seconds)?;
        }
        Ok(Self {
            abbreviation: abbreviation.to_owned(),
            utc_offset_seconds,
            daylight,
        })
    }

    /// Parses a POSIX `TZ` string (IEEE 1003.1 section 8.3, with the RFC 8536
    /// extension allowing transition times of ±167 hours).
    ///
    /// The implementation-defined default rule is deliberately not supported:
    /// a daylight abbreviation must be followed by explicit transitions.
    ///
    /// # Errors
    ///
    /// Returns a [`ZoneError`] describing the first problem found.
    pub fn parse_posix(text: &str) -> Result<Self, ZoneError> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            position: 0,
        };
        let abbreviation = parser.abbreviation()?;
        let utc_offset_seconds = -parser.offset()?;
        if parser.is_done() {
            return Self::new(&abbreviation, utc_offset_seconds, None);
        }
        let daylight_abbreviation = parser.abbreviation()?;
        let daylight_offset = if matches!(parser.peek(), Some(b'0'..=b'9' | b'+' | b'-')) {
            -parser.offset()?
        } else {
            utc_offset_seconds
                .checked_add(3_600)
                .ok_or(ZoneError::OffsetOutOfRange)?
        };
        if parser.is_done() {
            return Err(ZoneError::MissingRule);
        }
        parser.expect(b',', "expected ',' before the daylight start rule")?;
        let (start, start_time_seconds) = parser.rule()?;
        parser.expect(b',', "expected ',' before the daylight end rule")?;
        let (end, end_time_seconds) = parser.rule()?;
        if !parser.is_done() {
            return Err(ZoneError::Syntax("unexpected trailing text"));
        }
        Self::new(
            &abbreviation,
            utc_offset_seconds,
            Some(DaylightRule {
                abbreviation: daylight_abbreviation,
                utc_offset_seconds: daylight_offset,
                start,
                start_time_seconds,
                end,
                end_time_seconds,
            }),
        )
    }

    #[must_use]
    pub fn abbreviation(&self) -> &str {
        &self.abbreviation
    }

    /// Standard offset east of UTC.
    #[must_use]
    pub const fn utc_offset_seconds(&self) -> i32 {
        self.utc_offset_seconds
    }

    #[must_use]
    pub const fn daylight(&self) -> Option<&DaylightRule> {
        self.daylight.as_ref()
    }

    /// Returns the offset and abbreviation in effect at `instant`.
    ///
    /// Each year's rule gives one daylight interval: `[start, end)`, or
    /// `[start, next year's end)` when the end falls before the start in the
    /// year (southern hemisphere). Equal transitions mean no daylight time
    /// that year. Intervals of neighbouring years are considered too, so
    /// transition times beyond a day that cross a year boundary still apply.
    #[must_use]
    pub fn resolve(&self, instant: UtcInstant) -> ZoneResolution<'_> {
        let standard = ZoneResolution {
            utc_offset_seconds: self.utc_offset_seconds,
            abbreviation: &self.abbreviation,
            is_dst: false,
        };
        let Some(rule) = &self.daylight else {
            return standard;
        };
        let seconds = instant.unix_seconds();
        let local_standard = seconds + i64::from(self.utc_offset_seconds);
        let (year, _, _) =
            super::civil::civil_from_days(local_standard.div_euclid(SECONDS_PER_DAY));
        // Each rule year contributes one daylight interval: `[start, end)`,
        // or, when the end precedes the start (southern hemisphere),
        // `[start, next year's end)`. Transition times of up to ±167 hours
        // can move a year's transitions into a neighbouring year, so the
        // intervals of the surrounding years are checked too; years outside
        // `year - 2..=year + 1` cannot reach the instant.
        let start = |rule_year: i32| {
            rule.start.days_in_year(rule_year) * SECONDS_PER_DAY
                + i64::from(rule.start_time_seconds)
                - i64::from(self.utc_offset_seconds)
        };
        let end = |rule_year: i32| {
            rule.end.days_in_year(rule_year) * SECONDS_PER_DAY + i64::from(rule.end_time_seconds)
                - i64::from(rule.utc_offset_seconds)
        };
        let is_dst = (year - 2..=year + 1).any(|rule_year| {
            let (start, same_year_end) = (start(rule_year), end(rule_year));
            let end = match start.cmp(&same_year_end) {
                core::cmp::Ordering::Less => same_year_end,
                core::cmp::Ordering::Greater => end(rule_year + 1),
                core::cmp::Ordering::Equal => return false,
            };
            start <= seconds && seconds < end
        });
        if is_dst {
            ZoneResolution {
                utc_offset_seconds: rule.utc_offset_seconds,
                abbreviation: &rule.abbreviation,
                is_dst: true,
            }
        } else {
            standard
        }
    }

    /// Converts `instant` to local civil time in this zone.
    #[must_use]
    pub fn to_local(&self, instant: UtcInstant) -> LocalDateTime {
        instant.to_civil(self.resolve(instant).utc_offset_seconds)
    }
}

/// Writes the canonical POSIX `TZ` form, which [`TimeZone::parse_posix`]
/// accepts and parses back to an equal zone when the abbreviations are at
/// least three characters long.
impl fmt::Display for TimeZone {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_abbreviation(formatter, &self.abbreviation)?;
        write_hms(formatter, -self.utc_offset_seconds)?;
        let Some(rule) = &self.daylight else {
            return Ok(());
        };
        write_abbreviation(formatter, &rule.abbreviation)?;
        if Some(rule.utc_offset_seconds) != self.utc_offset_seconds.checked_add(3_600) {
            write_hms(formatter, -rule.utc_offset_seconds)?;
        }
        for (date, time) in [
            (rule.start, rule.start_time_seconds),
            (rule.end, rule.end_time_seconds),
        ] {
            formatter.write_str(",")?;
            match date {
                TransitionDate::MonthWeekDay {
                    month,
                    week,
                    weekday,
                } => write!(formatter, "M{month}.{week}.{weekday}")?,
                TransitionDate::JulianNoLeap(day) => write!(formatter, "J{day}")?,
                TransitionDate::ZeroBased(day) => write!(formatter, "{day}")?,
            }
            if time != DEFAULT_TRANSITION_SECONDS {
                formatter.write_str("/")?;
                write_hms(formatter, time)?;
            }
        }
        Ok(())
    }
}

fn write_abbreviation(formatter: &mut fmt::Formatter<'_>, abbreviation: &str) -> fmt::Result {
    if abbreviation.len() >= 3 && abbreviation.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        formatter.write_str(abbreviation)
    } else {
        write!(formatter, "<{abbreviation}>")
    }
}

fn write_hms(formatter: &mut fmt::Formatter<'_>, seconds: i32) -> fmt::Result {
    if seconds < 0 {
        formatter.write_str("-")?;
    }
    let magnitude = seconds.unsigned_abs();
    let (hours, minutes, seconds) = (magnitude / 3_600, magnitude / 60 % 60, magnitude % 60);
    write!(formatter, "{hours}")?;
    if minutes != 0 || seconds != 0 {
        write!(formatter, ":{minutes:02}")?;
    }
    if seconds != 0 {
        write!(formatter, ":{seconds:02}")?;
    }
    Ok(())
}

fn validate_abbreviation(abbreviation: &str, min_len: usize) -> Result<(), ZoneError> {
    let valid_len = (min_len..=MAX_ABBREVIATION_LEN).contains(&abbreviation.len());
    let valid_bytes = abbreviation
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'-');
    if valid_len && valid_bytes {
        Ok(())
    } else {
        Err(ZoneError::InvalidAbbreviation)
    }
}

const fn validate_offset(seconds: i32) -> Result<(), ZoneError> {
    if seconds.unsigned_abs() > MAX_OFFSET_SECONDS.unsigned_abs() {
        Err(ZoneError::OffsetOutOfRange)
    } else {
        Ok(())
    }
}

const fn validate_transition_time(seconds: i32) -> Result<(), ZoneError> {
    if seconds.unsigned_abs() > MAX_TRANSITION_SECONDS.unsigned_abs() {
        Err(ZoneError::TransitionTimeOutOfRange)
    } else {
        Ok(())
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn is_done(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn expect(&mut self, expected: u8, message: &'static str) -> Result<(), ZoneError> {
        if self.bump() == Some(expected) {
            Ok(())
        } else {
            Err(ZoneError::Syntax(message))
        }
    }

    fn abbreviation(&mut self) -> Result<String, ZoneError> {
        let quoted = self.peek() == Some(b'<');
        if quoted {
            self.position += 1;
        }
        let start = self.position;
        while let Some(byte) = self.peek() {
            let accepted = if quoted {
                byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'-'
            } else {
                byte.is_ascii_alphabetic()
            };
            if !accepted {
                break;
            }
            self.position += 1;
        }
        let text = core::str::from_utf8(&self.bytes[start..self.position])
            .map_err(|_| ZoneError::InvalidAbbreviation)?
            .to_owned();
        if quoted {
            self.expect(b'>', "unterminated quoted abbreviation")?;
        }
        validate_abbreviation(&text, 3)?;
        Ok(text)
    }

    /// Parses `[+-]hh[:mm[:ss]]` with at most `max_hour_digits` hour digits.
    fn signed_hms(&mut self, max_hour_digits: usize) -> Result<i32, ZoneError> {
        let negative = match self.peek() {
            Some(b'-') => {
                self.position += 1;
                true
            }
            Some(b'+') => {
                self.position += 1;
                false
            }
            _ => false,
        };
        let hours = self.number(1, max_hour_digits, "expected hours")?;
        let mut total = hours * 3_600;
        for scale in [60, 1] {
            if self.peek() != Some(b':') {
                break;
            }
            self.position += 1;
            let part = self.number(2, 2, "expected two-digit minutes or seconds")?;
            if part >= 60 {
                return Err(ZoneError::Syntax("minutes and seconds must be below 60"));
            }
            total += part * scale;
        }
        Ok(if negative { -total } else { total })
    }

    fn offset(&mut self) -> Result<i32, ZoneError> {
        let seconds = self.signed_hms(2)?;
        validate_offset(seconds)?;
        Ok(seconds)
    }

    fn number(
        &mut self,
        min_digits: usize,
        max_digits: usize,
        message: &'static str,
    ) -> Result<i32, ZoneError> {
        let start = self.position;
        while self.position - start < max_digits && matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        let digits = &self.bytes[start..self.position];
        if digits.len() < min_digits {
            return Err(ZoneError::Syntax(message));
        }
        Ok(digits
            .iter()
            .fold(0, |value, digit| value * 10 + i32::from(digit - b'0')))
    }

    fn rule(&mut self) -> Result<(TransitionDate, i32), ZoneError> {
        let date = match self.peek() {
            Some(b'M') => {
                self.position += 1;
                let month = self.small_number(2, "expected a month")?;
                self.expect(b'.', "expected '.' after the month")?;
                let week = self.small_number(1, "expected a week")?;
                self.expect(b'.', "expected '.' after the week")?;
                let weekday = self.small_number(1, "expected a weekday")?;
                TransitionDate::MonthWeekDay {
                    month,
                    week,
                    weekday,
                }
            }
            Some(b'J') => {
                self.position += 1;
                TransitionDate::JulianNoLeap(self.day_number()?)
            }
            Some(b'0'..=b'9') => TransitionDate::ZeroBased(self.day_number()?),
            _ => return Err(ZoneError::Syntax("expected a transition date")),
        }
        .validate()?;
        let time = if self.peek() == Some(b'/') {
            self.position += 1;
            let time = self.signed_hms(3)?;
            validate_transition_time(time)?;
            time
        } else {
            DEFAULT_TRANSITION_SECONDS
        };
        Ok((date, time))
    }

    fn small_number(&mut self, max_digits: usize, message: &'static str) -> Result<u8, ZoneError> {
        let value = self.number(1, max_digits, message)?;
        u8::try_from(value).map_err(|_| ZoneError::InvalidTransitionDate)
    }

    fn day_number(&mut self) -> Result<u16, ZoneError> {
        let value = self.number(1, 3, "expected a day number")?;
        u16::try_from(value).map_err(|_| ZoneError::InvalidTransitionDate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u8, day: u8) -> i64 {
        days_from_civil(year, month, day)
    }

    #[test]
    fn month_week_day_selects_nth_and_last_weekday() {
        let second_sunday_march = TransitionDate::MonthWeekDay {
            month: 3,
            week: 2,
            weekday: 0,
        };
        assert_eq!(second_sunday_march.days_in_year(2026), date(2026, 3, 8));
        // Week 5 means the last such weekday, whether the month has four or
        // five of them.
        let last_sunday_february = TransitionDate::MonthWeekDay {
            month: 2,
            week: 5,
            weekday: 0,
        };
        assert_eq!(last_sunday_february.days_in_year(2024), date(2024, 2, 25));
        let last_thursday_february = TransitionDate::MonthWeekDay {
            month: 2,
            week: 5,
            weekday: 4,
        };
        assert_eq!(last_thursday_february.days_in_year(2024), date(2024, 2, 29));
        assert_eq!(last_thursday_february.days_in_year(2026), date(2026, 2, 26));
        // A month starting on the requested weekday.
        let first_thursday_january = TransitionDate::MonthWeekDay {
            month: 1,
            week: 1,
            weekday: 4,
        };
        assert_eq!(first_thursday_january.days_in_year(2026), date(2026, 1, 1));
    }

    #[test]
    fn julian_forms_treat_february_29_differently() {
        assert_eq!(
            TransitionDate::JulianNoLeap(59).days_in_year(2024),
            date(2024, 2, 28)
        );
        assert_eq!(
            TransitionDate::JulianNoLeap(60).days_in_year(2024),
            date(2024, 3, 1)
        );
        assert_eq!(
            TransitionDate::JulianNoLeap(365).days_in_year(2024),
            date(2024, 12, 31)
        );
        assert_eq!(
            TransitionDate::ZeroBased(59).days_in_year(2024),
            date(2024, 2, 29)
        );
        assert_eq!(
            TransitionDate::ZeroBased(59).days_in_year(2026),
            date(2026, 3, 1)
        );
        assert_eq!(
            TransitionDate::ZeroBased(365).days_in_year(2024),
            date(2024, 12, 31)
        );
    }

    #[test]
    fn transition_dates_are_range_checked() {
        for invalid in [
            TransitionDate::MonthWeekDay {
                month: 0,
                week: 1,
                weekday: 0,
            },
            TransitionDate::MonthWeekDay {
                month: 12,
                week: 6,
                weekday: 0,
            },
            TransitionDate::MonthWeekDay {
                month: 12,
                week: 0,
                weekday: 0,
            },
            TransitionDate::MonthWeekDay {
                month: 12,
                week: 1,
                weekday: 7,
            },
            TransitionDate::JulianNoLeap(0),
            TransitionDate::JulianNoLeap(366),
            TransitionDate::ZeroBased(366),
        ] {
            assert_eq!(invalid.validate(), Err(ZoneError::InvalidTransitionDate));
        }
    }
}
