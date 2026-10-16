//! `FFmpeg`-backed network stream receiver (IN-015).
//!
//! A [`NetworkReceiver`] receives one RTSP, RTP, UDP, TCP, or HTTP MPEG-TS
//! stream, or an HLS playlist, through a supervised `ffmpeg` child, and hands
//! its consumer exactly one program frame per [`NetworkReceiver::pull`]
//! without ever blocking: the session loop that realizes Program pulls once
//! per engine frame and never waits on the network.
//!
//! # Decode
//!
//! The child decodes the stream's first video stream, scaled to the program
//! size (stretched, not letterboxed) and resampled to the program rate, as
//! raw RGBA on its standard output, and the first audio stream as
//! interleaved `f32` PCM at the program sample rate and channel count. The
//! audio reaches the receiver as the body of an HTTP `POST` to a loopback
//! listener that only accepts a random per-receiver path. A stream without
//! audio is detected when the child refuses its empty audio output; the
//! receiver then connects again at once without audio, and asks for audio
//! again on every later reconnect, so a source restarted with audio is
//! heard. (A connection that stays up is not re-probed: probing a second
//! time needs a second connection to the source, which a UDP or RTP
//! listener cannot open while the first holds its port.) A third output
//! copies the input packets into a frame CRC listing on standard error, which
//! the receiver sums into the received bitrate.
//!
//! # Jitter buffer and A/V sync
//!
//! Decoded pictures queue in a bounded jitter buffer. A connection buffers
//! until the queue holds the target (`jitter_buffer_ms` of frames), then goes
//! live; the queue holds at most twice the target, and a full queue drops its
//! oldest picture (`dropped_frames`). The target and capacity are also capped
//! so the queued pictures never exceed [`NetworkInputConfig::max_buffer_bytes`].
//! Audio queues beside the pictures and is cut in lockstep with them: the
//! `n`th pull takes exactly the samples of engine frame `n` at the program
//! rate, so audio stays locked to the engine clock and the picture it was
//! decoded with. A picture dropped for overflow drops its audio too.
//!
//! A live pull that finds no picture is late (`late_frames`) and repeats the
//! previous picture; a run of them stalls the stream, which buffers again
//! before it goes back on air. While it is not live (connecting, buffering,
//! stalled, reconnecting, or failed) the input shows its stall slate (the
//! last picture, or black) and its audio fades to silence over one frame
//! instead of stopping with a click, and fades back in when it recovers.
//!
//! # Reconnect
//!
//! A connection that delivers no media for `timeout_ms` (plus a fixed grace
//! before its first picture), or whose child exits, is dropped. The receiver
//! then waits the reconnect policy's backoff for attempt `n` and reconnects
//! (`reconnecting`); a connection that delivered media resets the attempt
//! count, and a policy that gives up after `n` attempts fails the stream.
//!
//! # Containment
//!
//! The child may open only its own scheme's protocols
//! (`-protocol_whitelist`): a served playlist, session description, or
//! redirect cannot reach `file`, `concat`, or a local RTP/UDP port. On
//! Linux the child carries a parent-death signal, so a receiver whose
//! process is killed outright cannot leave it holding a port; elsewhere, a
//! UDP or RTP child that hears nothing exits after its timeout plus the
//! connect grace. (The crate's recording and stream-output children need
//! neither: they read every frame from their parent through a pipe or a
//! loopback socket, so a killed parent ends their input and they exit.
//! This receiver's child is the only one that reads from the network, and
//! so the only one that could wait forever on its own.)
//!
//! Credentials in the stream URL never leave the receiver: the last error is
//! scrubbed of every URL secret and bounded, and [`NetworkInputConfig`]'s
//! `Debug` output redacts the URL. The URL is passed to the child on its
//! command line, as `ffmpeg` requires.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_types::FrameRate;
use fm_types::network_stream::{
    MAX_STREAM_ERROR_BYTES, NetworkStreamConfig, NetworkStreamError, NetworkStreamSettings,
    NetworkStreamState, NetworkStreamStatus, RtspTransport, StallSlate, StreamProtocol,
    redact_stream_url, scrub_stream_text, validate_stream_url,
};

use crate::config::Executable;

/// Default cap on the decoded pictures one receiver queues.
pub const DEFAULT_MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
/// Most audio channels a receiver decodes.
pub const MAX_NETWORK_AUDIO_CHANNELS: u16 = 8;
/// Extra time a connection has to deliver its first picture beyond the
/// stream timeout (probing, playlist fetch, and decoder start).
pub const CONNECT_GRACE: Duration = Duration::from_secs(5);

const AUDIO_HEADER_LIMIT: usize = 8 * 1024;
const BITRATE_WINDOW: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);
const AUDIO_READ_TIMEOUT: Duration = Duration::from_millis(200);
const TOKEN_BYTES: usize = 16;
const MAX_STDERR_LINE: u64 = 4 * 1024;

/// One network stream receiver's configuration.
#[derive(Clone)]
pub struct NetworkInputConfig {
    pub ffmpeg: Executable,
    /// The stream URL, credentials included.
    pub url: String,
    pub settings: NetworkStreamSettings,
    /// Program picture size every decoded picture is scaled to.
    pub width: u32,
    pub height: u32,
    /// Program frame rate: one picture per program frame.
    pub frame_rate: FrameRate,
    /// Program audio sample rate and channel count.
    pub sample_rate: u32,
    pub channels: u16,
    /// Cap on the queued decoded pictures, in bytes.
    pub max_buffer_bytes: usize,
}

impl NetworkInputConfig {
    /// A receiver of `stream` at `width`×`height` and `frame_rate`, with
    /// 48 kHz stereo audio and the default buffer cap.
    #[must_use]
    pub fn new(
        stream: &NetworkStreamConfig,
        width: u32,
        height: u32,
        frame_rate: FrameRate,
    ) -> Self {
        Self {
            ffmpeg: Executable::SearchPath,
            url: stream.url.clone(),
            settings: stream.settings,
            width,
            height,
            frame_rate,
            sample_rate: 48_000,
            channels: 2,
            max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
        }
    }

    fn frame_bytes(&self) -> Option<usize> {
        usize::try_from(self.width)
            .ok()?
            .checked_mul(usize::try_from(self.height).ok()?)?
            .checked_mul(4)
    }

    /// The jitter buffer's bounds for this configuration.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkInputError::InvalidFormat`] for an empty picture.
    pub fn buffer_bounds(&self) -> Result<BufferBounds, NetworkInputError> {
        let frame_bytes = self
            .frame_bytes()
            .filter(|bytes| *bytes > 0)
            .ok_or(NetworkInputError::InvalidFormat)?;
        let rate = self.frame_rate;
        let wanted = (u64::from(self.settings.jitter_buffer_ms) * u64::from(rate.numerator()))
            .div_ceil(1_000 * u64::from(rate.denominator()))
            .max(1);
        let wanted = usize::try_from(wanted).unwrap_or(usize::MAX);
        let affordable = (self.max_buffer_bytes / frame_bytes).max(2);
        let capacity = wanted.saturating_mul(2).min(affordable);
        Ok(BufferBounds {
            target_frames: wanted.min(capacity / 2).max(1),
            capacity_frames: capacity,
        })
    }
}

impl fmt::Debug for NetworkInputConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkInputConfig")
            .field("ffmpeg", &self.ffmpeg)
            .field("url", &redact_stream_url(&self.url))
            .field("settings", &self.settings)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frame_rate", &self.frame_rate)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("max_buffer_bytes", &self.max_buffer_bytes)
            .finish()
    }
}

/// The jitter buffer's bounds, in program frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferBounds {
    /// Frames a connection buffers before it goes live.
    pub target_frames: usize,
    /// Most frames the buffer holds; a full buffer drops its oldest.
    pub capacity_frames: usize,
}

/// Why a receiver cannot start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkInputError {
    /// The stream URL or settings are invalid.
    InvalidStream(NetworkStreamError),
    /// The picture size, rate, sample rate, or channel count is unusable.
    InvalidFormat,
    /// The configured `ffmpeg` executable is not an absolute file path.
    InvalidExecutable,
    /// The receiver's threads or loopback listener could not be created.
    Resources,
}

impl fmt::Display for NetworkInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStream(error) => write!(formatter, "invalid network stream: {error}"),
            Self::InvalidFormat => formatter.write_str("unusable network stream output format"),
            Self::InvalidExecutable => formatter.write_str("invalid ffmpeg executable"),
            Self::Resources => formatter.write_str("network stream receiver resources unavailable"),
        }
    }
}

impl std::error::Error for NetworkInputError {}

/// What a pull shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Picture {
    /// A new RGBA picture, `width * 4` bytes per row.
    New(Vec<u8>),
    /// Keep showing the current picture.
    Hold,
    /// Show black (the stall slate, once per stall).
    Black,
}

/// One program frame of a stream.
#[derive(Clone, Debug, PartialEq)]
pub struct PulledFrame {
    pub picture: Picture,
    /// Planar audio, one plane per channel, exactly this frame's samples.
    pub audio: Vec<Vec<f32>>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // independent receive-loop flags
struct Inner {
    shutdown: bool,
    /// Bumped per connection; a reader of an older connection stops.
    generation: u64,
    frames: VecDeque<Vec<u8>>,
    audio: Vec<VecDeque<f32>>,
    /// The stream has no audio: connect without an audio output.
    audio_absent: bool,
    /// The child refused an output without streams.
    saw_empty_output: bool,
    state: NetworkStreamState,
    reconnect_at: Option<Instant>,
    last_data: Option<Instant>,
    /// The current connection delivered a picture.
    delivered: bool,
    empty_pulls: usize,
    slate_shown: bool,
    /// Audio gain reached at the end of the last pull (0 silent, 1 full).
    gain: f32,
    last_samples: Vec<f32>,
    pulled: u64,
    dropped_frames: u64,
    late_frames: u64,
    reconnects: u32,
    bitrate_bps: u64,
    window_bytes: u64,
    window_start: Option<Instant>,
    last_error: Option<String>,
}

impl Inner {
    fn roll_bitrate(&mut self, now: Instant) {
        let start = *self.window_start.get_or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= BITRATE_WINDOW {
            let nanos = elapsed.as_nanos().max(1);
            self.bitrate_bps =
                u64::try_from(u128::from(self.window_bytes) * 8 * 1_000_000_000 / nanos)
                    .unwrap_or(u64::MAX);
            self.window_bytes = 0;
            self.window_start = Some(now);
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Fixed per-receiver values every thread reads.
struct Plan {
    config: NetworkInputConfig,
    executable: OsString,
    protocol: StreamProtocol,
    bounds: BufferBounds,
    frame_bytes: usize,
    channels: usize,
    token: String,
}

impl Plan {
    /// Audio samples of program frame `n`: `[S(n), S(n + 1))` with
    /// `S(k) = floor(k * rate * den / num)`.
    fn samples_of(&self, n: u64) -> usize {
        let at = |k: u64| {
            u128::from(k)
                * u128::from(self.config.sample_rate)
                * u128::from(self.config.frame_rate.denominator())
                / u128::from(self.config.frame_rate.numerator()).max(1)
        };
        usize::try_from(at(n + 1) - at(n)).unwrap_or(0)
    }

    /// Most audio samples queued per channel: one more frame than the
    /// picture capacity.
    fn audio_capacity(&self) -> usize {
        self.samples_of(0)
            .saturating_add(1)
            .saturating_mul(self.bounds.capacity_frames.saturating_add(1))
    }

    fn stall_after(&self) -> usize {
        (self.bounds.target_frames / 2).max(2)
    }
}

/// A running network stream receiver; dropping it stops its child.
pub struct NetworkReceiver {
    shared: Arc<Shared>,
    plan: Arc<Plan>,
    supervisor: Option<JoinHandle<()>>,
}

impl fmt::Debug for NetworkReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkReceiver")
            .field("config", &self.plan.config)
            .field("bounds", &self.plan.bounds)
            .finish_non_exhaustive()
    }
}

impl NetworkReceiver {
    /// Validates `config` and starts connecting.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkInputError`] for an invalid stream or output format,
    /// an unusable executable, or missing thread or randomness resources.
    /// A stream that does not connect is not an error: the receiver reports
    /// it through [`Self::status`] and keeps reconnecting.
    pub fn start(config: NetworkInputConfig) -> Result<Self, NetworkInputError> {
        let protocol =
            validate_stream_url(&config.url).map_err(NetworkInputError::InvalidStream)?;
        config
            .settings
            .validate()
            .map_err(NetworkInputError::InvalidStream)?;
        if config.settings.transport != RtspTransport::Auto && protocol != StreamProtocol::Rtsp {
            return Err(NetworkInputError::InvalidStream(
                NetworkStreamError::TransportNotRtsp,
            ));
        }
        if config.sample_rate == 0
            || config.channels == 0
            || config.channels > MAX_NETWORK_AUDIO_CHANNELS
            || config.width == 0
            || config.height == 0
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
        {
            return Err(NetworkInputError::InvalidFormat);
        }
        let bounds = config.buffer_bounds()?;
        let frame_bytes = config
            .frame_bytes()
            .ok_or(NetworkInputError::InvalidFormat)?;
        let executable = receiver_executable(&config.ffmpeg)?;
        let channels = usize::from(config.channels);
        let plan = Arc::new(Plan {
            executable,
            protocol,
            bounds,
            frame_bytes,
            channels,
            token: random_token()?,
            config,
        });
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                state: NetworkStreamState::Connecting,
                audio: vec![VecDeque::new(); channels],
                last_samples: vec![0.0; channels],
                // The consumer starts on a blank picture: no black slate is
                // due before the first stall.
                slate_shown: true,
                ..Inner::default()
            }),
            wake: Condvar::new(),
        });
        let supervisor = {
            let shared = Arc::clone(&shared);
            let plan = Arc::clone(&plan);
            thread::Builder::new()
                .name("freemix-network-input".into())
                .spawn(move || supervise(&shared, &plan))
                .map_err(|_| NetworkInputError::Resources)?
        };
        Ok(Self {
            shared,
            plan,
            supervisor: Some(supervisor),
        })
    }

    /// The jitter buffer's bounds.
    #[must_use]
    pub fn bounds(&self) -> BufferBounds {
        self.plan.bounds
    }

    /// Takes the next program frame: never blocks on the network.
    #[must_use]
    pub fn pull(&self) -> PulledFrame {
        let plan = &*self.plan;
        let mut inner = self.shared.lock();
        let samples = plan.samples_of(inner.pulled);
        inner.pulled = inner.pulled.wrapping_add(1);
        let mut picture = None;
        let mut taken = 0;
        let mut planes = vec![vec![0.0_f32; samples]; plan.channels];
        if inner.state == NetworkStreamState::Live {
            if let Some(frame) = inner.frames.pop_front() {
                inner.empty_pulls = 0;
                inner.slate_shown = false;
                picture = Some(Picture::New(frame));
                taken = samples.min(inner.audio.first().map_or(0, VecDeque::len));
                for (plane, queue) in planes.iter_mut().zip(&mut inner.audio) {
                    for (sample, value) in plane.iter_mut().zip(queue.drain(..taken)) {
                        *sample = value;
                    }
                }
            } else {
                inner.late_frames = inner.late_frames.saturating_add(1);
                inner.empty_pulls += 1;
                if inner.empty_pulls >= plan.stall_after() {
                    inner.state = NetworkStreamState::Stalled;
                }
            }
        }
        let picture = picture.unwrap_or_else(|| {
            let live = inner.state == NetworkStreamState::Live;
            if !live && plan.config.settings.stall == StallSlate::Black && !inner.slate_shown {
                inner.slate_shown = true;
                Picture::Black
            } else {
                Picture::Hold
            }
        });
        shape_audio(&mut inner, &mut planes, taken);
        PulledFrame {
            picture,
            audio: planes,
        }
    }

    /// The receiver's status now.
    #[must_use]
    pub fn status(&self) -> NetworkStreamStatus {
        let plan = &*self.plan;
        let inner = self.shared.lock();
        let state = match inner.state {
            NetworkStreamState::Reconnecting { attempt, .. } => {
                let next_in = inner.reconnect_at.map_or(0, |at| {
                    u32::try_from(at.saturating_duration_since(Instant::now()).as_millis())
                        .unwrap_or(u32::MAX)
                });
                NetworkStreamState::Reconnecting {
                    attempt,
                    next_in_ms: next_in,
                }
            }
            state => state,
        };
        let rate = plan.config.frame_rate;
        let buffered = u64::try_from(inner.frames.len()).unwrap_or(u64::MAX);
        let buffer_ms = buffered
            .saturating_mul(1_000)
            .saturating_mul(u64::from(rate.denominator()))
            / u64::from(rate.numerator()).max(1);
        NetworkStreamStatus {
            state,
            bitrate_bps: inner.bitrate_bps,
            buffer_ms: u32::try_from(buffer_ms).unwrap_or(u32::MAX),
            dropped_frames: inner.dropped_frames,
            late_frames: inner.late_frames,
            reconnects: inner.reconnects,
            last_error: inner.last_error.clone(),
        }
    }

    /// Pictures queued now (at most the buffer capacity).
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        self.shared.lock().frames.len()
    }
}

impl Drop for NetworkReceiver {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(supervisor) = self.supervisor.take() {
            // Bounded: the supervisor kills its child within one poll.
            let _ = supervisor.join();
        }
    }
}

/// Fades the frame's audio in after silence, and out to silence where the
/// queue ran short, so a stall or recovery never clicks.
fn shape_audio(inner: &mut Inner, planes: &mut [Vec<f32>], taken: usize) {
    let samples = planes.first().map_or(0, Vec::len);
    if taken > 0 && inner.gain < 1.0 {
        let start = inner.gain;
        #[allow(clippy::cast_precision_loss)]
        let span = taken as f32;
        for plane in planes.iter_mut() {
            for (index, sample) in plane[..taken].iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let ramp = start + (1.0 - start) * (index as f32 + 1.0) / span;
                *sample *= ramp;
            }
        }
    }
    if taken > 0 {
        inner.gain = 1.0;
        for (last, plane) in inner.last_samples.iter_mut().zip(planes.iter()) {
            *last = plane[taken - 1];
        }
    }
    if taken < samples {
        // Ran short: ramp the last value heard down to silence.
        #[allow(clippy::cast_precision_loss)]
        let span = (samples - taken) as f32;
        for (last, plane) in inner.last_samples.iter_mut().zip(planes.iter_mut()) {
            let from = *last * inner.gain;
            for (index, sample) in plane[taken..].iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let ramp = 1.0 - (index as f32 + 1.0) / span;
                *sample = from * ramp;
            }
            *last = 0.0;
        }
        inner.gain = 0.0;
    }
}

fn receiver_executable(executable: &Executable) -> Result<OsString, NetworkInputError> {
    match executable {
        Executable::SearchPath => Ok(OsString::from("ffmpeg")),
        Executable::Explicit(path) if path.is_absolute() => fs::canonicalize(path)
            .ok()
            .filter(|path| path.is_file())
            .map(std::path::PathBuf::into_os_string)
            .ok_or(NetworkInputError::InvalidExecutable),
        Executable::Explicit(_) => Err(NetworkInputError::InvalidExecutable),
    }
}

fn random_token() -> Result<String, NetworkInputError> {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| NetworkInputError::Resources)?;
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

/// The child's arguments: the input, then the video, optional audio, and
/// bitrate outputs.
fn arguments(plan: &Plan, audio_port: Option<u16>) -> Vec<OsString> {
    let config = &plan.config;
    let micros = (u64::from(config.settings.timeout_ms) * 1_000).to_string();
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostats",
        "-analyzeduration",
        "1000000",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    match plan.protocol {
        StreamProtocol::Rtsp => {
            match config.settings.transport {
                RtspTransport::Auto => {}
                RtspTransport::Tcp => args.extend(["-rtsp_transport".into(), "tcp".into()]),
                RtspTransport::Udp => args.extend(["-rtsp_transport".into(), "udp".into()]),
            }
            args.extend(["-timeout".into(), micros]);
        }
        StreamProtocol::Http | StreamProtocol::Hls => {
            args.extend(["-re".into(), "-rw_timeout".into(), micros]);
        }
        StreamProtocol::Tcp => args.extend(["-rw_timeout".into(), micros]),
        StreamProtocol::Rtp | StreamProtocol::Udp => {
            // A listener that hears nothing gives up on its own, so a child
            // the receiver can no longer kill frees its port in bounded time
            // even where no parent-death signal exists.
            let grace = u64::try_from(CONNECT_GRACE.as_micros()).unwrap_or(u64::MAX);
            let micros = (u64::from(config.settings.timeout_ms) * 1_000).saturating_add(grace);
            args.extend(["-timeout".into(), micros.to_string()]);
        }
    }
    args.extend([
        "-protocol_whitelist".into(),
        protocol_whitelist(plan.protocol).into(),
        "-i".into(),
        child_url(&config.url),
    ]);
    let rate = config.frame_rate;
    args.extend([
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        format!(
            "scale={}:{},fps=fps={}/{}:start_time=0,format=rgba",
            config.width,
            config.height,
            rate.numerator(),
            rate.denominator()
        ),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    if let Some(port) = audio_port {
        args.extend([
            "-map".into(),
            "0:a:0?".into(),
            "-af".into(),
            format!("aresample={}:async=1:first_pts=0", config.sample_rate),
            "-ac".into(),
            config.channels.to_string(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "-f".into(),
            "f32le".into(),
            "-chunked_post".into(),
            "0".into(),
            "-method".into(),
            "POST".into(),
            format!("http://127.0.0.1:{port}/{}", plan.token),
        ]);
    }
    args.extend([
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "framecrc".into(),
        "pipe:2".into(),
    ]);
    args.into_iter().map(OsString::from).collect()
}

/// The only `ffmpeg` protocols the input of a `protocol` stream may open.
///
/// Without an explicit list a stream inherits its first protocol's default
/// list, which for HTTP still admits `rtp`, `udp`, `httpproxy`, and `data`:
/// a served session description, playlist, or redirect could then open
/// local ports or other protocols. `file`, `concat`, `pipe`, and the rest
/// are never on any list.
const fn protocol_whitelist(protocol: StreamProtocol) -> &'static str {
    match protocol {
        StreamProtocol::Rtsp => "rtsp,rtsps,rtp,srtp,udp,tcp,tls",
        StreamProtocol::Rtp => "rtp,udp",
        StreamProtocol::Udp => "udp",
        StreamProtocol::Tcp => "tcp",
        StreamProtocol::Http => "http,https,tcp,tls",
        // AES-128 segments are read through `crypto`.
        StreamProtocol::Hls => "http,https,tcp,tls,crypto",
    }
}

/// `url` as the child receives it: with its scheme in lowercase, since
/// `ffmpeg` matches protocol names case-sensitively.
fn child_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{}://{rest}", scheme.to_ascii_lowercase()),
        None => url.to_owned(),
    }
}

/// A `setpriv` that can give the child a parent-death signal, found once.
///
/// The workspace forbids `unsafe`, so the receiver cannot call
/// `prctl(PR_SET_PDEATHSIG)` between `fork` and `exec` itself; util-linux's
/// `setpriv --pdeathsig KILL` sets it and then execs `ffmpeg` in the same
/// process. The kernel signals the child when the thread that spawned it
/// exits, and the receiver's supervisor thread outlives every child it
/// spawns, so the signal only fires once the receiver's process is gone,
/// however it died. Without `setpriv` (macOS, or a minimal Linux) the
/// child's own input timeouts bound how long it can outlive a killed
/// parent instead.
#[cfg(target_os = "linux")]
fn parent_death_wrapper() -> Option<&'static std::path::Path> {
    static WRAPPER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    WRAPPER
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            let setpriv = std::env::split_paths(&path)
                .map(|directory| directory.join("setpriv"))
                .find(|candidate| candidate.is_absolute() && candidate.is_file())?;
            // Older `setpriv` builds lack `--pdeathsig`: prove it works.
            let works = Command::new(&setpriv)
                .args(["--pdeathsig", "KILL", "--"])
                .arg(&setpriv)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            works.then_some(setpriv)
        })
        .as_deref()
}

fn command(plan: &Plan, audio_port: Option<u16>) -> Command {
    #[cfg(target_os = "linux")]
    let mut command = match parent_death_wrapper() {
        Some(setpriv) => {
            let mut command = Command::new(setpriv);
            command
                .args(["--pdeathsig", "KILL", "--"])
                .arg(&plan.executable);
            command
        }
        None => Command::new(&plan.executable),
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = Command::new(&plan.executable);
    command
        .args(arguments(plan, audio_port))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    #[cfg(windows)]
    let retained: &[&str] = &["PATH", "SystemRoot", "WINDIR", "PATHEXT"];
    #[cfg(not(windows))]
    let retained: &[&str] = &["PATH"];
    for name in retained {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("LC_ALL", "C");
    command
}

/// How one connection ended.
enum Ended {
    Shutdown,
    /// The connection ended; `delivered` if it delivered a picture.
    Dropped {
        delivered: bool,
    },
}

/// Connects, watches, and reconnects until shutdown or the policy gives up.
fn supervise(shared: &Arc<Shared>, plan: &Arc<Plan>) {
    let policy = plan.config.settings.reconnect;
    let mut attempt: u32 = 0;
    loop {
        match connect_once(shared, plan) {
            Ended::Shutdown => return,
            Ended::Dropped { delivered } => {
                if delivered {
                    attempt = 0;
                }
            }
        }
        {
            let mut inner = shared.lock();
            if inner.saw_empty_output && !inner.audio_absent && !inner.delivered {
                // No audio stream: connect again at once, without audio.
                inner.audio_absent = true;
                inner.saw_empty_output = false;
                inner.last_error = None;
                continue;
            }
        }
        attempt = attempt.saturating_add(1);
        let mut inner = shared.lock();
        // A source may come back with audio: every reconnect asks for it
        // again (a video-only source costs one refused attempt per
        // reconnect, retried at once without audio).
        inner.audio_absent = false;
        inner.frames.clear();
        inner.bitrate_bps = 0;
        if !policy.allows(attempt) {
            inner.state = NetworkStreamState::Failed;
            inner.reconnect_at = None;
            while !inner.shutdown {
                inner = shared
                    .wake
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            return;
        }
        inner.reconnects = inner.reconnects.saturating_add(1);
        let at = Instant::now() + policy.delay(attempt);
        inner.state = NetworkStreamState::Reconnecting {
            attempt,
            next_in_ms: 0,
        };
        inner.reconnect_at = Some(at);
        loop {
            if inner.shutdown {
                return;
            }
            let now = Instant::now();
            if now >= at {
                break;
            }
            inner = shared
                .wake
                .wait_timeout(inner, at - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        inner.reconnect_at = None;
    }
}

/// Runs one connection to its end.
fn connect_once(shared: &Arc<Shared>, plan: &Arc<Plan>) -> Ended {
    let (generation, audio_absent) = {
        let mut inner = shared.lock();
        if inner.shutdown {
            return Ended::Shutdown;
        }
        inner.generation = inner.generation.wrapping_add(1);
        inner.frames.clear();
        for queue in &mut inner.audio {
            queue.clear();
        }
        inner.delivered = false;
        inner.last_data = None;
        inner.empty_pulls = 0;
        inner.saw_empty_output = false;
        inner.window_bytes = 0;
        inner.window_start = None;
        (inner.generation, inner.audio_absent)
    };
    let listener = if audio_absent {
        None
    } else {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => Some(listener),
            Err(error) => {
                shared.lock().last_error = Some(format!("audio listener: {:?}", error.kind()));
                return Ended::Dropped { delivered: false };
            }
        }
    };
    let port = listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|address| address.port());
    let mut child = match command(plan, port).spawn() {
        Ok(child) => child,
        Err(error) => {
            shared.lock().last_error = Some(match error.kind() {
                io::ErrorKind::NotFound => "ffmpeg unavailable".to_owned(),
                kind => format!("ffmpeg spawn failed: {kind:?}"),
            });
            return Ended::Dropped { delivered: false };
        }
    };
    let started = Instant::now();
    let mut readers = Vec::with_capacity(3);
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader("video", shared, plan, move |shared, plan| {
            read_video(shared, plan, generation, stdout);
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader("log", shared, plan, move |shared, plan| {
            read_log(shared, plan, generation, stderr);
        }));
    }
    if let Some(listener) = listener {
        readers.push(spawn_reader("audio", shared, plan, move |shared, plan| {
            read_audio(shared, plan, generation, &listener);
        }));
    }
    let ended = watch(shared, plan, &mut child, started);
    let _ = child.kill();
    let _ = child.wait();
    {
        let mut inner = shared.lock();
        // Readers of this connection stop pushing.
        inner.generation = inner.generation.wrapping_add(1);
    }
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }
    ended
}

fn spawn_reader(
    name: &str,
    shared: &Arc<Shared>,
    plan: &Arc<Plan>,
    body: impl FnOnce(&Shared, &Plan) + Send + 'static,
) -> Option<JoinHandle<()>> {
    let shared = Arc::clone(shared);
    let plan = Arc::clone(plan);
    thread::Builder::new()
        .name(format!("freemix-network-{name}"))
        .spawn(move || body(&shared, &plan))
        .ok()
}

/// Watches one connection until it ends.
fn watch(shared: &Shared, plan: &Plan, child: &mut Child, started: Instant) -> Ended {
    let timeout = Duration::from_millis(u64::from(plan.config.settings.timeout_ms));
    let mut inner = shared.lock();
    loop {
        if inner.shutdown {
            return Ended::Shutdown;
        }
        let now = Instant::now();
        inner.roll_bitrate(now);
        let delivered = inner.delivered;
        let limit = if delivered {
            timeout
        } else {
            timeout + CONNECT_GRACE
        };
        let since = inner.last_data.unwrap_or(started);
        if now.saturating_duration_since(since) >= limit {
            inner.last_error = Some(format!("no media for {} ms", limit.as_millis()));
            return Ended::Dropped { delivered };
        }
        drop(inner);
        if let Ok(Some(status)) = child.try_wait() {
            // Let the log reader record the child's last words first.
            thread::sleep(POLL);
            let mut inner = shared.lock();
            if inner.last_error.is_none() {
                inner.last_error = Some(if status.success() {
                    "stream ended".to_owned()
                } else {
                    format!("ffmpeg exited ({status})")
                });
            }
            return Ended::Dropped {
                delivered: inner.delivered,
            };
        }
        inner = shared.lock();
        if !inner.shutdown {
            inner = shared
                .wake
                .wait_timeout(inner, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

fn read_video(shared: &Shared, plan: &Plan, generation: u64, mut stdout: ChildStdout) {
    loop {
        let mut frame = vec![0_u8; plan.frame_bytes];
        if stdout.read_exact(&mut frame).is_err() {
            return;
        }
        let mut inner = shared.lock();
        if inner.generation != generation || inner.shutdown {
            return;
        }
        admit_picture(&mut inner, plan, frame);
    }
}

/// Queues one decoded picture: drops the oldest (and its audio) from a full
/// buffer, and goes live once the buffer holds its target.
fn admit_picture(inner: &mut Inner, plan: &Plan, frame: Vec<u8>) {
    inner.last_data = Some(Instant::now());
    inner.delivered = true;
    inner.frames.push_back(frame);
    if inner.frames.len() > plan.bounds.capacity_frames {
        inner.frames.pop_front();
        inner.dropped_frames = inner.dropped_frames.saturating_add(1);
        // The dropped picture's audio goes with it.
        let samples = plan.samples_of(0);
        for queue in &mut inner.audio {
            let drop = samples.min(queue.len());
            queue.drain(..drop);
        }
    }
    match inner.state {
        NetworkStreamState::Connecting
        | NetworkStreamState::Reconnecting { .. }
        | NetworkStreamState::Stalled
        | NetworkStreamState::Idle => {
            inner.state = NetworkStreamState::Buffering;
            inner.last_error = None;
        }
        _ => {}
    }
    if inner.state == NetworkStreamState::Buffering
        && inner.frames.len() >= plan.bounds.target_frames
    {
        inner.state = NetworkStreamState::Live;
        inner.empty_pulls = 0;
        align_audio(inner, plan);
    }
}

/// Drops the audio queued beyond the queued pictures' samples, oldest
/// first, so the next pull's audio is the audio of its picture. Audio that
/// kept arriving while the pictures stalled would otherwise lag the
/// pictures for the rest of the connection.
fn align_audio(inner: &mut Inner, plan: &Plan) {
    let pictures = u64::try_from(inner.frames.len()).unwrap_or(u64::MAX);
    let wanted = (0..pictures)
        .map(|offset| plan.samples_of(inner.pulled.wrapping_add(offset)))
        .fold(0_usize, usize::saturating_add);
    for queue in &mut inner.audio {
        let excess = queue.len().saturating_sub(wanted);
        queue.drain(..excess);
    }
}

fn read_log(shared: &Shared, plan: &Plan, generation: u64, stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut reader)
            .take(MAX_STDERR_LINE)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let mut inner = shared.lock();
        if inner.generation != generation {
            return;
        }
        if let Some(size) = framecrc_size(text) {
            inner.window_bytes = inner.window_bytes.saturating_add(size);
            inner.roll_bitrate(Instant::now());
            continue;
        }
        if text.contains("does not contain any stream") {
            inner.saw_empty_output = true;
        }
        // The child echoes the URL it was given; the secrets are the same.
        let scrubbed = scrub_stream_text(text, &child_url(&plan.config.url));
        if !scrubbed.is_empty() {
            debug_assert!(scrubbed.len() <= MAX_STREAM_ERROR_BYTES);
            inner.last_error = Some(scrubbed);
        }
    }
}

/// The packet size of one frame CRC line (`stream, dts, pts, duration,
/// size, crc`).
fn framecrc_size(line: &str) -> Option<u64> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 6 || fields[0].parse::<u32>().is_err() || !fields[5].starts_with("0x") {
        return None;
    }
    fields[4].parse().ok()
}

fn read_audio(shared: &Shared, plan: &Plan, generation: u64, listener: &TcpListener) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    let current = || {
        let inner = shared.lock();
        inner.generation == generation && !inner.shutdown
    };
    loop {
        if !current() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if receive_audio(shared, plan, generation, stream, &current) {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(_) => return,
        }
    }
}

/// Receives one audio upload. Returns whether it was the child's (an
/// unauthenticated connection is dropped and the listener keeps waiting).
fn receive_audio(
    shared: &Shared,
    plan: &Plan,
    generation: u64,
    mut stream: TcpStream,
    current: &dyn Fn() -> bool,
) -> bool {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(AUDIO_READ_TIMEOUT)).is_err()
    {
        return false;
    }
    let mut buffer = Vec::with_capacity(AUDIO_HEADER_LIMIT);
    let mut chunk = [0_u8; 16 * 1024];
    let body_start = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if buffer.len() > AUDIO_HEADER_LIMIT || !current() {
            return false;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return false,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if is_timeout(&error) => {}
            Err(_) => return false,
        }
    };
    let expected = format!("POST /{} ", plan.token);
    if !buffer.starts_with(expected.as_bytes()) {
        return false;
    }
    let mut pending = buffer.split_off(body_start);
    let frame = plan.channels * 4;
    loop {
        let whole = pending.len() / frame * frame;
        if whole > 0 {
            let mut inner = shared.lock();
            if inner.generation != generation {
                return true;
            }
            inner.last_data = Some(Instant::now());
            for sample in pending[..whole].chunks_exact(frame) {
                for (channel, bytes) in sample.as_chunks::<4>().0.iter().enumerate() {
                    let value = f32::from_le_bytes(*bytes);
                    inner.audio[channel].push_back(if value.is_finite() {
                        value.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    });
                }
            }
            let capacity = plan.audio_capacity();
            for queue in &mut inner.audio {
                let excess = queue.len().saturating_sub(capacity);
                queue.drain(..excess);
            }
            drop(inner);
            pending.drain(..whole);
        }
        if !current() {
            return true;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return true,
            Ok(read) => pending.extend_from_slice(&chunk[..read]),
            Err(error) if is_timeout(&error) => {}
            Err(_) => return true,
        }
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use fm_types::network_stream::{GiveUp, ReconnectPolicy};

    use super::*;

    fn plan(url: &str, transport: RtspTransport) -> Plan {
        let mut stream = NetworkStreamConfig::new(url);
        stream.settings.transport = transport;
        let config = NetworkInputConfig::new(&stream, 64, 36, FrameRate::new(25, 1).unwrap());
        Plan {
            executable: "ffmpeg".into(),
            protocol: validate_stream_url(url).unwrap(),
            bounds: config.buffer_bounds().unwrap(),
            frame_bytes: config.frame_bytes().unwrap(),
            channels: 2,
            token: "t0k3n".into(),
            config,
        }
    }

    fn args(plan: &Plan, port: Option<u16>) -> Vec<String> {
        arguments(plan, port)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn rtsp_arguments_carry_the_transport_hint_and_socket_timeout() {
        let args = args(
            &plan("rtsp://cam.test:554/live", RtspTransport::Tcp),
            Some(4000),
        );
        let joined = args.join(" ");
        assert!(
            joined.contains(
                "-rtsp_transport tcp -timeout 5000000 \
                 -protocol_whitelist rtsp,rtsps,rtp,srtp,udp,tcp,tls -i rtsp://cam.test:554/live"
            ),
            "{joined}"
        );
        assert!(joined.contains("http://127.0.0.1:4000/t0k3n"));
        assert!(joined.contains("scale=64:36,fps=fps=25/1:start_time=0,format=rgba"));
        assert!(!args.iter().any(|arg| arg == "-re"));
    }

    #[test]
    fn every_scheme_opens_only_its_own_protocols() {
        for (url, list) in [
            ("rtsp://cam.test/live", "rtsp,rtsps,rtp,srtp,udp,tcp,tls"),
            ("rtp://239.0.0.1:5004", "rtp,udp"),
            ("udp://@:9000", "udp"),
            ("tcp://encoder.test:9000", "tcp"),
            ("https://cdn.test/live.ts", "http,https,tcp,tls"),
            (
                "http://cdn.test/live/index.m3u8",
                "http,https,tcp,tls,crypto",
            ),
        ] {
            let args = args(&plan(url, RtspTransport::Auto), Some(4000));
            let at = args
                .iter()
                .position(|arg| arg == "-protocol_whitelist")
                .unwrap_or_else(|| panic!("{url}: no whitelist in {args:?}"));
            assert_eq!(args[at + 1], list, "{url}");
            assert_eq!(args[at + 2], "-i", "{url}: the list must bind the input");
            for forbidden in ["file", "concat", "pipe", "data", "subfile"] {
                assert!(
                    !list.split(',').any(|protocol| protocol == forbidden),
                    "{url} admits {forbidden}"
                );
            }
        }
    }

    #[test]
    fn a_listening_child_gives_up_on_silence_by_itself() {
        // 5 s stream timeout plus the 5 s connect grace.
        for url in ["udp://@:9000", "rtp://239.0.0.1:5004"] {
            let joined = args(&plan(url, RtspTransport::Auto), None).join(" ");
            assert!(
                joined.contains("-timeout 10000000 -protocol_whitelist"),
                "{joined}"
            );
        }
    }

    #[test]
    fn the_child_receives_the_scheme_in_lowercase() {
        // `ffmpeg` matches protocol names case-sensitively: `RTSP://` would
        // never connect, and the receiver would reconnect forever.
        let args = args(
            &plan("RTSPS://u:p@Cam.Test/Live", RtspTransport::Auto),
            None,
        );
        assert!(
            args.iter().any(|arg| arg == "rtsps://u:p@Cam.Test/Live"),
            "{args:?}"
        );
    }

    #[test]
    fn paced_protocols_read_at_native_rate_and_audio_is_optional() {
        let args = args(
            &plan("https://cdn.test/live/index.m3u8", RtspTransport::Auto),
            None,
        );
        assert!(args.iter().any(|arg| arg == "-re"));
        assert!(!args.iter().any(|arg| arg == "f32le"));
        assert!(args.iter().any(|arg| arg == "framecrc"));
    }

    #[test]
    fn buffer_bounds_follow_the_jitter_target_and_the_byte_cap() {
        let mut stream = NetworkStreamConfig::new("udp://@:9000");
        stream.settings.jitter_buffer_ms = 300;
        let mut config =
            NetworkInputConfig::new(&stream, 64, 36, FrameRate::new(30_000, 1_001).unwrap());
        assert_eq!(
            config.buffer_bounds().unwrap(),
            BufferBounds {
                target_frames: 9,
                capacity_frames: 18
            }
        );
        config.max_buffer_bytes = 64 * 36 * 4 * 6;
        assert_eq!(
            config.buffer_bounds().unwrap(),
            BufferBounds {
                target_frames: 3,
                capacity_frames: 6
            }
        );
    }

    #[test]
    fn framecrc_lines_yield_packet_sizes() {
        assert_eq!(
            framecrc_size("0,          0,          0,     3600,     4512, 0x1f2e3d4c"),
            Some(4512)
        );
        assert_eq!(framecrc_size("[udp @ 0x55] Connection refused"), None);
        assert_eq!(framecrc_size("#tb 0: 1/90000"), None);
    }

    #[test]
    fn audio_samples_cut_exactly_on_frame_boundaries() {
        let mut stream = NetworkStreamConfig::new("udp://@:9000");
        stream.settings.reconnect = ReconnectPolicy {
            give_up: GiveUp::After(NonZeroU32::new(1).unwrap()),
            ..ReconnectPolicy::default()
        };
        let mut plan = plan("udp://@:9000", RtspTransport::Auto);
        plan.config.frame_rate = FrameRate::new(30_000, 1_001).unwrap();
        let total: usize = (0..5).map(|n| plan.samples_of(n)).sum();
        // Five NTSC frames are 8008 samples at 48 kHz.
        assert_eq!(total, 8_008);
    }

    #[test]
    fn audio_fades_out_when_it_runs_short_and_back_in() {
        let mut inner = Inner {
            audio: vec![VecDeque::new(); 1],
            last_samples: vec![0.0],
            gain: 1.0,
            ..Inner::default()
        };
        let mut planes = vec![vec![0.5; 4]];
        shape_audio(&mut inner, &mut planes, 4);
        assert_eq!(planes[0], [0.5; 4]);
        let mut planes = vec![vec![0.0; 4]];
        shape_audio(&mut inner, &mut planes, 0);
        assert_eq!(planes[0], [0.375, 0.25, 0.125, 0.0]);
        assert!(inner.gain.abs() < f32::EPSILON);
        let mut planes = vec![vec![1.0; 4]];
        shape_audio(&mut inner, &mut planes, 4);
        assert_eq!(planes[0], [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn audio_that_ran_on_through_a_stall_is_cut_back_to_the_pictures() {
        let plan = plan("udp://@:9000", RtspTransport::Auto);
        let per_frame = plan.samples_of(0);
        let mut inner = Inner {
            state: NetworkStreamState::Stalled,
            audio: vec![VecDeque::new(); 2],
            last_samples: vec![0.0; 2],
            ..Inner::default()
        };
        // The pictures stalled while audio kept arriving up to its cap; the
        // newest sample is 1.0, the older ones 0.0.
        for queue in &mut inner.audio {
            queue.extend(core::iter::repeat_n(0.0, plan.audio_capacity() - 1));
            queue.push_back(1.0);
        }
        for _ in 0..plan.bounds.target_frames {
            admit_picture(&mut inner, &plan, vec![0; plan.frame_bytes]);
        }
        assert_eq!(inner.state, NetworkStreamState::Live);
        for queue in &inner.audio {
            assert_eq!(queue.len(), plan.bounds.target_frames * per_frame);
            assert_eq!(queue.back(), Some(&1.0));
        }
    }

    #[test]
    fn debug_output_redacts_the_url() {
        let stream = NetworkStreamConfig::new("rtsp://admin:hunter2@cam.test/live");
        let config = NetworkInputConfig::new(&stream, 64, 36, FrameRate::new(25, 1).unwrap());
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("admin"));
    }
}
