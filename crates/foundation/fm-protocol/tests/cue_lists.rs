//! Protocol 2.51 operator cue lists (RC-017; media actions, millisecond
//! delays, and the run's media transports since 2.51; the run's script steps
//! since 2.61): the list edit, GO, standby,
//! and clock commands, the `cue_list_changed`, `cue_list_removed`, and
//! `cue_run_changed` events, the snapshot's `cue_list_states` count, and the
//! `cue_list_state` follow-on record — exactly, and strictly. Since 2.63
//! a fault carries what its action was refused with.

use core::num::NonZeroU128;

use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload,
    CueArmedStatus, CueFaultStatus, CueListSpec, CueListStateMessage, CueListStatus,
    CueMediaTransport, CueOperation, CuePendingStatus, CueRunChange, CueRunStatus, CueScriptRun,
    CueTitleText, CueTitleValue, EditHistoryStatus, EngineIdentity, EventCursor, EventMessage,
    EventPayload, FadeToBlackPosition, FadeToBlackState, InputAudioStripStatus, MAX_CUE_ACTIONS,
    MAX_CUE_ARM_MS, MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUE_TITLE_NUMBER, MAX_CUES,
    ManualTransitionStatus, MediaTime, MediaTransportState, OverlayStatus, PlaylistRunState,
    ProtocolVersion, RecordingStatus, StreamRealizedState, StreamStatus, TransitionFavoriteEffect,
    WireCue, WireCueAction, WireCueMediaOperation, WireInputId, WireMessage, WireOverlayChannelId,
    WireRecorderSlot, WireStingerSlotId, WireStreamTargetId, WireTitleFieldId, decode_line,
    encode_line,
};

const PREFIX: &str = "command\tprotocol=2.110\tid=a\tidempotency_key=k";

/// Lowercase hex of `text`'s UTF-8 bytes.
fn hex(text: &str) -> String {
    use core::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").unwrap();
        out
    })
}

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn channel(value: u8) -> WireOverlayChannelId {
    WireOverlayChannelId::new(value).unwrap()
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

fn every_action() -> Vec<WireCueAction> {
    vec![
        WireCueAction::Preview {
            input: fm_types::InputTarget::Fixed(input(3)),
        },
        WireCueAction::Take {
            input: Some(fm_types::InputTarget::Fixed(input(3))),
            effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            duration_frames: 25,
        },
        WireCueAction::OverlayOn {
            channel: channel(2),
            input: fm_types::InputTarget::Fixed(input(4)),
        },
        WireCueAction::OverlayOff {
            channel: channel(2),
        },
        WireCueAction::TitleOn {
            input: fm_types::InputTarget::Fixed(input(5)),
            channel: channel(8),
        },
        WireCueAction::TitleText {
            input: fm_types::InputTarget::Fixed(input(5)),
            field: WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
            text: "Señor, 1/2: ok~".to_owned(),
        },
        WireCueAction::TitleOff {
            input: fm_types::InputTarget::Fixed(input(5)),
        },
        WireCueAction::FadeToBlack {
            active: true,
            duration_frames: 30,
        },
        WireCueAction::PlaylistStart,
        WireCueAction::PlaylistStop,
        WireCueAction::AudioGain {
            input: fm_types::InputTarget::Fixed(input(3)),
            gain: fm_protocol::CueGain::Set(-12_500),
        },
        WireCueAction::AudioMute {
            input: fm_types::InputTarget::Fixed(input(4)),
            mute: fm_protocol::CueMute::Set(true),
        },
        WireCueAction::Record {
            recorder: WireRecorderSlot::new(2).unwrap(),
            start: true,
        },
        WireCueAction::Stream {
            target: WireStreamTargetId::new(NonZeroU128::new(9).unwrap()),
            start: false,
        },
        WireCueAction::Wait { frames: 50 },
        WireCueAction::Take {
            input: None,
            effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(1).unwrap()),
            duration_frames: 40,
        },
    ]
}

fn spec() -> CueListSpec {
    CueListSpec {
        name: "Main show".to_owned(),
        cues: vec![
            WireCue {
                id: 1,
                name: "Open, \"cold\"".to_owned(),
                auto_follow: true,
                actions: every_action(),
            },
            WireCue {
                id: 7,
                name: "Empty".to_owned(),
                auto_follow: false,
                actions: Vec::new(),
            },
            WireCue {
                id: 9,
                name: "Roll clip".to_owned(),
                auto_follow: false,
                actions: vec![
                    WireCueAction::Media {
                        input: fm_types::InputTarget::Fixed(input(6)),
                        operation: WireCueMediaOperation::Restart,
                    },
                    WireCueAction::Delay { millis: 1_500 },
                    WireCueAction::Media {
                        input: fm_types::InputTarget::Fixed(input(6)),
                        operation: WireCueMediaOperation::Seek { frame: 0 },
                    },
                    WireCueAction::Media {
                        input: fm_types::InputTarget::Fixed(input(6)),
                        operation: WireCueMediaOperation::Play,
                    },
                    WireCueAction::Media {
                        input: fm_types::InputTarget::Fixed(input(6)),
                        operation: WireCueMediaOperation::Pause,
                    },
                    WireCueAction::RunScript { script: 4 },
                    WireCueAction::TitleNumber {
                        input: fm_types::InputTarget::Fixed(input(5)),
                        field: WireTitleFieldId::new(NonZeroU128::new(3).unwrap()),
                        add: true,
                        value: -2,
                    },
                    WireCueAction::TitleNumber {
                        input: fm_types::InputTarget::Fixed(input(5)),
                        field: WireTitleFieldId::new(NonZeroU128::new(3).unwrap()),
                        add: false,
                        value: MAX_CUE_TITLE_NUMBER,
                    },
                ],
            },
        ],
    }
}

fn state() -> CueRunStatus {
    CueRunStatus {
        standby: Some(7),
        live: Some(1),
        pending: Some(CuePendingStatus {
            cue: 1,
            action: 15,
            due_frame: 1_234,
        }),
        played: vec![1],
        fault: None,
        armed: None,
        targets: None,
    }
}

fn cursor() -> EventCursor {
    EventCursor {
        engine: EngineIdentity {
            engine_id: "engine-a".to_owned(),
            state_epoch: 7,
            log_id: "log-a".to_owned(),
        },
        revision: 12,
    }
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: cursor(),
        payload,
        edit_history: EditHistoryStatus::EMPTY,
    })
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 110));
}

#[test]
fn cue_commands_round_trip_exactly() {
    let line = round_trip(&command(CommandPayload::SetCueList {
        list: 3,
        spec: Box::new(spec()),
        position: None,
    }));
    assert!(
        line.starts_with(&format!(
            "{PREFIX}\tpayload=cue_list_set\tlist=3\tname=Main%20show\tcues=1%7E"
        )),
        "{line}"
    );
    assert!(line.contains("take.3.fade.25%2Foverlay_on.2.4"), "{line}");
    assert!(line.contains("wait.50%2Ftake.-.stinger_1.40"), "{line}");
    assert!(line.contains("audio_gain.3.-12500"), "{line}");
    // Protocol 2.65: the data update action, `set` or `add` and a signed value.
    assert!(
        line.contains("title_number.5.3.add.-2%2Ftitle_number.5.3.set.999999999999"),
        "{line}"
    );
    assert!(
        line.contains(
            "media_restart.6%2Fdelay.1500%2Fmedia_seek.6.0%2Fmedia_play.6%2Fmedia_pause.6\
             %2Frun_script.4"
        ),
        "{line}"
    );

    for (operation, wire) in [
        (
            CueOperation::Go {
                cue: None,
                targets: fm_protocol::WireTargetBindings::default(),
            },
            "payload=cue_go\tlist=3\n",
        ),
        (
            CueOperation::Go {
                cue: Some(7),
                targets: fm_protocol::WireTargetBindings::default(),
            },
            "payload=cue_go\tlist=3\tcue=7\n",
        ),
        (
            CueOperation::Standby { cue: 1 },
            "payload=cue_standby\tlist=3\tcue=1\n",
        ),
        (CueOperation::Back, "payload=cue_back\tlist=3\n"),
        (CueOperation::Skip, "payload=cue_skip\tlist=3\n"),
        (CueOperation::Stop, "payload=cue_stop\tlist=3\n"),
        (CueOperation::Reset, "payload=cue_reset\tlist=3\n"),
        // Protocol 2.64: arm a GO of the standby or a named cue, disarm it.
        (
            CueOperation::Arm {
                cue: None,
                in_ms: 1,
                targets: fm_protocol::WireTargetBindings::default(),
            },
            "payload=cue_arm\tlist=3\tin_ms=1\n",
        ),
        (
            CueOperation::Arm {
                cue: Some(7),
                in_ms: MAX_CUE_ARM_MS,
                targets: fm_protocol::WireTargetBindings::default(),
            },
            "payload=cue_arm\tlist=3\tcue=7\tin_ms=86400000\n",
        ),
        (CueOperation::Disarm, "payload=cue_disarm\tlist=3\n"),
    ] {
        let line = round_trip(&command(CommandPayload::CueControl { list: 3, operation }));
        assert_eq!(line, format!("{PREFIX}\t{wire}"));
    }
    assert_eq!(
        round_trip(&command(CommandPayload::RemoveCueList { list: 3 })),
        format!("{PREFIX}\tpayload=cue_list_remove\tlist=3\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::CueAdvance {
            list: 3,
            frame: 900
        })),
        format!("{PREFIX}\tpayload=cue_advance\tlist=3\tframe=900\n")
    );
}

#[test]
fn cue_commands_are_strict() {
    for bad in [
        "payload=cue_go\tlist=0",
        "payload=cue_go\tlist=03",
        "payload=cue_go",
        "payload=cue_standby\tlist=3",
        "payload=cue_back\tlist=3\tcue=1",
        "payload=cue_list_remove\tlist=x",
        "payload=cue_list_set\tlist=3\tname=%20\tcues=",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Ewait.0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Esleep.5",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E2%7E",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E4%7E0%7E",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7E%2C1%7E42%7E0%7E",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etake.3.cut.5",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Eaudio_gain.3.-0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Eoverlay_off.9",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Edelay.0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Edelay.3600001",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Edelay.01",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Erun_script.0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Erun_script.04",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Erun_script",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Erun_script.x",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Emedia_play.0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Emedia_play.6.1",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Emedia_seek.6",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Emedia_seek.6.-1",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Emedia_stop.6",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.add.0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.set.-0",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.set.%2B1",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.add.01",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.set.1000000000000",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.inc.1",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.0.set.1",
        "payload=cue_list_set\tlist=3\tname=A\tcues=1%7E41%7E0%7Etitle_number.5.3.set",
        "payload=cue_advance\tlist=3",
        "payload=cue_arm\tlist=3",
        "payload=cue_arm\tlist=3\tin_ms=0",
        "payload=cue_arm\tlist=3\tin_ms=86400001",
        "payload=cue_arm\tlist=3\tin_ms=01",
        "payload=cue_arm\tlist=3\tcue=x\tin_ms=5",
        "payload=cue_arm\tlist=0\tin_ms=5",
        "payload=cue_disarm\tlist=3\tcue=1",
    ] {
        let line = format!("{PREFIX}\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }

    let encode = |spec: CueListSpec| {
        encode_line(&command(CommandPayload::SetCueList {
            list: 1,
            spec: Box::new(spec),
            position: None,
        }))
    };
    let mut many = spec();
    many.cues = (0..=MAX_CUES as u64)
        .map(|id| WireCue {
            id,
            name: "C".to_owned(),
            auto_follow: false,
            actions: Vec::new(),
        })
        .collect();
    assert_eq!(encode(many), Err(CodecError::TooManyItems("cues")));
    let mut long = spec();
    long.cues[1].actions = vec![WireCueAction::Wait { frames: 1 }; MAX_CUE_ACTIONS + 1];
    assert_eq!(encode(long), Err(CodecError::TooManyItems("cues")));
    for in_ms in [0, MAX_CUE_ARM_MS + 1] {
        assert!(
            encode_line(&command(CommandPayload::CueControl {
                list: 3,
                operation: CueOperation::Arm {
                    cue: None,
                    in_ms,
                    targets: fm_protocol::WireTargetBindings::default()
                },
            }))
            .is_err(),
            "{in_ms}"
        );
    }
}

/// Protocol 2.64: a list's run state carries its armed GO as
/// `<cue>.<due frame>` after the fault, strictly.
#[test]
fn an_armed_go_round_trips_in_the_run_state() {
    let armed = CueRunStatus {
        armed: Some(CueArmedStatus {
            cue: 7,
            due_frame: 90_000,
            targets: None,
        }),
        ..state()
    };
    let message = WireMessage::CueListState(CueListStateMessage {
        revision: 12,
        frame: 1_000,
        list: CueListStatus {
            id: 3,
            spec: spec(),
            state: armed,
        },
    });
    let line = round_trip(&message);
    assert!(
        line.contains("\tstate=7%3A1%3A1.15.1234%3A1%3A-%3A7.90000%3A-\n"),
        "{line}"
    );
    for bad in ["7.", "7", "x.5", "7.05", "7.5.1"] {
        let broken = line.replace("%3A7.90000%3A-\n", &format!("%3A{bad}%3A-\n"));
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    let five = line.replace("%3A-%3A7.90000%3A-\n", "%3A-%3A-\n");
    assert!(
        decode_line(&five).is_err(),
        "a state without its armed part"
    );
}

/// Protocol 2.89 (RC-004): a run state reports the inputs its run's
/// dynamic targets are bound to as a seventh part,
/// `<preview>.<program>.<selected>` (each an input id or `-`), and an armed
/// GO that bound targets names them after its due frame; strictly.
#[test]
fn a_run_reports_its_bound_targets() {
    use fm_protocol::WireTargetBindings;
    let bound = CueRunStatus {
        armed: Some(CueArmedStatus {
            cue: 7,
            due_frame: 90_000,
            targets: Some(WireTargetBindings {
                preview: Some(input(4)),
                program: Some(input(3)),
                selected: None,
            }),
        }),
        targets: Some(WireTargetBindings {
            preview: Some(input(3)),
            program: Some(input(5)),
            selected: Some(input(6)),
        }),
        ..state()
    };
    let message = WireMessage::CueListState(CueListStateMessage {
        revision: 12,
        frame: 1_000,
        list: CueListStatus {
            id: 3,
            spec: spec(),
            state: bound,
        },
    });
    let line = round_trip(&message);
    assert!(
        line.contains("\tstate=7%3A1%3A1.15.1234%3A1%3A-%3A7.90000.4.3.-%3A3.5.6\n"),
        "{line}"
    );
    for (good, bad) in [
        ("%3A3.5.6\n", "%3A3.5\n"),
        ("%3A3.5.6\n", "%3A3.5.6.7\n"),
        ("%3A3.5.6\n", "%3A3.05.6\n"),
        ("%3A3.5.6\n", "%3A0.5.6\n"),
        ("%3A3.5.6\n", "%3Apreview.5.6\n"),
        ("%3A3.5.6\n", "%3A\n"),
        ("%3A3.5.6\n", "\n"),
        ("7.90000.4.3.-", "7.90000.4.3"),
        ("7.90000.4.3.-", "7.90000.4"),
        ("7.90000.4.3.-", "7.90000.4.3.-.-"),
        ("7.90000.4.3.-", "7.90000.x.3.-"),
    ] {
        let broken = line.replace(good, bad);
        assert_ne!(broken, line, "{bad}");
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    // Every role unbound still names the run's targets.
    let open = line.replace("%3A3.5.6\n", "%3A-.-.-\n");
    let WireMessage::CueListState(decoded) = decode_line(&open).unwrap() else {
        panic!("expected a cue list state");
    };
    assert_eq!(decoded.list.state.targets, Some(WireTargetBindings::NONE));
}

/// A bare run event of list 3 on frame 1000 carrying `scripts`.
fn script_run_event(scripts: Vec<CueScriptRun>) -> WireMessage {
    event(EventPayload::CueRunChanged(Box::new(CueRunChange {
        list: 3,
        frame: 1_000,
        state: CueRunStatus::default(),
        fired: vec![1],
        program: input(3),
        preview: input(4),
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        overlays: OverlayStatus::empty_channels(),
        input_audio_strips: vec![InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(3),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        }],
        streams: Vec::new(),
        recordings: [1, 2]
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: false,
            })
            .to_vec(),
        playlist: PlaylistRunState::Stopped,
        titles: Vec::new(),
        media: Vec::new(),
        scripts,
    })))
}

/// Protocol 2.61: `cue_run_changed` ends with `scripts`, every script step
/// the run ran in order as `<script>.<actions>.<instructions>` joined by
/// `/`, at most one per script step a list may hold; a malformed, zero, or
/// oversized entry, or one too many, is refused both ways.
#[test]
fn cue_run_scripts_are_exact_and_bounded() {
    let run = |script, actions, instructions| CueScriptRun {
        script,
        actions,
        instructions,
    };
    let scripts = vec![run(2, 3, 17), run(5, 0, 1), run(2, 3, 17)];
    let line = round_trip(&script_run_event(scripts));
    assert!(
        line.contains("\tmedia=\tscripts=2.3.17%2F5.0.1%2F2.3.17\tundo_depth="),
        "{line}"
    );
    let largest = run(u64::MAX, u32::MAX, u32::MAX);
    let line = round_trip(&script_run_event(vec![largest; MAX_CUE_LIST_SCRIPT_STEPS]));
    let full =
        ["18446744073709551615.4294967295.4294967295"; MAX_CUE_LIST_SCRIPT_STEPS].join("%2F");
    assert!(line.contains(&format!("\tscripts={full}\t")), "{line}");

    assert_eq!(
        encode_line(&script_run_event(vec![
            run(1, 1, 1);
            MAX_CUE_LIST_SCRIPT_STEPS + 1
        ])),
        Err(CodecError::TooManyItems("scripts"))
    );
    assert!(encode_line(&script_run_event(vec![run(0, 1, 1)])).is_err());

    let line = encode_line(&script_run_event(vec![run(2, 3, 17)])).unwrap();
    assert!(line.contains("\tscripts=2.3.17\t"), "{line}");
    let nine = ["1.1.1"; MAX_CUE_LIST_SCRIPT_STEPS + 1].join("%2F");
    assert_eq!(
        decode_line(&line.replace("\tscripts=2.3.17\t", &format!("\tscripts={nine}\t"))),
        Err(CodecError::TooManyItems("scripts"))
    );
    for bad in [
        "0.1.1",
        "02.3.17",
        "2.03.17",
        "2.3.017",
        "2.4294967296.1",
        "2.1.4294967296",
        "18446744073709551616.1.1",
        "2.3",
        "2.3.17.1",
        "2..17",
        "-2.3.17",
        "2.3.17%2F",
        "%2F2.3.17",
        "2.3.17%2F%2F5.0.1",
        "x.3.17",
    ] {
        let broken = line.replace("\tscripts=2.3.17\t", &format!("\tscripts={bad}\t"));
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    let missing = line.replace("\tscripts=2.3.17\t", "\t");
    assert!(decode_line(&missing).is_err(), "{missing}");
}

#[test]
#[allow(clippy::too_many_lines)] // every cue event and the snapshot record in one pass
fn cue_events_and_the_snapshot_record_round_trip_exactly() {
    let status = CueListStatus {
        id: 3,
        spec: spec(),
        state: state(),
    };
    let line = round_trip(&event(EventPayload::CueListChanged(Box::new(
        status.clone(),
    ))));
    assert!(line.contains("\tstate=7%3A1%3A1.15.1234%3A1%3A-"), "{line}");
    round_trip(&event(EventPayload::CueListRemoved { list: 3 }));
    let change = CueRunChange {
        list: 3,
        frame: 1_000,
        state: CueRunStatus {
            fault: Some(CueFaultStatus {
                cue: 1,
                action: 2,
                detail: fm_command::FaultDetail {
                    reason: Some(fm_command::RefusalReason::InputInUse {
                        input: 4,
                        usage: fm_command::InputUse::Program,
                    }),
                    message: "input 4 is on Program".to_owned(),
                },
            }),
            pending: None,
            ..state()
        },
        fired: vec![1, 7],
        program: input(3),
        preview: input(4),
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: FadeToBlackState {
            target_active: true,
            position: FadeToBlackPosition::LIVE,
        },
        overlays: OverlayStatus::empty_channels(),
        input_audio_strips: vec![InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(3),
            gain_millidb: -12_500,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        }],
        streams: vec![StreamStatus {
            target: WireStreamTargetId::new(NonZeroU128::new(9).unwrap()),
            name: "Web".to_owned(),
            desired_running: false,
            realized: StreamRealizedState::Stopped,
            detail: None,
        }],
        recordings: vec![
            RecordingStatus {
                recorder: WireRecorderSlot::new(1).unwrap(),
                desired_active: false,
            },
            RecordingStatus {
                recorder: WireRecorderSlot::new(2).unwrap(),
                desired_active: true,
            },
        ],
        playlist: PlaylistRunState::Stopped,
        titles: vec![
            CueTitleText {
                input: input(5),
                field_revision: 4,
                field: WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
                value: CueTitleValue::Text("Señor".to_owned()),
            },
            // Protocol 2.65: a number field's value.
            CueTitleText {
                input: input(5),
                field_revision: 4,
                field: WireTitleFieldId::new(NonZeroU128::new(3).unwrap()),
                value: CueTitleValue::Number(-12),
            },
        ],
        media: vec![CueMediaTransport {
            input: input(6),
            state: MediaTransportState::Playing,
            position: 0,
            position_time: MediaTime {
                numerator: 0,
                denominator: 1,
            },
        }],
        scripts: Vec::new(),
    };
    let line = round_trip(&event(EventPayload::CueRunChanged(Box::new(
        change.clone(),
    ))));
    assert!(line.contains("\tfired=1.7\t"), "{line}");
    assert!(
        line.contains(&format!("\ttitles=5.4.2.{}%2F5.4.3.n-12\t", hex("Señor"))),
        "{line}"
    );
    for bad in ["5.4.3.n", "5.4.3.n-0", "5.4.3.n01", "5.4.3.nx"] {
        let broken = line.replacen("5.4.3.n-12", bad, 1);
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    // Protocol 2.63: `<cue>.<action>.<reason hex>.<message hex>`.
    let fault = format!(
        "1.2.{}.{}",
        hex("input_in_use:4:program"),
        hex("input 4 is on Program")
    );
    // Protocol 2.64: the armed GO follows the fault (`-` for none); Protocol
    // 2.89: then the run's bound targets (`-` for none).
    assert!(
        line.contains(&format!("%3A1%3A{fault}%3A-%3A-\t")),
        "{line}"
    );
    let no_reason = line.replacen(
        &format!("1.2.{}.", hex("input_in_use:4:program")),
        "1.2.-.",
        1,
    );
    let EventPayload::CueRunChanged(decoded) = (match decode_line(&no_reason).unwrap() {
        WireMessage::Event(event) => event.payload,
        other => panic!("{other:?}"),
    }) else {
        panic!("not a cue run change");
    };
    assert_eq!(
        decoded.state.fault.unwrap().detail,
        fm_command::FaultDetail {
            reason: None,
            message: "input 4 is on Program".to_owned(),
        }
    );
    for bad in [
        "1.2".to_owned(),
        "1.2.-".to_owned(),
        "1.2.-.".to_owned(),
        format!("1.2.-.{}", hex(" padded")),
        format!("1.2.-.{}", hex("a\tb")),
        format!(
            "1.2.-.{}",
            hex(&"x".repeat(fm_command::MAX_FAULT_MESSAGE_BYTES + 1))
        ),
        format!("1.2.{}.78", hex("input_in_use:4")),
        "1.2.zz.78".to_owned(),
        "1.2.-.7".to_owned(),
        "1.2.-.78.0".to_owned(),
    ] {
        let broken = line.replacen(&fault, &bad, 1);
        assert_ne!(broken, line);
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    let mut invalid = change.clone();
    if let Some(fault) = &mut invalid.state.fault {
        fault.detail.message = "two\nlines".to_owned();
    }
    assert!(encode_line(&event(EventPayload::CueRunChanged(Box::new(invalid)))).is_err());
    assert!(line.contains("\ttitles=5.4.2.5365c3b16f72"), "{line}");
    assert!(line.contains("\tmedia=6.playing.0.0.1"), "{line}");
    for bad in [
        "media=6.playing.0.0.0",
        "media=6.rolling.0.0.1",
        "media=0.playing.0.0.1",
        "media=6.playing.0.0",
    ] {
        let broken = line.replace("\tmedia=6.playing.0.0.1", &format!("\t{bad}"));
        assert!(decode_line(&broken).is_err(), "{bad}");
    }
    let mut bare = change;
    bare.media.clear();
    let line = round_trip(&event(EventPayload::CueRunChanged(Box::new(bare))));
    assert!(line.contains("\tmedia=\t"), "{line}");
    assert!(line.contains("\tmedia=\tscripts=\tundo_depth="), "{line}");

    round_trip(&WireMessage::CueListState(CueListStateMessage {
        revision: 12,
        frame: 1_001,
        list: status,
    }));
    assert!(
        decode_line(
            "cue_list_state\trevision=1\tframe=0\tlist=1\tname=A\tcues=\tstate=-%3A-%3A-%3A%3A-%3A-%3A-\n"
        )
        .is_ok()
    );
    for bad in [
        "state=-%3A-%3A-%3A2.1%3A-%3A-%3A-",
        "state=-%3A-%3A-%3A%3A",
        "state=-%3A-%3A1.1%3A%3A-%3A-%3A-",
        "state=x%3A-%3A-%3A%3A-%3A-%3A-",
        // The six-part state of Protocol 2.88 lacks the bound targets.
        "state=-%3A-%3A-%3A%3A-%3A-",
    ] {
        let line = format!("cue_list_state\trevision=1\tframe=0\tlist=1\tname=A\tcues=\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }
}

/// `cue_list_set` carries an optional `position` (the index in the
/// project's cue lists, below `MAX_CUE_LISTS`): an undone removal is
/// journaled with its old place so a replay restores the order.
#[test]
fn cue_list_set_carries_an_optional_position_exactly() {
    let set = |position| {
        command(CommandPayload::SetCueList {
            list: 3,
            spec: Box::new(spec()),
            position,
        })
    };
    let without = round_trip(&set(None));
    assert!(!without.contains("position="), "{without}");
    let line = round_trip(&set(Some(1)));
    assert!(line.ends_with("\tposition=1\n"), "{line}");
    let last = u8::try_from(fm_protocol::MAX_CUE_LISTS - 1).unwrap();
    let line = round_trip(&set(Some(last)));
    for bad in [
        format!("position={}", fm_protocol::MAX_CUE_LISTS),
        "position=07".to_owned(),
        "position=".to_owned(),
    ] {
        let line = line.replacen(&format!("\tposition={last}\n"), &format!("\t{bad}\n"), 1);
        assert!(decode_line(&line).is_err(), "{line}");
    }
    assert!(encode_line(&set(Some(last + 1))).is_err());
    assert!(
        decode_line(&format!(
            "{PREFIX}\tpayload=cue_list_remove\tlist=3\tposition=1\n"
        ))
        .is_err()
    );
}

/// Protocol 2.104 (GX-006): a title action whose value is hidden from the
/// session travels as `title_text.<input>.<field>.hidden` or
/// `title_number.<input>.<field>.hidden` in cue lists and shortcuts, exactly;
/// nothing else stands for it.
#[test]
fn the_hidden_title_value_marker_round_trips_exactly() {
    let field = WireTitleFieldId::new(NonZeroU128::new(2).unwrap());
    let text = WireCueAction::TitleHidden {
        input: fm_protocol::WireInputTarget::Fixed(input(3)),
        field,
        number: false,
    };
    let number = WireCueAction::TitleHidden {
        input: fm_protocol::WireInputTarget::Preview,
        field,
        number: true,
    };
    assert_eq!(
        fm_protocol::cue_action_token(&text),
        "title_text.3.2.hidden"
    );
    assert_eq!(
        fm_protocol::cue_action_token(&number),
        "title_number.preview.2.hidden"
    );
    for action in [&text, &number] {
        let token = fm_protocol::cue_action_token(action);
        assert_eq!(
            fm_protocol::parse_cue_action_token(&token).as_ref(),
            Some(action)
        );
    }
    // The marker each title action stands for.
    let set = WireCueAction::TitleText {
        input: fm_protocol::WireInputTarget::Fixed(input(3)),
        field,
        text: "note".into(),
    };
    assert_eq!(set.hidden_marker(), Some(text.clone()));
    assert_eq!(text.hidden_marker(), None);
    let mut spec = spec();
    assert!(!spec.has_hidden_title_value());
    spec.cues[0].actions = vec![text, number];
    assert!(spec.has_hidden_title_value());
    let list = CueListStatus {
        id: 3,
        spec: spec.clone(),
        state: CueRunStatus::default(),
    };
    let line = round_trip(&event(EventPayload::CueListChanged(Box::new(list.clone()))));
    assert!(
        line.contains("title_text.3.2.hidden%2Ftitle_number.preview.2.hidden"),
        "{line}"
    );
    round_trip(&WireMessage::CueListState(CueListStateMessage {
        revision: 4,
        frame: 0,
        list,
    }));
    round_trip(&command(CommandPayload::SetCueList {
        list: 3,
        spec: Box::new(spec),
        position: None,
    }));
    // Strict: only the exact marker, without a set/add mode or a value.
    for bad in [
        "title_text.3.2.HIDDEN",
        "title_text.3.2.hidde",
        "title_number.3.2.set.hidden",
        "title_number.3.2.hidden.1",
        "title_text.3.2.hidden.1",
        "title_text.3.hidden",
        "title_text.3.0.hidden",
    ] {
        assert_eq!(fm_protocol::parse_cue_action_token(bad), None, "{bad}");
    }
}
