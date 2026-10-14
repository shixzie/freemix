use std::num::NonZeroU128;

use fm_switcher::{
    MAX_MIX_BANK_INPUTS, MAX_MIX_TRANSITION_DURATION_FRAMES, MAX_PENDING_MIX_EFFECTS,
    MAX_SECONDARY_MIXES, MIX_BANK_CAPACITY, MixBank, MixBankCommand, MixBankError, MixBankEvent,
    MixConfig, MixEffect, MixId, MixLayer, MixMode, MixSet, MixSource, OverlayChannelId,
    StingerSlotId, TransitionKind,
};
use fm_types::InputId;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn mix(number: u8) -> MixId {
    MixId::new(number).unwrap()
}

fn src(value: u128) -> MixSource {
    MixSource::Input(input(value))
}

fn bank() -> MixBank {
    MixBank::new((1..=6).map(input).collect(), input(1), input(2)).unwrap()
}

fn add_pp(bank: &mut MixBank, number: u8, program: MixSource, preview: MixSource) {
    bank.apply(MixBankCommand::AddMix {
        mix: mix(number),
        config: MixConfig::PreviewProgram { program, preview },
    })
    .unwrap();
}

fn add_output_only(bank: &mut MixBank, number: u8, program: MixSource) {
    bank.apply(MixBankCommand::AddMix {
        mix: mix(number),
        config: MixConfig::OutputOnly { program },
    })
    .unwrap();
}

#[test]
fn bank_addresses_main_plus_fifteen_secondary_mixes() {
    assert_eq!(MAX_SECONDARY_MIXES, 15);
    assert_eq!(MIX_BANK_CAPACITY, 16);
    assert_eq!(MixId::new(0), None);
    assert_eq!(MixId::new(17), None);
    assert!(MixId::MAIN.is_main());

    let mut bank = bank();
    for number in 2..=16 {
        add_pp(&mut bank, number, src(3), src(4));
    }
    assert_eq!(bank.configured_mixes().len(), 16);
    let (frame, _) = bank.advance_frame();
    assert_eq!(frame.mixes.len(), 16);
    assert_eq!(
        frame
            .mixes
            .iter()
            .map(|m| m.mix.number())
            .collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );

    assert_eq!(
        bank.apply(MixBankCommand::AddMix {
            mix: mix(5),
            config: MixConfig::OutputOnly { program: src(1) },
        }),
        Err(MixBankError::MixAlreadyExists(mix(5)))
    );
}

#[test]
fn construction_validates_inputs_and_main_sources() {
    assert_eq!(
        MixBank::new(vec![input(1), input(2), input(1)], input(1), input(2)),
        Err(MixBankError::DuplicateInput(input(1)))
    );
    let too_many = (1..=u128::try_from(MAX_MIX_BANK_INPUTS).unwrap() + 1)
        .map(input)
        .collect();
    assert_eq!(
        MixBank::new(too_many, input(1), input(2)),
        Err(MixBankError::TooManyInputs {
            requested: MAX_MIX_BANK_INPUTS + 1,
            maximum: MAX_MIX_BANK_INPUTS,
        })
    );
    assert_eq!(
        MixBank::new(vec![input(1), input(2)], input(1), input(9)),
        Err(MixBankError::UnknownInput(input(9)))
    );
    assert_eq!(
        MixBank::new(vec![input(1), input(2)], input(1), input(1)),
        Err(MixBankError::PreviewEqualsProgram {
            mix: MixId::MAIN,
            source: src(1),
        })
    );
}

#[test]
fn each_mix_cuts_and_transitions_independently() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));

    assert_eq!(
        bank.apply(MixBankCommand::Cut(mix(2))),
        Ok(vec![MixBankEvent::ProgramChanged {
            mix: mix(2),
            previous: src(3),
            program: src(4),
        }])
    );
    let main = bank.mix(MixId::MAIN).unwrap();
    assert_eq!((main.program(), main.preview()), (src(1), Some(src(2))));
    let second = bank.mix(mix(2)).unwrap();
    assert_eq!((second.program(), second.preview()), (src(4), Some(src(3))));

    bank.apply(MixBankCommand::Transition {
        mix: mix(2),
        kind: TransitionKind::Fade,
        duration_frames: 2,
    })
    .unwrap();
    assert_eq!(
        bank.apply(MixBankCommand::Cut(mix(2))),
        Err(MixBankError::TransitionInProgress(mix(2)))
    );
    // Main is unaffected by Mix 2's running transition.
    bank.apply(MixBankCommand::Cut(MixId::MAIN)).unwrap();

    let (frame0, events0) = bank.advance_frame();
    let m2 = frame0.mix(mix(2)).unwrap();
    assert_eq!(m2.primary, MixLayer::Input(input(4)));
    assert_eq!(m2.secondary, Some(MixLayer::Input(input(3))));
    assert_eq!((m2.mix_numerator, m2.mix_denominator), (0, 2));
    assert_eq!(events0, Vec::new());

    let (frame1, events1) = bank.advance_frame();
    assert_eq!(frame1.mix(mix(2)).unwrap().mix_numerator, 1);
    assert_eq!(
        events1,
        vec![
            MixBankEvent::ProgramChanged {
                mix: mix(2),
                previous: src(4),
                program: src(3),
            },
            MixBankEvent::TransitionCompleted {
                mix: mix(2),
                kind: TransitionKind::Fade,
                program: src(3),
            },
        ]
    );
    let (frame2, _) = bank.advance_frame();
    let m2 = frame2.mix(mix(2)).unwrap();
    assert_eq!(
        (m2.primary, m2.secondary),
        (MixLayer::Input(input(3)), None)
    );
    assert_eq!(m2.preview, Some(MixLayer::Input(input(4))));
}

#[test]
fn transition_durations_are_bounded() {
    let mut bank = bank();
    for duration_frames in [0, MAX_MIX_TRANSITION_DURATION_FRAMES + 1] {
        assert_eq!(
            bank.apply(MixBankCommand::Transition {
                mix: MixId::MAIN,
                kind: TransitionKind::Fade,
                duration_frames,
            }),
            Err(MixBankError::InvalidTransitionDuration {
                duration_frames,
                maximum: MAX_MIX_TRANSITION_DURATION_FRAMES,
            })
        );
    }
    assert!(bank.mix(MixId::MAIN).unwrap().transition().is_none());
}

#[test]
fn output_only_mix_has_no_preview_and_takes_directly() {
    let mut bank = bank();
    add_output_only(&mut bank, 3, src(5));
    assert_eq!(bank.mix(mix(3)).unwrap().mode(), MixMode::OutputOnly);

    assert_eq!(
        bank.apply(MixBankCommand::Cut(mix(3))),
        Err(MixBankError::PreviewUnavailable(mix(3)))
    );
    assert_eq!(
        bank.apply(MixBankCommand::SelectPreview {
            mix: mix(3),
            source: src(1),
        }),
        Err(MixBankError::PreviewUnavailable(mix(3)))
    );
    assert_eq!(
        bank.apply(MixBankCommand::CutTo {
            mix: MixId::MAIN,
            source: src(3),
        }),
        Err(MixBankError::DirectTakeRequiresOutputOnly(MixId::MAIN))
    );
    assert_eq!(
        bank.apply(MixBankCommand::CutTo {
            mix: mix(3),
            source: src(5),
        }),
        Err(MixBankError::SourceAlreadyProgram {
            mix: mix(3),
            source: src(5),
        })
    );

    bank.apply(MixBankCommand::CutTo {
        mix: mix(3),
        source: src(6),
    })
    .unwrap();
    bank.apply(MixBankCommand::TransitionTo {
        mix: mix(3),
        source: src(4),
        kind: TransitionKind::Wipe,
        duration_frames: 1,
    })
    .unwrap();
    let (frame, events) = bank.advance_frame();
    let m3 = frame.mix(mix(3)).unwrap();
    assert_eq!(m3.preview, None);
    assert_eq!(m3.primary, MixLayer::Input(input(6)));
    assert_eq!(m3.secondary, Some(MixLayer::Input(input(4))));
    assert_eq!(m3.transition_kind, Some(TransitionKind::Wipe));
    assert!(events.contains(&MixBankEvent::TransitionCompleted {
        mix: mix(3),
        kind: TransitionKind::Wipe,
        program: src(4),
    }));
    let state = bank.mix(mix(3)).unwrap();
    assert_eq!((state.program(), state.preview()), (src(4), None));
}

#[test]
fn mode_switching_validates_and_main_keeps_preview() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));
    assert_eq!(
        bank.apply(MixBankCommand::SetOutputOnly(MixId::MAIN)),
        Err(MixBankError::MainMixRequiresPreview)
    );
    assert_eq!(
        bank.apply(MixBankCommand::AddMix {
            mix: MixId::MAIN,
            config: MixConfig::OutputOnly { program: src(1) },
        }),
        Err(MixBankError::MixAlreadyExists(MixId::MAIN))
    );
    assert_eq!(
        bank.apply(MixBankCommand::SetOutputOnly(mix(2))),
        Ok(vec![MixBankEvent::ModeChanged {
            mix: mix(2),
            mode: MixMode::OutputOnly,
        }])
    );
    assert_eq!(
        bank.apply(MixBankCommand::SetOutputOnly(mix(2))),
        Ok(vec![])
    );
    assert_eq!(
        bank.apply(MixBankCommand::EnablePreview {
            mix: mix(2),
            preview: src(3),
        }),
        Err(MixBankError::PreviewEqualsProgram {
            mix: mix(2),
            source: src(3),
        })
    );
    bank.apply(MixBankCommand::EnablePreview {
        mix: mix(2),
        preview: src(6),
    })
    .unwrap();
    assert_eq!(bank.mix(mix(2)).unwrap().preview(), Some(src(6)));
    assert_eq!(
        bank.apply(MixBankCommand::EnablePreview {
            mix: mix(2),
            preview: src(5),
        }),
        Err(MixBankError::PreviewAlreadyEnabled(mix(2)))
    );
}

#[test]
fn routing_rejects_self_routes_cycles_and_unknown_mixes() {
    let mut bank = bank();
    assert_eq!(
        bank.apply(MixBankCommand::AddMix {
            mix: mix(2),
            config: MixConfig::OutputOnly {
                program: MixSource::Mix(mix(2)),
            },
        }),
        Err(MixBankError::SelfRoute(mix(2)))
    );
    assert_eq!(
        bank.apply(MixBankCommand::AddMix {
            mix: mix(2),
            config: MixConfig::OutputOnly {
                program: MixSource::Mix(mix(9)),
            },
        }),
        Err(MixBankError::UnknownMix(mix(9)))
    );

    // Mix 3 <- Mix 2 <- Main.
    add_output_only(&mut bank, 2, MixSource::Mix(MixId::MAIN));
    add_output_only(&mut bank, 3, MixSource::Mix(mix(2)));

    let before = bank.clone();
    assert_eq!(
        bank.apply(MixBankCommand::SelectPreview {
            mix: MixId::MAIN,
            source: MixSource::Mix(mix(3)),
        }),
        Err(MixBankError::RoutingCycle {
            mix: MixId::MAIN,
            source: mix(3),
        })
    );
    assert_eq!(
        bank.apply(MixBankCommand::CutTo {
            mix: mix(2),
            source: MixSource::Mix(mix(3)),
        }),
        Err(MixBankError::RoutingCycle {
            mix: mix(2),
            source: mix(3),
        })
    );
    assert_eq!(
        bank.apply(MixBankCommand::TransitionTo {
            mix: mix(2),
            source: MixSource::Mix(mix(3)),
            kind: TransitionKind::Fade,
            duration_frames: 4,
        }),
        Err(MixBankError::RoutingCycle {
            mix: mix(2),
            source: mix(3),
        })
    );
    assert_eq!(bank, before, "rejected routes leave the bank unchanged");

    assert_eq!(
        bank.apply(MixBankCommand::RemoveMix(mix(2))),
        Err(MixBankError::MixReferenced {
            mix: mix(2),
            by: mix(3),
        })
    );
    assert_eq!(
        bank.apply(MixBankCommand::RemoveMix(MixId::MAIN)),
        Err(MixBankError::MainMixIsPermanent)
    );
    bank.apply(MixBankCommand::RemoveMix(mix(3))).unwrap();
    bank.apply(MixBankCommand::RemoveMix(mix(2))).unwrap();
    assert_eq!(bank.configured_mixes(), MixSet::single(MixId::MAIN));
}

#[test]
fn running_transition_endpoint_counts_as_a_route() {
    let mut bank = bank();
    add_output_only(&mut bank, 2, src(3));
    bank.apply(MixBankCommand::TransitionTo {
        mix: mix(2),
        source: MixSource::Mix(MixId::MAIN),
        kind: TransitionKind::Fade,
        duration_frames: 10,
    })
    .unwrap();
    assert_eq!(
        bank.apply(MixBankCommand::SelectPreview {
            mix: MixId::MAIN,
            source: MixSource::Mix(mix(2)),
        }),
        Err(MixBankError::RoutingCycle {
            mix: MixId::MAIN,
            source: mix(2),
        })
    );
}

#[test]
fn downstream_reads_are_delayed_exactly_one_frame() {
    let mut bank = bank();
    // Chain: Main -> Mix 2 -> Mix 3, plus Mix 4 reading Main with a higher
    // number and Main previewing Mix 5 with a lower number than its reader.
    add_output_only(&mut bank, 5, src(6));
    add_output_only(&mut bank, 2, MixSource::Mix(MixId::MAIN));
    add_output_only(&mut bank, 3, MixSource::Mix(mix(2)));
    bank.apply(MixBankCommand::SelectPreview {
        mix: MixId::MAIN,
        source: MixSource::Mix(mix(5)),
    })
    .unwrap();

    let (frame0, _) = bank.advance_frame();
    assert_eq!(frame0.frame, 0);
    assert_eq!(frame0.mix(mix(2)).unwrap().primary, MixLayer::Black);
    assert_eq!(frame0.mix(mix(3)).unwrap().primary, MixLayer::Black);
    assert_eq!(
        frame0.mix(MixId::MAIN).unwrap().preview,
        Some(MixLayer::Black)
    );

    let mut history = vec![frame0];
    for expected in 1..=4_u64 {
        let (frame, _) = bank.advance_frame();
        assert_eq!(frame.frame, expected);
        assert_eq!(
            frame.mix(mix(2)).unwrap().primary,
            MixLayer::Mix {
                mix: MixId::MAIN,
                frame: expected - 1,
            }
        );
        assert_eq!(
            frame.mix(mix(3)).unwrap().primary,
            MixLayer::Mix {
                mix: mix(2),
                frame: expected - 1,
            }
        );
        // Order-independent: Main (mix 1) reads Mix 5 from the previous frame too.
        assert_eq!(
            frame.mix(MixId::MAIN).unwrap().preview,
            Some(MixLayer::Mix {
                mix: mix(5),
                frame: expected - 1,
            })
        );
        history.push(frame);
    }

    // Chain latency: Mix 3 at frame 4 sees Mix 2's frame 3, which itself
    // shows Main's frame 2.
    let MixLayer::Mix {
        mix: upstream,
        frame: upstream_frame,
    } = history[4].mix(mix(3)).unwrap().primary
    else {
        panic!("mix 3 reads a mix");
    };
    assert_eq!((upstream, upstream_frame), (mix(2), 3));
    let upstream_frame = usize::try_from(upstream_frame).unwrap();
    assert_eq!(
        history[upstream_frame].mix(mix(2)).unwrap().primary,
        MixLayer::Mix {
            mix: MixId::MAIN,
            frame: 2,
        }
    );
    assert_eq!(
        history[2].mix(MixId::MAIN).unwrap().primary,
        MixLayer::Input(input(1))
    );
}

#[test]
fn transitions_between_mix_sources_resolve_both_layers_to_previous_frame() {
    let mut bank = bank();
    add_output_only(&mut bank, 2, src(3));
    add_pp(
        &mut bank,
        4,
        MixSource::Mix(MixId::MAIN),
        MixSource::Mix(mix(2)),
    );
    bank.advance_frame();
    bank.apply(MixBankCommand::Transition {
        mix: mix(4),
        kind: TransitionKind::Fade,
        duration_frames: 3,
    })
    .unwrap();
    let (frame, _) = bank.advance_frame();
    let m4 = frame.mix(mix(4)).unwrap();
    assert_eq!(
        m4.primary,
        MixLayer::Mix {
            mix: MixId::MAIN,
            frame: 0,
        }
    );
    assert_eq!(
        m4.secondary,
        Some(MixLayer::Mix {
            mix: mix(2),
            frame: 0,
        })
    );
}

#[test]
fn overlay_effect_applies_to_all_targets_at_the_next_boundary() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));
    add_output_only(&mut bank, 7, src(5));
    let channel = OverlayChannelId::new(2).unwrap();
    let targets = MixSet::from_mixes([mix(2), mix(7), mix(7)]);
    assert_eq!(targets.len(), 2);

    let (_, _) = bank.advance_frame();
    assert_eq!(
        bank.schedule_effect(
            MixEffect::OverlayOn {
                channel,
                source: input(6),
            },
            targets,
        ),
        Ok(MixBankEvent::EffectScheduled {
            effect: MixEffect::OverlayOn {
                channel,
                source: input(6),
            },
            targets,
            apply_frame: 1,
        })
    );
    // Not visible until the boundary.
    assert_eq!(bank.mix(mix(2)).unwrap().overlay(channel), None);
    assert_eq!(bank.pending_effects(), 1);

    let (frame1, events) = bank.advance_frame();
    assert_eq!(frame1.frame, 1);
    for target in [mix(2), mix(7)] {
        assert_eq!(
            frame1.mix(target).unwrap().overlays[channel.index()],
            Some(input(6))
        );
    }
    assert_eq!(frame1.mix(MixId::MAIN).unwrap().overlays, [None; 8]);
    assert_eq!(
        events.last(),
        Some(&MixBankEvent::EffectApplied {
            effect: MixEffect::OverlayOn {
                channel,
                source: input(6),
            },
            targets,
            frame: 1,
        })
    );
    assert_eq!(bank.pending_effects(), 0);

    bank.schedule_effect(MixEffect::OverlayOff { channel }, MixSet::single(mix(7)))
        .unwrap();
    let (frame2, _) = bank.advance_frame();
    assert_eq!(
        frame2.mix(mix(2)).unwrap().overlays[channel.index()],
        Some(input(6))
    );
    assert_eq!(frame2.mix(mix(7)).unwrap().overlays[channel.index()], None);
}

#[test]
fn effect_scheduling_validates_targets_and_bounds() {
    let mut bank = bank();
    let channel = OverlayChannelId::new(1).unwrap();
    assert_eq!(
        bank.schedule_effect(MixEffect::OverlayOff { channel }, MixSet::EMPTY),
        Err(MixBankError::EmptyTargetSet)
    );
    assert_eq!(
        bank.schedule_effect(
            MixEffect::OverlayOff { channel },
            MixSet::from_mixes([MixId::MAIN, mix(9)]),
        ),
        Err(MixBankError::UnknownMix(mix(9)))
    );
    assert_eq!(
        bank.schedule_effect(
            MixEffect::OverlayOn {
                channel,
                source: input(42),
            },
            MixSet::single(MixId::MAIN),
        ),
        Err(MixBankError::UnknownInput(input(42)))
    );
    for _ in 0..MAX_PENDING_MIX_EFFECTS {
        bank.schedule_effect(
            MixEffect::OverlayOff { channel },
            MixSet::single(MixId::MAIN),
        )
        .unwrap();
    }
    assert_eq!(
        bank.schedule_effect(
            MixEffect::OverlayOff { channel },
            MixSet::single(MixId::MAIN)
        ),
        Err(MixBankError::PendingEffectsFull {
            maximum: MAX_PENDING_MIX_EFFECTS,
        })
    );
    assert_eq!(bank.pending_effects(), MAX_PENDING_MIX_EFFECTS);
}

#[test]
fn stinger_effect_starts_synchronized_transitions_in_every_target() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));
    add_pp(&mut bank, 3, src(5), src(6));
    let slot = StingerSlotId::new(3).unwrap();
    let targets = MixSet::from_mixes([MixId::MAIN, mix(3)]);
    let effect = MixEffect::Stinger {
        slot,
        duration_frames: 2,
    };
    bank.schedule_effect(effect, targets).unwrap();

    let (frame0, events0) = bank.advance_frame();
    for target in [MixId::MAIN, mix(3)] {
        let composed = frame0.mix(target).unwrap();
        assert_eq!(
            composed.transition_kind,
            Some(TransitionKind::Stinger(slot))
        );
        assert_eq!((composed.mix_numerator, composed.mix_denominator), (0, 2));
    }
    assert_eq!(frame0.mix(mix(2)).unwrap().transition_kind, None);
    assert_eq!(
        events0.last(),
        Some(&MixBankEvent::EffectApplied {
            effect,
            targets,
            frame: 0,
        })
    );

    let (_, events1) = bank.advance_frame();
    let completed: Vec<_> = events1
        .iter()
        .filter_map(|event| match event {
            MixBankEvent::TransitionCompleted { mix, program, .. } => Some((*mix, *program)),
            _ => None,
        })
        .collect();
    assert_eq!(completed, vec![(MixId::MAIN, src(2)), (mix(3), src(6))]);
}

#[test]
fn stinger_effect_is_all_or_nothing_at_the_boundary() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));
    add_output_only(&mut bank, 3, src(5));
    let slot = StingerSlotId::new(1).unwrap();
    let effect = MixEffect::Stinger {
        slot,
        duration_frames: 5,
    };

    // Output-only targets cannot run a Preview-to-Program stinger.
    assert_eq!(
        bank.schedule_effect(effect, MixSet::from_mixes([mix(2), mix(3)])),
        Err(MixBankError::PreviewUnavailable(mix(3)))
    );

    // Valid when scheduled, invalidated before the boundary by a transition on Mix 2.
    let targets = MixSet::from_mixes([MixId::MAIN, mix(2)]);
    bank.schedule_effect(effect, targets).unwrap();
    bank.apply(MixBankCommand::Transition {
        mix: mix(2),
        kind: TransitionKind::Fade,
        duration_frames: 10,
    })
    .unwrap();
    let (frame, events) = bank.advance_frame();
    assert_eq!(
        events,
        vec![MixBankEvent::EffectRejected {
            effect,
            targets,
            frame: 0,
            error: MixBankError::TransitionInProgress(mix(2)),
        }]
    );
    // Main was a valid target but stays untouched.
    assert!(bank.mix(MixId::MAIN).unwrap().transition().is_none());
    assert_eq!(frame.mix(MixId::MAIN).unwrap().transition_kind, None);
}

#[test]
fn removed_target_rejects_pending_effect() {
    let mut bank = bank();
    add_pp(&mut bank, 2, src(3), src(4));
    let channel = OverlayChannelId::new(1).unwrap();
    let effect = MixEffect::OverlayOn {
        channel,
        source: input(5),
    };
    let targets = MixSet::from_mixes([MixId::MAIN, mix(2)]);
    bank.schedule_effect(effect, targets).unwrap();
    bank.apply(MixBankCommand::RemoveMix(mix(2))).unwrap();
    let (_, events) = bank.advance_frame();
    assert_eq!(
        events,
        vec![MixBankEvent::EffectRejected {
            effect,
            targets,
            frame: 0,
            error: MixBankError::UnknownMix(mix(2)),
        }]
    );
    assert_eq!(bank.mix(MixId::MAIN).unwrap().overlay(channel), None);
}

#[test]
fn identical_command_sequences_produce_identical_frames() {
    fn run() -> Vec<(fm_switcher::MixBankFrame, Vec<MixBankEvent>)> {
        let mut bank = bank();
        add_output_only(&mut bank, 2, MixSource::Mix(MixId::MAIN));
        add_pp(&mut bank, 3, MixSource::Mix(mix(2)), src(4));
        bank.apply(MixBankCommand::Transition {
            mix: mix(3),
            kind: TransitionKind::Slide,
            duration_frames: 3,
        })
        .unwrap();
        bank.schedule_effect(
            MixEffect::OverlayOn {
                channel: OverlayChannelId::new(4).unwrap(),
                source: input(6),
            },
            MixSet::from_mixes([mix(2), mix(3)]),
        )
        .unwrap();
        (0..6).map(|_| bank.advance_frame()).collect()
    }
    assert_eq!(run(), run());
}
