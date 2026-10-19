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
//! instructions it used. A cue's script step that ran the script reports the
//! same, naming its cue list (Protocol 2.61), and supersedes an earlier
//! stop.
//!
//! The draft follows the replicated script while it is unchanged, so an
//! edit, undo, or redo from another console shows at once; an edited draft
//! keeps the operator's text until it is saved or discarded.
//!
//! A script with `wait` runs in parts (Protocol 2.62). While its run is
//! paused at a wait the window says so, with the frame its next part runs
//! on, and offers Stop run (`script_stop`); the script cannot be saved or
//! deleted and no other script can run until the run ends. A part that
//! failed on its frame is reported with its typed reason and line (marked
//! in the editor) until Clear failed run (`script_stop`) or another run.

use egui::{Button, RichText, ScrollArea, TextEdit, Ui};
use fm_protocol::{
    MAX_SCRIPT_SOURCE_BYTES, MAX_SCRIPTS, RefusalReason, ScriptFailureKind, ScriptRunStatus,
    ScriptStatus,
};

use fm_ui_model::ScriptRunReport;

use crate::gating::Hint;
use crate::l10n::{self, Arg, Msg};
use crate::theme::{ERROR, caption, card, muted_color, section_label};
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 760.0;
const LIST_WIDTH: f32 = 180.0;
/// The space between the list and the editor, its divider in the middle.
const DIVIDER_GAP: f32 = 12.0;
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
    /// Stops the paused script run, or clears a failed run's report
    /// (`script_stop`, Protocol 2.62): an operator control, not undoable.
    Stop,
}

/// Where a refused run stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunFailure {
    pub(crate) script: u64,
    pub(crate) failure: ScriptFailureKind,
    pub(crate) line: u32,
    pub(crate) column: u32,
    /// The script's replicated last run when the refusal arrived: a later
    /// accepted run (from any console or a cue) supersedes the stop.
    pub(crate) since: Option<ScriptRunReport>,
    /// Why the server refused the run, from its English refusal text (for
    /// a refused action, the action's own refusal), when it says.
    pub(crate) detail: Option<String>,
}

/// The reason in a refused run's English refusal text
/// (`script 3 stopped at line 4:1: action refused: <reason>`), if any.
fn failure_detail(message: &str) -> Option<String> {
    let (_, stop) = message.split_once(" stopped at ")?;
    let mut parts = stop.splitn(3, ": ");
    let (_at, _kind) = (parts.next()?, parts.next()?);
    let detail = parts.next()?.trim();
    (!detail.is_empty()).then(|| detail.to_owned())
}

/// What the window reports about a script's last run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LastRun {
    /// A run this console asked for stopped here.
    Stopped(RunFailure),
    /// The last accepted run, direct or by a cue's script step.
    Ran(ScriptRunReport),
}

/// The replicated last accepted run of `script`.
fn replicated_run(state: &StudioUiState, script: u64) -> Option<ScriptRunReport> {
    state
        .project_state
        .as_ref()
        .and_then(|project| project.script_run(script))
}

/// What the window reports about `script`'s last run: where a run this
/// console asked for stopped, unless an accepted run came since, else the
/// last accepted run.
pub(crate) fn last_run(
    window: &ScriptsWindow,
    state: &StudioUiState,
    script: u64,
) -> Option<LastRun> {
    let ran = replicated_run(state, script);
    window
        .failure
        .clone()
        .filter(|failure| failure.script == script && failure.since == ran)
        .map(LastRun::Stopped)
        .or_else(|| ran.map(LastRun::Ran))
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
        if self.loaded == replicated {
            return;
        }
        match (&self.loaded, replicated) {
            // The draft's script is gone: removed here or elsewhere.
            (Some(_), None) => {
                let first = scripts.first().map(|script| script.id);
                self.select(scripts, first);
            }
            // A saved draft (new or edited) lands once its echo arrives.
            (_, Some(replicated)) if replicated == (self.name.clone(), self.source.clone()) => {
                self.loaded = Some(replicated);
                self.forget_failure(id);
            }
            // An unchanged draft follows its script.
            (Some(_), Some(_)) if !self.dirty() => {
                self.select(scripts, Some(id));
                self.forget_failure(id);
            }
            _ => {}
        }
    }

    /// Drops where `script`'s last run stopped once its saved text changes,
    /// so the editor never marks a line of a text that did not run.
    fn forget_failure(&mut self, script: u64) {
        if self
            .failure
            .as_ref()
            .is_some_and(|failure| failure.script == script)
        {
            self.failure = None;
        }
    }

    /// Notes where a run this console asked for stopped, from the refusals
    /// the session reports. Only a refused `script_run` names its script: a
    /// cue GO refused by its script step (Protocol 2.60) carries the same
    /// typed reason for a script of the cue's, not the last one run here.
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
            ) = (&refusal.reason, refusal.script_run)
            {
                self.failure = Some(RunFailure {
                    script,
                    failure: *failure,
                    line: *line,
                    column: *column,
                    since: replicated_run(state, script),
                    detail: failure_detail(&refusal.message),
                });
            }
        }
    }

    /// Drops a stop that a later accepted run of its script superseded
    /// (Protocol 2.61: a cue's script step counts), so the editor no longer
    /// marks its line.
    pub(crate) fn forget_superseded_failure(&mut self, state: &StudioUiState) {
        if let Some(failure) = &self.failure
            && replicated_run(state, failure.script) != failure.since
        {
            self.failure = None;
        }
    }
}

/// The replicated scripts, empty before a snapshot.
/// Script `script`'s replicated run state (Protocol 2.62).
#[must_use]
pub fn run_status(state: &StudioUiState, script: u64) -> ScriptRunStatus {
    state
        .project_state
        .as_ref()
        .map_or(ScriptRunStatus::Idle, |project| {
            project.script_run_status(script)
        })
}

/// The script whose run is paused at a wait, if one is.
fn paused_script(state: &StudioUiState) -> Option<u64> {
    state
        .project_state
        .as_ref()
        .and_then(fm_ui_model::ProjectState::active_script_run)
        .filter(|(_, run)| matches!(run, ScriptRunStatus::Waiting { .. }))
        .map(|(script, _)| script)
}

fn replicated(state: &StudioUiState) -> &[ScriptStatus] {
    state
        .project_state
        .as_ref()
        .map_or(&[][..], fm_ui_model::ProjectState::scripts)
}

/// Whether a cue list runs script `script` (Protocol 2.60): the authority
/// refuses to remove it.
#[must_use]
pub fn run_by_cue(state: &StudioUiState, script: u64) -> bool {
    state.project_state.as_ref().is_some_and(|project| {
        project
            .cue_lists()
            .iter()
            .flat_map(|list| &list.spec.cues)
            .flat_map(|cue| &cue.actions)
            .any(|action| matches!(action, fm_protocol::WireCueAction::RunScript { script: run } if *run == script))
    })
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
        if paused_script(state) == Some(script) {
            Some(Msg::ScriptReasonThisRunning.en())
        } else if !dirty {
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

/// Why script `script` cannot be deleted, if it cannot: the edit gate,
/// then a script not in the project yet, a run of it paused at a wait, or
/// a cue list that runs it.
#[must_use]
pub fn remove_block(state: &StudioUiState, script: u64, saved: bool) -> Option<&'static str> {
    crate::cues::edit_block(state).or_else(|| {
        if !saved {
            Some(Msg::ScriptReasonNotSaved.en())
        } else if paused_script(state) == Some(script) {
            Some(Msg::ScriptReasonThisRunning.en())
        } else if run_by_cue(state, script) {
            Some(Msg::ScriptReasonRunByCue.en())
        } else {
            None
        }
    })
}

/// Why the selected script cannot run, if it cannot: the session's
/// transition gate, then a script not in the project yet, unsaved edits,
/// a syntax error, or a script run already paused at a wait.
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
        } else if paused_script(state).is_some() {
            Some(Msg::ScriptReasonRunning.en())
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
        self.scripts.forget_superseded_failure(state);
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
    // The divider between the list and the editor is painted once the row's
    // height is known. A `ui.separator()` here took the whole available
    // height, which in an auto-sized modal is last frame's size, so the
    // window grew on every frame.
    let row = ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_WIDTH);
            draw_list(ui, state, window, scripts);
        });
        let (gap, _) = ui.allocate_exact_size(egui::vec2(DIVIDER_GAP, 0.0), egui::Sense::hover());
        ui.vertical(|ui| {
            if window.selected.is_some() {
                draw_editor(ui, state, window, scripts, intents);
            } else {
                ui.label(RichText::new(l.text(Msg::ScriptsEmpty)).color(muted_color()));
            }
        });
        gap.center().x
    });
    let divider = row.inner;
    ui.painter().vline(
        divider,
        row.response.rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
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
    let paused = paused_script(state);
    for script in scripts {
        let selected = window.selected == Some(script.id);
        let label = if paused == Some(script.id) {
            l.format(
                Msg::ScriptListRunning,
                &[("name", Arg::Str(&script.spec.name))],
            )
        } else {
            script.spec.name.clone()
        };
        if ui
            .selectable_label(selected, label)
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
/// where this script's last run stopped while the draft is unchanged: a
/// run this console asked for, or a part that failed on its frame.
fn marked_line(window: &ScriptsWindow, state: &StudioUiState) -> Option<u32> {
    if let Some(check) = fm_ui_model::check_script_syntax(&window.source) {
        return Some(check.at.line);
    }
    if window.dirty() {
        return None;
    }
    window
        .failure
        .as_ref()
        .filter(|failure| Some(failure.script) == window.selected)
        .map(|failure| failure.line)
        .or_else(
            || match window.selected.map(|script| run_status(state, script)) {
                Some(ScriptRunStatus::Faulted { line, .. }) => Some(line),
                _ => None,
            },
        )
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

    let marked = marked_line(window, state);
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
            window.failure = None;
            intents.push(StudioIntent::Script(ScriptIntent::Run { script }));
        }
        let remove = remove_block(state, script, saved);
        if ui
            .add_enabled(remove.is_none(), Button::new(l.text(Msg::ScriptRemove)))
            .hint(l.text(Msg::ScriptRemoveHover), remove.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Script(ScriptIntent::Remove { script }));
        }
    });

    draw_run_status(ui, state, script, intents);

    // The last run of this script: where it stopped, or what it did.
    match last_run(window, state, script) {
        Some(LastRun::Stopped(failure)) => {
            let mut text = l.format(
                Msg::ScriptLastFailure,
                &[
                    (
                        "failure",
                        Arg::Str(l.text(fm_ui_model::i18n::script_failure_msg(failure.failure))),
                    ),
                    ("line", Arg::Plain(u64::from(failure.line))),
                    ("column", Arg::Plain(u64::from(failure.column))),
                ],
            );
            if let Some(detail) = &failure.detail {
                text.push(' ');
                text.push_str(&l.format(Msg::ScriptFailureDetail, &[("detail", Arg::Str(detail))]));
            }
            ui.label(RichText::new(text).color(ERROR))
                .on_hover_text(l.text(Msg::ScriptLastRunHover));
        }
        Some(LastRun::Ran(ran)) => {
            let actions = l.format(
                Msg::ScriptActionCount,
                &[("count", Arg::Count(u64::from(ran.actions)))],
            );
            let instructions = l.format(
                Msg::ScriptInstructionCount,
                &[("count", Arg::Count(u64::from(ran.instructions)))],
            );
            let counts = [
                ("actions", Arg::Str(&actions)),
                ("instructions", Arg::Str(&instructions)),
                ("frame", Arg::Plain(ran.frame)),
            ];
            let text = match ran.cue_list {
                None => l.format(Msg::ScriptLastRun, &counts),
                Some(list) => {
                    let name = state
                        .project_state
                        .as_ref()
                        .and_then(|project| {
                            project.cue_lists().iter().find(|found| found.id == list)
                        })
                        .map_or_else(|| list.to_string(), |found| found.spec.name.clone());
                    let mut args = counts.to_vec();
                    args.push(("list", Arg::Str(&name)));
                    l.format(Msg::ScriptLastRunByCue, &args)
                }
            };
            ui.label(RichText::new(text).color(muted_color()))
                .on_hover_text(l.text(Msg::ScriptLastRunHover));
        }
        None => {}
    }
}

/// The selected script's paused run (with Stop run) or failed run (with
/// Clear failed run), Protocol 2.62.
fn draw_run_status(
    ui: &mut Ui,
    state: &StudioUiState,
    script: u64,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let (text, hover, color, button, button_hover) = match run_status(state, script) {
        ScriptRunStatus::Idle => return,
        ScriptRunStatus::Waiting {
            due_frame,
            waits,
            wait_frames,
            ..
        } => (
            l.format(
                Msg::ScriptRunWaiting,
                &[
                    ("frame", Arg::Plain(due_frame)),
                    ("waits", Arg::Plain(u64::from(waits))),
                    ("count", Arg::Count(u64::from(wait_frames))),
                ],
            ),
            Msg::ScriptRunWaitingHover,
            crate::theme::accent_color(),
            Msg::ScriptStop,
            Msg::ScriptStopHover,
        ),
        ScriptRunStatus::Faulted {
            frame,
            failure,
            line,
            column,
        } => (
            l.format(
                Msg::ScriptRunFaulted,
                &[
                    ("frame", Arg::Plain(frame)),
                    (
                        "failure",
                        Arg::Str(l.text(fm_ui_model::i18n::script_failure_msg(failure))),
                    ),
                    ("line", Arg::Plain(u64::from(line))),
                    ("column", Arg::Plain(u64::from(column))),
                ],
            ),
            Msg::ScriptRunFaultedHover,
            ERROR,
            Msg::ScriptClearFault,
            Msg::ScriptClearFaultHover,
        ),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(text).color(color))
            .on_hover_text(l.text(hover));
        let block = crate::cues::control_block(state);
        if ui
            .add_enabled(block.is_none(), Button::new(l.text(button)))
            .hint(l.text(button_hover), block.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Script(ScriptIntent::Stop));
        }
    });
}

#[cfg(test)]
mod tests {
    use fm_protocol::ScriptSpec;

    use super::*;

    /// A console with no replicated project: every script is idle.
    fn idle_state() -> StudioUiState {
        StudioUiState::new(crate::StudioConnectionStatus::Backoff)
    }

    fn script(id: u64, source: &str) -> ScriptStatus {
        ScriptStatus {
            id,
            spec: ScriptSpec {
                name: format!("Script {id}"),
                source: source.into(),
            },
        }
    }

    /// The Scripts window's screen rectangle over `passes` frames of the
    /// whole console on `state`, opened on its first script.
    fn window_rects(state: &StudioUiState, passes: u32) -> Vec<egui::Rect> {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        shell.open_scripts(state);
        (0..passes)
            .map(|pass| {
                context.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1920.0, 1080.0),
                    )),
                    time: Some(f64::from(pass) / 60.0),
                    ..egui::RawInput::default()
                });
                let mut ui = egui::Ui::new(
                    context.clone(),
                    egui::Id::new("scripts-window-size"),
                    egui::UiBuilder::new()
                        .layer_id(egui::LayerId::background())
                        .max_rect(context.viewport_rect()),
                );
                let _intents = shell.draw(&mut ui, state);
                drop(ui);
                let _output = context.end_pass();
                context
                    .memory(|memory| memory.area_rect(egui::Id::new("studio-scripts")))
                    .expect("the Scripts window is drawn")
            })
            .collect()
    }

    /// Regression (round 6): the vertical separator between the list and
    /// the editor took the auto-sized modal's whole available height, so
    /// the window grew taller every frame until it filled the screen and
    /// pushed its title off the top. Its size must settle and stay put.
    #[test]
    fn the_scripts_window_keeps_its_size_frame_after_frame() {
        let waiting = ScriptRunStatus::Waiting {
            started_frame: 196,
            due_frame: 1696,
            waits: 1,
            wait_frames: 1500,
        };
        for state in [
            crate::hover_audit::script_run_fixture(true, ScriptRunStatus::Idle),
            crate::hover_audit::script_run_fixture(true, waiting),
        ] {
            let rects = window_rects(&state, 120);
            let settled = rects[4];
            for (pass, rect) in rects.iter().enumerate().skip(4) {
                assert_eq!(*rect, settled, "the window changed size on pass {pass}");
            }
            assert!(
                settled.top() > 0.0 && settled.height() < 900.0,
                "the window does not fit the screen: {settled:?}"
            );
        }
    }

    /// Round 6: Delete script looked enabled while the script's run was
    /// paused at a wait, although the authority refuses it.
    #[test]
    fn a_waiting_script_cannot_be_deleted() {
        let waiting = ScriptRunStatus::Waiting {
            started_frame: 196,
            due_frame: 1696,
            waits: 1,
            wait_frames: 1500,
        };
        let state = crate::hover_audit::script_run_fixture(true, waiting);
        assert_eq!(
            remove_block(&state, 1, true),
            Some(Msg::ScriptReasonThisRunning.en())
        );
        let idle = crate::hover_audit::script_run_fixture(true, ScriptRunStatus::Idle);
        // Idle, the audit show's cue list still runs it.
        assert_eq!(
            remove_block(&idle, 1, true),
            Some(Msg::ScriptReasonRunByCue.en())
        );
        assert_eq!(
            remove_block(&idle, 1, false),
            Some(Msg::ScriptReasonNotSaved.en())
        );
    }

    /// Round 6: the run reports pluralize their counts in both languages,
    /// and the wait line says the waits add up to its frames rather than
    /// that they were already waited.
    #[test]
    fn run_reports_pluralize_their_counts() {
        let english = l10n::Locale::ENGLISH;
        let spanish = l10n::Locale::new(fm_ui_model::i18n::Language::Spanish);
        let count = |l: l10n::Locale, msg: Msg, n: u64| l.format(msg, &[("count", Arg::Count(n))]);
        assert_eq!(count(english, Msg::ScriptActionCount, 1), "1 action");
        assert_eq!(count(english, Msg::ScriptActionCount, 2), "2 actions");
        assert_eq!(
            count(english, Msg::ScriptInstructionCount, 1),
            "1 instruction"
        );
        assert_eq!(count(spanish, Msg::ScriptActionCount, 1), "1 acción");
        assert_eq!(count(spanish, Msg::ScriptActionCount, 3), "3 acciones");
        let waiting = english.format(
            Msg::ScriptRunWaiting,
            &[
                ("frame", Arg::Plain(1696)),
                ("waits", Arg::Plain(1)),
                ("count", Arg::Count(1500)),
            ],
        );
        assert!(
            waiting.contains("frame 1696") && waiting.contains("total 1,500 frames"),
            "{waiting}"
        );
        assert!(!waiting.contains("waited so far"), "{waiting}");
    }

    /// The refused action's own reason is read from the server's English
    /// refusal text when it gives one.
    #[test]
    fn a_refused_run_keeps_the_actions_reason() {
        assert_eq!(
            failure_detail(
                "script 3 stopped at line 4:1: action refused: the show has no input \"Sponsor Loop\""
            )
            .as_deref(),
            Some("the show has no input \"Sponsor Loop\"")
        );
        assert_eq!(
            failure_detail("run: script stopped at line 2, column 1"),
            None
        );
        assert_eq!(failure_detail("unrelated refusal"), None);
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

    /// Regression: saving an edit to an existing script left the draft
    /// dirty once its echo arrived (only new scripts adopted the echo), so
    /// Run stayed blocked as unsaved and the old run's stop line came back
    /// on the fixed text.
    #[test]
    fn a_saved_edit_lands_with_its_echo_and_forgets_the_old_stop() {
        let mut window = ScriptsWindow::default();
        let mut scripts = vec![script(1, "preview 2\ncut 9")];
        window.follow(&scripts);
        window.failure = Some(RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 2,
            column: 1,
            since: None,
            detail: None,
        });
        window.source = "preview 2\ncut 2".into();
        assert!(window.dirty());
        // Saved: the echo replaces the script.
        scripts[0].spec.source = "preview 2\ncut 2".into();
        window.follow(&scripts);
        assert!(!window.dirty());
        assert_eq!(window.failure, None);
        assert_eq!(marked_line(&window, &idle_state()), None);

        // Edited elsewhere while unchanged here: the stop line goes too.
        window.failure = Some(RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 2,
            column: 1,
            since: None,
            detail: None,
        });
        scripts[0].spec.source = "cut 3".into();
        window.follow(&scripts);
        assert_eq!(window.source, "cut 3");
        assert_eq!(window.failure, None);

        // An unrelated echo keeps the stop line.
        window.failure = Some(RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 1,
            column: 1,
            since: None,
            detail: None,
        });
        scripts.push(script(2, "cut 1"));
        window.follow(&scripts);
        assert!(window.failure.is_some());
    }

    /// Protocol 2.61: a cue's script step updates the window's report of
    /// that script's last run, naming the list, and supersedes where an
    /// earlier run from this console stopped; other scripts are untouched.
    #[test]
    fn a_cue_script_step_updates_the_last_run_report() {
        let state = crate::hover_audit::cue_ran_script_fixture();
        let ran = ScriptRunReport {
            script: 1,
            frame: 120,
            actions: 3,
            instructions: 7,
            cue_list: Some(1),
        };
        let mut window = ScriptsWindow::default();
        window.follow(replicated(&state));
        assert_eq!(last_run(&window, &state, 1), Some(LastRun::Ran(ran)));
        assert_eq!(last_run(&window, &state, 2), None);

        // A stop noted before the cue ran the script is superseded; a stop
        // of another script is kept.
        let stopped = RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 2,
            column: 1,
            since: None,
            detail: None,
        };
        window.failure = Some(stopped.clone());
        assert_eq!(last_run(&window, &state, 1), Some(LastRun::Ran(ran)));
        window.forget_superseded_failure(&state);
        assert_eq!(window.failure, None);
        let other = RunFailure {
            script: 2,
            ..stopped.clone()
        };
        window.failure = Some(other.clone());
        window.forget_superseded_failure(&state);
        assert_eq!(last_run(&window, &state, 2), Some(LastRun::Stopped(other)));
        // A stop noted after the cue run stands until the next run.
        let latest = RunFailure {
            since: Some(ran),
            ..stopped
        };
        window.failure = Some(latest.clone());
        window.forget_superseded_failure(&state);
        assert_eq!(last_run(&window, &state, 1), Some(LastRun::Stopped(latest)));

        // The report names the list in both languages.
        for locale in [
            l10n::Locale::ENGLISH,
            l10n::Locale::new(fm_ui_model::i18n::Language::Spanish),
        ] {
            let text = locale.format(
                Msg::ScriptLastRunByCue,
                &[
                    ("actions", Arg::Count(3)),
                    ("instructions", Arg::Count(7)),
                    ("frame", Arg::Plain(120)),
                    ("list", Arg::Str("Show open")),
                ],
            );
            assert!(text.contains("Show open") && text.contains("120"), "{text}");
        }
    }

    #[test]
    fn the_marked_line_is_the_syntax_error_or_where_the_run_stopped() {
        let mut window = ScriptsWindow::default();
        let scripts = vec![script(1, "preview 2\ncut 9")];
        window.follow(&scripts);
        assert_eq!(marked_line(&window, &idle_state()), None);
        window.failure = Some(RunFailure {
            script: 1,
            failure: ScriptFailureKind::Refused,
            line: 2,
            column: 1,
            since: None,
            detail: None,
        });
        assert_eq!(marked_line(&window, &idle_state()), Some(2));
        window.source = "preview 2\nif program = 1\nend".into();
        assert_eq!(marked_line(&window, &idle_state()), Some(2));
        window.source = "if\n".into();
        assert_eq!(marked_line(&window, &idle_state()), Some(1));
    }

    /// Regression (Protocol 2.62): editing a script whose waiting run
    /// faulted kept the fault, which then marked a line of the new source.
    /// The edit clears it, here as on the authority.
    #[test]
    fn an_edit_of_the_faulted_script_unmarks_its_line() {
        let faulted = ScriptRunStatus::Faulted {
            frame: 160,
            failure: ScriptFailureKind::Refused,
            line: 3,
            column: 1,
        };
        let state = crate::hover_audit::script_run_fixture(true, faulted);
        let mut window = ScriptsWindow::default();
        window.follow(replicated(&state));
        assert_eq!(window.selected, Some(1));
        assert_eq!(marked_line(&window, &state), Some(3));

        let source = "preview 2\ncut\nwait 5\n";
        let state = crate::hover_audit::script_edited_after_run_fixture(faulted, source);
        window.follow(replicated(&state));
        assert_eq!(window.source, source);
        assert_eq!(run_status(&state, 1), ScriptRunStatus::Idle);
        assert_eq!(marked_line(&window, &state), None);
    }

    /// Regression (Protocol 2.60): a cue GO refused by its script step
    /// carries the same typed `script_failed` reason as a refused run, and
    /// the window used to pin it on the script this console last ran,
    /// marking a line of a script that never stopped there.
    #[test]
    fn only_a_refused_run_marks_where_its_script_stopped() {
        use fm_protocol::{CommandPayload, CommandResult, CueOperation, RefusalReason};
        let refused = |id: &str, payload: &CommandPayload| {
            crate::CommandRefusalNotice::from_result(&CommandResult::Rejected {
                id: id.to_owned(),
                code: "not_found".to_owned(),
                message: format!("{id}: script stopped at line 2, column 1"),
                fields: Vec::new(),
                current_revision: 3,
                retryable: false,
                reason: Some(RefusalReason::ScriptFailed {
                    failure: ScriptFailureKind::Refused,
                    line: 2,
                    column: 1,
                }),
            })
            .unwrap()
            .with_command(payload)
        };
        let mut window = ScriptsWindow::default();
        window.follow(&[script(1, "cut 2"), script(2, "preview 2\ncut 9")]);
        let mut state = StudioUiState::new(crate::StudioConnectionStatus::Ready);

        // This console ran script 1 (accepted), then a GO's step failed in
        // another script: nothing is marked.
        let go = CommandPayload::CueControl {
            list: 7,
            operation: CueOperation::Go { cue: None },
        };
        state.command_refusals = vec![refused("go", &go)];
        window.observe_refusals(&state);
        assert_eq!(window.failure, None);

        // A refused run marks its own script.
        state.command_refusals = vec![refused("run", &CommandPayload::RunScript { script: 2 })];
        window.observe_refusals(&state);
        assert_eq!(
            window.failure,
            Some(RunFailure {
                script: 2,
                failure: ScriptFailureKind::Refused,
                line: 2,
                column: 1,
                since: None,
                detail: None,
            })
        );
    }
}
