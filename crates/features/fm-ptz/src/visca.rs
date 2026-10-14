//! Pure Sony VISCA message encoding and reply parsing.
//!
//! Nothing here touches a serial port or socket. Callers own the transport and
//! feed bytes in or write the returned bytes out.
//!
//! Sign conventions follow the domain types: positive pan is right, positive
//! tilt is up, positive zoom is tele, and positive focus is far.

use core::fmt;

use crate::{Axis, PtzIntent};

/// Terminator byte ending every VISCA message.
pub const VISCA_TERMINATOR: u8 = 0xFF;
/// Largest VISCA message, including header and terminator.
pub const VISCA_MAX_MESSAGE_LEN: usize = 16;
/// Largest pan speed byte accepted by `Pan-tiltDrive`.
pub const VISCA_MAX_PAN_SPEED: u8 = 0x18;
/// Largest tilt speed byte accepted by `Pan-tiltDrive`.
pub const VISCA_MAX_TILT_SPEED: u8 = 0x14;
/// Largest variable zoom/focus speed nibble.
pub const VISCA_MAX_LENS_SPEED: u8 = 0x07;
/// Largest preset slot that can be encoded without colliding with the terminator.
pub const VISCA_MAX_PRESET: u8 = 0xFE;

/// Peripheral address on a VISCA daisy chain (1 through 7).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ViscaAddress(u8);

impl ViscaAddress {
    /// Creates an address in `1..=7`.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= 7 {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Controller-to-camera header byte, `0x8x`.
    #[must_use]
    pub const fn command_header(self) -> u8 {
        0x80 | self.0
    }

    /// Camera-to-controller header byte, `0xy0` where `y = address + 8`.
    #[must_use]
    pub const fn reply_header(self) -> u8 {
        (self.0 + 8) << 4
    }
}

/// Memory operation for `CAM_Memory`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresetAction {
    Reset,
    Set,
    Recall,
}

impl PresetAction {
    const fn code(self) -> u8 {
        match self {
            Self::Reset => 0x00,
            Self::Set => 0x01,
            Self::Recall => 0x02,
        }
    }
}

/// Focus mode for `CAM_Focus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusMode {
    Auto,
    Manual,
}

/// Speeds used when an intent carries no explicit speed (absolute/relative moves).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViscaMoveSpeeds {
    pub pan: u8,
    pub tilt: u8,
}

impl Default for ViscaMoveSpeeds {
    fn default() -> Self {
        Self {
            pan: VISCA_MAX_PAN_SPEED,
            tilt: VISCA_MAX_TILT_SPEED,
        }
    }
}

/// One VISCA command. Values are validated when encoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaCommand {
    /// Broadcast `IF_Clear`: `88 01 00 01 FF`.
    IfClearBroadcast,
    /// Broadcast `AddressSet`: `88 30 01 FF`.
    AddressSetBroadcast,
    /// `CommandCancel` for socket 1 or 2: `8x 2p FF`.
    CommandCancel {
        socket: u8,
    },
    /// `Pan-tiltDrive` with signed speeds; zero stops that axis.
    ///
    /// Pan magnitude is `0..=0x18`, tilt magnitude is `0..=0x14`. A stopped
    /// axis is sent with speed byte `0x01`.
    PanTiltDrive {
        pan: i8,
        tilt: i8,
    },
    /// `Pan-tiltDrive` absolute position: 16-bit two's-complement nibbles.
    PanTiltAbsolute {
        speeds: ViscaMoveSpeeds,
        pan: i32,
        tilt: i32,
    },
    /// `Pan-tiltDrive` relative position: 16-bit two's-complement nibbles.
    PanTiltRelative {
        speeds: ViscaMoveSpeeds,
        pan: i32,
        tilt: i32,
    },
    PanTiltHome,
    PanTiltReset,
    /// Variable zoom: positive is tele, negative is wide, zero stops.
    /// Magnitude `n` in `1..=7` is sent as speed nibble `n`.
    ZoomDrive {
        speed: i8,
    },
    /// `CAM_Zoom` direct position `0..=0xFFFF`.
    ZoomDirect {
        position: i32,
    },
    /// Variable focus: positive is far, negative is near, zero stops.
    FocusDrive {
        speed: i8,
    },
    /// `CAM_Focus` direct position `0..=0xFFFF`.
    FocusDirect {
        position: i32,
    },
    FocusMode(FocusMode),
    /// `CAM_Memory` on slot `0..=0xFE`.
    Preset {
        action: PresetAction,
        slot: u16,
    },
}

/// Position inquiry commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaInquiry {
    /// `8x 09 06 12 FF`.
    PanTiltPosition,
    /// `8x 09 04 47 FF`.
    ZoomPosition,
    /// `8x 09 04 48 FF`.
    FocusPosition,
}

impl ViscaInquiry {
    #[must_use]
    pub fn encode(self, address: ViscaAddress) -> Vec<u8> {
        let body: [u8; 3] = match self {
            Self::PanTiltPosition => [0x09, 0x06, 0x12],
            Self::ZoomPosition => [0x09, 0x04, 0x47],
            Self::FocusPosition => [0x09, 0x04, 0x48],
        };
        let mut bytes = Vec::with_capacity(5);
        bytes.push(address.command_header());
        bytes.extend_from_slice(&body);
        bytes.push(VISCA_TERMINATOR);
        bytes
    }
}

/// Error returned when a command value cannot be represented in VISCA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaEncodeError {
    SpeedOutOfRange {
        axis: Axis,
        value: i32,
    },
    PositionOutOfRange {
        axis: Axis,
        value: i32,
    },
    PresetOutOfRange(u16),
    SocketOutOfRange(u8),
    /// VISCA has no relative zoom or focus command.
    UnsupportedRelativeAxis(Axis),
}

impl fmt::Display for ViscaEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpeedOutOfRange { axis, value } => {
                write!(formatter, "VISCA {axis:?} speed {value} is out of range")
            }
            Self::PositionOutOfRange { axis, value } => {
                write!(formatter, "VISCA {axis:?} position {value} is out of range")
            }
            Self::PresetOutOfRange(slot) => {
                write!(formatter, "VISCA preset slot {slot} is out of range")
            }
            Self::SocketOutOfRange(socket) => {
                write!(formatter, "VISCA socket {socket} is out of range")
            }
            Self::UnsupportedRelativeAxis(axis) => {
                write!(formatter, "VISCA has no relative {axis:?} command")
            }
        }
    }
}

impl std::error::Error for ViscaEncodeError {}

impl ViscaCommand {
    /// Encodes the command for one camera address.
    ///
    /// Broadcast commands ignore `address` and always use header `0x88`.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaEncodeError`] when a speed, position, socket, or preset
    /// slot is outside its VISCA range.
    pub fn encode(self, address: ViscaAddress) -> Result<Vec<u8>, ViscaEncodeError> {
        let header = address.command_header();
        let mut bytes = Vec::with_capacity(VISCA_MAX_MESSAGE_LEN);
        match self {
            Self::IfClearBroadcast => bytes.extend_from_slice(&[0x88, 0x01, 0x00, 0x01]),
            Self::AddressSetBroadcast => bytes.extend_from_slice(&[0x88, 0x30, 0x01]),
            Self::CommandCancel { socket } => {
                if !(1..=2).contains(&socket) {
                    return Err(ViscaEncodeError::SocketOutOfRange(socket));
                }
                bytes.extend_from_slice(&[header, 0x20 | socket]);
            }
            Self::PanTiltDrive { pan, tilt } => {
                let (pan_speed, pan_dir) =
                    drive_component(Axis::Pan, pan, VISCA_MAX_PAN_SPEED, 0x02, 0x01)?;
                let (tilt_speed, tilt_dir) =
                    drive_component(Axis::Tilt, tilt, VISCA_MAX_TILT_SPEED, 0x01, 0x02)?;
                bytes.extend_from_slice(&[
                    header, 0x01, 0x06, 0x01, pan_speed, tilt_speed, pan_dir, tilt_dir,
                ]);
            }
            Self::PanTiltAbsolute { speeds, pan, tilt } => {
                encode_position_move(&mut bytes, header, 0x02, speeds, pan, tilt)?;
            }
            Self::PanTiltRelative { speeds, pan, tilt } => {
                encode_position_move(&mut bytes, header, 0x03, speeds, pan, tilt)?;
            }
            Self::PanTiltHome => bytes.extend_from_slice(&[header, 0x01, 0x06, 0x04]),
            Self::PanTiltReset => bytes.extend_from_slice(&[header, 0x01, 0x06, 0x05]),
            Self::ZoomDrive { speed } => {
                let code = lens_drive_code(Axis::Zoom, speed)?;
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x07, code]);
            }
            Self::ZoomDirect { position } => {
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x47]);
                bytes.extend_from_slice(&unsigned_nibbles(Axis::Zoom, position)?);
            }
            Self::FocusDrive { speed } => {
                let code = lens_drive_code(Axis::Focus, speed)?;
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x08, code]);
            }
            Self::FocusDirect { position } => {
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x48]);
                bytes.extend_from_slice(&unsigned_nibbles(Axis::Focus, position)?);
            }
            Self::FocusMode(mode) => {
                let code = match mode {
                    FocusMode::Auto => 0x02,
                    FocusMode::Manual => 0x03,
                };
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x38, code]);
            }
            Self::Preset { action, slot } => {
                let slot = u8::try_from(slot)
                    .ok()
                    .filter(|slot| *slot <= VISCA_MAX_PRESET)
                    .ok_or(ViscaEncodeError::PresetOutOfRange(slot))?;
                bytes.extend_from_slice(&[header, 0x01, 0x04, 0x3F, action.code(), slot]);
            }
        }
        bytes.push(VISCA_TERMINATOR);
        Ok(bytes)
    }
}

fn drive_component(
    axis: Axis,
    speed: i8,
    max: u8,
    positive_direction: u8,
    negative_direction: u8,
) -> Result<(u8, u8), ViscaEncodeError> {
    let magnitude = speed.unsigned_abs();
    if magnitude > max {
        return Err(ViscaEncodeError::SpeedOutOfRange {
            axis,
            value: i32::from(speed),
        });
    }
    Ok(match speed.signum() {
        0 => (0x01, 0x03),
        1 => (magnitude, positive_direction),
        _ => (magnitude, negative_direction),
    })
}

fn lens_drive_code(axis: Axis, speed: i8) -> Result<u8, ViscaEncodeError> {
    let magnitude = speed.unsigned_abs();
    if magnitude > VISCA_MAX_LENS_SPEED {
        return Err(ViscaEncodeError::SpeedOutOfRange {
            axis,
            value: i32::from(speed),
        });
    }
    Ok(match speed.signum() {
        0 => 0x00,
        1 => 0x20 | magnitude,
        _ => 0x30 | magnitude,
    })
}

fn encode_position_move(
    bytes: &mut Vec<u8>,
    header: u8,
    kind: u8,
    speeds: ViscaMoveSpeeds,
    pan: i32,
    tilt: i32,
) -> Result<(), ViscaEncodeError> {
    if !(1..=VISCA_MAX_PAN_SPEED).contains(&speeds.pan) {
        return Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Pan,
            value: i32::from(speeds.pan),
        });
    }
    if !(1..=VISCA_MAX_TILT_SPEED).contains(&speeds.tilt) {
        return Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Tilt,
            value: i32::from(speeds.tilt),
        });
    }
    let pan = signed_nibbles(Axis::Pan, pan)?;
    let tilt = signed_nibbles(Axis::Tilt, tilt)?;
    bytes.extend_from_slice(&[header, 0x01, 0x06, kind, speeds.pan, speeds.tilt]);
    bytes.extend_from_slice(&pan);
    bytes.extend_from_slice(&tilt);
    Ok(())
}

fn signed_nibbles(axis: Axis, value: i32) -> Result<[u8; 4], ViscaEncodeError> {
    let value =
        i16::try_from(value).map_err(|_| ViscaEncodeError::PositionOutOfRange { axis, value })?;
    Ok(word_to_nibbles(value.cast_unsigned()))
}

fn unsigned_nibbles(axis: Axis, value: i32) -> Result<[u8; 4], ViscaEncodeError> {
    let value =
        u16::try_from(value).map_err(|_| ViscaEncodeError::PositionOutOfRange { axis, value })?;
    Ok(word_to_nibbles(value))
}

fn word_to_nibbles(word: u16) -> [u8; 4] {
    let [high, low] = word.to_be_bytes();
    [high >> 4, high & 0x0F, low >> 4, low & 0x0F]
}

/// Translates a domain intent into the VISCA commands that realize it.
///
/// Continuous moves and `Stop` always emit pan/tilt, zoom, and focus drive
/// commands so a zero component stops motion started by an earlier intent.
/// Speeds and positions must already be in VISCA units (see
/// [`PtzIntent::clamped`]). Preset names are not representable in VISCA and
/// are ignored; `DeletePreset` maps to `CAM_Memory Reset`.
///
/// # Errors
///
/// Returns [`ViscaEncodeError`] when a value cannot be represented, or when a
/// relative move carries a nonzero zoom or focus delta.
pub fn visca_commands_for_intent(
    intent: &PtzIntent,
    speeds: ViscaMoveSpeeds,
) -> Result<Vec<ViscaCommand>, ViscaEncodeError> {
    let commands = match intent {
        PtzIntent::MoveAbsolute(movement) => {
            let mut commands = vec![ViscaCommand::PanTiltAbsolute {
                speeds,
                pan: movement.pan,
                tilt: movement.tilt,
            }];
            if let Some(position) = movement.zoom {
                commands.push(ViscaCommand::ZoomDirect { position });
            }
            if let Some(position) = movement.focus {
                commands.push(ViscaCommand::FocusDirect { position });
            }
            commands
        }
        PtzIntent::MoveRelative(movement) => {
            if movement.zoom.is_some_and(|delta| delta != 0) {
                return Err(ViscaEncodeError::UnsupportedRelativeAxis(Axis::Zoom));
            }
            if movement.focus.is_some_and(|delta| delta != 0) {
                return Err(ViscaEncodeError::UnsupportedRelativeAxis(Axis::Focus));
            }
            vec![ViscaCommand::PanTiltRelative {
                speeds,
                pan: movement.pan,
                tilt: movement.tilt,
            }]
        }
        PtzIntent::MoveContinuous(movement) => vec![
            ViscaCommand::PanTiltDrive {
                pan: speed_i8(Axis::Pan, movement.pan)?,
                tilt: speed_i8(Axis::Tilt, movement.tilt)?,
            },
            ViscaCommand::ZoomDrive {
                speed: speed_i8(Axis::Zoom, movement.zoom)?,
            },
            ViscaCommand::FocusDrive {
                speed: speed_i8(Axis::Focus, movement.focus)?,
            },
        ],
        PtzIntent::Stop => vec![
            ViscaCommand::PanTiltDrive { pan: 0, tilt: 0 },
            ViscaCommand::ZoomDrive { speed: 0 },
            ViscaCommand::FocusDrive { speed: 0 },
        ],
        PtzIntent::Home => vec![ViscaCommand::PanTiltHome],
        PtzIntent::SavePreset { id, .. } => vec![ViscaCommand::Preset {
            action: PresetAction::Set,
            slot: id.get(),
        }],
        PtzIntent::RecallPreset(id) => vec![ViscaCommand::Preset {
            action: PresetAction::Recall,
            slot: id.get(),
        }],
        PtzIntent::DeletePreset(id) => vec![ViscaCommand::Preset {
            action: PresetAction::Reset,
            slot: id.get(),
        }],
    };
    Ok(commands)
}

fn speed_i8(axis: Axis, value: i32) -> Result<i8, ViscaEncodeError> {
    i8::try_from(value).map_err(|_| ViscaEncodeError::SpeedOutOfRange { axis, value })
}

/// Error code carried by a VISCA error reply (`y0 6z ee FF`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaErrorCode {
    MessageLength,
    Syntax,
    CommandBufferFull,
    CommandCancelled,
    NoSocket,
    CommandNotExecutable,
    Other(u8),
}

impl ViscaErrorCode {
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            0x01 => Self::MessageLength,
            0x02 => Self::Syntax,
            0x03 => Self::CommandBufferFull,
            0x04 => Self::CommandCancelled,
            0x05 => Self::NoSocket,
            0x41 => Self::CommandNotExecutable,
            other => Self::Other(other),
        }
    }
}

/// A decoded camera-to-controller VISCA message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViscaReply {
    /// `y0 4z FF`.
    Ack { address: u8, socket: u8 },
    /// `y0 5z [data...] FF`; `data` holds inquiry results and is empty for commands.
    Completion {
        address: u8,
        socket: u8,
        data: Vec<u8>,
    },
    /// `y0 6z ee FF`.
    Error {
        address: u8,
        socket: u8,
        code: ViscaErrorCode,
    },
    /// `y0 38 FF`.
    NetworkChange { address: u8 },
    /// Broadcast `88 30 0w FF`; `next_address` is one past the last camera.
    AddressSet { next_address: u8 },
}

/// Why bytes could not be decoded as a VISCA reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViscaParseError {
    TooShort,
    TooLong,
    MissingTerminator,
    EmbeddedTerminator,
    InvalidHeader(u8),
    UnknownMessage(u8),
    InvalidLength { expected: usize, actual: usize },
    InvalidNibble(u8),
}

impl fmt::Display for ViscaParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => formatter.write_str("VISCA message is too short"),
            Self::TooLong => formatter.write_str("VISCA message exceeds 16 bytes"),
            Self::MissingTerminator => formatter.write_str("VISCA message lacks 0xFF terminator"),
            Self::EmbeddedTerminator => {
                formatter.write_str("VISCA message contains an embedded 0xFF")
            }
            Self::InvalidHeader(byte) => {
                write!(formatter, "invalid VISCA reply header {byte:#04x}")
            }
            Self::UnknownMessage(byte) => {
                write!(formatter, "unknown VISCA reply type {byte:#04x}")
            }
            Self::InvalidLength { expected, actual } => write!(
                formatter,
                "VISCA reply length {actual} does not match expected {expected}"
            ),
            Self::InvalidNibble(byte) => {
                write!(formatter, "VISCA payload byte {byte:#04x} is not a nibble")
            }
        }
    }
}

impl std::error::Error for ViscaParseError {}

impl ViscaReply {
    /// Parses exactly one complete reply, including its terminator.
    ///
    /// # Errors
    ///
    /// Returns [`ViscaParseError`] for malformed framing, headers, or lengths.
    pub fn parse(bytes: &[u8]) -> Result<Self, ViscaParseError> {
        if bytes.len() < 3 {
            return Err(ViscaParseError::TooShort);
        }
        if bytes.len() > VISCA_MAX_MESSAGE_LEN {
            return Err(ViscaParseError::TooLong);
        }
        let (&last, body) = bytes.split_last().ok_or(ViscaParseError::TooShort)?;
        if last != VISCA_TERMINATOR {
            return Err(ViscaParseError::MissingTerminator);
        }
        if body.contains(&VISCA_TERMINATOR) {
            return Err(ViscaParseError::EmbeddedTerminator);
        }
        let header = body[0];
        let kind = body[1];
        if header == 0x88 {
            return match kind {
                0x30 => {
                    expect_len(bytes, 4)?;
                    Ok(Self::AddressSet {
                        next_address: body[2],
                    })
                }
                other => Err(ViscaParseError::UnknownMessage(other)),
            };
        }
        if header & 0x0F != 0 || !(0x90..=0xF0).contains(&header) {
            return Err(ViscaParseError::InvalidHeader(header));
        }
        let address = (header >> 4) - 8;
        let socket = kind & 0x0F;
        match kind & 0xF0 {
            0x40 => {
                expect_len(bytes, 3)?;
                Ok(Self::Ack { address, socket })
            }
            0x50 => Ok(Self::Completion {
                address,
                socket,
                data: body[2..].to_vec(),
            }),
            0x60 => {
                expect_len(bytes, 4)?;
                Ok(Self::Error {
                    address,
                    socket,
                    code: ViscaErrorCode::from_byte(body[2]),
                })
            }
            _ if kind == 0x38 => {
                expect_len(bytes, 3)?;
                Ok(Self::NetworkChange { address })
            }
            _ => Err(ViscaParseError::UnknownMessage(kind)),
        }
    }
}

fn expect_len(bytes: &[u8], expected: usize) -> Result<(), ViscaParseError> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(ViscaParseError::InvalidLength {
            expected,
            actual: bytes.len(),
        })
    }
}

fn nibbles_to_word(nibbles: &[u8]) -> Result<u16, ViscaParseError> {
    nibbles.iter().try_fold(0_u16, |word, &nibble| {
        if nibble > 0x0F {
            Err(ViscaParseError::InvalidNibble(nibble))
        } else {
            Ok((word << 4) | u16::from(nibble))
        }
    })
}

/// Decodes the data of a `Pan-tiltPosInq` completion into `(pan, tilt)`.
///
/// # Errors
///
/// Returns [`ViscaParseError`] unless `data` is exactly eight nibbles.
pub fn decode_pan_tilt_position(data: &[u8]) -> Result<(i32, i32), ViscaParseError> {
    if data.len() != 8 {
        return Err(ViscaParseError::InvalidLength {
            expected: 8,
            actual: data.len(),
        });
    }
    let pan = nibbles_to_word(&data[..4])?.cast_signed();
    let tilt = nibbles_to_word(&data[4..])?.cast_signed();
    Ok((i32::from(pan), i32::from(tilt)))
}

/// Decodes the data of a zoom or focus position inquiry completion.
///
/// # Errors
///
/// Returns [`ViscaParseError`] unless `data` is exactly four nibbles.
pub fn decode_lens_position(data: &[u8]) -> Result<i32, ViscaParseError> {
    if data.len() != 4 {
        return Err(ViscaParseError::InvalidLength {
            expected: 4,
            actual: data.len(),
        });
    }
    Ok(i32::from(nibbles_to_word(data)?))
}

/// Bounded incremental splitter for a VISCA byte stream (serial or TCP).
///
/// Bytes are buffered until a terminator arrives. A message that grows past
/// [`VISCA_MAX_MESSAGE_LEN`] without a terminator yields
/// [`ViscaParseError::TooLong`] once and the decoder discards input through
/// the next terminator, so memory stays bounded.
#[derive(Clone, Debug, Default)]
pub struct ViscaReplyDecoder {
    buffer: Vec<u8>,
    discarding: bool,
}

impl ViscaReplyDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bytes buffered for an incomplete message.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Feeds bytes and returns every reply or error completed by them, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<ViscaReply, ViscaParseError>> {
        let mut out = Vec::new();
        for &byte in bytes {
            if self.discarding {
                if byte == VISCA_TERMINATOR {
                    self.discarding = false;
                }
                continue;
            }
            self.buffer.push(byte);
            if byte == VISCA_TERMINATOR {
                out.push(ViscaReply::parse(&self.buffer));
                self.buffer.clear();
            } else if self.buffer.len() >= VISCA_MAX_MESSAGE_LEN {
                out.push(Err(ViscaParseError::TooLong));
                self.buffer.clear();
                self.discarding = true;
            }
        }
        out
    }
}
