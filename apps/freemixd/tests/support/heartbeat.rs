//! Keeps a test client's daemon session alive like a real client.
//!
//! freemixd closes any session that has not sent a heartbeat within its
//! session timeout (15 s), however busy the connection is otherwise, so a
//! test slowed by a loaded host would lose its session mid-run. A client
//! owns one [`Heartbeat`]: before each read it sends whatever [`Heartbeat::due`]
//! returns, and it passes every decoded message to [`Heartbeat::observe`],
//! which learns the server identity from the handshake response and tells
//! the client to skip acknowledgements of its own heartbeats, so the
//! messages a test checks are exactly those it saw before.

use std::time::{Duration, Instant};

use fm_protocol::{HeartbeatMessage, ServerIdentity, WireMessage};

/// How often a client heartbeats: well inside the daemon's 15 s timeout.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);

pub struct Heartbeat {
    server: Option<ServerIdentity>,
    sequence: u64,
    last: Instant,
}

impl Heartbeat {
    pub fn new() -> Self {
        Self {
            server: None,
            sequence: 0,
            last: Instant::now(),
        }
    }

    /// The heartbeat to send now, once the handshake has named the server
    /// and a heartbeat interval has passed since the last one.
    pub fn due(&mut self) -> Option<WireMessage> {
        let server = self.server.clone()?;
        if self.last.elapsed() < HEARTBEAT_EVERY {
            return None;
        }
        self.sequence += 1;
        self.last = Instant::now();
        Some(WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: self.sequence,
            sent_at_ms: self.sequence,
            last_applied: None,
        }))
    }

    /// Learns the server identity from a handshake response; true when
    /// `message` acknowledges one of this client's heartbeats, which the
    /// client skips.
    pub fn observe(&mut self, message: &WireMessage) -> bool {
        match message {
            WireMessage::HandshakeResponse(response) => {
                self.server = Some(response.server.clone());
                false
            }
            WireMessage::HeartbeatAcknowledgement(_) => true,
            _ => false,
        }
    }
}
