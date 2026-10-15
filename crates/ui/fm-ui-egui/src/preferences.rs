//! Console preferences: the Preferences window (language, clock, UI scale,
//! high-contrast theme) and its keyboard shortcuts.
//!
//! Everything here is local to this console. The shell holds the choices;
//! the host persists them (see [`Appearance::preference_entries`]) and never
//! sends them to the daemon.

use egui::{Button, Event, Key, Modifiers, RichText, Ui, Vec2};
use fm_ui_model::i18n::{HourCycle, Language, Locale};

use crate::gating::Hint;
use crate::l10n::{self, Arg, Msg};
use crate::theme::{caption, section_label, text_dim_color};
use crate::{StudioShell, toasts::ToastKind};

/// Display preferences: the UI scale and the high-contrast theme.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Appearance {
    /// Interface zoom in percent, within
    /// [`Self::MIN_UI_SCALE_PERCENT`]..=[`Self::MAX_UI_SCALE_PERCENT`].
    pub ui_scale_percent: u16,
    /// Draw with the high-contrast palette.
    pub high_contrast: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            ui_scale_percent: Self::DEFAULT_UI_SCALE_PERCENT,
            high_contrast: false,
        }
    }
}

impl Appearance {
    /// Smallest UI scale, in percent.
    pub const MIN_UI_SCALE_PERCENT: u16 = 90;
    /// Largest UI scale, in percent.
    pub const MAX_UI_SCALE_PERCENT: u16 = 150;
    /// The UI scale of a fresh console, in percent.
    pub const DEFAULT_UI_SCALE_PERCENT: u16 = 100;
    /// The scales offered in Preferences and stepped by Ctrl+= / Ctrl+-.
    pub const UI_SCALE_PRESETS: [u16; 5] = [90, 100, 110, 125, 150];

    /// Preference keys, in the order [`Self::preference_entries`] writes them.
    pub const PREFERENCE_KEYS: [&'static str; 2] = ["ui.scale", "ui.high_contrast"];

    /// Clamps any requested scale into the supported range.
    #[must_use]
    pub fn clamp_ui_scale(percent: i64) -> u16 {
        let clamped = percent.clamp(
            i64::from(Self::MIN_UI_SCALE_PERCENT),
            i64::from(Self::MAX_UI_SCALE_PERCENT),
        );
        u16::try_from(clamped).unwrap_or(Self::DEFAULT_UI_SCALE_PERCENT)
    }

    /// This appearance with the scale set to `percent`, clamped.
    #[must_use]
    pub fn with_ui_scale(self, percent: i64) -> Self {
        Self {
            ui_scale_percent: Self::clamp_ui_scale(percent),
            ..self
        }
    }

    /// The egui zoom factor for the scale (1.0 at 100 %).
    #[must_use]
    pub fn zoom_factor(self) -> f32 {
        f32::from(Self::clamp_ui_scale(i64::from(self.ui_scale_percent))) / 100.0
    }

    /// The next preset above (`larger`) or below the current scale; the
    /// current scale at either end of the range.
    #[must_use]
    pub fn stepped_ui_scale(self, larger: bool) -> u16 {
        let current = Self::clamp_ui_scale(i64::from(self.ui_scale_percent));
        let next = if larger {
            Self::UI_SCALE_PRESETS
                .iter()
                .copied()
                .find(|preset| *preset > current)
        } else {
            Self::UI_SCALE_PRESETS
                .iter()
                .rev()
                .copied()
                .find(|preset| *preset < current)
        };
        next.unwrap_or(current)
    }

    /// Reads a saved scale: a whole percent, optionally ending in `%`,
    /// clamped into range. `None` when it is not a number.
    #[must_use]
    pub fn parse_ui_scale(value: &str) -> Option<u16> {
        let value = value.trim();
        let digits = value.strip_suffix('%').unwrap_or(value).trim_end();
        digits.parse::<i64>().ok().map(Self::clamp_ui_scale)
    }

    /// Reads a saved contrast choice. `None` for an unknown value.
    #[must_use]
    pub fn parse_high_contrast(value: &str) -> Option<bool> {
        match value.trim() {
            "on" | "true" | "high" => Some(true),
            "off" | "false" | "standard" => Some(false),
            _ => None,
        }
    }

    /// `key = value` pairs that reproduce this appearance in a preferences
    /// file.
    #[must_use]
    pub fn preference_entries(&self) -> [(&'static str, String); 2] {
        let [scale, contrast] = Self::PREFERENCE_KEYS;
        [
            (scale, self.ui_scale_percent.to_string()),
            (
                contrast,
                if self.high_contrast { "on" } else { "off" }.to_owned(),
            ),
        ]
    }
}

/// Whether a key press with `modifiers` is Ctrl (Cmd on macOS) alone, as
/// the preference shortcuts require.
fn command_only(modifiers: Modifiers) -> bool {
    modifiers.command && !modifiers.alt
}

/// A preference shortcut pressed this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreferenceKey {
    /// Ctrl+, opens or closes Preferences.
    TogglePreferences,
    /// Ctrl+= (or Ctrl++) enlarges the interface one preset.
    Larger,
    /// Ctrl+- reduces it one preset.
    Smaller,
    /// Ctrl+0 returns to 100 %.
    ResetScale,
}

/// Maps a key press to its preference shortcut.
pub(crate) fn preference_key(key: Key, modifiers: Modifiers) -> Option<PreferenceKey> {
    if !command_only(modifiers) {
        return None;
    }
    match key {
        Key::Comma => Some(PreferenceKey::TogglePreferences),
        Key::Equals | Key::Plus => Some(PreferenceKey::Larger),
        Key::Minus => Some(PreferenceKey::Smaller),
        Key::Num0 => Some(PreferenceKey::ResetScale),
        _ => None,
    }
}

/// Key text of the Preferences shortcut.
pub(crate) const PREFERENCES_SHORTCUT: &str = "Ctrl+,";
/// Key text of the UI scale shortcuts.
pub(crate) const UI_SCALE_SHORTCUT: &str = "Ctrl+= · Ctrl+-";
/// Key text of the UI scale reset shortcut.
pub(crate) const UI_SCALE_RESET_SHORTCUT: &str = "Ctrl+0";

/// A caption whose command-line options (`--lang`, `--clock`) are set in
/// monospace, so their double hyphen reads as typed instead of running
/// together like a dash in the proportional caption font.
pub(crate) fn option_caption(text: &str) -> egui::text::LayoutJob {
    let size = crate::theme::legible(crate::theme::CAPTION_SIZE);
    let plain = egui::TextFormat {
        font_id: egui::FontId::proportional(size),
        color: crate::theme::muted_color(),
        ..Default::default()
    };
    let option = egui::TextFormat {
        font_id: egui::FontId::monospace(size),
        ..plain.clone()
    };
    let mut job = egui::text::LayoutJob::default();
    for (index, word) in text.split(' ').enumerate() {
        if index > 0 {
            job.append(" ", 0.0, plain.clone());
        }
        // Trailing punctuation (`--clock,`) stays in the caption font.
        let (name, rest) = if word.starts_with("--") {
            let end = word
                .find(|c: char| !(c.is_alphanumeric() || c == '-'))
                .unwrap_or(word.len());
            word.split_at(end)
        } else {
            ("", word)
        };
        if !name.is_empty() {
            job.append(name, 0.0, option.clone());
        }
        if !rest.is_empty() {
            job.append(rest, 0.0, plain.clone());
        }
    }
    job
}

/// Width of the Preferences window.
const PREFERENCES_WIDTH: f32 = 460.0;
/// Size of one UI scale preset key.
const SCALE_KEY_SIZE: Vec2 = Vec2::new(64.0, 28.0);

impl StudioShell {
    /// Returns the console's UI scale and contrast preference.
    #[must_use]
    pub const fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// Sets the UI scale (clamped) and contrast; the next frame draws with
    /// them.
    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance.with_ui_scale(i64::from(appearance.ui_scale_percent));
    }

    /// Sets the locale "Reset to defaults" returns to: the language the
    /// console would pick with no saved preference and no `--lang` option,
    /// with the 24-hour clock.
    pub const fn set_default_locale(&mut self, locale: Locale) {
        self.default_locale = Locale {
            hour_cycle: HourCycle::H23,
            ..locale
        };
    }

    /// Whether every Preferences choice already has its default.
    pub(crate) fn preferences_are_default(&self) -> bool {
        self.locale == self.default_locale && self.appearance == Appearance::default()
    }

    /// Returns language, clock, scale, and contrast to their defaults.
    pub(crate) fn reset_preferences(&mut self) {
        self.locale = self.default_locale;
        self.appearance = Appearance::default();
    }

    /// Applies the UI scale to the context; egui's own Ctrl +/- zoom is
    /// replaced by the shell's, which keeps the preference in step.
    pub(crate) fn apply_ui_scale(&self, context: &egui::Context) {
        if context.options(|options| options.zoom_with_keyboard) {
            context.options_mut(|options| options.zoom_with_keyboard = false);
        }
        let zoom = self.appearance.zoom_factor();
        if (context.zoom_factor() - zoom).abs() > f32::EPSILON {
            context.set_zoom_factor(zoom);
        }
    }

    /// Handles Ctrl+, (Preferences) and the UI scale keys. Works in every
    /// connection state, unless a text field has the keyboard; Ctrl+, also
    /// waits while another dialog is open.
    pub(crate) fn handle_preference_keys(&mut self, ui: &mut Ui) {
        let context = ui.ctx().clone();
        if context.egui_wants_keyboard_input() {
            return;
        }
        let other_dialog = self.show_shortcuts
            || self.show_about
            || self.add_input.is_some()
            || self.remove_input.is_some();
        let pressed: Vec<PreferenceKey> = ui.input_mut(|input| {
            let mut pressed = Vec::new();
            input.events.retain(|event| match event {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => preference_key(*key, *modifiers).is_none_or(|action| {
                    pressed.push(action);
                    false
                }),
                _ => true,
            });
            pressed
        });
        for action in pressed {
            match action {
                PreferenceKey::TogglePreferences if !other_dialog => {
                    self.show_preferences = !self.show_preferences;
                }
                PreferenceKey::TogglePreferences => {}
                PreferenceKey::Larger => {
                    self.change_ui_scale(self.appearance.stepped_ui_scale(true));
                }
                PreferenceKey::Smaller => {
                    self.change_ui_scale(self.appearance.stepped_ui_scale(false));
                }
                PreferenceKey::ResetScale => {
                    self.change_ui_scale(Appearance::DEFAULT_UI_SCALE_PERCENT);
                }
            }
        }
    }

    /// Sets the UI scale from a shortcut and confirms it with a toast.
    fn change_ui_scale(&mut self, percent: u16) {
        self.appearance = self.appearance.with_ui_scale(i64::from(percent));
        let l = self.locale;
        let value = l.format(
            Msg::PrefsPercent,
            &[(
                "percent",
                Arg::Plain(u64::from(self.appearance.ui_scale_percent)),
            )],
        );
        self.toasts.upsert(
            "ui-scale",
            ToastKind::Notice,
            l.format(Msg::PrefsUiScaleToast, &[("percent", Arg::Str(&value))]),
        );
    }

    /// Draws the Preferences window when open.
    pub(crate) fn draw_preferences_window(&mut self, context: &egui::Context) {
        if !self.show_preferences {
            return;
        }
        let l = self.locale;
        let mut done = false;
        let close = crate::menu::help_modal(
            context,
            "studio-preferences-window",
            l.text(Msg::PrefsTitle),
            PREFERENCES_WIDTH,
            |ui| done = self.preferences_contents(ui),
        );
        if close || done {
            self.show_preferences = false;
        }
    }

    /// The Preferences window body, in Tab order: language, clock, scale,
    /// contrast, Reset, Done.
    fn preferences_contents(&mut self, ui: &mut Ui) -> bool {
        let l = self.locale;
        // Generous rows: every radio, checkbox, and key is at least 24 px tall.
        ui.spacing_mut().interact_size.y = 24.0;
        ui.spacing_mut().item_spacing = Vec2::new(10.0, 6.0);

        ui.add_space(4.0);
        ui.label(section_label(l.text(Msg::ViewLanguage)));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for language in Language::ALL {
                // Each language is named in itself.
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
        });

        ui.add_space(8.0);
        ui.label(section_label(l.text(Msg::PrefsClock)));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (cycle, label) in [
                (HourCycle::H23, Msg::PrefsClock24h),
                (HourCycle::H12, Msg::PrefsClock12h),
            ] {
                if ui
                    .radio(self.locale.hour_cycle == cycle, l.text(label))
                    .on_hover_text(l.text(Msg::PrefsClockHover))
                    .clicked()
                {
                    self.locale = self.locale.with_hour_cycle(cycle);
                }
            }
        });

        ui.add_space(8.0);
        self.display_preferences(ui);

        ui.add_space(8.0);
        ui.separator();
        ui.add(egui::Label::new(option_caption(l.text(Msg::PrefsSavedNote))).wrap());
        ui.add_space(6.0);
        let mut done = false;
        ui.horizontal(|ui| {
            let defaults = self.preferences_are_default();
            if ui
                .add_enabled(
                    !defaults,
                    Button::new(l.text(Msg::PrefsReset)).min_size(Vec2::new(0.0, 28.0)),
                )
                .hint(l.text(Msg::PrefsResetHover), Msg::PrefsResetDisabled.en())
                .clicked()
            {
                self.reset_preferences();
            }
            ui.with_layout(l10n::trailing_layout(l), |ui| {
                if ui
                    .add(Button::new(l.text(Msg::PrefsDone)).min_size(Vec2::new(88.0, 28.0)))
                    .on_hover_text(l.text(Msg::PrefsDoneHover))
                    .clicked()
                {
                    done = true;
                }
            });
        });
        done
    }

    /// The Display group: UI scale keys and the high-contrast theme.
    fn display_preferences(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.label(section_label(l.text(Msg::PrefsDisplay)));
        let percent = |value: u16| {
            l.format(
                Msg::PrefsPercent,
                &[("percent", Arg::Plain(u64::from(value)))],
            )
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(l.text(Msg::PrefsUiScale)).color(text_dim_color()));
            ui.label(crate::theme::mono(percent(self.appearance.ui_scale_percent)).strong());
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for preset in Appearance::UI_SCALE_PRESETS {
                let selected = self.appearance.ui_scale_percent == preset;
                if ui
                    .add(
                        Button::selectable(selected, crate::theme::mono(percent(preset)))
                            .frame_when_inactive(true)
                            .min_size(SCALE_KEY_SIZE),
                    )
                    .on_hover_text(l.text(Msg::PrefsUiScaleHover))
                    .clicked()
                {
                    self.appearance = self.appearance.with_ui_scale(i64::from(preset));
                }
            }
        });
        ui.label(caption(l.text(Msg::PrefsUiScaleKeys)));
        ui.add_space(4.0);
        ui.checkbox(
            &mut self.appearance.high_contrast,
            l.text(Msg::PrefsHighContrast),
        )
        .on_hover_text(l.text(Msg::PrefsHighContrastHover));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_options_in_the_saved_note_are_set_in_monospace() {
        let job = option_caption("A --lang or --clock, option wins.");
        assert_eq!(job.text, "A --lang or --clock, option wins.");
        let monospace: Vec<&str> = job
            .sections
            .iter()
            .filter(|section| section.format.font_id.family == egui::FontFamily::Monospace)
            .map(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0])
            .collect();
        assert_eq!(monospace, ["--lang", "--clock"]);
    }

    #[test]
    fn ui_scale_clamps_parses_and_steps_through_presets() {
        assert_eq!(Appearance::clamp_ui_scale(40), 90);
        assert_eq!(Appearance::clamp_ui_scale(-5), 90);
        assert_eq!(Appearance::clamp_ui_scale(120), 120);
        assert_eq!(Appearance::clamp_ui_scale(400), 150);
        assert_eq!(Appearance::clamp_ui_scale(i64::MAX), 150);
        assert_eq!(Appearance::parse_ui_scale("125"), Some(125));
        assert_eq!(Appearance::parse_ui_scale(" 110% "), Some(110));
        assert_eq!(Appearance::parse_ui_scale("300"), Some(150));
        assert_eq!(Appearance::parse_ui_scale("big"), None);
        assert_eq!(Appearance::parse_ui_scale("1.5"), None);
        assert_eq!(Appearance::parse_high_contrast("on"), Some(true));
        assert_eq!(Appearance::parse_high_contrast("off"), Some(false));
        assert_eq!(Appearance::parse_high_contrast("maybe"), None);

        let at = |percent| Appearance::default().with_ui_scale(percent);
        assert_eq!(at(100).stepped_ui_scale(true), 110);
        assert_eq!(at(100).stepped_ui_scale(false), 90);
        assert_eq!(at(150).stepped_ui_scale(true), 150);
        assert_eq!(at(90).stepped_ui_scale(false), 90);
        // A hand-edited scale between presets steps to the neighbours.
        assert_eq!(at(120).stepped_ui_scale(true), 125);
        assert_eq!(at(120).stepped_ui_scale(false), 110);
        assert!((at(125).zoom_factor() - 1.25).abs() < f32::EPSILON);
        assert!((Appearance::default().zoom_factor() - 1.0).abs() < f32::EPSILON);
        // An out-of-range value set directly still zooms within range.
        let wild = Appearance {
            ui_scale_percent: 900,
            high_contrast: false,
        };
        assert!((wild.zoom_factor() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn appearance_preference_entries_round_trip() {
        let appearance = Appearance {
            ui_scale_percent: 125,
            high_contrast: true,
        };
        let entries = appearance.preference_entries();
        assert_eq!(entries[0], ("ui.scale", "125".to_owned()));
        assert_eq!(entries[1], ("ui.high_contrast", "on".to_owned()));
        assert_eq!(Appearance::parse_ui_scale(&entries[0].1), Some(125));
        assert_eq!(Appearance::parse_high_contrast(&entries[1].1), Some(true));
    }

    #[test]
    fn preference_keys_need_the_command_modifier() {
        let command = Modifiers::COMMAND;
        assert_eq!(
            preference_key(Key::Comma, command),
            Some(PreferenceKey::TogglePreferences)
        );
        assert_eq!(
            preference_key(Key::Equals, command),
            Some(PreferenceKey::Larger)
        );
        assert_eq!(
            preference_key(Key::Plus, command),
            Some(PreferenceKey::Larger)
        );
        assert_eq!(
            preference_key(Key::Minus, command),
            Some(PreferenceKey::Smaller)
        );
        assert_eq!(
            preference_key(Key::Num0, command),
            Some(PreferenceKey::ResetScale)
        );
        // Bare keys stay console keys (0 is not a preview key, but `-` and
        // `,` must not resize the console while switching).
        assert_eq!(preference_key(Key::Comma, Modifiers::NONE), None);
        assert_eq!(preference_key(Key::Minus, Modifiers::NONE), None);
        assert_eq!(preference_key(Key::Num0, Modifiers::ALT | command), None);
    }

    /// Presses `key` with `modifiers` through the shell's preference key
    /// handler in one egui pass.
    fn press(shell: &mut StudioShell, context: &egui::Context, key: Key, modifiers: Modifiers) {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        context.begin_pass(egui::RawInput {
            events: vec![event(true), event(false)],
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("studio-preference-keys"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        shell.handle_preference_keys(&mut ui);
        drop(ui);
        let _ = context.end_pass();
    }

    #[test]
    fn preference_shortcuts_toggle_the_window_and_step_the_scale() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        press(&mut shell, &context, Key::Comma, Modifiers::COMMAND);
        assert!(shell.show_preferences);
        // The open window owns the console keys.
        assert!(shell.console_keys_blocked(&context));
        press(&mut shell, &context, Key::Comma, Modifiers::COMMAND);
        assert!(!shell.show_preferences);
        press(&mut shell, &context, Key::Equals, Modifiers::COMMAND);
        assert_eq!(shell.appearance().ui_scale_percent, 110);
        press(&mut shell, &context, Key::Plus, Modifiers::COMMAND);
        assert_eq!(shell.appearance().ui_scale_percent, 125);
        press(&mut shell, &context, Key::Minus, Modifiers::COMMAND);
        press(&mut shell, &context, Key::Minus, Modifiers::COMMAND);
        press(&mut shell, &context, Key::Minus, Modifiers::COMMAND);
        press(&mut shell, &context, Key::Minus, Modifiers::COMMAND);
        assert_eq!(shell.appearance().ui_scale_percent, 90);
        press(&mut shell, &context, Key::Num0, Modifiers::COMMAND);
        assert_eq!(shell.appearance().ui_scale_percent, 100);
        // Another open dialog keeps Ctrl+, from stacking Preferences on it.
        shell.show_shortcuts = true;
        press(&mut shell, &context, Key::Comma, Modifiers::COMMAND);
        assert!(!shell.show_preferences);
    }

    #[test]
    fn reset_restores_the_default_locale_scale_and_theme() {
        let mut shell = StudioShell::default();
        shell.set_default_locale(Locale::new(Language::Spanish).with_hour_cycle(HourCycle::H12));
        // The default clock is always 24 hours.
        assert_eq!(shell.default_locale.hour_cycle, HourCycle::H23);
        shell.set_locale(Locale::ENGLISH.with_hour_cycle(HourCycle::H12));
        shell.set_appearance(Appearance {
            ui_scale_percent: 400,
            high_contrast: true,
        });
        assert_eq!(shell.appearance().ui_scale_percent, 150);
        assert!(!shell.preferences_are_default());
        shell.reset_preferences();
        assert_eq!(shell.locale(), Locale::new(Language::Spanish));
        assert_eq!(shell.appearance(), Appearance::default());
        assert!(shell.preferences_are_default());
    }
}
