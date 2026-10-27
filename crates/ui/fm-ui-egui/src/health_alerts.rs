//! Output ▸ Health alerts (`RC-015`, Protocol 2.109).
//!
//! Lists every daemon alert rule with its state in the latest
//! `health_status`: firing (and for how long it stays acknowledged), silenced
//! (and for how long), or quiet. An operator acknowledges a firing alert for
//! [`ACKNOWLEDGE_MINUTES`] or silences a rule for [`SILENCE_MINUTES`], or
//! ends a silence; each key sends one `health_alert_request`, and the
//! daemon's answer updates the list. The keys need transition authority, as
//! the daemon does.

use egui::{Button, RichText, Ui};
use fm_protocol::{DaemonHealth, HealthAlertAction, HealthAlertRule};

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
    let health = state.health.as_ref().map(|status| &status.health);
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        for rule in HealthAlertRule::ALL {
            draw_rule(ui, l, health, rule, blocked, intents);
        }
    });
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
}
