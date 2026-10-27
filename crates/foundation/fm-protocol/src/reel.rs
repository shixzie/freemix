//! Highlight reels (OR-013).
//!
//! A project holds at most [`MAX_REELS`] [`ReelSpec`]s. A reel is an ordered
//! list of at most [`MAX_REEL_CLIPS`] clips cut from recorded replay events:
//! each clip names the event, the recorded angle it plays, trims before and
//! after the event marks, a playback speed, and the frames it dissolves in
//! from the clip before (Protocol 2.111). The recorded media itself is never
//! part of the project.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`ReelOperation::Set`] | `reel_set` | `reel`, `name`, `clips` | project edit |
//! | [`ReelOperation::Delete`] | `reel_delete` | `reel` | project edit |
//! | [`ReelOperation::Export`] | `reel_export` | `job`, `reel`, `destination` | runtime |
//! | [`ReelOperation::CancelExport`] | `reel_export_cancel` | `job` | runtime |
//! | [`ReelOperation::ForgetExport`] | `reel_export_forget` | `job` | runtime |
//!
//! `clips` is a comma-separated list of 1 to [`MAX_REEL_CLIPS`]
//! `<event>:<angle>:<trim in>:<trim out>:<speed>:<dissolve>` records, every
//! number a canonical unsigned decimal, `speed` 25, 50, 75, or 100, each trim
//! at most [`MAX_REEL_TRIM_FRAMES`], and `dissolve` 0 (a cut) on the first
//! clip and at most [`MAX_REEL_DISSOLVE_FRAMES`] on the others. A `destination` is an absolute path (see
//! [`reel_export_path_is_valid`]).
//!
//! `reel_set` and `reel_delete` are padlocked, undoable project edits. An
//! accepted `reel_set` is announced by the durable `event=reel_changed`
//! carrying the reel afterwards (`reel`, `name`, `clips`, optional
//! `total_frames` and `error`); an accepted `reel_delete` by
//! `event=reel_removed` carrying `reel`. The export jobs are server runtime:
//! they are announced only by the lossy, latest-wins `reel_status` record
//! ([`ReelStatusMessage`]), pushed to raw control sessions after the snapshot
//! and whenever it changes, and sent to any session that asks with
//! `reel_status_request` (`request_id`), echoing the `request_id`.

use crate::ServerIdentity;

pub use fm_types::reel::{
    MAX_REEL_CLIPS, MAX_REEL_DISSOLVE_FRAMES, MAX_REEL_MUSIC_PATH_BYTES, MAX_REEL_MUSIC_PERCENT,
    MAX_REEL_NAME_BYTES, MAX_REEL_PATH_BYTES, MAX_REEL_TRIM_FRAMES, MAX_REELS, ReelClip, ReelMusic,
    ReelSpec, ReelSpecError, clip_dissolve_is_valid, reel_export_path_is_valid, reel_name_is_valid,
};

/// Most export jobs one `reel_status` lists: the running jobs and the
/// results kept until `reel_export_forget`.
pub const MAX_REEL_EXPORTS: usize = 8;

/// Longest reel status error or export failure, in UTF-8 bytes; the same
/// bound other status errors use.
pub const MAX_REEL_ERROR_BYTES: usize = 160;

/// Whether `text` is a usable reel status error or export failure: non-blank,
/// at most [`MAX_REEL_ERROR_BYTES`], and one line free of control characters.
#[must_use]
pub fn reel_error_is_valid(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_REEL_ERROR_BYTES
        && !text.chars().any(char::is_control)
}

/// One highlight reel command.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ReelOperation {
    /// Replaces the reel with `spec.id` or appends it.
    Set(Box<ReelSpec>),
    /// Deletes a reel.
    Delete { reel: u64 },
    /// Starts exporting a reel to `destination`.
    Export {
        job: u64,
        reel: u64,
        destination: String,
    },
    /// Cancels a running export job.
    CancelExport { job: u64 },
    /// Drops a finished, failed, or cancelled export job from the status.
    ForgetExport { job: u64 },
}

impl ReelOperation {
    /// Every payload token, in table order.
    pub const TOKENS: [&'static str; 5] = [
        "reel_set",
        "reel_delete",
        "reel_export",
        "reel_export_cancel",
        "reel_export_forget",
    ];

    /// The payload token.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Set(_) => "reel_set",
            Self::Delete { .. } => "reel_delete",
            Self::Export { .. } => "reel_export",
            Self::CancelExport { .. } => "reel_export_cancel",
            Self::ForgetExport { .. } => "reel_export_forget",
        }
    }

    /// Whether this is a live operation (the export jobs; everything but
    /// [`Self::Set`] and [`Self::Delete`]).
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Export { .. } | Self::CancelExport { .. } | Self::ForgetExport { .. }
        )
    }
}

/// One export job's state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ReelExportState {
    /// Accepted, waiting for the renderer.
    #[default]
    Queued,
    /// Rendering frames.
    Running,
    /// The file is written; the job is finishing it.
    Finalizing,
    /// The destination holds the whole export.
    Completed,
    /// The export failed; `failure` says why.
    Failed,
    /// The operator cancelled the export.
    Cancelled,
}

impl ReelExportState {
    /// Every state token.
    pub const TOKENS: [&'static str; 6] = [
        "queued",
        "running",
        "finalizing",
        "completed",
        "failed",
        "cancelled",
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Running,
            Self::Finalizing,
            Self::Completed,
            Self::Failed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|state| state.token() == token)
    }
}

/// One project reel and the runtime facts about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReelStatus {
    /// The reel's id; equals `spec.id`.
    pub id: u64,
    /// The reel's stored definition.
    pub spec: ReelSpec,
    /// The reel's duration in frames, once known.
    pub total_frames: Option<u64>,
    /// Why the reel cannot be exported, while it cannot.
    pub error: Option<String>,
}

/// One export job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReelExportStatus {
    pub job: u64,
    pub reel: u64,
    /// The absolute path the file is written to.
    pub destination: String,
    pub state: ReelExportState,
    /// Frames written so far.
    pub completed_frames: u64,
    /// The export's duration in frames, once known.
    pub total_frames: Option<u64>,
    /// Bytes written so far, once known.
    pub bytes: Option<u64>,
    /// Why the export failed, while `state` is [`ReelExportState::Failed`].
    pub failure: Option<String>,
}

/// Lossy, latest-wins highlight reel status (OR-013).
///
/// Fields: server identity, `sequence`, optional `request_id` (answering a
/// [`ReelStatusRequest`]), and two `;`-separated lists:
///
/// * `reels`: `<reel>:<name>:<total frames>:<error>:<clips>` per reel, the
///   name and error percent-escaped, `total frames` and `error` empty when
///   unknown;
/// * `exports`:
///   `<job>:<reel>:<destination>:<state>:<completed frames>:<total frames>:<bytes>:<failure>`
///   per export job, the destination and failure percent-escaped, `total
///   frames`, `bytes`, and `failure` empty when unknown, and `state` a
///   [`ReelExportState`] token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReelStatusMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub request_id: Option<String>,
    pub reels: Vec<ReelStatus>,
    pub exports: Vec<ReelExportStatus>,
}

/// Asks for a [`ReelStatusMessage`] (OR-013).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReelStatusRequest {
    pub request_id: String,
}
