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

use core::fmt;

use fm_clock::production::{
    ClockName, DisplayFormat, ProductionClockError, ProductionClocks, TimeReading,
};

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
