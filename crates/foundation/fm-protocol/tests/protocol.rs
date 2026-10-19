use core::{fmt::Write, num::NonZeroU128};

use fm_command::{Deadline, Revision, StateEpoch};
use fm_protocol::{
    AudioMeterChannel, AudioMetersMessage, CURRENT_PROTOCOL_VERSION, CapabilityReportMessage,
    CapabilityReportSummary, CodecError, CommandMessage, CommandPayload, CommandResult,
    DiagnosticsRequest, DiagnosticsResponse, DurableEvent, DurableEventBatch, DurableGap,
    EditHistoryStatus, EditLockScope, EditLockStatus, EngineIdentity, ErrorMessage, EventCursor,
    EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState, FieldIssue,
    HandshakeOutcome, HeartbeatAcknowledgementMessage, HeartbeatMessage, InputAudioMeters,
    InputAudioStripStatus, InputStatus, LineDecoder, LiveInputSource, MAX_EDIT_HISTORY_DEPTH,
    MAX_FIELD_VALUE_BYTES, MAX_FIELDS_PER_MESSAGE, MAX_LINE_BYTES, MAX_LIST_ITEMS,
    MAX_MESSAGES_PER_PUSH, MAX_MONITOR_FRAME_HEIGHT, MAX_MONITOR_FRAME_PART_BYTES,
    MAX_MONITOR_FRAME_PARTS, MAX_MONITOR_FRAME_WIDTH, ManualTransitionKind,
    ManualTransitionPosition, ManualTransitionState, ManualTransitionStatus, MonitorFrameAssembler,
    MonitorFrameMessage, MonitorFrameOrigin, MonitorTap, OutputStatus, OverlayStatus,
    OverlayTransitionKind, ProtocolVersion, RecordingStatus, ResumeCursor, RuntimeDomainBoundary,
    RuntimeEventMessage, RuntimeFailureDisposition, RuntimeLifecycleEvent, ServerIdentity,
    SnapshotMessage, SnapshotReason, StingerAudioPolicy, StingerMissingMediaFallback,
    StingerReadiness, StingerStatus, StreamRealizedState, StreamStatus, StructuredError,
    WireInputId, WireMessage, WireOutputId, WireOverlayChannelId, WireRecorderSlot,
    WireStingerSlotId, WireStreamTargetId, choose_handshake_outcome, decode_line, decode_qoi_rgb,
    encode_line, encode_qoi_rgb, monitor_frame_parts,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn recorder_slot(value: u8) -> WireRecorderSlot {
    WireRecorderSlot::new(value).unwrap()
}

fn stream_target(value: u128) -> WireStreamTargetId {
    WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
}

fn input_statuses(values: &[(u128, &str)]) -> Vec<InputStatus> {
    values
        .iter()
        .map(|&(value, name)| InputStatus {
            input: input(value),
            name: name.into(),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        })
        .collect()
}

fn input_audio_strips(values: &[u128]) -> Vec<InputAudioStripStatus> {
    values
        .iter()
        .map(|&value| InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(value),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        })
        .collect()
}

fn output(value: u128) -> WireOutputId {
    WireOutputId::new(NonZeroU128::new(value).unwrap())
}

fn identity() -> EngineIdentity {
    EngineIdentity {
        engine_id: "engine-a".to_owned(),
        state_epoch: 7,
        log_id: "log-a".to_owned(),
    }
}

fn cursor() -> EventCursor {
    EventCursor {
        engine: identity(),
        revision: 1_842,
    }
}

fn server_identity() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".to_owned(),
        project_id: "project-9".to_owned(),
        state_epoch: 7,
        log_id: "log-a".to_owned(),
    }
}

fn resume_cursor() -> ResumeCursor {
    ResumeCursor {
        server: server_identity(),
        revision: 1_842,
    }
}

fn structured_error() -> StructuredError {
    StructuredError {
        code: "invalid_command".to_owned(),
        message: "bad request".to_owned(),
        fields: vec![FieldIssue {
            field: "command.duration".to_owned(),
            code: "positive".to_owned(),
            message: "must be > 0".to_owned(),
        }],
        retryable: false,
    }
}

fn command() -> CommandMessage {
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:test".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(1_842),
        deadline_ms: Some(500),
        payload: CommandPayload::SelectPreview { input: input(42) },
    }
}

fn stream_status(value: u128, name: &str, realized: StreamRealizedState) -> StreamStatus {
    StreamStatus {
        target: stream_target(value),
        name: name.into(),
        desired_running: true,
        realized,
        detail: None,
    }
}

fn snapshot(inputs: Vec<InputStatus>) -> WireMessage {
    WireMessage::Snapshot(SnapshotMessage {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs: Vec::new(),
        capture_inputs: Vec::new(),
        engine: identity(),
        revision: 9,
        show_name: "My Show\nA".to_owned(),
        inputs,
        outputs: vec![
            OutputStatus {
                output: output(7),
                name: "Main".into(),
            },
            OutputStatus {
                output: output(8),
                name: "Aux".into(),
            },
        ],
        input_audio_strips: input_audio_strips(&[1, 2]),
        desired_program: input(2),
        desired_preview: input(1),
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
        recordings: vec![
            RecordingStatus {
                recorder: recorder_slot(1),
                desired_active: false,
            },
            RecordingStatus {
                recorder: recorder_slot(2),
                desired_active: false,
            },
        ],
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks: EditLockStatus::default(),
        edit_history: EditHistoryStatus::EMPTY,
        titles: Vec::new(),
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: fm_types::FrameRate::new(30_000, 1_001).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        cue_list_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
    })
}

#[test]
fn protocol_2_20_heartbeat_acknowledgement_codec_is_exact() {
    let acknowledgement = WireMessage::HeartbeatAcknowledgement(HeartbeatAcknowledgementMessage {
        server: server_identity(),
        heartbeat_sequence: 88,
        received_at_ms: 1_720_000_000_003,
    });
    let encoded = encode_line(&acknowledgement).unwrap();
    assert_eq!(
        encoded,
        "heartbeat_acknowledgement\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\theartbeat_sequence=88\treceived_at_ms=1720000000003\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), acknowledgement);
}

#[test]
#[allow(clippy::too_many_lines)]
fn audio_meter_codec_is_bounded_and_exact() {
    let message = WireMessage::AudioMeters(AudioMetersMessage {
        server: server_identity(),
        sequence: 9,
        frame: 42,
        start_sample: 80_000,
        end_sample: 81_920,
        master: vec![
            AudioMeterChannel {
                peak_millionths: 4_000_000,
                rms_millionths: 500_000,
            },
            AudioMeterChannel {
                peak_millionths: 250_000,
                rms_millionths: 125_000,
            },
        ],
        inputs: vec![InputAudioMeters {
            input: input(3),
            channels: vec![
                AudioMeterChannel {
                    peak_millionths: 500_000,
                    rms_millionths: 250_000,
                },
                AudioMeterChannel::default(),
            ],
        }],
        monitor: Some(fm_protocol::AudioMonitorMeters {
            solo: true,
            output: fm_protocol::AudioMonitorOutputState::Playing,
            channels: vec![
                AudioMeterChannel {
                    peak_millionths: 500_000,
                    rms_millionths: 250_000,
                },
                AudioMeterChannel {
                    peak_millionths: 500_000,
                    rms_millionths: 250_000,
                },
            ],
        }),
    });
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "audio_meters\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\tsequence=9\tframe=42\tstart_sample=80000\tend_sample=81920\tmaster=4000000%3A500000%2C250000%3A125000\tinputs=3%7E500000%3A250000%2C0%3A0\tmonitor=solo%7Eplaying%7E500000%3A250000%2C500000%3A250000\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);

    // The monitor bus follows Master, or is absent (`-`); its output device
    // is off, playing, or failed (2.53).
    for (from, to) in [
        ("monitor=solo%7E", "monitor=master%7E"),
        ("monitor=solo%7Eplaying%7E", "monitor=solo%7Eoff%7E"),
        ("monitor=solo%7Eplaying%7E", "monitor=solo%7Efailed%7E"),
        (
            "monitor=solo%7Eplaying%7E500000%3A250000%2C500000%3A250000",
            "monitor=-",
        ),
    ] {
        let line = encoded.replace(from, to);
        assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);
    }
    // A wrong channel count, unknown source or output state, the 2.44 shape
    // without an output state, or a missing field is refused.
    for (from, to) in [
        (
            "monitor=solo%7Eplaying%7E500000%3A250000%2C500000%3A250000",
            "monitor=solo%7Eplaying%7E500000%3A250000",
        ),
        ("monitor=solo%7E", "monitor=pfl%7E"),
        ("monitor=solo%7Eplaying%7E", "monitor=solo%7Eon%7E"),
        ("monitor=solo%7Eplaying%7E", "monitor=solo%7E"),
        ("monitor=solo%7E", "monitor="),
        (
            "\tmonitor=solo%7Eplaying%7E500000%3A250000%2C500000%3A250000",
            "",
        ),
    ] {
        assert!(decode_line(&encoded.replace(from, to)).is_err(), "{to}");
    }

    let invalid = encoded.replace("inputs=3%7E", "inputs=3%7E500000%3A250000%2C0%3A0%3B3%7E");
    assert!(matches!(
        decode_line(&invalid),
        Err(CodecError::InvalidField {
            field: "inputs",
            ..
        })
    ));

    let WireMessage::AudioMeters(mut wrong_channels) = message else {
        unreachable!()
    };
    let mut wrong_monitor = wrong_channels.clone();
    wrong_channels.inputs[0].channels.pop();
    assert!(matches!(
        encode_line(&WireMessage::AudioMeters(wrong_channels)),
        Err(CodecError::InvalidField {
            field: "inputs",
            ..
        })
    ));
    wrong_monitor.monitor.as_mut().unwrap().channels.pop();
    assert!(matches!(
        encode_line(&WireMessage::AudioMeters(wrong_monitor)),
        Err(CodecError::InvalidField {
            field: "monitor",
            ..
        })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_message_variant_round_trips() {
    let messages = vec![
        WireMessage::MonitorFrame(MonitorFrameMessage {
            server: server_identity(),
            tap: MonitorTap::Preview,
            sequence: 1,
            frame: 2,
            input: input(3),
            origin: MonitorFrameOrigin::Native,
            width: 1,
            height: 1,
            part: 0,
            parts: 1,
            data: vec![0xfb, 0xff, 0x00],
        }),
        WireMessage::AudioMeters(AudioMetersMessage {
            server: server_identity(),
            sequence: 1,
            frame: 1,
            start_sample: 0,
            end_sample: 1,
            master: vec![AudioMeterChannel::default()],
            inputs: Vec::new(),
            monitor: None,
        }),
        WireMessage::DiagnosticsRequest(DiagnosticsRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "diag-01".to_owned(),
        }),
        WireMessage::DiagnosticsResponse(DiagnosticsResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "diag-01".to_owned(),
            engine: identity(),
            current_revision: 1_842,
            oldest_retained_revision: Some(1_700),
            newest_retained_revision: Some(1_842),
            subscriber_count: 3,
            retained_events_limit: 4_096,
            subscriber_limit: 16,
            subscriber_queue_limit: 256,
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::ReorderInputs {
                inputs: vec![input(2), input(1)],
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::SetInputAudioStrip {
                pan_basis_points: 0,
                input: input(42),
                gain_millidb: -6_000,
                balance_basis_points: 2_500,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 2_400,
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::Cut,
            expected_revision: None,
            deadline_ms: None,
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::Fade { duration_frames: 9 },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload: CommandPayload::AlphaFade { duration_frames: 9 },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload: CommandPayload::Slide { duration_frames: 9 },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload: CommandPayload::Zoom { duration_frames: 9 },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload: CommandPayload::Wipe { duration_frames: 9 },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::TakeOverlay {
                channel: WireOverlayChannelId::new(1).unwrap(),
                source: input(42),
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::UpdateOverlay {
                channel: WireOverlayChannelId::new(8).unwrap(),
                source: input(7),
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::OverlayOff {
                channel: WireOverlayChannelId::new(2).unwrap(),
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::SetOverlayOutputInclusion {
                channel: WireOverlayChannelId::new(3).unwrap(),
                output: output(9),
                included: true,
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::ConfigureOverlayTransition {
                channel: WireOverlayChannelId::new(4).unwrap(),
                transition: OverlayTransitionKind::Fade,
                duration_frames: 24,
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::ConfigureOverlayAppearance {
                channel: WireOverlayChannelId::new(5).unwrap(),
                position: fm_protocol::OverlayPositionPreset::BottomRight,
                border: fm_protocol::OverlayBorderPreset::ThickWhite,
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::QueueOverlay {
                channel: WireOverlayChannelId::new(6).unwrap(),
                source: input(3),
            },
            ..command()
        }),
        WireMessage::Command(CommandMessage {
            payload: CommandPayload::TakeNextOverlay {
                channel: WireOverlayChannelId::new(6).unwrap(),
            },
            ..command()
        }),
        WireMessage::CommandResult(CommandResult::Accepted {
            id: "accepted".to_owned(),
            revision: 9,
            scheduled_frame: Some(12),
        }),
        WireMessage::CommandResult(CommandResult::Rejected {
            id: "rejected".to_owned(),
            code: "invalid_command".to_owned(),
            message: "bad\tvalue".to_owned(),
            fields: vec![FieldIssue {
                field: "payload.duration".to_owned(),
                code: "zero".to_owned(),
                message: "must be > 0".to_owned(),
            }],
            current_revision: 9,
            retryable: false,
            reason: None,
        }),
        snapshot(input_statuses(&[(1, "Camera, A"), (2, "Slides ~ Main")])),
        WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(1),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(&[1, 2]),
            },
        }),
        WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::InputOrderChanged {
                inputs: vec![input(2), input(1)],
            },
        }),
    ];
    for message in messages {
        let encoded = encode_line(&message).unwrap();
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    assert!(matches!(
        decode_line("diagnostics_request\tprotocol=2.13\trequest_id= \n"),
        Err(CodecError::InvalidField {
            field: "request_id",
            ..
        })
    ));
    assert_eq!(
        decode_line(&format!(
            "diagnostics_request\tprotocol=2.13\trequest_id={}\n",
            "x".repeat(MAX_FIELD_VALUE_BYTES + 1)
        )),
        Err(CodecError::FieldValueTooLong)
    );
    assert_eq!(
        decode_line(&format!(
            "diagnostics_request\tprotocol=2.13\trequest_id={}\n",
            "x".repeat(129)
        )),
        Err(CodecError::InvalidField {
            field: "request_id",
            value: "x".repeat(129),
        })
    );
    assert_eq!(
        decode_line("diagnostics_request\tprotocol=2.13\trequest_id=x%09y\n"),
        Err(CodecError::InvalidField {
            field: "request_id",
            value: "x\ty".to_owned(),
        })
    );
    assert_eq!(
        encode_line(&WireMessage::DiagnosticsRequest(DiagnosticsRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "x".repeat(129),
        })),
        Err(CodecError::InvalidField {
            field: "request_id",
            value: "x".repeat(129),
        })
    );
    assert_eq!(
        encode_line(&WireMessage::DiagnosticsRequest(DiagnosticsRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "x\ty".to_owned(),
        })),
        Err(CodecError::InvalidField {
            field: "request_id",
            value: "x\ty".to_owned(),
        })
    );

    for malformed in [
        "command\tprotocol=2.13\tid=01K%3Atest\tidempotency_key=operator-7%3A01K\tpayload=input_reorder\tinputs=\n",
        "command\tprotocol=2.13\tid=01K%3Atest\tidempotency_key=operator-7%3A01K\tpayload=input_reorder\tinputs=1%2C1\n",
    ] {
        assert!(matches!(
            decode_line(malformed),
            Err(CodecError::InvalidField {
                field: "inputs",
                ..
            })
        ));
    }
}

#[test]
fn input_statuses_reject_duplicate_ids() {
    let duplicate = snapshot(input_statuses(&[(1, "camera"), (1, "slides")]));
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "inputs",
            ..
        })
    ));

    let valid = encode_line(&snapshot(input_statuses(&[(1, "camera"), (2, "slides")]))).unwrap();
    let duplicate = valid.replace(
        "inputs=1%7Ecamera%7E-%3A-%3A0%3A0%3A-%2C2%7Eslides%7E-%3A-%3A0%3A0%3A-",
        "inputs=1%7Ecamera%7E-%3A-%3A0%3A0%3A-%2C1%7Eslides%7E-%3A-%3A0%3A0%3A-",
    );
    assert!(matches!(
        decode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "inputs",
            ..
        })
    ));
}

#[test]
fn output_statuses_preserve_escaped_order_and_reject_duplicate_ids() {
    let mut message = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!();
    };
    snapshot.outputs = vec![
        OutputStatus {
            output: output(7),
            name: "Clean, Main".into(),
        },
        OutputStatus {
            output: output(8),
            name: "Aux ~ ISO".into(),
        },
    ];
    let encoded = encode_line(&message).unwrap();
    assert_eq!(decode_line(&encoded), Ok(message.clone()));

    let mut duplicate = message.clone();
    let WireMessage::Snapshot(snapshot) = &mut duplicate else {
        unreachable!();
    };
    snapshot.outputs[1].output = output(7);
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "outputs",
            ..
        })
    ));

    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!();
    };
    snapshot.outputs.clear();
    let encoded = encode_line(&message).unwrap();
    assert_eq!(decode_line(&encoded), Ok(message));
}

#[test]
fn overlay_included_outputs_reject_duplicates() {
    let mut duplicate = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut duplicate else {
        unreachable!();
    };
    snapshot_message.desired_overlays[0].included_outputs =
        vec![output(7_000_000_001), output(7_000_000_001)];
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "desired_overlays",
            ..
        })
    ));

    let mut valid = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut valid else {
        unreachable!();
    };
    snapshot_message.desired_overlays[0].included_outputs =
        vec![output(7_000_000_001), output(7_000_000_002)];
    let valid = encode_line(&valid).unwrap();
    let duplicate = valid.replace("7000000001%2C7000000002", "7000000001%2C7000000001");
    assert_ne!(duplicate, valid);
    assert!(matches!(
        decode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "desired_overlays",
            ..
        })
    ));
}

#[test]
fn input_audio_strip_status_rejects_duplicates_and_out_of_range_controls() {
    let strip_event = |input_audio_strips| {
        WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips,
            },
        })
    };

    let duplicate = strip_event(vec![
        InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(1),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        },
        InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(1),
            gain_millidb: -6_000,
            balance_basis_points: 5_000,
            muted: true,
            soloed: true,
            follow_video: false,
            delay_samples: 1,
        },
    ]);
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "input_audio_strips",
            ..
        })
    ));

    let valid = encode_line(&strip_event(input_audio_strips(&[1, 2]))).unwrap();
    assert!(
        valid.contains("1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-"),
        "{valid}"
    );
    for malformed in [
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A48001%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A24001%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A10001%3A0%3A0%3A0%3A1%3A0%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A-10001%3A0%3A0%3A1%3A0%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A2%3A1%3A0%3A-",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A5",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A0",
        ),
        valid.replace(
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-",
            "1%3A0%3A0%3A0%3A0%3A0%3A1%3A0%3A-%2C2%3A0%3A0%3A0%3A0%3A0%3A1%3A0",
        ),
    ] {
        assert!(matches!(
            decode_line(&malformed),
            Err(CodecError::InvalidField {
                field: "input_audio_strips",
                ..
            })
        ));
    }
}

#[test]
fn stinger_slot_mutations_round_trip_exact_configuration() {
    for payload in [
        CommandPayload::ConfigureStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
            media_input: input(42),
            preload: true,
            cut_point_frames: 17,
            audio_policy: StingerAudioPolicy::MixWithProgram,
            missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
        },
        CommandPayload::RemoveStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
        },
    ] {
        let message = WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload,
            ..command()
        });
        let encoded = encode_line(&message).unwrap();
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    let event = WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: cursor(),
        payload: EventPayload::StingerSlotsChanged {
            program: input(1),
            preview: input(2),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            stingers: vec![StingerStatus {
                slot: WireStingerSlotId::new(8).unwrap(),
                media_input: input(42),
                preload: true,
                cut_point_frames: 17,
                audio_policy: StingerAudioPolicy::MixWithProgram,
                missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
                readiness: StingerReadiness::Ready,
            }],
            overlays: OverlayStatus::empty_channels(),
            input_audio_strips: input_audio_strips(&[1, 2, 42]),
        },
    });
    let encoded = encode_line(&event).unwrap();
    assert_eq!(decode_line(&encoded).unwrap(), event);
}

#[test]
fn stinger_encoder_rejects_too_many_or_duplicate_slots() {
    let status = StingerStatus {
        slot: WireStingerSlotId::new(1).unwrap(),
        media_input: input(42),
        preload: true,
        cut_point_frames: 17,
        audio_policy: StingerAudioPolicy::MixWithProgram,
        missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
        readiness: StingerReadiness::Ready,
    };
    let event = |stingers| {
        WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::StingerSlotsChanged {
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                stingers,
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(&[1, 2]),
            },
        })
    };
    assert!(matches!(
        encode_line(&event(vec![status, status])),
        Err(CodecError::InvalidField {
            field: "stingers",
            ..
        })
    ));
    assert_eq!(
        encode_line(&event(vec![status; 9])),
        Err(CodecError::TooManyItems("stingers"))
    );
}

#[test]
fn stream_start_and_stop_commands_round_trip_and_reject_invalid_targets() {
    for (expected_payload, payload) in [
        (
            "payload=stream_start",
            CommandPayload::StreamStart {
                target: stream_target(9_001),
            },
        ),
        (
            "payload=stream_stop",
            CommandPayload::StreamStop {
                target: stream_target(9_002),
            },
        ),
    ] {
        let message = WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload,
            ..command()
        });
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.contains(expected_payload));
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    let start = encode_line(&WireMessage::Command(CommandMessage {
        payload: CommandPayload::StreamStart {
            target: stream_target(9_001),
        },
        ..command()
    }))
    .unwrap();
    assert!(start.contains("target=9001"));

    for malformed in [
        start.replace("\ttarget=9001", ""),
        start.replace("target=9001", "target=0"),
        start.replace("target=9001", "target=twitch"),
    ] {
        if malformed.contains("target=") {
            assert!(matches!(
                decode_line(&malformed),
                Err(CodecError::InvalidField {
                    field: "target",
                    ..
                })
            ));
        } else {
            assert_eq!(
                decode_line(&malformed),
                Err(CodecError::MissingField("target"))
            );
        }
    }
    let unknown = start.replace("payload=stream_start", "payload=stream_pause");
    assert!(matches!(
        decode_line(&unknown),
        Err(CodecError::InvalidField {
            field: "payload",
            ..
        })
    ));
}

#[test]
fn snapshot_streams_round_trip_and_reject_duplicates_and_bounds() {
    let mut message = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut message else {
        unreachable!();
    };
    snapshot_message.streams = vec![
        StreamStatus {
            target: stream_target(9_001),
            name: "Twitch, Main".into(),
            desired_running: true,
            realized: StreamRealizedState::Live,
            detail: None,
        },
        StreamStatus {
            target: stream_target(9_002),
            name: "Restream ~ Backup".into(),
            desired_running: false,
            realized: StreamRealizedState::Failed,
            detail: Some("auth_rejected".into()),
        },
    ];
    let encoded = encode_line(&message).unwrap();
    assert!(encoded.contains("streams=9001%3A"));
    assert_eq!(decode_line(&encoded).unwrap(), message);

    let mut duplicate = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut duplicate else {
        unreachable!();
    };
    snapshot_message.streams[1].target = stream_target(9_001);
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));

    let duplicate_line = encoded.replace("9002%3A", "9001%3A");
    assert!(matches!(
        decode_line(&duplicate_line),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));

    let unknown_state = encoded.replace("%3A1%3Alive%3A", "%3A1%3Areconnecting%3A");
    assert!(matches!(
        decode_line(&unknown_state),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));
}

/// The snapshot's configured destinations (2.54): an SRT entry carries its
/// latency and whether a passphrase is stored; other protocols carry an
/// empty latency, and the passphrase never travels.
#[test]
fn snapshot_stream_targets_carry_srt_latency_and_passphrase_presence() {
    let mut message = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut message else {
        unreachable!();
    };
    let config = |name: &str, protocol, endpoint: &str, latency| fm_protocol::StreamTargetConfig {
        name: name.into(),
        protocol,
        endpoint: endpoint.into(),
        backup_endpoint: None,
        output: fm_protocol::WireOutputId::new(NonZeroU128::new(1).unwrap()),
        reconcile_on_startup: false,
        video_bitrate_kbps: 4_500,
        reconnect: fm_protocol::StreamReconnect::DEFAULT,
        srt_latency_ms: latency,
    };
    snapshot_message.stream_targets = vec![
        fm_protocol::StreamTargetEntry {
            target: stream_target(1),
            config: config(
                "SRT",
                fm_protocol::StreamTargetProtocol::Srt,
                "relay.example:9000",
                Some(300),
            ),
            has_key: true,
            ladder_rungs: 0,
            has_passphrase: true,
        },
        fm_protocol::StreamTargetEntry {
            target: stream_target(2),
            config: config(
                "RTMP",
                fm_protocol::StreamTargetProtocol::Rtmp,
                "ingest.example/live",
                None,
            ),
            has_key: true,
            ladder_rungs: 0,
            has_passphrase: false,
        },
    ];
    let encoded = encode_line(&message).unwrap();
    assert!(encoded.contains("%7E0%7E300%7E1%2C2%7E"), "{encoded}");
    assert!(
        encoded.ends_with(
            "%7E1%7E0%7E%7E0\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n"
        ),
        "{encoded}"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);
    // SRT without a latency, or a passphrase off SRT, is refused.
    for (from, to) in [
        ("%7E0%7E300%7E1%2C", "%7E0%7E%7E1%2C"),
        ("%7E1%7E0%7E%7E0\tlive", "%7E1%7E0%7E%7E1\tlive"),
    ] {
        assert!(encoded.contains(from), "{from}");
        assert!(decode_line(&encoded.replacen(from, to, 1)).is_err());
    }
}

#[test]
fn snapshot_stream_text_bounds_are_enforced_in_both_directions() {
    let mut message = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut message else {
        unreachable!();
    };
    snapshot_message.streams = vec![
        StreamStatus {
            target: stream_target(9_001),
            name: "Twitch, Main".into(),
            desired_running: true,
            realized: StreamRealizedState::Live,
            detail: None,
        },
        StreamStatus {
            target: stream_target(9_002),
            name: "Restream ~ Backup".into(),
            desired_running: false,
            realized: StreamRealizedState::Failed,
            detail: Some("auth_rejected".into()),
        },
    ];
    let encoded = encode_line(&message).unwrap();

    let mut oversized_name = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut oversized_name else {
        unreachable!();
    };
    snapshot_message.streams[0].name = "x".repeat(129);
    assert!(matches!(
        encode_line(&oversized_name),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));
    let oversized_name_line = encoded.replace("Twitch%252C%2520Main", &"x".repeat(129));
    assert!(matches!(
        decode_line(&oversized_name_line),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));

    let mut oversized_detail = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut oversized_detail else {
        unreachable!();
    };
    snapshot_message.streams[1].detail = Some("y".repeat(65));
    assert!(matches!(
        encode_line(&oversized_detail),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));
    let oversized_detail_line = encoded.replace("auth_rejected", &"y".repeat(65));
    assert!(matches!(
        decode_line(&oversized_detail_line),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));

    let mut empty_detail = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut empty_detail else {
        unreachable!();
    };
    snapshot_message.streams[0].detail = Some(String::new());
    assert!(matches!(
        encode_line(&empty_detail),
        Err(CodecError::InvalidField {
            field: "streams",
            ..
        })
    ));
}

#[test]
fn streams_changed_event_round_trips_every_realized_state() {
    let event = |streams| {
        WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::StreamsChanged { streams },
        })
    };
    let populated = event(vec![
        StreamStatus {
            target: stream_target(9_003),
            name: "YouTube A".into(),
            desired_running: true,
            realized: StreamRealizedState::WaitingToReconnect,
            detail: Some("rtmp_timeout".into()),
        },
        StreamStatus {
            target: stream_target(9_004),
            name: "Local".into(),
            desired_running: false,
            realized: StreamRealizedState::Stopped,
            detail: None,
        },
        stream_status(9_005, "Starting", StreamRealizedState::Starting),
        stream_status(9_006, "Slow", StreamRealizedState::Congested),
        stream_status(9_007, "Offline", StreamRealizedState::Unavailable),
    ]);
    let encoded = encode_line(&populated).unwrap();
    assert!(encoded.contains("event=streams_changed"));
    assert_eq!(decode_line(&encoded).unwrap(), populated);

    let empty = event(Vec::new());
    let encoded = encode_line(&empty).unwrap();
    assert_eq!(decode_line(&encoded).unwrap(), empty);

    let unknown_event = encode_line(&populated)
        .unwrap()
        .replace("event=streams_changed", "event=strings_changed");
    assert!(matches!(
        decode_line(&unknown_event),
        Err(CodecError::InvalidField { field: "event", .. })
    ));
}

#[test]
fn record_start_and_stop_commands_round_trip_for_each_slot_and_reject_invalid_slots() {
    for (expected_payload, recorder, payload) in [
        (
            "payload=record_start",
            1,
            CommandPayload::RecordStart {
                recorder: recorder_slot(1),
            },
        ),
        (
            "payload=record_start",
            2,
            CommandPayload::RecordStart {
                recorder: recorder_slot(2),
            },
        ),
        (
            "payload=record_stop",
            1,
            CommandPayload::RecordStop {
                recorder: recorder_slot(1),
            },
        ),
        (
            "payload=record_stop",
            2,
            CommandPayload::RecordStop {
                recorder: recorder_slot(2),
            },
        ),
    ] {
        let message = WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            payload,
            ..command()
        });
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.contains(expected_payload));
        assert!(encoded.contains(&format!("\trecorder={recorder}")));
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    let start = encode_line(&WireMessage::Command(CommandMessage {
        payload: CommandPayload::RecordStart {
            recorder: recorder_slot(1),
        },
        ..command()
    }))
    .unwrap();
    assert!(start.contains("recorder=1"));
    for malformed in [
        start.replace("\trecorder=1", ""),
        start.replace("recorder=1", "recorder=0"),
        start.replace("recorder=1", "recorder=3"),
        start.replace("recorder=1", "recorder=two"),
    ] {
        if malformed.contains("recorder=") {
            assert!(matches!(
                decode_line(&malformed),
                Err(CodecError::InvalidField {
                    field: "recorder",
                    ..
                })
            ));
        } else {
            assert_eq!(
                decode_line(&malformed),
                Err(CodecError::MissingField("recorder"))
            );
        }
    }
    let extra = start.replace('\n', "\ttarget=9\n");
    assert_eq!(
        decode_line(&extra),
        Err(CodecError::UnknownField("target".to_owned()))
    );
    let unknown = start.replace("payload=record_start", "payload=record_pause");
    assert!(matches!(
        decode_line(&unknown),
        Err(CodecError::InvalidField {
            field: "payload",
            ..
        })
    ));
}

#[test]
fn recorder_slot_constructor_bounds_are_exact() {
    for value in 1..=2 {
        assert_eq!(recorder_slot(value).get(), value);
    }
    for value in [0, 3, 4, u8::MAX] {
        assert_eq!(WireRecorderSlot::new(value), None);
    }
}

#[test]
fn snapshot_recordings_roster_round_trips_and_rejects_missing_duplicate_out_of_range_and_extra_entries()
 {
    let mut message = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]));
    let WireMessage::Snapshot(snapshot_message) = &mut message else {
        unreachable!();
    };
    snapshot_message.recordings = vec![
        RecordingStatus {
            recorder: recorder_slot(1),
            desired_active: true,
        },
        RecordingStatus {
            recorder: recorder_slot(2),
            desired_active: false,
        },
    ];
    let encoded = encode_line(&message).unwrap();
    let roster = "recordings=1%3A1%2C2%3A0";
    assert!(encoded.contains(roster));
    assert_eq!(decode_line(&encoded).unwrap(), message);

    let mut missing = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut missing else {
        unreachable!();
    };
    snapshot_message.recordings.truncate(1);
    assert!(matches!(
        encode_line(&missing),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));
    assert!(matches!(
        decode_line(&encoded.replace(roster, "recordings=1%3A1")),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));

    let mut duplicate = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut duplicate else {
        unreachable!();
    };
    snapshot_message.recordings[1].recorder = recorder_slot(1);
    assert!(matches!(
        encode_line(&duplicate),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));
    assert!(matches!(
        decode_line(&encoded.replace(roster, "recordings=1%3A1%2C1%3A0")),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));

    assert!(matches!(
        decode_line(&encoded.replace(roster, "recordings=1%3A1%2C3%3A0")),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));

    let mut extra = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut extra else {
        unreachable!();
    };
    snapshot_message.recordings.push(RecordingStatus {
        recorder: recorder_slot(2),
        desired_active: true,
    });
    assert!(matches!(
        encode_line(&extra),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));
    assert!(matches!(
        decode_line(&encoded.replace(roster, "recordings=1%3A1%2C2%3A0%2C1%3A0")),
        Err(CodecError::InvalidField {
            field: "recordings",
            ..
        })
    ));

    // The roster is canonical slot order in both directions.
    let mut reversed = message.clone();
    let WireMessage::Snapshot(snapshot_message) = &mut reversed else {
        unreachable!();
    };
    snapshot_message.recordings.reverse();
    assert_eq!(encode_line(&reversed).unwrap(), encoded);
    assert_eq!(
        decode_line(&encoded.replace(roster, "recordings=2%3A0%2C1%3A1")).unwrap(),
        message
    );
}

#[test]
fn recording_changed_event_round_trips_both_slots_and_states() {
    for (recorder, active) in [(1, true), (1, false), (2, true), (2, false)] {
        let message = WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::RecordingChanged {
                recorder: recorder_slot(recorder),
                active,
            },
        });
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.contains("event=recording_changed"));
        assert!(encoded.contains(&format!("\trecorder={recorder}")));
        assert!(encoded.contains(&format!("\tactive={}", u8::from(active))));
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    let populated = encode_line(&WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: cursor(),
        payload: EventPayload::RecordingChanged {
            recorder: recorder_slot(2),
            active: true,
        },
    }))
    .unwrap();
    assert_eq!(
        decode_line(&populated.replace("\trecorder=2", "")),
        Err(CodecError::MissingField("recorder"))
    );
    assert!(matches!(
        decode_line(&populated.replace("recorder=2", "recorder=3")),
        Err(CodecError::InvalidField {
            field: "recorder",
            ..
        })
    ));
    assert_eq!(
        decode_line(&populated.replace("\tactive=1", "")),
        Err(CodecError::MissingField("active"))
    );
    assert!(matches!(
        decode_line(&populated.replace("active=1", "active=yes")),
        Err(CodecError::InvalidField {
            field: "active",
            ..
        })
    ));
    let unknown = populated.replace("event=recording_changed", "event=recording_paused");
    assert!(matches!(
        decode_line(&unknown),
        Err(CodecError::InvalidField { field: "event", .. })
    ));
}

#[test]
fn decoder_rejects_out_of_range_manual_position() {
    let fixture = encode_line(&WireMessage::Command(CommandMessage {
        payload: CommandPayload::SetManualTransitionPosition {
            position: fm_protocol::ManualTransitionPosition::new(6_250).unwrap(),
        },
        ..command()
    }))
    .unwrap();
    let invalid = fixture.replace("position_basis_points=6250", "position_basis_points=10001");
    assert!(matches!(
        decode_line(&invalid),
        Err(CodecError::InvalidField {
            field: "position_basis_points",
            ..
        })
    ));
}

#[test]
fn manual_slide_codec_preserves_exact_current_kind_and_interval() {
    let start = WireMessage::Command(CommandMessage {
        payload: CommandPayload::StartManualTransition {
            kind: ManualTransitionKind::Slide,
        },
        ..command()
    });
    let encoded = encode_line(&start).unwrap();
    assert!(encoded.contains("transition=slide"));
    assert_eq!(decode_line(&encoded).unwrap(), start);

    let status = ManualTransitionStatus::Active(ManualTransitionState {
        kind: ManualTransitionKind::Slide,
        from: input(1),
        to: input(2),
        interval_start: ManualTransitionPosition::new(8_000).unwrap(),
        position: ManualTransitionPosition::new(2_500).unwrap(),
    });
    let event = WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: cursor(),
        payload: EventPayload::DesiredSwitcher {
            program: input(1),
            preview: input(2),
            manual_transition: status,
            fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            overlays: OverlayStatus::empty_channels(),
            input_audio_strips: input_audio_strips(&[1, 2]),
        },
    });
    let encoded = encode_line(&event).unwrap();
    assert_eq!(decode_line(&encoded).unwrap(), event);
}

#[test]
fn every_manual_kind_round_trips_and_a_stinger_carries_its_clip_length() {
    let stinger = ManualTransitionKind::Stinger {
        slot: fm_protocol::WireStingerSlotId::new(3).unwrap(),
        duration_frames: 24,
    };
    let kinds = fm_protocol::AutoTransitionKind::ALL
        .map(ManualTransitionKind::from_mix)
        .into_iter()
        .chain([stinger]);
    for kind in kinds {
        let start = WireMessage::Command(CommandMessage {
            payload: CommandPayload::StartManualTransition { kind },
            ..command()
        });
        let encoded = encode_line(&start).unwrap();
        let fields = match kind.duration_frames() {
            Some(frames) => format!("\ttransition={}\tduration_frames={frames}\n", kind.token()),
            None => format!("\ttransition={}\n", kind.token()),
        };
        assert!(encoded.ends_with(&fields), "{encoded}");
        assert_eq!(decode_line(&encoded).unwrap(), start);

        let status = ManualTransitionStatus::Active(ManualTransitionState {
            kind,
            from: input(1),
            to: input(2),
            interval_start: ManualTransitionPosition::new(8_000).unwrap(),
            position: ManualTransitionPosition::new(2_500).unwrap(),
        });
        let event = WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: cursor(),
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: status,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(&[1, 2]),
            },
        });
        let encoded = encode_line(&event).unwrap();
        assert!(
            encoded.contains(&format!("\tmanual_kind={}\t", kind.token())),
            "{encoded}"
        );
        assert_eq!(
            encoded.contains("\tmanual_duration_frames=24\t"),
            kind == stinger,
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), event);
    }

    // A Stinger needs a nonzero clip length; a mix carries none; Cut has
    // no manual form.
    let start = encode_line(&WireMessage::Command(CommandMessage {
        payload: CommandPayload::StartManualTransition { kind: stinger },
        ..command()
    }))
    .unwrap();
    for (from, to) in [
        ("\tduration_frames=24", ""),
        ("\tduration_frames=24", "\tduration_frames=0"),
        ("transition=stinger_3", "transition=stinger_9"),
        ("transition=stinger_3", "transition=fade"),
        ("transition=stinger_3\tduration_frames=24", "transition=cut"),
    ] {
        let malformed = start.replacen(from, to, 1);
        assert_ne!(malformed, start, "{from}");
        assert!(decode_line(&malformed).is_err(), "{from} -> {to}");
    }
    assert!(
        encode_line(&WireMessage::Command(CommandMessage {
            payload: CommandPayload::StartManualTransition {
                kind: ManualTransitionKind::Stinger {
                    slot: fm_protocol::WireStingerSlotId::new(3).unwrap(),
                    duration_frames: 0,
                },
            },
            ..command()
        }))
        .is_err()
    );
}

#[test]
fn exact_contract_rejects_every_unknown_field() {
    let fixture = encode_line(&WireMessage::Command(command())).unwrap();
    let required = fixture.replace('\n', "\tfuture=value\n");
    assert_eq!(
        decode_line(&required),
        Err(CodecError::UnknownField("future".to_owned()))
    );
}

#[test]
fn decoder_rejects_truncation_duplicates_and_invalid_values() {
    let fixture = encode_line(&WireMessage::Command(command())).unwrap();
    assert_eq!(
        decode_line(fixture.trim_end()),
        Err(CodecError::MissingNewline)
    );
    let duplicate = fixture.replace('\n', "\tid=again\n");
    assert_eq!(
        decode_line(&duplicate),
        Err(CodecError::DuplicateField("id".to_owned()))
    );
    let zero_input = fixture.replace("input=42", "input=0");
    assert!(matches!(
        decode_line(&zero_input),
        Err(CodecError::InvalidField { field: "input", .. })
    ));
}

#[test]
fn streaming_decoder_handles_split_and_multiple_records() {
    let command = encode_line(&WireMessage::Command(command())).unwrap();
    let bytes = format!("{command}{command}");
    let split = bytes.len() / 3;
    let mut decoder = LineDecoder::new();
    assert_eq!(
        decoder.push(&bytes.as_bytes()[..split]).unwrap(),
        [] as [fm_protocol::WireMessage; 0]
    );
    let messages = decoder.push(&bytes.as_bytes()[split..]).unwrap();
    assert_eq!(messages.len(), 2);
    decoder.finish().unwrap();

    let mut incomplete = LineDecoder::new();
    incomplete.push(b"command").unwrap();
    assert_eq!(incomplete.finish(), Err(CodecError::TrailingData));
}

#[test]
fn domain_conversion_helpers_are_explicit() {
    let message = command();
    assert_eq!(message.expected_revision, Some(1_842));
    let envelope = message.domain_envelope("converted");
    assert_eq!(envelope.expected_revision, Some(Revision::new(1_842)));
    assert_eq!(envelope.deadline, Some(Deadline::from_millis(500)));
    assert_eq!(cursor().domain_revision(), Revision::new(1_842));
    assert_eq!(identity().domain_state_epoch(), StateEpoch::new(7));
    assert_eq!(input(42).to_domain().get(), NonZeroU128::new(42).unwrap());
    assert_eq!(resume_cursor().domain_revision(), Revision::new(1_842));
    assert_eq!(server_identity().domain_state_epoch(), StateEpoch::new(7));
}

#[test]
fn handshake_choice_requires_an_exact_cursor_identity_and_available_history() {
    let server = server_identity();
    assert_eq!(
        choose_handshake_outcome(&server, 100, 90, None),
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::NoCursor
        }
    );

    let mut cursor = ResumeCursor {
        server: server.clone(),
        revision: 89,
    };
    assert_eq!(
        choose_handshake_outcome(&server, 100, 90, Some(&cursor)),
        HandshakeOutcome::Resume {
            cursor: cursor.clone()
        }
    );
    cursor.revision = 88;
    assert_eq!(
        choose_handshake_outcome(&server, 100, 90, Some(&cursor)),
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::HistoryUnavailable
        }
    );
    cursor.revision = 101;
    assert_eq!(
        choose_handshake_outcome(&server, 100, 90, Some(&cursor)),
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::CursorAhead
        }
    );
    cursor.revision = 100;
    cursor.server.project_id = "replaced".to_owned();
    assert_eq!(
        choose_handshake_outcome(&server, 100, 90, Some(&cursor)),
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::IdentityChanged
        }
    );
}

#[test]
fn runtime_lifecycle_variants_use_the_independent_runtime_sequence() {
    let events = [
        RuntimeLifecycleEvent::Accepted,
        RuntimeLifecycleEvent::Preparing,
        RuntimeLifecycleEvent::Scheduled {
            domains: vec![RuntimeDomainBoundary {
                domain: "video".to_owned(),
                boundary: 900,
            }],
        },
        RuntimeLifecycleEvent::Realized {
            domain: "video".to_owned(),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
        },
        RuntimeLifecycleEvent::Failed {
            error: structured_error(),
            disposition: RuntimeFailureDisposition::RetainedForRetry,
        },
        RuntimeLifecycleEvent::Superseded { by_revision: 1_844 },
    ];
    for (index, event) in events.into_iter().enumerate() {
        let message = WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(),
            revision: 1_843,
            generation: 12,
            sequence: u64::try_from(index + 1).unwrap(),
            event,
        });
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.starts_with("runtime_event\t"));
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }
}

#[test]
fn runtime_scheduled_domain_bounds_are_enforced() {
    let domains = (0..=MAX_LIST_ITEMS)
        .map(|index| RuntimeDomainBoundary {
            domain: format!("domain-{index}"),
            boundary: u64::try_from(index).unwrap(),
        })
        .collect();
    let message = WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: server_identity(),
        revision: 1_843,
        generation: 12,
        sequence: 3,
        event: RuntimeLifecycleEvent::Scheduled { domains },
    });
    assert_eq!(
        encode_line(&message),
        Err(CodecError::TooManyItems("domains"))
    );

    let domains = (0..=MAX_LIST_ITEMS)
        .map(|index| format!("domain-{index}%7E{index}"))
        .collect::<Vec<_>>()
        .join("%2C");
    let current = encode_line(&WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: server_identity(),
        revision: 1_843,
        generation: 12,
        sequence: 3,
        event: RuntimeLifecycleEvent::Scheduled {
            domains: vec![
                RuntimeDomainBoundary {
                    domain: "video".into(),
                    boundary: 900,
                },
                RuntimeDomainBoundary {
                    domain: "audio".into(),
                    boundary: 48_000,
                },
            ],
        },
    }))
    .unwrap();
    let oversized = current.replace("video%7E900%2Caudio%7E48000", &domains);
    assert_eq!(
        decode_line(&oversized),
        Err(CodecError::TooManyItems("domains"))
    );
}

#[test]
fn durable_gaps_are_structural_and_batches_cannot_hide_sequence_gaps() {
    let batch = WireMessage::DurableEventBatch(DurableEventBatch {
        cursor: resume_cursor(),
        events: vec![DurableEvent {
            sequence: 0,
            event_type: "switcher.desired".into(),
            payload: "program=2&preview=1".into(),
        }],
    });
    let malformed_batch = encode_line(&batch)
        .unwrap()
        .replace("events=0%7E", "events=1%7E");
    assert!(matches!(
        decode_line(&malformed_batch),
        Err(CodecError::InvalidField {
            field: "events",
            ..
        })
    ));

    let gap = WireMessage::DurableGap(DurableGap {
        server: server_identity(),
        requested_after_revision: 1_800,
        available_from_revision: 1_820,
        current_revision: 1_845,
    });
    let not_a_gap = encode_line(&gap).unwrap().replace(
        "requested_after_revision=1800",
        "requested_after_revision=1819",
    );
    assert!(matches!(
        decode_line(&not_a_gap),
        Err(CodecError::InvalidField {
            field: "available_from_revision",
            ..
        })
    ));
}

#[test]
fn streaming_and_field_bounds_are_enforced_before_growth() {
    let mut decoder = LineDecoder::new();
    let oversized = vec![b'x'; MAX_LINE_BYTES + 1];
    assert_eq!(decoder.push(&oversized), Err(CodecError::LineTooLong));
    let error = encode_line(&WireMessage::Error(ErrorMessage {
        request_id: Some("01K:test".into()),
        current_revision: Some(1_845),
        error: structured_error(),
    }))
    .unwrap();
    assert_eq!(decoder.push(error.as_bytes()).unwrap().len(), 1);

    let oversized_field = format!(
        "error\trequest_id={}\tcode=e\tmessage=m\tfields=\tretryable=0\n",
        "x".repeat(MAX_FIELD_VALUE_BYTES + 1)
    );
    assert_eq!(
        decode_line(&oversized_field),
        Err(CodecError::FieldValueTooLong)
    );

    let one = "error\tcode=e\tmessage=m\tfields=\tretryable=0\n";
    let too_many = one.repeat(MAX_MESSAGES_PER_PUSH + 1);
    assert_eq!(
        decoder.push(too_many.as_bytes()),
        Err(CodecError::TooManyMessages)
    );

    let oversized_dto = WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: "too_large".to_owned(),
            message: "x".repeat(MAX_FIELD_VALUE_BYTES + 1),
            fields: Vec::new(),
            retryable: false,
        },
    });
    assert_eq!(
        encode_line(&oversized_dto),
        Err(CodecError::FieldValueTooLong)
    );

    let mut fields = String::new();
    for index in 0..=MAX_FIELDS_PER_MESSAGE {
        write!(fields, "\tfield{index}=x").unwrap();
    }
    let too_many_fields = format!("error{fields}\n");
    assert_eq!(
        decode_line(&too_many_fields),
        Err(CodecError::TooManyFields)
    );

    let issues = (0..257).map(|_| "f~c~m").collect::<Vec<_>>().join(",");
    let too_many_items = format!("error\tcode=e\tmessage=m\tfields={issues}\tretryable=0\n");
    assert_eq!(
        decode_line(&too_many_items),
        Err(CodecError::TooManyItems("fields"))
    );
}

#[test]
fn phase_one_records_reject_malformed_and_unknown_required_fields() {
    let heartbeat = WireMessage::Heartbeat(HeartbeatMessage {
        server: server_identity(),
        sequence: 88,
        sent_at_ms: 1_720_000_000_000,
        last_applied: Some(resume_cursor()),
    });
    let missing_project = encode_line(&heartbeat)
        .unwrap()
        .replace("\tproject_id=project-9", "");
    assert_eq!(
        decode_line(&missing_project),
        Err(CodecError::MissingField("project_id"))
    );

    let batch = WireMessage::DurableEventBatch(DurableEventBatch {
        cursor: resume_cursor(),
        events: vec![DurableEvent {
            sequence: 0,
            event_type: "switcher.desired".into(),
            payload: "program=2&preview=1".into(),
        }],
    });
    let malformed_events = encode_line(&batch).unwrap().replace(
        "0%7Eswitcher.desired%7Eprogram%253D2%2526preview%253D1",
        "broken",
    );
    assert!(matches!(
        decode_line(&malformed_events),
        Err(CodecError::InvalidField {
            field: "events",
            ..
        })
    ));

    let report = WireMessage::CapabilityReport(CapabilityReportMessage {
        server: server_identity(),
        revision: 1_845,
        summary: CapabilityReportSummary {
            digest: "sha256:abc".into(),
            total: 4,
            available: 3,
            degraded: 1,
            unavailable: 0,
        },
    });
    let unknown = encode_line(&report)
        .unwrap()
        .replace('\n', "\tfuture_required=1\n");
    assert_eq!(
        decode_line(&unknown),
        Err(CodecError::UnknownField("future_required".to_owned()))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn stream_status_codec_is_lossy_bounded_and_exact() {
    use fm_protocol::{MAX_STREAM_SAMPLES, StreamStatusMessage, StreamStatusSample};

    let message = WireMessage::StreamStatus(StreamStatusMessage {
        server: server_identity(),
        sequence: 12,
        samples: vec![
            StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
                realized: StreamRealizedState::Live,
                connected: true,
                muxed_bytes: 1_048_576,
                enqueued_pairs: 900,
                dropped_pairs: 3,
                failure: None,
                local: None,
                bitrate_kbps: 4_500,
                reconnects: 2,
                uptime_ms: 61_000,
            },
            StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(11).unwrap()),
                realized: StreamRealizedState::WaitingToReconnect,
                connected: false,
                muxed_bytes: 0,
                enqueued_pairs: 0,
                dropped_pairs: 0,
                failure: Some("connect:refused".to_owned()),
                local: None,
                bitrate_kbps: 0,
                reconnects: 5,
                uptime_ms: 0,
            },
        ],
    });
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "stream_status\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\tsequence=12\tsamples=7%3Alive%3A1%3A1048576%3A900%3A3%3A2%3A61000%3A4500%3A%3B11%3Awaiting_to_reconnect%3A0%3A0%3A0%3A0%3A5%3A0%3A0%3Aconnect%253Arefused\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);

    let empty = StreamStatusMessage {
        server: server_identity(),
        sequence: 13,
        samples: Vec::new(),
    };
    assert!(matches!(
        encode_line(&WireMessage::StreamStatus(empty)),
        Err(CodecError::InvalidField {
            field: "samples",
            ..
        })
    ));

    let mut overflowing = StreamStatusMessage {
        server: server_identity(),
        sequence: 14,
        samples: Vec::new(),
    };
    for index in 0..=MAX_STREAM_SAMPLES {
        overflowing.samples.push(StreamStatusSample {
            target: WireStreamTargetId::new(
                NonZeroU128::new(u128::try_from(index).unwrap() + 1).unwrap(),
            ),
            realized: StreamRealizedState::Stopped,
            connected: false,
            muxed_bytes: 0,
            enqueued_pairs: 0,
            dropped_pairs: 0,
            failure: None,
            local: None,
            bitrate_kbps: 0,
            reconnects: 0,
            uptime_ms: 0,
        });
    }
    assert!(matches!(
        encode_line(&WireMessage::StreamStatus(overflowing)),
        Err(CodecError::TooManyItems("samples"))
    ));

    let unordered = StreamStatusMessage {
        server: server_identity(),
        sequence: 15,
        samples: vec![
            StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(9).unwrap()),
                realized: StreamRealizedState::Live,
                connected: true,
                muxed_bytes: 1,
                enqueued_pairs: 1,
                dropped_pairs: 0,
                failure: None,
                local: None,
                bitrate_kbps: 0,
                reconnects: 0,
                uptime_ms: 0,
            },
            StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(9).unwrap()),
                realized: StreamRealizedState::Stopped,
                connected: false,
                muxed_bytes: 0,
                enqueued_pairs: 0,
                dropped_pairs: 0,
                failure: None,
                local: None,
                bitrate_kbps: 0,
                reconnects: 0,
                uptime_ms: 0,
            },
        ],
    };
    assert!(matches!(
        encode_line(&WireMessage::StreamStatus(unordered)),
        Err(CodecError::InvalidField {
            field: "samples",
            ..
        })
    ));
}

#[test]
fn stream_local_status_codec_round_trips_with_and_without_local() {
    use fm_protocol::{StreamLocalStatus, StreamStatusMessage, StreamStatusSample};

    let network = StreamStatusSample {
        target: WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
        realized: StreamRealizedState::Live,
        connected: true,
        muxed_bytes: 1_048_576,
        enqueued_pairs: 900,
        dropped_pairs: 3,
        failure: None,
        local: None,
        bitrate_kbps: 0,
        reconnects: 0,
        uptime_ms: 0,
    };
    let local = StreamStatusSample {
        target: WireStreamTargetId::new(NonZeroU128::new(11).unwrap()),
        realized: StreamRealizedState::Live,
        connected: false,
        muxed_bytes: 0,
        enqueued_pairs: 0,
        dropped_pairs: 0,
        failure: None,
        local: Some(StreamLocalStatus {
            segments: 6,
            target_duration_ms: 6_000,
            free_bytes: 1_073_741_824,
        }),
        bitrate_kbps: 0,
        reconnects: 0,
        uptime_ms: 0,
    };
    // A sample list mixing a network target (no local field) and a local HLS
    // target keeps the exact 2.52 shape for the network sample and appends
    // only the local sample's trailing field.
    let message = WireMessage::StreamStatus(StreamStatusMessage {
        server: server_identity(),
        sequence: 16,
        samples: vec![network.clone(), local.clone()],
    });
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "stream_status\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\tsequence=16\tsamples=7%3Alive%3A1%3A1048576%3A900%3A3%3A0%3A0%3A0%3A%3B11%3Alive%3A0%3A0%3A0%3A0%3A0%3A0%3A0%3A%3Alocal%3D6%3A6000%3A1073741824\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);

    // A local sample with a sanitized failure keeps the failure field between
    // the counters and the local field.
    let with_failure = StreamStatusSample {
        failure: Some("connect:refused".to_owned()),
        local: Some(StreamLocalStatus {
            segments: 0,
            target_duration_ms: 0,
            free_bytes: 0,
        }),
        ..network
    };
    let message = WireMessage::StreamStatus(StreamStatusMessage {
        server: server_identity(),
        sequence: 17,
        samples: vec![with_failure],
    });
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "stream_status\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\tsequence=17\tsamples=7%3Alive%3A1%3A1048576%3A900%3A3%3A0%3A0%3A0%3Aconnect%253Arefused%3Alocal%3D0%3A0%3A0\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);
}

#[test]
fn stream_local_status_codec_enforces_bounds_and_shapes() {
    use fm_protocol::{
        MAX_STREAM_LOCAL_SEGMENTS, MAX_STREAM_LOCAL_TARGET_DURATION_MS, StreamLocalStatus,
        StreamStatusMessage, StreamStatusSample,
    };

    let encode_local = |segments: u32, target_duration_ms: u32, free_bytes: u64| {
        encode_line(&WireMessage::StreamStatus(StreamStatusMessage {
            server: server_identity(),
            sequence: 18,
            samples: vec![StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(9).unwrap()),
                realized: StreamRealizedState::Live,
                connected: true,
                muxed_bytes: 1,
                enqueued_pairs: 2,
                dropped_pairs: 0,
                failure: None,
                local: Some(StreamLocalStatus {
                    segments,
                    target_duration_ms,
                    free_bytes,
                }),
                bitrate_kbps: 0,
                reconnects: 0,
                uptime_ms: 0,
            }],
        }))
    };

    // Every bound: zero and each maximum round-trip; the extremes of `u64`
    // free bytes are both accepted.
    for (segments, target_duration_ms, free_bytes) in [
        (0, 0, 0),
        (
            MAX_STREAM_LOCAL_SEGMENTS,
            MAX_STREAM_LOCAL_TARGET_DURATION_MS,
            u64::MAX,
        ),
    ] {
        let encoded = encode_local(segments, target_duration_ms, free_bytes).unwrap();
        let WireMessage::StreamStatus(decoded) = decode_line(&encoded).unwrap() else {
            panic!("expected a stream_status record");
        };
        assert_eq!(
            decoded.samples[0].local,
            Some(StreamLocalStatus {
                segments,
                target_duration_ms,
                free_bytes,
            })
        );
    }

    // One past each bound is refused by the encoder and by the decoder.
    for (segments, target_duration_ms) in [
        (MAX_STREAM_LOCAL_SEGMENTS + 1, 0),
        (0, MAX_STREAM_LOCAL_TARGET_DURATION_MS + 1),
    ] {
        assert!(matches!(
            encode_local(segments, target_duration_ms, 0),
            Err(CodecError::InvalidField {
                field: "samples",
                ..
            })
        ));
    }
    let maximum = encode_local(
        MAX_STREAM_LOCAL_SEGMENTS,
        MAX_STREAM_LOCAL_TARGET_DURATION_MS,
        u64::MAX,
    )
    .unwrap();
    for (needle, replacement) in [
        ("local%3D4096%3A3600000", "local%3D4097%3A3600000"),
        ("local%3D4096%3A3600000", "local%3D4096%3A3600001"),
    ] {
        let malformed = maximum.replace(needle, replacement);
        assert!(matches!(
            decode_line(&malformed),
            Err(CodecError::InvalidField {
                field: "samples",
                ..
            })
        ));
    }

    // A local field that is not the exact trailing triple is refused: a
    // truncated pair, an unknown key, and an extra field all fail closed.
    for malformed in [
        maximum.replace(
            "%3Alocal%3D4096%3A3600000%3A18446744073709551615",
            "%3Alocal%3D4096%3A3600000",
        ),
        maximum.replace("%3Alocal%3D", "%3Aother%3D"),
        maximum.replace(
            "%3Alocal%3D4096%3A3600000%3A18446744073709551615",
            "%3Alocal%3D4096%3A3600000%3A18446744073709551615%3A1",
        ),
    ] {
        assert!(matches!(
            decode_line(&malformed),
            Err(CodecError::InvalidField {
                field: "samples",
                ..
            })
        ));
    }
}

/// The field bound applies to the escaped bytes on the wire, which is what the
/// decoder measures. A value that fits unescaped but not once escaped must be
/// refused by the encoder rather than sent to a peer that cannot read it.
#[test]
fn encoder_never_emits_a_field_the_decoder_refuses_as_too_long() {
    let message = WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: "too_large".to_owned(),
            // 20000 bytes unescaped, 60000 escaped: under the line bound,
            // over the field bound.
            message: " ".repeat(20_000),
            fields: Vec::new(),
            retryable: false,
        },
    });
    match encode_line(&message) {
        Ok(line) => assert_eq!(decode_line(&line), Ok(message)),
        Err(error) => assert_eq!(error, CodecError::FieldValueTooLong),
    }
}

/// A `stream_status` failure code the decoder refuses (empty or longer than
/// the redacted-detail bound) must be refused by the encoder too: a peer that
/// receives it drops the whole session, and the publisher discards a record it
/// cannot encode.
#[test]
fn stream_status_failure_bounds_are_enforced_by_the_encoder() {
    use fm_protocol::{StreamStatusMessage, StreamStatusSample};

    for failure in [String::new(), "x".repeat(65)] {
        let message = WireMessage::StreamStatus(StreamStatusMessage {
            server: server_identity(),
            sequence: 1,
            samples: vec![StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(3).unwrap()),
                realized: StreamRealizedState::Failed,
                connected: false,
                muxed_bytes: 0,
                enqueued_pairs: 0,
                dropped_pairs: 0,
                failure: Some(failure),
                local: None,
                bitrate_kbps: 0,
                reconnects: 0,
                uptime_ms: 0,
            }],
        });
        match encode_line(&message) {
            Ok(line) => assert_eq!(decode_line(&line), Ok(message)),
            Err(error) => assert!(matches!(
                error,
                CodecError::InvalidField {
                    field: "samples",
                    ..
                }
            )),
        }
    }
}

fn monitor_frame(data: Vec<u8>) -> MonitorFrameMessage {
    MonitorFrameMessage {
        server: server_identity(),
        tap: MonitorTap::Program,
        sequence: 12,
        frame: 340,
        input: input(3),
        origin: MonitorFrameOrigin::Simulated,
        width: 2,
        height: 1,
        part: 0,
        parts: 1,
        data,
    }
}

#[test]
fn protocol_2_21_monitor_frame_codec_is_exact() {
    let qoi = encode_qoi_rgb(2, 1, &[0, 0, 0, 255, 0, 0], 6, 3).unwrap();
    let message = WireMessage::MonitorFrame(monitor_frame(qoi.clone()));
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "monitor_frame\tengine_id=engine-a\tproject_id=project-9\tstate_epoch=7\tlog_id=log-a\ttap=program\tsequence=12\tframe=340\tinput=3\torigin=simulated\twidth=2\theight=1\tpart=0\tparts=1\tdata=cW9pZgAAAAIAAAABAwDAWgAAAAAAAAAB\n"
    );
    let decoded = decode_line(&encoded).unwrap();
    assert_eq!(decoded, message);
    let WireMessage::MonitorFrame(decoded) = decoded else {
        unreachable!()
    };
    assert_eq!(
        decode_qoi_rgb(decoded.width, decoded.height, &decoded.data).unwrap(),
        [0, 0, 0, 255, 255, 0, 0, 255]
    );

    let preview = encoded
        .replace("tap=program", "tap=preview")
        .replace("origin=simulated", "origin=native");
    let WireMessage::MonitorFrame(preview) = decode_line(&preview).unwrap() else {
        panic!("expected a monitor_frame record");
    };
    assert_eq!(preview.tap, MonitorTap::Preview);
    assert_eq!(preview.origin, MonitorFrameOrigin::Native);
}

#[test]
fn monitor_frame_decoder_rejects_out_of_contract_records() {
    let encoded = encode_line(&WireMessage::MonitorFrame(monitor_frame(vec![1, 2, 3, 4]))).unwrap();
    assert!(encoded.contains("\tdata=AQIDBA\n"));
    for (from, to, field) in [
        ("tap=program", "tap=multiview", "tap"),
        ("origin=simulated", "origin=gpu", "origin"),
        ("input=3", "input=0", "input"),
        ("width=2", "width=0", "width"),
        ("width=2", "width=641", "width"),
        ("height=1", "height=0", "height"),
        ("height=1", "height=361", "height"),
        ("width=2", "width=70000", "width"),
        ("parts=1", "parts=0", "parts"),
        ("parts=1", "parts=33", "parts"),
        ("part=0", "part=1", "part"),
        ("data=AQIDBA", "data=", "data"),
        ("data=AQIDBA", "data=AQIDBA%3D%3D", "data"),
        ("data=AQIDBA", "data=AQIDBB", "data"),
        ("data=AQIDBA", "data=AQID%2BA", "data"),
        ("data=AQIDBA", "data=AQID%2FA", "data"),
        ("data=AQIDBA", "data=AQIDB", "data"),
        ("data=AQIDBA", "data=AQ%20IDBA", "data"),
    ] {
        let line = encoded.replace(from, to);
        assert_ne!(line, encoded, "{from} -> {to}");
        assert!(
            matches!(
                decode_line(&line),
                Err(CodecError::InvalidField { field: rejected, .. }) if rejected == field
            ),
            "{to}: {:?}",
            decode_line(&line)
        );
    }
    assert!(matches!(
        decode_line(&encoded.replace("\tpart=0", "\tpart=0\textra=1")),
        Err(CodecError::UnknownField(field)) if field == "extra"
    ));
    assert!(matches!(
        decode_line(&encoded.replace("\tframe=340", "")),
        Err(CodecError::MissingField("frame"))
    ));

    let oversized = "A".repeat((MAX_MONITOR_FRAME_PART_BYTES * 4).div_ceil(3) + 4);
    assert!(matches!(
        decode_line(&encoded.replace("data=AQIDBA", &format!("data={oversized}"))),
        Err(CodecError::InvalidField { field: "data", .. })
    ));
}

#[test]
fn monitor_frame_encoder_enforces_bounds() {
    type Mutation = fn(&mut MonitorFrameMessage);
    let full = monitor_frame(vec![7; MAX_MONITOR_FRAME_PART_BYTES]);
    let line = encode_line(&WireMessage::MonitorFrame(full.clone())).unwrap();
    assert!(line.len() <= MAX_LINE_BYTES);
    assert!(!line.contains('%'), "base64url data must never be escaped");
    assert_eq!(decode_line(&line).unwrap(), WireMessage::MonitorFrame(full));

    let cases: [(&str, Mutation); 8] = [
        ("data", |message| message.data.clear()),
        ("data", |message| {
            message.data = vec![0; MAX_MONITOR_FRAME_PART_BYTES + 1];
        }),
        ("width", |message| message.width = 0),
        ("width", |message| {
            message.width = MAX_MONITOR_FRAME_WIDTH + 1;
        }),
        ("height", |message| {
            message.height = MAX_MONITOR_FRAME_HEIGHT + 1;
        }),
        ("parts", |message| message.parts = 0),
        ("parts", |message| {
            message.parts = MAX_MONITOR_FRAME_PARTS + 1;
        }),
        ("part", |message| message.part = 1),
    ];
    for (field, mutate) in cases {
        let mut message = monitor_frame(vec![1]);
        mutate(&mut message);
        assert!(
            matches!(
                encode_line(&WireMessage::MonitorFrame(message)),
                Err(CodecError::InvalidField { field: rejected, .. }) if rejected == field
            ),
            "{field}"
        );
    }
}

#[test]
fn maximum_monitor_frame_round_trips_through_the_wire() {
    let width = MAX_MONITOR_FRAME_WIDTH;
    let height = MAX_MONITOR_FRAME_HEIGHT;
    let mut state = 0x9e37_79b9_u32;
    let rgb: Vec<u8> = (0..usize::from(width) * usize::from(height) * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[1]
        })
        .collect();
    let qoi = encode_qoi_rgb(width, height, &rgb, usize::from(width) * 3, 3).unwrap();
    let parts = monitor_frame_parts(
        &server_identity(),
        MonitorTap::Preview,
        3,
        99,
        input(5),
        MonitorFrameOrigin::Simulated,
        width,
        height,
        &qoi,
    );
    assert!(parts.len() > 1);
    assert!(parts.len() <= usize::from(MAX_MONITOR_FRAME_PARTS));

    let mut stream = String::new();
    for part in &parts {
        stream.push_str(&encode_line(&WireMessage::MonitorFrame(part.clone())).unwrap());
    }
    let mut decoder = LineDecoder::new();
    let mut assembler = MonitorFrameAssembler::new();
    let mut images = Vec::new();
    for chunk in stream.as_bytes().chunks(7_919) {
        for message in decoder.push(chunk).unwrap() {
            let WireMessage::MonitorFrame(part) = message else {
                panic!("expected monitor_frame");
            };
            images.extend(assembler.push(&part).unwrap());
        }
    }
    decoder.finish().unwrap();
    assert_eq!(images.len(), 1);
    let image = &images[0];
    assert_eq!((image.width, image.height), (width, height));
    assert_eq!(
        (image.tap, image.sequence, image.frame),
        (MonitorTap::Preview, 3, 99)
    );
    assert_eq!(image.input, input(5));
    assert_eq!(
        image.rgba.len(),
        usize::from(width) * usize::from(height) * 4
    );
    assert!(
        image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgb.as_chunks::<3>().0)
            .all(|(rgba, rgb)| rgba[..3] == *rgb && rgba[3] == 255)
    );
}

#[test]
fn protocol_identifier_must_be_canonical_decimal() {
    let fixture = encode_line(&WireMessage::Command(command())).unwrap();
    let current = format!("protocol={CURRENT_PROTOCOL_VERSION}");
    assert!(fixture.contains(&current));
    for alias in [
        format!(
            "protocol=%2B{}.{}",
            CURRENT_PROTOCOL_VERSION.major, CURRENT_PROTOCOL_VERSION.minor
        ),
        format!(
            "protocol=0{}.{}",
            CURRENT_PROTOCOL_VERSION.major, CURRENT_PROTOCOL_VERSION.minor
        ),
        format!(
            "protocol={}.0{}",
            CURRENT_PROTOCOL_VERSION.major, CURRENT_PROTOCOL_VERSION.minor
        ),
        format!(
            "protocol={}.%2B{}",
            CURRENT_PROTOCOL_VERSION.major, CURRENT_PROTOCOL_VERSION.minor
        ),
    ] {
        assert!(
            matches!(
                decode_line(&fixture.replace(&current, &alias)),
                Err(CodecError::InvalidField {
                    field: "protocol",
                    ..
                })
            ),
            "{alias} must not alias the current identifier"
        );
    }
    let zero = encode_line(&WireMessage::Command(CommandMessage {
        protocol: ProtocolVersion::new(0, 0),
        ..command()
    }))
    .unwrap();
    assert!(decode_line(&zero).is_ok());
}

#[test]
fn encoder_refuses_overlay_records_the_decoder_rejects() {
    let channel = WireOverlayChannelId::new(1).unwrap();
    for duration_frames in [0, 3_601] {
        let message = WireMessage::Command(CommandMessage {
            payload: CommandPayload::ConfigureOverlayTransition {
                channel,
                transition: OverlayTransitionKind::Fade,
                duration_frames,
            },
            ..command()
        });
        assert!(
            matches!(
                encode_line(&message),
                Err(CodecError::InvalidField {
                    field: "duration_frames",
                    ..
                })
            ),
            "duration {duration_frames}"
        );
    }

    let WireMessage::Snapshot(valid) = snapshot(input_statuses(&[(1, "camera"), (2, "slides")]))
    else {
        unreachable!()
    };
    let mut zero_duration = valid.clone();
    zero_duration.desired_overlays[3].duration_frames = 0;
    assert!(matches!(
        encode_line(&WireMessage::Snapshot(zero_duration)),
        Err(CodecError::InvalidField {
            field: "desired_overlays",
            ..
        })
    ));
    let mut duplicate_channel = valid.clone();
    duplicate_channel.realized_overlays[7].channel = channel;
    assert!(matches!(
        encode_line(&WireMessage::Snapshot(duplicate_channel)),
        Err(CodecError::InvalidField {
            field: "realized_overlays",
            ..
        })
    ));
    let mut no_strips = valid;
    no_strips.input_audio_strips.clear();
    assert!(matches!(
        encode_line(&WireMessage::Snapshot(no_strips)),
        Err(CodecError::InvalidField {
            field: "input_audio_strips",
            ..
        })
    ));
}

#[test]
fn handshake_permissions_reject_empty_entries_in_both_directions() {
    let response = |permissions: Vec<String>| {
        WireMessage::HandshakeResponse(fm_protocol::HandshakeResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            granted_role: fm_protocol::Role::Operator,
            permissions,
            capabilities: CapabilityReportSummary {
                digest: "sha256:abc".to_owned(),
                total: 1,
                available: 1,
                degraded: 0,
                unavailable: 0,
            },
            server: server_identity(),
            current_revision: 9,
            outcome: HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        })
    };
    let valid = response(vec!["switcher.cut".to_owned(), "a,b".to_owned()]);
    let encoded = encode_line(&valid).unwrap();
    assert_eq!(decode_line(&encoded).unwrap(), valid);
    let none = response(Vec::new());
    assert_eq!(decode_line(&encode_line(&none).unwrap()).unwrap(), none);

    // `[""]` used to encode as the empty list and silently decode as `[]`.
    for permissions in [vec![String::new()], vec!["a".to_owned(), String::new()]] {
        assert!(matches!(
            encode_line(&response(permissions)),
            Err(CodecError::InvalidField {
                field: "permissions",
                ..
            })
        ));
    }
    let list = "permissions=switcher.cut%2Ca%252Cb";
    assert!(encoded.contains(list));
    for invalid in [
        "permissions=switcher.cut%2C",
        "permissions=%2Cswitcher.cut",
        "permissions=switcher.cut%2C%2Ca",
    ] {
        assert!(matches!(
            decode_line(&encoded.replace(list, invalid)),
            Err(CodecError::InvalidField {
                field: "permissions",
                ..
            })
        ));
    }
}

fn edit_command(payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        expected_revision: None,
        deadline_ms: None,
        payload,
        ..command()
    }
}

fn history(undo: (u16, Option<u64>), redo: (u16, Option<u64>)) -> EditHistoryStatus {
    EditHistoryStatus {
        undo_depth: undo.0,
        next_undo: undo.1,
        redo_depth: redo.0,
        next_redo: redo.1,
    }
}

fn edit_lock_event(scope: EditLockScope, edit_history: EditHistoryStatus) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: cursor(),
        payload: EventPayload::EditLockChanged {
            scope,
            locked: true,
        },
        edit_history,
    })
}

fn locked_snapshot(edit_locks: EditLockStatus, edit_history: EditHistoryStatus) -> WireMessage {
    let WireMessage::Snapshot(mut snapshot) =
        snapshot(input_statuses(&[(1, "Cam 1"), (2, "Cam 2")]))
    else {
        unreachable!("the fixture is a snapshot")
    };
    snapshot.edit_locks = edit_locks;
    snapshot.edit_history = edit_history;
    WireMessage::Snapshot(snapshot)
}

#[test]
fn edit_history_and_padlock_commands_are_exact() {
    let cases = [
        (
            CommandPayload::Undo {
                expected_edit: None,
            },
            "\tpayload=undo\n",
        ),
        (
            CommandPayload::Undo {
                expected_edit: Some(1_840),
            },
            "\tpayload=undo\texpected_edit=1840\n",
        ),
        (
            CommandPayload::Redo {
                expected_edit: Some(7),
            },
            "\tpayload=redo\texpected_edit=7\n",
        ),
        (
            CommandPayload::Redo {
                expected_edit: None,
            },
            "\tpayload=redo\n",
        ),
        (
            CommandPayload::SetEditLock {
                scope: EditLockScope::Project,
                locked: true,
            },
            "\tpayload=edit_lock\tscope=project\tlocked=1\n",
        ),
        (
            CommandPayload::SetEditLock {
                scope: EditLockScope::Input(input(42)),
                locked: false,
            },
            "\tpayload=edit_lock\tscope=input\tinput=42\tlocked=0\n",
        ),
    ];
    for (payload, tail) in cases {
        let message = WireMessage::Command(edit_command(payload));
        let encoded = encode_line(&message).unwrap();
        assert_eq!(
            encoded,
            format!(
                "command\tprotocol=2.57\tid=01K%3Atest\tidempotency_key=operator-7%3A01K{tail}"
            )
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }
}

#[test]
fn edit_commands_reject_out_of_contract_records() {
    let prefix = "command\tprotocol=2.57\tid=a\tidempotency_key=k";
    for (tail, expected) in [
        (
            "\tpayload=undo\texpected_edit=0",
            CodecError::InvalidField {
                field: "expected_edit",
                value: "0".to_owned(),
            },
        ),
        (
            "\tpayload=redo\texpected_edit=%2B5",
            CodecError::InvalidField {
                field: "expected_edit",
                value: "+5".to_owned(),
            },
        ),
        (
            "\tpayload=undo\texpected_edit=05",
            CodecError::InvalidField {
                field: "expected_edit",
                value: "05".to_owned(),
            },
        ),
        (
            "\tpayload=undo\tscope=project",
            CodecError::UnknownField("scope".to_owned()),
        ),
        (
            "\tpayload=edit_lock\tlocked=1",
            CodecError::MissingField("scope"),
        ),
        (
            "\tpayload=edit_lock\tscope=project",
            CodecError::MissingField("locked"),
        ),
        (
            "\tpayload=edit_lock\tscope=input\tlocked=1",
            CodecError::MissingField("input"),
        ),
        (
            "\tpayload=edit_lock\tscope=project\tinput=3\tlocked=1",
            CodecError::UnknownField("input".to_owned()),
        ),
        (
            "\tpayload=edit_lock\tscope=show\tlocked=1",
            CodecError::InvalidField {
                field: "scope",
                value: "show".to_owned(),
            },
        ),
        (
            "\tpayload=edit_lock\tscope=input\tinput=0\tlocked=1",
            CodecError::InvalidField {
                field: "input",
                value: "0".to_owned(),
            },
        ),
        (
            "\tpayload=edit_lock\tscope=input\tinput=03\tlocked=1",
            CodecError::InvalidField {
                field: "input",
                value: "03".to_owned(),
            },
        ),
        (
            "\tpayload=edit_lock\tscope=project\tlocked=2",
            CodecError::InvalidField {
                field: "locked",
                value: "2".to_owned(),
            },
        ),
    ] {
        assert_eq!(
            decode_line(&format!("{prefix}{tail}\n")),
            Err(expected),
            "{tail}"
        );
    }
    // The encoder refuses what the decoder refuses.
    for payload in [
        CommandPayload::Undo {
            expected_edit: Some(0),
        },
        CommandPayload::Redo {
            expected_edit: Some(0),
        },
    ] {
        assert_eq!(
            encode_line(&WireMessage::Command(edit_command(payload))),
            Err(CodecError::InvalidField {
                field: "expected_edit",
                value: "0".to_owned(),
            })
        );
    }
}

#[test]
fn edit_lock_changed_event_carries_the_post_revision_history() {
    let message = edit_lock_event(
        EditLockScope::Input(input(5)),
        history((3, Some(1_840)), (1, Some(1_841))),
    );
    let encoded = encode_line(&message).unwrap();
    assert_eq!(
        encoded,
        "event\tengine_id=engine-a\tstate_epoch=7\tlog_id=log-a\trevision=1842\t\
         event=edit_lock_changed\tscope=input\tinput=5\tlocked=1\t\
         undo_depth=3\tundo_edit=1840\tredo_depth=1\tredo_edit=1841\n"
    );
    assert_eq!(decode_line(&encoded).unwrap(), message);

    let project = edit_lock_event(EditLockScope::Project, EditHistoryStatus::EMPTY);
    let encoded = encode_line(&project).unwrap();
    assert!(encoded.ends_with(
        "\tevent=edit_lock_changed\tscope=project\tlocked=1\tundo_depth=0\tredo_depth=0\n"
    ));
    assert_eq!(decode_line(&encoded).unwrap(), project);

    // Every event kind carries the summary; it is never optional.
    let without = encoded.replace("\tundo_depth=0", "");
    assert_eq!(
        decode_line(&without),
        Err(CodecError::MissingField("undo_depth"))
    );
    let without = encoded.replace("\tredo_depth=0", "");
    assert_eq!(
        decode_line(&without),
        Err(CodecError::MissingField("redo_depth"))
    );
}

#[test]
fn edit_history_summary_is_strict_in_both_directions() {
    let invalid = [
        (
            history((MAX_EDIT_HISTORY_DEPTH + 1, Some(1)), (0, None)),
            "undo_edit",
        ),
        (
            history((0, None), (MAX_EDIT_HISTORY_DEPTH + 1, Some(1))),
            "redo_edit",
        ),
        (history((1, None), (0, None)), "undo_edit"),
        (history((0, Some(4)), (0, None)), "undo_edit"),
        (history((0, None), (2, None)), "redo_edit"),
        (history((1, Some(0)), (0, None)), "undo_edit"),
        (history((1, Some(1_843)), (0, None)), "undo_edit"),
        (history((0, None), (1, Some(1_843))), "redo_edit"),
        (history((1, Some(9)), (1, Some(9))), "redo_edit"),
    ];
    for (status, field) in invalid {
        let message = edit_lock_event(EditLockScope::Project, status);
        assert!(
            matches!(
                encode_line(&message),
                Err(CodecError::InvalidField { field: refused, .. }) if refused == field
            ),
            "encoder accepted {status:?}"
        );
    }
    let valid = edit_lock_event(
        EditLockScope::Project,
        history((MAX_EDIT_HISTORY_DEPTH, Some(1_842)), (1, Some(1))),
    );
    let encoded = encode_line(&valid).unwrap();
    assert_eq!(decode_line(&encoded).unwrap(), valid);

    for (from, to, field) in [
        ("undo_depth=100", "undo_depth=101", "undo_edit"),
        ("undo_depth=100", "undo_depth=0", "undo_edit"),
        ("undo_edit=1842", "undo_edit=1843", "undo_edit"),
        ("redo_edit=1", "redo_edit=0", "redo_edit"),
        ("redo_edit=1", "redo_edit=1842", "redo_edit"),
        ("undo_depth=100", "undo_depth=0100", "undo_depth"),
        ("undo_depth=100", "undo_depth=%2B100", "undo_depth"),
        ("undo_depth=100", "undo_depth=65636", "undo_depth"),
        ("redo_edit=1", "redo_edit=01", "redo_edit"),
    ] {
        let tampered = encoded.replace(from, to);
        assert!(
            matches!(
                decode_line(&tampered),
                Err(CodecError::InvalidField { field: refused, .. }) if refused == field
            ),
            "decoder accepted {to}: {:?}",
            decode_line(&tampered)
        );
    }
    let missing_edit = encoded.replace("\tredo_edit=1", "");
    assert!(matches!(
        decode_line(&missing_edit),
        Err(CodecError::InvalidField {
            field: "redo_edit",
            ..
        })
    ));
}

#[test]
fn snapshot_edit_locks_and_history_round_trip_and_are_strict() {
    let message = locked_snapshot(
        EditLockStatus {
            project: true,
            inputs: vec![input(1), input(2)],
        },
        history((2, Some(9)), (0, None)),
    );
    let encoded = encode_line(&message).unwrap();
    assert!(encoded.ends_with(
        "\tproject_locked=1\tlocked_inputs=1%2C2\tundo_depth=2\tundo_edit=9\tredo_depth=0\ttitles=\tscenes=\tlayout_scenes=\tframe_rate=30000%2F1001\tplaylist=0\tgrade_states=0\tanimation_preset_states=0\ttransition_favorites=fade%3A30%3Bwipe%3A30%3Bfly%3A30%3Bcube%3A30\tmedia_states=0\tcue_list_states=0\tinput_categories=\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CGroup%25202%7E0%7E0%7E0%7E-%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=sip\taudio_monitor=%7E0%7E0\tinput_audio_labels=\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n"
    ));
    assert_eq!(decode_line(&encoded).unwrap(), message);

    let unlocked = locked_snapshot(EditLockStatus::default(), EditHistoryStatus::EMPTY);
    let encoded_unlocked = encode_line(&unlocked).unwrap();
    assert!(
        encoded_unlocked
            .ends_with("\tproject_locked=0\tlocked_inputs=\tundo_depth=0\tredo_depth=0\ttitles=\tscenes=\tlayout_scenes=\tframe_rate=30000%2F1001\tplaylist=0\tgrade_states=0\tanimation_preset_states=0\ttransition_favorites=fade%3A30%3Bwipe%3A30%3Bfly%3A30%3Bcube%3A30\tmedia_states=0\tcue_list_states=0\tinput_categories=\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CGroup%25202%7E0%7E0%7E0%7E-%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=sip\taudio_monitor=%7E0%7E0\tinput_audio_labels=\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n")
    );
    assert_eq!(decode_line(&encoded_unlocked).unwrap(), unlocked);

    // The encoder refuses lock lists the decoder refuses.
    for inputs in [
        vec![input(2), input(1)],
        vec![input(1), input(1)],
        vec![input(3)],
    ] {
        let message = locked_snapshot(
            EditLockStatus {
                project: false,
                inputs,
            },
            EditHistoryStatus::EMPTY,
        );
        assert!(matches!(
            encode_line(&message),
            Err(CodecError::InvalidField {
                field: "locked_inputs",
                ..
            })
        ));
    }
    for tampered in [
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=2%2C1"),
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=1%2C1"),
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=3"),
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=01"),
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=1%2C"),
        encoded.replace("locked_inputs=1%2C2", "locked_inputs=0"),
    ] {
        assert!(
            matches!(
                decode_line(&tampered),
                Err(CodecError::InvalidField {
                    field: "locked_inputs",
                    ..
                })
            ),
            "{tampered}"
        );
    }
    assert_eq!(
        decode_line(&encoded.replace("project_locked=1", "project_locked=yes")),
        Err(CodecError::InvalidField {
            field: "project_locked",
            value: "yes".to_owned(),
        })
    );
    assert_eq!(
        decode_line(&encoded.replace("\tproject_locked=1", "")),
        Err(CodecError::MissingField("project_locked"))
    );
    assert_eq!(
        decode_line(&encoded.replace("\tundo_edit=9", "\tundo_edit=10")),
        Err(CodecError::InvalidField {
            field: "undo_edit",
            value: "2:10".to_owned(),
        })
    );
}

fn catalogue_event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: cursor(),
        payload,
        edit_history: EditHistoryStatus::EMPTY,
    })
}

#[test]
fn protocol_2_23_input_catalogue_commands_are_exact() {
    let cases = [
        (
            CommandPayload::AddInput {
                input: input(42),
                name: "Red Wall".to_owned(),
                source: LiveInputSource::Solid {
                    red: 255,
                    green: 0,
                    blue: 16,
                    alpha: 128,
                },
            },
            "\tpayload=input_add\tinput=42\tname=Red%20Wall\tkind=solid\tcolor=ff001080\n",
        ),
        (
            CommandPayload::AddInput {
                input: input(7),
                name: "Bars".to_owned(),
                source: LiveInputSource::Bars,
            },
            "\tpayload=input_add\tinput=7\tname=Bars\tkind=bars\n",
        ),
        (
            CommandPayload::RemoveInput { input: input(42) },
            "\tpayload=input_remove\tinput=42\n",
        ),
    ];
    for (payload, tail) in cases {
        let message = WireMessage::Command(edit_command(payload));
        let encoded = encode_line(&message).unwrap();
        assert_eq!(
            encoded,
            format!(
                "command\tprotocol=2.57\tid=01K%3Atest\tidempotency_key=operator-7%3A01K{tail}"
            )
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn input_catalogue_commands_reject_out_of_contract_records() {
    let prefix = "command\tprotocol=2.57\tid=a\tidempotency_key=k";
    let invalid = |field: &'static str, value: &str| CodecError::InvalidField {
        field,
        value: value.to_owned(),
    };
    let long_name = "n".repeat(fm_types::MAX_INPUT_NAME_BYTES + 1);
    for (tail, expected) in [
        (
            "\tpayload=input_add\tinput=4\tname=A".to_owned(),
            CodecError::MissingField("kind"),
        ),
        (
            "\tpayload=input_add\tinput=4\tkind=bars".to_owned(),
            CodecError::MissingField("name"),
        ),
        (
            "\tpayload=input_add\tname=A\tkind=bars".to_owned(),
            CodecError::MissingField("input"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=media".to_owned(),
            invalid("kind", "media"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=solid".to_owned(),
            CodecError::MissingField("color"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=bars\tcolor=000000ff".to_owned(),
            CodecError::UnknownField("color".to_owned()),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=solid\tcolor=FF0000FF".to_owned(),
            invalid("color", "FF0000FF"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=solid\tcolor=ff0000".to_owned(),
            invalid("color", "ff0000"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=solid\tcolor=ff0000fg".to_owned(),
            invalid("color", "ff0000fg"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=A\tkind=solid\tcolor=%2Bf0000ff".to_owned(),
            invalid("color", "+f0000ff"),
        ),
        (
            "\tpayload=input_add\tinput=0\tname=A\tkind=bars".to_owned(),
            invalid("input", "0"),
        ),
        (
            "\tpayload=input_add\tinput=04\tname=A\tkind=bars".to_owned(),
            invalid("input", "04"),
        ),
        (
            "\tpayload=input_add\tinput=4\tname=%20%20\tkind=bars".to_owned(),
            invalid("name", "  "),
        ),
        (
            format!("\tpayload=input_add\tinput=4\tname={long_name}\tkind=bars"),
            invalid("name", &long_name),
        ),
        (
            "\tpayload=input_remove".to_owned(),
            CodecError::MissingField("input"),
        ),
        (
            "\tpayload=input_remove\tinput=%2B4".to_owned(),
            invalid("input", "+4"),
        ),
        (
            "\tpayload=input_remove\tinput=4\tname=A".to_owned(),
            CodecError::UnknownField("name".to_owned()),
        ),
    ] {
        assert_eq!(
            decode_line(&format!("{prefix}{tail}\n")),
            Err(expected),
            "{tail}"
        );
    }
    // The encoder refuses what the decoder refuses.
    for name in ["", " \t", long_name.as_str()] {
        assert_eq!(
            encode_line(&WireMessage::Command(edit_command(
                CommandPayload::AddInput {
                    input: input(4),
                    name: name.to_owned(),
                    source: LiveInputSource::Bars,
                }
            ))),
            Err(invalid("name", name))
        );
        assert_eq!(
            encode_line(&catalogue_event(EventPayload::InputAdded {
                input: input(4),
                name: name.to_owned(),
                network_stream: None,
                system_audio: None,
                capture: None,
            })),
            Err(invalid("name", name))
        );
    }
}

#[test]
fn input_catalogue_events_are_exact_and_strict() {
    let prefix = "event\tengine_id=engine-a\tstate_epoch=7\tlog_id=log-a\trevision=1842";
    let history = "\tundo_depth=0\tredo_depth=0\n";
    let added = catalogue_event(EventPayload::InputAdded {
        input: input(9),
        name: "Slate 9".to_owned(),
        network_stream: None,
        system_audio: None,
        capture: None,
    });
    let encoded = encode_line(&added).unwrap();
    assert_eq!(
        encoded,
        format!("{prefix}\tevent=input_added\tinput=9\tname=Slate%209{history}")
    );
    assert_eq!(decode_line(&encoded).unwrap(), added);

    let removed = catalogue_event(EventPayload::InputRemoved { input: input(9) });
    let encoded = encode_line(&removed).unwrap();
    assert_eq!(
        encoded,
        format!("{prefix}\tevent=input_removed\tinput=9{history}")
    );
    assert_eq!(decode_line(&encoded).unwrap(), removed);

    for (body, expected) in [
        (
            "\tevent=input_added\tinput=9",
            CodecError::MissingField("name"),
        ),
        (
            "\tevent=input_added\tinput=9\tname=",
            CodecError::InvalidField {
                field: "name",
                value: String::new(),
            },
        ),
        (
            "\tevent=input_added\tinput=09\tname=A",
            CodecError::InvalidField {
                field: "input",
                value: "09".to_owned(),
            },
        ),
        (
            "\tevent=input_removed\tinput=9\tname=A",
            CodecError::UnknownField("name".to_owned()),
        ),
        ("\tevent=input_removed", CodecError::MissingField("input")),
        (
            "\tevent=input_removed\tinput=0",
            CodecError::InvalidField {
                field: "input",
                value: "0".to_owned(),
            },
        ),
    ] {
        assert_eq!(
            decode_line(&format!("{prefix}{body}{history}")),
            Err(expected),
            "{body}"
        );
    }
}

/// Protocol 2.24 titles: field save, take, off-air, `title_changed`, snapshot
/// summaries, and `title_state`.
mod titles {
    use super::*;
    use fm_protocol::{
        MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELD_NAMES_BYTES, MAX_TITLE_FIELDS,
        MAX_TITLE_NAME_BYTES, MAX_TITLE_TEXT_CHARS, TitleAirRequirement, TitleFieldStatus,
        TitleFieldUpdate, TitleFieldValue, TitlePhase, TitleStateMessage, TitleSummary,
        WireTitleFieldId,
    };

    const PREFIX: &str = "command\tprotocol=2.57\tid=a\tidempotency_key=k";

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn update(id: u128, value: TitleFieldValue) -> TitleFieldUpdate {
        TitleFieldUpdate {
            field: field(id),
            value,
        }
    }

    fn channel(number: u8) -> WireOverlayChannelId {
        WireOverlayChannelId::new(number).unwrap()
    }

    /// One field of every type, with text that needs escaping.
    fn sample_fields() -> Vec<TitleFieldUpdate> {
        vec![
            update(1, TitleFieldValue::Text("Ana, M: 50%".to_owned())),
            update(2, TitleFieldValue::Number(-7)),
            update(
                3,
                TitleFieldValue::Color {
                    red: 255,
                    green: 0,
                    blue: 16,
                    alpha: 128,
                },
            ),
            update(
                4,
                TitleFieldValue::Image {
                    asset_uri: "asset://logo.png".to_owned(),
                },
            ),
        ]
    }

    const SAMPLE_FIELDS_WIRE: &str = "1%3At%3A11%3AAna%2C%20M%3A%2050%25%2C2%3An%3A-7%2C3%3Ac%3Aff001080%2C4%3Ai%3A16%3Aasset%3A%2F%2Flogo.png";

    fn title_command(payload: CommandPayload) -> WireMessage {
        WireMessage::Command(edit_command(payload))
    }

    #[test]
    fn protocol_2_24_title_commands_are_exact() {
        assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 57));
        let cases = [
            (
                CommandPayload::SetTitleFields {
                    input: input(42),
                    base_revision: 3,
                    require: TitleAirRequirement::OffAir,
                    fields: sample_fields(),
                },
                format!(
                    "\tpayload=title_fields\tinput=42\tbase_revision=3\trequire=off_air\tfields={SAMPLE_FIELDS_WIRE}\n"
                ),
            ),
            (
                CommandPayload::SetTitleFields {
                    input: input(42),
                    base_revision: 0,
                    require: TitleAirRequirement::OnAir,
                    fields: vec![update(9, TitleFieldValue::Text(String::new()))],
                },
                "\tpayload=title_fields\tinput=42\tbase_revision=0\trequire=on_air\tfields=9%3At%3A0%3A\n"
                    .to_owned(),
            ),
            (
                CommandPayload::TakeTitle {
                    input: input(42),
                    channel: channel(2),
                    base_revision: 5,
                    fields: Vec::new(),
                },
                "\tpayload=title_take\tinput=42\tchannel=2\tbase_revision=5\tfields=\n".to_owned(),
            ),
            (
                CommandPayload::TakeTitle {
                    input: input(42),
                    channel: channel(8),
                    base_revision: 5,
                    fields: sample_fields(),
                },
                format!(
                    "\tpayload=title_take\tinput=42\tchannel=8\tbase_revision=5\tfields={SAMPLE_FIELDS_WIRE}\n"
                ),
            ),
            (
                CommandPayload::TitleOffAir { input: input(42) },
                "\tpayload=title_off_air\tinput=42\n".to_owned(),
            ),
        ];
        for (payload, tail) in cases {
            let message = title_command(payload);
            let encoded = encode_line(&message).unwrap();
            assert_eq!(
                encoded,
                format!(
                    "command\tprotocol=2.57\tid=01K%3Atest\tidempotency_key=operator-7%3A01K{tail}"
                )
            );
            assert_eq!(decode_line(&encoded).unwrap(), message);
        }
    }

    #[test]
    fn title_values_round_trip_at_their_extremes() {
        let fields = vec![
            update(1, TitleFieldValue::Number(i64::MIN)),
            update(2, TitleFieldValue::Number(i64::MAX)),
            update(3, TitleFieldValue::Number(0)),
            update(
                4,
                TitleFieldValue::Text("é,\u{1F600}:t:3:x,%2C\t\n".to_owned()),
            ),
            update(
                u128::MAX,
                TitleFieldValue::Image {
                    asset_uri: " asset://a,b:c ".to_owned(),
                },
            ),
        ];
        let message = title_command(CommandPayload::SetTitleFields {
            input: input(u128::MAX),
            base_revision: u64::MAX,
            require: TitleAirRequirement::OnAir,
            fields,
        });
        let encoded = encode_line(&message).unwrap();
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn title_commands_reject_out_of_contract_records() {
        let invalid = |field: &'static str, value: &str| CodecError::InvalidField {
            field,
            value: value.to_owned(),
        };
        let fields = |value: &str| {
            format!(
                "\tpayload=title_fields\tinput=4\tbase_revision=0\trequire=off_air\tfields={value}"
            )
        };
        for (tail, expected) in [
            (
                "\tpayload=title_fields\tinput=4\tbase_revision=0\tfields=".to_owned(),
                CodecError::MissingField("require"),
            ),
            (
                "\tpayload=title_fields\tinput=4\trequire=on_air\tfields=".to_owned(),
                CodecError::MissingField("base_revision"),
            ),
            (
                "\tpayload=title_fields\tinput=4\tbase_revision=0\trequire=on_air".to_owned(),
                CodecError::MissingField("fields"),
            ),
            (
                "\tpayload=title_fields\tinput=4\tbase_revision=0\trequire=live\tfields="
                    .to_owned(),
                invalid("require", "live"),
            ),
            (
                "\tpayload=title_fields\tinput=4\tbase_revision=01\trequire=on_air\tfields="
                    .to_owned(),
                invalid("base_revision", "01"),
            ),
            (
                "\tpayload=title_fields\tinput=04\tbase_revision=0\trequire=on_air\tfields="
                    .to_owned(),
                invalid("input", "04"),
            ),
            (
                "\tpayload=title_take\tinput=4\tchannel=0\tbase_revision=0\tfields=".to_owned(),
                invalid("channel", "0"),
            ),
            (
                "\tpayload=title_take\tinput=4\tchannel=9\tbase_revision=0\tfields=".to_owned(),
                invalid("channel", "9"),
            ),
            (
                "\tpayload=title_take\tinput=4\tbase_revision=0\tfields=".to_owned(),
                CodecError::MissingField("channel"),
            ),
            (
                "\tpayload=title_off_air\tinput=4\tchannel=1".to_owned(),
                CodecError::UnknownField("channel".to_owned()),
            ),
            (
                "\tpayload=title_off_air".to_owned(),
                CodecError::MissingField("input"),
            ),
            // Descending, duplicate, and zero ids.
            (fields("2%3An%3A1%2C1%3An%3A1"), invalid("fields", "1")),
            (fields("1%3An%3A1%2C1%3An%3A1"), invalid("fields", "1")),
            (fields("0%3An%3A1"), invalid("fields", "0:n:1")),
            (fields("01%3An%3A1"), invalid("fields", "01:n:1")),
            // Non-canonical numbers and colors.
            (fields("1%3An%3A%2B1"), invalid("fields", "1:n:+1")),
            (fields("1%3An%3A01"), invalid("fields", "1:n:01")),
            (fields("1%3An%3A-0"), invalid("fields", "1:n:-0")),
            (fields("1%3An%3A"), invalid("fields", "1:n:")),
            (
                fields("1%3An%3A9223372036854775808"),
                invalid("fields", "1:n:9223372036854775808"),
            ),
            (
                fields("1%3Ac%3AFF001080"),
                invalid("fields", "1:c:FF001080"),
            ),
            (fields("1%3Ac%3Aff0010"), invalid("fields", "1:c:ff0010")),
            // Unknown tags, bad counts, and separators.
            (fields("1%3Ax%3A1"), invalid("fields", "1:x:1")),
            (fields("1%3At%3A3%3Aab"), invalid("fields", "1:t:3:ab")),
            (fields("1%3At%3A1%3Aab"), invalid("fields", "1:t:1:ab")),
            (fields("1%3At%3A01%3Aa"), invalid("fields", "1:t:01:a")),
            (fields("1%3At%3Aa"), invalid("fields", "1:t:a")),
            // A count that splits a multi-byte character.
            (fields("1%3At%3A1%3A%C3%A9"), invalid("fields", "1:t:1:é")),
            (fields("1%3An%3A1%2C"), invalid("fields", "1:n:1,")),
            (fields("%2C1%3An%3A1"), invalid("fields", ",1:n:1")),
            // Blank image URIs.
            (fields("1%3Ai%3A0%3A"), invalid("fields", "1")),
            (fields("1%3Ai%3A2%3A%20%20"), invalid("fields", "1")),
        ] {
            assert_eq!(
                decode_line(&format!("{PREFIX}{tail}\n")),
                Err(expected),
                "{tail}"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn title_field_bounds_are_enforced_in_both_directions() {
        let set = |fields: Vec<TitleFieldUpdate>| {
            title_command(CommandPayload::SetTitleFields {
                input: input(4),
                base_revision: 0,
                require: TitleAirRequirement::OffAir,
                fields,
            })
        };
        // Exactly at every bound: accepted and exact.
        let longest = "é".repeat(MAX_TITLE_TEXT_CHARS);
        let at_bounds = set(vec![
            update(1, TitleFieldValue::Text(longest.clone())),
            update(
                2,
                TitleFieldValue::Text("x".repeat(MAX_TITLE_CONTENT_BYTES - longest.len())),
            ),
        ]);
        let encoded = encode_line(&at_bounds).unwrap();
        assert_eq!(decode_line(&encoded).unwrap(), at_bounds);
        let most = set((1..=MAX_TITLE_FIELDS as u128)
            .map(|id| update(id, TitleFieldValue::Number(1)))
            .collect());
        assert_eq!(decode_line(&encode_line(&most).unwrap()).unwrap(), most);

        // One past every bound: refused by the encoder and the decoder.
        let too_long = set(vec![update(
            1,
            TitleFieldValue::Text("a".repeat(MAX_TITLE_TEXT_CHARS + 1)),
        )]);
        let too_large = set(vec![
            update(1, TitleFieldValue::Text("a".repeat(4_000))),
            update(2, TitleFieldValue::Text("a".repeat(4_000))),
            update(3, TitleFieldValue::Text("a".repeat(4_000))),
            update(
                4,
                TitleFieldValue::Image {
                    asset_uri: "a".repeat(MAX_TITLE_CONTENT_BYTES - 12_000 + 1),
                },
            ),
        ]);
        let too_many = set((1..=MAX_TITLE_FIELDS as u128 + 1)
            .map(|id| update(id, TitleFieldValue::Number(1)))
            .collect());
        let unordered = set(vec![
            update(2, TitleFieldValue::Number(1)),
            update(1, TitleFieldValue::Number(1)),
        ]);
        let blank = set(vec![update(
            1,
            TitleFieldValue::Image {
                asset_uri: " ".to_owned(),
            },
        )]);
        for (message, expected_field) in [
            (too_long, "fields"),
            (too_large, "fields"),
            (unordered, "fields"),
            (blank, "fields"),
        ] {
            assert!(
                matches!(
                    encode_line(&message),
                    Err(CodecError::InvalidField { field, .. }) if field == expected_field
                ),
                "{message:?}"
            );
        }
        assert_eq!(
            encode_line(&too_many),
            Err(CodecError::TooManyItems("fields"))
        );

        // The decoder refuses the same values handcrafted onto the wire.
        let wire = |raw: &str| {
            let mut escaped = String::new();
            for byte in raw.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                    escaped.push(char::from(byte));
                } else {
                    write!(escaped, "%{byte:02X}").unwrap();
                }
            }
            format!(
                "{PREFIX}\tpayload=title_fields\tinput=4\tbase_revision=0\trequire=off_air\tfields={escaped}\n"
            )
        };
        let text = "a".repeat(MAX_TITLE_TEXT_CHARS + 1);
        assert_eq!(
            decode_line(&wire(&format!("1:t:{}:{text}", text.len()))),
            Err(CodecError::InvalidField {
                field: "fields",
                value: "1".to_owned(),
            })
        );
        let chunk = "a".repeat(4_000);
        let over = (1..=4)
            .map(|id| format!("{id}:t:{}:{chunk}", chunk.len()))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            decode_line(&wire(&over)),
            Err(CodecError::InvalidField {
                field: "fields",
                value: "16000 content bytes".to_owned(),
            })
        );
        let many = (1..=MAX_TITLE_FIELDS + 1)
            .map(|id| format!("{id}:n:1"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            decode_line(&wire(&many)),
            Err(CodecError::TooManyItems("fields"))
        );
    }

    fn overlays_with_title() -> Vec<OverlayStatus> {
        let mut overlays = OverlayStatus::empty_channels();
        overlays[1].source = Some(input(42));
        overlays[1].active = true;
        overlays[1].transition = OverlayTransitionKind::Fade;
        overlays[1].duration_frames = 12;
        overlays
    }

    #[test]
    fn title_changed_event_is_exact() {
        let message = WireMessage::Event(EventMessage {
            cursor: cursor(),
            payload: EventPayload::TitleChanged {
                input: input(42),
                field_revision: 6,
                fields: sample_fields(),
                overlays: overlays_with_title(),
            },
            edit_history: EditHistoryStatus::EMPTY,
        });
        let encoded = encode_line(&message).unwrap();
        assert!(
            encoded.starts_with(&format!(
                "event\tengine_id=engine-a\tstate_epoch=7\tlog_id=log-a\trevision=1842\tevent=title_changed\tinput=42\tfield_revision=6\tfields={SAMPLE_FIELDS_WIRE}\toverlays=1%3A0%3A0%3A0%3Acut%3A1"
            )),
            "{encoded}"
        );
        assert!(
            encoded.contains("%3B2%3A42%3A1%3A0%3Afade%3A12%3A"),
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);

        for (from, to, expected) in [
            (
                "\tfield_revision=6",
                "\tfield_revision=06",
                CodecError::InvalidField {
                    field: "field_revision",
                    value: "06".to_owned(),
                },
            ),
            (
                "\tfield_revision=6",
                "",
                CodecError::MissingField("field_revision"),
            ),
            (
                "fields=1%3At%3A11",
                "fields=1%3At%3A12",
                CodecError::InvalidField {
                    field: "fields",
                    value: "1:t:12:Ana, M: 50%,2:n:-7,3:c:ff001080,4:i:16:asset://logo.png"
                        .to_owned(),
                },
            ),
        ] {
            assert_eq!(
                decode_line(&encoded.replace(from, to)),
                Err(expected),
                "{to}"
            );
        }
        let no_overlays = encoded
            .split('\t')
            .filter(|field| !field.starts_with("overlays="))
            .collect::<Vec<_>>()
            .join("\t");
        assert_eq!(
            decode_line(&no_overlays),
            Err(CodecError::MissingField("overlays"))
        );
    }

    fn titled_snapshot(titles: Vec<TitleSummary>) -> WireMessage {
        let WireMessage::Snapshot(mut snapshot) = snapshot(input_statuses(&[
            (1, "Camera"),
            (2, "Lower third"),
            (3, "Score"),
        ])) else {
            unreachable!("the helper builds a snapshot");
        };
        snapshot.input_audio_strips = input_audio_strips(&[1, 2, 3]);
        snapshot.titles = titles;
        WireMessage::Snapshot(snapshot)
    }

    /// Protocol 2.31: the snapshot's `scenes` list names Scene inputs of the
    /// snapshot strictly ascending.
    #[test]
    fn snapshot_scene_lists_round_trip_and_are_strict() {
        let scened = |scenes: &[u128]| {
            let WireMessage::Snapshot(mut snapshot) = titled_snapshot(Vec::new()) else {
                unreachable!("the helper builds a snapshot");
            };
            snapshot.scenes = scenes.iter().map(|value| input(*value)).collect();
            WireMessage::Snapshot(snapshot)
        };
        let message = scened(&[1, 3]);
        let encoded = encode_line(&message).unwrap();
        assert!(
            encoded.contains("\ttitles=\tscenes=1%2C3\tlayout_scenes=\tframe_rate="),
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
        for scenes in [&[3, 1][..], &[2, 2], &[9]] {
            assert!(matches!(
                encode_line(&scened(scenes)),
                Err(CodecError::InvalidField {
                    field: "scenes",
                    ..
                })
            ));
        }
        for tampered in [
            "scenes=3%2C1",
            "scenes=1%2C1",
            "scenes=9",
            "scenes=01",
            "scenes=0",
            "scenes=1%2C",
        ] {
            let line = encoded.replace("scenes=1%2C3", tampered);
            assert!(
                matches!(
                    decode_line(&line),
                    Err(CodecError::InvalidField {
                        field: "scenes",
                        ..
                    })
                ),
                "{tampered}"
            );
        }
        let missing = encoded.replace("\tscenes=1%2C3", "");
        assert_eq!(
            decode_line(&missing),
            Err(CodecError::MissingField("scenes"))
        );
    }

    /// Protocol 2.35: the snapshot's `layout_scenes` list names every
    /// project scene strictly ascending, whether or not a Scene input shows
    /// it.
    #[test]
    fn snapshot_layout_scene_lists_round_trip_and_are_strict() {
        let scene = |value: u128| {
            fm_protocol::WireSceneId::new(core::num::NonZeroU128::new(value).unwrap())
        };
        let listed = |scenes: &[u128]| {
            let WireMessage::Snapshot(mut snapshot) = titled_snapshot(Vec::new()) else {
                unreachable!("the helper builds a snapshot");
            };
            snapshot.layout_scenes = scenes.iter().map(|value| scene(*value)).collect();
            WireMessage::Snapshot(snapshot)
        };
        let message = listed(&[2, 40]);
        let encoded = encode_line(&message).unwrap();
        assert!(
            encoded.contains("\tscenes=\tlayout_scenes=2%2C40\tframe_rate="),
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
        for scenes in [&[3, 1][..], &[2, 2]] {
            assert!(matches!(
                encode_line(&listed(scenes)),
                Err(CodecError::InvalidField {
                    field: "layout_scenes",
                    ..
                })
            ));
        }
        let too_many = (1..=257).collect::<Vec<_>>();
        assert!(encode_line(&listed(&too_many)).is_err());
        for tampered in [
            "layout_scenes=40%2C2",
            "layout_scenes=2%2C2",
            "layout_scenes=02",
            "layout_scenes=0",
            "layout_scenes=2%2C",
        ] {
            let line = encoded.replace("layout_scenes=2%2C40", tampered);
            assert!(decode_line(&line).is_err(), "{tampered}");
        }
        let missing = encoded.replace("\tlayout_scenes=2%2C40", "");
        assert_eq!(
            decode_line(&missing),
            Err(CodecError::MissingField("layout_scenes"))
        );
    }

    #[test]
    fn snapshot_title_summaries_round_trip_and_are_strict() {
        let message = titled_snapshot(vec![
            TitleSummary {
                input: input(2),
                field_revision: 0,
            },
            TitleSummary {
                input: input(3),
                field_revision: 17,
            },
        ]);
        let encoded = encode_line(&message).unwrap();
        assert!(
            encoded.ends_with(
                "\ttitles=2%3A0%2C3%3A17\tscenes=\tlayout_scenes=\tframe_rate=30000%2F1001\tplaylist=0\tgrade_states=0\tanimation_preset_states=0\ttransition_favorites=fade%3A30%3Bwipe%3A30%3Bfly%3A30%3Bcube%3A30\tmedia_states=0\tcue_list_states=0\tinput_categories=\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CGroup%25202%7E0%7E0%7E0%7E-%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=sip\taudio_monitor=%7E0%7E0\tinput_audio_labels=\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n"
            ),
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);

        for titles in [vec![(3, 1), (2, 1)], vec![(2, 1), (2, 1)], vec![(9, 1)]] {
            let message = titled_snapshot(
                titles
                    .into_iter()
                    .map(|(value, field_revision)| TitleSummary {
                        input: input(value),
                        field_revision,
                    })
                    .collect(),
            );
            assert!(matches!(
                encode_line(&message),
                Err(CodecError::InvalidField {
                    field: "titles",
                    ..
                })
            ));
        }
        for tampered in [
            "titles=3%3A1%2C2%3A1",
            "titles=2%3A1%2C2%3A1",
            "titles=9%3A1",
            "titles=02%3A1",
            "titles=2%3A01",
            "titles=2",
            "titles=2%3A1%2C",
        ] {
            assert!(
                matches!(
                    decode_line(&encoded.replace("titles=2%3A0%2C3%3A17", tampered)),
                    Err(CodecError::InvalidField {
                        field: "titles",
                        ..
                    })
                ),
                "{tampered}"
            );
        }
        assert_eq!(
            decode_line(&encoded.replace("\ttitles=2%3A0%2C3%3A17", "")),
            Err(CodecError::MissingField("titles"))
        );
    }

    fn state(fields: Vec<TitleFieldStatus>) -> WireMessage {
        WireMessage::TitleState(TitleStateMessage {
            revision: 9,
            input: input(2),
            name: "Lower third".to_owned(),
            field_revision: 4,
            phase: TitlePhase::In,
            phase_frame: 3,
            phase_frames: 20,
            fields,
        })
    }

    fn status(id: u128, name: &str, value: TitleFieldValue) -> TitleFieldStatus {
        TitleFieldStatus {
            field: field(id),
            name: name.to_owned(),
            value,
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn title_state_is_exact_and_strict() {
        let message = state(vec![
            status(1, "Name, first", TitleFieldValue::Text("Ana".to_owned())),
            status(2, "Score", TitleFieldValue::Number(3)),
        ]);
        let encoded = encode_line(&message).unwrap();
        assert_eq!(
            encoded,
            "title_state\trevision=9\tinput=2\tname=Lower%20third\tfield_revision=4\tphase=in\tphase_frame=3\tphase_frames=20\tfields=1%3At%3A3%3AAna%2C2%3An%3A3\tfield_names=11%3AName%2C%20first%2C5%3AScore\n"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
        let empty = state(Vec::new());
        let encoded_empty = encode_line(&empty).unwrap();
        assert!(encoded_empty.ends_with("\tfields=\tfield_names=\n"));
        assert_eq!(decode_line(&encoded_empty).unwrap(), empty);

        let invalid = |field: &'static str, value: &str| CodecError::InvalidField {
            field,
            value: value.to_owned(),
        };
        for (from, to, expected) in [
            (
                "field_names=11%3AName%2C%20first%2C5%3AScore",
                "field_names=11%3AName%2C%20first",
                invalid("field_names", "11:Name, first"),
            ),
            (
                "field_names=11%3AName%2C%20first%2C5%3AScore",
                "field_names=11%3AName%2C%20first%2C5%3AScore%2C1%3Ax",
                invalid("field_names", "11:Name, first,5:Score,1:x"),
            ),
            (
                "field_names=11%3AName%2C%20first%2C5%3AScore",
                "field_names=11%3AName%2C%20first%2C1%3A%20",
                invalid("field_names", " "),
            ),
            ("name=Lower%20third", "name=%20", invalid("name", " ")),
            ("revision=9", "revision=09", invalid("revision", "09")),
            ("input=2", "input=0", invalid("input", "0")),
            ("phase=in", "phase=In", invalid("phase", "In")),
            (
                "phase_frame=3",
                "phase_frame=03",
                invalid("phase_frame", "03"),
            ),
            (
                "phase_frame=3",
                "phase_frame=20",
                invalid("phase_frame", "20/20"),
            ),
            ("phase=in", "phase=hold", invalid("phase_frame", "3/20")),
        ] {
            assert_eq!(
                decode_line(&encoded.replace(from, to)),
                Err(expected),
                "{to}"
            );
        }
        assert_eq!(
            decode_line(&encoded.replace("\tfield_names=11%3AName%2C%20first%2C5%3AScore", "")),
            Err(CodecError::MissingField("field_names"))
        );
        assert_eq!(
            decode_line(&encoded.replace("\tphase=in", "")),
            Err(CodecError::MissingField("phase"))
        );
        assert_eq!(
            decode_line(&encoded.replace("\tphase_frames=20", "")),
            Err(CodecError::MissingField("phase_frames"))
        );
    }

    #[test]
    fn title_state_encoder_refuses_what_the_decoder_refuses() {
        let long_name = WireMessage::TitleState(TitleStateMessage {
            name: "n".repeat(MAX_TITLE_NAME_BYTES + 1),
            ..match state(Vec::new()) {
                WireMessage::TitleState(message) => message,
                _ => unreachable!(),
            }
        });
        assert!(matches!(
            encode_line(&long_name),
            Err(CodecError::InvalidField { field: "name", .. })
        ));
        let blank_field = state(vec![status(1, "  ", TitleFieldValue::Number(0))]);
        assert!(matches!(
            encode_line(&blank_field),
            Err(CodecError::InvalidField {
                field: "field_names",
                ..
            })
        ));
        let names_over = state(
            (1..=2)
                .map(|id| {
                    status(
                        id,
                        &"n".repeat(MAX_TITLE_FIELD_NAMES_BYTES / 2 + 1),
                        TitleFieldValue::Number(0),
                    )
                })
                .collect(),
        );
        assert!(matches!(
            encode_line(&names_over),
            Err(CodecError::InvalidField {
                field: "field_names",
                ..
            })
        ));
    }

    #[test]
    fn title_state_phase_progress_is_exact_and_strict() {
        // Every phase round-trips; idle and hold carry no progress.
        for (phase, token, frame, frames) in [
            (TitlePhase::Idle, "idle", 0, 0),
            (TitlePhase::In, "in", 0, 1),
            (TitlePhase::Hold, "hold", 0, 0),
            (TitlePhase::Out, "out", 11, 12),
        ] {
            let message = WireMessage::TitleState(TitleStateMessage {
                phase,
                phase_frame: frame,
                phase_frames: frames,
                ..match state(Vec::new()) {
                    WireMessage::TitleState(message) => message,
                    _ => unreachable!(),
                }
            });
            let line = encode_line(&message).unwrap();
            assert!(
                line.contains(&format!(
                    "\tphase={token}\tphase_frame={frame}\tphase_frames={frames}\t"
                )),
                "{line}"
            );
            assert_eq!(decode_line(&line).unwrap(), message);
        }
        // Progress outside its phase is refused both ways.
        for (phase, frame, frames) in [
            (TitlePhase::Idle, 0, 1),
            (TitlePhase::Hold, 1, 0),
            (TitlePhase::In, 0, 0),
            (TitlePhase::Out, 12, 12),
        ] {
            let message = WireMessage::TitleState(TitleStateMessage {
                phase,
                phase_frame: frame,
                phase_frames: frames,
                ..match state(Vec::new()) {
                    WireMessage::TitleState(message) => message,
                    _ => unreachable!(),
                }
            });
            assert!(matches!(
                encode_line(&message),
                Err(CodecError::InvalidField {
                    field: "phase_frame",
                    ..
                })
            ));
        }
    }

    /// The largest title the bounds admit, with the costliest bytes the
    /// record layer escapes, still fits one field value and one record in
    /// every message that carries it.
    #[test]
    fn a_maximal_title_fits_every_record_that_carries_it() {
        let per_field = MAX_TITLE_CONTENT_BYTES / MAX_TITLE_FIELDS;
        let per_name = MAX_TITLE_FIELD_NAMES_BYTES / MAX_TITLE_FIELDS;
        let ids = (0..MAX_TITLE_FIELDS as u128)
            .map(|offset| u128::MAX - MAX_TITLE_FIELDS as u128 + 1 + offset);
        let fields = ids
            .clone()
            .map(|id| update(id, TitleFieldValue::Text("%".repeat(per_field))))
            .collect::<Vec<_>>();
        let state = WireMessage::TitleState(TitleStateMessage {
            revision: u64::MAX,
            input: input(u128::MAX),
            name: "%".repeat(MAX_TITLE_NAME_BYTES),
            field_revision: u64::MAX,
            phase: TitlePhase::Out,
            phase_frame: u64::MAX - 1,
            phase_frames: u64::MAX,
            fields: ids
                .map(|id| {
                    status(
                        id,
                        &",".repeat(per_name),
                        TitleFieldValue::Text("%".repeat(per_field)),
                    )
                })
                .collect(),
        });
        let changed = WireMessage::Event(EventMessage {
            cursor: cursor(),
            payload: EventPayload::TitleChanged {
                input: input(u128::MAX),
                field_revision: u64::MAX,
                fields: fields.clone(),
                overlays: overlays_with_title(),
            },
            edit_history: EditHistoryStatus::EMPTY,
        });
        let take = title_command(CommandPayload::TakeTitle {
            input: input(u128::MAX),
            channel: channel(8),
            base_revision: u64::MAX,
            fields: fields.clone(),
        });
        let set = title_command(CommandPayload::SetTitleFields {
            input: input(u128::MAX),
            base_revision: u64::MAX,
            require: TitleAirRequirement::OnAir,
            fields,
        });
        for message in [state, changed, take, set] {
            let encoded = encode_line(&message).unwrap();
            assert!(encoded.len() <= MAX_LINE_BYTES);
            assert_eq!(decode_line(&encoded).unwrap(), message);
        }
    }

    #[test]
    fn title_state_is_a_distinct_record_kind() {
        let message = state(vec![status(1, "Name", TitleFieldValue::Number(0))]);
        let mut decoder = LineDecoder::new();
        let encoded = encode_line(&message).unwrap();
        assert_eq!(decoder.push(encoded.as_bytes()).unwrap(), vec![message]);
        assert_eq!(
            decode_line("title_status\trevision=1\n"),
            Err(CodecError::UnknownMessage("title_status".to_owned()))
        );
    }
}

/// Protocol 2.25 playlists: `playlist_set`, the playlist control commands,
/// the authority's `playlist_advance`, and `playlist_changed`; Protocol 2.26
/// adds the snapshot's `playlist_state` record and frame rate.
mod playlists {
    use super::*;
    use fm_protocol::{
        MAX_PLAYLIST_ENTRIES, PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation,
        PlaylistRunState, PlaylistStart, PlaylistStateMessage, PlaylistTransitionKind,
    };

    const PREFIX: &str = "command\tprotocol=2.57\tid=a\tidempotency_key=k";
    const EVENT: &str = "event\tengine_id=engine-a\tstate_epoch=7\tlog_id=log-a\trevision=1842";
    const HISTORY: &str = "\tundo_depth=0\tredo_depth=0\n";

    fn entry(
        id: u64,
        value: u128,
        duration_frames: u64,
        transition: PlaylistTransitionKind,
        transition_frames: u32,
    ) -> PlaylistEntrySpec {
        PlaylistEntrySpec {
            id,
            input: input(value),
            duration_frames,
            transition,
            transition_frames,
            enabled: true,
        }
    }

    fn sample_entries() -> Vec<PlaylistEntrySpec> {
        vec![
            entry(1, 42, 90, PlaylistTransitionKind::Cut, 0),
            PlaylistEntrySpec {
                enabled: false,
                ..entry(u64::MAX, 7, 3_600, PlaylistTransitionKind::AlphaFade, 3_600)
            },
            entry(3, 8, 10, PlaylistTransitionKind::Wipe, 1),
        ]
    }

    const SAMPLE_ENTRIES_WIRE: &str = "1%3A42%3A90%3Acut%3A0%3A1%2C18446744073709551615%3A7%3A3600%3Aalpha_fade%3A3600%3A0%2C3%3A8%3A10%3Awipe%3A1%3A1";

    fn control(operation: PlaylistOperation) -> CommandPayload {
        CommandPayload::PlaylistControl { operation }
    }

    #[test]
    fn protocol_2_25_playlist_commands_are_exact() {
        assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 57));
        let cases = [
            (
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::All,
                    entries: sample_entries(),
                },
                format!("\tpayload=playlist_set\tloop=all\tentries={SAMPLE_ENTRIES_WIRE}\n"),
            ),
            (
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: Vec::new(),
                },
                "\tpayload=playlist_set\tloop=off\tentries=\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Start {
                    at: PlaylistStart::Now,
                    from: None,
                }),
                "\tpayload=playlist_start\tstart=now\tfrom=-\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Start {
                    at: PlaylistStart::AfterFrames(75),
                    from: Some(3),
                }),
                "\tpayload=playlist_start\tstart=after%3A75\tfrom=3\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Start {
                    at: PlaylistStart::AtFrame(u64::MAX),
                    from: None,
                }),
                "\tpayload=playlist_start\tstart=at%3A18446744073709551615\tfrom=-\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Stop),
                "\tpayload=playlist_stop\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Pause),
                "\tpayload=playlist_pause\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Resume),
                "\tpayload=playlist_resume\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Next),
                "\tpayload=playlist_next\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Previous),
                "\tpayload=playlist_previous\n".to_owned(),
            ),
            (
                control(PlaylistOperation::Jump { entry: 0 }),
                "\tpayload=playlist_jump\tentry=0\n".to_owned(),
            ),
            (
                CommandPayload::PlaylistAdvance { frame: 1_250 },
                "\tpayload=playlist_advance\tframe=1250\n".to_owned(),
            ),
        ];
        for (payload, tail) in cases {
            let message = WireMessage::Command(edit_command(payload));
            let encoded = encode_line(&message).unwrap();
            assert_eq!(
                encoded,
                format!(
                    "command\tprotocol=2.57\tid=01K%3Atest\tidempotency_key=operator-7%3A01K{tail}"
                )
            );
            assert_eq!(decode_line(&encoded).unwrap(), message);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn playlist_commands_reject_out_of_contract_records() {
        let invalid = |field: &'static str, value: &str| CodecError::InvalidField {
            field,
            value: value.to_owned(),
        };
        for (tail, expected) in [
            (
                "\tpayload=playlist_set\tentries=",
                CodecError::MissingField("loop"),
            ),
            (
                "\tpayload=playlist_set\tloop=forever\tentries=",
                invalid("loop", "forever"),
            ),
            (
                "\tpayload=playlist_set\tloop=off",
                CodecError::MissingField("entries"),
            ),
            // Zero duration, a cut with frames, a fade without frames or
            // longer than its entry, an unknown kind, and a duplicate id.
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A0%3Acut%3A0%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Acut%3A1%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Afade%3A0%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Afade%3A6%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5000%3Afade%3A3601%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Astinger%3A2%3A1",
                invalid("entries", "1:2:5:stinger:2:1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Acut%3A0%3A1%2C1%3A3%3A5%3Acut%3A0%3A1",
                invalid("entries", "1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Acut%3A0%3Ayes",
                invalid("entries", "1:2:5:cut:0:yes"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A0%3A5%3Acut%3A0%3A1",
                invalid("entries", "1:0:5:cut:0:1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=01%3A2%3A5%3Acut%3A0%3A1",
                invalid("entries", "01:2:5:cut:0:1"),
            ),
            (
                "\tpayload=playlist_set\tloop=off\tentries=1%3A2%3A5%3Acut%3A0",
                invalid("entries", "1:2:5:cut:0"),
            ),
            (
                "\tpayload=playlist_start\tfrom=-",
                CodecError::MissingField("start"),
            ),
            (
                "\tpayload=playlist_start\tstart=now",
                CodecError::MissingField("from"),
            ),
            (
                "\tpayload=playlist_start\tstart=soon\tfrom=-",
                invalid("start", "soon"),
            ),
            (
                "\tpayload=playlist_start\tstart=after%3A%2B5\tfrom=-",
                invalid("start", "after:+5"),
            ),
            (
                "\tpayload=playlist_start\tstart=now\tfrom=none",
                invalid("from", "none"),
            ),
            ("\tpayload=playlist_jump", CodecError::MissingField("entry")),
            (
                "\tpayload=playlist_stop\tentry=1",
                CodecError::UnknownField("entry".to_owned()),
            ),
            (
                "\tpayload=playlist_advance\tframe=-1",
                invalid("frame", "-1"),
            ),
            (
                "\tpayload=playlist_rewind",
                invalid("payload", "playlist_rewind"),
            ),
        ] {
            assert_eq!(
                decode_line(&format!("{PREFIX}{tail}\n")),
                Err(expected),
                "{tail}"
            );
        }

        // The encoder refuses what the decoder rejects, and the list bound.
        let mut too_many = Vec::new();
        for id in 0..=MAX_PLAYLIST_ENTRIES {
            too_many.push(entry(
                u64::try_from(id).unwrap(),
                1,
                1,
                PlaylistTransitionKind::Cut,
                0,
            ));
        }
        for entries in [
            too_many,
            vec![entry(1, 1, 5, PlaylistTransitionKind::Fade, 6)],
        ] {
            assert!(
                encode_line(&WireMessage::Command(edit_command(
                    CommandPayload::SetPlaylist {
                        loop_mode: PlaylistLoopMode::Off,
                        entries,
                    }
                )))
                .is_err()
            );
        }
        // The largest playlist fits one record.
        let largest = largest_entries();
        let message = WireMessage::Event(EventMessage {
            cursor: cursor(),
            payload: EventPayload::PlaylistChanged {
                frame: u64::MAX,
                loop_mode: PlaylistLoopMode::One,
                entries: largest,
                state: PlaylistRunState::Paused {
                    entry: u64::MAX,
                    take: u64::MAX,
                    cycle: u64::MAX,
                    elapsed_frames: u64::MAX,
                },
                program: WireInputId::new(NonZeroU128::MAX),
                preview: WireInputId::new(NonZeroU128::MAX),
            },
            edit_history: EditHistoryStatus::EMPTY,
        });
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.len() <= MAX_LINE_BYTES);
        assert_eq!(decode_line(&encoded).unwrap(), message);
    }

    /// The longest possible entry list: every field at its widest encoding.
    fn largest_entries() -> Vec<PlaylistEntrySpec> {
        (0..MAX_PLAYLIST_ENTRIES)
            .map(|index| PlaylistEntrySpec {
                id: u64::MAX - u64::try_from(index).unwrap(),
                input: WireInputId::new(NonZeroU128::MAX),
                duration_frames: fm_protocol::MAX_PLAYLIST_ENTRY_FRAMES,
                transition: PlaylistTransitionKind::AlphaFade,
                transition_frames: fm_protocol::MAX_PLAYLIST_TRANSITION_FRAMES,
                enabled: false,
            })
            .collect()
    }

    fn changed(state: PlaylistRunState) -> WireMessage {
        WireMessage::Event(EventMessage {
            cursor: cursor(),
            payload: EventPayload::PlaylistChanged {
                frame: 120,
                loop_mode: PlaylistLoopMode::All,
                entries: sample_entries(),
                state,
                program: input(42),
                preview: input(7),
            },
            edit_history: EditHistoryStatus::EMPTY,
        })
    }

    #[test]
    fn playlist_changed_events_are_exact_for_every_state_and_strict() {
        for (state, wire) in [
            (PlaylistRunState::Stopped, "stopped"),
            (
                PlaylistRunState::Armed {
                    start_frame: 200,
                    from: None,
                },
                "armed%3A200%3A-",
            ),
            (
                PlaylistRunState::Armed {
                    start_frame: 200,
                    from: Some(3),
                },
                "armed%3A200%3A3",
            ),
            (
                PlaylistRunState::Playing {
                    entry: 1,
                    take: 4,
                    cycle: 2,
                    started_frame: 120,
                },
                "playing%3A1%3A4%3A2%3A120",
            ),
            (
                PlaylistRunState::Paused {
                    entry: 1,
                    take: 4,
                    cycle: 2,
                    elapsed_frames: 0,
                },
                "paused%3A1%3A4%3A2%3A0",
            ),
            (PlaylistRunState::Finished { last: Some(3) }, "finished%3A3"),
            (PlaylistRunState::Finished { last: None }, "finished%3A-"),
        ] {
            let message = changed(state);
            let encoded = encode_line(&message).unwrap();
            assert_eq!(
                encoded,
                format!(
                    "{EVENT}\tevent=playlist_changed\tframe=120\tloop=all\tentries={SAMPLE_ENTRIES_WIRE}\tstate={wire}\tprogram=42\tpreview=7{HISTORY}"
                )
            );
            assert_eq!(decode_line(&encoded).unwrap(), message);
        }

        let valid = "\tevent=playlist_changed\tframe=1\tloop=off\tentries=\tstate=stopped\tprogram=1\tpreview=2";
        assert!(decode_line(&format!("{EVENT}{valid}{HISTORY}")).is_ok());
        for (body, expected) in [
            (
                valid.replace("\tframe=1", ""),
                CodecError::MissingField("frame"),
            ),
            (
                valid.replace("state=stopped", "state=playing%3A1%3A2%3A3"),
                CodecError::InvalidField {
                    field: "state",
                    value: "playing:1:2:3".to_owned(),
                },
            ),
            (
                valid.replace("state=stopped", "state=finished"),
                CodecError::InvalidField {
                    field: "state",
                    value: "finished".to_owned(),
                },
            ),
            (
                valid.replace("state=stopped", "state=armed%3A01%3A-"),
                CodecError::InvalidField {
                    field: "state",
                    value: "armed:01:-".to_owned(),
                },
            ),
            (
                valid.replace("\tpreview=2", ""),
                CodecError::MissingField("preview"),
            ),
            (
                format!("{valid}\textra=1"),
                CodecError::UnknownField("extra".to_owned()),
            ),
        ] {
            assert_eq!(
                decode_line(&format!("{EVENT}{body}{HISTORY}")),
                Err(expected),
                "{body}"
            );
        }
    }

    fn playlist_state(entries: Vec<PlaylistEntrySpec>, state: PlaylistRunState) -> WireMessage {
        WireMessage::PlaylistState(PlaylistStateMessage {
            revision: 9,
            frame: 4_321,
            loop_mode: PlaylistLoopMode::One,
            entries,
            state,
        })
    }

    #[test]
    fn protocol_2_26_playlist_state_is_exact_and_strict() {
        assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 57));
        let message = playlist_state(
            sample_entries(),
            PlaylistRunState::Playing {
                entry: 3,
                take: 2,
                cycle: 0,
                started_frame: 4_300,
            },
        );
        let encoded = encode_line(&message).unwrap();
        assert_eq!(
            encoded,
            format!(
                "playlist_state\trevision=9\tframe=4321\tloop=one\tentries={SAMPLE_ENTRIES_WIRE}\tstate=playing%3A3%3A2%3A0%3A4300\n"
            )
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);

        let valid = "playlist_state\trevision=1\tframe=0\tloop=off\tentries=\tstate=stopped";
        assert!(decode_line(&format!("{valid}\n")).is_ok());
        for (line, expected) in [
            (
                valid.replace("\trevision=1", ""),
                CodecError::MissingField("revision"),
            ),
            (
                valid.replace("\tframe=0", ""),
                CodecError::MissingField("frame"),
            ),
            (
                valid.replace("frame=0", "frame=01"),
                CodecError::InvalidField {
                    field: "frame",
                    value: "01".to_owned(),
                },
            ),
            (
                valid.replace("loop=off", "loop=twice"),
                CodecError::InvalidField {
                    field: "loop",
                    value: "twice".to_owned(),
                },
            ),
            (
                valid.replace("entries=", "entries=1%3A2%3A0%3Acut%3A0%3A1"),
                CodecError::InvalidField {
                    field: "entries",
                    value: "1".to_owned(),
                },
            ),
            (
                valid.replace("state=stopped", "state=paused%3A1"),
                CodecError::InvalidField {
                    field: "state",
                    value: "paused:1".to_owned(),
                },
            ),
            (
                format!("{valid}\tprogram=1"),
                CodecError::UnknownField("program".to_owned()),
            ),
        ] {
            assert_eq!(decode_line(&format!("{line}\n")), Err(expected), "{line}");
        }
    }

    /// The playlist rides its own record so it never competes with the
    /// snapshot for the line budget: a maximal playlist fits one record.
    #[test]
    fn a_maximal_playlist_state_fits_one_record() {
        let message = playlist_state(
            largest_entries(),
            PlaylistRunState::Paused {
                entry: u64::MAX,
                take: u64::MAX,
                cycle: u64::MAX,
                elapsed_frames: u64::MAX,
            },
        );
        let encoded = encode_line(&message).unwrap();
        assert!(encoded.len() <= MAX_LINE_BYTES, "{}", encoded.len());
        assert_eq!(decode_line(&encoded).unwrap(), message);
        // The encoder refuses lists the decoder refuses.
        let mut too_many = largest_entries();
        too_many.push(entry(1, 1, 1, PlaylistTransitionKind::Cut, 0));
        assert_eq!(
            encode_line(&playlist_state(too_many, PlaylistRunState::Stopped)),
            Err(CodecError::TooManyItems("entries"))
        );
    }

    #[test]
    fn snapshot_frame_rate_and_playlist_announcement_are_strict() {
        let WireMessage::Snapshot(mut snapshot) =
            snapshot(input_statuses(&[(1, "Camera"), (2, "Clip")]))
        else {
            unreachable!("the helper builds a snapshot");
        };
        snapshot.frame_rate = fm_types::FrameRate::new(50, 1).unwrap();
        snapshot.playlist_follows = true;
        let message = WireMessage::Snapshot(snapshot);
        let encoded = encode_line(&message).unwrap();
        assert!(
            encoded.ends_with(
                "\tframe_rate=50%2F1\tplaylist=1\tgrade_states=0\tanimation_preset_states=0\ttransition_favorites=fade%3A30%3Bwipe%3A30%3Bfly%3A30%3Bcube%3A30\tmedia_states=0\tcue_list_states=0\tinput_categories=\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CGroup%25202%7E0%7E0%7E0%7E-%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=sip\taudio_monitor=%7E0%7E0\tinput_audio_labels=\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n"
            ),
            "{encoded}"
        );
        assert_eq!(decode_line(&encoded).unwrap(), message);
        for (from, to, expected) in [
            (
                "frame_rate=50%2F1",
                "frame_rate=100%2F2",
                CodecError::InvalidField {
                    field: "frame_rate",
                    value: "100/2".to_owned(),
                },
            ),
            (
                "frame_rate=50%2F1",
                "frame_rate=50%2F0",
                CodecError::InvalidField {
                    field: "frame_rate",
                    value: "50/0".to_owned(),
                },
            ),
            (
                "frame_rate=50%2F1",
                "frame_rate=50",
                CodecError::InvalidField {
                    field: "frame_rate",
                    value: "50".to_owned(),
                },
            ),
            (
                "frame_rate=50%2F1",
                "frame_rate=4294967296%2F1",
                CodecError::InvalidField {
                    field: "frame_rate",
                    value: "4294967296/1".to_owned(),
                },
            ),
            (
                "\tframe_rate=50%2F1",
                "",
                CodecError::MissingField("frame_rate"),
            ),
            (
                "playlist=1",
                "playlist=yes",
                CodecError::InvalidField {
                    field: "playlist",
                    value: "yes".to_owned(),
                },
            ),
            ("\tplaylist=1", "", CodecError::MissingField("playlist")),
        ] {
            assert_eq!(
                decode_line(&encoded.replace(from, to)),
                Err(expected),
                "{to}"
            );
        }
    }
}
