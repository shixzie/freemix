//! The daemon's utilization and alerts in the status bar's health strip
//! (`RC-015`).
//!
//! A daemon that reports `health_status` (Protocol 2.100) gets three more
//! lamps: `CPU` (the daemon process's share of every logical CPU), `GPU` (the
//! proxy: timestamped compositor render-pass time per wall second), and an
//! alert lamp that is green while nothing fires and names every firing alert
//! otherwise, with the most recent notification in its help.

use fm_protocol::{
    DaemonHealth, HealthAlert, HealthAlertRule, HealthNotification, HealthNotificationKind,
    HealthSeverity,
};

use crate::StudioUiState;
use crate::health::HealthItem;
use crate::l10n::{Arg, Locale, Msg};
use crate::theme::Tone;

/// Utilization at or above this permille reads amber.
const BUSY_PERMILLE: u16 = 750;

/// The health strip's daemon lamps, when the daemon reports its health.
pub(crate) fn health_items(l: Locale, state: &StudioUiState) -> Vec<HealthItem> {
    let Some(status) = state.health.as_ref() else {
        return Vec::new();
    };
    let health = &status.health;
    vec![
        utilization_item(l, health, Utilization::Cpu),
        utilization_item(l, health, Utilization::Gpu),
        alerts_item(l, health),
    ]
}

#[derive(Clone, Copy)]
enum Utilization {
    Cpu,
    Gpu,
}

/// One utilization lamp: unlit while unavailable, green when idle enough,
/// amber from 75%, red while its saturation alert fires.
fn utilization_item(l: Locale, health: &DaemonHealth, which: Utilization) -> HealthItem {
    let (permille, label, none, hover, none_hover, rule) = match which {
        Utilization::Cpu => (
            health.cpu_permille,
            Msg::HealthCpu,
            Msg::HealthCpuUnavailable,
            Msg::HealthCpuHover,
            Msg::HealthCpuUnavailableHover,
            HealthAlertRule::CpuSaturated,
        ),
        Utilization::Gpu => (
            health.gpu_permille,
            Msg::HealthGpu,
            Msg::HealthGpuUnavailable,
            Msg::HealthGpuHover,
            Msg::HealthGpuUnavailableHover,
            HealthAlertRule::GpuSaturated,
        ),
    };
    let Some(permille) = permille else {
        return HealthItem {
            label: l.text(none).to_owned(),
            tone: Tone::Idle,
            hover: l.text(none_hover).to_owned(),
        };
    };
    let value = l.percent(i64::from(permille.saturating_add(5) / 10));
    HealthItem {
        label: l.format(label, &[("value", Arg::Str(&value))]),
        tone: if health.alert(rule).is_some() {
            Tone::Fault
        } else if permille >= BUSY_PERMILLE {
            Tone::Pending
        } else {
            Tone::Ready
        },
        hover: l.format(hover, &[("value", Arg::Str(&value))]),
    }
}

const fn rule_word(rule: HealthAlertRule) -> Msg {
    match rule {
        HealthAlertRule::CpuSaturated => Msg::AlertRuleCpuSaturated,
        HealthAlertRule::DiskLow => Msg::AlertRuleDiskLow,
        HealthAlertRule::FrameLate => Msg::AlertRuleFrameLate,
        HealthAlertRule::FramesDropped => Msg::AlertRuleFramesDropped,
        HealthAlertRule::GpuSaturated => Msg::AlertRuleGpuSaturated,
        HealthAlertRule::OutputFailed => Msg::AlertRuleOutputFailed,
    }
}

const fn severity_word(severity: HealthSeverity) -> Msg {
    match severity {
        HealthSeverity::Info => Msg::AlertSeverityInfo,
        HealthSeverity::Warning => Msg::AlertSeverityWarning,
        HealthSeverity::Critical => Msg::AlertSeverityCritical,
    }
}

const fn kind_word(kind: HealthNotificationKind) -> Msg {
    match kind {
        HealthNotificationKind::Firing => Msg::AlertKindFiring,
        HealthNotificationKind::Resolved => Msg::AlertKindResolved,
        HealthNotificationKind::Reminder => Msg::AlertKindReminder,
    }
}

/// `CPU saturated (warning) for 12.0 s`.
fn alert_text(l: Locale, health: &DaemonHealth, alert: &HealthAlert) -> String {
    l.format(
        Msg::AlertLine,
        &[
            ("rule", Arg::Str(l.text(rule_word(alert.rule)))),
            ("severity", Arg::Str(l.text(severity_word(alert.severity)))),
            (
                "age",
                Arg::Str(&l.seconds_tenths(health.uptime_ms.saturating_sub(alert.since_ms))),
            ),
        ],
    )
}

/// `Last: output failed cleared 3.0 s ago`.
fn notification_text(
    l: Locale,
    health: &DaemonHealth,
    notification: &HealthNotification,
) -> String {
    l.format(
        Msg::AlertLastNotification,
        &[
            ("rule", Arg::Str(l.text(rule_word(notification.rule)))),
            ("kind", Arg::Str(l.text(kind_word(notification.kind)))),
            (
                "age",
                Arg::Str(&l.seconds_tenths(health.uptime_ms.saturating_sub(notification.at_ms))),
            ),
        ],
    )
}

/// Green `NO ALERTS`, or `N ALERTS` amber for warnings and red once any is
/// critical; the help names each firing alert on one line and the most
/// recent notification on the next.
fn alerts_item(l: Locale, health: &DaemonHealth) -> HealthItem {
    let mut hover = if health.alerts.is_empty() {
        l.text(Msg::HealthAlertsNoneHover).to_owned()
    } else {
        let firing: Vec<_> = health
            .alerts
            .iter()
            .map(|alert| alert_text(l, health, alert))
            .collect();
        format!("{}\n{}", l.text(Msg::HealthAlertsHover), firing.join(" · "))
    };
    if let Some(last) = health.notifications.last() {
        hover.push('\n');
        hover.push_str(&notification_text(l, health, last));
    }
    let count = u64::try_from(health.alerts.len()).unwrap_or(u64::MAX);
    HealthItem {
        label: if count == 0 {
            l.text(Msg::HealthAlertsNone).to_owned()
        } else {
            l.format(Msg::HealthAlerts, &[("count", Arg::Count(count))])
        },
        tone: match health.worst_severity() {
            None => Tone::Ready,
            Some(HealthSeverity::Critical) => Tone::Fault,
            Some(HealthSeverity::Warning | HealthSeverity::Info) => Tone::Pending,
        },
        hover,
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::{HealthStatusMessage, ServerIdentity};
    use fm_ui_model::i18n::Language;

    use super::*;

    fn state(health: DaemonHealth) -> StudioUiState {
        let mut state = StudioUiState::new(crate::StudioConnectionStatus::Ready);
        state.health = Some(Box::new(HealthStatusMessage {
            server: ServerIdentity {
                engine_id: "e".into(),
                project_id: "1".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            sequence: 1,
            health,
        }));
        state
    }

    #[test]
    fn no_health_adds_no_lamp() {
        let l = Locale::new(Language::English);
        let state = StudioUiState::new(crate::StudioConnectionStatus::Ready);
        assert_eq!(health_items(l, &state), []);
    }

    #[test]
    fn quiet_daemon_reads_green_with_unavailable_gpu() {
        let l = Locale::new(Language::English);
        let items = health_items(
            l,
            &state(DaemonHealth {
                uptime_ms: 5_000,
                cpu_permille: Some(124),
                ..DaemonHealth::default()
            }),
        );
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["CPU 12%", "GPU —", "NO ALERTS"]);
        assert_eq!(items[0].tone, Tone::Ready);
        assert_eq!(items[1].tone, Tone::Idle);
        assert_eq!(items[2].tone, Tone::Ready);
        assert!(items.iter().all(|item| !item.hover.is_empty()));
    }

    #[test]
    fn every_lamp_explains_itself_in_spanish() {
        let health = DaemonHealth {
            uptime_ms: 5_000,
            cpu_permille: Some(400),
            gpu_permille: Some(100),
            alerts: vec![HealthAlert {
                rule: HealthAlertRule::DiskLow,
                severity: HealthSeverity::Critical,
                since_ms: 1_000,
                value_milli: None,
            }],
            ..DaemonHealth::default()
        };
        let en = health_items(Locale::new(Language::English), &state(health.clone()));
        let es = health_items(Locale::new(Language::Spanish), &state(health));
        assert_eq!(es[2].label, "1 ALERTA");
        for (en, es) in en.iter().zip(&es) {
            assert_ne!(en.hover, es.hover);
        }
        assert!(es[2].hover.contains("poco disco en las salidas (crítica)"));
        let quiet = health_items(
            Locale::new(Language::Spanish),
            &state(DaemonHealth::default()),
        );
        assert_eq!(quiet[0].label, "CPU —");
        assert_eq!(quiet[2].label, "SIN ALERTAS");
    }

    #[test]
    fn firing_alerts_color_and_name_themselves() {
        let l = Locale::new(Language::English);
        let health = DaemonHealth {
            uptime_ms: 20_000,
            cpu_permille: Some(950),
            gpu_permille: Some(800),
            alerts: vec![
                HealthAlert {
                    rule: HealthAlertRule::CpuSaturated,
                    severity: HealthSeverity::Warning,
                    since_ms: 8_000,
                    value_milli: Some(950),
                },
                HealthAlert {
                    rule: HealthAlertRule::OutputFailed,
                    severity: HealthSeverity::Critical,
                    since_ms: 19_000,
                    value_milli: Some(1_000),
                },
            ],
            notifications: vec![HealthNotification {
                sequence: 2,
                rule: HealthAlertRule::OutputFailed,
                severity: HealthSeverity::Critical,
                kind: HealthNotificationKind::Firing,
                at_ms: 19_000,
                value_milli: Some(1_000),
            }],
        };
        let items = health_items(l, &state(health));
        assert_eq!(items[0].label, "CPU 95%");
        assert_eq!(items[0].tone, Tone::Fault);
        assert_eq!(items[1].tone, Tone::Pending);
        assert_eq!(items[2].label, "2 ALERTS");
        assert_eq!(items[2].tone, Tone::Fault);
        assert!(
            items[2]
                .hover
                .contains("CPU saturated (warning) for 12.0 s")
        );
        assert!(
            items[2]
                .hover
                .contains("output failed (critical) for 1.0 s")
        );
        assert!(items[2].hover.contains("output failed fired 1.0 s ago"));
        assert!(items[2].hover.lines().count() <= 3);
    }
}
