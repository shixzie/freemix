//! Bounded undo/redo of project edits and the edit padlock.
//!
//! # Command classes
//!
//! Every [`EngineCommand`] belongs to exactly one [`EngineCommandKind`]:
//!
//! - **Project edits** change durable show configuration: the input
//!   catalogue (adding and removing inputs), input names and order, Stinger
//!   slots, overlay transition, appearance, and output inclusion, and the
//!   project grading ([`crate::EngineGradeEdit`]), the animation preset
//!   library ([`crate::EngineAnimationPresetEdit`]), the input lifecycle
//!   ([`crate::EngineInputLifecycleEdit`]: duplicate, source replacement,
//!   categories, colour labels, pause, close, batch import, and multi-input
//!   removal), and live scene layer edits ([`crate::EngineSceneLayerEdit`]:
//!   geometry, appearance, crop, mask, z-order, position, add, and remove,
//!   each with an exact inverse). They are rejected while
//!   the edit padlock covers them (an input's padlock also covers its
//!   grade), and all but input and LUT removal are recorded in the undo
//!   history.
//! - **Live operations** act on what is on air or being delivered: Preview
//!   selection, cuts and transitions (including Stinger playback and the
//!   T-bar), overlay take/update/off/queue, Fade to Black, audio strip moves,
//!   streaming, recording, and title content (field edits, title take, and
//!   title off-air: content, not layout). They are never recorded, never
//!   reverted by undo or redo, and never blocked by a padlock, so a locked
//!   show stays fully switchable.
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
//! # Adding and removing inputs
//!
//! Adding an input is undoable: its inverse removes the input again, and redo
//! re-appends it with the same identifier, name, and source. Undo is refused
//! as drift unless the added input still exists under its recorded name at the
//! end of the input order (where the add put it), so redo restores the same
//! position. It is also refused as drift while the input carries a grade:
//! removal drops the grade, and the edits that set it may have been
//! forgotten (LUT removal forgets every entry naming the LUT), so redo could
//! not bring it back. Clear the grade first. Undo discards the input's Master strip moves, which are live
//! operations; redo restores the default strip, exactly as the add created it.
//! Undo is refused like any removal while the input is on air or configured
//! (Program, Preview, a transition, an overlay, or a Stinger slot), and while
//! its padlock is engaged.
//!
//! Removing an input is a project edit that is **not** undoable. Its inverse
//! would have to recreate the complete input: the engine knows only the
//! identifier, name, position, and strip, while the input's kind (a media
//! file, a capture device, a Scene, an alias, ...) and required capabilities
//! live in the persisted project, and most kinds cannot be added live at all.
//! An exact inverse is therefore not well-defined here. An accepted removal
//! instead clears the redo stack, like any new edit, and forgets every
//! retained undo entry that names the removed input (renames, reorders, adds,
//! and Stinger configurations), since none of them could apply again.
//!
//! # Input lifecycle edits
//!
//! Category, colour, pause, close, category list, and source replacement
//! edits record the previous value as their inverse. A duplicate or a batch
//! import is undone by removing exactly the inputs it appended
//! ([`crate::EngineInputLifecycleEdit::RemoveInputs`]); like undoing an add,
//! that is refused as drift unless those inputs are still the last of the
//! order under their recorded names and ungraded, so redo appends them to the
//! same position again. A direct multi-input removal is, like a single
//! removal, not undoable and forgets the history naming its inputs.
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
use fm_model::LutId;
use fm_types::InputId;

use crate::{EngineCommand, EngineGradeEdit, ShowState};

/// Maximum number of project edits retained for undo (and therefore redo).
pub const MAX_EDIT_HISTORY: usize = 100;

/// How many of the newest undo entries a continuing scene layer edit looks
/// through for its gesture's entry.
const MAX_GESTURE_FOLD_DEPTH: usize = 32;

/// The padlock target of an edit lock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EditLockScope {
    /// Locks every project edit, including undo and redo.
    Project,
    /// Locks edits that change one input's name or position, or remove it.
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
            | Self::AddInput { .. }
            | Self::RemoveInput { .. }
            | Self::ConfigureStinger { .. }
            | Self::RemoveStinger { .. }
            | Self::ConfigureOverlayTransition { .. }
            | Self::ConfigureOverlayAppearance { .. }
            | Self::SetOverlayOutputInclusion { .. }
            | Self::SetPlaylist { .. }
            | Self::Grade(_)
            | Self::TitleAnimation(_)
            | Self::SceneAnimation(_)
            | Self::SceneLayer(_)
            | Self::AnimationPreset(_)
            | Self::SetTransitionFavorite { .. }
            | Self::InputLifecycle(_) => EngineCommandKind::ProjectEdit,
            Self::Media(command) => {
                if command.operation.is_transport() {
                    EngineCommandKind::LiveOperation
                } else {
                    EngineCommandKind::ProjectEdit
                }
            }
            Self::Undo { .. } | Self::Redo { .. } => EngineCommandKind::History,
            Self::SetEditLock { .. } => EngineCommandKind::EditLock,
            Self::SetInputAudioStrip { .. }
            | Self::SelectPreview(_)
            | Self::Cut
            | Self::Fade { .. }
            | Self::AlphaFade { .. }
            | Self::Slide { .. }
            | Self::Zoom { .. }
            | Self::Transition { .. }
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
            | Self::RecordStop { .. }
            | Self::SetTitleFields { .. }
            | Self::TakeTitle { .. }
            | Self::TitleOffAir { .. }
            | Self::PlaylistControl(_)
            | Self::PlaylistAdvance { .. }
            | Self::RealizePlaylistCue(_)
            | Self::RealizeTitle(_) => EngineCommandKind::LiveOperation,
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
    Record {
        inverse: EngineCommand,
    },
    /// A project edit without an inverse: input removal. Clears the redo
    /// stack and forgets undo entries that name a removed input.
    Forget {
        inputs: Vec<InputId>,
    },
    /// LUT removal, which has no journalable inverse. Clears the redo stack
    /// and forgets undo entries that name the removed LUT.
    ForgetLut {
        lut: LutId,
    },
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
            EngineCommand::RemoveInput { input } => Ok(ResolvedCommand {
                command: command.clone(),
                step: HistoryStep::Forget {
                    inputs: vec![*input],
                },
            }),
            EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::RemoveInputs {
                inputs,
            }) => Ok(ResolvedCommand {
                command: command.clone(),
                step: HistoryStep::Forget {
                    inputs: inputs.clone(),
                },
            }),
            EngineCommand::Grade(EngineGradeEdit::RemoveLut { id }) => Ok(ResolvedCommand {
                command: command.clone(),
                step: HistoryStep::ForgetLut { lut: *id },
            }),
            command => {
                let command = normalized(state, command);
                Ok(ResolvedCommand {
                    step: inverse_for(state, &command)
                        .map_or(HistoryStep::None, |inverse| HistoryStep::Record { inverse }),
                    command,
                })
            }
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
        if !reverses(state, apply, recorded) {
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

    /// The undo entry a continuing scene layer `edit` folds into: the latest
    /// entry editing the same property of the same layer, found among the
    /// newest entries while they are all live edits of the same scene that
    /// touch other layers (a multi-layer gesture records one entry per
    /// layer, and edits of different layers commute). Any other entry ends
    /// the search.
    fn gesture_entry(
        &mut self,
        edit: &crate::EngineSceneLayerEdit,
    ) -> Option<&mut EditHistoryEntry> {
        if !edit.continues {
            return None;
        }
        for entry in self.undo.iter_mut().rev().take(MAX_GESTURE_FOLD_DEPTH) {
            let EngineCommand::SceneLayer(previous) = &entry.forward else {
                return None;
            };
            if previous.scene != edit.scene {
                return None;
            }
            if edit.folds_into(previous) {
                return Some(entry);
            }
            if previous.layer == edit.layer {
                return None;
            }
        }
        None
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
                // A gesture never folds across an undo.
                let foldable = self.redo.is_empty();
                self.redo.clear();
                // A recorded animation edit replays without a base revision;
                // the drift check stands in for it.
                let forward = match command {
                    EngineCommand::TitleAnimation(edit) => {
                        EngineCommand::TitleAnimation(edit.unchecked())
                    }
                    EngineCommand::SceneAnimation(edit) => {
                        EngineCommand::SceneAnimation(edit.unchecked())
                    }
                    // A gesture's continuing scene layer edit folds into the
                    // gesture's entry for the same layer property: the entry
                    // keeps its inverse (the state before the gesture) and
                    // takes the new forward edit.
                    EngineCommand::SceneLayer(edit) => {
                        if foldable && let Some(entry) = self.gesture_entry(&edit) {
                            entry.forward = EngineCommand::SceneLayer(edit.recorded());
                            return;
                        }
                        EngineCommand::SceneLayer(edit.recorded())
                    }
                    command => command,
                };
                self.undo.push_back(EditHistoryEntry {
                    edit: revision,
                    command_id,
                    forward,
                    inverse,
                });
                while self.undo.len() > MAX_EDIT_HISTORY {
                    self.undo.pop_front();
                }
            }
            HistoryStep::Forget { inputs } => {
                self.redo.clear();
                self.undo.retain(|entry| {
                    inputs.iter().all(|input| {
                        !names_input(&entry.forward, *input) && !names_input(&entry.inverse, *input)
                    })
                });
            }
            HistoryStep::ForgetLut { lut } => {
                self.redo.clear();
                self.undo.retain(|entry| {
                    !names_lut(&entry.forward, lut) && !names_lut(&entry.inverse, lut)
                });
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

/// Returns whether applying `apply` to `state` exactly reverses the state
/// that `recorded` produces.
fn reverses(state: &ShowState, apply: &EngineCommand, recorded: &EngineCommand) -> bool {
    match (apply, recorded) {
        // Undoing an add removes the input it added. The input must still
        // carry the recorded name and sit at the end of the order, where the
        // add appended it, so that redo re-appends it to the same position.
        // It must also be ungraded: removal drops the input's grade, and the
        // edits that set it may no longer be in the history (LUT removal
        // forgets them), so redo could not restore it.
        (
            EngineCommand::RemoveInput { input },
            EngineCommand::AddInput {
                input: added, name, ..
            },
        ) => {
            input == added
                && state.inputs().last() == Some(input)
                && state.input_name(*input) == Some(name.as_str())
                && !state.grading().inputs.contains_key(input)
        }
        // Undoing a duplicate or a batch import removes exactly the inputs
        // it appended (see `input_lifecycle::removal_reverses`).
        (
            EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::RemoveInputs { inputs }),
            EngineCommand::InputLifecycle(forward),
        ) => crate::input_lifecycle::removal_reverses(state, inputs, forward),
        // Undoing an import removes the LUT it imported, which must still be
        // the last LUT with the recorded name and text, so redo re-imports
        // it to the same position.
        (
            EngineCommand::Grade(EngineGradeEdit::RemoveLut { id }),
            EngineCommand::Grade(EngineGradeEdit::ImportLut {
                id: imported,
                name,
                source,
            }),
        ) => {
            id == imported
                && state
                    .grading()
                    .luts
                    .last()
                    .is_some_and(|lut| lut.id == *id && lut.name == *name)
                && state
                    .luts()
                    .get(id)
                    .is_some_and(|lut| lut.source() == source)
        }
        _ => inverse_for(state, apply).as_ref() == Some(recorded),
    }
}

/// Returns `command` with every implicit position made explicit against
/// `state`, so its recorded form replays exactly: a library grade added
/// without an index is added at the end of the current library.
fn normalized(state: &ShowState, command: &EngineCommand) -> EngineCommand {
    match command {
        EngineCommand::Grade(EngineGradeEdit::AddGrade { grade, index: None }) => {
            EngineCommand::Grade(EngineGradeEdit::AddGrade {
                grade: grade.clone(),
                index: Some(state.grading().grades.len()),
            })
        }
        // An animation preset or stretch is recorded as the clip it makes.
        EngineCommand::TitleAnimation(edit) => {
            EngineCommand::TitleAnimation(crate::title_animation::normalized(state, edit))
        }
        EngineCommand::SceneAnimation(edit) => {
            EngineCommand::SceneAnimation(crate::scene_animation::normalized(state, edit))
        }
        EngineCommand::AnimationPreset(edit) => {
            EngineCommand::AnimationPreset(crate::animation_preset::normalized(state, edit))
        }
        command => command.clone(),
    }
}

/// Returns whether `command` refers to LUT `lut`.
fn names_lut(command: &EngineCommand, lut: LutId) -> bool {
    matches!(command, EngineCommand::Grade(edit) if edit.names_lut(lut))
}

/// Returns whether `command` refers to `input`.
fn names_input(command: &EngineCommand, input: InputId) -> bool {
    match command {
        EngineCommand::RenameInput { input: named, .. }
        | EngineCommand::AddInput { input: named, .. }
        | EngineCommand::RemoveInput { input: named } => *named == input,
        EngineCommand::ReorderInputs { inputs } => inputs.contains(&input),
        EngineCommand::ConfigureStinger { descriptor, .. } => descriptor.media_input == input,
        EngineCommand::Grade(edit) => edit.input() == Some(input),
        EngineCommand::TitleAnimation(edit) => edit.input == input,
        EngineCommand::SceneAnimation(edit) => edit.input == input,
        EngineCommand::InputLifecycle(edit) => edit.names_input(input),
        EngineCommand::SceneLayer(edit) => edit.names_input(input),
        _ => false,
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
        // An add that targets a new identifier is reverted by removing it.
        // Removal has no inverse here; see the module documentation.
        EngineCommand::AddInput { input, .. } => (!state.inputs().contains(input))
            .then_some(EngineCommand::RemoveInput { input: *input }),
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
        EngineCommand::Grade(edit) => grade_inverse(state, edit).map(EngineCommand::Grade),
        EngineCommand::TitleAnimation(edit) => {
            crate::title_animation::inverse(state, edit).map(EngineCommand::TitleAnimation)
        }
        EngineCommand::SceneAnimation(edit) => {
            crate::scene_animation::inverse(state, edit).map(EngineCommand::SceneAnimation)
        }
        EngineCommand::AnimationPreset(edit) => {
            crate::animation_preset::inverse(state, edit).map(EngineCommand::AnimationPreset)
        }
        EngineCommand::InputLifecycle(edit) => crate::input_lifecycle::inverse(state, edit),
        EngineCommand::SceneLayer(edit) => {
            crate::scene_layer::inverse(state, edit).map(EngineCommand::SceneLayer)
        }
        EngineCommand::SetTransitionFavorite { slot, .. } => state
            .transition_favorites()
            .get(*slot)
            .map(|favorite| EngineCommand::SetTransitionFavorite {
                slot: *slot,
                favorite,
            }),
        _ => None,
    }
}

/// The exact edit that reverts grade `edit` on `state`, or `None` when the
/// edit targets nothing in `state` or (LUT removal) has no inverse.
fn grade_inverse(state: &ShowState, edit: &EngineGradeEdit) -> Option<EngineGradeEdit> {
    let grading = state.grading();
    match edit {
        EngineGradeEdit::SetInput { input, .. } => {
            state
                .inputs()
                .contains(input)
                .then(|| EngineGradeEdit::SetInput {
                    input: *input,
                    source: grading.inputs.get(input).cloned(),
                })
        }
        EngineGradeEdit::SetLayer { scene, layer, .. } => {
            crate::grade_edit::layer_target(state, *scene, *layer)
                .ok()
                .map(|target| EngineGradeEdit::SetLayer {
                    scene: *scene,
                    layer: *layer,
                    source: grading.layers.get(&target).cloned(),
                })
        }
        EngineGradeEdit::SetProgram { .. } => Some(EngineGradeEdit::SetProgram {
            source: grading.program.clone(),
        }),
        EngineGradeEdit::AddGrade { grade, .. } => grading
            .grade(grade.id)
            .is_none()
            .then_some(EngineGradeEdit::RemoveGrade { id: grade.id }),
        EngineGradeEdit::UpdateGrade { id, .. } => {
            grading
                .grade(*id)
                .map(|current| EngineGradeEdit::UpdateGrade {
                    id: *id,
                    name: current.name.clone(),
                    grade: Box::new(current.grade.clone()),
                })
        }
        EngineGradeEdit::RemoveGrade { id } => grading
            .grades
            .iter()
            .position(|candidate| candidate.id == *id)
            .map(|index| EngineGradeEdit::AddGrade {
                grade: Box::new(grading.grades[index].clone()),
                index: Some(index),
            }),
        EngineGradeEdit::ImportLut { id, .. } => grading
            .lut(*id)
            .is_none()
            .then_some(EngineGradeEdit::RemoveLut { id: *id }),
        EngineGradeEdit::RemoveLut { .. } => None,
    }
}

/// Rejects a project edit covered by the padlock.
#[allow(clippy::too_many_lines)] // one arm per lockable edit
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
        EngineCommand::RenameInput { input, .. }
        | EngineCommand::RemoveInput { input }
        | EngineCommand::Grade(EngineGradeEdit::SetInput { input, .. })
        | EngineCommand::Media(crate::EngineMediaCommand { input, .. })
            if state.is_input_locked(*input) =>
        {
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
        EngineCommand::InputLifecycle(edit) => {
            match edit
                .locked_inputs()
                .into_iter()
                .find(|input| state.is_input_locked(*input))
            {
                Some(locked) => Err(EditRejection::InputLocked(locked)),
                None => Ok(()),
            }
        }
        // Every Scene input showing the edited layer's scene shows the edit.
        EngineCommand::Grade(EngineGradeEdit::SetLayer { scene, .. })
        | EngineCommand::SceneLayer(crate::EngineSceneLayerEdit { scene, .. }) => {
            match state
                .scene_inputs()
                .iter()
                .filter(|(_, shown)| *shown == scene)
                .map(|(input, _)| *input)
                .find(|input| state.is_input_locked(*input))
            {
                Some(locked) => Err(EditRejection::InputLocked(locked)),
                None => Ok(()),
            }
        }
        // Every input graded with the library grade, and every Scene input
        // showing a layer graded with it, shows the edit.
        EngineCommand::Grade(EngineGradeEdit::UpdateGrade { id, .. }) => {
            let library = fm_model::GradeSource::Library(*id);
            let grading = state.grading();
            let graded_inputs = grading
                .inputs
                .iter()
                .filter(|(_, source)| **source == library)
                .map(|(input, _)| *input);
            let scene_inputs = state
                .scene_inputs()
                .iter()
                .filter(|(_, scene)| {
                    grading
                        .layers
                        .iter()
                        .any(|(target, source)| target.scene == **scene && *source == library)
                })
                .map(|(input, _)| *input);
            match graded_inputs
                .chain(scene_inputs)
                .find(|input| state.is_input_locked(*input))
            {
                Some(locked) => Err(EditRejection::InputLocked(locked)),
                None => Ok(()),
            }
        }
        // Every title sharing the animated template shows the edit.
        EngineCommand::TitleAnimation(edit) => {
            match crate::title_animation::title_template_inputs(state, edit.input)
                .into_iter()
                .find(|input| state.is_input_locked(*input))
            {
                Some(locked) => Err(EditRejection::InputLocked(locked)),
                None if state.is_input_locked(edit.input) => {
                    Err(EditRejection::InputLocked(edit.input))
                }
                None => Ok(()),
            }
        }
        // Every Scene input showing the animated scene shows the edit.
        EngineCommand::SceneAnimation(edit) => {
            match crate::scene_animation::scene_inputs_sharing(state, edit.input)
                .into_iter()
                .find(|input| state.is_input_locked(*input))
            {
                Some(locked) => Err(EditRejection::InputLocked(locked)),
                None if state.is_input_locked(edit.input) => {
                    Err(EditRejection::InputLocked(edit.input))
                }
                None => Ok(()),
            }
        }
        _ => Ok(()),
    }
}
