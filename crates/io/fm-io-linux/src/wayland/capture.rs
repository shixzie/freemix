//! One Wayland screen capture input's share, on its own thread (IN-011).
//!
//! [`PortalCapture::start`] runs shares through a [`PortalBackend`] until
//! it is dropped: it asks for the share (the desktop may show its dialog:
//! `awaiting_selection`), turns each shared picture into the program
//! picture (cropped, letterboxed, at most the capture rate), and keeps the
//! newest restore token the desktop granted. A share the operator or the
//! desktop refuses or stops is `denied` and is not asked for again; a
//! target that goes away (a closed window, an ended stream) is
//! `target_gone` for a second and asked for again (with the restore token,
//! usually without a dialog); a stream the desktop pauses is
//! `target_gone` until it resumes, without asking again; a failure is
//! retried with backoff
//! (`reopening`). Where asking again opens the desktop's sharing dialog
//! (no restore token), a share that ended or failed after asking waits
//! [`DIALOG_RETRY_MIN`] and longer (`reopening`, its error saying the
//! dialog opens again), so the operator is not prompted every few seconds. The owner polls [`PortalCapture::status`],
//! [`PortalCapture::take_picture`], and
//! [`PortalCapture::take_restore_token`]; none of them waits on the
//! desktop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fm_types::screen_capture::{
    ScreenCaptureCrop, ScreenCaptureState, ScreenCaptureStatus, bound_screen_capture_error,
    is_screen_capture_restore_token,
};

use super::convert::{SourcePicture, letterbox};
use super::{PortalBackend, ShareEnd, ShareRequest, ShareSink};

/// The first and the longest wait before a failed share is asked for
/// again.
pub const RETRY_MIN: Duration = Duration::from_millis(500);
pub const RETRY_MAX: Duration = Duration::from_secs(5);
/// How long a gone target waits before it is asked for again.
pub const GONE_WAIT: Duration = Duration::from_secs(1);
/// The first and the longest wait before a share that ended or failed is
/// asked for again when asking opens the sharing dialog (no restore
/// token): the operator is not prompted every few seconds.
pub const DIALOG_RETRY_MIN: Duration = Duration::from_secs(30);
pub const DIALOG_RETRY_MAX: Duration = Duration::from_secs(300);

/// What a capture makes of each shared picture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureSettings {
    /// Trimmed from the shared picture's edges before it is fitted.
    pub crop: ScreenCaptureCrop,
    /// The most pictures per second converted (at least one).
    pub fps: u32,
    /// The program picture's size.
    pub width: u32,
    pub height: u32,
}

struct State {
    status: ScreenCaptureStatus,
    picture: Option<Vec<u8>>,
    token: Option<String>,
    token_changed: bool,
    settings: CaptureSettings,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
    wake: Condvar,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Waits up to `duration` unless the capture is dropped meanwhile;
    /// returns whether it was.
    fn wait(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut state = self.state();
        loop {
            if self.stopping() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn set(&self, update: impl FnOnce(&mut ScreenCaptureStatus)) {
        update(&mut self.state().status);
    }
}

/// One input's share of a Wayland desktop, running until dropped.
pub struct PortalCapture {
    shared: Arc<Shared>,
}

impl PortalCapture {
    /// Starts sharing what `request` asks for through `backend`, making
    /// program pictures as `settings` say.
    #[must_use]
    pub fn start(
        backend: Box<dyn PortalBackend>,
        request: ShareRequest,
        settings: CaptureSettings,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                status: ScreenCaptureStatus {
                    state: ScreenCaptureState::Opening,
                    ..ScreenCaptureStatus::default()
                },
                picture: None,
                token: request
                    .restore_token
                    .clone()
                    .filter(|token| is_screen_capture_restore_token(token)),
                token_changed: false,
                settings,
            }),
            stop: AtomicBool::new(false),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("freemixd-portal-capture".into())
            .spawn(move || run(backend, &worker, &request));
        if let Err(error) = spawned {
            shared.set(|status| {
                status.state = ScreenCaptureState::Failed;
                status.last_error = Some(bound_screen_capture_error(&format!(
                    "the capture thread could not start: {error}"
                )));
            });
        }
        Self { shared }
    }

    /// Changes the crop, rate, or program size of the pictures to come.
    pub fn set_settings(&self, settings: CaptureSettings) {
        self.shared.state().settings = settings;
    }

    /// The capture's status now.
    #[must_use]
    pub fn status(&self) -> ScreenCaptureStatus {
        self.shared.state().status.clone()
    }

    /// The newest program picture (RGBA at the program size) not taken yet.
    #[must_use]
    pub fn take_picture(&self) -> Option<Vec<u8>> {
        self.shared.state().picture.take()
    }

    /// The restore token the desktop last granted, once after each change
    /// (`Some(None)` when the desktop granted a share without one).
    #[must_use]
    pub fn take_restore_token(&self) -> Option<Option<String>> {
        let mut state = self.shared.state();
        std::mem::take(&mut state.token_changed).then(|| state.token.clone())
    }
}

impl Drop for PortalCapture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Wake a backoff wait; a share in progress sees the flag itself.
        let _state = self.shared.state();
        self.shared.wake.notify_all();
    }
}

/// The shares of one capture, until it is dropped or refused.
fn run(mut backend: Box<dyn PortalBackend>, shared: &Shared, request: &ShareRequest) {
    let mut attempt = 0_u32;
    let mut restarts = 0_u32;
    loop {
        let restore_token = shared.state().token.clone();
        shared.set(|status| {
            *status = ScreenCaptureStatus {
                state: ScreenCaptureState::Opening,
                reopens: restarts,
                ..ScreenCaptureStatus::default()
            };
        });
        let mut sink = Sink {
            shared,
            last_picture: None,
            pending: None,
            delivered: false,
            asked: false,
        };
        let end = backend.share(
            &ShareRequest {
                restore_token,
                ..request.clone()
            },
            &mut sink,
        );
        if sink.delivered {
            attempt = 0;
        }
        // The desktop was asked, and asking again carries no restore token:
        // its sharing dialog would open again.
        let dialog = sink.asked && shared.state().token.is_none();
        let wait = match end {
            ShareEnd::Stopped => return,
            ShareEnd::Denied(reason) => {
                shared.set(|status| {
                    *status = ScreenCaptureStatus {
                        state: ScreenCaptureState::Denied,
                        reopens: restarts,
                        last_error: Some(bound_screen_capture_error(&reason)),
                        ..ScreenCaptureStatus::default()
                    };
                });
                return;
            }
            ShareEnd::Unsupported(reason) => {
                shared.set(|status| {
                    *status = ScreenCaptureStatus {
                        state: ScreenCaptureState::Unsupported,
                        reopens: restarts,
                        last_error: Some(bound_screen_capture_error(&reason)),
                        ..ScreenCaptureStatus::default()
                    };
                });
                return;
            }
            ShareEnd::Ended(reason) if !dialog => {
                shared.set(|status| {
                    *status = ScreenCaptureStatus {
                        state: ScreenCaptureState::TargetGone,
                        reopens: restarts,
                        last_error: Some(bound_screen_capture_error(&reason)),
                        ..ScreenCaptureStatus::default()
                    };
                });
                GONE_WAIT
            }
            ShareEnd::Ended(error) | ShareEnd::Failed(error) => {
                attempt = attempt.saturating_add(1);
                let (first, longest) = if dialog {
                    (DIALOG_RETRY_MIN, DIALOG_RETRY_MAX)
                } else {
                    (RETRY_MIN, RETRY_MAX)
                };
                let backoff = first.saturating_mul(1 << (attempt - 1).min(8)).min(longest);
                let error = if dialog {
                    format!("{}: {error}", super::DIALOG_AGAIN)
                } else {
                    error
                };
                shared.set(|status| {
                    *status = ScreenCaptureStatus {
                        state: ScreenCaptureState::Reopening {
                            attempt,
                            next_in_ms: u32::try_from(backoff.as_millis()).unwrap_or(u32::MAX),
                        },
                        reopens: restarts,
                        last_error: Some(bound_screen_capture_error(&error)),
                        ..ScreenCaptureStatus::default()
                    };
                });
                backoff
            }
        };
        if shared.wait(wait) {
            return;
        }
        restarts = restarts.saturating_add(1);
    }
}

/// A raw picture held back by the rate cap, converted when it is due.
struct Pending {
    data: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
    layout: super::PixelLayout,
}

struct Sink<'a> {
    shared: &'a Shared,
    last_picture: Option<Instant>,
    pending: Option<Pending>,
    delivered: bool,
    /// The share asked the desktop (`Start`, which may show its dialog).
    asked: bool,
}

impl Sink<'_> {
    /// Whether a picture may be converted now under the rate cap.
    fn due(&self, fps: u32) -> bool {
        self.last_picture.is_none_or(|last| {
            last.elapsed() + Duration::from_millis(2) >= Duration::from_secs(1) / fps.max(1)
        })
    }

    fn convert(&mut self, picture: &SourcePicture<'_>) {
        let settings = self.shared.state().settings;
        self.last_picture = Some(Instant::now());
        self.pending = None;
        let region = settings.crop.region(picture.width, picture.height);
        let converted =
            region.and_then(|region| letterbox(picture, region, settings.width, settings.height));
        let mut state = self.shared.state();
        let status = &mut state.status;
        status.width = picture.width;
        status.height = picture.height;
        status.state = ScreenCaptureState::Live;
        if status.last_error.as_deref() == Some(super::PAUSED_ON_DESKTOP) {
            status.last_error = None;
        }
        match converted {
            Some(converted) => {
                if status.last_error.as_deref() == Some(CROP_KEEPS_NOTHING) {
                    status.last_error = None;
                }
                state.picture = Some(converted);
                self.delivered = true;
            }
            None if region.is_none() => {
                status.last_error = Some(CROP_KEEPS_NOTHING.to_owned());
            }
            None => {}
        }
    }
}

/// Why a live share shows nothing new.
const CROP_KEEPS_NOTHING: &str = "the crop keeps nothing of the shared picture";

impl ShareSink for Sink<'_> {
    fn awaiting_selection(&mut self) {
        self.asked = true;
        self.shared.set(|status| {
            status.state = ScreenCaptureState::AwaitingSelection;
            status.last_error = None;
        });
    }

    fn granted(&mut self, restore_token: Option<String>, note: Option<&str>) {
        let mut state = self.shared.state();
        let token = restore_token.filter(|token| is_screen_capture_restore_token(token));
        if state.token != token {
            state.token = token;
            state.token_changed = true;
        }
        state.status.state = ScreenCaptureState::Opening;
        state.status.last_error = note.map(bound_screen_capture_error);
    }

    fn streaming(&mut self) {
        self.shared.set(|status| {
            status.state = ScreenCaptureState::Live;
            if status.last_error.as_deref() == Some(super::PAUSED_ON_DESKTOP) {
                status.last_error = None;
            }
        });
    }

    fn paused(&mut self) {
        // Nothing comes until the desktop resumes: the slate shows, as for
        // a target gone, but the share is kept.
        self.pending = None;
        self.shared.set(|status| {
            status.state = ScreenCaptureState::TargetGone;
            status.last_error = Some(super::PAUSED_ON_DESKTOP.to_owned());
        });
    }

    fn frame(&mut self, picture: &SourcePicture<'_>) {
        let fps = self.shared.state().settings.fps;
        if self.due(fps) {
            self.convert(picture);
            return;
        }
        // Held back by the rate cap: keep the newest one for later, so the
        // last change on a desktop that then stays still is shown.
        if picture.is_complete() {
            let pending = self.pending.get_or_insert_with(|| Pending {
                data: Vec::new(),
                width: 0,
                height: 0,
                stride: 0,
                layout: picture.layout,
            });
            let rows = usize::try_from(picture.height).unwrap_or(0);
            let length =
                picture.stride * (rows - 1) + usize::try_from(picture.width).unwrap_or(0) * 4;
            pending.data.clear();
            pending.data.extend_from_slice(&picture.data[..length]);
            pending.width = picture.width;
            pending.height = picture.height;
            pending.stride = picture.stride;
            pending.layout = picture.layout;
        }
    }

    fn stopping(&self) -> bool {
        self.shared.stopping()
    }

    fn idle(&mut self) {
        let fps = self.shared.state().settings.fps;
        if self.pending.is_none() || !self.due(fps) {
            return;
        }
        if let Some(pending) = self.pending.take() {
            let picture = SourcePicture {
                data: &pending.data,
                width: pending.width,
                height: pending.height,
                stride: pending.stride,
                layout: pending.layout,
            };
            self.convert(&picture);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use fm_types::screen_capture::ScreenCaptureCrop;

    use super::super::{PixelLayout, SourceKind};
    use super::*;

    /// One scripted share: what the desktop does, in order.
    #[derive(Clone, Debug)]
    enum Step {
        Await,
        Grant(Option<&'static str>),
        Frame(u32, u32, [u8; 3]),
        Pause,
        Resume,
        End(ShareEnd),
    }

    struct Scripted {
        shares: VecDeque<Vec<Step>>,
        requests: Arc<Mutex<Vec<ShareRequest>>>,
    }

    impl PortalBackend for Scripted {
        fn share(&mut self, request: &ShareRequest, sink: &mut dyn ShareSink) -> ShareEnd {
            self.requests.lock().unwrap().push(request.clone());
            let Some(steps) = self.shares.pop_front() else {
                // Nothing more scripted: wait to be dropped.
                while !sink.stopping() {
                    sink.idle();
                    std::thread::sleep(Duration::from_millis(5));
                }
                return ShareEnd::Stopped;
            };
            for step in steps {
                match step {
                    Step::Await => sink.awaiting_selection(),
                    Step::Grant(token) => sink.granted(token.map(str::to_owned), None),
                    Step::Frame(width, height, [r, g, b]) => {
                        let data: Vec<u8> =
                            (0..width * height).flat_map(|_| [b, g, r, 0]).collect();
                        sink.frame(&SourcePicture {
                            data: &data,
                            width,
                            height,
                            stride: width as usize * 4,
                            layout: PixelLayout::Bgrx,
                        });
                    }
                    Step::Pause => sink.paused(),
                    Step::Resume => sink.streaming(),
                    Step::End(end) => return end,
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            while !sink.stopping() {
                sink.idle();
                std::thread::sleep(Duration::from_millis(5));
            }
            ShareEnd::Stopped
        }
    }

    fn settings() -> CaptureSettings {
        CaptureSettings {
            crop: ScreenCaptureCrop::NONE,
            fps: 60,
            width: 8,
            height: 4,
        }
    }

    fn start(
        shares: Vec<Vec<Step>>,
        token: Option<&str>,
    ) -> (PortalCapture, Arc<Mutex<Vec<ShareRequest>>>) {
        start_with(shares, token, settings())
    }

    fn start_with(
        shares: Vec<Vec<Step>>,
        token: Option<&str>,
        settings: CaptureSettings,
    ) -> (PortalCapture, Arc<Mutex<Vec<ShareRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = PortalCapture::start(
            Box::new(Scripted {
                shares: shares.into(),
                requests: Arc::clone(&requests),
            }),
            ShareRequest {
                kind: SourceKind::Window,
                cursor: true,
                restore_token: token.map(str::to_owned),
            },
            settings,
        );
        (capture, requests)
    }

    fn settle(capture: &PortalCapture, done: impl Fn(&ScreenCaptureStatus) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = capture.status();
            if done(&status) {
                return;
            }
            assert!(Instant::now() < deadline, "{status:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_granted_share_waits_for_the_pick_then_goes_live_with_its_token() {
        let (capture, requests) = start(
            vec![vec![
                Step::Await,
                Step::Await,
                Step::Grant(Some("token-2")),
                Step::Frame(4, 2, [255, 0, 0]),
            ]],
            Some("token-1"),
        );
        settle(&capture, |status| {
            status.state == ScreenCaptureState::AwaitingSelection
        });
        settle(&capture, |status| status.state == ScreenCaptureState::Live);
        let status = capture.status();
        assert_eq!((status.width, status.height), (4, 2));
        // The last grant's token was asked with; the new one is kept.
        assert_eq!(
            requests.lock().unwrap()[0].restore_token.as_deref(),
            Some("token-1")
        );
        assert_eq!(
            capture.take_restore_token(),
            Some(Some("token-2".to_owned()))
        );
        assert_eq!(capture.take_restore_token(), None, "once per change");
        let picture = capture.take_picture().expect("a picture");
        assert_eq!(picture.len(), 8 * 4 * 4);
        // 4x2 in 8x4: fills it, red and opaque.
        assert_eq!(&picture[..4], &[255, 0, 0, 255]);
        assert_eq!(capture.take_picture(), None);
    }

    #[test]
    fn a_cancelled_dialog_is_denied_and_not_asked_again() {
        let (capture, requests) = start(
            vec![vec![
                Step::Await,
                Step::End(ShareEnd::Denied(super::super::DENIED_CANCELLED.into())),
            ]],
            None,
        );
        settle(&capture, |status| {
            status.state == ScreenCaptureState::Denied
        });
        std::thread::sleep(GONE_WAIT + Duration::from_millis(200));
        let status = capture.status();
        assert_eq!(status.state, ScreenCaptureState::Denied);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("sharing was cancelled")),
            "{status:?}"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_gone_target_and_a_failure_are_asked_for_again_with_the_newest_token() {
        let (capture, requests) = start(
            vec![
                vec![
                    Step::Grant(Some("first")),
                    Step::Frame(4, 2, [0, 255, 0]),
                    Step::End(ShareEnd::Ended("the shared window closed".into())),
                ],
                vec![Step::End(ShareEnd::Failed("no PipeWire".into()))],
                vec![Step::Grant(None), Step::Frame(2, 2, [0, 0, 255])],
            ],
            None,
        );
        settle(&capture, |status| {
            status.state == ScreenCaptureState::TargetGone
        });
        assert_eq!(
            capture.status().last_error.as_deref(),
            Some("the shared window closed")
        );
        settle(&capture, |status| {
            matches!(
                status.state,
                ScreenCaptureState::Reopening { attempt: 1, .. }
            )
        });
        settle(&capture, |status| {
            status.state == ScreenCaptureState::Live && status.width == 2
        });
        assert_eq!(capture.status().reopens, 2);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].restore_token, None);
        assert_eq!(requests[1].restore_token.as_deref(), Some("first"));
        assert_eq!(requests[2].restore_token.as_deref(), Some("first"));
        drop(requests);
        // The last grant kept no token: forgotten.
        assert_eq!(capture.take_restore_token(), Some(None));
    }

    #[test]
    fn a_paused_stream_shows_the_slate_until_it_resumes_without_asking_again() {
        let (capture, requests) = start(
            vec![vec![
                Step::Grant(Some("token")),
                Step::Frame(4, 2, [255, 0, 0]),
                Step::Pause,
                Step::Pause,
                Step::Pause,
                Step::Pause,
                Step::Pause,
                Step::Resume,
            ]],
            None,
        );
        settle(&capture, |status| status.state == ScreenCaptureState::Live);
        settle(&capture, |status| {
            status.state == ScreenCaptureState::TargetGone
                && status.last_error.as_deref() == Some(super::super::PAUSED_ON_DESKTOP)
        });
        settle(&capture, |status| {
            status.state == ScreenCaptureState::Live && status.last_error.is_none()
        });
        assert_eq!(capture.status().reopens, 0);
        assert_eq!(requests.lock().unwrap().len(), 1, "the share was kept");
    }

    fn reopening(status: &ScreenCaptureStatus) -> Option<(u32, u32)> {
        match status.state {
            ScreenCaptureState::Reopening {
                attempt,
                next_in_ms,
            } => Some((attempt, next_in_ms)),
            _ => None,
        }
    }

    #[test]
    fn without_a_restore_token_the_dialog_is_not_reopened_every_few_seconds() {
        let dialog_min = u32::try_from(DIALOG_RETRY_MIN.as_millis()).unwrap();
        for end in [
            ShareEnd::Failed("PipeWire: the shared stream failed".into()),
            ShareEnd::Ended("the shared stream ended".into()),
        ] {
            let (capture, requests) = start(
                vec![vec![
                    Step::Await,
                    Step::Grant(None),
                    Step::Frame(4, 2, [255, 0, 0]),
                    Step::End(end),
                ]],
                None,
            );
            settle(&capture, |status| reopening(status).is_some());
            let status = capture.status();
            assert_eq!(reopening(&status), Some((1, dialog_min)), "{status:?}");
            assert!(
                status
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.starts_with(super::super::DIALOG_AGAIN)),
                "{status:?}"
            );
            std::thread::sleep(RETRY_MAX.min(GONE_WAIT) + Duration::from_millis(300));
            assert_eq!(requests.lock().unwrap().len(), 1, "not asked again yet");
        }

        // A failure before the desktop was asked shows no dialog: retried
        // soon.
        let (capture, requests) = start(
            vec![
                vec![Step::End(ShareEnd::Failed("no bus".into()))],
                vec![Step::Await, Step::Grant(None), Step::Frame(4, 2, [0, 0, 0])],
            ],
            None,
        );
        settle(&capture, |status| status.state == ScreenCaptureState::Live);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn unsupported_shares_stop_and_crops_apply_to_the_shared_picture() {
        let (capture, _) = start(
            vec![vec![Step::End(ShareEnd::Unsupported(
                super::super::NO_WINDOW_SHARING.into(),
            ))]],
            None,
        );
        settle(&capture, |status| {
            status.state == ScreenCaptureState::Unsupported
        });

        let (capture, _) = start_with(
            vec![vec![Step::Grant(None), Step::Frame(8, 4, [255, 255, 255])]],
            None,
            CaptureSettings {
                crop: ScreenCaptureCrop {
                    left: 4,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                ..settings()
            },
        );
        settle(&capture, |status| status.state == ScreenCaptureState::Live);
        let picture = capture.take_picture().unwrap();
        // The right 4x4 half fitted into 8x4: bars at the sides.
        assert_eq!(&picture[..4], &[0, 0, 0, 255]);
        let middle = (2 * 8 + 4) * 4;
        assert_eq!(&picture[middle..middle + 4], &[255, 255, 255, 255]);
        assert_eq!((capture.status().width, capture.status().height), (8, 4));
    }
}
