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
//! to full scale, and handed to a dedicated writer thread through a bounded
//! queue. A full queue drops the block (counted) instead of stalling the
//! loop; a write error or an exited child marks the output failed, and only
//! choosing the device again reopens it.

use std::ffi::OsString;
use std::io::{self, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

/// Blocks the writer queue holds before the render loop drops a block: at
/// 30 frames per second about half a second of audio.
pub const MONITOR_QUEUE_BLOCKS: usize = 16;

/// Whether the monitor output is reaching its device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorOutputHealth {
    /// The device is open and blocks are being written.
    Playing,
    /// The device could not be opened, or the child or a write failed.
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

/// One open monitor output device; see the module documentation.
pub struct MonitorOutput {
    queue: Option<SyncSender<Vec<u8>>>,
    failed: Arc<AtomicBool>,
    child: Child,
    writer: Option<JoinHandle<()>>,
    gain: f32,
    channels: usize,
    dropped_blocks: u64,
}

impl MonitorOutput {
    /// Starts `program` with `args`, writing PCM blocks of `channels`
    /// interleaved channels to its standard input, at `gain` initially.
    ///
    /// # Errors
    ///
    /// Returns the spawn error, or a missing standard input pipe.
    pub fn start(
        program: &OsString,
        args: &[OsString],
        channels: usize,
        gain: f32,
    ) -> io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("monitor output child has no stdin"));
        };
        let (queue, blocks) = mpsc::sync_channel::<Vec<u8>>(MONITOR_QUEUE_BLOCKS);
        let failed = Arc::new(AtomicBool::new(false));
        let writer_failed = Arc::clone(&failed);
        let writer = thread::Builder::new()
            .name("freemixd-monitor-output".into())
            .spawn(move || write_blocks(stdin, &blocks, &writer_failed));
        let writer = match writer {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            queue: Some(queue),
            failed,
            child,
            writer: Some(writer),
            gain,
            channels,
            dropped_blocks: 0,
        })
    }

    /// Queues the first `samples` samples of `planes` at `gain`, ramping
    /// from the previous block's gain. Never blocks: a full queue drops the
    /// block. A block of another channel count is dropped too.
    pub fn push(&mut self, planes: &[Vec<f32>], samples: usize, gain: f32) {
        let from = self.gain;
        self.gain = gain;
        if planes.len() != self.channels || self.failed.load(Ordering::Acquire) {
            self.dropped_blocks = self.dropped_blocks.saturating_add(1);
            return;
        }
        let Some(queue) = &self.queue else {
            return;
        };
        match queue.try_send(interleave(planes, samples, from, gain)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped_blocks = self.dropped_blocks.saturating_add(1);
            }
            Err(TrySendError::Disconnected(_)) => self.failed.store(true, Ordering::Release),
        }
    }

    /// Whether the device is still being written.
    pub fn health(&mut self) -> MonitorOutputHealth {
        // Always poll the child, even once a failed write already marked the
        // output failed: an exited child is reaped here rather than left a
        // zombie until the device is chosen again. `try_wait` keeps the
        // status once reaped, so polling again is harmless.
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            self.failed.store(true, Ordering::Release);
        }
        if self.failed.load(Ordering::Acquire) {
            MonitorOutputHealth::Failed
        } else {
            MonitorOutputHealth::Playing
        }
    }

    /// Blocks dropped because the device fell behind or failed.
    #[must_use]
    pub const fn dropped_blocks(&self) -> u64 {
        self.dropped_blocks
    }
}

impl Drop for MonitorOutput {
    fn drop(&mut self) {
        // Closing the queue ends the writer once it drains; killing the
        // child first unblocks a writer stuck on a stalled device.
        self.queue = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
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
    /// The last open attempt failed; cleared when the device is chosen again.
    failed: bool,
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
            failed: false,
        }
    }

    /// Chooses the device (or none). Choosing a device, even the current
    /// one, closes the open output and reopens it on the next block, so a
    /// failed device is retried.
    pub fn set_device(&mut self, device: Option<String>) {
        self.output = None;
        self.format = None;
        self.failed = false;
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

    /// Plays one rendered block of the monitor bus: the first `samples`
    /// samples of each of `planes` at `sample_rate`.
    pub fn push(&mut self, planes: &[Vec<f32>], samples: usize, sample_rate: u32) {
        let format = (sample_rate, planes.len());
        if self.output.is_some() && self.format != Some(format) {
            self.output = None;
            self.reopen = true;
        }
        if self.reopen {
            self.reopen = false;
            if let Some(device) = &self.device {
                let args = ffmpeg_args(device, sample_rate, planes.len());
                match MonitorOutput::start(&self.program, &args, planes.len(), self.gain) {
                    Ok(output) => {
                        self.output = Some(output);
                        self.format = Some(format);
                    }
                    Err(_) => self.failed = true,
                }
            }
        }
        if let Some(output) = &mut self.output {
            output.push(planes, samples, self.gain);
        }
    }

    /// Whether the monitor bus is reaching the chosen device.
    pub fn state(&mut self) -> fm_protocol::AudioMonitorOutputState {
        if self.device.is_none() {
            return fm_protocol::AudioMonitorOutputState::Off;
        }
        if self.failed {
            return fm_protocol::AudioMonitorOutputState::Failed;
        }
        match self.output.as_mut().map(MonitorOutput::health) {
            Some(MonitorOutputHealth::Playing) => fm_protocol::AudioMonitorOutputState::Playing,
            Some(MonitorOutputHealth::Failed) => fm_protocol::AudioMonitorOutputState::Failed,
            None => fm_protocol::AudioMonitorOutputState::Off,
        }
    }
}

fn write_blocks(mut stdin: ChildStdin, blocks: &mpsc::Receiver<Vec<u8>>, failed: &AtomicBool) {
    for block in blocks {
        if stdin.write_all(&block).is_err() {
            failed.store(true, Ordering::Release);
            return;
        }
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
        let mut output = MonitorOutput::start(&OsString::from("true"), &[], 2, 1.0).unwrap();
        let pid = output.child.id();
        let planes = vec![vec![0.25; 4_096]; 2];
        let started = std::time::Instant::now();
        // Only the writer observes the exit: the health poll is not called
        // until a write has failed.
        while !output.failed.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "no write failed"
            );
            output.push(&planes, 4_096, 1.0);
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(output.health(), MonitorOutputHealth::Failed);
        assert_ne!(
            process_state(pid),
            Some('Z'),
            "the exited device child was left a zombie"
        );
        assert_eq!(output.health(), MonitorOutputHealth::Failed);
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
