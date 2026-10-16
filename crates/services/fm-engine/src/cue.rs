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
//!   actions, or resets the list: a live operation without undo, journaled
//!   like every command.
//! - [`crate::EngineCommand::CueAdvance`] is the daemon clock's: it runs a
//!   waiting cue's remaining actions on exactly their due frame (or the next
//!   frame once that has passed).
//!
//! A GO and an advance run every action due on the frame as one accepted
//! command: the actions are expanded to ordinary engine commands, applied to
//! the desired show in order, and realized together by one runtime action on
//! the command's target frame. One refused action refuses a GO whole; an
//! advance whose action is refused is accepted with the list's fault set and
//! its remaining actions dropped, so the clock never retries it.

use fm_automation::{CueStack, CueStackError, CueStackState, CueStep, StackCue};
use fm_command::{FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode};
use fm_model::{
    Cue, CueAction, CueFault, CueList, CueMediaOperation, CuePending, CueRunState, TransitionEffect,
};
use fm_switcher::{OverlayChannelId, RecorderSlot, StingerSlotId, StreamTargetId, TransitionKind};
use fm_titles::{FieldId, FieldValue};
use fm_types::FrameRate;

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
    /// Fires `cue`, or the standby cue with `None`.
    Go { cue: Option<u64> },
    /// Puts `cue` on standby (jump).
    Standby { cue: u64 },
    /// Moves standby to the previous cue.
    Back,
    /// Moves standby past its cue without firing it.
    Skip,
    /// Drops the waiting cue's remaining actions.
    Stop,
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
        fault: state.fault.map(|fault| (fault.cue, fault.action)),
    }
}

pub(crate) fn run_state(state: CueStackState) -> CueRunState {
    CueRunState {
        standby: state.standby,
        live: state.live,
        pending: state.pending.map(|pending| CuePending {
            cue: pending.cue,
            action: pending.step,
            due_frame: pending.due_frame,
        }),
        played: state.played,
        fault: state.fault.map(|(cue, action)| CueFault { cue, action }),
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
            if let Some(input) = action.input() {
                if !state.inputs().contains(&input) {
                    return Err(Rejection::new(
                        RejectionCode::NotFound,
                        format!("cue {} names input {input}, which the show lacks", cue.id),
                    )
                    .with_reason(crate::error::unknown_input_reason(input)));
                }
                if matches!(action, CueAction::Media { .. })
                    && state.media_settings(input).is_none()
                {
                    return Err(Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!(
                            "cue {} names input {input}, which is not a media input",
                            cue.id
                        ),
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
                        | CueAction::TitleOff { .. }
                );
                if title && state.title(input).is_none() {
                    return Err(Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!("cue {} names input {input}, which is not a title", cue.id),
                    )
                    .with_reason(RefusalReason::UnknownId {
                        kind: IdKind::Title,
                        id: input.get().get(),
                    }));
                }
            }
            if let CueAction::Stream { target, .. } = action {
                let target = StreamTargetId::from_non_zero(target.get());
                if state.desired_switcher().stream_running(target).is_none() {
                    return Err(Rejection::new(
                        RejectionCode::NotFound,
                        format!(
                            "cue {} names stream target {}, which the show lacks",
                            cue.id,
                            target.get()
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Applies a [`crate::EngineCommand::SetCueList`] to `state`.
///
/// # Errors
///
/// Refuses an invalid definition, an unknown reference, a list past the
/// project bound, removing an unknown list, and changing or removing a cue
/// whose deferred actions still wait.
pub(crate) fn apply_set(
    state: &mut ShowState,
    list: u64,
    definition: Option<&EngineCueListDefinition>,
) -> Result<(), Rejection> {
    let existing = state
        .cue_lists()
        .iter()
        .position(|candidate| candidate.id == list);
    let waiting = existing.and_then(|index| {
        let current = &state.cue_lists()[index];
        current
            .state
            .pending
            .and_then(|pending| current.cue(pending.cue).cloned())
    });
    let Some(definition) = definition else {
        let index = existing.ok_or_else(|| unknown_list(list))?;
        if let Some(cue) = waiting {
            return Err(running(cue.id));
        }
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
                fm_model::CueListError::TooMuchText { .. } => {
                    RefusalReason::bounds("text", fm_model::MAX_CUE_LIST_TEXT_BYTES as u64)
                }
                fm_model::CueListError::DuplicateCue(cue) => RefusalReason::DuplicateId {
                    kind: IdKind::Cue,
                    id: u128::from(cue),
                },
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
    if let Some(cue) = waiting
        && !definition.cues.contains(&cue)
    {
        return Err(running(cue.id));
    }
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
        next.state = run_state(new_stack.reconcile(&previous, &stack_state(&current.state)));
        state.cue_lists_mut()[index] = next;
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
            next.state = run_state(new_stack.initial_state());
            state.cue_lists_mut().push(next);
        }
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
#[allow(clippy::too_many_lines)] // one arm per action kind
pub(crate) fn expand(
    state: &ShowState,
    action: &CueAction,
    on_air: impl Fn(fm_types::InputId) -> bool,
) -> Vec<EngineCommand> {
    let channel = |number: u8| OverlayChannelId::new(number).expect("validated overlay channel");
    let switcher = state.desired_switcher();
    match action {
        CueAction::Preview { input } => vec![EngineCommand::SelectPreview(*input)],
        CueAction::Take {
            input,
            effect,
            duration_frames,
        } => {
            let mut commands = Vec::new();
            if let Some(input) = input {
                if switcher.program() == *input {
                    return commands;
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
        CueAction::TitleText { input, field, text } => {
            let live = switcher.on_air_usage(*input).is_some() || on_air(*input);
            vec![EngineCommand::SetTitleFields {
                input: *input,
                base_revision: title_revision(state, *input),
                require: if live {
                    crate::TitleAirRequirement::OnAir
                } else {
                    crate::TitleAirRequirement::OffAir
                },
                fields: vec![(FieldId::new(field.get()), FieldValue::Text(text.clone()))],
            }]
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
        CueAction::Wait { .. } | CueAction::Delay { .. } => Vec::new(),
    }
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

/// The earliest cue advance due: `(list, due frame)` of the waiting cue
/// with the smallest due frame, the first list in order on a tie.
#[must_use]
pub fn cue_due(lists: &[CueList]) -> Option<(u64, u64)> {
    lists
        .iter()
        .filter_map(|list| {
            list.state
                .pending
                .map(|pending| (list.id, pending.due_frame))
        })
        .min_by_key(|(_, due)| *due)
}
