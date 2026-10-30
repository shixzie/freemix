//! An `fm-io-api` input source that receives an OMT stream.
//!
//! The source connects to an OMT sender, subscribes to video and audio, and
//! converts the frames a receiver gets into CPU video payloads and planar
//! audio blocks. It speaks the shared [`fm_io_api::MediaSource`] lifecycle,
//! health, and non-blocking receive contract.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::num::{NonZeroU128, NonZeroUsize};

use fm_frame::{
    AudioBlock, ClockDomainId, CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MediaTimestamp,
    MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber,
    TimeBase,
};
use fm_io_api::{
    DeviceId, DriverState, EndpointCapabilities, EndpointHealth, EndpointHealthState, IoError,
    LifecycleState, MediaSource, MediaTransfer, OpenOptions, PermissionState, SignalLossPolicy,
    SourceDescriptor, SourceId, TimestampCapabilities, TimestampQuality, TransferLimits,
};
use fm_types::{
    Channel, ChannelLayout, DiscreteChannel, MemoryDomain, PixelFormat, SampleRate, VideoDimensions,
};

use crate::sink::{OmtMedia, format_descriptor};
use crate::{Frame, OmtError, Quality, Receiver, Tally, VideoCodec, VideoFrame};

/// What an [`OmtSource`] subscribes to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmtSourceConfig {
    pub id: SourceId,
    pub device_id: DeviceId,
    pub name: String,
    /// The sender's TCP address.
    pub address: SocketAddr,
    pub clock_domain: ClockDomainId,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: (u32, u32),
}

/// An OMT input source.
pub struct OmtSource {
    descriptor: SourceDescriptor,
    config: OmtSourceConfig,
    format: fm_capabilities::FormatDescriptor,
    lifecycle: LifecycleState,
    health: EndpointHealth,
    receiver: Option<Receiver>,
    resume_running: bool,
    sequence: u64,
    pending: VecDeque<OmtMedia>,
}

impl OmtSource {
    /// A closed source for `config`.
    ///
    /// # Errors
    ///
    /// The codec, dimensions, and rate must be ones the sink-side format
    /// advertises.
    pub fn new(config: OmtSourceConfig) -> Result<Self, IoError> {
        let sink_config = crate::OmtSinkConfig {
            id: fm_io_api::SinkId::new(NonZeroU128::new(1).expect("one is nonzero")),
            device_id: config.device_id,
            hostname: String::new(),
            source: config.name.clone(),
            port: 0,
            clock_domain: config.clock_domain,
            codec: config.codec,
            width: config.width,
            height: config.height,
            frame_rate: config.frame_rate,
            info: None,
        };
        let format = format_descriptor(&sink_config)?;
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
        let descriptor = SourceDescriptor {
            id: config.id,
            device_id: config.device_id,
            stable_key: format!("omt.source.v1:{}", config.address),
            name: config.name.clone(),
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
            receiver: None,
            resume_running: false,
            sequence: 0,
            pending: VecDeque::new(),
        })
    }

    /// The advertised format.
    #[must_use]
    pub const fn format(&self) -> &fm_capabilities::FormatDescriptor {
        &self.format
    }

    /// Reports this source's tally to the sender.
    ///
    /// # Errors
    ///
    /// The connection must be open and writable.
    pub fn set_tally(&mut self, tally: Tally) -> Result<(), IoError> {
        self.receiver
            .as_mut()
            .ok_or(IoError::InvalidState {
                operation: "set tally",
                state: self.lifecycle,
            })?
            .set_tally(tally)
            .map_err(|error| endpoint(error.to_string()))
    }

    /// Suggests a compression quality to the sender.
    ///
    /// # Errors
    ///
    /// The connection must be open and writable.
    pub fn suggest_quality(&mut self, quality: Quality) -> Result<(), IoError> {
        self.receiver
            .as_mut()
            .ok_or(IoError::InvalidState {
                operation: "suggest quality",
                state: self.lifecycle,
            })?
            .suggest_quality(quality)
            .map_err(|error| endpoint(error.to_string()))
    }

    fn next_media(&mut self) -> Result<Option<OmtMedia>, IoError> {
        if let Some(media) = self.pending.pop_front() {
            return Ok(Some(media));
        }
        for _ in 0..4 {
            let received = self
                .receiver
                .as_mut()
                .ok_or(IoError::InvalidState {
                    operation: "receive",
                    state: self.lifecycle,
                })?
                .receive(Some(std::time::Duration::from_millis(1)));
            let frame = match received {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(error) => {
                    self.health = EndpointHealth {
                        state: EndpointHealthState::SignalLost,
                        detail: Some(error.to_string()),
                        remediation: Some(fm_io_api::Remediation::ReconnectDevice),
                    };
                    return Err(IoError::SignalLost {
                        policy: SignalLossPolicy::Hold,
                    });
                }
            };
            match frame {
                Frame::Video(video) => return self.video(video).map(Some),
                Frame::Audio(audio) => return self.audio(audio).map(Some),
                Frame::Metadata(_) => {}
            }
        }
        Ok(None)
    }

    fn video(&mut self, frame: VideoFrame) -> Result<OmtMedia, IoError> {
        if frame.width != self.config.width
            || frame.height != self.config.height
            || frame.codec != self.config.codec
        {
            return Err(IoError::UnsupportedFormat);
        }
        let width = usize::try_from(frame.width).map_err(|_| IoError::UnsupportedFormat)?;
        let height = usize::try_from(frame.height).map_err(|_| IoError::UnsupportedFormat)?;
        let payload = match frame.codec {
            VideoCodec::Uyvy => CpuVideoPayload::new(
                PixelFormat::Yuv422,
                VideoDimensions::new(frame.width, frame.height)
                    .ok_or(IoError::UnsupportedFormat)?,
                vec![
                    CpuVideoPlane::new(width * 2, frame.data)
                        .map_err(|error| endpoint(error.to_string()))?,
                ],
            ),
            VideoCodec::Bgra => CpuVideoPayload::new(
                PixelFormat::Bgra8,
                VideoDimensions::new(frame.width, frame.height)
                    .ok_or(IoError::UnsupportedFormat)?,
                vec![
                    CpuVideoPlane::new(width * 4, frame.data)
                        .map_err(|error| endpoint(error.to_string()))?,
                ],
            ),
            VideoCodec::Nv12 => {
                let uv = width
                    .checked_mul(height / 2)
                    .ok_or(IoError::UnsupportedFormat)?;
                if frame.data.len() != width * height + uv {
                    return Err(IoError::UnsupportedFormat);
                }
                let (y, uv_data) = frame.data.split_at(width * height);
                CpuVideoPayload::new(
                    PixelFormat::Nv12,
                    VideoDimensions::new(frame.width, frame.height)
                        .ok_or(IoError::UnsupportedFormat)?,
                    vec![
                        CpuVideoPlane::new(width, y.to_vec())
                            .map_err(|error| endpoint(error.to_string()))?,
                        CpuVideoPlane::new(width, uv_data.to_vec())
                            .map_err(|error| endpoint(error.to_string()))?,
                    ],
                )
            }
            _ => return Err(IoError::UnsupportedFormat),
        }
        .map_err(|error| endpoint(error.to_string()))?;
        Ok(OmtMedia::Video(CpuVideoFrame::new(
            self.video_timing(frame.timestamp),
            payload,
        )))
    }

    fn audio(&mut self, frame: crate::AudioFrame) -> Result<OmtMedia, IoError> {
        let channels = frame.channels.len();
        let layout = layout(channels).ok_or(IoError::UnsupportedFormat)?;
        let timing = self.audio_timing(
            frame.timestamp,
            frame.samples_per_channel(),
            frame.sample_rate,
        );
        let rate = SampleRate::new(frame.sample_rate).ok_or(IoError::UnsupportedFormat)?;
        AudioBlock::new(timing, rate, layout, frame.channels)
            .map(OmtMedia::Audio)
            .map_err(|error| endpoint(error.to_string()))
    }

    fn video_timing(&mut self, ticks: i64) -> MediaTiming {
        let duration = 1_000_000_000_u64.saturating_mul(u64::from(self.config.frame_rate.1))
            / u64::from(self.config.frame_rate.0);
        self.timing(ticks, duration)
    }

    fn audio_timing(&mut self, ticks: i64, samples: usize, sample_rate: u32) -> MediaTiming {
        let duration = u64::try_from(samples)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000_000)
            / u64::from(sample_rate.max(1));
        self.timing(ticks, duration)
    }

    fn timing(&mut self, ticks: i64, duration_nanos: u64) -> MediaTiming {
        self.sequence = self.sequence.wrapping_add(1);
        MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(ticks),
                TimeBase::new(1, 10_000_000).expect("10 MHz is a valid time base"),
            ),
            NormalizedTimestamp::from_nanos(ticks.saturating_mul(100)),
            NormalizedDuration::from_nanos(duration_nanos)
                .unwrap_or(NormalizedDuration::from_nanos(1).expect("one nanosecond is nonzero")),
            self.config.clock_domain,
            SequenceNumber::new(self.sequence),
        )
        .unwrap_or_else(|_| {
            MediaTiming::new(
                OriginalTimestamp::new(
                    MediaTimestamp::new(0),
                    TimeBase::new(1, 10_000_000).expect("10 MHz is a valid time base"),
                ),
                NormalizedTimestamp::from_nanos(0),
                NormalizedDuration::from_nanos(1).expect("one nanosecond is nonzero"),
                self.config.clock_domain,
                SequenceNumber::new(self.sequence),
            )
            .expect("zero timing is valid")
        })
    }
}

impl MediaSource for OmtSource {
    type Media = OmtMedia;

    fn descriptor(&self) -> &SourceDescriptor {
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
        let receiver =
            Receiver::connect_timeout(self.config.address, std::time::Duration::from_secs(2))
                .map_err(|error| match error {
                    OmtError::Closed | OmtError::Io(_) => IoError::EndpointUnavailable {
                        remediation: fm_io_api::Remediation::ReconnectDevice,
                    },
                    other => endpoint(other.to_string()),
                })?;
        self.receiver = Some(receiver);
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
        self.receiver
            .as_mut()
            .ok_or(IoError::InvalidState {
                operation: "start",
                state: self.lifecycle,
            })?
            .subscribe(true, true, false)
            .map_err(|error| endpoint(error.to_string()))?;
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
        self.receiver = None;
        self.pending.clear();
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
        self.health = EndpointHealth::HEALTHY;
        self.lifecycle = if self.resume_running {
            LifecycleState::Running
        } else {
            LifecycleState::Open
        };
        Ok(())
    }

    fn try_receive(&mut self) -> Result<Option<MediaTransfer<Self::Media>>, IoError> {
        if self.lifecycle != LifecycleState::Running {
            return Err(IoError::InvalidState {
                operation: "receive",
                state: self.lifecycle,
            });
        }
        Ok(self.next_media()?.map(MediaTransfer::Live))
    }
}

fn layout(channels: usize) -> Option<ChannelLayout> {
    match channels {
        1 => ChannelLayout::new(vec![Channel::Mono]),
        2 => Some(ChannelLayout::stereo()),
        3..=16 => {
            let labels = (1..=channels)
                .map(|number| {
                    let number = u8::try_from(number).ok()?;
                    DiscreteChannel::new(number).map(Channel::Discrete)
                })
                .collect::<Option<Vec<_>>>()?;
            ChannelLayout::new(labels)
        }
        _ => None,
    }
}

fn endpoint(detail: impl Into<String>) -> IoError {
    IoError::AdapterFailure {
        detail: detail.into(),
        remediation: None,
    }
}
