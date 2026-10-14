use std::collections::{BTreeMap, BTreeSet};

use fm_frame::NormalizedTimestamp;

use crate::{
    AngleSpan, CameraId, CapturedEvent, EventId, FolderId, ListId, MAX_EVENTS, MAX_EVENTS_PER_LIST,
    MAX_FOLDERS, MAX_LISTS, MAX_NOTE_BYTES, MAX_REPLAY_SOURCES, MAX_TAGS_PER_EVENT,
    ProtectionReference, ReplayError, ReplayLimit, RollingSegmentCatalog, SourceTimeline,
    TimelineRange,
    types::{check_bytes, check_count, check_name},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplayMarks {
    pub mark_in: Option<NormalizedTimestamp>,
    pub mark_out: Option<NormalizedTimestamp>,
}

impl ReplayMarks {
    pub fn set_in(&mut self, timestamp: NormalizedTimestamp) {
        self.mark_in = Some(timestamp);
    }

    pub fn set_out(&mut self, timestamp: NormalizedTimestamp) {
        self.mark_out = Some(timestamp);
    }

    /// Returns the marked half-open event range.
    ///
    /// # Errors
    ///
    /// Both marks must exist and be ordered.
    pub fn range(self) -> Result<TimelineRange, ReplayError> {
        TimelineRange::new(
            self.mark_in.ok_or(ReplayError::MarkIncomplete)?,
            self.mark_out.ok_or(ReplayError::MarkIncomplete)?,
        )
    }

    /// Creates a last-N-seconds range ending at the supplied live edge.
    ///
    /// # Errors
    ///
    /// Zero or overflowing durations are rejected.
    pub fn last_n(
        live_edge: NormalizedTimestamp,
        seconds: u64,
    ) -> Result<TimelineRange, ReplayError> {
        let duration = seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(ReplayError::InvalidLastN)?;
        if duration == 0 {
            return Err(ReplayError::InvalidLastN);
        }
        let start = live_edge
            .as_nanos()
            .checked_sub(duration)
            .ok_or(ReplayError::InvalidLastN)?;
        TimelineRange::new(NormalizedTimestamp::from_nanos(start), live_edge)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayEvent {
    pub id: EventId,
    pub name: String,
    pub timeline: TimelineRange,
    pub angles: BTreeSet<CameraId>,
    pub preferred_angle: CameraId,
    pub tags: BTreeSet<String>,
    pub note: String,
    pub folders: BTreeSet<FolderId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventList {
    pub id: ListId,
    pub name: String,
    pub events: Vec<EventId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventFolder {
    pub id: FolderId,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct EventDatabase {
    events: BTreeMap<EventId, ReplayEvent>,
    lists: BTreeMap<ListId, EventList>,
    folders: BTreeMap<FolderId, EventFolder>,
}

impl EventDatabase {
    /// Adds a multi-angle event.
    ///
    /// # Errors
    ///
    /// IDs must be unique, names and angle sets non-empty, and the preferred
    /// angle must be one of the event angles. Names, notes, tags, angle count,
    /// folder references, and total event count are bounded.
    pub fn insert_event(&mut self, event: ReplayEvent) -> Result<(), ReplayError> {
        self.validate_event(&event)?;
        self.events.insert(event.id, event);
        Ok(())
    }

    fn validate_event(&self, event: &ReplayEvent) -> Result<(), ReplayError> {
        check_name(&event.name)?;
        check_bytes(&event.note, ReplayLimit::NoteBytes, MAX_NOTE_BYTES)?;
        check_count(
            event.tags.len(),
            ReplayLimit::TagsPerEvent,
            MAX_TAGS_PER_EVENT,
        )?;
        for tag in &event.tags {
            check_name(tag)?;
        }
        if event.angles.len() > MAX_REPLAY_SOURCES {
            return Err(ReplayError::TooManySources(event.angles.len()));
        }
        if !event.angles.contains(&event.preferred_angle) {
            return Err(ReplayError::PreferredAngleUnavailable(
                event.preferred_angle,
            ));
        }
        if let Some(folder) = event
            .folders
            .iter()
            .find(|folder| !self.folders.contains_key(folder))
        {
            return Err(ReplayError::UnknownFolder(*folder));
        }
        if self.events.contains_key(&event.id) {
            return Err(ReplayError::DuplicateEvent(event.id));
        }
        check_count(self.events.len() + 1, ReplayLimit::Events, MAX_EVENTS)
    }

    /// Validates a marked event against recorded media for every angle,
    /// protects the covering segments from retention, and inserts it.
    ///
    /// Either everything succeeds or neither the database nor the catalog is
    /// changed.
    ///
    /// # Errors
    ///
    /// All [`Self::insert_event`] rules apply. Every angle must also have
    /// gap-free segment coverage of the event range and at least one observed
    /// frame inside it.
    pub fn capture_event(
        &mut self,
        event: ReplayEvent,
        catalog: &mut RollingSegmentCatalog,
        timeline: &SourceTimeline,
    ) -> Result<CapturedEvent, ReplayError> {
        self.validate_event(&event)?;
        let mut angles = Vec::with_capacity(event.angles.len());
        for &camera in &event.angles {
            let coverage = catalog.coverage(camera, event.timeline);
            if let Some(&gap) = coverage.gaps.first() {
                return Err(ReplayError::CoverageGap { camera, gap });
            }
            let frames = timeline.frames_in(camera, event.timeline)?;
            angles.push(AngleSpan {
                frames,
                segments: coverage.segments,
            });
        }
        let reference = ProtectionReference::Event(event.id);
        for &camera in &event.angles {
            let _protected = catalog.protect_range(camera, event.timeline, reference);
        }
        let captured = CapturedEvent {
            event_id: event.id,
            timeline: event.timeline,
            angles,
        };
        self.events.insert(event.id, event);
        Ok(captured)
    }

    /// Removes an event, its list entries, and its retention protection.
    ///
    /// # Errors
    ///
    /// The event must exist.
    pub fn remove_event(
        &mut self,
        id: EventId,
        catalog: &mut RollingSegmentCatalog,
    ) -> Result<ReplayEvent, ReplayError> {
        let event = self
            .events
            .remove(&id)
            .ok_or(ReplayError::UnknownEvent(id))?;
        for list in self.lists.values_mut() {
            list.events.retain(|entry| *entry != id);
        }
        catalog.release_reference(ProtectionReference::Event(id));
        Ok(event)
    }

    /// Events carrying `tag`, in ascending event-ID order.
    #[must_use]
    pub fn events_with_tag(&self, tag: &str) -> Vec<EventId> {
        self.events
            .values()
            .filter(|event| event.tags.contains(tag))
            .map(|event| event.id)
            .collect()
    }

    /// Events placed in `folder_id`, in ascending event-ID order.
    #[must_use]
    pub fn events_in_folder(&self, folder_id: FolderId) -> Vec<EventId> {
        self.events
            .values()
            .filter(|event| event.folders.contains(&folder_id))
            .map(|event| event.id)
            .collect()
    }

    pub fn lists(&self) -> impl Iterator<Item = &EventList> {
        self.lists.values()
    }

    #[must_use]
    pub fn event(&self, id: EventId) -> Option<&ReplayEvent> {
        self.events.get(&id)
    }

    pub fn events(&self) -> impl Iterator<Item = &ReplayEvent> {
        self.events.values()
    }

    /// Replaces an event note.
    ///
    /// # Errors
    ///
    /// The event must exist and the note must fit [`MAX_NOTE_BYTES`].
    pub fn set_note(&mut self, id: EventId, note: impl Into<String>) -> Result<(), ReplayError> {
        let note = note.into();
        check_bytes(&note, ReplayLimit::NoteBytes, MAX_NOTE_BYTES)?;
        self.events
            .get_mut(&id)
            .ok_or(ReplayError::UnknownEvent(id))?
            .note = note;
        Ok(())
    }

    /// Adds a non-empty tag to an event.
    ///
    /// # Errors
    ///
    /// The event must exist, the tag must be a bounded non-empty name, and an
    /// event carries at most [`MAX_TAGS_PER_EVENT`] tags.
    pub fn add_tag(&mut self, id: EventId, tag: impl Into<String>) -> Result<(), ReplayError> {
        let tag = tag.into();
        check_name(&tag)?;
        let tags = &mut self
            .events
            .get_mut(&id)
            .ok_or(ReplayError::UnknownEvent(id))?
            .tags;
        if !tags.contains(&tag) {
            check_count(
                tags.len() + 1,
                ReplayLimit::TagsPerEvent,
                MAX_TAGS_PER_EVENT,
            )?;
            tags.insert(tag);
        }
        Ok(())
    }

    /// Adds a named folder.
    ///
    /// # Errors
    ///
    /// Empty or oversized names, duplicate IDs, and folders beyond
    /// [`MAX_FOLDERS`] are rejected.
    pub fn add_folder(&mut self, folder: EventFolder) -> Result<(), ReplayError> {
        check_name(&folder.name)?;
        if self.folders.contains_key(&folder.id) {
            return Err(ReplayError::DuplicateFolder(folder.id));
        }
        check_count(self.folders.len() + 1, ReplayLimit::Folders, MAX_FOLDERS)?;
        self.folders.insert(folder.id, folder);
        Ok(())
    }

    /// Adds an event to a folder without removing other folder memberships.
    ///
    /// # Errors
    ///
    /// Both event and folder must exist.
    pub fn place_in_folder(
        &mut self,
        event_id: EventId,
        folder_id: FolderId,
    ) -> Result<(), ReplayError> {
        if !self.folders.contains_key(&folder_id) {
            return Err(ReplayError::UnknownFolder(folder_id));
        }
        self.events
            .get_mut(&event_id)
            .ok_or(ReplayError::UnknownEvent(event_id))?
            .folders
            .insert(folder_id);
        Ok(())
    }

    /// Adds an ordered named event list.
    ///
    /// # Errors
    ///
    /// Empty/duplicate lists, references to unknown or repeated events, and
    /// lists beyond [`MAX_LISTS`] or [`MAX_EVENTS_PER_LIST`] are rejected.
    pub fn add_list(&mut self, list: EventList) -> Result<(), ReplayError> {
        check_name(&list.name)?;
        if self.lists.contains_key(&list.id) {
            return Err(ReplayError::DuplicateList(list.id));
        }
        check_count(self.lists.len() + 1, ReplayLimit::Lists, MAX_LISTS)?;
        check_count(
            list.events.len(),
            ReplayLimit::EventsPerList,
            MAX_EVENTS_PER_LIST,
        )?;
        let mut seen = BTreeSet::new();
        for event in &list.events {
            if !self.events.contains_key(event) {
                return Err(ReplayError::UnknownEvent(*event));
            }
            if !seen.insert(*event) {
                return Err(ReplayError::EventAlreadyInList {
                    event: *event,
                    list: list.id,
                });
            }
        }
        self.lists.insert(list.id, list);
        Ok(())
    }

    /// Appends one event to an existing list.
    ///
    /// # Errors
    ///
    /// The list and event must exist, and an event may occur only once per list.
    pub fn append_to_list(
        &mut self,
        list_id: ListId,
        event_id: EventId,
    ) -> Result<(), ReplayError> {
        if !self.events.contains_key(&event_id) {
            return Err(ReplayError::UnknownEvent(event_id));
        }
        let list = self
            .lists
            .get_mut(&list_id)
            .ok_or(ReplayError::UnknownList(list_id))?;
        if list.events.contains(&event_id) {
            return Err(ReplayError::EventAlreadyInList {
                event: event_id,
                list: list_id,
            });
        }
        check_count(
            list.events.len() + 1,
            ReplayLimit::EventsPerList,
            MAX_EVENTS_PER_LIST,
        )?;
        list.events.push(event_id);
        Ok(())
    }

    #[must_use]
    pub fn list(&self, id: ListId) -> Option<&EventList> {
        self.lists.get(&id)
    }

    #[must_use]
    pub fn folder(&self, id: FolderId) -> Option<&EventFolder> {
        self.folders.get(&id)
    }
}
