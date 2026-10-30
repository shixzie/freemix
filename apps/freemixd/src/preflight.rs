//! Bounded, non-fatal network preflight for configured stream endpoints.
//!
//! Every configured network streaming destination is probed once at native
//! startup: one TCP connect attempt per endpoint records whether the ingest
//! answered, refused, timed out, or could not be resolved. The probe is
//! deliberately informational — it never refuses startup and never changes
//! destination state, because an ingest may legitimately come up later — and
//! it reports one sanitized record per endpoint through [`probe_record`].
//! It proves reachability of the configured socket, not bandwidth, and it is
//! not a health guarantee for the stream that follows.

use std::{
    io,
    net::{TcpStream, ToSocketAddrs},
    num::NonZeroU16,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// How long the whole network preflight may block startup before every probe
/// that has not answered is reported as [`NetworkProbe::Unresolved`].
pub const NETWORK_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(2);

/// Endpoint label for a destination's primary endpoint.
pub const PRIMARY_ENDPOINT: &str = "primary";

/// Endpoint label for a destination's configured backup endpoint.
pub const BACKUP_ENDPOINT: &str = "backup";

/// The outcome of one TCP reachability probe. No variant carries endpoint
/// text, so a probe outcome can always be rendered safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkProbe {
    /// A resolved address accepted the TCP connection.
    Reachable,
    /// Every resolved address refused the connection while time remained.
    Refused,
    /// The overall deadline passed without a successful connection.
    TimedOut,
    /// The host did not resolve to any address.
    Unresolved,
}

impl NetworkProbe {
    /// Stable record token for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Refused => "refused",
            Self::TimedOut => "timed-out",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Probes one TCP endpoint under a single overall deadline.
///
/// Resolution failures and an empty address list map to
/// [`NetworkProbe::Unresolved`]. Every resolved address is then tried with
/// [`TcpStream::connect_timeout`] under the deadline: the first accepted
/// connection returns [`NetworkProbe::Reachable`], a connection failure tries
/// the next address while time remains, a fast refusal on every address
/// returns [`NetworkProbe::Refused`], and a deadline that passes without a
/// success returns [`NetworkProbe::TimedOut`]. Host and key text never appear
/// in the result.
#[must_use]
pub fn probe_tcp(host: &str, port: NonZeroU16, timeout: Duration) -> NetworkProbe {
    // A deadline that cannot be represented is unreachable for any duration
    // this daemon can configure; reporting it as timed out is still safe.
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return NetworkProbe::TimedOut;
    };
    let Ok(resolved) = (host, port.get()).to_socket_addrs() else {
        return NetworkProbe::Unresolved;
    };
    let mut addresses = resolved.peekable();
    if addresses.peek().is_none() {
        return NetworkProbe::Unresolved;
    }
    let mut saw_timeout = false;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return NetworkProbe::TimedOut;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(_) => return NetworkProbe::Reachable,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => saw_timeout = true,
            Err(_) => {}
        }
    }
    if saw_timeout || Instant::now() >= deadline {
        NetworkProbe::TimedOut
    } else {
        NetworkProbe::Refused
    }
}

/// One endpoint's completed probe: the engine stream target id, the endpoint
/// label, and the outcome. No host, path, or key text is carried.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointProbe {
    /// The engine stream target id the endpoint belongs to.
    pub target: u128,
    /// [`PRIMARY_ENDPOINT`] or [`BACKUP_ENDPOINT`].
    pub endpoint: &'static str,
    /// What the single TCP reachability attempt observed.
    pub probe: NetworkProbe,
}

/// One endpoint waiting to be probed: everything a worker thread needs plus
/// the identity its result is reported under. The caller bounds the count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingProbe {
    /// The engine stream target id the endpoint belongs to.
    pub target: u128,
    /// [`PRIMARY_ENDPOINT`] or [`BACKUP_ENDPOINT`].
    pub endpoint: &'static str,
    /// The endpoint host, held only until the worker has resolved it.
    pub host: String,
    /// The endpoint port; every network destination names an explicit port.
    pub port: NonZeroU16,
    /// The per-probe connect deadline, already bounded by the caller.
    pub timeout: Duration,
}

/// Renders the one sanitized record for a completed probe:
///
/// `FREEMIXD_PREFLIGHT\tv=1\ttarget=<target>\tendpoint=<primary|backup>\tnetwork=<reachable|refused|timed-out|unresolved>`
///
/// The record carries the target id and the endpoint label only; it never
/// carries the host, the port, the path, or key text.
#[must_use]
pub fn probe_record(probe: &EndpointProbe) -> String {
    format!(
        "FREEMIXD_PREFLIGHT\tv=1\ttarget={}\tendpoint={}\tnetwork={}",
        probe.target,
        probe.endpoint,
        probe.probe.as_str()
    )
}

/// Probes every pending endpoint and returns one result per pending probe, in
/// input order.
///
/// One short-lived worker thread is spawned per pending probe and sends its
/// outcome over an `mpsc` channel. The collector waits at most
/// [`NETWORK_PREFLIGHT_TIMEOUT`] in total, then reports every probe that has
/// not answered as [`NetworkProbe::Unresolved`] instead of blocking startup.
/// Workers still running past that point are detached: each is bounded by its
/// own `connect_timeout` and exits without being joined. A probe whose worker
/// cannot be spawned also reports [`NetworkProbe::Unresolved`].
#[must_use]
pub fn probe_targets(probes: Vec<PendingProbe>) -> Vec<EndpointProbe> {
    let (sender, receiver) = mpsc::channel::<(usize, NetworkProbe)>();
    let mut labels = Vec::with_capacity(probes.len());
    let mut answers = vec![None; probes.len()];
    let deadline = Instant::now()
        .checked_add(NETWORK_PREFLIGHT_TIMEOUT)
        .unwrap_or_else(Instant::now);
    for (index, pending) in probes.into_iter().enumerate() {
        labels.push((pending.target, pending.endpoint));
        let worker = thread::Builder::new()
            .name("freemix-preflight".to_owned())
            .spawn({
                let sender = sender.clone();
                move || {
                    let outcome = probe_tcp(&pending.host, pending.port, pending.timeout);
                    // A detached worker whose answer arrives after the
                    // collector stopped waiting has nowhere to send it, which
                    // is fine: the collector already reported it unresolved.
                    let _ = sender.send((index, outcome));
                }
            });
        if worker.is_err() {
            answers[index] = Some(NetworkProbe::Unresolved);
        }
    }
    drop(sender);
    let mut outstanding = answers.iter().filter(|answer| answer.is_none()).count();
    while outstanding > 0 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok((index, outcome)) => {
                if answers[index].is_none() {
                    answers[index] = Some(outcome);
                    outstanding -= 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    labels
        .into_iter()
        .zip(answers)
        .map(|((target, endpoint), answer)| EndpointProbe {
            target,
            endpoint,
            probe: answer.unwrap_or(NetworkProbe::Unresolved),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// Bounded probe timeout for tests whose endpoint answers immediately;
    /// generous enough that a slow CI host still sees the refusal it wrote.
    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    /// The `127.0.0.1` port a listener is bound to.
    fn loopback_port(listener: &TcpListener) -> NonZeroU16 {
        NonZeroU16::new(listener.local_addr().unwrap().port()).unwrap()
    }

    #[test]
    fn probe_tcp_reaches_a_bound_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();

        assert_eq!(
            probe_tcp("127.0.0.1", loopback_port(&listener), TEST_TIMEOUT),
            NetworkProbe::Reachable
        );
    }

    /// Whether `probe` is what a closed loopback port reports: refused,
    /// or on Windows also timed out, where loopback retransmits the SYN
    /// for longer than the probe waits before refusing it.
    fn is_closed_port_probe(probe: NetworkProbe) -> bool {
        probe == NetworkProbe::Refused || (cfg!(windows) && probe == NetworkProbe::TimedOut)
    }

    #[test]
    fn probe_tcp_reports_a_closed_port_as_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = loopback_port(&listener);
        drop(listener);

        let probe = probe_tcp("127.0.0.1", port, TEST_TIMEOUT);
        assert!(is_closed_port_probe(probe), "{probe:?}");
    }

    #[test]
    fn probe_tcp_reports_an_unresolvable_host_as_unresolved() {
        let port = NonZeroU16::new(19_35).unwrap();

        assert_eq!(
            probe_tcp("invalid.invalid.", port, TEST_TIMEOUT),
            NetworkProbe::Unresolved
        );
    }

    #[test]
    fn network_probe_tokens_are_stable() {
        let tokens = [
            (NetworkProbe::Reachable, "reachable"),
            (NetworkProbe::Refused, "refused"),
            (NetworkProbe::TimedOut, "timed-out"),
            (NetworkProbe::Unresolved, "unresolved"),
        ];

        for (outcome, token) in tokens {
            assert_eq!(outcome.as_str(), token);
        }
    }

    #[test]
    fn probe_record_renders_every_state_exactly() {
        let expected = [
            (
                NetworkProbe::Reachable,
                "FREEMIXD_PREFLIGHT\tv=1\ttarget=7\tendpoint=primary\tnetwork=reachable",
            ),
            (
                NetworkProbe::Refused,
                "FREEMIXD_PREFLIGHT\tv=1\ttarget=7\tendpoint=primary\tnetwork=refused",
            ),
            (
                NetworkProbe::TimedOut,
                "FREEMIXD_PREFLIGHT\tv=1\ttarget=7\tendpoint=primary\tnetwork=timed-out",
            ),
            (
                NetworkProbe::Unresolved,
                "FREEMIXD_PREFLIGHT\tv=1\ttarget=7\tendpoint=primary\tnetwork=unresolved",
            ),
        ];

        for (probe, record) in expected {
            assert_eq!(
                probe_record(&EndpointProbe {
                    target: 7,
                    endpoint: PRIMARY_ENDPOINT,
                    probe,
                }),
                record
            );
        }
        assert_eq!(
            probe_record(&EndpointProbe {
                target: 9,
                endpoint: BACKUP_ENDPOINT,
                probe: NetworkProbe::Refused,
            }),
            "FREEMIXD_PREFLIGHT\tv=1\ttarget=9\tendpoint=backup\tnetwork=refused"
        );
    }

    #[test]
    fn probe_targets_returns_one_result_per_pending_probe() {
        let reachable = TcpListener::bind("127.0.0.1:0").unwrap();
        let reachable_port = loopback_port(&reachable);
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_port = loopback_port(&closed);
        drop(closed);

        let results = probe_targets(vec![
            PendingProbe {
                target: 3,
                endpoint: PRIMARY_ENDPOINT,
                host: "127.0.0.1".to_owned(),
                port: reachable_port,
                timeout: TEST_TIMEOUT,
            },
            PendingProbe {
                target: 3,
                endpoint: BACKUP_ENDPOINT,
                host: "127.0.0.1".to_owned(),
                port: closed_port,
                timeout: TEST_TIMEOUT,
            },
        ]);

        assert_eq!(results.len(), 2, "{results:?}");
        assert_eq!(
            results[0],
            EndpointProbe {
                target: 3,
                endpoint: PRIMARY_ENDPOINT,
                probe: NetworkProbe::Reachable,
            }
        );
        assert_eq!(results[1].target, 3);
        assert_eq!(results[1].endpoint, BACKUP_ENDPOINT);
        assert!(is_closed_port_probe(results[1].probe), "{results:?}");
    }
}
