//! Output ▸ Web access (RC-007, RC-014, Protocol 2.70).
//!
//! Phones and tablets on the local network open the daemon's web tally and
//! operator pages. Each one pairs once: the operator starts pairing here
//! for a role, the window shows a QR code of the page link (with a one-use
//! link code and the certificate fingerprint in the fragment) and a
//! six-digit code to type, and the phone receives its own token from the
//! gateway. The paired devices are listed with a Revoke key each.
//!
//! The window never holds a device token: the daemon answers
//! `web_access_request` with the gateway status only. While the window is
//! open it asks for that status about once a second, so a device that
//! pairs, or a code that expires, shows up without a click.

use egui::{Button, Color32, ComboBox, RichText, Sense, Ui, Vec2};
use fm_protocol::{
    WebAccessAction, WebAccessMessage, WebAccessRefusal, WebDeviceEntry, WebDeviceRole,
    WebGatewayStatus,
};
use qrcodegen::{QrCode, QrCodeEcc};

use crate::gating::Hint;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, KeyIcon, MIN_TARGET, Tone, add_icon_button, card, muted_color, section_label,
    status_chip,
};
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 720.0;
/// How often the open window asks for the gateway status, in seconds.
pub(crate) const POLL_SECONDS: f64 = 1.0;
/// The QR code's side, in points.
const QR_SIDE: f32 = 208.0;
/// Light modules around the code, as the QR specification asks.
const QR_QUIET_ZONE: i32 = 4;

/// The window's open state, the role the next pairing grants, and when it
/// last asked for the status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WebAccessWindow {
    pub(crate) open: bool,
    pub(crate) role: WebDeviceRole,
    /// The egui time of the latest status poll, in milliseconds.
    last_poll_ms: Option<u64>,
}

impl Default for WebAccessWindow {
    fn default() -> Self {
        Self {
            open: false,
            role: WebDeviceRole::Operator,
            last_poll_ms: None,
        }
    }
}

impl WebAccessWindow {
    /// Whether a status poll is due at `now_seconds` (egui time); records it
    /// when it is.
    pub(crate) fn poll_due(&mut self, now_seconds: f64) -> bool {
        // Whole milliseconds keep the window state comparable.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let now_ms = (now_seconds.max(0.0) * 1_000.0) as u64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let period_ms = (POLL_SECONDS * 1_000.0) as u64;
        let due = self
            .last_poll_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= period_ms);
        if due {
            self.last_poll_ms = Some(now_ms);
        }
        due
    }
}

/// The catalog message wording a refusal for the operator.
#[must_use]
pub(crate) const fn refusal_message(refusal: WebAccessRefusal) -> Msg {
    match refusal {
        WebAccessRefusal::PermissionDenied => Msg::WebAccessRefusedPermission,
        WebAccessRefusal::WebSession => Msg::WebAccessRefusedWebSession,
        WebAccessRefusal::Unavailable => Msg::WebAccessRefusedUnavailable,
        WebAccessRefusal::UnknownDevice => Msg::WebAccessRefusedUnknownDevice,
        WebAccessRefusal::TooManyDevices => Msg::WebAccessRefusedTooMany,
        WebAccessRefusal::StorageFailed => Msg::WebAccessRefusedStorage,
    }
}

/// The role's label in the role picker and the device table.
#[must_use]
pub(crate) const fn role_message(role: WebDeviceRole) -> Msg {
    match role {
        WebDeviceRole::Viewer => Msg::WebAccessRoleViewer,
        WebDeviceRole::Audio => Msg::WebAccessRoleAudio,
        WebDeviceRole::Operator => Msg::WebAccessRoleOperator,
    }
}

/// The latest web access answer, if the daemon gave one.
fn answer(state: &StudioUiState) -> Option<&WebAccessMessage> {
    state.view.as_ref()?.web_access.as_ref()
}

/// Why pairing and revoking are unavailable, if they are: the connection
/// first, then the daemon's answer (none yet, refused, or no gateway).
#[must_use]
pub fn web_access_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if !state.connection_status.controls_enabled() || state.view.is_none() {
        // Not connected or not synchronized: the session gate names why
        // before any permission.
        return Some(crate::gating::unavailable_reason(
            state,
            crate::gating::Capability::EditProject,
        ));
    }
    let Some(answer) = answer(state) else {
        return Some(Msg::WebAccessReasonNoAnswer.en());
    };
    if answer.gateway.is_some() {
        return None;
    }
    Some(match answer.refusal {
        Some(refusal @ (WebAccessRefusal::PermissionDenied | WebAccessRefusal::WebSession)) => {
            refusal_message(refusal).en()
        }
        _ => Msg::WebAccessReasonNoGateway.en(),
    })
}

/// The intent that performs `action`, when web access can be managed.
#[must_use]
pub fn web_access_intent(state: &StudioUiState, action: WebAccessAction) -> Option<StudioIntent> {
    if matches!(action, WebAccessAction::Status) {
        return state
            .connection_status
            .controls_enabled()
            .then_some(StudioIntent::WebAccess(action));
    }
    web_access_block_reason(state)
        .is_none()
        .then_some(StudioIntent::WebAccess(action))
}

/// `042 137`: the code in two groups of three for reading aloud.
#[must_use]
pub(crate) fn grouped_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

/// `Oct 14, 2026, 09:41:07 UTC` for Unix milliseconds.
fn when(l: Locale, unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000;
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    l.format(
        Msg::WebAccessWhen,
        &[
            ("date", Arg::Str(&l.date(days))),
            ("time", Arg::Str(&l.time_of_day(seconds))),
        ],
    )
}

impl StudioShell {
    /// Draws the Web access window when open, polling the status while it
    /// is.
    pub(crate) fn draw_web_access_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.web_access.open {
            return;
        }
        let now = context.input(|input| input.time);
        if self.web_access.poll_due(now)
            && let Some(intent) = web_access_intent(state, WebAccessAction::Status)
        {
            intents.push(intent);
        }
        context.request_repaint_after(std::time::Duration::from_secs_f64(POLL_SECONDS));
        let l = self.locale;
        let window = &mut self.web_access;
        let close = crate::menu::help_modal(
            context,
            "studio-web-access",
            l.text(Msg::WebAccessTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, window, intents),
        );
        if close {
            self.web_access.open = false;
            self.web_access.last_poll_ms = None;
        }
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut WebAccessWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::WebAccessIntro))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
    );
    ui.add_space(6.0);
    let answer = answer(state);
    if let Some(refusal) = answer.and_then(|answer| answer.refusal) {
        ui.label(RichText::new(l.text(refusal_message(refusal))).color(AMBER));
        ui.add_space(4.0);
    }
    let gateway = answer.and_then(|answer| answer.gateway.as_ref());
    let blocked = web_access_block_reason(state);
    draw_gateway(ui, l, gateway);
    ui.add_space(6.0);
    draw_pairing(ui, l, state, window, gateway, blocked, intents);
    ui.add_space(6.0);
    draw_devices(ui, l, state, gateway, blocked, intents);
}

fn draw_gateway(ui: &mut Ui, l: Locale, gateway: Option<&WebGatewayStatus>) {
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.label(l.text(Msg::WebAccessAddress));
            ui.add_space(8.0);
            let url = gateway.map(|gateway| gateway.url.as_str());
            ui.label(RichText::new(url.unwrap_or(l.text(Msg::WebAccessAddressNone))).monospace());
            if ui
                .add_enabled(url.is_some(), Button::new(l.text(Msg::WebAccessCopy)))
                .hint(
                    l.text(Msg::WebAccessCopyHover),
                    Msg::WebAccessReasonNoAnswer.en(),
                )
                .clicked()
                && let Some(url) = url
            {
                ui.ctx().copy_text(url.to_owned());
            }
        });
        let Some(gateway) = gateway else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(l.text(Msg::WebAccessTls));
            let (text, tone) = if gateway.tls {
                (l.text(Msg::WebAccessTlsOn), Tone::Ready)
            } else {
                (l.text(Msg::WebAccessTlsOff), Tone::Pending)
            };
            status_chip(ui, text, tone, if gateway.tls { 96.0 } else { 300.0 })
                .on_hover_text(l.text(Msg::WebAccessTlsHover));
        });
        if let Some(fingerprint) = &gateway.fingerprint {
            ui.label(section_label(l.text(Msg::WebAccessFingerprint)));
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(fingerprint)
                            .monospace()
                            .size(crate::theme::legible(11.0)),
                    )
                    .wrap(),
                )
                .on_hover_text(l.text(Msg::WebAccessFingerprintHover));
            });
            if ui
                .button(l.text(Msg::WebAccessCopyFingerprint))
                .on_hover_text(l.text(Msg::WebAccessCopyFingerprintHover))
                .clicked()
            {
                ui.ctx().copy_text(fingerprint.clone());
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pairing(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    window: &mut WebAccessWindow,
    gateway: Option<&WebGatewayStatus>,
    blocked: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let editable = blocked.is_none();
    let reason = blocked.unwrap_or("");
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.label(l.text(Msg::WebAccessRole));
            ui.add_enabled_ui(editable, |ui| {
                ComboBox::from_id_salt("studio-web-access-role")
                    .selected_text(l.text(role_message(window.role)))
                    .show_ui(ui, |ui| {
                        for role in [
                            WebDeviceRole::Viewer,
                            WebDeviceRole::Audio,
                            WebDeviceRole::Operator,
                        ] {
                            let name = l.text(role_message(role));
                            if ui
                                .selectable_label(window.role == role, name)
                                .on_hover_text(l.format(
                                    Msg::WebAccessRoleOptionHover,
                                    &[("role", Arg::Str(name))],
                                ))
                                .clicked()
                            {
                                window.role = role;
                            }
                        }
                    })
                    .response
                    .hint(l.text(Msg::WebAccessRoleHover), reason);
            });
            if ui
                .add_enabled(editable, Button::new(l.text(Msg::WebAccessStart)))
                .hint(l.text(Msg::WebAccessStartHover), reason)
                .clicked()
                && let Some(intent) =
                    web_access_intent(state, WebAccessAction::Pair { role: window.role })
            {
                intents.push(intent);
            }
        });
        let pairing =
            gateway.and_then(|gateway| Some((gateway.pairing.as_ref()?, gateway.pairing_link()?)));
        let Some((pairing, link)) = pairing else {
            ui.label(RichText::new(l.text(Msg::WebAccessNoPairing)).color(muted_color()));
            return;
        };
        ui.label(l.text(Msg::WebAccessScan));
        ui.horizontal(|ui| {
            draw_qr(ui, &link).on_hover_text(l.text(Msg::WebAccessQrHover));
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(grouped_code(&pairing.code))
                        .monospace()
                        .strong()
                        .size(36.0),
                )
                .on_hover_text(l.text(Msg::WebAccessCodeHover));
                ui.label(l.format(
                    Msg::WebAccessPairingRole,
                    &[("role", Arg::Str(l.text(role_message(pairing.role))))],
                ));
                ui.label(
                    RichText::new(l.format(
                        Msg::WebAccessExpires,
                        &[(
                            "count",
                            Arg::Count(u64::from(pairing.expires_in_ms.div_ceil(1_000))),
                        )],
                    ))
                    .color(muted_color()),
                );
                if ui
                    .add_enabled(editable, Button::new(l.text(Msg::WebAccessCancel)))
                    .hint(
                        l.text(Msg::WebAccessCancelHover),
                        blocked.unwrap_or(Msg::WebAccessReasonNoPairing.en()),
                    )
                    .clicked()
                    && let Some(intent) = web_access_intent(state, WebAccessAction::CancelPairing)
                {
                    intents.push(intent);
                }
            });
        });
    });
}

/// Paints `text` as a QR code, dark modules on white with a quiet zone,
/// whatever the console theme; an unencodable text paints a blank square.
fn draw_qr(ui: &mut Ui, text: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(QR_SIDE), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::WHITE);
    if let Ok(code) = QrCode::encode_text(text, QrCodeEcc::Medium) {
        let size = code.size();
        #[allow(clippy::cast_precision_loss)] // at most 177 + 8 modules
        let module = QR_SIDE / (size + 2 * QR_QUIET_ZONE) as f32;
        for y in 0..size {
            for x in 0..size {
                if code.get_module(x, y) {
                    #[allow(clippy::cast_precision_loss)]
                    let min = rect.min
                        + Vec2::new(
                            (x + QR_QUIET_ZONE) as f32 * module,
                            (y + QR_QUIET_ZONE) as f32 * module,
                        );
                    painter.rect_filled(
                        egui::Rect::from_min_size(min, Vec2::splat(module)),
                        0.0,
                        Color32::BLACK,
                    );
                }
            }
        }
    }
    response
}

fn draw_devices(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    gateway: Option<&WebGatewayStatus>,
    blocked: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(section_label(l.text(Msg::WebAccessDevices)));
        let devices = gateway.map_or(&[][..], |gateway| gateway.devices.as_slice());
        if devices.is_empty() {
            ui.label(RichText::new(l.text(Msg::WebAccessNoDevices)).color(muted_color()));
            return;
        }
        egui::Grid::new("studio-web-access-devices")
            .num_columns(5)
            .spacing([12.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for heading in [
                    Msg::WebAccessColName,
                    Msg::WebAccessColRole,
                    Msg::WebAccessColPaired,
                    Msg::WebAccessColSeen,
                ] {
                    ui.label(RichText::new(l.text(heading)).color(muted_color()));
                }
                ui.label("");
                ui.end_row();
                for device in devices {
                    draw_device(ui, l, state, device, blocked, intents);
                    ui.end_row();
                }
            });
    });
}

fn draw_device(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    device: &WebDeviceEntry,
    blocked: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    ui.label(&device.name);
    ui.label(l.text(role_message(device.role)));
    ui.label(when(l, device.paired_at_ms));
    ui.label(
        device
            .last_seen_ms
            .map_or_else(|| l.text(Msg::WebAccessNever).to_owned(), |at| when(l, at)),
    );
    let name = [("name", Arg::Str(&device.name))];
    if add_icon_button(
        ui,
        blocked.is_none(),
        Button::new("").min_size(Vec2::splat(MIN_TARGET)),
        KeyIcon::Remove,
        &l.format(Msg::WebAccessRevokeName, &name),
    )
    .hint(
        &l.format(Msg::WebAccessRevokeHover, &name),
        blocked.unwrap_or(""),
    )
    .clicked()
        && let Some(intent) = web_access_intent(
            state,
            WebAccessAction::Revoke {
                device: device.id.clone(),
            },
        )
    {
        intents.push(intent);
    }
}

#[cfg(test)]
mod tests {
    use super::{WebAccessWindow, grouped_code, refusal_message};
    use fm_protocol::WebAccessRefusal;
    use fm_ui_model::i18n::Msg;

    #[test]
    fn codes_read_in_two_groups() {
        assert_eq!(grouped_code("042137"), "042 137");
        assert_eq!(grouped_code("12"), "12");
    }

    #[test]
    fn the_open_window_polls_about_once_a_second() {
        let mut window = WebAccessWindow::default();
        assert!(window.poll_due(10.0));
        assert!(!window.poll_due(10.5));
        assert!(window.poll_due(11.0));
        assert!(!window.poll_due(11.2));
    }

    #[test]
    fn refusals_are_worded() {
        assert_eq!(
            refusal_message(WebAccessRefusal::PermissionDenied).en(),
            "Managing web devices needs the admin role (start Studio with --role admin)"
        );
        assert_eq!(
            refusal_message(WebAccessRefusal::Unavailable),
            Msg::WebAccessRefusedUnavailable
        );
    }
}
