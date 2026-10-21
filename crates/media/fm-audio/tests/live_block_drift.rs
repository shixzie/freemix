//! AU-004 acceptance: the `AudioBlock`/`MediaTiming` adapter of a live
//! source that is not an `ffmpeg` receiver.
//!
//! A simulated device runs on its own crystal (skewed by up to a few
//! hundred ppm, or wandering), stamps its blocks on the host clock with
//! jitter, delivers them late by a random amount, loses blocks (a gap),
//! stalls and then delivers its backlog at once (a burst), restarts its
//! counter, or changes clock domain. The engine pulls one frame of audio
//! per frame through a [`LiveBlockSource`] and shows the picture it says.
//! The tests check, in the sample domain, that the backlog stays bounded,
//! that the correction converges on the true skew, that the pictures stay
//! within a frame of the audio, that every output sample-to-sample step
//! stays within what a continuous tone plus the converter's declick ramps
//! can produce, and that the run is deterministic.
//!
//! `long_drift_soak` (ignored) runs the same device for hours of simulated
//! time with a wandering skew and periodic faults:
//! `FM_AU004_SOAK_HOURS=24 cargo test -p fm-audio --test live_block_drift
//! long_drift_soak -- --ignored --nocapture`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::VecDeque;
use std::num::NonZeroU128;

use fm_audio::{
    BlockPlacement, LiveBlockConfig, LiveBlockError, LiveBlockSource, LiveBlockTelemetry,
    LiveSourceConfig, PictureShown, PushContinuity, SourceSyncState,
};
use fm_frame::{
    AudioBlock, Channel, ChannelLayout, ClockDomainId, MediaFlags, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber, TimeBase,
};
use fm_types::{FrameRate, SampleRate};

/// Samples per device block.
const BLOCK: usize = 256;
/// Tone amplitude and frequency of the device's audio.
const AMPLITUDE: f64 = 0.5;
const TONE_HZ: f64 = 440.0;

fn domain(value: u128) -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::new(value).unwrap())
}

fn timing(
    pts_nanos: i64,
    duration_nanos: u64,
    domain_id: ClockDomainId,
    sequence: u64,
) -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(pts_nanos),
            TimeBase::new(1, 1_000_000_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(pts_nanos),
        NormalizedDuration::from_nanos(duration_nanos).unwrap(),
        domain_id,
        SequenceNumber::new(sequence),
    )
    .unwrap()
}

fn layout(channels: usize) -> ChannelLayout {
    ChannelLayout::new(if channels == 1 {
        vec![Channel::Mono]
    } else {
        vec![Channel::Left, Channel::Right]
    })
    .unwrap()
}

fn live_config(hz: u32, fps: u32, channels: usize, latency_frames: usize) -> LiveSourceConfig {
    LiveSourceConfig::new(
        SampleRate::new(hz).unwrap(),
        channels,
        FrameRate::new(fps, 1).unwrap(),
        latency_frames,
    )
}

/// A deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// A device on its own clock and the engine consuming it through the
/// adapter, stepped one engine frame at a time.
struct Rig {
    source: LiveBlockSource<u64>,
    hz: u32,
    fps: u32,
    channels: usize,
    /// Output (and nominal input) samples per frame.
    frame: usize,
    domain: ClockDomainId,
    /// Device skew against the engine, in ppm.
    ppm: f64,
    /// Largest random delivery delay, in seconds.
    delivery_jitter: f64,
    /// Largest random host-stamp error of a block's timestamp, in seconds.
    stamp_jitter: f64,
    /// Device time of the next block's start, in engine seconds.
    device_time: f64,
    /// Device samples produced (the device's own sample timeline).
    produced: u64,
    sequence: u64,
    pictures_produced: u64,
    /// Blocks still to lose (a gap): produced and dropped.
    lose_blocks: usize,
    /// Delivery held until this engine time (a stall, then a burst).
    held_until: f64,
    /// Next block is flagged discontinuous / restarts its sequence.
    flag_next: bool,
    restart_sequence: bool,
    /// Blocks produced but not yet delivered, with their arrival times.
    in_flight: Vec<(f64, Option<AudioBlock>, Vec<u64>)>,
    frames: u64,
    rng: Rng,
    output: Vec<Vec<f32>>,
    /// Last output sample per channel, and the largest step seen while
    /// running.
    last: Vec<f32>,
    max_step: f32,
    hash: u64,
    max_av_error: f64,
    peak_fill: f64,
}

impl Rig {
    fn new(hz: u32, fps: u32, channels: usize, latency_frames: usize, seed: u64) -> Self {
        let live = live_config(hz, fps, channels, latency_frames);
        let frame = live.frame_samples();
        Self {
            source: LiveBlockSource::new(LiveBlockConfig::new(live)).unwrap(),
            hz,
            fps,
            channels,
            frame,
            domain: domain(7),
            ppm: 0.0,
            delivery_jitter: 0.0,
            stamp_jitter: 0.0,
            device_time: 0.0,
            produced: 0,
            sequence: 0,
            pictures_produced: 0,
            lose_blocks: 0,
            held_until: 0.0,
            flag_next: false,
            restart_sequence: false,
            in_flight: Vec::new(),
            frames: 0,
            rng: Rng(seed | 1),
            output: vec![vec![0.0; frame]; channels],
            last: vec![0.0; channels],
            max_step: 0.0,
            hash: 0xcbf2_9ce4_8422_2325,
            max_av_error: 0.0,
            peak_fill: 0.0,
        }
    }

    fn now(&self) -> f64 {
        self.frames as f64 / f64::from(self.fps)
    }

    /// The device's tone, by device sample.
    fn sample(&self, index: u64, channel: usize) -> f32 {
        let phase = (index % u64::from(self.hz)) as f64 / f64::from(self.hz);
        (AMPLITUDE * (core::f64::consts::TAU * TONE_HZ * phase + channel as f64).sin()) as f32
    }

    /// Produces the device's blocks up to engine time `until`.
    fn produce(&mut self, until: f64) {
        let rate = f64::from(self.hz) * (1.0 + self.ppm * 1e-6);
        let block_seconds = BLOCK as f64 / rate;
        while self.device_time + block_seconds <= until {
            let start = self.device_time;
            self.device_time += block_seconds;
            let first = self.produced;
            self.produced += BLOCK as u64;
            self.sequence = if self.restart_sequence {
                self.restart_sequence = false;
                1
            } else {
                self.sequence + 1
            };
            // Pictures whose frame ends inside this block travel with it.
            let mut pictures = Vec::new();
            while (self.pictures_produced + 1) * self.frame as u64 <= self.produced {
                pictures.push(self.pictures_produced);
                self.pictures_produced += 1;
            }
            if self.lose_blocks > 0 {
                // Lost on the way: its audio never arrives, its pictures do.
                self.lose_blocks -= 1;
                let arrival = self.device_time.max(self.held_until);
                self.in_flight.push((arrival, None, pictures));
                continue;
            }
            let planes = (0..self.channels)
                .map(|channel| {
                    (0..BLOCK)
                        .map(|offset| self.sample(first + offset as u64, channel))
                        .collect()
                })
                .collect();
            let stamp = start + (self.rng.unit() * 2.0 - 1.0) * self.stamp_jitter;
            let mut timing = timing(
                (stamp * 1e9).round() as i64,
                (BLOCK as f64 * 1e9 / f64::from(self.hz)).round() as u64,
                self.domain,
                self.sequence,
            );
            if self.flag_next {
                self.flag_next = false;
                timing = timing.with_flags(MediaFlags::DISCONTINUITY);
            }
            let block = AudioBlock::new(
                timing,
                SampleRate::new(self.hz).unwrap(),
                layout(self.channels),
                planes,
            )
            .unwrap();
            let arrival =
                (self.device_time + self.rng.unit() * self.delivery_jitter).max(self.held_until);
            self.in_flight.push((arrival, Some(block), pictures));
        }
    }

    /// One engine frame: deliver what arrived, then pull.
    fn step(&mut self) {
        let end = (self.frames + 1) as f64 / f64::from(self.fps);
        self.produce(end);
        // Deliver in production order; a block arrives no earlier than the
        // one before it (one delivery queue).
        let mut floor = 0.0_f64;
        let mut delivered = 0;
        for (arrival, block, pictures) in &self.in_flight {
            let at = arrival.max(floor);
            if at > end {
                break;
            }
            floor = at;
            if let Some(block) = block {
                self.source.push_audio(block, at).unwrap();
            }
            for picture in pictures {
                self.source.push_picture(*picture);
            }
            delivered += 1;
        }
        self.in_flight.drain(..delivered);
        let fill = self.source.live_telemetry().sync.buffer_fill_samples;
        self.peak_fill = self.peak_fill.max(fill);
        let mut planes: Vec<&mut [f32]> = self.output.iter_mut().map(Vec::as_mut_slice).collect();
        let pull = self.source.pull(&mut planes, end).unwrap();
        self.frames += 1;
        self.check(pull.picture);
    }

    fn check(&mut self, picture: PictureShown) {
        for (channel, plane) in self.output.iter().enumerate() {
            for sample in plane {
                self.hash = (self.hash ^ u64::from(sample.to_bits())).wrapping_mul(0x100_0000_01b3);
                self.max_step = self.max_step.max((sample - self.last[channel]).abs());
                self.last[channel] = *sample;
            }
        }
        let telemetry = self.source.live_telemetry();
        if telemetry.sync.state != SourceSyncState::Running
            || !self.source.pictures_live()
            || telemetry.sync.rendered_output_samples < 10 * u64::from(self.hz)
            || matches!(picture, PictureShown::None | PictureShown::Late)
        {
            return;
        }
        if let Some(shown) = self.source.current_picture() {
            let sync = telemetry.sync;
            // Concealed gap silence keeps the device's sample timeline.
            let position = (sync.consumed_input_samples
                + sync.priming_trimmed_samples
                + sync.resync_dropped_samples) as f64;
            let audio = position / self.frame as f64 - 1.0;
            self.max_av_error = self.max_av_error.max((*shown as f64 - audio).abs());
        }
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds * f64::from(self.fps)).round() as u64 {
            self.step();
        }
    }

    fn telemetry(&self) -> LiveBlockTelemetry {
        self.source.telemetry()
    }

    fn target(&self) -> f64 {
        self.telemetry().live.sync.target_fill_samples as f64
    }

    /// The largest step a continuous tone through a declick ramp can make.
    fn step_bound(&self) -> f32 {
        let tone = AMPLITUDE * core::f64::consts::TAU * TONE_HZ / f64::from(self.hz);
        let declick = f64::from(self.hz) / 1_000.0;
        (1.05 * (tone + AMPLITUDE / (declick + 1.0))) as f32
    }
}

/// Seconds until the applied correction, averaged over the last minute
/// (arrival jitter moves it from frame to frame), stays within
/// `tolerance` ppm of `ppm` for the rest of the run, sampled once a second;
/// and the largest error of that average from then on.
fn settle(rig: &mut Rig, ppm: f64, seconds: u64, tolerance: f64) -> Option<u64> {
    let mut window = VecDeque::with_capacity(61);
    let mut settled = None;
    for second in 0..seconds {
        rig.run(1.0);
        window.push_back(rig.telemetry().live.sync.applied_correction_ppm);
        if window.len() > 60 {
            window.pop_front();
        }
        let mean = window.iter().sum::<f64>() / window.len() as f64;
        match ((mean - ppm).abs() <= tolerance, settled) {
            (true, None) => settled = Some(second + 1),
            (false, _) => settled = None,
            (true, Some(_)) => {}
        }
    }
    settled
}

fn assert_clean(rig: &Rig) {
    let sync = rig.telemetry().live.sync;
    assert_eq!(sync.resyncs, 0, "{sync:?}");
    assert_eq!(sync.overruns, 0, "{sync:?}");
    assert_eq!(sync.underruns, 0, "{sync:?}");
    assert!(
        rig.max_step <= rig.step_bound(),
        "step {} above {}",
        rig.max_step,
        rig.step_bound()
    );
}

/// A device skewed by `ppm`, delivering up to `jitter` seconds late and
/// stamping its blocks with 2 ms of error, for ten minutes: the
/// minute-averaged correction settles within `tolerance` ppm by five
/// minutes, and so does the estimate.
///
/// The loop reads drift from the buffer fill (1 ppm is 0.1 ms of fill
/// over 100 s), so random arrival jitter moves the estimate by a few ppm
/// per millisecond of jitter; the ratio wander that results is far below
/// audibility (10 ppm is 0.02 cent) and the backlog stays bounded.
fn jittered_skew(ppm: f64, jitter: f64, tolerance: f64) {
    let mut rig = Rig::new(8_000, 25, 1, 3, 0x5eed);
    rig.ppm = ppm;
    rig.delivery_jitter = jitter;
    rig.stamp_jitter = 0.002;
    let target = rig.target();
    let settled = settle(&mut rig, ppm, 600, tolerance);
    let telemetry = rig.telemetry();
    let sync = telemetry.live.sync;
    println!(
        "{ppm:+} ppm, {:.0} ms jitter, 10 min: estimate {:+.3} ppm, applied {:+.3} ppm, settled {settled:?} s, \
         peak fill {:.0} (target {target}), picture drops {} repeats {}, max A/V {:.2} frames, max step {:.3}",
        jitter * 1e3,
        sync.estimated_drift_ppm,
        sync.applied_correction_ppm,
        rig.peak_fill,
        telemetry.live.picture_drops,
        telemetry.live.picture_repeats,
        rig.max_av_error,
        rig.max_step,
    );
    assert_clean(&rig);
    assert!(settled.is_some_and(|seconds| seconds <= 300), "{settled:?}");
    assert!(
        (sync.estimated_drift_ppm - ppm).abs() < tolerance,
        "{sync:?}"
    );
    assert!((sync.smoothed_fill_samples.unwrap() - target).abs() < 0.1 * target);
    assert!(
        rig.peak_fill < 1.5 * target + BLOCK as f64,
        "{}",
        rig.peak_fill
    );
    assert!(rig.max_av_error <= 1.5, "{}", rig.max_av_error);
    assert!(rig.source.queued_pictures() <= 6);
    // Contiguous sequences never become gaps, whatever the stamps say.
    assert_eq!(sync.gaps_filled, 0);
    assert_eq!(sync.overlaps_trimmed, 0);
    assert_eq!(sync.splices, 0);
    let skewed_frames = 600.0 * 25.0 * ppm.abs() * 1e-6;
    let corrections = if ppm > 0.0 {
        telemetry.live.picture_drops
    } else {
        telemetry.live.picture_repeats
    };
    assert!(
        (corrections as f64 - skewed_frames).abs() <= 3.0,
        "{corrections} corrections for {skewed_frames} skewed frames"
    );
}

#[test]
fn a_block_source_200_ppm_fast_converges_exactly_without_jitter() {
    jittered_skew(200.0, 0.0, 0.5);
}

#[test]
fn a_block_source_200_ppm_fast_with_jitter_converges_and_stays_bounded() {
    jittered_skew(200.0, 0.002, 5.0);
}

#[test]
fn a_block_source_200_ppm_slow_with_jitter_converges_and_stays_bounded() {
    jittered_skew(-200.0, 0.002, 5.0);
}

#[test]
fn a_block_source_with_network_jitter_stays_bounded() {
    jittered_skew(200.0, 0.012, 30.0);
}

#[test]
fn lost_blocks_are_concealed_on_the_timeline_without_a_click_or_resync() {
    // The loss is concealed only when the next block arrives, so the
    // latency covers the lost run, a block, and the jitter (200 ms).
    let mut rig = Rig::new(8_000, 25, 1, 5, 0x9a9);
    rig.ppm = 150.0;
    rig.delivery_jitter = 0.008;
    rig.stamp_jitter = 0.001;
    rig.run(300.0);
    let before = rig.telemetry();
    // Three blocks (96 ms) lost, twice.
    rig.lose_blocks = 3;
    rig.run(30.0);
    rig.lose_blocks = 3;
    rig.run(500.0);
    let after = rig.telemetry();
    let sync = after.live.sync;
    println!(
        "gaps: {} filled, {} samples concealed, estimate {:+.3} -> {:+.3} ppm, max A/V {:.2}, max step {:.3}",
        sync.gaps_filled,
        sync.concealed_gap_samples,
        before.live.sync.estimated_drift_ppm,
        sync.estimated_drift_ppm,
        rig.max_av_error,
        rig.max_step,
    );
    assert_clean(&rig);
    assert_eq!(after.counters.missed_blocks, 6);
    assert_eq!(sync.gaps_filled, 2);
    // Concealed to within the stamp jitter (a millisecond, 8 samples).
    let lost = 6 * BLOCK as u64;
    assert!(sync.concealed_gap_samples.abs_diff(lost) <= 20, "{sync:?}");
    assert!((sync.estimated_drift_ppm - 150.0).abs() < 2.0, "{sync:?}");
    assert!(rig.max_av_error <= 1.5, "{}", rig.max_av_error);
}

#[test]
fn a_delivery_burst_is_absorbed_without_an_underrun_or_resync() {
    let mut rig = Rig::new(8_000, 25, 1, 3, 0xb0b);
    rig.ppm = -120.0;
    rig.delivery_jitter = 0.001;
    rig.run(300.0);
    let target = rig.target();
    // The device's driver stalls 70 ms (under the 120 ms target), then
    // delivers its backlog at once; five times a minute apart.
    let mut worst = 0.0_f64;
    for _ in 0..5 {
        rig.held_until = rig.now() + 0.07;
        for _ in 0..60 {
            rig.run(1.0);
            let applied = rig.telemetry().live.sync.applied_correction_ppm;
            worst = worst.max((applied + 120.0).abs());
        }
    }
    // Then quiet again: the estimate returns to the true skew.
    rig.run(300.0);
    let sync = rig.telemetry().live.sync;
    println!(
        "bursts: peak fill {:.0} (target {target}), worst correction error {worst:.2} ppm, \
         estimate {:+.3} ppm after, max step {:.3}",
        rig.peak_fill, sync.estimated_drift_ppm, rig.max_step,
    );
    assert_clean(&rig);
    assert!(rig.peak_fill < 2.0 * target, "{}", rig.peak_fill);
    assert!(worst < 150.0, "{worst}");
    assert!((sync.estimated_drift_ppm + 120.0).abs() < 2.0, "{sync:?}");
    assert!(rig.max_av_error <= 1.5, "{}", rig.max_av_error);
}

#[test]
fn a_long_stall_underruns_declicked_and_reprimes_with_its_estimate() {
    let mut rig = Rig::new(8_000, 25, 1, 3, 0x57a1);
    rig.ppm = 250.0;
    rig.run(200.0);
    // 300 ms: longer than the target, shorter than the capacity.
    rig.held_until = rig.now() + 0.3;
    rig.run(200.0);
    let sync = rig.telemetry().live.sync;
    assert_eq!(sync.underruns, 1, "{sync:?}");
    assert_eq!(sync.resyncs, 0, "{sync:?}");
    assert_eq!(sync.overruns, 0, "{sync:?}");
    assert_eq!(sync.state, SourceSyncState::Running);
    assert!((sync.estimated_drift_ppm - 250.0).abs() < 3.0, "{sync:?}");
    assert!(rig.max_step <= rig.step_bound(), "{}", rig.max_step);
}

#[test]
fn flagged_and_restarted_sequences_splice_with_declick_ramps() {
    let mut rig = Rig::new(8_000, 25, 1, 3, 0x51ce);
    rig.ppm = 80.0;
    rig.run(300.0);
    rig.flag_next = true;
    rig.run(20.0);
    rig.restart_sequence = true;
    rig.run(300.0);
    let telemetry = rig.telemetry();
    let sync = telemetry.live.sync;
    assert_eq!(telemetry.counters.discontinuities, 2);
    assert_eq!(sync.splices, 2, "{sync:?}");
    assert_eq!(sync.resyncs + sync.overruns + sync.underruns, 0, "{sync:?}");
    assert!(rig.max_step <= rig.step_bound(), "{}", rig.max_step);
    assert!((sync.estimated_drift_ppm - 80.0).abs() < 3.0, "{sync:?}");
}

#[test]
fn a_new_clock_domain_restarts_and_forgets_the_drift() {
    let mut rig = Rig::new(8_000, 25, 1, 2, 0xd0);
    rig.ppm = 300.0;
    rig.run(200.0);
    assert!(rig.telemetry().live.sync.estimated_drift_ppm > 250.0);
    rig.domain = domain(8);
    rig.ppm = -100.0;
    rig.run(0.2);
    let telemetry = rig.telemetry();
    assert_eq!(telemetry.counters.clock_domain_changes, 1);
    assert_eq!(telemetry.live.restarts, 1);
    assert!(telemetry.live.sync.estimated_drift_ppm.abs() < 1.0);
    let settled = settle(&mut rig, -100.0, 300, 5.0);
    assert!(settled.is_some_and(|seconds| seconds <= 250), "{settled:?}");
}

#[test]
fn stereo_48k_runs_identically_twice() {
    let run = || {
        let mut rig = Rig::new(48_000, 30, 2, 2, 0xfeed);
        rig.ppm = 200.0;
        rig.delivery_jitter = 0.004;
        rig.stamp_jitter = 0.0005;
        rig.run(20.0);
        rig.lose_blocks = 2;
        rig.run(10.0);
        (rig.hash, rig.telemetry())
    };
    let (first, telemetry) = run();
    let (second, again) = run();
    assert_eq!(first, second);
    assert_eq!(telemetry, again);
    assert_eq!(telemetry.live.sync.gaps_filled, 1);
}

#[test]
fn blocks_of_another_format_are_refused_without_a_change() {
    let mut source: LiveBlockSource<()> =
        LiveBlockSource::new(LiveBlockConfig::new(live_config(48_000, 30, 2, 2))).unwrap();
    let rate = AudioBlock::silence(
        timing(0, 1_000_000, domain(1), 1),
        SampleRate::new(44_100).unwrap(),
        layout(2),
        441,
    )
    .unwrap();
    assert_eq!(
        source.push_audio(&rate, 0.0),
        Err(LiveBlockError::SampleRateMismatch {
            expected: 48_000,
            actual: 44_100
        })
    );
    let mono = AudioBlock::silence(
        timing(0, 1_000_000, domain(1), 1),
        SampleRate::new(48_000).unwrap(),
        layout(1),
        480,
    )
    .unwrap();
    assert_eq!(
        source.push_audio(&mono, 0.0),
        Err(LiveBlockError::ChannelCountMismatch {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(source.telemetry().counters.blocks, 0);
    assert_eq!(source.live_telemetry().sync.pushed_samples, 0);
}

#[test]
fn placement_follows_sequence_then_time() {
    let mut source: LiveBlockSource<()> =
        LiveBlockSource::new(LiveBlockConfig::new(live_config(48_000, 30, 1, 2))).unwrap();
    let block = |pts_ms: i64, sequence: u64| {
        AudioBlock::silence(
            timing(pts_ms * 1_000_000, 10_000_000, domain(3), sequence),
            SampleRate::new(48_000).unwrap(),
            layout(1),
            480,
        )
        .unwrap()
    };
    let push = |source: &mut LiveBlockSource<()>, block: &AudioBlock| {
        source.push_audio(block, 0.0).unwrap()
    };
    assert_eq!(
        push(&mut source, &block(0, 1)).placement,
        BlockPlacement::Anchored
    );
    // Stamped 4 ms late: still the next sequence, still contiguous.
    let next = push(&mut source, &block(14, 2));
    assert_eq!(next.placement, BlockPlacement::Contiguous);
    assert_eq!(next.report.continuity, PushContinuity::Contiguous);
    // Two blocks lost (sequences 3 and 4): placed 20 ms after block 2's end.
    let gap = push(&mut source, &block(44, 5));
    assert_eq!(gap.placement, BlockPlacement::Timed { missed_blocks: 2 });
    assert_eq!(
        gap.report.continuity,
        PushContinuity::GapFilled { samples: 960 }
    );
    // A lost block whose successor is stamped early: a trimmed overlap.
    let overlap = push(&mut source, &block(52, 7));
    assert_eq!(
        overlap.placement,
        BlockPlacement::Timed { missed_blocks: 1 }
    );
    assert_eq!(
        overlap.report.continuity,
        PushContinuity::OverlapTrimmed { samples: 96 }
    );
    // Far beyond the concealment limit: spliced.
    let jump = push(&mut source, &block(5_000, 9));
    assert_eq!(jump.report.continuity, PushContinuity::Spliced);
    // A counter restart is a discontinuity.
    let restart = push(&mut source, &block(5_010, 1));
    assert_eq!(restart.placement, BlockPlacement::Discontinuous);
    assert_eq!(restart.report.continuity, PushContinuity::Spliced);
    let counters = source.telemetry().counters;
    assert_eq!(counters.blocks, 6);
    assert_eq!(counters.missed_blocks, 4);
    assert_eq!(counters.discontinuities, 1);
}

#[test]
fn the_picture_queue_is_bounded_and_stalls_after_late_frames() {
    let config = LiveBlockConfig::new(live_config(48_000, 30, 1, 2)).with_picture_capacity(3);
    let mut source: LiveBlockSource<u32> = LiveBlockSource::new(config).unwrap();
    for picture in 0..5 {
        source.push_picture(picture);
    }
    assert_eq!(source.queued_pictures(), 3);
    assert_eq!(source.telemetry().counters.picture_overflows, 2);
    assert!(source.pictures_live());
    let mut plane = vec![0.0_f32; 1_600];
    let mut shown = Vec::new();
    for frame in 0..6 {
        let mut planes = [plane.as_mut_slice()];
        let pull = source.pull(&mut planes, f64::from(frame) / 30.0).unwrap();
        shown.push((pull.picture, source.current_picture().copied()));
    }
    // Priming audio: the clock does not judge the pictures yet, so they
    // advance one a frame, then run late and stall.
    assert_eq!(shown[0], (PictureShown::New { skipped: 0 }, Some(2)));
    assert_eq!(shown[2], (PictureShown::New { skipped: 0 }, Some(4)));
    assert_eq!(shown[3], (PictureShown::Late, Some(4)));
    assert_eq!(shown[4], (PictureShown::Late, Some(4)));
    assert_eq!(shown[5], (PictureShown::None, Some(4)));
    let counters = source.telemetry().counters;
    assert_eq!(counters.late_pictures, 2);
    assert_eq!(counters.picture_stalls, 1);
    assert!(!source.pictures_live());
    drop(plane);
}

/// Hours of a wandering device (a random walk within ±200 ppm, as a
/// crystal does with temperature), with delivery and stamp jitter, a lost
/// block run every few minutes and a delivery burst every few minutes.
/// Reports every simulated hour; fails on any resync, overrun, underrun,
/// click, unbounded backlog, A/V error beyond 2 frames, or a quiet minute
/// whose mean correction strays 10 ppm from the skew. 8 kHz mono keeps it fast;
/// the loop's dynamics are in seconds, not samples.
#[test]
#[ignore = "long soak; set FM_AU004_SOAK_HOURS (default 6)"]
fn long_drift_soak() {
    let hours: u64 = std::env::var("FM_AU004_SOAK_HOURS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6);
    // 200 ms of latency covers a lost run of three blocks (96 ms), the
    // block in flight, and the jitter.
    let mut rig = Rig::new(8_000, 25, 1, 5, 0x50a6);
    rig.delivery_jitter = 0.003;
    rig.stamp_jitter = 0.002;
    rig.ppm = 120.0;
    let target = rig.target();
    let mut faults = Rng(0xfa17);
    let mut worst_tracking = 0.0_f64;
    // Minutes since the last injected fault.
    let mut quiet = 0_u32;
    for hour in 1..=hours {
        for minute in 0..60 {
            // The crystal wanders by at most 10 ppm a minute.
            rig.ppm = (rig.ppm + (faults.unit() * 2.0 - 1.0) * 10.0).clamp(-200.0, 200.0);
            quiet += 1;
            match faults.next() % 6 {
                0 => {
                    rig.lose_blocks = 1 + (faults.next() % 3) as usize;
                    quiet = 0;
                }
                1 => {
                    rig.held_until = rig.now() + 0.02 + faults.unit() * 0.05;
                    quiet = 0;
                }
                _ => {}
            }
            let mut applied = 0.0;
            for _ in 0..60 {
                rig.run(1.0);
                applied += rig.telemetry().live.sync.applied_correction_ppm / 60.0;
            }
            // The minute's mean correction against the device's skew, once
            // the loop has first settled and two minutes after a fault (a
            // fault's fill transient moves the ratio by up to ~100 ppm for
            // tens of seconds, inaudibly).
            if (hour > 1 || minute >= 10) && quiet >= 2 {
                worst_tracking = worst_tracking.max((applied - rig.ppm).abs());
            }
        }
        let telemetry = rig.telemetry();
        let sync = telemetry.live.sync;
        println!(
            "hour {hour}: device {:+.1} ppm, estimate {:+.2}, applied {:+.2}, worst tracking {worst_tracking:.2} ppm, \
             fill {:.0}/{target} peak {:.0}, gaps {}, picture drops {} repeats {}, late {}, max A/V {:.2}, max step {:.3}",
            rig.ppm,
            sync.estimated_drift_ppm,
            sync.applied_correction_ppm,
            sync.buffer_fill_samples,
            rig.peak_fill,
            sync.gaps_filled,
            telemetry.live.picture_drops,
            telemetry.live.picture_repeats,
            telemetry.counters.late_pictures,
            rig.max_av_error,
            rig.max_step,
        );
        assert_clean(&rig);
        assert!(rig.peak_fill < 2.0 * target, "{}", rig.peak_fill);
        // Pictures catch up with a fault's audio jump within a second.
        assert!(rig.max_av_error <= 2.0, "{}", rig.max_av_error);
        assert!(worst_tracking < 10.0, "{worst_tracking}");
    }
}
