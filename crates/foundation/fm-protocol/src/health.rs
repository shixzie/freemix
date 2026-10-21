//! Daemon health: CPU and GPU utilization and alerts (Protocol 2.100,
//! `RC-015`).
//!
//! Both daemons sample their statistics once a second and evaluate a fixed
//! set of alert rules over them. One [`DaemonHealth`] reports the latest
//! sample: the daemon's uptime, its process CPU utilization and a GPU
//! utilization proxy in permille (absent while unavailable), every firing
//! alert, and the most recent fired, resolved, and reminder notifications.
//!
//! `health_status` is a lossy, non-resumable, latest-wins record like
//! `source_sync_status`: server identity, an independent strictly increasing
//! `sequence`, and the health fields. `diagnostics_response` carries the same
//! fields. On the wire:
//!
//! - `uptime_ms`: canonical milliseconds since the daemon's statistics began;
//! - `cpu_permille`, `gpu_permille`: optional canonical `0..=1000`;
//! - `alerts`: `<rule>:<severity>:<since_ms>:<value>` per firing rule in
//!   strictly increasing rule order, `;`-separated (empty when none fires);
//! - `notifications`: `<sequence>:<rule>:<severity>:<kind>:<at_ms>:<value>`
//!   per notification in strictly increasing sequence order, at most
//!   [`MAX_HEALTH_NOTIFICATIONS`].
//!
//! `<value>` is the rule's latest reading times 1000 as a canonical signed
//! decimal, or `none`. Times are milliseconds on the `uptime_ms` clock and
//! never exceed it.

use crate::ServerIdentity;

/// One default alert rule, in wire (rule ID) order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HealthAlertRule {
    /// The daemon process kept most of the machine's CPUs busy; the reading
    /// is the share of every logical CPU.
    CpuSaturated,
    /// An output filesystem's free space ran low; the reading is free bytes.
    DiskLow,
    /// Realized frames kept waking half a frame late; the reading is the
    /// late share of frames.
    FrameLate,
    /// Outputs kept dropping pictures; the reading is drops per second.
    FramesDropped,
    /// The GPU utilization proxy stayed near full; the reading is its share.
    GpuSaturated,
    /// A stream target or recorder slot latched a failure; the reading is
    /// the number of failed outputs.
    OutputFailed,
}

impl HealthAlertRule {
    /// Every rule in wire order.
    pub const ALL: [Self; 6] = [
        Self::CpuSaturated,
        Self::DiskLow,
        Self::FrameLate,
        Self::FramesDropped,
        Self::GpuSaturated,
        Self::OutputFailed,
    ];

    /// The wire token, which is also the daemon's rule ID.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::CpuSaturated => "cpu_saturated",
            Self::DiskLow => "disk_low",
            Self::FrameLate => "frame_late",
            Self::FramesDropped => "frames_dropped",
            Self::GpuSaturated => "gpu_saturated",
            Self::OutputFailed => "output_failed",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.token() == token)
    }
}

/// How serious an alert is.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HealthSeverity {
    Info,
    Warning,
    Critical,
}

impl HealthSeverity {
    /// Every severity in wire order.
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Critical];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|severity| severity.token() == token)
    }
}

/// What a notification reports.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HealthNotificationKind {
    /// The rule started firing.
    Firing,
    /// The rule stopped firing.
    Resolved,
    /// The rule still fires after an acknowledgement expired.
    Reminder,
}

impl HealthNotificationKind {
    /// Every kind in wire order.
    pub const ALL: [Self; 3] = [Self::Firing, Self::Resolved, Self::Reminder];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Resolved => "resolved",
            Self::Reminder => "reminder",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// The largest utilization a sample may carry, in permille.
pub const MAX_UTILIZATION_PERMILLE: u16 = 1_000;
/// The most notifications one record carries: the most recent ones.
pub const MAX_HEALTH_NOTIFICATIONS: usize = 16;

/// One firing alert.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HealthAlert {
    pub rule: HealthAlertRule,
    pub severity: HealthSeverity,
    /// When it fired, on the `uptime_ms` clock.
    pub since_ms: u64,
    /// The rule's latest reading times 1000, when it has one.
    pub value_milli: Option<i64>,
}

/// One fired, resolved, or reminder notification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HealthNotification {
    /// Strictly increasing across the daemon's run.
    pub sequence: u64,
    pub rule: HealthAlertRule,
    pub severity: HealthSeverity,
    pub kind: HealthNotificationKind,
    /// When it was raised, on the `uptime_ms` clock.
    pub at_ms: u64,
    /// The rule's reading then times 1000, when it had one.
    pub value_milli: Option<i64>,
}

/// The daemon's latest health sample.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct DaemonHealth {
    /// Milliseconds since the daemon's statistics began.
    pub uptime_ms: u64,
    /// The daemon process's CPU time as a share of every logical CPU, in
    /// permille; `None` where the platform offers no reading or before the
    /// second sample.
    pub cpu_permille: Option<u16>,
    /// The GPU time of the native compositor's timestamped render passes as
    /// a share of wall time, in permille (a proxy, not a vendor counter);
    /// `None` on the simulated daemon or without GPU timestamps.
    pub gpu_permille: Option<u16>,
    /// Every firing alert, in strictly increasing rule order.
    pub alerts: Vec<HealthAlert>,
    /// The most recent notifications, in strictly increasing sequence order.
    pub notifications: Vec<HealthNotification>,
}

impl DaemonHealth {
    /// Whether the sample keeps the wire invariants: utilization within
    /// [`MAX_UTILIZATION_PERMILLE`], alerts in strictly increasing rule
    /// order, at most [`MAX_HEALTH_NOTIFICATIONS`] notifications in strictly
    /// increasing sequence order, and no time past `uptime_ms`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let utilization = [self.cpu_permille, self.gpu_permille]
            .into_iter()
            .flatten()
            .all(|permille| permille <= MAX_UTILIZATION_PERMILLE);
        let alerts = self
            .alerts
            .windows(2)
            .all(|pair| pair[0].rule < pair[1].rule)
            && self
                .alerts
                .iter()
                .all(|alert| alert.since_ms <= self.uptime_ms);
        let notifications = self.notifications.len() <= MAX_HEALTH_NOTIFICATIONS
            && self
                .notifications
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
            && self
                .notifications
                .iter()
                .all(|notification| notification.at_ms <= self.uptime_ms);
        utilization && alerts && notifications
    }

    /// The firing alert of `rule`, if any.
    #[must_use]
    pub fn alert(&self, rule: HealthAlertRule) -> Option<&HealthAlert> {
        self.alerts.iter().find(|alert| alert.rule == rule)
    }

    /// The most serious firing severity, if any alert fires.
    #[must_use]
    pub fn worst_severity(&self) -> Option<HealthSeverity> {
        self.alerts.iter().map(|alert| alert.severity).max()
    }
}

/// Lossy, non-resumable daemon health (Protocol 2.100).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthStatusMessage {
    pub server: ServerIdentity,
    /// Strictly increasing per connection's daemon run.
    pub sequence: u64,
    pub health: DaemonHealth,
}
