//! Pure Sony VISCA-over-IP framing (UDP port 52381 on cameras).
//!
//! Each datagram carries an 8-byte big-endian header (payload type, payload
//! length, sequence number) followed by one VISCA or control payload.

use core::fmt;

use crate::visca::VISCA_MAX_MESSAGE_LEN;

/// Length of the VISCA-over-IP header.
pub const VISCA_IP_HEADER_LEN: usize = 8;

/// Payload type in bytes 0..2 of the header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViscaIpPayloadType {
    Command,
    Inquiry,
    Reply,
    DeviceSetting,
    ControlCommand,
    ControlReply,
}

impl ViscaIpPayloadType {
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Command => 0x0100,
            Self::Inquiry => 0x0110,
            Self::Reply => 0x0111,
            Self::DeviceSetting => 0x0120,
            Self::ControlCommand => 0x0200,
            Self::ControlReply => 0x0201,
        }
    }

    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            0x0100 => Some(Self::Command),
            0x0110 => Some(Self::Inquiry),
            0x0111 => Some(Self::Reply),
            0x0120 => Some(Self::DeviceSetting),
            0x0200 => Some(Self::ControlCommand),
            0x0201 => Some(Self::ControlReply),
            _ => None,
        }
    }
}

/// Decoded control-channel payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaIpControl {
    /// Control command `01`: reset the camera's expected sequence number.
    Reset,
    /// Control reply `01`: reset acknowledged.
    ResetAck,
    /// Control reply `0F 01`: abnormal sequence number.
    SequenceNumberError,
    /// Control reply `0F 02`: abnormal message type.
    MessageTypeError,
}

impl ViscaIpControl {
    #[must_use]
    pub const fn payload_type(self) -> ViscaIpPayloadType {
        match self {
            Self::Reset => ViscaIpPayloadType::ControlCommand,
            _ => ViscaIpPayloadType::ControlReply,
        }
    }

    #[must_use]
    pub const fn payload(self) -> &'static [u8] {
        match self {
            Self::Reset | Self::ResetAck => &[0x01],
            Self::SequenceNumberError => &[0x0F, 0x01],
            Self::MessageTypeError => &[0x0F, 0x02],
        }
    }

    /// Interprets a control packet payload.
    #[must_use]
    pub fn decode(payload_type: ViscaIpPayloadType, payload: &[u8]) -> Option<Self> {
        match (payload_type, payload) {
            (ViscaIpPayloadType::ControlCommand, [0x01]) => Some(Self::Reset),
            (ViscaIpPayloadType::ControlReply, [0x01]) => Some(Self::ResetAck),
            (ViscaIpPayloadType::ControlReply, [0x0F, 0x01]) => Some(Self::SequenceNumberError),
            (ViscaIpPayloadType::ControlReply, [0x0F, 0x02]) => Some(Self::MessageTypeError),
            _ => None,
        }
    }
}

/// Framing failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaIpError {
    EmptyPayload,
    PayloadTooLong(usize),
    TruncatedHeader(usize),
    UnknownPayloadType(u16),
    LengthMismatch { declared: u16, actual: usize },
}

impl fmt::Display for ViscaIpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPayload => formatter.write_str("VISCA-over-IP payload is empty"),
            Self::PayloadTooLong(len) => {
                write!(formatter, "VISCA-over-IP payload of {len} bytes exceeds 16")
            }
            Self::TruncatedHeader(len) => {
                write!(
                    formatter,
                    "VISCA-over-IP datagram of {len} bytes lacks a header"
                )
            }
            Self::UnknownPayloadType(code) => {
                write!(formatter, "unknown VISCA-over-IP payload type {code:#06x}")
            }
            Self::LengthMismatch { declared, actual } => write!(
                formatter,
                "VISCA-over-IP header declares {declared} bytes but {actual} follow"
            ),
        }
    }
}

impl std::error::Error for ViscaIpError {}

/// One VISCA-over-IP datagram.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViscaIpPacket {
    pub payload_type: ViscaIpPayloadType,
    pub sequence: u32,
    pub payload: Vec<u8>,
}

impl ViscaIpPacket {
    /// Encodes header and payload.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaIpError`] when the payload is empty or longer than 16 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ViscaIpError> {
        let length = validate_payload_len(self.payload.len())?;
        let mut bytes = Vec::with_capacity(VISCA_IP_HEADER_LEN + self.payload.len());
        bytes.extend_from_slice(&self.payload_type.code().to_be_bytes());
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Decodes one datagram. The declared length must match exactly.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaIpError`] for truncated headers, unknown payload types,
    /// or a declared length that disagrees with the datagram.
    pub fn decode(bytes: &[u8]) -> Result<Self, ViscaIpError> {
        let Some((header, payload)) = bytes.split_first_chunk::<VISCA_IP_HEADER_LEN>() else {
            return Err(ViscaIpError::TruncatedHeader(bytes.len()));
        };
        let code = u16::from_be_bytes([header[0], header[1]]);
        let declared = u16::from_be_bytes([header[2], header[3]]);
        let sequence = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        let payload_type =
            ViscaIpPayloadType::from_code(code).ok_or(ViscaIpError::UnknownPayloadType(code))?;
        if usize::from(declared) != payload.len() {
            return Err(ViscaIpError::LengthMismatch {
                declared,
                actual: payload.len(),
            });
        }
        validate_payload_len(payload.len())?;
        Ok(Self {
            payload_type,
            sequence,
            payload: payload.to_vec(),
        })
    }
}

fn validate_payload_len(len: usize) -> Result<u16, ViscaIpError> {
    if len == 0 {
        return Err(ViscaIpError::EmptyPayload);
    }
    if len > VISCA_MAX_MESSAGE_LEN {
        return Err(ViscaIpError::PayloadTooLong(len));
    }
    u16::try_from(len).map_err(|_| ViscaIpError::PayloadTooLong(len))
}

/// Controller-side sequence counter.
///
/// Sequence numbers increase by one per sent packet and wrap from
/// `u32::MAX` to zero. [`ViscaIpSequencer::reset`] emits a control RESET with
/// sequence zero and continues from one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViscaIpSequencer {
    next: u32,
}

impl ViscaIpSequencer {
    #[must_use]
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    #[must_use]
    pub const fn starting_at(next: u32) -> Self {
        Self { next }
    }

    /// Sequence number the next packet will carry.
    #[must_use]
    pub const fn peek(&self) -> u32 {
        self.next
    }

    /// Wraps a payload in the next sequence number.
    pub fn wrap(&mut self, payload_type: ViscaIpPayloadType, payload: Vec<u8>) -> ViscaIpPacket {
        let sequence = self.next;
        self.next = self.next.wrapping_add(1);
        ViscaIpPacket {
            payload_type,
            sequence,
            payload,
        }
    }

    /// Restarts at zero and returns the control RESET packet.
    pub fn reset(&mut self) -> ViscaIpPacket {
        self.next = 0;
        self.wrap(
            ViscaIpPayloadType::ControlCommand,
            ViscaIpControl::Reset.payload().to_vec(),
        )
    }
}
