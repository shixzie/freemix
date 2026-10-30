//! Native realization of network stream inputs (IN-015, with RTMP IN-016
//! and SRT IN-017).
//!
//! Each open network stream input of a native project owns one
//! [`NetworkReceiver`]: a supervised `ffmpeg` child that receives the stream,
//! decodes it at the program size, rate, and sample rate into a bounded
//! jitter buffer, and reconnects with backoff (see
//! [`fm_codec_ffmpeg::network_input`]). The session loop never waits on the
//! network: once per engine frame it pulls each receiver
//! ([`NativeNetworkStreams::pull`]), ingests a new picture as the input's live
//! source frame (dated on the output timeline), and hands the frame's audio
//! to the Master as that frame's pushed audio. A frame without a new picture
//! keeps the input's current one; a stalled stream shows its stall slate
//! (the last picture, or opaque black) and its audio fades to silence.
//!
//! A network input starts on a transparent black picture. A closed input
//! (IN-026) stops its receiver and shows no picture and no sound until it
//! reopens, which reconnects. A configuration change (`input_stream_set`)
//! restarts the input's receiver on the new URL and settings; the old
//! child stops before the new one starts, so a listener can rebind its
//! port. A listener (RTMP or SRT) waits for its publisher in `listening`
//! and listens again when the publisher leaves, without counting attempts.
//!
//! Each receiver's status reaches clients as `network_stream_status`
//! runtime events ([`NativeNetworkStreams::statuses`]): at once when its
//! state changes or it first reports an error, and otherwise (a changing
//! error text included) at most once a second. The daemon logs each
//! state change (and each new receiver) on standard error with the URL
//! redacted; credentials, stream keys, and passphrases never leave the
//! receiver.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{
    KeyLockout, LiveSourceTelemetry, NetworkInputConfig, NetworkReceiver, Picture, PulledFrame,
};
use fm_frame::{
    AlphaMode, AudioBlock, ChannelLayout, ChromaLocation, ClockDomainId, ColorMetadata,
    ColorPrimaries, CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MatrixCoefficients,
    MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SampleRate, SequenceNumber, SignalRange, TimeBase, TransferFunction, VideoFrameMetadata,
};
use fm_io_omt::{Frame as OmtFrame, OmtError, Receiver as OmtReceiver, VideoCodec as OmtCodec};
use fm_types::network_stream::{
    NetworkStreamConfig, NetworkStreamState, NetworkStreamStatus, omt_authority,
};
use fm_types::{FrameRate, InputId, PixelFormat, VideoDimensions};

use crate::native_files::{blank_picture, output_timing};
use crate::native_sync::{SourceKind, SyncLog, wire_sample};

/// Longest a changed-but-same-state status waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

/// One network stream input a native daemon receives.
pub(crate) struct NetworkStreamInput {
    pub(crate) input: InputId,
    pub(crate) config: NetworkStreamConfig,
    /// The input is closed (IN-026): nothing connects until it reopens.
    pub(crate) closed: bool,
}

/// One frame of one network input, collected by the loop.
pub(crate) struct PulledStream {
    pub(crate) input: InputId,
    /// A new picture to ingest; `None` keeps the input's current one.
    pub(crate) video: Option<CpuVideoFrame>,
    pub(crate) audio: Option<AudioBlock>,
}

/// The thing receiving one network stream: ffmpeg for the ffmpeg protocols,
/// or the in-tree OMT receiver for `omt://`.
enum StreamReceiver {
    Network(NetworkReceiver),
    Omt(OmtStreamReceiver),
}

impl StreamReceiver {
    fn pull(&mut self) -> PulledFrame {
        match self {
            Self::Network(receiver) => receiver.pull(),
            Self::Omt(receiver) => receiver.pull(),
        }
    }

    fn status(&self) -> NetworkStreamStatus {
        match self {
            Self::Network(receiver) => receiver.status(),
            Self::Omt(receiver) => receiver.status(),
        }
    }

    fn sync_telemetry(&self) -> Option<LiveSourceTelemetry> {
        match self {
            Self::Network(receiver) => receiver.sync_telemetry(),
            Self::Omt(_) => None,
        }
    }

    fn take_key_lockouts(&self) -> Vec<KeyLockout> {
        match self {
            Self::Network(receiver) => receiver.take_key_lockouts(),
            Self::Omt(_) => Vec::new(),
        }
    }
}

/// One `omt://` stream: the in-tree OMT receiver, its latest picture, audio,
/// and counters.
struct OmtStreamReceiver {
    receiver: OmtReceiver,
    dimensions: (u32, u32),
    error: Option<String>,
    source_channels: u16,
    dropped: u64,
    late: u64,
    reconnects: u32,
}

impl OmtStreamReceiver {
    fn start(config: &NetworkStreamConfig, dimensions: (u32, u32)) -> Result<Self, String> {
        let authority = omt_authority(&config.url).ok_or("the OMT URL has no authority")?;
        let mut receiver = OmtReceiver::connect_timeout(authority.as_str(), Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        receiver
            .subscribe(true, true, false)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            receiver,
            dimensions,
            error: None,
            source_channels: 2,
            dropped: 0,
            late: 0,
            reconnects: 0,
        })
    }

    fn pull(&mut self) -> PulledFrame {
        let mut picture = Picture::Hold;
        let mut audio = Vec::new();
        for _ in 0..4 {
            match self.receiver.receive(Some(Duration::from_millis(1))) {
                Ok(Some(OmtFrame::Video(video))) => {
                    if let Some(rgba) = rgba_picture(&video, self.dimensions) {
                        picture = Picture::New(rgba);
                        self.error = None;
                    } else {
                        self.error = Some("an OMT picture has an unsupported format".to_owned());
                    }
                }
                Ok(Some(OmtFrame::Audio(frame))) => {
                    self.source_channels = u16::try_from(frame.channels.len()).unwrap_or(u16::MAX);
                    if frame.channels.len() == 2 {
                        audio = frame.channels;
                        self.error = None;
                    }
                }
                Ok(Some(OmtFrame::Metadata(_)) | None) => {}
                Err(error) => {
                    self.error = Some(error.to_string());
                    if matches!(error, OmtError::Closed | OmtError::Io(_)) {
                        self.reconnects = self.reconnects.saturating_add(1);
                        self.late = self.late.saturating_add(1);
                    }
                    break;
                }
            }
        }
        PulledFrame { picture, audio }
    }

    fn status(&self) -> NetworkStreamStatus {
        NetworkStreamStatus {
            state: if self.error.is_some() {
                NetworkStreamState::Stalled
            } else {
                NetworkStreamState::Live
            },
            bitrate_bps: 0,
            buffer_ms: 0,
            dropped_frames: self.dropped,
            late_frames: self.late,
            reconnects: self.reconnects,
            source_channels: self.source_channels,
            last_error: self.error.clone(),
        }
    }
}

struct Stream {
    config: NetworkStreamConfig,
    receiver: Option<StreamReceiver>,
    /// Why the receiver could not start, if it could not.
    start_error: Option<String>,
    closed: bool,
    /// The input closed and its blank picture is still to be presented.
    blank_pending: bool,
    reported: Option<(NetworkStreamStatus, Instant)>,
    /// When its clock drift was last logged (AU-004).
    sync_log: SyncLog,
}

/// The network stream inputs of one native daemon and their receivers.
pub(crate) struct NativeNetworkStreams {
    streams: BTreeMap<InputId, Stream>,
    rate: FrameRate,
    clock_domain: ClockDomainId,
    dimensions: (u32, u32),
    sample_rate: SampleRate,
    /// The engine frame last pulled for.
    last_frame: Option<u64>,
}

impl NativeNetworkStreams {
    /// Starts a receiver for every open input.
    pub(crate) fn start(
        inputs: Vec<NetworkStreamInput>,
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
        sample_rate: SampleRate,
    ) -> Self {
        let mut streams = Self {
            streams: BTreeMap::new(),
            rate,
            clock_domain,
            dimensions,
            sample_rate,
            last_frame: None,
        };
        for NetworkStreamInput {
            input,
            config,
            closed,
        } in inputs
        {
            let mut stream = Stream {
                config,
                receiver: None,
                start_error: None,
                closed,
                blank_pending: false,
                reported: None,
                sync_log: SyncLog::default(),
            };
            if !closed {
                streams.connect(input, &mut stream);
            }
            streams.streams.insert(input, stream);
        }
        streams
    }

    fn connect(&self, input: InputId, stream: &mut Stream) {
        let mut config = NetworkInputConfig::new(
            &stream.config,
            self.dimensions.0,
            self.dimensions.1,
            self.rate,
        );
        config.sample_rate = self.sample_rate.hertz();
        config.channels = 2;
        // A new receiver reports (and logs its URL) afresh.
        stream.reported = None;
        let started = if omt_authority(&stream.config.url).is_some() {
            OmtStreamReceiver::start(&stream.config, self.dimensions).map(StreamReceiver::Omt)
        } else {
            NetworkReceiver::start(config)
                .map(StreamReceiver::Network)
                .map_err(|error| error.to_string())
        };
        match started {
            Ok(receiver) => {
                stream.receiver = Some(receiver);
                stream.start_error = None;
            }
            Err(error) => {
                eprintln!(
                    "FREEMIXD_NETWORK\tv=1\tinput={input}\tstart_failed\turl={}",
                    stream.config.redacted_url()
                );
                stream.receiver = None;
                stream.start_error = Some(error.clone());
            }
        }
    }

    /// Every network stream input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.streams.keys().copied().collect()
    }

    /// Follows the project: closes, reopens, and reconfigures inputs as
    /// `configs` and `closed` say. A changed configuration reconnects.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, NetworkStreamConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
        let mut streams = std::mem::take(&mut self.streams);
        streams.retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        for (input, config) in configs {
            let closed = closed(*input);
            let stream = streams.entry(*input).or_insert_with(|| Stream {
                config: config.clone(),
                receiver: None,
                start_error: None,
                closed: true,
                blank_pending: false,
                reported: None,
                sync_log: SyncLog::default(),
            });
            let changed = stream.config != *config;
            if changed {
                stream.config = config.clone();
            }
            if closed {
                if !stream.closed {
                    stream.receiver = None;
                    stream.start_error = None;
                    stream.blank_pending = true;
                }
                stream.closed = true;
                continue;
            }
            if stream.closed
                || changed
                || (stream.receiver.is_none() && stream.start_error.is_none())
            {
                stream.closed = false;
                stream.blank_pending = false;
                // Stop the old child before the new one binds the port.
                stream.receiver = None;
                self.connect(*input, stream);
            }
        }
        self.streams = streams;
    }

    /// Pulls every input's picture and audio for engine frame `frame`,
    /// once per frame. Never blocks on the network.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        if self.last_frame.is_some_and(|last| frame <= last) || self.streams.is_empty() {
            return Vec::new();
        }
        self.last_frame = Some(frame);
        let format = Format {
            rate: self.rate,
            clock_domain: self.clock_domain,
            dimensions: self.dimensions,
            sample_rate: self.sample_rate,
        };
        let mut pulled = Vec::with_capacity(self.streams.len());
        for (&input, stream) in &mut self.streams {
            if stream.closed {
                if stream.blank_pending {
                    stream.blank_pending = false;
                    pulled.push(PulledStream {
                        input,
                        video: output_timing(frame, format.rate, format.clock_domain)
                            .and_then(|timing| blank_picture(format.dimensions, timing)),
                        audio: None,
                    });
                }
                continue;
            }
            let Some(receiver) = stream.receiver.as_mut() else {
                continue;
            };
            let PulledFrame { picture, audio } = receiver.pull();
            let video = match picture {
                Picture::New(bytes) => format.picture(frame, bytes),
                Picture::Black => format.black(frame),
                Picture::Hold => None,
            };
            pulled.push(PulledStream {
                input,
                video,
                audio: format.audio(frame, audio),
            });
        }
        pulled
    }

    /// Every open stream's drift loop as wire samples (Protocol 2.76).
    pub(crate) fn sync_samples(&self) -> Vec<fm_protocol::SourceSyncSample> {
        self.streams
            .iter()
            .filter(|(_, stream)| !stream.closed)
            .filter_map(|(&input, stream)| {
                let telemetry = stream.receiver.as_ref()?.sync_telemetry()?;
                wire_sample(SourceKind::Stream, input, &telemetry)
            })
            .collect()
    }

    /// The statuses due for a report at `now`: a changed state at once,
    /// other changes at most once a second.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, NetworkStreamStatus)> {
        statuses(&mut self.streams, now)
    }
}

/// The program format every picture and audio block is delivered in
/// (shared with capture inputs, see [`crate::native_captures`]).
#[derive(Clone, Copy)]
pub(crate) struct Format {
    pub(crate) rate: FrameRate,
    pub(crate) clock_domain: ClockDomainId,
    pub(crate) dimensions: (u32, u32),
    pub(crate) sample_rate: SampleRate,
}

impl Format {
    /// An RGBA picture of program frame `frame`.
    pub(crate) fn picture(&self, frame: u64, bytes: Vec<u8>) -> Option<CpuVideoFrame> {
        let timing = output_timing(frame, self.rate, self.clock_domain)?;
        let size = VideoDimensions::new(self.dimensions.0, self.dimensions.1)?;
        let stride = usize::try_from(self.dimensions.0).ok()?.checked_mul(4)?;
        let plane = CpuVideoPlane::new(stride, bytes).ok()?;
        let payload = CpuVideoPayload::new(PixelFormat::Rgba8, size, vec![plane]).ok()?;
        CpuVideoFrame::new(timing, payload)
            .with_metadata(VideoFrameMetadata::new(
                ColorMetadata {
                    primaries: ColorPrimaries::Bt709,
                    transfer: TransferFunction::Srgb,
                    matrix: MatrixCoefficients::Identity,
                    range: SignalRange::Full,
                    chroma_location: ChromaLocation::Center,
                },
                Some(AlphaMode::Straight),
            ))
            .ok()
    }

    /// Opaque black at the program size, for program frame `frame`.
    pub(crate) fn black(&self, frame: u64) -> Option<CpuVideoFrame> {
        let mut bytes = vec![0_u8; picture_bytes(self.dimensions)];
        for pixel in bytes.as_chunks_mut::<4>().0 {
            pixel[3] = u8::MAX;
        }
        self.picture(frame, bytes)
    }

    /// Stereo silence lasting program frame `frame`.
    pub(crate) fn silence(&self, frame: u64) -> Option<AudioBlock> {
        let hertz = u128::from(self.sample_rate.hertz());
        let at = |k: u64| {
            u128::from(k) * hertz * u128::from(self.rate.denominator())
                / u128::from(self.rate.numerator()).max(1)
        };
        let samples = usize::try_from(at(frame.checked_add(1)?) - at(frame)).ok()?;
        self.audio(frame, vec![vec![0.0; samples]; 2])
    }

    /// Planar stereo audio of program frame `frame`.
    pub(crate) fn audio(&self, frame: u64, planes: Vec<Vec<f32>>) -> Option<AudioBlock> {
        let samples = u64::try_from(planes.first()?.len()).ok()?;
        let hertz = self.sample_rate.hertz();
        let start = u128::from(frame) * u128::from(hertz) * u128::from(self.rate.denominator())
            / u128::from(self.rate.numerator()).max(1);
        let pts = i64::try_from(start).ok()?;
        let nanos =
            |samples: u128| u64::try_from(samples * 1_000_000_000 / u128::from(hertz).max(1)).ok();
        let timing = MediaTiming::new(
            OriginalTimestamp::new(MediaTimestamp::new(pts), TimeBase::new(1, hertz).ok()?),
            NormalizedTimestamp::from_nanos(i64::try_from(nanos(start)?).ok()?),
            NormalizedDuration::from_nanos(nanos(u128::from(samples))?.max(1)).ok()?,
            self.clock_domain,
            SequenceNumber::new(frame),
        )
        .ok()?;
        AudioBlock::new(timing, self.sample_rate, ChannelLayout::stereo(), planes).ok()
    }
}

fn statuses(
    streams: &mut BTreeMap<InputId, Stream>,
    now: Instant,
) -> Vec<(InputId, NetworkStreamStatus)> {
    let mut due = Vec::new();
    for (&input, stream) in streams {
        // An RTMP listener's wrong-stream-key lockouts: the peer address
        // and back-off, never the key it tried.
        for lockout in stream
            .receiver
            .as_ref()
            .map(StreamReceiver::take_key_lockouts)
            .unwrap_or_default()
        {
            eprintln!(
                "FREEMIXD_NETWORK_LOCKOUT\tv=1\tinput={input}\tpeer={}\tfailures={}\tlockout_ms={}",
                lockout.peer,
                lockout.failures,
                lockout.lockout.as_millis()
            );
        }
        stream.sync_log.report(
            SourceKind::Stream,
            input,
            stream
                .receiver
                .as_ref()
                .and_then(StreamReceiver::sync_telemetry)
                .as_ref(),
            now,
        );
        let status = match (&stream.receiver, &stream.start_error) {
            _ if stream.closed => NetworkStreamStatus::default(),
            (Some(receiver), _) => receiver.status(),
            (None, Some(error)) => NetworkStreamStatus {
                state: NetworkStreamState::Failed,
                last_error: Some(error.clone()),
                ..NetworkStreamStatus::default()
            },
            (None, None) => NetworkStreamStatus::default(),
        };
        let report = match &stream.reported {
            None => true,
            Some((reported, at)) => {
                let state_changed = reported.state.token() != status.state.token()
                    || matches!(
                        (reported.state, status.state),
                        (
                            NetworkStreamState::Reconnecting { attempt: before, .. },
                            NetworkStreamState::Reconnecting { attempt: after, .. },
                        ) if before != after
                    )
                    // A first error is news; a changing error text (a
                    // decoder reports one per damaged picture) waits its
                    // interval like any other change.
                    || (reported.last_error.is_none() && status.last_error.is_some());
                state_changed
                    || (*reported != status
                        && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
            }
        };
        if !report {
            continue;
        }
        if stream
            .reported
            .as_ref()
            .is_none_or(|(reported, _)| reported.state.token() != status.state.token())
        {
            eprintln!(
                "FREEMIXD_NETWORK\tv=1\tinput={input}\tstate={}\treconnects={}\turl={}",
                status.state.token(),
                status.reconnects,
                stream.config.redacted_url()
            );
        }
        stream.reported = Some((status.clone(), now));
        due.push((input, status));
    }
    due
}

/// The RGBA picture an OMT video frame shows, when its dimensions match the
/// program and its codec is one this adapter converts.
fn rgba_picture(frame: &fm_io_omt::VideoFrame, dimensions: (u32, u32)) -> Option<Vec<u8>> {
    if frame.width != dimensions.0 || frame.height != dimensions.1 {
        return None;
    }
    let width = usize::try_from(frame.width).ok()?;
    let height = usize::try_from(frame.height).ok()?;
    let mut rgba = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    match frame.codec {
        OmtCodec::Bgra => {
            if frame.data.len() != width.checked_mul(height)?.checked_mul(4)? {
                return None;
            }
            for pixel in frame.data.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        OmtCodec::Uyvy => {
            if frame.data.len() != width.checked_mul(height)?.checked_mul(2)? {
                return None;
            }
            let bt601 = frame.color_space == fm_io_omt::ColorSpace::Bt601;
            for pair in frame.data.as_chunks::<4>().0 {
                let (u, y0, v, y1) = (pair[0], pair[1], pair[2], pair[3]);
                rgba.extend_from_slice(&yuv_to_rgba(y0, u, v, bt601));
                rgba.extend_from_slice(&yuv_to_rgba(y1, u, v, bt601));
            }
        }
        OmtCodec::Nv12 => {
            let luma = width.checked_mul(height)?;
            if frame.data.len() != luma.checked_add(width.checked_mul(height / 2)?)? {
                return None;
            }
            let (y_plane, uv_plane) = frame.data.split_at(luma);
            let bt601 = frame.color_space == fm_io_omt::ColorSpace::Bt601;
            for row in 0..height {
                for column in 0..width {
                    let y = y_plane[row * width + column];
                    let uv = (row / 2) * width + (column / 2) * 2;
                    let (u, v) = (uv_plane[uv], uv_plane[uv + 1]);
                    rgba.extend_from_slice(&yuv_to_rgba(y, u, v, bt601));
                }
            }
        }
        _ => return None,
    }
    Some(rgba)
}

/// BT.601 or BT.709 limited-range YUV to full-range RGBA.
#[allow(clippy::many_single_char_names)] // the standard Y'CbCr notation
fn yuv_to_rgba(y: u8, u: u8, v: u8, bt601: bool) -> [u8; 4] {
    let c = f32::from(y) - 16.0;
    let d = f32::from(u) - 128.0;
    let e = f32::from(v) - 128.0;
    let (rv, gu, gv, bu) = if bt601 {
        (1.596, -0.392, -0.813, 2.018)
    } else {
        (1.793, -0.213, -0.533, 2.112)
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255
    let clamp = |value: f32| value.clamp(0.0, 255.0).round() as u8;
    [
        clamp(1.164 * c + rv * e),
        clamp(1.164 * c + gu * d + gv * e),
        clamp(1.164 * c + bu * d),
        u8::MAX,
    ]
}

fn picture_bytes(dimensions: (u32, u32)) -> usize {
    usize::try_from(u64::from(dimensions.0) * u64::from(dimensions.1) * 4).unwrap_or(0)
}

/// A network input's startup picture: transparent black, sequence zero.
pub(crate) fn startup_picture(
    dimensions: (u32, u32),
    rate: FrameRate,
    clock_domain: ClockDomainId,
) -> Option<CpuVideoFrame> {
    blank_picture(
        dimensions,
        crate::native_files::timing_at(0, rate, clock_domain)?,
    )
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use super::*;

    fn input() -> InputId {
        InputId::new(NonZeroU128::new(9).unwrap())
    }

    fn streams(closed: bool) -> NativeNetworkStreams {
        NativeNetworkStreams::start(
            vec![NetworkStreamInput {
                input: input(),
                config: NetworkStreamConfig::new("udp://127.0.0.1:9"),
                closed,
            }],
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            (32, 18),
            SampleRate::new(48_000).unwrap(),
        )
    }

    #[test]
    fn a_changing_error_text_is_reported_at_most_once_a_second() {
        // A damaged stream makes the decoder report a new error line every
        // few milliseconds; each must not become a runtime event and a
        // snapshot refresh.
        let mut map = BTreeMap::new();
        map.insert(
            input(),
            Stream {
                config: NetworkStreamConfig::new("udp://127.0.0.1:9"),
                receiver: None,
                start_error: Some("error while decoding MB 1 1".into()),
                closed: false,
                blank_pending: false,
                reported: None,
                sync_log: SyncLog::default(),
            },
        );
        let start = Instant::now();
        assert_eq!(statuses(&mut map, start).len(), 1);
        for (step, millis) in [(2_u64, 20_u64), (3, 40), (4, 60)] {
            map.get_mut(&input()).unwrap().start_error =
                Some(format!("error while decoding MB {step} 1"));
            assert_eq!(
                statuses(&mut map, start + Duration::from_millis(millis)),
                [],
                "reported again after {millis} ms"
            );
        }
        let due = statuses(&mut map, start + STATUS_INTERVAL);
        assert_eq!(due.len(), 1);
        assert_eq!(
            due[0].1.last_error.as_deref(),
            Some("error while decoding MB 4 1")
        );
    }

    #[test]
    fn a_closed_input_is_idle_and_shows_one_blank_picture_when_it_closes() {
        let mut streams = streams(true);
        assert!(streams.pull(1).is_empty());
        let statuses = streams.statuses(Instant::now());
        assert_eq!(statuses, [(input(), NetworkStreamStatus::default())]);
        // Unchanged: not reported again.
        assert_eq!(streams.statuses(Instant::now() + STATUS_INTERVAL), []);

        let configs = [(input(), NetworkStreamConfig::new("udp://127.0.0.1:9"))];
        streams.sync(&configs, |_| false);
        assert!(streams.streams[&input()].receiver.is_some());
        streams.sync(&configs, |_| true);
        assert!(streams.streams[&input()].receiver.is_none());
        let pulled = streams.pull(2);
        assert_eq!(pulled.len(), 1);
        assert!(pulled[0].audio.is_none());
        let video = pulled[0].video.as_ref().expect("a blank picture");
        assert_eq!(video.timing().sequence().get(), 3);
        // Once, and never twice for one frame.
        assert!(streams.pull(2).is_empty());
        assert!(streams.pull(3).is_empty());
    }

    #[test]
    fn an_omt_url_receives_pictures_and_audio() {
        let sender = fm_io_omt::Sender::bind("127.0.0.1:0").unwrap();
        let mut streams = NativeNetworkStreams::start(
            vec![NetworkStreamInput {
                input: input(),
                config: NetworkStreamConfig::new(format!("omt://{}", sender.local_addr())),
                closed: false,
            }],
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            (2, 2),
            SampleRate::new(48_000).unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while (sender.subscribers(fm_io_omt::FrameKind::Video) == 0
            || sender.subscribers(fm_io_omt::FrameKind::Audio) == 0)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let video = fm_io_omt::VideoFrame::new(
            fm_io_omt::VideoCodec::Bgra,
            2,
            2,
            (25, 1),
            vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4],
        )
        .unwrap();
        let audio = fm_io_omt::AudioFrame::new(48_000, vec![vec![0.5; 2], vec![-0.5; 2]]).unwrap();
        sender.send_video(&video, None).unwrap();
        sender.send_audio(&audio).unwrap();
        let mut pulled = None;
        for frame in 0..200 {
            if let Some(result) = streams.pull(frame).into_iter().next()
                && result.video.is_some()
                && result.audio.is_some()
            {
                pulled = Some(result);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let pulled = pulled.expect("an OMT picture and audio");
        let video = pulled.video.expect("the picture");
        assert_eq!(video.payload().format(), PixelFormat::Rgba8);
        assert_eq!(video.payload().plane(0).unwrap().bytes()[..4], [3, 2, 1, 4]);
        let audio = pulled.audio.expect("the audio");
        assert!((audio.planes()[0][0] - 0.5).abs() < f32::EPSILON);
        assert!((audio.planes()[1][0] + 0.5).abs() < f32::EPSILON);
        sender.stop();
    }

    #[test]
    fn an_open_input_pulls_one_silent_frame_per_engine_frame_while_it_connects() {
        let mut streams = streams(false);
        let pulled = streams.pull(4);
        assert_eq!(pulled.len(), 1);
        assert!(pulled[0].video.is_none(), "holds its startup picture");
        let audio = pulled[0].audio.as_ref().expect("the frame's audio");
        assert_eq!(audio.sample_count(), 1_920);
        assert!(audio.planes().iter().flatten().all(|sample| *sample == 0.0));
        assert!(streams.pull(4).is_empty());
        let statuses = streams.statuses(Instant::now());
        assert_eq!(statuses.len(), 1);
        assert_ne!(statuses[0].1.state, NetworkStreamState::Idle);
    }
}
