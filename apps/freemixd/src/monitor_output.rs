//! The headphone output: the monitor bus on a local audio device (AU-001).
//!
//! The native Master renders a monitor bus beside Program (Master, or the
//! PFL/AFL solo). [`MonitorOutput`] plays that bus on one local audio output
//! device at its own level and mute, independent of Program, recordings,
//! streams, and the monitor meters.
//!
//! The device is opened through an `FFmpeg` child that reads interleaved
//! little-endian `f32` PCM on its standard input and writes the ALSA output
//! device (`-f alsa <device>`). The render loop never waits on the device:
//! each block is interleaved with the level applied (ramped linearly across
//! the block from the previous level, so a level move never clicks), clamped
//! to full scale, and handed to a dedicated writer thread through a small
//! [`BlockQueue`].
//!
//! # Latency
//!
//! Latency the device builds up is never kept (see [`LatencyPolicy`]): a
//! full queue drops its oldest block, so the queue holds at most
//! [`MONITOR_QUEUE_BLOCKS`] fresh blocks; the writer drops a block that
//! waited longer than [`MONITOR_MAX_BLOCK_AGE`]; and a write that blocked
//! (the child's input pipe was full, as it is after `FFmpeg` started, or
//! once a device clock running slow has caught up with the pipe) skips
//! blocks until the pipe backlog is drained to about
//! [`MONITOR_PIPE_TARGET`]. Every dropped block is counted.
//!
//! # Failure
//!
//! A write error, an exited child, or a device that stopped reading (a
//! write blocked for [`MONITOR_STALL_AFTER`]) marks the output failed; a
//! stalled child is killed. Each failure is logged as a
//! `FREEMIXD_MONITOR_OUTPUT_FAILURE` record carrying a bounded, redacted
//! tail of the child's standard error.
//!
//! # Hot-plug
//!
//! A failed device is retried on its own (Protocol 2.102): the first retry
//! waits [`MONITOR_RETRY_FIRST`], doubling after each failed attempt up to
//! [`MONITOR_RETRY_MAX`], and a device that played for
//! [`MONITOR_RETRY_RESET_AFTER`] starts the back-off over. Before an
//! attempt the slot asks whether the device is present (on Linux, whether its ALSA card and PCM are in `/proc/asound`): an
//! unplugged device is not opened, only looked for again every
//! [`MONITOR_PRESENCE_POLL`], and is opened as soon as it is back. The
//! output reads failed until a reopened device has played for
//! [`MONITOR_RETRY_PROBATION`]. Choosing the device again retries it at
//! once; a sample-rate or channel change never reopens a failed device
//! early.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read as _, Write as _};
use std::process::{Child, ChildStderr, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Blocks the writer queue holds; a newer block replaces the oldest.
pub const MONITOR_QUEUE_BLOCKS: usize = 2;

/// A queued block older than this when the writer reaches it is stale and
/// dropped.
pub const MONITOR_MAX_BLOCK_AGE: Duration = Duration::from_millis(100);

/// The input pipe capacity the writer assumes when it drains a backlog:
/// the Linux default.
pub const MONITOR_PIPE_BYTES: usize = 65_536;

/// The audio a drained pipe backlog keeps queued for the device.
pub const MONITOR_PIPE_TARGET: Duration = Duration::from_millis(40);

/// A write blocked at least this long found the input pipe full.
pub const MONITOR_BLOCKED_WRITE: Duration = Duration::from_millis(10);

/// A write blocked this long means the device stopped reading.
pub const MONITOR_STALL_AFTER: Duration = Duration::from_secs(3);

/// The wait before the first retry of a failed device.
pub const MONITOR_RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between retries of a failed device.
pub const MONITOR_RETRY_MAX: Duration = Duration::from_secs(30);

/// A device that played this long starts its retry back-off over.
pub const MONITOR_RETRY_RESET_AFTER: Duration = Duration::from_secs(10);

/// How often an unplugged device is looked for.
pub const MONITOR_PRESENCE_POLL: Duration = Duration::from_secs(1);

/// How long a reopened device must play before it no longer reads failed.
pub const MONITOR_RETRY_PROBATION: Duration = Duration::from_secs(1);

/// Whether an output device is present now: `Some(false)` when it is
/// known to be unplugged, `None` when that cannot be told.
pub type MonitorPresence = Box<dyn FnMut(&str) -> Option<bool> + Send>;

/// The presence probe of this platform: ALSA's procfs on Linux, and none
/// elsewhere (every retry then tries the device). The procfs read runs on
/// a poller thread ([`polled_presence`]), never on the render loop.
pub fn system_presence() -> MonitorPresence {
    #[cfg(target_os = "linux")]
    {
        let lister = fm_io_linux::audio_output::AudioOutputLister::default();
        polled_presence(
            move |device: &str| lister.present(device),
            MONITOR_PRESENCE_POLL / 4,
        )
    }
    #[cfg(not(target_os = "linux"))]
    Box::new(|_| None)
}

/// What the presence poller is asked and has answered.
#[derive(Default)]
struct PresencePoll {
    /// The device last asked about.
    wanted: Option<String>,
    /// The last answer, and the device it is for.
    answer: Option<(String, Option<bool>)>,
    /// The slot dropped its probe; the poller exits.
    closed: bool,
}

/// The slot's end of a presence poller: dropping it stops the poller.
struct PresenceHandle {
    shared: Arc<(Mutex<PresencePoll>, Condvar)>,
}

impl PresenceHandle {
    fn ask(&self, device: &str) -> Option<bool> {
        let (state, wake) = &*self.shared;
        // Never wait on the poller: a busy lock reads as "cannot tell".
        let mut state = state.try_lock().ok()?;
        if state.wanted.as_deref() != Some(device) {
            state.wanted = Some(device.to_owned());
            wake.notify_one();
        }
        state
            .answer
            .as_ref()
            .filter(|(answered, _)| answered == device)
            .and_then(|(_, present)| *present)
    }
}

impl Drop for PresenceHandle {
    fn drop(&mut self) {
        let (state, wake) = &*self.shared;
        state.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        wake.notify_one();
    }
}

/// A presence probe that never runs `probe` on the caller's thread: a
/// poller thread asks `probe` about the device last asked about every
/// `interval` (and at once when another device is asked about), and the
/// returned probe answers from its last result without waiting, `None`
/// until the poller has answered for that device.
pub fn polled_presence(
    mut probe: impl FnMut(&str) -> Option<bool> + Send + 'static,
    interval: Duration,
) -> MonitorPresence {
    let shared = Arc::new((Mutex::new(PresencePoll::default()), Condvar::new()));
    let poller = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("monitor-presence".into())
        .spawn(move || {
            let (state, wake) = &*poller;
            let mut polled: Option<String> = None;
            loop {
                let device = {
                    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
                    if polled.is_some() && guard.wanted == polled && !guard.closed {
                        guard = wake
                            .wait_timeout(guard, interval)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                    while guard.wanted.is_none() && !guard.closed {
                        guard = wake.wait(guard).unwrap_or_else(PoisonError::into_inner);
                    }
                    if guard.closed {
                        return;
                    }
                    guard.wanted.clone()
                };
                let Some(device) = device else { continue };
                let present = probe(&device);
                state.lock().unwrap_or_else(PoisonError::into_inner).answer =
                    Some((device.clone(), present));
                polled = Some(device);
            }
        });
    if spawned.is_err() {
        return Box::new(|_| None);
    }
    let handle = PresenceHandle { shared };
    Box::new(move |device| handle.ask(device))
}

/// The wait before retry `attempt` (counting from zero) of a failed
/// device: [`MONITOR_RETRY_FIRST`] doubled per attempt, at most
/// [`MONITOR_RETRY_MAX`].
#[must_use]
pub fn retry_delay(attempt: u32) -> Duration {
    MONITOR_RETRY_FIRST
        .saturating_mul(1_u32 << attempt.min(16))
        .min(MONITOR_RETRY_MAX)
}

/// The longest failure reason logged.
pub const MAX_MONITOR_FAILURE_BYTES: usize = 240;

/// The child standard error kept for a failure reason.
const STDERR_TAIL_BYTES: usize = 2_048;

/// Whether the monitor output is reaching its device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorOutputHealth {
    /// The device is open and blocks are being written.
    Playing,
    /// The device could not be opened, the child exited, a write failed, or
    /// the device stopped reading.
    Failed,
}

/// The `FFmpeg` arguments that play interleaved `f32le` PCM from standard
/// input on ALSA output `device`.
#[must_use]
pub fn ffmpeg_args(device: &str, sample_rate: u32, channels: usize) -> Vec<OsString> {
    [
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-f",
        "f32le",
        "-ar",
        &sample_rate.to_string(),
        "-ac",
        &channels.to_string(),
        "-i",
        "pipe:0",
        "-f",
        "alsa",
        device,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// The linear gain of a monitor level: silence while muted.
#[must_use]
pub fn monitor_gain(gain_millidb: i32, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        // Bounded to -96..=+24 dB by the model contract.
        #[allow(clippy::cast_possible_truncation)]
        let gain = 10_f64.powf(f64::from(gain_millidb) / 20_000.0) as f32;
        gain
    }
}

/// Interleaves the first `samples` samples of `planes` into little-endian
/// `f32` bytes, ramping the gain linearly from `from` to `to` across the
/// block and clamping every sample to full scale.
#[must_use]
pub fn interleave(planes: &[Vec<f32>], samples: usize, from: f32, to: f32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples * planes.len() * 4);
    #[allow(clippy::cast_precision_loss)] // a block is a few thousand samples
    let step = if samples == 0 {
        0.0
    } else {
        (to - from) / samples as f32
    };
    for index in 0..samples {
        #[allow(clippy::cast_precision_loss)]
        let gain = from + step * (index + 1) as f32;
        for plane in planes {
            let sample = plane.get(index).copied().unwrap_or(0.0);
            let sample = if sample.is_finite() {
                (sample * gain).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    bytes
}

/// The queue between the render loop and the writer: at most `depth`
/// blocks, each stamped with when it was queued. A block queued while the
/// queue is full replaces the oldest, so a slow device never accumulates
/// latency here.
#[derive(Debug)]
pub struct BlockQueue {
    blocks: VecDeque<(Instant, Vec<u8>)>,
    depth: usize,
}

impl BlockQueue {
    /// An empty queue of at most `depth` (at least one) blocks.
    #[must_use]
    pub fn new(depth: usize) -> Self {
        let depth = depth.max(1);
        Self {
            blocks: VecDeque::with_capacity(depth),
            depth,
        }
    }

    /// Queues `block`, dropping the oldest blocks to make room; returns how
    /// many were dropped.
    pub fn push(&mut self, queued_at: Instant, block: Vec<u8>) -> u64 {
        let mut dropped = 0;
        while self.blocks.len() >= self.depth {
            self.blocks.pop_front();
            dropped += 1;
        }
        self.blocks.push_back((queued_at, block));
        dropped
    }

    /// The oldest block and when it was queued.
    pub fn pop(&mut self) -> Option<(Instant, Vec<u8>)> {
        self.blocks.pop_front()
    }

    /// Blocks queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether no block is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// The writer's latency bound: which dequeued blocks reach the device.
#[derive(Clone, Debug)]
pub struct LatencyPolicy {
    /// The backlog a blocked write drains: the pipe capacity less the
    /// target.
    drain_bytes: usize,
    /// Bytes still to skip draining the last backlog.
    skip_bytes: usize,
}

impl LatencyPolicy {
    /// The policy for PCM of `bytes_per_second`.
    #[must_use]
    pub fn new(bytes_per_second: usize) -> Self {
        let target = bytes_per_second
            .saturating_mul(usize::try_from(MONITOR_PIPE_TARGET.as_millis()).unwrap_or(0))
            / 1_000;
        Self {
            drain_bytes: MONITOR_PIPE_BYTES.saturating_sub(target),
            skip_bytes: 0,
        }
    }

    /// Whether a dequeued block of `len` bytes queued `age` ago is written;
    /// `false` drops it as stale or as part of a backlog being drained.
    pub fn admit(&mut self, len: usize, age: Duration) -> bool {
        let draining = self.skip_bytes > 0;
        self.skip_bytes = self.skip_bytes.saturating_sub(len);
        !draining && age <= MONITOR_MAX_BLOCK_AGE
    }

    /// Records a write that took `blocked_for`: one that blocked found the
    /// pipe full, so the next blocks drain that backlog.
    pub fn wrote(&mut self, blocked_for: Duration) {
        if blocked_for >= MONITOR_BLOCKED_WRITE {
            self.skip_bytes = self.drain_bytes;
        }
    }
}

/// Why an open output failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Exited(ExitStatus),
    WriteFailed,
    Stalled,
}

struct WriterState {
    queue: BlockQueue,
    closed: bool,
    /// When the write in progress began.
    writing_since: Option<Instant>,
    /// Blocks the writer dropped as stale or draining a backlog.
    dropped: u64,
}

struct Shared {
    state: Mutex<WriterState>,
    ready: Condvar,
    failed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, WriterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One open monitor output device; see the module documentation.
pub struct MonitorOutput {
    shared: Arc<Shared>,
    child: Child,
    writer: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    gain: f32,
    channels: usize,
    dropped_blocks: u64,
    failure: Option<Failure>,
    stall_after: Duration,
}

impl MonitorOutput {
    /// Starts `program` with `args`, writing PCM blocks of `channels`
    /// interleaved channels at `sample_rate` to its standard input, at
    /// `gain` initially.
    ///
    /// # Errors
    ///
    /// Returns the spawn error, or a missing standard input pipe.
    pub fn start(
        program: &OsString,
        args: &[OsString],
        sample_rate: u32,
        channels: usize,
        gain: f32,
    ) -> io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("monitor output child has no stdin"));
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(WriterState {
                queue: BlockQueue::new(MONITOR_QUEUE_BLOCKS),
                closed: false,
                writing_since: None,
                dropped: 0,
            }),
            ready: Condvar::new(),
            failed: AtomicBool::new(false),
        });
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let bytes_per_second = usize::try_from(sample_rate)
            .unwrap_or(usize::MAX)
            .saturating_mul(channels)
            .saturating_mul(4);
        let writer_shared = Arc::clone(&shared);
        let writer = thread::Builder::new()
            .name("freemixd-monitor-output".into())
            .spawn(move || {
                write_blocks(stdin, &writer_shared, LatencyPolicy::new(bytes_per_second));
            });
        let writer = match writer {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let tail = Arc::clone(&stderr_tail);
        // Without a reader the tail is only lost; the device still plays.
        let stderr = thread::Builder::new()
            .name("freemixd-monitor-stderr".into())
            .spawn(move || read_stderr_tail(stderr, &tail))
            .ok();
        Ok(Self {
            shared,
            child,
            writer: Some(writer),
            stderr,
            stderr_tail,
            gain,
            channels,
            dropped_blocks: 0,
            failure: None,
            stall_after: MONITOR_STALL_AFTER,
        })
    }

    /// Queues the first `samples` samples of `planes` at `gain`, ramping
    /// from the previous block's gain. Never blocks: a full queue drops its
    /// oldest block. A block of another channel count is dropped too.
    pub fn push(&mut self, planes: &[Vec<f32>], samples: usize, gain: f32) {
        let from = self.gain;
        self.gain = gain;
        if planes.len() != self.channels
            || self.failure.is_some()
            || self.shared.failed.load(Ordering::Acquire)
        {
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
            return;
        }
        let block = interleave(planes, samples, from, gain);
        let dropped = {
            let mut state = self.shared.lock();
            if state.closed {
                return;
            }
            state.queue.push(Instant::now(), block)
        };
        self.shared.ready.notify_one();
        self.dropped_blocks = self.dropped_blocks.saturating_add(dropped);
    }

    /// Whether the device is still being written. A device that stopped
    /// reading is failed here and its child killed.
    pub fn health(&mut self) -> MonitorOutputHealth {
        // Always poll the child, even once a failed write already marked the
        // output failed: an exited child is reaped here rather than left a
        // zombie until the device is chosen again. `try_wait` keeps the
        // status once reaped, so polling again is harmless.
        if let Ok(Some(status)) = self.child.try_wait() {
            if !matches!(self.failure, Some(Failure::Stalled)) {
                self.failure = Some(Failure::Exited(status));
            }
        } else if self.failure.is_none() {
            if self.shared.failed.load(Ordering::Acquire) {
                self.failure = Some(Failure::WriteFailed);
            } else if self
                .shared
                .lock()
                .writing_since
                .is_some_and(|since| since.elapsed() >= self.stall_after)
            {
                self.failure = Some(Failure::Stalled);
                let _ = self.child.kill();
            }
        }
        if self.failure.is_some() {
            MonitorOutputHealth::Failed
        } else {
            MonitorOutputHealth::Playing
        }
    }

    /// Blocks dropped because the device fell behind, a backlog was
    /// drained, or the output failed.
    #[must_use]
    pub fn dropped_blocks(&self) -> u64 {
        self.dropped_blocks
            .saturating_add(self.shared.lock().dropped)
    }

    /// Closes the output and returns why it failed (or `None` while it was
    /// playing): the cause and a bounded, redacted tail of the child's
    /// standard error.
    pub fn into_failure(mut self) -> Option<String> {
        self.health();
        let failure = self.failure;
        self.shut_down();
        let cause = match failure? {
            Failure::Exited(status) => format!("the device child exited ({status})"),
            Failure::WriteFailed => "writing the device failed".to_owned(),
            Failure::Stalled => format!(
                "the device stopped reading for {:.1} s",
                self.stall_after.as_secs_f64()
            ),
        };
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tail = String::from_utf8_lossy(&tail);
        let tail = tail.trim();
        Some(if tail.is_empty() {
            scrub_failure(&cause)
        } else {
            scrub_failure(&format!("{cause}: {tail}"))
        })
    }

    fn shut_down(&mut self) {
        // Closing the queue ends an idle writer; killing the child unblocks
        // a writer stuck on a stalled device and ends the standard error.
        self.shared.lock().closed = true;
        self.shared.ready.notify_all();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

impl Drop for MonitorOutput {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// A failure reason fit for one log field: lines joined, credentials and
/// paths redacted, control characters removed, and at most
/// [`MAX_MONITOR_FAILURE_BYTES`] bytes, keeping the end (`FFmpeg` names the
/// cause last).
#[must_use]
pub fn scrub_failure(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    let redacted = fm_observability::Redactor.redact(&joined);
    let clean: String = redacted
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if clean.len() <= MAX_MONITOR_FAILURE_BYTES {
        return clean;
    }
    let mut start = clean.len() - (MAX_MONITOR_FAILURE_BYTES - 3);
    while !clean.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &clean[start..])
}

/// Keeps the last [`STDERR_TAIL_BYTES`] of `stderr` in `tail`, starting at
/// a line boundary so no line (or secret) is cut in half.
fn read_stderr_tail(mut stderr: ChildStderr, tail: &Mutex<Vec<u8>>) {
    let mut chunk = [0_u8; 1_024];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend_from_slice(&chunk[..read]);
                if tail.len() > STDERR_TAIL_BYTES {
                    let excess = tail.len() - STDERR_TAIL_BYTES;
                    let cut = tail[excess..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(tail.len(), |newline| excess + newline + 1);
                    tail.drain(..cut);
                }
            }
        }
    }
}

/// The daemon's monitor output: the chosen device and level, and the open
/// output while one is playing. The device opens lazily on the first block
/// rendered after it was chosen, so the sample rate and channel count are
/// the rendered bus's own.
pub struct MonitorOutputSlot {
    program: OsString,
    device: Option<String>,
    gain: f32,
    output: Option<MonitorOutput>,
    /// The format the open output was started with.
    format: Option<(u32, usize)>,
    /// Open (or reopen) the device on the next block.
    reopen: bool,
    /// Why the device failed; cleared when the device is chosen again or a
    /// retry has played through its probation.
    failure: Option<String>,
    /// When the open output started.
    opened_at: Option<Instant>,
    /// When a failed device is retried next.
    retry_at: Option<Instant>,
    /// Failed attempts since the device was chosen or last played long
    /// enough.
    attempts: u32,
    /// Whether the chosen device is present.
    presence: MonitorPresence,
}

impl MonitorOutputSlot {
    /// A slot playing on `device` (if any) through `program` at the given
    /// level, asking [`system_presence`] whether a failed device is back.
    #[must_use]
    pub fn new(program: OsString, device: Option<String>, gain_millidb: i32, muted: bool) -> Self {
        Self::with_presence(program, device, gain_millidb, muted, system_presence())
    }

    /// [`Self::new`] asking `presence` whether a failed device is back.
    #[must_use]
    pub fn with_presence(
        program: OsString,
        device: Option<String>,
        gain_millidb: i32,
        muted: bool,
        presence: MonitorPresence,
    ) -> Self {
        Self {
            program,
            reopen: device.is_some(),
            device,
            gain: monitor_gain(gain_millidb, muted),
            output: None,
            format: None,
            failure: None,
            opened_at: None,
            retry_at: None,
            attempts: 0,
            presence,
        }
    }

    /// Chooses the device (or none). Choosing a device, even the current
    /// one, closes the open output and reopens it on the next block, so a
    /// failed device is retried at once and its back-off starts over.
    pub fn set_device(&mut self, device: Option<String>) {
        self.output = None;
        self.format = None;
        self.failure = None;
        self.opened_at = None;
        self.retry_at = None;
        self.attempts = 0;
        self.reopen = device.is_some();
        self.device = device;
    }

    /// Moves the level; the next block ramps to it.
    pub fn set_level(&mut self, gain_millidb: i32, muted: bool) {
        self.gain = monitor_gain(gain_millidb, muted);
    }

    /// The chosen device.
    #[must_use]
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// Why the chosen device failed, if it did.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Plays one rendered block of the monitor bus: the first `samples`
    /// samples of each of `planes` at `sample_rate`. A playing device is
    /// reopened in a new format; a failed one waits for its retry.
    pub fn push(&mut self, planes: &[Vec<f32>], samples: usize, sample_rate: u32) {
        self.push_at(planes, samples, sample_rate, Instant::now());
    }

    /// [`Self::push`] at `now`.
    pub fn push_at(&mut self, planes: &[Vec<f32>], samples: usize, sample_rate: u32, now: Instant) {
        self.poll(now);
        self.due_retry(now);
        let format = (sample_rate, planes.len());
        if self.output.is_some() && self.format != Some(format) {
            self.output = None;
            self.reopen = true;
        }
        if self.reopen {
            self.reopen = false;
            if let Some(device) = &self.device {
                let args = ffmpeg_args(device, sample_rate, planes.len());
                match MonitorOutput::start(
                    &self.program,
                    &args,
                    sample_rate,
                    planes.len(),
                    self.gain,
                ) {
                    Ok(output) => {
                        self.output = Some(output);
                        self.format = Some(format);
                        self.opened_at = Some(now);
                    }
                    Err(error) => {
                        let program = self.program.to_string_lossy().into_owned();
                        self.fail(&format!("could not start {program}: {error}"), now);
                    }
                }
            }
        }
        if let Some(output) = &mut self.output {
            output.push(planes, samples, self.gain);
        }
    }

    /// Whether the monitor bus is reaching the chosen device. A device
    /// being retried reads failed until it has played through its
    /// probation.
    pub fn state(&mut self) -> fm_protocol::AudioMonitorOutputState {
        self.state_at(Instant::now())
    }

    /// [`Self::state`] at `now`.
    pub fn state_at(&mut self, now: Instant) -> fm_protocol::AudioMonitorOutputState {
        self.poll(now);
        if self.device.is_none() {
            fm_protocol::AudioMonitorOutputState::Off
        } else if self.failure.is_some() {
            fm_protocol::AudioMonitorOutputState::Failed
        } else if self.output.is_some() {
            fm_protocol::AudioMonitorOutputState::Playing
        } else {
            fm_protocol::AudioMonitorOutputState::Off
        }
    }

    /// When the failed device is retried next, if it is.
    #[must_use]
    pub const fn next_retry(&self) -> Option<Instant> {
        self.retry_at
    }

    /// Latches a failure of the open output: the output is closed (its
    /// child reaped), the reason logged, and a retry scheduled. A retried
    /// device that played through its probation no longer reads failed.
    fn poll(&mut self, now: Instant) {
        if self
            .output
            .as_mut()
            .is_some_and(|output| output.health() == MonitorOutputHealth::Failed)
            && let Some(output) = self.output.take()
        {
            let reason = output
                .into_failure()
                .unwrap_or_else(|| "the device failed".to_owned());
            self.fail(&reason, now);
        } else if self.output.is_some()
            && self.failure.is_some()
            && self
                .opened_at
                .is_some_and(|at| now.saturating_duration_since(at) >= MONITOR_RETRY_PROBATION)
        {
            self.failure = None;
        }
    }

    /// Reopens a failed device whose retry is due, unless it is known to
    /// be unplugged: then it is only looked for again shortly.
    fn due_retry(&mut self, now: Instant) {
        let (Some(at), Some(device)) = (self.retry_at, self.device.as_deref()) else {
            return;
        };
        if now < at || self.output.is_some() {
            return;
        }
        if (self.presence)(device) == Some(false) {
            self.retry_at = Some(now + MONITOR_PRESENCE_POLL);
            return;
        }
        self.retry_at = None;
        self.reopen = true;
    }

    fn fail(&mut self, reason: &str, now: Instant) {
        // A device that played long enough starts its back-off over.
        if self
            .opened_at
            .is_some_and(|at| now.saturating_duration_since(at) >= MONITOR_RETRY_RESET_AFTER)
        {
            self.attempts = 0;
        }
        if self.device.is_some() {
            self.retry_at = Some(now + retry_delay(self.attempts));
            self.attempts = self.attempts.saturating_add(1);
        }
        self.opened_at = None;
        let reason = scrub_failure(reason);
        eprintln!(
            "FREEMIXD_MONITOR_OUTPUT_FAILURE\tv=1\tdevice={}\tfailure={reason}",
            scrub_failure(self.device.as_deref().unwrap_or("")),
        );
        self.output = None;
        self.format = None;
        self.failure = Some(reason);
    }
}

fn write_blocks(mut stdin: ChildStdin, shared: &Shared, mut policy: LatencyPolicy) {
    loop {
        let (queued_at, block) = {
            let mut state = shared.lock();
            loop {
                if state.closed {
                    return;
                }
                if let Some(block) = state.queue.pop() {
                    break block;
                }
                state = shared
                    .ready
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let started = Instant::now();
        if !policy.admit(block.len(), started.saturating_duration_since(queued_at)) {
            shared.lock().dropped += 1;
            continue;
        }
        shared.lock().writing_since = Some(started);
        let written = stdin.write_all(&block);
        shared.lock().writing_since = None;
        if written.is_err() {
            shared.failed.store(true, Ordering::Release);
            return;
        }
        policy.wrote(started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_plays_f32le_stdin_on_the_alsa_device() {
        let args = ffmpeg_args("hw:CARD=Headset,DEV=0", 48_000, 2);
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "error",
                "-f",
                "f32le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-f",
                "alsa",
                "hw:CARD=Headset,DEV=0"
            ]
        );
    }

    /// The queue keeps only the newest blocks: a device that fell behind
    /// never holds more than the target depth.
    #[test]
    fn a_full_queue_replaces_its_oldest_block() {
        let now = Instant::now();
        let mut queue = BlockQueue::new(MONITOR_QUEUE_BLOCKS);
        assert!(queue.is_empty());
        for block in 0..MONITOR_QUEUE_BLOCKS {
            assert_eq!(queue.push(now, vec![u8::try_from(block).unwrap()]), 0);
        }
        assert_eq!(queue.push(now, vec![10]), 1);
        assert_eq!(queue.push(now, vec![11]), 1);
        assert_eq!(queue.len(), MONITOR_QUEUE_BLOCKS);
        let kept: Vec<u8> = std::iter::from_fn(|| queue.pop())
            .map(|(_, b)| b[0])
            .collect();
        assert_eq!(kept, [10, 11]);
        assert_eq!(BlockQueue::new(0).push(now, Vec::new()), 0);
    }

    /// Stale blocks are dropped, and a blocked write (a full pipe, as after
    /// `FFmpeg` started) drains the backlog down to the target before
    /// writing resumes.
    #[test]
    fn the_writer_drops_stale_blocks_and_drains_a_full_pipe() {
        // 48 kHz stereo f32: 384 000 B/s, a 40 ms target of 15 360 B.
        let mut policy = LatencyPolicy::new(48_000 * 2 * 4);
        let block = 12_800; // 1 600 samples, one 30 fps block
        let fresh = Duration::from_millis(5);
        assert!(policy.admit(block, fresh));
        assert!(!policy.admit(block, MONITOR_MAX_BLOCK_AGE + fresh), "stale");
        policy.wrote(Duration::from_millis(1));
        assert!(policy.admit(block, fresh), "a quick write keeps playing");

        policy.wrote(MONITOR_BLOCKED_WRITE);
        let backlog = MONITOR_PIPE_BYTES - 15_360;
        let skipped = std::iter::repeat_with(|| policy.admit(block, fresh))
            .take_while(|admitted| !admitted)
            .count();
        assert_eq!(skipped, backlog.div_ceil(block), "the backlog is drained");
        assert!(policy.admit(block, fresh), "then blocks play again");
    }

    #[test]
    fn a_failure_reason_is_one_bounded_redacted_field() {
        let reason = scrub_failure(
            "the device child exited (exit status: 1): \n[alsa @ 0x1]\tcannot open \
             password=hunter2\n\nfinal line\r",
        );
        assert!(!reason.contains("hunter2"), "{reason}");
        assert!(!reason.contains(['\n', '\t', '\r']), "{reason}");
        assert!(reason.ends_with("final line"), "{reason}");
        let long = scrub_failure(&format!("{}é tail", "x".repeat(1_000)));
        assert!(long.len() <= MAX_MONITOR_FAILURE_BYTES);
        assert!(long.starts_with("...") && long.ends_with("é tail"));
    }

    /// The child's process state letter from `/proc/<pid>/stat` (`Z` for a
    /// zombie), or `None` once it is gone.
    #[cfg(target_os = "linux")]
    fn process_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(") ")?.1.chars().next()
    }

    /// A device child that exits is reaped by the health poll even when a
    /// failed write noticed the exit first, so no zombie outlives it.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_exited_device_child_is_reaped_after_a_failed_write() {
        let mut output =
            MonitorOutput::start(&OsString::from("true"), &[], 48_000, 2, 1.0).unwrap();
        let pid = output.child.id();
        let planes = vec![vec![0.25; 4_096]; 2];
        let started = Instant::now();
        // Only the writer observes the exit: the health poll is not called
        // until a write has failed.
        while !output.shared.failed.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no write failed"
            );
            output.push(&planes, 4_096, 1.0);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(output.health(), MonitorOutputHealth::Failed);
        assert_ne!(
            process_state(pid),
            Some('Z'),
            "the exited device child was left a zombie"
        );
        assert_eq!(output.health(), MonitorOutputHealth::Failed);
    }

    /// A device whose child is alive but never reads is failed once a write
    /// has blocked for the stall period, instead of reporting playing while
    /// every block is dropped; the stalled child is killed.
    #[cfg(unix)]
    #[test]
    fn a_device_that_stops_reading_is_failed() {
        let mut output = MonitorOutput::start(
            &OsString::from("sleep"),
            &[OsString::from("30")],
            48_000,
            2,
            1.0,
        )
        .unwrap();
        output.stall_after = Duration::from_millis(300);
        let planes = vec![vec![0.25; 4_096]; 2];
        let started = Instant::now();
        while output.health() == MonitorOutputHealth::Playing {
            assert!(started.elapsed() < Duration::from_secs(10), "never stalled");
            output.push(&planes, 4_096, 1.0);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(
            output.dropped_blocks() > 0,
            "the stalled device dropped blocks"
        );
        let reason = output.into_failure().unwrap();
        assert_eq!(reason, "the device stopped reading for 0.3 s");
    }

    /// Writes an executable `script` and runs it once, so a later spawn
    /// never races the write (`ETXTBSY`).
    #[cfg(unix)]
    fn script(directory: &std::path::Path, body: &str) -> OsString {
        use std::os::unix::fs::PermissionsExt as _;
        let path = directory.join("device.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        while Command::new(&path)
            .env("WARM_UP", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "script never ran"
            );
            thread::sleep(Duration::from_millis(10));
        }
        path.into_os_string()
    }

    #[cfg(unix)]
    fn fail(slot: &mut MonitorOutputSlot, planes: &[Vec<f32>], sample_rate: u32, at: Instant) {
        let started = Instant::now();
        while slot.state_at(at) != fm_protocol::AudioMonitorOutputState::Failed {
            assert!(started.elapsed() < Duration::from_secs(10), "never failed");
            slot.push_at(planes, 1_024, sample_rate, at);
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Pushes a block at `at`, which retries the failed device, and waits
    /// for that retry to fail.
    #[cfg(unix)]
    fn retry_fails(slot: &mut MonitorOutputSlot, planes: &[Vec<f32>], at: Instant) {
        slot.push_at(planes, 1_024, 48_000, at);
        assert_eq!(slot.next_retry(), None, "the retry was not due");
        let started = Instant::now();
        while slot.next_retry().is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "never failed");
            thread::sleep(Duration::from_millis(5));
            slot.state_at(at);
        }
        assert_eq!(
            slot.state_at(at),
            fm_protocol::AudioMonitorOutputState::Failed
        );
    }

    /// The presence probe never runs on the render loop: asking answers at
    /// once from the poller's last result, even while the probe blocks.
    #[test]
    fn presence_is_probed_off_the_asking_thread() {
        let asker = thread::current().id();
        let (probed_on, probed) = std::sync::mpsc::channel();
        let mut presence = polled_presence(
            move |device: &str| {
                let _ = probed_on.send(thread::current().id());
                thread::sleep(Duration::from_millis(200));
                Some(device == "phones")
            },
            Duration::from_millis(20),
        );
        let started = Instant::now();
        assert_eq!(presence("phones"), None, "no answer before the first poll");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "asking waited on the probe"
        );
        let prober = probed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(prober, asker, "the probe ran on the asking thread");
        let started = Instant::now();
        while presence("phones") != Some(true) {
            assert!(started.elapsed() < Duration::from_secs(5), "never answered");
            thread::sleep(Duration::from_millis(5));
        }
        // Another device is not answered from the old device's result.
        assert_eq!(presence("speakers"), None);
        let started = Instant::now();
        while presence("speakers") != Some(false) {
            assert!(started.elapsed() < Duration::from_secs(5), "never answered");
            thread::sleep(Duration::from_millis(5));
        }
        drop(presence);
    }

    #[test]
    fn retries_back_off_from_one_second_to_thirty() {
        let delays: Vec<u64> = (0..8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(u32::MAX), MONITOR_RETRY_MAX);
    }

    /// A failed device is retried on its own with a growing back-off; an
    /// unplugged device is only looked for until it is back; a format
    /// change never reopens it early; choosing it again retries it at once;
    /// and a reopened device reads failed until it has played through its
    /// probation. The failure carries the child's redacted standard error.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_failed_device_is_retried_with_back_off_and_reopened_when_back() {
        use std::sync::atomic::{AtomicU8, AtomicUsize};

        let directory = tempfile::tempdir().unwrap();
        let spawns = directory.path().join("spawns");
        let plays = directory.path().join("plays");
        let program = script(
            directory.path(),
            &format!(
                "[ -n \"$WARM_UP\" ] && exit 0\n\
                 echo spawn >> '{}'\n\
                 [ -e '{}' ] && exec cat > /dev/null\n\
                 echo '[alsa @ 0x1] cannot open audio device token=hunter2' >&2\n\
                 exit 1",
                spawns.display(),
                plays.display()
            ),
        );
        let spawn_count =
            || std::fs::read_to_string(&spawns).map_or(0, |spawns| spawns.lines().count());
        // 0: cannot tell, 1: unplugged, 2: present.
        let presence = Arc::new(AtomicU8::new(0));
        let probes = Arc::new(AtomicUsize::new(0));
        let probe = {
            let presence = Arc::clone(&presence);
            let probes = Arc::clone(&probes);
            Box::new(move |device: &str| {
                assert_eq!(device, "phones");
                probes.fetch_add(1, Ordering::SeqCst);
                match presence.load(Ordering::SeqCst) {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                }
            })
        };
        let stereo = vec![vec![0.25; 1_024]; 2];
        let t0 = Instant::now();
        let at = |millis: u64| t0 + Duration::from_millis(millis);
        let mut slot =
            MonitorOutputSlot::with_presence(program, Some("phones".into()), 0, false, probe);
        fail(&mut slot, &stereo, 48_000, at(0));
        let reason = slot.failure().unwrap().to_owned();
        assert!(
            reason.starts_with("the device child exited")
                && reason.contains("cannot open audio device"),
            "{reason}"
        );
        assert!(!reason.contains("hunter2"), "{reason}");
        assert_eq!(spawn_count(), 1);
        assert_eq!(slot.next_retry(), Some(at(1_000)));

        // Before the retry is due, neither a block nor another sample rate
        // or channel count reopens it.
        slot.push_at(&stereo, 1_024, 44_100, at(500));
        slot.push_at(&[vec![0.25; 1_024]], 1_024, 44_100, at(999));
        assert_eq!(
            slot.state_at(at(999)),
            fm_protocol::AudioMonitorOutputState::Failed
        );
        assert_eq!(spawn_count(), 1, "a format change reopened a failed device");
        assert_eq!(probes.load(Ordering::SeqCst), 0);

        // The retry is due: the device is tried and fails again, so the
        // next waits twice as long.
        retry_fails(&mut slot, &stereo, at(1_000));
        assert_eq!(spawn_count(), 2);
        assert_eq!(slot.next_retry(), Some(at(3_000)));

        // Unplugged: looked for every second, never opened.
        presence.store(1, Ordering::SeqCst);
        slot.push_at(&stereo, 1_024, 48_000, at(3_000));
        assert_eq!(slot.next_retry(), Some(at(4_000)));
        slot.push_at(&stereo, 1_024, 48_000, at(4_000));
        assert_eq!(slot.next_retry(), Some(at(5_000)));
        assert_eq!(spawn_count(), 2);
        assert_eq!(probes.load(Ordering::SeqCst), 3, "one per retry or look");
        assert_eq!(
            slot.state_at(at(4_500)),
            fm_protocol::AudioMonitorOutputState::Failed
        );

        // Back: opened on the next look, and its failure grows the back-off.
        presence.store(2, Ordering::SeqCst);
        retry_fails(&mut slot, &stereo, at(5_000));
        assert_eq!(spawn_count(), 3);
        assert_eq!(slot.next_retry(), Some(at(9_000)));

        // Choosing it again retries it at once and starts the back-off over.
        slot.set_device(Some("phones".into()));
        assert_eq!(slot.failure(), None);
        assert_eq!(slot.next_retry(), None);
        fail(&mut slot, &stereo, 48_000, at(6_000));
        assert_eq!(spawn_count(), 4);
        assert_eq!(slot.next_retry(), Some(at(7_000)));

        // The device works again: the retry plays, but reads failed until
        // its probation is over.
        std::fs::write(&plays, "").unwrap();
        slot.push_at(&stereo, 1_024, 48_000, at(7_000));
        let started = Instant::now();
        while spawn_count() < 5 {
            assert!(started.elapsed() < Duration::from_secs(10), "never spawned");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(slot.next_retry(), None);
        assert_eq!(
            slot.state_at(at(7_500)),
            fm_protocol::AudioMonitorOutputState::Failed
        );
        slot.push_at(&stereo, 1_024, 48_000, at(8_000));
        assert_eq!(
            slot.state_at(at(8_000)),
            fm_protocol::AudioMonitorOutputState::Playing
        );
        assert_eq!(slot.failure(), None);
        assert_eq!(spawn_count(), 5);

        // Choosing no device stops it and schedules nothing.
        slot.set_device(None);
        assert_eq!(
            slot.state_at(at(9_000)),
            fm_protocol::AudioMonitorOutputState::Off
        );
        assert_eq!(slot.next_retry(), None);
    }

    #[test]
    fn the_level_ramps_across_the_block_and_clamps() {
        assert!((monitor_gain(0, false) - 1.0).abs() < 1e-6);
        assert!((monitor_gain(-6_000, false) - 0.501_187).abs() < 1e-5);
        assert!(monitor_gain(0, true).abs() < f32::EPSILON);
        let planes = vec![vec![0.5; 4], vec![2.0, f32::NAN, -2.0, 0.25]];
        let bytes = interleave(&planes, 4, 0.0, 1.0);
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        assert_eq!(
            samples,
            [0.125, 0.5, 0.25, 0.0, 0.375, -1.0, 0.5, 0.25],
            "left, right per sample; gain 0.25, 0.5, 0.75, 1.0"
        );
    }
}
