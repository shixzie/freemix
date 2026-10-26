//! Daemon health fields (Protocol 2.100): `uptime_ms`, optional
//! `cpu_permille` and `gpu_permille`, `alerts` as
//! `<rule>:<severity>:<since_ms>:<value>:<acknowledged_until_ms>` entries
//! (the last `none` while unacknowledged; Protocol 2.109), `notifications`
//! as `<sequence>:<rule>:<severity>:<kind>:<at_ms>:<value>` entries, and
//! `silences` as `<rule>:<until_ms>` entries (Protocol 2.109), each list
//! `;`-separated. `<value>` is a canonical signed decimal or `none`. Both
//! `health_status` and `diagnostics_response` carry them.

use crate::{
    CodecError, DaemonHealth, HealthAlert, HealthAlertRule, HealthNotification,
    HealthNotificationKind, HealthSeverity, HealthSilence, MAX_HEALTH_NOTIFICATIONS,
};

use super::source_sync::parse_canonical_i64;
use super::value::parse_canonical_u64;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn encode_value(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

/// A canonical signed decimal or `none`; `bad` names the refused entry.
fn decode_value(value: &str, bad: impl FnOnce() -> CodecError) -> Result<Option<i64>, CodecError> {
    if value == "none" {
        Ok(None)
    } else {
        parse_canonical_i64(value).map(Some).ok_or_else(bad)
    }
}

/// The health fields in record order.
pub(super) fn encode(health: &DaemonHealth) -> Result<Vec<(&'static str, String)>, CodecError> {
    if health.notifications.len() > MAX_HEALTH_NOTIFICATIONS {
        return Err(CodecError::TooManyItems("notifications"));
    }
    if !health.is_valid() {
        return Err(invalid("health", health.uptime_ms.to_string()));
    }
    let mut fields = vec![("uptime_ms", health.uptime_ms.to_string())];
    if let Some(cpu) = health.cpu_permille {
        fields.push(("cpu_permille", cpu.to_string()));
    }
    if let Some(gpu) = health.gpu_permille {
        fields.push(("gpu_permille", gpu.to_string()));
    }
    let alerts = health
        .alerts
        .iter()
        .map(|alert| {
            format!(
                "{}:{}:{}:{}:{}",
                alert.rule.token(),
                alert.severity.token(),
                alert.since_ms,
                encode_value(alert.value_milli),
                alert
                    .acknowledged_until_ms
                    .map_or_else(|| "none".to_owned(), |until| until.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    fields.push(("alerts", alerts));
    let notifications = health
        .notifications
        .iter()
        .map(|notification| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                notification.sequence,
                notification.rule.token(),
                notification.severity.token(),
                notification.kind.token(),
                notification.at_ms,
                encode_value(notification.value_milli),
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    fields.push(("notifications", notifications));
    let silences = health
        .silences
        .iter()
        .map(|silence| format!("{}:{}", silence.rule.token(), silence.until_ms))
        .collect::<Vec<_>>()
        .join(";");
    fields.push(("silences", silences));
    Ok(fields)
}

/// Where [`decode`] reads its fields from.
pub(super) struct HealthFields {
    pub(super) uptime_ms: String,
    pub(super) cpu_permille: Option<String>,
    pub(super) gpu_permille: Option<String>,
    pub(super) alerts: String,
    pub(super) notifications: String,
    pub(super) silences: String,
}

fn permille(field: &'static str, value: Option<String>) -> Result<Option<u16>, CodecError> {
    value
        .map(|value| {
            parse_canonical_u64(&value)
                .and_then(|parsed| u16::try_from(parsed).ok())
                .filter(|&parsed| parsed <= crate::MAX_UTILIZATION_PERMILLE)
                .ok_or_else(|| invalid(field, value))
        })
        .transpose()
}

fn decode_alerts(value: &str) -> Result<Vec<HealthAlert>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<_> = value.split(';').collect();
    if entries.len() > HealthAlertRule::ALL.len() {
        return Err(CodecError::TooManyItems("alerts"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let bad = || invalid("alerts", entry);
            let [rule, severity, since, value, acknowledged] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(bad());
            };
            Ok(HealthAlert {
                rule: HealthAlertRule::from_token(rule).ok_or_else(bad)?,
                severity: HealthSeverity::from_token(severity).ok_or_else(bad)?,
                since_ms: parse_canonical_u64(since).ok_or_else(bad)?,
                value_milli: decode_value(value, bad)?,
                acknowledged_until_ms: match acknowledged {
                    "none" => None,
                    until => Some(parse_canonical_u64(until).ok_or_else(bad)?),
                },
            })
        })
        .collect()
}

fn decode_silences(value: &str) -> Result<Vec<HealthSilence>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<_> = value.split(';').collect();
    if entries.len() > HealthAlertRule::ALL.len() {
        return Err(CodecError::TooManyItems("silences"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let bad = || invalid("silences", entry);
            let (rule, until) = entry.split_once(':').ok_or_else(bad)?;
            Ok(HealthSilence {
                rule: HealthAlertRule::from_token(rule).ok_or_else(bad)?,
                until_ms: parse_canonical_u64(until).ok_or_else(bad)?,
            })
        })
        .collect()
}

fn decode_notifications(value: &str) -> Result<Vec<HealthNotification>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<_> = value.split(';').collect();
    if entries.len() > MAX_HEALTH_NOTIFICATIONS {
        return Err(CodecError::TooManyItems("notifications"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let bad = || invalid("notifications", entry);
            let [sequence, rule, severity, kind, at, value] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(bad());
            };
            Ok(HealthNotification {
                sequence: parse_canonical_u64(sequence).ok_or_else(bad)?,
                rule: HealthAlertRule::from_token(rule).ok_or_else(bad)?,
                severity: HealthSeverity::from_token(severity).ok_or_else(bad)?,
                kind: HealthNotificationKind::from_token(kind).ok_or_else(bad)?,
                at_ms: parse_canonical_u64(at).ok_or_else(bad)?,
                value_milli: decode_value(value, bad)?,
            })
        })
        .collect()
}

/// Decodes the health fields, refusing anything but the exact contract.
pub(super) fn decode(fields: HealthFields) -> Result<DaemonHealth, CodecError> {
    let uptime_ms = parse_canonical_u64(&fields.uptime_ms)
        .ok_or_else(|| invalid("uptime_ms", fields.uptime_ms.clone()))?;
    let health = DaemonHealth {
        uptime_ms,
        cpu_permille: permille("cpu_permille", fields.cpu_permille)?,
        gpu_permille: permille("gpu_permille", fields.gpu_permille)?,
        alerts: decode_alerts(&fields.alerts)?,
        notifications: decode_notifications(&fields.notifications)?,
        silences: decode_silences(&fields.silences)?,
    };
    if !health
        .alerts
        .windows(2)
        .all(|pair| pair[0].rule < pair[1].rule)
        || !health.alerts.iter().all(|alert| {
            alert.since_ms <= uptime_ms
                && alert
                    .acknowledged_until_ms
                    .is_none_or(|until| until > uptime_ms)
        })
    {
        return Err(invalid("alerts", fields.alerts));
    }
    if !health
        .silences
        .windows(2)
        .all(|pair| pair[0].rule < pair[1].rule)
        || !health
            .silences
            .iter()
            .all(|silence| silence.until_ms > uptime_ms)
    {
        return Err(invalid("silences", fields.silences));
    }
    if !health.is_valid() {
        return Err(invalid("notifications", fields.notifications));
    }
    Ok(health)
}
