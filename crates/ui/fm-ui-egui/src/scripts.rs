//! Edit ▸ Scripts… (RC-011, Protocol 2.58).
//!
//! A project holds up to sixteen named `FreeMix` Script sources. The window
//! lists them, edits one at a time as a draft (its name and text), and
//! checks the draft's syntax as the operator types, marking the line of the
//! first syntax error in red. Save sends the whole script (`script_set`, a
//! padlocked, undoable project edit); Delete removes it (`script_remove`).
//! Run (`script_run`, an operator control) asks the server to run the saved
//! script: every action lands on one frame, or none does. A run that stops
//! early is refused with the line and column it stopped at, which the
//! window marks in the editor; an accepted run reports how many actions and
//! instructions it used.
//!
//! The draft follows the replicated script while it is unchanged, so an
//! edit, undo, or redo from another console shows at once; an edited draft
//! keeps the operator's text until it is saved or discarded.

use egui::{Button, RichText, ScrollArea, TextEdit, Ui};
use fm_protocol::{
    MAX_SCRIPT_SOURCE_BYTES, MAX_SCRIPTS, RefusalReason, ScriptFailureKind, ScriptStatus,
};

use crate::gating::Hint;
use crate::l10n::{self, Arg, Msg};
use crate::theme::{ERROR, caption, card, muted_color, section_label};
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 760.0;
const LIST_WIDTH: f32 = 180.0;
/// Refusal identifiers remembered so each is read once.
const SEEN_REFUSALS: usize = 32;

/// A project script action. Studio maps each to one protocol command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptIntent {
    /// Creates or replaces one script (`script_set`): an undoable project
    /// edit.
    Set {
        script: u64,
        name: String,
        source: String,
    },
    /// Deletes one script (`script_remove`): an undoable project edit.
    Remove { script: u64 },
    /// Runs one saved script (`script_run`): an operator control, not
    /// undoable.
    Run { script: u64 },
}

/// Where a refused run stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunFailure {
    pub(crate) script: u64,
    pub(crate) failure: ScriptFailureKind,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

/// The window's open state and the draft being edited.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScriptsWindow {
    pub(crate) open: bool,
    /// The script the draft edits; a new script's id until it is saved.
    pub(crate) selected: Option<u64>,
    pub(crate) name: String,
    pub(crate) source: String,
    /// The replicated name and text the draft was loaded from (`None` for
    /// a new script), so an unchanged draft follows the project.
    loaded: Option<(String, String)>,
    /// The script this console last asked to run.
    last_run: Option<u64>,
    pub(crate) failure: Option<RunFailure>,
    seen_refusals: Vec<String>,
}

impl ScriptsWindow {
    /// Opens the window on `script` (or a new draft for a project without
    /// scripts).
    pub(crate) fn select(&mut self, scripts: &[ScriptStatus], script: Option<u64>) {
        let found = script.and_then(|id| scripts.iter().find(|candidate| candidate.id == id));
        if let Some(found) = found {
            self.selected = Some(found.id);
            self.name.clone_from(&found.spec.name);
            self.source.clone_from(&found.spec.source);
            self.loaded = Some((found.spec.name.clone(), found.spec.source.clone()));
        } else {
            self.selected = None;
            self.name.clear();
            self.source.clear();
            self.loaded = None;
        }
    }

    /// Starts a new, empty draft under the next free id.
    pub(crate) fn new_draft(&mut self, scripts: &[ScriptStatus]) {
        let next = scripts.iter().map(|script| script.id).max().unwrap_or(0) + 1;
        self.selected = Some(next);
        self.name = format!("Script {next}");
        self.source.clear();
        self.loaded = None;
    }

    /// Whether the draft differs from what it was loaded from.
    pub(crate) fn dirty(&self) -> bool {
        self.loaded.as_ref() != Some(&(self.name.clone(), self.source.clone()))
    }

    /// Keeps the draft in step with the replicated scripts: an unchanged
    /// draft follows its script, a removed one moves to the first script,
    /// and a just-saved new script becomes the loaded one.
    pub(crate) fn follow(&mut self, scripts: &[ScriptStatus]) {
        let Some(id) = self.selected else {
            if let Some(first) = scripts.first() {
                self.select(scripts, Some(first.id));
            }
            return;
        };
        let replicated = scripts
            .iter()
            .find(|script| script.id == id)
            .map(|script| (script.spec.name.clone(), script.spec.source.clone()));
        match (&self.loaded, replicated) {
            // The draft's script is gone: removed here or elsewhere.
            (Some(_), None) => {
                let first = scripts.first().map(|script| script.id);
                self.select(scripts, first);
            }
            // An unchanged draft follows its script.
            (Some(loaded), Some(replicated)) if *loaded != replicated && !self.dirty() => {
                self.select(scripts, Some(id));
            }
            // A new draft lands once its echo arrives.
            (None, Some(replicated)) if replicated == (self.name.clone(), self.source.clone()) => {
                self.loaded = Some(replicated);
            }
            _ => {}
        }
    }

    /// Notes where a run this console asked for stopped, from the refusals
    /// the session reports.
    pub(crate) fn observe_refusals(&mut self, state: &StudioUiState) {
        for refusal in &state.command_refusals {
            if self.seen_refusals.contains(&refusal.command_id) {
                continue;
            }
            if self.seen_refusals.len() == SEEN_REFUSALS {
                self.seen_refusals.remove(0);
            }
            self.seen_refusals.push(refusal.command_id.clone());
            if let (
                Some(RefusalReason::ScriptFailed {
                    failure,
                    line,
                    column,
                }),
                Some(script),
            ) = (&refusal.reason, self.last_run)
            {
                self.failure = Some(RunFailure {
                    script,
                    failure: *failure,
                    line: *line,
                    column: *column,
                });
            }
        }
    }
}

/// The replicated scripts, empty before a snapshot.
fn replicated(state: &StudioUiState) -> &[ScriptStatus] {
    state
        .project_state
        .as_ref()
        .map_or(&[][..], fm_ui_model::ProjectState::scripts)
}

/// Why the draft cannot be saved, if it cannot.
#[must_use]
pub fn save_block(
    state: &StudioUiState,
    scripts: &[ScriptStatus],
    script: u64,
    name: &str,
    source: &str,
    dirty: bool,
) -> Option<&'static str> {
    crate::cues::edit_block(state).or_else(|| {
        if !dirty {
            Some(Msg::ScriptReasonUnchanged.en())
        } else if !fm_protocol::script_name_is_valid(name) {
            Some(Msg::ScriptReasonName.en())
        } else if !fm_protocol::script_source_is_valid(source) {
            Some(Msg::ScriptReasonTooLong.en())
        } else if scripts.len() >= MAX_SCRIPTS && !scripts.iter().any(|found| found.id == script) {
            Some(Msg::ScriptReasonLimit.en())
        } else {
            None
        }
    })
}

/// Why the selected script cannot run, if it cannot: the session's
/// transition gate, then a script not in the project yet, unsaved edits,
/// or a syntax error.
#[must_use]
pub fn run_block(
    state: &StudioUiState,
    saved: bool,
    dirty: bool,
    source: &str,
) -> Option<&'static str> {
    crate::cues::control_block(state).or_else(|| {
        if !saved {
            Some(Msg::ScriptReasonNotSaved.en())
        } else if dirty {
            Some(Msg::ScriptReasonUnsaved.en())
        } else if fm_ui_model::check_script_syntax(source).is_some() {
            Some(Msg::ScriptReasonSyntax.en())
        } else {
            None
        }
    })
}

impl StudioShell {
    /// Opens the Scripts window on the first script.
    pub(crate) fn open_scripts(&mut self, state: &StudioUiState) {
        let scripts = replicated(state);
        if !self.scripts.open {
            let selected = self
                .scripts
                .selected
                .filter(|id| scripts.iter().any(|script| script.id == *id))
                .or_else(|| scripts.first().map(|script| script.id));
            self.scripts.select(scripts, selected);
        }
        self.scripts.open = true;
    }

    /// Draws the Scripts window when open.
    pub(crate) fn draw_scripts_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.scripts.observe_refusals(state);
        if !self.scripts.open {
            return;
        }
        let l = self.locale;
        let window = &mut self.scripts;
        window.follow(replicated(state));
        let close = crate::menu::help_modal(
            context,
            "studio-scripts",
            l.text(Msg::ScriptsTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, window, intents),
        );
        if close {
            self.scripts.open = false;
        }
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ScriptsWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(caption(l.text(Msg::ScriptsIntro)));
    ui.add_space(6.0);
    let scripts = replicated(state);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_WIDTH);
            draw_list(ui, state, window, scripts);
        });
        ui.separator();
        ui.vertical(|ui| {
            if window.selected.is_some() {
                draw_editor(ui, state, window, scripts, intents);
            } else {
                ui.label(RichText::new(l.text(Msg::ScriptsEmpty)).color(muted_color()));
            }
        });
    });
    ui.add_space(6.0);
    ui.label(caption(l.text(Msg::ScriptReference)));
}

fn draw_list(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ScriptsWindow,
    scripts: &[ScriptStatus],
) {
    let l = l10n::locale(ui.ctx());
    ui.label(section_label(l.text(Msg::ScriptsTitle)));
    if scripts.is_empty() {
        ui.label(RichText::new(l.text(Msg::ScriptsEmpty)).color(muted_color()));
    }
    for script in scripts {
        let selected = window.selected == Some(script.id);
        if ui
            .selectable_label(selected, &script.spec.name)
            .on_hover_text(l.format(
                Msg::ScriptListHover,
                &[("name", Arg::Str(&script.spec.name))],
            ))
            .clicked()
            && !selected
        {
            window.select(scripts, Some(script.id));
        }
    }
    ui.add_space(4.0);
    let block = crate::cues::edit_block(state)
        .or_else(|| (scripts.len() >= MAX_SCRIPTS).then_some(Msg::ScriptReasonLimit.en()));
    if ui
        .add_enabled(block.is_none(), Button::new(l.text(Msg::ScriptNew)))
        .hint(l.text(Msg::ScriptNewHover), block.unwrap_or(""))
        .clicked()
    {
        window.new_draft(scripts);
    }
}

/// The line to mark in the editor: the draft's first syntax error, else
/// where this script's last run stopped while the draft is unchanged.
fn marked_line(window: &ScriptsWindow) -> Option<u32> {
    if let Some(check) = fm_ui_model::check_script_syntax(&window.source) {
        return Some(check.at.line);
    }
    window
        .failure
        .filter(|failure| Some(failure.script) == window.selected && !window.dirty())
        .map(|failure| failure.line)
}

#[allow(clippy::too_many_lines)] // one immediate-mode editor
fn draw_editor(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ScriptsWindow,
    scripts: &[ScriptStatus],
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let Some(script) = window.selected else {
        return;
    };
    let saved = scripts.iter().any(|found| found.id == script);
    let edit = crate::cues::edit_block(state);
    ui.horizontal(|ui| {
        ui.label(l.text(Msg::ScriptNameLabel));
        ui.add_enabled(
            edit.is_none(),
            TextEdit::singleline(&mut window.name)
                .char_limit(fm_protocol::MAX_SCRIPT_NAME_BYTES)
                .desired_width(260.0),
        )
        .hint(l.text(Msg::ScriptNameHover), edit.unwrap_or(""));
    });
    ui.add_space(4.0);

    let marked = marked_line(window);
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().widgets.inactive.text_color();
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap_width;
        for (index, line) in text.as_str().split_inclusive('\n').enumerate() {
            let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let mut format = egui::TextFormat::simple(font.clone(), color);
            if marked == Some(number) {
                format.background = ERROR.gamma_multiply(0.35);
                format.underline = egui::Stroke::new(1.0, ERROR);
            }
            job.append(line, 0.0, format);
        }
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ScrollArea::vertical()
            .id_salt("studio-script-source")
            .max_height(300.0)
            .show(ui, |ui| {
                ui.add_enabled(
                    edit.is_none(),
                    TextEdit::multiline(&mut window.source)
                        .code_editor()
                        .char_limit(MAX_SCRIPT_SOURCE_BYTES)
                        .desired_rows(14)
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter),
                )
                .hint(l.text(Msg::ScriptSourceHover), edit.unwrap_or(""));
            });
    });

    match fm_ui_model::check_script_syntax(&window.source) {
        Some(check) => ui.label(
            RichText::new(l.format(
                Msg::ScriptSyntaxError,
                &[
                    ("line", Arg::Plain(u64::from(check.at.line))),
                    ("column", Arg::Plain(u64::from(check.at.column))),
                    ("message", Arg::Str(&check.message)),
                ],
            ))
            .color(ERROR),
        ),
        None => ui.label(RichText::new(l.text(Msg::ScriptSyntaxOk)).color(muted_color())),
    };

    let dirty = window.dirty();
    ui.horizontal(|ui| {
        let save = save_block(state, scripts, script, &window.name, &window.source, dirty);
        if ui
            .add_enabled(save.is_none(), Button::new(l.text(Msg::ScriptSave)))
            .hint(l.text(Msg::ScriptSaveHover), save.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Script(ScriptIntent::Set {
                script,
                name: window.name.clone(),
                source: window.source.clone(),
            }));
        }
        let revert = (!dirty).then_some(Msg::ScriptReasonUnchanged.en());
        if ui
            .add_enabled(revert.is_none(), Button::new(l.text(Msg::ScriptRevert)))
            .hint(l.text(Msg::ScriptRevertHover), revert.unwrap_or(""))
            .clicked()
        {
            let keep = saved.then_some(script);
            window.select(
                scripts,
                keep.or_else(|| scripts.first().map(|found| found.id)),
            );
        }
        let run = run_block(state, saved, dirty, &window.source);
        if ui
            .add_enabled(run.is_none(), Button::new(l.text(Msg::ScriptRun)))
            .hint(l.text(Msg::ScriptRunHover), run.unwrap_or(""))
            .clicked()
        {
            window.last_run = Some(script);
            window.failure = None;
            intents.push(StudioIntent::Script(ScriptIntent::Run { script }));
        }
        let remove = edit.or_else(|| (!saved).then_some(Msg::ScriptReasonNotSaved.en()));
        if ui
            .add_enabled(remove.is_none(), Button::new(l.text(Msg::ScriptRemove)))
            .hint(l.text(Msg::ScriptRemoveHover), remove.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Script(ScriptIntent::Remove { script }));
        }
    });

    // The last run of this script: where it stopped, or what it did.
    if let Some(failure) = window.failure.filter(|failure| failure.script == script) {
        ui.label(
            RichText::new(l.format(
                Msg::ScriptLastFailure,
                &[
                    (
                        "failure",
                        Arg::Str(l.text(fm_ui_model::i18n::script_failure_msg(failure.failure))),
                    ),
                    ("line", Arg::Plain(u64::from(failure.line))),
                    ("column", Arg::Plain(u64::from(failure.column))),
                ],
            ))
            .color(ERROR),
        );
    } else if let Some(ran) = state
        .project_state
        .as_ref()
        .and_then(fm_ui_model::ProjectState::last_script_run)
        .filter(|ran| ran.script == script)
    {
        ui.label(
            RichText::new(l.format(
                Msg::ScriptLastRun,
                &[
                    ("actions", Arg::Count(u64::from(ran.actions))),
                    ("instructions", Arg::Count(u64::from(ran.instructions))),
                    ("frame", Arg::Plain(ran.frame)),
                ],
            ))
            .color(muted_color()),
        );
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::ScriptSpec;

    use super::*;

    fn script(id: u64, source: &str) -> ScriptStatus {
        ScriptStatus {
            id,
            spec: ScriptSpec {
                name: format!("Script {id}"),
                source: source.into(),
            },
        }
    }

    #[test]
    fn an_unchanged_draft_follows_its_script_and_an_edited_one_does_not() {
        let mut window = ScriptsWindow::default();
        let mut scripts = vec![script(1, "cut 2"), script(2, "cut 1")];
        window.follow(&scripts);
        assert_eq!(
            (window.selected, window.source.as_str()),
            (Some(1), "cut 2")
        );
        assert!(!window.dirty());

        // Changed elsewhere: the unchanged draft follows.
        scripts[0].spec.source = "cut 3".into();
        window.follow(&scripts);
        assert_eq!(window.source, "cut 3");

        // Edited here: the draft is kept.
        window.source = "preview 2".into();
        scripts[0].spec.source = "cut 4".into();
        window.follow(&scripts);
        assert_eq!(window.source, "preview 2");
        assert!(window.dirty());

        // Removed: the window moves to the first script left.
        scripts.remove(0);
        window.follow(&scripts);
        assert_eq!(window.selected, Some(2));
    }

    #[test]
    fn a_new_draft_takes_the_next_id_and_lands_with_its_echo() {
        let mut window = ScriptsWindow::default();
        let mut scripts = vec![script(4, "cut 2")];
        window.new_draft(&scripts);
        assert_eq!(
            (window.selected, window.name.as_str()),
            (Some(5), "Script 5")
        );
        assert!(window.dirty());
        window.source = "cut 1".into();
        window.follow(&scripts);
        assert_eq!(window.selected, Some(5));
        scripts.push(script(5, "cut 1"));
        window.follow(&scripts);
        assert!(!window.dirty());
    }

    #[test]
    fn the_marked_line_is_the_syntax_error_or_where_the_run_stopped() {
        let mut window = ScriptsWindow::default();
        let scripts = vec![script(1, "preview 2\ncut 9")];
        window.follow(&scripts);
        assert_eq!(marked_line(&window), None);
        window.failure = Some(RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 2,
            column: 1,
        });
        assert_eq!(marked_line(&window), Some(2));
        window.source = "preview 2\nif program = 1\nend".into();
        assert_eq!(marked_line(&window), Some(2));
        window.source = "if\n".into();
        assert_eq!(marked_line(&window), Some(1));
    }
}
