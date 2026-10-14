//! Encoded-fallback monitor images: a std-only QOI codec, the unpadded
//! base64url transport encoding used by `monitor_frame` records, frame
//! splitting, and per-tap frame reassembly.
//!
//! The wire carries one standard QOI stream per frame (14-byte `qoif` header
//! with three channels and the sRGB colorspace, QOI ops, and the 8-byte end
//! marker). Images are capped at [`MAX_MONITOR_FRAME_WIDTH`] x
//! [`MAX_MONITOR_FRAME_HEIGHT`], so a frame never exceeds
//! [`MAX_MONITOR_FRAME_PARTS`] parts of [`MAX_MONITOR_FRAME_PART_BYTES`].

use core::fmt;
use std::sync::Arc;

use crate::{
    MAX_MONITOR_FRAME_HEIGHT, MAX_MONITOR_FRAME_PART_BYTES, MAX_MONITOR_FRAME_PARTS,
    MAX_MONITOR_FRAME_WIDTH, MonitorFrameMessage, MonitorFrameOrigin, MonitorImage, MonitorTap,
    ServerIdentity, WireInputId,
};

const QOI_MAGIC: [u8; 4] = *b"qoif";
const QOI_HEADER_BYTES: usize = 14;
const QOI_END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
const QOI_OP_INDEX: u8 = 0x00;
const QOI_OP_DIFF: u8 = 0x40;
const QOI_OP_LUMA: u8 = 0x80;
const QOI_OP_RUN: u8 = 0xc0;
const QOI_OP_RGB: u8 = 0xfe;
const QOI_OP_RGBA: u8 = 0xff;
const QOI_MASK: u8 = 0xc0;
const QOI_MAX_RUN: u8 = 62;

/// Failure to encode, decode, or reassemble a monitor image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonitorImageError {
    /// Width or height is zero or exceeds the monitor frame cap.
    InvalidDimensions { width: u16, height: u16 },
    /// The source pixel buffer, stride, or bytes-per-pixel is inconsistent.
    InvalidPixelLayout,
    /// The QOI header is malformed, or its dimensions, channels, or
    /// colorspace differ from the record's.
    InvalidHeader,
    /// The QOI op stream is truncated or contains an unsupported op.
    InvalidChunk,
    /// A QOI run extends past the last pixel.
    RunOverflow,
    /// The pixels are not followed by the exact QOI end marker.
    InvalidEndMarker,
    /// Bytes follow the QOI end marker.
    TrailingData,
    /// A part's index, count, or payload size is out of bounds, or a group
    /// did not start at part `0` or skipped a part.
    UnexpectedPart { expected: u16, actual: u16 },
    /// A part's header fields differ from the in-progress group's.
    HeaderMismatch,
}

impl fmt::Display for MonitorImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(
                    formatter,
                    "monitor image dimensions {width}x{height} are invalid"
                )
            }
            Self::InvalidPixelLayout => {
                formatter.write_str("monitor image pixel layout is invalid")
            }
            Self::InvalidHeader => formatter.write_str("monitor image QOI header is invalid"),
            Self::InvalidChunk => formatter.write_str("monitor image QOI data is invalid"),
            Self::RunOverflow => formatter.write_str("monitor image QOI run overflows the image"),
            Self::InvalidEndMarker => {
                formatter.write_str("monitor image QOI end marker is missing")
            }
            Self::TrailingData => formatter.write_str("monitor image QOI has trailing data"),
            Self::UnexpectedPart { expected, actual } => write!(
                formatter,
                "monitor frame part {actual} is unexpected (expected {expected})"
            ),
            Self::HeaderMismatch => {
                formatter.write_str("monitor frame part header differs from its group")
            }
        }
    }
}

impl std::error::Error for MonitorImageError {}

const fn dimensions_valid(width: u16, height: u16) -> bool {
    width != 0
        && height != 0
        && width <= MAX_MONITOR_FRAME_WIDTH
        && height <= MAX_MONITOR_FRAME_HEIGHT
}

const fn qoi_hash(pixel: [u8; 4]) -> usize {
    (pixel[0] as usize * 3 + pixel[1] as usize * 5 + pixel[2] as usize * 7 + pixel[3] as usize * 11)
        % 64
}

/// Encodes an image as one standard three-channel QOI stream.
///
/// `pixels` holds `height` rows starting `stride` bytes apart; each row holds
/// `width` pixels of `bytes_per_pixel` bytes (`3` for RGB8, `4` for RGBA8)
/// whose first three bytes are red, green, and blue. Alpha is dropped.
///
/// # Errors
///
/// Returns [`MonitorImageError::InvalidDimensions`] when the image is empty
/// or exceeds the monitor frame cap, and
/// [`MonitorImageError::InvalidPixelLayout`] when `bytes_per_pixel` is not
/// `3` or `4`, `stride` is shorter than a row, or `pixels` is too short.
pub fn encode_qoi_rgb(
    width: u16,
    height: u16,
    pixels: &[u8],
    stride: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, MonitorImageError> {
    if !dimensions_valid(width, height) {
        return Err(MonitorImageError::InvalidDimensions { width, height });
    }
    if !matches!(bytes_per_pixel, 3 | 4) {
        return Err(MonitorImageError::InvalidPixelLayout);
    }
    let columns = usize::from(width);
    let rows = usize::from(height);
    let row_bytes = columns * bytes_per_pixel;
    let required = stride
        .checked_mul(rows - 1)
        .and_then(|bytes| bytes.checked_add(row_bytes));
    if stride < row_bytes || required.is_none_or(|required| pixels.len() < required) {
        return Err(MonitorImageError::InvalidPixelLayout);
    }

    let mut out = Vec::with_capacity(QOI_HEADER_BYTES + columns * rows + QOI_END_MARKER.len());
    out.extend_from_slice(&QOI_MAGIC);
    out.extend_from_slice(&u32::from(width).to_be_bytes());
    out.extend_from_slice(&u32::from(height).to_be_bytes());
    out.push(3);
    out.push(0);

    let mut index = [[0_u8; 4]; 64];
    let mut previous = [0, 0, 0, 255];
    let mut run = 0_u8;
    for row in 0..rows {
        let start = row * stride;
        for source in pixels[start..start + row_bytes].chunks_exact(bytes_per_pixel) {
            let pixel = [source[0], source[1], source[2], 255];
            if pixel == previous {
                run += 1;
                if run == QOI_MAX_RUN {
                    out.push(QOI_OP_RUN | (run - 1));
                    run = 0;
                }
                continue;
            }
            if run > 0 {
                out.push(QOI_OP_RUN | (run - 1));
                run = 0;
            }
            let hash = qoi_hash(pixel);
            if index[hash] == pixel {
                #[allow(clippy::cast_possible_truncation)]
                out.push(QOI_OP_INDEX | hash as u8);
            } else {
                index[hash] = pixel;
                push_qoi_color(&mut out, previous, pixel);
            }
            previous = pixel;
        }
    }
    if run > 0 {
        out.push(QOI_OP_RUN | (run - 1));
    }
    out.extend_from_slice(&QOI_END_MARKER);
    Ok(out)
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn push_qoi_color(out: &mut Vec<u8>, previous: [u8; 4], pixel: [u8; 4]) {
    let red = pixel[0].wrapping_sub(previous[0]) as i8;
    let green = pixel[1].wrapping_sub(previous[1]) as i8;
    let blue = pixel[2].wrapping_sub(previous[2]) as i8;
    let red_green = red.wrapping_sub(green);
    let blue_green = blue.wrapping_sub(green);
    if (-2..=1).contains(&red) && (-2..=1).contains(&green) && (-2..=1).contains(&blue) {
        out.push(
            QOI_OP_DIFF
                | (((red + 2) as u8) << 4)
                | (((green + 2) as u8) << 2)
                | ((blue + 2) as u8),
        );
    } else if (-32..=31).contains(&green)
        && (-8..=7).contains(&red_green)
        && (-8..=7).contains(&blue_green)
    {
        out.push(QOI_OP_LUMA | ((green + 32) as u8));
        out.push((((red_green + 8) as u8) << 4) | ((blue_green + 8) as u8));
    } else {
        out.extend_from_slice(&[QOI_OP_RGB, pixel[0], pixel[1], pixel[2]]);
    }
}

/// Strictly decodes one three-channel QOI stream into tightly packed RGBA8
/// with alpha `255`.
///
/// The header must carry the `qoif` magic, exactly `width` x `height`,
/// three channels, and colorspace `0`; the ops must produce exactly
/// `width * height` pixels without a run overflowing the image; and the
/// stream must end with exactly the QOI end marker. The `QOI_OP_RGBA` op is
/// rejected because three-channel monitor streams never carry alpha.
///
/// # Errors
///
/// Returns a [`MonitorImageError`] describing the first violation.
pub fn decode_qoi_rgb(width: u16, height: u16, data: &[u8]) -> Result<Vec<u8>, MonitorImageError> {
    if !dimensions_valid(width, height) {
        return Err(MonitorImageError::InvalidDimensions { width, height });
    }
    if data.len() < QOI_HEADER_BYTES + QOI_END_MARKER.len()
        || data[..4] != QOI_MAGIC
        || data[4..8] != u32::from(width).to_be_bytes()
        || data[8..12] != u32::from(height).to_be_bytes()
        || data[12] != 3
        || data[13] != 0
    {
        return Err(MonitorImageError::InvalidHeader);
    }
    let total = usize::from(width) * usize::from(height);
    let end = data.len() - QOI_END_MARKER.len();
    let mut out = Vec::with_capacity(total * 4);
    let mut index = [[0_u8; 4]; 64];
    let mut pixel = [0_u8, 0, 0, 255];
    let mut position = QOI_HEADER_BYTES;
    let mut produced = 0;
    while produced < total {
        if position >= end {
            return Err(MonitorImageError::InvalidChunk);
        }
        let op = data[position];
        position += 1;
        let mut count = 1;
        if op == QOI_OP_RGB {
            if position + 3 > end {
                return Err(MonitorImageError::InvalidChunk);
            }
            pixel[..3].copy_from_slice(&data[position..position + 3]);
            position += 3;
        } else if op == QOI_OP_RGBA {
            return Err(MonitorImageError::InvalidChunk);
        } else {
            match op & QOI_MASK {
                QOI_OP_INDEX => pixel = index[usize::from(op)],
                QOI_OP_DIFF => {
                    pixel[0] = pixel[0].wrapping_add((op >> 4) & 0x03).wrapping_sub(2);
                    pixel[1] = pixel[1].wrapping_add((op >> 2) & 0x03).wrapping_sub(2);
                    pixel[2] = pixel[2].wrapping_add(op & 0x03).wrapping_sub(2);
                }
                QOI_OP_LUMA => {
                    if position + 1 > end {
                        return Err(MonitorImageError::InvalidChunk);
                    }
                    let second = data[position];
                    position += 1;
                    let green = (op & 0x3f).wrapping_sub(32);
                    pixel[0] = pixel[0]
                        .wrapping_add(green)
                        .wrapping_add(second >> 4)
                        .wrapping_sub(8);
                    pixel[1] = pixel[1].wrapping_add(green);
                    pixel[2] = pixel[2]
                        .wrapping_add(green)
                        .wrapping_add(second & 0x0f)
                        .wrapping_sub(8);
                }
                _ => {
                    count = usize::from(op & 0x3f) + 1;
                    if produced + count > total {
                        return Err(MonitorImageError::RunOverflow);
                    }
                }
            }
        }
        index[qoi_hash(pixel)] = pixel;
        // The op state keeps the reference decoder's alpha (an index op can
        // load a never-written, all-zero slot), but a three-channel stream
        // carries no alpha, so every output pixel is opaque.
        let opaque = [pixel[0], pixel[1], pixel[2], 255];
        for _ in 0..count {
            out.extend_from_slice(&opaque);
        }
        produced += count;
    }
    if data[position..position + QOI_END_MARKER.len()] != QOI_END_MARKER {
        return Err(MonitorImageError::InvalidEndMarker);
    }
    if position + QOI_END_MARKER.len() != data.len() {
        return Err(MonitorImageError::TrailingData);
    }
    Ok(out)
}

/// Splits one encoded QOI stream into `monitor_frame` parts of at most
/// [`MAX_MONITOR_FRAME_PART_BYTES`] raw bytes, each repeating the header.
///
/// The parts are returned in send order. An empty `qoi` yields no parts.
/// A stream produced by [`encode_qoi_rgb`] always fits in
/// [`MAX_MONITOR_FRAME_PARTS`]; an oversized stream yields parts the codec
/// refuses to encode.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn monitor_frame_parts(
    server: &ServerIdentity,
    tap: MonitorTap,
    sequence: u64,
    frame: u64,
    input: WireInputId,
    origin: MonitorFrameOrigin,
    width: u16,
    height: u16,
    qoi: &[u8],
) -> Vec<MonitorFrameMessage> {
    let chunks = qoi.chunks(MAX_MONITOR_FRAME_PART_BYTES);
    let parts = u16::try_from(chunks.len()).unwrap_or(u16::MAX);
    chunks
        .enumerate()
        .map(|(part, data)| MonitorFrameMessage {
            server: server.clone(),
            tap,
            sequence,
            frame,
            input,
            origin,
            width,
            height,
            part: u16::try_from(part).unwrap_or(u16::MAX),
            parts,
            data: data.to_vec(),
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingFrame {
    server: ServerIdentity,
    tap: MonitorTap,
    sequence: u64,
    frame: u64,
    input: WireInputId,
    origin: MonitorFrameOrigin,
    width: u16,
    height: u16,
    parts: u16,
    next_part: u16,
    data: Vec<u8>,
}

impl PendingFrame {
    fn matches(&self, message: &MonitorFrameMessage) -> bool {
        self.server == message.server
            && self.tap == message.tap
            && self.sequence == message.sequence
            && self.frame == message.frame
            && self.input == message.input
            && self.origin == message.origin
            && self.width == message.width
            && self.height == message.height
            && self.parts == message.parts
    }
}

/// Reassembles one tap's `monitor_frame` parts into decoded images.
///
/// A group must start at part `0`, continue with contiguous parts whose
/// header fields are identical, and hold at most [`MAX_MONITOR_FRAME_PARTS`]
/// parts. The last part completes the group and decodes the QOI stream.
/// Any violation discards the in-progress group and returns an error.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MonitorFrameAssembler {
    pending: Option<PendingFrame>,
}

impl MonitorFrameAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Whether a group has started but not completed.
    #[must_use]
    pub const fn is_in_progress(&self) -> bool {
        self.pending.is_some()
    }

    /// The sequence of the in-progress group, if any.
    #[must_use]
    pub fn in_progress_sequence(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.sequence)
    }

    /// Discards any in-progress group.
    pub fn reset(&mut self) {
        self.pending = None;
    }

    /// Adds one part. Returns the decoded image when `message` completes its
    /// group and `None` while more parts are expected.
    ///
    /// # Errors
    ///
    /// Returns a [`MonitorImageError`] when the part is out of order or out
    /// of bounds, its header differs from the in-progress group, or the
    /// completed QOI stream fails strict decoding. The in-progress group is
    /// discarded on error.
    pub fn push(
        &mut self,
        message: &MonitorFrameMessage,
    ) -> Result<Option<MonitorImage>, MonitorImageError> {
        let result = self.push_inner(message);
        if result.is_err() {
            self.pending = None;
        }
        result
    }

    fn push_inner(
        &mut self,
        message: &MonitorFrameMessage,
    ) -> Result<Option<MonitorImage>, MonitorImageError> {
        if !dimensions_valid(message.width, message.height) {
            return Err(MonitorImageError::InvalidDimensions {
                width: message.width,
                height: message.height,
            });
        }
        let expected = self.pending.as_ref().map_or(0, |pending| pending.next_part);
        if message.parts == 0
            || message.parts > MAX_MONITOR_FRAME_PARTS
            || message.part != expected
            || message.part >= message.parts
            || message.data.is_empty()
            || message.data.len() > MAX_MONITOR_FRAME_PART_BYTES
        {
            return Err(MonitorImageError::UnexpectedPart {
                expected,
                actual: message.part,
            });
        }
        let pending = match self.pending.as_mut() {
            Some(pending) => {
                if !pending.matches(message) {
                    return Err(MonitorImageError::HeaderMismatch);
                }
                pending
            }
            None => self.pending.insert(PendingFrame {
                server: message.server.clone(),
                tap: message.tap,
                sequence: message.sequence,
                frame: message.frame,
                input: message.input,
                origin: message.origin,
                width: message.width,
                height: message.height,
                parts: message.parts,
                next_part: 0,
                data: Vec::new(),
            }),
        };
        pending.data.extend_from_slice(&message.data);
        pending.next_part += 1;
        if pending.next_part < pending.parts {
            return Ok(None);
        }
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        let rgba = decode_qoi_rgb(pending.width, pending.height, &pending.data)?;
        Ok(Some(MonitorImage {
            tap: pending.tap,
            sequence: pending.sequence,
            frame: pending.frame,
            input: pending.input,
            origin: pending.origin,
            width: pending.width,
            height: pending.height,
            rgba: Arc::from(rgba),
        }))
    }
}

const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes bytes as unpadded RFC 4648 base64url.
pub(crate) fn base64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let symbol = |value: u32| char::from(BASE64URL_ALPHABET[(value & 0x3f) as usize]);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let word = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        out.push(symbol(word >> 18));
        out.push(symbol(word >> 12));
        if chunk.len() > 1 {
            out.push(symbol(word >> 6));
        }
        if chunk.len() > 2 {
            out.push(symbol(word));
        }
    }
    out
}

const fn base64url_value(symbol: u8) -> Option<u32> {
    let value = match symbol {
        b'A'..=b'Z' => symbol - b'A',
        b'a'..=b'z' => symbol - b'a' + 26,
        b'0'..=b'9' => symbol - b'0' + 52,
        b'-' => 62,
        b'_' => 63,
        _ => return None,
    };
    Some(value as u32)
}

/// Decodes canonical unpadded base64url: padding, the standard `+`/`/`
/// alphabet, whitespace, impossible lengths, and nonzero trailing bits are
/// all rejected.
pub(crate) fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut word = 0_u32;
        for (offset, symbol) in chunk.iter().enumerate() {
            word |= base64url_value(*symbol)? << (18 - 6 * offset);
        }
        let [_, first, second, third] = word.to_be_bytes();
        match chunk.len() {
            2 => {
                if word & 0xffff != 0 {
                    return None;
                }
                out.push(first);
            }
            3 => {
                if word & 0xff != 0 {
                    return None;
                }
                out.extend_from_slice(&[first, second]);
            }
            _ => out.extend_from_slice(&[first, second, third]),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    const fn qoi_header(width: u8, height: u8) -> [u8; 14] {
        [
            b'q', b'o', b'i', b'f', 0, 0, 0, width, 0, 0, 0, height, 3, 0,
        ]
    }

    fn stream(width: u8, height: u8, ops: &[u8]) -> Vec<u8> {
        let mut data = qoi_header(width, height).to_vec();
        data.extend_from_slice(ops);
        data.extend_from_slice(&QOI_END_MARKER);
        data
    }

    fn server() -> ServerIdentity {
        ServerIdentity {
            engine_id: "engine-a".into(),
            project_id: "project-9".into(),
            state_epoch: 7,
            log_id: "log-a".into(),
        }
    }

    fn input() -> WireInputId {
        WireInputId::new(NonZeroU128::new(3).unwrap())
    }

    /// A deterministic, poorly compressible RGB pattern.
    fn noise(width: u16, height: u16) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        (0..usize::from(width) * usize::from(height) * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    fn rgba_of(rgb: &[u8]) -> Vec<u8> {
        rgb.as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect()
    }

    #[test]
    fn encoder_matches_the_reference_qoi_op_selection() {
        let pixels = [10, 0, 0, 15, 3, 4, 10, 0, 0, 10, 0, 0];
        let encoded = encode_qoi_rgb(4, 1, &pixels, 12, 3).unwrap();
        // RGB, LUMA, INDEX (hash 19), RUN 1.
        assert_eq!(
            encoded,
            stream(4, 1, &[0xfe, 10, 0, 0, 0xa3, 0xa9, 0x13, 0xc0])
        );
        assert_eq!(decode_qoi_rgb(4, 1, &encoded).unwrap(), rgba_of(&pixels));

        // The implicit previous pixel is opaque black; small deltas use DIFF.
        let encoded = encode_qoi_rgb(3, 1, &[0, 0, 0, 1, 0, 0, 0, 0xff, 0], 9, 3).unwrap();
        assert_eq!(encoded, stream(3, 1, &[0xc0, 0x7a, 0x56]));
    }

    #[test]
    fn runs_split_at_sixty_two_pixels() {
        let pixels = vec![0_u8; 130 * 3];
        let encoded = encode_qoi_rgb(130, 1, &pixels, 130 * 3, 3).unwrap();
        assert_eq!(encoded, stream(130, 1, &[0xfd, 0xfd, 0xc5]));
        assert_eq!(decode_qoi_rgb(130, 1, &encoded).unwrap(), rgba_of(&pixels));
    }

    #[test]
    fn rgba_input_with_padded_stride_drops_alpha() {
        let rgb = noise(5, 3);
        let stride = 5 * 4 + 7;
        let mut padded = vec![0xaa; stride * 3];
        for (index, pixel) in rgb.as_chunks::<3>().0.iter().enumerate() {
            let offset = (index / 5) * stride + (index % 5) * 4;
            padded[offset..offset + 3].copy_from_slice(pixel);
            padded[offset + 3] = 17;
        }
        let from_rgba = encode_qoi_rgb(5, 3, &padded[..stride * 2 + 20], stride, 4).unwrap();
        let from_rgb = encode_qoi_rgb(5, 3, &rgb, 15, 3).unwrap();
        assert_eq!(from_rgba, from_rgb);
        assert_eq!(decode_qoi_rgb(5, 3, &from_rgba).unwrap(), rgba_of(&rgb));
    }

    #[test]
    fn maximum_noise_frame_round_trips_within_the_part_cap() {
        let rgb = noise(MAX_MONITOR_FRAME_WIDTH, MAX_MONITOR_FRAME_HEIGHT);
        let encoded = encode_qoi_rgb(
            MAX_MONITOR_FRAME_WIDTH,
            MAX_MONITOR_FRAME_HEIGHT,
            &rgb,
            usize::from(MAX_MONITOR_FRAME_WIDTH) * 3,
            3,
        )
        .unwrap();
        assert!(
            encoded.len().div_ceil(MAX_MONITOR_FRAME_PART_BYTES)
                <= usize::from(MAX_MONITOR_FRAME_PARTS)
        );
        let decoded =
            decode_qoi_rgb(MAX_MONITOR_FRAME_WIDTH, MAX_MONITOR_FRAME_HEIGHT, &encoded).unwrap();
        assert_eq!(decoded, rgba_of(&rgb));
    }

    #[test]
    fn worst_case_stream_fits_the_part_cap() {
        let worst = QOI_HEADER_BYTES
            + 4 * usize::from(MAX_MONITOR_FRAME_WIDTH) * usize::from(MAX_MONITOR_FRAME_HEIGHT)
            + QOI_END_MARKER.len();
        assert_eq!(worst, 921_622);
        assert_eq!(worst.div_ceil(MAX_MONITOR_FRAME_PART_BYTES), 29);
    }

    #[test]
    fn encoder_rejects_invalid_dimensions_and_layouts() {
        for (width, height) in [(0, 1), (1, 0), (641, 1), (1, 361)] {
            assert_eq!(
                encode_qoi_rgb(width, height, &[0; 4096], 4096, 3),
                Err(MonitorImageError::InvalidDimensions { width, height })
            );
        }
        assert_eq!(
            encode_qoi_rgb(2, 1, &[0; 8], 8, 2),
            Err(MonitorImageError::InvalidPixelLayout)
        );
        assert_eq!(
            encode_qoi_rgb(2, 1, &[0; 8], 5, 3),
            Err(MonitorImageError::InvalidPixelLayout)
        );
        assert_eq!(
            encode_qoi_rgb(2, 2, &[0; 11], 6, 3),
            Err(MonitorImageError::InvalidPixelLayout)
        );
    }

    #[test]
    fn decoder_output_is_opaque_after_an_index_op_on_an_unwritten_slot() {
        // `0x05` loads the never-written all-zero index slot 5 (alpha 0), and
        // the zero `QOI_OP_DIFF` and a run then carry that pixel forward.
        let data = stream(4, 1, &[0x05, 0x6a, 0xc1]);
        assert_eq!(
            decode_qoi_rgb(4, 1, &data).unwrap(),
            [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn decoder_is_strict() {
        let valid = stream(2, 1, &[0xc1]);
        assert_eq!(
            decode_qoi_rgb(2, 1, &valid).unwrap(),
            [0, 0, 0, 255, 0, 0, 0, 255]
        );

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'Q';
        let mut four_channels = valid.clone();
        four_channels[12] = 4;
        let mut linear = valid.clone();
        linear[13] = 1;
        for data in [bad_magic, four_channels, linear, valid[..20].to_vec()] {
            assert_eq!(
                decode_qoi_rgb(2, 1, &data),
                Err(MonitorImageError::InvalidHeader)
            );
        }
        assert_eq!(
            decode_qoi_rgb(1, 2, &valid),
            Err(MonitorImageError::InvalidHeader)
        );
        assert_eq!(
            decode_qoi_rgb(0, 1, &valid),
            Err(MonitorImageError::InvalidDimensions {
                width: 0,
                height: 1
            })
        );
        assert_eq!(
            decode_qoi_rgb(2, 1, &stream(2, 1, &[0xc2])),
            Err(MonitorImageError::RunOverflow)
        );
        assert_eq!(
            decode_qoi_rgb(2, 1, &stream(2, 1, &[0xff, 1, 2, 3, 255, 0xc0])),
            Err(MonitorImageError::InvalidChunk)
        );
        // Too few pixels: the end marker cannot be consumed as ops.
        assert_eq!(
            decode_qoi_rgb(2, 1, &stream(2, 1, &[0xc0])),
            Err(MonitorImageError::InvalidChunk)
        );
        // Truncated multi-byte op.
        assert_eq!(
            decode_qoi_rgb(1, 1, &stream(1, 1, &[0xfe, 1])),
            Err(MonitorImageError::InvalidChunk)
        );
        let mut wrong_marker = valid.clone();
        *wrong_marker.last_mut().unwrap() = 2;
        assert_eq!(
            decode_qoi_rgb(2, 1, &wrong_marker),
            Err(MonitorImageError::InvalidEndMarker)
        );
        let mut extra_op = qoi_header(2, 1).to_vec();
        extra_op.extend_from_slice(&[0xc1, 0xc0]);
        extra_op.extend_from_slice(&QOI_END_MARKER);
        assert_eq!(
            decode_qoi_rgb(2, 1, &extra_op),
            Err(MonitorImageError::InvalidEndMarker)
        );
        let mut trailing = valid;
        trailing.push(0);
        assert_eq!(
            decode_qoi_rgb(2, 1, &trailing),
            Err(MonitorImageError::TrailingData)
        );
    }

    #[test]
    fn base64url_is_canonical_and_unpadded() {
        for (raw, text) in [
            (&b""[..], ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (&[0xfb, 0xff], "-_8"),
            (&[0xff, 0xff, 0xff], "____"),
        ] {
            assert_eq!(base64url_encode(raw), text);
            assert_eq!(base64url_decode(text).as_deref(), Some(raw));
        }
        for invalid in ["Z", "Zh", "Zm9", "Zg==", "+/8", "Zm 9v", "Zm9vY", "Zm9v\n"] {
            assert_eq!(base64url_decode(invalid), None, "{invalid:?}");
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64url_decode(&base64url_encode(&all)).unwrap(), all);
    }

    fn parts_for(width: u16, height: u16, sequence: u64) -> Vec<MonitorFrameMessage> {
        let rgb = noise(width, height);
        let qoi = encode_qoi_rgb(width, height, &rgb, usize::from(width) * 3, 3).unwrap();
        monitor_frame_parts(
            &server(),
            MonitorTap::Program,
            sequence,
            40,
            input(),
            MonitorFrameOrigin::Simulated,
            width,
            height,
            &qoi,
        )
    }

    #[test]
    fn frame_parts_split_and_reassemble() {
        let parts = parts_for(200, 100, 5);
        assert_eq!(parts.len(), 3);
        for (index, part) in parts.iter().enumerate() {
            assert_eq!(usize::from(part.part), index);
            assert_eq!(part.parts, 3);
            assert!(part.data.len() <= MAX_MONITOR_FRAME_PART_BYTES);
        }
        let mut assembler = MonitorFrameAssembler::new();
        assert_eq!(assembler.push(&parts[0]), Ok(None));
        assert_eq!(assembler.in_progress_sequence(), Some(5));
        assert_eq!(assembler.push(&parts[1]), Ok(None));
        let image = assembler.push(&parts[2]).unwrap().unwrap();
        assert!(!assembler.is_in_progress());
        assert_eq!(
            (image.tap, image.sequence, image.frame, image.input),
            (MonitorTap::Program, 5, 40, input())
        );
        assert_eq!((image.width, image.height), (200, 100));
        assert_eq!(&*image.rgba, rgba_of(&noise(200, 100)).as_slice());

        let single = parts_for(2, 2, 6);
        assert_eq!(single.len(), 1);
        assert!(assembler.push(&single[0]).unwrap().is_some());
    }

    #[test]
    fn assembler_rejects_out_of_order_and_mismatched_parts() {
        let parts = parts_for(200, 100, 5);
        let mut assembler = MonitorFrameAssembler::new();
        assert_eq!(
            assembler.push(&parts[1]),
            Err(MonitorImageError::UnexpectedPart {
                expected: 0,
                actual: 1
            })
        );

        assembler.push(&parts[0]).unwrap();
        assert_eq!(
            assembler.push(&parts[2]),
            Err(MonitorImageError::UnexpectedPart {
                expected: 1,
                actual: 2
            })
        );
        assert!(!assembler.is_in_progress());

        assembler.push(&parts[0]).unwrap();
        let mut restart = parts[0].clone();
        restart.sequence = 6;
        assert!(assembler.push(&restart).is_err());
        assert!(!assembler.is_in_progress());

        for mutate in [
            (|part: &mut MonitorFrameMessage| part.sequence += 1) as fn(&mut MonitorFrameMessage),
            |part| part.frame += 1,
            |part| part.tap = MonitorTap::Preview,
            |part| part.origin = MonitorFrameOrigin::Native,
            |part| part.width -= 1,
            |part| part.parts += 1,
            |part| part.server.state_epoch += 1,
            |part| part.input = WireInputId::new(NonZeroU128::new(9).unwrap()),
        ] {
            assembler.push(&parts[0]).unwrap();
            let mut next = parts[1].clone();
            mutate(&mut next);
            assert_eq!(
                assembler.push(&next),
                Err(MonitorImageError::HeaderMismatch)
            );
            assert!(!assembler.is_in_progress());
        }

        let mut too_many = parts[0].clone();
        too_many.parts = MAX_MONITOR_FRAME_PARTS + 1;
        assert!(assembler.push(&too_many).is_err());
        let mut empty = parts[0].clone();
        empty.data.clear();
        assert!(assembler.push(&empty).is_err());
        let mut oversized = parts[0].clone();
        oversized.width = MAX_MONITOR_FRAME_WIDTH + 1;
        assert!(assembler.push(&oversized).is_err());
    }

    #[test]
    fn assembler_rejects_an_invalid_completed_stream() {
        let mut parts = parts_for(4, 4, 1);
        parts[0].data.push(0);
        let mut assembler = MonitorFrameAssembler::new();
        assert_eq!(
            assembler.push(&parts[0]),
            Err(MonitorImageError::TrailingData)
        );
        assert!(!assembler.is_in_progress());
    }
}
