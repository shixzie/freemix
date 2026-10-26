//! The daemon's utilization and alerts in the status bar's health strip
//! (`RC-015`).
//!
//! A daemon that reports `health_status` (Protocol 2.100) gets three more
//! lamps: `CPU` (the daemon process's share of every logical CPU), `GPU` (the
//! proxy: timestamped compositor render-pass time per wall second), and an
//! alert lamp that is green while nothing fires and names every firing alert
//! otherwise, with the most recent notification in its help. Once no
//! `health_status` has arrived for [`HEALTH_STALE_AFTER`] while connected,
//! all three read stale (amber) rather than showing the last report as
//! current.

use std::time::Duration;

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

/// The daemon samples its health every second; a report older than this
/// reads stale.
pub(crate) const HEALTH_STALE_AFTER: Duration = Duration::from_secs(5);

/// The health strip's daemon lamps, when the daemon reports its health;
/// `age` is how long ago the latest report arrived.
pub(crate) fn health_items(
    l: Locale,
    state: &StudioUiState,
    age: Option<Duration>,
) -> Vec<HealthItem> {
    let Some(status) = state.health.as_ref() else {
        return Vec::new();
    };
    if let Some(age) = age.filter(|age| *age > HEALTH_STALE_AFTER) {
        let hover = l.format(
            Msg::HealthDaemonStaleHover,
            &[(
                "age",
                Arg::Str(&l.seconds_tenths(u64::try_from(age.as_millis()).unwrap_or(u64::MAX))),
            )],
        );
        return [
            Msg::HealthCpuStale,
            Msg::HealthGpuStale,
            Msg::HealthAlertsStale,
        ]
        .into_iter()
        .map(|label| HealthItem {
            label: l.text(label).to_owned(),
            tone: Tone::Pending,
            hover: hover.clone(),
        })
        .collect();
    }
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
        HealthAlertRule::EncoderSaturated => Msg::AlertRuleEncoderSaturated,
        HealthAlertRule::StreamReconnecting => Msg::AlertRuleStreamReconnecting,
        HealthAlertRule::SyncDegraded => Msg::AlertRuleSyncDegraded,
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
            request_id: None,
        }));
        state
    }

    #[test]
    fn no_health_adds_no_lamp() {
        let l = Locale::new(Language::English);
        let state = StudioUiState::new(crate::StudioConnectionStatus::Ready);
        assert_eq!(health_items(l, &state, None), []);
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
            None,
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
                acknowledged_until_ms: None,
            }],
            ..DaemonHealth::default()
        };
        let en = health_items(Locale::new(Language::English), &state(health.clone()), None);
        let es = health_items(Locale::new(Language::Spanish), &state(health), None);
        assert_eq!(es[2].label, "1 ALERTA");
        for (en, es) in en.iter().zip(&es) {
            assert_ne!(en.hover, es.hover);
        }
        assert!(es[2].hover.contains("poco disco en las salidas (crítica)"));
        let quiet = health_items(
            Locale::new(Language::Spanish),
            &state(DaemonHealth::default()),
            None,
        );
        assert_eq!(quiet[0].label, "CPU —");
        assert_eq!(quiet[2].label, "SIN ALERTAS");
    }

    /// While connected, a daemon whose `health_status` stopped arriving
    /// reads stale on every lamp instead of its last report as current.
    #[test]
    fn a_report_that_stops_arriving_reads_stale() {
        let l = Locale::new(Language::English);
        let state = state(DaemonHealth {
            uptime_ms: 20_000,
            cpu_permille: Some(950),
            alerts: vec![HealthAlert {
                rule: HealthAlertRule::CpuSaturated,
                severity: HealthSeverity::Warning,
                since_ms: 8_000,
                value_milli: Some(950),
                acknowledged_until_ms: None,
            }],
            ..DaemonHealth::default()
        });
        let current = health_items(l, &state, Some(Duration::from_secs(1)));
        assert_eq!(current[0].label, "CPU 95%");
        assert_eq!(current[0].tone, Tone::Fault);

        let mut tracker = crate::health::HealthTracker::default();
        let arrived = std::time::Instant::now();
        tracker.observe(&state, arrived);
        // The same report seen again later does not refresh it.
        tracker.observe(&state, arrived + Duration::from_secs(4));
        let items =
            crate::health::health_items(l, &state, &tracker, arrived + Duration::from_secs(6));
        let labels: Vec<_> = items
            .iter()
            .rev()
            .take(3)
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, ["ALERTS STALE", "GPU STALE", "CPU STALE"]);
        for item in items.iter().rev().take(3) {
            assert_eq!(item.tone, Tone::Pending);
            assert!(item.hover.contains("6.0 s"), "{}", item.hover);
        }
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
                    acknowledged_until_ms: None,
                },
                HealthAlert {
                    rule: HealthAlertRule::OutputFailed,
                    severity: HealthSeverity::Critical,
                    since_ms: 19_000,
                    value_milli: Some(1_000),
                    acknowledged_until_ms: None,
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
            silences: Vec::new(),
        };
        let items = health_items(l, &state(health), None);
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

    /// Every rule the daemon can report has its own non-empty word in both
    /// languages, so no two alerts read alike.
    #[test]
    fn every_alert_rule_has_its_own_word_in_both_languages() {
        let en = Locale::new(Language::English);
        let es = Locale::new(Language::Spanish);
        let mut english = std::collections::BTreeSet::new();
        let mut spanish = std::collections::BTreeSet::new();
        for rule in HealthAlertRule::ALL {
            let msg = rule_word(rule);
            let english_word = en.text(msg);
            let spanish_word = es.text(msg);
            assert!(
                !english_word.is_empty(),
                "{} has no English word",
                rule.token()
            );
            assert!(
                !spanish_word.is_empty(),
                "{} has no Spanish word",
                rule.token()
            );
            assert_ne!(
                english_word,
                spanish_word,
                "{} is untranslated",
                rule.token()
            );
            assert!(
                english.insert(english_word),
                "{} shares the English word {english_word:?}",
                rule.token()
            );
            assert!(
                spanish.insert(spanish_word),
                "{} shares the Spanish word {spanish_word:?}",
                rule.token()
            );
        }
        assert_eq!(english.len(), HealthAlertRule::ALL.len());
        assert_eq!(spanish.len(), HealthAlertRule::ALL.len());
    }

    /// The encoder, stream, and sync rules added in Protocol 2.108 name
    /// themselves in the alert lamp's help, in English and Spanish.
    #[test]
    fn encoder_stream_and_sync_alerts_render_in_both_languages() {
        let health = DaemonHealth {
            uptime_ms: 12_000,
            alerts: vec![
                HealthAlert {
                    rule: HealthAlertRule::EncoderSaturated,
                    severity: HealthSeverity::Warning,
                    since_ms: 2_000,
                    value_milli: Some(4_000),
                    acknowledged_until_ms: None,
                },
                HealthAlert {
                    rule: HealthAlertRule::StreamReconnecting,
                    severity: HealthSeverity::Warning,
                    since_ms: 5_000,
                    value_milli: Some(2_000),
                    acknowledged_until_ms: None,
                },
                HealthAlert {
                    rule: HealthAlertRule::SyncDegraded,
                    severity: HealthSeverity::Info,
                    since_ms: 9_000,
                    value_milli: Some(250),
                    acknowledged_until_ms: None,
                },
            ],
            notifications: vec![HealthNotification {
                sequence: 1,
                rule: HealthAlertRule::EncoderSaturated,
                severity: HealthSeverity::Warning,
                kind: HealthNotificationKind::Firing,
                at_ms: 2_000,
                value_milli: Some(4_000),
            }],
            ..DaemonHealth::default()
        };
        let en = health_items(Locale::new(Language::English), &state(health.clone()), None);
        assert_eq!(en[2].label, "3 ALERTS");
        assert_eq!(en[2].tone, Tone::Pending);
        for line in [
            "encoder queue saturated (warning) for 10.0 s",
            "streams reconnecting (warning) for 7.0 s",
            "clock sync degraded (info) for 3.0 s",
            "Last: encoder queue saturated fired 10.0 s ago",
        ] {
            assert!(en[2].hover.contains(line), "{line}: {}", en[2].hover);
        }
        assert!(en[2].hover.lines().count() <= 3);

        let es = health_items(Locale::new(Language::Spanish), &state(health), None);
        assert_eq!(es[2].label, "3 ALERTAS");
        for word in [
            "cola del codificador saturada",
            "emisiones reconectándose",
            "sincronización de reloj degradada",
        ] {
            assert!(es[2].hover.contains(word), "{word}: {}", es[2].hover);
        }

        // The quiet lamp's help names the new rule families too.
        let quiet_en = health_items(
            Locale::new(Language::English),
            &state(DaemonHealth::default()),
            None,
        );
        for family in ["encoder queues", "stream reconnects", "clock sync"] {
            assert!(
                quiet_en[2].hover.contains(family),
                "{family}: {}",
                quiet_en[2].hover
            );
        }
        let quiet_spanish = health_items(
            Locale::new(Language::Spanish),
            &state(DaemonHealth::default()),
            None,
        );
        for family in [
            "colas del codificador",
            "reconexiones de emisiones",
            "sincronización de reloj",
        ] {
            assert!(
                quiet_spanish[2].hover.contains(family),
                "{family}: {}",
                quiet_spanish[2].hover
            );
        }
    }
}
