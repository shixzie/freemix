//! Native realization of screen and window capture inputs (IN-011).
//!
//! A native daemon owns one [`NativeScreenCapture`]: a desktop watch
//! thread that lists the screens and windows of the desktop the daemon runs
//! on every [`LIST_INTERVAL`] (the X11 display of `DISPLAY` on Linux, see
//! [`crate::desktop_capture`] for every platform), and, for each open
//! screen capture input whose target the latest listing holds, one
//! [`NetworkReceiver`] reading the platform's capture device (`ffmpeg -f
//! x11grab` on Linux) at the program size, letterboxed, and rate. The
//! session loop never waits on the desktop: once per engine frame it pulls
//! each receiver ([`NativeScreenCapture::pull`]) and ingests a new picture
//! as the input's live source frame.
//!
//! The target is resolved again on every listing. A window is followed by
//! its rule (its id while it lives, otherwise its application and title),
//! so a closed window that opens again is captured again; a window that
//! moves, is resized, or whose crop changes restarts its capture at the
//! new region. While no listed window or screen matches, the input is
//! `target_gone` and shows its slate (the last picture, or opaque black);
//! a capture that stops on its own reopens with backoff. A session that
//! cannot be captured at all (no `DISPLAY`, a Wayland session without
//! `XWayland`, or a platform without a backend) reports `unsupported` with
//! the reason, and a live add of a screen capture input is refused there.
//!
//! A screen capture input's sound ([`ScreenCaptureConfig::audio`]) feeds
//! its own Master strip: none (the strip gets silence), the whole system
//! output, or the sound of the application owning the captured window. The
//! sound is captured by the same capture child as the picture (see
//! [`crate::desktop_capture::AudioPlan`]): read directly where `ffmpeg`
//! can (the `PulseAudio`/`PipeWire` default output's monitor), or served by
//! an [`AudioTap`] that gathers the application's streams (or a platform
//! helper's sound) and paces it on the host clock the picture is grabbed
//! on. Picture and sound then share one jitter buffer and the receiver's
//! drift loop (AU-004), whose telemetry is reported as a `screen_capture`
//! live source. The audio's own state (`off`, `opening`, `live`,
//! `no_stream`, `unsupported`, `failed`) travels in the input's status
//! beside the picture's. A tap outlives capture restarts of the same
//! window (a resize reconnects the capture child to the same tap).
//!
//! On a Wayland desktop the inputs are handed to
//! [`crate::native_portal_capture`] instead: nothing is listed, and the
//! desktop's own sharing dialog picks each input's screen or window. A
//! portal capture has no sound yet: its audio is `unsupported` and its
//! Master strip gets silence.
//!
//! A closed input (IN-026) stops its capture and shows a blank picture
//! until it reopens.
//!
//! Each input's status reaches clients as `screen_capture_status` runtime
//! events ([`NativeScreenCapture::statuses`]): at once when its state,
//! captured size, window, or restart count changes, and a changed error
//! text at most once a second. The daemon logs each state change on
//! standard error as `FREEMIXD_SCREEN_CAPTURE` records.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, Picture, PulledFrame};
use fm_frame::{ClockDomainId, SampleRate};
use fm_io_linux::capture_audio::{AudioTap, TapSource, TapState};
use fm_io_linux::screen_capture::{CaptureRegion, DesktopListing, Resolution, capture_settings};
use fm_protocol::{
    ScreenCaptureAudioState, ScreenCaptureConfig, ScreenCaptureState, ScreenCaptureStatus,
    bound_screen_capture_error,
};
use fm_types::network_stream::{NetworkStreamState, NetworkStreamStatus, StallSlate};
use fm_types::{FrameRate, InputId};

use crate::desktop_capture::{AudioPlan, DesktopBackend};
use crate::native_files::{blank_picture, output_timing};
use crate::native_streams::{Format, PulledStream};
use crate::native_sync::{SourceKind, SyncLog, wire_sample};

/// Longest a changed error text waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// How often the desktop is listed while screen capture inputs exist.
pub(crate) const LIST_INTERVAL: Duration = Duration::from_millis(500);

/// One screen capture input a native daemon realizes.
pub(crate) struct ScreenCaptureInput {
    pub(crate) input: InputId,
    pub(crate) config: ScreenCaptureConfig,
    /// The input is closed (IN-026): nothing captures until it reopens.
    pub(crate) closed: bool,
}

/// What a source is doing about its target.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Target {
    /// No listing has been seen yet.
    Unknown,
    /// Capturing this region.
    Captured(CaptureRegion),
    /// The target is not on the desktop, and why.
    Gone(String),
    /// The target can never be captured here, and why.
    Unsupported(String),
}

struct Source {
    config: ScreenCaptureConfig,
    closed: bool,
    target: Target,
    receiver: Option<NetworkReceiver>,
    /// Why the receiver could not start, if it could not.
    start_error: Option<String>,
    /// Captures restarted for this input: a new region, or a target found
    /// again.
    restarts: u32,
    /// The listing generation the target was last resolved against.
    resolved: Option<u64>,
    /// A slate picture is due: black when the target went away and the
    /// slate is black, or blank when the input closed.
    pending: Option<Pending>,
    reported: Option<(ScreenCaptureStatus, Instant)>,
    /// The reopen count of the receiver connection last seen live.
    live_connection: Option<u32>,
    /// How the captured target's sound is captured.
    audio: AudioPlan,
    /// The tap serving the sound, and the source it gathers; kept across
    /// capture restarts while the source stays the same.
    tap: Option<(TapSource, AudioTap)>,
    /// Why the tap could not start, if it could not.
    tap_error: Option<String>,
    /// When its clock drift was last logged (AU-004).
    sync_log: SyncLog,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pending {
    Black,
    Blank,
}

impl Source {
    fn new(config: ScreenCaptureConfig, closed: bool) -> Self {
        Self {
            config,
            closed,
            target: Target::Unknown,
            receiver: None,
            start_error: None,
            restarts: 0,
            resolved: None,
            pending: None,
            reported: None,
            live_connection: None,
            audio: AudioPlan::Silent,
            tap: None,
            tap_error: None,
            sync_log: SyncLog::default(),
        }
    }

    /// Stops capturing; the slate is shown next.
    fn lose(&mut self, target: Target) {
        let had_capture = matches!(self.target, Target::Captured(_));
        self.receiver = None;
        self.start_error = None;
        self.live_connection = None;
        self.audio = AudioPlan::Silent;
        self.tap = None;
        self.tap_error = None;
        if had_capture && self.config.slate == StallSlate::Black {
            self.pending = Some(Pending::Black);
        }
        self.target = target;
    }
}

/// The latest desktop listing, shared with the watch thread.
#[derive(Default)]
struct Latest {
    listing: Option<Arc<DesktopListing>>,
    generation: u64,
    shutdown: bool,
}

struct Watch {
    shared: Arc<(Mutex<Latest>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    fn spawn(backend: &DesktopBackend, interval: Duration) -> Self {
        let shared = Arc::new((Mutex::new(Latest::default()), Condvar::new()));
        let mut lister = backend.lister();
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("freemixd-desktop-watch".into())
                .spawn(move || {
                    loop {
                        let listing = Arc::new(lister());
                        let (lock, wake) = &*shared;
                        let mut latest = lock.lock().unwrap_or_else(PoisonError::into_inner);
                        if latest.shutdown {
                            return;
                        }
                        if latest.listing.as_deref() != Some(&*listing) {
                            latest.listing = Some(listing);
                            latest.generation += 1;
                        }
                        let (latest, _) = wake
                            .wait_timeout(latest, interval)
                            .unwrap_or_else(PoisonError::into_inner);
                        if latest.shutdown {
                            return;
                        }
                    }
                })
                .ok()
        };
        Self { shared, thread }
    }

    fn latest(&self) -> (u64, Option<Arc<DesktopListing>>) {
        let latest = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        (latest.generation, latest.listing.clone())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown = true;
        self.shared.1.notify_all();
        // A listing in progress finishes on its own; never wait for it.
        drop(self.thread.take());
    }
}

/// The screen capture inputs of one native daemon.
pub(crate) struct NativeScreenCapture {
    backend: DesktopBackend,
    sources: BTreeMap<InputId, Source>,
    format: Format,
    /// The desktop watch, running while any screen capture input exists.
    watch: Option<Watch>,
    interval: Duration,
    /// The engine frame last pulled for.
    last_frame: Option<u64>,
    /// A Wayland desktop's portal captures, which replace the listing.
    portal: Option<crate::native_portal_capture::PortalSources>,
}

impl NativeScreenCapture {
    /// Starts watching the desktop for `inputs` through `backend`.
    pub(crate) fn start(
        backend: DesktopBackend,
        inputs: Vec<ScreenCaptureInput>,
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
        sample_rate: SampleRate,
    ) -> Self {
        let format = Format {
            rate,
            clock_domain,
            dimensions,
            sample_rate,
        };
        if let Some(factory) = backend.portal() {
            let mut portal = crate::native_portal_capture::PortalSources::new(factory, format);
            let configs: Vec<_> = inputs
                .iter()
                .map(|input| (input.input, input.config.clone()))
                .collect();
            portal.sync(&configs, |id| {
                inputs.iter().any(|input| input.input == id && input.closed)
            });
            return Self {
                backend,
                sources: BTreeMap::new(),
                format,
                watch: None,
                interval: LIST_INTERVAL,
                last_frame: None,
                portal: Some(portal),
            };
        }
        let mut capture = Self {
            backend,
            sources: BTreeMap::new(),
            format,
            watch: None,
            interval: LIST_INTERVAL,
            last_frame: None,
            portal: None,
        };
        for ScreenCaptureInput {
            input,
            config,
            closed,
        } in inputs
        {
            capture.sources.insert(input, Source::new(config, closed));
        }
        capture.follow_watch();
        capture
    }

    /// Runs the desktop watch exactly while screen capture inputs exist.
    fn follow_watch(&mut self) {
        if self.sources.is_empty() {
            self.watch = None;
        } else if self.watch.is_none() {
            self.watch = Some(Watch::spawn(&self.backend, self.interval));
        }
    }

    /// Every screen capture input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        if let Some(portal) = &self.portal {
            return portal.inputs();
        }
        self.sources.keys().copied().collect()
    }

    /// Takes the project's restore tokens of a Wayland desktop's grants
    /// (unused elsewhere), before any capture starts.
    pub(crate) fn set_restore_tokens(&mut self, tokens: BTreeMap<InputId, String>) {
        if let Some(portal) = &mut self.portal {
            portal.set_restore_tokens(tokens);
        }
    }

    /// Every input's restore token, when a Wayland desktop granted or
    /// dropped one since the last call.
    pub(crate) fn take_restore_tokens(&mut self) -> Option<BTreeMap<InputId, String>> {
        self.portal
            .as_mut()
            .and_then(crate::native_portal_capture::PortalSources::take_restore_tokens)
    }

    /// Picks `input`'s target again: a Wayland desktop's dialog asks anew
    /// (its restore token forgotten); a listed desktop captures again.
    /// Returns whether `input` is a screen capture input here.
    pub(crate) fn pick(&mut self, input: InputId) -> bool {
        if let Some(portal) = &mut self.portal {
            return portal.pick(input);
        }
        let Some(source) = self.sources.get_mut(&input) else {
            return false;
        };
        source.receiver = None;
        source.resolved = None;
        true
    }

    /// Follows the project: closes, reopens, adds, removes, and
    /// reconfigures inputs as `configs` and `closed` say. A changed
    /// configuration resolves its target again.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, ScreenCaptureConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
        if let Some(portal) = &mut self.portal {
            portal.sync(configs, closed);
            return;
        }
        self.sources
            .retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        for (input, config) in configs {
            let closed = closed(*input);
            let source = self
                .sources
                .entry(*input)
                .or_insert_with(|| Source::new(config.clone(), closed));
            if source.config != *config {
                source.config = config.clone();
                source.resolved = None;
                // The cursor, rate, and slate are options of the running
                // capture, not of its region: a capture of an unchanged
                // region restarts so a changed option takes effect.
                source.receiver = None;
                source.live_connection = None;
            }
            if closed && !source.closed {
                source.lose(Target::Unknown);
                source.pending = Some(Pending::Blank);
                source.resolved = None;
            }
            if !closed && source.closed {
                source.pending = None;
                source.resolved = None;
            }
            source.closed = closed;
        }
        self.follow_watch();
    }

    /// Resolves every open input against the latest listing and starts,
    /// restarts, or stops its capture.
    fn follow_targets(&mut self) {
        let Some(watch) = &self.watch else {
            return;
        };
        let (generation, listing) = watch.latest();
        let Some(listing) = listing else {
            return;
        };
        for (&input, source) in &mut self.sources {
            if source.closed || source.resolved == Some(generation) {
                continue;
            }
            source.resolved = Some(generation);
            match crate::desktop_capture::resolve(&self.backend, &listing, &source.config) {
                Resolution::Found(region) => {
                    let window = region.window_id.and_then(|id| {
                        listing
                            .inventory
                            .windows
                            .iter()
                            .find(|window| window.id == id)
                    });
                    let audio = self.backend.audio_plan(&source.config, &region, window);
                    if source.target == Target::Captured(region)
                        && source.receiver.is_some()
                        && source.audio == audio
                    {
                        continue;
                    }
                    let found_again = !matches!(source.target, Target::Unknown);
                    if found_again {
                        source.restarts = source.restarts.saturating_add(1);
                    }
                    // Release the old capture before the new one starts.
                    source.receiver = None;
                    source.live_connection = None;
                    source.target = Target::Captured(region);
                    let mut device =
                        self.backend
                            .capture_device(&region, &source.config, self.format.rate);
                    attach_audio(input, source, audio, &mut device);
                    let mut config = NetworkInputConfig::device(
                        device,
                        capture_settings(&source.config),
                        self.format.dimensions.0,
                        self.format.dimensions.1,
                        self.format.rate,
                    );
                    config.sample_rate = self.format.sample_rate.hertz();
                    config.channels = 2;
                    match NetworkReceiver::start(config) {
                        Ok(receiver) => {
                            source.receiver = Some(receiver);
                            source.start_error = None;
                        }
                        Err(error) => {
                            eprintln!(
                                "FREEMIXD_SCREEN_CAPTURE\tv=1\tinput={input}\tstart_failed\t\
                                 backend={}",
                                self.backend.token()
                            );
                            source.start_error = Some(error.to_string());
                        }
                    }
                }
                Resolution::Gone(reason) => {
                    if source.target != Target::Gone(reason.clone()) {
                        source.lose(Target::Gone(reason));
                    }
                }
                Resolution::Unsupported(reason) => {
                    if source.target != Target::Unsupported(reason.clone()) {
                        source.lose(Target::Unsupported(reason));
                    }
                }
            }
        }
    }

    /// Pulls every input's picture for engine frame `frame`, once per
    /// frame. Never blocks on the desktop.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        if let Some(portal) = &mut self.portal {
            if self.last_frame.is_some_and(|last| frame <= last) {
                return Vec::new();
            }
            self.last_frame = Some(frame);
            return portal.pull(frame);
        }
        if self.last_frame.is_some_and(|last| frame <= last) || self.sources.is_empty() {
            return Vec::new();
        }
        self.last_frame = Some(frame);
        self.follow_targets();
        let format = self.format;
        let mut pulled = Vec::with_capacity(self.sources.len());
        for (&input, source) in &mut self.sources {
            // The receiver hands over each frame's picture and sound
            // together, once per frame.
            let (picture, sound) = match &source.receiver {
                Some(receiver) if !source.closed => {
                    let PulledFrame { picture, audio } = receiver.pull();
                    (Some(picture), Some(audio))
                }
                _ => (None, None),
            };
            let video = match source.pending.take() {
                Some(Pending::Blank) => output_timing(frame, format.rate, format.clock_domain)
                    .and_then(|timing| blank_picture(format.dimensions, timing)),
                Some(Pending::Black) => format.black(frame),
                None => match picture {
                    Some(Picture::New(bytes)) => format.picture(frame, bytes),
                    Some(Picture::Black) => format.black(frame),
                    Some(Picture::Hold) | None => None,
                },
            };
            if source.closed && video.is_none() {
                continue;
            }
            let audio = match sound {
                Some(planes) if source.audio.captures_sound() => format.audio(frame, planes),
                _ => format.silence(frame),
            };
            pulled.push(PulledStream {
                input,
                video,
                audio,
            });
        }
        pulled
    }

    /// Every open screen capture with sound's drift loop as wire samples
    /// (Protocol 2.76).
    pub(crate) fn sync_samples(&self) -> Vec<fm_protocol::SourceSyncSample> {
        self.sources
            .iter()
            .filter(|(_, source)| !source.closed && source.audio.captures_sound())
            .filter_map(|(&input, source)| {
                let telemetry = source.receiver.as_ref()?.sync_telemetry()?;
                wire_sample(SourceKind::ScreenCapture, input, &telemetry)
            })
            .collect()
    }

    /// The statuses due for a report at `now`: a changed state, size,
    /// window, attempt, restart count, or audio state at once, a changed
    /// error at most once a second.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, ScreenCaptureStatus)> {
        if let Some(portal) = &mut self.portal {
            return portal.statuses(now);
        }
        self.follow_targets();
        let mut due = Vec::new();
        for (&input, source) in &mut self.sources {
            if !source.closed && source.audio.captures_sound() {
                source.sync_log.report(
                    SourceKind::ScreenCapture,
                    input,
                    source
                        .receiver
                        .as_ref()
                        .and_then(NetworkReceiver::sync_telemetry)
                        .as_ref(),
                    now,
                );
            }
            let status = status_of(source);
            let report = match &source.reported {
                None => true,
                Some((reported, at)) => {
                    attempt_of(reported.state) != attempt_of(status.state)
                        || reported.reopens != status.reopens
                        || reported.width != status.width
                        || reported.height != status.height
                        || reported.window_id != status.window_id
                        || reported.audio != status.audio
                        || (reported.last_error.is_none() && status.last_error.is_some())
                        || (reported.audio_error.is_none() && status.audio_error.is_some())
                        || ((reported.last_error != status.last_error
                            || reported.audio_error != status.audio_error)
                            && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
                }
            };
            if !report {
                continue;
            }
            if source
                .reported
                .as_ref()
                .is_none_or(|(reported, _)| reported.state.token() != status.state.token())
            {
                eprintln!(
                    "FREEMIXD_SCREEN_CAPTURE\tv=1\tinput={input}\tstate={}\treopens={}\t\
                     size={}x{}\ttarget={}",
                    status.state.token(),
                    status.reopens,
                    status.width,
                    status.height,
                    source.config.target.kind_token()
                );
            }
            if source
                .reported
                .as_ref()
                .is_none_or(|(reported, _)| reported.audio != status.audio)
                && status.audio != ScreenCaptureAudioState::Off
            {
                eprintln!(
                    "FREEMIXD_SCREEN_CAPTURE_AUDIO\tv=1\tinput={input}\taudio={}\tstate={}",
                    source.config.audio.token(),
                    status.audio.token(),
                );
            }
            source.reported = Some((status.clone(), now));
            due.push((input, status));
        }
        due
    }
}

/// The capture child's audio input for `audio`, attached to `device`; a
/// tap is started, or kept while it gathers the same source.
fn attach_audio(
    input: InputId,
    source: &mut Source,
    audio: AudioPlan,
    device: &mut fm_codec_ffmpeg::network_input::DeviceInput,
) {
    match &audio {
        AudioPlan::Silent | AudioPlan::Unsupported(_) => {
            source.tap = None;
            source.tap_error = None;
        }
        AudioPlan::Device { input, environment } => {
            source.tap = None;
            source.tap_error = None;
            device.audio = Some(input.clone());
            for name in environment {
                if !device.environment.contains(name) {
                    device.environment.push(name.clone());
                }
            }
        }
        AudioPlan::Tap(wanted) => {
            if source
                .tap
                .as_ref()
                .is_none_or(|(running, _)| running != wanted)
            {
                source.tap = None;
                match AudioTap::start(wanted.clone()) {
                    Ok(tap) => {
                        source.tap = Some((wanted.clone(), tap));
                        source.tap_error = None;
                    }
                    Err(error) => {
                        eprintln!("FREEMIXD_SCREEN_CAPTURE_AUDIO\tv=1\tinput={input}\ttap_failed");
                        source.tap_error = Some(format!("the audio tap did not start: {error}"));
                    }
                }
            }
            if let Some((_, tap)) = &source.tap {
                device.audio = Some(tap.audio_input());
            }
        }
    }
    source.audio = audio;
}

impl AudioPlan {
    /// Whether the plan captures any sound.
    pub(crate) fn captures_sound(&self) -> bool {
        matches!(self, Self::Device { .. } | Self::Tap(_))
    }
}

/// The audio state of a source whose picture is in `capture`.
fn audio_status(
    source: &Source,
    capture: ScreenCaptureState,
) -> (ScreenCaptureAudioState, Option<String>) {
    let bounded =
        |text: &str| Some(bound_screen_capture_error(text)).filter(|text| !text.is_empty());
    if source.closed || !matches!(source.target, Target::Captured(_)) {
        return (ScreenCaptureAudioState::Off, None);
    }
    let receiver = match &source.audio {
        AudioPlan::Silent => return (ScreenCaptureAudioState::Off, None),
        AudioPlan::Unsupported(reason) => {
            return (ScreenCaptureAudioState::Unsupported, bounded(reason));
        }
        AudioPlan::Tap(_) if source.tap.is_none() => {
            return (
                ScreenCaptureAudioState::Failed,
                source.tap_error.as_deref().and_then(bounded),
            );
        }
        AudioPlan::Device { .. } | AudioPlan::Tap(_) => match &source.receiver {
            Some(receiver) => receiver,
            None => return (ScreenCaptureAudioState::Opening, None),
        },
    };
    let live = capture == ScreenCaptureState::Live;
    if live && !receiver.audio_active() {
        return (
            ScreenCaptureAudioState::Failed,
            bounded(match &source.audio {
                AudioPlan::Device { .. } => {
                    "the system audio did not open (is PulseAudio or PipeWire running?)"
                }
                _ => "the capture could not read its audio",
            }),
        );
    }
    let tap_state = source.tap.as_ref().map(|(_, tap)| tap.state());
    match tap_state {
        Some(TapState::NoStream) => (ScreenCaptureAudioState::NoStream, None),
        Some(TapState::Unsupported(reason)) => {
            (ScreenCaptureAudioState::Unsupported, bounded(&reason))
        }
        Some(TapState::Failed(reason)) => (ScreenCaptureAudioState::Failed, bounded(&reason)),
        Some(TapState::Live) | None if live => (ScreenCaptureAudioState::Live, None),
        Some(TapState::Starting | TapState::Live) | None => {
            (ScreenCaptureAudioState::Opening, None)
        }
    }
}

/// A source's status now.
fn status_of(source: &mut Source) -> ScreenCaptureStatus {
    let mut status = picture_status(source);
    let (audio, audio_error) = audio_status(source, status.state);
    status.audio = audio;
    status.audio_error = audio_error;
    status
}

/// A source's picture status now.
fn picture_status(source: &mut Source) -> ScreenCaptureStatus {
    if source.closed {
        return ScreenCaptureStatus::default();
    }
    match &source.target {
        Target::Unknown => ScreenCaptureStatus {
            state: ScreenCaptureState::Opening,
            reopens: source.restarts,
            ..ScreenCaptureStatus::default()
        },
        Target::Gone(reason) => ScreenCaptureStatus {
            state: ScreenCaptureState::TargetGone,
            reopens: source.restarts,
            last_error: Some(reason.clone()),
            ..ScreenCaptureStatus::default()
        },
        Target::Unsupported(reason) => ScreenCaptureStatus {
            state: ScreenCaptureState::Unsupported,
            reopens: source.restarts,
            last_error: Some(reason.clone()),
            ..ScreenCaptureStatus::default()
        },
        Target::Captured(region) => {
            let mut status = match (&source.receiver, &source.start_error) {
                (Some(receiver), _) => {
                    capture_status(&receiver.status(), &mut source.live_connection)
                }
                (None, Some(error)) => ScreenCaptureStatus {
                    state: ScreenCaptureState::Failed,
                    last_error: Some(error.clone()),
                    ..ScreenCaptureStatus::default()
                },
                (None, None) => ScreenCaptureStatus {
                    state: ScreenCaptureState::Opening,
                    ..ScreenCaptureStatus::default()
                },
            };
            status.reopens = status.reopens.saturating_add(source.restarts);
            status.width = region.target_width;
            status.height = region.target_height;
            status.window_id = region.window_id.filter(|id| *id != 0);
            status
        }
    }
}

/// The state token and reopen attempt of `state`, which a report compares
/// (the countdown to the next attempt changes every instant).
fn attempt_of(state: ScreenCaptureState) -> (&'static str, u32) {
    match state {
        ScreenCaptureState::Reopening { attempt, .. } => ("reopening", attempt),
        other => (other.token(), 0),
    }
}

/// The capture status of a receiver's status: opening until the capture
/// first delivers, live while it delivers (through brief stalls of the
/// same connection), reopening between attempts.
fn capture_status(
    status: &NetworkStreamStatus,
    live_connection: &mut Option<u32>,
) -> ScreenCaptureStatus {
    let opened = *live_connection == Some(status.reconnects);
    ScreenCaptureStatus {
        state: match status.state {
            NetworkStreamState::Idle => ScreenCaptureState::Idle,
            NetworkStreamState::Buffering | NetworkStreamState::Stalled if opened => {
                ScreenCaptureState::Live
            }
            NetworkStreamState::Connecting
            | NetworkStreamState::Listening
            | NetworkStreamState::Buffering
            | NetworkStreamState::Stalled => ScreenCaptureState::Opening,
            NetworkStreamState::Live => {
                *live_connection = Some(status.reconnects);
                ScreenCaptureState::Live
            }
            NetworkStreamState::Reconnecting {
                attempt,
                next_in_ms,
            } => ScreenCaptureState::Reopening {
                attempt: attempt.max(1),
                next_in_ms,
            },
            NetworkStreamState::Failed => ScreenCaptureState::Failed,
        },
        reopens: status.reconnects,
        last_error: status.last_error.clone(),
        ..ScreenCaptureStatus::default()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_io_linux::screen_capture::DesktopListing;
    use fm_protocol::{
        ScreenCaptureInventory, ScreenCaptureTarget, ScreenInfo, WindowInfo, WindowRule,
    };

    use super::*;

    fn input() -> InputId {
        InputId::new(NonZeroU128::new(11).unwrap())
    }

    fn window_config() -> ScreenCaptureConfig {
        ScreenCaptureConfig {
            slate: StallSlate::Black,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
                "XTerm", "build", None,
            )))
        }
    }

    fn listing(windows: Vec<WindowInfo>) -> DesktopListing {
        DesktopListing {
            inventory: ScreenCaptureInventory {
                supported: true,
                backend: "fake".into(),
                screens: vec![ScreenInfo {
                    index: 0,
                    name: "fake".into(),
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 36,
                    primary: true,
                }],
                windows,
                error: None,
            },
            desktop: Some((64, 36)),
        }
    }

    fn window(id: u64, width: u32) -> WindowInfo {
        WindowInfo {
            id,
            app: "XTerm".into(),
            title: "build".into(),
            pid: None,
            x: 0,
            y: 0,
            width,
            height: 20,
        }
    }

    /// A capture whose desktop listings a test sets.
    fn capture(listings: Arc<Mutex<DesktopListing>>, closed: bool) -> NativeScreenCapture {
        let mut capture = NativeScreenCapture {
            backend: DesktopBackend::fake(listings),
            sources: BTreeMap::new(),
            format: Format {
                rate: FrameRate::new(25, 1).unwrap(),
                clock_domain: ClockDomainId::new(NonZeroU128::new(1).unwrap()),
                dimensions: (32, 18),
                sample_rate: SampleRate::new(48_000).unwrap(),
            },
            watch: None,
            interval: Duration::from_millis(10),
            last_frame: None,
            portal: None,
        };
        capture.sync(&[(input(), window_config())], |_| closed);
        capture
    }

    fn settle(capture: &mut NativeScreenCapture, frame: &mut u64, done: impl Fn(&Source) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            *frame += 1;
            let _ = capture.pull(*frame);
            if done(&capture.sources[&input()]) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{:?}",
                capture.sources[&input()].target
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn targets_are_followed_through_resize_closure_and_return() {
        let listings = Arc::new(Mutex::new(listing(vec![window(7, 40)])));
        let mut capture = capture(Arc::clone(&listings), false);
        let mut frame = 0;
        settle(&mut capture, &mut frame, |source| {
            matches!(source.target, Target::Captured(_))
        });
        let Target::Captured(region) = capture.sources[&input()].target else {
            unreachable!();
        };
        assert_eq!(
            (region.window_id, region.width, region.height),
            (Some(7), 40, 20)
        );
        let status = capture.statuses(Instant::now()).remove(0).1;
        assert_eq!(
            (status.width, status.height, status.window_id),
            (40, 20, Some(7))
        );
        assert_eq!(status.reopens, 0);

        // A resize restarts the capture at the new size.
        *listings.lock().unwrap() = listing(vec![window(7, 30)]);
        settle(
            &mut capture,
            &mut frame,
            |source| matches!(source.target, Target::Captured(region) if region.width == 30),
        );
        let status = capture.statuses(Instant::now()).remove(0).1;
        assert_eq!((status.width, status.reopens), (30, 1));

        // Closing the window shows the black slate once and reports the
        // target gone.
        *listings.lock().unwrap() = listing(Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        let black = loop {
            frame += 1;
            if let Some(pulled) = capture.pull(frame).pop()
                && let Some(video) = pulled.video
            {
                break video;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            black.payload().planes()[0].bytes()[3],
            u8::MAX,
            "opaque black"
        );
        assert!(capture.sources[&input()].receiver.is_none());
        let status = capture.statuses(Instant::now()).remove(0).1;
        assert_eq!(status.state, ScreenCaptureState::TargetGone);
        assert_eq!(
            status.last_error.as_deref(),
            Some("no XTerm window titled \"build\" is open")
        );
        assert_eq!((status.width, status.window_id), (0, None));

        // The window opens again under a new id: captured again.
        *listings.lock().unwrap() = listing(vec![window(9, 40)]);
        settle(
            &mut capture,
            &mut frame,
            |source| matches!(source.target, Target::Captured(region) if region.window_id == Some(9)),
        );
        let status = capture.statuses(Instant::now()).remove(0).1;
        assert_eq!(status.window_id, Some(9));
        assert_eq!(status.reopens, 2);
        assert_ne!(status.state, ScreenCaptureState::TargetGone);
    }

    #[test]
    fn a_changed_cursor_or_rate_restarts_a_capture_of_the_same_region() {
        let listings = Arc::new(Mutex::new(listing(vec![window(7, 40)])));
        let mut capture = capture(Arc::clone(&listings), false);
        let mut frame = 0;
        settle(&mut capture, &mut frame, |source| {
            matches!(source.target, Target::Captured(_)) && source.receiver.is_some()
        });
        assert_eq!(capture.sources[&input()].restarts, 0);
        // The window stays where it is; only the cursor and rate change.
        let changed = ScreenCaptureConfig {
            cursor: false,
            fps: 10,
            ..window_config()
        };
        capture.sync(&[(input(), changed)], |_| false);
        settle(&mut capture, &mut frame, |source| {
            source.restarts == 1 && source.receiver.is_some()
        });
    }

    #[test]
    fn unsupported_desktops_and_closed_inputs_report_their_state() {
        let listings = Arc::new(Mutex::new(DesktopListing {
            inventory: ScreenCaptureInventory::unsupported("no desktop session"),
            desktop: None,
        }));
        let mut capture = capture(Arc::clone(&listings), false);
        let mut frame = 0;
        settle(&mut capture, &mut frame, |source| {
            matches!(source.target, Target::Unsupported(_))
        });
        let status = capture.statuses(Instant::now()).remove(0).1;
        assert_eq!(status.state, ScreenCaptureState::Unsupported);
        assert_eq!(status.last_error.as_deref(), Some("no desktop session"));
        // Every frame pushes silence and no picture.
        frame += 1;
        let pulled = capture.pull(frame);
        assert_eq!(pulled.len(), 1);
        assert!(pulled[0].video.is_none());
        assert!(pulled[0].audio.is_some());

        // Closing shows one blank picture, then nothing; the input is idle.
        capture.sync(&[(input(), window_config())], |_| true);
        frame += 1;
        let pulled = capture.pull(frame);
        assert!(pulled[0].video.is_some(), "the blank picture");
        frame += 1;
        assert_eq!(capture.pull(frame).len(), 0);
        assert_eq!(
            capture.statuses(Instant::now()),
            [(input(), ScreenCaptureStatus::default())]
        );
        // Removing the last input stops the desktop watch.
        capture.sync(&[], |_| false);
        assert!(capture.watch.is_none());
        assert_eq!(capture.inputs().len(), 0);
    }

    /// Each audio choice plans its capture: silence, the default output's
    /// monitor read by the capture child, or a tap following the window's
    /// process (or, without one, its application); application audio
    /// without a window cannot be captured.
    #[test]
    fn audio_choices_plan_their_capture() {
        use fm_io_linux::capture_audio::AppAudioKey;
        use fm_protocol::ScreenCaptureAudio;

        let backend = DesktopBackend::fake(Arc::new(Mutex::new(listing(Vec::new()))));
        let region = CaptureRegion {
            window_id: Some(7),
            screen: None,
            origin_x: 0,
            origin_y: 0,
            target_width: 40,
            target_height: 20,
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let with = |audio| ScreenCaptureConfig {
            audio,
            ..window_config()
        };
        let mut owned = window(7, 40);
        owned.pid = Some(4_120);
        assert_eq!(
            backend.audio_plan(&with(ScreenCaptureAudio::None), &region, Some(&owned)),
            AudioPlan::Silent
        );
        let AudioPlan::Device { input, environment } =
            backend.audio_plan(&with(ScreenCaptureAudio::System), &region, Some(&owned))
        else {
            panic!("system audio is read by the capture child");
        };
        assert_eq!(
            (input.format.as_str(), input.path.as_str()),
            ("pulse", "@DEFAULT_MONITOR@")
        );
        assert!(environment.contains(&"PULSE_SERVER".to_owned()));
        assert_eq!(
            backend.audio_plan(
                &with(ScreenCaptureAudio::Application),
                &region,
                Some(&owned)
            ),
            AudioPlan::Tap(TapSource::pulse_application(AppAudioKey {
                pid: Some(4_120),
                app: "XTerm".into(),
            }))
        );
        assert!(matches!(
            backend.audio_plan(&with(ScreenCaptureAudio::Application), &region, None),
            AudioPlan::Unsupported(_)
        ));
        assert!(
            AudioPlan::Tap(TapSource::pulse_application(AppAudioKey::default())).captures_sound()
        );
        assert!(!AudioPlan::Silent.captures_sound());
    }
}
