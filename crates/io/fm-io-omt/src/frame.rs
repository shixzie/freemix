//! OMT 1.0 frame headers, codecs, and bounded frame encoding.

use crate::OmtError;

/// The only OMT protocol version this implementation speaks.
pub const OMT_VERSION: u8 = 1;
/// Bytes of every OMT frame header.
pub const FRAME_HEADER_BYTES: usize = 16;
/// Bytes of the video extended header.
pub const VIDEO_EXTENDED_HEADER_BYTES: usize = 32;
/// Bytes of the audio extended header.
pub const AUDIO_EXTENDED_HEADER_BYTES: usize = 24;
/// Most per-frame metadata bytes the header can count.
pub const MAX_METADATA_BYTES: usize = u16::MAX as usize;
/// Largest accepted frame payload (16 MiB), a bound no real OMT frame reaches.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The only audio codec OMT defines: 32-bit float planar.
const FPA1: u32 = 0x3141_5046;
/// Most audio channels OMT allows.
pub const MAX_AUDIO_CHANNELS: usize = 32;

/// The frame type byte: metadata, video, or audio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    Metadata,
    Video,
    Audio,
}

impl FrameKind {
    /// The frame type byte.
    #[must_use]
    pub const fn token(self) -> u8 {
        match self {
            Self::Metadata => 1,
            Self::Video => 2,
            Self::Audio => 4,
        }
    }

    /// Parses a frame type byte.
    #[must_use]
    pub const fn from_token(token: u8) -> Option<Self> {
        match token {
            1 => Some(Self::Metadata),
            2 => Some(Self::Video),
            4 => Some(Self::Audio),
            _ => None,
        }
    }
}

/// The video codecs OMT 1.0 defines, by `FourCC`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoCodec {
    /// VMX compressed video.
    Vmx1,
    /// Packed 8-bit YUV, U first.
    Uyvy,
    /// Packed 8-bit YUV, Y first.
    Yuy2,
    /// 32-bit BGRA (BGRX without the alpha flag).
    Bgra,
    /// Planar 4:2:0 YUV, Y then interleaved UV.
    Nv12,
    /// Planar 4:2:0 YUV, Y then U then V.
    Yv12,
    /// Packed 8-bit YUV followed by an alpha plane.
    Uyva,
    /// Planar 16-bit 4:2:2 YUV.
    P216,
    /// P216 followed by a 16-bit alpha plane.
    Pa16,
}

impl VideoCodec {
    /// Every codec, in token order.
    pub const ALL: [Self; 9] = [
        Self::Vmx1,
        Self::Uyvy,
        Self::Yuy2,
        Self::Bgra,
        Self::Nv12,
        Self::Yv12,
        Self::Uyva,
        Self::P216,
        Self::Pa16,
    ];

    /// The codec's `FourCC` as a little-endian integer.
    #[must_use]
    pub const fn token(self) -> u32 {
        match self {
            Self::Vmx1 => 0x3158_4D56,
            Self::Uyvy => 0x5956_5955,
            Self::Yuy2 => 0x3259_5559,
            Self::Bgra => 0x4152_4742,
            Self::Nv12 => 0x3231_564E,
            Self::Yv12 => 0x3231_5659,
            Self::Uyva => 0x4156_5955,
            Self::P216 => 0x3631_3250,
            Self::Pa16 => 0x3631_4150,
        }
    }

    /// Parses a codec `FourCC`.
    #[must_use]
    pub fn from_token(token: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|codec| codec.token() == token)
    }

    /// Whether the frame data is compressed rather than raw pixels.
    #[must_use]
    pub const fn is_compressed(self) -> bool {
        matches!(self, Self::Vmx1)
    }

    /// Whether the codec carries an alpha channel.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra | Self::Uyva | Self::Pa16)
    }

    /// The exact data bytes one raw frame of `width` by `height` needs, or
    /// `None` for a compressed codec or dimensions it cannot hold.
    #[must_use]
    pub fn data_len(self, width: u32, height: u32) -> Option<usize> {
        if width == 0 || height == 0 {
            return None;
        }
        let pixels = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?;
        match self {
            Self::Vmx1 => None,
            Self::Uyvy | Self::Yuy2 => pixels.checked_mul(2),
            Self::Bgra | Self::P216 => pixels.checked_mul(4),
            Self::Nv12 | Self::Yv12 => {
                if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                    return None;
                }
                pixels.checked_mul(3)?.checked_div(2)
            }
            Self::Uyva => pixels.checked_mul(3),
            Self::Pa16 => pixels.checked_mul(6),
        }
    }
}

/// The video flags `OMTVideoFlags` defines.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoFlags(u32);

impl VideoFlags {
    /// No flag.
    pub const NONE: Self = Self(0);
    /// Interlaced fields.
    pub const INTERLACED: Self = Self(1);
    /// The frame carries alpha.
    pub const ALPHA: Self = Self(2);
    /// Alpha is premultiplied.
    pub const PRE_MULTIPLIED: Self = Self(4);
    /// A one-eighth preview frame.
    pub const PREVIEW: Self = Self(8);
    /// A 16-bit high-bit-depth frame.
    pub const HIGH_BIT_DEPTH: Self = Self(16);

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// A flags value from raw bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Whether every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for VideoFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The video colour space OMT names.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorSpace {
    /// Undefined: senders and receivers assume BT.601 below 720 lines.
    #[default]
    Undefined,
    /// BT.601.
    Bt601,
    /// BT.709.
    Bt709,
}

impl ColorSpace {
    /// The wire value.
    #[must_use]
    pub const fn token(self) -> u32 {
        match self {
            Self::Undefined => 0,
            Self::Bt601 => 601,
            Self::Bt709 => 709,
        }
    }

    /// Parses a wire value.
    #[must_use]
    pub const fn from_token(token: u32) -> Option<Self> {
        match token {
            0 => Some(Self::Undefined),
            601 => Some(Self::Bt601),
            709 => Some(Self::Bt709),
            _ => None,
        }
    }

    /// The colour space a frame of `height` lines gets when it names none.
    #[must_use]
    pub const fn for_height(height: u32) -> Self {
        if height < 720 {
            Self::Bt601
        } else {
            Self::Bt709
        }
    }
}

/// One uncompressed (or VMX-compressed) video frame.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoFrame {
    /// Timestamp where one second is 10,000,000 ticks.
    pub timestamp: i64,
    /// The codec `FourCC`.
    pub codec: VideoCodec,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Frame-rate numerator and denominator.
    pub frame_rate: (u32, u32),
    /// Display aspect ratio.
    pub aspect_ratio: f32,
    /// Video flags.
    pub flags: VideoFlags,
    /// Colour space.
    pub color_space: ColorSpace,
    /// Raw pixel (or compressed) data.
    pub data: Vec<u8>,
    /// Per-frame metadata: UTF-8, null-terminated, or empty.
    pub metadata: Vec<u8>,
}

impl VideoFrame {
    /// A raw frame at `codec`, `width` by `height`, `frame_rate` frames per
    /// second, at a 1:1 display aspect ratio and the default colour space.
    ///
    /// # Errors
    ///
    /// `data` must hold exactly the bytes the codec needs for those
    /// dimensions.
    pub fn new(
        codec: VideoCodec,
        width: u32,
        height: u32,
        frame_rate: (u32, u32),
        data: Vec<u8>,
    ) -> Result<Self, OmtError> {
        let frame = Self {
            timestamp: 0,
            codec,
            width,
            height,
            frame_rate,
            aspect_ratio: aspect(width, height),
            flags: VideoFlags::NONE,
            color_space: ColorSpace::for_height(height),
            data,
            metadata: Vec::new(),
        };
        frame.validate()?;
        Ok(frame)
    }

    /// The same frame with `timestamp`.
    #[must_use]
    pub const fn at(mut self, timestamp: i64) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// The same frame with `flags`.
    #[must_use]
    pub const fn with_flags(mut self, flags: VideoFlags) -> Self {
        self.flags = flags;
        self
    }

    /// The same frame with the UTF-8 `xml` as its per-frame metadata.
    #[must_use]
    pub fn with_metadata(mut self, xml: &str) -> Self {
        self.metadata = null_terminated(xml);
        self
    }

    /// Checks every field the wire requires.
    ///
    /// # Errors
    ///
    /// Dimensions, rate, aspect ratio, codec data length, flags, metadata,
    /// and the total payload bound are all checked.
    pub fn validate(&self) -> Result<(), OmtError> {
        if self.width == 0 || self.height == 0 {
            return Err(OmtError::Video("dimensions must be nonzero"));
        }
        if self.width > i32::MAX as u32 || self.height > i32::MAX as u32 {
            return Err(OmtError::Video("dimensions do not fit the header"));
        }
        if self.frame_rate.0 == 0
            || self.frame_rate.1 == 0
            || self.frame_rate.0 > i32::MAX as u32
            || self.frame_rate.1 > i32::MAX as u32
        {
            return Err(OmtError::Video("the frame rate must fit the header"));
        }
        if !self.aspect_ratio.is_finite() || self.aspect_ratio <= 0.0 {
            return Err(OmtError::Video("the aspect ratio must be positive"));
        }
        if self.codec.is_compressed() {
            if self.data.is_empty() {
                return Err(OmtError::Video("a compressed frame needs data"));
            }
        } else {
            let expected = self
                .codec
                .data_len(self.width, self.height)
                .ok_or(OmtError::Video("the codec cannot hold these dimensions"))?;
            if self.data.len() != expected {
                return Err(OmtError::Video("the pixel data length is wrong"));
            }
        }
        let flags = self.flags;
        if flags.contains(VideoFlags::ALPHA) && !self.codec.has_alpha() {
            return Err(OmtError::Video("the codec has no alpha channel"));
        }
        if flags.contains(VideoFlags::PRE_MULTIPLIED) && !flags.contains(VideoFlags::ALPHA) {
            return Err(OmtError::Video("premultiplied alpha needs the alpha flag"));
        }
        if flags.contains(VideoFlags::HIGH_BIT_DEPTH)
            && !matches!(
                self.codec,
                VideoCodec::P216 | VideoCodec::Pa16 | VideoCodec::Vmx1
            )
        {
            return Err(OmtError::Video("the codec is not high bit depth"));
        }
        check_metadata(&self.metadata)?;
        self.encoded_len().map(|_| ())
    }

    fn encoded_len(&self) -> Result<usize, OmtError> {
        let length = VIDEO_EXTENDED_HEADER_BYTES
            .checked_add(self.data.len())
            .and_then(|length| length.checked_add(self.metadata.len()))
            .ok_or(OmtError::Length)?;
        if length > MAX_FRAME_BYTES {
            return Err(OmtError::Length);
        }
        Ok(length)
    }
}

/// One planar 32-bit float audio frame (`FPA1`).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFrame {
    /// Timestamp where one second is 10,000,000 ticks.
    pub timestamp: i64,
    /// The sample rate in hertz.
    pub sample_rate: u32,
    /// One plane per channel, each `samples_per_channel` long.
    pub channels: Vec<Vec<f32>>,
    /// Per-frame metadata: UTF-8, null-terminated, or empty.
    pub metadata: Vec<u8>,
}

impl AudioFrame {
    /// A silent-metadata frame of `sample_rate` and planar `channels`.
    ///
    /// # Errors
    ///
    /// There must be 1 to 32 channels of one nonzero length each.
    pub fn new(sample_rate: u32, channels: Vec<Vec<f32>>) -> Result<Self, OmtError> {
        let frame = Self {
            timestamp: 0,
            sample_rate,
            channels,
            metadata: Vec::new(),
        };
        frame.validate()?;
        Ok(frame)
    }

    /// The same frame with `timestamp`.
    #[must_use]
    pub const fn at(mut self, timestamp: i64) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// The same frame with the UTF-8 `xml` as its per-frame metadata.
    #[must_use]
    pub fn with_metadata(mut self, xml: &str) -> Self {
        self.metadata = null_terminated(xml);
        self
    }

    /// Samples per channel.
    #[must_use]
    pub fn samples_per_channel(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// The bit field of channels that are not entirely silent.
    #[must_use]
    pub fn active_channels(&self) -> u32 {
        self.channels
            .iter()
            .enumerate()
            .fold(0_u32, |mask, (index, plane)| {
                if plane.iter().any(|sample| *sample != 0.0) {
                    mask | (1 << index)
                } else {
                    mask
                }
            })
    }

    /// Checks every field the wire requires.
    ///
    /// # Errors
    ///
    /// The channel count, sample rate, planarity, and payload bound are
    /// checked.
    pub fn validate(&self) -> Result<(), OmtError> {
        let channels = self.channels.len();
        if channels == 0 || channels > MAX_AUDIO_CHANNELS {
            return Err(OmtError::Audio("there must be 1 to 32 channels"));
        }
        if self.sample_rate == 0 {
            return Err(OmtError::Audio("the sample rate must be nonzero"));
        }
        let samples = self.samples_per_channel();
        if samples == 0 || self.channels.iter().any(|plane| plane.len() != samples) {
            return Err(OmtError::Audio("every channel must hold the same samples"));
        }
        if samples > i32::MAX as usize || channels > i32::MAX as usize {
            return Err(OmtError::Audio("the frame is too large"));
        }
        let bytes = samples
            .checked_mul(channels)
            .and_then(|samples| samples.checked_mul(4))
            .ok_or(OmtError::Length)?;
        if AUDIO_EXTENDED_HEADER_BYTES
            .checked_add(bytes)
            .and_then(|length| length.checked_add(self.metadata.len()))
            .is_none_or(|length| length > MAX_FRAME_BYTES)
        {
            return Err(OmtError::Length);
        }
        check_metadata(&self.metadata)
    }
}

/// One metadata frame: UTF-8 XML (per-frame metadata or a control command).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataFrame {
    /// Timestamp where one second is 10,000,000 ticks.
    pub timestamp: i64,
    /// The XML text, without its terminating null.
    pub xml: String,
}

impl MetadataFrame {
    /// A metadata frame carrying `xml`.
    ///
    /// # Errors
    ///
    /// The text must be nonempty, fit the header's count, and hold no
    /// interior null.
    pub fn new(xml: impl Into<String>) -> Result<Self, OmtError> {
        let frame = Self {
            timestamp: 0,
            xml: xml.into(),
        };
        frame.validate()?;
        Ok(frame)
    }

    /// The same frame with `timestamp`.
    #[must_use]
    pub const fn at(mut self, timestamp: i64) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Checks the text the wire requires.
    ///
    /// # Errors
    ///
    /// Empty, overlong, or interior-null text is refused.
    pub fn validate(&self) -> Result<(), OmtError> {
        if self.xml.is_empty() {
            return Err(OmtError::Metadata("the text is empty"));
        }
        if self.xml.len() >= MAX_METADATA_BYTES {
            return Err(OmtError::Metadata("the text is too long"));
        }
        if self.xml.contains('\0') {
            return Err(OmtError::Metadata("the text holds a null"));
        }
        Ok(())
    }
}

/// One decoded OMT frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A metadata frame.
    Metadata(MetadataFrame),
    /// A video frame.
    Video(VideoFrame),
    /// An audio frame.
    Audio(AudioFrame),
}

impl Frame {
    /// The frame's type.
    #[must_use]
    pub const fn kind(&self) -> FrameKind {
        match self {
            Self::Metadata(_) => FrameKind::Metadata,
            Self::Video(_) => FrameKind::Video,
            Self::Audio(_) => FrameKind::Audio,
        }
    }

    /// The frame's timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> i64 {
        match self {
            Self::Metadata(frame) => frame.timestamp,
            Self::Video(frame) => frame.timestamp,
            Self::Audio(frame) => frame.timestamp,
        }
    }

    /// Encodes the frame: header, extended header, data, and metadata.
    ///
    /// # Errors
    ///
    /// Every field is validated and the payload is bounded before encoding.
    pub fn encode(&self) -> Result<Vec<u8>, OmtError> {
        match self {
            Self::Metadata(frame) => {
                frame.validate()?;
                let data = null_terminated(&frame.xml);
                encode(
                    &Header {
                        kind: FrameKind::Metadata,
                        timestamp: frame.timestamp,
                        metadata_length: 0,
                        data_length: i32::try_from(data.len()).map_err(|_| OmtError::Length)?,
                    },
                    &[&data],
                )
            }
            Self::Video(frame) => {
                frame.validate()?;
                let mut extended = Vec::with_capacity(VIDEO_EXTENDED_HEADER_BYTES);
                extended.extend_from_slice(&frame.codec.token().to_le_bytes());
                extended.extend_from_slice(&frame.width.cast_signed().to_le_bytes());
                extended.extend_from_slice(&frame.height.cast_signed().to_le_bytes());
                extended.extend_from_slice(&frame.frame_rate.0.cast_signed().to_le_bytes());
                extended.extend_from_slice(&frame.frame_rate.1.cast_signed().to_le_bytes());
                extended.extend_from_slice(&frame.aspect_ratio.to_le_bytes());
                extended.extend_from_slice(&frame.flags.bits().cast_signed().to_le_bytes());
                extended.extend_from_slice(&frame.color_space.token().cast_signed().to_le_bytes());
                let metadata_length =
                    u16::try_from(frame.metadata.len()).map_err(|_| OmtError::Length)?;
                let data_length =
                    i32::try_from(frame.encoded_len()?).map_err(|_| OmtError::Length)?;
                encode(
                    &Header {
                        kind: FrameKind::Video,
                        timestamp: frame.timestamp,
                        metadata_length,
                        data_length,
                    },
                    &[&extended, &frame.data, &frame.metadata],
                )
            }
            Self::Audio(frame) => {
                frame.validate()?;
                let samples =
                    i32::try_from(frame.samples_per_channel()).map_err(|_| OmtError::Length)?;
                let channels = i32::try_from(frame.channels.len()).map_err(|_| OmtError::Length)?;
                let mut extended = Vec::with_capacity(AUDIO_EXTENDED_HEADER_BYTES);
                extended.extend_from_slice(&FPA1.to_le_bytes());
                extended.extend_from_slice(
                    &i32::try_from(frame.sample_rate)
                        .map_err(|_| OmtError::Length)?
                        .to_le_bytes(),
                );
                extended.extend_from_slice(&samples.to_le_bytes());
                extended.extend_from_slice(&channels.to_le_bytes());
                extended.extend_from_slice(&frame.active_channels().to_le_bytes());
                extended.extend_from_slice(&0_i32.to_le_bytes());
                let mut data =
                    Vec::with_capacity(frame.samples_per_channel() * frame.channels.len() * 4);
                for plane in &frame.channels {
                    for sample in plane {
                        data.extend_from_slice(&sample.to_le_bytes());
                    }
                }
                let metadata_length =
                    u16::try_from(frame.metadata.len()).map_err(|_| OmtError::Length)?;
                let data_length =
                    i32::try_from(AUDIO_EXTENDED_HEADER_BYTES + data.len() + frame.metadata.len())
                        .map_err(|_| OmtError::Length)?;
                encode(
                    &Header {
                        kind: FrameKind::Audio,
                        timestamp: frame.timestamp,
                        metadata_length,
                        data_length,
                    },
                    &[&extended, &data, &frame.metadata],
                )
            }
        }
    }

    /// Decodes one complete frame, header included.
    ///
    /// # Errors
    ///
    /// The version, type, lengths, extended headers, and every field are
    /// validated before any data is returned.
    pub fn decode(bytes: &[u8]) -> Result<Self, OmtError> {
        let header = Header::decode(bytes.get(..FRAME_HEADER_BYTES).ok_or(OmtError::Length)?)?;
        let payload = bytes.get(FRAME_HEADER_BYTES..).ok_or(OmtError::Length)?;
        if payload.len() != usize::try_from(header.data_length).map_err(|_| OmtError::Length)? {
            return Err(OmtError::Length);
        }
        match header.kind {
            FrameKind::Metadata => decode_metadata(header, payload),
            FrameKind::Video => decode_video(header, payload),
            FrameKind::Audio => decode_audio(header, payload),
        }
    }
}

/// The length one complete frame occupies on the wire, from its 16-byte
/// header alone.
///
/// # Errors
///
/// The header's version, type, and payload length are checked.
pub fn frame_len(header: &[u8]) -> Result<usize, OmtError> {
    let header = Header::decode(header)?;
    usize::try_from(header.data_length)
        .ok()
        .and_then(|length| FRAME_HEADER_BYTES.checked_add(length))
        .ok_or(OmtError::Length)
}

#[derive(Clone, Copy, Debug)]
struct Header {
    kind: FrameKind,
    timestamp: i64,
    metadata_length: u16,
    data_length: i32,
}

impl Header {
    fn decode(bytes: &[u8]) -> Result<Self, OmtError> {
        if bytes.len() < FRAME_HEADER_BYTES {
            return Err(OmtError::Length);
        }
        if bytes[0] != OMT_VERSION {
            return Err(OmtError::Version(bytes[0]));
        }
        let kind = FrameKind::from_token(bytes[1]).ok_or(OmtError::FrameKind(bytes[1]))?;
        let timestamp = i64::from_le_bytes(bytes[2..10].try_into().expect("eight bytes"));
        let metadata_length = u16::from_le_bytes(bytes[10..12].try_into().expect("two bytes"));
        let data_length = i32::from_le_bytes(bytes[12..16].try_into().expect("four bytes"));
        if data_length < 0
            || usize::try_from(data_length).map_or(true, |length| length > MAX_FRAME_BYTES)
        {
            return Err(OmtError::Length);
        }
        Ok(Self {
            kind,
            timestamp,
            metadata_length,
            data_length,
        })
    }
}

fn encode(header: &Header, parts: &[&[u8]]) -> Result<Vec<u8>, OmtError> {
    let length = parts
        .iter()
        .try_fold(0_usize, |length, part| length.checked_add(part.len()))
        .ok_or(OmtError::Length)?;
    if length != usize::try_from(header.data_length).map_err(|_| OmtError::Length)? {
        return Err(OmtError::Length);
    }
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + length);
    out.push(OMT_VERSION);
    out.push(header.kind.token());
    out.extend_from_slice(&header.timestamp.to_le_bytes());
    out.extend_from_slice(&header.metadata_length.to_le_bytes());
    out.extend_from_slice(&header.data_length.to_le_bytes());
    for part in parts {
        out.extend_from_slice(part);
    }
    Ok(out)
}

fn decode_metadata(header: Header, payload: &[u8]) -> Result<Frame, OmtError> {
    if header.metadata_length != 0 {
        return Err(OmtError::Metadata(
            "a metadata frame carries no metadata length",
        ));
    }
    let xml = decode_null_terminated(payload).ok_or(OmtError::Metadata(
        "a metadata frame must end with one null",
    ))?;
    let frame = MetadataFrame {
        timestamp: header.timestamp,
        xml,
    };
    frame.validate()?;
    Ok(Frame::Metadata(frame))
}

fn decode_video(header: Header, payload: &[u8]) -> Result<Frame, OmtError> {
    let extended = payload
        .get(..VIDEO_EXTENDED_HEADER_BYTES)
        .ok_or(OmtError::Video("the extended header is short"))?;
    let read_i32 = |at: usize| i32::from_le_bytes(extended[at..at + 4].try_into().expect("four"));
    let word = |at: usize| u32::from_le_bytes(extended[at..at + 4].try_into().expect("four"));
    let codec = VideoCodec::from_token(word(0)).ok_or(OmtError::Codec(word(0)))?;
    let width = u32::try_from(read_i32(4)).map_err(|_| OmtError::Video("negative width"))?;
    let height = u32::try_from(read_i32(8)).map_err(|_| OmtError::Video("negative height"))?;
    let frame_rate = (
        u32::try_from(read_i32(12)).map_err(|_| OmtError::Video("negative rate"))?,
        u32::try_from(read_i32(16)).map_err(|_| OmtError::Video("negative rate"))?,
    );
    let aspect_ratio = f32::from_le_bytes(extended[20..24].try_into().expect("four"));
    let flags = VideoFlags::from_bits(word(24));
    let color_space =
        ColorSpace::from_token(word(28)).ok_or(OmtError::Video("unknown colour space"))?;
    let metadata_length = usize::from(header.metadata_length);
    let body = payload
        .get(VIDEO_EXTENDED_HEADER_BYTES..)
        .ok_or(OmtError::Length)?;
    let data_length = body
        .len()
        .checked_sub(metadata_length)
        .ok_or(OmtError::Length)?;
    let (data, metadata) = body.split_at(data_length);
    let frame = VideoFrame {
        timestamp: header.timestamp,
        codec,
        width,
        height,
        frame_rate,
        aspect_ratio,
        flags,
        color_space,
        data: data.to_vec(),
        metadata: metadata.to_vec(),
    };
    frame.validate()?;
    Ok(Frame::Video(frame))
}

fn decode_audio(header: Header, payload: &[u8]) -> Result<Frame, OmtError> {
    let extended = payload
        .get(..AUDIO_EXTENDED_HEADER_BYTES)
        .ok_or(OmtError::Audio("the extended header is short"))?;
    let read_i32 = |at: usize| i32::from_le_bytes(extended[at..at + 4].try_into().expect("four"));
    let word = |at: usize| u32::from_le_bytes(extended[at..at + 4].try_into().expect("four"));
    let codec = word(0);
    if codec != FPA1 {
        return Err(OmtError::Codec(codec));
    }
    let sample_rate = u32::try_from(read_i32(4)).map_err(|_| OmtError::Audio("negative rate"))?;
    let samples = u32::try_from(read_i32(8)).map_err(|_| OmtError::Audio("negative samples"))?;
    let channels = u32::try_from(read_i32(12)).map_err(|_| OmtError::Audio("negative channels"))?;
    let active = word(16);
    let metadata_length = usize::from(header.metadata_length);
    let body = payload
        .get(AUDIO_EXTENDED_HEADER_BYTES..)
        .ok_or(OmtError::Length)?;
    let data_length = body
        .len()
        .checked_sub(metadata_length)
        .ok_or(OmtError::Length)?;
    let (data, metadata) = body.split_at(data_length);
    let channels = usize::try_from(channels).map_err(|_| OmtError::Audio("too many channels"))?;
    let samples = usize::try_from(samples).map_err(|_| OmtError::Audio("too many samples"))?;
    if channels == 0 || channels > MAX_AUDIO_CHANNELS {
        return Err(OmtError::Audio("there must be 1 to 32 channels"));
    }
    if active & !channel_mask(channels) != 0 {
        return Err(OmtError::Audio("the active channel mask is out of range"));
    }
    let expected = channels
        .checked_mul(samples)
        .and_then(|count| count.checked_mul(4))
        .ok_or(OmtError::Length)?;
    if data.len() != expected {
        return Err(OmtError::Audio("the sample data length is wrong"));
    }
    let mut planes = Vec::with_capacity(channels);
    for plane in 0..channels {
        let start = plane * samples * 4;
        let mut samples_out = Vec::with_capacity(samples);
        for sample in 0..samples {
            let at = start + sample * 4;
            samples_out.push(f32::from_le_bytes(
                data[at..at + 4].try_into().expect("four bytes"),
            ));
        }
        planes.push(samples_out);
    }
    let frame = AudioFrame {
        timestamp: header.timestamp,
        sample_rate,
        channels: planes,
        metadata: metadata.to_vec(),
    };
    frame.validate()?;
    Ok(Frame::Audio(frame))
}

const fn channel_mask(channels: usize) -> u32 {
    if channels >= 32 {
        u32::MAX
    } else {
        (1 << channels) - 1
    }
}

fn aspect(width: u32, height: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // dimensions are bounded by the frame bound
    {
        width as f32 / height as f32
    }
}

fn null_terminated(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    bytes
}

fn decode_null_terminated(bytes: &[u8]) -> Option<String> {
    let text = bytes.strip_suffix(&[0])?;
    if text.contains(&0) {
        return None;
    }
    String::from_utf8(text.to_vec()).ok()
}

fn check_metadata(metadata: &[u8]) -> Result<(), OmtError> {
    if metadata.is_empty() {
        return Ok(());
    }
    if metadata.len() > MAX_METADATA_BYTES || metadata.last() != Some(&0) {
        return Err(OmtError::Metadata("the metadata must be null-terminated"));
    }
    let text = &metadata[..metadata.len() - 1];
    if text.contains(&0) || core::str::from_utf8(text).is_err() {
        return Err(OmtError::Metadata("the metadata is not one UTF-8 string"));
    }
    Ok(())
}
