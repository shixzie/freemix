//! Locale-aware number, level, size, time, and date formatting.
//!
//! The conventions follow CLDR for the shipped languages: English groups
//! thousands with `,` and uses `.` as the decimal mark; Spanish uses `.` and
//! `,`, groups only from five digits (`1234`, `12.345`), and separates a
//! percent sign with a no-break space (`45 %`). Signs stay ASCII so typed
//! values round-trip through [`Locale::parse_decimal`].

use super::{Arg, HourCycle, Language, Locale, Msg};

/// No-break space between a number and a unit or percent sign.
const NBSP: char = '\u{a0}';

struct NumberSymbols {
    decimal: char,
    group: char,
    /// Digits an integer part needs before it is grouped at all.
    min_grouping_digits: usize,
    percent_space: bool,
}

const fn symbols(language: Language) -> NumberSymbols {
    match language {
        Language::English => NumberSymbols {
            decimal: '.',
            group: ',',
            min_grouping_digits: 4,
            percent_space: false,
        },
        Language::Spanish => NumberSymbols {
            decimal: ',',
            group: '.',
            min_grouping_digits: 5,
            percent_space: true,
        },
    }
}

impl Locale {
    /// The decimal mark of the active language.
    #[must_use]
    pub const fn decimal_separator(self) -> char {
        symbols(self.language).decimal
    }

    /// An unsigned integer with locale digit grouping (`12,345` / `12.345`).
    #[must_use]
    pub fn integer(self, value: u64) -> String {
        self.group_digits(&value.to_string())
    }

    /// A signed integer with locale digit grouping.
    #[must_use]
    pub fn signed_integer(self, value: i64) -> String {
        let grouped = self.integer(value.unsigned_abs());
        if value < 0 {
            format!("-{grouped}")
        } else {
            grouped
        }
    }

    /// A finite value with `places` decimals, locale grouping, and decimal
    /// mark (`1,234.5` / `1234,5`). Non-finite values print as `-inf`,
    /// `inf`, or `NaN`.
    #[must_use]
    pub fn decimal(self, value: f64, places: usize) -> String {
        self.decimal_with_sign(value, places, false)
    }

    /// Like [`Self::decimal`] with an explicit `+` on non-negative values.
    #[must_use]
    pub fn signed_decimal(self, value: f64, places: usize) -> String {
        self.decimal_with_sign(value, places, true)
    }

    fn decimal_with_sign(self, value: f64, places: usize, plus: bool) -> String {
        if !value.is_finite() {
            return if value.is_nan() {
                "NaN".to_owned()
            } else if value < 0.0 {
                "-inf".to_owned()
            } else {
                "inf".to_owned()
            };
        }
        let raw = format!("{:.*}", places, value.abs());
        let negative =
            value.is_sign_negative() && raw.bytes().any(|b| b.is_ascii_digit() && b != b'0');
        let (whole, fraction) = raw.split_once('.').unwrap_or((&raw, ""));
        let mut out = String::with_capacity(raw.len() + 4);
        if negative {
            out.push('-');
        } else if plus {
            out.push('+');
        }
        out.push_str(&self.group_digits(whole));
        if !fraction.is_empty() {
            out.push(self.decimal_separator());
            out.push_str(fraction);
        }
        out
    }

    /// A whole percentage (`45%` / `45 %`).
    #[must_use]
    pub fn percent(self, percent: i64) -> String {
        let number = self.signed_integer(percent);
        if symbols(self.language).percent_space {
            format!("{number}{NBSP}%")
        } else {
            format!("{number}%")
        }
    }

    /// A signed level with one decimal (`+0.0 dB`, `-6,5 dB`); levels at or
    /// below `floor_db`, and non-finite levels, read `-inf dB`.
    #[must_use]
    pub fn decibels(self, db: f64, floor_db: f64) -> String {
        if db.is_finite() && db > floor_db {
            format!("{} dB", self.signed_decimal(db, 1))
        } else {
            "-inf dB".to_owned()
        }
    }

    /// A byte count in binary units with one decimal (`1023 B`, `1.5 MB`,
    /// `1,5 MB`). Steps up on the displayed value, so 1023.96 KB reads
    /// `1.0 MB` rather than `1024.0 KB`.
    #[must_use]
    pub fn bytes(self, bytes: u64) -> String {
        const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
        if bytes < 1_024 {
            return format!("{bytes} B");
        }
        #[allow(clippy::cast_precision_loss)] // display only
        let mut value = bytes as f64 / 1_024.0;
        let mut unit = 0;
        while (value * 10.0).round() >= 10_240.0 && unit + 1 < UNITS.len() {
            value /= 1_024.0;
            unit += 1;
        }
        // Byte sizes are never digit-grouped: `16777216.0 TB` stays legible
        // and matches the English console's existing readout.
        let raw = format!("{value:.1}");
        format!(
            "{} {}",
            raw.replace('.', &self.decimal_separator().to_string()),
            UNITS[unit]
        )
    }

    /// A duration in milliseconds as seconds with one decimal, truncated
    /// (`1.5 s` / `1,5 s`).
    #[must_use]
    pub fn seconds_tenths(self, milliseconds: u64) -> String {
        format!(
            "{}{}{} s",
            self.integer(milliseconds / 1_000),
            self.decimal_separator(),
            (milliseconds % 1_000) / 100
        )
    }

    /// A time of day from seconds since midnight (wrapping past a day), in
    /// the locale's hour cycle: `13:05:09`, `1:05:09 PM`, `1:05:09 p. m.`.
    #[must_use]
    pub fn time_of_day(self, seconds: u64) -> String {
        let seconds = seconds % 86_400;
        let (hour, minute, second) = (seconds / 3_600, (seconds / 60) % 60, seconds % 60);
        match self.hour_cycle {
            HourCycle::H23 => format!("{hour:02}:{minute:02}:{second:02}"),
            HourCycle::H12 => {
                let marker = if hour < 12 { Msg::TimeAm } else { Msg::TimePm };
                let twelve = match hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                format!(
                    "{twelve}:{minute:02}:{second:02}{NBSP}{}",
                    self.text(marker)
                )
            }
        }
    }

    /// A calendar date from days since 1970-01-01 (proleptic Gregorian):
    /// `Oct 14, 2026` / `14 oct 2026`.
    #[must_use]
    pub fn date(self, days_since_epoch: i64) -> String {
        let (year, month, day) = civil_from_days(days_since_epoch);
        let month = self.text(MONTHS[usize::from(month - 1)]);
        self.format(
            Msg::DateLong,
            &[
                ("day", Arg::Plain(u64::from(day))),
                ("month", Arg::Str(month)),
                ("year", Arg::Str(&year.to_string())),
            ],
        )
    }

    /// Parses a typed decimal in this locale. The locale's decimal mark is
    /// accepted, and so is `.` (keypads and habits type it everywhere);
    /// grouping separators are not, so `1,5` is refused in English rather
    /// than misread as fifteen.
    #[must_use]
    pub fn parse_decimal(self, text: &str) -> Option<f64> {
        let text = text.trim();
        let normalized = match self.decimal_separator() {
            '.' => text.to_owned(),
            mark => {
                if text.contains(mark) && text.contains('.') {
                    return None;
                }
                text.replace(mark, ".")
            }
        };
        normalized
            .parse::<f64>()
            .ok()
            .filter(|value| !value.is_nan())
    }

    fn group_digits(self, digits: &str) -> String {
        let NumberSymbols {
            group,
            min_grouping_digits,
            ..
        } = symbols(self.language);
        if digits.len() < min_grouping_digits {
            return digits.to_owned();
        }
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                out.push(group);
            }
            out.push(digit);
        }
        out
    }
}

const MONTHS: [Msg; 12] = [
    Msg::MonthJan,
    Msg::MonthFeb,
    Msg::MonthMar,
    Msg::MonthApr,
    Msg::MonthMay,
    Msg::MonthJun,
    Msg::MonthJul,
    Msg::MonthAug,
    Msg::MonthSep,
    Msg::MonthOct,
    Msg::MonthNov,
    Msg::MonthDec,
];

/// Howard Hinnant's `civil_from_days`: (year, month 1–12, day 1–31).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // bounded by construction
const fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
