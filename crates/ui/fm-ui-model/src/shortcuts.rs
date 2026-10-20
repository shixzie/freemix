//! Replicated user shortcuts (Protocol 2.72, RC-004).
//!
//! The project's shortcuts arrive as `shortcut_state` records after a
//! snapshot and change with `shortcut_changed` and `shortcut_removed`. A
//! fire of cue actions arrives as `shortcut_fired`, carrying the desired
//! state afterwards in the shape of a cue run, which is applied like one.

use fm_protocol::{ShortcutFireChange, ShortcutStatus};

use crate::{BusSelection, CueRunUpdate, ManualTransitionStatus, ModelError, ProjectState};

/// An accepted fire of a shortcut's cue actions as a durable change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutFireUpdate {
    pub shortcut: u64,
    /// The frame the actions reach Program on.
    pub frame: u64,
    /// Actions the fire ran.
    pub actions: u32,
    /// The desired state afterwards, in the shape of a cue run (its `list`
    /// is 0, its run state idle, and nothing fired).
    pub effects: CueRunUpdate,
}

impl ShortcutFireUpdate {
    pub(crate) fn from_protocol(change: ShortcutFireChange) -> Self {
        Self {
            shortcut: change.shortcut,
            frame: change.frame,
            actions: change.actions,
            effects: CueRunUpdate {
                list: 0,
                frame: change.frame,
                state: fm_protocol::CueRunStatus::default(),
                fired: Vec::new(),
                selection: BusSelection::new(
                    change.program.to_domain(),
                    change.preview.to_domain(),
                ),
                manual_transition: ManualTransitionStatus::from_protocol(change.manual_transition),
                fade_to_black: change.fade_to_black,
                overlays: crate::protocol_overlays(change.overlays),
                input_audio_strips: crate::protocol_input_audio_strips(change.input_audio_strips),
                streams: crate::protocol_streams(change.streams),
                recordings: change.recordings,
                playlist: change.playlist,
                titles: change.titles,
                media: change.media,
                scripts: change.scripts,
            },
        }
    }
}

/// The last accepted fire of a shortcut's cue actions, as replicated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutFireReport {
    pub shortcut: u64,
    pub frame: u64,
    pub actions: u32,
}

/// Checks a whole shortcut against the wire bounds.
pub(crate) fn validate_shortcut(shortcut: &ShortcutStatus) -> Result<(), ModelError> {
    if shortcut.id == 0 || fm_protocol::validate_shortcut_spec(&shortcut.spec).is_err() {
        return Err(ModelError::InvalidShortcut(shortcut.id));
    }
    Ok(())
}

/// Checks a fire update against the replicated project; the caller
/// validates its switcher state like a `desired_switcher`.
pub(crate) fn validate_fire(
    update: &ShortcutFireUpdate,
    state: &ProjectState,
) -> Result<(), ModelError> {
    if state.shortcut(update.shortcut).is_none() {
        return Err(ModelError::InvalidShortcut(update.shortcut));
    }
    crate::cue_lists::validate_effects(&update.effects, state)
}

/// Applies a validated fire update.
pub(crate) fn apply_fire(state: &mut ProjectState, update: ShortcutFireUpdate) {
    state.last_shortcut_fire = Some(ShortcutFireReport {
        shortcut: update.shortcut,
        frame: update.frame,
        actions: update.actions,
    });
    crate::cue_lists::apply_effects(state, update.effects);
}

/// Inserts or replaces a whole shortcut, keeping project order (new
/// shortcuts are appended).
pub(crate) fn apply_shortcut(state: &mut ProjectState, shortcut: ShortcutStatus) {
    match state
        .shortcuts
        .iter_mut()
        .find(|candidate| candidate.id == shortcut.id)
    {
        Some(current) => *current = shortcut,
        None => state.shortcuts.push(shortcut),
    }
}
