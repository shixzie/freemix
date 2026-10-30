//! Open Media Transport (OMT) 1.0, implemented by `FreeMix`.
//!
//! OMT is an open, MIT-licensed protocol for low-latency video, audio, and
//! metadata over a LAN. `FreeMix` implements it here from the published
//! protocol so the product depends on no licensed vendor SDK: this crate is
//! the transport itself, not a binding.
//!
//! * [`frame`] — the 16-byte frame header, the 32-byte video and 24-byte
//!   audio extended headers, the uncompressed codecs, and bounded
//!   encode/decode of one frame.
//! * [`metadata`] — the exact control strings a receiver sends
//!   (`OMTSubscribe`, `OMTSettings`, `OMTTally`, `OMTInfo`) and their parse.
//! * [`discovery`] — the `_omt._tcp` service name, the register/deregister
//!   `OMTAddress` records, and the discovery server's registry rules.
//! * [`receiver`] — a blocking receiver connection: subscribe, preview,
//!   tally, suggested quality, and frame reception.
//! * [`sender`] — the sender: one TCP listener, one session per receiver,
//!   subscription gating, the combined tally, and frame fan-out.
//!
//! Every record is bounded and every decode validates before it allocates.
//!
//! A poisoned internal lock is a programming error; the crate documents it
//! as a panic in the few places a lock can be poisoned.
#![allow(clippy::missing_panics_doc)]

mod discovery;
mod dns_sd;
mod frame;
mod metadata;
mod receiver;
mod sender;
mod wire;

use std::fmt;

pub use discovery::{
    DISCOVERY_PORT, DiscoveryClient, DiscoveryError, DiscoveryRegistry, DiscoveryServer,
    OMT_SERVICE_TYPE, OmtAddress, SOURCE_PORT_END, SOURCE_PORT_START,
};
pub use dns_sd::{OMT_DNS_SD_SERVICE, OmtAdvertiser, browse, service_info};
pub use frame::{
    AUDIO_EXTENDED_HEADER_BYTES, AudioFrame, ColorSpace, FRAME_HEADER_BYTES, Frame, FrameKind,
    MAX_FRAME_BYTES, MAX_METADATA_BYTES, MetadataFrame, OMT_VERSION, VIDEO_EXTENDED_HEADER_BYTES,
    VideoCodec, VideoFlags, VideoFrame, frame_len,
};
pub use metadata::{
    Command, PREVIEW_OFF, PREVIEW_ON, Quality, SUBSCRIBE_AUDIO, SUBSCRIBE_METADATA,
    SUBSCRIBE_VIDEO, SenderInfo, TALLY_BOTH, TALLY_NONE, TALLY_PREVIEW, TALLY_PROGRAM, Tally,
};
pub use receiver::Receiver;
pub use sender::Sender;

/// Why an OMT record or connection was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OmtError {
    /// The frame header names a protocol version other than 1.
    Version(u8),
    /// The frame type byte is none of metadata, video, or audio.
    FrameKind(u8),
    /// A length is negative, too large, or does not match the payload.
    Length,
    /// A video or audio codec token is unknown or wrong for the frame type.
    Codec(u32),
    /// A video frame is malformed.
    Video(&'static str),
    /// An audio frame is malformed.
    Audio(&'static str),
    /// A metadata record is malformed.
    Metadata(&'static str),
    /// A string is not UTF-8.
    Utf8,
    /// The peer closed the connection.
    Closed,
    /// The transport failed.
    Io(String),
}

impl fmt::Display for OmtError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(version) => write!(formatter, "OMT version {version} is not supported"),
            Self::FrameKind(kind) => write!(formatter, "OMT frame type {kind} is unknown"),
            Self::Length => formatter.write_str("an OMT length is invalid"),
            Self::Codec(codec) => write!(formatter, "OMT codec {codec:#010x} is not supported"),
            Self::Video(reason) => write!(formatter, "invalid OMT video frame: {reason}"),
            Self::Audio(reason) => write!(formatter, "invalid OMT audio frame: {reason}"),
            Self::Metadata(reason) => write!(formatter, "invalid OMT metadata: {reason}"),
            Self::Utf8 => formatter.write_str("OMT text is not UTF-8"),
            Self::Closed => formatter.write_str("the OMT peer closed the connection"),
            Self::Io(reason) => write!(formatter, "OMT transport error: {reason}"),
        }
    }
}

impl std::error::Error for OmtError {}

impl From<std::io::Error> for OmtError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::Closed
        } else {
            Self::Io(error.to_string())
        }
    }
}
