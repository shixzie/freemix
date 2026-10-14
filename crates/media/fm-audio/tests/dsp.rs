#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]

use fm_audio::{
    AudioBlock, Biquad, BiquadCoefficients, BiquadKind, Compressor, DspError, DspMeter,
    DspMeterReading, Gate, Limiter, MAX_CHANNELS, MAX_EQ_BANDS, ParametricEq, Polarity, StripDsp,
};
use fm_types::{AudioFormat, Channel, ChannelLayout, SampleFormat, SampleRate};

const SAMPLE_RATE_HZ: f32 = 48_000.0;
const SAMPLE_RATE: u32 = 48_000;
const Q_BUTTERWORTH: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn stereo_format() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(SAMPLE_RATE).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::stereo(),
    }
}

fn mono_format() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(SAMPLE_RATE).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::new(vec![Channel::Mono]).unwrap(),
    }
}

fn stereo_block(planes: Vec<Vec<f32>>) -> AudioBlock {
    AudioBlock::from_planar(stereo_format(), planes).unwrap()
}

fn mono_block(plane: Vec<f32>) -> AudioBlock {
    AudioBlock::from_planar(mono_format(), vec![plane]).unwrap()
}

fn sine_plane(frequency_hz: f32, amplitude: f32, samples: usize) -> Vec<f32> {
    let increment = std::f64::consts::TAU * f64::from(frequency_hz) / f64::from(SAMPLE_RATE_HZ);
    (0..samples)
        .map(|index| (increment * index as f64).sin() as f32 * amplitude)
        .collect()
}

fn process_slice(filter: &mut Biquad, input: &[f32]) -> Vec<f32> {
    input.iter().map(|sample| filter.process(*sample)).collect()
}

fn rms(samples: &[f32]) -> f32 {
    let squares: f32 = samples.iter().map(|sample| sample * sample).sum();
    (squares / samples.len() as f32).sqrt()
}

fn measured_gain(filter: &mut Biquad, frequency_hz: f32, amplitude: f32) -> f32 {
    let input = sine_plane(frequency_hz, amplitude, 4_800);
    let output = process_slice(filter, &input);
    rms(&output[3_800..]) / rms(&input[3_800..])
}

fn assert_close(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}, got {actual} (tolerance {tolerance})"
    );
}

fn assert_bits_equal(actual: &AudioBlock, expected: &AudioBlock) {
    assert_eq!(actual.samples(), expected.samples());
    for (actual_plane, expected_plane) in actual.planes().iter().zip(expected.planes()) {
        for (actual_sample, expected_sample) in actual_plane.iter().zip(expected_plane) {
            assert_eq!(
                actual_sample.to_bits(),
                expected_sample.to_bits(),
                "sample bits differ: {actual_sample} != {expected_sample}"
            );
        }
    }
}

fn configured_strip() -> StripDsp {
    let mut strip = StripDsp::new(SAMPLE_RATE_HZ, 2).unwrap();
    strip.set_polarity(Polarity::Inverted);
    strip.set_high_pass(120.0, Q_BUTTERWORTH).unwrap();
    strip.set_eq_band(0, 1_000.0, 1.0, 4.0).unwrap();
    strip.set_eq_band(1, 250.0, 0.8, -3.0).unwrap();
    strip.set_gate(-45.0, 30.0, 1.0, 10.0, 50.0).unwrap();
    strip
        .set_compressor(-12.0, 4.0, 5.0, 80.0, 6.0, 3.0)
        .unwrap();
    strip.set_limiter(-1.0, 50.0).unwrap();
    strip
}

fn chain_input_block() -> AudioBlock {
    stereo_block(vec![
        sine_plane(440.0, 0.9, 4_096),
        sine_plane(660.0, 0.4, 4_096),
    ])
}

#[test]
fn polarity_negates_exactly() {
    assert_eq!(Polarity::Normal.apply(0.25), 0.25);
    assert_eq!(Polarity::Inverted.apply(0.25), -0.25);
    assert_eq!(Polarity::Inverted.apply(-0.25), 0.25);
    assert!(Polarity::Inverted.apply(0.0).is_sign_negative());
    assert!(Polarity::Inverted.apply(-0.0).is_sign_positive());
}

#[test]
fn high_pass_blocks_dc_and_passes_high_frequencies() {
    let coefficients = BiquadCoefficients::high_pass(SAMPLE_RATE_HZ, 100.0, Q_BUTTERWORTH).unwrap();
    let mut filter = Biquad::new(coefficients);
    let mut output = 0.0_f32;
    for _ in 0..20_000 {
        output = filter.process(1.0);
    }
    assert!(output.abs() < 1e-3, "high-pass DC output is {output}");

    let mut filter = Biquad::new(coefficients);
    let input = sine_plane(10_000.0, 0.5, 4_800);
    let output = process_slice(&mut filter, &input);
    let ratio = rms(&output[3_800..]) / rms(&input[3_800..]);
    assert_close(ratio, 1.0, 1e-3);
}

#[test]
fn peaking_eq_applies_center_gain() {
    let coefficients = BiquadCoefficients::peaking_eq(SAMPLE_RATE_HZ, 1_000.0, 1.0, 6.0).unwrap();
    let mut filter = Biquad::new(coefficients);
    let input = sine_plane(1_000.0, 0.25, 4_800);
    let output = process_slice(&mut filter, &input);
    let ratio = rms(&output[3_800..]) / rms(&input[3_800..]);
    assert_close(ratio, 10.0_f32.powf(6.0 / 20.0), 2e-3);
}

#[test]
fn shelves_apply_expected_low_and_high_gain() {
    let low_shelf =
        BiquadCoefficients::low_shelf(SAMPLE_RATE_HZ, 1_000.0, Q_BUTTERWORTH, 6.0).unwrap();
    let boosted_low = measured_gain(&mut Biquad::new(low_shelf), 50.0, 0.25);
    let flat_high = measured_gain(&mut Biquad::new(low_shelf), 10_000.0, 0.25);
    assert_close(boosted_low, 10.0_f32.powf(6.0 / 20.0), 0.01);
    assert_close(flat_high, 1.0, 0.01);

    let high_shelf =
        BiquadCoefficients::high_shelf(SAMPLE_RATE_HZ, 1_000.0, Q_BUTTERWORTH, 6.0).unwrap();
    let boosted_high = measured_gain(&mut Biquad::new(high_shelf), 10_000.0, 0.25);
    let flat_low = measured_gain(&mut Biquad::new(high_shelf), 50.0, 0.25);
    assert_close(boosted_high, 10.0_f32.powf(6.0 / 20.0), 0.01);
    assert_close(flat_low, 1.0, 0.01);
}

#[test]
fn biquad_reset_and_coefficient_changes_clear_history() {
    let coefficients =
        BiquadCoefficients::high_pass(SAMPLE_RATE_HZ, 1_000.0, Q_BUTTERWORTH).unwrap();
    let mut primed = Biquad::new(coefficients);
    for _ in 0..256 {
        let _ = primed.process(1.0);
    }
    let input = sine_plane(2_000.0, 0.5, 1_024);
    let primed_then_reset = {
        primed.reset();
        process_slice(&mut primed, &input)
    };
    let mut fresh = Biquad::new(coefficients);
    assert_eq!(primed_then_reset, process_slice(&mut fresh, &input));
    assert_eq!(primed.coefficients(), coefficients);

    let mut primed = Biquad::new(coefficients);
    for _ in 0..256 {
        let _ = primed.process(1.0);
    }
    primed.set_coefficients(BiquadCoefficients::identity());
    assert_eq!(primed.coefficients(), BiquadCoefficients::identity());
    assert_eq!(primed.process(0.0), 0.0);

    let identity = BiquadCoefficients::identity();
    assert_eq!(identity.b0(), 1.0);
    assert_eq!(identity.b1(), 0.0);
    assert_eq!(identity.b2(), 0.0);
    assert_eq!(identity.a1(), 0.0);
    assert_eq!(identity.a2(), 0.0);
}

#[test]
fn parametric_eq_composes_high_pass_and_bands() {
    let mut eq = ParametricEq::new(SAMPLE_RATE_HZ).unwrap();
    assert_eq!(eq.sample_rate_hz(), SAMPLE_RATE_HZ);
    assert!(eq.high_pass().is_none());
    assert!(eq.band(0).is_none());

    eq.set_high_pass(100.0, Q_BUTTERWORTH).unwrap();
    eq.set_band(0, 1_000.0, 1.0, 6.0).unwrap();
    let high_pass = eq.high_pass().unwrap();
    assert_eq!(high_pass.kind, BiquadKind::HighPass);
    assert_eq!(high_pass.frequency_hz, 100.0);
    let band = eq.band(0).unwrap();
    assert_eq!(band.kind, BiquadKind::PeakingEq);
    assert_eq!(band.frequency_hz, 1_000.0);
    assert_eq!(band.q, 1.0);
    assert_eq!(band.gain_db, 6.0);

    let input = sine_plane(1_000.0, 0.25, 4_800);
    let output: Vec<f32> = input.iter().map(|sample| eq.process(*sample)).collect();
    let ratio = rms(&output[3_800..]) / rms(&input[3_800..]);
    assert_close(ratio, 10.0_f32.powf(6.0 / 20.0), 5e-3);

    eq.clear_band(0).unwrap();
    assert!(eq.band(0).is_none());
    eq.clear_high_pass();
    assert!(eq.high_pass().is_none());

    assert!(matches!(
        eq.set_band(MAX_EQ_BANDS, 1_000.0, 1.0, 0.0),
        Err(DspError::BandIndexOutOfRange { .. })
    ));
    assert!(matches!(
        eq.clear_band(MAX_EQ_BANDS),
        Err(DspError::BandIndexOutOfRange { .. })
    ));

    eq.reset();
    let impulse_output: Vec<f32> = (0..8).map(|_| eq.process(0.0)).collect();
    assert!(impulse_output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn biquad_coefficients_reject_invalid_parameters() {
    for rate in [0.0, -SAMPLE_RATE_HZ, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(
                BiquadCoefficients::high_pass(rate, 100.0, 1.0),
                Err(DspError::InvalidSampleRate(_))
            ),
            "rate {rate}"
        );
    }
    for frequency in [0.0, -1.0, SAMPLE_RATE_HZ / 2.0, SAMPLE_RATE_HZ, f32::NAN] {
        assert!(
            matches!(
                BiquadCoefficients::low_pass(SAMPLE_RATE_HZ, frequency, 1.0),
                Err(DspError::InvalidFrequency(_))
            ),
            "frequency {frequency}"
        );
    }
    for q in [0.0, -0.5, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(
                BiquadCoefficients::peaking_eq(SAMPLE_RATE_HZ, 1_000.0, q, 3.0),
                Err(DspError::InvalidQ(_))
            ),
            "q {q}"
        );
    }
    for gain in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0e30] {
        assert!(
            matches!(
                BiquadCoefficients::peaking_eq(SAMPLE_RATE_HZ, 1_000.0, 1.0, gain),
                Err(DspError::InvalidEqGainDb(_))
            ),
            "gain {gain}"
        );
    }
    assert!(BiquadCoefficients::high_pass(SAMPLE_RATE_HZ, 1.0, 0.01).is_ok());
    assert!(BiquadCoefficients::new(BiquadKind::LowPass, SAMPLE_RATE_HZ, 20.0, 0.5, 0.0).is_ok());
    assert!(BiquadCoefficients::peaking_eq(SAMPLE_RATE_HZ, 23_999.0, 10.0, 60.0).is_ok());
}

#[test]
fn compressor_static_curve_matches_ratio_and_threshold() {
    let mut compressor = Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 0.0, 0.0).unwrap();
    let mut output = 0.0;
    for _ in 0..4_800 {
        output = compressor.process(1.0);
    }
    assert_close(output, 10.0_f32.powf(-7.5 / 20.0), 1e-3);
    assert_close(compressor.current_gain_reduction_db(), 7.5, 1e-3);

    let mut compressor =
        Compressor::new(SAMPLE_RATE_HZ, -30.0, 20.0, 1.0, 100.0, 0.0, 0.0).unwrap();
    for _ in 0..4_800 {
        output = compressor.process(1.0);
    }
    assert_close(output, 10.0_f32.powf(-28.5 / 20.0), 1e-3);
    assert_close(compressor.current_gain_reduction_db(), 28.5, 1e-3);
}

#[test]
fn compressor_soft_knee_is_continuous_and_monotonic() {
    let threshold_db = -20.0_f32;
    let knee_db = 12.0_f32;
    let ratio = 4.0_f32;
    let mut previous_reduction = f32::NEG_INFINITY;
    for step in -120..=120 {
        let over_db = step as f32 * 0.1;
        let amplitude = 10.0_f32.powf((threshold_db + over_db) / 20.0);
        let mut compressor =
            Compressor::new(SAMPLE_RATE_HZ, threshold_db, ratio, 1.0, 10.0, knee_db, 0.0).unwrap();
        for _ in 0..3_000 {
            let _ = compressor.process(amplitude);
        }
        let reduction = compressor.current_gain_reduction_db();
        assert!(
            reduction >= previous_reduction - 1e-4,
            "reduction decreased at {over_db} dB: {previous_reduction} -> {reduction}"
        );
        assert!(
            (reduction - previous_reduction).abs() < 0.1 || previous_reduction.is_infinite(),
            "soft knee is discontinuous at {over_db} dB: {previous_reduction} -> {reduction}"
        );
        previous_reduction = reduction;
    }

    let amplitude = 10.0_f32.powf(threshold_db / 20.0);
    let mut compressor =
        Compressor::new(SAMPLE_RATE_HZ, threshold_db, ratio, 1.0, 10.0, knee_db, 0.0).unwrap();
    for _ in 0..3_000 {
        let _ = compressor.process(amplitude);
    }
    assert_close(compressor.current_gain_reduction_db(), 1.125, 1e-2);
}

#[test]
fn compressor_makeup_and_below_threshold_unity() {
    let mut compressor = Compressor::new(SAMPLE_RATE_HZ, -30.0, 4.0, 1.0, 100.0, 0.0, 0.0).unwrap();
    let mut output = 0.0;
    for _ in 0..1_000 {
        output = compressor.process(0.01);
    }
    assert_close(output, 0.01, 1e-6);
    assert_eq!(compressor.current_gain_reduction_db(), 0.0);

    let makeup_gain_db = 6.0_f32;
    let mut compressor =
        Compressor::new(SAMPLE_RATE_HZ, -30.0, 1.0, 1.0, 100.0, 0.0, makeup_gain_db).unwrap();
    for _ in 0..1_000 {
        output = compressor.process(0.5);
    }
    assert_close(output, 0.5 * 10.0_f32.powf(makeup_gain_db / 20.0), 1e-4);
}

#[test]
fn compressor_runs_are_deterministic_and_reset_restores_the_response() {
    let input = sine_plane(500.0, 1.0, 2_000);
    let mut first = Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 6.0, 0.0).unwrap();
    let mut second = Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 6.0, 0.0).unwrap();
    let first_output: Vec<f32> = input.iter().map(|sample| first.process(*sample)).collect();
    let second_output: Vec<f32> = input.iter().map(|sample| second.process(*sample)).collect();
    assert_eq!(first_output, second_output);

    first.reset();
    let replay: Vec<f32> = input.iter().map(|sample| first.process(*sample)).collect();
    assert_eq!(replay, first_output);
}

#[test]
fn compressor_rejects_invalid_parameters_and_keeps_setters_transactional() {
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -60.1, 4.0, 1.0, 100.0, 0.0, 0.0),
        Err(DspError::InvalidThresholdDb(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, 0.1, 4.0, 1.0, 100.0, 0.0, 0.0),
        Err(DspError::InvalidThresholdDb(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 0.5, 1.0, 100.0, 0.0, 0.0),
        Err(DspError::InvalidRatio(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 20.1, 1.0, 100.0, 0.0, 0.0),
        Err(DspError::InvalidRatio(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 0.05, 100.0, 0.0, 0.0),
        Err(DspError::InvalidAttackMs(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1_000.1, 100.0, 0.0, 0.0),
        Err(DspError::InvalidAttackMs(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 0.05, 0.0, 0.0),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 1_000.1, 0.0, 0.0),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, -0.1, 0.0),
        Err(DspError::InvalidKneeDb(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 24.1, 0.0),
        Err(DspError::InvalidKneeDb(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 0.0, -0.1),
        Err(DspError::InvalidMakeupGainDb(_))
    ));
    assert!(matches!(
        Compressor::new(SAMPLE_RATE_HZ, -10.0, 4.0, 1.0, 100.0, 0.0, 24.1),
        Err(DspError::InvalidMakeupGainDb(_))
    ));
    assert!(Compressor::new(SAMPLE_RATE_HZ, -60.0, 1.0, 0.1, 0.1, 0.0, 0.0).is_ok());
    assert!(Compressor::new(SAMPLE_RATE_HZ, 0.0, 20.0, 1_000.0, 1_000.0, 24.0, 24.0).is_ok());

    let mut compressor =
        Compressor::new(SAMPLE_RATE_HZ, -12.0, 4.0, 10.0, 100.0, 3.0, 6.0).unwrap();
    assert!(compressor.set_threshold_db(1.0).is_err());
    assert_eq!(compressor.threshold_db(), -12.0);
    assert!(compressor.set_ratio(0.5).is_err());
    assert_eq!(compressor.ratio(), 4.0);
    assert!(compressor.set_attack_ms(0.0).is_err());
    assert_eq!(compressor.attack_ms(), 10.0);
    assert!(compressor.set_release_ms(0.0).is_err());
    assert_eq!(compressor.release_ms(), 100.0);
    assert!(compressor.set_knee_db(25.0).is_err());
    assert_eq!(compressor.knee_db(), 3.0);
    assert!(compressor.set_makeup_gain_db(-1.0).is_err());
    assert_eq!(compressor.makeup_gain_db(), 6.0);
    assert_eq!(compressor.sample_rate_hz(), SAMPLE_RATE_HZ);
    compressor.set_threshold_db(-20.0).unwrap();
    compressor.set_ratio(8.0).unwrap();
    compressor.set_attack_ms(5.0).unwrap();
    compressor.set_release_ms(50.0).unwrap();
    compressor.set_knee_db(6.0).unwrap();
    compressor.set_makeup_gain_db(3.0).unwrap();
    assert_eq!(compressor.threshold_db(), -20.0);
    assert_eq!(compressor.ratio(), 8.0);
    assert_eq!(compressor.attack_ms(), 5.0);
    assert_eq!(compressor.release_ms(), 50.0);
    assert_eq!(compressor.knee_db(), 6.0);
    assert_eq!(compressor.makeup_gain_db(), 3.0);
}

#[test]
fn limiter_never_exceeds_ceiling_and_releases() {
    let mut limiter = Limiter::new(SAMPLE_RATE_HZ, -6.0, 5.0).unwrap();
    let ceiling = limiter.ceiling_linear();
    assert!(ceiling < 1.0);
    let burst = [4.0_f32, -8.0, 2.0, -0.5, 1.5, -3.0];
    let mut output = 0.0;
    for value in burst.iter().copied().cycle().take(4_800) {
        output = limiter.process(value);
        assert!(
            output.abs() <= ceiling,
            "limiter output {output} exceeds {ceiling}"
        );
    }
    assert!(limiter.current_gain_reduction_db() > 10.0);

    for _ in 0..4_800 {
        output = limiter.process(0.1);
    }
    assert_close(output, 0.1, 1e-4);
    assert_close(limiter.current_gain_reduction_db(), 0.0, 1e-3);

    limiter.reset();
    assert_eq!(limiter.current_gain_reduction_db(), 0.0);
    assert_eq!(limiter.process(8.0), ceiling);
}

#[test]
fn limiter_zero_ceiling_is_exactly_unity() {
    let mut limiter = Limiter::new(SAMPLE_RATE_HZ, 0.0, 1.0).unwrap();
    assert_eq!(limiter.ceiling_linear(), 1.0);
    assert_eq!(limiter.process(2.0), 1.0);
    assert_eq!(limiter.process(-3.0).abs(), 1.0);
}

#[test]
fn limiter_rejects_invalid_parameters_and_keeps_setters_transactional() {
    assert!(matches!(
        Limiter::new(SAMPLE_RATE_HZ, -60.1, 5.0),
        Err(DspError::InvalidCeilingDb(_))
    ));
    assert!(matches!(
        Limiter::new(SAMPLE_RATE_HZ, 0.1, 5.0),
        Err(DspError::InvalidCeilingDb(_))
    ));
    assert!(matches!(
        Limiter::new(SAMPLE_RATE_HZ, -6.0, 0.05),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(matches!(
        Limiter::new(SAMPLE_RATE_HZ, -6.0, 1_000.1),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(matches!(
        Limiter::new(0.0, -6.0, 5.0),
        Err(DspError::InvalidSampleRate(_))
    ));
    assert!(Limiter::new(SAMPLE_RATE_HZ, -60.0, 0.1).is_ok());
    assert!(Limiter::new(SAMPLE_RATE_HZ, 0.0, 1_000.0).is_ok());

    let mut limiter = Limiter::new(SAMPLE_RATE_HZ, -6.0, 5.0).unwrap();
    assert!(limiter.set_ceiling_db(0.5).is_err());
    assert_eq!(limiter.ceiling_db(), -6.0);
    assert!(limiter.set_ceiling_db(0.0).is_err() || limiter.ceiling_db() == 0.0);
    limiter.set_ceiling_db(-3.0).unwrap();
    assert_eq!(limiter.ceiling_db(), -3.0);
    assert!(limiter.set_release_ms(0.0).is_err());
    assert_eq!(limiter.release_ms(), 5.0);
    limiter.set_release_ms(20.0).unwrap();
    assert_eq!(limiter.release_ms(), 20.0);
    assert_eq!(limiter.sample_rate_hz(), SAMPLE_RATE_HZ);
}

#[test]
fn gate_closes_below_threshold_and_opens_above() {
    let range_db = 30.0_f32;
    let mut gate = Gate::new(SAMPLE_RATE_HZ, -40.0, range_db, 1.0, 5.0, 5.0).unwrap();
    for _ in 0..5_000 {
        let _ = gate.process(0.001);
    }
    assert_close(gate.current_attenuation_db(), range_db, 0.05);

    let mut output = 0.0;
    for _ in 0..2_000 {
        output = gate.process(0.5);
    }
    assert_close(gate.current_attenuation_db(), 0.0, 0.05);
    assert_close(output, 0.5, 1e-3);
}

#[test]
fn gate_hold_keeps_it_open_through_bounded_silence() {
    let hold_ms = 10.0_f32;
    let release_ms = 1.0_f32;
    let mut gate = Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 0.1, hold_ms, release_ms).unwrap();
    for _ in 0..480 {
        let _ = gate.process(1.0);
    }
    assert_close(gate.current_attenuation_db(), 0.0, 1e-6);

    let hold_samples = 480_usize;
    for _ in 0..hold_samples / 2 {
        let _ = gate.process(0.0);
    }
    assert_close(gate.current_attenuation_db(), 0.0, 1e-6);

    for _ in 0..2_000 {
        let _ = gate.process(0.0);
    }
    assert!(
        gate.current_attenuation_db() > 25.0,
        "gate did not close after the hold window"
    );
}

#[test]
fn gate_reset_restores_the_initial_response() {
    let input: Vec<f32> = (0..1_024)
        .map(|index| if index % 64 == 0 { 0.5 } else { 0.0 })
        .collect();
    let mut gate = Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, 10.0, 5.0).unwrap();
    let first: Vec<f32> = input.iter().map(|sample| gate.process(*sample)).collect();
    gate.reset();
    assert_eq!(gate.current_attenuation_db(), 0.0);
    let replay: Vec<f32> = input.iter().map(|sample| gate.process(*sample)).collect();
    assert_eq!(first, replay);
}

#[test]
fn gate_rejects_invalid_parameters_and_keeps_setters_transactional() {
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -80.1, 30.0, 1.0, 10.0, 5.0),
        Err(DspError::InvalidThresholdDb(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, 0.1, 30.0, 1.0, 10.0, 5.0),
        Err(DspError::InvalidThresholdDb(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, -0.1, 1.0, 10.0, 5.0),
        Err(DspError::InvalidRangeDb(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 80.1, 1.0, 10.0, 5.0),
        Err(DspError::InvalidRangeDb(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 0.05, 10.0, 5.0),
        Err(DspError::InvalidAttackMs(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1_000.1, 10.0, 5.0),
        Err(DspError::InvalidAttackMs(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, -0.1, 5.0),
        Err(DspError::InvalidHoldMs(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, 1_000.1, 5.0),
        Err(DspError::InvalidHoldMs(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, 10.0, 0.05),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(matches!(
        Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, 10.0, 5_000.1),
        Err(DspError::InvalidReleaseMs(_))
    ));
    assert!(Gate::new(SAMPLE_RATE_HZ, -80.0, 0.0, 0.1, 0.0, 0.1).is_ok());
    assert!(Gate::new(SAMPLE_RATE_HZ, 0.0, 80.0, 1_000.0, 1_000.0, 5_000.0).is_ok());

    let mut gate = Gate::new(SAMPLE_RATE_HZ, -40.0, 30.0, 1.0, 10.0, 5.0).unwrap();
    assert!(gate.set_threshold_db(1.0).is_err());
    assert_eq!(gate.threshold_db(), -40.0);
    assert!(gate.set_range_db(81.0).is_err());
    assert_eq!(gate.range_db(), 30.0);
    assert!(gate.set_attack_ms(0.0).is_err());
    assert_eq!(gate.attack_ms(), 1.0);
    assert!(gate.set_hold_ms(-1.0).is_err());
    assert_eq!(gate.hold_ms(), 10.0);
    assert!(gate.set_release_ms(6_000.0).is_err());
    assert_eq!(gate.release_ms(), 5.0);
    gate.set_threshold_db(-50.0).unwrap();
    gate.set_range_db(20.0).unwrap();
    gate.set_attack_ms(2.0).unwrap();
    gate.set_hold_ms(20.0).unwrap();
    gate.set_release_ms(100.0).unwrap();
    assert_eq!(gate.threshold_db(), -50.0);
    assert_eq!(gate.range_db(), 20.0);
    assert_eq!(gate.attack_ms(), 2.0);
    assert_eq!(gate.hold_ms(), 20.0);
    assert_eq!(gate.release_ms(), 100.0);
    assert_eq!(gate.sample_rate_hz(), SAMPLE_RATE_HZ);
}

#[test]
fn dsp_meter_accumulates_peak_and_rms() {
    let mut meter = DspMeter::new();
    assert_eq!(meter.rms(), 0.0);
    meter.add_sample(1.0);
    meter.add_sample(-1.0);
    meter.add_sample(0.5);
    assert_eq!(meter.samples(), 3);
    assert_eq!(meter.peak(), 1.0);
    assert_close(meter.rms(), ((1.0_f32 + 1.0 + 0.25) / 3.0).sqrt(), 1e-6);
    let reading = meter.reading();
    assert_eq!(reading.peak, 1.0);
    assert_close(reading.rms, meter.rms(), 0.0);

    meter.reset();
    assert_eq!(meter.samples(), 0);
    assert_eq!(
        meter.reading(),
        DspMeterReading {
            peak: 0.0,
            rms: 0.0
        }
    );
}

#[test]
fn bypassed_strip_is_exact_passthrough() {
    let mut strip = StripDsp::new(SAMPLE_RATE_HZ, 1).unwrap();
    let mut audio = mono_block(vec![0.25, -0.5, 0.125, 1.0, -1.0, 0.0]);
    let expected = audio.clone();
    strip.process_block(&mut audio).unwrap();
    assert_eq!(audio, expected);
}

#[test]
fn strip_process_sample_negates_exactly_and_validates_the_channel() {
    let mut strip = StripDsp::new(SAMPLE_RATE_HZ, 2).unwrap();
    strip.set_polarity(Polarity::Inverted);
    assert_eq!(strip.process_sample(0, 0.25).unwrap(), -0.25);
    assert_eq!(strip.process_sample(1, -0.25).unwrap(), 0.25);
    assert_eq!(
        strip.process_sample(2, 0.0),
        Err(DspError::ChannelIndexOutOfRange {
            channel: 2,
            channels: 2
        })
    );
    assert_eq!(strip.channels(), 2);
    assert_eq!(strip.polarity(), Polarity::Inverted);
}

#[test]
fn strip_processes_block_in_place_and_preserves_shape() {
    let mut strip = configured_strip();
    let mut audio = chain_input_block();
    let format = audio.format().clone();
    strip.process_block(&mut audio).unwrap();

    assert_eq!(audio.format(), &format);
    assert_eq!(audio.samples(), 4_096);
    assert_eq!(audio.channels(), 2);
    let ceiling = 10.0_f32.powf(-1.0 / 20.0);
    for sample in audio.planes().iter().flatten() {
        assert!(sample.is_finite(), "chain produced {sample}");
        assert!(sample.abs() <= ceiling + 1e-6, "{sample} exceeds {ceiling}");
    }

    let meter = strip.output_meter(0).unwrap();
    assert!(meter.peak > 0.0);
    assert!(meter.rms > 0.0);
    assert!(strip.output_meter(2).is_none());
    assert!(strip.compressor_gain_reduction_db(0).is_some());
    assert!(strip.limiter_gain_reduction_db(0).is_some());
    assert!(strip.gate_attenuation_db(0).is_some());

    strip.reset();
    assert_eq!(
        strip.output_meter(0),
        Some(DspMeterReading {
            peak: 0.0,
            rms: 0.0
        })
    );
}

#[test]
fn strip_rejects_wrong_block_shape_without_mutation() {
    let mut strip = configured_strip();
    let mut mono = mono_block(vec![0.5; 64]);
    let before = mono.clone();
    assert_eq!(
        strip.process_block(&mut mono),
        Err(DspError::ChannelCountMismatch {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(mono, before);

    let mut wrong_rate = AudioBlock::from_planar(
        AudioFormat {
            sample_rate: SampleRate::new(44_100).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
        vec![vec![0.5; 64], vec![0.5; 64]],
    )
    .unwrap();
    let before = wrong_rate.clone();
    assert!(matches!(
        strip.process_block(&mut wrong_rate),
        Err(DspError::SampleRateMismatch { .. })
    ));
    assert_eq!(wrong_rate, before);
}

#[test]
fn identical_strip_runs_are_bit_identical() {
    let mut first = configured_strip();
    let mut second = configured_strip();
    let mut first_block = chain_input_block();
    let mut second_block = chain_input_block();
    first.process_block(&mut first_block).unwrap();
    second.process_block(&mut second_block).unwrap();
    assert_bits_equal(&first_block, &second_block);
}

#[test]
fn strip_construction_enforces_bounds() {
    assert!(StripDsp::new(SAMPLE_RATE_HZ, 1).is_ok());
    assert!(StripDsp::new(SAMPLE_RATE_HZ, MAX_CHANNELS).is_ok());
    assert!(matches!(
        StripDsp::new(SAMPLE_RATE_HZ, 0),
        Err(DspError::ChannelCountOutOfRange {
            actual: 0,
            maximum: MAX_CHANNELS
        })
    ));
    assert!(matches!(
        StripDsp::new(SAMPLE_RATE_HZ, MAX_CHANNELS + 1),
        Err(DspError::ChannelCountOutOfRange {
            actual,
            maximum: MAX_CHANNELS
        }) if actual == MAX_CHANNELS + 1
    ));
    for rate in [0.0, -SAMPLE_RATE_HZ, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(StripDsp::new(rate, 1), Err(DspError::InvalidSampleRate(_))),
            "rate {rate}"
        );
    }
}

#[test]
fn strip_setters_validate_before_mutating() {
    let mut strip = StripDsp::new(SAMPLE_RATE_HZ, 2).unwrap();
    strip.set_eq_band(1, 1_000.0, 1.0, 3.0).unwrap();
    strip.set_gate(-40.0, 30.0, 1.0, 10.0, 50.0).unwrap();
    strip
        .set_compressor(-12.0, 4.0, 5.0, 80.0, 6.0, 3.0)
        .unwrap();
    strip.set_limiter(-1.0, 50.0).unwrap();
    strip.set_high_pass(120.0, Q_BUTTERWORTH).unwrap();
    let mut reference = strip.clone();

    assert!(matches!(
        strip.set_eq_band(0, 0.0, 1.0, 0.0),
        Err(DspError::InvalidFrequency(_))
    ));
    assert!(matches!(
        strip.set_eq_band(MAX_EQ_BANDS, 1_000.0, 1.0, 0.0),
        Err(DspError::BandIndexOutOfRange { .. })
    ));
    assert!(strip.set_high_pass(SAMPLE_RATE_HZ / 2.0, 1.0).is_err());
    assert!(strip.set_gate(-40.0, 30.0, 1.0, 10.0, 50_000.0).is_err());
    assert!(
        strip
            .set_compressor(-12.0, 0.5, 5.0, 80.0, 6.0, 3.0)
            .is_err()
    );
    assert!(strip.set_limiter(1.0, 50.0).is_err());
    assert!(matches!(
        strip.clear_eq_band(MAX_EQ_BANDS),
        Err(DspError::BandIndexOutOfRange { .. })
    ));

    let mut actual_block = chain_input_block();
    let mut expected_block = chain_input_block();
    strip.process_block(&mut actual_block).unwrap();
    reference.process_block(&mut expected_block).unwrap();
    assert_bits_equal(&actual_block, &expected_block);

    assert_eq!(strip.eq_band(1).unwrap().frequency_hz, 1_000.0);
    assert_eq!(strip.high_pass().unwrap().frequency_hz, 120.0);
}

#[test]
fn dsp_errors_display_bounded_messages() {
    let error = DspError::InvalidFrequency(0.0);
    assert!(error.to_string().contains("frequency"));
    let error = DspError::ChannelCountMismatch {
        expected: 2,
        actual: 1,
    };
    assert!(error.to_string().contains('2'));
    let source = std::error::Error::source(&error);
    assert!(source.is_none());
}
