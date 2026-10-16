//! Local capture input value types (IN-001).
//!
//! A capture input acquires video (and optionally audio) from a local
//! camera or capture card named by an adapter-qualified stable key (on
//! Linux, the V4L2 bus and card; see `fm-io-linux`). Its
//! [`CaptureSettings`] choose the device format (picture size, frame rate,
//! pixel format, and progressive or interlaced scan, or `None` to let the
//! runtime pick the device format nearest the program), the audio the input
//! hears ([`CaptureAudio`]), and what it shows while the signal is lost
//! ([`CaptureLossSlate`]).
//!
//! The runtime reports each capture input's [`CaptureStatus`]: its
//! [`CaptureSignalState`], why the signal is lost
//! ([`CaptureLossReason`]), the device node and the format it negotiated,
//! and loss, recovery, and renegotiation counts. A lost device is
//! reacquired under the same input ID when it returns.
//!
//! [`CaptureInventory`] lists the capture devices a daemon can open, each
//! with its formats, bounded for the wire.
//!
//! [`negotiate`] picks the device format an input opens: an exact match of
//! the requested format when the device offers it, otherwise the nearest
//! offered format (same scan first, then picture size, frame rate, and
//! pixel format preference).

use std::fmt;

use crate::FrameRate;

/// The most capture inputs one project holds.
pub const MAX_CAPTURE_INPUTS: usize = 16;
/// The most devices a capture inventory lists.
pub const MAX_CAPTURE_DEVICES: usize = 16;
/// The most formats one inventory device lists.
pub const MAX_CAPTURE_DEVICE_FORMATS: usize = 128;
/// The longest device text (name, node path, driver, bus, ALSA device), in
/// bytes.
pub const MAX_CAPTURE_TEXT_BYTES: usize = 128;
/// The longest stable key of a capture device, in bytes.
pub const MAX_CAPTURE_KEY_BYTES: usize = 256;
/// The longest reported error text, in bytes.
pub const MAX_CAPTURE_ERROR_BYTES: usize = 160;
/// The longest ALSA device name an input may name, in bytes.
pub const MAX_ALSA_DEVICE_BYTES: usize = 64;
/// The smallest capture picture side, in pixels.
pub const MIN_CAPTURE_DIMENSION: u32 = 16;
/// The largest capture picture side, in pixels.
pub const MAX_CAPTURE_DIMENSION: u32 = 8_192;
/// The highest capture frame rate, in frames per second.
pub const MAX_CAPTURE_FPS: u32 = 240;

/// Why a capture value is invalid.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureError {
    /// The picture size is outside the capture bounds.
    Dimensions,
    /// The frame rate is above [`MAX_CAPTURE_FPS`].
    Rate,
    /// A format token is malformed.
    FormatToken,
    /// The ALSA device name is empty, too long, or not printable ASCII.
    AlsaDevice,
    /// An audio or loss-slate token is unknown.
    Token,
    /// The stable key is empty, too long, or holds control characters.
    StableKey,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Dimensions => "capture picture size is out of range",
            Self::Rate => "capture frame rate is out of range",
            Self::FormatToken => "capture format is malformed",
            Self::AlsaDevice => "ALSA device name is invalid",
            Self::Token => "capture setting is unknown",
            Self::StableKey => "capture device key is invalid",
        })
    }
}

impl std::error::Error for CaptureError {}

/// The pixel format a capture device delivers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapturePixelFormat {
    /// Packed 4:2:2 YUYV.
    Yuyv,
    /// Semi-planar 4:2:0 NV12.
    Nv12,
    /// Motion JPEG.
    Mjpeg,
}

impl CapturePixelFormat {
    pub const ALL: [Self; 3] = [Self::Yuyv, Self::Nv12, Self::Mjpeg];

    /// `yuyv`, `nv12`, or `mjpeg`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Yuyv => "yuyv",
            Self::Nv12 => "nv12",
            Self::Mjpeg => "mjpeg",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }

    /// The V4L2 four-character code.
    #[must_use]
    pub const fn fourcc(self) -> [u8; 4] {
        match self {
            Self::Yuyv => *b"YUYV",
            Self::Nv12 => *b"NV12",
            Self::Mjpeg => *b"MJPG",
        }
    }

    /// The format of a V4L2 four-character code, if it is one of these.
    #[must_use]
    pub fn from_fourcc(fourcc: [u8; 4]) -> Option<Self> {
        match &fourcc {
            b"YUYV" => Some(Self::Yuyv),
            b"NV12" => Some(Self::Nv12),
            b"MJPG" | b"JPEG" => Some(Self::Mjpeg),
            _ => None,
        }
    }

    /// Whether the format is compressed (needs a decoder).
    #[must_use]
    pub const fn is_compressed(self) -> bool {
        matches!(self, Self::Mjpeg)
    }

    /// The upper-case display label: `YUYV`, `NV12`, or `MJPEG`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Yuyv => "YUYV",
            Self::Nv12 => "NV12",
            Self::Mjpeg => "MJPEG",
        }
    }
}

/// Progressive or interlaced scan, with the field order.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CaptureScan {
    #[default]
    Progressive,
    /// Interlaced, top field first.
    InterlacedTff,
    /// Interlaced, bottom field first.
    InterlacedBff,
}

impl CaptureScan {
    pub const ALL: [Self; 3] = [Self::Progressive, Self::InterlacedTff, Self::InterlacedBff];

    /// `p`, `tff`, or `bff`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Progressive => "p",
            Self::InterlacedTff => "tff",
            Self::InterlacedBff => "bff",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }

    #[must_use]
    pub const fn is_interlaced(self) -> bool {
        !matches!(self, Self::Progressive)
    }
}

/// The broadcast resolution class of a picture height.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResolutionClass {
    /// Below 720 lines (480i/576i and smaller).
    Sd,
    /// 720 to 1439 lines (720p, 1080i/p).
    Hd,
    /// 1440 lines and more (2160p and up).
    Uhd,
}

impl ResolutionClass {
    #[must_use]
    pub const fn of_height(height: u32) -> Self {
        if height < 720 {
            Self::Sd
        } else if height < 1_440 {
            Self::Hd
        } else {
            Self::Uhd
        }
    }

    /// `SD`, `HD`, or `UHD`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sd => "SD",
            Self::Hd => "HD",
            Self::Uhd => "UHD",
        }
    }
}

/// One device format: picture size, frame rate, pixel format, and scan.
///
/// `rate` is the frame rate: an interlaced format at 30000/1001 delivers
/// 59.94 fields per second.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureFormat {
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub pixel_format: CapturePixelFormat,
    pub scan: CaptureScan,
}

impl CaptureFormat {
    /// Checks the size and rate bounds.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::Dimensions`] or [`CaptureError::Rate`].
    pub fn validate(&self) -> Result<(), CaptureError> {
        let side = MIN_CAPTURE_DIMENSION..=MAX_CAPTURE_DIMENSION;
        if !side.contains(&self.width) || !side.contains(&self.height) {
            return Err(CaptureError::Dimensions);
        }
        if u64::from(self.rate.numerator())
            > u64::from(MAX_CAPTURE_FPS) * u64::from(self.rate.denominator())
        {
            return Err(CaptureError::Rate);
        }
        Ok(())
    }

    /// The compact token `<w>x<h>@<num>/<den>:<pixel format>:<scan>`, e.g.
    /// `1920x1080@30000/1001:yuyv:tff`.
    #[must_use]
    pub fn token(&self) -> String {
        format!(
            "{}x{}@{}/{}:{}:{}",
            self.width,
            self.height,
            self.rate.numerator(),
            self.rate.denominator(),
            self.pixel_format.token(),
            self.scan.token()
        )
    }

    /// Parses [`Self::token`] and validates the result.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::FormatToken`] for a malformed token, or the
    /// bound it breaks.
    pub fn from_token(token: &str) -> Result<Self, CaptureError> {
        let malformed = CaptureError::FormatToken;
        let mut parts = token.split(':');
        let (Some(mode), Some(pixel), Some(scan), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(malformed);
        };
        let (size, rate) = mode.split_once('@').ok_or(malformed)?;
        let (width, height) = size.split_once('x').ok_or(malformed)?;
        let (numerator, denominator) = rate.split_once('/').ok_or(malformed)?;
        let number = |text: &str| -> Result<u32, CaptureError> {
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(malformed);
            }
            text.parse().map_err(|_| malformed)
        };
        let numerator = number(numerator)?;
        let denominator = number(denominator)?;
        let rate = FrameRate::new(numerator, denominator).map_err(|_| malformed)?;
        if rate.numerator() != numerator || rate.denominator() != denominator {
            // Only the reduced form is canonical.
            return Err(malformed);
        }
        let format = Self {
            width: number(width)?,
            height: number(height)?,
            rate,
            pixel_format: CapturePixelFormat::from_token(pixel).ok_or(malformed)?,
            scan: CaptureScan::from_token(scan).ok_or(malformed)?,
        };
        format.validate()?;
        Ok(format)
    }

    /// The frame rate in millihertz, rounded down.
    #[must_use]
    pub fn millihertz(&self) -> u64 {
        u64::from(self.rate.numerator()) * 1_000 / u64::from(self.rate.denominator()).max(1)
    }

    /// A readable label such as `1920x1080 59.94i YUYV`: the rate is the
    /// field rate for interlaced formats, as broadcast names them.
    #[must_use]
    pub fn label(&self) -> String {
        let mut millihertz = self.millihertz();
        if self.scan.is_interlaced() {
            millihertz *= 2;
        }
        let rate = if millihertz.is_multiple_of(1_000) {
            (millihertz / 1_000).to_string()
        } else {
            let hundredths = (millihertz + 5) / 10;
            format!("{}.{:02}", hundredths / 100, hundredths % 100)
        };
        format!(
            "{}x{} {}{} {}",
            self.width,
            self.height,
            rate,
            if self.scan.is_interlaced() { 'i' } else { 'p' },
            self.pixel_format.label()
        )
    }

    /// The format's resolution class.
    #[must_use]
    pub const fn class(&self) -> ResolutionClass {
        ResolutionClass::of_height(self.height)
    }
}

/// What a capture input hears.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum CaptureAudio {
    /// Silence.
    None,
    /// The audio device paired with the video device (the same USB device
    /// or card), if it has one; silence otherwise.
    #[default]
    Paired,
    /// A named ALSA capture device such as `hw:CARD=C920,DEV=0`.
    Alsa(String),
}

impl CaptureAudio {
    /// `none`, `paired`, or `alsa:<device>`.
    #[must_use]
    pub fn token(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Paired => "paired".to_owned(),
            Self::Alsa(device) => format!("alsa:{device}"),
        }
    }

    /// Parses [`Self::token`] and validates the result.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::Token`] or [`CaptureError::AlsaDevice`].
    pub fn from_token(token: &str) -> Result<Self, CaptureError> {
        let audio = match token {
            "none" => Self::None,
            "paired" => Self::Paired,
            _ => Self::Alsa(
                token
                    .strip_prefix("alsa:")
                    .ok_or(CaptureError::Token)?
                    .to_owned(),
            ),
        };
        audio.validate()?;
        Ok(audio)
    }

    /// Checks the ALSA device name: 1 to [`MAX_ALSA_DEVICE_BYTES`] bytes of
    /// printable ASCII without spaces.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::AlsaDevice`].
    pub fn validate(&self) -> Result<(), CaptureError> {
        match self {
            Self::Alsa(device)
                if device.is_empty()
                    || device.len() > MAX_ALSA_DEVICE_BYTES
                    || !device.bytes().all(|byte| byte.is_ascii_graphic()) =>
            {
                Err(CaptureError::AlsaDevice)
            }
            _ => Ok(()),
        }
    }
}

/// What a capture input shows while its signal is lost.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CaptureLossSlate {
    /// Keep the last picture.
    #[default]
    Hold,
    /// Show opaque black.
    Black,
}

impl CaptureLossSlate {
    pub const ALL: [Self; 2] = [Self::Hold, Self::Black];

    /// `hold` or `black`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Black => "black",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// A capture input's settings.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CaptureSettings {
    /// The requested device format; `None` picks the device format nearest
    /// the program size and rate.
    pub format: Option<CaptureFormat>,
    pub audio: CaptureAudio,
    pub loss: CaptureLossSlate,
}

impl CaptureSettings {
    /// Checks the format and audio device.
    ///
    /// # Errors
    ///
    /// Returns the first invalid value's [`CaptureError`].
    pub fn validate(&self) -> Result<(), CaptureError> {
        if let Some(format) = &self.format {
            format.validate()?;
        }
        self.audio.validate()
    }
}

/// A capture input's device and settings: what adds a capture input.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CaptureInputConfig {
    /// The adapter-qualified stable key of the device.
    pub stable_key: String,
    pub settings: CaptureSettings,
}

impl CaptureInputConfig {
    /// A config for `stable_key` with default settings.
    #[must_use]
    pub fn new(stable_key: impl Into<String>) -> Self {
        Self {
            stable_key: stable_key.into(),
            settings: CaptureSettings::default(),
        }
    }

    /// Checks the stable key and settings.
    ///
    /// # Errors
    ///
    /// Returns the first invalid value's [`CaptureError`].
    pub fn validate(&self) -> Result<(), CaptureError> {
        validate_stable_key(&self.stable_key)?;
        self.settings.validate()
    }
}

/// Checks a capture device stable key: 1 to [`MAX_CAPTURE_KEY_BYTES`] bytes,
/// not blank, without control characters.
///
/// # Errors
///
/// Returns [`CaptureError::StableKey`].
pub fn validate_stable_key(key: &str) -> Result<(), CaptureError> {
    if key.trim().is_empty()
        || key.len() > MAX_CAPTURE_KEY_BYTES
        || key.chars().any(char::is_control)
    {
        return Err(CaptureError::StableKey);
    }
    Ok(())
}

/// A capture input's acquisition state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CaptureSignalState {
    /// Nothing acquires the device: the input is closed, or the daemon does
    /// not realize capture inputs.
    #[default]
    Idle,
    /// The device is opening; no picture yet.
    Starting,
    /// Pictures arrive.
    Live,
    /// The device is gone, delivers nothing, or reports no signal; the
    /// input shows its loss slate and reacquires the device when it returns.
    SignalLost,
    /// The device cannot be acquired at all (no capture backend, or no
    /// usable format).
    Failed,
}

impl CaptureSignalState {
    pub const ALL: [Self; 5] = [
        Self::Idle,
        Self::Starting,
        Self::Live,
        Self::SignalLost,
        Self::Failed,
    ];

    /// `idle`, `starting`, `live`, `signal_lost`, or `failed`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Live => "live",
            Self::SignalLost => "signal_lost",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Why a capture signal is lost.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureLossReason {
    /// The device is not present.
    Unplugged,
    /// The device delivers no pictures.
    Stalled,
    /// The device reports no input signal (a capture card without a
    /// source).
    NoSignal,
    /// Acquisition failed (the device is busy, or its format went away).
    Error,
}

impl CaptureLossReason {
    pub const ALL: [Self; 4] = [Self::Unplugged, Self::Stalled, Self::NoSignal, Self::Error];

    /// `unplugged`, `stalled`, `no_signal`, or `error`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Unplugged => "unplugged",
            Self::Stalled => "stalled",
            Self::NoSignal => "no_signal",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// A capture input's runtime status.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CaptureStatus {
    pub state: CaptureSignalState,
    /// Why the signal is lost, while it is.
    pub reason: Option<CaptureLossReason>,
    /// The device node acquired, while one is.
    pub device: Option<String>,
    /// The format negotiated with the device, while one is.
    pub format: Option<CaptureFormat>,
    /// Whether audio is acquired with the pictures.
    pub audio: bool,
    /// Pictures received since the input opened.
    pub frames: u64,
    /// Times the signal was lost.
    pub losses: u32,
    /// Times the signal came back after a loss.
    pub recoveries: u32,
    /// Times the device format changed and acquisition was renegotiated.
    pub renegotiations: u32,
    /// The last acquisition error, bounded to [`MAX_CAPTURE_ERROR_BYTES`].
    pub last_error: Option<String>,
}

/// One capture device a daemon can open.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CaptureDevice {
    /// The adapter-qualified stable key a capture input names.
    pub stable_key: String,
    /// The device name (the V4L2 card name).
    pub name: String,
    /// The device node, such as `/dev/video0`; it may change on replug.
    pub path: String,
    pub driver: String,
    /// The bus location, such as `usb-0000:00:14.0-1`.
    pub bus: String,
    /// The paired audio device, if any.
    pub audio: Option<String>,
    /// The formats the device offers, at most
    /// [`MAX_CAPTURE_DEVICE_FORMATS`].
    pub formats: Vec<CaptureFormat>,
}

/// The capture devices a daemon can open.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CaptureInventory {
    /// The capture backend (`v4l2`, `avfoundation`, `fake`), or `None` when
    /// this daemon realizes no capture inputs.
    pub backend: Option<String>,
    /// Why the last enumeration failed, if it did.
    pub error: Option<String>,
    /// Devices in stable-key order, at most [`MAX_CAPTURE_DEVICES`].
    pub devices: Vec<CaptureDevice>,
}

/// `text` with control characters replaced and bounded to `max` bytes on a
/// character boundary.
#[must_use]
pub fn bounded_capture_text(text: &str, max: usize) -> String {
    let mut text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned();
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The program a capture input is mixed into: the target of [`negotiate`]
/// when no format is requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureTarget {
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
}

/// Picks the device format to open from `offered`.
///
/// A `requested` format the device offers is used as is. Otherwise the
/// nearest offered format wins: the requested (or progressive) scan first,
/// then the smallest picture at least the target size (or, failing that,
/// the largest below it), then the rate nearest the target (at least it
/// before below it), then the requested pixel format, then YUYV, NV12, and
/// MJPEG. The target is the requested format's size and rate, or the
/// program's.
#[must_use]
pub fn negotiate(
    offered: &[CaptureFormat],
    requested: Option<&CaptureFormat>,
    program: CaptureTarget,
) -> Option<CaptureFormat> {
    if let Some(requested) = requested
        && offered.contains(requested)
    {
        return Some(*requested);
    }
    let target = requested.map_or(program, |format| CaptureTarget {
        width: format.width,
        height: format.height,
        rate: format.rate,
    });
    let scan = requested.map_or(CaptureScan::Progressive, |format| format.scan);
    let pixel = requested.map(|format| format.pixel_format);
    let area = |width: u32, height: u32| u64::from(width) * u64::from(height);
    let target_area = area(target.width, target.height);
    let target_rate = CaptureFormat {
        width: target.width,
        height: target.height,
        rate: target.rate,
        pixel_format: CapturePixelFormat::Yuyv,
        scan,
    }
    .millihertz();
    offered.iter().copied().min_by_key(|format| {
        let covers = format.width >= target.width && format.height >= target.height;
        let format_area = area(format.width, format.height);
        let size = if covers {
            (0_u8, format_area - target_area.min(format_area))
        } else {
            (1, target_area.saturating_sub(format_area))
        };
        let rate = format.millihertz();
        let rate = if rate >= target_rate {
            (0_u8, rate - target_rate)
        } else {
            (1, target_rate - rate)
        };
        let pixel_rank = if Some(format.pixel_format) == pixel {
            0_u8
        } else {
            1 + format.pixel_format as u8
        };
        (format.scan != scan, size, rate, pixel_rank, format.token())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(
        width: u32,
        height: u32,
        (numerator, denominator): (u32, u32),
        pixel_format: CapturePixelFormat,
        scan: CaptureScan,
    ) -> CaptureFormat {
        CaptureFormat {
            width,
            height,
            rate: FrameRate::new(numerator, denominator).unwrap(),
            pixel_format,
            scan,
        }
    }

    #[test]
    fn format_tokens_round_trip_and_reject_malformed_text() {
        let ntsc = format(
            720,
            480,
            (30_000, 1_001),
            CapturePixelFormat::Yuyv,
            CaptureScan::InterlacedBff,
        );
        assert_eq!(ntsc.token(), "720x480@30000/1001:yuyv:bff");
        assert_eq!(CaptureFormat::from_token(&ntsc.token()), Ok(ntsc));
        assert_eq!(ntsc.label(), "720x480 59.94i YUYV");
        assert_eq!(ntsc.class(), ResolutionClass::Sd);
        let uhd = format(
            3840,
            2160,
            (60, 1),
            CapturePixelFormat::Nv12,
            CaptureScan::Progressive,
        );
        assert_eq!(uhd.label(), "3840x2160 60p NV12");
        assert_eq!(uhd.class(), ResolutionClass::Uhd);
        for bad in [
            "",
            "1920x1080@30/1:yuyv",
            "1920x1080@30/1:yuyv:p:x",
            "1920x1080@60/2:yuyv:p",
            "1920x1080@30/0:yuyv:p",
            "1920x1080@+30/1:yuyv:p",
            "1920x@30/1:yuyv:p",
            "1920x1080@30/1:rgb:p",
            "1920x1080@30/1:yuyv:i",
            "8x8@30/1:yuyv:p",
            "1920x1080@241/1:yuyv:p",
        ] {
            assert!(CaptureFormat::from_token(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn audio_and_slate_tokens_round_trip() {
        for audio in [
            CaptureAudio::None,
            CaptureAudio::Paired,
            CaptureAudio::Alsa("hw:CARD=C920,DEV=0".into()),
        ] {
            assert_eq!(CaptureAudio::from_token(&audio.token()), Ok(audio));
        }
        assert_eq!(
            CaptureAudio::from_token("alsa:"),
            Err(CaptureError::AlsaDevice)
        );
        assert_eq!(
            CaptureAudio::from_token("alsa:hw 0"),
            Err(CaptureError::AlsaDevice)
        );
        assert_eq!(CaptureAudio::from_token("pulse"), Err(CaptureError::Token));
        assert_eq!(
            CaptureLossSlate::from_token("black"),
            Some(CaptureLossSlate::Black)
        );
        assert_eq!(validate_stable_key(" "), Err(CaptureError::StableKey));
        assert_eq!(validate_stable_key("v4l2:usb-1#Cam"), Ok(()));
    }

    #[test]
    fn negotiation_prefers_exact_then_scan_size_rate_and_pixel_format() {
        use CapturePixelFormat::{Mjpeg, Nv12, Yuyv};
        use CaptureScan::{InterlacedTff, Progressive};
        let offered = [
            format(640, 480, (30, 1), Yuyv, Progressive),
            format(1280, 720, (10, 1), Yuyv, Progressive),
            format(1280, 720, (30, 1), Mjpeg, Progressive),
            format(1920, 1080, (5, 1), Yuyv, Progressive),
            format(1920, 1080, (30, 1), Mjpeg, Progressive),
            format(1920, 1080, (30, 1), Nv12, Progressive),
            format(1920, 1080, (30_000, 1_001), Yuyv, InterlacedTff),
        ];
        let program = CaptureTarget {
            width: 1920,
            height: 1080,
            rate: FrameRate::new(30, 1).unwrap(),
        };
        // Program target: 1080p30, uncompressed NV12 before MJPEG.
        assert_eq!(negotiate(&offered, None, program), Some(offered[5]));
        // An exact request is honoured.
        assert_eq!(
            negotiate(&offered, Some(&offered[4]), program),
            Some(offered[4])
        );
        assert_eq!(
            negotiate(&offered, Some(&offered[6]), program),
            Some(offered[6])
        );
        // A request the device lacks: nearest size and rate, requested
        // pixel format first.
        let wanted = format(1280, 720, (25, 1), Mjpeg, Progressive);
        assert_eq!(
            negotiate(&offered, Some(&wanted), program),
            Some(offered[2])
        );
        // A request for interlaced 1080 at another rate keeps interlaced.
        let wanted = format(1920, 1080, (25, 1), Yuyv, InterlacedTff);
        assert_eq!(
            negotiate(&offered, Some(&wanted), program),
            Some(offered[6])
        );
        // A small program picks the smallest covering picture.
        let small = CaptureTarget {
            width: 640,
            height: 360,
            rate: FrameRate::new(30, 1).unwrap(),
        };
        assert_eq!(negotiate(&offered, None, small), Some(offered[0]));
        // A program larger than anything: the largest at the best rate.
        let huge = CaptureTarget {
            width: 3840,
            height: 2160,
            rate: FrameRate::new(30, 1).unwrap(),
        };
        assert_eq!(negotiate(&offered, None, huge), Some(offered[5]));
        assert_eq!(negotiate(&[], None, program), None);
    }
}
