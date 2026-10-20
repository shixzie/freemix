//! AU-004 acceptance: drift correction of a live source with its own clock.
//!
//! A simulated source runs at a skewed clock and delivers its audio in
//! bursts and its pictures one at a time; the engine pulls one frame of
//! audio per frame and pops pictures as the [`LiveSourceClock`] says. The
//! tests check, in the sample domain, that the buffer stays bounded with no
//! hard resync, that the correction converges on the true skew, that the
//! shown picture stays within a frame of the audio being played, and that
//! the whole run is deterministic.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::VecDeque;

use fm_audio::{
    LiveSourceClock, LiveSourceConfig, LiveSourceTelemetry, PictureAction, SourceSyncState,
};
use fm_types::{FrameRate, SampleRate};

/// Bursts the source delivers its audio in (as an `ffmpeg` child does).
const BURST: usize = 256;

fn config(hz: u32, fps: u32, channels: usize, latency_frames: usize) -> LiveSourceConfig {
    LiveSourceConfig::new(
        SampleRate::new(hz).unwrap(),
        channels,
        FrameRate::new(fps, 1).unwrap(),
        latency_frames,
    )
}

/// A source on its own clock and the engine consuming it, both stepped one
/// engine frame at a time.
struct Show {
    clock: LiveSourceClock,
    channels: usize,
    /// Input (and output) samples per frame.
    frame: usize,
    hz: u32,
    /// Source skew against the engine, in ppm.
    ppm: f64,
    paused: bool,
    /// Source time elapsed, in source samples.
    source_time: f64,
    produced: u64,
    pictures_produced: u64,
    queue: VecDeque<u64>,
    /// The picture shown, by source frame index.
    shown: Option<u64>,
    pictures_live: bool,
    late_run: usize,
    frames: u64,
    output: Vec<Vec<f32>>,
    scratch: Vec<Vec<f32>>,
    /// Running hash of every output sample, for determinism.
    hash: u64,
    /// Largest |picture − audio| offset, in frames, while both are live.
    max_av_error: f64,
    peak_queue: usize,
}

impl Show {
    fn new(config: LiveSourceConfig) -> Self {
        let frame = config.frame_samples();
        let channels = config.channels();
        Self {
            clock: LiveSourceClock::new(config).unwrap(),
            channels,
            frame,
            hz: config.sample_rate().hertz(),
            ppm: 0.0,
            paused: false,
            source_time: 0.0,
            produced: 0,
            pictures_produced: 0,
            queue: VecDeque::new(),
            shown: None,
            pictures_live: false,
            late_run: 0,
            frames: 0,
            output: vec![vec![0.0; frame]; channels],
            scratch: vec![vec![0.0; BURST]; channels],
            hash: 0xcbf2_9ce4_8422_2325,
            max_av_error: 0.0,
            peak_queue: 0,
        }
    }

    /// The source's audio: a 440 Hz tone per channel (a different phase
    /// each), indexed by source sample.
    fn sample(&self, index: u64, channel: usize) -> f32 {
        let phase = (index % u64::from(self.hz)) as f64 / f64::from(self.hz);
        (0.5 * (core::f64::consts::TAU * 440.0 * phase + channel as f64).sin()) as f32
    }

    /// One engine frame: the source runs for a frame of its own clock, then
    /// the engine pulls a frame of audio and takes its picture.
    fn step(&mut self) {
        let fps = f64::from(self.hz) / self.frame as f64;
        let start_time = self.source_time;
        if !self.paused {
            self.source_time += self.frame as f64 * (1.0 + self.ppm * 1e-6);
        }
        let frame_start = self.frames as f64 / fps;
        // Engine time at which the source reached source sample `index`.
        let arrival = |index: u64| {
            let span = self.source_time - start_time;
            let into = if span > 0.0 {
                (index as f64 - start_time) / span
            } else {
                0.0
            };
            frame_start + into.clamp(0.0, 1.0) / fps
        };
        let available = self.source_time as u64;
        while self.produced + BURST as u64 <= available {
            let at = arrival(self.produced + BURST as u64);
            for channel in 0..self.channels {
                for offset in 0..BURST {
                    self.scratch[channel][offset] =
                        self.sample(self.produced + offset as u64, channel);
                }
            }
            let planes: Vec<&[f32]> = self.scratch.iter().map(Vec::as_slice).collect();
            self.clock.push(&planes, at).unwrap();
            self.produced += BURST as u64;
        }
        while (self.pictures_produced + 1) * self.frame as u64 <= available {
            self.queue.push_back(self.pictures_produced);
            self.pictures_produced += 1;
        }
        self.peak_queue = self.peak_queue.max(self.queue.len());
        // The receiver's picture liveness: live once the queue holds the
        // latency, stalled after two late frames in a row.
        if !self.pictures_live && self.queue.len() >= 2 {
            self.pictures_live = true;
        }
        let queued = self.pictures_live.then_some(self.queue.len());
        let mut planes: Vec<&mut [f32]> = self.output.iter_mut().map(Vec::as_mut_slice).collect();
        let pull = self
            .clock
            .pull(&mut planes, queued, (self.frames + 1) as f64 / fps)
            .unwrap();
        for plane in &self.output {
            for sample in plane {
                self.hash = (self.hash ^ u64::from(sample.to_bits())).wrapping_mul(0x100_0000_01b3);
            }
        }
        if self.pictures_live {
            match pull.picture {
                PictureAction::Hold => {}
                PictureAction::Advance { skip } => {
                    for _ in 0..skip {
                        if self.queue.len() > 1 {
                            self.queue.pop_front();
                        }
                    }
                    if let Some(picture) = self.queue.pop_front() {
                        self.shown = Some(picture);
                        self.late_run = 0;
                    } else {
                        self.late_run += 1;
                        if self.late_run >= 2 {
                            self.pictures_live = false;
                        }
                    }
                }
            }
        }
        self.frames += 1;
        self.check_alignment();
    }

    /// The source position of the audio just played, in source samples.
    fn audio_position(&self) -> f64 {
        let sync = self.clock.telemetry().sync;
        (sync.consumed_input_samples + sync.priming_trimmed_samples + sync.resync_dropped_samples)
            as f64
    }

    fn check_alignment(&mut self) {
        let telemetry = self.clock.telemetry();
        // Judge alignment once the latency smoothing has settled.
        if telemetry.sync.state != SourceSyncState::Running
            || !self.pictures_live
            || telemetry.sync.rendered_output_samples < 10 * u64::from(self.hz)
        {
            return;
        }
        if let Some(shown) = self.shown {
            // The audio played this frame started a frame ago.
            let audio = self.audio_position() / self.frame as f64 - 1.0;
            let error = (shown as f64 - audio).abs();
            self.max_av_error = self.max_av_error.max(error);
        }
    }

    fn run(&mut self, seconds: f64, fps: u32) {
        let frames = (seconds * f64::from(fps)) as u64;
        for _ in 0..frames {
            self.step();
        }
    }

    fn telemetry(&self) -> LiveSourceTelemetry {
        self.clock.telemetry()
    }
}

/// Seconds until the applied correction stays within `tolerance` ppm of
/// `ppm` for the rest of the run, sampled once a second.
fn settle(show: &mut Show, ppm: f64, seconds: u64, fps: u32, tolerance: f64) -> Option<u64> {
    let mut settled = None;
    for second in 0..seconds {
        show.run(1.0, fps);
        let error = (show.telemetry().sync.applied_correction_ppm - ppm).abs();
        match (error <= tolerance, settled) {
            (true, None) => settled = Some(second + 1),
            (false, _) => settled = None,
            (true, Some(_)) => {}
        }
    }
    settled
}

fn skewed_hour(ppm: f64) {
    let (hz, fps) = (8_000, 25);
    let mut show = Show::new(config(hz, fps, 1, 2));
    show.ppm = ppm;
    let target = show.telemetry().sync.target_fill_samples as f64;
    let settled = settle(&mut show, ppm, 3_600, fps, 5.0);
    let telemetry = show.telemetry();
    let sync = telemetry.sync;
    println!(
        "{ppm:+} ppm over 1 h: estimate {:+.3} ppm, applied {:+.3} ppm, within 5 ppm after {settled:?} s, \
         fill {:.1} (target {target}), peak fill {}, picture drops {} repeats {}, max A/V error {:.2} frames, \
         peak queue {}",
        sync.estimated_drift_ppm,
        sync.applied_correction_ppm,
        sync.buffer_fill_samples,
        sync.peak_fill_samples,
        telemetry.picture_drops,
        telemetry.picture_repeats,
        show.max_av_error,
        show.peak_queue,
    );
    assert_eq!(telemetry.hard_resyncs(), 0);
    assert_eq!(sync.underruns, 0);
    assert_eq!(sync.overruns, 0);
    assert_eq!(sync.resyncs, 0);
    assert!(settled.is_some_and(|seconds| seconds <= 300), "{settled:?}");
    assert!((sync.estimated_drift_ppm - ppm).abs() < 1.0);
    assert!((sync.smoothed_fill_samples.unwrap() - target).abs() < 0.05 * target);
    assert!((sync.peak_fill_samples as f64) < 1.5 * target + 256.0);
    // The pictures follow the audio: one extra drop (fast source) or repeat
    // (slow source) per skewed frame, the other kind never.
    let skewed_frames = 3_600.0 * f64::from(fps) * ppm.abs() * 1e-6;
    let (corrections, opposite) = if ppm > 0.0 {
        (telemetry.picture_drops, telemetry.picture_repeats)
    } else {
        (telemetry.picture_repeats, telemetry.picture_drops)
    };
    assert!(
        (corrections as f64 - skewed_frames).abs() <= 3.0,
        "{corrections} corrections for {skewed_frames} skewed frames"
    );
    assert!(opposite <= 1, "{opposite}");
    assert!(show.max_av_error <= 1.5, "{}", show.max_av_error);
    assert!(show.peak_queue <= 5, "{}", show.peak_queue);
}

#[test]
fn a_source_500_ppm_fast_stays_bounded_for_an_hour_without_resyncs() {
    skewed_hour(500.0);
}

#[test]
fn a_source_500_ppm_slow_stays_bounded_for_an_hour_without_resyncs() {
    skewed_hour(-500.0);
}

#[test]
fn a_step_change_in_rate_converges_without_a_resync() {
    let (hz, fps) = (48_000, 30);
    let mut show = Show::new(config(hz, fps, 2, 2));
    show.ppm = -300.0;
    let first = settle(&mut show, -300.0, 300, fps, 5.0);
    show.ppm = 700.0;
    let second = settle(&mut show, 700.0, 300, fps, 5.0);
    let telemetry = show.telemetry();
    println!(
        "step -300 -> +700 ppm: settled after {first:?} s then {second:?} s, estimate {:+.3} ppm, \
         peak fill {} (target {}), max A/V error {:.2} frames",
        telemetry.sync.estimated_drift_ppm,
        telemetry.sync.peak_fill_samples,
        telemetry.sync.target_fill_samples,
        show.max_av_error,
    );
    assert!(first.is_some_and(|seconds| seconds <= 180), "{first:?}");
    assert!(second.is_some_and(|seconds| seconds <= 180), "{second:?}");
    assert_eq!(telemetry.hard_resyncs(), 0);
    assert_eq!(telemetry.sync.underruns, 0);
    assert!((telemetry.sync.estimated_drift_ppm - 700.0).abs() < 1.0);
    assert!(show.max_av_error <= 1.5, "{}", show.max_av_error);
}

#[test]
fn a_paused_source_resumes_at_the_target_and_keeps_its_drift_estimate() {
    let (hz, fps) = (8_000, 25);
    let mut show = Show::new(config(hz, fps, 1, 2));
    show.ppm = 400.0;
    show.run(240.0, fps);
    let before = show.telemetry();
    assert!((before.sync.estimated_drift_ppm - 400.0).abs() < 5.0);
    show.paused = true;
    show.run(5.0, fps);
    let paused = show.telemetry();
    assert_eq!(paused.sync.state, SourceSyncState::Recovering);
    assert_eq!(paused.sync.underruns, 1);
    assert!(!show.pictures_live);
    show.paused = false;
    show.run(60.0, fps);
    let after = show.telemetry();
    println!(
        "pause/resume: estimate {:+.3} -> {:+.3} ppm, fill {:.1} (target {}), underruns {}, resyncs {}",
        before.sync.estimated_drift_ppm,
        after.sync.estimated_drift_ppm,
        after.sync.buffer_fill_samples,
        after.sync.target_fill_samples,
        after.sync.underruns,
        after.hard_resyncs(),
    );
    assert_eq!(after.sync.state, SourceSyncState::Running);
    assert_eq!(after.sync.underruns, 1);
    assert_eq!(after.hard_resyncs(), 0);
    assert!((after.sync.estimated_drift_ppm - 400.0).abs() < 5.0);
    let target = after.sync.target_fill_samples as f64;
    assert!((after.sync.smoothed_fill_samples.unwrap() - target).abs() < 0.1 * target);
    assert!(show.pictures_live);
    assert!(show.max_av_error <= 1.5, "{}", show.max_av_error);
}

#[test]
fn a_reconnect_restarts_the_buffer_and_keeps_the_drift_estimate() {
    let (hz, fps) = (8_000, 25);
    let mut show = Show::new(config(hz, fps, 1, 3));
    show.ppm = -250.0;
    show.run(300.0, fps);
    show.clock.restart();
    show.queue.clear();
    show.pictures_live = false;
    show.run(30.0, fps);
    let telemetry = show.telemetry();
    println!(
        "reconnect: estimate {:+.3} ppm 30 s after, fill {:.1} (target {})",
        telemetry.sync.estimated_drift_ppm,
        telemetry.sync.buffer_fill_samples,
        telemetry.sync.target_fill_samples,
    );
    assert_eq!(telemetry.restarts, 1);
    assert_eq!(telemetry.sync.resets, 1);
    assert_eq!(telemetry.hard_resyncs(), 0);
    assert_eq!(telemetry.sync.underruns, 0);
    assert!((telemetry.sync.estimated_drift_ppm + 250.0).abs() < 5.0);
}

#[test]
fn drift_beyond_the_correction_range_hard_resyncs_declicked_and_counted() {
    let (hz, fps) = (8_000, 25);
    let mut show = Show::new(config(hz, fps, 1, 2));
    show.ppm = 5_000.0;
    let mut largest_step = 0.0_f32;
    let mut previous = 0.0_f32;
    for _ in 0..(600 * fps) {
        show.step();
        if show.clock.state() == SourceSyncState::Running {
            for &sample in &show.output[0] {
                largest_step = largest_step.max((sample - previous).abs());
                previous = sample;
            }
        }
    }
    let telemetry = show.telemetry();
    let target = telemetry.sync.target_fill_samples;
    println!(
        "+5000 ppm: estimate {:+.1} ppm, applied {:+.1} ppm, resyncs {} ({} samples), overruns {}, peak fill {} (target {target}), \
         largest sample step {largest_step:.4}",
        telemetry.sync.estimated_drift_ppm,
        telemetry.sync.applied_correction_ppm,
        telemetry.sync.resyncs,
        telemetry.sync.resync_dropped_samples,
        telemetry.sync.overruns,
        telemetry.sync.peak_fill_samples,
    );
    assert!(telemetry.sync.estimated_drift_ppm > 990.0);
    assert!(telemetry.sync.resyncs > 0);
    assert_eq!(telemetry.sync.overruns, 0);
    assert_eq!(telemetry.sync.underruns, 0);
    assert!(telemetry.sync.peak_fill_samples < 2 * target + 2 * BURST + show.frame);
    // A 440 Hz tone at 0.5 moves at most ~0.17 per 8 kHz sample; the cuts
    // fade out and in instead of stepping.
    assert!(largest_step < 0.2, "{largest_step}");
    assert!(show.max_av_error <= 1.5, "{}", show.max_av_error);
}

#[test]
fn identical_deliveries_render_identical_audio_and_pictures() {
    let run = || {
        let mut show = Show::new(config(48_000, 30, 2, 2));
        show.ppm = 137.0;
        show.run(60.0, 30);
        show.paused = true;
        show.run(1.0, 30);
        show.paused = false;
        show.ppm = -91.0;
        show.run(60.0, 30);
        (show.hash, show.shown, show.telemetry())
    };
    let first = run();
    let second = run();
    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
    assert_eq!(first.2, second.2);
}

#[test]
fn a_matched_clock_settles_at_no_correction() {
    let (hz, fps) = (48_000, 25);
    let mut show = Show::new(config(hz, fps, 1, 2));
    show.run(600.0, fps);
    let telemetry = show.telemetry();
    assert!(telemetry.sync.applied_correction_ppm.abs() < 1.0);
    assert!(telemetry.sync.estimated_drift_ppm.abs() < 1.0);
    assert_eq!(telemetry.picture_drops + telemetry.picture_repeats, 0);
    assert_eq!(telemetry.hard_resyncs() + telemetry.sync.underruns, 0);
}
