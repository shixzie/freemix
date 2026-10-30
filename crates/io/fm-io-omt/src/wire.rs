//! Reading and writing one OMT frame on a TCP stream, with timeouts.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::{FRAME_HEADER_BYTES, Frame, OmtError};

/// Writes one metadata record.
pub(crate) fn write_metadata(
    stream: &mut TcpStream,
    timestamp: i64,
    xml: &str,
) -> Result<(), OmtError> {
    let frame = Frame::Metadata(crate::MetadataFrame::new(xml)?.at(timestamp));
    let bytes = frame.encode()?;
    stream.write_all(&bytes)?;
    Ok(())
}

/// Reads one frame, `None` when `timeout` passed before the first header
/// byte arrived.
pub(crate) fn read_frame(
    stream: &mut TcpStream,
    timeout: Option<Duration>,
) -> Result<Option<Frame>, OmtError> {
    stream.set_read_timeout(timeout)?;
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    let mut read = 0;
    while read < header.len() {
        match stream.read(&mut header[read..]) {
            Ok(0) => return Err(OmtError::Closed),
            Ok(count) => read += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return if read == 0 {
                    Ok(None)
                } else {
                    Err(OmtError::Io("a frame header was cut short".to_owned()))
                };
            }
            Err(error) => return Err(error.into()),
        }
    }
    let length = crate::frame::frame_len(&header)?;
    let mut payload = vec![0_u8; length - FRAME_HEADER_BYTES];
    stream.read_exact(&mut payload)?;
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&payload);
    Frame::decode(&bytes).map(Some)
}

/// Reads one frame that must be metadata, `None` on a timeout.
pub(crate) fn read_metadata(
    stream: &mut TcpStream,
    timeout: Option<Duration>,
) -> Result<Option<String>, OmtError> {
    match read_frame(stream, timeout)? {
        Some(Frame::Metadata(metadata)) => Ok(Some(metadata.xml)),
        Some(_) => Err(OmtError::Metadata("a control channel carried media")),
        None => Ok(None),
    }
}
