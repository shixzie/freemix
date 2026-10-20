//! `FFmpeg`-backed network stream receiver (IN-015, IN-016, IN-017).
//!
//! A [`NetworkReceiver`] receives one RTSP, RTP, UDP, TCP, or HTTP MPEG-TS
//! stream, an HLS playlist, an RTMP(S) stream, or an SRT stream through a
//! supervised `ffmpeg` child, and hands
//! its consumer exactly one program frame per [`NetworkReceiver::pull`]
//! without ever blocking: the session loop that realizes Program pulls once
//! per engine frame and never waits on the network.
//!
//! # Decode
//!
//! The child decodes the stream's first video stream, scaled to the program
//! size (stretched, not letterboxed) and resampled to the program rate, as
//! raw RGBA on its standard output, and the first audio stream as
//! interleaved `f32` PCM at the program sample rate and channel count (a
//! downmix of every source channel, or one chosen stereo pair of a source of
//! up to eight channels). The
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
//! live; the queue holds at most twice the target (and at least three frames
//! more than it), and a full queue drops its oldest picture
//! (`dropped_frames`). The target and capacity are also capped so the queued
//! pictures never exceed [`NetworkInputConfig::max_buffer_bytes`].
//!
//! # Source clock drift (AU-004)
//!
//! The source runs on its own clock (the sender's, a capture card's, or a
//! sound card's crystal), a few hundred ppm off the engine's. Its audio
//! feeds a [`LiveSourceClock`]: the connection's samples are pushed with
//! their arrival time, and each pull renders exactly the samples of engine
//! frame `n` through a windowed-sinc converter whose ratio a smoothed PI
//! loop trims (within ±1000 ppm) to hold the audio backlog at the jitter
//! target, so a long show neither drifts out of lip sync nor clicks when a
//! backlog would overflow or drain. Only a backlog more than one target
//! beyond the target is skipped back (declicked), and only a source that
//! stops delivering underruns; both are counted
//! ([`NetworkReceiver::sync_telemetry`]). The pictures follow the audio: one
//! is popped per pull, and one extra is dropped, or the current one
//! repeated, whenever the picture queue's latency strays more than
//! three-quarters of a frame from the audio's. While the stream is not live
//! its audio is still pulled (and discarded), so the backlog stays at the
//! target and the drift estimate survives a stall or a reconnect.
//!
//! A live pull that finds no picture is late (`late_frames`) and repeats the
//! previous picture; a run of them stalls the stream, which buffers again
//! before it goes back on air. While it is not live (connecting, buffering,
//! stalled, reconnecting, or failed) the input shows its stall slate (the
//! last picture, or black) and its audio fades to silence over one frame
//! instead of stopping with a click, and fades back in when it recovers.
//!
//! # Listening (RTMP and SRT)
//!
//! A listener reports `listening` only once its port accepts its peer, and
//! `connecting` before that.
//!
//! An RTMP listener's port is the receiver's own: it binds the configured
//! address once and keeps it bound for its whole life, so a publisher is
//! never refused between sessions (after a session ends, or after a
//! publisher with the wrong stream key is dropped). Each accepted publisher
//! gets its own child, an `ffmpeg -listen 1` on a fresh loopback port, and
//! the receiver relays bytes between the two sockets. (`ffmpeg`'s own
//! listener accepts exactly one connection and then closes its listening
//! socket, and a second child cannot bind the port while the first holds
//! it, so a child-owned port is closed from every accept until the next
//! child binds it.) The publisher then has `timeout_ms` plus
//! [`CONNECT_GRACE`] to deliver its first picture. While one publisher is
//! on air, another's connection waits in the listen backlog until the first
//! leaves. A port the receiver cannot bind (taken) reconnects with the
//! backoff policy.
//!
//! An SRT listener or rendezvous waits for its peer without a data timeout.
//! Its child gives up after [`LISTEN_CYCLE`] without a peer (so a child the
//! receiver can no longer kill frees its port in bounded time), and the
//! receiver listens again at once without counting a reconnect attempt; it
//! does the same when a caller disconnects. An SRT listener's port is
//! `libsrt`'s, inside the child: the receiver reports `listening` once the
//! child answers an SRT induction handshake (stateless on the listener: it
//! only returns a cookie) on that port. Its port is still closed from a
//! session's end until the next child binds it, but an SRT caller
//! retransmits its handshake until its connect timeout (3 s by default), so
//! that gap delays a caller rather than refusing it. A rendezvous binds no
//! port that answers a stranger, so it reports `listening` as its child
//! starts. `ffmpeg`'s RTMP listener accepts any stream name, so the
//! receiver itself drops a publisher whose stream key is not the configured
//! one. A child that fails quickly (its port is taken) reconnects with the
//! backoff policy.
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
//! # Audio-only devices (AU-011)
//!
//! A system audio source (a `PulseAudio`/`PipeWire` source or an ALSA capture
//! device) is read as an audio-only [`DeviceInput`]: the child decodes the
//! device's audio and paces one tiny `showwaves` picture per program frame
//! from that same audio, so the jitter buffer, drift-corrected audio,
//! stall detection, and reopen policy work exactly as for a stream, and a
//! device that stops delivering stops the pictures too. A device's audio is
//! resampled without timestamp compensation, so its sample count stays on
//! the sound card's clock and the receiver's drift loop corrects it. Its audio is
//! required: the receiver never falls back to reopening it without audio.
//! The child keeps the audio-server environment (`HOME`,
//! `XDG_RUNTIME_DIR`, `PULSE_SERVER`, `PULSE_COOKIE`, and
//! `PULSE_RUNTIME_PATH`), which `PulseAudio` needs to find its server and
//! ALSA to read the user's `~/.asoundrc`.
//!
//! Credentials in the stream URL, the RTMP stream key, and the SRT
//! passphrase never leave the receiver: the last error is scrubbed of every
//! secret and bounded, and [`NetworkInputConfig`]'s `Debug` output redacts
//! them. The SRT passphrase never reaches the child's command line, which
//! every local user can read in `/proc/<pid>/cmdline`: an encrypted SRT
//! child reads its input through `ffmpeg`'s `concat` demuxer from a
//! one-entry script the receiver writes into its stdin and closes (the
//! stream URL plus each SRT option, the passphrase among them, as `option`
//! lines). That child may open only `pipe` and `srt`. Credentials in the URL
//! and the RTMP stream key are still passed on the command line, as
//! `ffmpeg` requires.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_audio::{LiveSourceClock, LiveSourceConfig, PictureAction};
use fm_types::network_stream::{
    MAX_STREAM_ERROR_BYTES, NetworkStreamConfig, NetworkStreamError, NetworkStreamSettings,
    NetworkStreamState, NetworkStreamStatus, REDACTED, RtspTransport, StallSlate, StreamMode,
    StreamProtocol, redact_stream_url, scrub_stream_text,
};

use fm_types::{FrameRate, SampleRate};

use crate::config::Executable;

pub use fm_audio::LiveSourceTelemetry;

/// Default cap on the decoded pictures one receiver queues.
pub const DEFAULT_MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
/// Most audio channels a receiver decodes.
pub const MAX_NETWORK_AUDIO_CHANNELS: u16 = 8;
/// Extra time a connection has to deliver its first picture beyond the
/// stream timeout (probing, playlist fetch, and decoder start).
pub const CONNECT_GRACE: Duration = Duration::from_secs(5);
/// How long a listening child waits for its peer before it exits and the
/// receiver listens again.
pub const LISTEN_CYCLE: Duration = Duration::from_secs(30);
/// A listening child that ran at least this long before it ended listens
/// again at once; one that ended sooner failed (its port is taken) and
/// reconnects with the backoff policy.
const LISTEN_FAILURE_WINDOW: Duration = Duration::from_secs(2);
/// How often an RTMP relay tries its child's loopback port while the child
/// starts, and how long one try may take.
const RELAY_RETRY: Duration = Duration::from_millis(10);
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// How often an SRT listener's readiness probe repeats its handshake.
const SRT_PROBE_INTERVAL: Duration = Duration::from_millis(100);

const AUDIO_HEADER_LIMIT: usize = 8 * 1024;
const BITRATE_WINDOW: Duration = Duration::from_secs(1);
/// Media a connection must receive before it publishes its first bitrate.
const BITRATE_FIRST_READING: Duration = Duration::from_millis(200);
/// Stream indexes beyond this carry no media time into the bitrate.
const BITRATE_MAX_STREAMS: usize = 16;
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
    /// A local capture device read instead of [`Self::url`] (IN-001).
    pub device: Option<DeviceInput>,
    /// SRT stream ID (IN-017).
    pub stream_id: Option<String>,
    /// RTMP stream key (IN-016), a secret.
    pub stream_key: Option<String>,
    /// SRT passphrase (IN-017), a secret.
    pub passphrase: Option<String>,
}

/// A local capture device a receiver reads instead of a stream URL
/// (IN-001): one `ffmpeg` demuxer input for the pictures and, optionally, a
/// second one for the audio.
///
/// The receiver treats the device like a stream: it buffers pictures in the
/// same bounded jitter buffer, drift-corrects its audio into the engine
/// clock (the pictures following it), and reconnects (reopens the device) when it stops delivering. An audio input
/// that keeps the device from opening is dropped: the receiver reopens the
/// device without audio.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInput {
    /// The `ffmpeg` input format, such as `v4l2`.
    pub format: String,
    /// Demuxer options given before `-i`, as name and value pairs (such as
    /// `input_format`, `video_size`, and `framerate`).
    pub options: Vec<(String, String)>,
    /// The device path, such as `/dev/video0`.
    pub path: String,
    /// The field order of interlaced pictures (`tff` or `bff`), which are
    /// deinterlaced before scaling; `None` for progressive pictures.
    pub field_order: Option<&'static str>,
    /// The audio input read beside the pictures, if any.
    pub audio: Option<DeviceAudioInput>,
    /// The device is an audio source without pictures (AU-011): `format`,
    /// `options`, and `path` name the audio input, `audio` is `None`, and
    /// the receiver paces its pictures from the audio.
    pub audio_only: bool,
    /// Fit the pictures into the program picture keeping their aspect
    /// ratio, with black bars, rather than stretching them (IN-011).
    pub letterbox: bool,
    /// Environment variables the child keeps beside `PATH`, such as
    /// `DISPLAY` and `XAUTHORITY` for an X11 screen capture (IN-011).
    pub environment: Vec<String>,
    /// A helper whose standard output becomes the child's standard input,
    /// such as a screen capture helper that writes raw pictures (IN-011).
    /// It is started before each connection and killed after it.
    pub feeder: Option<DeviceFeeder>,
}

/// A helper process feeding a [`DeviceInput`]'s child through a pipe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceFeeder {
    /// The helper executable.
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
}

/// The audio input of a [`DeviceInput`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceAudioInput {
    /// The `ffmpeg` input format, such as `alsa`.
    pub format: String,
    /// Demuxer options given before `-i`, as name and value pairs (such as
    /// `sample_rate` and `ch_layout` for raw samples read from a socket,
    /// IN-011).
    pub options: Vec<(String, String)>,
    /// The audio device, such as `hw:CARD=C920,DEV=0`.
    pub path: String,
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
            device: None,
            stream_id: stream.stream_id.clone(),
            stream_key: stream.stream_key.clone(),
            passphrase: stream.passphrase.clone(),
        }
    }

    /// The stream this receiver opens: its URL, settings, and connection
    /// fields.
    #[must_use]
    pub fn stream(&self) -> NetworkStreamConfig {
        NetworkStreamConfig {
            url: self.url.clone(),
            settings: self.settings,
            stream_id: self.stream_id.clone(),
            stream_key: self.stream_key.clone(),
            passphrase: self.passphrase.clone(),
        }
    }

    /// A receiver of the local capture `device` at `width`×`height` and
    /// `frame_rate`, with 48 kHz stereo audio, the default buffer cap, and
    /// `settings` for its buffer, reopen policy, and timeout.
    #[must_use]
    pub fn device(
        device: DeviceInput,
        settings: NetworkStreamSettings,
        width: u32,
        height: u32,
        frame_rate: FrameRate,
    ) -> Self {
        Self {
            ffmpeg: Executable::SearchPath,
            url: device.path.clone(),
            settings,
            width,
            height,
            frame_rate,
            sample_rate: 48_000,
            channels: 2,
            max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
            device: Some(device),
            stream_id: None,
            stream_key: None,
            passphrase: None,
        }
    }

    /// A receiver of the audio-only system audio source `path` read with
    /// the `ffmpeg` input `format` (such as `pulse` or `alsa`, AU-011), one
    /// 2x2 pacing picture per program frame at `frame_rate`, with 48 kHz
    /// stereo audio and `settings` for its buffer, reopen policy, and
    /// timeout.
    #[must_use]
    pub fn audio_device(
        format: impl Into<String>,
        path: impl Into<String>,
        settings: NetworkStreamSettings,
        frame_rate: FrameRate,
    ) -> Self {
        Self::device(
            DeviceInput {
                format: format.into(),
                options: Vec::new(),
                path: path.into(),
                field_order: None,
                audio: None,
                audio_only: true,
                letterbox: false,
                environment: Vec::new(),
                feeder: None,
            },
            settings,
            2,
            2,
            frame_rate,
        )
    }

    /// Whether this receiver reads an audio-only device (AU-011).
    #[must_use]
    pub fn is_audio_only(&self) -> bool {
        self.device.as_ref().is_some_and(|device| device.audio_only)
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
        // Room for the pictures to follow audio buffered a frame or two
        // deeper than a short target (see `LiveSourceClock`).
        let capacity = wanted
            .saturating_mul(2)
            .max(wanted.saturating_add(3))
            .min(affordable);
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
            .field("device", &self.device)
            .field("stream_id", &self.stream_id)
            .field("stream_key", &self.stream_key.as_ref().map(|_| REDACTED))
            .field("passphrase", &self.passphrase.as_ref().map(|_| REDACTED))
            .finish()
    }
}

/// The jitter buffer's bounds, in program frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferBounds {
    /// Frames a connection buffers before it goes live.
    pub target_frames: usize,
    /// Most frames the buffer holds; a full buffer drops its oldest. Twice
    /// the target, and at least three frames more than it.
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
    /// The connection's audio, drift-corrected into the engine clock;
    /// `None` only in unit tests that exercise the pictures alone.
    clock: Option<LiveSourceClock>,
    /// The stream has no audio: connect without an audio output.
    audio_absent: bool,
    /// The child refused an output without streams.
    saw_empty_output: bool,
    /// The child reported an error naming a device's audio input.
    audio_input_failed: bool,
    /// The connected RTMP publisher used another stream key: drop it.
    rejected_peer: bool,
    /// The child's log is inside its input stream dump (`Input #0`).
    in_input_dump: bool,
    /// Audio channels of the source's first audio stream; 0 unknown.
    source_channels: u16,
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
    bitrate: BitrateMeter,
    last_error: Option<String>,
}

/// The received media bitrate over about the last second.
///
/// A window's bytes are divided by the media time they carry (the longest
/// per-stream sum of packet durations), not by the wall time they took to
/// arrive: a connection's first packets reach the receiver in one burst once
/// the child has probed the stream, which a wall-clock window reads as a
/// spike, and a window that rolled during the silent probe read zero for up
/// to a second while the stream was already live. The window still rolls
/// once a wall-clock second (a window without packets reads zero, and so
/// does a connection until its first packet), and a connection publishes its
/// first reading as soon as it has received [`BITRATE_FIRST_READING`] of
/// media instead of a window later. Packets without durations (or streams
/// without a time base) fall back to the window's wall time.
#[derive(Default)]
struct BitrateMeter {
    bps: u64,
    bytes: u64,
    /// Media time received per stream index in this window, in nanoseconds.
    media_nanos: Vec<u64>,
    /// The connection's stream time bases (`#tb` lines): numerator and
    /// denominator.
    time_bases: Vec<Option<(u64, u64)>>,
    /// The window's start; `None` until the connection's first packet.
    start: Option<Instant>,
    /// The connection published a reading.
    measured: bool,
}

impl BitrateMeter {
    /// Forgets the reading and the connection's streams.
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Records a `#tb <stream>: <num>/<den>` line.
    fn time_base(&mut self, line: &str) {
        let Some((stream, base)) = line
            .strip_prefix("#tb ")
            .and_then(|rest| rest.split_once(':'))
        else {
            return;
        };
        let Some((numerator, denominator)) = base.trim().split_once('/') else {
            return;
        };
        let (Ok(stream), Ok(numerator), Ok(denominator)) = (
            stream.trim().parse::<usize>(),
            numerator.trim().parse::<u64>(),
            denominator.trim().parse::<u64>(),
        ) else {
            return;
        };
        if stream >= BITRATE_MAX_STREAMS || numerator == 0 || denominator == 0 {
            return;
        }
        if self.time_bases.len() <= stream {
            self.time_bases.resize(stream + 1, None);
        }
        self.time_bases[stream] = Some((numerator, denominator));
    }

    /// Counts one received packet.
    fn packet(&mut self, packet: CrcPacket, now: Instant) {
        let start = *self.start.get_or_insert(now);
        self.bytes = self.bytes.saturating_add(packet.size);
        if let Some(Some((numerator, denominator))) = self.time_bases.get(packet.stream) {
            let nanos = u128::from(packet.duration) * u128::from(*numerator) * 1_000_000_000
                / u128::from(*denominator);
            if self.media_nanos.len() <= packet.stream {
                self.media_nanos.resize(packet.stream + 1, 0);
            }
            let total = &mut self.media_nanos[packet.stream];
            *total = total.saturating_add(u64::try_from(nanos).unwrap_or(u64::MAX));
        }
        if !self.measured && u128::from(self.media()) >= BITRATE_FIRST_READING.as_nanos() {
            self.bps = self.rate(now.saturating_duration_since(start));
            self.measured = true;
        }
        self.roll(now);
    }

    /// Publishes the window once it spans [`BITRATE_WINDOW`] and starts the
    /// next one.
    fn roll(&mut self, now: Instant) {
        let Some(start) = self.start else {
            return;
        };
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= BITRATE_WINDOW {
            self.bps = self.rate(elapsed);
            self.measured = true;
            self.bytes = 0;
            self.media_nanos.fill(0);
            self.start = Some(now);
        }
    }

    fn media(&self) -> u64 {
        self.media_nanos.iter().copied().max().unwrap_or(0)
    }

    fn rate(&self, elapsed: Duration) -> u64 {
        let media = self.media();
        let nanos = if media > 0 {
            u128::from(media)
        } else {
            elapsed.as_nanos()
        }
        .max(1);
        u64::try_from(u128::from(self.bytes) * 8 * 1_000_000_000 / nanos).unwrap_or(u64::MAX)
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    /// The epoch of the arrival and pull times the drift loop sees.
    origin: Instant,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Seconds since [`Self::origin`].
    fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }
}

/// Fixed per-receiver values every thread reads.
struct Plan {
    config: NetworkInputConfig,
    executable: OsString,
    /// The stream protocol; `None` for a capture device.
    protocol: Option<StreamProtocol>,
    /// The stream, for its connect URL and secrets; `None` for a device.
    stream: Option<NetworkStreamConfig>,
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

    fn stall_after(&self) -> usize {
        (self.bounds.target_frames / 2).max(2)
    }

    /// The `concat` script the child reads from its stdin, `Some` only for
    /// an encrypted SRT stream ([`srt_script`]).
    fn script(&self) -> Option<String> {
        let stream = self.stream.as_ref()?;
        srt_script(self, &child_url(&stream.connect_url()))
    }

    /// Whether the child waits for its peer (an RTMP or SRT listener, or an
    /// SRT rendezvous).
    fn listens(&self) -> bool {
        self.stream.is_some() && self.config.settings.mode.waits_for_peer()
    }

    /// `text` with every secret of the stream masked and bounded.
    fn scrub(&self, text: &str) -> String {
        match &self.stream {
            Some(stream) => stream.scrub(text),
            None => scrub_stream_text(text, &self.config.url),
        }
    }

    /// Whether the receiver owns the listening port and relays each
    /// accepted publisher to its child (an RTMP listener).
    fn relays(&self) -> bool {
        self.protocol == Some(StreamProtocol::Rtmp)
            && self.stream.is_some()
            && self.config.settings.mode == StreamMode::Listener
    }

    /// Whether the receiver probes its child's port before it reports
    /// `listening` (an SRT listener).
    fn probes(&self) -> bool {
        self.protocol == Some(StreamProtocol::Srt)
            && self.stream.is_some()
            && self.config.settings.mode == StreamMode::Listener
    }

    /// The state the receiver starts in: `listening` only for a rendezvous,
    /// whose wait begins as its child starts; a listener is `connecting`
    /// until its port accepts.
    fn initial_state(&self) -> NetworkStreamState {
        if self.listens() && self.config.settings.mode == StreamMode::Rendezvous {
            NetworkStreamState::Listening
        } else {
            NetworkStreamState::Connecting
        }
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
        let (protocol, stream) = if let Some(device) = &config.device {
            if device.path.is_empty()
                || device.format.is_empty()
                || (device.audio_only && device.audio.is_some())
            {
                return Err(NetworkInputError::InvalidFormat);
            }
            config
                .settings
                .validate()
                .map_err(NetworkInputError::InvalidStream)?;
            if config.settings.transport != RtspTransport::Auto
                || config.settings.mode != StreamMode::Caller
            {
                return Err(NetworkInputError::InvalidStream(
                    NetworkStreamError::TransportNotRtsp,
                ));
            }
            (None, None)
        } else {
            let stream = config.stream();
            let protocol = stream
                .validate()
                .map_err(NetworkInputError::InvalidStream)?;
            (Some(protocol), Some(stream))
        };
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
        let clock = SampleRate::new(config.sample_rate)
            .map(|rate| {
                LiveSourceConfig::new(
                    rate,
                    usize::from(config.channels),
                    config.frame_rate,
                    bounds.target_frames,
                )
            })
            .and_then(|live| LiveSourceClock::new(live).ok())
            .ok_or(NetworkInputError::InvalidFormat)?;
        let frame_bytes = config
            .frame_bytes()
            .ok_or(NetworkInputError::InvalidFormat)?;
        let executable = receiver_executable(&config.ffmpeg)?;
        let channels = usize::from(config.channels);
        // A device without an audio input connects without audio; an
        // audio-only device always reads its audio.
        let audio_absent = config
            .device
            .as_ref()
            .is_some_and(|device| device.audio.is_none() && !device.audio_only);
        let plan = Arc::new(Plan {
            executable,
            protocol,
            stream,
            bounds,
            frame_bytes,
            channels,
            token: random_token()?,
            config,
        });
        let state = plan.initial_state();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                state,
                audio_absent,
                clock: Some(clock),
                last_samples: vec![0.0; channels],
                // The consumer starts on a blank picture: no black slate is
                // due before the first stall.
                slate_shown: true,
                ..Inner::default()
            }),
            wake: Condvar::new(),
            origin: Instant::now(),
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
        let at = self.shared.now();
        pull_frame(&mut self.shared.lock(), &self.plan, at)
    }

    /// The source's drift telemetry: estimated clock drift, audio backlog,
    /// and hard resyncs (AU-004). `None` while the receiver has no audio.
    #[must_use]
    pub fn sync_telemetry(&self) -> Option<LiveSourceTelemetry> {
        let inner = self.shared.lock();
        if inner.audio_absent {
            return None;
        }
        inner.clock.as_ref().map(LiveSourceClock::telemetry)
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
            bitrate_bps: inner.bitrate.bps,
            buffer_ms: u32::try_from(buffer_ms).unwrap_or(u32::MAX),
            dropped_frames: inner.dropped_frames,
            late_frames: inner.late_frames,
            reconnects: inner.reconnects,
            source_channels: inner.source_channels,
            last_error: inner.last_error.clone(),
        }
    }

    /// Pictures queued now (at most the buffer capacity).
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        self.shared.lock().frames.len()
    }

    /// Whether the receiver connects with audio: `false` once it found the
    /// stream (or device) has none, or a device's audio input failed.
    #[must_use]
    pub fn audio_active(&self) -> bool {
        !self.shared.lock().audio_absent
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

/// Takes program frame `inner.pulled` at `at` seconds: the frame's
/// drift-corrected audio and, while live, the picture the audio says.
fn pull_frame(inner: &mut Inner, plan: &Plan, at: f64) -> PulledFrame {
    let samples = plan.samples_of(inner.pulled);
    inner.pulled = inner.pulled.wrapping_add(1);
    let mut planes = vec![vec![0.0_f32; samples]; plan.channels];
    let live = inner.state == NetworkStreamState::Live;
    let mut action = PictureAction::Advance { skip: 0 };
    let mut rendered = 0;
    if let Some(clock) = inner.clock.as_mut() {
        let queued = live.then_some(inner.frames.len());
        let mut output: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
        // Pulled even while not live, so the backlog stays at its target.
        if let Ok(pull) = clock.pull(&mut output, queued, at) {
            action = pull.picture;
            rendered = pull.audio.rendered_samples;
        }
    }
    let mut picture = None;
    let mut taken = 0;
    if live {
        if action == PictureAction::Hold && !inner.frames.is_empty() {
            // The pictures lead the audio: show this one a frame longer.
            inner.empty_pulls = 0;
            picture = Some(Picture::Hold);
            taken = rendered;
        } else {
            if let PictureAction::Advance { skip } = action {
                for _ in 0..skip.min(inner.frames.len().saturating_sub(1)) {
                    inner.frames.pop_front();
                    inner.dropped_frames = inner.dropped_frames.saturating_add(1);
                }
            }
            if let Some(frame) = inner.frames.pop_front() {
                inner.empty_pulls = 0;
                inner.slate_shown = false;
                picture = Some(Picture::New(frame));
                taken = rendered;
            } else {
                inner.late_frames = inner.late_frames.saturating_add(1);
                inner.empty_pulls += 1;
                if inner.empty_pulls >= plan.stall_after() {
                    inner.state = NetworkStreamState::Stalled;
                } else {
                    // A late picture keeps its audio playing.
                    taken = rendered;
                }
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
    shape_audio(inner, &mut planes, taken);
    PulledFrame {
        picture,
        audio: planes,
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
/// bitrate outputs. An RTMP listener's child listens on `relay_port` of the
/// loopback interface instead of the configured address.
fn arguments(plan: &Plan, audio_port: Option<u16>, relay_port: Option<u16>) -> Vec<OsString> {
    let config = &plan.config;
    let micros = (u64::from(config.settings.timeout_ms) * 1_000).to_string();
    // Informational lines (tagged by level) name the source's audio layout;
    // only errors become the reported error.
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "level+info",
        "-nostats",
        "-analyzeduration",
        ANALYZE_MICROS,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if let Some(device) = &config.device {
        return device_arguments(plan, device, args, audio_port);
    }
    let protocol = plan.protocol.unwrap_or(StreamProtocol::Udp);
    let mut url = plan
        .stream
        .as_ref()
        .map_or_else(|| config.url.clone(), NetworkStreamConfig::connect_url);
    if let Some(port) = relay_port.filter(|_| plan.relays()) {
        url = relay_url(&url, port);
    }
    if plan.script().is_some() {
        // Every SRT option travels in the script: given here, it would
        // apply to the script's own input and be refused as unused.
        args.extend([
            "-f".into(),
            "concat".into(),
            // The script names a network URL, which `safe` mode refuses.
            "-safe".into(),
            "0".into(),
            // The entry inherits this list: `pipe` reads the script, `srt`
            // is the stream itself.
            "-protocol_whitelist".into(),
            "pipe,srt".into(),
            "-i".into(),
            SCRIPT_INPUT.into(),
        ]);
    } else {
        protocol_arguments(plan, protocol, &mut args, micros);
        args.extend([
            "-protocol_whitelist".into(),
            protocol_whitelist(protocol).into(),
            "-i".into(),
            child_url(&url),
        ]);
    }
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
            format!(
                "{}aresample={}:async=1:first_pts=0",
                pair_filter(config),
                config.sample_rate
            ),
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

/// The SRT input options: mode, latency, encryption, and timeouts (a
/// listener waits [`LISTEN_CYCLE`] for its caller). The stream ID travels in
/// the connect URL ([`NetworkStreamConfig::connect_url`]).
/// The input options of a `protocol` stream: its transport hint and its
/// connect, read, or listen timeouts.
fn protocol_arguments(
    plan: &Plan,
    protocol: StreamProtocol,
    args: &mut Vec<String>,
    micros: String,
) {
    let config = &plan.config;
    match protocol {
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
        StreamProtocol::Rtmp => {
            if config.settings.mode == StreamMode::Listener {
                // `-timeout` is the listen timeout, in seconds: the relayed
                // child's peer is the receiver itself, which connects as
                // soon as the port is bound.
                args.extend([
                    "-listen".into(),
                    "1".into(),
                    "-timeout".into(),
                    CONNECT_GRACE.as_secs().to_string(),
                ]);
            } else {
                args.extend(["-rw_timeout".into(), micros]);
            }
        }
        StreamProtocol::Srt => srt_arguments(plan, args, micros),
    }
}

fn srt_arguments(plan: &Plan, args: &mut Vec<String>, micros: String) {
    for (name, value) in srt_options(plan, micros) {
        args.extend([format!("-{name}"), value]);
    }
}

/// The SRT protocol options, as name and value pairs.
fn srt_options(plan: &Plan, micros: String) -> Vec<(&'static str, String)> {
    let config = &plan.config;
    let settings = &config.settings;
    let mut options = vec![
        ("mode", settings.mode.token().to_owned()),
        (
            "latency",
            (u64::from(settings.latency_ms) * 1_000).to_string(),
        ),
    ];
    if let Some(passphrase) = &config.passphrase {
        options.extend([
            ("passphrase", passphrase.clone()),
            ("pbkeylen", settings.key_length.bytes().to_string()),
        ]);
    }
    if settings.mode == StreamMode::Listener {
        let listen = u64::try_from(LISTEN_CYCLE.as_micros()).unwrap_or(u64::MAX);
        options.push(("listen_timeout", listen.to_string()));
    }
    options.push(("timeout", micros));
    options
}

/// The input of an encrypted SRT stream: `ffmpeg`'s `concat` demuxer
/// reading a one-entry script from the child's stdin (`pipe:0`), so the
/// passphrase never reaches argv, which every local user can read in
/// `/proc/<pid>/cmdline`. Each `option` line is applied to the entry's own
/// input exactly as the same option before `-i` would be.
fn srt_script(plan: &Plan, url: &str) -> Option<String> {
    if plan.protocol != Some(StreamProtocol::Srt) || plan.config.passphrase.is_none() {
        return None;
    }
    let micros = (u64::from(plan.config.settings.timeout_ms) * 1_000).to_string();
    // The entry is probed on its own, with the receiver's probe window.
    let mut script = format!(
        "ffconcat version 1.0\nfile {}\noption analyzeduration {ANALYZE_MICROS}\n",
        concat_quote(url)
    );
    for (name, value) in srt_options(plan, micros) {
        let _ = writeln!(script, "option {name} {}", concat_quote(&value));
    }
    Some(script)
}

/// `value` as one `ffconcat` token: single-quoted, each inner quote closed,
/// escaped, and reopened. Validated URLs and passphrases hold no newline.
fn concat_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Where the child reads its `concat` script, and the name it echoes in its
/// messages in place of the stream URL.
const SCRIPT_INPUT: &str = "pipe:0";

/// The probe window of a stream's input, in microseconds.
const ANALYZE_MICROS: &str = "1000000";

/// The `pan` filter that picks the configured stereo pair of the source
/// (empty for a downmix). A pair the source lacks is silent.
fn pair_filter(config: &NetworkInputConfig) -> String {
    config
        .settings
        .audio
        .first_channel()
        .filter(|_| config.device.is_none())
        .map(|left| format!("pan=stereo|c0=c{left}|c1=c{},", left + 1))
        .unwrap_or_default()
}

/// The only `ffmpeg` protocols the input of a `protocol` stream may open.
///
/// Without an explicit list a stream inherits its first protocol's default
/// list, which for HTTP still admits `rtp`, `udp`, `httpproxy`, and `data`:
/// a served session description, playlist, or redirect could then open
/// local ports or other protocols. `file`, `concat`, `pipe`, and the rest
/// are never on any list (an encrypted SRT stream's own child reads its
/// script through `pipe`, and its stream through `srt` only).
const fn protocol_whitelist(protocol: StreamProtocol) -> &'static str {
    match protocol {
        StreamProtocol::Rtsp => "rtsp,rtsps,rtp,srtp,udp,tcp,tls",
        StreamProtocol::Rtp => "rtp,udp",
        StreamProtocol::Udp => "udp",
        StreamProtocol::Tcp => "tcp",
        StreamProtocol::Http => "http,https,tcp,tls",
        // AES-128 segments are read through `crypto`.
        StreamProtocol::Hls => "http,https,tcp,tls,crypto",
        StreamProtocol::Rtmp => "rtmp,rtmps,tcp,tls",
        StreamProtocol::Srt => "srt",
    }
}

/// An RTMP listener's `url` with its authority replaced by the loopback
/// `port` its relayed child listens on; the application and stream key
/// path is kept, since the child checks the publisher's stream name.
fn relay_url(url: &str, port: u16) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("rtmp", url));
    let path = rest.find(['/', '?', '#']).map_or("", |at| &rest[at..]);
    format!("{scheme}://127.0.0.1:{port}{path}")
}

/// The host and port a listener binds: the URL's host (a bracketed IPv6
/// host unwrapped) and its listen port.
fn listen_address(plan: &Plan) -> Option<(String, u16)> {
    let stream = plan.stream.as_ref()?;
    let port = stream.listen_endpoint()?.port;
    let (_, rest) = stream.url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.0,
        None => host_port
            .rsplit_once(':')
            .map_or(host_port, |(host, _)| host),
    };
    Some((host.to_owned(), port))
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

/// The child's arguments for a capture device: the picture input, the
/// optional audio input, then the video and audio outputs.
fn device_arguments(
    plan: &Plan,
    device: &DeviceInput,
    mut args: Vec<String>,
    audio_port: Option<u16>,
) -> Vec<OsString> {
    let config = &plan.config;
    if device.audio_only {
        return audio_device_arguments(plan, device, args, audio_port);
    }
    args.extend(["-thread_queue_size".into(), "64".into()]);
    args.extend(["-f".into(), device.format.clone()]);
    for (name, value) in &device.options {
        args.extend([format!("-{name}"), value.clone()]);
    }
    args.extend(["-i".into(), device.path.clone()]);
    let audio = device.audio.as_ref().filter(|_| audio_port.is_some());
    if let Some(audio) = audio {
        args.extend([
            "-thread_queue_size".into(),
            "1024".into(),
            "-f".into(),
            audio.format.clone(),
        ]);
        for (name, value) in &audio.options {
            args.extend([format!("-{name}"), value.clone()]);
        }
        args.extend(["-i".into(), audio.path.clone()]);
    }
    let rate = config.frame_rate;
    let deinterlace = device
        .field_order
        .map(|order| format!("yadif=mode=send_frame:parity={order},"))
        .unwrap_or_default();
    let (width, height) = (config.width, config.height);
    let scale = if device.letterbox {
        // Fit inside the program picture and centre on opaque black.
        format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,setsar=1,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black"
        )
    } else {
        format!("scale={width}:{height}")
    };
    args.extend([
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        format!(
            "{deinterlace}{scale},fps=fps={}/{}:start_time=0,format=rgba",
            rate.numerator(),
            rate.denominator()
        ),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    if let (Some(_), Some(port)) = (audio, audio_port) {
        args.extend([
            "-map".into(),
            "1:a:0".into(),
            "-af".into(),
            // No timestamp compensation: the sample count stays on the
            // sound card's own clock, which the receiver's drift loop
            // follows (AU-004) instead of `ffmpeg` padding and trimming
            // against the host clock.
            format!("aresample={}", config.sample_rate),
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
    args.into_iter().map(OsString::from).collect()
}

/// The child's arguments for an audio-only device (AU-011): the audio
/// input, the pacing pictures `showwaves` draws from it, and the audio.
fn audio_device_arguments(
    plan: &Plan,
    device: &DeviceInput,
    mut args: Vec<String>,
    audio_port: Option<u16>,
) -> Vec<OsString> {
    let config = &plan.config;
    args.extend([
        "-thread_queue_size".into(),
        "1024".into(),
        "-f".into(),
        device.format.clone(),
    ]);
    for (name, value) in &device.options {
        args.extend([format!("-{name}"), value.clone()]);
    }
    args.extend(["-i".into(), device.path.clone()]);
    let rate = config.frame_rate;
    args.extend([
        "-filter_complex".into(),
        format!(
            "[0:a:0]showwaves=s={}x{}:rate={}/{}:mode=point,format=rgba[pace]",
            config.width,
            config.height,
            rate.numerator(),
            rate.denominator()
        ),
        "-map".into(),
        "[pace]".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    if let Some(port) = audio_port {
        args.extend([
            "-map".into(),
            "0:a:0".into(),
            "-af".into(),
            // As for a capture device's audio: the device's own clock.
            format!("aresample={}", config.sample_rate),
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
    args.into_iter().map(OsString::from).collect()
}

/// The audio-server environment an audio-only device's child keeps
/// (AU-011): `PulseAudio` finds its server and cookie through these, and
/// ALSA reads `~/.asoundrc` through `HOME`.
const AUDIO_SERVER_ENVIRONMENT: &[&str] = &[
    "HOME",
    "XDG_RUNTIME_DIR",
    "PULSE_SERVER",
    "PULSE_COOKIE",
    "PULSE_RUNTIME_PATH",
];

/// Starts a device's feeder with its standard output piped, the same
/// retained environment as the child, and its messages discarded.
fn spawn_feeder(plan: &Plan, spec: &DeviceFeeder) -> Result<Child, String> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    let environment = plan
        .config
        .device
        .iter()
        .flat_map(|device| device.environment.iter().map(String::as_str));
    for name in ["PATH", "HOME", "TMPDIR"].into_iter().chain(environment) {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => "capture helper unavailable".to_owned(),
        kind => format!("capture helper spawn failed: {kind:?}"),
    })
}

fn command(plan: &Plan, audio_port: Option<u16>, relay_port: Option<u16>) -> Command {
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
        .args(arguments(plan, audio_port, relay_port))
        .stdin(if plan.script().is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    #[cfg(windows)]
    let retained: &[&str] = &["PATH", "SystemRoot", "WINDIR", "PATHEXT"];
    #[cfg(not(windows))]
    let retained: &[&str] = &["PATH"];
    let audio_server: &[&str] = if plan.config.is_audio_only() {
        AUDIO_SERVER_ENVIRONMENT
    } else {
        &[]
    };
    let device_environment = plan
        .config
        .device
        .iter()
        .flat_map(|device| device.environment.iter().map(String::as_str));
    for name in retained
        .iter()
        .chain(audio_server)
        .copied()
        .chain(device_environment)
    {
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
    /// A listener's wait or session ended: listen again at once.
    Relisten,
    /// The connection ended; `delivered` if it delivered a picture.
    Dropped {
        delivered: bool,
    },
}

/// Connects, watches, and reconnects until shutdown or the policy gives up.
fn supervise(shared: &Arc<Shared>, plan: &Arc<Plan>) {
    let policy = plan.config.settings.reconnect;
    let mut attempt: u32 = 0;
    // An RTMP listener's own port, bound once and kept.
    let mut relay = None;
    loop {
        match connect_once(shared, plan, &mut relay) {
            Ended::Shutdown => return,
            Ended::Relisten => {
                attempt = 0;
                let mut inner = shared.lock();
                inner.audio_absent = false;
                inner.frames.clear();
                inner.bitrate.clear();
                if inner.state == NetworkStreamState::Listening {
                    // A wait that ran out its cycle is not an error; a
                    // refused publisher or an ended session keeps its
                    // reason.
                    if !inner.rejected_peer {
                        inner.last_error = None;
                    }
                } else {
                    inner.reconnects = inner.reconnects.saturating_add(1);
                }
                continue;
            }
            Ended::Dropped { delivered } => {
                if delivered {
                    attempt = 0;
                }
            }
        }
        if !plan.config.is_audio_only() {
            let mut inner = shared.lock();
            if inner.audio_input_failed && !inner.audio_absent && !inner.delivered {
                // The audio input kept the device from opening: reopen it
                // without audio at once, before the reconnect below asks
                // for audio again.
                inner.audio_absent = true;
                inner.audio_input_failed = false;
                inner.last_error = None;
                continue;
            }
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
        inner.bitrate.clear();
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

/// Resets the shared state for a new connection: its generation and
/// whether the source has no audio, or `None` once shut down. A listener's
/// connection starts `listening` when its port already accepts (a bound
/// RTMP relay, or a rendezvous), and `connecting` until then.
fn begin_connection(shared: &Shared, plan: &Plan, accepting: bool) -> Option<(u64, bool)> {
    let mut inner = shared.lock();
    if inner.shutdown {
        return None;
    }
    inner.generation = inner.generation.wrapping_add(1);
    inner.frames.clear();
    if let Some(clock) = inner.clock.as_mut() {
        clock.restart();
    }
    inner.delivered = false;
    inner.last_data = None;
    inner.empty_pulls = 0;
    inner.saw_empty_output = false;
    inner.audio_input_failed = false;
    inner.rejected_peer = false;
    inner.in_input_dump = false;
    if plan.listens() {
        inner.state = if accepting {
            NetworkStreamState::Listening
        } else {
            NetworkStreamState::Connecting
        };
    }
    inner.bitrate.clear();
    Some((inner.generation, inner.audio_absent))
}

/// Ends a connection's feeder process, if it has one.
fn stop_feeder(feeder: Option<Child>) {
    if let Some(mut feeder) = feeder {
        let _ = feeder.kill();
        let _ = feeder.wait();
    }
}

/// A connection about to start its child.
struct Opened {
    generation: u64,
    audio_absent: bool,
    /// An SRT listener's readiness probe.
    probe: Option<SrtProbe>,
    /// An RTMP listener's accepted publisher and its child's loopback port.
    publisher: Option<(TcpStream, u16)>,
}

/// Starts a connection: resets the shared state and, for an RTMP listener,
/// waits for a publisher on the receiver's own port.
fn open_connection(
    shared: &Shared,
    plan: &Plan,
    relay: &mut Option<TcpListener>,
) -> Result<Opened, Ended> {
    let probe = if plan.probes() {
        SrtProbe::new(plan)
    } else {
        None
    };
    // Without a probe, an SRT listener reports `listening` as its child
    // starts, as a rendezvous does.
    let accepting = relay.is_some() || (plan.listens() && !plan.relays() && probe.is_none());
    let (generation, audio_absent) =
        begin_connection(shared, plan, accepting).ok_or(Ended::Shutdown)?;
    let publisher = if plan.relays() {
        let publisher = accept_publisher(shared, plan, relay)?;
        match free_loopback_port() {
            Ok(port) => Some((publisher, port)),
            Err(error) => {
                shared.lock().last_error = Some(format!("relay port: {:?}", error.kind()));
                return Err(Ended::Dropped { delivered: false });
            }
        }
    } else {
        None
    };
    Ok(Opened {
        generation,
        audio_absent,
        probe,
        publisher,
    })
}

/// Spawns a connection's feeder (a device's capture helper) and child, and
/// hands an encrypted SRT child its script.
fn spawn_child(
    shared: &Shared,
    plan: &Plan,
    audio_port: Option<u16>,
    relay_port: Option<u16>,
) -> Result<(Child, Option<Child>), Ended> {
    let feeder = plan
        .config
        .device
        .as_ref()
        .and_then(|device| device.feeder.as_ref())
        .map(|spec| spawn_feeder(plan, spec))
        .transpose();
    let mut feeder = match feeder {
        Ok(feeder) => feeder,
        Err(error) => {
            shared.lock().last_error = Some(error);
            return Err(Ended::Dropped { delivered: false });
        }
    };
    let mut command = command(plan, audio_port, relay_port);
    if let Some(stdout) = feeder.as_mut().and_then(|feeder| feeder.stdout.take()) {
        command.stdin(Stdio::from(stdout));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            stop_feeder(feeder);
            shared.lock().last_error = Some(match error.kind() {
                io::ErrorKind::NotFound => "ffmpeg unavailable".to_owned(),
                kind => format!("ffmpeg spawn failed: {kind:?}"),
            });
            return Err(Ended::Dropped { delivered: false });
        }
    };
    if let Some(script) = plan.script() {
        // The script is far smaller than `PIPE_BUF`, so the write is atomic
        // and cannot block; closing stdin ends the script. A child that did
        // not get it whole must not run on without its passphrase.
        let written = child.stdin.take().map_or_else(
            || Err(io::ErrorKind::BrokenPipe.into()),
            |mut stdin| stdin.write_all(script.as_bytes()),
        );
        if let Err(error) = written {
            let _ = child.kill();
            let _ = child.wait();
            stop_feeder(feeder);
            shared.lock().last_error = Some(format!("ffmpeg stdin: {:?}", error.kind()));
            return Err(Ended::Dropped { delivered: false });
        }
    }
    Ok((child, feeder))
}

/// Runs one connection to its end. `relay` is an RTMP listener's own
/// listening socket, bound at its first connection and kept across them.
fn connect_once(shared: &Arc<Shared>, plan: &Arc<Plan>, relay: &mut Option<TcpListener>) -> Ended {
    let Opened {
        generation,
        audio_absent,
        probe,
        publisher,
    } = match open_connection(shared, plan, relay) {
        Ok(opened) => opened,
        Err(ended) => return ended,
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
    let audio_port = listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|address| address.port());
    let relay_port = publisher.as_ref().map(|(_, port)| *port);
    let (mut child, feeder) = match spawn_child(shared, plan, audio_port, relay_port) {
        Ok(spawned) => spawned,
        Err(ended) => return ended,
    };
    let mut readers = Vec::with_capacity(5);
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
    // An accepted publisher is relayed to the child once it listens; the
    // sockets are kept to end the relay.
    let linked = match publisher {
        Some((publisher, port)) => {
            relay_publisher(shared, &mut child, publisher, port, &mut readers)
        }
        None => Ok(Vec::new()),
    };
    let started = Instant::now();
    let (ended, sockets) = match linked {
        // The relayed publisher has its media timeout from the start.
        Ok(sockets) => (
            watch(
                shared,
                plan,
                &mut child,
                started,
                plan.listens() && !plan.relays(),
                probe,
            ),
            sockets,
        ),
        Err(ended) => (ended, Vec::new()),
    };
    let ran = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    stop_feeder(feeder);
    // Ends the relay pumps, and drops the publisher.
    for socket in &sockets {
        let _ = socket.shutdown(Shutdown::Both);
    }
    {
        let mut inner = shared.lock();
        // Readers of this connection stop pushing.
        inner.generation = inner.generation.wrapping_add(1);
    }
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }
    match ended {
        // A relayed publisher's session ended (it left, was refused, or
        // sent nothing): the port still listens, so listen on at once. A
        // stream found without audio is the supervisor's to retry without.
        Ended::Dropped { delivered }
            if !sockets.is_empty() && (delivered || !shared.lock().saw_empty_output) =>
        {
            Ended::Relisten
        }
        // A listener that waited out its cycle, or whose peer left (or was
        // refused), listens again at once.
        Ended::Dropped { delivered }
            if plan.listens() && (delivered || ran >= LISTEN_FAILURE_WINDOW) =>
        {
            Ended::Relisten
        }
        ended => ended,
    }
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

/// Watches one connection until it ends. A connection that `waits` (a
/// listener's or rendezvous's child) has no media timeout until its peer
/// delivers; an SRT listener's `probe` turns it `listening` once its port
/// answers.
fn watch(
    shared: &Shared,
    plan: &Plan,
    child: &mut Child,
    started: Instant,
    waits: bool,
    mut probe: Option<SrtProbe>,
) -> Ended {
    let timeout = Duration::from_millis(u64::from(plan.config.settings.timeout_ms));
    let mut inner = shared.lock();
    loop {
        if inner.shutdown {
            return Ended::Shutdown;
        }
        let now = Instant::now();
        inner.bitrate.roll(now);
        let delivered = inner.delivered;
        if inner.rejected_peer {
            return Ended::Dropped { delivered };
        }
        let limit = if delivered {
            timeout
        } else {
            timeout + CONNECT_GRACE
        };
        let since = inner.last_data.unwrap_or(started);
        // A listener waits for its peer as long as its child does.
        let waiting = waits && inner.last_data.is_none();
        if !waiting && now.saturating_duration_since(since) >= limit {
            inner.last_error = Some(format!("no media for {} ms", limit.as_millis()));
            return Ended::Dropped { delivered };
        }
        drop(inner);
        if probe.as_mut().is_some_and(SrtProbe::answered) {
            probe = None;
            let mut inner = shared.lock();
            if inner.state == NetworkStreamState::Connecting {
                inner.state = NetworkStreamState::Listening;
            }
        }
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
        if inner.rejected_peer {
            continue;
        }
        admit_picture(&mut inner, plan, frame);
    }
}

/// Queues one decoded picture: drops the oldest from a full buffer, and
/// goes live once the buffer holds its target. The audio is not touched:
/// the pictures follow it at the next pulls.
fn admit_picture(inner: &mut Inner, plan: &Plan, frame: Vec<u8>) {
    inner.last_data = Some(Instant::now());
    inner.delivered = true;
    inner.frames.push_back(frame);
    if inner.frames.len() > plan.bounds.capacity_frames {
        inner.frames.pop_front();
        inner.dropped_frames = inner.dropped_frames.saturating_add(1);
    }
    match inner.state {
        NetworkStreamState::Connecting
        | NetworkStreamState::Listening
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
        if text.is_empty() {
            continue;
        }
        let mut inner = shared.lock();
        if inner.generation != generation {
            return;
        }
        if text.starts_with('#') {
            inner.bitrate.time_base(text);
            continue;
        }
        if let Some(packet) = framecrc_packet(text) {
            inner.bitrate.packet(packet, Instant::now());
            continue;
        }
        let (level, text) = split_level(text);
        let text = text.as_str();
        match level {
            Some(LogLevel::Info) => {
                note_input_dump(&mut inner, text);
                continue;
            }
            Some(LogLevel::Warning) => {
                // `ffmpeg`'s RTMP listener only warns about a publisher's
                // other stream name: the stream key is the receiver's to
                // enforce.
                if plan.config.stream_key.is_some()
                    && plan.listens()
                    && text.contains("Unexpected stream")
                {
                    inner.rejected_peer = true;
                    inner.frames.clear();
                    inner.last_error = Some("a publisher used another stream key".to_owned());
                }
                continue;
            }
            Some(LogLevel::Error) | None => {}
        }
        if text.contains("does not contain any stream") {
            inner.saw_empty_output = true;
        }
        if let Some(audio) = plan
            .config
            .device
            .as_ref()
            .and_then(|device| device.audio.as_ref())
            && (text.contains(&audio.path) || text.contains(&format!("[{} @", audio.format)))
        {
            inner.audio_input_failed = true;
        }
        // The child echoes the URL it was given; the secrets are the same.
        let scrubbed = plan.scrub(&lowercase_scheme_echo(text, plan));
        if !scrubbed.is_empty() {
            debug_assert!(scrubbed.len() <= MAX_STREAM_ERROR_BYTES);
            inner.last_error = Some(scrubbed);
        }
    }
}

/// The level tag `-loglevel level+...` puts on a log line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogLevel {
    /// `info`, `verbose`, `debug`, or `trace`.
    Info,
    Warning,
    /// `error`, `fatal`, or `panic`.
    Error,
}

/// Splits a line's level tag off: `[ctx @ 0x..] [error] text` becomes the
/// level and `[ctx @ 0x..] text`. A line without a tag (a library's own
/// output) has no level and is treated as an error.
fn split_level(line: &str) -> (Option<LogLevel>, String) {
    for (tag, level) in [
        ("[info] ", LogLevel::Info),
        ("[verbose] ", LogLevel::Info),
        ("[debug] ", LogLevel::Info),
        ("[trace] ", LogLevel::Info),
        ("[warning] ", LogLevel::Warning),
        ("[error] ", LogLevel::Error),
        ("[fatal] ", LogLevel::Error),
        ("[panic] ", LogLevel::Error),
    ] {
        // The tag is the first or second bracketed group.
        let at = line.find(tag).filter(|at| {
            *at == 0 || line[..*at].starts_with('[') && line[..*at].trim_end().ends_with(']')
        });
        if let Some(at) = at {
            let mut text = line[..at].to_owned();
            text.push_str(&line[at + tag.len()..]);
            return (Some(level), text);
        }
    }
    (None, line.to_owned())
}

/// Follows the child's input dump and records its first audio stream's
/// channel count.
fn note_input_dump(inner: &mut Inner, text: &str) {
    if text.starts_with("Input #0") {
        inner.in_input_dump = true;
    } else if text.starts_with("Output #") || text.starts_with("Stream mapping") {
        inner.in_input_dump = false;
    } else if inner.in_input_dump
        && inner.source_channels == 0
        && let Some(channels) = audio_stream_channels(text)
    {
        inner.source_channels = channels;
    }
}

/// The channel count of a dumped audio stream line (`Stream #0:1: Audio:
/// aac (LC), 48000 Hz, 5.1, fltp`), from its channel layout.
fn audio_stream_channels(line: &str) -> Option<u16> {
    let (_, audio) = line.trim_start().split_once("Audio: ")?;
    if !line.trim_start().starts_with("Stream #") {
        return None;
    }
    let fields = audio.split(", ").collect::<Vec<_>>();
    let rate = fields.iter().position(|field| field.ends_with(" Hz"))?;
    let layout = fields.get(rate + 1)?.trim();
    let base = layout.split('(').next().unwrap_or(layout).trim();
    let channels = match base {
        "mono" => 1,
        "stereo" | "downmix" => 2,
        "quad" => 4,
        "hexagonal" => 6,
        "octagonal" | "cube" => 8,
        "hexadecagonal" => 16,
        other => {
            if let Some(count) = other.strip_suffix(" channels") {
                count.parse().ok()?
            } else {
                let (front, low) = other.split_once('.')?;
                front.parse::<u16>().ok()? + low.parse::<u16>().ok()?
            }
        }
    };
    Some(channels)
}

/// `text` with the child's lowercase-scheme echo of the connect URL put
/// back as configured, so scrubbing finds it.
fn lowercase_scheme_echo(text: &str, plan: &Plan) -> String {
    let Some(stream) = &plan.stream else {
        return text.replace(&child_url(&plan.config.url), &plan.config.url);
    };
    let connect = stream.connect_url();
    let text = text.replace(&child_url(&connect), &connect);
    // A scripted child names its input by the script's pipe instead.
    if plan.script().is_some() {
        text.replace(SCRIPT_INPUT, &connect)
    } else {
        text
    }
}

/// One packet of the frame CRC listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CrcPacket {
    stream: usize,
    /// In the stream's time base; 0 when unknown.
    duration: u64,
    size: u64,
}

/// The packet of one frame CRC line (`stream, dts, pts, duration, size,
/// crc`).
fn framecrc_packet(line: &str) -> Option<CrcPacket> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 6 || !fields[5].starts_with("0x") {
        return None;
    }
    Some(CrcPacket {
        stream: fields[0].parse().ok()?,
        duration: fields[3].parse().unwrap_or(0),
        size: fields[4].parse().ok()?,
    })
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
    let mut planes = vec![Vec::new(); plan.channels];
    loop {
        let whole = pending.len() / frame * frame;
        if whole > 0 {
            deinterleave(&pending[..whole], &mut planes);
            let at = shared.now();
            let mut inner = shared.lock();
            if inner.generation != generation {
                return true;
            }
            inner.last_data = Some(Instant::now());
            push_audio(&mut inner, &planes, at);
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

/// Splits interleaved little-endian `f32` samples into `planes` (one per
/// channel), clamped to ±1 with non-finite samples zeroed.
fn deinterleave(bytes: &[u8], planes: &mut [Vec<f32>]) {
    let channels = planes.len().max(1);
    for plane in planes.iter_mut() {
        plane.clear();
    }
    for sample in bytes.chunks_exact(channels * 4) {
        for (plane, bytes) in planes.iter_mut().zip(sample.as_chunks::<4>().0) {
            let value = f32::from_le_bytes(*bytes);
            plane.push(if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            });
        }
    }
}

/// Pushes one received block of planar audio, which arrived at `at`.
fn push_audio(inner: &mut Inner, planes: &[Vec<f32>], at: f64) {
    if let Some(clock) = inner.clock.as_mut() {
        let planes: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        // Shapes are the receiver's own and samples are finite: no error.
        let _ = clock.push(&planes, at);
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// The SRT socket ID the readiness probe names itself by.
const SRT_PROBE_SOCKET: u32 = 0x4652_4D58;

/// An SRT (HSv4-compatible) caller's induction handshake: a control packet
/// of type handshake to socket 0, carrying version 4, socket type
/// `SOCK_DGRAM`, an MTU of 1500, a flow window of 8192, handshake type
/// `INDUCTION`, and no cookie. A listener answers it statelessly with its
/// SYN cookie and keeps no state for it.
fn srt_induction() -> [u8; 64] {
    let words: [u32; 13] = [
        0x8000_0000, // control packet, type 0 (handshake)
        0,           // type-specific information
        0,           // timestamp
        0,           // destination socket: the listener
        4,           // handshake version
        2,           // encryption field 0, extension field 2 (SOCK_DGRAM)
        0x0000_1000, // initial packet sequence number
        1_500,       // maximum transmission unit
        8_192,       // maximum flow window
        1,           // handshake type: induction
        SRT_PROBE_SOCKET,
        0, // SYN cookie
        0, // peer address (first word; the rest stays zero)
    ];
    let mut packet = [0_u8; 64];
    for (chunk, word) in packet.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    packet
}

/// Whether `packet` is a listener's answer to [`srt_induction`]: a
/// handshake control packet to the probe's socket, of type induction.
fn is_srt_induction_answer(packet: &[u8]) -> bool {
    let word = |at: usize| {
        packet
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_be_bytes)
    };
    packet.len() >= 64
        && word(0) == Some(0x8000_0000)
        && word(12) == Some(SRT_PROBE_SOCKET)
        && word(36) == Some(1)
}

/// Probes whether an SRT listener's child has bound its port: repeats an
/// induction handshake to it until the listener answers.
struct SrtProbe {
    socket: UdpSocket,
    sent: Option<Instant>,
}

impl SrtProbe {
    /// A probe of the plan's listen address (a wildcard address probed on
    /// loopback), or `None` when the address does not resolve or no
    /// socket opens.
    fn new(plan: &Plan) -> Option<Self> {
        let (host, port) = listen_address(plan)?;
        let mut target = (host.as_str(), port).to_socket_addrs().ok()?.next()?;
        if target.ip().is_unspecified() {
            target.set_ip(match target {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            });
        }
        let local: SocketAddr = match target {
            SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(local).ok()?;
        socket.connect(target).ok()?;
        socket.set_nonblocking(true).ok()?;
        Some(Self { socket, sent: None })
    }

    /// Whether the listener has answered; sends the handshake again once
    /// [`SRT_PROBE_INTERVAL`] passed without an answer. Never blocks.
    fn answered(&mut self) -> bool {
        let mut packet = [0_u8; 256];
        // Errors include a refusal of a port not bound yet.
        while let Ok(read) = self.socket.recv(&mut packet) {
            if is_srt_induction_answer(&packet[..read]) {
                return true;
            }
        }
        if self
            .sent
            .is_none_or(|at| at.elapsed() >= SRT_PROBE_INTERVAL)
        {
            let _ = self.socket.send(&srt_induction());
            self.sent = Some(Instant::now());
        }
        false
    }
}

/// A free loopback TCP port for a relayed child to listen on.
fn free_loopback_port() -> io::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

/// Binds an RTMP listener's configured address, nonblocking.
fn bind_relay(plan: &Plan) -> io::Result<TcpListener> {
    let (host, port) = listen_address(plan).ok_or(io::ErrorKind::InvalidInput)?;
    let listener = TcpListener::bind((host.as_str(), port))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Waits for an RTMP publisher on the receiver's own listening port,
/// binding it first if it is not bound yet. The receiver reports
/// `listening` from the bind on.
fn accept_publisher(
    shared: &Shared,
    plan: &Plan,
    relay: &mut Option<TcpListener>,
) -> Result<TcpStream, Ended> {
    if relay.is_none() {
        match bind_relay(plan) {
            Ok(listener) => {
                *relay = Some(listener);
                let mut inner = shared.lock();
                inner.state = NetworkStreamState::Listening;
                inner.last_error = None;
            }
            Err(error) => {
                shared.lock().last_error = Some(format!("cannot listen: {:?}", error.kind()));
                return Err(Ended::Dropped { delivered: false });
            }
        }
    }
    let Some(listener) = relay.as_ref() else {
        return Err(Ended::Dropped { delivered: false });
    };
    loop {
        if let Ok((publisher, _)) = listener.accept()
            && publisher.set_nonblocking(false).is_ok()
        {
            let _ = publisher.set_nodelay(true);
            return Ok(publisher);
        }
        let inner = shared.lock();
        if inner.shutdown {
            return Err(Ended::Shutdown);
        }
        drop(
            shared
                .wake
                .wait_timeout(inner, POLL)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Connects to a relayed child's loopback `port` once the child has bound
/// it, within [`CONNECT_GRACE`].
fn link_child(shared: &Shared, child: &mut Child, port: u16) -> Result<TcpStream, Ended> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + CONNECT_GRACE;
    loop {
        if shared.lock().shutdown {
            return Err(Ended::Shutdown);
        }
        if let Ok(link) = TcpStream::connect_timeout(&address, RELAY_CONNECT_TIMEOUT) {
            let _ = link.set_nodelay(true);
            return Ok(link);
        }
        if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= deadline {
            let mut inner = shared.lock();
            if inner.last_error.is_none() {
                inner.last_error = Some("ffmpeg did not take the publisher".to_owned());
            }
            return Err(Ended::Dropped { delivered: false });
        }
        thread::sleep(RELAY_RETRY);
    }
}

/// Relays an accepted `publisher` to its child's loopback `port` once the
/// child listens, pushing the two pumps onto `readers`. Returns the two
/// sockets, whose shutdown ends the relay; a publisher that cannot be
/// relayed is dropped.
fn relay_publisher(
    shared: &Shared,
    child: &mut Child,
    publisher: TcpStream,
    port: u16,
    readers: &mut Vec<Option<JoinHandle<()>>>,
) -> Result<Vec<TcpStream>, Ended> {
    let link = match link_child(shared, child, port) {
        Ok(link) => link,
        Err(ended) => {
            let _ = publisher.shutdown(Shutdown::Both);
            return Err(ended);
        }
    };
    for (name, from, to) in [
        ("relay-in", publisher.try_clone(), link.try_clone()),
        ("relay-out", link.try_clone(), publisher.try_clone()),
    ] {
        if let (Ok(from), Ok(to)) = (from, to) {
            readers.push(spawn_pump(name, from, to));
        }
    }
    Ok(vec![publisher, link])
}

/// Copies `from` into `to` until either ends, then ends `to`'s writes.
fn spawn_pump(name: &str, mut from: TcpStream, mut to: TcpStream) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("freemix-network-{name}"))
        .spawn(move || {
            let _ = io::copy(&mut from, &mut to);
            let _ = to.shutdown(Shutdown::Write);
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use fm_types::network_stream::{GiveUp, ReconnectPolicy};

    use super::*;

    fn plan(url: &str, transport: RtspTransport) -> Plan {
        let mut stream = NetworkStreamConfig::new(url);
        stream.settings.transport = transport;
        stream_plan(&stream)
    }

    fn stream_plan(stream: &NetworkStreamConfig) -> Plan {
        let config = NetworkInputConfig::new(stream, 64, 36, FrameRate::new(25, 1).unwrap());
        Plan {
            executable: "ffmpeg".into(),
            protocol: Some(stream.validate().unwrap()),
            stream: Some(stream.clone()),
            bounds: config.buffer_bounds().unwrap(),
            frame_bytes: config.frame_bytes().unwrap(),
            channels: 2,
            token: "t0k3n".into(),
            config,
        }
    }

    /// The child's arguments; an RTMP listener's child listens on loopback
    /// port 40000.
    fn args(plan: &Plan, port: Option<u16>) -> Vec<String> {
        arguments(plan, port, Some(40_000))
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
            ("rtmp://ingest.test/live/key", "rtmp,rtmps,tcp,tls"),
            ("srt://enc.test:9000", "srt"),
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
    fn device_arguments_read_the_device_and_its_audio_and_deinterlace() {
        let device = DeviceInput {
            format: "v4l2".into(),
            options: vec![
                ("input_format".into(), "yuyv422".into()),
                ("video_size".into(), "1920x1080".into()),
                ("framerate".into(), "30000/1001".into()),
            ],
            path: "/dev/video2".into(),
            field_order: Some("tff"),
            audio: Some(DeviceAudioInput {
                format: "alsa".into(),
                options: Vec::new(),
                path: "hw:CARD=Cam,DEV=0".into(),
            }),
            audio_only: false,
            letterbox: false,
            environment: Vec::new(),
            feeder: None,
        };
        let config = NetworkInputConfig::device(
            device,
            NetworkStreamSettings::default(),
            64,
            36,
            FrameRate::new(25, 1).unwrap(),
        );
        let mut plan = plan("udp://@:9000", RtspTransport::Auto);
        plan.protocol = None;
        plan.stream = None;
        plan.config = config;
        let joined = args(&plan, Some(4000)).join(" ");
        assert!(joined.contains(
            "-f v4l2 -input_format yuyv422 -video_size 1920x1080 -framerate 30000/1001 -i /dev/video2"
        ));
        assert!(joined.contains("-f alsa -i hw:CARD=Cam,DEV=0"));
        assert!(joined.contains("-map 1:a:0"));
        assert!(joined.contains(
            "yadif=mode=send_frame:parity=tff,scale=64:36,fps=fps=25/1:start_time=0,format=rgba"
        ));
        assert!(!joined.contains("framecrc"));
        let silent = args(&plan, None).join(" ");
        assert!(!silent.contains("alsa"));
        assert!(!silent.contains("f32le"));
    }

    #[test]
    fn audio_device_arguments_pace_pictures_from_the_audio() {
        let config = NetworkInputConfig::audio_device(
            "pulse",
            "sink.monitor",
            NetworkStreamSettings::default(),
            FrameRate::new(30_000, 1_001).unwrap(),
        );
        assert!(config.is_audio_only());
        assert_eq!((config.width, config.height), (2, 2));
        let mut plan = plan("udp://@:9000", RtspTransport::Auto);
        plan.protocol = None;
        plan.stream = None;
        plan.config = config;
        let joined = args(&plan, Some(4000)).join(" ");
        assert!(joined.contains("-thread_queue_size 1024 -f pulse -i sink.monitor"));
        assert!(joined.contains(
            "[0:a:0]showwaves=s=2x2:rate=30000/1001:mode=point,format=rgba[pace] -map [pace]"
        ));
        assert!(joined.contains("-map 0:a:0 -af aresample=48000 -ac 2"));
        assert!(!joined.contains("framecrc"));
        assert!(!joined.contains("scale="));
        // Audio is never dropped: an audio-only device always reads it.
        let receiver = NetworkReceiver::start(NetworkInputConfig {
            ffmpeg: Executable::Explicit("/nonexistent/ffmpeg".into()),
            ..plan.config.clone()
        });
        assert!(matches!(
            receiver,
            Err(NetworkInputError::InvalidExecutable)
        ));
        let mut both = plan.config.clone();
        if let Some(device) = &mut both.device {
            device.audio = Some(DeviceAudioInput {
                format: "alsa".into(),
                options: Vec::new(),
                path: "default".into(),
            });
        }
        assert!(matches!(
            NetworkReceiver::start(both),
            Err(NetworkInputError::InvalidFormat)
        ));
    }

    #[test]
    fn framecrc_lines_yield_packets() {
        assert_eq!(
            framecrc_packet("1,          0,          0,     3600,     4512, 0x1f2e3d4c"),
            Some(CrcPacket {
                stream: 1,
                duration: 3600,
                size: 4512,
            })
        );
        assert_eq!(
            framecrc_packet(
                "0,      -2698,        902,       -1,     4984, 0x28378265, S=1,        1"
            )
            .map(|packet| (packet.duration, packet.size)),
            Some((0, 4984))
        );
        assert_eq!(framecrc_packet("[udp @ 0x55] Connection refused"), None);
        assert_eq!(framecrc_packet("#tb 0: 1/90000"), None);
    }

    /// 40 ms video packets of 5 000 bytes: 1 Mbit/s of media.
    fn video_packet() -> CrcPacket {
        CrcPacket {
            stream: 0,
            duration: 3_600,
            size: 5_000,
        }
    }

    fn at(start: Instant, millis: u32) -> Instant {
        start + Duration::from_millis(u64::from(millis))
    }

    #[test]
    fn the_first_reading_comes_from_media_time_not_from_a_burst() {
        let mut meter = BitrateMeter::default();
        meter.time_base("#tb 0: 1/90000");
        let start = Instant::now();
        // The probe's backlog arrives at once: 200 ms of media publish a
        // reading at once, at the stream's rate rather than as a spike.
        for _ in 0..4 {
            meter.packet(video_packet(), start);
        }
        assert_eq!(meter.bps, 0, "less media than the first reading needs");
        meter.packet(video_packet(), start);
        assert_eq!(meter.bps, 1_000_000);
        // A window holding the rest of the burst and then real time still
        // reads the stream's rate.
        for _ in 0..10 {
            meter.packet(video_packet(), start);
        }
        for frame in 1..=25 {
            meter.packet(video_packet(), at(start, frame * 40));
        }
        assert_eq!(meter.bps, 1_000_000);
    }

    #[test]
    fn a_connection_reads_zero_until_its_first_packet_and_after_it_goes_quiet() {
        let mut meter = BitrateMeter::default();
        meter.time_base("#tb 0: 1/90000");
        let start = Instant::now();
        // Probing: no packet yet, no window to roll.
        meter.roll(at(start, 3_000));
        assert_eq!(meter.bps, 0);
        for frame in 0..25 {
            meter.packet(video_packet(), at(start, 3_000 + frame * 40));
        }
        assert_eq!(meter.bps, 1_000_000);
        meter.roll(at(start, 4_000));
        assert_eq!(meter.bps, 1_000_000);
        // Nothing arrived over the next window.
        meter.roll(at(start, 5_000));
        assert_eq!(meter.bps, 0);
    }

    #[test]
    fn packets_without_a_time_base_fall_back_to_wall_time() {
        let mut meter = BitrateMeter::default();
        let start = Instant::now();
        meter.packet(video_packet(), start);
        assert_eq!(meter.bps, 0, "no media time: wait for a full window");
        meter.packet(video_packet(), at(start, 500));
        meter.roll(at(start, 1_000));
        assert_eq!(meter.bps, 80_000);
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

    /// A receiver's state with its drift-corrected audio, as `start` makes.
    fn live_inner(plan: &Plan) -> Inner {
        let config = &plan.config;
        let clock = LiveSourceClock::new(LiveSourceConfig::new(
            SampleRate::new(config.sample_rate).unwrap(),
            usize::from(config.channels),
            config.frame_rate,
            plan.bounds.target_frames,
        ))
        .unwrap();
        Inner {
            clock: Some(clock),
            last_samples: vec![0.0; plan.channels],
            ..Inner::default()
        }
    }

    /// A source `ppm` off the engine clock feeding `inner` for `frames`
    /// engine frames: 1024-sample audio bursts and one picture per source
    /// frame, each stamped with its arrival time; the engine pulls once a
    /// frame. Returns the frames whose audio was silent after going live.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn run_source(
        inner: &mut Inner,
        plan: &Plan,
        ppm: f64,
        frames: u64,
        deliver: impl Fn(u64) -> bool,
    ) -> u64 {
        const BURST: usize = 1_024;
        let frame = plan.samples_of(0) as f64;
        let fps = f64::from(plan.config.sample_rate) / frame;
        let mut source = inner.pulled as f64 * frame;
        let mut produced = (source as u64 / BURST as u64) * BURST as u64;
        let mut pictures = (source / frame) as u64;
        let mut silent = 0;
        let mut planes = vec![vec![0.0_f32; BURST]; plan.channels];
        for _ in 0..frames {
            let n = inner.pulled;
            let start = source;
            source += frame * (1.0 + ppm * 1e-6);
            let at = |index: f64| {
                (n as f64 + ((index - start) / (source - start)).clamp(0.0, 1.0)) / fps
            };
            if deliver(n) {
                while (produced + BURST as u64) as f64 <= source {
                    for (channel, plane) in planes.iter_mut().enumerate() {
                        for (offset, sample) in plane.iter_mut().enumerate() {
                            let index = (produced + offset as u64) as f64;
                            *sample = (0.25 * (index * 0.05 + channel as f64).sin()) as f32;
                        }
                    }
                    produced += BURST as u64;
                    push_audio(inner, &planes, at(produced as f64));
                }
                while ((pictures + 1) as f64) * frame <= source {
                    pictures += 1;
                    admit_picture(inner, plan, vec![0; plan.frame_bytes]);
                }
            } else {
                // Nothing arrives: the source's time still passes.
                produced = (source as u64 / BURST as u64) * BURST as u64;
                pictures = (source / frame) as u64;
            }
            let was_live = inner.state == NetworkStreamState::Live;
            let pulled = pull_frame(inner, plan, (n + 1) as f64 / fps);
            if was_live && pulled.audio.iter().flatten().all(|sample| *sample == 0.0) {
                silent += 1;
            }
        }
        silent
    }

    #[test]
    fn a_drifting_source_stays_live_and_audible_with_its_pictures_following_it() {
        for ppm in [800.0, -800.0] {
            let plan = plan("udp://@:9000", RtspTransport::Auto);
            let mut inner = live_inner(&plan);
            inner.state = NetworkStreamState::Connecting;
            // Four minutes at 25 fps.
            let silent = run_source(&mut inner, &plan, ppm, 6_000, |_| true);
            let telemetry = inner.clock.as_ref().unwrap().telemetry();
            assert_eq!(inner.state, NetworkStreamState::Live, "{ppm}");
            assert_eq!(silent, 0, "{ppm}");
            assert_eq!(telemetry.hard_resyncs(), 0, "{ppm}");
            assert_eq!(telemetry.sync.underruns, 0, "{ppm}");
            assert!(
                (telemetry.sync.estimated_drift_ppm - ppm).abs() < 2.0,
                "{ppm}: {}",
                telemetry.sync.estimated_drift_ppm
            );
            // About 0.08 % of 6 000 frames: 5 pictures dropped (fast) or
            // repeated (slow), and never late.
            let (corrections, overflow) = if ppm > 0.0 {
                (
                    telemetry.picture_drops,
                    inner.dropped_frames - telemetry.picture_drops,
                )
            } else {
                (telemetry.picture_repeats, inner.dropped_frames)
            };
            assert!((3..=7).contains(&corrections), "{ppm}: {corrections}");
            assert_eq!(overflow, 0, "{ppm}");
            assert_eq!(inner.late_frames, 0, "{ppm}");
        }
    }

    #[test]
    fn a_stalled_source_fades_out_and_comes_back_with_its_drift_estimate() {
        let plan = plan("udp://@:9000", RtspTransport::Auto);
        let mut inner = live_inner(&plan);
        inner.state = NetworkStreamState::Connecting;
        run_source(&mut inner, &plan, 300.0, 7_500, |_| true);
        let before = inner.clock.as_ref().unwrap().telemetry();
        // Two seconds without media: the stream stalls and its audio fades.
        run_source(&mut inner, &plan, 300.0, 50, |_| false);
        assert_eq!(inner.state, NetworkStreamState::Stalled);
        let at = f64::from(u32::try_from(inner.pulled + 1).unwrap()) / 25.0;
        let pulled = pull_frame(&mut inner, &plan, at);
        assert!(pulled.audio.iter().flatten().all(|sample| *sample == 0.0));
        let silent = run_source(&mut inner, &plan, 300.0, 1_500, |_| true);
        let after = inner.clock.as_ref().unwrap().telemetry();
        assert_eq!(inner.state, NetworkStreamState::Live);
        assert!(silent <= 1, "{silent}");
        assert_eq!(after.hard_resyncs(), 0);
        assert_eq!(after.sync.underruns, before.sync.underruns + 1);
        assert!((after.sync.estimated_drift_ppm - 300.0).abs() < 5.0);
    }

    #[test]
    fn debug_output_redacts_the_url() {
        let stream = NetworkStreamConfig::new("rtsp://admin:hunter2@cam.test/live");
        let config = NetworkInputConfig::new(&stream, 64, 36, FrameRate::new(25, 1).unwrap());
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("admin"));
    }

    #[test]
    fn rtmp_listener_arguments_listen_on_the_relay_port_and_append_the_stream_key() {
        let mut stream = NetworkStreamConfig::new("RTMP://0.0.0.0:19350/live");
        stream.settings.mode = StreamMode::Listener;
        stream.stream_key = Some("sk_9f8e".into());
        let plan = stream_plan(&stream);
        assert!(plan.listens() && plan.relays() && !plan.probes());
        assert_eq!(plan.initial_state(), NetworkStreamState::Connecting);
        assert_eq!(listen_address(&plan), Some(("0.0.0.0".into(), 19350)));
        // The child listens on loopback only, for the receiver's relay.
        let joined = args(&plan, None).join(" ");
        assert!(
            joined.contains(
                "-listen 1 -timeout 5 -protocol_whitelist rtmp,rtmps,tcp,tls \
                 -i rtmp://127.0.0.1:40000/live/sk_9f8e"
            ),
            "{joined}"
        );
        assert!(!joined.contains("19350"), "{joined}");
        assert!(!joined.contains("-rw_timeout"));
        let mut caller = NetworkStreamConfig::new("rtmp://ingest.test/live");
        caller.stream_key = Some("k1".into());
        let joined = args(&stream_plan(&caller), None).join(" ");
        assert!(joined.contains("-rw_timeout 5000000 -protocol_whitelist"));
        assert!(joined.contains("-i rtmp://ingest.test/live/k1"));
        assert!(!joined.contains("-listen"));
    }

    #[test]
    fn listeners_bind_the_url_host_and_port() {
        let listener = |url: &str| {
            let mut stream = NetworkStreamConfig::new(url);
            stream.settings.mode = StreamMode::Listener;
            stream_plan(&stream)
        };
        assert_eq!(
            listen_address(&listener("rtmp://[::]:1935/live")),
            Some(("::".into(), 1935))
        );
        assert_eq!(
            listen_address(&listener("rtmp://localhost/live")),
            Some(("localhost".into(), 1935))
        );
        let srt = listener("srt://127.0.0.1:9000");
        assert!(srt.probes() && !srt.relays());
        assert_eq!(srt.initial_state(), NetworkStreamState::Connecting);
        assert_eq!(listen_address(&srt), Some(("127.0.0.1".into(), 9000)));
        let mut rendezvous = NetworkStreamConfig::new("srt://127.0.0.1:9000");
        rendezvous.settings.mode = StreamMode::Rendezvous;
        let rendezvous = stream_plan(&rendezvous);
        assert!(!rendezvous.probes() && !rendezvous.relays());
        assert_eq!(rendezvous.initial_state(), NetworkStreamState::Listening);
        assert_eq!(
            relay_url("rtmp://0.0.0.0:1935/live/k?x=1", 40_001),
            "rtmp://127.0.0.1:40001/live/k?x=1"
        );
    }

    #[test]
    fn the_srt_probe_sends_an_induction_and_knows_its_answer() {
        let packet = srt_induction();
        // A handshake control packet to socket 0: version 4, induction.
        assert_eq!(packet[..4], [0x80, 0, 0, 0]);
        assert_eq!(packet[12..16], [0; 4]);
        assert_eq!(packet[16..20], [0, 0, 0, 4]);
        assert_eq!(packet[36..40], [0, 0, 0, 1]);
        assert!(!is_srt_induction_answer(&packet), "addressed to socket 0");
        // A listener's answer (captured from `libsrt` 1.5): version 5, the
        // SRT magic, the probe's socket as destination, and a cookie.
        let mut answer = packet;
        answer[12..16].copy_from_slice(&SRT_PROBE_SOCKET.to_be_bytes());
        answer[16..24].copy_from_slice(&[0, 0, 0, 5, 0, 0, 0x4a, 0x17]);
        answer[44..48].copy_from_slice(&[0xa4, 0x33, 0x11, 0xf1]);
        assert!(is_srt_induction_answer(&answer));
        assert!(!is_srt_induction_answer(&answer[..40]));
        let mut conclusion = answer;
        conclusion[39] = 0xff;
        assert!(!is_srt_induction_answer(&conclusion));
    }

    #[test]
    fn srt_arguments_carry_mode_latency_encryption_and_stream_id() {
        let mut stream = NetworkStreamConfig::new("srt://0.0.0.0:9000");
        stream.settings.mode = StreamMode::Listener;
        stream.settings.latency_ms = 250;
        stream.settings.key_length = fm_types::network_stream::SrtKeyLength::Aes256;
        stream.passphrase = Some("0123456789abc".into());
        stream.stream_id = Some("cam1".into());
        // Unencrypted: every option on the command line.
        let mut plain = stream.clone();
        plain.passphrase = None;
        let joined = args(&stream_plan(&plain), None).join(" ");
        assert!(
            joined.contains(
                "-mode listener -latency 250000 -listen_timeout 30000000 -timeout 5000000 \
                 -protocol_whitelist srt -i srt://0.0.0.0:9000?streamid=cam1"
            ),
            "{joined}"
        );
        assert!(stream_plan(&plain).script().is_none());
        stream.settings.mode = StreamMode::Caller;
        let caller = args(&stream_plan(&stream), None).join(" ");
        assert!(caller.contains("-f concat -safe 0 -protocol_whitelist pipe,srt -i pipe:0"));
        plain.settings.mode = StreamMode::Caller;
        let joined = args(&stream_plan(&plain), None).join(" ");
        assert!(joined.contains("-mode caller -latency 250000 -timeout"));
        assert!(!joined.contains("passphrase"));
        assert!(!stream_plan(&plain).listens());
    }

    #[test]
    fn an_encrypted_srt_passphrase_travels_in_the_stdin_script_not_argv() {
        const PASSPHRASE: &str = "it's a pass phrase";
        let mut stream = NetworkStreamConfig::new("SRT://0.0.0.0:9000");
        stream.settings.mode = StreamMode::Listener;
        stream.settings.latency_ms = 250;
        stream.settings.key_length = fm_types::network_stream::SrtKeyLength::Aes256;
        stream.passphrase = Some(PASSPHRASE.into());
        stream.stream_id = Some("cam1".into());
        let plan = stream_plan(&stream);
        let argv = args(&plan, Some(4000));
        // Neither the passphrase nor any SRT option is on the command line.
        assert!(argv.iter().all(|arg| !arg.contains(PASSPHRASE)), "{argv:?}");
        assert!(argv.iter().all(|arg| !arg.contains("pass")), "{argv:?}");
        assert!(!argv.iter().any(|arg| arg == "-mode" || arg == "-latency"));
        let input = argv.iter().position(|arg| arg == "-i").unwrap();
        assert_eq!(
            argv[input - 6..=input + 1],
            [
                "-f",
                "concat",
                "-safe",
                "0",
                "-protocol_whitelist",
                "pipe,srt",
                "-i",
                "pipe:0"
            ]
        );
        assert_eq!(
            plan.script().unwrap(),
            "ffconcat version 1.0\n\
             file 'srt://0.0.0.0:9000?streamid=cam1'\n\
             option analyzeduration 1000000\n\
             option mode 'listener'\n\
             option latency '250000'\n\
             option passphrase 'it'\\''s a pass phrase'\n\
             option pbkeylen '32'\n\
             option listen_timeout '30000000'\n\
             option timeout '5000000'\n"
        );
        // The child's echo of its scripted input reads as the stream, whose
        // secrets are then scrubbed.
        assert_eq!(
            lowercase_scheme_echo("Error opening input file pipe:0.", &plan),
            "Error opening input file SRT://0.0.0.0:9000?streamid=cam1."
        );
        // Debug output never renders it either.
        assert!(!format!("{:?}", plan.config).contains(PASSPHRASE));
        assert!(
            !plan
                .scrub(&format!("bad passphrase {PASSPHRASE}"))
                .contains(PASSPHRASE)
        );
    }

    #[test]
    fn a_stereo_pair_is_panned_out_of_the_source() {
        let mut stream = NetworkStreamConfig::new("srt://enc.test:9000");
        stream.settings.audio = fm_types::network_stream::AudioChannels::Pair56;
        let joined = args(&stream_plan(&stream), Some(4000)).join(" ");
        assert!(
            joined.contains("-af pan=stereo|c0=c4|c1=c5,aresample=48000:async=1:first_pts=0"),
            "{joined}"
        );
        stream.settings.audio = fm_types::network_stream::AudioChannels::Mix;
        let joined = args(&stream_plan(&stream), Some(4000)).join(" ");
        assert!(joined.contains("-af aresample=48000"));
    }

    #[test]
    fn log_levels_split_and_audio_layouts_count_channels() {
        assert_eq!(
            split_level("[rtmp @ 0x1] [warning] Unexpected stream x, expecting y"),
            (
                Some(LogLevel::Warning),
                "[rtmp @ 0x1] Unexpected stream x, expecting y".to_owned()
            )
        );
        assert_eq!(
            split_level("[error] Error opening input file"),
            (Some(LogLevel::Error), "Error opening input file".to_owned())
        );
        assert_eq!(
            split_level("12:00/SRT:RcvQ: rejected [info] BADSECRET"),
            (None, "12:00/SRT:RcvQ: rejected [info] BADSECRET".to_owned())
        );
        for (line, channels) in [
            (
                "  Stream #0:1[0x101]: Audio: aac (LC) ([15][0][0][0] / 0x000F), 44100 Hz, 7.1, fltp, 180 kb/s",
                Some(8),
            ),
            (
                "  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp",
                Some(2),
            ),
            (
                "  Stream #0:1: Audio: aac, 48000 Hz, 5.1(side), fltp",
                Some(6),
            ),
            (
                "  Stream #0:1: Audio: pcm_s16le, 48000 Hz, 4 channels, s16",
                Some(4),
            ),
            ("  Stream #0:1: Audio: aac, 48000 Hz, mono, fltp", Some(1)),
            ("  Stream #0:0: Video: h264, yuv420p, 1920x1080", None),
        ] {
            assert_eq!(audio_stream_channels(line), channels, "{line}");
        }
        let mut inner = Inner::default();
        for line in [
            "Input #0, flv, from 'rtmp://h/live/***':",
            "  Stream #0:1: Audio: aac (LC), 48000 Hz, 5.1, fltp",
            "Output #0, f32le, to 'http://127.0.0.1:1/t':",
            "  Stream #0:1: Audio: pcm_f32le, 48000 Hz, stereo, flt",
        ] {
            note_input_dump(&mut inner, line);
        }
        assert_eq!(inner.source_channels, 6);
    }

    #[test]
    fn debug_output_masks_the_stream_key_and_passphrase() {
        let mut stream = NetworkStreamConfig::new("rtmp://ingest.test/live");
        stream.stream_key = Some("sk_live_secret".into());
        let config = NetworkInputConfig::new(&stream, 64, 36, FrameRate::new(25, 1).unwrap());
        assert!(!format!("{config:?}").contains("sk_live_secret"));
        let mut srt = NetworkStreamConfig::new("srt://enc.test:9000");
        srt.passphrase = Some("hunter2hunter2".into());
        let config = NetworkInputConfig::new(&srt, 64, 36, FrameRate::new(25, 1).unwrap());
        assert!(!format!("{config:?}").contains("hunter2"));
        let plan = stream_plan(&srt);
        assert!(
            !plan
                .scrub("srt passphrase hunter2hunter2 rejected")
                .contains("hunter2")
        );
    }
}
