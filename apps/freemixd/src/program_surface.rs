//! Borderless fullscreen display outputs (OR-011) and their platform-neutral
//! presentation policies.
//!
//! One winit event loop owns one borderless, cursor-hidden window per
//! display-output slot (hidden while the slot is empty). A shared
//! [`DisplayOutputTracker`] matches each configured slot to its display
//! (stable id, then name), parks the window while its display is absent, and
//! reattaches it when the display returns. Each window's surface is
//! presented by its own [`ProgramPresentation`] on the daemon worker with
//! the slot's own source and [`fm_types::OutputScaling`].
//!
//! The worker configures slots live through [`WindowedDisplays`]: it
//! queues each slot edit and wakes the event loop, which applies it to the
//! tracker and publishes every slot's state and the monitor inventory back
//! as a [`DisplayReport`].

use std::{
    future::Future,
    net::SocketAddr,
    num::NonZeroU32,
    path::PathBuf,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use super::{
    RECORDER_SLOTS,
    display_outputs::{DisplayReport, cleared_record, status_record},
};
use fm_color::NativeSdrOutputTransform;
use fm_gpu::{
    FrameDecision, FrameGeneration, NativeContext, NativeGpuError, NativeSurface,
    NativeSurfaceAcquire, NativeSurfaceFactory, NativeTexture, PresentationAction,
    PresentationFrame, PresentationLifecycle, PresentationState, ResizeGeneration,
    SurfaceAcquisition,
};
use fm_io_api::display::{
    DisplayId, DisplayInfo, DisplayMode, DisplayOutputConfig, DisplayOutputReconfiguration,
    DisplayOutputSlot, DisplayOutputSource, DisplayOutputTracker, MAX_DISPLAY_OUTPUTS,
};
use fm_types::OutputScaling;

#[derive(Clone)]
pub(super) struct ShutdownSignal(Arc<AtomicBool>);

impl ShutdownSignal {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn request(&self) -> bool {
        !self.0.swap(true, AtomicOrdering::AcqRel)
    }

    pub(super) fn requested(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

fn begin_shutdown<T>(signal: &ShutdownSignal, deadline: &mut Option<T>, value: T) -> bool {
    if !signal.request() {
        return false;
    }
    *deadline = Some(value);
    true
}

fn program_shutdown_timeout(recorder: bool) -> Duration {
    if recorder {
        super::PROGRAM_RECORDER_STOP_TIMEOUT
            + super::PROGRAM_RECORDER_KILL_TIMEOUT
            + super::PROGRAM_CHECKPOINT_MARGIN
    } else {
        Duration::from_secs(5)
    }
}

const fn accepts_escape_press(pressed: bool, repeat: bool, escape: bool) -> bool {
    pressed && !repeat && escape
}

struct CoalescingSender<T> {
    slot: Arc<Mutex<Option<T>>>,
    wake: SyncSender<()>,
}

pub(super) struct CoalescingReceiver<T> {
    slot: Arc<Mutex<Option<T>>>,
    wake: Receiver<()>,
}

fn coalescing_channel<T>() -> (CoalescingSender<T>, CoalescingReceiver<T>) {
    let slot = Arc::new(Mutex::new(None));
    let (wake, receiver) = mpsc::sync_channel(1);
    (
        CoalescingSender {
            slot: Arc::clone(&slot),
            wake,
        },
        CoalescingReceiver {
            slot,
            wake: receiver,
        },
    )
}

impl<T> CoalescingSender<T> {
    fn send(&self, value: T) -> Result<(), ()> {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err(()),
        }
    }
}

#[derive(Debug)]
struct Correlated<T> {
    generation: ResizeGeneration,
    value: T,
}

struct RecreationCoordinator<T> {
    pending: Option<ResizeGeneration>,
    ready: Option<Correlated<T>>,
}

impl<T> RecreationCoordinator<T> {
    const fn new() -> Self {
        Self {
            pending: None,
            ready: None,
        }
    }

    fn request(&mut self, generation: ResizeGeneration) {
        if self.pending.is_none_or(|pending| generation > pending) {
            self.pending = Some(generation);
        }
        if self
            .ready
            .as_ref()
            .is_some_and(|ready| Some(ready.generation) != self.pending)
        {
            self.ready = None;
        }
    }

    fn pending(&self) -> Option<ResizeGeneration> {
        self.pending
    }

    fn prepare(&mut self, create: impl FnOnce(ResizeGeneration) -> T) {
        if self.ready.is_none()
            && let Some(generation) = self.pending
        {
            self.ready = Some(Correlated {
                generation,
                value: create(generation),
            });
        }
    }

    fn take_ready(&mut self) -> Option<Correlated<T>> {
        self.ready.take()
    }

    fn pressure(&mut self, response: Correlated<T>) {
        if self.pending == Some(response.generation) {
            self.ready = Some(response);
        }
    }

    fn sent(&mut self, generation: ResizeGeneration) {
        if self.pending == Some(generation) {
            self.pending = None;
        }
    }
}

impl<T> CoalescingReceiver<T> {
    pub(super) fn take_latest(&self) -> Option<T> {
        match self.wake.try_recv() {
            Ok(()) => self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

pub(super) struct ProgramWorkerChannels {
    sizes: CoalescingReceiver<(u32, u32)>,
    replacements: Receiver<Correlated<Result<NativeSurface, String>>>,
    recreate: CoalescingSender<ResizeGeneration>,
    wake: winit::event_loop::EventLoopProxy<UserEvent>,
    shutdown: ShutdownSignal,
}

impl ProgramWorkerChannels {
    fn request_recreation(&self, generation: ResizeGeneration) -> Result<(), String> {
        self.recreate
            .send(generation)
            .map_err(|()| "Program surface recreation channel is unavailable".to_owned())?;
        self.wake
            .send_event(UserEvent::Wake)
            .map_err(|_| "Program surface event loop is unavailable".to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementDisposition {
    Install,
    Discard,
    DiscardAndRetry(ResizeGeneration),
}

fn replacement_disposition(
    state: PresentationState,
    response: ResizeGeneration,
) -> ReplacementDisposition {
    match state {
        PresentationState::Recreate {
            resize_generation, ..
        } if resize_generation == response => ReplacementDisposition::Install,
        PresentationState::Recreate {
            resize_generation, ..
        } if resize_generation > response => {
            ReplacementDisposition::DiscardAndRetry(resize_generation)
        }
        _ => ReplacementDisposition::Discard,
    }
}

/// One display-output slot's worker-side surface and channels.
pub(super) struct DisplayPresentationSetup {
    surface: NativeSurface,
    channels: ProgramWorkerChannels,
}

/// The event loop's side of live display-output configuration: slot edits
/// queued by the worker and what the event loop last observed.
#[derive(Default)]
struct WindowedDisplayState {
    pending: Vec<(DisplayOutputSlot, Option<DisplayOutputConfig>)>,
    report: DisplayReport,
}

/// The worker's handle on the windowed presenter: queues live slot edits
/// and reads the latest [`DisplayReport`].
pub(super) struct WindowedDisplays {
    state: Arc<Mutex<WindowedDisplayState>>,
    wake: winit::event_loop::EventLoopProxy<UserEvent>,
}

impl WindowedDisplays {
    /// Queues one slot edit for the event loop and wakes it.
    pub(super) fn configure(&self, slot: DisplayOutputSlot, config: Option<DisplayOutputConfig>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .push((slot, config));
        let _ = self.wake.send_event(UserEvent::Wake);
    }

    /// Whether the event loop last saw `slot` active on its display.
    pub(super) fn active(&self, slot: DisplayOutputSlot) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report
            .states
            .get(&slot)
            .is_some_and(|state| state.active().is_some())
    }

    /// What the event loop last observed.
    pub(super) fn report(&self) -> DisplayReport {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .report
            .clone()
    }
}

pub(super) struct ProgramPresentation {
    surface: Option<NativeSurface>,
    transform: Option<NativeSdrOutputTransform>,
    lifecycle: PresentationLifecycle,
    frame_generation: u64,
    channels: ProgramWorkerChannels,
    /// The slot's source and scaling, or `None` while the slot is empty.
    config: Option<(DisplayOutputSource, OutputScaling)>,
}

impl ProgramPresentation {
    pub(super) fn new(setup: DisplayPresentationSetup) -> Self {
        Self {
            surface: Some(setup.surface),
            transform: None,
            lifecycle: PresentationLifecycle::new(),
            frame_generation: 0,
            channels: setup.channels,
            config: None,
        }
    }

    /// The picture this slot shows, or `None` while it is empty.
    pub(super) fn source(&self) -> Option<DisplayOutputSource> {
        self.config.map(|(source, _)| source)
    }

    /// Takes a slot edit's source and scaling at once; the event loop moves
    /// the window when the display changes.
    pub(super) fn set_config(&mut self, config: Option<&DisplayOutputConfig>) {
        self.config = config.map(|config| (config.source, config.scaling));
    }

    pub(super) fn shutdown_requested(&self) -> bool {
        self.channels.shutdown.requested()
    }

    pub(super) fn service_control(&mut self, context: &NativeContext) -> Result<(), String> {
        if let Some((width, height)) = self.channels.sizes.take_latest() {
            let action = self.lifecycle.resize(width, height);
            self.apply_configuration_action(context, action)?;
        }
        match self.channels.replacements.try_recv() {
            Ok(response) => {
                match replacement_disposition(self.lifecycle.state(), response.generation) {
                    ReplacementDisposition::Install => match response.value {
                        Ok(surface) => {
                            self.surface = Some(surface);
                            let action =
                                self.lifecycle.finish_recreation(response.generation, true);
                            self.apply_configuration_action(context, action)?;
                        }
                        Err(error) => {
                            let _ = self.lifecycle.finish_recreation(response.generation, false);
                            return Err(error);
                        }
                    },
                    ReplacementDisposition::Discard => {}
                    ReplacementDisposition::DiscardAndRetry(generation) => {
                        self.channels.request_recreation(generation)?;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                return Err("Program surface replacement channel disconnected".to_owned());
            }
        }
        Ok(())
    }

    pub(super) fn present_latest(
        &mut self,
        context: &NativeContext,
        texture: &NativeTexture,
    ) -> Result<(), String> {
        self.service_control(context)?;
        let Some(resize_generation) = self.lifecycle.resize_generation() else {
            return Ok(());
        };
        self.frame_generation = self
            .frame_generation
            .checked_add(1)
            .ok_or_else(|| "Program frame generation exhausted".to_owned())?;
        let frame = PresentationFrame::new(
            FrameGeneration::new(self.frame_generation),
            resize_generation,
        );
        if matches!(
            self.lifecycle.submit_frame(frame),
            FrameDecision::Rejected(_)
        ) {
            return Ok(());
        }

        let Some(surface) = self.surface.as_mut() else {
            return Ok(());
        };
        let Some((_, scaling)) = self.config else {
            return Ok(());
        };
        let follow_up =
            match block_on(surface.acquire(context)).map_err(|error| surface_error(&error))? {
                NativeSurfaceAcquire::Success(surface_frame) => {
                    let action = self
                        .lifecycle
                        .handle_acquisition(SurfaceAcquisition::Success);
                    submit_acquired(
                        context,
                        &mut self.lifecycle,
                        self.transform.as_ref(),
                        texture,
                        scaling,
                        surface_frame,
                        action,
                    )?;
                    None
                }
                NativeSurfaceAcquire::Suboptimal(surface_frame) => {
                    let action = self
                        .lifecycle
                        .handle_acquisition(SurfaceAcquisition::Suboptimal);
                    submit_acquired(
                        context,
                        &mut self.lifecycle,
                        self.transform.as_ref(),
                        texture,
                        scaling,
                        surface_frame,
                        action,
                    )?;
                    Some(action)
                }
                NativeSurfaceAcquire::Timeout | NativeSurfaceAcquire::Occluded => {
                    let _ = self
                        .lifecycle
                        .handle_acquisition(SurfaceAcquisition::Timeout);
                    None
                }
                NativeSurfaceAcquire::Outdated => {
                    let action = self
                        .lifecycle
                        .handle_acquisition(SurfaceAcquisition::Outdated);
                    Some(action)
                }
                NativeSurfaceAcquire::Lost => {
                    let action = self.lifecycle.handle_acquisition(SurfaceAcquisition::Lost);
                    Some(action)
                }
                NativeSurfaceAcquire::Validation(detail) => {
                    return Err(surface_validation_error(&detail));
                }
            };
        if let Some(action) = follow_up {
            self.apply_configuration_action(context, action)?;
        }
        Ok(())
    }

    pub(super) fn telemetry(&self) -> fm_gpu::PresentationTelemetry {
        self.lifecycle.telemetry()
    }

    pub(super) fn telemetry_for_shutdown(&mut self) -> fm_gpu::PresentationTelemetry {
        let _ = self.lifecycle.resize(0, 0);
        self.lifecycle.telemetry()
    }

    fn apply_configuration_action(
        &mut self,
        context: &NativeContext,
        action: PresentationAction,
    ) -> Result<(), String> {
        let (extent, generation) = match action {
            PresentationAction::Configure {
                extent,
                resize_generation,
            }
            | PresentationAction::Reconfigure {
                extent,
                resize_generation,
            }
            | PresentationAction::PresentAndReconfigure {
                extent,
                resize_generation,
                ..
            } => (extent, resize_generation),
            PresentationAction::Recreate {
                resize_generation, ..
            } => {
                self.surface.take();
                self.transform.take();
                return self.channels.request_recreation(resize_generation);
            }
            PresentationAction::Fail(reason) => {
                return Err(format!("Program presentation failed: {reason:?}"));
            }
            _ => return Ok(()),
        };
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| "Program surface is unavailable during configuration".to_owned())?;
        let configuration = surface
            .select_opaque_sdr_configuration(extent.width().get(), extent.height().get())
            .map_err(|error| surface_error(&error))?;
        if self.transform.is_none() {
            self.transform = Some(
                block_on(NativeSdrOutputTransform::new_for_format(
                    context,
                    configuration.format,
                ))
                .map_err(|error| format!("Program output transform failed: {error}"))?,
            );
        }
        match block_on(surface.configure(context, configuration)) {
            Ok(()) => {
                let _ = self.lifecycle.finish_configuration(generation, true);
                Ok(())
            }
            Err(error) => {
                let result = self.lifecycle.finish_configuration(generation, false);
                Err(format!(
                    "Program surface configuration failed ({result:?}): {error}"
                ))
            }
        }
    }
}

fn submit_acquired(
    context: &NativeContext,
    lifecycle: &mut PresentationLifecycle,
    transform: Option<&NativeSdrOutputTransform>,
    texture: &NativeTexture,
    scaling: OutputScaling,
    surface_frame: fm_gpu::NativeSurfaceFrame<'_>,
    action: PresentationAction,
) -> Result<(), String> {
    let (PresentationAction::Present(presented)
    | PresentationAction::PresentAndReconfigure {
        frame: presented, ..
    }) = action
    else {
        return Ok(());
    };
    let transform =
        transform.ok_or_else(|| "Program output transform is not configured".to_owned())?;
    let submitted = match block_on(transform.transform_to_surface_scaled(
        context,
        texture,
        surface_frame,
        scaling,
    )) {
        Ok(submitted) => submitted,
        Err(error) => {
            let _ = lifecycle.finish_present(presented, false);
            return Err(surface_error(&error));
        }
    };
    if let Err(error) = block_on(context.present(submitted)) {
        let _ = lifecycle.finish_present(presented, false);
        return Err(surface_error(&error));
    }
    let _ = lifecycle.finish_present(presented, true);
    Ok(())
}

fn surface_error(error: &NativeGpuError) -> String {
    format!("Program surface GPU failure: {error}")
}

fn surface_validation_error(detail: &str) -> String {
    format!("fatal Program surface validation failure: {detail}")
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

#[derive(Debug)]
enum UserEvent {
    Wake,
    Finished(Result<(), String>),
}

type SurfaceReplacement = Correlated<Result<NativeSurface, String>>;

/// The event-loop side of one display output: its borderless window, the
/// monitor it is attached to, and the channels to its worker presentation.
struct OutputWindow {
    window: Option<Arc<winit::window::Window>>,
    /// The attached monitor, or `None` while the output waits for a display.
    target: Option<winit::monitor::MonitorHandle>,
    /// The exact video mode to drive, when it differs from the current mode.
    exclusive: Option<winit::monitor::VideoModeHandle>,
    size_sender: CoalescingSender<(u32, u32)>,
    size_receiver: Option<CoalescingReceiver<(u32, u32)>>,
    replacement_sender: SyncSender<SurfaceReplacement>,
    replacement_receiver: Option<Receiver<SurfaceReplacement>>,
    recreate_sender: Option<CoalescingSender<ResizeGeneration>>,
    recreate_receiver: CoalescingReceiver<ResizeGeneration>,
    recreations: RecreationCoordinator<Result<NativeSurface, String>>,
}

impl OutputWindow {
    fn new() -> Self {
        let (size_sender, size_receiver) = coalescing_channel();
        let (replacement_sender, replacement_receiver) = mpsc::sync_channel(1);
        let (recreate_sender, recreate_receiver) = coalescing_channel();
        Self {
            window: None,
            target: None,
            exclusive: None,
            size_sender,
            size_receiver: Some(size_receiver),
            replacement_sender,
            replacement_receiver: Some(replacement_receiver),
            recreate_sender: Some(recreate_sender),
            recreate_receiver,
            recreations: RecreationCoordinator::new(),
        }
    }

    fn fullscreen(&self) -> Option<winit::window::Fullscreen> {
        let target = self.target.as_ref()?;
        Some(self.exclusive.as_ref().map_or_else(
            || winit::window::Fullscreen::Borderless(Some(target.clone())),
            |mode| winit::window::Fullscreen::Exclusive(mode.clone()),
        ))
    }

    fn suspend(&self) {
        if let Some(window) = &self.window {
            window.set_visible(false);
        }
        let _ = self.size_sender.send((0, 0));
    }

    /// Shows the window only once it is fullscreen on its attached monitor;
    /// otherwise hides it and (re)requests fullscreen on that monitor.
    fn synchronize(&self) {
        let Some(window) = &self.window else {
            return;
        };
        if self.target.is_none() {
            self.suspend();
            return;
        }
        if !accepts_resize(self.target.as_ref(), window.current_monitor().as_ref()) {
            window.set_visible(false);
            let _ = self.size_sender.send((0, 0));
            window.set_fullscreen(self.fullscreen());
            return;
        }
        window.set_visible(true);
        window.set_cursor_visible(false);
        let size = window.inner_size();
        let _ = self.size_sender.send((size.width, size.height));
    }

    fn handle_resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        let Some(window) = &self.window else {
            return;
        };
        if accepts_resize(self.target.as_ref(), window.current_monitor().as_ref()) {
            window.set_visible(true);
            let _ = self.size_sender.send((size.width, size.height));
        } else {
            self.suspend();
        }
    }

    fn receive_recreation_request(&mut self) {
        if let Some(generation) = self.recreate_receiver.take_latest() {
            self.recreations.request(generation);
        }
    }

    fn recreate_if_pending(&mut self, factory: &NativeSurfaceFactory) -> Result<(), String> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        if self.target.is_none() || self.recreations.pending().is_none() {
            return Ok(());
        }
        self.recreations.prepare(|_| {
            factory
                .create_surface(Arc::clone(window))
                .map_err(|error| format!("Program surface recreation failed: {error}"))
        });
        let Some(response) = self.recreations.take_ready() else {
            return Ok(());
        };
        let generation = response.generation;
        match self.replacement_sender.try_send(response) {
            Ok(()) => self.recreations.sent(generation),
            Err(TrySendError::Full(response)) => self.recreations.pressure(response),
            Err(TrySendError::Disconnected(_)) => {
                return Err("Program surface worker disconnected".to_owned());
            }
        }
        Ok(())
    }
}

/// A window accepts its size only while attached and fullscreen on the
/// attached monitor; a window the platform re-homed elsewhere is suspended.
fn accepts_resize<M: PartialEq>(target: Option<&M>, current: Option<&M>) -> bool {
    target.is_some() && target == current
}

/// Daemon options handed to the worker unchanged.
pub(super) struct ProgramWorkerOptions {
    pub(super) project: PathBuf,
    pub(super) listen: SocketAddr,
    /// The web gateway (RC-007), served exactly as by the headless daemon.
    pub(super) web_listen: Option<SocketAddr>,
    pub(super) web: super::web::WebSettings,
    pub(super) once: bool,
    /// `--display-output` slots replacing the project's on load.
    pub(super) display_overrides: Vec<DisplayOutputConfig>,
    pub(super) camera_helper: Option<PathBuf>,
    pub(super) record_program: Option<PathBuf>,
    pub(super) record_output: Option<fm_types::OutputId>,
    pub(super) record_program_2: Option<PathBuf>,
    pub(super) record_output_2: Option<fm_types::OutputId>,
    pub(super) record_wav: bool,
    pub(super) record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    pub(super) record_bitrate_kbps: Option<u32>,
    pub(super) record_bitrate_kbps_2: Option<u32>,
    pub(super) record_audio_bitrate_kbps: Option<u32>,
    pub(super) record_audio_bitrate_kbps_2: Option<u32>,
}

struct ProgramApp {
    options: Option<ProgramWorkerOptions>,
    tracker: DisplayOutputTracker,
    displays: Arc<Mutex<WindowedDisplayState>>,
    outputs: Vec<OutputWindow>,
    factory: Option<NativeSurfaceFactory>,
    started: bool,
    shutdown: ShutdownSignal,
    shutdown_timeout: Duration,
    recorder_requested: bool,
    shutdown_deadline: Option<Instant>,
    worker: Option<thread::JoinHandle<()>>,
    result: Option<Result<(), String>>,
    shutdown_timed_out: bool,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
}

impl ProgramApp {
    fn new(
        options: ProgramWorkerOptions,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) -> Self {
        let recorder_requested =
            options.record_program.is_some() || options.record_program_2.is_some();
        let shutdown_timeout = program_shutdown_timeout(recorder_requested);
        let outputs = (0..MAX_DISPLAY_OUTPUTS)
            .map(|_| OutputWindow::new())
            .collect();
        Self {
            options: Some(options),
            // The presenter is SDR-only, so an HDR request reports why it
            // presents SDR instead of claiming HDR. The worker fills the
            // slots from the project once it has loaded it.
            tracker: DisplayOutputTracker::empty(false),
            displays: Arc::new(Mutex::new(WindowedDisplayState::default())),
            outputs,
            factory: None,
            started: false,
            shutdown: ShutdownSignal::new(),
            shutdown_timeout,
            recorder_requested,
            shutdown_deadline: None,
            worker: None,
            result: None,
            shutdown_timed_out: false,
            proxy,
        }
    }

    fn fail(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, error: impl Into<String>) {
        self.result = Some(Err(error.into()));
        let _ = self.shutdown.request();
        event_loop.exit();
    }

    fn request_shutdown(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if !begin_shutdown(
            &self.shutdown,
            &mut self.shutdown_deadline,
            Instant::now() + self.shutdown_timeout,
        ) {
            return;
        }
        for output in &self.outputs {
            if let Some(window) = &output.window {
                window.set_visible(false);
            }
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(20),
        ));
    }

    /// Applies the worker's queued slot edits: an emptied or rebinding
    /// slot hides its window until the next reconcile attaches it.
    fn apply_pending_configuration(&mut self) {
        let pending = std::mem::take(
            &mut self
                .displays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending,
        );
        for (slot, config) in pending {
            let index = slot.index();
            match self.tracker.configure(index, config) {
                DisplayOutputReconfiguration::Unchanged
                | DisplayOutputReconfiguration::Retargeted => {}
                DisplayOutputReconfiguration::Rebinding => {
                    if let (Some(config), Some(state)) =
                        (self.tracker.config(index), self.tracker.state(index))
                    {
                        eprintln!("{}", status_record(index, config, state));
                    }
                    self.detach(index);
                }
                DisplayOutputReconfiguration::Cleared => {
                    eprintln!("{}", cleared_record(index));
                    self.detach(index);
                }
            }
        }
    }

    fn detach(&mut self, index: usize) {
        let Some(output) = self.outputs.get_mut(index) else {
            return;
        };
        output.target = None;
        output.exclusive = None;
        output.suspend();
    }

    fn poll_monitors(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.apply_pending_configuration();
        let monitors = enumerate_monitors(event_loop);
        let inventory = monitors
            .iter()
            .map(|(info, _)| info.clone())
            .collect::<Vec<_>>();
        for change in self.tracker.reconcile(&inventory) {
            if let Some(config) = self.tracker.config(change.output) {
                eprintln!("{}", status_record(change.output, config, &change.current));
            }
            let output = &mut self.outputs[change.output];
            let attached = change.current.active().and_then(|active| {
                monitors
                    .iter()
                    .find(|(info, _)| info.id == active.id)
                    .map(|(info, handle)| (active, info, handle))
            });
            let Some((active, info, handle)) = attached else {
                output.target = None;
                output.exclusive = None;
                output.suspend();
                continue;
            };
            output.target = Some(handle.clone());
            output.exclusive = if active.mode == info.current_mode {
                None
            } else {
                exclusive_mode(handle, active.mode)
            };
            if let Some(window) = &output.window {
                window.set_visible(false);
                let _ = output.size_sender.send((0, 0));
                window.set_fullscreen(output.fullscreen());
            }
        }
        for output in &self.outputs {
            output.synchronize();
        }
        self.recreate_if_pending();
        let states = DisplayOutputSlot::ALL
            .into_iter()
            .filter_map(|slot| {
                self.tracker
                    .state(slot.index())
                    .map(|state| (slot, state.clone()))
            })
            .collect();
        let mut displays = self
            .displays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        displays.report = DisplayReport {
            states,
            displays: inventory,
            error: None,
        };
    }

    fn receive_recreation_requests(&mut self) {
        for output in &mut self.outputs {
            output.receive_recreation_request();
        }
    }

    fn recreate_if_pending(&mut self) {
        let Some(factory) = &self.factory else {
            return;
        };
        for output in &mut self.outputs {
            if let Err(error) = output.recreate_if_pending(factory) {
                self.result = Some(Err(error));
            }
        }
    }

    #[allow(clippy::expect_used)] // channels are taken exactly once when the worker starts
    fn start(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) -> Result<(), String> {
        let mut windows = Vec::with_capacity(self.outputs.len());
        for index in 0..self.outputs.len() {
            let attributes = winit::window::Window::default_attributes()
                .with_title(format!("FreeMix Display Output {}", index + 1))
                .with_decorations(false)
                .with_visible(false);
            let window = event_loop
                .create_window(attributes)
                .map_err(|error| format!("Program window creation failed: {error}"))?;
            window.set_cursor_visible(false);
            windows.push(Arc::new(window));
        }
        let first = windows
            .first()
            .ok_or_else(|| "no display outputs are configured".to_owned())?;
        let (context, first_surface) = block_on(NativeContext::new_with_surface(
            [super::platform_native_backend()],
            Arc::clone(first),
        ))
        .map_err(|error| format!("Program GPU setup failed: {error}"))?;
        let factory = context.surface_factory();
        let mut surfaces = vec![first_surface];
        for window in windows.iter().skip(1) {
            surfaces.push(
                factory
                    .create_surface(Arc::clone(window))
                    .map_err(|error| format!("Program surface creation failed: {error}"))?,
            );
        }
        for (output, window) in self.outputs.iter_mut().zip(windows) {
            output.window = Some(window);
        }
        self.factory = Some(factory);
        self.poll_monitors(event_loop);

        let mut setups = Vec::with_capacity(self.outputs.len());
        for (output, surface) in self.outputs.iter_mut().zip(surfaces) {
            setups.push(DisplayPresentationSetup {
                surface,
                channels: ProgramWorkerChannels {
                    sizes: output.size_receiver.take().expect("worker starts once"),
                    replacements: output
                        .replacement_receiver
                        .take()
                        .expect("worker starts once"),
                    recreate: output.recreate_sender.take().expect("worker starts once"),
                    wake: self.proxy.clone(),
                    shutdown: self.shutdown.clone(),
                },
            });
        }
        let options = self.options.take().expect("worker starts once");
        let proxy = self.proxy.clone();
        let displays = WindowedDisplays {
            state: Arc::clone(&self.displays),
            wake: self.proxy.clone(),
        };
        self.worker = Some(
            thread::Builder::new()
                .name("freemixd-program-worker".to_owned())
                .spawn(move || {
                    let result = super::serve_program_worker(
                        &options.project,
                        options.listen,
                        options.web_listen,
                        options.web,
                        options.once,
                        context,
                        setups,
                        displays,
                        options.display_overrides,
                        options.camera_helper,
                        options.record_program,
                        options.record_output,
                        options.record_program_2,
                        options.record_output_2,
                        options.record_wav,
                        options.record_segment_seconds,
                        options.record_bitrate_kbps,
                        options.record_bitrate_kbps_2,
                        options.record_audio_bitrate_kbps,
                        options.record_audio_bitrate_kbps_2,
                    )
                    .map_err(|error| error.to_string());
                    let _ = proxy.send_event(UserEvent::Finished(result));
                })
                .map_err(|error| format!("Program worker thread creation failed: {error}"))?,
        );
        Ok(())
    }
}

impl winit::application::ApplicationHandler<UserEvent> for ProgramApp {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        if let Err(error) = self.start(event_loop) {
            self.fail(event_loop, error);
            return;
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(500),
        ));
    }

    fn user_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                self.receive_recreation_requests();
                self.poll_monitors(event_loop);
            }
            UserEvent::Finished(result) => {
                self.result = Some(result);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let Some(index) = self.outputs.iter().position(|output| {
            output
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
        }) else {
            return;
        };
        match event {
            winit::event::WindowEvent::CloseRequested => self.request_shutdown(event_loop),
            winit::event::WindowEvent::KeyboardInput { event, .. }
                if accepts_escape_press(
                    event.state == winit::event::ElementState::Pressed,
                    event.repeat,
                    event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                ) =>
            {
                self.request_shutdown(event_loop);
            }
            winit::event::WindowEvent::Resized(size) => {
                self.poll_monitors(event_loop);
                self.outputs[index].handle_resize(size);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.shutdown.requested() {
            if self
                .shutdown_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.shutdown_timed_out = true;
                self.result = Some(Err(if self.recorder_requested {
                    format!(
                        "forced Program shutdown after {} seconds; checkpoint and recorder cleanup completion were not confirmed",
                        self.shutdown_timeout.as_secs()
                    )
                } else {
                    "forced Program shutdown after 5 seconds; checkpoint completion was not confirmed"
                        .to_owned()
                }));
                event_loop.exit();
                return;
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(20),
            ));
            return;
        }
        self.receive_recreation_requests();
        self.poll_monitors(event_loop);
        if self.result.is_some() {
            event_loop.exit();
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(500),
            ));
        }
    }
}

/// Finds the monitor video mode matching `mode`, for exclusive fullscreen.
fn exclusive_mode(
    monitor: &winit::monitor::MonitorHandle,
    mode: DisplayMode,
) -> Option<winit::monitor::VideoModeHandle> {
    monitor
        .video_modes()
        .find(|video| video_display_mode(video).matches(mode))
}

fn video_display_mode(video: &winit::monitor::VideoModeHandle) -> DisplayMode {
    let size = video.size();
    DisplayMode::new(size.width, size.height, video.refresh_rate_millihertz())
}

/// Enumerates every connected monitor as a portable [`DisplayInfo`] with a
/// platform display id. winit does not expose HDR capability, so every
/// display reports [`fm_io_api::display::HdrCapability::Unknown`].
fn enumerate_monitors(
    event_loop: &winit::event_loop::ActiveEventLoop,
) -> Vec<(DisplayInfo, winit::monitor::MonitorHandle)> {
    event_loop
        .available_monitors()
        .filter_map(|monitor| {
            let size = monitor.size();
            let position = monitor.position();
            let current = DisplayMode::new(
                size.width,
                size.height,
                monitor.refresh_rate_millihertz().unwrap_or_default(),
            );
            let name = monitor.name().unwrap_or_default();
            let id = DisplayId::new(sanitize_display_text(&platform_display_id(
                &monitor, &name, position.x, position.y,
            )))
            .ok()?;
            let name = if name.is_empty() {
                id.to_string()
            } else {
                name
            };
            let mut info = DisplayInfo::new(id, name, current);
            info.x = position.x;
            info.y = position.y;
            let mut modes = monitor
                .video_modes()
                .map(|video| video_display_mode(&video))
                .collect::<Vec<_>>();
            modes.push(current);
            modes.sort_unstable();
            modes.dedup();
            info.modes = modes;
            Some((info, monitor))
        })
        .collect()
}

fn sanitize_display_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() || c == ',' { '_' } else { c })
        .collect()
}

/// The most stable identity the platform exposes for a monitor:
/// `CGDirectDisplayID` on macOS, the GDI device name on Windows, the `RandR`
/// output or Wayland output id on Linux, and the desktop placement elsewhere.
#[cfg(target_os = "macos")]
fn platform_display_id(
    monitor: &winit::monitor::MonitorHandle,
    _name: &str,
    _x: i32,
    _y: i32,
) -> String {
    use winit::platform::macos::MonitorHandleExtMacOS;
    format!("cg:{}", monitor.native_id())
}

#[cfg(target_os = "windows")]
fn platform_display_id(
    monitor: &winit::monitor::MonitorHandle,
    _name: &str,
    _x: i32,
    _y: i32,
) -> String {
    use winit::platform::windows::MonitorHandleExtWindows;
    format!("win:{}", monitor.native_id())
}

#[cfg(target_os = "linux")]
fn platform_display_id(
    monitor: &winit::monitor::MonitorHandle,
    _name: &str,
    _x: i32,
    _y: i32,
) -> String {
    use winit::platform::x11::MonitorHandleExtX11;
    format!("output:{}", monitor.native_id())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_display_id(
    _monitor: &winit::monitor::MonitorHandle,
    name: &str,
    x: i32,
    y: i32,
) -> String {
    format!("desktop:{name}@{x}x{y}")
}

/// Runs the windowed presenter's event loop on this (main) thread and the
/// daemon on its worker until both stop.
///
/// # Errors
///
/// A window system, window, or GPU that is unavailable before the worker
/// starts serving fails with [`super::PresenterUnavailable`], so the caller
/// may serve headless instead; later failures are returned as they are.
pub(super) fn run(options: ProgramWorkerOptions) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|error| super::PresenterUnavailable(error.to_string()))?;
    let proxy = event_loop.create_proxy();
    let mut app = ProgramApp::new(options, proxy);
    let ran = event_loop.run_app(&mut app);
    if app.worker.is_none() {
        // The worker never started: nothing was loaded or served yet.
        let error = match (ran, app.result.take()) {
            (Err(error), _) => error.to_string(),
            (Ok(()), Some(Err(error))) => error,
            (Ok(()), _) => "the display event loop exited before it started".to_owned(),
        };
        return Err(super::PresenterUnavailable(error).into());
    }
    ran?;
    if !app.shutdown_timed_out
        && let Some(worker) = app.worker.take()
    {
        worker
            .join()
            .map_err(|_| "Program worker panicked".to_owned())?;
    }
    app.result
        .unwrap_or_else(|| Err("Program event loop exited without a worker result".to_owned()))
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescing_channel_keeps_only_latest_value() {
        let (sender, receiver) = coalescing_channel();
        sender.send(1).unwrap();
        sender.send(2).unwrap();
        sender.send(3).unwrap();
        assert_eq!(receiver.take_latest(), Some(3));
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn recreation_requests_coalesce_to_latest_resize_generation() {
        let (sender, receiver) = coalescing_channel();
        sender.send(ResizeGeneration::new(7)).unwrap();
        sender.send(ResizeGeneration::new(8)).unwrap();
        assert_eq!(receiver.take_latest(), Some(ResizeGeneration::new(8)));
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn shutdown_signal_is_shared_and_idempotent() {
        let shutdown = ShutdownSignal::new();
        let worker = shutdown.clone();
        assert!(!worker.requested());
        assert!(shutdown.request());
        assert!(!shutdown.request());
        assert!(worker.requested());
    }

    #[test]
    fn shutdown_deadline_is_set_once_and_repeated_escape_is_ignored() {
        let shutdown = ShutdownSignal::new();
        let mut deadline = None;
        assert!(begin_shutdown(&shutdown, &mut deadline, 100));
        assert!(!begin_shutdown(&shutdown, &mut deadline, 200));
        assert_eq!(deadline, Some(100));
        assert!(accepts_escape_press(true, false, true));
        assert!(!accepts_escape_press(true, true, true));
    }

    #[test]
    fn recorder_shutdown_deadline_covers_stop_kill_and_checkpoint_margin() {
        assert_eq!(program_shutdown_timeout(false), Duration::from_secs(5));
        assert_eq!(
            program_shutdown_timeout(true),
            super::super::PROGRAM_RECORDER_STOP_TIMEOUT
                + super::super::PROGRAM_RECORDER_KILL_TIMEOUT
                + super::super::PROGRAM_CHECKPOINT_MARGIN
        );
        assert!(program_shutdown_timeout(true) > program_shutdown_timeout(false));
    }

    #[test]
    fn resize_is_rejected_when_detached_or_window_is_rehomed() {
        assert!(accepts_resize(Some(&20), Some(&20)));
        assert!(!accepts_resize(Some(&20), Some(&10)));
        assert!(!accepts_resize(Some(&20), None));
        assert!(!accepts_resize::<u32>(None, None));
        assert!(!accepts_resize(None, Some(&20)));
    }

    #[test]
    fn display_ids_are_sanitized_for_records_and_selectors() {
        assert_eq!(sanitize_display_text("a,b\tc"), "a_b_c");
        assert!(DisplayId::new(sanitize_display_text("\\\\.\\DISPLAY1")).is_ok());
    }

    #[test]
    fn stale_recreation_response_is_discarded_and_new_generation_retried() {
        let mut lifecycle = PresentationLifecycle::new();
        let PresentationAction::Configure {
            resize_generation: first,
            ..
        } = lifecycle.resize(1920, 1080)
        else {
            unreachable!()
        };
        lifecycle.finish_configuration(first, true);
        assert!(matches!(
            lifecycle.handle_acquisition(SurfaceAcquisition::Lost),
            PresentationAction::Recreate { .. }
        ));
        let PresentationAction::Recreate {
            resize_generation: second,
            ..
        } = lifecycle.resize(1280, 720)
        else {
            unreachable!()
        };
        assert!(second > first);
        assert_eq!(
            replacement_disposition(lifecycle.state(), first),
            ReplacementDisposition::DiscardAndRetry(second)
        );
        assert_eq!(
            replacement_disposition(lifecycle.state(), second),
            ReplacementDisposition::Install
        );
    }

    #[test]
    fn response_pressure_preserves_and_retries_newer_generation() {
        let first = ResizeGeneration::new(1);
        let second = ResizeGeneration::new(2);
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut coordinator = RecreationCoordinator::new();

        coordinator.request(first);
        coordinator.prepare(ResizeGeneration::get);
        let response = coordinator.take_ready().unwrap();
        sender.try_send(response).unwrap();
        coordinator.sent(first);

        coordinator.request(second);
        coordinator.prepare(ResizeGeneration::get);
        let response = coordinator.take_ready().unwrap();
        let TrySendError::Full(response) = sender.try_send(response).unwrap_err() else {
            unreachable!()
        };
        coordinator.pressure(response);
        assert_eq!(coordinator.pending(), Some(second));

        assert_eq!(receiver.try_recv().unwrap().generation, first);
        let response = coordinator.take_ready().unwrap();
        assert_eq!(response.generation, second);
        sender.try_send(response).unwrap();
        coordinator.sent(second);
        assert_eq!(coordinator.pending(), None);
        assert_eq!(receiver.try_recv().unwrap().generation, second);
    }

    #[test]
    fn surface_validation_failure_does_not_record_out_of_memory() {
        let lifecycle = PresentationLifecycle::new();
        let before = lifecycle.telemetry().out_of_memory_failures;
        assert_eq!(
            surface_validation_error("injected"),
            "fatal Program surface validation failure: injected"
        );
        assert_eq!(lifecycle.telemetry().out_of_memory_failures, before);
    }

    #[test]
    fn acquisition_policy_classifies_retry_reconfigure_recreate_and_fatal() {
        let mut lifecycle = PresentationLifecycle::new();
        let action = lifecycle.resize(1920, 1080);
        let PresentationAction::Configure {
            resize_generation, ..
        } = action
        else {
            panic!("expected configure");
        };
        assert_eq!(
            lifecycle.finish_configuration(resize_generation, true),
            PresentationAction::None
        );
        assert_eq!(
            lifecycle.handle_acquisition(SurfaceAcquisition::Timeout),
            PresentationAction::Retry
        );
        assert!(matches!(
            lifecycle.handle_acquisition(SurfaceAcquisition::Outdated),
            PresentationAction::Reconfigure { .. }
        ));
        assert_eq!(lifecycle.telemetry().timeouts, 1);
        assert_eq!(lifecycle.telemetry().outdated_acquisitions, 1);

        let mut lost = PresentationLifecycle::new();
        let PresentationAction::Configure {
            resize_generation, ..
        } = lost.resize(1, 1)
        else {
            unreachable!()
        };
        lost.finish_configuration(resize_generation, true);
        assert!(matches!(
            lost.handle_acquisition(SurfaceAcquisition::Lost),
            PresentationAction::Recreate { .. }
        ));
        assert_eq!(lost.telemetry().surface_losses, 1);
    }

    #[test]
    fn lifecycle_replaces_latest_frame_and_acknowledges_the_presented_generation() {
        let mut lifecycle = PresentationLifecycle::new();
        let PresentationAction::Configure {
            resize_generation, ..
        } = lifecycle.resize(1920, 1080)
        else {
            unreachable!()
        };
        lifecycle.finish_configuration(resize_generation, true);
        let first = PresentationFrame::new(FrameGeneration::new(1), resize_generation);
        let latest = PresentationFrame::new(FrameGeneration::new(2), resize_generation);
        assert_eq!(lifecycle.submit_frame(first), FrameDecision::Queued);
        assert_eq!(
            lifecycle.submit_frame(latest),
            FrameDecision::Replaced { dropped: first }
        );
        assert_eq!(
            lifecycle.handle_acquisition(SurfaceAcquisition::Success),
            PresentationAction::Present(latest)
        );
        assert_eq!(
            lifecycle.finish_present(latest, true),
            PresentationAction::None
        );
        let telemetry = lifecycle.telemetry();
        assert_eq!(telemetry.frames_replaced, 1);
        assert_eq!(telemetry.frames_dropped, 1);
        assert_eq!(telemetry.frames_presented, 1);
    }
}
