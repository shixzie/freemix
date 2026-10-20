//! AU-001 solo modes (solo-in-place, PFL, AFL) on the monitor bus, per-group
//! solo, and nested VCA groups.

use std::num::NonZeroU128;

use fm_audio::{
    AudioBlock, AudioError, ChannelMapping, ChannelMeter, ClippingPolicy, Gain, GroupState,
    InputState, MasterMixer, MixerGroup, MixerMeterSlices, Pan, PlanarAudioSource, SoloMode,
    SourceGain,
};
use fm_frame::{
    ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SequenceNumber,
};
use fm_types::{
    AudioFormat, Channel, ChannelLayout, InputId, MediaTimestamp, SampleFormat, SampleRate,
    TimeBase,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn group(number: u8) -> MixerGroup {
    MixerGroup::new(number).unwrap()
}

fn stereo() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(48_000).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::stereo(),
    }
}

fn mono() -> AudioFormat {
    AudioFormat {
        channels: ChannelLayout::new(vec![Channel::Mono]).unwrap(),
        ..stereo()
    }
}

fn db(value: f32) -> f32 {
    10.0_f32.powf(value / 20.0)
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1.0e-6,
        "{actual} != {expected} (difference {})",
        (actual - expected).abs()
    );
}

fn add(mixer: &mut MasterMixer, id: InputId, state: InputState) {
    mixer
        .add_input(
            id,
            stereo(),
            ChannelMapping::identity(ChannelLayout::stereo()).unwrap(),
            state,
        )
        .unwrap();
}

fn constant(value: f32) -> AudioBlock {
    AudioBlock::from_planar(stereo(), vec![vec![value], vec![value]]).unwrap()
}

/// Two strips: `1` carries 0.5 at -6 dB, `2` carries 0.25 at unity.
fn two_strip_mixer() -> MasterMixer {
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    mixer.set_clipping_policy(ClippingPolicy::Allow);
    add(
        &mut mixer,
        input(1),
        InputState {
            gain: Gain::from_db(-6.0).unwrap(),
            ..InputState::default()
        },
    );
    add(&mut mixer, input(2), InputState::default());
    mixer
}

fn solo(mixer: &mut MasterMixer, id: InputId) {
    let state = InputState {
        soloed: true,
        ..mixer.input_state(id).unwrap()
    };
    mixer.set_input_state(id, state, 0).unwrap();
}

/// Renders one sample of 0.5 on strip 1 and 0.25 on strip 2; returns
/// `(master left, monitor left, monitor carries solo)`.
fn render(mixer: &mut MasterMixer) -> (f32, f32, bool) {
    let (one, two) = (constant(0.5), constant(0.25));
    let output = mixer
        .mix(1, &[(input(1), &one), (input(2), &two)], None)
        .unwrap();
    (
        output.block.planes()[0][0],
        output.monitor.planes[0][0],
        output.monitor.solo,
    )
}

#[test]
fn monitor_follows_master_while_nothing_is_soloed_in_every_mode() {
    for mode in SoloMode::ALL {
        let mut mixer = two_strip_mixer();
        mixer.set_solo_mode(mode);
        let (master, monitor, solo) = render(&mut mixer);
        assert_close(master, 0.5 * db(-6.0) + 0.25);
        assert_eq!(monitor, master);
        assert!(!solo);
    }
}

#[test]
fn solo_in_place_gates_program_and_monitor_follows_it() {
    let mut mixer = two_strip_mixer();
    assert_eq!(mixer.solo_mode(), SoloMode::InPlace);
    solo(&mut mixer, input(2));
    let (master, monitor, solo) = render(&mut mixer);
    assert_close(master, 0.25);
    assert_eq!(monitor, master);
    assert!(!solo);
}

#[test]
fn pfl_leaves_program_untouched_and_hears_the_pre_fader_signal() {
    let mut mixer = two_strip_mixer();
    mixer.set_solo_mode(SoloMode::Pfl);
    solo(&mut mixer, input(1));
    let (master, monitor, solo) = render(&mut mixer);
    // Program keeps both strips at their faders.
    assert_close(master, 0.5 * db(-6.0) + 0.25);
    // The monitor hears strip 1 before its -6 dB fader.
    assert_close(monitor, 0.5);
    assert!(solo);
}

#[test]
fn pfl_hears_a_muted_strip_and_ignores_follow_video_and_source_envelopes() {
    let mut mixer = two_strip_mixer();
    mixer.set_solo_mode(SoloMode::Pfl);
    let state = InputState {
        muted: true,
        soloed: true,
        follow_video: true,
        ..mixer.input_state(input(1)).unwrap()
    };
    mixer.set_input_state(input(1), state, 0).unwrap();
    let (master, monitor, _) = render(&mut mixer);
    assert_close(master, 0.25);
    assert_close(monitor, 0.5);

    let planes = vec![vec![0.5], vec![0.5]];
    let format = stereo();
    let source = PlanarAudioSource {
        input: input(1),
        sample_rate: format.sample_rate,
        channel_layout: &format.channels,
        planes: &planes,
        samples: 1,
        source_gain: SourceGain::new(0, 0, 1).unwrap(),
    };
    let mut output = vec![vec![0.0; 1]; 2];
    let mut monitor = vec![vec![0.0; 1]; 2];
    let (mut master_meters, mut input_meters, mut monitor_meters) = (
        [ChannelMeter::default(); 2],
        [ChannelMeter::default(); 4],
        [ChannelMeter::default(); 2],
    );
    let solo = mixer
        .mix_planar_timed_into_with_monitor(
            timing(1),
            1,
            &[source],
            &[],
            &mut output,
            &mut monitor,
            MixerMeterSlices {
                master: &mut master_meters,
                inputs: &mut input_meters,
                monitor: &mut monitor_meters,
                loudness: None,
            },
            None,
        )
        .unwrap();
    assert!(solo);
    assert_eq!(output, [vec![0.0], vec![0.0]]);
    assert_eq!(monitor, [vec![0.5], vec![0.5]]);
    assert_eq!(monitor_meters[0].peak, 0.5);
    assert_eq!(master_meters[0].peak, 0.0);
}

#[test]
fn afl_hears_the_post_fader_signal_through_group_faders() {
    let mut mixer = two_strip_mixer();
    mixer.set_solo_mode(SoloMode::Afl);
    let state = InputState {
        group: Some(group(1)),
        ..mixer.input_state(input(1)).unwrap()
    };
    mixer.set_input_state(input(1), state, 0).unwrap();
    mixer
        .set_group_state(
            group(1),
            GroupState {
                gain: Gain::from_db(-6.0).unwrap(),
                muted: true,
            },
            0,
        )
        .unwrap();
    solo(&mut mixer, input(1));
    let (master, monitor, solo) = render(&mut mixer);
    // The group mute silences strip 1 in Program only.
    assert_close(master, 0.25);
    assert_close(monitor, 0.5 * db(-12.0));
    assert!(solo);
}

#[test]
fn pfl_ignores_pan_and_afl_keeps_it() {
    for (mode, right) in [(SoloMode::Pfl, 1.0), (SoloMode::Afl, 0.0)] {
        let mut mixer = MasterMixer::new(stereo()).unwrap();
        let mapping = ChannelMapping::new(
            mono().channels,
            ChannelLayout::stereo(),
            vec![
                fm_audio::ChannelMappingRoute {
                    source: Channel::Mono,
                    destination: Channel::Left,
                    coefficient: 1.0,
                },
                fm_audio::ChannelMappingRoute {
                    source: Channel::Mono,
                    destination: Channel::Right,
                    coefficient: 1.0,
                },
            ],
        )
        .unwrap();
        mixer
            .add_input(
                input(1),
                mono(),
                mapping,
                InputState {
                    pan: Pan::from_basis_points(-10_000).unwrap(),
                    soloed: true,
                    ..InputState::default()
                },
            )
            .unwrap();
        mixer.set_solo_mode(mode);
        let block = AudioBlock::from_planar(mono(), vec![vec![0.5]]).unwrap();
        let output = mixer.mix(1, &[(input(1), &block)], None).unwrap();
        assert_close(output.monitor.planes[1][0], 0.5 * right);
    }
}

#[test]
fn soloing_a_group_solos_its_members_in_every_mode() {
    for mode in SoloMode::ALL {
        let mut mixer = two_strip_mixer();
        mixer.set_solo_mode(mode);
        let state = InputState {
            group: Some(group(3)),
            ..mixer.input_state(input(2)).unwrap()
        };
        mixer.set_input_state(input(2), state, 0).unwrap();
        mixer.set_group_soloed(group(3), true);
        assert!(mixer.group_soloed(group(3)));
        assert_eq!(mixer.input_effectively_soloed(input(2)), Some(true));
        assert_eq!(mixer.input_effectively_soloed(input(1)), Some(false));
        // The strip's own solo flag is untouched.
        assert!(!mixer.input_state(input(2)).unwrap().soloed);
        let (master, monitor, solo) = render(&mut mixer);
        if mode == SoloMode::InPlace {
            assert_close(master, 0.25);
            assert_eq!(monitor, master);
            assert!(!solo);
        } else {
            assert_close(master, 0.5 * db(-6.0) + 0.25);
            assert_close(monitor, 0.25);
            assert!(solo);
        }
        mixer.set_group_soloed(group(3), false);
        let (master, _, solo) = render(&mut mixer);
        assert_close(master, 0.5 * db(-6.0) + 0.25);
        assert!(!solo);
    }
}

#[test]
fn nested_groups_multiply_gains_and_inherit_mute_and_solo() {
    let mut mixer = two_strip_mixer();
    let state = InputState {
        group: Some(group(1)),
        ..mixer.input_state(input(1)).unwrap()
    };
    mixer.set_input_state(input(1), state, 0).unwrap();
    // 1 inside 2 inside 3.
    mixer.set_group_parent(group(1), Some(group(2))).unwrap();
    mixer.set_group_parent(group(2), Some(group(3))).unwrap();
    assert_eq!(mixer.group_parent(group(1)), Some(group(2)));
    for (number, gain) in [(1, -3.0), (2, -6.0), (3, -9.0)] {
        mixer
            .set_group_state(
                group(number),
                GroupState {
                    gain: Gain::from_db(gain).unwrap(),
                    muted: false,
                },
                0,
            )
            .unwrap();
    }
    let effective = db(-6.0) * db(-3.0) * db(-6.0) * db(-9.0);
    assert_close(
        mixer.current_effective_linear_gain(input(1)).unwrap(),
        effective,
    );
    let (master, _, _) = render(&mut mixer);
    assert_close(master, 0.5 * effective + 0.25);

    // The grandparent's mute silences the member.
    mixer
        .set_group_state(
            group(3),
            GroupState {
                gain: Gain::from_db(-9.0).unwrap(),
                muted: true,
            },
            0,
        )
        .unwrap();
    let (master, _, _) = render(&mut mixer);
    assert_close(master, 0.25);

    // The grandparent's solo solos the member.
    mixer.set_group_soloed(group(3), true);
    assert_eq!(mixer.input_effectively_soloed(input(1)), Some(true));

    // Leaving the chain restores the member's own group alone.
    mixer.set_group_parent(group(1), None).unwrap();
    assert_eq!(mixer.input_effectively_soloed(input(1)), Some(false));
    assert_close(
        mixer.current_effective_linear_gain(input(1)).unwrap(),
        db(-6.0) * db(-3.0),
    );
}

#[test]
fn group_parents_never_form_a_cycle() {
    let mut mixer = MasterMixer::new(stereo()).unwrap();
    assert_eq!(
        mixer.set_group_parent(group(1), Some(group(1))),
        Err(AudioError::GroupCycle {
            group: group(1),
            parent: group(1),
        })
    );
    mixer.set_group_parent(group(1), Some(group(2))).unwrap();
    mixer.set_group_parent(group(2), Some(group(3))).unwrap();
    mixer.set_group_parent(group(3), Some(group(4))).unwrap();
    assert_eq!(
        mixer.set_group_parent(group(4), Some(group(1))),
        Err(AudioError::GroupCycle {
            group: group(4),
            parent: group(1),
        })
    );
    assert_eq!(mixer.group_parent(group(4)), None);
    // Re-parenting along the chain without a cycle is fine.
    mixer.set_group_parent(group(1), Some(group(4))).unwrap();
}

#[test]
fn group_ramps_along_a_chain_advance_once_per_sample() {
    let mut mixer = two_strip_mixer();
    let state = InputState {
        gain: Gain::UNITY,
        group: Some(group(1)),
        ..InputState::default()
    };
    mixer.set_input_state(input(1), state, 0).unwrap();
    mixer.set_group_parent(group(1), Some(group(2))).unwrap();
    for number in [1, 2] {
        mixer
            .set_group_state(
                group(number),
                GroupState {
                    gain: Gain::from_linear(0.5).unwrap(),
                    muted: false,
                },
                2,
            )
            .unwrap();
    }
    let one = AudioBlock::from_planar(stereo(), vec![vec![1.0; 2], vec![1.0; 2]]).unwrap();
    let silence = AudioBlock::from_planar(stereo(), vec![vec![0.0; 2], vec![0.0; 2]]).unwrap();
    let output = mixer
        .mix(2, &[(input(1), &one), (input(2), &silence)], None)
        .unwrap();
    // Each group ramps 1.0 -> 0.75 -> 0.5; the member hears their product.
    assert_close(output.block.planes()[0][0], 0.75 * 0.75);
    assert_close(output.block.planes()[0][1], 0.25);
}

#[test]
fn solo_mode_and_groups_copy_with_runtime_state() {
    let mut source = two_strip_mixer();
    source.set_solo_mode(SoloMode::Afl);
    source.set_group_soloed(group(2), true);
    source.set_group_parent(group(2), Some(group(4))).unwrap();
    let mut target = two_strip_mixer();
    target.copy_runtime_state_from(&source).unwrap();
    assert_eq!(target.solo_mode(), SoloMode::Afl);
    assert!(target.group_soloed(group(2)));
    assert_eq!(target.group_parent(group(2)), Some(group(4)));
}

#[test]
fn monitor_meter_slices_are_checked_before_rendering() {
    let mut mixer = two_strip_mixer();
    let mut output = vec![vec![9.0; 1]; 2];
    let mut monitor = vec![vec![9.0; 1]; 2];
    assert_eq!(
        mixer.mix_planar_timed_into_with_monitor(
            timing(1),
            1,
            &[],
            &[],
            &mut output,
            &mut monitor,
            MixerMeterSlices {
                master: &mut [ChannelMeter::default(); 2],
                inputs: &mut [ChannelMeter::default(); 4],
                monitor: &mut [ChannelMeter::default(); 1],
                loudness: None,
            },
            None,
        ),
        Err(AudioError::MeterCountMismatch {
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(output, vec![vec![9.0; 1]; 2]);
    assert_eq!(monitor, vec![vec![9.0; 1]; 2]);
}

#[allow(clippy::cast_possible_truncation)]
fn timing(samples: usize) -> MediaTiming {
    let duration_nanos = (samples as u64) * 1_000_000_000 / 48_000;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, 48_000).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(duration_nanos).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}
