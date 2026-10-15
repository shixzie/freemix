//! SW-012: four independent logical outputs with clean feeds, overlay subsets,
//! audio maps, multiview composition, and routing validation.

use std::num::NonZeroU128;

use fm_switcher::{
    AudioMap, AudioSource, BackgroundPlan, CellFeed, FadeToBlackPosition, FeedLayer,
    LOGICAL_OUTPUT_COUNT, LogicalOutputConfig, LogicalOutputSlot, LogicalOutputs,
    MAX_OUTPUT_MULTIVIEW_CELLS, MixBank, MixBankCommand, MixBankFrame, MixConfig, MixId, MixSource,
    MultiViewCell, OutputRenderPlan, OutputRoutingError, OutputRoutingFrame, OutputSource,
    OutputVideo, OverlayChannelId, OverlayPolicy, OverlaySet, RoutingContext, SwitcherCommand,
    SwitcherState, TransitionKind,
};
use fm_types::{BusId, InputId, OutputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn output(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(value).unwrap())
}

fn bus(value: u128) -> BusId {
    BusId::new(NonZeroU128::new(value).unwrap())
}

fn slot(number: u8) -> LogicalOutputSlot {
    LogicalOutputSlot::new(number).unwrap()
}

fn channel(number: u8) -> OverlayChannelId {
    OverlayChannelId::new(number).unwrap()
}

fn mix(number: u8) -> MixId {
    MixId::new(number).unwrap()
}

fn switcher() -> SwitcherState {
    SwitcherState::new((1..=6).map(input).collect(), input(1), input(2)).unwrap()
}

/// Bank whose Mix 2 shows input 5 on Program and input 6 on Preview.
fn bank_frame() -> MixBankFrame {
    let mut bank = MixBank::new((1..=6).map(input).collect(), input(1), input(2)).unwrap();
    bank.apply(MixBankCommand::AddMix {
        mix: mix(2),
        config: MixConfig::PreviewProgram {
            program: MixSource::Input(input(5)),
            preview: MixSource::Input(input(6)),
        },
    })
    .unwrap();
    bank.advance_frame().0
}

const BUSES: [u128; 2] = [10, 11];

fn buses() -> Vec<BusId> {
    BUSES.into_iter().map(bus).collect()
}

fn resolve(
    routing: &LogicalOutputs,
    switcher: &SwitcherState,
    mixes: Option<&MixBankFrame>,
) -> OutputRoutingFrame {
    let buses = buses();
    routing.resolve(&RoutingContext {
        switcher,
        mixes,
        buses: &buses,
    })
}

fn plan(frame: &OutputRoutingFrame, number: u8) -> &OutputRenderPlan {
    frame
        .output(slot(number))
        .expect("slot is enabled")
        .as_ref()
        .expect("slot resolves")
}

fn background(plan: &OutputRenderPlan) -> BackgroundPlan {
    match &plan.video {
        OutputVideo::Background(background) => *background,
        OutputVideo::MultiView(_) => panic!("expected a background feed"),
    }
}

fn overlay_channels(plan: &OutputRenderPlan) -> Vec<u8> {
    plan.overlays
        .iter()
        .map(|layer| layer.channel.number())
        .collect()
}

fn settled(layer: FeedLayer) -> BackgroundPlan {
    BackgroundPlan {
        primary: layer,
        secondary: None,
        transition_kind: None,
        mix_numerator: 0,
        mix_denominator: 1,
        fade_to_black: FadeToBlackPosition::LIVE,
    }
}

/// Program with overlays, clean Preview, an input with overlay 2 only, and Mix 2.
fn four_outputs() -> LogicalOutputs {
    let mut routing = LogicalOutputs::new();
    routing
        .configure(slot(1), Some(LogicalOutputConfig::program(output(1))))
        .unwrap();
    routing
        .configure(
            slot(2),
            Some(
                LogicalOutputConfig::program(output(2))
                    .with_source(OutputSource::Preview)
                    .with_overlays(OverlayPolicy::Clean)
                    .with_audio(AudioMap::FollowVideo),
            ),
        )
        .unwrap();
    routing
        .configure(
            slot(3),
            Some(
                LogicalOutputConfig::program(output(3))
                    .with_source(OutputSource::Input(input(4)))
                    .with_overlays(OverlayPolicy::Channels(OverlaySet::from_channels([
                        channel(2),
                    ])))
                    .with_audio(AudioMap::Bus(bus(11))),
            ),
        )
        .unwrap();
    routing
        .configure(
            slot(4),
            Some(
                LogicalOutputConfig::program(output(4))
                    .with_source(OutputSource::Mix(mix(2)))
                    .with_audio(AudioMap::FollowVideo),
            ),
        )
        .unwrap();
    routing
}

fn with_two_overlays() -> SwitcherState {
    let mut switcher = switcher();
    switcher.take_overlay(channel(1), input(5)).unwrap();
    switcher.take_overlay(channel(2), input(6)).unwrap();
    switcher
}

#[test]
fn four_outputs_select_sources_independently() {
    assert_eq!(LOGICAL_OUTPUT_COUNT, 4);
    assert_eq!(LogicalOutputSlot::new(0), None);
    assert_eq!(LogicalOutputSlot::new(5), None);

    let routing = four_outputs();
    let switcher = switcher();
    let mixes = bank_frame();
    let frame = resolve(&routing, &switcher, Some(&mixes));

    assert_eq!(frame.render_order, LogicalOutputSlot::ALL.to_vec());
    assert_eq!(plan(&frame, 1).output, output(1));
    assert_eq!(
        background(plan(&frame, 1)),
        settled(FeedLayer::Input(input(1)))
    );
    assert_eq!(
        background(plan(&frame, 2)),
        settled(FeedLayer::Input(input(2)))
    );
    assert_eq!(
        background(plan(&frame, 3)),
        settled(FeedLayer::Input(input(4)))
    );
    assert_eq!(
        background(plan(&frame, 4)),
        settled(FeedLayer::Input(input(5)))
    );

    assert_eq!(plan(&frame, 1).audio, AudioSource::Master);
    assert_eq!(plan(&frame, 2).audio, AudioSource::Input(input(2)));
    assert_eq!(plan(&frame, 3).audio, AudioSource::Bus(bus(11)));
    assert_eq!(plan(&frame, 4).audio, AudioSource::Input(input(5)));
}

#[test]
fn changing_one_output_leaves_the_others_untouched() {
    let mut routing = four_outputs();
    let switcher = with_two_overlays();
    let mixes = bank_frame();
    let before = resolve(&routing, &switcher, Some(&mixes));

    routing
        .set_source(slot(2), OutputSource::Input(input(6)))
        .unwrap();
    routing
        .set_overlay_policy(slot(2), OverlayPolicy::All)
        .unwrap();
    routing.set_audio_map(slot(2), AudioMap::Master).unwrap();
    let after = resolve(&routing, &switcher, Some(&mixes));

    for number in [1, 3, 4] {
        assert_eq!(plan(&before, number), plan(&after, number));
    }
    assert_eq!(
        background(plan(&after, 2)),
        settled(FeedLayer::Input(input(6)))
    );
    assert_eq!(overlay_channels(plan(&after, 2)), vec![1, 2]);
    assert_eq!(plan(&after, 2).audio, AudioSource::Master);
}

#[test]
fn clean_feed_excludes_overlays_while_other_outputs_keep_their_selection() {
    let mut routing = four_outputs();
    routing
        .configure(
            slot(4),
            Some(LogicalOutputConfig::program(output(4)).with_overlays(OverlayPolicy::Routed)),
        )
        .unwrap();
    let mut switcher = with_two_overlays();
    let _ = switcher.set_overlay_output_inclusion(channel(1), output(4), true);

    let frame = resolve(&routing, &switcher, None);
    assert_eq!(overlay_channels(plan(&frame, 1)), vec![1, 2]);
    assert_eq!(plan(&frame, 1).overlays[0].source, input(5));
    assert_eq!(plan(&frame, 1).overlays[0].opacity, u8::MAX);
    assert_eq!(plan(&frame, 2).overlays.len(), 0, "clean feed");
    assert_eq!(overlay_channels(plan(&frame, 3)), vec![2]);
    assert_eq!(overlay_channels(plan(&frame, 4)), vec![1]);

    // Overlays turned off disappear from every output, including the routed one.
    switcher.overlay_off(channel(1)).unwrap();
    let frame = resolve(&routing, &switcher, None);
    assert_eq!(overlay_channels(plan(&frame, 1)), vec![2]);
    assert_eq!(plan(&frame, 4).overlays.len(), 0);
}

#[test]
fn cut_moves_program_and_preview_outputs_but_not_fixed_inputs() {
    let routing = four_outputs();
    let mut switcher = with_two_overlays();
    let mixes = bank_frame();
    let before = resolve(&routing, &switcher, Some(&mixes));

    switcher.apply(SwitcherCommand::Cut).unwrap();
    let after = resolve(&routing, &switcher, Some(&mixes));

    assert_eq!(
        background(plan(&after, 1)),
        settled(FeedLayer::Input(input(2)))
    );
    assert_eq!(
        background(plan(&after, 2)),
        settled(FeedLayer::Input(input(1)))
    );
    assert_eq!(plan(&after, 2).audio, AudioSource::Input(input(1)));
    assert_eq!(plan(&before, 3), plan(&after, 3));
    assert_eq!(plan(&before, 4), plan(&after, 4));
    assert_eq!(plan(&after, 1).overlays, plan(&before, 1).overlays);
}

#[test]
fn timed_transition_and_fade_to_black_reach_only_program_feeds() {
    let mut routing = four_outputs();
    routing
        .configure(
            slot(4),
            Some(
                LogicalOutputConfig::program(output(4))
                    .with_source(OutputSource::Mix(MixId::MAIN))
                    .with_overlays(OverlayPolicy::Clean),
            ),
        )
        .unwrap();
    let mut switcher = switcher();
    switcher
        .apply(SwitcherCommand::Transition {
            kind: TransitionKind::Fade,
            duration_frames: 4,
        })
        .unwrap();
    switcher.request_fade_to_black(true, 2).unwrap();
    let _ = switcher.advance_frame_events();

    let frame = resolve(&routing, &switcher, None);
    let program = background(plan(&frame, 1));
    assert_eq!(program.primary, FeedLayer::Input(input(1)));
    assert_eq!(program.secondary, Some(FeedLayer::Input(input(2))));
    assert_eq!(program.transition_kind, Some(TransitionKind::Fade));
    assert_eq!((program.mix_numerator, program.mix_denominator), (1, 4));
    assert_ne!(program.fade_to_black, FadeToBlackPosition::LIVE);
    // The clean Main-mix feed shows the same moving background.
    assert_eq!(background(plan(&frame, 4)), program);
    // Preview and fixed-input feeds are settled and never faded.
    assert_eq!(
        background(plan(&frame, 2)),
        settled(FeedLayer::Input(input(2)))
    );
    assert_eq!(
        background(plan(&frame, 3)),
        settled(FeedLayer::Input(input(4)))
    );

    for _ in 0..3 {
        let _ = switcher.advance_frame_events();
    }
    let frame = resolve(&routing, &switcher, None);
    let program = background(plan(&frame, 1));
    assert_eq!(program.primary, FeedLayer::Input(input(2)));
    assert_eq!(program.secondary, None);
    assert_eq!(program.fade_to_black, FadeToBlackPosition::BLACK);
    assert_eq!(
        background(plan(&frame, 2)),
        settled(FeedLayer::Input(input(1)))
    );
}

#[test]
fn secondary_mix_output_follows_its_own_transition_and_overlays() {
    let mut bank = MixBank::new((1..=6).map(input).collect(), input(1), input(2)).unwrap();
    bank.apply(MixBankCommand::AddMix {
        mix: mix(2),
        config: MixConfig::OutputOnly {
            program: MixSource::Input(input(5)),
        },
    })
    .unwrap();
    bank.apply(MixBankCommand::AddMix {
        mix: mix(3),
        config: MixConfig::OutputOnly {
            program: MixSource::Mix(mix(2)),
        },
    })
    .unwrap();
    bank.apply(MixBankCommand::TransitionTo {
        mix: mix(2),
        source: MixSource::Input(input(6)),
        kind: TransitionKind::Wipe,
        duration_frames: 2,
    })
    .unwrap();
    let (first, _) = bank.advance_frame();

    let mut routing = LogicalOutputs::new();
    routing
        .configure(
            slot(1),
            Some(
                LogicalOutputConfig::program(output(1))
                    .with_source(OutputSource::Mix(mix(2)))
                    .with_audio(AudioMap::FollowVideo),
            ),
        )
        .unwrap();
    routing
        .configure(
            slot(2),
            Some(
                LogicalOutputConfig::program(output(2))
                    .with_source(OutputSource::Mix(mix(3)))
                    .with_audio(AudioMap::FollowVideo),
            ),
        )
        .unwrap();
    let switcher = switcher();

    let frame = resolve(&routing, &switcher, Some(&first));
    let mix_two = background(plan(&frame, 1));
    assert_eq!(mix_two.primary, FeedLayer::Input(input(5)));
    assert_eq!(mix_two.secondary, Some(FeedLayer::Input(input(6))));
    assert_eq!(mix_two.transition_kind, Some(TransitionKind::Wipe));
    assert_eq!(plan(&frame, 1).audio, AudioSource::Input(input(5)));
    // Bank frame zero has no previous Mix 2 frame to read.
    assert_eq!(background(plan(&frame, 2)).primary, FeedLayer::Black);
    assert_eq!(plan(&frame, 2).audio, AudioSource::Silent);

    let (second, _) = bank.advance_frame();
    let frame = resolve(&routing, &switcher, Some(&second));
    assert_eq!(
        background(plan(&frame, 2)).primary,
        FeedLayer::Mix {
            mix: mix(2),
            frame: 0
        }
    );
    // Audio follows the mix chain to the input Mix 2 currently shows.
    assert_eq!(plan(&frame, 2).audio, AudioSource::Input(input(5)));
}

#[test]
fn multiview_composes_cells_and_renders_after_referenced_outputs() {
    let mut routing = four_outputs();
    routing
        .set_source(
            slot(1),
            OutputSource::MultiView(vec![
                MultiViewCell::Program,
                MultiViewCell::Output(slot(3)),
                MultiViewCell::Input(input(6)),
                MultiViewCell::Mix(mix(2)),
            ]),
        )
        .unwrap();
    routing
        .set_audio_map(slot(1), AudioMap::FollowVideo)
        .unwrap();
    let switcher = with_two_overlays();
    let mixes = bank_frame();
    let frame = resolve(&routing, &switcher, Some(&mixes));

    assert_eq!(frame.render_order, vec![slot(2), slot(3), slot(1), slot(4)]);
    let multiview = plan(&frame, 1);
    assert_eq!(multiview.overlays.len(), 0);
    assert_eq!(multiview.audio, AudioSource::Silent);
    let OutputVideo::MultiView(cells) = &multiview.video else {
        panic!("expected a MultiView");
    };
    let feeds: Vec<CellFeed> = cells.iter().map(|cell| cell.feed).collect();
    assert_eq!(
        feeds,
        vec![
            CellFeed::Background(settled(FeedLayer::Input(input(1)))),
            CellFeed::Output(slot(3)),
            CellFeed::Background(settled(FeedLayer::Input(input(6)))),
            CellFeed::Background(settled(FeedLayer::Input(input(5)))),
        ]
    );
    // The referenced output keeps its own overlay selection inside the cell.
    assert_eq!(overlay_channels(plan(&frame, 3)), vec![2]);
}

#[test]
fn multiview_recursion_is_rejected_atomically() {
    let mut routing = four_outputs();
    let original = routing.clone();

    assert_eq!(
        routing.set_source(
            slot(1),
            OutputSource::MultiView(vec![MultiViewCell::Output(slot(1))])
        ),
        Err(OutputRoutingError::MultiViewRecursion {
            output: slot(1),
            via: slot(1)
        })
    );
    assert_eq!(routing, original);

    routing
        .set_source(
            slot(2),
            OutputSource::MultiView(vec![MultiViewCell::Output(slot(1))]),
        )
        .unwrap();
    routing
        .set_source(
            slot(3),
            OutputSource::MultiView(vec![MultiViewCell::Output(slot(2))]),
        )
        .unwrap();
    let chained = routing.clone();
    assert_eq!(
        routing.set_source(
            slot(1),
            OutputSource::MultiView(vec![MultiViewCell::Program, MultiViewCell::Output(slot(3))])
        ),
        Err(OutputRoutingError::MultiViewRecursion {
            output: slot(1),
            via: slot(3)
        })
    );
    assert_eq!(routing, chained);
    let frame = resolve(&routing, &switcher(), Some(&bank_frame()));
    assert_eq!(frame.render_order, vec![slot(1), slot(2), slot(3), slot(4)]);
}

#[test]
fn structural_validation_rejects_malformed_routing() {
    let mut routing = four_outputs();
    let original = routing.clone();

    assert_eq!(
        routing.configure(slot(2), Some(LogicalOutputConfig::program(output(1)))),
        Err(OutputRoutingError::DuplicateOutputId {
            slot: slot(2),
            output: output(1),
            existing: slot(1)
        })
    );
    assert_eq!(
        routing.set_source(slot(2), OutputSource::MultiView(Vec::new())),
        Err(OutputRoutingError::EmptyMultiView(slot(2)))
    );
    assert_eq!(
        routing.set_source(
            slot(2),
            OutputSource::MultiView(vec![MultiViewCell::Preview, MultiViewCell::Preview])
        ),
        Err(OutputRoutingError::DuplicateMultiViewCell {
            slot: slot(2),
            cell: MultiViewCell::Preview
        })
    );
    let too_many = (1..=u128::try_from(MAX_OUTPUT_MULTIVIEW_CELLS).unwrap() + 1)
        .map(|value| MultiViewCell::Input(input(value)))
        .collect();
    assert_eq!(
        routing.set_source(slot(2), OutputSource::MultiView(too_many)),
        Err(OutputRoutingError::MultiViewCellLimit {
            slot: slot(2),
            cells: MAX_OUTPUT_MULTIVIEW_CELLS + 1,
            maximum: MAX_OUTPUT_MULTIVIEW_CELLS
        })
    );
    assert_eq!(routing, original);

    routing.configure(slot(4), None).unwrap();
    assert_eq!(
        routing.set_source(
            slot(1),
            OutputSource::MultiView(vec![MultiViewCell::Output(slot(4))])
        ),
        Err(OutputRoutingError::UnknownOutput {
            slot: slot(1),
            referenced: slot(4)
        })
    );
    assert_eq!(
        routing.set_audio_map(slot(4), AudioMap::Master),
        Err(OutputRoutingError::OutputDisabled(slot(4)))
    );

    // An output shown by a MultiView cannot be disabled underneath it.
    routing
        .set_source(
            slot(1),
            OutputSource::MultiView(vec![MultiViewCell::Output(slot(3))]),
        )
        .unwrap();
    assert_eq!(
        routing.configure(slot(3), None),
        Err(OutputRoutingError::UnknownOutput {
            slot: slot(1),
            referenced: slot(3)
        })
    );
    assert!(routing.slot(slot(3)).is_some());
}

#[test]
fn live_validation_fails_only_the_affected_outputs() {
    let mut routing = four_outputs();
    routing
        .set_source(slot(3), OutputSource::Input(input(99)))
        .unwrap();
    routing
        .set_audio_map(slot(2), AudioMap::Bus(bus(77)))
        .unwrap();
    routing
        .set_source(
            slot(1),
            OutputSource::MultiView(vec![MultiViewCell::Program, MultiViewCell::Output(slot(3))]),
        )
        .unwrap();
    let switcher = switcher();

    // No bank frame: Mix 2 is unknown.
    let frame = resolve(&routing, &switcher, None);
    assert_eq!(
        frame.output(slot(3)),
        Some(&Err(OutputRoutingError::UnknownInput {
            slot: slot(3),
            input: input(99)
        }))
    );
    assert_eq!(
        frame.output(slot(2)),
        Some(&Err(OutputRoutingError::UnknownBus {
            slot: slot(2),
            bus: bus(77)
        }))
    );
    assert_eq!(
        frame.output(slot(4)),
        Some(&Err(OutputRoutingError::UnknownMix {
            slot: slot(4),
            mix: mix(2)
        }))
    );
    assert_eq!(
        frame.output(slot(1)),
        Some(&Err(OutputRoutingError::DependentOutputUnavailable {
            slot: slot(1),
            referenced: slot(3)
        }))
    );

    // Fixing slot 3 and supplying the bank heals exactly those outputs.
    routing
        .set_source(slot(3), OutputSource::Input(input(4)))
        .unwrap();
    let mixes = bank_frame();
    let frame = resolve(&routing, &switcher, Some(&mixes));
    for number in [1, 3, 4] {
        assert!(frame.output(slot(number)).unwrap().is_ok());
    }
    assert!(frame.output(slot(2)).unwrap().is_err());
    let error = frame.output(slot(2)).unwrap().as_ref().unwrap_err();
    assert_eq!(error.to_string(), "output 2 routes unknown bus 77");
}

#[test]
fn disabled_slots_produce_no_plan_and_resolution_is_deterministic() {
    let mut routing = LogicalOutputs::new();
    let switcher = with_two_overlays();
    let empty = resolve(&routing, &switcher, None);
    assert!(empty.outputs.iter().all(Option::is_none));
    assert_eq!(empty.render_order.len(), 0);

    routing
        .configure(
            slot(3),
            Some(LogicalOutputConfig::program(output(9)).with_overlays(OverlayPolicy::Clean)),
        )
        .unwrap();
    let first = resolve(&routing, &switcher, None);
    let second = resolve(&routing, &switcher, None);
    assert_eq!(first, second);
    assert_eq!(first.render_order, vec![slot(3)]);
    assert!(first.output(slot(1)).is_none());
    assert_eq!(routing.enabled().count(), 1);
}

#[test]
fn fade_to_black_fades_only_the_outputs_it_affects() {
    let mut routing = four_outputs();
    routing
        .configure(
            slot(4),
            Some(
                LogicalOutputConfig::program(output(4))
                    .with_source(OutputSource::MultiView(vec![MultiViewCell::Program])),
            ),
        )
        .unwrap();
    routing
        .configure(
            slot(2),
            Some(LogicalOutputConfig::program(output(2)).with_overlays(OverlayPolicy::Clean)),
        )
        .unwrap();
    let mut switcher = switcher();
    assert!(switcher.fade_to_black_affects(output(2)));
    let events = switcher
        .apply(SwitcherCommand::SetFadeToBlackOutput {
            output: output(2),
            affected: false,
        })
        .unwrap();
    assert_eq!(
        events,
        vec![fm_switcher::SwitcherEvent::FadeToBlackOutputChanged {
            output: output(2),
            affected: false,
        }]
    );
    let _ = switcher
        .apply(SwitcherCommand::SetFadeToBlackOutput {
            output: output(4),
            affected: false,
        })
        .unwrap();
    switcher.request_fade_to_black(true, 1).unwrap();
    let _ = switcher.advance_frame_events();
    assert_eq!(
        switcher.fade_to_black_position(),
        FadeToBlackPosition::BLACK
    );

    let frame = resolve(&routing, &switcher, None);
    // Output 1 is affected: Program is black.
    assert_eq!(
        background(plan(&frame, 1)).fade_to_black,
        FadeToBlackPosition::BLACK
    );
    // Output 2 shows the same Program, live.
    assert_eq!(
        background(plan(&frame, 2)),
        settled(FeedLayer::Input(input(1)))
    );
    // A multiview's Program cell follows its own output's selection.
    let OutputVideo::MultiView(cells) = &plan(&frame, 4).video else {
        panic!("expected a multiview");
    };
    assert_eq!(
        cells[0].feed,
        CellFeed::Background(settled(FeedLayer::Input(input(1))))
    );
    assert_eq!(
        switcher.fade_to_black_frame_for(output(2)),
        fm_switcher::FadeToBlackFrame::LIVE
    );
    assert_eq!(
        switcher.fade_to_black_frame_for(output(1)),
        switcher.fade_to_black_frame()
    );

    // Re-including the output fades it too.
    let _ = switcher
        .apply(SwitcherCommand::SetFadeToBlackOutput {
            output: output(2),
            affected: true,
        })
        .unwrap();
    assert_eq!(
        switcher
            .fade_to_black_excluded_outputs()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![output(4)]
    );
    let frame = resolve(&routing, &switcher, None);
    assert_eq!(
        background(plan(&frame, 2)).fade_to_black,
        FadeToBlackPosition::BLACK
    );
}
