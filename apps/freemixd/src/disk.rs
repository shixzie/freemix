//! Bounded disk preflight and low-space alerting for configured program
//! outputs: the recording output and the local directories of HLS stream
//! targets.
//!
//! The preflight is fail-closed: a configured `--record-program` output whose
//! parent cannot be queried or has too little free space refuses daemon
//! startup, and every authored local HLS output directory is held to the same
//! floor through its nearest existing ancestor. Once running, the render loop
//! feeds free-space samples into a hysteretic alert so an output approaching
//! the filesystem floor is reported exactly at the edges, never per frame.

use std::{error::Error, fmt, io, path::Path};

use fm_observability::{
    AlertPolicy, AlertState, AlertTransition, ThresholdAlert, ThresholdDirection,
};

/// Free bytes a configured local output must have available before daemon
/// startup proceeds: the parent of a `--record-program` output, or the nearest
/// existing ancestor of an authored local HLS output directory.
pub const RECORDING_MINIMUM_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// Free bytes at or below which the running output alert fires.
pub const DISK_LOW_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Free bytes at or above which a firing output alert clears.
pub const DISK_CLEAR_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How many path components [`existing_ancestor`] inspects before giving up:
/// far beyond any operator-authored output directory while still bounding an
/// adversarial path that names a long chain of nonexistent parents.
const EXISTING_ANCESTOR_DEPTH_LIMIT: usize = 64;

/// Failure of a configured output's disk preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskPreflightError {
    /// The free-space query failed with this I/O kind.
    Unavailable(io::ErrorKind),
    /// The queried filesystem had fewer than [`RECORDING_MINIMUM_FREE_BYTES`]
    /// free bytes. The byte count is carried for diagnosis; no error text
    /// ever carries a path.
    BelowMinimum { available: u64 },
}

impl fmt::Display for DiskPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(kind) => {
                write!(formatter, "output free space is unavailable: {kind}")
            }
            Self::BelowMinimum { available } => write!(
                formatter,
                "an output requires at least {RECORDING_MINIMUM_FREE_BYTES} free bytes, \
                 found {available}"
            ),
        }
    }
}

impl Error for DiskPreflightError {}

/// Queries the free bytes available to the process on the filesystem holding
/// `path`.
///
/// # Errors
///
/// Returns the free-space query failure for a path whose filesystem cannot be
/// inspected.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    fs4::available_space(path)
}

/// The first existing ancestor of `path`, starting with `path` itself.
///
/// Walks up at most [`EXISTING_ANCESTOR_DEPTH_LIMIT`] path components and
/// returns `None` when nothing along that walk exists. A local HLS sink
/// creates its output directory later, so this is the most specific path
/// whose filesystem the daemon can measure now.
#[must_use]
pub fn existing_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    for _ in 0..EXISTING_ANCESTOR_DEPTH_LIMIT {
        let current = candidate?;
        if current.exists() {
            return Some(current);
        }
        candidate = current.parent();
    }
    None
}

/// Pure decision logic for the configured output minimum, kept separate so it
/// is testable without a real filesystem.
///
/// # Errors
///
/// Returns [`DiskPreflightError::BelowMinimum`] carrying `available` when it
/// is below [`RECORDING_MINIMUM_FREE_BYTES`].
fn preflight_available(available: u64) -> Result<u64, DiskPreflightError> {
    if available < RECORDING_MINIMUM_FREE_BYTES {
        Err(DiskPreflightError::BelowMinimum { available })
    } else {
        Ok(available)
    }
}

/// Queries `path`'s free bytes and applies the configured output floor.
///
/// # Errors
///
/// Returns [`DiskPreflightError::Unavailable`] when the free-space query
/// fails, or [`DiskPreflightError::BelowMinimum`] when it succeeds with too
/// little space. Neither variant renders the path.
fn preflight_existing_path(path: &Path) -> Result<u64, DiskPreflightError> {
    let available =
        available_bytes(path).map_err(|error| DiskPreflightError::Unavailable(error.kind()))?;
    preflight_available(available)
}

/// Verifies that `path` — the caller-resolved parent directory of the
/// configured recording output — has at least [`RECORDING_MINIMUM_FREE_BYTES`]
/// free.
///
/// # Errors
///
/// Returns [`DiskPreflightError::Unavailable`] when the free-space query
/// fails, or [`DiskPreflightError::BelowMinimum`] when it succeeds with too
/// little space. Neither variant renders the path.
pub fn preflight_recording_parent(path: &Path) -> Result<u64, DiskPreflightError> {
    preflight_existing_path(path)
}

/// Verifies that `path` — the absolute directory of an authored local HLS
/// stream target — has at least [`RECORDING_MINIMUM_FREE_BYTES`] free.
///
/// The local HLS sink creates the leaf directory later, so the leaf normally
/// does not exist at preflight time: the nearest existing ancestor is what can
/// be measured now, and it lies on the filesystem the sink will publish into.
///
/// # Errors
///
/// Returns [`DiskPreflightError::Unavailable`] when no ancestor exists or the
/// free-space query fails, or [`DiskPreflightError::BelowMinimum`] when the
/// query succeeds with too little space. Neither variant renders the path.
pub fn preflight_local_output(path: &Path) -> Result<u64, DiskPreflightError> {
    let existing =
        existing_ancestor(path).ok_or(DiskPreflightError::Unavailable(io::ErrorKind::NotFound))?;
    preflight_existing_path(existing)
}

/// Hysteretic free-space alert for the configured program outputs.
///
/// Wraps [`ThresholdAlert`] with the daemon's fixed below policy: it fires at
/// or below [`DISK_LOW_FREE_BYTES`] and clears at or above
/// [`DISK_CLEAR_FREE_BYTES`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiskSpaceAlert {
    alert: ThresholdAlert,
}

// The policy thresholds and every sample that can reach the band convert
// between u64 and f64 without meaningful loss: both are exact well past the
// 2 GiB clear watermark.
#[allow(clippy::cast_precision_loss)]
impl DiskSpaceAlert {
    /// Creates an inactive alert with the daemon's fixed policy.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed threshold constants stopped forming a valid
    /// hysteresis band, which the constants rule out.
    #[must_use]
    pub fn new() -> Self {
        Self {
            alert: ThresholdAlert::new(
                AlertPolicy::new(
                    ThresholdDirection::Below,
                    DISK_LOW_FREE_BYTES as f64,
                    DISK_CLEAR_FREE_BYTES as f64,
                )
                .expect("the free-space constants form a hysteresis band"),
            ),
        }
    }

    /// Applies one free-space sample in bytes, returning only edge
    /// transitions.
    ///
    /// The byte count converts to `f64`; values above 2^53 (~8 PiB) round to
    /// the nearest representable byte, far beyond the 1 GiB/2 GiB band, so
    /// the comparison against the thresholds stays exact for any sample this
    /// daemon can observe.
    ///
    /// # Panics
    ///
    /// Panics only if a `u64` byte count converted to `f64` stopped being
    /// finite, which is impossible.
    #[must_use]
    pub fn observe(&mut self, available_bytes: u64) -> AlertTransition {
        self.alert
            .evaluate(available_bytes as f64)
            .expect("a byte count converts to a finite f64")
    }

    /// The current alert state.
    #[must_use]
    pub fn state(&self) -> AlertState {
        self.alert.state()
    }
}

impl Default for DiskSpaceAlert {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn preflight_accepts_exactly_the_minimum() {
        assert_eq!(
            preflight_available(RECORDING_MINIMUM_FREE_BYTES),
            Ok(RECORDING_MINIMUM_FREE_BYTES)
        );
    }

    #[test]
    fn preflight_rejects_one_byte_below_with_the_byte_count() {
        let below = RECORDING_MINIMUM_FREE_BYTES - 1;
        let error = preflight_available(below).expect_err("one byte below is refused");

        assert_eq!(error, DiskPreflightError::BelowMinimum { available: below });
        assert!(error.to_string().contains(&below.to_string()));
    }

    #[test]
    fn disk_alert_fires_at_or_below_the_low_watermark() {
        for available in [DISK_LOW_FREE_BYTES - 1, DISK_LOW_FREE_BYTES] {
            let mut alert = DiskSpaceAlert::new();

            assert_eq!(alert.observe(available), AlertTransition::Fired);
            assert_eq!(alert.state(), AlertState::Firing);
        }
    }

    #[test]
    fn disk_alert_stays_firing_inside_the_band_and_clears_at_the_clear_watermark() {
        let mut alert = DiskSpaceAlert::new();

        assert_eq!(alert.observe(DISK_LOW_FREE_BYTES), AlertTransition::Fired);
        assert_eq!(
            alert.observe(DISK_LOW_FREE_BYTES + 1),
            AlertTransition::None
        );
        assert_eq!(
            alert.observe(DISK_CLEAR_FREE_BYTES - 1),
            AlertTransition::None
        );
        assert_eq!(alert.state(), AlertState::Firing);
        assert_eq!(
            alert.observe(DISK_CLEAR_FREE_BYTES),
            AlertTransition::Cleared
        );
        assert_eq!(alert.state(), AlertState::Inactive);
        assert_eq!(
            alert.observe(DISK_CLEAR_FREE_BYTES + 1),
            AlertTransition::None
        );
    }

    #[test]
    fn available_bytes_reports_a_nonzero_value_for_an_existing_directory() {
        let directory = tempfile::tempdir().unwrap();

        assert!(available_bytes(directory.path()).unwrap() > 0);
    }

    #[test]
    fn preflight_maps_a_missing_path_to_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("absent");

        assert!(matches!(
            preflight_recording_parent(&missing),
            Err(DiskPreflightError::Unavailable(_))
        ));
    }

    #[test]
    fn existing_ancestor_returns_an_existing_path_itself() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(existing_ancestor(directory.path()), Some(directory.path()));
    }

    #[test]
    fn existing_ancestor_walks_up_to_the_nearest_existing_parent() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("hls").join("live");

        assert_eq!(existing_ancestor(&missing), Some(directory.path()));
    }

    #[test]
    fn existing_ancestor_handles_relative_paths() {
        assert_eq!(existing_ancestor(Path::new(".")), Some(Path::new(".")));
        // The walk bottoms out at the empty relative parent: nothing along
        // this path exists, so there is no filesystem to name.
        assert_eq!(
            existing_ancestor(Path::new("freemix-absent-ancestor-test-path/leaf")),
            None
        );
    }

    #[test]
    fn existing_ancestor_stops_after_the_bounded_depth() {
        // The root exists, but the bounded walk gives up before reaching it.
        let mut deep = PathBuf::from("/freemix-absent-ancestor");
        for ordinal in 0..EXISTING_ANCESTOR_DEPTH_LIMIT {
            deep.push(format!("level-{ordinal}"));
        }

        assert_eq!(existing_ancestor(&deep), None);
    }

    #[test]
    fn preflight_local_output_measures_a_missing_leafs_existing_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("hls").join("live");

        // The sink creates the leaf later, so the temp directory's filesystem
        // is what the preflight measures; only an ancestor is reachable at
        // startup.
        let available = preflight_local_output(&missing)
            .expect("the temp filesystem is expected to have enough free space");
        assert!(available >= RECORDING_MINIMUM_FREE_BYTES);
    }

    #[test]
    fn preflight_local_output_is_unavailable_without_an_existing_ancestor() {
        assert_eq!(
            preflight_local_output(Path::new("freemix-absent-ancestor-test-path/leaf")),
            Err(DiskPreflightError::Unavailable(io::ErrorKind::NotFound))
        );
    }
}
