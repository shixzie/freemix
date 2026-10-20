//! Acquisition through a supervised `ffmpeg` child (`-f v4l2`, `-f alsa`).
//!
//! [`FfmpegAcquirer`] opens a device with
//! [`fm_codec_ffmpeg::network_input::NetworkReceiver`] in its device mode:
//! the child reads the node in the negotiated pixel format, size, and frame
//! rate (MJPEG is decoded), deinterlaces interlaced pictures with the
//! reported field order, scales to the program size, resamples to the
//! program rate, and reads the audio device beside it. The receiver keeps
//! a short jitter buffer, converts the sound card's audio into the engine
//! clock with drift correction (AU-004; the pictures follow it), and
//! reopens the device when it stops delivering; an audio device that keeps
//! the node from opening is dropped. Like every `FFmpeg` path, it needs
//! `ffmpeg` on `PATH` (or an explicit executable).

use fm_codec_ffmpeg::Executable;
use fm_codec_ffmpeg::network_input::{
    DeviceAudioInput, DeviceInput, LiveSourceTelemetry, NetworkInputConfig, NetworkReceiver,
    Picture,
};
use fm_types::capture::{CapturePixelFormat, CaptureScan};
use fm_types::network_stream::{
    GiveUp, NetworkStreamSettings, NetworkStreamState, ReconnectPolicy, RtspTransport, StallSlate,
};

use crate::acquire::{AcquireRequest, AcquiredFrame, Acquirer, Acquisition, AcquisitionHealth};

/// The jitter buffer of a local device, in milliseconds (the receiver's
/// minimum).
const DEVICE_JITTER_MS: u32 = 40;
/// How long a device may deliver nothing before the child is restarted.
const DEVICE_TIMEOUT_MS: u32 = 2_000;

/// Opens devices through `ffmpeg`.
#[derive(Clone, Debug)]
pub struct FfmpegAcquirer {
    pub ffmpeg: Executable,
    /// The picture demuxer (`v4l2`).
    pub demuxer: String,
    /// The audio demuxer (`alsa`).
    pub audio_demuxer: String,
    /// Whether to pass the negotiated format as demuxer options
    /// (`input_format`, `video_size`, `framerate`); only V4L2 takes them.
    pub format_options: bool,
}

impl FfmpegAcquirer {
    /// The V4L2 and ALSA acquirer.
    #[must_use]
    pub fn v4l2() -> Self {
        Self {
            ffmpeg: Executable::SearchPath,
            demuxer: "v4l2".into(),
            audio_demuxer: "alsa".into(),
            format_options: true,
        }
    }

    /// The `ffmpeg` arguments of `request`, as a device input.
    #[must_use]
    pub fn device_input(&self, request: &AcquireRequest) -> DeviceInput {
        let format = &request.format;
        let options = if self.format_options {
            vec![
                (
                    "input_format".to_owned(),
                    match format.pixel_format {
                        CapturePixelFormat::Yuyv => "yuyv422",
                        CapturePixelFormat::Nv12 => "nv12",
                        CapturePixelFormat::Mjpeg => "mjpeg",
                    }
                    .to_owned(),
                ),
                (
                    "video_size".to_owned(),
                    format!("{}x{}", format.width, format.height),
                ),
                (
                    "framerate".to_owned(),
                    format!("{}/{}", format.rate.numerator(), format.rate.denominator()),
                ),
            ]
        } else {
            Vec::new()
        };
        DeviceInput {
            format: self.demuxer.clone(),
            options,
            path: request.device.path.clone(),
            field_order: match format.scan {
                CaptureScan::Progressive => None,
                CaptureScan::InterlacedTff => Some("tff"),
                CaptureScan::InterlacedBff => Some("bff"),
            },
            audio: request.audio.as_ref().map(|path| DeviceAudioInput {
                format: self.audio_demuxer.clone(),
                path: path.clone(),
            }),
            audio_only: false,
        }
    }
}

/// The receive settings of a local device: the shortest jitter buffer,
/// quick reopen, never giving up.
fn device_settings() -> NetworkStreamSettings {
    NetworkStreamSettings {
        transport: RtspTransport::Auto,
        jitter_buffer_ms: DEVICE_JITTER_MS,
        reconnect: ReconnectPolicy {
            backoff_min_ms: 250,
            backoff_max_ms: 1_000,
            give_up: GiveUp::Never,
        },
        timeout_ms: DEVICE_TIMEOUT_MS,
        stall: StallSlate::Freeze,
        ..NetworkStreamSettings::default()
    }
}

impl Acquirer for FfmpegAcquirer {
    fn start(&self, request: &AcquireRequest) -> Result<Box<dyn Acquisition>, String> {
        let mut config = NetworkInputConfig::device(
            self.device_input(request),
            device_settings(),
            request.width,
            request.height,
            request.rate,
        );
        config.ffmpeg = self.ffmpeg.clone();
        config.sample_rate = request.sample_rate;
        config.channels = 2;
        let receiver = NetworkReceiver::start(config).map_err(|error| error.to_string())?;
        Ok(Box::new(FfmpegAcquisition { receiver }))
    }
}

struct FfmpegAcquisition {
    receiver: NetworkReceiver,
}

impl Acquisition for FfmpegAcquisition {
    fn pull(&mut self) -> AcquiredFrame {
        let frame = self.receiver.pull();
        AcquiredFrame {
            picture: match frame.picture {
                Picture::New(bytes) => Some(bytes),
                Picture::Hold | Picture::Black => None,
            },
            audio: self.receiver.audio_active().then_some(frame.audio),
        }
    }

    fn health(&self) -> AcquisitionHealth {
        let status = self.receiver.status();
        match status.state {
            NetworkStreamState::Idle
            | NetworkStreamState::Connecting
            | NetworkStreamState::Listening
            | NetworkStreamState::Buffering => AcquisitionHealth::Starting,
            NetworkStreamState::Live => AcquisitionHealth::Live,
            NetworkStreamState::Stalled | NetworkStreamState::Reconnecting { .. } => {
                AcquisitionHealth::Stalled
            }
            NetworkStreamState::Failed => AcquisitionHealth::Failed(
                status
                    .last_error
                    .unwrap_or_else(|| "the device stopped".to_owned()),
            ),
        }
    }

    fn audio_active(&self) -> bool {
        self.receiver.audio_active()
    }

    fn last_error(&self) -> Option<String> {
        self.receiver.status().last_error
    }

    fn sync_telemetry(&self) -> Option<LiveSourceTelemetry> {
        self.receiver.sync_telemetry()
    }
}
