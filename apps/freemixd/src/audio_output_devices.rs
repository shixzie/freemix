//! The server's audio output devices for the headphone output (AU-001,
//! Protocol 2.102).
//!
//! An `audio_output_devices_request` is answered by `audio_output_devices`
//! with the same `request_id`. Listing reads the audio system (ALSA's
//! procfs on Linux, see [`fm_io_linux::audio_output`]), so like the system
//! audio device list it never runs on the session loop, which realizes
//! native frames: the loop hands each request to a worker thread
//! ([`AudioOutputDevices::submit`]) and sends the answers it finished on a
//! later pass ([`AudioOutputDevices::take_finished`]). One pending request
//! per peer is kept (a newer one replaces it), and a listing younger than
//! [`REUSE_FOR`] answers later requests.
//!
//! A native daemon on Linux lists its ALSA output devices. A simulated
//! daemon lists a fixed, deterministic set ([`AudioOutputInventory::simulated`])
//! so clients and tests see a picker without hardware; any other native
//! daemon (its headphone output plays only through ALSA) answers
//! `supported=0`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fm_protocol::{AudioOutputDevicesMessage, AudioOutputInventory};

use crate::title_preview::LatestWorker;

/// How long one listing answers later requests.
pub(crate) const REUSE_FOR: Duration = Duration::from_secs(2);

/// Lists the server's output devices. It may block; it runs on the
/// listing worker only.
pub(crate) trait AudioOutputEnumerator: Send + 'static {
    fn list(&mut self) -> AudioOutputInventory;
}

/// A native daemon whose platform has no headphone output device.
#[cfg_attr(all(feature = "native-media", target_os = "linux"), allow(dead_code))]
struct Unsupported;

impl AudioOutputEnumerator for Unsupported {
    fn list(&mut self) -> AudioOutputInventory {
        AudioOutputInventory::unsupported()
    }
}

/// A simulated daemon's fixed list.
struct Simulated;

impl AudioOutputEnumerator for Simulated {
    fn list(&mut self) -> AudioOutputInventory {
        AudioOutputInventory::simulated()
    }
}

#[cfg(all(feature = "native-media", target_os = "linux"))]
impl AudioOutputEnumerator for fm_io_linux::audio_output::AudioOutputLister {
    fn list(&mut self) -> AudioOutputInventory {
        Self::list(self)
    }
}

/// The enumerator of this daemon: ALSA on a native Linux daemon, nothing on
/// another native daemon, and the fixed list on a simulated one.
pub(crate) fn enumerator(native: bool) -> Box<dyn AudioOutputEnumerator> {
    if !native {
        return Box::new(Simulated);
    }
    #[cfg(all(feature = "native-media", target_os = "linux"))]
    return Box::new(fm_io_linux::audio_output::AudioOutputLister::default());
    #[cfg(not(all(feature = "native-media", target_os = "linux")))]
    Box::new(Unsupported)
}

/// The listing worker of a session loop.
pub(crate) struct AudioOutputDevices {
    worker: LatestWorker<String, AudioOutputDevicesMessage>,
    /// Answers a peer could not take yet, offered again on the next pass.
    deferred: RefCell<BTreeMap<u64, AudioOutputDevicesMessage>>,
}

impl AudioOutputDevices {
    /// Starts the worker listing with `enumerator`, reusing a listing for
    /// `reuse_for`.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub(crate) fn spawn(
        mut enumerator: Box<dyn AudioOutputEnumerator>,
        reuse_for: Duration,
    ) -> std::io::Result<Self> {
        let mut listed: Option<(Instant, AudioOutputInventory)> = None;
        let worker = LatestWorker::spawn("freemixd-audio-output-devices", move |request_id| {
            let inventory = match &listed {
                Some((at, inventory)) if at.elapsed() < reuse_for => inventory.clone(),
                _ => {
                    let inventory = enumerator.list().fitted();
                    listed = Some((Instant::now(), inventory.clone()));
                    inventory
                }
            };
            AudioOutputDevicesMessage {
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
    pub(crate) fn take_finished(&self) -> BTreeMap<u64, AudioOutputDevicesMessage> {
        let mut answers = std::mem::take(&mut *self.deferred.borrow_mut());
        answers.extend(self.worker.take_finished());
        answers
    }

    /// Offers `answer` to `peer` again on the next pass, unless a newer
    /// one is already waiting.
    pub(crate) fn defer(&self, peer: u64, answer: AudioOutputDevicesMessage) {
        self.deferred.borrow_mut().entry(peer).or_insert(answer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fm_protocol::{AudioOutputDevice, WireMessage, decode_line, encode_line};

    use super::*;

    /// Lists one device per call, out of order and with an unusable one,
    /// counting calls.
    struct Fake {
        calls: Arc<AtomicUsize>,
    }

    impl AudioOutputEnumerator for Fake {
        fn list(&mut self) -> AudioOutputInventory {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let device = |device: &str| AudioOutputDevice {
                device: device.into(),
                description: format!(" Output {call}\n"),
                channels: Some(2),
                default: false,
            };
            AudioOutputInventory {
                supported: true,
                devices: vec![
                    device("plughw:CARD=B,DEV=0"),
                    device("-bad"),
                    device("hw:A"),
                ],
                error: None,
            }
        }
    }

    fn wait_for(
        devices: &AudioOutputDevices,
        peers: &[u64],
    ) -> BTreeMap<u64, AudioOutputDevicesMessage> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answers = BTreeMap::new();
        while !peers.iter().all(|peer| answers.contains_key(peer)) {
            assert!(Instant::now() < deadline, "the worker did not answer");
            answers.extend(devices.take_finished());
            std::thread::sleep(Duration::from_millis(1));
        }
        answers
    }

    /// Requests are answered off the caller with a list fitted for the
    /// wire; a young listing answers later requests, and an answer a peer
    /// could not take is offered again.
    #[test]
    fn listings_are_fitted_reused_and_deferred() {
        let calls = Arc::new(AtomicUsize::new(0));
        let devices = AudioOutputDevices::spawn(
            Box::new(Fake {
                calls: Arc::clone(&calls),
            }),
            Duration::from_mins(1),
        )
        .unwrap();
        devices.submit(1, "a".into());
        let answer = wait_for(&devices, &[1]).remove(&1).unwrap();
        assert_eq!(answer.request_id, "a");
        let names: Vec<&str> = answer
            .inventory
            .devices
            .iter()
            .map(|device| device.device.as_str())
            .collect();
        assert_eq!(names, ["hw:A", "plughw:CARD=B,DEV=0"]);
        assert_eq!(answer.inventory.devices[0].description, "Output 1");
        let message = WireMessage::AudioOutputDevices(answer.clone());
        assert_eq!(
            decode_line(&encode_line(&message).unwrap()).unwrap(),
            message
        );
        devices.submit(2, "b".into());
        assert_eq!(wait_for(&devices, &[2])[&2].request_id, "b");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a young listing is reused");
        devices.defer(1, answer);
        assert_eq!(devices.take_finished()[&1].request_id, "a");
        assert_eq!(devices.take_finished(), BTreeMap::new());
    }

    /// A simulated daemon lists the fixed set; a native daemon off Linux
    /// lists nothing.
    #[test]
    fn simulated_daemons_list_the_fixed_set() {
        let devices = AudioOutputDevices::spawn(enumerator(false), REUSE_FOR).unwrap();
        devices.submit(7, "ao".into());
        assert_eq!(
            wait_for(&devices, &[7])[&7].inventory,
            AudioOutputInventory::simulated()
        );
        assert_eq!(Unsupported.list(), AudioOutputInventory::unsupported());
    }
}
