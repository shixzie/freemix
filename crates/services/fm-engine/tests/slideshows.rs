//! IN-009 in the engine: slideshow inputs advance once per frame, change
//! slides on exact frames, take paged navigation as live operations
//! realized on their target frame, follow folder listings without dropping
//! the slide on air, and replay identically.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{
    Engine, EngineCommand, EngineCommandKind, EngineEvent, EngineInputLifecycleEdit, EngineSlide,
    EngineSlideshowError, EngineSlideshowFrame, ShowState,
};
use fm_model::InputSourceSpec;
use fm_types::paged::PagedNavigation;
use fm_types::still::{
    SlideTransitionStyle, SlideshowConfig, SlideshowItem, SlideshowPage, SlideshowSource,
};
use fm_types::{FrameRate, InputId};

const SHOW: u128 = 2;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn list(names: &[&str]) -> SlideshowSource {
    SlideshowSource::List(
        names
            .iter()
            .map(|name| SlideshowItem::new(format!("asset://{name}")))
            .collect(),
    )
}

fn cut_config(source: SlideshowSource) -> SlideshowConfig {
    SlideshowConfig {
        slide_frames: 3,
        transition: SlideTransitionStyle::Cut,
        ..SlideshowConfig::new(source)
    }
}

fn engine_with(config: SlideshowConfig) -> Engine {
    let mut show = ShowState::new(
        "slides",
        vec![(input(1), "Camera".into()), (input(SHOW), "Slides".into())],
        input(1),
        input(SHOW),
    )
    .unwrap();
    show.set_input_source(input(SHOW), InputSourceSpec::Slideshow(config))
        .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> fm_engine::EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn navigate(engine: &mut Engine, key: &str, navigation: PagedNavigation) {
    let outcome = run(
        engine,
        key,
        EngineCommand::PageNavigate {
            input: input(SHOW),
            navigation,
        },
    );
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
}

fn step(engine: &mut Engine) -> EngineSlideshowFrame {
    engine
        .tick()
        .unwrap()
        .slideshows
        .into_iter()
        .find(|frame| frame.input == input(SHOW))
        .expect("the slideshow reports every frame")
}

fn pages(engine: &mut Engine, frames: usize) -> Vec<u32> {
    (0..frames).map(|_| step(engine).page.index).collect()
}

#[test]
fn a_list_slideshow_holds_each_slide_for_its_frames_and_loops() {
    let mut engine = engine_with(cut_config(list(&["a.png", "b.jpg", "c.webp"])));
    assert!(engine.slideshows_advancing());
    let first = step(&mut engine);
    assert_eq!(first.page, SlideshowPage { index: 0, count: 3 });
    assert_eq!(first.label, "a.png");
    assert!(first.changed);
    assert!(first.outgoing.is_none());
    assert_eq!(
        first.incoming.unwrap().slide.asset_uri.as_ref(),
        "asset://a.png"
    );
    let second = step(&mut engine);
    assert!(!second.changed);
    assert_eq!(pages(&mut engine, 10), [0, 1, 1, 1, 2, 2, 2, 0, 0, 0]);
    let status = engine.slideshow_status(input(SHOW)).unwrap();
    assert_eq!(status.page, SlideshowPage { index: 0, count: 3 });
    assert_eq!(status.label, "a.png");
    assert!(engine.slideshow_status(input(1)).is_none());
}

#[test]
fn a_fade_mixes_outgoing_and_incoming_slides_on_exact_frames() {
    let mut engine = engine_with(SlideshowConfig {
        slide_frames: 4,
        transition: SlideTransitionStyle::Fade,
        transition_frames: 2,
        ..SlideshowConfig::new(list(&["a.png", "b.png"]))
    });
    let frames: Vec<_> = (0..8).map(|_| step(&mut engine)).collect();
    let mixes: Vec<_> = frames
        .iter()
        .map(|frame| {
            (
                frame.incoming.as_ref().unwrap().index,
                frame.outgoing.as_ref().map(|view| view.index),
                frame.mix_basis_points,
            )
        })
        .collect();
    assert!(
        mixes
            .iter()
            .any(|(_, outgoing, mix)| outgoing == &Some(0) && *mix < 10_000)
    );
    assert!(
        mixes
            .iter()
            .all(|(_, outgoing, mix)| outgoing.is_some() || *mix == 10_000)
    );
    // The page changes on the frame the incoming slide starts to show.
    let change = frames
        .iter()
        .position(|frame| frame.page.index == 1)
        .unwrap();
    assert!(frames[change].changed);
    assert!(frames[change].outgoing.is_some());
}

#[test]
fn navigation_is_live_and_realizes_on_its_target_frame() {
    let mut engine = engine_with(SlideshowConfig {
        auto_advance: false,
        ..cut_config(list(&["a.png", "b.png", "c.png", "d.png"]))
    });
    let next = EngineCommand::PageNavigate {
        input: input(SHOW),
        navigation: PagedNavigation::Next,
    };
    assert_eq!(next.kind(), EngineCommandKind::LiveOperation);
    assert_eq!(pages(&mut engine, 4), [0, 0, 0, 0]);
    assert!(!engine.slideshows_advancing());
    let outcome = run(&mut engine, "next", next);
    let frame = outcome
        .receipt
        .accepted()
        .map(|_| ())
        .and(
            outcome
                .events
                .iter()
                .find_map(|event| match &event.payload {
                    EngineEvent::PageNavigated {
                        input: navigated,
                        frame,
                        navigation,
                    } if *navigated == input(SHOW) && *navigation == PagedNavigation::Next => {
                        Some(*frame)
                    }
                    _ => None,
                }),
        );
    assert_eq!(frame, Some(4));
    assert!(engine.slideshows_advancing());
    let realized = step(&mut engine);
    assert_eq!(realized.page.index, 1);
    assert!(realized.changed);
    navigate(&mut engine, "last", PagedNavigation::Last);
    assert_eq!(step(&mut engine).page.index, 3);
    // A looping slideshow wraps; previous from the first goes to the last.
    navigate(&mut engine, "next-wrap", PagedNavigation::Next);
    assert_eq!(step(&mut engine).page.index, 0);
    navigate(&mut engine, "previous-wrap", PagedNavigation::Previous);
    assert_eq!(step(&mut engine).page.index, 3);
    navigate(&mut engine, "goto", PagedNavigation::Goto(2));
    assert_eq!(step(&mut engine).page.index, 2);
    // Go to past the end clamps to the last slide.
    navigate(&mut engine, "goto-far", PagedNavigation::Goto(99));
    assert_eq!(step(&mut engine).page.index, 3);
    navigate(&mut engine, "first", PagedNavigation::First);
    let first = step(&mut engine);
    assert_eq!((first.page.index, first.label.as_str()), (0, "a.png"));
    // Two navigations accepted for one frame both realize, in order.
    navigate(&mut engine, "twice-1", PagedNavigation::Next);
    navigate(&mut engine, "twice-2", PagedNavigation::Next);
    assert_eq!(step(&mut engine).page.index, 2);
}

#[test]
fn navigation_is_refused_for_inputs_that_are_not_paged() {
    let mut engine = engine_with(cut_config(list(&["a.png"])));
    for (key, target) in [("camera", input(1)), ("missing", input(9))] {
        let outcome = run(
            &mut engine,
            key,
            EngineCommand::PageNavigate {
                input: target,
                navigation: PagedNavigation::Next,
            },
        );
        assert!(outcome.receipt.rejected().is_some(), "{key}");
    }
}

#[test]
fn a_non_looping_slideshow_stops_on_its_last_slide() {
    let mut engine = engine_with(SlideshowConfig {
        loop_show: false,
        ..cut_config(list(&["a.png", "b.png"]))
    });
    assert_eq!(pages(&mut engine, 9), [0, 0, 0, 1, 1, 1, 1, 1, 1]);
    navigate(&mut engine, "next", PagedNavigation::Next);
    assert_eq!(step(&mut engine).page.index, 1);
}

#[test]
fn a_folder_slideshow_follows_its_listing_without_dropping_the_slide_on_air() {
    let mut engine = engine_with(cut_config(SlideshowSource::Folder {
        asset_uri: "asset://photos".into(),
    }));
    let empty = step(&mut engine);
    assert_eq!(empty.page, SlideshowPage::default());
    assert!(empty.incoming.is_none());
    assert!(!engine.slideshows_advancing());
    let slide = |name: &str| EngineSlide::new(format!("asset://photos/{name}"), None);
    engine
        .set_slideshow_listing(input(SHOW), vec![slide("1.png"), slide("2.png")])
        .unwrap();
    let listed = step(&mut engine);
    assert_eq!(listed.page, SlideshowPage { index: 0, count: 2 });
    assert_eq!(listed.generation, 1);
    assert!(listed.changed);
    assert_eq!(listed.label, "1.png");
    // Removing the slide on air keeps it on air until it leaves.
    engine
        .set_slideshow_listing(
            input(SHOW),
            vec![slide("0.png"), slide("2.png"), slide("3.png")],
        )
        .unwrap();
    let kept = step(&mut engine);
    assert_eq!(kept.label, "1.png");
    assert_eq!(kept.generation, 2);
    assert!(kept.changed);
    assert_eq!(kept.page.count, 3);
    // The same listing again changes nothing.
    engine
        .set_slideshow_listing(
            input(SHOW),
            vec![slide("0.png"), slide("2.png"), slide("3.png")],
        )
        .unwrap();
    let after: Vec<_> = (0..4).map(|_| step(&mut engine)).collect();
    assert!(after.iter().all(|frame| frame.generation == 2));
    assert!(after.iter().any(|frame| frame.label != "1.png"));
    assert_eq!(engine.slideshow_listing(input(SHOW)).len(), 3);
    assert_eq!(
        engine.set_slideshow_listing(input(1), Vec::new()),
        Err(EngineSlideshowError::NotSlideshowInput(input(1)))
    );
}

#[test]
fn a_list_slideshow_refuses_a_folder_listing() {
    let mut engine = engine_with(cut_config(list(&["a.png"])));
    assert_eq!(
        engine.set_slideshow_listing(input(SHOW), Vec::new()),
        Err(EngineSlideshowError::NotFolderSource(input(SHOW)))
    );
}

#[test]
fn a_paused_slideshow_holds_and_a_settings_change_keeps_the_slide_on_air() {
    let mut engine = engine_with(cut_config(list(&["a.png", "b.png", "c.png"])));
    assert_eq!(pages(&mut engine, 4), [0, 0, 0, 1]);
    let outcome = run(
        &mut engine,
        "pause",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::SetPaused {
            input: input(SHOW),
            paused: true,
        }),
    );
    assert!(outcome.receipt.accepted().is_some());
    assert_eq!(pages(&mut engine, 6), [1, 1, 1, 1, 1, 1]);
    assert!(!engine.slideshows_advancing());
    let outcome = run(
        &mut engine,
        "resume",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::SetPaused {
            input: input(SHOW),
            paused: false,
        }),
    );
    assert!(outcome.receipt.accepted().is_some());
    step(&mut engine);
    // Slower slides, the same list: the slide on air stays.
    let outcome = run(
        &mut engine,
        "slower",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(SHOW),
            source: InputSourceSpec::Slideshow(SlideshowConfig {
                slide_frames: 5,
                ..cut_config(list(&["a.png", "b.png", "c.png"]))
            }),
        }),
    );
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    let held = pages(&mut engine, 6);
    assert_eq!(held[..5], [1; 5]);
    assert_eq!(held[5], 2);
}

#[test]
fn ken_burns_moves_the_crop_every_frame() {
    let mut engine = engine_with(SlideshowConfig {
        ken_burns: true,
        ..cut_config(list(&["a.png", "b.png"]))
    });
    let crops: Vec<_> = (0..3)
        .map(|_| step(&mut engine).incoming.unwrap().crop)
        .collect();
    assert_ne!(crops[0], crops[2]);
    assert!(crops.iter().any(|crop| !crop.is_full()));
    for crop in crops {
        let [x, y, width, height] = crop.fractions();
        assert!(x >= 0.0 && y >= 0.0 && x + width <= 1.0 + 1e-9 && y + height <= 1.0 + 1e-9);
    }
}

#[test]
fn identical_commands_replay_identical_slideshow_frames() {
    let config = SlideshowConfig {
        slide_frames: 4,
        transition_frames: 2,
        ken_burns: true,
        order: fm_types::still::SlideOrder::Random { seed: 11 },
        ..SlideshowConfig::new(list(&["a.png", "b.png", "c.png", "d.png", "e.png"]))
    };
    let script = |engine: &mut Engine| {
        let mut frames = Vec::new();
        for frame in 0..40 {
            if frame == 7 {
                navigate(engine, "next", PagedNavigation::Next);
            }
            if frame == 19 {
                navigate(engine, "goto", PagedNavigation::Goto(4));
            }
            if frame == 23 {
                navigate(engine, "previous", PagedNavigation::Previous);
            }
            frames.push(step(engine));
        }
        frames
    };
    let first = script(&mut engine_with(config.clone()));
    let second = script(&mut engine_with(config));
    assert_eq!(first, second);
    let visited: std::collections::BTreeSet<_> =
        first.iter().map(|frame| frame.page.index).collect();
    assert!(visited.len() >= 3);
}
