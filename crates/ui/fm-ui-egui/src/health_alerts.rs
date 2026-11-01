//! Output ▸ Health alerts (`RC-015`, Protocol 2.109).
//!
//! Lists every daemon alert rule with its state in the latest
//! `health_status`: firing (and for how long it stays acknowledged), silenced
//! (and for how long), or quiet. An operator acknowledges a firing alert for
//! [`ACKNOWLEDGE_MINUTES`] or silences a rule for [`SILENCE_MINUTES`], or
//! ends a silence; each key sends one `health_alert_request`, and the
//! daemon's answer updates the list. The keys need transition authority, as
//! the daemon does.
//!
//! Under each rule, the show's setting for it (Protocol 2.112): whether the
//! rule is evaluated, the reading it fires at and clears at (a percentage,
//! free GiB, pictures a second, or a count), and how long the trigger must
//! hold, with a key back to the rule's defaults. Each change is one
//! undoable `alert_rule_set` project edit, sent when the field is let go;
//! the fields need project-edit authority and follow the project padlock.

use egui::{Button, Checkbox, ComboBox, DragValue, Id, RichText, Ui};
use fm_protocol::{
    AlertReading, AlertRuleSetting, AlertSeverity, DaemonHealth, HealthAlertAction,
    HealthAlertRule, MAX_ALERT_HOLD_MS,
};

use crate::daemon_health_ui::{alert_text, rule_word};
use crate::gating::Hint;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{Tone, card, muted_color, status_chip};
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 620.0;
/// How long the Acknowledge key acknowledges a firing alert.
pub(crate) const ACKNOWLEDGE_MINUTES: u64 = 15;
/// How long the Silence key silences a rule.
pub(crate) const SILENCE_MINUTES: u64 = 60;

/// Whether the window is open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HealthAlertsWindow {
    pub(crate) open: bool,
}

/// Why no alert key works now, in English (see [`l10n::disabled_text`]):
/// the session gates for transition authority, then a missing health report.
pub(crate) fn alert_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if !state.connection_status.controls_enabled() || state.view.is_none() || !state.can_transition
    {
        return Some(crate::gating::unavailable_reason(
            state,
            crate::gating::Capability::Transition,
        ));
    }
    state
        .health
        .is_none()
        .then_some(Msg::HealthAlertsReasonNoReport.en())
}

/// A threshold (the reading times 1000) in the unit the window shows it.
pub(crate) fn display_value(rule: HealthAlertRule, milli: i64) -> f64 {
    rule.reading().shown(milli)
}

/// [`display_value`]'s inverse, rounded to a whole milli unit.
pub(crate) fn milli_value(rule: HealthAlertRule, shown: f64) -> i64 {
    rule.reading().milli(shown)
}

/// The unit a rule's thresholds are shown in.
const fn unit(rule: HealthAlertRule) -> Msg {
    match rule.reading() {
        AlertReading::Share => Msg::HealthRuleUnitPercent,
        AlertReading::Bytes => Msg::HealthRuleUnitGib,
        AlertReading::PerSecond => Msg::HealthRuleUnitPerSecond,
        AlertReading::Count => Msg::HealthRuleUnitCount,
    }
}

/// The intent one setting change sends.
pub(crate) const fn setting_intent(setting: AlertRuleSetting) -> StudioIntent {
    StudioIntent::SetAlertRule(setting)
}

/// The intent one alert key sends.
pub(crate) const fn alert_intent(rule: HealthAlertRule, action: HealthAlertAction) -> StudioIntent {
    StudioIntent::HealthAlert { rule, action }
}

impl StudioShell {
    /// Draws the Health alerts window while it is open.
    pub(crate) fn draw_health_alerts_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.health_alerts.open {
            return;
        }
        let l = self.locale;
        let close = crate::menu::help_modal(
            context,
            "studio-health-alerts",
            l.text(Msg::HealthAlertsTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, intents),
        );
        if close {
            self.health_alerts.open = false;
        }
    }
}

fn draw_contents(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::HealthAlertsIntro))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
    );
    ui.add_space(6.0);
    let blocked = alert_block_reason(state);
    let edit = crate::cues::edit_block(state);
    let health = state.health.as_ref().map(|status| &status.health);
    let settings = state
        .view
        .as_ref()
        .map_or(&[] as &[AlertRuleSetting], |view| {
            view.alert_rules.as_slice()
        });
    egui::ScrollArea::vertical()
        .id_salt("studio-health-alerts-scroll")
        .max_height(520.0)
        .show(ui, |ui| {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for rule in HealthAlertRule::ALL {
                    let setting = fm_protocol::effective_alert_rule(settings, rule);
                    // The daemon does not evaluate a rule the show switched
                    // off, so it has nothing to acknowledge or silence.
                    let keys = blocked
                        .or_else(|| (!setting.enabled).then_some(Msg::HealthRuleReasonOff.en()));
                    draw_rule(ui, l, health, rule, keys, intents);
                    draw_setting(ui, l, setting, edit, intents);
                    ui.separator();
                }
            });
        });
}

/// One threshold or hold field of a rule's setting.
#[derive(Clone, Copy, Debug, Hash)]
enum Field {
    Trigger,
    Clear,
    Hold,
}

/// The localized name of one alert severity.
fn severity_label(severity: AlertSeverity) -> Msg {
    match severity {
        AlertSeverity::Info => Msg::AlertSeverityInfo,
        AlertSeverity::Warning => Msg::AlertSeverityWarning,
        AlertSeverity::Critical => Msg::AlertSeverityCritical,
    }
}

/// The rule's setting: whether it is evaluated, its thresholds and hold,
/// and the key back to its defaults.
fn draw_setting(
    ui: &mut Ui,
    l: Locale,
    setting: AlertRuleSetting,
    edit: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let rule = setting.rule;
    let reason = edit.unwrap_or_default();
    let mut send = |updated: AlertRuleSetting, ui: &mut Ui| {
        if updated == setting {
            return;
        }
        let refusal = updated.check().err();
        ui.data_mut(|data| data.insert_temp(Id::new(("studio-alert-rule-refusal", rule)), refusal));
        if refusal.is_none() {
            intents.push(setting_intent(updated));
        }
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let mut enabled = setting.enabled;
        if ui
            .add_enabled(
                edit.is_none(),
                Checkbox::new(&mut enabled, l.text(Msg::HealthRuleEvaluate)),
            )
            .hint(l.text(Msg::HealthRuleEvaluateHover), reason)
            .changed()
        {
            send(AlertRuleSetting { enabled, ..setting }, ui);
        }
        ui.label(RichText::new(l.text(Msg::HealthRuleSeverity)).color(muted_color()))
            .on_hover_text(l.text(Msg::HealthRuleSeverityHover));
        let mut severity = setting.severity;
        let picker = ComboBox::from_id_salt(("studio-alert-rule-severity", rule))
            .selected_text(l.text(severity_label(severity)))
            .show_ui(ui, |ui| {
                for choice in AlertSeverity::ALL {
                    ui.selectable_value(&mut severity, choice, l.text(severity_label(choice)))
                        .on_hover_text(l.text(Msg::HealthRuleSeverityHover));
                }
            });
        picker
            .response
            .on_hover_text(l.text(Msg::HealthRuleSeverityHover));
        if severity != setting.severity {
            send(
                AlertRuleSetting {
                    severity,
                    ..setting
                },
                ui,
            );
        }
        for (field, label, hover) in [
            (
                Field::Trigger,
                Msg::HealthRuleFiresAt,
                Msg::HealthRuleFiresAtHover,
            ),
            (
                Field::Clear,
                Msg::HealthRuleClearsAt,
                Msg::HealthRuleClearsAtHover,
            ),
            (Field::Hold, Msg::HealthRuleHold, Msg::HealthRuleHoldHover),
        ] {
            ui.label(RichText::new(l.text(label)).color(muted_color()))
                .on_hover_text(l.text(hover));
            if let Some(updated) = draw_field(ui, l, setting, field, edit, hover) {
                send(updated, ui);
            }
        }
        if ui
            .add_enabled(
                edit.is_none() && !setting.is_default(),
                Button::new(l.text(Msg::HealthRuleUseDefaults)),
            )
            .hint(
                l.text(Msg::HealthRuleUseDefaultsHover),
                edit.unwrap_or(Msg::HealthRuleReasonDefault.en()),
            )
            .clicked()
        {
            send(AlertRuleSetting::default_for(rule), ui);
        }
    });
    let refusal = ui.data(|data| {
        data.get_temp::<Option<fm_protocol::AlertRuleError>>(Id::new((
            "studio-alert-rule-refusal",
            rule,
        )))
        .flatten()
    });
    if let Some(refusal) = refusal {
        let text = match refusal {
            fm_protocol::AlertRuleError::Hysteresis if rule.fires_above() => {
                Msg::HealthRuleRefusedBandBelow
            }
            fm_protocol::AlertRuleError::Hysteresis => Msg::HealthRuleRefusedBandAbove,
            _ => Msg::HealthRuleRefusedRange,
        };
        ui.label(RichText::new(l.text(text)).color(crate::theme::ERROR));
    }
}

/// One threshold or hold field; the changed setting once the field is let
/// go.
fn draw_field(
    ui: &mut Ui,
    l: Locale,
    setting: AlertRuleSetting,
    field: Field,
    edit: Option<&'static str>,
    hover: Msg,
) -> Option<AlertRuleSetting> {
    let rule = setting.rule;
    let (low, high) = rule.threshold_range();
    let current = match field {
        Field::Trigger => display_value(rule, setting.trigger_milli),
        Field::Clear => display_value(rule, setting.clear_milli),
        Field::Hold => f64::from(setting.hold_ms) / 1000.0,
    };
    let id = Id::new(("studio-alert-rule-field", rule, field));
    let mut shown = ui.data(|data| data.get_temp::<f64>(id)).unwrap_or(current);
    let drag = match field {
        Field::Hold => DragValue::new(&mut shown)
            .range(0.0..=f64::from(MAX_ALERT_HOLD_MS) / 1000.0)
            .speed(0.5)
            .max_decimals(1)
            .suffix(format!(" {}", l.text(Msg::HealthRuleUnitSeconds))),
        Field::Trigger | Field::Clear => {
            let decimals = match rule.reading() {
                AlertReading::Count => 0,
                AlertReading::Share | AlertReading::PerSecond => 1,
                AlertReading::Bytes => 2,
            };
            DragValue::new(&mut shown)
                .range(display_value(rule, low)..=display_value(rule, high))
                .speed(match rule.reading() {
                    AlertReading::Bytes => 0.1,
                    _ => 0.5,
                })
                .max_decimals(decimals)
                .suffix(format!(" {}", l.text(unit(rule))))
        }
    };
    let response = ui
        .add_enabled(edit.is_none(), drag)
        .hint(l.text(hover), edit.unwrap_or_default());
    if response.dragged() || response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, shown));
    } else {
        ui.data_mut(|data| data.remove::<f64>(id));
    }
    if !(response.drag_stopped() || response.lost_focus()) {
        return None;
    }
    let updated = match field {
        Field::Trigger => AlertRuleSetting {
            trigger_milli: milli_value(rule, shown).clamp(low, high),
            ..setting
        },
        Field::Clear => AlertRuleSetting {
            clear_milli: milli_value(rule, shown).clamp(low, high),
            ..setting
        },
        Field::Hold => AlertRuleSetting {
            hold_ms: hold_ms(shown),
            ..setting
        },
    };
    (updated != setting).then_some(updated)
}

/// A hold typed in seconds, in milliseconds, within the protocol's bound.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
fn hold_ms(seconds: f64) -> u32 {
    (seconds * 1000.0)
        .round()
        .clamp(0.0, f64::from(MAX_ALERT_HOLD_MS)) as u32
}

/// One rule's row: its name, its state, and its three keys.
fn draw_rule(
    ui: &mut Ui,
    l: Locale,
    health: Option<&DaemonHealth>,
    rule: HealthAlertRule,
    blocked: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let alert = health.and_then(|health| health.alert(rule));
    let silence = health.and_then(|health| health.silence(rule));
    let time = |until_ms: u64| {
        let uptime = health.map_or(0, |health| health.uptime_ms);
        l.seconds_tenths(until_ms.saturating_sub(uptime))
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(l.text(rule_word(rule))).strong());
        let (mut text, tone) = match (alert, health) {
            (Some(alert), Some(health)) => (
                alert_text(l, health, alert),
                if alert.acknowledged_until_ms.is_some() {
                    Tone::Pending
                } else {
                    Tone::Fault
                },
            ),
            _ => (l.text(Msg::HealthAlertsQuiet).to_owned(), Tone::Ready),
        };
        if let Some(until) = alert.and_then(|alert| alert.acknowledged_until_ms) {
            text.push_str(", ");
            text.push_str(&l.format(
                Msg::HealthAlertsAcknowledgedFor,
                &[("time", Arg::Str(&time(until)))],
            ));
        }
        status_chip(ui, &text, tone, 260.0);
        if let Some(silence) = silence {
            ui.label(
                RichText::new(l.format(
                    Msg::HealthAlertsSilencedFor,
                    &[("time", Arg::Str(&time(silence.until_ms)))],
                ))
                .color(muted_color()),
            );
        }
    });
    ui.horizontal(|ui| {
        let acknowledge = blocked.or_else(|| {
            alert
                .is_none()
                .then_some(Msg::HealthAlertsReasonNotFiring.en())
        });
        if ui
            .add_enabled(
                acknowledge.is_none(),
                Button::new(l.text(Msg::HealthAlertsAcknowledge)),
            )
            .hint(
                l.text(Msg::HealthAlertsAcknowledgeHover),
                acknowledge.unwrap_or_default(),
            )
            .clicked()
        {
            intents.push(alert_intent(
                rule,
                HealthAlertAction::Acknowledge {
                    duration_ms: ACKNOWLEDGE_MINUTES * 60_000,
                },
            ));
        }
        if ui
            .add_enabled(
                blocked.is_none(),
                Button::new(l.text(Msg::HealthAlertsSilence)),
            )
            .hint(
                l.text(Msg::HealthAlertsSilenceHover),
                blocked.unwrap_or_default(),
            )
            .clicked()
        {
            intents.push(alert_intent(
                rule,
                HealthAlertAction::Silence {
                    duration_ms: SILENCE_MINUTES * 60_000,
                },
            ));
        }
        let unsilence = blocked.or_else(|| {
            silence
                .is_none()
                .then_some(Msg::HealthAlertsReasonNotSilenced.en())
        });
        if ui
            .add_enabled(
                unsilence.is_none(),
                Button::new(l.text(Msg::HealthAlertsUnsilence)),
            )
            .hint(
                l.text(Msg::HealthAlertsUnsilenceHover),
                unsilence.unwrap_or_default(),
            )
            .clicked()
        {
            intents.push(alert_intent(rule, HealthAlertAction::Unsilence));
        }
    });
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use fm_protocol::{HealthAlert, HealthSeverity, HealthSilence, HealthStatusMessage};

    use super::*;
    use crate::StudioConnectionStatus;

    fn status(health: DaemonHealth) -> HealthStatusMessage {
        HealthStatusMessage {
            server: fm_protocol::ServerIdentity {
                engine_id: "e".into(),
                project_id: "1".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            sequence: 1,
            request_id: None,
            health,
        }
    }

    /// RC-015: the keys need a ready, synchronized session with transition
    /// authority and a health report; each sends one request.
    #[test]
    fn alert_keys_need_transition_authority_and_a_report() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert!(alert_block_reason(&state).is_some());
        state = crate::hover_audit::ready(true, crate::hover_audit::Variant::default());
        state.health = None;
        state.can_transition = false;
        assert_eq!(
            alert_block_reason(&state),
            Some(Msg::ReasonNoTransition.en())
        );
        state.can_transition = true;
        assert_eq!(
            alert_block_reason(&state),
            Some(Msg::HealthAlertsReasonNoReport.en())
        );
        state.health = Some(Box::new(status(DaemonHealth {
            uptime_ms: 10_000,
            alerts: vec![HealthAlert {
                rule: HealthAlertRule::DiskLow,
                severity: HealthSeverity::Critical,
                since_ms: 5_000,
                value_milli: None,
                acknowledged_until_ms: None,
            }],
            silences: vec![HealthSilence {
                rule: HealthAlertRule::CpuSaturated,
                until_ms: 70_000,
            }],
            ..DaemonHealth::default()
        })));
        assert_eq!(alert_block_reason(&state), None);
        assert_eq!(
            alert_intent(HealthAlertRule::DiskLow, HealthAlertAction::Unsilence),
            StudioIntent::HealthAlert {
                rule: HealthAlertRule::DiskLow,
                action: HealthAlertAction::Unsilence,
            }
        );
    }

    /// Protocol 2.112: thresholds show as a percentage, free GiB, pictures
    /// a second, or a count, and convert back to the exact milli units.
    #[test]
    fn thresholds_show_in_their_rules_units() {
        let cases = [
            (HealthAlertRule::CpuSaturated, 900, 90.0),
            (HealthAlertRule::DiskLow, 1_073_741_824_000, 1.0),
            (HealthAlertRule::FramesDropped, 1_500, 1.5),
            (HealthAlertRule::OutputFailed, 2_000, 2.0),
        ];
        for (rule, milli, shown) in cases {
            assert!(
                (display_value(rule, milli) - shown).abs() < 1e-9,
                "{rule:?}"
            );
            assert_eq!(milli_value(rule, shown), milli, "{rule:?}");
        }
        let setting = AlertRuleSetting {
            enabled: false,
            ..AlertRuleSetting::default_for(HealthAlertRule::GpuSaturated)
        };
        assert_eq!(setting_intent(setting), StudioIntent::SetAlertRule(setting));
    }
}
