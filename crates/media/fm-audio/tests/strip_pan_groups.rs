//! AU-001 strip pan (mono positioning, -3 dB constant-power law) and VCA-style
//! strip groups (group fader and mute composed with each member's own fader).

use std::num::NonZeroU128;

use fm_audio::{
    AudioBlock, Balance, ChannelMapping, ChannelMappingRoute, ClippingPolicy, Gain, GroupState,
    InputState, MIXER_GROUP_COUNT, MasterMixer, MixerGroup, Pan,
};
use fm_types::{AudioFormat, Channel, ChannelLayout, InputId, SampleFormat, SampleRate};

const SQRT_2: f32 = std::f32::consts::SQRT_2;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn format(channels: ChannelLayout) -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(48_000).unwrap(),
        sample_format: SampleFormat::F32,
        channels,
    }
}

fn mono() -> AudioFormat {
    format(ChannelLayout::new(vec![Channel::Mono]).unwrap())
}

fn stereo() -> AudioFormat {
    format(ChannelLayout::stereo())
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1.0e-6,
        "{actual} != {expected} (difference {})",
        (actual - expected).abs()
    );
}

/// A Master mixer with a mono strip routed at unity to both Left and Right.
fn mono_mixer(id: InputId, state: InputState) -> MasterMixer {
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    let mapping = ChannelMapping::new(
        mono().channels,
        ChannelLayout::stereo(),
        vec![
            ChannelMappingRoute {
                source: Channel::Mono,
                destination: Channel::Left,
                coefficient: 1.0,
            },
            ChannelMappingRoute {
                source: Channel::Mono,
                destination: Channel::Right,
                coefficient: 1.0,
            },
        ],
    )
    .unwrap();
    mixer.add_input(id, mono(), mapping, state).unwrap();
    mixer
}

fn add_stereo(mixer: &mut MasterMixer, id: InputId, state: InputState) {
    mixer
        .add_input(
            id,
            stereo(),
            ChannelMapping::identity(ChannelLayout::stereo()).unwrap(),
            state,
        )
        .unwrap();
}

fn render(
    mixer: &mut MasterMixer,
    blocks: &[(InputId, &AudioBlock)],
    samples: usize,
) -> Vec<Vec<f32>> {
    mixer
        .mix(samples, blocks, None)
        .unwrap()
        .block
        .planes()
        .to_vec()
}

fn panned(basis_points: i32) -> InputState {
    InputState {
        pan: Pan::from_basis_points(basis_points).unwrap(),
        ..InputState::default()
    }
}

#[test]
fn pan_law_is_unity_at_center_and_constant_power_everywhere() {
    assert_eq!(Pan::CENTER.gains(), (1.0, 1.0));
    let (left, right) = Pan::from_basis_points(-10_000).unwrap().gains();
    assert_close(left, SQRT_2);
    assert_close(right, 0.0);
    let (left, right) = Pan::from_basis_points(10_000).unwrap().gains();
    assert_close(left, 0.0);
    assert_close(right, SQRT_2);
    for basis_points in (-10_000..=10_000).step_by(250) {
        let (left, right) = Pan::from_basis_points(basis_points).unwrap().gains();
        assert_close(left.mul_add(left, right * right), 2.0);
    }
    // Halfway right: theta = 3*pi/8.
    let (left, right) = Pan::from_basis_points(5_000).unwrap().gains();
    assert_close(left, SQRT_2 * (3.0 * std::f32::consts::PI / 8.0).cos());
    assert_close(right, SQRT_2 * (3.0 * std::f32::consts::PI / 8.0).sin());
    // The center is exactly 3 dB below a hard pan.
    assert_close(20.0 * (1.0 / SQRT_2).log10(), -3.010_3);
    assert_eq!(Pan::from_basis_points(10_001), None);
    assert_eq!(Pan::from_basis_points(-10_001), None);
}

#[test]
fn pan_positions_a_mono_strip_with_exact_sample_values() {
    let id = input(1);
    let source = AudioBlock::from_planar(mono(), vec![vec![0.5, -0.25]]).unwrap();
    let mut centered = mono_mixer(id, InputState::default());
    assert_eq!(
        render(&mut centered, &[(id, &source)], 2),
        vec![vec![0.5, -0.25], vec![0.5, -0.25]]
    );

    let mut hard_left = mono_mixer(id, panned(-10_000));
    let planes = render(&mut hard_left, &[(id, &source)], 2);
    assert_close(planes[0][0], 0.5 * SQRT_2);
    assert_close(planes[0][1], -0.25 * SQRT_2);
    assert_close(planes[1][0], 0.0);
    assert_close(planes[1][1], 0.0);

    let mut quarter_right = mono_mixer(id, panned(5_000));
    let source = AudioBlock::from_planar(mono(), vec![vec![0.5]]).unwrap();
    let planes = render(&mut quarter_right, &[(id, &source)], 1);
    let theta = 3.0 * std::f32::consts::PI / 8.0;
    assert_close(planes[0][0], 0.5 * SQRT_2 * theta.cos());
    assert_close(planes[1][0], 0.5 * SQRT_2 * theta.sin());
}

#[test]
fn pan_is_separate_from_balance_and_composes_with_it() {
    let id = input(1);
    let source = AudioBlock::from_planar(mono(), vec![vec![0.5]]).unwrap();
    // Balance alone trims the opposite side linearly; pan stays centered.
    let mut balanced = mono_mixer(
        id,
        InputState {
            balance: Balance::from_basis_points(5_000).unwrap(),
            ..InputState::default()
        },
    );
    let planes = render(&mut balanced, &[(id, &source)], 1);
    assert_close(planes[0][0], 0.25);
    assert_close(planes[1][0], 0.5);
    // Pan positions the mono source first, balance then trims the result.
    let mut both = mono_mixer(
        id,
        InputState {
            pan: Pan::from_basis_points(-10_000).unwrap(),
            balance: Balance::from_basis_points(5_000).unwrap(),
            ..InputState::default()
        },
    );
    let planes = render(&mut both, &[(id, &source)], 1);
    assert_close(planes[0][0], 0.5 * SQRT_2 * 0.5);
    assert_close(planes[1][0], 0.0);
}

#[test]
fn pan_leaves_a_stereo_strip_unchanged() {
    let id = input(1);
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    add_stereo(&mut mixer, id, panned(-10_000));
    let source = AudioBlock::from_planar(stereo(), vec![vec![0.5], vec![0.25]]).unwrap();
    assert_eq!(
        render(&mut mixer, &[(id, &source)], 1),
        vec![vec![0.5], vec![0.25]]
    );
}

#[test]
fn pan_moves_ramp_per_sample_without_zipper_steps() {
    let id = input(1);
    let mut mixer = mono_mixer(id, InputState::default());
    mixer.set_clipping_policy(ClippingPolicy::Allow);
    mixer.set_input_state(id, panned(10_000), 4).unwrap();
    let source = AudioBlock::from_planar(mono(), vec![vec![1.0; 4]]).unwrap();
    let planes = render(&mut mixer, &[(id, &source)], 4);
    for (sample, position) in [0.25_f32, 0.5, 0.75, 1.0].into_iter().enumerate() {
        let theta = (position + 1.0) * std::f32::consts::FRAC_PI_4;
        assert_close(planes[0][sample], SQRT_2 * theta.cos());
        assert_close(planes[1][sample], SQRT_2 * theta.sin());
    }
    assert_eq!(mixer.current_normalized_pan(id), Some(1.0));
}

#[test]
fn group_fader_composes_with_each_member_fader() {
    let group = MixerGroup::new(2).unwrap();
    let (member, other, outside) = (input(1), input(2), input(3));
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    add_stereo(
        &mut mixer,
        member,
        InputState {
            gain: Gain::from_db(-6.0).unwrap(),
            group: Some(group),
            ..InputState::default()
        },
    );
    add_stereo(
        &mut mixer,
        other,
        InputState {
            group: Some(group),
            ..InputState::default()
        },
    );
    add_stereo(&mut mixer, outside, InputState::default());
    mixer
        .set_group_state(
            group,
            GroupState {
                gain: Gain::from_db(-6.0).unwrap(),
                muted: false,
            },
            0,
        )
        .unwrap();
    let silence = AudioBlock::from_planar(stereo(), vec![vec![0.0], vec![0.0]]).unwrap();
    let one = AudioBlock::from_planar(stereo(), vec![vec![1.0], vec![1.0]]).unwrap();
    // The member's -6 dB and the group's -6 dB add to -12 dB.
    let member_only = render(
        &mut mixer,
        &[(member, &one), (other, &silence), (outside, &silence)],
        1,
    );
    assert_close(member_only[0][0], 10.0_f32.powf(-12.0 / 20.0));
    // A unity member of the same group follows the group fader alone.
    let other_only = render(
        &mut mixer,
        &[(member, &silence), (other, &one), (outside, &silence)],
        1,
    );
    assert_close(other_only[0][0], 10.0_f32.powf(-6.0 / 20.0));
    // A strip outside the group is untouched.
    let outside_only = render(
        &mut mixer,
        &[(member, &silence), (other, &silence), (outside, &one)],
        1,
    );
    assert_close(outside_only[0][0], 1.0);
}

#[test]
fn group_mute_silences_every_member_and_only_members() {
    let group = MixerGroup::new(1).unwrap();
    let (member, outside) = (input(1), input(2));
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    add_stereo(
        &mut mixer,
        member,
        InputState {
            group: Some(group),
            ..InputState::default()
        },
    );
    add_stereo(&mut mixer, outside, InputState::default());
    let member_block = AudioBlock::from_planar(stereo(), vec![vec![0.5], vec![0.5]]).unwrap();
    let outside_block = AudioBlock::from_planar(stereo(), vec![vec![0.125], vec![0.25]]).unwrap();
    let blocks = [(member, &member_block), (outside, &outside_block)];
    assert_eq!(
        render(&mut mixer, &blocks, 1),
        vec![vec![0.625], vec![0.75]]
    );
    mixer
        .set_group_state(
            group,
            GroupState {
                gain: Gain::UNITY,
                muted: true,
            },
            0,
        )
        .unwrap();
    let output = mixer.mix(1, &blocks, None).unwrap();
    assert_eq!(output.block.planes(), &[vec![0.125], vec![0.25]]);
    let member_meter = output
        .input_meters
        .iter()
        .find(|reading| reading.input == member)
        .unwrap();
    assert!(
        member_meter
            .meters
            .channels()
            .iter()
            .all(|meter| meter.peak == 0.0)
    );
    // The member's own unmuted state is preserved; unmuting the group
    // restores it without touching the strip.
    assert!(!mixer.input_state(member).unwrap().muted);
    mixer
        .set_group_state(group, GroupState::default(), 0)
        .unwrap();
    assert_eq!(
        render(&mut mixer, &blocks, 1),
        vec![vec![0.625], vec![0.75]]
    );
}

#[test]
fn group_fader_ramps_once_per_sample_across_members() {
    let group = MixerGroup::new(4).unwrap();
    let (first, second) = (input(1), input(2));
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    mixer.set_clipping_policy(ClippingPolicy::Allow);
    for id in [first, second] {
        add_stereo(
            &mut mixer,
            id,
            InputState {
                group: Some(group),
                ..InputState::default()
            },
        );
    }
    mixer
        .set_group_state(
            group,
            GroupState {
                gain: Gain::SILENCE,
                muted: false,
            },
            4,
        )
        .unwrap();
    let one = AudioBlock::from_planar(stereo(), vec![vec![1.0; 4], vec![0.0; 4]]).unwrap();
    // Both members ride the same ramp: 0.75, 0.5, 0.25, 0.0 each.
    let planes = render(&mut mixer, &[(first, &one), (second, &one)], 4);
    for (sample, expected) in [1.5_f32, 1.0, 0.5, 0.0].into_iter().enumerate() {
        assert_close(planes[0][sample], expected);
    }
    assert_close(mixer.current_group_linear_gain(group), 0.0);
}

#[test]
fn groups_are_numbered_one_through_four_and_reset_with_runtime_state() {
    assert_eq!(MIXER_GROUP_COUNT, 4);
    assert_eq!(MixerGroup::new(0), None);
    assert_eq!(MixerGroup::new(5), None);
    assert_eq!(MixerGroup::ALL.map(MixerGroup::number), [1, 2, 3, 4]);
    let group = MixerGroup::new(3).unwrap();
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    let state = GroupState {
        gain: Gain::from_db(-12.0).unwrap(),
        muted: true,
    };
    mixer.set_group_state(group, state, 100).unwrap();
    assert_close(mixer.current_group_linear_gain(group), 1.0);
    mixer.reset_runtime_state();
    assert_eq!(mixer.group_state(group), state);
    assert_close(mixer.current_group_linear_gain(group), state.gain.linear());
    let mut copy = MasterMixer::new(stereo()).unwrap();
    copy.copy_runtime_state_from(&mixer).unwrap();
    assert_eq!(copy.group_state(group), state);
    assert!(mixer.set_group_state(group, state, 48_001).is_err());
}
