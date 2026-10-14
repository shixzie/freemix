use std::{
    collections::{BTreeMap, BTreeSet},
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

    /// Reclaims at least `requested_bytes` where possible, oldest first.
    /// Open or referenced segments are never selected.
    ///
    /// This only updates the in-memory catalog; the caller owns durable deletion.
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

        let mut report = RetentionReport::default();
        for (_, _, id) in candidates {
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
