//! AU-004 acceptance: sample-rate conversion into the 48 kHz engine clock,
//! buffer-fill drift compensation, discontinuity handling, and telemetry.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::needless_range_loop
)]

use fm_audio::{
    DiscontinuityPolicy, DriftControlConfig, DriftController, ENGINE_SAMPLE_RATE,
    ENGINE_SAMPLE_RATE_HZ, JumpAction, PushContinuity, ResamplerError, SincKernel, SourceSync,
    SourceSyncConfig, SourceSyncError, SourceSyncState,
};
use fm_types::SampleRate;

const PULL: usize = 480;

fn rate(hz: u32) -> SampleRate {
    SampleRate::new(hz).unwrap()
}

fn tone(index: u64, frequency: f64, rate_hz: u32, amplitude: f64) -> f32 {
    let phase = (index % u64::from(rate_hz)) as f64 / f64::from(rate_hz);
    (amplitude * (core::f64::consts::TAU * frequency * phase).sin()) as f32
}

/// Source pushing on its own clock, running `drift_ppm` fast relative to the
/// engine, and an engine pulling fixed blocks.
struct Sim {
    sync: SourceSync,
    input_hz: u32,
    drift_ppm: i64,
    produced: u64,
    pulled: u64,
    frequency: f64,
    scratch: Vec<f32>,
}

impl Sim {
    fn new(sync: SourceSync, drift_ppm: i64, frequency: f64) -> Self {
        let input_hz = sync.config().input_rate().hertz();
        Self {
            sync,
            input_hz,
            drift_ppm,
            produced: 0,
            pulled: 0,
            frequency,
            scratch: vec![0.0; 192_000],
        }
    }

    /// Source samples produced by the time `pulled` engine samples have elapsed.
    fn due(&self, pulled: u64) -> u64 {
        let numerator = i128::from(pulled)
            * i128::from(self.input_hz)
            * (1_000_000 + i128::from(self.drift_ppm));
        (numerator / (i128::from(ENGINE_SAMPLE_RATE_HZ) * 1_000_000)) as u64
    }

    fn produce(&mut self, count: u64) {
        let count = count as usize;
        for (offset, sample) in self.scratch[..count].iter_mut().enumerate() {
            *sample = tone(
                self.produced + offset as u64,
                self.frequency,
                self.input_hz,
                0.5,
            );
        }
        self.sync
            .push(self.produced, &[&self.scratch[..count]])
            .unwrap();
        self.produced += count as u64;
    }

    /// One engine period: the source delivers what it produced during the
    /// period, then the engine pulls `PULL` samples.
    fn tick(&mut self, output: &mut [f32]) -> fm_audio::PullReport {
        let due = self.due(self.pulled + PULL as u64);
        self.produce(due - self.produced);
        self.pulled += PULL as u64;
        self.sync.pull(&mut [output]).unwrap()
    }
}

fn mono(input_hz: u32) -> SourceSyncConfig {
    SourceSyncConfig::new(rate(input_hz), 1)
}

/// Least-squares fit of `a sin + b cos + c` at a known frequency; returns
/// (fitted amplitude, SNR in dB of the residual against the fitted tone).
fn fit_tone(samples: &[f32], frequency: f64, rate_hz: f64) -> (f64, f64) {
    let omega = core::f64::consts::TAU * frequency / rate_hz;
    let mut m = [[0.0_f64; 3]; 3];
    let mut v = [0.0_f64; 3];
    for (n, &y) in samples.iter().enumerate() {
        let basis = [(omega * n as f64).sin(), (omega * n as f64).cos(), 1.0];
        for row in 0..3 {
            for column in 0..3 {
                m[row][column] += basis[row] * basis[column];
            }
            v[row] += basis[row] * f64::from(y);
        }
    }
    let solution = solve3(m, v);
    let mut residual = 0.0;
    for (n, &y) in samples.iter().enumerate() {
        let model = solution[0] * (omega * n as f64).sin()
            + solution[1] * (omega * n as f64).cos()
            + solution[2];
        residual += (f64::from(y) - model).powi(2);
    }
    let amplitude = solution[0].hypot(solution[1]);
    let signal = amplitude * amplitude / 2.0 * samples.len() as f64;
    (amplitude, 10.0 * (signal / residual).log10())
}

fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> [f64; 3] {
    for pivot in 0..3 {
        for row in pivot + 1..3 {
            let factor = m[row][pivot] / m[pivot][pivot];
            for column in pivot..3 {
                m[row][column] -= factor * m[pivot][column];
            }
            v[row] -= factor * v[pivot];
        }
    }
    let mut x = [0.0; 3];
    for row in (0..3).rev() {
        let mut sum = v[row];
        for column in row + 1..3 {
            sum -= m[row][column] * x[column];
        }
        x[row] = sum / m[row][row];
    }
    x
}

/// Converts a tone at a fixed nominal ratio and returns the steady-state
/// engine-rate output.
fn convert_tone(input_hz: u32, frequency: f64, output_samples: usize) -> Vec<f32> {
    let sync =
        SourceSync::new(mono(input_hz).with_drift_control(DriftControlConfig::disabled())).unwrap();
    let mut sim = Sim::new(sync, 0, frequency);
    let mut block = [0.0_f32; PULL];
    let mut collected = Vec::with_capacity(output_samples);
    let mut warmup = 20;
    while collected.len() < output_samples {
        let report = sim.tick(&mut block);
        assert!(!report.underrun);
        if report.rendered_samples == PULL {
            if warmup == 0 {
                collected.extend_from_slice(&block);
            } else {
                warmup -= 1;
            }
        }
    }
    collected
}

#[test]
fn engine_clock_defaults_to_48_khz() {
    assert_eq!(ENGINE_SAMPLE_RATE.hertz(), 48_000);
    let config = mono(44_100);
    assert_eq!(config.output_rate(), ENGINE_SAMPLE_RATE);
    assert_eq!(config.target_fill_samples(), 882);
    let sync = SourceSync::new(config).unwrap();
    assert_eq!(sync.state(), SourceSyncState::Priming);
    assert_eq!(sync.telemetry().output_rate_hz, 48_000);

    let taps = |hz| {
        SincKernel::new(rate(hz), ENGINE_SAMPLE_RATE)
            .unwrap()
            .taps()
    };
    assert_eq!(taps(44_100), 72);
    assert_eq!(taps(48_000), 72);
    assert_eq!(taps(96_000), 142);
    let cutoff = SincKernel::new(rate(44_100), ENGINE_SAMPLE_RATE)
        .unwrap()
        .cutoff_hz();
    assert!((cutoff - 20_065.5).abs() < 0.1, "{cutoff}");
}

#[test]
fn converted_sines_meet_snr_and_passband_bounds() {
    // (input rate, tone, minimum SNR dB, maximum passband error dB).
    let cases = [
        (44_100, 1_000.0, 100.0, 0.01),
        (44_100, 10_000.0, 100.0, 0.01),
        (44_100, 17_000.0, 100.0, 0.01),
        (48_000, 1_000.0, 100.0, 0.01),
        (48_000, 10_000.0, 100.0, 0.01),
        (48_000, 17_000.0, 100.0, 0.01),
        (96_000, 1_000.0, 100.0, 0.01),
        (96_000, 10_000.0, 100.0, 0.01),
        (96_000, 17_000.0, 100.0, 0.01),
    ];
    for (input_hz, frequency, minimum_snr, maximum_error_db) in cases {
        let output = convert_tone(input_hz, frequency, 24_000);
        let (amplitude, snr) = fit_tone(&output, frequency, 48_000.0);
        let gain_db = 20.0 * (amplitude / 0.5).log10();
        println!("{input_hz} Hz -> 48 kHz, {frequency} Hz: SNR {snr:.1} dB, gain {gain_db:+.4} dB");
        assert!(snr >= minimum_snr, "{input_hz} {frequency}: SNR {snr}");
        assert!(
            gain_db.abs() <= maximum_error_db,
            "{input_hz} {frequency}: {gain_db}"
        );
    }
}

#[test]
fn decimation_rejects_content_above_engine_nyquist() {
    for frequency in [26_000.0, 30_000.0, 40_000.0] {
        let output = convert_tone(96_000, frequency, 24_000);
        let rms = (output.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / output.len() as f64)
            .sqrt();
        let level_db = 20.0 * (rms / (0.5 / 2.0_f64.sqrt())).log10();
        println!("96 kHz tone {frequency} Hz aliased residue {level_db:.1} dB");
        assert!(level_db < -100.0, "{frequency}: {level_db}");
    }
}

#[test]
fn nominal_ratio_consumes_an_exact_sample_count_over_long_runs() {
    let sync =
        SourceSync::new(mono(44_100).with_drift_control(DriftControlConfig::disabled())).unwrap();
    let mut sim = Sim::new(sync, 0, 440.0);
    let mut block = [0.0_f32; PULL];
    // Two minutes of engine time.
    for _ in 0..12_000 {
        assert!(!sim.tick(&mut block).underrun);
    }
    let telemetry = sim.sync.telemetry();
    assert_eq!(telemetry.underruns, 0);
    assert_eq!(telemetry.overruns, 0);
    assert_eq!(telemetry.pushed_samples, 44_100 * 120);
    let rendered = telemetry.rendered_output_samples;
    assert_eq!(rendered + telemetry.silent_output_samples, 48_000 * 120);
    assert_eq!(telemetry.consumed_input_samples, rendered * 147 / 160);
    let fractional = (rendered * 147 % 160) as f64 / 160.0;
    let expected_fill =
        (telemetry.pushed_samples - telemetry.consumed_input_samples) as f64 - fractional;
    assert!((telemetry.buffer_fill_samples - expected_fill).abs() < 1e-9);
    assert_eq!(telemetry.applied_correction_ppm, 0.0);
}

fn drift_run(input_hz: u32, drift_ppm: i64, seconds: u64) {
    let sync = SourceSync::new(mono(input_hz)).unwrap();
    let target = sync.config().target_fill_samples() as f64;
    let mut sim = Sim::new(sync, drift_ppm, 1_000.0);
    let mut block = [0.0_f32; PULL];
    let ticks = seconds * 100;
    let late_ticks = 1_000;
    let mut late_fill_error = 0.0_f64;
    let mut late_ratio_error = 0.0_f64;
    let mut late_ratio_sum = 0.0_f64;
    for tick in 0..ticks {
        let report = sim.tick(&mut block);
        assert!(
            !report.underrun,
            "{input_hz} {drift_ppm}: underrun at {tick}"
        );
        let telemetry = sim.sync.telemetry();
        assert_eq!(telemetry.overruns, 0);
        if tick >= ticks - late_ticks {
            let fill = telemetry.smoothed_fill_samples.unwrap();
            late_fill_error = late_fill_error.max((fill - target).abs());
            let ratio_error = telemetry.applied_correction_ppm - drift_ppm as f64;
            late_ratio_error = late_ratio_error.max(ratio_error.abs());
            late_ratio_sum += ratio_error;
        }
    }
    let telemetry = sim.sync.telemetry();
    let late_ratio_mean = late_ratio_sum / late_ticks as f64;
    println!(
        "{input_hz} Hz at {drift_ppm:+} ppm: estimate {:+.3} ppm, peak fill {} (target {target}), \
         last 10 s: fill error <= {late_fill_error:.2} samples, ratio error mean {late_ratio_mean:+.3} \
         max {late_ratio_error:.3} ppm",
        telemetry.estimated_drift_ppm, telemetry.peak_fill_samples
    );
    assert_eq!(telemetry.underruns, 0);
    assert_eq!(telemetry.overruns, 0);
    assert!((telemetry.estimated_drift_ppm - drift_ppm as f64).abs() < 1.0);
    assert!(late_ratio_mean.abs() < 0.5, "{late_ratio_mean}");
    assert!(late_ratio_error < 3.0, "{late_ratio_error}");
    assert!(late_fill_error < 2.0, "{late_fill_error}");
    assert!(telemetry.peak_fill_samples < sim.sync.config().capacity_samples() / 4);
}

#[test]
fn drift_compensation_converges_for_plus_and_minus_100_ppm() {
    for input_hz in [44_100, 48_000, 96_000] {
        for drift_ppm in [-100, 100] {
            drift_run(input_hz, drift_ppm, 30);
        }
    }
}

#[test]
fn drift_controller_stays_bounded_over_an_hour() {
    // Fill-only model: fill changes by (drift - correction) * input rate.
    for (drift_ppm, max_ppm) in [
        (100.0, 500.0),
        (-100.0, 500.0),
        (300.0, 500.0),
        (900.0, 500.0),
    ] {
        let config = DriftControlConfig::new(max_ppm, 0.05, 1.0, 0.5, 200.0).unwrap();
        let mut controller = DriftController::new(config, rate(48_000), 960.0);
        let mut fill = 960.0;
        let mut previous_ppb = 0;
        let mut max_step_ppm = 0.0_f64;
        for _ in 0..360_000 {
            let dt = 0.01;
            let ppb = controller.update(fill, dt);
            max_step_ppm = max_step_ppm.max((ppb - previous_ppb).abs() as f64 / 1_000.0);
            previous_ppb = ppb;
            fill += (drift_ppm - ppb as f64 / 1_000.0) * 1e-6 * 48_000.0 * dt;
        }
        assert!(controller.applied_correction_ppm().abs() <= max_ppm + 1e-9);
        assert!(max_step_ppm <= 2.0 + 1e-6, "slew {max_step_ppm}");
        if drift_ppm.abs() < max_ppm {
            assert!((controller.estimated_drift_ppm() - drift_ppm).abs() < 0.01);
            assert!((fill - 960.0).abs() < 0.5, "{fill}");
        } else {
            assert_eq!(controller.applied_correction_ppm(), max_ppm);
        }
    }
}

fn primed(input_hz: u32, policy: DiscontinuityPolicy) -> (SourceSync, u64) {
    let mut sync = SourceSync::new(
        mono(input_hz)
            .with_drift_control(DriftControlConfig::disabled())
            .with_discontinuity_policy(policy),
    )
    .unwrap();
    let block: Vec<f32> = (0..2_000)
        .map(|i| tone(i, 1_000.0, input_hz, 0.5))
        .collect();
    // Prime with exactly the target, start playback, then add slack so the
    // tests below can pull several blocks without underrunning.
    let target = sync.config().target_fill_samples();
    sync.push(0, &[&block[..target]]).unwrap();
    let mut out = [0.0_f32; PULL];
    sync.pull(&mut [&mut out]).unwrap();
    assert_eq!(sync.state(), SourceSyncState::Running);
    sync.push(target as u64, &[&block[target..]]).unwrap();
    (sync, 2_000)
}

fn max_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

fn pull_all(sync: &mut SourceSync, count: usize) -> Vec<f32> {
    let mut collected = Vec::new();
    let mut out = [0.0_f32; PULL];
    for _ in 0..count {
        sync.pull(&mut [&mut out]).unwrap();
        collected.extend_from_slice(&out);
    }
    collected
}

#[test]
fn small_gaps_are_concealed_with_silence_on_the_source_timeline() {
    let policy = DiscontinuityPolicy::for_rate(rate(48_000));
    let (mut sync, next) = primed(48_000, policy);
    let block: Vec<f32> = (0..1_000)
        .map(|i| tone(next + 300 + i, 1_000.0, 48_000, 0.5))
        .collect();
    let before = sync.buffer_fill();
    let report = sync.push(next + 300, &[&block]).unwrap();
    assert_eq!(
        report.continuity,
        PushContinuity::GapFilled { samples: 300 }
    );
    assert!((sync.buffer_fill() - before - 1_300.0).abs() < 1e-9);
    // The next contiguous block follows the concealed timeline.
    let report = sync.push(next + 1_300, &[&block[..10]]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Contiguous);
    let output = pull_all(&mut sync, 3);
    assert!(max_step(&output) < 0.1, "{}", max_step(&output));
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.gaps_filled, 1);
    assert_eq!(telemetry.concealed_gap_samples, 300);
    assert_eq!(telemetry.discontinuities, 0);
    assert_eq!(telemetry.underruns, 0);
}

#[test]
fn small_overlaps_trim_duplicate_samples() {
    let (mut sync, next) = primed(48_000, DiscontinuityPolicy::for_rate(rate(48_000)));
    let block: Vec<f32> = (0..500)
        .map(|i| tone(next - 200 + i, 1_000.0, 48_000, 0.5))
        .collect();
    let report = sync.push(next - 200, &[&block]).unwrap();
    assert_eq!(
        report.continuity,
        PushContinuity::OverlapTrimmed { samples: 200 }
    );
    assert_eq!(report.accepted_samples, 300);
    // A fully duplicated block is ignored.
    let report = sync.push(next, &[&block[..100]]).unwrap();
    assert_eq!(report.accepted_samples, 0);
    let report = sync.push(next + 300, &[&block[..10]]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Contiguous);
    let output = pull_all(&mut sync, 3);
    assert!(max_step(&output) < 0.07, "{}", max_step(&output));
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.overlaps_trimmed, 2);
    assert_eq!(telemetry.trimmed_overlap_samples, 300);
    assert_eq!(telemetry.pushed_samples, 2_000 + 300 + 10);
    assert_eq!(sync.telemetry().underruns, 0);
}

#[test]
fn large_jumps_splice_without_clicks_and_keep_latency() {
    let (mut sync, _) = primed(48_000, DiscontinuityPolicy::for_rate(rate(48_000)));
    let before = sync.buffer_fill();
    // Jump forward a minute with a phase-inverted tone: a hard cut would step by ~1.0.
    let block: Vec<f32> = (0..480)
        .map(|i| -tone(i + 17, 1_000.0, 48_000, 0.5))
        .collect();
    let report = sync.push(60 * 48_000, &[&block]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Spliced);
    assert!((sync.buffer_fill() - before - 480.0).abs() < 1e-9);
    let report = sync.push(60 * 48_000 + 480, &[&block[..10]]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Contiguous);
    // Backward jumps beyond the overlap tolerance also splice.
    let report = sync.push(5, &[&block]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Spliced);
    let output = pull_all(&mut sync, 4);
    assert!(max_step(&output) < 0.1, "{}", max_step(&output));
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.discontinuities, 2);
    assert_eq!(telemetry.splices, 2);
    assert_eq!(telemetry.resets, 0);
    assert_eq!(sync.telemetry().underruns, 0);
}

#[test]
fn flush_policy_resets_latency_to_the_target() {
    let policy = DiscontinuityPolicy::new(48, 48, JumpAction::Flush, 48).unwrap();
    let (mut sync, next) = primed(48_000, policy);
    let block: Vec<f32> = (0..200).map(|i| tone(i, 1_000.0, 48_000, 0.5)).collect();
    // A flagged block is a discontinuity even if its index is contiguous.
    let report = sync.push_discontinuity(next, &[&block]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Flushed);
    let target = sync.config().target_fill_samples() as f64;
    assert!(
        (sync.buffer_fill() - target).abs() < 1.0,
        "{}",
        sync.buffer_fill()
    );
    let report = sync.push(next + 10_000, &[&block]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Flushed);
    let output = pull_all(&mut sync, 1);
    assert!(max_step(&output) < 0.1, "{}", max_step(&output));
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.discontinuities, 2);
    assert_eq!(telemetry.resets, 2);
    assert_eq!(telemetry.underruns, 0);
}

#[test]
fn underruns_fade_out_then_reprime_and_fade_in() {
    let (mut sync, next) = primed(48_000, DiscontinuityPolicy::for_rate(rate(48_000)));
    // 2000 pushed, 480 consumed: the next pulls drain the buffer.
    let mut out = [0.0_f32; PULL];
    let mut underrun_block = None;
    for index in 0..4 {
        let report = sync.pull(&mut [&mut out]).unwrap();
        if report.underrun {
            underrun_block = Some((index, report, out));
            break;
        }
    }
    let (_, report, block) = underrun_block.expect("buffer must underrun");
    assert!(report.rendered_samples > 0 && report.silent_samples > 0);
    let last = report.rendered_samples - 1;
    assert!(
        block[last].abs() < 0.5 / 48.0 + 1e-6,
        "fade-out tail {}",
        block[last]
    );
    assert!(block[report.rendered_samples..].iter().all(|s| *s == 0.0));
    assert_eq!(sync.state(), SourceSyncState::Recovering);

    // Silent while below target.
    let tail: Vec<f32> = (0..400)
        .map(|i| tone(next + i, 1_000.0, 48_000, 0.5))
        .collect();
    sync.push(next, &[&tail]).unwrap();
    let report = sync.pull(&mut [&mut out]).unwrap();
    assert_eq!(report.silent_samples, PULL);
    assert!(out.iter().all(|s| *s == 0.0));

    // Refill past the target: resumes with a fade-in from silence.
    let more: Vec<f32> = (0..1_000)
        .map(|i| tone(next + 400 + i, 1_000.0, 48_000, 0.5))
        .collect();
    sync.push(next + 400, &[&more]).unwrap();
    let report = sync.pull(&mut [&mut out]).unwrap();
    assert_eq!(report.rendered_samples, PULL);
    assert_eq!(sync.state(), SourceSyncState::Running);
    assert!(out[0].abs() < 0.5 / 48.0 + 1e-6);
    assert!(max_step(&out) < 0.07);
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.underruns, 1);
    // Every silent sample here followed the fault (priming was never silent).
    assert_eq!(telemetry.underrun_samples, telemetry.silent_output_samples);
}

#[test]
fn overruns_drop_back_to_the_target_fill() {
    let (mut sync, next) = primed(48_000, DiscontinuityPolicy::for_rate(rate(48_000)));
    let capacity = sync.config().capacity_samples();
    let target = sync.config().target_fill_samples() as f64;
    let mut index = next;
    let block: Vec<f32> = (0..1_000).map(|i| tone(i, 1_000.0, 48_000, 0.5)).collect();
    let mut dropped = 0;
    while dropped == 0 {
        let report = sync.push(index, &[&block]).unwrap();
        index += 1_000;
        dropped = report.overrun_dropped_samples;
    }
    assert!(
        (sync.buffer_fill() - target).abs() < 1.0,
        "{}",
        sync.buffer_fill()
    );
    let telemetry = sync.telemetry();
    assert_eq!(telemetry.overruns, 1);
    assert_eq!(telemetry.overrun_dropped_samples, dropped as u64);
    assert!(telemetry.peak_fill_samples <= capacity);
    let output = pull_all(&mut sync, 1);
    assert!(max_step(&output) < 0.1, "{}", max_step(&output));
    assert_eq!(sync.telemetry().underruns, 0);
}

#[test]
fn conversion_is_deterministic_and_channels_are_independent() {
    let run = || {
        let mut sync = SourceSync::new(SourceSyncConfig::new(rate(44_100), 2)).unwrap();
        let left: Vec<f32> = (0..441).map(|i| tone(i, 1_000.0, 44_100, 0.5)).collect();
        let mut outputs = Vec::new();
        let mut l = [0.0_f32; PULL];
        let mut r = [0.0_f32; PULL];
        let mut index = 0_u64;
        for tick in 0..300_u64 {
            // +37 ppm-ish irregular delivery exercises the controller.
            let count = if tick % 27 == 0 { 442 } else { 441 };
            let block: Vec<f32> = (0..count).map(|i| left[i % 441]).collect();
            let silent = vec![0.0_f32; count];
            sync.push(index, &[&block, &silent]).unwrap();
            index += count as u64;
            sync.pull(&mut [&mut l, &mut r]).unwrap();
            assert!(r.iter().all(|s| *s == 0.0));
            outputs.extend(l.iter().map(|s| s.to_bits()));
        }
        (outputs, sync.telemetry())
    };
    let (first, first_telemetry) = run();
    let (second, second_telemetry) = run();
    assert_eq!(first, second);
    assert_eq!(first_telemetry, second_telemetry);
}

#[test]
fn configuration_and_io_shapes_are_validated() {
    assert_eq!(
        SincKernel::new(rate(4_000), ENGINE_SAMPLE_RATE).unwrap_err(),
        ResamplerError::InputRateOutOfRange(4_000)
    );
    assert_eq!(
        SincKernel::new(rate(192_000), rate(44_100)).unwrap_err(),
        ResamplerError::DecimationTooLarge {
            input_hz: 192_000,
            output_hz: 44_100
        }
    );
    assert_eq!(
        SourceSync::new(SourceSyncConfig::new(rate(48_000), 0)).unwrap_err(),
        SourceSyncError::ChannelCountOutOfRange(0)
    );
    assert_eq!(
        SourceSync::new(mono(48_000).with_target_fill_samples(10)).unwrap_err(),
        SourceSyncError::TargetFillTooSmall {
            target: 10,
            minimum: 37
        }
    );
    assert_eq!(
        SourceSync::new(mono(48_000).with_capacity_samples(1_000)).unwrap_err(),
        SourceSyncError::CapacityOutOfRange {
            capacity: 1_000,
            minimum: 1_920
        }
    );
    assert_eq!(
        DriftControlConfig::new(10_000.0, 0.05, 1.0, 0.5, 50.0).unwrap_err(),
        SourceSyncError::InvalidDriftControl
    );
    assert_eq!(
        DriftControlConfig::new(100.0, f64::NAN, 1.0, 0.5, 50.0).unwrap_err(),
        SourceSyncError::InvalidDriftControl
    );
    assert_eq!(
        DiscontinuityPolicy::new(0, 0, JumpAction::Splice, 10_000).unwrap_err(),
        SourceSyncError::DeclickTooLong(10_000)
    );

    let mut sync = SourceSync::new(mono(48_000)).unwrap();
    let samples = [0.0_f32, f32::NAN];
    assert_eq!(
        sync.push(0, &[&samples]).unwrap_err(),
        SourceSyncError::NonFiniteSample {
            channel: 0,
            sample: 1
        }
    );
    assert_eq!(
        sync.push(0, &[&samples, &samples]).unwrap_err(),
        SourceSyncError::PlaneCountMismatch {
            expected: 1,
            actual: 2
        }
    );
    assert_eq!(
        sync.push(u64::MAX, &[&[0.0_f32][..]]).unwrap_err(),
        SourceSyncError::TimestampOverflow
    );
    let mut big = vec![0.0_f32; 48_001];
    assert_eq!(
        sync.pull(&mut [&mut big]).unwrap_err(),
        SourceSyncError::PullTooLarge(48_001)
    );
    // Errors leave state untouched.
    assert_eq!(sync.telemetry().pushed_samples, 0);
    assert_eq!(sync.state(), SourceSyncState::Priming);
}

#[test]
fn explicit_reset_reprimes_and_keeps_the_drift_estimate() {
    let sync = SourceSync::new(mono(48_000)).unwrap();
    let mut sim = Sim::new(sync, 100, 1_000.0);
    let mut block = [0.0_f32; PULL];
    for _ in 0..3_000 {
        sim.tick(&mut block);
    }
    let estimate = sim.sync.telemetry().estimated_drift_ppm;
    assert!(estimate > 50.0, "{estimate}");
    sim.sync.reset();
    let telemetry = sim.sync.telemetry();
    assert_eq!(telemetry.state, SourceSyncState::Priming);
    assert_eq!(telemetry.resets, 1);
    assert_eq!(telemetry.buffer_fill_samples, 0.0);
    assert_eq!(telemetry.estimated_drift_ppm, estimate);
    // The next push re-anchors at any index.
    let samples = vec![0.0_f32; 1_000];
    let report = sim.sync.push(123_456_789, &[&samples]).unwrap();
    assert_eq!(report.continuity, PushContinuity::Anchored);
    sim.sync.reset_drift_estimate();
    assert_eq!(sim.sync.telemetry().estimated_drift_ppm, 0.0);
}

#[test]
fn oversized_target_fill_is_rejected_without_overflow() {
    assert_eq!(
        SourceSync::new(mono(48_000).with_target_fill_samples(usize::MAX)).unwrap_err(),
        SourceSyncError::CapacityOutOfRange {
            capacity: 9_600,
            minimum: usize::MAX
        }
    );
}

#[test]
fn infinite_elapsed_time_does_not_poison_the_drift_controller() {
    let mut controller = DriftController::new(DriftControlConfig::default(), rate(48_000), 960.0);
    controller.update(1_000.0, 0.01);
    let before = controller.applied_correction_ppm();
    controller.update(960.0, f64::INFINITY);
    assert!(controller.estimated_drift_ppm().is_finite());
    assert_eq!(controller.applied_correction_ppm(), before);
    for _ in 0..1_000 {
        controller.update(1_200.0, 0.01);
    }
    assert!(controller.applied_correction_ppm() > 0.0);
    assert!(controller.estimated_drift_ppm() > 0.0);
}

#[test]
fn huge_concealable_gap_drops_back_to_the_target_without_overflow() {
    let policy = DiscontinuityPolicy::new(usize::MAX, 0, JumpAction::Splice, 48).unwrap();
    let mut sync = SourceSync::new(
        mono(48_000)
            .with_drift_control(DriftControlConfig::disabled())
            .with_discontinuity_policy(policy),
    )
    .unwrap();
    let block = vec![0.25_f32; 1_000];
    sync.push(0, &[&block]).unwrap();
    // A flagged discontinuity splices after the buffered audio, so the fill
    // now exceeds the span of the source timeline.
    sync.push_discontinuity(0, &[&block]).unwrap();
    let tail = [0.25_f32; 10];
    let first = u64::MAX - 10;
    let report = sync.push(first, &[&tail]).unwrap();
    assert_eq!(
        report.continuity,
        PushContinuity::GapFilled {
            samples: first - 1_000
        }
    );
    assert_eq!(report.overrun_dropped_samples, usize::MAX);
    assert_eq!(report.accepted_samples, 10);
    let target = sync.config().target_fill_samples();
    assert_eq!(sync.buffer_fill(), target as f64);
    let mut out = [0.0_f32; PULL];
    let pulled = sync.pull(&mut [&mut out]).unwrap();
    assert_eq!(pulled.rendered_samples, PULL);
}
