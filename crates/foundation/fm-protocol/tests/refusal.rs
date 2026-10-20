//! Protocol 2.45 typed refusal reasons on `command_result`: every reason
//! kind round-trips exactly, a refusal without one omits the field, and
//! malformed reasons are refused both ways.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandResult, IdKind, InputUse, ProtocolVersion, RefusalReason,
    WireMessage, decode_line, decode_refusal_reason, encode_line, encode_refusal_reason,
};

fn rejected(reason: Option<RefusalReason>) -> WireMessage {
    WireMessage::CommandResult(CommandResult::Rejected {
        id: "c-1".to_owned(),
        code: "conflict".to_owned(),
        message: "input 2 cannot be removed: it is a layer source of scene 100".to_owned(),
        fields: Vec::new(),
        current_revision: 9,
        retryable: false,
        reason,
    })
}

/// One reason of every kind, with its exact wire form.
#[allow(clippy::too_many_lines)] // one entry per reason
fn every_reason() -> Vec<(RefusalReason, &'static str)> {
    let in_use = |usage| RefusalReason::InputInUse { input: 2, usage };
    vec![
        (RefusalReason::PermissionDenied, "permission_denied"),
        (RefusalReason::DeadlineExceeded, "deadline_exceeded"),
        (
            RefusalReason::RevisionConflict { current: 41 },
            "revision_conflict:41",
        ),
        (
            RefusalReason::UnknownId {
                kind: IdKind::Scene,
                id: 100,
            },
            "unknown_id:scene:100",
        ),
        (
            RefusalReason::DuplicateId {
                kind: IdKind::Input,
                id: u128::MAX,
            },
            "duplicate_id:input:340282366920938463463374607431768211455",
        ),
        (RefusalReason::DuplicateName, "duplicate_name"),
        (
            RefusalReason::InvalidName { max_bytes: 128 },
            "invalid_name:128",
        ),
        (RefusalReason::ProjectLocked, "project_locked"),
        (RefusalReason::InputLocked { input: 3 }, "input_locked:3"),
        (in_use(InputUse::Program), "input_in_use:2:program"),
        (in_use(InputUse::Preview), "input_in_use:2:preview"),
        (in_use(InputUse::Transition), "input_in_use:2:transition"),
        (
            in_use(InputUse::ManualTransition),
            "input_in_use:2:manual_transition",
        ),
        (in_use(InputUse::SceneOnAir), "input_in_use:2:scene_on_air"),
        (
            in_use(InputUse::Overlay { channel: 8 }),
            "input_in_use:2:overlay:8",
        ),
        (
            in_use(InputUse::OverlayQueue { channel: 1 }),
            "input_in_use:2:overlay_queue:1",
        ),
        (
            in_use(InputUse::Stinger { slot: 3 }),
            "input_in_use:2:stinger:3",
        ),
        (
            in_use(InputUse::SceneLayer { scene: 100 }),
            "input_in_use:2:scene_layer:100",
        ),
        (
            in_use(InputUse::SceneAudioSource { scene_input: 7 }),
            "input_in_use:2:scene_audio_source:7",
        ),
        (
            in_use(InputUse::AliasTarget { alias: 9 }),
            "input_in_use:2:alias_target:9",
        ),
        (
            in_use(InputUse::PlaylistEntry { entry: 0 }),
            "input_in_use:2:playlist_entry:0",
        ),
        (RefusalReason::DisplayInUse { slot: 2 }, "display_in_use:2"),
        (
            RefusalReason::bounds("inputs", 256),
            "bounds_exceeded:inputs:256",
        ),
        (RefusalReason::NothingToUndo, "nothing_to_undo"),
        (RefusalReason::NothingToRedo, "nothing_to_redo"),
        (RefusalReason::HistoryConflict, "history_conflict"),
        (RefusalReason::HistoryDrift, "history_drift"),
        (
            RefusalReason::StingerRealizing { slot: 4 },
            "stinger_realizing:4",
        ),
        (
            RefusalReason::PlaylistTakeScheduled { frame: 1_800 },
            "playlist_take_scheduled:1800",
        ),
        (
            in_use(InputUse::CueAction { list: 3, cue: 0 }),
            "input_in_use:2:cue_action:3:0",
        ),
        (
            RefusalReason::CueScheduled { frame: 90 },
            "cue_scheduled:90",
        ),
        (RefusalReason::NoCueOnStandby, "no_cue_on_standby"),
        (RefusalReason::CueRunning { cue: 4 }, "cue_running:4"),
        (
            RefusalReason::ScriptFailed {
                failure: fm_command::ScriptFailureKind::Syntax,
                line: 3,
                column: 12,
            },
            "script_failed:syntax:3:12",
        ),
        (
            RefusalReason::ScriptFailed {
                failure: fm_command::ScriptFailureKind::InstructionBudget,
                line: 1,
                column: 1,
            },
            "script_failed:instructions:1:1",
        ),
        (
            RefusalReason::ScriptFailed {
                failure: fm_command::ScriptFailureKind::Refused,
                line: 256,
                column: 4,
            },
            "script_failed:refused:256:4",
        ),
        (RefusalReason::NotDurable, "not_durable"),
        (RefusalReason::RateLimited, "rate_limited"),
        (RefusalReason::RuntimeUnsupported, "runtime_unsupported"),
    ]
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 70));
}

#[test]
fn every_reason_kind_round_trips_exactly_on_a_rejected_result() {
    for kind in IdKind::ALL {
        for reason in [
            RefusalReason::UnknownId { kind, id: 5 },
            RefusalReason::DuplicateId { kind, id: 5 },
        ] {
            let encoded = encode_refusal_reason(&reason).unwrap();
            assert_eq!(decode_refusal_reason(&encoded).unwrap(), reason);
        }
    }
    for (reason, wire) in every_reason() {
        assert_eq!(encode_refusal_reason(&reason).unwrap(), wire);
        assert_eq!(decode_refusal_reason(wire).unwrap(), reason);
        assert!(wire.starts_with(reason.kind()), "{wire}");
        let message = rejected(Some(reason.clone()));
        let line = encode_line(&message).unwrap();
        let escaped = wire.replace(':', "%3A");
        assert!(
            line.ends_with(&format!("\tretryable=0\treason={escaped}\n")),
            "{line}"
        );
        assert_eq!(decode_line(&line).unwrap(), message);
    }
}

#[test]
fn a_refusal_without_a_reason_omits_the_field() {
    let message = rejected(None);
    let line = encode_line(&message).unwrap();
    assert!(!line.contains("reason="), "{line}");
    assert!(line.ends_with("\tretryable=0\n"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn malformed_reasons_are_refused_both_ways() {
    for bad in [
        "",
        "who_knows",
        "permission_denied:1",
        "revision_conflict",
        "revision_conflict:01",
        "revision_conflict:-1",
        "unknown_id:widget:1",
        "unknown_id:scene",
        "unknown_id:scene:1:2",
        "input_in_use:2",
        "input_in_use:2:onstage",
        "input_in_use:2:overlay:0",
        "input_in_use:2:overlay:256",
        "input_in_use:2:program:1",
        "input_in_use:x:program",
        "display_in_use:0",
        "bounds_exceeded:Inputs:3",
        "bounds_exceeded::3",
        "bounds_exceeded:inputs",
        "stinger_realizing:0",
        "invalid_name:4294967296",
        "script_failed:syntax:0:1",
        "script_failed:syntax:1:0",
        "script_failed:typo:1:1",
        "script_failed:refused:1",
        "script_failed:refused:1:01",
        "script_failed:refused:1:1:1",
    ] {
        assert!(decode_refusal_reason(bad).is_err(), "{bad:?}");
    }
    let line = encode_line(&rejected(Some(RefusalReason::ProjectLocked))).unwrap();
    let broken = line.replace("reason=project_locked", "reason=project_unlocked");
    assert!(decode_line(&broken).is_err());
    for reason in [
        RefusalReason::DisplayInUse { slot: 0 },
        RefusalReason::StingerRealizing { slot: 0 },
        RefusalReason::InputInUse {
            input: 1,
            usage: InputUse::Stinger { slot: 0 },
        },
        RefusalReason::BoundsExceeded {
            field: "Bad Field".to_owned(),
            max: 1,
        },
        RefusalReason::BoundsExceeded {
            field: "x".repeat(fm_protocol::MAX_REFUSAL_FIELD_BYTES + 1),
            max: 1,
        },
    ] {
        assert!(encode_refusal_reason(&reason).is_err(), "{reason:?}");
        assert!(encode_line(&rejected(Some(reason))).is_err());
    }
}
