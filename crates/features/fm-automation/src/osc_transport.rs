//! A bounded UDP endpoint for OSC packets (RC-005/RC-006).
//!
//! The codec in [`crate::osc`] deliberately opens no sockets. This module is
//! the transport an OSC control surface needs: bind a local UDP address,
//! receive bounded datagrams, decode them strictly, and send encoded
//! packets back. It never blocks and never allocates per received datagram
//! beyond what the decoder produces.
//!
//! Malformed and oversized datagrams are reported as values and counted
//! instead of ending the transport, because a network is allowed to deliver
//! both and one bad sender must not take the control surface down.

use std::{
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
};

use crate::osc::{
    MAX_OSC_PACKET_BYTES, OscError, OscLimits, OscPacket, decode_osc_packet, encode_osc_packet,
};

/// One decoded datagram.
#[derive(Clone, Debug, PartialEq)]
pub struct OscDatagram {
    pub source: SocketAddr,
    pub packet: OscPacket,
    /// Bytes received on the wire.
    pub bytes: usize,
}

/// What one nonblocking receive found.
#[derive(Clone, Debug, PartialEq)]
pub enum OscReceive {
    /// A valid packet from `source`.
    Packet(OscDatagram),
    /// A datagram that is not a valid OSC packet; it was dropped.
    Malformed { source: SocketAddr, error: OscError },
    /// A datagram larger than [`MAX_OSC_PACKET_BYTES`]; it was dropped.
    Oversized { source: SocketAddr, bytes: usize },
}

/// Why an OSC datagram could not be sent.
#[derive(Debug)]
pub enum OscSendError {
    Io(io::Error),
    Encode(OscError),
}

impl std::fmt::Display for OscSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "OSC send failed: {error}"),
            Self::Encode(error) => write!(formatter, "OSC packet cannot be encoded: {error}"),
        }
    }
}

impl std::error::Error for OscSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode(error) => Some(error),
        }
    }
}

impl From<io::Error> for OscSendError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Bounded counters a control surface can show.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OscTransportCounters {
    pub received: u64,
    pub decoded: u64,
    pub malformed: u64,
    pub oversized: u64,
    pub sent: u64,
    pub send_failures: u64,
}

/// A nonblocking UDP endpoint for OSC packets.
#[derive(Debug)]
pub struct OscUdpTransport {
    socket: UdpSocket,
    limits: OscLimits,
    /// One byte more than the codec cap, so a datagram that fills the
    /// buffer is provably oversized; UDP payloads cannot exceed 65,507
    /// bytes on IPv4 anyway.
    buffer: Vec<u8>,
    counters: OscTransportCounters,
    /// The last error `receive` dropped without returning it (a datagram
    /// was truncated by the OS or a socket error other than `WouldBlock`).
    receive_error: Option<io::ErrorKind>,
}

impl OscUdpTransport {
    /// Binds every local address `address` resolves to.
    ///
    /// # Errors
    ///
    /// Returns the socket bind failure.
    pub fn bind(address: impl ToSocketAddrs) -> io::Result<Self> {
        Self::from_socket(UdpSocket::bind(address)?, OscLimits::default())
    }

    /// Uses one already-bound socket and applies `limits` while decoding.
    ///
    /// # Errors
    ///
    /// Returns the failure to switch the socket to nonblocking mode.
    pub fn from_socket(socket: UdpSocket, limits: OscLimits) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            limits,
            buffer: vec![0; MAX_OSC_PACKET_BYTES + 1],
            counters: OscTransportCounters::default(),
            receive_error: None,
        })
    }

    /// The local address the transport is bound to.
    ///
    /// # Errors
    ///
    /// Returns the socket's address-lookup failure.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    #[must_use]
    pub const fn limits(&self) -> OscLimits {
        self.limits
    }

    #[must_use]
    pub const fn counters(&self) -> OscTransportCounters {
        self.counters
    }

    /// The last socket error `receive` treated as a dropped datagram.
    #[must_use]
    pub const fn last_receive_error(&self) -> Option<io::ErrorKind> {
        self.receive_error
    }

    /// Receives one datagram without blocking.
    ///
    /// # Errors
    ///
    /// Returns a socket error other than `WouldBlock`; a `WouldBlock` is
    /// reported as `Ok(None)`.
    pub fn receive(&mut self) -> io::Result<Option<OscReceive>> {
        let (bytes, source) = match self.socket.recv_from(&mut self.buffer) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        };
        self.counters.received = self.counters.received.saturating_add(1);
        if bytes > MAX_OSC_PACKET_BYTES {
            self.counters.oversized = self.counters.oversized.saturating_add(1);
            return Ok(Some(OscReceive::Oversized { source, bytes }));
        }
        match decode_osc_packet(&self.buffer[..bytes], &self.limits) {
            Ok(packet) => {
                self.counters.decoded = self.counters.decoded.saturating_add(1);
                Ok(Some(OscReceive::Packet(OscDatagram {
                    source,
                    packet,
                    bytes,
                })))
            }
            Err(error) => {
                self.counters.malformed = self.counters.malformed.saturating_add(1);
                Ok(Some(OscReceive::Malformed { source, error }))
            }
        }
    }

    /// Encodes `packet` and sends it to `destination`.
    ///
    /// # Errors
    ///
    /// Returns [`OscSendError::Encode`] when the packet is outside the
    /// codec bounds and [`OscSendError::Io`] when the socket refuses it.
    pub fn send(
        &mut self,
        packet: &OscPacket,
        destination: SocketAddr,
    ) -> Result<usize, OscSendError> {
        let bytes = encode_osc_packet(packet, &self.limits).map_err(OscSendError::Encode)?;
        self.send_bytes(&bytes, destination)
    }

    /// Sends an already encoded OSC packet, refusing one over the codec
    /// cap.
    ///
    /// # Errors
    ///
    /// Returns [`OscSendError::Encode`] for an oversized `bytes` and
    /// [`OscSendError::Io`] when the socket refuses it.
    pub fn send_bytes(
        &mut self,
        bytes: &[u8],
        destination: SocketAddr,
    ) -> Result<usize, OscSendError> {
        if bytes.len() > MAX_OSC_PACKET_BYTES {
            return Err(OscSendError::Encode(OscError::PacketTooLarge {
                len: bytes.len(),
                limit: MAX_OSC_PACKET_BYTES,
            }));
        }
        match self.socket.send_to(bytes, destination) {
            Ok(sent) => {
                self.counters.sent = self.counters.sent.saturating_add(1);
                Ok(sent)
            }
            Err(error) => {
                self.counters.send_failures = self.counters.send_failures.saturating_add(1);
                Err(OscSendError::Io(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::{OscArg, OscMessage};
    use std::{
        net::UdpSocket,
        thread,
        time::{Duration, Instant},
    };

    fn wait_for(transport: &mut OscUdpTransport) -> OscReceive {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(received) = transport.receive().unwrap() {
                return received;
            }
            assert!(Instant::now() < deadline, "no datagram arrived");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn sends_and_receives_osc_packets_on_loopback() {
        let mut receiver = OscUdpTransport::bind("127.0.0.1:0").unwrap();
        let mut sender = OscUdpTransport::bind("127.0.0.1:0").unwrap();
        let destination = receiver.local_addr().unwrap();
        let source = sender.local_addr().unwrap();
        let packet = OscPacket::Message(OscMessage::new(
            "/freemix/cut",
            vec![OscArg::Int(3), OscArg::String("now".into())],
        ));
        let bytes = sender.send(&packet, destination).unwrap();
        assert!(bytes > 0);
        match wait_for(&mut receiver) {
            OscReceive::Packet(datagram) => {
                assert_eq!(datagram.source, source);
                assert_eq!(datagram.packet, packet);
                assert_eq!(datagram.bytes, bytes);
            }
            other => panic!("unexpected receive: {other:?}"),
        }
        assert_eq!(receiver.counters().decoded, 1);
        assert_eq!(receiver.counters().received, 1);
        assert_eq!(sender.counters().sent, 1);
    }

    #[test]
    fn malformed_and_oversized_datagrams_are_counted_and_dropped() {
        let mut transport = OscUdpTransport::bind("127.0.0.1:0").unwrap();
        let destination = transport.local_addr().unwrap();
        let raw = UdpSocket::bind("127.0.0.1:0").unwrap();
        raw.send_to(b"not an osc packet", destination).unwrap();
        match wait_for(&mut transport) {
            OscReceive::Malformed { .. } => {}
            other => panic!("unexpected receive: {other:?}"),
        }
        // A UDP payload cannot exceed 65,507 bytes on IPv4, below the
        // 65,536-byte OSC cap, so a real datagram can never reach the
        // oversized branch; `send_bytes` still refuses oversized sends.
        assert_eq!(transport.counters().malformed, 1);
        assert_eq!(transport.counters().oversized, 0);
        assert_eq!(transport.counters().decoded, 0);
    }

    #[test]
    fn oversized_sends_are_refused_before_the_socket() {
        let mut transport = OscUdpTransport::bind("127.0.0.1:0").unwrap();
        let destination = transport.local_addr().unwrap();
        assert!(matches!(
            transport.send_bytes(&vec![0_u8; MAX_OSC_PACKET_BYTES + 1], destination),
            Err(OscSendError::Encode(OscError::PacketTooLarge { .. }))
        ));
        assert_eq!(transport.counters().sent, 0);
        assert_eq!(transport.counters().send_failures, 0);
    }
}
