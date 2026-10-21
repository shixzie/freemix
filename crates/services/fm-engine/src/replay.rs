//! Instant replay (OR-012).
//!
//! The show holds what is recorded for replay
//! ([`crate::ShowState::replay`]: the source inputs, the retention window,
//! and the storage root); everything else is engine runtime, never
//! snapshotted and empty after a restart: the rolling segment catalog and
//! synchronized source timeline of the recorded media
//! ([`fm_replay::RollingSegmentCatalog`], [`fm_replay::SourceTimeline`]),
//! the in and out marks, the captured events ([`fm_replay::EventDatabase`]),
//! and every Replay input's player.
//!
//! The replay timeline is the engine's frame clock: frame `f` of every
//! source is the picture the source showed on engine frame `f`, at
//! synchronized time `f` frame durations. A source is observed once per
//! frame, so a source frame is exact and a Replay input playing at a
//! quarter, half, three quarters, or full speed advances its position by
//! one through four quarter frames a frame: at half speed every recorded
//! frame is shown on two consecutive frames.
//!
//! Replay commands are accepted independently of the recording (like media
//! transports, see [`crate::media`]): a capture, a rename, a delete, or a
//! cue naming an event that does not exist (any more) is accepted and
//! changes nothing, so journal replay after a restart, whose events are
//! gone, accepts exactly what the live show did. They are realized on their
//! target frame.
//!
//! The recording is either *synthetic* (the simulated engine records every
//! source itself into two-second segments whose files never exist, so
//! retention and capture are exercised without media) or *external* (a
//! media plane writes the segment files and reports every recorded frame
//! with [`crate::Engine::replay_observe`] and every finished segment file
//! with [`crate::Engine::replay_segment_closed`]); either way retention
//! drops segments that ended more than the retention window behind the
//! live edge unless an event holds them, and an external recording's
//! dropped files are handed back to be deleted
//! ([`crate::Engine::take_replay_pruned`]).

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU128,
    path::{Path, PathBuf},
};

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_frame::{ClockDomainId, NormalizedTimestamp, SequenceNumber};
use fm_playback::FrameIndex;
use fm_replay::{
    CameraId, CameraSource, EventDatabase, EventId, ProtectionReference, ReplayEvent,
    RollingSegmentCatalog, SegmentId, SegmentMetadata, SegmentState, SourceCatalog, SourceTimeline,
    StorageRoot, StorageRootId, TimelineRange,
};
use fm_types::{
    ChannelLayout, FrameRate, InputId,
    replay::{
        MAX_REPLAY_EVENTS, REPLAY_LAST_SECONDS, ReplaySettings, ReplaySettingsError, ReplaySpeed,
        replay_event_name_is_valid,
    },
};

use crate::ShowState;

/// Frames of one synthetic segment, in seconds of the show's frame rate.
pub const SYNTHETIC_REPLAY_SEGMENT_SECONDS: u64 = 2;
/// Bytes a synthetic segment reports per recorded frame.
pub const SYNTHETIC_REPLAY_FRAME_BYTES: u64 = 4_096;
/// The storage root a show uses when its settings name none and the
/// daemon chose none.
pub const DEFAULT_REPLAY_ROOT: &str = "replay";

/// One replay command (OR-012).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EngineReplayOperation {
    /// Replaces the replay settings: a project edit, undone by restoring
    /// the previous settings. Another source set or storage root starts a
    /// new recording: every recorded segment and every event is dropped.
    Configure(Box<ReplaySettings>),
    /// Marks the frame it is realized on as the next event's first frame.
    MarkIn,
    /// Marks the frame it is realized on as the next event's last frame.
    MarkOut,
    /// Captures the marked range as an event (named `name`, or `Event N`)
    /// and clears the marks; nothing happens without both ordered marks.
    CaptureMarks { name: Option<String> },
    /// Captures the last `seconds` (one of [`REPLAY_LAST_SECONDS`]) behind
    /// the frame it is realized on as an event named `Last N s`.
    CaptureLast { seconds: u8 },
    /// Renames event `event`.
    RenameEvent { event: u32, name: String },
    /// Deletes event `event`; Replay inputs cued on it go empty and its
    /// segments return to retention.
    DeleteEvent { event: u32 },
    /// Cues Replay input `input` paused on the first frame of event
    /// `event`, showing `angle` (or the event's preferred angle).
    Cue {
        input: InputId,
        event: u32,
        angle: Option<InputId>,
    },
    /// Switches Replay input `input` to another angle of its event, at the
    /// same position.
    Angle { input: InputId, angle: InputId },
    /// Plays Replay input `input` at `speed` from its position; it pauses
    /// on the event's last frame.
    Play { input: InputId, speed: ReplaySpeed },
    /// Pauses Replay input `input`.
    Pause { input: InputId },
    /// Jumps Replay input `input` back to its event's first frame, paused.
    CueIn { input: InputId },
}

impl EngineReplayOperation {
    /// Whether this is a live operation (everything but
    /// [`Self::Configure`]): never undone, never padlocked.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        !matches!(self, Self::Configure(_))
    }

    /// The Replay input this operation drives, if it drives one.
    #[must_use]
    pub const fn player(&self) -> Option<InputId> {
        match self {
            Self::Cue { input, .. }
            | Self::Angle { input, .. }
            | Self::Play { input, .. }
            | Self::Pause { input }
            | Self::CueIn { input } => Some(*input),
            _ => None,
        }
    }
}

/// How the replay recording is fed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EngineReplayFeed {
    /// The engine records every source itself, without files.
    #[default]
    Synthetic,
    /// A media plane records the sources and reports frames and segments.
    External,
}

/// One recorded source's retained media.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplaySourceStatus {
    pub input: InputId,
    /// Retained segments, open or closed.
    pub segments: u32,
    /// Bytes the retained segments hold.
    pub bytes: u64,
    /// The oldest retained frame, if any.
    pub first_frame: Option<u64>,
    /// The newest observed frame, if any.
    pub last_frame: Option<u64>,
}

/// One captured event as clients see it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayEventStatus {
    pub id: u32,
    pub name: String,
    /// First frame.
    pub in_frame: u64,
    /// One past the last frame.
    pub out_frame: u64,
    /// The angles (sources) the event holds, in source order.
    pub angles: Vec<InputId>,
    pub preferred_angle: InputId,
    /// Whether recorded segments cover the whole event on every angle.
    pub complete: bool,
}

/// One Replay input's player.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayPlayerStatus {
    pub input: InputId,
    pub event: Option<u32>,
    pub angle: Option<InputId>,
    pub speed: ReplaySpeed,
    pub playing: bool,
    /// The source frame shown, if cued.
    pub frame: Option<u64>,
}

/// The replay runtime as clients see it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayStatus {
    pub settings: ReplaySettings,
    /// The storage root in use (the settings' root or the daemon's).
    pub storage_root: String,
    pub feed: EngineReplayFeed,
    /// The newest frame observed on any source.
    pub live_frame: Option<u64>,
    pub sources: Vec<EngineReplaySourceStatus>,
    pub mark_in: Option<u64>,
    pub mark_out: Option<u64>,
    pub events: Vec<EngineReplayEventStatus>,
    pub players: Vec<EngineReplayPlayerStatus>,
}

/// Where a recorded frame lives on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayLocation {
    pub path: PathBuf,
    /// Frame index inside the segment file.
    pub frame: u64,
}

/// What one Replay input shows on one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayFrame {
    pub input: InputId,
    pub event: Option<u32>,
    /// The angle shown.
    pub angle: Option<InputId>,
    /// The recorded frame (engine frame of the source) shown.
    pub source_frame: Option<u64>,
    /// Where that frame was recorded, once a closed segment holds it.
    pub location: Option<EngineReplayLocation>,
    pub speed: ReplaySpeed,
    pub playing: bool,
}

#[derive(Clone, Debug, Default)]
struct Player {
    event: Option<u32>,
    angle: Option<InputId>,
    speed: ReplaySpeed,
    playing: bool,
    /// Quarter frames since the event's first frame.
    position: u64,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    input: InputId,
    first_frame: u64,
    /// Stream frame (index in the source's recording) of its first frame.
    first_stream_frame: u64,
    frames: u64,
}

#[derive(Clone, Debug)]
struct Event {
    in_frame: u64,
    out_frame: u64,
    angles: Vec<InputId>,
}

/// The replay runtime (see the module documentation).
#[derive(Clone, Debug)]
pub(crate) struct ReplayRuntime {
    feed: EngineReplayFeed,
    frame_rate: FrameRate,
    default_root: PathBuf,
    settings: ReplaySettings,
    root: PathBuf,
    catalog: RollingSegmentCatalog,
    timeline: SourceTimeline,
    segments: BTreeMap<SegmentId, Segment>,
    /// Each source's open synthetic segment and its next stream frame.
    recording: BTreeMap<InputId, (Option<SegmentId>, u64)>,
    last_frames: BTreeMap<InputId, u64>,
    next_segment: u128,
    events: EventDatabase,
    event_frames: BTreeMap<u32, Event>,
    next_event: u32,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    players: BTreeMap<InputId, Player>,
    pruned: Vec<PathBuf>,
    /// Bumped whenever anything clients see changes.
    generation: u64,
}

const ROOT: StorageRootId = StorageRootId::new(NonZeroU128::MIN);

fn camera(input: InputId) -> CameraId {
    CameraId::new(input.get())
}

impl ReplayRuntime {
    pub(crate) fn new(frame_rate: FrameRate) -> Self {
        let mut runtime = Self {
            feed: EngineReplayFeed::Synthetic,
            frame_rate,
            default_root: PathBuf::from(DEFAULT_REPLAY_ROOT),
            settings: ReplaySettings::default(),
            root: PathBuf::from(DEFAULT_REPLAY_ROOT),
            catalog: RollingSegmentCatalog::default(),
            timeline: SourceTimeline::new(&SourceCatalog::default()),
            segments: BTreeMap::new(),
            recording: BTreeMap::new(),
            last_frames: BTreeMap::new(),
            next_segment: 1,
            events: EventDatabase::default(),
            event_frames: BTreeMap::new(),
            next_event: 1,
            mark_in: None,
            mark_out: None,
            players: BTreeMap::new(),
            pruned: Vec::new(),
            generation: 0,
        };
        runtime.restart(ReplaySettings::default());
        runtime
    }

    pub(crate) const fn feed(&self) -> EngineReplayFeed {
        self.feed
    }

    pub(crate) fn set_feed(&mut self, feed: EngineReplayFeed) {
        if self.feed != feed {
            self.feed = feed;
            self.restart(self.settings.clone());
        }
    }

    pub(crate) fn set_default_root(&mut self, root: PathBuf) {
        if self.default_root != root {
            self.default_root = root;
            self.restart(self.settings.clone());
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Frames in `seconds` at the show's rate, rounded.
    fn frames_in(&self, seconds: u64) -> u64 {
        let numerator = u64::from(self.frame_rate.numerator());
        let denominator = u64::from(self.frame_rate.denominator());
        (seconds * numerator + denominator / 2) / denominator
    }

    /// The synchronized time of frame `frame`.
    fn at(&self, frame: u64) -> NormalizedTimestamp {
        let nanos = u128::from(frame) * 1_000_000_000 * u128::from(self.frame_rate.denominator())
            / u128::from(self.frame_rate.numerator());
        NormalizedTimestamp::from_nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
    }

    fn range(&self, first: u64, end: u64) -> Option<TimelineRange> {
        TimelineRange::new(self.at(first), self.at(end)).ok()
    }

    /// Drops every recorded segment and event and starts recording
    /// `settings` anew.
    fn restart(&mut self, settings: ReplaySettings) {
        self.pruned.extend(
            self.segments
                .keys()
                .filter_map(|id| self.catalog.segment(*id))
                .filter(|_| self.feed == EngineReplayFeed::External)
                .map(|segment| segment.path.clone()),
        );
        self.root = settings
            .storage_root
            .as_ref()
            .map_or_else(|| self.default_root.clone(), PathBuf::from);
        let mut catalog = RollingSegmentCatalog::default();
        let _ = catalog.add_root(StorageRoot {
            id: ROOT,
            path: self.root.clone(),
            capacity_bytes: u64::MAX,
        });
        let sources = settings
            .sources
            .iter()
            .map(|input| CameraSource {
                id: camera(*input),
                name: format!("input {input}"),
                storage_root_id: ROOT,
                recorder_id: fm_record::RecorderId::new(input.get()),
                clock_domain: ClockDomainId::new(NonZeroU128::MIN),
                audio_layout: ChannelLayout::stereo(),
            })
            .collect();
        for input in &settings.sources {
            let _ = catalog.assign_camera(camera(*input), ROOT);
        }
        let sources = SourceCatalog::new(sources).unwrap_or_default();
        self.catalog = catalog;
        self.timeline = SourceTimeline::new(&sources);
        self.segments.clear();
        self.recording = settings
            .sources
            .iter()
            .map(|input| (*input, (None, 0)))
            .collect();
        self.last_frames.clear();
        self.events = EventDatabase::default();
        self.event_frames.clear();
        self.mark_in = None;
        self.mark_out = None;
        for player in self.players.values_mut() {
            *player = Player {
                speed: player.speed,
                ..Player::default()
            };
        }
        self.settings = settings;
        self.touch();
    }

    /// Follows the show's settings: another source set or root restarts the
    /// recording; another retention only moves the cutoff.
    fn follow(&mut self, settings: &ReplaySettings) {
        if &self.settings == settings {
            return;
        }
        if self.settings.sources == settings.sources
            && self.settings.storage_root == settings.storage_root
        {
            self.settings = settings.clone();
            self.touch();
        } else {
            self.restart(settings.clone());
        }
    }

    /// Records frame `frame` of source `input` as stream frame
    /// `stream_frame` of its recording.
    pub(crate) fn observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool {
        if !self.recording.contains_key(&input) {
            return false;
        }
        let observed = self
            .timeline
            .observe(
                camera(input),
                self.at(frame),
                SequenceNumber::new(frame),
                FrameIndex::new(stream_frame),
            )
            .is_ok();
        if observed {
            self.last_frames.insert(input, frame);
        }
        observed
    }

    /// Catalogs a finished segment file of source `input`: `frames` stream
    /// frames from `first_stream_frame`, the first recorded on engine frame
    /// `first_frame`.
    pub(crate) fn segment_closed(
        &mut self,
        input: InputId,
        recorder_index: u64,
        path: PathBuf,
        first_frame: u64,
        first_stream_frame: u64,
        frames: u64,
        bytes: u64,
    ) -> bool {
        if !self.recording.contains_key(&input) || frames == 0 {
            return false;
        }
        let Some(timeline) = self.range(first_frame, first_frame + frames) else {
            return false;
        };
        let id = SegmentId::new(NonZeroU128::new(self.next_segment).unwrap_or(NonZeroU128::MIN));
        let inserted = self
            .catalog
            .insert_segment(SegmentMetadata {
                id,
                camera_id: camera(input),
                storage_root_id: ROOT,
                recorder_segment_index: recorder_index,
                timeline,
                path,
                bytes,
                state: SegmentState::Closed,
                protections: BTreeSet::new(),
            })
            .is_ok();
        if inserted {
            self.next_segment += 1;
            self.segments.insert(
                id,
                Segment {
                    input,
                    first_frame,
                    first_stream_frame,
                    frames,
                },
            );
            self.protect_events();
            self.touch();
        }
        inserted
    }

    /// Protects every segment an event overlaps, including segments
    /// cataloged after the event was captured.
    fn protect_events(&mut self) {
        for (id, event) in &self.event_frames {
            let Some(range) = self.range(event.in_frame, event.out_frame) else {
                continue;
            };
            let reference = ProtectionReference::Event(EventId::new(
                NonZeroU128::new(u128::from(*id)).unwrap_or(NonZeroU128::MIN),
            ));
            for angle in &event.angles {
                let _ = self.catalog.protect_range(camera(*angle), range, reference);
            }
        }
    }

    /// The synthetic recording of frame `frame`: every source is observed
    /// and its open segment grows, closing after
    /// [`SYNTHETIC_REPLAY_SEGMENT_SECONDS`].
    fn record_synthetic(&mut self, frame: u64) {
        let per_segment = self.frames_in(SYNTHETIC_REPLAY_SEGMENT_SECONDS).max(1);
        let sources: Vec<InputId> = self.recording.keys().copied().collect();
        for input in sources {
            let (open, stream_frame) = self.recording[&input];
            if !self.observe(input, frame, stream_frame) {
                continue;
            }
            let id = if let Some(id) = open {
                let end = self.at(frame + 1);
                let _ = self.catalog.extend_open_segment(id, end);
                if let Some(segment) = self.segments.get_mut(&id) {
                    segment.frames += 1;
                }
                id
            } else {
                let id =
                    SegmentId::new(NonZeroU128::new(self.next_segment).unwrap_or(NonZeroU128::MIN));
                let index = stream_frame / per_segment + 1;
                let Some(timeline) = self.range(frame, frame + 1) else {
                    continue;
                };
                let path = self
                    .root
                    .join(format!("synthetic-{input}"))
                    .join(fm_replay::segment_path(Path::new(""), "segment", index));
                if self
                    .catalog
                    .insert_segment(SegmentMetadata {
                        id,
                        camera_id: camera(input),
                        storage_root_id: ROOT,
                        recorder_segment_index: index,
                        timeline,
                        path,
                        bytes: 0,
                        state: SegmentState::Open,
                        protections: BTreeSet::new(),
                    })
                    .is_err()
                {
                    continue;
                }
                self.next_segment += 1;
                self.segments.insert(
                    id,
                    Segment {
                        input,
                        first_frame: frame,
                        first_stream_frame: stream_frame,
                        frames: 1,
                    },
                );
                id
            };
            let frames = self.segments.get(&id).map_or(0, |segment| segment.frames);
            let open = if frames >= per_segment {
                let _ = self.catalog.close_segment(
                    id,
                    self.at(frame + 1),
                    frames * SYNTHETIC_REPLAY_FRAME_BYTES,
                );
                None
            } else {
                Some(id)
            };
            self.recording.insert(input, (open, stream_frame + 1));
            self.protect_events();
        }
    }

    /// Drops segments that ended more than the retention window behind
    /// `frame`, unless an event holds them.
    fn retain(&mut self, frame: u64) {
        let window = self.frames_in(u64::from(self.settings.retention_seconds));
        let Some(cutoff_frame) = frame.checked_sub(window) else {
            return;
        };
        let cutoff = self.at(cutoff_frame);
        let Ok(dropped) = self.catalog.reclaim_ended_before(ROOT, cutoff) else {
            return;
        };
        if dropped.is_empty() {
            return;
        }
        for segment in dropped {
            self.segments.remove(&segment.id);
            if self.feed == EngineReplayFeed::External {
                self.pruned.push(segment.path);
            }
        }
        // Observations an event still needs stay.
        let oldest_event = self
            .event_frames
            .values()
            .map(|event| event.in_frame)
            .min()
            .unwrap_or(u64::MAX);
        let _ = self
            .timeline
            .prune_before(self.at(cutoff_frame.min(oldest_event)));
        self.touch();
    }

    fn event_id(id: u32) -> EventId {
        EventId::new(NonZeroU128::new(u128::from(id)).unwrap_or(NonZeroU128::MIN))
    }

    /// Captures frames `in_frame..out_frame` of every source as a new event.
    fn capture(&mut self, in_frame: u64, out_frame: u64, name: Option<String>) {
        let Some(&preferred) = self.settings.sources.first() else {
            return;
        };
        let Some(range) = self.range(in_frame, out_frame) else {
            return;
        };
        if out_frame <= in_frame {
            return;
        }
        // The oldest event makes room for the newest.
        while self.event_frames.len() >= MAX_REPLAY_EVENTS {
            let Some((&oldest, _)) = self.event_frames.iter().next() else {
                break;
            };
            self.delete(oldest);
        }
        let id = self.next_event;
        self.next_event = self.next_event.saturating_add(1);
        let name = name.unwrap_or_else(|| format!("Event {id}"));
        let event = ReplayEvent {
            id: Self::event_id(id),
            name,
            timeline: range,
            angles: self.settings.sources.iter().copied().map(camera).collect(),
            preferred_angle: camera(preferred),
            tags: BTreeSet::new(),
            note: String::new(),
            folders: BTreeSet::new(),
        };
        if self.events.insert_event(event).is_err() {
            return;
        }
        self.event_frames.insert(
            id,
            Event {
                in_frame,
                out_frame,
                angles: self.settings.sources.clone(),
            },
        );
        self.protect_events();
        self.touch();
    }

    fn delete(&mut self, id: u32) {
        if self.event_frames.remove(&id).is_none() {
            return;
        }
        let _ = self
            .events
            .remove_event(Self::event_id(id), &mut self.catalog);
        for player in self.players.values_mut() {
            if player.event == Some(id) {
                *player = Player {
                    speed: player.speed,
                    ..Player::default()
                };
            }
        }
        self.touch();
    }

    /// Realizes one accepted operation on frame `frame`.
    pub(crate) fn realize(
        &mut self,
        show: &ShowState,
        operation: &EngineReplayOperation,
        frame: u64,
    ) {
        self.follow(show.replay());
        match operation {
            EngineReplayOperation::Configure(_) => {}
            EngineReplayOperation::MarkIn => {
                self.mark_in = Some(frame);
                self.touch();
            }
            EngineReplayOperation::MarkOut => {
                self.mark_out = Some(frame + 1);
                self.touch();
            }
            EngineReplayOperation::CaptureMarks { name } => {
                if let (Some(in_frame), Some(out_frame)) = (self.mark_in, self.mark_out)
                    && in_frame < out_frame
                {
                    self.capture(in_frame, out_frame, name.clone());
                    self.mark_in = None;
                    self.mark_out = None;
                    self.touch();
                }
            }
            EngineReplayOperation::CaptureLast { seconds } => {
                let in_frame = frame.saturating_sub(self.frames_in(u64::from(*seconds)));
                self.capture(in_frame, frame, Some(format!("Last {seconds} s")));
            }
            EngineReplayOperation::RenameEvent { event, name } => {
                if self.event_frames.contains_key(event)
                    && let Some(stored) = self.events.event(Self::event_id(*event)).cloned()
                {
                    let _ = self
                        .events
                        .remove_event(Self::event_id(*event), &mut self.catalog);
                    let _ = self.events.insert_event(ReplayEvent {
                        name: name.clone(),
                        ..stored
                    });
                    self.protect_events();
                    self.touch();
                }
            }
            EngineReplayOperation::DeleteEvent { event } => self.delete(*event),
            EngineReplayOperation::Cue {
                input,
                event,
                angle,
            } => {
                if let Some(stored) = self.event_frames.get(event) {
                    let angle = angle
                        .filter(|angle| stored.angles.contains(angle))
                        .or_else(|| stored.angles.first().copied());
                    let player = self.players.entry(*input).or_default();
                    player.event = Some(*event);
                    player.angle = angle;
                    player.playing = false;
                    player.position = 0;
                    self.touch();
                }
            }
            EngineReplayOperation::Angle { input, angle } => {
                if let Some(player) = self.players.get_mut(input)
                    && let Some(event) = player.event.and_then(|id| self.event_frames.get(&id))
                    && event.angles.contains(angle)
                {
                    player.angle = Some(*angle);
                    self.generation = self.generation.wrapping_add(1);
                }
            }
            EngineReplayOperation::Play { input, speed } => {
                let player = self.players.entry(*input).or_default();
                player.speed = *speed;
                if player.event.is_some() {
                    player.playing = true;
                }
                self.touch();
            }
            EngineReplayOperation::Pause { input } => {
                if let Some(player) = self.players.get_mut(input) {
                    player.playing = false;
                    self.generation = self.generation.wrapping_add(1);
                }
            }
            EngineReplayOperation::CueIn { input } => {
                if let Some(player) = self.players.get_mut(input) {
                    player.playing = false;
                    player.position = 0;
                    self.generation = self.generation.wrapping_add(1);
                }
            }
        }
    }

    /// Where frame `frame` of source `input` was recorded.
    fn locate(&self, input: InputId, frame: u64) -> Option<EngineReplayLocation> {
        let at = self.at(frame);
        let observation = self.timeline.resolve(camera(input), at).ok()?;
        let metadata = self.catalog.segment_at(camera(input), at)?;
        if metadata.state != SegmentState::Closed {
            return None;
        }
        let segment = self.segments.get(&metadata.id)?;
        let index = observation
            .source_frame
            .get()
            .checked_sub(segment.first_stream_frame)?;
        (index < segment.frames).then(|| EngineReplayLocation {
            path: metadata.path.clone(),
            frame: index,
        })
    }

    /// Steps the runtime to frame `frame` and reports what every Replay
    /// input shows on it.
    pub(crate) fn step(&mut self, show: &ShowState, frame: u64) -> Vec<EngineReplayFrame> {
        self.follow(show.replay());
        if self.feed == EngineReplayFeed::Synthetic {
            self.record_synthetic(frame);
        }
        self.retain(frame);
        let inputs: BTreeSet<InputId> = show.replay_inputs().collect();
        if self.players.keys().any(|input| !inputs.contains(input)) {
            self.players.retain(|input, _| inputs.contains(input));
            self.touch();
        }
        let mut frames = Vec::with_capacity(inputs.len());
        let mut finished = false;
        for input in inputs {
            let player = self.players.entry(input).or_default().clone();
            let event = player.event.and_then(|id| self.event_frames.get(&id));
            let source_frame = event.map(|event| {
                (event.in_frame + player.position / 4).min(event.out_frame.saturating_sub(1))
            });
            let location = player
                .angle
                .zip(source_frame)
                .and_then(|(angle, source)| self.locate(angle, source));
            frames.push(EngineReplayFrame {
                input,
                event: player.event,
                angle: player.angle,
                source_frame,
                location,
                speed: player.speed,
                playing: player.playing,
            });
            if player.playing
                && let Some(event) = event
            {
                let length = event.out_frame - event.in_frame;
                let next = player.position + player.speed.quarter_frames();
                let stored = self.players.entry(input).or_default();
                if next / 4 >= length {
                    stored.position = (length - 1) * 4;
                    stored.playing = false;
                    finished = true;
                } else {
                    stored.position = next;
                }
            }
        }
        if finished {
            self.touch();
        }
        frames
    }

    /// Takes the files retention or a restart dropped.
    pub(crate) fn take_pruned(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pruned)
    }

    pub(crate) fn status(&self, show: &ShowState) -> EngineReplayStatus {
        let sources = self
            .settings
            .sources
            .iter()
            .map(|input| {
                let mut segments = 0_u32;
                let mut bytes = 0_u64;
                let mut first_frame: Option<u64> = None;
                for segment in self
                    .segments
                    .values()
                    .filter(|segment| segment.input == *input)
                {
                    segments += 1;
                    first_frame = Some(
                        first_frame
                            .map_or(segment.first_frame, |first| first.min(segment.first_frame)),
                    );
                }
                for (id, _) in self
                    .segments
                    .iter()
                    .filter(|(_, segment)| segment.input == *input)
                {
                    bytes += self.catalog.segment(*id).map_or(0, |segment| segment.bytes);
                }
                EngineReplaySourceStatus {
                    input: *input,
                    segments,
                    bytes,
                    first_frame,
                    last_frame: self.last_frames.get(input).copied(),
                }
            })
            .collect();
        let events = self
            .event_frames
            .iter()
            .filter_map(|(id, event)| {
                let stored = self.events.event(Self::event_id(*id))?;
                let range = self.range(event.in_frame, event.out_frame)?;
                let complete = event.angles.iter().all(|angle| {
                    let coverage = self.catalog.coverage(camera(*angle), range);
                    coverage.is_complete()
                        && coverage.segments.iter().all(|segment| {
                            self.catalog
                                .segment(*segment)
                                .is_some_and(|segment| segment.state == SegmentState::Closed)
                        })
                });
                Some(EngineReplayEventStatus {
                    id: *id,
                    name: stored.name.clone(),
                    in_frame: event.in_frame,
                    out_frame: event.out_frame,
                    angles: event.angles.clone(),
                    preferred_angle: event.angles.first().copied()?,
                    complete,
                })
            })
            .collect();
        let players = show
            .replay_inputs()
            .map(|input| {
                let player = self.players.get(&input).cloned().unwrap_or_default();
                let frame = player
                    .event
                    .and_then(|id| self.event_frames.get(&id))
                    .map(|event| {
                        (event.in_frame + player.position / 4)
                            .min(event.out_frame.saturating_sub(1))
                    });
                EngineReplayPlayerStatus {
                    input,
                    event: player.event,
                    angle: player.angle,
                    speed: player.speed,
                    playing: player.playing,
                    frame,
                }
            })
            .collect();
        EngineReplayStatus {
            settings: self.settings.clone(),
            storage_root: self.root.to_string_lossy().into_owned(),
            feed: self.feed,
            live_frame: self.last_frames.values().copied().max(),
            sources,
            mark_in: self.mark_in,
            mark_out: self.mark_out,
            events,
            players,
        }
    }
}

fn invalid(field: &'static str, code: &'static str, message: String) -> Rejection {
    Rejection::new(RejectionCode::InvalidCommand, message.clone())
        .with_field(FieldIssue::new(field, code, message))
}

/// The refusal of replay settings the show cannot take.
pub(crate) fn settings_rejection(error: ReplaySettingsError) -> Rejection {
    let message = error.to_string();
    match error {
        ReplaySettingsError::UnknownSource(_) => Rejection::new(
            RejectionCode::NotFound,
            message.clone(),
        )
        .with_field(FieldIssue::new("sources", "unknown_input", message)),
        ReplaySettingsError::TooManySources => invalid("sources", "too_many_sources", message),
        ReplaySettingsError::DuplicateSource(_) => invalid("sources", "duplicate_source", message),
        ReplaySettingsError::UnrecordableSource(_) => {
            invalid("sources", "unrecordable_source", message)
        }
        ReplaySettingsError::Retention => invalid("retention_seconds", "out_of_range", message),
        ReplaySettingsError::StorageRoot => invalid("storage_root", "invalid", message),
    }
}

fn check_player(state: &ShowState, input: InputId) -> Result<(), Rejection> {
    if !state.inputs().contains(&input) {
        let message = format!("input {input} is not part of this show");
        return Err(Rejection::new(RejectionCode::NotFound, message.clone())
            .with_field(FieldIssue::new("input", "unknown_input", message)));
    }
    if !matches!(
        state.input_source(input),
        Some(fm_model::InputSourceSpec::Replay)
    ) {
        return Err(invalid(
            "input",
            "not_replay_input",
            format!("input {input} is not a Replay input"),
        ));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), Rejection> {
    if replay_event_name_is_valid(name) {
        Ok(())
    } else {
        Err(invalid(
            "name",
            "invalid_name",
            format!(
                "a replay event name must be 1 to {} bytes without control characters",
                fm_types::replay::MAX_REPLAY_EVENT_NAME_BYTES
            ),
        ))
    }
}

fn check_recording(state: &ShowState) -> Result<(), Rejection> {
    if state.replay().sources.is_empty() {
        let message = "replay records no input".to_owned();
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("sources", "replay_off", message)));
    }
    Ok(())
}

/// Accepts one replay operation against the show: settings are applied,
/// everything else is checked against what the show alone decides.
pub(crate) fn accept(
    state: &mut ShowState,
    operation: &EngineReplayOperation,
) -> Result<(), Rejection> {
    match operation {
        EngineReplayOperation::Configure(settings) => state
            .set_replay((**settings).clone())
            .map_err(settings_rejection),
        EngineReplayOperation::MarkIn | EngineReplayOperation::MarkOut => check_recording(state),
        EngineReplayOperation::CaptureMarks { name } => {
            check_recording(state)?;
            name.as_deref().map_or(Ok(()), check_name)
        }
        EngineReplayOperation::CaptureLast { seconds } => {
            check_recording(state)?;
            if REPLAY_LAST_SECONDS.contains(seconds) {
                Ok(())
            } else {
                Err(invalid(
                    "seconds",
                    "out_of_range",
                    format!("a replay capture takes the last 5, 10, or 20 seconds, not {seconds}"),
                ))
            }
        }
        EngineReplayOperation::RenameEvent { name, .. } => check_name(name),
        EngineReplayOperation::DeleteEvent { .. } => Ok(()),
        EngineReplayOperation::Cue { input, angle, .. } => {
            check_player(state, *input)?;
            match angle {
                Some(angle) => check_angle(state, *angle),
                None => Ok(()),
            }
        }
        EngineReplayOperation::Angle { input, angle } => {
            check_player(state, *input)?;
            check_angle(state, *angle)
        }
        EngineReplayOperation::Play { input, .. }
        | EngineReplayOperation::Pause { input }
        | EngineReplayOperation::CueIn { input } => check_player(state, *input),
    }
}

fn check_angle(state: &ShowState, angle: InputId) -> Result<(), Rejection> {
    if state.replay().sources.contains(&angle) {
        Ok(())
    } else {
        Err(invalid(
            "angle",
            "unknown_angle",
            format!("input {angle} is not recorded for replay"),
        ))
    }
}
