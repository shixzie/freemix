//! Protocol 2.46 network stream inputs (IN-015): `input_add` with
//! `kind=network_stream`, the `input_stream_set` command, the
//! `network_stream_changed` durable event, the `network_stream_status`
//! runtime event, and the snapshot's `network_streams` — exactly, and
//! strictly.

use std::num::NonZeroU32;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, GiveUp, LiveInputSource,
    NetworkStreamConfig, NetworkStreamEntry, NetworkStreamSettings, NetworkStreamState,
    NetworkStreamStatus, ProtocolVersion, ReconnectPolicy, RtspTransport, RuntimeEventMessage,
    RuntimeLifecycleEvent, ServerIdentity, StallSlate, StreamUrl, WireInputId, WireMessage,
    decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.47\tid=a\tidempotency_key=k";

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

fn settings() -> NetworkStreamSettings {
    NetworkStreamSettings {
        transport: RtspTransport::Tcp,
        jitter_buffer_ms: 500,
        reconnect: ReconnectPolicy {
            backoff_min_ms: 250,
            backoff_max_ms: 8_000,
            give_up: GiveUp::After(NonZeroU32::new(5).unwrap()),
        },
        timeout_ms: 3_000,
        stall: StallSlate::Black,
    }
}

const SETTINGS: &str = "transport=tcp\tjitter_ms=500\tbackoff_min_ms=250\tbackoff_max_ms=8000\tgive_up=5\ttimeout_ms=3000\tstall=black";

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 47));
}

#[test]
fn commands_have_an_exact_wire_form() {
    let add = command(CommandPayload::AddInput {
        input: input(5),
        name: "Camera".into(),
        source: LiveInputSource::NetworkStream(Box::new(NetworkStreamConfig {
            url: "rtsp://admin:pw@cam.test:554/live".into(),
            settings: settings(),
        })),
    });
    assert_eq!(
        round_trip(&add),
        format!(
            "{PREFIX}\tpayload=input_add\tinput=5\tname=Camera\tkind=network_stream\turl=rtsp%3A%2F%2Fadmin%3Apw%40cam.test%3A554%2Flive\t{SETTINGS}\n"
        )
    );
    let set = command(CommandPayload::SetNetworkStream {
        input: input(5),
        url: Some(StreamUrl::new("udp://@:9000")),
        settings: NetworkStreamSettings::default(),
    });
    assert_eq!(
        round_trip(&set),
        format!(
            "{PREFIX}\tpayload=input_stream_set\tinput=5\turl=udp%3A%2F%2F%40%3A9000\ttransport=auto\tjitter_ms=300\tbackoff_min_ms=500\tbackoff_max_ms=10000\tgive_up=never\ttimeout_ms=5000\tstall=freeze\n"
        )
    );
    // Without a URL the edit keeps the input's URL.
    let keep = command(CommandPayload::SetNetworkStream {
        input: input(5),
        url: None,
        settings: settings(),
    });
    assert_eq!(
        round_trip(&keep),
        format!("{PREFIX}\tpayload=input_stream_set\tinput=5\t{SETTINGS}\n")
    );
}

#[test]
fn commands_are_strict() {
    let keep = format!("{PREFIX}\tpayload=input_stream_set\tinput=5\t{SETTINGS}\n");
    for (from, to) in [
        ("transport=tcp", "transport=quic"),
        ("jitter_ms=500", "jitter_ms=39"),
        ("jitter_ms=500", "jitter_ms=5001"),
        ("jitter_ms=500", "jitter_ms=0500"),
        ("backoff_min_ms=250", "backoff_min_ms=9000"),
        ("backoff_max_ms=8000", "backoff_max_ms=60001"),
        ("give_up=5", "give_up=0"),
        ("give_up=5", "give_up=always"),
        ("timeout_ms=3000", "timeout_ms=499"),
        ("stall=black", "stall=blue"),
        ("\tstall=black", ""),
        ("input=5", "input=0"),
    ] {
        let line = keep.replacen(from, to, 1);
        assert_ne!(line, keep);
        refused(&line);
    }
    for url in [
        "ftp%3A%2F%2Fhost%2Fx",
        "udp%3A%2F%2Fhost",
        "http%3A%2F%2Fhost%2Fa%20b",
        "",
    ] {
        refused(&keep.replacen("input=5\t", &format!("input=5\turl={url}\t"), 1));
    }
    let long = format!("http://host/{}", "a".repeat(520));
    assert!(matches!(
        encode_line(&command(CommandPayload::SetNetworkStream {
            input: input(5),
            url: Some(StreamUrl::new(long)),
            settings: settings(),
        })),
        Err(CodecError::InvalidField { field: "url", .. })
    ));
    // An unknown input kind stays refused.
    refused(&format!(
        "{PREFIX}\tpayload=input_add\tinput=5\tname=Camera\tkind=srt_stream\turl=x\t{SETTINGS}\n"
    ));
}

#[test]
fn events_carry_the_redacted_config() {
    let config = NetworkStreamConfig {
        url: "rtsp://***@cam.test:554/live?token=***".into(),
        settings: settings(),
    };
    let changed = event(EventPayload::NetworkStreamChanged {
        input: input(5),
        config: Box::new(config.clone()),
    });
    let line = round_trip(&changed);
    assert!(
        line.contains(&format!(
            "\tevent=network_stream_changed\tinput=5\turl=rtsp%3A%2F%2F%2A%2A%2A%40cam.test%3A554%2Flive%3Ftoken%3D%2A%2A%2A\t{SETTINGS}\t"
        )),
        "{line}"
    );
    let added = event(EventPayload::InputAdded {
        input: input(5),
        name: "Camera".into(),
        network_stream: Some(Box::new(config)),
        capture: None,
    });
    let line = round_trip(&added);
    assert!(line.contains("\tkind=network_stream\turl="), "{line}");
    let plain = event(EventPayload::InputAdded {
        input: input(6),
        name: "Slate".into(),
        network_stream: None,
        capture: None,
    });
    assert!(!round_trip(&plain).contains("kind="));
}

#[test]
fn status_events_are_exact_for_every_state() {
    let status = |state| NetworkStreamStatus {
        state,
        bitrate_bps: 4_500_000,
        buffer_ms: 280,
        dropped_frames: 3,
        late_frames: 7,
        reconnects: 2,
        last_error: Some("Connection refused ~ 127.0.0.1:9000".into()),
    };
    for state in [
        NetworkStreamState::Idle,
        NetworkStreamState::Connecting,
        NetworkStreamState::Buffering,
        NetworkStreamState::Live,
        NetworkStreamState::Stalled,
        NetworkStreamState::Reconnecting {
            attempt: 3,
            next_in_ms: 1_500,
        },
        NetworkStreamState::Failed,
    ] {
        let message = runtime(RuntimeLifecycleEvent::NetworkStreamStatus {
            input: input(5),
            status: Box::new(status(state)),
        });
        let line = round_trip(&message);
        assert!(
            line.contains(&format!(
                "\tevent=network_stream_status\tinput=5\tstate={}",
                state.token()
            )),
            "{line}"
        );
        assert_eq!(
            line.contains("\tattempt=3\tnext_in_ms=1500"),
            matches!(state, NetworkStreamState::Reconnecting { .. }),
            "{line}"
        );
    }
    let quiet = runtime(RuntimeLifecycleEvent::NetworkStreamStatus {
        input: input(5),
        status: Box::new(NetworkStreamStatus::default()),
    });
    let line = round_trip(&quiet);
    assert!(!line.contains("last_error"), "{line}");
    // Reconnect fields only with the reconnecting state.
    refused(&line.replacen("state=idle", "state=idle\tattempt=1\tnext_in_ms=5", 1));
    refused(&line.replacen("state=idle", "state=reconnecting", 1));
    refused(&line.replacen("state=idle", "state=lost", 1));
}

#[test]
fn snapshot_entries_round_trip_and_are_bounded() {
    let entries = vec![
        NetworkStreamEntry {
            input: input(5),
            config: NetworkStreamConfig {
                url: "rtsp://***@cam.test/live,a~b".into(),
                settings: settings(),
            },
            status: NetworkStreamStatus {
                state: NetworkStreamState::Reconnecting {
                    attempt: 1,
                    next_in_ms: 500,
                },
                reconnects: 1,
                ..NetworkStreamStatus::default()
            },
        },
        NetworkStreamEntry {
            input: input(7),
            config: NetworkStreamConfig::new("https://cdn.test/live/index.m3u8"),
            status: NetworkStreamStatus {
                state: NetworkStreamState::Live,
                bitrate_bps: 2_000_000,
                buffer_ms: 300,
                ..NetworkStreamStatus::default()
            },
        },
    ];
    let line = encode_line(&fm_protocol_test_snapshot(entries.clone())).unwrap();
    let WireMessage::Snapshot(decoded) = decode_line(&line).unwrap() else {
        panic!("{line}");
    };
    assert_eq!(decoded.network_streams, entries);

    // At most one entry per network input, ascending.
    let mut descending = entries;
    descending.reverse();
    assert!(encode_line(&fm_protocol_test_snapshot(descending)).is_err());
}

fn fm_protocol_test_snapshot(network_streams: Vec<NetworkStreamEntry>) -> WireMessage {
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
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams,
        capture_inputs: Vec::new(),
    })
}
