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
    EngineReplayFeed, EngineReplayOperation, EngineReplayPlaylistEntry, FrameResult, ShowState,
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
        auto_return: None,
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

/// OR-012 auto return (Protocol 2.116): a Replay input on Program that plays
/// its event to the end hands the cue clock a due advance on its reserved
/// list, which takes Program back to the last live source; nothing is due
/// with auto return off, a client's `Return` is refused, and once Program
/// has left the Replay input nothing is due again.
#[test]
#[allow(clippy::too_many_lines)] // the recording, the take, the return
fn a_finished_replay_on_program_returns_to_the_live_source() {
    let program = |engine: &Engine| engine.show().desired_switcher().program();
    let mut engine = engine();
    add_player(&mut engine, 9);
    let mut settings = settings(&[1], 600);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings.clone())),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    replay(
        &mut engine,
        "last",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    replay(
        &mut engine,
        "cue",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: None,
        },
    );
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(9)),
    );
    accept(&mut engine, "take", EngineCommand::Cut);
    settle(&mut engine);
    assert_eq!(program(&engine), input(9));
    let play = |engine: &mut Engine, key: &str| {
        replay(
            engine,
            key,
            EngineReplayOperation::Play {
                input: input(9),
                speed: ReplaySpeed::Full,
            },
        );
    };
    // Auto return off: the finished replay stays on air.
    play(&mut engine, "play-off");
    ticks(&mut engine, 6 * FPS);
    assert_eq!(engine.cue_advance_frame(), None);
    assert_eq!(program(&engine), input(9));

    settings.auto_return = Some(0);
    replay(
        &mut engine,
        "auto-return",
        EngineReplayOperation::Configure(Box::new(settings)),
    );
    settle(&mut engine);
    replay(
        &mut engine,
        "cue-in",
        EngineReplayOperation::CueIn { input: input(9) },
    );
    play(&mut engine, "play-on");
    ticks(&mut engine, 4 * FPS);
    assert_eq!(engine.cue_advance_frame(), None, "still playing");
    ticks(&mut engine, 2 * FPS);
    let (list, frame) = engine.cue_advance_frame().expect("a return is due");
    assert_eq!(list, fm_engine::REPLAY_RETURN_LIST);

    // A client cannot ask for the return itself.
    let outcome = execute(
        &mut engine,
        "client-return",
        EngineCommand::Replay(EngineReplayOperation::Return {
            input: input(9),
            to: input(1),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);

    let outcome = accept(
        &mut engine,
        "return",
        EngineCommand::CueAdvance { list, frame },
    );
    assert!(outcome.events.iter().any(|event| matches!(
        event.payload,
        EngineEvent::ReplayChanged {
            operation: EngineReplayOperation::Return { input: from, to },
            ..
        } if from == input(9) && to == input(1)
    )));
    settle(&mut engine);
    assert_eq!(program(&engine), input(1));
    assert_eq!(engine.cue_advance_frame(), None);
    // Taking the finished replay back to air does not return at once: it
    // has to play to its end again.
    accept(
        &mut engine,
        "preview-again",
        EngineCommand::SelectPreview(input(9)),
    );
    accept(&mut engine, "take-again", EngineCommand::Cut);
    settle(&mut engine);
    ticks(&mut engine, 2);
    assert_eq!(engine.cue_advance_frame(), None);
    assert_eq!(program(&engine), input(9));
}

/// Protocol 2.119: a Replay input's playlist plays its events back to
/// back, and Program returns to the live source only after the last one.
#[test]
#[allow(clippy::too_many_lines)] // two captures, the take, and the return
fn a_playlist_plays_its_events_back_to_back_and_returns_after_the_last() {
    let program = |engine: &Engine| engine.show().desired_switcher().program();
    let mut engine = engine();
    add_player(&mut engine, 9);
    let mut settings = settings(&[1, 2], 600);
    settings.auto_return = Some(0);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings)),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    replay(
        &mut engine,
        "last-1",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    ticks(&mut engine, FPS);
    replay(
        &mut engine,
        "last-2",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    let events = engine.replay_status().events;
    assert_eq!(events.len(), 2);
    let entry = |event: u32, angle: Option<u128>, speed| EngineReplayPlaylistEntry {
        event,
        angle: angle.map(input),
        speed,
    };
    replay(
        &mut engine,
        "playlist",
        EngineReplayOperation::PlaylistSet {
            input: input(9),
            entries: vec![
                entry(1, Some(2), ReplaySpeed::Full),
                entry(2, None, ReplaySpeed::Half),
            ],
        },
    );
    settle(&mut engine);
    let status = engine.replay_status();
    assert_eq!(status.players[0].playlist.len(), 2);
    assert_eq!(status.players[0].playlist_index, None);

    // Cueing a listed event puts the player on its entry.
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
    let status = engine.replay_status();
    assert_eq!(status.players[0].playlist_index, Some(0));
    // A cue names its own angle (or the event's preferred one); the
    // entry's angle applies when the playlist moves on to it.
    assert_eq!(status.players[0].angle, Some(input(1)));

    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(9)),
    );
    accept(&mut engine, "take", EngineCommand::Cut);
    settle(&mut engine);
    assert_eq!(program(&engine), input(9));
    replay(
        &mut engine,
        "play",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::Full,
        },
    );
    settle(&mut engine);
    // The first event ends and the second starts on the same player, at
    // its entry's angle and speed; Program stays on the replay.
    ticks(&mut engine, 5 * FPS);
    assert_eq!(engine.cue_advance_frame(), None, "the playlist goes on");
    let status = engine.replay_status();
    let player = &status.players[0];
    assert_eq!(player.event, Some(2));
    assert_eq!(player.playlist_index, Some(1));
    assert_eq!(player.angle, Some(input(1)));
    assert_eq!(player.speed, ReplaySpeed::Half);
    assert!(player.playing);
    assert_eq!(program(&engine), input(9));
    // The last event ends: the player stops and Program returns.
    ticks(&mut engine, 10 * FPS);
    assert!(!engine.replay_status().players[0].playing);
    let (list, frame) = engine.cue_advance_frame().expect("a return is due");
    assert_eq!(list, fm_engine::REPLAY_RETURN_LIST);
    accept(
        &mut engine,
        "return",
        EngineCommand::CueAdvance { list, frame },
    );
    settle(&mut engine);
    assert_eq!(program(&engine), input(1));
}

/// Protocol 2.119: NEXT and PREV cue the neighbouring playlist entries a
/// player still has, and CLEAR empties the playlist without moving it.
#[test]
#[allow(clippy::too_many_lines)] // two captured events and every step
fn playlist_next_and_prev_cue_entries_and_clear_keeps_the_player() {
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
        "last-1",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    ticks(&mut engine, FPS);
    replay(
        &mut engine,
        "last-2",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    let entry = |event: u32, speed| EngineReplayPlaylistEntry {
        event,
        angle: None,
        speed,
    };
    replay(
        &mut engine,
        "playlist",
        EngineReplayOperation::PlaylistSet {
            input: input(9),
            entries: vec![entry(1, ReplaySpeed::Full), entry(2, ReplaySpeed::Quarter)],
        },
    );
    settle(&mut engine);
    let player = |engine: &Engine| engine.replay_status().players[0].clone();

    // Nothing cued: NEXT starts at the first entry, PREV at the last.
    replay(
        &mut engine,
        "next",
        EngineReplayOperation::PlaylistNext { input: input(9) },
    );
    settle(&mut engine);
    let now = player(&engine);
    assert_eq!(now.event, Some(1));
    assert_eq!(now.playlist_index, Some(0));
    assert!(!now.playing);
    let first = engine
        .replay_status()
        .events
        .iter()
        .find(|event| event.id == 1)
        .expect("event 1")
        .in_frame;
    assert_eq!(now.frame, Some(first));

    replay(
        &mut engine,
        "next-2",
        EngineReplayOperation::PlaylistNext { input: input(9) },
    );
    settle(&mut engine);
    let now = player(&engine);
    assert_eq!(now.event, Some(2));
    assert_eq!(now.playlist_index, Some(1));
    assert_eq!(now.speed, ReplaySpeed::Quarter);
    // The last entry's NEXT changes nothing.
    replay(
        &mut engine,
        "next-3",
        EngineReplayOperation::PlaylistNext { input: input(9) },
    );
    settle(&mut engine);
    assert_eq!(player(&engine).event, Some(2));
    replay(
        &mut engine,
        "prev",
        EngineReplayOperation::PlaylistPrev { input: input(9) },
    );
    settle(&mut engine);
    let now = player(&engine);
    assert_eq!(now.event, Some(1));
    assert_eq!(now.playlist_index, Some(0));
    assert_eq!(now.speed, ReplaySpeed::Full);
    // And the first entry's PREV changes nothing either.
    replay(
        &mut engine,
        "prev-2",
        EngineReplayOperation::PlaylistPrev { input: input(9) },
    );
    settle(&mut engine);
    assert_eq!(player(&engine).event, Some(1));

    // CLEAR leaves the cued event and its position alone.
    replay(
        &mut engine,
        "clear",
        EngineReplayOperation::PlaylistSet {
            input: input(9),
            entries: Vec::new(),
        },
    );
    settle(&mut engine);
    let now = player(&engine);
    assert!(now.playlist.is_empty());
    assert_eq!(now.playlist_index, None);
    assert_eq!(now.event, Some(1));
}

/// Protocol 2.124: A/B linking cues both players on one event at the same
/// position while each keeps its own angle, and play, pause, cue-in, and
/// step on either move both until they are unlinked.
#[test]
#[allow(clippy::too_many_lines)] // the link, both angles, and every transport
fn linked_players_share_transport_but_keep_their_angles() {
    let player = |engine: &Engine, value: u128| {
        engine
            .replay_status()
            .players
            .into_iter()
            .find(|player| player.input == input(value))
            .unwrap_or_else(|| panic!("no player {value}"))
    };
    let mut engine = engine();
    add_player(&mut engine, 9);
    add_player(&mut engine, 10);
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
    replay(
        &mut engine,
        "cue-a",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: Some(input(2)),
        },
    );
    replay(
        &mut engine,
        "link",
        EngineReplayOperation::Link {
            input: input(9),
            linked: Some(input(10)),
        },
    );
    settle(&mut engine);
    let a = player(&engine, 9);
    let b = player(&engine, 10);
    assert_eq!(a.linked, Some(input(10)));
    assert_eq!(b.linked, Some(input(9)));
    // B is cued on A's event, position, and angle; each can switch its own
    // angle afterwards.
    assert_eq!(a.event, Some(1));
    assert_eq!(b.event, Some(1));
    assert_eq!(a.angle, Some(input(2)));
    assert_eq!(b.angle, Some(input(2)));
    assert_eq!(a.frame, b.frame);
    assert!(!a.playing && !b.playing);

    // Each player switches angle independently.
    replay(
        &mut engine,
        "angle-a",
        EngineReplayOperation::Angle {
            input: input(9),
            angle: input(1),
        },
    );
    replay(
        &mut engine,
        "angle-b",
        EngineReplayOperation::Angle {
            input: input(10),
            angle: input(2),
        },
    );
    settle(&mut engine);
    assert_eq!(player(&engine, 9).angle, Some(input(1)));
    assert_eq!(player(&engine, 10).angle, Some(input(2)));

    // Play on B starts both at the same speed and position.
    replay(
        &mut engine,
        "play-b",
        EngineReplayOperation::Play {
            input: input(10),
            speed: ReplaySpeed::Half,
        },
    );
    settle(&mut engine);
    ticks(&mut engine, 2 * FPS);
    let a = player(&engine, 9);
    let b = player(&engine, 10);
    assert!(a.playing && b.playing);
    assert_eq!(a.speed, ReplaySpeed::Half);
    assert_eq!(b.speed, ReplaySpeed::Half);
    assert_eq!(a.frame, b.frame, "A and B stay on the same instant");

    // Pause on A pauses both; a step on A moves both by the same frames.
    let before = a.frame.unwrap();
    replay(
        &mut engine,
        "pause-a",
        EngineReplayOperation::Pause { input: input(9) },
    );
    settle(&mut engine);
    assert!(!player(&engine, 9).playing);
    assert!(!player(&engine, 10).playing);
    replay(
        &mut engine,
        "step-a",
        EngineReplayOperation::Step {
            input: input(9),
            frames: 3,
        },
    );
    settle(&mut engine);
    assert_eq!(player(&engine, 9).frame, Some(before + 3));
    assert_eq!(player(&engine, 10).frame, Some(before + 3));

    // Cue-in on B returns both to the event's first frame.
    replay(
        &mut engine,
        "cue-in-b",
        EngineReplayOperation::CueIn { input: input(10) },
    );
    settle(&mut engine);
    let first = engine.replay_status().events[0].in_frame;
    assert_eq!(player(&engine, 9).frame, Some(first));
    assert_eq!(player(&engine, 10).frame, Some(first));

    // Unlinking from either side clears both; play no longer propagates.
    replay(
        &mut engine,
        "unlink",
        EngineReplayOperation::Link {
            input: input(10),
            linked: None,
        },
    );
    settle(&mut engine);
    assert_eq!(player(&engine, 9).linked, None);
    assert_eq!(player(&engine, 10).linked, None);
    replay(
        &mut engine,
        "play-a",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::Full,
        },
    );
    settle(&mut engine);
    ticks(&mut engine, 3);
    assert!(player(&engine, 9).playing);
    assert!(!player(&engine, 10).playing);
    assert_ne!(player(&engine, 9).frame, player(&engine, 10).frame);
}

/// Protocol 2.124: a link names two distinct Replay inputs.
#[test]
fn a_link_needs_two_distinct_replay_inputs() {
    let mut engine = engine();
    add_player(&mut engine, 9);
    let link = |engine: &mut Engine, key: &str, linked: Option<u128>| {
        execute(
            engine,
            key,
            EngineCommand::Replay(EngineReplayOperation::Link {
                input: input(9),
                linked: linked.map(input),
            }),
        )
    };
    // A camera is not a Replay input.
    let outcome = link(&mut engine, "camera", Some(1));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].field, "linked");
    assert_eq!(rejection(&outcome).fields[0].code, "not_replay_input");
    // A player cannot link to itself.
    let outcome = link(&mut engine, "self", Some(9));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].code, "self_link");
    // An unknown input is refused.
    let outcome = link(&mut engine, "unknown", Some(99));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].code, "unknown_input");
    // Unlinking needs only the player.
    accept(
        &mut engine,
        "unlink",
        EngineCommand::Replay(EngineReplayOperation::Link {
            input: input(9),
            linked: None,
        }),
    );
    settle(&mut engine);
    assert_eq!(engine.replay_status().players[0].linked, None);
}

/// Protocol 2.124: linked players leave an event together. Both advance to
/// their own playlist's next entry only when both have one; otherwise both
/// stop instead of drifting apart.
#[test]
#[allow(clippy::too_many_lines)] // two captures, two playlists, two ends
fn linked_players_leave_an_event_only_together() {
    let player = |engine: &Engine, value: u128| {
        engine
            .replay_status()
            .players
            .into_iter()
            .find(|player| player.input == input(value))
            .unwrap_or_else(|| panic!("no player {value}"))
    };
    let entry = |event: u32| EngineReplayPlaylistEntry {
        event,
        angle: None,
        speed: ReplaySpeed::Full,
    };
    let mut engine = engine();
    add_player(&mut engine, 9);
    add_player(&mut engine, 10);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1, 2], 600))),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    replay(
        &mut engine,
        "last-1",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    ticks(&mut engine, FPS);
    replay(
        &mut engine,
        "last-2",
        EngineReplayOperation::CaptureLast { seconds: 5 },
    );
    settle(&mut engine);
    for input_value in [9, 10] {
        replay(
            &mut engine,
            &format!("playlist-{input_value}"),
            EngineReplayOperation::PlaylistSet {
                input: input(input_value),
                entries: vec![entry(1), entry(2)],
            },
        );
    }
    replay(
        &mut engine,
        "cue",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: Some(input(1)),
        },
    );
    replay(
        &mut engine,
        "angle-b",
        EngineReplayOperation::Angle {
            input: input(10),
            angle: input(2),
        },
    );
    replay(
        &mut engine,
        "link",
        EngineReplayOperation::Link {
            input: input(9),
            linked: Some(input(10)),
        },
    );
    replay(
        &mut engine,
        "play",
        EngineReplayOperation::Play {
            input: input(9),
            speed: ReplaySpeed::Full,
        },
    );
    settle(&mut engine);
    ticks(&mut engine, 5 * FPS + 1);
    let a = player(&engine, 9);
    let b = player(&engine, 10);
    assert_eq!(a.event, Some(2), "A moved to its next entry");
    assert_eq!(b.event, Some(2), "B moved to its next entry");
    assert_eq!(a.playlist_index, Some(1));
    assert_eq!(b.playlist_index, Some(1));
    assert!(a.playing && b.playing);
    assert_eq!(a.frame, b.frame);

    // B loses its next entry: at the next end both stop, still on the same
    // event and frame, rather than A moving on alone.
    replay(
        &mut engine,
        "shrink-b",
        EngineReplayOperation::PlaylistSet {
            input: input(10),
            entries: vec![entry(1)],
        },
    );
    replay(
        &mut engine,
        "pause",
        EngineReplayOperation::Pause { input: input(9) },
    );
    replay(
        &mut engine,
        "cue-again",
        EngineReplayOperation::Cue {
            input: input(9),
            event: 1,
            angle: Some(input(1)),
        },
    );
    replay(
        &mut engine,
        "play-again",
        EngineReplayOperation::Play {
            input: input(10),
            speed: ReplaySpeed::Full,
        },
    );
    settle(&mut engine);
    ticks(&mut engine, 5 * FPS + 1);
    let a = player(&engine, 9);
    let b = player(&engine, 10);
    assert_eq!(a.event, Some(1));
    assert_eq!(b.event, Some(1));
    assert!(!a.playing && !b.playing);
    assert_eq!(a.frame, b.frame);
    assert_eq!(a.playlist_index, Some(0));
    assert_eq!(b.playlist_index, Some(0));
}

/// Protocol 2.125: the daemon's free-space sample becomes a predicted
/// recording time at the rate the retained segments were written, with
/// warning, critical, and full levels.
#[test]
fn storage_free_space_predicts_the_recording_time_left() {
    let mut engine = engine();
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1], 600))),
    );
    settle(&mut engine);
    ticks(&mut engine, 12 * FPS);
    assert!(engine.replay_status().storage.is_none());

    // Synthetic segments report 4096 bytes per recorded frame, so one
    // second of one source is 4096 * 25 bytes.
    let rate = fm_engine::SYNTHETIC_REPLAY_FRAME_BYTES * FPS;
    engine.set_replay_storage_free(Some(10 * 1024 * 1024 * 1024));
    let storage = engine.replay_status().storage.expect("a sampled root");
    assert_eq!(storage.bytes_per_second, rate);
    assert_eq!(
        storage.remaining_seconds,
        Some(10 * 1024 * 1024 * 1024 / rate)
    );
    assert_eq!(storage.level, fm_engine::EngineReplayStorageLevel::Ok);

    // Five seconds left is critical; five minutes is a warning.
    engine.set_replay_storage_free(Some(5 * rate));
    assert_eq!(
        engine.replay_status().storage.unwrap().level,
        fm_engine::EngineReplayStorageLevel::Critical
    );
    engine.set_replay_storage_free(Some(300 * rate));
    assert_eq!(
        engine.replay_status().storage.unwrap().level,
        fm_engine::EngineReplayStorageLevel::Warning
    );
    // No free space is full; no sample hides the line again.
    engine.set_replay_storage_free(Some(0));
    let storage = engine.replay_status().storage.unwrap();
    assert_eq!(storage.level, fm_engine::EngineReplayStorageLevel::Full);
    assert_eq!(storage.remaining_seconds, Some(0));
    engine.set_replay_storage_free(None);
    assert!(engine.replay_status().storage.is_none());
}
