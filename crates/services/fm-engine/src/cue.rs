//! Operator cue lists in the engine (RC-017).
//!
//! The show owns the persisted [`fm_model::CueList`]s. Three engine commands
//! act on them:
//!
//! - [`crate::EngineCommand::SetCueList`] creates, replaces, or removes one
//!   list's definition: a project edit honouring the project padlock, undone
//!   by restoring the previous definition. The run state carries across the
//!   edit (see [`fm_automation::CueStack::reconcile`]); an edit that would
//!   change or remove the cue whose deferred actions still wait is refused.
//! - [`crate::EngineCommand::CueControl`] fires the standby (or a named)
//!   cue, moves the standby pointer, drops a waiting cue's remaining
//!   actions, arms or disarms a GO for a later frame, or resets the list: a
//!   live operation without undo, journaled like every command.
//! - [`crate::EngineCommand::CueAdvance`] is the daemon clock's: it runs a
//!   waiting cue's remaining actions, or fires the armed GO (schema 58), on
//!   exactly their due frame (or the next frame once that has passed). A
//!   wait due on the same frame as the list's armed GO runs first; the
//!   armed GO then fires on the next frame.
//!
//! A GO and an advance run every action due on the frame as one accepted
//! command: the actions are expanded to ordinary engine commands, applied to
//! the desired show in order, and realized together by one runtime action on
//! the command's target frame. One refused action refuses a GO whole; an
//! advance whose action is refused is accepted with the list's fault set and
//! its remaining actions dropped, so the clock never retries it. The fault
//! keeps the refusal's typed reason and bounded message (schema 57,
//! Protocol 2.63), what a refused GO of the action would have said. An
//! armed GO is unattended like an advance: a refused action disarms it with
//! the fault set and applies nothing of its frame, and a cue still waiting
//! in the list keeps waiting.

use fm_automation::{CueStack, CueStackError, CueStackState, CueStep, StackCue};
use fm_command::{FaultDetail, FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode};
use fm_model::{
    Cue, CueAction, CueArmed, CueFault, CueList, CueMediaOperation, CuePending, CueRunState,
    TransitionEffect,
};
use fm_model::{ResolvedCueAction, TargetBindings};
use fm_switcher::{OverlayChannelId, RecorderSlot, StingerSlotId, StreamTargetId, TransitionKind};
use fm_titles::{FieldId, FieldType, FieldValue};
use fm_types::{FrameRate, InputTarget};

use crate::{
    EngineCommand, EngineMediaOperation, EngineMediaSeek, EnginePlaylistOperation,
    EnginePlaylistStart, ShowState,
};

/// One cue list definition: its name and cues.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineCueListDefinition {
    pub name: String,
    pub cues: Vec<Cue>,
}

/// A live cue list operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineCueOperation {
    /// Fires `cue`, or the standby cue with `None`. The list's dynamic
    /// targets bind under `targets` (roles left open read the show's
    /// Preview and Program) for the whole run the GO starts (RC-004).
    Go {
        cue: Option<u64>,
        targets: TargetBindings,
    },
    /// Puts `cue` on standby (jump).
    Standby { cue: u64 },
    /// Moves standby to the previous cue.
    Back,
    /// Moves standby past its cue without firing it.
    Skip,
    /// Drops the waiting cue's remaining actions and the armed GO.
    Stop,
    /// Arms a GO of `cue`, or of the standby cue with `None`, to fire
    /// `in_millis` milliseconds (`1..=`[`fm_model::MAX_CUE_ARM_MS`]) after
    /// the command's frame, converted to whole frames at the show rate and
    /// rounded up (schema 58). Replaces a GO armed before. The armed GO's
    /// dynamic targets bind under `targets` when it is armed (RC-004).
    Arm {
        cue: Option<u64>,
        in_millis: u32,
        targets: TargetBindings,
    },
    /// Drops the armed GO.
    Disarm,
    /// Returns the list to its initial state: nothing played, the first
    /// cue on standby.
    Reset,
}

pub(crate) fn unknown_list(list: u64) -> Rejection {
    Rejection::new(
        RejectionCode::NotFound,
        format!("the show has no cue list {list}"),
    )
    .with_reason(RefusalReason::UnknownId {
        kind: IdKind::CueList,
        id: u128::from(list),
    })
}

fn unknown_cue(list: u64, cue: u64) -> Rejection {
    Rejection::new(
        RejectionCode::NotFound,
        format!("cue list {list} has no cue {cue}"),
    )
    .with_reason(RefusalReason::UnknownId {
        kind: IdKind::Cue,
        id: u128::from(cue),
    })
}

pub(crate) fn stack_rejection(list: u64, error: CueStackError) -> Rejection {
    match error {
        CueStackError::UnknownCue(cue) => unknown_cue(list, cue),
        CueStackError::NothingOnStandby => Rejection::new(
            RejectionCode::Conflict,
            format!("cue list {list} has no cue on standby"),
        )
        .with_reason(RefusalReason::NoCueOnStandby),
        error => Rejection::new(RejectionCode::Conflict, format!("cue list {list}: {error}")),
    }
}

/// The automation stack of a list, its millisecond delays converted to
/// whole frames at `rate` (see [`fm_model::delay_frames`]). A list edit
/// only reconciles the run state, which never depends on a wait's length,
/// so it may pass any rate.
pub(crate) fn stack(list: &CueList, rate: FrameRate) -> CueStack<CueAction> {
    CueStack::new(
        list.cues
            .iter()
            .map(|cue| StackCue {
                id: cue.id,
                steps: cue
                    .actions
                    .iter()
                    .map(|action| match action {
                        CueAction::Wait { frames } => CueStep::Wait(*frames),
                        CueAction::Delay { millis } => {
                            CueStep::Wait(fm_model::delay_frames(*millis, rate))
                        }
                        action => CueStep::Action(action.clone()),
                    })
                    .collect(),
                auto_follow: cue.auto_follow,
            })
            .collect(),
    )
    .expect("validated cue lists have unique ids and nonzero waits")
}

pub(crate) fn stack_state(state: &CueRunState) -> CueStackState {
    CueStackState {
        standby: state.standby,
        live: state.live,
        pending: state.pending.map(|pending| fm_automation::CuePendingStep {
            cue: pending.cue,
            step: pending.action,
            due_frame: pending.due_frame,
        }),
        played: state.played.clone(),
        fault: state.fault.as_ref().map(|fault| (fault.cue, fault.action)),
        armed: state.armed.map(|armed| fm_automation::CueArmedGo {
            cue: armed.cue,
            due_frame: armed.due_frame,
        }),
    }
}

/// The run state of `state`, a fault the stack kept from `previous`
/// keeping its detail (schema 57). A fault the stack did not keep is new:
/// only a refused deferred action sets one, and the caller gives it the
/// refusal's detail (see [`fault`]).
pub(crate) fn run_state(state: CueStackState, previous: &CueRunState) -> CueRunState {
    CueRunState {
        standby: state.standby,
        live: state.live,
        pending: state.pending.map(|pending| CuePending {
            cue: pending.cue,
            action: pending.step,
            due_frame: pending.due_frame,
        }),
        played: state.played,
        fault: state.fault.map(|(cue, action)| {
            previous
                .fault
                .as_ref()
                .filter(|fault| fault.cue == cue && fault.action == action)
                .cloned()
                .unwrap_or_else(|| CueFault {
                    cue,
                    action,
                    detail: FaultDetail::new(None, ""),
                })
        }),
        armed: state.armed.map(|armed| CueArmed {
            cue: armed.cue,
            due_frame: armed.due_frame,
            targets: previous
                .armed
                .filter(|kept| kept.cue == armed.cue && kept.due_frame == armed.due_frame)
                .and_then(|kept| kept.targets),
        }),
        targets: previous.targets,
    }
}

/// The refusal of a dynamic target whose role is unbound (RC-004): no
/// input was selected on the console that fired it.
fn unbound(target: InputTarget) -> Rejection {
    Rejection::new(
        RejectionCode::InvalidCommand,
        format!("the action targets the {target} input, but none is bound"),
    )
    .with_field(FieldIssue::new(
        "selected",
        "unbound",
        format!("select an input on the console before firing an action on the {target} input"),
    ))
}

/// `action` with its target resolved under `targets` (RC-004). A dynamic
/// target is checked against `show` like a fixed one at definition time
/// (the input exists and is a title, media, or paged input as the action
/// needs); a fixed one was checked when it was defined.
///
/// # Errors
///
/// Refuses an unbound role and a resolved input the action cannot act on.
pub(crate) fn resolve_action(
    show: &ShowState,
    action: &CueAction,
    targets: &TargetBindings,
) -> Result<ResolvedCueAction, Rejection> {
    let resolved = action.resolve(targets).map_err(unbound)?;
    if action.is_dynamic() {
        check_resolved_references(show, &resolved, "the action")?;
    }
    Ok(resolved)
}

/// Refuses a GO or arm from `start` when an action of that cue, or of a
/// cue it auto-follows into, targets the selected input and the fire
/// carries no selection (RC-004): such a step after a wait would otherwise
/// fault the run later instead of refusing the fire now. Preview and
/// Program are always bound, so only the selection can be missing.
///
/// # Errors
///
/// The [`unbound`] refusal, naming the cue and action.
pub(crate) fn check_selection(
    list: &CueList,
    start: u64,
    targets: &TargetBindings,
) -> Result<(), Rejection> {
    if targets.selected.is_some() {
        return Ok(());
    }
    let Some(first) = list.cues.iter().position(|cue| cue.id == start) else {
        return Ok(());
    };
    for cue in &list.cues[first..] {
        if let Some(step) = cue
            .actions
            .iter()
            .position(|action| action.target() == Some(InputTarget::Selected))
        {
            let rejection = unbound(InputTarget::Selected);
            return Err(Rejection {
                message: format!("cue {} action {}: {}", cue.id, step + 1, rejection.message),
                ..rejection
            });
        }
        if !cue.auto_follow {
            break;
        }
    }
    Ok(())
}

/// The cue of `list` whose action still to run resolves a dynamic target
/// to `input` under the bindings its run in progress (or its armed GO)
/// holds (RC-004): the waiting cue from its next action, or the armed cue,
/// then each cue it auto-follows into. Removing that input would fault the
/// run at that action, so the removal is refused like that of an input a
/// cue names (see [`crate::ShowState::remove_input`]).
pub(crate) fn run_binding(list: &CueList, input: fm_types::InputId) -> Option<u64> {
    let state = &list.state;
    let waiting = state
        .pending
        .zip(state.targets)
        .map(|(pending, targets)| (pending.cue, pending.action, targets));
    let armed = state
        .armed
        .and_then(|armed| armed.targets.map(|targets| (armed.cue, 0, targets)));
    waiting
        .into_iter()
        .chain(armed)
        .find_map(|(cue, step, targets)| {
            let first = list.position(cue)?;
            let mut step = step;
            for cue in &list.cues[first..] {
                let binds = cue.actions.iter().skip(step).any(|action| {
                    action.is_dynamic()
                        && action.target().and_then(|target| target.resolve(&targets))
                            == Some(input)
                });
                if binds {
                    return Some(cue.id);
                }
                if !cue.auto_follow {
                    break;
                }
                step = 0;
            }
            None
        })
}

/// The fault of action `action` of cue `cue`, refused with `rejection`
/// when its wait ended (schema 57): the refusal's typed reason and
/// message, exactly as a refused GO of the action would have said.
pub(crate) fn fault(cue: u64, action: usize, rejection: &Rejection) -> CueFault {
    CueFault {
        cue,
        action,
        detail: FaultDetail::of(rejection),
    }
}

/// Refuses a cue naming an input, title, or stream target outside `state`.
///
/// # Errors
///
/// Returns the refusal for the first unknown reference.
pub(crate) fn check_references(state: &ShowState, cues: &[Cue]) -> Result<(), Rejection> {
    for cue in cues {
        for action in &cue.actions {
            check_action_references(state, action, &format!("cue {}", cue.id))?;
        }
    }
    Ok(())
}

/// Refuses one action naming an input, title, or stream target outside
/// `state`; `owner` names what holds the action in the message. A dynamic
/// target is checked when it is fired (see [`resolve_action`]).
///
/// # Errors
///
/// Returns the refusal for the first unknown reference.
pub(crate) fn check_action_references(
    state: &ShowState,
    action: &CueAction,
    owner: &str,
) -> Result<(), Rejection> {
    match action.as_fixed() {
        Some(fixed) => check_resolved_references(state, &fixed, owner),
        None => Ok(()),
    }
}

/// [`check_action_references`] of an action naming fixed inputs.
///
/// # Errors
///
/// Returns the refusal for the first unknown reference.
pub(crate) fn check_resolved_references(
    state: &ShowState,
    action: &ResolvedCueAction,
    owner: &str,
) -> Result<(), Rejection> {
    if let Some(input) = action.input() {
        if !state.inputs().contains(&input) {
            return Err(Rejection::new(
                RejectionCode::NotFound,
                format!("{owner} names input {input}, which the show lacks"),
            )
            .with_reason(crate::error::unknown_input_reason(input)));
        }
        if matches!(action, CueAction::Media { .. }) && state.media_settings(input).is_none() {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                format!("{owner} names input {input}, which is not a media input"),
            )
            .with_field(FieldIssue::new(
                "cues",
                "not_media",
                format!("input {input} is not a media input"),
            )));
        }
        let title = matches!(
            action,
            CueAction::TitleOn { .. }
                | CueAction::TitleText { .. }
                | CueAction::TitleNumber { .. }
                | CueAction::TitleOff { .. }
        );
        if title && state.title(input).is_none() {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                format!("{owner} names input {input}, which is not a title"),
            )
            .with_reason(RefusalReason::UnknownId {
                kind: IdKind::Title,
                id: input.get().get(),
            }));
        }
        if let CueAction::TitleNumber { field, .. } = action {
            number_field(state, input, FieldId::new(field.get()), owner)?;
        }
    }
    if let Some(script) = action.script() {
        match state.script(script) {
            None => return Err(crate::script::unknown_script(script)),
            // A script step runs on its cue's frame (schema 56).
            Some(found) if fm_model::script_waits(&found.source) => {
                return Err(crate::script::waits_in_cue(script, owner));
            }
            Some(_) => {}
        }
    }
    if let CueAction::Stream { target, .. } = action {
        let target = StreamTargetId::from_non_zero(target.get());
        if state.desired_switcher().stream_running(target).is_none() {
            return Err(Rejection::new(
                RejectionCode::NotFound,
                format!(
                    "{owner} names stream target {}, which the show lacks",
                    target.get()
                ),
            ));
        }
    }
    Ok(())
}

/// Refuses a number update of a title field that is missing or neither a
/// number nor a text field (schema 59); returns the field's type.
fn number_field(
    state: &ShowState,
    input: fm_types::InputId,
    field: FieldId,
    owner: &str,
) -> Result<FieldType, Rejection> {
    let found = state
        .title(input)
        .and_then(|title| title.fields.field_type(field));
    match found {
        Some(kind @ (FieldType::Number | FieldType::Text)) => Ok(kind),
        Some(FieldType::Image | FieldType::Color) => Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "{owner} updates field {} of title {input}, which is not a number or text field",
                field.get()
            ),
        )
        .with_field(FieldIssue::new(
            "cues",
            "not_number",
            format!("field {} is not a number or text field", field.get()),
        ))),
        None => Err(Rejection::new(
            RejectionCode::NotFound,
            format!(
                "{owner} updates field {} of title {input}, which the title lacks",
                field.get()
            ),
        )
        .with_field(FieldIssue::new(
            "cues",
            "unknown_field",
            format!("title {input} has no field {}", field.get()),
        ))),
    }
}

/// The value a [`CueAction::TitleNumber`] gives its field on `state`, the
/// show as earlier actions of the frame left it (schema 59): a number for a
/// number field, decimal text for a text field holding a plain integer.
///
/// # Errors
///
/// Refuses a missing or unsuitable field, text that is not a plain
/// integer, and a result beyond [`fm_model::MAX_CUE_TITLE_NUMBER`].
pub(crate) fn title_number_value(
    state: &ShowState,
    input: fm_types::InputId,
    field: FieldId,
    update: fm_model::CueNumberUpdate,
) -> Result<FieldValue, Rejection> {
    let kind = number_field(state, input, field, "the action")?;
    // Setting needs no current value; stepping needs a whole number.
    let current = match (
        update,
        state
            .title(input)
            .and_then(|title| title.fields.value(field)),
    ) {
        (fm_model::CueNumberUpdate::Set(_), _) => Some(0),
        (_, Some(FieldValue::Number(number))) => Some(*number),
        (_, Some(FieldValue::Text(text))) => fm_model::title_text_number(text),
        _ => None,
    };
    let Some(current) = current else {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "field {} of title {input} does not hold a whole number",
                field.get()
            ),
        )
        .with_field(FieldIssue::new(
            "cues",
            "not_number",
            format!("field {} does not hold a whole number", field.get()),
        )));
    };
    let max = fm_model::MAX_CUE_TITLE_NUMBER;
    let value = update.apply(current).ok_or_else(|| {
        Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "field {} of title {input} would leave -{max} to {max}",
                field.get()
            ),
        )
        .with_reason(RefusalReason::bounds("title_number", max.unsigned_abs()))
    })?;
    Ok(match kind {
        FieldType::Text => FieldValue::Text(value.to_string()),
        _ => FieldValue::Number(value),
    })
}

/// The refusal of a list id of zero: the cue clock's list `0` is the
/// waiting script run (schema 56).
fn zero_list() -> Rejection {
    Rejection::new(
        RejectionCode::InvalidCommand,
        "cue list id must not be zero",
    )
    .with_field(FieldIssue::new(
        "list",
        "zero",
        "cue list id must not be zero",
    ))
}

/// Applies a [`crate::EngineCommand::SetCueList`] to `state`.
///
/// # Errors
///
/// Refuses list id zero (the cue clock's list `0` is the waiting script
/// run, schema 56), an invalid definition, an unknown reference, a list
/// past the project bound, removing an unknown list, and changing or
/// removing a cue whose deferred actions still wait. `position` places the
/// list in the project's cue lists (see [`crate::EngineCommand::SetCueList`]).
pub(crate) fn apply_set(
    state: &mut ShowState,
    list: u64,
    definition: Option<&EngineCueListDefinition>,
    position: Option<usize>,
) -> Result<(), Rejection> {
    if list == 0 && definition.is_some() {
        return Err(zero_list());
    }
    replace(state, list, definition, position)
}

/// Creates, replaces, or removes list `list` (see [`apply_set`]).
fn replace(
    state: &mut ShowState,
    list: u64,
    definition: Option<&EngineCueListDefinition>,
    position: Option<usize>,
) -> Result<(), Rejection> {
    let existing = state
        .cue_lists()
        .iter()
        .position(|candidate| candidate.id == list);
    let current = existing.map(|index| &state.cue_lists()[index]);
    let Some(definition) = definition else {
        let index = existing.ok_or_else(|| unknown_list(list))?;
        guard_running(current, None)?;
        state.cue_lists_mut().remove(index);
        return Ok(());
    };
    fm_model::validate_cue_list_definition(&definition.name, &definition.cues).map_err(
        |error| {
            let message = error.to_string();
            let reason = match error {
                fm_model::CueListError::InvalidName { .. } => RefusalReason::InvalidName {
                    max_bytes: u32::try_from(fm_model::MAX_CUE_NAME_BYTES).unwrap_or(u32::MAX),
                },
                fm_model::CueListError::TooManyCues { .. } => {
                    RefusalReason::bounds("cues", fm_model::MAX_CUES as u64)
                }
                fm_model::CueListError::TooManyActions { .. } => {
                    RefusalReason::bounds("actions", fm_model::MAX_CUE_ACTIONS as u64)
                }
                fm_model::CueListError::TooManyListActions { .. } => {
                    RefusalReason::bounds("list_actions", fm_model::MAX_CUE_LIST_ACTIONS as u64)
                }
                fm_model::CueListError::TooManyScriptSteps { .. } => RefusalReason::bounds(
                    "script_steps",
                    fm_model::MAX_CUE_LIST_SCRIPT_STEPS as u64,
                ),
                fm_model::CueListError::TooMuchText { .. } => {
                    RefusalReason::bounds("text", fm_model::MAX_CUE_LIST_TEXT_BYTES as u64)
                }
                fm_model::CueListError::DuplicateCue(cue) => RefusalReason::DuplicateId {
                    kind: IdKind::Cue,
                    id: u128::from(cue),
                },
                fm_model::CueListError::ConflictingFollowTakes { .. } => {
                    return Rejection::new(RejectionCode::InvalidCommand, message.clone())
                        .with_field(FieldIssue::new("cues", "conflicting_follow_takes", message));
                }
                fm_model::CueListError::InvalidAction { .. }
                | fm_model::CueListError::ConflictingTakes { .. }
                | fm_model::CueListError::InvalidState => {
                    return Rejection::new(RejectionCode::InvalidCommand, message.clone())
                        .with_field(FieldIssue::new("cues", "invalid_cue", message));
                }
            };
            Rejection::new(RejectionCode::InvalidCommand, message).with_reason(reason)
        },
    )?;
    check_references(state, &definition.cues)?;
    guard_running(
        existing.map(|index| &state.cue_lists()[index]),
        Some(&definition.cues),
    )?;
    let mut next = CueList {
        id: list,
        name: definition.name.clone(),
        cues: definition.cues.clone(),
        state: CueRunState::default(),
    };
    // Reconciling the run state never reads a wait's length.
    let new_stack = stack(
        &next,
        FrameRate::new(25, 1).expect("25 fps is a valid rate"),
    );
    if let Some(index) = existing {
        let current = &state.cue_lists()[index];
        let previous: Vec<u64> = current.cues.iter().map(|cue| cue.id).collect();
        let reconciled = new_stack.reconcile(&previous, &stack_state(&current.state));
        next.state = run_state(reconciled, &current.state);
        crate::state::place_in_list(state.cue_lists_mut(), Some(index), next, position);
    } else {
        {
            if state.cue_lists().len() >= fm_model::MAX_CUE_LISTS {
                let message = format!(
                    "the show already holds the maximum of {} cue lists",
                    fm_model::MAX_CUE_LISTS
                );
                return Err(
                    Rejection::new(RejectionCode::Conflict, message).with_reason(
                        RefusalReason::bounds("cue_lists", fm_model::MAX_CUE_LISTS as u64),
                    ),
                );
            }
            next.state = run_state(new_stack.initial_state(), &CueRunState::default());
            crate::state::place_in_list(state.cue_lists_mut(), None, next, position);
        }
    }
    Ok(())
}

/// Refuses an edit of `current` to `cues` (a removal with `None`) that
/// changes or removes the cue whose deferred actions still wait, or
/// removes the armed cue. The armed cue may change (the armed authority
/// covers the edit, see `fm-control`), but not go away.
fn guard_running(current: Option<&CueList>, cues: Option<&[Cue]>) -> Result<(), Rejection> {
    let Some(current) = current else {
        return Ok(());
    };
    if let Some(cue) = current
        .state
        .pending
        .and_then(|pending| current.cue(pending.cue))
        && cues.is_none_or(|cues| !cues.contains(cue))
    {
        return Err(running(cue.id));
    }
    if let Some(armed) = current.state.armed
        && cues.is_none_or(|cues| !cues.iter().any(|cue| cue.id == armed.cue))
    {
        return Err(armed_cue(armed.cue));
    }
    Ok(())
}

fn running(cue: u64) -> Rejection {
    Rejection::new(
        RejectionCode::Conflict,
        format!("cue {cue} is still running; stop it before changing it"),
    )
    .with_reason(RefusalReason::CueRunning { cue })
}

fn armed_cue(cue: u64) -> Rejection {
    Rejection::new(
        RejectionCode::Conflict,
        format!("cue {cue} is armed; disarm it before removing it"),
    )
    .with_reason(RefusalReason::CueRunning { cue })
}

/// The refusal of an armed GO delay outside `1..=`[`fm_model::MAX_CUE_ARM_MS`].
pub(crate) fn arm_delay_out_of_range() -> Rejection {
    let message = format!(
        "an armed GO fires 1 to {} milliseconds after it is armed",
        fm_model::MAX_CUE_ARM_MS
    );
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        "in_ms",
        "out_of_range",
        message,
    ))
}

/// The definition of list `list` in `state`, for an undo.
pub(crate) fn definition(state: &ShowState, list: u64) -> Option<EngineCueListDefinition> {
    state.cue_list(list).map(|list| EngineCueListDefinition {
        name: list.name.clone(),
        cues: list.cues.clone(),
    })
}

const fn switcher_kind(effect: TransitionEffect) -> Option<TransitionKind> {
    Some(match effect {
        TransitionEffect::Fade => TransitionKind::Fade,
        TransitionEffect::AlphaFade => TransitionKind::AlphaFade,
        TransitionEffect::Wipe => TransitionKind::Wipe,
        TransitionEffect::VerticalWipe => TransitionKind::VerticalWipe,
        TransitionEffect::Slide => TransitionKind::Slide,
        TransitionEffect::VerticalSlide => TransitionKind::VerticalSlide,
        TransitionEffect::Zoom => TransitionKind::Zoom,
        TransitionEffect::Fly => TransitionKind::Fly,
        TransitionEffect::FlyRotate => TransitionKind::FlyRotate,
        TransitionEffect::Cube => TransitionKind::Cube,
        TransitionEffect::CubeZoom => TransitionKind::CubeZoom,
        TransitionEffect::CrossZoom => TransitionKind::CrossZoom,
        TransitionEffect::Merge => TransitionKind::Merge,
        TransitionEffect::Cut | TransitionEffect::Stinger(_) => return None,
    })
}

/// Expands one cue action into the engine commands that perform it on the
/// show as it stands (`state`, which already holds the effect of every
/// earlier action on the frame). `on_air` reports whether a title is on air
/// in the realized switcher.
///
/// # Errors
///
/// Refuses a title number update its field cannot take (see
/// [`title_number_value`]).
#[allow(clippy::too_many_lines)] // one arm per action kind
pub(crate) fn expand(
    state: &ShowState,
    action: &ResolvedCueAction,
    on_air: impl Fn(fm_types::InputId) -> bool,
) -> Result<Vec<EngineCommand>, Rejection> {
    let channel = |number: u8| OverlayChannelId::new(number).expect("validated overlay channel");
    let switcher = state.desired_switcher();
    let set_field = |input: fm_types::InputId, field: FieldId, value: FieldValue| {
        let live = switcher.on_air_usage(input).is_some() || on_air(input);
        EngineCommand::SetTitleFields {
            input,
            base_revision: title_revision(state, input),
            require: if live {
                crate::TitleAirRequirement::OnAir
            } else {
                crate::TitleAirRequirement::OffAir
            },
            fields: vec![(field, value)],
        }
    };
    Ok(match action {
        CueAction::Preview { input } => vec![EngineCommand::SelectPreview(*input)],
        CueAction::Take {
            input,
            effect,
            duration_frames,
        } => {
            let mut commands = Vec::new();
            if let Some(input) = input {
                if switcher.program() == *input {
                    return Ok(commands);
                }
                if switcher.preview() != *input {
                    commands.push(EngineCommand::SelectPreview(*input));
                }
            }
            commands.push(match (effect, switcher_kind(*effect)) {
                (TransitionEffect::Stinger(slot), _) => EngineCommand::Stinger {
                    slot: StingerSlotId::new(slot.number()).expect("model stinger slots match"),
                    duration_frames: *duration_frames,
                },
                (_, Some(kind)) => EngineCommand::Transition {
                    kind,
                    duration_frames: *duration_frames,
                },
                (_, None) => EngineCommand::Cut,
            });
            commands
        }
        CueAction::OverlayOn {
            channel: number,
            input,
        } => vec![EngineCommand::TakeOverlay {
            channel: channel(*number),
            source: *input,
        }],
        CueAction::OverlayOff { channel: number } => vec![EngineCommand::OverlayOff {
            channel: channel(*number),
        }],
        CueAction::TitleOn {
            input,
            channel: number,
        } => vec![EngineCommand::TakeTitle {
            input: *input,
            channel: channel(*number),
            base_revision: title_revision(state, *input),
            fields: Vec::new(),
        }],
        CueAction::TitleText { input, field, text } => vec![set_field(
            *input,
            FieldId::new(field.get()),
            FieldValue::Text(text.clone()),
        )],
        CueAction::TitleNumber {
            input,
            field,
            update,
        } => {
            let field = FieldId::new(field.get());
            let value = title_number_value(state, *input, field, *update)?;
            vec![set_field(*input, field, value)]
        }
        CueAction::TitleOff { input } => vec![EngineCommand::TitleOffAir { input: *input }],
        CueAction::FadeToBlack {
            active,
            duration_frames,
        } => vec![EngineCommand::FadeToBlack {
            active: *active,
            duration_frames: *duration_frames,
        }],
        CueAction::PlaylistStart => vec![EngineCommand::PlaylistControl(
            EnginePlaylistOperation::Start {
                at: EnginePlaylistStart::Now,
                from: None,
            },
        )],
        CueAction::PlaylistStop => vec![EngineCommand::PlaylistControl(
            EnginePlaylistOperation::Stop,
        )],
        CueAction::AudioGain {
            input,
            gain_millidb,
        } => state
            .input_audio_strip(*input)
            .map(|strip| EngineCommand::SetInputAudioStrip {
                input: *input,
                state: crate::EngineInputAudioStripState {
                    gain_millidb: *gain_millidb,
                    ..strip
                },
            })
            .into_iter()
            .collect(),
        CueAction::AudioMute { input, muted } => state
            .input_audio_strip(*input)
            .map(|strip| EngineCommand::SetInputAudioStrip {
                input: *input,
                state: crate::EngineInputAudioStripState {
                    muted: *muted,
                    ..strip
                },
            })
            .into_iter()
            .collect(),
        CueAction::Record { recorder, start } => {
            let recorder = RecorderSlot::new(recorder.get()).expect("model recorder slots match");
            vec![if *start {
                EngineCommand::RecordStart { recorder }
            } else {
                EngineCommand::RecordStop { recorder }
            }]
        }
        CueAction::Stream { target, start } => {
            let target = StreamTargetId::from_non_zero(target.get());
            vec![if *start {
                EngineCommand::StreamStart { target }
            } else {
                EngineCommand::StreamStop { target }
            }]
        }
        CueAction::Page { input, navigation } => vec![EngineCommand::PageNavigate {
            input: *input,
            navigation: *navigation,
        }],
        CueAction::Media { input, operation } => {
            let media = |operation| {
                EngineCommand::Media(crate::EngineMediaCommand {
                    input: *input,
                    operation,
                })
            };
            match operation {
                CueMediaOperation::Play => vec![media(EngineMediaOperation::Play)],
                CueMediaOperation::Pause => vec![media(EngineMediaOperation::Pause)],
                CueMediaOperation::Seek { frame } => {
                    vec![media(EngineMediaOperation::Seek(EngineMediaSeek::Frame(
                        *frame,
                    )))]
                }
                // The seek clamps to the trimmed range's first frame.
                CueMediaOperation::Restart => vec![
                    media(EngineMediaOperation::Seek(EngineMediaSeek::Frame(0))),
                    media(EngineMediaOperation::Play),
                ],
            }
        }
        // A script step is run by `Engine::apply_cue_actions`.
        CueAction::Wait { .. } | CueAction::Delay { .. } | CueAction::RunScript { .. } => {
            Vec::new()
        }
    })
}

fn title_revision(state: &ShowState, input: fm_types::InputId) -> u64 {
    state
        .title(input)
        .map_or(0, |title| title.fields.revision())
}

/// Whether `command` takes Program (a cut, a timed transition, a Stinger,
/// or a playlist control that may put an entry on air).
pub(crate) const fn takes_program(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::Cut
            | EngineCommand::Transition { .. }
            | EngineCommand::Stinger { .. }
            | EngineCommand::PlaylistControl(EnginePlaylistOperation::Start { .. })
    )
}

/// The earliest cue advance due: `(list, due frame)` of the waiting cue or
/// armed GO with the smallest due frame, the first list in order on a tie.
#[must_use]
pub fn cue_due(lists: &[CueList]) -> Option<(u64, u64)> {
    lists
        .iter()
        .filter_map(|list| {
            let pending = list.state.pending.map(|pending| pending.due_frame);
            let armed = list.state.armed.map(|armed| armed.due_frame);
            match (pending, armed) {
                (Some(pending), Some(armed)) => Some(pending.min(armed)),
                (pending, armed) => pending.or(armed),
            }
            .map(|due| (list.id, due))
        })
        .min_by_key(|(_, due)| *due)
}
