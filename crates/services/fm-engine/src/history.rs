//! Bounded undo/redo of project edits and the edit padlock.
//!
//! # Command classes
//!
//! Every [`EngineCommand`] belongs to exactly one [`EngineCommandKind`]:
//!
//! - **Project edits** change durable show configuration: input names and
//!   order, Stinger slots, and overlay transition, appearance, and output
//!   inclusion. They are recorded in the undo history and are rejected while
//!   the edit padlock covers them.
//! - **Live operations** act on what is on air or being delivered: Preview
//!   selection, cuts and transitions (including Stinger playback and the
//!   T-bar), overlay take/update/off/queue, Fade to Black, audio strip moves,
//!   streaming, and recording. They are never recorded, never reverted by
//!   undo or redo, and never blocked by a padlock, so a locked show stays
//!   fully switchable.
//! - **History** commands ([`EngineCommand::Undo`], [`EngineCommand::Redo`])
//!   resolve to the stored inverse or forward project edit and then run
//!   through the same validation, lock, idempotency, and scheduling path as a
//!   direct edit.
//! - **Edit locks** ([`EngineCommand::SetEditLock`]) toggle the padlock. They
//!   are durable commands but are not themselves undoable.
//!
//! # Inverse commands, not snapshots
//!
//! The show state mixes edit configuration with live routing (Program,
//! Preview, overlays on air, recording and streaming flags). Restoring a
//! structural snapshot would therefore revert live switching performed after
//! an edit. Each recorded edit instead stores an exact inverse
//! [`EngineCommand`] computed from the pre-edit state. Undo schedules that
//! inverse as an ordinary runtime action, so realized state follows the same
//! path as any edit and the one-event-per-accepted-command counter invariant
//! is preserved.
//!
//! # Interaction rules
//!
//! - History is one linear, shared stack of at most [`MAX_EDIT_HISTORY`]
//!   entries; the oldest entry is discarded when a new edit overflows it.
//! - A new accepted project edit clears the redo stack. Live operations and
//!   lock changes leave both stacks unchanged.
//! - Undo and redo may name the edit they expect to revert or reapply. When
//!   another editor's later edit is on top instead, the command is rejected
//!   with [`EditRejectionKind::HistoryConflict`] rather than reverting someone
//!   else's work. Omitting the expectation targets the top entry.
//! - Before reverting or reapplying, the engine checks that the affected
//!   configuration still equals what the entry recorded; a mismatch is
//!   rejected with [`EditRejectionKind::HistoryDrift`].
//! - History is session state. It is not part of [`crate::EngineSnapshot`],
//!   and both [`crate::Engine::restore`] and
//!   [`crate::Engine::restore_persisted`] start with empty undo and redo
//!   stacks. Padlock state is part of [`crate::ShowState`], so it survives an
//!   engine snapshot round trip.

use std::collections::VecDeque;

use fm_command::{CommandId, FieldIssue, Rejection, RejectionCode, Revision};
use fm_types::InputId;

use crate::{EngineCommand, ShowState};

/// Maximum number of project edits retained for undo (and therefore redo).
pub const MAX_EDIT_HISTORY: usize = 100;

/// The padlock target of an edit lock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EditLockScope {
    /// Locks every project edit, including undo and redo.
    Project,
    /// Locks edits that change one input's name or position.
    Input(InputId),
}

/// The safety class of an engine command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineCommandKind {
    ProjectEdit,
    LiveOperation,
    History,
    EditLock,
}

impl EngineCommand {
    /// Returns this command's undo and padlock class.
    #[must_use]
    pub const fn kind(&self) -> EngineCommandKind {
        match self {
            Self::RenameInput { .. }
            | Self::ReorderInputs { .. }
            | Self::ConfigureStinger { .. }
            | Self::RemoveStinger { .. }
            | Self::ConfigureOverlayTransition { .. }
            | Self::ConfigureOverlayAppearance { .. }
            | Self::SetOverlayOutputInclusion { .. } => EngineCommandKind::ProjectEdit,
            Self::Undo { .. } | Self::Redo { .. } => EngineCommandKind::History,
            Self::SetEditLock { .. } => EngineCommandKind::EditLock,
            Self::SetInputAudioStrip { .. }
            | Self::SelectPreview(_)
            | Self::Cut
            | Self::Fade { .. }
            | Self::AlphaFade { .. }
            | Self::Slide { .. }
            | Self::Zoom { .. }
            | Self::Stinger { .. }
            | Self::TakeOverlay { .. }
            | Self::UpdateOverlay { .. }
            | Self::OverlayOff { .. }
            | Self::QueueOverlay { .. }
            | Self::TakeNextOverlay { .. }
            | Self::Wipe { .. }
            | Self::FadeToBlack { .. }
            | Self::StartManualTransition { .. }
            | Self::SetManualTransitionPosition { .. }
            | Self::CommitManualTransition
            | Self::CancelManualTransition
            | Self::StreamStart { .. }
            | Self::StreamStop { .. }
            | Self::RecordStart { .. }
            | Self::RecordStop { .. } => EngineCommandKind::LiveOperation,
        }
    }
}

/// Stable classification of an undo, redo, or padlock rejection.
///
/// The engine reports these through ordinary [`Rejection`] receipts whose
/// first field issue carries [`EditRejectionKind::code`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditRejectionKind {
    ProjectLocked,
    InputLocked,
    NothingToUndo,
    NothingToRedo,
    HistoryConflict,
    HistoryDrift,
}

impl EditRejectionKind {
    const ALL: [Self; 6] = [
        Self::ProjectLocked,
        Self::InputLocked,
        Self::NothingToUndo,
        Self::NothingToRedo,
        Self::HistoryConflict,
        Self::HistoryDrift,
    ];

    /// Returns the stable field-issue code for this rejection.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProjectLocked => "project_locked",
            Self::InputLocked => "input_locked",
            Self::NothingToUndo => "nothing_to_undo",
            Self::NothingToRedo => "nothing_to_redo",
            Self::HistoryConflict => "history_conflict",
            Self::HistoryDrift => "history_drift",
        }
    }

    /// Classifies a rejection produced by the undo history or padlock.
    #[must_use]
    pub fn of(rejection: &Rejection) -> Option<Self> {
        let issue = rejection.fields.first()?;
        Self::ALL.into_iter().find(|kind| kind.code() == issue.code)
    }
}

/// A typed undo, redo, or padlock refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditRejection {
    ProjectLocked,
    InputLocked(InputId),
    NothingToUndo,
    NothingToRedo,
    HistoryConflict {
        expected: Revision,
        actual: Revision,
    },
    HistoryDrift {
        edit: Revision,
    },
}

impl EditRejection {
    #[must_use]
    pub const fn kind(self) -> EditRejectionKind {
        match self {
            Self::ProjectLocked => EditRejectionKind::ProjectLocked,
            Self::InputLocked(_) => EditRejectionKind::InputLocked,
            Self::NothingToUndo => EditRejectionKind::NothingToUndo,
            Self::NothingToRedo => EditRejectionKind::NothingToRedo,
            Self::HistoryConflict { .. } => EditRejectionKind::HistoryConflict,
            Self::HistoryDrift { .. } => EditRejectionKind::HistoryDrift,
        }
    }

    /// Converts this refusal into a receipt rejection with a stable field code.
    #[must_use]
    pub fn into_rejection(self) -> Rejection {
        let (code, field, message) = match self {
            Self::ProjectLocked => (
                RejectionCode::Conflict,
                "edit_lock",
                "the project is locked against edits".to_owned(),
            ),
            Self::InputLocked(input) => (
                RejectionCode::Conflict,
                "edit_lock",
                format!("input {input} is locked against edits"),
            ),
            Self::NothingToUndo => (
                RejectionCode::NotFound,
                "history",
                "there is no project edit to undo".to_owned(),
            ),
            Self::NothingToRedo => (
                RejectionCode::NotFound,
                "history",
                "there is no project edit to redo".to_owned(),
            ),
            Self::HistoryConflict { expected, actual } => (
                RejectionCode::Conflict,
                "history",
                format!(
                    "expected to target the edit from revision {}, but the edit from revision {} \
                     is next",
                    expected.get(),
                    actual.get()
                ),
            ),
            Self::HistoryDrift { edit } => (
                RejectionCode::Conflict,
                "history",
                format!(
                    "the configuration changed by the edit from revision {} no longer matches \
                     its recorded state",
                    edit.get()
                ),
            ),
        };
        Rejection::new(code, message.clone()).with_field(FieldIssue::new(
            field,
            self.kind().code(),
            message,
        ))
    }
}

/// One undoable project edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditHistoryEntry {
    edit: Revision,
    command_id: CommandId,
    forward: EngineCommand,
    inverse: EngineCommand,
}

impl EditHistoryEntry {
    /// Returns the revision at which the edit was first accepted, which
    /// identifies the entry across undo and redo.
    #[must_use]
    pub const fn edit(&self) -> Revision {
        self.edit
    }

    /// Returns the command identifier of the original edit.
    #[must_use]
    pub const fn command_id(&self) -> &CommandId {
        &self.command_id
    }

    /// Returns the command that applies the edit.
    #[must_use]
    pub const fn forward(&self) -> &EngineCommand {
        &self.forward
    }

    /// Returns the command that reverts the edit.
    #[must_use]
    pub const fn inverse(&self) -> &EngineCommand {
        &self.inverse
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EditHistory {
    undo: VecDeque<EditHistoryEntry>,
    redo: Vec<EditHistoryEntry>,
}

/// How an accepted resolved command changes the history.
#[derive(Clone, Debug)]
pub(crate) enum HistoryStep {
    None,
    Record { inverse: EngineCommand },
    Undo,
    Redo,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedCommand {
    pub(crate) command: EngineCommand,
    pub(crate) step: HistoryStep,
}

impl EditHistory {
    /// Undo entries, oldest first; the last entry is undone next.
    pub(crate) fn undo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.undo.iter()
    }

    /// Redo entries, oldest first; the last entry is redone next.
    pub(crate) fn redo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.redo.iter()
    }

    /// Resolves a submitted command into the command that will execute.
    pub(crate) fn resolve(
        &self,
        state: &ShowState,
        command: &EngineCommand,
    ) -> Result<ResolvedCommand, Rejection> {
        match command {
            EngineCommand::Undo { expected_edit } => {
                let entry = self.undo.back();
                Self::resolve_step(state, entry, *expected_edit, true)
            }
            EngineCommand::Redo { expected_edit } => {
                let entry = self.redo.last();
                Self::resolve_step(state, entry, *expected_edit, false)
            }
            command => Ok(ResolvedCommand {
                command: command.clone(),
                step: inverse_for(state, command)
                    .map_or(HistoryStep::None, |inverse| HistoryStep::Record { inverse }),
            }),
        }
    }

    fn resolve_step(
        state: &ShowState,
        entry: Option<&EditHistoryEntry>,
        expected_edit: Option<Revision>,
        undo: bool,
    ) -> Result<ResolvedCommand, Rejection> {
        if state.is_project_locked() {
            return Err(EditRejection::ProjectLocked.into_rejection());
        }
        let Some(entry) = entry else {
            let empty = if undo {
                EditRejection::NothingToUndo
            } else {
                EditRejection::NothingToRedo
            };
            return Err(empty.into_rejection());
        };
        if let Some(expected) = expected_edit
            && expected != entry.edit
        {
            return Err(EditRejection::HistoryConflict {
                expected,
                actual: entry.edit,
            }
            .into_rejection());
        }
        let (apply, recorded) = if undo {
            (&entry.inverse, &entry.forward)
        } else {
            (&entry.forward, &entry.inverse)
        };
        // Applying `apply` must exactly reverse the state `recorded` would
        // produce, which holds only while the edited configuration is
        // unchanged since the entry was last applied.
        if inverse_for(state, apply).as_ref() != Some(recorded) {
            return Err(EditRejection::HistoryDrift { edit: entry.edit }.into_rejection());
        }
        Ok(ResolvedCommand {
            command: apply.clone(),
            step: if undo {
                HistoryStep::Undo
            } else {
                HistoryStep::Redo
            },
        })
    }

    /// Applies the history change for one accepted, newly committed command.
    pub(crate) fn commit(
        &mut self,
        step: HistoryStep,
        command: EngineCommand,
        command_id: CommandId,
        revision: Revision,
    ) {
        match step {
            HistoryStep::None => {}
            HistoryStep::Record { inverse } => {
                self.redo.clear();
                self.undo.push_back(EditHistoryEntry {
                    edit: revision,
                    command_id,
                    forward: command,
                    inverse,
                });
                while self.undo.len() > MAX_EDIT_HISTORY {
                    self.undo.pop_front();
                }
            }
            HistoryStep::Undo => {
                if let Some(entry) = self.undo.pop_back() {
                    self.redo.push(entry);
                }
            }
            HistoryStep::Redo => {
                if let Some(entry) = self.redo.pop() {
                    self.undo.push_back(entry);
                }
            }
        }
    }
}

/// Returns the exact command that would revert `command` on `state`, or
/// `None` when `command` is not a project edit or targets nothing in `state`.
pub(crate) fn inverse_for(state: &ShowState, command: &EngineCommand) -> Option<EngineCommand> {
    let switcher = state.desired_switcher();
    match command {
        EngineCommand::RenameInput { input, .. } => Some(EngineCommand::RenameInput {
            input: *input,
            name: state.input_name(*input)?.to_owned(),
        }),
        EngineCommand::ReorderInputs { .. } => Some(EngineCommand::ReorderInputs {
            inputs: state.inputs().to_vec(),
        }),
        EngineCommand::ConfigureStinger { slot, .. } | EngineCommand::RemoveStinger { slot } => {
            Some(match switcher.stinger(*slot).descriptor() {
                Some(descriptor) => EngineCommand::ConfigureStinger {
                    slot: *slot,
                    descriptor: *descriptor,
                },
                None => EngineCommand::RemoveStinger { slot: *slot },
            })
        }
        EngineCommand::ConfigureOverlayTransition { channel, .. } => {
            let overlay = switcher.overlay(*channel);
            Some(EngineCommand::ConfigureOverlayTransition {
                channel: *channel,
                transition: overlay.transition(),
                duration_frames: overlay.duration_frames(),
            })
        }
        EngineCommand::ConfigureOverlayAppearance { channel, .. } => {
            let overlay = switcher.overlay(*channel);
            Some(EngineCommand::ConfigureOverlayAppearance {
                channel: *channel,
                position: overlay.position(),
                border: overlay.border(),
            })
        }
        EngineCommand::SetOverlayOutputInclusion {
            channel, output, ..
        } => Some(EngineCommand::SetOverlayOutputInclusion {
            channel: *channel,
            output: *output,
            included: switcher.overlay(*channel).is_included_in(*output),
        }),
        _ => None,
    }
}

/// Rejects a project edit covered by the padlock.
pub(crate) fn check_edit_locks(
    state: &ShowState,
    command: &EngineCommand,
) -> Result<(), EditRejection> {
    if command.kind() != EngineCommandKind::ProjectEdit {
        return Ok(());
    }
    if state.is_project_locked() {
        return Err(EditRejection::ProjectLocked);
    }
    match command {
        EngineCommand::RenameInput { input, .. } if state.is_input_locked(*input) => {
            Err(EditRejection::InputLocked(*input))
        }
        EngineCommand::ReorderInputs { inputs } => {
            for (index, input) in state.inputs().iter().enumerate() {
                if state.is_input_locked(*input)
                    && inputs
                        .iter()
                        .position(|candidate| candidate == input)
                        .is_some_and(|position| position != index)
                {
                    return Err(EditRejection::InputLocked(*input));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
