use std::{env, error::Error, fmt, net::SocketAddr, path::PathBuf, time::Duration};

use fm_client::{
    Client, ConnectionState, ReconnectBackoff, SessionEvent, TcpSession, TcpSessionError,
};
use fm_protocol::{
    AudioPluginStatusRequest, CommandMessage, CommandPayload, HeartbeatMessage,
    ReplayPreviewRequest, ScenePreviewRequest, TitlePreviewRequest,
};
use fm_record::{RecordBus, RecorderTrackMap};
use fm_ui_egui::{
    DisplayOutputAnswer, RecorderTrackLayout, RecorderTrackMapView, RecorderTrackView,
    TitleAnimationRefusal, TitleUpdate,
};

use crate::titles::TitleRelay;

use crate::{
    ConnectionConfig, ControlTransport, DaemonSupervisor, ReadinessRecord, StudioConfig,
    SupervisorError, SupervisorState, native_client_config,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    LaunchingDaemon,
    DaemonExited { code: Option<i32> },
    DaemonFailed,
    RestartLimitReached,
    Disconnected,
    Connecting,
    Synchronizing,
    Ready,
    Backoff(ReconnectBackoff),
    ResyncRequired,
    ProtocolMismatch,
}

#[derive(Debug)]
pub enum StudioError {
    Supervisor(SupervisorError),
    Client(fm_client::ClientError),
    Session(TcpSessionError),
    BackoffNotElapsed {
        required: Duration,
        supplied: Duration,
    },
    NoSupervisedDaemon,
    MissingWebSocketToken,
}

impl fmt::Display for StudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supervisor(error) => error.fmt(formatter),
            Self::Client(error) => error.fmt(formatter),
            Self::Session(error) => error.fmt(formatter),
            Self::BackoffNotElapsed { required, supplied } => write!(
                formatter,
                "reconnect backoff not elapsed: required {required:?}, supplied {supplied:?}"
            ),
            Self::NoSupervisedDaemon => {
                formatter.write_str("runtime does not own a supervised daemon")
            }
            Self::MissingWebSocketToken => {
                formatter.write_str("FREEMIXD_WEB_TOKEN is missing or empty")
            }
        }
    }
}

impl Error for StudioError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Supervisor(error) => Some(error),
            Self::Client(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::BackoffNotElapsed { .. }
            | Self::NoSupervisedDaemon
            | Self::MissingWebSocketToken => None,
        }
    }
}

impl From<SupervisorError> for StudioError {
    fn from(value: SupervisorError) -> Self {
        Self::Supervisor(value)
    }
}

impl From<fm_client::ClientError> for StudioError {
    fn from(value: fm_client::ClientError) -> Self {
        Self::Client(value)
    }
}

impl From<TcpSessionError> for StudioError {
    fn from(value: TcpSessionError) -> Self {
        Self::Session(value)
    }
}

/// The server lists the UI has asked for: once it has (or a list is
/// known), every (re)connect asks for that list again.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // one independent wish per server list
struct WantedLists {
    /// The display inventory (Protocol 2.42).
    display_inventory: bool,
    /// The capture devices (IN-001, Protocol 2.47).
    capture_devices: bool,
    /// The system audio devices (AU-011, Protocol 2.68).
    system_audio_devices: bool,
    /// The headphone output devices (AU-001, Protocol 2.102).
    audio_output_devices: bool,
    /// The web gateway status (RC-007, Protocol 2.71).
    web_access: bool,
    /// The screens and windows the server can capture (IN-011).
    screen_capture_targets: bool,
}

/// A synchronous Studio control runtime. All clocks and waits are caller-owned.
#[derive(Debug)]
pub struct StudioRuntime {
    supervisor: Option<DaemonSupervisor>,
    address: SocketAddr,
    session: TcpSession,
    transport: ControlTransport,
    /// Correlates title-editor commands with this session's results.
    titles: TitleRelay,
    /// The latest refused or lost exact title preview; every publication
    /// carries it so the UI cannot miss it.
    preview_failed: Option<String>,
    /// The latest refused title animation save, carried the same way.
    animation_refusal: Option<TitleAnimationRefusal>,
    /// The server lists the UI has asked for.
    wanted: WantedLists,
    /// Whether the UI shows the input bank and so wants the live input
    /// thumbnail feed (Protocol 2.59); every (re)connect subscribes again
    /// while it does.
    input_thumbnails_wanted: bool,
    /// The latest answer per display-output slot to this session's set or
    /// clear, carried the same way.
    display_output_answers: [Option<DisplayOutputAnswer>; fm_protocol::MAX_DISPLAY_OUTPUTS],
    /// The latest production-clock readings (SW-013, Protocol 2.149).
    clocks: Vec<fm_protocol::ProductionClockStatus>,
    /// The diagnostics request ID a clock refresh awaits, and when it was
    /// sent, if one is in flight.
    clocks_request: Option<(String, std::time::Instant)>,
    /// The latest clock refusal or request failure the window shows.
    clocks_error: Option<String>,
    /// The supervised project bundle, when Studio owns the daemon.
    project_bundle: Option<PathBuf>,
    /// Each recorder slot's authored track map, read from the bundle.
    record_track_maps: [Option<RecorderTrackMapView>; 2],
    /// Why the bundle's track maps could not be read, if they could not.
    record_track_error: Option<String>,
}

impl StudioRuntime {
    /// Creates a disconnected runtime, launching a supervised daemon when requested.
    ///
    /// # Errors
    ///
    /// Returns an error for daemon startup/readiness or invalid client configuration.
    pub fn new(config: StudioConfig) -> Result<Self, StudioError> {
        Self::new_cancellable(config, Duration::from_millis(50), || false)
    }

    /// Creates a disconnected runtime with cancellable supervised readiness.
    ///
    /// # Errors
    ///
    /// Returns startup, readiness, cancellation, or client configuration errors.
    pub fn new_cancellable(
        config: StudioConfig,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, StudioError> {
        let transport = config.transport;
        let project_bundle = match &config.connection {
            ConnectionConfig::Supervised(supervised) => Some(supervised.project_bundle.clone()),
            ConnectionConfig::Existing(_) => None,
        };
        let (record_track_maps, record_track_error) =
            load_record_track_views(project_bundle.as_deref());
        let (supervisor, address, project_id) = match config.connection {
            ConnectionConfig::Supervised(supervised) => {
                let supervisor = DaemonSupervisor::launch_cancellable(
                    supervised,
                    config.restart_policy,
                    poll_interval,
                    cancelled,
                )?;
                let readiness = supervisor
                    .readiness()
                    .ok_or(StudioError::Supervisor(SupervisorError::MissingReadiness))?;
                (Some(supervisor), readiness.address, readiness.project_id)
            }
            ConnectionConfig::Existing(existing) => {
                (None, existing.address, existing.expected_project_id)
            }
        };
        let client = Client::new(native_client_config(
            config.desired_role,
            config.client_id,
            project_id,
        ))?;
        Ok(Self {
            supervisor,
            address,
            session: TcpSession::new(client),
            transport,
            titles: TitleRelay::default(),
            preview_failed: None,
            animation_refusal: None,
            wanted: WantedLists::default(),
            input_thumbnails_wanted: false,
            display_output_answers: [const { None }; fm_protocol::MAX_DISPLAY_OUTPUTS],
            clocks: Vec::new(),
            clocks_request: None,
            clocks_error: None,
            project_bundle,
            record_track_maps,
            record_track_error,
        })
    }

    #[must_use]
    pub const fn session(&self) -> &TcpSession {
        &self.session
    }

    #[must_use]
    pub const fn session_mut(&mut self) -> &mut TcpSession {
        &mut self.session
    }

    /// The control transport this runtime connects with.
    #[must_use]
    pub const fn transport(&self) -> ControlTransport {
        self.transport
    }

    /// Sends one exact title preview request. A request that cannot be
    /// sent (no raw TCP connection, or the write failed) is recorded as
    /// failed for the UI instead; previews are lossy and never retried.
    pub(crate) fn send_title_preview(&mut self, request: TitlePreviewRequest) {
        let request_id = request.request_id.clone();
        let sent = self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self.session.send_title_preview(request).is_ok();
        if !sent {
            self.preview_failed = Some(request_id);
        }
    }

    /// Sends one exact scene preview request (Protocol 2.32), failing it
    /// for the UI like [`Self::send_title_preview`] when it cannot go out.
    pub(crate) fn send_scene_preview(&mut self, request: ScenePreviewRequest) {
        let request_id = request.request_id.clone();
        let sent = self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self.session.send_scene_preview(request).is_ok();
        if !sent {
            self.preview_failed = Some(request_id);
        }
    }

    /// Sends one audio plugin status request (2.129, AU-006). The answer is
    /// read from [`TcpSession::latest_audio_plugin_status`].
    pub(crate) fn request_audio_plugin_status(&mut self, request: AudioPluginStatusRequest) {
        if self.transport == ControlTransport::Tcp && self.session.is_connected() {
            let _ = self.session.request_audio_plugin_status(request);
        }
    }

    /// Sends one exact Replay multiview request (OR-014), failing it for
    /// the UI like [`Self::send_title_preview`] when it cannot go out.
    pub(crate) fn send_replay_preview(&mut self, request: ReplayPreviewRequest) {
        let request_id = request.request_id.clone();
        let sent = self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self.session.send_replay_preview(request).is_ok();
        if !sent {
            self.preview_failed = Some(request_id);
        }
    }

    /// Records that the UI wants the display inventory without asking now
    /// (the session is recovering); the next (re)connect asks for it.
    pub(crate) const fn want_display_inventory(&mut self) {
        self.wanted.display_inventory = true;
    }

    /// Asks the server for its connected displays (Protocol 2.42); the
    /// reply updates the replicated inventory. Returns whether the request
    /// went out; one that cannot be sent is dropped, and the next
    /// (re)connect asks again.
    pub(crate) fn request_display_inventory(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.display_inventory = true;
        let request_id = format!(
            "studio-displays-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_display_inventory_request(fm_protocol::DisplayInventoryRequest { request_id })
                .is_ok()
    }

    /// Records that the UI wants the capture devices without asking now
    /// (the session is recovering); the next (re)connect asks for them.
    pub(crate) const fn want_capture_devices(&mut self) {
        self.wanted.capture_devices = true;
    }

    /// Asks the server for its capture devices (IN-001, Protocol 2.47);
    /// the reply updates the replicated capture inventory. Returns whether
    /// the request went out; one that cannot be sent is dropped, and the
    /// next (re)connect asks again.
    pub(crate) fn request_capture_devices(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.capture_devices = true;
        let request_id = format!(
            "studio-capture-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_capture_devices_request(fm_protocol::CaptureDevicesRequest { request_id })
                .is_ok()
    }

    /// Asks the server for one document page's notes or thumbnail
    /// (IN-014); the answer lands in the session's page answers. Returns
    /// whether the request went out.
    pub(crate) fn request_paged_page(&mut self, request: fm_protocol::PagedPageRequest) -> bool {
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self.session.send_paged_page_request(request).is_ok()
    }

    /// Records that the UI wants the system audio devices without asking
    /// now (the session is recovering); the next (re)connect asks for them.
    pub(crate) const fn want_system_audio_devices(&mut self) {
        self.wanted.system_audio_devices = true;
    }

    /// Asks the server for its system audio capture devices (AU-011,
    /// Protocol 2.68); the reply updates the replicated device list.
    /// Returns whether the request went out; one that cannot be sent is
    /// dropped, and the next (re)connect asks again.
    pub(crate) fn request_system_audio_devices(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.system_audio_devices = true;
        let request_id = format!(
            "studio-system-audio-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_system_audio_devices_request(fm_protocol::SystemAudioDevicesRequest {
                    request_id,
                })
                .is_ok()
    }

    /// Records that the UI wants the headphone output devices without
    /// asking now (the session is recovering); the next (re)connect asks
    /// for them.
    pub(crate) const fn want_audio_output_devices(&mut self) {
        self.wanted.audio_output_devices = true;
    }

    /// Asks the server for its headphone output devices (AU-001, Protocol
    /// 2.102); the reply updates the replicated device list. Returns
    /// whether the request went out; one that cannot be sent is dropped,
    /// and the next (re)connect asks again.
    pub(crate) fn request_audio_output_devices(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.audio_output_devices = true;
        let request_id = format!(
            "studio-audio-output-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_audio_output_devices_request(fm_protocol::AudioOutputDevicesRequest {
                    request_id,
                })
                .is_ok()
    }

    /// Records that the UI wants the web gateway status without asking now
    /// (the session is recovering); the next (re)connect asks for it.
    pub(crate) const fn want_web_access(&mut self) {
        self.wanted.web_access = true;
    }

    /// Sends one web access request (RC-007, Protocol 2.71): a status
    /// read, or starting or cancelling pairing, or revoking a device. The
    /// answer updates the replicated web access state. Returns whether the
    /// request went out; one that cannot be sent is dropped (a pairing or
    /// revoke is never replayed), and the next (re)connect asks for the
    /// status again.
    pub(crate) fn request_web_access(&mut self, action: fm_protocol::WebAccessAction) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.web_access = true;
        let request_id = format!(
            "studio-web-access-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_web_access_request(fm_protocol::WebAccessRequest { request_id, action })
                .is_ok()
    }

    /// Acknowledges, silences, or unsilences one daemon alert rule (RC-015,
    /// Protocol 2.109). Returns whether the request went out; the daemon's
    /// answering `health_status` updates the health view.
    pub(crate) fn request_health_alert(
        &mut self,
        rule: fm_protocol::HealthAlertRule,
        action: fm_protocol::HealthAlertAction,
    ) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request_id = format!(
            "studio-health-alert-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_health_alert_request(fm_protocol::HealthAlertRequest {
                    protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
                    request_id,
                    rule,
                    action,
                })
                .is_ok()
    }

    /// Each recorder slot's authored track map (AU-010).
    #[must_use]
    pub(crate) fn record_track_maps(&self) -> &[Option<RecorderTrackMapView>; 2] {
        &self.record_track_maps
    }

    /// Why the supervised bundle's track maps could not be read, if they
    /// could not.
    #[must_use]
    pub(crate) fn record_track_error(&self) -> Option<&str> {
        self.record_track_error.as_deref()
    }

    /// Whether this Studio has the supervised project bundle locally.
    #[must_use]
    pub(crate) const fn record_tracks_local(&self) -> bool {
        self.project_bundle.is_some()
    }

    /// Re-reads the supervised bundle's recorder track maps (AU-010).
    pub(crate) fn reload_record_track_views(&mut self) {
        let (maps, error) = load_record_track_views(self.project_bundle.as_deref());
        self.record_track_maps = maps;
        self.record_track_error = error;
    }

    /// The latest production-clock readings (SW-013), in name order.
    #[must_use]
    pub(crate) fn clocks(&self) -> &[fm_protocol::ProductionClockStatus] {
        &self.clocks
    }

    /// The latest clock refusal or request failure the clocks window
    /// shows, if any.
    #[must_use]
    pub(crate) fn clocks_error(&self) -> Option<&str> {
        self.clocks_error.as_deref()
    }

    /// Asks the daemon for every production clock's reading (SW-013,
    /// Protocol 2.149) without blocking; the answer arrives as
    /// [`SessionEvent::DiagnosticsResponse`] and is applied with
    /// [`Self::apply_clocks_response`].
    pub(crate) fn request_clocks(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        if !self.session.is_connected() {
            self.clocks_request = None;
            self.clocks_error = Some("the control connection is not ready".to_owned());
            return false;
        }
        if let Some((_, sent_at)) = &self.clocks_request {
            if sent_at.elapsed() < std::time::Duration::from_secs(5) {
                // One reading request is already in flight; the next window
                // refresh asks again.
                return false;
            }
            // A lost or unanswered request must not wedge every later one.
            self.clocks_request = None;
        }
        let request_id = format!(
            "studio-clocks-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let sent = self.transport == ControlTransport::Tcp
            && self.session.send_diagnostics_request(&request_id).is_ok();
        if sent {
            self.clocks_request = Some((request_id, std::time::Instant::now()));
            self.clocks_error = None;
        } else {
            self.clocks_error = Some("the clock readings could not be requested".to_owned());
        }
        sent
    }

    /// Applies one diagnostics answer: every production clock's reading.
    pub(crate) fn apply_clocks_response(&mut self, response: &fm_protocol::DiagnosticsResponse) {
        if self
            .clocks_request
            .as_ref()
            .is_some_and(|(request_id, _)| request_id == &response.request_id)
        {
            self.clocks_request = None;
        }
        self.clocks.clone_from(&response.production_clocks);
        self.clocks_error = None;
    }

    /// Operates one production clock (SW-013, Protocol 2.150) without
    /// blocking; the answer arrives as
    /// [`SessionEvent::ClockControlResponse`].
    pub(crate) fn request_clock_control(
        &mut self,
        name: &str,
        operation: fm_protocol::ClockControlOperation,
    ) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        if !fm_protocol::clock_name_is_valid(name) {
            self.clocks_error = Some("the clock name is not valid".to_owned());
            return false;
        }
        if !self.session.is_connected() {
            self.clocks_error = Some("the control connection is not ready".to_owned());
            return false;
        }
        let request_id = format!(
            "studio-clock-control-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let sent = self.transport == ControlTransport::Tcp
            && self
                .session
                .send_clock_control_request(fm_protocol::ClockControlRequest {
                    protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
                    request_id,
                    name: name.to_owned(),
                    operation,
                })
                .is_ok();
        if sent {
            self.clocks_error = None;
        } else {
            self.clocks_error = Some("the clock control could not be sent".to_owned());
        }
        sent
    }

    /// Applies the daemon's answer to one clock control: an accepted
    /// operation carries the clock's reading; a refusal carries its reason.
    pub(crate) fn apply_clock_control_response(
        &mut self,
        response: &fm_protocol::ClockControlResponse,
    ) {
        if !response.accepted {
            self.clocks_error = response
                .error
                .clone()
                .or_else(|| Some("the daemon refused the clock control".to_owned()));
            return;
        }
        self.clocks_error = None;
        let Some(reading) = &response.reading else {
            return;
        };
        match self
            .clocks
            .binary_search_by(|clock| clock.name.cmp(&reading.name))
        {
            Ok(index) => self.clocks[index] = reading.clone(),
            Err(index) => self.clocks.insert(index, reading.clone()),
        }
    }

    /// Asks the server for one window of slideshow `input`'s slide list
    /// from `offset` (IN-009); the reply fills the replicated listing.
    /// Returns whether the request went out; the Media tab asks again for
    /// a window that never arrives.
    pub(crate) fn request_slideshow_slides(
        &mut self,
        input: fm_types::InputId,
        offset: u32,
    ) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request_id = format!(
            "studio-slides-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_slideshow_slides_request(fm_protocol::SlideshowSlidesRequest {
                    request_id,
                    input: fm_protocol::WireInputId::from_domain(input),
                    offset,
                    limit: fm_protocol::MAX_SLIDESHOW_SLIDES_PAGE,
                })
                .is_ok()
    }

    /// Records that the UI wants the capture targets without asking now
    /// (the session is recovering); the next (re)connect asks for them.
    pub(crate) const fn want_screen_capture_targets(&mut self) {
        self.wanted.screen_capture_targets = true;
    }

    /// Asks the server for the screens and windows it can capture
    /// (IN-011); the reply updates the replicated target list. Returns
    /// whether the request went out; one that cannot be sent is dropped,
    /// and the next (re)connect asks again.
    pub(crate) fn request_screen_capture_targets(&mut self) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.wanted.screen_capture_targets = true;
        let request_id = format!(
            "studio-screen-capture-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_screen_capture_targets_request(fm_protocol::ScreenCaptureTargetsRequest {
                    request_id,
                })
                .is_ok()
    }

    /// Asks the server's desktop to pick screen capture input `input`'s
    /// screen or window again (IN-011, Protocol 2.85). Returns whether the
    /// request went out; one that cannot be sent is dropped.
    pub(crate) fn pick_screen_capture(&mut self, input: fm_types::InputId) -> bool {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request_id = format!(
            "studio-screen-capture-pick-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_screen_capture_pick_request(fm_protocol::ScreenCapturePickRequest {
                    request_id,
                    input: fm_protocol::WireInputId::from_domain(input),
                })
                .is_ok()
    }

    /// Turns the live input thumbnail feed on or off (Protocol 2.59) and
    /// remembers the choice, so a (re)connect subscribes again while it is
    /// on. Returns whether the request went out; one that cannot be sent now
    /// is sent by the next (re)connect.
    pub(crate) fn set_input_thumbnails(&mut self, enabled: bool) -> bool {
        self.input_thumbnails_wanted = enabled;
        self.transport == ControlTransport::Tcp
            && self.session.is_connected()
            && self
                .session
                .send_input_thumbnail_subscription(enabled)
                .is_ok()
    }

    /// Remembers the thumbnail feed choice without sending it (the session
    /// is recovering); the next (re)connect subscribes when it is on.
    pub(crate) const fn set_input_thumbnails_later(&mut self, enabled: bool) {
        self.input_thumbnails_wanted = enabled;
    }

    /// Subscribes to the input thumbnail feed again after a (re)connect when
    /// the UI wants it: a fresh connection starts unsubscribed.
    fn resubscribe_input_thumbnails(&mut self) {
        if self.input_thumbnails_wanted {
            self.set_input_thumbnails(true);
        }
    }

    /// Records that the server refused or dropped preview `request_id`.
    pub(crate) fn note_preview_failure(&mut self, request_id: &str) {
        self.preview_failed = Some(request_id.to_owned());
    }

    /// The latest refused or lost exact preview.
    pub(crate) fn preview_failure(&self) -> Option<&str> {
        self.preview_failed.as_deref()
    }

    /// Records a refused title animation save.
    pub(crate) fn note_animation_refusal(&mut self, refusal: TitleAnimationRefusal) {
        self.animation_refusal = Some(refusal);
    }

    /// The latest refused title animation save.
    pub(crate) const fn animation_refusal(&self) -> Option<&TitleAnimationRefusal> {
        self.animation_refusal.as_ref()
    }

    /// Records the answer to a display-output set or clear.
    pub(crate) fn note_display_output_answer(&mut self, answer: DisplayOutputAnswer) {
        let index = answer.slot.index();
        self.display_output_answers[index] = Some(answer);
    }

    /// The latest answer per display-output slot.
    pub(crate) const fn display_output_answers(
        &self,
    ) -> &[Option<DisplayOutputAnswer>; fm_protocol::MAX_DISPLAY_OUTPUTS] {
        &self.display_output_answers
    }

    pub(crate) const fn titles_mut(&mut self) -> &mut TitleRelay {
        &mut self.titles
    }

    /// Resolves tracked title commands other than `keep`; see
    /// [`TitleRelay::settle`].
    pub(crate) fn settle_titles(&mut self, keep: Option<&str>) {
        self.titles.settle(self.session.client(), keep);
    }

    /// Advances the title relay against the client and drains its updates.
    pub(crate) fn take_title_updates(&mut self, ready: bool) -> Vec<TitleUpdate> {
        self.titles.observe(self.session.client(), ready)
    }

    #[must_use]
    pub const fn supervisor(&self) -> Option<&DaemonSupervisor> {
        self.supervisor.as_ref()
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Derives display lifecycle directly from process and protocol state.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error while refreshing supervised process state.
    pub fn lifecycle(&mut self) -> Result<LifecycleState, StudioError> {
        if let Some(supervisor) = &mut self.supervisor {
            match supervisor.poll()? {
                SupervisorState::Launching => return Ok(LifecycleState::LaunchingDaemon),
                SupervisorState::Exited { code } => {
                    return Ok(LifecycleState::DaemonExited { code });
                }
                SupervisorState::Failed => return Ok(LifecycleState::DaemonFailed),
                SupervisorState::RestartLimitReached => {
                    return Ok(LifecycleState::RestartLimitReached);
                }
                SupervisorState::Ready(_) => {}
            }
        }
        Ok(match self.session.client().state() {
            ConnectionState::Disconnected => LifecycleState::Disconnected,
            ConnectionState::Connecting | ConnectionState::AwaitingHandshake => {
                LifecycleState::Connecting
            }
            ConnectionState::Synchronizing { .. } => LifecycleState::Synchronizing,
            ConnectionState::Ready => LifecycleState::Ready,
            ConnectionState::Backoff(backoff) => LifecycleState::Backoff(*backoff),
            ConnectionState::ResyncRequired { .. } => LifecycleState::ResyncRequired,
            ConnectionState::ProtocolMismatch { .. } => LifecycleState::ProtocolMismatch,
        })
    }

    /// Connects and synchronizes by snapshot or resume using the supplied timeout.
    ///
    /// # Errors
    ///
    /// Returns a transport, handshake, synchronization, or supervisor error.
    pub fn connect(&mut self, connect_timeout: Duration) -> Result<SessionEvent, StudioError> {
        if let Some(supervisor) = &mut self.supervisor {
            supervisor.poll()?;
        }
        self.connect_transport(connect_timeout, Duration::from_millis(50), || false, false)
    }

    /// Connects and synchronizes while polling a caller-owned cancellation source.
    ///
    /// # Errors
    ///
    /// Returns a transport, handshake, synchronization, cancellation, or supervisor error.
    pub fn connect_cancellable(
        &mut self,
        connect_timeout: Duration,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<SessionEvent, StudioError> {
        if let Some(supervisor) = &mut self.supervisor {
            supervisor.poll()?;
        }
        self.connect_transport(connect_timeout, poll_interval, cancelled, true)
    }

    fn connect_transport(
        &mut self,
        connect_timeout: Duration,
        poll_interval: Duration,
        mut cancelled: impl FnMut() -> bool,
        cancellable_tcp: bool,
    ) -> Result<SessionEvent, StudioError> {
        let event = match self.transport {
            ControlTransport::Tcp if cancellable_tcp => self.session.connect_cancellable(
                self.address(),
                connect_timeout,
                poll_interval,
                cancelled,
            )?,
            ControlTransport::Tcp => self.session.connect(self.address(), connect_timeout)?,
            ControlTransport::WebSocket => {
                let token = env::var("FREEMIXD_WEB_TOKEN")
                    .ok()
                    .filter(|token| !token.is_empty())
                    .ok_or(StudioError::MissingWebSocketToken)?;
                self.session.connect_websocket_cancellable(
                    self.address(),
                    &token,
                    connect_timeout,
                    if cancellable_tcp {
                        poll_interval
                    } else {
                        Duration::from_millis(50)
                    },
                    &mut cancelled,
                )?
            }
        };
        self.requery_display_inventory();
        self.requery_capture_devices();
        self.requery_system_audio_devices();
        self.requery_audio_output_devices();
        self.requery_web_access();
        self.requery_screen_capture_targets();
        self.resubscribe_input_thumbnails();
        Ok(event)
    }

    /// Asks for the display inventory again after a (re)connect when the
    /// UI wanted it or it was known: it is runtime state that neither a
    /// resumed nor a re-snapshotted session carries, and displays may have
    /// changed while disconnected.
    fn requery_display_inventory(&mut self) {
        if self.wanted.display_inventory
            || self.session.client().model().display_inventory().is_some()
        {
            self.request_display_inventory();
        }
    }

    /// Asks for the capture devices again after a (re)connect when the UI
    /// wanted them or they were known: like the display inventory, they are
    /// runtime state no resumed or re-snapshotted session carries, and
    /// cameras may have been plugged in or out while disconnected.
    fn requery_capture_devices(&mut self) {
        if self.wanted.capture_devices
            || self.session.client().model().capture_inventory().is_some()
        {
            self.request_capture_devices();
        }
    }

    /// Asks for the system audio devices again after a (re)connect when the
    /// UI wanted them or they were known: devices may have come or gone
    /// while disconnected.
    /// Asks for the web gateway status again after a (re)connect when the
    /// UI wanted it: devices may have paired or a code expired meanwhile.
    fn requery_web_access(&mut self) {
        if self.wanted.web_access || self.session.client().model().web_access().is_some() {
            self.request_web_access(fm_protocol::WebAccessAction::Status);
        }
    }

    fn requery_system_audio_devices(&mut self) {
        if self.wanted.system_audio_devices
            || self
                .session
                .client()
                .model()
                .system_audio_inventory()
                .is_some()
        {
            self.request_system_audio_devices();
        }
    }

    /// Asks for the headphone output devices again after a (re)connect when
    /// the UI wanted them or they were known: devices may have been plugged
    /// in or out while disconnected.
    fn requery_audio_output_devices(&mut self) {
        if self.wanted.audio_output_devices
            || self
                .session
                .client()
                .model()
                .audio_output_inventory()
                .is_some()
        {
            self.request_audio_output_devices();
        }
    }

    /// Asks for the capture targets again after a (re)connect when the UI
    /// wanted them or they were known: windows open and close while
    /// disconnected.
    fn requery_screen_capture_targets(&mut self) {
        if self.wanted.screen_capture_targets
            || self
                .session
                .client()
                .model()
                .screen_capture_inventory()
                .is_some()
        {
            self.request_screen_capture_targets();
        }
    }

    /// Reconnects only after the caller reports that the client-selected backoff elapsed.
    /// If the owned daemon has exited, one bounded restart is performed first.
    ///
    /// # Errors
    ///
    /// Rejects insufficient elapsed backoff and propagates restart/session errors.
    pub fn reconnect(
        &mut self,
        elapsed_backoff: Duration,
        connect_timeout: Duration,
    ) -> Result<SessionEvent, StudioError> {
        if let Some(backoff) = self.session.reconnect_backoff() {
            let required = Duration::from_millis(backoff.delay_ms);
            if elapsed_backoff < required {
                return Err(StudioError::BackoffNotElapsed {
                    required,
                    supplied: elapsed_backoff,
                });
            }
        }
        if let Some(supervisor) = &mut self.supervisor
            && matches!(supervisor.poll()?, SupervisorState::Exited { .. })
        {
            self.address = supervisor.restart()?.address;
        }
        self.connect(connect_timeout)
    }

    /// Reconnects after backoff while polling a caller-owned cancellation source.
    ///
    /// # Errors
    ///
    /// Rejects insufficient elapsed backoff and propagates restart/session errors.
    pub fn reconnect_cancellable(
        &mut self,
        elapsed_backoff: Duration,
        connect_timeout: Duration,
        poll_interval: Duration,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<SessionEvent, StudioError> {
        if let Some(backoff) = self.session.reconnect_backoff() {
            let required = Duration::from_millis(backoff.delay_ms);
            if elapsed_backoff < required {
                return Err(StudioError::BackoffNotElapsed {
                    required,
                    supplied: elapsed_backoff,
                });
            }
        }
        if let Some(supervisor) = &mut self.supervisor
            && matches!(supervisor.poll()?, SupervisorState::Exited { .. })
        {
            self.address = supervisor
                .restart_cancellable(poll_interval, &mut cancelled)?
                .address;
        }
        self.connect_cancellable(connect_timeout, poll_interval, cancelled)
    }

    /// Explicitly restarts the owned daemon after disconnecting the session.
    /// The caller decides when this bounded restart is appropriate.
    ///
    /// # Errors
    ///
    /// Returns an error for existing mode, exhausted restart policy, or process/readiness failure.
    pub fn restart_daemon(&mut self) -> Result<ReadinessRecord, StudioError> {
        self.session.disconnect();
        let readiness = self
            .supervisor
            .as_mut()
            .ok_or(StudioError::NoSupervisedDaemon)?
            .restart()?;
        self.address = readiness.address;
        Ok(readiness)
    }

    /// Queues a durable command in the bounded client queue.
    ///
    /// # Errors
    ///
    /// Propagates client state, capacity, and idempotency errors.
    pub fn queue_command(
        &mut self,
        payload: CommandPayload,
        idempotency_key: impl Into<String>,
        expected_revision: Option<u64>,
        deadline_ms: Option<u64>,
    ) -> Result<CommandMessage, StudioError> {
        Ok(self
            .session
            .queue_command(payload, idempotency_key, expected_revision, deadline_ms)?)
    }

    /// Sends all currently queued commands and heartbeats.
    ///
    /// # Errors
    ///
    /// Propagates transport and codec errors.
    pub fn flush(&mut self) -> Result<usize, StudioError> {
        Ok(self.session.flush()?)
    }

    /// Sends queued records while polling writes for cancellation.
    ///
    /// # Errors
    ///
    /// Propagates transport, codec, and cancellation errors.
    pub fn flush_cancellable(
        &mut self,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<usize, StudioError> {
        Ok(self.session.flush_cancellable(poll_interval, cancelled)?)
    }

    /// Queues and immediately flushes a heartbeat using caller-supplied time.
    ///
    /// # Errors
    ///
    /// Propagates client, transport, and codec errors.
    pub fn send_heartbeat(&mut self, sent_at_ms: u64) -> Result<HeartbeatMessage, StudioError> {
        let heartbeat = self.session.queue_heartbeat(sent_at_ms)?;
        self.session.flush()?;
        Ok(heartbeat)
    }

    /// Queues a heartbeat and flushes it with cancellable writes.
    ///
    /// # Errors
    ///
    /// Propagates client, transport, codec, and cancellation errors.
    pub fn send_heartbeat_cancellable(
        &mut self,
        sent_at_ms: u64,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<HeartbeatMessage, StudioError> {
        let heartbeat = self.session.queue_heartbeat(sent_at_ms)?;
        self.session.flush_cancellable(poll_interval, cancelled)?;
        Ok(heartbeat)
    }

    /// Blocks for one durable event, runtime event, command result, server error, or EOF.
    ///
    /// # Errors
    ///
    /// Propagates transport, codec, and client intake errors.
    pub fn receive(&mut self) -> Result<SessionEvent, StudioError> {
        Ok(self.session.receive()?)
    }

    /// Waits for one event, returning `None` on timeout.
    ///
    /// # Errors
    ///
    /// Propagates transport, codec, and client intake errors.
    pub fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<SessionEvent>, StudioError> {
        Ok(self.session.receive_timeout(timeout)?)
    }

    /// Waits for one session event while polling a caller-owned cancellation source.
    ///
    /// # Errors
    ///
    /// Propagates transport, codec, client intake, and cancellation errors.
    pub fn receive_cancellable(
        &mut self,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<SessionEvent, StudioError> {
        Ok(self.session.receive_cancellable(poll_interval, cancelled)?)
    }

    /// Sends one diagnostics request and waits for its validated response.
    ///
    /// # Errors
    ///
    /// Propagates transport, codec, client intake, and cancellation errors.
    pub fn send_diagnostics_cancellable(
        &mut self,
        request_id: impl Into<String>,
        poll_interval: Duration,
        cancelled: impl FnMut() -> bool,
    ) -> Result<SessionEvent, StudioError> {
        Ok(self
            .session
            .send_diagnostics_cancellable(request_id, poll_interval, cancelled)?)
    }
}

/// Reads both recorder slots' track maps from a supervised project bundle
/// (AU-010). The bundle stores them as `recorders/<slot>.trackmap`.
fn load_record_track_views(
    bundle: Option<&std::path::Path>,
) -> ([Option<RecorderTrackMapView>; 2], Option<String>) {
    let Some(bundle) = bundle else {
        return ([None, None], None);
    };
    let mut views: [Option<RecorderTrackMapView>; 2] = [None, None];
    let mut error = None;
    for (index, slot) in views.iter_mut().enumerate() {
        let path = bundle
            .join("recorders")
            .join(format!("{}.trackmap", index + 1));
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(read) if read.kind() == std::io::ErrorKind::NotFound => continue,
            Err(read) => {
                error.get_or_insert_with(|| format!("recorder {} track map: {read}", index + 1));
                continue;
            }
        };
        match RecorderTrackMap::from_document(&text) {
            Ok(map) => {
                *slot = Some(track_map_view(index + 1, &map));
            }
            Err(read) => {
                error.get_or_insert_with(|| format!("recorder {} track map: {read}", index + 1));
            }
        }
    }
    (views, error)
}

fn track_map_view(slot: usize, map: &RecorderTrackMap) -> RecorderTrackMapView {
    RecorderTrackMapView {
        slot: u8::try_from(slot).unwrap_or(u8::MAX),
        layout: match map.layout() {
            fm_record::TrackMapLayout::Interleaved => RecorderTrackLayout::Interleaved,
            fm_record::TrackMapLayout::SplitPerTrack => RecorderTrackLayout::SplitPerTrack,
        },
        tracks: map
            .tracks()
            .iter()
            .map(|track| RecorderTrackView {
                name: track.name().to_owned(),
                channels: track.layout().channel_count(),
                sources: track
                    .sources()
                    .iter()
                    .map(|source| match source.bus {
                        RecordBus::Master => format!("master {}", source.channel),
                        RecordBus::Aux(bus) => {
                            format!("aux {} {}", bus.letter(), source.channel)
                        }
                    })
                    .collect(),
            })
            .collect(),
        channels: map.total_channels(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_track_maps_become_console_views() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("show.freemix");
        std::fs::create_dir_all(bundle.join("recorders")).unwrap();
        let map = RecorderTrackMap::new(
            vec![
                fm_record::RecordTrack::stereo(
                    "Program",
                    fm_record::TrackSource::master(0),
                    fm_record::TrackSource::master(1),
                ),
                fm_record::RecordTrack::mono(
                    "Bus A",
                    fm_record::TrackSource::new(
                        RecordBus::Aux(fm_types::audio_bus::AuxBusNumber::new(1).unwrap()),
                        2,
                    ),
                ),
            ],
            fm_record::BusInventory::new(vec![
                (RecordBus::Master, 2),
                (
                    RecordBus::Aux(fm_types::audio_bus::AuxBusNumber::new(1).unwrap()),
                    4,
                ),
            ])
            .unwrap(),
        )
        .unwrap()
        .with_layout(fm_record::TrackMapLayout::SplitPerTrack);
        std::fs::write(bundle.join("recorders/1.trackmap"), map.to_document()).unwrap();
        let (views, error) = load_record_track_views(Some(&bundle));
        assert!(error.is_none(), "{error:?}");
        let view = views[0].as_ref().unwrap();
        assert_eq!(view.slot, 1);
        assert_eq!(view.layout, RecorderTrackLayout::SplitPerTrack);
        assert_eq!(view.channels, 3);
        assert_eq!(view.tracks[0].name, "Program");
        assert_eq!(view.tracks[0].sources, ["master 0", "master 1"]);
        assert_eq!(view.tracks[1].sources, ["aux A 2"]);
        assert!(views[1].is_none());
        assert!(load_record_track_views(None).1.is_none());
    }
}
