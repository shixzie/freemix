//! A show's health alert rule settings in `project.json` (RC-015,
//! schema 85).
//!
//! ```text
//! "alert_rules": [
//!   {"rule": "cpu_saturated", "enabled": true, "trigger": 800, "clear": 600, "hold_ms": 5000},
//!   {"rule": "output_failed", "enabled": false, "trigger": 1000, "clear": 0, "hold_ms": 0}
//! ]
//! ```
//!
//! Only rules the show changes are written, in rule order; thresholds are
//! the reading times 1000. An empty list is omitted. Unknown fields and
//! rules are refused; thresholds out of range or without a hysteresis
//! band, an overlong hold, a repeated rule, and a setting equal to its
//! rule's default are reported by project validation.

use core::fmt::Write as _;

use fm_types::alert_rules::{AlertRuleSetting, HealthAlertRule};

use super::{DecodeError, Object, Value, parse_array, syntax};

/// Writes `,\n    "alert_rules": [...]`; an empty list is omitted.
pub(crate) fn write_alert_rules(output: &mut String, project: &fm_model::Project) {
    let settings = project.alert_rules();
    if settings.is_empty() {
        return;
    }
    output.push_str(",\n    \"alert_rules\": [");
    for (index, setting) in settings.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"rule\": \"{}\", \"enabled\": {}, \"trigger\": {}, \"clear\": {}, \"hold_ms\": {}}}",
            setting.rule.token(),
            setting.enabled,
            setting.trigger_milli,
            setting.clear_milli,
            setting.hold_ms
        )
        .expect("writing to a string cannot fail");
    }
    output.push(']');
}

/// Parses `"alert_rules"` from the project object; an absent list is
/// empty.
pub(crate) fn parse_alert_rules(object: &mut Object) -> Result<Vec<AlertRuleSetting>, DecodeError> {
    let Some(value) = object.take_optional("alert_rules") else {
        return Ok(Vec::new());
    };
    parse_array(value, "alert_rules", parse_setting)
}

fn parse_setting(value: Value) -> Result<AlertRuleSetting, DecodeError> {
    let mut setting = Object::new(value, "alert rule")?;
    let token = setting.string("rule")?;
    let rule = HealthAlertRule::from_token(&token)
        .ok_or_else(|| syntax(format!("alert rule {token:?} is unknown")))?;
    let enabled = setting.boolean("enabled")?;
    let trigger_milli = setting.i64("trigger")?;
    let clear_milli = setting.i64("clear")?;
    let hold_ms = setting.u32("hold_ms")?;
    setting.finish()?;
    Ok(AlertRuleSetting {
        rule,
        enabled,
        trigger_milli,
        clear_milli,
        hold_ms,
    })
}
