//! The simulated audio plane (AU-005): the persisted strip and Master chains
//! applied to deterministic simulated tones, with the same meters the native
//! daemon reports.

#![allow(clippy::float_cmp)]

use std::num::NonZeroU128;

use fm_frame::ClockDomainId;
use fm_sim::{
    SimulatedAudioConfig, SimulatedAudioPlane, SimulatedAudioStrip, SimulatedStripState,
    SimulatedTone,
};
use fm_switcher::ProgramFrame;
use fm_types::strip_dsp::{StripDsp, StripDspTarget};
use fm_types::{AudioFormat, ChannelLayout, FrameRate, InputId, SampleFormat, SampleRate};

const SAMPLE_RATE_HZ: u32 = 48_000;
const FRAME_RATE: u32 = 60;
const FRAME_SAMPLES: usize = 800;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn format() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(SAMPLE_RATE_HZ).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::stereo(),
    }
}

fn frame_rate() -> FrameRate {
    FrameRate::new(FRAME_RATE, 1).unwrap()
}

fn clock() -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::new(1).unwrap())
}

fn program(input: InputId) -> ProgramFrame {
    ProgramFrame {
        primary: input,
        secondary: None,
        transition_kind: None,
        mix_numerator: 0,
        mix_denominator: 1,
        mix_start_numerator: 0,
        mix_end_numerator: 0,
    }
}

fn config(input: InputId, dsp: StripDsp, tone: Option<SimulatedTone>) -> SimulatedAudioConfig {
    let mut config = SimulatedAudioConfig::new(format(), frame_rate());
    config.strips.push(SimulatedAudioStrip::new(
        input,
        SimulatedStripState::default(),
        dsp,
        tone,
    ));
    config
}

fn plane(config: SimulatedAudioConfig) -> SimulatedAudioPlane {
    SimulatedAudioPlane::new(config, clock()).unwrap()
}

/// Renders `frames` and appends every frame's Program PCM per channel.
fn accumulate(
    plane: &mut SimulatedAudioPlane,
    input: InputId,
    frames: std::ops::Range<u64>,
) -> Vec<Vec<f32>> {
    let mut pcm = vec![Vec::new(); plane.program_planes().len()];
    for frame in frames {
        plane.render(program(input), frame, &[]).unwrap();
        let samples = plane.program_samples();
        for (channel, rendered) in pcm.iter_mut().zip(plane.program_planes()) {
            channel.extend_from_slice(&rendered[..samples]);
        }
    }
    pcm
}

#[test]
fn polarity_invert_negates_every_rendered_sample() {
    let input = input(1);
    let tone = Some(SimulatedTone::new(997.0));
    let mut inverted_chain = StripDsp::DEFAULT;
    inverted_chain.invert_polarity = true;
    let mut normal = plane(config(input, StripDsp::DEFAULT, tone));
    let mut inverted = plane(config(input, inverted_chain, tone));
    let normal_pcm = accumulate(&mut normal, input, 0..60);
    let inverted_pcm = accumulate(&mut inverted, input, 0..60);

    for (normal_channel, inverted_channel) in normal_pcm.iter().zip(&inverted_pcm) {
        assert_eq!(normal_channel.len(), 48_000, "one second at 60 fps");
        assert_eq!(inverted_channel.len(), 48_000);
        for (normal, inverted) in normal_channel.iter().zip(inverted_channel) {
            assert_eq!(*inverted, -*normal);
        }
        // The last ten frames are bit-exact negations. (An exact zero sums to
        // +0.0 on both sides, so only nonzero samples carry the sign bit.)
        let last_ten_frames = normal_channel.len() - 10 * FRAME_SAMPLES;
        for (normal, inverted) in normal_channel[last_ten_frames..]
            .iter()
            .zip(&inverted_channel[last_ten_frames..])
        {
            if *normal == 0.0 {
                assert_eq!(*inverted, 0.0);
            } else {
                assert_eq!(inverted.to_bits(), (-*normal).to_bits());
            }
        }
    }
}

#[test]
fn bypassed_chain_renders_exactly_the_default_chain_pcm() {
    let input = input(2);
    let tone = Some(SimulatedTone::new(440.0));
    let mut bypassed_chain = StripDsp::DEFAULT;
    bypassed_chain.bypass = true;
    let mut default_plane = plane(config(input, StripDsp::DEFAULT, tone));
    let mut bypassed_plane = plane(config(input, bypassed_chain, tone));
    let default_pcm = accumulate(&mut default_plane, input, 0..30);
    let bypassed_pcm = accumulate(&mut bypassed_plane, input, 0..30);

    assert_eq!(default_pcm, bypassed_pcm);
}

#[test]
fn master_limiter_caps_the_true_peak_and_reports_gain_reduction() {
    let input = input(3);
    let mut config = config(input, StripDsp::DEFAULT, Some(SimulatedTone::new(997.0)));
    // -18 dBFS + 24 dB = +6 dBFS into the Master chain.
    config.strips[0].state.gain_millidb = 24_000;
    config.master_dsp.limiter.enabled = true;
    config.master_dsp.limiter.ceiling_millidb = -1_000;
    let mut plane = plane(config);
    let pcm = accumulate(&mut plane, input, 0..120);

    let peak = pcm.iter().fold(0.0_f32, |peak, channel| {
        channel
            .iter()
            .fold(peak, |peak, sample| peak.max(sample.abs()))
    });
    let ceiling = 10.0_f32.powf(-1.0 / 20.0);
    assert!(
        peak <= ceiling * 1.000_001,
        "peak {peak} exceeds the limiter ceiling {ceiling}"
    );
    assert!(peak > 0.8, "the hot tone should drive the limiter: {peak}");

    let meters = plane.take_meters();
    let (_, dynamics) = meters
        .dynamics
        .iter()
        .find(|(target, _)| target.is_none())
        .expect("the Master chain reports dynamics");
    let reduction = dynamics
        .limiter_db
        .expect("the limiter is running and reports gain reduction");
    assert!(reduction > 0.5, "limiter reduction {reduction} dB");
}

#[test]
fn loudness_reset_restarts_integrated_but_not_momentary() {
    let input = input(4);
    let mut plane = plane(config(
        input,
        StripDsp::DEFAULT,
        Some(SimulatedTone::new(997.0)),
    ));
    accumulate(&mut plane, input, 0..90);
    {
        let meters = plane.take_meters();
        let (_, master) = meters.loudness.first().expect("the Master meter");
        assert!(master.momentary_lufs.is_some(), "1.5 s fills 400 ms");
        assert!(master.integrated_lufs.is_some());
    }

    plane
        .render(program(input), 90, &[StripDspTarget::Master])
        .unwrap();
    let meters = plane.take_meters();
    let (_, master) = meters.loudness.first().expect("the Master meter");
    assert!(
        master.momentary_lufs.is_some(),
        "momentary keeps running across a reset"
    );
    assert_eq!(
        master.integrated_lufs, None,
        "integrated restarts from the reset"
    );
}

#[test]
fn the_plane_is_deterministic_for_a_frame() {
    let input = input(5);
    let tone = Some(SimulatedTone::new(997.0));
    let mut first = plane(config(input, StripDsp::DEFAULT, tone));
    let mut second = plane(config(input, StripDsp::DEFAULT, tone));
    let first_pcm = accumulate(&mut first, input, 0..12);
    let second_pcm = accumulate(&mut second, input, 0..12);
    assert_eq!(first_pcm, second_pcm);

    // The same frame rendered from two identical fresh planes is identical.
    let mut third = plane(config(input, StripDsp::DEFAULT, tone));
    let mut fourth = plane(config(input, StripDsp::DEFAULT, tone));
    let third_pcm = accumulate(&mut third, input, 7..8);
    let fourth_pcm = accumulate(&mut fourth, input, 7..8);
    assert_eq!(third.sample_span(), fourth.sample_span());
    assert_eq!(third_pcm, fourth_pcm);
}
