use std::{
    io,
    net::{SocketAddr, UdpSocket},
    str,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use fm_ui_egui::CueKey;

const DATAGRAM_CAPACITY: usize = 128;
const ACTION_CAPACITY: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_millis(25);
/// The first pause after a receive error; each further error in a row
/// doubles it, up to [`MAX_ERROR_BACKOFF`].
const MIN_ERROR_BACKOFF: Duration = Duration::from_millis(10);
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OscAction {
    SelectPreview(u8),
    Cut,
    Fade,
    AlphaFade,
    Slide,
    Zoom,
    Wipe,
    FadeToBlack {
        active: bool,
    },
    /// `/freemix/switcher/manual/start/<kind>`: a mix token or
    /// `stinger_<slot>` (SW-004).
    StartManualTransition(fm_protocol::TransitionFavoriteEffect),
    /// `/freemix/switcher/manual/position` with one `i` argument, 0 through
    /// 10000 basis points (SW-004).
    SetManualTransitionPosition(fm_protocol::ManualTransitionPosition),
    CommitManualTransition,
    CancelManualTransition,
    /// `/freemix/cue/<go|previous|next|stop|reset|arm|disarm>`: a key of
    /// the Cues panel's shown list (RC-017). `/freemix/cue/arm` uses the
    /// panel's "Fire in" delay, or its one `i` argument in milliseconds.
    Cue(fm_ui_egui::CueKey),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OscCounters {
    pub(crate) malformed: u64,
    pub(crate) rejected: u64,
    pub(crate) overflow: u64,
    pub(crate) failed: u64,
}

#[derive(Default)]
struct SharedCounters {
    malformed: AtomicU64,
    rejected: AtomicU64,
    overflow: AtomicU64,
    failed: AtomicU64,
    /// Whether the worker still receives; cleared however it exits.
    listening: AtomicBool,
}

/// Clears [`SharedCounters::listening`] when the worker exits, even by a
/// panic, so a dead listener shows instead of silently ignoring OSC.
struct ListeningGuard<'a>(&'a AtomicBool);

impl Drop for ListeningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct OscReceiver {
    actions: Receiver<OscAction>,
    counters: Arc<SharedCounters>,
    cancel: Arc<AtomicBool>,
    wake_socket: UdpSocket,
    local_address: SocketAddr,
    repaint: egui::Context,
    worker: Option<JoinHandle<()>>,
}

impl OscReceiver {
    /// Listens on loopback `address`. Each queued action wakes `repaint`'s
    /// UI, which drains the queue in its next frame: OSC must not wait for
    /// an unrelated repaint, nor overflow while the console is idle.
    pub(crate) fn bind(address: SocketAddr, repaint: egui::Context) -> io::Result<Self> {
        validate_listen_address(address)?;
        let socket = UdpSocket::bind(address)?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;
        let local_address = socket.local_addr()?;
        let wake_socket = socket.try_clone()?;
        let (action_sender, actions) = mpsc::sync_channel(ACTION_CAPACITY);
        let counters = Arc::new(SharedCounters::default());
        counters.listening.store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_counters = Arc::clone(&counters);
        let worker_cancel = Arc::clone(&cancel);
        let worker_repaint = repaint.clone();
        let worker = thread::Builder::new()
            .name("freemix-osc".to_owned())
            .spawn(move || {
                let _listening = ListeningGuard(&worker_counters.listening);
                receive_loop(
                    &mut |datagram| socket.recv_from(datagram),
                    &mut thread::sleep,
                    &action_sender,
                    &worker_counters,
                    &worker_cancel,
                    &worker_repaint,
                );
            })?;

        Ok(Self {
            actions,
            counters,
            cancel,
            wake_socket,
            local_address,
            repaint,
            worker: Some(worker),
        })
    }

    /// Asks for another UI frame, for actions still queued after a drain.
    pub(crate) fn request_repaint(&self) {
        self.repaint.request_repaint();
    }

    #[cfg(test)]
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_address
    }

    pub(crate) fn try_recv(&self) -> Result<OscAction, TryRecvError> {
        self.actions.try_recv()
    }

    pub(crate) fn counters(&self) -> OscCounters {
        OscCounters {
            malformed: self.counters.malformed.load(Ordering::Relaxed),
            rejected: self.counters.rejected.load(Ordering::Relaxed),
            overflow: self.counters.overflow.load(Ordering::Relaxed),
            failed: self.counters.failed.load(Ordering::Relaxed),
        }
    }

    /// Whether the listener still receives OSC. A receive error only
    /// pauses it; it stops for good only if its worker exits, which the
    /// operator must see.
    pub(crate) fn is_listening(&self) -> bool {
        self.counters.listening.load(Ordering::Acquire)
    }

    pub(crate) fn shutdown(mut self) -> thread::Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> thread::Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        self.cancel.store(true, Ordering::Release);
        let _ = self.wake_socket.send_to(&[], self.local_address);
        worker.join()
    }
}

impl Drop for OscReceiver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn validate_listen_address(address: SocketAddr) -> io::Result<()> {
    if address.ip().is_loopback() && address.port() != 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OSC listen address must be loopback with a nonzero port",
        ))
    }
}

/// Reads one datagram into the buffer: its length and sender.
type ReceiveFn<'a> = dyn FnMut(&mut [u8]) -> io::Result<(usize, SocketAddr)> + 'a;

/// Receives datagrams until `cancel` is set or the UI drops its queue.
/// An interrupted receive retries at once; any other error is counted,
/// logged once per run of errors, and retried after a growing `pause`, so
/// a transient socket fault never kills the listener.
fn receive_loop(
    receive: &mut ReceiveFn<'_>,
    pause: &mut dyn FnMut(Duration),
    actions: &SyncSender<OscAction>,
    counters: &SharedCounters,
    cancel: &AtomicBool,
    repaint: &egui::Context,
) {
    let mut datagram = [0_u8; DATAGRAM_CAPACITY];
    let mut backoff: Option<Duration> = None;
    while !cancel.load(Ordering::Acquire) {
        match receive(&mut datagram) {
            Ok((length, sender)) => {
                backoff = None;
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                if !sender.ip().is_loopback() {
                    counters.rejected.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let Ok(action) = decode(&datagram[..length]) else {
                    counters.malformed.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                match actions.try_send(action) {
                    Ok(()) => repaint.request_repaint(),
                    Err(TrySendError::Full(_)) => {
                        counters.overflow.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                backoff = None;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                counters.failed.fetch_add(1, Ordering::Relaxed);
                let delay = backoff.map_or(MIN_ERROR_BACKOFF, |previous| {
                    previous.saturating_mul(2).min(MAX_ERROR_BACKOFF)
                });
                if backoff.is_none() {
                    eprintln!("freemix-studio: OSC receive failed, retrying: {error}");
                }
                backoff = Some(delay);
                pause(delay);
            }
        }
    }
}

fn decode(datagram: &[u8]) -> Result<OscAction, ()> {
    let (address, type_offset) = padded_string(datagram, 0)?;
    let (types, end) = padded_string(datagram, type_offset)?;
    if address == "/freemix/switcher/manual/position" {
        let argument: [u8; 4] = datagram
            .get(end..)
            .filter(|_| types == ",i")
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(())?;
        return u16::try_from(i32::from_be_bytes(argument))
            .ok()
            .and_then(fm_protocol::ManualTransitionPosition::new)
            .map(OscAction::SetManualTransitionPosition)
            .ok_or(());
    }
    if address == "/freemix/cue/arm" && types == ",i" {
        let argument: [u8; 4] = datagram
            .get(end..)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(())?;
        return u32::try_from(i32::from_be_bytes(argument))
            .ok()
            .filter(|in_ms| (1..=fm_protocol::MAX_CUE_ARM_MS).contains(in_ms))
            .map(|in_ms| OscAction::Cue(CueKey::ArmIn(in_ms)))
            .ok_or(());
    }
    if types != "," || end != datagram.len() {
        return Err(());
    }
    if let Some(kind) = address.strip_prefix("/freemix/switcher/manual/start/") {
        return match fm_protocol::TransitionFavoriteEffect::from_token(kind) {
            Some(fm_protocol::TransitionFavoriteEffect::Cut) | None => Err(()),
            Some(effect) => Ok(OscAction::StartManualTransition(effect)),
        };
    }
    if let Some(input) = address.strip_prefix("/freemix/switcher/preview/") {
        return match input.as_bytes() {
            [number @ b'1'..=b'8'] => Ok(OscAction::SelectPreview(number - b'0')),
            _ => Err(()),
        };
    }

    match address {
        "/freemix/switcher/cut" => Ok(OscAction::Cut),
        "/freemix/switcher/fade" => Ok(OscAction::Fade),
        "/freemix/switcher/alpha-fade" => Ok(OscAction::AlphaFade),
        "/freemix/switcher/slide" => Ok(OscAction::Slide),
        "/freemix/switcher/zoom" => Ok(OscAction::Zoom),
        "/freemix/switcher/wipe" => Ok(OscAction::Wipe),
        "/freemix/switcher/ftb/live" => Ok(OscAction::FadeToBlack { active: false }),
        "/freemix/switcher/ftb/black" => Ok(OscAction::FadeToBlack { active: true }),
        "/freemix/switcher/manual/commit" => Ok(OscAction::CommitManualTransition),
        "/freemix/switcher/manual/cancel" => Ok(OscAction::CancelManualTransition),
        "/freemix/cue/go" => Ok(OscAction::Cue(CueKey::Go)),
        "/freemix/cue/previous" => Ok(OscAction::Cue(CueKey::Previous)),
        "/freemix/cue/next" => Ok(OscAction::Cue(CueKey::Next)),
        "/freemix/cue/stop" => Ok(OscAction::Cue(CueKey::Stop)),
        "/freemix/cue/reset" => Ok(OscAction::Cue(CueKey::Reset)),
        "/freemix/cue/arm" => Ok(OscAction::Cue(CueKey::Arm)),
        "/freemix/cue/disarm" => Ok(OscAction::Cue(CueKey::Disarm)),
        _ => Err(()),
    }
}

fn padded_string(datagram: &[u8], start: usize) -> Result<(&str, usize), ()> {
    let bytes = datagram.get(start..).ok_or(())?;
    let string_length = bytes.iter().position(|byte| *byte == 0).ok_or(())?;
    let padded_length = (string_length + 1).next_multiple_of(4);
    let end = start.checked_add(padded_length).ok_or(())?;
    let padded = datagram.get(start..end).ok_or(())?;
    if padded[string_length..].iter().any(|byte| *byte != 0) {
        return Err(());
    }
    let value = str::from_utf8(&padded[..string_length]).map_err(|_| ())?;
    Ok((value, end))
}

#[cfg(test)]
mod tests {
    use std::{
        net::UdpSocket,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread::sleep,
        time::{Duration, Instant},
    };

    use eframe::egui;

    use super::{
        CueKey, DATAGRAM_CAPACITY, ListeningGuard, MAX_ERROR_BACKOFF, MIN_ERROR_BACKOFF, OscAction,
        OscReceiver, SharedCounters, decode, receive_loop,
    };

    /// An interrupted receive retries at once and other errors back off
    /// and retry: neither stops the listener, which still decodes the next
    /// datagram. A good datagram resets the backoff.
    #[test]
    fn receive_errors_back_off_and_never_stop_the_listener() {
        use std::{
            io,
            net::SocketAddr,
            sync::{atomic::AtomicBool, mpsc},
        };
        let loopback: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let cut = message("/freemix/switcher/cut");
        let fade = message("/freemix/switcher/fade");
        let errors = |kind| Err::<Vec<u8>, _>(io::Error::from(kind));
        let mut script = vec![
            errors(io::ErrorKind::Interrupted),
            errors(io::ErrorKind::ConnectionRefused),
            errors(io::ErrorKind::Other),
            Ok(cut),
            errors(io::ErrorKind::Interrupted),
            errors(io::ErrorKind::ConnectionReset),
            Ok(fade),
        ];
        script.extend((0..12).map(|_| errors(io::ErrorKind::Other)));
        script.reverse();
        let counters = SharedCounters::default();
        let cancel = AtomicBool::new(false);
        let (sender, actions) = mpsc::sync_channel(4);
        let mut pauses = Vec::new();
        receive_loop(
            &mut |datagram| {
                let Some(next) = script.pop() else {
                    cancel.store(true, Ordering::Release);
                    return Err(io::ErrorKind::TimedOut.into());
                };
                let bytes = next?;
                datagram[..bytes.len()].copy_from_slice(&bytes);
                Ok((bytes.len(), loopback))
            },
            &mut |delay| pauses.push(delay),
            &sender,
            &counters,
            &cancel,
            &egui::Context::default(),
        );
        assert_eq!(
            actions.try_iter().collect::<Vec<_>>(),
            [OscAction::Cut, OscAction::Fade]
        );
        // Interrupted receives are neither counted nor paused for.
        assert_eq!(counters.failed.load(Ordering::Relaxed), 15);
        let ms = |ms| Duration::from_millis(ms);
        let mut expected = vec![ms(10), ms(20), ms(10)];
        expected.extend([10, 20, 40, 80, 160, 320, 640].map(ms));
        expected.extend([MAX_ERROR_BACKOFF; 5]);
        assert_eq!(pauses, expected);
        assert_eq!(pauses[0], MIN_ERROR_BACKOFF);
    }

    /// A worker that exits, even by a panic, clears the listening flag the
    /// console shows.
    #[test]
    fn a_dead_listener_is_reported() {
        let repaint = egui::Context::default();
        let address = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let receiver = OscReceiver::bind(address, repaint).unwrap();
        assert!(receiver.is_listening());
        let counters = Arc::clone(&receiver.counters);
        receiver.shutdown().unwrap();
        assert!(!counters.listening.load(Ordering::Acquire));

        let counters = Arc::new(SharedCounters::default());
        counters.listening.store(true, Ordering::Release);
        let worker = Arc::clone(&counters);
        let panicked = std::thread::spawn(move || {
            let _listening = ListeningGuard(&worker.listening);
            panic!("worker fault");
        })
        .join();
        assert!(panicked.is_err());
        assert!(!counters.listening.load(Ordering::Acquire));
    }

    /// A queued action wakes the UI: an idle console (no monitor timers, no
    /// state traffic) must not hold an OSC GO until something else repaints.
    #[test]
    fn a_queued_action_wakes_the_ui() {
        let repaint = egui::Context::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        repaint.set_request_repaint_callback(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let address = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let receiver = OscReceiver::bind(address, repaint).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(&message("/freemix/switcher/cut"), receiver.local_addr())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let action = loop {
            if let Ok(action) = receiver.try_recv() {
                break Some(action);
            }
            if Instant::now() >= deadline {
                break None;
            }
            sleep(Duration::from_millis(5));
        };
        assert_eq!(action, Some(OscAction::Cut));
        assert!(wakes.load(Ordering::SeqCst) >= 1, "the UI was not woken");
        // A malformed datagram wakes nothing.
        let woken = wakes.load(Ordering::SeqCst);
        sender.send_to(b"/freemix", receiver.local_addr()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while receiver.counters().malformed == 0 && Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }
        assert_eq!(receiver.counters().malformed, 1);
        assert_eq!(wakes.load(Ordering::SeqCst), woken);
    }

    #[test]
    fn osc_decoder_accepts_only_the_exact_action_contract() {
        for input in 1..=8 {
            let address = format!("/freemix/switcher/preview/{input}");
            assert_eq!(
                decode(&message(&address)),
                Ok(OscAction::SelectPreview(input))
            );
        }
        let cases = [
            ("/freemix/switcher/cut", OscAction::Cut),
            ("/freemix/switcher/fade", OscAction::Fade),
            ("/freemix/switcher/alpha-fade", OscAction::AlphaFade),
            ("/freemix/switcher/slide", OscAction::Slide),
            ("/freemix/switcher/zoom", OscAction::Zoom),
            ("/freemix/switcher/wipe", OscAction::Wipe),
            (
                "/freemix/switcher/ftb/live",
                OscAction::FadeToBlack { active: false },
            ),
            (
                "/freemix/switcher/ftb/black",
                OscAction::FadeToBlack { active: true },
            ),
            (
                "/freemix/switcher/manual/commit",
                OscAction::CommitManualTransition,
            ),
            (
                "/freemix/switcher/manual/cancel",
                OscAction::CancelManualTransition,
            ),
        ];
        for (address, action) in cases {
            assert_eq!(decode(&message(address)), Ok(action));
        }
    }

    /// RC-017: the Cues panel's keys, and `arm`'s optional delay.
    #[test]
    fn osc_decoder_accepts_the_cue_keys() {
        for (address, key) in [
            ("/freemix/cue/go", CueKey::Go),
            ("/freemix/cue/previous", CueKey::Previous),
            ("/freemix/cue/next", CueKey::Next),
            ("/freemix/cue/stop", CueKey::Stop),
            ("/freemix/cue/reset", CueKey::Reset),
            ("/freemix/cue/arm", CueKey::Arm),
            ("/freemix/cue/disarm", CueKey::Disarm),
        ] {
            assert_eq!(decode(&message(address)), Ok(OscAction::Cue(key)));
        }
        assert_eq!(
            decode(&with_types(
                "/freemix/cue/arm",
                ",i",
                &30_000_i32.to_be_bytes()
            )),
            Ok(OscAction::Cue(CueKey::ArmIn(30_000)))
        );
        let longest = i32::try_from(fm_protocol::MAX_CUE_ARM_MS).unwrap();
        for datagram in [
            with_types("/freemix/cue/arm", ",i", &0_i32.to_be_bytes()),
            with_types("/freemix/cue/arm", ",i", &(-5_i32).to_be_bytes()),
            with_types("/freemix/cue/arm", ",i", &(longest + 1).to_be_bytes()),
            with_types("/freemix/cue/arm", ",f", &[0, 0, 0, 0]),
            with_types("/freemix/cue/arm", ",i", &[0, 0, 1]),
            with_types("/freemix/cue/go", ",i", &[0, 0, 0, 1]),
            message("/freemix/cue/GO"),
            message("/freemix/cue"),
        ] {
            assert_eq!(decode(&datagram), Err(()));
        }
    }

    #[test]
    fn osc_decoder_bounds_manual_transitions_and_rejects_the_rest() {
        assert_eq!(
            decode(&message("/freemix/switcher/manual/start/cube_zoom")),
            Ok(OscAction::StartManualTransition(
                fm_protocol::TransitionFavoriteEffect::Mix(
                    fm_protocol::AutoTransitionKind::CubeZoom
                )
            ))
        );
        assert_eq!(
            decode(&message("/freemix/switcher/manual/start/stinger_2")),
            Ok(OscAction::StartManualTransition(
                fm_protocol::TransitionFavoriteEffect::Stinger(
                    fm_protocol::WireStingerSlotId::new(2).unwrap()
                )
            ))
        );
        assert_eq!(
            decode(&with_types(
                "/freemix/switcher/manual/position",
                ",i",
                &2_500_i32.to_be_bytes()
            )),
            Ok(OscAction::SetManualTransitionPosition(
                fm_protocol::ManualTransitionPosition::new(2_500).unwrap()
            ))
        );
        for datagram in [
            message("/freemix/switcher/manual/start/cut"),
            message("/freemix/switcher/manual/start/FADE"),
            message("/freemix/switcher/manual/position"),
            with_types(
                "/freemix/switcher/manual/position",
                ",i",
                &10_001_i32.to_be_bytes(),
            ),
            with_types(
                "/freemix/switcher/manual/position",
                ",i",
                &(-1_i32).to_be_bytes(),
            ),
            with_types("/freemix/switcher/manual/position", ",f", &[0, 0, 0, 0]),
            with_types("/freemix/switcher/manual/position", ",i", &[0, 0, 0]),
        ] {
            assert_eq!(decode(&datagram), Err(()));
        }

        let rejected = [
            bundle(),
            message("/freemix/switcher/preview/0"),
            message("/freemix/switcher/preview/9"),
            message("/freemix/switcher/CUT"),
            with_types("/freemix/switcher/cut", ",i", &[0, 0, 0, 1]),
            malformed_padding(),
            trailing_bytes(),
            vec![b'/'; DATAGRAM_CAPACITY + 1],
        ];
        for datagram in rejected {
            assert_eq!(decode(&datagram), Err(()));
        }
    }

    fn message(address: &str) -> Vec<u8> {
        with_types(address, ",", &[])
    }

    fn bundle() -> Vec<u8> {
        let mut bundle = b"#bundle\0".to_vec();
        bundle.extend_from_slice(&[0; 8]);
        bundle
    }

    fn with_types(address: &str, types: &str, arguments: &[u8]) -> Vec<u8> {
        let mut message = Vec::new();
        push_string(&mut message, address);
        push_string(&mut message, types);
        message.extend_from_slice(arguments);
        message
    }

    fn push_string(message: &mut Vec<u8>, value: &str) {
        message.extend_from_slice(value.as_bytes());
        message.push(0);
        while !message.len().is_multiple_of(4) {
            message.push(0);
        }
    }

    fn malformed_padding() -> Vec<u8> {
        let mut message = message("/freemix/switcher/cut");
        message[23] = 1;
        message
    }

    fn trailing_bytes() -> Vec<u8> {
        let mut message = message("/freemix/switcher/cut");
        message.extend_from_slice(&[0; 4]);
        message
    }
}
