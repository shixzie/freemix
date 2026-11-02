use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use fm_frame::NormalizedTimestamp;

use crate::{
    CameraId, EventId, ExportJobId, HighlightId, ReplayError, SegmentCoverage, SegmentId,
    StorageRootId, TimelineRange,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageRoot {
    pub id: StorageRootId,
    pub path: PathBuf,
    pub capacity_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentState {
    Open,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProtectionReference {
    Event(EventId),
    Highlight(HighlightId),
    Export(ExportJobId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentMetadata {
    pub id: SegmentId,
    pub camera_id: CameraId,
    pub storage_root_id: StorageRootId,
    pub recorder_segment_index: u64,
    pub timeline: TimelineRange,
    pub path: PathBuf,
    pub bytes: u64,
    pub state: SegmentState,
    pub protections: BTreeSet<ProtectionReference>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetentionReport {
    pub deleted: Vec<SegmentId>,
    pub reclaimed_bytes: u64,
    pub blocked_bytes: u64,
    pub protected_segments: Vec<SegmentId>,
}

/// One segment file that could not be deleted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReclamationFailure {
    pub segment: SegmentId,
    pub path: PathBuf,
    pub kind: io::ErrorKind,
}

/// Outcome of [`RollingSegmentCatalog::reclaim_and_delete`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileReclamation {
    /// The in-memory retention report; `deleted` includes files already gone.
    pub report: RetentionReport,
    /// Segments whose files were removed.
    pub removed: Vec<SegmentId>,
    /// Segments whose files were already missing, so their bytes are free.
    pub missing: Vec<SegmentId>,
    /// Segments kept in the catalog because their file could not be removed.
    pub failed: Vec<FileReclamationFailure>,
}

#[derive(Clone, Debug, Default)]
pub struct RollingSegmentCatalog {
    roots: BTreeMap<StorageRootId, StorageRoot>,
    camera_roots: BTreeMap<CameraId, StorageRootId>,
    segments: BTreeMap<SegmentId, SegmentMetadata>,
}

impl RollingSegmentCatalog {
    /// Registers a storage root as metadata without probing its throughput.
    ///
    /// # Errors
    ///
    /// Duplicate IDs are rejected.
    pub fn add_root(&mut self, root: StorageRoot) -> Result<(), ReplayError> {
        if self.roots.contains_key(&root.id) {
            return Err(ReplayError::DuplicateStorageRoot(root.id));
        }
        self.roots.insert(root.id, root);
        Ok(())
    }

    /// Assigns one camera to a previously registered storage root.
    ///
    /// # Errors
    ///
    /// The root must exist.
    pub fn assign_camera(
        &mut self,
        camera_id: CameraId,
        root_id: StorageRootId,
    ) -> Result<(), ReplayError> {
        if !self.roots.contains_key(&root_id) {
            return Err(ReplayError::UnknownStorageRoot(root_id));
        }
        self.camera_roots.insert(camera_id, root_id);
        Ok(())
    }

    /// Adds segment metadata after validating its camera/root assignment.
    ///
    /// # Errors
    ///
    /// Unknown/mismatched roots and duplicate segment IDs are rejected.
    pub fn insert_segment(&mut self, segment: SegmentMetadata) -> Result<(), ReplayError> {
        let assigned = self
            .camera_roots
            .get(&segment.camera_id)
            .ok_or(ReplayError::UnknownCamera(segment.camera_id))?;
        if *assigned != segment.storage_root_id {
            return Err(ReplayError::CameraRootMismatch);
        }
        if self.segments.contains_key(&segment.id) {
            return Err(ReplayError::DuplicateSegment(segment.id));
        }
        self.segments.insert(segment.id, segment);
        Ok(())
    }

    /// Moves the end of open segment `id` forward to `end` as its recorder
    /// writes more media into it (the live edge of a rolling recording).
    ///
    /// # Errors
    ///
    /// The segment must exist and still be open, and `end` must not move
    /// its end backwards or before its start.
    pub fn extend_open_segment(
        &mut self,
        id: SegmentId,
        end: NormalizedTimestamp,
    ) -> Result<(), ReplayError> {
        let segment = self
            .segments
            .get_mut(&id)
            .ok_or(ReplayError::UnknownSegment(id))?;
        if segment.state != SegmentState::Open {
            return Err(ReplayError::SegmentClosed(id));
        }
        if end < segment.timeline.end {
            return Err(ReplayError::InvalidRange);
        }
        segment.timeline.end = end;
        Ok(())
    }

    /// Closes open segment `id` once its recorder finished the file: its
    /// exact end on the timeline and its final size. A closed segment is
    /// eligible for retention unless something protects it.
    ///
    /// # Errors
    ///
    /// The segment must exist and still be open, and `end` must lie after
    /// its start.
    pub fn close_segment(
        &mut self,
        id: SegmentId,
        end: NormalizedTimestamp,
        bytes: u64,
    ) -> Result<(), ReplayError> {
        let segment = self
            .segments
            .get_mut(&id)
            .ok_or(ReplayError::UnknownSegment(id))?;
        if segment.state != SegmentState::Open {
            return Err(ReplayError::SegmentClosed(id));
        }
        if end <= segment.timeline.start {
            return Err(ReplayError::InvalidRange);
        }
        segment.timeline.end = end;
        segment.bytes = bytes;
        segment.state = SegmentState::Closed;
        Ok(())
    }

    /// Removes every closed, unprotected segment of `root_id` that ends at
    /// or before `cutoff` (a time-based retention window), oldest first, and
    /// returns their metadata so the caller can delete the files. Open and
    /// referenced segments are kept whatever their age.
    ///
    /// # Errors
    ///
    /// The root must be registered.
    pub fn reclaim_ended_before(
        &mut self,
        root_id: StorageRootId,
        cutoff: NormalizedTimestamp,
    ) -> Result<Vec<SegmentMetadata>, ReplayError> {
        if !self.roots.contains_key(&root_id) {
            return Err(ReplayError::UnknownStorageRoot(root_id));
        }
        let mut expired: Vec<_> = self
            .segments
            .values()
            .filter(|segment| {
                segment.storage_root_id == root_id
                    && segment.state == SegmentState::Closed
                    && segment.protections.is_empty()
                    && segment.timeline.end <= cutoff
            })
            .map(|segment| {
                (
                    segment.timeline.start,
                    segment.recorder_segment_index,
                    segment.id,
                )
            })
            .collect();
        expired.sort_unstable();
        Ok(expired
            .into_iter()
            .filter_map(|(_, _, id)| self.segments.remove(&id))
            .collect())
    }

    /// Forgets every segment of `camera_id` (its recording stopped and its
    /// files are gone) and returns them. Protections are dropped with them.
    pub fn remove_camera_segments(&mut self, camera_id: CameraId) -> Vec<SegmentMetadata> {
        let ids: Vec<_> = self
            .segments
            .values()
            .filter(|segment| segment.camera_id == camera_id)
            .map(|segment| segment.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.segments.remove(&id))
            .collect()
    }

    #[must_use]
    pub fn root(&self, id: StorageRootId) -> Option<&StorageRoot> {
        self.roots.get(&id)
    }

    pub fn roots(&self) -> impl Iterator<Item = &StorageRoot> {
        self.roots.values()
    }

    #[must_use]
    pub fn segment(&self, id: SegmentId) -> Option<&SegmentMetadata> {
        self.segments.get(&id)
    }

    pub fn segments(&self) -> impl Iterator<Item = &SegmentMetadata> {
        self.segments.values()
    }

    /// Returns the segment holding `camera_id` media at `at`.
    ///
    /// When segments overlap, the earliest (timeline start, recorder index,
    /// ID) wins so resolution is deterministic.
    #[must_use]
    pub fn segment_at(
        &self,
        camera_id: CameraId,
        at: NormalizedTimestamp,
    ) -> Option<&SegmentMetadata> {
        self.camera_segments(camera_id)
            .into_iter()
            .find(|segment| segment.timeline.contains(at))
    }

    /// Reports which segments cover `range` for one camera and which
    /// sub-ranges have no recorded media.
    #[must_use]
    pub fn coverage(&self, camera_id: CameraId, range: TimelineRange) -> SegmentCoverage {
        let mut segments = Vec::new();
        let mut gaps = Vec::new();
        let mut cursor = range.start;
        for segment in self.camera_segments(camera_id) {
            if !segment.timeline.overlaps(range) {
                continue;
            }
            if segment.timeline.start > cursor {
                gaps.push(TimelineRange {
                    start: cursor,
                    end: segment.timeline.start,
                });
            }
            cursor = cursor.max(segment.timeline.end);
            segments.push(segment.id);
        }
        if cursor < range.end {
            gaps.push(TimelineRange {
                start: cursor,
                end: range.end,
            });
        }
        SegmentCoverage {
            camera_id,
            range,
            segments,
            gaps,
        }
    }

    fn camera_segments(&self, camera_id: CameraId) -> Vec<&SegmentMetadata> {
        let mut segments: Vec<_> = self
            .segments
            .values()
            .filter(|segment| segment.camera_id == camera_id)
            .collect();
        segments.sort_by_key(|segment| {
            (
                segment.timeline.start,
                segment.recorder_segment_index,
                segment.id,
            )
        });
        segments
    }

    #[must_use]
    pub fn retained_bytes(&self, root_id: StorageRootId) -> u64 {
        self.segments
            .values()
            .filter(|segment| segment.storage_root_id == root_id)
            .fold(0_u64, |total, segment| total.saturating_add(segment.bytes))
    }

    /// Protects every segment intersecting a camera event range.
    #[must_use]
    pub fn protect_range(
        &mut self,
        camera_id: CameraId,
        range: TimelineRange,
        reference: ProtectionReference,
    ) -> Vec<SegmentId> {
        let mut protected = Vec::new();
        for segment in self.segments.values_mut() {
            if segment.camera_id == camera_id && segment.timeline.overlaps(range) {
                segment.protections.insert(reference);
                protected.push(segment.id);
            }
        }
        protected
    }

    pub fn release_reference(&mut self, reference: ProtectionReference) {
        for segment in self.segments.values_mut() {
            segment.protections.remove(&reference);
        }
    }

    /// Segment IDs of one root in eviction order: oldest timeline first,
    /// then recorder order and ID.
    fn reclaim_candidates(&self, root_id: StorageRootId) -> Vec<SegmentId> {
        let mut candidates: Vec<_> = self
            .segments
            .values()
            .filter(|segment| segment.storage_root_id == root_id)
            .map(|segment| {
                (
                    segment.timeline.start,
                    segment.recorder_segment_index,
                    segment.id,
                )
            })
            .collect();
        candidates.sort_unstable();
        candidates.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Reclaims at least `requested_bytes` where possible, oldest first.
    /// Open or referenced segments are never selected.
    ///
    /// This only updates the in-memory catalog; the caller owns durable
    /// deletion with [`Self::reclaim_and_delete`].
    ///
    /// # Errors
    ///
    /// The root must be registered.
    pub fn reclaim(
        &mut self,
        root_id: StorageRootId,
        requested_bytes: u64,
    ) -> Result<RetentionReport, ReplayError> {
        if !self.roots.contains_key(&root_id) {
            return Err(ReplayError::UnknownStorageRoot(root_id));
        }
        let mut report = RetentionReport::default();
        for id in self.reclaim_candidates(root_id) {
            if report.reclaimed_bytes >= requested_bytes {
                break;
            }
            let segment = &self.segments[&id];
            if segment.state == SegmentState::Open || !segment.protections.is_empty() {
                report.protected_segments.push(id);
                continue;
            }
            report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(segment.bytes);
            report.deleted.push(id);
        }
        for id in &report.deleted {
            self.segments.remove(id);
        }
        report.blocked_bytes = requested_bytes.saturating_sub(report.reclaimed_bytes);
        Ok(report)
    }

    /// Reclaims at least `requested_bytes` and deletes each selected file
    /// from disk, oldest first. Open, referenced, and out-of-root segments are
    /// never deleted.
    ///
    /// A missing file counts as reclaimed (its bytes are already free); a
    /// file that cannot be removed stays in the catalog and in `failed`, so a
    /// later call retries it. Segment paths must lie under the root path as
    /// registered; a path outside it is refused with
    /// [`io::ErrorKind::InvalidInput`].
    ///
    /// # Errors
    ///
    /// The root must be registered.
    pub fn reclaim_and_delete(
        &mut self,
        root_id: StorageRootId,
        requested_bytes: u64,
    ) -> Result<FileReclamation, ReplayError> {
        let root = self
            .roots
            .get(&root_id)
            .cloned()
            .ok_or(ReplayError::UnknownStorageRoot(root_id))?;
        let mut result = FileReclamation::default();
        for id in self.reclaim_candidates(root_id) {
            if result.report.reclaimed_bytes >= requested_bytes {
                break;
            }
            let Some(segment) = self.segments.get(&id).cloned() else {
                continue;
            };
            if segment.state == SegmentState::Open || !segment.protections.is_empty() {
                result.report.protected_segments.push(id);
                continue;
            }
            if !segment.path.starts_with(&root.path) {
                result.failed.push(FileReclamationFailure {
                    segment: id,
                    path: segment.path,
                    kind: io::ErrorKind::InvalidInput,
                });
                continue;
            }
            match fs::remove_file(&segment.path) {
                Ok(()) => {
                    self.segments.remove(&id);
                    result.removed.push(id);
                    result.report.deleted.push(id);
                    result.report.reclaimed_bytes =
                        result.report.reclaimed_bytes.saturating_add(segment.bytes);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.segments.remove(&id);
                    result.missing.push(id);
                    result.report.deleted.push(id);
                    result.report.reclaimed_bytes =
                        result.report.reclaimed_bytes.saturating_add(segment.bytes);
                }
                Err(error) => result.failed.push(FileReclamationFailure {
                    segment: id,
                    path: segment.path,
                    kind: error.kind(),
                }),
            }
        }
        result.report.blocked_bytes = requested_bytes.saturating_sub(result.report.reclaimed_bytes);
        Ok(result)
    }

    /// Applies a maximum retained-byte policy using the same protected eviction.
    ///
    /// # Errors
    ///
    /// The root must be registered.
    pub fn enforce_retained_bytes(
        &mut self,
        root_id: StorageRootId,
        maximum_bytes: u64,
    ) -> Result<RetentionReport, ReplayError> {
        let excess = self.retained_bytes(root_id).saturating_sub(maximum_bytes);
        self.reclaim(root_id, excess)
    }

    pub(crate) fn known_paths(&self, root_id: StorageRootId) -> BTreeSet<&Path> {
        self.segments
            .values()
            .filter(|segment| segment.storage_root_id == root_id)
            .map(|segment| segment.path.as_path())
            .collect()
    }
}
