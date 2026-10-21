//! Strip and Master DSP wired into the Master mixer (AU-005): the chains
//! change what Program hears end to end, edits glide without clicks, the
//! transactional mixer pair carries DSP state.
//!
//! The workspace forbids `unsafe`, so no counting global allocator can
//! observe the render path; its allocation freedom rests on construction
//! (fixed-size per-channel state, preallocated scratch, stack sub-blocks).

use std::num::NonZeroU128;

use fm_audio::{
    ChannelMapping, ChannelMeter, InputState, MasterMixer, PlanarAudioSource, SourceGain,
};
use fm_frame::{
    ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SequenceNumber,
};
use fm_types::strip_dsp::{StripDsp, StripDspPreset};
use fm_types::{
    AudioFormat, ChannelLayout, InputId, MediaTimestamp, SampleFormat, SampleRate, TimeBase,
};

const RATE: u32 = 48_000;
const BLOCK: usize = 960;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn stereo() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(RATE).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::stereo(),
    }
}

fn timing(samples: usize) -> MediaTiming {
    let duration_nanos = (samples as u64) * 1_000_000_000 / u64::from(RATE);
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, RATE).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(duration_nanos).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}

fn mixer_with(inputs: &[InputId]) -> MasterMixer {
    let format = stereo();
    let mut mixer = MasterMixer::new(format.clone()).unwrap();
    for &id in inputs {
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                InputState {
                    follow_video: false,
                    ..InputState::default()
                },
            )
            .unwrap();
    }
    mixer
}

/// A continuous sine generator.
struct Tone {
    frequency: f64,
    amplitude: f32,
    phase: u64,
}

impl Tone {
    fn new(frequency: f64, amplitude: f32) -> Self {
        Self {
            frequency,
            amplitude,
            phase: 0,
        }
    }

    fn next_block(&mut self) -> Vec<Vec<f32>> {
        let plane: Vec<f32> = (0..BLOCK)
            .map(|offset| {
                #[allow(clippy::cast_precision_loss)]
                let time = (self.phase + offset as u64) as f64 / f64::from(RATE);
                #[allow(clippy::cast_possible_truncation)]
                let value = (core::f64::consts::TAU * self.frequency * time).sin() as f32;
                self.amplitude * value
            })
            .collect();
        self.phase += BLOCK as u64;
        vec![plane.clone(), plane]
    }
}

/// Renders one block of every `(input, planes)` into `output`; returns the
/// Master meters.
fn render(
    mixer: &mut MasterMixer,
    sources: &[(InputId, &[Vec<f32>])],
    output: &mut [Vec<f32>],
    input_meters: &mut [ChannelMeter],
) -> [ChannelMeter; 2] {
    let layout = ChannelLayout::stereo();
    let submissions: Vec<PlanarAudioSource<'_>> = sources
        .iter()
        .map(|(id, planes)| PlanarAudioSource {
            input: *id,
            sample_rate: SampleRate::new(RATE).unwrap(),
            channel_layout: &layout,
            planes,
            samples: BLOCK,
            source_gain: SourceGain::UNITY,
        })
        .collect();
    let mut master = [ChannelMeter::default(); 2];
    mixer
        .mix_planar_timed_into_with_meters(
            timing(BLOCK),
            BLOCK,
            &submissions,
            &[],
            output,
            (&mut master, input_meters),
        )
        .unwrap();
    master
}

/// The settled Master peak of `tone` through `mixer` after one second.
fn settled_peak(mixer: &mut MasterMixer, id: InputId, tone: &mut Tone) -> f32 {
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 2];
    let mut peak = 0.0;
    for _ in 0..50 {
        let planes = tone.next_block();
        peak = render(mixer, &[(id, &planes)], &mut output, &mut input_meters)[0].peak;
    }
    peak
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.log10()
}

#[test]
fn a_strip_eq_boost_raises_its_band_and_leaves_others_alone() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.eq.enabled = true;
    chain.eq.bands[2].gain_millidb = 12_000;
    chain.eq.bands[2].frequency_hz = 1_000;

    let mut flat = mixer_with(&[id]);
    let mut boosted = mixer_with(&[id]);
    boosted.set_input_dsp(id, &chain).unwrap();
    assert_eq!(boosted.input_dsp(id), Some(&chain));

    let reference = settled_peak(&mut flat, id, &mut Tone::new(1_000.0, 0.1));
    let at_band = settled_peak(&mut boosted, id, &mut Tone::new(1_000.0, 0.1));
    assert!((db(at_band / reference) - 12.0).abs() < 0.3, "{at_band}");

    let mut boosted = mixer_with(&[id]);
    boosted.set_input_dsp(id, &chain).unwrap();
    let far = settled_peak(&mut boosted, id, &mut Tone::new(12_000.0, 0.1));
    let reference = settled_peak(&mut mixer_with(&[id]), id, &mut Tone::new(12_000.0, 0.1));
    assert!(db(far / reference).abs() < 1.0, "{far} vs {reference}");
}

#[test]
fn a_strip_high_pass_removes_rumble() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.high_pass.enabled = true;
    chain.high_pass.frequency_hz = 300;
    let mut mixer = mixer_with(&[id]);
    mixer.set_input_dsp(id, &chain).unwrap();
    let rumble = settled_peak(&mut mixer, id, &mut Tone::new(40.0, 0.5));
    assert!(db(rumble / 0.5) < -30.0, "{rumble}");
}

#[test]
fn a_strip_gate_closes_on_low_level_noise_and_reports_its_attenuation() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.gate.enabled = true;
    chain.gate.threshold_millidb = -40_000;
    chain.gate.range_millidb = 40_000;
    let mut mixer = mixer_with(&[id]);
    mixer.set_input_dsp(id, &chain).unwrap();
    // A -60 dBFS hum sits under the -40 dBFS threshold.
    let hum = settled_peak(&mut mixer, id, &mut Tone::new(100.0, 0.001));
    assert!(db(hum / 0.001) < -35.0, "{hum}");
    let reading = mixer.input_dynamics(id).unwrap();
    assert!(reading.gate_db.unwrap() > 35.0, "{reading:?}");
    assert_eq!(reading.compressor_db, None);

    // Speech-level signal opens it again.
    let open = settled_peak(&mut mixer, id, &mut Tone::new(100.0, 0.3));
    assert!(db(open / 0.3).abs() < 0.5, "{open}");
}

#[test]
fn a_strip_compressor_reduces_peaks_and_reports_gain_reduction() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.compressor.enabled = true;
    chain.compressor.threshold_millidb = -20_000;
    chain.compressor.ratio_milli = 4_000;
    chain.compressor.knee_millidb = 0;
    let mut mixer = mixer_with(&[id]);
    mixer.set_input_dsp(id, &chain).unwrap();
    let loud = settled_peak(&mut mixer, id, &mut Tone::new(200.0, 0.9));
    let reduction = -db(loud / 0.9);
    assert!(reduction > 10.0, "{reduction}");
    let reading = mixer.input_dynamics(id).unwrap();
    assert!(reading.compressor_db.unwrap() > 10.0, "{reading:?}");
}

#[test]
fn the_master_limiter_holds_the_summed_bus_under_its_ceiling() {
    let (first, second) = (input(1), input(2));
    let mut mixer = mixer_with(&[first, second]);
    mixer
        .set_master_dsp(&StripDspPreset::Master.chain())
        .unwrap();
    let mut tone = Tone::new(300.0, 0.8);
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 4];
    let ceiling = 10.0_f32.powf(-1.0 / 20.0);
    for _ in 0..25 {
        let planes = tone.next_block();
        let master = render(
            &mut mixer,
            &[(first, &planes), (second, &planes)],
            &mut output,
            &mut input_meters,
        );
        assert!(master[0].peak <= ceiling + 1.0e-5, "{}", master[0].peak);
    }
    let reading = mixer.master_dynamics().unwrap();
    assert!(reading.limiter_db.unwrap() > 0.0 || reading.compressor_db.unwrap() > 3.0);
}

#[test]
fn enabling_a_stage_mid_stream_crossfades_without_a_click() {
    let id = input(1);
    let mut mixer = mixer_with(&[id]);
    // Create the chain disabled so the later edit is a pure retarget.
    let mut chain = StripDsp::DEFAULT;
    chain.bypass = true;
    chain.eq.enabled = true;
    chain.eq.bands[2].gain_millidb = 18_000;
    mixer.set_input_dsp(id, &chain).unwrap();
    let mut tone = Tone::new(1_000.0, 0.05);
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 2];
    let mut previous = 0.0_f32;
    let mut largest_step = 0.0_f32;
    for block in 0..20 {
        if block == 5 {
            chain.bypass = false;
            mixer.set_input_dsp(id, &chain).unwrap();
        }
        if block == 12 {
            chain.eq.bands[2].gain_millidb = -18_000;
            mixer.set_input_dsp(id, &chain).unwrap();
        }
        let planes = tone.next_block();
        render(&mut mixer, &[(id, &planes)], &mut output, &mut input_meters);
        for &sample in &output[0] {
            largest_step = largest_step.max((sample - previous).abs());
            previous = sample;
        }
    }
    // A +18 dB 1 kHz sine at 0.05 moves at most 0.4 * 2 pi * 1000 / 48000
    // per sample; a hard switch would jump far more.
    let slope = 0.05 * 7.95 * core::f32::consts::TAU * 1_000.0 / 48_000.0;
    assert!(largest_step < slope * 1.25, "{largest_step} vs {slope}");
}

#[test]
fn the_transactional_mixer_pair_carries_dsp_state() {
    let id = input(1);
    let chain = StripDspPreset::Voice.chain();
    let mut mixer = mixer_with(&[id]);
    mixer.set_input_dsp(id, &chain).unwrap();
    let mut pending = mixer.clone();
    let mut reference = mixer.clone();
    let mut tone = Tone::new(500.0, 0.4);
    let mut tone_reference = Tone::new(500.0, 0.4);
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut expected = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 2];
    for _ in 0..10 {
        // The daemon's pattern: copy, render into the pending mixer, swap.
        pending.copy_runtime_state_from(&mixer).unwrap();
        let planes = tone.next_block();
        render(
            &mut pending,
            &[(id, &planes)],
            &mut output,
            &mut input_meters,
        );
        core::mem::swap(&mut mixer, &mut pending);
        let planes = tone_reference.next_block();
        render(
            &mut reference,
            &[(id, &planes)],
            &mut expected,
            &mut input_meters,
        );
        assert_eq!(output, expected);
    }
    // A chain present on one side only is a shape mismatch.
    let mut bare = mixer_with(&[id]);
    assert!(bare.copy_runtime_state_from(&mixer).is_err());
}

/// A block the mixer rejects for a non-finite mixed sample, found only
/// after the strip chains ran, leaves their state as it was: the next
/// block renders exactly as on a mixer that never saw the rejected one.
#[test]
fn a_rejected_block_does_not_advance_the_dsp_state() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.high_pass.enabled = true;
    chain.high_pass.frequency_hz = 120;
    let build = || {
        let mut mixer = mixer_with(&[id, input(2), input(3)]);
        mixer.set_input_dsp(id, &chain).unwrap();
        mixer
            .set_master_dsp(&StripDspPreset::Master.chain())
            .unwrap();
        mixer
    };
    let (mut mixer, mut reference) = (build(), build());
    let mut tone = Tone::new(220.0, 0.5);
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut expected = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 6];
    for _ in 0..3 {
        let planes = tone.next_block();
        render(&mut mixer, &[(id, &planes)], &mut output, &mut input_meters);
        render(
            &mut reference,
            &[(id, &planes)],
            &mut expected,
            &mut input_meters,
        );
    }
    // Finite samples so large that two strips' sum overflows: the mixer
    // finds it only after the strip chain processed them.
    let mut poisoned = tone.next_block();
    for plane in &mut poisoned {
        for sample in &mut plane[BLOCK - 8..] {
            *sample = f32::MAX;
        }
    }
    let layout = ChannelLayout::stereo();
    let rejected = mixer.mix_planar_timed_into_with_meters(
        timing(BLOCK),
        BLOCK,
        &[id, input(2), input(3)].map(|input| PlanarAudioSource {
            input,
            sample_rate: SampleRate::new(RATE).unwrap(),
            channel_layout: &layout,
            planes: &poisoned,
            samples: BLOCK,
            source_gain: SourceGain::UNITY,
        }),
        &[],
        &mut output,
        (&mut [ChannelMeter::default(); 2], &mut input_meters),
    );
    assert!(rejected.is_err(), "{rejected:?}");
    for _ in 0..3 {
        let planes = tone.next_block();
        render(&mut mixer, &[(id, &planes)], &mut output, &mut input_meters);
        render(
            &mut reference,
            &[(id, &planes)],
            &mut expected,
            &mut input_meters,
        );
        assert_eq!(output, expected);
    }
}

/// A strip chain's scratch is sized when the mixer is configured and never
/// grows on the audio thread: a longer render through an active chain is
/// refused before anything changes, a transparent chain does not limit it.
#[test]
fn a_render_longer_than_the_dsp_block_capacity_is_refused() {
    let id = input(1);
    let mut chain = StripDsp::DEFAULT;
    chain.high_pass.enabled = true;
    let mut mixer = mixer_with(&[id]);
    mixer.set_dsp_block_capacity(BLOCK);
    mixer.set_input_dsp(id, &chain).unwrap();
    let long = 2 * BLOCK;
    let mut output = vec![vec![0.0; long]; 2];
    let mut meters = vec![ChannelMeter::default(); 2];
    let planes = vec![vec![0.25_f32; long]; 2];
    let layout = ChannelLayout::stereo();
    let mut mix = |mixer: &mut MasterMixer, samples: usize| {
        mixer.mix_planar_timed_into_with_meters(
            timing(samples),
            samples,
            &[PlanarAudioSource {
                input: id,
                sample_rate: SampleRate::new(RATE).unwrap(),
                channel_layout: &layout,
                planes: &planes,
                samples,
                source_gain: SourceGain::UNITY,
            }],
            &[],
            &mut output,
            (&mut [ChannelMeter::default(); 2], &mut meters),
        )
    };
    assert_eq!(
        mix(&mut mixer, long),
        Err(fm_audio::AudioError::DspBlockOverCapacity {
            samples: long,
            capacity: BLOCK,
        })
    );
    assert_eq!(mix(&mut mixer, BLOCK), Ok(()));
    // Configured for the longer block, it renders.
    mixer.set_dsp_block_capacity(long);
    assert_eq!(mix(&mut mixer, long), Ok(()));
    // A transparent chain processes nothing and so limits nothing.
    let mut bypassed = mixer_with(&[id]);
    bypassed.set_dsp_block_capacity(BLOCK);
    chain.bypass = true;
    bypassed.set_input_dsp(id, &chain).unwrap();
    assert_eq!(mix(&mut bypassed, long), Ok(()));
}

/// The delay stage holds a strip and then Program back to the exact
/// sample, and a rejected block between does not shift it.
#[test]
fn strip_and_master_delays_add_up_to_the_sample_through_a_rejected_block() {
    let id = input(1);
    let mut strip = StripDsp::DEFAULT;
    strip.delay.enabled = true;
    strip.delay.delay_us = 5_000;
    let mut master = StripDsp::DEFAULT;
    master.delay.enabled = true;
    master.delay.delay_us = 2_000;
    let mut mixer = mixer_with(&[id, input(2), input(3)]);
    mixer.set_input_dsp(id, &strip).unwrap();
    mixer.set_master_dsp(&master).unwrap();
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut input_meters = vec![ChannelMeter::default(); 6];
    let mut heard = Vec::new();
    let silence = vec![vec![0.0; BLOCK]; 2];
    let mut first = silence.clone();
    first[0][10] = 0.5;
    first[1][10] = 0.5;
    render(&mut mixer, &[(id, &first)], &mut output, &mut input_meters);
    heard.extend_from_slice(&output[0]);
    // Rejected after the chains ran: two strips' sum overflows.
    let poisoned = vec![vec![f32::MAX; BLOCK]; 2];
    let layout = ChannelLayout::stereo();
    let rejected = mixer.mix_planar_timed_into_with_meters(
        timing(BLOCK),
        BLOCK,
        &[id, input(2), input(3)].map(|input| PlanarAudioSource {
            input,
            sample_rate: SampleRate::new(RATE).unwrap(),
            channel_layout: &layout,
            planes: &poisoned,
            samples: BLOCK,
            source_gain: SourceGain::UNITY,
        }),
        &[],
        &mut output,
        (&mut [ChannelMeter::default(); 2], &mut input_meters),
    );
    assert!(rejected.is_err(), "{rejected:?}");
    render(
        &mut mixer,
        &[(id, &silence)],
        &mut output,
        &mut input_meters,
    );
    heard.extend_from_slice(&output[0]);
    // 5 ms + 2 ms at 48 kHz is 336 samples.
    let loud: Vec<usize> = heard
        .iter()
        .enumerate()
        .filter(|(_, sample)| **sample != 0.0)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(loud, vec![10 + 336], "{heard:?}");
}
