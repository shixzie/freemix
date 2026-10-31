//! Alert rule settings codec (Protocol 2.146, RC-015).
//!
//! * `alert_rule_set` carries `rule` (a rule token), `enabled` (`0|1`),
//!   `severity` (`info|warning|critical`), `trigger` and `clear` (the reading
//!   times 1000, canonical signed decimals), and `hold_ms` (canonical, at
//!   most an hour). Setting a rule to its default removes the show's own
//!   setting.
//! * A snapshot's `alert_rules` and an `alert_rules_changed` event's
//!   `alert_rules` list the show's own settings as
//!   `<rule>:<enabled>:<severity>:<trigger>:<clear>:<hold_ms>` entries,
//!   `;`-separated in strictly increasing rule order, none at its rule's
//!   default; empty when every rule uses its default.

use crate::{AlertRuleSetting, AlertSeverity, CodecError, HealthAlertRule};

use super::media::{FieldSource, Push};
use super::source_sync::parse_canonical_i64;
use super::value::parse_canonical_u64;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_rule(field: &'static str, value: &str) -> Result<HealthAlertRule, CodecError> {
    HealthAlertRule::from_token(value).ok_or_else(|| invalid(field, value))
}

fn parse_severity(field: &'static str, value: &str) -> Result<AlertSeverity, CodecError> {
    AlertSeverity::from_token(value).ok_or_else(|| invalid(field, value))
}

fn parse_milli(field: &'static str, value: &str) -> Result<i64, CodecError> {
    parse_canonical_i64(value).ok_or_else(|| invalid(field, value))
}

fn parse_hold(field: &'static str, value: &str) -> Result<u32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid(field, value))
}

fn check(field: &'static str, setting: &AlertRuleSetting) -> Result<(), CodecError> {
    setting
        .check()
        .map_err(|_| invalid(field, format!("{setting:?}")))
}

/// Encodes an `alert_rule_set` command's fields after its `payload`.
pub(super) fn encode_set(
    setting: &AlertRuleSetting,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check("alert_rule", setting)?;
    push("rule", setting.rule.token().to_owned())?;
    push(
        "enabled",
        if setting.enabled { "1" } else { "0" }.to_owned(),
    )?;
    push("severity", setting.severity.token().to_owned())?;
    push("trigger", setting.trigger_milli.to_string())?;
    push("clear", setting.clear_milli.to_string())?;
    push("hold_ms", setting.hold_ms.to_string())
}

/// Decodes an `alert_rule_set` command's fields.
pub(super) fn decode_set(fields: &mut dyn FieldSource) -> Result<AlertRuleSetting, CodecError> {
    let setting = AlertRuleSetting {
        rule: parse_rule("rule", &fields.required("rule")?)?,
        enabled: parse_flag("enabled", &fields.required("enabled")?)?,
        severity: parse_severity("severity", &fields.required("severity")?)?,
        trigger_milli: parse_milli("trigger", &fields.required("trigger")?)?,
        clear_milli: parse_milli("clear", &fields.required("clear")?)?,
        hold_ms: parse_hold("hold_ms", &fields.required("hold_ms")?)?,
    };
    check("alert_rule", &setting)?;
    Ok(setting)
}

/// Encodes a show's settings list (`alert_rules`).
pub(super) fn encode_settings(settings: &[AlertRuleSetting]) -> Result<String, CodecError> {
    crate::check_alert_rule_settings(settings)
        .map_err(|_| invalid("alert_rules", format!("{settings:?}")))?;
    Ok(settings
        .iter()
        .map(|setting| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                setting.rule.token(),
                u8::from(setting.enabled),
                setting.severity.token(),
                setting.trigger_milli,
                setting.clear_milli,
                setting.hold_ms
            )
        })
        .collect::<Vec<_>>()
        .join(";"))
}

/// Decodes a show's settings list (`alert_rules`).
pub(super) fn decode_settings(value: &str) -> Result<Vec<AlertRuleSetting>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > HealthAlertRule::ALL.len() {
        return Err(CodecError::TooManyItems("alert_rules"));
    }
    let settings = entries
        .into_iter()
        .map(|entry| {
            let [rule, enabled, severity, trigger, clear, hold] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("alert_rules", entry));
            };
            Ok(AlertRuleSetting {
                rule: parse_rule("alert_rules", rule)?,
                enabled: parse_flag("alert_rules", enabled)?,
                severity: parse_severity("alert_rules", severity)?,
                trigger_milli: parse_milli("alert_rules", trigger)?,
                clear_milli: parse_milli("alert_rules", clear)?,
                hold_ms: parse_hold("alert_rules", hold)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::check_alert_rule_settings(&settings).map_err(|_| invalid("alert_rules", value))?;
    Ok(settings)
}
