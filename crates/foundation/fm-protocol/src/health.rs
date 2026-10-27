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
//!
//! # Acknowledgements and silences (Protocol 2.109)
//!
//! An operator acknowledges a firing alert, or silences a rule's
//! notifications, for a bounded time with a `health_alert_request`
//! ([`HealthAlertRequest`]): `request_id`, `rule`, `action`
//! (`acknowledge`, `silence`, or `unsilence`), and `duration_ms` (1 to
//! [`MAX_HEALTH_ALERT_HOLD_MS`], present exactly for `acknowledge` and
//! `silence`). It needs `transition`; the daemon answers with a
//! `health_status` carrying the same `request_id`, or an `error` naming it.
//! An acknowledgement ends when its rule resolves, and one that expires
//! while the rule still fires raises a `reminder`. A silence keeps the
//! rule's state tracked but raises no notification until it ends; silencing
//! a silenced rule replaces its end, and `unsilence` ends it at once. Both
//! are runtime state, gone when the daemon restarts.
//!
//! Each `alerts` entry ends with `:<acknowledged_until_ms>` (or `:none`),
//! and `silences` lists `<rule>:<until_ms>` per silenced rule in strictly
//! increasing rule order, `;`-separated (empty when none). Both ends lie
//! after `uptime_ms`: an expired acknowledgement or silence is not sent.

use crate::ServerIdentity;

/// One alert rule, in wire (rule ID) order; since schema 85 a show may
/// set each rule's thresholds and hold ([`fm_types::alert_rules`]).
pub use fm_types::alert_rules::HealthAlertRule;
/// A show's settings for its alert rules (Protocol 2.112): see
/// [`fm_types::alert_rules`].
pub use fm_types::alert_rules::{
    AlertReading, AlertRuleDefaults, AlertRuleError, AlertRuleSetting, MAX_ALERT_HOLD_MS,
    check_settings as check_alert_rule_settings, effective_setting as effective_alert_rule,
};

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

/// The longest one acknowledgement or silence may last: a day.
pub const MAX_HEALTH_ALERT_HOLD_MS: u64 = 86_400_000;

/// One firing alert.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HealthAlert {
    pub rule: HealthAlertRule,
    pub severity: HealthSeverity,
    /// When it fired, on the `uptime_ms` clock.
    pub since_ms: u64,
    /// The rule's latest reading times 1000, when it has one.
    pub value_milli: Option<i64>,
    /// When its acknowledgement ends (exclusive), on the `uptime_ms` clock,
    /// while one holds (Protocol 2.109).
    pub acknowledged_until_ms: Option<u64>,
}

/// One silenced rule (Protocol 2.109).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HealthSilence {
    pub rule: HealthAlertRule,
    /// When the silence ends (exclusive), on the `uptime_ms` clock.
    pub until_ms: u64,
}

/// What an operator does to one alert rule (Protocol 2.109).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HealthAlertAction {
    /// Acknowledges the rule's firing alert for `duration_ms`.
    Acknowledge { duration_ms: u64 },
    /// Silences the rule's notifications for `duration_ms`.
    Silence { duration_ms: u64 },
    /// Ends the rule's silence.
    Unsilence,
}

impl HealthAlertAction {
    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Acknowledge { .. } => "acknowledge",
            Self::Silence { .. } => "silence",
            Self::Unsilence => "unsilence",
        }
    }

    /// How long the action holds, for those that hold.
    #[must_use]
    pub const fn duration_ms(self) -> Option<u64> {
        match self {
            Self::Acknowledge { duration_ms } | Self::Silence { duration_ms } => Some(duration_ms),
            Self::Unsilence => None,
        }
    }

    /// The action `token` names, holding for `duration_ms` exactly when it
    /// holds at all, and for 1 to [`MAX_HEALTH_ALERT_HOLD_MS`].
    #[must_use]
    pub fn from_parts(token: &str, duration_ms: Option<u64>) -> Option<Self> {
        let hold = duration_ms.filter(|ms| (1..=MAX_HEALTH_ALERT_HOLD_MS).contains(ms));
        match (token, duration_ms) {
            ("acknowledge", Some(_)) => hold.map(|duration_ms| Self::Acknowledge { duration_ms }),
            ("silence", Some(_)) => hold.map(|duration_ms| Self::Silence { duration_ms }),
            ("unsilence", None) => Some(Self::Unsilence),
            _ => None,
        }
    }

    /// Whether [`Self::from_parts`] would accept this action.
    #[must_use]
    pub fn is_valid(self) -> bool {
        Self::from_parts(self.token(), self.duration_ms()) == Some(self)
    }
}

/// Acknowledges, silences, or unsilences one alert rule (Protocol 2.109).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthAlertRequest {
    pub protocol: crate::ProtocolVersion,
    pub request_id: String,
    pub rule: HealthAlertRule,
    pub action: HealthAlertAction,
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
    /// Every silenced rule, in strictly increasing rule order (Protocol
    /// 2.109).
    pub silences: Vec<HealthSilence>,
}

impl DaemonHealth {
    /// Whether the sample keeps the wire invariants: utilization within
    /// [`MAX_UTILIZATION_PERMILLE`], at most one alert per rule in strictly
    /// increasing rule order, at most [`MAX_HEALTH_NOTIFICATIONS`]
    /// notifications in strictly increasing sequence order, no time past
    /// `uptime_ms`, and at most one silence per rule in strictly increasing
    /// rule order, every acknowledgement and silence ending after
    /// `uptime_ms`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let utilization = [self.cpu_permille, self.gpu_permille]
            .into_iter()
            .flatten()
            .all(|permille| permille <= MAX_UTILIZATION_PERMILLE);
        let alerts = self.alerts.len() <= HealthAlertRule::ALL.len()
            && self
                .alerts
                .windows(2)
                .all(|pair| pair[0].rule < pair[1].rule)
            && self.alerts.iter().all(|alert| {
                alert.since_ms <= self.uptime_ms
                    && alert
                        .acknowledged_until_ms
                        .is_none_or(|until| until > self.uptime_ms)
            });
        let silences = self.silences.len() <= HealthAlertRule::ALL.len()
            && self
                .silences
                .windows(2)
                .all(|pair| pair[0].rule < pair[1].rule)
            && self
                .silences
                .iter()
                .all(|silence| silence.until_ms > self.uptime_ms);
        let notifications = self.notifications.len() <= MAX_HEALTH_NOTIFICATIONS
            && self
                .notifications
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
            && self
                .notifications
                .iter()
                .all(|notification| notification.at_ms <= self.uptime_ms);
        utilization && alerts && notifications && silences
    }

    /// The silence of `rule`, if one holds.
    #[must_use]
    pub fn silence(&self, rule: HealthAlertRule) -> Option<&HealthSilence> {
        self.silences.iter().find(|silence| silence.rule == rule)
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
    /// The `health_alert_request` this record answers (Protocol 2.109);
    /// `None` for the once-a-second record.
    pub request_id: Option<String>,
    pub health: DaemonHealth,
}
