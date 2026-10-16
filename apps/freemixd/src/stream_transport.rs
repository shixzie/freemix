//! Stream transports driven off the daemon loop.
//!
//! The shared [`OutputSet`] drives each destination through a
//! [`TransportSink`] whose `connect` and `disconnect` may block: the `FFmpeg`
//! transport spawns a child and waits up to its connect timeout for the
//! child's first input request, and a stop drains the session within the stop
//! and kill budgets. Called from the daemon loop, a `StreamStart`, every
//! reconnect attempt, and every stop or failure froze frame realization,
//! heartbeats, monitor frames and every session for seconds.
//!
//! [`BackgroundTransport`] keeps the set's policy exactly as it is and moves
//! only that blocking I/O to one dedicated worker per destination. The daemon
//! asks [`connect_due`] whether the set's next poll would connect; while the
//! worker is still connecting, the daemon simply skips that destination's poll,
//! so the set stays in the state it was in (`Connecting`, or a due
//! `WaitingToReconnect`). Once the worker hands the sink back, the next poll
//! connects and receives the realized result, so the set records the attempt,
//! classification and backoff identically. Disconnects are dispatched to the
//! same worker, which runs jobs strictly in order, so a later connect always
//! follows every earlier stop.

use std::sync::mpsc;
use std::thread;

use fm_io_network::{
    ConnectionObservation, ConnectionTarget, DestinationConfig, DestinationId, DestinationState,
    Endpoint, FailureStage, OutputPacket, OutputSet, SinkError, SinkWrite, TransportSink,
};

type ConnectResult = Result<ConnectionObservation, SinkError>;

/// Work handed to the destination's worker together with the sink itself.
enum Job<S> {
    Connect {
        sink: S,
        config: Box<DestinationConfig>,
        endpoint: Endpoint,
    },
    Disconnect {
        sink: S,
    },
}

/// The sink coming back from the worker, with the connect outcome if the job
/// was a connect.
struct Returned<S> {
    sink: S,
    connected: Option<ConnectResult>,
}

struct Worker<S> {
    jobs: mpsc::Sender<Job<S>>,
    returned: mpsc::Receiver<Returned<S>>,
}

/// The job the worker currently holds the sink for.
enum InFlight {
    Connect {
        endpoint: Endpoint,
        /// The set disconnected while the connect was still running; the
        /// session it opens is stopped as soon as it is handed back.
        disconnect_after: bool,
    },
    Disconnect,
}

/// A [`TransportSink`] whose blocking connect and disconnect run on a
/// dedicated worker; see the module documentation.
pub(crate) struct BackgroundTransport<S> {
    /// The sink while no worker job holds it.
    home: Option<S>,
    /// `None` only if the worker could not be spawned or has died; jobs then
    /// run synchronously, as before.
    worker: Option<Worker<S>>,
    in_flight: Option<InFlight>,
    /// A connect realized by the worker, waiting for the set's next poll.
    ready: Option<(Endpoint, ConnectResult)>,
}

fn lost_transport() -> SinkError {
    SinkError::new(
        FailureStage::Connect,
        None,
        "stream transport worker exited",
        false,
    )
}

impl<S: TransportSink + 'static> BackgroundTransport<S> {
    /// Wraps `sink` and starts its worker thread. If the thread cannot be
    /// spawned, the transport degrades to running its I/O synchronously.
    pub(crate) fn new(sink: S, name: String) -> Self {
        let (jobs, job_receiver) = mpsc::channel::<Job<S>>();
        let (returned_sender, returned) = mpsc::channel();
        let spawned = thread::Builder::new().name(name).spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                let back = run_job(job);
                // The transport is gone: dropping the sink here stops any
                // session it still holds, still off the daemon loop.
                if returned_sender.send(back).is_err() {
                    return;
                }
            }
        });
        Self {
            home: Some(sink),
            worker: spawned.ok().map(|_| Worker { jobs, returned }),
            in_flight: None,
            ready: None,
        }
    }

    /// Adopts a sink the worker handed back. Never blocks.
    pub(crate) fn advance(&mut self) {
        if self.in_flight.is_none() {
            return;
        }
        let Some(worker) = &self.worker else {
            return;
        };
        match worker.returned.try_recv() {
            Ok(back) => self.complete(back),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                // The worker died holding the sink; every later connect fails
                // terminally instead of hanging the destination.
                self.worker = None;
                self.in_flight = None;
            }
        }
    }

    /// Whether the set may run its pending connect to `endpoint` now. The
    /// first call hands the connect to the worker; it returns `true` once the
    /// realized result is waiting for [`TransportSink::connect`].
    pub(crate) fn prepare_connect(
        &mut self,
        config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> bool {
        self.advance();
        if self.in_flight.is_some() {
            return false;
        }
        match &self.ready {
            Some((ready, _)) if ready == endpoint => return true,
            // A result for another endpoint is stale; the sink's own connect
            // replaces the session it opened.
            Some(_) => self.ready = None,
            None => {}
        }
        let Some(sink) = self.home.take() else {
            self.ready = Some((endpoint.clone(), Err(lost_transport())));
            return true;
        };
        self.in_flight = Some(InFlight::Connect {
            endpoint: endpoint.clone(),
            disconnect_after: false,
        });
        self.dispatch(Job::Connect {
            sink,
            config: Box::new(config.clone()),
            endpoint: endpoint.clone(),
        });
        self.ready.is_some()
    }

    /// Waits for every dispatched job and returns the sink. Only shutdown
    /// calls this; each job is bounded by the sink's own deadlines.
    pub(crate) fn settle(&mut self) -> Option<&S> {
        while self.in_flight.is_some() {
            let received = self
                .worker
                .as_ref()
                .and_then(|worker| worker.returned.recv().ok());
            if let Some(back) = received {
                self.complete(back);
            } else {
                // The worker died holding the sink.
                self.worker = None;
                self.in_flight = None;
            }
        }
        self.home.as_ref()
    }

    /// The sink while no worker job holds it, for read-only telemetry.
    pub(crate) fn idle_sink(&self) -> Option<&S> {
        if self.in_flight.is_none() {
            self.home.as_ref()
        } else {
            None
        }
    }

    /// Whether the worker currently holds the sink.
    #[cfg(test)]
    pub(crate) const fn busy(&self) -> bool {
        self.in_flight.is_some()
    }

    fn complete(&mut self, back: Returned<S>) {
        self.home = Some(back.sink);
        match self.in_flight.take() {
            Some(InFlight::Connect {
                endpoint,
                disconnect_after,
            }) => {
                if disconnect_after {
                    self.start_disconnect();
                } else if let Some(result) = back.connected {
                    self.ready = Some((endpoint, result));
                }
            }
            Some(InFlight::Disconnect) | None => {}
        }
    }

    fn start_disconnect(&mut self) {
        if let Some(sink) = self.home.take() {
            self.in_flight = Some(InFlight::Disconnect);
            self.dispatch(Job::Disconnect { sink });
        }
    }

    /// Hands `job` to the worker, or runs it here if no worker is alive.
    fn dispatch(&mut self, job: Job<S>) {
        let job = match &self.worker {
            Some(worker) => match worker.jobs.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => {
                    self.worker = None;
                    job
                }
            },
            None => job,
        };
        let back = run_job(job);
        self.complete(back);
    }
}

fn run_job<S: TransportSink>(job: Job<S>) -> Returned<S> {
    match job {
        Job::Connect {
            mut sink,
            config,
            endpoint,
        } => {
            let connected = sink.connect(&config, &endpoint);
            Returned {
                sink,
                connected: Some(connected),
            }
        }
        Job::Disconnect { mut sink } => {
            sink.disconnect();
            Returned {
                sink,
                connected: None,
            }
        }
    }
}

impl<S: TransportSink + 'static> TransportSink for BackgroundTransport<S> {
    fn connect(
        &mut self,
        config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<ConnectionObservation, SinkError> {
        match self.ready.take() {
            Some((ready, result)) if ready == *endpoint => result,
            // Only reachable when a caller polls without `prepare_connect`:
            // realize the connect in place, as the set expects.
            _ => match (&self.in_flight, self.home.as_mut()) {
                (None, Some(sink)) => sink.connect(config, endpoint),
                (Some(_), _) => Err(SinkError::new(
                    FailureStage::Connect,
                    None,
                    "stream transport is still busy",
                    true,
                )),
                (None, None) => Err(lost_transport()),
            },
        }
    }

    fn write(&mut self, packet: &OutputPacket) -> Result<SinkWrite, SinkError> {
        match (&self.in_flight, self.home.as_mut()) {
            (None, Some(sink)) => sink.write(packet),
            _ => Err(SinkError::new(
                FailureStage::Connect,
                None,
                "no streaming session is open",
                true,
            )),
        }
    }

    fn session_proven(&self) -> bool {
        self.in_flight.is_none() && self.home.as_ref().is_some_and(S::session_proven)
    }

    fn disconnect(&mut self) {
        // A connect result nobody consumed still left a session open.
        self.ready = None;
        match &mut self.in_flight {
            Some(InFlight::Connect {
                disconnect_after, ..
            }) => *disconnect_after = true,
            Some(InFlight::Disconnect) => {}
            None => self.start_disconnect(),
        }
    }
}

/// The endpoint the set's next poll of `destination` would connect to, if
/// that poll connects at all; mirrors the set's own poll transitions.
pub(crate) fn connect_due(
    output_set: &OutputSet,
    destination: DestinationId,
    now_ms: u64,
) -> Option<(&DestinationConfig, &Endpoint)> {
    match output_set.state(destination)? {
        DestinationState::Connecting => {}
        DestinationState::WaitingToReconnect { retry_at_ms, .. } if now_ms >= retry_at_ms => {}
        _ => return None,
    }
    let config = output_set.config(destination)?;
    let endpoint = match output_set.connection_target(destination)? {
        ConnectionTarget::Primary => config.endpoint(),
        ConnectionTarget::Backup => config.backup_endpoint()?,
    };
    Some((config, endpoint))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use fm_io_network::{
        OutputProtocol, PollEvent, QueueCapacity, ReconnectPolicy, SendObservation,
    };

    use super::*;

    /// A sink whose every connect blocks until the test releases it, and
    /// which records the order of operations it performed.
    struct GatedSink {
        gate: mpsc::Receiver<ConnectResult>,
        log: Arc<Mutex<Vec<String>>>,
        open: bool,
    }

    impl TransportSink for GatedSink {
        fn connect(
            &mut self,
            _config: &DestinationConfig,
            endpoint: &Endpoint,
        ) -> Result<ConnectionObservation, SinkError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("connect:{}", endpoint.host()));
            let result = self.gate.recv().unwrap();
            self.open = result.is_ok();
            result
        }

        fn write(&mut self, _packet: &OutputPacket) -> Result<SinkWrite, SinkError> {
            assert!(self.open, "a write needs an open session");
            Ok(SinkWrite::Sent(SendObservation::default()))
        }

        fn disconnect(&mut self) {
            self.open = false;
            self.log.lock().unwrap().push("disconnect".to_owned());
        }
    }

    fn destination() -> DestinationConfig {
        DestinationConfig::new(
            DestinationId::new(1).unwrap(),
            OutputProtocol::Rtmp,
            Endpoint::new("primary.example.test", 1935, "/live").unwrap(),
            Some(Endpoint::new("backup.example.test", 1935, "/live").unwrap()),
            None,
            None,
            QueueCapacity::new(1).unwrap(),
            ReconnectPolicy::new(10, 20, 2, Some(3)).unwrap(),
        )
        .unwrap()
    }

    fn gated() -> (
        BackgroundTransport<GatedSink>,
        mpsc::Sender<ConnectResult>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let (release, gate) = mpsc::channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = GatedSink {
            gate,
            log: Arc::clone(&log),
            open: false,
        };
        (
            BackgroundTransport::new(sink, "stream-transport-test".to_owned()),
            release,
            log,
        )
    }

    /// Polls `destination` the way the daemon does: never while its connect
    /// is still on the worker.
    fn daemon_poll(
        set: &mut OutputSet,
        transport: &mut BackgroundTransport<GatedSink>,
        now_ms: u64,
    ) -> Option<PollEvent> {
        let id = DestinationId::new(1).unwrap();
        transport.advance();
        if let Some(ready) = connect_due(set, id, now_ms)
            .map(|(config, endpoint)| transport.prepare_connect(config, endpoint))
            && !ready
        {
            return None;
        }
        Some(set.poll(id, now_ms, transport).unwrap())
    }

    fn poll_until_connected(
        set: &mut OutputSet,
        transport: &mut BackgroundTransport<GatedSink>,
        now_ms: u64,
    ) -> PollEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = daemon_poll(set, transport, now_ms) {
                return event;
            }
            assert!(Instant::now() < deadline, "the worker never handed back");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_pending_connect_never_blocks_the_poll_and_keeps_the_set_connecting() {
        let id = DestinationId::new(1).unwrap();
        let mut set = OutputSet::new();
        set.add_destination(destination()).unwrap();
        let (mut transport, release, log) = gated();
        set.start(id).unwrap();

        // The connect is on the worker and held by the gate: the poll returns
        // at once and the set has not observed any attempt yet.
        for _ in 0..3 {
            assert_eq!(daemon_poll(&mut set, &mut transport, 0), None);
        }
        assert!(transport.busy());
        assert_eq!(set.state(id), Some(DestinationState::Connecting));
        assert_eq!(set.telemetry(id).unwrap().connect_attempts(), 0);

        release.send(Ok(ConnectionObservation::default())).unwrap();
        assert_eq!(
            poll_until_connected(&mut set, &mut transport, 0),
            PollEvent::Connected
        );
        assert_eq!(set.state(id), Some(DestinationState::Live));
        assert_eq!(set.telemetry(id).unwrap().connect_attempts(), 1);
        assert_eq!(*log.lock().unwrap(), ["connect:primary.example.test"]);
    }

    #[test]
    fn a_failed_background_connect_schedules_the_reconnect_on_the_backup() {
        let id = DestinationId::new(1).unwrap();
        let mut set = OutputSet::new();
        set.add_destination(destination()).unwrap();
        let (mut transport, release, log) = gated();
        set.start(id).unwrap();

        assert_eq!(daemon_poll(&mut set, &mut transport, 0), None);
        release
            .send(Err(SinkError::new(
                FailureStage::Connect,
                None,
                "refused",
                true,
            )))
            .unwrap();
        assert_eq!(
            poll_until_connected(&mut set, &mut transport, 0),
            PollEvent::ReconnectScheduled { retry_at_ms: 10 }
        );
        // Not due yet: no connect is dispatched.
        assert_eq!(
            daemon_poll(&mut set, &mut transport, 5),
            Some(PollEvent::WaitingToReconnect { retry_at_ms: 10 })
        );
        // Due: the reconnect runs on the worker against the backup endpoint.
        assert_eq!(daemon_poll(&mut set, &mut transport, 10), None);
        assert!(matches!(
            set.state(id),
            Some(DestinationState::WaitingToReconnect { .. })
        ));
        release.send(Ok(ConnectionObservation::default())).unwrap();
        assert!(matches!(
            poll_until_connected(&mut set, &mut transport, 10),
            PollEvent::AwaitingRandomAccess { .. }
        ));
        assert_eq!(
            *log.lock().unwrap(),
            [
                "connect:primary.example.test",
                "connect:backup.example.test"
            ]
        );
    }

    #[test]
    fn a_stop_during_a_pending_connect_closes_that_session_before_the_restart() {
        let id = DestinationId::new(1).unwrap();
        let mut set = OutputSet::new();
        set.add_destination(destination()).unwrap();
        let (mut transport, release, log) = gated();
        set.start(id).unwrap();
        assert_eq!(daemon_poll(&mut set, &mut transport, 0), None);

        // The operator stops and restarts while the first connect still runs.
        set.stop(id, &mut transport).unwrap();
        set.start(id).unwrap();
        assert_eq!(daemon_poll(&mut set, &mut transport, 0), None);

        release.send(Ok(ConnectionObservation::default())).unwrap();
        release.send(Ok(ConnectionObservation::default())).unwrap();
        assert_eq!(
            poll_until_connected(&mut set, &mut transport, 0),
            PollEvent::Connected
        );
        assert_eq!(
            *log.lock().unwrap(),
            [
                "connect:primary.example.test",
                "disconnect",
                "connect:primary.example.test"
            ]
        );
        assert_eq!(set.telemetry(id).unwrap().connect_attempts(), 1);

        set.stop(id, &mut transport).unwrap();
        assert!(transport.settle().is_some());
        assert_eq!(log.lock().unwrap().last().unwrap(), "disconnect");
    }
}
