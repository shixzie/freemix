//! The application menu bar plus the Help windows.
//!
//! Every action item maps to an existing [`StudioIntent`] under that intent's
//! usual gate; gated items are disabled with the blocking reason as tooltip.
//! Output covers transitions, Fade to black, the manual T-bar, per-slot
//! recorders, per-target stream Start/Stop, and Stinger slot assignment.
//! Inputs also opens the Add input dialog and the Remove confirmation.
//! View items only change local layout state.

use egui::{Button, RichText, Ui, Vec2};
use fm_protocol::{WireRecorderSlot, WireStingerSlotId, WireStreamTargetId};
use fm_types::InputId;

use crate::gating::{Capability, unavailable_reason};
use crate::input_bank::{neighbour_move_lock_reason, project_editing_available};
use crate::l10n::{self, Arg, Msg};
use crate::outputs::{recorder_row, stream_row};
use crate::panels::SideTab;
use crate::panels::{
    latest_stream_sample, stinger_slot_presentation, stinger_slot_state, stinger_slot_tone,
};
use crate::stream_control::{stream_action_text, stream_unavailable_reason};
use crate::theme::{
    ACCENT, MUTED, TEXT, TEXT_DIM, Tone, ellipsize, mono, paint_lamp, section_label,
};
use crate::{
    HistoryVerb, REDO_SHORTCUT, UNDO_SHORTCUT, edit_history_availability, input_edit_lock_reason,
    padlock_toggle,
};
use crate::{
    InputMoveDirection, ManualTransitionAvailability, RECORDER_SLOT_COUNT, RecordingAction,
    RenameDraft, STUDIO_SHORTCUTS, ShortcutGroup, StreamAction, StudioIntent, StudioShell,
    StudioUiState, TransitionGate, fade_to_black, preview_selection_available, recording_action,
    stinger_assign_intent, stinger_remove_intent, stinger_setup_block_reason, stream_action,
    stream_command_pending, transition_availability,
};
use fm_ui_model::i18n::{HourCycle, Language, Locale};

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
    let l = l10n::locale(ui.ctx());
    ui.add_enabled(enabled, button)
        .on_disabled_hover_text(l10n::unavailable(l, reason))
        .clicked()
}

/// A menu entry whose right column shows the target's live status in its
/// tone colour instead of a shortcut (for example `RECORDING` in red).
fn status_item(
    ui: &mut Ui,
    enabled: bool,
    label: &str,
    (status, tone): (&str, Tone),
    hover: &str,
    reason: &str,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let button = Button::new(label).shortcut_text(
        RichText::new(l.tr(status))
            .size(10.0)
            .strong()
            .color(tone.color()),
    );
    ui.add_enabled(enabled, button)
        .on_hover_text(hover)
        .on_disabled_hover_text(l10n::disabled_text(l, hover, reason))
        .clicked()
}

/// Replicated readiness label and tone of one Stinger slot.
fn slot_status(state: &StudioUiState, slot: u8) -> (&'static str, Tone) {
    let slot_state = stinger_slot_state(
        state.view.as_ref().map(|view| view.stingers.as_slice()),
        slot,
    );
    (
        stinger_slot_presentation(slot_state).label,
        stinger_slot_tone(slot_state),
    )
}

/// Longest menu label, in characters, before it ends in an ellipsis.
const MENU_LABEL_CHARS: usize = 32;

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
            let l = self.locale;
            ui.menu_button(l.text(Msg::MenuFile), |ui| self.file_menu(ui, state));
            ui.menu_button(l.text(Msg::MenuEdit), |ui| {
                self.edit_menu(ui, state, intents);
            });
            ui.menu_button(l.text(Msg::MenuView), |ui| self.view_menu(ui));
            ui.menu_button(l.text(Msg::MenuInputs), |ui| {
                self.inputs_menu(ui, state, intents);
            });
            ui.menu_button(l.text(Msg::MenuOutput), |ui| {
                self.output_menu(ui, state, manual, intents);
            });
            ui.menu_button(l.text(Msg::MenuHelp), |ui| self.help_menu(ui));
        });
    }

    #[allow(clippy::unused_self)] // keeps every menu a shell method
    fn file_menu(&mut self, ui: &mut Ui, state: &StudioUiState) {
        let l = self.locale;
        ui.set_min_width(240.0);
        match &state.view {
            Some(view) => {
                heading(ui, l.text(Msg::FileShowHeading));
                ui.label(RichText::new(ellipsize(&view.show_name, MENU_LABEL_CHARS)).color(TEXT))
                    .on_hover_text(&view.show_name);
                ui.label(
                    mono(l.format(
                        Msg::FileRevisionInputs,
                        &[
                            ("revision", Arg::Plain(view.cursor.revision.get())),
                            ("count", Arg::from(view.inputs.len())),
                        ],
                    ))
                    .size(11.0)
                    .color(TEXT_DIM),
                );
            }
            None => {
                ui.label(RichText::new(l.text(Msg::NoSynchronizedShow)).color(MUTED));
            }
        }
        ui.label(
            RichText::new(l.text(Msg::FileSavedByDaemon))
                .size(10.5)
                .color(MUTED),
        );
        ui.separator();
        if ui
            .add(Button::new(l.text(Msg::FileQuit)))
            .on_hover_text(l.text(Msg::FileQuitHover))
            .clicked()
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
        ui.set_min_width(260.0);
        history_menu_items(ui, state, intents);
        ui.separator();
        self.preview_input_menu_items(ui, state, intents);
    }

    fn preview_input_menu_items(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = self.locale;
        let preview = preview_input(state);
        let lock_reason = preview.and_then(|(_, input)| input_edit_lock_reason(state, input));
        let editing = project_editing_available(state) && lock_reason.is_none();
        let reason = if project_editing_available(state) {
            lock_reason.unwrap_or("")
        } else {
            unavailable_reason(state, Capability::EditProject)
        };
        let count = state.view.as_ref().map_or(0, |view| view.inputs.len());
        heading(ui, l.text(Msg::EditPreviewInputHeading));
        let name = preview.and_then(|(index, _)| {
            state
                .view
                .as_ref()
                .map(|view| view.input_names[index].clone())
        });
        ui.label(
            RichText::new(name.as_deref().map_or_else(
                || l.text(Msg::None).to_owned(),
                |name| ellipsize(name, MENU_LABEL_CHARS),
            ))
            .color(if name.is_some() { TEXT } else { MUTED }),
        );
        let no_preview = Msg::ReasonNoPreviewInput.en();
        let locked = lock_reason.is_some();
        if lockable_item(
            ui,
            (editing && preview.is_some(), locked),
            l.text(Msg::RenameEllipsis),
            None,
            None,
            if editing { no_preview } else { reason },
        ) && let Some((_, input)) = preview
            && let Some(name) = &name
        {
            self.layout.show_input_bank = true;
            self.rename_draft = Some(RenameDraft::new(input, name));
        }
        let neighbour_lock = |direction| {
            state
                .view
                .as_ref()
                .zip(preview)
                .and_then(|(view, (index, _))| {
                    neighbour_move_lock_reason(&view.inputs, &view.edit_locks, index, direction)
                })
        };
        let earlier_lock = neighbour_lock(InputMoveDirection::Up);
        let later_lock = neighbour_lock(InputMoveDirection::Down);
        let earlier = preview.is_some_and(|(index, _)| index > 0);
        if lockable_item(
            ui,
            (
                editing && earlier && earlier_lock.is_none(),
                locked || earlier_lock.is_some(),
            ),
            l.text(Msg::MoveEarlier),
            None,
            None,
            if !editing {
                reason
            } else if earlier {
                earlier_lock.unwrap_or("")
            } else {
                Msg::ReasonPreviewFirst.en()
            },
        ) && let Some((_, input)) = preview
        {
            intents.push(StudioIntent::MoveInput {
                input,
                direction: InputMoveDirection::Up,
            });
        }
        let later = preview.is_some_and(|(index, _)| index + 1 < count);
        if lockable_item(
            ui,
            (
                editing && later && later_lock.is_none(),
                locked || later_lock.is_some(),
            ),
            l.text(Msg::MoveLater),
            None,
            None,
            if !editing {
                reason
            } else if later {
                later_lock.unwrap_or("")
            } else {
                Msg::ReasonPreviewLast.en()
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
        let l = self.locale;
        ui.set_min_width(230.0);
        heading(ui, l.text(Msg::ViewPanels));
        ui.checkbox(&mut self.layout.show_input_bank, l.text(Msg::InputBank))
            .on_hover_text(l.text(Msg::ViewInputBankHover));
        ui.checkbox(&mut self.layout.show_mixer, l.text(Msg::ViewAudioMixer))
            .on_hover_text(l.text(Msg::ViewAudioMixerHover));
        ui.checkbox(&mut self.layout.show_side_panel, l.text(Msg::ViewSidePanel))
            .on_hover_text(l.text(Msg::ViewSidePanelHover));
        ui.checkbox(&mut self.layout.show_status_bar, l.text(Msg::ViewStatusBar))
            .on_hover_text(l.text(Msg::ViewStatusBarHover));
        ui.separator();
        heading(ui, l.text(Msg::ViewSidePanelTab));
        ui.add_enabled_ui(self.layout.show_side_panel, |ui| {
            ui.radio_value(
                &mut self.side_tab,
                SideTab::Overlays,
                l.text(Msg::TabOverlays),
            );
            ui.radio_value(
                &mut self.side_tab,
                SideTab::Stingers,
                l.text(Msg::TabStingers),
            );
        });
        ui.separator();
        if ui
            .add_enabled(
                self.layout != ConsoleLayout::default(),
                Button::new(l.text(Msg::ViewResetLayout)),
            )
            .on_disabled_hover_text(l.text(Msg::ViewResetLayoutDisabled))
            .clicked()
        {
            self.layout = ConsoleLayout::default();
        }
        ui.separator();
        self.language_menu_items(ui);
    }

    /// View › Language and clock: switches the console's language and hour
    /// cycle at once. Local to this console; nothing is sent to the daemon.
    fn language_menu_items(&mut self, ui: &mut Ui) {
        let l = self.locale;
        heading(ui, l.text(Msg::ViewLanguage));
        for language in Language::ALL {
            // Each language is named in itself, so an operator who cannot
            // read the current one still finds theirs.
            if ui
                .radio(self.locale.language == language, language.native_name())
                .on_hover_text(l.text(Msg::ViewLanguageHover))
                .clicked()
            {
                self.locale = Locale {
                    language,
                    ..self.locale
                };
            }
        }
        let mut twelve = self.locale.hour_cycle == HourCycle::H12;
        if ui
            .checkbox(&mut twelve, l.text(Msg::ViewClock12h))
            .on_hover_text(l.text(Msg::ViewClock12hHover))
            .changed()
        {
            self.locale = self.locale.with_hour_cycle(if twelve {
                HourCycle::H12
            } else {
                HourCycle::H23
            });
        }
    }

    fn inputs_menu(&mut self, ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
        let l = self.locale;
        ui.set_min_width(260.0);
        self.catalogue_menu_items(ui, state);
        ui.separator();
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
            ui.add_enabled(false, Button::new(l.text(Msg::NoInputs)))
                .on_disabled_hover_text(l10n::unavailable(l, preview_reason));
            return;
        };
        heading(ui, l.text(Msg::SelectOnPreview));
        for (index, (input, name)) in view.inputs.iter().zip(&view.input_names).enumerate() {
            let shortcut = (index < 8).then(|| format!("{}", index + 1));
            let label = format!("{:>2}  {}", index + 1, ellipsize(name, MENU_LABEL_CHARS));
            if item(ui, preview, &label, shortcut.as_deref(), preview_reason) {
                intents.push(StudioIntent::SelectPreview(*input));
            }
        }
        ui.separator();
        ui.add_enabled_ui(cut, |ui| {
            ui.menu_button(l.text(Msg::CutDirectlyToProgram), |ui| {
                for (index, (input, name)) in view.inputs.iter().zip(&view.input_names).enumerate()
                {
                    if item(
                        ui,
                        cut,
                        &format!("{:>2}  {}", index + 1, ellipsize(name, MENU_LABEL_CHARS)),
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
        .on_disabled_hover_text(l10n::unavailable(l, cut_reason));
    }

    /// Inputs › Add input… and the Remove input submenu, whose right column
    /// says why an input cannot be removed (`PGM`, `PVW`, `LOCKED`, `IN USE`).
    fn catalogue_menu_items(&mut self, ui: &mut Ui, state: &StudioUiState) {
        let l = self.locale;
        heading(ui, l.text(Msg::CatalogueHeading));
        let add = self.add_input_gate(state);
        if lockable_item(
            ui,
            (add.enabled, add.locked()),
            l.text(Msg::AddInputEllipsis),
            None,
            Some(l.text(Msg::AddInputMenuHover)),
            add.reason,
        ) {
            self.open_add_input(state);
        }
        let editing = project_editing_available(state);
        let reason = if editing {
            Msg::ReasonShowHasNoInputs.en()
        } else {
            unavailable_reason(state, Capability::EditProject)
        };
        let inputs: Vec<(InputId, String)> = state.view.as_ref().map_or_else(Vec::new, |view| {
            view.inputs
                .iter()
                .copied()
                .zip(view.input_names.iter().cloned())
                .collect()
        });
        let mut open = None;
        ui.add_enabled_ui(editing && !inputs.is_empty(), |ui| {
            ui.menu_button(l.text(Msg::RemoveInput), |ui| {
                ui.set_min_width(260.0);
                for (index, (input, name)) in inputs.iter().enumerate() {
                    let gate = self.remove_input_gate(state, *input);
                    let label = format!("{:>2}  {}", index + 1, ellipsize(name, MENU_LABEL_CHARS));
                    let tone = match gate.mark {
                        Some("PGM") => Tone::OnAir,
                        Some("PVW") => Tone::Ready,
                        Some(_) => Tone::Pending,
                        None => Tone::Idle,
                    };
                    if status_item(
                        ui,
                        gate.enabled,
                        &label,
                        (gate.mark.unwrap_or(""), tone),
                        &l.format(Msg::RemoveInputHover, &[("name", Arg::Str(name))]),
                        gate.reason,
                    ) {
                        open = Some((*input, name.clone()));
                    }
                }
            })
        })
        .response
        .on_hover_text(l.text(Msg::RemoveInputMenuHover))
        .on_disabled_hover_text(l10n::unavailable(l, reason));
        if let Some((input, name)) = open {
            self.open_remove_input(input, &name);
        }
    }

    #[allow(clippy::too_many_lines)] // one menu, one immediate-mode region
    fn output_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        manual: ManualTransitionAvailability,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = self.locale;
        ui.set_min_width(270.0);
        let availability = transition_availability(TransitionGate::from_state(state));
        let reason = unavailable_reason(state, Capability::Transition);
        let duration_frames = self.transition_duration_frames;
        heading(
            ui,
            &l.format(
                Msg::TransitionHeading,
                &[("frames", Arg::from(duration_frames))],
            ),
        );
        let transitions: [(&str, Option<&str>, bool, StudioIntent); 6] = [
            (
                l.text(Msg::TransitionCut),
                Some("C"),
                availability.basic(),
                StudioIntent::Cut,
            ),
            (
                l.text(Msg::TransitionFade),
                Some("F"),
                availability.basic(),
                StudioIntent::Fade { duration_frames },
            ),
            (
                l.text(Msg::TransitionWipe),
                None,
                availability.wipe,
                StudioIntent::Wipe { duration_frames },
            ),
            (
                l.text(Msg::TransitionAlphaFade),
                None,
                availability.alpha_fade,
                StudioIntent::AlphaFade { duration_frames },
            ),
            (
                l.text(Msg::TransitionSlide),
                None,
                availability.slide(),
                StudioIntent::Slide { duration_frames },
            ),
            (
                l.text(Msg::TransitionZoom),
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
        heading(ui, l.text(Msg::FadeToBlack));
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
            (l.text(Msg::ReturnToLive), ftb.to_live && !in_flight)
        } else {
            (l.text(Msg::FadeToBlack), ftb.to_black && !in_flight)
        };
        if item(ui, enabled, label, Some("B"), reason) {
            intents.push(StudioIntent::FadeToBlack {
                active: !black,
                duration_frames: self.fade_to_black_duration_frames,
            });
        }
        ui.separator();
        heading(ui, l.text(Msg::ManualTBarHeading));
        let session_ok = state.connection_status.controls_enabled()
            && state.view.is_some()
            && state.can_transition;
        let manual_reason = if session_ok {
            Msg::ReasonNoManualActive.en()
        } else {
            reason
        };
        if item(
            ui,
            manual.active_controls,
            l.text(Msg::CommitTransition),
            None,
            manual_reason,
        ) {
            intents.push(StudioIntent::CommitManualTransition);
        }
        if item(
            ui,
            manual.active_controls,
            l.text(Msg::CancelTransition),
            Some("Esc"),
            manual_reason,
        ) {
            intents.push(StudioIntent::CancelManualTransition);
        }
        ui.separator();
        heading(ui, l.text(Msg::RecordingHeading));
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
            let label = l.format(
                if desired {
                    Msg::StopRecorder
                } else {
                    Msg::StartRecorder
                },
                &[("slot", Arg::from(index + 1))],
            );
            let pending = index < RECORDER_SLOT_COUNT && state.recording_pending[index];
            let slot_reason = if !session_ok {
                reason
            } else if pending {
                Msg::ReasonRecordingPending.en()
            } else {
                Msg::ReasonStateDisallows.en()
            };
            let row = recorder_row(l, state.view.is_some(), desired, pending);
            if status_item(
                ui,
                allowed.is_some(),
                &label,
                (row.status, row.tone),
                &l.format(
                    Msg::RecorderHover,
                    &[("label", Arg::Str(&label)), ("slot", Arg::from(index + 1))],
                ),
                slot_reason,
            ) && let Some(action) = allowed
            {
                intents.push(StudioIntent::Recording(action));
            }
        }
        ui.separator();
        heading(ui, l.text(Msg::StreamsHeading));
        if state.streams.is_empty() {
            ui.add_enabled(false, Button::new(l.text(Msg::NoStreamTargets)))
                .on_disabled_hover_text(l10n::unavailable(
                    l,
                    if state.view.is_some() {
                        Msg::ReasonNoStreamTargets.en()
                    } else {
                        Msg::ReasonNotSynchronized.en()
                    },
                ));
        }
        for stream in &state.streams {
            let requested = StreamAction::for_stream(stream);
            let allowed = stream_action(state, requested);
            let label = l.format(
                if requested.starts() {
                    Msg::StartStream
                } else {
                    Msg::StopStream
                },
                &[("name", Arg::Str(&ellipsize(&stream.name, MENU_LABEL_CHARS)))],
            );
            let row = stream_row(
                l,
                stream,
                latest_stream_sample(state.stream_status.as_ref(), stream.target),
                stream_command_pending(state, WireStreamTargetId::new(stream.target)),
            );
            if status_item(
                ui,
                allowed.is_some(),
                &label,
                (row.status, row.tone),
                &stream_action_text(requested, &stream.name),
                stream_unavailable_reason(state, stream),
            ) && let Some(action) = allowed
            {
                intents.push(StudioIntent::Stream(action));
            }
        }
        ui.separator();
        heading(ui, l.text(Msg::TabStingers));
        self.stinger_menu(ui, state, intents);
    }

    /// Output ▸ Stingers: assign the Preview input to a slot, or clear a slot.
    fn stinger_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = self.locale;
        let blocked = stinger_setup_block_reason(state);
        let available = blocked.is_none();
        let preview = preview_input(state).map(|(_, input)| input);
        let assign_reason = blocked.unwrap_or(Msg::ReasonNoPreviewInput.en());
        let duration_frames = self.transition_duration_frames;
        ui.add_enabled_ui(available && preview.is_some(), |ui| {
            ui.menu_button(l.text(Msg::AssignStingerMedia), |ui| {
                for number in 1..=8_u8 {
                    let slot =
                        WireStingerSlotId::new(number).expect("Studio lists slots 1 through 8");
                    let intent = preview.and_then(|preview| {
                        stinger_assign_intent(state, slot, preview, duration_frames)
                    });
                    if status_item(
                        ui,
                        intent.is_some(),
                        &l.format(Msg::StingerSlot, &[("slot", Arg::Plain(number.into()))]),
                        slot_status(state, number),
                        &l.format(
                            Msg::AssignStingerSlotHover,
                            &[("slot", Arg::Plain(number.into()))],
                        ),
                        assign_reason,
                    ) && let Some(intent) = intent
                    {
                        intents.push(intent);
                    }
                }
            })
        })
        .response
        .on_hover_text(l.text(Msg::AssignStingerMediaHover))
        .on_disabled_hover_text(l10n::unavailable(l, assign_reason));
        let configured = state
            .view
            .as_ref()
            .is_some_and(|view| !view.stingers.is_empty());
        let clear_reason = blocked.unwrap_or(Msg::ReasonNoStingerConfigured.en());
        ui.add_enabled_ui(available && configured, |ui| {
            ui.menu_button(l.text(Msg::ClearStingerSlot), |ui| {
                for number in 1..=8_u8 {
                    let slot =
                        WireStingerSlotId::new(number).expect("Studio lists slots 1 through 8");
                    let intent = stinger_remove_intent(state, slot);
                    if status_item(
                        ui,
                        intent.is_some(),
                        &l.format(Msg::StingerSlot, &[("slot", Arg::Plain(number.into()))]),
                        slot_status(state, number),
                        &l.format(
                            Msg::ClearStingerSlotHover,
                            &[("slot", Arg::Plain(number.into()))],
                        ),
                        Msg::ReasonStingerSlotNotConfigured.en(),
                    ) && let Some(intent) = intent
                    {
                        intents.push(intent);
                    }
                }
            })
        })
        .response
        .on_disabled_hover_text(l10n::unavailable(l, clear_reason));
        if ui
            .add_enabled(
                self.layout.show_side_panel,
                Button::new(l.text(Msg::StingerSlotsEllipsis)),
            )
            .on_hover_text(l.text(Msg::StingerSlotsHover))
            .on_disabled_hover_text(l10n::unavailable(l, Msg::ReasonSidePanelHidden.en()))
            .clicked()
        {
            self.side_tab = SideTab::Stingers;
        }
    }

    fn help_menu(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.set_min_width(220.0);
        if ui.button(l.text(Msg::KeyboardShortcuts)).clicked() {
            self.show_shortcuts = true;
        }
        if ui.button(l.text(Msg::AboutStudio)).clicked() {
            self.show_about = true;
        }
    }

    /// Draws the Help dialogs when open.
    pub(crate) fn draw_help_windows(&mut self, context: &egui::Context, state: &StudioUiState) {
        let l = self.locale;
        if self.show_shortcuts {
            let (status, tone) = shortcut_status(l, state);
            let response = help_modal(
                context,
                "studio-shortcuts-window",
                l.text(Msg::KeyboardShortcuts),
                500.0,
                |ui| {
                    ui.horizontal(|ui| {
                        let (lamp, _) =
                            ui.allocate_exact_size(Vec2::new(12.0, 12.0), egui::Sense::hover());
                        paint_lamp(ui.painter(), lamp.center(), 4.0, tone);
                        ui.label(
                            RichText::new(status)
                                .size(11.5)
                                .strong()
                                .color(tone.color()),
                        );
                    });
                    ui.label(
                        RichText::new(l.text(Msg::ShortcutsIntro))
                            .size(11.0)
                            .color(TEXT_DIM),
                    );
                    for group in [
                        ShortcutGroup::Switching,
                        ShortcutGroup::Transitions,
                        ShortcutGroup::Editing,
                        ShortcutGroup::InputBank,
                    ] {
                        ui.add_space(10.0);
                        ui.label(section_label(l.tr(group.title())));
                        ui.add_space(2.0);
                        egui::Grid::new(("studio-shortcuts", group.title()))
                            .num_columns(2)
                            .spacing(Vec2::new(16.0, 6.0))
                            .show(ui, |ui| {
                                for entry in
                                    STUDIO_SHORTCUTS.iter().filter(|entry| entry.group == group)
                                {
                                    key_cap(ui, l.tr(entry.keys));
                                    ui.label(RichText::new(l.tr(entry.action)).color(TEXT));
                                    ui.end_row();
                                }
                            });
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label(
                        RichText::new(l.text(Msg::ShortcutsFooter))
                            .size(10.5)
                            .color(MUTED),
                    );
                },
            );
            if response {
                self.show_shortcuts = false;
            }
        }
        self.draw_about_window(context);
    }

    fn draw_about_window(&mut self, context: &egui::Context) {
        let l = self.locale;
        if self.show_about {
            let response = help_modal(
                context,
                "studio-about-window",
                l.text(Msg::About),
                400.0,
                |ui| {
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
                    ui.label(
                        mono(l.format(
                            Msg::AboutVersion,
                            &[("version", Arg::Str(env!("CARGO_PKG_VERSION")))],
                        ))
                        .color(TEXT_DIM),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new(l.text(Msg::AboutDescription)).color(TEXT));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(l.format(
                            Msg::AboutLicense,
                            &[("license", Arg::Str(env!("CARGO_PKG_LICENSE")))],
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                },
            );
            if response {
                self.show_about = false;
            }
        }
    }
}

/// A centered modal dialog with a title row and close key; returns `true`
/// when the operator closed it (close key, Escape, or a click outside).
pub(crate) fn help_modal(
    context: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut Ui),
) -> bool {
    let l = l10n::locale(context);
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
                ui.with_layout(l10n::trailing_layout(l), |ui| {
                    if ui
                        .add(Button::new(RichText::new("×").color(TEXT_DIM)).frame(false))
                        .on_hover_text(l.text(Msg::CloseEsc))
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

/// A fixed-width key cap such as `C` or `1 – 8`, so the action column aligns.
fn key_cap(ui: &mut Ui, keys: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(104.0, 22.0), egui::Sense::hover());
    let painter = ui.painter();
    let cap = egui::Rect::from_min_size(
        rect.min,
        Vec2::new(
            (painter
                .layout_no_wrap(
                    keys.to_owned(),
                    egui::FontId::new(11.5, egui::FontFamily::Monospace),
                    TEXT,
                )
                .size()
                .x
                + 16.0)
                .clamp(28.0, rect.width()),
            rect.height(),
        ),
    );
    painter.rect_filled(cap, egui::CornerRadius::same(3), crate::theme::WIDGET);
    // A darker bottom lip reads as a physical key.
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(cap.min.x, cap.max.y - 2.0), cap.max),
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 3,
            se: 3,
        },
        crate::theme::GRAPHITE,
    );
    painter.rect_stroke(
        cap,
        egui::CornerRadius::same(3),
        egui::Stroke::new(1.0, crate::theme::BORDER_STRONG),
        egui::StrokeKind::Inside,
    );
    painter.text(
        cap.center() - Vec2::new(0.0, 1.0),
        egui::Align2::CENTER_CENTER,
        keys,
        egui::FontId::new(11.5, egui::FontFamily::Monospace),
        TEXT,
    );
}

/// Whether the global keys currently act, and why not when they do not.
pub(crate) fn shortcut_status(l: Locale, state: &StudioUiState) -> (String, Tone) {
    if state.connection_status.controls_enabled() && state.view.is_some() {
        (l.text(Msg::KeysActive).to_owned(), Tone::Ready)
    } else {
        (
            l.format(
                Msg::KeysInactive,
                &[(
                    "reason",
                    Arg::Str(l.tr(unavailable_reason(state, Capability::Transition))),
                )],
            ),
            Tone::Pending,
        )
    }
}

/// Right-column marker for a menu item blocked by a padlock.
const LOCKED_MARK: &str = Msg::Locked.en();

/// A menu entry whose right column reads `LOCKED` (amber) while a padlock
/// blocks it, and its shortcut otherwise.
fn lockable_item(
    ui: &mut Ui,
    (enabled, locked): (bool, bool),
    label: &str,
    shortcut: Option<&str>,
    hover: Option<&str>,
    reason: &str,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let mut button = Button::new(label);
    if locked {
        button = button.shortcut_text(
            RichText::new(l.tr(LOCKED_MARK))
                .size(10.0)
                .strong()
                .color(Tone::Pending.color()),
        );
    } else if let Some(shortcut) = shortcut {
        button = button.shortcut_text(RichText::new(shortcut).color(MUTED));
    }
    let response = ui.add_enabled(enabled, button);
    let response = match hover {
        Some(hover) => response.on_hover_text(hover),
        None => response,
    };
    response
        .on_disabled_hover_text(l10n::unavailable(l, reason))
        .clicked()
}

/// Edit-menu History items: Undo/Redo naming the revision they target, with
/// shortcut hints (or `LOCKED`), and the padlock with its current state.
fn history_menu_items(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let history = edit_history_availability(state);
    heading(ui, l.text(Msg::HistoryHeading));
    for (verb, shortcut, side, intent) in [
        (
            HistoryVerb::Undo,
            UNDO_SHORTCUT,
            history.undo,
            history.undo_intent(),
        ),
        (
            HistoryVerb::Redo,
            REDO_SHORTCUT,
            history.redo,
            history.redo_intent(),
        ),
    ] {
        if lockable_item(
            ui,
            (side.enabled(), side.locked),
            &side.menu_label(l, verb),
            Some(shortcut),
            Some(&side.hover(l, verb, shortcut)),
            side.reason,
        ) && let Some(intent) = intent
        {
            intents.push(intent);
        }
    }
    ui.separator();
    let padlock = padlock_toggle(state);
    let (status, tone) = if padlock.locked {
        (Msg::Locked.en(), Tone::Pending)
    } else {
        (Msg::Unlocked.en(), Tone::Idle)
    };
    if status_item(
        ui,
        padlock.enabled,
        l.tr(padlock.menu_label()),
        (status, tone),
        l.tr(padlock.hover()),
        padlock.reason,
    ) && let Some(intent) = padlock.intent()
    {
        intents.push(intent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StudioConnectionStatus;

    #[test]
    fn shortcut_status_reports_why_keys_are_inactive() {
        let state = StudioUiState::new(StudioConnectionStatus::Backoff);
        assert_eq!(
            shortcut_status(Locale::ENGLISH, &state),
            (
                "Keys inactive: the control connection is not ready yet".to_owned(),
                Tone::Pending
            )
        );
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(
            shortcut_status(Locale::ENGLISH, &state).0,
            "Keys inactive: project state is not synchronized yet"
        );
        assert_eq!(
            shortcut_status(Locale::new(Language::Spanish), &state).0,
            "Teclas inactivas: el estado del proyecto aún no está sincronizado"
        );
    }
}
