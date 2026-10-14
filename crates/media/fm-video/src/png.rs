//! Exact, lossless, dependency-free PNG still encoder.
//!
//! Output is a non-interlaced truecolor-with-alpha PNG (color type 6) at 8 or
//! 16 bits per sample. Every scanline uses filter type 0 (None) and the zlib
//! stream contains only stored (uncompressed) DEFLATE blocks, so the encoded
//! bytes are a pure deterministic function of the pixel values.

use core::fmt;

use crate::ImageFrame;

/// PNG file signature.
pub const PNG_SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// Largest stored DEFLATE block payload.
pub const STORED_BLOCK_MAX: usize = 65_535;

/// Upper bound on filtered scanline bytes accepted by the encoder.
pub const MAX_PNG_RAW_BYTES: usize = 512 * 1024 * 1024;

/// Largest PNG dimension allowed by the specification.
pub const MAX_PNG_DIMENSION: u32 = 0x7fff_ffff;

/// Sample depth of an encoded RGBA still.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PngBitDepth {
    Eight,
    Sixteen,
}

impl PngBitDepth {
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }

    const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Eight => 4,
            Self::Sixteen => 8,
        }
    }
}

/// Explicit PNG encoding failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PngEncodeError {
    ZeroWidth,
    ZeroHeight,
    DimensionTooLarge { actual: u32, maximum: u32 },
    SampleCountMismatch { expected: usize, actual: usize },
    LayoutOverflow,
    RawDataTooLarge { required: usize, maximum: usize },
}

impl fmt::Display for PngEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroWidth => formatter.write_str("PNG width must be nonzero"),
            Self::ZeroHeight => formatter.write_str("PNG height must be nonzero"),
            Self::DimensionTooLarge { actual, maximum } => {
                write!(formatter, "PNG dimension {actual} exceeds {maximum}")
            }
            Self::SampleCountMismatch { expected, actual } => {
                write!(formatter, "expected {expected} samples, got {actual}")
            }
            Self::LayoutOverflow => formatter.write_str("PNG layout overflows addressable memory"),
            Self::RawDataTooLarge { required, maximum } => write!(
                formatter,
                "PNG scanline data of {required} bytes exceeds {maximum}"
            ),
        }
    }
}

impl std::error::Error for PngEncodeError {}

/// Encodes an RGBA8 frame (straight alpha as stored; row padding ignored).
///
/// # Errors
///
/// Returns [`PngEncodeError::RawDataTooLarge`] if the filtered scanlines exceed
/// [`MAX_PNG_RAW_BYTES`].
pub fn encode_png_rgba8(frame: &ImageFrame) -> Result<Vec<u8>, PngEncodeError> {
    let (row_bytes, raw_len) = raw_layout(frame.width(), frame.height(), PngBitDepth::Eight)?;
    let mut raw = Vec::with_capacity(raw_len);
    for row in frame
        .pixels()
        .chunks(frame.stride())
        .take(height_usize(frame.height())?)
    {
        raw.push(0);
        raw.extend_from_slice(&row[..row_bytes]);
    }
    Ok(assemble(
        frame.width(),
        frame.height(),
        PngBitDepth::Eight,
        &raw,
    ))
}

/// Encodes tightly packed RGBA16 samples (`width * height * 4` values,
/// row-major, R G B A order). Samples are written big-endian as PNG requires.
///
/// # Errors
///
/// Returns typed errors for zero or oversized dimensions, a sample count
/// mismatch, or scanline data exceeding [`MAX_PNG_RAW_BYTES`].
pub fn encode_png_rgba16(
    width: u32,
    height: u32,
    samples: &[u16],
) -> Result<Vec<u8>, PngEncodeError> {
    let (row_bytes, raw_len) = raw_layout(width, height, PngBitDepth::Sixteen)?;
    let row_samples = row_bytes / 2;
    let expected = row_samples
        .checked_mul(height_usize(height)?)
        .ok_or(PngEncodeError::LayoutOverflow)?;
    if samples.len() != expected {
        return Err(PngEncodeError::SampleCountMismatch {
            expected,
            actual: samples.len(),
        });
    }
    let mut raw = Vec::with_capacity(raw_len);
    for row in samples.chunks(row_samples) {
        raw.push(0);
        for sample in row {
            raw.extend_from_slice(&sample.to_be_bytes());
        }
    }
    Ok(assemble(width, height, PngBitDepth::Sixteen, &raw))
}

/// CRC-32 (ISO-HDLC / PNG polynomial `0xEDB88320`, reflected).
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32_update(0xffff_ffff, bytes) ^ 0xffff_ffff
}

/// Adler-32 checksum as used by zlib.
#[must_use]
pub fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    // 5552 is the largest run that cannot overflow u32 before reduction.
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MODULUS;
        b %= MODULUS;
    }
    (b << 16) | a
}

const CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

fn height_usize(height: u32) -> Result<usize, PngEncodeError> {
    usize::try_from(height).map_err(|_| PngEncodeError::LayoutOverflow)
}

/// Returns `(row_bytes_without_filter, total_filtered_bytes)`.
fn raw_layout(
    width: u32,
    height: u32,
    depth: PngBitDepth,
) -> Result<(usize, usize), PngEncodeError> {
    if width == 0 {
        return Err(PngEncodeError::ZeroWidth);
    }
    if height == 0 {
        return Err(PngEncodeError::ZeroHeight);
    }
    for dimension in [width, height] {
        if dimension > MAX_PNG_DIMENSION {
            return Err(PngEncodeError::DimensionTooLarge {
                actual: dimension,
                maximum: MAX_PNG_DIMENSION,
            });
        }
    }
    let width = usize::try_from(width).map_err(|_| PngEncodeError::LayoutOverflow)?;
    let row_bytes = width
        .checked_mul(depth.bytes_per_pixel())
        .ok_or(PngEncodeError::LayoutOverflow)?;
    let required = row_bytes
        .checked_add(1)
        .and_then(|filtered| filtered.checked_mul(height_usize(height).ok()?))
        .ok_or(PngEncodeError::LayoutOverflow)?;
    if required > MAX_PNG_RAW_BYTES {
        return Err(PngEncodeError::RawDataTooLarge {
            required,
            maximum: MAX_PNG_RAW_BYTES,
        });
    }
    Ok((row_bytes, required))
}

fn assemble(width: u32, height: u32, depth: PngBitDepth, raw: &[u8]) -> Vec<u8> {
    let zlib = zlib_stored(raw);
    let mut out = Vec::with_capacity(PNG_SIGNATURE.len() + 25 + 12 + zlib.len() + 12);
    out.extend_from_slice(&PNG_SIGNATURE);

    let mut header = [0u8; 13];
    header[0..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    header[8] = depth.bits();
    header[9] = 6; // truecolor with alpha
    // compression 0, filter method 0, interlace 0 remain zero.
    write_chunk(&mut out, *b"IHDR", &header);
    write_chunk(&mut out, *b"IDAT", &zlib);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    // Bounded by MAX_PNG_RAW_BYTES plus stored-block overhead, < 2^31.
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32_update(crc32_update(0xffff_ffff, &kind), data) ^ 0xffff_ffff;
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Wraps `raw` in a zlib stream of stored DEFLATE blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let blocks = raw.len().div_ceil(STORED_BLOCK_MAX).max(1);
    let mut out = Vec::with_capacity(2 + raw.len() + blocks * 5 + 4);
    // CMF: deflate, 32 KiB window. FLG: no dictionary, level 0, FCHECK so
    // that (CMF * 256 + FLG) % 31 == 0.
    out.extend_from_slice(&[0x78, 0x01]);
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    let mut chunks = raw.chunks(STORED_BLOCK_MAX).peekable();
    while let Some(chunk) = chunks.next() {
        let final_flag = u8::from(chunks.peek().is_none());
        // STORED_BLOCK_MAX fits in u16.
        let length = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.push(final_flag);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}
