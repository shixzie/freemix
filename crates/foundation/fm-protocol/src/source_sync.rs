//! Live source clock drift telemetry (Protocol 2.76, `AU-004`).
//!
//! Every live source a native daemon ingests (capture inputs, network
//! streams, system audio inputs) runs on its own clock. Its receiver converts
//! its audio into the engine clock with a drift-correcting resampler and
//! keeps its pictures following that audio. One [`SourceSyncSample`] reports
//! one source's loop: the estimated drift and the applied correction (which
//! together with the two sample rates give the resampling ratio), the audio
//! backlog against its target, and its recovery counts.
//!
//! `source_sync_status` is a lossy, non-resumable, latest-wins record like
//! `recorder_status`: server identity, an independent strictly increasing
//! `sequence`, and `sources`, one sample per running live source in strictly
//! increasing input order, `;`-separated (empty when no live source runs).
//! Each sample is
//! `<input>:<kind>:<state>:<input_hz>:<output_hz>:<drift_mppm>:<correction_mppm>:<buffer_us>:<target_us>:<resyncs>:<underruns>:<overruns>:<picture_drops>:<picture_repeats>:<restarts>`
//! with signed canonical milli-ppm and unsigned canonical counts. The same
//! samples answer `diagnostics_response`'s `source_sync` field.

use crate::WireInputId;

/// The kind of live source a sample reports.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceSyncKind {
    /// A camera / capture input.
    Capture,
    /// A network stream input.
    Stream,
    /// A system audio input.
    SystemAudio,
}

impl SourceSyncKind {
    /// Every kind in wire order.
    pub const ALL: [Self; 3] = [Self::Capture, Self::Stream, Self::SystemAudio];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Stream => "stream",
            Self::SystemAudio => "system_audio",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// Where a source's drift loop is.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SourceClockState {
    /// Filling its buffer to the target before playing.
    #[default]
    Priming,
    /// Playing with drift correction.
    Running,
    /// Recovering from an underrun or overrun.
    Recovering,
}

impl SourceClockState {
    /// Every state in wire order.
    pub const ALL: [Self; 3] = [Self::Priming, Self::Running, Self::Recovering];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Priming => "priming",
            Self::Running => "running",
            Self::Recovering => "recovering",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.token() == token)
    }
}

/// The largest sample rate a sample may name, in hertz.
pub const MAX_SOURCE_SYNC_RATE_HZ: u32 = 768_000;
/// The largest drift or correction magnitude a sample may carry, in
/// milli-ppm (one part in a thousand of the nominal rate).
pub const MAX_SOURCE_SYNC_MPPM: i64 = 1_000_000_000;
/// The most samples one `source_sync_status` (or diagnostics response)
/// carries; a daemon with more live sources reports the lowest inputs.
pub const MAX_SOURCE_SYNC_SAMPLES: usize = 64;

/// One live source's drift loop.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceSyncSample {
    pub input: WireInputId,
    pub kind: SourceSyncKind,
    pub state: SourceClockState,
    /// The source's audio sample rate.
    pub input_rate_hz: u32,
    /// The engine clock's sample rate.
    pub output_rate_hz: u32,
    /// Estimated source-vs-engine clock drift, in milli-ppm.
    pub drift_mppm: i64,
    /// Ratio correction currently applied, in milli-ppm; positive consumes
    /// the source faster.
    pub correction_mppm: i64,
    /// Unread source audio, in microseconds.
    pub buffer_us: u64,
    /// The backlog the loop steers to, in microseconds.
    pub target_us: u64,
    /// Read-side hard resyncs (a backlog far beyond the target skipped).
    pub resyncs: u64,
    /// Times the source stopped delivering and the buffer ran dry.
    pub underruns: u64,
    /// Times the buffer overflowed and audio was dropped.
    pub overruns: u64,
    /// Extra pictures dropped to follow the audio.
    pub picture_drops: u64,
    /// Pictures repeated to wait for the audio.
    pub picture_repeats: u64,
    /// Connections (or other restarts) of the source.
    pub restarts: u64,
}

impl SourceSyncSample {
    /// Whether the sample keeps the wire invariants: both rates in
    /// `1..=MAX_SOURCE_SYNC_RATE_HZ` and both ppm values within
    /// [`MAX_SOURCE_SYNC_MPPM`].
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let rates = (1..=MAX_SOURCE_SYNC_RATE_HZ).contains(&self.input_rate_hz)
            && (1..=MAX_SOURCE_SYNC_RATE_HZ).contains(&self.output_rate_hz);
        let ppm = self.drift_mppm.unsigned_abs() <= MAX_SOURCE_SYNC_MPPM.unsigned_abs()
            && self.correction_mppm.unsigned_abs() <= MAX_SOURCE_SYNC_MPPM.unsigned_abs();
        rates && ppm
    }

    /// Estimated drift in ppm.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // bounded by MAX_SOURCE_SYNC_MPPM
    pub fn drift_ppm(&self) -> f64 {
        self.drift_mppm as f64 / 1_000.0
    }

    /// Applied correction in ppm.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // bounded by MAX_SOURCE_SYNC_MPPM
    pub fn correction_ppm(&self) -> f64 {
        self.correction_mppm as f64 / 1_000.0
    }

    /// Source samples consumed per engine sample: the nominal rate ratio
    /// scaled by the applied correction.
    #[must_use]
    pub fn resampling_ratio(&self) -> f64 {
        f64::from(self.input_rate_hz) / f64::from(self.output_rate_hz.max(1))
            * (1.0 + self.correction_ppm() * 1e-6)
    }

    /// Backlog in milliseconds.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // microseconds of audio
    pub fn buffer_ms(&self) -> f64 {
        self.buffer_us as f64 / 1_000.0
    }

    /// Target backlog in milliseconds.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // microseconds of audio
    pub fn target_ms(&self) -> f64 {
        self.target_us as f64 / 1_000.0
    }

    /// Hard resyncs plus overruns plus underruns: every audible recovery.
    #[must_use]
    pub const fn faults(&self) -> u64 {
        self.resyncs
            .saturating_add(self.underruns)
            .saturating_add(self.overruns)
    }
}

/// Lossy, non-resumable live source drift telemetry (Protocol 2.76).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSyncStatusMessage {
    pub server: crate::ServerIdentity,
    pub sequence: u64,
    /// One sample per running live source, in strictly increasing input
    /// order; empty when none runs.
    pub sources: Vec<SourceSyncSample>,
}
