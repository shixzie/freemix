//! Output ▸ Secondary mixes… (IN-023, Protocol 2.158/2.159).
//!
//! The project configures up to fifteen secondary mixes beside Main. Each
//! has a Preview/Program or output-only mode and a Program source, plus a
//! Preview source when it keeps one; sources name an input or another mix.
//! Configured mixes take live cuts and timed transitions, and the daemon
//! reports each mix's live Program/Preview and transition progress in every
//! snapshot. Save, Clear, cut, and transition are ordinary protocol
//! commands: the first two are padlocked project edits, the latter live
//! operations.

use egui::{ComboBox, RichText, TextEdit};
use fm_protocol::{
    AutoTransitionKind, WireInputId, WireMixNumber, WireSecondaryMixConfig, WireSecondaryMixMode,
    WireSecondaryMixSource, WireSecondaryMixState,
};

use crate::l10n::{Locale, Msg};
use crate::theme::{AMBER, section_label};
use crate::{StudioIntent, StudioShell, StudioUiState};

/// Matches the protocol's secondary-mix name bound.
const MAX_SECONDARY_MIX_NAME_BYTES: usize = 128;

/// One unsaved mix form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SecondaryMixDraft {
    pub(crate) number: u8,
    pub(crate) name: String,
    pub(crate) mode: WireSecondaryMixMode,
    pub(crate) program: String,
    pub(crate) preview: String,
}

impl Default for SecondaryMixDraft {
    fn default() -> Self {
        Self {
            number: 2,
            name: "Mix 2".to_owned(),
            mode: WireSecondaryMixMode::PreviewProgram,
            program: String::new(),
            preview: String::new(),
        }
    }
}

/// The Secondary mixes dialog's open state and unsaved form.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SecondaryMixesDialog {
    pub(crate) open: bool,
    pub(crate) draft: SecondaryMixDraft,
    pub(crate) error: Option<String>,
}

impl SecondaryMixDraft {
    /// A draft loaded from one configured mix.
    pub(crate) fn from_config(config: &WireSecondaryMixConfig) -> Self {
        Self {
            number: config.number.number(),
            name: config.name.clone(),
            mode: config.mode,
            program: source_text(config.program),
            preview: config.preview.map_or_else(String::new, source_text),
        }
    }

    /// The configuration this form describes, or why it is not yet valid.
    pub(crate) fn config(&self) -> Result<WireSecondaryMixConfig, &'static str> {
        let number = WireMixNumber::new(self.number).ok_or("Mix number must be 2 through 16")?;
        if number.is_main() {
            return Err("Main is not a secondary mix");
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Mix name must not be empty");
        }
        if name.len() > MAX_SECONDARY_MIX_NAME_BYTES {
            return Err("Mix name is too long");
        }
        let program =
            parse_source(&self.program).ok_or("Program source must be input:<id> or mix:<n>")?;
        let preview = match self.mode {
            WireSecondaryMixMode::PreviewProgram => Some(
                parse_source(&self.preview)
                    .ok_or("Preview source must be input:<id> or mix:<n>")?,
            ),
            WireSecondaryMixMode::OutputOnly => None,
        };
        if preview == Some(program) {
            return Err("Preview must differ from Program");
        }
        Ok(WireSecondaryMixConfig {
            number,
            name: name.to_owned(),
            mode: self.mode,
            program,
            preview,
        })
    }
}

fn parse_source(value: &str) -> Option<WireSecondaryMixSource> {
    if let Some(input) = value.trim().strip_prefix("input:") {
        let input = input.parse::<u128>().ok()?;
        return Some(WireSecondaryMixSource::Input(WireInputId::new(
            core::num::NonZeroU128::new(input)?,
        )));
    }
    if let Some(mix) = value.trim().strip_prefix("mix:") {
        return Some(WireSecondaryMixSource::Mix(WireMixNumber::new(
            mix.parse::<u8>().ok()?,
        )?));
    }
    None
}

fn source_text(source: WireSecondaryMixSource) -> String {
    match source {
        WireSecondaryMixSource::Input(input) => format!("input:{}", input.get().get()),
        WireSecondaryMixSource::Mix(number) => format!("mix:{}", number.number()),
    }
}

fn mode_text(l: Locale, mode: WireSecondaryMixMode) -> String {
    l.text(match mode {
        WireSecondaryMixMode::PreviewProgram => Msg::SecondaryMixPreviewProgram,
        WireSecondaryMixMode::OutputOnly => Msg::SecondaryMixOutputOnly,
    })
    .to_owned()
}

impl StudioShell {
    /// Opens the Secondary mixes dialog.
    pub(crate) fn open_secondary_mixes(&mut self, state: &StudioUiState) {
        self.secondary_mixes.open = true;
        // Start the form on the first two inputs when it has never been
        // filled in.
        if self.secondary_mixes.draft.program.is_empty()
            && let Some(view) = state.view.as_ref()
        {
            if let Some(first) = view.inputs.first() {
                self.secondary_mixes.draft.program = format!("input:{}", first.get().get());
            }
            if let Some(second) = view.inputs.get(1) {
                self.secondary_mixes.draft.preview = format!("input:{}", second.get().get());
            }
        }
    }

    /// Draws the Secondary mixes dialog when open.
    #[allow(clippy::too_many_lines)] // one dialog: live rows and one form
    pub(crate) fn draw_secondary_mixes_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.secondary_mixes.open {
            return;
        }
        let l = self.locale;
        let frames = self.transition_duration_frames;
        let mut open = self.secondary_mixes.open;
        egui::Window::new(l.text(Msg::SecondaryMixesTitle))
            .open(&mut open)
            .resizable(false)
            .default_width(560.0)
            .show(context, |ui| {
                let view = state.view.as_ref();
                let mixes = view.map_or(&[][..], |view| view.secondary_mixes.as_slice());
                if mixes.is_empty() {
                    ui.label(l.text(Msg::SecondaryMixesNone));
                }
                for config in mixes {
                    let live = view.and_then(|view| {
                        view.secondary_mix_states
                            .iter()
                            .find(|state| state.number == config.number)
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Mix {} — {}",
                                config.number.number(),
                                config.name
                            ))
                            .strong(),
                        );
                        ui.label(mode_text(l, config.mode));
                        ui.label(format!(
                            "P {} · {}",
                            source_text(config.program),
                            config.preview.map_or_else(|| "-".to_owned(), source_text)
                        ));
                        if let Some(live) = live {
                            ui.label(live_text(live));
                        }
                    });
                    ui.horizontal(|ui| {
                        let cut = ui
                            .add_enabled(
                                state.can_transition,
                                egui::Button::new(l.text(Msg::SecondaryMixCut)),
                            )
                            .on_hover_text(l.text(Msg::SecondaryMixCutHover));
                        if cut.clicked() {
                            intents.push(StudioIntent::SecondaryMixCut {
                                number: config.number,
                            });
                        }
                        let fade = ui
                            .add_enabled(
                                state.can_transition,
                                egui::Button::new(l.text(Msg::SecondaryMixFade)),
                            )
                            .on_hover_text(l.text(Msg::SecondaryMixFadeHover));
                        if fade.clicked() {
                            intents.push(StudioIntent::SecondaryMixTransition {
                                number: config.number,
                                kind: AutoTransitionKind::Fade,
                                duration_frames: frames,
                            });
                        }
                        let edit = ui.button(l.text(Msg::SecondaryMixEdit));
                        if edit.clicked() {
                            self.secondary_mixes.draft = SecondaryMixDraft::from_config(config);
                            self.secondary_mixes.error = None;
                        }
                        let remove = ui
                            .add_enabled(
                                state.can_edit_project,
                                egui::Button::new(l.text(Msg::SecondaryMixRemove)),
                            )
                            .on_hover_text(l.text(Msg::SecondaryMixRemoveHover));
                        if remove.clicked() {
                            intents.push(StudioIntent::ClearSecondaryMix {
                                number: config.number,
                            });
                        }
                    });
                    ui.separator();
                }

                ui.label(section_label(l.text(Msg::SecondaryMixForm)));
                let draft = &mut self.secondary_mixes.draft;
                ui.horizontal(|ui| {
                    ui.label(l.text(Msg::SecondaryMixNumber));
                    ui.add(egui::DragValue::new(&mut draft.number).range(2..=16))
                        .on_hover_text(l.text(Msg::SecondaryMixNumberHover));
                    ui.label(l.text(Msg::SecondaryMixName));
                    ui.add(TextEdit::singleline(&mut draft.name).desired_width(180.0))
                        .on_hover_text(l.text(Msg::SecondaryMixNameHover));
                });
                ComboBox::from_label(l.text(Msg::SecondaryMixMode))
                    .selected_text(mode_text(l, draft.mode))
                    .show_ui(ui, |ui| {
                        for mode in [
                            WireSecondaryMixMode::PreviewProgram,
                            WireSecondaryMixMode::OutputOnly,
                        ] {
                            ui.selectable_value(&mut draft.mode, mode, mode_text(l, mode));
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label(l.text(Msg::SecondaryMixProgram));
                    ui.add(TextEdit::singleline(&mut draft.program).desired_width(140.0))
                        .on_hover_text(l.text(Msg::SecondaryMixSourceHover));
                    ui.label(l.text(Msg::SecondaryMixPreview));
                    ui.add_enabled(
                        draft.mode == WireSecondaryMixMode::PreviewProgram,
                        TextEdit::singleline(&mut draft.preview).desired_width(140.0),
                    )
                    .on_hover_text(l.text(Msg::SecondaryMixSourceHover));
                });
                ui.horizontal(|ui| {
                    let save = ui
                        .add_enabled(
                            state.can_edit_project,
                            egui::Button::new(l.text(Msg::SecondaryMixSave)),
                        )
                        .on_hover_text(l.text(Msg::SecondaryMixSaveHover));
                    if save.clicked() {
                        match draft.config() {
                            Ok(config) => {
                                self.secondary_mixes.error = None;
                                intents.push(StudioIntent::SetSecondaryMix {
                                    config: Box::new(config),
                                });
                            }
                            Err(error) => self.secondary_mixes.error = Some(error.to_owned()),
                        }
                    }
                    if !state.can_edit_project {
                        ui.label(RichText::new(l.text(Msg::SecondaryMixEditLocked)).small());
                    }
                });
                if let Some(error) = &self.secondary_mixes.error {
                    ui.label(RichText::new(error).color(AMBER));
                }
            });
        self.secondary_mixes.open = open;
    }
}

fn live_text(state: &WireSecondaryMixState) -> String {
    format!(
        "live {} / {} · {}/{}",
        state.program.map_or_else(|| "-".to_owned(), source_text),
        state.preview.map_or_else(|| "-".to_owned(), source_text),
        state.numerator,
        state.denominator,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_builds_the_exact_configuration() {
        let draft = SecondaryMixDraft {
            number: 3,
            name: "  Guest mix  ".into(),
            mode: WireSecondaryMixMode::PreviewProgram,
            program: "input:7".into(),
            preview: "mix:2".into(),
        };
        assert_eq!(
            draft.config().unwrap(),
            WireSecondaryMixConfig {
                number: WireMixNumber::new(3).unwrap(),
                name: "Guest mix".into(),
                mode: WireSecondaryMixMode::PreviewProgram,
                program: WireSecondaryMixSource::Input(WireInputId::new(
                    core::num::NonZeroU128::new(7).unwrap()
                )),
                preview: Some(WireSecondaryMixSource::Mix(WireMixNumber::new(2).unwrap())),
            }
        );
    }

    #[test]
    fn a_draft_refuses_main_missing_sources_and_preview_equals_program() {
        let mut draft = SecondaryMixDraft {
            number: 1,
            ..SecondaryMixDraft::default()
        };
        assert!(draft.config().is_err());
        draft.number = 2;
        draft.program = "input:0".into();
        assert!(draft.config().is_err());
        draft.program = "input:1".into();
        draft.preview = "input:1".into();
        assert!(draft.config().is_err());
        draft.mode = WireSecondaryMixMode::OutputOnly;
        assert_eq!(draft.config().unwrap().preview, None);
    }
}
