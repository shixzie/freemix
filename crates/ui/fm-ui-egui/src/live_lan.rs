//! Output ▸ `LiveLAN` viewers… (OR-017, Protocol 2.57).
//!
//! `LiveLAN` lets people on the local network watch Program, or one output
//! with the audio that output hears, in a web browser, optionally behind a
//! PIN. Its settings are project configuration: every control here sends
//! `live_lan_set` at once, a padlocked, undoable project edit, built from
//! the replicated settings with only the edited field changed. The PIN is
//! typed into a masked field and sent only by Set PIN; it is never shown
//! again, and the window only knows whether one is stored. The status line
//! shows the server's report: the state, the address viewers open, how
//! many are watching, and the bandwidth they take.

use egui::{Button, ComboBox, DragValue, RichText, TextEdit, Ui};
use fm_protocol::{
    LiveLanEntry, LiveLanSource, LiveLanState, LiveLanStatus, MAX_LIVE_LAN_PIN_DIGITS,
    MAX_LIVE_LAN_VIEWERS, MIN_LIVE_LAN_VIEWERS, SecretEdit,
};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{Tone, card, muted_color, section_label, status_chip};
use crate::{StudioIntent, StudioShell, StudioUiState, stinger_edit_lock_reason};

const DIALOG_WIDTH: f32 = 520.0;

/// The window's open state and the PIN being typed.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct LiveLanWindow {
    pub(crate) open: bool,
    /// The PIN typed and not sent yet; never logged or rendered unmasked.
    pub(crate) pin: String,
}

impl core::fmt::Debug for LiveLanWindow {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LiveLanWindow")
            .field("open", &self.open)
            .field("pin", &"***")
            .finish()
    }
}

/// Why `LiveLAN` edits are unavailable, if they are: a session gate first
/// (connection, synchronized state, the project-edit permission), then the
/// project padlock.
#[must_use]
pub fn live_lan_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if state.connection_status.controls_enabled() && state.view.is_some() && state.can_edit_project
    {
        stinger_edit_lock_reason(state)
    } else {
        Some(unavailable_reason(state, Capability::EditProject))
    }
}

/// The intent that stores `config` with the PIN edit `pin`, when edits
/// are allowed and an output source names an output of the replicated view.
#[must_use]
pub fn live_lan_set_intent(
    state: &StudioUiState,
    config: LiveLanEntry,
    pin: SecretEdit,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    if live_lan_block_reason(state).is_some() {
        return None;
    }
    if let LiveLanSource::Output(output) = config.source
        && !view.outputs.iter().any(|status| status.output == output)
    {
        return None;
    }
    Some(StudioIntent::SetLiveLan { config, pin })
}

impl StudioShell {
    /// Draws the `LiveLAN` viewers window when open.
    pub(crate) fn draw_live_lan_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.live_lan.open {
            return;
        }
        let l = self.locale;
        let window = &mut self.live_lan;
        let close = crate::menu::help_modal(
            context,
            "studio-live-lan",
            l.text(Msg::LiveLanTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, window, intents),
        );
        if close {
            self.live_lan.open = false;
            self.live_lan.pin.clear();
        }
    }
}

/// The status chip, its tone, and an optional detail line.
fn status_chip_of(l: Locale, status: &LiveLanStatus) -> (&'static str, Tone, Option<&'static str>) {
    match status.state {
        LiveLanState::Off => (l.text(Msg::StreamOff), Tone::Idle, None),
        LiveLanState::Unavailable => (
            l.text(Msg::StreamUnavailable),
            Tone::Pending,
            Some(l.text(Msg::LiveLanUnavailableDetail)),
        ),
        LiveLanState::Starting => (l.text(Msg::StreamStarting), Tone::Pending, None),
        LiveLanState::Live => (l.text(Msg::Live), Tone::OnAir, None),
        LiveLanState::Failed => (
            l.text(Msg::ConnFailed),
            Tone::Fault,
            Some(l.text(Msg::LiveLanFailedDetail)),
        ),
    }
}

#[allow(clippy::too_many_lines)] // one immediate-mode window
fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut LiveLanWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::LiveLanIntro))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
    );
    ui.add_space(6.0);
    let view = state.view.as_ref();
    let config = view.map_or_else(LiveLanEntry::default, |view| view.live_lan);
    let status = view.map_or_else(LiveLanStatus::default, |view| view.live_lan_status.clone());
    let blocked = live_lan_block_reason(state);
    let reason = blocked.unwrap_or_else(|| unavailable_reason(state, Capability::EditProject));
    let editable = blocked.is_none();
    let send = |intents: &mut Vec<StudioIntent>, config: LiveLanEntry, pin: SecretEdit| {
        if let Some(intent) = live_lan_set_intent(state, config, pin) {
            intents.push(intent);
        }
    };

    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        let (chip, tone, detail) = status_chip_of(l, &status);
        ui.horizontal(|ui| {
            ui.label(section_label(l.text(Msg::LiveLanTitle)));
            // Unavailable or failed, the hover says why, not only what.
            status_chip(ui, chip, tone, 96.0)
                .on_hover_text(detail.unwrap_or_else(|| l.text(Msg::LiveLanStatusHover)));
            if status.state == LiveLanState::Live {
                ui.label(l.format(
                    Msg::LiveLanViewers,
                    &[
                        ("count", Arg::Count(u64::from(status.viewers))),
                        ("max", Arg::Count(u64::from(config.max_viewers))),
                    ],
                ));
                ui.label(
                    RichText::new(l.format(
                        Msg::LiveLanEgress,
                        &[("kbps", Arg::Count(u64::from(status.egress_kbps)))],
                    ))
                    .color(muted_color()),
                );
            }
        });
        if let Some(detail) = detail {
            ui.label(
                RichText::new(detail)
                    .size(crate::theme::legible(11.0))
                    .color(muted_color()),
            );
        }
        ui.horizontal(|ui| {
            ui.label(l.text(Msg::LiveLanAddress));
            let address = if status.url.is_empty() {
                l.text(Msg::LiveLanAddressNone)
            } else {
                status.url.as_str()
            };
            ui.label(RichText::new(address).monospace());
            if ui
                .add_enabled(
                    !status.url.is_empty(),
                    Button::new(l.text(Msg::LiveLanCopy)),
                )
                .hint(
                    l.text(Msg::LiveLanCopyHover),
                    Msg::LiveLanReasonNoAddress.en(),
                )
                .clicked()
            {
                ui.ctx().copy_text(status.url.clone());
            }
        });
    });
    ui.add_space(6.0);

    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        let mut enabled = config.enabled;
        if ui
            .add_enabled(
                editable,
                egui::Checkbox::new(&mut enabled, l.text(Msg::LiveLanEnable)),
            )
            .hint(l.text(Msg::LiveLanEnableHover), reason)
            .changed()
        {
            send(
                intents,
                LiveLanEntry { enabled, ..config },
                SecretEdit::Keep,
            );
        }

        ui.horizontal(|ui| {
            ui.label(l.text(Msg::LiveLanSource));
            let outputs = view.map_or(&[][..], |view| view.outputs.as_slice());
            let source_name = |source: LiveLanSource| match source {
                LiveLanSource::Program => l.text(Msg::PanelProgram).to_owned(),
                LiveLanSource::Output(output) => outputs
                    .iter()
                    .find(|status| status.output == output)
                    .map_or_else(|| output.to_string(), |status| status.name.clone()),
            };
            ui.add_enabled_ui(editable, |ui| {
                ComboBox::from_id_salt("studio-live-lan-source")
                    .selected_text(source_name(config.source))
                    .show_ui(ui, |ui| {
                        let choices = std::iter::once(LiveLanSource::Program).chain(
                            outputs
                                .iter()
                                .map(|status| LiveLanSource::Output(status.output)),
                        );
                        for choice in choices {
                            let name = source_name(choice);
                            if ui
                                .selectable_label(choice == config.source, &name)
                                .on_hover_text(l.format(
                                    Msg::LiveLanSourceOptionHover,
                                    &[("source", Arg::Str(&name))],
                                ))
                                .clicked()
                                && choice != config.source
                            {
                                send(
                                    intents,
                                    LiveLanEntry {
                                        source: choice,
                                        ..config
                                    },
                                    SecretEdit::Keep,
                                );
                            }
                        }
                    })
                    .response
                    .hint(l.text(Msg::LiveLanSourceHover), reason);
            });
        });

        ui.horizontal(|ui| {
            ui.label(l.text(Msg::LiveLanMaxViewers));
            let mut viewers = config.max_viewers;
            let response = ui
                .add_enabled(
                    editable,
                    DragValue::new(&mut viewers)
                        .range(MIN_LIVE_LAN_VIEWERS..=MAX_LIVE_LAN_VIEWERS)
                        .speed(0.1),
                )
                .hint(l.text(Msg::LiveLanMaxViewersHover), reason);
            // A drag sends once, when it ends; a typed value at once.
            if viewers != config.max_viewers && !response.dragged() {
                send(
                    intents,
                    LiveLanEntry {
                        max_viewers: viewers,
                        ..config
                    },
                    SecretEdit::Keep,
                );
            }
        });
    });
    ui.add_space(6.0);

    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(section_label(l.text(Msg::LiveLanPinLabel)));
        ui.label(
            RichText::new(if config.has_pin {
                l.text(Msg::LiveLanPinStored)
            } else {
                l.text(Msg::LiveLanPinNone)
            })
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
        );
        ui.horizontal(|ui| {
            ui.add_enabled(
                editable,
                TextEdit::singleline(&mut window.pin)
                    .password(true)
                    .char_limit(MAX_LIVE_LAN_PIN_DIGITS)
                    .desired_width(140.0),
            )
            .hint(l.text(Msg::LiveLanPinHover), reason);
            window.pin.retain(|character| character.is_ascii_digit());
            let valid = fm_protocol::live_lan_pin_is_valid(&window.pin);
            if ui
                .add_enabled(editable && valid, Button::new(l.text(Msg::LiveLanPinSet)))
                .hint(
                    l.text(Msg::LiveLanPinSetHover),
                    blocked.unwrap_or(Msg::LiveLanReasonPinDigits.en()),
                )
                .clicked()
            {
                send(
                    intents,
                    config,
                    SecretEdit::Set(core::mem::take(&mut window.pin)),
                );
            }
            if ui
                .add_enabled(
                    editable && config.has_pin,
                    Button::new(l.text(Msg::LiveLanPinClear)),
                )
                .hint(
                    l.text(Msg::LiveLanPinClearHover),
                    blocked.unwrap_or(Msg::LiveLanReasonNoPin.en()),
                )
                .clicked()
            {
                send(intents, config, SecretEdit::Clear);
            }
        });
    });
}
