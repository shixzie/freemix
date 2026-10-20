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
//! stalled child is killed. The failure is logged once as a
//! `FREEMIXD_MONITOR_OUTPUT_FAILURE` record carrying a bounded, redacted
//! tail of the child's standard error. Only choosing the device again
//! reopens a failed device: a later sample-rate or channel change does not.

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
    /// Why the device failed; cleared only when the device is chosen again.
    failure: Option<String>,
}

impl MonitorOutputSlot {
    /// A slot playing on `device` (if any) through `program` at the given
    /// level.
    #[must_use]
    pub fn new(program: OsString, device: Option<String>, gain_millidb: i32, muted: bool) -> Self {
        Self {
            program,
            reopen: device.is_some(),
            device,
            gain: monitor_gain(gain_millidb, muted),
            output: None,
            format: None,
            failure: None,
        }
    }

    /// Chooses the device (or none). Choosing a device, even the current
    /// one, closes the open output and reopens it on the next block, so a
    /// failed device is retried. Nothing else reopens a failed device.
    pub fn set_device(&mut self, device: Option<String>) {
        self.output = None;
        self.format = None;
        self.failure = None;
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
    /// reopened in a new format; a failed one stays failed.
    pub fn push(&mut self, planes: &[Vec<f32>], samples: usize, sample_rate: u32) {
        self.poll();
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
                    }
                    Err(error) => {
                        let program = self.program.to_string_lossy().into_owned();
                        self.fail(&format!("could not start {program}: {error}"));
                    }
                }
            }
        }
        if let Some(output) = &mut self.output {
            output.push(planes, samples, self.gain);
        }
    }

    /// Whether the monitor bus is reaching the chosen device.
    pub fn state(&mut self) -> fm_protocol::AudioMonitorOutputState {
        self.poll();
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

    /// Latches a failure of the open output: the output is closed (its
    /// child reaped) and the reason logged.
    fn poll(&mut self) {
        if self
            .output
            .as_mut()
            .is_some_and(|output| output.health() == MonitorOutputHealth::Failed)
            && let Some(output) = self.output.take()
        {
            let reason = output
                .into_failure()
                .unwrap_or_else(|| "the device failed".to_owned());
            self.fail(&reason);
        }
    }

    fn fail(&mut self, reason: &str) {
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
    fn fail(slot: &mut MonitorOutputSlot, planes: &[Vec<f32>], sample_rate: u32) {
        let started = Instant::now();
        while slot.state() != fm_protocol::AudioMonitorOutputState::Failed {
            assert!(started.elapsed() < Duration::from_secs(10), "never failed");
            slot.push(planes, 1_024, sample_rate);
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// A failed device is reopened only by choosing it again: neither a
    /// sample-rate nor a channel change retries it. The failure carries the
    /// child's redacted standard error.
    #[cfg(unix)]
    #[test]
    fn only_choosing_a_failed_device_again_reopens_it() {
        let directory = tempfile::tempdir().unwrap();
        let spawns = directory.path().join("spawns");
        let program = script(
            directory.path(),
            &format!(
                "[ -n \"$WARM_UP\" ] && exit 0\n\
                 echo spawn >> '{}'\n\
                 echo '[alsa @ 0x1] cannot open audio device token=hunter2' >&2\n\
                 exit 1",
                spawns.display()
            ),
        );
        let spawn_count =
            || std::fs::read_to_string(&spawns).map_or(0, |spawns| spawns.lines().count());
        let stereo = vec![vec![0.25; 1_024]; 2];
        let mut slot = MonitorOutputSlot::new(program, Some("phones".into()), 0, false);
        fail(&mut slot, &stereo, 48_000);
        let reason = slot.failure().unwrap().to_owned();
        assert!(
            reason.starts_with("the device child exited")
                && reason.contains("cannot open audio device"),
            "{reason}"
        );
        assert!(!reason.contains("hunter2"), "{reason}");
        assert_eq!(spawn_count(), 1);

        // Another sample rate or channel count keeps the device failed.
        slot.push(&stereo, 1_024, 44_100);
        slot.push(&[vec![0.25; 1_024]], 1_024, 44_100);
        assert_eq!(slot.state(), fm_protocol::AudioMonitorOutputState::Failed);
        assert_eq!(spawn_count(), 1, "a format change reopened a failed device");

        // Choosing it again retries it.
        slot.set_device(Some("phones".into()));
        assert_eq!(slot.failure(), None);
        fail(&mut slot, &stereo, 48_000);
        assert_eq!(spawn_count(), 2);
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
