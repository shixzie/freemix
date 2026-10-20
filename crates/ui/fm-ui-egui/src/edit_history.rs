//! Undo, redo, and the project edit padlock.
//!
//! Pure view-model helpers decide availability and wording; the menu, the top
//! bar, and the keyboard handler only draw or read keys. History lives on the
//! server and is shared by every editor, so Undo and Redo always name the
//! edit the operator saw ([`fm_protocol::EditHistoryStatus::next_undo`]) and
//! the server refuses a different one instead of reverting someone else's
//! later work.

use core::num::NonZeroU128;
use egui::{Event, Key, Ui};

use fm_protocol::{CommandResult, FieldIssue, RefusalReason};
use fm_types::InputId;
use fm_ui_model::ClientView;

use crate::gating::{Capability, unavailable_reason};
use crate::input_bank::project_editing_available;
use crate::l10n::{Arg, Locale, Msg};
use crate::{StudioIntent, StudioUiState};

/// Keyboard shortcut text for Undo.
pub const UNDO_SHORTCUT: &str = "Ctrl+Z";
/// Keyboard shortcut text for Redo.
pub const REDO_SHORTCUT: &str = "Ctrl+Shift+Z";

pub(crate) const PROJECT_LOCKED_REASON: &str = Msg::ReasonProjectLocked.en();
pub(crate) const INPUT_LOCKED_REASON: &str = Msg::ReasonInputLocked.en();
pub(crate) const NOTHING_TO_UNDO_REASON: &str = Msg::ReasonNothingToUndo.en();
pub(crate) const NOTHING_TO_REDO_REASON: &str = Msg::ReasonNothingToRedo.en();

/// Which side of the shared history an action walks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryVerb {
    Undo,
    Redo,
}

/// Availability of one history action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryActionAvailability {
    /// The edit revision the action targets; `Some` exactly when enabled.
    pub expected_edit: Option<u64>,
    /// Why the action is unavailable; empty when enabled.
    pub reason: &'static str,
    /// Whether the project padlock is what blocks the action.
    pub locked: bool,
    /// Entries on this side of the shared history.
    pub depth: u16,
}

impl HistoryActionAvailability {
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.expected_edit.is_some()
    }

    const fn available(edit: u64, depth: u16) -> Self {
        Self {
            expected_edit: Some(edit),
            reason: "",
            locked: false,
            depth,
        }
    }

    const fn unavailable(reason: &'static str, locked: bool, depth: u16) -> Self {
        Self {
            expected_edit: None,
            reason,
            locked,
            depth,
        }
    }

    /// Menu label: the verb plus the revision it reverts or reapplies, so the
    /// operator sees which shared edit the action targets.
    #[must_use]
    pub fn menu_label(self, locale: Locale, verb: HistoryVerb) -> String {
        let (plain, at) = match verb {
            HistoryVerb::Undo => (Msg::Undo, Msg::UndoEditAt),
            HistoryVerb::Redo => (Msg::Redo, Msg::RedoEditAt),
        };
        match self.expected_edit {
            Some(edit) => locale.format(at, &[("revision", Arg::Plain(edit))]),
            None => locale.text(plain).to_owned(),
        }
    }

    /// Hover text: what the action does, its key, and the history depth.
    #[must_use]
    pub fn hover(self, locale: Locale, verb: HistoryVerb, shortcut: &str) -> String {
        let steps = match self.depth {
            0 => locale.text(Msg::HistoryEmpty).to_owned(),
            depth => locale.format(Msg::HistorySteps, &[("count", Arg::from(depth))]),
        };
        let (plain, at) = match verb {
            HistoryVerb::Undo => (Msg::Undo, Msg::UndoHoverAt),
            HistoryVerb::Redo => (Msg::Redo, Msg::RedoHoverAt),
        };
        let shortcut = locale.keys(shortcut);
        match self.expected_edit {
            Some(edit) => format!(
                "{}  [{shortcut}]\n{steps}",
                locale.format(at, &[("revision", Arg::Plain(edit))])
            ),
            None => format!(
                "{}  [{shortcut}]\n{steps}\n{}",
                locale.text(plain),
                crate::l10n::unavailable(locale, self.reason)
            ),
        }
    }
}

/// Undo and Redo availability for one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditHistoryAvailability {
    pub undo: HistoryActionAvailability,
    pub redo: HistoryActionAvailability,
}

impl EditHistoryAvailability {
    /// The Undo intent, when Undo is available.
    #[must_use]
    pub const fn undo_intent(self) -> Option<StudioIntent> {
        match self.undo.expected_edit {
            Some(expected_edit) => Some(StudioIntent::Undo { expected_edit }),
            None => None,
        }
    }

    /// The Redo intent, when Redo is available.
    #[must_use]
    pub const fn redo_intent(self) -> Option<StudioIntent> {
        match self.redo.expected_edit {
            Some(expected_edit) => Some(StudioIntent::Redo { expected_edit }),
            None => None,
        }
    }
}

/// Computes Undo and Redo availability in the order an operator would fix
/// it: connection, synchronized state, `edit_project` permission, the
/// project padlock, then an empty history.
#[must_use]
pub fn edit_history_availability(state: &StudioUiState) -> EditHistoryAvailability {
    let blocked = match &state.view {
        _ if !project_editing_available(state) => {
            Some((unavailable_reason(state, Capability::EditProject), false))
        }
        Some(view) if view.edit_locks.project => Some((PROJECT_LOCKED_REASON, true)),
        _ => None,
    };
    let history = state
        .view
        .as_ref()
        .map(|view| view.edit_history)
        .unwrap_or_default();
    let side = |next: Option<u64>, depth: u16, empty: &'static str| match (blocked, next) {
        (Some((reason, locked)), _) => {
            HistoryActionAvailability::unavailable(reason, locked, depth)
        }
        (None, None) => HistoryActionAvailability::unavailable(empty, false, depth),
        (None, Some(edit)) => HistoryActionAvailability::available(edit, depth),
    };
    EditHistoryAvailability {
        undo: side(
            history.next_undo,
            history.undo_depth,
            NOTHING_TO_UNDO_REASON,
        ),
        redo: side(
            history.next_redo,
            history.redo_depth,
            NOTHING_TO_REDO_REASON,
        ),
    }
}

/// The top-bar padlock: current project lock state and whether it toggles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PadlockToggle {
    /// Whether the project-wide edit padlock is engaged.
    pub locked: bool,
    /// Whether this session may toggle it now.
    pub enabled: bool,
    /// Why toggling is unavailable; empty when enabled.
    pub reason: &'static str,
}

impl PadlockToggle {
    /// The intent a click emits, when toggling is available.
    #[must_use]
    pub const fn intent(self) -> Option<StudioIntent> {
        if self.enabled {
            Some(StudioIntent::SetProjectLock {
                locked: !self.locked,
            })
        } else {
            None
        }
    }

    /// Short top-bar label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        if self.locked {
            Msg::Locked.en()
        } else {
            Msg::Unlocked.en()
        }
    }

    /// Hover text describing the state and the click action.
    #[must_use]
    pub const fn hover(self) -> &'static str {
        if self.locked {
            Msg::PadlockHoverLocked.en()
        } else {
            Msg::PadlockHoverUnlocked.en()
        }
    }

    /// Edit menu wording for the toggle.
    #[must_use]
    pub const fn menu_label(self) -> &'static str {
        if self.locked {
            Msg::PadlockMenuUnlock.en()
        } else {
            Msg::PadlockMenuLock.en()
        }
    }
}

/// Computes the padlock state and whether this session may toggle it. The
/// padlock is an `edit_project` action and is never blocked by itself.
#[must_use]
pub fn padlock_toggle(state: &StudioUiState) -> PadlockToggle {
    let locked = state
        .view
        .as_ref()
        .is_some_and(|view| view.edit_locks.project);
    let enabled = project_editing_available(state);
    PadlockToggle {
        locked,
        enabled,
        reason: if enabled {
            ""
        } else {
            unavailable_reason(state, Capability::EditProject)
        },
    }
}

/// Console-wide padlock facts that badge editable controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditLockSummary {
    /// The project-wide padlock is engaged.
    pub project: bool,
    /// Inputs with their own padlock engaged.
    pub locked_inputs: usize,
}

impl EditLockSummary {
    /// Whether any padlock is engaged.
    #[must_use]
    pub const fn any(self) -> bool {
        self.project || self.locked_inputs > 0
    }

    /// Short chip text for the input bank header, if any lock applies.
    #[must_use]
    pub fn bank_chip(self, locale: Locale) -> Option<String> {
        if self.project {
            Some(locale.text(Msg::EditsLocked).to_owned())
        } else if self.locked_inputs > 0 {
            Some(locale.format(
                Msg::InputsLockedChip,
                &[("count", Arg::from(self.locked_inputs))],
            ))
        } else {
            None
        }
    }

    /// One-paragraph explanation of what the padlocks block and keep.
    #[must_use]
    pub fn detail(self, locale: Locale) -> String {
        let inputs = match self.locked_inputs {
            0 => String::new(),
            count => locale.format(Msg::InputsLockedDetail, &[("count", Arg::from(count))]),
        };
        let lead = locale.text(if self.project {
            Msg::ProjectEditsLockedDetail
        } else {
            Msg::ProjectEditsUnlocked
        });
        match (self.project, inputs.is_empty()) {
            (_, true) => lead.to_owned(),
            (true, false) => format!("{lead}\n{inputs}"),
            (false, false) => format!("{lead} {inputs}"),
        }
    }
}

/// Summarizes the replicated padlocks; empty without synchronized state.
#[must_use]
pub fn edit_lock_summary(state: &StudioUiState) -> EditLockSummary {
    state
        .view
        .as_ref()
        .map_or_else(EditLockSummary::default, |view| EditLockSummary {
            project: view.edit_locks.project,
            locked_inputs: view.edit_locks.inputs.len(),
        })
}

/// Returns why Stinger slot setup is blocked by the project padlock, if it is.
/// Slot configuration is a project edit; firing a Stinger is live switching
/// and is never locked.
#[must_use]
pub fn stinger_edit_lock_reason(state: &StudioUiState) -> Option<&'static str> {
    state
        .view
        .as_ref()
        .is_some_and(|view| view.edit_locks.project)
        .then_some(PROJECT_LOCKED_REASON)
}

/// Returns why one input's rename or move is blocked by a padlock, if it is.
#[must_use]
pub fn input_edit_lock_reason(state: &StudioUiState, input: InputId) -> Option<&'static str> {
    let locks = &state.view.as_ref()?.edit_locks;
    if locks.project {
        Some(PROJECT_LOCKED_REASON)
    } else if locks.is_input_locked(input) {
        Some(INPUT_LOCKED_REASON)
    } else {
        None
    }
}

/// A refused command as the console words it (Protocol 2.45): published
/// once with its rejection, toasted in the console language from its typed
/// reason (or a field-issue code the console words). A refusal without
/// either keeps the worker's English text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRefusalNotice {
    /// The refused command's identifier; each refusal is announced once.
    pub command_id: String,
    /// Top-level rejection code (`conflict`, `not_found`, ...).
    pub code: String,
    /// The daemon's English text, the fallback wording.
    pub message: String,
    pub reason: Option<RefusalReason>,
    /// The stable code of the rejection's first field issue, if any.
    pub issue: Option<String>,
    /// The worker's English error line for this rejection
    /// ([`command_rejection_message`]): the part of
    /// [`StudioUiState::error`] this refusal explains. Anything else joined
    /// onto that error is not explained by the refusal and is shown too.
    pub error: String,
    /// The script a refused `script_run` asked to run, set by the worker
    /// that sent the command ([`Self::with_command`]). A cue GO whose
    /// script step fails carries the same typed `script_failed` reason
    /// (Protocol 2.60) for a script the console cannot tell, so only this
    /// says which script a stop belongs to.
    pub script_run: Option<u64>,
}

impl CommandRefusalNotice {
    /// The notice of a rejected command result; `None` for an acceptance.
    #[must_use]
    pub fn from_result(result: &CommandResult) -> Option<Self> {
        match result {
            CommandResult::Rejected {
                id,
                code,
                message,
                fields,
                reason,
                ..
            } => Some(Self {
                command_id: id.clone(),
                code: code.clone(),
                message: message.clone(),
                reason: reason.clone(),
                issue: fields.first().map(|issue| issue.code.clone()),
                error: command_rejection_message(code, message, fields),
                script_run: None,
            }),
            CommandResult::Accepted { .. } => None,
        }
    }

    /// The notice of a refused command `payload`: a `script_run` notes the
    /// script it asked to run ([`Self::script_run`]).
    #[must_use]
    pub fn with_command(mut self, payload: &fm_protocol::CommandPayload) -> Self {
        if let fm_protocol::CommandPayload::RunScript { script } = payload {
            self.script_run = Some(*script);
        }
        self
    }

    /// The console's own wording of an untyped field issue, if it has one.
    fn issue_msg(&self) -> Option<Msg> {
        match self.issue.as_deref()? {
            "unsupported_scheme" => Some(Msg::RsnUnsupportedScheme),
            _ => None,
        }
    }

    /// Whether the console words this refusal itself (from a typed reason
    /// or a known field issue) rather than showing the worker's English.
    #[must_use]
    pub fn is_localized(&self) -> bool {
        self.reason.is_some() || self.issue_msg().is_some()
    }

    /// The refusal's toast: worded in the console language when it can be,
    /// else the worker's English error line.
    #[must_use]
    pub fn toast_text(&self, l: Locale, view: Option<&ClientView>) -> String {
        if self.is_localized() {
            self.text(l, view)
        } else {
            self.error.clone()
        }
    }

    /// What `error` holds beyond this refusal's own line: `None` when the
    /// error is not this refusal's at all, `Some(None)` when the refusal
    /// explains all of it, and `Some(Some(rest))` for the text joined onto
    /// it (a persistent error, a deferred-queue overflow).
    #[must_use]
    pub fn remainder_of(&self, error: &str) -> Option<Option<String>> {
        let line = self.error.as_str();
        if line.is_empty() {
            return None;
        }
        if error == line {
            return Some(None);
        }
        let parts = error.split("; ").collect::<Vec<_>>();
        let width = line.split("; ").count();
        let at = parts
            .windows(width)
            .position(|window| window.join("; ") == line)?;
        let rest = parts[..at]
            .iter()
            .chain(&parts[at + width..])
            .copied()
            .collect::<Vec<_>>()
            .join("; ");
        Some((!rest.is_empty()).then_some(rest))
    }

    /// The refusal in the console language, naming inputs from `view`.
    #[must_use]
    pub fn text(&self, l: Locale, view: Option<&ClientView>) -> String {
        let names = |input: u128| {
            let input = InputId::new(NonZeroU128::new(input)?);
            let view = view?;
            view.inputs
                .iter()
                .position(|candidate| *candidate == input)
                .and_then(|index| view.input_names.get(index).cloned())
        };
        if self.reason.is_none()
            && let Some(msg) = self.issue_msg()
        {
            return l.format(Msg::CommandRefused, &[("reason", Arg::Str(l.text(msg)))]);
        }
        fm_ui_model::i18n::command_refusal_text(l, &self.message, self.reason.as_ref(), &names)
    }

    /// The stable code behind this refusal: the typed reason's kind, else
    /// the field issue's code, else the top-level rejection code.
    #[must_use]
    pub fn reason_code(&self) -> &str {
        match (&self.reason, self.issue.as_deref()) {
            (Some(reason), _) => reason.kind(),
            (None, Some(issue)) => issue,
            (None, None) => &self.code,
        }
    }
}

/// Formats a command rejection for the error toast.
///
/// Undo, redo, padlock, and input add/remove refusals carry a stable reason
/// code in their first field issue (`project_locked`, `input_locked`,
/// `nothing_to_undo`, `nothing_to_redo`, `history_conflict`, `history_drift`,
/// `input_in_use`, `duplicate_input`, `too_many_inputs`, `duplicate_name`,
/// `invalid_name`); the toast and status bar lead
/// with that code and an operator explanation. Other rejections keep the
/// generic `Command rejected (<code>): <message>` form.
#[must_use]
pub fn command_rejection_message(code: &str, message: &str, fields: &[FieldIssue]) -> String {
    let Some(issue) = fields.first() else {
        return format!("Command rejected ({code}): {message}");
    };
    let advice = match issue.code.as_str() {
        "project_locked" => "Unlock the padlock in the top bar to edit; switching still works.",
        "input_locked" => "This input's padlock blocks renaming, moving, and removing it.",
        "input_in_use" => "Take the input off air or out of its slot first.",
        "duplicate_input" => "Another input already has this id; try again.",
        "too_many_inputs" => "The show is at its input limit; remove an input first.",
        "duplicate_name" => "Another input already uses this name.",
        "invalid_name" => "Use a non-blank name of at most 128 bytes.",
        "nothing_to_undo" => "There is nothing left to undo.",
        "nothing_to_redo" => "There is nothing left to redo.",
        "history_conflict" => "Someone else edited first; their edit is next in the history.",
        "history_drift" => "The edited setting changed since; undo or redo it by hand.",
        _ => return format!("Command rejected ({code}): {message}"),
    };
    format!("Edit refused ({}): {} {advice}", issue.code, issue.message)
}

/// Handles Ctrl+Z (Undo) and Ctrl+Shift+Z (Redo), using the platform command
/// key. Keys are consumed only when their action is available, and never
/// while a text field owns the keyboard (its own undo wins there).
pub(crate) fn history_shortcuts(
    ui: &mut Ui,
    availability: EditHistoryAvailability,
) -> Vec<StudioIntent> {
    if ui.ctx().egui_wants_keyboard_input() {
        return Vec::new();
    }
    ui.input_mut(|input| {
        let mut intents = Vec::new();
        input.events.retain(|event| {
            let Event::Key {
                key: Key::Z,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            if !modifiers.command || modifiers.alt {
                return true;
            }
            let intent = if modifiers.shift {
                availability.redo_intent()
            } else {
                availability.undo_intent()
            };
            intent.is_none_or(|intent| {
                intents.push(intent);
                false
            })
        });
        intents
    })
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use egui::Modifiers;
    use fm_protocol::EditHistoryStatus;
    use fm_ui_model::{ClientModel, EditLocks, ProjectSnapshot};

    use super::*;
    use crate::StudioConnectionStatus;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn view(locks: &EditLocks, history: EditHistoryStatus) -> fm_ui_model::ClientView {
        let project = fm_types::ProjectId::new(NonZeroU128::new(1).unwrap());
        let mut snapshot = fm_protocol::SnapshotMessage {
            audio_mixer: fm_protocol::AudioMixerStatus::default(),
            fade_to_black_excluded_outputs: Vec::new(),
            fade_to_black_output_durations: Vec::new(),
            fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
            display_outputs: Vec::new(),
            display_output_states: Vec::new(),
            network_streams: Vec::new(),
            system_audio_inputs: Vec::new(),
            screen_capture_inputs: Vec::new(),
            capture_inputs: Vec::new(),
            engine: fm_protocol::EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 9,
            show_name: "Show".into(),
            inputs: [(1, "Cam"), (2, "Slides")]
                .map(|(id, name)| fm_protocol::InputStatus {
                    input: fm_protocol::WireInputId::from_domain(input(id)),
                    name: name.into(),
                    lifecycle: fm_protocol::InputLifecycleStatus::default(),
                })
                .to_vec(),
            outputs: Vec::new(),
            input_audio_strips: [1, 2]
                .map(|id| fm_protocol::InputAudioStripStatus {
                    pan_basis_points: 0,
                    group: None,
                    input: fm_protocol::WireInputId::from_domain(input(id)),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .to_vec(),
            desired_program: fm_protocol::WireInputId::from_domain(input(1)),
            desired_preview: fm_protocol::WireInputId::from_domain(input(2)),
            realized_program: fm_protocol::WireInputId::from_domain(input(1)),
            realized_preview: fm_protocol::WireInputId::from_domain(input(2)),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| fm_protocol::RecordingStatus {
                    recorder: fm_protocol::WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: fm_protocol::OverlayStatus::empty_channels(),
            realized_overlays: fm_protocol::OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: history,
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            paged_states: 0,
            cue_list_states: 0,
            script_states: 0,
            shortcut_states: 0,
            still_states: 0,
            media_list_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
            stream_targets: Vec::new(),
            live_lan: fm_protocol::LiveLanEntry::default(),
            live_lan_status: fm_protocol::LiveLanStatus::default(),
            transitions_in_flight: Vec::new(),
        };
        snapshot.edit_locks.project = locks.project;
        snapshot.edit_locks.inputs = locks
            .inputs
            .iter()
            .copied()
            .map(fm_protocol::WireInputId::from_domain)
            .collect();
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        model.view().unwrap()
    }

    #[allow(clippy::needless_pass_by_value)] // Test fixtures read best by value.
    fn editor(locks: EditLocks, history: EditHistoryStatus) -> StudioUiState {
        StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view(&locks, history))
            .with_edit_project_permission(true)
    }

    const HISTORY: EditHistoryStatus = EditHistoryStatus {
        undo_depth: 2,
        next_undo: Some(7),
        redo_depth: 1,
        next_redo: Some(8),
    };

    #[test]
    fn history_actions_target_the_edit_the_operator_saw() {
        let state = editor(EditLocks::default(), HISTORY);
        let availability = edit_history_availability(&state);
        assert_eq!(
            availability.undo_intent(),
            Some(StudioIntent::Undo { expected_edit: 7 })
        );
        assert_eq!(
            availability.redo_intent(),
            Some(StudioIntent::Redo { expected_edit: 8 })
        );
        assert!(availability.undo.enabled() && availability.undo.reason.is_empty());
    }

    #[test]
    fn history_actions_explain_why_they_are_unavailable() {
        let empty =
            edit_history_availability(&editor(EditLocks::default(), EditHistoryStatus::EMPTY));
        assert_eq!(empty.undo.reason, NOTHING_TO_UNDO_REASON);
        assert_eq!(empty.redo.reason, NOTHING_TO_REDO_REASON);
        assert_eq!(empty.undo_intent(), None);

        let locked = edit_history_availability(&editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        ));
        assert_eq!(locked.undo.reason, PROJECT_LOCKED_REASON);
        assert_eq!(locked.redo.reason, PROJECT_LOCKED_REASON);
        assert_eq!(locked.redo_intent(), None);

        // An input padlock does not block history; the server checks the
        // resolved edit against it.
        let input_locked = edit_history_availability(&editor(
            EditLocks {
                project: false,
                inputs: vec![input(1)],
            },
            HISTORY,
        ));
        assert!(input_locked.undo.enabled());

        let mut viewer = editor(EditLocks::default(), HISTORY);
        viewer.can_edit_project = false;
        let denied = edit_history_availability(&viewer);
        assert_eq!(
            denied.undo.reason,
            "this session lacks the edit_project permission"
        );
        let offline =
            edit_history_availability(&StudioUiState::new(StudioConnectionStatus::Disconnected));
        assert_eq!(offline.undo.reason, "there is no control connection");
        assert_eq!(offline.redo.reason, "there is no control connection");
    }

    #[test]
    fn padlock_toggle_reflects_and_flips_the_project_lock() {
        let unlocked = padlock_toggle(&editor(EditLocks::default(), HISTORY));
        assert!(!unlocked.locked && unlocked.enabled);
        assert_eq!(unlocked.label(), "UNLOCKED");
        assert_eq!(unlocked.menu_label(), "Lock project edits");
        assert_eq!(
            unlocked.intent(),
            Some(StudioIntent::SetProjectLock { locked: true })
        );

        let locked = padlock_toggle(&editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        ));
        assert!(locked.locked && locked.enabled);
        assert_eq!(locked.label(), "LOCKED");
        assert_eq!(
            locked.intent(),
            Some(StudioIntent::SetProjectLock { locked: false })
        );

        let mut operator = editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        );
        operator.can_edit_project = false;
        let read_only = padlock_toggle(&operator);
        assert!(read_only.locked && !read_only.enabled);
        assert_eq!(read_only.intent(), None);
        assert_eq!(
            read_only.reason,
            "this session lacks the edit_project permission"
        );
    }

    #[test]
    fn history_labels_name_the_target_revision_and_depth() {
        let available = edit_history_availability(&editor(EditLocks::default(), HISTORY));
        assert_eq!(
            available
                .undo
                .menu_label(Locale::ENGLISH, HistoryVerb::Undo),
            "Undo edit at REV 7"
        );
        assert_eq!(
            available
                .redo
                .menu_label(Locale::ENGLISH, HistoryVerb::Redo),
            "Redo edit at REV 8"
        );
        assert_eq!(
            available
                .undo
                .hover(Locale::ENGLISH, HistoryVerb::Undo, UNDO_SHORTCUT),
            "Undo the project edit at REV 7  [Ctrl+Z]\n2 steps in the shared history"
        );
        assert!(
            available
                .redo
                .hover(Locale::ENGLISH, HistoryVerb::Redo, REDO_SHORTCUT)
                .ends_with("1 step in the shared history")
        );
        assert!(!available.undo.locked);

        let locked = edit_history_availability(&editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        ));
        assert!(locked.undo.locked && locked.redo.locked);
        assert_eq!(
            locked.undo.menu_label(Locale::ENGLISH, HistoryVerb::Undo),
            "Undo"
        );
        assert_eq!(
            locked
                .undo
                .hover(Locale::ENGLISH, HistoryVerb::Undo, UNDO_SHORTCUT),
            "Undo  [Ctrl+Z]\n2 steps in the shared history\nUnavailable: the project is locked against edits"
        );

        let empty =
            edit_history_availability(&editor(EditLocks::default(), EditHistoryStatus::EMPTY));
        assert!(!empty.undo.locked);
        assert!(
            empty
                .undo
                .hover(Locale::ENGLISH, HistoryVerb::Undo, UNDO_SHORTCUT)
                .contains("history empty")
        );
    }

    #[test]
    fn lock_summary_badges_project_and_known_input_locks() {
        let none = edit_lock_summary(&editor(EditLocks::default(), HISTORY));
        assert!(!none.any());
        assert_eq!(none.bank_chip(Locale::ENGLISH), None);
        assert_eq!(none.detail(Locale::ENGLISH), "Project edits are unlocked.");

        let inputs = edit_lock_summary(&editor(
            EditLocks {
                project: false,
                inputs: vec![input(2)],
            },
            HISTORY,
        ));
        assert_eq!(inputs.locked_inputs, 1);
        assert_eq!(
            inputs.bank_chip(Locale::ENGLISH).as_deref(),
            Some("1 LOCKED")
        );
        assert_eq!(
            inputs.detail(Locale::ENGLISH),
            "Project edits are unlocked. 1 input is locked against rename, move, and removal."
        );

        let project = edit_lock_summary(&editor(
            EditLocks {
                project: true,
                inputs: vec![input(1), input(2)],
            },
            HISTORY,
        ));
        assert!(project.any());
        assert_eq!(
            project.bank_chip(Locale::ENGLISH).as_deref(),
            Some("EDITS LOCKED")
        );
        assert!(
            project
                .detail(Locale::ENGLISH)
                .starts_with("Project edits are locked")
        );
        assert!(
            project
                .detail(Locale::ENGLISH)
                .ends_with("2 inputs are locked against rename, move, and removal.")
        );

        let offline = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(edit_lock_summary(&offline), EditLockSummary::default());

        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(inputs.bank_chip(spanish).as_deref(), Some("1 BLOQUEADA"));
        assert_eq!(
            inputs.detail(spanish),
            "Las ediciones del proyecto están desbloqueadas. 1 entrada está bloqueada contra cambio de nombre, movimiento y eliminación."
        );
    }

    #[test]
    fn history_hover_is_worded_per_language() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let available = edit_history_availability(&editor(EditLocks::default(), HISTORY));
        assert_eq!(
            available.undo.menu_label(spanish, HistoryVerb::Undo),
            "Deshacer edición en REV 7"
        );
        assert_eq!(
            available
                .undo
                .hover(spanish, HistoryVerb::Undo, UNDO_SHORTCUT),
            "Deshacer la edición del proyecto en REV 7  [Ctrl+Z]\n2 pasos en el historial compartido"
        );
        let locked = edit_history_availability(&editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        ));
        assert!(
            locked
                .redo
                .hover(spanish, HistoryVerb::Redo, REDO_SHORTCUT)
                .ends_with("No disponible: el proyecto está bloqueado contra ediciones")
        );
    }

    #[test]
    fn stinger_setup_is_blocked_only_by_the_project_padlock() {
        let input_only = editor(
            EditLocks {
                project: false,
                inputs: vec![input(1)],
            },
            HISTORY,
        );
        assert_eq!(stinger_edit_lock_reason(&input_only), None);
        let project = editor(
            EditLocks {
                project: true,
                inputs: Vec::new(),
            },
            HISTORY,
        );
        assert_eq!(
            stinger_edit_lock_reason(&project),
            Some(PROJECT_LOCKED_REASON)
        );
    }

    #[test]
    fn input_edits_report_the_blocking_padlock() {
        let state = editor(
            EditLocks {
                project: false,
                inputs: vec![input(2)],
            },
            HISTORY,
        );
        assert_eq!(input_edit_lock_reason(&state, input(1)), None);
        assert_eq!(
            input_edit_lock_reason(&state, input(2)),
            Some(INPUT_LOCKED_REASON)
        );
        let state = editor(
            EditLocks {
                project: true,
                inputs: vec![input(2)],
            },
            HISTORY,
        );
        assert_eq!(
            input_edit_lock_reason(&state, input(1)),
            Some(PROJECT_LOCKED_REASON)
        );
    }

    #[test]
    fn lock_rejections_toast_with_their_stable_reason_code() {
        let issue = |code: &str, message: &str| FieldIssue {
            field: "edit_lock".into(),
            code: code.into(),
            message: message.into(),
        };
        let toast = command_rejection_message(
            "conflict",
            "the project is locked against edits",
            &[issue(
                "project_locked",
                "the project is locked against edits",
            )],
        );
        assert!(
            toast.starts_with("Edit refused (project_locked): the project is locked"),
            "{toast}"
        );
        assert!(toast.contains("Unlock the padlock"));
        for code in [
            "input_locked",
            "input_in_use",
            "duplicate_input",
            "too_many_inputs",
            "duplicate_name",
            "invalid_name",
            "nothing_to_undo",
            "nothing_to_redo",
            "history_conflict",
            "history_drift",
        ] {
            assert!(
                command_rejection_message("conflict", "m", &[issue(code, "m")])
                    .starts_with(&format!("Edit refused ({code}): m ")),
                "{code}"
            );
        }
        assert_eq!(
            command_rejection_message("stale_revision", "moved", &[issue("other", "x")]),
            "Command rejected (stale_revision): moved"
        );
        assert_eq!(
            command_rejection_message("invalid_command", "bad", &[]),
            "Command rejected (invalid_command): bad"
        );
    }

    #[test]
    fn ctrl_z_and_ctrl_shift_z_emit_history_intents_only_when_available() {
        let context = egui::Context::default();
        let key = |modifiers| Event::Key {
            key: Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        // egui reports a press of a key that is still down as a repeat.
        let release = || Event::Key {
            key: Key::Z,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let run = |events, availability, focused| {
            context.begin_pass(egui::RawInput {
                events,
                ..Default::default()
            });
            if focused {
                context.memory_mut(|memory| memory.request_focus(egui::Id::new("text-editor")));
            }
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("history-shortcuts"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let intents = history_shortcuts(&mut ui, availability);
            let remaining = context.input(|input| input.events.len());
            drop(ui);
            let _ = context.end_pass();
            (intents, remaining)
        };
        let command_shift = Modifiers {
            shift: true,
            ..Modifiers::COMMAND
        };
        let available = edit_history_availability(&editor(EditLocks::default(), HISTORY));
        let (intents, remaining) = run(
            vec![
                key(Modifiers::COMMAND),
                release(),
                key(command_shift),
                release(),
                key(Modifiers::NONE),
                release(),
                key(Modifiers::ALT | Modifiers::COMMAND),
                release(),
            ],
            available,
            false,
        );
        assert_eq!(
            intents,
            [
                StudioIntent::Undo { expected_edit: 7 },
                StudioIntent::Redo { expected_edit: 8 },
            ]
        );
        // Plain Z, Ctrl+Alt+Z, and every release stay unhandled.
        assert_eq!(remaining, 6);

        let empty =
            edit_history_availability(&editor(EditLocks::default(), EditHistoryStatus::EMPTY));
        let (intents, remaining) = run(
            vec![
                key(Modifiers::COMMAND),
                release(),
                key(command_shift),
                release(),
            ],
            empty,
            false,
        );
        assert_eq!(intents, []);
        assert_eq!(remaining, 4);

        let (intents, _) = run(vec![key(Modifiers::COMMAND), release()], available, true);
        assert!(intents.is_empty(), "a focused text field keeps Ctrl+Z");
    }
}
