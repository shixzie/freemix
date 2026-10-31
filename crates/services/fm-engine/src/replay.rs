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
//! A Replay input may carry a playlist (Protocol 2.119): an ordered list of
//! events with an optional angle and a speed. A playing player that reaches
//! the end of its event moves on to the next listed entry that still
//! exists, from that event's first frame and at that entry's speed; only a
//! playlist with nothing left stops the player (and returns Program when
//! auto return is set).
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
    sync::Arc,
};

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_frame::{ClockDomainId, NormalizedTimestamp, SequenceNumber};
use fm_playback::FrameIndex;
use fm_replay::{
    CameraId, CameraSource, CompiledReel, EventDatabase, EventId, MAX_MULTIVIEW_ANGLES,
    MAX_RECORD_EXPORT_JOBS, MAX_SEGMENTS_PER_EXPORT, ProtectionReference, RecordExportError,
    RecordExportQueue, RecordExportState, ReplayError, ReplayEvent, RollingSegmentCatalog,
    SegmentId, SegmentMetadata, SegmentState, SourceCatalog, SourceTimeline, StorageRoot,
    StorageRootId, TimelineRange,
};
use fm_types::{
    ChannelLayout, FrameRate, InputId,
    replay::{
        MAX_REPLAY_EVENTS, REPLAY_LAST_SECONDS, ReplaySettings, ReplaySettingsError, ReplaySpeed,
        replay_event_name_is_valid,
    },
};

use crate::ShowState;
use crate::reel::{
    EngineReelChunk, EngineReelError, EngineReelExportState, EngineReelExportStatus,
    EngineReelStatus, EngineReelSummary, ReelExportPlan,
};

/// Frames of one synthetic segment, in seconds of the show's frame rate.
pub const SYNTHETIC_REPLAY_SEGMENT_SECONDS: u64 = 2;
/// Bytes a synthetic segment reports per recorded frame.
pub const SYNTHETIC_REPLAY_FRAME_BYTES: u64 = 4_096;
/// The storage root a show uses when its settings name none and the
/// daemon chose none.
pub const DEFAULT_REPLAY_ROOT: &str = "replay";

/// One entry in a Replay input's playlist (Protocol 2.119).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EngineReplayPlaylistEntry {
    /// The event to play.
    pub event: u32,
    /// The angle to play; `None` plays the event's preferred one.
    pub angle: Option<InputId>,
    /// The speed the entry plays at.
    pub speed: ReplaySpeed,
}

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
    /// Shuttles Replay input `input` at `rate_milli` thousandths of real
    /// time (negative plays backwards); it pauses at the event's first or
    /// last frame. Zero pauses.
    Shuttle { input: InputId, rate_milli: i32 },
    /// Pauses Replay input `input`.
    Pause { input: InputId },
    /// Jumps Replay input `input` back to its event's first frame, paused.
    CueIn { input: InputId },
    /// Moves a cued Replay input `frames` source frames (back when
    /// negative), paused, within its event (jog).
    Step { input: InputId, frames: i32 },
    /// Links Replay input `input` to `linked` for A/B playback, or clears
    /// its link with `linked: None` (Protocol 2.124).
    Link {
        input: InputId,
        linked: Option<InputId>,
    },
    /// Replaces Replay input `input`'s playlist (Protocol 2.119); an empty
    /// list clears it.
    PlaylistSet {
        input: InputId,
        entries: Vec<EngineReplayPlaylistEntry>,
    },
    /// Cues the next playlist entry that still exists, at its first frame,
    /// keeping the player playing or paused as it was.
    PlaylistNext { input: InputId },
    /// Cues the previous playlist entry that still exists, at its first
    /// frame, keeping the player playing or paused as it was.
    PlaylistPrev { input: InputId },
    /// Program went back from Replay input `input`, which played its event
    /// to the end on air, to `to` (auto return). Only the cue clock's
    /// advance issues it ([`REPLAY_RETURN_LIST`]); a client's is refused.
    Return { input: InputId, to: InputId },
}

/// The cue list number the cue clock's `cue_advance` names for an auto
/// return (no cue list may take it).
pub const REPLAY_RETURN_LIST: u64 = u64::MAX;

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
            | Self::Shuttle { input, .. }
            | Self::Pause { input }
            | Self::CueIn { input }
            | Self::Step { input, .. }
            | Self::Link { input, .. }
            | Self::PlaylistSet { input, .. }
            | Self::PlaylistNext { input }
            | Self::PlaylistPrev { input }
            | Self::Return { input, .. } => Some(*input),
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
    /// The shuttle rate, or zero when not shuttling.
    pub rate_milli: i32,
    pub shuttling: bool,
    pub playing: bool,
    /// The source frame shown, if cued.
    pub frame: Option<u64>,
    /// The player's playlist, in order (Protocol 2.119).
    pub playlist: Vec<EngineReplayPlaylistEntry>,
    /// The playlist entry the player is on, while it is on one.
    pub playlist_index: Option<u32>,
    /// The Replay input this player plays in step with, if any (2.124).
    pub linked: Option<InputId>,
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
    /// Free space and the predicted recording time at the observed write
    /// rate, while the daemon samples the storage root (OR-014).
    pub storage: Option<EngineReplayStorageStatus>,
}

/// How close a replay storage root is to full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineReplayStorageLevel {
    Ok,
    /// Less than ten minutes at the observed write rate.
    Warning,
    /// Less than two minutes at the observed write rate.
    Critical,
    /// No free space, or the root cannot be sampled.
    Full,
}

/// A drive's SMART self-assessment (OR-014, 2.139).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineReplaySmartHealth {
    Passed,
    Warning,
    Failed,
    Unknown,
}

/// The SMART health of the drive holding the replay root (OR-014, 2.139).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineReplaySmartStatus {
    pub health: EngineReplaySmartHealth,
    pub temperature_celsius: Option<u16>,
    pub percentage_used: Option<u8>,
}

/// Free space and the observed write rate of the replay root (OR-014).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineReplayStorageStatus {
    pub free_bytes: u64,
    /// Bytes written per second by the retained recording, from its closed
    /// segment files; zero until a segment reports its size.
    pub bytes_per_second: u64,
    /// Seconds of recording left at `bytes_per_second`, when it is nonzero.
    pub remaining_seconds: Option<u64>,
    pub level: EngineReplayStorageLevel,
    /// The root drive's SMART status, when the daemon samples it (2.139).
    pub smart: Option<EngineReplaySmartStatus>,
}

/// Seconds of storage left that still reads as a warning.
pub const REPLAY_STORAGE_WARNING_SECONDS: u64 = 600;
/// Seconds of storage left that reads as critical.
pub const REPLAY_STORAGE_CRITICAL_SECONDS: u64 = 120;

/// One angle tile of a Replay input's multiview at its current position
/// (OR-014).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayTile {
    /// The recorded angle.
    pub angle: InputId,
    /// The engine frame the tile shows.
    pub source_frame: u64,
    /// Where that frame is, once a closed segment holds it.
    pub location: Option<EngineReplayLocation>,
}

/// The four-angle view of one Replay input (OR-014): its cued event, the
/// engine frame shown, and one tile per angle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReplayMultiview {
    pub input: InputId,
    pub event: u32,
    pub source_frame: u64,
    /// The Replay input itself, then its linked A/B partner when it has one:
    /// each channel's own angle and position at this instant.
    pub channels: Vec<EngineReplayTile>,
    /// Up to [`MAX_MULTIVIEW_ANGLES`] angles, in the event's source order.
    pub tiles: Vec<EngineReplayTile>,
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
    /// The shuttle rate, or zero when not shuttling.
    pub rate_milli: i32,
    pub shuttling: bool,
    pub playing: bool,
}

#[derive(Clone, Debug, Default)]
struct Player {
    event: Option<u32>,
    angle: Option<InputId>,
    speed: ReplaySpeed,
    playing: bool,
    /// The shuttle rate in thousandths of real time while shuttling.
    rate_milli: i32,
    /// Whether the player advances at `rate_milli` instead of `speed`.
    shuttling: bool,
    /// Milli-quarter-frames carried between frames while shuttling.
    shuttle_remainder: i32,
    /// Quarter frames since the event's first frame.
    position: u64,
    /// The player played its event to the end while on Program, and
    /// Program has not left it since (auto return).
    ended_on_air: bool,
    /// The playlist entries, in order (Protocol 2.119).
    playlist: Vec<EngineReplayPlaylistEntry>,
    /// The entry the player is on, while it is on one.
    playlist_index: Option<usize>,
    /// The Replay input this player plays in step with (Protocol 2.124).
    linked: Option<InputId>,
}

impl Player {
    /// Cues `entry`, the playlist's `index`th, on its first frame: its
    /// angle when that event holds it, else the event's preferred one, at
    /// the entry's speed.
    fn cue_entry(&mut self, index: usize, entry: EngineReplayPlaylistEntry, event: &Event) {
        self.playlist_index = Some(index);
        self.event = Some(entry.event);
        self.angle = entry
            .angle
            .filter(|angle| event.angles.contains(angle))
            .or_else(|| event.angles.first().copied());
        self.speed = entry.speed;
        self.rate_milli = 0;
        self.shuttling = false;
        self.shuttle_remainder = 0;
        self.position = 0;
    }

    /// The first playlist entry after the current one whose event still
    /// exists.
    fn next_playlist_entry(
        &self,
        events: &BTreeMap<u32, Event>,
    ) -> Option<(usize, EngineReplayPlaylistEntry)> {
        let index = self.playlist_index?;
        self.playlist
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, entry)| events.contains_key(&entry.event))
            .map(|(next, entry)| (next, *entry))
    }

    /// Cues the playlist entry before (`forward` false) or after the
    /// current one, skipping entries whose event is gone; the player keeps
    /// playing or paused as it was.
    fn move_playlist(&mut self, events: &BTreeMap<u32, Event>, forward: bool) {
        if self.playlist.is_empty() {
            return;
        }
        let last = self.playlist.len() - 1;
        let mut cursor = match self.playlist_index {
            Some(index) if forward => index + 1,
            Some(0) => return,
            Some(index) => index - 1,
            None if forward => 0,
            None => last,
        };
        if cursor > last {
            return;
        }
        loop {
            let entry = self.playlist[cursor];
            if let Some(event) = events.get(&entry.event) {
                self.cue_entry(cursor, entry, event);
                return;
            }
            if forward {
                if cursor == last {
                    return;
                }
                cursor += 1;
            } else {
                if cursor == 0 {
                    return;
                }
                cursor -= 1;
            }
        }
    }
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

/// The recorded media and the events: shared between an engine and its
/// clones (a projected frame clones the engine every frame) until one of
/// them changes it.
#[derive(Clone, Debug)]
struct Store {
    catalog: RollingSegmentCatalog,
    timeline: SourceTimeline,
    segments: BTreeMap<SegmentId, Segment>,
    /// Each source's open synthetic segment and its next stream frame.
    recording: BTreeMap<InputId, (Option<SegmentId>, u64)>,
    last_frames: BTreeMap<InputId, u64>,
    events: EventDatabase,
    event_frames: BTreeMap<u32, Event>,
}

/// The recorded media a reel compiles and resolves against, without the
/// replay runtime's private bookkeeping (OR-013).
pub(crate) struct ReelMedia<'a> {
    pub(crate) events: &'a EventDatabase,
    pub(crate) timeline: &'a SourceTimeline,
    pub(crate) catalog: &'a RollingSegmentCatalog,
    segments: &'a BTreeMap<SegmentId, Segment>,
}

impl ReelMedia<'_> {
    /// Where segment `id` starts on disk: its file and the stream frame of
    /// its first frame.
    pub(crate) fn segment_origin(&self, id: SegmentId) -> Option<(PathBuf, u64)> {
        let path = self.catalog.segment(id)?.path.clone();
        let first_stream_frame = self.segments.get(&id)?.first_stream_frame;
        Some((path, first_stream_frame))
    }

    /// Mirrors [`RecordExportQueue::submit`]'s media checks without taking
    /// the job: every clip is covered gap-free by finalized segments, its
    /// lead frame resolves, and it reads at most
    /// [`MAX_SEGMENTS_PER_EXPORT`] distinct segments.
    pub(crate) fn export_ready(&self, reel: &CompiledReel) -> Result<(), RecordExportError> {
        let mut segments = BTreeSet::new();
        for clip in reel.clips() {
            let coverage = self.catalog.coverage(clip.angle, clip.source);
            if let Some(&gap) = coverage.gaps.first() {
                return Err(RecordExportError::CoverageGap { clip: clip.id, gap });
            }
            let lead = self
                .timeline
                .resolve(clip.angle, clip.source.start)
                .map_err(|error| RecordExportError::Unresolved {
                    clip: clip.id,
                    error,
                })?;
            let lead_segment = self
                .catalog
                .segment_at(clip.angle, lead.synchronized_at)
                .ok_or(RecordExportError::Unresolved {
                    clip: clip.id,
                    error: ReplayError::SegmentUnavailable {
                        camera: clip.angle,
                        at: lead.synchronized_at,
                    },
                })?
                .id;
            for segment_id in coverage.segments.into_iter().chain([lead_segment]) {
                let segment =
                    self.catalog
                        .segment(segment_id)
                        .ok_or(RecordExportError::Unresolved {
                            clip: clip.id,
                            error: ReplayError::UnknownSegment(segment_id),
                        })?;
                if segment.state != SegmentState::Closed {
                    return Err(RecordExportError::SegmentNotFinalized {
                        clip: clip.id,
                        segment: segment_id,
                    });
                }
                segments.insert(segment_id);
            }
            self.timeline
                .frames_in(clip.angle, clip.source)
                .map_err(|error| RecordExportError::Unresolved {
                    clip: clip.id,
                    error,
                })?;
            if segments.len() > MAX_SEGMENTS_PER_EXPORT {
                return Err(RecordExportError::TooManySegments {
                    maximum: MAX_SEGMENTS_PER_EXPORT,
                });
            }
        }
        Ok(())
    }
}

/// What the media plane reported about a terminal reel export beyond the
/// frame planner's state.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ReelExportOutcome {
    /// The file was written; its size.
    Finished { bytes: u64 },
    /// The media plane failed to write it.
    Failed { reason: String },
    /// The operator cancelled it while it was finalizing.
    Cancelled,
}

/// One accepted reel export's project data and reported outcome.
#[derive(Clone, Debug)]
struct ReelExportMeta {
    reel: u64,
    destination: String,
    total_frames: u64,
    music: Option<fm_types::reel::ReelMusic>,
    outcome: Option<ReelExportOutcome>,
}

/// The replay runtime (see the module documentation).
#[derive(Clone, Debug)]
pub(crate) struct ReplayRuntime {
    feed: EngineReplayFeed,
    frame_rate: FrameRate,
    default_root: PathBuf,
    settings: ReplaySettings,
    root: PathBuf,
    store: Arc<Store>,
    next_segment: u128,
    next_event: u32,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    players: BTreeMap<InputId, Player>,
    /// The last source Program showed that is not a Replay input (auto
    /// return goes back to it).
    live_program: Option<InputId>,
    pruned: Vec<PathBuf>,
    /// The reel exports, in job order (OR-013).
    exports: RecordExportQueue,
    /// Every accepted export's reel, destination, and reported outcome.
    export_meta: BTreeMap<u64, ReelExportMeta>,
    /// Exports whose media is not finalized yet: their segments are still
    /// open, so they wait for the recorder to close them (OR-013).
    deferred_exports: BTreeMap<u64, ReelExportPlan>,
    /// Free bytes on the storage root, as the daemon last sampled it.
    storage_free: Option<u64>,
    /// The root drive's SMART status, as the daemon last sampled it
    /// (OR-014, 2.139).
    storage_smart: Option<EngineReplaySmartStatus>,
    /// Bumped whenever anything clients see changes.
    generation: u64,
}

const ROOT: StorageRootId = StorageRootId::new(NonZeroU128::MIN);

fn camera(input: InputId) -> CameraId {
    CameraId::new(input.get())
}

/// The synchronized time of frame `frame` at `rate`.
fn time_of(rate: FrameRate, frame: u64) -> NormalizedTimestamp {
    let nanos = u128::from(frame) * 1_000_000_000 * u128::from(rate.denominator())
        / u128::from(rate.numerator());
    NormalizedTimestamp::from_nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
}

/// The frame at synchronized time `time` at `rate`, rounded down.
fn frame_of(rate: FrameRate, time: NormalizedTimestamp) -> u64 {
    let nanos = u128::try_from(time.as_nanos()).unwrap_or(0);
    let frames =
        nanos * u128::from(rate.numerator()) / (1_000_000_000 * u128::from(rate.denominator()));
    u64::try_from(frames).unwrap_or(u64::MAX)
}

fn range_of(rate: FrameRate, first: u64, end: u64) -> Option<TimelineRange> {
    TimelineRange::new(time_of(rate, first), time_of(rate, end)).ok()
}

impl ReplayRuntime {
    pub(crate) fn new(frame_rate: FrameRate) -> Self {
        let mut runtime = Self {
            feed: EngineReplayFeed::Synthetic,
            frame_rate,
            default_root: PathBuf::from(DEFAULT_REPLAY_ROOT),
            settings: ReplaySettings::default(),
            root: PathBuf::from(DEFAULT_REPLAY_ROOT),
            store: Arc::new(Store {
                catalog: RollingSegmentCatalog::default(),
                timeline: SourceTimeline::new(&SourceCatalog::default()),
                segments: BTreeMap::new(),
                recording: BTreeMap::new(),
                last_frames: BTreeMap::new(),
                events: EventDatabase::default(),
                event_frames: BTreeMap::new(),
            }),
            next_segment: 1,
            next_event: 1,
            mark_in: None,
            mark_out: None,
            players: BTreeMap::new(),
            live_program: None,
            pruned: Vec::new(),
            exports: RecordExportQueue::default(),
            export_meta: BTreeMap::new(),
            deferred_exports: BTreeMap::new(),
            storage_free: None,
            storage_smart: None,
            generation: 0,
        };
        runtime.restart(ReplaySettings::default());
        runtime
    }

    /// Whether the replay recorder is observing any input.
    pub(crate) fn recording_active(&self) -> bool {
        !self.store.recording.is_empty()
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

    /// The show's frame rate, which reel clips and trims are measured in.
    pub(crate) const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    /// Reads the recorded media through [`ReelMedia`] without exposing the
    /// store's private bookkeeping (OR-013).
    pub(crate) fn with_media<R>(&self, read: impl FnOnce(ReelMedia<'_>) -> R) -> R {
        read(ReelMedia {
            events: &self.store.events,
            timeline: &self.store.timeline,
            catalog: &self.store.catalog,
            segments: &self.store.segments,
        })
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn touch(&mut self) {
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
        time_of(self.frame_rate, frame)
    }

    fn range(&self, first: u64, end: u64) -> Option<TimelineRange> {
        range_of(self.frame_rate, first, end)
    }

    /// The store, copied first if a clone of the engine shares it.
    fn store_mut(&mut self) -> &mut Store {
        Arc::make_mut(&mut self.store)
    }

    /// Drops every recorded segment and event and starts recording
    /// `settings` anew.
    fn restart(&mut self, settings: ReplaySettings) {
        self.pruned.extend(
            self.store
                .segments
                .keys()
                .filter_map(|id| self.store.catalog.segment(*id))
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
        self.store = Arc::new(Store {
            catalog,
            timeline: SourceTimeline::new(&sources),
            segments: BTreeMap::new(),
            recording: settings
                .sources
                .iter()
                .map(|input| (*input, (None, 0)))
                .collect(),
            last_frames: BTreeMap::new(),
            events: EventDatabase::default(),
            event_frames: BTreeMap::new(),
        });
        self.mark_in = None;
        self.mark_out = None;
        for (job, _) in std::mem::take(&mut self.deferred_exports) {
            if let Some(meta) = self.export_meta.get_mut(&job)
                && meta.outcome.is_none()
            {
                meta.outcome = Some(ReelExportOutcome::Failed {
                    reason: "the replay recording restarted".to_owned(),
                });
            }
        }
        for player in self.players.values_mut() {
            *player = Player {
                speed: player.speed,
                linked: player.linked,
                ..Player::default()
            };
        }
        self.fail_restarted_exports();
        self.settings = settings;
        self.touch();
    }

    /// Fails every active export whose media a restart dropped, releasing
    /// whatever protection the new catalog still holds.
    fn fail_restarted_exports(&mut self) {
        if self.export_meta.is_empty() {
            return;
        }
        let failures = {
            let Self { store, exports, .. } = self;
            let store = Arc::make_mut(store);
            exports.revalidate(&mut store.catalog, &store.timeline)
        };
        for (job, failure) in failures {
            if let Some(meta) = self.export_meta.get_mut(&crate::reel::export_number(job))
                && meta.outcome.is_none()
            {
                meta.outcome = Some(ReelExportOutcome::Failed {
                    reason: failure.to_string(),
                });
            }
        }
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

    /// Records the free bytes on the storage root, as the daemon sampled
    /// them, and announces the change (OR-014).
    pub(crate) fn set_storage_free(&mut self, free: Option<u64>) {
        if self.storage_free == free {
            return;
        }
        self.storage_free = free;
        self.touch();
    }

    /// Records the root drive's SMART status, as the daemon sampled it, and
    /// announces the change (OR-014, 2.139).
    pub(crate) fn set_storage_smart(&mut self, smart: Option<EngineReplaySmartStatus>) {
        if self.storage_smart == smart {
            return;
        }
        self.storage_smart = smart;
        self.touch();
    }

    /// Free space and the observed write rate of the storage root, when the
    /// daemon has sampled it.
    fn storage_status(&self) -> Option<EngineReplayStorageStatus> {
        let free_bytes = self.storage_free?;
        let mut bytes = 0_u64;
        let mut frames: BTreeMap<InputId, u64> = BTreeMap::new();
        for (id, segment) in &self.store.segments {
            let Some(metadata) = self.store.catalog.segment(*id) else {
                continue;
            };
            if metadata.bytes == 0 {
                continue;
            }
            bytes = bytes.saturating_add(metadata.bytes);
            let entry = frames.entry(segment.input).or_default();
            *entry = entry.saturating_add(segment.frames);
        }
        let longest = frames.values().copied().max().unwrap_or(0);
        let rate = if longest == 0 {
            0
        } else {
            let numerator =
                u128::from(bytes).saturating_mul(u128::from(self.frame_rate.numerator()));
            let denominator =
                u128::from(longest).saturating_mul(u128::from(self.frame_rate.denominator()));
            u64::try_from(numerator / denominator.max(1)).unwrap_or(u64::MAX)
        };
        let remaining_seconds = (rate > 0).then(|| free_bytes / rate);
        let level = match remaining_seconds {
            _ if free_bytes == 0 => EngineReplayStorageLevel::Full,
            Some(seconds) if seconds < REPLAY_STORAGE_CRITICAL_SECONDS => {
                EngineReplayStorageLevel::Critical
            }
            Some(seconds) if seconds < REPLAY_STORAGE_WARNING_SECONDS => {
                EngineReplayStorageLevel::Warning
            }
            _ => EngineReplayStorageLevel::Ok,
        };
        Some(EngineReplayStorageStatus {
            free_bytes,
            bytes_per_second: rate,
            remaining_seconds,
            level,
            smart: self.storage_smart,
        })
    }

    /// Records frame `frame` of source `input` as stream frame
    /// `stream_frame` of its recording.
    pub(crate) fn observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool {
        if !self.store.recording.contains_key(&input) {
            return false;
        }
        let at = self.at(frame);
        let store = self.store_mut();
        let observed = store
            .timeline
            .observe(
                camera(input),
                at,
                SequenceNumber::new(frame),
                FrameIndex::new(stream_frame),
            )
            .is_ok();
        if observed {
            store.last_frames.insert(input, frame);
        }
        observed
    }

    /// Catalogs a finished segment file of source `input`: `frames` stream
    /// frames from `first_stream_frame`, the first recorded on engine frame
    /// `first_frame`.
    #[allow(clippy::too_many_arguments)]
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
        if !self.store.recording.contains_key(&input) || frames == 0 {
            return false;
        }
        let Some(timeline) = self.range(first_frame, first_frame + frames) else {
            return false;
        };
        let id = SegmentId::new(NonZeroU128::new(self.next_segment).unwrap_or(NonZeroU128::MIN));
        let inserted = self
            .store_mut()
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
            self.store_mut().segments.insert(
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
        let rate = self.frame_rate;
        let store = self.store_mut();
        for (id, event) in &store.event_frames {
            let Some(range) = range_of(rate, event.in_frame, event.out_frame) else {
                continue;
            };
            let reference = ProtectionReference::Event(EventId::new(
                NonZeroU128::new(u128::from(*id)).unwrap_or(NonZeroU128::MIN),
            ));
            for angle in &event.angles {
                let _ = store
                    .catalog
                    .protect_range(camera(*angle), range, reference);
            }
        }
    }

    /// The synthetic recording of frame `frame`: every source is observed
    /// and its open segment grows, closing after
    /// [`SYNTHETIC_REPLAY_SEGMENT_SECONDS`].
    fn record_synthetic(&mut self, frame: u64) {
        let per_segment = self.frames_in(SYNTHETIC_REPLAY_SEGMENT_SECONDS).max(1);
        let sources: Vec<InputId> = self.store.recording.keys().copied().collect();
        for input in sources {
            let (open, stream_frame) = self.store.recording[&input];
            if !self.observe(input, frame, stream_frame) {
                continue;
            }
            let id = if let Some(id) = open {
                let end = self.at(frame + 1);
                let store = self.store_mut();
                let _ = store.catalog.extend_open_segment(id, end);
                if let Some(segment) = store.segments.get_mut(&id) {
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
                    .store_mut()
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
                self.store_mut().segments.insert(
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
            let frames = self
                .store
                .segments
                .get(&id)
                .map_or(0, |segment| segment.frames);
            let open = if frames >= per_segment {
                let end = self.at(frame + 1);
                let _ = self.store_mut().catalog.close_segment(
                    id,
                    end,
                    frames * SYNTHETIC_REPLAY_FRAME_BYTES,
                );
                None
            } else {
                Some(id)
            };
            self.store_mut()
                .recording
                .insert(input, (open, stream_frame + 1));
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
        // Only a store with something to drop is changed (and so copied
        // away from the clones that share it).
        let droppable = self.store.segments.iter().any(|(id, segment)| {
            segment.first_frame + segment.frames <= cutoff_frame
                && self.store.catalog.segment(*id).is_some_and(|metadata| {
                    metadata.state == SegmentState::Closed && metadata.protections.is_empty()
                })
        });
        if !droppable {
            return;
        }
        let cutoff = self.at(cutoff_frame);
        let Ok(dropped) = self.store_mut().catalog.reclaim_ended_before(ROOT, cutoff) else {
            return;
        };
        if dropped.is_empty() {
            return;
        }
        for segment in dropped {
            self.store_mut().segments.remove(&segment.id);
            if self.feed == EngineReplayFeed::External {
                self.pruned.push(segment.path);
            }
        }
        // Observations an event still needs stay, and so do those an active
        // reel export reads (its clips may start before their event's mark).
        let oldest_event = self
            .store
            .event_frames
            .values()
            .map(|event| event.in_frame)
            .min()
            .unwrap_or(u64::MAX);
        let oldest_export = self
            .export_meta
            .keys()
            .filter(|job| self.export_active(**job))
            .filter_map(|job| self.exports.job(crate::reel::export_id(*job)))
            .flat_map(|job| job.reel().clips())
            .chain(
                self.deferred_exports
                    .values()
                    .flat_map(|plan| plan.compiled.clips()),
            )
            .map(|clip| frame_of(self.frame_rate, clip.source.start))
            .min()
            .unwrap_or(u64::MAX);
        let prune = self.at(cutoff_frame.min(oldest_event).min(oldest_export));
        let _ = self.store_mut().timeline.prune_before(prune);
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
        while self.store.event_frames.len() >= MAX_REPLAY_EVENTS {
            let Some((&oldest, _)) = self.store.event_frames.iter().next() else {
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
        if self.store_mut().events.insert_event(event).is_err() {
            return;
        }
        let angles = self.settings.sources.clone();
        self.store_mut().event_frames.insert(
            id,
            Event {
                in_frame,
                out_frame,
                angles,
            },
        );
        self.protect_events();
        self.touch();
    }

    fn delete(&mut self, id: u32) {
        let store = self.store_mut();
        if store.event_frames.remove(&id).is_none() {
            return;
        }
        let _ = store
            .events
            .remove_event(Self::event_id(id), &mut store.catalog);
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
    #[allow(clippy::too_many_lines)] // one arm per replay operation
    pub(crate) fn realize(
        &mut self,
        show: &ShowState,
        operation: &EngineReplayOperation,
        frame: u64,
    ) {
        self.follow(show.replay());
        self.rearm(operation);
        match operation {
            // A return is realized as a take by its plan's own action.
            EngineReplayOperation::Configure(_) | EngineReplayOperation::Return { .. } => {}
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
                if self.store.event_frames.contains_key(event)
                    && let Some(stored) = self.store.events.event(Self::event_id(*event)).cloned()
                {
                    let store = self.store_mut();
                    let _ = store
                        .events
                        .remove_event(Self::event_id(*event), &mut store.catalog);
                    let _ = store.events.insert_event(ReplayEvent {
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
                if let Some(stored_angles) = self
                    .store
                    .event_frames
                    .get(event)
                    .map(|stored| stored.angles.clone())
                {
                    for target in self.ganged(*input) {
                        let player = self.players.entry(target).or_default();
                        player.event = Some(*event);
                        // The cue's angle applies to the named input; a
                        // linked partner keeps its own angle when the event
                        // holds it, so A and B show different angles.
                        player.angle = if target == *input {
                            angle
                                .filter(|angle| stored_angles.contains(angle))
                                .or_else(|| stored_angles.first().copied())
                        } else {
                            player
                                .angle
                                .filter(|angle| stored_angles.contains(angle))
                                .or_else(|| stored_angles.first().copied())
                        };
                        player.playing = false;
                        player.shuttling = false;
                        player.rate_milli = 0;
                        player.shuttle_remainder = 0;
                        player.position = 0;
                        player.playlist_index = player
                            .playlist
                            .iter()
                            .position(|entry| entry.event == *event);
                    }
                    self.touch();
                }
            }
            EngineReplayOperation::Angle { input, angle } => {
                if let Some(player) = self.players.get_mut(input)
                    && let Some(event) =
                        player.event.and_then(|id| self.store.event_frames.get(&id))
                    && event.angles.contains(angle)
                {
                    player.angle = Some(*angle);
                    self.generation = self.generation.wrapping_add(1);
                }
            }
            EngineReplayOperation::Play { input, speed } => {
                for target in self.ganged(*input) {
                    let player = self.players.entry(target).or_default();
                    player.speed = *speed;
                    player.rate_milli = 0;
                    player.shuttling = false;
                    player.shuttle_remainder = 0;
                    if player.event.is_some() {
                        player.playing = true;
                    }
                }
                self.touch();
            }
            EngineReplayOperation::Shuttle { input, rate_milli } => {
                for target in self.ganged(*input) {
                    let player = self.players.entry(target).or_default();
                    player.rate_milli = *rate_milli;
                    player.shuttling = true;
                    player.shuttle_remainder = 0;
                    player.playing = player.event.is_some() && *rate_milli != 0;
                }
                self.touch();
            }
            EngineReplayOperation::Pause { input } => {
                for target in self.ganged(*input) {
                    if let Some(player) = self.players.get_mut(&target) {
                        player.playing = false;
                        player.shuttling = false;
                        player.rate_milli = 0;
                        player.shuttle_remainder = 0;
                    }
                }
                self.generation = self.generation.wrapping_add(1);
            }
            EngineReplayOperation::CueIn { input } => {
                for target in self.ganged(*input) {
                    if let Some(player) = self.players.get_mut(&target) {
                        player.playing = false;
                        player.shuttling = false;
                        player.rate_milli = 0;
                        player.shuttle_remainder = 0;
                        player.position = 0;
                    }
                }
                self.generation = self.generation.wrapping_add(1);
            }
            EngineReplayOperation::Step { input, frames } => {
                for target in self.ganged(*input) {
                    self.jog(target, *frames);
                }
            }
            EngineReplayOperation::Link { input, linked } => self.link(*input, *linked),
            EngineReplayOperation::PlaylistSet { input, entries } => {
                let player = self.players.entry(*input).or_default();
                player.playlist.clone_from(entries);
                player.playlist_index = player.event.and_then(|event| {
                    player
                        .playlist
                        .iter()
                        .position(|entry| entry.event == event)
                });
                self.touch();
            }
            EngineReplayOperation::PlaylistNext { input } => {
                let events = &self.store.event_frames;
                if let Some(player) = self.players.get_mut(input) {
                    player.move_playlist(events, true);
                }
                self.generation = self.generation.wrapping_add(1);
            }
            EngineReplayOperation::PlaylistPrev { input } => {
                let events = &self.store.event_frames;
                if let Some(player) = self.players.get_mut(input) {
                    player.move_playlist(events, false);
                }
                self.generation = self.generation.wrapping_add(1);
            }
        }
    }

    /// A player cued, played, or moved again has to reach its end again
    /// before it returns (auto return).
    fn rearm(&mut self, operation: &EngineReplayOperation) {
        if let EngineReplayOperation::Cue { input, .. }
        | EngineReplayOperation::Play { input, .. }
        | EngineReplayOperation::Shuttle { input, .. }
        | EngineReplayOperation::CueIn { input }
        | EngineReplayOperation::Step { input, .. }
        | EngineReplayOperation::PlaylistNext { input }
        | EngineReplayOperation::PlaylistPrev { input } = operation
            && let Some(player) = self.players.get_mut(input)
        {
            player.ended_on_air = false;
        }
    }

    /// The auto return due on `show`, if any: the Replay input on Program
    /// that played its event to the end there, the source to go back to,
    /// and the mix's frames.
    pub(crate) fn return_due(&self, show: &ShowState) -> Option<(InputId, InputId, u32)> {
        let frames = show.replay().auto_return?;
        let live = self.live_program?;
        let program = show.desired_switcher().program();
        let player = self.players.get(&program)?;
        (player.ended_on_air && live != program).then_some((program, live, frames))
    }

    /// Moves Replay input `input` `frames` source frames (back when
    /// negative), paused, clamped to its event's first and last frames.
    fn jog(&mut self, input: InputId, frames: i32) {
        let Some(player) = self.players.get_mut(&input) else {
            return;
        };
        let Some(event) = player.event.and_then(|id| self.store.event_frames.get(&id)) else {
            return;
        };
        let last = (event.out_frame - event.in_frame).saturating_sub(1);
        let current = player.position / 4;
        let distance = u64::from(frames.unsigned_abs());
        let target = if frames < 0 {
            current.saturating_sub(distance)
        } else {
            current.saturating_add(distance)
        };
        player.position = target.min(last) * 4;
        player.playing = false;
        player.shuttling = false;
        player.rate_milli = 0;
        player.shuttle_remainder = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    /// `input` and the Replay input it plays in step with (A/B), when it is
    /// linked.
    fn ganged(&self, input: InputId) -> Vec<InputId> {
        match self.players.get(&input).and_then(|player| player.linked) {
            Some(partner) if partner != input && self.players.contains_key(&partner) => {
                vec![input, partner]
            }
            _ => vec![input],
        }
    }

    /// Links `input` to `linked` for A/B playback, or clears the link with
    /// `linked: None` (Protocol 2.124). Linking cues the partner on the
    /// named player's event at its position, speed, and playing state while
    /// keeping the partner's angle when the event holds it; any previous
    /// partner on either side is unlinked.
    fn link(&mut self, input: InputId, linked: Option<InputId>) {
        if let Some(previous) = self.players.get(&input).and_then(|player| player.linked)
            && let Some(player) = self.players.get_mut(&previous)
            && player.linked == Some(input)
        {
            player.linked = None;
        }
        let Some(linked) = linked else {
            self.players.entry(input).or_default().linked = None;
            self.touch();
            return;
        };
        if let Some(previous) = self.players.get(&linked).and_then(|player| player.linked)
            && previous != input
            && let Some(player) = self.players.get_mut(&previous)
            && player.linked == Some(linked)
        {
            player.linked = None;
        }
        let source = self.players.get(&input).cloned().unwrap_or_default();
        if let Some(event) = source.event
            && let Some(angles) = self
                .store
                .event_frames
                .get(&event)
                .map(|stored| stored.angles.clone())
        {
            let target = self.players.entry(linked).or_default();
            target.event = Some(event);
            target.angle = target
                .angle
                .filter(|angle| angles.contains(angle))
                .or_else(|| source.angle.filter(|angle| angles.contains(angle)))
                .or_else(|| angles.first().copied());
            target.position = source.position;
            target.speed = source.speed;
            target.rate_milli = source.rate_milli;
            target.shuttling = source.shuttling;
            target.shuttle_remainder = source.shuttle_remainder;
            target.playing = source.playing;
            target.playlist_index = target
                .playlist
                .iter()
                .position(|entry| entry.event == event);
        }
        self.players.entry(input).or_default().linked = Some(linked);
        self.players.entry(linked).or_default().linked = Some(input);
        for id in [input, linked] {
            if let Some(player) = self.players.get_mut(&id) {
                player.ended_on_air = false;
            }
        }
        self.touch();
    }

    /// Where frame `frame` of source `input` was recorded.
    fn locate(&self, input: InputId, frame: u64) -> Option<EngineReplayLocation> {
        let at = self.at(frame);
        let observation = self.store.timeline.resolve(camera(input), at).ok()?;
        let metadata = self.store.catalog.segment_at(camera(input), at)?;
        if metadata.state != SegmentState::Closed {
            return None;
        }
        let segment = self.store.segments.get(&metadata.id)?;
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
        self.start_deferred_exports();
        let inputs: BTreeSet<InputId> = show.replay_inputs().collect();
        if self.players.keys().any(|input| !inputs.contains(input)) {
            self.players.retain(|input, _| inputs.contains(input));
            for player in self.players.values_mut() {
                if player
                    .linked
                    .is_some_and(|linked| !inputs.contains(&linked))
                {
                    player.linked = None;
                }
            }
            self.touch();
        }
        let program = show.desired_switcher().program();
        if !inputs.contains(&program) {
            self.live_program = Some(program);
        }
        let mut frames = Vec::with_capacity(inputs.len());
        let mut changed = false;
        let mut ended = Vec::new();
        for input in inputs {
            if input != program
                && let Some(player) = self.players.get_mut(&input)
                && player.ended_on_air
            {
                player.ended_on_air = false;
                self.generation = self.generation.wrapping_add(1);
            }
            let player = self.players.entry(input).or_default().clone();
            let event = player.event.and_then(|id| self.store.event_frames.get(&id));
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
                rate_milli: player.rate_milli,
                shuttling: player.shuttling,
                playing: player.playing,
            });
            if player.playing
                && let Some(event) = event
            {
                let length = event.out_frame - event.in_frame;
                let delta = if player.shuttling {
                    let total =
                        i64::from(player.rate_milli) * 4 + i64::from(player.shuttle_remainder);
                    let remainder = i32::try_from(total % 1_000).unwrap_or(0);
                    let stored = self.players.entry(input).or_default();
                    stored.shuttle_remainder = remainder;
                    total / 1_000
                } else {
                    i64::try_from(player.speed.quarter_frames()).unwrap_or(i64::MAX)
                };
                let current = i64::try_from(player.position).unwrap_or(i64::MAX);
                let next = current.saturating_add(delta);
                let last =
                    i64::try_from(length.saturating_sub(1).saturating_mul(4)).unwrap_or(i64::MAX);
                let stored = self.players.entry(input).or_default();
                if next < 0 {
                    stored.position = 0;
                    stored.playing = false;
                    stored.shuttling = false;
                    stored.rate_milli = 0;
                    stored.shuttle_remainder = 0;
                    changed = true;
                } else if next > last {
                    stored.position = u64::try_from(last).unwrap_or(0);
                    stored.playing = false;
                    stored.shuttling = false;
                    stored.rate_milli = 0;
                    stored.shuttle_remainder = 0;
                    if !player.shuttling {
                        // The playlist's next entry (Protocol 2.119) takes
                        // over below; a linked pair moves on only together.
                        ended.push(input);
                    }
                    changed = true;
                } else {
                    stored.position = u64::try_from(next).unwrap_or(0);
                }
            }
        }
        changed |= self.settle_ended_players(ended, program);
        if changed {
            self.touch();
        }
        frames
    }

    /// Applies a playlist's next entry, or a stop, to every player that
    /// reached its event's end this frame. Linked players move on only when
    /// both have a next entry, so A/B stays in step; `true` when anything
    /// changed.
    fn settle_ended_players(&mut self, ended: Vec<InputId>, program: InputId) -> bool {
        let mut changed = false;
        // A player that reached its event's end either moves on to its
        // playlist's next entry or stops. Linked players (Protocol 2.124)
        // only move on when both have a next entry, so A/B stays in step.
        let ended_set: BTreeSet<InputId> = ended.iter().copied().collect();
        let mut handled = BTreeSet::new();
        for input in ended {
            if handled.contains(&input) {
                continue;
            }
            let partner = self
                .players
                .get(&input)
                .and_then(|player| player.linked)
                .filter(|partner| ended_set.contains(partner));
            if let Some(partner) = partner {
                handled.insert(input);
                handled.insert(partner);
                let both_advance = {
                    let events = &self.store.event_frames;
                    [input, partner].iter().all(|id| {
                        self.players
                            .get(id)
                            .and_then(|player| player.next_playlist_entry(events))
                            .is_some()
                    })
                };
                if both_advance {
                    for id in [input, partner] {
                        let events = &self.store.event_frames;
                        let next = self
                            .players
                            .get(&id)
                            .and_then(|player| player.next_playlist_entry(events));
                        if let Some((index, entry)) = next
                            && let Some(event) = events.get(&entry.event)
                        {
                            let player = self.players.get_mut(&id).expect("the player was checked");
                            player.cue_entry(index, entry, event);
                            player.playing = true;
                        }
                    }
                } else {
                    for id in [input, partner] {
                        if let Some(player) = self.players.get_mut(&id) {
                            player.playing = false;
                            player.ended_on_air = id == program;
                        }
                    }
                }
                changed = true;
                continue;
            }
            handled.insert(input);
            let events = &self.store.event_frames;
            let next = self
                .players
                .get(&input)
                .and_then(|player| player.next_playlist_entry(events));
            if let Some((index, entry)) = next
                && let Some(event) = events.get(&entry.event)
            {
                let player = self
                    .players
                    .get_mut(&input)
                    .expect("the player was checked");
                player.cue_entry(index, entry, event);
                player.playing = true;
            } else if let Some(player) = self.players.get_mut(&input) {
                player.playing = false;
                player.ended_on_air = input == program;
            }
            changed = true;
        }
        changed
    }

    /// Takes the files retention or a restart dropped.
    pub(crate) fn take_pruned(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pruned)
    }

    /// Plans one reel export against the show and the recorded media: the
    /// reel exists, every event it names is completely recorded, the
    /// destination is usable and new, the job id is unused, and no other
    /// export is active.
    pub(crate) fn plan_export(
        &self,
        show: &ShowState,
        job: u64,
        reel: u64,
        destination: &str,
    ) -> Result<ReelExportPlan, Rejection> {
        crate::reel::check_job(job, self.export_meta.contains_key(&job))?;
        crate::reel::check_no_active_export(
            self.export_meta
                .keys()
                .copied()
                .find(|job| self.export_active(*job)),
        )?;
        crate::reel::check_export_reel(show, reel)?;
        crate::reel::check_destination(destination)?;
        let spec = show.reel(reel).expect("the reel was checked");
        let compiled = crate::reel::compile(self, spec)
            .map_err(|error| crate::reel::plan_rejection(&error))?;
        if self.export_meta.len() >= MAX_RECORD_EXPORT_JOBS {
            return Err(Rejection::new(
                RejectionCode::Conflict,
                format!(
                    "the reel export queue already holds {MAX_RECORD_EXPORT_JOBS} jobs; forget a \
                     finished one first"
                ),
            )
            .with_field(FieldIssue::new(
                "job",
                "too_many_exports",
                format!("the reel export queue holds at most {MAX_RECORD_EXPORT_JOBS} jobs"),
            )));
        }
        match self.with_media(|media| media.export_ready(&compiled)) {
            // Open segments close as the recording advances; the export
            // starts then instead of being refused (OR-013).
            Ok(()) | Err(RecordExportError::SegmentNotFinalized { .. }) => {}
            Err(error) => return Err(crate::reel::export_rejection(&error)),
        }
        Ok(ReelExportPlan {
            job,
            reel,
            destination: destination.to_owned(),
            compiled,
            music: spec.music.clone(),
        })
    }

    /// Submits a planned export, protecting every segment it reads from
    /// retention. Returns whether it was queued; a plan accepted by
    /// [`Self::plan_export`] always is.
    pub(crate) fn submit_export(&mut self, plan: ReelExportPlan) -> bool {
        let submitted = self.try_submit_export(&plan);
        if submitted.is_none() {
            return false;
        }
        self.export_meta.insert(
            plan.job,
            ReelExportMeta {
                reel: plan.reel,
                destination: plan.destination.clone(),
                total_frames: plan.compiled.total_frames(),
                music: plan.music.clone(),
                outcome: None,
            },
        );
        if submitted == Some(false) {
            self.deferred_exports.insert(plan.job, plan);
        }
        self.touch();
        true
    }

    /// Submits `plan` to the export queue: `Some(true)` when queued,
    /// `Some(false)` while its media is still open, and `None` when the
    /// queue refused it for another reason.
    fn try_submit_export(&mut self, plan: &ReelExportPlan) -> Option<bool> {
        let Self { store, exports, .. } = self;
        let store = Arc::make_mut(store);
        match exports.submit(
            crate::reel::export_id(plan.job),
            &plan.compiled,
            &mut store.catalog,
            &store.timeline,
        ) {
            Ok(_) => Some(true),
            Err(RecordExportError::SegmentNotFinalized { .. }) => Some(false),
            Err(_) => None,
        }
    }

    /// Starts every deferred export whose media is now finalized. A plan
    /// whose media was pruned or otherwise became unusable fails its job.
    fn start_deferred_exports(&mut self) {
        if self.deferred_exports.is_empty() {
            return;
        }
        let jobs: Vec<u64> = self.deferred_exports.keys().copied().collect();
        for job in jobs {
            let Some(plan) = self.deferred_exports.get(&job).cloned() else {
                continue;
            };
            match self.try_submit_export(&plan) {
                Some(true) => {
                    self.deferred_exports.remove(&job);
                    self.touch();
                }
                Some(false) => {}
                None => {
                    self.deferred_exports.remove(&job);
                    if let Some(meta) = self.export_meta.get_mut(&job)
                        && meta.outcome.is_none()
                    {
                        meta.outcome = Some(ReelExportOutcome::Failed {
                            reason: "export media became unavailable".to_owned(),
                        });
                    }
                }
            }
        }
    }

    /// Resolves up to `max_frames` further output frames of export `job`.
    pub(crate) fn advance_export(
        &mut self,
        job: u64,
        max_frames: u64,
    ) -> Result<EngineReelChunk, EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        if meta.outcome.is_some() || self.deferred_exports.contains_key(&job) {
            return Err(EngineReelError::ExportNotActive(job));
        }
        let chunk = {
            let Self { store, exports, .. } = self;
            let store = Arc::make_mut(store);
            exports.advance(
                crate::reel::export_id(job),
                max_frames,
                &mut store.catalog,
                &store.timeline,
            )
        };
        self.touch();
        let chunk = chunk.map_err(EngineReelError::Export)?;
        crate::reel::resolve_chunk(self, chunk)
    }

    /// Records that the media plane wrote export `job`'s file, `bytes`
    /// long. The export must have resolved every output frame.
    pub(crate) fn export_finished(&mut self, job: u64, bytes: u64) -> Result<(), EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        if meta.outcome.is_some() {
            return Err(EngineReelError::ExportNotActive(job));
        }
        if self.export_state(job) != Some(RecordExportState::Completed) {
            return Err(EngineReelError::ExportNotFinalizing(job));
        }
        self.export_meta
            .get_mut(&job)
            .expect("the export was checked")
            .outcome = Some(ReelExportOutcome::Finished { bytes });
        self.touch();
        Ok(())
    }

    /// Records that the media plane could not write export `job`'s file,
    /// releasing its retention protection.
    pub(crate) fn export_failed(
        &mut self,
        job: u64,
        reason: String,
    ) -> Result<(), EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        if meta.outcome.is_some() {
            return Err(EngineReelError::ExportNotActive(job));
        }
        if self.deferred_exports.remove(&job).is_some() {
            self.export_meta
                .get_mut(&job)
                .expect("the export was checked")
                .outcome = Some(ReelExportOutcome::Failed { reason });
            self.touch();
            return Ok(());
        }
        match self.export_state(job) {
            Some(RecordExportState::Queued | RecordExportState::Running) => {
                let store = Arc::make_mut(&mut self.store);
                let _ = self
                    .exports
                    .cancel(crate::reel::export_id(job), &mut store.catalog);
            }
            Some(RecordExportState::Completed) => {}
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) | None => {
                return Err(EngineReelError::ExportNotActive(job));
            }
        }
        self.export_meta
            .get_mut(&job)
            .expect("the export was checked")
            .outcome = Some(ReelExportOutcome::Failed { reason });
        self.touch();
        Ok(())
    }

    /// Whether export `job` can be cancelled: it exists, is not terminal,
    /// and its file is not finished.
    pub(crate) fn check_cancel_export(&self, job: u64) -> Result<(), EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        if meta.outcome.is_some() {
            return Err(EngineReelError::ExportNotActive(job));
        }
        match self.export_state(job) {
            Some(
                RecordExportState::Queued
                | RecordExportState::Running
                | RecordExportState::Completed,
            ) => Ok(()),
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) | None => {
                Err(EngineReelError::ExportNotActive(job))
            }
        }
    }

    /// Whether terminal export `job` can be forgotten.
    pub(crate) fn check_forget_export(&self, job: u64) -> Result<(), EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        // A deferred export never entered the queue; once its outcome is
        // recorded there is nothing left to forget there.
        let terminal = match self.export_state(job) {
            None | Some(RecordExportState::Completed) => meta.outcome.is_some(),
            Some(RecordExportState::Queued | RecordExportState::Running) => false,
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) => true,
        };
        if terminal {
            Ok(())
        } else {
            Err(EngineReelError::ExportStillActive(job))
        }
    }

    /// Cancels export `job`, releasing its retention protection. A
    /// finalizing export is cancelled without touching the media plane's
    /// file write.
    pub(crate) fn cancel_export(&mut self, job: u64) -> Result<(), EngineReelError> {
        let meta = self
            .export_meta
            .get(&job)
            .ok_or(EngineReelError::UnknownExport(job))?;
        if meta.outcome.is_some() {
            return Err(EngineReelError::ExportNotActive(job));
        }
        if self.deferred_exports.remove(&job).is_some() {
            if let Some(meta) = self.export_meta.get_mut(&job) {
                meta.outcome = Some(ReelExportOutcome::Cancelled);
            }
            self.touch();
            return Ok(());
        }
        match self.export_state(job) {
            Some(RecordExportState::Queued | RecordExportState::Running) => {
                let store = Arc::make_mut(&mut self.store);
                self.exports
                    .cancel(crate::reel::export_id(job), &mut store.catalog)
                    .map_err(EngineReelError::Export)?;
            }
            Some(RecordExportState::Completed) => {
                self.export_meta
                    .get_mut(&job)
                    .expect("the export was checked")
                    .outcome = Some(ReelExportOutcome::Cancelled);
            }
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) | None => {
                return Err(EngineReelError::ExportNotActive(job));
            }
        }
        self.touch();
        Ok(())
    }

    /// Drops terminal export `job` so its id can be reused.
    pub(crate) fn forget_export(&mut self, job: u64) -> Result<(), EngineReelError> {
        let Some(meta) = self.export_meta.get(&job) else {
            return Err(EngineReelError::UnknownExport(job));
        };
        let terminal = match self.export_state(job) {
            None | Some(RecordExportState::Completed) => meta.outcome.is_some(),
            Some(RecordExportState::Queued | RecordExportState::Running) => false,
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) => true,
        };
        if !terminal {
            return Err(EngineReelError::ExportStillActive(job));
        }
        if self.exports.job(crate::reel::export_id(job)).is_some() {
            self.exports
                .forget(crate::reel::export_id(job))
                .map_err(EngineReelError::Export)?;
        }
        self.export_meta.remove(&job);
        self.touch();
        Ok(())
    }

    /// The planner's state of export `job`.
    fn export_state(&self, job: u64) -> Option<RecordExportState> {
        if self.deferred_exports.contains_key(&job) {
            return Some(RecordExportState::Queued);
        }
        self.exports
            .job(crate::reel::export_id(job))
            .map(|job| job.state().clone())
    }

    /// Whether export `job` still holds its slot: a queued or running job,
    /// or one whose frames are resolved but whose file is still being
    /// written.
    fn export_active(&self, job: u64) -> bool {
        let Some(meta) = self.export_meta.get(&job) else {
            return false;
        };
        match self.export_state(job) {
            Some(RecordExportState::Queued | RecordExportState::Running) => true,
            Some(RecordExportState::Completed) => meta.outcome.is_none(),
            Some(RecordExportState::Failed(_) | RecordExportState::Cancelled) | None => false,
        }
    }

    /// One export as clients see it.
    fn export_status(&self, job: u64, meta: &ReelExportMeta) -> Option<EngineReelExportStatus> {
        if self.deferred_exports.contains_key(&job) {
            return Some(EngineReelExportStatus {
                job,
                reel: meta.reel,
                destination: meta.destination.clone(),
                state: EngineReelExportState::Deferred,
                completed_frames: 0,
                total_frames: meta.total_frames,
                bytes: None,
                failure: None,
                music: meta.music.clone(),
            });
        }
        let Some(entry) = self.exports.job(crate::reel::export_id(job)) else {
            // A deferred job that failed or was cancelled never entered the
            // queue; its recorded outcome is all that is left to report.
            let (state, bytes, failure) = match &meta.outcome {
                Some(ReelExportOutcome::Finished { bytes }) => {
                    (EngineReelExportState::Completed, Some(*bytes), None)
                }
                Some(ReelExportOutcome::Failed { reason }) => {
                    (EngineReelExportState::Failed, None, Some(reason.clone()))
                }
                Some(ReelExportOutcome::Cancelled) => {
                    (EngineReelExportState::Cancelled, None, None)
                }
                None => return None,
            };
            return Some(EngineReelExportStatus {
                job,
                reel: meta.reel,
                destination: meta.destination.clone(),
                state,
                completed_frames: 0,
                total_frames: meta.total_frames,
                bytes,
                failure,
                music: meta.music.clone(),
            });
        };
        let progress = entry.progress();
        let (state, bytes, failure) = match &meta.outcome {
            Some(ReelExportOutcome::Finished { bytes }) => {
                (EngineReelExportState::Completed, Some(*bytes), None)
            }
            Some(ReelExportOutcome::Failed { reason }) => {
                (EngineReelExportState::Failed, None, Some(reason.clone()))
            }
            Some(ReelExportOutcome::Cancelled) => (EngineReelExportState::Cancelled, None, None),
            None => match entry.state() {
                RecordExportState::Queued => (EngineReelExportState::Queued, None, None),
                RecordExportState::Running => (EngineReelExportState::Running, None, None),
                RecordExportState::Completed => (EngineReelExportState::Finalizing, None, None),
                RecordExportState::Failed(failure) => (
                    EngineReelExportState::Failed,
                    None,
                    Some(failure.to_string()),
                ),
                RecordExportState::Cancelled => (EngineReelExportState::Cancelled, None, None),
            },
        };
        Some(EngineReelExportStatus {
            job,
            reel: meta.reel,
            destination: meta.destination.clone(),
            state,
            completed_frames: progress.completed_frames,
            total_frames: progress.total_frames,
            bytes,
            failure,
            music: meta.music.clone(),
        })
    }

    /// The reels and their exports as clients see them (OR-013).
    pub(crate) fn reel_status(&self, show: &ShowState) -> EngineReelStatus {
        let reels = show
            .reels()
            .iter()
            .map(|spec| {
                let (total_frames, error) = match crate::reel::compile(self, spec) {
                    Ok(compiled) => (
                        Some(compiled.total_frames()),
                        self.with_media(|media| media.export_ready(&compiled))
                            .err()
                            .map(|error| error.to_string()),
                    ),
                    Err(error) => (None, Some(error.to_string())),
                };
                EngineReelSummary {
                    id: spec.id,
                    name: spec.name.clone(),
                    clips: u32::try_from(spec.clips.len()).unwrap_or(u32::MAX),
                    total_frames,
                    error,
                }
            })
            .collect();
        let exports = self
            .export_meta
            .iter()
            .filter_map(|(job, meta)| self.export_status(*job, meta))
            .collect();
        EngineReelStatus { reels, exports }
    }

    /// The up-to-eight-angle view of Replay input `input` at its player's
    /// current position, with the player and its linked A/B partner as the
    /// first channel tiles, or `None` when nothing is cued on it (OR-014).
    pub(crate) fn multiview(&self, input: InputId) -> Option<EngineReplayMultiview> {
        let player = self.players.get(&input)?;
        let linked = player.linked;
        let event = player
            .event
            .and_then(|id| self.store.event_frames.get(&id).map(|stored| (id, stored)))?;
        let (id, stored) = event;
        let source_frame =
            (stored.in_frame + player.position / 4).min(stored.out_frame.saturating_sub(1));
        let mut channels: Vec<EngineReplayTile> = self.channel_tile(input).into_iter().collect();
        if let Some(linked) = linked
            && linked != input
        {
            channels.extend(self.channel_tile(linked));
        }
        let tiles = stored
            .angles
            .iter()
            .take(MAX_MULTIVIEW_ANGLES)
            .map(|angle| EngineReplayTile {
                angle: *angle,
                source_frame,
                location: self.locate(*angle, source_frame),
            })
            .collect();
        Some(EngineReplayMultiview {
            input,
            event: id,
            source_frame,
            channels,
            tiles,
        })
    }

    /// One Replay input's own tile: the angle it shows at its own position.
    fn channel_tile(&self, input: InputId) -> Option<EngineReplayTile> {
        let player = self.players.get(&input)?;
        let event = player
            .event
            .and_then(|id| self.store.event_frames.get(&id))?;
        let source_frame =
            (event.in_frame + player.position / 4).min(event.out_frame.saturating_sub(1));
        let angle = player
            .angle
            .filter(|angle| event.angles.contains(angle))
            .or_else(|| event.angles.first().copied())?;
        Some(EngineReplayTile {
            angle,
            source_frame,
            location: self.locate(angle, source_frame),
        })
    }

    /// The compiled output length of reel `reel`.
    pub(crate) fn reel_plan(&self, show: &ShowState, reel: u64) -> Result<u64, EngineReelError> {
        let spec = show.reel(reel).ok_or(EngineReelError::UnknownReel(reel))?;
        Ok(crate::reel::compile(self, spec)?.total_frames())
    }

    #[allow(clippy::too_many_lines)]
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
                    .store
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
                    .store
                    .segments
                    .iter()
                    .filter(|(_, segment)| segment.input == *input)
                {
                    bytes += self
                        .store
                        .catalog
                        .segment(*id)
                        .map_or(0, |segment| segment.bytes);
                }
                EngineReplaySourceStatus {
                    input: *input,
                    segments,
                    bytes,
                    first_frame,
                    last_frame: self.store.last_frames.get(input).copied(),
                }
            })
            .collect();
        let events = self
            .store
            .event_frames
            .iter()
            .filter_map(|(id, event)| {
                let stored = self.store.events.event(Self::event_id(*id))?;
                let range = self.range(event.in_frame, event.out_frame)?;
                let complete = event.angles.iter().all(|angle| {
                    let coverage = self.store.catalog.coverage(camera(*angle), range);
                    coverage.is_complete()
                        && coverage.segments.iter().all(|segment| {
                            self.store
                                .catalog
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
                    .and_then(|id| self.store.event_frames.get(&id))
                    .map(|event| {
                        (event.in_frame + player.position / 4)
                            .min(event.out_frame.saturating_sub(1))
                    });
                EngineReplayPlayerStatus {
                    input,
                    event: player.event,
                    angle: player.angle,
                    speed: player.speed,
                    rate_milli: player.rate_milli,
                    shuttling: player.shuttling,
                    playing: player.playing,
                    frame,
                    playlist: player.playlist.clone(),
                    playlist_index: player
                        .playlist_index
                        .and_then(|index| u32::try_from(index).ok()),
                    linked: player.linked,
                }
            })
            .collect();
        EngineReplayStatus {
            settings: self.settings.clone(),
            storage_root: self.root.to_string_lossy().into_owned(),
            feed: self.feed,
            live_frame: self.store.last_frames.values().copied().max(),
            sources,
            mark_in: self.mark_in,
            mark_out: self.mark_out,
            events,
            players,
            storage: self.storage_status(),
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
        ReplaySettingsError::AutoReturn => invalid("auto_return", "out_of_range", message),
        ReplaySettingsError::Transition => invalid("transition", "out_of_range", message),
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

fn check_linked(state: &ShowState, linked: InputId) -> Result<(), Rejection> {
    if !state.inputs().contains(&linked) {
        return Err(invalid(
            "linked",
            "unknown_input",
            format!("input {linked} is not part of this show"),
        ));
    }
    if !matches!(
        state.input_source(linked),
        Some(fm_model::InputSourceSpec::Replay)
    ) {
        return Err(invalid(
            "linked",
            "not_replay_input",
            format!("input {linked} is not a Replay input"),
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
#[allow(clippy::too_many_lines)]
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
        EngineReplayOperation::Shuttle { input, rate_milli } => {
            check_player(state, *input)?;
            if (-16_000..=16_000).contains(rate_milli) {
                Ok(())
            } else {
                Err(invalid(
                    "rate",
                    "out_of_range",
                    format!("a replay shuttle rate is -16x through +16x, not {rate_milli}/1000"),
                ))
            }
        }
        EngineReplayOperation::Play { input, .. }
        | EngineReplayOperation::Pause { input }
        | EngineReplayOperation::CueIn { input }
        | EngineReplayOperation::PlaylistNext { input }
        | EngineReplayOperation::PlaylistPrev { input } => check_player(state, *input),
        EngineReplayOperation::Return { .. } => Err(Rejection::new(
            RejectionCode::Conflict,
            "replay_return is issued by the server's auto return",
        )),
        EngineReplayOperation::Link { input, linked } => {
            check_player(state, *input)?;
            if let Some(linked) = linked {
                check_linked(state, *linked)?;
                if *linked == *input {
                    return Err(invalid(
                        "linked",
                        "self_link",
                        "a Replay input cannot link to itself".to_owned(),
                    ));
                }
            }
            Ok(())
        }
        EngineReplayOperation::PlaylistSet { input, entries } => {
            check_player(state, *input)?;
            if entries.len() > fm_types::replay::MAX_REPLAY_PLAYLIST_ENTRIES {
                return Err(invalid(
                    "entries",
                    "too_many_entries",
                    format!(
                        "a replay playlist holds at most {} entries",
                        fm_types::replay::MAX_REPLAY_PLAYLIST_ENTRIES
                    ),
                ));
            }
            for entry in entries {
                if let Some(angle) = entry.angle {
                    check_angle(state, angle)?;
                }
            }
            Ok(())
        }
        EngineReplayOperation::Step { input, frames } => {
            check_player(state, *input)?;
            if fm_types::replay::replay_step_is_valid(*frames) {
                Ok(())
            } else {
                Err(invalid(
                    "frames",
                    "invalid_step",
                    format!(
                        "a replay step moves 1 to {} frames either way",
                        fm_types::replay::MAX_REPLAY_STEP_FRAMES
                    ),
                ))
            }
        }
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

/// Plans the auto return the cue clock's advance for `frame` names
/// ([`REPLAY_RETURN_LIST`]): Program takes the last live source back with
/// the show's auto return mix, as one cue action.
pub(crate) fn plan_return(
    engine: &crate::Engine,
    frame: u64,
    target_frame: fm_scheduler::FrameNumber,
) -> Result<crate::engine::CuePlan, Rejection> {
    let state = engine.show();
    let Some((input, to, frames)) = engine.replay_return_due() else {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            format!("no replay auto return is due at frame {frame}"),
        ));
    };
    let effect = if frames == 0 {
        fm_model::TransitionEffect::Cut
    } else {
        fm_model::TransitionEffect::Fade
    };
    let fired = [fm_automation::FiredCueAction {
        cue: REPLAY_RETURN_LIST,
        step: 0,
        action: fm_model::CueAction::Take {
            input: Some(fm_types::InputTarget::Fixed(to)),
            effect,
            duration_frames: frames,
        },
    }];
    let mut show = state.clone();
    let mut runtime = Vec::new();
    let mut transition = None;
    let mut title_texts = Vec::new();
    engine
        .apply_cue_actions(
            &mut show,
            &fired,
            target_frame,
            &mut crate::engine::CueFrame {
                runtime: &mut runtime,
                transition: &mut transition,
                title_texts: &mut title_texts,
                scripts: None,
                takes: 0,
                targets: fm_types::TargetBindings::default(),
            },
        )
        .map_err(|(_, _, rejection)| rejection)?;
    Ok(crate::engine::CuePlan {
        show: Box::new(show),
        event: crate::EngineEvent::ReplayChanged {
            frame: target_frame.get(),
            operation: EngineReplayOperation::Return { input, to },
        },
        runtime,
        transition,
    })
}
