//! IN-003 in the engine: media-file transport commands realize on exact
//! frames; settings changes are padlocked project edits; synthetic sources
//! exercise seek, pause, resume, loop, trim, speed, end triggers, audio
//! track selection, growing files, and frame-accurate positions without a
//! decoder; external sources report growth, completion, and faults.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::fm_playback::{AudioPlan, AudioTrack, FrameIndex, MediaEvent, MediaState};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent,
    EngineMediaCommand, EngineMediaFault, EngineMediaOperation, EngineMediaSeek, EngineMediaSource,
    ShowState,
};
use fm_model::{MediaAudioSpeedPolicy, MediaEndAction, MediaGrowing, MediaPlaybackSettings};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

const CLIP: u128 = 2;

fn rate() -> FrameRate {
    FrameRate::new(25, 1).unwrap()
}

fn engine_with(settings: MediaPlaybackSettings) -> Engine {
    let mut show = ShowState::new(
        "media",
        vec![(input(1), "Camera".into()), (input(CLIP), "Clip".into())],
        input(1),
        input(CLIP),
    )
    .unwrap();
    show.set_media_input(input(CLIP), settings).unwrap();
    let mut engine = Engine::new(
        show,
        rate(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    engine
        .set_media_source(
            input(CLIP),
            Some(EngineMediaSource::synthetic(&settings, rate())),
        )
        .unwrap();
    engine
}

fn engine() -> Engine {
    engine_with(MediaPlaybackSettings::default())
}

fn execute(
    engine: &mut Engine,
    key: &str,
    operation: EngineMediaOperation,
) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(
                format!("command-{key}"),
                IdempotencyKey::new(key),
                EngineCommand::Media(EngineMediaCommand {
                    input: input(CLIP),
                    operation,
                }),
            ),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, operation: EngineMediaOperation) {
    let outcome = execute(engine, key, operation);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
}

/// Ticks once and returns the clip's transport tick.
fn step(engine: &mut Engine) -> fm_engine::fm_playback::MediaTick {
    let frame = engine.tick().unwrap();
    frame
        .media
        .into_iter()
        .find(|media| media.input == input(CLIP))
        .and_then(|media| media.tick)
        .expect("the clip has an open source")
}

#[test]
fn a_synthetic_source_is_cued_stopped_with_two_tracks() {
    let engine = engine();
    let status = engine.media_status(input(CLIP)).unwrap();
    assert!(status.available);
    assert_eq!(status.state, MediaState::Stopped);
    assert_eq!(status.position, 0);
    assert_eq!(status.frames, 250);
    assert!(status.complete);
    assert_eq!(status.playable_end, 250);
    assert_eq!(status.audio_track, Some(1));
    assert_eq!(
        status
            .tracks
            .iter()
            .map(|track| (track.stream_index, track.language.as_deref()))
            .collect::<Vec<_>>(),
        [(1, Some("eng")), (2, Some("spa"))]
    );
    assert_eq!(status.source_rate, Some(rate()));
    assert!(engine.media_status(input(1)).is_none());
    assert!(!engine.media_advancing());
}

#[test]
fn transport_commands_are_live_and_realize_on_their_frame() {
    let mut engine = engine();
    let play = EngineCommand::Media(EngineMediaCommand {
        input: input(CLIP),
        operation: EngineMediaOperation::Play,
    });
    assert_eq!(play.kind(), EngineCommandKind::LiveOperation);
    let outcome = execute(&mut engine, "play", EngineMediaOperation::Play);
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>(),
        [EngineEvent::MediaChanged {
            input: input(CLIP),
            frame: 0,
            operation: EngineMediaOperation::Play,
        }]
    );
    // The projected status already plays; the transport realizes on frame 0.
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().state,
        MediaState::Playing
    );
    assert!(engine.media_advancing());
    for expected in 0..5 {
        let tick = step(&mut engine);
        assert_eq!(tick.frame.map(FrameIndex::get), Some(expected));
        assert_eq!(tick.state, MediaState::Playing);
    }
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!(status.position, 5);
    assert_eq!(
        (
            status.position_time.numerator(),
            status.position_time.denominator()
        ),
        (1, 5)
    );

    accept(&mut engine, "pause", EngineMediaOperation::Pause);
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(5));
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(5));
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().state,
        MediaState::Paused
    );

    accept(
        &mut engine,
        "seek",
        EngineMediaOperation::Seek(EngineMediaSeek::Frame(100)),
    );
    let tick = step(&mut engine);
    assert_eq!(tick.frame.map(FrameIndex::get), Some(100));
    assert!(tick.discontinuity);
    assert!(tick.events.contains(&MediaEvent::Seeked {
        frame: fm_engine::fm_playback::FrameIndex::new(100)
    }));

    // A time seek lands on the frame displayed at that time; out-of-range
    // seeks clamp to the playable range.
    accept(
        &mut engine,
        "seek-time",
        EngineMediaOperation::Seek(EngineMediaSeek::Time {
            numerator: 3,
            denominator: 2,
        }),
    );
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(37));
    accept(
        &mut engine,
        "seek-far",
        EngineMediaOperation::Seek(EngineMediaSeek::Frame(9_999)),
    );
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(249));

    accept(&mut engine, "resume", EngineMediaOperation::Play);
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(249));
    // Hold (the default end action) keeps the final frame.
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().state,
        MediaState::Ended
    );
    accept(&mut engine, "stop", EngineMediaOperation::Stop);
    assert_eq!(step(&mut engine).frame.map(FrameIndex::get), Some(0));
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().state,
        MediaState::Stopped
    );
}

#[test]
fn trimmed_loops_fire_end_triggers_and_count_loops() {
    let settings = MediaPlaybackSettings {
        trim_in: 10,
        trim_out: Some(20),
        end_action: MediaEndAction::Loop,
        end_trigger_lead: Some(3),
        start_frame: 10,
        ..MediaPlaybackSettings::default()
    };
    let mut engine = engine_with(settings);
    accept(&mut engine, "play", EngineMediaOperation::Play);
    let mut frames = Vec::new();
    let mut events = Vec::new();
    for _ in 0..12 {
        let tick = step(&mut engine);
        frames.push(tick.frame.unwrap().get());
        events.extend(tick.events);
    }
    assert_eq!(frames, [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 10, 11]);
    assert!(events.contains(&MediaEvent::EndTrigger {
        remaining_frames: 3
    }));
    assert!(events.contains(&MediaEvent::Looped { count: 1 }));
    assert_eq!(engine.media_status(input(CLIP)).unwrap().loops, 1);
}

#[test]
fn speed_and_audio_policy_change_the_frame_step_and_the_audio_plan() {
    let mut engine = engine();
    accept(&mut engine, "play", EngineMediaOperation::Play);
    accept(
        &mut engine,
        "rate",
        EngineMediaOperation::Rate {
            speed_basis_points: 20_000,
        },
    );
    let frames: Vec<_> = (0..4)
        .map(|_| step(&mut engine).frame.unwrap().get())
        .collect();
    assert_eq!(frames, [0, 2, 4, 6]);
    // Muted at a non-normal rate by default.
    assert!(matches!(step(&mut engine).audio, AudioPlan::Silence { .. }));
    accept(
        &mut engine,
        "resample",
        EngineMediaOperation::AudioPolicy(MediaAudioSpeedPolicy::Resample),
    );
    assert!(matches!(
        step(&mut engine).audio,
        AudioPlan::Source { resample: true, .. }
    ));
    accept(
        &mut engine,
        "quarter",
        EngineMediaOperation::Rate {
            speed_basis_points: 2_500,
        },
    );
    let first = step(&mut engine);
    let second = step(&mut engine);
    assert_eq!(first.frame, second.frame);
    assert!(second.repeated);
    let settings = *engine.show().media_settings(input(CLIP)).unwrap();
    assert_eq!(settings.speed_basis_points, 2_500);
    assert_eq!(settings.audio_policy, MediaAudioSpeedPolicy::Resample);
}

#[test]
fn audio_tracks_switch_by_stream_index_and_unknown_tracks_are_refused() {
    let mut engine = engine();
    accept(
        &mut engine,
        "spanish",
        EngineMediaOperation::AudioTrack(Some(2)),
    );
    let tick = step(&mut engine);
    assert!(tick.events.contains(&MediaEvent::AudioTrackChanged {
        from: Some(1),
        to: Some(2),
    }));
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().audio_track,
        Some(2)
    );
    assert_eq!(
        engine
            .show()
            .media_settings(input(CLIP))
            .unwrap()
            .audio_stream,
        Some(2)
    );
    let refused = execute(
        &mut engine,
        "missing",
        EngineMediaOperation::AudioTrack(Some(7)),
    );
    let rejection = &refused.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    // The file's default track.
    accept(
        &mut engine,
        "default",
        EngineMediaOperation::AudioTrack(None),
    );
    step(&mut engine);
    assert_eq!(
        engine.media_status(input(CLIP)).unwrap().audio_track,
        Some(1)
    );
}

#[test]
fn settings_changes_are_padlocked_project_edits_and_validated() {
    let mut engine = engine();
    let trim = EngineCommand::Media(EngineMediaCommand {
        input: input(CLIP),
        operation: EngineMediaOperation::Trim {
            trim_in: 5,
            trim_out: Some(50),
        },
    });
    assert_eq!(trim.kind(), EngineCommandKind::ProjectEdit);
    for (key, operation, field) in [
        (
            "rate",
            EngineMediaOperation::Rate {
                speed_basis_points: 1_000,
            },
            "speed_basis_points",
        ),
        (
            "trim",
            EngineMediaOperation::Trim {
                trim_in: 9,
                trim_out: Some(9),
            },
            "trim_out",
        ),
        (
            "lead",
            EngineMediaOperation::EndTriggerLead(Some(3_601)),
            "end_trigger_lead",
        ),
    ] {
        let outcome = execute(&mut engine, key, operation);
        let rejection = &outcome.receipt.rejected().unwrap().rejection;
        assert_eq!(rejection.fields[0].field, field, "{key}");
    }
    // A non-media input refuses media commands.
    let outcome = engine
        .execute(
            CommandEnvelope::new(
                "camera",
                IdempotencyKey::new("camera"),
                EngineCommand::Media(EngineMediaCommand {
                    input: input(1),
                    operation: EngineMediaOperation::Play,
                }),
            ),
            0,
        )
        .unwrap();
    assert!(outcome.receipt.rejected().is_some());

    engine
        .execute(
            CommandEnvelope::new(
                "lock",
                IdempotencyKey::new("lock"),
                EngineCommand::SetEditLock {
                    scope: EditLockScope::Input(input(CLIP)),
                    locked: true,
                },
            ),
            0,
        )
        .unwrap();
    let locked = execute(
        &mut engine,
        "locked-trim",
        EngineMediaOperation::EndAction(MediaEndAction::Loop),
    );
    assert!(locked.receipt.rejected().is_some());
    // Transport stays live under the padlock.
    accept(&mut engine, "locked-play", EngineMediaOperation::Play);

    // A trim pulls the start frame inside and clamps the playhead.
    let mut engine = self::engine();
    accept(
        &mut engine,
        "trim",
        EngineMediaOperation::Trim {
            trim_in: 30,
            trim_out: Some(60),
        },
    );
    let settings = *engine.show().media_settings(input(CLIP)).unwrap();
    assert_eq!(
        (settings.trim_in, settings.trim_out, settings.start_frame),
        (30, Some(60), 30)
    );
    assert_eq!(step(&mut engine).frame.unwrap().get(), 30);
    assert_eq!(engine.media_status(input(CLIP)).unwrap().playable_end, 60);
}

#[test]
fn growing_synthetic_sources_wait_at_the_live_edge_and_complete() {
    let settings = MediaPlaybackSettings {
        growing: Some(MediaGrowing {
            live_edge_offset_frames: 2,
            idle_completion_refreshes: 0,
        }),
        speed_basis_points: 20_000,
        ..MediaPlaybackSettings::default()
    };
    let mut engine = engine_with(settings);
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!((status.frames, status.complete), (50, false));
    assert_eq!(status.playable_end, 48);
    assert!(engine.media_advancing());
    accept(&mut engine, "play", EngineMediaOperation::Play);
    let mut events = Vec::new();
    let mut waited = false;
    for _ in 0..60 {
        let tick = step(&mut engine);
        waited |= tick.state == MediaState::Waiting;
        events.extend(tick.events);
    }
    assert!(waited, "2x playback catches the 1x writer");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MediaEvent::LiveEdgeReached { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MediaEvent::LiveEdgeResumed { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MediaEvent::SourceGrew { .. }))
    );
    accept(
        &mut engine,
        "complete",
        EngineMediaOperation::CompleteSource,
    );
    let tick = step(&mut engine);
    assert!(
        tick.events
            .iter()
            .any(|event| matches!(event, MediaEvent::SourceCompleted { .. }))
    );
    assert!(engine.media_status(input(CLIP)).unwrap().complete);

    // An idle completion count completes the source by itself once it
    // stops growing.
    let idle = MediaPlaybackSettings {
        growing: Some(MediaGrowing {
            live_edge_offset_frames: 1,
            idle_completion_refreshes: 3,
        }),
        ..MediaPlaybackSettings::default()
    };
    let mut engine = engine_with(idle);
    for _ in 0..(250 - 50 + 3) {
        engine.tick().unwrap();
    }
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!((status.frames, status.complete), (250, true));
    assert!(!engine.media_advancing());
}

#[test]
fn external_sources_report_growth_completion_and_faults() {
    let settings = MediaPlaybackSettings {
        growing: Some(MediaGrowing {
            live_edge_offset_frames: 1,
            idle_completion_refreshes: 0,
        }),
        ..MediaPlaybackSettings::default()
    };
    let mut engine = engine_with(settings);
    let control_track = AudioTrack {
        stream_index: 4,
        sample_rate: 48_000,
        channels: 2,
        language: Some("eng\u{7}lish-and-much-longer-than-thirty-two-bytes".into()),
        title: Some(String::new()),
        default: false,
    };
    engine
        .set_media_source(
            input(CLIP),
            Some(EngineMediaSource::external(
                FrameRate::new(30_000, 1_001).unwrap(),
                10,
                false,
                vec![control_track.clone(), control_track],
            )),
        )
        .unwrap();
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!(status.tracks.len(), 1, "duplicate stream indexes collapse");
    let language = status.tracks[0].language.as_deref().unwrap();
    assert!(language.len() <= 32 && !language.contains('\u{7}'));
    assert_eq!(status.tracks[0].title, None);
    assert_eq!(status.audio_track, Some(4));

    // Completion of an external source is requested of its decoder.
    accept(
        &mut engine,
        "complete",
        EngineMediaOperation::CompleteSource,
    );
    let frame = engine.tick().unwrap();
    assert!(frame.media[0].completion_requested);
    engine.update_media_source(input(CLIP), 12, true).unwrap();
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!((status.frames, status.complete), (12, true));

    // A shrinking source faults, reported once.
    engine.update_media_source(input(CLIP), 5, true).unwrap();
    let status = engine.media_status(input(CLIP)).unwrap();
    assert!(!status.available);
    assert_eq!(status.fault, Some(EngineMediaFault::SourceTruncated));
    assert!(engine.media_advancing());
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.media[0].faulted,
        Some(EngineMediaFault::SourceTruncated)
    );
    assert_eq!(engine.tick().unwrap().media, Vec::new());
    assert!(!engine.media_advancing());
    // Commands stay accepted while the source is unavailable.
    accept(&mut engine, "play-faulted", EngineMediaOperation::Play);
    assert!(
        engine
            .set_media_fault(input(1), EngineMediaFault::DecodeFailed)
            .is_err()
    );
}
