//! Pure, crash-recoverable RIFF/WAVE and RF64 PCM writer.
//!
//! The header is written once at creation with zero sizes and has a fixed
//! length, so sample data always begins at the same offset. Samples are
//! converted into a bounded buffer that is flushed in whole frames;
//! [`WavWriter::checkpoint`] and [`WavWriter::finalize`] rewrite the header
//! with the current sizes. When [`LargeFilePolicy::Rf64WhenNeeded`] is chosen a
//! 28-byte `JUNK` chunk is reserved directly after `WAVE` (EBU Tech 3306) and is
//! promoted to `ds64` when the file outgrows 32-bit RIFF sizes.
//!
//! A file whose writer never finalized (process kill, power loss) is repaired
//! by [`recover_wav`], which recomputes every size from the data length,
//! drops a trailing partial frame, and restores the RIFF pad byte.

use core::fmt;
use std::{
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use fm_types::{FrameRate, Timecode};

/// Maximum channels in one WAV file.
pub const MAX_WAV_CHANNELS: u16 = 64;
/// Maximum bytes held in a writer's block buffer.
pub const MAX_WAV_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// Fixed (non-coding-history) size of a `bext` chunk payload.
pub const BEXT_FIXED_BYTES: usize = 602;
/// Maximum coding-history bytes accepted in a `bext` chunk.
pub const MAX_CODING_HISTORY_BYTES: usize = 1024;
/// Size of the reserved `JUNK`/`ds64` payload.
pub const DS64_PAYLOAD_BYTES: u32 = 28;
/// Largest RIFF size expressible without RF64 (`0xFFFF_FFFF` is the RF64 marker).
pub const MAX_RIFF_SIZE: u64 = 0xFFFF_FFFE;

const MAX_HEADER_CHUNKS: usize = 64;
const MAX_FMT_BYTES: u32 = 1024;
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// Bytes 2..16 of the `KSDATAFORMAT_SUBTYPE_*` GUIDs shared by PCM and float.
const SUBFORMAT_GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// Write side of a WAV sink. Blanket-implemented for every `Write + Seek`.
pub trait WavSink: Write + Seek {}
impl<T: Write + Seek + ?Sized> WavSink for T {}

/// Read side used to inspect WAV headers.
pub trait WavSource: Read + Seek {}
impl<T: Read + Seek + ?Sized> WavSource for T {}

/// Read/write target used by crash recovery.
pub trait WavRecoveryTarget: Read + Write + Seek {}
impl<T: Read + Write + Seek + ?Sized> WavRecoveryTarget for T {}

/// Sample encoding of WAV data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WavSampleFormat {
    Pcm16,
    Pcm24,
    Pcm32,
    Float32,
}

impl WavSampleFormat {
    #[must_use]
    pub const fn bits_per_sample(self) -> u16 {
        match self {
            Self::Pcm16 => 16,
            Self::Pcm24 => 24,
            Self::Pcm32 | Self::Float32 => 32,
        }
    }

    #[must_use]
    pub const fn bytes_per_sample(self) -> u16 {
        self.bits_per_sample() / 8
    }

    const fn format_tag(self) -> u16 {
        match self {
            Self::Float32 => WAVE_FORMAT_IEEE_FLOAT,
            _ => WAVE_FORMAT_PCM,
        }
    }
}

/// What to do when data would exceed 32-bit RIFF sizes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LargeFilePolicy {
    /// Plain RIFF; writes that would exceed [`MAX_RIFF_SIZE`] are refused.
    RiffOnly,
    /// Reserve a `JUNK` chunk and promote to RF64/`ds64` when required.
    Rf64WhenNeeded,
}

/// Broadcast Wave Format `bext` metadata (written as version 1).
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct BextMetadata {
    /// Up to 256 printable ASCII bytes.
    pub description: String,
    /// Up to 32 printable ASCII bytes.
    pub originator: String,
    /// Up to 32 printable ASCII bytes.
    pub originator_reference: String,
    /// Empty or `yyyy-mm-dd` (separators `-_:. ` allowed). Supplied by the caller;
    /// the writer never reads a clock.
    pub origination_date: String,
    /// Empty or `hh:mm:ss` (separators `-_:. ` allowed).
    pub origination_time: String,
    /// Sample count since midnight of the first sample.
    pub time_reference: u64,
    /// Printable ASCII plus CR/LF, at most [`MAX_CODING_HISTORY_BYTES`].
    pub coding_history: String,
}

/// Field of [`BextMetadata`] that failed validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BextField {
    Description,
    Originator,
    OriginatorReference,
    OriginationDate,
    OriginationTime,
    CodingHistory,
}

impl BextMetadata {
    /// Converts a non-drop-frame timecode into a `bext` time reference.
    ///
    /// The frame count `((h*3600 + m*60 + s) * nominal + f)` uses the nominal
    /// integer rate `ceil(rate)` and is scaled to samples with the exact
    /// rational `rate`, rounding down.
    ///
    /// # Errors
    ///
    /// Returns [`WavError::InvalidTimecode`] when the frame component is not
    /// below the nominal rate, and [`WavError::ZeroSampleRate`] for a zero rate.
    pub fn time_reference_for_timecode(
        timecode: Timecode,
        rate: FrameRate,
        sample_rate: u32,
    ) -> Result<u64, WavError> {
        if sample_rate == 0 {
            return Err(WavError::ZeroSampleRate);
        }
        let numerator = u128::from(rate.numerator());
        let denominator = u128::from(rate.denominator());
        let nominal = numerator.div_ceil(denominator);
        let (hours, minutes, seconds, frames) = timecode.components();
        if u128::from(frames) >= nominal {
            return Err(WavError::InvalidTimecode);
        }
        let whole_seconds =
            u128::from(hours) * 3600 + u128::from(minutes) * 60 + u128::from(seconds);
        let frame_count = whole_seconds * nominal + u128::from(frames);
        let samples = frame_count * u128::from(sample_rate) * denominator / numerator;
        u64::try_from(samples).map_err(|_| WavError::InvalidTimecode)
    }

    /// Checks field lengths and character sets.
    ///
    /// # Errors
    ///
    /// Returns [`WavError::InvalidBext`] naming the first invalid field.
    pub fn validate(&self) -> Result<(), WavError> {
        let fixed = [
            (&self.description, 256, BextField::Description),
            (&self.originator, 32, BextField::Originator),
            (
                &self.originator_reference,
                32,
                BextField::OriginatorReference,
            ),
        ];
        for (value, limit, field) in fixed {
            if value.len() > limit || !value.bytes().all(is_printable_ascii) {
                return Err(WavError::InvalidBext { field });
            }
        }
        if !valid_bext_stamp(&self.origination_date, &[4, 2, 2]) {
            return Err(WavError::InvalidBext {
                field: BextField::OriginationDate,
            });
        }
        if !valid_bext_stamp(&self.origination_time, &[2, 2, 2]) {
            return Err(WavError::InvalidBext {
                field: BextField::OriginationTime,
            });
        }
        if self.coding_history.len() > MAX_CODING_HISTORY_BYTES
            || !self
                .coding_history
                .bytes()
                .all(|byte| is_printable_ascii(byte) || byte == b'\r' || byte == b'\n')
        {
            return Err(WavError::InvalidBext {
                field: BextField::CodingHistory,
            });
        }
        Ok(())
    }

    fn payload_len(&self) -> usize {
        BEXT_FIXED_BYTES + self.coding_history.len()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        put_fixed_ascii(out, &self.description, 256);
        put_fixed_ascii(out, &self.originator, 32);
        put_fixed_ascii(out, &self.originator_reference, 32);
        put_fixed_ascii(out, &self.origination_date, 10);
        put_fixed_ascii(out, &self.origination_time, 8);
        out.extend_from_slice(&self.time_reference.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        // UMID (64) and version-1 reserved bytes (190) stay zero.
        out.resize(out.len() + 64 + 190, 0);
        out.extend_from_slice(self.coding_history.as_bytes());
    }

    fn decode(payload: &[u8]) -> Result<Self, WavError> {
        if payload.len() < BEXT_FIXED_BYTES {
            return Err(WavError::Malformed("bext chunk is shorter than 602 bytes"));
        }
        let time_low = u64::from(le_u32(&payload[338..342]));
        let time_high = u64::from(le_u32(&payload[342..346]));
        Ok(Self {
            description: fixed_ascii(&payload[0..256]),
            originator: fixed_ascii(&payload[256..288]),
            originator_reference: fixed_ascii(&payload[288..320]),
            origination_date: fixed_ascii(&payload[320..330]),
            origination_time: fixed_ascii(&payload[330..338]),
            time_reference: time_low | (time_high << 32),
            coding_history: fixed_ascii(&payload[BEXT_FIXED_BYTES..]),
        })
    }
}

/// Complete description of a WAV file's format and metadata.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WavSpec {
    pub channels: u16,
    pub sample_rate: u32,
    pub format: WavSampleFormat,
    /// `dwChannelMask` speaker bits. Zero means direct-out (no speaker
    /// assignment). Files with more than two channels or a nonzero mask use
    /// `WAVE_FORMAT_EXTENSIBLE`.
    pub channel_mask: u32,
    pub large_file: LargeFilePolicy,
    pub bext: Option<BextMetadata>,
}

impl WavSpec {
    /// A spec with no channel mask, RF64 reservation, and no `bext`.
    #[must_use]
    pub const fn new(channels: u16, sample_rate: u32, format: WavSampleFormat) -> Self {
        Self {
            channels,
            sample_rate,
            format,
            channel_mask: 0,
            large_file: LargeFilePolicy::Rf64WhenNeeded,
            bext: None,
        }
    }

    /// Bytes per interleaved frame.
    #[must_use]
    pub fn block_align(&self) -> u16 {
        self.channels.saturating_mul(self.format.bytes_per_sample())
    }

    /// Checks channel, rate, mask, and metadata limits.
    ///
    /// # Errors
    ///
    /// Returns the first violated [`WavError`] constraint.
    pub fn validate(&self) -> Result<(), WavError> {
        if self.channels == 0 || self.channels > MAX_WAV_CHANNELS {
            return Err(WavError::InvalidChannelCount {
                channels: self.channels,
            });
        }
        if self.sample_rate == 0 {
            return Err(WavError::ZeroSampleRate);
        }
        if u64::from(self.sample_rate) * u64::from(self.block_align()) > u64::from(u32::MAX) {
            return Err(WavError::ByteRateOverflow);
        }
        if self.channel_mask.count_ones() > u32::from(self.channels) {
            return Err(WavError::InvalidChannelMask {
                mask: self.channel_mask,
            });
        }
        if let Some(bext) = &self.bext {
            bext.validate()?;
        }
        Ok(())
    }

    fn uses_extensible(&self) -> bool {
        self.channels > 2 || self.channel_mask != 0
    }

    /// Length of the encoded header; sample data starts at this offset.
    #[must_use]
    pub fn header_len(&self) -> u64 {
        let mut len = 12_u64;
        if self.large_file == LargeFilePolicy::Rf64WhenNeeded {
            len += 8 + u64::from(DS64_PAYLOAD_BYTES);
        }
        len += 8 + if self.uses_extensible() {
            40
        } else if self.format == WavSampleFormat::Float32 {
            18
        } else {
            16
        };
        if self.format == WavSampleFormat::Float32 {
            len += 12;
        }
        if let Some(bext) = &self.bext {
            let payload = bext.payload_len() as u64;
            len += 8 + payload + (payload & 1);
        }
        len + 8
    }

    /// Largest data length this spec can describe.
    #[must_use]
    pub fn max_data_bytes(&self) -> u64 {
        match self.large_file {
            LargeFilePolicy::RiffOnly => {
                let mut limit = MAX_RIFF_SIZE - (self.header_len() - 8);
                if limit & 1 == 1 {
                    // An odd data length needs a pad byte, so the largest
                    // usable length is the even value at or below the limit.
                    limit -= 1;
                }
                limit
            }
            LargeFilePolicy::Rf64WhenNeeded => u64::MAX / 2,
        }
    }
}

/// Encodes the complete header for `spec` describing `data_bytes` of samples.
///
/// The returned length always equals [`WavSpec::header_len`]. Sizes include
/// the RIFF pad byte required after an odd-length data chunk.
///
/// # Errors
///
/// Returns spec validation errors and [`WavError::SizeLimit`] when
/// `data_bytes` exceeds [`WavSpec::max_data_bytes`].
#[allow(clippy::cast_possible_truncation)]
pub fn encode_wav_header(spec: &WavSpec, data_bytes: u64) -> Result<Vec<u8>, WavError> {
    spec.validate()?;
    let limit = spec.max_data_bytes();
    if data_bytes > limit {
        return Err(WavError::SizeLimit {
            requested: data_bytes,
            limit,
        });
    }
    let header_len = spec.header_len();
    let riff_size = header_len - 8 + data_bytes + (data_bytes & 1);
    let rf64 = riff_size > MAX_RIFF_SIZE || data_bytes > MAX_RIFF_SIZE;
    let frames = data_bytes / u64::from(spec.block_align());
    let block_align = spec.block_align();
    let byte_rate = spec.sample_rate * u32::from(block_align);
    let mut out = Vec::with_capacity(usize::try_from(header_len).unwrap_or(0));

    out.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
    out.extend_from_slice(&size32(riff_size, rf64).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    if spec.large_file == LargeFilePolicy::Rf64WhenNeeded {
        if rf64 {
            out.extend_from_slice(b"ds64");
            out.extend_from_slice(&DS64_PAYLOAD_BYTES.to_le_bytes());
            out.extend_from_slice(&riff_size.to_le_bytes());
            out.extend_from_slice(&data_bytes.to_le_bytes());
            out.extend_from_slice(&frames.to_le_bytes());
            out.extend_from_slice(&0_u32.to_le_bytes());
        } else {
            out.extend_from_slice(b"JUNK");
            out.extend_from_slice(&DS64_PAYLOAD_BYTES.to_le_bytes());
            out.extend_from_slice(&[0; DS64_PAYLOAD_BYTES as usize]);
        }
    }

    out.extend_from_slice(b"fmt ");
    let bits = spec.format.bits_per_sample();
    if spec.uses_extensible() {
        out.extend_from_slice(&40_u32.to_le_bytes());
        out.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
    } else if spec.format == WavSampleFormat::Float32 {
        out.extend_from_slice(&18_u32.to_le_bytes());
        out.extend_from_slice(&spec.format.format_tag().to_le_bytes());
    } else {
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&spec.format.format_tag().to_le_bytes());
    }
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    if spec.uses_extensible() {
        out.extend_from_slice(&22_u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(&spec.channel_mask.to_le_bytes());
        out.extend_from_slice(&spec.format.format_tag().to_le_bytes());
        out.extend_from_slice(&SUBFORMAT_GUID_TAIL);
    } else if spec.format == WavSampleFormat::Float32 {
        out.extend_from_slice(&0_u16.to_le_bytes());
    }

    if spec.format == WavSampleFormat::Float32 {
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4_u32.to_le_bytes());
        out.extend_from_slice(&size32(frames, rf64 || frames > MAX_RIFF_SIZE).to_le_bytes());
    }

    if let Some(bext) = &spec.bext {
        let payload = bext.payload_len();
        out.extend_from_slice(b"bext");
        // Validation bounds the payload far below u32::MAX.
        out.extend_from_slice(&(payload as u32).to_le_bytes());
        bext.encode_into(&mut out);
        if payload & 1 == 1 {
            out.push(0);
        }
    }

    out.extend_from_slice(b"data");
    out.extend_from_slice(&size32(data_bytes, rf64).to_le_bytes());
    debug_assert_eq!(out.len() as u64, header_len);
    Ok(out)
}

fn size32(value: u64, rf64: bool) -> u32 {
    if rf64 {
        u32::MAX
    } else {
        u32::try_from(value).unwrap_or(u32::MAX)
    }
}

/// Converts one normalized sample and appends its little-endian bytes.
///
/// Integer formats scale by `2^(bits-1)`, round half away from zero, and
/// clamp to the format range. `NaN` encodes as silence; float output maps
/// infinities to `±1.0`.
pub fn encode_sample(format: WavSampleFormat, sample: f32, out: &mut Vec<u8>) {
    let value = if sample.is_nan() { 0.0 } else { sample };
    match format {
        WavSampleFormat::Pcm16 => {
            out.extend_from_slice(&quantize(value, 32_768.0).to_le_bytes()[..2]);
        }
        WavSampleFormat::Pcm24 => {
            out.extend_from_slice(&quantize(value, 8_388_608.0).to_le_bytes()[..3]);
        }
        WavSampleFormat::Pcm32 => {
            out.extend_from_slice(&quantize(value, 2_147_483_648.0).to_le_bytes());
        }
        WavSampleFormat::Float32 => {
            let finite = if value.is_infinite() {
                value.signum()
            } else {
                value
            };
            out.extend_from_slice(&finite.to_le_bytes());
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn quantize(sample: f32, scale: f64) -> i32 {
    // The clamp keeps the value inside the target range, so the cast is exact.
    (f64::from(sample) * scale)
        .round()
        .clamp(-scale, scale - 1.0) as i32
}

/// Writer and recovery failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WavError {
    InvalidChannelCount {
        channels: u16,
    },
    ZeroSampleRate,
    ByteRateOverflow,
    InvalidChannelMask {
        mask: u32,
    },
    InvalidBext {
        field: BextField,
    },
    InvalidTimecode,
    InvalidBufferFrames {
        frames: u32,
    },
    PartialFrame {
        len: usize,
        frame: usize,
    },
    SizeLimit {
        requested: u64,
        limit: u64,
    },
    /// An earlier I/O error left the writer in an unknown state.
    Poisoned,
    Io(io::ErrorKind),
    NotWave,
    Truncated,
    Malformed(&'static str),
    UnsupportedFormat {
        tag: u16,
        bits: u16,
    },
    /// The file outgrew RIFF sizes but has no reserved `JUNK`/`ds64` chunk.
    Rf64Unavailable,
}

impl fmt::Display for WavError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannelCount { channels } => write!(
                formatter,
                "WAV channel count {channels} is outside 1..={MAX_WAV_CHANNELS}"
            ),
            Self::ZeroSampleRate => formatter.write_str("WAV sample rate must be nonzero"),
            Self::ByteRateOverflow => formatter.write_str("WAV byte rate does not fit 32 bits"),
            Self::InvalidChannelMask { mask } => write!(
                formatter,
                "channel mask {mask:#x} assigns more speakers than channels"
            ),
            Self::InvalidBext { field } => write!(formatter, "invalid bext field {field:?}"),
            Self::InvalidTimecode => formatter.write_str("timecode is invalid for the frame rate"),
            Self::InvalidBufferFrames { frames } => {
                write!(formatter, "WAV buffer of {frames} frames is out of range")
            }
            Self::PartialFrame { len, frame } => write!(
                formatter,
                "{len} samples or bytes is not a whole number of {frame}-unit frames"
            ),
            Self::SizeLimit { requested, limit } => write!(
                formatter,
                "WAV data of {requested} bytes exceeds the {limit}-byte limit"
            ),
            Self::Poisoned => formatter.write_str("WAV writer failed earlier and is closed"),
            Self::Io(kind) => write!(formatter, "WAV I/O failed: {kind}"),
            Self::NotWave => formatter.write_str("not a RIFF/RF64 WAVE file"),
            Self::Truncated => formatter.write_str("WAV header is truncated"),
            Self::Malformed(reason) => write!(formatter, "malformed WAV: {reason}"),
            Self::UnsupportedFormat { tag, bits } => write!(
                formatter,
                "unsupported WAV format tag {tag:#06x} with {bits} bits"
            ),
            Self::Rf64Unavailable => {
                formatter.write_str("WAV data exceeds RIFF limits and no ds64 space is reserved")
            }
        }
    }
}

impl std::error::Error for WavError {}

impl From<io::Error> for WavError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Self::Truncated
        } else {
            Self::Io(error.kind())
        }
    }
}

/// Result of a finalized writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavSummary {
    pub data_bytes: u64,
    pub frames: u64,
    pub file_len: u64,
    pub rf64: bool,
}

/// A finalized file and the sink it was written through.
pub struct FinalizedWav {
    pub summary: WavSummary,
    pub sink: Box<dyn WavSink>,
}

impl fmt::Debug for FinalizedWav {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalizedWav")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

/// Bounded, buffered WAV writer over a `Write + Seek` trait object.
pub struct WavWriter {
    sink: Box<dyn WavSink>,
    spec: WavSpec,
    header_len: u64,
    buffer: Vec<u8>,
    buffer_limit: usize,
    written: u64,
    poisoned: bool,
}

impl fmt::Debug for WavWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WavWriter")
            .field("spec", &self.spec)
            .field("written", &self.written)
            .field("buffered", &self.buffer.len())
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl WavWriter {
    /// Writes a zero-size header at offset 0 and returns a writer whose block
    /// buffer holds `buffer_frames` frames.
    ///
    /// # Errors
    ///
    /// Returns spec validation errors, [`WavError::InvalidBufferFrames`] when
    /// the buffer is empty or exceeds [`MAX_WAV_BUFFER_BYTES`], and I/O errors.
    pub fn create(
        mut sink: Box<dyn WavSink>,
        spec: WavSpec,
        buffer_frames: u32,
    ) -> Result<Self, WavError> {
        let header = encode_wav_header(&spec, 0)?;
        let buffer_limit = usize::try_from(buffer_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(spec.block_align())))
            .filter(|bytes| *bytes > 0 && *bytes <= MAX_WAV_BUFFER_BYTES)
            .ok_or(WavError::InvalidBufferFrames {
                frames: buffer_frames,
            })?;
        sink.seek(SeekFrom::Start(0))?;
        sink.write_all(&header)?;
        Ok(Self {
            sink,
            header_len: header.len() as u64,
            spec,
            buffer: Vec::with_capacity(buffer_limit),
            buffer_limit,
            written: 0,
            poisoned: false,
        })
    }

    #[must_use]
    pub fn spec(&self) -> &WavSpec {
        &self.spec
    }

    /// Byte offset where sample data begins.
    #[must_use]
    pub fn data_offset(&self) -> u64 {
        self.header_len
    }

    /// Data bytes accepted so far, including buffered bytes.
    #[must_use]
    pub fn data_bytes(&self) -> u64 {
        self.written + self.buffer.len() as u64
    }

    /// Data bytes already handed to the sink.
    #[must_use]
    pub fn flushed_bytes(&self) -> u64 {
        self.written
    }

    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    #[must_use]
    pub fn buffer_capacity_bytes(&self) -> usize {
        self.buffer_limit
    }

    #[must_use]
    pub fn frames(&self) -> u64 {
        self.data_bytes() / u64::from(self.spec.block_align())
    }

    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Whether `frames` more frames fit under the size limit.
    #[must_use]
    pub fn can_accept_frames(&self, frames: u64) -> bool {
        frames
            .checked_mul(u64::from(self.spec.block_align()))
            .and_then(|bytes| bytes.checked_add(self.data_bytes()))
            .is_some_and(|total| total <= self.spec.max_data_bytes())
    }

    /// Appends interleaved normalized samples.
    ///
    /// # Errors
    ///
    /// Refuses partial frames and writes past the size limit without writing
    /// anything; I/O errors poison the writer.
    pub fn write_f32(&mut self, samples: &[f32]) -> Result<(), WavError> {
        self.ensure_live()?;
        let channels = usize::from(self.spec.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(WavError::PartialFrame {
                len: samples.len(),
                frame: channels,
            });
        }
        self.ensure_capacity((samples.len() / channels) as u64)?;
        let format = self.spec.format;
        for sample in samples {
            encode_sample(format, *sample, &mut self.buffer);
            if self.buffer.len() >= self.buffer_limit {
                self.flush_buffer()?;
            }
        }
        Ok(())
    }

    /// Appends already-encoded little-endian sample bytes.
    ///
    /// # Errors
    ///
    /// Same as [`Self::write_f32`].
    pub fn write_encoded(&mut self, bytes: &[u8]) -> Result<(), WavError> {
        self.ensure_live()?;
        let block = usize::from(self.spec.block_align());
        if !bytes.len().is_multiple_of(block) {
            return Err(WavError::PartialFrame {
                len: bytes.len(),
                frame: block,
            });
        }
        self.ensure_capacity((bytes.len() / block) as u64)?;
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = self.buffer_limit - self.buffer.len();
            let take = room.min(rest.len());
            self.buffer.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.buffer.len() >= self.buffer_limit {
                self.flush_buffer()?;
            }
        }
        Ok(())
    }

    /// Flushes buffered frames and rewrites the header with current sizes so
    /// the file on the sink is readable up to this point.
    ///
    /// # Errors
    ///
    /// I/O errors poison the writer.
    pub fn checkpoint(&mut self) -> Result<(), WavError> {
        self.ensure_live()?;
        self.flush_buffer()?;
        let result = self.patch_header(self.written).and_then(|_| {
            self.sink
                .seek(SeekFrom::Start(self.header_len + self.written))?;
            self.sink.flush()?;
            Ok(())
        });
        self.poison_on_err(result)
    }

    /// Flushes data, writes the pad byte for odd data, writes the final
    /// header (promoting to RF64 when needed), and returns the sink.
    ///
    /// # Errors
    ///
    /// Returns [`WavError::Poisoned`] after an earlier failure, or I/O errors.
    /// The partially written file remains recoverable with [`recover_wav`].
    pub fn finalize(mut self) -> Result<FinalizedWav, WavError> {
        self.ensure_live()?;
        self.flush_buffer()?;
        let data_bytes = self.written;
        let pad = data_bytes & 1;
        let result = (|| {
            if pad == 1 {
                self.sink.write_all(&[0])?;
            }
            let rf64 = self.patch_header(data_bytes)?;
            self.sink.flush()?;
            Ok(rf64)
        })();
        let rf64 = self.poison_on_err(result)?;
        Ok(FinalizedWav {
            summary: WavSummary {
                data_bytes,
                frames: data_bytes / u64::from(self.spec.block_align()),
                file_len: self.header_len + data_bytes + pad,
                rf64,
            },
            sink: self.sink,
        })
    }

    fn patch_header(&mut self, data_bytes: u64) -> Result<bool, WavError> {
        let header = encode_wav_header(&self.spec, data_bytes)?;
        self.sink.seek(SeekFrom::Start(0))?;
        self.sink.write_all(&header)?;
        Ok(&header[0..4] == b"RF64")
    }

    fn ensure_live(&self) -> Result<(), WavError> {
        if self.poisoned {
            Err(WavError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn ensure_capacity(&self, frames: u64) -> Result<(), WavError> {
        if self.can_accept_frames(frames) {
            Ok(())
        } else {
            Err(WavError::SizeLimit {
                requested: self
                    .data_bytes()
                    .saturating_add(frames.saturating_mul(u64::from(self.spec.block_align()))),
                limit: self.spec.max_data_bytes(),
            })
        }
    }

    fn flush_buffer(&mut self) -> Result<(), WavError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let result = self.sink.write_all(&self.buffer).map_err(WavError::from);
        self.poison_on_err(result)?;
        self.written += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }

    fn poison_on_err<T>(&mut self, result: Result<T, WavError>) -> Result<T, WavError> {
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

/// Parsed header of an existing WAV file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WavInfo {
    /// Format and metadata. `large_file` is `Rf64WhenNeeded` when a
    /// `JUNK`/`ds64` reservation directly follows `WAVE`.
    pub spec: WavSpec,
    pub rf64: bool,
    /// Declared RIFF size (from `ds64` for RF64 files).
    pub riff_size: u64,
    /// Declared data size (from `ds64` for RF64 files).
    pub data_bytes: u64,
    pub data_offset: u64,
}

impl WavInfo {
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.data_bytes / u64::from(self.spec.block_align())
    }
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_field_names)]
struct ParsedLayout {
    reserve_offset: Option<u64>,
    fact_payload_offset: Option<u64>,
    data_size_offset: u64,
}

/// Reads and validates the header of a WAV or RF64 file.
///
/// # Errors
///
/// Returns [`WavError::NotWave`], [`WavError::Truncated`],
/// [`WavError::Malformed`], or [`WavError::UnsupportedFormat`].
pub fn read_wav_info(source: &mut dyn WavSource) -> Result<WavInfo, WavError> {
    parse_header(source).map(|(info, _)| info)
}

#[allow(clippy::too_many_lines)]
fn parse_header(source: &mut dyn WavSource) -> Result<(WavInfo, ParsedLayout), WavError> {
    source.seek(SeekFrom::Start(0))?;
    let mut head = [0_u8; 12];
    source.read_exact(&mut head)?;
    let rf64 = match &head[0..4] {
        b"RIFF" => false,
        b"RF64" => true,
        _ => return Err(WavError::NotWave),
    };
    if &head[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }
    let mut riff_size = u64::from(le_u32(&head[4..8]));
    let mut ds64: Option<(u64, u64)> = None;
    let mut reserve_offset = None;
    let mut fmt: Option<(WavSampleFormat, u16, u32, u32)> = None;
    let mut fact_payload_offset = None;
    let mut bext = None;
    let mut offset = 12_u64;
    for _ in 0..MAX_HEADER_CHUNKS {
        let mut chunk = [0_u8; 8];
        source.seek(SeekFrom::Start(offset))?;
        source.read_exact(&mut chunk)?;
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = le_u32(&chunk[4..8]);
        let payload_offset = offset + 8;
        match &id {
            b"data" => {
                let Some((format, channels, sample_rate, channel_mask)) = fmt else {
                    return Err(WavError::Malformed("data chunk precedes fmt chunk"));
                };
                let mut data_bytes = u64::from(size);
                if rf64 {
                    let Some((ds_riff, ds_data)) = ds64 else {
                        return Err(WavError::Malformed("RF64 file has no ds64 chunk"));
                    };
                    riff_size = ds_riff;
                    if size == u32::MAX {
                        data_bytes = ds_data;
                    }
                }
                let spec = WavSpec {
                    channels,
                    sample_rate,
                    format,
                    channel_mask,
                    large_file: if reserve_offset.is_some() {
                        LargeFilePolicy::Rf64WhenNeeded
                    } else {
                        LargeFilePolicy::RiffOnly
                    },
                    bext,
                };
                return Ok((
                    WavInfo {
                        spec,
                        rf64,
                        riff_size,
                        data_bytes,
                        data_offset: payload_offset,
                    },
                    ParsedLayout {
                        reserve_offset,
                        fact_payload_offset,
                        data_size_offset: offset + 4,
                    },
                ));
            }
            b"ds64" | b"JUNK" if offset == 12 && size >= DS64_PAYLOAD_BYTES => {
                reserve_offset = Some(offset);
                if &id == b"ds64" {
                    let payload = read_payload(source, payload_offset, DS64_PAYLOAD_BYTES)?;
                    ds64 = Some((le_u64(&payload[0..8]), le_u64(&payload[8..16])));
                }
            }
            b"ds64" => return Err(WavError::Malformed("ds64 chunk is not first")),
            b"fmt " => {
                if !(16..=MAX_FMT_BYTES).contains(&size) {
                    return Err(WavError::Malformed("fmt chunk size is out of range"));
                }
                let payload = read_payload(source, payload_offset, size)?;
                fmt = Some(parse_fmt(&payload)?);
            }
            b"fact" => {
                if size < 4 {
                    return Err(WavError::Malformed("fact chunk is shorter than 4 bytes"));
                }
                fact_payload_offset = Some(payload_offset);
            }
            b"bext" => {
                let limit = u32::try_from(BEXT_FIXED_BYTES + MAX_CODING_HISTORY_BYTES)
                    .expect("bext bound fits u32");
                if size > limit {
                    return Err(WavError::Malformed("bext chunk is too large"));
                }
                let payload = read_payload(source, payload_offset, size)?;
                bext = Some(BextMetadata::decode(&payload)?);
            }
            _ => {}
        }
        offset = payload_offset + u64::from(size) + u64::from(size & 1);
    }
    Err(WavError::Malformed("too many chunks before data"))
}

fn parse_fmt(payload: &[u8]) -> Result<(WavSampleFormat, u16, u32, u32), WavError> {
    let mut tag = le_u16(&payload[0..2]);
    let channels = le_u16(&payload[2..4]);
    let sample_rate = le_u32(&payload[4..8]);
    let block_align = le_u16(&payload[12..14]);
    let bits = le_u16(&payload[14..16]);
    let mut channel_mask = 0;
    if tag == WAVE_FORMAT_EXTENSIBLE {
        if payload.len() < 40 || le_u16(&payload[16..18]) < 22 {
            return Err(WavError::Malformed("extensible fmt chunk is too short"));
        }
        if le_u16(&payload[18..20]) != bits {
            return Err(WavError::UnsupportedFormat { tag, bits });
        }
        channel_mask = le_u32(&payload[20..24]);
        if payload[26..40] != SUBFORMAT_GUID_TAIL {
            return Err(WavError::UnsupportedFormat { tag, bits });
        }
        tag = le_u16(&payload[24..26]);
    }
    let format = match (tag, bits) {
        (WAVE_FORMAT_PCM, 16) => WavSampleFormat::Pcm16,
        (WAVE_FORMAT_PCM, 24) => WavSampleFormat::Pcm24,
        (WAVE_FORMAT_PCM, 32) => WavSampleFormat::Pcm32,
        (WAVE_FORMAT_IEEE_FLOAT, 32) => WavSampleFormat::Float32,
        _ => return Err(WavError::UnsupportedFormat { tag, bits }),
    };
    if channels == 0 || channels > MAX_WAV_CHANNELS {
        return Err(WavError::InvalidChannelCount { channels });
    }
    if sample_rate == 0 {
        return Err(WavError::ZeroSampleRate);
    }
    if u32::from(block_align) != u32::from(channels) * u32::from(format.bytes_per_sample()) {
        return Err(WavError::Malformed("block align does not match format"));
    }
    Ok((format, channels, sample_rate, channel_mask))
}

fn read_payload(source: &mut dyn WavSource, offset: u64, size: u32) -> Result<Vec<u8>, WavError> {
    let mut payload = vec![0; usize::try_from(size).map_err(|_| WavError::Truncated)?];
    source.seek(SeekFrom::Start(offset))?;
    source.read_exact(&mut payload)?;
    Ok(payload)
}

/// Outcome of [`recover_wav`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WavRecovery {
    /// Header after recovery.
    pub info: WavInfo,
    /// The declared sizes already matched the file; nothing was written.
    pub was_consistent: bool,
    /// Trailing bytes that did not form a whole frame and are now excluded.
    pub discarded_tail_bytes: u64,
    /// Length the file must be truncated (or was extended) to.
    pub file_len: u64,
}

/// Repairs an unfinalized WAV/RF64 file in place.
///
/// When the declared sizes do not describe the file, the data chunk is
/// assumed to run to end of file: its length is rounded down to whole frames,
/// a zero pad byte is written after odd-length data, and the RIFF, `ds64`,
/// `fact`, and `data` sizes are rewritten. Files that outgrow RIFF limits are
/// promoted to RF64 when a reserved `JUNK` chunk directly follows `WAVE`.
/// Callers holding a file must truncate it to [`WavRecovery::file_len`];
/// [`recover_wav_file`] does this.
///
/// # Errors
///
/// Returns header parse errors, [`WavError::Rf64Unavailable`], or I/O errors.
pub fn recover_wav(target: &mut dyn WavRecoveryTarget) -> Result<WavRecovery, WavError> {
    let file_len = target.seek(SeekFrom::End(0))?;
    let mut reader: &mut dyn WavRecoveryTarget = &mut *target;
    let (info, layout) = parse_header(&mut reader)?;
    let data_offset = info.data_offset;
    if file_len < data_offset {
        return Err(WavError::Truncated);
    }
    let declared_end = data_offset + info.data_bytes + (info.data_bytes & 1);
    let declared_ok = info.riff_size == file_len - 8
        && declared_end <= file_len
        && info
            .data_bytes
            .is_multiple_of(u64::from(info.spec.block_align()))
        && (info.rf64 || info.data_bytes < u64::from(u32::MAX));
    if declared_ok {
        return Ok(WavRecovery {
            info,
            was_consistent: true,
            discarded_tail_bytes: 0,
            file_len,
        });
    }

    let available = file_len - data_offset;
    let block = u64::from(info.spec.block_align());
    let data_bytes = available - available % block;
    let pad = data_bytes & 1;
    let new_len = data_offset + data_bytes + pad;
    let riff_size = new_len - 8;
    let frames = data_bytes / block;
    let rf64 = riff_size > MAX_RIFF_SIZE || data_bytes > MAX_RIFF_SIZE;

    if rf64 && layout.reserve_offset.is_none() {
        return Err(WavError::Rf64Unavailable);
    }
    if pad == 1 {
        target.seek(SeekFrom::Start(data_offset + data_bytes))?;
        target.write_all(&[0])?;
    }
    write_sizes(target, layout, rf64, riff_size, data_bytes, frames)?;
    target.flush()?;

    Ok(WavRecovery {
        info: WavInfo {
            rf64,
            riff_size,
            data_bytes,
            ..info
        },
        was_consistent: false,
        discarded_tail_bytes: available - data_bytes,
        file_len: new_len,
    })
}

/// Rewrites the RIFF, `ds64`/`JUNK`, `fact`, and `data` sizes of a parsed
/// header in `target`.
fn write_sizes<T: Write + Seek + ?Sized>(
    target: &mut T,
    layout: ParsedLayout,
    rf64: bool,
    riff_size: u64,
    data_bytes: u64,
    frames: u64,
) -> Result<(), WavError> {
    target.seek(SeekFrom::Start(0))?;
    target.write_all(if rf64 { b"RF64" } else { b"RIFF" })?;
    target.write_all(&size32(riff_size, rf64).to_le_bytes())?;
    match (rf64, layout.reserve_offset) {
        (true, Some(reserve)) => {
            target.seek(SeekFrom::Start(reserve))?;
            target.write_all(b"ds64")?;
            target.seek(SeekFrom::Start(reserve + 8))?;
            target.write_all(&riff_size.to_le_bytes())?;
            target.write_all(&data_bytes.to_le_bytes())?;
            target.write_all(&frames.to_le_bytes())?;
            target.write_all(&0_u32.to_le_bytes())?;
        }
        (false, Some(reserve)) => {
            target.seek(SeekFrom::Start(reserve))?;
            target.write_all(b"JUNK")?;
            target.seek(SeekFrom::Start(reserve + 8))?;
            target.write_all(&[0; DS64_PAYLOAD_BYTES as usize])?;
        }
        (_, None) => {}
    }
    if let Some(fact) = layout.fact_payload_offset {
        target.seek(SeekFrom::Start(fact))?;
        target.write_all(&size32(frames, rf64 || frames > MAX_RIFF_SIZE).to_le_bytes())?;
    }
    target.seek(SeekFrom::Start(layout.data_size_offset))?;
    target.write_all(&size32(data_bytes, rf64).to_le_bytes())?;
    Ok(())
}

/// Returns a copy of `header` (every byte before the sample data, ending with
/// the `data` chunk header) whose sizes describe `data_bytes` of samples plus
/// the RIFF pad byte an odd length needs. Placeholder sizes from a streaming
/// muxer are replaced; RF64 is used when the sizes outgrow RIFF and a
/// `JUNK`/`ds64` reservation exists.
///
/// # Errors
///
/// Header parse errors, [`WavError::Malformed`] when `header` does not end
/// exactly at the sample data or `data_bytes` is not whole frames, and
/// [`WavError::Rf64Unavailable`].
pub(crate) fn sized_wav_header(header: &[u8], data_bytes: u64) -> Result<Vec<u8>, WavError> {
    let mut cursor = io::Cursor::new(header.to_vec());
    let (info, layout) = parse_header(&mut cursor)?;
    if info.data_offset != header.len() as u64 {
        return Err(WavError::Malformed(
            "header does not end at the sample data",
        ));
    }
    let block = u64::from(info.spec.block_align());
    if !data_bytes.is_multiple_of(block) {
        return Err(WavError::Malformed("data is not a whole number of frames"));
    }
    let riff_size = (info.data_offset - 8)
        .saturating_add(data_bytes)
        .saturating_add(data_bytes & 1);
    let rf64 = riff_size > MAX_RIFF_SIZE || data_bytes > MAX_RIFF_SIZE;
    if rf64 && layout.reserve_offset.is_none() {
        return Err(WavError::Rf64Unavailable);
    }
    write_sizes(
        &mut cursor,
        layout,
        rf64,
        riff_size,
        data_bytes,
        data_bytes / block,
    )?;
    Ok(cursor.into_inner())
}

/// Opens `path`, runs [`recover_wav`], truncates to the recovered length, and
/// syncs the file.
///
/// # Errors
///
/// Same as [`recover_wav`], plus open/truncate/sync I/O errors.
pub fn recover_wav_file(path: &Path) -> Result<WavRecovery, WavError> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let recovery = recover_wav(&mut file)?;
    if !recovery.was_consistent {
        file.set_len(recovery.file_len)?;
        file.sync_all()?;
    }
    Ok(recovery)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    let mut array = [0; 8];
    array.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(array)
}

fn is_printable_ascii(byte: u8) -> bool {
    (0x20..=0x7E).contains(&byte)
}

fn valid_bext_stamp(value: &str, groups: &[usize]) -> bool {
    if value.is_empty() {
        return true;
    }
    let bytes = value.as_bytes();
    let expected: usize = groups.iter().sum::<usize>() + groups.len() - 1;
    if bytes.len() != expected {
        return false;
    }
    let mut position = 0;
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            if !b"-_:. ".contains(&bytes[position]) {
                return false;
            }
            position += 1;
        }
        if !bytes[position..position + group]
            .iter()
            .all(u8::is_ascii_digit)
        {
            return false;
        }
        position += group;
    }
    true
}

fn put_fixed_ascii(out: &mut Vec<u8>, value: &str, width: usize) {
    out.extend_from_slice(value.as_bytes());
    out.resize(out.len() + (width - value.len()), 0);
}

fn fixed_ascii(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}
