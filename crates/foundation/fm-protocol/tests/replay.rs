//! OR-012 instant replay: every `replay_*` command, the durable
//! `replay_changed` event, `replay_status`, and `replay_status_request`
//! round-trip exactly and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, ReplayEventStatus, ReplayFeed, ReplayOperation,
    ReplayPlayerStatus, ReplaySettings, ReplaySourceStatus, ReplaySpeed, ReplayStatusMessage,
    ReplayStatusRequest, ServerIdentity, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::InputId;

fn wire(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input(value: u128) -> InputId {
    wire(value).to_domain()
}

fn prefix() -> String {
    format!("command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid=a\tidempotency_key=k")
}

fn command(payload: CommandPayload) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn operations() -> Vec<(ReplayOperation, &'static str)> {
    vec![
        (
            ReplayOperation::Configure(Box::new(ReplaySettings {
                sources: vec![input(3), input(1)],
                retention_seconds: 600,
                storage_root: Some("/srv/replay a".to_owned()),
            })),
            "replay_configure\tsources=3%2C1\tretention_seconds=600\tstorage_root=%2Fsrv%2Freplay%20a",
        ),
        (
            ReplayOperation::Configure(Box::new(ReplaySettings::default())),
            "replay_configure\tsources=\tretention_seconds=600",
        ),
        (ReplayOperation::MarkIn, "replay_mark_in"),
        (ReplayOperation::MarkOut, "replay_mark_out"),
        (
            ReplayOperation::CaptureMarks { name: None },
            "replay_capture_marks",
        ),
        (
            ReplayOperation::CaptureMarks {
                name: Some("Goal".to_owned()),
            },
            "replay_capture_marks\tname=Goal",
        ),
        (
            ReplayOperation::CaptureLast { seconds: 10 },
            "replay_capture_last\tseconds=10",
        ),
        (
            ReplayOperation::RenameEvent {
                event: 4,
                name: "Save".to_owned(),
            },
            "replay_event_rename\tevent_id=4\tname=Save",
        ),
        (
            ReplayOperation::DeleteEvent { event: 4 },
            "replay_event_delete\tevent_id=4",
        ),
        (
            ReplayOperation::Cue {
                input: wire(9),
                event: 2,
                angle: Some(wire(3)),
            },
            "replay_cue\tinput=9\tevent_id=2\tangle=3",
        ),
        (
            ReplayOperation::Cue {
                input: wire(9),
                event: 2,
                angle: None,
            },
            "replay_cue\tinput=9\tevent_id=2",
        ),
        (
            ReplayOperation::Angle {
                input: wire(9),
                angle: wire(1),
            },
            "replay_angle\tinput=9\tangle=1",
        ),
        (
            ReplayOperation::Play {
                input: wire(9),
                speed: ReplaySpeed::Half,
            },
            "replay_play\tinput=9\tspeed=50",
        ),
        (
            ReplayOperation::Pause { input: wire(9) },
            "replay_pause\tinput=9",
        ),
        (
            ReplayOperation::CueIn { input: wire(9) },
            "replay_cue_in\tinput=9",
        ),
    ]
}

#[test]
fn every_command_and_its_event_have_an_exact_wire_form() {
    let tokens: Vec<_> = operations()
        .iter()
        .map(|(operation, _)| operation.token())
        .collect();
    for token in ReplayOperation::TOKENS {
        assert!(tokens.contains(&token), "{token} untested");
    }
    for (operation, fields) in operations() {
        let line = round_trip(&command(CommandPayload::Replay(operation.clone())));
        assert_eq!(line, format!("{}\tpayload={fields}\n", prefix()));
        let line = round_trip(&event(EventPayload::ReplayChanged {
            frame: 120,
            operation: operation.clone(),
        }));
        let (token, rest) = fields.split_once('\t').unwrap_or((fields, ""));
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!("\t{rest}")
        };
        assert!(
            line.contains(&format!(
                "\trevision=4\tevent=replay_changed\tframe=120\toperation={token}{rest}\tundo_depth=0"
            )),
            "{line}"
        );
        assert_eq!(operation.is_live(), !token.ends_with("configure"));
    }
}

#[test]
fn commands_are_strict() {
    let prefix = prefix();
    for fields in [
        // Unknown operation, and fields out of contract.
        "replay_rewind\tinput=9",
        "replay_capture_last\tseconds=7",
        "replay_capture_last\tseconds=010",
        "replay_play\tinput=9\tspeed=60",
        "replay_play\tinput=0\tspeed=50",
        "replay_pause",
        "replay_cue\tinput=9",
        "replay_mark_in\tinput=9",
        "replay_event_rename\tevent_id=1\tname=",
        "replay_event_rename\tevent_id=1\tname=%09",
        "replay_configure\tsources=1%2C1\tretention_seconds=600",
        "replay_configure\tsources=1%2C2%2C3%2C4%2C5\tretention_seconds=600",
        "replay_configure\tsources=1\tretention_seconds=9",
        "replay_configure\tsources=1\tretention_seconds=600\tstorage_root=relative",
        "replay_configure\tsources=1",
    ] {
        refused(&format!("{prefix}\tpayload={fields}\n"));
    }
    // An unusable name is refused at encode time too.
    assert!(
        encode_line(&command(CommandPayload::Replay(
            ReplayOperation::RenameEvent {
                event: 1,
                name: " ".to_owned(),
            }
        )))
        .is_err()
    );
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "e".into(),
        project_id: "p".into(),
        state_epoch: 1,
        log_id: "l".into(),
    }
}

fn status(request_id: Option<&str>) -> ReplayStatusMessage {
    ReplayStatusMessage {
        server: server(),
        sequence: 7,
        request_id: request_id.map(str::to_owned),
        feed: ReplayFeed::Recorded,
        settings: ReplaySettings {
            sources: vec![input(1), input(2)],
            retention_seconds: 120,
            storage_root: None,
        },
        root: "/var/lib/freemix/replay".to_owned(),
        live_frame: Some(3_000),
        mark_in: Some(2_900),
        mark_out: None,
        recorded: vec![
            ReplaySourceStatus {
                input: wire(1),
                segments: 60,
                bytes: 9_000_000,
                first_frame: Some(0),
                last_frame: Some(3_000),
            },
            ReplaySourceStatus {
                input: wire(2),
                segments: 0,
                bytes: 0,
                first_frame: None,
                last_frame: None,
            },
        ],
        events: vec![
            ReplayEventStatus {
                id: 1,
                name: "Goal: 1:0; header".to_owned(),
                in_frame: 2_500,
                out_frame: 2_750,
                angles: vec![wire(1), wire(2)],
                complete: true,
            },
            ReplayEventStatus {
                id: 3,
                name: "Last 5 s".to_owned(),
                in_frame: 2_875,
                out_frame: 3_000,
                angles: vec![wire(1)],
                complete: false,
            },
        ],
        players: vec![
            ReplayPlayerStatus {
                input: wire(9),
                event: Some(1),
                angle: Some(wire(2)),
                speed: ReplaySpeed::Quarter,
                playing: true,
                frame: Some(2_510),
            },
            ReplayPlayerStatus {
                input: wire(10),
                event: None,
                angle: None,
                speed: ReplaySpeed::Full,
                playing: false,
                frame: None,
            },
        ],
    }
}

#[test]
fn status_and_its_request_have_an_exact_wire_form() {
    let line = round_trip(&WireMessage::ReplayStatus(Box::new(status(None))));
    assert!(
        line.starts_with(
            "replay_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\tsequence=7\t\
             feed=recorded\tsources=1%2C2\tretention_seconds=120\t\
             root=%2Fvar%2Flib%2Ffreemix%2Freplay\tlive_frame=3000\tmark_in=2900\t\
             recorded=1%3A60%3A9000000%3A0%3A3000%3B2%3A0%3A0%3A%3A\t"
        ),
        "{line}"
    );
    assert!(
        line.contains("\tplayers=9%3A1%3A2%3A25%3A1%3A2510%3B10%3A%3A%3A100%3A0%3A\n"),
        "{line}"
    );
    round_trip(&WireMessage::ReplayStatus(Box::new(status(Some("r-1")))));
    let empty = ReplayStatusMessage {
        recorded: Vec::new(),
        events: Vec::new(),
        players: Vec::new(),
        live_frame: None,
        mark_in: None,
        settings: ReplaySettings::default(),
        feed: ReplayFeed::Synthetic,
        ..status(None)
    };
    round_trip(&WireMessage::ReplayStatus(Box::new(empty)));
    let line = round_trip(&WireMessage::ReplayStatusRequest(ReplayStatusRequest {
        request_id: "r-2".to_owned(),
    }));
    assert_eq!(line, "replay_status_request\trequest_id=r-2\n");

    let good = encode_line(&WireMessage::ReplayStatus(Box::new(status(None)))).unwrap();
    for (from, to) in [
        ("feed=recorded", "feed=live"),
        ("players=9%3A1", "players=9%3Ax"),
        ("%3A25%3A1%3A2510", "%3A30%3A1%3A2510"),
        ("recorded=1%3A60", "recorded=1%3A-1"),
        ("live_frame=3000", "live_frame=03000"),
    ] {
        assert!(good.contains(from), "{from}");
        refused(&good.replacen(from, to, 1));
    }
    refused("replay_status_request\n");
}
