//! Fault-tolerant fragmented MP4 writer over a [`RecordingSession`].
//!
//! [`ResilientFmp4Writer`] accepts the muxer's fragmented MP4 byte stream (for
//! example `FFmpeg`'s `pipe:1` output with
//! `+empty_moov+default_base_moof+frag_keyframe`), splits it into the init
//! segment and whole fragments, and appends whole fragments to the session's
//! current segment. Each segment starts with a copy of the init segment, so
//! every segment file is independently playable.
//!
//! A fragment is held in memory until it is fully written (and synced, when
//! configured). When a write, sync, or segment creation fails, the writer
//! truncates the failed segment back to its last complete fragment, lets the
//! session's failure policy pick the same root or a fallback, opens a new
//! segment there, and writes the init segment plus the same fragment again.
//! A failure therefore costs no media while any root remains usable, and the
//! session reports [`SessionError::Exhausted`] instead of stopping silently
//! once none does.
//!
//! The source stream's trailing `mfra` index is dropped: its absolute offsets
//! describe the original stream, not the re-homed segment files.

use std::num::NonZeroU64;

use crate::{
    fmp4::{Fmp4Splitter, Fmp4Unit, SplitError},
    session::{
        FailureDecision, FailureOp, RecordingSession, SegmentEntry, SegmentSink, SegmentStorage,
        SessionError, SessionSnapshot,
    },
};

/// Writer limits and durability policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResilientWriterConfig {
    /// Largest init segment or fragment buffered from the stream.
    pub max_unit_bytes: u64,
    /// `sync_data` after this many fragments; `None` leaves flushing to the
    /// operating system.
    pub sync_every_fragments: Option<NonZeroU64>,
    /// Rotate to a new segment after this many fragments.
    pub max_segment_fragments: Option<NonZeroU64>,
    /// Rotate before a fragment would push a segment past this many bytes.
    /// A segment always takes at least one fragment.
    pub max_segment_bytes: Option<NonZeroU64>,
}

impl Default for ResilientWriterConfig {
    fn default() -> Self {
        Self {
            max_unit_bytes: 64 * 1024 * 1024,
            sync_every_fragments: NonZeroU64::new(1),
            max_segment_fragments: None,
            max_segment_bytes: None,
        }
    }
}

/// Counters for one writer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriterStats {
    pub fragments_written: u64,
    /// Fragments written again into a new segment after a failure.
    pub fragments_rewritten: u64,
    pub segments_opened: u64,
    pub retries: u64,
    pub failovers: u64,
    /// Bytes landed in segments, including init copies.
    pub bytes_written: u64,
    /// Trailing `mfra` indexes dropped from the source stream.
    pub indexes_dropped: u64,
}

#[derive(Debug)]
pub enum WriterError {
    /// The source stream is not a relocatable fragmented MP4.
    Stream(SplitError),
    /// The session refused or every root failed.
    Session(SessionError),
}

impl std::fmt::Display for WriterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(error) => write!(formatter, "recording stream: {error}"),
            Self::Session(error) => write!(formatter, "recording session: {error}"),
        }
    }
}

impl std::error::Error for WriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stream(error) => Some(error),
            Self::Session(error) => Some(error),
        }
    }
}

impl From<SplitError> for WriterError {
    fn from(error: SplitError) -> Self {
        Self::Stream(error)
    }
}

impl From<SessionError> for WriterError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// Everything [`ResilientFmp4Writer::finish_report`] observed while ending a
/// stream, including the stopped session when the stream itself ended badly.
#[derive(Debug)]
pub struct WriterFinish {
    pub stats: WriterStats,
    /// Final manifest segment entries.
    pub segments: Vec<SegmentEntry>,
    /// The stopped session, or why it could not be stopped.
    pub snapshot: Result<SessionSnapshot, SessionError>,
    /// Bytes of an incomplete trailing unit that never reached a segment.
    pub discarded_bytes: u64,
    /// The first placement failure, else the stream-end failure.
    pub error: Option<WriterError>,
}

#[derive(Debug)]
struct Current<K> {
    sink: K,
    committed: u64,
    fragments: u64,
    unsynced: u64,
    proven: bool,
}

/// Fragment-granular, failover-capable fragmented MP4 segment writer.
#[derive(Debug)]
pub struct ResilientFmp4Writer<S: SegmentStorage> {
    session: RecordingSession<S>,
    config: ResilientWriterConfig,
    splitter: Fmp4Splitter,
    init: Option<Vec<u8>>,
    current: Option<Current<S::Sink>>,
    units: Vec<Fmp4Unit>,
    stats: WriterStats,
    rotate_requested: bool,
}

impl<S: SegmentStorage> ResilientFmp4Writer<S> {
    #[must_use]
    pub fn new(session: RecordingSession<S>, config: ResilientWriterConfig) -> Self {
        Self {
            session,
            splitter: Fmp4Splitter::new(config.max_unit_bytes),
            config,
            init: None,
            current: None,
            units: Vec::new(),
            stats: WriterStats::default(),
            rotate_requested: false,
        }
    }

    #[must_use]
    pub fn session(&self) -> &RecordingSession<S> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut RecordingSession<S> {
        &mut self.session
    }

    #[must_use]
    pub fn stats(&self) -> WriterStats {
        self.stats
    }

    /// Bytes held for a fragment that has not completed in the stream yet.
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.splitter.buffered_bytes()
    }

    /// Feeds muxer output. Complete fragments are written before returning.
    ///
    /// # Errors
    ///
    /// Returns [`WriterError::Stream`] for an invalid stream and
    /// [`WriterError::Session`] when the session cannot place a fragment on
    /// any root.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), WriterError> {
        self.splitter.push(bytes, &mut self.units)?;
        self.drain_units()
    }

    /// Closes the current segment after the fragment in flight; the next
    /// fragment opens a new segment.
    pub fn request_rotation(&mut self) {
        self.rotate_requested = true;
    }

    /// Ends the stream, closes the last segment, and stops the session.
    ///
    /// # Errors
    ///
    /// Returns [`WriterError::Stream`] when the stream ended inside a
    /// fragment (the partial fragment is not written), or a session error.
    pub fn finish(mut self) -> Result<SessionSnapshot, WriterError> {
        let ended = self.splitter.finish(&mut self.units);
        self.drain_units()?;
        if self.current.is_none() && self.init.is_some() && self.stats.fragments_written == 0 {
            // A stream with no fragments still yields one playable, empty
            // segment rather than nothing.
            self.ensure_segment()?;
        }
        self.close_current();
        let snapshot = self.session.stop()?;
        ended?;
        Ok(snapshot)
    }

    /// Like [`Self::finish`], but always closes the last segment and stops the
    /// session, and reports the outcome instead of returning early: a stream
    /// that ended inside a fragment (a killed muxer) still leaves a stopped
    /// session whose segments all end on fragment boundaries.
    #[must_use]
    pub fn finish_report(mut self) -> WriterFinish {
        let ended = self.splitter.finish(&mut self.units);
        let discarded_bytes = match &ended {
            Err(SplitError::Truncated { buffered }) => *buffered as u64,
            _ => 0,
        };
        let mut placed = self.drain_units();
        if placed.is_ok()
            && self.current.is_none()
            && self.init.is_some()
            && self.stats.fragments_written == 0
        {
            placed = self.ensure_segment();
        }
        self.close_current();
        let stats = self.stats;
        let segments = self.session.manifest().segments.clone();
        let snapshot = self.session.stop();
        let error = placed
            .err()
            .or_else(|| ended.err().map(WriterError::Stream));
        WriterFinish {
            stats,
            segments,
            snapshot,
            discarded_bytes,
            error,
        }
    }

    fn drain_units(&mut self) -> Result<(), WriterError> {
        let units = std::mem::take(&mut self.units);
        for unit in units {
            match unit {
                Fmp4Unit::Init { bytes, .. } => self.init = Some(bytes),
                Fmp4Unit::Fragment { bytes, .. } => self.write_fragment(&bytes)?,
                Fmp4Unit::Index { .. } => self.stats.indexes_dropped += 1,
            }
        }
        Ok(())
    }

    fn close_current(&mut self) {
        if let Some(current) = self.current.take() {
            let mut sink = current.sink;
            let synced = if current.unsynced > 0 {
                sink.sync_data()
            } else {
                Ok(())
            };
            match synced {
                Ok(()) => {
                    // `close_segment` cannot fail here: a segment is open.
                    let _ = self.session.close_segment(current.committed);
                }
                Err(error) => {
                    let _ = self
                        .session
                        .record_failure(FailureOp::Sync, &error, current.committed);
                }
            }
        }
    }

    fn rotation_due(&self, next_len: u64) -> bool {
        let Some(current) = &self.current else {
            return false;
        };
        if current.fragments == 0 {
            return false;
        }
        self.rotate_requested
            || self
                .config
                .max_segment_fragments
                .is_some_and(|limit| current.fragments >= limit.get())
            || self
                .config
                .max_segment_bytes
                .is_some_and(|limit| current.committed.saturating_add(next_len) > limit.get())
    }

    /// Opens a segment and writes the init segment into it, retrying through
    /// the failure policy.
    fn ensure_segment(&mut self) -> Result<(), WriterError> {
        while self.current.is_none() {
            let init = self.init.as_deref().ok_or(SplitError::Invalid {
                reason: "fragment before init segment",
                offset: 0,
            })?;
            let mut opened = self.session.open_segment()?;
            self.stats.segments_opened += 1;
            match opened.sink.write_all(init) {
                Ok(()) => {
                    self.stats.bytes_written += init.len() as u64;
                    self.session.note_progress(init.len() as u64);
                    self.current = Some(Current {
                        sink: opened.sink,
                        committed: init.len() as u64,
                        fragments: 0,
                        unsynced: 1,
                        proven: false,
                    });
                }
                Err(error) => {
                    let _ = opened.sink.truncate(0);
                    let decision = self.session.record_failure(FailureOp::Write, &error, 0)?;
                    self.apply(decision)?;
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, decision: FailureDecision) -> Result<(), WriterError> {
        match decision {
            FailureDecision::Retry { .. } => {
                self.stats.retries += 1;
                Ok(())
            }
            FailureDecision::Fallback { .. } => {
                self.stats.failovers += 1;
                Ok(())
            }
            FailureDecision::Exhausted => Err(WriterError::Session(SessionError::Exhausted {
                last: self.session.manifest().errors.last().copied(),
            })),
        }
    }

    fn write_fragment(&mut self, bytes: &[u8]) -> Result<(), WriterError> {
        let length = bytes.len() as u64;
        if self.rotation_due(length) {
            self.close_current();
            self.rotate_requested = false;
        }
        let mut rewriting = false;
        loop {
            self.ensure_segment()?;
            let Some(current) = self.current.as_mut() else {
                continue;
            };
            let sync_due = self
                .config
                .sync_every_fragments
                .is_some_and(|every| current.unsynced + 1 >= every.get());
            let result = match current.sink.write_all(bytes) {
                Err(error) => Err((FailureOp::Write, error)),
                Ok(()) if sync_due => current
                    .sink
                    .sync_data()
                    .map_err(|error| (FailureOp::Sync, error)),
                Ok(()) => Ok(()),
            };
            match result {
                Ok(()) => {
                    current.committed += length;
                    current.fragments += 1;
                    current.unsynced = if sync_due { 0 } else { current.unsynced + 1 };
                    let committed = current.committed;
                    let first_success = !current.proven;
                    current.proven = true;
                    self.stats.fragments_written += 1;
                    self.stats.bytes_written += length;
                    if rewriting {
                        self.stats.fragments_rewritten += 1;
                    }
                    self.session.note_progress(committed);
                    if first_success {
                        self.session.mark_healthy();
                    }
                    return Ok(());
                }
                Err((op, error)) => {
                    let Some(mut failed) = self.current.take() else {
                        continue;
                    };
                    if let Err(truncate_error) = failed.sink.truncate(failed.committed) {
                        self.session
                            .record_incident(FailureOp::Truncate, &truncate_error);
                    }
                    let decision = self.session.record_failure(op, &error, failed.committed)?;
                    rewriting = true;
                    self.apply(decision)?;
                }
            }
        }
    }
}
