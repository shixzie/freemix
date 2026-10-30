//! The OMT receiver: subscribe, tally, suggested quality, and frames.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::metadata::{Command, Quality, Tally};
use crate::wire::{read_frame, write_metadata};
use crate::{FrameKind, OmtError};

/// One connection to an OMT sender.
pub struct Receiver {
    stream: TcpStream,
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
        Ok(Self { stream })
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
