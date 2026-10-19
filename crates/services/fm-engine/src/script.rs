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
//!
//! # Waits (schema 56, Protocol 2.62)
//!
//! A script's `wait FRAMES` spans frames. The parts after a wait are
//! evaluated on their due frame, not planned up front: a script reads
//! Program and Preview, and a part planned at the start would act on a show
//! that no longer exists by then. A run that reaches a wait therefore ends
//! its part there: the part's actions apply on its frame as above (all or
//! nothing), and the show's one run slot ([`fm_model::ScriptRunState`])
//! saves the run with its interpreter state ([`fm_script::Suspended`]: the
//! statement path to the wait, loop counters, variables, and budget use,
//! all bounded by the run's limits) and the frame it resumes on.
//!
//! - The cue clock resumes it: [`crate::Engine::cue_advance_frame`] reports
//!   the run's due frame as list `0` when it is the earliest automation
//!   due, and a [`crate::EngineCommand::CueAdvance`] for list `0` runs the
//!   next part on exactly that frame (or the next frame once it has
//!   passed), journaled like a cue advance. A part that fails is accepted
//!   with nothing of its frame applied: the run ends and the slot keeps a
//!   typed [`fm_model::ScriptFault`] (the failure kind, line, and column,
//!   and, since schema 57, the refusal's [`FaultDetail`]: a refused
//!   action's typed reason and bounded message, else the failure's own
//!   message), so the clock never retries it.
//! - The run is bounded: at most [`fm_model::MAX_SCRIPT_WAITS`] waits
//!   adding up to [`fm_model::MAX_SCRIPT_WAIT_SECONDS`] of frames, and one
//!   run's instruction, action, variable, and string budgets cover all of
//!   its parts. Only one run waits at a time: another `script_run` is
//!   refused with [`RefusalReason::ScriptRunning`], as is an edit or
//!   removal of the running script (the saved state belongs to its
//!   source). [`crate::EngineCommand::StopScript`] ends the run (or clears
//!   the fault) on its target frame: a live operation without undo. An
//!   accepted edit or removal of the faulted script clears its fault too:
//!   the fault's line and column name the source that ran.
//! - The slot is project state: it is saved with the project and journaled
//!   with every command, so a restarted show resumes the run on its due
//!   engine frame, exactly like a waiting cue (the time the show was down
//!   does not count).
//! - A cue's script step runs on its cue's frame, so the script it runs
//!   must not wait: a cue list naming such a script, or an edit adding a
//!   wait to a script a cue runs, is refused (field issue
//!   `script_waits`), and a step that reached a wait anyway would fail
//!   with the `waits` budget.

use fm_automation::FiredCueAction;
use fm_command::{
    FaultDetail, FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode, ScriptFailureKind,
};
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

/// The refusal of a run, edit, or removal while script `script` runs.
pub(crate) fn running(script: u64) -> Rejection {
    Rejection::new(
        RejectionCode::Conflict,
        format!("script {script} is running; stop it first"),
    )
    .with_reason(RefusalReason::ScriptRunning { script })
}

/// The refusal of a cue step running script `script`, which waits; `owner`
/// names the cue or list.
pub(crate) fn waits_in_cue(script: u64, owner: &str) -> Rejection {
    Rejection::new(
        RejectionCode::InvalidCommand,
        format!("{owner} runs script {script}, which waits; a cue's script step cannot wait"),
    )
    .with_field(FieldIssue::new(
        "script",
        "script_waits",
        "use cue waits instead of wait in a script a cue runs",
    ))
}

/// Applies a [`crate::EngineCommand::SetScript`] to `state`.
///
/// # Errors
///
/// Refuses script id zero, an invalid name or source, a script past the
/// project bound, removing an unknown script, removing a script a cue
/// runs, adding a wait to a script a cue runs, and changing or removing
/// the running script. Replacing or removing a faulted script clears its
/// fault.
pub(crate) fn apply_set(
    state: &mut ShowState,
    script: u64,
    definition: Option<&EngineScriptDefinition>,
) -> Result<(), Rejection> {
    let existing = state
        .scripts()
        .iter()
        .position(|candidate| candidate.id == script);
    if state
        .script_run()
        .running
        .as_ref()
        .is_some_and(|run| run.script == script)
    {
        return Err(running(script));
    }
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
        if state.script_run().script() == Some(script) {
            state.script_run_mut().fault = None;
        }
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
    if fm_model::script_waits(&definition.source)
        && let Some(list) = state.cue_lists().iter().find(|list| {
            list.cues
                .iter()
                .flat_map(|cue| &cue.actions)
                .any(|action| action.script() == Some(script))
        })
    {
        return Err(waits_in_cue(script, &format!("cue list {}", list.id)));
    }
    let next = fm_model::Script {
        id: script,
        name: definition.name.clone(),
        source: definition.source.clone(),
    };
    if let Some(index) = existing {
        state.scripts_mut()[index] = next;
        // The fault's line and column belong to the source that ran: an
        // edit clears it, as a removal does (Protocol 2.62).
        if state.script_run().script() == Some(script) {
            state.script_run_mut().fault = None;
        }
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

/// The typed refusal kind of a run failure (the wire form of a
/// [`fm_model::ScriptFault`]).
#[must_use]
pub const fn script_failure_kind(kind: FailureKind) -> ScriptFailureKind {
    failure_kind(kind)
}

const fn failure_kind(kind: FailureKind) -> ScriptFailureKind {
    match kind {
        FailureKind::Syntax => ScriptFailureKind::Syntax,
        FailureKind::Runtime => ScriptFailureKind::Runtime,
        FailureKind::Budget(Budget::Instructions) => ScriptFailureKind::InstructionBudget,
        FailureKind::Budget(Budget::Actions) => ScriptFailureKind::ActionBudget,
        FailureKind::Budget(Budget::Variables) => ScriptFailureKind::VariableBudget,
        FailureKind::Budget(Budget::StringBytes) => ScriptFailureKind::StringBudget,
        FailureKind::Budget(Budget::Waits) => ScriptFailureKind::WaitBudget,
        FailureKind::Budget(Budget::WaitFrames) => ScriptFailureKind::WaitFramesBudget,
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
            scripts: None,
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

/// Where a run of a script begins.
#[derive(Clone, Copy)]
pub(crate) enum Entry<'a> {
    /// A new run of a cue step, which may not wait.
    Step,
    /// A new run on its own, which may wait.
    Start,
    /// The next part of a run that waited.
    Resume(&'a fm_script::Suspended),
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
    entry: Entry<'_>,
    target_frame: FrameNumber,
    show: &mut ShowState,
    frame: &mut CueFrame<'_>,
) -> Result<ScriptOutcome, Rejection> {
    let found = show.script(script).ok_or_else(|| unknown_script(script))?;
    let program = fm_script::parse(&found.source)
        .map_err(|failure| failure_rejection(script, &failure, None))?;
    let limits = match entry {
        Entry::Step => Limits {
            waits: 0,
            ..Limits::default()
        },
        Entry::Start | Entry::Resume(_) => fm_model::script_limits(engine.frame_rate()),
    };
    let mut host = Host {
        engine,
        script,
        target_frame,
        show,
        frame: CueFrame {
            runtime: &mut *frame.runtime,
            transition: &mut *frame.transition,
            title_texts: &mut *frame.title_texts,
            scripts: None,
            takes: frame.takes,
        },
        step: 0,
        refused: None,
        requested: Vec::new(),
    };
    let report = match entry {
        Entry::Step | Entry::Start => fm_script::run(&program, &mut host, limits),
        Entry::Resume(state) => fm_script::resume(&program, state, &mut host, limits),
    };
    frame.takes = host.frame.takes;
    Ok(ScriptOutcome {
        report,
        refused: host.refused,
        requested: host.requested,
    })
}

/// Runs script `script` (or the next part of its waiting run) on a copy of
/// the show for `target_frame`.
fn run_alone(
    engine: &Engine,
    script: u64,
    entry: Entry<'_>,
    target_frame: FrameNumber,
) -> Result<(ScriptOutcome, CuePlan), Rejection> {
    let mut show = engine.show().clone();
    let mut runtime = Vec::new();
    let mut transition = None;
    let mut title_texts = Vec::new();
    let outcome = run_on(
        engine,
        script,
        entry,
        target_frame,
        &mut show,
        &mut CueFrame {
            runtime: &mut runtime,
            transition: &mut transition,
            title_texts: &mut title_texts,
            scripts: None,
            takes: 0,
        },
    )?;
    // The part's actions applied; the slot now holds the run if it waits
    // again, and nothing once it ends.
    let started_frame = match (entry, &engine.show().script_run().running) {
        (Entry::Resume(_), Some(run)) => run.started_frame,
        _ => target_frame.get(),
    };
    *show.script_run_mut() = fm_model::ScriptRunState {
        running: outcome
            .report
            .suspension
            .as_ref()
            .map(|suspension| fm_model::ScriptRun {
                script,
                started_frame,
                due_frame: target_frame
                    .get()
                    .saturating_add(u64::from(suspension.frames)),
                state: suspension.state.clone(),
            }),
        fault: None,
    };
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
/// once every action of the run's first part ran (holding the run if it
/// waits), the event, and the runtime actions realizing it.
///
/// # Errors
///
/// Refuses a run while a script runs, an unknown script, and a run whose
/// first part stops early.
pub(crate) fn plan(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Result<CuePlan, Rejection> {
    if let Some(run) = &engine.show().script_run().running {
        return Err(running(run.script));
    }
    let (outcome, plan) = run_alone(engine, script, Entry::Start, target_frame)?;
    match outcome.failure(script) {
        Some(rejection) => Err(rejection),
        None => Ok(plan),
    }
}

/// Plans the cue clock's advance of the waiting run (list `0`, see the
/// module documentation) for `target_frame`: the next part runs on the
/// show as it is then. A part that stops early is planned as a fault:
/// nothing of the frame applies and the run ends.
///
/// # Errors
///
/// Refuses an advance for any frame but the run's (see
/// [`crate::Engine::cue_advance_frame`]).
pub(crate) fn plan_resume(
    engine: &Engine,
    frame: u64,
    target_frame: FrameNumber,
) -> Result<CuePlan, Rejection> {
    let due = engine.cue_advance_frame();
    let run = engine.show().script_run().running.as_ref();
    let (Some(run), Some((0, at))) = (run, due) else {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            format!("no script run waits for frame {frame}"),
        ));
    };
    if at != frame {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            format!("the script run resumes at frame {at}, not frame {frame}"),
        ));
    }
    let script = run.script;
    let state = run.state.clone();
    let outcome = run_alone(engine, script, Entry::Resume(&state), target_frame);
    let (failure, refused) = match outcome {
        Ok((outcome, plan)) => match outcome.report.failure {
            None => return Ok(plan),
            Some(failure) => (failure, outcome.refused),
        },
        // The run's script is never removed or edited while it runs, so it
        // parses; a show restored without it fails here like a bad part.
        Err(_) => (
            fm_script::ScriptFailure {
                kind: FailureKind::Runtime,
                at: fm_script::Span::new(1, 1),
                message: format!("script {script} cannot resume"),
            },
            None,
        ),
    };
    // What a refused command would have said (Protocol 2.63): the refused
    // action's own reason and message, else the failure's message.
    let detail = refused
        .as_ref()
        .map_or_else(|| FaultDetail::new(None, &failure.message), FaultDetail::of);
    let mut show = engine.show().clone();
    *show.script_run_mut() = fm_model::ScriptRunState {
        running: None,
        fault: Some(fm_model::ScriptFault {
            script,
            frame: target_frame.get(),
            failure: failure.kind,
            at: fm_script::Span::new(failure.at.line.max(1), failure.at.column.max(1)),
            detail,
        }),
    };
    Ok(CuePlan {
        show: Box::new(show),
        event: EngineEvent::ScriptRunChanged {
            script,
            frame: target_frame.get(),
        },
        runtime: Vec::new(),
        transition: None,
    })
}

/// Plans a [`crate::EngineCommand::StopScript`] for `target_frame`: the
/// waiting run ends (nothing more of it runs) or the last fault clears.
///
/// # Errors
///
/// Refuses a stop with no run waiting and no fault to clear.
pub(crate) fn plan_stop(engine: &Engine, target_frame: FrameNumber) -> Result<CuePlan, Rejection> {
    let Some(script) = engine.show().script_run().script() else {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            "no script is running",
        ));
    };
    let mut show = engine.show().clone();
    *show.script_run_mut() = fm_model::ScriptRunState::default();
    Ok(CuePlan {
        show: Box::new(show),
        event: EngineEvent::ScriptRunChanged {
            script,
            frame: target_frame.get(),
        },
        runtime: Vec::new(),
        transition: None,
    })
}

/// Every show action a [`crate::EngineCommand::RunScript`] planned for
/// `target_frame` asks for, in order: the run happens on a copy of the show
/// exactly as [`plan`] runs it, so the list follows the branches the script
/// takes on the current show (up to its first wait). A run that stops
/// early lists the actions it asked for up to and including the refused
/// one; an unknown script or a syntax error lists none.
pub(crate) fn requested_actions(
    engine: &Engine,
    script: u64,
    target_frame: FrameNumber,
) -> Vec<CueAction> {
    run_alone(engine, script, Entry::Start, target_frame)
        .map_or_else(|_| Vec::new(), |(outcome, _)| outcome.requested)
}

/// Whether script `script`'s source holds a `wait` (see
/// [`fm_script::Program::waits`]); `None` for an unknown script.
pub(crate) fn waits(state: &ShowState, script: u64) -> Option<bool> {
    state
        .script(script)
        .map(|found| fm_model::script_waits(&found.source))
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
