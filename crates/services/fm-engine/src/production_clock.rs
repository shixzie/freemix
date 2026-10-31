//! Project production clock definitions bridged into the live registry
//! (SW-013).

use core::fmt;

use fm_clock::production::{
    ClockName, ClockNameError, ClockValue, ClockValueError, DurationFormat, OverrunPolicy,
    TimerError,
    ProductionClock, ProductionClockError, ProductionClocks, SessionKind, TimeOfDayFormat,
    TimeZone, TimerConfig, TimerDirection, ZoneError,
};
use fm_types::{
    FrameRate, ProjectClock, ProjectClockDirection, ProjectClockKind, ProjectClockOverrun,
    ProjectDurationFormat, ProjectSessionKind, ProjectTimeFormat, RateError,
};

/// Why a project clock definition could not become a live clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineProductionClockError {
    Name(ClockNameError),
    Zone(ZoneError),
    FrameRate(RateError),
    Value(ClockValueError),
    Registry(ProductionClockError),
}

impl fmt::Display for EngineProductionClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(error) => error.fmt(formatter),
            Self::Zone(error) => error.fmt(formatter),
            Self::FrameRate(error) => error.fmt(formatter),
            Self::Value(error) => error.fmt(formatter),
            Self::Registry(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EngineProductionClockError {}

impl From<ClockNameError> for EngineProductionClockError {
    fn from(error: ClockNameError) -> Self {
        Self::Name(error)
    }
}

impl From<ZoneError> for EngineProductionClockError {
    fn from(error: ZoneError) -> Self {
        Self::Zone(error)
    }
}

impl From<RateError> for EngineProductionClockError {
    fn from(error: RateError) -> Self {
        Self::FrameRate(error)
    }
}

impl From<ClockValueError> for EngineProductionClockError {
    fn from(error: ClockValueError) -> Self {
        Self::Value(error)
    }
}

impl From<ProductionClockError> for EngineProductionClockError {
    fn from(error: ProductionClockError) -> Self {
        Self::Registry(error)
    }
}

impl From<TimerError> for EngineProductionClockError {
    fn from(error: TimerError) -> Self {
        Self::Registry(ProductionClockError::Timer(error))
    }
}

/// Builds the live registry from `definitions`, skipping invalid entries.
///
/// Definitions that reach an engine are validated by the project model; a
/// hand-built show with an unparseable zone or an out-of-range value simply
/// contributes no clock rather than making the engine unusable.
#[must_use]
pub fn registry_from_definitions(definitions: &[ProjectClock]) -> ProductionClocks {
    let mut registry = ProductionClocks::new();
    for definition in definitions {
        let Ok(name) = ClockName::new(&definition.name) else {
            continue;
        };
        let Ok(clock) = production_clock_from_definition(definition) else {
            continue;
        };
        let _ = registry.insert(name, clock);
    }
    registry
}

/// Converts one project clock definition into a live registry clock.
///
/// # Errors
///
/// Returns [`EngineProductionClockError`] for an invalid name, zone, frame
/// rate, value, or timer configuration.
pub fn production_clock_from_definition(
    definition: &ProjectClock,
) -> Result<ProductionClock, EngineProductionClockError> {
    let _ = ClockName::new(&definition.name)?;
    match &definition.kind {
        ProjectClockKind::Timer {
            direction,
            preset_nanos,
            limit_nanos,
            overrun,
            format,
        } => {
            let config = TimerConfig {
                direction: match direction {
                    ProjectClockDirection::CountUp => TimerDirection::CountUp,
                    ProjectClockDirection::CountDown => TimerDirection::CountDown,
                },
                preset: ClockValue::from_nanos(*preset_nanos)?,
                limit: limit_nanos
                    .map(ClockValue::from_nanos)
                    .transpose()
                    .map_err(EngineProductionClockError::from)?,
                overrun: match overrun {
                    ProjectClockOverrun::StopAtTarget => OverrunPolicy::StopAtTarget,
                    ProjectClockOverrun::Continue => OverrunPolicy::Continue,
                },
            };
            Ok(ProductionClock::timer(config, duration_format(*format)?)?)
        }
        ProjectClockKind::TimeOfDay { zone, format } => Ok(ProductionClock::time_of_day(
            TimeZone::parse_posix(zone)?,
            time_format(*format),
        )),
        ProjectClockKind::MediaRemaining { format } => {
            Ok(ProductionClock::media_remaining(duration_format(*format)?))
        }
        ProjectClockKind::Session { kind, format } => Ok(ProductionClock::session(
            match kind {
                ProjectSessionKind::Stream => SessionKind::Stream,
                ProjectSessionKind::Recording => SessionKind::Recording,
                ProjectSessionKind::Replay => SessionKind::Replay,
                ProjectSessionKind::MultiCorder => SessionKind::MultiCorder,
                ProjectSessionKind::External => SessionKind::External,
            },
            duration_format(*format)?,
        )),
    }
}

fn duration_format(format: ProjectDurationFormat) -> Result<DurationFormat, RateError> {
    Ok(match format {
        ProjectDurationFormat::HoursMinutesSeconds => DurationFormat::HoursMinutesSeconds,
        ProjectDurationFormat::MinutesSeconds => DurationFormat::MinutesSeconds,
        ProjectDurationFormat::Seconds => DurationFormat::Seconds,
        ProjectDurationFormat::HoursMinutesSecondsTenths => DurationFormat::HoursMinutesSecondsTenths,
        ProjectDurationFormat::MinutesSecondsTenths => DurationFormat::MinutesSecondsTenths,
        ProjectDurationFormat::HoursMinutesSecondsMillis => DurationFormat::HoursMinutesSecondsMillis,
        ProjectDurationFormat::HoursMinutesSecondsFrames {
            numerator,
            denominator,
        } => DurationFormat::HoursMinutesSecondsFrames(FrameRate::new(numerator, denominator)?),
        ProjectDurationFormat::Compact => DurationFormat::Compact,
    })
}

const fn time_format(format: ProjectTimeFormat) -> TimeOfDayFormat {
    match format {
        ProjectTimeFormat::Hours24 => TimeOfDayFormat::Hours24,
        ProjectTimeFormat::Hours24Minutes => TimeOfDayFormat::Hours24Minutes,
        ProjectTimeFormat::Hours12 => TimeOfDayFormat::Hours12,
        ProjectTimeFormat::Hours12Minutes => TimeOfDayFormat::Hours12Minutes,
        ProjectTimeFormat::Date => TimeOfDayFormat::Date,
        ProjectTimeFormat::Rfc3339 => TimeOfDayFormat::Rfc3339,
    }
}
