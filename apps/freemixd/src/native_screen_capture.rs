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
//! A screen capture input has no sound: it pushes silence to its Master
//! strip (application audio can be added with a system audio input).
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
use fm_io_linux::screen_capture::{CaptureRegion, DesktopListing, Resolution, capture_settings};
use fm_protocol::{ScreenCaptureConfig, ScreenCaptureState, ScreenCaptureStatus};
use fm_types::network_stream::{NetworkStreamState, NetworkStreamStatus, StallSlate};
use fm_types::{FrameRate, InputId};

use crate::desktop_capture::DesktopBackend;
use crate::native_files::{blank_picture, output_timing};
use crate::native_streams::{Format, PulledStream};

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
        }
    }

    /// Stops capturing; the slate is shown next.
    fn lose(&mut self, target: Target) {
        let had_capture = matches!(self.target, Target::Captured(_));
        self.receiver = None;
        self.start_error = None;
        self.live_connection = None;
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
        let mut capture = Self {
            backend,
            sources: BTreeMap::new(),
            format: Format {
                rate,
                clock_domain,
                dimensions,
                sample_rate,
            },
            watch: None,
            interval: LIST_INTERVAL,
            last_frame: None,
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
        self.sources.keys().copied().collect()
    }

    /// Follows the project: closes, reopens, adds, removes, and
    /// reconfigures inputs as `configs` and `closed` say. A changed
    /// configuration resolves its target again.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, ScreenCaptureConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
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
                    if source.target == Target::Captured(region) && source.receiver.is_some() {
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
                    let device =
                        self.backend
                            .capture_device(&region, &source.config, self.format.rate);
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
        if self.last_frame.is_some_and(|last| frame <= last) || self.sources.is_empty() {
            return Vec::new();
        }
        self.last_frame = Some(frame);
        self.follow_targets();
        let format = self.format;
        let mut pulled = Vec::with_capacity(self.sources.len());
        for (&input, source) in &mut self.sources {
            let video = match source.pending.take() {
                Some(Pending::Blank) => output_timing(frame, format.rate, format.clock_domain)
                    .and_then(|timing| blank_picture(format.dimensions, timing)),
                Some(Pending::Black) => format.black(frame),
                None => match &source.receiver {
                    Some(receiver) if !source.closed => {
                        let PulledFrame { picture, .. } = receiver.pull();
                        match picture {
                            Picture::New(bytes) => format.picture(frame, bytes),
                            Picture::Black => format.black(frame),
                            Picture::Hold => None,
                        }
                    }
                    _ => None,
                },
            };
            if source.closed && video.is_none() {
                continue;
            }
            pulled.push(PulledStream {
                input,
                video,
                audio: format.silence(frame),
            });
        }
        pulled
    }

    /// The statuses due for a report at `now`: a changed state, size,
    /// window, attempt, or restart count at once, a changed error at most
    /// once a second.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, ScreenCaptureStatus)> {
        self.follow_targets();
        let mut due = Vec::new();
        for (&input, source) in &mut self.sources {
            let status = status_of(source);
            let report = match &source.reported {
                None => true,
                Some((reported, at)) => {
                    attempt_of(reported.state) != attempt_of(status.state)
                        || reported.reopens != status.reopens
                        || reported.width != status.width
                        || reported.height != status.height
                        || reported.window_id != status.window_id
                        || (reported.last_error.is_none() && status.last_error.is_some())
                        || (reported.last_error != status.last_error
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
            source.reported = Some((status.clone(), now));
            due.push((input, status));
        }
        due
    }
}

/// A source's status now.
fn status_of(source: &mut Source) -> ScreenCaptureStatus {
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
}
