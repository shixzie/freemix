//! Screen capture inputs of a Wayland desktop, through its
//! xdg-desktop-portal (IN-011).
//!
//! On a Wayland desktop ([`crate::desktop_capture::DesktopBackend::Portal`])
//! [`crate::native_screen_capture::NativeScreenCapture`] hands its inputs to
//! one [`PortalSources`]: each open input runs one
//! [`fm_io_linux::wayland::PortalCapture`], which asks the desktop to share
//! a screen (a screen target) or a window (a window target) and turns the
//! shared `PipeWire` pictures into program pictures (cropped, letterboxed,
//! at most the capture rate). Nothing is listed: the desktop's own sharing
//! dialog picks the target, and the input is `awaiting_selection` while it
//! waits on the server's desktop. A cancelled or refused dialog, or sharing
//! stopped on the desktop, is `denied` until the operator picks again
//! ([`PortalSources::pick`]); a shared window that closes is `target_gone`
//! and asked for again.
//!
//! Each grant's restore token is kept per input
//! ([`PortalSources::take_restore_tokens`]) and the session loop stores it
//! in the project (see [`keep_restore_tokens`]), so a reopened project
//! shares the same targets again without the dialog where the desktop
//! allows it. A new pick forgets the input's token first, so the dialog
//! asks again.
//!
//! Statuses are reported like every other screen capture's: at once when
//! the state, size, or restart count changes, a changed error at most once
//! a second, and each state change logged as a `FREEMIXD_SCREEN_CAPTURE`
//! record (`backend=portal`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_io_linux::screen_capture::capture_rate;
use fm_io_linux::wayland::{
    CaptureSettings, PortalBackend, PortalCapture, ShareRequest, SourceKind,
};
use fm_persistence::StoredProject;
use fm_protocol::{
    ScreenCaptureAudio, ScreenCaptureAudioState, ScreenCaptureConfig, ScreenCaptureState,
    ScreenCaptureStatus,
};
use fm_types::InputId;
use fm_types::network_stream::StallSlate;

use crate::native_files::{blank_picture, output_timing};
use crate::native_streams::{Format, PulledStream};

/// Starts one share of the desktop (the real portal, or a fake in tests).
pub(crate) type PortalFactory = Arc<dyn Fn() -> Box<dyn PortalBackend> + Send + Sync>;

/// Longest a changed error text waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

struct Source {
    config: ScreenCaptureConfig,
    closed: bool,
    capture: Option<PortalCapture>,
    /// Shares started for this input after its first.
    restarts: u32,
    /// A captured picture is on the input (what a black slate replaces).
    showing: bool,
    /// A blank picture is due: the input closed.
    blank: bool,
    reported: Option<(ScreenCaptureStatus, Instant)>,
}

/// The screen capture inputs of a daemon on a Wayland desktop.
pub(crate) struct PortalSources {
    factory: PortalFactory,
    format: Format,
    sources: BTreeMap<InputId, Source>,
    /// Each input's latest restore token.
    tokens: BTreeMap<InputId, String>,
    tokens_changed: bool,
}

/// Why a desktop-portal capture carries no sound.
const NO_PORTAL_AUDIO: &str =
    "a Wayland desktop capture has no sound here yet; add a system audio input for it";

/// What a capture asks of the desktop for `config`.
fn request(config: &ScreenCaptureConfig, token: Option<&String>) -> ShareRequest {
    ShareRequest {
        kind: SourceKind::of(&config.target),
        cursor: config.cursor,
        restore_token: token.cloned(),
    }
}

impl PortalSources {
    pub(crate) fn new(factory: PortalFactory, format: Format) -> Self {
        Self {
            factory,
            format,
            sources: BTreeMap::new(),
            tokens: BTreeMap::new(),
            tokens_changed: false,
        }
    }

    fn settings(&self, config: &ScreenCaptureConfig) -> CaptureSettings {
        CaptureSettings {
            crop: config.crop,
            fps: capture_rate(config, self.format.rate),
            width: self.format.dimensions.0,
            height: self.format.dimensions.1,
        }
    }

    /// Takes the restore tokens the project keeps (before any capture
    /// starts).
    pub(crate) fn set_restore_tokens(&mut self, tokens: BTreeMap<InputId, String>) {
        self.tokens = tokens;
        self.tokens_changed = false;
    }

    /// Every screen capture input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.sources.keys().copied().collect()
    }

    /// Follows the project: closes, reopens, adds, removes, and
    /// reconfigures inputs. A new target kind or pointer setting asks the
    /// desktop again; a new crop or rate applies to the next pictures.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, ScreenCaptureConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
        self.sources
            .retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        let before = self.tokens.len();
        self.tokens
            .retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        self.tokens_changed |= self.tokens.len() != before;
        for (input, config) in configs {
            let closed = closed(*input);
            let settings = self.settings(config);
            let source = self.sources.entry(*input).or_insert_with(|| Source {
                config: config.clone(),
                closed,
                capture: None,
                restarts: 0,
                showing: false,
                blank: false,
                reported: None,
            });
            if source.config != *config {
                let old = &source.config;
                if SourceKind::of(&old.target) != SourceKind::of(&config.target) {
                    // A token restores a screen or a window, not the other.
                    self.tokens_changed |= self.tokens.remove(input).is_some();
                    source.capture = None;
                } else if old.cursor != config.cursor {
                    source.capture = None;
                } else if let Some(capture) = &source.capture {
                    capture.set_settings(settings);
                }
                source.config = config.clone();
            }
            if closed && !source.closed {
                source.capture = None;
                source.showing = false;
                source.blank = true;
            }
            if !closed && source.closed {
                source.blank = false;
            }
            source.closed = closed;
        }
    }

    /// Starts a share for every open input without one.
    fn follow(&mut self) {
        for (input, source) in &mut self.sources {
            if source.closed || source.capture.is_some() {
                continue;
            }
            if source.reported.is_some() {
                source.restarts = source.restarts.saturating_add(1);
            }
            let settings = CaptureSettings {
                crop: source.config.crop,
                fps: capture_rate(&source.config, self.format.rate),
                width: self.format.dimensions.0,
                height: self.format.dimensions.1,
            };
            source.capture = Some(PortalCapture::start(
                (self.factory)(),
                request(&source.config, self.tokens.get(input)),
                settings,
            ));
        }
    }

    /// Asks the desktop to pick `input`'s screen or window again: its
    /// restore token is forgotten and its share starts over. Returns
    /// whether `input` is one of these inputs.
    pub(crate) fn pick(&mut self, input: InputId) -> bool {
        let Some(source) = self.sources.get_mut(&input) else {
            return false;
        };
        self.tokens_changed |= self.tokens.remove(&input).is_some();
        source.capture = None;
        true
    }

    /// Pulls every input's picture for engine frame `frame`.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        self.follow();
        let format = self.format;
        let mut pulled = Vec::with_capacity(self.sources.len());
        for (&input, source) in &mut self.sources {
            let video = if std::mem::take(&mut source.blank) {
                output_timing(frame, format.rate, format.clock_domain)
                    .and_then(|timing| blank_picture(format.dimensions, timing))
            } else {
                source.capture.as_ref().and_then(|capture| {
                    if let Some(bytes) = capture.take_picture() {
                        source.showing = true;
                        return format.picture(frame, bytes);
                    }
                    let lost = source.showing
                        && capture.status().state != ScreenCaptureState::Live
                        && source.config.slate == StallSlate::Black;
                    if lost {
                        source.showing = false;
                        format.black(frame)
                    } else {
                        None
                    }
                })
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

    /// Collects the restore tokens the captures were granted.
    fn collect_tokens(&mut self) {
        for (input, source) in &self.sources {
            let Some(token) = source
                .capture
                .as_ref()
                .and_then(PortalCapture::take_restore_token)
            else {
                continue;
            };
            let changed = match token {
                Some(token) => self.tokens.insert(*input, token.clone()) != Some(token),
                None => self.tokens.remove(input).is_some(),
            };
            self.tokens_changed |= changed;
        }
    }

    /// Every input's restore token, when any changed since the last call.
    pub(crate) fn take_restore_tokens(&mut self) -> Option<BTreeMap<InputId, String>> {
        self.collect_tokens();
        std::mem::take(&mut self.tokens_changed).then(|| self.tokens.clone())
    }

    /// The statuses due for a report at `now`.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, ScreenCaptureStatus)> {
        self.follow();
        self.collect_tokens();
        let mut due = Vec::new();
        for (&input, source) in &mut self.sources {
            let status = match (&source.capture, source.closed) {
                (Some(capture), false) => {
                    let mut status = capture.status();
                    status.reopens = status.reopens.saturating_add(source.restarts);
                    if source.config.audio != ScreenCaptureAudio::None {
                        status.audio = ScreenCaptureAudioState::Unsupported;
                        status.audio_error = Some(NO_PORTAL_AUDIO.to_owned());
                    }
                    status
                }
                _ => ScreenCaptureStatus::default(),
            };
            let report = source.reported.as_ref().is_none_or(|(reported, at)| {
                attempt_of(reported.state) != attempt_of(status.state)
                    || reported.reopens != status.reopens
                    || reported.width != status.width
                    || reported.height != status.height
                    || reported.audio != status.audio
                    || (reported.last_error.is_none() && status.last_error.is_some())
                    || (reported.last_error != status.last_error
                        && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
            });
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
                     size={}x{}\ttarget={}\tbackend=portal",
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

/// The state token and reopen attempt of `state`, which a report compares.
fn attempt_of(state: ScreenCaptureState) -> (&'static str, u32) {
    match state {
        ScreenCaptureState::Reopening { attempt, .. } => ("reopening", attempt),
        other => (other.token(), 0),
    }
}

/// Keeps `tokens` as the screen capture restore tokens of the `durable`
/// project; returns whether the project changed (and is due to be
/// written).
pub(crate) fn keep_restore_tokens(
    durable: &mut StoredProject,
    tokens: BTreeMap<InputId, String>,
) -> bool {
    let mut project = durable.project().clone();
    if !project.set_screen_capture_restore_tokens(tokens) {
        return false;
    }
    match durable.with_project(project) {
        Ok(updated) => {
            *durable = updated;
            true
        }
        Err(error) => {
            eprintln!("FREEMIXD_SCREEN_CAPTURE\tv=1\trestore_tokens=refused\terror={error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::num::NonZeroU128;
    use std::sync::Mutex;

    use fm_frame::{ClockDomainId, SampleRate};
    use fm_io_linux::wayland::{PixelLayout, ShareEnd, ShareSink, SourcePicture};
    use fm_protocol::{ScreenCaptureTarget, WindowRule};
    use fm_types::FrameRate;

    use super::*;

    /// What one fake share does.
    #[derive(Clone, Debug)]
    enum Share {
        /// The dialog waits until the capture is dropped.
        Wait,
        /// Granted with this token, then red pictures until dropped.
        Grant(Option<&'static str>),
        /// Refused.
        Deny,
    }

    #[derive(Default)]
    struct Script {
        shares: VecDeque<Share>,
        requests: Vec<ShareRequest>,
    }

    struct Fake(Arc<Mutex<Script>>);

    impl PortalBackend for Fake {
        fn share(&mut self, request: &ShareRequest, sink: &mut dyn ShareSink) -> ShareEnd {
            let share = {
                let mut script = self.0.lock().unwrap();
                script.requests.push(request.clone());
                script.shares.pop_front().unwrap_or(Share::Wait)
            };
            match share {
                Share::Deny => {
                    sink.awaiting_selection();
                    return ShareEnd::Denied(fm_io_linux::wayland::DENIED_CANCELLED.into());
                }
                Share::Wait => sink.awaiting_selection(),
                Share::Grant(token) => sink.granted(token.map(str::to_owned), None),
            }
            let data: Vec<u8> = (0..16).flat_map(|_| [0, 0, 255, 0]).collect();
            while !sink.stopping() {
                if matches!(share, Share::Grant(_)) {
                    sink.frame(&SourcePicture {
                        data: &data,
                        width: 4,
                        height: 4,
                        stride: 16,
                        layout: PixelLayout::Bgrx,
                    });
                }
                sink.idle();
                std::thread::sleep(Duration::from_millis(5));
            }
            ShareEnd::Stopped
        }
    }

    fn input(id: u128) -> InputId {
        InputId::new(NonZeroU128::new(id).unwrap())
    }

    fn sources(shares: Vec<Share>) -> (PortalSources, Arc<Mutex<Script>>) {
        let script = Arc::new(Mutex::new(Script {
            shares: shares.into(),
            requests: Vec::new(),
        }));
        let fake = Arc::clone(&script);
        let factory: PortalFactory =
            Arc::new(move || Box::new(Fake(Arc::clone(&fake))) as Box<dyn PortalBackend>);
        let format = Format {
            rate: FrameRate::new(30, 1).unwrap(),
            clock_domain: ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            dimensions: (8, 8),
            sample_rate: SampleRate::new(48_000).unwrap(),
        };
        (PortalSources::new(factory, format), script)
    }

    fn window() -> ScreenCaptureConfig {
        ScreenCaptureConfig {
            slate: StallSlate::Black,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::picked()))
        }
    }

    fn settle(
        sources: &mut PortalSources,
        input: InputId,
        state: impl Fn(ScreenCaptureState) -> bool,
    ) -> ScreenCaptureStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        loop {
            for (reported, status) in sources.statuses(Instant::now()) {
                if reported == input {
                    last = Some(status);
                }
            }
            if let Some(status) = last.as_ref().filter(|status| state(status.state)) {
                return status.clone();
            }
            assert!(Instant::now() < deadline, "{last:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_share_waits_for_the_pick_goes_live_and_keeps_its_token() {
        let (mut sources, script) = sources(vec![Share::Wait]);
        sources.set_restore_tokens(BTreeMap::from([(input(1), "old".to_owned())]));
        sources.sync(&[(input(1), window())], |_| false);
        assert_eq!(sources.inputs(), vec![input(1)]);
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        assert_eq!(
            script.lock().unwrap().requests[0],
            ShareRequest {
                kind: SourceKind::Window,
                cursor: true,
                restore_token: Some("old".into())
            }
        );
        assert_eq!(sources.take_restore_tokens(), None);

        // Pick again: the token is forgotten and the dialog asks anew.
        script
            .lock()
            .unwrap()
            .shares
            .push_back(Share::Grant(Some("new")));
        assert!(sources.pick(input(1)));
        assert!(!sources.pick(input(2)));
        assert_eq!(sources.take_restore_tokens(), Some(BTreeMap::new()));
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Live
        });
        assert_eq!((status.width, status.height, status.reopens), (4, 4, 1));
        assert_eq!(status.audio, ScreenCaptureAudioState::Off);
        assert_eq!(script.lock().unwrap().requests[1].restore_token, None);
        assert_eq!(
            sources.take_restore_tokens(),
            Some(BTreeMap::from([(input(1), "new".to_owned())]))
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        for frame in 1.. {
            let pulled = sources.pull(frame);
            if pulled.into_iter().any(|stream| stream.video.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "no picture");
            std::thread::sleep(Duration::from_millis(5));
        }

        // Closing shows a blank picture and stops the share.
        sources.sync(&[(input(1), window())], |_| true);
        let pulled = sources.pull(1_000_000);
        assert!(pulled[0].video.is_some());
        assert_eq!(
            settle(&mut sources, input(1), |state| state
                == ScreenCaptureState::Idle)
            .state,
            ScreenCaptureState::Idle
        );
    }

    #[test]
    fn a_denied_share_waits_for_a_pick_and_a_new_kind_forgets_the_token() {
        let (mut sources, script) = sources(vec![Share::Deny]);
        sources.set_restore_tokens(BTreeMap::from([(input(1), "old".to_owned())]));
        sources.sync(&[(input(1), window())], |_| false);
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Denied
        });
        assert!(
            status
                .last_error
                .is_some_and(|error| error.starts_with("sharing was cancelled"))
        );
        std::thread::sleep(Duration::from_millis(100));
        sources.statuses(Instant::now());
        assert_eq!(script.lock().unwrap().requests.len(), 1, "not asked again");

        // A screen instead of a window: the window's token is no use.
        // Its system audio is reported unsupported (a portal capture is
        // silent).
        let screen = ScreenCaptureConfig {
            audio: ScreenCaptureAudio::System,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 })
        };
        sources.sync(&[(input(1), screen)], |_| false);
        assert_eq!(sources.take_restore_tokens(), Some(BTreeMap::new()));
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        assert_eq!(status.audio, ScreenCaptureAudioState::Unsupported);
        assert_eq!(status.audio_error.as_deref(), Some(NO_PORTAL_AUDIO));
        let script = script.lock().unwrap();
        assert_eq!(script.requests[1].kind, SourceKind::Monitor);
        assert_eq!(script.requests[1].restore_token, None);
        drop(script);

        // A removed input's token goes with it.
        sources.set_restore_tokens(BTreeMap::from([(input(1), "kept".to_owned())]));
        sources.sync(&[], |_| false);
        assert_eq!(sources.take_restore_tokens(), Some(BTreeMap::new()));
        assert_eq!(sources.inputs(), Vec::<InputId>::new());
    }
}
