#![allow(
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::too_many_lines
)]

use fm_audio::{
    AutomixChannelSettings, AutomixConfig, AutomixError, Automixer, MAX_AUTOMIX_CHANNELS,
    MAX_AUTOMIX_SMOOTHING_SAMPLES, MAX_MONITOR_AUX_BUSES, MAX_MONITOR_STRIPS,
    MAX_SAMPLES_PER_BLOCK, MAX_TALKBACK_ROUTES, MonitorBus, MonitorDeviceId, MonitorError,
    MonitorSource, MonitorTap, TalkbackDestination, TalkbackRoute,
};

fn automixer(config: AutomixConfig) -> Automixer {
    Automixer::new(config).unwrap()
}

/// Processes one block of constant-valued planes and returns the outputs.
fn run(mixer: &mut Automixer, levels: &[f32], samples: usize) -> Vec<Vec<f32>> {
    let inputs: Vec<Vec<f32>> = levels.iter().map(|level| vec![*level; samples]).collect();
    run_planes(mixer, &inputs)
}

fn run_planes(mixer: &mut Automixer, inputs: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let samples = inputs[0].len();
    let input_refs: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
    let mut outputs = vec![vec![0.0_f32; samples]; inputs.len()];
    let mut output_refs: Vec<&mut [f32]> = outputs.iter_mut().map(Vec::as_mut_slice).collect();
    mixer.process(&input_refs, &mut output_refs, None).unwrap();
    outputs
}

fn shares(mixer: &Automixer) -> Vec<f32> {
    mixer
        .telemetry()
        .channels()
        .iter()
        .map(|channel| channel.share)
        .collect()
}

#[test]
fn single_open_mic_receives_full_gain_and_closed_mic_none() {
    let mut mixer = automixer(AutomixConfig::new(2));
    let outputs = run(&mut mixer, &[0.5, 0.0], 4);

    assert_eq!(outputs[0], vec![0.5; 4]);
    assert_eq!(outputs[1], vec![0.0; 4]);
    let telemetry = mixer.telemetry();
    assert_eq!(telemetry.block_index, 0);
    assert_eq!(telemetry.samples, 4);
    assert_eq!(telemetry.open_mics, 1);
    assert!(!telemetry.held);
    assert_eq!(telemetry.total_weighted_power, 0.25);
    assert_eq!(telemetry.nom_gain, 1.0);
    let channel = telemetry.channels()[0];
    assert_eq!(channel.mean_square, 0.25);
    assert!(channel.active);
    assert!(!channel.ducked);
    assert_eq!(channel.share, 1.0);
    assert_eq!(channel.target_gain, 1.0);
    assert_eq!(channel.gain, 1.0);
    assert!(!telemetry.channels()[1].active);
    assert_eq!(mixer.gains(), &[1.0, 0.0]);
}

#[test]
fn equal_levels_share_gain_at_constant_total_power() {
    let mut mixer = automixer(AutomixConfig::new(2));
    let outputs = run(&mut mixer, &[0.5, 0.5], 3);

    let gain = 0.5_f32.sqrt();
    assert_eq!(shares(&mixer), vec![0.5, 0.5]);
    assert_eq!(mixer.gains(), &[gain, gain]);
    assert_eq!(outputs[0], vec![0.5 * gain; 3]);
    assert_eq!(outputs[1], vec![0.5 * gain; 3]);
    assert_eq!(mixer.telemetry().open_mics, 2);
}

#[test]
fn shares_follow_level_ratio_and_weights() {
    let mut mixer = automixer(AutomixConfig::new(2));
    run(&mut mixer, &[0.5, 0.25], 2);
    assert_eq!(shares(&mixer), vec![0.8, 0.2]);
    assert_eq!(mixer.gains(), &[0.8_f32.sqrt(), 0.2_f32.sqrt()]);
    assert_eq!(mixer.telemetry().total_weighted_power, 0.3125);

    let mut weighted = automixer(AutomixConfig::new(2));
    weighted
        .set_channel(
            0,
            AutomixChannelSettings {
                weight: 3.0,
                ..AutomixChannelSettings::default()
            },
        )
        .unwrap();
    run(&mut weighted, &[0.5, 0.5], 2);
    assert_eq!(shares(&weighted), vec![0.75, 0.25]);
    assert_eq!(weighted.gains(), &[0.75_f32.sqrt(), 0.5]);
    assert_eq!(weighted.telemetry().total_weighted_power, 1.0);
}

#[test]
fn higher_priority_open_mic_ducks_lower_priorities() {
    let chair = AutomixChannelSettings {
        priority: 1,
        ..AutomixChannelSettings::default()
    };

    let mut full_cut = automixer(AutomixConfig::new(3));
    full_cut.set_channel(0, chair).unwrap();
    run(&mut full_cut, &[0.5, 0.5, 0.0], 2);
    assert_eq!(shares(&full_cut), vec![1.0, 0.0, 0.0]);
    let channels = full_cut.telemetry().channels();
    assert!(!channels[0].ducked);
    assert!(channels[1].ducked);
    assert!(channels[2].ducked);
    assert!(channels[1].active);

    let mut partial = automixer(AutomixConfig {
        priority_duck_db: Some(-20.0),
        ..AutomixConfig::new(2)
    });
    partial.set_channel(0, chair).unwrap();
    run(&mut partial, &[0.5, 0.5], 2);
    let duck = 10.0_f64.powf(-20.0 / 10.0);
    let total = 0.25 + 0.25 * duck;
    assert_eq!(
        shares(&partial),
        vec![(0.25 / total) as f32, (0.25 * duck / total) as f32]
    );
    assert_eq!(partial.telemetry().total_weighted_power, total as f32);

    // Once the chair closes, the lower-priority mic is no longer ducked.
    run(&mut partial, &[0.0, 0.5], 2);
    assert_eq!(shares(&partial), vec![0.0, 1.0]);
    assert!(!partial.telemetry().channels()[1].ducked);
}

#[test]
fn last_mic_hold_keeps_shares_through_silence() {
    let mut mixer = automixer(AutomixConfig::new(2));
    // Before any speech, a held automixer starts from equal shares.
    run(&mut mixer, &[0.0, 0.0], 2);
    assert!(mixer.telemetry().held);
    assert_eq!(shares(&mixer), vec![0.5, 0.5]);

    run(&mut mixer, &[0.0, 0.5], 2);
    assert_eq!(shares(&mixer), vec![0.0, 1.0]);
    let outputs = run(&mut mixer, &[0.0, 0.0001], 2);
    let telemetry = mixer.telemetry();
    assert_eq!(telemetry.open_mics, 0);
    assert!(telemetry.held);
    assert_eq!(telemetry.block_index, 2);
    assert_eq!(shares(&mixer), vec![0.0, 1.0]);
    assert_eq!(outputs[1], vec![0.0001; 2]);

    let mut no_hold = automixer(AutomixConfig {
        last_mic_hold: false,
        ..AutomixConfig::new(2)
    });
    run(&mut no_hold, &[0.0, 0.5], 2);
    run(&mut no_hold, &[0.0, 0.0], 2);
    assert!(!no_hold.telemetry().held);
    assert_eq!(shares(&no_hold), vec![0.5, 0.5]);
}

#[test]
fn attack_and_release_slew_linearly_in_samples() {
    let mut mixer = automixer(AutomixConfig {
        attack_samples: 4,
        release_samples: 8,
        ..AutomixConfig::new(2)
    });
    let outputs = run(&mut mixer, &[1.0, 0.0], 6);
    assert_eq!(outputs[0], vec![0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
    assert_eq!(mixer.telemetry().channels()[0].gain, 1.0);

    let outputs = run(&mut mixer, &[1.0, 1.0], 8);
    let target = 0.5_f32.sqrt();
    let mut expected_fall = Vec::new();
    let mut gain = 1.0_f32;
    for _ in 0..8 {
        gain = (gain - 0.125).max(target);
        expected_fall.push(gain);
    }
    assert_eq!(outputs[0], expected_fall);
    assert_eq!(outputs[0][..2], [0.875, 0.75]);
    let mut expected_rise = Vec::new();
    let mut gain = 0.0_f32;
    for _ in 0..8 {
        gain = (gain + 0.25).min(target);
        expected_rise.push(gain);
    }
    assert_eq!(outputs[1], expected_rise);
    assert_eq!(outputs[1][..3], [0.25, 0.5, target]);
    assert_eq!(mixer.gains(), &[target, target]);
}

#[test]
fn nom_attenuation_applies_only_to_the_summed_mix() {
    for (nom_attenuation, expected_mix) in [(false, 1.0), (true, 0.5)] {
        let mut mixer = automixer(AutomixConfig {
            nom_attenuation,
            ..AutomixConfig::new(4)
        });
        let inputs = [[0.5_f32; 2]; 4];
        let input_refs: Vec<&[f32]> = inputs.iter().map(<[f32; 2]>::as_slice).collect();
        let mut outputs = [[0.0_f32; 2]; 4];
        let mut output_refs: Vec<&mut [f32]> =
            outputs.iter_mut().map(<[f32; 2]>::as_mut_slice).collect();
        let mut mix = [9.0_f32; 2];
        let telemetry = mixer
            .process(&input_refs, &mut output_refs, Some(&mut mix))
            .unwrap();
        assert_eq!(telemetry.open_mics, 4);
        assert_eq!(telemetry.nom_gain, if nom_attenuation { 0.5 } else { 1.0 });
        assert_eq!(outputs, [[0.25; 2]; 4]);
        assert_eq!(mix, [expected_mix; 2]);
    }
}

#[test]
fn disabled_and_below_threshold_channels_are_closed() {
    let mut mixer = automixer(AutomixConfig::new(3));
    mixer
        .set_channel(
            2,
            AutomixChannelSettings {
                enabled: false,
                ..AutomixChannelSettings::default()
            },
        )
        .unwrap();
    // 0.0001 is -80 dBFS, below the default -60 dBFS threshold.
    let outputs = run(&mut mixer, &[0.5, 0.0001, 0.5], 2);
    let telemetry = mixer.telemetry();
    assert_eq!(telemetry.open_mics, 1);
    assert!(!telemetry.channels()[1].active);
    assert!(!telemetry.channels()[2].active);
    assert_eq!(telemetry.channels()[2].share, 0.0);
    assert_eq!(outputs[2], vec![0.0; 2]);
    // The quiet mic still takes its tiny power share.
    let total = 0.25_f64 + f64::from(0.0001_f32) * f64::from(0.0001_f32);
    assert_eq!(telemetry.channels()[0].share, (0.25 / total) as f32);
}

#[test]
fn processing_is_deterministic_across_instances() {
    let config = AutomixConfig {
        attack_samples: 3,
        release_samples: 5,
        ..AutomixConfig::new(3)
    };
    let blocks: Vec<Vec<Vec<f32>>> = (0..4_u8)
        .map(|block| {
            (0..3_u8)
                .map(|channel| {
                    (0..7_u8)
                        .map(|sample| f32::from((block + channel * 3 + sample) % 5) * 0.1 - 0.2)
                        .collect()
                })
                .collect()
        })
        .collect();
    let mut first = automixer(config);
    let mut second = automixer(config);
    for block in &blocks {
        assert_eq!(
            run_planes(&mut first, block),
            run_planes(&mut second, block)
        );
        assert_eq!(first.telemetry(), second.telemetry());
    }
    first.reset();
    assert_eq!(first.gains(), &[0.0, 0.0, 0.0]);
}

#[test]
fn automix_rejects_invalid_configuration_and_blocks_without_side_effects() {
    for channels in [0, MAX_AUTOMIX_CHANNELS + 1] {
        assert_eq!(
            Automixer::new(AutomixConfig::new(channels)).unwrap_err(),
            AutomixError::ChannelCountOutOfRange {
                actual: channels,
                maximum: MAX_AUTOMIX_CHANNELS
            }
        );
    }
    assert_eq!(
        Automixer::new(AutomixConfig {
            release_samples: MAX_AUTOMIX_SMOOTHING_SAMPLES + 1,
            ..AutomixConfig::new(1)
        })
        .unwrap_err(),
        AutomixError::SmoothingOutOfRange {
            actual: MAX_AUTOMIX_SMOOTHING_SAMPLES + 1,
            maximum: MAX_AUTOMIX_SMOOTHING_SAMPLES
        }
    );
    assert_eq!(
        Automixer::new(AutomixConfig {
            activity_threshold_db: 1.0,
            ..AutomixConfig::new(1)
        })
        .unwrap_err(),
        AutomixError::InvalidActivityThresholdDb(1.0)
    );
    assert_eq!(
        Automixer::new(AutomixConfig {
            priority_duck_db: Some(3.0),
            ..AutomixConfig::new(1)
        })
        .unwrap_err(),
        AutomixError::InvalidPriorityDuckDb(3.0)
    );

    let mut mixer = automixer(AutomixConfig::new(2));
    assert_eq!(
        mixer.set_channel(2, AutomixChannelSettings::default()),
        Err(AutomixError::ChannelIndexOutOfRange {
            channel: 2,
            channels: 2
        })
    );
    assert_eq!(
        mixer.set_channel(
            0,
            AutomixChannelSettings {
                weight: -1.0,
                ..AutomixChannelSettings::default()
            }
        ),
        Err(AutomixError::InvalidWeight(-1.0))
    );

    run(&mut mixer, &[0.5, 0.0], 2);
    let before = mixer.telemetry().clone();
    let good = [0.5_f32; 2];
    let nan = [0.5, f32::NAN];
    let short = [0.5_f32; 1];
    let mut out_a = [7.0_f32; 2];
    let mut out_b = [7.0_f32; 2];
    let mut mix = [7.0_f32; 3];
    let cases: [(&[&[f32]], bool, AutomixError); 4] = [
        (
            &[&good],
            false,
            AutomixError::PlaneCountMismatch {
                expected: 2,
                actual: 1,
            },
        ),
        (
            &[&good, &short],
            false,
            AutomixError::PlaneLengthMismatch {
                plane: 1,
                expected: 2,
                actual: 1,
            },
        ),
        (
            &[&good, &nan],
            false,
            AutomixError::NonFiniteSample {
                channel: 1,
                sample: 1,
            },
        ),
        (
            &[&good, &good],
            true,
            AutomixError::MixLengthMismatch {
                expected: 2,
                actual: 3,
            },
        ),
    ];
    for (inputs, with_mix, expected) in cases {
        let mut outputs: [&mut [f32]; 2] = [&mut out_a, &mut out_b];
        let mix = with_mix.then_some(&mut mix[..]);
        assert_eq!(
            mixer.process(inputs, &mut outputs, mix).unwrap_err(),
            expected
        );
    }
    let empty: [f32; 0] = [];
    let mut outputs: [&mut [f32]; 2] = [&mut [], &mut []];
    assert_eq!(
        mixer
            .process(&[&empty, &empty], &mut outputs, None)
            .unwrap_err(),
        AutomixError::SampleCountOutOfRange {
            actual: 0,
            maximum: MAX_SAMPLES_PER_BLOCK
        }
    );
    assert_eq!(out_a, [7.0; 2]);
    assert_eq!(out_b, [7.0; 2]);
    assert_eq!(mix, [7.0; 3]);
    assert_eq!(mixer.telemetry(), &before);
    assert_eq!(mixer.gains(), &[1.0, 0.0]);
}

const GUEST: TalkbackDestination = TalkbackDestination::Guest(7);
const CAMERA: TalkbackDestination = TalkbackDestination::Camera(2);

struct Fixture {
    program: [[f32; 3]; 2],
    strips: [[[f32; 3]; 2]; 3],
    aux: [[f32; 3]; 2],
    mic: [f32; 3],
}

impl Fixture {
    fn new() -> Self {
        Self {
            program: [[0.5, -0.25, 0.125], [0.25, 0.5, -0.5]],
            strips: [
                [[0.125, 0.0, 0.0], [0.0, 0.125, 0.0]],
                [[0.25, 0.25, 0.25], [0.5, 0.5, 0.5]],
                [[0.0, 0.0, 0.5], [-0.25, 0.0, 0.0]],
            ],
            aux: [[0.75, 0.0, -0.75], [0.0, 0.75, 0.0]],
            mic: [0.5, -0.5, 0.25],
        }
    }

    fn render(&self, bus: &mut MonitorBus) -> ([[f32; 3]; 2], fm_audio::MonitorTelemetry) {
        let program: [&[f32]; 2] = [&self.program[0], &self.program[1]];
        let strip_planes: Vec<[&[f32]; 2]> = self
            .strips
            .iter()
            .map(|strip| [&strip[0][..], &strip[1][..]])
            .collect();
        let strips: Vec<MonitorTap<'_>> = strip_planes
            .iter()
            .enumerate()
            .map(|(index, planes)| MonitorTap {
                index,
                planes: &planes[..],
            })
            .collect();
        let aux_planes: [&[f32]; 2] = [&self.aux[0], &self.aux[1]];
        let auxes = [MonitorTap {
            index: 3,
            planes: &aux_planes,
        }];
        let mut out = [[9.0_f32; 3]; 2];
        let (left, right) = out.split_at_mut(1);
        let mut out_refs: [&mut [f32]; 2] = [&mut left[0], &mut right[0]];
        let telemetry = bus
            .render(&program, &strips, &auxes, &self.mic, &mut out_refs)
            .unwrap();
        (out, telemetry)
    }
}

fn scaled(planes: [[f32; 3]; 2], gain: f32) -> [[f32; 3]; 2] {
    planes.map(|plane| plane.map(|sample| sample * gain))
}

fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

#[test]
fn master_monitor_never_contains_talkback() {
    let fixture = Fixture::new();
    let mut bus = MonitorBus::new(2, 8).unwrap();
    bus.add_talkback_route(TalkbackRoute {
        active: true,
        gain_db: 6.0,
        ..TalkbackRoute::new(GUEST)
    })
    .unwrap();
    bus.add_talkback_route(TalkbackRoute::new(CAMERA)).unwrap();
    bus.select_device(Some(MonitorDeviceId(4)));

    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(out, fixture.program);
    assert_eq!(telemetry.source, MonitorSource::Master);
    assert_eq!(telemetry.samples, 3);
    assert_eq!(telemetry.active_talkback_routes, 1);
    assert_eq!(telemetry.gain, 1.0);
    assert!(!telemetry.dimmed);
    assert_eq!(telemetry.device, Some(MonitorDeviceId(4)));
    assert_eq!(fixture.program, Fixture::new().program);

    let gain = db_to_linear(6.0);
    let guest = bus.talkback_feed(GUEST).unwrap();
    assert_eq!(guest.destination(), GUEST);
    assert_eq!(guest.samples(), &fixture.mic.map(|sample| sample * gain));
    assert_eq!(bus.talkback_feed(CAMERA).unwrap().samples(), &[0.0; 3]);
    assert!(bus.talkback_feed(TalkbackDestination::Guest(8)).is_none());

    // Every destination is a guest or camera feed; no program variant exists.
    for route in bus.talkback_routes() {
        match route.destination {
            TalkbackDestination::Guest(_) | TalkbackDestination::Camera(_) => {}
        }
    }
}

#[test]
fn dim_level_and_dim_on_talkback_apply_exact_gains() {
    let fixture = Fixture::new();
    let mut bus = MonitorBus::new(2, 8).unwrap();
    bus.set_level_db(-6.0).unwrap();
    let level = db_to_linear(-6.0);
    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(telemetry.gain, level);
    assert_eq!(out, scaled(fixture.program, level));

    bus.set_dim(true);
    let dimmed = level * db_to_linear(-20.0);
    let (out, telemetry) = fixture.render(&mut bus);
    assert!(telemetry.dimmed);
    assert_eq!(telemetry.gain, dimmed);
    assert_eq!(out, scaled(fixture.program, dimmed));

    bus.set_dim(false);
    bus.set_dim_on_talkback(true);
    bus.add_talkback_route(TalkbackRoute::new(CAMERA)).unwrap();
    assert!(!fixture.render(&mut bus).1.dimmed);
    bus.set_talkback_active(CAMERA, true).unwrap();
    let (out, telemetry) = fixture.render(&mut bus);
    assert!(telemetry.dimmed);
    assert_eq!(out, scaled(fixture.program, dimmed));
    assert_eq!(bus.talkback_feed(CAMERA).unwrap().samples(), &fixture.mic);
}

#[test]
fn pfl_sums_only_selected_strips() {
    let fixture = Fixture::new();
    let mut bus = MonitorBus::new(2, 8).unwrap();
    bus.set_source(MonitorSource::Pfl).unwrap();
    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(out, [[0.0; 3]; 2]);
    assert_eq!(telemetry.pfl_taps_mixed, 0);

    bus.set_pfl(0, true).unwrap();
    bus.set_pfl(2, true).unwrap();
    bus.set_pfl(40, true).unwrap();
    assert_eq!(bus.pfl_strips().collect::<Vec<_>>(), vec![0, 2, 40]);
    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(out, [[0.125, 0.0, 0.5], [-0.25, 0.125, 0.0]]);
    assert_eq!(telemetry.pfl_taps_mixed, 2);

    bus.set_pfl(2, false).unwrap();
    assert!(!bus.is_pfl(2));
    let (out, _) = fixture.render(&mut bus);
    assert_eq!(out, fixture.strips[0]);
    bus.clear_pfl();
    assert_eq!(bus.pfl_strips().count(), 0);
}

#[test]
fn aux_source_selects_matching_tap_or_silence() {
    let fixture = Fixture::new();
    let mut bus = MonitorBus::new(2, 8).unwrap();
    bus.set_source(MonitorSource::Aux(3)).unwrap();
    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(out, fixture.aux);
    assert!(telemetry.aux_present);

    bus.set_source(MonitorSource::Aux(1)).unwrap();
    let (out, telemetry) = fixture.render(&mut bus);
    assert_eq!(out, [[0.0; 3]; 2]);
    assert!(!telemetry.aux_present);
}

#[test]
fn monitor_rejects_invalid_controls_and_blocks() {
    assert_eq!(
        MonitorBus::new(0, 8).unwrap_err(),
        MonitorError::ChannelCountOutOfRange {
            actual: 0,
            maximum: 8
        }
    );
    assert_eq!(
        MonitorBus::new(2, MAX_SAMPLES_PER_BLOCK + 1).unwrap_err(),
        MonitorError::BlockSamplesOutOfRange {
            actual: MAX_SAMPLES_PER_BLOCK + 1,
            maximum: MAX_SAMPLES_PER_BLOCK
        }
    );
    let mut bus = MonitorBus::new(2, 2).unwrap();
    assert_eq!(
        bus.set_source(MonitorSource::Aux(MAX_MONITOR_AUX_BUSES)),
        Err(MonitorError::AuxIndexOutOfRange {
            aux: MAX_MONITOR_AUX_BUSES,
            maximum: MAX_MONITOR_AUX_BUSES
        })
    );
    assert_eq!(
        bus.set_pfl(MAX_MONITOR_STRIPS, true),
        Err(MonitorError::StripIndexOutOfRange {
            strip: MAX_MONITOR_STRIPS,
            maximum: MAX_MONITOR_STRIPS
        })
    );
    assert_eq!(
        bus.set_level_db(12.5),
        Err(MonitorError::InvalidLevelDb(12.5))
    );
    assert_eq!(
        bus.add_talkback_route(TalkbackRoute {
            gain_db: f32::INFINITY,
            ..TalkbackRoute::new(GUEST)
        }),
        Err(MonitorError::InvalidTalkbackGainDb(f32::INFINITY))
    );
    for id in 0..u16::try_from(MAX_TALKBACK_ROUTES).unwrap() {
        bus.add_talkback_route(TalkbackRoute::new(TalkbackDestination::Guest(id)))
            .unwrap();
    }
    assert_eq!(
        bus.add_talkback_route(TalkbackRoute::new(TalkbackDestination::Guest(0))),
        Err(MonitorError::DuplicateTalkbackRoute(
            TalkbackDestination::Guest(0)
        ))
    );
    assert_eq!(
        bus.add_talkback_route(TalkbackRoute::new(CAMERA)),
        Err(MonitorError::TalkbackRouteLimit {
            maximum: MAX_TALKBACK_ROUTES
        })
    );
    assert_eq!(
        bus.set_talkback_active(CAMERA, true),
        Err(MonitorError::UnknownTalkbackRoute(CAMERA))
    );
    assert_eq!(
        bus.remove_talkback_route(TalkbackDestination::Guest(0)),
        Ok(TalkbackRoute::new(TalkbackDestination::Guest(0)))
    );
    bus.set_talkback_gain_db(TalkbackDestination::Guest(1), -6.0)
        .unwrap();

    let good = [0.5_f32; 2];
    let nan = [f32::NAN, 0.0];
    let three = [0.0_f32; 3];
    let program: [&[f32]; 2] = [&good, &good];
    let planes: [&[f32]; 2] = [&good, &good];
    let mut a = [7.0_f32; 2];
    let mut b = [7.0_f32; 2];
    let mut out: [&mut [f32]; 2] = [&mut a, &mut b];
    assert_eq!(
        bus.render(&program, &[], &[], &three, &mut out),
        Err(MonitorError::BlockSamplesOutOfRange {
            actual: 3,
            maximum: 2
        })
    );
    assert_eq!(
        bus.render(&program, &[], &[], &nan, &mut out),
        Err(MonitorError::NonFiniteSample { sample: 0 })
    );
    assert_eq!(
        bus.render(&[&good], &[], &[], &good, &mut out),
        Err(MonitorError::PlaneCountMismatch {
            expected: 2,
            actual: 1
        })
    );
    let tap = MonitorTap {
        index: 1,
        planes: &planes,
    };
    assert_eq!(
        bus.render(&program, &[tap, tap], &[], &good, &mut out),
        Err(MonitorError::DuplicateTap { index: 1 })
    );
    let aux_tap = MonitorTap {
        index: MAX_MONITOR_AUX_BUSES,
        planes: &planes,
    };
    assert_eq!(
        bus.render(&program, &[], &[aux_tap], &good, &mut out),
        Err(MonitorError::AuxIndexOutOfRange {
            aux: MAX_MONITOR_AUX_BUSES,
            maximum: MAX_MONITOR_AUX_BUSES
        })
    );
    assert_eq!(a, [7.0; 2]);
    assert_eq!(b, [7.0; 2]);
    let feed = bus.talkback_feed(TalkbackDestination::Guest(1)).unwrap();
    assert_eq!(feed.samples(), &[] as &[f32]);
}
