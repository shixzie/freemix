//! Protocol 2.52 stream destinations (OR-006): the per-destination retry,
//! the group start and stop, the destination edit and removal commands, the
//! `stream_target_changed` durable event, the snapshot's `stream_targets`,
//! and the status sample's reconnect, uptime, and bitrate fields — exactly,
//! and strictly, with the stream key never reported back.

use std::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, MAX_STREAM_TARGETS, ProtocolVersion, SecretEdit,
    StreamRealizedState, StreamReconnect, StreamStatus, StreamTargetConfig, StreamTargetEntry,
    StreamTargetProtocol, WireMessage, WireOutputId, WireStreamTargetId, decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.52\tid=a\tidempotency_key=k";
const KEY: &str = "live-secret-key-0001";

fn target(value: u128) -> WireStreamTargetId {
    WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
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

fn config() -> StreamTargetConfig {
    StreamTargetConfig {
        name: "Main, ingest".to_owned(),
        protocol: StreamTargetProtocol::Rtmp,
        endpoint: "ingest.example.test:1935/live".to_owned(),
        backup_endpoint: Some("backup.example.test:1935/live".to_owned()),
        output: WireOutputId::new(NonZeroU128::new(30).unwrap()),
        reconcile_on_startup: true,
        video_bitrate_kbps: 4_500,
        reconnect: StreamReconnect {
            initial_delay_ms: 500,
            max_delay_ms: 8_000,
            max_attempts: Some(12),
        },
    }
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 52));
    assert_eq!(MAX_STREAM_TARGETS, 5);
    assert_eq!(
        StreamReconnect::DEFAULT,
        StreamReconnect {
            initial_delay_ms: 250,
            max_delay_ms: 2_000,
            max_attempts: Some(40),
        }
    );
}

#[test]
fn group_and_retry_commands_are_exact() {
    assert_eq!(
        round_trip(&command(CommandPayload::StreamStartAll)),
        format!("{PREFIX}\tpayload=stream_start_all\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::StreamStopAll)),
        format!("{PREFIX}\tpayload=stream_stop_all\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::StreamRetry { target: target(7) })),
        format!("{PREFIX}\tpayload=stream_retry\ttarget=7\n")
    );
    refused(&format!("{PREFIX}\tpayload=stream_retry\n"));
    refused(&format!("{PREFIX}\tpayload=stream_retry\ttarget=0\n"));
    refused(&format!("{PREFIX}\tpayload=stream_start_all\ttarget=7\n"));
}

#[test]
fn destination_edits_round_trip_every_key_edit() {
    for stream_key in [
        SecretEdit::Keep,
        SecretEdit::Clear,
        SecretEdit::Set(KEY.to_owned()),
    ] {
        let line = round_trip(&command(CommandPayload::SetStreamTarget {
            target: target(7),
            config: Box::new(config()),
            stream_key: stream_key.clone(),
        }));
        assert!(line.contains("\tpayload=stream_target_set\t"), "{line}");
        assert!(line.contains("\tstream_protocol=rtmp\t"), "{line}");
        assert!(line.contains("\tstartup=reconcile\t"), "{line}");
        assert!(line.contains("\treconnect=500%3A8000%3A12"), "{line}");
        match stream_key {
            SecretEdit::Keep => assert!(!line.contains("stream_key"), "{line}"),
            SecretEdit::Clear => assert!(
                line.contains("\tstream_key=\t") || line.ends_with("\tstream_key=\n"),
                "{line}"
            ),
            SecretEdit::Set(_) => assert!(line.contains(KEY), "{line}"),
        }
    }
    // Unlimited attempts, no backup, an HLS directory.
    let hls = StreamTargetConfig {
        protocol: StreamTargetProtocol::Hls,
        endpoint: "/var/lib/freemix/live".to_owned(),
        backup_endpoint: None,
        reconcile_on_startup: false,
        reconnect: StreamReconnect {
            max_attempts: None,
            ..StreamReconnect::DEFAULT
        },
        ..config()
    };
    let line = round_trip(&command(CommandPayload::SetStreamTarget {
        target: target(8),
        config: Box::new(hls),
        stream_key: SecretEdit::Keep,
    }));
    assert!(line.contains("\treconnect=250%3A2000%3Anone"), "{line}");
    assert!(line.contains("\tstartup=stopped"), "{line}");
    assert_eq!(
        round_trip(&command(CommandPayload::RemoveStreamTarget {
            target: target(7)
        })),
        format!("{PREFIX}\tpayload=stream_target_remove\ttarget=7\n")
    );
}

#[test]
fn destination_edits_are_strict() {
    let line = encode_line(&command(CommandPayload::SetStreamTarget {
        target: target(7),
        config: Box::new(config()),
        stream_key: SecretEdit::Set(KEY.to_owned()),
    }))
    .unwrap();
    // A redacted echo can never be stored as a key.
    refused(&line.replace(KEY, "***"));
    for (from, to) in [
        ("stream_protocol=rtmp", "stream_protocol=ftp"),
        ("startup=reconcile", "startup=always"),
        ("video_bitrate_kbps=4500", "video_bitrate_kbps=999"),
        ("video_bitrate_kbps=4500", "video_bitrate_kbps=100001"),
        ("reconnect=500%3A8000%3A12", "reconnect=99%3A8000%3A12"),
        ("reconnect=500%3A8000%3A12", "reconnect=500%3A400%3A12"),
        ("reconnect=500%3A8000%3A12", "reconnect=500%3A300001%3A12"),
        ("reconnect=500%3A8000%3A12", "reconnect=500%3A8000%3A0"),
        ("reconnect=500%3A8000%3A12", "reconnect=500%3A8000%3A10001"),
        ("reconnect=500%3A8000%3A12", "reconnect=500%3A8000"),
        ("target=7", "target=0"),
        ("output=30", "output=0"),
    ] {
        assert!(line.contains(from), "{from} in {line}");
        refused(&line.replacen(from, to, 1));
    }
    // Every configuration field is required.
    for field in [
        "\tname=",
        "\tstream_protocol=",
        "\tendpoint=",
        "\toutput=",
        "\tstartup=",
        "\tvideo_bitrate_kbps=",
        "\treconnect=",
    ] {
        let start = line.find(field).unwrap();
        let end = line[start + 1..]
            .find(['\t', '\n'])
            .map_or(line.len(), |offset| start + 1 + offset);
        refused(&format!("{}{}", &line[..start], &line[end..]));
    }
}

#[test]
fn the_changed_event_reports_configuration_without_the_key() {
    let streams = vec![StreamStatus {
        target: target(7),
        name: "Main, ingest".to_owned(),
        desired_running: false,
        realized: StreamRealizedState::Stopped,
        detail: None,
    }];
    let entry = StreamTargetEntry {
        target: target(7),
        config: config(),
        has_key: true,
        ladder_rungs: 0,
    };
    let line = round_trip(&event(EventPayload::StreamTargetChanged {
        target: target(7),
        entry: Some(Box::new(entry)),
        streams: streams.clone(),
    }));
    assert!(line.contains("\tevent=stream_target_changed\t"), "{line}");
    assert!(line.contains("\tconfigured=1"), "{line}");
    assert!(line.contains("\thas_key=1"), "{line}");
    assert!(!line.contains(KEY), "{line}");
    let removed = round_trip(&event(EventPayload::StreamTargetChanged {
        target: target(9),
        entry: None,
        streams,
    }));
    assert!(removed.contains("\tconfigured=0"), "{removed}");
    assert!(!removed.contains("\tname=Main"), "{removed}");
}
