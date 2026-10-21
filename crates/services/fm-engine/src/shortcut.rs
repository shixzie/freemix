//! User shortcuts in the engine (RC-004).
//!
//! The show owns the persisted [`fm_model::Shortcut`]s. Two engine commands
//! act on them:
//!
//! - [`crate::EngineCommand::SetShortcut`] creates, replaces, or removes one
//!   shortcut: a project edit honouring the project padlock, undone by
//!   restoring the previous definition. A key or OSC name another shortcut
//!   already uses is refused.
//! - [`crate::EngineCommand::FireShortcut`] fires one: a disabled or unknown
//!   shortcut is refused, and an enabled one resolves, before it is
//!   scheduled, into the command it stands for. A cue list operation
//!   becomes that [`crate::EngineCommand::CueControl`] and a script run that
//!   [`crate::EngineCommand::RunScript`], each then accepted or refused
//!   exactly as if it had been sent directly (with its own event). A
//!   shortcut that runs cue actions stays a `FireShortcut` and is planned
//!   here like the first frame of a cue GO: every action is checked against
//!   the show (an input, title, or stream target it names must exist),
//!   expanded to ordinary engine commands, and applied in order to a copy of
//!   the show; one refused action refuses the whole fire, and the accepted
//!   actions are realized together on the command's target frame. A fire is
//!   a journaled live operation without undo.
//!
//! An action taking its gain from the firing control (RC-004 dynamic
//! values, [`fm_model::CueGain::Fired`]) reads the fire's value, fixed
//! into a set gain before the fire is planned; a fire without one is
//! refused whole (field `value`, `unbound`). A step or toggle reads the
//! strip as it stands when the action runs.

use fm_automation::FiredCueAction;
use fm_command::{FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode};
use fm_model::{
    FireValue, Shortcut, ShortcutAction, ShortcutCueOperation, ShortcutError, TargetBindings,
};
use fm_scheduler::FrameNumber;

use crate::engine::{CueFrame, CuePlan};
use crate::{Engine, EngineCommand, EngineCueOperation, EngineEvent, ShowState};

pub(crate) fn unknown_shortcut(shortcut: u64) -> Rejection {
    Rejection::new(
        RejectionCode::NotFound,
        format!("the show has no shortcut {shortcut}"),
    )
    .with_reason(RefusalReason::UnknownId {
        kind: IdKind::Shortcut,
        id: u128::from(shortcut),
    })
}

fn invalid(error: &ShortcutError) -> Rejection {
    let message = error.to_string();
    match error {
        ShortcutError::InvalidName => Rejection::new(RejectionCode::InvalidCommand, message)
            .with_reason(RefusalReason::InvalidName {
                max_bytes: u32::try_from(fm_model::MAX_SHORTCUT_NAME_BYTES).unwrap_or(u32::MAX),
            }),
        ShortcutError::DuplicateKey { .. } => Rejection::new(RejectionCode::Conflict, message)
            .with_field(FieldIssue::new(
                "key",
                "key_in_use",
                "another shortcut already uses this key",
            )),
        ShortcutError::DuplicateOsc { .. } => Rejection::new(RejectionCode::Conflict, message)
            .with_field(FieldIssue::new(
                "osc",
                "osc_in_use",
                "another shortcut already uses this OSC name",
            )),
        ShortcutError::TooManyShortcuts => Rejection::new(RejectionCode::Conflict, message)
            .with_reason(RefusalReason::bounds(
                "shortcuts",
                fm_model::MAX_SHORTCUTS as u64,
            )),
        _ => Rejection::new(RejectionCode::InvalidCommand, message),
    }
}

/// Applies a [`crate::EngineCommand::SetShortcut`] to `state`.
///
/// # Errors
///
/// Refuses shortcut id zero, an invalid definition, a definition whose id
/// differs from the command's, a key or OSC name another shortcut uses, a
/// shortcut past the project bound, cue actions running a script that
/// waits, and removing an unknown shortcut. `position` places the
/// shortcut in the project's list (see [`crate::EngineCommand::SetShortcut`]).
pub(crate) fn apply_set(
    state: &mut ShowState,
    shortcut: u64,
    definition: Option<&Shortcut>,
    position: Option<usize>,
) -> Result<(), Rejection> {
    let existing = state
        .shortcuts()
        .iter()
        .position(|candidate| candidate.id == shortcut);
    let Some(definition) = definition else {
        let index = existing.ok_or_else(|| unknown_shortcut(shortcut))?;
        state.shortcuts_mut().remove(index);
        return Ok(());
    };
    if shortcut == 0 || definition.id != shortcut {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            "shortcut id must be nonzero and match its definition",
        )
        .with_field(FieldIssue::new(
            "shortcut",
            "zero",
            "shortcut id must not be zero",
        )));
    }
    fm_model::validate_shortcut_definition(definition).map_err(|error| invalid(&error))?;
    // Its actions run on one frame, like a cue's: a script step among them
    // cannot wait. A script the show lacks is refused only when fired.
    if let ShortcutAction::Actions(actions) = &definition.action
        && let Some(script) =
            actions
                .iter()
                .filter_map(fm_model::CueAction::script)
                .find(|script| {
                    state
                        .script(*script)
                        .is_some_and(|found| fm_model::script_waits(&found.source))
                })
    {
        return Err(crate::script::waits_in_cue(
            script,
            &format!("shortcut {shortcut}"),
        ));
    }
    let mut next = state.shortcuts().to_vec();
    crate::state::place_in_list(&mut next, existing, definition.clone(), position);
    fm_model::validate_shortcuts(&next).map_err(|error| invalid(&error))?;
    *state.shortcuts_mut() = next;
    Ok(())
}

/// The definition of shortcut `shortcut` in `state`, for an undo.
pub(crate) fn definition(state: &ShowState, shortcut: u64) -> Option<Box<Shortcut>> {
    state.shortcut(shortcut).cloned().map(Box::new)
}

/// The command a fire of `shortcut` stands for on `state`: the cue list
/// operation, the script run, or the fire itself for cue actions.
///
/// # Errors
///
/// Refuses an unknown or disabled shortcut.
pub(crate) fn resolve(
    state: &ShowState,
    shortcut: u64,
    targets: TargetBindings,
    value: Option<FireValue>,
) -> Result<EngineCommand, Rejection> {
    let found = state
        .shortcut(shortcut)
        .ok_or_else(|| unknown_shortcut(shortcut))?;
    if !found.enabled {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            format!("shortcut {shortcut} is disabled"),
        )
        .with_field(FieldIssue::new(
            "shortcut",
            "disabled",
            "enable the shortcut before firing it",
        )));
    }
    Ok(match &found.action {
        ShortcutAction::Actions(_) => EngineCommand::FireShortcut {
            shortcut,
            targets,
            value,
        },
        ShortcutAction::Cue { list, operation } => EngineCommand::CueControl {
            list: *list,
            operation: match operation {
                ShortcutCueOperation::Go { cue } => EngineCueOperation::Go { cue: *cue, targets },
                ShortcutCueOperation::Stop => EngineCueOperation::Stop,
                ShortcutCueOperation::Back => EngineCueOperation::Back,
                ShortcutCueOperation::Skip => EngineCueOperation::Skip,
                ShortcutCueOperation::Reset => EngineCueOperation::Reset,
            },
        },
        ShortcutAction::Script { script } => EngineCommand::RunScript { script: *script },
    })
}

/// The refusal of a fire without a value of an action taking its value
/// from the firing control (RC-004).
fn unbound_value(owner: &str, step: usize) -> Rejection {
    Rejection::new(
        RejectionCode::InvalidCommand,
        format!(
            "{owner} action {}: the action takes its value from the firing control, \
             but the fire carries none",
            step + 1
        ),
    )
    .with_field(FieldIssue::new(
        "value",
        "unbound",
        "fire the shortcut from a control that sends a value (an OSC float from 0 to 1)",
    ))
}

/// Plans a fire of `shortcut`'s cue actions for `target_frame` (see the
/// module documentation).
///
/// # Errors
///
/// Refuses an unknown or disabled shortcut, an action naming something the
/// show lacks, and the first action the show refuses.
pub(crate) fn plan(
    engine: &Engine,
    shortcut: u64,
    targets: TargetBindings,
    value: Option<FireValue>,
    target_frame: FrameNumber,
) -> Result<CuePlan, Rejection> {
    let state = engine.show();
    let found = state
        .shortcut(shortcut)
        .ok_or_else(|| unknown_shortcut(shortcut))?;
    let ShortcutAction::Actions(actions) = &found.action else {
        return Err(Rejection::new(
            RejectionCode::Internal,
            "a cue or script shortcut resolves before it is planned",
        ));
    };
    if !found.enabled {
        return Err(
            resolve(state, shortcut, targets, value).expect_err("a disabled shortcut is refused")
        );
    }
    let owner = format!("shortcut {shortcut}");
    // Dynamic targets bind to the show the fire is planned on (RC-004).
    let targets = engine.bind_targets(targets);
    for (step, action) in actions.iter().enumerate() {
        if action.is_dynamic() {
            crate::cue::resolve_action(state, action, &targets).map_err(|rejection| Rejection {
                message: format!("{owner} action {}: {}", step + 1, rejection.message),
                ..rejection
            })?;
        } else {
            crate::cue::check_action_references(state, action, &owner)?;
        }
    }
    // A value read from the firing control is fixed before the actions
    // are planned (RC-004 dynamic values).
    let fired = actions
        .iter()
        .enumerate()
        .map(|(step, action)| {
            action
                .with_fire_value(value)
                .map(|action| FiredCueAction {
                    cue: shortcut,
                    step,
                    action,
                })
                .ok_or_else(|| unbound_value(&owner, step))
        })
        .collect::<Result<Vec<FiredCueAction<fm_model::CueAction>>, Rejection>>()?;
    let mut show = state.clone();
    let mut runtime = Vec::new();
    let mut transition = None;
    let mut title_texts = Vec::new();
    let mut scripts = Vec::new();
    engine
        .apply_cue_actions(
            &mut show,
            &fired,
            target_frame,
            &mut CueFrame {
                runtime: &mut runtime,
                transition: &mut transition,
                title_texts: &mut title_texts,
                scripts: Some(&mut scripts),
                takes: 0,
                targets,
            },
        )
        .map_err(|(_, step, rejection)| Rejection {
            message: format!("{owner} action {}: {}", step + 1, rejection.message),
            ..rejection
        })?;
    Ok(CuePlan {
        show: Box::new(show),
        event: EngineEvent::ShortcutFired {
            shortcut,
            frame: target_frame.get(),
            actions: u32::try_from(actions.len()).unwrap_or(u32::MAX),
            title_texts,
            applied: runtime.clone(),
            scripts,
        },
        runtime,
        transition,
    })
}
