//! A bounded UDP transport for VISCA-over-IP cameras (IN-028).
//!
//! The codecs in [`crate::visca`] and [`crate::visca_ip`] are pure. This
//! module is the network boundary a camera adapter needs: one connected UDP
//! peer, sequence-numbered datagrams, bounded retransmission, a per-attempt
//! read timeout, reply correlation by sequence, and recovery from the
//! camera's abnormal-sequence control reply.
//!
//! Nothing here reads a wall clock or blocks past the configured timeout.

use std::{
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    time::{Duration, Instant},
};

use crate::visca::VISCA_MAX_MESSAGE_LEN;
use crate::visca_ip::{
    VISCA_IP_HEADER_LEN, ViscaIpControl, ViscaIpError, ViscaIpPacket, ViscaIpPayloadType,
    ViscaIpSequencer,
};

/// The VISCA-over-IP UDP port Sony cameras listen on.
pub const VISCA_IP_DEFAULT_PORT: u16 = 52_381;
/// Largest datagram this transport accepts: one header and one maximum
/// payload.
pub const MAX_VISCA_IP_DATAGRAM: usize = VISCA_IP_HEADER_LEN + VISCA_MAX_MESSAGE_LEN;
/// Default per-attempt read timeout.
pub const DEFAULT_VISCA_TIMEOUT: Duration = Duration::from_millis(500);
/// Default total attempts (the first send plus retransmissions).
pub const DEFAULT_VISCA_ATTEMPTS: u32 = 3;
/// Default bound on sequence resynchronisations per transaction.
pub const DEFAULT_VISCA_SEQUENCE_RESETS: u32 = 2;

/// Bounded retransmission policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViscaUdpConfig {
    /// How long one attempt waits for a reply.
    pub timeout: Duration,
    /// Total sends per transaction, at least one.
    pub attempts: u32,
    /// How many abnormal-sequence replies may be recovered per transaction.
    pub sequence_resets: u32,
}

impl Default for ViscaUdpConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_VISCA_TIMEOUT,
            attempts: DEFAULT_VISCA_ATTEMPTS,
            sequence_resets: DEFAULT_VISCA_SEQUENCE_RESETS,
        }
    }
}

impl ViscaUdpConfig {
    /// Whether every value is usable.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        !self.timeout.is_zero() && self.attempts > 0
    }
}

/// Counters an operator or camera status can show.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViscaUdpCounters {
    pub sent: u64,
    pub retransmitted: u64,
    /// Well-framed datagrams received from the peer.
    pub received: u64,
    /// Datagrams that did not frame or were too large.
    pub malformed: u64,
    /// Well-framed replies whose sequence did not match the request.
    pub unmatched: u64,
    pub timeouts: u64,
    pub sequence_resets: u64,
}

/// Why one VISCA transaction failed.
#[derive(Debug)]
pub enum ViscaUdpError {
    Io(io::Error),
    Encode(ViscaIpError),
    /// No matching reply arrived within `attempts * timeout`.
    Timeout,
    /// The camera reported an abnormal sequence number and the reset did
    /// not recover it within the configured bound.
    SequenceUnresolved,
    /// The camera reported an abnormal message type.
    MessageType,
}

impl std::fmt::Display for ViscaUdpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "VISCA-over-IP transport: {error}"),
            Self::Encode(error) => write!(formatter, "VISCA-over-IP framing: {error}"),
            Self::Timeout => formatter.write_str("VISCA-over-IP camera did not reply"),
            Self::SequenceUnresolved => {
                formatter.write_str("VISCA-over-IP camera rejected the sequence number")
            }
            Self::MessageType => formatter.write_str("VISCA-over-IP camera rejected the message"),
        }
    }
}

impl std::error::Error for ViscaUdpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ViscaUdpError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ViscaIpError> for ViscaUdpError {
    fn from(error: ViscaIpError) -> Self {
        Self::Encode(error)
    }
}

/// A connected VISCA-over-IP UDP endpoint.
#[derive(Debug)]
pub struct ViscaUdpTransport {
    socket: UdpSocket,
    remote: SocketAddr,
    sequencer: ViscaIpSequencer,
    config: ViscaUdpConfig,
    counters: ViscaUdpCounters,
}

impl ViscaUdpTransport {
    /// Binds `local` and talks only to `remote`.
    ///
    /// # Errors
    ///
    /// Returns a bind error, or [`io::ErrorKind::InvalidInput`] for an
    /// unusable config or a `remote` that resolves to nothing.
    pub fn bind(
        local: impl ToSocketAddrs,
        remote: impl ToSocketAddrs,
        config: ViscaUdpConfig,
    ) -> io::Result<Self> {
        if !config.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VISCA-over-IP timeout and attempts must be non-zero",
            ));
        }
        let remote = remote
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no remote address"))?;
        let socket = UdpSocket::bind(local)?;
        socket.connect(remote)?;
        Self::from_socket(socket, remote, config)
    }

    /// Uses an already connected socket.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for an unusable config.
    pub fn from_socket(
        socket: UdpSocket,
        remote: SocketAddr,
        config: ViscaUdpConfig,
    ) -> io::Result<Self> {
        if !config.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VISCA-over-IP timeout and attempts must be non-zero",
            ));
        }
        Ok(Self {
            socket,
            remote,
            sequencer: ViscaIpSequencer::new(),
            config,
            counters: ViscaUdpCounters::default(),
        })
    }

    #[must_use]
    pub const fn remote(&self) -> SocketAddr {
        self.remote
    }

    #[must_use]
    pub const fn config(&self) -> ViscaUdpConfig {
        self.config
    }

    #[must_use]
    pub const fn counters(&self) -> ViscaUdpCounters {
        self.counters
    }

    #[must_use]
    pub const fn sequencer(&self) -> ViscaIpSequencer {
        self.sequencer
    }

    /// Sends one packet without waiting for a reply.
    ///
    /// # Errors
    ///
    /// Returns a framing or send error.
    pub fn send(&mut self, payload_type: ViscaIpPayloadType, payload: Vec<u8>) -> Result<u32, ViscaUdpError> {
        let packet = self.sequencer.wrap(payload_type, payload);
        let bytes = packet.encode()?;
        self.socket.send(&bytes)?;
        self.counters.sent = self.counters.sent.saturating_add(1);
        Ok(packet.sequence)
    }

    /// Sends one payload and waits for the reply carrying its sequence,
    /// retransmitting the same datagram up to the configured attempts.
    ///
    /// A camera's abnormal-sequence control reply resets the sequencer and
    /// retries the payload with a new sequence, up to
    /// [`ViscaUdpConfig::sequence_resets`] times.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaUdpError::Timeout`] when no matching reply arrives,
    /// [`ViscaUdpError::SequenceUnresolved`] when resynchronisation fails,
    /// [`ViscaUdpError::MessageType`] for an abnormal-message reply, and a
    /// framing or socket error.
    pub fn transact(
        &mut self,
        payload_type: ViscaIpPayloadType,
        payload: &[u8],
    ) -> Result<ViscaIpPacket, ViscaUdpError> {
        let mut resets = 0;
        loop {
            let packet = self.sequencer.wrap(payload_type, payload.to_vec());
            let bytes = packet.encode()?;
            match self.exchange(&bytes, packet.sequence, expected_reply(payload_type))? {
                Exchange::Reply(reply) => return Ok(reply),
                Exchange::SequenceNumberError => {
                    if resets >= self.config.sequence_resets {
                        self.counters.sequence_resets = self.counters.sequence_resets.saturating_add(1);
                        return Err(ViscaUdpError::SequenceUnresolved);
                    }
                    resets += 1;
                    self.counters.sequence_resets = self.counters.sequence_resets.saturating_add(1);
                    self.resync()?;
                }
                Exchange::MessageTypeError => return Err(ViscaUdpError::MessageType),
            }
        }
    }

    /// Sends the control RESET and waits for its acknowledgement.
    fn resync(&mut self) -> Result<(), ViscaUdpError> {
        let reset = self.sequencer.reset();
        let bytes = reset.encode()?;
        match self.exchange(
            &bytes,
            reset.sequence,
            ViscaIpPayloadType::ControlReply,
        )? {
            Exchange::Reply(reply)
                if ViscaIpControl::decode(reply.payload_type, &reply.payload)
                    == Some(ViscaIpControl::ResetAck) =>
            {
                Ok(())
            }
            Exchange::Reply(_) | Exchange::SequenceNumberError | Exchange::MessageTypeError => {
                Err(ViscaUdpError::SequenceUnresolved)
            }
        }
    }

    /// Sends `bytes` once per attempt and waits for a correlated reply.
    fn exchange(
        &mut self,
        bytes: &[u8],
        sequence: u32,
        expected: ViscaIpPayloadType,
    ) -> Result<Exchange, ViscaUdpError> {
        for attempt in 0..self.config.attempts {
            self.socket.send(bytes)?;
            self.counters.sent = self.counters.sent.saturating_add(1);
            if attempt > 0 {
                self.counters.retransmitted = self.counters.retransmitted.saturating_add(1);
            }
            let deadline = Instant::now() + self.config.timeout;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                self.socket.set_read_timeout(Some(remaining))?;
                let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
                match self.socket.recv(&mut buffer) {
                    Ok(len) => {
                        self.counters.received = self.counters.received.saturating_add(1);
                        let Ok(packet) = ViscaIpPacket::decode(&buffer[..len]) else {
                            self.counters.malformed = self.counters.malformed.saturating_add(1);
                            continue;
                        };
                        if packet.sequence != sequence {
                            self.counters.unmatched = self.counters.unmatched.saturating_add(1);
                            continue;
                        }
                        if packet.payload_type == ViscaIpPayloadType::ControlReply {
                            match ViscaIpControl::decode(packet.payload_type, &packet.payload) {
                                Some(ViscaIpControl::SequenceNumberError) => {
                                    return Ok(Exchange::SequenceNumberError);
                                }
                                Some(ViscaIpControl::MessageTypeError) => {
                                    return Ok(Exchange::MessageTypeError);
                                }
                                _ => {}
                            }
                        }
                        if packet.payload_type == expected {
                            return Ok(Exchange::Reply(packet));
                        }
                        // A correlated packet of another type is not the
                        // reply this transaction asked for.
                        self.counters.unmatched = self.counters.unmatched.saturating_add(1);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => return Err(ViscaUdpError::Io(error)),
                }
            }
        }
        self.counters.timeouts = self.counters.timeouts.saturating_add(1);
        Err(ViscaUdpError::Timeout)
    }
}

enum Exchange {
    Reply(ViscaIpPacket),
    SequenceNumberError,
    MessageTypeError,
}

const fn expected_reply(request: ViscaIpPayloadType) -> ViscaIpPayloadType {
    match request {
        ViscaIpPayloadType::ControlCommand => ViscaIpPayloadType::ControlReply,
        _ => ViscaIpPayloadType::Reply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::UdpSocket, thread};

    const COMMAND: [u8; 6] = [0x81, 0x01, 0x06, 0x01, 0x03, 0x03];
    const REPLY: [u8; 3] = [0x90, 0x50, 0xFF];

    fn config(timeout_ms: u64, attempts: u32) -> ViscaUdpConfig {
        ViscaUdpConfig {
            timeout: Duration::from_millis(timeout_ms),
            attempts,
            sequence_resets: 2,
        }
    }

    fn client(server: SocketAddr, config: ViscaUdpConfig) -> ViscaUdpTransport {
        ViscaUdpTransport::bind("127.0.0.1:0", server, config).unwrap()
    }

    #[test]
    fn a_correlated_reply_is_returned() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let packet = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            assert_eq!(packet.payload_type, ViscaIpPayloadType::Command);
            assert_eq!(packet.payload, COMMAND);
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::Reply,
                        sequence: packet.sequence,
                        payload: REPLY.to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
        });
        let mut transport = client(address, config(500, 2));
        let reply = transport
            .transact(ViscaIpPayloadType::Command, &COMMAND)
            .unwrap();
        assert_eq!(reply.payload_type, ViscaIpPayloadType::Reply);
        assert_eq!(reply.payload, REPLY);
        assert_eq!(transport.counters().sent, 1);
        worker.join().unwrap();
    }

    #[test]
    fn a_dropped_datagram_is_retransmitted() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
            let (first, _) = server.recv_from(&mut buffer).unwrap();
            assert!(first > 0);
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let packet = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::Reply,
                        sequence: packet.sequence,
                        payload: REPLY.to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
        });
        let mut transport = client(address, config(200, 2));
        transport
            .transact(ViscaIpPayloadType::Command, &COMMAND)
            .unwrap();
        assert_eq!(transport.counters().retransmitted, 1);
        worker.join().unwrap();
    }

    #[test]
    fn a_missing_reply_times_out_after_every_attempt() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
            for _ in 0..2 {
                server.recv_from(&mut buffer).unwrap();
            }
        });
        let mut transport = client(address, config(40, 2));
        assert!(matches!(
            transport.transact(ViscaIpPayloadType::Command, &COMMAND),
            Err(ViscaUdpError::Timeout)
        ));
        assert_eq!(transport.counters().timeouts, 1);
        assert_eq!(transport.counters().retransmitted, 1);
        worker.join().unwrap();
    }

    #[test]
    fn an_unmatched_reply_is_ignored() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let packet = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            let stale = ViscaIpPacket {
                payload_type: ViscaIpPayloadType::Reply,
                sequence: packet.sequence.wrapping_sub(1),
                payload: REPLY.to_vec(),
            }
            .encode()
            .unwrap();
            server.send_to(&stale, source).unwrap();
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::Reply,
                        sequence: packet.sequence,
                        payload: REPLY.to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
        });
        let mut transport = client(address, config(500, 1));
        transport
            .transact(ViscaIpPayloadType::Command, &COMMAND)
            .unwrap();
        assert_eq!(transport.counters().unmatched, 1);
        worker.join().unwrap();
    }

    #[test]
    fn an_abnormal_sequence_reply_resets_and_retries() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut buffer = [0_u8; MAX_VISCA_IP_DATAGRAM];
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let packet = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            // Reject the first command's sequence.
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::ControlReply,
                        sequence: packet.sequence,
                        payload: ViscaIpControl::SequenceNumberError.payload().to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
            // Acknowledge the reset.
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let reset = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            assert_eq!(reset.payload_type, ViscaIpPayloadType::ControlCommand);
            assert_eq!(ViscaIpControl::decode(reset.payload_type, &reset.payload), Some(ViscaIpControl::Reset));
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::ControlReply,
                        sequence: reset.sequence,
                        payload: ViscaIpControl::ResetAck.payload().to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
            // Answer the retried command.
            let (len, source) = server.recv_from(&mut buffer).unwrap();
            let retried = ViscaIpPacket::decode(&buffer[..len]).unwrap();
            assert_eq!(retried.payload_type, ViscaIpPayloadType::Command);
            server
                .send_to(
                    &ViscaIpPacket {
                        payload_type: ViscaIpPayloadType::Reply,
                        sequence: retried.sequence,
                        payload: REPLY.to_vec(),
                    }
                    .encode()
                    .unwrap(),
                    source,
                )
                .unwrap();
        });
        let mut transport = client(address, config(1_000, 2));
        let reply = transport
            .transact(ViscaIpPayloadType::Command, &COMMAND)
            .unwrap();
        assert_eq!(reply.payload, REPLY);
        assert_eq!(transport.counters().sequence_resets, 1);
        worker.join().unwrap();
    }
}
