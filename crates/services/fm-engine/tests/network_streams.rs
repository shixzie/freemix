//! IN-015: network stream inputs are added live with their configuration,
//! and `SetNetworkStream` edits are undoable project edits behind the
//! project and input padlocks; refusals name the problem and change
//! nothing.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineInputSource, ShowState,
};
use fm_types::network_stream::{
    MAX_NETWORK_STREAM_INPUTS, NetworkStreamConfig, RtspTransport, StallSlate,
};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "mixer",
        (1..=2)
            .map(|value| (input(value), format!("Input {value}")))
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

fn add(value: u128, url: &str) -> EngineCommand {
    EngineCommand::AddInput {
        input: input(value),
        name: format!("Stream {value}"),
        source: EngineInputSource::NetworkStream(Box::new(NetworkStreamConfig::new(url))),
    }
}

fn set(value: u128, config: NetworkStreamConfig) -> EngineCommand {
    EngineCommand::SetNetworkStream {
        input: input(value),
        config: Box::new(config),
    }
}

#[test]
fn network_inputs_are_added_edited_and_undone_exactly() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, "rtsp://admin:pw@cam.test/live"));
    assert_eq!(
        engine.show().network_stream(input(5)),
        Some(&NetworkStreamConfig::new("rtsp://admin:pw@cam.test/live"))
    );

    let mut edited = NetworkStreamConfig::new("rtsp://admin:pw@cam.test/live2");
    edited.settings.transport = RtspTransport::Tcp;
    edited.settings.jitter_buffer_ms = 900;
    edited.settings.stall = StallSlate::Black;
    let command = set(5, edited.clone());
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    let outcome = accept(&mut engine, "set", command);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::NetworkStreamChanged { input: changed, config }
            if *changed == input(5) && **config == edited
    )));
    assert_eq!(engine.show().network_stream(input(5)), Some(&edited));
    assert_eq!(
        engine.show().network_streams().collect::<Vec<_>>(),
        [(input(5), &edited)]
    );

    accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        engine.show().network_stream(input(5)),
        Some(&NetworkStreamConfig::new("rtsp://admin:pw@cam.test/live"))
    );
    accept(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().network_stream(input(5)), Some(&edited));
}

#[test]
fn invalid_edits_are_refused_and_change_nothing() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, "udp://@:9000"));
    let before = engine.show().network_stream(input(5)).cloned();

    let mut rtsp_only = NetworkStreamConfig::new("udp://@:9000");
    rtsp_only.settings.transport = RtspTransport::Udp;
    let mut too_small = NetworkStreamConfig::new("udp://@:9000");
    too_small.settings.jitter_buffer_ms = 1;
    for (key, value, config, code, field) in [
        (
            "transport",
            5,
            rtsp_only,
            RejectionCode::InvalidCommand,
            "transport",
        ),
        (
            "jitter",
            5,
            too_small,
            RejectionCode::InvalidCommand,
            "jitter_ms",
        ),
        (
            "scheme",
            5,
            NetworkStreamConfig::new("ftp://host/live"),
            RejectionCode::InvalidCommand,
            "url",
        ),
        (
            "generator",
            1,
            NetworkStreamConfig::new("udp://@:9000"),
            RejectionCode::InvalidCommand,
            "input",
        ),
        (
            "unknown",
            9,
            NetworkStreamConfig::new("udp://@:9000"),
            RejectionCode::NotFound,
            "",
        ),
    ] {
        let outcome = execute(&mut engine, key, set(value, config));
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, code, "{key}");
        if !field.is_empty() {
            assert!(
                rejection.fields.iter().any(|issue| issue.field == field),
                "{key}: {rejection:?}"
            );
        }
        assert!(outcome.events.is_empty(), "{key}");
    }
    assert_eq!(engine.show().network_stream(input(5)).cloned(), before);

    // An invalid stream is not added either.
    let outcome = execute(&mut engine, "bad-add", add(6, "udp://host"));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert!(!engine.show().inputs().contains(&input(6)));
}

#[test]
fn the_network_input_count_is_bounded() {
    let mut engine = engine();
    let limit = u128::try_from(MAX_NETWORK_STREAM_INPUTS).unwrap();
    for value in 0..limit {
        accept(
            &mut engine,
            &format!("add-{value}"),
            add(10 + value, &format!("udp://@:{}", 9000 + value)),
        );
    }
    let outcome = execute(&mut engine, "one-too-many", add(100, "udp://@:8999"));
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(
        engine.show().network_streams().count(),
        MAX_NETWORK_STREAM_INPUTS
    );
}

#[test]
fn edits_honour_the_project_and_input_padlocks() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, "udp://@:9000"));
    accept(
        &mut engine,
        "lock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(5)),
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "input-locked",
        set(5, NetworkStreamConfig::new("udp://@:9001")),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    accept(
        &mut engine,
        "unlock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(5)),
            locked: false,
        },
    );
    accept(
        &mut engine,
        "lock-project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "project-locked",
        set(5, NetworkStreamConfig::new("udp://@:9001")),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(
        engine.show().network_stream(input(5)),
        Some(&NetworkStreamConfig::new("udp://@:9000"))
    );
}
