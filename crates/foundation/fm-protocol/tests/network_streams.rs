//! Protocol 2.46 network stream inputs (IN-015), with Protocol 2.49's RTMP
//! and SRT fields (IN-016, IN-017): `input_add` with `kind=network_stream`,
//! the `input_stream_set` command (and its stream key and passphrase edits),
//! the `network_stream_changed` durable event, the `network_stream_status`
//! runtime event, and the snapshot's `network_streams` — exactly, and
//! strictly, with every secret masked in what a server reports.

use std::num::NonZeroU32;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, GiveUp, LiveInputSource,
    NetworkStreamConfig, NetworkStreamEntry, NetworkStreamSettings, NetworkStreamState,
    NetworkStreamStatus, ProtocolVersion, ReconnectPolicy, RtspTransport, RuntimeEventMessage,
    RuntimeLifecycleEvent, SecretEdit, ServerIdentity, StallSlate, StreamUrl, WireInputId,
    WireMessage, decode_line, encode_line,
};
use fm_types::network_stream::{AudioChannels, SrtKeyLength, StreamMode};

const PREFIX: &str = "command\tprotocol=2.134\tid=a\tidempotency_key=k";

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
        ..NetworkStreamSettings::default()
    }
}

const SETTINGS: &str = "transport=tcp\tjitter_ms=500\tbackoff_min_ms=250\tbackoff_max_ms=8000\tgive_up=5\ttimeout_ms=3000\tstall=black\tmode=caller\tlatency_ms=120\tkey_length=16\taudio=mix";

/// `SetNetworkStream` keeping both secrets and without a stream ID.
fn set(url: Option<StreamUrl>, settings: NetworkStreamSettings) -> CommandPayload {
    CommandPayload::SetNetworkStream {
        input: input(5),
        url,
        settings,
        stream_id: None,
        stream_key: SecretEdit::Keep,
        passphrase: SecretEdit::Keep,
    }
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 132));
}

#[test]
fn commands_have_an_exact_wire_form() {
    let add = command(CommandPayload::AddInput {
        input: input(5),
        name: "Camera".into(),
        source: LiveInputSource::NetworkStream(Box::new(NetworkStreamConfig {
            url: "rtsp://admin:pw@cam.test:554/live".into(),
            settings: settings(),
            ..NetworkStreamConfig::default()
        })),
    });
    assert_eq!(
        round_trip(&add),
        format!(
            "{PREFIX}\tpayload=input_add\tinput=5\tname=Camera\tkind=network_stream\turl=rtsp%3A%2F%2Fadmin%3Apw%40cam.test%3A554%2Flive\t{SETTINGS}\n"
        )
    );
    let set_udp = command(set(
        Some(StreamUrl::new("udp://@:9000")),
        NetworkStreamSettings::default(),
    ));
    assert_eq!(
        round_trip(&set_udp),
        format!(
            "{PREFIX}\tpayload=input_stream_set\tinput=5\turl=udp%3A%2F%2F%40%3A9000\ttransport=auto\tjitter_ms=300\tbackoff_min_ms=500\tbackoff_max_ms=10000\tgive_up=never\ttimeout_ms=5000\tstall=freeze\tmode=caller\tlatency_ms=120\tkey_length=16\taudio=mix\n"
        )
    );
    // Without a URL the edit keeps the input's URL.
    let keep = command(set(None, settings()));
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
        ("mode=caller", "mode=push"),
        ("\tmode=caller", ""),
        ("latency_ms=120", "latency_ms=19"),
        ("latency_ms=120", "latency_ms=8001"),
        ("key_length=16", "key_length=20"),
        ("audio=mix", "audio=9-10"),
        ("\taudio=mix", ""),
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
        encode_line(&command(set(Some(StreamUrl::new(long)), settings()))),
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
        ..NetworkStreamConfig::default()
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
        system_audio: None,
        screen_capture: None,
        capture: None,
        still: None,
    });
    let line = round_trip(&added);
    assert!(line.contains("\tkind=network_stream\turl="), "{line}");
    let plain = event(EventPayload::InputAdded {
        input: input(6),
        name: "Slate".into(),
        network_stream: None,
        system_audio: None,
        screen_capture: None,
        capture: None,
        still: None,
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
        source_channels: 8,
        last_error: Some("Connection refused ~ 127.0.0.1:9000".into()),
    };
    for state in [
        NetworkStreamState::Idle,
        NetworkStreamState::Connecting,
        NetworkStreamState::Listening,
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
        assert!(
            line.contains("\treconnects=2\tsource_channels=8\t"),
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
                ..NetworkStreamConfig::default()
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
    let mut descending = entries.clone();
    descending.reverse();
    assert!(encode_line(&fm_protocol_test_snapshot(descending)).is_err());

    // IN-016/IN-017: an SRT entry's stream ID (raw, length-prefixed, with
    // the separators), mode, latency, key length, audio pair, and the set
    // flags of its secrets, which travel only as `***`.
    let mut srt = NetworkStreamConfig::new("srt://0.0.0.0:9000");
    srt.settings.mode = StreamMode::Listener;
    srt.settings.latency_ms = 250;
    srt.settings.key_length = SrtKeyLength::Aes256;
    srt.settings.audio = AudioChannels::Pair78;
    srt.stream_id = Some("#!::r=live/a,b~c:d".into());
    srt.passphrase = Some("correct-horse-battery".into());
    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("sk_live_123".into());
    let mut with_secrets = entries;
    with_secrets.push(NetworkStreamEntry {
        input: input(9),
        config: srt.redacted(),
        status: NetworkStreamStatus {
            state: NetworkStreamState::Listening,
            source_channels: 8,
            ..NetworkStreamStatus::default()
        },
    });
    with_secrets.push(NetworkStreamEntry {
        input: input(10),
        config: rtmp.redacted(),
        status: NetworkStreamStatus::default(),
    });
    let line = encode_line(&fm_protocol_test_snapshot(with_secrets.clone())).unwrap();
    for secret in ["correct-horse-battery", "sk_live_123"] {
        assert!(!line.contains(secret), "{line}");
    }
    let WireMessage::Snapshot(decoded) = decode_line(&line).unwrap() else {
        panic!("{line}");
    };
    assert_eq!(decoded.network_streams, with_secrets);
    assert_eq!(
        decoded.network_streams[2].config.passphrase.as_deref(),
        Some("***")
    );
    // A snapshot never carries a secret in the clear.
    let mut leaked = with_secrets;
    leaked[3].config = rtmp;
    assert!(encode_line(&fm_protocol_test_snapshot(leaked)).is_err());
}

#[test]
fn rtmp_and_srt_configs_have_an_exact_wire_form() {
    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("sk_live_123".into());
    let add = command(CommandPayload::AddInput {
        input: input(5),
        name: "OBS".into(),
        source: LiveInputSource::NetworkStream(Box::new(rtmp.clone())),
    });
    assert_eq!(
        round_trip(&add),
        format!(
            "{PREFIX}\tpayload=input_add\tinput=5\tname=OBS\tkind=network_stream\turl=rtmp%3A%2F%2F0.0.0.0%3A1935%2Flive\ttransport=auto\tjitter_ms=300\tbackoff_min_ms=500\tbackoff_max_ms=10000\tgive_up=never\ttimeout_ms=5000\tstall=freeze\tmode=listener\tlatency_ms=120\tkey_length=16\taudio=mix\tstream_key=sk_live_123\n"
        )
    );
    let mut srt = NetworkStreamConfig::new("srt://enc.test:9000");
    srt.settings.mode = StreamMode::Rendezvous;
    srt.settings.key_length = SrtKeyLength::Aes192;
    srt.settings.audio = AudioChannels::Pair34;
    srt.settings.latency_ms = 400;
    srt.stream_id = Some("cam 1".into());
    srt.passphrase = Some("0123456789".into());
    let add = command(CommandPayload::AddInput {
        input: input(6),
        name: "Remote".into(),
        source: LiveInputSource::NetworkStream(Box::new(srt.clone())),
    });
    let line = round_trip(&add);
    assert!(
        line.ends_with(
            "\tmode=rendezvous\tlatency_ms=400\tkey_length=24\taudio=3-4\tstream_id=cam%201\tpassphrase=0123456789\n"
        ),
        "{line}"
    );
    // Commands are validated whole: a passphrase on RTMP, a short
    // passphrase, a listener below port 1024, and a rendezvous over RTMP
    // are refused on both sides.
    for (from, to) in [
        ("passphrase=0123456789", "passphrase=short"),
        ("key_length=24", "key_length=8"),
    ] {
        refused(&line.replacen(from, to, 1));
    }
    let mut bad = rtmp.clone();
    bad.passphrase = Some("0123456789".into());
    let mut low_port = rtmp.clone();
    low_port.url = "rtmp://0.0.0.0:80/live".into();
    let mut rendezvous = rtmp;
    rendezvous.settings.mode = StreamMode::Rendezvous;
    for config in [bad, low_port, rendezvous] {
        assert!(
            encode_line(&command(CommandPayload::AddInput {
                input: input(5),
                name: "OBS".into(),
                source: LiveInputSource::NetworkStream(Box::new(config)),
            }))
            .is_err()
        );
    }
    // A report carries `***` for each secret, and only `***`.
    let changed = event(EventPayload::NetworkStreamChanged {
        input: input(6),
        config: Box::new(srt.redacted()),
    });
    let line = round_trip(&changed);
    assert!(line.contains("\tpassphrase=%2A%2A%2A"), "{line}");
    assert!(!line.contains("0123456789"), "{line}");
    refused(&line.replacen("passphrase=%2A%2A%2A", "passphrase=0123456789", 1));
    assert!(
        encode_line(&event(EventPayload::NetworkStreamChanged {
            input: input(6),
            config: Box::new(srt),
        }))
        .is_err(),
        "an event never carries a secret in the clear"
    );
}

#[test]
fn stream_set_keeps_clears_or_replaces_each_secret() {
    let edit = |stream_key, passphrase, stream_id| {
        command(CommandPayload::SetNetworkStream {
            input: input(5),
            url: None,
            settings: settings(),
            stream_id,
            stream_key,
            passphrase,
        })
    };
    let keep = round_trip(&edit(SecretEdit::Keep, SecretEdit::Keep, None));
    assert!(
        !keep.contains("stream_key") && !keep.contains("passphrase"),
        "{keep}"
    );
    let clear = round_trip(&edit(SecretEdit::Clear, SecretEdit::Clear, None));
    assert!(clear.ends_with("\tstream_key=\tpassphrase=\n"), "{clear}");
    let replace = round_trip(&edit(
        SecretEdit::Set("k2".into()),
        SecretEdit::Set("0123456789".into()),
        Some("cam2".into()),
    ));
    assert!(
        replace.ends_with("\tstream_id=cam2\tstream_key=k2\tpassphrase=0123456789\n"),
        "{replace}"
    );
    // The mask is never a secret, and an empty stream ID is no stream ID.
    refused(&keep.replacen('\n', "\tstream_key=%2A%2A%2A\n", 1));
    refused(&keep.replacen('\n', "\tstream_id=\n", 1));
    assert!(encode_line(&edit(SecretEdit::Set("***".into()), SecretEdit::Keep, None)).is_err());
    let debug = format!(
        "{:?}",
        edit(SecretEdit::Set("hunter2".into()), SecretEdit::Keep, None)
    );
    assert!(
        !debug.contains("hunter2"),
        "Debug masks a new secret: {debug}"
    );
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
                sends: fm_types::audio_bus::AudioSends::MASTER,
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
        paged_states: 0,
        cue_list_states: 0,
        script_states: 0,
        shortcut_states: 0,
        still_states: 0,
        media_list_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams,
        system_audio_inputs: Vec::new(),
        screen_capture_inputs: Vec::new(),
        capture_inputs: Vec::new(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
        alert_rules: Vec::new(),
        transitions_in_flight: Vec::new(),
    })
}
