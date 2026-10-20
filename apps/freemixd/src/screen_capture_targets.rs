//! What the server can capture as screen capture inputs (IN-011).
//!
//! A `screen_capture_targets_request` is answered by
//! `screen_capture_targets` with the same `request_id`. Listing asks the
//! desktop (an X11 server, PowerShell, or `ScreenCaptureKit`, see
//! [`crate::desktop_capture`]) and may take a while, so it never runs on
//! the session loop: the loop hands each request to a worker thread
//! ([`ScreenCaptureTargets::submit`]) and sends the answers it finished on
//! a later pass ([`ScreenCaptureTargets::take_finished`]). One pending
//! request per peer is kept (a newer one replaces it), and a listing
//! younger than [`REUSE_FOR`] answers later requests.
//!
//! A native daemon lists its own desktop. A simulated daemon, which
//! captures nothing, answers with a fixed simulated desktop (one screen and
//! two windows, backend `simulated`) so clients can be exercised without a
//! desktop; its screen capture inputs show the simulated stand-in picture.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fm_protocol::{ScreenCaptureInventory, ScreenCaptureTargetsMessage, ScreenInfo, WindowInfo};

use crate::title_preview::LatestWorker;

/// How long one listing answers later requests.
pub(crate) const REUSE_FOR: Duration = Duration::from_secs(1);

/// The backend token of the simulated desktop.
pub(crate) const SIMULATED_BACKEND: &str = "simulated";

/// Lists what can be captured. It may block; it runs on the listing worker
/// only.
pub(crate) trait TargetEnumerator: Send + 'static {
    fn list(&mut self) -> ScreenCaptureInventory;
}

/// The fixed desktop of a simulated daemon.
struct Simulated;

/// The simulated desktop: one 1920x1080 screen and two windows.
pub(crate) fn simulated_inventory() -> ScreenCaptureInventory {
    let window = |id, app: &str, title: &str, x, y| WindowInfo {
        id,
        app: app.to_owned(),
        title: title.to_owned(),
        pid: None,
        x,
        y,
        width: 1280,
        height: 720,
    };
    ScreenCaptureInventory {
        supported: true,
        backend: SIMULATED_BACKEND.to_owned(),
        screens: vec![ScreenInfo {
            index: 0,
            name: "Simulated display".to_owned(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
        }],
        windows: vec![
            window(1, "Slides", "Keynote deck", 80, 60),
            window(2, "Browser", "Live scores", 560, 300),
        ],
        error: None,
    }
}

impl TargetEnumerator for Simulated {
    fn list(&mut self) -> ScreenCaptureInventory {
        simulated_inventory()
    }
}

/// A native daemon's own desktop.
#[cfg(feature = "native-media")]
struct Desktop(crate::desktop_capture::Lister);

#[cfg(feature = "native-media")]
impl TargetEnumerator for Desktop {
    fn list(&mut self) -> ScreenCaptureInventory {
        (self.0)().inventory
    }
}

/// The enumerator of this daemon: its desktop on a native daemon, the
/// simulated desktop elsewhere.
pub(crate) fn enumerator(native: bool) -> Box<dyn TargetEnumerator> {
    #[cfg(feature = "native-media")]
    if native {
        return Box::new(Desktop(
            crate::desktop_capture::DesktopBackend::platform().lister(),
        ));
    }
    let _ = native;
    Box::new(Simulated)
}

/// The listing worker of a session loop.
pub(crate) struct ScreenCaptureTargets {
    worker: LatestWorker<String, ScreenCaptureTargetsMessage>,
    /// Answers a peer could not take yet, offered again on the next pass.
    deferred: RefCell<BTreeMap<u64, ScreenCaptureTargetsMessage>>,
}

impl ScreenCaptureTargets {
    /// Starts the worker listing with `enumerator`, reusing a listing for
    /// `reuse_for`.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub(crate) fn spawn(
        mut enumerator: Box<dyn TargetEnumerator>,
        reuse_for: Duration,
    ) -> std::io::Result<Self> {
        let mut listed: Option<(Instant, ScreenCaptureInventory)> = None;
        let worker = LatestWorker::spawn("freemixd-screen-capture-targets", move |request_id| {
            let inventory = match &listed {
                Some((at, inventory)) if at.elapsed() < reuse_for => inventory.clone(),
                _ => {
                    let inventory = enumerator.list().fitted();
                    listed = Some((Instant::now(), inventory.clone()));
                    inventory
                }
            };
            ScreenCaptureTargetsMessage {
                request_id,
                inventory,
            }
        })?;
        Ok(Self {
            worker,
            deferred: RefCell::new(BTreeMap::new()),
        })
    }

    /// Queues `peer`'s request, replacing its request not yet started.
    pub(crate) fn submit(&self, peer: u64, request_id: String) {
        self.worker.submit(peer, request_id);
    }

    /// The answers finished since the last call and those deferred, one
    /// per peer (the newest).
    pub(crate) fn take_finished(&self) -> BTreeMap<u64, ScreenCaptureTargetsMessage> {
        let mut answers = std::mem::take(&mut *self.deferred.borrow_mut());
        answers.extend(self.worker.take_finished());
        answers
    }

    /// Offers `answer` to `peer` again on the next pass, unless a newer
    /// one is already waiting.
    pub(crate) fn defer(&self, peer: u64, answer: ScreenCaptureTargetsMessage) {
        self.deferred.borrow_mut().entry(peer).or_insert(answer);
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::{WireMessage, decode_line, encode_line};

    use super::*;

    fn wait_for(targets: &ScreenCaptureTargets, peer: u64) -> ScreenCaptureTargetsMessage {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(answer) = targets.take_finished().remove(&peer) {
                return answer;
            }
            assert!(Instant::now() < deadline, "the worker did not answer");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_simulated_daemon_lists_its_simulated_desktop() {
        let targets = ScreenCaptureTargets::spawn(enumerator(false), REUSE_FOR).unwrap();
        targets.submit(3, "t1".into());
        let answer = wait_for(&targets, 3);
        assert_eq!(answer.request_id, "t1");
        assert_eq!(answer.inventory, simulated_inventory());
        assert_eq!(answer.inventory.backend, SIMULATED_BACKEND);
        let message = WireMessage::ScreenCaptureTargets(answer.clone());
        assert_eq!(
            decode_line(&encode_line(&message).unwrap()).unwrap(),
            message
        );
        // A deferred answer is offered again; a newer one wins.
        targets.defer(3, answer.clone());
        targets.defer(
            3,
            ScreenCaptureTargetsMessage {
                request_id: "older".into(),
                ..answer
            },
        );
        assert_eq!(targets.take_finished()[&3].request_id, "t1");
    }
}
