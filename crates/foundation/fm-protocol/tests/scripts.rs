//! Protocol 2.58 project scripts (RC-011): the `script_set`,
//! `script_remove`, and `script_run` commands, the `script_changed`,
//! `script_removed`, and `script_ran` events, the snapshot's
//! `script_states` count, the `script_state` follow-on record, and the
//! typed `script_failed` refusal — exactly, and strictly. Protocol 2.62
//! adds script waits: `script_stop`, the `run` state, `script_run_changed`,
//! `cue_advance` of list 0, and the `script_running` refusal. Protocol
//! 2.63 adds what a faulted run was refused with.

use core::num::NonZeroU128;

use fm_command::{FaultDetail, MAX_FAULT_MESSAGE_BYTES, RefusalReason, ScriptFailureKind};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, CueTitleText,
    EditHistoryStatus, EngineIdentity, EventCursor, EventMessage, EventPayload,
    FadeToBlackPosition, FadeToBlackState, MAX_SCRIPT_SOURCE_BYTES, ManualTransitionStatus,
    OverlayStatus, PlaylistRunState, ProtocolVersion, RecordingStatus, ScriptRunChange,
    ScriptRunStatus, ScriptSpec, ScriptStateMessage, ScriptStatus, WireInputId, WireMessage,
    WireRecorderSlot, WireTitleFieldId, decode_line, decode_refusal_reason, encode_line,
    encode_refusal_reason,
};

const PREFIX: &str = "command\tprotocol=2.86\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
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

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload,
        edit_history: EditHistoryStatus::EMPTY,
    })
}

/// Lowercase hex of `text`'s UTF-8 bytes.
fn hex(text: &str) -> String {
    use core::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").unwrap();
        out
    })
}

fn detail(reason: Option<RefusalReason>, message: &str) -> FaultDetail {
    FaultDetail {
        reason,
        message: message.to_owned(),
    }
}

fn spec() -> ScriptSpec {
    ScriptSpec {
        name: "Open: \"show\"".to_owned(),
        source: "# ñ\npreview 2\n\ttransition fade, 25\r\n".to_owned(),
    }
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 86));
}

#[test]
fn script_commands_round_trip_exactly() {
    let line = round_trip(&command(CommandPayload::SetScript {
        script: 4,
        spec: Box::new(spec()),
        position: None,
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=script_set\tscript=4\tname=Open%3A%20\"show\"\tsource=\
             2320c3b10a707265766965772032\
             0a097472616e736974696f6e20666164652c2032350d0a\n"
        )
        .replace("%20\"show\"", "%20%22show%22")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::RemoveScript { script: 4 })),
        format!("{PREFIX}\tpayload=script_remove\tscript=4\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::RunScript { script: 9 })),
        format!("{PREFIX}\tpayload=script_run\tscript=9\n")
    );
    // An empty source is a valid draft.
    round_trip(&command(CommandPayload::SetScript {
        script: 1,
        spec: Box::new(ScriptSpec {
            name: "Empty".to_owned(),
            source: String::new(),
        }),
        position: None,
    }));
}

#[test]
fn script_commands_are_strict() {
    for bad in [
        "payload=script_run\tscript=0",
        "payload=script_run\tscript=04",
        "payload=script_run",
        "payload=script_run\tscript=1\tname=A",
        "payload=script_remove\tscript=x",
        "payload=script_set\tscript=1\tname=%20\tsource=",
        "payload=script_set\tscript=1\tname=A\tsource=0",
        "payload=script_set\tscript=1\tname=A\tsource=0A",
        "payload=script_set\tscript=1\tname=A\tsource=ff",
        "payload=script_set\tscript=1\tname=A\tsource=00",
        "payload=script_set\tscript=1\tname=A",
        "payload=script_set\tscript=0\tname=A\tsource=",
    ] {
        let line = format!("{PREFIX}\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }
    let encode = |script: u64, spec: ScriptSpec| {
        encode_line(&command(CommandPayload::SetScript {
            script,
            spec: Box::new(spec),
            position: None,
        }))
    };
    assert!(matches!(
        encode(
            1,
            ScriptSpec {
                name: "A".to_owned(),
                source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES + 1),
            }
        ),
        Err(CodecError::InvalidField {
            field: "source",
            ..
        })
    ));
    assert!(
        encode(
            1,
            ScriptSpec {
                name: "A".to_owned(),
                source: "#".repeat(MAX_SCRIPT_SOURCE_BYTES),
            }
        )
        .is_ok()
    );
    assert!(encode(0, spec()).is_err());
    assert!(
        encode(
            1,
            ScriptSpec {
                name: "x".repeat(65),
                source: String::new(),
            }
        )
        .is_err()
    );
    assert!(encode_line(&command(CommandPayload::RunScript { script: 0 })).is_err());
}

#[test]
fn script_events_and_the_snapshot_record_round_trip_exactly() {
    let status = ScriptStatus {
        id: 4,
        spec: spec(),
    };
    round_trip(&event(EventPayload::ScriptChanged(Box::new(
        status.clone(),
    ))));
    let line = round_trip(&event(EventPayload::ScriptRemoved { script: 4 }));
    assert!(line.contains("\tevent=script_removed\tscript=4"), "{line}");

    let change = ScriptRunChange {
        script: 4,
        frame: 1_000,
        actions: 3,
        instructions: 41,
        run: ScriptRunStatus::Waiting {
            run: 6,
            started_frame: 990,
            due_frame: 1_025,
            waits: 2,
            wait_frames: 35,
            line: 7,
        },
        program: input(3),
        preview: input(4),
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        overlays: OverlayStatus::empty_channels(),
        input_audio_strips: vec![fm_protocol::InputAudioStripStatus {
            input: input(3),
            gain_millidb: -6_000,
            balance_basis_points: 0,
            pan_basis_points: 0,
            group: None,
            muted: true,
            soloed: false,
            follow_video: false,
            delay_samples: 0,
        }],
        streams: Vec::new(),
        recordings: vec![
            RecordingStatus {
                recorder: WireRecorderSlot::new(1).unwrap(),
                desired_active: true,
            },
            RecordingStatus {
                recorder: WireRecorderSlot::new(2).unwrap(),
                desired_active: false,
            },
        ],
        playlist: PlaylistRunState::Stopped,
        titles: vec![CueTitleText {
            input: input(5),
            field_revision: 2,
            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
            value: fm_protocol::CueTitleValue::Text("3".to_owned()),
        }],
        media: Vec::new(),
    };
    let line = round_trip(&event(EventPayload::ScriptRan(Box::new(change))));
    assert!(
        line.contains(
            "\tevent=script_ran\tscript=4\tframe=1000\tactions=3\tinstructions=41\t\
             run=waiting%3A6%3A990%3A1025%3A2%3A35%3A7\t"
        ),
        "{line}"
    );
    for (from, to) in [
        ("\tactions=3\t", "\tactions=03\t"),
        ("\tactions=3\t", "\tactions=4294967296\t"),
        ("\tscript=4\t", "\tscript=0\t"),
        ("\tinstructions=41\t", "\t"),
    ] {
        let broken = line.replacen(from, to, 1);
        assert!(decode_line(&broken).is_err(), "{from}");
    }

    round_trip(&WireMessage::ScriptState(ScriptStateMessage {
        revision: 12,
        script: status,
        run: ScriptRunStatus::Idle,
    }));
    assert!(decode_line("script_state\trevision=1\tscript=1\tname=A\tsource=\trun=idle\n").is_ok());
    for bad in [
        "script_state\trevision=1\tscript=0\tname=A\tsource=\trun=idle\n",
        "script_state\trevision=1\tscript=1\tname=A\trun=idle\n",
        "script_state\trevision=1\tscript=1\tname=A\tsource=zz\trun=idle\n",
        "script_state\trevision=1\tscript=1\tname=A\tsource=\n",
    ] {
        assert!(decode_line(bad).is_err(), "{bad}");
    }
}

#[test]
fn script_failures_are_typed_refusals() {
    for failure in ScriptFailureKind::ALL {
        let reason = RefusalReason::ScriptFailed {
            failure,
            line: 7,
            column: 3,
        };
        let wire = encode_refusal_reason(&reason).unwrap();
        assert_eq!(wire, format!("script_failed:{}:7:3", failure.token()));
        assert_eq!(decode_refusal_reason(&wire).unwrap(), reason);
    }
    assert!(
        encode_refusal_reason(&RefusalReason::ScriptFailed {
            failure: ScriptFailureKind::Syntax,
            line: 0,
            column: 1,
        })
        .is_err()
    );
}

/// Protocol 2.62 script waits: the run state in every spelling, exactly
/// and strictly; `script_stop`; `script_run_changed`; the clock's
/// `cue_advance` of list 0; and the `script_running` refusal.
#[test]
#[allow(clippy::too_many_lines)] // every run state spelling
fn script_waits_round_trip_exactly() {
    let line = round_trip(&command(CommandPayload::StopScript { run: None }));
    assert_eq!(line, format!("{PREFIX}\tpayload=script_stop\n"));
    let line = round_trip(&command(CommandPayload::CueAdvance { list: 0, frame: 40 }));
    assert_eq!(
        line,
        format!("{PREFIX}\tpayload=cue_advance\tlist=0\tframe=40\n")
    );

    for (run, wire) in [
        (ScriptRunStatus::Idle, "idle"),
        (
            ScriptRunStatus::Waiting {
                run: 1,
                started_frame: 0,
                due_frame: 25,
                waits: 1,
                wait_frames: 25,
                line: 3,
            },
            "waiting:1:0:25:1:25:3",
        ),
        (
            ScriptRunStatus::Faulted {
                run: 12,
                frame: 30,
                failure: ScriptFailureKind::Refused,
                line: 4,
                column: 1,
                detail: detail(Some(RefusalReason::ProjectLocked), "locked"),
            },
            &format!(
                "faulted:12:30:refused:4:1:{}:6c6f636b6564",
                hex("project_locked")
            ),
        ),
        (
            ScriptRunStatus::Faulted {
                run: 2,
                frame: 30,
                failure: ScriptFailureKind::WaitFramesBudget,
                line: 2,
                column: 3,
                detail: detail(None, "x"),
            },
            "faulted:2:30:wait_frames:2:3:-:78",
        ),
    ] {
        let line = round_trip(&event(EventPayload::ScriptRunChanged {
            script: 2,
            frame: 30,
            run,
        }));
        assert!(
            line.contains(&format!(
                "\tevent=script_run_changed\tscript=2\tframe=30\trun={}\t",
                wire.replace(':', "%3A")
            )),
            "{line}"
        );
    }
    let valid = round_trip(&event(EventPayload::ScriptRunChanged {
        script: 2,
        frame: 30,
        run: ScriptRunStatus::Idle,
    }));
    for bad in [
        "waiting:1:25:25:1:25:1",
        "waiting:1:0:25:0:25:1",
        "waiting:1:0:25:1:0:1",
        "waiting:1:0:25:1:25:0",
        "waiting:0:0:25:1:25:1",
        "waiting:01:0:25:1:25:1",
        "waiting:1:0:25:1:25",
        "waiting:0:25:1:25",
        "faulted:1:30:refused:0:1:-:78",
        "faulted:1:30:bogus:1:1:-:78",
        "faulted:1:30:refused:1:1",
        "faulted:0:30:refused:1:1:-:78",
        "faulted:30:refused:1:1:-:78",
        "running",
        "",
    ] {
        let broken = valid.replacen("run=idle", &format!("run={bad}"), 1);
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    assert!(
        encode_line(&event(EventPayload::ScriptRunChanged {
            script: 2,
            frame: 30,
            run: ScriptRunStatus::Waiting {
                run: 1,
                started_frame: 5,
                due_frame: 5,
                waits: 1,
                wait_frames: 1,
                line: 1,
            },
        }))
        .is_err()
    );
    for (run, line) in [(0, 1), (1, 0)] {
        assert!(
            encode_line(&event(EventPayload::ScriptRunChanged {
                script: 2,
                frame: 30,
                run: ScriptRunStatus::Waiting {
                    run,
                    started_frame: 5,
                    due_frame: 6,
                    waits: 1,
                    wait_frames: 1,
                    line,
                },
            }))
            .is_err()
        );
    }

    let reason = RefusalReason::ScriptRunning { script: 3 };
    let wire = encode_refusal_reason(&reason).unwrap();
    assert_eq!(wire, "script_running:3");
    assert_eq!(decode_refusal_reason(&wire).unwrap(), reason);
    assert!(decode_refusal_reason("script_running:0").is_err());
    assert!(encode_refusal_reason(&RefusalReason::ScriptRunning { script: 0 }).is_err());
}

/// Protocol 2.63: a faulted run says what it was refused with, the refused
/// action's typed reason (hex of its `reason` token, `-` for none) and its
/// bounded single-line English message (hex), exactly and strictly, on
/// `script_run_changed` and `script_state`.
#[test]
fn faulted_runs_carry_the_refusal_reason_and_a_bounded_message() {
    assert_eq!(MAX_FAULT_MESSAGE_BYTES, 256);
    let reason = RefusalReason::UnknownId {
        kind: fm_command::IdKind::Input,
        id: 9,
    };
    let message = "the show has no input named \"Cam 9\"; ñ";
    let run = ScriptRunStatus::Faulted {
        run: 4,
        frame: 75,
        failure: ScriptFailureKind::Refused,
        line: 3,
        column: 1,
        detail: detail(Some(reason.clone()), message),
    };
    let wire = format!(
        "faulted%3A4%3A75%3Arefused%3A3%3A1%3A{}%3A{}",
        hex("unknown_id:input:9"),
        hex(message)
    );
    let line = round_trip(&event(EventPayload::ScriptRunChanged {
        script: 2,
        frame: 75,
        run: run.clone(),
    }));
    assert!(
        line.contains(&format!(
            "\tevent=script_run_changed\tscript=2\tframe=75\trun={wire}\t"
        )),
        "{line}"
    );
    let state = round_trip(&WireMessage::ScriptState(ScriptStateMessage {
        revision: 12,
        script: ScriptStatus {
            id: 2,
            spec: spec(),
        },
        run,
    }));
    assert!(state.ends_with(&format!("\trun={wire}\n")), "{state}");

    // The longest message fits; one byte more, a blank or padded message,
    // a control character, or a reason no token holds is never encoded.
    let longest = "é".repeat(MAX_FAULT_MESSAGE_BYTES / 2);
    let faulted = |detail: FaultDetail| {
        event(EventPayload::ScriptRunChanged {
            script: 2,
            frame: 75,
            run: ScriptRunStatus::Faulted {
                run: 4,
                frame: 75,
                failure: ScriptFailureKind::Runtime,
                line: 1,
                column: 1,
                detail,
            },
        })
    };
    round_trip(&faulted(detail(None, &longest)));
    for bad in [
        detail(None, &format!("{longest}x")),
        detail(None, ""),
        detail(None, " padded"),
        detail(None, "two\nlines"),
        detail(Some(RefusalReason::ScriptRunning { script: 0 }), "x"),
    ] {
        assert!(encode_line(&faulted(bad.clone())).is_err(), "{bad:?}");
    }

    // A decoder refuses the same, and malformed hex or reason tokens.
    let valid = round_trip(&faulted(detail(None, "x")));
    assert!(
        valid.contains("run=faulted%3A4%3A75%3Aruntime%3A1%3A1%3A-%3A78\t"),
        "{valid}"
    );
    for bad in [
        format!("-:{}", hex(&format!("{longest}x"))),
        "-:".to_owned(),
        format!("-:{}", hex(" padded")),
        format!("-:{}", hex("two\nlines")),
        format!("-:{}", hex("z").to_uppercase()),
        "-:7".to_owned(),
        "-:zz".to_owned(),
        "-:ff".to_owned(),
        format!("{}:78", hex("bogus_reason")),
        format!("{}:78", hex("script_running:0")),
        format!("{}:78", hex("project_locked").to_uppercase()),
        "project_locked:78".to_owned(),
        ":78".to_owned(),
        "-:78:extra".to_owned(),
    ] {
        let broken = valid.replacen("%3A-%3A78", &format!("%3A{}", bad.replace(':', "%3A")), 1);
        assert_ne!(broken, valid);
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
}

/// Protocol 2.69: several runs wait at once. `script_stop` names one run
/// or, without `run`, stops them all; `script_runs_stopped` names the
/// scripts a stop of every run idled, exactly and strictly.
#[test]
fn concurrent_runs_stop_one_or_all_exactly() {
    assert_eq!(fm_protocol::MAX_SCRIPT_RUNS, 8);
    let line = round_trip(&command(CommandPayload::StopScript { run: Some(17) }));
    assert_eq!(line, format!("{PREFIX}\tpayload=script_stop\trun=17\n"));
    for bad in ["run=0", "run=017", "run=x", "run=", "run=-1"] {
        let broken = format!("{PREFIX}\tpayload=script_stop\t{bad}\n");
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    assert!(encode_line(&command(CommandPayload::StopScript { run: Some(0) })).is_err());

    let line = round_trip(&event(EventPayload::ScriptRunsStopped {
        frame: 90,
        scripts: vec![1, 4, 16],
    }));
    assert!(
        line.contains("\tevent=script_runs_stopped\tframe=90\tscripts=1%2C4%2C16"),
        "{line}"
    );
    let all: Vec<u64> = (1..=16).collect();
    round_trip(&event(EventPayload::ScriptRunsStopped {
        frame: 90,
        scripts: all,
    }));
    for bad in [
        Vec::new(),
        vec![0, 1],
        vec![2, 1],
        vec![3, 3],
        (1..=17).collect(),
    ] {
        assert!(
            encode_line(&event(EventPayload::ScriptRunsStopped {
                frame: 90,
                scripts: bad.clone(),
            }))
            .is_err(),
            "{bad:?}"
        );
    }
    for bad in ["", "0", "2%2C1", "1%2C1", "1%2C", "01", "a"] {
        let broken = line.replacen("scripts=1%2C4%2C16", &format!("scripts={bad}"), 1);
        assert_ne!(broken, line);
        assert!(decode_line(&broken).is_err(), "{bad}");
    }

    let reason = RefusalReason::bounds("script_runs", 8);
    let wire = encode_refusal_reason(&reason).unwrap();
    assert_eq!(wire, "bounds_exceeded:script_runs:8");
    assert_eq!(decode_refusal_reason(&wire).unwrap(), reason);
}

/// `script_set` carries an optional `position` (the index in the project's
/// script list, below `MAX_SCRIPTS`): an undone removal is journaled with
/// its old place so a replay restores the order.
#[test]
fn script_set_carries_an_optional_position_exactly() {
    let set = |position| {
        command(CommandPayload::SetScript {
            script: 4,
            spec: Box::new(spec()),
            position,
        })
    };
    let without = round_trip(&set(None));
    assert!(!without.contains("position="), "{without}");
    let line = round_trip(&set(Some(3)));
    assert!(line.ends_with("\tposition=3\n"), "{line}");
    let last = u8::try_from(fm_protocol::MAX_SCRIPTS - 1).unwrap();
    let line = round_trip(&set(Some(last)));
    for bad in [
        format!("position={}", fm_protocol::MAX_SCRIPTS),
        "position=03".to_owned(),
        "position=".to_owned(),
        "position=x".to_owned(),
    ] {
        let line = line.replacen(&format!("position={last}"), &bad, 1);
        assert!(decode_line(&line).is_err(), "{line}");
    }
    assert!(encode_line(&set(Some(last + 1))).is_err());
    assert!(
        decode_line(&format!(
            "{PREFIX}\tpayload=script_remove\tscript=4\tposition=1\n"
        ))
        .is_err()
    );
}
