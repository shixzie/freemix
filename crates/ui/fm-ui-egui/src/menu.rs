//! The application menu bar plus the Help windows.
//!
//! Every action item maps to an existing [`StudioIntent`] under that intent's
//! usual gate; gated items are disabled with the blocking reason as tooltip.
//! Output covers transitions, Fade to black, the manual T-bar, per-slot
//! recorders, per-target stream Start/Stop, and Stinger slot assignment.
//! View items only change local layout state.

use egui::{Button, RichText, Ui, Vec2};
use fm_protocol::{WireRecorderSlot, WireStingerSlotId};
use fm_types::InputId;

use crate::gating::{Capability, unavailable_reason};
use crate::input_bank::project_editing_available;
use crate::panels::SideTab;
use crate::stream_control::{stream_action_text, stream_unavailable_reason};
use crate::theme::{ACCENT, MUTED, TEXT, TEXT_DIM, mono, section_label};
use crate::{
    InputMoveDirection, ManualTransitionAvailability, RECORDER_SLOT_COUNT, RecordingAction,
    RenameDraft, STUDIO_SHORTCUTS, ShortcutGroup, StreamAction, StudioIntent, StudioShell,
    StudioUiState, TransitionGate, fade_to_black, preview_selection_available, recording_action,
    stinger_assign_intent, stinger_configuration_available, stinger_remove_intent, stream_action,
    transition_availability,
};

/// Which console regions are visible; stored in the shell between frames.
#[allow(clippy::struct_excessive_bools)] // each flag is one independent panel toggle
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsoleLayout {
    pub show_input_bank: bool,
    pub show_mixer: bool,
    pub show_side_panel: bool,
    pub show_status_bar: bool,
}

impl Default for ConsoleLayout {
    fn default() -> Self {
        Self {
            show_input_bank: true,
            show_mixer: true,
            show_side_panel: true,
            show_status_bar: true,
        }
    }
}

/// One menu entry: label, optional shortcut, enabled flag, disabled reason.
fn item(ui: &mut Ui, enabled: bool, label: &str, shortcut: Option<&str>, reason: &str) -> bool {
    let mut button = Button::new(label);
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(RichText::new(shortcut).color(MUTED));
    }
    ui.add_enabled(enabled, button)
        .on_disabled_hover_text(format!("Unavailable: {reason}"))
        .clicked()
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(section_label(text).size(9.5));
}

/// The Preview input the Edit menu acts on: the desired Preview selection.
fn preview_input(state: &StudioUiState) -> Option<(usize, InputId)> {
    let view = state.view.as_ref()?;
    let preview = view.switcher.desired.preview;
    view.inputs
        .iter()
        .position(|input| *input == preview)
        .map(|index| (index, preview))
}

impl StudioShell {
    pub(crate) fn draw_menu_bar(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        manual: ManualTransitionAvailability,
        intents: &mut Vec<StudioIntent>,
    ) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.spacing_mut().button_padding = Vec2::new(8.0, 2.0);
            ui.menu_button("File", |ui| self.file_menu(ui, state));
            ui.menu_button("Edit", |ui| self.edit_menu(ui, state, intents));
            ui.menu_button("View", |ui| self.view_menu(ui));
            ui.menu_button("Inputs", |ui| self.inputs_menu(ui, state, intents));
            ui.menu_button("Output", |ui| self.output_menu(ui, state, manual, intents));
            ui.menu_button("Help", |ui| self.help_menu(ui));
        });
    }

    #[allow(clippy::unused_self)] // keeps every menu a shell method
    fn file_menu(&mut self, ui: &mut Ui, state: &StudioUiState) {
        ui.set_min_width(240.0);
        match &state.view {
            Some(view) => {
                heading(ui, "Show");
                ui.label(RichText::new(&view.show_name).color(TEXT));
                ui.label(
                    mono(format!(
                        "Revision {} · {} inputs",
                        view.cursor.revision.get(),
                        view.inputs.len()
                    ))
                    .size(11.0)
                    .color(TEXT_DIM),
                );
            }
            None => {
                ui.label(RichText::new("No synchronized show").color(MUTED));
            }
        }
        ui.label(
            RichText::new("Shows are saved by the daemon after every accepted edit.")
                .size(10.5)
                .color(MUTED),
        );
        ui.separator();
        if ui
            .add(Button::new("Quit FreeMix Studio"))
            .on_hover_text("Close this window; a supervised daemon is shut down with it")
            .clicked()
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
        ui.set_min_width(260.0);
        let editing = project_editing_available(state);
        let reason = unavailable_reason(state, Capability::EditProject);
        let preview = preview_input(state);
        let count = state.view.as_ref().map_or(0, |view| view.inputs.len());
        heading(ui, "Preview input");
        let name = preview.and_then(|(index, _)| {
            state
                .view
                .as_ref()
                .map(|view| view.input_names[index].clone())
        });
        ui.label(
            RichText::new(name.clone().unwrap_or_else(|| "none".to_owned()))
                .color(if name.is_some() { TEXT } else { MUTED }),
        );
        let no_preview = "no input is selected on Preview";
        if item(
            ui,
            editing && preview.is_some(),
            "Rename…",
            None,
            if editing { no_preview } else { reason },
        ) && let Some((_, input)) = preview
            && let Some(name) = &name
        {
            self.layout.show_input_bank = true;
            self.rename_draft = Some(RenameDraft::new(input, name));
        }
        let earlier = preview.is_some_and(|(index, _)| index > 0);
        if item(
            ui,
            editing && earlier,
            "Move earlier",
            None,
            if editing {
                "the Preview input is already first"
            } else {
                reason
            },
        ) && let Some((_, input)) = preview
        {
            intents.push(StudioIntent::MoveInput {
                input,
                direction: InputMoveDirection::Up,
            });
        }
        let later = preview.is_some_and(|(index, _)| index + 1 < count);
        if item(
            ui,
            editing && later,
            "Move later",
            None,
            if editing {
                "the Preview input is already last"
            } else {
                reason
            },
        ) && let Some((_, input)) = preview
        {
            intents.push(StudioIntent::MoveInput {
                input,
                direction: InputMoveDirection::Down,
            });
        }
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(230.0);
        heading(ui, "Panels");
        ui.checkbox(&mut self.layout.show_input_bank, "Input bank")
            .on_hover_text("Show or hide the input bank");
        ui.checkbox(&mut self.layout.show_mixer, "Audio mixer")
            .on_hover_text("Show or hide the audio mixer");
        ui.checkbox(&mut self.layout.show_side_panel, "Side panel")
            .on_hover_text("Show or hide recording, streams, overlays, and stingers");
        ui.checkbox(&mut self.layout.show_status_bar, "Status bar")
            .on_hover_text("Show or hide the bottom status bar");
        ui.separator();
        heading(ui, "Side panel tab");
        ui.add_enabled_ui(self.layout.show_side_panel, |ui| {
            ui.radio_value(&mut self.side_tab, SideTab::Overlays, "Overlays");
            ui.radio_value(&mut self.side_tab, SideTab::Stingers, "Stingers");
        });
        ui.separator();
        if ui
            .add_enabled(
                self.layout != ConsoleLayout::default(),
                Button::new("Reset layout"),
            )
            .on_disabled_hover_text("Every panel is already shown")
            .clicked()
        {
            self.layout = ConsoleLayout::default();
        }
    }

    #[allow(clippy::unused_self)] // keeps every menu a shell method
    fn inputs_menu(&mut self, ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
        ui.set_min_width(260.0);
        let gate = TransitionGate::from_state(state);
        let preview = preview_selection_available(gate, state.can_select_preview);
        let cut = preview && transition_availability(gate).basic();
        let preview_reason = unavailable_reason(state, Capability::SelectPreview);
        let cut_reason = if preview {
            unavailable_reason(state, Capability::Transition)
        } else {
            preview_reason
        };
        let Some(view) = &state.view else {
            ui.add_enabled(false, Button::new("No inputs"))
                .on_disabled_hover_text(format!("Unavailable: {preview_reason}"));
            return;
        };
        heading(ui, "Select on Preview");
        for (index, (input, name)) in view.inputs.iter().zip(&view.input_names).enumerate() {
            let shortcut = (index < 8).then(|| format!("{}", index + 1));
            let label = format!("{:>2}  {name}", index + 1);
            if item(ui, preview, &label, shortcut.as_deref(), preview_reason) {
                intents.push(StudioIntent::SelectPreview(*input));
            }
        }
        ui.separator();
        ui.add_enabled_ui(cut, |ui| {
            ui.menu_button("Cut directly to Program", |ui| {
                for (index, (input, name)) in view.inputs.iter().zip(&view.input_names).enumerate()
                {
                    if item(
                        ui,
                        cut,
                        &format!("{:>2}  {name}", index + 1),
                        None,
                        cut_reason,
                    ) {
                        intents.push(StudioIntent::SelectPreview(*input));
                        intents.push(StudioIntent::Cut);
                    }
                }
            })
        })
        .response
        .on_disabled_hover_text(format!("Unavailable: {cut_reason}"));
    }

    #[allow(clippy::too_many_lines)] // one menu, one immediate-mode region
    fn output_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        manual: ManualTransitionAvailability,
        intents: &mut Vec<StudioIntent>,
    ) {
        ui.set_min_width(270.0);
        let availability = transition_availability(TransitionGate::from_state(state));
        let reason = unavailable_reason(state, Capability::Transition);
        let duration_frames = self.transition_duration_frames;
        heading(ui, &format!("Transition · {duration_frames} fr"));
        let transitions: [(&str, Option<&str>, bool, StudioIntent); 6] = [
            ("Cut", Some("C"), availability.basic(), StudioIntent::Cut),
            (
                "Fade",
                Some("F"),
                availability.basic(),
                StudioIntent::Fade { duration_frames },
            ),
            (
                "Wipe",
                None,
                availability.wipe,
                StudioIntent::Wipe { duration_frames },
            ),
            (
                "AlphaFade",
                None,
                availability.alpha_fade,
                StudioIntent::AlphaFade { duration_frames },
            ),
            (
                "Slide",
                None,
                availability.slide(),
                StudioIntent::Slide { duration_frames },
            ),
            (
                "Zoom",
                None,
                availability.zoom(),
                StudioIntent::Zoom { duration_frames },
            ),
        ];
        for (label, shortcut, enabled, intent) in transitions {
            if item(ui, enabled, label, shortcut, reason) {
                intents.push(intent);
            }
        }
        ui.separator();
        heading(ui, "Fade to black");
        let desired = state
            .view
            .as_ref()
            .map(|view| view.switcher.desired_fade_to_black);
        let ftb = fade_to_black::fade_to_black_availability(
            fade_to_black::FadeToBlackGate::from_state(state),
            desired,
        );
        let in_flight = TransitionGate::from_state(state).manual_transition_in_flight;
        let black = desired.is_some_and(|desired| desired.target_active);
        let (label, enabled) = if black {
            ("Return to live", ftb.to_live && !in_flight)
        } else {
            ("Fade to black", ftb.to_black && !in_flight)
        };
        if item(ui, enabled, label, Some("B"), reason) {
            intents.push(StudioIntent::FadeToBlack {
                active: !black,
                duration_frames: self.fade_to_black_duration_frames,
            });
        }
        ui.separator();
        heading(ui, "Manual T-bar");
        let session_ok = state.connection_status.controls_enabled()
            && state.view.is_some()
            && state.can_transition;
        let manual_reason = if session_ok {
            "no manual transition is active"
        } else {
            reason
        };
        if item(
            ui,
            manual.active_controls,
            "Commit transition",
            None,
            manual_reason,
        ) {
            intents.push(StudioIntent::CommitManualTransition);
        }
        if item(
            ui,
            manual.active_controls,
            "Cancel transition",
            Some("Esc"),
            manual_reason,
        ) {
            intents.push(StudioIntent::CancelManualTransition);
        }
        ui.separator();
        heading(ui, "Recording");
        for (index, desired) in state.recording_desired.iter().copied().enumerate() {
            let Some(recorder) = u8::try_from(index + 1).ok().and_then(WireRecorderSlot::new)
            else {
                continue;
            };
            let requested = if desired {
                RecordingAction::Stop(recorder)
            } else {
                RecordingAction::Start(recorder)
            };
            let allowed = recording_action(state, requested);
            let label = format!(
                "{} recorder {}",
                if desired { "Stop" } else { "Start" },
                index + 1
            );
            let slot_reason = if !session_ok {
                reason
            } else if index < RECORDER_SLOT_COUNT && state.recording_pending[index] {
                "an earlier recording command is still pending"
            } else {
                "the current state does not allow it"
            };
            if item(ui, allowed.is_some(), &label, None, slot_reason)
                && let Some(action) = allowed
            {
                intents.push(StudioIntent::Recording(action));
            }
        }
        ui.separator();
        heading(ui, "Streams");
        if state.streams.is_empty() {
            ui.add_enabled(false, Button::new("No stream targets"))
                .on_disabled_hover_text(if state.view.is_some() {
                    "Unavailable: the show has no configured stream targets"
                } else {
                    "Unavailable: project state is not synchronized yet"
                });
        }
        for stream in &state.streams {
            let requested = StreamAction::for_stream(stream);
            let allowed = stream_action(state, requested);
            let label = format!(
                "{} {}",
                if requested.starts() { "Start" } else { "Stop" },
                stream.name
            );
            if ui
                .add_enabled(allowed.is_some(), Button::new(label))
                .on_hover_text(stream_action_text(requested, &stream.name))
                .on_disabled_hover_text(format!(
                    "Unavailable: {}",
                    stream_unavailable_reason(state, stream)
                ))
                .clicked()
                && let Some(action) = allowed
            {
                intents.push(StudioIntent::Stream(action));
            }
        }
        ui.separator();
        heading(ui, "Stingers");
        self.stinger_menu(ui, state, reason, intents);
    }

    /// Output ▸ Stingers: assign the Preview input to a slot, or clear a slot.
    fn stinger_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        reason: &str,
        intents: &mut Vec<StudioIntent>,
    ) {
        let available = stinger_configuration_available(state);
        let preview = preview_input(state).map(|(_, input)| input);
        let assign_reason = if available {
            "no input is selected on Preview"
        } else {
            reason
        };
        let duration_frames = self.transition_duration_frames;
        ui.add_enabled_ui(available && preview.is_some(), |ui| {
            ui.menu_button("Assign Preview as stinger media", |ui| {
                for number in 1..=8_u8 {
                    let slot =
                        WireStingerSlotId::new(number).expect("Studio lists slots 1 through 8");
                    let intent = preview.and_then(|preview| {
                        stinger_assign_intent(state, slot, preview, duration_frames)
                    });
                    if item(
                        ui,
                        intent.is_some(),
                        &format!("Slot S{number}"),
                        None,
                        assign_reason,
                    ) && let Some(intent) = intent
                    {
                        intents.push(intent);
                    }
                }
            })
        })
        .response
        .on_hover_text("Use the Preview input as a slot's media; other slot settings are kept")
        .on_disabled_hover_text(format!("Unavailable: {assign_reason}"));
        let configured = state
            .view
            .as_ref()
            .is_some_and(|view| !view.stingers.is_empty());
        let clear_reason = if available {
            "no Stinger slot is configured"
        } else {
            reason
        };
        ui.add_enabled_ui(available && configured, |ui| {
            ui.menu_button("Clear stinger slot", |ui| {
                for number in 1..=8_u8 {
                    let slot =
                        WireStingerSlotId::new(number).expect("Studio lists slots 1 through 8");
                    let intent = stinger_remove_intent(state, slot);
                    if item(
                        ui,
                        intent.is_some(),
                        &format!("Slot S{number}"),
                        None,
                        "this Stinger slot is not configured",
                    ) && let Some(intent) = intent
                    {
                        intents.push(intent);
                    }
                }
            })
        })
        .response
        .on_disabled_hover_text(format!("Unavailable: {clear_reason}"));
        if ui
            .add_enabled(self.layout.show_side_panel, Button::new("Stinger slots…"))
            .on_hover_text("Open the Stingers tab to pick any input as slot media")
            .on_disabled_hover_text("Unavailable: the side panel is hidden")
            .clicked()
        {
            self.side_tab = SideTab::Stingers;
        }
    }

    fn help_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(220.0);
        if ui.button("Keyboard shortcuts").clicked() {
            self.show_shortcuts = true;
        }
        if ui.button("About FreeMix Studio").clicked() {
            self.show_about = true;
        }
    }

    /// Draws the Help dialogs when open.
    pub(crate) fn draw_help_windows(&mut self, context: &egui::Context) {
        if self.show_shortcuts {
            let response = help_modal(
                context,
                "studio-shortcuts-window",
                "Keyboard shortcuts",
                470.0,
                |ui| {
                    ui.label(
                    RichText::new(
                        "Keys act only while the console is ready, no text field has focus, and the session holds the matching permission.",
                    )
                    .size(11.0)
                    .color(TEXT_DIM),
                );
                    for group in [
                        ShortcutGroup::Switching,
                        ShortcutGroup::Transitions,
                        ShortcutGroup::InputBank,
                    ] {
                        ui.add_space(8.0);
                        ui.label(section_label(group.title()));
                        ui.add_space(2.0);
                        egui::Grid::new(("studio-shortcuts", group.title()))
                            .num_columns(2)
                            .spacing(Vec2::new(18.0, 6.0))
                            .min_col_width(120.0)
                            .show(ui, |ui| {
                                for entry in
                                    STUDIO_SHORTCUTS.iter().filter(|entry| entry.group == group)
                                {
                                    key_cap(ui, entry.keys);
                                    ui.label(RichText::new(entry.action).color(TEXT));
                                    ui.end_row();
                                }
                            });
                    }
                },
            );
            if response {
                self.show_shortcuts = false;
            }
        }
        if self.show_about {
            let response = help_modal(context, "studio-about-window", "About", 400.0, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("FREEMIX")
                            .size(20.0)
                            .strong()
                            .extra_letter_spacing(3.0)
                            .color(ACCENT),
                    );
                    ui.label(
                        RichText::new("STUDIO")
                            .size(14.0)
                            .extra_letter_spacing(2.0)
                            .color(MUTED),
                    );
                });
                ui.label(mono(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(TEXT_DIM));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Native operator console for the freemixd live production engine. The console renders replicated control state and sends commands; the daemon owns media, persistence, and outputs.",
                    )
                    .color(TEXT),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("License: {}", env!("CARGO_PKG_LICENSE")))
                        .size(11.0)
                        .color(MUTED),
                );
            });
            if response {
                self.show_about = false;
            }
        }
    }
}

/// A centered modal dialog with a title row and close key; returns `true`
/// when the operator closed it (close key, Escape, or a click outside).
fn help_modal(
    context: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut Ui),
) -> bool {
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new(id))
        .backdrop_color(egui::Color32::from_black_alpha(150))
        .frame(
            egui::Frame::new()
                .fill(crate::theme::GRAPHITE_RAISED)
                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_STRONG))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(context, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Button::new(RichText::new("×").color(TEXT_DIM)).frame(false))
                        .on_hover_text("Close  [Esc]")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.separator();
            add_contents(ui);
        });
    close || response.should_close()
}

/// A key-cap styled label such as `C` or `1 – 8`.
fn key_cap(ui: &mut Ui, keys: &str) {
    egui::Frame::new()
        .fill(crate::theme::WIDGET)
        .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_STRONG))
        .corner_radius(egui::CornerRadius::same(3))
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(mono(keys).size(11.5).color(TEXT));
        });
}
