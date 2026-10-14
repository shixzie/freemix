//! OSC 1.0 packet decoding/encoding, address-pattern matching, and conversion
//! of decoded messages into typed control events.
//!
//! Supported argument type tags are `i`, `f`, `s`, `b`, `T`, and `F`. Every
//! string and blob is NUL/zero padded to a 4-byte boundary and padding bytes
//! are validated on decode. Bundles nest up to a caller-bounded depth. No
//! sockets are opened and time tags are carried, never interpreted against a
//! clock.

use crate::control::{ControlEvent, ControlId, ControlValue};
use core::fmt;

/// Hard upper bound for an encoded OSC packet.
pub const MAX_OSC_PACKET_BYTES: usize = 65_536;
/// The OSC "immediately" time tag.
pub const OSC_TIME_TAG_IMMEDIATE: u64 = 1;
const BUNDLE_HEADER: &[u8; 8] = b"#bundle\0";

/// Bounds applied while decoding and encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OscLimits {
    /// Maximum bundle nesting; `1` allows a bundle of messages only.
    pub max_bundle_depth: usize,
    /// Maximum messages across the whole packet.
    pub max_messages: usize,
    /// Maximum arguments in one message.
    pub max_arguments: usize,
    /// Maximum bytes of one address or string argument (excluding NUL).
    pub max_string_bytes: usize,
}

impl Default for OscLimits {
    fn default() -> Self {
        Self {
            max_bundle_depth: 4,
            max_messages: 128,
            max_arguments: 64,
            max_string_bytes: 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OscArg {
    Int(i32),
    Float(f32),
    String(String),
    Blob(Vec<u8>),
    True,
    False,
}

impl OscArg {
    const fn tag(&self) -> u8 {
        match self {
            Self::Int(_) => b'i',
            Self::Float(_) => b'f',
            Self::String(_) => b's',
            Self::Blob(_) => b'b',
            Self::True => b'T',
            Self::False => b'F',
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OscMessage {
    /// A concrete address or an address pattern.
    pub address: String,
    pub args: Vec<OscArg>,
}

impl OscMessage {
    #[must_use]
    pub fn new(address: impl Into<String>, args: Vec<OscArg>) -> Self {
        Self {
            address: address.into(),
            args,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OscBundle {
    /// NTP-format time tag; carried verbatim.
    pub time_tag: u64,
    pub elements: Vec<OscPacket>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OscPacket {
    Message(OscMessage),
    Bundle(OscBundle),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OscError {
    EmptyPacket,
    PacketTooLarge {
        len: usize,
        limit: usize,
    },
    Misaligned {
        len: usize,
    },
    Truncated,
    /// A string had no NUL terminator, non-ASCII bytes, or non-zero padding.
    InvalidString,
    StringTooLong {
        limit: usize,
    },
    InvalidAddress(String),
    InvalidPattern(String),
    MissingTypeTags,
    UnsupportedTypeTag(char),
    InvalidBlobPadding,
    InvalidElementSize(i32),
    TrailingBytes,
    BundleTooDeep {
        limit: usize,
    },
    TooManyMessages {
        limit: usize,
    },
    TooManyArguments {
        limit: usize,
    },
    /// A control event needs a numeric or boolean first argument.
    UnsupportedControlArgument(char),
    NonFiniteArgument,
}

impl fmt::Display for OscError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPacket => formatter.write_str("OSC packet is empty"),
            Self::PacketTooLarge { len, limit } => {
                write!(formatter, "OSC packet of {len} bytes exceeds {limit}")
            }
            Self::Misaligned { len } => {
                write!(formatter, "OSC packet length {len} is not a multiple of 4")
            }
            Self::Truncated => formatter.write_str("OSC packet is truncated"),
            Self::InvalidString => formatter.write_str("OSC string is malformed"),
            Self::StringTooLong { limit } => write!(formatter, "OSC string exceeds {limit} bytes"),
            Self::InvalidAddress(address) => {
                write!(formatter, "OSC address {address:?} is invalid")
            }
            Self::InvalidPattern(pattern) => {
                write!(formatter, "OSC address pattern {pattern:?} is invalid")
            }
            Self::MissingTypeTags => formatter.write_str("OSC message has no type tag string"),
            Self::UnsupportedTypeTag(tag) => {
                write!(formatter, "OSC type tag {tag:?} is unsupported")
            }
            Self::InvalidBlobPadding => formatter.write_str("OSC blob padding is not zero"),
            Self::InvalidElementSize(size) => {
                write!(formatter, "OSC bundle element size {size} is invalid")
            }
            Self::TrailingBytes => formatter.write_str("OSC message has trailing bytes"),
            Self::BundleTooDeep { limit } => {
                write!(formatter, "OSC bundle nesting exceeds {limit}")
            }
            Self::TooManyMessages { limit } => {
                write!(formatter, "OSC packet exceeds {limit} messages")
            }
            Self::TooManyArguments { limit } => {
                write!(formatter, "OSC message exceeds {limit} arguments")
            }
            Self::UnsupportedControlArgument(tag) => {
                write!(formatter, "OSC argument {tag:?} cannot drive a control")
            }
            Self::NonFiniteArgument => formatter.write_str("OSC float argument is not finite"),
        }
    }
}

impl std::error::Error for OscError {}

// ---------------------------------------------------------------- decoding

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], OscError> {
        if self.remaining() < len {
            return Err(OscError::Truncated);
        }
        let slice = &self.bytes[self.position..self.position + len];
        self.position += len;
        Ok(slice)
    }

    fn word(&mut self) -> Result<[u8; 4], OscError> {
        let mut word = [0; 4];
        word.copy_from_slice(self.take(4)?);
        Ok(word)
    }

    fn string(&mut self, limit: usize) -> Result<String, OscError> {
        let rest = &self.bytes[self.position..];
        let len = rest
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(OscError::InvalidString)?;
        if len > limit {
            return Err(OscError::StringTooLong { limit });
        }
        let padded = padded_len(len + 1);
        let block = self.take(padded)?;
        let (text, padding) = block.split_at(len);
        if !text.is_ascii() || padding.iter().any(|&byte| byte != 0) {
            return Err(OscError::InvalidString);
        }
        Ok(text.iter().map(|&byte| char::from(byte)).collect())
    }
}

const fn padded_len(len: usize) -> usize {
    len.div_ceil(4) * 4
}

/// Decodes one OSC packet (message or bundle).
///
/// # Errors
///
/// Returns an error for malformed framing, alignment, padding, type tags,
/// addresses, or any exceeded limit.
pub fn decode_osc_packet(bytes: &[u8], limits: &OscLimits) -> Result<OscPacket, OscError> {
    check_packet_len(bytes.len())?;
    let mut messages = 0;
    decode_element(bytes, 0, limits, &mut messages)
}

fn check_packet_len(len: usize) -> Result<(), OscError> {
    if len == 0 {
        return Err(OscError::EmptyPacket);
    }
    if len > MAX_OSC_PACKET_BYTES {
        return Err(OscError::PacketTooLarge {
            len,
            limit: MAX_OSC_PACKET_BYTES,
        });
    }
    if !len.is_multiple_of(4) {
        return Err(OscError::Misaligned { len });
    }
    Ok(())
}

fn decode_element(
    bytes: &[u8],
    depth: usize,
    limits: &OscLimits,
    messages: &mut usize,
) -> Result<OscPacket, OscError> {
    if bytes.starts_with(BUNDLE_HEADER) {
        if depth >= limits.max_bundle_depth {
            return Err(OscError::BundleTooDeep {
                limit: limits.max_bundle_depth,
            });
        }
        let mut reader = Reader { bytes, position: 8 };
        let time_tag = u64::from_be_bytes(
            reader
                .take(8)?
                .try_into()
                .map_err(|_| OscError::Truncated)?,
        );
        let mut elements = Vec::new();
        while reader.remaining() > 0 {
            let size = i32::from_be_bytes(reader.word()?);
            let len = usize::try_from(size)
                .ok()
                .filter(|&len| len > 0 && len.is_multiple_of(4) && len <= reader.remaining())
                .ok_or(OscError::InvalidElementSize(size))?;
            let element = reader.take(len)?;
            elements.push(decode_element(element, depth + 1, limits, messages)?);
        }
        return Ok(OscPacket::Bundle(OscBundle { time_tag, elements }));
    }
    *messages += 1;
    if *messages > limits.max_messages {
        return Err(OscError::TooManyMessages {
            limit: limits.max_messages,
        });
    }
    decode_message(bytes, limits).map(OscPacket::Message)
}

fn decode_message(bytes: &[u8], limits: &OscLimits) -> Result<OscMessage, OscError> {
    let mut reader = Reader { bytes, position: 0 };
    let address = reader.string(limits.max_string_bytes)?;
    validate_osc_pattern(&address)?;
    if reader.remaining() == 0 {
        return Err(OscError::MissingTypeTags);
    }
    let tags = reader.string(limits.max_string_bytes)?;
    let Some(tags) = tags.strip_prefix(',') else {
        return Err(OscError::MissingTypeTags);
    };
    if tags.len() > limits.max_arguments {
        return Err(OscError::TooManyArguments {
            limit: limits.max_arguments,
        });
    }
    let mut args = Vec::with_capacity(tags.len());
    for tag in tags.chars() {
        args.push(match tag {
            'i' => OscArg::Int(i32::from_be_bytes(reader.word()?)),
            'f' => OscArg::Float(f32::from_be_bytes(reader.word()?)),
            's' => OscArg::String(reader.string(limits.max_string_bytes)?),
            'b' => {
                let size = i32::from_be_bytes(reader.word()?);
                let len = usize::try_from(size).map_err(|_| OscError::InvalidElementSize(size))?;
                if len > reader.remaining() {
                    return Err(OscError::Truncated);
                }
                let block = reader.take(padded_len(len))?;
                let (blob, padding) = block.split_at(len);
                if padding.iter().any(|&byte| byte != 0) {
                    return Err(OscError::InvalidBlobPadding);
                }
                OscArg::Blob(blob.to_vec())
            }
            'T' => OscArg::True,
            'F' => OscArg::False,
            other => return Err(OscError::UnsupportedTypeTag(other)),
        });
    }
    if reader.remaining() != 0 {
        return Err(OscError::TrailingBytes);
    }
    Ok(OscMessage { address, args })
}

// ---------------------------------------------------------------- encoding

/// Encodes one OSC packet with exact 4-byte alignment.
///
/// # Errors
///
/// Returns an error for invalid addresses/patterns, non-ASCII or NUL-bearing
/// strings, or any exceeded limit.
pub fn encode_osc_packet(packet: &OscPacket, limits: &OscLimits) -> Result<Vec<u8>, OscError> {
    let mut out = Vec::new();
    let mut messages = 0;
    encode_element(packet, 0, limits, &mut messages, &mut out)?;
    check_packet_len(out.len())?;
    Ok(out)
}

fn encode_element(
    packet: &OscPacket,
    depth: usize,
    limits: &OscLimits,
    messages: &mut usize,
    out: &mut Vec<u8>,
) -> Result<(), OscError> {
    if out.len() > MAX_OSC_PACKET_BYTES {
        return Err(OscError::PacketTooLarge {
            len: out.len(),
            limit: MAX_OSC_PACKET_BYTES,
        });
    }
    match packet {
        OscPacket::Message(message) => {
            *messages += 1;
            if *messages > limits.max_messages {
                return Err(OscError::TooManyMessages {
                    limit: limits.max_messages,
                });
            }
            encode_message(message, limits, out)
        }
        OscPacket::Bundle(bundle) => {
            if depth >= limits.max_bundle_depth {
                return Err(OscError::BundleTooDeep {
                    limit: limits.max_bundle_depth,
                });
            }
            out.extend_from_slice(BUNDLE_HEADER);
            out.extend_from_slice(&bundle.time_tag.to_be_bytes());
            for element in &bundle.elements {
                let size_at = out.len();
                out.extend_from_slice(&[0; 4]);
                encode_element(element, depth + 1, limits, messages, out)?;
                let size = i32::try_from(out.len() - size_at - 4).map_err(|_| {
                    OscError::PacketTooLarge {
                        len: out.len(),
                        limit: MAX_OSC_PACKET_BYTES,
                    }
                })?;
                out[size_at..size_at + 4].copy_from_slice(&size.to_be_bytes());
            }
            Ok(())
        }
    }
}

fn write_string(text: &str, limit: usize, out: &mut Vec<u8>) -> Result<(), OscError> {
    if text.len() > limit {
        return Err(OscError::StringTooLong { limit });
    }
    if !text.is_ascii() || text.bytes().any(|byte| byte == 0) {
        return Err(OscError::InvalidString);
    }
    out.extend_from_slice(text.as_bytes());
    out.resize(out.len() + padded_len(text.len() + 1) - text.len(), 0);
    Ok(())
}

fn encode_message(
    message: &OscMessage,
    limits: &OscLimits,
    out: &mut Vec<u8>,
) -> Result<(), OscError> {
    validate_osc_pattern(&message.address)?;
    if message.args.len() > limits.max_arguments {
        return Err(OscError::TooManyArguments {
            limit: limits.max_arguments,
        });
    }
    write_string(&message.address, limits.max_string_bytes, out)?;
    let tags: String = core::iter::once(',')
        .chain(message.args.iter().map(|arg| char::from(arg.tag())))
        .collect();
    write_string(&tags, usize::MAX, out)?;
    for arg in &message.args {
        match arg {
            OscArg::Int(value) => out.extend_from_slice(&value.to_be_bytes()),
            OscArg::Float(value) => out.extend_from_slice(&value.to_be_bytes()),
            OscArg::String(text) => write_string(text, limits.max_string_bytes, out)?,
            OscArg::Blob(blob) => {
                let size = i32::try_from(blob.len())
                    .ok()
                    .filter(|_| blob.len() <= MAX_OSC_PACKET_BYTES)
                    .ok_or(OscError::PacketTooLarge {
                        len: blob.len(),
                        limit: MAX_OSC_PACKET_BYTES,
                    })?;
                out.extend_from_slice(&size.to_be_bytes());
                out.extend_from_slice(blob);
                out.resize(out.len() + padded_len(blob.len()) - blob.len(), 0);
            }
            OscArg::True | OscArg::False => {}
        }
        if out.len() > MAX_OSC_PACKET_BYTES {
            return Err(OscError::PacketTooLarge {
                len: out.len(),
                limit: MAX_OSC_PACKET_BYTES,
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------- address patterns

const RESERVED: &[u8] = b" #*,/?[]{}";

/// Validates a concrete OSC address: `/`-separated, non-empty parts of
/// printable ASCII with none of `space # * , / ? [ ] { }`.
///
/// # Errors
///
/// Returns [`OscError::InvalidAddress`] when the address is not concrete.
pub fn validate_osc_address(address: &str) -> Result<(), OscError> {
    let invalid = || OscError::InvalidAddress(address.to_owned());
    let rest = address.strip_prefix('/').ok_or_else(invalid)?;
    for part in rest.split('/') {
        if part.is_empty()
            || part
                .bytes()
                .any(|byte| !byte.is_ascii_graphic() || RESERVED.contains(&byte))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Validates an OSC address pattern (a concrete address may also use `?`,
/// `*`, `[...]`, `[!...]`, and `{a,b}` within parts).
///
/// # Errors
///
/// Returns [`OscError::InvalidPattern`] for malformed patterns.
pub fn validate_osc_pattern(pattern: &str) -> Result<(), OscError> {
    let rest = pattern
        .strip_prefix('/')
        .ok_or_else(|| OscError::InvalidPattern(pattern.to_owned()))?;
    for part in rest.split('/') {
        tokenize(part).ok_or_else(|| OscError::InvalidPattern(pattern.to_owned()))?;
    }
    Ok(())
}

#[derive(Debug)]
enum Token {
    Literal(u8),
    AnyOne,
    AnyRun,
    Class {
        negated: bool,
        ranges: Vec<(u8, u8)>,
    },
    Alternatives(Vec<Vec<u8>>),
}

fn tokenize(part: &str) -> Option<Vec<Token>> {
    let bytes = part.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii_graphic() || matches!(byte, b'#' | b',' | b']' | b'}') {
            return None;
        }
        index += 1;
        tokens.push(match byte {
            b'?' => Token::AnyOne,
            b'*' => Token::AnyRun,
            b'[' => {
                let end = index + bytes[index..].iter().position(|&b| b == b']')?;
                let mut body = &bytes[index..end];
                index = end + 1;
                let negated = body.first() == Some(&b'!');
                if negated {
                    body = &body[1..];
                }
                if body.is_empty() {
                    return None;
                }
                let mut ranges = Vec::new();
                let mut at = 0;
                while at < body.len() {
                    if at + 2 < body.len() && body[at + 1] == b'-' {
                        if body[at] > body[at + 2] {
                            return None;
                        }
                        ranges.push((body[at], body[at + 2]));
                        at += 3;
                    } else {
                        ranges.push((body[at], body[at]));
                        at += 1;
                    }
                }
                Token::Class { negated, ranges }
            }
            b'{' => {
                let end = index + bytes[index..].iter().position(|&b| b == b'}')?;
                let body = &bytes[index..end];
                index = end + 1;
                if body.iter().any(|&b| matches!(b, b'{' | b'[' | b'*' | b'?')) {
                    return None;
                }
                Token::Alternatives(body.split(|&b| b == b',').map(<[u8]>::to_vec).collect())
            }
            literal => Token::Literal(literal),
        });
    }
    Some(tokens)
}

/// Reports whether an OSC address pattern matches a concrete address. Parts
/// are matched one-to-one; `*` never crosses a `/`.
///
/// # Errors
///
/// Returns an error when the pattern or address is malformed.
pub fn osc_address_matches(pattern: &str, address: &str) -> Result<bool, OscError> {
    validate_osc_pattern(pattern)?;
    validate_osc_address(address)?;
    let pattern_parts: Vec<&str> = pattern[1..].split('/').collect();
    let address_parts: Vec<&str> = address[1..].split('/').collect();
    if pattern_parts.len() != address_parts.len() {
        return Ok(false);
    }
    for (pattern_part, address_part) in pattern_parts.iter().zip(&address_parts) {
        let tokens =
            tokenize(pattern_part).ok_or_else(|| OscError::InvalidPattern(pattern.to_owned()))?;
        if !part_matches(&tokens, address_part.as_bytes()) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Set-of-positions simulation: `O(tokens * len * alternatives)`, no
/// backtracking.
fn part_matches(tokens: &[Token], text: &[u8]) -> bool {
    let mut current = vec![false; text.len() + 1];
    current[0] = true;
    for token in tokens {
        let mut next = vec![false; text.len() + 1];
        for position in (0..=text.len()).filter(|&position| current[position]) {
            match token {
                Token::Literal(byte) => {
                    if text.get(position) == Some(byte) {
                        next[position + 1] = true;
                    }
                }
                Token::AnyOne => {
                    if position < text.len() {
                        next[position + 1] = true;
                    }
                }
                Token::AnyRun => {
                    next[position..].iter_mut().for_each(|slot| *slot = true);
                    break;
                }
                Token::Class { negated, ranges } => {
                    if let Some(byte) = text.get(position) {
                        let inside = ranges
                            .iter()
                            .any(|&(low, high)| (low..=high).contains(byte));
                        if inside != *negated {
                            next[position + 1] = true;
                        }
                    }
                }
                Token::Alternatives(alternatives) => {
                    for alternative in alternatives {
                        if text[position..].starts_with(alternative) {
                            next[position + alternative.len()] = true;
                        }
                    }
                }
            }
        }
        if !next.iter().any(|&slot| slot) {
            return false;
        }
        current = next;
    }
    current[text.len()]
}

// ---------------------------------------------------------------- controls

/// Flattens a packet depth-first into control events, one per message, in
/// packet order. The first argument selects the value: no arguments or `T`
/// is a press, `F` a release, `i` an integer, and `f` a finite float. Further
/// arguments are ignored. Event controls carry the (possibly patterned)
/// address verbatim.
///
/// # Errors
///
/// Returns an error when a message's first argument is a string or blob, or
/// a float is not finite.
pub fn osc_control_events(packet: &OscPacket) -> Result<Vec<ControlEvent>, OscError> {
    let mut events = Vec::new();
    collect_events(packet, &mut events)?;
    Ok(events)
}

fn collect_events(packet: &OscPacket, events: &mut Vec<ControlEvent>) -> Result<(), OscError> {
    match packet {
        OscPacket::Bundle(bundle) => {
            for element in &bundle.elements {
                collect_events(element, events)?;
            }
        }
        OscPacket::Message(message) => {
            let value = match message.args.first() {
                None | Some(OscArg::True) => ControlValue::Button {
                    pressed: true,
                    velocity: 127,
                },
                Some(OscArg::False) => ControlValue::Button {
                    pressed: false,
                    velocity: 0,
                },
                Some(OscArg::Int(value)) => ControlValue::Integer(*value),
                Some(OscArg::Float(value)) if value.is_finite() => ControlValue::Float(*value),
                Some(OscArg::Float(_)) => return Err(OscError::NonFiniteArgument),
                Some(other) => {
                    return Err(OscError::UnsupportedControlArgument(char::from(
                        other.tag(),
                    )));
                }
            };
            events.push(ControlEvent {
                control: ControlId::Osc(message.address.clone()),
                value,
            });
        }
    }
    Ok(())
}
