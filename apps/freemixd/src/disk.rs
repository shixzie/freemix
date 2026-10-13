//! Bounded disk preflight and low-space alerting for configured program
//! recording.
//!
//! The preflight is fail-closed: a configured `--record-program` output whose
//! parent cannot be queried or has too little free space refuses daemon
//! startup. Once running, the render loop feeds free-space samples into a
//! hysteretic alert so a recording approaching the filesystem floor is
//! reported exactly at the edges, never per frame.

use std::{error::Error, fmt, io, path::Path};

use fm_observability::{
    AlertPolicy, AlertState, AlertTransition, ThresholdAlert, ThresholdDirection,
};

/// Free bytes a configured recording output's parent must have available
/// before daemon startup proceeds.
pub const RECORDING_MINIMUM_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// Free bytes at or below which a running recording's alert fires.
pub const DISK_LOW_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Free bytes at or above which a firing recording alert clears.
pub const DISK_CLEAR_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Failure of the configured recording disk preflight.
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
                write!(formatter, "recording free space is unavailable: {kind}")
            }
            Self::BelowMinimum { available } => write!(
                formatter,
                "recording requires at least {RECORDING_MINIMUM_FREE_BYTES} free bytes, \
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

/// Pure decision logic for the configured recording minimum, kept separate so
/// it is testable without a real filesystem.
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
    let available =
        available_bytes(path).map_err(|error| DiskPreflightError::Unavailable(error.kind()))?;
    preflight_available(available)
}

/// Hysteretic free-space alert for a configured recording output.
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
    /// Creates an inactive alert with the daemon's recording policy.
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
                .expect("the recording free-space constants form a hysteresis band"),
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
}
