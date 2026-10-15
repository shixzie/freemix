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
//!
//! A draft records the slot configuration it was started from. Apply sends
//! it only while the live slot still equals that base, and keeps it until
//! the server answers: an accepted change clears it once the change lands,
//! a refused one keeps it and shows why. A slot changed elsewhere meanwhile
//! offers Reload (start again from the live slot) or Overwrite (send the
//! draft anyway).

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
const SLOTS: usize = fm_protocol::MAX_DISPLAY_OUTPUTS;

/// The server's answer to this session's latest set or clear of one slot,
/// kept by the host per slot until another answer for that slot arrives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputAnswer {
    pub command_id: String,
    pub slot: DisplayOutputSlot,
    /// The revision the accepted change lands at, or why it was refused.
    pub outcome: Result<u64, String>,
}

/// One slot's unsaved edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SlotDraft {
    /// The edited configuration.
    pub(crate) config: DisplayOutputConfig,
    /// The live configuration the edit started from (`None`: unconfigured).
    pub(crate) base: Option<DisplayOutputConfig>,
}

/// A change this dialog sent for one slot and has not seen settle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Awaiting {
    /// Sent; the server has not answered.
    Set(DisplayOutputConfig),
    /// Accepted at a revision the replicated view has not reached yet.
    Landing {
        config: DisplayOutputConfig,
        revision: u64,
    },
    /// A clear was sent.
    Clear,
}

/// The dialog's open state and per-slot drafts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DisplayOutputsDialog {
    /// Whether the dialog is open.
    pub(crate) open: bool,
    /// Unsaved edits per slot; `None` follows the replicated configuration.
    pub(crate) drafts: [Option<SlotDraft>; SLOTS],
    /// The change sent per slot and not settled yet.
    pub(crate) awaiting: [Option<Awaiting>; SLOTS],
    /// Why the server refused the latest change per slot.
    pub(crate) refusals: [Option<String>; SLOTS],
    /// The command id of the latest answer handled per slot.
    seen_answers: [Option<String>; SLOTS],
}

impl DisplayOutputsDialog {
    /// Settles sent changes against the server's answers and the
    /// replicated slots; run every frame, open or not. `revision` is the
    /// replicated view's, and `live` each slot's configuration.
    pub(crate) fn settle(
        &mut self,
        revision: Option<u64>,
        live: [Option<&DisplayOutputConfig>; SLOTS],
        answers: &[Option<DisplayOutputAnswer>; SLOTS],
    ) {
        for index in 0..SLOTS {
            if let Some(answer) = &answers[index]
                && self.seen_answers[index].as_ref() != Some(&answer.command_id)
            {
                self.seen_answers[index] = Some(answer.command_id.clone());
                match (&answer.outcome, self.awaiting[index].take()) {
                    (Err(reason), Some(_)) => self.refusals[index] = Some(reason.clone()),
                    (Ok(revision), Some(Awaiting::Set(config))) => {
                        self.awaiting[index] = Some(Awaiting::Landing {
                            config,
                            revision: *revision,
                        });
                    }
                    (_, awaiting) => self.awaiting[index] = awaiting,
                }
            }
            let landed = match &self.awaiting[index] {
                // The change is on screen (its echo may beat the answer).
                Some(Awaiting::Set(config) | Awaiting::Landing { config, .. })
                    if live[index] == Some(config) =>
                {
                    Some(config.clone())
                }
                // Accepted and replicated, though something newer may have
                // replaced it since.
                Some(Awaiting::Landing {
                    config,
                    revision: at,
                }) if revision.is_some_and(|revision| revision >= *at) => Some(config.clone()),
                Some(Awaiting::Clear) if live[index].is_none() => {
                    self.awaiting[index] = None;
                    None
                }
                _ => None,
            };
            if let Some(config) = landed {
                self.awaiting[index] = None;
                // The applied draft is done; later edits build on it.
                match &mut self.drafts[index] {
                    Some(draft) if draft.config == config => self.drafts[index] = None,
                    Some(draft) => draft.base = Some(config),
                    None => {}
                }
            }
            // A draft equal to the live configuration is no draft.
            if self.awaiting[index].is_none()
                && self.drafts[index]
                    .as_ref()
                    .is_some_and(|draft| Some(&draft.config) == live[index])
            {
                self.drafts[index] = None;
            }
        }
    }

    /// Records an edit of `slot`; a new draft starts from `live`.
    pub(crate) fn edit(
        &mut self,
        slot: DisplayOutputSlot,
        live: Option<&DisplayOutputConfig>,
        config: DisplayOutputConfig,
    ) {
        match &mut self.drafts[slot.index()] {
            Some(draft) => draft.config = config,
            draft @ None => {
                *draft = Some(SlotDraft {
                    config,
                    base: live.cloned(),
                });
            }
        }
    }

    /// Whether `slot` changed elsewhere since its draft started.
    pub(crate) fn changed_elsewhere(
        &self,
        slot: DisplayOutputSlot,
        live: Option<&DisplayOutputConfig>,
    ) -> bool {
        self.awaiting[slot.index()].is_none()
            && self.drafts[slot.index()]
                .as_ref()
                .is_some_and(|draft| draft.base.as_ref() != live)
    }

    /// Whether a change to `slot` was sent and has not settled.
    pub(crate) const fn applying(&self, slot: DisplayOutputSlot) -> bool {
        self.awaiting[slot.index()].is_some()
    }

    /// The intent that applies the draft of `slot`: only while the live
    /// slot still equals the draft's base, unless `overwrite` rebases the
    /// draft onto the live slot first. Marks the slot as applying.
    pub(crate) fn apply(
        &mut self,
        state: &StudioUiState,
        slot: DisplayOutputSlot,
        overwrite: bool,
    ) -> Option<StudioIntent> {
        let live = state
            .view
            .as_ref()
            .and_then(|view| view.display_output(slot))
            .cloned();
        if self.applying(slot) || (!overwrite && self.changed_elsewhere(slot, live.as_ref())) {
            return None;
        }
        let draft = self.drafts[slot.index()].as_mut()?;
        let intent = display_output_set_intent(state, slot, &draft.config)?;
        draft.base = live;
        self.awaiting[slot.index()] = Some(Awaiting::Set(draft.config.clone()));
        self.refusals[slot.index()] = None;
        Some(intent)
    }

    /// The intent that clears `slot`, discarding its draft.
    pub(crate) fn clear(
        &mut self,
        state: &StudioUiState,
        slot: DisplayOutputSlot,
    ) -> Option<StudioIntent> {
        let intent = display_output_clear_intent(state, slot)?;
        self.drafts[slot.index()] = None;
        self.awaiting[slot.index()] = Some(Awaiting::Clear);
        self.refusals[slot.index()] = None;
        Some(intent)
    }

    /// Discards the draft of `slot` and forgets any change in flight, so
    /// the slot follows the live configuration again.
    pub(crate) fn discard(&mut self, slot: DisplayOutputSlot) {
        self.drafts[slot.index()] = None;
        self.awaiting[slot.index()] = None;
        self.refusals[slot.index()] = None;
    }
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
        let view = state.view.as_ref();
        self.display_outputs.settle(
            view.map(|view| view.cursor.revision.get()),
            DisplayOutputSlot::ALL.map(|slot| view.and_then(|view| view.display_output(slot))),
            &state.display_output_answers,
        );
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
    let draft = dialog.drafts[slot.index()]
        .as_ref()
        .map(|draft| draft.config.clone())
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
        if edited != draft
            && let Some(config) = edited.clone()
        {
            dialog.edit(slot, saved.as_ref(), config);
        }
        draw_slot_footer(ui, state, dialog, slot, edited.is_none(), intents);
    });
}

/// The slot's draft notices (unsaved, applying, refused, changed
/// elsewhere) and its buttons.
#[allow(clippy::too_many_lines)] // one immediate-mode row of notices and buttons
fn draw_slot_footer(
    ui: &mut Ui,
    state: &StudioUiState,
    dialog: &mut DisplayOutputsDialog,
    slot: DisplayOutputSlot,
    unconfigured: bool,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let blocked = display_output_block_reason(state);
    let reason = blocked.unwrap_or_else(|| unavailable_reason(state, Capability::EditProject));
    let slot_arg = ("slot", Arg::Plain(u64::from(slot.get())));
    let saved = state
        .view
        .as_ref()
        .and_then(|view| view.display_output(slot))
        .cloned();
    let unsaved = dialog.drafts[slot.index()].is_some();
    let applying = dialog.applying(slot);
    let elsewhere = dialog.changed_elsewhere(slot, saved.as_ref());
    let note = |ui: &mut Ui, text: &str, color: egui::Color32| {
        ui.label(
            RichText::new(text)
                .size(crate::theme::legible(10.5))
                .color(color),
        );
    };
    if applying {
        note(ui, l.text(Msg::DisplayApplying), muted_color());
    } else if unsaved {
        note(
            ui,
            l.text(Msg::DisplayUnsaved),
            crate::theme::accent_color(),
        );
    }
    if let Some(refusal) = &dialog.refusals[slot.index()] {
        note(
            ui,
            &l.format(Msg::DisplayRefused, &[("reason", Arg::Str(l.tr(refusal)))]),
            crate::theme::ERROR,
        );
    }
    if elsewhere {
        note(
            ui,
            &l.format(Msg::DisplayChangedElsewhere, &[slot_arg]),
            crate::theme::AMBER,
        );
    }
    ui.horizontal(|ui| {
        let applicable = dialog.drafts[slot.index()]
            .as_ref()
            .is_some_and(|draft| display_output_set_intent(state, slot, &draft.config).is_some());
        if elsewhere {
            if ui
                .button(l.text(Msg::DisplayReload))
                .on_hover_text(l.format(Msg::DisplayReloadHover, &[slot_arg]))
                .clicked()
            {
                dialog.discard(slot);
            }
            if ui
                .add_enabled(applicable, Button::new(l.text(Msg::DisplayOverwrite)))
                .hint(&l.format(Msg::DisplayOverwriteHover, &[slot_arg]), reason)
                .clicked()
                && let Some(intent) = dialog.apply(state, slot, true)
            {
                intents.push(intent);
            }
        } else {
            let apply_reason = if blocked.is_some() {
                reason
            } else if applying {
                Msg::ReasonDisplayApplying.en()
            } else if unconfigured {
                Msg::ReasonDisplayChooseDisplay.en()
            } else {
                Msg::ReasonDisplayNoChanges.en()
            };
            if ui
                .add_enabled(
                    applicable && !applying,
                    Button::new(l.text(Msg::DisplayApply)),
                )
                .hint(&l.format(Msg::DisplayApplyHover, &[slot_arg]), apply_reason)
                .clicked()
                && let Some(intent) = dialog.apply(state, slot, false)
            {
                intents.push(intent);
            }
        }
        if ui
            .add_enabled(unsaved || applying, Button::new(l.text(Msg::DisplayRevert)))
            .hint(
                &l.format(Msg::DisplayRevertHover, &[slot_arg]),
                Msg::ReasonDisplayNoChanges.en(),
            )
            .clicked()
        {
            dialog.discard(slot);
        }
        let clear = !applying && display_output_clear_intent(state, slot).is_some();
        if ui
            .add_enabled(clear, Button::new(l.text(Msg::DisplayClear)))
            .hint(
                &l.format(Msg::DisplayClearHover, &[slot_arg]),
                if blocked.is_some() {
                    reason
                } else if applying {
                    Msg::ReasonDisplayApplying.en()
                } else {
                    Msg::ReasonDisplayNotConfigured.en()
                },
            )
            .clicked()
            && let Some(intent) = dialog.clear(state, slot)
        {
            intents.push(intent);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_protocol::{DisplayId, WireActiveDisplay};

    fn slot(value: u8) -> DisplayOutputSlot {
        DisplayOutputSlot::new(value).unwrap()
    }

    fn config(scaling: OutputScaling) -> DisplayOutputConfig {
        DisplayOutputConfig {
            scaling,
            ..DisplayOutputConfig::program_on(DisplaySelector::Name("Wall".into()))
        }
    }

    /// A ready operator session whose slot 1 is `live`, at `revision`.
    fn state(live: Option<DisplayOutputConfig>, revision: u64) -> StudioUiState {
        let mut view = crate::tests::recording_view([false, false]);
        let mut cursor = view.cursor.resume_cursor();
        cursor.revision = revision;
        view.cursor =
            fm_ui_model::ProjectCursor::from_resume_cursor(view.cursor.project_id, cursor).unwrap();
        view.display_outputs = live
            .into_iter()
            .map(|config| fm_protocol::DisplayOutputEntry {
                slot: slot(1),
                config,
            })
            .collect();
        let mut state = StudioUiState::new(crate::StudioConnectionStatus::Ready).with_view(view);
        state.can_edit_project = true;
        state
    }

    fn settle(dialog: &mut DisplayOutputsDialog, state: &StudioUiState) {
        let view = state.view.as_ref().unwrap();
        dialog.settle(
            Some(view.cursor.revision.get()),
            DisplayOutputSlot::ALL.map(|slot| view.display_output(slot)),
            &state.display_output_answers,
        );
    }

    fn answer(command_id: &str, outcome: Result<u64, String>) -> [Option<DisplayOutputAnswer>; 2] {
        [
            Some(DisplayOutputAnswer {
                command_id: command_id.into(),
                slot: slot(1),
                outcome,
            }),
            None,
        ]
    }

    #[test]
    fn a_refused_apply_keeps_the_draft_and_says_why() {
        let mut dialog = DisplayOutputsDialog::default();
        let mut live = state(Some(config(OutputScaling::Fit)), 5);
        dialog.edit(
            slot(1),
            live.view.as_ref().unwrap().display_output(slot(1)),
            config(OutputScaling::Fill),
        );
        let intent = dialog.apply(&live, slot(1), false);
        assert!(matches!(
            intent,
            Some(StudioIntent::SetDisplayOutput { .. })
        ));
        settle(&mut dialog, &live);
        assert!(dialog.applying(slot(1)), "the draft waits for the answer");
        assert_eq!(
            dialog.drafts[0].as_ref().map(|draft| &draft.config),
            Some(&config(OutputScaling::Fill))
        );
        assert_eq!(dialog.apply(&live, slot(1), false), None, "no second send");

        live.display_output_answers = answer("cmd-1", Err("the project is locked".into()));
        settle(&mut dialog, &live);
        assert!(!dialog.applying(slot(1)));
        assert_eq!(dialog.refusals[0].as_deref(), Some("the project is locked"));
        assert_eq!(
            dialog.drafts[0].as_ref().map(|draft| &draft.config),
            Some(&config(OutputScaling::Fill)),
            "the refused edit is kept"
        );
        // The same answer again changes nothing; Apply can retry.
        settle(&mut dialog, &live);
        assert!(dialog.apply(&live, slot(1), false).is_some());
        assert_eq!(dialog.refusals[0], None);
    }

    #[test]
    fn an_accepted_apply_clears_the_draft_once_it_lands() {
        let mut dialog = DisplayOutputsDialog::default();
        let before = state(Some(config(OutputScaling::Fit)), 5);
        dialog.edit(
            slot(1),
            Some(&config(OutputScaling::Fit)),
            config(OutputScaling::Fill),
        );
        assert!(dialog.apply(&before, slot(1), false).is_some());
        // The acceptance can arrive before its echo: the draft stays.
        let mut accepted = before.clone();
        accepted.display_output_answers = answer("cmd-1", Ok(6));
        settle(&mut dialog, &accepted);
        assert!(dialog.applying(slot(1)));
        assert!(dialog.drafts[0].is_some());
        // The echo lands: the draft is done.
        let mut landed = state(Some(config(OutputScaling::Fill)), 6);
        landed.display_output_answers = accepted.display_output_answers.clone();
        settle(&mut dialog, &landed);
        assert!(!dialog.applying(slot(1)));
        assert_eq!(dialog.drafts[0], None);
        assert!(!dialog.changed_elsewhere(slot(1), Some(&config(OutputScaling::Fill))));

        // Accepted, but another client's change replaced it in the same
        // update: the applied draft is done all the same.
        let mut dialog = DisplayOutputsDialog::default();
        dialog.edit(
            slot(1),
            Some(&config(OutputScaling::Fit)),
            config(OutputScaling::Fill),
        );
        assert!(dialog.apply(&before, slot(1), false).is_some());
        let mut replaced = state(Some(config(OutputScaling::Stretch)), 7);
        replaced.display_output_answers = answer("cmd-2", Ok(6));
        settle(&mut dialog, &replaced);
        assert!(!dialog.applying(slot(1)));
        assert_eq!(dialog.drafts[0], None);
    }

    #[test]
    fn a_slot_changed_elsewhere_is_not_clobbered_until_overwrite() {
        let mut dialog = DisplayOutputsDialog::default();
        dialog.edit(
            slot(1),
            Some(&config(OutputScaling::Fit)),
            config(OutputScaling::Fill),
        );
        // Another client changes the slot meanwhile.
        let theirs = state(Some(config(OutputScaling::Stretch)), 6);
        settle(&mut dialog, &theirs);
        let live = Some(config(OutputScaling::Stretch));
        assert!(dialog.changed_elsewhere(slot(1), live.as_ref()));
        assert_eq!(
            dialog.apply(&theirs, slot(1), false),
            None,
            "Apply sends nothing"
        );
        assert!(!dialog.applying(slot(1)));

        // Reload discards the draft and follows the live slot.
        let mut reloaded = dialog.clone();
        reloaded.discard(slot(1));
        assert_eq!(reloaded.drafts[0], None);
        assert!(!reloaded.changed_elsewhere(slot(1), live.as_ref()));

        // Overwrite sends the draft, rebased onto the live slot.
        let Some(StudioIntent::SetDisplayOutput { config: sent, .. }) =
            dialog.apply(&theirs, slot(1), true)
        else {
            panic!("overwrite sends the draft");
        };
        assert_eq!(*sent, config(OutputScaling::Fill));
        assert_eq!(
            dialog.drafts[0]
                .as_ref()
                .and_then(|draft| draft.base.clone()),
            live
        );
        assert!(dialog.applying(slot(1)));
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
