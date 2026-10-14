//! A bounded, name-keyed set of production clocks with typed commands and
//! flat readings for API and title data binding.

use core::fmt;
use std::collections::BTreeMap;

use super::{
    ClockValue, DurationFormat, LocalDateTime, MediaProgress, Rounding, SessionError, SessionTimer,
    TimeOfDayFormat, TimeReading, TimeZone, Timer, TimerConfig, TimerError, TimerEvent, TimerPhase,
    format_duration, format_time_of_day,
};
use crate::ClockTime;

pub const MAX_PRODUCTION_CLOCKS: usize = 256;
pub const MAX_CLOCK_NAME_LEN: usize = 64;

/// A clock name: 1 to 64 ASCII letters, digits, `_` or `-`. Dots are
/// excluded because binding keys use `name.field`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClockName(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockNameError {
    Empty,
    TooLong,
    InvalidCharacter,
}

impl fmt::Display for ClockNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "clock name is empty",
            Self::TooLong => "clock name exceeds 64 bytes",
            Self::InvalidCharacter => "clock names use only ASCII letters, digits, '_' and '-'",
        })
    }
}

impl std::error::Error for ClockNameError {}

impl ClockName {
    /// Validates a clock name.
    ///
    /// # Errors
    ///
    /// Returns a [`ClockNameError`] for an empty, too long, or non-ASCII-word
    /// name.
    pub fn new(name: &str) -> Result<Self, ClockNameError> {
        if name.is_empty() {
            return Err(ClockNameError::Empty);
        }
        if name.len() > MAX_CLOCK_NAME_LEN {
            return Err(ClockNameError::TooLong);
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(ClockNameError::InvalidCharacter);
        }
        Ok(Self(name.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClockName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The output a session clock follows.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionKind {
    Stream,
    Recording,
    Replay,
    MultiCorder,
    External,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClockKind {
    Timer,
    TimeOfDay,
    MediaRemaining,
    Session(SessionKind),
}

impl ClockKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::TimeOfDay => "time_of_day",
            Self::MediaRemaining => "media_remaining",
            Self::Session(SessionKind::Stream) => "stream",
            Self::Session(SessionKind::Recording) => "recording",
            Self::Session(SessionKind::Replay) => "replay",
            Self::Session(SessionKind::MultiCorder) => "multicorder",
            Self::Session(SessionKind::External) => "external",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DisplayFormat {
    Duration(DurationFormat),
    TimeOfDay(TimeOfDayFormat),
}

/// One configured clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductionClock {
    Timer {
        timer: Timer,
        format: DurationFormat,
    },
    TimeOfDay {
        zone: TimeZone,
        format: TimeOfDayFormat,
    },
    MediaRemaining {
        progress: Option<MediaProgress>,
        format: DurationFormat,
    },
    Session {
        kind: SessionKind,
        timer: SessionTimer,
        format: DurationFormat,
    },
}

impl ProductionClock {
    /// An idle count-up or countdown timer.
    ///
    /// # Errors
    ///
    /// Returns the [`TimerError`] for an invalid configuration.
    pub fn timer(config: TimerConfig, format: DurationFormat) -> Result<Self, TimerError> {
        Ok(Self::Timer {
            timer: Timer::new(config)?,
            format,
        })
    }

    #[must_use]
    pub const fn time_of_day(zone: TimeZone, format: TimeOfDayFormat) -> Self {
        Self::TimeOfDay { zone, format }
    }

    /// Remaining time of a media item, unavailable until the first update.
    #[must_use]
    pub const fn media_remaining(format: DurationFormat) -> Self {
        Self::MediaRemaining {
            progress: None,
            format,
        }
    }

    #[must_use]
    pub const fn session(kind: SessionKind, format: DurationFormat) -> Self {
        Self::Session {
            kind,
            timer: SessionTimer::new(),
            format,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ClockKind {
        match self {
            Self::Timer { .. } => ClockKind::Timer,
            Self::TimeOfDay { .. } => ClockKind::TimeOfDay,
            Self::MediaRemaining { .. } => ClockKind::MediaRemaining,
            Self::Session { kind, .. } => ClockKind::Session(*kind),
        }
    }

    /// The clock's configured display format.
    #[must_use]
    pub const fn display_format(&self) -> DisplayFormat {
        match self {
            Self::Timer { format, .. }
            | Self::MediaRemaining { format, .. }
            | Self::Session { format, .. } => DisplayFormat::Duration(*format),
            Self::TimeOfDay { format, .. } => DisplayFormat::TimeOfDay(*format),
        }
    }

    /// Whether `format` is of the kind this clock displays: a duration
    /// format for timers, media and sessions, a time-of-day format for time
    /// of day.
    #[must_use]
    pub const fn accepts_format(&self, format: DisplayFormat) -> bool {
        matches!(
            (self, format),
            (
                Self::Timer { .. } | Self::MediaRemaining { .. } | Self::Session { .. },
                DisplayFormat::Duration(_)
            ) | (Self::TimeOfDay { .. }, DisplayFormat::TimeOfDay(_))
        )
    }
}

/// Operator or API commands. Each is valid only for some clock kinds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClockCommand {
    /// Timer: start or resume.
    Start,
    /// Timer: pause.
    Pause,
    /// Timer: back to the preset; session: clear session, total and count.
    Reset,
    /// Timer: set the current value.
    SetValue(ClockValue),
    /// Timer: add a signed amount.
    Adjust(ClockValue),
    /// Timer: change the preset and reset.
    SetPreset(ClockValue),
    /// Session: the followed output started.
    SessionStarted,
    /// Session: the followed output paused.
    SessionPaused,
    /// Session: the followed output resumed.
    SessionResumed,
    /// Session: the followed output stopped.
    SessionStopped,
    /// Media: new position, or `None` when no media is loaded.
    UpdateMedia(Option<MediaProgress>),
    /// Time of day: replace the zone.
    SetTimeZone(TimeZone),
    /// Any clock: replace the display format with one of the matching kind.
    SetFormat(DisplayFormat),
}

impl ClockCommand {
    const fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Pause => "pause",
            Self::Reset => "reset",
            Self::SetValue(_) => "set_value",
            Self::Adjust(_) => "adjust",
            Self::SetPreset(_) => "set_preset",
            Self::SessionStarted => "session_started",
            Self::SessionPaused => "session_paused",
            Self::SessionResumed => "session_resumed",
            Self::SessionStopped => "session_stopped",
            Self::UpdateMedia(_) => "update_media",
            Self::SetTimeZone(_) => "set_time_zone",
            Self::SetFormat(_) => "set_format",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductionClockError {
    UnknownClock(ClockName),
    DuplicateClock(ClockName),
    CapacityExceeded,
    UnsupportedCommand {
        kind: ClockKind,
        command: &'static str,
    },
    /// A duration format given to a time-of-day clock or vice versa.
    FormatMismatch,
    TimeRegression {
        last: ClockTime,
        attempted: ClockTime,
    },
    Timer(TimerError),
}

impl fmt::Display for ProductionClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownClock(name) => write!(formatter, "unknown clock {name}"),
            Self::DuplicateClock(name) => write!(formatter, "clock {name} already exists"),
            Self::CapacityExceeded => write!(
                formatter,
                "at most {MAX_PRODUCTION_CLOCKS} production clocks are supported"
            ),
            Self::UnsupportedCommand { kind, command } => write!(
                formatter,
                "command {command} does not apply to a {} clock",
                kind.as_str()
            ),
            Self::FormatMismatch => {
                formatter.write_str("display format does not match the clock kind")
            }
            Self::TimeRegression { last, attempted } => write!(
                formatter,
                "clock operation at {}ns precedes the previous operation at {}ns",
                attempted.as_nanos(),
                last.as_nanos()
            ),
            Self::Timer(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProductionClockError {}

impl From<TimerError> for ProductionClockError {
    fn from(error: TimerError) -> Self {
        Self::Timer(error)
    }
}

impl From<SessionError> for ProductionClockError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::TimeRegression { last, attempted } => {
                Self::TimeRegression { last, attempted }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReadingState {
    /// Timer at its preset.
    Idle,
    /// Timer running or session output active.
    Running,
    /// Timer or session output paused.
    Paused,
    /// Timer stopped at its target.
    Expired,
    /// Session output not active.
    Stopped,
    /// Time of day, or media with a known position.
    Live,
    /// Media clock with nothing loaded.
    Unavailable,
}

impl ReadingState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Expired => "expired",
            Self::Stopped => "stopped",
            Self::Live => "live",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A clock's state at one [`TimeReading`], ready for an API response or a
/// title data binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockReading {
    pub name: ClockName,
    pub kind: ClockKind,
    pub state: ReadingState,
    /// Formatted display text.
    pub text: String,
    /// Signed value for duration clocks; `None` for time of day and for
    /// unavailable media.
    pub value: Option<ClockValue>,
    /// Timer target reached (and, for overrunning timers, still past it).
    pub expired: bool,
    /// Local civil time for time-of-day clocks.
    pub local: Option<LocalDateTime>,
    /// Zone abbreviation in effect for time-of-day clocks.
    pub zone: Option<String>,
    /// Session clocks: duration summed over all sessions.
    pub total: Option<ClockValue>,
}

impl ClockReading {
    /// Flat `key = value` strings in a fixed order. The display text is
    /// bound to the bare name; other keys are `name.field`:
    /// `kind`, `state`, `ms` (signed milliseconds, truncated toward zero, or
    /// empty), `expired`, and, when present, `zone`, `utc_offset`,
    /// `total_ms`.
    #[must_use]
    pub fn binding_values(&self) -> Vec<(String, String)> {
        let name = self.name.as_str();
        let mut values = vec![
            (name.to_owned(), self.text.clone()),
            (format!("{name}.kind"), self.kind.as_str().to_owned()),
            (format!("{name}.state"), self.state.as_str().to_owned()),
            (
                format!("{name}.ms"),
                self.value
                    .map_or_else(String::new, |value| value.as_millis().to_string()),
            ),
            (format!("{name}.expired"), self.expired.to_string()),
        ];
        if let Some(zone) = &self.zone {
            values.push((format!("{name}.zone"), zone.clone()));
        }
        if let Some(local) = &self.local {
            let offset = local.utc_offset_seconds;
            let magnitude = offset.unsigned_abs();
            values.push((
                format!("{name}.utc_offset"),
                format!(
                    "{}{:02}:{:02}",
                    if offset < 0 { '-' } else { '+' },
                    magnitude / 3_600,
                    magnitude / 60 % 60
                ),
            ));
        }
        if let Some(total) = self.total {
            values.push((format!("{name}.total_ms"), total.as_millis().to_string()));
        }
        values
    }
}

/// The registry. All mutating operations share one monotonic order.
#[derive(Clone, Debug, Default)]
pub struct ProductionClocks {
    clocks: BTreeMap<ClockName, ProductionClock>,
    last_operation: Option<ClockTime>,
}

impl ProductionClocks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.clocks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clocks.is_empty()
    }

    #[must_use]
    pub fn get(&self, name: &ClockName) -> Option<&ProductionClock> {
        self.clocks.get(name)
    }

    /// Clock names in their deterministic (byte-wise) order.
    pub fn names(&self) -> impl Iterator<Item = &ClockName> {
        self.clocks.keys()
    }

    /// Adds a clock.
    ///
    /// # Errors
    ///
    /// Returns [`ProductionClockError::DuplicateClock`] or
    /// [`ProductionClockError::CapacityExceeded`].
    pub fn insert(
        &mut self,
        name: ClockName,
        clock: ProductionClock,
    ) -> Result<(), ProductionClockError> {
        if self.clocks.contains_key(&name) {
            return Err(ProductionClockError::DuplicateClock(name));
        }
        if self.clocks.len() == MAX_PRODUCTION_CLOCKS {
            return Err(ProductionClockError::CapacityExceeded);
        }
        self.clocks.insert(name, clock);
        Ok(())
    }

    /// Removes a clock, returning it.
    ///
    /// # Errors
    ///
    /// Returns [`ProductionClockError::UnknownClock`].
    pub fn remove(&mut self, name: &ClockName) -> Result<ProductionClock, ProductionClockError> {
        self.clocks
            .remove(name)
            .ok_or_else(|| ProductionClockError::UnknownClock(name.clone()))
    }

    /// Applies one command at monotonic time `now`. The registry is
    /// unchanged on error.
    ///
    /// # Errors
    ///
    /// Returns [`ProductionClockError`] for an unknown clock, a command that
    /// does not apply to the clock kind, a mismatched format, time moving
    /// backward, or a timer error.
    pub fn apply(
        &mut self,
        name: &ClockName,
        command: ClockCommand,
        now: ClockTime,
    ) -> Result<(), ProductionClockError> {
        self.check_time(now)?;
        let clock = self
            .clocks
            .get(name)
            .ok_or_else(|| ProductionClockError::UnknownClock(name.clone()))?;
        let mut updated = clock.clone();
        apply_to(&mut updated, command, now)?;
        self.clocks.insert(name.clone(), updated);
        self.last_operation = Some(now);
        Ok(())
    }

    /// Settles every timer at `now` and drains their events in name order.
    ///
    /// # Errors
    ///
    /// Returns [`ProductionClockError::TimeRegression`].
    pub fn poll(
        &mut self,
        now: ClockTime,
    ) -> Result<Vec<(ClockName, TimerEvent)>, ProductionClockError> {
        self.check_time(now)?;
        self.last_operation = Some(now);
        let mut events = Vec::new();
        for (name, clock) in &mut self.clocks {
            if let ProductionClock::Timer { timer, .. } = clock {
                for event in timer.poll(now)? {
                    events.push((name.clone(), event));
                }
            }
        }
        Ok(events)
    }

    /// Reads one clock.
    #[must_use]
    pub fn reading(&self, name: &ClockName, at: TimeReading) -> Option<ClockReading> {
        self.clocks
            .get(name)
            .map(|clock| read(name, clock, at, clock.display_format()))
    }

    /// Reads one clock with its text in `format` instead of the clock's
    /// configured format, without changing the clock. State, value, and
    /// rounding are exactly those of [`Self::reading`]; only
    /// [`ClockReading::text`] differs. This is how a consumer such as a title
    /// element shows one shared clock in its own preset.
    ///
    /// # Errors
    ///
    /// Returns [`ProductionClockError::UnknownClock`], or
    /// [`ProductionClockError::FormatMismatch`] when `format` is not of the
    /// clock's kind (see [`ProductionClock::accepts_format`]).
    pub fn reading_with_format(
        &self,
        name: &ClockName,
        at: TimeReading,
        format: DisplayFormat,
    ) -> Result<ClockReading, ProductionClockError> {
        let clock = self
            .clocks
            .get(name)
            .ok_or_else(|| ProductionClockError::UnknownClock(name.clone()))?;
        if !clock.accepts_format(format) {
            return Err(ProductionClockError::FormatMismatch);
        }
        Ok(read(name, clock, at, format))
    }

    /// Reads every clock in name order.
    #[must_use]
    pub fn readings(&self, at: TimeReading) -> Vec<ClockReading> {
        self.clocks
            .iter()
            .map(|(name, clock)| read(name, clock, at, clock.display_format()))
            .collect()
    }

    fn check_time(&self, now: ClockTime) -> Result<(), ProductionClockError> {
        match self.last_operation {
            Some(last) if now < last => Err(ProductionClockError::TimeRegression {
                last,
                attempted: now,
            }),
            _ => Ok(()),
        }
    }
}

fn unsupported(clock: &ProductionClock, command: &ClockCommand) -> ProductionClockError {
    ProductionClockError::UnsupportedCommand {
        kind: clock.kind(),
        command: command.name(),
    }
}

fn apply_to(
    clock: &mut ProductionClock,
    command: ClockCommand,
    now: ClockTime,
) -> Result<(), ProductionClockError> {
    if let ClockCommand::SetFormat(new_format) = command {
        return match (clock, new_format) {
            (
                ProductionClock::Timer { format, .. }
                | ProductionClock::MediaRemaining { format, .. }
                | ProductionClock::Session { format, .. },
                DisplayFormat::Duration(new_format),
            ) => {
                *format = new_format;
                Ok(())
            }
            (ProductionClock::TimeOfDay { format, .. }, DisplayFormat::TimeOfDay(new_format)) => {
                *format = new_format;
                Ok(())
            }
            _ => Err(ProductionClockError::FormatMismatch),
        };
    }
    let error = unsupported(clock, &command);
    match clock {
        ProductionClock::Timer { timer, .. } => match command {
            ClockCommand::Start => timer.start(now)?,
            ClockCommand::Pause => timer.pause(now)?,
            ClockCommand::Reset => timer.reset(now)?,
            ClockCommand::SetValue(value) => timer.set_value(now, value)?,
            ClockCommand::Adjust(delta) => timer.adjust(now, delta)?,
            ClockCommand::SetPreset(preset) => timer.set_preset(now, preset)?,
            _ => return Err(error),
        },
        ProductionClock::Session { timer, .. } => match command {
            ClockCommand::SessionStarted => timer.start(now)?,
            ClockCommand::SessionPaused => timer.pause(now)?,
            ClockCommand::SessionResumed => timer.resume(now)?,
            ClockCommand::SessionStopped => timer.stop(now)?,
            ClockCommand::Reset => timer.clear(now)?,
            _ => return Err(error),
        },
        ProductionClock::MediaRemaining { progress, .. } => match command {
            ClockCommand::UpdateMedia(update) => *progress = update,
            _ => return Err(error),
        },
        ProductionClock::TimeOfDay { zone, .. } => match command {
            ClockCommand::SetTimeZone(new_zone) => *zone = new_zone,
            _ => return Err(error),
        },
    }
    Ok(())
}

fn placeholder(format: DurationFormat) -> String {
    format_duration(ClockValue::ZERO, format, Rounding::Floor)
        .chars()
        .map(|character| {
            if character.is_ascii_digit() {
                '-'
            } else {
                character
            }
        })
        .collect()
}

/// Reads `clock` with its text in `display`. A `display` of the wrong kind
/// (callers check [`ProductionClock::accepts_format`] first) falls back to the
/// clock's own format, keeping the read total.
fn read(
    name: &ClockName,
    clock: &ProductionClock,
    at: TimeReading,
    display: DisplayFormat,
) -> ClockReading {
    let duration_format = |own: DurationFormat| match display {
        DisplayFormat::Duration(format) => format,
        DisplayFormat::TimeOfDay(_) => own,
    };
    let mut reading = ClockReading {
        name: name.clone(),
        kind: clock.kind(),
        state: ReadingState::Live,
        text: String::new(),
        value: None,
        expired: false,
        local: None,
        zone: None,
        total: None,
    };
    match clock {
        ProductionClock::Timer { timer, format } => {
            let snapshot = timer.snapshot(at.monotonic);
            reading.state = match snapshot.phase {
                TimerPhase::Idle => ReadingState::Idle,
                TimerPhase::Running => ReadingState::Running,
                TimerPhase::Paused => ReadingState::Paused,
                TimerPhase::Expired => ReadingState::Expired,
            };
            reading.text = format_duration(
                snapshot.value,
                duration_format(*format),
                snapshot.config.direction.rounding(),
            );
            reading.value = Some(snapshot.value);
            reading.expired = snapshot.expired;
        }
        ProductionClock::TimeOfDay { zone, format } => {
            let resolution = zone.resolve(at.utc);
            let local = at.utc.to_civil(resolution.utc_offset_seconds);
            let format = match display {
                DisplayFormat::TimeOfDay(format) => format,
                DisplayFormat::Duration(_) => *format,
            };
            reading.text = format_time_of_day(&local, format);
            reading.local = Some(local);
            reading.zone = Some(resolution.abbreviation.to_owned());
        }
        ProductionClock::MediaRemaining { progress, format } => {
            if let Some(progress) = progress {
                let remaining = progress.remaining();
                reading.text = format_duration(remaining, duration_format(*format), Rounding::Ceil);
                reading.value = Some(remaining);
                reading.expired = progress.is_finished();
            } else {
                reading.state = ReadingState::Unavailable;
                reading.text = placeholder(duration_format(*format));
            }
        }
        ProductionClock::Session { timer, format, .. } => {
            let snapshot = timer.snapshot(at.monotonic);
            reading.state = if snapshot.paused {
                ReadingState::Paused
            } else if snapshot.active {
                ReadingState::Running
            } else {
                ReadingState::Stopped
            };
            reading.text =
                format_duration(snapshot.session, duration_format(*format), Rounding::Floor);
            reading.value = Some(snapshot.session);
            reading.total = Some(snapshot.total);
        }
    }
    reading
}
