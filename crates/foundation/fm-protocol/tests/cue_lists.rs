//! Protocol 2.51 operator cue lists (RC-017; media actions, millisecond
//! delays, and the run's media transports since 2.51; the run's script steps
//! since 2.61): the list edit, GO, standby,
//! and clock commands, the `cue_list_changed`, `cue_list_removed`, and
//! `cue_run_changed` events, the snapshot's `cue_list_states` count, and the
//! `cue_list_state` follow-on record — exactly, and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload,
    CueListSpec, CueListStateMessage, CueListStatus, CueMediaTransport, CueOperation,
    CuePendingStatus, CueRunChange, CueRunStatus, CueScriptRun, CueTitleText, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    InputAudioStripStatus, MAX_CUE_ACTIONS, MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUES,
    ManualTransitionStatus, MediaTime, MediaTransportState, OverlayStatus, PlaylistRunState,
    ProtocolVersion, RecordingStatus, StreamRealizedState, StreamStatus, TransitionFavoriteEffect,
    WireCue, WireCueAction, WireCueMediaOperation, WireInputId, WireMessage, WireOverlayChannelId,
    WireRecorderSlot, WireStingerSlotId, WireStreamTargetId, WireTitleFieldId, decode_line,
    encode_line,
};

const PREFIX: &str = "command\tprotocol=2.61\tid=a\tidempotency_key=k";

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
        WireCueAction::Preview { input: input(3) },
        WireCueAction::Take {
            input: Some(input(3)),
            effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            duration_frames: 25,
        },
        WireCueAction::OverlayOn {
            channel: channel(2),
            input: input(4),
        },
        WireCueAction::OverlayOff {
            channel: channel(2),
        },
        WireCueAction::TitleOn {
            input: input(5),
            channel: channel(8),
        },
        WireCueAction::TitleText {
            input: input(5),
            field: WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
            text: "Señor, 1/2: ok~".to_owned(),
        },
        WireCueAction::TitleOff { input: input(5) },
        WireCueAction::FadeToBlack {
            active: true,
            duration_frames: 30,
        },
        WireCueAction::PlaylistStart,
        WireCueAction::PlaylistStop,
        WireCueAction::AudioGain {
            input: input(3),
            gain_millidb: -12_500,
        },
        WireCueAction::AudioMute {
            input: input(4),
            muted: true,
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
                        input: input(6),
                        operation: WireCueMediaOperation::Restart,
                    },
                    WireCueAction::Delay { millis: 1_500 },
                    WireCueAction::Media {
                        input: input(6),
                        operation: WireCueMediaOperation::Seek { frame: 0 },
                    },
                    WireCueAction::Media {
                        input: input(6),
                        operation: WireCueMediaOperation::Play,
                    },
                    WireCueAction::Media {
                        input: input(6),
                        operation: WireCueMediaOperation::Pause,
                    },
                    WireCueAction::RunScript { script: 4 },
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
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 61));
}

#[test]
fn cue_commands_round_trip_exactly() {
    let line = round_trip(&command(CommandPayload::SetCueList {
        list: 3,
        spec: Box::new(spec()),
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
    assert!(
        line.contains(
            "media_restart.6%2Fdelay.1500%2Fmedia_seek.6.0%2Fmedia_play.6%2Fmedia_pause.6\
             %2Frun_script.4"
        ),
        "{line}"
    );

    for (operation, wire) in [
        (CueOperation::Go { cue: None }, "payload=cue_go\tlist=3\n"),
        (
            CueOperation::Go { cue: Some(7) },
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
        "payload=cue_advance\tlist=3",
    ] {
        let line = format!("{PREFIX}\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }

    let encode = |spec: CueListSpec| {
        encode_line(&command(CommandPayload::SetCueList {
            list: 1,
            spec: Box::new(spec),
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
            fault: Some((1, 2)),
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
        titles: vec![CueTitleText {
            input: input(5),
            field_revision: 4,
            field: WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
            text: "Señor".to_owned(),
        }],
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
            "cue_list_state\trevision=1\tframe=0\tlist=1\tname=A\tcues=\tstate=-%3A-%3A-%3A%3A-\n"
        )
        .is_ok()
    );
    for bad in [
        "state=-%3A-%3A-%3A2.1%3A-",
        "state=-%3A-%3A-%3A%3A",
        "state=-%3A-%3A1.1%3A%3A-",
        "state=x%3A-%3A-%3A%3A-",
    ] {
        let line = format!("cue_list_state\trevision=1\tframe=0\tlist=1\tname=A\tcues=\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }
}
