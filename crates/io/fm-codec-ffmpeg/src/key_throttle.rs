//! Per-peer back-off for RTMP publishers that present a wrong stream key.
//!
//! An RTMP listener's stream key is the only thing that keeps a stranger off
//! the air, and every publisher costs the receiver an `ffmpeg` child. Without
//! a limit a peer could guess keys as fast as children start. The receiver
//! therefore remembers wrong-key attempts per peer address: the first
//! [`FREE_KEY_FAILURES`] minus one are free, and every failure from the
//! [`FREE_KEY_FAILURES`]th on locks the address out for an exponentially
//! growing time, from [`KEY_LOCKOUT_BASE`] up to [`KEY_LOCKOUT_MAX`]. A locked
//! out peer's connection is closed as soon as it is accepted, before any
//! child exists. A publisher that delivers a picture clears its address.
//!
//! IPv6 peers are keyed by their `/64` prefix (one host usually owns the
//! whole prefix, so per-address keying would be trivially evaded), and an
//! IPv4-mapped IPv6 address by its IPv4 address. Memory is bounded: at most
//! [`KEY_THROTTLE_PEERS`] addresses are remembered, an address is forgotten
//! [`KEY_FAILURE_MEMORY`] after its last failure or lockout ends, and a full
//! table evicts the address seen longest ago.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::time::{Duration, Instant};

/// Wrong keys a peer may present before the first lockout starts: the
/// `FREE_KEY_FAILURES`th failure locks it out.
pub const FREE_KEY_FAILURES: u32 = 3;
/// The first lockout; each further failure doubles it.
pub const KEY_LOCKOUT_BASE: Duration = Duration::from_secs(1);
/// The longest lockout.
pub const KEY_LOCKOUT_MAX: Duration = Duration::from_secs(300);
/// How long an address's failures are remembered after its last failure
/// (or after its lockout ends, whichever is later).
pub const KEY_FAILURE_MEMORY: Duration = Duration::from_secs(600);
/// Most peer addresses one receiver remembers.
pub const KEY_THROTTLE_PEERS: usize = 1024;

/// One lockout the receiver imposed, for its owner to log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyLockout {
    /// The peer address (an IPv6 peer's `/64` prefix).
    pub peer: IpAddr,
    /// Consecutive wrong keys from that address.
    pub failures: u32,
    /// How long its connections are refused.
    pub lockout: Duration,
}

#[derive(Clone, Copy, Debug)]
struct Failures {
    count: u32,
    locked_until: Option<Instant>,
    last_failure: Instant,
}

impl Failures {
    fn forget_at(&self) -> Instant {
        let after_failure = self.last_failure + KEY_FAILURE_MEMORY;
        self.locked_until.map_or(after_failure, |until| {
            after_failure.max(until + KEY_FAILURE_MEMORY)
        })
    }
}

/// The wrong-key memory of one RTMP listener.
#[derive(Debug, Default)]
pub struct KeyThrottle {
    peers: HashMap<IpAddr, Failures>,
}

impl KeyThrottle {
    /// The address a peer is remembered under.
    #[must_use]
    pub fn key(peer: IpAddr) -> IpAddr {
        let IpAddr::V6(v6) = peer else {
            return peer;
        };
        if let Some(v4) = v6.to_ipv4_mapped() {
            return IpAddr::V4(v4);
        }
        let s = v6.segments();
        IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
    }

    /// Whether a connection from `peer` may be relayed now; `false` while
    /// the address is locked out.
    pub fn admits(&mut self, peer: IpAddr, now: Instant) -> bool {
        let key = Self::key(peer);
        match self.peers.get(&key) {
            Some(failures) if failures.forget_at() <= now => {
                self.peers.remove(&key);
                true
            }
            Some(failures) => failures.locked_until.is_none_or(|until| until <= now),
            None => true,
        }
    }

    /// Records a wrong key from `peer`; returns the lockout it starts, if
    /// any.
    pub fn failed(&mut self, peer: IpAddr, now: Instant) -> Option<KeyLockout> {
        let key = Self::key(peer);
        if !self.peers.contains_key(&key) && self.peers.len() >= KEY_THROTTLE_PEERS {
            self.peers.retain(|_, failures| failures.forget_at() > now);
            if self.peers.len() >= KEY_THROTTLE_PEERS
                && let Some(oldest) = self
                    .peers
                    .iter()
                    .min_by_key(|(_, failures)| failures.last_failure)
                    .map(|(address, _)| *address)
            {
                self.peers.remove(&oldest);
            }
        }
        let entry = self.peers.entry(key).or_insert(Failures {
            count: 0,
            locked_until: None,
            last_failure: now,
        });
        if entry.forget_at() <= now {
            entry.count = 0;
            entry.locked_until = None;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last_failure = now;
        if entry.count < FREE_KEY_FAILURES {
            return None;
        }
        let doublings = (entry.count - FREE_KEY_FAILURES).min(16);
        let lockout = KEY_LOCKOUT_BASE
            .saturating_mul(1 << doublings)
            .min(KEY_LOCKOUT_MAX);
        entry.locked_until = Some(now + lockout);
        Some(KeyLockout {
            peer: key,
            failures: entry.count,
            lockout,
        })
    }

    /// Forgets `peer`'s failures: it published with the right key.
    pub fn succeeded(&mut self, peer: IpAddr) {
        self.peers.remove(&Self::key(peer));
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));

    #[test]
    fn repeated_wrong_keys_lock_a_peer_out_exponentially_up_to_the_cap() {
        let mut throttle = KeyThrottle::default();
        let start = Instant::now();
        let mut now = start;
        for _ in 1..FREE_KEY_FAILURES {
            assert!(throttle.admits(PEER, now));
            assert_eq!(throttle.failed(PEER, now), None);
        }
        let mut expected = KEY_LOCKOUT_BASE;
        for failures in FREE_KEY_FAILURES..FREE_KEY_FAILURES + 12 {
            assert!(throttle.admits(PEER, now));
            let lockout = throttle.failed(PEER, now).expect("locked out");
            assert_eq!(lockout.failures, failures);
            assert_eq!(lockout.lockout, expected);
            // Refused for the whole lockout, admitted once it ends.
            assert!(!throttle.admits(PEER, now));
            assert!(!throttle.admits(
                PEER,
                now + expected.saturating_sub(Duration::from_millis(1))
            ));
            now += expected;
            expected = (expected * 2).min(KEY_LOCKOUT_MAX);
        }
        assert_eq!(expected, KEY_LOCKOUT_MAX);
        // Another address is unaffected.
        assert!(throttle.admits(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8)), now));
    }

    #[test]
    fn a_successful_publish_or_a_quiet_period_forgets_a_peer() {
        let mut throttle = KeyThrottle::default();
        let now = Instant::now();
        for _ in 0..FREE_KEY_FAILURES {
            throttle.failed(PEER, now);
        }
        assert!(!throttle.admits(PEER, now));
        throttle.succeeded(PEER);
        assert!(throttle.admits(PEER, now));
        assert_eq!(throttle.failed(PEER, now), None);

        let later = now + KEY_FAILURE_MEMORY;
        assert!(throttle.admits(PEER, later));
        assert!(throttle.peers.is_empty());
        assert_eq!(throttle.failed(PEER, later), None, "the count restarted");
    }

    #[test]
    fn ipv6_peers_share_their_prefix_and_mapped_ipv4_is_ipv4() {
        let mut throttle = KeyThrottle::default();
        let now = Instant::now();
        for host in 1..=u16::try_from(FREE_KEY_FAILURES).unwrap() {
            let peer = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, host));
            throttle.failed(peer, now);
        }
        let sibling = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 9, 9, 9, 9));
        assert!(!throttle.admits(sibling, now));
        let other = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 3, 0, 0, 0, 1));
        assert!(throttle.admits(other, now));

        for _ in 0..FREE_KEY_FAILURES {
            throttle.failed(PEER, now);
        }
        let IpAddr::V4(v4) = PEER else { unreachable!() };
        assert!(!throttle.admits(IpAddr::V6(v4.to_ipv6_mapped()), now));
    }

    #[test]
    fn memory_is_bounded_by_evicting_the_stalest_peer() {
        let mut throttle = KeyThrottle::default();
        let start = Instant::now();
        let peers = u32::try_from(KEY_THROTTLE_PEERS).unwrap() + 10;
        for index in 0..peers {
            let peer = IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + index));
            throttle.failed(peer, start + Duration::from_millis(u64::from(index)));
            assert!(throttle.peers.len() <= KEY_THROTTLE_PEERS);
        }
        assert_eq!(throttle.peers.len(), KEY_THROTTLE_PEERS);
        // The newest peer is still remembered; the oldest was evicted.
        let newest = IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + peers - 1));
        assert!(throttle.peers.contains_key(&newest));
        assert!(
            !throttle
                .peers
                .contains_key(&IpAddr::V4(Ipv4Addr::from(0x0a00_0000)))
        );
    }
}
