//! Realized program recorder status (Protocol 2.66).
//!
//! `recorder_status` is a lossy, non-resumable, latest-wins record like
//! `stream_status`: server identity, an independent strictly increasing
//! `sequence`, and `recorders`, one sample per recorder slot in strict slot
//! order, `;`-separated, each `<slot>:<state>:<elapsed_ms>:<failure>`:
//!
//! - `state` is `unavailable` (no output is configured for the slot), `off`,
//!   `starting` (the segment is opening or awaiting its first muxed output),
//!   `recording`, or `failed`;
//! - `elapsed_ms` is how long the slot has been recording (across segment
//!   rotations), and `0` in every other state;
//! - `failure` is a sanitized short code (at most
//!   [`crate::MAX_STREAM_DETAIL_BYTES`] bytes, never a path), escaped like a
//!   `stream_status` failure, present exactly when `state` is `failed`.
//!
//! The record never advances or carries a durable revision; the desired
//! state stays the snapshot's `recordings` roster.

use crate::{ServerIdentity, WireRecorderSlot};

/// Realized state of one program recorder slot (Protocol 2.66).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RecorderRealizedState {
    /// No output is configured for the slot, so it cannot record.
    Unavailable,
    /// Configured and not recording.
    #[default]
    Off,
    /// A segment is opening or awaiting its first muxed output.
    Starting,
    /// A segment is being written.
    Recording,
    /// The last start or the running segment failed.
    Failed,
}

impl RecorderRealizedState {
    /// Every state in wire order.
    pub const ALL: [Self; 5] = [
        Self::Unavailable,
        Self::Off,
        Self::Starting,
        Self::Recording,
        Self::Failed,
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Failed => "failed",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.token() == token)
    }
}

/// One recorder slot's realized state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderStatusSample {
    pub recorder: WireRecorderSlot,
    pub realized: RecorderRealizedState,
    /// Milliseconds the slot has been recording; `0` unless `realized` is
    /// [`RecorderRealizedState::Recording`].
    pub elapsed_ms: u64,
    /// Sanitized short failure code; present exactly when `realized` is
    /// [`RecorderRealizedState::Failed`].
    pub failure: Option<String>,
}

impl RecorderStatusSample {
    /// Whether the sample keeps the wire invariants: elapsed time only while
    /// recording, a nonblank bounded failure exactly when failed.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let elapsed = self.elapsed_ms == 0 || self.realized == RecorderRealizedState::Recording;
        let failure = match &self.failure {
            Some(failure) => {
                self.realized == RecorderRealizedState::Failed
                    && !failure.is_empty()
                    && failure.len() <= crate::MAX_STREAM_DETAIL_BYTES
            }
            None => self.realized != RecorderRealizedState::Failed,
        };
        elapsed && failure
    }
}

/// The most samples one `recorder_status` carries: one per recorder slot.
pub const MAX_RECORDER_STATUS_SAMPLES: usize = 2;

/// Lossy, non-resumable realized recorder status (Protocol 2.66).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderStatusMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub samples: Vec<RecorderStatusSample>,
}
