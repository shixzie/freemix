//! VISCA over an RS-232 byte stream (IN-028).
//!
//! [`ViscaSerialTransport`] owns a caller-supplied [`SerialPort`] and exchanges
//! one VISCA command at a time with a camera: it writes a complete frame,
//! reads until a reply terminator arrives, and returns the parsed reply.
//! Framing, message bounds, malformed-reply recovery, and counters come from
//! the shared VISCA codec; opening a real serial device belongs to an
//! integration crate.
//!
//! Reads are blocking and bounded by the timeout the port applies in
//! [`SerialPort::set_read_timeout`]; a port that reports `WouldBlock` or
//! `TimedOut` surfaces [`ViscaSerialError::Timeout`]. Replies completed in one
//! read are queued, so a camera that sends an acknowledgement and a completion
//! together keeps both.

use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    time::Duration,
};

use crate::visca::{
    VISCA_MAX_MESSAGE_LEN, VISCA_TERMINATOR, ViscaParseError, ViscaReply, ViscaReplyDecoder,
};

/// Largest command accepted by [`ViscaSerialTransport::send`].
pub const MAX_VISCA_SERIAL_COMMAND_LEN: usize = VISCA_MAX_MESSAGE_LEN;

/// A blocking serial byte stream owned by [`ViscaSerialTransport`].
///
/// An integration crate wraps its platform serial device in this trait.
pub trait SerialPort: Read + Write {
    /// Applies the timeout used by blocking reads; `None` waits forever.
    ///
    /// # Errors
    ///
    /// Returns the platform's timeout-configuration error.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
}

/// Counters kept by one serial transport.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViscaSerialCounters {
    /// Commands successfully written.
    pub commands: u64,
    /// Replies successfully parsed and returned.
    pub replies: u64,
    /// Replies the decoder rejected.
    pub malformed_replies: u64,
    /// Reads that timed out waiting for a reply.
    pub timeouts: u64,
    /// Command bytes written to the port.
    pub bytes_written: u64,
}

/// Why a serial VISCA exchange failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaSerialError {
    /// A command is empty, longer than a VISCA message, lacks its
    /// terminator, or embeds one.
    InvalidCommand,
    /// No complete reply arrived before the port's read timeout.
    Timeout,
    /// The port failed; the kind is kept for diagnostics.
    Io(io::ErrorKind),
    /// A complete reply did not decode.
    Malformed(ViscaParseError),
}

impl fmt::Display for ViscaSerialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand => formatter.write_str("VISCA serial command is not a frame"),
            Self::Timeout => formatter.write_str("VISCA serial reply timed out"),
            Self::Io(kind) => write!(formatter, "VISCA serial port failed: {kind:?}"),
            Self::Malformed(error) => write!(formatter, "VISCA serial reply is malformed: {error}"),
        }
    }
}

impl std::error::Error for ViscaSerialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ViscaSerialError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// One camera's blocking VISCA serial link.
pub struct ViscaSerialTransport<P: SerialPort> {
    port: P,
    decoder: ViscaReplyDecoder,
    pending: VecDeque<Result<ViscaReply, ViscaParseError>>,
    counters: ViscaSerialCounters,
    timeout: Duration,
}

impl<P: SerialPort> ViscaSerialTransport<P> {
    /// Takes ownership of `port` and applies `timeout` to blocking reads.
    ///
    /// # Errors
    ///
    /// Returns the port's timeout-configuration error.
    pub fn new(mut port: P, timeout: Duration) -> io::Result<Self> {
        port.set_read_timeout(Some(timeout))?;
        Ok(Self {
            port,
            decoder: ViscaReplyDecoder::new(),
            pending: VecDeque::new(),
            counters: ViscaSerialCounters::default(),
            timeout,
        })
    }

    /// The configured reply timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The transport's counters.
    #[must_use]
    pub const fn counters(&self) -> ViscaSerialCounters {
        self.counters
    }

    /// Writes one complete VISCA command frame.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaSerialError::InvalidCommand`] for a non-frame and the
    /// port's error for a failed write or flush.
    pub fn send(&mut self, command: &[u8]) -> Result<(), ViscaSerialError> {
        if command.is_empty()
            || command.len() > MAX_VISCA_SERIAL_COMMAND_LEN
            || command.last() != Some(&VISCA_TERMINATOR)
            || command[..command.len() - 1].contains(&VISCA_TERMINATOR)
        {
            return Err(ViscaSerialError::InvalidCommand);
        }
        self.port.write_all(command)?;
        self.port.flush()?;
        self.counters.commands = self.counters.commands.saturating_add(1);
        self.counters.bytes_written = self
            .counters
            .bytes_written
            .saturating_add(u64::try_from(command.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Waits for the next complete reply.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaSerialError::Timeout`] when the port's read timeout
    /// expires, [`ViscaSerialError::Malformed`] for a complete reply that does
    /// not decode, and [`ViscaSerialError::Io`] for any other port failure.
    pub fn receive(&mut self) -> Result<ViscaReply, ViscaSerialError> {
        if let Some(result) = self.pending.pop_front() {
            return self.finish(result);
        }
        let mut buffer = [0_u8; 64];
        loop {
            match self.port.read(&mut buffer) {
                Ok(0) => return Err(ViscaSerialError::Io(io::ErrorKind::UnexpectedEof)),
                Ok(read) => {
                    for result in self.decoder.push(&buffer[..read]) {
                        self.pending.push_back(result);
                    }
                    if let Some(result) = self.pending.pop_front() {
                        return self.finish(result);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    self.counters.timeouts = self.counters.timeouts.saturating_add(1);
                    return Err(ViscaSerialError::Timeout);
                }
                Err(error) => return Err(ViscaSerialError::Io(error.kind())),
            }
        }
    }

    /// Sends one command and waits for its first reply.
    ///
    /// # Errors
    ///
    /// Returns the send or receive error.
    pub fn transaction(&mut self, command: &[u8]) -> Result<ViscaReply, ViscaSerialError> {
        self.send(command)?;
        self.receive()
    }

    fn finish(
        &mut self,
        result: Result<ViscaReply, ViscaParseError>,
    ) -> Result<ViscaReply, ViscaSerialError> {
        match result {
            Ok(reply) => {
                self.counters.replies = self.counters.replies.saturating_add(1);
                Ok(reply)
            }
            Err(error) => {
                self.counters.malformed_replies = self.counters.malformed_replies.saturating_add(1);
                Err(ViscaSerialError::Malformed(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakePort {
        replies: VecDeque<u8>,
        written: Vec<u8>,
        timeout: Option<Duration>,
        block: bool,
    }

    impl FakePort {
        fn with_replies(replies: &[u8], block: bool) -> Self {
            Self {
                replies: replies.iter().copied().collect(),
                written: Vec::new(),
                timeout: None,
                block,
            }
        }
    }

    impl Read for FakePort {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.replies.is_empty() {
                return if self.block {
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                } else {
                    Ok(0)
                };
            }
            let take = buffer.len().min(self.replies.len());
            for (slot, byte) in buffer.iter_mut().zip(self.replies.drain(..take)) {
                *slot = byte;
            }
            Ok(take)
        }
    }

    impl Write for FakePort {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for FakePort {
        fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.timeout = timeout;
            Ok(())
        }
    }

    fn transport(replies: &[u8], block: bool) -> ViscaSerialTransport<FakePort> {
        ViscaSerialTransport::new(
            FakePort::with_replies(replies, block),
            Duration::from_millis(5),
        )
        .unwrap()
    }

    const PAN_TILT: [u8; 9] = [0x81, 0x01, 0x06, 0x01, 0x01, 0x01, 0x01, 0x01, 0xFF];

    #[test]
    fn a_transaction_writes_the_frame_and_parses_the_ack() {
        let mut transport = transport(&[0x90, 0x41, 0xFF], false);
        assert_eq!(
            transport.transaction(&PAN_TILT).unwrap(),
            ViscaReply::Ack {
                address: 1,
                socket: 1
            }
        );
        assert_eq!(transport.port.written, PAN_TILT);
        assert_eq!(transport.port.timeout, Some(Duration::from_millis(5)));
        assert_eq!(
            transport.counters(),
            ViscaSerialCounters {
                commands: 1,
                replies: 1,
                malformed_replies: 0,
                timeouts: 0,
                bytes_written: 9,
            }
        );
    }

    #[test]
    fn replies_completed_in_one_read_are_kept_in_order() {
        let mut transport = transport(&[0x90, 0x41, 0xFF, 0x90, 0x51, 0xFF], false);
        transport.send(&PAN_TILT).unwrap();
        assert_eq!(
            transport.receive().unwrap(),
            ViscaReply::Ack {
                address: 1,
                socket: 1
            }
        );
        assert_eq!(
            transport.receive().unwrap(),
            ViscaReply::Completion {
                address: 1,
                socket: 1,
                data: Vec::new()
            }
        );
        assert_eq!(transport.counters().replies, 2);
    }

    #[test]
    fn malformed_and_timed_out_replies_are_typed() {
        let mut malformed = transport(&[0x01, 0xFF], false);
        assert!(matches!(
            malformed.transaction(&PAN_TILT),
            Err(ViscaSerialError::Malformed(ViscaParseError::TooShort))
        ));
        assert_eq!(malformed.counters().malformed_replies, 1);

        let mut timed_out = transport(&[], true);
        assert_eq!(
            timed_out.transaction(&PAN_TILT),
            Err(ViscaSerialError::Timeout)
        );
        assert_eq!(timed_out.counters().timeouts, 1);

        let mut closed = transport(&[], false);
        assert_eq!(
            closed.transaction(&PAN_TILT),
            Err(ViscaSerialError::Io(io::ErrorKind::UnexpectedEof))
        );
    }

    #[test]
    fn only_complete_bounded_frames_are_sent() {
        let mut transport = transport(&[0x90, 0x41, 0xFF], false);
        for command in [
            &[][..],
            &[0x81, 0x01][..],
            &[0x81, 0x01, 0x00, 0xFF, 0xFF][..],
            &[0x81; 17][..],
        ] {
            assert_eq!(
                transport.send(command),
                Err(ViscaSerialError::InvalidCommand),
                "{command:?}"
            );
        }
        assert!(transport.send(&[0x81, 0x01, 0x00, 0xFF]).is_ok());
        assert_eq!(transport.counters().commands, 1);
    }
}
