use core::fmt::Write as _;

use fm_types::FrameRate;

use super::{ClockValue, LocalDateTime};

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// Display presets for durations and timer values.
///
/// Hours and total minutes are never wrapped: `MinutesSeconds` shows
/// 90 minutes as `90:00`, and `HoursMinutesSeconds` shows 100 hours as
/// `100:00:00`. Negative values are prefixed with `-` unless they round to
/// zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DurationFormat {
    /// `HH:MM:SS`
    HoursMinutesSeconds,
    /// `MM:SS` with unwrapped minutes.
    MinutesSeconds,
    /// Whole seconds without padding, e.g. `95`.
    Seconds,
    /// `HH:MM:SS.t`
    HoursMinutesSecondsTenths,
    /// `MM:SS.t`
    MinutesSecondsTenths,
    /// `HH:MM:SS.mmm`
    HoursMinutesSecondsMillis,
    /// `HH:MM:SS:FF`: real-time seconds plus the frame within that second
    /// at the given rate. For fractional rates a second holds either
    /// `floor(rate)` or `ceil(rate)` frames, so the seconds always agree
    /// with the other presets; this is not drop-frame SMPTE timecode.
    HoursMinutesSecondsFrames(FrameRate),
    /// `H:MM:SS` from one hour upward, otherwise `M:SS`.
    Compact,
}

/// Direction in which a value is rounded to the display unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Rounding {
    /// Toward negative infinity. Used by count-up displays.
    Floor,
    /// Toward positive infinity. Used by countdown displays so the last
    /// label before zero lasts a full unit and zero appears at expiry.
    Ceil,
}

/// Display presets for time of day.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TimeOfDayFormat {
    /// `HH:MM:SS`, 24-hour.
    Hours24,
    /// `HH:MM`, 24-hour.
    Hours24Minutes,
    /// `h:MM:SS AM`, 12-hour.
    Hours12,
    /// `h:MM AM`, 12-hour.
    Hours12Minutes,
    /// `YYYY-MM-DD`
    Date,
    /// RFC 3339 `YYYY-MM-DDTHH:MM:SS+hh:mm` with whole seconds.
    Rfc3339,
}

enum Grid {
    /// Units per second.
    Decimal(i128),
    Frames {
        numerator: i128,
        denominator: i128,
    },
}

impl DurationFormat {
    fn grid(self) -> Grid {
        match self {
            Self::HoursMinutesSeconds | Self::MinutesSeconds | Self::Seconds | Self::Compact => {
                Grid::Decimal(1)
            }
            Self::HoursMinutesSecondsTenths | Self::MinutesSecondsTenths => Grid::Decimal(10),
            Self::HoursMinutesSecondsMillis => Grid::Decimal(1_000),
            Self::HoursMinutesSecondsFrames(rate) => Grid::Frames {
                numerator: i128::from(rate.numerator()),
                denominator: i128::from(rate.denominator()),
            },
        }
    }
}

fn div_floor(numerator: i128, denominator: i128) -> i128 {
    let quotient = numerator / denominator;
    if numerator % denominator != 0 && (numerator < 0) != (denominator < 0) {
        quotient - 1
    } else {
        quotient
    }
}

fn div_ceil(numerator: i128, denominator: i128) -> i128 {
    let quotient = numerator / denominator;
    if numerator % denominator != 0 && (numerator < 0) == (denominator < 0) {
        quotient + 1
    } else {
        quotient
    }
}

fn divide(numerator: i128, denominator: i128, rounding: Rounding) -> i128 {
    match rounding {
        Rounding::Floor => div_floor(numerator, denominator),
        Rounding::Ceil => div_ceil(numerator, denominator),
    }
}

/// Rounds non-negative `nanos` to a count of grid units.
fn grid_units(grid: &Grid, nanos: i128, rounding: Rounding) -> i128 {
    match *grid {
        Grid::Decimal(per_second) => divide(nanos * per_second, NANOS_PER_SECOND, rounding),
        // Frame `k` starts at the integer nanosecond `floor(k / rate)`, the
        // same convention as `FrameCadence`. Floor picks the last frame
        // starting at or before the value; ceil picks the first frame
        // starting at or after it.
        Grid::Frames {
            numerator,
            denominator,
        } => {
            let per_frame = denominator * NANOS_PER_SECOND;
            match rounding {
                Rounding::Floor => div_ceil((nanos + 1) * numerator, per_frame) - 1,
                Rounding::Ceil => div_ceil(nanos * numerator, per_frame),
            }
        }
    }
}

/// Formats a signed value with a preset, rounding to the preset's smallest
/// unit (second, tenth, millisecond, or frame) in the given direction.
#[must_use]
pub fn format_duration(value: ClockValue, format: DurationFormat, rounding: Rounding) -> String {
    let grid = format.grid();
    // Negative values mirror positive ones: the magnitude is rounded in the
    // opposite direction and negated, so `-x` floors to the negation of `x`
    // ceiled. The frame grid is not symmetric about zero in integer
    // nanoseconds, so this keeps negative frame labels the mirror image of
    // positive ones.
    let magnitude_nanos = i128::from(value.as_nanos()).abs();
    let units = if value.is_negative() {
        let opposite = match rounding {
            Rounding::Floor => Rounding::Ceil,
            Rounding::Ceil => Rounding::Floor,
        };
        -grid_units(&grid, magnitude_nanos, opposite)
    } else {
        grid_units(&grid, magnitude_nanos, rounding)
    };
    let magnitude = units.unsigned_abs();
    let (seconds, sub) = match grid {
        Grid::Decimal(per_second) => {
            let per_second = per_second.unsigned_abs();
            (magnitude / per_second, magnitude % per_second)
        }
        Grid::Frames {
            numerator,
            denominator,
        } => {
            let (numerator, denominator) = (numerator.unsigned_abs(), denominator.unsigned_abs());
            let seconds = magnitude * denominator / numerator;
            let first_frame_of_second = (seconds * numerator).div_ceil(denominator);
            (seconds, magnitude - first_frame_of_second)
        }
    };
    let mut text = String::new();
    if units < 0 {
        text.push('-');
    }
    let (hours, minutes, secs) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    let total_minutes = seconds / 60;
    // Writing to a String cannot fail.
    let _ = match format {
        DurationFormat::HoursMinutesSeconds => {
            write!(text, "{hours:02}:{minutes:02}:{secs:02}")
        }
        DurationFormat::MinutesSeconds => write!(text, "{total_minutes:02}:{secs:02}"),
        DurationFormat::Seconds => write!(text, "{seconds}"),
        DurationFormat::HoursMinutesSecondsTenths => {
            write!(text, "{hours:02}:{minutes:02}:{secs:02}.{sub}")
        }
        DurationFormat::MinutesSecondsTenths => write!(text, "{total_minutes:02}:{secs:02}.{sub}"),
        DurationFormat::HoursMinutesSecondsMillis => {
            write!(text, "{hours:02}:{minutes:02}:{secs:02}.{sub:03}")
        }
        DurationFormat::HoursMinutesSecondsFrames(_) => {
            write!(text, "{hours:02}:{minutes:02}:{secs:02}:{sub:02}")
        }
        DurationFormat::Compact if hours > 0 => write!(text, "{hours}:{minutes:02}:{secs:02}"),
        DurationFormat::Compact => write!(text, "{minutes}:{secs:02}"),
    };
    text
}

/// Formats a local date-time. Sub-second parts are truncated.
#[must_use]
pub fn format_time_of_day(local: &LocalDateTime, format: TimeOfDayFormat) -> String {
    let LocalDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        ..
    } = *local;
    let twelve = match hour % 12 {
        0 => 12,
        other => other,
    };
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    match format {
        TimeOfDayFormat::Hours24 => format!("{hour:02}:{minute:02}:{second:02}"),
        TimeOfDayFormat::Hours24Minutes => format!("{hour:02}:{minute:02}"),
        TimeOfDayFormat::Hours12 => format!("{twelve}:{minute:02}:{second:02} {meridiem}"),
        TimeOfDayFormat::Hours12Minutes => format!("{twelve}:{minute:02} {meridiem}"),
        TimeOfDayFormat::Date => format!("{year:04}-{month:02}-{day:02}"),
        TimeOfDayFormat::Rfc3339 => {
            let offset = local.utc_offset_seconds;
            let sign = if offset < 0 { '-' } else { '+' };
            let magnitude = offset.unsigned_abs();
            // RFC 3339 offsets have minute precision; sub-minute offsets
            // (historical only) are truncated.
            format!(
                "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{sign}{:02}:{:02}",
                magnitude / 3_600,
                magnitude / 60 % 60
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_division_rounds_in_the_requested_direction() {
        assert_eq!(div_floor(7, 2), 3);
        assert_eq!(div_floor(-7, 2), -4);
        assert_eq!(div_floor(-8, 2), -4);
        assert_eq!(div_ceil(7, 2), 4);
        assert_eq!(div_ceil(-7, 2), -3);
        assert_eq!(div_ceil(8, 2), 4);
        assert_eq!(div_ceil(0, 2), 0);
    }
}
