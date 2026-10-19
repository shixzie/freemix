//! Project scripts in the engine (RC-011).
//!
//! The show owns the persisted [`fm_model::Script`]s. Two engine commands act
//! on them:
//!
//! - [`crate::EngineCommand::SetScript`] creates, replaces, or removes one
//!   script: a project edit honouring the project padlock, undone by
//!   restoring the previous definition.
//! - [`crate::EngineCommand::RunScript`] runs one script in the `fm-script`
//!   sandbox. The script's only capabilities are reading the Program and
//!   Preview input numbers and asking for show actions; each action is the
//!   same typed action a cue performs, expanded to ordinary engine commands
//!   and applied, in order, to a copy of the show exactly like a cue GO
//!   (every refusal, the transition-in-flight rule, and the one take of
//!   Program per frame apply). The run is all or nothing: when the script
//!   finishes, its actions are realized together by one runtime action on
//!   the command's target frame; when it stops early (a syntax or runtime
//!   error, an exhausted budget, or a refused action) the command is
//!   refused with [`RefusalReason::ScriptFailed`] naming the line and column
//!   and nothing changes. A run is a journaled live operation without undo,
//!   like a GO; the actions it performed are reported with the event.
//!
//! A cue's [`CueAction::RunScript`] step (schema 55) runs a script the same
//! way, inside the cue's frame: its actions apply after the frame's earlier
//! actions, share that frame's one take of Program and transition rule, and
//! are realized with the rest of the frame. A failed run fails the step like
//! any refused cue action, with the same [`RefusalReason::ScriptFailed`]. A
//! script cannot run a cue or another script, so runs never nest deeper.
//! Removing a script a cue runs is refused.

use fm_automation::FiredCueAction;
use fm_command::{FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode, ScriptFailureKind};
use fm_model::{
    CueAction, CueMediaOperation, RecorderSlot, StingerSlotNumber, StreamTargetId, TitleFieldId,
    TransitionEffect,
};
use fm_script::{
    Budget, Effect, FailureKind, InputRef, Limits, MediaOperation, ScriptAction, ScriptFailure,
    ScriptHost, TakeEffect,
};
use fm_types::InputId;

use crate::engine::{CueFrame, CuePlan};
use crate::{Engine, EngineEvent, ShowState};
use fm_scheduler::FrameNumber;

/// One script's name and source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineScriptDefinition {
    pub name: String,
    pub source: String,
}

pub(crate) fn unknown_script(script: u64) -> Rejection {
    Rejection::new(
        RejectionCode::NotFound,
        format!("the show has no script {script}"),
    )
    .with_reason(RefusalReason::UnknownId {
        kind: IdKind::Script,
        id: u128::from(script),
    })
}

/// Applies a [`crate::EngineCommand::SetScript`] to `state`.
///
/// # Errors
///
/// Refuses script id zero, an invalid name or source, a script past the
/// project bound, removing an unknown script, and removing a script a cue
/// runs.
pub(crate) fn apply_set(
    state: &mut ShowState,
    script: u64,
    definition: Option<&EngineScriptDefinition>,
) -> Result<(), Rejection> {
    let existing = state
        .scripts()
        .iter()
        .position(|candidate| candidate.id == script);
    let Some(definition) = definition else {
        let index = existing.ok_or_else(|| unknown_script(script))?;
        if let Some(list) = state.cue_lists().iter().find(|list| {
            list.cues
                .iter()
                .flat_map(|cue| &cue.actions)
                .any(|action| action.script() == Some(script))
        }) {
            return Err(Rejection::new(
                RejectionCode::Conflict,
                format!("script {script} is run by cue list {}", list.id),
            )
            .with_field(FieldIssue::new(
                "script",
                "script_in_use",
                "remove the script from every cue list first",
            )));
        }
        state.scripts_mut().remove(index);
        return Ok(());
    };
    if script == 0 {
        return Err(
            Rejection::new(RejectionCode::InvalidCommand, "script id must not be zero").with_field(
                FieldIssue::new("script", "zero", "script id must not be zero"),
            ),
        );
    }
    fm_model::validate_script_definition(&definition.name, &definition.source).map_err(
        |error| {
            let message = error.to_string();
            match error {
                fm_model::ScriptError::InvalidName => Rejection::new(
                    RejectionCode::InvalidCommand,
                    message,
                )
                .with_reason(RefusalReason::InvalidName {
                    max_bytes: u32::try_from(fm_model::MAX_SCRIPT_NAME_BYTES).unwrap_or(u32::MAX),
                }),
                _ => Rejection::new(RejectionCode::InvalidCommand, message).with_reason(
                    RefusalReason::bounds("source", fm_model::MAX_SCRIPT_SOURCE_BYTES as u64),
                ),
            }
        },
    )?;
    let next = fm_model::Script {
        id: script,
        name: definition.name.clone(),
        source: definition.source.clone(),
    };
    if let Some(index) = existing {
        state.scripts_mut()[index] = next;
        return Ok(());
    }
    if state.scripts().len() >= fm_model::MAX_SCRIPTS {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            format!(
                "the show already holds the maximum of {} scripts",
                fm_model::MAX_SCRIPTS
            ),
        )
        .with_reason(RefusalReason::bounds(
            "scripts",
            fm_model::MAX_SCRIPTS as u64,
        )));
    }
    state.scripts_mut().push(next);
    Ok(())
}

/// The definition of script `script` in `state`, for an undo.
pub(crate) fn definition(state: &ShowState, script: u64) -> Option<EngineScriptDefinition> {
    state.script(script).map(|found| EngineScriptDefinition {
        name: found.name.clone(),
        source: found.source.clone(),
    })
}

const fn failure_kind(kind: FailureKind) -> ScriptFailureKind {
    match kind {
        FailureKind::Syntax => ScriptFailureKind::Syntax,
        FailureKind::Runtime => ScriptFailureKind::Runtime,
        FailureKind::Budget(Budget::Instructions) => ScriptFailureKind::InstructionBudget,
        FailureKind::Budget(Budget::Actions) => ScriptFailureKind::ActionBudget,
        FailureKind::Budget(Budget::Variables) => ScriptFailureKind::VariableBudget,
        FailureKind::Budget(Budget::StringBytes) => ScriptFailureKind::StringBudget,
        FailureKind::Refused => ScriptFailureKind::Refused,
    }
}

/// The refusal of a run that stopped at `failure`. A refused action keeps
/// the code and field issues of the refusal the show gave it.
pub(crate) fn failure_rejection(
    script: u64,
    failure: &ScriptFailure,
    refused: Option<Rejection>,
) -> Rejection {
    let failure_kind = failure_kind(failure.kind);
    let code = match (&refused, failure_kind) {
        (Some(refused), _) => refused.code,
        (None, kind) if kind.is_budget() => RejectionCode::ResourceExhausted,
        (None, _) => RejectionCode::InvalidCommand,
    };
    let fields = refused.map(|refused| refused.fields).unwrap_or_default();
    Rejection {
        code,
        message: format!("script {script} stopped at {failure}"),
        fields,
        retryable: false,
        reason: Some(RefusalReason::ScriptFailed {
            failure: failure_kind,
            line: failure.at.line.max(1),
            column: failure.at.column.max(1),
        }),
    }
}

const fn transition_effect(effect: Effect) -> TransitionEffect {
    match effect {
        Effect::Fade => TransitionEffect::Fade,
        Effect::AlphaFade => TransitionEffect::AlphaFade,
        Effect::Wipe => TransitionEffect::Wipe,
        Effect::VerticalWipe => TransitionEffect::VerticalWipe,
        Effect::Slide => TransitionEffect::Slide,
        Effect::VerticalSlide => TransitionEffect::VerticalSlide,
        Effect::Zoom => TransitionEffect::Zoom,
        Effect::Fly => TransitionEffect::Fly,
        Effect::FlyRotate => TransitionEffect::FlyRotate,
        Effect::Cube => TransitionEffect::Cube,
        Effect::CubeZoom => TransitionEffect::CubeZoom,
        Effect::CrossZoom => TransitionEffect::CrossZoom,
        Effect::Merge => TransitionEffect::Merge,
    }
}

/// The input `reference` names in `state`: its 1-based position or its
/// exact name.
fn resolve(state: &ShowState, reference: &InputRef) -> Result<InputId, String> {
    match reference {
        InputRef::Number(number) => usize::try_from(*number)
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| state.inputs().get(index).copied())
            .ok_or_else(|| format!("the show has no input number {number}")),
        InputRef::Name(name) => state
            .inputs()
            .iter()
            .copied()
            .find(|input| state.input_name(*input) == Some(name.as_str()))
            .ok_or_else(|| format!("the show has no input named \"{name}\"")),
    }
}

fn nonzero(value: u64, what: &str) -> Result<core::num::NonZeroU128, String> {
    core::num::NonZeroU128::new(u128::from(value)).ok_or_else(|| format!("{what} 0 does not exist"))
}

/// The cue action a script action performs on `state`.
fn cue_action(state: &ShowState, action: &ScriptAction) -> Result<CueAction, String> {
    let input = |reference: &InputRef| resolve(state, reference);
    Ok(match action {
        ScriptAction::Preview(reference) => CueAction::Preview {
            input: input(reference)?,
        },
        ScriptAction::Take {
            input: reference,
            effect,
            frames,
        } => CueAction::Take {
            input: reference.as_ref().map(input).transpose()?,
            effect: match effect {
                TakeEffect::Cut => TransitionEffect::Cut,
                TakeEffect::Transition(effect) => transition_effect(*effect),
                TakeEffect::Stinger(slot) => TransitionEffect::Stinger(
                    StingerSlotNumber::new(*slot)
                        .ok_or_else(|| format!("Stinger slot {slot} does not exist"))?,
                ),
            },
            duration_frames: *frames,
        },
        ScriptAction::OverlayOn {
            channel,
            input: reference,
        } => CueAction::OverlayOn {
            channel: *channel,
            input: input(reference)?,
        },
        ScriptAction::OverlayOff { channel } => CueAction::OverlayOff { channel: *channel },
        ScriptAction::TitleOn {
            input: reference,
            channel,
        } => CueAction::TitleOn {
            input: input(reference)?,
            channel: *channel,
        },
        ScriptAction::TitleText {
            input: reference,
            field,
            text,
        } => CueAction::TitleText {
            input: input(reference)?,
            field: TitleFieldId::new(nonzero(*field, "title field")?),
            text: text.clone(),
        },
        ScriptAction::TitleOff(reference) => CueAction::TitleOff {
            input: input(reference)?,
        },
        ScriptAction::FadeToBlack { active, frames } => CueAction::FadeToBlack {
            active: *active,
            duration_frames: *frames,
        },
        ScriptAction::PlaylistStart => CueAction::PlaylistStart,
        ScriptAction::PlaylistStop => CueAction::PlaylistStop,
        ScriptAction::Gain {
            input: reference,
            millidb,
        } => CueAction::AudioGain {
            input: input(reference)?,
            gain_millidb: *millidb,
        },
        ScriptAction::Mute {
            input: reference,
            muted,
        } => CueAction::AudioMute {
            input: input(reference)?,
            muted: *muted,
        },
        ScriptAction::Record { recorder, start } => CueAction::Record {
            recorder: RecorderSlot::new(*recorder)
                .map_err(|_| format!("recorder {recorder} does not exist"))?,
            start: *start,
        },
        ScriptAction::Stream { target, start } => CueAction::Stream {
            target: StreamTargetId::new(nonzero(*target, "stream target")?),
            start: *start,
        },
        ScriptAction::Media {
            input: reference,
            operation,
        } => CueAction::Media {
            input: input(reference)?,
            operation: match operation {
                MediaOperation::Play => CueMediaOperation::Play,
                MediaOperation::Pause => CueMediaOperation::Pause,
                MediaOperation::Seek(frame) => CueMediaOperation::Seek { frame: *frame },
                MediaOperation::Restart => CueMediaOperation::Restart,
            },
        },
    })
}

/// The sandbox's only window on the show: the show of the frame the run
/// lands on, which its actions apply to in order.
struct Host<'e, 'f> {
    engine: &'e Engine,
    script: u64,
    target_frame: FrameNumber,
    show: &'f mut ShowState,
    frame: CueFrame<'f>,
    step: usize,
    /// The show's refusal of the action that stopped the run.
    refused: Option<Rejection>,
    /// Every action the run asked for that names things the show holds,
    /// in order, including one the show then refused.
    requested: Vec<CueAction>,
}

impl Host<'_, '_> {
    fn number(&self, input: InputId) -> u32 {
        self.show
            .inputs()
            .iter()
            .position(|candidate| *candidate == input)
            .and_then(|index| u32::try_from(index + 1).ok())
            .unwrap_or(0)
    }
}

impl ScriptHost for Host<'_, '_> {
    fn program(&self) -> u32 {
        self.number(self.show.desired_switcher().program())
    }

    fn preview(&self) -> u32 {
        self.number(self.show.desired_switcher().preview())
    }

    fn act(&mut self, action: &ScriptAction) -> Result<(), String> {
        let refuse = |host: &mut Self, rejection: Rejection| {
            let message = rejection.message.clone();
            host.refused = Some(rejection);
            Err(message)
        };
        let action = match cue_action(self.show, action) {
            Ok(action) => action,
            Err(message) => {
                let rejection = Rejection::new(RejectionCode::NotFound, message);
                return refuse(self, rejection);
            }
        };
        self.requested.push(action.clone());
        if !fm_model::cue_action_is_valid(&action) {
            let rejection = Rejection::new(
                RejectionCode::InvalidCommand,
                "an argument is out of range (a duration of 1 to 3600 frames, an overlay \
                 channel of 1 to 8, a gain in range, or title text of at most 128 bytes)",
            );
            return refuse(self, rejection);
        }
        if let Err(rejection) =
            crate::cue::check_action_references(self.show, &action, "the script")
        {
            return refuse(self, rejection);
        }
        let fired = [FiredCueAction {
            cue: self.script,
            step: self.step,
            action,
        }];
        let mut show = self.show.clone();
        let mut frame = CueFrame {
            runtime: &mut *self.frame.runtime,
            transition: &mut *self.frame.transition,
            title_texts: &mut *self.frame.title_texts,
            takes: self.frame.takes,
        };
        let applied =
            self.engine
                .apply_cue_actions(&mut show, &fired, self.target_frame, &mut frame);
        self.frame.takes = frame.takes;
        match applied {
            Ok(()) => {
                *self.show = show;
                self.step += 1;
                Ok(())
            }
            Err((_, _, rejection)) => refuse(self, rejection),
        }
    }
}

/// What one run of a script on a frame did.
pub(crate) struct ScriptOutcome {
    pub(crate) report: fm_script::ScriptReport,
    /// The show's refusal of the action that stopped the run, if one did.
    pub(crate) refused: Option<Rejection>,
    /// Every action the run asked for (see [`requested_actions`]).
    pub(crate) requested: Vec<CueAction>,
}

impl ScriptOutcome {
    /// The refusal of a run that stopped early.
    pub(crate) fn failure(self, script: u64) -> Option<Rejection> {
        self.report
            .failure
            .as_ref()
            .map(|failure| failure_rejection(script, failure, self.refused))
    }
}

/// Runs script `script` in the sandbox on `show`, the show of the frame
/// the run lands on as earlier actions of that frame left it, collecting
/// what its actions realize into `frame`. A run that stops early leaves
/// `show` and `frame` holding its earlier actions; the caller discards
/// them.
///
/// # Errors
///
/// Refuses an unknown script and a syntax error before anything runs.
pub(crate) fn run_on(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
    show: &mut ShowState,
    frame: &mut CueFrame<'_>,
) -> Result<ScriptOutcome, Rejection> {
    let found = show.script(script).ok_or_else(|| unknown_script(script))?;
    let program = fm_script::parse(&found.source)
        .map_err(|failure| failure_rejection(script, &failure, None))?;
    let mut host = Host {
        engine,
        script,
        target_frame,
        show,
        frame: CueFrame {
            runtime: &mut *frame.runtime,
            transition: &mut *frame.transition,
            title_texts: &mut *frame.title_texts,
            takes: frame.takes,
        },
        step: 0,
        refused: None,
        requested: Vec::new(),
    };
    let report = fm_script::run(&program, &mut host, Limits::default());
    frame.takes = host.frame.takes;
    Ok(ScriptOutcome {
        report,
        refused: host.refused,
        requested: host.requested,
    })
}

/// Runs script `script` on a copy of the show for `target_frame`.
fn run_alone(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Result<(ScriptOutcome, CuePlan), Rejection> {
    let mut show = engine.show().clone();
    let mut runtime = Vec::new();
    let mut transition = None;
    let mut title_texts = Vec::new();
    let outcome = run_on(
        engine,
        script,
        target_frame,
        &mut show,
        &mut CueFrame {
            runtime: &mut runtime,
            transition: &mut transition,
            title_texts: &mut title_texts,
            takes: 0,
        },
    )?;
    let event = EngineEvent::ScriptRan {
        script,
        frame: target_frame.get(),
        actions: outcome.report.actions,
        instructions: outcome.report.instructions,
        title_texts,
        applied: runtime.clone(),
    };
    Ok((
        outcome,
        CuePlan {
            show: Box::new(show),
            event,
            runtime,
            transition,
        },
    ))
}

/// Plans a [`crate::EngineCommand::RunScript`] for `target_frame`: the show
/// once every action ran, the event, and the runtime actions realizing it.
///
/// # Errors
///
/// Refuses an unknown script and a run that stops early.
pub(crate) fn plan(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Result<CuePlan, Rejection> {
    let (outcome, plan) = run_alone(engine, script, target_frame)?;
    match outcome.failure(script) {
        Some(rejection) => Err(rejection),
        None => Ok(plan),
    }
}

/// Every show action a [`crate::EngineCommand::RunScript`] planned for
/// `target_frame` asks for, in order: the run happens on a copy of the show
/// exactly as [`plan`] runs it, so the list follows the branches the script
/// takes on the current show. A run that stops early lists the actions it
/// asked for up to and including the refused one; an unknown script or a
/// syntax error lists none.
pub(crate) fn requested_actions(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Vec<CueAction> {
    run_alone(engine, script, target_frame)
        .map_or_else(|_| Vec::new(), |(outcome, _)| outcome.requested)
}

/// Every action verb script `script` holds, in any branch (see
/// [`fm_script::Program::verbs`]): what any run of it can ask for,
/// whatever the show it runs on. Empty for a source that does not parse
/// (its runs fail before acting); `None` for an unknown script.
pub(crate) fn verbs(state: &ShowState, script: u64) -> Option<Vec<fm_script::Verb>> {
    state.script(script).map(|found| {
        fm_script::parse(&found.source)
            .map(|program| program.verbs())
            .unwrap_or_default()
    })
}
