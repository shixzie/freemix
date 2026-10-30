//! Open Media Transport as a [`TransportSink`]: raw pairs to an OMT source.
//!
//! The shared output pipeline packs one tightly packed RGBA picture followed
//! by sample-major `f32le` audio per packet. This sink converts that pair to
//! the OMT BGRA picture and planar audio, and hands it to an in-tree
//! [`Sender`] bound to the endpoint's port. Receivers subscribe over OMT; a
//! source with no receivers still accepts every pair.

use crate::config::{DestinationConfig, Endpoint};
use crate::output::{
    ConnectionObservation, FailureCause, FailureStage, OutputPacket, SendObservation, SinkError,
    SinkWrite, TransportSink,
};
use crate::rtmp::RtmpSinkConfig;
use fm_io_omt::{AudioFrame, Sender, SenderInfo, VideoCodec, VideoFrame};

/// An OMT source driven by the shared output set.
pub struct OmtTransportSink {
    config: RtmpSinkConfig,
    sender: Option<Sender>,
    info: SenderInfo,
}

impl OmtTransportSink {
    /// A sink for `config`'s rendition, format, and queue policy.
    #[must_use]
    pub fn new(config: RtmpSinkConfig) -> Self {
        Self {
            config,
            sender: None,
            info: SenderInfo {
                product_name: "FreeMix".to_owned(),
                manufacturer: "FreeMix".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        }
    }

    /// The bound sender, once connected.
    #[must_use]
    pub const fn sender(&self) -> Option<&Sender> {
        self.sender.as_ref()
    }

    fn reject(message: &str) -> SinkError {
        SinkError::rejected_packet(FailureStage::Protocol, None, message.to_owned())
            .with_cause(FailureCause::Unsupported)
    }
}

impl TransportSink for OmtTransportSink {
    fn connect(
        &mut self,
        _config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<ConnectionObservation, SinkError> {
        let port = endpoint.port().get();
        let sender = Sender::bind(("0.0.0.0", port)).map_err(|error| {
            SinkError::new(FailureStage::Connect, None, error.to_string(), true)
                .with_cause(FailureCause::IoError)
        })?;
        sender.set_info(&self.info).map_err(|error| {
            SinkError::new(FailureStage::Connect, None, error.to_string(), false)
                .with_cause(FailureCause::IoError)
        })?;
        self.sender = Some(sender);
        Ok(ConnectionObservation::default())
    }

    fn write(&mut self, packet: &OutputPacket) -> Result<SinkWrite, SinkError> {
        let Some(sender) = &self.sender else {
            return Err(SinkError::new(
                FailureStage::Connect,
                None,
                "the OMT source is not listening",
                true,
            )
            .with_cause(FailureCause::ConnectionLost));
        };
        let format = &self.config.format;
        let rgba_len = format.rgba_bytes_per_frame();
        let payload = packet.payload();
        let Some((rgba, audio)) = payload.split_at_checked(rgba_len) else {
            return Err(Self::reject("an OMT pair is shorter than its RGBA picture"));
        };
        let channels = format.channel_layout().channels().len();
        if audio.len() % (channels * 4) != 0 {
            return Err(Self::reject(
                "an OMT pair's audio span is not whole samples",
            ));
        }
        let samples = audio.len() / (channels * 4);
        let mut planes = vec![Vec::with_capacity(samples); channels];
        for (index, sample) in audio.as_chunks::<4>().0.iter().enumerate() {
            let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            planes[index % channels].push(value);
        }
        let dimensions = format.dimensions();
        let rate = format.frame_rate();
        let ticks = packet.timestamp().as_nanos() / 100;
        let mut bgra = Vec::with_capacity(rgba.len());
        for pixel in rgba.as_chunks::<4>().0 {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        let video = VideoFrame::new(
            VideoCodec::Bgra,
            dimensions.width(),
            dimensions.height(),
            (rate.numerator(), rate.denominator()),
            bgra,
        )
        .map(|frame| VideoFrame {
            timestamp: ticks,
            ..frame
        })
        .map_err(|error| Self::reject(&error.to_string()))?;
        let audio = AudioFrame::new(format.sample_rate().hertz(), planes)
            .map(|frame| AudioFrame {
                timestamp: ticks,
                ..frame
            })
            .map_err(|error| Self::reject(&error.to_string()))?;
        sender
            .send_video(&video, None)
            .map_err(|error| Self::reject(&error.to_string()))?;
        sender
            .send_audio(&audio)
            .map_err(|error| Self::reject(&error.to_string()))?;
        Ok(SinkWrite::Sent(SendObservation::default()))
    }

    fn disconnect(&mut self) {
        self.sender = None;
    }

    fn session_proven(&self) -> bool {
        self.sender.is_some()
    }
}

impl std::fmt::Debug for OmtTransportSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OmtTransportSink")
            .field("rendition", &self.config.rendition)
            .field("listening", &self.sender.is_some())
            .field("info", &self.info)
            .finish()
    }
}
