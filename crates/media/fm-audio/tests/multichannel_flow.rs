//! 7.1 and 16-channel discrete layouts travel through `fm-types`
//! `AudioBlock`s, `ChannelMapping`, and the Master mixer (`AU-003`).

#![allow(clippy::float_cmp)]

use std::num::NonZeroU128;

use fm_audio::{
    ChannelMapping, ChannelMappingRoute, ClippingPolicy, DownmixOptions, GainMatrix, InputState,
    MINUS_3_DB, MasterMixer, StandardLayout,
};
use fm_frame::{
    AudioBlock, ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp,
    OriginalTimestamp, SequenceNumber,
};
use fm_types::{
    AudioFormat, Channel, ChannelLayout, DiscreteChannel, InputId, MediaTimestamp, SampleFormat,
    SampleRate, TimeBase,
};

const SAMPLES: usize = 4;

fn timing() -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, 48_000).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(83_333).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}

fn format(channels: ChannelLayout) -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(48_000).unwrap(),
        sample_format: SampleFormat::F32,
        channels,
    }
}

fn block(layout: &ChannelLayout, planes: Vec<Vec<f32>>) -> AudioBlock {
    AudioBlock::new(
        timing(),
        SampleRate::new(48_000).unwrap(),
        layout.clone(),
        planes,
    )
    .unwrap()
}

fn seven_one() -> ChannelLayout {
    ChannelLayout::new(vec![
        Channel::Left,
        Channel::Right,
        Channel::Center,
        Channel::LowFrequency,
        Channel::LeftSurround,
        Channel::RightSurround,
        Channel::LeftRearSurround,
        Channel::RightRearSurround,
    ])
    .unwrap()
}

fn discrete(number: u8) -> Channel {
    Channel::Discrete(DiscreteChannel::new(number).unwrap())
}

fn discrete16() -> ChannelLayout {
    ChannelLayout::new((1..=16).map(discrete).collect()).unwrap()
}

/// Distinct, exactly representable planes: channel `c` sample `s` is
/// `(c + 1) / 16 + s / 64`.
#[allow(clippy::cast_precision_loss, reason = "small exact integers")]
fn ramp_planes(channels: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|channel| {
            (0..SAMPLES)
                .map(|sample| (channel + 1) as f32 / 16.0 + sample as f32 / 64.0)
                .collect()
        })
        .collect()
}

fn mix_one(
    master: &ChannelLayout,
    source: &ChannelLayout,
    mapping: ChannelMapping,
    input: &AudioBlock,
) -> AudioBlock {
    let id = InputId::new(NonZeroU128::new(1).unwrap());
    let mut mixer = MasterMixer::new(format(master.clone())).unwrap();
    mixer.set_clipping_policy(ClippingPolicy::Allow);
    mixer
        .add_input(id, format(source.clone()), mapping, InputState::default())
        .unwrap();
    mixer
        .mix_timed(timing(), SAMPLES, &[(id, input)], None)
        .unwrap()
        .block
}

fn gain_matrix_render(matrix: &GainMatrix, planes: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut output = vec![vec![0.0; SAMPLES]; matrix.output_layout().channel_count()];
    matrix.process(planes, &mut output).unwrap();
    output
}

#[test]
fn seven_one_block_maps_and_mixes_at_unity() {
    let layout = seven_one();
    assert_eq!(
        StandardLayout::SevenPointOne.layout().to_channel_layout(),
        layout
    );
    let planes = ramp_planes(8);
    let input = block(&layout, planes.clone());

    // Semantic reorder: rear surrounds swap places with side surrounds.
    let mut reordered_channels = layout.channels().to_vec();
    reordered_channels.swap(4, 6);
    reordered_channels.swap(5, 7);
    let reordered = ChannelLayout::new(reordered_channels).unwrap();
    let reorder = ChannelMapping::matching(layout.clone(), reordered.clone()).unwrap();
    let mapped = reorder.map(&input).unwrap();
    assert_eq!(mapped.channel_layout(), &reordered);
    assert_eq!(mapped.planes()[4], planes[6]);
    assert_eq!(mapped.planes()[7], planes[5]);

    // A 7.1 Master bus carries every channel, including the rear pair.
    let identity = ChannelMapping::identity(layout.clone()).unwrap();
    let mixed = mix_one(&layout, &layout, identity, &input);
    assert_eq!(mixed.channel_layout(), &layout);
    assert_eq!(mixed.planes(), planes.as_slice());
}

#[test]
fn seven_one_downmixes_to_stereo_master_through_gain_matrices() {
    let options = DownmixOptions::default();
    let to_five_one = GainMatrix::downmix(
        StandardLayout::SevenPointOne,
        StandardLayout::FivePointOne,
        options,
    )
    .unwrap();
    let to_stereo = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        options,
    )
    .unwrap();
    let layout = seven_one();
    let planes = ramp_planes(8);
    let input = block(&layout, planes.clone());

    // 7.1 -> 5.1 through ChannelMapping equals the GainMatrix render.
    let fold = to_five_one.to_channel_mapping().unwrap();
    assert_eq!(fold.source_layout(), &layout);
    let five_one = fold.map(&input).unwrap();
    let expected_five_one = gain_matrix_render(&to_five_one, &planes);
    assert_eq!(five_one.planes(), expected_five_one.as_slice());
    assert_eq!(
        five_one.planes()[4][0],
        MINUS_3_DB * planes[4][0] + MINUS_3_DB * planes[6][0]
    );

    // 5.1 -> 2.0 as a Master strip mapping into a stereo Master bus.
    let stereo = ChannelLayout::stereo();
    let downmix = to_stereo.to_channel_mapping().unwrap();
    let mixed = mix_one(
        &stereo,
        five_one.channel_layout(),
        downmix.clone(),
        &five_one,
    );
    assert_eq!(mixed.channel_layout(), &stereo);
    let expected = gain_matrix_render(&to_stereo, five_one.planes());
    for (actual, expected) in mixed.planes().iter().zip(&expected) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "{actual} vs {expected}"
            );
        }
    }
    // Every 7.1 channel except LFE reached the stereo bus.
    let mut silent_rears = planes.clone();
    silent_rears[6] = vec![0.0; SAMPLES];
    silent_rears[7] = vec![0.0; SAMPLES];
    let without_rears = mix_one(
        &stereo,
        five_one.channel_layout(),
        downmix,
        &fold.map(&block(&layout, silent_rears)).unwrap(),
    );
    assert!(without_rears.planes()[0][0] < mixed.planes()[0][0]);
    assert!(without_rears.planes()[1][0] < mixed.planes()[1][0]);
}

#[test]
fn sixteen_discrete_channels_map_and_mix() {
    let layout = discrete16();
    assert_eq!(
        StandardLayout::Discrete16.layout().to_channel_layout(),
        layout
    );
    let planes = ramp_planes(16);
    let input = block(&layout, planes.clone());

    // Unity 16-channel Master bus.
    let identity = ChannelMapping::identity(layout.clone()).unwrap();
    let mixed = mix_one(&layout, &layout, identity, &input);
    assert_eq!(mixed.channel_layout(), &layout);
    assert_eq!(mixed.planes(), planes.as_slice());

    // Pick SDI pair 2 (Ch3/Ch4) into stereo by label.
    let stereo = ChannelLayout::stereo();
    let pair = ChannelMapping::new(
        layout.clone(),
        stereo.clone(),
        vec![
            ChannelMappingRoute {
                source: discrete(3),
                destination: Channel::Left,
                coefficient: 1.0,
            },
            ChannelMappingRoute {
                source: discrete(4),
                destination: Channel::Right,
                coefficient: 1.0,
            },
        ],
    )
    .unwrap();
    let picked = pair.map(&input).unwrap();
    assert_eq!(picked.planes(), &[planes[2].clone(), planes[3].clone()]);

    // Fold all sixteen channels into a stereo Master: odd channels left, even
    // channels right, at 1/8 each; the result matches the GainMatrix render.
    let matrix = GainMatrix::new(
        StandardLayout::Discrete16.layout(),
        StandardLayout::Stereo.layout(),
        &(0..16)
            .map(|input| fm_audio::MatrixRoute {
                input,
                output: input % 2,
                gain: 0.125,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let fold = matrix.to_channel_mapping().unwrap();
    assert_eq!(fold.routes().len(), 16);
    let mixed = mix_one(&stereo, &layout, fold, &input);
    let expected = gain_matrix_render(&matrix, &planes);
    for (actual, expected) in mixed.planes().iter().zip(&expected) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "{actual} vs {expected}"
            );
        }
    }
    // Left = (1 + 3 + ... + 15) / 16 / 8 + sample / 64.
    assert_eq!(mixed.planes()[0][0], 0.5);
    assert_eq!(mixed.planes()[1][0], 0.5625);
}
