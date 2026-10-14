//! Replay disk capacity/health prediction from configured camera bitrates.
//!
//! Unlike [`crate::CapacityPrediction`], which measures an observed write
//! sample, this predicts consumption from per-camera bitrates before or during
//! recording, and classifies low space with hysteresis so the operator alert
//! does not flap around a threshold. All inputs are caller-supplied; nothing
//! reads the system clock or the filesystem.

use core::fmt;
use std::collections::BTreeMap;

use crate::{CameraId, MAX_REPLAY_SOURCES, SourceCatalog, StorageRootId};

/// Largest accepted per-camera bitrate (video plus audio): 4 Gbit/s.
pub const MAX_CAMERA_BITS_PER_SECOND: u64 = 4_000_000_000;
/// Largest accepted container/index overhead, in thousandths (100 %).
pub const MAX_OVERHEAD_PERMILLE: u16 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CameraBitrate {
    pub camera_id: CameraId,
    pub video_bits_per_second: u64,
    pub audio_bits_per_second: u64,
}

/// Validated recording load: bytes per second written for each camera.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingLoad {
    bytes_per_second: BTreeMap<CameraId, u64>,
    overhead_permille: u16,
}

impl RecordingLoad {
    /// Converts bitrates to bytes per second, rounding up, after applying
    /// `overhead_permille` of container/index overhead.
    ///
    /// # Errors
    ///
    /// At most eight distinct cameras; each total bitrate must be nonzero and
    /// at most [`MAX_CAMERA_BITS_PER_SECOND`]; overhead at most
    /// [`MAX_OVERHEAD_PERMILLE`].
    pub fn new(
        bitrates: Vec<CameraBitrate>,
        overhead_permille: u16,
    ) -> Result<Self, DiskHealthError> {
        if bitrates.len() > MAX_REPLAY_SOURCES {
            return Err(DiskHealthError::TooManyCameras(bitrates.len()));
        }
        if overhead_permille > MAX_OVERHEAD_PERMILLE {
            return Err(DiskHealthError::InvalidOverhead(overhead_permille));
        }
        let mut bytes_per_second = BTreeMap::new();
        for bitrate in bitrates {
            let bits = bitrate
                .video_bits_per_second
                .checked_add(bitrate.audio_bits_per_second)
                .filter(|bits| (1..=MAX_CAMERA_BITS_PER_SECOND).contains(bits))
                .ok_or(DiskHealthError::InvalidBitrate(bitrate.camera_id))?;
            let scaled = u128::from(bits) * (1_000 + u128::from(overhead_permille));
            let bytes = u64::try_from(scaled.div_ceil(8_000))
                .map_err(|_| DiskHealthError::InvalidBitrate(bitrate.camera_id))?;
            if bytes_per_second.insert(bitrate.camera_id, bytes).is_some() {
                return Err(DiskHealthError::DuplicateCamera(bitrate.camera_id));
            }
        }
        Ok(Self {
            bytes_per_second,
            overhead_permille,
        })
    }

    #[must_use]
    pub const fn overhead_permille(&self) -> u16 {
        self.overhead_permille
    }

    #[must_use]
    pub fn camera_bytes_per_second(&self, camera_id: CameraId) -> Option<u64> {
        self.bytes_per_second.get(&camera_id).copied()
    }

    #[must_use]
    pub fn total_bytes_per_second(&self) -> u64 {
        self.bytes_per_second
            .values()
            .fold(0_u64, |sum, bytes| sum.saturating_add(*bytes))
    }

    /// Bytes per second written to one storage root by the cameras the source
    /// catalog assigns to it.
    ///
    /// # Errors
    ///
    /// Every loaded camera must exist in `sources`.
    pub fn root_bytes_per_second(
        &self,
        sources: &SourceCatalog,
        root: StorageRootId,
    ) -> Result<u64, DiskHealthError> {
        let mut total = 0_u64;
        for (camera_id, bytes) in &self.bytes_per_second {
            let source = sources
                .get(*camera_id)
                .ok_or(DiskHealthError::UnknownCamera(*camera_id))?;
            if source.storage_root_id == root {
                total = total.saturating_add(*bytes);
            }
        }
        Ok(total)
    }
}

/// Low-space alert severity, ordered from best to worst.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiskHealthLevel {
    Healthy,
    Warning,
    Critical,
    Full,
}

/// Remaining-recording-time thresholds with a clearing margin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskThresholds {
    warning_seconds: u64,
    critical_seconds: u64,
    hysteresis_seconds: u64,
    reserve_bytes: u64,
}

impl DiskThresholds {
    /// A level is entered once remaining time drops below its threshold and
    /// cleared only once remaining time reaches threshold plus
    /// `hysteresis_seconds`. `reserve_bytes` is never counted as usable.
    ///
    /// # Errors
    ///
    /// Requires `0 < critical_seconds < warning_seconds` and that
    /// `warning_seconds + hysteresis_seconds` does not overflow.
    pub fn new(
        warning_seconds: u64,
        critical_seconds: u64,
        hysteresis_seconds: u64,
        reserve_bytes: u64,
    ) -> Result<Self, DiskHealthError> {
        if critical_seconds == 0
            || critical_seconds >= warning_seconds
            || warning_seconds.checked_add(hysteresis_seconds).is_none()
        {
            return Err(DiskHealthError::InvalidThresholds);
        }
        Ok(Self {
            warning_seconds,
            critical_seconds,
            hysteresis_seconds,
            reserve_bytes,
        })
    }

    #[must_use]
    pub const fn warning_seconds(self) -> u64 {
        self.warning_seconds
    }

    #[must_use]
    pub const fn critical_seconds(self) -> u64 {
        self.critical_seconds
    }

    #[must_use]
    pub const fn hysteresis_seconds(self) -> u64 {
        self.hysteresis_seconds
    }

    #[must_use]
    pub const fn reserve_bytes(self) -> u64 {
        self.reserve_bytes
    }

    fn classify(self, usable_bytes: u64, remaining: Option<u64>, margin: u64) -> DiskHealthLevel {
        if usable_bytes == 0 {
            return DiskHealthLevel::Full;
        }
        match remaining {
            Some(seconds) if seconds < self.critical_seconds + margin => DiskHealthLevel::Critical,
            Some(seconds) if seconds < self.warning_seconds + margin => DiskHealthLevel::Warning,
            None | Some(_) => DiskHealthLevel::Healthy,
        }
    }
}

/// One caller-supplied observation of a storage root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskSample {
    pub free_bytes: u64,
    /// Measured sustained write throughput, if known.
    pub sustained_write_bytes_per_second: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskHealthReport {
    pub required_bytes_per_second: u64,
    /// Free bytes minus the configured reserve.
    pub usable_bytes: u64,
    /// Whole seconds of recording left; `None` when nothing is recording.
    pub remaining_seconds: Option<u64>,
    pub level: DiskHealthLevel,
    pub previous: DiskHealthLevel,
    /// Bytes per second the disk falls short of the load, when measured.
    pub throughput_shortfall: Option<u64>,
}

impl DiskHealthReport {
    #[must_use]
    pub fn changed(&self) -> bool {
        self.level != self.previous
    }
}

/// Stateful low-space classifier for one storage root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskHealthMonitor {
    thresholds: DiskThresholds,
    level: DiskHealthLevel,
}

impl DiskHealthMonitor {
    #[must_use]
    pub const fn new(thresholds: DiskThresholds) -> Self {
        Self {
            thresholds,
            level: DiskHealthLevel::Healthy,
        }
    }

    #[must_use]
    pub const fn level(&self) -> DiskHealthLevel {
        self.level
    }

    /// Predicts remaining time for `required_bytes_per_second` and updates the
    /// alert level: worsening applies immediately, improving only past the
    /// hysteresis margin.
    pub fn observe(
        &mut self,
        required_bytes_per_second: u64,
        sample: DiskSample,
    ) -> DiskHealthReport {
        let thresholds = self.thresholds;
        let usable_bytes = sample.free_bytes.saturating_sub(thresholds.reserve_bytes);
        let remaining_seconds =
            (required_bytes_per_second != 0).then(|| usable_bytes / required_bytes_per_second);
        let raw = thresholds.classify(usable_bytes, remaining_seconds, 0);
        let previous = self.level;
        self.level = if raw >= previous {
            raw
        } else {
            let clearing = thresholds.classify(
                usable_bytes,
                remaining_seconds,
                thresholds.hysteresis_seconds,
            );
            previous.min(clearing)
        };
        DiskHealthReport {
            required_bytes_per_second,
            usable_bytes,
            remaining_seconds,
            level: self.level,
            previous,
            throughput_shortfall: sample
                .sustained_write_bytes_per_second
                .map(|measured| required_bytes_per_second.saturating_sub(measured)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiskHealthError {
    TooManyCameras(usize),
    DuplicateCamera(CameraId),
    UnknownCamera(CameraId),
    InvalidBitrate(CameraId),
    InvalidOverhead(u16),
    InvalidThresholds,
}

impl fmt::Display for DiskHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyCameras(count) => write!(
                formatter,
                "{count} recording cameras exceed the limit of {MAX_REPLAY_SOURCES}"
            ),
            Self::DuplicateCamera(id) => write!(formatter, "camera {id} has two bitrates"),
            Self::UnknownCamera(id) => write!(formatter, "camera {id} is not a replay source"),
            Self::InvalidBitrate(id) => write!(
                formatter,
                "camera {id} bitrate must be within 1..={MAX_CAMERA_BITS_PER_SECOND} bit/s"
            ),
            Self::InvalidOverhead(permille) => write!(
                formatter,
                "overhead {permille}‰ exceeds {MAX_OVERHEAD_PERMILLE}‰"
            ),
            Self::InvalidThresholds => formatter.write_str(
                "disk thresholds require 0 < critical < warning and a non-overflowing margin",
            ),
        }
    }
}

impl std::error::Error for DiskHealthError {}
