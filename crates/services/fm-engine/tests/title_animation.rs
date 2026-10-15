//! GX-011 stage 3: title animations play on air, frame-accurately, against
//! the engine's frame index and rate.
//!
//! The title is a 16x16 red box at (4, 4) sized 8x8. Its clip runs at 30/1:
//! the In phase fades opacity 0 -> 200 over 10 clip frames, the Out phase
//! moves it right 0 -> 6 px over 6 clip frames. The engine runs at 60/1,
//! so In lasts 20 engine frames and Out 12, each engine frame half a clip
//! frame.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineTitle, EngineTitleAir, FrameResult,
    ShowState, SnapshotError, TitleRejectionKind,
};
use fm_switcher::{OverlayChannelId, OverlayTransitionKind};
use fm_titles::fm_animation::{
    ClipPosition, ClipState, Easing, Keyframe, Phase, Property, Track, TrackKey, Value,
};
use fm_titles::{
    AnimSample, AssetCatalog, Bounds, Color, Element, ElementId, ElementKind, ReferenceRenderer,
    Style, TemplateId, TitleAirPhase, TitleAirTiming, TitleAnimation, TitleFieldSet, TitleId,
    TitleTemplate,
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

fn channel(number: u8) -> OverlayChannelId {
    OverlayChannelId::new(number).unwrap()
}

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn element() -> ElementId {
    ElementId::new(nonzero(1))
}

fn linear(frame: u32, value: i64) -> Keyframe {
    Keyframe::new(frame, Value::Scalar(value), Easing::Linear)
}

/// The animated box (see the module docs).
fn template() -> TitleTemplate {
    let mut template = TitleTemplate {
        id: TemplateId::new(nonzero(9)),
        name: "Animated".into(),
        width: 16,
        height: 16,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: vec![Element {
            id: element(),
            name: "box".into(),
            bounds: Bounds {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            },
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::new(255, 0, 0, 255),
                ..Style::default()
            },
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
                TrackKey::new(element(), Phase::In, Property::Opacity),
                vec![linear(0, 0), linear(10, 200)],
            ))?;
            clip.set_track(Track::new(
                TrackKey::new(element(), Phase::Out, Property::PositionX),
                vec![linear(0, 0), linear(6, 6)],
            ))
        })
        .unwrap();
    template
}

fn engine_at(frame_rate: FrameRate) -> Engine {
    let template = template();
    let mut show = ShowState::new(
        "animated titles",
        (PROGRAM..=TITLE)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    show.set_title(
        input(TITLE),
        EngineTitle::new("Animated", TitleFieldSet::from_template(&template).unwrap())
            .with_timing(TitleAirTiming::of(&template.animation)),
    )
    .unwrap();
    Engine::new(show, frame_rate, ClockDomainId::new(nonzero(100)))
}

fn engine() -> Engine {
    engine_at(rate(60, 1))
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) {
    let outcome = execute(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
}

fn take(on: u8) -> EngineCommand {
    EngineCommand::TakeTitle {
        input: input(TITLE),
        channel: channel(on),
        base_revision: 0,
        fields: Vec::new(),
    }
}

fn off_air() -> EngineCommand {
    EngineCommand::TitleOffAir {
        input: input(TITLE),
    }
}

/// Ticks to (and not including) engine frame `frame`.
fn tick_to(engine: &mut Engine, frame: u64) {
    while engine.frame_cursor().get() < frame {
        engine.tick().unwrap();
    }
}

/// The title's report on one realized frame, if it is tracked there.
fn title_on(result: &FrameResult) -> Option<EngineTitleAir> {
    assert!(result.titles.len() <= 1);
    result.titles.first().copied()
}

/// The 16x16 picture of the title in `sample`.
fn render(sample: AnimSample) -> Vec<[u8; 4]> {
    let scene = template().instantiate(TitleId::new(nonzero(1))).unwrap();
    let frame = ReferenceRenderer
        .render_sampled(&scene, 0, &AssetCatalog::new(), sample)
        .unwrap()
        .frame;
    (0..16)
        .flat_map(|y| (0..16).map(move |x| (x, y)))
        .map(|(x, y)| frame.pixel(x, y).unwrap().to_bytes())
        .collect()
}

/// Asserts the picture is the box at `x` (its width eight, y 4..12) in
/// premultiplied red at alpha `alpha`, transparent elsewhere.
fn assert_box(pixels: &[[u8; 4]], x: u32, alpha: u8, context: &str) {
    for (index, pixel) in pixels.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        let (px, py) = (index % 16, index / 16);
        let inside = (x..x + 8).contains(&px) && (4..12).contains(&py);
        let expected = if inside { [alpha, 0, 0, alpha] } else { [0; 4] };
        assert_eq!(*pixel, expected, "{context}: pixel ({px}, {py})");
    }
}

#[test]
fn take_plays_in_from_the_take_frame_holds_and_off_air_plays_out_then_removes() {
    let mut engine = engine();
    assert_eq!(engine.title_phase_frames(input(TITLE), Phase::In), 20);
    assert_eq!(engine.title_phase_frames(input(TITLE), Phase::Out), 12);
    // A configured fade is bypassed: the animation is the entrance.
    accept(
        &mut engine,
        "fade",
        EngineCommand::ConfigureOverlayTransition {
            channel: channel(2),
            transition: OverlayTransitionKind::Fade,
            duration_frames: 8,
        },
    );
    engine.tick().unwrap();
    tick_to(&mut engine, 5);
    accept(&mut engine, "take", take(2));

    let mut frames = Vec::new();
    for _ in 0..30 {
        frames.push(engine.tick().unwrap());
    }
    for (offset, result) in frames.iter().enumerate() {
        let frame = result.frame.get();
        assert_eq!(frame, 5 + offset as u64);
        let overlay = &result.overlays[channel(2).index()];
        assert!(overlay.is_active(), "frame {frame}");
        assert_eq!(overlay.opacity(), u8::MAX, "frame {frame}");
        let title = title_on(result).expect("the title is tracked on air");
        assert_eq!(title.input, input(TITLE));
        assert_eq!(title.channel, channel(2));
        let k = offset as u64;
        if k < 20 {
            // Engine frame k of the In phase is clip frame k / 2.
            assert_eq!(title.frame.phase, TitleAirPhase::In, "frame {frame}");
            assert_eq!(title.frame.phase_frame, k);
            assert_eq!(title.frame.phase_frames, 20);
            assert_eq!(
                title.frame.sample,
                ClipState::In(ClipPosition::new(u128::from(k), 2))
            );
        } else {
            assert_eq!(title.frame.phase, TitleAirPhase::Hold, "frame {frame}");
            assert_eq!((title.frame.phase_frame, title.frame.phase_frames), (0, 0));
            assert_eq!(title.frame.sample, ClipState::Hold);
        }
    }
    // Opacity 0 -> 200 over clip frames 0..10. Engine frames 5, 10, and 15
    // are clip frames 2.5, 5, and 7.5: a quarter, half, and three quarters
    // of the phase, exact in the Q16 easing progress.
    for (k, alpha) in [(0, 0), (5, 50), (10, 100), (15, 150), (20, 200), (29, 200)] {
        let sample = title_on(&frames[k]).unwrap().frame.sample;
        assert_box(&render(sample), 4, alpha, &format!("In frame {k}"));
    }
    // An animating title is work in flight; a holding one is not.
    assert!(engine.snapshot().is_ok());

    accept(&mut engine, "off", off_air());
    let mut out = Vec::new();
    for _ in 0..14 {
        out.push(engine.tick().unwrap());
    }
    for (offset, result) in out.iter().enumerate() {
        let frame = result.frame.get();
        let overlay = &result.overlays[channel(2).index()];
        let k = offset as u64;
        if k < 12 {
            assert!(overlay.is_active(), "frame {frame}");
            assert_eq!(overlay.opacity(), u8::MAX, "frame {frame}");
            let title = title_on(result).expect("the title plays Out on air");
            assert_eq!(title.frame.phase, TitleAirPhase::Out);
            assert_eq!(title.frame.phase_frame, k);
            assert_eq!(title.frame.phase_frames, 12);
            assert_eq!(
                title.frame.sample,
                ClipState::Out(ClipPosition::new(u128::from(k), 2))
            );
        } else {
            // The overlay is gone the frame after the last Out frame.
            assert!(!overlay.is_active(), "frame {frame}");
            assert_eq!(title_on(result), None);
        }
    }
    // Position 0 -> 6 px over clip frames 0..6: engine frame 6 is clip
    // frame 3, exactly half way.
    for (k, dx) in [(0, 0), (6, 3)] {
        let sample = title_on(&out[k]).unwrap().frame.sample;
        assert_box(&render(sample), 4 + dx, u8::MAX, &format!("Out frame {k}"));
    }
    assert_eq!(engine.titles_on_air(), Vec::new());
    assert!(engine.snapshot().is_ok());
}

#[test]
fn an_animating_title_is_work_in_flight_for_snapshots() {
    let mut engine = engine();
    accept(&mut engine, "take", take(1));
    engine.tick().unwrap();
    assert_eq!(engine.snapshot().err(), Some(SnapshotError::WorkInFlight));
    let now = engine.titles_on_air();
    assert_eq!(now.len(), 1);
    assert_eq!(now[0].frame.phase, TitleAirPhase::In);
    assert_eq!(now[0].frame.phase_frame, 1);
    tick_to(&mut engine, 20);
    assert_eq!(engine.titles_on_air()[0].frame.phase, TitleAirPhase::Hold);
    assert!(engine.snapshot().is_ok());

    accept(&mut engine, "off", off_air());
    engine.tick().unwrap();
    assert_eq!(engine.snapshot().err(), Some(SnapshotError::WorkInFlight));
    tick_to(&mut engine, 20 + 12);
    assert_eq!(engine.titles_on_air(), Vec::new());
    assert!(engine.snapshot().is_ok());
}

#[test]
fn taking_off_air_during_in_starts_out_at_its_first_frame_on_the_off_air_frame() {
    let mut engine = engine();
    accept(&mut engine, "take", take(1));
    for _ in 0..4 {
        engine.tick().unwrap();
    }
    accept(&mut engine, "off", off_air());
    let first = engine.tick().unwrap();
    assert_eq!(first.frame.get(), 4);
    let title = title_on(&first).unwrap();
    assert_eq!(title.frame.phase, TitleAirPhase::Out);
    assert_eq!(title.frame.phase_frame, 0);
    assert_eq!(title.frame.sample, ClipState::Out(ClipPosition::frame(0)));
    // The Out pose has no opacity track: the abandoned In fade no longer
    // applies, so the box is fully opaque at its static place.
    assert_box(&render(title.frame.sample), 4, u8::MAX, "Out frame 0");
    for _ in 1..12 {
        let result = engine.tick().unwrap();
        assert!(result.overlays[channel(1).index()].is_active());
        assert_eq!(title_on(&result).unwrap().frame.phase, TitleAirPhase::Out);
    }
    let gone = engine.tick().unwrap();
    assert_eq!(gone.frame.get(), 16);
    assert!(!gone.overlays[channel(1).index()].is_active());
    assert_eq!(title_on(&gone), None);
}

#[test]
fn retaking_during_out_restarts_in_on_the_same_channel_only() {
    let mut engine = engine();
    accept(&mut engine, "take", take(3));
    tick_to(&mut engine, 25);
    accept(&mut engine, "off", off_air());
    tick_to(&mut engine, 30);
    assert_eq!(engine.titles_on_air()[0].frame.phase, TitleAirPhase::Out);

    // Another channel is refused: the title is still on air on channel 3.
    let other = execute(&mut engine, "retake-elsewhere", take(4));
    let rejection = &other.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::Conflict, "{rejection:?}");
    assert_eq!(
        TitleRejectionKind::of(rejection),
        Some(TitleRejectionKind::OnAir)
    );

    accept(&mut engine, "retake", take(3));
    let result = engine.tick().unwrap();
    assert_eq!(result.frame.get(), 30);
    let title = title_on(&result).unwrap();
    assert_eq!(title.channel, channel(3));
    assert_eq!(title.frame.phase, TitleAirPhase::In);
    assert_eq!(title.frame.phase_frame, 0);
    assert_eq!(title.frame.sample, ClipState::In(ClipPosition::frame(0)));
    // The held channel is on air again, no longer leaving.
    let overlay = &result.overlays[channel(3).index()];
    assert!(overlay.is_active());
    assert!(!overlay.is_transitioning());
    tick_to(&mut engine, 30 + 20);
    assert_eq!(engine.titles_on_air()[0].frame.phase, TitleAirPhase::Hold);
    assert!(engine.realized_switcher().overlay(channel(3)).is_active());
}

#[test]
fn a_29_97_output_resamples_a_30p_clip_exactly() {
    let mut engine = engine_at(rate(30_000, 1_001));
    // ceil(10 * 30000 / (30 * 1001)) = ceil(9.99) = 10.
    assert_eq!(engine.title_phase_frames(input(TITLE), Phase::In), 10);
    accept(&mut engine, "take", take(1));
    let samples = (0..11)
        .map(|_| title_on(&engine.tick().unwrap()).unwrap().frame.sample)
        .collect::<Vec<_>>();
    for (k, sample) in samples.iter().enumerate().take(10) {
        // Clip position k * 30 * 1001 / 30000 = k * 1.001.
        assert_eq!(
            *sample,
            ClipState::In(ClipPosition::new(k as u128 * 1_001, 1_000)),
            "frame {k}"
        );
    }
    assert_eq!(samples[10], ClipState::Hold);
}

#[test]
fn an_unanimated_title_holds_at_once_and_leaves_with_the_channel_transition() {
    let template = template();
    let mut show = ShowState::new(
        "static titles",
        (PROGRAM..=TITLE)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    show.set_title(
        input(TITLE),
        EngineTitle::new("Static", TitleFieldSet::from_template(&template).unwrap()),
    )
    .unwrap();
    let mut engine = Engine::new(show, rate(25, 1), ClockDomainId::new(nonzero(100)));
    accept(
        &mut engine,
        "fade",
        EngineCommand::ConfigureOverlayTransition {
            channel: channel(1),
            transition: OverlayTransitionKind::Fade,
            duration_frames: 4,
        },
    );
    engine.tick().unwrap();
    accept(&mut engine, "take", take(1));
    let first = engine.tick().unwrap();
    // The channel fades in; the title holds from its take frame.
    assert_eq!(first.overlays[channel(1).index()].opacity(), 0);
    assert_eq!(title_on(&first).unwrap().frame.phase, TitleAirPhase::Hold);
    tick_to(&mut engine, 10);
    accept(&mut engine, "off", off_air());
    let leaving = engine.tick().unwrap();
    assert!(leaving.overlays[channel(1).index()].is_transitioning());
    assert_eq!(title_on(&leaving), None);
    tick_to(&mut engine, 15);
    assert!(!engine.realized_switcher().overlay(channel(1)).is_active());
}

#[test]
fn a_submitted_title_cue_is_refused_and_never_realized() {
    let mut engine = engine();
    let outcome = execute(
        &mut engine,
        "cue",
        EngineCommand::RealizeTitle(fm_engine::TitleCue {
            input: input(TITLE),
            channel: channel(1),
            action: fm_engine::TitleCueAction::Take,
            timing: TitleAirTiming::of(&template().animation),
        }),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(
        rejection.code,
        RejectionCode::InvalidCommand,
        "{rejection:?}"
    );
    let result = engine.tick().unwrap();
    assert!(!result.overlays[channel(1).index()].is_active());
    assert_eq!(title_on(&result), None);
}

#[test]
fn a_plain_take_of_a_leaving_title_on_its_channel_ends_the_out_phase() {
    let mut engine = engine();
    accept(&mut engine, "take", take(3));
    tick_to(&mut engine, 25);
    accept(&mut engine, "off", off_air());
    tick_to(&mut engine, 30);
    assert_eq!(engine.titles_on_air()[0].frame.phase, TitleAirPhase::Out);

    // The operator puts the title back with a plain overlay take: it is on
    // air the plain way, in its Hold pose, not still animating out (and
    // then snapping back to Hold while on air).
    accept(
        &mut engine,
        "plain-take",
        EngineCommand::TakeOverlay {
            channel: channel(3),
            source: input(TITLE),
        },
    );
    let result = engine.tick().unwrap();
    assert!(result.overlays[channel(3).index()].is_active());
    assert!(!result.overlays[channel(3).index()].is_transitioning());
    assert_eq!(title_on(&result), None);
    assert_eq!(engine.titles_on_air(), Vec::new());
    assert!(engine.snapshot().is_ok());
}

#[test]
fn a_plain_off_fading_past_the_out_phase_still_ends_with_it() {
    let mut engine = engine();
    accept(
        &mut engine,
        "fade",
        EngineCommand::ConfigureOverlayTransition {
            channel: channel(3),
            transition: OverlayTransitionKind::Fade,
            duration_frames: 30,
        },
    );
    accept(&mut engine, "take", take(3));
    tick_to(&mut engine, 25);
    accept(&mut engine, "off", off_air());
    tick_to(&mut engine, 30);
    // A plain off while the title plays Out (frames 25..37) fades the
    // channel over 30 frames, past the end of the Out phase.
    accept(
        &mut engine,
        "plain-off",
        EngineCommand::OverlayOff {
            channel: channel(3),
        },
    );
    tick_to(&mut engine, 36);
    let last_out = engine.tick().unwrap();
    assert_eq!(last_out.frame.get(), 36);
    assert_eq!(title_on(&last_out).unwrap().frame.phase, TitleAirPhase::Out);
    // The title animated out: it never comes back in its Hold pose.
    let after = engine.tick().unwrap();
    assert_eq!(title_on(&after), None);
    assert!(
        !after.overlays[channel(3).index()].is_active(),
        "the channel goes off when the Out phase ends"
    );
    assert!(!engine.realized_switcher().overlay(channel(3)).is_active());
    assert!(engine.snapshot().is_ok());
}
