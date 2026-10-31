//! OR-013 highlight reels: every `reel_*` command, the durable
//! `reel_changed` and `reel_removed` events, `reel_status`, and
//! `reel_status_request` round-trip exactly and strictly at Protocol 2.107.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, ProtocolVersion, ReelClip, ReelExportState,
    ReelExportStatus, ReelMusic, ReelOperation, ReelSpec, ReelStatus, ReelStatusMessage,
    ReelStatusRequest, ServerIdentity, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::InputId;

fn wire(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input(value: u128) -> InputId {
    wire(value).to_domain()
}

fn clip(event: u32, angle: u128, trim_in_frames: u32, trim_out_frames: u32, speed: u8) -> ReelClip {
    ReelClip {
        event,
        angle: input(angle),
        trim_in_frames,
        trim_out_frames,
        speed_percent: speed,
        reverse: false,
        dissolve_frames: 0,
    }
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

fn spec() -> ReelSpec {
    ReelSpec {
        id: 1,
        name: "Goals: 1; 2".to_owned(),
        clips: vec![
            clip(3, 1, 0, 0, 100),
            ReelClip {
                dissolve_frames: 9,
                ..clip(4, 2, 12, 6, 50)
            },
        ],
        music: Some(music()),
    }
}

fn music() -> ReelMusic {
    ReelMusic {
        path: "/srv/music/bed:1.wav".to_owned(),
        level_percent: 80,
        duck_percent: 25,
    }
}

fn operations() -> Vec<(ReelOperation, &'static str)> {
    vec![
        (
            ReelOperation::Set(Box::new(spec())),
            "reel_set\treel=1\tname=Goals%3A%201%3B%202\t\
             clips=3%3A1%3A0%3A0%3A100%3A0%3A0%2C4%3A2%3A12%3A6%3A50%3A0%3A9\t\
             music=80%3A25%3A%2Fsrv%2Fmusic%2Fbed%3A1.wav",
        ),
        (ReelOperation::Delete { reel: 2 }, "reel_delete\treel=2"),
        (
            ReelOperation::Export {
                job: 3,
                reel: 1,
                destination: "/srv/reels/goals 1.mp4".to_owned(),
            },
            "reel_export\tjob=3\treel=1\t\
             destination=%2Fsrv%2Freels%2Fgoals%201.mp4",
        ),
        (
            ReelOperation::CancelExport { job: 3 },
            "reel_export_cancel\tjob=3",
        ),
        (
            ReelOperation::ForgetExport { job: 4 },
            "reel_export_forget\tjob=4",
        ),
    ]
}

#[test]
fn every_command_and_its_events_have_an_exact_wire_form() {
    let tokens: Vec<_> = operations()
        .iter()
        .map(|(operation, _)| operation.token())
        .collect();
    for token in ReelOperation::TOKENS {
        assert!(tokens.contains(&token), "{token} untested");
    }
    for (operation, fields) in operations() {
        let line = round_trip(&command(CommandPayload::Reel(operation.clone())));
        assert_eq!(line, format!("{}\tpayload={fields}\n", prefix()));
        match &operation {
            ReelOperation::Set(status) => {
                let line = round_trip(&event(EventPayload::ReelChanged(Box::new(ReelStatus {
                    id: status.id,
                    spec: status.as_ref().clone(),
                    total_frames: None,
                    error: None,
                }))));
                assert!(
                    line.contains(&format!(
                        "\trevision=4\tevent=reel_changed\t{}\tundo_depth=0",
                        fields.strip_prefix("reel_set\t").unwrap()
                    )),
                    "{line}"
                );
            }
            ReelOperation::Delete { reel } => {
                let line = round_trip(&event(EventPayload::ReelRemoved { reel: *reel }));
                assert!(
                    line.contains(&format!(
                        "\trevision=4\tevent=reel_removed\treel={reel}\tundo_depth=0"
                    )),
                    "{line}"
                );
            }
            ReelOperation::Export { .. }
            | ReelOperation::CancelExport { .. }
            | ReelOperation::ForgetExport { .. } => {}
        }
        assert_eq!(
            operation.is_live(),
            !matches!(
                operation,
                ReelOperation::Set(_) | ReelOperation::Delete { .. }
            )
        );
    }
}

#[test]
fn reel_changed_carries_frames_and_a_usable_error() {
    let changed = ReelStatus {
        id: 1,
        spec: spec(),
        total_frames: Some(1_500),
        error: Some("event 3; angle 2: incomplete".to_owned()),
    };
    let line = round_trip(&event(EventPayload::ReelChanged(Box::new(changed.clone()))));
    assert!(
        line.contains(
            "\tevent=reel_changed\treel=1\tname=Goals%3A%201%3B%202\t\
             clips=3%3A1%3A0%3A0%3A100%3A0%3A0%2C4%3A2%3A12%3A6%3A50%3A0%3A9\t\
             music=80%3A25%3A%2Fsrv%2Fmusic%2Fbed%3A1.wav\t\
             total_frames=1500\terror=event%203%3B%20angle%202%3A%20incomplete\tundo_depth=0"
        ),
        "{line}"
    );
    // An error over the bound, and a status whose id and spec disagree, are
    // refused at encode time.
    assert!(
        encode_line(&event(EventPayload::ReelChanged(Box::new(ReelStatus {
            error: Some("a".repeat(161)),
            ..changed.clone()
        }))))
        .is_err()
    );
    assert!(
        encode_line(&event(EventPayload::ReelChanged(Box::new(ReelStatus {
            id: 2,
            ..changed
        }))))
        .is_err()
    );
    // Canonical ids are enforced on decode too.
    let good = encode_line(&event(EventPayload::ReelChanged(Box::new(ReelStatus {
        id: 1,
        spec: spec(),
        total_frames: Some(1_500),
        error: None,
    }))))
    .unwrap();
    for (from, to) in [
        ("reel=1", "reel=01"),
        ("total_frames=1500", "total_frames=01500"),
    ] {
        assert!(good.contains(from), "{from}");
        refused(&good.replacen(from, to, 1));
    }
    let removed = encode_line(&event(EventPayload::ReelRemoved { reel: 2 })).unwrap();
    assert!(removed.contains("\treel=2\t"), "{removed}");
    refused(&removed.replacen("reel=2", "reel=02", 1));
}

#[test]
#[allow(clippy::too_many_lines)] // one refusal per command shape and bound
fn commands_are_strict() {
    let prefix = prefix();
    for fields in [
        // Unknown operation, and fields out of contract.
        "reel_rewind\treel=1",
        "reel_delete",
        "reel_delete\treel=01",
        "reel_delete\treel=1\treel=1",
        "reel_set\treel=1\tname=Goals",
        "reel_set\treel=1\tname= \tclips=1:1:0:0:100:0:0",
        "reel_set\treel=1\tname=Goals\tclips=",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:60:0:0",
        "reel_set\treel=1\tname=Goals\tclips=1:1:216001:0:100:0:0",
        "reel_set\treel=1\tname=Goals\tclips=1:0:0:0:100:0:0",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100:0:",
        // A clip without its dissolve, a first clip that dissolves, an
        // overlong or non-canonical dissolve (Protocol 2.111), and a reverse
        // flag that is not 0 or 1 (Protocol 2.136).
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100:2:0",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100:5",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100:0,2:1:0:0:100:0:601",
        "reel_set\treel=1\tname=Goals\tclips=1:1:0:0:100:0:0,2:1:0:0:100:0:09",
        "reel_export\tjob=1\treel=1\tdestination=relative.mp4",
        "reel_export\tjob=1\treel=1",
        "reel_export\tjob=01\treel=1\tdestination=%2Ftmp%2Fa.mp4",
        "reel_export_cancel",
        "reel_export_forget\tjob=-1",
        "reel_export_cancel\tjob=1\treel=1",
    ] {
        refused(&format!("{prefix}\tpayload={fields}\n"));
    }
    // An unusable name or destination is refused at encode time too.
    assert!(
        encode_line(&command(CommandPayload::Reel(ReelOperation::Set(
            Box::new(ReelSpec {
                id: 1,
                name: " ".to_owned(),
                clips: vec![clip(1, 1, 0, 0, 100)],
                music: None,
            })
        ))))
        .is_err()
    );
    assert!(
        encode_line(&command(CommandPayload::Reel(ReelOperation::Export {
            job: 1,
            reel: 1,
            destination: "relative.mp4".to_owned(),
        })))
        .is_err()
    );
    // A full reel is accepted; one clip more is not.
    let full = |count: u32| {
        let spec = ReelSpec {
            id: 1,
            name: "Full".to_owned(),
            clips: (0..count).map(|event| clip(event, 1, 0, 0, 100)).collect(),
            music: None,
        };
        command(CommandPayload::Reel(ReelOperation::Set(Box::new(spec))))
    };
    assert!(encode_line(&full(64)).is_ok());
    assert!(encode_line(&full(65)).is_err());
    // A music bed needs an absolute, bounded path and levels to 100.
    let bed = |music: ReelMusic| {
        command(CommandPayload::Reel(ReelOperation::Set(Box::new(
            ReelSpec {
                music: Some(music),
                ..spec()
            },
        ))))
    };
    round_trip(&bed(ReelMusic {
        level_percent: 100,
        duck_percent: 0,
        ..music()
    }));
    round_trip(&bed(ReelMusic {
        path: format!(
            "/{}",
            "a".repeat(fm_protocol::MAX_REEL_MUSIC_PATH_BYTES - 1)
        ),
        ..music()
    }));
    for unusable in [
        ReelMusic {
            level_percent: 101,
            ..music()
        },
        ReelMusic {
            duck_percent: 101,
            ..music()
        },
        ReelMusic {
            path: "music/bed.wav".to_owned(),
            ..music()
        },
        ReelMusic {
            path: format!("/{}", "a".repeat(fm_protocol::MAX_REEL_MUSIC_PATH_BYTES)),
            ..music()
        },
    ] {
        assert!(encode_line(&bed(unusable)).is_err());
    }
    let set = encode_line(&command(CommandPayload::Reel(ReelOperation::Set(
        Box::new(spec()),
    ))))
    .unwrap();
    for (from, to) in [
        ("music=80%3A25%3A", "music=080%3A25%3A"),
        ("music=80%3A25%3A", "music=80%3A"),
        ("music=80%3A25%3A", "music=80%3A101%3A"),
        ("music=80%3A25%3A%2Fsrv", "music=80%3A25%3Asrv"),
    ] {
        assert!(set.contains(from), "{from}");
        refused(&set.replacen(from, to, 1));
    }
    // The longest dissolve is accepted on a later clip, never on the first.
    let dissolving = |first: u32, second: u32| {
        command(CommandPayload::Reel(ReelOperation::Set(Box::new(
            ReelSpec {
                id: 1,
                name: "Dissolves".to_owned(),
                clips: vec![
                    ReelClip {
                        dissolve_frames: first,
                        ..clip(1, 1, 0, 0, 100)
                    },
                    ReelClip {
                        dissolve_frames: second,
                        ..clip(2, 1, 0, 0, 100)
                    },
                ],
                music: None,
            },
        ))))
    };
    round_trip(&dissolving(0, fm_protocol::MAX_REEL_DISSOLVE_FRAMES));
    assert!(encode_line(&dissolving(1, 0)).is_err());
    assert!(encode_line(&dissolving(0, fm_protocol::MAX_REEL_DISSOLVE_FRAMES + 1)).is_err());
    let clips = (0..65)
        .map(|event| format!("{event}:1:0:0:100:0"))
        .collect::<Vec<_>>()
        .join(",");
    refused(&format!(
        "{prefix}\tpayload=reel_set\treel=1\tname=Full\tclips={clips}\n"
    ));
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "e".into(),
        project_id: "p".into(),
        state_epoch: 1,
        log_id: "l".into(),
    }
}

fn status(request_id: Option<&str>) -> ReelStatusMessage {
    ReelStatusMessage {
        server: server(),
        sequence: 7,
        request_id: request_id.map(str::to_owned),
        reels: vec![
            ReelStatus {
                id: 1,
                spec: spec(),
                total_frames: Some(1_500),
                error: Some("event 3 is incomplete".to_owned()),
            },
            ReelStatus {
                id: 2,
                spec: ReelSpec {
                    id: 2,
                    name: "Last 5 s".to_owned(),
                    clips: vec![clip(7, 3, 0, 0, 25)],
                    music: None,
                },
                total_frames: None,
                error: None,
            },
        ],
        exports: vec![
            ReelExportStatus {
                job: 1,
                reel: 1,
                destination: "/srv/reels/goals 1.mp4".to_owned(),
                state: ReelExportState::Running,
                completed_frames: 120,
                total_frames: Some(1_500),
                bytes: Some(9_000_000),
                failure: None,
            },
            ReelExportStatus {
                job: 2,
                reel: 2,
                destination: "C:\\Reels\\last.mp4".to_owned(),
                state: ReelExportState::Failed,
                completed_frames: 30,
                total_frames: Some(300),
                bytes: None,
                failure: Some("encoder: disk full; stopped".to_owned()),
            },
            ReelExportStatus {
                job: 3,
                reel: 2,
                destination: "/srv/reels/open.mp4".to_owned(),
                state: ReelExportState::Deferred,
                completed_frames: 0,
                total_frames: Some(250),
                bytes: None,
                failure: None,
            },
        ],
    }
}

#[test]
fn export_state_tokens_are_stable() {
    for token in ReelExportState::TOKENS {
        let state = ReelExportState::from_token(token).unwrap();
        assert_eq!(state.token(), token);
    }
    assert_eq!(ReelExportState::from_token("done"), None);
    assert_eq!(ReelExportState::default(), ReelExportState::Queued);
}

#[test]
fn protocol_2_109_is_current() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 143));
    assert_eq!(prefix(), "command\tprotocol=2.143\tid=a\tidempotency_key=k");
}

#[test]
fn status_and_its_request_have_an_exact_wire_form() {
    let line = round_trip(&WireMessage::ReelStatus(Box::new(status(None))));
    assert!(
        line.starts_with(
            "reel_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\tsequence=7\t\
             reels=1%3AGoals%253A%25201%253B%25202%3A1500%3A\
             event%25203%2520is%2520incomplete%3A\
             80%253A25%253A%252Fsrv%252Fmusic%252Fbed%253A1.wav%3A\
             3%3A1%3A0%3A0%3A100%3A0%3A0%2C4%3A2%3A12%3A6%3A50%3A0%3A9%3B\
             2%3ALast%25205%2520s%3A%3A%3A%3A7%3A3%3A0%3A0%3A25%3A0%3A0\t"
        ),
        "{line}"
    );
    // The destination and failure texts are escaped inside their record and
    // again with the field, so no `:`, `;`, or space reaches the wire raw.
    assert!(
        line.contains(
            "\texports=1%3A1%3A%252Fsrv%252Freels%252Fgoals%25201.mp4%3Arunning%3A120%3A\
             1500%3A9000000%3A%3B\
             2%3A2%3AC%253A%255CReels%255Clast.mp4%3Afailed%3A30%3A300%3A%3A\
             encoder%253A%2520disk%2520full%253B%2520stopped%3B\
             3%3A2%3A%252Fsrv%252Freels%252Fopen.mp4%3Adeferred%3A0%3A250%3A%3A\n"
        ),
        "{line}"
    );
    let answered = round_trip(&WireMessage::ReelStatus(Box::new(status(Some("r-1")))));
    assert!(answered.contains("\trequest_id=r-1\treels="), "{answered}");
    let empty = ReelStatusMessage {
        reels: Vec::new(),
        exports: Vec::new(),
        ..status(None)
    };
    round_trip(&WireMessage::ReelStatus(Box::new(empty)));
    let line = round_trip(&WireMessage::ReelStatusRequest(ReelStatusRequest {
        request_id: "r-2".to_owned(),
    }));
    assert_eq!(line, "reel_status_request\trequest_id=r-2\n");

    let good = encode_line(&WireMessage::ReelStatus(Box::new(status(None)))).unwrap();
    for (from, to) in [
        // Canonical ids: no leading zero.
        ("reels=1%3A", "reels=01%3A"),
        ("exports=1%3A", "exports=01%3A"),
        ("sequence=7", "sequence=07"),
        // One state token, one duplicate reel, one duplicate export job.
        ("%3Arunning%3A", "%3Alive%3A"),
        ("%3B2%3ALast", "%3B1%3ALast"),
        ("%3B2%3A2%3AC", "%3B1%3A2%3AC"),
        // A music bed's level over 100, or a relative path.
        ("80%253A25%253A%252F", "101%253A25%253A%252F"),
        ("80%253A25%253A%252Fsrv", "80%253A25%253Asrv"),
    ] {
        assert!(good.contains(from), "{from}");
        refused(&good.replacen(from, to, 1));
    }
    refused("reel_status_request\n");
    refused("reel_status_request\trequest_id=bad id\n");
}

#[test]
fn malformed_and_oversized_status_records_are_refused() {
    let good = encode_line(&WireMessage::ReelStatus(Box::new(status(None)))).unwrap();
    for (from, to) in [
        ("reels=1%3A", "reels=x%3A"),
        ("exports=1%3A", "exports=1"),
        ("%3Afailed%3A", "%3Adone%3A"),
        ("%3Adeferred%3A", "%3Apending%3A"),
    ] {
        assert!(good.contains(from), "{from}");
        refused(&good.replacen(from, to, 1));
    }
    // A failure text over the bound is refused when decoding too.
    let long = "a".repeat(161);
    refused(&format!(
        "reel_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\tsequence=1\t\
         reels=\texports=1:1:/srv/a.mp4:failed:0:::{long}\n"
    ));
    // More reels or export jobs than one status may list are refused when
    // encoding.
    let reels: Vec<ReelStatus> = (0..=8)
        .map(|id| ReelStatus {
            id,
            spec: ReelSpec {
                id,
                name: format!("Reel {id}"),
                clips: vec![clip(1, 1, 0, 0, 100)],
                music: None,
            },
            total_frames: None,
            error: None,
        })
        .collect();
    let exports: Vec<ReelExportStatus> = (0..=8)
        .map(|job| ReelExportStatus {
            job,
            reel: 1,
            destination: "/srv/a.mp4".to_owned(),
            state: ReelExportState::Queued,
            completed_frames: 0,
            total_frames: None,
            bytes: None,
            failure: None,
        })
        .collect();
    for message in [
        ReelStatusMessage {
            reels,
            exports: Vec::new(),
            ..status(None)
        },
        ReelStatusMessage {
            reels: Vec::new(),
            exports,
            ..status(None)
        },
    ] {
        assert!(
            encode_line(&WireMessage::ReelStatus(Box::new(message))).is_err(),
            "accepted an oversized status"
        );
    }
    // A failure text over the bound is refused when encoding.
    assert!(
        encode_line(&WireMessage::ReelStatus(Box::new(ReelStatusMessage {
            exports: vec![ReelExportStatus {
                job: 1,
                reel: 1,
                destination: "/srv/a.mp4".to_owned(),
                state: ReelExportState::Failed,
                completed_frames: 0,
                total_frames: None,
                bytes: None,
                failure: Some("a".repeat(161)),
            }],
            ..status(None)
        })))
        .is_err()
    );
}
