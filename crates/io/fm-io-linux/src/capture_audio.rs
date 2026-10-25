//! The sound captured beside a screen or window capture (IN-011).
//!
//! A screen capture input may capture the server's whole system output or
//! only the sound of the application owning its window. Where `ffmpeg` can
//! read that sound itself (the `PulseAudio`/`PipeWire` default output's
//! monitor, [`pulse_system_audio`]) the capture child opens it as its second
//! input. Everything else goes through an [`AudioTap`]: the daemon gathers
//! the sound from its producers, mixes it, paces it in real time on the
//! host's monotonic clock (the clock the picture is grabbed on), and serves
//! it as raw 48 kHz stereo `f32le` samples on a private local socket
//! (a Unix socket in an owner-only directory; loopback TCP on Windows) that
//! the capture child reads as its audio input ([`AudioTap::audio_input`]).
//! The picture and the sound then share one capture child, one jitter
//! buffer, and the receiver's drift loop (AU-004), so lip-sync holds.
//!
//! A tap's producers are either:
//!
//! - **`PulseAudio`/`PipeWire` application streams** (Linux,
//!   [`TapSource::PulseApplication`]): every half second `pactl list
//!   sink-inputs` lists the playback streams, and each stream whose
//!   `application.process.id` is the window's process or one of its
//!   descendants (browsers play from a child process) — or, when the window
//!   system names no process (Wayland), whose application id or binary
//!   names the window's application — is recorded on its own with `parec
//!   --monitor-stream`. Monitoring a stream does not move or re-route it:
//!   the application stays audible on its own output. Streams that come and
//!   go are followed; while none plays the tap reports
//!   [`TapState::NoStream`] and serves silence.
//! - **A helper command** ([`TapSource::Command`]): the Windows WASAPI
//!   loopback / process loopback helper or the macOS `ScreenCaptureKit`
//!   helper, writing raw 48 kHz stereo `f32le` to its standard output. A
//!   helper reports its state with standard error lines
//!   `FMAUDIO<TAB><state><TAB><detail>` (`live`, `no_stream`,
//!   `unsupported`, `error`) and exits with [`UNSUPPORTED_EXIT`] when this
//!   system cannot capture the sound asked for; any other exit is retried.
//!
//! The tap's threads never block the frame loop: dropping an [`AudioTap`]
//! only signals them, and they stop their children and remove the socket
//! on their own.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::DeviceAudioInput;

use crate::system_audio::{AUDIO_SERVER_ENVIRONMENT, run_pactl};

/// The sample rate a tap serves.
pub const SAMPLE_RATE: u32 = 48_000;
/// The channels a tap serves, interleaved.
pub const CHANNELS: usize = 2;
/// The exit status of a helper that cannot capture the sound asked for on
/// this system; it is not restarted.
pub const UNSUPPORTED_EXIT: i32 = 3;
/// The prefix of a helper's status lines on standard error.
pub const STATUS_PREFIX: &str = "FMAUDIO";
/// The `PulseAudio` source naming the default output's monitor, which
/// `PipeWire`'s `pipewire-pulse` honours too.
pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

/// How often the tap paces out samples.
const TICK: Duration = Duration::from_millis(10);
/// How often application streams are listed.
const POLL: Duration = Duration::from_millis(500);
/// The bound of one `pactl` call.
const PACTL_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a failed helper or stream recorder waits before it restarts.
const RESTART_DELAY: Duration = Duration::from_secs(1);
/// Samples per channel a stream buffers before it is mixed (30 ms): the
/// jitter its producer's chunks need.
const PRIME_FRAMES: usize = 1_440;
/// Samples per channel past which a stream drops its oldest (150 ms): a
/// producer whose clock runs fast is pulled back to [`PRIME_FRAMES`].
const MAX_FRAMES: usize = 7_200;
/// The most samples per channel one tick catches up after a stall (1 s).
const MAX_CATCH_UP: u64 = 48_000;
/// The longest a write to the capture child may block.
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
/// The longest standard error line of a helper kept.
const MAX_STATUS_LINE: usize = 512;

/// One playback stream (`pactl list sink-inputs`, C locale).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SinkInput {
    /// The sink input's index, which `parec --monitor-stream` names.
    pub index: u32,
    /// `application.process.id`.
    pub pid: Option<u32>,
    /// `application.process.binary`.
    pub binary: String,
    /// `application.name`.
    pub name: String,
    /// `application.id` (Flatpak and desktop applications).
    pub app_id: String,
    /// `pipewire.access.portal.app_id` (portal-confined applications).
    pub portal_app_id: String,
}

/// Parses `pactl list sink-inputs` (C locale): one stream per `Sink Input
/// #` block, with the application properties that identify its owner.
#[must_use]
pub fn parse_pactl_sink_inputs(text: &str) -> Vec<SinkInput> {
    let mut inputs = Vec::new();
    let mut current: Option<SinkInput> = None;
    for line in text.lines() {
        if let Some(index) = line.strip_prefix("Sink Input #") {
            inputs.extend(current.take());
            current = index.trim().parse().ok().map(|index| SinkInput {
                index,
                ..SinkInput::default()
            });
            continue;
        }
        let Some(input) = current.as_mut() else {
            continue;
        };
        // Properties sit two tabs in: `\t\tkey = "value"`.
        let Some(property) = line.strip_prefix("\t\t") else {
            continue;
        };
        let Some((key, value)) = property.split_once(" = ") else {
            continue;
        };
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value)
            .to_owned();
        match key.trim() {
            "application.process.id" => input.pid = value.parse().ok().filter(|pid| *pid != 0),
            "application.process.binary" => input.binary = value,
            "application.name" => input.name = value,
            "application.id" => input.app_id = value,
            "pipewire.access.portal.app_id" => input.portal_app_id = value,
            _ => {}
        }
    }
    inputs.extend(current);
    inputs
}

/// Which application's sound a window capture takes: its process (the
/// window system's `_NET_WM_PID` on X11, the owning process on Windows and
/// macOS), or, when no process is known (Wayland), its application id.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AppAudioKey {
    pub pid: Option<u32>,
    /// The application id or name, matched without regard to ASCII case;
    /// used only without a process.
    pub app: String,
}

impl AppAudioKey {
    /// Whether the key names an application at all.
    #[must_use]
    pub fn is_known(&self) -> bool {
        self.pid.is_some() || !self.app.trim().is_empty()
    }

    /// Whether `input` plays for this application: its process is the
    /// key's process or a descendant of it (`parent_of` walks up the process
    /// tree), or, without a process, one of its application ids or its
    /// binary is the key's application.
    #[must_use]
    pub fn matches(&self, input: &SinkInput, parent_of: &dyn Fn(u32) -> Option<u32>) -> bool {
        if let Some(root) = self.pid {
            // Every process descends from init: it names no application.
            if root <= 1 {
                return false;
            }
            let mut pid = input.pid;
            // A bounded walk: a process tree is never this deep.
            for _ in 0..64 {
                match pid {
                    Some(current) if current == root => return true,
                    Some(current) if current > 1 => pid = parent_of(current),
                    _ => return false,
                }
            }
            return false;
        }
        let app = self.app.trim();
        !app.is_empty()
            && [
                &input.app_id,
                &input.portal_app_id,
                &input.binary,
                &input.name,
            ]
            .into_iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(app))
    }
}

/// The parent of process `pid`, read from `/proc/<pid>/stat`; `None` where
/// there is no procfs or the process is gone.
#[must_use]
pub fn proc_parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat_parent(&stat)
}

/// The parent process of a `/proc/<pid>/stat` line: the field after the
/// state, which follows the command name's last `)` (the name itself may
/// hold spaces and parentheses).
#[must_use]
pub fn parse_stat_parent(stat: &str) -> Option<u32> {
    let (_, after) = stat.rsplit_once(')')?;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// The `ffmpeg` input reading the `PulseAudio`/`PipeWire` default output's
/// monitor: the server's whole system output, read by the capture child
/// beside its picture, with short fragments so the sound is not late.
#[must_use]
pub fn pulse_system_audio() -> DeviceAudioInput {
    DeviceAudioInput {
        format: "pulse".to_owned(),
        options: vec![
            ("sample_rate".to_owned(), SAMPLE_RATE.to_string()),
            ("channels".to_owned(), CHANNELS.to_string()),
            // 20 ms of 48 kHz stereo 16-bit samples.
            ("fragment_size".to_owned(), "3840".to_owned()),
        ],
        path: DEFAULT_MONITOR.to_owned(),
    }
}

/// The environment a capture child reading `PulseAudio` keeps beside
/// `PATH`: the server, its cookie, and the runtime directory they live in.
#[must_use]
pub fn audio_server_environment() -> Vec<String> {
    AUDIO_SERVER_ENVIRONMENT
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// A helper command writing raw 48 kHz stereo `f32le` to its standard
/// output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProducerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment variables the helper keeps beside `PATH`, `HOME`, and
    /// `TMPDIR` (and, on Windows, `SystemRoot` and `WINDIR`).
    pub environment: Vec<String>,
}

/// Where a tap's sound comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TapSource {
    /// The playback streams of one application on the `PulseAudio` (or
    /// `pipewire-pulse`) server, through `pactl` and `parec`.
    PulseApplication {
        key: AppAudioKey,
        pactl: OsString,
        parec: OsString,
    },
    /// One helper's standard output.
    Command(ProducerCommand),
}

impl TapSource {
    /// The streams of the application `key` names, through the `pactl` and
    /// `parec` on `PATH`.
    #[must_use]
    pub fn pulse_application(key: AppAudioKey) -> Self {
        Self::PulseApplication {
            key,
            pactl: OsString::from("pactl"),
            parec: OsString::from("parec"),
        }
    }
}

/// What a tap is doing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TapState {
    /// Nothing is known yet.
    #[default]
    Starting,
    /// At least one producer delivers sound.
    Live,
    /// The application plays no stream yet: the tap serves silence.
    NoStream,
    /// This system cannot capture the sound asked for, and why.
    Unsupported(String),
    /// The producers failed (and are retried), and why.
    Failed(String),
}

/// Mixes producers' interleaved stereo samples, each behind its own small
/// jitter buffer.
#[derive(Debug, Default)]
pub struct Mixer {
    streams: BTreeMap<u64, Queue>,
}

#[derive(Debug, Default)]
struct Queue {
    samples: VecDeque<f32>,
    /// Buffered to [`PRIME_FRAMES`] since its last underrun.
    primed: bool,
}

impl Mixer {
    /// Appends interleaved stereo `samples` to `stream`'s buffer.
    pub fn push(&mut self, stream: u64, samples: &[f32]) {
        let queue = self.streams.entry(stream).or_default();
        queue.samples.extend(samples);
        let excess = queue.samples.len().saturating_sub(MAX_FRAMES * CHANNELS);
        if excess > 0 {
            // A fast producer: drop back to the priming depth.
            let drop = queue.samples.len() - PRIME_FRAMES * CHANNELS;
            queue.samples.drain(..drop);
        }
    }

    /// Forgets `stream`.
    pub fn remove(&mut self, stream: u64) {
        self.streams.remove(&stream);
    }

    /// The streams mixed.
    #[must_use]
    pub fn streams(&self) -> usize {
        self.streams.len()
    }

    /// The next `frames` samples per channel, interleaved: the sum of every
    /// primed stream, silence where none is. A stream that runs dry is
    /// padded with silence and buffers again before it is mixed again.
    #[must_use]
    pub fn take(&mut self, frames: usize) -> Vec<f32> {
        let wanted = frames * CHANNELS;
        let mut mixed = vec![0.0_f32; wanted];
        for queue in self.streams.values_mut() {
            if !queue.primed && queue.samples.len() >= PRIME_FRAMES * CHANNELS {
                queue.primed = true;
            }
            if !queue.primed {
                continue;
            }
            let available = queue.samples.len().min(wanted);
            for (out, sample) in mixed.iter_mut().zip(queue.samples.drain(..available)) {
                *out += sample;
            }
            if available < wanted {
                queue.primed = false;
            }
        }
        mixed
    }
}

/// Converts whole interleaved frames of `f32le` bytes, keeping a partial
/// frame in `carry` for the next chunk.
fn decode_f32le(carry: &mut Vec<u8>, chunk: &[u8]) -> Vec<f32> {
    carry.extend_from_slice(chunk);
    let frame_bytes = 4 * CHANNELS;
    let whole = carry.len() / frame_bytes * frame_bytes;
    let samples = carry[..whole]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .map(|sample| if sample.is_finite() { sample } else { 0.0 })
        .collect();
    carry.drain(..whole);
    samples
}

struct Inner {
    shutdown: bool,
    mixer: Mixer,
    state: TapState,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn shutting_down(&self) -> bool {
        self.lock().shutdown
    }

    /// Waits up to `timeout` or until shutdown; whether it shut down.
    fn wait(&self, timeout: Duration) -> bool {
        let inner = self.lock();
        if inner.shutdown {
            return true;
        }
        let (inner, _) = self
            .wake
            .wait_timeout(inner, timeout)
            .unwrap_or_else(PoisonError::into_inner);
        inner.shutdown
    }

    fn set_state(&self, state: TapState) {
        self.lock().state = state;
    }
}

/// The local endpoint a capture child reads a tap's sound from.
enum Listener {
    #[cfg(unix)]
    Unix {
        listener: std::os::unix::net::UnixListener,
        directory: PathBuf,
    },
    #[cfg(not(unix))]
    Tcp(std::net::TcpListener),
}

/// A connected capture child.
enum Consumer {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(not(unix))]
    Tcp(std::net::TcpStream),
}

impl Write for Consumer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(bytes),
            #[cfg(not(unix))]
            Self::Tcp(stream) => stream.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

static NEXT_TAP: AtomicU64 = AtomicU64::new(1);

impl Listener {
    /// Binds a private endpoint; its `ffmpeg` URL.
    fn bind() -> io::Result<(Self, String)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            let directory = std::env::temp_dir().join(format!(
                "freemix-audio-{}-{}",
                std::process::id(),
                NEXT_TAP.fetch_add(1, Ordering::Relaxed)
            ));
            // A stale directory of an earlier daemon with this process id.
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
            let path = directory.join("tap.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            let url = format!("unix:{}", path.display());
            Ok((
                Self::Unix {
                    listener,
                    directory,
                },
                url,
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = NEXT_TAP.fetch_add(1, Ordering::Relaxed);
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            listener.set_nonblocking(true)?;
            let url = format!("tcp://127.0.0.1:{}", listener.local_addr()?.port());
            Ok((Self::Tcp(listener), url))
        }
    }

    /// The newest waiting connection, if any.
    fn accept(&self) -> Option<Consumer> {
        let mut newest = None;
        loop {
            match self {
                #[cfg(unix)]
                Self::Unix { listener, .. } => match listener.accept() {
                    Ok((stream, _)) => {
                        if stream.set_nonblocking(false).is_ok()
                            && stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_ok()
                        {
                            newest = Some(Consumer::Unix(stream));
                        }
                    }
                    Err(_) => return newest,
                },
                #[cfg(not(unix))]
                Self::Tcp(listener) => match listener.accept() {
                    Ok((stream, peer)) => {
                        if peer.ip().is_loopback()
                            && stream.set_nonblocking(false).is_ok()
                            && stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_ok()
                        {
                            let _ = stream.set_nodelay(true);
                            newest = Some(Consumer::Tcp(stream));
                        }
                    }
                    Err(_) => return newest,
                },
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let Self::Unix { directory, .. } = self;
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

/// A running tap: producers, their mix, and the endpoint serving it.
/// Dropping it stops everything without waiting.
pub struct AudioTap {
    shared: Arc<Shared>,
    url: String,
}

impl std::fmt::Debug for AudioTap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioTap")
            .field("url", &self.url)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl AudioTap {
    /// Binds the endpoint and starts gathering `source`.
    ///
    /// # Errors
    ///
    /// Returns the error binding the endpoint or starting a thread.
    pub fn start(source: TapSource) -> io::Result<Self> {
        let (listener, url) = Listener::bind()?;
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                shutdown: false,
                mixer: Mixer::default(),
                state: TapState::Starting,
            }),
            wake: Condvar::new(),
        });
        {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("freemix-audio-tap".into())
                .spawn(move || pace(&shared, &listener))?;
        }
        let follower = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("freemix-audio-tap-source".into())
                .spawn(move || match source {
                    TapSource::PulseApplication { key, pactl, parec } => {
                        follow_pulse(&shared, &key, &pactl, &parec);
                    }
                    TapSource::Command(command) => follow_command(&shared, &command),
                })
        };
        if let Err(error) = follower {
            shared.lock().shutdown = true;
            shared.wake.notify_all();
            return Err(error);
        }
        Ok(Self { shared, url })
    }

    /// The capture child's audio input reading this tap.
    #[must_use]
    pub fn audio_input(&self) -> DeviceAudioInput {
        DeviceAudioInput {
            format: "f32le".to_owned(),
            options: vec![
                ("sample_rate".to_owned(), SAMPLE_RATE.to_string()),
                ("ch_layout".to_owned(), "stereo".to_owned()),
            ],
            path: self.url.clone(),
        }
    }

    /// The endpoint's `ffmpeg` URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// What the tap is doing now.
    #[must_use]
    pub fn state(&self) -> TapState {
        self.shared.lock().state.clone()
    }
}

impl Drop for AudioTap {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

/// Paces the mix out to the newest connected capture child, one tick at a
/// time on the monotonic clock, until shutdown.
fn pace(shared: &Shared, listener: &Listener) {
    let started = Instant::now();
    let mut sent: u64 = 0;
    let mut consumer: Option<Consumer> = None;
    let mut bytes = Vec::new();
    loop {
        if shared.wait(TICK) {
            return;
        }
        if let Some(newest) = listener.accept() {
            consumer = Some(newest);
        }
        let elapsed =
            u64::try_from(started.elapsed().as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000)
                .unwrap_or(u64::MAX);
        let mut due = elapsed.saturating_sub(sent);
        if due > MAX_CATCH_UP {
            // The thread stalled: skip ahead rather than burst.
            sent = elapsed - MAX_CATCH_UP;
            due = MAX_CATCH_UP;
        }
        sent += due;
        let samples = shared
            .lock()
            .mixer
            .take(usize::try_from(due).unwrap_or(usize::MAX));
        let Some(writer) = consumer.as_mut() else {
            continue;
        };
        bytes.clear();
        bytes.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        if writer.write_all(&bytes).is_err() {
            consumer = None;
        }
    }
}

/// Starts a reader of `stdout` feeding mixer stream `stream`; the stream is
/// forgotten when the producer ends. `on_data` runs on the first samples.
fn spawn_reader(
    shared: &Arc<Shared>,
    stream: u64,
    stdout: impl Read + Send + 'static,
    on_data: impl FnOnce(&Shared) + Send + 'static,
) {
    let shared = Arc::clone(shared);
    let _ = std::thread::Builder::new()
        .name("freemix-audio-tap-read".into())
        .spawn(move || {
            let mut stdout = stdout;
            let mut chunk = vec![0_u8; 4_096];
            let mut carry = Vec::new();
            let mut on_data = Some(on_data);
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let samples = decode_f32le(&mut carry, &chunk[..read]);
                        if samples.is_empty() {
                            continue;
                        }
                        if let Some(on_data) = on_data.take() {
                            on_data(&shared);
                        }
                        let mut inner = shared.lock();
                        if inner.shutdown {
                            break;
                        }
                        inner.mixer.push(stream, &samples);
                    }
                }
            }
            shared.lock().mixer.remove(stream);
        });
}

static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

/// A command with a cleared environment keeping `PATH`, `names`, and the C
/// locale, its standard input closed.
fn quiet_command(program: &std::ffi::OsStr, names: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.env_clear().env("LC_ALL", "C").stdin(Stdio::null());
    #[cfg(windows)]
    let base: &[&str] = &["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"];
    #[cfg(not(windows))]
    let base: &[&str] = &["PATH"];
    for name in base.iter().chain(names) {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

/// One `parec` recording one sink input.
struct Recorder {
    child: Child,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Follows the application's playback streams until shutdown.
fn follow_pulse(shared: &Arc<Shared>, key: &AppAudioKey, pactl: &OsString, parec: &OsString) {
    if !key.is_known() {
        shared.set_state(TapState::Unsupported(
            "the window system does not say which application owns this window".to_owned(),
        ));
        return;
    }
    let mut recorders: BTreeMap<u32, Recorder> = BTreeMap::new();
    // When a stream's recorder last failed to start or stopped early.
    let mut failed_at: BTreeMap<u32, Instant> = BTreeMap::new();
    while !shared.shutting_down() {
        match run_pactl(pactl, &["list", "sink-inputs"], PACTL_TIMEOUT) {
            Err(error) => {
                recorders.clear();
                shared.set_state(TapState::Failed(error));
            }
            Ok(listing) => {
                let wanted: Vec<u32> = parse_pactl_sink_inputs(&listing)
                    .iter()
                    .filter(|input| key.matches(input, &proc_parent))
                    .map(|input| input.index)
                    .collect();
                recorders.retain(|index, _| wanted.contains(index));
                failed_at.retain(|index, _| wanted.contains(index));
                // A recorder whose stream ended (or that failed) is
                // replaced after a pause.
                recorders.retain(|index, recorder| {
                    let running = matches!(recorder.child.try_wait(), Ok(None));
                    if !running {
                        failed_at.insert(*index, Instant::now());
                    }
                    running
                });
                let mut error = None;
                for index in wanted {
                    if recorders.contains_key(&index)
                        || failed_at
                            .get(&index)
                            .is_some_and(|at| at.elapsed() < RESTART_DELAY)
                    {
                        continue;
                    }
                    match start_recorder(shared, parec, index) {
                        Ok(recorder) => {
                            recorders.insert(index, recorder);
                        }
                        Err(reason) => {
                            failed_at.insert(index, Instant::now());
                            error = Some(reason);
                        }
                    }
                }
                shared.set_state(match (recorders.is_empty(), error) {
                    (true, Some(error)) => TapState::Failed(error),
                    (true, None) => TapState::NoStream,
                    (false, _) => TapState::Live,
                });
            }
        }
        if shared.wait(POLL) {
            break;
        }
    }
    // Dropping the recorders stops them.
}

/// Starts `parec` on sink input `index`.
fn start_recorder(shared: &Arc<Shared>, parec: &OsString, index: u32) -> Result<Recorder, String> {
    let mut command = quiet_command(parec, AUDIO_SERVER_ENVIRONMENT);
    command
        .arg(format!("--monitor-stream={index}"))
        .args([
            "--raw",
            "--format=float32le",
            "--rate=48000",
            "--channels=2",
            "--latency-msec=10",
            "--client-name=freemix",
            "--stream-name=freemix window audio",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            "parec is not installed".to_owned()
        } else {
            format!("parec did not start: {error}")
        }
    })?;
    let stream = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(shared, stream, stdout, |_| {});
    }
    Ok(Recorder { child })
}

/// The state a helper's status line reports, if it is one.
fn status_line(line: &str) -> Option<TapState> {
    let mut fields = line.trim_end().splitn(3, '\t');
    if fields.next()? != STATUS_PREFIX {
        return None;
    }
    let state = fields.next()?;
    let detail = fields.next().unwrap_or_default().trim().to_owned();
    let reason = |fallback: &str| {
        if detail.is_empty() {
            fallback.to_owned()
        } else {
            detail.clone()
        }
    };
    Some(match state {
        "live" => TapState::Live,
        "no_stream" => TapState::NoStream,
        "unsupported" => TapState::Unsupported(reason("audio capture is not supported here")),
        "error" => TapState::Failed(reason("the audio helper failed")),
        _ => return None,
    })
}

/// Runs the helper, restarting it after a failure, until shutdown or it
/// reports that it cannot work here.
fn follow_command(shared: &Arc<Shared>, producer: &ProducerCommand) {
    let names: Vec<&str> = ["HOME", "TMPDIR"]
        .into_iter()
        .chain(producer.environment.iter().map(String::as_str))
        .collect();
    while !shared.shutting_down() {
        let mut command = quiet_command(producer.program.as_os_str(), &names);
        command
            .args(&producer.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                shared.set_state(TapState::Failed(
                    if error.kind() == io::ErrorKind::NotFound {
                        "the audio capture helper is unavailable".to_owned()
                    } else {
                        format!("the audio capture helper did not start: {error}")
                    },
                ));
                if shared.wait(RESTART_DELAY) {
                    return;
                }
                continue;
            }
        };
        shared.set_state(TapState::Starting);
        let stream = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(shared, stream, stdout, |shared| {
                let mut inner = shared.lock();
                if inner.state == TapState::Starting {
                    inner.state = TapState::Live;
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let shared = Arc::clone(shared);
            let _ = std::thread::Builder::new()
                .name("freemix-audio-tap-status".into())
                .spawn(move || {
                    let mut lines = BufReader::new(stderr);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match (&mut lines)
                            .take(MAX_STATUS_LINE as u64)
                            .read_line(&mut line)
                        {
                            Ok(0) | Err(_) => return,
                            Ok(_) => {
                                if let Some(state) = status_line(&line) {
                                    shared.set_state(state);
                                }
                            }
                        }
                    }
                });
        }
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if shared.wait(Duration::from_millis(50)) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                }
                Err(_) => break None,
            }
        };
        // Let the status reader catch the helper's last words.
        std::thread::sleep(Duration::from_millis(50));
        let unsupported = status.and_then(|status| status.code()) == Some(UNSUPPORTED_EXIT);
        let mut inner = shared.lock();
        inner.mixer.remove(stream);
        match &inner.state {
            TapState::Unsupported(_) => {}
            _ if unsupported => {
                inner.state =
                    TapState::Unsupported("audio capture is not supported here".to_owned());
            }
            TapState::Failed(_) => {}
            _ => {
                inner.state = TapState::Failed(match status {
                    Some(status) => format!("the audio capture helper stopped ({status})"),
                    None => "the audio capture helper stopped".to_owned(),
                });
            }
        }
        if matches!(inner.state, TapState::Unsupported(_)) {
            // Nothing will change: serve silence until shutdown.
            drop(inner);
            while !shared.wait(Duration::from_hours(1)) {}
            return;
        }
        drop(inner);
        if shared.wait(RESTART_DELAY) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINK_INPUTS: &str = "Sink Input #12\n\
\tDriver: protocol-native.c\n\
\tSink: 0\n\
\tCorked: no\n\
\tProperties:\n\
\t\tmedia.name = \"Playback\"\n\
\t\tapplication.name = \"Firefox\"\n\
\t\tapplication.process.id = \"4120\"\n\
\t\tapplication.process.binary = \"firefox\"\n\
\n\
Sink Input #13\n\
\tProperties:\n\
\t\tapplication.name = \"Lavf60.16.100\"\n\
\t\tapplication.process.id = \"77\"\n\
\t\tapplication.process.binary = \"ffmpeg\"\n\
\t\tapplication.id = \"org.example.Player\"\n\
\t\tpipewire.access.portal.app_id = \"org.example.Player\"\n\
Sink Input #x\n\
\t\tapplication.process.id = \"5\"\n";

    #[test]
    fn sink_inputs_are_parsed_with_their_owners() {
        let inputs = parse_pactl_sink_inputs(SINK_INPUTS);
        assert_eq!(
            inputs,
            [
                SinkInput {
                    index: 12,
                    pid: Some(4_120),
                    binary: "firefox".into(),
                    name: "Firefox".into(),
                    app_id: String::new(),
                    portal_app_id: String::new(),
                },
                SinkInput {
                    index: 13,
                    pid: Some(77),
                    binary: "ffmpeg".into(),
                    name: "Lavf60.16.100".into(),
                    app_id: "org.example.Player".into(),
                    portal_app_id: "org.example.Player".into(),
                },
            ]
        );
        assert_eq!(parse_pactl_sink_inputs(""), []);
    }

    #[test]
    fn streams_match_the_window_process_tree_or_its_application_id() {
        let inputs = parse_pactl_sink_inputs(SINK_INPUTS);
        // 4120 is a child of 4000, which is a child of 1.
        let parent_of = |pid: u32| match pid {
            4_120 => Some(4_000),
            4_000 | 77 => Some(1),
            _ => None,
        };
        let by_pid = |pid| AppAudioKey {
            pid: Some(pid),
            app: "ignored".into(),
        };
        assert!(by_pid(4_120).matches(&inputs[0], &parent_of));
        assert!(by_pid(4_000).matches(&inputs[0], &parent_of), "a child");
        assert!(!by_pid(4_000).matches(&inputs[1], &parent_of));
        assert!(!by_pid(1).matches(&inputs[1], &parent_of), "never init");
        // A cycle in a broken tree ends.
        assert!(!by_pid(9).matches(&inputs[0], &|pid| Some(pid)));
        let by_app = |app: &str| AppAudioKey {
            pid: None,
            app: app.into(),
        };
        assert!(by_app("org.example.player").matches(&inputs[1], &parent_of));
        assert!(by_app("Firefox").matches(&inputs[0], &parent_of));
        assert!(!by_app("Firefox").matches(&inputs[1], &parent_of));
        assert!(!by_app(" ").matches(&inputs[0], &parent_of));
        assert!(!AppAudioKey::default().is_known());
        assert!(by_app("x").is_known());
    }

    #[test]
    fn stat_lines_name_the_parent_even_with_odd_command_names() {
        assert_eq!(
            parse_stat_parent("4120 (Web Content) S 4000 4000 0 0 -1"),
            Some(4_000)
        );
        assert_eq!(parse_stat_parent("7 (a) b) (c) R 1 7 7"), Some(1));
        assert_eq!(parse_stat_parent("garbage"), None);
        #[cfg(target_os = "linux")]
        assert_eq!(
            proc_parent(std::process::id()),
            Some(std::os::unix::process::parent_id())
        );
    }

    #[test]
    fn the_mixer_primes_each_stream_and_pads_underruns() {
        let mut mixer = Mixer::default();
        assert_eq!(mixer.take(4), vec![0.0; 8]);
        // Below the priming depth: not mixed yet.
        mixer.push(1, &vec![0.25; (PRIME_FRAMES - 1) * CHANNELS]);
        assert!(mixer.take(10).iter().all(|sample| *sample == 0.0));
        mixer.push(1, &[0.25, 0.25]);
        mixer.push(2, &vec![0.5; PRIME_FRAMES * CHANNELS]);
        let both = mixer.take(PRIME_FRAMES - 10);
        assert!(both.iter().all(|sample| (*sample - 0.75).abs() < 1e-6));
        // Stream 1 holds 10 more frames, stream 2 holds 10: an underrun
        // pads with silence and both buffer again.
        let padded = mixer.take(20);
        assert!(
            padded[..20]
                .iter()
                .all(|sample| (*sample - 0.75).abs() < 1e-6)
        );
        assert!(padded[20..].iter().all(|sample| *sample == 0.0));
        mixer.push(1, &[1.0; 4]);
        assert!(
            mixer.take(2).iter().all(|sample| *sample == 0.0),
            "re-priming"
        );
        // A runaway producer is pulled back to the priming depth.
        mixer.push(3, &vec![0.1; (MAX_FRAMES + 1) * CHANNELS]);
        assert_eq!(mixer.streams[&3].samples.len(), PRIME_FRAMES * CHANNELS);
        mixer.remove(1);
        mixer.remove(2);
        assert_eq!(mixer.streams(), 1);
    }

    #[test]
    fn f32le_bytes_decode_across_chunk_boundaries() {
        let bytes: Vec<u8> = [0.5_f32, -0.25, 1.0, f32::NAN]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut carry = Vec::new();
        assert_eq!(decode_f32le(&mut carry, &bytes[..5]), Vec::<f32>::new());
        assert_eq!(decode_f32le(&mut carry, &bytes[5..11]), vec![0.5, -0.25]);
        assert_eq!(decode_f32le(&mut carry, &bytes[11..]), vec![1.0, 0.0]);
        assert_eq!(carry, Vec::<u8>::new());
    }

    #[test]
    fn helper_status_lines_are_read() {
        assert_eq!(status_line("FMAUDIO\tlive\n"), Some(TapState::Live));
        assert_eq!(status_line("FMAUDIO\tno_stream"), Some(TapState::NoStream));
        assert_eq!(
            status_line("FMAUDIO\tunsupported\tneeds Windows 10 2004\r\n"),
            Some(TapState::Unsupported("needs Windows 10 2004".into()))
        );
        assert_eq!(
            status_line("FMAUDIO\terror"),
            Some(TapState::Failed("the audio helper failed".into()))
        );
        assert_eq!(status_line("warning: something"), None);
        assert_eq!(status_line("FMAUDIO\tdancing"), None);
    }

    #[test]
    fn the_system_audio_input_reads_the_default_monitor() {
        let input = pulse_system_audio();
        assert_eq!(input.format, "pulse");
        assert_eq!(input.path, "@DEFAULT_MONITOR@");
        assert!(audio_server_environment().contains(&"XDG_RUNTIME_DIR".to_owned()));
    }

    /// Reads what a tap serves for about `duration`.
    #[cfg(unix)]
    fn consume(tap: &AudioTap, duration: Duration) -> Vec<f32> {
        let path = tap.url().strip_prefix("unix:").unwrap().to_owned();
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let deadline = Instant::now() + duration;
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4_096];
        while Instant::now() < deadline {
            if let Ok(read) = stream.read(&mut chunk) {
                bytes.extend_from_slice(&chunk[..read]);
            }
        }
        decode_f32le(&mut Vec::new(), &bytes)
    }

    /// A helper's sound is served paced in real time, and its status lines
    /// and unsupported exit reach the tap's state.
    #[cfg(unix)]
    #[test]
    fn a_helper_command_is_served_and_its_state_followed() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping: ffmpeg is unavailable");
            return;
        }
        let tone = AudioTap::start(TapSource::Command(ProducerCommand {
            program: "ffmpeg".into(),
            args: [
                "-hide_banner",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:r=48000",
                "-ac",
                "2",
                "-f",
                "f32le",
                "-",
            ]
            .map(str::to_owned)
            .to_vec(),
            environment: Vec::new(),
        }))
        .unwrap();
        assert!(tone.audio_input().path.starts_with("unix:"));
        let samples = consume(&tone, Duration::from_millis(1_200));
        let frames = samples.len() / CHANNELS;
        // Paced in real time: about 1.2 s of samples, not a burst.
        assert!(
            (38_400..=72_000).contains(&frames),
            "{frames} frames in 1.2 s"
        );
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.05, "the tone is served (peak {peak})");
        assert_eq!(tone.state(), TapState::Live);
        let directory = PathBuf::from(tone.url().strip_prefix("unix:").unwrap())
            .parent()
            .unwrap()
            .to_owned();
        drop(tone);
        let deadline = Instant::now() + Duration::from_secs(5);
        while directory.exists() {
            assert!(Instant::now() < deadline, "the socket is removed");
            std::thread::sleep(Duration::from_millis(10));
        }

        let unsupported = AudioTap::start(TapSource::Command(ProducerCommand {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'FMAUDIO\\tunsupported\\tper-application audio needs Windows 10 2004\\n' \
                 >&2; exit 3"
                    .into(),
            ],
            environment: Vec::new(),
        }))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(unsupported.state(), TapState::Unsupported(_)) {
            assert!(Instant::now() < deadline, "{:?}", unsupported.state());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unsupported.state(),
            TapState::Unsupported("per-application audio needs Windows 10 2004".into())
        );
        // Still serves silence.
        let silence = consume(&unsupported, Duration::from_millis(300));
        assert_ne!(silence, Vec::<f32>::new());
        assert!(silence.iter().all(|sample| *sample == 0.0));

        let missing = AudioTap::start(TapSource::Command(ProducerCommand {
            program: "fm-io-linux-no-such-helper".into(),
            args: Vec::new(),
            environment: Vec::new(),
        }))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while missing.state() == TapState::Starting {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            missing.state(),
            TapState::Failed("the audio capture helper is unavailable".into())
        );
    }

    /// An application tap without a usable `pactl` fails, and one whose
    /// window names no application is unsupported.
    #[test]
    fn application_taps_report_missing_tools_and_unknown_owners() {
        let tap = AudioTap::start(TapSource::PulseApplication {
            key: AppAudioKey {
                pid: Some(4_120),
                app: String::new(),
            },
            pactl: "fm-io-linux-no-such-pactl".into(),
            parec: "fm-io-linux-no-such-parec".into(),
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tap.state() == TapState::Starting {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            tap.state(),
            TapState::Failed("pactl is not installed".into())
        );
        let unknown =
            AudioTap::start(TapSource::pulse_application(AppAudioKey::default())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while unknown.state() == TapState::Starting {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(unknown.state(), TapState::Unsupported(_)));
    }
}
