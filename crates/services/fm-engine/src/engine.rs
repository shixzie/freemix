use std::num::NonZeroUsize;
use std::sync::Arc;

use fm_clock::{Clock, ClockDomainId, ClockTime, ManualClock};
use fm_command::{
    ApplyOutcome, CommandEnvelope, CommandReceipt, CommandState, EventSequence, FieldIssue,
    IdempotencyKey, Mutation, Rejection, RejectionCode, Revision, RuntimeGeneration, StateEpoch,
};
use fm_scheduler::{FrameNumber, FrameScheduler, PlanGeneration};
use fm_switcher::{
    FadeToBlackFrame, FadeToBlackPosition, InputUsage, OVERLAY_CHANNEL_COUNT, OverlayBorderPreset,
    OverlayChannelId, OverlayChannelState, OverlayPositionPreset, OverlayTransitionKind,
    ProgramFrame, RecorderSlot, StingerDescriptor, StingerSlotId, StreamTargetId, SwitcherCommand,
    SwitcherError, SwitcherEvent, SwitcherState, TBarPosition, TransitionKind,
};
use fm_titles::{FieldId, FieldValue};
use fm_types::{FrameRate, InputId, InputOrderError, OutputId, RenameInputError};

use crate::history::{EditHistory, HistoryStep, ResolvedCommand, check_edit_locks};
use crate::{
    EditHistoryEntry, EditLockScope, EngineError, EngineInputAudioStripState, EngineInputSource,
    InputCatalogueError, ShowState, SnapshotError, TitleRejection,
};

type RuntimeScheduler = FrameScheduler<(), (), EngineCommand, ()>;
const MAX_TRANSITION_DURATION_FRAMES: u32 = 3_600;
/// Maximum idempotency receipts an engine retains in memory.
///
/// Retention keeps the newest receipts by the revision they were recorded at;
/// a rejection recorded at revision `r` ranks just after the accepted command
/// that produced `r`, and ties break by key. The receipt a command just
/// produced is never evicted. Resubmitting an evicted key executes it as a new
/// command. Persistence stores at most the same bound, so snapshots and
/// restores round-trip the retained window.
pub const MAX_RETAINED_RECEIPTS: usize = 1_024;
const RECEIPT_LIMIT: NonZeroUsize = NonZeroUsize::new(MAX_RETAINED_RECEIPTS).unwrap();
pub const MAX_INPUT_AUDIO_DELAY_SAMPLES: u32 = 48_000;
pub const MIN_INPUT_AUDIO_GAIN_MILLIDB: i32 = -96_000;
pub const MAX_INPUT_AUDIO_GAIN_MILLIDB: i32 = 24_000;
pub const MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS: i32 = -10_000;
pub const MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS: i32 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineManualTransitionKind {
    Fade,
    Wipe,
    AlphaFade,
    Slide,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EngineManualTransitionPosition(TBarPosition);

impl EngineManualTransitionPosition {
    pub const MAX: u16 = TBarPosition::MAX;
    pub const START: Self = Self(TBarPosition::START);
    pub const END: Self = Self(TBarPosition::END);

    #[must_use]
    pub const fn new(basis_points: u16) -> Option<Self> {
        match TBarPosition::new(basis_points) {
            Some(position) => Some(Self(position)),
            None => None,
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0.basis_points()
    }

    const fn domain(self) -> TBarPosition {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineManualTransitionState {
    pub kind: EngineManualTransitionKind,
    pub from: InputId,
    pub to: InputId,
    pub interval_start: EngineManualTransitionPosition,
    pub position: EngineManualTransitionPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineFadeToBlackState {
    pub active: bool,
    pub position: FadeToBlackPosition,
}

/// Whether a title field edit requires its title to be off air or on air.
///
/// A title is on air while it is on Program, an endpoint of an automatic or
/// manual transition, the media of a Stinger transition in flight, or the
/// source of an overlay channel that is shown or still fading, in either the
/// desired or the realized switcher. An overlay
/// fading out therefore keeps its title on air until the fade completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleAirRequirement {
    /// Prepare (save) a title that is not on air.
    OffAir,
    /// Update a title live while it is on air.
    OnAir,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineCommand {
    ReorderInputs {
        inputs: Vec<InputId>,
    },
    RenameInput {
        input: InputId,
        name: String,
    },
    /// Appends a new input with a default Master strip and no padlock.
    ///
    /// The source is carried for durability and redo; the engine itself
    /// renders nothing.
    AddInput {
        input: InputId,
        name: String,
        source: EngineInputSource,
    },
    /// Removes an input that nothing on air or configured references.
    RemoveInput {
        input: InputId,
    },
    SetInputAudioStrip {
        input: InputId,
        state: EngineInputAudioStripState,
    },
    SelectPreview(InputId),
    Cut,
    Fade {
        duration_frames: u32,
    },
    AlphaFade {
        duration_frames: u32,
    },
    Slide {
        duration_frames: u32,
    },
    Zoom {
        duration_frames: u32,
    },
    Stinger {
        slot: StingerSlotId,
        duration_frames: u32,
    },
    ConfigureStinger {
        slot: StingerSlotId,
        descriptor: StingerDescriptor,
    },
    RemoveStinger {
        slot: StingerSlotId,
    },
    TakeOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    UpdateOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayOff {
        channel: OverlayChannelId,
    },
    ConfigureOverlayTransition {
        channel: OverlayChannelId,
        transition: OverlayTransitionKind,
        duration_frames: u32,
    },
    ConfigureOverlayAppearance {
        channel: OverlayChannelId,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    },
    QueueOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    TakeNextOverlay {
        channel: OverlayChannelId,
    },
    SetOverlayOutputInclusion {
        channel: OverlayChannelId,
        output: OutputId,
        included: bool,
    },
    Wipe {
        duration_frames: u32,
    },
    FadeToBlack {
        active: bool,
        duration_frames: u32,
    },
    StartManualTransition {
        kind: EngineManualTransitionKind,
    },
    SetManualTransitionPosition {
        position: EngineManualTransitionPosition,
    },
    CommitManualTransition,
    CancelManualTransition,
    StreamStart {
        target: StreamTargetId,
    },
    StreamStop {
        target: StreamTargetId,
    },
    RecordStart {
        recorder: RecorderSlot,
    },
    RecordStop {
        recorder: RecorderSlot,
    },
    /// Reverts the most recent project edit in the shared history.
    ///
    /// `expected_edit` names the [`EditHistoryEntry::edit`] the caller means
    /// to revert; a different top entry is rejected as a history conflict.
    Undo {
        expected_edit: Option<Revision>,
    },
    /// Reapplies the most recently undone project edit.
    ///
    /// `expected_edit` names the [`EditHistoryEntry::edit`] the caller means
    /// to reapply; a different top entry is rejected as a history conflict.
    Redo {
        expected_edit: Option<Revision>,
    },
    /// Engages or releases an edit padlock. Live operations are never locked.
    SetEditLock {
        scope: EditLockScope,
        locked: bool,
    },
    /// Atomically sets title field values when `base_revision` is the title's
    /// current field revision and the title's air state meets `require`.
    ///
    /// An empty field list is accepted without advancing the revision.
    SetTitleFields {
        input: InputId,
        base_revision: u64,
        require: TitleAirRequirement,
        fields: Vec<(FieldId, FieldValue)>,
    },
    /// Applies field values to an off-air title and takes it onto an overlay
    /// channel. The fields change at acceptance, before the overlay realizes,
    /// so the first frame on air already shows them.
    TakeTitle {
        input: InputId,
        channel: OverlayChannelId,
        base_revision: u64,
        fields: Vec<(FieldId, FieldValue)>,
    },
    /// Takes a title off the single overlay channel that shows it.
    TitleOffAir {
        input: InputId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineAcceptance {
    pub target_frame: FrameNumber,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineEvent {
    DesiredSwitcherChanged(EngineCommand),
    InputRenamed {
        input: InputId,
        name: String,
    },
    InputOrderChanged {
        inputs: Vec<InputId>,
    },
    InputAdded {
        input: InputId,
        name: String,
        source: EngineInputSource,
    },
    InputRemoved {
        input: InputId,
    },
    StreamsChanged {
        streams: Vec<EngineStreamStatus>,
    },
    RecordingChanged {
        recorder: RecorderSlot,
        active: bool,
    },
    EditLockChanged {
        scope: EditLockScope,
        locked: bool,
    },
    /// A [`EngineCommand::SetTitleFields`] was accepted; `revision` is the
    /// title's field revision afterwards (unchanged for an empty edit).
    ///
    /// [`EngineCommand::TakeTitle`] and [`EngineCommand::TitleOffAir`] report
    /// [`EngineEvent::DesiredSwitcherChanged`] instead; the taken title's
    /// field revision is read from [`ShowState::title`].
    TitleFieldsChanged {
        input: InputId,
        revision: u64,
    },
}

/// Engine-level desired status of one stream target; realization is owned by
/// the media plane and always reads as `Stopped` at this layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineStreamStatus {
    pub target: StreamTargetId,
    pub name: String,
    pub desired_running: bool,
}

pub type EngineCommandOutcome = ApplyOutcome<EngineAcceptance, EngineEvent>;

/// The result of preparing an engine command without changing the live engine.
#[derive(Debug)]
pub enum EnginePrepareOutcome {
    /// The idempotency key was already durable, so there is nothing to commit.
    Replayed(EngineCommandOutcome),
    /// A new accepted or rejected receipt is staged and ready to commit.
    Prepared(Box<PreparedEngineExecution>),
}

impl EnginePrepareOutcome {
    #[must_use]
    pub fn prepared(self) -> Option<PreparedEngineExecution> {
        match self {
            Self::Prepared(prepared) => Some(*prepared),
            Self::Replayed(_) => None,
        }
    }

    #[must_use]
    pub fn replayed(self) -> Option<EngineCommandOutcome> {
        match self {
            Self::Prepared(_) => None,
            Self::Replayed(outcome) => Some(outcome),
        }
    }
}

/// An isolated post-command engine ready for projection or atomic installation.
#[derive(Debug)]
pub struct PreparedEngineExecution {
    staged: Engine,
    outcome: EngineCommandOutcome,
    base: EnginePreparationBase,
}

impl PreparedEngineExecution {
    #[must_use]
    pub const fn outcome(&self) -> &EngineCommandOutcome {
        &self.outcome
    }

    /// Returns the staged post-command engine without exposing mutable access.
    #[must_use]
    pub const fn staged_engine(&self) -> &Engine {
        &self.staged
    }

    /// Projects the staged engine by an exact number of deterministic ticks.
    ///
    /// The returned snapshot includes the projected runtime state and receipt.
    ///
    /// # Errors
    ///
    /// Returns a tick error if projection cannot advance, or
    /// [`SnapshotError::WorkInFlight`] if `ticks` does not reach an idle frame
    /// boundary.
    pub fn project(&self, ticks: u64) -> Result<EngineSnapshot, EngineError> {
        let mut projected = self.staged.clone();
        for _ in 0..ticks {
            projected.tick()?;
        }
        projected.snapshot().map_err(EngineError::from)
    }

    /// Consumes this preparation without changing its originating engine.
    pub fn abort(self) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EnginePreparationBase {
    state_epoch: StateEpoch,
    revision: Revision,
    event_sequence: EventSequence,
    receipts_recorded: u64,
    frame_cursor: FrameNumber,
    clock_time: ClockTime,
    runtime_generation: RuntimeGeneration,
}

/// Persisted engine coordinates needed to resume an idle runtime exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineRestoreState {
    pub state_epoch: StateEpoch,
    pub revision: Revision,
    pub event_sequence: EventSequence,
    pub runtime_generation: RuntimeGeneration,
    pub clock_time: ClockTime,
    pub frame_cursor: FrameNumber,
    pub receipts: Vec<(IdempotencyKey, CommandReceipt<EngineAcceptance>)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub frame: FrameNumber,
    pub deadline: ClockTime,
    pub program: ProgramFrame,
    pub fade_to_black: FadeToBlackFrame,
    pub overlays: [OverlayChannelState; OVERLAY_CHANNEL_COUNT],
    pub events: Vec<SwitcherEvent>,
    pub input_audio_strip_updates: Vec<(InputId, EngineInputAudioStripState)>,
    pub revision: Revision,
    pub runtime_generation: RuntimeGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSnapshot {
    show: ShowState,
    realized_switcher: SwitcherState,
    state_epoch: StateEpoch,
    revision: Revision,
    event_sequence: EventSequence,
    runtime_generation: RuntimeGeneration,
    frame_rate: FrameRate,
    clock_domain: ClockDomainId,
    clock_time: ClockTime,
    frames_rendered: u64,
    receipts: Vec<(IdempotencyKey, CommandReceipt<EngineAcceptance>)>,
}

impl EngineSnapshot {
    #[must_use]
    pub const fn show(&self) -> &ShowState {
        &self.show
    }

    #[must_use]
    pub const fn realized_switcher(&self) -> &SwitcherState {
        &self.realized_switcher
    }

    #[must_use]
    pub fn desired_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.show()
            .desired_switcher()
            .t_bar()
            .map(engine_manual_state)
    }

    #[must_use]
    pub const fn desired_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(self.show.desired_switcher())
    }

    #[must_use]
    pub const fn realized_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(&self.realized_switcher)
    }

    #[must_use]
    pub fn realized_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.realized_switcher.t_bar().map(engine_manual_state)
    }

    #[must_use]
    pub const fn state_epoch(&self) -> StateEpoch {
        self.state_epoch
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    #[must_use]
    pub const fn event_sequence(&self) -> EventSequence {
        self.event_sequence
    }

    #[must_use]
    pub const fn runtime_generation(&self) -> RuntimeGeneration {
        self.runtime_generation
    }

    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    #[must_use]
    pub const fn clock_domain(&self) -> ClockDomainId {
        self.clock_domain
    }

    #[must_use]
    pub const fn clock_time(&self) -> ClockTime {
        self.clock_time
    }

    #[must_use]
    pub const fn frames_rendered(&self) -> u64 {
        self.frames_rendered
    }

    #[must_use]
    pub fn receipts(&self) -> &[(IdempotencyKey, CommandReceipt<EngineAcceptance>)] {
        &self.receipts
    }
}

#[derive(Clone, Debug)]
pub struct Engine {
    commands: CommandState<ShowState, EngineAcceptance>,
    /// Monotonic count of receipts recorded, so preparations go stale after
    /// any committed receipt even once retention holds the count constant.
    receipts_recorded: u64,
    realized_switcher: SwitcherState,
    scheduler: RuntimeScheduler,
    clock: ManualClock,
    frame_rate: FrameRate,
    runtime_generation: RuntimeGeneration,
    pending_actions: usize,
    transition_in_flight: Option<TransitionKind>,
    /// Session-scoped undo/redo history; never snapshotted or restored.
    history: EditHistory,
}

impl Engine {
    #[must_use]
    pub fn new(show: ShowState, frame_rate: FrameRate, clock_domain: ClockDomainId) -> Self {
        let realized_switcher = show.desired_switcher().clone();
        Self {
            commands: CommandState::new(show, StateEpoch::new(1)).with_receipt_limit(RECEIPT_LIMIT),
            receipts_recorded: 0,
            realized_switcher,
            scheduler: RuntimeScheduler::new(frame_rate, 0, PlanGeneration::new(0), Arc::new(())),
            clock: ManualClock::new(clock_domain, ClockTime::ZERO),
            frame_rate,
            runtime_generation: RuntimeGeneration::default(),
            pending_actions: 0,
            transition_in_flight: None,
            history: EditHistory::default(),
        }
    }

    #[must_use]
    pub const fn show(&self) -> &ShowState {
        self.commands.state()
    }

    #[must_use]
    pub const fn realized_switcher(&self) -> &SwitcherState {
        &self.realized_switcher
    }

    #[must_use]
    pub fn desired_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.show()
            .desired_switcher()
            .t_bar()
            .map(engine_manual_state)
    }

    #[must_use]
    pub fn realized_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.realized_switcher.t_bar().map(engine_manual_state)
    }

    #[must_use]
    pub const fn desired_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(self.show().desired_switcher())
    }

    #[must_use]
    pub const fn realized_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(&self.realized_switcher)
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.commands.revision()
    }

    #[must_use]
    pub const fn state_epoch(&self) -> StateEpoch {
        self.commands.state_epoch()
    }

    #[must_use]
    pub const fn event_sequence(&self) -> EventSequence {
        self.commands.event_sequence()
    }

    #[must_use]
    pub const fn runtime_generation(&self) -> RuntimeGeneration {
        self.runtime_generation
    }

    #[must_use]
    pub const fn frame_cursor(&self) -> FrameNumber {
        self.scheduler.pacer().next_frame()
    }

    #[must_use]
    pub fn clock_time(&self) -> ClockTime {
        self.clock.now()
    }

    /// Returns the next deterministic frame deadline without advancing state.
    ///
    /// # Errors
    ///
    /// Returns a pacing error when the next deadline exceeds the media clock.
    pub fn next_frame_deadline(&self) -> Result<ClockTime, EngineError> {
        self.scheduler
            .pacer()
            .next_deadline()
            .map(|deadline| ClockTime::from_nanos(deadline.at_ns))
            .map_err(|error| EngineError::Tick(fm_scheduler::TickError::Pacing(error)))
    }

    /// Returns the durable receipt recorded for an idempotency key, if any.
    ///
    /// This is a read-only lookup: it neither validates nor stages a command.
    #[must_use]
    pub fn receipt(&self, key: &IdempotencyKey) -> Option<&CommandReceipt<EngineAcceptance>> {
        self.commands.receipt(key)
    }

    /// Returns undoable project edits, oldest first; the last is undone next.
    ///
    /// At most [`crate::MAX_EDIT_HISTORY`] entries are retained.
    pub fn undo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.history.undo_entries()
    }

    /// Returns undone project edits, oldest first; the last is redone next.
    pub fn redo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.history.redo_entries()
    }

    /// Returns how many idempotency receipts are retained, at most
    /// [`MAX_RETAINED_RECEIPTS`].
    #[must_use]
    pub fn receipt_count(&self) -> usize {
        self.commands.receipt_count()
    }

    /// Prepares a durable command and its runtime action on an isolated clone.
    ///
    /// Duplicate idempotency keys return their durable receipt without creating
    /// a staged execution.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Schedule`] if the staged runtime action sequence
    /// is exhausted.
    pub fn prepare_execute(
        &self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EnginePrepareOutcome, EngineError> {
        if let Some(receipt) = self.commands.receipt(&envelope.idempotency_key) {
            return Ok(EnginePrepareOutcome::Replayed(EngineCommandOutcome {
                receipt: receipt.clone(),
                events: Vec::new(),
                replayed: true,
            }));
        }

        let base = self.preparation_base();
        let mut staged = self.clone();
        let outcome = staged.execute_immediate(envelope, now_millis)?;
        Ok(EnginePrepareOutcome::Prepared(Box::new(
            PreparedEngineExecution {
                staged,
                outcome,
                base,
            },
        )))
    }

    /// Atomically installs a prepared engine if its live base is still current.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::StalePreparation`] if durable authority, receipt
    /// history, or the runtime cursor changed after preparation.
    pub fn commit_execute(
        &mut self,
        prepared: PreparedEngineExecution,
    ) -> Result<EngineCommandOutcome, EngineError> {
        if self.preparation_base() != prepared.base {
            return Err(EngineError::StalePreparation);
        }

        let PreparedEngineExecution {
            staged, outcome, ..
        } = prepared;
        *self = staged;
        Ok(outcome)
    }

    /// Accepts a durable command and schedules its runtime action.
    ///
    /// Duplicate idempotency keys replay the original receipt without adding
    /// another runtime action.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Schedule`] if the runtime action sequence is
    /// exhausted.
    pub fn execute(
        &mut self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EngineCommandOutcome, EngineError> {
        match self.prepare_execute(envelope, now_millis)? {
            EnginePrepareOutcome::Replayed(outcome) => Ok(outcome),
            EnginePrepareOutcome::Prepared(prepared) => self.commit_execute(*prepared),
        }
    }

    fn execute_immediate(
        &mut self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EngineCommandOutcome, EngineError> {
        let key = envelope.idempotency_key.clone();
        let command_id = envelope.id.clone();
        let target_frame = self.scheduler.pacer().next_frame();
        let already_seen = self.commands.receipt(&key).is_some();
        // Undo and redo resolve to the stored project edit before scheduling,
        // so the runtime only ever realizes ordinary edit commands.
        let resolution = self
            .history
            .resolve(self.commands.state(), &envelope.command);
        let scheduled = match (&resolution, already_seen) {
            (Ok(resolved), false) => Some(
                self.scheduler
                    .schedule_action(
                        target_frame,
                        runtime_action(self.commands.state(), &resolved.command),
                    )
                    .map_err(EngineError::Schedule)?,
            ),
            _ => None,
        };
        let runtime_busy = self.transition_in_flight;
        // The realized switcher lags the desired one by up to a frame and
        // keeps fading overlays on air, so a removal must also be free of
        // every realized reference or its scheduled action could not apply.
        let realized_usage = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::RemoveInput { input }) => self
                .realized_switcher
                .input_usage(*input)
                .map(|usage| (*input, usage)),
            _ => None,
        };
        // Title air checks span both switchers for the same reason: a title
        // fading out of an overlay is still on air.
        let realized_on_air = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(
                EngineCommand::SetTitleFields { input, .. }
                | EngineCommand::TakeTitle { input, .. }
                | EngineCommand::TitleOffAir { input },
            ) => self.realized_switcher.on_air_usage(*input),
            _ => None,
        };
        let staged_resolution = resolution.clone();
        let outcome = self.commands.apply(envelope, now_millis, move |_, _| {
            let ResolvedCommand { command, .. } = staged_resolution?;
            if let Some((input, usage)) = realized_usage {
                return Err(input_catalogue_rejection(InputCatalogueError::InUse {
                    input,
                    usage,
                }));
            }
            if let Some(kind) = runtime_busy
                && !matches!(&command, EngineCommand::FadeToBlack { .. })
                && !matches!(&command, EngineCommand::RenameInput { .. })
                && !matches!(&command, EngineCommand::ReorderInputs { .. })
                && !matches!(&command, EngineCommand::AddInput { .. })
                && !matches!(&command, EngineCommand::SetEditLock { .. })
                && !matches!(
                    &command,
                    EngineCommand::StreamStart { .. } | EngineCommand::StreamStop { .. }
                )
                && !matches!(
                    &command,
                    EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. }
                )
                && !is_overlay_command(&command)
                && !is_title_command(&command)
            {
                let name = match kind {
                    TransitionKind::Fade => "fade",
                    TransitionKind::AlphaFade => "alpha fade",
                    TransitionKind::Slide => "slide",
                    TransitionKind::Zoom => "zoom",
                    TransitionKind::Wipe => "wipe",
                    TransitionKind::Stinger(_) => "stinger",
                };
                return Err(Rejection::new(
                    RejectionCode::Conflict,
                    format!("a {name} transition is already in flight"),
                ));
            }
            Ok(EngineMutation {
                command,
                target_frame,
                realized_on_air,
            })
        });

        if !outcome.replayed {
            self.receipts_recorded = self.receipts_recorded.wrapping_add(1);
        }
        let accepted_revision = outcome
            .receipt
            .accepted()
            .filter(|_| !outcome.replayed)
            .map(|accepted| accepted.revision);
        if let (Some(revision), Ok(resolved)) = (accepted_revision, resolution) {
            let ResolvedCommand { command, step } = resolved;
            self.pending_actions += 1;
            if let Some(kind) = runtime_transition_kind(self.commands.state(), &command) {
                self.transition_in_flight = Some(kind);
            }
            if !matches!(step, HistoryStep::None) {
                self.history.commit(step, command, command_id, revision);
            }
        } else if let Some(action) = scheduled {
            self.scheduler.cancel_action(action);
        }
        Ok(outcome)
    }

    fn preparation_base(&self) -> EnginePreparationBase {
        EnginePreparationBase {
            state_epoch: self.commands.state_epoch(),
            revision: self.commands.revision(),
            event_sequence: self.commands.event_sequence(),
            receipts_recorded: self.receipts_recorded,
            frame_cursor: self.scheduler.pacer().next_frame(),
            clock_time: self.clock.now(),
            runtime_generation: self.runtime_generation,
        }
    }

    /// Renders the next frame at its exact simulated deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the clock, scheduler, switcher, or runtime
    /// generation cannot advance.
    pub fn tick(&mut self) -> Result<FrameResult, EngineError> {
        let deadline = self
            .scheduler
            .pacer()
            .next_deadline()
            .map_err(|error| EngineError::Tick(fm_scheduler::TickError::Pacing(error)))?;
        self.clock.set(ClockTime::from_nanos(deadline.at_ns))?;
        let tick = self.scheduler.tick(deadline.at_ns)?;
        self.pending_actions = self.pending_actions.saturating_sub(tick.actions.len());

        let mut events = Vec::new();
        let mut input_audio_strip_updates = Vec::new();
        for scheduled in tick.actions {
            if let EngineCommand::SetInputAudioStrip { input, state } = scheduled.action {
                input_audio_strip_updates.push((input, state));
            }
            let command_events = apply_runtime(&mut self.realized_switcher, scheduled.action)?;
            events.extend(command_events);
            self.runtime_generation = self.runtime_generation.checked_next()?;
        }

        let program = self.realized_switcher.program_frame();
        let fade_to_black = self.realized_switcher.fade_to_black_frame();
        let overlays = self.realized_switcher.overlays().clone();
        for event in self.realized_switcher.advance_frame_events() {
            if matches!(event, SwitcherEvent::ProgramChanged { .. }) {
                self.transition_in_flight = None;
            }
            events.push(event);
        }

        Ok(FrameResult {
            frame: tick.deadline.frame,
            deadline: ClockTime::from_nanos(tick.deadline.at_ns),
            program,
            fade_to_black,
            overlays,
            events,
            input_audio_strip_updates,
            revision: self.commands.revision(),
            runtime_generation: self.runtime_generation,
        })
    }

    /// Captures durable and realized state at an idle frame boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::WorkInFlight`] while an action or transition
    /// is pending.
    pub fn snapshot(&self) -> Result<EngineSnapshot, SnapshotError> {
        if self.pending_actions != 0
            || self.transition_in_flight.is_some()
            || self.realized_switcher.transition().is_some()
            || self
                .commands
                .state()
                .desired_switcher()
                .fade_to_black_is_automatic()
            || self.realized_switcher.fade_to_black_is_automatic()
            || self.realized_switcher.overlays_in_motion()
        {
            return Err(SnapshotError::WorkInFlight);
        }
        let mut receipts: Vec<_> = self
            .commands
            .receipts()
            .map(|(key, receipt)| (key.clone(), receipt.clone()))
            .collect();
        receipts.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        Ok(EngineSnapshot {
            show: self.commands.state().clone(),
            realized_switcher: self.realized_switcher.clone(),
            state_epoch: self.commands.state_epoch(),
            revision: self.commands.revision(),
            event_sequence: self.commands.event_sequence(),
            runtime_generation: self.runtime_generation,
            frame_rate: self.frame_rate,
            clock_domain: self.clock.domain(),
            clock_time: self.clock.now(),
            frames_rendered: self.scheduler.pacer().next_frame().get(),
            receipts,
        })
    }

    /// Restores an idle snapshot, including durable and runtime counters.
    ///
    /// # Errors
    ///
    /// Returns an error if the saved scheduler cursor cannot be reconstructed.
    pub fn restore(snapshot: EngineSnapshot) -> Result<Self, EngineError> {
        let restore_state = EngineRestoreState {
            state_epoch: snapshot.state_epoch,
            revision: snapshot.revision,
            event_sequence: snapshot.event_sequence,
            runtime_generation: snapshot.runtime_generation,
            clock_time: snapshot.clock_time,
            frame_cursor: FrameNumber::new(snapshot.frames_rendered),
            receipts: snapshot.receipts,
        };
        Self::restore_persisted(
            snapshot.show,
            snapshot.realized_switcher,
            snapshot.frame_rate,
            snapshot.clock_domain,
            restore_state,
        )
        .map_err(EngineError::from)
    }

    /// Restores durable and idle runtime state supplied by persistence.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] if the persisted state is not a consistent
    /// idle frame boundary or its frame cursor cannot be reconstructed.
    pub fn restore_persisted(
        show: ShowState,
        realized_switcher: SwitcherState,
        frame_rate: FrameRate,
        clock_domain: ClockDomainId,
        restore_state: EngineRestoreState,
    ) -> Result<Self, SnapshotError> {
        validate_idle_restore(&show, &realized_switcher, &restore_state, frame_rate)?;
        let scheduler = RuntimeScheduler::restore(
            frame_rate,
            0,
            restore_state.frame_cursor,
            PlanGeneration::new(0),
            Arc::new(()),
        )
        .map_err(|_| SnapshotError::InvalidFrameCounter)?;
        Ok(Self {
            commands: CommandState::restore_with_receipts(
                show,
                restore_state.state_epoch,
                restore_state.revision,
                restore_state.event_sequence,
                restore_state.receipts,
            )
            .with_receipt_limit(RECEIPT_LIMIT),
            receipts_recorded: 0,
            realized_switcher,
            scheduler,
            clock: ManualClock::new(clock_domain, restore_state.clock_time),
            frame_rate,
            runtime_generation: restore_state.runtime_generation,
            pending_actions: 0,
            transition_in_flight: None,
            history: EditHistory::default(),
        })
    }
}

fn validate_idle_restore(
    show: &ShowState,
    realized_switcher: &SwitcherState,
    restore_state: &EngineRestoreState,
    frame_rate: FrameRate,
) -> Result<(), SnapshotError> {
    if show.desired_switcher().transition().is_some() || realized_switcher.transition().is_some() {
        return Err(SnapshotError::WorkInFlight);
    }
    if show.desired_switcher().fade_to_black_is_automatic()
        || realized_switcher.fade_to_black_is_automatic()
    {
        return Err(SnapshotError::WorkInFlight);
    }
    if show.inputs() != realized_switcher.inputs() {
        return Err(SnapshotError::IncompatibleSwitcher);
    }
    if show.desired_switcher().program() != realized_switcher.program()
        || show.desired_switcher().preview() != realized_switcher.preview()
        || show.desired_switcher().fade_to_black() != realized_switcher.fade_to_black()
        || show.desired_switcher().fade_to_black_position()
            != realized_switcher.fade_to_black_position()
        || show.desired_switcher().stingers() != realized_switcher.stingers()
        || show.desired_switcher().overlays() != realized_switcher.overlays()
        || show.desired_switcher().streams() != realized_switcher.streams()
        || show.desired_switcher().running_stream_targets()
            != realized_switcher.running_stream_targets()
        || RecorderSlot::ALL.into_iter().any(|slot| {
            show.desired_switcher().recording_desired(slot)
                != realized_switcher.recording_desired(slot)
        })
    {
        return Err(SnapshotError::MismatchedSwitcherRouting);
    }
    match (show.desired_switcher().t_bar(), realized_switcher.t_bar()) {
        (None, None) => {}
        (Some(desired), Some(realized))
            if manual_transition_kind_supported(desired.kind())
                && manual_transition_kind_supported(realized.kind())
                && desired.kind() == realized.kind()
                && desired.from() == realized.from()
                && desired.to() == realized.to()
                && desired.interval_start() == TBarPosition::START
                && desired.position() == realized.position()
                && realized.interval_start() == realized.position() => {}
        _ => return Err(SnapshotError::MismatchedManualTransition),
    }

    let cursor = restore_state.frame_cursor.get();
    let pacer = fm_scheduler::FramePacer::new(frame_rate, 0);
    let expected_clock_ns = if cursor == 0 {
        0
    } else {
        pacer
            .deadline_for(FrameNumber::new(cursor - 1))
            .map_err(|_| SnapshotError::InvalidFrameCounter)?
            .at_ns
    };
    if restore_state.clock_time.as_nanos() != expected_clock_ns {
        return Err(SnapshotError::ClockTimeMismatch {
            expected_ns: expected_clock_ns,
            actual_ns: restore_state.clock_time.as_nanos(),
        });
    }

    let mut accepted_commands = 0_u64;
    for (_, receipt) in &restore_state.receipts {
        if let Some(accepted) = receipt.accepted() {
            accepted_commands = accepted_commands
                .checked_add(1)
                .ok_or(SnapshotError::InvalidFrameCounter)?;
            if accepted.result.target_frame >= restore_state.frame_cursor {
                return Err(SnapshotError::UnrealizedAcceptedReceipt {
                    target_frame: accepted.result.target_frame.get(),
                    frame_cursor: cursor,
                });
            }
        }
    }

    // Every accepted command advances all three counters together, so they
    // must agree. Persisted receipts are a bounded retention window over the
    // accepted commands, so they may cover fewer commands than the counters
    // record, but never more.
    if accepted_commands > restore_state.revision.get()
        || restore_state.event_sequence.get() != restore_state.revision.get()
        || restore_state.runtime_generation.get() != restore_state.revision.get()
    {
        return Err(SnapshotError::CounterMismatch {
            accepted_commands,
            revision: restore_state.revision.get(),
            event_sequence: restore_state.event_sequence.get(),
            runtime_generation: restore_state.runtime_generation.get(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct EngineMutation {
    command: EngineCommand,
    target_frame: FrameNumber,
    /// The realized switcher's on-air use of a title command's input.
    realized_on_air: Option<InputUsage>,
}

impl Mutation<ShowState, EngineEvent, EngineAcceptance> for EngineMutation {
    #[allow(clippy::too_many_lines)]
    fn apply(
        self,
        state: &mut ShowState,
        events: &mut Vec<EngineEvent>,
    ) -> Result<EngineAcceptance, Rejection> {
        if matches!(
            &self.command,
            EngineCommand::Undo { .. } | EngineCommand::Redo { .. }
        ) {
            return Err(Rejection::new(
                RejectionCode::Internal,
                "undo and redo must be resolved before mutation",
            ));
        }
        if let EngineCommand::SetEditLock { scope, locked } = self.command {
            return apply_edit_lock_mutation(state, events, scope, locked, self.target_frame);
        }
        check_edit_locks(state, &self.command).map_err(crate::EditRejection::into_rejection)?;
        if is_title_command(&self.command) {
            return apply_title_mutation(state, events, self);
        }
        if matches!(
            &self.command,
            EngineCommand::ConfigureStinger { .. } | EngineCommand::RemoveStinger { .. }
        ) {
            return apply_stinger_mutation(state, events, self);
        }
        if matches!(&self.command, EngineCommand::RenameInput { .. }) {
            let EngineCommand::RenameInput { input, name } = self.command else {
                unreachable!("only input renames enter the rename mutation")
            };
            state
                .rename_input(input, name.clone())
                .map_err(rename_input_rejection)?;
            events.push(EngineEvent::InputRenamed { input, name });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. }
        ) {
            return apply_input_catalogue_mutation(state, events, self);
        }
        if let EngineCommand::ReorderInputs { inputs } = &self.command {
            state
                .reorder_inputs(inputs.clone())
                .map_err(reorder_inputs_rejection)?;
            events.push(EngineEvent::InputOrderChanged {
                inputs: inputs.clone(),
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetInputAudioStrip {
            input,
            state: strip_state,
        } = &self.command
        {
            if !(MIN_INPUT_AUDIO_GAIN_MILLIDB..=MAX_INPUT_AUDIO_GAIN_MILLIDB)
                .contains(&strip_state.gain_millidb)
            {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio gain must be between {MIN_INPUT_AUDIO_GAIN_MILLIDB} and \
                         {MAX_INPUT_AUDIO_GAIN_MILLIDB} millidB"
                    ),
                ));
            }
            if !(MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS..=MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS)
                .contains(&strip_state.balance_basis_points)
            {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio balance must be between \
                         {MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS} and \
                         {MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS} basis points"
                    ),
                ));
            }
            if strip_state.delay_samples > MAX_INPUT_AUDIO_DELAY_SAMPLES {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio delay must not exceed {MAX_INPUT_AUDIO_DELAY_SAMPLES} samples"
                    ),
                ));
            }
            state
                .set_input_audio_strip(*input, *strip_state)
                .map_err(|_| {
                    Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!("input {input} is not part of this show"),
                    )
                })?;
            events.push(EngineEvent::DesiredSwitcherChanged(self.command));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::StreamStart { .. } | EngineCommand::StreamStop { .. }
        ) {
            return apply_stream_mutation(state, events, self);
        }
        if matches!(
            &self.command,
            EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. }
        ) {
            return apply_recording_mutation(state, events, self);
        }
        if matches!(
            &self.command,
            EngineCommand::TakeOverlay { .. }
                | EngineCommand::UpdateOverlay { .. }
                | EngineCommand::OverlayOff { .. }
                | EngineCommand::ConfigureOverlayTransition { .. }
                | EngineCommand::ConfigureOverlayAppearance { .. }
                | EngineCommand::QueueOverlay { .. }
                | EngineCommand::TakeNextOverlay { .. }
                | EngineCommand::SetOverlayOutputInclusion { .. }
        ) {
            return apply_overlay_mutation(state, events, self);
        }
        if matches!(&self.command, EngineCommand::FadeToBlack { .. }) {
            return apply_fade_to_black_mutation(state, events, self);
        }
        let switcher_command = match &self.command {
            EngineCommand::RenameInput { .. } => {
                unreachable!("input renames return before switcher command mapping")
            }
            EngineCommand::ReorderInputs { .. } => {
                unreachable!("input reorders return before switcher command mapping")
            }
            EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. } => {
                unreachable!("input catalogue mutations return before switcher command mapping")
            }
            EngineCommand::SetInputAudioStrip { .. } => {
                unreachable!("audio-strip mutations return before switcher command mapping")
            }
            EngineCommand::SelectPreview(input) => SwitcherCommand::SelectPreview(*input),
            EngineCommand::Cut => SwitcherCommand::Cut,
            EngineCommand::Fade { duration_frames } => {
                validate_transition_duration("fade", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::AlphaFade { duration_frames } => {
                validate_transition_duration("alpha fade", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Slide { duration_frames } => {
                validate_transition_duration("slide", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Zoom { duration_frames } => {
                validate_transition_duration("zoom", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Stinger {
                slot,
                duration_frames,
            } => {
                validate_transition_duration("stinger", *duration_frames)?;
                // The runtime switcher refuses every automatic transition,
                // including a KeepProgram fallback, while a manual one is
                // active; accepting it here would fail the scheduled tick.
                if state.desired_switcher().t_bar().is_some() {
                    return Err(switcher_rejection(SwitcherError::TransitionInProgress));
                }
                match state.desired_switcher().stinger_playback_decision(*slot) {
                    fm_switcher::StingerPlaybackDecision::Play => {
                        let descriptor = state
                            .desired_switcher()
                            .stinger(*slot)
                            .descriptor()
                            .expect("playable stinger has a descriptor");
                        if descriptor.cut_point_frames > *duration_frames {
                            return Err(switcher_rejection(
                                SwitcherError::StingerCutPointOutOfRange {
                                    slot: *slot,
                                    cut_point_frames: descriptor.cut_point_frames,
                                    duration_frames: *duration_frames,
                                },
                            ));
                        }
                        SwitcherCommand::Cut
                    }
                    fm_switcher::StingerPlaybackDecision::Fallback(
                        fm_switcher::MissingMediaFallback::KeepProgram,
                    ) => {
                        events.push(EngineEvent::DesiredSwitcherChanged(self.command));
                        return Ok(EngineAcceptance {
                            target_frame: self.target_frame,
                        });
                    }
                    fm_switcher::StingerPlaybackDecision::Fallback(
                        fm_switcher::MissingMediaFallback::Cut
                        | fm_switcher::MissingMediaFallback::Fade,
                    ) => SwitcherCommand::Cut,
                    fm_switcher::StingerPlaybackDecision::Unconfigured => {
                        return Err(switcher_rejection(SwitcherError::UnconfiguredStinger(
                            *slot,
                        )));
                    }
                }
            }
            EngineCommand::ConfigureStinger { .. } | EngineCommand::RemoveStinger { .. } => {
                unreachable!("Stinger mutations return before switcher command mapping")
            }
            EngineCommand::TakeOverlay { .. }
            | EngineCommand::UpdateOverlay { .. }
            | EngineCommand::OverlayOff { .. }
            | EngineCommand::ConfigureOverlayTransition { .. }
            | EngineCommand::ConfigureOverlayAppearance { .. }
            | EngineCommand::QueueOverlay { .. }
            | EngineCommand::TakeNextOverlay { .. }
            | EngineCommand::SetOverlayOutputInclusion { .. } => {
                unreachable!("overlay mutations return before switcher command mapping")
            }
            EngineCommand::Wipe { duration_frames } => {
                validate_transition_duration("wipe", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::FadeToBlack { .. } => {
                unreachable!("Fade-to-Black mutations return before switcher command mapping")
            }
            EngineCommand::StartManualTransition { kind } => SwitcherCommand::StartTBar {
                kind: manual_transition_kind(*kind),
            },
            EngineCommand::SetManualTransitionPosition { position } => {
                SwitcherCommand::SetTBarPosition(position.domain())
            }
            EngineCommand::CommitManualTransition => SwitcherCommand::CommitTBar,
            EngineCommand::CancelManualTransition => SwitcherCommand::CancelTBar,
            EngineCommand::StreamStart { .. } | EngineCommand::StreamStop { .. } => {
                unreachable!("stream mutations return before switcher command mapping")
            }
            EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. } => {
                unreachable!("recording mutations return before switcher command mapping")
            }
            EngineCommand::Undo { .. }
            | EngineCommand::Redo { .. }
            | EngineCommand::SetEditLock { .. } => {
                unreachable!("history and lock mutations return before switcher command mapping")
            }
            EngineCommand::SetTitleFields { .. }
            | EngineCommand::TakeTitle { .. }
            | EngineCommand::TitleOffAir { .. } => {
                unreachable!("title mutations return before switcher command mapping")
            }
        };
        state
            .desired_switcher_mut()
            .apply(switcher_command)
            .map_err(switcher_rejection)?;
        events.push(EngineEvent::DesiredSwitcherChanged(self.command));
        Ok(EngineAcceptance {
            target_frame: self.target_frame,
        })
    }
}

fn apply_input_catalogue_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    match command {
        EngineCommand::AddInput {
            input,
            name,
            source,
        } => {
            state
                .add_input(input, name.clone())
                .map_err(input_catalogue_rejection)?;
            events.push(EngineEvent::InputAdded {
                input,
                name,
                source,
            });
        }
        EngineCommand::RemoveInput { input } => {
            state
                .remove_input(input)
                .map_err(input_catalogue_rejection)?;
            events.push(EngineEvent::InputRemoved { input });
        }
        _ => unreachable!("only input catalogue commands are delegated"),
    }
    Ok(EngineAcceptance { target_frame })
}

fn input_catalogue_rejection(error: InputCatalogueError) -> Rejection {
    let (code, field, issue) = match error {
        InputCatalogueError::DuplicateInput(_) => {
            (RejectionCode::Conflict, "input", "duplicate_input")
        }
        InputCatalogueError::EmptyName | InputCatalogueError::NameTooLong => {
            (RejectionCode::InvalidCommand, "name", "invalid_name")
        }
        InputCatalogueError::DuplicateName => (RejectionCode::Conflict, "name", "duplicate_name"),
        InputCatalogueError::TooManyInputs { .. } => {
            (RejectionCode::Conflict, "input", "too_many_inputs")
        }
        InputCatalogueError::UnknownInput(_) => (RejectionCode::NotFound, "input", "unknown_input"),
        InputCatalogueError::InUse { .. } => (RejectionCode::Conflict, "input", "input_in_use"),
    };
    let message = error.to_string();
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

fn apply_edit_lock_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    scope: EditLockScope,
    locked: bool,
    target_frame: FrameNumber,
) -> Result<EngineAcceptance, Rejection> {
    match scope {
        EditLockScope::Project => state.set_project_locked(locked),
        EditLockScope::Input(input) => state.set_input_locked(input, locked).map_err(|_| {
            Rejection::new(
                RejectionCode::NotFound,
                format!("show does not contain input {input}"),
            )
        })?,
    }
    // Exactly one durable event per acceptance keeps revision, event
    // sequence, and runtime generation in lockstep for restore validation.
    events.push(EngineEvent::EditLockChanged { scope, locked });
    Ok(EngineAcceptance { target_frame })
}

fn apply_stream_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    let running = matches!(command, EngineCommand::StreamStart { .. });
    let target = match &command {
        EngineCommand::StreamStart { target } | EngineCommand::StreamStop { target } => *target,
        _ => unreachable!("only stream mutations are delegated"),
    };
    state
        .desired_switcher_mut()
        .set_stream_running(target, running)
        .map_err(switcher_rejection)?;
    events.push(EngineEvent::StreamsChanged {
        streams: desired_stream_statuses(state),
    });
    Ok(EngineAcceptance { target_frame })
}

fn apply_recording_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        target_frame,
        command,
        ..
    } = mutation;
    let (recorder, active) = match &command {
        EngineCommand::RecordStart { recorder } => (*recorder, true),
        EngineCommand::RecordStop { recorder } => (*recorder, false),
        _ => unreachable!("only recording commands are delegated"),
    };
    state
        .desired_switcher_mut()
        .set_recording_desired(recorder, active)
        .map_err(switcher_rejection)?;
    let resulting = state.desired_switcher().recording_desired(recorder);
    // Every acceptance carries one durable event, exactly like the stream
    // roster events: the snapshot's revision, event sequence, and runtime
    // generation advance together per accepted command, so a command that
    // leaves the addressed slot unchanged (an operator stopping a stopped
    // recorder) must still emit it or the persisted counters diverge and the
    // project refuses to restore.
    events.push(EngineEvent::RecordingChanged {
        recorder,
        active: resulting,
    });
    Ok(EngineAcceptance { target_frame })
}

fn desired_stream_statuses(state: &ShowState) -> Vec<EngineStreamStatus> {
    state
        .streams()
        .iter()
        .map(|stream| EngineStreamStatus {
            target: stream.id(),
            name: stream.name().to_owned(),
            desired_running: state
                .stream_running(stream.id())
                .expect("inventoried stream has a desired flag"),
        })
        .collect()
}

fn apply_fade_to_black_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    let EngineCommand::FadeToBlack {
        active,
        duration_frames,
    } = &command
    else {
        unreachable!("only Fade-to-Black mutations are delegated")
    };
    validate_fade_to_black_duration(*duration_frames)?;
    state
        .desired_switcher_mut()
        .restore_settled_fade_to_black(*active);
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

fn apply_overlay_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    if let EngineCommand::SetOverlayOutputInclusion { output, .. } = &command
        && !state
            .outputs()
            .iter()
            .any(|(candidate, _)| candidate == output)
    {
        return Err(Rejection::new(
            RejectionCode::NotFound,
            format!("show does not contain output {output}"),
        ));
    }
    let switcher_command = match &command {
        EngineCommand::TakeOverlay { channel, source } => SwitcherCommand::TakeOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::UpdateOverlay { channel, source } => SwitcherCommand::UpdateOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::OverlayOff { channel } => SwitcherCommand::OverlayOff(*channel),
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => SwitcherCommand::ConfigureOverlayTransition {
            channel: *channel,
            transition: *transition,
            duration_frames: *duration_frames,
        },
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => SwitcherCommand::ConfigureOverlayAppearance {
            channel: *channel,
            position: *position,
            border: *border,
        },
        EngineCommand::QueueOverlay { channel, source } => SwitcherCommand::QueueOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::TakeNextOverlay { channel } => SwitcherCommand::TakeNextOverlay(*channel),
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => SwitcherCommand::SetOverlayOutputInclusion {
            channel: *channel,
            output: *output,
            included: *included,
        },
        _ => unreachable!("only overlay mutation commands are delegated"),
    };
    state
        .desired_switcher_mut()
        .apply(switcher_command)
        .map_err(switcher_rejection)?;
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

fn apply_stinger_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    if state.desired_switcher().t_bar().is_some() {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            "Stinger slot configuration cannot change during a manual transition",
        ));
    }
    match &command {
        EngineCommand::ConfigureStinger { slot, descriptor } => {
            state
                .desired_switcher_mut()
                .configure_stinger(*slot, *descriptor)
                .map_err(switcher_rejection)?;
            if descriptor.preload {
                let _ = state
                    .desired_switcher_mut()
                    .preload_stinger(*slot, true)
                    .map_err(switcher_rejection)?;
            }
        }
        EngineCommand::RemoveStinger { slot } => state.remove_stinger(*slot),
        _ => unreachable!("only Stinger mutation commands are delegated"),
    }
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

#[allow(clippy::too_many_lines)]
fn apply_runtime(
    switcher: &mut SwitcherState,
    command: EngineCommand,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    if matches!(
        &command,
        EngineCommand::RenameInput { .. }
            | EngineCommand::SetInputAudioStrip { .. }
            | EngineCommand::SetEditLock { .. }
            | EngineCommand::SetTitleFields { .. }
    ) {
        return Ok(Vec::new());
    }
    if let Some(result) = apply_runtime_stream(switcher, &command) {
        return result;
    }
    if let Some(result) = apply_runtime_recording(switcher, &command) {
        return result;
    }
    if let EngineCommand::ReorderInputs { inputs } = command {
        switcher
            .reorder_inputs(inputs)
            .expect("accepted engine input orders are validated");
        return Ok(Vec::new());
    }
    match &command {
        EngineCommand::AddInput { input, .. } => {
            switcher.add_input(*input)?;
            return Ok(Vec::new());
        }
        EngineCommand::RemoveInput { input } => {
            switcher.remove_input(*input)?;
            return Ok(Vec::new());
        }
        _ => {}
    }
    if let EngineCommand::FadeToBlack {
        active,
        duration_frames,
    } = &command
    {
        return Ok(switcher
            .request_fade_to_black(*active, *duration_frames)
            .expect("accepted engine FTB durations are validated"));
    }
    if is_overlay_command(&command) {
        return apply_runtime_overlay(switcher, &command);
    }
    switcher.apply(match command {
        EngineCommand::RenameInput { .. } => {
            unreachable!("input renames return before switcher command mapping")
        }
        EngineCommand::ReorderInputs { .. } => {
            unreachable!("input reorders return before switcher command mapping")
        }
        EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. } => {
            unreachable!("input catalogue commands return before switcher command mapping")
        }
        EngineCommand::SetInputAudioStrip { .. } => {
            unreachable!("audio-strip commands return before switcher command mapping")
        }
        EngineCommand::SelectPreview(input) => SwitcherCommand::SelectPreview(input),
        EngineCommand::Cut => SwitcherCommand::Cut,
        EngineCommand::Fade { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Fade,
            duration_frames,
        },
        EngineCommand::AlphaFade { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::AlphaFade,
            duration_frames,
        },
        EngineCommand::Slide { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Slide,
            duration_frames,
        },
        EngineCommand::Zoom { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Zoom,
            duration_frames,
        },
        EngineCommand::Stinger {
            slot,
            duration_frames,
        } => SwitcherCommand::Transition {
            kind: TransitionKind::Stinger(slot),
            duration_frames,
        },
        EngineCommand::ConfigureStinger { slot, descriptor } => {
            switcher.configure_stinger(slot, descriptor)?;
            let mut events = Vec::new();
            if descriptor.preload {
                events.push(switcher.preload_stinger(slot, true)?);
            }
            return Ok(events);
        }
        EngineCommand::RemoveStinger { slot } => {
            switcher.remove_stinger(slot);
            return Ok(Vec::new());
        }
        EngineCommand::TakeOverlay { .. }
        | EngineCommand::UpdateOverlay { .. }
        | EngineCommand::OverlayOff { .. }
        | EngineCommand::ConfigureOverlayTransition { .. }
        | EngineCommand::ConfigureOverlayAppearance { .. }
        | EngineCommand::QueueOverlay { .. }
        | EngineCommand::TakeNextOverlay { .. }
        | EngineCommand::SetOverlayOutputInclusion { .. } => {
            unreachable!("overlay commands return before switcher command mapping")
        }
        EngineCommand::Wipe { duration_frames } => SwitcherCommand::Wipe { duration_frames },
        EngineCommand::FadeToBlack { .. } => {
            unreachable!("Fade-to-Black commands return before switcher command mapping")
        }
        EngineCommand::StartManualTransition { kind } => SwitcherCommand::StartTBar {
            kind: manual_transition_kind(kind),
        },
        EngineCommand::SetManualTransitionPosition { position } => {
            SwitcherCommand::SetTBarPosition(position.domain())
        }
        EngineCommand::CommitManualTransition => SwitcherCommand::CommitTBar,
        EngineCommand::CancelManualTransition => SwitcherCommand::CancelTBar,
        EngineCommand::StreamStart { .. } | EngineCommand::StreamStop { .. } => {
            unreachable!("stream commands return before switcher command mapping")
        }
        EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. } => {
            unreachable!("recording commands return before switcher command mapping")
        }
        EngineCommand::Undo { .. } | EngineCommand::Redo { .. } => {
            unreachable!("undo and redo are resolved to edits before scheduling")
        }
        EngineCommand::SetEditLock { .. } => {
            unreachable!("edit-lock commands return before switcher command mapping")
        }
        EngineCommand::SetTitleFields { .. }
        | EngineCommand::TakeTitle { .. }
        | EngineCommand::TitleOffAir { .. } => {
            unreachable!("title commands are scheduled as overlay commands or ignored")
        }
    })
}

fn apply_runtime_stream(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Option<Result<Vec<SwitcherEvent>, SwitcherError>> {
    match command {
        EngineCommand::StreamStart { target } => Some(switcher.set_stream_running(*target, true)),
        EngineCommand::StreamStop { target } => Some(switcher.set_stream_running(*target, false)),
        _ => None,
    }
}

fn apply_runtime_recording(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Option<Result<Vec<SwitcherEvent>, SwitcherError>> {
    match command {
        EngineCommand::RecordStart { recorder } => {
            Some(switcher.set_recording_desired(*recorder, true))
        }
        EngineCommand::RecordStop { recorder } => {
            Some(switcher.set_recording_desired(*recorder, false))
        }
        _ => None,
    }
}

fn apply_runtime_overlay(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    match command {
        EngineCommand::TakeOverlay { channel, source } => {
            switcher.request_overlay_take(*channel, *source)
        }
        EngineCommand::OverlayOff { channel } => Ok(switcher.request_overlay_off(*channel)),
        EngineCommand::TakeNextOverlay { channel } => switcher.request_overlay_take_next(*channel),
        EngineCommand::UpdateOverlay { channel, source } => {
            switcher.apply(SwitcherCommand::UpdateOverlay {
                channel: *channel,
                source: *source,
            })
        }
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => switcher.apply(SwitcherCommand::ConfigureOverlayTransition {
            channel: *channel,
            transition: *transition,
            duration_frames: *duration_frames,
        }),
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => switcher.apply(SwitcherCommand::ConfigureOverlayAppearance {
            channel: *channel,
            position: *position,
            border: *border,
        }),
        EngineCommand::QueueOverlay { channel, source } => {
            switcher.apply(SwitcherCommand::QueueOverlay {
                channel: *channel,
                source: *source,
            })
        }
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => switcher.apply(SwitcherCommand::SetOverlayOutputInclusion {
            channel: *channel,
            output: *output,
            included: *included,
        }),
        _ => unreachable!("only overlay commands are delegated"),
    }
}

fn runtime_transition_kind(show: &ShowState, command: &EngineCommand) -> Option<TransitionKind> {
    match command {
        EngineCommand::Fade { .. } => Some(TransitionKind::Fade),
        EngineCommand::AlphaFade { .. } => Some(TransitionKind::AlphaFade),
        EngineCommand::Slide { .. } => Some(TransitionKind::Slide),
        EngineCommand::Zoom { .. } => Some(TransitionKind::Zoom),
        EngineCommand::Wipe { .. } => Some(TransitionKind::Wipe),
        EngineCommand::Stinger { slot, .. } => stinger_runtime_transition(show, *slot),
        EngineCommand::ConfigureStinger { .. }
        | EngineCommand::RemoveStinger { .. }
        | EngineCommand::RenameInput { .. }
        | EngineCommand::ReorderInputs { .. }
        | EngineCommand::AddInput { .. }
        | EngineCommand::RemoveInput { .. }
        | EngineCommand::SetInputAudioStrip { .. }
        | EngineCommand::TakeOverlay { .. }
        | EngineCommand::UpdateOverlay { .. }
        | EngineCommand::OverlayOff { .. }
        | EngineCommand::ConfigureOverlayTransition { .. }
        | EngineCommand::ConfigureOverlayAppearance { .. }
        | EngineCommand::QueueOverlay { .. }
        | EngineCommand::TakeNextOverlay { .. }
        | EngineCommand::SetOverlayOutputInclusion { .. }
        | EngineCommand::SelectPreview(_)
        | EngineCommand::Cut
        | EngineCommand::FadeToBlack { .. }
        | EngineCommand::StartManualTransition { .. }
        | EngineCommand::SetManualTransitionPosition { .. }
        | EngineCommand::CommitManualTransition
        | EngineCommand::CancelManualTransition
        | EngineCommand::StreamStart { .. }
        | EngineCommand::StreamStop { .. }
        | EngineCommand::RecordStart { .. }
        | EngineCommand::RecordStop { .. }
        | EngineCommand::Undo { .. }
        | EngineCommand::Redo { .. }
        | EngineCommand::SetEditLock { .. }
        | EngineCommand::SetTitleFields { .. }
        | EngineCommand::TakeTitle { .. }
        | EngineCommand::TitleOffAir { .. } => None,
    }
}

const fn is_overlay_command(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::TakeOverlay { .. }
            | EngineCommand::UpdateOverlay { .. }
            | EngineCommand::OverlayOff { .. }
            | EngineCommand::ConfigureOverlayTransition { .. }
            | EngineCommand::ConfigureOverlayAppearance { .. }
            | EngineCommand::QueueOverlay { .. }
            | EngineCommand::TakeNextOverlay { .. }
            | EngineCommand::SetOverlayOutputInclusion { .. }
    )
}

const fn is_title_command(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::SetTitleFields { .. }
            | EngineCommand::TakeTitle { .. }
            | EngineCommand::TitleOffAir { .. }
    )
}

/// Returns the runtime action that realizes an accepted `command`.
///
/// Title takes and off-airs realize as the overlay commands they resolve to
/// against the pre-command desired state, which is the state the mutation
/// validates; a refused command's action is cancelled before it runs.
fn runtime_action(state: &ShowState, command: &EngineCommand) -> EngineCommand {
    match command {
        EngineCommand::TakeTitle { input, channel, .. } => EngineCommand::TakeOverlay {
            channel: *channel,
            source: *input,
        },
        EngineCommand::TitleOffAir { input } => {
            match title_off_air_channel(state.desired_switcher(), None, *input) {
                Ok(channel) => EngineCommand::OverlayOff { channel },
                Err(_) => command.clone(),
            }
        }
        _ => command.clone(),
    }
}

/// Resolves the one desired overlay channel that takes a title off air.
fn title_off_air_channel(
    desired: &SwitcherState,
    realized_on_air: Option<InputUsage>,
    input: InputId,
) -> Result<OverlayChannelId, TitleRejection> {
    let on_bus = |usage: Option<InputUsage>| {
        matches!(
            usage,
            Some(InputUsage::Program | InputUsage::Transition | InputUsage::ManualTransition)
        )
    };
    if on_bus(desired.on_air_usage(input)) || on_bus(realized_on_air) {
        return Err(TitleRejection::OnProgram(input));
    }
    let mut channels = desired.overlay_channels_showing(input);
    match (channels.next(), channels.next()) {
        (None, _) => Err(TitleRejection::OffAir(input)),
        (Some(channel), None) => Ok(channel),
        (Some(_), Some(_)) => Err(TitleRejection::OnMultipleOverlays(input)),
    }
}

fn apply_title_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        realized_on_air,
    } = mutation;
    let input = match &command {
        EngineCommand::SetTitleFields { input, .. }
        | EngineCommand::TakeTitle { input, .. }
        | EngineCommand::TitleOffAir { input } => *input,
        _ => unreachable!("only title commands are delegated"),
    };
    if state.title(input).is_none() {
        return Err(TitleRejection::UnknownTitle(input).into_rejection());
    }
    let on_air = state
        .desired_switcher()
        .on_air_usage(input)
        .or(realized_on_air)
        .is_some();
    match &command {
        EngineCommand::SetTitleFields {
            base_revision,
            require,
            fields,
            ..
        } => {
            match (require, on_air) {
                (TitleAirRequirement::OffAir, true) => {
                    return Err(TitleRejection::OnAir(input).into_rejection());
                }
                (TitleAirRequirement::OnAir, false) => {
                    return Err(TitleRejection::OffAir(input).into_rejection());
                }
                _ => {}
            }
            let revision = update_title_fields(state, input, *base_revision, fields)?;
            events.push(EngineEvent::TitleFieldsChanged { input, revision });
        }
        EngineCommand::TakeTitle {
            channel,
            base_revision,
            fields,
            ..
        } => {
            if on_air {
                return Err(TitleRejection::OnAir(input).into_rejection());
            }
            update_title_fields(state, input, *base_revision, fields)?;
            state
                .desired_switcher_mut()
                .apply(SwitcherCommand::TakeOverlay {
                    channel: *channel,
                    source: input,
                })
                .map_err(switcher_rejection)?;
            events.push(EngineEvent::DesiredSwitcherChanged(command));
        }
        EngineCommand::TitleOffAir { .. } => {
            let channel = title_off_air_channel(state.desired_switcher(), realized_on_air, input)
                .map_err(TitleRejection::into_rejection)?;
            state
                .desired_switcher_mut()
                .apply(SwitcherCommand::OverlayOff(channel))
                .map_err(switcher_rejection)?;
            events.push(EngineEvent::DesiredSwitcherChanged(command));
        }
        _ => unreachable!("only title commands are delegated"),
    }
    Ok(EngineAcceptance { target_frame })
}

fn update_title_fields(
    state: &mut ShowState,
    input: InputId,
    base_revision: u64,
    fields: &[(FieldId, FieldValue)],
) -> Result<u64, Rejection> {
    let title = state
        .title_mut(input)
        .ok_or_else(|| TitleRejection::UnknownTitle(input).into_rejection())?;
    title
        .fields
        .update(base_revision, fields)
        .map_err(|error| TitleRejection::from(error).into_rejection())
}

fn rename_input_rejection(error: RenameInputError) -> Rejection {
    let code = match error {
        RenameInputError::UnknownInput(_) => RejectionCode::NotFound,
        RenameInputError::EmptyName | RenameInputError::NameTooLong => {
            RejectionCode::InvalidCommand
        }
        RenameInputError::DuplicateName => RejectionCode::Conflict,
    };
    Rejection::new(code, error.to_string())
}

fn reorder_inputs_rejection(error: InputOrderError) -> Rejection {
    let code = match error {
        InputOrderError::UnknownInput(_) => RejectionCode::NotFound,
        InputOrderError::EmptyOrder
        | InputOrderError::WrongLength { .. }
        | InputOrderError::DuplicateInput(_) => RejectionCode::InvalidCommand,
    };
    Rejection::new(code, error.to_string())
}

const fn engine_fade_to_black_state(switcher: &SwitcherState) -> EngineFadeToBlackState {
    EngineFadeToBlackState {
        active: switcher.fade_to_black(),
        position: switcher.fade_to_black_position(),
    }
}

fn validate_fade_to_black_duration(duration_frames: u32) -> Result<(), Rejection> {
    if duration_frames == 0 {
        Err(Rejection::new(
            RejectionCode::InvalidCommand,
            "Fade-to-Black duration must be nonzero",
        ))
    } else if duration_frames > fm_switcher::MAX_FADE_TO_BLACK_DURATION_FRAMES {
        Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "Fade-to-Black duration must not exceed {} frames",
                fm_switcher::MAX_FADE_TO_BLACK_DURATION_FRAMES
            ),
        ))
    } else {
        Ok(())
    }
}

fn validate_transition_duration(name: &str, duration_frames: u32) -> Result<(), Rejection> {
    if duration_frames == 0 {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!("{name} duration must be nonzero"),
        ));
    }
    if duration_frames > MAX_TRANSITION_DURATION_FRAMES {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!("{name} duration must not exceed {MAX_TRANSITION_DURATION_FRAMES} frames"),
        ));
    }
    Ok(())
}

fn stinger_runtime_transition(
    show: &ShowState,
    slot: fm_switcher::StingerSlotId,
) -> Option<TransitionKind> {
    match show.desired_switcher().stinger_playback_decision(slot) {
        fm_switcher::StingerPlaybackDecision::Play => Some(TransitionKind::Stinger(slot)),
        fm_switcher::StingerPlaybackDecision::Fallback(fm_switcher::MissingMediaFallback::Fade) => {
            Some(TransitionKind::Fade)
        }
        fm_switcher::StingerPlaybackDecision::Fallback(
            fm_switcher::MissingMediaFallback::Cut | fm_switcher::MissingMediaFallback::KeepProgram,
        )
        | fm_switcher::StingerPlaybackDecision::Unconfigured => None,
    }
}

const fn manual_transition_kind(kind: EngineManualTransitionKind) -> TransitionKind {
    match kind {
        EngineManualTransitionKind::Fade => TransitionKind::Fade,
        EngineManualTransitionKind::Wipe => TransitionKind::Wipe,
        EngineManualTransitionKind::AlphaFade => TransitionKind::AlphaFade,
        EngineManualTransitionKind::Slide => TransitionKind::Slide,
    }
}

const fn manual_transition_kind_supported(kind: TransitionKind) -> bool {
    matches!(
        kind,
        TransitionKind::Fade
            | TransitionKind::Wipe
            | TransitionKind::AlphaFade
            | TransitionKind::Slide
    )
}

fn engine_manual_state(state: fm_switcher::TBarState) -> EngineManualTransitionState {
    EngineManualTransitionState {
        kind: match state.kind() {
            TransitionKind::Fade => EngineManualTransitionKind::Fade,
            TransitionKind::Wipe => EngineManualTransitionKind::Wipe,
            TransitionKind::AlphaFade => EngineManualTransitionKind::AlphaFade,
            TransitionKind::Slide => EngineManualTransitionKind::Slide,
            _ => unreachable!("engine manual transitions are Fade, Wipe, AlphaFade, or Slide"),
        },
        from: state.from(),
        to: state.to(),
        interval_start: EngineManualTransitionPosition::new(state.interval_start().basis_points())
            .expect("switcher manual transition positions are bounded"),
        position: EngineManualTransitionPosition::new(state.position().basis_points())
            .expect("switcher manual transition positions are bounded"),
    }
}

fn switcher_rejection(error: SwitcherError) -> Rejection {
    let code = match error {
        SwitcherError::UnknownInput(_)
        | SwitcherError::UnconfiguredStinger(_)
        | SwitcherError::UnknownStreamTarget(_) => RejectionCode::NotFound,
        SwitcherError::TransitionInProgress
        | SwitcherError::DuplicateInput(_)
        | SwitcherError::InputInUse { .. } => RejectionCode::Conflict,
        SwitcherError::UnsupportedManualTransitionKind
        | SwitcherError::InvalidManualTransitionRoute
        | SwitcherError::PreviewEqualsProgram(_)
        | SwitcherError::ZeroDuration
        | SwitcherError::InvalidOverlayTransitionDuration { .. }
        | SwitcherError::OverlayQueueFull { .. }
        | SwitcherError::OverlayQueueEmpty(_)
        | SwitcherError::StingerCutPointOutOfRange { .. }
        | SwitcherError::DuplicateStreamTarget(_)
        | SwitcherError::InvalidStreamName
        | SwitcherError::TooManyStreams { .. } => RejectionCode::InvalidCommand,
    };
    Rejection::new(code, error.to_string())
}
