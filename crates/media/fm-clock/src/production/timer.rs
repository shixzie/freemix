//! Count-up and countdown timers driven by caller-supplied monotonic time.
//!
//! A running timer stores only the time it was anchored and the value at the
//! anchor, so its value at any later instant is one exact subtraction and
//! never accumulates drift. Every mutating operation takes the time at which
//! it happens and first *settles* the timer at that instant: if the target
//! was crossed since the anchor, the exact crossing time is recorded and,
//! under [`OverrunPolicy::StopAtTarget`], the timer stops there.

use core::fmt;

use super::{ClockValue, Rounding};
use crate::{ClockDuration, ClockTime};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TimerDirection {
    CountUp,
    CountDown,
}

impl TimerDirection {
    /// Display rounding that matches this direction.
    #[must_use]
    pub const fn rounding(self) -> Rounding {
        match self {
            Self::CountUp => Rounding::Floor,
            Self::CountDown => Rounding::Ceil,
        }
    }
}

/// What happens when the timer reaches its target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OverrunPolicy {
    /// Stop exactly at the target (zero for a countdown, the limit for a
    /// count-up).
    StopAtTarget,
    /// Keep running past the target: a countdown goes negative, a count-up
    /// exceeds its limit. The value saturates at the supported range.
    Continue,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimerConfig {
    pub direction: TimerDirection,
    /// Value shown before the first start and after a reset. Never negative.
    pub preset: ClockValue,
    /// Count-up only: the value at which the timer expires. A countdown
    /// always targets zero and must not set a limit.
    pub limit: Option<ClockValue>,
    pub overrun: OverrunPolicy,
}

impl TimerConfig {
    /// A countdown from `preset` to zero.
    #[must_use]
    pub const fn countdown(preset: ClockValue, overrun: OverrunPolicy) -> Self {
        Self {
            direction: TimerDirection::CountDown,
            preset,
            limit: None,
            overrun,
        }
    }

    /// A count-up from zero with no limit.
    #[must_use]
    pub const fn stopwatch() -> Self {
        Self {
            direction: TimerDirection::CountUp,
            preset: ClockValue::ZERO,
            limit: None,
            overrun: OverrunPolicy::Continue,
        }
    }

    /// A count-up from `preset` that expires at `limit`.
    #[must_use]
    pub const fn count_up_to(
        preset: ClockValue,
        limit: ClockValue,
        overrun: OverrunPolicy,
    ) -> Self {
        Self {
            direction: TimerDirection::CountUp,
            preset,
            limit: Some(limit),
            overrun,
        }
    }

    fn validate(self) -> Result<Self, TimerError> {
        if self.preset.is_negative() {
            return Err(TimerError::NegativePreset);
        }
        match (self.direction, self.limit) {
            (TimerDirection::CountDown, Some(_)) => Err(TimerError::LimitOnCountdown),
            (TimerDirection::CountUp, Some(limit)) if limit <= self.preset => {
                Err(TimerError::LimitNotAbovePreset)
            }
            _ => Ok(self),
        }
    }

    /// The value at which the timer expires, if any.
    #[must_use]
    pub const fn target(self) -> Option<ClockValue> {
        match self.direction {
            TimerDirection::CountDown => Some(ClockValue::ZERO),
            TimerDirection::CountUp => self.limit,
        }
    }

    /// Whether `value` has reached or passed the target.
    const fn has_reached_target(self, value: ClockValue) -> bool {
        match (self.direction, self.target()) {
            (TimerDirection::CountDown, Some(target)) => value.as_nanos() <= target.as_nanos(),
            (TimerDirection::CountUp, Some(target)) => value.as_nanos() >= target.as_nanos(),
            (_, None) => false,
        }
    }

    /// Smallest and largest values the timer may hold.
    const fn bounds(self) -> (ClockValue, ClockValue) {
        match (self.direction, self.overrun, self.target()) {
            (TimerDirection::CountDown, OverrunPolicy::StopAtTarget, _) => {
                (ClockValue::ZERO, ClockValue::MAX)
            }
            (TimerDirection::CountDown, OverrunPolicy::Continue, _) => {
                (ClockValue::MIN, ClockValue::MAX)
            }
            (TimerDirection::CountUp, OverrunPolicy::StopAtTarget, Some(limit)) => {
                (ClockValue::ZERO, limit)
            }
            (TimerDirection::CountUp, _, _) => (ClockValue::ZERO, ClockValue::MAX),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerError {
    NegativePreset,
    LimitOnCountdown,
    LimitNotAbovePreset,
    /// A value outside what this timer may hold, e.g. a negative value for a
    /// count-up or a stopping countdown, or a value past a stopping limit.
    ValueOutOfRange,
    /// An operation was given a time earlier than a previous operation.
    TimeRegression {
        last: ClockTime,
        attempted: ClockTime,
    },
    /// The timer stopped at its target; reset or set it before starting.
    Expired,
}

impl fmt::Display for TimerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativePreset => formatter.write_str("timer preset must not be negative"),
            Self::LimitOnCountdown => formatter.write_str("a countdown always targets zero"),
            Self::LimitNotAbovePreset => {
                formatter.write_str("count-up limit must be above the preset")
            }
            Self::ValueOutOfRange => formatter.write_str("value is outside this timer's range"),
            Self::TimeRegression { last, attempted } => write!(
                formatter,
                "timer operation at {}ns precedes the previous operation at {}ns",
                attempted.as_nanos(),
                last.as_nanos()
            ),
            Self::Expired => formatter.write_str("timer stopped at its target"),
        }
    }
}

impl std::error::Error for TimerError {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TimerPhase {
    /// Showing the preset; never started since the last reset.
    Idle,
    Running,
    Paused,
    /// Stopped at the target under [`OverrunPolicy::StopAtTarget`].
    Expired,
}

/// Maximum number of undrained events a timer keeps; older ones are dropped.
pub const MAX_PENDING_TIMER_EVENTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerEvent {
    /// The target was reached at the exact monotonic time `at` (for a value
    /// set onto or past the target, the time of that operation). Raised once
    /// per arming; a reset, a new preset, or a value set before the target
    /// re-arms it.
    Expired { at: ClockTime },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerSnapshot {
    pub config: TimerConfig,
    pub phase: TimerPhase,
    pub value: ClockValue,
    /// Whether the target has been reached since the timer was last armed.
    /// Stays true while a [`OverrunPolicy::Continue`] timer overruns.
    pub expired: bool,
    /// When the target was reached; see [`TimerEvent::Expired`].
    pub expired_at: Option<ClockTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Idle,
    Running { anchor: ClockTime, base: ClockValue },
    Paused { value: ClockValue },
    Stopped { value: ClockValue },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Timer {
    config: TimerConfig,
    mode: Mode,
    expired_at: Option<ClockTime>,
    pending: Vec<TimerEvent>,
    last_operation: Option<ClockTime>,
}

impl Timer {
    /// Creates an idle timer.
    ///
    /// # Errors
    ///
    /// Returns a [`TimerError`] for a negative preset, a countdown limit, or
    /// a count-up limit not above the preset.
    pub fn new(config: TimerConfig) -> Result<Self, TimerError> {
        Ok(Self {
            config: config.validate()?,
            mode: Mode::Idle,
            expired_at: None,
            pending: Vec::new(),
            last_operation: None,
        })
    }

    #[must_use]
    pub const fn config(&self) -> TimerConfig {
        self.config
    }

    /// Reads the timer at `now` without changing it. A `now` earlier than the
    /// running anchor reads as the anchor.
    #[must_use]
    pub fn snapshot(&self, now: ClockTime) -> TimerSnapshot {
        let mut settled = self.clone();
        settled.settle(now);
        let value = settled.value_now(now);
        let phase = match settled.mode {
            Mode::Idle => TimerPhase::Idle,
            Mode::Running { .. } => TimerPhase::Running,
            Mode::Paused { .. } => TimerPhase::Paused,
            Mode::Stopped { .. } => TimerPhase::Expired,
        };
        TimerSnapshot {
            config: self.config,
            phase,
            value,
            expired: settled.expired_at.is_some(),
            expired_at: settled.expired_at,
        }
    }

    /// Starts an idle timer or resumes a paused one. Starting a running
    /// timer is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`] or, for a timer stopped at its
    /// target, [`TimerError::Expired`].
    pub fn start(&mut self, now: ClockTime) -> Result<(), TimerError> {
        self.begin(now)?;
        let base = match self.mode {
            Mode::Running { .. } => return Ok(()),
            Mode::Stopped { .. } => return Err(TimerError::Expired),
            Mode::Idle => self.config.preset,
            Mode::Paused { value } => value,
        };
        self.mode = Mode::Running { anchor: now, base };
        self.arm_if_reached(base, now);
        self.settle(now);
        Ok(())
    }

    /// Pauses a running timer. Pausing any other timer is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`].
    pub fn pause(&mut self, now: ClockTime) -> Result<(), TimerError> {
        self.begin(now)?;
        if let Mode::Running { .. } = self.mode {
            self.mode = Mode::Paused {
                value: self.value_now(now),
            };
        }
        Ok(())
    }

    /// Returns to the idle preset and re-arms expiry.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`].
    pub fn reset(&mut self, now: ClockTime) -> Result<(), TimerError> {
        self.begin(now)?;
        self.mode = Mode::Idle;
        self.expired_at = None;
        Ok(())
    }

    /// Changes the preset and resets to it.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`] or a configuration error; the
    /// timer is unchanged on error.
    pub fn set_preset(&mut self, now: ClockTime, preset: ClockValue) -> Result<(), TimerError> {
        let config = TimerConfig {
            preset,
            ..self.config
        }
        .validate()?;
        self.check_time(now)?;
        self.config = config;
        self.reset(now)
    }

    /// Sets the current value, keeping the timer running or paused. An idle
    /// or expired timer becomes paused at the value. A value before the
    /// target re-arms expiry; a value on or past the target expires the
    /// timer at `now` (and stops it under [`OverrunPolicy::StopAtTarget`]).
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`] or
    /// [`TimerError::ValueOutOfRange`]; the timer is unchanged on error.
    pub fn set_value(&mut self, now: ClockTime, value: ClockValue) -> Result<(), TimerError> {
        let (low, high) = self.config.bounds();
        if value < low || value > high {
            return Err(TimerError::ValueOutOfRange);
        }
        self.begin(now)?;
        let reached = self.config.has_reached_target(value);
        self.mode = if reached && self.config.overrun == OverrunPolicy::StopAtTarget {
            Mode::Stopped { value }
        } else if matches!(self.mode, Mode::Running { .. }) {
            Mode::Running {
                anchor: now,
                base: value,
            }
        } else {
            Mode::Paused { value }
        };
        if reached {
            self.mark_expired(now);
        } else {
            self.expired_at = None;
        }
        Ok(())
    }

    /// Adds a signed amount to the current value, clamped to the timer's
    /// range, with the semantics of [`Timer::set_value`].
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`].
    pub fn adjust(&mut self, now: ClockTime, delta: ClockValue) -> Result<(), TimerError> {
        self.check_time(now)?;
        let mut settled = self.clone();
        settled.settle(now);
        let (low, high) = self.config.bounds();
        let value = settled
            .value_now(now)
            .saturating_add(delta)
            .clamp(low, high);
        self.set_value(now, value)
    }

    /// Settles the timer at `now` and drains events raised since the last
    /// poll, oldest first. Events raised by any operation are kept until
    /// polled, so an expiry is not lost when a later operation re-arms the
    /// timer before the poll.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::TimeRegression`].
    pub fn poll(&mut self, now: ClockTime) -> Result<Vec<TimerEvent>, TimerError> {
        self.begin(now)?;
        Ok(core::mem::take(&mut self.pending))
    }

    fn check_time(&self, now: ClockTime) -> Result<(), TimerError> {
        match self.last_operation {
            Some(last) if now < last => Err(TimerError::TimeRegression {
                last,
                attempted: now,
            }),
            _ => Ok(()),
        }
    }

    fn begin(&mut self, now: ClockTime) -> Result<(), TimerError> {
        self.check_time(now)?;
        self.last_operation = Some(now);
        self.settle(now);
        Ok(())
    }

    fn arm_if_reached(&mut self, value: ClockValue, now: ClockTime) {
        if self.config.has_reached_target(value) {
            self.mark_expired(now);
        }
    }

    /// Records the first expiry since the timer was last armed.
    fn mark_expired(&mut self, at: ClockTime) {
        if self.expired_at.is_some() {
            return;
        }
        self.expired_at = Some(at);
        if self.pending.len() == MAX_PENDING_TIMER_EVENTS {
            self.pending.remove(0);
        }
        self.pending.push(TimerEvent::Expired { at });
    }

    /// Value at `now` given the current mode, without settling.
    fn value_now(&self, now: ClockTime) -> ClockValue {
        match self.mode {
            Mode::Idle => self.config.preset,
            Mode::Paused { value } | Mode::Stopped { value } => value,
            Mode::Running { anchor, base } => {
                let elapsed = ClockValue::saturating_from_duration(
                    now.duration_since(anchor).unwrap_or(ClockDuration::ZERO),
                );
                let raw = match self.config.direction {
                    TimerDirection::CountUp => base.saturating_add(elapsed),
                    TimerDirection::CountDown => base.saturating_sub(elapsed),
                };
                let (low, high) = self.config.bounds();
                raw.clamp(low, high)
            }
        }
    }

    /// Records a target crossing between the running anchor and `now`.
    fn settle(&mut self, now: ClockTime) {
        let Mode::Running { anchor, base } = self.mode else {
            return;
        };
        let Some(target) = self.config.target() else {
            return;
        };
        let distance = match self.config.direction {
            TimerDirection::CountDown => base.saturating_sub(target),
            TimerDirection::CountUp => target.saturating_sub(base),
        };
        let distance = u64::try_from(distance.as_nanos()).unwrap_or(0);
        let elapsed = now
            .duration_since(anchor)
            .map_or(0, ClockDuration::as_nanos);
        if elapsed < distance {
            return;
        }
        let at = ClockTime::from_nanos(anchor.as_nanos().saturating_add(distance));
        self.mark_expired(at);
        if self.config.overrun == OverrunPolicy::StopAtTarget {
            self.mode = Mode::Stopped { value: target };
        }
    }
}
