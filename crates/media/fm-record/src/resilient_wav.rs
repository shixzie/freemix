//! Fault-tolerant WAV writer over a [`RecordingSession`].
//!
//! [`ResilientWavWriter`] accepts a streaming muxer's RIFF/WAVE byte stream
//! (for example `FFmpeg`'s `-f wav` output on a non-seekable destination,
//! whose header carries placeholder sizes). It parses the header once, then
//! appends whole sample frames to the session's current segment. Every
//! segment starts with a copy of the header, so every segment file is an
//! independently playable WAV.
//!
//! Audio written since the last `sync_data` is also held in memory. A sync is
//! issued after roughly [`ResilientWavConfig::sync_interval`] of audio. When a
//! write, sync, or segment creation fails, the writer truncates the failed
//! segment back to its last synced frame, patches that segment's header sizes
//! (best effort), lets the session's failure policy pick the same root or a
//! fallback, opens `<name>-NNNN.wav` there, and writes the header plus the
//! held audio again. A failure therefore costs no audio while any root
//! remains usable, and the session reports [`SessionError::Exhausted`] once
//! none does.
//!
//! Closing a segment rewrites its RIFF, `ds64`, `fact`, and `data` sizes in
//! place. A crash leaves placeholder or stale sizes, which
//! [`recover_wav_session`] repairs through [`recover_wav_file`]. A segment
//! whose header reserves no RF64 space rotates before it outgrows RIFF sizes.

use std::{io, num::NonZeroU64, path::PathBuf, time::Duration};

use crate::{
    session::{
        FailureDecision, FailureOp, ManifestRead, RecordingSession, SegmentEntry, SegmentFormat,
        SegmentPresence, SegmentSink, SegmentStorage, SessionConfig, SessionError, SessionManifest,
        SessionSnapshot, SessionState, rewrite_recovered_manifest, scan_session, stat_segment,
    },
    wav::{
        LargeFilePolicy, MAX_RIFF_SIZE, WavError, WavInfo, WavRecovery, read_wav_info,
        recover_wav_file, sized_wav_header,
    },
};

/// Default bound on the stream header before the `data` chunk.
pub const DEFAULT_MAX_WAV_HEADER_BYTES: usize = 64 * 1024;

/// Writer limits and durability policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResilientWavConfig {
    /// Largest stream header (everything before the sample data) accepted.
    pub max_header_bytes: usize,
    /// Audio between `sync_data` calls. Audio since the last sync is held in
    /// memory so a failure can rewrite it into the next segment. Rounded up
    /// to whole frames, and never below one frame.
    pub sync_interval: Duration,
    /// Rotate before a segment's sample data would exceed this many bytes
    /// (rounded down to whole frames, at least one frame). Segments whose
    /// header reserves no RF64 space also rotate at the RIFF size limit.
    pub max_segment_data_bytes: Option<NonZeroU64>,
}

impl Default for ResilientWavConfig {
    fn default() -> Self {
        Self {
            max_header_bytes: DEFAULT_MAX_WAV_HEADER_BYTES,
            sync_interval: Duration::from_secs(1),
            max_segment_data_bytes: None,
        }
    }
}

/// Counters for one WAV writer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WavWriterStats {
    /// Sample frames accepted into a segment, each counted once.
    pub frames_written: u64,
    /// Sample bytes accepted into a segment, each counted once.
    pub data_bytes: u64,
    /// Held sample bytes written again into a new segment after a failure.
    pub bytes_rewritten: u64,
    /// Bytes landed in segments, including header copies and rewrites.
    pub bytes_written: u64,
    pub segments_opened: u64,
    pub retries: u64,
    pub failovers: u64,
    pub syncs: u64,
    /// Segments whose header sizes could not be patched in place; recovery
    /// repairs them.
    pub header_patch_failures: u64,
}

#[derive(Debug)]
pub enum WavWriterError {
    /// The source stream is not a supported WAV stream.
    Stream(WavError),
    /// The session refused or every root failed.
    Session(SessionError),
}

impl std::fmt::Display for WavWriterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(error) => write!(formatter, "WAV recording stream: {error}"),
            Self::Session(error) => write!(formatter, "WAV recording session: {error}"),
        }
    }
}

impl std::error::Error for WavWriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stream(error) => Some(error),
            Self::Session(error) => Some(error),
        }
    }
}

impl From<WavError> for WavWriterError {
    fn from(error: WavError) -> Self {
        Self::Stream(error)
    }
}

impl From<SessionError> for WavWriterError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// Everything [`ResilientWavWriter::finish_report`] observed while ending a
/// stream.
#[derive(Debug)]
pub struct WavWriterFinish {
    pub stats: WavWriterStats,
    /// Final manifest segment entries.
    pub segments: Vec<SegmentEntry>,
    /// The stopped session, or why it could not be stopped.
    pub snapshot: Result<SessionSnapshot, SessionError>,
    /// Bytes that never reached a segment: a trailing partial frame, or an
    /// incomplete header.
    pub discarded_bytes: u64,
    /// The first placement failure, else [`WavError::Truncated`] when the
    /// stream ended inside its header.
    pub error: Option<WavWriterError>,
}

#[derive(Debug)]
struct StreamHeader {
    /// Every byte before the sample data, ending with the `data` chunk
    /// header; copied to the start of every segment.
    bytes: Vec<u8>,
    info: WavInfo,
    block: usize,
    sync_bytes: usize,
    /// Largest whole-frame sample payload of one segment.
    data_limit: u64,
}

#[derive(Debug)]
struct Current<K> {
    sink: K,
    /// Sample bytes in the segment.
    data: u64,
    /// Sample bytes known durable.
    synced: u64,
    /// Bytes written since the last sync, including the header.
    dirty: bool,
    proven: bool,
}

/// Frame-granular, failover-capable WAV segment writer.
#[derive(Debug)]
pub struct ResilientWavWriter<S: SegmentStorage> {
    session: RecordingSession<S>,
    config: ResilientWavConfig,
    header_buffer: Vec<u8>,
    stream: Option<StreamHeader>,
    /// A trailing partial frame awaiting its remaining bytes.
    partial: Vec<u8>,
    /// Sample bytes not yet known durable: written to the current segment
    /// when one is open, else waiting for the next segment.
    held: Vec<u8>,
    current: Option<Current<S::Sink>>,
    stats: WavWriterStats,
    rotate_requested: bool,
}

impl<S: SegmentStorage> ResilientWavWriter<S> {
    /// Wraps a started session, normally one configured with
    /// [`SegmentFormat::Wav`]. Nothing is written until the stream header and
    /// first sample bytes arrive.
    #[must_use]
    pub fn new(session: RecordingSession<S>, config: ResilientWavConfig) -> Self {
        Self {
            session,
            config,
            header_buffer: Vec::new(),
            stream: None,
            partial: Vec::new(),
            held: Vec::new(),
            current: None,
            stats: WavWriterStats::default(),
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
    pub fn stats(&self) -> WavWriterStats {
        self.stats
    }

    /// The parsed stream header, once it has arrived.
    #[must_use]
    pub fn info(&self) -> Option<&WavInfo> {
        self.stream.as_ref().map(|stream| &stream.info)
    }

    /// Sample bytes held in memory because they are not yet known durable.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        self.held.len()
    }

    /// Closes the current segment before the next sample frame; that frame
    /// opens a new segment.
    pub fn request_rotation(&mut self) {
        self.rotate_requested = true;
    }

    /// Feeds muxer output. Whole frames are written before returning.
    ///
    /// # Errors
    ///
    /// Returns [`WavWriterError::Stream`] for an unsupported or oversized
    /// header and [`WavWriterError::Session`] when the session cannot place
    /// audio on any root.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), WavWriterError> {
        if self.stream.is_some() {
            return self.accept(bytes);
        }
        self.header_buffer.extend_from_slice(bytes);
        let info = match read_wav_info(&mut io::Cursor::new(self.header_buffer.as_slice())) {
            Ok(info) => info,
            Err(WavError::Truncated) => {
                if self.header_buffer.len() > self.config.max_header_bytes {
                    return Err(WavError::Malformed("WAV header exceeds its bound").into());
                }
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let header_len = usize::try_from(info.data_offset)
            .ok()
            .filter(|length| *length <= self.config.max_header_bytes)
            .ok_or(WavError::Malformed("WAV header exceeds its bound"))?;
        let mut buffer = std::mem::take(&mut self.header_buffer);
        let samples = buffer.split_off(header_len);
        self.stream = Some(self.stream_header(buffer, info));
        self.accept(&samples)
    }

    /// Like [`Self::finish_report`], but returns the first failure.
    ///
    /// # Errors
    ///
    /// Returns the first placement failure, [`WavError::Truncated`] when the
    /// stream ended inside its header, or the session stop failure.
    pub fn finish(self) -> Result<SessionSnapshot, WavWriterError> {
        let finish = self.finish_report();
        if let Some(error) = finish.error {
            return Err(error);
        }
        finish.snapshot.map_err(WavWriterError::Session)
    }

    /// Ends the stream: drops a trailing partial frame, makes held audio
    /// durable, patches the last segment's header sizes, closes it, and stops
    /// the session, reporting the outcome instead of returning early. A
    /// stream with a header but no samples still yields one valid, empty
    /// segment.
    #[must_use]
    pub fn finish_report(mut self) -> WavWriterFinish {
        let (discarded_bytes, error) = if self.stream.is_none() {
            let buffered = self.header_buffer.len() as u64;
            let error = (buffered > 0).then_some(WavWriterError::Stream(WavError::Truncated));
            (buffered, error)
        } else {
            let mut placed = Ok(());
            if self.current.is_none() && self.stats.segments_opened == 0 {
                placed = self.ensure_segment();
            }
            let placed = placed.and_then(|()| self.close_current());
            (self.partial.len() as u64, placed.err())
        };
        let stats = self.stats;
        let segments = self.session.manifest().segments.clone();
        let snapshot = self.session.stop();
        WavWriterFinish {
            stats,
            segments,
            snapshot,
            discarded_bytes,
            error,
        }
    }

    fn stream_header(&self, bytes: Vec<u8>, info: WavInfo) -> StreamHeader {
        let block = usize::from(info.spec.block_align());
        let block64 = block as u64;
        let rate = u128::from(info.spec.sample_rate);
        let frames = (rate * self.config.sync_interval.as_nanos()).div_ceil(1_000_000_000);
        let sync_frames = usize::try_from(frames).unwrap_or(usize::MAX).max(1);
        let riff_limit = if info.spec.large_file == LargeFilePolicy::Rf64WhenNeeded {
            u64::MAX
        } else {
            // RIFF size = header after the RIFF chunk header + data + pad.
            (MAX_RIFF_SIZE + 8)
                .saturating_sub(bytes.len() as u64)
                .saturating_sub(1)
        };
        let configured = self
            .config
            .max_segment_data_bytes
            .map_or(u64::MAX, NonZeroU64::get);
        let data_limit = (riff_limit.min(configured) / block64 * block64).max(block64);
        StreamHeader {
            bytes,
            info,
            block,
            sync_bytes: sync_frames.saturating_mul(block),
            data_limit,
        }
    }

    fn accept(&mut self, mut bytes: &[u8]) -> Result<(), WavWriterError> {
        let Some(block) = self.stream.as_ref().map(|stream| stream.block) else {
            return Ok(());
        };
        if !self.partial.is_empty() {
            let take = (block - self.partial.len()).min(bytes.len());
            self.partial.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.partial.len() < block {
                return Ok(());
            }
            let frame = std::mem::take(&mut self.partial);
            self.write_frames(&frame)?;
        }
        let whole = bytes.len() - bytes.len() % block;
        self.write_frames(&bytes[..whole])?;
        self.partial.extend_from_slice(&bytes[whole..]);
        Ok(())
    }

    /// Writes whole frames, split so every sync lands exactly on a sync
    /// interval boundary of the stream and no segment outgrows its limit.
    fn write_frames(&mut self, mut bytes: &[u8]) -> Result<(), WavWriterError> {
        let Some((limit, sync_bytes)) = self
            .stream
            .as_ref()
            .map(|stream| (stream.data_limit, stream.sync_bytes))
        else {
            return Ok(());
        };
        while !bytes.is_empty() {
            if self.rotate_requested {
                self.rotate_requested = false;
                if self
                    .current
                    .as_ref()
                    .is_some_and(|current| current.data > 0)
                {
                    self.close_current()?;
                }
            }
            self.ensure_segment()?;
            let data = self.current.as_ref().map_or(0, |current| current.data);
            let room = limit.saturating_sub(data);
            if room == 0 {
                self.close_current()?;
                continue;
            }
            let until_sync = sync_bytes.saturating_sub(self.held.len()).max(1);
            let take = usize::try_from(room)
                .unwrap_or(usize::MAX)
                .min(until_sync)
                .min(bytes.len());
            self.write_chunk(&bytes[..take])?;
            bytes = &bytes[take..];
        }
        Ok(())
    }

    /// Appends whole frames that fit the open segment, failing over on error.
    fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), WavWriterError> {
        let length = chunk.len() as u64;
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
        let written = current.sink.write_all(chunk);
        self.held.extend_from_slice(chunk);
        match written {
            Ok(()) => {
                current.data += length;
                current.dirty = true;
                self.stats.bytes_written += length;
            }
            Err(error) => {
                // The chunk is held, so the next segment receives it again.
                self.fail_current(FailureOp::Write, &error)?;
                self.ensure_segment()?;
            }
        }
        self.stats.data_bytes += length;
        if let Some(stream) = &self.stream {
            self.stats.frames_written += length / stream.block as u64;
        }
        self.sync_if_due(false)
    }

    /// Syncs the open segment once enough audio is held, or always when
    /// `force`, failing over and rewriting held audio until a sync lands.
    fn sync_if_due(&mut self, force: bool) -> Result<(), WavWriterError> {
        let (header_len, sync_bytes) = match &self.stream {
            Some(stream) => (stream.bytes.len() as u64, stream.sync_bytes),
            None => return Ok(()),
        };
        loop {
            let Some(current) = self.current.as_mut() else {
                return Ok(());
            };
            if !current.dirty || (!force && self.held.len() < sync_bytes) {
                return Ok(());
            }
            match current.sink.sync_data() {
                Ok(()) => {
                    current.synced = current.data;
                    current.dirty = false;
                    let first = !current.proven;
                    current.proven = true;
                    let durable = header_len + current.synced;
                    self.held.clear();
                    self.stats.syncs += 1;
                    self.session.note_progress(durable);
                    if first {
                        self.session.mark_healthy();
                    }
                    // A preferred root re-enabled after running out of space
                    // takes the next segment.
                    if self.session.poll_space_restore() {
                        self.rotate_requested = true;
                    }
                    return Ok(());
                }
                Err(error) => {
                    self.fail_current(FailureOp::Sync, &error)?;
                    self.ensure_segment()?;
                }
            }
        }
    }

    /// Opens a segment and writes the header plus any held audio into it,
    /// retrying through the failure policy.
    fn ensure_segment(&mut self) -> Result<(), WavWriterError> {
        while self.current.is_none() {
            let Some(stream) = &self.stream else {
                return Ok(());
            };
            let mut opened = self.session.open_segment()?;
            self.stats.segments_opened += 1;
            let mut written = opened.sink.write_all(&stream.bytes);
            if written.is_ok() && !self.held.is_empty() {
                written = opened.sink.write_all(&self.held);
            }
            match written {
                Ok(()) => {
                    let held = self.held.len() as u64;
                    self.stats.bytes_written += stream.bytes.len() as u64 + held;
                    self.stats.bytes_rewritten += held;
                    self.session.note_progress(stream.bytes.len() as u64);
                    self.current = Some(Current {
                        sink: opened.sink,
                        data: held,
                        synced: 0,
                        dirty: true,
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

    /// Ends the open segment after a failure: truncates it to its last synced
    /// frame, patches its header sizes (best effort), and applies the failure
    /// policy. Held audio stays held for the next segment.
    fn fail_current(&mut self, op: FailureOp, error: &io::Error) -> Result<(), WavWriterError> {
        let Some(mut failed) = self.current.take() else {
            return Ok(());
        };
        let header_len = self
            .stream
            .as_ref()
            .map_or(0, |stream| stream.bytes.len() as u64);
        let mut valid = header_len + failed.synced;
        match failed.sink.truncate(valid) {
            Ok(()) => valid += self.patch_header(&mut failed.sink, failed.synced),
            Err(truncate_error) => {
                self.session
                    .record_incident(FailureOp::Truncate, &truncate_error);
            }
        }
        let decision = self.session.record_failure(op, error, valid)?;
        self.apply(decision)
    }

    /// Makes held audio durable, patches the header sizes, and closes the
    /// open segment.
    fn close_current(&mut self) -> Result<(), WavWriterError> {
        self.sync_if_due(true)?;
        let Some(mut current) = self.current.take() else {
            return Ok(());
        };
        let header_len = self
            .stream
            .as_ref()
            .map_or(0, |stream| stream.bytes.len() as u64);
        let pad = self.patch_header(&mut current.sink, current.data);
        // `close_segment` cannot fail here: a segment is open.
        let _ = self.session.close_segment(header_len + current.data + pad);
        Ok(())
    }

    /// Rewrites the header sizes for `data` sample bytes, appends the RIFF
    /// pad byte an odd length needs, and syncs. Returns the pad length. A
    /// failure is recorded as an incident; recovery repairs the sizes later.
    fn patch_header(&mut self, sink: &mut S::Sink, data: u64) -> u64 {
        let Some(stream) = &self.stream else {
            return 0;
        };
        let pad = data & 1;
        let result = sized_wav_header(&stream.bytes, data)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            .and_then(|sized| sink.write_at(0, &sized))
            .and_then(|()| {
                if pad == 1 {
                    sink.write_all(&[0])
                } else {
                    Ok(())
                }
            })
            .and_then(|()| sink.sync_data());
        match result {
            Ok(()) => pad,
            Err(error) => {
                self.stats.header_patch_failures += 1;
                self.session.record_incident(FailureOp::Write, &error);
                0
            }
        }
    }

    fn apply(&mut self, decision: FailureDecision) -> Result<(), WavWriterError> {
        match decision {
            FailureDecision::Retry { .. } => {
                self.stats.retries += 1;
                Ok(())
            }
            FailureDecision::Fallback { .. } => {
                self.stats.failovers += 1;
                Ok(())
            }
            FailureDecision::Exhausted => Err(WavWriterError::Session(SessionError::Exhausted {
                last: self.session.manifest().errors.last().copied(),
            })),
        }
    }
}

/// What WAV recovery did with one segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WavSegmentOutcome {
    /// The header already described the file; untouched.
    Intact {
        recovery: WavRecovery,
    },
    /// Sizes were rewritten from the data length and any partial trailing
    /// frame was dropped.
    Repaired {
        recovery: WavRecovery,
    },
    /// The header is missing or invalid; the file was left untouched.
    Unrecoverable {
        error: WavError,
        file_length: u64,
    },
    /// The manifest named an open segment whose file was never created.
    NeverCreated,
    /// The file is gone, or its root is no longer configured.
    Missing,
    Unreadable {
        kind: io::ErrorKind,
    },
}

/// Recovery result for one WAV segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredWavSegment {
    pub index: u32,
    pub root: usize,
    pub path: Option<PathBuf>,
    /// Manifest entry, or `None` for an orphan file adopted from disk.
    pub recorded: Option<SegmentEntry>,
    pub outcome: WavSegmentOutcome,
}

impl RecoveredWavSegment {
    fn recovery(&self) -> Option<&WavRecovery> {
        match &self.outcome {
            WavSegmentOutcome::Intact { recovery } | WavSegmentOutcome::Repaired { recovery } => {
                Some(recovery)
            }
            _ => None,
        }
    }

    /// Playable bytes after recovery, header included.
    #[must_use]
    pub fn kept_bytes(&self) -> u64 {
        self.recovery().map_or(0, |recovery| recovery.file_len)
    }

    /// Bytes that were on disk but are not playable.
    #[must_use]
    pub fn discarded_bytes(&self) -> u64 {
        match &self.outcome {
            WavSegmentOutcome::Repaired { recovery } => recovery.discarded_tail_bytes,
            WavSegmentOutcome::Unrecoverable { file_length, .. } => *file_length,
            _ => 0,
        }
    }

    /// Bytes the manifest recorded as durable that are no longer on disk.
    #[must_use]
    pub fn shortfall_bytes(&self) -> u64 {
        let recorded = self.recorded.map_or(0, |entry| entry.bytes);
        recorded.saturating_sub(self.kept_bytes())
    }

    #[must_use]
    pub fn recovered_frames(&self) -> u64 {
        self.recovery().map_or(0, |recovery| recovery.info.frames())
    }

    #[must_use]
    pub fn recovered_duration_nanos(&self) -> u64 {
        self.recovery().map_or(0, |recovery| {
            let rate = u128::from(recovery.info.spec.sample_rate.max(1));
            let nanos = u128::from(recovery.info.frames()) * 1_000_000_000 / rate;
            u64::try_from(nanos).unwrap_or(u64::MAX)
        })
    }

    /// Whether some loss in this segment has no exactly known span. A
    /// dropped partial trailing frame is not such a loss.
    #[must_use]
    pub fn has_unknown_loss(&self) -> bool {
        match &self.outcome {
            WavSegmentOutcome::Intact { .. }
            | WavSegmentOutcome::Repaired { .. }
            | WavSegmentOutcome::NeverCreated => self.shortfall_bytes() > 0,
            // An empty file the manifest never credited with bytes held
            // nothing: a segment that failed before its header landed.
            WavSegmentOutcome::Unrecoverable { file_length, .. } => {
                *file_length > 0 || self.shortfall_bytes() > 0
            }
            WavSegmentOutcome::Missing | WavSegmentOutcome::Unreadable { .. } => true,
        }
    }
}

/// Full WAV session crash-recovery report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavSessionRecovery {
    pub manifests: Vec<ManifestRead>,
    /// The manifest recovery trusted, if any copy was valid.
    pub manifest: Option<SessionManifest>,
    pub segments: Vec<RecoveredWavSegment>,
    /// Generation of the rewritten manifest, when one was written.
    pub rewritten_generation: Option<u64>,
    /// Roots that received the rewritten manifest.
    pub rewritten_roots: Vec<usize>,
}

impl WavSessionRecovery {
    /// Session state recorded before recovery.
    #[must_use]
    pub fn previous_state(&self) -> Option<SessionState> {
        self.manifest.as_ref().map(|manifest| manifest.state)
    }

    #[must_use]
    pub fn kept_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredWavSegment::kept_bytes)
            .sum()
    }

    #[must_use]
    pub fn discarded_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredWavSegment::discarded_bytes)
            .sum()
    }

    #[must_use]
    pub fn recovered_duration_nanos(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredWavSegment::recovered_duration_nanos)
            .sum()
    }

    /// Segments with loss whose span is not exactly known.
    #[must_use]
    pub fn segments_with_unknown_loss(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| segment.has_unknown_loss())
            .count()
    }
}

/// Recovers a crashed or interrupted [`SegmentFormat::Wav`] session.
///
/// Uses the same manifest selection and orphan adoption as
/// [`recover_session`](crate::recover_session), repairs every segment with
/// [`recover_wav_file`] (sizes recomputed from the data length, partial
/// trailing frame dropped), and rewrites the manifest in state
/// [`SessionState::Recovered`] to every root that exists.
///
/// # Errors
///
/// Returns [`SessionError::InvalidConfig`] for a non-WAV session,
/// [`SessionError::Locked`] while a live session or another recovery of the
/// same name holds a root, and an I/O error when a root directory listing fails for a reason other than the
/// directory being absent.
pub fn recover_wav_session(config: &SessionConfig) -> Result<WavSessionRecovery, SessionError> {
    if config.segment_format() != SegmentFormat::Wav {
        return Err(SessionError::InvalidConfig(
            "recover_wav_session needs a WAV session",
        ));
    }
    // Held until the rewritten manifest is in place.
    let _locks = crate::session::lock_session(config)?;
    let scan = scan_session(config, |path, recorded| {
        match stat_segment(path, recorded) {
            Ok(()) => {}
            Err(SegmentPresence::NeverCreated) => return WavSegmentOutcome::NeverCreated,
            Err(SegmentPresence::Missing) => return WavSegmentOutcome::Missing,
            Err(SegmentPresence::Unreadable(kind)) => {
                return WavSegmentOutcome::Unreadable { kind };
            }
        }
        match recover_wav_file(path) {
            Ok(recovery) if recovery.was_consistent => WavSegmentOutcome::Intact { recovery },
            Ok(recovery) => WavSegmentOutcome::Repaired { recovery },
            Err(WavError::Io(kind)) => WavSegmentOutcome::Unreadable { kind },
            Err(error) => WavSegmentOutcome::Unrecoverable {
                error,
                file_length: std::fs::metadata(path).map_or(0, |metadata| metadata.len()),
            },
        }
    })?;
    let segments: Vec<RecoveredWavSegment> = scan
        .segments
        .into_iter()
        .map(|segment| RecoveredWavSegment {
            index: segment.index,
            root: segment.root,
            path: segment.path,
            recorded: segment.recorded,
            outcome: segment.outcome,
        })
        .collect();
    let mut recovery = WavSessionRecovery {
        manifests: scan.manifests,
        manifest: scan.manifest,
        segments,
        rewritten_generation: None,
        rewritten_roots: Vec::new(),
    };
    if recovery.manifest.is_none() && recovery.segments.is_empty() {
        return Ok(recovery);
    }
    let entries = recovery
        .segments
        .iter()
        .filter(|segment| segment.outcome != WavSegmentOutcome::NeverCreated)
        .map(|segment| (segment.index, segment.root, segment.kept_bytes()))
        .collect();
    (recovery.rewritten_generation, recovery.rewritten_roots) = rewrite_recovered_manifest(
        config,
        &recovery.manifests,
        recovery.manifest.as_ref(),
        entries,
    );
    Ok(recovery)
}
