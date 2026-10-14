//! Production clocks: operator-facing timers and time displays.
//!
//! This module is the deterministic model behind the clocks an operator puts
//! on screen or reads through the API: count-up and countdown timers,
//! time-of-day in an explicit time zone, remaining media time, and
//! stream/record/replay session timers.
//!
//! # Time sources
//!
//! Nothing here reads a system clock. Two independent inputs are supplied by
//! the caller in a [`TimeReading`]:
//!
//! - a monotonic [`ClockTime`](crate::ClockTime) that drives every timer, so timers never jump
//!   when the wall clock is stepped by NTP or an operator;
//! - a [`UtcInstant`] that is used only for time-of-day displays.
//!
//! Mutating operations take the monotonic time at which they happen and
//! reject time that moves backward. Reads never mutate and are total.
//!
//! # Values and display
//!
//! Timer values are signed nanoseconds ([`ClockValue`]) bounded by
//! [`ClockValue::MAX`], so a countdown that is allowed to overrun can go
//! negative. Displays are produced by [`format_duration`] with an explicit
//! [`Rounding`]: countdowns round toward positive infinity (a label is shown
//! for the whole unit before it is reached and `00:00` appears exactly at
//! expiry), count-ups round toward negative infinity.
//!
//! # API exposure
//!
//! [`ProductionClocks`] is a bounded, name-keyed registry. It accepts typed
//! [`ClockCommand`]s and produces [`ClockReading`] values whose
//! [`ClockReading::binding_values`] are flat key/value strings suitable for
//! title data binding or a control API. [`parse_clock_value`] parses the
//! strict text form used by commands such as "set countdown to 00:05:00".

mod civil;
mod format;
mod media;
mod registry;
mod session;
mod timer;
mod value;
mod zone;

pub use civil::{
    CivilError, LocalDateTime, ManualWallClock, TimeReading, UtcInstant, WallClock, Weekday,
};
pub use format::{DurationFormat, Rounding, TimeOfDayFormat, format_duration, format_time_of_day};
pub use media::{MediaProgress, MediaProgressError};
pub use registry::{
    ClockCommand, ClockKind, ClockName, ClockNameError, ClockReading, DisplayFormat,
    MAX_CLOCK_NAME_LEN, MAX_PRODUCTION_CLOCKS, ProductionClock, ProductionClockError,
    ProductionClocks, ReadingState, SessionKind,
};
pub use session::{SessionError, SessionSnapshot, SessionTimer};
pub use timer::{
    MAX_PENDING_TIMER_EVENTS, OverrunPolicy, Timer, TimerConfig, TimerDirection, TimerError,
    TimerEvent, TimerPhase, TimerSnapshot,
};
pub use value::{ClockValue, ClockValueError, parse_clock_value};
pub use zone::{DaylightRule, TimeZone, TransitionDate, ZoneError, ZoneResolution};
