//! The BS.1770 / EBU R128 meter against the EBU Tech 3341 reference cases
//! that can be synthesized (AU-005): steady sines, the gating cases, a
//! 5.0 layout, and inter-sample true peaks; the EBU Tech 3342 loudness
//! range cases that can be synthesized; and the operator reset.

use fm_audio::{LoudnessMeter, LoudnessReading};
use fm_types::{Channel, ChannelLayout};

const RATE: u32 = 48_000;

fn stereo() -> ChannelLayout {
    ChannelLayout::new(vec![Channel::Left, Channel::Right]).unwrap()
}

/// Feeds `seconds` of a sine at `dbfs` peak (`phase` in radians) to each
/// channel with a per-channel level offset.
fn feed(meter: &mut LoudnessMeter, frequency: f64, levels_dbfs: &[f64], seconds: f64, phase: f64) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (seconds * f64::from(RATE)).round() as usize;
    let amplitudes: Vec<f64> = levels_dbfs
        .iter()
        .map(|dbfs| 10.0_f64.powf(dbfs / 20.0))
        .collect();
    let mut frame = vec![0.0_f32; amplitudes.len()];
    for index in 0..samples {
        #[allow(clippy::cast_precision_loss)]
        let time = index as f64 / f64::from(RATE);
        let value = (core::f64::consts::TAU * frequency * time + phase).sin();
        for (sample, amplitude) in frame.iter_mut().zip(&amplitudes) {
            #[allow(clippy::cast_possible_truncation)]
            {
                *sample = (value * amplitude) as f32;
            }
        }
        meter.push_frame(&frame);
    }
}

fn feed_stereo(meter: &mut LoudnessMeter, dbfs: f64, seconds: f64) {
    feed(meter, 1_000.0, &[dbfs, dbfs], seconds, 0.0);
}

fn assert_near(actual: Option<f64>, expected: f64, tolerance: f64, what: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{what}: no reading"));
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} +-{tolerance}"
    );
}

#[test]
fn case_1_and_2_a_steady_sine_reads_its_level_everywhere() {
    for dbfs in [-23.0, -33.0] {
        let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
        feed_stereo(&mut meter, dbfs, 20.0);
        let reading = meter.reading();
        assert_near(reading.momentary_lufs, dbfs, 0.1, "momentary");
        assert_near(reading.short_term_lufs, dbfs, 0.1, "short-term");
        assert_near(reading.integrated_lufs, dbfs, 0.1, "integrated");
    }
}

#[test]
fn case_3_and_4_quiet_passages_fall_under_the_gates() {
    // Case 3: -36 / -23 / -36 dBFS for 10 / 60 / 10 s; the relative gate
    // drops the -36 passages.
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -36.0, 10.0);
    feed_stereo(&mut meter, -23.0, 60.0);
    feed_stereo(&mut meter, -36.0, 10.0);
    assert_near(meter.reading().integrated_lufs, -23.0, 0.1, "case 3");

    // Case 4 adds -72 dBFS passages the absolute gate drops.
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    for (dbfs, seconds) in [
        (-72.0, 10.0),
        (-36.0, 10.0),
        (-23.0, 60.0),
        (-36.0, 10.0),
        (-72.0, 10.0),
    ] {
        feed_stereo(&mut meter, dbfs, seconds);
    }
    assert_near(meter.reading().integrated_lufs, -23.0, 0.1, "case 4");
}

#[test]
fn case_5_louder_and_softer_passages_average_inside_the_gate() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -26.0, 20.0);
    feed_stereo(&mut meter, -20.0, 20.1);
    feed_stereo(&mut meter, -26.0, 20.0);
    assert_near(meter.reading().integrated_lufs, -23.0, 0.1, "case 5");
}

#[test]
fn ungated_audio_reports_no_integrated_loudness() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -75.0, 5.0);
    let reading = meter.reading();
    assert_eq!(reading.integrated_lufs, None);
    assert_near(reading.momentary_lufs, -75.0, 0.1, "momentary");
}

#[test]
fn surround_channels_carry_the_bs1770_weight() {
    // 5.0 with every front at -28 dBFS and each surround 10 log10(1.41) dB
    // lower: the 1.41 weight makes all five count alike, so the reading is
    // the stereo -28 LUFS plus 10 log10(5 / 2).
    let surround = -28.0 - 10.0 * 1.41_f64.log10();
    let expected = -28.0 + 10.0 * 2.5_f64.log10();
    let layout = ChannelLayout::new(vec![
        Channel::Left,
        Channel::Right,
        Channel::Center,
        Channel::LeftSurround,
        Channel::RightSurround,
    ])
    .unwrap();
    let mut meter = LoudnessMeter::new(RATE, &layout).unwrap();
    feed(
        &mut meter,
        1_000.0,
        &[-28.0, -28.0, -28.0, surround, surround],
        20.0,
        0.0,
    );
    assert_near(meter.reading().integrated_lufs, expected, 0.1, "5.0");
    // The LFE is not measured.
    let layout = ChannelLayout::new(vec![Channel::Left, Channel::LowFrequency]).unwrap();
    let mut meter = LoudnessMeter::new(RATE, &layout).unwrap();
    feed(&mut meter, 1_000.0, &[-100.0, -10.0], 5.0, 0.0);
    assert!(
        meter
            .reading()
            .momentary_lufs
            .is_none_or(|lufs| lufs < -90.0)
    );
}

#[test]
fn momentary_follows_a_change_within_400_ms_and_short_term_within_3_s() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -33.0, 5.0);
    feed_stereo(&mut meter, -23.0, 0.4);
    let reading = meter.reading();
    assert_near(reading.momentary_lufs, -23.0, 0.1, "momentary");
    // 0.4 s of -23 inside 3 s: 10 log10((0.4 * 10^-2.3 + 2.6 * 10^-3.3) / 3).
    let expected = 10.0 * ((0.4 * 10.0_f64.powf(-2.3) + 2.6 * 10.0_f64.powf(-3.3)) / 3.0).log10();
    assert_near(reading.short_term_lufs, expected, 0.1, "short-term");
    feed_stereo(&mut meter, -23.0, 3.0);
    assert_near(meter.reading().short_term_lufs, -23.0, 0.1, "settled");
}

#[test]
fn true_peak_finds_inter_sample_peaks() {
    // A quarter-rate sine at 45 degrees never samples its crest: every
    // sample is 3 dB under it. The true peak still reads -6 dBTP
    // (Tech 3341 tolerance +0.2 / -0.4 dB).
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed(
        &mut meter,
        f64::from(RATE) / 4.0,
        &[-6.0, -6.0],
        1.0,
        core::f64::consts::FRAC_PI_4,
    );
    let reading = meter.take_reading();
    let true_peak = reading.true_peak_dbtp.unwrap();
    assert!((-6.4..=-5.8).contains(&true_peak), "{true_peak}");
    // A low sine reads its own level.
    for (frequency, dbfs) in [(997.0, -6.0), (576.0, 0.0), (9_000.0, -12.0)] {
        let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
        feed(&mut meter, frequency, &[dbfs, dbfs], 1.0, 0.3);
        let true_peak = meter.reading().true_peak_dbtp.unwrap();
        assert!(
            (dbfs - 0.4..=dbfs + 0.2).contains(&true_peak),
            "{frequency} Hz: {true_peak}"
        );
    }
}

#[test]
fn the_interval_true_peak_starts_over_and_the_maximum_holds_until_reset() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -6.0, 0.5);
    let first = meter.take_reading();
    // The interpolator still holds the loud samples for a moment.
    feed_stereo(&mut meter, -20.0, 0.5);
    let _ = meter.take_reading();
    feed_stereo(&mut meter, -20.0, 0.5);
    let second = meter.take_reading();
    assert!(second.true_peak_dbtp.unwrap() < -19.0);
    assert_eq!(second.max_true_peak_dbtp, first.max_true_peak_dbtp);
    meter.reset();
    assert_eq!(meter.reading(), LoudnessReading::default());
}

#[test]
fn planes_and_frames_measure_the_same() {
    let mut by_frame = LoudnessMeter::new(RATE, &stereo()).unwrap();
    let mut by_plane = LoudnessMeter::new(RATE, &stereo()).unwrap();
    let plane: Vec<f32> = (0..48_000)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let time = index as f32 / 48_000.0;
            0.1 * (core::f32::consts::TAU * 440.0 * time).sin()
        })
        .collect();
    for chunk in plane.chunks(1_000) {
        by_plane.process_planes(&[chunk.to_vec(), chunk.to_vec()], chunk.len());
    }
    for sample in &plane {
        by_frame.push_frame(&[*sample, *sample]);
    }
    assert_eq!(by_frame.reading(), by_plane.reading());
}

/// Feeds a sequence of `(dBFS, seconds)` 1 kHz stereo passages.
fn feed_passages(meter: &mut LoudnessMeter, passages: &[(f64, f64)]) {
    for &(dbfs, seconds) in passages {
        feed_stereo(meter, dbfs, seconds);
    }
}

#[test]
fn tech_3342_cases_1_to_4_read_their_loudness_range() {
    // EBU Tech 3342 cases 1 to 4 (tolerance +-1 LU): stereo 1 kHz sines.
    let cases: [(&[(f64, f64)], f64); 4] = [
        (&[(-20.0, 20.0), (-30.0, 20.0)], 10.0),
        (&[(-20.0, 20.0), (-15.0, 20.0)], 5.0),
        (&[(-40.0, 20.0), (-20.0, 20.0)], 20.0),
        (
            &[
                (-50.0, 20.0),
                (-35.0, 20.0),
                (-20.0, 20.0),
                (-35.0, 20.0),
                (-50.0, 20.0),
            ],
            15.0,
        ),
    ];
    for (index, (passages, expected)) in cases.iter().enumerate() {
        let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
        feed_passages(&mut meter, passages);
        assert_near(
            meter.reading().loudness_range_lu,
            *expected,
            1.0,
            &format!("Tech 3342 case {}", index + 1),
        );
    }
}

#[test]
fn a_steady_programme_has_no_loudness_range_and_silence_none_at_all() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -23.0, 2.9);
    // No 3 s short-term value yet.
    assert_eq!(meter.reading().loudness_range_lu, None);
    feed_stereo(&mut meter, -23.0, 10.0);
    assert_near(meter.reading().loudness_range_lu, 0.0, 0.1, "steady");
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -80.0, 10.0);
    assert_eq!(meter.reading().loudness_range_lu, None);
}

#[test]
fn the_relative_gate_drops_short_term_values_20_lu_under_the_mean() {
    // A long -20 programme with a -45 passage: the passage sits 25 LU
    // under the mean, so it does not widen the range; the 3 s transitions
    // are under 5 % of the values, so they fall outside the percentiles.
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_passages(&mut meter, &[(-20.0, 60.0), (-45.0, 20.0), (-20.0, 60.0)]);
    assert_near(meter.reading().loudness_range_lu, 0.0, 1.0, "gated");
}

#[test]
fn an_operator_reset_restarts_integrated_range_and_max_true_peak_only() {
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_passages(&mut meter, &[(-10.0, 10.0), (-30.0, 10.0)]);
    let before = meter.take_reading();
    assert!(before.loudness_range_lu.unwrap() > 5.0);
    assert!(before.max_true_peak_dbtp.unwrap() > -11.0);
    meter.reset_measurement();
    // Momentary and short-term keep running through the reset.
    let reading = meter.reading();
    assert_near(reading.momentary_lufs, -30.0, 0.1, "momentary");
    assert_near(reading.short_term_lufs, -30.0, 0.1, "short-term");
    assert_eq!(reading.integrated_lufs, None);
    assert_eq!(reading.loudness_range_lu, None);
    assert_eq!(reading.max_true_peak_dbtp, None);
    // Only what follows the reset is measured: a -23 programme reads -23
    // integrated with no range, though the windows still held -30.
    feed_stereo(&mut meter, -23.0, 20.0);
    let reading = meter.reading();
    assert_near(reading.integrated_lufs, -23.0, 0.1, "integrated");
    assert_near(reading.loudness_range_lu, 0.0, 0.1, "range");
    assert!(reading.max_true_peak_dbtp.unwrap() < -22.0);
}

#[test]
fn a_reset_mid_hop_ignores_the_hop_that_began_before_it() {
    // 1.05 s of -10 then a reset: the hop in progress began loud, so
    // no gating block or short-term value may include it.
    let mut meter = LoudnessMeter::new(RATE, &stereo()).unwrap();
    feed_stereo(&mut meter, -10.0, 1.05);
    meter.reset_measurement();
    feed_stereo(&mut meter, -40.0, 0.35);
    // Four whole hops after the reset have not completed yet.
    assert_eq!(meter.reading().integrated_lufs, None);
    feed_stereo(&mut meter, -40.0, 0.05);
    assert_eq!(meter.reading().integrated_lufs, None);
    feed_stereo(&mut meter, -40.0, 0.1);
    assert_near(meter.reading().integrated_lufs, -40.0, 0.1, "first block");
}
