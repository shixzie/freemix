//! Strip loudness meters and the Master bus insert in the Master mixer
//! (AU-005): a strip's loudness hears its post-fader contribution, a muted
//! strip measures silence, and an insert passes the Master chain.

use std::num::NonZeroU128;

use fm_audio::{
    ChannelMapping, ChannelMeter, InputState, LoudnessMeter, MasterBusInsert, MasterMixer,
    MixerMeterSlices, PlanarAudioSource, SourceGain,
};
use fm_frame::{
    ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SequenceNumber,
};
use fm_types::strip_dsp::StripDsp;
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

fn timing() -> MediaTiming {
    let duration_nanos = (BLOCK as u64) * 1_000_000_000 / u64::from(RATE);
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, RATE).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(duration_nanos).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}

fn mixer_with(inputs: &[(InputId, bool)]) -> MasterMixer {
    let format = stereo();
    let mut mixer = MasterMixer::new(format.clone()).unwrap();
    for &(id, muted) in inputs {
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                InputState {
                    follow_video: false,
                    muted,
                    ..InputState::default()
                },
            )
            .unwrap();
    }
    mixer
}

/// One block of a 1 kHz sine at `amplitude`, starting at sample `start`.
fn tone(amplitude: f32, start: usize) -> Vec<Vec<f32>> {
    let plane: Vec<f32> = (start..start + BLOCK)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let time = index as f64 / f64::from(RATE);
            #[allow(clippy::cast_possible_truncation)]
            let value = (core::f64::consts::TAU * 1_000.0 * time).sin() as f32;
            amplitude * value
        })
        .collect();
    vec![plane.clone(), plane]
}

#[allow(clippy::type_complexity)]
fn render(
    mixer: &mut MasterMixer,
    sources: &[(InputId, &[Vec<f32>])],
    loudness: &mut [LoudnessMeter],
    insert: Option<MasterBusInsert<'_>>,
) -> Vec<Vec<f32>> {
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
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut monitor = vec![vec![0.0; BLOCK]; 2];
    let inputs = loudness.len();
    mixer
        .mix_planar_timed_into_with_monitor(
            timing(),
            BLOCK,
            &submissions,
            &[],
            &mut output,
            &mut monitor,
            MixerMeterSlices {
                master: &mut [ChannelMeter::default(); 2],
                inputs: &mut vec![ChannelMeter::default(); inputs * 2],
                monitor: &mut [ChannelMeter::default(); 2],
                loudness: Some(loudness),
            },
            insert,
        )
        .unwrap();
    output
}

#[test]
fn each_strip_meters_its_own_contribution_and_a_muted_strip_silence() {
    let mut mixer = mixer_with(&[(input(1), false), (input(2), true)]);
    let layout = ChannelLayout::stereo();
    let mut loudness = vec![
        LoudnessMeter::new(RATE, &layout).unwrap(),
        LoudnessMeter::new(RATE, &layout).unwrap(),
    ];
    let amplitude = 10.0_f32.powf(-23.0 / 20.0);
    for block in 0..250 {
        let first = tone(amplitude, block * BLOCK);
        let second = tone(0.5, block * BLOCK);
        render(
            &mut mixer,
            &[(input(1), &first), (input(2), &second)],
            &mut loudness,
            None,
        );
    }
    let reading = loudness[0].reading();
    let integrated = reading.integrated_lufs.unwrap();
    assert!((integrated + 23.0).abs() < 0.1, "{integrated}");
    assert!((reading.short_term_lufs.unwrap() + 23.0).abs() < 0.1);
    assert_eq!(loudness[1].reading().momentary_lufs, None);
    assert_eq!(loudness[1].reading().true_peak_dbtp, None);
}

#[test]
fn a_meter_list_of_the_wrong_shape_is_refused() {
    let mut mixer = mixer_with(&[(input(1), false)]);
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut monitor = vec![vec![0.0; BLOCK]; 2];
    let mut loudness: Vec<LoudnessMeter> = Vec::new();
    assert!(
        mixer
            .mix_planar_timed_into_with_monitor(
                timing(),
                BLOCK,
                &[],
                &[],
                &mut output,
                &mut monitor,
                MixerMeterSlices {
                    master: &mut [ChannelMeter::default(); 2],
                    inputs: &mut [ChannelMeter::default(); 2],
                    monitor: &mut [ChannelMeter::default(); 2],
                    loudness: Some(&mut loudness),
                },
                None,
            )
            .is_err()
    );
}

#[test]
fn an_insert_adds_or_replaces_before_the_master_chain() {
    let mut mixer = mixer_with(&[(input(1), false)]);
    let mut chain = StripDsp::DEFAULT;
    chain.limiter.enabled = true;
    chain.limiter.ceiling_millidb = -6_000;
    mixer.set_master_dsp(&chain).unwrap();
    let ceiling = 10.0_f32.powf(-6.0 / 20.0);
    let strip = vec![vec![0.25_f32; BLOCK]; 2];
    let clip = vec![vec![0.75_f32; BLOCK]; 2];
    let mut loudness = vec![LoudnessMeter::new(RATE, &ChannelLayout::stereo()).unwrap()];
    // Mixed with Program: 0.25 + 0.75 = 1.0, held at the -6 dB ceiling.
    let output = render(
        &mut mixer,
        &[(input(1), &strip)],
        &mut loudness,
        Some(MasterBusInsert {
            planes: &clip,
            replace: false,
        }),
    );
    assert!(output[0].iter().all(|sample| *sample <= ceiling + 1e-6));
    assert!((output[0][BLOCK - 1] - ceiling).abs() < 1e-3);
    // Replacing Program: only the clip, still through the limiter (which
    // recovers from the loud block over its release).
    let quiet = vec![vec![0.1_f32; BLOCK]; 2];
    let mut output = Vec::new();
    for _ in 0..20 {
        output = render(
            &mut mixer,
            &[(input(1), &strip)],
            &mut loudness,
            Some(MasterBusInsert {
                planes: &quiet,
                replace: true,
            }),
        );
    }
    assert!(
        (output[1][BLOCK - 1] - 0.1).abs() < 1e-3,
        "{}",
        output[1][BLOCK - 1]
    );
    // A non-finite insert is refused before anything renders.
    let broken = vec![vec![f32::NAN; BLOCK]; 2];
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut monitor = vec![vec![0.0; BLOCK]; 2];
    assert!(
        mixer
            .mix_planar_timed_into_with_monitor(
                timing(),
                BLOCK,
                &[],
                &[],
                &mut output,
                &mut monitor,
                MixerMeterSlices {
                    master: &mut [ChannelMeter::default(); 2],
                    inputs: &mut [ChannelMeter::default(); 2],
                    monitor: &mut [ChannelMeter::default(); 2],
                    loudness: None,
                },
                Some(MasterBusInsert {
                    planes: &broken,
                    replace: false,
                }),
            )
            .is_err()
    );
}
