//! The OMT receiver: subscribe, tally, suggested quality, and frames.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::metadata::{Command, Quality, Tally};
use crate::wire::{read_frame, write_metadata};
use crate::{FrameKind, OmtError};

/// One connection to an OMT sender.
pub struct Receiver {
    stream: TcpStream,
    /// Frames skipped because their payload was corrupt but their length was
    /// intact.
    corrupt_frames: u64,
}

impl Receiver {
    /// Connects to a sender.
    ///
    /// # Errors
    ///
    /// The address must resolve and accept the connection.
    pub fn connect(address: impl ToSocketAddrs) -> Result<Self, OmtError> {
        let stream = TcpStream::connect(address)?;
        Self::from_stream(stream)
    }

    /// Connects to a sender, giving up after `timeout` for each address.
    ///
    /// # Errors
    ///
    /// The address must resolve and accept the connection in time.
    pub fn connect_timeout(
        address: impl ToSocketAddrs,
        timeout: Duration,
    ) -> Result<Self, OmtError> {
        let address = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| OmtError::Io("the address resolved to nothing".to_owned()))?;
        let stream = TcpStream::connect_timeout(&address, timeout)?;
        Self::from_stream(stream)
    }

    fn from_stream(stream: TcpStream) -> Result<Self, OmtError> {
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            corrupt_frames: 0,
        })
    }

    /// The remote sender's address.
    #[must_use]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.stream.peer_addr().ok()
    }

    /// Subscribes to the frame kinds the sender should now send.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn subscribe(&mut self, video: bool, audio: bool, metadata: bool) -> Result<(), OmtError> {
        for (subscribed, kind) in [
            (video, FrameKind::Video),
            (audio, FrameKind::Audio),
            (metadata, FrameKind::Metadata),
        ] {
            if subscribed {
                self.send(&Command::Subscribe(kind))?;
            }
        }
        Ok(())
    }

    /// Asks for one-eighth preview frames instead of the full picture.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn set_preview(&mut self, preview: bool) -> Result<(), OmtError> {
        self.send(&Command::Preview(preview))
    }

    /// Reports this receiver's tally to the sender.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn set_tally(&mut self, tally: Tally) -> Result<(), OmtError> {
        self.send(&Command::Tally(tally))
    }

    /// Suggests a compression quality to the sender.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn suggest_quality(&mut self, quality: Quality) -> Result<(), OmtError> {
        self.send(&Command::Quality(quality))
    }

    /// Reads the next frame, `None` when `timeout` passed before one
    /// arrived.
    ///
    /// # Errors
    ///
    /// The frame must be valid and the connection open.
    pub fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<crate::Frame>, OmtError> {
        read_frame(&mut self.stream, timeout)
    }

    /// Reads the next frame, skipping a frame whose payload is corrupt while
    /// its declared length was intact, so one bad frame never ends the
    /// stream. Length, transport, and header faults stay fatal: the stream
    /// may no longer be aligned after them.
    ///
    /// # Errors
    ///
    /// A fatal transport or framing failure.
    pub fn receive_skipping_corrupt(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Option<crate::Frame>, OmtError> {
        loop {
            match self.receive(timeout) {
                Ok(frame) => return Ok(frame),
                Err(error) if is_corrupt_frame(&error) => {
                    self.corrupt_frames = self.corrupt_frames.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Frames [`Self::receive_skipping_corrupt`] has skipped.
    #[must_use]
    pub const fn corrupt_frames(&self) -> u64 {
        self.corrupt_frames
    }

    /// Reads the next metadata record, `None` on a timeout.
    ///
    /// # Errors
    ///
    /// The record must be valid and the connection open.
    pub fn receive_metadata(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Option<String>, OmtError> {
        match self.receive(timeout)? {
            Some(crate::Frame::Metadata(metadata)) => Ok(Some(metadata.xml)),
            Some(_) => Err(OmtError::Metadata(
                "a media frame arrived on the control read",
            )),
            None => Ok(None),
        }
    }

    fn send(&mut self, command: &Command) -> Result<(), OmtError> {
        write_metadata(&mut self.stream, 0, &command.command())
    }
}

/// Whether a frame payload was fully read and can be skipped without losing
/// the stream's alignment.
const fn is_corrupt_frame(error: &OmtError) -> bool {
    matches!(
        error,
        OmtError::Codec(_)
            | OmtError::Video(_)
            | OmtError::Audio(_)
            | OmtError::Metadata(_)
            | OmtError::Utf8
    )
}
