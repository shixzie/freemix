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
    }
}

fn set(target_id: u128, spec: EngineStreamTargetSpec) -> EngineCommand {
    EngineCommand::SetStreamTarget {
        target: target(target_id),
        spec: Some(Box::new(spec)),
    }
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
    let remove = submit(
        &mut engine,
        "remove-running",
        EngineCommand::SetStreamTarget {
            target: target(1),
            spec: None,
        },
    );
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

    accept(
        &mut engine,
        "remove-5",
        EngineCommand::SetStreamTarget {
            target: target(5),
            spec: None,
        },
    );
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
