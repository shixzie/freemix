//! AU-009: zipper-free fader gain. Control-rate targets become per-sample
//! linear ramps with a bounded step, bounded duration, and exact arrival.

use fm_audio::{
    Gain, GainSmoother, GainSmootherConfig, GainSmootherError, MAX_CHANNELS,
    MAX_GAIN_SMOOTHER_RAMP_SAMPLES, MAX_SAMPLES_PER_BLOCK,
};

/// Floating-point slack on the analytic step bound (f64 ramp, f32 output).
const STEP_SLACK: f32 = 1.0e-6;

fn gain(linear: f32) -> Gain {
    Gain::from_linear(linear).unwrap()
}

fn db(value: f32) -> Gain {
    Gain::from_db(value).unwrap()
}

fn config(max_step: f32, min: u32, max: u32) -> GainSmootherConfig {
    GainSmootherConfig {
        max_step,
        min_ramp_samples: min,
        max_ramp_samples: max,
    }
}

fn run(smoother: &mut GainSmoother, samples: usize) -> Vec<f32> {
    let mut gains = vec![0.0; samples];
    smoother.fill(&mut gains).unwrap();
    gains
}

fn max_step(previous: f32, gains: &[f32]) -> f32 {
    let mut last = previous;
    let mut largest = 0.0_f32;
    for &gain in gains {
        largest = largest.max((gain - last).abs());
        last = gain;
    }
    largest
}

/// Deterministic xorshift so the jitter is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[test]
fn unit_ramp_arrives_exactly_with_uniform_steps() {
    let mut smoother = GainSmoother::new(config(1.0 / 480.0, 1, 4_800), Gain::SILENCE).unwrap();
    assert_eq!(smoother.set_target(Gain::UNITY), 480);
    let gains = run(&mut smoother, 600);
    // The first sample is one step from the start, the 480th is exactly the
    // target, and the ramp then holds bit-exactly.
    assert!((gains[0] - 1.0 / 480.0).abs() < 1.0e-7);
    assert!(gains[478] < 1.0);
    assert_eq!(gains[479].to_bits(), 1.0_f32.to_bits());
    assert!(
        gains[479..]
            .iter()
            .all(|gain| gain.to_bits() == 1.0_f32.to_bits())
    );
    assert!(gains.windows(2).all(|pair| pair[1] >= pair[0]));
    assert!(max_step(0.0, &gains) <= 1.0 / 480.0 + STEP_SLACK);
    assert!(smoother.is_settled());
    assert_eq!(smoother.remaining(), 0);
}

#[test]
fn high_rate_fader_stream_has_bounded_steps_and_settles_exactly() {
    let cfg = GainSmootherConfig::default();
    let mut smoother = GainSmoother::new(cfg, Gain::UNITY).unwrap();
    let bound = smoother.max_step_bound();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut previous = smoother.current();
    let mut worst = 0.0_f32;
    let mut naive_worst = 0.0_f32;
    let mut naive_previous = previous;
    // 2 000 coalesced control ticks of jittered size (16..=1024 samples),
    // each carrying a fader position anywhere from -inf to +24 dB, delivered
    // in blocks of arbitrary size.
    let mut final_target = Gain::UNITY;
    for _ in 0..2_000 {
        let target = if rng.below(10) == 0 {
            Gain::SILENCE
        } else {
            #[allow(clippy::cast_precision_loss)]
            let value = -60.0 + rng.below(8_401) as f32 / 100.0;
            db(value)
        };
        final_target = target;
        smoother.set_target(target);
        naive_worst = naive_worst.max((target.linear() - naive_previous).abs());
        naive_previous = target.linear();
        let mut left = 16 + usize::try_from(rng.below(1_009)).unwrap();
        while left > 0 {
            let block = left.min(1 + usize::try_from(rng.below(256)).unwrap());
            let gains = run(&mut smoother, block);
            worst = worst.max(max_step(previous, &gains));
            previous = *gains.last().unwrap();
            left -= block;
        }
    }
    assert!(worst <= bound + STEP_SLACK, "worst {worst} > bound {bound}");
    // Applying the same targets as block steps would jump by up to ~15.8.
    assert!(naive_worst > 100.0 * bound);
    // Settling time is bounded by max_ramp_samples and lands exactly.
    let tail = run(&mut smoother, cfg.max_ramp_samples as usize);
    assert_eq!(
        tail.last().unwrap().to_bits(),
        final_target.linear().to_bits()
    );
    assert!(smoother.is_settled());
    let telemetry = smoother.telemetry();
    assert!(telemetry.retargets > 1_500);
    assert!(telemetry.clamped_ramps > 0);
}

#[test]
fn block_split_and_skip_match_per_sample_processing_bit_exactly() {
    let cfg = config(1.0 / 1_000.0, 32, 4_000);
    let mut whole = GainSmoother::new(cfg, db(-20.0)).unwrap();
    let mut split = whole.clone();
    let mut skipped = whole.clone();
    for smoother in [&mut whole, &mut split, &mut skipped] {
        smoother.set_target(db(6.0));
    }
    let reference = run(&mut whole, 2_000);
    let mut pieces = Vec::new();
    for block in [1, 7, 100, 372, 1, 1_519] {
        pieces.extend(run(&mut split, block));
    }
    assert_eq!(
        reference
            .iter()
            .map(|gain| gain.to_bits())
            .collect::<Vec<_>>(),
        pieces.iter().map(|gain| gain.to_bits()).collect::<Vec<_>>()
    );
    // An inaudible strip skips ahead and resumes exactly in step.
    skipped.skip(1_234);
    assert_eq!(skipped.current().to_bits(), reference[1_233].to_bits());
    let resumed = run(&mut skipped, 766);
    assert_eq!(
        resumed
            .iter()
            .map(|gain| gain.to_bits())
            .collect::<Vec<_>>(),
        reference[1_234..]
            .iter()
            .map(|gain| gain.to_bits())
            .collect::<Vec<_>>()
    );
}

#[test]
fn retargeting_mid_ramp_reverses_without_a_discontinuity() {
    let mut smoother = GainSmoother::new(config(1.0 / 480.0, 1, 4_800), Gain::SILENCE).unwrap();
    smoother.set_target(Gain::UNITY);
    let rising = run(&mut smoother, 240);
    let midpoint = smoother.current();
    assert!((midpoint - 0.5).abs() < 1.0e-6);
    // The fader reverses: the new ramp starts from wherever we are.
    assert_eq!(smoother.set_target(Gain::SILENCE), 240);
    let falling = run(&mut smoother, 300);
    assert!(max_step(*rising.last().unwrap(), &falling) <= 1.0 / 480.0 + STEP_SLACK);
    assert!(falling[0] < midpoint);
    assert_eq!(falling[239].to_bits(), 0.0_f32.to_bits());
    // Re-sending the pending target (a coalesced duplicate) does not restart
    // or lengthen the ramp.
    smoother.set_target(Gain::UNITY);
    run(&mut smoother, 100);
    assert_eq!(smoother.set_target(Gain::UNITY), 380);
    assert_eq!(smoother.telemetry().retargets, 3);
}

#[test]
fn ramp_duration_is_bounded_even_for_the_largest_move() {
    let cfg = config(1.0 / 480.0, 48, 2_400);
    let mut smoother = GainSmoother::new(cfg, Gain::SILENCE).unwrap();
    let loudest = db(24.0);
    // At max_step this move would need ~7 600 samples; it is stretched to
    // exactly max_ramp_samples instead, with a step within the bound.
    assert_eq!(smoother.set_target(loudest), 2_400);
    let gains = run(&mut smoother, 2_400);
    assert_eq!(gains[2_399].to_bits(), loudest.linear().to_bits());
    let bound = smoother.max_step_bound();
    assert!((bound - loudest.linear() / 2_400.0).abs() < 1.0e-6);
    assert!(max_step(0.0, &gains) <= bound + STEP_SLACK);
    assert_eq!(smoother.telemetry().clamped_ramps, 1);
    // A tiny change still ramps over at least min_ramp_samples.
    let nudge = gain(loudest.linear() - 0.0001);
    assert_eq!(smoother.set_target(nudge), 48);
}

#[test]
fn planar_apply_shares_one_step_per_frame_and_validates_first() {
    let mut smoother = GainSmoother::new(config(0.25, 1, 64), Gain::UNITY).unwrap();
    smoother.set_target(Gain::SILENCE);
    let mut left = vec![1.0_f32; 6];
    let mut right = vec![-0.5_f32; 6];
    smoother.apply(&mut [&mut left, &mut right]).unwrap();
    assert_eq!(left, [0.75, 0.5, 0.25, 0.0, 0.0, 0.0]);
    assert_eq!(right, [-0.375, -0.25, -0.125, 0.0, 0.0, 0.0]);

    smoother.set_target(Gain::UNITY);
    let before = (smoother.current(), smoother.remaining());
    let mut short = vec![1.0_f32; 3];
    let mut long = vec![1.0_f32; 4];
    assert_eq!(
        smoother.apply(&mut [&mut long, &mut short]),
        Err(GainSmootherError::PlaneLengthMismatch {
            plane: 1,
            expected: 4,
            actual: 3
        })
    );
    assert_eq!(long, [1.0; 4]);
    assert_eq!(
        smoother.apply(&mut []),
        Err(GainSmootherError::ChannelCountOutOfRange(0))
    );
    let mut planes: Vec<Vec<f32>> = vec![vec![0.0; 1]; MAX_CHANNELS + 1];
    let mut refs: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
    assert_eq!(
        smoother.apply(&mut refs),
        Err(GainSmootherError::ChannelCountOutOfRange(MAX_CHANNELS + 1))
    );
    let mut huge = vec![0.0_f32; MAX_SAMPLES_PER_BLOCK + 1];
    assert_eq!(
        smoother.fill(&mut huge),
        Err(GainSmootherError::SampleCountOutOfRange(
            MAX_SAMPLES_PER_BLOCK + 1
        ))
    );
    assert_eq!((smoother.current(), smoother.remaining()), before);
}

#[test]
fn configuration_is_validated_and_jump_is_explicit() {
    for step in [0.0, -0.1, 1.5, f32::NAN, f32::INFINITY] {
        assert!(matches!(
            GainSmoother::new(config(step, 1, 10), Gain::UNITY),
            Err(GainSmootherError::InvalidMaxStep(_))
        ));
    }
    for (min, max) in [(0, 10), (11, 10), (1, MAX_GAIN_SMOOTHER_RAMP_SAMPLES + 1)] {
        assert_eq!(
            GainSmoother::new(config(0.1, min, max), Gain::UNITY).unwrap_err(),
            GainSmootherError::InvalidRampBounds {
                minimum: min,
                maximum: max
            }
        );
    }
    let mut smoother = GainSmoother::new(GainSmootherConfig::default(), Gain::UNITY).unwrap();
    smoother.set_target(Gain::SILENCE);
    run(&mut smoother, 10);
    smoother.jump_to(db(-6.0));
    assert_eq!(smoother.current(), db(-6.0).linear());
    assert!(smoother.is_settled());
    assert_eq!(smoother.telemetry().jumps, 1);
    // Targeting the current value is a no-op.
    assert_eq!(smoother.set_target(db(-6.0)), 0);
}
