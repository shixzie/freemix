//! GX-001: a title's timeline (tickers, scene clocks, since-take clocks,
//! and its In/Out on Program or Preview) starts on the frame it is taken to
//! Program, an overlay channel, or Preview, and survives a snapshot, so a
//! checkpoint followed by journal replay continues it exactly.
//!
//! The title's clip runs at 30/1 with a 10-frame In and a 6-frame Out; the
//! engine runs at 60/1, so In lasts 20 engine frames and Out 12.

use core::num::NonZeroU128;
use std::collections::BTreeMap;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{Engine, EngineCommand, EngineTitle, EngineTitleShown, ShowState, SnapshotError};
use fm_model::{TitleShownOn, TitleTimeline};
use fm_switcher::OverlayChannelId;
use fm_titles::fm_animation::{Easing, Keyframe, Phase, Property, Track, TrackKey, Value};
use fm_titles::{
    Bounds, Color, Element, ElementId, ElementKind, Style, TemplateId, TitleAirPhase,
    TitleAirTiming, TitleAnimation, TitleFieldSet, TitleTemplate,
};
use fm_types::{FrameRate, InputId};

const PROGRAM: u128 = 1;
const PREVIEW: u128 = 2;
const TITLE: u128 = 3;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn template() -> TitleTemplate {
    let element = ElementId::new(nonzero(1));
    let mut template = TitleTemplate {
        id: TemplateId::new(nonzero(9)),
        name: "Ticker".into(),
        width: 16,
        height: 16,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: vec![Element {
            id: element,
            name: "box".into(),
            bounds: Bounds {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            },
            z_index: 0,
            visible: true,
            style: Style::default(),
            color_field: None,
            kind: ElementKind::Rectangle,
        }],
        animation: TitleAnimation::default(),
    };
    template
        .edit_animation(|clip| {
            clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
            clip.set_phase_frames_keeping_keys(Phase::Out, 6)?;
            clip.set_track(Track::new(
                TrackKey::new(element, Phase::In, Property::Opacity),
                vec![
                    Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                    Keyframe::new(10, Value::Scalar(255), Easing::Linear),
                ],
            ))
        })
        .unwrap();
    template
}

fn engine() -> Engine {
    let template = template();
    let mut show = ShowState::new(
        "title timelines",
        (PROGRAM..=TITLE)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    show.set_title(
        input(TITLE),
        EngineTitle::new("Ticker", TitleFieldSet::from_template(&template).unwrap())
            .with_timing(TitleAirTiming::of(&template.animation)),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(60, 1).unwrap(),
        ClockDomainId::new(nonzero(100)),
    )
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
}

fn tick_to(engine: &mut Engine, frame: u64) -> Option<EngineTitleShown> {
    let mut last = None;
    while engine.frame_cursor().get() < frame {
        let result = engine.tick().unwrap();
        assert!(result.titles_shown.len() <= 1);
        last = result.titles_shown.first().copied();
    }
    last
}

fn tick_by(engine: &mut Engine, frames: u64) -> Option<EngineTitleShown> {
    let to = engine.frame_cursor().get() + frames;
    tick_to(engine, to)
}

fn timeline(on: TitleShownOn, since_frame: u64) -> TitleTimeline {
    TitleTimeline { on, since_frame }
}

#[test]
fn a_title_is_dated_to_the_frame_it_reaches_preview_then_program() {
    let mut engine = engine();
    assert_eq!(tick_to(&mut engine, 30), None, "not shown: no timeline");
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    );
    let first = tick_by(&mut engine, 1).unwrap();
    let preview_frame = first.timeline.since_frame;
    assert_eq!(first.timeline.on, TitleShownOn::Preview);
    // The In phase plays on Preview from that frame.
    let bus = first.bus.unwrap();
    assert_eq!(
        (bus.phase, bus.phase_frame, bus.phase_frames),
        (TitleAirPhase::In, 0, 20)
    );
    let held = tick_to(&mut engine, preview_frame + 25).unwrap();
    assert_eq!(
        held.timeline,
        timeline(TitleShownOn::Preview, preview_frame)
    );
    assert_eq!(held.bus.unwrap().phase, TitleAirPhase::Hold);

    // A cut takes it to air: the timeline restarts there and In plays again.
    accept(&mut engine, "cut", EngineCommand::Cut);
    let cut = tick_by(&mut engine, 1).unwrap();
    let cut_frame = cut.timeline.since_frame;
    assert!(cut_frame > preview_frame);
    assert_eq!(cut.timeline.on, TitleShownOn::Air);
    assert_eq!(cut.bus.unwrap().phase, TitleAirPhase::In);
    // It keeps its date while it stays on air.
    let later = tick_to(&mut engine, cut_frame + 100).unwrap();
    assert_eq!(later.timeline, timeline(TitleShownOn::Air, cut_frame));
    assert!(!later.in_motion());
}

#[test]
fn a_transition_off_program_plays_out_and_a_plain_overlay_take_holds() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    );
    tick_to(&mut engine, 5);
    accept(&mut engine, "cut", EngineCommand::Cut);
    tick_to(&mut engine, 40);
    // The old Program is now on Preview: fade the title off over 30 frames.
    accept(
        &mut engine,
        "fade",
        EngineCommand::Fade {
            duration_frames: 30,
        },
    );
    let leaving = tick_by(&mut engine, 1).unwrap();
    let bus = leaving.bus.unwrap();
    assert_eq!(
        (bus.phase, bus.phase_frame, bus.phase_frames),
        (TitleAirPhase::Out, 0, 12)
    );
    // An Out shorter than the transition holds its last frame.
    let ended = tick_by(&mut engine, 14).unwrap();
    assert_eq!(
        (ended.bus.unwrap().phase, ended.bus.unwrap().phase_frames),
        (TitleAirPhase::Out, 0)
    );
    tick_by(&mut engine, 30);

    // Shown only on an overlay by a plain take: on air, no bus phase.
    accept(
        &mut engine,
        "overlay",
        EngineCommand::TakeOverlay {
            channel: OverlayChannelId::new(1).unwrap(),
            source: input(TITLE),
        },
    );
    let overlay_frame = engine.frame_cursor().get();
    let shown = tick_to(&mut engine, overlay_frame + 3).unwrap();
    assert_eq!(shown.timeline.on, TitleShownOn::Air);
    assert_eq!(shown.bus, None);
}

#[test]
fn a_title_take_restarts_the_timeline_on_its_frame() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    );
    tick_to(&mut engine, 10);
    accept(
        &mut engine,
        "take",
        EngineCommand::TakeTitle {
            input: input(TITLE),
            channel: OverlayChannelId::new(2).unwrap(),
            base_revision: 0,
            fields: Vec::new(),
        },
    );
    let take_frame = engine.frame_cursor().get();
    let taken = tick_to(&mut engine, take_frame + 1).unwrap();
    assert_eq!(taken.timeline, timeline(TitleShownOn::Air, take_frame));
}

#[test]
fn a_snapshot_carries_the_timeline_and_a_restored_engine_continues_it() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    );
    tick_to(&mut engine, 3);
    accept(&mut engine, "cut", EngineCommand::Cut);
    tick_to(&mut engine, 200);
    let snapshot = engine.snapshot().unwrap();
    let saved = snapshot.title_timelines().clone();
    assert_eq!(saved.len(), 1);
    let since = saved[&input(TITLE)];
    assert_eq!(since.on, TitleShownOn::Air);

    let mut restored = Engine::restore(snapshot).unwrap();
    assert_eq!(restored.title_timelines(), saved);
    // Both engines realize the same frames with the same timeline and pose.
    for _ in 0..50 {
        let original = engine.tick().unwrap();
        let replayed = restored.tick().unwrap();
        assert_eq!(original.titles_shown, replayed.titles_shown);
        assert_eq!(replayed.titles_shown[0].timeline, since);
    }
}

#[test]
fn a_restored_timeline_must_name_a_title_shown_before_the_cursor() {
    let mut engine = engine();
    tick_to(&mut engine, 10);
    let not_a_title = BTreeMap::from([(input(PREVIEW), timeline(TitleShownOn::Air, 2))]);
    assert!(matches!(
        engine.restore_title_timelines(&not_a_title),
        Err(SnapshotError::TitleTimelineOutOfReach { .. })
    ));
    let ahead = BTreeMap::from([(input(TITLE), timeline(TitleShownOn::Air, 10))]);
    assert!(matches!(
        engine.restore_title_timelines(&ahead),
        Err(SnapshotError::TitleTimelineOutOfReach { .. })
    ));
    let fine = BTreeMap::from([(input(TITLE), timeline(TitleShownOn::Air, 9))]);
    engine.restore_title_timelines(&fine).unwrap();
    assert_eq!(engine.title_timelines(), fine);
}
