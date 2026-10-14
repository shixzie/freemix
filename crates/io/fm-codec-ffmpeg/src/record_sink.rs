//! Destinations for the recorder's fragmented MP4 byte stream.
//!
//! [`Recorder`](crate::record::Recorder) copies `FFmpeg`'s `pipe:1` output
//! into a [`RecordSink`] on its output worker. A plain [`File`] keeps the
//! historical single-file behaviour. [`ResilientOutput`] instead routes the
//! stream through `fm-record`'s fragment splitter into a
//! [`ResilientFmp4Writer`], so a write or sync failure on one directory
//! re-homes the in-flight fragment into a new segment on the next usable
//! fallback directory rather than failing the recording, and every segment is
//! independently playable and crash-recoverable through the session manifest.
//!
//! [`ResilientWavOutput`] does the same for the recorder's streamed WAV
//! sidecar through a [`ResilientWavWriter`]: audio is re-homed frame-exactly
//! into `<name>-NNNN.wav` on failure, every segment starts with the stream's
//! header, and header sizes are patched on close.

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use fm_record::{
    FailureKind, RecordingSession, ResilientFmp4Writer, ResilientWavConfig, ResilientWavWriter,
    ResilientWriterConfig, SegmentEntry, SegmentStorage, SessionError, SessionSnapshot, SplitError,
    WavError, WavWriterError, WavWriterFinish, WavWriterStats, WriterError, WriterFinish,
    WriterStats,
};

/// Destination for the recorder's muxed output.
///
/// The output worker calls [`Self::write_all`] for every chunk read from the
/// child until the first error, then [`Self::flush`] and [`Self::finish`]
/// exactly once when the child's output ends, including after a write error.
pub trait RecordSink: Send {
    /// Accepts the next chunk of the muxed stream.
    ///
    /// # Errors
    ///
    /// A failure is terminal for the recording; the worker stops writing.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Pushes buffered bytes towards the destination.
    ///
    /// # Errors
    ///
    /// Reported as an output flush failure.
    fn flush(&mut self) -> io::Result<()>;

    /// Makes everything written durable and closes the destination.
    ///
    /// # Errors
    ///
    /// Reported as an output sync failure.
    fn finish(&mut self) -> io::Result<()>;
}

impl<T: RecordSink + ?Sized> RecordSink for Box<T> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        (**self).write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        (**self).flush()
    }

    fn finish(&mut self) -> io::Result<()> {
        (**self).finish()
    }
}

impl RecordSink for File {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        Write::write_all(self, bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }

    fn finish(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}

/// Latest state of a [`ResilientOutput`], shared with its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResilientOutputState {
    pub stats: WriterStats,
    pub snapshot: SessionSnapshot,
    /// Manifest segment entries: index, root, state, and known-good bytes.
    pub segments: Vec<SegmentEntry>,
    /// Bytes of an incomplete trailing fragment discarded when the stream
    /// ended inside it (a killed or crashed muxer). Those bytes never reached
    /// a segment, so every segment stays playable.
    pub discarded_tail_bytes: u64,
    /// First terminal error, rendered.
    pub error: Option<String>,
    /// Whether [`RecordSink::finish`] stopped the session.
    pub finished: bool,
}

/// Cloneable read handle on a [`ResilientOutput`]'s state.
#[derive(Clone, Debug)]
pub struct ResilientOutputStatus(Arc<Mutex<ResilientOutputState>>);

impl ResilientOutputStatus {
    /// A consistent copy of the latest state.
    #[must_use]
    pub fn get(&self) -> ResilientOutputState {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, apply: impl FnOnce(&mut ResilientOutputState)) {
        apply(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// Fault-tolerant [`RecordSink`] over a recording session.
pub struct ResilientOutput<S: SegmentStorage> {
    writer: Option<ResilientFmp4Writer<S>>,
    status: ResilientOutputStatus,
    published: WriterStats,
}

impl<S: SegmentStorage> fmt::Debug for ResilientOutput<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResilientOutput")
            .field("status", &self.status.get())
            .finish_non_exhaustive()
    }
}

impl<S: SegmentStorage> ResilientOutput<S> {
    /// Wraps a started session. Nothing is written until the muxer's init
    /// segment and first fragment arrive.
    #[must_use]
    pub fn new(session: RecordingSession<S>, config: ResilientWriterConfig) -> Self {
        let state = ResilientOutputState {
            stats: WriterStats::default(),
            snapshot: session.snapshot(),
            segments: session.manifest().segments.clone(),
            discarded_tail_bytes: 0,
            error: None,
            finished: false,
        };
        Self {
            writer: Some(ResilientFmp4Writer::new(session, config)),
            status: ResilientOutputStatus(Arc::new(Mutex::new(state))),
            published: WriterStats::default(),
        }
    }

    /// Read handle that stays valid after the output moves into a recorder.
    #[must_use]
    pub fn status(&self) -> ResilientOutputStatus {
        self.status.clone()
    }

    /// Publishes counters on every call and the snapshot plus segment list
    /// only when segment placement changed, so the per-chunk cost stays a
    /// counter copy.
    fn publish(&mut self) {
        let Some(writer) = &self.writer else {
            return;
        };
        let stats = writer.stats();
        let placement_changed = stats.segments_opened != self.published.segments_opened
            || stats.failovers != self.published.failovers
            || stats.retries != self.published.retries;
        self.published = stats;
        let snapshot = placement_changed.then(|| writer.session().snapshot());
        let segments = placement_changed.then(|| writer.session().manifest().segments.clone());
        self.status.update(|state| {
            state.stats = stats;
            if let Some(snapshot) = snapshot {
                state.snapshot = snapshot;
            }
            if let Some(segments) = segments {
                state.segments = segments;
            }
        });
    }

    fn fail(&self, error: &WriterError) -> io::Error {
        let rendered = error.to_string();
        self.status.update(|state| {
            state.error.get_or_insert(rendered);
        });
        writer_io_error(error)
    }

    /// Publishes a finished writer's outcome. A stream that ended inside a
    /// fragment (a killed or crashed muxer) is not an output failure: the
    /// partial fragment never reached a segment and the session is stopped
    /// with every segment ending on a fragment boundary. It therefore never
    /// stands in for a failed session stop, which is reported instead; when a
    /// placement failure and a stop failure both occur, the placement failure
    /// picks the error kind and the rendered error names both.
    fn conclude(&self, finish: WriterFinish) -> io::Result<()> {
        let WriterFinish {
            stats,
            segments,
            snapshot,
            discarded_bytes,
            error,
        } = finish;
        let error = error
            .filter(|error| !matches!(error, WriterError::Stream(SplitError::Truncated { .. })));
        let (snapshot, failure) = match (snapshot, error) {
            (Ok(snapshot), None) => (Some(snapshot), None),
            (Ok(snapshot), Some(error)) => (
                Some(snapshot),
                Some((error.to_string(), writer_io_error(&error))),
            ),
            (Err(stop), None) => {
                let error = WriterError::Session(stop);
                (None, Some((error.to_string(), writer_io_error(&error))))
            }
            (Err(stop), Some(error)) => {
                let rendered = format!("{error}; stopping the session also failed: {stop}");
                let kind = writer_io_error(&error).kind();
                (
                    None,
                    Some((rendered.clone(), io::Error::new(kind, rendered))),
                )
            }
        };
        self.status.update(|state| {
            state.stats = stats;
            state.segments = segments;
            if let Some(snapshot) = snapshot {
                state.snapshot = snapshot;
            }
            state.discarded_tail_bytes = discarded_bytes;
            if let Some((rendered, _)) = &failure {
                state.error.get_or_insert_with(|| rendered.clone());
            }
            state.finished = true;
        });
        failure.map_or(Ok(()), |(_, error)| Err(error))
    }
}

impl<S> RecordSink for ResilientOutput<S>
where
    S: SegmentStorage + Send,
    S::Sink: Send,
{
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(io::Error::other("recording session already finished"));
        };
        let result = writer.push(bytes);
        self.publish();
        result.map_err(|error| self.fail(&error))
    }

    fn flush(&mut self) -> io::Result<()> {
        // Whole fragments are written and synced as they complete; a partial
        // fragment is only ever buffered in memory.
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        self.conclude(writer.finish_report())
    }
}

impl<S: SegmentStorage> Drop for ResilientOutput<S> {
    /// A sink dropped without [`RecordSink::finish`] (a recorder whose start
    /// failed before its output worker ran) still closes its session, so no
    /// manifest is left claiming a live recording.
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = self.conclude(writer.finish_report());
        }
    }
}

/// Latest state of a [`ResilientWavOutput`], shared with its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResilientWavOutputState {
    pub stats: WavWriterStats,
    pub snapshot: SessionSnapshot,
    /// Manifest segment entries: index, root, state, and known-good bytes.
    pub segments: Vec<SegmentEntry>,
    /// Bytes that never reached a segment: a trailing partial sample frame,
    /// or a header cut short by a killed or crashed muxer.
    pub discarded_tail_bytes: u64,
    /// First terminal error, rendered.
    pub error: Option<String>,
    /// Whether [`RecordSink::finish`] stopped the session.
    pub finished: bool,
}

/// Cloneable read handle on a [`ResilientWavOutput`]'s state.
#[derive(Clone, Debug)]
pub struct ResilientWavOutputStatus(Arc<Mutex<ResilientWavOutputState>>);

impl ResilientWavOutputStatus {
    /// A consistent copy of the latest state.
    #[must_use]
    pub fn get(&self) -> ResilientWavOutputState {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, apply: impl FnOnce(&mut ResilientWavOutputState)) {
        apply(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// Fault-tolerant [`RecordSink`] for the recorder's streamed WAV output,
/// over a session configured with [`fm_record::SegmentFormat::Wav`].
pub struct ResilientWavOutput<S: SegmentStorage> {
    writer: Option<ResilientWavWriter<S>>,
    status: ResilientWavOutputStatus,
    published: WavWriterStats,
}

impl<S: SegmentStorage> fmt::Debug for ResilientWavOutput<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResilientWavOutput")
            .field("status", &self.status.get())
            .finish_non_exhaustive()
    }
}

impl<S: SegmentStorage> ResilientWavOutput<S> {
    /// Wraps a started WAV session. Nothing is written until the stream
    /// header and first sample bytes arrive.
    #[must_use]
    pub fn new(session: RecordingSession<S>, config: ResilientWavConfig) -> Self {
        let state = ResilientWavOutputState {
            stats: WavWriterStats::default(),
            snapshot: session.snapshot(),
            segments: session.manifest().segments.clone(),
            discarded_tail_bytes: 0,
            error: None,
            finished: false,
        };
        Self {
            writer: Some(ResilientWavWriter::new(session, config)),
            status: ResilientWavOutputStatus(Arc::new(Mutex::new(state))),
            published: WavWriterStats::default(),
        }
    }

    /// Read handle that stays valid after the output moves into a recorder.
    #[must_use]
    pub fn status(&self) -> ResilientWavOutputStatus {
        self.status.clone()
    }

    fn publish(&mut self) {
        let Some(writer) = &self.writer else {
            return;
        };
        let stats = writer.stats();
        let placement_changed = stats.segments_opened != self.published.segments_opened
            || stats.failovers != self.published.failovers
            || stats.retries != self.published.retries;
        self.published = stats;
        let snapshot = placement_changed.then(|| writer.session().snapshot());
        let segments = placement_changed.then(|| writer.session().manifest().segments.clone());
        self.status.update(|state| {
            state.stats = stats;
            if let Some(snapshot) = snapshot {
                state.snapshot = snapshot;
            }
            if let Some(segments) = segments {
                state.segments = segments;
            }
        });
    }

    fn fail(&self, error: &WavWriterError) -> io::Error {
        let rendered = error.to_string();
        self.status.update(|state| {
            state.error.get_or_insert(rendered);
        });
        wav_writer_io_error(error)
    }

    /// Publishes a finished writer's outcome. A stream that ended inside its
    /// header (a muxer killed before any audio) is not an output failure;
    /// a failed session stop always is.
    fn conclude(&self, finish: WavWriterFinish) -> io::Result<()> {
        let WavWriterFinish {
            stats,
            segments,
            snapshot,
            discarded_bytes,
            error,
        } = finish;
        let error =
            error.filter(|error| !matches!(error, WavWriterError::Stream(WavError::Truncated)));
        let (snapshot, failure) = match (snapshot, error) {
            (Ok(snapshot), None) => (Some(snapshot), None),
            (Ok(snapshot), Some(error)) => (
                Some(snapshot),
                Some((error.to_string(), wav_writer_io_error(&error))),
            ),
            (Err(stop), None) => {
                let error = WavWriterError::Session(stop);
                (None, Some((error.to_string(), wav_writer_io_error(&error))))
            }
            (Err(stop), Some(error)) => {
                let rendered = format!("{error}; stopping the session also failed: {stop}");
                let kind = wav_writer_io_error(&error).kind();
                (
                    None,
                    Some((rendered.clone(), io::Error::new(kind, rendered))),
                )
            }
        };
        self.status.update(|state| {
            state.stats = stats;
            state.segments = segments;
            if let Some(snapshot) = snapshot {
                state.snapshot = snapshot;
            }
            state.discarded_tail_bytes = discarded_bytes;
            if let Some((rendered, _)) = &failure {
                state.error.get_or_insert_with(|| rendered.clone());
            }
            state.finished = true;
        });
        failure.map_or(Ok(()), |(_, error)| Err(error))
    }
}

impl<S> RecordSink for ResilientWavOutput<S>
where
    S: SegmentStorage + Send,
    S::Sink: Send,
{
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(io::Error::other("WAV recording session already finished"));
        };
        let result = writer.push(bytes);
        self.publish();
        result.map_err(|error| self.fail(&error))
    }

    fn flush(&mut self) -> io::Result<()> {
        // Whole frames are written as they arrive and synced on the writer's
        // interval; a partial frame is only ever buffered in memory.
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        self.conclude(writer.finish_report())
    }
}

impl<S: SegmentStorage> Drop for ResilientWavOutput<S> {
    /// A sink dropped without [`RecordSink::finish`] still closes its
    /// session, so no manifest is left claiming a live recording.
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = self.conclude(writer.finish_report());
        }
    }
}

/// Maps a writer failure onto an I/O error whose kind names the last
/// storage failure, so recorder telemetry stays path-free and typed.
fn writer_io_error(error: &WriterError) -> io::Error {
    let kind = match error {
        WriterError::Stream(_) => io::ErrorKind::InvalidData,
        WriterError::Session(error) => session_io_kind(error),
    };
    io::Error::new(kind, error.to_string())
}

fn wav_writer_io_error(error: &WavWriterError) -> io::Error {
    let kind = match error {
        WavWriterError::Stream(_) => io::ErrorKind::InvalidData,
        WavWriterError::Session(error) => session_io_kind(error),
    };
    io::Error::new(kind, error.to_string())
}

fn session_io_kind(error: &SessionError) -> io::ErrorKind {
    match error {
        SessionError::Exhausted { last: Some(record) } => failure_kind_io(record.kind),
        SessionError::Io { source, .. } => source.kind(),
        SessionError::ManifestUnwritable(kind) => *kind,
        _ => io::ErrorKind::Other,
    }
}

fn failure_kind_io(kind: FailureKind) -> io::ErrorKind {
    match kind {
        FailureKind::StorageFull => io::ErrorKind::StorageFull,
        FailureKind::QuotaExceeded => io::ErrorKind::QuotaExceeded,
        FailureKind::ReadOnly => io::ErrorKind::ReadOnlyFilesystem,
        FailureKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        FailureKind::FileTooLarge => io::ErrorKind::FileTooLarge,
        FailureKind::NotFound => io::ErrorKind::NotFound,
        FailureKind::AlreadyExists => io::ErrorKind::AlreadyExists,
        FailureKind::Interrupted => io::ErrorKind::Interrupted,
        FailureKind::Other => io::ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_record::{FileStorage, SessionConfig, UnknownSpace};

    fn output(directory: &std::path::Path) -> ResilientOutput<FileStorage<UnknownSpace>> {
        let config = SessionConfig::new("conclude", directory).unwrap();
        let session = RecordingSession::start(config, FileStorage::new(UnknownSpace)).unwrap();
        ResilientOutput::new(session, ResilientWriterConfig::default())
    }

    #[test]
    fn a_truncated_stream_does_not_mask_a_failed_session_stop() {
        let directory = tempfile::tempdir().unwrap();
        let mut output = output(directory.path());
        let mut finish = output.writer.take().unwrap().finish_report();
        finish.snapshot = Err(SessionError::ManifestUnwritable(
            io::ErrorKind::ReadOnlyFilesystem,
        ));
        finish.error = Some(WriterError::Stream(SplitError::Truncated { buffered: 7 }));
        finish.discarded_bytes = 7;

        let error = output.conclude(finish).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ReadOnlyFilesystem);
        let state = output.status().get();
        assert!(state.finished);
        assert_eq!(state.discarded_tail_bytes, 7);
        let rendered = state.error.expect("the stop failure is reported");
        assert!(rendered.contains("manifest"), "{rendered}");
    }

    #[test]
    fn a_placement_failure_and_a_failed_session_stop_are_both_reported() {
        let directory = tempfile::tempdir().unwrap();
        let mut output = output(directory.path());
        let mut finish = output.writer.take().unwrap().finish_report();
        finish.snapshot = Err(SessionError::InvalidState("close the open segment first"));
        finish.error = Some(WriterError::Session(SessionError::ManifestUnwritable(
            io::ErrorKind::StorageFull,
        )));

        let error = output.conclude(finish).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        let rendered = output.status().get().error.unwrap();
        assert!(
            rendered.contains("close the open segment first"),
            "{rendered}"
        );
        assert_eq!(error.to_string(), rendered);
    }

    #[test]
    fn a_truncated_stream_with_a_stopped_session_is_not_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut output = output(directory.path());
        let mut finish = output.writer.take().unwrap().finish_report();
        finish.error = Some(WriterError::Stream(SplitError::Truncated { buffered: 3 }));

        output.conclude(finish).unwrap();
        assert!(output.status().get().error.is_none());
    }
}
