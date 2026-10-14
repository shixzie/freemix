//! Export-during-record job planning for compiled highlight reels.
//!
//! A job snapshots a [`CompiledReel`], requires every segment it can read to
//! be finalized (closed), and protects those segments from retention with an
//! [`ProtectionReference::Export`] reference for as long as it is active.
//! Progress advances in bounded chunks of output frames; before every chunk
//! the job is re-validated against the catalog and the source timeline so an
//! export never resolves media that retention or observation pruning
//! removed. Capture is never touched. Nothing is encoded or written here.

use core::{fmt, ops::Range};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    CameraId, CompiledReel, ExportJobId, HighlightId, ProtectionReference, ReelError, ReelFrame,
    ReplayError, RollingSegmentCatalog, SegmentId, SegmentState, SourceObservation, SourceTimeline,
    TimelineRange,
};

/// Maximum jobs (active or finished) held by one queue.
pub const MAX_RECORD_EXPORT_JOBS: usize = 32;
/// Maximum distinct segments one export may reference.
pub const MAX_SEGMENTS_PER_EXPORT: usize = 4_096;
/// Maximum output frames resolved by one [`RecordExportQueue::advance`] call.
pub const MAX_EXPORT_CHUNK_FRAMES: u64 = 1_024;

/// Why an active job was failed during re-validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordExportFailure {
    /// A planned segment no longer exists in the catalog.
    SegmentPruned(SegmentId),
    /// A planned segment lost this job's retention protection.
    ProtectionReleased(SegmentId),
    /// A planned segment is no longer finalized.
    SegmentReopened(SegmentId),
    /// Source observations backing a clip were pruned or changed.
    SourceFramesPruned(HighlightId),
    /// A frame resolved outside the planned segments.
    UnplannedSegment {
        output_frame: u64,
        segment: SegmentId,
    },
    /// A frame could not be resolved.
    Unresolved(ReelError),
}

impl fmt::Display for RecordExportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SegmentPruned(id) => write!(formatter, "segment {id} was pruned"),
            Self::ProtectionReleased(id) => {
                write!(formatter, "segment {id} lost its export protection")
            }
            Self::SegmentReopened(id) => write!(formatter, "segment {id} is no longer finalized"),
            Self::SourceFramesPruned(id) => {
                write!(formatter, "source frames of reel clip {id} were pruned")
            }
            Self::UnplannedSegment {
                output_frame,
                segment,
            } => write!(
                formatter,
                "output frame {output_frame} resolved unplanned segment {segment}"
            ),
            Self::Unresolved(error) => error.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordExportState {
    Queued,
    Running,
    Completed,
    Failed(RecordExportFailure),
    Cancelled,
}

impl RecordExportState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }
}

/// Frame progress of a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordExportProgress {
    pub completed_frames: u64,
    pub total_frames: u64,
}

impl RecordExportProgress {
    /// Completion in thousandths, rounded down.
    #[must_use]
    pub fn permille(self) -> u16 {
        let value =
            u128::from(self.completed_frames) * 1_000 / u128::from(self.total_frames.max(1));
        u16::try_from(value).unwrap_or(1_000)
    }
}

/// Per-clip source anchors captured at submission for re-validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClipAnchor {
    clip: HighlightId,
    camera: CameraId,
    source: TimelineRange,
    /// Frame displayed at the clip's first source instant.
    lead: SourceObservation,
    /// First and last observation inside the source range, and their count.
    first: SourceObservation,
    last: SourceObservation,
    frame_count: usize,
}

/// One export job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordExportJob {
    id: ExportJobId,
    reel: CompiledReel,
    segments: BTreeMap<SegmentId, CameraId>,
    anchors: Vec<ClipAnchor>,
    state: RecordExportState,
    completed_frames: u64,
}

impl RecordExportJob {
    #[must_use]
    pub const fn id(&self) -> ExportJobId {
        self.id
    }

    #[must_use]
    pub const fn reel(&self) -> &CompiledReel {
        &self.reel
    }

    #[must_use]
    pub const fn state(&self) -> &RecordExportState {
        &self.state
    }

    /// Finalized segments the job reads and protects, in ascending ID order.
    pub fn segments(&self) -> impl Iterator<Item = SegmentId> + '_ {
        self.segments.keys().copied()
    }

    #[must_use]
    pub const fn progress(&self) -> RecordExportProgress {
        RecordExportProgress {
            completed_frames: self.completed_frames,
            total_frames: self.reel.total_frames(),
        }
    }

    fn reference(&self) -> ProtectionReference {
        ProtectionReference::Export(self.id)
    }

    fn validate(
        &self,
        catalog: &RollingSegmentCatalog,
        timeline: &SourceTimeline,
    ) -> Result<(), RecordExportFailure> {
        let reference = self.reference();
        for &id in self.segments.keys() {
            let segment = catalog
                .segment(id)
                .ok_or(RecordExportFailure::SegmentPruned(id))?;
            if segment.state != SegmentState::Closed {
                return Err(RecordExportFailure::SegmentReopened(id));
            }
            if !segment.protections.contains(&reference) {
                return Err(RecordExportFailure::ProtectionReleased(id));
            }
        }
        for anchor in &self.anchors {
            if capture_anchor(anchor.clip, anchor.camera, anchor.source, timeline).ok()
                != Some(*anchor)
            {
                return Err(RecordExportFailure::SourceFramesPruned(anchor.clip));
            }
        }
        Ok(())
    }
}

fn capture_anchor(
    clip: HighlightId,
    camera: CameraId,
    source: TimelineRange,
    timeline: &SourceTimeline,
) -> Result<ClipAnchor, ReplayError> {
    let lead = timeline.resolve(camera, source.start)?;
    let span = timeline.frames_in(camera, source)?;
    Ok(ClipAnchor {
        clip,
        camera,
        source,
        lead,
        first: span.first,
        last: span.last,
        frame_count: span.frame_count,
    })
}

/// Frames resolved by one progress step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportChunk {
    pub job: ExportJobId,
    pub frames: Range<u64>,
    /// Exact composition of every frame in `frames`, in order.
    pub resolved: Vec<ReelFrame>,
    /// Distinct segments read by this chunk, ascending.
    pub segments: Vec<SegmentId>,
    pub progress: RecordExportProgress,
    /// The chunk finished the job and released its protection.
    pub completed: bool,
}

/// Bounded set of export-during-record jobs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordExportQueue {
    jobs: BTreeMap<ExportJobId, RecordExportJob>,
}

impl RecordExportQueue {
    #[must_use]
    pub fn job(&self, id: ExportJobId) -> Option<&RecordExportJob> {
        self.jobs.get(&id)
    }

    pub fn jobs(&self) -> impl Iterator<Item = &RecordExportJob> {
        self.jobs.values()
    }

    /// Plans an export of `reel`, verifying that every segment any frame can
    /// read is recorded without gaps and finalized, then protects those
    /// segments for the job. Either the job is queued and protection applied,
    /// or nothing changes.
    ///
    /// # Errors
    ///
    /// IDs must be unique and the queue bounded; each clip needs gap-free
    /// coverage of closed segments, an observed frame at its start and inside
    /// its range, and the job must reference at most
    /// [`MAX_SEGMENTS_PER_EXPORT`] segments.
    pub fn submit(
        &mut self,
        id: ExportJobId,
        reel: &CompiledReel,
        catalog: &mut RollingSegmentCatalog,
        timeline: &SourceTimeline,
    ) -> Result<&RecordExportJob, RecordExportError> {
        if self.jobs.contains_key(&id) {
            return Err(RecordExportError::DuplicateJob(id));
        }
        if self.jobs.len() >= MAX_RECORD_EXPORT_JOBS {
            return Err(RecordExportError::TooManyJobs {
                maximum: MAX_RECORD_EXPORT_JOBS,
            });
        }
        let mut segments = BTreeMap::new();
        let mut anchors = Vec::with_capacity(reel.clips().len());
        for clip in reel.clips() {
            let unresolved = |error| RecordExportError::Unresolved {
                clip: clip.id,
                error,
            };
            let coverage = catalog.coverage(clip.angle, clip.source);
            if let Some(&gap) = coverage.gaps.first() {
                return Err(RecordExportError::CoverageGap { clip: clip.id, gap });
            }
            let anchor =
                capture_anchor(clip.id, clip.angle, clip.source, timeline).map_err(unresolved)?;
            let lead_segment = catalog
                .segment_at(clip.angle, anchor.lead.synchronized_at)
                .ok_or_else(|| {
                    unresolved(ReplayError::SegmentUnavailable {
                        camera: clip.angle,
                        at: anchor.lead.synchronized_at,
                    })
                })?
                .id;
            for segment_id in coverage.segments.into_iter().chain([lead_segment]) {
                let segment = catalog
                    .segment(segment_id)
                    .ok_or_else(|| unresolved(ReplayError::UnknownSegment(segment_id)))?;
                if segment.state != SegmentState::Closed {
                    return Err(RecordExportError::SegmentNotFinalized {
                        clip: clip.id,
                        segment: segment_id,
                    });
                }
                segments.insert(segment_id, clip.angle);
            }
            if segments.len() > MAX_SEGMENTS_PER_EXPORT {
                return Err(RecordExportError::TooManySegments {
                    maximum: MAX_SEGMENTS_PER_EXPORT,
                });
            }
            anchors.push(anchor);
        }
        let reference = ProtectionReference::Export(id);
        for (&segment_id, &camera) in &segments {
            if let Some(range) = catalog.segment(segment_id).map(|segment| segment.timeline) {
                let _protected = catalog.protect_range(camera, range, reference);
            }
        }
        let job = RecordExportJob {
            id,
            reel: reel.clone(),
            segments,
            anchors,
            state: RecordExportState::Queued,
            completed_frames: 0,
        };
        Ok(self.jobs.entry(id).or_insert(job))
    }

    /// Re-validates the job, then resolves up to `max_frames` further output
    /// frames. A queued job starts running. Completing the last frame marks
    /// the job completed and releases its protection.
    ///
    /// # Errors
    ///
    /// The job must exist and be active and `max_frames` must be
    /// 1..=[`MAX_EXPORT_CHUNK_FRAMES`]. If re-validation or resolution fails,
    /// the job becomes [`RecordExportState::Failed`], its protection is
    /// released, and [`RecordExportError::Failed`] is returned.
    pub fn advance(
        &mut self,
        id: ExportJobId,
        max_frames: u64,
        catalog: &mut RollingSegmentCatalog,
        timeline: &SourceTimeline,
    ) -> Result<ExportChunk, RecordExportError> {
        if max_frames == 0 || max_frames > MAX_EXPORT_CHUNK_FRAMES {
            return Err(RecordExportError::InvalidChunk {
                requested: max_frames,
                maximum: MAX_EXPORT_CHUNK_FRAMES,
            });
        }
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or(RecordExportError::UnknownJob(id))?;
        if job.state.is_terminal() {
            return Err(RecordExportError::NotActive(id));
        }
        let start = job.completed_frames;
        let end = start
            .saturating_add(max_frames)
            .min(job.reel.total_frames());
        let outcome = job.validate(catalog, timeline).and_then(|()| {
            let mut resolved = Vec::new();
            let mut read = BTreeSet::new();
            for frame in start..end {
                let composed = job
                    .reel
                    .resolve_frame(frame, timeline, catalog)
                    .map_err(RecordExportFailure::Unresolved)?;
                for layer in &composed.layers {
                    if !job.segments.contains_key(&layer.segment_id) {
                        return Err(RecordExportFailure::UnplannedSegment {
                            output_frame: frame,
                            segment: layer.segment_id,
                        });
                    }
                    read.insert(layer.segment_id);
                }
                resolved.push(composed);
            }
            Ok((resolved, read))
        });
        let (resolved, read) = match outcome {
            Ok(result) => result,
            Err(failure) => {
                catalog.release_reference(job.reference());
                job.state = RecordExportState::Failed(failure.clone());
                return Err(RecordExportError::Failed { job: id, failure });
            }
        };
        job.completed_frames = end;
        let completed = end == job.reel.total_frames();
        job.state = if completed {
            catalog.release_reference(job.reference());
            RecordExportState::Completed
        } else {
            RecordExportState::Running
        };
        Ok(ExportChunk {
            job: id,
            frames: start..end,
            resolved,
            segments: read.into_iter().collect(),
            progress: job.progress(),
            completed,
        })
    }

    /// Cancels an active job and releases its protection.
    ///
    /// # Errors
    ///
    /// The job must exist and be active.
    pub fn cancel(
        &mut self,
        id: ExportJobId,
        catalog: &mut RollingSegmentCatalog,
    ) -> Result<(), RecordExportError> {
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or(RecordExportError::UnknownJob(id))?;
        if job.state.is_terminal() {
            return Err(RecordExportError::NotActive(id));
        }
        catalog.release_reference(job.reference());
        job.state = RecordExportState::Cancelled;
        Ok(())
    }

    /// Re-validates every active job (for example after retention ran or
    /// the source timeline was pruned), failing and unprotecting any job
    /// that references removed media. Returns the failures in job-ID order.
    pub fn revalidate(
        &mut self,
        catalog: &mut RollingSegmentCatalog,
        timeline: &SourceTimeline,
    ) -> Vec<(ExportJobId, RecordExportFailure)> {
        let mut failures = Vec::new();
        for job in self.jobs.values_mut() {
            if job.state.is_terminal() {
                continue;
            }
            if let Err(failure) = job.validate(catalog, timeline) {
                catalog.release_reference(job.reference());
                job.state = RecordExportState::Failed(failure.clone());
                failures.push((job.id, failure));
            }
        }
        failures
    }

    /// Drops a finished job so its slot can be reused.
    ///
    /// # Errors
    ///
    /// The job must exist and be terminal.
    pub fn forget(&mut self, id: ExportJobId) -> Result<RecordExportJob, RecordExportError> {
        let job = self
            .jobs
            .get(&id)
            .ok_or(RecordExportError::UnknownJob(id))?;
        if !job.state.is_terminal() {
            return Err(RecordExportError::StillActive(id));
        }
        self.jobs
            .remove(&id)
            .ok_or(RecordExportError::UnknownJob(id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordExportError {
    DuplicateJob(ExportJobId),
    UnknownJob(ExportJobId),
    TooManyJobs {
        maximum: usize,
    },
    TooManySegments {
        maximum: usize,
    },
    CoverageGap {
        clip: HighlightId,
        gap: TimelineRange,
    },
    SegmentNotFinalized {
        clip: HighlightId,
        segment: SegmentId,
    },
    Unresolved {
        clip: HighlightId,
        error: ReplayError,
    },
    InvalidChunk {
        requested: u64,
        maximum: u64,
    },
    NotActive(ExportJobId),
    StillActive(ExportJobId),
    Failed {
        job: ExportJobId,
        failure: RecordExportFailure,
    },
}

impl fmt::Display for RecordExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateJob(id) => write!(formatter, "duplicate export job {id}"),
            Self::UnknownJob(id) => write!(formatter, "unknown export job {id}"),
            Self::TooManyJobs { maximum } => {
                write!(formatter, "export queue exceeds {maximum} jobs")
            }
            Self::TooManySegments { maximum } => {
                write!(formatter, "export references more than {maximum} segments")
            }
            Self::CoverageGap { clip, gap } => write!(
                formatter,
                "reel clip {clip} has no recorded segment for {}..{} ns",
                gap.start.as_nanos(),
                gap.end.as_nanos()
            ),
            Self::SegmentNotFinalized { clip, segment } => write!(
                formatter,
                "reel clip {clip} reads segment {segment}, which is still recording"
            ),
            Self::Unresolved { clip, error } => {
                write!(formatter, "reel clip {clip} is unresolved: {error}")
            }
            Self::InvalidChunk { requested, maximum } => write!(
                formatter,
                "export chunk of {requested} frames is outside 1..={maximum}"
            ),
            Self::NotActive(id) => write!(formatter, "export job {id} is not active"),
            Self::StillActive(id) => write!(formatter, "export job {id} is still active"),
            Self::Failed { job, failure } => {
                write!(formatter, "export job {job} failed: {failure}")
            }
        }
    }
}

impl std::error::Error for RecordExportError {}
