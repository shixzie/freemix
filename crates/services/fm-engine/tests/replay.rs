//! OR-012: replay settings are an undoable project edit; marks, captures,
//! and Replay input transports are live operations realized on their
//! target frame against a rolling recording whose retention drops old
//! segments but never the media an event holds; a Replay input plays its
//! event frame-exactly at every speed.

use core::num::NonZeroU128;
use std::path::PathBuf;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RefusalReason, Rejection, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent, EngineInputSource,
    EngineReplayFeed, EngineReplayOperation, FrameResult, ShowState,
};
use fm_types::replay::{ReplaySettings, ReplaySpeed};
use fm_types::{FrameRate, InputId};

const FPS: u64 = 25;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "replay",
        (1..=3)
            .map(|value| (input(value), format!("Camera {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = execute(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
    outcome
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

fn replay(
    engine: &mut Engine,
    key: &str,
    operation: EngineReplayOperation,
) -> EngineCommandOutcome {
    accept(engine, key, EngineCommand::Replay(operation))
}

fn settings(sources: &[u128], retention_seconds: u32) -> ReplaySettings {
    ReplaySettings {
        sources: sources.iter().copied().map(input).collect(),
        retention_seconds,
        storage_root: Some("/srv/replay".to_owned()),
    }
}

fn add_player(engine: &mut Engine, value: u128) {
    accept(
        engine,
        &format!("add-{value}"),
        EngineCommand::AddInput {
            input: input(value),
            name: format!("Replay {value}"),
            source: EngineInputSource::Replay,
        },
    );
}

fn ticks(engine: &mut Engine, count: u64) -> Vec<FrameResult> {
    (0..count).map(|_| engine.tick().unwrap()).collect()
}

/// Ticks until every pending action is realized and returns the frames.
fn settle(engine: &mut Engine) -> Vec<FrameResult> {
    let mut frames = Vec::new();
    while engine.work_in_flight() {
        frames.push(engine.tick().unwrap());
    }
    frames
}

#[test]
fn settings_are_an_undoable_project_edit_checked_against_the_show() {
    let configure = |settings: ReplaySettings| {
        EngineCommand::Replay(EngineReplayOperation::Configure(Box::new(settings)))
    };
    assert_eq!(
        configure(settings(&[1], 600)).kind(),
        EngineCommandKind::ProjectEdit
    );
    assert_eq!(
        EngineCommand::Replay(EngineReplayOperation::MarkIn).kind(),
        EngineCommandKind::LiveOperation
    );
    let mut engine = engine();
    add_player(&mut engine, 9);
    let outcome = accept(&mut engine, "configure", configure(settings(&[3, 1], 60)));
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ReplayChanged {
            operation: EngineReplayOperation::Configure(_),
            ..
        }
    )));
    assert_eq!(engine.show().replay(), &settings(&[3, 1], 60));
    accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().replay(), &ReplaySettings::default());
    accept(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().replay(), &settings(&[3, 1], 60));

    // Unknown, Replay, repeated sources and a retention out of range are
    // refused and change nothing.
    for (key, refused, code) in [
        ("unknown", settings(&[7], 60), RejectionCode::NotFound),
        ("player", settings(&[9], 60), RejectionCode::InvalidCommand),
        (
            "twice",
            settings(&[1, 1], 60),
            RejectionCode::InvalidCommand,
        ),
        ("short", settings(&[1], 9), RejectionCode::InvalidCommand),
    ] {
        let outcome = execute(&mut engine, key, configure(refused));
        assert_eq!(rejection(&outcome).code, code, "{key}");
    }
    assert_eq!(engine.show().replay(), &settings(&[3, 1], 60));

    // A recorded input cannot be removed, nor become a player.
    let outcome = execute(
        &mut engine,
        "remove-source",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert!(matches!(
        rejection(&outcome).reason,
        Some(RefusalReason::InputInUse {
            input: 3,
            usage: fm_command::InputUse::ReplaySource
        })
    ));
    // At most two Replay inputs.
    add_player(&mut engine, 10);
    let outcome = execute(
        &mut engine,
        "third-player",
        EngineCommand::AddInput {
            input: input(11),
            name: "Replay 11".to_owned(),
            source: EngineInputSource::Replay,
        },
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    // Transports name a Replay input and a recorded angle.
    let outcome = execute(
        &mut engine,
        "play-camera",
        EngineCommand::Replay(EngineReplayOperation::Play {
            input: input(3),
            speed: ReplaySpeed::Half,
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    let outcome = execute(
        &mut engine,
        "cue-angle",
        EngineCommand::Replay(EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: Some(input(2)),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    let outcome = execute(
        &mut engine,
        "last-7",
        EngineCommand::Replay(EngineReplayOperation::CaptureLast { seconds: 7 }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
}

#[test]
fn captures_take_marked_and_last_ranges_and_events_are_named_and_deleted() {
    let mut engine = engine();
    // Without a source nothing is recorded, so marking is refused.
    let outcome = execute(
        &mut engine,
        "mark-off",
        EngineCommand::Replay(EngineReplayOperation::MarkIn),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1, 3], 600))),
    );
    settle(&mut engine);
    ticks(&mut engine, 30 * FPS);
    // Last 10 s: the ten seconds before the frame it is realized on.
    replay(
        &mut engine,
        "last",
        EngineReplayOperation::CaptureLast { seconds: 10 },
    );
    let realized = settle(&mut engine).last().unwrap().frame.get();
    let status = engine.replay_status();
    assert_eq!(status.events.len(), 1);
    let event = &status.events[0];
    assert_eq!(
        (
            event.id,
            event.name.as_str(),
            event.in_frame,
            event.out_frame
        ),
        (1, "Last 10 s", realized - 10 * FPS, realized)
    );
    assert_eq!(event.angles, [input(1), input(3)]);
    assert_eq!(event.preferred_angle, input(1));

    // Mark in, mark out, capture: the marked frames inclusive.
    replay(&mut engine, "in", EngineReplayOperation::MarkIn);
    let mark_in = settle(&mut engine).last().unwrap().frame.get();
    ticks(&mut engine, 2 * FPS);
    replay(&mut engine, "out", EngineReplayOperation::MarkOut);
    let mark_out = settle(&mut engine).last().unwrap().frame.get();
    assert_eq!(engine.replay_status().mark_in, Some(mark_in));
    assert_eq!(engine.replay_status().mark_out, Some(mark_out + 1));
    replay(
        &mut engine,
        "capture",
        EngineReplayOperation::CaptureMarks {
            name: Some("Goal".to_owned()),
        },
    );
    settle(&mut engine);
    let status = engine.replay_status();
    assert_eq!(status.mark_in, None);
    assert_eq!(status.events.len(), 2);
    assert_eq!(
        (
            status.events[1].id,
            status.events[1].name.as_str(),
            status.events[1].in_frame,
            status.events[1].out_frame
        ),
        (2, "Goal", mark_in, mark_out + 1)
    );
    // Synthetic segments cover a captured event once they close.
    ticks(&mut engine, 3 * FPS);
    assert!(
        engine
            .replay_status()
            .events
            .iter()
            .all(|event| event.complete)
    );

    replay(
        &mut engine,
        "rename",
        EngineReplayOperation::RenameEvent {
            event: 2,
            name: "Header".to_owned(),
        },
    );
    replay(
        &mut engine,
        "delete",
        EngineReplayOperation::DeleteEvent { event: 1 },
    );
    // An event that does not exist is accepted and changes nothing.
    replay(
        &mut engine,
        "delete-missing",
        EngineReplayOperation::DeleteEvent { event: 40 },
    );
    settle(&mut engine);
    let names: Vec<_> = engine
        .replay_status()
        .events
        .iter()
        .map(|event| (event.id, event.name.clone()))
        .collect();
    assert_eq!(names, [(2, "Header".to_owned())]);
}

#[test]
fn retention_drops_old_segments_but_keeps_what_an_event_holds() {
    let mut engine = engine();
    engine.set_replay_feed(EngineReplayFeed::External);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1], 10))),
    );
    settle(&mut engine);
    // The media plane records one frame a tick and closes a one-second
    // segment every second.
    let mut stream_frame = 0_u64;
    let mut segment_first = None;
    let mut index = 0_u64;
    let mut record = |engine: &mut Engine, count: u64| {
        for _ in 0..count {
            let frame = engine.tick().unwrap().frame.get();
            assert!(engine.replay_observe(input(1), frame, stream_frame));
            let first = *segment_first.get_or_insert((frame, stream_frame));
            stream_frame += 1;
            if stream_frame - first.1 == FPS {
                index += 1;
                assert!(engine.replay_segment_closed(
                    input(1),
                    index,
                    PathBuf::from(format!("/srv/replay/one-{index:06}.mp4")),
                    first.0,
                    first.1,
                    FPS,
                    1_000,
                ));
                segment_first = None;
            }
        }
    };
    record(&mut engine, 5 * FPS);
    replay(
        &mut engine,
        "last",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    record(&mut engine, 2);
    assert_eq!(engine.take_replay_pruned(), Vec::<PathBuf>::new());
    record(&mut engine, 20 * FPS);
    let pruned = engine.take_replay_pruned();
    let status = engine.replay_status();
    // Segments 1..=5 are held by the event; 6 onwards roll off once they
    // end more than ten seconds behind the live edge.
    let kept = status.sources[0].segments;
    assert_ne!(pruned, Vec::<PathBuf>::new());
    for index in 1..=5 {
        let path = PathBuf::from(format!("/srv/replay/one-{index:06}.mp4"));
        assert!(!pruned.contains(&path), "{index} pruned");
    }
    assert!(pruned.contains(&PathBuf::from("/srv/replay/one-000007.mp4")));
    assert_eq!(kept as usize + pruned.len(), 25);
    assert!(status.events[0].complete);
    // Deleting the event returns its segments to retention.
    replay(
        &mut engine,
        "delete",
        EngineReplayOperation::DeleteEvent { event: 1 },
    );
    record(&mut engine, 2);
    let pruned = engine.take_replay_pruned();
    assert!(pruned.contains(&PathBuf::from("/srv/replay/one-000001.mp4")));
    // Another source set starts a new recording: every file goes.
    let retained = engine.replay_status().sources[0].segments as usize;
    replay(
        &mut engine,
        "reconfigure",
        EngineReplayOperation::Configure(Box::new(settings(&[2], 10))),
    );
    settle(&mut engine);
    assert_eq!(engine.take_replay_pruned().len(), retained);
    assert_eq!(engine.replay_status().events, Vec::new());
}

#[test]
#[allow(clippy::too_many_lines)]
fn a_replay_input_plays_its_event_frame_exactly_at_every_speed() {
    let mut engine = engine();
    add_player(&mut engine, 9);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1, 2], 600))),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    replay(
        &mut engine,
        "last",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    let event = engine.replay_status().events[0].clone();
    replay(
        &mut engine,
        "cue",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: Some(input(2)),
        },
    );
    let cued = settle(&mut engine);
    let shown = cued.last().unwrap().replay[0].clone();
    assert_eq!(shown.input, input(9));
    assert_eq!(shown.angle, Some(input(2)));
    assert_eq!(shown.source_frame, Some(event.in_frame));
    assert!(!shown.playing);

    for (key, speed, repeats) in [
        ("half", ReplaySpeed::Half, 2),
        ("quarter", ReplaySpeed::Quarter, 4),
        ("full", ReplaySpeed::Full, 1),
    ] {
        replay(
            &mut engine,
            &format!("{key}-in"),
            EngineReplayOperation::CueIn { input: input(9) },
        );
        settle(&mut engine);
        replay(
            &mut engine,
            &format!("{key}-play"),
            EngineReplayOperation::Play {
                input: input(9),
                speed,
            },
        );
        let mut shown: Vec<u64> = settle(&mut engine)
            .iter()
            .filter(|frame| frame.replay[0].playing)
            .filter_map(|frame| frame.replay[0].source_frame)
            .collect();
        shown.extend(
            ticks(&mut engine, 40)
                .iter()
                .filter_map(|frame| frame.replay[0].source_frame),
        );
        let expected: Vec<u64> = (0..=40)
            .map(|index| event.in_frame + index / repeats)
            .collect();
        assert_eq!(shown[..40], expected[..40], "{key}");
    }
    // Three quarters: frames advance by three quarter frames a tick.
    replay(
        &mut engine,
        "tq-in",
        EngineReplayOperation::CueIn { input: input(9) },
    );
    settle(&mut engine);
    replay(
        &mut engine,
        "tq-play",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::ThreeQuarters,
        },
    );
    let mut shown: Vec<u64> = settle(&mut engine)
        .iter()
        .filter(|frame| frame.replay[0].playing)
        .filter_map(|frame| frame.replay[0].source_frame)
        .collect();
    shown.extend(
        ticks(&mut engine, 8)
            .iter()
            .filter_map(|frame| frame.replay[0].source_frame),
    );
    let expected: Vec<u64> = (0..8).map(|index| event.in_frame + index * 3 / 4).collect();
    assert_eq!(shown[..8], expected[..]);

    // Pause holds; another angle keeps the position; the end pauses on the
    // last frame.
    replay(
        &mut engine,
        "pause",
        EngineReplayOperation::Pause { input: input(9) },
    );
    let held = settle(&mut engine).last().unwrap().replay[0].clone();
    let after = ticks(&mut engine, 5).last().unwrap().replay[0].clone();
    assert_eq!(held.source_frame, after.source_frame);
    replay(
        &mut engine,
        "angle",
        EngineReplayOperation::Angle {
            input: input(9),
            angle: input(1),
        },
    );
    let switched = settle(&mut engine).last().unwrap().replay[0].clone();
    assert_eq!(switched.angle, Some(input(1)));
    assert_eq!(switched.source_frame, held.source_frame);
    replay(
        &mut engine,
        "to-end",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::Full,
        },
    );
    settle(&mut engine);
    let end = ticks(&mut engine, 6 * FPS).last().unwrap().replay[0].clone();
    assert!(!end.playing);
    assert_eq!(end.source_frame, Some(event.out_frame - 1));
    // Deleting the event empties the player.
    replay(
        &mut engine,
        "delete",
        EngineReplayOperation::DeleteEvent { event: 1 },
    );
    let empty = settle(&mut engine).last().unwrap().replay[0].clone();
    assert_eq!(empty.event, None);
    assert_eq!(empty.source_frame, None);
}

/// Protocol 2.114 jog: a step moves a cued player that many source frames,
/// paused, and stops at its event's first and last frames; a zero or
/// overlong step is refused.
#[test]
fn a_replay_step_jogs_a_cued_player_within_its_event() {
    let mut engine = engine();
    add_player(&mut engine, 9);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1], 600))),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    replay(
        &mut engine,
        "last",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    let event = engine.replay_status().events[0].clone();
    replay(
        &mut engine,
        "cue",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: None,
        },
    );
    settle(&mut engine);
    let shown = |engine: &mut Engine| {
        let frame = settle(engine).last().unwrap().replay[0].clone();
        assert!(!frame.playing);
        frame.source_frame
    };
    let step = |engine: &mut Engine, key: &str, frames: i32| {
        replay(
            engine,
            key,
            EngineReplayOperation::Step {
                input: input(9),
                frames,
            },
        );
    };
    step(&mut engine, "on-3", 3);
    assert_eq!(shown(&mut engine), Some(event.in_frame + 3));
    step(&mut engine, "back-1", -1);
    assert_eq!(shown(&mut engine), Some(event.in_frame + 2));
    // Before the first frame it stays on the first frame.
    step(&mut engine, "back-many", -600);
    assert_eq!(shown(&mut engine), Some(event.in_frame));
    // Past the last frame it stays on the last.
    step(&mut engine, "on-many", 600);
    assert_eq!(shown(&mut engine), Some(event.out_frame - 1));
    // A playing player stops where the step puts it.
    replay(
        &mut engine,
        "play",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::Full,
        },
    );
    step(&mut engine, "back-10", -10);
    assert_eq!(shown(&mut engine), Some(event.out_frame - 11));

    for (key, frames) in [("zero", 0), ("too-far", 601), ("too-far-back", -601)] {
        let outcome = execute(
            &mut engine,
            key,
            EngineCommand::Replay(EngineReplayOperation::Step {
                input: input(9),
                frames,
            }),
        );
        assert_eq!(rejection(&outcome).fields[0].code, "invalid_step", "{key}");
    }
}
