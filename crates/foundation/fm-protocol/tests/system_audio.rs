//! Protocol 2.55 system audio inputs (AU-011): `input_add` with
//! `kind=system_audio`, the `input_added` durable event, the
//! `system_audio_status` runtime event, the snapshot's
//! `system_audio_inputs`, and the `system_audio:` replacement source —
//! exactly, and strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, InputLifecycleOperation, LiveInputSource,
    MAX_SYSTEM_AUDIO_ERROR_BYTES, MAX_SYSTEM_AUDIO_INPUTS, ProtocolVersion, RuntimeEventMessage,
    RuntimeLifecycleEvent, ServerIdentity, SystemAudioBackend, SystemAudioConfig, SystemAudioEntry,
    SystemAudioState, SystemAudioStatus, WireInputId, WireInputSource, WireMessage, decode_line,
    encode_line,
};

const PREFIX: &str = "command\tprotocol=2.60\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
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

fn runtime(event: RuntimeLifecycleEvent) -> WireMessage {
    WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "p".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 4,
        generation: 1,
        sequence: 9,
        event,
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

fn monitor() -> SystemAudioConfig {
    SystemAudioConfig::new(SystemAudioBackend::Pulse, "@DEFAULT_MONITOR@")
}

#[test]
fn the_current_protocol_is_2_55() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 60));
}

#[test]
fn the_add_command_has_an_exact_wire_form_and_is_strict() {
    let add = command(CommandPayload::AddInput {
        input: input(5),
        name: "Desktop".into(),
        source: LiveInputSource::SystemAudio(Box::new(monitor())),
    });
    let line = round_trip(&add);
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=input_add\tinput=5\tname=Desktop\tkind=system_audio\tbackend=pulse\tdevice=%40DEFAULT_MONITOR%40\n"
        )
    );
    let alsa = command(CommandPayload::AddInput {
        input: input(6),
        name: "Loopback".into(),
        source: LiveInputSource::SystemAudio(Box::new(SystemAudioConfig::new(
            SystemAudioBackend::Alsa,
            "hw:Loopback,1",
        ))),
    });
    assert!(round_trip(&alsa).contains("\tbackend=alsa\tdevice=hw%3ALoopback%2C1\n"));
    // A backend and a device, each valid; nothing else.
    refused(&line.replacen("\tbackend=pulse", "", 1));
    refused(&line.replacen("\tbackend=pulse", "\tbackend=jack", 1));
    refused(&line.replacen("\tdevice=%40DEFAULT_MONITOR%40", "", 1));
    refused(&line.replacen("%40DEFAULT_MONITOR%40", "", 1));
    refused(&line.replacen("%40DEFAULT_MONITOR%40", "-f", 1));
    refused(&line.replacen("%40DEFAULT_MONITOR%40", "a%09b", 1));
    refused(&line.replacen('\n', "\textra=1\n", 1));
    // An unusable device is not encoded either.
    let bad = command(CommandPayload::AddInput {
        input: input(5),
        name: "Desktop".into(),
        source: LiveInputSource::SystemAudio(Box::new(SystemAudioConfig::new(
            SystemAudioBackend::Pulse,
            "x".repeat(129),
        ))),
    });
    assert!(encode_line(&bad).is_err());
}

#[test]
fn the_added_event_carries_one_kind() {
    let added = event(EventPayload::InputAdded {
        input: input(5),
        name: "Desktop".into(),
        network_stream: None,
        capture: None,
        system_audio: Some(Box::new(monitor())),
    });
    let line = round_trip(&added);
    assert!(
        line.contains("\tkind=system_audio\tbackend=pulse\tdevice=%40DEFAULT_MONITOR%40"),
        "{line}"
    );
    refused(&line.replacen("\tdevice=%40DEFAULT_MONITOR%40", "", 1));
    let both = event(EventPayload::InputAdded {
        input: input(5),
        name: "Desktop".into(),
        network_stream: None,
        capture: Some(Box::new(fm_protocol::CaptureInputConfig::new("cam"))),
        system_audio: Some(Box::new(monitor())),
    });
    assert!(encode_line(&both).is_err());
}

#[test]
fn status_events_are_exact_and_bounded() {
    let reopening = runtime(RuntimeLifecycleEvent::SystemAudioStatus {
        input: input(5),
        status: Box::new(SystemAudioStatus {
            state: SystemAudioState::Reopening {
                attempt: 3,
                next_in_ms: 2_000,
            },
            reopens: 3,
            last_error: Some("Connection refused".into()),
        }),
    });
    let line = round_trip(&reopening);
    assert!(
        line.ends_with(
            "\tevent=system_audio_status\tinput=5\tstate=reopening\tattempt=3\tnext_in_ms=2000\treopens=3\tlast_error=Connection%20refused\n"
        ),
        "{line}"
    );
    for state in [
        SystemAudioState::Idle,
        SystemAudioState::Opening,
        SystemAudioState::Live,
        SystemAudioState::Failed,
    ] {
        let line = round_trip(&runtime(RuntimeLifecycleEvent::SystemAudioStatus {
            input: input(5),
            status: Box::new(SystemAudioStatus {
                state,
                reopens: 1,
                last_error: None,
            }),
        }));
        assert!(
            line.ends_with(&format!("\tinput=5\tstate={}\treopens=1\n", state.token())),
            "{line}"
        );
        // Only a reopening input carries its attempt.
        refused(&line.replacen("\treopens=1", "\tattempt=1\tnext_in_ms=0\treopens=1", 1));
    }
    refused(&line.replacen("\tattempt=3", "\tattempt=0", 1));
    refused(&line.replacen("\tattempt=3", "", 1));
    refused(&line.replacen("state=reopening", "state=stalled", 1));
    refused(&line.replacen("reopens=3", "reopens=03", 1));
    // Error text is one bounded line.
    let long = runtime(RuntimeLifecycleEvent::SystemAudioStatus {
        input: input(5),
        status: Box::new(SystemAudioStatus {
            last_error: Some("e".repeat(MAX_SYSTEM_AUDIO_ERROR_BYTES + 1)),
            ..SystemAudioStatus::default()
        }),
    });
    assert!(encode_line(&long).is_err());
    let blank = runtime(RuntimeLifecycleEvent::SystemAudioStatus {
        input: input(5),
        status: Box::new(SystemAudioStatus {
            last_error: Some(String::new()),
            ..SystemAudioStatus::default()
        }),
    });
    assert!(encode_line(&blank).is_err());
}

#[test]
fn the_replacement_source_round_trips() {
    let replace = command(CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::SystemAudio(SystemAudioConfig::new(
                SystemAudioBackend::Alsa,
                "hw:Loopback,1",
            )),
        },
    ));
    let line = round_trip(&replace);
    assert!(line.contains("system_audio"), "{line}");
    refused(&line.replacen("alsa", "jack", 1));
}

fn entry(value: u128, device: &str, status: SystemAudioStatus) -> SystemAudioEntry {
    SystemAudioEntry {
        input: input(value),
        config: SystemAudioConfig::new(SystemAudioBackend::Alsa, device),
        status,
    }
}

#[test]
fn snapshot_entries_round_trip_and_are_bounded() {
    let entries = vec![
        entry(
            5,
            "hw:Loopback,1~x",
            SystemAudioStatus {
                state: SystemAudioState::Reopening {
                    attempt: 2,
                    next_in_ms: 1_000,
                },
                reopens: 2,
                last_error: None,
            },
        ),
        SystemAudioEntry {
            input: input(7),
            config: monitor(),
            status: SystemAudioStatus::default(),
        },
    ];
    let line = encode_line(&snapshot(entries.clone())).unwrap();
    let WireMessage::Snapshot(decoded) = decode_line(&line).unwrap() else {
        panic!("{line}");
    };
    assert_eq!(decoded.system_audio_inputs, entries);
    assert!(
        line.contains(
            "\tsystem_audio_inputs=5%7Ealsa%7Ereopening%7E2%7E1000%7E2%7E15%3Ahw%3ALoopback%2C1%7Ex%2C7%7Epulse%7Eidle%7E0%7E0%7E0%7E17%3A%40DEFAULT_MONITOR%40\t"
        ),
        "{line}"
    );
    // Ascending, without error text, and bounded.
    let mut descending = entries.clone();
    descending.reverse();
    assert!(encode_line(&snapshot(descending)).is_err());
    let mut with_error = entries.clone();
    with_error[0].status.last_error = Some("boom".into());
    assert!(encode_line(&snapshot(with_error)).is_err());
    let too_many = (1..=u128::try_from(MAX_SYSTEM_AUDIO_INPUTS).unwrap() + 1)
        .map(|value| entry(value, "hw:0", SystemAudioStatus::default()))
        .collect();
    assert!(encode_line(&snapshot(too_many)).is_err());
    // A wrong device length is refused.
    refused(&line.replacen("%7E15%3A", "%7E14%3A", 1));
    refused(&line.replacen("%7Eidle%7E0%7E0", "%7Eidle%7E1%7E0", 1));
}

fn snapshot(system_audio_inputs: Vec<SystemAudioEntry>) -> WireMessage {
    use fm_protocol::{
        EditLockStatus, FadeToBlackPosition, FadeToBlackState, InputStatus, ManualTransitionStatus,
        OverlayStatus, RecordingStatus, SnapshotMessage, WireRecorderSlot,
    };
    WireMessage::Snapshot(SnapshotMessage {
        engine: EngineIdentity {
            engine_id: "e".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 12,
        show_name: "Show".to_owned(),
        inputs: [1, 2, 5, 7]
            .map(|value| InputStatus {
                input: input(value),
                name: format!("Input {value}"),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            })
            .into(),
        outputs: Vec::new(),
        input_audio_strips: [1, 2, 5, 7]
            .map(|value| fm_protocol::InputAudioStripStatus {
                input: input(value),
                gain_millidb: 0,
                balance_basis_points: 0,
                pan_basis_points: 0,
                group: None,
                muted: false,
                soloed: false,
                follow_video: true,
                delay_samples: 0,
            })
            .into(),
        desired_program: input(1),
        desired_preview: input(2),
        realized_program: input(1),
        realized_preview: input(2),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        realized_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: [1, 2]
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: false,
            })
            .into(),
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks: EditLockStatus::default(),
        edit_history: EditHistoryStatus::EMPTY,
        titles: Vec::new(),
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        cue_list_states: 0,
        script_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs,
        capture_inputs: Vec::new(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
    })
}
