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
//! | [`ReplayOperation::Return`] | `replay_return` | `input`, `to` | announced only: the server's auto return; a client's is refused |
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
    MAX_REPLAY_EVENTS, MAX_REPLAY_INPUTS, MAX_REPLAY_RECORD_SOURCES, MAX_REPLAY_RETENTION_SECONDS,
    MAX_REPLAY_STEP_FRAMES, MAX_REPLAY_STORAGE_ROOT_BYTES, MIN_REPLAY_RETENTION_SECONDS,
    REPLAY_LAST_SECONDS, ReplaySettings, ReplaySettingsError, ReplaySpeed,
    replay_event_name_is_valid, replay_step_is_valid, replay_storage_root_is_valid,
};

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
    /// Pauses a Replay input.
    Pause { input: WireInputId },
    /// Jumps a Replay input back to its event's first frame, paused.
    CueIn { input: WireInputId },
    /// Moves a cued Replay input `frames` source frames (back when
    /// negative), paused, within its event (Protocol 2.114, jog).
    Step { input: WireInputId, frames: i32 },
    /// Program went back from Replay input `input`, which played its event
    /// to the end on air, to `to` (Protocol 2.116 auto return). Only the
    /// server issues it.
    Return { input: WireInputId, to: WireInputId },
}

impl ReplayOperation {
    /// Every payload token, in table order.
    pub const TOKENS: [&'static str; 14] = [
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
        "replay_pause",
        "replay_cue_in",
        "replay_step",
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
            Self::Pause { .. } => "replay_pause",
            Self::CueIn { .. } => "replay_cue_in",
            Self::Step { .. } => "replay_step",
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayPlayerStatus {
    pub input: WireInputId,
    pub event: Option<u32>,
    pub angle: Option<WireInputId>,
    pub speed: ReplaySpeed,
    pub playing: bool,
    /// The source frame shown, while cued.
    pub frame: Option<u64>,
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
/// * `players`: `<input>:<event>:<angle>:<speed>:<playing 0|1>:<frame>`,
///   `event`, `angle`, and `frame` empty while nothing is cued.
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
}

/// Asks for a [`ReplayStatusMessage`] (OR-012).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReplayStatusRequest {
    pub request_id: String,
}
