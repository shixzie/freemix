//! The console key profiles in Studio (RC-004): the Preferences window's
//! Keyboard section, which chooses the profile the console answers, and
//! the Key profiles window, which copies a profile into one of the
//! operator's own and changes its keys (see [`crate::console_keymap`]).
//!
//! Built-in profiles are shown read only. "Set key" takes the next key
//! press (Esc cancels); a console key outside the profiles, another
//! action's key, or a key the console cannot bind is refused with the
//! reason. While the window is open no console key acts. Any profile
//! exports to a `.fmkeys` file, and an imported file becomes one of the
//! operator's own.

use egui::{Button, ComboBox, RichText, TextEdit, Ui, Vec2};

use crate::StudioShell;
use crate::console_keymap::{
    BuiltinProfile, ConsoleAction, ConsoleStroke, KeyRefusal, MAX_CUSTOM_PROFILES,
    MAX_PROFILE_NAME_BYTES, ProfileChoice, profile_name_is_valid,
};
use crate::gating::Hint;
use crate::l10n::{Arg, Locale, Msg};
use crate::preset_files::PresetFileRequest;
use crate::theme::{caption, section_label, small_button, text_dim_color};
use crate::user_shortcuts::{Captured, capture};

const WINDOW_WIDTH: f32 = 560.0;
const WARN: egui::Color32 = egui::Color32::from_rgb(0xff, 0xb0, 0x00);

/// The Key profiles window's state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KeyProfilesWindow {
    pub(crate) open: bool,
    /// The profile shown.
    pub(crate) selected: ProfileChoice,
    /// The action waiting for its key.
    pub(crate) capturing: Option<ConsoleAction>,
    /// Why the last key change was refused, in the console's words.
    pub(crate) refusal: Option<String>,
    /// The selected custom profile's name as typed.
    pub(crate) name: String,
    /// The `.fmkeys` path typed for export and import.
    pub(crate) file_path: String,
    /// File work for the host, taken by
    /// [`StudioShell::take_preset_file_requests`].
    pub(crate) file_requests: Vec<PresetFileRequest>,
}

/// Every profile the picker offers: the built-in ones, then the
/// operator's.
fn choices(count: usize) -> impl Iterator<Item = ProfileChoice> {
    BuiltinProfile::ALL
        .into_iter()
        .map(ProfileChoice::Builtin)
        .chain((0..count).map(ProfileChoice::Custom))
}

/// A refusal in `l`'s words.
fn refusal_text(l: Locale, stroke: ConsoleStroke, refusal: &KeyRefusal) -> String {
    let keys = l.keys(&stroke.text());
    match refusal {
        KeyRefusal::Fixed(_) => l.format(Msg::KeyProfilesFixed, &[("keys", Arg::Str(&keys))]),
        KeyRefusal::Taken(action) => l.format(
            Msg::KeyProfilesTaken,
            &[
                ("keys", Arg::Str(&keys)),
                ("action", Arg::Str(&action.label(l))),
            ],
        ),
        KeyRefusal::Builtin | KeyRefusal::Missing => {
            l.text(Msg::KeyProfilesReasonBuiltin).to_owned()
        }
    }
}

/// The console stroke a captured shortcut key names, if a profile can
/// take it: one stroke of a key (not a mouse button or a chord).
fn stroke_of(captured: &fm_protocol::WireShortcutKey) -> Option<ConsoleStroke> {
    if captured.then.is_some() || captured.is_mouse() {
        return None;
    }
    Some(ConsoleStroke {
        key: egui::Key::from_name(&captured.key)?,
        command: captured.command,
        shift: captured.shift,
        alt: captured.alt,
    })
}

impl StudioShell {
    /// Opens the Key profiles window on the profile in use.
    pub(crate) fn open_key_profiles(&mut self) {
        let window = &mut self.key_profiles_window;
        if !window.open {
            window.selected = self.key_profiles.active();
            window.capturing = None;
            window.refusal = None;
            window.name = self.key_profiles.name(self.locale, window.selected);
        }
        window.open = true;
    }

    /// The Preferences window's Keyboard section: the profile the console
    /// answers and the way to the Key profiles window.
    pub(crate) fn key_profile_preferences(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.add_space(8.0);
        ui.label(section_label(l.text(Msg::PrefsKeyboard)));
        let active = self.key_profiles.active();
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(l.text(Msg::PrefsKeyProfile)).color(text_dim_color()));
            ComboBox::from_id_salt("studio-prefs-key-profile")
                .selected_text(self.key_profiles.name(l, active))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for choice in choices(self.key_profiles.custom().len()) {
                        let hover = match choice {
                            ProfileChoice::Builtin(profile) => l.text(profile.hover()),
                            ProfileChoice::Custom(_) => l.text(Msg::KeyProfilesUseHover),
                        };
                        if ui
                            .selectable_label(choice == active, self.key_profiles.name(l, choice))
                            .on_hover_text(hover)
                            .clicked()
                        {
                            chosen = Some(choice);
                        }
                    }
                })
                .response
                .on_hover_text(l.text(Msg::PrefsKeyProfileHover));
            if ui
                .button(l.text(Msg::PrefsEditKeyProfiles))
                .on_hover_text(l.text(Msg::PrefsEditKeyProfilesHover))
                .clicked()
            {
                self.open_key_profiles();
            }
        });
        if let Some(choice) = chosen {
            self.key_profiles.choose(choice);
        }
    }

    /// Draws the Key profiles window when open.
    pub(crate) fn draw_key_profiles_window(&mut self, context: &egui::Context) {
        if !self.key_profiles_window.open {
            return;
        }
        let l = self.locale;
        if let Some(action) = self.key_profiles_window.capturing {
            match context.input_mut(|input| capture(&mut input.events)) {
                Some(Captured::Key(key)) => {
                    let window = &mut self.key_profiles_window;
                    window.capturing = None;
                    window.refusal = match (stroke_of(&key), window.selected) {
                        (Some(stroke), ProfileChoice::Custom(index)) => self
                            .key_profiles
                            .set_key(index, action, Some(stroke))
                            .err()
                            .map(|refusal| refusal_text(l, stroke, &refusal)),
                        (Some(_), ProfileChoice::Builtin(_)) => {
                            Some(l.text(Msg::KeyProfilesReasonBuiltin).to_owned())
                        }
                        (None, _) => Some(l.text(Msg::KeyProfilesUnsupported).to_owned()),
                    };
                }
                Some(Captured::Cancel) => self.key_profiles_window.capturing = None,
                Some(Captured::Unsupported) => {
                    self.key_profiles_window.capturing = None;
                    self.key_profiles_window.refusal =
                        Some(l.text(Msg::KeyProfilesUnsupported).to_owned());
                }
                None => {}
            }
        }
        let close = crate::menu::help_modal(
            context,
            "studio-key-profiles",
            l.text(Msg::KeyProfilesTitle),
            WINDOW_WIDTH,
            |ui| self.key_profiles_contents(ui),
        );
        if close {
            self.key_profiles_window.open = false;
            self.key_profiles_window.capturing = None;
        }
    }

    #[allow(clippy::too_many_lines)] // one immediate-mode window
    fn key_profiles_contents(&mut self, ui: &mut Ui) {
        let l = self.locale;
        let profiles = &mut self.key_profiles;
        let window = &mut self.key_profiles_window;
        if profiles.keymap(window.selected).is_none() {
            window.selected = profiles.active();
        }
        let selected = window.selected;
        let custom = match selected {
            ProfileChoice::Custom(index) => Some(index),
            ProfileChoice::Builtin(_) => None,
        };
        let builtin_reason = custom
            .is_none()
            .then_some(Msg::KeyProfilesReasonBuiltin.en());

        let full = profiles.custom().len() >= MAX_CUSTOM_PROFILES;
        ui.horizontal_wrapped(|ui| {
            ComboBox::from_id_salt("studio-key-profiles-picker")
                .selected_text(profiles.name(l, selected))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for choice in choices(profiles.custom().len()) {
                        let hover = match choice {
                            ProfileChoice::Builtin(profile) => l.text(profile.hover()),
                            ProfileChoice::Custom(_) => l.text(Msg::KeyProfilesNameHover),
                        };
                        if ui
                            .selectable_label(choice == selected, profiles.name(l, choice))
                            .on_hover_text(hover)
                            .clicked()
                        {
                            window.selected = choice;
                            window.capturing = None;
                            window.refusal = None;
                            window.name = profiles.name(l, choice);
                        }
                    }
                })
                .response
                .on_hover_text(l.text(Msg::KeyProfilesNameHover));
            if profiles.active() == selected {
                ui.label(
                    RichText::new(l.text(Msg::InUse))
                        .strong()
                        .color(crate::theme::text_color()),
                )
                .on_hover_text(l.text(Msg::KeyProfilesInUseHover));
            } else if ui
                .button(l.text(Msg::KeyProfilesUse))
                .on_hover_text(l.text(Msg::KeyProfilesUseHover))
                .clicked()
            {
                profiles.choose(selected);
            }
            if ui
                .add_enabled(!full, Button::new(l.text(Msg::KeyProfilesCopy)))
                .hint(
                    l.text(Msg::KeyProfilesCopyHover),
                    Msg::KeyProfilesReasonFull.en(),
                )
                .clicked()
            {
                let mut name = l.format(
                    Msg::KeyProfilesCopyName,
                    &[("name", Arg::Str(&profiles.name(l, selected)))],
                );
                while name.len() > MAX_PROFILE_NAME_BYTES {
                    name.pop();
                }
                if let Some(copy) = profiles.add(&name, selected) {
                    window.selected = copy;
                    window.name = name;
                    window.refusal = None;
                }
            }
            if let Some(index) = custom
                && ui
                    .button(l.text(Msg::KeyProfilesDelete))
                    .on_hover_text(l.text(Msg::KeyProfilesDeleteHover))
                    .clicked()
            {
                profiles.remove(index);
                window.selected = profiles.active();
                window.name = profiles.name(l, window.selected);
                window.capturing = None;
                window.refusal = None;
            }
        });
        if let Some(index) = custom {
            let response = ui
                .add(
                    TextEdit::singleline(&mut window.name)
                        .char_limit(MAX_PROFILE_NAME_BYTES)
                        .desired_width(240.0),
                )
                .on_hover_text(l.text(Msg::KeyProfilesNameHover));
            if response.changed() && profile_name_is_valid(&window.name) {
                profiles.rename(index, &window.name);
            }
        }
        ui.label(caption(l.text(if custom.is_none() {
            Msg::KeyProfilesBuiltinNote
        } else {
            Msg::KeyProfilesFocusNote
        })));
        if let Some(refusal) = &window.refusal {
            ui.label(RichText::new(refusal).color(WARN));
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::KeyProfilesFile)))
                .on_hover_text(l.text(Msg::KeyProfilesFileHover));
            ui.add(
                TextEdit::singleline(&mut window.file_path)
                    .hint_text(l.text(Msg::KeyProfilesPathHint))
                    .desired_width(240.0),
            )
            .on_hover_text(l.text(Msg::KeyProfilesPathHover));
            let no_path = window.file_path.trim().is_empty();
            if ui
                .add_enabled(!no_path, small_button(l.text(Msg::KeyProfilesExport)))
                .hint(
                    l.text(Msg::KeyProfilesExportHover),
                    Msg::AnimReasonNoPath.en(),
                )
                .clicked()
                && let Some(text) = profiles.export_text(l, selected)
            {
                window
                    .file_requests
                    .push(PresetFileRequest::ExportKeyProfile {
                        path: window.file_path.trim().into(),
                        text,
                    });
            }
            let import_block = if no_path {
                Some(Msg::AnimReasonNoPath.en())
            } else {
                full.then_some(Msg::KeyProfilesReasonFull.en())
            };
            if ui
                .add_enabled(
                    import_block.is_none(),
                    small_button(l.text(Msg::KeyProfilesImport)),
                )
                .hint(
                    l.text(Msg::KeyProfilesImportHover),
                    import_block.unwrap_or(""),
                )
                .clicked()
            {
                window
                    .file_requests
                    .push(PresetFileRequest::ImportKeyProfile {
                        path: window.file_path.trim().into(),
                    });
            }
        });
        ui.add_space(6.0);
        let Some(keymap) = profiles.keymap(selected) else {
            return;
        };
        let base = custom
            .and_then(|index| profiles.custom().get(index))
            .map(|profile| profile.base.keymap());
        egui::ScrollArea::vertical()
            .id_salt("studio-key-profiles-scroll")
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("studio-key-profiles-grid")
                    .num_columns(3)
                    .spacing(Vec2::new(12.0, 4.0))
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(caption(l.text(Msg::KeyProfilesAction)));
                        ui.label(caption(l.text(Msg::KeyProfilesKey)));
                        ui.label("");
                        ui.end_row();
                        for action in ConsoleAction::all() {
                            let stroke = keymap.stroke(action);
                            ui.label(action.label(l));
                            let keys = stroke.map_or_else(
                                || l.text(Msg::KeyProfilesNoKeyText).to_owned(),
                                |stroke| l.keys(&stroke.text()),
                            );
                            ui.label(RichText::new(keys).monospace());
                            ui.horizontal(|ui| {
                                let capturing = window.capturing == Some(action);
                                let set_text = if capturing {
                                    l.text(Msg::KeyProfilesPress)
                                } else {
                                    l.text(Msg::UserShortcutSetKey)
                                };
                                if ui
                                    .add_enabled(builtin_reason.is_none(), Button::new(set_text))
                                    .hint(
                                        l.text(Msg::KeyProfilesSetKeyHover),
                                        builtin_reason.unwrap_or(""),
                                    )
                                    .clicked()
                                {
                                    window.capturing = Some(action);
                                    window.refusal = None;
                                }
                                if ui
                                    .add_enabled(
                                        builtin_reason.is_none() && stroke.is_some(),
                                        Button::new(l.text(Msg::UserShortcutKeyNone)),
                                    )
                                    .hint(
                                        l.text(Msg::KeyProfilesNoKeyHover),
                                        builtin_reason.unwrap_or(Msg::KeyProfilesNoKeyHover.en()),
                                    )
                                    .clicked()
                                    && let Some(index) = custom
                                {
                                    window.refusal = profiles
                                        .set_key(index, action, None)
                                        .err()
                                        .map(|_| l.text(Msg::KeyProfilesReasonBuiltin).to_owned());
                                }
                                let unchanged = base
                                    .as_ref()
                                    .is_none_or(|base| base.stroke(action) == stroke);
                                let reset_reason = builtin_reason
                                    .or(unchanged.then_some(Msg::KeyProfilesReasonUnchanged.en()));
                                if ui
                                    .add_enabled(
                                        reset_reason.is_none(),
                                        Button::new(l.text(Msg::KeyProfilesReset)),
                                    )
                                    .hint(
                                        l.text(Msg::KeyProfilesResetHover),
                                        reset_reason.unwrap_or(""),
                                    )
                                    .clicked()
                                    && let Some(index) = custom
                                {
                                    let wanted = base.as_ref().and_then(|base| base.stroke(action));
                                    window.refusal =
                                        profiles.reset_key(index, action).err().zip(wanted).map(
                                            |(refusal, stroke)| refusal_text(l, stroke, &refusal),
                                        );
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::stroke_of;
    use crate::console_keymap::{BuiltinProfile, ConsoleStroke, ProfileChoice};
    use crate::preset_files::{PresetFileAnswer, PresetFileRequest};
    use crate::{StudioShell, StudioUiState};

    #[test]
    fn imported_key_files_become_profiles_of_your_own() {
        let state = StudioUiState::default();
        let mut shell = StudioShell::default();
        let request = PresetFileRequest::ImportKeyProfile {
            path: "booth.fmkeys".into(),
        };
        let text = "name = Booth\nbase = vmix\n";
        let intents = shell.receive_preset_file(
            &state,
            &request,
            PresetFileAnswer::KeyProfile(text.to_owned()),
        );
        assert!(intents.is_empty());
        let profiles = shell.console_key_profiles();
        assert_eq!(profiles.custom().len(), 1);
        assert_eq!(profiles.custom()[0].base, BuiltinProfile::Vmix);
        // The window shows the new profile; the console keeps its own.
        assert_eq!(shell.key_profiles_window.selected, ProfileChoice::Custom(0));
        assert_eq!(shell.key_profiles_window.name, "Booth");
        assert_eq!(profiles.active(), ProfileChoice::default());

        // A file naming no profile adds nothing.
        shell.receive_preset_file(
            &state,
            &request,
            PresetFileAnswer::KeyProfile("base = vmix\n".to_owned()),
        );
        assert_eq!(shell.console_key_profiles().custom().len(), 1);
    }

    #[test]
    fn captured_keys_become_strokes_but_mouse_buttons_and_chords_do_not() {
        let key = fm_protocol::WireShortcutKey::stroke("F9", true, false, true);
        assert_eq!(
            stroke_of(&key),
            Some(ConsoleStroke {
                key: egui::Key::F9,
                command: true,
                shift: false,
                alt: true,
            })
        );
        let mouse = fm_protocol::WireShortcutKey::stroke("MouseBack", false, false, false);
        assert_eq!(stroke_of(&mouse), None);
        let chord = fm_protocol::WireShortcutKey {
            then: Some(Box::new(key.clone())),
            ..key
        };
        assert_eq!(stroke_of(&chord), None);
    }
}
