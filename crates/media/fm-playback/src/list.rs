//! Ordered media-list input with deterministic auto-advance, shuffle, and
//! frame-exact programmed GO.
//!
//! A [`ListPlayer`] is the transport for one list input. The engine drives it
//! with exactly one [`ListPlayer::tick`] per output frame at a strictly
//! increasing [`ScheduleCoordinate`]. Programmed GO commands due at or before
//! the tick coordinate are applied first (ordered by coordinate, then stable
//! [`GoId`]), so the frame emitted by that tick is already the GO target's
//! first frame. No wall clock or ambient randomness is consulted: shuffle
//! order is a pure function of the seed and loop cycle.

use crate::{
    CancelGoOutcome, ClipId, ClipLibrary, EndBehavior, FrameCodec, GoId, GoScheduleOutcome,
    GoStatus, LibraryError, ListItemId, Marks, PlaybackError, PlaybackFrame, ScheduleCoordinate,
    Speed, Transport,
};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of items accepted by [`MediaList::new`].
pub const MAX_LIST_ITEMS: usize = 4096;
/// Maximum number of pending programmed GO commands per list input.
pub const MAX_PENDING_LIST_GO: usize = 1024;
/// Maximum number of retained GO records (pending plus terminal) per input.
///
/// Terminal records are retained so repeated submissions stay idempotent; use
/// [`ListPlayer::prune_go_history`] to release them explicitly.
pub const MAX_LIST_GO_RECORDS: usize = 4096;

/// One list entry: a clip, an optional inclusive marked range, and an optional
/// exact output duration in frames.
///
/// Without a duration the item ends when its marked range is exhausted. With
/// a duration the item ends after exactly that many played output frames; a
/// duration longer than the marked range holds the final marked frame (the
/// still-image case), a shorter one cuts the range early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListItem {
    id: ListItemId,
    clip_id: ClipId,
    marks: Option<Marks>,
    duration: Option<u64>,
}

impl ListItem {
    #[must_use]
    pub const fn new(id: ListItemId, clip_id: ClipId) -> Self {
        Self {
            id,
            clip_id,
            marks: None,
            duration: None,
        }
    }

    #[must_use]
    pub const fn with_marks(mut self, marks: Marks) -> Self {
        self.marks = Some(marks);
        self
    }

    /// Sets an exact played duration in output frames. Zero is rejected by
    /// [`MediaList::new`].
    #[must_use]
    pub const fn with_duration(mut self, frames: u64) -> Self {
        self.duration = Some(frames);
        self
    }

    #[must_use]
    pub const fn id(self) -> ListItemId {
        self.id
    }

    #[must_use]
    pub const fn clip_id(self) -> ClipId {
        self.clip_id
    }

    #[must_use]
    pub const fn marks(self) -> Option<Marks> {
        self.marks
    }

    #[must_use]
    pub const fn duration(self) -> Option<u64> {
        self.duration
    }
}

/// List progression options.
///
/// * `auto_next`: when an item ends, the next item in play order starts on
///   the following tick. Without it every item end holds the final frame.
/// * `loop_list`: advancing past the last item (automatically or through
///   [`ListAction::Next`]) wraps to the first item and starts a new cycle;
///   [`ListAction::Previous`] on the first item wraps to the last.
/// * `auto_first`: when the last item ends under `auto_next` without
///   `loop_list`, the first item is cued paused instead of holding the last
///   frame.
/// * `shuffle_seed`: play order is a deterministic permutation derived from
///   the seed and the loop cycle; each wrap reshuffles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ListOptions {
    pub auto_next: bool,
    pub auto_first: bool,
    pub loop_list: bool,
    pub shuffle_seed: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListError {
    Empty,
    TooManyItems {
        count: usize,
        max: usize,
    },
    DuplicateItem(ListItemId),
    ZeroDuration(ListItemId),
    Library(LibraryError),
    InvalidMarks {
        item: ListItemId,
        source: PlaybackError,
    },
}

impl fmt::Display for ListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a media list must contain at least one item"),
            Self::TooManyItems { count, max } => {
                write!(
                    formatter,
                    "media list has {count} items; the limit is {max}"
                )
            }
            Self::DuplicateItem(id) => write!(formatter, "list item {id} is duplicated"),
            Self::ZeroDuration(id) => write!(formatter, "list item {id} has a zero duration"),
            Self::Library(error) => error.fmt(formatter),
            Self::InvalidMarks { item, source } => {
                write!(formatter, "list item {item} has invalid marks: {source}")
            }
        }
    }
}

impl std::error::Error for ListError {}

impl From<LibraryError> for ListError {
    fn from(value: LibraryError) -> Self {
        Self::Library(value)
    }
}

/// A validated, bounded, ordered list of items plus progression options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaList {
    items: Vec<ListItem>,
    options: ListOptions,
}

impl MediaList {
    /// Validates item count, ID uniqueness, and durations.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty list, more than [`MAX_LIST_ITEMS`]
    /// items, a duplicate item ID, or a zero duration.
    pub fn new(items: Vec<ListItem>, options: ListOptions) -> Result<Self, ListError> {
        if items.is_empty() {
            return Err(ListError::Empty);
        }
        if items.len() > MAX_LIST_ITEMS {
            return Err(ListError::TooManyItems {
                count: items.len(),
                max: MAX_LIST_ITEMS,
            });
        }
        let mut seen = BTreeSet::new();
        for item in &items {
            if !seen.insert(item.id()) {
                return Err(ListError::DuplicateItem(item.id()));
            }
            if item.duration() == Some(0) {
                return Err(ListError::ZeroDuration(item.id()));
            }
        }
        Ok(Self { items, options })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items in authored (unshuffled) order.
    #[must_use]
    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    #[must_use]
    pub const fn options(&self) -> ListOptions {
        self.options
    }

    #[must_use]
    pub fn item(&self, id: ListItemId) -> Option<&ListItem> {
        self.items.iter().find(|item| item.id() == id)
    }
}

/// A transport action a programmed GO (or a manual control) performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListAction {
    Play(ListItemId),
    Next,
    Previous,
    First,
    Stop,
}

/// A programmed GO for one list input at an absolute output-frame coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListGo {
    pub id: GoId,
    pub coordinate: ScheduleCoordinate,
    pub action: ListAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationCause {
    Manual,
    Go(GoId),
    AutoNext,
    Loop,
    AutoFirst,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListControlError {
    NoActiveItem,
    EndOfList,
    MissingItem(ListItemId),
    Library(LibraryError),
    InvalidMarks {
        item: ListItemId,
        source: PlaybackError,
    },
}

impl fmt::Display for ListControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveItem => formatter.write_str("no list item is active"),
            Self::EndOfList => {
                formatter.write_str("the list is at its last item and does not loop")
            }
            Self::MissingItem(id) => write!(formatter, "list item {id} does not exist"),
            Self::Library(error) => error.fmt(formatter),
            Self::InvalidMarks { item, source } => {
                write!(formatter, "list item {item} has invalid marks: {source}")
            }
        }
    }
}

impl std::error::Error for ListControlError {}

/// Observable list-input events, reported by the tick that caused them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListEvent {
    GoExecuted {
        go: GoId,
        scheduled: ScheduleCoordinate,
    },
    /// The GO was consumed but its action could not apply (for example
    /// [`ListAction::Next`] at the end of a non-looping list).
    GoIgnored {
        go: GoId,
        reason: ListControlError,
    },
    ItemActivated {
        item: ListItemId,
        position: usize,
        cause: ActivationCause,
    },
    ItemEnded {
        item: ListItemId,
    },
    /// The last item of play-order cycle `cycle` ended.
    ListCompleted {
        cycle: u64,
    },
    Stopped {
        item: ListItemId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListGoError {
    UnknownGo(GoId),
    ConflictingGo(GoId),
    CoordinateElapsed(ScheduleCoordinate),
    CoordinateOverflow,
    MissingItem(ListItemId),
    TooManyPending { max: usize },
    TooManyRecords { max: usize },
}

impl fmt::Display for ListGoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownGo(id) => write!(formatter, "programmed GO {id} does not exist"),
            Self::ConflictingGo(id) => {
                write!(
                    formatter,
                    "programmed GO {id} was reused with different data"
                )
            }
            Self::CoordinateElapsed(coordinate) => {
                write!(formatter, "schedule coordinate {coordinate} has elapsed")
            }
            Self::CoordinateOverflow => formatter.write_str("schedule coordinate overflowed"),
            Self::MissingItem(id) => write!(formatter, "list item {id} does not exist"),
            Self::TooManyPending { max } => {
                write!(
                    formatter,
                    "at most {max} programmed GO commands may be pending"
                )
            }
            Self::TooManyRecords { max } => {
                write!(
                    formatter,
                    "at most {max} programmed GO records may be retained"
                )
            }
        }
    }
}

impl std::error::Error for ListGoError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListTickError {
    CoordinateNotAdvanced {
        previous: ScheduleCoordinate,
        requested: ScheduleCoordinate,
    },
    /// Frame retrieval failed. GO commands due at this coordinate were
    /// already applied and the coordinate is consumed.
    Playback {
        item: ListItemId,
        source: PlaybackError,
    },
    Control(ListControlError),
}

impl fmt::Display for ListTickError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoordinateNotAdvanced {
                previous,
                requested,
            } => write!(
                formatter,
                "tick coordinate {requested} does not advance past {previous}"
            ),
            Self::Playback { item, source } => {
                write!(formatter, "list item {item} failed to play: {source}")
            }
            Self::Control(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ListTickError {}

impl From<ListControlError> for ListTickError {
    fn from(value: ListControlError) -> Self {
        Self::Control(value)
    }
}

/// The output of one engine-frame tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListTick<F> {
    pub coordinate: ScheduleCoordinate,
    /// `None` while no item has been activated. `frame.ended` reports the
    /// item end (duration-aware), not only the marked-range end.
    pub frame: Option<PlaybackFrame<F>>,
    /// The item that produced `frame`.
    pub item: Option<ListItemId>,
    pub events: Vec<ListEvent>,
}

#[derive(Clone, Copy, Debug)]
struct GoRecord {
    command: ListGo,
    status: GoStatus,
}

#[derive(Clone, Debug)]
struct Active<F> {
    position: usize,
    transport: Transport<F>,
    playing: bool,
    elapsed: u64,
}

/// Deterministic transport for one list input.
#[derive(Clone, Debug)]
pub struct ListPlayer<F> {
    library: ClipLibrary<F>,
    list: MediaList,
    order: Vec<usize>,
    cycle: u64,
    active: Option<Active<F>>,
    go_records: BTreeMap<GoId, GoRecord>,
    pending: BTreeSet<(ScheduleCoordinate, GoId)>,
    last_tick: Option<ScheduleCoordinate>,
}

impl<F: Clone> ListPlayer<F> {
    /// Creates an idle player after validating every clip reference and
    /// marked range.
    ///
    /// # Errors
    ///
    /// Returns an error when an item references a missing clip or has marks
    /// outside that clip.
    pub fn new(library: ClipLibrary<F>, list: MediaList) -> Result<Self, ListError> {
        for item in list.items() {
            let clip = library
                .get(item.clip_id())
                .ok_or(LibraryError::MissingClip(item.clip_id()))?;
            if let Some(marks) = item.marks() {
                Transport::new(clip.clone())
                    .set_marks(marks)
                    .map_err(|source| ListError::InvalidMarks {
                        item: item.id(),
                        source,
                    })?;
            }
        }
        let order = play_order(list.len(), list.options().shuffle_seed, 0);
        Ok(Self {
            library,
            list,
            order,
            cycle: 0,
            active: None,
            go_records: BTreeMap::new(),
            pending: BTreeSet::new(),
            last_tick: None,
        })
    }

    #[must_use]
    pub const fn list(&self) -> &MediaList {
        &self.list
    }

    /// Item IDs in the current cycle's play order.
    #[must_use]
    pub fn order(&self) -> Vec<ListItemId> {
        self.order
            .iter()
            .map(|&index| self.list.items[index].id())
            .collect()
    }

    /// Zero-based loop cycle; incremented on every wrap past the last item.
    #[must_use]
    pub const fn cycle(&self) -> u64 {
        self.cycle
    }

    #[must_use]
    pub fn current_item(&self) -> Option<ListItemId> {
        self.active
            .as_ref()
            .map(|active| self.list.items[self.order[active.position]].id())
    }

    /// Position of the active item within the current play order.
    #[must_use]
    pub fn position(&self) -> Option<usize> {
        self.active.as_ref().map(|active| active.position)
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active.playing)
    }

    /// Played output frames of the active item.
    #[must_use]
    pub fn elapsed_frames(&self) -> Option<u64> {
        self.active.as_ref().map(|active| active.elapsed)
    }

    #[must_use]
    pub fn transport(&self) -> Option<&Transport<F>> {
        self.active.as_ref().map(|active| &active.transport)
    }

    #[must_use]
    pub const fn last_coordinate(&self) -> Option<ScheduleCoordinate> {
        self.last_tick
    }

    /// Immediately performs a control action (takes effect on the next tick).
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown item, a relative action with no active
    /// item, or [`ListAction::Next`] past the end of a non-looping list.
    pub fn control(&mut self, action: ListAction) -> Result<(), ListControlError> {
        let mut events = Vec::new();
        self.perform(action, ActivationCause::Manual, &mut events)
    }

    /// Pauses the active item, holding its current frame.
    ///
    /// # Errors
    ///
    /// Returns [`ListControlError::NoActiveItem`] when idle.
    pub fn pause(&mut self) -> Result<(), ListControlError> {
        let active = self.active.as_mut().ok_or(ListControlError::NoActiveItem)?;
        active.playing = false;
        active.transport.pause();
        Ok(())
    }

    /// Resumes the active item from its current frame.
    ///
    /// # Errors
    ///
    /// Returns [`ListControlError::NoActiveItem`] when idle.
    pub fn resume(&mut self) -> Result<(), ListControlError> {
        let active = self.active.as_mut().ok_or(ListControlError::NoActiveItem)?;
        active.playing = true;
        active.transport.set_speed(Speed::Forward1x);
        Ok(())
    }

    /// Schedules an idempotent programmed GO at an absolute coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown item, conflicting ID reuse, a
    /// coordinate at or before the last tick, or exhausted bounds.
    pub fn schedule_go(&mut self, command: ListGo) -> Result<GoScheduleOutcome, ListGoError> {
        if let ListAction::Play(item) = command.action
            && self.list.item(item).is_none()
        {
            return Err(ListGoError::MissingItem(item));
        }
        if let Some(record) = self.go_records.get(&command.id) {
            return if record.command == command {
                Ok(GoScheduleOutcome::Unchanged(record.status))
            } else {
                Err(ListGoError::ConflictingGo(command.id))
            };
        }
        if self
            .last_tick
            .is_some_and(|coordinate| command.coordinate <= coordinate)
        {
            return Err(ListGoError::CoordinateElapsed(command.coordinate));
        }
        if self.pending.len() >= MAX_PENDING_LIST_GO {
            return Err(ListGoError::TooManyPending {
                max: MAX_PENDING_LIST_GO,
            });
        }
        if self.go_records.len() >= MAX_LIST_GO_RECORDS {
            return Err(ListGoError::TooManyRecords {
                max: MAX_LIST_GO_RECORDS,
            });
        }
        self.pending.insert((command.coordinate, command.id));
        self.go_records.insert(
            command.id,
            GoRecord {
                command,
                status: GoStatus::Pending,
            },
        );
        Ok(GoScheduleOutcome::Scheduled)
    }

    /// Schedules a GO `frames` output frames after the last tick (or at
    /// coordinate `frames` before the first tick).
    ///
    /// The relative offset is resolved to an absolute [`ListGo`] at call
    /// time; retries must resubmit the returned absolute command through
    /// [`Self::schedule_go`] to stay idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`ListGoError::CoordinateOverflow`] when the target coordinate
    /// does not fit, or any [`Self::schedule_go`] error.
    pub fn schedule_go_in(
        &mut self,
        id: GoId,
        frames: u64,
        action: ListAction,
    ) -> Result<(ListGo, GoScheduleOutcome), ListGoError> {
        let base = self.last_tick.map_or(0, ScheduleCoordinate::get);
        let coordinate = base
            .checked_add(frames)
            .ok_or(ListGoError::CoordinateOverflow)?;
        let command = ListGo {
            id,
            coordinate: ScheduleCoordinate::new(coordinate),
            action,
        };
        let outcome = self.schedule_go(command)?;
        Ok((command, outcome))
    }

    /// Cancels a pending GO while retaining its terminal status.
    ///
    /// # Errors
    ///
    /// Returns [`ListGoError::UnknownGo`] when the ID is not retained.
    pub fn cancel_go(&mut self, id: GoId) -> Result<CancelGoOutcome, ListGoError> {
        let record = self
            .go_records
            .get_mut(&id)
            .ok_or(ListGoError::UnknownGo(id))?;
        match record.status {
            GoStatus::Pending => {
                self.pending.remove(&(record.command.coordinate, id));
                record.status = GoStatus::Cancelled;
                Ok(CancelGoOutcome::Cancelled)
            }
            GoStatus::Cancelled => Ok(CancelGoOutcome::AlreadyCancelled),
            GoStatus::Executed => Ok(CancelGoOutcome::AlreadyExecuted),
        }
    }

    #[must_use]
    pub fn go_status(&self, id: GoId) -> Option<GoStatus> {
        self.go_records.get(&id).map(|record| record.status)
    }

    #[must_use]
    pub fn pending_go_count(&self) -> usize {
        self.pending.len()
    }

    /// Pending GO IDs in execution order.
    #[must_use]
    pub fn pending_go(&self) -> Vec<(ScheduleCoordinate, GoId)> {
        self.pending.iter().copied().collect()
    }

    /// Forgets terminal (cancelled or executed) GO records whose coordinate is
    /// at or before `through`, returning how many were released. Released IDs
    /// may be reused afterwards.
    pub fn prune_go_history(&mut self, through: ScheduleCoordinate) -> usize {
        let before = self.go_records.len();
        self.go_records.retain(|_, record| {
            record.status == GoStatus::Pending || record.command.coordinate > through
        });
        before - self.go_records.len()
    }

    /// Advances one output frame at `coordinate`.
    ///
    /// Due GO commands apply first in (coordinate, ID) order, then the active
    /// item emits exactly one frame, then an item end performs the configured
    /// progression so the next tick emits the successor's first frame.
    /// Coordinates may skip values; GO commands inside a skipped gap apply at
    /// the next tick and report their original scheduled coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error when `coordinate` does not advance, or when frame
    /// retrieval fails.
    pub fn tick(
        &mut self,
        coordinate: ScheduleCoordinate,
        codec: Option<&mut dyn FrameCodec<F>>,
    ) -> Result<ListTick<F>, ListTickError> {
        if let Some(previous) = self.last_tick
            && coordinate <= previous
        {
            return Err(ListTickError::CoordinateNotAdvanced {
                previous,
                requested: coordinate,
            });
        }
        self.last_tick = Some(coordinate);

        let mut events = Vec::new();
        let due: Vec<_> = self
            .pending
            .range(..=(coordinate, GoId::new(u128::MAX)))
            .copied()
            .collect();
        for (scheduled, id) in due {
            self.pending.remove(&(scheduled, id));
            let Some(record) = self.go_records.get_mut(&id) else {
                continue;
            };
            record.status = GoStatus::Executed;
            let action = record.command.action;
            events.push(ListEvent::GoExecuted { go: id, scheduled });
            if let Err(reason) = self.perform(action, ActivationCause::Go(id), &mut events) {
                events.push(ListEvent::GoIgnored { go: id, reason });
            }
        }

        let Some(active) = self.active.as_mut() else {
            return Ok(ListTick {
                coordinate,
                frame: None,
                item: None,
                events,
            });
        };
        let item = self.list.items[self.order[active.position]];
        let mut output =
            active
                .transport
                .pull_frame(codec)
                .map_err(|source| ListTickError::Playback {
                    item: item.id(),
                    source,
                })?;
        let mut ended = false;
        if active.playing {
            active.elapsed = active.elapsed.saturating_add(1);
            ended = match item.duration() {
                Some(duration) => active.elapsed >= duration,
                None => output.ended,
            };
        }
        output.ended = ended;
        if ended {
            self.finish_item(item.id(), &mut events)?;
        }
        Ok(ListTick {
            coordinate,
            frame: Some(output),
            item: Some(item.id()),
            events,
        })
    }

    fn finish_item(
        &mut self,
        item: ListItemId,
        events: &mut Vec<ListEvent>,
    ) -> Result<(), ListControlError> {
        events.push(ListEvent::ItemEnded { item });
        let options = self.list.options();
        let position = self.position().ok_or(ListControlError::NoActiveItem)?;
        let last = position + 1 == self.order.len();
        if options.auto_next && !last {
            return self.activate(position + 1, true, ActivationCause::AutoNext, events);
        }
        if last {
            events.push(ListEvent::ListCompleted { cycle: self.cycle });
        }
        if options.auto_next && last && options.loop_list {
            self.advance_cycle();
            return self.activate(0, true, ActivationCause::Loop, events);
        }
        if options.auto_next && last && options.auto_first {
            return self.activate(0, false, ActivationCause::AutoFirst, events);
        }
        if let Some(active) = self.active.as_mut() {
            active.playing = false;
            active.transport.pause();
        }
        Ok(())
    }

    fn perform(
        &mut self,
        action: ListAction,
        cause: ActivationCause,
        events: &mut Vec<ListEvent>,
    ) -> Result<(), ListControlError> {
        let len = self.order.len();
        let loop_list = self.list.options().loop_list;
        match action {
            ListAction::Play(id) => {
                let position = self
                    .order
                    .iter()
                    .position(|&index| self.list.items[index].id() == id)
                    .ok_or(ListControlError::MissingItem(id))?;
                self.activate(position, true, cause, events)
            }
            ListAction::First => self.activate(0, true, cause, events),
            ListAction::Next => {
                let position = self.position().ok_or(ListControlError::NoActiveItem)?;
                if position + 1 < len {
                    self.activate(position + 1, true, cause, events)
                } else if loop_list {
                    self.advance_cycle();
                    self.activate(0, true, cause, events)
                } else {
                    Err(ListControlError::EndOfList)
                }
            }
            ListAction::Previous => {
                let position = self.position().ok_or(ListControlError::NoActiveItem)?;
                let target = match position.checked_sub(1) {
                    Some(previous) => previous,
                    None if loop_list => len - 1,
                    None => 0,
                };
                self.activate(target, true, cause, events)
            }
            ListAction::Stop => {
                let item = self.current_item().ok_or(ListControlError::NoActiveItem)?;
                if let Some(active) = self.active.as_mut() {
                    active.playing = false;
                    active.elapsed = 0;
                    active.transport.stop();
                }
                events.push(ListEvent::Stopped { item });
                Ok(())
            }
        }
    }

    fn activate(
        &mut self,
        position: usize,
        playing: bool,
        cause: ActivationCause,
        events: &mut Vec<ListEvent>,
    ) -> Result<(), ListControlError> {
        let item = self.list.items[self.order[position]];
        let clip = self
            .library
            .get(item.clip_id())
            .ok_or(ListControlError::Library(LibraryError::MissingClip(
                item.clip_id(),
            )))?
            .clone();
        let mut transport = Transport::new(clip);
        if let Some(marks) = item.marks() {
            transport
                .set_marks(marks)
                .map_err(|source| ListControlError::InvalidMarks {
                    item: item.id(),
                    source,
                })?;
        }
        transport.set_end_behavior(EndBehavior::Hold);
        if playing {
            transport.set_speed(Speed::Forward1x);
        } else {
            transport.pause();
        }
        self.active = Some(Active {
            position,
            transport,
            playing,
            elapsed: 0,
        });
        events.push(ListEvent::ItemActivated {
            item: item.id(),
            position,
            cause,
        });
        Ok(())
    }

    fn advance_cycle(&mut self) {
        self.cycle = self.cycle.wrapping_add(1);
        self.order = play_order(
            self.list.len(),
            self.list.options().shuffle_seed,
            self.cycle,
        );
    }
}

/// Deterministic play order: identity without a seed, otherwise a
/// Fisher-Yates permutation driven by `SplitMix64` seeded from (seed, cycle).
fn play_order(len: usize, seed: Option<u64>, cycle: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let Some(seed) = seed else {
        return order;
    };
    let mut state = seed ^ cycle.wrapping_mul(0xD1B5_4A32_D192_ED03);
    for index in (1..len).rev() {
        let bound = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(splitmix64(&mut state) % bound).unwrap_or(0);
        order.swap(index, pick);
    }
    order
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
