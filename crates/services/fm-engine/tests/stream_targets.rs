//! Stream destinations (OR-006): five independently controlled
//! destinations, the group start and stop that never touch a recorder, a
//! retry only for a running destination, and destination edits that are
//! validated, undoable, refused while running, and never reveal the key.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineEvent, EngineStreamControl,
    EngineStreamTargetSpec, ShowState,
};
use fm_model::{StartupPolicy, StreamProtocol, StreamReconnectPolicy};
use fm_switcher::{RecorderSlot, StreamTargetId};
use fm_types::{FrameRate, InputId, OutputId};

const KEY: &str = "live-secret-key-0001";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn output() -> OutputId {
    OutputId::new(NonZeroU128::new(30).unwrap())
}

fn target(value: u128) -> StreamTargetId {
    StreamTargetId::from_non_zero(NonZeroU128::new(value).unwrap())
}

fn spec(name: &str) -> EngineStreamTargetSpec {
    EngineStreamTargetSpec {
        name: name.to_owned(),
        protocol: StreamProtocol::Rtmp,
        endpoint: "ingest.example.test:1935/live".to_owned(),
        backup_endpoint: None,
        key: Some(KEY.to_owned()),
        output: output(),
        startup: StartupPolicy::Stopped,
        video_bitrate_kbps: 4_500,
        reconnect_initial_delay_ms: 500,
        reconnect_max_delay_ms: 4_000,
        reconnect_max_attempts: Some(6),
        ladder: None,
        srt_latency_ms: None,
        srt_passphrase: None,
    }
}

fn set(target_id: u128, spec: EngineStreamTargetSpec) -> EngineCommand {
    EngineCommand::SetStreamTarget {
        target: target(target_id),
        spec: Some(Box::new(spec)),
        position: None,
    }
}

fn remove(target_id: u128) -> EngineCommand {
    EngineCommand::SetStreamTarget {
        target: target(target_id),
        spec: None,
        position: None,
    }
}

fn undo() -> EngineCommand {
    EngineCommand::Undo {
        expected_edit: None,
    }
}

fn redo() -> EngineCommand {
    EngineCommand::Redo {
        expected_edit: None,
    }
}

/// The authored destination order and the desired roster order.
fn order(engine: &Engine) -> (Vec<u128>, Vec<u128>) {
    (
        engine
            .show()
            .stream_targets()
            .iter()
            .map(|target| target.id().get().get())
            .collect(),
        engine
            .show()
            .streams()
            .iter()
            .map(|stream| stream.id().get().get())
            .collect(),
    )
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "stream show",
            vec![
                (input(1), "Input 1".to_owned()),
                (input(2), "Input 2".to_owned()),
            ],
            input(1),
            input(2),
        )
        .unwrap()
        .with_outputs(vec![(output(), "Program".to_owned())])
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn submit(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = submit(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key} was rejected: {:?}",
        outcome.receipt
    );
    outcome
}

/// The rejection code and first field issue code.
fn refusal(outcome: &EngineCommandOutcome) -> (RejectionCode, Option<String>) {
    let rejection = &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection;
    (
        rejection.code,
        rejection.fields.first().map(|field| field.code.clone()),
    )
}

/// An engine with five keyed destinations, 1..=5.
fn five() -> Engine {
    let mut engine = engine();
    for id in 1..=5 {
        accept(
            &mut engine,
            &format!("add-{id}"),
            set(id, spec(&format!("Dest {id}"))),
        );
    }
    engine
}

fn running(engine: &Engine) -> Vec<bool> {
    (1..=5)
        .map(|id| engine.show().stream_running(target(id)).unwrap())
        .collect()
}

#[test]
fn five_destinations_are_added_with_their_policy_and_a_sixth_is_refused() {
    let mut engine = five();
    assert_eq!(engine.show().stream_targets().len(), 5);
    let stored = engine.show().stream_target(target(3)).unwrap();
    assert_eq!(stored.name(), "Dest 3");
    assert_eq!(
        stored.reconnect(),
        StreamReconnectPolicy::new(500, 4_000, Some(6)).unwrap()
    );
    assert_eq!(stored.key().unwrap().expose_secret(), KEY);
    assert_eq!(running(&engine), [false; 5]);

    let sixth = submit(&mut engine, "add-6", set(6, spec("Dest 6")));
    assert_eq!(
        refusal(&sixth),
        (
            RejectionCode::InvalidCommand,
            Some("too_many_streams".to_owned())
        )
    );
    assert_eq!(engine.show().stream_targets().len(), 5);
}

#[test]
fn each_destination_starts_and_stops_independently() {
    let mut engine = five();
    accept(
        &mut engine,
        "start-2",
        EngineCommand::StreamStart { target: target(2) },
    );
    accept(
        &mut engine,
        "start-4",
        EngineCommand::StreamStart { target: target(4) },
    );
    assert_eq!(running(&engine), [false, true, false, true, false]);
    accept(
        &mut engine,
        "stop-2",
        EngineCommand::StreamStop { target: target(2) },
    );
    assert_eq!(running(&engine), [false, false, false, true, false]);
}

#[test]
fn group_start_and_stop_reach_every_destination_and_never_a_recorder() {
    let mut engine = five();
    let recorder = RecorderSlot::new(1).unwrap();
    accept(&mut engine, "rec", EngineCommand::RecordStart { recorder });
    engine.tick().unwrap();

    let started = accept(
        &mut engine,
        "start-all",
        EngineCommand::StreamControl(EngineStreamControl::StartAll),
    );
    assert_eq!(running(&engine), [true; 5]);
    assert!(matches!(
        &started.events[0].payload,
        EngineEvent::StreamsChanged { streams } if streams.iter().all(|stream| stream.desired_running)
    ));
    assert!(
        started
            .events
            .iter()
            .all(|event| !matches!(event.payload, EngineEvent::RecordingChanged { .. }))
    );
    assert!(engine.show().desired_switcher().recording_desired(recorder));

    accept(
        &mut engine,
        "stop-all",
        EngineCommand::StreamControl(EngineStreamControl::StopAll),
    );
    assert_eq!(running(&engine), [false; 5]);
    engine.tick().unwrap();
    assert!(
        engine.show().desired_switcher().recording_desired(recorder),
        "stopping every stream leaves the recording running"
    );
    assert!(engine.realized_switcher().recording_desired(recorder));
}

#[test]
fn retry_is_only_for_a_running_destination() {
    let mut engine = five();
    let stopped = submit(
        &mut engine,
        "retry-stopped",
        EngineCommand::StreamControl(EngineStreamControl::Retry { target: target(1) }),
    );
    assert_eq!(refusal(&stopped).0, RejectionCode::Conflict);
    let unknown = submit(
        &mut engine,
        "retry-unknown",
        EngineCommand::StreamControl(EngineStreamControl::Retry { target: target(9) }),
    );
    assert_eq!(refusal(&unknown).0, RejectionCode::NotFound);

    accept(
        &mut engine,
        "start-1",
        EngineCommand::StreamStart { target: target(1) },
    );
    accept(
        &mut engine,
        "retry",
        EngineCommand::StreamControl(EngineStreamControl::Retry { target: target(1) }),
    );
    assert_eq!(running(&engine), [true, false, false, false, false]);
}

#[test]
fn edits_and_removal_are_refused_while_running_and_validated() {
    let mut engine = five();
    accept(
        &mut engine,
        "start-1",
        EngineCommand::StreamStart { target: target(1) },
    );
    let edit = submit(&mut engine, "edit-running", set(1, spec("Renamed")));
    assert_eq!(
        refusal(&edit),
        (RejectionCode::Conflict, Some("stream_running".to_owned()))
    );
    let remove = submit(&mut engine, "remove-running", remove(1));
    assert_eq!(
        refusal(&remove),
        (RejectionCode::Conflict, Some("stream_running".to_owned()))
    );

    let duplicate = submit(&mut engine, "dup", set(2, spec("dest 3")));
    assert_eq!(
        refusal(&duplicate),
        (RejectionCode::Conflict, Some("duplicate_name".to_owned()))
    );
    let mut bad_reconnect = spec("Dest 2");
    bad_reconnect.reconnect_max_delay_ms = 100;
    let refused = submit(&mut engine, "bad-reconnect", set(2, bad_reconnect));
    assert_eq!(
        refusal(&refused),
        (
            RejectionCode::InvalidCommand,
            Some("invalid_reconnect".to_owned())
        )
    );
    let mut keyless = spec("Dest 2");
    keyless.key = None;
    let refused = submit(&mut engine, "keyless", set(2, keyless));
    assert_eq!(refusal(&refused).0, RejectionCode::InvalidCommand);
    let mut wrong_output = spec("Dest 2");
    wrong_output.output = OutputId::new(NonZeroU128::new(31).unwrap());
    let refused = submit(&mut engine, "wrong-output", set(2, wrong_output));
    assert_eq!(refusal(&refused).0, RejectionCode::NotFound);

    // A refusal never echoes the key.
    let mut bad_key = spec("Dest 2");
    bad_key.key = Some(format!("{KEY} with spaces\n"));
    let refused = submit(&mut engine, "bad-key", set(2, bad_key));
    assert!(!format!("{:?}", refused.receipt).contains(KEY));
}

#[test]
fn destination_edits_are_undoable_and_events_carry_the_configuration() {
    let mut engine = five();
    let mut edited = spec("Dest 2 backup");
    edited.backup_endpoint = Some("backup.example.test:1935/live".to_owned());
    edited.reconnect_max_attempts = None;
    let outcome = accept(&mut engine, "edit-2", set(2, edited));
    match &outcome.events[0].payload {
        EngineEvent::StreamTargetChanged {
            target: changed,
            config: Some(config),
            streams,
        } => {
            assert_eq!(*changed, target(2));
            assert_eq!(config.name(), "Dest 2 backup");
            assert_eq!(config.reconnect().max_attempts(), None);
            assert_eq!(streams.len(), 5);
        }
        other => panic!("unexpected event {other:?}"),
    }
    accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        engine.show().stream_target(target(2)).unwrap().name(),
        "Dest 2"
    );

    accept(&mut engine, "remove-5", remove(5));
    assert_eq!(engine.show().stream_targets().len(), 4);
    assert_eq!(engine.show().stream_running(target(5)), None);
    accept(
        &mut engine,
        "undo-remove",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().stream_targets().len(), 5);
    assert_eq!(
        engine
            .show()
            .stream_target(target(5))
            .unwrap()
            .key()
            .unwrap()
            .expose_secret(),
        KEY
    );
}

/// Undoing a removal puts the destination back where it was, in both the
/// authored list and the desired roster, and redo and undo again agree.
#[test]
fn undoing_a_removal_restores_the_destination_at_its_old_place() {
    let mut engine = engine();
    for id in 1..=3 {
        accept(
            &mut engine,
            &format!("add-{id}"),
            set(id, spec(&format!("Dest {id}"))),
        );
    }
    assert_eq!(order(&engine), (vec![1, 2, 3], vec![1, 2, 3]));
    accept(&mut engine, "remove-2", remove(2));
    assert_eq!(order(&engine), (vec![1, 3], vec![1, 3]));
    accept(&mut engine, "undo-remove", undo());
    assert_eq!(order(&engine), (vec![1, 2, 3], vec![1, 2, 3]));
    accept(&mut engine, "redo-remove", redo());
    assert_eq!(order(&engine), (vec![1, 3], vec![1, 3]));
    accept(&mut engine, "undo-again", undo());
    assert_eq!(order(&engine), (vec![1, 2, 3], vec![1, 2, 3]));

    // Removing the first one comes back first, and an edit keeps its place.
    accept(&mut engine, "remove-1", remove(1));
    accept(&mut engine, "undo-first", undo());
    assert_eq!(order(&engine), (vec![1, 2, 3], vec![1, 2, 3]));
    accept(&mut engine, "edit-2", set(2, spec("Dest 2 renamed")));
    assert_eq!(order(&engine).0, vec![1, 2, 3]);
    accept(&mut engine, "undo-edit", undo());
    assert_eq!(order(&engine).0, vec![1, 2, 3]);
    // An add undone and redone returns to the end.
    accept(&mut engine, "add-4", set(4, spec("Dest 4")));
    accept(&mut engine, "undo-add", undo());
    assert_eq!(order(&engine).0, vec![1, 2, 3]);
    accept(&mut engine, "redo-add", redo());
    assert_eq!(order(&engine), (vec![1, 2, 3, 4], vec![1, 2, 3, 4]));
    // An explicit position places a new destination.
    accept(
        &mut engine,
        "add-5-first",
        EngineCommand::SetStreamTarget {
            target: target(5),
            spec: Some(Box::new(spec("Dest 5"))),
            position: Some(0),
        },
    );
    assert_eq!(order(&engine), (vec![5, 1, 2, 3, 4], vec![5, 1, 2, 3, 4]));
}

/// An SRT destination carries its latency and an optional passphrase that
/// survives undo and never reaches a refusal or `Debug`.
#[test]
fn srt_destinations_carry_latency_and_a_secret_passphrase() {
    const PASSPHRASE: &str = "srt-pass-phrase-0001";
    let srt = |latency: Option<u32>, passphrase: Option<&str>| EngineStreamTargetSpec {
        protocol: StreamProtocol::Srt,
        endpoint: "srt.example.test:9000".to_owned(),
        srt_latency_ms: latency,
        srt_passphrase: passphrase.map(str::to_owned),
        ..spec("SRT relay")
    };
    let mut engine = engine();
    accept(
        &mut engine,
        "add-srt",
        set(1, srt(Some(250), Some(PASSPHRASE))),
    );
    let stored = engine.show().stream_target(target(1)).unwrap();
    assert_eq!(stored.srt_latency_ms(), Some(250));
    assert_eq!(stored.srt_passphrase().unwrap().expose_secret(), PASSPHRASE);
    assert!(!format!("{stored:?}").contains(PASSPHRASE));
    assert!(!format!("{:?}", srt(Some(250), Some(PASSPHRASE))).contains(PASSPHRASE));

    // SRT needs its latency in range; other protocols carry none, and a
    // passphrase off SRT is refused.
    for (key, bad) in [
        ("no-latency", srt(None, None)),
        ("low-latency", srt(Some(19), None)),
        ("high-latency", srt(Some(8_001), None)),
        (
            "rtmp-latency",
            EngineStreamTargetSpec {
                srt_latency_ms: Some(120),
                ..spec("SRT relay")
            },
        ),
        (
            "rtmp-passphrase",
            EngineStreamTargetSpec {
                srt_passphrase: Some(PASSPHRASE.to_owned()),
                ..spec("SRT relay")
            },
        ),
        ("short-passphrase", srt(Some(120), Some("short"))),
        (
            "spaced-passphrase",
            srt(Some(120), Some("has a space in it")),
        ),
    ] {
        let refused = submit(&mut engine, key, set(1, bad));
        assert_eq!(refusal(&refused).0, RejectionCode::InvalidCommand, "{key}");
        assert!(
            !format!("{:?}", refused.receipt).contains(PASSPHRASE),
            "{key}"
        );
    }

    // Clearing the passphrase is undoable back to the exact secret.
    accept(&mut engine, "clear", set(1, srt(Some(500), None)));
    assert!(
        engine
            .show()
            .stream_target(target(1))
            .unwrap()
            .srt_passphrase()
            .is_none()
    );
    accept(&mut engine, "undo-clear", undo());
    let restored = engine.show().stream_target(target(1)).unwrap();
    assert_eq!(restored.srt_latency_ms(), Some(250));
    assert_eq!(
        restored.srt_passphrase().unwrap().expose_secret(),
        PASSPHRASE
    );
}
