//! The application menu bar plus the Help windows.
//!
//! Every action item maps to an existing [`StudioIntent`] under that intent's
//! usual gate. Every item says what it does (and its key) on hover; a
//! disabled item adds the blocking reason.
//! Output covers transitions, Fade to black, the manual T-bar, per-slot
//! recorders, per-target stream Start/Stop, and Stinger slot assignment.
//! Inputs also opens the Add input dialog and the Remove confirmation.
//! View items only change local layout state.

use egui::{Button, RichText, Ui, Vec2};
use fm_protocol::{WireRecorderSlot, WireStingerSlotId, WireStreamTargetId};
use fm_types::InputId;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_bank::{neighbour_move_lock_reason, project_editing_available};
use crate::l10n::{self, Arg, Msg};
use crate::outputs::{recorder_row, stream_row};
use crate::panels::SideTab;
use crate::panels::{
    latest_stream_sample, stinger_slot_presentation, stinger_slot_state, stinger_slot_tone,
};
use crate::stream_control::{stream_action_text, stream_unavailable_reason};
use crate::theme::{
    Tone, accent_color, ellipsize, mono, muted_color, paint_lamp, section_label, text_color,
    text_dim_color,
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

/// Which console regions are visible and how wide the side panel is;
/// stored in the shell between frames and, by the host, between sessions.
#[allow(clippy::struct_excessive_bools)] // each flag is one independent panel toggle
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsoleLayout {
    pub show_input_bank: bool,
    pub show_mixer: bool,
    pub show_side_panel: bool,
    pub show_status_bar: bool,
    /// The side panel width the operator dragged to, in points; `None`
    /// follows the default width. Clamped to the window when drawn.
    pub side_panel_width: Option<u16>,
    /// Progressive disclosure: the mixer strips' typed gain, balance, and
    /// delay entries. Hidden by default; the fader, Mute, Solo, and AFV
    /// keys always show.
    pub show_mixer_details: bool,
    /// Progressive disclosure: the manual T-bar's exact position, Commit
    /// and Cancel keys, and status card while no manual transition is
    /// active. Hidden by default; they always show during a transition.
    pub show_manual_details: bool,
    /// Whether the Animation window is docked along the bottom of the
    /// console instead of floating over it.
    pub animation_docked: bool,
    /// The floating Animation window's size the operator resized it to, in
    /// points; `None` uses the default size. Clamped to the window when
    /// drawn.
    pub animation_size: Option<(u16, u16)>,
    /// The docked Animation panel's height the operator dragged to, in
    /// points; `None` uses the default height.
    pub animation_dock_height: Option<u16>,
}

impl Default for ConsoleLayout {
    fn default() -> Self {
        Self {
            show_input_bank: true,
            show_mixer: true,
            show_side_panel: true,
            show_status_bar: true,
            side_panel_width: None,
            show_mixer_details: false,
            show_manual_details: false,
            animation_docked: false,
            animation_size: None,
            animation_dock_height: None,
        }
    }
}

impl ConsoleLayout {
    /// Narrowest side panel width the console draws, in points.
    pub const MIN_SIDE_PANEL_WIDTH: u16 = 300;
    /// Widest side panel width kept in preferences, in points.
    pub const MAX_SIDE_PANEL_WIDTH: u16 = 1_200;

    /// Smallest floating Animation window side kept, in points.
    pub const MIN_ANIMATION_SIDE: u16 = 360;
    /// Largest floating Animation window side kept, in points.
    pub const MAX_ANIMATION_SIDE: u16 = 4_000;
    /// Lowest docked Animation panel kept, in points.
    pub const MIN_ANIMATION_DOCK_HEIGHT: u16 = 240;
    /// Tallest docked Animation panel kept, in points.
    pub const MAX_ANIMATION_DOCK_HEIGHT: u16 = 2_000;

    /// Preference keys, in the order [`Self::preference_entries`] writes them.
    pub const PREFERENCE_KEYS: [&'static str; 10] = [
        "layout.input_bank",
        "layout.mixer",
        "layout.side_panel",
        "layout.status_bar",
        "layout.side_panel_width",
        "layout.mixer_details",
        "layout.manual_details",
        "layout.animation",
        "layout.animation_size",
        "layout.animation_dock_height",
    ];

    /// `key = value` pairs that reproduce this layout in a preferences file.
    /// An automatic width, size, or height is written as `auto`; the
    /// Animation window as `docked` or `floating`, its size as
    /// `<width>x<height>`.
    #[must_use]
    pub fn preference_entries(&self) -> [(&'static str, String); 10] {
        let shown = |shown: bool| if shown { "shown" } else { "hidden" }.to_owned();
        let points =
            |value: Option<u16>| value.map_or_else(|| "auto".to_owned(), |value| value.to_string());
        let [
            bank,
            mixer,
            side,
            status,
            width,
            mixer_details,
            manual_details,
            animation,
            animation_size,
            animation_dock_height,
        ] = Self::PREFERENCE_KEYS;
        [
            (bank, shown(self.show_input_bank)),
            (mixer, shown(self.show_mixer)),
            (side, shown(self.show_side_panel)),
            (status, shown(self.show_status_bar)),
            (width, points(self.side_panel_width)),
            (mixer_details, shown(self.show_mixer_details)),
            (manual_details, shown(self.show_manual_details)),
            (
                animation,
                if self.animation_docked {
                    "docked"
                } else {
                    "floating"
                }
                .to_owned(),
            ),
            (
                animation_size,
                self.animation_size.map_or_else(
                    || "auto".to_owned(),
                    |(width, height)| format!("{width}x{height}"),
                ),
            ),
            (animation_dock_height, points(self.animation_dock_height)),
        ]
    }

    /// `value` as points within `range`.
    fn parse_points(value: &str, range: &core::ops::RangeInclusive<u16>) -> Option<u16> {
        value
            .parse::<u16>()
            .ok()
            .filter(|points| range.contains(points))
    }

    /// Stores `value`, `auto` (`None`) or points within `range`, in `slot`.
    /// Returns `false`, leaving `slot` unchanged, for any other value.
    fn apply_points(
        slot: &mut Option<u16>,
        value: &str,
        range: &core::ops::RangeInclusive<u16>,
    ) -> bool {
        if value == "auto" {
            *slot = None;
            return true;
        }
        Self::parse_points(value, range)
            .map(|points| *slot = Some(points))
            .is_some()
    }

    /// Applies one preference entry. Returns `false`, leaving the layout
    /// unchanged, for an unknown key or a value it cannot use.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        let shown = match value {
            "shown" | "true" | "on" => Some(true),
            "hidden" | "false" | "off" => Some(false),
            _ => None,
        };
        let [
            bank,
            mixer,
            side,
            status,
            width,
            mixer_details,
            manual_details,
            animation,
            animation_size,
            animation_dock_height,
        ] = Self::PREFERENCE_KEYS;
        let flag = match key {
            key if key == bank => &mut self.show_input_bank,
            key if key == mixer => &mut self.show_mixer,
            key if key == side => &mut self.show_side_panel,
            key if key == status => &mut self.show_status_bar,
            key if key == mixer_details => &mut self.show_mixer_details,
            key if key == manual_details => &mut self.show_manual_details,
            key if key == width => {
                return Self::apply_points(
                    &mut self.side_panel_width,
                    value,
                    &(Self::MIN_SIDE_PANEL_WIDTH..=Self::MAX_SIDE_PANEL_WIDTH),
                );
            }
            key if key == animation => {
                self.animation_docked = match value {
                    "docked" => true,
                    "floating" => false,
                    _ => return false,
                };
                return true;
            }
            key if key == animation_size => {
                if value == "auto" {
                    self.animation_size = None;
                    return true;
                }
                let range = Self::MIN_ANIMATION_SIDE..=Self::MAX_ANIMATION_SIDE;
                let parsed = value.split_once('x').and_then(|(width, height)| {
                    Some((
                        Self::parse_points(width, &range)?,
                        Self::parse_points(height, &range)?,
                    ))
                });
                return parsed
                    .map(|sides| self.animation_size = Some(sides))
                    .is_some();
            }
            key if key == animation_dock_height => {
                return Self::apply_points(
                    &mut self.animation_dock_height,
                    value,
                    &(Self::MIN_ANIMATION_DOCK_HEIGHT..=Self::MAX_ANIMATION_DOCK_HEIGHT),
                );
            }
            _ => return false,
        };
        match shown {
            Some(shown) => {
                *flag = shown;
                true
            }
            None => false,
        }
    }

    /// Records a floating Animation window size the operator resized to,
    /// clamped to the kept range.
    pub fn record_animation_size(&mut self, size: egui::Vec2) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
        let clamped = |value: f32| {
            value.round().clamp(
                f32::from(Self::MIN_ANIMATION_SIDE),
                f32::from(Self::MAX_ANIMATION_SIDE),
            ) as u16
        };
        self.animation_size = Some((clamped(size.x), clamped(size.y)));
    }

    /// Records a docked Animation panel height the operator dragged to,
    /// clamped to the kept range.
    pub fn record_animation_dock_height(&mut self, height: f32) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
        let points = height.round().clamp(
            f32::from(Self::MIN_ANIMATION_DOCK_HEIGHT),
            f32::from(Self::MAX_ANIMATION_DOCK_HEIGHT),
        ) as u16;
        self.animation_dock_height = Some(points);
    }

    /// The side panel width to draw in a window allowing at most
    /// `max_width`, given the default width.
    #[must_use]
    pub fn side_panel_width_within(&self, default_width: f32, max_width: f32) -> f32 {
        let min = f32::from(Self::MIN_SIDE_PANEL_WIDTH);
        let max = max_width.max(min);
        self.side_panel_width
            .map_or(default_width, f32::from)
            .clamp(min, max)
    }

    /// Records a side panel width the operator dragged to: the default
    /// width, within a point, is stored as automatic.
    pub fn record_side_panel_width(&mut self, width: f32, default_width: f32) {
        self.side_panel_width = if (width - default_width).abs() < 1.0 {
            None
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
            let points = width.round().clamp(
                f32::from(Self::MIN_SIDE_PANEL_WIDTH),
                f32::from(Self::MAX_SIDE_PANEL_WIDTH),
            ) as u16;
            Some(points)
        };
    }
}

/// One menu entry: label, optional shortcut, what it does (`hover`), and
/// the reason shown while it is disabled.
fn item(
    ui: &mut Ui,
    enabled: bool,
    label: &str,
    shortcut: Option<&str>,
    hover: &str,
    reason: &str,
) -> bool {
    let mut button = Button::new(label);
    let l = l10n::locale(ui.ctx());
    let hover = match shortcut {
        Some(shortcut) => {
            button = button.shortcut_text(RichText::new(l.keys(shortcut)).color(muted_color()));
            l10n::with_key(l, hover, shortcut)
        }
        None => hover.to_owned(),
    };
    ui.add_enabled(enabled, button)
        .hint(&hover, reason)
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
            .size(crate::theme::legible(10.0))
            .strong()
            .color(tone.color()),
    );
    ui.add_enabled(enabled, button)
        .hint(hover, reason)
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

/// Height the keyboard reference keeps for its title, status, intro, and
/// footer around the scrolling key list.
const SHORTCUTS_CHROME_HEIGHT: f32 = 230.0;

/// Width of the View menu, which wraps its layout note to it.
const VIEW_MENU_WIDTH: f32 = 230.0;

/// Longest menu label, in characters, before it ends in an ellipsis.
const MENU_LABEL_CHARS: usize = 32;

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(section_label(text).size(crate::theme::legible(9.5)));
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
            ui.menu_button(l.text(Msg::MenuFile), |ui| self.file_menu(ui, state))
                .response
                .on_hover_text(l.text(Msg::MenuFileHover));
            ui.menu_button(l.text(Msg::MenuEdit), |ui| {
                self.edit_menu(ui, state, intents);
            })
            .response
            .on_hover_text(l.text(Msg::MenuEditHover));
            ui.menu_button(l.text(Msg::MenuView), |ui| self.view_menu(ui))
                .response
                .on_hover_text(l.text(Msg::MenuViewHover));
            ui.menu_button(l.text(Msg::MenuInputs), |ui| {
                self.inputs_menu(ui, state, intents);
            })
            .response
            .on_hover_text(l.text(Msg::MenuInputsHover));
            ui.menu_button(l.text(Msg::MenuOutput), |ui| {
                self.output_menu(ui, state, manual, intents);
            })
            .response
            .on_hover_text(l.text(Msg::MenuOutputHover));
            ui.menu_button(l.text(Msg::MenuHelp), |ui| self.help_menu(ui))
                .response
                .on_hover_text(l.text(Msg::MenuHelpHover));
        });
    }

    fn file_menu(&mut self, ui: &mut Ui, state: &StudioUiState) {
        let l = self.locale;
        ui.set_min_width(240.0);
        match &state.view {
            Some(view) => {
                heading(ui, l.text(Msg::FileShowHeading));
                ui.label(
                    RichText::new(ellipsize(&view.show_name, MENU_LABEL_CHARS)).color(text_color()),
                )
                .on_hover_text(&view.show_name);
                ui.label(
                    mono(l.format(
                        Msg::FileRevisionInputs,
                        &[
                            ("revision", Arg::Plain(view.cursor.revision.get())),
                            ("count", Arg::from(view.inputs.len())),
                        ],
                    ))
                    .size(crate::theme::legible(11.0))
                    .color(text_dim_color()),
                );
            }
            None => {
                ui.label(RichText::new(l.text(Msg::NoSynchronizedShow)).color(muted_color()));
            }
        }
        ui.label(
            RichText::new(l.text(Msg::FileSavedByDaemon))
                .size(crate::theme::legible(10.5))
                .color(muted_color()),
        );
        ui.separator();
        if ui
            .add(
                Button::new(l.text(Msg::MenuPreferences)).shortcut_text(
                    RichText::new(l.keys(crate::preferences::PREFERENCES_SHORTCUT))
                        .color(muted_color()),
                ),
            )
            .on_hover_text(l.text(Msg::MenuPreferencesHover))
            .clicked()
        {
            self.show_preferences = true;
        }
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

    #[allow(clippy::too_many_lines)] // one menu group, one immediate-mode region
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
            .color(if name.is_some() {
                text_color()
            } else {
                muted_color()
            }),
        );
        let no_preview = Msg::ReasonNoPreviewInput.en();
        let locked = lock_reason.is_some();
        if lockable_item(
            ui,
            (editing && preview.is_some(), locked),
            l.text(Msg::RenameEllipsis),
            None,
            l.text(Msg::RenamePreviewHover),
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
            l.text(Msg::MoveEarlierHover),
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
            l.text(Msg::MoveLaterHover),
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
        // A Scene input on Preview: its scene in the layer designer, which
        // opens to look even when edits are locked.
        let scene =
            preview.and_then(|(_, input)| state.project_state.as_ref()?.scene(input)?.scene);
        if let Some(scene) = scene
            && item(
                ui,
                true,
                l.text(Msg::DesignOpen),
                Some(crate::SCENE_DESIGNER_SHORTCUT),
                l.text(Msg::DesignOpenHover),
                "",
            )
        {
            self.open_designer(Some(scene));
        }
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.set_min_width(VIEW_MENU_WIDTH);
        if item(
            ui,
            true,
            l.text(Msg::ColorMenuItem),
            Some(crate::COLOR_SHORTCUT),
            l.text(Msg::ColorMenuHover),
            "",
        ) {
            self.open_color(None);
        }
        if item(
            ui,
            true,
            l.text(Msg::AnimMenuItem),
            Some(crate::ANIMATION_SHORTCUT),
            l.text(Msg::AnimMenuHover),
            "",
        ) {
            self.open_animation(None);
        }
        if item(
            ui,
            true,
            l.text(Msg::DesignMenuItem),
            Some(crate::SCENE_DESIGNER_SHORTCUT),
            l.text(Msg::DesignMenuHover),
            "",
        ) {
            self.open_designer(None);
        }
        ui.separator();
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
        heading(ui, l.text(Msg::ViewDetails));
        ui.checkbox(
            &mut self.layout.show_mixer_details,
            l.text(Msg::ViewMixerDetails),
        )
        .on_hover_text(l.text(Msg::ViewMixerDetailsHover));
        ui.checkbox(
            &mut self.layout.show_manual_details,
            l.text(Msg::ViewManualDetails),
        )
        .on_hover_text(l.text(Msg::ViewManualDetailsHover));
        ui.separator();
        heading(ui, l.text(Msg::ViewSidePanelTab));
        let tab_reason = Msg::ReasonSidePanelHidden.en();
        ui.add_enabled_ui(self.layout.show_side_panel, |ui| {
            for tab in SideTab::ALL {
                ui.radio_value(&mut self.side_tab, tab, l.text(tab.title()))
                    .hint(l.text(tab.hover()), tab_reason);
            }
        });
        ui.separator();
        if ui
            .add_enabled(
                self.layout != ConsoleLayout::default(),
                Button::new(l.text(Msg::ViewResetLayout)),
            )
            .hint(
                l.text(Msg::ViewResetLayoutHover),
                Msg::ViewResetLayoutDisabled.en(),
            )
            .clicked()
        {
            self.reset_layout(ui.ctx());
        }
        ui.scope(|ui| {
            // Wrap the note to the menu's own width instead of widening it.
            ui.set_max_width(VIEW_MENU_WIDTH);
            ui.add(
                egui::Label::new(
                    RichText::new(l.text(Msg::ViewLayoutRemembered))
                        .size(crate::theme::legible(10.0))
                        .color(muted_color()),
                )
                .wrap(),
            );
        });
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
                .hint(l.text(Msg::ShortcutSelectPreview), preview_reason);
            return;
        };
        heading(ui, l.text(Msg::SelectOnPreview));
        for (index, (input, name)) in view.inputs.iter().zip(&view.input_names).enumerate() {
            let shortcut = (index < 8).then(|| format!("{}", index + 1));
            let label = format!("{:>2}  {}", index + 1, ellipsize(name, MENU_LABEL_CHARS));
            if item(
                ui,
                preview,
                &label,
                shortcut.as_deref(),
                &l.format(Msg::SelectNamedOnPreviewHover, &[("name", Arg::Str(name))]),
                preview_reason,
            ) {
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
                        &l.format(Msg::CutNamedToProgramHover, &[("name", Arg::Str(name))]),
                        cut_reason,
                    ) {
                        intents.push(StudioIntent::SelectPreview(*input));
                        intents.push(StudioIntent::Cut);
                    }
                }
            })
            .response
            .hint(l.text(Msg::ShortcutTileDoubleClick), cut_reason);
        });
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
            l.text(Msg::AddInputMenuHover),
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
            .response
            .hint(l.text(Msg::RemoveInputMenuHover), reason);
        });
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
        let transitions: [(Msg, Option<&str>, Msg, bool, StudioIntent); 6] = [
            (
                Msg::TransitionCut,
                Some("C"),
                Msg::ShortcutCut,
                availability.basic(),
                StudioIntent::Cut,
            ),
            (
                Msg::TransitionFade,
                Some("F"),
                Msg::AutoFadeHover,
                availability.basic(),
                StudioIntent::Fade { duration_frames },
            ),
            (
                Msg::TransitionWipe,
                None,
                Msg::AutoWipeHover,
                availability.wipe,
                StudioIntent::Wipe { duration_frames },
            ),
            (
                Msg::TransitionAlphaFade,
                None,
                Msg::AutoAlphaHover,
                availability.alpha_fade,
                StudioIntent::AlphaFade { duration_frames },
            ),
            (
                Msg::TransitionSlide,
                None,
                Msg::AutoSlideHover,
                availability.slide(),
                StudioIntent::Slide { duration_frames },
            ),
            (
                Msg::TransitionZoom,
                None,
                Msg::AutoZoomHover,
                availability.zoom(),
                StudioIntent::Zoom { duration_frames },
            ),
        ];
        for (label, shortcut, hover, enabled, intent) in transitions {
            if item(ui, enabled, l.text(label), shortcut, l.text(hover), reason) {
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
        let (label, hover, enabled) = if black {
            (
                l.text(Msg::ReturnToLive),
                l.text(Msg::FtbReverseHover),
                ftb.to_live && !in_flight,
            )
        } else {
            (
                l.text(Msg::FadeToBlack),
                l.text(Msg::FtbHover),
                ftb.to_black && !in_flight,
            )
        };
        if item(ui, enabled, label, Some("B"), hover, reason) {
            intents.push(StudioIntent::FadeToBlack {
                active: !black,
                duration_frames: self.fade_to_black_duration_frames,
            });
        }
        if state
            .view
            .as_ref()
            .is_some_and(|view| !view.outputs.is_empty())
        {
            ui.horizontal(|ui| {
                ui.label(l.text(Msg::FtbOutputsMenu))
                    .on_hover_text(l.text(Msg::FtbOutputsHover));
                fade_to_black::draw_fade_to_black_outputs(ui, state, intents);
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
            l.text(Msg::CommitHover),
            manual_reason,
        ) {
            intents.push(StudioIntent::CommitManualTransition);
        }
        if item(
            ui,
            manual.active_controls,
            l.text(Msg::CancelTransition),
            Some("Esc"),
            l.text(Msg::ShortcutCancelManual),
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
                .hint(
                    l.text(Msg::NoStreamTargetsHover),
                    if state.view.is_some() {
                        Msg::ReasonNoStreamTargets.en()
                    } else {
                        Msg::ReasonNotSynchronized.en()
                    },
                );
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
                &stream_action_text(l, requested, &stream.name),
                stream_unavailable_reason(state, stream),
            ) && let Some(action) = allowed
            {
                intents.push(StudioIntent::Stream(action));
            }
        }
        ui.separator();
        heading(ui, l.text(Msg::TabStingers));
        self.stinger_menu(ui, state, intents);
        ui.separator();
        if ui
            .button(l.text(Msg::MenuFullscreenOutputs))
            .on_hover_text(l.text(Msg::MenuFullscreenOutputsHover))
            .clicked()
        {
            self.open_display_outputs(intents);
            ui.close();
        }
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
            .response
            .hint(l.text(Msg::AssignStingerMediaHover), assign_reason);
        });
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
            .response
            .hint(l.text(Msg::ClearStingerMenuHover), clear_reason);
        });
        if ui
            .add_enabled(
                self.layout.show_side_panel,
                Button::new(l.text(Msg::StingerSlotsEllipsis)),
            )
            .hint(
                l.text(Msg::StingerSlotsHover),
                Msg::ReasonSidePanelHidden.en(),
            )
            .clicked()
        {
            self.side_tab = SideTab::Stingers;
        }
    }

    fn help_menu(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.set_min_width(220.0);
        if ui
            .add(
                Button::new(l.text(Msg::KeyboardShortcuts))
                    .shortcut_text(RichText::new(l.text(Msg::KeyHelp)).color(muted_color())),
            )
            .on_hover_text(l10n::with_key(
                l,
                l.text(Msg::ShortcutHelp),
                l.text(Msg::KeyHelp),
            ))
            .clicked()
        {
            self.show_shortcuts = true;
        }
        if ui
            .button(l.text(Msg::AboutStudio))
            .on_hover_text(l.text(Msg::AboutHover))
            .clicked()
        {
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
                                .size(crate::theme::legible(11.5))
                                .strong()
                                .color(tone.color()),
                        );
                    });
                    ui.label(
                        RichText::new(l.text(Msg::ShortcutsIntro))
                            .size(crate::theme::legible(11.0))
                            .color(text_dim_color()),
                    );
                    // Short windows scroll the reference instead of
                    // pushing its footer off screen.
                    let max_height =
                        (context.viewport_rect().height() - SHORTCUTS_CHROME_HEIGHT).max(160.0);
                    egui::ScrollArea::vertical()
                        .id_salt("studio-shortcuts-scroll")
                        .auto_shrink([false, true])
                        .max_height(max_height)
                        .show(ui, |ui| {
                            // One key column for every group, as wide as
                            // the widest cap in any shipped language, so
                            // the action column lines up across groups
                            // and does not move when the language changes.
                            let column = key_cap_column(ui);
                            for group in ShortcutGroup::ALL {
                                ui.add_space(10.0);
                                ui.label(section_label(l.tr(group.title())));
                                ui.add_space(2.0);
                                egui::Grid::new(("studio-shortcuts", group.title()))
                                    .num_columns(2)
                                    .spacing(Vec2::new(16.0, 6.0))
                                    .show(ui, |ui| {
                                        for entry in STUDIO_SHORTCUTS
                                            .iter()
                                            .filter(|entry| entry.group == group)
                                        {
                                            key_cap(ui, &l.keys(entry.keys), column);
                                            ui.label(
                                                RichText::new(l.tr(entry.action))
                                                    .color(text_color()),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            }
                        });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label(
                        RichText::new(l.text(Msg::ShortcutsFooter))
                            .size(crate::theme::legible(10.5))
                            .color(muted_color()),
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
                                .color(accent_color()),
                        );
                        ui.label(
                            RichText::new("STUDIO")
                                .size(14.0)
                                .extra_letter_spacing(2.0)
                                .color(muted_color()),
                        );
                    });
                    ui.label(
                        mono(l.format(
                            Msg::AboutVersion,
                            &[("version", Arg::Str(env!("CARGO_PKG_VERSION")))],
                        ))
                        .color(text_dim_color()),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new(l.text(Msg::AboutDescription)).color(text_color()));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(l.format(
                            Msg::AboutLicense,
                            &[("license", Arg::Str(env!("CARGO_PKG_LICENSE")))],
                        ))
                        .size(crate::theme::legible(11.0))
                        .color(muted_color()),
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
                .fill(crate::theme::raised_color())
                .stroke(egui::Stroke::new(
                    crate::theme::line_width(),
                    crate::theme::border_strong_color(),
                ))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(context, |ui| {
            ui.set_width(width);
            // The close key's place is reserved in the title row but the key
            // is created after the contents, so Tab reaches the dialog's own
            // fields first and the close key last.
            let mut close_slot = egui::Rect::NOTHING;
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).strong().color(text_color()));
                ui.with_layout(l10n::trailing_layout(l), |ui| {
                    close_slot = ui
                        .allocate_exact_size(
                            Vec2::splat(crate::theme::MIN_TARGET),
                            egui::Sense::hover(),
                        )
                        .0;
                });
            });
            ui.separator();
            add_contents(ui);
            if ui
                .put(close_slot, crate::theme::close_key(14.0))
                .on_hover_text(l.text(Msg::CloseEsc))
                .clicked()
            {
                close = true;
            }
        });
    close || response.should_close()
}

/// Minimum width of the key column, so the action column aligns.
const KEY_CAP_COLUMN: f32 = 104.0;

/// The width of a key cap for `keys`: the text plus padding, at least
/// 28 px. A long combination widens its cap (and so the grid's key column)
/// instead of overflowing it.
fn key_cap_width(ui: &Ui, keys: &str) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(
            keys.to_owned(),
            egui::FontId::new(11.5, egui::FontFamily::Monospace),
            text_color(),
        )
        .size()
        .x;
    (text + 16.0).max(28.0)
}

/// The width of the reference's key column: the widest key cap of every
/// shortcut in every shipped language, at least [`KEY_CAP_COLUMN`].
fn key_cap_column(ui: &Ui) -> f32 {
    fm_ui_model::i18n::Language::ALL
        .into_iter()
        .map(l10n::Locale::new)
        .flat_map(|locale| {
            STUDIO_SHORTCUTS
                .iter()
                .map(move |entry| locale.keys(entry.keys))
        })
        .map(|keys| key_cap_width(ui, &keys))
        .fold(KEY_CAP_COLUMN, f32::max)
}

/// A key cap such as `C` or `1 – 8` in a `column` wide slot (see
/// [`key_cap_column`]), so the action column aligns across groups.
fn key_cap(ui: &mut Ui, keys: &str, column: f32) {
    let width = key_cap_width(ui, keys);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(width.max(column), 22.0), egui::Sense::hover());
    let painter = ui.painter();
    let cap = egui::Rect::from_min_size(rect.min, Vec2::new(width, rect.height()));
    painter.rect_filled(
        cap,
        egui::CornerRadius::same(3),
        crate::theme::widget_color(),
    );
    // A darker bottom lip reads as a physical key.
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(cap.min.x, cap.max.y - 2.0), cap.max),
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 3,
            se: 3,
        },
        crate::theme::graphite_color(),
    );
    painter.rect_stroke(
        cap,
        egui::CornerRadius::same(3),
        egui::Stroke::new(
            crate::theme::line_width(),
            crate::theme::border_strong_color(),
        ),
        egui::StrokeKind::Inside,
    );
    painter.text(
        cap.center() - Vec2::new(0.0, 1.0),
        egui::Align2::CENTER_CENTER,
        keys,
        egui::FontId::new(11.5, egui::FontFamily::Monospace),
        text_color(),
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
    hover: &str,
    reason: &str,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let mut button = Button::new(label);
    if locked {
        button = button.shortcut_text(
            RichText::new(l.tr(LOCKED_MARK))
                .size(crate::theme::legible(10.0))
                .strong()
                .color(Tone::Pending.color()),
        );
    } else if let Some(shortcut) = shortcut {
        button = button.shortcut_text(RichText::new(l.keys(shortcut)).color(muted_color()));
    }
    ui.add_enabled(enabled, button)
        .hint(hover, reason)
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
            &side.hover(l, verb, shortcut),
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
