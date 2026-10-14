use core::fmt;

use crate::{Clock, ClockTime};

pub(crate) const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_SECOND: i128 = 1_000_000_000;
/// 0001-01-01T00:00:00Z.
const MIN_UNIX_SECONDS: i64 = -62_135_596_800;
/// 9999-12-31T23:59:59Z.
const MAX_UNIX_SECONDS: i64 = 253_402_300_799;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CivilError {
    /// The instant is outside 0001-01-01T00:00:00Z..=9999-12-31T23:59:59.999999999Z.
    OutOfRange,
    /// Nanoseconds must be below one second.
    InvalidNanos,
}

impl fmt::Display for CivilError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutOfRange => "instant is outside years 0001 through 9999",
            Self::InvalidNanos => "nanoseconds must be below one second",
        })
    }
}

impl std::error::Error for CivilError {}

/// A UTC instant on the proleptic Gregorian calendar without leap seconds
/// (Unix time), restricted to years 0001 through 9999.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UtcInstant {
    seconds: i64,
    nanos: u32,
}

impl UtcInstant {
    pub const UNIX_EPOCH: Self = Self {
        seconds: 0,
        nanos: 0,
    };

    /// Creates an instant from Unix seconds and a sub-second part.
    ///
    /// # Errors
    ///
    /// Returns [`CivilError::InvalidNanos`] for `nanos >= 1_000_000_000` and
    /// [`CivilError::OutOfRange`] outside years 0001 through 9999.
    pub const fn new(unix_seconds: i64, nanos: u32) -> Result<Self, CivilError> {
        if nanos >= 1_000_000_000 {
            return Err(CivilError::InvalidNanos);
        }
        if unix_seconds < MIN_UNIX_SECONDS || unix_seconds > MAX_UNIX_SECONDS {
            return Err(CivilError::OutOfRange);
        }
        Ok(Self {
            seconds: unix_seconds,
            nanos,
        })
    }

    /// Creates an instant from whole Unix seconds.
    ///
    /// # Errors
    ///
    /// Returns [`CivilError::OutOfRange`] outside years 0001 through 9999.
    pub const fn from_unix_seconds(unix_seconds: i64) -> Result<Self, CivilError> {
        Self::new(unix_seconds, 0)
    }

    /// Creates an instant from signed Unix nanoseconds. Negative values are
    /// floored, so `-1` is one nanosecond before the epoch.
    ///
    /// # Errors
    ///
    /// Returns [`CivilError::OutOfRange`] outside years 0001 through 9999.
    pub fn from_unix_nanos(unix_nanos: i128) -> Result<Self, CivilError> {
        let seconds = i64::try_from(unix_nanos.div_euclid(NANOS_PER_SECOND))
            .map_err(|_| CivilError::OutOfRange)?;
        let nanos = u32::try_from(unix_nanos.rem_euclid(NANOS_PER_SECOND))
            .map_err(|_| CivilError::InvalidNanos)?;
        Self::new(seconds, nanos)
    }

    /// Creates an instant from a civil UTC date and time.
    ///
    /// # Errors
    ///
    /// Returns [`CivilError::OutOfRange`] for an invalid date or time or a
    /// year outside 0001 through 9999, and [`CivilError::InvalidNanos`] for
    /// `nanos >= 1_000_000_000`.
    pub fn from_utc_civil(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        nanos: u32,
    ) -> Result<Self, CivilError> {
        if !(1..=9_999).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour >= 24
            || minute >= 60
            || second >= 60
        {
            return Err(CivilError::OutOfRange);
        }
        let seconds = days_from_civil(year, month, day) * SECONDS_PER_DAY
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
            + i64::from(second);
        Self::new(seconds, nanos)
    }

    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        self.seconds
    }

    #[must_use]
    pub const fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Converts to a civil date-time at a fixed offset east of UTC.
    #[must_use]
    pub fn to_civil(self, utc_offset_seconds: i32) -> LocalDateTime {
        let local = self.seconds + i64::from(utc_offset_seconds);
        let days = local.div_euclid(SECONDS_PER_DAY);
        let second_of_day = local.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        LocalDateTime {
            year,
            month,
            day,
            hour: narrow(second_of_day / 3_600),
            minute: narrow(second_of_day / 60 % 60),
            second: narrow(second_of_day % 60),
            nanosecond: self.nanos,
            weekday: weekday_from_days(days),
            utc_offset_seconds,
        }
    }
}

fn narrow(value: i64) -> u8 {
    u8::try_from(value).expect("time-of-day components are below 60")
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// Zero-based index with Sunday = 0, as used by POSIX TZ rules.
    #[must_use]
    pub const fn index_from_sunday(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Sunday => "Sun",
            Self::Monday => "Mon",
            Self::Tuesday => "Tue",
            Self::Wednesday => "Wed",
            Self::Thursday => "Thu",
            Self::Friday => "Fri",
            Self::Saturday => "Sat",
        }
    }
}

/// A civil date-time at a known UTC offset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
    pub weekday: Weekday,
    /// Offset east of UTC that produced this local time.
    pub utc_offset_seconds: i32,
}

/// A source of UTC wall time. The daemon implements this over the system
/// clock; tests use [`ManualWallClock`].
pub trait WallClock {
    #[must_use]
    fn utc_now(&self) -> UtcInstant;
}

/// A settable wall clock. Unlike monotonic clocks it may be stepped
/// backward, which is exactly what NTP or an operator can do to real UTC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualWallClock {
    now: UtcInstant,
}

impl ManualWallClock {
    #[must_use]
    pub const fn new(now: UtcInstant) -> Self {
        Self { now }
    }

    pub fn set(&mut self, now: UtcInstant) {
        self.now = now;
    }
}

impl WallClock for ManualWallClock {
    fn utc_now(&self) -> UtcInstant {
        self.now
    }
}

/// One sample of both time inputs, taken together by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeReading {
    /// Drives timers, session timers, and expiry.
    pub monotonic: ClockTime,
    /// Drives time-of-day only.
    pub utc: UtcInstant,
}

impl TimeReading {
    #[must_use]
    pub const fn new(monotonic: ClockTime, utc: UtcInstant) -> Self {
        Self { monotonic, utc }
    }

    /// Samples a monotonic clock and a wall clock.
    #[must_use]
    pub fn sample(monotonic: &impl Clock, wall: &impl WallClock) -> Self {
        Self::new(monotonic.now(), wall.utc_now())
    }
}

pub(crate) const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
pub(crate) fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
pub(crate) fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        i32::try_from(year).expect("supported instants have four-digit years"),
        u8::try_from(month).expect("month is 1 through 12"),
        u8::try_from(day).expect("day is 1 through 31"),
    )
}

pub(crate) fn weekday_from_days(days: i64) -> Weekday {
    // 1970-01-01 was a Thursday.
    match (days + 4).rem_euclid(7) {
        0 => Weekday::Sunday,
        1 => Weekday::Monday,
        2 => Weekday::Tuesday,
        3 => Weekday::Wednesday,
        4 => Weekday::Thursday,
        5 => Weekday::Friday,
        _ => Weekday::Saturday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips_across_supported_range() {
        let mut days = days_from_civil(1, 1, 1);
        let last = days_from_civil(9_999, 12, 31);
        let mut expected = (1, 1, 1);
        while days <= last {
            let civil = civil_from_days(days);
            assert_eq!(civil, expected, "day {days}");
            assert_eq!(days_from_civil(civil.0, civil.1, civil.2), days);
            expected = if expected.2 < days_in_month(expected.0, expected.1) {
                (expected.0, expected.1, expected.2 + 1)
            } else if expected.1 < 12 {
                (expected.0, expected.1 + 1, 1)
            } else {
                (expected.0 + 1, 1, 1)
            };
            days += 1;
        }
    }

    #[test]
    fn range_bounds_match_constants() {
        assert_eq!(days_from_civil(1, 1, 1) * SECONDS_PER_DAY, MIN_UNIX_SECONDS);
        assert_eq!(
            days_from_civil(9_999, 12, 31) * SECONDS_PER_DAY + SECONDS_PER_DAY - 1,
            MAX_UNIX_SECONDS
        );
        assert_eq!(weekday_from_days(0), Weekday::Thursday);
        assert_eq!(weekday_from_days(-1), Weekday::Wednesday);
    }
}
