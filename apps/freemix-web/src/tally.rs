//! Transport-free phone tally: per-input lights, connection status with a
//! clock-driven reconnect/heartbeat driver, and role-limited shortcuts.
//!
//! The driver owns one [`fm_client::Client`] and never performs I/O or reads a
//! clock. A transport adapter feeds it transport lifecycle changes, inbound
//! [`WireMessage`]s, and the current time, and executes the returned
//! [`TallyAction`]s. Lights derive only from *realized* switcher state, and a
//! tally that is not live presents [`TallyIndicator::Offline`] instead of a
//! last-known light, so a dropped connection never keeps showing a stale
//! on-air state.

use fm_client::{Client, ClientConfig, ClientError, ConnectionState, Intake, Outbound, Session};
use fm_protocol::{CommandPayload, WireInputId, WireMessage};
use fm_types::InputId;
use fm_ui_model::{ClientView, ManualTransitionStatus, SwitcherState};

use crate::TransitionControlState;

/// Interval between client heartbeats once a session is established. The
/// daemon expires a session after 15 s without one.
pub const TALLY_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
/// How long a sent heartbeat may stay unacknowledged before the transport is
/// treated as dead and closed.
pub const TALLY_HEARTBEAT_ACK_TIMEOUT_MS: u64 = 5_000;
/// How long an opened transport may take to complete handshake and snapshot.
pub const TALLY_SYNC_TIMEOUT_MS: u64 = 5_000;
/// First reconnect delay; later delays double up to [`TALLY_MAX_BACKOFF_MS`].
pub const TALLY_INITIAL_BACKOFF_MS: u64 = 500;
/// Largest reconnect delay.
pub const TALLY_MAX_BACKOFF_MS: u64 = 10_000;

/// The realized bus state of one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TallyLight {
    /// Visible on realized Program, including both sides of an active manual
    /// transition.
    Program,
    /// Selected on realized Preview and not on air.
    Preview,
    Idle,
}

impl TallyLight {
    /// Short, high-contrast label rendered on the tally screen.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Program => "ON AIR",
            Self::Preview => "PREVIEW",
            Self::Idle => "IDLE",
        }
    }
}

/// What the tally screen shows for one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TallyIndicator {
    Light(TallyLight),
    /// The connection is not live; the realized bus state is unknown.
    Offline,
}

impl TallyIndicator {
    /// Screen-reader text for the indicator.
    #[must_use]
    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::Light(TallyLight::Program) => "Tally: on air in Program",
            Self::Light(TallyLight::Preview) => "Tally: in Preview",
            Self::Light(TallyLight::Idle) => "Tally: idle",
            Self::Offline => "Tally: offline, state unknown",
        }
    }
}

/// One input's label and realized light.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TallyEntry {
    pub input: InputId,
    pub label: String,
    pub light: TallyLight,
}

/// Realized tally for every input, in canonical project order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TallyBoard {
    entries: Vec<TallyEntry>,
}

impl TallyBoard {
    /// Projects replicated state. Optimistic intent never lights a tally.
    #[must_use]
    pub fn from_view(view: &ClientView) -> Self {
        let entries = view
            .inputs
            .iter()
            .zip(&view.input_names)
            .map(|(&input, label)| TallyEntry {
                input,
                label: label.clone(),
                light: light_for(input, &view.switcher),
            })
            .collect();
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[TallyEntry] {
        &self.entries
    }

    #[must_use]
    pub fn entry(&self, input: InputId) -> Option<&TallyEntry> {
        self.entries.iter().find(|entry| entry.input == input)
    }
}

fn light_for(input: InputId, switcher: &SwitcherState) -> TallyLight {
    let in_manual_transition = matches!(
        switcher.realized_manual_transition,
        ManualTransitionStatus::Active(active) if active.from == input || active.to == input
    );
    if switcher.realized.program == input || in_manual_transition {
        TallyLight::Program
    } else if switcher.realized.preview == input {
        TallyLight::Preview
    } else {
        TallyLight::Idle
    }
}

/// Why the driver stopped reconnecting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TallyStop {
    /// The server rejected the handshake with a non-retryable error, such as
    /// a denied role.
    Rejected { code: String },
    /// The server speaks a different protocol identifier.
    Incompatible,
    /// The local user stopped the tally.
    Stopped,
}

/// Connection status presented on the tally screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TallyConnection {
    /// Not started.
    Idle,
    /// Transport opening, handshake, or snapshot synchronization in progress.
    /// `attempt` is zero for the first connection.
    Connecting {
        attempt: u32,
    },
    Live,
    /// Waiting before the next attempt.
    Reconnecting {
        attempt: u32,
        retry_in_ms: u64,
    },
    Stopped(TallyStop),
}

impl TallyConnection {
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// Short status text for the tally screen.
    #[must_use]
    pub fn status_text(&self) -> String {
        match self {
            Self::Idle => "Not connected".to_owned(),
            Self::Connecting { attempt: 0 } => "Connecting".to_owned(),
            Self::Connecting { attempt } => format!("Reconnecting (attempt {attempt})"),
            Self::Live => "Live".to_owned(),
            Self::Reconnecting {
                attempt,
                retry_in_ms,
            } => format!(
                "Connection lost; retry {attempt} in {}.{} s",
                retry_in_ms / 1_000,
                (retry_in_ms % 1_000) / 100
            ),
            Self::Stopped(TallyStop::Rejected { code }) => format!("Rejected by server: {code}"),
            Self::Stopped(TallyStop::Incompatible) => "Server protocol is incompatible".to_owned(),
            Self::Stopped(TallyStop::Stopped) => "Stopped".to_owned(),
        }
    }
}

/// One tally-screen shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TallyShortcut {
    /// Local full-screen toggle; never sends a command.
    ToggleFullScreen,
    /// Selects the tally's input on Preview.
    PreviewSelected,
    /// Cuts Preview to Program.
    Cut,
}

impl TallyShortcut {
    pub const ALL: [Self; 3] = [Self::ToggleFullScreen, Self::PreviewSelected, Self::Cut];

    /// Keyboard key bound to the shortcut.
    #[must_use]
    pub const fn key(self) -> char {
        match self {
            Self::ToggleFullScreen => 'f',
            Self::PreviewSelected => 'p',
            Self::Cut => 'c',
        }
    }

    /// Resolves a pressed key, ignoring ASCII case.
    #[must_use]
    pub fn from_key(key: char) -> Option<Self> {
        let key = key.to_ascii_lowercase();
        Self::ALL.into_iter().find(|shortcut| shortcut.key() == key)
    }

    #[must_use]
    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::ToggleFullScreen => "Toggle full-screen tally",
            Self::PreviewSelected => "Put this input on Preview",
            Self::Cut => "Cut Preview to Program",
        }
    }

    /// Server permission the granted role must carry, if any.
    #[must_use]
    pub const fn required_permission(self) -> Option<&'static str> {
        match self {
            Self::ToggleFullScreen => None,
            Self::PreviewSelected => Some("select_preview"),
            Self::Cut => Some("transition"),
        }
    }
}

/// Local, non-replicated tally screen state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TallyScreen {
    pub selected: Option<InputId>,
    pub full_screen: bool,
}

/// The selected input's label and indicator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedTally {
    pub input: InputId,
    pub label: String,
    pub indicator: TallyIndicator,
}

/// Everything a renderer needs for one tally frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TallyPresentation {
    pub connection: TallyConnection,
    pub show_name: Option<String>,
    pub board: TallyBoard,
    pub selected: Option<SelectedTally>,
    pub full_screen: bool,
    pub shortcuts: Vec<(TallyShortcut, TransitionControlState)>,
}

/// A transport operation requested by the driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TallyAction {
    OpenTransport,
    Send(Box<WireMessage>),
    CloseTransport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkPhase {
    Idle,
    /// Transport open requested; `deadline_ms` set once it opens.
    Connecting,
    Open,
    Waiting {
        retry_at_ms: u64,
        attempt: u32,
    },
    Stopped,
}

/// Clock-driven reconnecting tally session over one [`Client`].
#[derive(Debug)]
pub struct TallyLink {
    client: Client,
    phase: LinkPhase,
    stop: Option<TallyStop>,
    attempt: u32,
    sync_deadline_ms: Option<u64>,
    next_heartbeat_ms: Option<u64>,
    heartbeat_deadline_ms: Option<u64>,
}

impl TallyLink {
    /// Creates an idle driver with the tally reconnect range.
    ///
    /// # Errors
    ///
    /// Returns the client's configuration error.
    pub fn new(mut config: ClientConfig) -> Result<Self, ClientError> {
        config.initial_backoff_ms = TALLY_INITIAL_BACKOFF_MS;
        config.max_backoff_ms = TALLY_MAX_BACKOFF_MS;
        Ok(Self {
            client: Client::new(config)?,
            phase: LinkPhase::Idle,
            stop: None,
            attempt: 0,
            sync_deadline_ms: None,
            next_heartbeat_ms: None,
            heartbeat_deadline_ms: None,
        })
    }

    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// Requests the first transport. Ignored unless idle.
    pub fn start(&mut self) -> Option<TallyAction> {
        if self.phase != LinkPhase::Idle {
            return None;
        }
        self.begin_attempt()
    }

    /// Stops reconnecting permanently and closes any open transport.
    pub fn stop(&mut self) -> Option<TallyAction> {
        let open = matches!(self.phase, LinkPhase::Connecting | LinkPhase::Open);
        self.halt(TallyStop::Stopped);
        open.then_some(TallyAction::CloseTransport)
    }

    /// Reports an opened transport and returns the handshake to send.
    ///
    /// # Errors
    ///
    /// Returns a client state error if no attempt is in progress.
    pub fn transport_opened(&mut self, now_ms: u64) -> Result<TallyAction, ClientError> {
        let request = self.client.transport_connected()?;
        self.phase = LinkPhase::Open;
        self.sync_deadline_ms = Some(now_ms.saturating_add(TALLY_SYNC_TIMEOUT_MS));
        Ok(TallyAction::Send(Box::new(WireMessage::HandshakeRequest(
            request,
        ))))
    }

    /// Reports a closed or failed transport and schedules the next attempt
    /// unless the driver has stopped.
    pub fn transport_closed(&mut self, now_ms: u64) {
        self.clear_timers();
        if self.phase == LinkPhase::Stopped {
            return;
        }
        let backoff = self.client.transport_disconnected();
        self.attempt = backoff.attempt;
        self.phase = LinkPhase::Waiting {
            retry_at_ms: now_ms.saturating_add(backoff.delay_ms),
            attempt: backoff.attempt,
        };
    }

    /// Applies one inbound record. Returns [`TallyAction::CloseTransport`]
    /// when the record ends the session; the caller then closes the transport
    /// and reports [`Self::transport_closed`].
    pub fn receive(&mut self, message: WireMessage, now_ms: u64) -> Option<TallyAction> {
        if self.phase != LinkPhase::Open {
            return None;
        }
        if matches!(message, WireMessage::Error(_)) {
            // Includes `server_shutting_down`: reconnect after backoff.
            return Some(TallyAction::CloseTransport);
        }
        match self.client.intake(message) {
            Ok(Intake::HeartbeatAcknowledged) => {
                self.heartbeat_deadline_ms = None;
                self.next_heartbeat_ms = Some(now_ms.saturating_add(TALLY_HEARTBEAT_INTERVAL_MS));
            }
            Ok(Intake::ResyncRequested) => return Some(TallyAction::CloseTransport),
            Ok(_) => {}
            Err(ClientError::HandshakeRejected(error)) => {
                if !error.retryable {
                    self.halt(TallyStop::Rejected { code: error.code });
                }
                return Some(TallyAction::CloseTransport);
            }
            Err(ClientError::ProtocolMismatch(_)) => {
                self.halt(TallyStop::Incompatible);
                return Some(TallyAction::CloseTransport);
            }
            Err(_) => return Some(TallyAction::CloseTransport),
        }
        if self.client.state() == &ConnectionState::Ready && self.sync_deadline_ms.is_some() {
            self.sync_deadline_ms = None;
            self.attempt = 0;
            self.next_heartbeat_ms = Some(now_ms.saturating_add(TALLY_HEARTBEAT_INTERVAL_MS));
        }
        if matches!(self.client.state(), ConnectionState::ResyncRequired { .. }) {
            return Some(TallyAction::CloseTransport);
        }
        None
    }

    /// Advances timers and drains queued records. Call repeatedly until it
    /// returns `None`.
    pub fn poll(&mut self, now_ms: u64) -> Option<TallyAction> {
        match self.phase {
            LinkPhase::Idle | LinkPhase::Stopped | LinkPhase::Connecting => None,
            LinkPhase::Waiting { retry_at_ms, .. } => (now_ms >= retry_at_ms)
                .then(|| self.begin_attempt())
                .flatten(),
            LinkPhase::Open => self.poll_open(now_ms),
        }
    }

    fn poll_open(&mut self, now_ms: u64) -> Option<TallyAction> {
        let overdue = |deadline: Option<u64>| deadline.is_some_and(|deadline| now_ms >= deadline);
        if overdue(self.sync_deadline_ms) || overdue(self.heartbeat_deadline_ms) {
            return Some(TallyAction::CloseTransport);
        }
        if self.heartbeat_deadline_ms.is_none()
            && overdue(self.next_heartbeat_ms)
            && self.client.queue_heartbeat(now_ms).is_ok()
        {
            self.next_heartbeat_ms = None;
        }
        match self.client.pop_outbound()? {
            Outbound::Heartbeat(heartbeat) => {
                self.heartbeat_deadline_ms =
                    Some(now_ms.saturating_add(TALLY_HEARTBEAT_ACK_TIMEOUT_MS));
                Some(TallyAction::Send(Box::new(WireMessage::Heartbeat(
                    heartbeat,
                ))))
            }
            Outbound::Command(command) => {
                Some(TallyAction::Send(Box::new(WireMessage::Command(command))))
            }
        }
    }

    /// Current connection status.
    #[must_use]
    pub fn connection(&self, now_ms: u64) -> TallyConnection {
        match self.phase {
            LinkPhase::Idle => TallyConnection::Idle,
            LinkPhase::Stopped => {
                TallyConnection::Stopped(self.stop.clone().unwrap_or(TallyStop::Stopped))
            }
            LinkPhase::Waiting {
                retry_at_ms,
                attempt,
            } => TallyConnection::Reconnecting {
                attempt,
                retry_in_ms: retry_at_ms.saturating_sub(now_ms),
            },
            LinkPhase::Open if self.client.state() == &ConnectionState::Ready => {
                TallyConnection::Live
            }
            LinkPhase::Connecting | LinkPhase::Open => TallyConnection::Connecting {
                attempt: self.attempt,
            },
        }
    }

    /// Visibility and interactivity of one shortcut.
    ///
    /// Permissioned shortcuts are hidden unless the granted role carries the
    /// permission, and disabled while the tally is not live or has nothing to
    /// act on. The server still authorizes every command.
    #[must_use]
    pub fn shortcut_state(
        &self,
        shortcut: TallyShortcut,
        screen: &TallyScreen,
        now_ms: u64,
    ) -> TransitionControlState {
        let Some(permission) = shortcut.required_permission() else {
            return TransitionControlState::Enabled;
        };
        if !self
            .client
            .session()
            .is_some_and(|session| has_permission(session, permission))
        {
            return TransitionControlState::Hidden;
        }
        if !self.connection(now_ms).is_live() {
            return TransitionControlState::Disabled;
        }
        let Some(view) = self.client.model().view() else {
            return TransitionControlState::Disabled;
        };
        let enabled = match shortcut {
            TallyShortcut::ToggleFullScreen | TallyShortcut::Cut => true,
            TallyShortcut::PreviewSelected => screen.selected.is_some_and(|input| {
                view.inputs.contains(&input) && view.switcher.desired.preview != input
            }),
        };
        if enabled {
            TransitionControlState::Enabled
        } else {
            TransitionControlState::Disabled
        }
    }

    /// Executes an enabled shortcut. Full screen toggles locally; command
    /// shortcuts are queued and later drained by [`Self::poll`].
    ///
    /// `idempotency_key` must be globally unique for the server's receipt
    /// retention window.
    ///
    /// Returns whether the shortcut ran.
    pub fn press(
        &mut self,
        shortcut: TallyShortcut,
        screen: &mut TallyScreen,
        idempotency_key: impl Into<String>,
        now_ms: u64,
    ) -> bool {
        if !self.shortcut_state(shortcut, screen, now_ms).is_enabled() {
            return false;
        }
        let payload = match shortcut {
            TallyShortcut::ToggleFullScreen => {
                screen.full_screen = !screen.full_screen;
                return true;
            }
            TallyShortcut::PreviewSelected => {
                let Some(input) = screen.selected else {
                    return false;
                };
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input),
                }
            }
            TallyShortcut::Cut => CommandPayload::Cut,
        };
        self.client
            .queue_command(payload, idempotency_key, None, None)
            .is_ok()
    }

    /// Projects one renderable tally frame.
    #[must_use]
    pub fn presentation(&self, screen: &TallyScreen, now_ms: u64) -> TallyPresentation {
        let connection = self.connection(now_ms);
        let view = self.client.model().view();
        let board = view.as_ref().map(TallyBoard::from_view).unwrap_or_default();
        let selected = screen.selected.and_then(|input| {
            board.entry(input).map(|entry| SelectedTally {
                input,
                label: entry.label.clone(),
                indicator: if connection.is_live() {
                    TallyIndicator::Light(entry.light)
                } else {
                    TallyIndicator::Offline
                },
            })
        });
        let shortcuts = TallyShortcut::ALL
            .into_iter()
            .map(|shortcut| (shortcut, self.shortcut_state(shortcut, screen, now_ms)))
            .filter(|(_, state)| state.is_exposed())
            .collect();
        TallyPresentation {
            connection,
            show_name: view.map(|view| view.show_name),
            board,
            selected,
            full_screen: screen.full_screen,
            shortcuts,
        }
    }

    fn begin_attempt(&mut self) -> Option<TallyAction> {
        self.client.start_connect().ok()?;
        self.phase = LinkPhase::Connecting;
        Some(TallyAction::OpenTransport)
    }

    fn halt(&mut self, reason: TallyStop) {
        self.phase = LinkPhase::Stopped;
        self.stop = Some(reason);
        self.clear_timers();
    }

    fn clear_timers(&mut self) {
        self.sync_deadline_ms = None;
        self.next_heartbeat_ms = None;
        self.heartbeat_deadline_ms = None;
    }
}

fn has_permission(session: &Session, permission: &str) -> bool {
    session
        .permissions
        .iter()
        .any(|granted| granted == permission)
}
