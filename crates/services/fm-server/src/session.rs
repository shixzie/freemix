use std::{collections::VecDeque, error::Error, fmt};

use fm_auth::{AuthorizationDenial, CommandClass, Policy, Principal};
use fm_control::RequiredClasses;
use fm_protocol::{
    CommandMessage, DiagnosticsRequest, EngineIdentity, EventCursor, ProtocolVersion,
    ScenePreviewRequest, TitlePreviewRequest,
};

use crate::{RateLimit, SessionLimits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    SlowClient,
    HeartbeatTimeout,
    ServerShutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    Connected,
    Disconnected(DisconnectReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heartbeat {
    pub last_applied: Option<EventCursor>,
    pub clock_sample_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatState {
    pub last_applied: Option<EventCursor>,
    pub client_clock_sample_ms: Option<u64>,
    pub last_received_at_ms: u64,
    pub received_total: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionAccounting {
    pub inbound_commands_admitted_total: u64,
    pub inbound_commands_inflight: usize,
    pub outbound_messages_queued_total: u64,
    pub outbound_messages_queued: usize,
    pub outbound_bytes_queued: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowCounter {
    started_at_ms: u64,
    count: u64,
}

impl WindowCounter {
    const fn new(started_at_ms: u64) -> Self {
        Self {
            started_at_ms,
            count: 0,
        }
    }

    fn refresh(&mut self, now_ms: u64, window_ms: u64) {
        // A clock that stepped backwards opens a fresh window; otherwise the
        // spent window would refuse every message until the clock caught up.
        if now_ms < self.started_at_ms || now_ms - self.started_at_ms >= window_ms {
            self.started_at_ms = now_ms;
            self.count = 0;
        }
    }

    fn has_capacity(&mut self, now_ms: u64, limit: RateLimit) -> bool {
        self.refresh(now_ms, limit.window_ms);
        self.count < limit.maximum
    }

    fn record(&mut self) {
        self.count = self.count.saturating_add(1);
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    state: SessionState,
    protocol: ProtocolVersion,
    engine: EngineIdentity,
    current_revision: u64,
    principal: Principal,
    policy: Policy,
    limits: SessionLimits,
    inbound_window: WindowCounter,
    outbound_window: WindowCounter,
    preview_window: WindowCounter,
    outbound_sizes: VecDeque<usize>,
    accounting: SessionAccounting,
    heartbeat: HeartbeatState,
}

impl Session {
    pub(crate) fn new(
        protocol: ProtocolVersion,
        engine: EngineIdentity,
        current_revision: u64,
        principal: Principal,
        policy: Policy,
        limits: SessionLimits,
        now_ms: u64,
    ) -> Self {
        Self {
            state: SessionState::Connected,
            protocol,
            engine,
            current_revision,
            principal,
            policy,
            limits,
            inbound_window: WindowCounter::new(now_ms),
            outbound_window: WindowCounter::new(now_ms),
            preview_window: WindowCounter::new(now_ms),
            outbound_sizes: VecDeque::new(),
            accounting: SessionAccounting::default(),
            heartbeat: HeartbeatState {
                last_applied: None,
                client_clock_sample_ms: None,
                last_received_at_ms: now_ms,
                received_total: 0,
            },
        }
    }

    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol
    }

    #[must_use]
    pub const fn current_revision(&self) -> u64 {
        self.current_revision
    }

    #[must_use]
    pub fn engine(&self) -> &EngineIdentity {
        &self.engine
    }

    #[must_use]
    pub const fn accounting(&self) -> SessionAccounting {
        self.accounting
    }

    #[must_use]
    pub fn heartbeat(&self) -> &HeartbeatState {
        &self.heartbeat
    }

    /// Authorizes and accounts for one command before control-plane execution.
    ///
    /// `required` must be the classes the control plane resolves for this
    /// command at execution time ([`fm_control::ControlService::required_classes`]): the
    /// session authorizes its scoped role against exactly the classes
    /// fm-control would, including the class of the edit undo or redo
    /// targets. The caller must invoke [`Self::command_completed`] after
    /// execution.
    ///
    /// # Errors
    ///
    /// Returns a session, authorization, protocol, size, concurrency, or rate
    /// error when the command cannot be admitted.
    pub fn admit_command(
        &mut self,
        command: &CommandMessage,
        required: RequiredClasses,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.ensure_connected()?;
        required.authorize(&self.policy, &self.principal)?;

        if command.protocol != self.protocol {
            return Err(SessionError::ProtocolMismatch {
                expected: self.protocol,
                received: command.protocol,
            });
        }
        if encoded_bytes > self.limits.max_command_bytes {
            return Err(SessionError::CommandTooLarge {
                size: encoded_bytes,
                maximum: self.limits.max_command_bytes,
            });
        }
        if self.accounting.inbound_commands_inflight >= self.limits.max_inflight_commands {
            return Err(SessionError::TooManyInflightCommands);
        }
        if !self
            .inbound_window
            .has_capacity(now_ms, self.limits.inbound_commands)
        {
            return Err(SessionError::InboundRateLimited);
        }

        self.inbound_window.record();
        self.accounting.inbound_commands_admitted_total = self
            .accounting
            .inbound_commands_admitted_total
            .saturating_add(1);
        self.accounting.inbound_commands_inflight += 1;
        Ok(())
    }

    /// Authorizes and accounts for one read-only diagnostics query.
    ///
    /// # Errors
    ///
    /// Returns a session, authorization, protocol, size, or rate error when
    /// the diagnostics query cannot be admitted.
    pub fn admit_diagnostics(
        &mut self,
        request: &DiagnosticsRequest,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.ensure_connected()?;
        self.policy
            .authorize(&self.principal, CommandClass::ViewStatus)?;
        if request.protocol != self.protocol {
            return Err(SessionError::ProtocolMismatch {
                expected: self.protocol,
                received: request.protocol,
            });
        }
        if encoded_bytes > self.limits.max_command_bytes {
            return Err(SessionError::CommandTooLarge {
                size: encoded_bytes,
                maximum: self.limits.max_command_bytes,
            });
        }
        if !self
            .inbound_window
            .has_capacity(now_ms, self.limits.inbound_commands)
        {
            return Err(SessionError::InboundRateLimited);
        }
        self.inbound_window.record();
        Ok(())
    }

    /// Authorizes and accounts for one web access request (Protocol 2.71).
    ///
    /// Pairing a device grants a new user and revoking one removes it, so
    /// every web access request, even a status read that reveals the
    /// outstanding pairing code, requires [`CommandClass::ManageUsers`]. It
    /// counts against the inbound command rate.
    ///
    /// # Errors
    ///
    /// Returns a session, authorization, or rate error when the request
    /// cannot be admitted.
    pub fn admit_web_access(&mut self, now_ms: u64) -> Result<(), SessionError> {
        self.ensure_connected()?;
        self.policy
            .authorize(&self.principal, CommandClass::ManageUsers)?;
        if !self
            .inbound_window
            .has_capacity(now_ms, self.limits.inbound_commands)
        {
            return Err(SessionError::InboundRateLimited);
        }
        self.inbound_window.record();
        Ok(())
    }

    /// Authorizes and accounts for one exact title preview request.
    ///
    /// A preview renders a title template, which only someone who may edit
    /// it needs, so it requires [`CommandClass::EditProject`]. It counts
    /// against both the inbound command rate and the tighter
    /// [`SessionLimits::title_previews`] rate.
    ///
    /// # Errors
    ///
    /// Returns a session, authorization, protocol, size, or rate error when
    /// the preview cannot be admitted.
    pub fn admit_title_preview(
        &mut self,
        request: &TitlePreviewRequest,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.admit_preview(request.protocol, encoded_bytes, now_ms)
    }

    /// Authorizes and accounts for one exact scene preview request
    /// (Protocol 2.32), exactly like [`Self::admit_title_preview`]: it needs
    /// [`CommandClass::EditProject`] and shares the
    /// [`SessionLimits::title_previews`] rate with title previews.
    ///
    /// # Errors
    ///
    /// Returns a session, authorization, protocol, size, or rate error when
    /// the preview cannot be admitted.
    pub fn admit_scene_preview(
        &mut self,
        request: &ScenePreviewRequest,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.admit_preview(request.protocol, encoded_bytes, now_ms)
    }

    fn admit_preview(
        &mut self,
        protocol: ProtocolVersion,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.ensure_connected()?;
        self.policy
            .authorize(&self.principal, CommandClass::EditProject)?;
        if protocol != self.protocol {
            return Err(SessionError::ProtocolMismatch {
                expected: self.protocol,
                received: protocol,
            });
        }
        if encoded_bytes > self.limits.max_command_bytes {
            return Err(SessionError::CommandTooLarge {
                size: encoded_bytes,
                maximum: self.limits.max_command_bytes,
            });
        }
        if !self
            .inbound_window
            .has_capacity(now_ms, self.limits.inbound_commands)
        {
            return Err(SessionError::InboundRateLimited);
        }
        if !self
            .preview_window
            .has_capacity(now_ms, self.limits.title_previews)
        {
            return Err(SessionError::TitlePreviewRateLimited);
        }
        self.inbound_window.record();
        self.preview_window.record();
        Ok(())
    }

    /// Releases one in-flight command accounting slot.
    ///
    /// # Errors
    ///
    /// Returns an accounting error if no command is in flight.
    pub fn command_completed(&mut self) -> Result<(), SessionError> {
        if self.accounting.inbound_commands_inflight == 0 {
            return Err(SessionError::NoInflightCommand);
        }
        self.accounting.inbound_commands_inflight -= 1;
        Ok(())
    }

    /// Reserves bounded outbound queue capacity for one encoded message.
    ///
    /// Queue overflow disconnects the session as a slow client. Rate exhaustion
    /// is retryable and does not disconnect it.
    ///
    /// # Errors
    ///
    /// Returns a disconnected or outbound rate-limit error when the message
    /// cannot be queued.
    pub fn queue_outbound(
        &mut self,
        encoded_bytes: usize,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        self.ensure_connected()?;
        if !self
            .outbound_window
            .has_capacity(now_ms, self.limits.outbound_messages)
        {
            return Err(SessionError::OutboundRateLimited);
        }

        let Some(bytes) = self
            .accounting
            .outbound_bytes_queued
            .checked_add(encoded_bytes)
        else {
            self.disconnect(DisconnectReason::SlowClient);
            return Err(SessionError::Disconnected(DisconnectReason::SlowClient));
        };
        if self.accounting.outbound_messages_queued >= self.limits.max_outbound_messages
            || bytes > self.limits.max_outbound_bytes
        {
            self.disconnect(DisconnectReason::SlowClient);
            return Err(SessionError::Disconnected(DisconnectReason::SlowClient));
        }

        self.outbound_window.record();
        self.outbound_sizes.push_back(encoded_bytes);
        self.accounting.outbound_messages_queued += 1;
        self.accounting.outbound_bytes_queued = bytes;
        self.accounting.outbound_messages_queued_total = self
            .accounting
            .outbound_messages_queued_total
            .saturating_add(1);
        Ok(())
    }

    /// Releases the oldest outbound message after transport delivery.
    ///
    /// # Errors
    ///
    /// Returns an accounting error if the queue is empty.
    pub fn outbound_delivered(&mut self) -> Result<usize, SessionError> {
        self.release_outbound(0)
    }

    /// Releases one discarded outbound message by queue index.
    ///
    /// # Errors
    ///
    /// Returns an accounting error if the index is not queued.
    pub fn discard_outbound(&mut self, index: usize) -> Result<usize, SessionError> {
        self.release_outbound(index)
    }

    fn release_outbound(&mut self, index: usize) -> Result<usize, SessionError> {
        let Some(bytes) = self.outbound_sizes.remove(index) else {
            return Err(SessionError::NoQueuedOutboundMessage);
        };
        self.accounting.outbound_messages_queued -= 1;
        self.accounting.outbound_bytes_queued -= bytes;
        Ok(bytes)
    }

    /// Records the client's clock sample and optional latest applied cursor.
    ///
    /// # Errors
    ///
    /// Returns an identity or regression error for an invalid cursor, or a
    /// disconnected error after the session has closed.
    pub fn record_heartbeat(
        &mut self,
        heartbeat: Heartbeat,
        received_at_ms: u64,
    ) -> Result<(), SessionError> {
        self.ensure_connected()?;
        if let Some(cursor) = heartbeat.last_applied {
            if cursor.engine != self.engine {
                return Err(SessionError::CursorIdentityMismatch);
            }
            if self
                .heartbeat
                .last_applied
                .as_ref()
                .is_some_and(|previous| cursor.revision < previous.revision)
            {
                return Err(SessionError::CursorRegression);
            }
            self.heartbeat.last_applied = Some(cursor);
        }

        self.heartbeat.client_clock_sample_ms = Some(heartbeat.clock_sample_ms);
        self.heartbeat.last_received_at_ms = received_at_ms;
        self.heartbeat.received_total = self.heartbeat.received_total.saturating_add(1);
        Ok(())
    }

    /// Applies the configured heartbeat timeout.
    ///
    /// # Errors
    ///
    /// Returns a disconnected error if the session was already disconnected or
    /// reaches its heartbeat deadline.
    pub fn check_heartbeat(&mut self, now_ms: u64) -> Result<(), SessionError> {
        self.ensure_connected()?;
        if now_ms.saturating_sub(self.heartbeat.last_received_at_ms)
            >= self.limits.heartbeat_timeout_ms
        {
            self.disconnect(DisconnectReason::HeartbeatTimeout);
            return Err(SessionError::Disconnected(
                DisconnectReason::HeartbeatTimeout,
            ));
        }
        Ok(())
    }

    pub fn disconnect(&mut self, reason: DisconnectReason) {
        if self.state == SessionState::Connected {
            self.state = SessionState::Disconnected(reason);
        }
    }

    fn ensure_connected(&self) -> Result<(), SessionError> {
        match self.state {
            SessionState::Connected => Ok(()),
            SessionState::Disconnected(reason) => Err(SessionError::Disconnected(reason)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    Disconnected(DisconnectReason),
    Authorization(AuthorizationDenial),
    ProtocolMismatch {
        expected: ProtocolVersion,
        received: ProtocolVersion,
    },
    CommandTooLarge {
        size: usize,
        maximum: usize,
    },
    TooManyInflightCommands,
    InboundRateLimited,
    TitlePreviewRateLimited,
    OutboundRateLimited,
    NoInflightCommand,
    NoQueuedOutboundMessage,
    CursorIdentityMismatch,
    CursorRegression,
}

impl From<AuthorizationDenial> for SessionError {
    fn from(value: AuthorizationDenial) -> Self {
        Self::Authorization(value)
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(reason) => write!(formatter, "session disconnected: {reason:?}"),
            Self::Authorization(denial) => denial.fmt(formatter),
            Self::ProtocolMismatch { expected, received } => {
                write!(
                    formatter,
                    "command protocol {received} does not match session protocol {expected}"
                )
            }
            Self::CommandTooLarge { size, maximum } => {
                write!(formatter, "command size {size} exceeds maximum {maximum}")
            }
            Self::TooManyInflightCommands => formatter.write_str("too many commands in flight"),
            Self::InboundRateLimited => formatter.write_str("inbound command rate limit exceeded"),
            Self::TitlePreviewRateLimited => {
                formatter.write_str("title preview rate limit exceeded")
            }
            Self::OutboundRateLimited => {
                formatter.write_str("outbound message rate limit exceeded")
            }
            Self::NoInflightCommand => formatter.write_str("no command is in flight"),
            Self::NoQueuedOutboundMessage => formatter.write_str("no outbound message is queued"),
            Self::CursorIdentityMismatch => {
                formatter.write_str("heartbeat cursor engine identity does not match the session")
            }
            Self::CursorRegression => {
                formatter.write_str("heartbeat cursor revision moved backwards")
            }
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Authorization(denial) => Some(denial),
            _ => None,
        }
    }
}
