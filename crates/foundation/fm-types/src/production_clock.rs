//! Project-level production clock definitions (SW-013).
//!
//! These are the clocks a show defines once and every operator surface can
//! read or control, as opposed to the per-template clocks a title document
//! carries. The live registry lives in `fm-clock`; this module is the
//! persisted, foundation-layer shape so `fm-model` and `fm-persistence` can
//! carry it without depending on the media layer.
//!
//! Values are signed nanoseconds within
//! `0..=MAX_PROJECT_CLOCK_NANOS`, names are the same ASCII-word form the
//! production registry uses, and a time-of-day clock names its POSIX zone.
//! Zones are validated by the media layer when a document is decoded; this
//! module only requires a non-empty, bounded string.

use core::fmt;

/// Maximum clocks one project may define.
pub const MAX_PROJECT_CLOCKS: usize = 256;
/// Maximum clock name length in bytes.
pub const MAX_PROJECT_CLOCK_NAME_LEN: usize = 64;
/// Maximum POSIX time-zone text length in bytes.
pub const MAX_PROJECT_CLOCK_ZONE_LEN: usize = 256;
/// Largest clock value: 100,000 hours, matching `fm-clock`'s bound.
pub const MAX_PROJECT_CLOCK_NANOS: i64 = 100_000 * 3_600 * 1_000_000_000 - 1;

/// A named clock a project defines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectClock {
    pub name: String,
    pub kind: ProjectClockKind,
}

/// What a project clock shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectClockKind {
    Timer {
        direction: ProjectClockDirection,
        /// Value shown before the first start and after a reset.
        preset_nanos: i64,
        /// Count-up only: the value at which the timer expires.
        limit_nanos: Option<i64>,
        overrun: ProjectClockOverrun,
        format: ProjectDurationFormat,
    },
    TimeOfDay {
        zone: String,
        format: ProjectTimeFormat,
    },
    MediaRemaining {
        /// The media input this clock follows; `None` leaves it unavailable.
        input: Option<crate::InputId>,
        format: ProjectDurationFormat,
    },
    Session {
        kind: ProjectSessionKind,
        format: ProjectDurationFormat,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectClockDirection {
    CountUp,
    CountDown,
}

impl ProjectClockDirection {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::CountUp => "count_up",
            Self::CountDown => "count_down",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "count_up" => Some(Self::CountUp),
            "count_down" => Some(Self::CountDown),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectClockOverrun {
    StopAtTarget,
    Continue,
}

impl ProjectClockOverrun {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::StopAtTarget => "stop_at_target",
            Self::Continue => "continue",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "stop_at_target" => Some(Self::StopAtTarget),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectSessionKind {
    Stream,
    Recording,
    Replay,
    MultiCorder,
    External,
}

impl ProjectSessionKind {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Recording => "recording",
            Self::Replay => "replay",
            Self::MultiCorder => "multicorder",
            Self::External => "external",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "stream" => Some(Self::Stream),
            "recording" => Some(Self::Recording),
            "replay" => Some(Self::Replay),
            "multicorder" => Some(Self::MultiCorder),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectDurationFormat {
    HoursMinutesSeconds,
    MinutesSeconds,
    Seconds,
    HoursMinutesSecondsTenths,
    MinutesSecondsTenths,
    HoursMinutesSecondsMillis,
    HoursMinutesSecondsFrames { numerator: u32, denominator: u32 },
    Compact,
}

impl ProjectDurationFormat {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::HoursMinutesSeconds => "hours_minutes_seconds",
            Self::MinutesSeconds => "minutes_seconds",
            Self::Seconds => "seconds",
            Self::HoursMinutesSecondsTenths => "hours_minutes_seconds_tenths",
            Self::MinutesSecondsTenths => "minutes_seconds_tenths",
            Self::HoursMinutesSecondsMillis => "hours_minutes_seconds_millis",
            Self::HoursMinutesSecondsFrames { .. } => "hours_minutes_seconds_frames",
            Self::Compact => "compact",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "hours_minutes_seconds" => Some(Self::HoursMinutesSeconds),
            "minutes_seconds" => Some(Self::MinutesSeconds),
            "seconds" => Some(Self::Seconds),
            "hours_minutes_seconds_tenths" => Some(Self::HoursMinutesSecondsTenths),
            "minutes_seconds_tenths" => Some(Self::MinutesSecondsTenths),
            "hours_minutes_seconds_millis" => Some(Self::HoursMinutesSecondsMillis),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectTimeFormat {
    Hours24,
    Hours24Minutes,
    Hours12,
    Hours12Minutes,
    Date,
    Rfc3339,
}

impl ProjectTimeFormat {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Hours24 => "hours_24",
            Self::Hours24Minutes => "hours_24_minutes",
            Self::Hours12 => "hours_12",
            Self::Hours12Minutes => "hours_12_minutes",
            Self::Date => "date",
            Self::Rfc3339 => "rfc3339",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "hours_24" => Some(Self::Hours24),
            "hours_24_minutes" => Some(Self::Hours24Minutes),
            "hours_12" => Some(Self::Hours12),
            "hours_12_minutes" => Some(Self::Hours12Minutes),
            "date" => Some(Self::Date),
            "rfc3339" => Some(Self::Rfc3339),
            _ => None,
        }
    }
}

/// Why a list of project clocks is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectClockError {
    TooMany { count: usize, maximum: usize },
    InvalidName(String),
    DuplicateName(String),
    EmptyZone(String),
    ZoneTooLong(String),
    ValueOutOfRange(String),
    InvalidLimit(String),
    InvalidFrameRate(String),
}

impl fmt::Display for ProjectClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany { count, maximum } => {
                write!(formatter, "{count} clocks are defined, maximum is {maximum}")
            }
            Self::InvalidName(name) => write!(
                formatter,
                "clock name {name:?} must be 1 to {MAX_PROJECT_CLOCK_NAME_LEN} ASCII letters, digits, '_' or '-'"
            ),
            Self::DuplicateName(name) => write!(formatter, "clock {name} is defined twice"),
            Self::EmptyZone(name) => write!(formatter, "clock {name} names an empty time zone"),
            Self::ZoneTooLong(name) => write!(
                formatter,
                "clock {name} names a time zone longer than {MAX_PROJECT_CLOCK_ZONE_LEN} bytes"
            ),
            Self::ValueOutOfRange(name) => write!(
                formatter,
                "clock {name} value is outside 0..={MAX_PROJECT_CLOCK_NANOS}"
            ),
            Self::InvalidLimit(name) => write!(
                formatter,
                "clock {name} has a limit before its preset or a countdown limit"
            ),
            Self::InvalidFrameRate(name) => {
                write!(formatter, "clock {name} has an invalid frame-rate format")
            }
        }
    }
}

impl std::error::Error for ProjectClockError {}

/// Validates a project clock list: bounds, names, values, and formats.
///
/// # Errors
///
/// Returns the first [`ProjectClockError`] found.
pub fn validate_project_clocks(clocks: &[ProjectClock]) -> Result<(), ProjectClockError> {
    if clocks.len() > MAX_PROJECT_CLOCKS {
        return Err(ProjectClockError::TooMany {
            count: clocks.len(),
            maximum: MAX_PROJECT_CLOCKS,
        });
    }
    for (index, clock) in clocks.iter().enumerate() {
        let name = &clock.name;
        if name.is_empty()
            || name.len() > MAX_PROJECT_CLOCK_NAME_LEN
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(ProjectClockError::InvalidName(name.clone()));
        }
        if clocks[..index].iter().any(|previous| previous.name == *name) {
            return Err(ProjectClockError::DuplicateName(name.clone()));
        }
        clock.kind.validate(name)?;
    }
    Ok(())
}

impl ProjectClockKind {
    fn validate(&self, name: &str) -> Result<(), ProjectClockError> {
        match self {
            Self::Timer {
                direction,
                preset_nanos,
                limit_nanos,
                format,
                ..
            } => {
                check_value(name, *preset_nanos)?;
                match (*direction, *limit_nanos) {
                    (ProjectClockDirection::CountDown, Some(_)) => {
                        return Err(ProjectClockError::InvalidLimit(name.to_owned()));
                    }
                    (ProjectClockDirection::CountUp, Some(limit)) => {
                        check_value(name, limit)?;
                        if limit < *preset_nanos {
                            return Err(ProjectClockError::InvalidLimit(name.to_owned()));
                        }
                    }
                    (_, None) => {}
                }
                format.validate(name)
            }
            Self::TimeOfDay { zone, format } => {
                if zone.is_empty() {
                    return Err(ProjectClockError::EmptyZone(name.to_owned()));
                }
                if zone.len() > MAX_PROJECT_CLOCK_ZONE_LEN {
                    return Err(ProjectClockError::ZoneTooLong(name.to_owned()));
                }
                let _ = format;
                Ok(())
            }
            Self::MediaRemaining { input: _, format } | Self::Session { format, .. } => {
                format.validate(name)
            }
        }
    }
}

impl ProjectDurationFormat {
    fn validate(self, name: &str) -> Result<(), ProjectClockError> {
        if let Self::HoursMinutesSecondsFrames {
            numerator,
            denominator,
        } = self
            && (numerator == 0 || denominator == 0)
        {
            return Err(ProjectClockError::InvalidFrameRate(name.to_owned()));
        }
        Ok(())
    }
}

fn check_value(name: &str, nanos: i64) -> Result<(), ProjectClockError> {
    if (0..=MAX_PROJECT_CLOCK_NANOS).contains(&nanos) {
        Ok(())
    } else {
        Err(ProjectClockError::ValueOutOfRange(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(name: &str) -> ProjectClock {
        ProjectClock {
            name: name.into(),
            kind: ProjectClockKind::Timer {
                direction: ProjectClockDirection::CountDown,
                preset_nanos: 300_000_000_000,
                limit_nanos: None,
                overrun: ProjectClockOverrun::StopAtTarget,
                format: ProjectDurationFormat::MinutesSeconds,
            },
        }
    }

    #[test]
    fn a_valid_registry_passes() {
        validate_project_clocks(&[
            timer("Countdown"),
            ProjectClock {
                name: "Local".into(),
                kind: ProjectClockKind::TimeOfDay {
                    zone: "EST5EDT,M3.2.0,M11.1.0".into(),
                    format: ProjectTimeFormat::Hours24,
                },
            },
        ])
        .unwrap();
    }

    #[test]
    fn names_values_and_limits_are_strict() {
        for name in ["", "spaces in name", "dot.name"] {
            let mut clock = timer(name);
            clock.name = name.into();
            assert!(validate_project_clocks(&[clock]).is_err(), "{name}");
        }
        assert_eq!(
            validate_project_clocks(&[timer("a"), timer("a")]),
            Err(ProjectClockError::DuplicateName("a".into()))
        );
        let mut clock = timer("a");
        if let ProjectClockKind::Timer { preset_nanos, .. } = &mut clock.kind {
            *preset_nanos = -1;
        }
        assert_eq!(
            validate_project_clocks(&[clock]),
            Err(ProjectClockError::ValueOutOfRange("a".into()))
        );
        let mut clock = timer("a");
        if let ProjectClockKind::Timer { limit_nanos, .. } = &mut clock.kind {
            *limit_nanos = Some(1);
        }
        assert_eq!(
            validate_project_clocks(&[clock]),
            Err(ProjectClockError::InvalidLimit("a".into()))
        );
    }
}
