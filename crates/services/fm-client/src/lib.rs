//! Transport-independent diagnostic protocol client state machine.

#[cfg(feature = "std-tcp")]
mod tcp;
#[cfg(feature = "std-websocket")]
mod ws;

#[cfg(feature = "std-tcp")]
pub use tcp::{
    DisconnectCause, SessionEvent, TcpConnection, TcpConnectionError, TcpSession, TcpSessionError,
};

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;

use fm_command::{CommandId, Revision};
use fm_protocol::{
    AnimationPresetStateMessage, CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage,
    CommandPayload, CommandResult, DurableGap, EngineIdentity, EventMessage, GradeStateMessage,
    HandshakeOutcome, HandshakeRequest, HandshakeResponse, HeartbeatAcknowledgementMessage,
    HeartbeatMessage, PlaylistStateMessage, ProtocolVersion, ResumeCursor, Role,
    RuntimeEventMessage, RuntimeLifecycleEvent, SceneAnimationStateMessage,
    SceneLayoutStateMessage, ServerHello, ServerIdentity, SnapshotMessage, StructuredError,
    TitleAnimationStateMessage, TitleLayoutStateMessage, TitleStateMessage, WireMessage,
};
use fm_types::ProjectId;
use fm_ui_model::{
    ClientModel, DurableProjectEvent, ManualTransitionStatus as ModelManualTransitionStatus,
    ModelError, OptimisticChange, ProjectSnapshot, RuntimeRealization,
};

/// Default number of recent completed command statuses retained locally.
pub const DEFAULT_COMPLETED_COMMAND_CAPACITY: usize = 256;
/// Largest accepted completed-command history bound.
pub const MAX_COMPLETED_COMMAND_CAPACITY: usize = 65_536;

/// Static client settings. An external scheduler supplies all clock behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientConfig {
    pub build: String,
    pub client_type: ClientType,
    pub desired_role: Role,
    pub client_id: String,
    pub project_id: ProjectId,
    pub outbound_capacity: usize,
    /// Maximum locally retained terminal records. Eviction forgets the key only
    /// locally; callers must keep keys globally unique over server receipt retention.
    pub completed_command_capacity: usize,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl ClientConfig {
    /// Creates settings with conservative queue and reconnect defaults.
    #[must_use]
    pub fn new(
        build: impl Into<String>,
        client_type: ClientType,
        desired_role: Role,
        client_id: impl Into<String>,
        project_id: ProjectId,
    ) -> Self {
        Self {
            build: build.into(),
            client_type,
            desired_role,
            client_id: client_id.into(),
            project_id,
            outbound_capacity: 256,
            completed_command_capacity: DEFAULT_COMPLETED_COMMAND_CAPACITY,
            initial_backoff_ms: 250,
            max_backoff_ms: 30_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    Snapshot,
    Resume,
}

/// A reconnect delay for the transport owner's scheduler to elapse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectBackoff {
    pub attempt: u32,
    pub delay_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    AwaitingHandshake,
    Synchronizing {
        mode: SyncMode,
        target_revision: u64,
    },
    Ready,
    Backoff(ReconnectBackoff),
    ResyncRequired {
        expected_revision: u64,
        received_revision: u64,
    },
    ProtocolMismatch {
        protocol: ProtocolVersion,
    },
}

/// Metadata accepted from the current handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub protocol: ProtocolVersion,
    pub granted_role: Role,
    pub permissions: Vec<String>,
    pub server: ServerIdentity,
    pub capabilities_digest: String,
}

/// Items waiting for a transport adapter. The bounded queue deliberately does
/// not contain connection setup; [`Client::transport_connected`] returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outbound {
    Command(CommandMessage),
    Heartbeat(HeartbeatMessage),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Queued,
    Sent,
    Completed(CommandResult),
    TerminalUncertain(CommandUncertainty),
}

impl CommandStatus {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed(_) | Self::TerminalUncertain(_))
    }

    const fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Sent)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandUncertainty {
    IdempotencyReplayCollision { received_command_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRecord {
    pub command: CommandMessage,
    pub status: CommandStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intake {
    Handshake,
    SnapshotApplied,
    EventApplied,
    RuntimeEventObserved,
    ResultReconciled,
    DuplicateResult,
    HeartbeatAcknowledged,
    ResyncRequested,
    /// A `title_state` completed one title the snapshot announced.
    TitleStateApplied,
    /// The `playlist_state` the snapshot announced completed it.
    PlaylistStateApplied,
    /// A `grade_state` completed one grading item the snapshot announced.
    GradeStateApplied,
    /// A `title_animation_state` completed one title's animation the
    /// snapshot announced.
    TitleAnimationStateApplied,
    /// A `title_layout_state` completed one title's layout the snapshot
    /// announced.
    TitleLayoutStateApplied,
    /// A `scene_animation_state` completed one Scene input's animation the
    /// snapshot announced (Protocol 2.31).
    SceneAnimationStateApplied,
    /// A `scene_layout_state` completed one Scene input's layout the
    /// snapshot announced (Protocol 2.31).
    SceneLayoutStateApplied,
    /// An `animation_preset_state` completed one library preset the
    /// snapshot announced (Protocol 2.34).
    AnimationPresetStateApplied,
    /// A `media_state` completed one media input the snapshot announced
    /// (Protocol 2.37).
    MediaStateApplied,
    /// A `cue_list_state` completed one cue list the snapshot announced
    /// (Protocol 2.50).
    CueListStateApplied,
    /// A `script_state` completed one project script the snapshot
    /// announced (Protocol 2.58).
    ScriptStateApplied,
    /// A `shortcut_state` completed one user shortcut the snapshot
    /// announced (Protocol 2.72).
    ShortcutStateApplied,
    /// A `media_list_state` completed one media list input the snapshot
    /// announced (IN-006).
    MediaListStateApplied,

    /// An `audio_strip_dsp_state` completed one input strip's DSP chain the
    /// snapshot announced (Protocol 2.75).
    AudioStripDspStateApplied,
    /// A `display_inventory` reply updated the replicated display inventory
    /// (Protocol 2.42).
    DisplayInventoryApplied,
    /// A `capture_devices` reply updated the replicated capture device
    /// inventory (Protocol 2.47).
    CaptureDevicesApplied,
    /// A `system_audio_devices` reply updated the replicated system audio
    /// device list (Protocol 2.68).
    SystemAudioDevicesApplied,
    /// A `web_access` reply updated the replicated web access state
    /// (Protocol 2.71).
    WebAccessApplied,
    /// A `still_state` completed one image or slideshow input the snapshot
    /// announced (IN-008, IN-009).
    StillStateApplied,
    /// A `slideshow_slides` reply updated a replicated slideshow listing
    /// (IN-009).
    SlideshowSlidesApplied,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    InvalidConfig(&'static str),
    InvalidState {
        operation: &'static str,
        state: ConnectionState,
    },
    ProtocolMismatch(ProtocolVersion),
    HandshakeRejected(StructuredError),
    InvalidHandshake(&'static str),
    InvalidSnapshot(&'static str),
    Model(ModelError),
    StaleEvent {
        expected_revision: u64,
        received_revision: u64,
    },
    ResyncRequired {
        expected_revision: u64,
        received_revision: u64,
    },
    RuntimeIdentityMismatch {
        expected: Box<ServerIdentity>,
        received: Box<ServerIdentity>,
    },
    RuntimeRevisionAhead {
        current_revision: u64,
        received_revision: u64,
    },
    RuntimeSequenceGap {
        generation: u64,
        expected_sequence: u64,
        received_sequence: u64,
    },
    RuntimeGenerationRegressed {
        current_generation: u64,
        received_generation: u64,
    },
    QueueFull {
        capacity: usize,
    },
    EmptyIdempotencyKey,
    DuplicateIdempotencyKey(String),
    CommandIdExhausted,
    HeartbeatSequenceExhausted,
    HeartbeatAcknowledgementPending,
    InvalidHeartbeatAcknowledgement(&'static str),
    UnknownCommand(String),
    CommandAlreadyCompleted(String),
    CommandTerminalUncertain(String),
    ConflictingResult(String),
    IdempotencyReplayCollision {
        received_command_id: String,
        affected_command_ids: Vec<String>,
    },
    UnexpectedMessage,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message)
            | Self::InvalidHandshake(message)
            | Self::InvalidSnapshot(message)
            | Self::InvalidHeartbeatAcknowledgement(message) => formatter.write_str(message),
            Self::InvalidState { operation, state } => {
                write!(formatter, "cannot {operation} while client is {state:?}")
            }
            Self::ProtocolMismatch(version) => {
                write!(formatter, "server selected non-current protocol {version}")
            }
            Self::HandshakeRejected(error) => {
                write!(
                    formatter,
                    "handshake rejected: {}: {}",
                    error.code, error.message
                )
            }
            Self::Model(error) => error.fmt(formatter),
            Self::StaleEvent {
                expected_revision,
                received_revision,
            } => write!(
                formatter,
                "stale event revision {received_revision}; expected {expected_revision}"
            ),
            Self::ResyncRequired {
                expected_revision,
                received_revision,
            } => write!(
                formatter,
                "event gap at revision {received_revision}; expected {expected_revision}"
            ),
            Self::RuntimeIdentityMismatch { .. } => {
                formatter.write_str("runtime event identity does not match the session")
            }
            Self::RuntimeRevisionAhead {
                current_revision,
                received_revision,
            } => write!(
                formatter,
                "runtime event revision {received_revision} is ahead of durable revision {current_revision}"
            ),
            Self::RuntimeSequenceGap {
                generation,
                expected_sequence,
                received_sequence,
            } => write!(
                formatter,
                "runtime event sequence gap for generation {generation}: expected {expected_sequence}, got {received_sequence}"
            ),
            Self::RuntimeGenerationRegressed {
                current_generation,
                received_generation,
            } => write!(
                formatter,
                "runtime event generation {received_generation} is older than generation {current_generation}"
            ),
            Self::QueueFull { capacity } => {
                write!(formatter, "outbound queue reached capacity {capacity}")
            }
            Self::EmptyIdempotencyKey => formatter.write_str("idempotency key must not be blank"),
            Self::DuplicateIdempotencyKey(key) => {
                write!(formatter, "idempotency key {key:?} is already in use")
            }
            Self::CommandIdExhausted => formatter.write_str("client command ID space exhausted"),
            Self::HeartbeatSequenceExhausted => {
                formatter.write_str("heartbeat sequence space exhausted")
            }
            Self::HeartbeatAcknowledgementPending => {
                formatter.write_str("a heartbeat acknowledgement is already pending")
            }
            Self::UnknownCommand(id) => write!(formatter, "unknown command result ID {id:?}"),
            Self::CommandAlreadyCompleted(id) => {
                write!(formatter, "command {id:?} is already complete")
            }
            Self::CommandTerminalUncertain(id) => {
                write!(formatter, "command {id:?} has a terminal uncertain outcome")
            }
            Self::ConflictingResult(id) => {
                write!(formatter, "command {id:?} received conflicting results")
            }
            Self::IdempotencyReplayCollision {
                received_command_id,
                affected_command_ids,
            } => write!(
                formatter,
                "server replayed unknown command ID {received_command_id:?} while sent command(s) {affected_command_ids:?} were active; outcomes are uncertain and an authoritative snapshot is required"
            ),
            Self::UnexpectedMessage => formatter.write_str("unexpected inbound message type"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ModelError> for ClientError {
    fn from(value: ModelError) -> Self {
        Self::Model(value)
    }
}

/// Deterministic client state driven entirely by explicit transport events.
#[derive(Debug)]
pub struct Client {
    config: ClientConfig,
    state: ConnectionState,
    session: Option<Session>,
    model: ClientModel,
    outbound: VecDeque<Outbound>,
    commands: BTreeMap<String, CommandRecord>,
    idempotency_keys: HashSet<String>,
    completed_command_ids: VecDeque<String>,
    next_command_id: u64,
    next_heartbeat_sequence: u64,
    expected_heartbeat_sequence: Option<u64>,
    reconnect_attempt: u32,
    force_snapshot: bool,
    runtime_server: Option<ServerIdentity>,
    /// The newest runtime generation observed on this connection and its
    /// last sequence. Runtime generations only advance, so nothing older is
    /// retained.
    runtime_sequence: Option<(u64, u64)>,
}

impl Client {
    /// Creates a disconnected client.
    ///
    /// # Errors
    ///
    /// Rejects settings that cannot provide bounded operation.
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        if config.client_id.is_empty() {
            return Err(ClientError::InvalidConfig("client ID must not be empty"));
        }
        if config.outbound_capacity == 0 {
            return Err(ClientError::InvalidConfig(
                "outbound capacity must be greater than zero",
            ));
        }
        if config.completed_command_capacity == 0
            || config.completed_command_capacity > MAX_COMPLETED_COMMAND_CAPACITY
        {
            return Err(ClientError::InvalidConfig(
                "completed command capacity must be finite and between 1 and 65536",
            ));
        }
        if config.initial_backoff_ms == 0 || config.max_backoff_ms < config.initial_backoff_ms {
            return Err(ClientError::InvalidConfig(
                "invalid reconnect backoff range",
            ));
        }
        let project_id = config.project_id;
        Ok(Self {
            config,
            state: ConnectionState::Disconnected,
            session: None,
            model: ClientModel::new(project_id),
            outbound: VecDeque::new(),
            commands: BTreeMap::new(),
            idempotency_keys: HashSet::new(),
            completed_command_ids: VecDeque::new(),
            next_command_id: 1,
            next_heartbeat_sequence: 1,
            expected_heartbeat_sequence: None,
            reconnect_attempt: 0,
            force_snapshot: false,
            runtime_server: None,
            runtime_sequence: None,
        })
    }

    #[must_use]
    pub const fn state(&self) -> &ConnectionState {
        &self.state
    }

    #[must_use]
    pub const fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    #[must_use]
    pub const fn model(&self) -> &ClientModel {
        &self.model
    }

    #[must_use]
    pub fn last_applied_cursor(&self) -> Option<ResumeCursor> {
        let cursor = self.model.reconnect_cursor()?;
        Some(ResumeCursor {
            server: ServerIdentity {
                engine_id: cursor.engine.engine_id.clone(),
                project_id: self.config.project_id.to_string(),
                state_epoch: cursor.engine.state_epoch,
                log_id: cursor.engine.log_id.clone(),
            },
            revision: cursor.revision.get(),
        })
    }

    #[must_use]
    pub fn outbound_len(&self) -> usize {
        self.outbound.len()
    }

    /// Maximum number of queued transport records and TCP in-flight commands.
    #[must_use]
    pub const fn outbound_capacity(&self) -> usize {
        self.config.outbound_capacity
    }

    #[must_use]
    pub fn command(&self, id: &str) -> Option<&CommandRecord> {
        self.commands.get(id)
    }

    /// Number of active commands plus bounded, completion-ordered local history.
    #[must_use]
    pub fn retained_command_count(&self) -> usize {
        self.commands.len()
    }

    /// Begins a connection attempt. Backoff timing remains caller-owned.
    ///
    /// # Errors
    ///
    /// Requires a disconnected, backed-off, or resync-required state.
    pub fn start_connect(&mut self) -> Result<(), ClientError> {
        match self.state {
            ConnectionState::Disconnected
            | ConnectionState::Backoff(_)
            | ConnectionState::ResyncRequired { .. } => {
                self.state = ConnectionState::Connecting;
                Ok(())
            }
            _ => Err(self.invalid_state("start a connection")),
        }
    }

    /// Constructs the initial handshake after transport establishment.
    ///
    /// # Errors
    ///
    /// Requires an active connection attempt.
    pub fn transport_connected(&mut self) -> Result<HandshakeRequest, ClientError> {
        if self.state != ConnectionState::Connecting {
            return Err(self.invalid_state("report transport connected"));
        }
        let request = HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: self.config.build.clone(),
            client_type: self.config.client_type,
            desired_role: self.config.desired_role,
            // A snapshot whose titles or playlist never completed cannot be
            // resumed: resumed events carry no title or playlist states.
            resume_cursor: if self.force_snapshot || !self.snapshot_complete() {
                None
            } else {
                self.last_applied_cursor()
            },
        };
        self.state = ConnectionState::AwaitingHandshake;
        Ok(request)
    }

    /// Enters exponential backoff without sleeping or consulting a clock.
    #[must_use]
    pub fn transport_disconnected(&mut self) -> ReconnectBackoff {
        self.session = None;
        self.expected_heartbeat_sequence = None;
        self.reset_runtime_sequences();
        self.outbound
            .retain(|item| matches!(item, Outbound::Command(_)));
        let backoff = self.next_reconnect_backoff();
        self.state = ConnectionState::Backoff(backoff);
        backoff
    }

    fn next_reconnect_backoff(&mut self) -> ReconnectBackoff {
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        let exponent = self.reconnect_attempt.saturating_sub(1).min(63);
        let delay_ms = self
            .config
            .initial_backoff_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.config.max_backoff_ms);
        ReconnectBackoff {
            attempt: self.reconnect_attempt,
            delay_ms,
        }
    }

    /// Dispatches one inbound protocol DTO.
    ///
    /// # Errors
    ///
    /// Rejects messages invalid for the current state or replicated model.
    #[allow(clippy::too_many_lines)] // one dispatch over every message type
    pub fn intake(&mut self, message: WireMessage) -> Result<Intake, ClientError> {
        match message {
            WireMessage::HandshakeResponse(response) => {
                self.accept_handshake(response)?;
                Ok(Intake::Handshake)
            }
            WireMessage::Snapshot(snapshot) => {
                self.apply_snapshot(snapshot)?;
                Ok(Intake::SnapshotApplied)
            }
            WireMessage::Event(event) => {
                self.apply_event(event)?;
                Ok(Intake::EventApplied)
            }
            WireMessage::RuntimeEvent(event) => {
                self.apply_runtime_event(event)?;
                Ok(Intake::RuntimeEventObserved)
            }
            WireMessage::HeartbeatAcknowledgement(acknowledgement) => {
                self.accept_heartbeat_acknowledgement(&acknowledgement)?;
                Ok(Intake::HeartbeatAcknowledged)
            }
            WireMessage::CommandResult(result) => self.reconcile_result(result),
            WireMessage::DurableGap(gap) => {
                self.apply_gap(&gap)?;
                Ok(Intake::ResyncRequested)
            }
            WireMessage::TitleState(state) => {
                self.apply_title_state(state)?;
                Ok(Intake::TitleStateApplied)
            }
            WireMessage::PlaylistState(state) => {
                self.apply_playlist_state(state)?;
                Ok(Intake::PlaylistStateApplied)
            }
            WireMessage::GradeState(state) => {
                self.apply_grade_state(state)?;
                Ok(Intake::GradeStateApplied)
            }
            WireMessage::TitleAnimationState(state) => {
                self.apply_title_animation_state(state)?;
                Ok(Intake::TitleAnimationStateApplied)
            }
            WireMessage::TitleLayoutState(state) => {
                self.apply_title_layout_state(state)?;
                Ok(Intake::TitleLayoutStateApplied)
            }
            WireMessage::SceneAnimationState(state) => {
                self.apply_scene_animation_state(state)?;
                Ok(Intake::SceneAnimationStateApplied)
            }
            WireMessage::SceneLayoutState(state) => {
                self.apply_scene_layout_state(state)?;
                Ok(Intake::SceneLayoutStateApplied)
            }
            WireMessage::AnimationPresetState(state) => {
                self.apply_animation_preset_state(state)?;
                Ok(Intake::AnimationPresetStateApplied)
            }
            WireMessage::MediaState(state) => {
                self.apply_media_state(state)?;
                Ok(Intake::MediaStateApplied)
            }
            WireMessage::CueListState(state) => {
                self.apply_cue_list_state(state)?;
                Ok(Intake::CueListStateApplied)
            }
            WireMessage::ScriptState(state) => {
                self.apply_script_state(state)?;
                Ok(Intake::ScriptStateApplied)
            }
            WireMessage::ShortcutState(state) => {
                self.apply_shortcut_state(state)?;
                Ok(Intake::ShortcutStateApplied)
            }
            WireMessage::StillState(state) => {
                self.apply_still_state(state)?;
                Ok(Intake::StillStateApplied)
            }
            WireMessage::SlideshowSlides(message) => {
                self.model.apply_slideshow_slides(&message)?;
                Ok(Intake::SlideshowSlidesApplied)
            }
            WireMessage::MediaListState(state) => {
                self.apply_media_list_state(state)?;
                Ok(Intake::MediaListStateApplied)
            }
            WireMessage::AudioStripDspState(state) => {
                self.apply_audio_strip_dsp_state(state)?;
                Ok(Intake::AudioStripDspStateApplied)
            }
            WireMessage::DisplayInventory(message) => {
                self.model.apply_display_inventory(message.inventory);
                Ok(Intake::DisplayInventoryApplied)
            }
            WireMessage::CaptureDevices(message) => {
                self.model.apply_capture_inventory(message.inventory);
                Ok(Intake::CaptureDevicesApplied)
            }
            WireMessage::SystemAudioDevices(message) => {
                self.model.apply_system_audio_inventory(message.inventory);
                Ok(Intake::SystemAudioDevicesApplied)
            }
            WireMessage::WebAccess(message) => {
                self.model.apply_web_access(message);
                Ok(Intake::WebAccessApplied)
            }
            WireMessage::DisplayInventoryRequest(_)
            | WireMessage::CaptureDevicesRequest(_)
            | WireMessage::SystemAudioDevicesRequest(_)
            | WireMessage::WebAccessRequest(_)
            | WireMessage::SlideshowSlidesRequest(_)
            | WireMessage::Command(_)
            | WireMessage::HandshakeRequest(_)
            | WireMessage::DurableEventBatch(_)
            | WireMessage::Heartbeat(_)
            | WireMessage::AudioMeters(_)
            | WireMessage::StreamStatus(_)
            | WireMessage::RecorderStatus(_)
            | WireMessage::MonitorFrame(_)
            | WireMessage::InputThumbnailSubscription(_)
            | WireMessage::InputThumbnail(_)
            | WireMessage::CapabilityReport(_)
            | WireMessage::DiagnosticsRequest(_)
            | WireMessage::DiagnosticsResponse(_)
            | WireMessage::TitlePreviewRequest(_)
            | WireMessage::TitlePreviewFrame(_)
            | WireMessage::ScenePreviewRequest(_)
            | WireMessage::ScenePreviewFrame(_)
            | WireMessage::Error(_) => Err(ClientError::UnexpectedMessage),
        }
    }

    /// Validates protocol and identity before choosing snapshot or resume.
    ///
    /// # Errors
    ///
    /// Rejects non-current versions, project identities, resume cursors, and
    /// structured handshake failures.
    pub fn accept_handshake(&mut self, response: HandshakeResponse) -> Result<(), ClientError> {
        if self.state != ConnectionState::AwaitingHandshake {
            return Err(self.invalid_state("accept a handshake"));
        }
        if response.protocol != CURRENT_PROTOCOL_VERSION {
            self.state = ConnectionState::ProtocolMismatch {
                protocol: response.protocol,
            };
            return Err(ClientError::ProtocolMismatch(response.protocol));
        }
        if response.server.project_id != self.config.project_id.to_string() {
            return Err(ClientError::InvalidHandshake(
                "server selected a different project",
            ));
        }

        let resume = match &response.outcome {
            HandshakeOutcome::Snapshot { .. } => false,
            HandshakeOutcome::Resume { cursor } => {
                if self.force_snapshot
                    || cursor.server != response.server
                    || self.last_applied_cursor().as_ref() != Some(cursor)
                    || cursor.revision > response.current_revision
                {
                    return Err(ClientError::InvalidHandshake(
                        "server offered an invalid resume cursor",
                    ));
                }
                true
            }
            HandshakeOutcome::Rejected { error } => {
                self.state = ConnectionState::Disconnected;
                return Err(ClientError::HandshakeRejected(error.clone()));
            }
        };

        let engine = engine_identity(&response.server);
        if self
            .runtime_server
            .as_ref()
            .is_some_and(|server| server != &response.server)
        {
            self.reset_runtime_sequences();
        }
        self.model.observe_server(&ServerHello {
            protocol: response.protocol,
            granted_role: response.granted_role,
            permissions: response.permissions.clone(),
            capabilities_digest: response.capabilities.digest.clone(),
            engine,
            current_revision: response.current_revision,
            resume,
        })?;
        self.session = Some(Session {
            protocol: response.protocol,
            granted_role: response.granted_role,
            permissions: response.permissions,
            server: response.server,
            capabilities_digest: response.capabilities.digest,
        });
        for record in self.commands.values_mut() {
            if record.status.is_active() {
                record.command.protocol = response.protocol;
            }
        }
        for item in &mut self.outbound {
            if let Outbound::Command(command) = item {
                command.protocol = response.protocol;
            }
        }
        let mode = if resume {
            SyncMode::Resume
        } else {
            SyncMode::Snapshot
        };
        self.state = ConnectionState::Synchronizing {
            mode,
            target_revision: response.current_revision,
        };
        if resume
            && self
                .model
                .reconnect_cursor()
                .map(|cursor| cursor.revision.get())
                == Some(response.current_revision)
        {
            self.finish_sync();
        }
        Ok(())
    }

    /// Installs an authoritative snapshot through [`ClientModel`].
    ///
    /// # Errors
    ///
    /// Requires synchronization and exact session identity/revision.
    pub fn apply_snapshot(&mut self, snapshot: SnapshotMessage) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            target_revision, ..
        } = self.state
        else {
            return Err(self.invalid_state("apply a snapshot"));
        };
        let session = self
            .session
            .as_ref()
            .ok_or(ClientError::InvalidSnapshot("snapshot has no session"))?;
        if snapshot.engine != engine_identity(&session.server) {
            return Err(ClientError::InvalidSnapshot(
                "snapshot identity does not match the session",
            ));
        }
        if snapshot.revision != target_revision {
            return Err(ClientError::InvalidSnapshot(
                "snapshot revision does not match the handshake",
            ));
        }
        self.model.install_snapshot(ProjectSnapshot::from_protocol(
            self.config.project_id,
            snapshot,
        ))?;
        // Every title the snapshot announced is completed by the
        // `title_state` records that follow it, and an announced playlist by
        // its `playlist_state`; until then the client keeps synchronizing.
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one title the snapshot announced with its `title_state`.
    ///
    /// Title states arrive only while synchronizing from a snapshot, after
    /// the snapshot and before any event. The last one finishes the sync.
    ///
    /// # Errors
    ///
    /// A title state in any other state, at another revision, or that does
    /// not complete an announced title with the announced field revision
    /// forces a fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_title_state(&mut self, state: TitleStateMessage) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || self.model.apply_title_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes the snapshot with the `playlist_state` it announced.
    ///
    /// The playlist state arrives only while synchronizing from a snapshot,
    /// after the snapshot's title states and before any event.
    ///
    /// # Errors
    ///
    /// A playlist state in any other state, at another revision, that the
    /// snapshot did not announce, or before every announced title state
    /// forces a fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_playlist_state(&mut self, state: PlaylistStateMessage) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !self.titles_complete()
            || self.model.apply_playlist_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one grading item the snapshot announced with its
    /// `grade_state`.
    ///
    /// Grade states arrive only while synchronizing from a snapshot, after
    /// the snapshot's title states and playlist state and before any event.
    ///
    /// # Errors
    ///
    /// A grade state in any other state, at another revision, that the
    /// snapshot did not announce, out of order, or before the title and
    /// playlist states forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_grade_state(&mut self, state: GradeStateMessage) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let playlist_complete = self
            .model
            .state()
            .is_none_or(fm_ui_model::ProjectState::playlist_complete);
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !self.titles_complete()
            || !playlist_complete
            || self.model.apply_grade_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one title's animation the snapshot announced with its
    /// `title_animation_state`.
    ///
    /// Animation states arrive only while synchronizing from a snapshot,
    /// one per announced title after the title, playlist, and grade states
    /// and before any event.
    ///
    /// # Errors
    ///
    /// An animation state in any other state, at another revision, for a
    /// title the snapshot did not announce or already completed, or before
    /// the earlier follow-on records forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_title_animation_state(
        &mut self,
        state: TitleAnimationStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete() && state.playlist_complete() && state.grading_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_title_animation_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one title's layout the snapshot announced with its
    /// `title_layout_state` (Protocol 2.30).
    ///
    /// Layout states arrive only while synchronizing from a snapshot, one
    /// per announced title after every `title_animation_state` and before
    /// any event.
    ///
    /// # Errors
    ///
    /// A layout state in any other state, at another revision, for a title
    /// the snapshot did not announce or already completed, or before the
    /// earlier follow-on records forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_title_layout_state(
        &mut self,
        state: TitleLayoutStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_title_layout_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one Scene input's animation the snapshot announced with
    /// its `scene_animation_state` (Protocol 2.31).
    ///
    /// Scene animation states arrive only while synchronizing from a
    /// snapshot, one per announced Scene input after every
    /// `title_layout_state` and before any event.
    ///
    /// # Errors
    ///
    /// A scene animation state in any other state, at another revision, for
    /// a Scene input the snapshot did not announce or already completed, or
    /// before the earlier follow-on records forces a fresh-snapshot
    /// reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_scene_animation_state(
        &mut self,
        state: SceneAnimationStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_scene_animation_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one scene's layout the snapshot announced with its
    /// `scene_layout_state` (Protocol 2.31; keyed by scene since 2.35), and
    /// the layout of every Scene input showing it.
    ///
    /// Scene layout states arrive only while synchronizing from a
    /// snapshot, one per scene of its `layout_scenes` list after every
    /// `scene_animation_state` and before any event.
    ///
    /// # Errors
    ///
    /// A scene layout state in any other state, at another revision, for a
    /// scene the snapshot did not announce or already completed, or before
    /// the earlier follow-on records forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_scene_layout_state(
        &mut self,
        state: SceneLayoutStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_scene_layout_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one library preset the snapshot announced with its
    /// `animation_preset_state` (Protocol 2.34).
    ///
    /// Animation preset states arrive only while synchronizing from a
    /// snapshot, in library order after every other follow-on record and
    /// before any event.
    ///
    /// # Errors
    ///
    /// An animation preset state in any other state, at another revision,
    /// beyond the announced count, duplicating a preset, or before the
    /// earlier follow-on records forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_animation_preset_state(
        &mut self,
        state: AnimationPresetStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_animation_preset_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one media input the snapshot announced with its
    /// `media_state` (Protocol 2.37).
    ///
    /// Media states arrive only while synchronizing from a snapshot, in
    /// ascending input order after every other follow-on record and before
    /// any event.
    ///
    /// # Errors
    ///
    /// A media state in any other state, at another revision, beyond the
    /// announced count, out of order, or before the earlier follow-on records
    /// forces a fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_media_state(
        &mut self,
        state: fm_protocol::MediaStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_media_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Adds one `cue_list_state` the installed snapshot announced
    /// (Protocol 2.50). Cue list states arrive only while synchronizing from
    /// a snapshot, after its media states and before any event.
    ///
    /// # Errors
    ///
    /// A cue list state in any other state, at another revision, beyond the
    /// announced count, or before the earlier follow-on records forces a
    /// fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_cue_list_state(
        &mut self,
        state: fm_protocol::CueListStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_cue_list_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Adds one `script_state` the installed snapshot announced (Protocol
    /// 2.58). Script states arrive only while synchronizing from a snapshot,
    /// after its cue list states and before any event.
    ///
    /// # Errors
    ///
    /// A script state in any other state, at another revision, beyond the
    /// announced count, or before the earlier follow-on records forces a
    /// fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_script_state(
        &mut self,
        state: fm_protocol::ScriptStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_script_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Adds one `shortcut_state` the installed snapshot announced
    /// (Protocol 2.72). Shortcut states arrive only while synchronizing
    /// from a snapshot, after its script states and before any event.
    ///
    /// # Errors
    ///
    /// A shortcut state in any other state, at another revision, beyond the
    /// announced count, or before the earlier follow-on records forces a
    /// fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_shortcut_state(
        &mut self,
        state: fm_protocol::ShortcutStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
                && state.scripts_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_shortcut_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Applies one `still_state` that completes the installed snapshot
    /// (IN-008, IN-009); it follows every shortcut state.
    ///
    /// # Errors
    ///
    /// A record out of order or unannounced forces a fresh snapshot.
    pub fn apply_still_state(
        &mut self,
        state: fm_protocol::StillStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
                && state.scripts_complete()
                && state.shortcuts_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_still_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Adds one `media_list_state` the installed snapshot announced
    /// (IN-006). Media list states arrive only while synchronizing from a
    /// snapshot, after its still states and before any event.
    ///
    /// # Errors
    ///
    /// A media list state in any other state, at another revision, beyond
    /// the announced count, or before the earlier follow-on records forces a
    /// fresh-snapshot reconnect ([`ClientError::ResyncRequired`]).
    pub fn apply_media_list_state(
        &mut self,
        state: fm_protocol::MediaListStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
                && state.scripts_complete()
                && state.shortcuts_complete()
                && state.stills_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_media_list_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    /// Completes one input strip's DSP chain the snapshot announced with
    /// its `audio_strip_dsp_state` (Protocol 2.75).
    ///
    /// Strip DSP states arrive only while synchronizing from a snapshot, in
    /// ascending input order after every other follow-on record and before
    /// any event.
    ///
    /// # Errors
    ///
    /// A strip DSP state in any other state, at another revision, beyond the
    /// announced count, out of order, or before the earlier follow-on
    /// records forces a fresh-snapshot reconnect
    /// ([`ClientError::ResyncRequired`]).
    pub fn apply_audio_strip_dsp_state(
        &mut self,
        state: fm_protocol::AudioStripDspStateMessage,
    ) -> Result<(), ClientError> {
        let ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision,
        } = self.state
        else {
            let expected = self
                .model
                .reconnect_cursor()
                .map_or(0, |cursor| cursor.revision.get());
            return self.require_resync(expected, state.revision);
        };
        let received_revision = state.revision;
        let earlier_complete = self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
                && state.scripts_complete()
                && state.shortcuts_complete()
                && state.stills_complete()
                && state.media_lists_complete()
        });
        if self
            .model
            .reconnect_cursor()
            .is_none_or(|cursor| cursor.revision.get() != target_revision)
            || !earlier_complete
            || self.model.apply_audio_strip_dsp_state(state).is_err()
        {
            return self.require_resync(target_revision, received_revision);
        }
        if self.snapshot_complete() {
            self.finish_sync();
        }
        Ok(())
    }

    fn titles_complete(&self) -> bool {
        self.model
            .state()
            .is_none_or(fm_ui_model::ProjectState::titles_complete)
    }

    /// Whether the installed snapshot has every title, playlist, grade,
    /// title animation, title layout, scene animation, scene layout, and
    /// animation preset state it announced.
    fn snapshot_complete(&self) -> bool {
        self.model.state().is_none_or(|state| {
            state.titles_complete()
                && state.playlist_complete()
                && state.grading_complete()
                && state.title_animations_complete()
                && state.title_layouts_complete()
                && state.scene_animations_complete()
                && state.scene_layouts_complete()
                && state.animation_presets_complete()
                && state.media_complete()
                && state.cue_lists_complete()
                && state.scripts_complete()
                && state.shortcuts_complete()
                && state.stills_complete()
                && state.media_lists_complete()
                && state.audio_strip_dsps_complete()
        })
    }

    /// Applies exactly the next ordered event through [`ClientModel`].
    ///
    /// # Errors
    ///
    /// Forward gaps and model-invalid events force a fresh-snapshot reconnect.
    pub fn apply_event(&mut self, event: EventMessage) -> Result<(), ClientError> {
        let target_revision = match self.state {
            ConnectionState::Synchronizing {
                mode: SyncMode::Resume,
                target_revision,
            } => Some(target_revision),
            ConnectionState::Ready => None,
            // An event before the snapshot's title and playlist states have
            // all arrived breaks the snapshot's contiguity.
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                target_revision,
            } if !self.snapshot_complete()
                && self
                    .model
                    .reconnect_cursor()
                    .is_some_and(|cursor| cursor.revision.get() == target_revision) =>
            {
                return self
                    .require_resync(target_revision.saturating_add(1), event.cursor.revision);
            }
            _ => return Err(self.invalid_state("apply an event")),
        };
        let current = self
            .model
            .reconnect_cursor()
            .ok_or(ClientError::InvalidSnapshot("event has no base snapshot"))?;
        self.session
            .as_ref()
            .ok_or(ClientError::InvalidSnapshot("event has no session"))?;
        let expected_revision = current
            .revision
            .get()
            .checked_add(1)
            .ok_or(ClientError::InvalidSnapshot("event revision overflow"))?;
        if event.cursor.revision < expected_revision {
            return Err(ClientError::StaleEvent {
                expected_revision,
                received_revision: event.cursor.revision,
            });
        }
        if event.cursor.engine != current.engine
            || event.cursor.revision > expected_revision
            || target_revision.is_some_and(|target| event.cursor.revision > target)
        {
            return self.require_resync(expected_revision, event.cursor.revision);
        }
        let received_revision = event.cursor.revision;
        if let Err(error) = self.model.apply_event(DurableProjectEvent::from_protocol(
            self.config.project_id,
            event,
        )) {
            self.force_snapshot = true;
            self.state = ConnectionState::ResyncRequired {
                expected_revision,
                received_revision,
            };
            return Err(ClientError::Model(error));
        }
        if target_revision == Some(received_revision) {
            self.finish_sync();
        }
        Ok(())
    }

    /// Observes the next independently ordered runtime lifecycle event.
    ///
    /// Runtime events may reference any retained durable revision, but never
    /// advance the durable cursor or trigger durable snapshot recovery.
    ///
    /// # Errors
    ///
    /// Rejects events outside an active replicated session, for another server,
    /// ahead of the durable cursor, from an older generation than one already
    /// observed, or with a gap in their generation sequence.
    pub fn apply_runtime_event(&mut self, event: RuntimeEventMessage) -> Result<(), ClientError> {
        if !matches!(
            self.state,
            ConnectionState::Synchronizing {
                mode: SyncMode::Resume,
                ..
            } | ConnectionState::Ready
        ) {
            return Err(self.invalid_state("apply a runtime event"));
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| self.invalid_state("apply a runtime event"))?;
        if event.server != session.server {
            return Err(ClientError::RuntimeIdentityMismatch {
                expected: Box::new(session.server.clone()),
                received: Box::new(event.server),
            });
        }
        let current_revision = self
            .model
            .reconnect_cursor()
            .ok_or(ClientError::InvalidSnapshot(
                "runtime event has no base snapshot",
            ))?
            .revision
            .get();
        if event.revision > current_revision {
            return Err(ClientError::RuntimeRevisionAhead {
                current_revision,
                received_revision: event.revision,
            });
        }

        // The server numbers one generation's events across every connection,
        // so the first event observed after (re)connecting may continue a
        // generation whose earlier events were sent before this connection
        // existed. It becomes the baseline; everything after it is contiguous.
        let expected_sequence = match self.runtime_sequence {
            None => event.sequence.max(1),
            Some((generation, _)) if event.generation < generation => {
                return Err(ClientError::RuntimeGenerationRegressed {
                    current_generation: generation,
                    received_generation: event.generation,
                });
            }
            Some((generation, sequence)) if event.generation == generation => {
                sequence.saturating_add(1)
            }
            Some(_) => 1,
        };
        if event.sequence != expected_sequence {
            return Err(ClientError::RuntimeSequenceGap {
                generation: event.generation,
                expected_sequence,
                received_sequence: event.sequence,
            });
        }
        if let RuntimeLifecycleEvent::Realized {
            domain,
            manual_transition,
            fade_to_black,
        } = &event.event
            && domain == "switcher"
        {
            self.model.apply_runtime_realization(RuntimeRealization {
                project_id: self.config.project_id,
                engine: engine_identity(&event.server),
                revision: Revision::new(event.revision),
                generation: event.generation,
                sequence: event.sequence,
                manual_transition: ModelManualTransitionStatus::from_protocol(*manual_transition),
                fade_to_black: *fade_to_black,
            })?;
        }
        if let RuntimeLifecycleEvent::Media { status, .. } = &event.event {
            self.model.apply_runtime_media(status.as_ref().clone())?;
        }
        if let RuntimeLifecycleEvent::InputAvailability { input, offline } = &event.event {
            self.model
                .apply_runtime_input_availability(input.to_domain(), *offline)?;
        }
        if let RuntimeLifecycleEvent::DisplayOutputStatus(status) = &event.event {
            self.model
                .apply_runtime_display_output_status(status.clone())?;
        }
        if let RuntimeLifecycleEvent::DisplayInventory(inventory) = &event.event {
            self.model.apply_display_inventory(inventory.clone());
        }
        if let RuntimeLifecycleEvent::LiveLanStatus(status) = &event.event {
            self.model.apply_runtime_live_lan_status(status.clone())?;
        }
        self.apply_runtime_source_status(&event.event)?;
        self.runtime_server = Some(event.server);
        self.runtime_sequence = Some((event.generation, event.sequence));
        Ok(())
    }

    /// Applies a capture, system audio, or network stream input's status
    /// and the capture device inventory.
    fn apply_runtime_source_status(
        &mut self,
        event: &RuntimeLifecycleEvent,
    ) -> Result<(), ClientError> {
        match event {
            RuntimeLifecycleEvent::CaptureStatus { input, status } => {
                self.model
                    .apply_runtime_capture_status(input.to_domain(), (**status).clone())?;
            }
            RuntimeLifecycleEvent::CaptureDevices(inventory) => {
                self.model.apply_capture_inventory(inventory.clone());
            }
            RuntimeLifecycleEvent::SystemAudioStatus { input, status } => {
                self.model
                    .apply_runtime_system_audio_status(input.to_domain(), (**status).clone())?;
            }
            RuntimeLifecycleEvent::SlideshowPage { input, status } => {
                self.model
                    .apply_runtime_slideshow_page(input.to_domain(), (**status).clone())?;
            }
            RuntimeLifecycleEvent::NetworkStreamStatus { input, status } => {
                self.model
                    .apply_runtime_network_stream_status(input.to_domain(), (**status).clone())?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Adds a command with a monotonic client-local ID and explicit replay key.
    /// [`CommandPayload::SelectPreview`] is also tracked as optimistic intent
    /// by the UI model. A key remains reserved while its command is active or
    /// retained in terminal local history. Local eviction forgets the key, but
    /// callers must still generate globally unique keys for at least as long as
    /// the server can retain receipts.
    ///
    /// # Errors
    ///
    /// Requires ready state, queue space, and a unique nonblank idempotency key.
    #[allow(clippy::too_many_lines)] // one optimistic arm per command payload
    pub fn queue_command(
        &mut self,
        payload: CommandPayload,
        idempotency_key: impl Into<String>,
        expected_revision: Option<u64>,
        deadline_ms: Option<u64>,
    ) -> Result<CommandMessage, ClientError> {
        if self.state != ConnectionState::Ready {
            return Err(self.invalid_state("queue a command"));
        }
        let protocol = self
            .session
            .as_ref()
            .ok_or_else(|| self.invalid_state("queue a command"))?
            .protocol;
        self.ensure_queue_space()?;
        let idempotency_key = idempotency_key.into();
        if idempotency_key.trim().is_empty() {
            return Err(ClientError::EmptyIdempotencyKey);
        }
        if self.idempotency_keys.contains(&idempotency_key) {
            return Err(ClientError::DuplicateIdempotencyKey(idempotency_key));
        }
        let next = self
            .next_command_id
            .checked_add(1)
            .ok_or(ClientError::CommandIdExhausted)?;
        let client_id = &self.config.client_id;
        let sequence = self.next_command_id;
        let command = CommandMessage {
            protocol,
            id: format!("{client_id}:{sequence}"),
            idempotency_key: idempotency_key.clone(),
            expected_revision,
            deadline_ms,
            payload,
        };
        let optimistic = match &command.payload {
            CommandPayload::SelectPreview { input } => {
                Some(OptimisticChange::DesiredPreview(input.to_domain()))
            }
            CommandPayload::ReorderInputs { .. }
            | CommandPayload::MediaList { .. }
            | CommandPayload::RenameInput { .. }
            | CommandPayload::AddInput { .. }
            | CommandPayload::RemoveInput { .. }
            | CommandPayload::SetInputAudioStrip { .. }
            | CommandPayload::Cut
            | CommandPayload::Fade { .. }
            | CommandPayload::AlphaFade { .. }
            | CommandPayload::Slide { .. }
            | CommandPayload::Zoom { .. }
            | CommandPayload::Transition { .. }
            | CommandPayload::SetTransitionFavorite { .. }
            | CommandPayload::Media { .. }
            | CommandPayload::PageNavigate { .. }
            | CommandPayload::InputLifecycle(_)
            | CommandPayload::SceneLayer(_)
            | CommandPayload::AudioMixer(_)
            | CommandPayload::SetFadeToBlackOutput { .. }
            | CommandPayload::SetFadeToBlackOutputDuration { .. }
            | CommandPayload::SetFadeToBlackCurve { .. }
            | CommandPayload::SetDisplayOutput { .. }
            | CommandPayload::ClearDisplayOutput { .. }
            | CommandPayload::SetLiveLan { .. }
            | CommandPayload::SetNetworkStream { .. }
            | CommandPayload::SetCaptureSettings { .. }
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
            | CommandPayload::StreamRetry { .. }
            | CommandPayload::StreamStartAll
            | CommandPayload::StreamStopAll
            | CommandPayload::SetStreamTarget { .. }
            | CommandPayload::RemoveStreamTarget { .. }
            | CommandPayload::RecordStart { .. }
            | CommandPayload::RecordStop { .. }
            | CommandPayload::Undo { .. }
            | CommandPayload::Redo { .. }
            | CommandPayload::SetEditLock { .. }
            | CommandPayload::SetTitleFields { .. }
            | CommandPayload::TakeTitle { .. }
            | CommandPayload::TitleOffAir { .. }
            | CommandPayload::SetPlaylist { .. }
            | CommandPayload::PlaylistControl { .. }
            | CommandPayload::PlaylistAdvance { .. }
            | CommandPayload::SetCueList { .. }
            | CommandPayload::RemoveCueList { .. }
            | CommandPayload::CueControl { .. }
            | CommandPayload::CueAdvance { .. }
            | CommandPayload::SetScript { .. }
            | CommandPayload::RemoveScript { .. }
            | CommandPayload::RunScript { .. }
            | CommandPayload::StopScript { .. }
            | CommandPayload::SetShortcut { .. }
            | CommandPayload::RemoveShortcut { .. }
            | CommandPayload::FireShortcut { .. }
            | CommandPayload::SetInputGrade { .. }
            | CommandPayload::SetLayerGrade { .. }
            | CommandPayload::SetProgramGrade { .. }
            | CommandPayload::AddLibraryGrade { .. }
            | CommandPayload::UpdateLibraryGrade { .. }
            | CommandPayload::RemoveLibraryGrade { .. }
            | CommandPayload::ImportGradeLut { .. }
            | CommandPayload::AddAnimationPreset { .. }
            | CommandPayload::RenameAnimationPreset { .. }
            | CommandPayload::RemoveAnimationPreset { .. }
            | CommandPayload::ApplyTitleAnimationUserPreset { .. }
            | CommandPayload::ApplySceneAnimationUserPreset { .. }
            | CommandPayload::RemoveGradeLut { .. }
            | CommandPayload::SetTitleAnimation { .. }
            | CommandPayload::ApplyTitleAnimationPreset { .. }
            | CommandPayload::StretchTitleAnimation { .. }
            | CommandPayload::SetSceneAnimation { .. }
            | CommandPayload::ApplySceneAnimationPreset { .. }
            | CommandPayload::StretchSceneAnimation { .. } => None,
        };
        self.model
            .track_command(CommandId::new(command.id.clone()), optimistic)?;
        self.next_command_id = next;
        self.idempotency_keys.insert(idempotency_key);
        self.commands.insert(
            command.id.clone(),
            CommandRecord {
                command: command.clone(),
                status: CommandStatus::Queued,
            },
        );
        self.outbound.push_back(Outbound::Command(command.clone()));
        Ok(command)
    }

    /// Requeues an unresolved command with its original idempotency envelope.
    ///
    /// # Errors
    ///
    /// Requires ready state, queue space, and a known unresolved command.
    pub fn retry_command(&mut self, id: &str) -> Result<(), ClientError> {
        if self.state != ConnectionState::Ready {
            return Err(self.invalid_state("retry a command"));
        }
        self.ensure_queue_space()?;
        let record = self
            .commands
            .get_mut(id)
            .ok_or_else(|| ClientError::UnknownCommand(id.to_owned()))?;
        match &record.status {
            CommandStatus::Completed(_) => {
                return Err(ClientError::CommandAlreadyCompleted(id.to_owned()));
            }
            CommandStatus::TerminalUncertain(_) => {
                return Err(ClientError::CommandTerminalUncertain(id.to_owned()));
            }
            CommandStatus::Queued | CommandStatus::Sent => {}
        }
        if !matches!(record.status, CommandStatus::Queued) {
            record.status = CommandStatus::Queued;
            self.outbound
                .push_back(Outbound::Command(record.command.clone()));
        }
        Ok(())
    }

    /// Enqueues a protocol heartbeat carrying the last fully applied cursor.
    /// `sent_at_ms` is supplied by the caller's clock.
    ///
    /// # Errors
    ///
    /// Requires an established current-protocol session, queue space, and sequence capacity.
    pub fn queue_heartbeat(&mut self, sent_at_ms: u64) -> Result<HeartbeatMessage, ClientError> {
        if !matches!(
            self.state,
            ConnectionState::Synchronizing { .. } | ConnectionState::Ready
        ) {
            return Err(self.invalid_state("queue a heartbeat"));
        }
        if self.expected_heartbeat_sequence.is_some() {
            return Err(ClientError::HeartbeatAcknowledgementPending);
        }
        self.ensure_queue_space()?;
        let next = self
            .next_heartbeat_sequence
            .checked_add(1)
            .ok_or(ClientError::HeartbeatSequenceExhausted)?;
        let heartbeat = HeartbeatMessage {
            server: self
                .session
                .as_ref()
                .ok_or_else(|| self.invalid_state("queue a heartbeat"))?
                .server
                .clone(),
            sequence: self.next_heartbeat_sequence,
            sent_at_ms,
            last_applied: self.last_applied_cursor(),
        };
        self.next_heartbeat_sequence = next;
        self.expected_heartbeat_sequence = Some(heartbeat.sequence);
        self.outbound
            .push_back(Outbound::Heartbeat(heartbeat.clone()));
        Ok(heartbeat)
    }

    /// Validates the server identity and sequence of one heartbeat acknowledgement.
    ///
    /// # Errors
    ///
    /// Rejects acknowledgements outside an established session, from another server,
    /// or for an unexpected heartbeat sequence.
    pub fn accept_heartbeat_acknowledgement(
        &mut self,
        acknowledgement: &HeartbeatAcknowledgementMessage,
    ) -> Result<(), ClientError> {
        if !matches!(
            self.state,
            ConnectionState::Synchronizing { .. } | ConnectionState::Ready
        ) {
            return Err(self.invalid_state("accept a heartbeat acknowledgement"));
        }
        if self.session.as_ref().map(|session| &session.server) != Some(&acknowledgement.server) {
            return Err(ClientError::InvalidHeartbeatAcknowledgement(
                "heartbeat acknowledgement server identity does not match the session",
            ));
        }
        if self.expected_heartbeat_sequence != Some(acknowledgement.heartbeat_sequence) {
            return Err(ClientError::InvalidHeartbeatAcknowledgement(
                "heartbeat acknowledgement sequence does not match the pending heartbeat",
            ));
        }
        self.expected_heartbeat_sequence = None;
        Ok(())
    }

    /// Removes the oldest item and marks dequeued commands as sent.
    pub fn pop_outbound(&mut self) -> Option<Outbound> {
        if matches!(self.outbound.front(), Some(Outbound::Command(_)))
            && self.state != ConnectionState::Ready
        {
            return None;
        }
        let item = self.outbound.pop_front()?;
        if let Outbound::Command(command) = &item
            && let Some(record) = self.commands.get_mut(&command.id)
            && !record.status.is_terminal()
        {
            record.status = CommandStatus::Sent;
        }
        Some(item)
    }

    /// Reconciles a result through both command records and the UI model.
    ///
    /// # Errors
    ///
    /// Rejects unknown command IDs and conflicting duplicate results.
    pub fn reconcile_result(&mut self, result: CommandResult) -> Result<Intake, ClientError> {
        let id = result_id(&result);
        let Some(record) = self.commands.get(id) else {
            return self.handle_unknown_result_collision(&result);
        };
        if let CommandStatus::Completed(previous) = &record.status {
            return if previous == &result {
                Ok(Intake::DuplicateResult)
            } else {
                Err(ClientError::ConflictingResult(id.to_owned()))
            };
        }
        if matches!(record.status, CommandStatus::TerminalUncertain(_)) {
            return Err(ClientError::CommandTerminalUncertain(id.to_owned()));
        }
        self.model.reconcile_command(&result)?;
        let completed_id = id.to_owned();
        if let Some(record) = self.commands.get_mut(id) {
            record.status = CommandStatus::Completed(result);
        }
        self.outbound.retain(
            |item| !matches!(item, Outbound::Command(command) if command.id == completed_id),
        );
        self.completed_command_ids.push_back(completed_id);
        self.prune_completed_commands();
        Ok(Intake::ResultReconciled)
    }

    fn handle_unknown_result_collision<T>(
        &mut self,
        result: &CommandResult,
    ) -> Result<T, ClientError> {
        let received_command_id = result_id(result).to_owned();
        let affected_command_ids = self
            .commands
            .iter()
            .filter(|(_, record)| matches!(record.status, CommandStatus::Sent))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if affected_command_ids.is_empty() {
            return Err(ClientError::UnknownCommand(received_command_id));
        }

        let current_revision = self
            .last_applied_cursor()
            .map_or(0, |cursor| cursor.revision);
        for id in &affected_command_ids {
            self.model.reconcile_command(&CommandResult::Rejected {
                id: id.clone(),
                code: "idempotency_replay_collision".to_owned(),
                message: format!(
                    "server returned retained receipt for unknown command {received_command_id:?}"
                ),
                fields: Vec::new(),
                current_revision,
                retryable: false,
                reason: None,
            })?;
            if let Some(record) = self.commands.get_mut(id) {
                record.status = CommandStatus::TerminalUncertain(
                    CommandUncertainty::IdempotencyReplayCollision {
                        received_command_id: received_command_id.clone(),
                    },
                );
            }
            self.completed_command_ids.push_back(id.clone());
        }
        self.outbound.retain(|item| {
            !matches!(item, Outbound::Command(command) if affected_command_ids.contains(&command.id))
        });
        self.prune_completed_commands();
        self.force_snapshot = true;
        self.state = ConnectionState::ResyncRequired {
            expected_revision: current_revision.saturating_add(1),
            received_revision: result_revision(result),
        };
        Err(ClientError::IdempotencyReplayCollision {
            received_command_id,
            affected_command_ids,
        })
    }

    fn prune_completed_commands(&mut self) {
        while self.completed_command_ids.len() > self.config.completed_command_capacity {
            let id = self
                .completed_command_ids
                .pop_front()
                .expect("completed command retention must have an oldest record");
            let record = self
                .commands
                .remove(&id)
                .expect("completed command index must reference a retained record");
            debug_assert!(record.status.is_terminal());
            self.idempotency_keys
                .remove(&record.command.idempotency_key);
        }
    }

    fn apply_gap(&mut self, gap: &DurableGap) -> Result<(), ClientError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| self.invalid_state("apply a durable gap"))?;
        if gap.server != session.server {
            return Err(ClientError::InvalidHandshake(
                "durable gap identity does not match the session",
            ));
        }
        self.require_resync(
            gap.requested_after_revision.saturating_add(1),
            gap.available_from_revision,
        )
    }

    fn finish_sync(&mut self) {
        self.state = ConnectionState::Ready;
        self.reconnect_attempt = 0;
        self.force_snapshot = false;
    }

    fn reset_runtime_sequences(&mut self) {
        self.runtime_server = None;
        self.runtime_sequence = None;
    }

    fn require_resync<T>(
        &mut self,
        expected_revision: u64,
        received_revision: u64,
    ) -> Result<T, ClientError> {
        self.force_snapshot = true;
        self.state = ConnectionState::ResyncRequired {
            expected_revision,
            received_revision,
        };
        Err(ClientError::ResyncRequired {
            expected_revision,
            received_revision,
        })
    }

    fn ensure_queue_space(&self) -> Result<(), ClientError> {
        if self.outbound.len() >= self.config.outbound_capacity {
            Err(ClientError::QueueFull {
                capacity: self.config.outbound_capacity,
            })
        } else {
            Ok(())
        }
    }

    fn invalid_state(&self, operation: &'static str) -> ClientError {
        ClientError::InvalidState {
            operation,
            state: self.state.clone(),
        }
    }
}

fn engine_identity(server: &ServerIdentity) -> EngineIdentity {
    EngineIdentity {
        engine_id: server.engine_id.clone(),
        state_epoch: server.state_epoch,
        log_id: server.log_id.clone(),
    }
}

fn result_id(result: &CommandResult) -> &str {
    match result {
        CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
    }
}

fn result_revision(result: &CommandResult) -> u64 {
    match result {
        CommandResult::Accepted { revision, .. } => *revision,
        CommandResult::Rejected {
            current_revision, ..
        } => *current_revision,
    }
}
