//! The camera telemetry every native daemon reports: the counters a captured
//! camera feed produces, whether or not the platform has camera inputs
//! (they are zero where it does not).
//!
//! Extracted from `main.rs` as a mechanical move.

#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeCameraTelemetry {
    pub(crate) configured_sources: u64,
    pub(crate) frames_received: u64,
    pub(crate) frames_ingested: u64,
    pub(crate) native_dropped: u64,
    pub(crate) queue_dropped: u64,
    pub(crate) queue_depth: u64,
    pub(crate) queue_peak_depth: u64,
    pub(crate) continuity_rejected: u64,
    pub(crate) recovery_timeout_discarded: u64,
    pub(crate) terminal_error_discarded: u64,
    pub(crate) terminal_trigger_discarded: u64,
    pub(crate) ready_delivery_depth: u64,
    pub(crate) ready_delivery_discarded: u64,
    pub(crate) cancellation_discarded: u64,
    pub(crate) supervisor_slot_replaced: u64,
    pub(crate) supervisor_slot_depth: u64,
    pub(crate) ingest_failed: u64,
    pub(crate) preflight_depth: u64,
    pub(crate) preflight_discarded: u64,
    pub(crate) recovery_attempts: u64,
    pub(crate) recovery_successes: u64,
    pub(crate) recovery_exhausted: u64,
    pub(crate) recovery_worker_failures: u64,
}
