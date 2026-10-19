//! Protocol 2.58 project scripts (RC-011): the `script_set`,
//! `script_remove`, and `script_run` commands, the `script_changed`,
//! `script_removed`, and `script_ran` events, the snapshot's
//! `script_states` count, the `script_state` follow-on record, and the
//! typed `script_failed` refusal — exactly, and strictly.

use core::num::NonZeroU128;

use fm_command::{RefusalReason, ScriptFailureKind};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, CueTitleText,
    EditHistoryStatus, EngineIdentity, EventCursor, EventMessage, EventPayload,
    FadeToBlackPosition, FadeToBlackState, MAX_SCRIPT_SOURCE_BYTES, ManualTransitionStatus,
    OverlayStatus, PlaylistRunState, ProtocolVersion, RecordingStatus, ScriptRunChange, ScriptSpec,
    ScriptStateMessage, ScriptStatus, WireInputId, WireMessage, WireRecorderSlot, WireTitleFieldId,
    decode_line, decode_refusal_reason, encode_line, encode_refusal_reason,
};

const PREFIX: &str = "command\tprotocol=2.60\tid=a\tidempotency_key=k";

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

fn spec() -> ScriptSpec {
    ScriptSpec {
        name: "Open: \"show\"".to_owned(),
        source: "# ñ\npreview 2\n\ttransition fade, 25\r\n".to_owned(),
    }
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 60));
}

#[test]
fn script_commands_round_trip_exactly() {
    let line = round_trip(&command(CommandPayload::SetScript {
        script: 4,
        spec: Box::new(spec()),
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
            text: "3".to_owned(),
        }],
        media: Vec::new(),
    };
    let line = round_trip(&event(EventPayload::ScriptRan(Box::new(change))));
    assert!(
        line.contains("\tevent=script_ran\tscript=4\tframe=1000\tactions=3\tinstructions=41\t"),
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
    }));
    assert!(decode_line("script_state\trevision=1\tscript=1\tname=A\tsource=\n").is_ok());
    for bad in [
        "script_state\trevision=1\tscript=0\tname=A\tsource=\n",
        "script_state\trevision=1\tscript=1\tname=A\n",
        "script_state\trevision=1\tscript=1\tname=A\tsource=zz\n",
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
