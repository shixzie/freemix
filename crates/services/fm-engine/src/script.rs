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
use fm_switcher::TransitionKind;
use fm_titles::FieldId;
use fm_types::InputId;

use crate::engine::{CueFrame, CuePlan};
use crate::{Engine, EngineCommand, EngineEvent, ShowState};
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
/// project bound, and removing an unknown script.
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
fn failure_rejection(
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

/// The sandbox's only window on the show: a copy of the show the run's
/// actions apply to in order.
struct Host<'e> {
    engine: &'e Engine,
    script: u64,
    target_frame: FrameNumber,
    show: ShowState,
    runtime: Vec<EngineCommand>,
    transition: Option<TransitionKind>,
    title_texts: Vec<(InputId, FieldId, String)>,
    takes: usize,
    step: usize,
    /// The show's refusal of the action that stopped the run.
    refused: Option<Rejection>,
    /// Every action the run asked for that names things the show holds,
    /// in order, including one the show then refused.
    requested: Vec<CueAction>,
}

impl Host<'_> {
    fn number(&self, input: InputId) -> u32 {
        self.show
            .inputs()
            .iter()
            .position(|candidate| *candidate == input)
            .and_then(|index| u32::try_from(index + 1).ok())
            .unwrap_or(0)
    }
}

impl ScriptHost for Host<'_> {
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
        let action = match cue_action(&self.show, action) {
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
            crate::cue::check_action_references(&self.show, &action, "the script")
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
            runtime: &mut self.runtime,
            transition: &mut self.transition,
            title_texts: &mut self.title_texts,
            takes: self.takes,
        };
        let applied =
            self.engine
                .apply_cue_actions(&mut show, &fired, self.target_frame, &mut frame);
        self.takes = frame.takes;
        match applied {
            Ok(()) => {
                self.show = show;
                self.step += 1;
                Ok(())
            }
            Err((_, _, rejection)) => refuse(self, rejection),
        }
    }
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
    let (mut host, report) = run(engine, script, target_frame)?;
    if let Some(failure) = &report.failure {
        return Err(failure_rejection(script, failure, host.refused.take()));
    }
    Ok(CuePlan {
        show: Box::new(host.show),
        event: EngineEvent::ScriptRan {
            script,
            frame: target_frame.get(),
            actions: report.actions,
            instructions: report.instructions,
            title_texts: host.title_texts,
            applied: host.runtime.clone(),
        },
        runtime: host.runtime,
        transition: host.transition,
    })
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
    run(engine, script, target_frame).map_or_else(|_| Vec::new(), |(host, _)| host.requested)
}

/// Runs script `script` in the sandbox against a copy of the show.
fn run(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Result<(Host<'_>, fm_script::ScriptReport), Rejection> {
    let found = engine
        .show()
        .script(script)
        .ok_or_else(|| unknown_script(script))?;
    let program = fm_script::parse(&found.source)
        .map_err(|failure| failure_rejection(script, &failure, None))?;
    let mut host = Host {
        engine,
        script,
        target_frame,
        show: engine.show().clone(),
        runtime: Vec::new(),
        transition: None,
        title_texts: Vec::new(),
        takes: 0,
        step: 0,
        refused: None,
        requested: Vec::new(),
    };
    let report = fm_script::run(&program, &mut host, Limits::default());
    Ok((host, report))
}
