use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use fm_client::{
    Client, ClientError, CommandStatus, CommandUncertainty, SessionEvent, SyncMode, TcpSessionError,
};
use fm_protocol::{
    CommandPayload, CommandResult, DurableGap, EditLockScope, MonitorTap, OverlayBorderPreset,
    OverlayPositionPreset, OverlayTransitionKind, StingerAudioPolicy, StingerMissingMediaFallback,
    WireInputId, WireMessage, WireOutputId, WireRecorderSlot, WireStreamTargetId,
};
use fm_types::OutputId;
use fm_ui_egui::{
    CatalogueRefusal, CatalogueRequest, ExternalStudioAction, InputAudioStripUpdate,
    InputMoveDirection, ReconnectStatus, RecordingAction, StreamAction, StudioConnectionStatus,
    StudioIntent, StudioShell, StudioUiState, TerminalUncertaintyNotice, TitleUpdate,
    external_intent,
};
use fm_ui_model::ClientView;
use fm_ui_model::title_editor::{TitleAccess, title_payload};

#[cfg(test)]
use fm_protocol::{AudioMeterChannel, AudioMetersMessage};

use crate::osc::{OscAction, OscReceiver};
use crate::{
    LifecycleState, PreferenceStore, SavedPreferences, StudioConfig, StudioRuntime,
    preferences_path,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const PEER_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
// Short enough to keep timeout paths quick, yet wide enough that a debug
// build decoding several multi-part monitor frames under parallel-test CPU
// load finishes a command or heartbeat wait well inside it.
#[cfg(test)]
const PEER_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(not(test))]
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(25);
const REQUEST_CAPACITY: usize = 16;
const DEFERRED_INTENT_CAPACITY: usize = 16;
const STATE_NOTIFICATION_CAPACITY: usize = 1;
const MAX_COMMAND_RECORDS: usize = 8;
const TERMINAL_UNCERTAINTY_CAPACITY: usize = 8;
static NEXT_WORKER_NONCE: AtomicU64 = AtomicU64::new(1);

/// Opens the cross-platform native Studio window.
///
/// # Errors
///
/// Returns an error when the native event loop or renderer cannot start.
pub fn launch_native(config: StudioConfig) -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 940.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "FreeMix Studio",
        options,
        Box::new(move |creation_context| Ok(Box::new(StudioApp::new(config, creation_context)?))),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerRequest {
    Intent(StudioIntent),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnqueueError {
    Full,
    Disconnected,
}

impl fmt::Display for EnqueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("Studio command queue is full"),
            Self::Disconnected => formatter.write_str("Studio worker is disconnected"),
        }
    }
}

fn try_enqueue(
    sender: &SyncSender<WorkerRequest>,
    request: WorkerRequest,
) -> Result<(), EnqueueError> {
    sender.try_send(request).map_err(|error| match error {
        TrySendError::Full(_) => EnqueueError::Full,
        TrySendError::Disconnected(_) => EnqueueError::Disconnected,
    })
}

fn coalesce_adjacent_intent(tail: &mut StudioIntent, incoming: StudioIntent) -> bool {
    match (tail, incoming) {
        (
            StudioIntent::SetManualTransitionPosition { position: current },
            StudioIntent::SetManualTransitionPosition { position },
        ) => {
            *current = position;
            true
        }
        (
            StudioIntent::SetInputAudioStrip {
                input: current_input,
                update: current_update,
            },
            StudioIntent::SetInputAudioStrip { input, update },
        ) if *current_input == input => {
            current_update.gain_millidb = update.gain_millidb.or(current_update.gain_millidb);
            current_update.balance_basis_points = update
                .balance_basis_points
                .or(current_update.balance_basis_points);
            current_update.muted = update.muted.or(current_update.muted);
            current_update.soloed = update.soloed.or(current_update.soloed);
            current_update.follow_video = update.follow_video.or(current_update.follow_video);
            current_update.delay_samples = update.delay_samples.or(current_update.delay_samples);
            true
        }
        _ => false,
    }
}

/// Whether a full worker-side FIFO must still keep `intent`. A title
/// command is already pending in the UI's editor, which allows one per
/// title, so title commands are bounded by the title count and are never
/// dropped (a dropped one would leave its title blocked).
const fn exceeds_capacity_allowed(intent: &StudioIntent) -> bool {
    matches!(intent, StudioIntent::Title(_))
}

struct PendingIntents {
    intents: VecDeque<StudioIntent>,
}

impl PendingIntents {
    fn new() -> Self {
        Self {
            intents: VecDeque::with_capacity(REQUEST_CAPACITY),
        }
    }

    fn submit(
        &mut self,
        sender: &SyncSender<WorkerRequest>,
        intent: StudioIntent,
    ) -> Result<(), EnqueueError> {
        if self.intents.is_empty() {
            match try_enqueue(sender, WorkerRequest::Intent(intent.clone())) {
                Ok(()) => return Ok(()),
                Err(EnqueueError::Disconnected) => return Err(EnqueueError::Disconnected),
                Err(EnqueueError::Full) => {}
            }
        }
        self.push(intent)
    }

    fn flush(&mut self, sender: &SyncSender<WorkerRequest>) -> Result<(), EnqueueError> {
        while let Some(intent) = self.intents.front().cloned() {
            match try_enqueue(sender, WorkerRequest::Intent(intent)) {
                Ok(()) => {
                    self.intents.pop_front();
                }
                Err(EnqueueError::Full) => return Ok(()),
                Err(EnqueueError::Disconnected) => return Err(EnqueueError::Disconnected),
            }
        }
        Ok(())
    }

    fn push(&mut self, intent: StudioIntent) -> Result<(), EnqueueError> {
        if let Some(tail) = self.intents.back_mut()
            && coalesce_adjacent_intent(tail, intent.clone())
        {
            return Ok(());
        }
        if self.intents.len() == REQUEST_CAPACITY {
            return Err(EnqueueError::Full);
        }
        self.intents.push_back(intent);
        Ok(())
    }

    /// Empties the queue and returns the title requests it held.
    fn clear(&mut self) -> Vec<u64> {
        self.intents
            .drain(..)
            .filter_map(|intent| intent.title_request())
            .collect()
    }
}

struct StudioApp {
    shell: StudioShell,
    state: StudioUiState,
    requests: Option<SyncSender<WorkerRequest>>,
    pending_intents: PendingIntents,
    updates: Option<StateUpdates>,
    worker: Option<JoinHandle<()>>,
    shutdown_sent: bool,
    osc: Option<OscReceiver>,
    osc_rejected: u64,
    osc_notice: Option<String>,
    /// Saves View › Language and the clock choice for the next launch.
    preferences: PreferenceStore,
    /// Why the preferences could not be read or saved, shown as a notice.
    preferences_notice: Option<String>,
}

impl StudioApp {
    fn new(
        config: StudioConfig,
        creation_context: &eframe::CreationContext<'_>,
    ) -> Result<Self, std::io::Error> {
        let osc = config.osc_listen.map(OscReceiver::bind).transpose()?;
        let mut shell = StudioShell::default();
        let env = |name: &str| std::env::var(name).ok();
        let path = preferences_path(env);
        let (saved, preferences_notice) = path.as_deref().map_or_else(
            || (SavedPreferences::default(), None),
            |path| match SavedPreferences::load(path) {
                Ok(saved) => (saved, None),
                Err(error) => (
                    SavedPreferences::default(),
                    Some(format!(
                        "Console preferences not read from {}: {error}",
                        path.display()
                    )),
                ),
            },
        );
        let locale = config.ui.locale_with(saved, env);
        shell.set_locale(locale);
        let preferences = PreferenceStore::new(path, locale);
        let (request_sender, request_receiver) = sync_channel(REQUEST_CAPACITY);
        let repaint_context = creation_context.egui_ctx.clone();
        let (publisher, updates) = state_mailbox(repaint_context);
        let worker = thread::Builder::new()
            .name("freemix-studio-worker".to_owned())
            .spawn(move || {
                run_worker(config, &request_receiver, &publisher);
            })?;
        Ok(Self {
            shell,
            state: StudioUiState::new(StudioConnectionStatus::Launching),
            requests: Some(request_sender),
            pending_intents: PendingIntents::new(),
            updates: Some(updates),
            worker: Some(worker),
            shutdown_sent: false,
            osc,
            osc_rejected: 0,
            osc_notice: None,
            preferences,
            preferences_notice,
        })
    }

    fn report_enqueue_error(&mut self, error: EnqueueError) {
        self.state.error = Some(error.to_string());
    }

    fn enqueue_intent(&mut self, intent: StudioIntent) {
        let title_request = intent.title_request();
        let result = self
            .requests
            .as_ref()
            .map_or(Err(EnqueueError::Disconnected), |sender| {
                self.pending_intents.submit(sender, intent)
            });
        if let Err(error) = result {
            // The worker will never answer a title command it did not get.
            if let Some(request) = title_request {
                self.shell
                    .receive_title_updates([TitleUpdate::Abandoned { request }]);
            }
            self.handle_enqueue_error(error);
        }
    }

    fn flush_pending_intents(&mut self) {
        let result = self
            .requests
            .as_ref()
            .map_or(Err(EnqueueError::Disconnected), |sender| {
                self.pending_intents.flush(sender)
            });
        if let Err(error) = result {
            self.handle_enqueue_error(error);
        }
    }

    fn handle_enqueue_error(&mut self, error: EnqueueError) {
        if error == EnqueueError::Disconnected {
            let abandoned = self.pending_intents.clear();
            self.shell.receive_title_updates(
                abandoned
                    .into_iter()
                    .map(|request| TitleUpdate::Abandoned { request }),
            );
            self.requests.take();
        }
        self.report_enqueue_error(error);
    }

    fn check_worker_health(&mut self) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if !worker.is_finished() {
            return;
        }
        let worker = self.worker.take().expect("worker handle was just checked");
        let failed = worker.join().is_err();
        // Nothing will answer the title commands it was pursuing.
        self.shell.abandon_title_requests();
        if !self.shutdown_sent {
            reconcile_finished_worker(&mut self.state, &mut self.updates, failed);
        }
    }

    fn send_shutdown(&mut self) {
        if !self.shutdown_sent {
            if let Some(requests) = &self.requests {
                let _ = try_enqueue(requests, WorkerRequest::Shutdown);
            }
            self.shutdown_sent = true;
        }
    }

    fn drain_osc(&mut self) {
        for _ in 0..8 {
            let Some(action) = self.osc.as_ref().and_then(|osc| osc.try_recv().ok()) else {
                break;
            };
            let action = match action {
                OscAction::SelectPreview(number) => ExternalStudioAction::SelectPreview(number),
                OscAction::Cut => ExternalStudioAction::Cut,
                OscAction::Fade => ExternalStudioAction::Fade,
                OscAction::AlphaFade => ExternalStudioAction::AlphaFade,
                OscAction::Slide => ExternalStudioAction::Slide,
                OscAction::Zoom => ExternalStudioAction::Zoom,
                OscAction::Wipe => ExternalStudioAction::Wipe,
                OscAction::FadeToBlack { active } => ExternalStudioAction::FadeToBlack { active },
                OscAction::CommitManualTransition => ExternalStudioAction::CommitManualTransition,
                OscAction::CancelManualTransition => ExternalStudioAction::CancelManualTransition,
            };
            let intent = external_intent(
                &self.state,
                action,
                self.shell.transition_duration_frames(),
                self.shell.fade_to_black_duration_frames(),
            );
            if let Some(intent) = intent {
                self.enqueue_intent(intent);
            } else {
                self.osc_rejected = self.osc_rejected.saturating_add(1);
            }
        }
        if let Some(osc) = &self.osc {
            let counters = osc.counters();
            let rejected = counters.rejected.saturating_add(self.osc_rejected);
            if counters.failed > 0 {
                self.osc_notice = Some("OSC receive failed".to_owned());
            } else if counters.malformed > 0 || rejected > 0 || counters.overflow > 0 {
                self.osc_notice = Some(format!(
                    "OSC malformed={} rejected={} overflow={}",
                    counters.malformed, rejected, counters.overflow
                ));
            }
            self.state.external_notice = self.osc_notice.clone();
        }
    }
}

impl eframe::App for StudioApp {
    fn logic(&mut self, _context: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(updates) = &self.updates
            && let Some(mut state) = updates.try_recv()
        {
            self.shell
                .receive_title_updates(std::mem::take(&mut state.title_updates));
            self.state = state;
        }
        #[cfg(feature = "capture-meter-signal")]
        crate::capture_signal::inject(&mut self.state);
        self.check_worker_health();
        self.drain_osc();
        if self.state.external_notice.is_none() {
            self.state
                .external_notice
                .clone_from(&self.preferences_notice);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.flush_pending_intents();
        for intent in self.shell.draw(ui, &self.state) {
            self.enqueue_intent(intent);
        }
        if let Err(error) = self.preferences.observe(self.shell.locale()) {
            self.preferences_notice = Some(format!("Console preferences not saved: {error}"));
        }
    }

    fn on_exit(&mut self) {
        self.send_shutdown();
        if let Some(osc) = self.osc.take() {
            let _ = osc.shutdown();
        }
    }
}

impl Drop for StudioApp {
    fn drop(&mut self) {
        self.send_shutdown();
        self.requests.take();
        if let Some(osc) = self.osc.take() {
            let _ = osc.shutdown();
        }
        self.updates.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Settles the UI state once the worker thread has finished without a
/// requested shutdown. The worker's last publication (such as a startup
/// failure) can land between this frame's mailbox read and the finish check,
/// so it is drained before the mailbox is dropped; otherwise its specific
/// error would be replaced by a generic one.
fn reconcile_finished_worker(
    state: &mut StudioUiState,
    updates: &mut Option<StateUpdates>,
    panicked: bool,
) {
    if let Some(mut last) = updates.take().and_then(|updates| updates.try_recv()) {
        // The worker is gone; its title answers no longer matter.
        last.title_updates.clear();
        *state = last;
    }
    if panicked {
        *state = StudioUiState::new(StudioConnectionStatus::Failed);
        state.error = Some("Studio worker panicked".to_owned());
    } else if state.connection_status != StudioConnectionStatus::Failed {
        *state = StudioUiState::new(StudioConnectionStatus::Failed);
        state.error = Some("Studio worker stopped unexpectedly".to_owned());
    }
}

struct StatePublisher {
    latest: Arc<Mutex<Option<StudioUiState>>>,
    sender: SyncSender<()>,
    repaint_context: egui::Context,
}

struct StateUpdates {
    latest: Arc<Mutex<Option<StudioUiState>>>,
    receiver: Receiver<()>,
}

fn state_mailbox(repaint_context: egui::Context) -> (StatePublisher, StateUpdates) {
    let latest = Arc::new(Mutex::new(None));
    let (sender, receiver) = sync_channel(STATE_NOTIFICATION_CAPACITY);
    (
        StatePublisher {
            latest: Arc::clone(&latest),
            sender,
            repaint_context,
        },
        StateUpdates { latest, receiver },
    )
}

impl StatePublisher {
    /// Replaces the latest state. Title updates are delivered exactly once:
    /// a state the UI has not taken yet hands its updates on to this one.
    fn publish(&self, mut state: StudioUiState) -> bool {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = latest.take()
            && !previous.title_updates.is_empty()
        {
            let mut updates = previous.title_updates;
            updates.append(&mut state.title_updates);
            state.title_updates = updates;
        }
        *latest = Some(state);
        drop(latest);
        match self.sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {
                self.repaint_context.request_repaint();
                true
            }
            Err(TrySendError::Disconnected(())) => false,
        }
    }
}

impl StateUpdates {
    fn try_recv(&self) -> Option<StudioUiState> {
        self.receiver.try_recv().ok()?;
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

#[derive(Debug)]
struct IdempotencyKeys {
    nonce: String,
    next: u64,
}

impl IdempotencyKeys {
    fn new(nonce: impl Into<String>) -> Self {
        Self {
            nonce: nonce.into(),
            next: 0,
        }
    }

    fn next(&mut self) -> Option<String> {
        self.next = self.next.checked_add(1)?;
        Some(format!("{}:{}", self.nonce, self.next))
    }
}

fn worker_nonce() -> String {
    let started_at_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_WORKER_NONCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "freemix-studio-{}-{started_at_nanos}-{sequence}",
        std::process::id()
    )
}

#[derive(Clone, Copy, Debug)]
struct ReconnectWait {
    started: Instant,
    delay: Duration,
}

impl ReconnectWait {
    fn from_runtime(runtime: &StudioRuntime) -> Option<Self> {
        runtime.session().reconnect_backoff().map(|backoff| Self {
            started: Instant::now(),
            delay: Duration::from_millis(backoff.delay_ms),
        })
    }

    fn remaining(self) -> Duration {
        self.delay.saturating_sub(self.started.elapsed())
    }
}

#[derive(Debug)]
enum WorkerFailure {
    Transport(String),
    Resync(String),
    Fatal(String),
    Shutdown,
}

impl WorkerFailure {
    fn message(&self) -> &str {
        match self {
            Self::Transport(message) | Self::Resync(message) | Self::Fatal(message) => message,
            Self::Shutdown => "Studio worker shut down",
        }
    }

    const fn is_recoverable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Resync(_))
    }

    const fn invalidates_realization(&self) -> bool {
        self.is_recoverable()
    }
}

#[derive(Debug)]
struct WorkerRecovery {
    pending_command: Option<String>,
    deferred_intents: VecDeque<StudioIntent>,
    reconnect_wait: Option<ReconnectWait>,
    visible_error: Option<String>,
    realization_uncertain: bool,
    deferred_rejections: usize,
    // Never evicted during a worker lifetime; restart is the explicit clear operation.
    terminal_uncertainties: Vec<TerminalUncertaintyNotice>,
}

impl WorkerRecovery {
    fn new(visible_error: Option<String>, reconnect_wait: Option<ReconnectWait>) -> Self {
        Self {
            pending_command: None,
            deferred_intents: VecDeque::new(),
            reconnect_wait,
            visible_error,
            realization_uncertain: false,
            deferred_rejections: 0,
            terminal_uncertainties: Vec::new(),
        }
    }

    fn active(&self) -> bool {
        self.reconnect_wait.is_some() || self.pending_command.is_some()
    }

    fn defer_intent(
        &mut self,
        runtime: &mut StudioRuntime,
        intent: StudioIntent,
        publisher: &StatePublisher,
    ) -> bool {
        if let Some(tail) = self.deferred_intents.back_mut()
            && coalesce_adjacent_intent(tail, intent.clone())
        {
            return publish_deferred_runtime(
                runtime,
                publisher,
                &self.deferred_intents,
                self.error(),
                &self.terminal_uncertainties,
            );
        }
        if self.deferred_intents.len() >= DEFERRED_INTENT_CAPACITY
            && !exceeds_capacity_allowed(&intent)
        {
            self.deferred_rejections = self.deferred_rejections.saturating_add(1);
            return publish_runtime_with_uncertainties(
                runtime,
                publisher,
                self.error(),
                &self.terminal_uncertainties,
            );
        }
        self.deferred_intents.push_back(intent);
        publish_deferred_runtime(
            runtime,
            publisher,
            &self.deferred_intents,
            self.error(),
            &self.terminal_uncertainties,
        )
    }

    fn error(&self) -> Option<String> {
        combined_error(
            join_errors(self.visible_error.clone(), self.terminal_error()),
            self.deferred_rejections,
        )
    }

    fn terminal_error(&self) -> Option<String> {
        (!self.terminal_uncertainties.is_empty()).then(|| {
            let commands = self
                .terminal_uncertainties
                .iter()
                .map(|notice| {
                    format!(
                        "{:?} (server receipt {:?})",
                        notice.command_id, notice.received_command_id
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("Terminal command uncertainty remains after authoritative resync: {commands}")
        })
    }

    fn capture_pending_terminal_uncertainty(&mut self, runtime: &StudioRuntime) -> bool {
        let Some(command_id) = self.pending_command.as_ref() else {
            return false;
        };
        let Some(CommandStatus::TerminalUncertain(
            CommandUncertainty::IdempotencyReplayCollision {
                received_command_id,
            },
        )) = runtime
            .session()
            .client()
            .command(command_id)
            .map(|record| &record.status)
        else {
            return false;
        };
        if self
            .terminal_uncertainties
            .iter()
            .any(|notice| notice.command_id == *command_id)
        {
            return true;
        }
        if self.terminal_uncertainties.len() == TERMINAL_UNCERTAINTY_CAPACITY {
            self.visible_error = Some(format!(
                "Terminal uncertainty ledger reached capacity {TERMINAL_UNCERTAINTY_CAPACITY}; restart Studio to explicitly clear operator history"
            ));
            return false;
        }
        self.terminal_uncertainties.push(TerminalUncertaintyNotice {
            command_id: command_id.clone(),
            received_command_id: received_command_id.clone(),
        });
        true
    }
}

fn join_errors(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(first), None) => Some(first),
        (None, second) => second,
    }
}

fn combined_error(transient: Option<String>, deferred_rejections: usize) -> Option<String> {
    let overflow = (deferred_rejections > 0).then(|| {
        format!(
            "Rejected {deferred_rejections} command(s): Studio deferred command queue reached capacity {DEFERRED_INTENT_CAPACITY}"
        )
    });
    match (transient, overflow) {
        (Some(transient), Some(overflow)) => Some(format!("{transient}; {overflow}")),
        (Some(transient), None) => Some(transient),
        (None, overflow) => overflow,
    }
}

fn cancellation_requested(
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
) -> bool {
    match requests.try_recv() {
        Ok(WorkerRequest::Shutdown) | Err(TryRecvError::Disconnected) => true,
        Ok(WorkerRequest::Intent(intent)) => {
            if let Some(tail) = deferred_intents.back_mut()
                && coalesce_adjacent_intent(tail, intent.clone())
            {
                return false;
            }
            if deferred_intents.len() >= DEFERRED_INTENT_CAPACITY
                && !exceeds_capacity_allowed(&intent)
            {
                *deferred_rejections = deferred_rejections.saturating_add(1);
            } else {
                deferred_intents.push_back(intent);
            }
            false
        }
        Err(TryRecvError::Empty) => false,
    }
}

fn connect_worker(
    runtime: &mut StudioRuntime,
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
) -> Option<Result<SessionEvent, crate::StudioError>> {
    let started = Instant::now();
    let mut shutdown = false;
    let result = runtime.connect_cancellable(CONNECT_TIMEOUT, IO_POLL_INTERVAL, || {
        shutdown = cancellation_requested(requests, deferred_intents, deferred_rejections);
        shutdown || started.elapsed() >= PEER_WAIT_TIMEOUT
    });
    (!shutdown).then_some(result)
}

fn reconnect_worker(
    runtime: &mut StudioRuntime,
    elapsed_backoff: Duration,
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
) -> Option<Result<SessionEvent, crate::StudioError>> {
    let started = Instant::now();
    let mut shutdown = false;
    let result =
        runtime.reconnect_cancellable(elapsed_backoff, CONNECT_TIMEOUT, IO_POLL_INTERVAL, || {
            shutdown = cancellation_requested(requests, deferred_intents, deferred_rejections);
            shutdown || started.elapsed() >= PEER_WAIT_TIMEOUT
        });
    (!shutdown).then_some(result)
}

fn start_worker_runtime(
    config: StudioConfig,
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
) -> Option<Result<StudioRuntime, crate::StudioError>> {
    let started = Instant::now();
    let mut shutdown = false;
    let result = StudioRuntime::new_cancellable(config, IO_POLL_INTERVAL, || {
        shutdown = cancellation_requested(requests, deferred_intents, deferred_rejections);
        shutdown || started.elapsed() >= PEER_WAIT_TIMEOUT
    });
    (!shutdown).then_some(result)
}

fn startup_failure_message(error: &crate::StudioError, deferred_rejections: usize) -> String {
    combined_error(
        Some(format!("Studio startup failed: {error}")),
        deferred_rejections,
    )
    .expect("startup failure always has an error")
}

fn initialize_worker(
    config: StudioConfig,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
) -> Option<(StudioRuntime, WorkerRecovery)> {
    if !publisher.publish(StudioUiState::new(StudioConnectionStatus::Launching)) {
        return None;
    }
    let mut deferred_intents = VecDeque::new();
    let mut deferred_rejections = 0;
    let startup_result = start_worker_runtime(
        config,
        requests,
        &mut deferred_intents,
        &mut deferred_rejections,
    )?;
    let mut runtime = match startup_result {
        Ok(runtime) => runtime,
        Err(error) => {
            let mut state = StudioUiState::new(StudioConnectionStatus::Failed);
            state.error = Some(startup_failure_message(&error, deferred_rejections));
            publisher.publish(state);
            return None;
        }
    };
    if !publish_runtime(
        &mut runtime,
        publisher,
        combined_error(None, deferred_rejections),
    ) {
        return None;
    }
    let connect_result = connect_worker(
        &mut runtime,
        requests,
        &mut deferred_intents,
        &mut deferred_rejections,
    )?;
    let (visible_error, reconnect_wait) = match connect_result {
        Ok(_) => (None, None),
        Err(error) => {
            let reconnect_wait = is_recoverable_failure(&error)
                .then(|| ReconnectWait::from_runtime(&runtime))
                .flatten();
            (Some(format!("Connection failed: {error}")), reconnect_wait)
        }
    };
    let mut recovery = WorkerRecovery::new(visible_error, reconnect_wait);
    recovery.deferred_intents = deferred_intents;
    recovery.deferred_rejections = deferred_rejections;
    if !publish_recovery_runtime(&mut runtime, publisher, &recovery) {
        return None;
    }
    Some((runtime, recovery))
}

#[allow(clippy::too_many_lines)]
fn run_worker(
    config: StudioConfig,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
) {
    let Some((mut runtime, mut recovery)) = initialize_worker(config, requests, publisher) else {
        return;
    };
    let started = Instant::now();
    let mut next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
    let mut keys = IdempotencyKeys::new(worker_nonce());

    // Poll requests before one bounded socket read so neither source starves.
    loop {
        if !recovery.active()
            && let Some(intent) = recovery.deferred_intents.pop_front()
        {
            if !handle_worker_intent(
                &mut runtime,
                intent,
                &mut keys,
                requests,
                publisher,
                &mut recovery,
            ) {
                break;
            }
            continue;
        }
        match requests.try_recv() {
            Ok(WorkerRequest::Shutdown) | Err(TryRecvError::Disconnected) => break,
            Ok(WorkerRequest::Intent(intent)) => {
                if !recovery.defer_intent(&mut runtime, intent, publisher) {
                    break;
                }
                continue;
            }
            Err(TryRecvError::Empty) => {}
        }
        if !recovery.active()
            && recovery.reconnect_wait.is_none()
            && Instant::now() < next_heartbeat
            && runtime.session().is_connected()
        {
            let publish = match runtime.receive_timeout(IO_POLL_INTERVAL) {
                Ok(Some(
                    SessionEvent::Event { .. }
                    | SessionEvent::RuntimeEvent { .. }
                    | SessionEvent::AudioMeters { .. }
                    | SessionEvent::StreamStatus { .. },
                )) => true,
                // Lossy monitor parts publish only once a frame completes.
                Ok(Some(SessionEvent::MonitorFrame { complete, .. })) => complete,
                Ok(Some(SessionEvent::Disconnected { cause, .. })) => {
                    recovery.visible_error = Some(format!("Disconnected while idle: {cause:?}"));
                    recovery.reconnect_wait = ReconnectWait::from_runtime(&runtime);
                    true
                }
                Ok(Some(SessionEvent::DurableGap { .. })) => {
                    recovery.realization_uncertain =
                        runtime.session().client().model().view().is_some();
                    recovery.reconnect_wait = ReconnectWait::from_runtime(&runtime);
                    true
                }
                Ok(Some(other)) => {
                    recovery.visible_error = Some(format!("Unexpected idle response: {other:?}"));
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    recovery.visible_error = Some(format!("Idle receive failed: {error}"));
                    if is_recoverable_failure(&error) {
                        recovery.reconnect_wait = ReconnectWait::from_runtime(&runtime);
                    }
                    true
                }
            };
            if publish && !publish_recovery_runtime(&mut runtime, publisher, &recovery) {
                break;
            }
            continue;
        }
        let now = Instant::now();
        let timeout = recovery.reconnect_wait.map_or_else(
            || next_heartbeat.saturating_duration_since(now),
            ReconnectWait::remaining,
        );
        match requests.recv_timeout(timeout) {
            Ok(WorkerRequest::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(WorkerRequest::Intent(intent)) => {
                if recovery.active() || !recovery.deferred_intents.is_empty() {
                    if !recovery.defer_intent(&mut runtime, intent, publisher) {
                        break;
                    }
                    continue;
                }
                if !handle_worker_intent(
                    &mut runtime,
                    intent,
                    &mut keys,
                    requests,
                    publisher,
                    &mut recovery,
                ) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !handle_worker_timeout(
                    &mut runtime,
                    requests,
                    publisher,
                    &mut recovery,
                    &mut next_heartbeat,
                    started,
                ) {
                    break;
                }
            }
        }
    }
}

fn handle_worker_intent(
    runtime: &mut StudioRuntime,
    intent: StudioIntent,
    keys: &mut IdempotencyKeys,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
    recovery: &mut WorkerRecovery,
) -> bool {
    let title_request = intent.title_request();
    if recovery.terminal_uncertainties.len() == TERMINAL_UNCERTAINTY_CAPACITY {
        recovery.visible_error = Some(format!(
            "Command not sent: terminal uncertainty ledger reached capacity {TERMINAL_UNCERTAINTY_CAPACITY}; restart Studio to explicitly clear operator history"
        ));
        if let Some(request) = title_request {
            runtime.titles_mut().unsent(request);
        }
        return publish_recovery_runtime(runtime, publisher, recovery);
    }
    let command_id = match begin_intent(
        runtime,
        intent,
        keys,
        publisher,
        recovery.error(),
        &recovery.terminal_uncertainties,
    ) {
        Ok(command_id) => command_id,
        Err(error) => {
            if let Some(request) = title_request {
                runtime.titles_mut().unsent(request);
            }
            recovery.visible_error = Some(error);
            return publish_runtime_with_uncertainties(
                runtime,
                publisher,
                recovery.error(),
                &recovery.terminal_uncertainties,
            );
        }
    };
    recovery.pending_command = Some(command_id.clone());
    let persistent_error = recovery.terminal_error();
    let terminal_uncertainties = recovery.terminal_uncertainties.clone();
    let publication = CommandPublication {
        publisher,
        publish_updates: true,
        persistent_error: persistent_error.as_deref(),
        terminal_uncertainties: &terminal_uncertainties,
    };
    let result = flush_worker(runtime, requests, recovery).and_then(|()| {
        consume_command_sequence(
            runtime,
            &command_id,
            publication,
            requests,
            &mut recovery.deferred_intents,
            &mut recovery.deferred_rejections,
        )
    });
    match result {
        Ok(()) => {
            recovery.pending_command = None;
            recovery.visible_error = None;
        }
        Err(WorkerFailure::Shutdown) => return false,
        Err(error) => {
            recovery.visible_error = Some(error.message().to_owned());
            if recovery.capture_pending_terminal_uncertainty(runtime) {
                recovery.visible_error = None;
            }
            if error.invalidates_realization() {
                recovery.realization_uncertain = true;
            }
            recovery.reconnect_wait = if error.is_recoverable() {
                ReconnectWait::from_runtime(runtime)
            } else {
                recovery.pending_command = None;
                None
            };
        }
    }
    (recovery.error().is_none() && recovery.deferred_intents.is_empty())
        || publish_recovery_runtime(runtime, publisher, recovery)
}

fn flush_worker(
    runtime: &mut StudioRuntime,
    requests: &Receiver<WorkerRequest>,
    recovery: &mut WorkerRecovery,
) -> Result<(), WorkerFailure> {
    let started = Instant::now();
    let mut shutdown = false;
    let result = runtime.flush_cancellable(IO_POLL_INTERVAL, || {
        shutdown = cancellation_requested(
            requests,
            &mut recovery.deferred_intents,
            &mut recovery.deferred_rejections,
        );
        shutdown || started.elapsed() >= PEER_WAIT_TIMEOUT
    });
    if shutdown {
        Err(WorkerFailure::Shutdown)
    } else {
        result
            .map(|_| ())
            .map_err(|error| worker_error("Could not send command", &error))
    }
}

fn handle_worker_timeout(
    runtime: &mut StudioRuntime,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
    recovery: &mut WorkerRecovery,
    next_heartbeat: &mut Instant,
    started: Instant,
) -> bool {
    if let Some(wait) = recovery.reconnect_wait {
        if !wait.remaining().is_zero() {
            return true;
        }
        let Some(reconnect_result) = reconnect_worker(
            runtime,
            wait.started.elapsed(),
            requests,
            &mut recovery.deferred_intents,
            &mut recovery.deferred_rejections,
        ) else {
            return false;
        };
        match reconnect_result {
            Ok(SessionEvent::Connected { mode }) => {
                recovery.reconnect_wait = None;
                recovery.visible_error = None;
                *next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
                if mode == SyncMode::Snapshot {
                    recovery.realization_uncertain = false;
                }
                if !recovery.realization_uncertain
                    && !publish_recovery_runtime(runtime, publisher, recovery)
                {
                    return false;
                }

                if !resume_pending_command(runtime, requests, publisher, recovery) {
                    return false;
                }
            }
            Ok(other) => {
                recovery.visible_error = Some(format!("Unexpected reconnect result: {other:?}"));
                recovery.pending_command = None;
                recovery.reconnect_wait = None;
            }
            Err(error) => {
                recovery.visible_error = Some(format!("Reconnect failed: {error}"));
                if is_resync_failure(&error) {
                    recovery.realization_uncertain = true;
                }
                recovery.reconnect_wait = if is_recoverable_failure(&error) {
                    ReconnectWait::from_runtime(runtime)
                } else {
                    recovery.pending_command = None;
                    None
                };
            }
        }
        return publish_recovery_runtime(runtime, publisher, recovery);
    }

    handle_heartbeat_timeout(
        runtime,
        requests,
        publisher,
        recovery,
        next_heartbeat,
        started,
    )
}

fn resume_pending_command(
    runtime: &mut StudioRuntime,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
    recovery: &mut WorkerRecovery,
) -> bool {
    if let Some(command_id) = recovery.pending_command.clone() {
        let terminal = runtime
            .session()
            .client()
            .command(&command_id)
            .is_some_and(|record| record.status.is_terminal());
        if terminal {
            recovery.pending_command = None;
        } else {
            let persistent_error = recovery.terminal_error();
            let terminal_uncertainties = recovery.terminal_uncertainties.clone();
            let publication = CommandPublication {
                publisher,
                publish_updates: !recovery.realization_uncertain,
                persistent_error: persistent_error.as_deref(),
                terminal_uncertainties: &terminal_uncertainties,
            };
            let result = consume_command_sequence(
                runtime,
                &command_id,
                publication,
                requests,
                &mut recovery.deferred_intents,
                &mut recovery.deferred_rejections,
            );
            match result {
                Ok(()) => recovery.pending_command = None,
                Err(WorkerFailure::Shutdown) => return false,
                Err(error) => {
                    recovery.visible_error = Some(error.message().to_owned());
                    if recovery.capture_pending_terminal_uncertainty(runtime) {
                        recovery.visible_error = None;
                    }
                    if error.invalidates_realization() {
                        recovery.realization_uncertain = true;
                    }
                    recovery.reconnect_wait = if error.is_recoverable() {
                        ReconnectWait::from_runtime(runtime)
                    } else {
                        recovery.pending_command = None;
                        None
                    };
                }
            }
        }
    }
    if recovery.reconnect_wait.is_none() && recovery.realization_uncertain {
        match require_snapshot_reconnect(runtime) {
            Ok(wait) => {
                recovery.reconnect_wait = Some(wait);
                recovery.visible_error = Some(
                    "Runtime realization changed during reconnect; requesting snapshot".to_owned(),
                );
            }
            Err(error) => {
                recovery.visible_error = Some(error.message().to_owned());
                recovery.pending_command = None;
            }
        }
    }
    true
}

fn handle_heartbeat_timeout(
    runtime: &mut StudioRuntime,
    requests: &Receiver<WorkerRequest>,
    publisher: &StatePublisher,
    recovery: &mut WorkerRecovery,
    next_heartbeat: &mut Instant,
    started: Instant,
) -> bool {
    let lifecycle = runtime.lifecycle();
    if let Ok(LifecycleState::DaemonExited { code }) = &lifecycle {
        recovery.realization_uncertain = runtime.session().client().model().view().is_some();
        let _ = runtime.session_mut().disconnect();
        recovery.reconnect_wait = ReconnectWait::from_runtime(runtime);
        recovery.visible_error = Some(format!(
            "Supervised daemon exited with code {code:?}; reconnecting after bounded backoff"
        ));
        if !publish_recovery_runtime(runtime, publisher, recovery) {
            return false;
        }
    }
    if matches!(lifecycle, Ok(LifecycleState::Ready)) {
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let wait_started = Instant::now();
        let mut shutdown = false;
        let heartbeat = runtime.send_heartbeat_cancellable(elapsed_ms, IO_POLL_INTERVAL, || {
            shutdown = cancellation_requested(
                requests,
                &mut recovery.deferred_intents,
                &mut recovery.deferred_rejections,
            );
            shutdown || wait_started.elapsed() >= PEER_WAIT_TIMEOUT
        });
        let result = heartbeat
            .map_err(|error| worker_error("Heartbeat failed", &error))
            .and_then(|heartbeat| {
                let event = loop {
                    let event = runtime
                        .receive_cancellable(IO_POLL_INTERVAL, || {
                            shutdown = cancellation_requested(
                                requests,
                                &mut recovery.deferred_intents,
                                &mut recovery.deferred_rejections,
                            );
                            shutdown || wait_started.elapsed() >= PEER_WAIT_TIMEOUT
                        })
                        .map_err(|error| {
                            worker_error("Heartbeat acknowledgement failed", &error)
                        })?;
                    let publish = match &event {
                        SessionEvent::AudioMeters { .. }
                        | SessionEvent::RuntimeEvent { .. }
                        | SessionEvent::Event { .. }
                        | SessionEvent::StreamStatus { .. } => Some(true),
                        SessionEvent::MonitorFrame { complete, .. } => Some(*complete),
                        _ => None,
                    };
                    if let Some(publish) = publish {
                        if publish && !publish_recovery_runtime(runtime, publisher, recovery) {
                            return Err(WorkerFailure::Fatal("Studio UI disconnected".to_owned()));
                        }
                        continue;
                    }
                    break event;
                };
                match event {
                    SessionEvent::HeartbeatAcknowledged { acknowledgement }
                        if acknowledgement.heartbeat_sequence == heartbeat.sequence =>
                    {
                        Ok(())
                    }
                    other => Err(unexpected_failure("heartbeat acknowledgement", &other)),
                }
            });
        if shutdown {
            return false;
        }
        if let Err(error) = result {
            recovery.visible_error = Some(error.message().to_owned());
            if error.is_recoverable() {
                recovery.realization_uncertain =
                    runtime.session().client().model().view().is_some();
                recovery.reconnect_wait = ReconnectWait::from_runtime(runtime);
            }
            if !publish_runtime_with_uncertainties(
                runtime,
                publisher,
                recovery.error(),
                &recovery.terminal_uncertainties,
            ) {
                return false;
            }
        }
    }
    *next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
    true
}

fn begin_intent(
    runtime: &mut StudioRuntime,
    intent: StudioIntent,
    keys: &mut IdempotencyKeys,
    publisher: &StatePublisher,
    persistent_error: Option<String>,
    terminal_uncertainties: &[TerminalUncertaintyNotice],
) -> Result<String, String> {
    let view = runtime.session().client().model().view();
    let title = match &intent {
        StudioIntent::Title(command) => Some(command.clone()),
        _ => None,
    };
    let payload = intent_payload(intent, view.as_ref())?;
    let expected_revision = view
        .as_ref()
        .map(|view| view.cursor.revision.get())
        .ok_or_else(|| "Cannot send a command before project state is synchronized".to_owned())?;
    let key = keys
        .next()
        .ok_or_else(|| "Studio idempotency key space is exhausted".to_owned())?;
    // A title command carries its own base revision; an unrelated project
    // change must not refuse it.
    let expected_revision = title.is_none().then_some(expected_revision);
    let command = runtime
        .queue_command(payload, key, expected_revision, None)
        .map_err(|error| format!("Could not queue command: {error}"))?;
    if let Some(title) = &title {
        runtime.titles_mut().track(&command.id, title);
    }

    // This publication exposes SelectPreview's optimistic desired state before I/O.
    if !publish_runtime_with_uncertainties(
        runtime,
        publisher,
        persistent_error,
        terminal_uncertainties,
    ) {
        return Err("Studio UI disconnected".to_owned());
    }
    Ok(command.id)
}

fn resolve_input_audio_strip(
    view: Option<&ClientView>,
    input: fm_types::InputId,
    update: InputAudioStripUpdate,
) -> Result<CommandPayload, String> {
    let view =
        view.ok_or_else(|| "Cannot edit audio before project state is synchronized".to_owned())?;
    let strip = view
        .input_audio_strips
        .iter()
        .find(|strip| strip.input == input)
        .ok_or_else(|| "Cannot edit audio: input is no longer available".to_owned())?;
    Ok(CommandPayload::SetInputAudioStrip {
        input: WireInputId::from_domain(input),
        gain_millidb: update.gain_millidb.unwrap_or(strip.gain_millidb),
        balance_basis_points: update
            .balance_basis_points
            .unwrap_or(strip.balance_basis_points),
        muted: update.muted.unwrap_or(strip.muted),
        soloed: update.soloed.unwrap_or(strip.soloed),
        follow_video: update.follow_video.unwrap_or(strip.follow_video),
        delay_samples: update.delay_samples.unwrap_or(strip.delay_samples),
    })
}

fn resolve_input_move(
    view: Option<&ClientView>,
    input: fm_types::InputId,
    direction: InputMoveDirection,
) -> Result<CommandPayload, String> {
    let view = view
        .ok_or_else(|| "Cannot reorder inputs before project state is synchronized".to_owned())?;
    let index = view
        .inputs
        .iter()
        .position(|candidate| *candidate == input)
        .ok_or_else(|| "Cannot reorder inputs: input is no longer available".to_owned())?;
    let neighbor = match direction {
        InputMoveDirection::Up => index.checked_sub(1),
        InputMoveDirection::Down => (index + 1 < view.inputs.len()).then_some(index + 1),
    }
    .ok_or_else(|| "Cannot reorder inputs: input is already at the edge".to_owned())?;
    let mut inputs = view.inputs.clone();
    inputs.swap(index, neighbor);
    Ok(CommandPayload::ReorderInputs {
        inputs: inputs.into_iter().map(WireInputId::from_domain).collect(),
    })
}

/// Resolves a stream action against the latest confirmed roster so a target
/// removed after the click is reported instead of sent.
fn resolve_stream_action(
    view: Option<&ClientView>,
    action: StreamAction,
) -> Result<CommandPayload, String> {
    let view = view
        .ok_or_else(|| "Cannot control a stream before project state is synchronized".to_owned())?;
    let target = action.target();
    if !view
        .streams
        .iter()
        .any(|stream| stream.target == target.get())
    {
        return Err("Stream target no longer exists".to_owned());
    }
    Ok(match action {
        StreamAction::Start(target) => CommandPayload::StreamStart { target },
        StreamAction::Stop(target) => CommandPayload::StreamStop { target },
    })
}

/// Studio's descriptor for a Stinger slot that has no configuration yet.
const DEFAULT_STINGER_PRELOAD: bool = true;
const DEFAULT_STINGER_AUDIO_POLICY: StingerAudioPolicy = StingerAudioPolicy::MixWithProgram;
const DEFAULT_STINGER_FALLBACK: StingerMissingMediaFallback = StingerMissingMediaFallback::Cut;

/// Resolves a media assignment from the latest confirmed slot: a configured
/// slot keeps every other setting, an unconfigured slot gets Studio defaults.
fn resolve_stinger_assignment(
    view: Option<&ClientView>,
    slot: fm_protocol::WireStingerSlotId,
    media_input: fm_types::InputId,
    default_cut_point_frames: u32,
) -> Result<CommandPayload, String> {
    let view = view.ok_or_else(|| {
        "Cannot configure a Stinger slot before project state is synchronized".to_owned()
    })?;
    if !view.inputs.contains(&media_input) {
        return Err("Stinger media input no longer exists".to_owned());
    }
    let current = view
        .stingers
        .iter()
        .find(|status| status.slot == slot.number());
    Ok(CommandPayload::ConfigureStinger {
        slot,
        media_input: WireInputId::from_domain(media_input),
        preload: current.map_or(DEFAULT_STINGER_PRELOAD, |status| status.preload),
        cut_point_frames: current
            .map_or(default_cut_point_frames, |status| status.cut_point_frames),
        audio_policy: current.map_or(DEFAULT_STINGER_AUDIO_POLICY, |status| status.audio_policy),
        missing_media_fallback: current.map_or(DEFAULT_STINGER_FALLBACK, |status| {
            status.missing_media_fallback
        }),
    })
}

fn desired_overlay(
    view: Option<&ClientView>,
    channel: fm_protocol::WireOverlayChannelId,
) -> Result<&fm_ui_model::OverlayStatus, String> {
    let view =
        view.ok_or_else(|| "Cannot edit overlay before project state is synchronized".to_owned())?;
    let overlay = view
        .desired_overlays
        .iter()
        .find(|overlay| overlay.channel == channel.number())
        .ok_or_else(|| "Cannot edit overlay: channel is no longer available".to_owned())?;
    Ok(overlay)
}

fn desired_overlay_output_included(
    view: Option<&ClientView>,
    channel: fm_protocol::WireOverlayChannelId,
    output: OutputId,
) -> Result<bool, String> {
    let view =
        view.ok_or_else(|| "Cannot edit overlay before project state is synchronized".to_owned())?;
    let overlay = desired_overlay(Some(view), channel)?;
    if !view
        .outputs
        .iter()
        .any(|candidate| candidate.output == output)
    {
        return Err("Cannot edit overlay: output is no longer available".to_owned());
    }
    Ok(overlay.included_outputs.contains(&output))
}

#[allow(clippy::too_many_lines)]
fn consume_command_sequence(
    runtime: &mut StudioRuntime,
    command_id: &str,
    publication: CommandPublication<'_>,
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
) -> Result<(), WorkerFailure> {
    let deadline = Instant::now()
        .checked_add(PEER_WAIT_TIMEOUT)
        .expect("peer wait timeout must fit in Instant");
    let mut consumed = 0;
    let result = loop {
        match receive_command_event(
            runtime,
            publication,
            requests,
            deferred_intents,
            deferred_rejections,
            deadline,
            &mut consumed,
        )? {
            SessionEvent::CommandResult { result, intake } => break (result, intake),
            SessionEvent::Event { .. } | SessionEvent::RuntimeEvent { .. } => {}
            other => return Err(unexpected_failure("command result", &other)),
        }
    };
    let (result, intake) = result;
    if result_id(&result) != command_id {
        return Err(WorkerFailure::Fatal(format!(
            "Unexpected command result ID {:?}; expected {command_id:?}",
            result_id(&result)
        )));
    }
    let accepted_revision = match &result {
        CommandResult::Accepted { revision, .. } => *revision,
        CommandResult::Rejected {
            code,
            message,
            fields,
            ..
        } => {
            let refusal = catalogue_refusal(runtime.session().client(), command_id, &result);
            publication.rejection(
                runtime,
                fm_ui_egui::command_rejection_message(code, message, fields),
                refusal,
                *deferred_rejections,
            );
            return Ok(());
        }
    };
    publication.update(runtime, *deferred_rejections)?;
    // Snapshot recovery already carries realized state. Resume recovery keeps
    // realization uncertain and forces a snapshot after this replay.
    if matches!(intake, fm_client::Intake::DuplicateResult)
        && runtime
            .session()
            .client()
            .last_applied_cursor()
            .is_some_and(|cursor| cursor.revision >= accepted_revision)
    {
        return Ok(());
    }

    loop {
        match receive_command_event(
            runtime,
            publication,
            requests,
            deferred_intents,
            deferred_rejections,
            deadline,
            &mut consumed,
        )? {
            SessionEvent::Event { event, .. } if event.cursor.revision == accepted_revision => {
                break;
            }
            SessionEvent::Event { event, .. } if event.cursor.revision < accepted_revision => {}
            SessionEvent::Event { event, .. } => {
                return Err(WorkerFailure::Fatal(format!(
                    "Unexpected durable event revision {}; expected {accepted_revision}",
                    event.cursor.revision
                )));
            }
            SessionEvent::RuntimeEvent { .. } => {}
            other => return Err(unexpected_failure("durable event", &other)),
        }
    }
    publication.update(runtime, *deferred_rejections)?;

    loop {
        match receive_command_event(
            runtime,
            publication,
            requests,
            deferred_intents,
            deferred_rejections,
            deadline,
            &mut consumed,
        )? {
            SessionEvent::RuntimeEvent { event, .. } if event.revision == accepted_revision => {
                break;
            }
            SessionEvent::RuntimeEvent { event, .. } if event.revision < accepted_revision => {}
            SessionEvent::RuntimeEvent { event, .. } => {
                return Err(WorkerFailure::Fatal(format!(
                    "Unexpected runtime event revision {}; expected {accepted_revision}",
                    event.revision
                )));
            }
            SessionEvent::Event { .. } => {}
            other => return Err(unexpected_failure("runtime event", &other)),
        }
    }
    publication.update(runtime, *deferred_rejections)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct CommandPublication<'a> {
    publisher: &'a StatePublisher,
    publish_updates: bool,
    persistent_error: Option<&'a str>,
    terminal_uncertainties: &'a [TerminalUncertaintyNotice],
}

impl CommandPublication<'_> {
    fn update(
        self,
        runtime: &mut StudioRuntime,
        deferred_rejections: usize,
    ) -> Result<(), WorkerFailure> {
        if self.publish_updates
            && !publish_runtime_with_uncertainties(
                runtime,
                self.publisher,
                combined_error(
                    self.persistent_error.map(str::to_owned),
                    deferred_rejections,
                ),
                self.terminal_uncertainties,
            )
        {
            Err(WorkerFailure::Fatal("Studio UI disconnected".to_owned()))
        } else {
            Ok(())
        }
    }

    fn rejection(
        self,
        runtime: &mut StudioRuntime,
        error: String,
        catalogue_refusal: Option<CatalogueRefusal>,
        deferred_rejections: usize,
    ) {
        if self.publish_updates {
            let mut state = runtime_state(
                runtime,
                combined_error(
                    join_errors(Some(error), self.persistent_error.map(str::to_owned)),
                    deferred_rejections,
                ),
            );
            state.terminal_uncertainties = self.terminal_uncertainties.to_vec();
            state.catalogue_refusal = catalogue_refusal;
            self.publisher.publish(state);
        }
    }
}

fn count_record(consumed: &mut usize) -> Result<(), WorkerFailure> {
    *consumed = consumed.saturating_add(1);
    if *consumed > MAX_COMMAND_RECORDS {
        Err(WorkerFailure::Fatal(
            "Command response exceeded the record limit".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn receive_command_event(
    runtime: &mut StudioRuntime,
    publication: CommandPublication<'_>,
    requests: &Receiver<WorkerRequest>,
    deferred_intents: &mut VecDeque<StudioIntent>,
    deferred_rejections: &mut usize,
    deadline: Instant,
    consumed: &mut usize,
) -> Result<SessionEvent, WorkerFailure> {
    let mut shutdown = false;
    loop {
        let result = runtime.receive_cancellable(IO_POLL_INTERVAL, || {
            shutdown = cancellation_requested(requests, deferred_intents, deferred_rejections);
            shutdown || Instant::now() >= deadline
        });
        if shutdown {
            return Err(WorkerFailure::Shutdown);
        }
        let event = result.map_err(|error| worker_error("Command response failed", &error))?;
        if matches!(
            event,
            SessionEvent::AudioMeters { .. } | SessionEvent::StreamStatus { .. }
        ) {
            publication.update(runtime, *deferred_rejections)?;
            continue;
        }
        // Lossy monitor parts are status, never command records: they are not
        // counted and publish only once a frame completes.
        if let SessionEvent::MonitorFrame { complete, .. } = event {
            if complete {
                publication.update(runtime, *deferred_rejections)?;
            }
            continue;
        }
        count_record(consumed)?;
        if matches!(
            event,
            SessionEvent::Event { .. } | SessionEvent::RuntimeEvent { .. }
        ) {
            publication.update(runtime, *deferred_rejections)?;
        }
        return Ok(event);
    }
}

fn require_snapshot_reconnect(runtime: &mut StudioRuntime) -> Result<ReconnectWait, WorkerFailure> {
    let client = runtime.session().client();
    let server = client
        .session()
        .map(|session| session.server.clone())
        .ok_or_else(|| {
            WorkerFailure::Fatal("Cannot request snapshot without a session".to_owned())
        })?;
    let revision = client
        .last_applied_cursor()
        .map_or(0, |cursor| cursor.revision);
    // Reuse the client's snapshot-only gap transition so unresolved command
    // records survive the extra reconnect needed to refresh runtime state.
    let gap = DurableGap {
        server,
        requested_after_revision: revision.saturating_sub(2),
        available_from_revision: revision,
        current_revision: revision,
    };
    match runtime
        .session_mut()
        .client_mut()
        .intake(WireMessage::DurableGap(gap))
    {
        Err(ClientError::ResyncRequired { .. }) => {}
        Err(error) => {
            return Err(WorkerFailure::Fatal(format!(
                "Could not request authoritative snapshot: {error}"
            )));
        }
        Ok(intake) => {
            return Err(WorkerFailure::Fatal(format!(
                "Snapshot request unexpectedly succeeded: {intake:?}"
            )));
        }
    }
    runtime.session_mut().disconnect().ok_or_else(|| {
        WorkerFailure::Fatal("Cannot reconnect for snapshot without a transport".to_owned())
    })?;
    ReconnectWait::from_runtime(runtime)
        .ok_or_else(|| WorkerFailure::Fatal("Snapshot reconnect did not enter backoff".to_owned()))
}

fn worker_error(context: &str, error: &crate::StudioError) -> WorkerFailure {
    let message = format!("{context}: {error}");
    if is_resync_failure(error) {
        WorkerFailure::Resync(message)
    } else if is_transport_failure(error) {
        WorkerFailure::Transport(message)
    } else {
        WorkerFailure::Fatal(message)
    }
}

const fn is_transport_failure(error: &crate::StudioError) -> bool {
    matches!(
        error,
        crate::StudioError::Session(
            TcpSessionError::Disconnected { .. }
                | TcpSessionError::Cancelled { .. }
                | TcpSessionError::Client(ClientError::InvalidHeartbeatAcknowledgement(_))
        )
    )
}

const fn is_resync_failure(error: &crate::StudioError) -> bool {
    matches!(
        error,
        crate::StudioError::Client(ClientError::ResyncRequired { .. })
            | crate::StudioError::Session(
                TcpSessionError::ResyncRequired(_)
                    | TcpSessionError::Client(ClientError::ResyncRequired { .. })
            )
    )
}

const fn is_recoverable_failure(error: &crate::StudioError) -> bool {
    is_transport_failure(error) || is_resync_failure(error)
}

fn unexpected_failure(expected: &str, event: &SessionEvent) -> WorkerFailure {
    let message = match event {
        SessionEvent::ServerError(error) => format!(
            "Server error while awaiting {expected}: {}: {}",
            error.error.code, error.error.message
        ),
        SessionEvent::DurableGap { .. } => {
            return WorkerFailure::Resync(format!(
                "Durable event gap while awaiting {expected}; requesting snapshot"
            ));
        }
        SessionEvent::Disconnected { cause, .. } => {
            return WorkerFailure::Transport(format!(
                "Disconnected while awaiting {expected}: {cause:?}"
            ));
        }
        _ => format!("Unexpected response while awaiting {expected}: {event:?}"),
    };
    WorkerFailure::Fatal(message)
}

fn result_id(result: &CommandResult) -> &str {
    match result {
        CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
    }
}

#[allow(clippy::too_many_lines)]
fn intent_payload(
    intent: StudioIntent,
    view: Option<&ClientView>,
) -> Result<CommandPayload, String> {
    let payload = match intent {
        StudioIntent::RenameInput { input, name } => {
            let view = view.ok_or_else(|| "Project state is not synchronized".to_owned())?;
            if !view.inputs.contains(&input) {
                return Err("Input no longer exists".to_owned());
            }
            CommandPayload::RenameInput {
                input: WireInputId::from_domain(input),
                name,
            }
        }
        StudioIntent::MoveInput { input, direction } => {
            return resolve_input_move(view, input, direction);
        }
        StudioIntent::SetInputAudioStrip { input, update } => {
            return resolve_input_audio_strip(view, input, update);
        }
        StudioIntent::SelectPreview(input) => CommandPayload::SelectPreview {
            input: WireInputId::from_domain(input),
        },
        StudioIntent::Cut => CommandPayload::Cut,
        StudioIntent::Fade { duration_frames } => CommandPayload::Fade { duration_frames },
        StudioIntent::AlphaFade { duration_frames } => {
            CommandPayload::AlphaFade { duration_frames }
        }
        StudioIntent::Slide { duration_frames } => CommandPayload::Slide { duration_frames },
        StudioIntent::Zoom { duration_frames } => CommandPayload::Zoom { duration_frames },
        StudioIntent::Stinger {
            slot,
            duration_frames,
        } => CommandPayload::Stinger {
            slot,
            duration_frames,
        },
        StudioIntent::TakeOverlay { channel, source } => CommandPayload::TakeOverlay {
            channel,
            source: WireInputId::from_domain(source),
        },
        StudioIntent::UpdateOverlay { channel, source } => CommandPayload::UpdateOverlay {
            channel,
            source: WireInputId::from_domain(source),
        },
        StudioIntent::OverlayOff { channel } => CommandPayload::OverlayOff { channel },
        StudioIntent::ToggleOverlayTransition {
            channel,
            duration_frames,
        } => {
            let overlay = desired_overlay(view, channel)?;
            CommandPayload::ConfigureOverlayTransition {
                channel,
                transition: match overlay.transition {
                    OverlayTransitionKind::Cut => OverlayTransitionKind::Fade,
                    OverlayTransitionKind::Fade => OverlayTransitionKind::Cut,
                },
                duration_frames,
            }
        }
        StudioIntent::CycleOverlayPosition { channel } => {
            let overlay = desired_overlay(view, channel)?;
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: match overlay.position {
                    OverlayPositionPreset::FullFrame => OverlayPositionPreset::TopLeft,
                    OverlayPositionPreset::TopLeft => OverlayPositionPreset::TopRight,
                    OverlayPositionPreset::TopRight => OverlayPositionPreset::BottomLeft,
                    OverlayPositionPreset::BottomLeft => OverlayPositionPreset::BottomRight,
                    OverlayPositionPreset::BottomRight => OverlayPositionPreset::FullFrame,
                },
                border: overlay.border,
            }
        }
        StudioIntent::CycleOverlayBorder { channel } => {
            let overlay = desired_overlay(view, channel)?;
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: overlay.position,
                border: match overlay.border {
                    OverlayBorderPreset::None => OverlayBorderPreset::ThinWhite,
                    OverlayBorderPreset::ThinWhite => OverlayBorderPreset::ThickWhite,
                    OverlayBorderPreset::ThickWhite => OverlayBorderPreset::None,
                },
            }
        }
        StudioIntent::QueueOverlay { channel, source } => CommandPayload::QueueOverlay {
            channel,
            source: WireInputId::from_domain(source),
        },
        StudioIntent::TakeNextOverlay { channel } => CommandPayload::TakeNextOverlay { channel },
        StudioIntent::ToggleOverlayOutput { channel, output } => {
            let included = desired_overlay_output_included(view, channel, output)?;
            CommandPayload::SetOverlayOutputInclusion {
                channel,
                output: WireOutputId::from_domain(output),
                included: !included,
            }
        }
        StudioIntent::Wipe { duration_frames } => CommandPayload::Wipe { duration_frames },
        StudioIntent::FadeToBlack {
            active,
            duration_frames,
        } => CommandPayload::FadeToBlack {
            active,
            duration_frames,
        },
        StudioIntent::ToggleFadeToBlack { duration_frames } => {
            let view = view.ok_or_else(|| "Project state is not synchronized".to_owned())?;
            CommandPayload::FadeToBlack {
                active: !view.switcher.desired_fade_to_black.target_active,
                duration_frames,
            }
        }
        StudioIntent::Recording(action) => match action {
            RecordingAction::Start(recorder) => CommandPayload::RecordStart { recorder },
            RecordingAction::Stop(recorder) => CommandPayload::RecordStop { recorder },
        },
        StudioIntent::Stream(action) => return resolve_stream_action(view, action),
        StudioIntent::AssignStingerMedia {
            slot,
            media_input,
            default_cut_point_frames,
        } => return resolve_stinger_assignment(view, slot, media_input, default_cut_point_frames),
        StudioIntent::RemoveStinger { slot } => {
            let view = view.ok_or_else(|| {
                "Cannot clear a Stinger slot before project state is synchronized".to_owned()
            })?;
            if !view
                .stingers
                .iter()
                .any(|status| status.slot == slot.number())
            {
                return Err(format!("Stinger slot {slot} is not configured"));
            }
            CommandPayload::RemoveStinger { slot }
        }
        StudioIntent::StartManualTransition { kind } => {
            CommandPayload::StartManualTransition { kind }
        }
        StudioIntent::SetManualTransitionPosition { position } => {
            CommandPayload::SetManualTransitionPosition { position }
        }
        StudioIntent::CommitManualTransition => CommandPayload::CommitManualTransition,
        StudioIntent::CancelManualTransition => CommandPayload::CancelManualTransition,
        // History commands name the edit the operator saw, so a concurrent
        // editor's later edit is refused rather than silently reverted.
        StudioIntent::Undo { expected_edit } => CommandPayload::Undo {
            expected_edit: Some(expected_edit),
        },
        StudioIntent::Redo { expected_edit } => CommandPayload::Redo {
            expected_edit: Some(expected_edit),
        },
        StudioIntent::SetProjectLock { locked } => CommandPayload::SetEditLock {
            scope: EditLockScope::Project,
            locked,
        },
        StudioIntent::AddInput {
            input,
            name,
            source,
        } => {
            let view = view.ok_or_else(|| "Project state is not synchronized".to_owned())?;
            if view.inputs.contains(&input) {
                return Err(format!(
                    "Input {input} already exists; open Add input again for a fresh id"
                ));
            }
            CommandPayload::AddInput {
                input: WireInputId::from_domain(input),
                name,
                source,
            }
        }
        StudioIntent::Title(command) => title_payload(&command)
            .map_err(|error| format!("Cannot send title command: {error}"))?,
        StudioIntent::RemoveInput { input } => {
            let view = view.ok_or_else(|| "Project state is not synchronized".to_owned())?;
            if !view.inputs.contains(&input) {
                return Err("Input no longer exists".to_owned());
            }
            CommandPayload::RemoveInput {
                input: WireInputId::from_domain(input),
            }
        }
    };
    Ok(payload)
}

/// The live input add or remove a command payload carries, if any.
fn catalogue_request(payload: &CommandPayload) -> Option<CatalogueRequest> {
    match payload {
        CommandPayload::AddInput { input, name, .. } => Some(CatalogueRequest::Add {
            input: input.to_domain(),
            name: name.clone(),
        }),
        CommandPayload::RemoveInput { input } => Some(CatalogueRequest::Remove {
            input: input.to_domain(),
        }),
        _ => None,
    }
}

/// The refusal of a live input add or remove, with its stable reason code,
/// for the console's refusal toast.
fn catalogue_refusal(
    client: &Client,
    command_id: &str,
    result: &CommandResult,
) -> Option<CatalogueRefusal> {
    let CommandResult::Rejected {
        code,
        message,
        fields,
        ..
    } = result
    else {
        return None;
    };
    let request = catalogue_request(&client.command(command_id)?.command.payload)?;
    let issue = fields.first();
    Some(CatalogueRefusal {
        command_id: command_id.to_owned(),
        request,
        code: code.clone(),
        reason: issue.map(|issue| issue.code.clone()),
        message: issue.map_or_else(|| message.clone(), |issue| issue.message.clone()),
    })
}

/// Unresolved live input adds and removes, in send order. Like the recorder
/// flags, a request clears exactly when its command resolves.
fn catalogue_pending_requests(client: &Client) -> Vec<CatalogueRequest> {
    client
        .model()
        .pending_commands()
        .iter()
        .filter_map(|command| client.command(command.id.as_str()))
        .filter_map(|record| catalogue_request(&record.command.payload))
        .collect()
}

const fn lifecycle_status(lifecycle: LifecycleState) -> StudioConnectionStatus {
    match lifecycle {
        LifecycleState::LaunchingDaemon => StudioConnectionStatus::Launching,
        LifecycleState::Disconnected => StudioConnectionStatus::Disconnected,
        LifecycleState::Connecting => StudioConnectionStatus::Connecting,
        LifecycleState::Synchronizing => StudioConnectionStatus::Synchronizing,
        LifecycleState::Ready => StudioConnectionStatus::Ready,
        LifecycleState::Backoff(_) => StudioConnectionStatus::Backoff,
        LifecycleState::ProtocolMismatch => StudioConnectionStatus::ProtocolMismatch,
        LifecycleState::DaemonExited { .. }
        | LifecycleState::DaemonFailed
        | LifecycleState::RestartLimitReached
        | LifecycleState::ResyncRequired => StudioConnectionStatus::Failed,
    }
}

/// Reconnect attempt facts for the operator while the client is in backoff.
const fn lifecycle_reconnect(lifecycle: LifecycleState) -> Option<ReconnectStatus> {
    match lifecycle {
        LifecycleState::Backoff(backoff) => Some(ReconnectStatus {
            attempt: backoff.attempt,
            delay_ms: backoff.delay_ms,
        }),
        _ => None,
    }
}

fn runtime_state(runtime: &mut StudioRuntime, error: Option<String>) -> StudioUiState {
    let (connection_status, reconnect, lifecycle_error) = match runtime.lifecycle() {
        Ok(lifecycle) => (
            lifecycle_status(lifecycle),
            lifecycle_reconnect(lifecycle),
            None,
        ),
        Err(error) => (
            StudioConnectionStatus::Failed,
            None,
            Some(format!("Lifecycle check failed: {error}")),
        ),
    };
    let client = runtime.session().client();
    let permissions = client
        .session()
        .map(|session| session.permissions.as_slice());
    let (can_select_preview, can_transition) = switcher_permissions(permissions);
    let can_control_audio =
        permissions.is_some_and(|values| values.iter().any(|value| value == "control_audio"));
    let can_edit_project =
        permissions.is_some_and(|values| values.iter().any(|value| value == "edit_project"));
    let mut state = StudioUiState::new(connection_status)
        .with_switcher_permissions(can_select_preview, can_transition)
        .with_audio_permission(can_control_audio)
        .with_edit_project_permission(can_edit_project);
    state.reconnect = reconnect;
    let ready = connection_status == StudioConnectionStatus::Ready;
    state.title_access = TitleAccess::from_permissions(ready, permissions.unwrap_or_default());
    if ready {
        state.project_state = client.model().state().cloned();
        let view = client.model().view();
        state.streams = view
            .as_ref()
            .map_or_else(Vec::new, |render| render.streams.clone());
        if let Some(view) = &view {
            state.audio_meters = runtime.session().latest_audio_meters().cloned();
            state.stream_status = runtime.session().latest_stream_status().cloned();
            state.recording_desired = recording_desired_slots(view);
        }
        state.view = view;
        for tap in MonitorTap::ALL {
            state.monitor_images[tap.index()] = runtime
                .session()
                .latest_monitor_frame(tap)
                .map(|(image, received_at)| (image.clone(), received_at));
        }
    }
    state.pending_commands = client.model().pending_commands().len();
    state.recording_pending = recording_command_pending(client);
    state.stream_pending = stream_command_pending_targets(client);
    state.catalogue_pending = catalogue_pending_requests(client);
    state.error = error.or(lifecycle_error);
    state.title_updates = runtime.take_title_updates(ready);
    state
}

/// Projects the replicated two-slot recording roster into the fixed per-slot
/// desired flags the UI renders, in slot order.
fn recording_desired_slots(view: &ClientView) -> [bool; 2] {
    std::array::from_fn(|index| {
        view.recording_desired(
            WireRecorderSlot::new(u8::try_from(index + 1).expect("two recorder slots fit in u8"))
                .expect("recorder slots are 1 or 2"),
        )
    })
}

/// Per-slot pending flags for unresolved `record_start`/`record_stop`
/// commands. The replicated model keeps an accepted command pending until its
/// durable revision is applied and removes a rejected one at its result, so a
/// slot's flag clears exactly when that slot's command resolves.
fn recording_command_pending(client: &Client) -> [bool; 2] {
    let mut pending = [false; 2];
    for command in client.model().pending_commands() {
        let Some(record) = client.command(command.id.as_str()) else {
            continue;
        };
        let recorder = match &record.command.payload {
            CommandPayload::RecordStart { recorder } | CommandPayload::RecordStop { recorder } => {
                *recorder
            }
            _ => continue,
        };
        let slot = usize::from(recorder.get()) - 1;
        if let Some(flag) = pending.get_mut(slot) {
            *flag = true;
        }
    }
    pending
}

/// Stream targets with an unresolved `stream_start`/`stream_stop` command, in
/// first-pending order without duplicates. Like the recorder flags, a target
/// clears exactly when its command resolves.
fn stream_command_pending_targets(client: &Client) -> Vec<WireStreamTargetId> {
    let mut pending = Vec::new();
    for command in client.model().pending_commands() {
        let Some(record) = client.command(command.id.as_str()) else {
            continue;
        };
        let target = match &record.command.payload {
            CommandPayload::StreamStart { target } | CommandPayload::StreamStop { target } => {
                *target
            }
            _ => continue,
        };
        if !pending.contains(&target) {
            pending.push(target);
        }
    }
    pending
}

fn switcher_permissions(permissions: Option<&[String]>) -> (bool, bool) {
    (
        permissions.is_some_and(|values| values.iter().any(|value| value == "select_preview")),
        permissions.is_some_and(|values| values.iter().any(|value| value == "transition")),
    )
}

fn publish_runtime(
    runtime: &mut StudioRuntime,
    publisher: &StatePublisher,
    error: Option<String>,
) -> bool {
    publisher.publish(runtime_state(runtime, error))
}

fn publish_runtime_with_uncertainties(
    runtime: &mut StudioRuntime,
    publisher: &StatePublisher,
    error: Option<String>,
    terminal_uncertainties: &[TerminalUncertaintyNotice],
) -> bool {
    let mut state = runtime_state(runtime, error);
    state.terminal_uncertainties = terminal_uncertainties.to_vec();
    publisher.publish(state)
}

fn publish_deferred_runtime(
    runtime: &mut StudioRuntime,
    publisher: &StatePublisher,
    deferred: &VecDeque<StudioIntent>,
    error: Option<String>,
    terminal_uncertainties: &[TerminalUncertaintyNotice],
) -> bool {
    let mut state = runtime_state(runtime, error);
    state.terminal_uncertainties = terminal_uncertainties.to_vec();
    state.notice = Some(format!(
        "Queued {} command(s) in operator FIFO",
        deferred.len()
    ));
    publisher.publish(state)
}

fn publish_recovery_runtime(
    runtime: &mut StudioRuntime,
    publisher: &StatePublisher,
    recovery: &WorkerRecovery,
) -> bool {
    // Between commands the worker pursues only `pending_command`; a title
    // command it gave up on will never be answered.
    runtime.settle_titles(recovery.pending_command.as_deref());
    if recovery.deferred_intents.is_empty() {
        publish_runtime_with_uncertainties(
            runtime,
            publisher,
            recovery.error(),
            &recovery.terminal_uncertainties,
        )
    } else {
        publish_deferred_runtime(
            runtime,
            publisher,
            &recovery.deferred_intents,
            recovery.error(),
            &recovery.terminal_uncertainties,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream, UdpSocket},
        sync::mpsc::sync_channel,
        thread::sleep,
    };

    use core::num::NonZeroU128;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::{fs, path::PathBuf, process::Command};

    use fm_protocol::{
        CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, EngineIdentity, EventCursor,
        EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState, HandshakeOutcome,
        HandshakeResponse, HeartbeatAcknowledgementMessage, InputAudioStripStatus, InputStatus,
        ManualTransitionPosition, ManualTransitionStatus, OutputStatus, OverlayBorderPreset,
        OverlayPositionPreset, OverlayStatus, Role, RuntimeEventMessage, RuntimeLifecycleEvent,
        ServerIdentity, SnapshotMessage, SnapshotReason, StreamRealizedState, StreamStatusMessage,
        StreamStatusSample, WireMessage, WireOutputId, WireStreamTargetId, decode_line,
        encode_line,
    };
    use fm_types::ProjectId;
    use fm_ui_model::{ClientModel, DurableChange, DurableProjectEvent, ProjectSnapshot};

    #[cfg(unix)]
    use crate::SupervisedConfig;
    use crate::{ConnectionConfig, ExistingConfig, RestartPolicy};

    use super::*;

    /// Failure bound for test-side waits; it must comfortably exceed
    /// `PEER_WAIT_TIMEOUT` so a slow but successful worker wait is not
    /// misreported by the test's own deadline.
    const HEARTBEAT_TEST_TIMEOUT: Duration = Duration::from_secs(10);

    struct HeartbeatPeer {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    }

    impl HeartbeatPeer {
        fn accept(listener: &TcpListener) -> Self {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(HEARTBEAT_TEST_TIMEOUT))
                .unwrap();
            Self {
                reader: BufReader::new(stream.try_clone().unwrap()),
                writer: stream,
            }
        }

        fn receive(&mut self) -> WireMessage {
            let mut line = String::new();
            assert_ne!(self.reader.read_line(&mut line).unwrap(), 0);
            decode_line(&line).unwrap()
        }

        fn handshake_request(&mut self) -> fm_protocol::HandshakeRequest {
            let WireMessage::HandshakeRequest(request) = self.receive() else {
                panic!("expected handshake request");
            };
            request
        }

        fn send(&mut self, message: &WireMessage) {
            self.writer
                .write_all(encode_line(message).unwrap().as_bytes())
                .unwrap();
        }

        /// Reads the next line, or `None` once the worker closed the session.
        /// A worker that shuts down with an unread heartbeat answer in its
        /// receive buffer resets the connection instead of a clean EOF, so
        /// a reset ends the scripted peer just like EOF.
        fn next_line(&mut self) -> Option<String> {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(line),
                Err(error) if peer_closed(&error) => None,
                Err(error) => panic!("peer read failed: {error}"),
            }
        }

        /// Sends a record that may race the worker's shutdown; returns
        /// `false` once the worker closed the session.
        fn send_unless_closed(&mut self, message: &WireMessage) -> bool {
            match self
                .writer
                .write_all(encode_line(message).unwrap().as_bytes())
            {
                Ok(()) => true,
                Err(error) if peer_closed(&error) => false,
                Err(error) => panic!("peer write failed: {error}"),
            }
        }

        fn acknowledge_heartbeats_until_eof(&mut self) {
            loop {
                let Some(line) = self.next_line() else {
                    return;
                };
                let WireMessage::Heartbeat(heartbeat) = decode_line(&line).unwrap() else {
                    panic!("expected heartbeat");
                };
                if !self.send_unless_closed(&WireMessage::AudioMeters(AudioMetersMessage {
                    server: heartbeat.server.clone(),
                    sequence: heartbeat.sequence,
                    frame: 1,
                    start_sample: 0,
                    end_sample: 1,
                    master: vec![AudioMeterChannel {
                        peak_millionths: 500_000,
                        rms_millionths: 250_000,
                    }],
                    inputs: Vec::new(),
                })) {
                    return;
                }
                if !self.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                    HeartbeatAcknowledgementMessage {
                        server: heartbeat.server,
                        heartbeat_sequence: heartbeat.sequence,
                        received_at_ms: heartbeat.sent_at_ms,
                    },
                )) {
                    return;
                }
            }
        }

        /// Answers every heartbeat with one stream-status record ahead of its
        /// acknowledgement, so the worker's acknowledgement wait must swallow
        /// the lossy record before matching the pending heartbeat.
        fn acknowledge_heartbeats_with_stream_status_until_eof(&mut self) {
            let mut sequence = 0;
            loop {
                let Some(line) = self.next_line() else {
                    return;
                };
                let WireMessage::Heartbeat(heartbeat) = decode_line(&line).unwrap() else {
                    panic!("expected heartbeat");
                };
                sequence += 1;
                if !self
                    .send_unless_closed(&WireMessage::StreamStatus(stream_status_message(sequence)))
                {
                    return;
                }
                if !self.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                    HeartbeatAcknowledgementMessage {
                        server: heartbeat.server,
                        heartbeat_sequence: heartbeat.sequence,
                        received_at_ms: heartbeat.sent_at_ms,
                    },
                )) {
                    return;
                }
            }
        }

        /// Answers every heartbeat with one complete multi-part Program
        /// monitor frame ahead of its acknowledgement.
        fn acknowledge_heartbeats_with_monitor_frames_until_eof(&mut self) {
            let mut sequence = 0;
            loop {
                let Some(line) = self.next_line() else {
                    return;
                };
                let WireMessage::Heartbeat(heartbeat) = decode_line(&line).unwrap() else {
                    panic!("expected heartbeat");
                };
                sequence += 1;
                for record in monitor_frame_records(MonitorTap::Program, sequence) {
                    if !self.send_unless_closed(&record) {
                        return;
                    }
                }
                if !self.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                    HeartbeatAcknowledgementMessage {
                        server: heartbeat.server,
                        heartbeat_sequence: heartbeat.sequence,
                        received_at_ms: heartbeat.sent_at_ms,
                    },
                )) {
                    return;
                }
            }
        }
    }

    fn peer_closed(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        )
    }

    fn heartbeat_handshake(outcome: HandshakeOutcome) -> HandshakeResponse {
        HandshakeResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            granted_role: Role::Operator,
            permissions: Vec::new(),
            capabilities: CapabilityReportSummary {
                digest: String::new(),
                total: 0,
                available: 0,
                degraded: 0,
                unavailable: 0,
            },
            server: ServerIdentity {
                engine_id: "e".into(),
                project_id: "1".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            current_revision: 4,
            outcome,
        }
    }

    fn heartbeat_snapshot() -> SnapshotMessage {
        let input = WireInputId::new(NonZeroU128::new(2).unwrap());
        SnapshotMessage {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
            show_name: String::new(),
            inputs: vec![InputStatus {
                input,
                name: "i".into(),
            }],
            outputs: Vec::new(),
            input_audio_strips: vec![InputAudioStripStatus {
                input,
                gain_millidb: 0,
                balance_basis_points: 0,
                muted: false,
                soloed: false,
                follow_video: false,
                delay_samples: 0,
            }],
            desired_program: input,
            desired_preview: input,
            realized_program: input,
            realized_preview: input,
            desired_manual_transition: ManualTransitionStatus::Inactive,
            realized_manual_transition: ManualTransitionStatus::Inactive,
            desired_fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            stingers: Vec::new(),
            streams: vec![fm_protocol::StreamStatus {
                target: WireStreamTargetId::new(NonZeroU128::new(40).unwrap()),
                name: "rtmp-primary".into(),
                desired_running: true,
                realized: StreamRealizedState::Starting,
                detail: None,
            }],
            recordings: recording_roster([false, false]),
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: Vec::new(),
        }
    }

    /// The fixed two-slot recorder roster for test snapshots, in slot order.
    fn recording_roster(desired: [bool; 2]) -> Vec<fm_protocol::RecordingStatus> {
        desired
            .into_iter()
            .enumerate()
            .map(|(index, desired_active)| fm_protocol::RecordingStatus {
                recorder: wire_recorder(
                    u8::try_from(index + 1).expect("two recorder slots fit in u8"),
                ),
                desired_active,
            })
            .collect()
    }

    /// The wire recorder slot for `number`, which tests only call with 1 or 2.
    fn wire_recorder(number: u8) -> WireRecorderSlot {
        WireRecorderSlot::new(number).expect("test recorder slots are 1 or 2")
    }

    fn stream_status_message(sequence: u64) -> StreamStatusMessage {
        StreamStatusMessage {
            server: ServerIdentity {
                engine_id: "e".into(),
                project_id: "1".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            sequence,
            samples: vec![StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(40).unwrap()),
                realized: StreamRealizedState::Live,
                connected: true,
                muxed_bytes: 2048,
                enqueued_pairs: 12,
                dropped_pairs: 3,
                failure: None,
                local: None,
            }],
        }
    }

    fn command_event(revision: u64) -> EventMessage {
        let snapshot = heartbeat_snapshot();
        let input = snapshot.inputs[0].input;
        EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "e".into(),
                    state_epoch: 1,
                    log_id: "l".into(),
                },
                revision,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input,
                preview: input,
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: snapshot.input_audio_strips,
            },
        }
    }

    fn command_runtime_event(revision: u64) -> RuntimeEventMessage {
        RuntimeEventMessage {
            server: ServerIdentity {
                engine_id: "e".into(),
                project_id: "1".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".to_owned(),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
            },
        }
    }

    fn recording_event(revision: u64, recorder: u8, active: bool) -> EventMessage {
        EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "e".into(),
                    state_epoch: 1,
                    log_id: "l".into(),
                },
                revision,
            },
            payload: EventPayload::RecordingChanged {
                recorder: wire_recorder(recorder),
                active,
            },
        }
    }

    fn recording_runtime_event(revision: u64, sequence: u64) -> RuntimeEventMessage {
        RuntimeEventMessage {
            sequence,
            ..command_runtime_event(revision)
        }
    }

    #[cfg(unix)]
    #[allow(clippy::needless_pass_by_value)] // ownership moves into the spawned server thread
    fn serve_worker_recovery(listener: TcpListener) {
        let mut first = HeartbeatPeer::accept(&listener);
        first.handshake_request();
        first.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        )));
        first.send(&WireMessage::Snapshot(heartbeat_snapshot()));
        first.acknowledge_heartbeats_until_eof();

        let mut resume = HeartbeatPeer::accept(&listener);
        let cursor = resume
            .handshake_request()
            .resume_cursor
            .expect("resume cursor");
        resume.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
            HandshakeOutcome::Resume { cursor },
        )));
        resume.acknowledge_heartbeats_until_eof();

        let mut snapshot = HeartbeatPeer::accept(&listener);
        assert_eq!(snapshot.handshake_request().resume_cursor, None);
        snapshot.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::HistoryUnavailable,
            },
        )));
        snapshot.send(&WireMessage::Snapshot(heartbeat_snapshot()));
        snapshot.acknowledge_heartbeats_until_eof();
    }

    #[cfg(unix)]
    struct TestDirectory(PathBuf);

    #[cfg(unix)]
    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "freemix-studio-idle-exit-{}-{}",
                std::process::id(),
                worker_nonce()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    #[cfg(unix)]
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn idle_exit_helper(directory: &TestDirectory, address: std::net::SocketAddr) -> PathBuf {
        let path = directory.path("freemixd-test-helper");
        // Stage the text in a never-executed file and let a `cp` child create
        // the executable inode, so no write descriptor to it can leak into a
        // concurrent test thread's fork and make our exec fail with ETXTBSY.
        let staged = directory.path("freemixd-test-helper.source");
        fs::write(
            &staged,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$2.pid\"\nprintf '%s\\n' \"$$\" >> \"$2.launches\"\nprintf 'FREEMIXD_READY\\tv=1\\taddress={address}\\tproject_id=1\\n'\nIFS= read -r hold\n"
            ),
        )
        .unwrap();
        let status = std::process::Command::new("/bin/cp")
            .arg(&staged)
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "failed to install test helper script");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn receive_connection_state(updates: &StateUpdates, expected: StudioConnectionStatus) {
        receive_state_where(updates, |state| state.connection_status == expected);
    }

    #[cfg(unix)]
    fn receive_meter_state(updates: &StateUpdates) {
        let state = receive_state_where(updates, |state| state.audio_meters.is_some());
        assert_eq!(state.connection_status, StudioConnectionStatus::Ready);
    }

    /// Mirrors [`receive_meter_state`] for the lossy stream-status roster:
    /// returns the first publication that hydrated a latest status sample.
    fn receive_stream_status_state(updates: &StateUpdates) -> StudioUiState {
        loop {
            let state = receive_next_state(updates);
            if state.stream_status.is_some() {
                assert_eq!(state.connection_status, StudioConnectionStatus::Ready);
                assert!(
                    state.error.is_none(),
                    "stream status publication carried an error: {:?}",
                    state.error
                );
                return state;
            }
        }
    }

    #[test]
    fn state_mailbox_keeps_latest_state_when_notification_is_full() {
        let (publisher, updates) = state_mailbox(egui::Context::default());
        assert!(publisher.publish(StudioUiState::new(StudioConnectionStatus::Launching)));
        assert!(publisher.publish(StudioUiState::new(StudioConnectionStatus::Ready)));
        assert_eq!(
            updates.try_recv().unwrap().connection_status,
            StudioConnectionStatus::Ready
        );
        drop(updates);
        assert!(!publisher.publish(StudioUiState::new(StudioConnectionStatus::Failed)));
    }

    #[test]
    fn finished_worker_keeps_its_last_published_failure() {
        // The worker publishes its startup failure and exits between the
        // frame's mailbox read and the finish check: the specific error must
        // survive instead of becoming "stopped unexpectedly".
        let (publisher, updates) = state_mailbox(egui::Context::default());
        let mut failed = StudioUiState::new(StudioConnectionStatus::Failed);
        failed.error = Some("Studio startup failed: freemixd not found".to_owned());
        assert!(publisher.publish(failed.clone()));
        let mut state = StudioUiState::new(StudioConnectionStatus::Launching);
        let mut updates = Some(updates);
        reconcile_finished_worker(&mut state, &mut updates, false);
        assert!(updates.is_none());
        assert_eq!(state, failed);

        // Without a final publication the exit is still reported.
        let (_publisher, updates) = state_mailbox(egui::Context::default());
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        reconcile_finished_worker(&mut state, &mut Some(updates), false);
        assert_eq!(state.connection_status, StudioConnectionStatus::Failed);
        assert_eq!(
            state.error.as_deref(),
            Some("Studio worker stopped unexpectedly")
        );
        let mut state = failed.clone();
        reconcile_finished_worker(&mut state, &mut None, true);
        assert_eq!(state.error.as_deref(), Some("Studio worker panicked"));
    }

    #[test]
    fn backoff_lifecycle_exposes_reconnect_attempt_to_the_ui() {
        let backoff = LifecycleState::Backoff(fm_client::ReconnectBackoff {
            attempt: 4,
            delay_ms: 1_600,
        });
        assert_eq!(lifecycle_status(backoff), StudioConnectionStatus::Backoff);
        assert_eq!(
            lifecycle_reconnect(backoff),
            Some(ReconnectStatus {
                attempt: 4,
                delay_ms: 1_600,
            })
        );
        assert_eq!(lifecycle_reconnect(LifecycleState::Ready), None);
        assert_eq!(lifecycle_reconnect(LifecycleState::Connecting), None);
    }

    #[test]
    fn worker_panic_is_reported_without_blocking_live_ui() {
        let worker = thread::spawn(|| panic!("test worker panic"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        let mut app = StudioApp {
            shell: StudioShell::default(),
            state: StudioUiState::new(StudioConnectionStatus::Ready),
            requests: None,
            pending_intents: PendingIntents::new(),
            updates: None,
            worker: Some(worker),
            shutdown_sent: false,
            osc: None,
            osc_rejected: 0,
            osc_notice: None,
            preferences: PreferenceStore::new(None, fm_ui_model::i18n::Locale::ENGLISH),
            preferences_notice: None,
        };

        app.check_worker_health();

        assert!(app.worker.is_none());
        assert_eq!(app.state.connection_status, StudioConnectionStatus::Failed);
        assert_eq!(app.state.error.as_deref(), Some("Studio worker panicked"));
    }

    #[test]
    fn recovery_publication_preserves_terminal_uncertainties_and_deferred_fifo() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            peer.acknowledge_heartbeats_until_eof();
        });
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let mut runtime = StudioRuntime::new(StudioConfig {
            connection: ConnectionConfig::Existing(ExistingConfig {
                address,
                expected_project_id: project,
            }),
            client_id: "terminal-uncertainty-publication-test".to_owned(),
            desired_role: Role::Operator,
            restart_policy: RestartPolicy {
                maximum_restarts: 0,
            },
            osc_listen: None,
            transport: crate::ControlTransport::Tcp,
            ui: crate::UiPreferences::default(),
        })
        .unwrap();
        runtime.connect(HEARTBEAT_TEST_TIMEOUT).unwrap();

        let first = TerminalUncertaintyNotice {
            command_id: "local-1".to_owned(),
            received_command_id: "server-1".to_owned(),
        };
        let second = TerminalUncertaintyNotice {
            command_id: "local-2".to_owned(),
            received_command_id: "server-2".to_owned(),
        };
        let mut recovery = WorkerRecovery::new(Some("reconnect pending".to_owned()), None);
        recovery.terminal_uncertainties = vec![first.clone(), second.clone()];
        recovery.deferred_intents.push_back(StudioIntent::Cut);
        let (publisher, updates) = state_mailbox(egui::Context::default());

        assert!(publish_recovery_runtime(
            &mut runtime,
            &publisher,
            &recovery
        ));
        updates
            .receiver
            .recv_timeout(HEARTBEAT_TEST_TIMEOUT)
            .unwrap();
        let state = updates.latest.lock().unwrap().take().unwrap();
        assert_eq!(state.terminal_uncertainties, vec![first, second]);
        assert_eq!(
            state.notice.as_deref(),
            Some("Queued 1 command(s) in operator FIFO")
        );
        assert_eq!(
            state.error.as_deref(),
            Some(
                "reconnect pending; Terminal command uncertainty remains after authoritative resync: \"local-1\" (server receipt \"server-1\"), \"local-2\" (server receipt \"server-2\")"
            )
        );
        drop(runtime);
        server.join().unwrap();
    }

    fn manual_position(basis_points: u16) -> StudioIntent {
        StudioIntent::SetManualTransitionPosition {
            position: ManualTransitionPosition::new(basis_points).unwrap(),
        }
    }

    #[test]
    fn pending_intents_coalesce_manual_positions_after_outer_queue_full() {
        let (sender, receiver) = sync_channel(1);
        sender
            .send(WorkerRequest::Intent(StudioIntent::Cut))
            .unwrap();
        let mut pending = PendingIntents::new();
        for position in [1_000, 2_000, 3_000] {
            assert_eq!(pending.submit(&sender, manual_position(position)), Ok(()));
        }

        assert_eq!(
            receiver.try_recv().unwrap(),
            WorkerRequest::Intent(StudioIntent::Cut)
        );
        assert_eq!(pending.flush(&sender), Ok(()));
        assert_eq!(
            receiver.try_recv().unwrap(),
            WorkerRequest::Intent(manual_position(3_000))
        );
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // values bounded by REQUEST_CAPACITY
    fn pending_intents_coalesce_adjacent_audio_updates_without_crossing_boundaries() {
        let input = WireInputId::new(NonZeroU128::new(2).unwrap()).to_domain();
        let other_input = WireInputId::new(NonZeroU128::new(3).unwrap()).to_domain();
        let mut pending = PendingIntents::new();
        let initial = StudioIntent::SetInputAudioStrip {
            input,
            update: InputAudioStripUpdate {
                muted: Some(false),
                soloed: Some(false),
                follow_video: Some(false),
                ..InputAudioStripUpdate::default()
            },
        };
        assert_eq!(pending.push(initial), Ok(()));
        for value in 1..=REQUEST_CAPACITY {
            assert_eq!(
                pending.push(StudioIntent::SetInputAudioStrip {
                    input,
                    update: InputAudioStripUpdate {
                        gain_millidb: Some(value as i32),
                        balance_basis_points: Some(-(value as i32)),
                        delay_samples: Some(0),
                        ..InputAudioStripUpdate::default()
                    },
                }),
                Ok(())
            );
        }
        let coalesced = StudioIntent::SetInputAudioStrip {
            input,
            update: InputAudioStripUpdate {
                gain_millidb: Some(REQUEST_CAPACITY as i32),
                balance_basis_points: Some(-(REQUEST_CAPACITY as i32)),
                muted: Some(false),
                soloed: Some(false),
                follow_video: Some(false),
                delay_samples: Some(0),
            },
        };
        assert_eq!(pending.intents, VecDeque::from([coalesced.clone()]));

        let other = StudioIntent::SetInputAudioStrip {
            input: other_input,
            update: InputAudioStripUpdate {
                muted: Some(true),
                ..InputAudioStripUpdate::default()
            },
        };
        assert_eq!(pending.push(other.clone()), Ok(()));
        assert_eq!(pending.push(StudioIntent::Cut), Ok(()));
        assert_eq!(pending.push(other.clone()), Ok(()));
        assert_eq!(
            pending.intents,
            VecDeque::from([coalesced.clone(), other.clone(), StudioIntent::Cut, other])
        );
    }

    #[test]
    fn pending_intents_keep_manual_position_before_commit_after_retry() {
        let (sender, receiver) = sync_channel(1);
        sender
            .send(WorkerRequest::Intent(StudioIntent::Cut))
            .unwrap();
        let mut pending = PendingIntents::new();
        assert_eq!(pending.submit(&sender, manual_position(3_000)), Ok(()));
        assert_eq!(
            pending.submit(&sender, StudioIntent::CommitManualTransition),
            Ok(())
        );

        assert_eq!(
            receiver.try_recv().unwrap(),
            WorkerRequest::Intent(StudioIntent::Cut)
        );
        assert_eq!(pending.flush(&sender), Ok(()));
        assert_eq!(
            receiver.try_recv().unwrap(),
            WorkerRequest::Intent(manual_position(3_000))
        );
        assert_eq!(pending.flush(&sender), Ok(()));
        assert_eq!(
            receiver.try_recv().unwrap(),
            WorkerRequest::Intent(StudioIntent::CommitManualTransition)
        );
    }

    #[cfg(unix)]
    #[test]
    fn studio_worker_recovers_after_idle_supervised_daemon_exit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || serve_worker_recovery(listener));
        let directory = TestDirectory::new();
        let project_bundle = directory.path("show.freemix");
        let daemon_executable = idle_exit_helper(&directory, address);
        let (request_sender, request_receiver) = sync_channel(REQUEST_CAPACITY);
        let (publisher, updates) = state_mailbox(egui::Context::default());
        let worker = thread::spawn(move || {
            run_worker(
                StudioConfig {
                    connection: ConnectionConfig::Supervised(SupervisedConfig {
                        project_bundle,
                        daemon_executable,
                        listen: "127.0.0.1:0".parse().unwrap(),
                    }),
                    client_id: "idle-exit-worker-test".to_owned(),
                    desired_role: Role::Operator,
                    restart_policy: RestartPolicy {
                        maximum_restarts: 1,
                    },
                    osc_listen: None,
                    transport: crate::ControlTransport::Tcp,
                    ui: crate::UiPreferences::default(),
                },
                &request_receiver,
                &publisher,
            );
        });

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        receive_meter_state(&updates);
        let pid_path = directory.path("show.freemix.pid");
        let pid = fs::read_to_string(pid_path).unwrap();
        assert!(
            Command::new("/bin/kill")
                .args(["-TERM", pid.trim()])
                .status()
                .unwrap()
                .success()
        );
        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        assert_eq!(
            fs::read_to_string(directory.path("show.freemix.launches"))
                .unwrap()
                .lines()
                .count(),
            2
        );

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    /// Spawns the real worker loop against a scripted TCP peer.
    fn spawn_existing_worker(
        address: std::net::SocketAddr,
        client_id: &'static str,
    ) -> (
        std::sync::mpsc::SyncSender<WorkerRequest>,
        StateUpdates,
        thread::JoinHandle<()>,
    ) {
        let (request_sender, request_receiver) = sync_channel(REQUEST_CAPACITY);
        let (publisher, updates) = state_mailbox(egui::Context::default());
        let worker = thread::spawn(move || {
            run_worker(
                StudioConfig {
                    connection: ConnectionConfig::Existing(ExistingConfig {
                        address,
                        expected_project_id: ProjectId::new(NonZeroU128::new(1).unwrap()),
                    }),
                    client_id: client_id.to_owned(),
                    desired_role: Role::Operator,
                    restart_policy: RestartPolicy {
                        maximum_restarts: 0,
                    },
                    osc_listen: None,
                    transport: crate::ControlTransport::Tcp,
                    ui: crate::UiPreferences::default(),
                },
                &request_receiver,
                &publisher,
            );
        });
        (request_sender, updates, worker)
    }

    #[test]
    fn idle_worker_swallows_stream_status_records_like_meters() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            // Unsolicited lossy record while idle; it must never surface as an
            // "Unexpected idle response" failure.
            peer.send(&WireMessage::StreamStatus(stream_status_message(1)));
            peer.acknowledge_heartbeats_until_eof();
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "stream-status-idle-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        let state = receive_stream_status_state(&updates);
        assert_eq!(state.streams.len(), 1);
        assert_eq!(state.streams[0].name, "rtmp-primary");
        assert!(state.streams[0].desired_running);
        assert_eq!(
            state.stream_status.as_ref().unwrap().samples[0].muxed_bytes,
            2048
        );

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn heartbeat_ack_wait_swallows_stream_status_records() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            peer.acknowledge_heartbeats_with_stream_status_until_eof();
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "stream-status-heartbeat-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        // The record sits between each heartbeat and its acknowledgement, so a
        // failed swallow would abort the wait with an unexpected-event error.
        for _ in 0..4 {
            let state = receive_stream_status_state(&updates);
            assert_eq!(
                state.streams.len(),
                1,
                "snapshot roster must hydrate beside meters"
            );
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn command_wait_swallows_stream_status_without_returning_it_as_a_result() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            let mut command_seen = false;
            loop {
                let Some(line) = peer.next_line() else {
                    return;
                };
                match decode_line(&line).unwrap() {
                    WireMessage::Heartbeat(heartbeat) => {
                        if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                            HeartbeatAcknowledgementMessage {
                                server: heartbeat.server,
                                heartbeat_sequence: heartbeat.sequence,
                                received_at_ms: heartbeat.sent_at_ms,
                            },
                        )) {
                            return;
                        }
                    }
                    WireMessage::Command(command) => {
                        assert!(!command_seen, "worker retried the command unexpectedly");
                        command_seen = true;
                        let accepted_revision = heartbeat_snapshot().revision + 1;
                        // The lossy record arrives mid-command-wait and must be
                        // consumed as status, not mistaken for the result.
                        peer.send(&WireMessage::StreamStatus(stream_status_message(1)));
                        peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                            id: command.id,
                            revision: accepted_revision,
                            scheduled_frame: None,
                        }));
                        peer.send(&WireMessage::Event(command_event(accepted_revision)));
                        peer.send(&WireMessage::RuntimeEvent(command_runtime_event(
                            accepted_revision,
                        )));
                    }
                    other => panic!("unexpected peer message: {other:?}"),
                }
            }
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "stream-status-command-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        request_sender
            .send(WorkerRequest::Intent(StudioIntent::Cut))
            .unwrap();
        let deadline = Instant::now() + HEARTBEAT_TEST_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "command sequence did not complete"
            );
            let state = receive_next_state(&updates);
            assert_eq!(state.connection_status, StudioConnectionStatus::Ready);
            assert!(
                state.error.is_none(),
                "command publication carried an error: {:?}",
                state.error
            );
            if state.pending_commands == 0
                && let Some(status) = &state.stream_status
            {
                assert_eq!(state.streams.len(), 1);
                assert_eq!(status.samples[0].muxed_bytes, 2048);
                break;
            }
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    /// Pixels for one deterministic noise monitor frame; noise defeats QOI
    /// runs so every frame needs more than one `monitor_frame` part.
    fn monitor_frame_pixels(sequence: u64) -> (u16, u16, Vec<u8>) {
        let (width, height) = (128_u16, 72_u16);
        let mut state = 0x9E37_79B9_u32 ^ u32::try_from(sequence).unwrap();
        let pixels = (0..usize::from(width) * usize::from(height) * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        (width, height, pixels)
    }

    fn monitor_frame_records(tap: MonitorTap, sequence: u64) -> Vec<WireMessage> {
        let (width, height, pixels) = monitor_frame_pixels(sequence);
        let qoi =
            fm_protocol::encode_qoi_rgb(width, height, &pixels, usize::from(width) * 3, 3).unwrap();
        let parts = fm_protocol::monitor_frame_parts(
            &heartbeat_handshake(HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            })
            .server,
            tap,
            sequence,
            sequence,
            heartbeat_snapshot().inputs[0].input,
            fm_protocol::MonitorFrameOrigin::Simulated,
            width,
            height,
            &qoi,
        );
        assert!(parts.len() >= 2, "test frames must span several parts");
        parts.into_iter().map(WireMessage::MonitorFrame).collect()
    }

    fn assert_monitor_frame(state: &StudioUiState, tap: MonitorTap, sequence: u64) {
        let (image, _) = state.monitor_images[tap.index()]
            .as_ref()
            .expect("monitor frame published");
        assert_eq!(image.tap, tap);
        assert_eq!(image.sequence, sequence);
        assert_eq!(image.origin, fm_protocol::MonitorFrameOrigin::Simulated);
        assert_eq!(image.input, heartbeat_snapshot().inputs[0].input);
        let (width, height, pixels) = monitor_frame_pixels(sequence);
        assert_eq!((image.width, image.height), (width, height));
        let expected = pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect::<Vec<_>>();
        assert_eq!(&image.rgba[..], &expected[..]);
    }

    #[test]
    fn idle_worker_publishes_monitor_frames_to_the_ui_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            // Unsolicited lossy frames while idle; neither may surface as an
            // "Unexpected idle response" failure.
            for tap in MonitorTap::ALL {
                for record in monitor_frame_records(tap, 1) {
                    peer.send(&record);
                }
            }
            peer.acknowledge_heartbeats_until_eof();
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "monitor-frame-idle-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        let state = receive_state_where(&updates, |state| {
            state.monitor_images.iter().all(Option::is_some)
        });
        assert_eq!(state.connection_status, StudioConnectionStatus::Ready);
        assert!(state.error.is_none(), "{:?}", state.error);
        for tap in MonitorTap::ALL {
            assert_monitor_frame(&state, tap, 1);
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn heartbeat_ack_wait_swallows_monitor_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            peer.acknowledge_heartbeats_with_monitor_frames_until_eof();
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "monitor-frame-heartbeat-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        // Each frame sits between a heartbeat and its acknowledgement, so a
        // failed swallow would abort the wait with an unexpected-event error.
        for sequence in 1..=4 {
            let state = receive_state_where(&updates, |state| {
                state.monitor_images[MonitorTap::Program.index()]
                    .as_ref()
                    .is_some_and(|(image, _)| image.sequence >= sequence)
            });
            assert_eq!(state.connection_status, StudioConnectionStatus::Ready);
            assert!(state.error.is_none(), "{:?}", state.error);
            let published = state.monitor_images[MonitorTap::Program.index()]
                .as_ref()
                .unwrap()
                .0
                .sequence;
            assert_monitor_frame(&state, MonitorTap::Program, published);
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one scripted peer conversation
    fn command_wait_swallows_monitor_frames_without_counting_them_as_records() {
        // More parts than the command record limit arrive mid-wait; counting
        // them would fail the command.
        const FRAMES_PER_TAP: u64 = 3;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // Encode every frame before the worker connects: the command wait is
        // bounded by `PEER_WAIT_TIMEOUT`, so the peer must not spend that
        // window generating noise and QOI streams.
        let mid_wait_records = (1..=FRAMES_PER_TAP)
            .flat_map(|sequence| MonitorTap::ALL.map(|tap| monitor_frame_records(tap, sequence)))
            .flatten()
            .collect::<Vec<_>>();
        assert!(mid_wait_records.len() > MAX_COMMAND_RECORDS);
        let post_result_records = monitor_frame_records(MonitorTap::Program, FRAMES_PER_TAP + 1);
        let server = thread::spawn(move || {
            let mut peer = HeartbeatPeer::accept(&listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
            let mut command_seen = false;
            loop {
                let Some(line) = peer.next_line() else {
                    return;
                };
                match decode_line(&line).unwrap() {
                    WireMessage::Heartbeat(heartbeat) => {
                        if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                            HeartbeatAcknowledgementMessage {
                                server: heartbeat.server,
                                heartbeat_sequence: heartbeat.sequence,
                                received_at_ms: heartbeat.sent_at_ms,
                            },
                        )) {
                            return;
                        }
                    }
                    WireMessage::Command(command) => {
                        assert!(!command_seen, "worker retried the command unexpectedly");
                        command_seen = true;
                        let accepted_revision = heartbeat_snapshot().revision + 1;
                        for record in &mid_wait_records {
                            peer.send(record);
                        }
                        peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                            id: command.id,
                            revision: accepted_revision,
                            scheduled_frame: None,
                        }));
                        for record in &post_result_records {
                            peer.send(record);
                        }
                        peer.send(&WireMessage::Event(command_event(accepted_revision)));
                        peer.send(&WireMessage::RuntimeEvent(command_runtime_event(
                            accepted_revision,
                        )));
                    }
                    other => panic!("unexpected peer message: {other:?}"),
                }
            }
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "monitor-frame-command-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        request_sender
            .send(WorkerRequest::Intent(StudioIntent::Cut))
            .unwrap();
        let deadline = Instant::now() + HEARTBEAT_TEST_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "command sequence did not complete"
            );
            let state = receive_next_state(&updates);
            assert_eq!(
                state.connection_status,
                StudioConnectionStatus::Ready,
                "{:?}",
                state.error
            );
            assert!(
                state.error.is_none(),
                "command publication carried an error: {:?}",
                state.error
            );
            if state.pending_commands == 0
                && state.monitor_images[MonitorTap::Program.index()]
                    .as_ref()
                    .is_some_and(|(image, _)| image.sequence == FRAMES_PER_TAP + 1)
            {
                assert_monitor_frame(&state, MonitorTap::Program, FRAMES_PER_TAP + 1);
                assert_monitor_frame(&state, MonitorTap::Preview, FRAMES_PER_TAP);
                break;
            }
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    /// Receives the next publication satisfying `predicate`, tolerating a
    /// notification that was coalesced with a state the caller already took.
    fn receive_state_where(
        updates: &StateUpdates,
        predicate: impl Fn(&StudioUiState) -> bool,
    ) -> StudioUiState {
        loop {
            let state = receive_next_state(updates);
            if predicate(&state) {
                return state;
            }
        }
    }

    /// Receives the next unseen publication. A notification can arrive after
    /// the caller already took the state it announced (the mailbox coalesces
    /// to the latest state), so an empty mailbox waits for the next one.
    fn receive_next_state(updates: &StateUpdates) -> StudioUiState {
        loop {
            updates
                .receiver
                .recv_timeout(HEARTBEAT_TEST_TIMEOUT)
                .unwrap();
            if let Some(state) = updates
                .latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                return state;
            }
        }
    }

    /// Answers one scripted start/stop pair per recorder slot in order,
    /// holding each result open until the test confirmed that the dispatched
    /// intent was observed pending for exactly the addressed slot.
    fn serve_worker_recording(
        listener: &TcpListener,
        command_seen: &SyncSender<()>,
        release: &Receiver<()>,
    ) {
        let mut peer = HeartbeatPeer::accept(listener);
        peer.handshake_request();
        peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        )));
        peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
        let mut revision = heartbeat_snapshot().revision;
        for (sequence, recorder, active) in
            [(1, 1_u8, true), (2, 1, false), (3, 2, true), (4, 2, false)]
        {
            loop {
                let Some(line) = peer.next_line() else {
                    return;
                };
                match decode_line(&line).unwrap() {
                    WireMessage::Heartbeat(heartbeat) => {
                        if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                            HeartbeatAcknowledgementMessage {
                                server: heartbeat.server,
                                heartbeat_sequence: heartbeat.sequence,
                                received_at_ms: heartbeat.sent_at_ms,
                            },
                        )) {
                            return;
                        }
                    }
                    WireMessage::Command(command) => {
                        let expected = if active {
                            CommandPayload::RecordStart {
                                recorder: wire_recorder(recorder),
                            }
                        } else {
                            CommandPayload::RecordStop {
                                recorder: wire_recorder(recorder),
                            }
                        };
                        assert_eq!(command.payload, expected);
                        assert_eq!(command.expected_revision, Some(revision));
                        command_seen.send(()).unwrap();
                        release.recv_timeout(HEARTBEAT_TEST_TIMEOUT).unwrap();
                        revision += 1;
                        peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                            id: command.id,
                            revision,
                            scheduled_frame: None,
                        }));
                        peer.send(&WireMessage::Event(recording_event(
                            revision, recorder, active,
                        )));
                        peer.send(&WireMessage::RuntimeEvent(recording_runtime_event(
                            revision, sequence,
                        )));
                        break;
                    }
                    other => panic!("unexpected peer message: {other:?}"),
                }
            }
        }
        // The test shuts the worker down as soon as every command resolved, so
        // a closed pipe while answering the final heartbeats simply ends this
        // scripted peer instead of failing the join.
        loop {
            let Some(line) = peer.next_line() else {
                return;
            };
            let WireMessage::Heartbeat(heartbeat) = decode_line(&line).unwrap() else {
                panic!("expected heartbeat");
            };
            if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                HeartbeatAcknowledgementMessage {
                    server: heartbeat.server,
                    heartbeat_sequence: heartbeat.sequence,
                    received_at_ms: heartbeat.sent_at_ms,
                },
            )) {
                return;
            }
        }
    }

    #[test]
    fn worker_recording_intents_send_slot_payloads_and_track_pending_per_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (command_seen, command_seen_receiver) = sync_channel(4);
        let (release, release_receiver) = sync_channel(4);
        let server = thread::spawn(move || {
            serve_worker_recording(&listener, &command_seen, &release_receiver);
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "recording-intent-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        let mut desired = [false, false];
        for (slot, active) in [(1_u8, true), (1, false), (2, true), (2, false)] {
            let index = usize::from(slot) - 1;
            let action = if active {
                RecordingAction::Start(wire_recorder(slot))
            } else {
                RecordingAction::Stop(wire_recorder(slot))
            };
            request_sender
                .send(WorkerRequest::Intent(StudioIntent::Recording(action)))
                .unwrap();
            command_seen_receiver
                .recv_timeout(HEARTBEAT_TEST_TIMEOUT)
                .unwrap();
            // Only the addressed slot reports pending while its command is
            // unresolved; the confirmed roster is unchanged.
            let mut expected_pending = [false, false];
            expected_pending[index] = true;
            let dispatched = receive_state_where(&updates, |state| state.recording_pending[index]);
            assert_eq!(dispatched.connection_status, StudioConnectionStatus::Ready);
            assert_eq!(dispatched.recording_pending, expected_pending);
            assert_eq!(dispatched.recording_desired, desired);
            assert_eq!(dispatched.pending_commands, 1);
            assert!(
                dispatched.error.is_none(),
                "recording dispatch carried an error: {:?}",
                dispatched.error
            );
            release.send(()).unwrap();
            desired[index] = active;
            let resolved = receive_state_where(&updates, |state| {
                !state.recording_pending.iter().any(|pending| *pending)
                    && state.pending_commands == 0
                    && state
                        .view
                        .as_ref()
                        .is_some_and(|view| view.recordings == recording_roster(desired))
            });
            assert_eq!(resolved.recording_desired, desired);
            assert_eq!(resolved.connection_status, StudioConnectionStatus::Ready);
            assert!(
                resolved.error.is_none(),
                "recording resolution carried an error: {:?}",
                resolved.error
            );
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    fn stream_target_40() -> WireStreamTargetId {
        WireStreamTargetId::new(NonZeroU128::new(40).unwrap())
    }

    fn streams_event(revision: u64, desired_running: bool) -> EventMessage {
        EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "e".into(),
                    state_epoch: 1,
                    log_id: "l".into(),
                },
                revision,
            },
            payload: EventPayload::StreamsChanged {
                streams: vec![fm_protocol::StreamStatus {
                    target: stream_target_40(),
                    name: "rtmp-primary".into(),
                    desired_running,
                    realized: StreamRealizedState::Starting,
                    detail: None,
                }],
            },
        }
    }

    /// Answers a scripted stop then start of stream target 40, holding each
    /// result open until the test observed that target pending.
    fn serve_worker_streams(
        listener: &TcpListener,
        command_seen: &SyncSender<()>,
        release: &Receiver<()>,
    ) {
        let mut peer = HeartbeatPeer::accept(listener);
        peer.handshake_request();
        peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        )));
        peer.send(&WireMessage::Snapshot(heartbeat_snapshot()));
        let mut revision = heartbeat_snapshot().revision;
        let mut script = [false, true].into_iter();
        let mut next = script.next();
        loop {
            let Some(line) = peer.next_line() else {
                return;
            };
            match decode_line(&line).unwrap() {
                WireMessage::Heartbeat(heartbeat) => {
                    // The test may shut down while heartbeats are in flight.
                    if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                        HeartbeatAcknowledgementMessage {
                            server: heartbeat.server,
                            heartbeat_sequence: heartbeat.sequence,
                            received_at_ms: heartbeat.sent_at_ms,
                        },
                    )) {
                        return;
                    }
                }
                WireMessage::Command(command) => {
                    let start = next.expect("no more scripted stream commands");
                    let expected = if start {
                        CommandPayload::StreamStart {
                            target: stream_target_40(),
                        }
                    } else {
                        CommandPayload::StreamStop {
                            target: stream_target_40(),
                        }
                    };
                    assert_eq!(command.payload, expected);
                    assert_eq!(command.expected_revision, Some(revision));
                    command_seen.send(()).unwrap();
                    release.recv_timeout(HEARTBEAT_TEST_TIMEOUT).unwrap();
                    revision += 1;
                    peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                        id: command.id,
                        revision,
                        scheduled_frame: None,
                    }));
                    peer.send(&WireMessage::Event(streams_event(revision, start)));
                    peer.send(&WireMessage::RuntimeEvent(recording_runtime_event(
                        revision,
                        revision - heartbeat_snapshot().revision,
                    )));
                    next = script.next();
                }
                other => panic!("unexpected peer message: {other:?}"),
            }
        }
    }

    #[test]
    fn worker_stream_intents_send_target_payloads_and_track_pending_per_target() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (command_seen, command_seen_receiver) = sync_channel(2);
        let (release, release_receiver) = sync_channel(2);
        let server = thread::spawn(move || {
            serve_worker_streams(&listener, &command_seen, &release_receiver);
        });
        let (request_sender, updates, worker) =
            spawn_existing_worker(address, "stream-intent-test");

        receive_connection_state(&updates, StudioConnectionStatus::Ready);
        for start in [false, true] {
            let action = if start {
                StreamAction::Start(stream_target_40())
            } else {
                StreamAction::Stop(stream_target_40())
            };
            request_sender
                .send(WorkerRequest::Intent(StudioIntent::Stream(action)))
                .unwrap();
            command_seen_receiver
                .recv_timeout(HEARTBEAT_TEST_TIMEOUT)
                .unwrap();
            let dispatched =
                receive_state_where(&updates, |state| !state.stream_pending.is_empty());
            assert_eq!(dispatched.stream_pending, vec![stream_target_40()]);
            // The scripted session grants no permissions; with `transition`
            // granted, the pending target still blocks a second action.
            let permitted = dispatched.clone().with_switcher_permissions(true, true);
            assert_eq!(fm_ui_egui::stream_action(&permitted, action), None);
            assert_eq!(dispatched.streams[0].desired_running, !start);
            assert!(
                dispatched.error.is_none(),
                "stream dispatch carried an error: {:?}",
                dispatched.error
            );
            release.send(()).unwrap();
            let resolved = receive_state_where(&updates, |state| {
                state.stream_pending.is_empty()
                    && state.pending_commands == 0
                    && state
                        .streams
                        .first()
                        .is_some_and(|stream| stream.desired_running == start)
            });
            assert!(
                resolved.error.is_none(),
                "stream resolution carried an error: {:?}",
                resolved.error
            );
            // The confirmed roster now offers the opposite action.
            let opposite = if start {
                StreamAction::Stop(stream_target_40())
            } else {
                StreamAction::Start(stream_target_40())
            };
            let permitted = resolved.with_switcher_permissions(true, true);
            assert_eq!(
                fm_ui_egui::stream_action(&permitted, opposite),
                Some(opposite)
            );
        }

        request_sender.send(WorkerRequest::Shutdown).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn queued_audio_edits_merge_with_the_latest_confirmed_strip() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let snapshot = heartbeat_snapshot();
        let input = snapshot.inputs[0].input.to_domain();
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        let confirmed = model.view().unwrap();
        assert_eq!(
            resolve_input_audio_strip(
                Some(&confirmed),
                input,
                InputAudioStripUpdate {
                    muted: Some(true),
                    ..InputAudioStripUpdate::default()
                },
            ),
            Ok(CommandPayload::SetInputAudioStrip {
                input: WireInputId::from_domain(input),
                gain_millidb: 0,
                balance_basis_points: 0,
                muted: true,
                soloed: false,
                follow_video: false,
                delay_samples: 0,
            })
        );
        let mut input_audio_strips = confirmed.input_audio_strips.clone();
        input_audio_strips[0].muted = true;
        let mut cursor = confirmed.cursor.clone();
        cursor.revision = cursor.revision.checked_next().unwrap();
        model
            .apply_event(DurableProjectEvent {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor,
                change: DurableChange::DesiredSwitcher {
                    selection: confirmed.switcher.desired,
                    manual_transition: confirmed.switcher.desired_manual_transition,
                    fade_to_black: confirmed.switcher.desired_fade_to_black,
                    overlays: confirmed.desired_overlays.clone(),
                    input_audio_strips,
                },
            })
            .unwrap();
        let confirmed = model.view().unwrap();
        assert_eq!(
            resolve_input_audio_strip(
                Some(&confirmed),
                input,
                InputAudioStripUpdate {
                    soloed: Some(true),
                    ..InputAudioStripUpdate::default()
                },
            ),
            Ok(CommandPayload::SetInputAudioStrip {
                input: WireInputId::from_domain(input),
                gain_millidb: 0,
                balance_basis_points: 0,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 0,
            })
        );
    }

    #[test]
    fn input_moves_resolve_each_step_from_latest_confirmed_order() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let ids = [2_u128, 3, 4].map(|id| WireInputId::new(NonZeroU128::new(id).unwrap()));
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project,
                heartbeat_snapshot(),
            ))
            .unwrap();
        let mut first = model.view().unwrap();
        first.inputs = ids.map(WireInputId::to_domain).to_vec();
        let first_payload = intent_payload(
            StudioIntent::MoveInput {
                input: ids[0].to_domain(),
                direction: InputMoveDirection::Down,
            },
            Some(&first),
        )
        .unwrap();
        assert_eq!(
            first_payload,
            CommandPayload::ReorderInputs {
                inputs: vec![ids[1], ids[0], ids[2]]
            }
        );
        let mut latest = first.clone();
        latest.inputs = vec![ids[1].to_domain(), ids[0].to_domain(), ids[2].to_domain()];
        assert_eq!(
            intent_payload(
                StudioIntent::MoveInput {
                    input: ids[0].to_domain(),
                    direction: InputMoveDirection::Down,
                },
                Some(&latest),
            ),
            Ok(CommandPayload::ReorderInputs {
                inputs: vec![ids[1], ids[2], ids[0]]
            })
        );
    }

    #[test]
    fn input_catalogue_intents_map_to_add_and_remove_payloads() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project,
                heartbeat_snapshot(),
            ))
            .unwrap();
        let view = model.view().unwrap();
        let existing = view.inputs[0];
        let fresh = WireInputId::new(NonZeroU128::new(9).unwrap());
        let source = fm_protocol::LiveInputSource::Solid {
            red: 1,
            green: 2,
            blue: 3,
            alpha: 255,
        };
        assert_eq!(
            intent_payload(
                StudioIntent::AddInput {
                    input: fresh.to_domain(),
                    name: "Solid Colour 1".to_owned(),
                    source,
                },
                Some(&view),
            ),
            Ok(CommandPayload::AddInput {
                input: fresh,
                name: "Solid Colour 1".to_owned(),
                source,
            })
        );
        // A reused identifier is refused before sending.
        assert!(
            intent_payload(
                StudioIntent::AddInput {
                    input: existing,
                    name: "Bars".to_owned(),
                    source: fm_protocol::LiveInputSource::Bars,
                },
                Some(&view),
            )
            .unwrap_err()
            .contains("already exists")
        );
        assert_eq!(
            intent_payload(StudioIntent::RemoveInput { input: existing }, Some(&view)),
            Ok(CommandPayload::RemoveInput {
                input: WireInputId::from_domain(existing),
            })
        );
        assert_eq!(
            intent_payload(
                StudioIntent::RemoveInput {
                    input: fresh.to_domain()
                },
                Some(&view)
            ),
            Err("Input no longer exists".to_owned())
        );
        assert!(intent_payload(StudioIntent::RemoveInput { input: existing }, None).is_err());
    }

    #[test]
    fn catalogue_commands_publish_pending_requests_and_refusal_reasons() {
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let mut client = Client::new(crate::native_client_config(
            Role::Admin,
            "studio-catalogue",
            project,
        ))
        .unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client
            .accept_handshake(heartbeat_handshake(HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            }))
            .unwrap();
        client.apply_snapshot(heartbeat_snapshot()).unwrap();
        let fresh = WireInputId::new(NonZeroU128::new(9).unwrap());
        let existing = WireInputId::new(NonZeroU128::new(2).unwrap());
        let add = client
            .queue_command(
                CommandPayload::AddInput {
                    input: fresh,
                    name: "Colour Bars 1".to_owned(),
                    source: fm_protocol::LiveInputSource::Bars,
                },
                "catalogue-add",
                Some(4),
                None,
            )
            .unwrap();
        client
            .queue_command(CommandPayload::Cut, "catalogue-cut", Some(4), None)
            .unwrap();
        let remove = client
            .queue_command(
                CommandPayload::RemoveInput { input: existing },
                "catalogue-remove",
                Some(4),
                None,
            )
            .unwrap();
        assert_eq!(
            catalogue_pending_requests(&client),
            [
                CatalogueRequest::Add {
                    input: fresh.to_domain(),
                    name: "Colour Bars 1".to_owned(),
                },
                CatalogueRequest::Remove {
                    input: existing.to_domain(),
                },
            ]
        );
        let rejected = |id: &str, fields: Vec<fm_protocol::FieldIssue>| CommandResult::Rejected {
            id: id.to_owned(),
            code: "conflict".to_owned(),
            message: "input 2 cannot be removed: it is on Program".to_owned(),
            fields,
            current_revision: 4,
            retryable: false,
        };
        let in_use = rejected(
            &remove.id,
            vec![fm_protocol::FieldIssue {
                field: "input".to_owned(),
                code: "input_in_use".to_owned(),
                message: "input 2 cannot be removed: it is on Program".to_owned(),
            }],
        );
        assert_eq!(
            catalogue_refusal(&client, &remove.id, &in_use),
            Some(CatalogueRefusal {
                command_id: remove.id.clone(),
                request: CatalogueRequest::Remove {
                    input: existing.to_domain(),
                },
                code: "conflict".to_owned(),
                reason: Some("input_in_use".to_owned()),
                message: "input 2 cannot be removed: it is on Program".to_owned(),
            })
        );
        // A daemon refusal without a field issue keeps its message as detail.
        let refusal = catalogue_refusal(&client, &add.id, &rejected(&add.id, Vec::new())).unwrap();
        assert_eq!(refusal.reason, None);
        assert!(refusal.message.contains("cannot be removed"));
        // Other commands and accepted results carry no catalogue refusal.
        assert_eq!(
            catalogue_refusal(&client, "unknown", &rejected("unknown", Vec::new())),
            None
        );
        let accepted = CommandResult::Accepted {
            id: add.id.clone(),
            revision: 5,
            scheduled_frame: None,
        };
        assert_eq!(catalogue_refusal(&client, &add.id, &accepted), None);
    }

    #[test]
    fn recording_intents_map_to_slot_qualified_record_payloads() {
        assert_eq!(
            intent_payload(
                StudioIntent::Recording(RecordingAction::Start(wire_recorder(1))),
                None
            ),
            Ok(CommandPayload::RecordStart {
                recorder: wire_recorder(1)
            })
        );
        assert_eq!(
            intent_payload(
                StudioIntent::Recording(RecordingAction::Stop(wire_recorder(2))),
                None
            ),
            Ok(CommandPayload::RecordStop {
                recorder: wire_recorder(2)
            })
        );
    }

    #[test]
    fn stream_intents_map_to_target_payloads_from_the_confirmed_roster() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project,
                heartbeat_snapshot(),
            ))
            .unwrap();
        let view = model.view().unwrap();
        assert_eq!(
            intent_payload(
                StudioIntent::Stream(StreamAction::Start(stream_target_40())),
                Some(&view)
            ),
            Ok(CommandPayload::StreamStart {
                target: stream_target_40()
            })
        );
        assert_eq!(
            intent_payload(
                StudioIntent::Stream(StreamAction::Stop(stream_target_40())),
                Some(&view)
            ),
            Ok(CommandPayload::StreamStop {
                target: stream_target_40()
            })
        );
        let missing = WireStreamTargetId::new(NonZeroU128::new(41).unwrap());
        assert_eq!(
            intent_payload(
                StudioIntent::Stream(StreamAction::Start(missing)),
                Some(&view)
            ),
            Err("Stream target no longer exists".to_owned())
        );
        assert!(
            intent_payload(
                StudioIntent::Stream(StreamAction::Start(stream_target_40())),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn stinger_intents_keep_confirmed_settings_or_use_studio_defaults() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let mut snapshot = heartbeat_snapshot();
        let first = snapshot.inputs[0].input;
        let second = WireInputId::new(NonZeroU128::new(3).unwrap());
        snapshot.inputs.push(InputStatus {
            input: second,
            name: "clip".into(),
        });
        snapshot.input_audio_strips.push(InputAudioStripStatus {
            input: second,
            ..snapshot.input_audio_strips[0]
        });
        let slot = |number| fm_protocol::WireStingerSlotId::new(number).unwrap();
        snapshot.stingers = vec![fm_protocol::StingerStatus {
            slot: slot(2),
            media_input: first,
            preload: false,
            cut_point_frames: 12,
            audio_policy: StingerAudioPolicy::Muted,
            missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
            readiness: fm_protocol::StingerReadiness::NotRequested,
        }];
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        let view = model.view().unwrap();
        let assign = |number, media: WireInputId| StudioIntent::AssignStingerMedia {
            slot: slot(number),
            media_input: media.to_domain(),
            default_cut_point_frames: 15,
        };
        // A configured slot changes only its media.
        assert_eq!(
            intent_payload(assign(2, second), Some(&view)),
            Ok(CommandPayload::ConfigureStinger {
                slot: slot(2),
                media_input: second,
                preload: false,
                cut_point_frames: 12,
                audio_policy: StingerAudioPolicy::Muted,
                missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
            })
        );
        // An unconfigured slot gets the Studio defaults.
        assert_eq!(
            intent_payload(assign(5, second), Some(&view)),
            Ok(CommandPayload::ConfigureStinger {
                slot: slot(5),
                media_input: second,
                preload: true,
                cut_point_frames: 15,
                audio_policy: StingerAudioPolicy::MixWithProgram,
                missing_media_fallback: StingerMissingMediaFallback::Cut,
            })
        );
        let gone = WireInputId::new(NonZeroU128::new(99).unwrap());
        assert!(intent_payload(assign(1, gone), Some(&view)).is_err());
        assert!(intent_payload(assign(1, second), None).is_err());
        assert_eq!(
            intent_payload(StudioIntent::RemoveStinger { slot: slot(2) }, Some(&view)),
            Ok(CommandPayload::RemoveStinger { slot: slot(2) })
        );
        assert_eq!(
            intent_payload(StudioIntent::RemoveStinger { slot: slot(3) }, Some(&view)),
            Err("Stinger slot 3 is not configured".to_owned())
        );
    }

    fn apply_overlay_change(
        model: &mut ClientModel,
        view: &ClientView,
        change: impl FnOnce(&mut fm_ui_model::OverlayStatus),
    ) {
        let mut overlays = view.desired_overlays.clone();
        change(&mut overlays[0]);
        let mut cursor = view.cursor.clone();
        cursor.revision = cursor.revision.checked_next().unwrap();
        model
            .apply_event(DurableProjectEvent {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor,
                change: DurableChange::DesiredSwitcher {
                    selection: view.switcher.desired,
                    manual_transition: view.switcher.desired_manual_transition,
                    fade_to_black: view.switcher.desired_fade_to_black,
                    overlays,
                    input_audio_strips: view.input_audio_strips.clone(),
                },
            })
            .unwrap();
    }

    fn assert_overlay_payload(view: &ClientView, intent: StudioIntent, expected: CommandPayload) {
        assert_eq!(intent_payload(intent, Some(view)), Ok(expected));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn osc_ingress_obeys_limits_gates_and_worker_order() {
        let address = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let receiver = OscReceiver::bind(address).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let queued = [
            "/freemix/switcher/preview/1",
            "/freemix/switcher/cut",
            "/freemix/switcher/fade",
            "/freemix/switcher/preview/1",
            "/freemix/switcher/cut",
            "/freemix/switcher/fade",
            "/freemix/switcher/preview/1",
            "/freemix/switcher/cut",
        ]
        .iter()
        .flat_map(|address| [*address, *address])
        .collect::<Vec<_>>();
        for address in queued.iter().chain(std::iter::once(&queued[0])) {
            sender
                .send_to(&osc_message(address), receiver.local_addr())
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while receiver.counters().overflow == 0 && Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }
        assert_eq!(receiver.counters().overflow, 1);

        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project,
                heartbeat_snapshot(),
            ))
            .unwrap();
        let view = model.view().unwrap();
        let ready = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view.clone())
            .with_switcher_permissions(true, true);
        assert_eq!(
            external_intent(&ready, ExternalStudioAction::SelectPreview(1), 42, 84),
            Some(StudioIntent::SelectPreview(view.inputs[0]))
        );
        assert_eq!(
            external_intent(
                &ready,
                ExternalStudioAction::FadeToBlack { active: true },
                42,
                84
            ),
            Some(StudioIntent::FadeToBlack {
                active: true,
                duration_frames: 84
            })
        );
        let mut black = ready.clone();
        black
            .view
            .as_mut()
            .unwrap()
            .switcher
            .desired_fade_to_black
            .target_active = true;
        assert_eq!(
            external_intent(
                &black,
                ExternalStudioAction::FadeToBlack { active: false },
                42,
                84
            ),
            Some(StudioIntent::FadeToBlack {
                active: false,
                duration_frames: 84
            })
        );
        assert_eq!(
            external_intent(&ready, ExternalStudioAction::Fade, 42, 84),
            Some(StudioIntent::Fade {
                duration_frames: 42
            })
        );
        for (action, intent) in [
            (
                ExternalStudioAction::AlphaFade,
                StudioIntent::AlphaFade {
                    duration_frames: 42,
                },
            ),
            (
                ExternalStudioAction::Slide,
                StudioIntent::Slide {
                    duration_frames: 42,
                },
            ),
            (
                ExternalStudioAction::Zoom,
                StudioIntent::Zoom {
                    duration_frames: 42,
                },
            ),
            (
                ExternalStudioAction::Wipe,
                StudioIntent::Wipe {
                    duration_frames: 42,
                },
            ),
        ] {
            assert_eq!(external_intent(&ready, action, 42, 84), Some(intent));
        }
        let mut manual = ready.clone();
        let active_manual_transition = fm_ui_model::ActiveManualTransition {
            kind: fm_protocol::ManualTransitionKind::Fade,
            from: view.switcher.desired.preview,
            to: view.switcher.desired.program,
            interval_start: ManualTransitionPosition::START,
            position: ManualTransitionPosition::START,
        };
        manual
            .view
            .as_mut()
            .unwrap()
            .switcher
            .desired_manual_transition =
            fm_ui_model::ManualTransitionStatus::Active(active_manual_transition);
        assert!(
            external_intent(&ready, ExternalStudioAction::CommitManualTransition, 42, 84).is_none()
        );
        let manual_ready = manual.clone();
        let manual_denied = manual.clone().with_switcher_permissions(true, false);
        assert!(
            external_intent(
                &manual_denied,
                ExternalStudioAction::CancelManualTransition,
                42,
                84
            )
            .is_none()
        );
        assert!(external_intent(&manual, ExternalStudioAction::Cut, 42, 84).is_none());
        manual
            .view
            .as_mut()
            .unwrap()
            .switcher
            .desired_manual_transition = fm_ui_model::ManualTransitionStatus::Inactive;
        manual
            .view
            .as_mut()
            .unwrap()
            .switcher
            .realized_manual_transition =
            fm_ui_model::ManualTransitionStatus::Active(fm_ui_model::ActiveManualTransition {
                kind: fm_protocol::ManualTransitionKind::Fade,
                from: view.switcher.realized.preview,
                to: view.switcher.realized.program,
                interval_start: ManualTransitionPosition::START,
                position: ManualTransitionPosition::START,
            });
        assert!(external_intent(&manual, ExternalStudioAction::Cut, 42, 84).is_none());
        let denied = StudioUiState::new(StudioConnectionStatus::Synchronizing)
            .with_view(view.clone())
            .with_switcher_permissions(true, true);
        assert!(external_intent(&denied, ExternalStudioAction::Cut, 42, 84).is_none());

        let (tx, rx) = sync_channel(REQUEST_CAPACITY);
        let mut app = StudioApp {
            shell: StudioShell::default(),
            state: ready,
            requests: Some(tx),
            pending_intents: PendingIntents::new(),
            updates: None,
            worker: None,
            shutdown_sent: false,
            osc: Some(receiver),
            osc_rejected: 0,
            osc_notice: None,
            preferences: PreferenceStore::new(None, fm_ui_model::i18n::Locale::ENGLISH),
            preferences_notice: None,
        };
        app.shell.set_transition_duration_frames(42);
        app.shell.set_fade_to_black_duration_frames(84);
        app.drain_osc();
        assert_eq!(app.pending_intents.intents.len(), 0);
        app.drain_osc();
        let order = rx
            .try_iter()
            .map(|request| match request {
                WorkerRequest::Intent(intent) => intent,
                WorkerRequest::Shutdown => panic!("unexpected shutdown"),
            })
            .collect::<Vec<_>>();
        let expected = [
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::Cut,
            StudioIntent::Cut,
            StudioIntent::Fade {
                duration_frames: 42,
            },
            StudioIntent::Fade {
                duration_frames: 42,
            },
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::Cut,
            StudioIntent::Cut,
            StudioIntent::Fade {
                duration_frames: 42,
            },
            StudioIntent::Fade {
                duration_frames: 42,
            },
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::SelectPreview(view.inputs[0]),
            StudioIntent::Cut,
            StudioIntent::Cut,
        ];
        assert_eq!(order, expected);

        app.state = manual_ready;
        let osc_address = app.osc.as_ref().unwrap().local_addr();
        for address in [
            "/freemix/switcher/manual/commit",
            "/freemix/switcher/manual/cancel",
        ] {
            sender.send_to(&osc_message(address), osc_address).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut manual_order = Vec::new();
        while manual_order.len() < 2 && Instant::now() < deadline {
            app.drain_osc();
            manual_order.extend(rx.try_iter().map(|request| match request {
                WorkerRequest::Intent(intent) => intent,
                WorkerRequest::Shutdown => panic!("unexpected shutdown"),
            }));
            if manual_order.len() < 2 {
                sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(
            manual_order,
            [
                StudioIntent::CommitManualTransition,
                StudioIntent::CancelManualTransition,
            ]
        );
    }

    fn osc_message(address: &str) -> Vec<u8> {
        let mut message = Vec::new();
        for value in [address, ","] {
            message.extend_from_slice(value.as_bytes());
            message.push(0);
            while !message.len().is_multiple_of(4) {
                message.push(0);
            }
        }
        message
    }

    #[test]
    fn fade_to_black_shortcut_resolves_each_press_from_latest_confirmed_desired_target() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project,
                heartbeat_snapshot(),
            ))
            .unwrap();
        let confirmed = model.view().unwrap();
        let intent = StudioIntent::ToggleFadeToBlack {
            duration_frames: 84,
        };
        let assert_target = |view: &ClientView, active| {
            assert_eq!(
                intent_payload(intent.clone(), Some(view)),
                Ok(CommandPayload::FadeToBlack {
                    active,
                    duration_frames: 84,
                })
            );
        };
        assert_target(&confirmed, true);

        let mut cursor = confirmed.cursor.clone();
        cursor.revision = cursor.revision.checked_next().unwrap();
        model
            .apply_event(DurableProjectEvent {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor,
                change: DurableChange::DesiredSwitcher {
                    selection: confirmed.switcher.desired,
                    manual_transition: confirmed.switcher.desired_manual_transition,
                    fade_to_black: FadeToBlackState {
                        target_active: true,
                        position: FadeToBlackPosition::LIVE,
                    },
                    overlays: confirmed.desired_overlays.clone(),
                    input_audio_strips: confirmed.input_audio_strips.clone(),
                },
            })
            .unwrap();
        let confirmed = model.view().unwrap();
        assert_target(&confirmed, false);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn queued_overlay_actions_resolve_from_the_latest_confirmed_channel() {
        let project = ProjectId::new(NonZeroU128::new(7).unwrap());
        let output = WireOutputId::new(NonZeroU128::new(9).unwrap());
        let mut snapshot = heartbeat_snapshot();
        snapshot.outputs = vec![OutputStatus {
            output,
            name: "Program".into(),
        }];
        let channel = snapshot.desired_overlays[0].channel;
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        let confirmed = model.view().unwrap();
        let preview = confirmed.switcher.desired.preview;
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.active = true;
            overlay.source = Some(preview);
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::UpdateOverlay {
                channel,
                source: confirmed.switcher.desired.preview,
            },
            CommandPayload::UpdateOverlay {
                channel,
                source: WireInputId::from_domain(confirmed.switcher.desired.preview),
            },
        );
        assert_overlay_payload(
            &confirmed,
            StudioIntent::ToggleOverlayTransition {
                channel,
                duration_frames: 42,
            },
            CommandPayload::ConfigureOverlayTransition {
                channel,
                transition: OverlayTransitionKind::Fade,
                duration_frames: 42,
            },
        );
        assert_overlay_payload(
            &confirmed,
            StudioIntent::ToggleOverlayOutput {
                channel,
                output: output.to_domain(),
            },
            CommandPayload::SetOverlayOutputInclusion {
                channel,
                output,
                included: true,
            },
        );
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.transition = OverlayTransitionKind::Fade;
            overlay.included_outputs.push(output.to_domain());
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::ToggleOverlayTransition {
                channel,
                duration_frames: 42,
            },
            CommandPayload::ConfigureOverlayTransition {
                channel,
                transition: OverlayTransitionKind::Cut,
                duration_frames: 42,
            },
        );
        assert_overlay_payload(
            &confirmed,
            StudioIntent::ToggleOverlayOutput {
                channel,
                output: output.to_domain(),
            },
            CommandPayload::SetOverlayOutputInclusion {
                channel,
                output,
                included: false,
            },
        );
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.transition = OverlayTransitionKind::Cut;
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::CycleOverlayPosition { channel },
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: OverlayPositionPreset::TopLeft,
                border: OverlayBorderPreset::None,
            },
        );
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.position = OverlayPositionPreset::TopLeft;
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::CycleOverlayPosition { channel },
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: OverlayPositionPreset::TopRight,
                border: OverlayBorderPreset::None,
            },
        );
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.position = OverlayPositionPreset::TopRight;
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::CycleOverlayBorder { channel },
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: OverlayPositionPreset::TopRight,
                border: OverlayBorderPreset::ThinWhite,
            },
        );
        apply_overlay_change(&mut model, &confirmed, |overlay| {
            overlay.border = OverlayBorderPreset::ThinWhite;
        });
        let confirmed = model.view().unwrap();
        assert_overlay_payload(
            &confirmed,
            StudioIntent::CycleOverlayBorder { channel },
            CommandPayload::ConfigureOverlayAppearance {
                channel,
                position: OverlayPositionPreset::TopRight,
                border: OverlayBorderPreset::ThickWhite,
            },
        );
    }

    mod titles {
        use fm_protocol::{
            TitleFieldStatus, TitleFieldValue, TitleStateMessage, TitleSummary,
            WireOverlayChannelId, WireTitleFieldId,
        };
        use fm_ui_model::title_editor::{
            TitleCommand, TitleCommandBody, TitleReply, TitleSnapshot,
        };

        use super::*;

        const TITLE_INPUT: u128 = 3;

        fn title_input() -> WireInputId {
            WireInputId::new(NonZeroU128::new(TITLE_INPUT).unwrap())
        }

        fn title_field() -> WireTitleFieldId {
            WireTitleFieldId::new(NonZeroU128::new(1).unwrap())
        }

        /// The heartbeat show plus title input 3 (one text field).
        fn title_snapshot() -> SnapshotMessage {
            let mut snapshot = heartbeat_snapshot();
            snapshot.inputs.push(InputStatus {
                input: title_input(),
                name: "Lower third".into(),
            });
            let mut strip = snapshot.input_audio_strips[0];
            strip.input = title_input();
            snapshot.input_audio_strips.push(strip);
            snapshot.titles = vec![TitleSummary {
                input: title_input(),
                field_revision: 0,
            }];
            snapshot
        }

        fn title_state() -> TitleStateMessage {
            TitleStateMessage {
                revision: title_snapshot().revision,
                input: title_input(),
                name: "Lower third".into(),
                field_revision: 0,
                fields: vec![TitleFieldStatus {
                    field: title_field(),
                    name: "Name".into(),
                    value: TitleFieldValue::Text("Ana".into()),
                }],
            }
        }

        fn command(request: u64, body: TitleCommandBody) -> TitleCommand {
            TitleCommand {
                request,
                title: title_input().to_domain(),
                body,
            }
        }

        fn take(request: u64, channel: u8) -> TitleCommand {
            command(
                request,
                TitleCommandBody::Take {
                    channel,
                    base_revision: 0,
                    fields: Vec::new(),
                },
            )
        }

        fn taken_event(revision: u64) -> EventMessage {
            let mut overlays = OverlayStatus::empty_channels();
            overlays[0].source = Some(title_input());
            overlays[0].active = true;
            overlays[0].opacity = 255;
            EventMessage {
                payload: EventPayload::TitleChanged {
                    input: title_input(),
                    field_revision: 0,
                    fields: vec![fm_protocol::TitleFieldUpdate {
                        field: title_field(),
                        value: TitleFieldValue::Text("Ana".into()),
                    }],
                    overlays,
                },
                ..command_event(revision)
            }
        }

        #[test]
        fn title_intents_encode_their_protocol_payloads() {
            let wire = title_input();
            assert_eq!(
                intent_payload(StudioIntent::Title(take(1, 3)), None),
                Ok(CommandPayload::TakeTitle {
                    input: wire,
                    channel: WireOverlayChannelId::new(3).unwrap(),
                    base_revision: 0,
                    fields: Vec::new(),
                })
            );
            assert_eq!(
                intent_payload(
                    StudioIntent::Title(command(2, TitleCommandBody::OffAir)),
                    None
                ),
                Ok(CommandPayload::TitleOffAir { input: wire })
            );
            let save = command(
                3,
                TitleCommandBody::Save {
                    base_revision: 4,
                    fields: vec![(title_field(), TitleFieldValue::Text("Bea".into()))],
                },
            );
            assert!(matches!(
                intent_payload(StudioIntent::Title(save), None),
                Ok(CommandPayload::SetTitleFields {
                    base_revision: 4,
                    require: fm_protocol::TitleAirRequirement::OffAir,
                    ..
                })
            ));
            let refused = intent_payload(StudioIntent::Title(take(4, 0)), None).unwrap_err();
            assert!(refused.contains("overlay channel 0"), "{refused}");
            assert_eq!(StudioIntent::Title(take(9, 1)).title_request(), Some(9));
            assert_eq!(StudioIntent::Cut.title_request(), None);
        }

        #[test]
        fn title_updates_survive_the_latest_state_mailbox_in_order() {
            let (publisher, updates) = state_mailbox(egui::Context::default());
            let mut first = StudioUiState::new(StudioConnectionStatus::Ready);
            first.title_updates = vec![TitleUpdate::Abandoned { request: 1 }];
            let mut second = StudioUiState::new(StudioConnectionStatus::Backoff);
            second.title_updates = vec![TitleUpdate::Abandoned { request: 2 }];
            assert!(publisher.publish(first));
            assert!(publisher.publish(second));
            assert!(publisher.publish(StudioUiState::new(StudioConnectionStatus::Backoff)));
            let latest = updates.try_recv().unwrap();
            assert_eq!(latest.connection_status, StudioConnectionStatus::Backoff);
            assert_eq!(
                latest.title_updates,
                [
                    TitleUpdate::Abandoned { request: 1 },
                    TitleUpdate::Abandoned { request: 2 }
                ]
            );
            // Taken once: the next state starts empty.
            assert!(publisher.publish(StudioUiState::new(StudioConnectionStatus::Ready)));
            assert_eq!(updates.try_recv().unwrap().title_updates, Vec::new());
        }

        #[test]
        fn title_intents_are_never_dropped_and_report_lost_requests() {
            let mut pending = PendingIntents::new();
            pending.push(StudioIntent::Cut).unwrap();
            pending.push(StudioIntent::Title(take(5, 1))).unwrap();
            pending
                .push(StudioIntent::Title(command(6, TitleCommandBody::OffAir)))
                .unwrap();
            assert_eq!(pending.clear(), [5, 6]);

            let (sender, receiver) = sync_channel(1);
            for _ in 0..=DEFERRED_INTENT_CAPACITY {
                sender
                    .try_send(WorkerRequest::Intent(StudioIntent::Cut))
                    .unwrap_or(());
                let _ = receiver.try_recv();
            }
            // A full deferred FIFO rejects other intents but keeps titles.
            let mut deferred: VecDeque<StudioIntent> =
                std::iter::repeat_n(StudioIntent::Cut, DEFERRED_INTENT_CAPACITY).collect();
            let mut rejected = 0;
            sender
                .try_send(WorkerRequest::Intent(StudioIntent::Wipe {
                    duration_frames: 1,
                }))
                .unwrap();
            assert!(!cancellation_requested(
                &receiver,
                &mut deferred,
                &mut rejected
            ));
            assert_eq!(rejected, 1);
            sender
                .try_send(WorkerRequest::Intent(StudioIntent::Title(take(7, 1))))
                .unwrap();
            assert!(!cancellation_requested(
                &receiver,
                &mut deferred,
                &mut rejected
            ));
            assert_eq!(rejected, 1);
            assert_eq!(
                deferred.back().and_then(StudioIntent::title_request),
                Some(7)
            );
        }

        /// Serves a title show: answers a `title_take` with its result,
        /// `title_changed`, and runtime realization, then drops the
        /// connection on the next command, after accepting it when
        /// `accept_then_drop` (so its reply is held for an event that never
        /// comes) or before any result otherwise.
        fn serve_worker_titles(
            listener: &TcpListener,
            command_seen: &SyncSender<CommandPayload>,
            accept_then_drop: bool,
        ) {
            let mut peer = HeartbeatPeer::accept(listener);
            peer.handshake_request();
            peer.send(&WireMessage::HandshakeResponse(heartbeat_handshake(
                HandshakeOutcome::Snapshot {
                    reason: SnapshotReason::NoCursor,
                },
            )));
            peer.send(&WireMessage::Snapshot(title_snapshot()));
            peer.send(&WireMessage::TitleState(title_state()));
            let mut answered = false;
            loop {
                let Some(line) = peer.next_line() else {
                    return;
                };
                match decode_line(&line).unwrap() {
                    WireMessage::Heartbeat(heartbeat) => {
                        if !peer.send_unless_closed(&WireMessage::HeartbeatAcknowledgement(
                            HeartbeatAcknowledgementMessage {
                                server: heartbeat.server,
                                heartbeat_sequence: heartbeat.sequence,
                                received_at_ms: heartbeat.sent_at_ms,
                            },
                        )) {
                            return;
                        }
                    }
                    WireMessage::Command(command) => {
                        // Title commands carry their own base revision.
                        assert_eq!(command.expected_revision, None);
                        command_seen.send(command.payload).unwrap();
                        if answered {
                            if accept_then_drop {
                                peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                                    id: command.id,
                                    revision: title_snapshot().revision + 2,
                                    scheduled_frame: None,
                                }));
                            }
                            return;
                        }
                        answered = true;
                        let revision = title_snapshot().revision + 1;
                        peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
                            id: command.id,
                            revision,
                            scheduled_frame: None,
                        }));
                        peer.send(&WireMessage::Event(taken_event(revision)));
                        peer.send(&WireMessage::RuntimeEvent(command_runtime_event(revision)));
                    }
                    other => panic!("unexpected peer message: {other:?}"),
                }
            }
        }

        #[test]
        fn worker_abandons_a_sent_title_command_on_connection_loss() {
            worker_routes_title_results_and_abandons_on_connection_loss(false);
        }

        #[test]
        fn worker_abandons_a_held_title_reply_on_connection_loss() {
            worker_routes_title_results_and_abandons_on_connection_loss(true);
        }

        fn worker_routes_title_results_and_abandons_on_connection_loss(accept_then_drop: bool) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (command_seen, commands) = sync_channel(4);
            let server = thread::spawn(move || {
                serve_worker_titles(&listener, &command_seen, accept_then_drop);
            });
            let (request_sender, updates, worker) =
                spawn_existing_worker(address, "title-intent-test");
            let ready = receive_state_where(&updates, |state| {
                state.connection_status == StudioConnectionStatus::Ready
                    && state.project_state.is_some()
            });
            let project = ready.project_state.unwrap();
            let before = TitleSnapshot::from_state(&project, title_input().to_domain()).unwrap();
            assert!(!before.on_air);
            // The test session holds no permissions, so neither may be used.
            assert!(ready.title_access.is_ready() && !ready.title_access.can_air());

            request_sender
                .send(WorkerRequest::Intent(StudioIntent::Title(take(41, 1))))
                .unwrap();
            assert_eq!(
                commands.recv_timeout(HEARTBEAT_TEST_TIMEOUT).unwrap(),
                CommandPayload::TakeTitle {
                    input: title_input(),
                    channel: WireOverlayChannelId::new(1).unwrap(),
                    base_revision: 0,
                    fields: Vec::new(),
                }
            );
            // The acceptance is held until `title_changed` is applied, so
            // the reply already shows the title on air.
            let replied = receive_state_where(&updates, |state| !state.title_updates.is_empty());
            let [TitleUpdate::Reply(TitleReply::Applied { request, snapshot })] =
                replied.title_updates.as_slice()
            else {
                panic!("expected one applied reply: {:?}", replied.title_updates);
            };
            assert_eq!(*request, 41);
            assert!(snapshot.on_air);
            assert_eq!(snapshot.fields, before.fields);
            assert!(replied.error.is_none(), "{:?}", replied.error);

            request_sender
                .send(WorkerRequest::Intent(StudioIntent::Title(command(
                    42,
                    TitleCommandBody::OffAir,
                ))))
                .unwrap();
            assert_eq!(
                commands.recv_timeout(HEARTBEAT_TEST_TIMEOUT).unwrap(),
                CommandPayload::TitleOffAir {
                    input: title_input()
                }
            );
            let lost = receive_state_where(&updates, |state| !state.title_updates.is_empty());
            assert_eq!(lost.title_updates, [TitleUpdate::Abandoned { request: 42 }]);
            assert_ne!(lost.connection_status, StudioConnectionStatus::Ready);
            assert!(!lost.title_access.is_ready());

            request_sender.send(WorkerRequest::Shutdown).unwrap();
            worker.join().unwrap();
            server.join().unwrap();
        }
    }
}
