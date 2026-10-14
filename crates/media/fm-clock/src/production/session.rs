//! Timers that follow an output's lifetime: streaming, recording, replay.
//!
//! The owner of the output reports its transitions (started, paused,
//! resumed, stopped) with the monotonic time they happened. The timer shows
//! the current session's active duration (pauses excluded) and a running
//! total across sessions. After a stop the last session's duration stays
//! readable until the next start.

use core::fmt;

use super::ClockValue;
use crate::{ClockDuration, ClockTime};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    TimeRegression {
        last: ClockTime,
        attempted: ClockTime,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimeRegression { last, attempted } => write!(
                formatter,
                "session transition at {}ns precedes the previous transition at {}ns",
                attempted.as_nanos(),
                last.as_nanos()
            ),
        }
    }
}

impl std::error::Error for SessionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Inactive,
    Active { since: ClockTime },
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    pub active: bool,
    pub paused: bool,
    /// Active duration of the current (or most recent) session.
    pub session: ClockValue,
    /// Active duration summed over every session.
    pub total: ClockValue,
    /// Number of sessions started.
    pub sessions: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTimer {
    state: State,
    /// Session time accumulated before the current active stretch.
    session_banked: ClockValue,
    /// Total time accumulated before the current active stretch.
    total_banked: ClockValue,
    sessions: u64,
    last_transition: Option<ClockTime>,
}

impl Default for SessionTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTimer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Inactive,
            session_banked: ClockValue::ZERO,
            total_banked: ClockValue::ZERO,
            sessions: 0,
            last_transition: None,
        }
    }

    /// Reads the timer at `now`. A `now` before the active stretch began
    /// reads as its start.
    #[must_use]
    pub fn snapshot(&self, now: ClockTime) -> SessionSnapshot {
        let running = match self.state {
            State::Active { since } => ClockValue::saturating_from_duration(
                now.duration_since(since).unwrap_or(ClockDuration::ZERO),
            ),
            State::Inactive | State::Paused => ClockValue::ZERO,
        };
        SessionSnapshot {
            active: !matches!(self.state, State::Inactive),
            paused: matches!(self.state, State::Paused),
            session: self.session_banked.saturating_add(running),
            total: self.total_banked.saturating_add(running),
            sessions: self.sessions,
        }
    }

    /// The output started. Starting an active or paused session is a no-op,
    /// so a repeated "started" notification cannot reset the display.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TimeRegression`].
    pub fn start(&mut self, now: ClockTime) -> Result<(), SessionError> {
        self.begin(now)?;
        if matches!(self.state, State::Inactive) {
            self.session_banked = ClockValue::ZERO;
            self.sessions = self.sessions.saturating_add(1);
            self.state = State::Active { since: now };
        }
        Ok(())
    }

    /// The output paused (for example a paused recording). No-op unless
    /// active.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TimeRegression`].
    pub fn pause(&mut self, now: ClockTime) -> Result<(), SessionError> {
        self.begin(now)?;
        if let State::Active { .. } = self.state {
            self.bank(now);
            self.state = State::Paused;
        }
        Ok(())
    }

    /// The output resumed after a pause. No-op unless paused.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TimeRegression`].
    pub fn resume(&mut self, now: ClockTime) -> Result<(), SessionError> {
        self.begin(now)?;
        if let State::Paused = self.state {
            self.state = State::Active { since: now };
        }
        Ok(())
    }

    /// The output stopped. The session duration remains readable.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TimeRegression`].
    pub fn stop(&mut self, now: ClockTime) -> Result<(), SessionError> {
        self.begin(now)?;
        if let State::Active { .. } = self.state {
            self.bank(now);
        }
        self.state = State::Inactive;
        Ok(())
    }

    /// Clears the session, the total, and the session count. An active
    /// output keeps running from zero.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TimeRegression`].
    pub fn clear(&mut self, now: ClockTime) -> Result<(), SessionError> {
        self.begin(now)?;
        self.session_banked = ClockValue::ZERO;
        self.total_banked = ClockValue::ZERO;
        self.sessions = u64::from(!matches!(self.state, State::Inactive));
        if let State::Active { .. } = self.state {
            self.state = State::Active { since: now };
        }
        Ok(())
    }

    fn bank(&mut self, now: ClockTime) {
        let snapshot = self.snapshot(now);
        self.session_banked = snapshot.session;
        self.total_banked = snapshot.total;
    }

    fn begin(&mut self, now: ClockTime) -> Result<(), SessionError> {
        if let Some(last) = self.last_transition
            && now < last
        {
            return Err(SessionError::TimeRegression {
                last,
                attempted: now,
            });
        }
        self.last_transition = Some(now);
        Ok(())
    }
}
