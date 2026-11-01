//! Deterministic playlist automation across inputs.
//!
//! A [`Playlist`] is an ordered list of [`PlaylistEntry`] values, each naming
//! an input, an on-air duration (fixed, or the input's media length), the
//! transition that brings it on air, and an enabled flag. The engine never
//! reads a clock: every call receives a caller-owned, non-decreasing wall
//! time in milliseconds, and the result is a pure function of the call
//! sequence. It performs no switching itself; it returns ordered
//! [`PlaylistEvent`] values that the switcher executes (most importantly
//! [`PlaylistEventKind::EntryOn`], "transition this input to program").
//!
//! Time-driven progress is evaluated at logical boundaries, not poll times:
//! an entry that should have ended at `t` ends at `t` even when the caller
//! polls later, so the resulting state does not depend on poll cadence.
//! When one call crosses several boundaries, entries that would have started
//! and ended entirely inside the gap are reported as
//! [`PlaylistEventKind::Skipped`] and only the entry on air at the call time
//! is reported as `EntryOn`, with `offset_ms` saying how far into it the
//! playlist already is. Whole skipped loop cycles are collapsed into a single
//! [`PlaylistEventKind::Looped`] event, so one call emits a bounded number of
//! events regardless of the gap length.
//!
//! Every fallible call validates against a scratch copy of the cursor first,
//! so an `Err` never changes state and never drops events.

use core::fmt;
use fm_types::InputId;

/// Maximum number of entries in one playlist.
pub const MAX_PLAYLIST_ENTRIES: usize = 1024;
/// Maximum entry duration, media length, or transition duration (7 days).
pub const MAX_PLAYLIST_ENTRY_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Largest accepted wall time. Bounding time keeps all deadline arithmetic
/// exact without saturation.
pub const MAX_PLAYLIST_TIME_MS: u64 = u64::MAX / 4;

/// Stable identity of a playlist entry, unique within one playlist.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlaylistEntryId(u64);

impl PlaylistEntryId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PlaylistEntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// How long an entry stays on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryDuration {
    /// Exactly `ms` milliseconds, including the entry's incoming transition.
    Fixed { ms: u64 },
    /// The input's media length. With a known length this behaves like
    /// `Fixed`; without one the entry stays on air until
    /// [`PlaylistCommand::MediaEnded`] or a navigation command.
    Media { length_ms: Option<u64> },
}

impl EntryDuration {
    /// The duration in milliseconds when it is known up front.
    #[must_use]
    pub const fn known_ms(self) -> Option<u64> {
        match self {
            Self::Fixed { ms } => Some(ms),
            Self::Media { length_ms } => length_ms,
        }
    }
}

/// Transition effects a playlist entry may request from the switcher.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PlaylistTransitionKind {
    Cut,
    Fade,
    AlphaFade,
    Wipe,
    Slide,
    Zoom,
    Stinger,
}

/// The transition that brings an entry on air. A cut has zero duration;
/// every other kind needs a nonzero duration no longer than the entry. A
/// Stinger names its slot; no other kind has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistTransition {
    pub kind: PlaylistTransitionKind,
    pub duration_ms: u64,
    /// The Stinger slot (`1..=8`) a Stinger transition takes.
    pub stinger_slot: Option<u8>,
}

impl PlaylistTransition {
    pub const CUT: Self = Self {
        kind: PlaylistTransitionKind::Cut,
        duration_ms: 0,
        stinger_slot: None,
    };

    #[must_use]
    pub const fn new(kind: PlaylistTransitionKind, duration_ms: u64) -> Self {
        Self {
            kind,
            duration_ms,
            stinger_slot: None,
        }
    }

    /// A Stinger transition from `slot` lasting `duration_ms`.
    #[must_use]
    pub const fn stinger(slot: u8, duration_ms: u64) -> Self {
        Self {
            kind: PlaylistTransitionKind::Stinger,
            duration_ms,
            stinger_slot: Some(slot),
        }
    }
}

/// One playlist row. Fields are plain data so the entry can be persisted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistEntry {
    pub id: PlaylistEntryId,
    pub input: InputId,
    pub duration: EntryDuration,
    pub transition: PlaylistTransition,
    pub enabled: bool,
}

impl PlaylistEntry {
    /// An enabled entry that cuts to `input`.
    #[must_use]
    pub const fn new(id: PlaylistEntryId, input: InputId, duration: EntryDuration) -> Self {
        Self {
            id,
            input,
            duration,
            transition: PlaylistTransition::CUT,
            enabled: true,
        }
    }

    #[must_use]
    pub const fn with_transition(mut self, transition: PlaylistTransition) -> Self {
        self.transition = transition;
        self
    }

    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// What happens when the on-air entry ends.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LoopMode {
    /// Advance to the next enabled entry; finish after the last one.
    /// Next/Previous do not wrap.
    #[default]
    Off,
    /// Advance and wrap from the last enabled entry to the first, starting a
    /// new cycle. Next/Previous wrap too.
    All,
    /// Restart the on-air entry while it stays enabled. Next/Previous move
    /// without wrapping.
    One,
}

/// How [`PlaylistCommand::Start`] treats an absolute start already in the
/// past at the time of the call.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LatePolicy {
    /// Refuse with [`PlaylistError::ScheduleInPast`].
    #[default]
    Reject,
    /// Start from the first entry at the call time.
    StartNow,
    /// Start as if the playlist had begun at the scheduled time, joining
    /// whichever entry would be on air now at the matching offset.
    JoinInProgress,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PlaylistConfig {
    pub loop_mode: LoopMode,
    pub late_policy: LatePolicy,
}

/// When a started playlist puts its first entry on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartAt {
    Now,
    /// Relative start: `delay_ms` after the call time.
    After {
        delay_ms: u64,
    },
    /// Absolute start at a wall time.
    At {
        wall_ms: u64,
    },
}

/// Externally visible and persisted playlist state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistState {
    Stopped,
    /// Waiting for `start_at_ms`; starts from `from` (or the first entry),
    /// or the next enabled entry after it at fire time.
    Armed {
        start_at_ms: u64,
        from: Option<PlaylistEntryId>,
    },
    /// `started_at_ms` is the virtual start: the time the entry would have
    /// started had it never been paused.
    Playing {
        entry: PlaylistEntryId,
        take: u64,
        cycle: u64,
        started_at_ms: u64,
    },
    Paused {
        entry: PlaylistEntryId,
        take: u64,
        cycle: u64,
        elapsed_ms: u64,
    },
    /// Ran off the end (or nothing was enabled when the next entry was due).
    /// The last input stays on air; the playlist no longer drives it.
    Finished {
        last: Option<PlaylistEntryId>,
    },
}

/// Why an entry came on air.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AdvanceCause {
    Start,
    /// The previous entry's known duration elapsed.
    Duration,
    /// [`PlaylistCommand::MediaEnded`] ended the previous entry.
    MediaEnded,
    Next,
    Previous,
    Jump,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistEventKind {
    Armed {
        start_at_ms: u64,
    },
    /// Switcher action: transition `input` to program with `transition`.
    /// `offset_ms` is how far into the entry the playlist already is (nonzero
    /// when joining in progress or catching up a late call); media inputs
    /// should be cued at that offset. `take` identifies this airing and is
    /// what [`PlaylistCommand::MediaEnded`] must quote.
    EntryOn {
        entry: PlaylistEntryId,
        input: InputId,
        transition: PlaylistTransition,
        take: u64,
        cycle: u64,
        offset_ms: u64,
        cause: AdvanceCause,
    },
    /// An airing that started and ended entirely between two calls. It was
    /// never put on air; it is reported for the as-run log only.
    Skipped {
        entry: PlaylistEntryId,
        input: InputId,
        take: u64,
    },
    /// Switcher action: pause the on-air input's media.
    Paused {
        entry: PlaylistEntryId,
        take: u64,
        elapsed_ms: u64,
    },
    /// Switcher action: resume the on-air input's media.
    Resumed {
        entry: PlaylistEntryId,
        take: u64,
        elapsed_ms: u64,
    },
    /// A loop wrap into `cycle`. `skipped_cycles` whole cycles elapsed inside
    /// one call gap and were collapsed without per-entry events; their
    /// airings still consume take numbers.
    Looped {
        cycle: u64,
        skipped_cycles: u64,
    },
    Finished {
        last: Option<PlaylistEntryId>,
    },
    Stopped,
}

/// An event stamped with the logical time it belongs to (which may precede
/// the call time when a call catches up).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistEvent {
    pub at_ms: u64,
    pub kind: PlaylistEventKind,
}

/// The API-control surface of a playlist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistCommand {
    Start {
        at: StartAt,
        from: Option<PlaylistEntryId>,
    },
    Pause,
    Resume,
    Next,
    Previous,
    JumpTo(PlaylistEntryId),
    Stop,
    SetEnabled {
        entry: PlaylistEntryId,
        enabled: bool,
    },
    SetLoopMode(LoopMode),
    SetLatePolicy(LatePolicy),
    /// The on-air media input reached its end. Only the current `take` is
    /// accepted, so a late report about an earlier airing cannot skip the
    /// entry that replaced it.
    MediaEnded {
        take: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistError {
    TooManyEntries {
        count: usize,
        max: usize,
    },
    DuplicateEntry(PlaylistEntryId),
    ZeroDuration(PlaylistEntryId),
    DurationTooLong {
        entry: PlaylistEntryId,
        ms: u64,
        max: u64,
    },
    /// A cut with a duration, or another kind with none or one over the max.
    InvalidTransition(PlaylistEntryId),
    TransitionLongerThanEntry {
        entry: PlaylistEntryId,
        transition_ms: u64,
        duration_ms: u64,
    },
    TimeOutOfRange {
        ms: u64,
        max: u64,
    },
    ClockRegressed {
        observed_ms: u64,
        now_ms: u64,
    },
    Empty,
    NoEnabledEntries,
    UnknownEntry(PlaylistEntryId),
    EntryDisabled(PlaylistEntryId),
    AlreadyActive,
    /// The command needs an entry on air (playing or paused).
    NotOnAir,
    /// The command needs a playing (not paused) entry.
    NotPlaying,
    NoNextEntry,
    NoPreviousEntry,
    ScheduleInPast {
        start_at_ms: u64,
        now_ms: u64,
    },
    StaleTake {
        take: u64,
        current: Option<u64>,
    },
    NotMediaDuration(PlaylistEntryId),
    /// Entries can only be replaced while stopped or finished.
    EditWhileActive,
}

impl fmt::Display for PlaylistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries { count, max } => {
                write!(
                    formatter,
                    "playlist has {count} entries; the limit is {max}"
                )
            }
            Self::DuplicateEntry(id) => write!(formatter, "playlist entry {id} is duplicated"),
            Self::ZeroDuration(id) => write!(formatter, "playlist entry {id} has a zero duration"),
            Self::DurationTooLong { entry, ms, max } => write!(
                formatter,
                "playlist entry {entry} lasts {ms} ms; the limit is {max} ms"
            ),
            Self::InvalidTransition(id) => {
                write!(formatter, "playlist entry {id} has an invalid transition")
            }
            Self::TransitionLongerThanEntry {
                entry,
                transition_ms,
                duration_ms,
            } => write!(
                formatter,
                "playlist entry {entry} transition of {transition_ms} ms exceeds its {duration_ms} ms duration"
            ),
            Self::TimeOutOfRange { ms, max } => {
                write!(formatter, "time {ms} ms exceeds the limit of {max} ms")
            }
            Self::ClockRegressed {
                observed_ms,
                now_ms,
            } => write!(
                formatter,
                "time {now_ms} ms precedes the last observed time {observed_ms} ms"
            ),
            Self::Empty => formatter.write_str("the playlist has no entries"),
            Self::NoEnabledEntries => formatter.write_str("the playlist has no enabled entries"),
            Self::UnknownEntry(id) => write!(formatter, "playlist entry {id} does not exist"),
            Self::EntryDisabled(id) => write!(formatter, "playlist entry {id} is disabled"),
            Self::AlreadyActive => formatter.write_str("the playlist is already active"),
            Self::NotOnAir => formatter.write_str("no playlist entry is on air"),
            Self::NotPlaying => formatter.write_str("the playlist is not playing"),
            Self::NoNextEntry => formatter.write_str("there is no next enabled entry"),
            Self::NoPreviousEntry => formatter.write_str("there is no previous enabled entry"),
            Self::ScheduleInPast {
                start_at_ms,
                now_ms,
            } => write!(
                formatter,
                "start time {start_at_ms} ms is before the current time {now_ms} ms"
            ),
            Self::StaleTake { take, current } => match current {
                Some(current) => write!(formatter, "take {take} is not the on-air take {current}"),
                None => write!(formatter, "take {take} is not on air"),
            },
            Self::NotMediaDuration(id) => {
                write!(
                    formatter,
                    "playlist entry {id} does not follow its media length"
                )
            }
            Self::EditWhileActive => {
                formatter.write_str("playlist entries cannot be replaced while it is active")
            }
        }
    }
}

impl std::error::Error for PlaylistError {}

/// Plain-data image of a playlist, sufficient to restore it exactly.
///
/// See [`crate::encode_playlist_snapshot`] for a text encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistSnapshot {
    pub entries: Vec<PlaylistEntry>,
    pub config: PlaylistConfig,
    /// The last wall time the playlist observed.
    pub observed_ms: u64,
    /// The take number the next airing receives (takes start at 1).
    pub next_take: u64,
    pub state: PlaylistState,
}

/// Why [`Playlist::restore`] rejected a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    Entries(PlaylistError),
    TimeOutOfRange {
        ms: u64,
    },
    ZeroNextTake,
    UnknownEntry(PlaylistEntryId),
    /// Armed or on air with no entries at all.
    Empty,
    /// The take was never issued (`0` or not below `next_take`).
    UnissuedTake {
        take: u64,
        next_take: u64,
    },
    /// An armed start at or before the observed time would already have fired.
    ArmedStartPassed {
        start_at_ms: u64,
        observed_ms: u64,
    },
    /// A playing entry started after the observed time, or a paused entry
    /// accumulated more time than has been observed.
    TimeAfterObserved {
        ms: u64,
        observed_ms: u64,
    },
    /// A playing entry's known end is at or before the observed time, so the
    /// boundary would already have been processed.
    BoundaryNotProcessed {
        entry: PlaylistEntryId,
        end_ms: u64,
        observed_ms: u64,
    },
    /// A paused entry's elapsed time reaches its known duration.
    ElapsedBeyondDuration {
        entry: PlaylistEntryId,
        elapsed_ms: u64,
        duration_ms: u64,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entries(error) => error.fmt(formatter),
            Self::TimeOutOfRange { ms } => {
                write!(formatter, "snapshot time {ms} ms is out of range")
            }
            Self::ZeroNextTake => formatter.write_str("snapshot next take must be nonzero"),
            Self::UnknownEntry(id) => write!(formatter, "snapshot refers to unknown entry {id}"),
            Self::Empty => formatter.write_str("an active snapshot has no entries"),
            Self::UnissuedTake { take, next_take } => write!(
                formatter,
                "snapshot take {take} was never issued (next take {next_take})"
            ),
            Self::ArmedStartPassed {
                start_at_ms,
                observed_ms,
            } => write!(
                formatter,
                "armed start {start_at_ms} ms is not after the observed time {observed_ms} ms"
            ),
            Self::TimeAfterObserved { ms, observed_ms } => write!(
                formatter,
                "snapshot time {ms} ms is after the observed time {observed_ms} ms"
            ),
            Self::BoundaryNotProcessed {
                entry,
                end_ms,
                observed_ms,
            } => write!(
                formatter,
                "entry {entry} ended at {end_ms} ms, before the observed time {observed_ms} ms"
            ),
            Self::ElapsedBeyondDuration {
                entry,
                elapsed_ms,
                duration_ms,
            } => write!(
                formatter,
                "paused entry {entry} elapsed {elapsed_ms} ms of a {duration_ms} ms duration"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    Stopped,
    Armed {
        start_at_ms: u64,
        from: Option<usize>,
    },
    Playing {
        index: usize,
        take: u64,
        cycle: u64,
        started_at_ms: u64,
    },
    Paused {
        index: usize,
        take: u64,
        cycle: u64,
        elapsed_ms: u64,
    },
    Finished {
        last: Option<usize>,
    },
}

/// The deterministic playlist automation engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    entries: Vec<PlaylistEntry>,
    config: PlaylistConfig,
    slot: Slot,
    observed_ms: u64,
    next_take: u64,
}

impl Playlist {
    /// Creates a stopped playlist. An empty list is accepted for editing but
    /// cannot be started.
    ///
    /// # Errors
    ///
    /// Returns an error for too many entries, duplicate ids, zero or
    /// over-long durations, or invalid transitions.
    pub fn new(entries: Vec<PlaylistEntry>, config: PlaylistConfig) -> Result<Self, PlaylistError> {
        validate_entries(&entries)?;
        Ok(Self {
            entries,
            config,
            slot: Slot::Stopped,
            observed_ms: 0,
            next_take: 1,
        })
    }

    #[must_use]
    pub fn entries(&self) -> &[PlaylistEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn config(&self) -> PlaylistConfig {
        self.config
    }

    #[must_use]
    pub const fn observed_ms(&self) -> u64 {
        self.observed_ms
    }

    #[must_use]
    pub fn state(&self) -> PlaylistState {
        let id = |index: usize| self.entries[index].id;
        match self.slot {
            Slot::Stopped => PlaylistState::Stopped,
            Slot::Armed { start_at_ms, from } => PlaylistState::Armed {
                start_at_ms,
                from: from.map(id),
            },
            Slot::Playing {
                index,
                take,
                cycle,
                started_at_ms,
            } => PlaylistState::Playing {
                entry: id(index),
                take,
                cycle,
                started_at_ms,
            },
            Slot::Paused {
                index,
                take,
                cycle,
                elapsed_ms,
            } => PlaylistState::Paused {
                entry: id(index),
                take,
                cycle,
                elapsed_ms,
            },
            Slot::Finished { last } => PlaylistState::Finished { last: last.map(id) },
        }
    }

    /// The entry currently on air (playing or paused).
    #[must_use]
    pub fn on_air(&self) -> Option<&PlaylistEntry> {
        match self.slot {
            Slot::Playing { index, .. } | Slot::Paused { index, .. } => Some(&self.entries[index]),
            _ => None,
        }
    }

    /// Replaces every entry. While stopped or finished the playlist becomes
    /// stopped; while armed, playing, or paused the new list applies at once,
    /// keeping the armed start or the current entry's on-air timing. An edit
    /// that removes or disables the entry that is armed or on air is refused.
    ///
    /// # Errors
    ///
    /// Returns [`PlaylistError::EditWhileActive`] when the armed or on-air
    /// entry is not in `entries` (or is disabled), or any entry validation
    /// error.
    pub fn replace_entries(&mut self, entries: Vec<PlaylistEntry>) -> Result<(), PlaylistError> {
        validate_entries(&entries)?;
        match self.slot {
            Slot::Stopped | Slot::Finished { .. } => {
                self.entries = entries;
                self.slot = Slot::Stopped;
                Ok(())
            }
            Slot::Armed { start_at_ms, from } => {
                let from = from
                    .and_then(|index| self.entries.get(index))
                    .and_then(|entry| {
                        entries
                            .iter()
                            .position(|candidate| candidate.id == entry.id)
                    });
                self.entries = entries;
                self.slot = Slot::Armed { start_at_ms, from };
                Ok(())
            }
            Slot::Playing {
                index,
                take,
                cycle,
                started_at_ms,
            } => {
                let next_index = self.replaced_entry(&entries, index)?;
                self.entries = entries;
                self.slot = Slot::Playing {
                    index: next_index,
                    take,
                    cycle,
                    started_at_ms,
                };
                Ok(())
            }
            Slot::Paused {
                index,
                take,
                cycle,
                elapsed_ms,
            } => {
                let next_index = self.replaced_entry(&entries, index)?;
                self.entries = entries;
                self.slot = Slot::Paused {
                    index: next_index,
                    take,
                    cycle,
                    elapsed_ms,
                };
                Ok(())
            }
        }
    }

    /// The index of the entry that is currently armed or on air in a
    /// replacement list. A replacement that drops or disables it cannot be
    /// applied without interrupting the airing, so it is refused.
    fn replaced_entry(
        &self,
        entries: &[PlaylistEntry],
        current: usize,
    ) -> Result<usize, PlaylistError> {
        let current = self.entries[current].id;
        let index = entries
            .iter()
            .position(|entry| entry.id == current)
            .ok_or(PlaylistError::EditWhileActive)?;
        if entries[index].enabled {
            Ok(index)
        } else {
            Err(PlaylistError::EditWhileActive)
        }
    }

    /// Processes every time-driven boundary up to `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns an error when `now_ms` is out of range or precedes the last
    /// observed time.
    pub fn poll(&mut self, now_ms: u64) -> Result<Vec<PlaylistEvent>, PlaylistError> {
        self.check_time(now_ms)?;
        let mut step = Step::new(self, now_ms);
        step.advance();
        step.flush();
        let (slot, next_take, events) = step.finish();
        self.commit(now_ms, slot, next_take);
        Ok(events)
    }

    /// Processes boundaries up to `now_ms`, then applies `command` at
    /// `now_ms`. The returned events cover both, in order.
    ///
    /// Pause, Resume and Stop are idempotent: repeating them returns no events.
    ///
    /// # Errors
    ///
    /// Returns an error when time is invalid or the command does not apply
    /// to the state reached at `now_ms`. On error nothing changes; boundary
    /// events are delivered by the next successful call.
    pub fn apply(
        &mut self,
        now_ms: u64,
        command: PlaylistCommand,
    ) -> Result<Vec<PlaylistEvent>, PlaylistError> {
        self.check_time(now_ms)?;
        if let PlaylistCommand::SetEnabled { entry, .. } = command {
            self.index_of(entry)?;
        }
        let mut step = Step::new(self, now_ms);
        step.advance();
        step.flush();
        step.command(command, self.config.late_policy)?;
        step.flush();
        let (slot, next_take, events) = step.finish();
        self.commit(now_ms, slot, next_take);
        match command {
            PlaylistCommand::SetEnabled { entry, enabled } => {
                let index = self.index_of(entry)?;
                self.entries[index].enabled = enabled;
            }
            PlaylistCommand::SetLoopMode(mode) => self.config.loop_mode = mode,
            PlaylistCommand::SetLatePolicy(policy) => self.config.late_policy = policy,
            _ => {}
        }
        Ok(events)
    }

    /// Captures the complete state.
    #[must_use]
    pub fn snapshot(&self) -> PlaylistSnapshot {
        PlaylistSnapshot {
            entries: self.entries.clone(),
            config: self.config,
            observed_ms: self.observed_ms,
            next_take: self.next_take,
            state: self.state(),
        }
    }

    /// Rebuilds a playlist from a snapshot after checking that the state is
    /// one this engine could have produced. A restored playing entry keeps
    /// running on wall time: the next call catches up any downtime.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] describing the first inconsistency.
    pub fn restore(snapshot: PlaylistSnapshot) -> Result<Self, SnapshotError> {
        let PlaylistSnapshot {
            entries,
            config,
            observed_ms,
            next_take,
            state,
        } = snapshot;
        validate_entries(&entries).map_err(SnapshotError::Entries)?;
        if observed_ms > MAX_PLAYLIST_TIME_MS {
            return Err(SnapshotError::TimeOutOfRange { ms: observed_ms });
        }
        if next_take == 0 {
            return Err(SnapshotError::ZeroNextTake);
        }
        let slot = restore_slot(&entries, observed_ms, next_take, state)?;
        Ok(Self {
            entries,
            config,
            slot,
            observed_ms,
            next_take,
        })
    }

    fn check_time(&self, now_ms: u64) -> Result<(), PlaylistError> {
        if now_ms > MAX_PLAYLIST_TIME_MS {
            return Err(PlaylistError::TimeOutOfRange {
                ms: now_ms,
                max: MAX_PLAYLIST_TIME_MS,
            });
        }
        if now_ms < self.observed_ms {
            return Err(PlaylistError::ClockRegressed {
                observed_ms: self.observed_ms,
                now_ms,
            });
        }
        Ok(())
    }

    fn index_of(&self, id: PlaylistEntryId) -> Result<usize, PlaylistError> {
        index_of(&self.entries, id)
    }

    fn commit(&mut self, now_ms: u64, slot: Slot, next_take: u64) {
        self.observed_ms = now_ms;
        self.slot = slot;
        self.next_take = next_take;
    }
}

/// Rebuilds the internal cursor from persisted state, rejecting any state
/// the engine could not have produced.
fn restore_slot(
    entries: &[PlaylistEntry],
    observed_ms: u64,
    next_take: u64,
    state: PlaylistState,
) -> Result<Slot, SnapshotError> {
    let find = |id: PlaylistEntryId| {
        entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(SnapshotError::UnknownEntry(id))
    };
    let check_take = |take: u64| {
        if take == 0 || take >= next_take {
            Err(SnapshotError::UnissuedTake { take, next_take })
        } else {
            Ok(())
        }
    };
    let not_after_observed = |ms: u64| {
        if ms > observed_ms {
            Err(SnapshotError::TimeAfterObserved { ms, observed_ms })
        } else {
            Ok(())
        }
    };
    if !matches!(
        state,
        PlaylistState::Stopped | PlaylistState::Finished { .. }
    ) && entries.is_empty()
    {
        return Err(SnapshotError::Empty);
    }
    Ok(match state {
        PlaylistState::Stopped => Slot::Stopped,
        PlaylistState::Armed { start_at_ms, from } => {
            if start_at_ms > MAX_PLAYLIST_TIME_MS {
                return Err(SnapshotError::TimeOutOfRange { ms: start_at_ms });
            }
            if start_at_ms <= observed_ms {
                return Err(SnapshotError::ArmedStartPassed {
                    start_at_ms,
                    observed_ms,
                });
            }
            Slot::Armed {
                start_at_ms,
                from: from.map(find).transpose()?,
            }
        }
        PlaylistState::Playing {
            entry,
            take,
            cycle,
            started_at_ms,
        } => {
            let index = find(entry)?;
            check_take(take)?;
            not_after_observed(started_at_ms)?;
            if let Some(duration) = entries[index].duration.known_ms() {
                let end_ms = started_at_ms + duration;
                if end_ms <= observed_ms {
                    return Err(SnapshotError::BoundaryNotProcessed {
                        entry,
                        end_ms,
                        observed_ms,
                    });
                }
            }
            Slot::Playing {
                index,
                take,
                cycle,
                started_at_ms,
            }
        }
        PlaylistState::Paused {
            entry,
            take,
            cycle,
            elapsed_ms,
        } => {
            let index = find(entry)?;
            check_take(take)?;
            not_after_observed(elapsed_ms)?;
            if let Some(duration_ms) = entries[index].duration.known_ms()
                && elapsed_ms >= duration_ms
            {
                return Err(SnapshotError::ElapsedBeyondDuration {
                    entry,
                    elapsed_ms,
                    duration_ms,
                });
            }
            Slot::Paused {
                index,
                take,
                cycle,
                elapsed_ms,
            }
        }
        PlaylistState::Finished { last } => Slot::Finished {
            last: last.map(find).transpose()?,
        },
    })
}

fn index_of(entries: &[PlaylistEntry], id: PlaylistEntryId) -> Result<usize, PlaylistError> {
    entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or(PlaylistError::UnknownEntry(id))
}

fn validate_entries(entries: &[PlaylistEntry]) -> Result<(), PlaylistError> {
    if entries.len() > MAX_PLAYLIST_ENTRIES {
        return Err(PlaylistError::TooManyEntries {
            count: entries.len(),
            max: MAX_PLAYLIST_ENTRIES,
        });
    }
    for (position, entry) in entries.iter().enumerate() {
        if entries[..position].iter().any(|other| other.id == entry.id) {
            return Err(PlaylistError::DuplicateEntry(entry.id));
        }
        if let Some(ms) = entry.duration.known_ms() {
            if ms == 0 {
                return Err(PlaylistError::ZeroDuration(entry.id));
            }
            if ms > MAX_PLAYLIST_ENTRY_MS {
                return Err(PlaylistError::DurationTooLong {
                    entry: entry.id,
                    ms,
                    max: MAX_PLAYLIST_ENTRY_MS,
                });
            }
        }
        let transition = entry.transition;
        let valid = match transition.kind {
            PlaylistTransitionKind::Cut => {
                transition.duration_ms == 0 && transition.stinger_slot.is_none()
            }
            PlaylistTransitionKind::Stinger => {
                (1..=MAX_PLAYLIST_ENTRY_MS).contains(&transition.duration_ms)
                    && transition
                        .stinger_slot
                        .is_some_and(|slot| (1..=8).contains(&slot))
            }
            _ => {
                (1..=MAX_PLAYLIST_ENTRY_MS).contains(&transition.duration_ms)
                    && transition.stinger_slot.is_none()
            }
        };
        if !valid {
            return Err(PlaylistError::InvalidTransition(entry.id));
        }
        if let Some(duration_ms) = entry.duration.known_ms()
            && transition.duration_ms > duration_ms
        {
            return Err(PlaylistError::TransitionLongerThanEntry {
                entry: entry.id,
                transition_ms: transition.duration_ms,
                duration_ms,
            });
        }
    }
    Ok(())
}

/// An airing begun inside the current call whose `EntryOn` is not yet
/// emitted, because a later boundary in the same call may end it unseen.
#[derive(Clone, Copy)]
struct PendingOn {
    index: usize,
    take: u64,
    cycle: u64,
    at_ms: u64,
    cause: AdvanceCause,
}

/// Scratch cursor for one call. Nothing is committed unless it succeeds.
struct Step<'a> {
    entries: &'a [PlaylistEntry],
    loop_mode: LoopMode,
    now: u64,
    slot: Slot,
    next_take: u64,
    pending: Option<PendingOn>,
    events: Vec<PlaylistEvent>,
}

impl<'a> Step<'a> {
    fn new(playlist: &'a Playlist, now: u64) -> Self {
        Self {
            entries: &playlist.entries,
            loop_mode: playlist.config.loop_mode,
            now,
            slot: playlist.slot,
            next_take: playlist.next_take,
            pending: None,
            events: Vec::new(),
        }
    }

    fn finish(self) -> (Slot, u64, Vec<PlaylistEvent>) {
        (self.slot, self.next_take, self.events)
    }

    fn push(&mut self, at_ms: u64, kind: PlaylistEventKind) {
        self.events.push(PlaylistEvent { at_ms, kind });
    }

    fn flush(&mut self) {
        if let Some(pending) = self.pending.take() {
            let entry = self.entries[pending.index];
            self.push(
                pending.at_ms,
                PlaylistEventKind::EntryOn {
                    entry: entry.id,
                    input: entry.input,
                    transition: entry.transition,
                    take: pending.take,
                    cycle: pending.cycle,
                    offset_ms: self.now - pending.at_ms,
                    cause: pending.cause,
                },
            );
        }
    }

    fn advance(&mut self) {
        loop {
            match self.slot {
                Slot::Armed { start_at_ms, from } if start_at_ms <= self.now => {
                    if let Some(index) = self.first_enabled_from(from.unwrap_or(0)) {
                        self.begin(index, 0, start_at_ms, AdvanceCause::Start, false);
                    } else {
                        self.slot = Slot::Finished { last: None };
                        self.push(start_at_ms, PlaylistEventKind::Finished { last: None });
                    }
                }
                Slot::Playing {
                    index,
                    cycle,
                    started_at_ms,
                    ..
                } => {
                    let Some(duration) = self.entries[index].duration.known_ms() else {
                        break;
                    };
                    let end = started_at_ms + duration;
                    if end > self.now {
                        break;
                    }
                    self.end_entry(index, cycle, end, AdvanceCause::Duration);
                }
                _ => break,
            }
        }
    }

    /// Ends the on-air entry at `at` and puts its successor on air.
    fn end_entry(&mut self, index: usize, cycle: u64, at: u64, cause: AdvanceCause) {
        if let Some(pending) = self.pending.take() {
            let entry = self.entries[pending.index];
            self.push(
                pending.at_ms,
                PlaylistEventKind::Skipped {
                    entry: entry.id,
                    input: entry.input,
                    take: pending.take,
                },
            );
        }
        let entry = self.entries[index];
        if self.loop_mode == LoopMode::One && entry.enabled {
            let mut at = at;
            if let Some(duration) = entry.duration.known_ms() {
                // Collapse whole repeats inside the gap. They still consume
                // take numbers so takes do not depend on poll cadence.
                let repeats = (self.now - at) / duration;
                at += repeats * duration;
                self.next_take = self.next_take.saturating_add(repeats);
            }
            self.begin(index, cycle, at, cause, false);
            return;
        }
        if let Some(next) = self.next_enabled_after(index) {
            self.begin(next, cycle, at, cause, false);
            return;
        }
        if self.loop_mode == LoopMode::All
            && let Some(first) = self.first_enabled_from(0)
        {
            let mut at = at;
            let mut skipped = 0;
            if let Some(length) = self.cycle_length() {
                skipped = (self.now - at) / length;
                at += skipped * length;
                let airings = self.entries.iter().filter(|entry| entry.enabled).count();
                let airings = u64::try_from(airings).unwrap_or(u64::MAX);
                self.next_take = self
                    .next_take
                    .saturating_add(skipped.saturating_mul(airings));
            }
            let cycle = cycle.saturating_add(1).saturating_add(skipped);
            self.push(
                at,
                PlaylistEventKind::Looped {
                    cycle,
                    skipped_cycles: skipped,
                },
            );
            self.begin(first, cycle, at, cause, false);
            return;
        }
        let last = Some(entry.id);
        self.slot = Slot::Finished { last: Some(index) };
        self.push(at, PlaylistEventKind::Finished { last });
    }

    fn begin(&mut self, index: usize, cycle: u64, at: u64, cause: AdvanceCause, paused: bool) {
        let take = self.next_take;
        self.next_take = self.next_take.saturating_add(1);
        self.pending = Some(PendingOn {
            index,
            take,
            cycle,
            at_ms: at,
            cause,
        });
        if paused {
            self.slot = Slot::Paused {
                index,
                take,
                cycle,
                elapsed_ms: 0,
            };
            self.flush();
            self.push(
                at,
                PlaylistEventKind::Paused {
                    entry: self.entries[index].id,
                    take,
                    elapsed_ms: 0,
                },
            );
        } else {
            self.slot = Slot::Playing {
                index,
                take,
                cycle,
                started_at_ms: at,
            };
        }
    }

    fn command(
        &mut self,
        command: PlaylistCommand,
        late_policy: LatePolicy,
    ) -> Result<(), PlaylistError> {
        let now = self.now;
        match command {
            PlaylistCommand::Start { at, from } => self.start(at, from, late_policy),
            PlaylistCommand::Pause => self.pause(),
            PlaylistCommand::Resume => self.resume(),
            PlaylistCommand::Next => self.next(),
            PlaylistCommand::Previous => {
                let (index, cycle, paused) = self.on_air()?;
                let target = self.entries[..index]
                    .iter()
                    .rposition(|entry| entry.enabled)
                    .or_else(|| {
                        (self.loop_mode == LoopMode::All)
                            .then(|| self.entries.iter().rposition(|entry| entry.enabled))
                            .flatten()
                    })
                    .ok_or(PlaylistError::NoPreviousEntry)?;
                self.begin(target, cycle, now, AdvanceCause::Previous, paused);
                Ok(())
            }
            PlaylistCommand::JumpTo(id) => {
                let target = index_of(self.entries, id)?;
                if !self.entries[target].enabled {
                    return Err(PlaylistError::EntryDisabled(id));
                }
                let (_, cycle, paused) = self.on_air()?;
                self.begin(target, cycle, now, AdvanceCause::Jump, paused);
                Ok(())
            }
            PlaylistCommand::Stop => {
                if self.slot != Slot::Stopped {
                    self.slot = Slot::Stopped;
                    self.push(now, PlaylistEventKind::Stopped);
                }
                Ok(())
            }
            PlaylistCommand::MediaEnded { take } => self.media_ended(take),
            PlaylistCommand::SetEnabled { .. }
            | PlaylistCommand::SetLoopMode(_)
            | PlaylistCommand::SetLatePolicy(_) => Ok(()),
        }
    }

    fn pause(&mut self) -> Result<(), PlaylistError> {
        match self.slot {
            Slot::Playing {
                index,
                take,
                cycle,
                started_at_ms,
            } => {
                let elapsed_ms = self.now - started_at_ms;
                self.slot = Slot::Paused {
                    index,
                    take,
                    cycle,
                    elapsed_ms,
                };
                let entry = self.entries[index].id;
                self.push(
                    self.now,
                    PlaylistEventKind::Paused {
                        entry,
                        take,
                        elapsed_ms,
                    },
                );
                Ok(())
            }
            Slot::Paused { .. } => Ok(()),
            _ => Err(PlaylistError::NotOnAir),
        }
    }

    fn resume(&mut self) -> Result<(), PlaylistError> {
        match self.slot {
            Slot::Paused {
                index,
                take,
                cycle,
                elapsed_ms,
            } => {
                // `restore` guarantees `elapsed_ms <= observed_ms <= now`.
                let started_at_ms = self.now - elapsed_ms;
                self.slot = Slot::Playing {
                    index,
                    take,
                    cycle,
                    started_at_ms,
                };
                let entry = self.entries[index].id;
                self.push(
                    self.now,
                    PlaylistEventKind::Resumed {
                        entry,
                        take,
                        elapsed_ms,
                    },
                );
                Ok(())
            }
            Slot::Playing { .. } => Ok(()),
            _ => Err(PlaylistError::NotOnAir),
        }
    }

    fn next(&mut self) -> Result<(), PlaylistError> {
        let (index, cycle, paused) = self.on_air()?;
        let (target, cycle) = match self.next_enabled_after(index) {
            Some(target) => (target, cycle),
            None if self.loop_mode == LoopMode::All => {
                let target = self
                    .first_enabled_from(0)
                    .ok_or(PlaylistError::NoNextEntry)?;
                let cycle = cycle.saturating_add(1);
                self.push(
                    self.now,
                    PlaylistEventKind::Looped {
                        cycle,
                        skipped_cycles: 0,
                    },
                );
                (target, cycle)
            }
            None => return Err(PlaylistError::NoNextEntry),
        };
        self.begin(target, cycle, self.now, AdvanceCause::Next, paused);
        Ok(())
    }

    fn media_ended(&mut self, take: u64) -> Result<(), PlaylistError> {
        match self.slot {
            Slot::Playing {
                index,
                take: current,
                cycle,
                ..
            } => {
                if take != current {
                    return Err(PlaylistError::StaleTake {
                        take,
                        current: Some(current),
                    });
                }
                let entry = self.entries[index];
                if !matches!(entry.duration, EntryDuration::Media { .. }) {
                    return Err(PlaylistError::NotMediaDuration(entry.id));
                }
                self.end_entry(index, cycle, self.now, AdvanceCause::MediaEnded);
                Ok(())
            }
            Slot::Paused { take: current, .. } if take == current => Err(PlaylistError::NotPlaying),
            Slot::Paused { take: current, .. } => Err(PlaylistError::StaleTake {
                take,
                current: Some(current),
            }),
            _ => Err(PlaylistError::StaleTake {
                take,
                current: None,
            }),
        }
    }

    fn start(
        &mut self,
        at: StartAt,
        from: Option<PlaylistEntryId>,
        late_policy: LatePolicy,
    ) -> Result<(), PlaylistError> {
        let now = self.now;
        if !matches!(self.slot, Slot::Stopped | Slot::Finished { .. }) {
            return Err(PlaylistError::AlreadyActive);
        }
        if self.entries.is_empty() {
            return Err(PlaylistError::Empty);
        }
        let from = match from {
            Some(id) => {
                let index = index_of(self.entries, id)?;
                if !self.entries[index].enabled {
                    return Err(PlaylistError::EntryDisabled(id));
                }
                Some(index)
            }
            None => None,
        };
        if !self.entries.iter().any(|entry| entry.enabled) {
            return Err(PlaylistError::NoEnabledEntries);
        }
        let start_at_ms = match at {
            StartAt::Now => now,
            StartAt::After { delay_ms } => now
                .checked_add(delay_ms)
                .filter(|start| *start <= MAX_PLAYLIST_TIME_MS)
                .ok_or(PlaylistError::TimeOutOfRange {
                    ms: delay_ms,
                    max: MAX_PLAYLIST_TIME_MS - now,
                })?,
            StartAt::At { wall_ms } => {
                if wall_ms > MAX_PLAYLIST_TIME_MS {
                    return Err(PlaylistError::TimeOutOfRange {
                        ms: wall_ms,
                        max: MAX_PLAYLIST_TIME_MS,
                    });
                }
                if wall_ms >= now {
                    wall_ms
                } else {
                    match late_policy {
                        LatePolicy::Reject => {
                            return Err(PlaylistError::ScheduleInPast {
                                start_at_ms: wall_ms,
                                now_ms: now,
                            });
                        }
                        LatePolicy::StartNow => now,
                        LatePolicy::JoinInProgress => wall_ms,
                    }
                }
            }
        };
        self.slot = Slot::Armed { start_at_ms, from };
        self.push(now, PlaylistEventKind::Armed { start_at_ms });
        self.advance();
        Ok(())
    }

    fn on_air(&self) -> Result<(usize, u64, bool), PlaylistError> {
        match self.slot {
            Slot::Playing { index, cycle, .. } => Ok((index, cycle, false)),
            Slot::Paused { index, cycle, .. } => Ok((index, cycle, true)),
            _ => Err(PlaylistError::NotOnAir),
        }
    }

    fn first_enabled_from(&self, start: usize) -> Option<usize> {
        (start..self.entries.len())
            .chain(0..start)
            .find(|index| self.entries[*index].enabled)
    }

    fn next_enabled_after(&self, index: usize) -> Option<usize> {
        (index + 1..self.entries.len()).find(|candidate| self.entries[*candidate].enabled)
    }

    /// Total known duration of one cycle over enabled entries, or `None` when
    /// any enabled entry has an unknown length.
    fn cycle_length(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.duration.known_ms())
            .sum()
    }
}
