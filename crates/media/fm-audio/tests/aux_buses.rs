//! AU-002: the live Master mixer renders aux buses A–G beside Master. Each
//! strip sends to a bus at its own level, pre- or post-fader; strip mute and
//! follow-video cut every send while solo never does; each bus has its own
//! fader ramp, mute, clipping, and meters.

use std::num::NonZeroU128;

use fm_audio::{
    AUX_BUS_COUNT, AudioError, AuxBusOutput, Balance, BusSend, ChannelMapping, ChannelMeter,
    ClippingPolicy, Gain, GroupState, InputState, MasterMixer, MixBus, MixerMeterSlices,
    PlanarAudioSource, SendTap, SourceGain,
};
use fm_frame::{
    ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SequenceNumber,
};
use fm_types::{
    AudioFormat, ChannelLayout, InputId, MediaTimestamp, SampleFormat, SampleRate, TimeBase,
};

const RATE: u32 = 48_000;
const SAMPLES: usize = 480;

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
    let samples = u64::try_from(SAMPLES).unwrap();
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, RATE).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(samples * 1_000_000_000 / u64::from(RATE)).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}

fn mixer(strips: &[(InputId, InputState)]) -> MasterMixer {
    let format = stereo();
    let mut mixer = MasterMixer::new(format.clone()).unwrap();
    for &(id, state) in strips {
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                state,
            )
            .unwrap();
    }
    mixer
}

fn audible() -> InputState {
    InputState {
        follow_video: false,
        ..InputState::default()
    }
}

fn send(db: f32, tap: SendTap) -> BusSend {
    BusSend {
        enabled: true,
        level: Gain::from_db(db).unwrap(),
        tap,
    }
}

/// One render's Master planes, aux planes (bus-major), and aux meters.
struct Rendered {
    master: Vec<Vec<f32>>,
    aux: Vec<Vec<f32>>,
    aux_meters: Vec<ChannelMeter>,
}

impl Rendered {
    fn bus(&self, bus: MixBus) -> &[Vec<f32>] {
        let start = (bus.index() - 1) * 2;
        &self.aux[start..start + 2]
    }
}

/// Renders `strips`, each a constant `(left, right)` level, with `active`
/// on air.
fn render(
    mixer: &mut MasterMixer,
    strips: &[(InputId, (f32, f32))],
    active: &[InputId],
) -> Result<Rendered, AudioError> {
    let layout = ChannelLayout::stereo();
    let planes: Vec<Vec<Vec<f32>>> = strips
        .iter()
        .map(|&(_, (left, right))| vec![vec![left; SAMPLES], vec![right; SAMPLES]])
        .collect();
    let sources: Vec<PlanarAudioSource<'_>> = strips
        .iter()
        .zip(&planes)
        .map(|(&(id, _), planes)| PlanarAudioSource {
            input: id,
            sample_rate: SampleRate::new(RATE).unwrap(),
            channel_layout: &layout,
            planes,
            samples: SAMPLES,
            source_gain: SourceGain::UNITY,
        })
        .collect();
    let mut master = vec![vec![0.0; SAMPLES]; 2];
    let mut monitor = vec![vec![0.0; SAMPLES]; 2];
    let mut aux = vec![vec![9.0; SAMPLES]; AUX_BUS_COUNT * 2];
    let mut aux_meters = vec![ChannelMeter::default(); AUX_BUS_COUNT * 2];
    let mut master_meters = [ChannelMeter::default(); 2];
    let mut input_meters = vec![ChannelMeter::default(); strips.len() * 2];
    let mut monitor_meters = [ChannelMeter::default(); 2];
    mixer.mix_planar_timed_into_with_aux(
        timing(),
        SAMPLES,
        &sources,
        active,
        (&mut master, &mut monitor),
        MixerMeterSlices {
            master: &mut master_meters,
            inputs: &mut input_meters,
            monitor: &mut monitor_meters,
            loudness: None,
        },
        None,
        Some(AuxBusOutput {
            planes: &mut aux,
            meters: &mut aux_meters,
        }),
    )?;
    Ok(Rendered {
        master,
        aux,
        aux_meters,
    })
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1.0e-5,
        "{actual} != {expected} (difference {})",
        (actual - expected).abs()
    );
}

fn assert_level(planes: &[Vec<f32>], left: f32, right: f32) {
    for &sample in &planes[0] {
        assert_close(sample, left);
    }
    for &sample in &planes[1] {
        assert_close(sample, right);
    }
}

#[test]
fn a_post_fader_send_hears_the_strip_exactly_as_master_does_scaled_by_its_level() {
    let id = input(1);
    let state = InputState {
        gain: Gain::from_db(-6.0).unwrap(),
        ..audible()
    };
    let mut mixer = mixer(&[(id, state)]);
    mixer
        .set_aux_send(id, MixBus::A, send(0.0, SendTap::PostFader))
        .unwrap();
    mixer
        .set_aux_send(id, MixBus::C, send(-12.0, SendTap::PostFader))
        .unwrap();
    let rendered = render(&mut mixer, &[(id, (0.5, 0.25))], &[id]).unwrap();

    assert_eq!(rendered.bus(MixBus::A), &rendered.master[..]);
    let level = Gain::from_db(-12.0).unwrap().linear();
    let fader = Gain::from_db(-6.0).unwrap().linear();
    assert_level(
        rendered.bus(MixBus::C),
        0.5 * fader * level,
        0.25 * fader * level,
    );
    // Buses the strip does not send to are silent, not left as they were.
    for bus in [MixBus::B, MixBus::D, MixBus::E, MixBus::F, MixBus::G] {
        assert_level(rendered.bus(bus), 0.0, 0.0);
    }
    let a = (MixBus::A.index() - 1) * 2;
    assert_close(rendered.aux_meters[a].peak, 0.5 * fader);
    assert_close(rendered.aux_meters[a + 2].peak, 0.0);
}

#[test]
fn a_pre_fader_send_ignores_the_fader_balance_and_pan() {
    let id = input(1);
    let state = InputState {
        gain: Gain::from_db(-20.0).unwrap(),
        balance: Balance::from_basis_points(-10_000).unwrap(),
        ..audible()
    };
    let mut mixer = mixer(&[(id, state)]);
    mixer
        .set_aux_send(id, MixBus::B, send(-6.0, SendTap::PreFader))
        .unwrap();
    let rendered = render(&mut mixer, &[(id, (0.4, 0.2))], &[id]).unwrap();
    let level = Gain::from_db(-6.0).unwrap().linear();
    assert_level(rendered.bus(MixBus::B), 0.4 * level, 0.2 * level);
}

#[test]
fn mute_and_follow_video_cut_every_send_but_solo_does_not() {
    let (muted, offline, plain, soloed) = (input(1), input(2), input(3), input(4));
    let mut mixer = mixer(&[
        (
            muted,
            InputState {
                muted: true,
                ..audible()
            },
        ),
        (
            offline,
            InputState {
                follow_video: true,
                ..audible()
            },
        ),
        (plain, audible()),
        (
            soloed,
            InputState {
                soloed: true,
                ..audible()
            },
        ),
    ]);
    for id in [muted, offline, plain] {
        mixer
            .set_aux_send(id, MixBus::A, send(0.0, SendTap::PreFader))
            .unwrap();
        mixer
            .set_aux_send(id, MixBus::B, send(0.0, SendTap::PostFader))
            .unwrap();
    }
    let rendered = render(
        &mut mixer,
        &[
            (muted, (0.1, 0.1)),
            (offline, (0.2, 0.2)),
            (plain, (0.3, 0.3)),
            (soloed, (0.4, 0.4)),
        ],
        &[soloed],
    )
    .unwrap();
    // Solo-in-place silences `plain` in Master; the aux buses still hear it.
    assert_level(&rendered.master, 0.4, 0.4);
    assert_level(rendered.bus(MixBus::A), 0.3, 0.3);
    assert_level(rendered.bus(MixBus::B), 0.3, 0.3);
}

#[test]
fn a_bus_fader_ramps_a_bus_mute_silences_and_clamping_applies() {
    let id = input(1);
    let mut mixer = mixer(&[(id, audible())]);
    for bus in [MixBus::A, MixBus::B, MixBus::C] {
        mixer
            .set_aux_send(id, bus, send(6.0, SendTap::PostFader))
            .unwrap();
    }
    let half = GroupState {
        gain: Gain::from_linear(0.5).unwrap(),
        muted: false,
    };
    mixer.set_aux_bus_state(MixBus::A, half, 0).unwrap();
    mixer
        .set_aux_bus_state(
            MixBus::B,
            GroupState {
                muted: true,
                ..GroupState::default()
            },
            0,
        )
        .unwrap();
    mixer.set_clipping_policy(ClippingPolicy::Clamp);
    let rendered = render(&mut mixer, &[(id, (0.8, -0.8))], &[id]).unwrap();
    let boosted = 0.8 * Gain::from_db(6.0).unwrap().linear();
    assert_level(rendered.bus(MixBus::A), boosted * 0.5, -boosted * 0.5);
    assert_level(rendered.bus(MixBus::B), 0.0, 0.0);
    assert_level(rendered.bus(MixBus::C), 1.0, -1.0);
    assert_eq!(mixer.aux_bus_state(MixBus::A), Some(half));

    // A ramped bus fader reaches its target exactly on the ramp's last
    // sample and holds it.
    mixer
        .set_aux_bus_state(MixBus::A, GroupState::default(), SAMPLES)
        .unwrap();
    let rendered = render(&mut mixer, &[(id, (0.25, 0.25))], &[id]).unwrap();
    let scaled = 0.25 * Gain::from_db(6.0).unwrap().linear();
    let plane = &rendered.bus(MixBus::A)[0];
    assert!(plane[0] < scaled);
    assert_close(plane[SAMPLES - 1], scaled);
    let rendered = render(&mut mixer, &[(id, (0.25, 0.25))], &[id]).unwrap();
    assert_level(rendered.bus(MixBus::A), scaled, scaled);
}

#[test]
fn master_is_not_an_aux_bus_and_unknown_strips_are_refused() {
    let id = input(1);
    let mut mixer = mixer(&[(id, audible())]);
    assert_eq!(
        mixer.set_aux_send(id, MixBus::Master, BusSend::UNITY),
        Err(AudioError::NotAuxBus(MixBus::Master))
    );
    assert_eq!(
        mixer.set_aux_bus_state(MixBus::Master, GroupState::default(), 0),
        Err(AudioError::NotAuxBus(MixBus::Master))
    );
    assert_eq!(
        mixer.set_aux_send(input(9), MixBus::A, BusSend::UNITY),
        Err(AudioError::UnknownInput(input(9)))
    );
    assert_eq!(mixer.aux_send(id, MixBus::A), Some(BusSend::DISABLED));
    assert_eq!(mixer.aux_send(id, MixBus::Master), None);
    assert_eq!(mixer.aux_bus_state(MixBus::Master), None);
}

#[test]
fn copied_runtime_state_carries_sends_and_bus_faders() {
    let id = input(1);
    let mut source = mixer(&[(id, audible())]);
    source
        .set_aux_send(id, MixBus::G, send(-3.0, SendTap::PreFader))
        .unwrap();
    let quiet = GroupState {
        gain: Gain::from_db(-10.0).unwrap(),
        muted: false,
    };
    source.set_aux_bus_state(MixBus::G, quiet, 0).unwrap();
    let mut copy = mixer(&[(id, audible())]);
    copy.copy_runtime_state_from(&source).unwrap();
    assert_eq!(
        copy.aux_send(id, MixBus::G),
        Some(send(-3.0, SendTap::PreFader))
    );
    assert_eq!(copy.aux_bus_state(MixBus::G), Some(quiet));
}

#[test]
fn a_misshapen_aux_target_is_refused_before_anything_renders() {
    let id = input(1);
    let mut mixer = mixer(&[(id, audible())]);
    let layout = ChannelLayout::stereo();
    let planes = vec![vec![0.5; SAMPLES]; 2];
    let source = PlanarAudioSource {
        input: id,
        sample_rate: SampleRate::new(RATE).unwrap(),
        channel_layout: &layout,
        planes: &planes,
        samples: SAMPLES,
        source_gain: SourceGain::UNITY,
    };
    let mut master = vec![vec![7.0; SAMPLES]; 2];
    let mut monitor = vec![vec![0.0; SAMPLES]; 2];
    let mut aux = vec![vec![0.0; SAMPLES]; 2];
    let mut aux_meters = vec![ChannelMeter::default(); 2];
    let result = mixer.mix_planar_timed_into_with_aux(
        timing(),
        SAMPLES,
        &[source],
        &[id],
        (&mut master, &mut monitor),
        MixerMeterSlices {
            master: &mut [ChannelMeter::default(); 2],
            inputs: &mut [ChannelMeter::default(); 2],
            monitor: &mut [ChannelMeter::default(); 2],
            loudness: None,
        },
        None,
        Some(AuxBusOutput {
            planes: &mut aux,
            meters: &mut aux_meters,
        }),
    );
    assert_eq!(
        result,
        Err(AudioError::PlaneCountMismatch {
            expected: AUX_BUS_COUNT * 2,
            actual: 2,
        })
    );
    assert_level(&master, 7.0, 7.0);
}
