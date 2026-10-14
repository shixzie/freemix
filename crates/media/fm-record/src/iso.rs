//! Bounded ISO multi-tap orchestration with a deterministic text manifest.
//!
//! [`IsoTapSession`] records several selected camera or input feeds at once by
//! starting one [`RecorderCoordinator`] recorder per tap. The ordered tap
//! roster, each resolved start timecode, and the per-tap segment and queue
//! bounds are rendered into a bounded, human-readable `iso-manifest.txt`
//! document that parses back into exactly the same entries.

use std::{collections::HashSet, fmt, fs::OpenOptions, io::Write, num::NonZeroU128, path::PathBuf};

use fm_frame::NormalizedDuration;
use fm_types::{InputId, Timecode};

use crate::{
    ActionOutcome, ActionReceiptId, EnqueueError, IsoTapMetadata, QueueLimits, RecordEvent,
    RecorderConfig, RecorderCoordinator, RecorderError, RecorderId, RecorderSnapshot,
    SegmentPolicy, TimecodeSource, format,
};

/// Maximum number of ISO taps a single session can start.
pub const MAX_ISO_TAPS: usize = 8;

/// File name of the deterministic ISO manifest stored in the session root.
pub const ISO_MANIFEST_NAME: &str = "iso-manifest.txt";

const ISO_MANIFEST_MAGIC: &str = "fm-record-iso";
const ISO_MANIFEST_VERSION: u64 = 1;
const ISO_MANIFEST_FIELDS: usize = 11;
const MAX_ISO_MANIFEST_LINE_BYTES: usize = 512;
const START_RECEIPT_TAG: u8 = b'S';
const STOP_RECEIPT_TAG: u8 = b'T';

/// Configuration for one ISO tap in a multi-tap session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IsoTapConfig {
    /// Recorder that owns this tap's crash-safe spool.
    pub recorder: RecorderId,
    /// Raw video input recorded by this tap.
    pub video_input: InputId,
    /// Embedded or mapped audio input for this tap, when it records audio.
    pub audio_input: Option<InputId>,
    /// Timecode domain used for the tap's resolved start timecode.
    pub timecode_source: TimecodeSource,
    /// Source-original start timecode.
    ///
    /// Required for, and only valid with, [`TimecodeSource::SourceOriginal`].
    pub source_timecode: Option<Timecode>,
    /// Segment rotation policy for this tap.
    pub segments: SegmentPolicy,
    /// Bounded queue limits for this tap.
    pub queue: QueueLimits,
}

impl IsoTapConfig {
    /// Resolves this tap's manifest start timecode against the session master.
    fn resolved_timecode(self, master_timecode: Option<Timecode>) -> Option<Timecode> {
        match self.timecode_source {
            TimecodeSource::SharedMaster => master_timecode,
            TimecodeSource::SourceOriginal => self.source_timecode,
        }
    }

    fn recorder_config(self, start_timecode: Option<Timecode>) -> RecorderConfig {
        RecorderConfig {
            queue: self.queue,
            segments: self.segments,
            tap: IsoTapMetadata {
                video_input: Some(self.video_input),
                audio_input: self.audio_input,
                timecode_source: self.timecode_source,
                start_timecode,
            },
        }
    }
}

/// A live view of one session tap: its slot, configuration, and recorder state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsoTapSnapshot {
    slot: u8,
    config: IsoTapConfig,
    snapshot: RecorderSnapshot,
}

impl IsoTapSnapshot {
    /// Returns the 1-based slot of the tap.
    #[must_use]
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Returns the configuration the tap was started with.
    #[must_use]
    pub const fn config(&self) -> &IsoTapConfig {
        &self.config
    }

    /// Returns the underlying coordinator snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &RecorderSnapshot {
        &self.snapshot
    }
}

/// One parsed tap line of a deterministic ISO manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IsoManifestEntry {
    slot: u8,
    recorder: RecorderId,
    video_input: InputId,
    audio_input: Option<InputId>,
    timecode_source: TimecodeSource,
    start_timecode: Option<Timecode>,
    segments: SegmentPolicy,
    queue: QueueLimits,
}

impl IsoManifestEntry {
    /// Returns the 1-based slot of the tap.
    #[must_use]
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Returns the tap recorder.
    #[must_use]
    pub const fn recorder(&self) -> RecorderId {
        self.recorder
    }

    /// Returns the raw video input recorded by the tap.
    #[must_use]
    pub const fn video_input(&self) -> InputId {
        self.video_input
    }

    /// Returns the embedded or mapped audio input, when one is recorded.
    #[must_use]
    pub const fn audio_input(&self) -> Option<InputId> {
        self.audio_input
    }

    /// Returns the tap timecode domain.
    #[must_use]
    pub const fn timecode_source(&self) -> TimecodeSource {
        self.timecode_source
    }

    /// Returns the resolved start timecode, when one is known.
    #[must_use]
    pub const fn start_timecode(&self) -> Option<Timecode> {
        self.start_timecode
    }

    /// Returns the tap segment rotation policy.
    #[must_use]
    pub const fn segments(&self) -> SegmentPolicy {
        self.segments
    }

    /// Returns the tap queue limits.
    #[must_use]
    pub const fn queue(&self) -> QueueLimits {
        self.queue
    }
}

/// Errors from starting, addressing, or stopping an ISO multi-tap session.
#[derive(Debug)]
pub enum IsoTapError {
    /// The tap count is outside `1..=`[`MAX_ISO_TAPS`].
    InvalidTapCount {
        /// Requested tap count.
        count: usize,
    },
    /// Two taps address the same recorder.
    DuplicateRecorder(RecorderId),
    /// Two taps record the same raw video input.
    DuplicateVideoInput(InputId),
    /// A source-original tap has no source timecode.
    MissingSourceTimecode {
        /// 1-based slot of the offending tap.
        slot: u8,
    },
    /// A shared-master tap carries a per-tap source timecode.
    UnexpectedSourceTimecode {
        /// 1-based slot of the offending tap.
        slot: u8,
    },
    /// A tap's queue or segment policy is invalid.
    InvalidConfig {
        /// 1-based slot of the offending tap.
        slot: u8,
        /// Underlying recorder configuration error.
        source: RecorderError,
    },
    /// The requested 1-based slot is not part of this session.
    UnknownSlot {
        /// Requested slot.
        slot: u8,
    },
    /// The tap recorder rejected an event without accepting ownership.
    Enqueue {
        /// 1-based slot of the offending tap.
        slot: u8,
        /// Underlying non-blocking enqueue error.
        source: EnqueueError,
    },
    /// A tap recorder could not be started.
    Start {
        /// 1-based slot of the offending tap.
        slot: u8,
        /// Underlying coordinator error.
        source: RecorderError,
    },
    /// A recorder operation failed for the addressed tap.
    Recorder {
        /// 1-based slot of the offending tap.
        slot: u8,
        /// Underlying coordinator error.
        source: RecorderError,
    },
    /// Start receipts are only partially applied.
    MixedStartOutcome,
    /// The coordinator could not be opened.
    Coordinator(RecorderError),
    /// The ISO manifest could not be stored.
    Manifest(RecorderError),
}

impl fmt::Display for IsoTapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTapCount { count } => {
                write!(
                    formatter,
                    "ISO tap count {count} is outside 1..={MAX_ISO_TAPS}"
                )
            }
            Self::DuplicateRecorder(recorder) => {
                write!(formatter, "duplicate ISO recorder {recorder}")
            }
            Self::DuplicateVideoInput(input) => {
                write!(formatter, "duplicate ISO video input {input}")
            }
            Self::MissingSourceTimecode { slot } => write!(
                formatter,
                "ISO tap slot {slot} uses source-original timecode without a source timecode"
            ),
            Self::UnexpectedSourceTimecode { slot } => write!(
                formatter,
                "ISO tap slot {slot} uses shared-master timecode with a source timecode"
            ),
            Self::InvalidConfig { slot, source } => {
                write!(
                    formatter,
                    "ISO tap slot {slot} has an invalid config: {source}"
                )
            }
            Self::UnknownSlot { slot } => {
                write!(formatter, "ISO slot {slot} is not part of this session")
            }
            Self::Enqueue { slot, source } => write!(formatter, "ISO tap slot {slot}: {source}"),
            Self::Start { slot, source } => {
                write!(formatter, "ISO tap slot {slot} failed to start: {source}")
            }
            Self::Recorder { slot, source } => write!(formatter, "ISO tap slot {slot}: {source}"),
            Self::MixedStartOutcome => {
                formatter.write_str("ISO tap start receipts are only partially applied")
            }
            Self::Coordinator(source) => write!(formatter, "ISO coordinator: {source}"),
            Self::Manifest(source) => write!(formatter, "ISO manifest: {source}"),
        }
    }
}

impl std::error::Error for IsoTapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig { source, .. }
            | Self::Start { source, .. }
            | Self::Recorder { source, .. }
            | Self::Coordinator(source)
            | Self::Manifest(source) => Some(source),
            Self::Enqueue { source, .. } => Some(source),
            Self::InvalidTapCount { .. }
            | Self::DuplicateRecorder(_)
            | Self::DuplicateVideoInput(_)
            | Self::MissingSourceTimecode { .. }
            | Self::UnexpectedSourceTimecode { .. }
            | Self::UnknownSlot { .. }
            | Self::MixedStartOutcome => None,
        }
    }
}

/// Errors from parsing a deterministic ISO manifest.
///
/// Parsing is all-or-nothing: any invalid line rejects the whole document.
#[derive(Debug)]
pub enum IsoManifestError {
    /// The document has no `fm-record-iso` version header.
    InvalidHeader,
    /// The header version is not supported.
    UnsupportedVersion {
        /// Version found in the header.
        version: u64,
    },
    /// The document contains a header but no taps.
    NoTaps,
    /// The document lists more taps than [`MAX_ISO_TAPS`].
    TooManyLines {
        /// Number of tap lines found.
        lines: usize,
    },
    /// A line exceeds the manifest line length bound.
    LineTooLong {
        /// 1-based line number.
        line: usize,
    },
    /// A line has the wrong number of tab-separated fields.
    FieldCount {
        /// 1-based line number.
        line: usize,
        /// Number of fields found.
        count: usize,
    },
    /// A field is malformed.
    MalformedField {
        /// 1-based line number.
        line: usize,
        /// Name of the malformed field.
        field: &'static str,
    },
    /// A slot is outside `1..=`[`MAX_ISO_TAPS`].
    SlotOutOfRange {
        /// 1-based line number.
        line: usize,
        /// Slot value found.
        slot: u64,
    },
    /// A slot appears twice.
    DuplicateSlot {
        /// Repeated slot.
        slot: u8,
    },
    /// Slots are not strictly increasing.
    SlotsOutOfOrder {
        /// 1-based line number.
        line: usize,
        /// Slot of the previous line.
        previous: u8,
        /// Slot of this line.
        slot: u8,
    },
    /// Two lines name the same recorder.
    DuplicateRecorder {
        /// Repeated recorder.
        recorder: RecorderId,
    },
    /// Two lines name the same raw video input.
    DuplicateVideoInput {
        /// Repeated input.
        video_input: InputId,
    },
    /// A timecode source is neither `shared` nor `source`.
    UnknownTimecodeSource {
        /// 1-based line number.
        line: usize,
    },
    /// A zero identifier was found where a nonzero one is required.
    ZeroIdentifier {
        /// 1-based line number.
        line: usize,
        /// Name of the offending field.
        field: &'static str,
    },
    /// A bound is zero or does not fit this platform.
    InvalidBound {
        /// 1-based line number.
        line: usize,
        /// Name of the offending field.
        field: &'static str,
    },
    /// A source-original tap has no start timecode.
    MissingSourceTimecode {
        /// 1-based line number.
        line: usize,
    },
}

impl fmt::Display for IsoManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader => formatter.write_str("invalid ISO manifest header"),
            Self::UnsupportedVersion { version } => {
                write!(formatter, "unsupported ISO manifest version {version}")
            }
            Self::NoTaps => formatter.write_str("ISO manifest lists no taps"),
            Self::TooManyLines { lines } => write!(
                formatter,
                "ISO manifest lists {lines} taps, more than {MAX_ISO_TAPS}"
            ),
            Self::LineTooLong { line } => {
                write!(formatter, "ISO manifest line {line} is too long")
            }
            Self::FieldCount { line, count } => write!(
                formatter,
                "ISO manifest line {line} has {count} fields, expected {ISO_MANIFEST_FIELDS}"
            ),
            Self::MalformedField { line, field } => {
                write!(
                    formatter,
                    "ISO manifest line {line} has a malformed {field}"
                )
            }
            Self::SlotOutOfRange { line, slot } => write!(
                formatter,
                "ISO manifest line {line} slot {slot} is outside 1..={MAX_ISO_TAPS}"
            ),
            Self::DuplicateSlot { slot } => {
                write!(formatter, "ISO manifest repeats slot {slot}")
            }
            Self::SlotsOutOfOrder {
                line,
                previous,
                slot,
            } => write!(
                formatter,
                "ISO manifest line {line} slot {slot} does not follow slot {previous}"
            ),
            Self::DuplicateRecorder { recorder } => {
                write!(formatter, "ISO manifest repeats recorder {recorder}")
            }
            Self::DuplicateVideoInput { video_input } => {
                write!(formatter, "ISO manifest repeats video input {video_input}")
            }
            Self::UnknownTimecodeSource { line } => {
                write!(
                    formatter,
                    "ISO manifest line {line} has an unknown timecode source"
                )
            }
            Self::ZeroIdentifier { line, field } => {
                write!(formatter, "ISO manifest line {line} has a zero {field}")
            }
            Self::InvalidBound { line, field } => {
                write!(formatter, "ISO manifest line {line} has an invalid {field}")
            }
            Self::MissingSourceTimecode { line } => write!(
                formatter,
                "ISO manifest line {line} uses source timecode without a start timecode"
            ),
        }
    }
}

impl std::error::Error for IsoManifestError {}

/// An ordered, bounded multi-tap ISO recording session.
///
/// The session owns the [`RecorderCoordinator`] and its tap registry. Tap
/// order is start order, and every public operation addresses a tap by its
/// 1-based slot.
pub struct IsoTapSession {
    coordinator: RecorderCoordinator,
    taps: Vec<IsoTapConfig>,
    start_timecodes: Vec<Option<Timecode>>,
}

impl IsoTapSession {
    /// Starts every tap in slot order and durably stores the ISO manifest.
    ///
    /// The tap roster is validated and every manifest timecode is resolved
    /// before the coordinator touches the filesystem. Shared-master taps
    /// resolve to `master_timecode`; source-original taps use their own
    /// [`IsoTapConfig::source_timecode`].
    ///
    /// If every tap's start receipt was already applied, the session is still
    /// built and [`ActionOutcome::AlreadyApplied`] is returned. A mixture of
    /// applied and fresh receipts is rejected with
    /// [`IsoTapError::MixedStartOutcome`].
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError`] when the roster is invalid, the coordinator
    /// cannot be opened, a recorder cannot start, the start receipts are only
    /// partially applied, or the manifest cannot be stored. A start that fails
    /// after some taps were started stops those taps again, best effort, before
    /// returning the original error, so no recorder is left live by a failed
    /// session start.
    pub fn start(
        root: impl Into<PathBuf>,
        master_timecode: Option<Timecode>,
        taps: &[IsoTapConfig],
    ) -> Result<(Self, ActionOutcome), IsoTapError> {
        validate_taps(taps)?;
        let resolved = taps
            .iter()
            .map(|tap| tap.resolved_timecode(master_timecode))
            .collect::<Vec<_>>();
        let mut coordinator = RecorderCoordinator::new(root).map_err(IsoTapError::Coordinator)?;

        let mut outcomes = Vec::with_capacity(taps.len());
        for (index, (tap, start_timecode)) in taps.iter().zip(&resolved).enumerate() {
            let slot = slot_for_index(index);
            let receipt = derived_receipt(START_RECEIPT_TAG, slot, tap.recorder);
            match coordinator.start(tap.recorder, receipt, tap.recorder_config(*start_timecode)) {
                Ok(outcome) => outcomes.push(outcome),
                Err(source) => {
                    rollback(&mut coordinator, taps, index);
                    return Err(IsoTapError::Start { slot, source });
                }
            }
        }

        let mut session = Self {
            coordinator,
            taps: taps.to_vec(),
            start_timecodes: resolved,
        };
        let Some(outcome) = combined_start_outcome(&outcomes) else {
            session.rollback_all();
            return Err(IsoTapError::MixedStartOutcome);
        };
        if let Err(source) = session.write_manifest() {
            session.rollback_all();
            return Err(IsoTapError::Manifest(source));
        }
        Ok((session, outcome))
    }

    /// Enqueues one event for the tap in the given 1-based slot.
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError::UnknownSlot`] for a slot outside this session,
    /// or [`IsoTapError::Enqueue`] when the tap recorder rejects the event.
    pub fn record(&mut self, slot: u8, event: RecordEvent) -> Result<(), IsoTapError> {
        let index = self.index_for_slot(slot)?;
        self.coordinator
            .enqueue(self.taps[index].recorder, event)
            .map_err(|source| IsoTapError::Enqueue { slot, source })
    }

    /// Durably drains the queue of the tap in the given 1-based slot.
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError::UnknownSlot`] for a slot outside this session,
    /// or [`IsoTapError::Recorder`] when the tap cannot be flushed.
    pub fn flush(&mut self, slot: u8) -> Result<usize, IsoTapError> {
        let index = self.index_for_slot(slot)?;
        self.coordinator
            .flush(self.taps[index].recorder)
            .map_err(|source| IsoTapError::Recorder { slot, source })
    }

    /// Durably drains every tap in slot order, stopping at the first failure.
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError::Recorder`] for the first tap that cannot flush.
    pub fn flush_all(&mut self) -> Result<usize, IsoTapError> {
        let mut written = 0usize;
        for (index, tap) in self.taps.iter().enumerate() {
            let slot = slot_for_index(index);
            written = written.saturating_add(
                self.coordinator
                    .flush(tap.recorder)
                    .map_err(|source| IsoTapError::Recorder { slot, source })?,
            );
        }
        Ok(written)
    }

    /// Flushes, closes, and durably stores the stop receipt of one tap.
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError::UnknownSlot`] for a slot outside this session,
    /// or [`IsoTapError::Recorder`] when the tap cannot be stopped.
    pub fn stop(
        &mut self,
        slot: u8,
        receipt: ActionReceiptId,
    ) -> Result<ActionOutcome, IsoTapError> {
        let index = self.index_for_slot(slot)?;
        self.coordinator
            .stop(self.taps[index].recorder, receipt)
            .map_err(|source| IsoTapError::Recorder { slot, source })
    }

    /// Stops every tap in slot order with deterministic receipts.
    ///
    /// Returns [`ActionOutcome::AlreadyApplied`] when every tap's stop receipt
    /// was already applied.
    ///
    /// # Errors
    ///
    /// Returns [`IsoTapError::Recorder`] for the first tap that cannot stop.
    pub fn stop_all(&mut self) -> Result<ActionOutcome, IsoTapError> {
        let mut applied = false;
        for (index, tap) in self.taps.iter().enumerate() {
            let slot = slot_for_index(index);
            let receipt = derived_receipt(STOP_RECEIPT_TAG, slot, tap.recorder);
            match self
                .coordinator
                .stop(tap.recorder, receipt)
                .map_err(|source| IsoTapError::Recorder { slot, source })?
            {
                ActionOutcome::Applied => applied = true,
                ActionOutcome::AlreadyApplied => {}
            }
        }
        Ok(if applied {
            ActionOutcome::Applied
        } else {
            ActionOutcome::AlreadyApplied
        })
    }

    /// Returns the underlying coordinator.
    #[must_use]
    pub const fn coordinator(&self) -> &RecorderCoordinator {
        &self.coordinator
    }

    /// Returns one snapshot per tap in slot order.
    #[must_use]
    pub fn tap_snapshots(&self) -> Vec<IsoTapSnapshot> {
        self.taps
            .iter()
            .enumerate()
            .filter_map(|(index, tap)| {
                self.coordinator
                    .snapshot(tap.recorder)
                    .map(|snapshot| IsoTapSnapshot {
                        slot: slot_for_index(index),
                        config: *tap,
                        snapshot,
                    })
            })
            .collect()
    }

    /// Returns the resolved start timecode of the tap in the given 1-based
    /// slot, when the slot exists and a timecode was resolved.
    #[must_use]
    pub fn timecode_for(&self, slot: u8) -> Option<Timecode> {
        let index = usize::from(slot).checked_sub(1)?;
        self.start_timecodes.get(index).copied().flatten()
    }

    /// Renders the deterministic ISO manifest for this session.
    #[must_use]
    pub fn render_iso_manifest(&self) -> String {
        render_iso_manifest_entries(&self.manifest_entries())
    }

    fn manifest_entries(&self) -> Vec<IsoManifestEntry> {
        self.taps
            .iter()
            .zip(&self.start_timecodes)
            .enumerate()
            .map(|(index, (tap, start_timecode))| IsoManifestEntry {
                slot: slot_for_index(index),
                recorder: tap.recorder,
                video_input: tap.video_input,
                audio_input: tap.audio_input,
                timecode_source: tap.timecode_source,
                start_timecode: *start_timecode,
                segments: tap.segments,
                queue: tap.queue,
            })
            .collect()
    }

    fn index_for_slot(&self, slot: u8) -> Result<usize, IsoTapError> {
        let index = usize::from(slot)
            .checked_sub(1)
            .ok_or(IsoTapError::UnknownSlot { slot })?;
        if index >= self.taps.len() {
            return Err(IsoTapError::UnknownSlot { slot });
        }
        Ok(index)
    }

    fn rollback_all(&mut self) {
        for (index, tap) in self.taps.iter().enumerate() {
            stop_best_effort(&mut self.coordinator, index, *tap);
        }
    }

    fn write_manifest(&self) -> Result<(), RecorderError> {
        let path = self.coordinator.root().join(ISO_MANIFEST_NAME);
        let rendered = self.render_iso_manifest();
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|error| RecorderError::io(&path, error))?;
        file.write_all(rendered.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| RecorderError::io(&path, error))?;
        format::sync_directory(self.coordinator.root())
    }
}

/// Renders a complete ISO manifest document from parsed or authored entries.
///
/// The document starts with the `fm-record-iso` magic and version header and
/// then writes one tab-separated line per entry. Entries are rendered as given;
/// use [`parse_iso_manifest`] for untrusted input.
#[must_use]
pub fn render_iso_manifest_entries(entries: &[IsoManifestEntry]) -> String {
    let mut output = String::with_capacity(entries.len().saturating_add(1).saturating_mul(128));
    output.push_str(ISO_MANIFEST_MAGIC);
    output.push('\t');
    output.push_str(&ISO_MANIFEST_VERSION.to_string());
    output.push('\n');
    for entry in entries {
        output.push_str(&render_iso_manifest_line(entry));
        output.push('\n');
    }
    output
}

/// Parses a bounded ISO manifest document back into ordered entries.
///
/// # Errors
///
/// Returns [`IsoManifestError`] when the header is missing or malformed, the
/// version is unsupported, the document holds no taps or more than
/// [`MAX_ISO_TAPS`] taps, a line is too long, a line has the wrong number of
/// fields, a numeric field or start timecode is malformed, a bound is zero or
/// too large for this platform, a source timecode is missing, or the slots,
/// recorders, or video inputs are not unique.
pub fn parse_iso_manifest(text: &str) -> Result<Vec<IsoManifestEntry>, IsoManifestError> {
    let document = text.strip_suffix('\n').unwrap_or(text);
    let Some(header) = document.split('\n').next() else {
        return Err(IsoManifestError::InvalidHeader);
    };
    if header.len() > MAX_ISO_MANIFEST_LINE_BYTES {
        return Err(IsoManifestError::LineTooLong { line: 1 });
    }
    parse_iso_manifest_header(header)?;

    let tap_lines = document.split('\n').count().saturating_sub(1);
    if tap_lines > MAX_ISO_TAPS {
        return Err(IsoManifestError::TooManyLines { lines: tap_lines });
    }
    let mut entries = Vec::with_capacity(tap_lines);
    let mut recorders = HashSet::with_capacity(tap_lines);
    let mut video_inputs = HashSet::with_capacity(tap_lines);
    for (index, line) in document.split('\n').skip(1).enumerate() {
        let line_number = index.saturating_add(2);
        if line.len() > MAX_ISO_MANIFEST_LINE_BYTES {
            return Err(IsoManifestError::LineTooLong { line: line_number });
        }
        let entry = parse_iso_manifest_line(
            line,
            line_number,
            &mut recorders,
            &mut video_inputs,
            entries.last(),
        )?;
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(IsoManifestError::NoTaps);
    }
    Ok(entries)
}

fn parse_iso_manifest_header(header: &str) -> Result<(), IsoManifestError> {
    let mut fields = header.split('\t');
    if fields.next() != Some(ISO_MANIFEST_MAGIC) {
        return Err(IsoManifestError::InvalidHeader);
    }
    let Some(version) = fields.next() else {
        return Err(IsoManifestError::InvalidHeader);
    };
    if fields.next().is_some() {
        return Err(IsoManifestError::InvalidHeader);
    }
    let version = version
        .parse::<u64>()
        .map_err(|_| IsoManifestError::InvalidHeader)?;
    if version != ISO_MANIFEST_VERSION {
        return Err(IsoManifestError::UnsupportedVersion { version });
    }
    Ok(())
}

fn parse_iso_manifest_line(
    line: &str,
    line_number: usize,
    recorders: &mut HashSet<RecorderId>,
    video_inputs: &mut HashSet<InputId>,
    previous: Option<&IsoManifestEntry>,
) -> Result<IsoManifestEntry, IsoManifestError> {
    let mut fields = ManifestFields::new(line, line_number);
    let slot = parse_slot(fields.take()?, line_number)?;
    match previous.map(IsoManifestEntry::slot) {
        Some(previous) if slot == previous => return Err(IsoManifestError::DuplicateSlot { slot }),
        Some(previous) if slot < previous => {
            return Err(IsoManifestError::SlotsOutOfOrder {
                line: line_number,
                previous,
                slot,
            });
        }
        _ => {}
    }
    let recorder = parse_recorder(fields.take()?, line_number)?;
    if !recorders.insert(recorder) {
        return Err(IsoManifestError::DuplicateRecorder { recorder });
    }
    let video_input = parse_video_input(fields.take()?, line_number)?;
    if !video_inputs.insert(video_input) {
        return Err(IsoManifestError::DuplicateVideoInput { video_input });
    }
    let audio_input = parse_audio_input(fields.take()?, line_number)?;
    let timecode_source = parse_timecode_source(fields.take()?, line_number)?;
    let start_timecode = parse_start_timecode(fields.take()?, line_number)?;
    if timecode_source == TimecodeSource::SourceOriginal && start_timecode.is_none() {
        return Err(IsoManifestError::MissingSourceTimecode { line: line_number });
    }
    let segments = SegmentPolicy {
        max_frames: parse_optional_bound(fields.take()?, line_number, "segment_max_frames")?,
        max_bytes: parse_optional_bound(fields.take()?, line_number, "segment_max_bytes")?,
        max_duration: parse_optional_bound(fields.take()?, line_number, "segment_max_duration")?
            .map(NormalizedDuration::from_nanos)
            .transpose()
            .map_err(|_| IsoManifestError::InvalidBound {
                line: line_number,
                field: "segment_max_duration",
            })?,
    };
    let queue = QueueLimits {
        max_events: parse_queue_bound(fields.take()?, line_number, "queue_max_events")?,
        max_bytes: parse_queue_bound(fields.take()?, line_number, "queue_max_bytes")?,
    };
    fields.finish()?;
    Ok(IsoManifestEntry {
        slot,
        recorder,
        video_input,
        audio_input,
        timecode_source,
        start_timecode,
        segments,
        queue,
    })
}

fn parse_slot(field: &str, line: usize) -> Result<u8, IsoManifestError> {
    let slot = field
        .parse::<u64>()
        .map_err(|_| IsoManifestError::MalformedField {
            line,
            field: "slot",
        })?;
    if slot == 0 || slot > u64::try_from(MAX_ISO_TAPS).unwrap_or(u64::MAX) {
        return Err(IsoManifestError::SlotOutOfRange { line, slot });
    }
    u8::try_from(slot).map_err(|_| IsoManifestError::SlotOutOfRange { line, slot })
}

fn parse_recorder(field: &str, line: usize) -> Result<RecorderId, IsoManifestError> {
    let value = field
        .parse::<u128>()
        .map_err(|_| IsoManifestError::MalformedField {
            line,
            field: "recorder",
        })?;
    NonZeroU128::new(value)
        .map(RecorderId::new)
        .ok_or(IsoManifestError::ZeroIdentifier {
            line,
            field: "recorder",
        })
}

fn parse_video_input(field: &str, line: usize) -> Result<InputId, IsoManifestError> {
    parse_input_id(field, line, "video_input")
}

fn parse_audio_input(field: &str, line: usize) -> Result<Option<InputId>, IsoManifestError> {
    if field == "none" {
        return Ok(None);
    }
    parse_input_id(field, line, "audio_input").map(Some)
}

fn parse_input_id(
    field: &str,
    line: usize,
    name: &'static str,
) -> Result<InputId, IsoManifestError> {
    let value = field
        .parse::<u128>()
        .map_err(|_| IsoManifestError::MalformedField { line, field: name })?;
    NonZeroU128::new(value)
        .map(InputId::new)
        .ok_or(IsoManifestError::ZeroIdentifier { line, field: name })
}

fn parse_timecode_source(field: &str, line: usize) -> Result<TimecodeSource, IsoManifestError> {
    match field {
        "shared" => Ok(TimecodeSource::SharedMaster),
        "source" => Ok(TimecodeSource::SourceOriginal),
        _ => Err(IsoManifestError::UnknownTimecodeSource { line }),
    }
}

fn parse_start_timecode(field: &str, line: usize) -> Result<Option<Timecode>, IsoManifestError> {
    if field == "none" {
        return Ok(None);
    }
    parse_manifest_timecode(field)
        .map(Some)
        .ok_or(IsoManifestError::MalformedField {
            line,
            field: "start_timecode",
        })
}

/// Parses a strict `HH:MM:SS:FF` timecode in the exact `{:02}` rendering.
///
/// A timecode carries no frame-rate domain, so the nominal rate used to bound
/// the frame component is `u8::MAX`. High-rate frame values of 100 or more
/// render with three digits and are accepted in that canonical form only.
fn parse_manifest_timecode(field: &str) -> Option<Timecode> {
    let mut components = field.split(':');
    let hours = parse_timecode_component(components.next()?)?;
    let minutes = parse_timecode_component(components.next()?)?;
    let seconds = parse_timecode_component(components.next()?)?;
    let frames = parse_timecode_component(components.next()?)?;
    if components.next().is_some() {
        return None;
    }
    Timecode::new(hours, minutes, seconds, frames, u8::MAX).ok()
}

fn parse_timecode_component(component: &str) -> Option<u8> {
    if !(2..=3).contains(&component.len()) || !component.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = component.parse::<u8>().ok()?;
    (format!("{value:02}") == component).then_some(value)
}

fn parse_optional_bound(
    field: &str,
    line: usize,
    name: &'static str,
) -> Result<Option<u64>, IsoManifestError> {
    if field == "none" {
        return Ok(None);
    }
    let value = field
        .parse::<u64>()
        .map_err(|_| IsoManifestError::MalformedField { line, field: name })?;
    if value == 0 {
        return Err(IsoManifestError::InvalidBound { line, field: name });
    }
    Ok(Some(value))
}

fn parse_queue_bound(
    field: &str,
    line: usize,
    name: &'static str,
) -> Result<usize, IsoManifestError> {
    let value = field
        .parse::<u64>()
        .map_err(|_| IsoManifestError::MalformedField { line, field: name })?;
    if value == 0 {
        return Err(IsoManifestError::InvalidBound { line, field: name });
    }
    usize::try_from(value).map_err(|_| IsoManifestError::InvalidBound { line, field: name })
}

/// Splits one manifest line into exactly [`ISO_MANIFEST_FIELDS`] fields.
struct ManifestFields<'a> {
    fields: std::str::Split<'a, char>,
    line: usize,
    taken: usize,
}

impl<'a> ManifestFields<'a> {
    fn new(line: &'a str, number: usize) -> Self {
        Self {
            fields: line.split('\t'),
            line: number,
            taken: 0,
        }
    }

    fn take(&mut self) -> Result<&'a str, IsoManifestError> {
        match self.fields.next() {
            Some(field) => {
                self.taken = self.taken.saturating_add(1);
                Ok(field)
            }
            None => Err(IsoManifestError::FieldCount {
                line: self.line,
                count: self.taken,
            }),
        }
    }

    fn finish(mut self) -> Result<(), IsoManifestError> {
        let Some(_extra) = self.fields.next() else {
            return Ok(());
        };
        Err(IsoManifestError::FieldCount {
            line: self.line,
            count: self
                .taken
                .saturating_add(1)
                .saturating_add(self.fields.count()),
        })
    }
}

fn validate_taps(taps: &[IsoTapConfig]) -> Result<(), IsoTapError> {
    if taps.is_empty() || taps.len() > MAX_ISO_TAPS {
        return Err(IsoTapError::InvalidTapCount { count: taps.len() });
    }
    let mut recorders = HashSet::with_capacity(taps.len());
    let mut video_inputs = HashSet::with_capacity(taps.len());
    for (index, tap) in taps.iter().enumerate() {
        let slot = slot_for_index(index);
        if !recorders.insert(tap.recorder) {
            return Err(IsoTapError::DuplicateRecorder(tap.recorder));
        }
        if !video_inputs.insert(tap.video_input) {
            return Err(IsoTapError::DuplicateVideoInput(tap.video_input));
        }
        match tap.timecode_source {
            TimecodeSource::SharedMaster if tap.source_timecode.is_some() => {
                return Err(IsoTapError::UnexpectedSourceTimecode { slot });
            }
            TimecodeSource::SourceOriginal if tap.source_timecode.is_none() => {
                return Err(IsoTapError::MissingSourceTimecode { slot });
            }
            TimecodeSource::SharedMaster | TimecodeSource::SourceOriginal => {}
        }
        tap.recorder_config(tap.source_timecode)
            .validate()
            .map_err(|source| IsoTapError::InvalidConfig { slot, source })?;
    }
    Ok(())
}

fn combined_start_outcome(outcomes: &[ActionOutcome]) -> Option<ActionOutcome> {
    if outcomes
        .iter()
        .all(|outcome| *outcome == ActionOutcome::AlreadyApplied)
    {
        Some(ActionOutcome::AlreadyApplied)
    } else if outcomes
        .iter()
        .all(|outcome| *outcome == ActionOutcome::Applied)
    {
        Some(ActionOutcome::Applied)
    } else {
        None
    }
}

fn rollback(coordinator: &mut RecorderCoordinator, taps: &[IsoTapConfig], started: usize) {
    for (index, tap) in taps.iter().take(started).enumerate() {
        stop_best_effort(coordinator, index, *tap);
    }
}

fn stop_best_effort(coordinator: &mut RecorderCoordinator, index: usize, tap: IsoTapConfig) {
    let slot = slot_for_index(index);
    let receipt = derived_receipt(STOP_RECEIPT_TAG, slot, tap.recorder);
    // Best-effort rollback: a stop failure cannot supersede the original
    // failure returned to the caller, and reopening the coordinator
    // reconciles any recorder this leaves recording.
    let _ = coordinator.stop(tap.recorder, receipt);
}

/// Derives a durable action receipt for one tap action.
///
/// Receipts are a stable FNV-1a hash of the action tag, the 1-based slot, and
/// the recorder id, so repeating `start` after a restart deduplicates exactly
/// as [`RecorderCoordinator`] requires. Start and stop receipts use disjoint
/// tags and can never collide for the same tap.
fn derived_receipt(tag: u8, slot: u8, recorder: RecorderId) -> ActionReceiptId {
    const FNV_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in recorder
        .get()
        .get()
        .to_le_bytes()
        .into_iter()
        .chain([tag, slot])
    {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    ActionReceiptId::new(nonzero(hash | 1))
}

const fn nonzero(value: u128) -> NonZeroU128 {
    match NonZeroU128::new(value) {
        Some(value) => value,
        None => NonZeroU128::MIN,
    }
}

fn render_iso_manifest_line(entry: &IsoManifestEntry) -> String {
    let fields = [
        entry.slot.to_string(),
        entry.recorder.get().get().to_string(),
        entry.video_input.get().get().to_string(),
        entry
            .audio_input
            .map_or_else(|| "none".to_owned(), |input| input.get().get().to_string()),
        timecode_source_name(entry.timecode_source).to_owned(),
        entry
            .start_timecode
            .map_or_else(|| "none".to_owned(), render_timecode),
        render_bound(entry.segments.max_frames),
        render_bound(entry.segments.max_bytes),
        render_bound(
            entry
                .segments
                .max_duration
                .map(NormalizedDuration::as_nanos),
        ),
        entry.queue.max_events.to_string(),
        entry.queue.max_bytes.to_string(),
    ];
    fields.join("\t")
}

fn render_bound(value: Option<u64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn render_timecode(timecode: Timecode) -> String {
    let (hours, minutes, seconds, frames) = timecode.components();
    format!("{hours:02}:{minutes:02}:{seconds:02}:{frames:02}")
}

const fn timecode_source_name(source: TimecodeSource) -> &'static str {
    match source {
        TimecodeSource::SharedMaster => "shared",
        TimecodeSource::SourceOriginal => "source",
    }
}

/// Converts a zero-based tap index into the public 1-based slot.
///
/// Tap rosters are validated to hold at most [`MAX_ISO_TAPS`] entries, so the
/// conversion cannot overflow.
fn slot_for_index(index: usize) -> u8 {
    u8::try_from(index.saturating_add(1)).unwrap_or(u8::MAX)
}
