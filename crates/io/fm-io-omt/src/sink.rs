//! An `fm-io-api` output sink that publishes an OMT source.
//!
//! The sink takes CPU video frames (UYVY/YUV 4:2:2, BGRA, or NV12) and planar
//! audio, converts them to the OMT frame for the opened format, and hands
//! them to the [`Sender`]. Receivers subscribe over OMT; a sink with no
//! receivers accepts frames and sends them nowhere, exactly as a sender with
//! no subscribers does.

use std::net::SocketAddr;
use std::num::{NonZeroU128, NonZeroUsize};

use fm_capabilities::{FormatDescriptor, StableId};
use fm_frame::{AudioBlock, ClockDomainId, CpuVideoFrame, MediaTiming};
use fm_io_api::{
    DeviceId, DriverState, EndpointCapabilities, EndpointHealth, IoError, LifecycleState,
    MediaSink, MediaUnit, OpenOptions, PermissionState, SinkDescriptor, SinkId,
    TimestampCapabilities, TimestampQuality, TransferLimits, WriteError,
};
use fm_types::{MemoryDomain, PixelFormat};

use crate::{AudioFrame, OmtError, Sender, SenderInfo, VideoCodec, VideoFlags, VideoFrame};

/// The format kind an OMT sink advertises.
pub const OMT_FORMAT_KIND: &str = "video.omt";

/// What an [`OmtSink`] publishes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmtSinkConfig {
    pub id: SinkId,
    pub device_id: DeviceId,
    /// The machine name in `HOSTNAME (Source Name)`; empty disables DNS-SD.
    pub hostname: String,
    pub source: String,
    /// The TCP port; 0 picks an ephemeral one.
    pub port: u16,
    pub clock_domain: ClockDomainId,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: (u32, u32),
    pub info: Option<SenderInfo>,
}

/// One unit a sink can publish.
#[derive(Clone, Debug)]
pub enum OmtMedia {
    Video(CpuVideoFrame),
    Audio(AudioBlock),
}

impl MediaUnit for OmtMedia {
    fn timing(&self) -> MediaTiming {
        match self {
            Self::Video(frame) => frame.timing(),
            Self::Audio(block) => block.timing(),
        }
    }

    fn byte_len(&self) -> usize {
        match self {
            Self::Video(frame) => frame.payload().byte_len(),
            Self::Audio(block) => block
                .sample_count()
                .saturating_mul(block.planes().len())
                .saturating_mul(size_of::<f32>()),
        }
    }
}

/// An OMT output sink.
pub struct OmtSink {
    descriptor: SinkDescriptor,
    config: OmtSinkConfig,
    format: FormatDescriptor,
    lifecycle: LifecycleState,
    health: EndpointHealth,
    sender: Option<Sender>,
    advertised_port: Option<u16>,
    resume_running: bool,
    options: Option<OpenOptions>,
}

impl OmtSink {
    /// A closed sink for `config`, advertising one concrete video format.
    ///
    /// # Errors
    ///
    /// The format fields or the source name must be valid.
    pub fn new(config: OmtSinkConfig) -> Result<Self, IoError> {
        let format = format_descriptor(&config)?;
        let clock = fm_io_api::ClockCapability {
            domain: config.clock_domain,
            timestamps: TimestampCapabilities {
                quality: TimestampQuality::Monotonic,
                resolution_nanos: NonZeroU128::new(1).expect("one is nonzero"),
                max_error_nanos: None,
                monotonic: true,
            },
            can_follow_external: false,
        };
        let descriptor = SinkDescriptor {
            id: config.id,
            device_id: config.device_id,
            name: config.source.clone(),
            capabilities: EndpointCapabilities {
                formats: vec![format.clone()],
                clocks: vec![clock],
                memory_domains: vec![MemoryDomain::Cpu],
                transfer: TransferLimits::new(
                    NonZeroUsize::new(16).expect("16 is nonzero"),
                    NonZeroUsize::new(64 * 1024 * 1024).expect("64 MiB is nonzero"),
                ),
            },
            permission: PermissionState::Granted,
            driver: DriverState::Ready,
        };
        Ok(Self {
            descriptor,
            config,
            format,
            lifecycle: LifecycleState::Closed,
            health: EndpointHealth::HEALTHY,
            sender: None,
            advertised_port: None,
            resume_running: false,
            options: None,
        })
    }

    /// The advertised format.
    #[must_use]
    pub const fn format(&self) -> &FormatDescriptor {
        &self.format
    }

    /// The bound sender address while open.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.sender.as_ref().map(Sender::local_addr)
    }

    /// The port receivers should connect to while open.
    #[must_use]
    pub const fn source_port(&self) -> Option<u16> {
        self.advertised_port
    }

    /// How many receivers subscribed to `kind`.
    #[must_use]
    pub fn subscribers(&self, kind: crate::FrameKind) -> usize {
        self.sender
            .as_ref()
            .map_or(0, |sender| sender.subscribers(kind))
    }

    fn publish(&mut self, media: &OmtMedia) -> Result<(), IoError> {
        let sender = self.sender.as_ref().ok_or(IoError::InvalidState {
            operation: "send",
            state: self.lifecycle,
        })?;
        if self.lifecycle != LifecycleState::Running {
            return Err(IoError::InvalidState {
                operation: "send",
                state: self.lifecycle,
            });
        }
        match media {
            OmtMedia::Video(frame) => {
                let converted = video_frame(&self.config, frame)?;
                sender
                    .send_video(&converted, None)
                    .map_err(|error| adapter(error.to_string()))?;
            }
            OmtMedia::Audio(block) => {
                let converted = audio_frame(block)?;
                sender
                    .send_audio(&converted)
                    .map_err(|error| adapter(error.to_string()))?;
            }
        }
        Ok(())
    }
}

impl MediaSink for OmtSink {
    type Media = OmtMedia;

    fn descriptor(&self) -> &SinkDescriptor {
        &self.descriptor
    }

    fn lifecycle(&self) -> LifecycleState {
        self.lifecycle
    }

    fn health(&self) -> &EndpointHealth {
        &self.health
    }

    fn open(&mut self, options: OpenOptions) -> Result<(), IoError> {
        if self.lifecycle != LifecycleState::Closed {
            return Err(IoError::InvalidState {
                operation: "open",
                state: self.lifecycle,
            });
        }
        if options.format != self.format || !self.descriptor.capabilities.supports(&options) {
            return Err(IoError::UnsupportedFormat);
        }
        let sender = Sender::bind(("0.0.0.0", self.config.port))
            .map_err(|error| adapter(error.to_string()))?;
        if let Some(info) = &self.config.info {
            sender
                .set_info(info)
                .map_err(|error| adapter(error.to_string()))?;
        }
        self.advertised_port = Some(sender.local_addr().port());
        self.sender = Some(sender);
        self.options = Some(options);
        self.lifecycle = LifecycleState::Open;
        Ok(())
    }

    fn start(&mut self) -> Result<(), IoError> {
        if self.lifecycle != LifecycleState::Open {
            return Err(IoError::InvalidState {
                operation: "start",
                state: self.lifecycle,
            });
        }
        self.resume_running = true;
        self.lifecycle = LifecycleState::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), IoError> {
        if self.lifecycle != LifecycleState::Running {
            return Err(IoError::InvalidState {
                operation: "stop",
                state: self.lifecycle,
            });
        }
        self.resume_running = false;
        self.lifecycle = LifecycleState::Open;
        Ok(())
    }

    fn close(&mut self) -> Result<(), IoError> {
        if self.lifecycle == LifecycleState::Closed {
            return Ok(());
        }
        self.sender = None;
        self.advertised_port = None;
        self.options = None;
        self.resume_running = false;
        self.lifecycle = LifecycleState::Closed;
        Ok(())
    }

    fn begin_recovery(&mut self) -> Result<(), IoError> {
        if self.lifecycle != LifecycleState::Lost {
            return Err(IoError::InvalidState {
                operation: "begin recovery",
                state: self.lifecycle,
            });
        }
        self.lifecycle = LifecycleState::Recovering;
        Ok(())
    }

    fn finish_recovery(&mut self) -> Result<(), IoError> {
        if self.lifecycle != LifecycleState::Recovering {
            return Err(IoError::InvalidState {
                operation: "finish recovery",
                state: self.lifecycle,
            });
        }
        self.lifecycle = if self.resume_running {
            LifecycleState::Running
        } else {
            LifecycleState::Open
        };
        Ok(())
    }

    fn try_send(&mut self, media: Self::Media) -> Result<(), WriteError<Self::Media>> {
        if self.lifecycle != LifecycleState::Running {
            return Err(WriteError::Rejected {
                media,
                error: IoError::InvalidState {
                    operation: "send",
                    state: self.lifecycle,
                },
            });
        }
        match self.publish(&media) {
            Ok(()) => Ok(()),
            Err(error) => Err(WriteError::Rejected { media, error }),
        }
    }
}

/// The one video descriptor an OMT sink advertises.
///
/// # Errors
///
/// The codec, dimensions, and rate must be ones an OMT frame holds.
pub fn format_descriptor(config: &OmtSinkConfig) -> Result<FormatDescriptor, IoError> {
    if config.width == 0
        || config.height == 0
        || config.frame_rate.0 == 0
        || config.frame_rate.1 == 0
        || !matches!(
            config.codec,
            VideoCodec::Uyvy | VideoCodec::Bgra | VideoCodec::Nv12
        )
        || config.codec.data_len(config.width, config.height).is_none()
    {
        return Err(IoError::UnsupportedFormat);
    }
    let key =
        |name: &str| StableId::new(name).map_err(|_| adapter("the format field name is invalid"));
    Ok(FormatDescriptor::new(
        StableId::new(OMT_FORMAT_KIND).map_err(|_| adapter("the format kind is invalid"))?,
    )
    .with_field(key("codec")?, codec_label(config.codec))
    .with_field(key("width")?, u64::from(config.width))
    .with_field(key("height")?, u64::from(config.height))
    .with_field(key("rate_n")?, u64::from(config.frame_rate.0))
    .with_field(key("rate_d")?, u64::from(config.frame_rate.1)))
}

const fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Uyvy => "uyvy",
        VideoCodec::Bgra => "bgra",
        VideoCodec::Nv12 => "nv12",
        _ => "unsupported",
    }
}

fn video_frame(config: &OmtSinkConfig, frame: &CpuVideoFrame) -> Result<VideoFrame, IoError> {
    let payload = frame.payload();
    let dimensions = payload.dimensions();
    if dimensions.width() != config.width || dimensions.height() != config.height {
        return Err(IoError::UnsupportedFormat);
    }
    let width = usize::try_from(config.width).map_err(|_| IoError::UnsupportedFormat)?;
    let height = usize::try_from(config.height).map_err(|_| IoError::UnsupportedFormat)?;
    let data = match (config.codec, payload.format()) {
        (VideoCodec::Uyvy, PixelFormat::Yuv422) => rows(
            payload.plane(0).ok_or(IoError::UnsupportedFormat)?,
            width * 2,
            height,
        )?,
        (VideoCodec::Bgra, PixelFormat::Bgra8) => rows(
            payload.plane(0).ok_or(IoError::UnsupportedFormat)?,
            width * 4,
            height,
        )?,
        (VideoCodec::Nv12, PixelFormat::Nv12) => {
            let mut data = rows(
                payload.plane(0).ok_or(IoError::UnsupportedFormat)?,
                width,
                height,
            )?;
            data.extend(rows(
                payload.plane(1).ok_or(IoError::UnsupportedFormat)?,
                width,
                height / 2,
            )?);
            data
        }
        _ => return Err(IoError::UnsupportedFormat),
    };
    let mut out = VideoFrame::new(
        config.codec,
        config.width,
        config.height,
        config.frame_rate,
        data,
    )
    .map_err(|error| adapter(error.to_string()))?;
    out.timestamp = ticks(frame.timing());
    if config.codec == VideoCodec::Bgra {
        out.flags = VideoFlags::ALPHA;
    }
    Ok(out)
}

fn audio_frame(block: &AudioBlock) -> Result<AudioFrame, IoError> {
    let mut frame = AudioFrame::new(block.sample_rate().hertz(), block.planes().to_vec())
        .map_err(|error| adapter(error.to_string()))?;
    frame.timestamp = ticks(block.timing());
    Ok(frame)
}

fn rows(
    plane: &fm_frame::CpuVideoPlane,
    row_bytes: usize,
    rows: usize,
) -> Result<Vec<u8>, IoError> {
    if plane.stride() < row_bytes
        || plane.bytes().len() < plane.stride().saturating_mul(rows.saturating_sub(1)) + row_bytes
    {
        return Err(IoError::UnsupportedFormat);
    }
    let mut out = Vec::with_capacity(row_bytes * rows);
    for row in 0..rows {
        let start = row * plane.stride();
        out.extend_from_slice(&plane.bytes()[start..start + row_bytes]);
    }
    Ok(out)
}

fn ticks(timing: MediaTiming) -> i64 {
    timing.presentation_timestamp().as_nanos() / 100
}

fn adapter(detail: impl Into<String>) -> IoError {
    IoError::AdapterFailure {
        detail: detail.into(),
        remediation: None,
    }
}

impl From<OmtError> for IoError {
    fn from(error: OmtError) -> Self {
        adapter(error.to_string())
    }
}
