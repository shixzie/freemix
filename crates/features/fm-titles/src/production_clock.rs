//! Title elements that show a shared production clock.
//!
//! A [`ElementKind::ProductionClock`](crate::ElementKind::ProductionClock)
//! element names a clock in an `fm_clock` [`ProductionClocks`] registry and,
//! optionally, a display preset of its own. The title crate holds no clock
//! state and formats nothing itself: the caller supplies the registry and the
//! [`TimeReading`] for the frame through a [`ClockSource`], and the text is
//! exactly the registry's [`ProductionClocks::reading_with_format`] (or
//! [`ProductionClocks::reading`]) text, so an on-air title, the control API,
//! and a data binding of the same clock always agree.
//!
//! A stored template cannot hold live clock state, so it defines each clock
//! it shows as a [`TitleClock`]: time of day in a POSIX zone, a countdown to
//! a UTC instant, or a stopwatch since the title was taken. A host turns the
//! definitions into a registry for each frame with [`title_clock_registry`],
//! from its own monotonic and wall clocks and the title's take instant.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use fm_clock::ClockTime;
use fm_clock::production::{
    ClockName, ClockValue, DisplayFormat, DurationFormat, MAX_PRODUCTION_CLOCKS, OverrunPolicy,
    ProductionClock, ProductionClockError, ProductionClocks, TimeOfDayFormat, TimeReading,
    TimeZone, Timer, TimerConfig, UtcInstant,
};

use crate::{ElementId, ElementKind, TitleTemplate};

/// What a production clock element shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionClockSpec {
    /// The registry clock to read.
    pub clock: ClockName,
    /// A preset overriding the clock's configured format for this element
    /// only. It must be of the clock's kind: a duration format for timers,
    /// media, and session clocks, a time-of-day format for time of day.
    pub format: Option<DisplayFormat>,
}

impl ProductionClockSpec {
    /// Shows `clock` in its configured format.
    #[must_use]
    pub const fn new(clock: ClockName) -> Self {
        Self {
            clock,
            format: None,
        }
    }

    /// Shows `clock` in `format`.
    #[must_use]
    pub const fn with_format(clock: ClockName, format: DisplayFormat) -> Self {
        Self {
            clock,
            format: Some(format),
        }
    }
}

/// The production clocks a frame reads and the instant it reads them at.
///
/// One source is used for every element of a frame, so two elements bound to
/// the same clock never disagree within a frame.
#[derive(Clone, Copy, Debug)]
pub struct ClockSource<'a> {
    pub clocks: &'a ProductionClocks,
    pub at: TimeReading,
}

impl<'a> ClockSource<'a> {
    #[must_use]
    pub const fn new(clocks: &'a ProductionClocks, at: TimeReading) -> Self {
        Self { clocks, at }
    }
}

/// Why a production clock element has no text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockUnresolved {
    /// The frame was rendered without a [`ClockSource`].
    NoClockSource,
    /// The source has no clock with the element's name.
    UnknownClock,
    /// The element's preset is not of the clock's kind.
    FormatMismatch,
}

impl fmt::Display for ClockUnresolved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoClockSource => "no production clock source was supplied",
            Self::UnknownClock => "the production clock does not exist",
            Self::FormatMismatch => "the display preset does not match the clock kind",
        })
    }
}

impl std::error::Error for ClockUnresolved {}

/// The text a production clock element shows at `source`.
///
/// # Errors
///
/// Returns [`ClockUnresolved::UnknownClock`] or
/// [`ClockUnresolved::FormatMismatch`].
pub fn evaluate_production_clock(
    spec: &ProductionClockSpec,
    source: ClockSource<'_>,
) -> Result<String, ClockUnresolved> {
    let reading = match spec.format {
        None => source
            .clocks
            .reading(&spec.clock, source.at)
            .ok_or(ClockUnresolved::UnknownClock),
        Some(format) => source
            .clocks
            .reading_with_format(&spec.clock, source.at, format)
            .map_err(|error| match error {
                ProductionClockError::FormatMismatch => ClockUnresolved::FormatMismatch,
                _ => ClockUnresolved::UnknownClock,
            }),
    }?;
    Ok(reading.text)
}

/// How a clock a template defines for its own elements is driven.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleClockKind {
    /// Wall-clock time of day in `zone`, whose POSIX rules include daylight
    /// saving.
    TimeOfDay {
        zone: TimeZone,
        format: TimeOfDayFormat,
    },
    /// Time remaining until the UTC instant `target`, rounded up like every
    /// countdown. At the target it shows zero and then holds
    /// ([`OverrunPolicy::StopAtTarget`]) or counts negative
    /// ([`OverrunPolicy::Continue`]).
    CountdownTo {
        target: UtcInstant,
        overrun: OverrunPolicy,
        format: DurationFormat,
    },
    /// Time since the title was taken on air, rounded down like every
    /// count-up; zero while the title is off air.
    SinceTake { format: DurationFormat },
}

impl TitleClockKind {
    /// The clock's own display format.
    #[must_use]
    pub const fn display_format(&self) -> DisplayFormat {
        match self {
            Self::TimeOfDay { format, .. } => DisplayFormat::TimeOfDay(*format),
            Self::CountdownTo { format, .. } | Self::SinceTake { format } => {
                DisplayFormat::Duration(*format)
            }
        }
    }

    /// Whether an element preset is of this clock's kind.
    #[must_use]
    pub const fn accepts_format(&self, format: DisplayFormat) -> bool {
        matches!(
            (self, format),
            (Self::TimeOfDay { .. }, DisplayFormat::TimeOfDay(_))
                | (
                    Self::CountdownTo { .. } | Self::SinceTake { .. },
                    DisplayFormat::Duration(_)
                )
        )
    }
}

/// One named clock a template defines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleClock {
    pub name: ClockName,
    pub kind: TitleClockKind,
}

/// When a title's clocks are read: the frame's [`TimeReading`] and, while the
/// title is on air, the monotonic instant it was taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleClockTime {
    pub at: TimeReading,
    /// `None` while the title is off air. An instant after `at.monotonic` is
    /// treated as off air.
    pub taken_at: Option<ClockTime>,
}

/// Why a template's clock definitions do not fit its elements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleClockError {
    /// More than [`MAX_PRODUCTION_CLOCKS`] clocks.
    TooManyClocks { count: usize },
    /// Two definitions share a name.
    DuplicateClock(ClockName),
    /// A production clock element names a clock the template does not
    /// define.
    UndefinedClock {
        element: ElementId,
        clock: ClockName,
    },
    /// A definition no element shows.
    UnusedClock(ClockName),
    /// An element preset is not of its clock's kind.
    FormatMismatch {
        element: ElementId,
        clock: ClockName,
    },
}

impl fmt::Display for TitleClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyClocks { count } => write!(
                formatter,
                "{count} clocks are defined, maximum is {MAX_PRODUCTION_CLOCKS}"
            ),
            Self::DuplicateClock(name) => write!(formatter, "clock {name} is defined twice"),
            Self::UndefinedClock { element, clock } => write!(
                formatter,
                "element {element} shows clock {clock}, which is not defined"
            ),
            Self::UnusedClock(name) => {
                write!(formatter, "clock {name} is defined but never shown")
            }
            Self::FormatMismatch { element, clock } => write!(
                formatter,
                "element {element} gives clock {clock} a preset of the wrong kind"
            ),
        }
    }
}

impl std::error::Error for TitleClockError {}

/// Requires `clocks` to be distinct, within [`MAX_PRODUCTION_CLOCKS`], and
/// exactly the clocks `template`'s production clock elements show, with
/// every element preset of its clock's kind.
///
/// # Errors
///
/// Returns the first [`TitleClockError`] found.
pub fn validate_title_clocks(
    template: &TitleTemplate,
    clocks: &[TitleClock],
) -> Result<(), TitleClockError> {
    if clocks.len() > MAX_PRODUCTION_CLOCKS {
        return Err(TitleClockError::TooManyClocks {
            count: clocks.len(),
        });
    }
    let mut defined = BTreeMap::new();
    for clock in clocks {
        if defined.insert(&clock.name, &clock.kind).is_some() {
            return Err(TitleClockError::DuplicateClock(clock.name.clone()));
        }
    }
    let mut used = BTreeSet::new();
    for element in &template.elements {
        let ElementKind::ProductionClock(spec) = &element.kind else {
            continue;
        };
        let Some(kind) = defined.get(&spec.clock) else {
            return Err(TitleClockError::UndefinedClock {
                element: element.id,
                clock: spec.clock.clone(),
            });
        };
        if let Some(format) = spec.format
            && !kind.accepts_format(format)
        {
            return Err(TitleClockError::FormatMismatch {
                element: element.id,
                clock: spec.clock.clone(),
            });
        }
        used.insert(&spec.clock);
    }
    if let Some(unused) = defined.keys().find(|name| !used.contains(*name)) {
        return Err(TitleClockError::UnusedClock((*unused).clone()));
    }
    Ok(())
}

/// Builds the registry a frame reads `clocks` from at `time`.
///
/// Every text then comes from `fm_clock`'s formatters: time of day resolves
/// its zone at `time.at.utc`; a countdown-to holds the signed time from
/// `time.at.utc` to its target (clamped to the clock range, and to zero once
/// reached under [`OverrunPolicy::StopAtTarget`]); a since-take stopwatch is
/// started at `time.taken_at` and read at `time.at.monotonic`, or sits at
/// zero while off air.
///
/// # Errors
///
/// Returns [`ProductionClockError`] for more than [`MAX_PRODUCTION_CLOCKS`]
/// clocks or a duplicate name.
pub fn title_clock_registry(
    clocks: &[TitleClock],
    time: TitleClockTime,
) -> Result<ProductionClocks, ProductionClockError> {
    let mut registry = ProductionClocks::new();
    for clock in clocks {
        let production = match &clock.kind {
            TitleClockKind::TimeOfDay { zone, format } => {
                ProductionClock::time_of_day(zone.clone(), *format)
            }
            TitleClockKind::CountdownTo {
                target,
                overrun,
                format,
            } => {
                let mut timer = Timer::new(TimerConfig::countdown(ClockValue::ZERO, *overrun))?;
                let floor = match overrun {
                    OverrunPolicy::StopAtTarget => ClockValue::ZERO,
                    OverrunPolicy::Continue => ClockValue::MIN,
                };
                let remaining = (utc_nanos(*target) - utc_nanos(time.at.utc)).clamp(
                    i128::from(floor.as_nanos()),
                    i128::from(ClockValue::MAX.as_nanos()),
                );
                let value = i64::try_from(remaining)
                    .ok()
                    .and_then(|nanos| ClockValue::from_nanos(nanos).ok())
                    .unwrap_or(floor);
                timer.set_value(time.at.monotonic, value)?;
                ProductionClock::Timer {
                    timer,
                    format: *format,
                }
            }
            TitleClockKind::SinceTake { format } => {
                let mut timer = Timer::new(TimerConfig::stopwatch())?;
                if let Some(taken_at) = time.taken_at
                    && taken_at <= time.at.monotonic
                {
                    timer.start(taken_at)?;
                }
                ProductionClock::Timer {
                    timer,
                    format: *format,
                }
            }
        };
        registry.insert(clock.name.clone(), production)?;
    }
    Ok(registry)
}

fn utc_nanos(instant: UtcInstant) -> i128 {
    i128::from(instant.unix_seconds()) * 1_000_000_000 + i128::from(instant.subsec_nanos())
}
