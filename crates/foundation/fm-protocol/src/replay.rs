//! Instant replay (OR-012).
//!
//! The server records chosen inputs continuously for replay into rolling
//! segment files and keeps the last [`ReplaySettings::retention_seconds`]
//! of each; operators mark and capture events and play them on Replay
//! inputs (`kind=replay` sources) at a quarter, half, three quarters, or
//! full speed. Frames are engine frames: frame `f` of a source is the
//! picture it showed on engine frame `f`.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`ReplayOperation::Configure`] | `replay_configure` | `sources` (comma-separated input ids in angle order, may be empty), `retention_seconds` (`10..=3600`), optional `storage_root` (absolute path; absent lets the server choose) | project edit |
//! | [`ReplayOperation::MarkIn`] | `replay_mark_in` | — | transition |
//! | [`ReplayOperation::MarkOut`] | `replay_mark_out` | — | transition |
//! | [`ReplayOperation::CaptureMarks`] | `replay_capture_marks` | optional `name` | transition |
//! | [`ReplayOperation::CaptureLast`] | `replay_capture_last` | `seconds` (`5\|10\|20`) | transition |
//! | [`ReplayOperation::RenameEvent`] | `replay_event_rename` | `event_id`, `name` | transition |
//! | [`ReplayOperation::DeleteEvent`] | `replay_event_delete` | `event_id` | transition |
//! | [`ReplayOperation::Cue`] | `replay_cue` | `input`, `event_id`, optional `angle` | transition |
//! | [`ReplayOperation::Angle`] | `replay_angle` | `input`, `angle` | transition |
//! | [`ReplayOperation::Play`] | `replay_play` | `input`, `speed` (`25\|50\|75\|100`) | transition |
//! | [`ReplayOperation::Pause`] | `replay_pause` | `input` | transition |
//! | [`ReplayOperation::CueIn`] | `replay_cue_in` | `input` | transition |
//! | [`ReplayOperation::Step`] | `replay_step` | `input`, `frames` (a canonical signed decimal, nonzero, at most 600 either way) | transition |
//! | [`ReplayOperation::Link`] | `replay_link` | `input`, `linked` (a Replay input; empty unlinks) | transition |
//! | [`ReplayOperation::PlaylistSet`] | `replay_playlist_set` | `input`, `entries` (`event/angle/speed` entries joined by `\|`, empty clears it; at most 16) | transition |
//! | [`ReplayOperation::PlaylistNext`] | `replay_playlist_next` | `input` | transition |
//! | [`ReplayOperation::PlaylistPrev`] | `replay_playlist_prev` | `input` | transition |
//! | [`ReplayOperation::Return`] | `replay_return` | `input`, `to` | announced only: the server's auto return; a client's is refused |
//!
//! A Replay input may carry a playlist: an ordered list of up to
//! [`MAX_REPLAY_PLAYLIST_ENTRIES`] events, each with an optional angle
//! (absent plays the event's preferred one) and a speed. A playing player
//! that reaches the end of its event moves on to the next listed event that
//! still exists, playing at that entry's speed, and only a playlist with
//! nothing left stops the player (and returns Program when auto return is
//! set); `replay_playlist_next` and `replay_playlist_prev` cue the next or
//! previous listed event at its first frame, keeping the player playing or
//! paused as it was.
//!
//! Two Replay inputs may be linked (Protocol 2.124, A/B): linking cues both
//! on the same event at the same position, speed, and playing state while
//! each keeps its own angle, and play, pause, cue-in, and step on either
//! move both. The link is symmetric and clears any previous partner on both
//! sides; a linked player that reaches its event's end advances its playlist
//! only when its partner can too, and otherwise both stop together.
//!
//! Only `replay_configure` is padlocked and undoable. Every accepted
//! operation is announced by the durable `event=replay_changed` carrying
//! `frame` (the frame it is realized on), `operation` (the payload token),
//! and the operation's fields. Events are server runtime: an operation
//! naming an event that does not exist is accepted and changes nothing.
//!
//! The recording, marks, events, and players are announced by the lossy,
//! latest-wins `replay_status` record ([`ReplayStatusMessage`]), pushed to
//! raw control sessions after the snapshot and whenever it changes, and
//! sent to any session that asks with `replay_status_request`
//! (`request_id`), echoing the `request_id`.

use crate::{ServerIdentity, WireInputId};

pub use fm_types::replay::{
    DEFAULT_REPLAY_RETENTION_SECONDS, MAX_REPLAY_AUTO_RETURN_FRAMES, MAX_REPLAY_EVENT_NAME_BYTES,
    MAX_REPLAY_EVENTS, MAX_REPLAY_INPUTS, MAX_REPLAY_PLAYLIST_ENTRIES, MAX_REPLAY_RECORD_SOURCES,
    MAX_REPLAY_RETENTION_SECONDS, MAX_REPLAY_STEP_FRAMES, MAX_REPLAY_STORAGE_ROOT_BYTES,
    MIN_REPLAY_RETENTION_SECONDS, REPLAY_LAST_SECONDS, ReplaySettings, ReplaySettingsError,
    ReplaySpeed, replay_event_name_is_valid, replay_step_is_valid, replay_storage_root_is_valid,
};

/// One entry in a Replay input's playlist (Protocol 2.119).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReplayPlaylistEntry {
    /// The event to play.
    pub event: u32,
    /// The angle to play; `None` plays the event's preferred one.
    pub angle: Option<WireInputId>,
    /// The speed the entry plays at.
    pub speed: ReplaySpeed,
}

/// One replay command.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ReplayOperation {
    /// Replaces what is recorded for replay.
    Configure(Box<ReplaySettings>),
    /// Marks the next event's first frame.
    MarkIn,
    /// Marks the next event's last frame.
    MarkOut,
    /// Captures the marked range as an event.
    CaptureMarks { name: Option<String> },
    /// Captures the last `seconds` as an event.
    CaptureLast { seconds: u8 },
    /// Renames an event.
    RenameEvent { event: u32, name: String },
    /// Deletes an event.
    DeleteEvent { event: u32 },
    /// Cues a Replay input on an event's first frame, paused.
    Cue {
        input: WireInputId,
        event: u32,
        angle: Option<WireInputId>,
    },
    /// Switches a Replay input's angle.
    Angle {
        input: WireInputId,
        angle: WireInputId,
    },
    /// Plays a Replay input at `speed`.
    Play {
        input: WireInputId,
        speed: ReplaySpeed,
    },
    /// Shuttles a Replay input at `rate_milli` thousandths of real time
    /// (negative plays backwards; zero pauses), pausing at the event's
    /// first or last frame (Protocol 2.131).
    Shuttle { input: WireInputId, rate_milli: i32 },
    /// Pauses a Replay input.
    Pause { input: WireInputId },
    /// Jumps a Replay input back to its event's first frame, paused.
    CueIn { input: WireInputId },
    /// Moves a cued Replay input `frames` source frames (back when
    /// negative), paused, within its event (Protocol 2.114, jog).
    Step { input: WireInputId, frames: i32 },
    /// Links Replay input `input` to `linked` for A/B playback, or clears
    /// its link with `linked: None` (Protocol 2.124).
    Link {
        input: WireInputId,
        linked: Option<WireInputId>,
    },
    /// Replaces Replay input `input`'s playlist (Protocol 2.119); an empty
    /// list clears it.
    PlaylistSet {
        input: WireInputId,
        entries: Vec<ReplayPlaylistEntry>,
    },
    /// Cues the next playlist entry that still exists, at its first frame,
    /// keeping the player playing or paused as it was.
    PlaylistNext { input: WireInputId },
    /// Cues the previous playlist entry that still exists, at its first
    /// frame, keeping the player playing or paused as it was.
    PlaylistPrev { input: WireInputId },
    /// Program went back from Replay input `input`, which played its event
    /// to the end on air, to `to` (Protocol 2.116 auto return). Only the
    /// server issues it.
    Return { input: WireInputId, to: WireInputId },
}

impl ReplayOperation {
    /// Every payload token, in table order.
    pub const TOKENS: [&'static str; 19] = [
        "replay_configure",
        "replay_mark_in",
        "replay_mark_out",
        "replay_capture_marks",
        "replay_capture_last",
        "replay_event_rename",
        "replay_event_delete",
        "replay_cue",
        "replay_angle",
        "replay_play",
        "replay_shuttle",
        "replay_pause",
        "replay_cue_in",
        "replay_step",
        "replay_link",
        "replay_playlist_set",
        "replay_playlist_next",
        "replay_playlist_prev",
        "replay_return",
    ];

    /// The payload token.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Configure(_) => "replay_configure",
            Self::MarkIn => "replay_mark_in",
            Self::MarkOut => "replay_mark_out",
            Self::CaptureMarks { .. } => "replay_capture_marks",
            Self::CaptureLast { .. } => "replay_capture_last",
            Self::RenameEvent { .. } => "replay_event_rename",
            Self::DeleteEvent { .. } => "replay_event_delete",
            Self::Cue { .. } => "replay_cue",
            Self::Angle { .. } => "replay_angle",
            Self::Play { .. } => "replay_play",
            Self::Shuttle { .. } => "replay_shuttle",
            Self::Pause { .. } => "replay_pause",
            Self::CueIn { .. } => "replay_cue_in",
            Self::Step { .. } => "replay_step",
            Self::Link { .. } => "replay_link",
            Self::PlaylistSet { .. } => "replay_playlist_set",
            Self::PlaylistNext { .. } => "replay_playlist_next",
            Self::PlaylistPrev { .. } => "replay_playlist_prev",
            Self::Return { .. } => "replay_return",
        }
    }

    /// Whether this is a live operation (everything but
    /// [`Self::Configure`]).
    #[must_use]
    pub const fn is_live(&self) -> bool {
        !matches!(self, Self::Configure(_))
    }
}

/// How the server feeds its replay recording.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ReplayFeed {
    /// A simulated server records synthetically, without files.
    #[default]
    Synthetic,
    /// The native media plane writes segment files.
    Recorded,
}

impl ReplayFeed {
    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Recorded => "recorded",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Synthetic, Self::Recorded]
            .into_iter()
            .find(|feed| feed.token() == token)
    }
}

/// One recorded source's retained media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplaySourceStatus {
    pub input: WireInputId,
    pub segments: u32,
    pub bytes: u64,
    /// The oldest retained frame.
    pub first_frame: Option<u64>,
    /// The newest recorded frame.
    pub last_frame: Option<u64>,
}

/// One captured event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayEventStatus {
    pub id: u32,
    pub name: String,
    /// First frame.
    pub in_frame: u64,
    /// One past the last frame.
    pub out_frame: u64,
    /// The angles, in source order; the first is preferred.
    pub angles: Vec<WireInputId>,
    /// Whether recorded segment files cover the whole event on every angle.
    pub complete: bool,
}

/// One Replay input's player.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayPlayerStatus {
    pub input: WireInputId,
    pub event: Option<u32>,
    pub angle: Option<WireInputId>,
    pub speed: ReplaySpeed,
    /// The shuttle rate, or zero when not shuttling (Protocol 2.131).
    pub rate_milli: i32,
    pub shuttling: bool,
    pub playing: bool,
    /// The source frame shown, while cued.
    pub frame: Option<u64>,
    /// The player's playlist, in order (Protocol 2.119).
    pub playlist: Vec<ReplayPlaylistEntry>,
    /// The playlist entry the player is on, while it is on one.
    pub playlist_index: Option<u32>,
    /// The Replay input this player plays in step with, if any (2.124).
    pub linked: Option<WireInputId>,
}

/// How close the replay storage root is to full (2.125, OR-014).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplayStorageLevel {
    Ok,
    /// Less than ten minutes of recording left.
    Warning,
    /// Less than two minutes of recording left.
    Critical,
    /// No free space, or the root cannot be sampled.
    Full,
}

impl ReplayStorageLevel {
    /// Every level token.
    pub const TOKENS: [&'static str; 4] = ["ok", "warning", "critical", "full"];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Full => "full",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Ok, Self::Warning, Self::Critical, Self::Full]
            .into_iter()
            .find(|level| level.token() == token)
    }
}

/// Free space and the predicted recording time of the replay root (2.125,
/// OR-014).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayStorageStatus {
    pub free_bytes: u64,
    /// Bytes written per second by the retained recording.
    pub bytes_per_second: u64,
    /// Seconds of recording left at that rate, when it is nonzero.
    pub remaining_seconds: Option<u64>,
    pub level: ReplayStorageLevel,
}

/// Lossy, latest-wins replay status (OR-012).
///
/// Fields: server identity, `sequence`, optional `request_id` (answering a
/// [`ReplayStatusRequest`]), `feed` (`synthetic|recorded`), `sources`,
/// `retention_seconds`, optional `storage_root` (the settings), `root`
/// (the directory in use), optional `live_frame`, optional `mark_in` and
/// `mark_out` (one past the marked frame), and three `;`-separated lists:
///
/// * `recorded`: `<input>:<segments>:<bytes>:<first>:<last>` per source,
///   `first` and `last` empty while nothing is retained;
/// * `events`: `<id>:<in>:<out>:<complete 0|1>:<angles '/'-joined>:<name>`,
///   the name percent-escaped;
/// * `players`: `<input>:<event>:<angle>:<speed>:<playing 0|1>:<frame>:<playlist index>:<playlist>:<linked>`,
///   `event`, `angle`, and `frame` empty while nothing is cued, the index
///   empty while the player is on no playlist entry, the playlist a
///   `|`-joined list of `<event>/<angle>/<speed>` entries (the angle empty
///   for the event's preferred one), and `linked` the Replay input the
///   player is linked to (empty when none);
/// * `storage`: `-` when the daemon does not sample the root, else
///   `<free bytes>:<bytes per second>:<remaining seconds or ->:<level>`
///   (2.125).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayStatusMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub request_id: Option<String>,
    pub feed: ReplayFeed,
    pub settings: ReplaySettings,
    pub root: String,
    pub live_frame: Option<u64>,
    pub mark_in: Option<u64>,
    pub mark_out: Option<u64>,
    pub recorded: Vec<ReplaySourceStatus>,
    pub events: Vec<ReplayEventStatus>,
    pub players: Vec<ReplayPlayerStatus>,
    pub storage: Option<ReplayStorageStatus>,
}

/// Asks for a [`ReplayStatusMessage`] (OR-012).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReplayStatusRequest {
    pub request_id: String,
}
