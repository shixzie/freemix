//! Output ▸ Fullscreen outputs… (OR-011, Protocol 2.42).
//!
//! Up to two display-output slots, each showing Program, Preview, or one
//! output's mix fullscreen on one display. A slot is project configuration:
//! Save and apply sends `set_display_output` and Clear sends
//! `clear_display_output`, both padlocked, undoable project edits the
//! daemon applies live. The dialog edits a local draft per slot until it is
//! applied, lists the displays from the daemon's `display_inventory` reply
//! (with their modes and HDR capability), and shows each configured slot's
//! live state (active on a display, or waiting and why).

use egui::{Button, ComboBox, RichText, Ui, Vec2};
use fm_protocol::{
    DisplayInventory, DisplayInventoryEntry, DisplayMode, DisplayOutputConfig, DisplayOutputSlot,
    DisplayOutputSource, DisplaySelector, HdrCapability, HdrRequest, HdrState, ModeRequest,
    WaitReason, WireDisplayOutputState,
};
use fm_types::OutputScaling;
use fm_ui_model::ClientView;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{Tone, card, muted_color, section_label, status_chip, text_dim_color};
use crate::{StudioIntent, StudioShell, StudioUiState, stinger_edit_lock_reason};

const DIALOG_WIDTH: f32 = 600.0;

/// The dialog's open state and per-slot drafts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DisplayOutputsDialog {
    /// Whether the dialog is open.
    pub(crate) open: bool,
    /// Unsaved edits per slot; `None` follows the replicated configuration.
    pub(crate) drafts: [Option<DisplayOutputConfig>; fm_protocol::MAX_DISPLAY_OUTPUTS],
}

/// Why slot edits are unavailable, if they are: a session gate first
/// (connection, synchronized state, the project-edit permission), then the
/// project padlock.
#[must_use]
pub fn display_output_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if state.connection_status.controls_enabled() && state.view.is_some() && state.can_edit_project
    {
        stinger_edit_lock_reason(state)
    } else {
        Some(unavailable_reason(state, Capability::EditProject))
    }
}

/// The intent that saves `config` into `slot`, when edits are allowed and
/// an output source names an output of the replicated view.
#[must_use]
pub fn display_output_set_intent(
    state: &StudioUiState,
    slot: DisplayOutputSlot,
    config: &DisplayOutputConfig,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    let known = match config.source {
        DisplayOutputSource::Output(output) => {
            view.outputs.iter().any(|status| status.output == output)
        }
        DisplayOutputSource::Program | DisplayOutputSource::Preview => true,
    };
    (display_output_block_reason(state).is_none()
        && known
        && view.display_output(slot) != Some(config))
    .then(|| StudioIntent::SetDisplayOutput {
        slot,
        config: Box::new(config.clone()),
    })
}

/// The intent that clears `slot`, when edits are allowed and it is
/// configured.
#[must_use]
pub fn display_output_clear_intent(
    state: &StudioUiState,
    slot: DisplayOutputSlot,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    (display_output_block_reason(state).is_none() && view.display_output(slot).is_some())
        .then_some(StudioIntent::ClearDisplayOutput { slot })
}

/// The inventory display `selector` resolves to, the way the presenter
/// resolves it: by id, by a unique name, or by desktop index.
pub(crate) fn resolve_display<'a>(
    inventory: Option<&'a DisplayInventory>,
    selector: &DisplaySelector,
) -> Option<&'a DisplayInventoryEntry> {
    let displays = &inventory?.displays;
    let by_name = |name: &str| {
        let mut named = displays.iter().filter(|display| display.name == name);
        let first = named.next()?;
        named.next().is_none().then_some(first)
    };
    match selector {
        DisplaySelector::Id(id) => displays.iter().find(|display| &display.id == id),
        DisplaySelector::IdOrName { id, name } => displays
            .iter()
            .find(|display| &display.id == id)
            .or_else(|| by_name(name)),
        DisplaySelector::Name(name) => by_name(name),
        DisplaySelector::Index(index) => displays.get(*index),
    }
}

const fn hdr_capability_text(capability: HdrCapability) -> Msg {
    match capability {
        HdrCapability::Hdr => Msg::HdrCapHdr,
        HdrCapability::Sdr => Msg::HdrCapSdr,
        HdrCapability::Unknown => Msg::HdrCapUnknown,
    }
}

const fn hdr_state_text(state: HdrState) -> Msg {
    match state {
        HdrState::Sdr => Msg::HdrStateSdr,
        HdrState::Hdr => Msg::HdrStateHdr,
        HdrState::SdrDisplayNotCapable => Msg::HdrStateNotCapable,
        HdrState::SdrCapabilityUnknown => Msg::HdrStateUnknown,
        HdrState::SdrPresenterLimited => Msg::HdrStatePresenterLimited,
    }
}

/// The operator sentence for a waiting slot.
#[must_use]
pub(crate) const fn wait_reason_text(reason: WaitReason) -> Msg {
    match reason {
        WaitReason::DisplayAbsent => Msg::WaitDisplayAbsent,
        WaitReason::DisplayInUse => Msg::WaitDisplayInUse,
        WaitReason::AmbiguousName => Msg::WaitAmbiguousName,
        WaitReason::EnumerationFailed => Msg::WaitEnumerationFailed,
        WaitReason::NoPresenter => Msg::WaitNoPresenter,
    }
}

const fn scaling_text(scaling: OutputScaling) -> (Msg, Msg) {
    match scaling {
        OutputScaling::Fit => (Msg::ScalingFit, Msg::ScalingFitHover),
        OutputScaling::Fill => (Msg::ScalingFill, Msg::ScalingFillHover),
        OutputScaling::Stretch => (Msg::ScalingStretch, Msg::ScalingStretchHover),
        OutputScaling::OneToOne => (Msg::ScalingOneToOne, Msg::ScalingOneToOneHover),
    }
}

/// `1920×1080 @ 59.94 Hz` in the locale's decimal style.
pub(crate) fn mode_text(mode: DisplayMode) -> String {
    let hertz = mode.refresh_millihertz / 1000;
    let fraction = mode.refresh_millihertz % 1000;
    if fraction == 0 {
        format!("{}×{} @ {hertz} Hz", mode.width, mode.height)
    } else {
        let fraction = format!("{fraction:03}");
        format!(
            "{}×{} @ {hertz}.{} Hz",
            mode.width,
            mode.height,
            fraction.trim_end_matches('0')
        )
    }
}

/// The status chip, its tone, and the detail line of one slot.
pub(crate) fn slot_status(
    l: Locale,
    configured: bool,
    state: Option<&WireDisplayOutputState>,
) -> (&'static str, Tone, String) {
    if !configured {
        return (
            Msg::DisplayStatusOff.en(),
            Tone::Idle,
            l.text(Msg::DisplayNotConfigured).to_owned(),
        );
    }
    match state {
        Some(WireDisplayOutputState::Active(active)) => {
            let mut detail = l.format(
                Msg::DisplayActiveDetail,
                &[
                    ("name", Arg::Str(&active.name)),
                    ("mode", Arg::Str(&mode_text(active.mode))),
                    ("hdr", Arg::Str(l.text(hdr_state_text(active.hdr)))),
                ],
            );
            if active.mode_fallback {
                detail.push_str(". ");
                detail.push_str(l.text(Msg::DisplayModeFallback));
            }
            (Msg::DisplayStatusActive.en(), Tone::Ready, detail)
        }
        Some(WireDisplayOutputState::Waiting(reason)) => (
            Msg::DisplayStatusWaiting.en(),
            Tone::Pending,
            l.text(wait_reason_text(*reason)).to_owned(),
        ),
        None => (
            Msg::DisplayStatusWaiting.en(),
            Tone::Pending,
            l.text(Msg::WaitNoPresenter).to_owned(),
        ),
    }
}

fn source_label(l: Locale, view: &ClientView, source: DisplayOutputSource) -> String {
    match source {
        DisplayOutputSource::Program => l.text(Msg::DisplaySourceProgram).to_owned(),
        DisplayOutputSource::Preview => l.text(Msg::DisplaySourcePreview).to_owned(),
        DisplayOutputSource::Output(output) => view
            .outputs
            .iter()
            .find(|status| status.output == output)
            .map_or_else(|| output.to_string(), |status| status.name.clone()),
    }
}

fn display_label(l: Locale, display: &DisplayInventoryEntry) -> String {
    l.format(
        Msg::DisplayEntry,
        &[
            ("name", Arg::Str(&display.name)),
            ("mode", Arg::Str(&mode_text(display.current_mode))),
        ],
    )
}

fn inventory_line(l: Locale, inventory: Option<&DisplayInventory>) -> String {
    match inventory {
        None => l.text(Msg::DisplayInventoryUnknown).to_owned(),
        Some(inventory) if !inventory.presenter => {
            l.text(Msg::DisplayInventoryNoPresenter).to_owned()
        }
        Some(inventory) => {
            #[allow(clippy::cast_possible_truncation)] // at most 16 displays
            let mut line = l.format(
                Msg::DisplayInventoryCount,
                &[("count", Arg::from(inventory.displays.len() as u32))],
            );
            if let Some(error) = &inventory.error {
                line.push_str(". ");
                line.push_str(&l.format(Msg::DisplayInventoryError, &[("error", Arg::Str(error))]));
            }
            line
        }
    }
}

impl StudioShell {
    /// Opens the Fullscreen outputs dialog and asks for the display list.
    pub(crate) fn open_display_outputs(&mut self, intents: &mut Vec<StudioIntent>) {
        self.display_outputs.open = true;
        intents.push(StudioIntent::RequestDisplayInventory);
    }

    /// Draws the Fullscreen outputs dialog when open.
    pub(crate) fn draw_display_outputs_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.display_outputs.open {
            return;
        }
        let l = self.locale;
        let dialog = &mut self.display_outputs;
        let close = crate::menu::help_modal(
            context,
            "studio-display-outputs",
            l.text(Msg::DisplayOutputsTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, dialog, intents),
        );
        if close {
            self.display_outputs.open = false;
        }
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    dialog: &mut DisplayOutputsDialog,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::DisplayOutputsIntro))
            .size(crate::theme::legible(11.0))
            .color(text_dim_color()),
    );
    let inventory = state
        .view
        .as_ref()
        .and_then(|view| view.display_inventory.as_ref());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(inventory_line(l, inventory))
                .size(crate::theme::legible(11.0))
                .color(muted_color()),
        );
        ui.with_layout(l10n::trailing_layout(l), |ui| {
            if ui
                .add_enabled(
                    state.connection_status.controls_enabled(),
                    Button::new(l.text(Msg::DisplayRefresh)),
                )
                .hint(
                    l.text(Msg::DisplayRefreshHover),
                    unavailable_reason(state, Capability::EditProject),
                )
                .clicked()
            {
                intents.push(StudioIntent::RequestDisplayInventory);
            }
        });
    });
    ui.add_space(6.0);
    for slot in DisplayOutputSlot::ALL {
        draw_slot(ui, state, dialog, slot, intents);
        ui.add_space(6.0);
    }
}

#[allow(clippy::too_many_lines)] // one immediate-mode card
fn draw_slot(
    ui: &mut Ui,
    state: &StudioUiState,
    dialog: &mut DisplayOutputsDialog,
    slot: DisplayOutputSlot,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let blocked = display_output_block_reason(state);
    let reason = blocked.unwrap_or_else(|| unavailable_reason(state, Capability::EditProject));
    let slot_arg = ("slot", Arg::Plain(u64::from(slot.get())));
    let view = state.view.as_ref();
    let saved = view.and_then(|view| view.display_output(slot)).cloned();
    // A draft equal to the saved configuration is no draft.
    if dialog.drafts[slot.index()].as_ref() == saved.as_ref() {
        dialog.drafts[slot.index()] = None;
    }
    let draft = dialog.drafts[slot.index()]
        .clone()
        .or_else(|| saved.clone());
    let inventory = view.and_then(|view| view.display_inventory.as_ref());
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        let (chip, tone, detail) = slot_status(
            l,
            saved.is_some(),
            view.and_then(|view| view.display_output_state(slot)),
        );
        ui.horizontal(|ui| {
            ui.label(section_label(
                &l.format(Msg::DisplaySlotHeading, &[slot_arg]),
            ));
            ui.with_layout(l10n::trailing_layout(l), |ui| {
                status_chip(ui, l.tr(chip), tone, 96.0)
                    .on_hover_text(l.format(Msg::DisplayStatusHover, &[slot_arg]));
            });
        });
        ui.label(
            RichText::new(detail)
                .size(crate::theme::legible(10.5))
                .color(text_dim_color()),
        );
        let Some(view) = view else {
            return;
        };
        let mut edited = draft.clone();
        let editable = blocked.is_none();
        let salt = |name: &str| format!("studio-display-{name}-{slot}");
        egui::Grid::new(salt("grid"))
            .num_columns(2)
            .spacing(Vec2::new(12.0, 4.0))
            .show(ui, |ui| {
                // Display.
                ui.label(l.text(Msg::DisplayPick));
                ui.add_enabled_ui(editable, |ui| {
                    let selected = edited.as_ref().map(|config| config.display.clone());
                    let selected = selected.as_ref();
                    let selected_text = selected.map_or_else(
                        || l.text(Msg::DisplayNone).to_owned(),
                        |selector| {
                            resolve_display(inventory, selector).map_or_else(
                                || {
                                    l.format(
                                        Msg::DisplayNotConnected,
                                        &[("display", Arg::Str(&selector.to_string()))],
                                    )
                                },
                                |display| display_label(l, display),
                            )
                        },
                    );
                    ComboBox::from_id_salt(salt("display"))
                        .width(360.0)
                        .truncate()
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            if let Some(selector) = selected
                                && resolve_display(inventory, selector).is_none()
                            {
                                let text = selector.to_string();
                                let _ = ui
                                    .selectable_label(
                                        true,
                                        l.format(
                                            Msg::DisplayNotConnected,
                                            &[("display", Arg::Str(&text))],
                                        ),
                                    )
                                    .on_hover_text(l.format(
                                        Msg::DisplayNotConnectedHover,
                                        &[("display", Arg::Str(&text))],
                                    ));
                            }
                            for display in inventory.map_or(&[][..], |i| i.displays.as_slice()) {
                                let chosen = selected.is_some_and(|selector| {
                                    resolve_display(inventory, selector)
                                        .is_some_and(|resolved| resolved.id == display.id)
                                });
                                if ui
                                    .selectable_label(chosen, display_label(l, display))
                                    .on_hover_text(l.format(
                                        Msg::DisplayOptionHover,
                                        &[
                                            ("name", Arg::Str(&display.name)),
                                            ("x", Arg::Str(&display.x.to_string())),
                                            ("y", Arg::Str(&display.y.to_string())),
                                            ("mode", Arg::Str(&mode_text(display.current_mode))),
                                            (
                                                "hdr",
                                                Arg::Str(l.text(hdr_capability_text(display.hdr))),
                                            ),
                                        ],
                                    ))
                                    .clicked()
                                    && !chosen
                                {
                                    // Remembered by id, then by a unique name.
                                    let selector = DisplaySelector::IdOrName {
                                        id: display.id.clone(),
                                        name: display.name.clone(),
                                    };
                                    match edited.as_mut() {
                                        Some(config) => {
                                            config.display = selector;
                                            // A mode the new display lacks reverts to current.
                                            if let ModeRequest::Exact(mode) = config.mode
                                                && !display.modes.contains(&mode)
                                            {
                                                config.mode = ModeRequest::Current;
                                            }
                                        }
                                        None => {
                                            edited =
                                                Some(DisplayOutputConfig::program_on(selector));
                                        }
                                    }
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::DisplayPickHover), reason);
                });
                ui.end_row();
                let Some(config) = edited.as_mut() else {
                    return;
                };
                let resolved = resolve_display(inventory, &config.display);
                // Source.
                ui.label(l.text(Msg::DisplaySource));
                ui.add_enabled_ui(editable, |ui| {
                    ComboBox::from_id_salt(salt("source"))
                        .width(360.0)
                        .truncate()
                        .selected_text(source_label(l, view, config.source))
                        .show_ui(ui, |ui| {
                            let sources =
                                [DisplayOutputSource::Program, DisplayOutputSource::Preview]
                                    .into_iter()
                                    .chain(
                                        view.outputs.iter().map(|status| {
                                            DisplayOutputSource::Output(status.output)
                                        }),
                                    );
                            for source in sources {
                                let label = source_label(l, view, source);
                                if ui
                                    .selectable_label(config.source == source, &label)
                                    .on_hover_text(l.format(
                                        Msg::DisplaySourceOptionHover,
                                        &[("source", Arg::Str(&label))],
                                    ))
                                    .clicked()
                                {
                                    config.source = source;
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::DisplaySourceHover), reason);
                });
                ui.end_row();
                // Mode.
                ui.label(l.text(Msg::DisplayModeLabel));
                ui.add_enabled_ui(editable, |ui| {
                    let current = l.text(Msg::DisplayModeCurrent);
                    let selected = match config.mode {
                        ModeRequest::Current => current.to_owned(),
                        ModeRequest::Exact(mode) => mode_text(mode),
                    };
                    ComboBox::from_id_salt(salt("mode"))
                        .width(360.0)
                        .truncate()
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(config.mode == ModeRequest::Current, current)
                                .on_hover_text(l.text(Msg::DisplayModeCurrentHover))
                                .clicked()
                            {
                                config.mode = ModeRequest::Current;
                            }
                            let mut modes: Vec<DisplayMode> = resolved
                                .map(|display| display.modes.clone())
                                .unwrap_or_default();
                            if let ModeRequest::Exact(mode) = config.mode
                                && !modes.contains(&mode)
                            {
                                modes.insert(0, mode);
                            }
                            for mode in modes {
                                let text = mode_text(mode);
                                if ui
                                    .selectable_label(
                                        config.mode == ModeRequest::Exact(mode),
                                        &text,
                                    )
                                    .on_hover_text(l.format(
                                        Msg::DisplayModeOptionHover,
                                        &[("mode", Arg::Str(&text))],
                                    ))
                                    .clicked()
                                {
                                    config.mode = ModeRequest::Exact(mode);
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::DisplayModeHover), reason);
                });
                ui.end_row();
                // Scaling.
                ui.label(l.text(Msg::DisplayScaling));
                ui.add_enabled_ui(editable, |ui| {
                    ComboBox::from_id_salt(salt("scaling"))
                        .width(360.0)
                        .selected_text(l.text(scaling_text(config.scaling).0))
                        .show_ui(ui, |ui| {
                            for scaling in OutputScaling::ALL {
                                let (label, hover) = scaling_text(scaling);
                                if ui
                                    .selectable_label(config.scaling == scaling, l.text(label))
                                    .on_hover_text(l.text(hover))
                                    .clicked()
                                {
                                    config.scaling = scaling;
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::DisplayScalingHover), reason);
                });
                ui.end_row();
                // HDR.
                ui.label(l.text(Msg::DisplayHdr));
                ui.add_enabled_ui(editable, |ui| {
                    let capability = resolved.map_or(HdrCapability::Unknown, |display| display.hdr);
                    let hdr_label = |hdr: HdrRequest| match hdr {
                        HdrRequest::Off => (Msg::HdrOff, Msg::HdrOffHover),
                        HdrRequest::Auto => (Msg::HdrAuto, Msg::HdrAutoHover),
                    };
                    ComboBox::from_id_salt(salt("hdr"))
                        .width(360.0)
                        .selected_text(l.text(hdr_label(config.hdr).0))
                        .show_ui(ui, |ui| {
                            for hdr in [HdrRequest::Off, HdrRequest::Auto] {
                                let (label, hover) = hdr_label(hdr);
                                if ui
                                    .selectable_label(config.hdr == hdr, l.text(label))
                                    .on_hover_text(l.text(hover))
                                    .clicked()
                                {
                                    config.hdr = hdr;
                                }
                            }
                        })
                        .response
                        .hint(
                            &l.format(
                                Msg::DisplayHdrHover,
                                &[(
                                    "capability",
                                    Arg::Str(l.text(hdr_capability_text(capability))),
                                )],
                            ),
                            reason,
                        );
                });
                ui.end_row();
            });
        if edited != draft {
            dialog.drafts[slot.index()].clone_from(&edited);
        }
        let unsaved = dialog.drafts[slot.index()].is_some();
        if unsaved {
            ui.label(
                RichText::new(l.text(Msg::DisplayUnsaved))
                    .size(crate::theme::legible(10.5))
                    .color(crate::theme::accent_color()),
            );
        }
        ui.horizontal(|ui| {
            let apply = edited
                .as_ref()
                .and_then(|config| display_output_set_intent(state, slot, config));
            let apply_reason = if blocked.is_some() {
                reason
            } else if edited.is_none() {
                Msg::ReasonDisplayChooseDisplay.en()
            } else {
                Msg::ReasonDisplayNoChanges.en()
            };
            if ui
                .add_enabled(apply.is_some(), Button::new(l.text(Msg::DisplayApply)))
                .hint(&l.format(Msg::DisplayApplyHover, &[slot_arg]), apply_reason)
                .clicked()
                && let Some(intent) = apply
            {
                intents.push(intent);
                dialog.drafts[slot.index()] = None;
            }
            if ui
                .add_enabled(unsaved, Button::new(l.text(Msg::DisplayRevert)))
                .hint(
                    &l.format(Msg::DisplayRevertHover, &[slot_arg]),
                    Msg::ReasonDisplayNoChanges.en(),
                )
                .clicked()
            {
                dialog.drafts[slot.index()] = None;
            }
            let clear = display_output_clear_intent(state, slot);
            if ui
                .add_enabled(clear.is_some(), Button::new(l.text(Msg::DisplayClear)))
                .hint(
                    &l.format(Msg::DisplayClearHover, &[slot_arg]),
                    if blocked.is_some() {
                        reason
                    } else {
                        Msg::ReasonDisplayNotConfigured.en()
                    },
                )
                .clicked()
                && let Some(intent) = clear
            {
                intents.push(intent);
                dialog.drafts[slot.index()] = None;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_protocol::{DisplayId, WireActiveDisplay};

    fn slot(value: u8) -> DisplayOutputSlot {
        DisplayOutputSlot::new(value).unwrap()
    }

    fn inventory() -> DisplayInventory {
        let entry = |id: &str, name: &str| DisplayInventoryEntry {
            id: DisplayId::new(id).unwrap(),
            name: name.into(),
            x: 0,
            y: 0,
            current_mode: DisplayMode::new(1920, 1080, 60_000),
            modes: vec![DisplayMode::new(1920, 1080, 60_000)],
            hdr: HdrCapability::Sdr,
        };
        DisplayInventory {
            presenter: true,
            error: None,
            displays: vec![
                entry("left", "Studio"),
                entry("right", "Studio"),
                entry("wall", "Wall"),
            ],
        }
    }

    #[test]
    fn selectors_resolve_like_the_presenter() {
        let inventory = inventory();
        let resolve = |text: &str| {
            resolve_display(Some(&inventory), &text.parse().unwrap())
                .map(|display| display.id.as_str().to_owned())
        };
        assert_eq!(resolve("id:right").as_deref(), Some("right"));
        let renumbered = DisplaySelector::IdOrName {
            id: DisplayId::new("gone").unwrap(),
            name: "Wall".into(),
        };
        assert_eq!(
            resolve_display(Some(&inventory), &renumbered).map(|display| display.id.as_str()),
            Some("wall")
        );
        assert_eq!(resolve("name:Wall").as_deref(), Some("wall"));
        assert_eq!(
            resolve("name:Studio"),
            None,
            "an ambiguous name resolves nowhere"
        );
        assert_eq!(resolve("index:1").as_deref(), Some("right"));
        assert_eq!(resolve("index:7"), None);
        assert_eq!(resolve_display(None, &"index:0".parse().unwrap()), None);
    }

    #[test]
    fn modes_and_slot_states_read_in_words() {
        assert_eq!(
            mode_text(DisplayMode::new(1920, 1080, 59_940)),
            "1920×1080 @ 59.94 Hz"
        );
        assert_eq!(
            mode_text(DisplayMode::new(3840, 2160, 50_000)),
            "3840×2160 @ 50 Hz"
        );
        let l = Locale::ENGLISH;
        assert_eq!(
            slot_status(l, false, None),
            ("UNUSED", Tone::Idle, "Not configured".to_owned())
        );
        assert_eq!(
            slot_status(
                l,
                true,
                Some(&WireDisplayOutputState::Waiting(WaitReason::DisplayInUse))
            ),
            (
                "STANDBY",
                Tone::Pending,
                "Waiting: another output uses this display".to_owned()
            )
        );
        let active = WireDisplayOutputState::Active(WireActiveDisplay {
            id: DisplayId::new("wall").unwrap(),
            name: "Wall".into(),
            mode: DisplayMode::new(1920, 1080, 60_000),
            mode_fallback: true,
            hdr: HdrState::SdrDisplayNotCapable,
        });
        let (chip, tone, detail) = slot_status(l, true, Some(&active));
        assert_eq!((chip, tone), ("ACTIVE", Tone::Ready));
        assert!(detail.starts_with("On Wall at 1920×1080 @ 60 Hz; SDR, the display"));
        assert!(detail.ends_with("so the current mode is used"), "{detail}");
        assert_eq!(slot(2).get(), 2);
    }
}
