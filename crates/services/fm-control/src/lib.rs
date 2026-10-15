//! Authoritative, transport-independent command control and event resumption.
//!
//! The retained log in this crate is bounded memory, not filesystem durability.
//! Newly accepted records are returned to the caller so a persistence layer can
//! compose durable storage without coupling it to a transport.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
};

mod animation_preset;
mod automation;
mod grade;
mod playlist;
mod scene_animation;
mod title_animation;
mod title_layout;
pub use scene_animation::{
    scene_clip_fits_wire, scene_clip_from_wire, scene_clip_to_wire, scene_layout_to_wire,
};
pub use title_animation::{title_clip_fits_wire, title_clip_from_wire, title_clip_to_wire};
pub use title_layout::title_layout_to_wire;

pub use automation::{
    ArmTimeAuthority, AuthorityResolver, AutomationBinding, AutomationError, AutomationIdentity,
    AutomationLimits, AutomationMissedOccurrences, AutomationPlane, AutomationRefusal,
    AutomationRefusalReason, AutomationRequest, AutomationResource, AutomationSource,
    AutomationSubmission, AutomationTick,
};

use fm_auth::{AuthorizationDenial, CommandClass, Policy, Principal};
use fm_command::{CommandReceipt, DurableEvent, IdempotencyKey, RejectionCode, Revision};
use fm_engine::{
    EditLockScope as EngineEditLockScope, Engine, EngineAcceptance, EngineCommand,
    EngineCommandOutcome, EngineError, EngineEvent, EngineFadeToBlackState,
    EngineInputAudioStripState, EngineInputSource, EngineManualTransitionKind,
    EngineManualTransitionPosition, EngineManualTransitionState, EnginePrepareOutcome,
    EngineSnapshot, EngineTitle, FrameResult, PreparedEngineExecution, SnapshotError,
    TitleAirRequirement as EngineTitleAirRequirement,
};
use fm_protocol::{
    CommandMessage, CommandPayload, CommandResult, EditHistoryStatus, EditLockScope,
    EditLockStatus, EngineIdentity, EventCursor, EventMessage, EventPayload, FadeToBlackPosition,
    FadeToBlackState, FieldIssue, GradeStateMessage, InputAudioStripStatus, InputStatus,
    LiveInputSource, ManualTransitionKind, ManualTransitionPosition, ManualTransitionState,
    ManualTransitionStatus, OutputStatus, OverlayBorderPreset as ProtocolOverlayBorder,
    OverlayPositionPreset as ProtocolOverlayPosition, OverlayStatus,
    OverlayTransitionKind as ProtocolOverlayTransition, PlaylistStateMessage, RecordingStatus,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage,
    StingerAudioPolicy as ProtocolStingerAudioPolicy,
    StingerMissingMediaFallback as ProtocolStingerFallback, StingerReadiness, StingerStatus,
    StreamRealizedState, StreamStatus, TitleAirRequirement, TitleAnimationStateMessage,
    TitleFieldStatus, TitleFieldUpdate, TitleFieldValue, TitlePhase, TitleStateMessage,
    TitleSummary, WireInputId, WireMessage, WireOutputId, WireOverlayChannelId, WireRecorderSlot,
    WireStingerSlotId, WireStreamTargetId, WireTitleFieldId,
};
use fm_switcher::{
    MissingMediaFallback, OverlayBorderPreset as SwitcherOverlayBorder, OverlayChannelId,
    OverlayChannelState, OverlayPositionPreset as SwitcherOverlayPosition,
    OverlayTransitionKind as SwitcherOverlayTransition, RecorderSlot, StingerAudioPolicy,
    StingerDescriptor, StingerPreloadState, StingerSlotId, StreamTargetId,
};
use fm_titles::fm_animation::Phase;
use fm_titles::{Color as TitleColor, FieldId, FieldValue, ImageSource, TitleAirPhase};
use fm_types::InputId;

// The wire carries exactly the title values the engine holds, so the
// protocol's bounds must be the title domain's bounds.
const _: () = {
    assert!(fm_protocol::MAX_TITLE_FIELDS == fm_titles::MAX_TITLE_FIELDS);
    assert!(fm_protocol::MAX_TITLE_TEXT_CHARS == fm_titles::MAX_TITLE_TEXT_CHARS);
    assert!(fm_protocol::MAX_TITLE_CONTENT_BYTES == fm_titles::MAX_TITLE_CONTENT_BYTES);
};

/// Target-free authorization called before an engine command is constructed or validated.
pub trait AuthorizationHook {
    /// Authorizes one command category.
    ///
    /// # Errors
    ///
    /// Returns a safe, target-free denial when the principal lacks permission.
    fn authorize(
        &self,
        principal: &Principal,
        command: CommandClass,
    ) -> Result<(), AuthorizationDenial>;
}

impl AuthorizationHook for Policy {
    fn authorize(
        &self,
        principal: &Principal,
        command: CommandClass,
    ) -> Result<(), AuthorizationDenial> {
        Policy::authorize(self, principal, command)
    }
}

/// The command classes one command needs, resolved against engine state.
///
/// Every command needs its own target-free class. Undo and redo additionally
/// need the class of the edit they would revert or reapply, so history never
/// lets a role change configuration it could not change directly. A title take
/// that also saves field values edits project content as well as switching,
/// so it additionally needs `EditProject`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequiredClasses {
    primary: CommandClass,
    additional: Option<CommandClass>,
}

impl RequiredClasses {
    fn resolve(payload: &CommandPayload, history_target: Option<&EngineCommand>) -> Self {
        let additional = history_target
            .and_then(wire_edit_payload)
            .map(|target| command_class(&target))
            .or_else(|| additional_command_class(payload));
        Self {
            primary: command_class(payload),
            additional,
        }
    }

    /// The payload's own target-free class.
    #[must_use]
    pub const fn primary(self) -> CommandClass {
        self.primary
    }

    /// The second class the command needs, if any.
    #[must_use]
    pub const fn additional(self) -> Option<CommandClass> {
        self.additional
    }

    /// Authorizes every required class, the payload's own class first.
    ///
    /// # Errors
    ///
    /// Returns the first denial.
    pub fn authorize<A: AuthorizationHook + ?Sized>(
        self,
        authorizer: &A,
        principal: &Principal,
    ) -> Result<(), AuthorizationDenial> {
        authorizer.authorize(principal, self.primary)?;
        self.additional
            .map_or(Ok(()), |class| authorizer.authorize(principal, class))
    }
}

/// Independent bounds for resume history and live subscribers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlLimits {
    pub retained_events: usize,
    pub max_subscribers: usize,
    pub subscriber_queue: usize,
}

impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            retained_events: 1_024,
            max_subscribers: 64,
            subscriber_queue: 64,
        }
    }
}

/// A point-in-time protocol snapshot retained for resume fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRecord {
    pub cursor: EventCursor,
    pub snapshot: SnapshotMessage,
}

/// The complete result of a resume decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResumeDecision {
    /// Every revision after the supplied cursor is still contiguous and retained.
    Events(Vec<EventMessage>),
    /// Identity changed, the cursor is invalid, or required events were compacted.
    Snapshot(Box<SnapshotRecord>),
}

/// Wire output whose shape makes command-result-before-events ordering explicit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultBeforeEvents {
    pub result: CommandResult,
    pub events: Vec<EventMessage>,
}

impl ResultBeforeEvents {
    #[must_use]
    pub fn into_wire_messages(self) -> Vec<WireMessage> {
        let mut messages = Vec::with_capacity(self.events.len() + 1);
        messages.push(WireMessage::CommandResult(self.result));
        messages.extend(self.events.into_iter().map(WireMessage::Event));
        messages
    }
}

/// Engine-owned records for a newly accepted command.
///
/// `Some(AcceptedOutcome)` means persistence work is required. Replayed
/// acceptances do not produce this value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedOutcome {
    pub idempotency_key: IdempotencyKey,
    pub receipt: CommandReceipt<EngineAcceptance>,
    pub events: Vec<DurableEvent<EngineEvent>>,
}

/// A command result plus optional persistence and subscriber side effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSubmission {
    pub output: ResultBeforeEvents,
    pub replayed: bool,
    pub accepted: Option<AcceptedOutcome>,
    pub subscriber_failures: Vec<SubscriberFailure>,
}

impl CommandSubmission {
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(&self.output.result, CommandResult::Accepted { .. })
    }
}

/// The result of preparing a control submission without changing live state.
pub enum PrepareSubmitOutcome<'a, A = Policy> {
    /// The idempotency key already has a durable engine receipt or authorization denial.
    Replayed(CommandSubmission),
    /// A new engine receipt or authorization denial is staged for explicit commit.
    Prepared(PreparedSubmission<'a, A>),
}

impl<'a, A> PrepareSubmitOutcome<'a, A> {
    #[must_use]
    pub fn prepared(self) -> Option<PreparedSubmission<'a, A>> {
        match self {
            Self::Prepared(prepared) => Some(prepared),
            Self::Replayed(_) => None,
        }
    }

    #[must_use]
    pub fn replayed(self) -> Option<CommandSubmission> {
        match self {
            Self::Prepared(_) => None,
            Self::Replayed(submission) => Some(submission),
        }
    }
}

enum PreparedSubmissionKind {
    Engine {
        execution: Box<PreparedEngineExecution>,
        /// The command the engine executes: for undo and redo, the stored
        /// project edit they resolve to rather than the history command.
        command: EngineCommand,
        /// The wire payload that replays this acceptance on a fresh engine:
        /// the resolved edit for undo and redo, which a restored engine with
        /// empty history could not resolve again.
        replay_payload: Option<CommandPayload>,
    },
    AuthorizationDenial {
        /// False when the key already owns a durable engine receipt, which a
        /// cached denial must never shadow.
        remember: bool,
    },
}

/// An isolated submission that exclusively borrows its control authority.
///
/// Dropping this value aborts the submission without changing the service.
pub struct PreparedSubmission<'a, A = Policy> {
    control: &'a mut ControlService<A>,
    idempotency_key: IdempotencyKey,
    submission: CommandSubmission,
    kind: PreparedSubmissionKind,
}

impl<A: AuthorizationHook> PreparedSubmission<'_, A> {
    /// Returns the prospective metadata. Subscriber failures remain empty until commit.
    #[must_use]
    pub const fn submission(&self) -> &CommandSubmission {
        &self.submission
    }

    /// Returns the prospective command result and durable protocol events.
    #[must_use]
    pub const fn output(&self) -> &ResultBeforeEvents {
        &self.submission.output
    }

    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the command a durable journal must record so that replaying
    /// it against a freshly restored engine reproduces this acceptance.
    ///
    /// This is `message` itself except for an accepted undo or redo: undo
    /// history is session state and a restored engine starts with none, so
    /// the journal records the exact project edit the history command
    /// resolved to, under the same id, idempotency key, expected revision,
    /// and deadline. Replaying it lands on the same revision, events, and
    /// receipt; only the restored session's history differs, which starts
    /// from the replayed edits instead of the pre-crash stack.
    #[must_use]
    pub fn replay_command(&self, message: &CommandMessage) -> CommandMessage {
        match &self.kind {
            PreparedSubmissionKind::Engine {
                replay_payload: Some(payload),
                ..
            } if self.submission.is_accepted() => CommandMessage {
                payload: payload.clone(),
                ..message.clone()
            },
            PreparedSubmissionKind::Engine { .. }
            | PreparedSubmissionKind::AuthorizationDenial { .. } => message.clone(),
        }
    }

    /// Projects the post-commit engine by an exact number of deterministic ticks.
    ///
    /// # Errors
    ///
    /// Returns an engine tick error, or a snapshot error when the requested tick
    /// count does not reach an idle frame boundary.
    pub fn project(&self, ticks: u64) -> Result<EngineSnapshot, ControlError> {
        match &self.kind {
            PreparedSubmissionKind::Engine { execution, .. } => {
                execution.project(ticks).map_err(ControlError::from)
            }
            PreparedSubmissionKind::AuthorizationDenial { .. } => {
                project_engine(&self.control.engine, ticks).map_err(ControlError::from)
            }
        }
    }

    /// Returns `ticks` extended until no scene is mid In or Out phase
    /// (GX-011 stage 5): the frames a command must realize so the scenes it
    /// takes to Program or Preview (or off Program) finish their phases, as
    /// a title take realizes its whole In phase. A staged engine without
    /// Scene inputs, or one that cannot tick, needs exactly `ticks`.
    #[must_use]
    pub fn scene_settle_ticks(&self, ticks: u64) -> u64 {
        let engine = match &self.kind {
            PreparedSubmissionKind::Engine { execution, .. } => execution.staged_engine(),
            PreparedSubmissionKind::AuthorizationDenial { .. } => &self.control.engine,
        };
        if engine.show().scene_inputs().is_empty() {
            return ticks;
        }
        let mut projected = engine.clone();
        for _ in 0..ticks {
            if projected.tick().is_err() {
                return ticks;
            }
        }
        let mut settled = ticks;
        while projected.scenes_in_motion() {
            if projected.tick().is_err() {
                break;
            }
            settled += 1;
        }
        settled
    }

    /// Returns the number of frames needed for the staged overlay channel to
    /// reach its configured take/off target.
    #[must_use]
    pub fn overlay_transition_ticks(&self, channel: OverlayChannelId) -> u32 {
        let engine = match &self.kind {
            PreparedSubmissionKind::Engine { execution, .. } => execution.staged_engine(),
            PreparedSubmissionKind::AuthorizationDenial { .. } => &self.control.engine,
        };
        let overlay = engine.show().desired_switcher().overlay(channel);
        match overlay.transition() {
            SwitcherOverlayTransition::Cut => 1,
            SwitcherOverlayTransition::Fade => overlay.duration_frames(),
        }
    }

    /// Returns the number of frames needed to take title `input` off air:
    /// its Out phase when it has one (the channel is held that long, then
    /// cut), otherwise the staged transition of the overlay channel that
    /// showed it before this command, or one frame when no channel showed it
    /// (a refusal).
    #[must_use]
    pub fn title_off_air_ticks(&self, input: InputId) -> u32 {
        let channel = self
            .control
            .engine
            .show()
            .desired_switcher()
            .overlay_channels_showing(input)
            .next();
        let Some(channel) = channel else {
            return 1;
        };
        match self.control.engine.title_phase_frames(input, Phase::Out) {
            0 => self.overlay_transition_ticks(channel),
            frames => u32::try_from(frames).unwrap_or(u32::MAX),
        }
    }

    /// Returns the number of frames needed to take title `input` to air on
    /// `channel` and settle: its In phase when it has one (the channel cuts
    /// in and the title animates), otherwise the staged transition of
    /// `channel`.
    #[must_use]
    pub fn title_take_ticks(&self, input: InputId, channel: OverlayChannelId) -> u32 {
        match self.control.engine.title_phase_frames(input, Phase::In) {
            0 => self.overlay_transition_ticks(channel),
            frames => u32::try_from(frames).unwrap_or(u32::MAX),
        }
    }

    /// Returns the number of frames needed to realize a playlist command:
    /// up to and including its target frame (a playlist advance may target
    /// a later frame), plus the rest of any Program transition it started.
    /// A refusal needs one frame.
    #[must_use]
    pub fn playlist_ticks(&self) -> u64 {
        let Some(accepted) = &self.submission.accepted else {
            return 1;
        };
        let Some(acceptance) = accepted.receipt.accepted() else {
            return 1;
        };
        let cursor = self.control.engine.frame_cursor().get();
        let target = acceptance.result.target_frame.get();
        let transition = accepted
            .events
            .iter()
            .find_map(|event| match &event.payload {
                EngineEvent::PlaylistChanged { cue: Some(cue), .. } => {
                    Some(cue.transition.duration_ms)
                }
                _ => None,
            })
            .unwrap_or(0);
        target.saturating_sub(cursor) + transition.max(1)
    }

    /// Atomically installs the staged decision and then applies control side effects.
    ///
    /// # Errors
    ///
    /// Returns an engine infrastructure error. The exclusive service borrow makes
    /// stale engine preparation impossible through the safe control API.
    pub fn commit(self) -> Result<CommandSubmission, ControlError> {
        let Self {
            control,
            idempotency_key,
            mut submission,
            kind,
        } = self;

        match kind {
            PreparedSubmissionKind::Engine {
                execution, command, ..
            } => {
                let _ = control.engine.commit_execute(*execution)?;

                if submission.accepted.is_some() {
                    for event in &submission.output.events {
                        control.log.push_back(event.clone());
                    }
                    while control.log.len() > control.limits.retained_events {
                        control.log.pop_front();
                    }
                    control
                        .pending_runtime_actions
                        .push_back(PendingRuntimeAction {
                            revision: control.engine.revision().get(),
                            command,
                        });
                }
                control.snapshot = snapshot_record(&control.engine, &control.identity);

                let live_events = submission
                    .output
                    .events
                    .iter()
                    .cloned()
                    .map(LiveEvent::Durable)
                    .collect::<Vec<_>>();
                submission.subscriber_failures = control.publish(&live_events);
            }
            PreparedSubmissionKind::AuthorizationDenial { remember } => {
                if remember {
                    control.remember_authorization_denial(
                        idempotency_key,
                        submission.output.result.clone(),
                    );
                }
            }
        }

        Ok(submission)
    }

    /// Consumes this preparation without changing the service.
    pub fn abort(self) {}
}

/// Output of exactly one simulated engine tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickOutcome {
    pub frame: FrameResult,
    pub runtime_events: Vec<RuntimeEventMessage>,
    pub subscriber_failures: Vec<SubscriberFailure>,
}

/// Failure from ticking the control engine or realizing its rendered frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickWithRealizerError<E> {
    /// The control tick failed before or after frame realization.
    Tick(ControlError),
    /// The frame realization callback failed after the engine advanced.
    Realization(E),
}

impl<E> TickWithRealizerError<E> {
    #[must_use]
    pub const fn tick_error(&self) -> Option<&ControlError> {
        match self {
            Self::Tick(error) => Some(error),
            Self::Realization(_) => None,
        }
    }

    #[must_use]
    pub const fn realization_error(&self) -> Option<&E> {
        match self {
            Self::Tick(_) => None,
            Self::Realization(error) => Some(error),
        }
    }

    #[must_use]
    pub fn into_tick_error(self) -> Option<ControlError> {
        match self {
            Self::Tick(error) => Some(error),
            Self::Realization(_) => None,
        }
    }

    #[must_use]
    pub fn into_realization_error(self) -> Option<E> {
        match self {
            Self::Tick(_) => None,
            Self::Realization(error) => Some(error),
        }
    }
}

impl<E: fmt::Display> fmt::Display for TickWithRealizerError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tick(error) => write!(formatter, "control tick failed: {error}"),
            Self::Realization(error) => write!(formatter, "frame realization failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for TickWithRealizerError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Tick(error) => Some(error),
            Self::Realization(error) => Some(error),
        }
    }
}

/// A live subscription item with an explicit durability class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveEvent {
    Durable(EventMessage),
    Runtime(RuntimeEventMessage),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubscriberId(u64);

impl SubscriberId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriberFailureReason {
    SlowClient,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriberFailure {
    pub subscriber: SubscriberId,
    pub reason: SubscriberFailureReason,
}

/// The receiving half of a bounded live-event subscription.
#[derive(Debug)]
pub struct Subscription {
    id: SubscriberId,
    receiver: Receiver<LiveEvent>,
    failure: Arc<Mutex<Option<SubscriberFailureReason>>>,
}

impl Subscription {
    #[must_use]
    pub const fn id(&self) -> SubscriberId {
        self.id
    }

    /// Receives a queued event without blocking.
    ///
    /// # Errors
    ///
    /// Returns `Empty` when no event is ready or `Disconnected` after removal.
    pub fn try_recv(&self) -> Result<LiveEvent, TryRecvError> {
        self.receiver.try_recv()
    }

    #[must_use]
    pub fn failure(&self) -> Option<SubscriberFailureReason> {
        match self.failure.lock() {
            Ok(failure) => *failure,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscribeError {
    LimitReached,
    IdentifierExhausted,
}

impl fmt::Display for SubscribeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LimitReached => "subscriber limit reached",
            Self::IdentifierExhausted => "subscriber identifier space exhausted",
        })
    }
}

impl Error for SubscribeError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    Engine(EngineError),
    ServerIdentityMismatch,
    RuntimeGenerationOutOfOrder,
    RuntimeActionMismatch,
    RuntimeSequenceExhausted,
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(error) => error.fmt(formatter),
            Self::ServerIdentityMismatch => {
                formatter.write_str("runtime server identity does not match the control engine")
            }
            Self::RuntimeGenerationOutOfOrder => {
                formatter.write_str("runtime generation did not advance monotonically")
            }
            Self::RuntimeActionMismatch => {
                formatter.write_str("runtime generation advanced without a pending action")
            }
            Self::RuntimeSequenceExhausted => {
                formatter.write_str("runtime event sequence exhausted")
            }
        }
    }
}

impl Error for ControlError {}

impl From<EngineError> for ControlError {
    fn from(value: EngineError) -> Self {
        Self::Engine(value)
    }
}

/// Capability-neutral operational state for diagnostic adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlDiagnostics {
    pub engine: EngineIdentity,
    pub current_revision: u64,
    pub oldest_retained_revision: Option<u64>,
    pub newest_retained_revision: Option<u64>,
    pub subscriber_count: usize,
    pub limits: ControlLimits,
}

struct Subscriber {
    sender: SyncSender<LiveEvent>,
    failure: Arc<Mutex<Option<SubscriberFailureReason>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRuntimeAction {
    revision: u64,
    command: EngineCommand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveTransition {
    revision: u64,
    generation: u64,
}

/// Single-writer Phase 1 control service.
pub struct ControlService<A = Policy> {
    engine: Engine,
    authorizer: A,
    identity: EngineIdentity,
    limits: ControlLimits,
    authorization_denials: HashMap<IdempotencyKey, CommandResult>,
    authorization_denial_order: VecDeque<IdempotencyKey>,
    log: VecDeque<EventMessage>,
    snapshot: SnapshotRecord,
    subscribers: HashMap<SubscriberId, Subscriber>,
    next_subscriber_id: u64,
    pending_runtime_actions: VecDeque<PendingRuntimeAction>,
    active_transition: Option<ActiveTransition>,
    active_fade_to_black: Option<ActiveTransition>,
    runtime_sequence_generation: u64,
    runtime_sequence: u64,
}

impl<A: AuthorizationHook> ControlService<A> {
    #[must_use]
    pub fn new(
        mut engine: Engine,
        authorizer: A,
        engine_id: impl Into<String>,
        log_id: impl Into<String>,
        limits: ControlLimits,
    ) -> Self {
        // Every animation an edit leaves must be carried by its event and
        // by later snapshots' `title_animation_state` records.
        engine.set_title_clip_bound(title_animation::title_clip_fits_wire);
        engine.set_scene_clip_bound(scene_animation::scene_clip_fits_wire);
        let runtime_sequence_generation = engine.runtime_generation().get();
        let identity = EngineIdentity {
            engine_id: engine_id.into(),
            state_epoch: engine.state_epoch().get(),
            log_id: log_id.into(),
        };
        let snapshot = snapshot_record(&engine, &identity);
        Self {
            engine,
            authorizer,
            identity,
            limits,
            authorization_denials: HashMap::new(),
            authorization_denial_order: VecDeque::with_capacity(limits.retained_events),
            log: VecDeque::with_capacity(limits.retained_events),
            snapshot,
            subscribers: HashMap::new(),
            next_subscriber_id: 1,
            pending_runtime_actions: VecDeque::new(),
            active_transition: None,
            active_fade_to_black: None,
            runtime_sequence_generation,
            runtime_sequence: 0,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn snapshot(&self) -> &SnapshotRecord {
        &self.snapshot
    }

    /// Returns one `title_state` per title in [`Self::snapshot`]'s title
    /// order, at the current revision.
    ///
    /// A session writes these immediately after a snapshot, before any event,
    /// so a client completes every title summary the snapshot announced.
    #[must_use]
    pub fn title_states(&self) -> Vec<TitleStateMessage> {
        title_states(&self.engine)
    }

    /// Compiles the engine's current per-input and program grades.
    ///
    /// # Errors
    ///
    /// Returns [`fm_engine::GradeCompileError`] for a grade outside its
    /// bounds, which accepted edits never produce.
    pub fn compiled_grades(
        &self,
    ) -> Result<fm_engine::CompiledProjectGrades, fm_engine::GradeCompileError> {
        self.engine.show().compiled_grades()
    }

    /// Returns the `grade_state` records a session writes after
    /// [`Self::snapshot`]'s title states and playlist state, exactly as many
    /// as the snapshot announces.
    #[must_use]
    pub fn grade_states(&self) -> Vec<GradeStateMessage> {
        grade::grade_states(&self.engine)
    }

    /// Returns the `animation_preset_state` records a session writes after
    /// [`Self::scene_layout_states`], last of a snapshot's follow-on
    /// records: exactly as many as the snapshot announces, in library order
    /// (Protocol 2.34).
    #[must_use]
    pub fn animation_preset_states(&self) -> Vec<fm_protocol::AnimationPresetStateMessage> {
        animation_preset::animation_preset_states(&self.engine)
    }

    /// The template of title input `input`, when it was restored with one.
    #[must_use]
    pub fn title_template(&self, input: InputId) -> Option<&fm_engine::EngineTitleTemplate> {
        self.engine.show().title(input)?.template.as_ref()
    }

    /// Every distinct title template the engine holds (one per shared key,
    /// the stored template URI), as renderers must draw them now.
    #[must_use]
    pub fn title_templates(&self) -> Vec<fm_engine::EngineTitleTemplate> {
        let mut templates: Vec<fm_engine::EngineTitleTemplate> = Vec::new();
        for title in self.engine.show().titles().values() {
            if let Some(template) = &title.template
                && !templates.iter().any(|known| known.key == template.key)
            {
                templates.push(template.clone());
            }
        }
        templates
    }

    /// Returns the `title_animation_state` records a session writes after
    /// [`Self::grade_states`]: one per title [`Self::snapshot`] announces, in
    /// the same order.
    #[must_use]
    pub fn title_animation_states(&self) -> Vec<TitleAnimationStateMessage> {
        title_animation::title_animation_states(&self.engine)
    }

    /// Returns the `title_layout_state` records a session writes after
    /// [`Self::title_animation_states`]: one per title [`Self::snapshot`]
    /// announces, in the same order (Protocol 2.30).
    #[must_use]
    pub fn title_layout_states(&self) -> Vec<fm_protocol::TitleLayoutStateMessage> {
        title_layout::title_layout_states(&self.engine)
    }

    /// Returns the `scene_animation_state` records a session writes after
    /// [`Self::title_layout_states`]: one per Scene input [`Self::snapshot`]
    /// announces in its `scenes` list, in the same order (Protocol 2.31).
    #[must_use]
    pub fn scene_animation_states(&self) -> Vec<fm_protocol::SceneAnimationStateMessage> {
        scene_animation::scene_animation_states(&self.engine)
    }

    /// Returns the `scene_layout_state` records a session writes after
    /// [`Self::scene_animation_states`]: one per announced Scene input, in
    /// the same order (Protocol 2.31).
    #[must_use]
    pub fn scene_layout_states(&self) -> Vec<fm_protocol::SceneLayoutStateMessage> {
        scene_animation::scene_layout_states(&self.engine)
    }

    /// The scene Scene input `input` shows, when it is one.
    #[must_use]
    pub fn scene_input(&self, input: InputId) -> Option<&fm_engine::EngineScene> {
        self.engine.show().scene_input(input)
    }

    /// Every Scene input whose scene is mid In or Out phase (or holding the
    /// end of an Out phase) at the frame cursor, with its pose.
    #[must_use]
    pub fn scenes_on_air(&self) -> Vec<fm_engine::EngineSceneAir> {
        self.engine.scenes_on_air()
    }

    /// The pose Scene input `input` shows at the frame cursor.
    #[must_use]
    pub fn scene_pose(&self, input: InputId) -> fm_model::fm_animation::ClipState {
        self.engine.scene_pose(input)
    }

    /// Whether some scene is still mid In or Out phase at the frame cursor.
    #[must_use]
    pub fn scenes_in_motion(&self) -> bool {
        self.engine.scenes_in_motion()
    }

    /// Returns the `playlist_state` a session writes after [`Self::snapshot`]
    /// (and its title states) when the snapshot announces one
    /// ([`SnapshotMessage::playlist_follows`]), at the current revision and
    /// frame cursor.
    #[must_use]
    pub fn playlist_state(&self) -> Option<PlaylistStateMessage> {
        playlist::playlist_state(&self.engine)
    }

    /// Every title a title take put on an overlay channel, as the frame
    /// cursor shows it absent new commands: its channel, phase, and pose.
    #[must_use]
    pub fn titles_on_air(&self) -> Vec<fm_engine::EngineTitleAir> {
        self.engine.titles_on_air()
    }

    /// Returns the engine's next unrendered frame number (its frame cursor).
    #[must_use]
    pub const fn frame_cursor(&self) -> u64 {
        self.engine.frame_cursor().get()
    }

    /// Returns the engine's next deterministic frame deadline without ticking.
    ///
    /// # Errors
    ///
    /// Returns an engine pacing error when the next deadline is not representable.
    pub fn next_frame_deadline(&self) -> Result<fm_clock::ClockTime, ControlError> {
        self.engine.next_frame_deadline().map_err(Into::into)
    }

    /// Returns the only frame a `playlist_advance` is accepted for, or
    /// `None` while no playlist boundary is pending.
    #[must_use]
    pub fn playlist_advance_frame(&self) -> Option<u64> {
        self.engine.playlist_advance_frame()
    }

    /// Projects the next deterministic frame without advancing authority state.
    ///
    /// This is intended for bounded media preparation that must complete before
    /// the authoritative tick is realized.
    ///
    /// # Errors
    ///
    /// Returns an engine tick error when the projected frame cannot advance.
    pub fn project_next_frame(&self) -> Result<FrameResult, ControlError> {
        self.engine.clone().tick().map_err(Into::into)
    }

    /// Captures the current engine only when it is at an idle frame boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::WorkInFlight`] while an engine action or
    /// transition is pending, including an unpublished control realization.
    pub fn idle_engine_snapshot(&self) -> Result<EngineSnapshot, SnapshotError> {
        if !self.pending_runtime_actions.is_empty()
            || self.active_transition.is_some()
            || self.active_fade_to_black.is_some()
        {
            return Err(SnapshotError::WorkInFlight);
        }
        self.engine.snapshot()
    }

    #[must_use]
    pub fn diagnostics(&self) -> ControlDiagnostics {
        ControlDiagnostics {
            engine: self.identity.clone(),
            current_revision: self.engine.revision().get(),
            oldest_retained_revision: self.log.front().map(|event| event.cursor.revision),
            newest_retained_revision: self.log.back().map(|event| event.cursor.revision),
            subscriber_count: self.subscribers.len(),
            limits: self.limits,
        }
    }

    /// Creates a bounded live-event subscription.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured subscriber limit or identifier
    /// space is exhausted.
    pub fn subscribe(&mut self) -> Result<Subscription, SubscribeError> {
        if self.subscribers.len() >= self.limits.max_subscribers {
            return Err(SubscribeError::LimitReached);
        }
        let id = SubscriberId(self.next_subscriber_id);
        self.next_subscriber_id = self
            .next_subscriber_id
            .checked_add(1)
            .ok_or(SubscribeError::IdentifierExhausted)?;
        let (sender, receiver) = sync_channel(self.limits.subscriber_queue);
        let failure = Arc::new(Mutex::new(None));
        self.subscribers.insert(
            id,
            Subscriber {
                sender,
                failure: Arc::clone(&failure),
            },
        );
        Ok(Subscription {
            id,
            receiver,
            failure,
        })
    }

    /// Removes a live subscription and returns whether it was present.
    pub fn unsubscribe(&mut self, subscriber: SubscriberId) -> bool {
        self.subscribers.remove(&subscriber).is_some()
    }

    /// Returns retained events only when identity and every following revision match.
    #[must_use]
    pub fn resume(&self, cursor: &EventCursor) -> ResumeDecision {
        let current_revision = self.engine.revision().get();
        if cursor.engine != self.identity || cursor.revision > current_revision {
            return ResumeDecision::Snapshot(Box::new(self.snapshot.clone()));
        }
        if cursor.revision == current_revision {
            return ResumeDecision::Events(Vec::new());
        }

        let events: Vec<_> = self
            .log
            .iter()
            .filter(|event| event.cursor.revision > cursor.revision)
            .cloned()
            .collect();
        let mut expected = cursor.revision.checked_add(1);
        let contiguous = !events.is_empty()
            && events.iter().all(|event| {
                let matches = expected == Some(event.cursor.revision);
                expected = event.cursor.revision.checked_add(1);
                matches
            })
            && events.last().map(|event| event.cursor.revision) == Some(current_revision);
        if contiguous {
            ResumeDecision::Events(events)
        } else {
            ResumeDecision::Snapshot(Box::new(self.snapshot.clone()))
        }
    }

    /// Returns every command class a principal must hold for `payload` to be
    /// authorized against the current engine state, exactly as
    /// [`Self::prepare_submit`] authorizes it.
    ///
    /// A transport session that scopes a connection to a narrower role than
    /// the principal executing in this service must authorize these classes,
    /// not only the payload's own class: undo and redo resolve to the edit at
    /// the top of the history stack, which only the engine knows.
    #[must_use]
    pub fn required_classes(&self, payload: &CommandPayload) -> RequiredClasses {
        RequiredClasses::resolve(payload, history_target(&self.engine, payload).as_ref())
    }

    /// Prepares an authorized protocol command without changing live control state.
    ///
    /// A cached target-free authorization denial is replayed first. Otherwise,
    /// authorization sees only the target-free command class and runs before
    /// conversion and detailed validation. Engine execution remains the
    /// authority for other idempotency, deadline, revision, and domain behavior.
    ///
    /// # Errors
    ///
    /// Returns an engine infrastructure error. Command rejections and authorization
    /// denials are staged as normal [`CommandResult::Rejected`] values.
    pub fn prepare_submit<'a>(
        &'a mut self,
        principal: &Principal,
        message: CommandMessage,
        now_millis: u64,
    ) -> Result<PrepareSubmitOutcome<'a, A>, ControlError> {
        let idempotency_key = IdempotencyKey::new(message.idempotency_key.clone());
        if let Some(result) = self.authorization_denials.get(&idempotency_key) {
            return Ok(PrepareSubmitOutcome::Replayed(CommandSubmission {
                output: ResultBeforeEvents {
                    result: result.clone(),
                    events: Vec::new(),
                },
                replayed: true,
                accepted: None,
                subscriber_failures: Vec::new(),
            }));
        }

        let history_target = history_target(&self.engine, &message.payload);
        let authorization = RequiredClasses::resolve(&message.payload, history_target.as_ref())
            .authorize(&self.authorizer, principal);
        if let Err(denial) = authorization {
            // A cached denial must never shadow a durable engine receipt.
            let remember = self.engine.receipt(&idempotency_key).is_none();
            let result = CommandResult::Rejected {
                id: message.id,
                code: RejectionCode::PermissionDenied.as_str().to_owned(),
                message: denial.to_string(),
                fields: Vec::new(),
                current_revision: self.engine.revision().get(),
                retryable: false,
            };
            return Ok(PrepareSubmitOutcome::Prepared(PreparedSubmission {
                control: self,
                idempotency_key,
                kind: PreparedSubmissionKind::AuthorizationDenial { remember },
                submission: CommandSubmission {
                    output: ResultBeforeEvents {
                        result,
                        events: Vec::new(),
                    },
                    replayed: false,
                    accepted: None,
                    subscriber_failures: Vec::new(),
                },
            }));
        }

        let command = engine_command(&message.payload);
        let replay_payload = history_target.as_ref().and_then(wire_edit_payload);
        let executed = history_target.unwrap_or_else(|| command.clone());
        match self
            .engine
            .prepare_execute(message.domain_envelope(command), now_millis)?
        {
            EnginePrepareOutcome::Replayed(outcome) => Ok(PrepareSubmitOutcome::Replayed(
                engine_submission(&self.identity, idempotency_key, &self.engine, &outcome),
            )),
            EnginePrepareOutcome::Prepared(execution) => {
                let submission = engine_submission(
                    &self.identity,
                    idempotency_key.clone(),
                    execution.staged_engine(),
                    execution.outcome(),
                );
                Ok(PrepareSubmitOutcome::Prepared(PreparedSubmission {
                    control: self,
                    idempotency_key,
                    submission,
                    kind: PreparedSubmissionKind::Engine {
                        execution,
                        command: executed,
                        replay_payload,
                    },
                }))
            }
        }
    }

    /// Authorizes, prepares, and immediately commits a protocol command.
    ///
    /// # Errors
    ///
    /// Returns an engine infrastructure error. Command rejections are normal
    /// [`CommandResult::Rejected`] values.
    pub fn submit(
        &mut self,
        principal: &Principal,
        message: CommandMessage,
        now_millis: u64,
    ) -> Result<CommandSubmission, ControlError> {
        match self.prepare_submit(principal, message, now_millis)? {
            PrepareSubmitOutcome::Replayed(submission) => Ok(submission),
            PrepareSubmitOutcome::Prepared(prepared) => prepared.commit(),
        }
    }

    fn remember_authorization_denial(
        &mut self,
        idempotency_key: IdempotencyKey,
        result: CommandResult,
    ) {
        if self.limits.retained_events == 0 {
            return;
        }
        while self.authorization_denials.len() >= self.limits.retained_events {
            let Some(expired) = self.authorization_denial_order.pop_front() else {
                break;
            };
            self.authorization_denials.remove(&expired);
        }
        self.authorization_denial_order
            .push_back(idempotency_key.clone());
        self.authorization_denials.insert(idempotency_key, result);
    }

    /// Advances exactly one frame at the engine's deterministic simulated deadline.
    ///
    /// # Errors
    ///
    /// Returns an engine tick error without advancing an additional frame.
    pub fn tick(&mut self, server: &ServerIdentity) -> Result<TickOutcome, ControlError> {
        match self.tick_with_realizer(server, |_| Ok::<(), Infallible>(())) {
            Ok(outcome) => Ok(outcome),
            Err(TickWithRealizerError::Tick(error)) => Err(error),
            Err(TickWithRealizerError::Realization(error)) => match error {},
        }
    }

    /// Advances one shutdown-only frame without publishing a runtime-realized
    /// event for media that was deliberately not rendered.
    ///
    /// This exists only to settle already accepted work to an idle checkpoint
    /// after the process has committed to exit.
    ///
    /// # Errors
    ///
    /// Returns an engine tick error without advancing an additional frame.
    pub fn tick_for_shutdown(
        &mut self,
        server: &ServerIdentity,
    ) -> Result<TickOutcome, ControlError> {
        match self.tick_with_realizer_inner(server, |_| Ok::<(), Infallible>(()), false) {
            Ok(outcome) => Ok(outcome),
            Err(TickWithRealizerError::Tick(error)) => Err(error),
            Err(TickWithRealizerError::Realization(error)) => match error {},
        }
    }

    /// Advances one engine frame, realizes it, then publishes its control effects.
    ///
    /// The realizer receives the exact [`FrameResult`] produced by the engine.
    /// Pending runtime actions, realized lifecycle events, snapshots, and live
    /// subscribers are updated only after the callback succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`TickWithRealizerError::Tick`] for a control tick failure. If the
    /// callback fails, returns [`TickWithRealizerError::Realization`] without
    /// publishing or changing control-side realization state. The engine has
    /// already advanced in that case, so the failed frame cannot be retried;
    /// callers must treat realization failure as fatal to the tick loop.
    pub fn tick_with_realizer<E>(
        &mut self,
        server: &ServerIdentity,
        realizer: impl FnOnce(&FrameResult) -> Result<(), E>,
    ) -> Result<TickOutcome, TickWithRealizerError<E>> {
        self.tick_with_realizer_inner(server, realizer, true)
    }

    fn tick_with_realizer_inner<E>(
        &mut self,
        server: &ServerIdentity,
        realizer: impl FnOnce(&FrameResult) -> Result<(), E>,
        publish_runtime_realization: bool,
    ) -> Result<TickOutcome, TickWithRealizerError<E>> {
        if server.engine_id != self.identity.engine_id
            || server.state_epoch != self.identity.state_epoch
            || server.log_id != self.identity.log_id
        {
            return Err(TickWithRealizerError::Tick(
                ControlError::ServerIdentityMismatch,
            ));
        }

        let previous_generation = self.engine.runtime_generation().get();
        let frame = self
            .engine
            .tick()
            .map_err(ControlError::from)
            .map_err(TickWithRealizerError::Tick)?;
        realizer(&frame).map_err(TickWithRealizerError::Realization)?;
        let current_generation = frame.runtime_generation.get();
        let applied_actions = current_generation
            .checked_sub(previous_generation)
            .ok_or(ControlError::RuntimeGenerationOutOfOrder)
            .map_err(TickWithRealizerError::Tick)?;
        let mut runtime_events = Vec::new();
        for offset in 1..=applied_actions {
            let pending = self
                .pending_runtime_actions
                .pop_front()
                .ok_or(ControlError::RuntimeActionMismatch)
                .map_err(TickWithRealizerError::Tick)?;
            let generation = previous_generation
                .checked_add(offset)
                .ok_or(ControlError::RuntimeGenerationOutOfOrder)
                .map_err(TickWithRealizerError::Tick)?;
            if is_program_transition(&pending.command) {
                self.active_transition = Some(ActiveTransition {
                    revision: pending.revision,
                    generation,
                });
            } else if matches!(&pending.command, EngineCommand::FadeToBlack { .. }) {
                if let Some(superseded) = self.active_fade_to_black.replace(ActiveTransition {
                    revision: pending.revision,
                    generation,
                }) && publish_runtime_realization
                {
                    runtime_events.push(
                        self.runtime_superseded(
                            server,
                            superseded.revision,
                            generation,
                            pending.revision,
                        )
                        .map_err(TickWithRealizerError::Tick)?,
                    );
                }
            } else if publish_runtime_realization {
                runtime_events.push(
                    self.runtime_realized(
                        server,
                        pending.revision,
                        generation,
                        runtime_domain(&pending.command),
                    )
                    .map_err(TickWithRealizerError::Tick)?,
                );
            }
        }

        let completions = self.take_completed_switcher_operations();
        if publish_runtime_realization {
            for operation in completions {
                runtime_events.push(
                    self.runtime_realized(
                        server,
                        operation.revision,
                        current_generation,
                        "switcher",
                    )
                    .map_err(TickWithRealizerError::Tick)?,
                );
            }
        }

        self.snapshot = snapshot_record(&self.engine, &self.identity);
        let live_events = runtime_events
            .iter()
            .cloned()
            .map(LiveEvent::Runtime)
            .collect::<Vec<_>>();
        let subscriber_failures = self.publish(&live_events);
        Ok(TickOutcome {
            frame,
            runtime_events,
            subscriber_failures,
        })
    }

    fn take_completed_switcher_operations(&mut self) -> Vec<ActiveTransition> {
        let transition = (self.engine.realized_switcher().transition().is_none()
            && self.engine.realized_switcher().program()
                == self.engine.show().desired_switcher().program()
            && self.engine.realized_switcher().preview()
                == self.engine.show().desired_switcher().preview())
        .then(|| self.active_transition.take())
        .flatten();
        let fade_to_black = (!self.engine.realized_switcher().fade_to_black_is_automatic()
            && self.engine.realized_fade_to_black() == self.engine.desired_fade_to_black())
        .then(|| self.active_fade_to_black.take())
        .flatten();
        let mut completed = [transition, fade_to_black]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        completed.sort_by_key(|operation| operation.generation);
        completed
    }

    fn runtime_realized(
        &mut self,
        server: &ServerIdentity,
        revision: u64,
        generation: u64,
        domain: &'static str,
    ) -> Result<RuntimeEventMessage, ControlError> {
        let event = RuntimeLifecycleEvent::Realized {
            domain: domain.to_owned(),
            manual_transition: protocol_manual_status(self.engine.realized_manual_transition()),
            fade_to_black: protocol_fade_to_black_state(self.engine.realized_fade_to_black()),
        };
        self.runtime_event(server, revision, generation, event)
    }

    fn runtime_superseded(
        &mut self,
        server: &ServerIdentity,
        revision: u64,
        generation: u64,
        by_revision: u64,
    ) -> Result<RuntimeEventMessage, ControlError> {
        self.runtime_event(
            server,
            revision,
            generation,
            RuntimeLifecycleEvent::Superseded { by_revision },
        )
    }

    fn runtime_event(
        &mut self,
        server: &ServerIdentity,
        revision: u64,
        generation: u64,
        event: RuntimeLifecycleEvent,
    ) -> Result<RuntimeEventMessage, ControlError> {
        if generation != self.runtime_sequence_generation {
            if generation < self.runtime_sequence_generation {
                return Err(ControlError::RuntimeGenerationOutOfOrder);
            }
            self.runtime_sequence_generation = generation;
            self.runtime_sequence = 0;
        }
        self.runtime_sequence = self
            .runtime_sequence
            .checked_add(1)
            .ok_or(ControlError::RuntimeSequenceExhausted)?;
        Ok(RuntimeEventMessage {
            server: server.clone(),
            revision,
            generation,
            sequence: self.runtime_sequence,
            event,
        })
    }

    fn publish(&mut self, events: &[LiveEvent]) -> Vec<SubscriberFailure> {
        let mut failures = Vec::new();
        for event in events {
            self.subscribers.retain(|id, subscriber| {
                let failure = match subscriber.sender.try_send(event.clone()) {
                    Ok(()) => return true,
                    Err(TrySendError::Full(_)) => SubscriberFailureReason::SlowClient,
                    Err(TrySendError::Disconnected(_)) => SubscriberFailureReason::Disconnected,
                };
                match subscriber.failure.lock() {
                    Ok(mut status) => *status = Some(failure),
                    Err(poisoned) => *poisoned.into_inner() = Some(failure),
                }
                failures.push(SubscriberFailure {
                    subscriber: *id,
                    reason: failure,
                });
                false
            });
        }
        failures
    }
}

const fn runtime_domain(command: &EngineCommand) -> &'static str {
    match command {
        EngineCommand::ReorderInputs { .. }
        | EngineCommand::RenameInput { .. }
        | EngineCommand::AddInput { .. }
        | EngineCommand::RemoveInput { .. }
        | EngineCommand::SetEditLock { .. }
        | EngineCommand::SetTitleFields { .. }
        | EngineCommand::SetPlaylist { .. }
        | EngineCommand::Grade(_)
        | EngineCommand::TitleAnimation(_)
        | EngineCommand::SceneAnimation(_)
        | EngineCommand::AnimationPreset(_) => "project",
        EngineCommand::SetInputAudioStrip { .. } => "audio",
        _ => "switcher",
    }
}

/// Returns the target-free authorization class of one wire command.
///
/// Adding and removing inputs change the durable catalogue, so they are
/// project edits. Undo and redo are project edits. The edit padlock is also
/// `EditProject`:
/// it is an operator guard against accidental edits, not an access-control
/// boundary, so anyone who may edit may also guard (or release) their edits.
/// Roles remain the access-control mechanism; a Viewer or an Operator without
/// `edit_project` can neither edit nor toggle the padlock.
///
/// Saving title fields edits project content, so it is `EditProject` (the
/// Graphics role); taking a title on or off air switches, so it is
/// `Transition`. A take that also carries field values additionally needs
/// `EditProject` ([`additional_command_class`]).
///
/// Grade edits (input, program, and library grades and LUT imports) change
/// the project's look like title content, so they are `EditProject` (the
/// Graphics role), never switching. Title animation edits change a title
/// template like a field save, so they are `EditProject` too.
const fn command_class(payload: &CommandPayload) -> CommandClass {
    match payload {
        CommandPayload::SetTitleAnimation { .. }
        | CommandPayload::ApplyTitleAnimationPreset { .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { .. }
        | CommandPayload::ApplySceneAnimationUserPreset { .. }
        | CommandPayload::AddAnimationPreset { .. }
        | CommandPayload::RenameAnimationPreset { .. }
        | CommandPayload::RemoveAnimationPreset { .. }
        | CommandPayload::StretchTitleAnimation { .. }
        | CommandPayload::SetSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationPreset { .. }
        | CommandPayload::StretchSceneAnimation { .. }
        | CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetLayerGrade { .. }
        | CommandPayload::SetProgramGrade { .. }
        | CommandPayload::AddLibraryGrade { .. }
        | CommandPayload::UpdateLibraryGrade { .. }
        | CommandPayload::RemoveLibraryGrade { .. }
        | CommandPayload::ImportGradeLut { .. }
        | CommandPayload::RemoveGradeLut { .. }
        | CommandPayload::ReorderInputs { .. }
        | CommandPayload::RenameInput { .. }
        | CommandPayload::AddInput { .. }
        | CommandPayload::RemoveInput { .. }
        | CommandPayload::Undo { .. }
        | CommandPayload::Redo { .. }
        | CommandPayload::SetEditLock { .. }
        | CommandPayload::SetTitleFields { .. }
        | CommandPayload::SetPlaylist { .. } => CommandClass::EditProject,
        CommandPayload::TakeTitle { .. } | CommandPayload::TitleOffAir { .. } => {
            CommandClass::Transition
        }
        CommandPayload::SetInputAudioStrip { .. } => CommandClass::ControlAudio,
        CommandPayload::SelectPreview { .. } => CommandClass::SelectPreview,
        CommandPayload::Cut
        | CommandPayload::Fade { .. }
        | CommandPayload::AlphaFade { .. }
        | CommandPayload::Slide { .. }
        | CommandPayload::Zoom { .. }
        | CommandPayload::Stinger { .. }
        | CommandPayload::ConfigureStinger { .. }
        | CommandPayload::RemoveStinger { .. }
        | CommandPayload::TakeOverlay { .. }
        | CommandPayload::UpdateOverlay { .. }
        | CommandPayload::OverlayOff { .. }
        | CommandPayload::SetOverlayOutputInclusion { .. }
        | CommandPayload::ConfigureOverlayTransition { .. }
        | CommandPayload::ConfigureOverlayAppearance { .. }
        | CommandPayload::QueueOverlay { .. }
        | CommandPayload::TakeNextOverlay { .. }
        | CommandPayload::Wipe { .. }
        | CommandPayload::FadeToBlack { .. }
        | CommandPayload::StartManualTransition { .. }
        | CommandPayload::SetManualTransitionPosition { .. }
        | CommandPayload::CommitManualTransition
        | CommandPayload::CancelManualTransition
        | CommandPayload::StreamStart { .. }
        | CommandPayload::StreamStop { .. }
        | CommandPayload::RecordStart { .. }
        | CommandPayload::RecordStop { .. }
        | CommandPayload::PlaylistControl { .. }
        | CommandPayload::PlaylistAdvance { .. } => CommandClass::Transition,
    }
}

/// Returns the second class a command needs beyond [`command_class`], other
/// than undo and redo (which need the class of the edit they resolve to).
///
/// A title take that carries field values saves content as it switches, so
/// it also needs `EditProject`; an empty take only switches.
const fn additional_command_class(payload: &CommandPayload) -> Option<CommandClass> {
    match payload {
        CommandPayload::TakeTitle { fields, .. } if !fields.is_empty() => {
            Some(CommandClass::EditProject)
        }
        _ => None,
    }
}

fn input_audio_strip_engine_command(payload: &CommandPayload) -> EngineCommand {
    let CommandPayload::SetInputAudioStrip {
        input,
        gain_millidb,
        balance_basis_points,
        muted,
        soloed,
        follow_video,
        delay_samples,
    } = payload
    else {
        unreachable!("only input audio strip commands are delegated")
    };
    EngineCommand::SetInputAudioStrip {
        input: input.to_domain(),
        state: EngineInputAudioStripState {
            gain_millidb: *gain_millidb,
            balance_basis_points: *balance_basis_points,
            muted: *muted,
            soloed: *soloed,
            follow_video: *follow_video,
            delay_samples: *delay_samples,
        },
    }
}

#[allow(clippy::too_many_lines)] // one arm per payload family
fn engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        payload @ (CommandPayload::ReorderInputs { .. }
        | CommandPayload::RenameInput { .. }
        | CommandPayload::AddInput { .. }
        | CommandPayload::RemoveInput { .. }) => input_catalogue_engine_command(payload),
        payload @ CommandPayload::SetInputAudioStrip { .. } => {
            input_audio_strip_engine_command(payload)
        }
        CommandPayload::SelectPreview { input } => EngineCommand::SelectPreview(input.to_domain()),
        CommandPayload::Cut => EngineCommand::Cut,
        CommandPayload::Fade { duration_frames } => EngineCommand::Fade {
            duration_frames: *duration_frames,
        },
        CommandPayload::AlphaFade { duration_frames } => EngineCommand::AlphaFade {
            duration_frames: *duration_frames,
        },
        CommandPayload::Slide { duration_frames } => EngineCommand::Slide {
            duration_frames: *duration_frames,
        },
        CommandPayload::Zoom { duration_frames } => EngineCommand::Zoom {
            duration_frames: *duration_frames,
        },
        payload @ (CommandPayload::Stinger { .. }
        | CommandPayload::ConfigureStinger { .. }
        | CommandPayload::RemoveStinger { .. }) => stinger_engine_command(payload),
        payload @ (CommandPayload::TakeOverlay { .. }
        | CommandPayload::UpdateOverlay { .. }
        | CommandPayload::OverlayOff { .. }
        | CommandPayload::SetOverlayOutputInclusion { .. }
        | CommandPayload::ConfigureOverlayTransition { .. }
        | CommandPayload::ConfigureOverlayAppearance { .. }
        | CommandPayload::QueueOverlay { .. }
        | CommandPayload::TakeNextOverlay { .. }) => overlay_engine_command(payload),
        CommandPayload::Wipe { duration_frames } => EngineCommand::Wipe {
            duration_frames: *duration_frames,
        },
        CommandPayload::FadeToBlack {
            active,
            duration_frames,
        } => EngineCommand::FadeToBlack {
            active: *active,
            duration_frames: *duration_frames,
        },
        CommandPayload::StartManualTransition { kind } => EngineCommand::StartManualTransition {
            kind: match kind {
                ManualTransitionKind::Fade => EngineManualTransitionKind::Fade,
                ManualTransitionKind::Wipe => EngineManualTransitionKind::Wipe,
                ManualTransitionKind::AlphaFade => EngineManualTransitionKind::AlphaFade,
                ManualTransitionKind::Slide => EngineManualTransitionKind::Slide,
            },
        },
        CommandPayload::SetManualTransitionPosition { position } => {
            EngineCommand::SetManualTransitionPosition {
                position: engine_manual_position(*position),
            }
        }
        CommandPayload::CommitManualTransition => EngineCommand::CommitManualTransition,
        CommandPayload::CancelManualTransition => EngineCommand::CancelManualTransition,
        CommandPayload::StreamStart { target } => EngineCommand::StreamStart {
            target: domain_stream_target(*target),
        },
        CommandPayload::StreamStop { target } => EngineCommand::StreamStop {
            target: domain_stream_target(*target),
        },
        CommandPayload::RecordStart { recorder } => EngineCommand::RecordStart {
            recorder: domain_recorder_slot(*recorder),
        },
        CommandPayload::RecordStop { recorder } => EngineCommand::RecordStop {
            recorder: domain_recorder_slot(*recorder),
        },
        payload @ (CommandPayload::Undo { .. }
        | CommandPayload::Redo { .. }
        | CommandPayload::SetEditLock { .. }) => edit_engine_command(payload),
        payload @ (CommandPayload::SetTitleFields { .. }
        | CommandPayload::TakeTitle { .. }
        | CommandPayload::TitleOffAir { .. }) => title_engine_command(payload),
        payload @ (CommandPayload::SetPlaylist { .. }
        | CommandPayload::PlaylistControl { .. }
        | CommandPayload::PlaylistAdvance { .. }) => playlist::playlist_engine_command(payload),
        payload @ (CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetLayerGrade { .. }
        | CommandPayload::SetProgramGrade { .. }
        | CommandPayload::AddLibraryGrade { .. }
        | CommandPayload::UpdateLibraryGrade { .. }
        | CommandPayload::RemoveLibraryGrade { .. }
        | CommandPayload::ImportGradeLut { .. }
        | CommandPayload::RemoveGradeLut { .. }) => grade::grade_engine_command(payload),
        payload @ (CommandPayload::SetTitleAnimation { .. }
        | CommandPayload::ApplyTitleAnimationPreset { .. }
        | CommandPayload::StretchTitleAnimation { .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { .. }) => {
            title_animation::title_animation_engine_command(payload)
        }
        payload @ (CommandPayload::SetSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationPreset { .. }
        | CommandPayload::StretchSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationUserPreset { .. }) => {
            scene_animation::scene_animation_engine_command(payload)
        }
        payload @ (CommandPayload::AddAnimationPreset { .. }
        | CommandPayload::RenameAnimationPreset { .. }
        | CommandPayload::RemoveAnimationPreset { .. }) => {
            animation_preset::animation_preset_engine_command(payload)
        }
    }
}

/// Maps the title commands: field save, take, and off-air.
fn title_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetTitleFields {
            input,
            base_revision,
            require,
            fields,
        } => EngineCommand::SetTitleFields {
            input: input.to_domain(),
            base_revision: *base_revision,
            require: match require {
                TitleAirRequirement::OffAir => EngineTitleAirRequirement::OffAir,
                TitleAirRequirement::OnAir => EngineTitleAirRequirement::OnAir,
            },
            fields: domain_title_fields(fields),
        },
        CommandPayload::TakeTitle {
            input,
            channel,
            base_revision,
            fields,
        } => EngineCommand::TakeTitle {
            input: input.to_domain(),
            channel: domain_overlay_channel(*channel),
            base_revision: *base_revision,
            fields: domain_title_fields(fields),
        },
        CommandPayload::TitleOffAir { input } => EngineCommand::TitleOffAir {
            input: input.to_domain(),
        },
        _ => unreachable!("only title commands are delegated"),
    }
}

fn domain_title_fields(fields: &[TitleFieldUpdate]) -> Vec<(FieldId, FieldValue)> {
    fields
        .iter()
        .map(|update| {
            let value = match &update.value {
                TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
                TitleFieldValue::Number(number) => FieldValue::Number(*number),
                TitleFieldValue::Color {
                    red,
                    green,
                    blue,
                    alpha,
                } => FieldValue::Color(TitleColor::new(*red, *green, *blue, *alpha)),
                TitleFieldValue::Image { asset_uri } => {
                    FieldValue::Image(ImageSource::new(asset_uri.clone()))
                }
            };
            (FieldId::new(update.field.get()), value)
        })
        .collect()
}

fn wire_title_value(value: &FieldValue) -> TitleFieldValue {
    match value {
        FieldValue::Text(text) => TitleFieldValue::Text(text.clone()),
        FieldValue::Number(number) => TitleFieldValue::Number(*number),
        FieldValue::Color(color) => TitleFieldValue::Color {
            red: color.r,
            green: color.g,
            blue: color.b,
            alpha: color.a,
        },
        FieldValue::Image(image) => TitleFieldValue::Image {
            asset_uri: image.asset.clone(),
        },
    }
}

/// A title's complete values in strictly ascending field order.
fn wire_title_fields(title: &EngineTitle) -> Vec<TitleFieldUpdate> {
    title
        .fields
        .values()
        .iter()
        .map(|(field, value)| TitleFieldUpdate {
            field: WireTitleFieldId::new(field.get()),
            value: wire_title_value(value),
        })
        .collect()
}

/// The `title_changed` payload for `input` in the post-command `engine`.
fn title_changed(engine: &Engine, input: InputId) -> EventPayload {
    let title = engine
        .show()
        .title(input)
        .expect("an accepted title command names a title input");
    EventPayload::TitleChanged {
        input: WireInputId::from_domain(input),
        field_revision: title.fields.revision(),
        fields: wire_title_fields(title),
        overlays: protocol_overlays(engine.show().desired_switcher().overlays()),
    }
}

/// One `title_state` per title input, ascending by input, at the engine's
/// current revision.
fn title_states(engine: &Engine) -> Vec<TitleStateMessage> {
    let revision = engine.revision().get();
    let on_air = engine.titles_on_air();
    engine
        .show()
        .titles()
        .iter()
        .map(|(input, title)| {
            let air = on_air
                .iter()
                .find(|air| air.input == *input)
                .map(|air| air.frame);
            TitleStateMessage {
                revision,
                input: WireInputId::from_domain(*input),
                name: title.name.clone(),
                field_revision: title.fields.revision(),
                phase: air.map_or(TitlePhase::Idle, |air| match air.phase {
                    TitleAirPhase::In => TitlePhase::In,
                    TitleAirPhase::Hold => TitlePhase::Hold,
                    TitleAirPhase::Out => TitlePhase::Out,
                }),
                phase_frame: air.map_or(0, |air| air.phase_frame),
                phase_frames: air.map_or(0, |air| air.phase_frames),
                fields: title
                    .fields
                    .values()
                    .iter()
                    .map(|(field, value)| TitleFieldStatus {
                        field: WireTitleFieldId::new(field.get()),
                        name: title
                            .fields
                            .definitions()
                            .iter()
                            .find(|definition| definition.id == *field)
                            .map(|definition| definition.name.clone())
                            .expect("every title value has a definition"),
                        value: wire_title_value(value),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Maps the input catalogue commands: reorder, rename, add, and remove.
fn input_catalogue_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::ReorderInputs { inputs } => EngineCommand::ReorderInputs {
            inputs: inputs.iter().copied().map(WireInputId::to_domain).collect(),
        },
        CommandPayload::RenameInput { input, name } => EngineCommand::RenameInput {
            input: input.to_domain(),
            name: name.clone(),
        },
        CommandPayload::AddInput {
            input,
            name,
            source,
        } => EngineCommand::AddInput {
            input: input.to_domain(),
            name: name.clone(),
            source: engine_input_source(*source),
        },
        CommandPayload::RemoveInput { input } => EngineCommand::RemoveInput {
            input: input.to_domain(),
        },
        _ => unreachable!("only input catalogue commands are routed here"),
    }
}

const fn engine_input_source(source: LiveInputSource) -> EngineInputSource {
    match source {
        LiveInputSource::Solid {
            red,
            green,
            blue,
            alpha,
        } => EngineInputSource::Solid {
            red,
            green,
            blue,
            alpha,
        },
        LiveInputSource::Bars => EngineInputSource::Bars,
    }
}

const fn wire_input_source(source: EngineInputSource) -> LiveInputSource {
    match source {
        EngineInputSource::Solid {
            red,
            green,
            blue,
            alpha,
        } => LiveInputSource::Solid {
            red,
            green,
            blue,
            alpha,
        },
        EngineInputSource::Bars => LiveInputSource::Bars,
    }
}

fn edit_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::Undo { expected_edit } => EngineCommand::Undo {
            expected_edit: expected_edit.map(Revision::new),
        },
        CommandPayload::Redo { expected_edit } => EngineCommand::Redo {
            expected_edit: expected_edit.map(Revision::new),
        },
        CommandPayload::SetEditLock { scope, locked } => EngineCommand::SetEditLock {
            scope: match scope {
                EditLockScope::Project => EngineEditLockScope::Project,
                EditLockScope::Input(input) => EngineEditLockScope::Input(input.to_domain()),
            },
            locked: *locked,
        },
        _ => unreachable!("only edit-history and padlock commands are delegated"),
    }
}

/// Returns the project edit an undo or redo would execute if accepted: the
/// top undo entry's inverse or the top redo entry's forward command.
///
/// `None` for every other payload and for an empty stack, which the engine
/// rejects without executing anything. An animation edit carries the
/// animation revision it applies at, so it journals as a revision-checked
/// `title_animation_set`.
fn history_target(engine: &Engine, payload: &CommandPayload) -> Option<EngineCommand> {
    let target = match payload {
        CommandPayload::Undo { .. } => engine
            .undo_entries()
            .last()
            .map(|entry| entry.inverse().clone()),
        CommandPayload::Redo { .. } => engine
            .redo_entries()
            .last()
            .map(|entry| entry.forward().clone()),
        _ => None,
    }?;
    let target = title_animation::checked_history_target(engine, target);
    Some(scene_animation::checked_history_target(engine, target))
}

/// Maps an undoable engine project edit back to its exact wire payload.
///
/// Returns `None` for commands that are not project edits; history entries
/// only ever hold project edits.
fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    Some(match command {
        EngineCommand::RenameInput { input, name } => CommandPayload::RenameInput {
            input: WireInputId::from_domain(*input),
            name: name.clone(),
        },
        EngineCommand::ReorderInputs { inputs } => CommandPayload::ReorderInputs {
            inputs: inputs
                .iter()
                .copied()
                .map(WireInputId::from_domain)
                .collect(),
        },
        EngineCommand::AddInput {
            input,
            name,
            source,
        } => CommandPayload::AddInput {
            input: WireInputId::from_domain(*input),
            name: name.clone(),
            source: wire_input_source(*source),
        },
        EngineCommand::RemoveInput { input } => CommandPayload::RemoveInput {
            input: WireInputId::from_domain(*input),
        },
        EngineCommand::ConfigureStinger { slot, descriptor } => CommandPayload::ConfigureStinger {
            slot: WireStingerSlotId::new(slot.number()).expect("domain Stinger slots are bounded"),
            media_input: WireInputId::from_domain(descriptor.media_input),
            preload: descriptor.preload,
            cut_point_frames: descriptor.cut_point_frames,
            audio_policy: match descriptor.audio_policy {
                StingerAudioPolicy::Muted => ProtocolStingerAudioPolicy::Muted,
                StingerAudioPolicy::StingerOnly => ProtocolStingerAudioPolicy::StingerOnly,
                StingerAudioPolicy::MixWithProgram => ProtocolStingerAudioPolicy::MixWithProgram,
            },
            missing_media_fallback: match descriptor.missing_media_fallback {
                MissingMediaFallback::Cut => ProtocolStingerFallback::Cut,
                MissingMediaFallback::Fade => ProtocolStingerFallback::Fade,
                MissingMediaFallback::KeepProgram => ProtocolStingerFallback::KeepProgram,
            },
        },
        EngineCommand::RemoveStinger { slot } => CommandPayload::RemoveStinger {
            slot: WireStingerSlotId::new(slot.number()).expect("domain Stinger slots are bounded"),
        },
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => CommandPayload::ConfigureOverlayTransition {
            channel: wire_overlay_channel(*channel),
            transition: match transition {
                SwitcherOverlayTransition::Cut => ProtocolOverlayTransition::Cut,
                SwitcherOverlayTransition::Fade => ProtocolOverlayTransition::Fade,
            },
            duration_frames: *duration_frames,
        },
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => CommandPayload::ConfigureOverlayAppearance {
            channel: wire_overlay_channel(*channel),
            position: protocol_overlay_position(*position),
            border: protocol_overlay_border(*border),
        },
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => CommandPayload::SetOverlayOutputInclusion {
            channel: wire_overlay_channel(*channel),
            output: WireOutputId::from_domain(*output),
            included: *included,
        },
        EngineCommand::Grade(edit) => grade::wire_grade_payload(edit),
        EngineCommand::AnimationPreset(edit) => {
            animation_preset::wire_animation_preset_payload(edit)
        }
        EngineCommand::TitleAnimation(edit) => {
            return title_animation::wire_title_animation_payload(edit);
        }
        EngineCommand::SceneAnimation(edit) => {
            return scene_animation::wire_scene_animation_payload(edit);
        }
        _ => return None,
    })
}

fn wire_overlay_channel(channel: OverlayChannelId) -> WireOverlayChannelId {
    WireOverlayChannelId::new(channel.number()).expect("domain overlay channels are bounded")
}

/// Projects the engine's session undo/redo history onto the wire summary.
fn protocol_edit_history(engine: &Engine) -> EditHistoryStatus {
    let depth = |count: usize| {
        u16::try_from(count)
            .unwrap_or(u16::MAX)
            .min(fm_protocol::MAX_EDIT_HISTORY_DEPTH)
    };
    let undo = engine.undo_entries();
    let redo = engine.redo_entries();
    EditHistoryStatus {
        undo_depth: depth(undo.len()),
        next_undo: undo.last().map(|entry| entry.edit().get()),
        redo_depth: depth(redo.len()),
        next_redo: redo.last().map(|entry| entry.edit().get()),
    }
}

/// Projects the engaged padlocks onto the wire status, inputs ascending.
fn protocol_edit_locks(engine: &Engine) -> EditLockStatus {
    EditLockStatus {
        project: engine.show().is_project_locked(),
        inputs: engine
            .show()
            .locked_inputs()
            .iter()
            .copied()
            .map(WireInputId::from_domain)
            .collect(),
    }
}

fn protocol_edit_lock_scope(scope: EngineEditLockScope) -> EditLockScope {
    match scope {
        EngineEditLockScope::Project => EditLockScope::Project,
        EngineEditLockScope::Input(input) => EditLockScope::Input(WireInputId::from_domain(input)),
    }
}

fn domain_recorder_slot(recorder: WireRecorderSlot) -> RecorderSlot {
    RecorderSlot::new(recorder.get()).expect("wire recorder slots are bounded")
}

fn wire_recorder_slot(recorder: RecorderSlot) -> WireRecorderSlot {
    WireRecorderSlot::new(recorder.get()).expect("domain recorder slots are bounded")
}

fn domain_stream_target(target: WireStreamTargetId) -> StreamTargetId {
    StreamTargetId::from_non_zero(target.get())
}

fn stinger_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::Stinger {
            slot,
            duration_frames,
        } => EngineCommand::Stinger {
            slot: StingerSlotId::new(slot.number()).expect("wire Stinger slots are bounded"),
            duration_frames: *duration_frames,
        },
        CommandPayload::ConfigureStinger {
            slot,
            media_input,
            preload,
            cut_point_frames,
            audio_policy,
            missing_media_fallback,
        } => EngineCommand::ConfigureStinger {
            slot: StingerSlotId::new(slot.number()).expect("wire Stinger slots are bounded"),
            descriptor: StingerDescriptor::new(
                media_input.to_domain(),
                *preload,
                *cut_point_frames,
                match audio_policy {
                    ProtocolStingerAudioPolicy::Muted => StingerAudioPolicy::Muted,
                    ProtocolStingerAudioPolicy::StingerOnly => StingerAudioPolicy::StingerOnly,
                    ProtocolStingerAudioPolicy::MixWithProgram => {
                        StingerAudioPolicy::MixWithProgram
                    }
                },
                match missing_media_fallback {
                    ProtocolStingerFallback::Cut => MissingMediaFallback::Cut,
                    ProtocolStingerFallback::Fade => MissingMediaFallback::Fade,
                    ProtocolStingerFallback::KeepProgram => MissingMediaFallback::KeepProgram,
                },
            ),
        },
        CommandPayload::RemoveStinger { slot } => EngineCommand::RemoveStinger {
            slot: StingerSlotId::new(slot.number()).expect("wire Stinger slots are bounded"),
        },
        _ => unreachable!("only Stinger commands are delegated"),
    }
}

fn overlay_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::TakeOverlay { channel, source } => EngineCommand::TakeOverlay {
            channel: domain_overlay_channel(*channel),
            source: source.to_domain(),
        },
        CommandPayload::UpdateOverlay { channel, source } => EngineCommand::UpdateOverlay {
            channel: domain_overlay_channel(*channel),
            source: source.to_domain(),
        },
        CommandPayload::OverlayOff { channel } => EngineCommand::OverlayOff {
            channel: domain_overlay_channel(*channel),
        },
        CommandPayload::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => EngineCommand::SetOverlayOutputInclusion {
            channel: domain_overlay_channel(*channel),
            output: output.to_domain(),
            included: *included,
        },
        CommandPayload::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => EngineCommand::ConfigureOverlayTransition {
            channel: domain_overlay_channel(*channel),
            transition: match transition {
                ProtocolOverlayTransition::Cut => SwitcherOverlayTransition::Cut,
                ProtocolOverlayTransition::Fade => SwitcherOverlayTransition::Fade,
            },
            duration_frames: *duration_frames,
        },
        CommandPayload::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => EngineCommand::ConfigureOverlayAppearance {
            channel: domain_overlay_channel(*channel),
            position: match position {
                ProtocolOverlayPosition::FullFrame => SwitcherOverlayPosition::FullFrame,
                ProtocolOverlayPosition::TopLeft => SwitcherOverlayPosition::TopLeft,
                ProtocolOverlayPosition::TopRight => SwitcherOverlayPosition::TopRight,
                ProtocolOverlayPosition::BottomLeft => SwitcherOverlayPosition::BottomLeft,
                ProtocolOverlayPosition::BottomRight => SwitcherOverlayPosition::BottomRight,
            },
            border: match border {
                ProtocolOverlayBorder::None => SwitcherOverlayBorder::None,
                ProtocolOverlayBorder::ThinWhite => SwitcherOverlayBorder::ThinWhite,
                ProtocolOverlayBorder::ThickWhite => SwitcherOverlayBorder::ThickWhite,
            },
        },
        CommandPayload::QueueOverlay { channel, source } => EngineCommand::QueueOverlay {
            channel: domain_overlay_channel(*channel),
            source: source.to_domain(),
        },
        CommandPayload::TakeNextOverlay { channel } => EngineCommand::TakeNextOverlay {
            channel: domain_overlay_channel(*channel),
        },
        _ => unreachable!("only overlay commands are delegated"),
    }
}

fn domain_overlay_channel(channel: WireOverlayChannelId) -> OverlayChannelId {
    OverlayChannelId::new(channel.number()).expect("wire overlay channels are bounded")
}

const fn is_program_transition(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::Fade { .. }
            | EngineCommand::AlphaFade { .. }
            | EngineCommand::Slide { .. }
            | EngineCommand::Zoom { .. }
            | EngineCommand::Stinger { .. }
            | EngineCommand::Wipe { .. }
            // A playlist operation may take an entry to Program with a
            // transition; it is realized once the realized switcher matches
            // the desired one, which is immediate when it cut or took nothing.
            | EngineCommand::PlaylistControl(_)
            | EngineCommand::PlaylistAdvance { .. }
    )
}

fn engine_manual_position(
    position: fm_protocol::ManualTransitionPosition,
) -> EngineManualTransitionPosition {
    EngineManualTransitionPosition::new(position.basis_points())
        .expect("protocol manual position is bounded")
}

fn command_result(receipt: &CommandReceipt<EngineAcceptance>) -> CommandResult {
    match receipt {
        CommandReceipt::Accepted {
            command_id,
            acceptance,
        } => CommandResult::Accepted {
            id: command_id.as_str().to_owned(),
            revision: acceptance.revision.get(),
            scheduled_frame: Some(acceptance.result.target_frame.get()),
        },
        CommandReceipt::Rejected {
            command_id,
            rejection,
        } => CommandResult::Rejected {
            id: command_id.as_str().to_owned(),
            code: rejection.rejection.code.as_str().to_owned(),
            message: rejection.rejection.message.clone(),
            fields: rejection
                .rejection
                .fields
                .iter()
                .map(|field| FieldIssue {
                    field: field.field.clone(),
                    code: field.code.clone(),
                    message: field.message.clone(),
                })
                .collect(),
            current_revision: rejection.current_revision.get(),
            retryable: rejection.rejection.retryable,
        },
    }
}

fn engine_submission(
    identity: &EngineIdentity,
    idempotency_key: IdempotencyKey,
    engine: &Engine,
    outcome: &EngineCommandOutcome,
) -> CommandSubmission {
    let result = command_result(&outcome.receipt);
    let events = if !outcome.replayed && outcome.receipt.accepted().is_some() {
        vec![EventMessage {
            cursor: EventCursor {
                engine: identity.clone(),
                revision: engine.revision().get(),
            },
            payload: accepted_event_payload(engine, outcome),
            edit_history: protocol_edit_history(engine),
        }]
    } else {
        Vec::new()
    };
    let accepted = if !outcome.replayed && outcome.receipt.accepted().is_some() {
        Some(AcceptedOutcome {
            idempotency_key,
            receipt: outcome.receipt.clone(),
            events: outcome.events.clone(),
        })
    } else {
        None
    };
    CommandSubmission {
        output: ResultBeforeEvents { result, events },
        replayed: outcome.replayed,
        accepted,
        subscriber_failures: Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn accepted_event_payload(engine: &Engine, outcome: &EngineCommandOutcome) -> EventPayload {
    // A field save and a title take both publish the title's complete values
    // with the desired overlays. The engine reports a take as a desired
    // switcher change (one event per command); its values and field revision
    // are read back from the post-command show. A title off-air changes only
    // the overlays and publishes `desired_switcher` exactly like
    // `overlay_off`.
    if let Some(input) = outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EngineEvent::TitleFieldsChanged { input, .. }
            | EngineEvent::DesiredSwitcherChanged(EngineCommand::TakeTitle { input, .. }) => {
                Some(*input)
            }
            _ => None,
        })
    {
        return title_changed(engine, input);
    }
    if let Some(edit) = outcome.events.iter().find_map(|event| {
        let EngineEvent::GradingChanged(edit) = &event.payload else {
            return None;
        };
        Some(edit)
    }) {
        return grade::grade_changed(engine, edit);
    }
    if let Some(edit) = outcome.events.iter().find_map(|event| {
        let EngineEvent::AnimationPresetsChanged(edit) = &event.payload else {
            return None;
        };
        Some(edit)
    }) {
        return animation_preset::animation_preset_changed(engine, edit);
    }
    if let Some(input) = outcome.events.iter().find_map(|event| {
        let EngineEvent::TitleAnimationChanged { input } = &event.payload else {
            return None;
        };
        Some(*input)
    }) {
        return title_animation::title_animation_changed(engine, input);
    }
    if let Some(input) = outcome.events.iter().find_map(|event| {
        let EngineEvent::SceneAnimationChanged { input } = &event.payload else {
            return None;
        };
        Some(*input)
    }) {
        return scene_animation::scene_animation_changed(engine, input);
    }
    if let Some(frame) = outcome.events.iter().find_map(|event| {
        let EngineEvent::PlaylistChanged { frame, .. } = &event.payload else {
            return None;
        };
        Some(*frame)
    }) {
        return playlist::playlist_changed(engine, frame);
    }
    if let Some((scope, locked)) = outcome.events.iter().find_map(|event| {
        let EngineEvent::EditLockChanged { scope, locked } = &event.payload else {
            return None;
        };
        Some((*scope, *locked))
    }) {
        return EventPayload::EditLockChanged {
            scope: protocol_edit_lock_scope(scope),
            locked,
        };
    }
    let streams_changed = outcome
        .events
        .iter()
        .any(|event| matches!(&event.payload, EngineEvent::StreamsChanged { .. }));
    if streams_changed {
        return EventPayload::StreamsChanged {
            streams: protocol_streams(engine),
        };
    }
    if let Some((recorder, active)) = outcome.events.iter().find_map(|event| {
        let EngineEvent::RecordingChanged { recorder, active } = &event.payload else {
            return None;
        };
        Some((*recorder, *active))
    }) {
        return EventPayload::RecordingChanged {
            recorder: wire_recorder_slot(recorder),
            active,
        };
    }
    if let Some((input, name)) = outcome.events.iter().find_map(|event| {
        let EngineEvent::InputRenamed { input, name } = &event.payload else {
            return None;
        };
        Some((*input, name))
    }) {
        return EventPayload::InputRenamed {
            input: WireInputId::from_domain(input),
            name: name.clone(),
        };
    }
    if let Some(payload) = outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EngineEvent::InputAdded { input, name, .. } => Some(EventPayload::InputAdded {
                input: WireInputId::from_domain(*input),
                name: name.clone(),
            }),
            EngineEvent::InputRemoved { input } => Some(EventPayload::InputRemoved {
                input: WireInputId::from_domain(*input),
            }),
            _ => None,
        })
    {
        return payload;
    }
    let input_order_changed = outcome.events.iter().find_map(|event| {
        let EngineEvent::InputOrderChanged { inputs } = &event.payload else {
            return None;
        };
        Some(EventPayload::InputOrderChanged {
            inputs: inputs
                .iter()
                .copied()
                .map(WireInputId::from_domain)
                .collect(),
        })
    });
    let stinger_slots_changed = outcome.events.iter().any(|event| {
        matches!(
            &event.payload,
            EngineEvent::DesiredSwitcherChanged(
                EngineCommand::ConfigureStinger { .. } | EngineCommand::RemoveStinger { .. }
            )
        )
    });
    let program = WireInputId::from_domain(engine.show().desired_switcher().program());
    let preview = WireInputId::from_domain(engine.show().desired_switcher().preview());
    let manual_transition = protocol_manual_status(engine.desired_manual_transition());
    let fade_to_black = protocol_fade_to_black_state(engine.desired_fade_to_black());
    let overlays = protocol_overlays(engine.show().desired_switcher().overlays());
    let input_audio_strips = protocol_input_audio_strips(engine);
    if stinger_slots_changed {
        EventPayload::StingerSlotsChanged {
            program,
            preview,
            manual_transition,
            fade_to_black,
            stingers: protocol_desired_stingers(engine),
            overlays,
            input_audio_strips,
        }
    } else if let Some(payload) = input_order_changed {
        payload
    } else {
        EventPayload::DesiredSwitcher {
            program,
            preview,
            manual_transition,
            fade_to_black,
            overlays,
            input_audio_strips,
        }
    }
}

fn project_engine(engine: &Engine, ticks: u64) -> Result<EngineSnapshot, EngineError> {
    let mut projected = engine.clone();
    for _ in 0..ticks {
        projected.tick()?;
    }
    projected.snapshot().map_err(EngineError::from)
}

fn snapshot_record(engine: &Engine, identity: &EngineIdentity) -> SnapshotRecord {
    let snapshot = SnapshotMessage {
        engine: identity.clone(),
        revision: engine.revision().get(),
        show_name: engine.show().name().to_owned(),
        inputs: engine
            .show()
            .inputs()
            .iter()
            .copied()
            .zip(engine.show().input_names().iter().cloned())
            .map(|(input, name)| InputStatus {
                input: WireInputId::from_domain(input),
                name,
            })
            .collect(),
        outputs: engine
            .show()
            .outputs()
            .iter()
            .map(|(output, name)| OutputStatus {
                output: WireOutputId::from_domain(*output),
                name: name.clone(),
            })
            .collect(),
        input_audio_strips: protocol_input_audio_strips(engine),
        desired_program: WireInputId::from_domain(engine.show().desired_switcher().program()),
        desired_preview: WireInputId::from_domain(engine.show().desired_switcher().preview()),
        realized_program: WireInputId::from_domain(engine.realized_switcher().program()),
        realized_preview: WireInputId::from_domain(engine.realized_switcher().preview()),
        desired_manual_transition: protocol_manual_status(engine.desired_manual_transition()),
        realized_manual_transition: protocol_manual_status(engine.realized_manual_transition()),
        desired_fade_to_black: protocol_fade_to_black_state(engine.desired_fade_to_black()),
        realized_fade_to_black: protocol_fade_to_black_state(engine.realized_fade_to_black()),
        stingers: protocol_stingers(engine),
        streams: protocol_streams(engine),
        recordings: RecorderSlot::ALL
            .into_iter()
            .map(|recorder| RecordingStatus {
                recorder: wire_recorder_slot(recorder),
                desired_active: engine.show().desired_switcher().recording_desired(recorder),
            })
            .collect(),
        desired_overlays: protocol_overlays(engine.show().desired_switcher().overlays()),
        realized_overlays: protocol_overlays(engine.realized_switcher().overlays()),
        edit_locks: protocol_edit_locks(engine),
        edit_history: protocol_edit_history(engine),
        titles: engine
            .show()
            .titles()
            .iter()
            .map(|(input, title)| TitleSummary {
                input: WireInputId::from_domain(*input),
                field_revision: title.fields.revision(),
            })
            .collect(),
        scenes: scene_animation::snapshot_scenes(engine),
        frame_rate: engine.frame_rate(),
        playlist_follows: playlist::playlist_follows(engine),
        grade_states: grade::grade_state_count(engine),
        animation_preset_states: animation_preset::animation_preset_state_count(engine),
    };
    SnapshotRecord {
        cursor: EventCursor {
            engine: identity.clone(),
            revision: snapshot.revision,
        },
        snapshot,
    }
}

fn protocol_input_audio_strips(engine: &Engine) -> Vec<InputAudioStripStatus> {
    engine
        .show()
        .input_audio_strips()
        .iter()
        .map(|(&input, &state)| InputAudioStripStatus {
            input: WireInputId::from_domain(input),
            gain_millidb: state.gain_millidb,
            balance_basis_points: state.balance_basis_points,
            muted: state.muted,
            soloed: state.soloed,
            follow_video: state.follow_video,
            delay_samples: state.delay_samples,
        })
        .collect()
}

fn protocol_overlays(overlays: &[OverlayChannelState; 8]) -> Vec<OverlayStatus> {
    overlays
        .iter()
        .enumerate()
        .map(|(index, overlay)| OverlayStatus {
            channel: WireOverlayChannelId::new(
                u8::try_from(index + 1).expect("overlay channel index fits u8"),
            )
            .expect("overlay channel index is bounded"),
            source: overlay.source().map(WireInputId::from_domain),
            active: overlay.is_active(),
            opacity: overlay.opacity(),
            transition: match overlay.transition() {
                SwitcherOverlayTransition::Cut => ProtocolOverlayTransition::Cut,
                SwitcherOverlayTransition::Fade => ProtocolOverlayTransition::Fade,
            },
            duration_frames: overlay.duration_frames(),
            position: protocol_overlay_position(overlay.position()),
            border: protocol_overlay_border(overlay.border()),
            queued_sources: overlay
                .queued_sources()
                .iter()
                .copied()
                .map(WireInputId::from_domain)
                .collect(),
            included_outputs: overlay
                .included_outputs()
                .iter()
                .copied()
                .map(WireOutputId::from_domain)
                .collect(),
        })
        .collect()
}

const fn protocol_overlay_position(position: SwitcherOverlayPosition) -> ProtocolOverlayPosition {
    match position {
        SwitcherOverlayPosition::FullFrame => ProtocolOverlayPosition::FullFrame,
        SwitcherOverlayPosition::TopLeft => ProtocolOverlayPosition::TopLeft,
        SwitcherOverlayPosition::TopRight => ProtocolOverlayPosition::TopRight,
        SwitcherOverlayPosition::BottomLeft => ProtocolOverlayPosition::BottomLeft,
        SwitcherOverlayPosition::BottomRight => ProtocolOverlayPosition::BottomRight,
    }
}

const fn protocol_overlay_border(border: SwitcherOverlayBorder) -> ProtocolOverlayBorder {
    match border {
        SwitcherOverlayBorder::None => ProtocolOverlayBorder::None,
        SwitcherOverlayBorder::ThinWhite => ProtocolOverlayBorder::ThinWhite,
        SwitcherOverlayBorder::ThickWhite => ProtocolOverlayBorder::ThickWhite,
    }
}

fn protocol_stingers(engine: &Engine) -> Vec<StingerStatus> {
    engine
        .show()
        .desired_switcher()
        .stingers()
        .iter()
        .enumerate()
        .filter_map(|(index, desired)| {
            let descriptor = desired.descriptor()?;
            let slot = StingerSlotId::from_index(index).expect("Stinger array index is bounded");
            let realized = engine.realized_switcher().stinger(slot);
            Some(protocol_stinger_status(
                slot,
                descriptor,
                if realized.descriptor() == Some(descriptor) {
                    realized.preload_state()
                } else {
                    StingerPreloadState::NotRequested
                },
            ))
        })
        .collect()
}

fn protocol_stinger_status(
    slot: StingerSlotId,
    descriptor: &StingerDescriptor,
    readiness: StingerPreloadState,
) -> StingerStatus {
    StingerStatus {
        slot: WireStingerSlotId::new(slot.number()).expect("domain Stinger slot is bounded"),
        media_input: WireInputId::from_domain(descriptor.media_input),
        preload: descriptor.preload,
        cut_point_frames: descriptor.cut_point_frames,
        audio_policy: match descriptor.audio_policy {
            StingerAudioPolicy::Muted => ProtocolStingerAudioPolicy::Muted,
            StingerAudioPolicy::StingerOnly => ProtocolStingerAudioPolicy::StingerOnly,
            StingerAudioPolicy::MixWithProgram => ProtocolStingerAudioPolicy::MixWithProgram,
        },
        missing_media_fallback: match descriptor.missing_media_fallback {
            MissingMediaFallback::Cut => ProtocolStingerFallback::Cut,
            MissingMediaFallback::Fade => ProtocolStingerFallback::Fade,
            MissingMediaFallback::KeepProgram => ProtocolStingerFallback::KeepProgram,
        },
        readiness: match readiness {
            StingerPreloadState::NotRequested => StingerReadiness::NotRequested,
            StingerPreloadState::Ready => StingerReadiness::Ready,
            StingerPreloadState::Missing => StingerReadiness::Missing,
        },
    }
}

fn protocol_desired_stingers(engine: &Engine) -> Vec<StingerStatus> {
    engine
        .show()
        .desired_switcher()
        .stingers()
        .iter()
        .enumerate()
        .filter_map(|(index, desired)| {
            let descriptor = desired.descriptor()?;
            let slot = StingerSlotId::from_index(index).expect("Stinger array index is bounded");
            Some(protocol_stinger_status(
                slot,
                descriptor,
                desired.preload_state(),
            ))
        })
        .collect()
}

fn protocol_streams(engine: &Engine) -> Vec<StreamStatus> {
    let desired = engine.show().desired_switcher();
    desired
        .streams()
        .iter()
        .map(|stream| StreamStatus {
            target: WireStreamTargetId::new(stream.id().get()),
            name: stream.name().to_owned(),
            desired_running: desired
                .stream_running(stream.id())
                .expect("inventoried stream has a desired flag"),
            realized: StreamRealizedState::Stopped,
            detail: None,
        })
        .collect()
}

fn protocol_manual_status(state: Option<EngineManualTransitionState>) -> ManualTransitionStatus {
    let Some(state) = state else {
        return ManualTransitionStatus::Inactive;
    };
    let kind = match state.kind {
        EngineManualTransitionKind::Fade => ManualTransitionKind::Fade,
        EngineManualTransitionKind::Wipe => ManualTransitionKind::Wipe,
        EngineManualTransitionKind::AlphaFade => ManualTransitionKind::AlphaFade,
        EngineManualTransitionKind::Slide => ManualTransitionKind::Slide,
    };
    ManualTransitionStatus::Active(ManualTransitionState {
        kind,
        from: WireInputId::from_domain(state.from),
        to: WireInputId::from_domain(state.to),
        interval_start: ManualTransitionPosition::new(state.interval_start.basis_points())
            .expect("engine manual transition positions are bounded"),
        position: ManualTransitionPosition::new(state.position.basis_points())
            .expect("engine manual transition positions are bounded"),
    })
}

fn protocol_fade_to_black_state(state: EngineFadeToBlackState) -> FadeToBlackState {
    FadeToBlackState {
        target_active: state.active,
        position: FadeToBlackPosition::new(
            u16::try_from(state.position.numerator())
                .expect("engine Fade-to-Black positions use a u16 denominator"),
        ),
    }
}

#[cfg(test)]
mod tests;
