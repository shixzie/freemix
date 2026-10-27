//! OR-013: highlight reels are undoable project edits; an export compiles
//! the reel against the captured events, resolves exact recorded frames
//! through the segment catalog, and protects the segments it reads from
//! retention until it completes, fails, or is cancelled.

use core::num::NonZeroU128;
use std::path::PathBuf;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RefusalReason, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent,
    EngineInputLifecycleEdit, EngineReelExportState, EngineReelOperation, EngineReplayFeed,
    EngineReplayOperation, FrameResult, ShowState,
};
use fm_types::reel::{ReelClip, ReelMusic, ReelSpec};
use fm_types::replay::ReplaySettings;
use fm_types::{FrameRate, InputId};

const FPS: u64 = 25;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "reels",
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
        ClockDomainId::new(NonZeroU128::new(200).unwrap()),
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

fn reel(engine: &mut Engine, key: &str, spec: ReelSpec) -> EngineCommandOutcome {
    accept(
        engine,
        key,
        EngineCommand::Reel(EngineReelOperation::Set(Box::new(spec))),
    )
}

fn delete_reel(engine: &mut Engine, key: &str, id: u64) -> EngineCommandOutcome {
    accept(
        engine,
        key,
        EngineCommand::Reel(EngineReelOperation::Delete { reel: id }),
    )
}

fn export(
    engine: &mut Engine,
    key: &str,
    job: u64,
    reel: u64,
    destination: &str,
) -> EngineCommandOutcome {
    accept(
        engine,
        key,
        EngineCommand::Reel(EngineReelOperation::Export {
            job,
            reel,
            destination: destination.to_owned(),
        }),
    )
}

fn settings(sources: &[u128], retention_seconds: u32) -> ReplaySettings {
    ReplaySettings {
        sources: sources.iter().copied().map(input).collect(),
        retention_seconds,
        storage_root: Some("/srv/replay".to_owned()),
    }
}

fn spec(id: u64, event: u32, angle: u128) -> ReelSpec {
    ReelSpec {
        id,
        name: format!("Reel {id}"),
        clips: vec![ReelClip {
            event,
            angle: input(angle),
            trim_in_frames: 0,
            trim_out_frames: 0,
            speed_percent: 100,
            dissolve_frames: 0,
        }],
        music: None,
    }
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

/// One external recording: the media plane records one frame a tick and
/// closes a one-second segment every second.
struct Recording {
    stream_frame: u64,
    segment_first: Option<(u64, u64)>,
    index: u64,
}

impl Recording {
    fn new() -> Self {
        Self {
            stream_frame: 0,
            segment_first: None,
            index: 0,
        }
    }

    fn record(&mut self, engine: &mut Engine, source: InputId, count: u64) {
        for _ in 0..count {
            let frame = engine.tick().unwrap().frame.get();
            assert!(engine.replay_observe(source, frame, self.stream_frame));
            let (first_frame, first_stream) =
                *self.segment_first.get_or_insert((frame, self.stream_frame));
            self.stream_frame += 1;
            if self.stream_frame - first_stream == FPS {
                self.index += 1;
                assert!(engine.replay_segment_closed(
                    source,
                    self.index,
                    PathBuf::from(format!("/srv/replay/one-{:06}.mp4", self.index)),
                    first_frame,
                    first_stream,
                    FPS,
                    1_000,
                ));
                self.segment_first = None;
            }
        }
    }
}

fn external_engine(sources: &[u128], retention_seconds: u32) -> (Engine, Recording) {
    let mut engine = engine();
    engine.set_replay_feed(EngineReplayFeed::External);
    replay(
        &mut engine,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(sources, retention_seconds))),
    );
    settle(&mut engine);
    (engine, Recording::new())
}

fn capture_last(engine: &mut Engine, key: &str, seconds: u8) {
    replay(engine, key, EngineReplayOperation::CaptureLast { seconds });
    settle(engine);
}

#[test]
#[allow(clippy::too_many_lines)]
fn reel_edits_are_undoable_project_edits_refused_under_the_padlock() {
    let first = spec(1, 7, 1);
    assert_eq!(
        EngineCommand::Reel(EngineReelOperation::Set(Box::new(first.clone()))).kind(),
        EngineCommandKind::ProjectEdit
    );
    assert_eq!(
        EngineCommand::Reel(EngineReelOperation::Delete { reel: 1 }).kind(),
        EngineCommandKind::ProjectEdit
    );
    assert_eq!(
        EngineCommand::Reel(EngineReelOperation::Export {
            job: 1,
            reel: 1,
            destination: "/srv/reels/one.mp4".to_owned(),
        })
        .kind(),
        EngineCommandKind::LiveOperation
    );

    let mut engine = engine();
    let outcome = reel(&mut engine, "set", first.clone());
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ReelChanged {
            operation: EngineReelOperation::Set(changed),
        } if **changed == first
    )));
    assert_eq!(engine.show().reels().to_vec(), vec![first.clone()]);

    // A new reel is undone by removing it again and redone by restoring it.
    accept(
        &mut engine,
        "undo-set",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert!(engine.show().reels().is_empty());
    accept(
        &mut engine,
        "redo-set",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().reels().to_vec(), vec![first.clone()]);

    // A removed reel is restored by undo and removed again by redo.
    let outcome = delete_reel(&mut engine, "delete", 1);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ReelChanged {
            operation: EngineReelOperation::Delete { reel: 1 },
        }
    )));
    assert!(engine.show().reels().is_empty());
    accept(
        &mut engine,
        "undo-delete",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().reels().to_vec(), vec![first.clone()]);
    accept(
        &mut engine,
        "redo-delete",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert!(engine.show().reels().is_empty());

    // Both reel edits honour the project padlock.
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    for (key, command) in [
        (
            "locked-set",
            EngineCommand::Reel(EngineReelOperation::Set(Box::new(first.clone()))),
        ),
        (
            "locked-delete",
            EngineCommand::Reel(EngineReelOperation::Delete { reel: 1 }),
        ),
    ] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(rejection(&outcome).code, RejectionCode::Conflict, "{key}");
        assert_eq!(
            rejection(&outcome).fields[0].code,
            "project_locked",
            "{key}"
        );
    }
    // Export control is live: an unknown job is refused as not found, never
    // as a locked project edit.
    let outcome = execute(
        &mut engine,
        "locked-cancel",
        EngineCommand::Reel(EngineReelOperation::CancelExport { job: 99 }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(rejection(&outcome).fields[0].code, "unknown_export");
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_resolves_exact_frames_protects_segments_and_refuses_partial_or_existing_destinations() {
    let (mut engine, mut recording) = external_engine(&[1], 10);
    recording.record(&mut engine, input(1), 5 * FPS);
    capture_last(&mut engine, "last", 5);
    let event = engine.replay_status().events[0].clone();
    assert!(event.complete);
    reel(&mut engine, "set", spec(1, event.id, 1));
    let total = engine.reel_plan(1).unwrap();
    assert_eq!(total, event.out_frame - event.in_frame);
    export(&mut engine, "export", 1, 1, "/srv/reels/one.mp4");
    let status = engine.reel_status();
    assert_eq!(status.reels.len(), 1);
    assert_eq!(status.reels[0].total_frames, Some(total));
    assert_eq!(status.exports.len(), 1);
    assert_eq!(status.exports[0].state, EngineReelExportState::Queued);
    assert_eq!(status.exports[0].total_frames, total);

    // Frame 0 reads the event's first frame in segment 1; frame 24 reads
    // the last frame segment 1 holds, and frame 25 crosses into segment 2.
    let chunk = engine.reel_export_chunk(1, 32).unwrap();
    assert_eq!(chunk.frames, 0..32);
    assert!(!chunk.completed);
    let first = &chunk.frames_out[0];
    assert_eq!(first.output_frame, 0);
    assert_eq!(first.layers.len(), 1);
    assert_eq!(first.layers[0].input, input(1));
    assert_eq!(
        first.layers[0].path,
        PathBuf::from("/srv/replay/one-000001.mp4")
    );
    assert_eq!(first.layers[0].frame, 0);
    assert_eq!(
        (first.layers[0].weight_num, first.layers[0].weight_den),
        (1, 1)
    );
    let boundary = &chunk.frames_out[24];
    assert_eq!(
        boundary.layers[0].path,
        PathBuf::from("/srv/replay/one-000001.mp4")
    );
    assert_eq!(boundary.layers[0].frame, 24);
    let next = &chunk.frames_out[25];
    assert_eq!(
        next.layers[0].path,
        PathBuf::from("/srv/replay/one-000002.mp4")
    );
    assert_eq!(next.layers[0].frame, 0);

    // An active export keeps its segments when the event is deleted and
    // retention rolls past them.
    replay(
        &mut engine,
        "delete-event",
        EngineReplayOperation::DeleteEvent { event: event.id },
    );
    settle(&mut engine);
    recording.record(&mut engine, input(1), 20 * FPS);
    let pruned = engine.take_replay_pruned();
    assert!(!pruned.is_empty());
    for index in 1..=5 {
        assert!(
            !pruned.contains(&PathBuf::from(format!("/srv/replay/one-{index:06}.mp4"))),
            "segment {index} was pruned while an export read it"
        );
    }
    assert!(pruned.contains(&PathBuf::from("/srv/replay/one-000006.mp4")));

    let mut completed = false;
    while !completed {
        let chunk = engine.reel_export_chunk(1, 64).unwrap();
        completed = chunk.completed;
        assert!(chunk.frames.end <= total);
    }
    assert_eq!(
        engine.reel_status().exports[0].state,
        EngineReelExportState::Finalizing
    );
    engine.reel_export_finished(1, 5_000).unwrap();
    let status = engine.reel_status();
    assert_eq!(status.exports[0].state, EngineReelExportState::Completed);
    assert_eq!(status.exports[0].bytes, Some(5_000));
    assert_eq!(status.exports[0].completed_frames, total);

    // Completing the export releases its protection: the same segments now
    // roll off.
    recording.record(&mut engine, input(1), 2 * FPS);
    let pruned = engine.take_replay_pruned();
    assert!(pruned.contains(&PathBuf::from("/srv/replay/one-000001.mp4")));

    // A partial event is refused, not deferred: the synthetic segment that
    // holds its last frame is still open.
    let mut partial = crate::engine();
    replay(
        &mut partial,
        "configure",
        EngineReplayOperation::Configure(Box::new(settings(&[1], 600))),
    );
    settle(&mut partial);
    ticks(&mut partial, 12 * FPS);
    capture_last(&mut partial, "last", 5);
    let event = partial.replay_status().events[0].clone();
    assert!(!event.complete);
    reel(&mut partial, "set", spec(1, event.id, 1));
    let outcome = execute(
        &mut partial,
        "partial",
        EngineCommand::Reel(EngineReelOperation::Export {
            job: 1,
            reel: 1,
            destination: "/srv/reels/partial.mp4".to_owned(),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "partial_event");

    // An existing destination is refused before any media is read.
    let existing = std::env::temp_dir().join(format!(
        "fm-engine-reel-existing-{}.mp4",
        std::process::id()
    ));
    std::fs::write(&existing, b"already here").unwrap();
    let outcome = execute(
        &mut partial,
        "existing",
        EngineCommand::Reel(EngineReelOperation::Export {
            job: 2,
            reel: 1,
            destination: existing.to_string_lossy().into_owned(),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "destination_exists");
    std::fs::remove_file(existing).unwrap();
}

/// Schema 84: a clip with `dissolve_frames` overlaps the end of the clip
/// before it, so the reel is that much shorter, and every frame of the
/// overlap reads both clips' recorded frames with linear weights summing to
/// one; a dissolve longer than either clip is refused at export.
#[test]
fn a_dissolve_overlaps_two_clips_with_linear_weights() {
    let (mut engine, mut recording) = external_engine(&[1], 60);
    recording.record(&mut engine, input(1), 6 * FPS);
    capture_last(&mut engine, "first", 5);
    recording.record(&mut engine, input(1), 6 * FPS);
    capture_last(&mut engine, "second", 5);
    // A frame later closes the segment holding the second event's last frame.
    recording.record(&mut engine, input(1), FPS);
    let events = engine.replay_status().events;
    let (first, second) = (events[0].clone(), events[1].clone());
    assert!(first.complete && second.complete);
    let first_len = first.out_frame - first.in_frame;
    let second_len = second.out_frame - second.in_frame;
    let mut dissolving = spec(1, first.id, 1);
    dissolving.clips.push(ReelClip {
        dissolve_frames: 10,
        ..spec(1, second.id, 1).clips[0]
    });
    reel(&mut engine, "set", dissolving.clone());
    let total = engine.reel_plan(1).unwrap();
    assert_eq!(total, first_len + second_len - 10);
    export(&mut engine, "export", 1, 1, "/srv/reels/dissolve.mp4");
    let mut frames = Vec::new();
    loop {
        let chunk = engine.reel_export_chunk(1, 64).unwrap();
        frames.extend(chunk.frames_out);
        if chunk.completed {
            break;
        }
    }
    assert_eq!(frames.len(), usize::try_from(total).unwrap());
    let start = first_len - 10;
    for (output, frame) in frames.iter().enumerate() {
        let output = u64::try_from(output).unwrap();
        if output < start || output >= first_len {
            assert_eq!(frame.layers.len(), 1, "frame {output}");
            continue;
        }
        // Overlap frame k weights the outgoing clip (10 - k) / 11 and the
        // incoming one (k + 1) / 11, the outgoing clip first.
        let k = u32::try_from(output - start).unwrap();
        let [outgoing, incoming] = &frame.layers[..] else {
            panic!("frame {output} has {} layers", frame.layers.len());
        };
        assert_eq!((outgoing.weight_num, outgoing.weight_den), (10 - k, 11));
        assert_eq!((incoming.weight_num, incoming.weight_den), (k + 1, 11));
        assert_ne!(
            (&outgoing.path, outgoing.frame),
            (&incoming.path, incoming.frame),
            "frame {output} reads one recorded frame twice"
        );
    }
    engine.reel_export_finished(1, 1_000).unwrap();

    // A dissolve longer than the clips it joins leaves the reel unplayable.
    dissolving.clips[1].dissolve_frames = 600;
    reel(&mut engine, "too-long", dissolving);
    assert!(engine.reel_plan(1).is_err());
    let outcome = execute(
        &mut engine,
        "export-too-long",
        EngineCommand::Reel(EngineReelOperation::Export {
            job: 2,
            reel: 1,
            destination: "/srv/reels/too-long.mp4".to_owned(),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].code, "dissolve_too_long");

    // The first clip always cuts.
    let mut first_dissolves = spec(2, first.id, 1);
    first_dissolves.clips[0].dissolve_frames = 5;
    let outcome = execute(
        &mut engine,
        "first-dissolves",
        EngineCommand::Reel(EngineReelOperation::Set(Box::new(first_dissolves))),
    );
    assert_eq!(rejection(&outcome).fields[0].code, "invalid_dissolve");
}

#[test]
fn an_export_snapshots_the_music_bed_and_an_unusable_bed_is_refused() {
    let (mut engine, mut recording) = external_engine(&[1], 60);
    recording.record(&mut engine, input(1), 6 * FPS);
    capture_last(&mut engine, "first", 5);
    recording.record(&mut engine, input(1), FPS);
    let event = engine.replay_status().events[0].clone();
    assert!(event.complete);
    let bed = ReelMusic {
        path: "/srv/music/bed.wav".to_owned(),
        level_percent: 80,
        duck_percent: 25,
    };
    reel(
        &mut engine,
        "set",
        ReelSpec {
            music: Some(bed.clone()),
            ..spec(1, event.id, 1)
        },
    );
    export(&mut engine, "export", 1, 1, "/srv/reels/music.mp4");
    // A later edit of the reel leaves the accepted export's bed alone.
    reel(&mut engine, "unset", spec(1, event.id, 1));
    let exports = engine.reel_status().exports;
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].music, Some(bed.clone()));

    let outcome = execute(
        &mut engine,
        "relative",
        EngineCommand::Reel(EngineReelOperation::Set(Box::new(ReelSpec {
            music: Some(ReelMusic {
                path: "music/bed.wav".to_owned(),
                ..bed
            }),
            ..spec(2, event.id, 1)
        }))),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].field, "music");
    assert_eq!(rejection(&outcome).fields[0].code, "invalid_music");
}

#[test]
#[allow(clippy::too_many_lines)]
fn cancel_releases_protection_and_a_second_active_export_is_refused() {
    let (mut engine, mut recording) = external_engine(&[1], 600);
    recording.record(&mut engine, input(1), 5 * FPS);
    capture_last(&mut engine, "last", 5);
    let event = engine.replay_status().events[0].clone();
    reel(&mut engine, "set", spec(1, event.id, 1));
    export(&mut engine, "export-one", 1, 1, "/srv/reels/one.mp4");

    // One active export at a time.
    let outcome = execute(
        &mut engine,
        "export-two",
        EngineCommand::Reel(EngineReelOperation::Export {
            job: 2,
            reel: 1,
            destination: "/srv/reels/two.mp4".to_owned(),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "export_active");

    // Cancelling frees the slot for another export.
    accept(
        &mut engine,
        "cancel-one",
        EngineCommand::Reel(EngineReelOperation::CancelExport { job: 1 }),
    );
    assert_eq!(
        engine.reel_status().exports[0].state,
        EngineReelExportState::Cancelled
    );
    export(&mut engine, "export-two-again", 2, 1, "/srv/reels/two.mp4");
    let status = engine.reel_status();
    assert_eq!(status.exports.len(), 2);
    assert_eq!(status.exports[1].job, 2);
    assert_eq!(status.exports[1].state, EngineReelExportState::Queued);

    // A queued export cannot be forgotten; a cancelled one can.
    let outcome = execute(
        &mut engine,
        "forget-active",
        EngineCommand::Reel(EngineReelOperation::ForgetExport { job: 2 }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "export_still_active");
    accept(
        &mut engine,
        "cancel-two",
        EngineCommand::Reel(EngineReelOperation::CancelExport { job: 2 }),
    );
    accept(
        &mut engine,
        "forget-two",
        EngineCommand::Reel(EngineReelOperation::ForgetExport { job: 2 }),
    );
    accept(
        &mut engine,
        "forget-one",
        EngineCommand::Reel(EngineReelOperation::ForgetExport { job: 1 }),
    );
    assert!(engine.reel_status().exports.is_empty());

    // Cancelling an export releases the segments it protected: deleting the
    // event leaves them unprotected, and retention rolls them off once the
    // export is cancelled.
    let (mut engine, mut recording) = external_engine(&[1], 10);
    recording.record(&mut engine, input(1), 5 * FPS);
    capture_last(&mut engine, "last", 5);
    let event = engine.replay_status().events[0].clone();
    reel(&mut engine, "set", spec(1, event.id, 1));
    export(&mut engine, "export", 1, 1, "/srv/reels/one.mp4");
    replay(
        &mut engine,
        "delete-event",
        EngineReplayOperation::DeleteEvent { event: event.id },
    );
    settle(&mut engine);
    recording.record(&mut engine, input(1), 20 * FPS);
    let pruned = engine.take_replay_pruned();
    for index in 1..=5 {
        assert!(
            !pruned.contains(&PathBuf::from(format!("/srv/replay/one-{index:06}.mp4"))),
            "segment {index} was pruned while the export was active"
        );
    }
    accept(
        &mut engine,
        "cancel",
        EngineCommand::Reel(EngineReelOperation::CancelExport { job: 1 }),
    );
    assert_eq!(
        engine.reel_status().exports[0].state,
        EngineReelExportState::Cancelled
    );
    recording.record(&mut engine, input(1), 2 * FPS);
    let pruned = engine.take_replay_pruned();
    assert!(pruned.contains(&PathBuf::from("/srv/replay/one-000001.mp4")));
}

#[test]
fn an_input_a_reel_plays_cannot_be_removed() {
    let mut engine = engine();
    reel(&mut engine, "set", spec(1, 7, 3));

    let outcome = execute(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "input_in_use");
    assert!(matches!(
        rejection(&outcome).reason,
        Some(RefusalReason::InputInUse {
            input: 3,
            usage: fm_command::InputUse::ReplaySource
        })
    ));
    assert!(rejection(&outcome).message.contains("reel 1"));

    // A batch removal refuses the same input.
    let outcome = execute(
        &mut engine,
        "remove-batch",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::RemoveInputs {
            inputs: vec![input(3)],
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "input_in_use");

    // Removing the reel releases the input.
    delete_reel(&mut engine, "delete-reel", 1);
    accept(
        &mut engine,
        "remove-after",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert!(!engine.show().inputs().contains(&input(3)));
}
