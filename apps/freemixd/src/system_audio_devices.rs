//! The server's system audio capture devices (AU-011, Protocol 2.68).
//!
//! A `system_audio_devices_request` is answered by `system_audio_devices`
//! with the same `request_id`. Listing asks the audio systems (`pactl`
//! and `/proc/asound` on Linux, see [`fm_io_linux::system_audio`]) and may
//! take a while, so it never runs on the session loop, which realizes
//! native frames: the loop hands each request to a worker thread
//! ([`SystemAudioDevices::submit`]) and sends the answers it finished on a
//! later pass ([`SystemAudioDevices::take_finished`]). One pending request
//! per peer is kept (a newer one replaces it), and a listing younger than
//! [`REUSE_FOR`] answers later requests, so however often clients ask the
//! audio systems are asked at most once per [`REUSE_FOR`].
//!
//! Only a native daemon on Linux captures system audio and so lists
//! devices; any other daemon answers `supported=0` with nothing listed.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fm_protocol::{SystemAudioDevicesMessage, SystemAudioInventory};

use crate::title_preview::LatestWorker;

/// How long one listing answers later requests.
pub(crate) const REUSE_FOR: Duration = Duration::from_secs(2);

/// Lists the capture devices of the server's audio systems. It may block;
/// it runs on the listing worker only.
pub(crate) trait SystemAudioEnumerator: Send + 'static {
    fn list(&mut self) -> SystemAudioInventory;
}

/// A daemon that does not capture system audio.
struct Unsupported;

impl SystemAudioEnumerator for Unsupported {
    fn list(&mut self) -> SystemAudioInventory {
        SystemAudioInventory::unsupported()
    }
}

#[cfg(all(feature = "native-media", target_os = "linux"))]
impl SystemAudioEnumerator for fm_io_linux::system_audio::SystemAudioLister {
    fn list(&mut self) -> SystemAudioInventory {
        Self::list(self)
    }
}

/// The enumerator of this daemon: the real audio systems on a native Linux
/// daemon, which captures system audio, and none elsewhere.
pub(crate) fn enumerator(native: bool) -> Box<dyn SystemAudioEnumerator> {
    #[cfg(all(feature = "native-media", target_os = "linux"))]
    if native {
        return Box::new(fm_io_linux::system_audio::SystemAudioLister::default());
    }
    let _ = native;
    Box::new(Unsupported)
}

/// The listing worker of a session loop.
pub(crate) struct SystemAudioDevices {
    worker: LatestWorker<String, SystemAudioDevicesMessage>,
    /// Answers a peer could not take yet (its outbound queue was full or
    /// it was resynchronizing), offered again on the next pass.
    deferred: RefCell<BTreeMap<u64, SystemAudioDevicesMessage>>,
}

impl SystemAudioDevices {
    /// Starts the worker listing with `enumerator`, reusing a listing for
    /// `reuse_for`.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub(crate) fn spawn(
        mut enumerator: Box<dyn SystemAudioEnumerator>,
        reuse_for: Duration,
    ) -> std::io::Result<Self> {
        let mut listed: Option<(Instant, SystemAudioInventory)> = None;
        let worker = LatestWorker::spawn("freemixd-system-audio-devices", move |request_id| {
            let inventory = match &listed {
                Some((at, inventory)) if at.elapsed() < reuse_for => inventory.clone(),
                _ => {
                    let inventory = enumerator.list().fitted();
                    listed = Some((Instant::now(), inventory.clone()));
                    inventory
                }
            };
            SystemAudioDevicesMessage {
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
    /// Never waits for a listing.
    pub(crate) fn submit(&self, peer: u64, request_id: String) {
        self.worker.submit(peer, request_id);
    }

    /// The answers finished since the last call and those deferred, one
    /// per peer (the newest).
    pub(crate) fn take_finished(&self) -> BTreeMap<u64, SystemAudioDevicesMessage> {
        let mut answers = std::mem::take(&mut *self.deferred.borrow_mut());
        answers.extend(self.worker.take_finished());
        answers
    }

    /// Offers `answer` to `peer` again on the next pass, unless a newer
    /// one is already waiting.
    pub(crate) fn defer(&self, peer: u64, answer: SystemAudioDevicesMessage) {
        self.deferred.borrow_mut().entry(peer).or_insert(answer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use fm_protocol::{
        SystemAudioBackend, SystemAudioBackendError, SystemAudioDevice, WireMessage, decode_line,
        encode_line,
    };

    use super::*;

    /// Lists two devices out of order (and one unusable), counting calls;
    /// each call waits for a release so a test can hold a listing.
    struct Fake {
        calls: Arc<AtomicUsize>,
        release: Option<mpsc::Receiver<()>>,
    }

    impl SystemAudioEnumerator for Fake {
        fn list(&mut self) -> SystemAudioInventory {
            if let Some(release) = &self.release {
                release.recv().unwrap();
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let device = |backend, device: &str| SystemAudioDevice {
                backend,
                device: device.into(),
                description: format!("  Device {call}\n"),
                channels: Some(2),
                default: false,
                monitor: false,
            };
            SystemAudioInventory {
                supported: true,
                devices: vec![
                    device(SystemAudioBackend::Alsa, "plughw:Loopback,1"),
                    device(SystemAudioBackend::Pulse, "sink.monitor"),
                    device(SystemAudioBackend::Pulse, "-bad"),
                ],
                errors: vec![SystemAudioBackendError {
                    backend: SystemAudioBackend::Alsa,
                    error: format!("listing {call}"),
                }],
            }
        }
    }

    /// Collects answers until every one of `peers` has one.
    fn wait_for(
        devices: &SystemAudioDevices,
        peers: &[u64],
    ) -> BTreeMap<u64, SystemAudioDevicesMessage> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answers = BTreeMap::new();
        while !peers.iter().all(|peer| answers.contains_key(peer)) {
            assert!(Instant::now() < deadline, "the worker did not answer");
            answers.extend(devices.take_finished());
            std::thread::sleep(Duration::from_millis(1));
        }
        answers
    }

    /// Requests are answered off the caller with their own request ids and
    /// a list fitted for the wire; a listing answers later requests until
    /// it is older than the reuse bound.
    #[test]
    fn requests_are_answered_off_the_loop_and_listings_are_reused() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, released) = mpsc::channel();
        let devices = SystemAudioDevices::spawn(
            Box::new(Fake {
                calls: Arc::clone(&calls),
                release: Some(released),
            }),
            Duration::from_secs(1),
        )
        .unwrap();
        // The listing is held: submitting returns at once, and the newer of
        // peer 1's waiting requests replaces the older.
        let submitted = Instant::now();
        devices.submit(1, "a".into());
        devices.submit(2, "b".into());
        devices.submit(1, "c".into());
        assert!(submitted.elapsed() < Duration::from_secs(1));
        assert_eq!(devices.take_finished(), BTreeMap::new());
        release.send(()).unwrap();
        let answers = wait_for(&devices, &[1, 2]);
        let ids: Vec<(u64, &str)> = answers
            .iter()
            .map(|(peer, answer)| (*peer, answer.request_id.as_str()))
            .collect();
        assert!(
            ids == [(1, "a"), (2, "b")] || ids == [(1, "c"), (2, "b")],
            "{ids:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing answers both");
        let inventory = &answers[&2].inventory;
        let names: Vec<&str> = inventory
            .devices
            .iter()
            .map(|device| device.device.as_str())
            .collect();
        assert_eq!(names, ["sink.monitor", "plughw:Loopback,1"]);
        assert_eq!(inventory.devices[0].description, "Device 1");
        assert_eq!(
            inventory.error_of(SystemAudioBackend::Alsa),
            Some("listing 1")
        );
        // Every answer encodes and decodes exactly.
        for answer in answers.into_values() {
            let message = WireMessage::SystemAudioDevices(answer);
            assert_eq!(
                decode_line(&encode_line(&message).unwrap()).unwrap(),
                message
            );
        }
        // A young listing answers a later request; an old one is listed
        // again.
        devices.submit(3, "d".into());
        assert_eq!(wait_for(&devices, &[3])[&3].request_id, "d");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a young listing is reused");
        std::thread::sleep(Duration::from_millis(1_100));
        release.send(()).unwrap();
        devices.submit(3, "e".into());
        let answer = &wait_for(&devices, &[3])[&3];
        assert_eq!(answer.request_id, "e");
        assert_eq!(answer.inventory.devices[0].description, "Device 2");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// An answer a peer could not take is offered again, and a newer one
    /// replaces it.
    #[test]
    fn deferred_answers_are_offered_again() {
        let devices = SystemAudioDevices::spawn(enumerator(false), REUSE_FOR).unwrap();
        devices.submit(4, "first".into());
        let answer = wait_for(&devices, &[4]).remove(&4).unwrap();
        devices.defer(4, answer);
        assert_eq!(devices.take_finished()[&4].request_id, "first");
        assert_eq!(devices.take_finished(), BTreeMap::new());
        devices.submit(4, "second".into());
        let answer = wait_for(&devices, &[4]).remove(&4).unwrap();
        devices.defer(4, answer.clone());
        devices.defer(
            4,
            SystemAudioDevicesMessage {
                request_id: "older".into(),
                ..answer
            },
        );
        assert_eq!(devices.take_finished()[&4].request_id, "second");
    }

    #[test]
    fn a_daemon_without_system_audio_lists_nothing() {
        let devices = SystemAudioDevices::spawn(enumerator(false), REUSE_FOR).unwrap();
        devices.submit(7, "sa".into());
        let answer = &wait_for(&devices, &[7])[&7];
        assert_eq!(answer.request_id, "sa");
        assert_eq!(answer.inventory, SystemAudioInventory::unsupported());
    }
}
