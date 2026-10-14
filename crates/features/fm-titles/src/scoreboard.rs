//! Typed scoreboard state, a frame-exact game clock, and atomic mapping onto
//! title template fields.
//!
//! A [`Scoreboard`] holds two teams (name, abbreviation, score, featured
//! player), a period, and a [`GameClock`]. Operators change it with
//! [`ScoreboardAction`]s applied as one atomic, revision-checked batch at an
//! engine frame index: either every action applies or none does. Score and
//! period *adjustments* (the increment/decrement shortcuts) saturate at their
//! bounds and report it; explicit *sets* outside the bounds are refused.
//!
//! # Clock
//!
//! The clock never reads a wall clock. Running time is accumulated in whole
//! engine frames and converted with
//! `elapsed_ms = floor(frames * 1000 * rate_den / rate_num)`, so start/stop
//! cycles never accumulate rounding drift. A count-down clock expires when its
//! value reaches zero; a count-up clock with a limit expires when it reaches the
//! limit. Expiry happens at the exact first frame that crosses the target,
//! stops the clock there, and is reported as one [`ScoreboardEvent`] exactly
//! once (re-armed only by setting the clock). Count-down displays round whole
//! seconds up, so `00:00` appears only at expiry; count-up displays round down.
//!
//! # Mapping onto a title
//!
//! A [`ScoreboardBinding`] names the template field for each value.
//! [`Scoreboard::publish`] writes every bound value that differs from the
//! scene's current value through one [`TitleScene::update_fields`] call, so a
//! title never shows a half-applied scoreboard and the scene revision advances
//! at most once per publish.
//!
//! # External data
//!
//! [`actions_from_external`] maps flat `key = value` pairs (as delivered by a
//! data feed or control surface) onto typed actions with strict parsing; see
//! that function for the key set.

use crate::{
    ClockDirection, ClockFormat, FieldId, FieldValue, TitleScene, UpdateError,
    runtime::format_clock_ms,
};
use core::fmt;
use fm_types::FrameRate;

/// Maximum characters in a team name, abbreviation, or player name.
pub const MAX_SCOREBOARD_TEXT_CHARS: usize = 64;
/// Maximum actions in one batch, and entries in one external update.
pub const MAX_SCOREBOARD_ACTIONS: usize = 64;
/// Largest clock value accepted, in milliseconds (100 hours).
pub const MAX_GAME_CLOCK_MS: u64 = 100 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TeamSide {
    Home,
    Away,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Player {
    pub number: Option<u16>,
    pub name: String,
}

impl Player {
    /// `"23 Jane Doe"`, or just the name without a number.
    #[must_use]
    pub fn display(&self) -> String {
        match self.number {
            Some(number) => format!("{number} {}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamState {
    pub name: String,
    pub abbreviation: String,
    pub score: u32,
    pub player: Option<Player>,
}

impl TeamState {
    #[must_use]
    pub fn new(name: impl Into<String>, abbreviation: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            abbreviation: abbreviation.into(),
            score: 0,
            player: None,
        }
    }
}

/// Inclusive bounds: scores are `0..=max_score`, periods `1..=max_period`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScoreboardRules {
    pub max_score: u32,
    pub max_period: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameClockConfig {
    pub direction: ClockDirection,
    pub format: ClockFormat,
    pub frame_rate: FrameRate,
    /// Value the clock is set to initially, in milliseconds.
    pub initial_ms: u64,
    /// Count-up only: the value at which the clock expires and stops.
    pub limit_ms: Option<u64>,
}

/// A frame-driven game clock. See the module documentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameClock {
    config: GameClockConfig,
    base_ms: u64,
    accumulated_frames: u64,
    running_since: Option<u64>,
    expired_at: Option<u64>,
    expiry_pending: bool,
}

impl GameClock {
    fn new(config: GameClockConfig) -> Result<Self, ScoreboardError> {
        check_clock_ms(config.initial_ms)?;
        if let Some(limit) = config.limit_ms {
            check_clock_ms(limit)?;
        }
        Ok(Self {
            config,
            base_ms: config.initial_ms,
            accumulated_frames: 0,
            running_since: None,
            expired_at: None,
            expiry_pending: false,
        })
    }

    #[must_use]
    pub const fn config(&self) -> GameClockConfig {
        self.config
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    /// Frame at which the clock expired, until it is set again.
    #[must_use]
    pub const fn expired_at(&self) -> Option<u64> {
        self.expired_at
    }

    fn elapsed_frames(&self, frame: u64) -> u64 {
        let running = self
            .running_since
            .map_or(0, |since| frame.saturating_sub(since));
        self.accumulated_frames.saturating_add(running)
    }

    fn frames_to_ms(&self, frames: u64) -> u64 {
        let rate = self.config.frame_rate;
        let ms = u128::from(frames) * 1_000 * u128::from(rate.denominator())
            / u128::from(rate.numerator());
        u64::try_from(ms).unwrap_or(u64::MAX)
    }

    /// Elapsed running time at which the clock expires, if it can.
    fn target_ms(&self) -> Option<u64> {
        match self.config.direction {
            ClockDirection::CountDown => Some(self.base_ms),
            ClockDirection::CountUp => self
                .config
                .limit_ms
                .map(|limit| limit.saturating_sub(self.base_ms)),
        }
    }

    /// Exact running frames needed to reach the target.
    fn target_frames(&self) -> Option<u64> {
        let target = self.target_ms()?;
        let rate = self.config.frame_rate;
        let frames = (u128::from(target) * u128::from(rate.numerator()))
            .div_ceil(1_000 * u128::from(rate.denominator()));
        Some(u64::try_from(frames).unwrap_or(u64::MAX))
    }

    /// The first frame at which a running clock reaches its target.
    #[must_use]
    pub fn expiry_frame(&self) -> Option<u64> {
        let since = self.running_since?;
        let remaining = self
            .target_frames()?
            .saturating_sub(self.accumulated_frames);
        Some(since.saturating_add(remaining))
    }

    /// Clock value at `frame`, in milliseconds, clamped at zero or the limit.
    #[must_use]
    pub fn value_ms(&self, frame: u64) -> u64 {
        let elapsed = self.frames_to_ms(self.elapsed_frames(frame));
        match self.config.direction {
            ClockDirection::CountDown => self.base_ms.saturating_sub(elapsed),
            ClockDirection::CountUp => {
                let value = self.base_ms.saturating_add(elapsed);
                self.config.limit_ms.map_or(value, |limit| value.min(limit))
            }
        }
    }

    /// Display text at `frame` in the configured format.
    #[must_use]
    pub fn display(&self, frame: u64) -> String {
        format_clock_ms(
            self.value_ms(frame),
            self.config.format,
            self.config.direction,
        )
    }

    /// Stops a running clock at its exact expiry frame if `frame` reached it.
    fn settle(&mut self, frame: u64) {
        let Some(expiry) = self.expiry_frame() else {
            return;
        };
        if expiry > frame {
            return;
        }
        if let Some(since) = self.running_since.take() {
            self.accumulated_frames = self
                .accumulated_frames
                .saturating_add(expiry.saturating_sub(since));
        }
        self.expired_at = Some(expiry);
        self.expiry_pending = true;
    }

    fn start(&mut self, frame: u64) -> Result<(), ScoreboardError> {
        if self.running_since.is_some() {
            return Ok(());
        }
        if self.expired_at.is_some() {
            return Err(ScoreboardError::ClockExpired);
        }
        self.running_since = Some(frame);
        // A zero count-down (or a count-up already at its limit) expires on
        // the start frame itself.
        self.settle(frame);
        Ok(())
    }

    fn stop(&mut self, frame: u64) {
        if let Some(since) = self.running_since.take() {
            self.accumulated_frames = self
                .accumulated_frames
                .saturating_add(frame.saturating_sub(since));
        }
    }

    fn set(&mut self, frame: u64, value_ms: u64) -> Result<(), ScoreboardError> {
        check_clock_ms(value_ms)?;
        self.base_ms = value_ms;
        self.accumulated_frames = 0;
        self.expired_at = None;
        self.expiry_pending = false;
        if self.running_since.is_some() {
            self.running_since = Some(frame);
            self.settle(frame);
        }
        Ok(())
    }
}

fn check_clock_ms(value_ms: u64) -> Result<(), ScoreboardError> {
    if value_ms > MAX_GAME_CLOCK_MS {
        return Err(ScoreboardError::ClockOutOfRange {
            value_ms,
            maximum: MAX_GAME_CLOCK_MS,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScoreboardAction {
    /// Increment/decrement shortcut; saturates at `0..=max_score`.
    AdjustScore {
        side: TeamSide,
        delta: i64,
    },
    /// Refused outside `0..=max_score`.
    SetScore {
        side: TeamSide,
        score: u32,
    },
    /// Both scores to zero.
    ResetScores,
    SetTeamName {
        side: TeamSide,
        name: String,
    },
    SetAbbreviation {
        side: TeamSide,
        abbreviation: String,
    },
    SetPlayer {
        side: TeamSide,
        player: Option<Player>,
    },
    /// Saturates at `1..=max_period`.
    AdjustPeriod {
        delta: i32,
    },
    /// Refused outside `1..=max_period`.
    SetPeriod {
        period: u16,
    },
    StartClock,
    StopClock,
    /// Sets the value and re-arms expiry; a running clock keeps running.
    SetClock {
        value_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreboardEvent {
    /// The clock reached zero (count-down) or its limit (count-up) at `frame`.
    ClockExpired { frame: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreboardOutcome {
    pub revision: u64,
    /// Indices of adjustments that hit a bound.
    pub saturated: Vec<usize>,
    pub events: Vec<ScoreboardEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScoreboardError {
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    RevisionExhausted,
    FrameRegressed {
        frame: u64,
        last_frame: u64,
    },
    TooManyActions {
        count: usize,
        maximum: usize,
    },
    InvalidRules,
    ScoreOutOfRange {
        side: TeamSide,
        score: u32,
        maximum: u32,
    },
    PeriodOutOfRange {
        period: u16,
        maximum: u16,
    },
    TextTooLong {
        maximum: usize,
    },
    ControlCharacter,
    ClockOutOfRange {
        value_ms: u64,
        maximum: u64,
    },
    /// Starting an expired clock; set it first.
    ClockExpired,
    UnknownExternalKey {
        index: usize,
    },
    InvalidExternalValue {
        index: usize,
    },
}

impl fmt::Display for ScoreboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "expected scoreboard revision {expected}, current revision is {actual}"
            ),
            Self::RevisionExhausted => formatter.write_str("scoreboard revision is exhausted"),
            Self::FrameRegressed { frame, last_frame } => write!(
                formatter,
                "scoreboard frame {frame} precedes last frame {last_frame}"
            ),
            Self::TooManyActions { count, maximum } => {
                write!(formatter, "{count} scoreboard actions exceed {maximum}")
            }
            Self::InvalidRules => formatter.write_str("scoreboard rules are invalid"),
            Self::ScoreOutOfRange {
                side,
                score,
                maximum,
            } => write!(formatter, "{side:?} score {score} is outside 0..={maximum}"),
            Self::PeriodOutOfRange { period, maximum } => {
                write!(formatter, "period {period} is outside 1..={maximum}")
            }
            Self::TextTooLong { maximum } => {
                write!(formatter, "scoreboard text exceeds {maximum} characters")
            }
            Self::ControlCharacter => {
                formatter.write_str("scoreboard text contains a control character")
            }
            Self::ClockOutOfRange { value_ms, maximum } => {
                write!(formatter, "clock value {value_ms}ms exceeds {maximum}ms")
            }
            Self::ClockExpired => formatter.write_str("clock has expired; set it before starting"),
            Self::UnknownExternalKey { index } => {
                write!(formatter, "external entry {index} has an unknown key")
            }
            Self::InvalidExternalValue { index } => {
                write!(formatter, "external entry {index} has an invalid value")
            }
        }
    }
}

impl std::error::Error for ScoreboardError {}

/// Template fields that scoreboard values are written to. Unbound values are
/// skipped. Score and period fields may be `Number` or `Text` fields; every
/// other value is written as text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScoreboardBinding {
    pub home_name: Option<FieldId>,
    pub home_abbreviation: Option<FieldId>,
    pub home_score: Option<FieldId>,
    pub home_player: Option<FieldId>,
    pub away_name: Option<FieldId>,
    pub away_abbreviation: Option<FieldId>,
    pub away_score: Option<FieldId>,
    pub away_player: Option<FieldId>,
    pub period: Option<FieldId>,
    pub clock: Option<FieldId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scoreboard {
    home: TeamState,
    away: TeamState,
    period: u16,
    clock: GameClock,
    rules: ScoreboardRules,
    revision: u64,
    last_frame: u64,
}

impl Scoreboard {
    /// Creates a scoreboard at period 1 with zero scores and a stopped clock.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreboardError::InvalidRules`] for a zero `max_period`, or
    /// a text or clock error for the initial teams or clock.
    pub fn new(
        home: TeamState,
        away: TeamState,
        rules: ScoreboardRules,
        clock: GameClockConfig,
    ) -> Result<Self, ScoreboardError> {
        if rules.max_period == 0 {
            return Err(ScoreboardError::InvalidRules);
        }
        for (side, team) in [(TeamSide::Home, &home), (TeamSide::Away, &away)] {
            check_text(&team.name)?;
            check_text(&team.abbreviation)?;
            if let Some(player) = &team.player {
                check_text(&player.name)?;
            }
            if team.score > rules.max_score {
                return Err(ScoreboardError::ScoreOutOfRange {
                    side,
                    score: team.score,
                    maximum: rules.max_score,
                });
            }
        }
        Ok(Self {
            home,
            away,
            period: 1,
            clock: GameClock::new(clock)?,
            rules,
            revision: 0,
            last_frame: 0,
        })
    }

    #[must_use]
    pub const fn team(&self, side: TeamSide) -> &TeamState {
        match side {
            TeamSide::Home => &self.home,
            TeamSide::Away => &self.away,
        }
    }

    #[must_use]
    pub const fn period(&self) -> u16 {
        self.period
    }

    #[must_use]
    pub const fn clock(&self) -> &GameClock {
        &self.clock
    }

    #[must_use]
    pub const fn rules(&self) -> ScoreboardRules {
        self.rules
    }

    /// Revision advanced by every accepted, non-empty action batch.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn last_frame(&self) -> u64 {
        self.last_frame
    }

    const fn team_mut(&mut self, side: TeamSide) -> &mut TeamState {
        match side {
            TeamSide::Home => &mut self.home,
            TeamSide::Away => &mut self.away,
        }
    }

    fn check_frame(&self, frame: u64) -> Result<(), ScoreboardError> {
        if frame < self.last_frame {
            return Err(ScoreboardError::FrameRegressed {
                frame,
                last_frame: self.last_frame,
            });
        }
        Ok(())
    }

    /// Advances to `frame` without operator actions, settling clock expiry.
    /// Does not change the revision.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreboardError::FrameRegressed`] for an earlier frame.
    pub fn advance(&mut self, frame: u64) -> Result<Vec<ScoreboardEvent>, ScoreboardError> {
        self.check_frame(frame)?;
        self.clock.settle(frame);
        self.last_frame = frame;
        Ok(self.take_events())
    }

    fn take_events(&mut self) -> Vec<ScoreboardEvent> {
        let mut events = Vec::new();
        if self.clock.expiry_pending {
            self.clock.expiry_pending = false;
            if let Some(frame) = self.clock.expired_at {
                events.push(ScoreboardEvent::ClockExpired { frame });
            }
        }
        events
    }

    /// Applies `actions` atomically at `frame`.
    ///
    /// Clock expiry up to `frame` is settled first, so an expiry that happened
    /// before the batch is reported (once) in the outcome. An empty batch only
    /// settles and does not advance the revision.
    ///
    /// # Errors
    ///
    /// Returns a revision conflict, frame regression, bound violation, or
    /// invalid text without changing anything (including pending events).
    pub fn apply(
        &mut self,
        expected_revision: u64,
        frame: u64,
        actions: &[ScoreboardAction],
    ) -> Result<ScoreboardOutcome, ScoreboardError> {
        if expected_revision != self.revision {
            return Err(ScoreboardError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        self.check_frame(frame)?;
        if actions.len() > MAX_SCOREBOARD_ACTIONS {
            return Err(ScoreboardError::TooManyActions {
                count: actions.len(),
                maximum: MAX_SCOREBOARD_ACTIONS,
            });
        }
        let mut next = self.clone();
        next.clock.settle(frame);
        next.last_frame = frame;
        let mut saturated = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            if next.apply_one(frame, action)? {
                saturated.push(index);
            }
        }
        if !actions.is_empty() {
            next.revision = self
                .revision
                .checked_add(1)
                .ok_or(ScoreboardError::RevisionExhausted)?;
        }
        *self = next;
        Ok(ScoreboardOutcome {
            revision: self.revision,
            saturated,
            events: self.take_events(),
        })
    }

    /// Parses external `key = value` entries and applies them atomically.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`actions_from_external`] and [`Self::apply`].
    pub fn apply_external(
        &mut self,
        expected_revision: u64,
        frame: u64,
        entries: &[(&str, &str)],
    ) -> Result<ScoreboardOutcome, ScoreboardError> {
        let actions = actions_from_external(entries)?;
        self.apply(expected_revision, frame, &actions)
    }

    /// Returns whether the action saturated.
    fn apply_one(
        &mut self,
        frame: u64,
        action: &ScoreboardAction,
    ) -> Result<bool, ScoreboardError> {
        let max_score = self.rules.max_score;
        let max_period = self.rules.max_period;
        match action {
            ScoreboardAction::AdjustScore { side, delta } => {
                let team = self.team_mut(*side);
                let wanted = i128::from(team.score) + i128::from(*delta);
                let clamped = wanted.clamp(0, i128::from(max_score));
                team.score = u32::try_from(clamped).unwrap_or(max_score);
                return Ok(clamped != wanted);
            }
            ScoreboardAction::SetScore { side, score } => {
                if *score > max_score {
                    return Err(ScoreboardError::ScoreOutOfRange {
                        side: *side,
                        score: *score,
                        maximum: max_score,
                    });
                }
                self.team_mut(*side).score = *score;
            }
            ScoreboardAction::ResetScores => {
                self.home.score = 0;
                self.away.score = 0;
            }
            ScoreboardAction::SetTeamName { side, name } => {
                check_text(name)?;
                self.team_mut(*side).name.clone_from(name);
            }
            ScoreboardAction::SetAbbreviation { side, abbreviation } => {
                check_text(abbreviation)?;
                self.team_mut(*side).abbreviation.clone_from(abbreviation);
            }
            ScoreboardAction::SetPlayer { side, player } => {
                if let Some(player) = player {
                    check_text(&player.name)?;
                }
                self.team_mut(*side).player.clone_from(player);
            }
            ScoreboardAction::AdjustPeriod { delta } => {
                let wanted = i64::from(self.period) + i64::from(*delta);
                let clamped = wanted.clamp(1, i64::from(max_period));
                self.period = u16::try_from(clamped).unwrap_or(max_period);
                return Ok(clamped != wanted);
            }
            ScoreboardAction::SetPeriod { period } => {
                if *period == 0 || *period > max_period {
                    return Err(ScoreboardError::PeriodOutOfRange {
                        period: *period,
                        maximum: max_period,
                    });
                }
                self.period = *period;
            }
            ScoreboardAction::StartClock => self.clock.start(frame)?,
            ScoreboardAction::StopClock => self.clock.stop(frame),
            ScoreboardAction::SetClock { value_ms } => self.clock.set(frame, *value_ms)?,
        }
        Ok(false)
    }

    /// Bound field values at `frame` that differ from `scene`'s current ones.
    #[must_use]
    pub fn field_updates(
        &self,
        binding: &ScoreboardBinding,
        scene: &TitleScene,
        frame: u64,
    ) -> Vec<(FieldId, FieldValue)> {
        let player = |team: &TeamState| {
            team.player
                .as_ref()
                .map_or_else(String::new, Player::display)
        };
        let texts = [
            (binding.home_name, self.home.name.clone()),
            (binding.home_abbreviation, self.home.abbreviation.clone()),
            (binding.home_player, player(&self.home)),
            (binding.away_name, self.away.name.clone()),
            (binding.away_abbreviation, self.away.abbreviation.clone()),
            (binding.away_player, player(&self.away)),
            (binding.clock, self.clock.display(frame)),
        ];
        let numbers = [
            (binding.home_score, i64::from(self.home.score)),
            (binding.away_score, i64::from(self.away.score)),
            (binding.period, i64::from(self.period)),
        ];
        let mut updates = Vec::new();
        for (field, text) in texts {
            if let Some(field) = field {
                updates.push((field, FieldValue::Text(text)));
            }
        }
        for (field, number) in numbers {
            if let Some(field) = field {
                let value = match scene.field_value(field) {
                    Some(FieldValue::Number(_)) => FieldValue::Number(number),
                    _ => FieldValue::Text(number.to_string()),
                };
                updates.push((field, value));
            }
        }
        updates.retain(|(field, value)| scene.field_value(*field) != Some(value));
        updates
    }

    /// Writes every changed bound value into `scene` in one atomic update.
    ///
    /// # Errors
    ///
    /// Returns the scene's [`UpdateError`] (revision conflict, unknown or
    /// duplicate field, type mismatch) without changing any field.
    pub fn publish(
        &self,
        binding: &ScoreboardBinding,
        scene: &mut TitleScene,
        expected_scene_revision: u64,
        frame: u64,
    ) -> Result<u64, UpdateError> {
        let updates = self.field_updates(binding, scene, frame);
        scene.update_fields(expected_scene_revision, &updates)
    }
}

fn check_text(text: &str) -> Result<(), ScoreboardError> {
    if text.chars().nth(MAX_SCOREBOARD_TEXT_CHARS).is_some() {
        return Err(ScoreboardError::TextTooLong {
            maximum: MAX_SCOREBOARD_TEXT_CHARS,
        });
    }
    if text.chars().any(char::is_control) {
        return Err(ScoreboardError::ControlCharacter);
    }
    Ok(())
}

/// Maps external `key = value` entries onto typed actions, in order.
///
/// Keys (`<side>` is `home` or `away`):
///
/// | key | value | action |
/// | --- | --- | --- |
/// | `<side>.name` | text | [`ScoreboardAction::SetTeamName`] |
/// | `<side>.abbreviation` | text | [`ScoreboardAction::SetAbbreviation`] |
/// | `<side>.score` | decimal `u32` | [`ScoreboardAction::SetScore`] |
/// | `<side>.score.delta` | signed decimal | [`ScoreboardAction::AdjustScore`] |
/// | `<side>.player` | `""`, `name`, or `number name` | [`ScoreboardAction::SetPlayer`] |
/// | `period` | decimal `u16` | [`ScoreboardAction::SetPeriod`] |
/// | `clock.ms` | decimal `u64` | [`ScoreboardAction::SetClock`] |
/// | `clock.running` | `true` / `false` | start / stop |
///
/// Numbers are plain ASCII decimal (a leading `-` only for deltas); no
/// whitespace, signs, or separators are tolerated.
///
/// # Errors
///
/// Returns [`ScoreboardError::TooManyActions`], or the index of the first
/// unknown key or unparsable value.
pub fn actions_from_external(
    entries: &[(&str, &str)],
) -> Result<Vec<ScoreboardAction>, ScoreboardError> {
    if entries.len() > MAX_SCOREBOARD_ACTIONS {
        return Err(ScoreboardError::TooManyActions {
            count: entries.len(),
            maximum: MAX_SCOREBOARD_ACTIONS,
        });
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, (key, value))| external_action(index, key, value))
        .collect()
}

fn external_action(
    index: usize,
    key: &str,
    value: &str,
) -> Result<ScoreboardAction, ScoreboardError> {
    let invalid = ScoreboardError::InvalidExternalValue { index };
    let text = || -> Result<String, ScoreboardError> {
        check_text(value).map_err(|_| ScoreboardError::InvalidExternalValue { index })?;
        Ok(value.to_owned())
    };
    let (side, field) = match key.split_once('.') {
        Some(("home", field)) => (Some(TeamSide::Home), field),
        Some(("away", field)) => (Some(TeamSide::Away), field),
        _ => (None, key),
    };
    let action = match (side, field) {
        (Some(side), "name") => ScoreboardAction::SetTeamName {
            side,
            name: text()?,
        },
        (Some(side), "abbreviation") => ScoreboardAction::SetAbbreviation {
            side,
            abbreviation: text()?,
        },
        (Some(side), "score") => ScoreboardAction::SetScore {
            side,
            score: parse_unsigned(value).ok_or(invalid)?,
        },
        (Some(side), "score.delta") => ScoreboardAction::AdjustScore {
            side,
            delta: parse_signed(value).ok_or(invalid)?,
        },
        (Some(side), "player") => ScoreboardAction::SetPlayer {
            side,
            player: parse_player(&text()?),
        },
        (None, "period") => ScoreboardAction::SetPeriod {
            period: parse_unsigned(value).ok_or(invalid)?,
        },
        (None, "clock.ms") => ScoreboardAction::SetClock {
            value_ms: parse_unsigned(value).ok_or(invalid)?,
        },
        (None, "clock.running") => match value {
            "true" => ScoreboardAction::StartClock,
            "false" => ScoreboardAction::StopClock,
            _ => return Err(invalid),
        },
        _ => return Err(ScoreboardError::UnknownExternalKey { index }),
    };
    Ok(action)
}

fn parse_unsigned<T: core::str::FromStr>(value: &str) -> Option<T> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn parse_signed(value: &str) -> Option<i64> {
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let magnitude: i64 = parse_unsigned(digits)?;
    Some(if negative { -magnitude } else { magnitude })
}

fn parse_player(value: &str) -> Option<Player> {
    if value.trim().is_empty() {
        return None;
    }
    if let Some((number, name)) = value.split_once(' ')
        && let Some(number) = parse_unsigned::<u16>(number)
        && !name.trim().is_empty()
    {
        return Some(Player {
            number: Some(number),
            name: name.to_owned(),
        });
    }
    Some(Player {
        number: None,
        name: value.to_owned(),
    })
}
