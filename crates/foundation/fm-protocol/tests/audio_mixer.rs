//! Protocol 2.40 mixer strip labels, VCA-style strip groups, mono pan, and
//! the Fade-to-Black output selection (AU-001, SW-004), plus Protocol 2.44
//! group solo, nested groups, the solo mode, and the monitor bus meters, and
//! the Protocol 2.53 headphone output (device, level, and mute): the
//! commands, the `audio_mixer_changed` and `fade_to_black_output_changed`
//! durable events, and the snapshot fields — exactly, and strictly.

use fm_protocol::{
    AudioGroupStatus, AudioMixerOperation, AudioMixerStatus, AudioMonitorStatus, AudioSoloMode,
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EditLockStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    InputAudioLabel, InputAudioStripStatus, InputStatus, ManualTransitionStatus, OverlayStatus,
    ProtocolVersion, RecordingStatus, SnapshotMessage, WireInputId, WireMessage, WireOutputId,
    WireRecorderSlot, decode_line, encode_line,
};
use fm_types::strip_dsp::{STRIP_DSP_CODE_LEN, StripDsp, StripDspPreset};

const PREFIX: &str = "command\tprotocol=2.147\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn output(value: u128) -> WireOutputId {
    WireOutputId::new(core::num::NonZeroU128::new(value).unwrap())
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

fn cursor() -> EventCursor {
    EventCursor {
        engine: EngineIdentity {
            engine_id: "e".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 4,
    }
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: cursor(),
        edit_history: EditHistoryStatus::EMPTY,
        payload,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn strip(value: u128, pan_basis_points: i32, group: Option<u8>) -> InputAudioStripStatus {
    InputAudioStripStatus {
        input: input(value),
        gain_millidb: -6_000,
        balance_basis_points: 2_500,
        pan_basis_points,
        group,
        muted: false,
        soloed: false,
        follow_video: true,
        delay_samples: 0,
        sends: fm_types::audio_bus::AudioSends::MASTER,
    }
}

fn snapshot(audio_mixer: AudioMixerStatus, excluded: Vec<WireOutputId>) -> WireMessage {
    WireMessage::Snapshot(SnapshotMessage {
        engine: cursor().engine,
        revision: 12,
        show_name: "Show".to_owned(),
        inputs: [1, 2]
            .map(|value| InputStatus {
                input: input(value),
                name: format!("Input {value}"),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            })
            .into(),
        outputs: [7, 8]
            .map(|value| fm_protocol::OutputStatus {
                output: output(value),
                name: format!("Output {value}"),
                aux_bus: None,
                mix_minus: None,
            })
            .into(),
        input_audio_strips: vec![strip(1, -10_000, Some(4)), strip(2, 0, None)],
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
        audio_mixer,
        fade_to_black_excluded_outputs: excluded,
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
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

fn mixer() -> AudioMixerStatus {
    let mut status = AudioMixerStatus::default();
    status.groups[1] = AudioGroupStatus {
        name: "Mics, wireless".into(),
        gain_millidb: -12_000,
        muted: true,
        soloed: true,
        parent: Some(3),
    };
    status.solo_mode = AudioSoloMode::Pfl;
    status.labels = vec![
        InputAudioLabel {
            input: input(1),
            label: "Host~mic".into(),
        },
        InputAudioLabel {
            input: input(2),
            label: "Guest".into(),
        },
    ];
    status
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 147));
}

#[test]
#[allow(clippy::too_many_lines)]
fn mixer_commands_have_an_exact_wire_form() {
    let cases = [
        (
            AudioMixerOperation::SetLabel {
                input: input(3),
                label: "Host mic".into(),
            },
            "\tpayload=audio_strip_label\tinput=3\tlabel=Host%20mic\n",
        ),
        (
            AudioMixerOperation::SetLabel {
                input: input(3),
                label: String::new(),
            },
            "\tpayload=audio_strip_label\tinput=3\tlabel=\n",
        ),
        (
            AudioMixerOperation::SetGroup {
                input: input(3),
                group: Some(2),
            },
            "\tpayload=audio_strip_group\tinput=3\tgroup=2\n",
        ),
        (
            AudioMixerOperation::SetGroup {
                input: input(3),
                group: None,
            },
            "\tpayload=audio_strip_group\tinput=3\tgroup=-\n",
        ),
        (
            AudioMixerOperation::RenameGroup {
                group: 4,
                name: "Band".into(),
            },
            "\tpayload=audio_group_name\tgroup=4\tname=Band\n",
        ),
        (
            AudioMixerOperation::SetGroupLevel {
                group: 1,
                gain_millidb: -6_000,
                muted: true,
            },
            "\tpayload=audio_group_level\tgroup=1\tgain_millidb=-6000\tmuted=1\n",
        ),
        (
            AudioMixerOperation::SetGroupSolo {
                group: 2,
                soloed: true,
            },
            "\tpayload=audio_group_solo\tgroup=2\tsoloed=1\n",
        ),
        (
            AudioMixerOperation::SetGroupParent {
                group: 2,
                parent: Some(4),
            },
            "\tpayload=audio_group_parent\tgroup=2\tparent=4\n",
        ),
        (
            AudioMixerOperation::SetGroupParent {
                group: 2,
                parent: None,
            },
            "\tpayload=audio_group_parent\tgroup=2\tparent=-\n",
        ),
        (
            AudioMixerOperation::SetSoloMode {
                mode: AudioSoloMode::InPlace,
            },
            "\tpayload=audio_solo_mode\tmode=sip\n",
        ),
        (
            AudioMixerOperation::SetSoloMode {
                mode: AudioSoloMode::Pfl,
            },
            "\tpayload=audio_solo_mode\tmode=pfl\n",
        ),
        (
            AudioMixerOperation::SetSoloMode {
                mode: AudioSoloMode::Afl,
            },
            "\tpayload=audio_solo_mode\tmode=afl\n",
        ),
        (
            AudioMixerOperation::SetMonitorDevice {
                device: Some("hw:CARD=Headset,DEV=0".into()),
            },
            "\tpayload=audio_monitor_device\tdevice=hw%3ACARD%3DHeadset%2CDEV%3D0\n",
        ),
        (
            AudioMixerOperation::SetMonitorDevice { device: None },
            "\tpayload=audio_monitor_device\tdevice=\n",
        ),
        (
            AudioMixerOperation::SetMonitorLevel {
                gain_millidb: -6_000,
                muted: true,
            },
            "\tpayload=audio_monitor_level\tgain_millidb=-6000\tmuted=1\n",
        ),
    ];
    for (operation, suffix) in cases {
        let line = round_trip(&command(CommandPayload::AudioMixer(operation.clone())));
        assert_eq!(line, format!("{PREFIX}{suffix}"));
        let line = round_trip(&event(EventPayload::AudioMixerChanged(operation.clone())));
        assert!(
            line.contains(&format!(
                "\tevent=audio_mixer_changed\toperation={}",
                operation.payload()
            )),
            "{line}"
        );
    }
    assert!(
        !AudioMixerOperation::SetLabel {
            input: input(1),
            label: String::new()
        }
        .is_live()
    );
    assert!(
        AudioMixerOperation::SetGroupLevel {
            group: 1,
            gain_millidb: 0,
            muted: false
        }
        .is_live()
    );
    assert!(
        AudioMixerOperation::SetGroupSolo {
            group: 1,
            soloed: false
        }
        .is_live()
    );
    assert!(
        !AudioMixerOperation::SetGroupParent {
            group: 1,
            parent: None
        }
        .is_live()
    );
    assert!(
        !AudioMixerOperation::SetSoloMode {
            mode: AudioSoloMode::Afl
        }
        .is_live()
    );
    // Protocol 2.53: the headphone device is a project edit, its level live.
    assert!(!AudioMixerOperation::SetMonitorDevice { device: None }.is_live());
    assert!(
        AudioMixerOperation::SetMonitorLevel {
            gain_millidb: 0,
            muted: false
        }
        .is_live()
    );
}

#[test]
fn mixer_commands_are_strict() {
    for bad in [
        "\tpayload=audio_strip_group\tinput=3\tgroup=0",
        "\tpayload=audio_strip_group\tinput=3\tgroup=5",
        "\tpayload=audio_strip_group\tinput=3\tgroup=02",
        "\tpayload=audio_strip_group\tinput=03\tgroup=1",
        "\tpayload=audio_strip_label\tinput=3\tlabel=a%0Ab",
        "\tpayload=audio_strip_label\tinput=3\tlabel=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "\tpayload=audio_group_name\tgroup=1\tname=",
        "\tpayload=audio_group_name\tgroup=1\tname=%20",
        "\tpayload=audio_group_level\tgroup=1\tgain_millidb=24001\tmuted=0",
        "\tpayload=audio_group_level\tgroup=1\tgain_millidb=-0\tmuted=0",
        "\tpayload=audio_group_level\tgroup=1\tgain_millidb=0\tmuted=2",
        "\tpayload=audio_group_level\tgroup=1\tgain_millidb=0",
        "\tpayload=audio_group_solo\tgroup=1\tsoloed=2",
        "\tpayload=audio_group_solo\tgroup=5\tsoloed=1",
        "\tpayload=audio_group_solo\tgroup=1",
        "\tpayload=audio_group_parent\tgroup=1\tparent=1",
        "\tpayload=audio_group_parent\tgroup=1\tparent=5",
        "\tpayload=audio_group_parent\tgroup=1\tparent=",
        "\tpayload=audio_group_parent\tgroup=0\tparent=-",
        "\tpayload=audio_solo_mode\tmode=PFL",
        "\tpayload=audio_solo_mode\tmode=",
        "\tpayload=audio_solo_mode",
        "\tpayload=audio_monitor_device",
        "\tpayload=audio_monitor_device\tdevice=my%20headset",
        "\tpayload=audio_monitor_device\tdevice=caf%C3%A9",
        "\tpayload=audio_monitor_device\tdevice=-report",
        "\tpayload=audio_monitor_level\tgain_millidb=24001\tmuted=0",
        "\tpayload=audio_monitor_level\tgain_millidb=0\tmuted=yes",
        "\tpayload=audio_monitor_level\tmuted=0",
        "\tpayload=fade_to_black_output\toutput=0\taffected=1",
        "\tpayload=fade_to_black_output\toutput=7\taffected=true",
        "\tpayload=input_audio_strip\tinput=1\tgain_millidb=0\tbalance_basis_points=0\tmuted=0\tsoloed=0\tfollow_video=1\tdelay_samples=0",
    ] {
        assert!(decode_line(&format!("{PREFIX}{bad}\n")).is_err(), "{bad}");
    }
    assert!(
        encode_line(&command(CommandPayload::AudioMixer(
            AudioMixerOperation::RenameGroup {
                group: 5,
                name: "x".into()
            }
        )))
        .is_err()
    );
    assert!(
        encode_line(&command(CommandPayload::AudioMixer(
            AudioMixerOperation::SetGroupParent {
                group: 2,
                parent: Some(2)
            }
        )))
        .is_err()
    );
}

#[test]
fn strip_pan_and_fade_to_black_output_commands_round_trip() {
    let line = round_trip(&command(CommandPayload::SetInputAudioStrip {
        input: input(1),
        gain_millidb: 0,
        balance_basis_points: 0,
        pan_basis_points: -7_500,
        muted: false,
        soloed: false,
        follow_video: true,
        delay_samples: 0,
        sends: fm_types::audio_bus::AudioSends::MASTER,
    }));
    assert!(line.contains("\tpan_basis_points=-7500\t"), "{line}");
    let line = round_trip(&command(CommandPayload::SetFadeToBlackOutput {
        output: output(8),
        affected: false,
    }));
    assert_eq!(
        line,
        format!("{PREFIX}\tpayload=fade_to_black_output\toutput=8\taffected=0\n")
    );
    let line = round_trip(&event(EventPayload::FadeToBlackOutputChanged {
        output: output(8),
        affected: true,
    }));
    assert!(
        line.contains("\tevent=fade_to_black_output_changed\toutput=8\taffected=1\t"),
        "{line}"
    );
}

#[test]
fn fade_to_black_durations_and_curve_round_trip_strictly() {
    // Commands: a duration, or `-` to follow the shared one; a curve.
    for (duration_frames, field) in [(Some(45), "45"), (Some(3_600), "3600"), (None, "-")] {
        let line = round_trip(&command(CommandPayload::SetFadeToBlackOutputDuration {
            output: output(8),
            duration_frames,
        }));
        assert_eq!(
            line,
            format!(
                "{PREFIX}\tpayload=fade_to_black_output_duration\toutput=8\tduration_frames={field}\n"
            )
        );
        let line = round_trip(&event(EventPayload::FadeToBlackOutputDurationChanged {
            output: output(8),
            duration_frames,
        }));
        assert!(
            line.contains(&format!(
                "\tevent=fade_to_black_output_duration_changed\toutput=8\tduration_frames={field}\t"
            )),
            "{line}"
        );
    }
    for curve in fm_protocol::FadeToBlackCurve::ALL {
        let line = round_trip(&command(CommandPayload::SetFadeToBlackCurve { curve }));
        assert_eq!(
            line,
            format!(
                "{PREFIX}\tpayload=fade_to_black_curve\tcurve={}\n",
                curve.token()
            )
        );
        let line = round_trip(&event(EventPayload::FadeToBlackCurveChanged { curve }));
        assert!(
            line.contains(&format!(
                "\tevent=fade_to_black_curve_changed\tcurve={}\t",
                curve.token()
            )),
            "{line}"
        );
    }
    for bad in [
        "\tpayload=fade_to_black_output_duration\toutput=8\tduration_frames=0",
        "\tpayload=fade_to_black_output_duration\toutput=8\tduration_frames=3601",
        "\tpayload=fade_to_black_output_duration\toutput=8\tduration_frames=045",
        "\tpayload=fade_to_black_output_duration\toutput=8\tduration_frames=",
        "\tpayload=fade_to_black_output_duration\toutput=0\tduration_frames=45",
        "\tpayload=fade_to_black_output_duration\toutput=8",
        "\tpayload=fade_to_black_curve\tcurve=Ease",
        "\tpayload=fade_to_black_curve\tcurve=",
        "\tpayload=fade_to_black_curve",
    ] {
        assert!(decode_line(&format!("{PREFIX}{bad}\n")).is_err(), "{bad}");
    }
    assert!(
        encode_line(&command(CommandPayload::SetFadeToBlackOutputDuration {
            output: output(8),
            duration_frames: Some(0),
        }))
        .is_err()
    );

    // The snapshot lists every output's own duration and the curve.
    let mut message = snapshot(AudioMixerStatus::default(), Vec::new());
    if let WireMessage::Snapshot(snapshot) = &mut message {
        snapshot.fade_to_black_output_durations = vec![(output(7), 45), (output(8), 3_600)];
        snapshot.fade_to_black_curve = fm_protocol::FadeToBlackCurve::Ease;
    }
    let line = round_trip(&message);
    assert!(
        line.contains("\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=7%3A45%2C8%3A3600\tfade_to_black_curve=ease\tdisplay_outputs="),
        "{line}"
    );
    let valid = encode_line(&message).unwrap();
    for (from, to) in [
        ("7%3A45%2C8%3A3600", "8%3A3600%2C7%3A45"),
        ("7%3A45%2C8%3A3600", "7%3A45%2C7%3A46"),
        ("7%3A45%2C8%3A3600", "7%3A0"),
        ("7%3A45%2C8%3A3600", "7%3A3601"),
        ("7%3A45%2C8%3A3600", "7"),
        ("fade_to_black_curve=ease", "fade_to_black_curve=EASE"),
        ("\tfade_to_black_curve=ease", ""),
        ("\tfade_to_black_output_durations=7%3A45%2C8%3A3600", ""),
    ] {
        let malformed = valid.replacen(from, to, 1);
        assert_ne!(malformed, valid, "{from}");
        assert!(decode_line(&malformed).is_err(), "{from} -> {to}");
    }
}

#[test]
fn snapshot_carries_groups_labels_strip_groups_and_ftb_exclusions() {
    let line = round_trip(&snapshot(mixer(), vec![output(8)]));
    assert!(
        line.contains("\tinput_audio_strips=1%3A-6000%3A2500%3A-10000%3A0%3A0%3A1%3A0%3A4%3AM%2C2%3A-6000%3A2500%3A0%3A0%3A0%3A1%3A0%3A-%3AM\t"),
        "{line}"
    );
    assert!(
        line.ends_with(&format!(
            "\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CMics%252C%2520wireless%7E-12000%7E1%7E1%7E3%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=pfl\taudio_monitor=%7E0%7E0\tinput_audio_labels=1%7E8%3AHost%7Emic%2C2%7E5%3AGuest\taudio_master_dsp={}\taudio_strip_dsp_states=0\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\taudio_aux_bus_devices=\taudio_aux_bus_omt=\tfade_to_black_excluded_outputs=8\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tscreen_capture_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\talert_rules=\ttransitions_in_flight=\n",
            fm_types::strip_dsp::StripDsp::DEFAULT.code()
        )),
        "{line}"
    );
    // Output ids span the full 128-bit range.
    let wide = 18_446_744_073_709_552_017_u128;
    let line = round_trip(&snapshot(
        AudioMixerStatus::default(),
        vec![output(8), output(wide)],
    ));
    assert!(
        line.ends_with(&format!("\tfade_to_black_excluded_outputs=8%2C{wide}\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tscreen_capture_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\talert_rules=\ttransitions_in_flight=\n"
        )),
        "{line}"
    );
    let line = round_trip(&snapshot(AudioMixerStatus::default(), Vec::new()));
    assert!(
        line.ends_with(&format!("\tinput_audio_labels=\taudio_master_dsp={}\taudio_strip_dsp_states=0\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\taudio_aux_bus_devices=\taudio_aux_bus_omt=\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tscreen_capture_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\talert_rules=\ttransitions_in_flight=\n", fm_types::strip_dsp::StripDsp::DEFAULT.code())),
        "{line}"
    );

    // Unordered or duplicate exclusions and labels, empty labels, and a
    // wrong group count are refused both ways.
    assert!(encode_line(&snapshot(mixer(), vec![output(8), output(7)])).is_err());
    let mut unordered = mixer();
    unordered.labels.reverse();
    assert!(encode_line(&snapshot(unordered, Vec::new())).is_err());
    let mut empty = mixer();
    empty.labels[0].label.clear();
    assert!(encode_line(&snapshot(empty, Vec::new())).is_err());
    let valid = encode_line(&snapshot(mixer(), vec![output(7), output(8)])).unwrap();
    for (from, to) in [
        (
            "fade_to_black_excluded_outputs=7%2C8",
            "fade_to_black_excluded_outputs=8%2C7",
        ),
        (
            "fade_to_black_excluded_outputs=7%2C8",
            "fade_to_black_excluded_outputs=7%2C7",
        ),
        (
            "fade_to_black_excluded_outputs=7%2C8",
            "fade_to_black_excluded_outputs=0",
        ),
        (
            "input_audio_labels=1%7E8%3AHost%7Emic%2C2%7E5%3AGuest",
            "input_audio_labels=2%7E5%3AGuest%2C1%7E4%3AHost",
        ),
        (
            "input_audio_labels=1%7E8%3AHost%7Emic%2C2%7E5%3AGuest",
            "input_audio_labels=1%7E",
        ),
        ("%2CGroup%25204%7E0%7E0%7E0%7E-\t", "\t"),
        (
            "%2CGroup%25204%7E0%7E0%7E0%7E-\t",
            "%2CGroup%25204%7E0%7E0%7E0%7E-%2CGroup%25205%7E0%7E0%7E0%7E-\t",
        ),
        ("Group%25203%7E0%7E0", "%7E0%7E0"),
        // The 2.40 three-part group tuple is refused.
        ("Group%25204%7E0%7E0%7E0%7E-", "Group%25204%7E0%7E0"),
        (
            "wireless%7E-12000%7E1%7E1%7E3",
            "wireless%7E-12000%7E1%7E2%7E3",
        ),
        (
            "wireless%7E-12000%7E1%7E1%7E3",
            "wireless%7E-12000%7E1%7E1%7E2",
        ),
        (
            "wireless%7E-12000%7E1%7E1%7E3",
            "wireless%7E-12000%7E1%7E1%7E5",
        ),
        ("\taudio_solo_mode=pfl", "\taudio_solo_mode=solo"),
        ("\taudio_solo_mode=pfl", ""),
    ] {
        assert!(valid.contains(from), "{from}");
        let malformed = valid.replace(from, to);
        assert!(decode_line(&malformed).is_err(), "{to}");
    }
    let missing = valid.replace("\tfade_to_black_excluded_outputs=7%2C8", "");
    assert!(decode_line(&missing).is_err());
}

#[test]
fn label_lengths_must_match_their_labels() {
    // A length that disagrees with the label, a zero or non-canonical
    // length, a length past the label bound, and the pre-2.45 escaped form
    // are refused.
    let valid = encode_line(&snapshot(mixer(), Vec::new())).unwrap();
    let from = "input_audio_labels=1%7E8%3AHost%7Emic%2C2%7E5%3AGuest";
    assert!(valid.contains(from), "{valid}");
    for to in [
        "input_audio_labels=1%7E7%3AHost%7Emic%2C2%7E5%3AGuest",
        "input_audio_labels=1%7E9%3AHost%7Emic%2C2%7E5%3AGuest",
        "input_audio_labels=1%7E0%3A%2C2%7E5%3AGuest",
        "input_audio_labels=1%7E08%3AHost%7Emic%2C2%7E5%3AGuest",
        "input_audio_labels=1%7E33%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "input_audio_labels=1%7EHost%257Emic%2C2%7EGuest",
    ] {
        assert!(decode_line(&valid.replace(from, to)).is_err(), "{to}");
    }
}

/// 32 label bytes that each escape to three line bytes (Protocol 2.45
/// writes a label raw, so the line encoding escapes it exactly once).
fn worst_label(index: usize) -> String {
    let label = format!("{}ñ", ["~", ","][index % 2]).repeat(10) + "é";
    assert_eq!(label.len(), fm_protocol::MAX_AUDIO_LABEL_BYTES);
    label
}

fn worst_labels(ids: impl Iterator<Item = u128>) -> Vec<InputAudioLabel> {
    ids.enumerate()
        .map(|(index, id)| InputAudioLabel {
            input: input(id),
            label: worst_label(index),
        })
        .collect()
}

#[test]
fn a_worst_case_label_list_fits_one_field_with_the_largest_ids() {
    // 256 labels of 32 escaping bytes on the largest input ids.
    let labels =
        worst_labels((0..fm_protocol::MAX_LIST_ITEMS as u128).map(|n| u128::MAX - 255 + n));
    let mut message = snapshot(AudioMixerStatus::default(), Vec::new());
    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!()
    };
    snapshot.audio_mixer.labels = labels;
    // The rest of this snapshot is small; the label list alone is near its
    // worst case and still inside one field.
    let line = round_trip(&message);
    let field = line
        .split('\t')
        .find_map(|field| field.strip_prefix("input_audio_labels="))
        .unwrap();
    assert!(
        field.len() <= fm_protocol::MAX_FIELD_VALUE_BYTES,
        "{}",
        field.len()
    );
    // Each entry is `<39-digit id>%7E32%3A` + 96 escaped label bytes,
    // joined by `%2C`: escaped once, not twice.
    assert_eq!(field.len(), 256 * (39 + 3 + 2 + 3 + 96) + 255 * 3);
}

#[test]
fn the_largest_snapshot_mixer_fits_one_line() {
    // A show at its 256-input limit with every strip labelled by a worst-case
    // label: before Protocol 2.45 the doubly escaped labels pushed this
    // snapshot past the line limit.
    let ids = 1..=fm_protocol::MAX_LIST_ITEMS as u128;
    let mut message = snapshot(AudioMixerStatus::default(), Vec::new());
    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!()
    };
    snapshot.inputs = ids
        .clone()
        .map(|id| InputStatus {
            input: input(id),
            name: format!("Input {id}"),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        })
        .collect();
    snapshot.input_audio_strips = ids.clone().map(|id| strip(id, 0, Some(4))).collect();
    for group in &mut snapshot.audio_mixer.groups {
        group.name = "ñ".repeat(fm_protocol::MAX_AUDIO_LABEL_BYTES / 2);
    }
    snapshot.audio_mixer.labels = worst_labels(ids);
    // A worst-case headphone device escapes every byte (2.53).
    snapshot.audio_mixer.monitor.device =
        Some("~".repeat(fm_protocol::MAX_AUDIO_MONITOR_DEVICE_BYTES));
    // A Master chain and the most per-strip chain records (2.75): the strip
    // chains follow as their own records, so only a count rides the line.
    snapshot.audio_mixer.master_dsp = StripDspPreset::Master.chain();
    snapshot.audio_mixer.strip_dsp_states = fm_protocol::MAX_AUDIO_STRIP_DSP_STATES;
    // Every strip on Master and all seven aux buses, every aux bus with a
    // worst-case name, and every output on an aux bus (2.118).
    for strip in &mut snapshot.input_audio_strips {
        // The snapshot carries the enable mask; levels and taps ride
        // `audio_strip_send_state` records, counted here.
        strip.sends = fm_types::audio_bus::AudioSends::from_mask(
            fm_types::audio_bus::AudioBusMask::from_bits(u8::MAX),
        );
    }
    snapshot.audio_mixer.send_states = fm_protocol::MAX_AUDIO_SEND_STATES;
    for (index, bus) in snapshot.audio_mixer.aux_buses.iter_mut().enumerate() {
        bus.name = worst_label(index);
        bus.gain_millidb = -96_000;
    }
    for output in &mut snapshot.outputs {
        output.aux_bus = Some(bus('G'));
    }
    let line = round_trip(&message);
    assert!(line.len() <= fm_protocol::MAX_LINE_BYTES, "{}", line.len());
}

#[test]
fn applying_operations_updates_the_replicated_mixer() {
    let mut status = AudioMixerStatus::default();
    status.apply(&AudioMixerOperation::SetLabel {
        input: input(5),
        label: "B".into(),
    });
    status.apply(&AudioMixerOperation::SetLabel {
        input: input(2),
        label: "A".into(),
    });
    assert_eq!(status.label(input(2)), "A");
    assert_eq!(
        status
            .labels
            .iter()
            .map(|label| label.input)
            .collect::<Vec<_>>(),
        vec![input(2), input(5)]
    );
    status.apply(&AudioMixerOperation::SetLabel {
        input: input(2),
        label: String::new(),
    });
    assert_eq!(status.label(input(2)), "");
    status.apply(&AudioMixerOperation::RenameGroup {
        group: 3,
        name: "Band".into(),
    });
    status.apply(&AudioMixerOperation::SetGroupLevel {
        group: 3,
        gain_millidb: -3_000,
        muted: true,
    });
    status.apply(&AudioMixerOperation::SetGroupSolo {
        group: 3,
        soloed: true,
    });
    status.apply(&AudioMixerOperation::SetGroupParent {
        group: 3,
        parent: Some(1),
    });
    status.apply(&AudioMixerOperation::SetSoloMode {
        mode: AudioSoloMode::Afl,
    });
    assert_eq!(
        status.groups[2],
        AudioGroupStatus {
            name: "Band".into(),
            gain_millidb: -3_000,
            muted: true,
            soloed: true,
            parent: Some(1),
        }
    );
    assert_eq!(status.solo_mode, AudioSoloMode::Afl);
    assert!(status.solo_mode.listens());
    assert_eq!(status.chain(Some(3)).collect::<Vec<_>>(), [3, 1]);
    assert!(status.group_chain_soloed(Some(3)));
    assert!(!status.group_chain_soloed(Some(1)));
    // Nesting group 1 inside group 3 would close a cycle.
    assert!(!status.parent_is_acyclic(1, Some(3)));
    assert!(status.parent_is_acyclic(1, Some(2)));
    assert!(fm_protocol::fade_to_black_affects(&[output(8)], output(7)));
    assert!(!fm_protocol::fade_to_black_affects(&[output(8)], output(8)));
}

#[test]
fn snapshot_carries_the_monitor_output() {
    let mut status = mixer();
    status.monitor = AudioMonitorStatus {
        device: Some("hw:CARD=Headset,DEV=0".into()),
        gain_millidb: -6_000,
        muted: true,
    };
    let line = round_trip(&snapshot(status, Vec::new()));
    assert!(
        line.contains(
            "\taudio_solo_mode=pfl\taudio_monitor=hw%253ACARD%253DHeadset%252CDEV%253D0%7E-6000%7E1\tinput_audio_labels="
        ),
        "{line}"
    );
    for (from, to) in [
        // The 2.52 snapshot without the field.
        (
            "\taudio_monitor=hw%253ACARD%253DHeadset%252CDEV%253D0%7E-6000%7E1",
            "",
        ),
        ("%7E-6000%7E1\t", "%7E-6000\t"),
        ("%7E-6000%7E1\t", "%7E-6000%7E2\t"),
        ("%7E-6000%7E1\t", "%7E24001%7E1\t"),
        ("%7E-6000%7E1\t", "%7E-6000%7E1%7E0\t"),
        ("hw%253ACARD", "hw%2520CARD"),
    ] {
        assert!(line.contains(from), "{from}");
        assert!(decode_line(&line.replace(from, to)).is_err(), "{to}");
    }
    // An unusable device is refused on encode.
    let mut bad = mixer();
    bad.monitor.device = Some("my headset".into());
    assert!(encode_line(&snapshot(bad, Vec::new())).is_err());
    let mut bad = mixer();
    bad.monitor.gain_millidb = 24_001;
    assert!(encode_line(&snapshot(bad, Vec::new())).is_err());

    // Applying the operations updates the replicated monitor output.
    let mut status = AudioMixerStatus::default();
    status.apply(&AudioMixerOperation::SetMonitorDevice {
        device: Some("default".into()),
    });
    status.apply(&AudioMixerOperation::SetMonitorLevel {
        gain_millidb: -3_000,
        muted: true,
    });
    assert_eq!(
        status.monitor,
        AudioMonitorStatus {
            device: Some("default".into()),
            gain_millidb: -3_000,
            muted: true,
        }
    );
    status.apply(&AudioMixerOperation::SetMonitorDevice { device: None });
    assert_eq!(status.monitor.device, None);
    assert!(fm_protocol::audio_monitor_device_is_valid(
        "hw:CARD=Headset,DEV=0"
    ));
    assert!(!fm_protocol::audio_monitor_device_is_valid(""));
    assert!(!fm_protocol::audio_monitor_device_is_valid("two words"));
    // A leading `-` would reach the daemon's FFmpeg child as an option.
    assert!(!fm_protocol::audio_monitor_device_is_valid("-report"));
    assert!(fm_protocol::audio_monitor_device_is_valid("hw:0-1"));
}

fn gesture(token: u64, continues: bool) -> fm_protocol::GradeGesture {
    fm_protocol::GradeGesture {
        token: core::num::NonZeroU64::new(token).unwrap(),
        continues,
    }
}

#[test]
fn strip_dsp_commands_have_an_exact_wire_form() {
    let voice = StripDspPreset::Voice.chain();
    let code = voice.code();
    assert_eq!(code.len(), STRIP_DSP_CODE_LEN);
    let line = round_trip(&command(CommandPayload::AudioMixer(
        AudioMixerOperation::SetStripDsp {
            target: fm_protocol::AudioDspTarget::Input(input(3)),
            chain: Box::new(voice),
            gesture: None,
        },
    )));
    assert_eq!(
        line,
        format!("{PREFIX}\tpayload=audio_strip_dsp\ttarget=3\tchain={code}\n")
    );
    let master = StripDspPreset::Master.chain();
    let line = round_trip(&command(CommandPayload::AudioMixer(
        AudioMixerOperation::SetStripDsp {
            target: fm_protocol::AudioDspTarget::Master,
            chain: Box::new(master),
            gesture: Some(gesture(41, true)),
        },
    )));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=audio_strip_dsp\ttarget=master\tchain={}\tgesture=41\tcontinues=true\n",
            master.code()
        )
    );
    // The accepted edit is announced with its chain.
    round_trip(&event(EventPayload::AudioMixerChanged(
        AudioMixerOperation::SetStripDsp {
            target: fm_protocol::AudioDspTarget::Input(input(3)),
            chain: Box::new(voice),
            gesture: None,
        },
    )));
}

#[test]
fn strip_dsp_commands_are_strict() {
    let code = StripDspPreset::Voice.chain().code();
    let mut uppercase = code.clone();
    uppercase.replace_range(0..1, "Z");
    for bad in [
        "\tpayload=audio_strip_dsp\ttarget=3".to_owned(),
        format!("\tpayload=audio_strip_dsp\tchain={code}"),
        format!("\tpayload=audio_strip_dsp\ttarget=Master\tchain={code}"),
        format!("\tpayload=audio_strip_dsp\ttarget=03\tchain={code}"),
        format!("\tpayload=audio_strip_dsp\ttarget=0\tchain={code}"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={}", &code[1..]),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={code}A"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={uppercase}"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={code}\tcontinues=true"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={code}\tgesture=4"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={code}\tgesture=0\tcontinues=true"),
        format!("\tpayload=audio_strip_dsp\ttarget=3\tchain={code}\tgesture=4\tcontinues=1"),
    ] {
        assert!(decode_line(&format!("{PREFIX}{bad}\n")).is_err(), "{bad}");
    }
    // The encoder refuses a chain the decoder would refuse.
    let mut broken = StripDsp::DEFAULT;
    broken.compressor.ratio_milli = 0;
    assert!(
        encode_line(&command(CommandPayload::AudioMixer(
            AudioMixerOperation::SetStripDsp {
                target: fm_protocol::AudioDspTarget::Master,
                chain: Box::new(broken),
                gesture: None,
            }
        )))
        .is_err()
    );
}

#[test]
fn aux_chain_commands_name_their_bus_strictly() {
    let music = StripDspPreset::Music.chain();
    let line = round_trip(&command(CommandPayload::AudioMixer(
        AudioMixerOperation::SetStripDsp {
            target: fm_protocol::AudioDspTarget::Aux(bus('C')),
            chain: Box::new(music),
            gesture: Some(gesture(9, false)),
        },
    )));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=audio_strip_dsp\ttarget=aux%3Ac\tchain={}\tgesture=9\tcontinues=false\n",
            music.code()
        )
    );
    // The whole-chain form keeps its own payload for presets and the CLI.
    let line = round_trip(&command(CommandPayload::AudioMixer(
        AudioMixerOperation::SetAuxBusDsp {
            bus: bus('C'),
            chain: Box::new(music),
        },
    )));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=audio_aux_bus_dsp\tbus=C\tchain={}\n",
            music.code()
        )
    );
    // The status follows an accepted aux chain set.
    let mut status = mixer();
    status.apply(&AudioMixerOperation::SetStripDsp {
        target: fm_protocol::AudioDspTarget::Aux(bus('C')),
        chain: Box::new(music),
        gesture: None,
    });
    assert_eq!(status.aux_buses[2].dsp, music);
    for bad in ["aux:", "aux:CC", "aux:1", "aux:h", "AUX:c", "Aux:c"] {
        assert!(
            decode_line(&format!(
                "{PREFIX}\tpayload=audio_strip_dsp\ttarget={bad}\tchain={}\n",
                music.code()
            ))
            .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn strip_dsp_state_records_and_snapshot_fields_round_trip_strictly() {
    let music = StripDspPreset::Music.chain();
    let record = WireMessage::AudioStripDspState(fm_protocol::AudioStripDspStateMessage {
        revision: 12,
        input: input(7),
        chain: music,
    });
    let line = round_trip(&record);
    assert_eq!(
        line,
        format!(
            "audio_strip_dsp_state\trevision=12\tinput=7\tchain={}\n",
            music.code()
        )
    );
    assert!(decode_line(&line.replace("\tinput=7", "\tinput=07")).is_err());
    assert!(decode_line(&line.replace("\tinput=7", "")).is_err());
    assert!(decode_line(&line.replace("\tchain=", "\tchain=x")).is_err());

    let mixer = AudioMixerStatus {
        master_dsp: StripDspPreset::Master.chain(),
        strip_dsp_states: 2,
        ..AudioMixerStatus::default()
    };
    let message = snapshot(mixer, Vec::new());
    let line = round_trip(&message);
    let field = format!(
        "\taudio_master_dsp={}\taudio_strip_dsp_states=2\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
        StripDspPreset::Master.chain().code()
    );
    assert!(line.contains(&field), "{line}");
    for (from, to) in [
        (
            "\taudio_strip_dsp_states=2\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
            "\taudio_strip_dsp_states=257\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
        ),
        (
            "\taudio_strip_dsp_states=2\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
            "\taudio_strip_dsp_states=02\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
        ),
        (
            "\taudio_strip_dsp_states=2\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
            "\t",
        ),
        (
            field.as_str(),
            "\taudio_strip_dsp_states=2\taudio_strip_send_states=0\taudio_aux_buses=Bus%2520A%7E0%7E0%2CBus%2520B%7E0%7E0%2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0%2CBus%2520G%7E0%7E0\t",
        ),
    ] {
        let bad = line.replace(from, to);
        assert_ne!(bad, line, "{from}");
        assert!(decode_line(&bad).is_err(), "{to}");
    }
    let over = AudioMixerStatus {
        strip_dsp_states: fm_protocol::MAX_AUDIO_STRIP_DSP_STATES + 1,
        ..AudioMixerStatus::default()
    };
    assert!(encode_line(&snapshot(over, Vec::new())).is_err());

    // The replicated mixer follows Master edits; strip chains are records.
    let mut status = AudioMixerStatus::default();
    status.apply(&AudioMixerOperation::SetStripDsp {
        target: fm_protocol::AudioDspTarget::Master,
        chain: Box::new(music),
        gesture: None,
    });
    assert_eq!(status.master_dsp, music);
    status.apply(&AudioMixerOperation::SetStripDsp {
        target: fm_protocol::AudioDspTarget::Input(input(1)),
        chain: Box::new(StripDsp::DEFAULT),
        gesture: None,
    });
    assert_eq!(status.master_dsp, music);
}

/// Protocol 2.92 (AU-005): an operator restarts one bus's integrated
/// loudness, loudness range, and maximum true peak. A live operation that
/// changes no mixer status; strict about its target.
#[test]
fn loudness_reset_round_trips_strictly_and_is_live() {
    use fm_protocol::AudioDspTarget;
    for (target, suffix) in [
        (
            AudioDspTarget::Master,
            "\tpayload=audio_loudness_reset\ttarget=master\n",
        ),
        (
            AudioDspTarget::Input(input(12)),
            "\tpayload=audio_loudness_reset\ttarget=12\n",
        ),
    ] {
        let operation = AudioMixerOperation::ResetLoudness { target };
        assert!(operation.is_live());
        assert_eq!(operation.payload(), "audio_loudness_reset");
        let line = round_trip(&command(CommandPayload::AudioMixer(operation.clone())));
        assert_eq!(line, format!("{PREFIX}{suffix}"));
        let line = round_trip(&event(EventPayload::AudioMixerChanged(operation.clone())));
        assert!(
            line.contains("\tevent=audio_mixer_changed\toperation=audio_loudness_reset\ttarget="),
            "{line}"
        );
        let mut status = AudioMixerStatus::default();
        status.apply(&operation);
        assert_eq!(status, AudioMixerStatus::default());
    }
    for bad in [
        "\tpayload=audio_loudness_reset",
        "\tpayload=audio_loudness_reset\ttarget=",
        "\tpayload=audio_loudness_reset\ttarget=Master",
        "\tpayload=audio_loudness_reset\ttarget=0",
        "\tpayload=audio_loudness_reset\ttarget=012",
        "\tpayload=audio_loudness_reset\ttarget=master\textra=1",
    ] {
        assert!(decode_line(&format!("{PREFIX}{bad}\n")).is_err(), "{bad}");
    }
}

fn bus(letter: char) -> fm_types::audio_bus::AuxBusNumber {
    fm_types::audio_bus::AuxBusNumber::from_letter(letter).unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // one operation per aux bus command
fn aux_bus_commands_have_an_exact_wire_form_and_are_strict() {
    for (operation, fields) in [
        (
            AudioMixerOperation::SetAuxBusLevel {
                bus: bus('B'),
                gain_millidb: -6_000,
                muted: true,
            },
            "payload=audio_aux_bus_level\tbus=B\tgain_millidb=-6000\tmuted=1",
        ),
        (
            AudioMixerOperation::SetOutputBus {
                output: output(8),
                bus: Some(bus('G')),
            },
            "payload=audio_output_bus\toutput=8\tbus=G",
        ),
        (
            AudioMixerOperation::SetOutputBus {
                output: output(8),
                bus: None,
            },
            "payload=audio_output_bus\toutput=8\tbus=-",
        ),
        (
            AudioMixerOperation::SetOutputMixMinus {
                output: output(8),
                input: Some(input(7)),
            },
            "payload=audio_output_mix_minus\toutput=8\tinput=7",
        ),
        (
            AudioMixerOperation::SetOutputMixMinus {
                output: output(8),
                input: None,
            },
            "payload=audio_output_mix_minus\toutput=8\tinput=-",
        ),
        (
            AudioMixerOperation::SetAuxBusDevice {
                bus: bus('B'),
                device: Some("plughw:CARD=Headset,DEV=0".into()),
            },
            "payload=audio_aux_bus_device\tbus=B\tdevice=plughw%3ACARD%3DHeadset%2CDEV%3D0",
        ),
        (
            AudioMixerOperation::SetAuxBusDevice {
                bus: bus('B'),
                device: None,
            },
            "payload=audio_aux_bus_device\tbus=B\tdevice=",
        ),
        (
            AudioMixerOperation::RenameAuxBus {
                bus: bus('A'),
                name: "Stage".into(),
            },
            "payload=audio_aux_bus_name\tbus=A\tname=Stage",
        ),
    ] {
        let line = round_trip(&command(CommandPayload::AudioMixer(operation.clone())));
        assert_eq!(line, format!("{PREFIX}\t{fields}\n"));
        round_trip(&event(EventPayload::AudioMixerChanged(operation)));
    }
    assert!(
        AudioMixerOperation::SetAuxBusLevel {
            bus: bus('A'),
            gain_millidb: 0,
            muted: false,
        }
        .is_live()
    );
    assert!(
        !AudioMixerOperation::SetOutputBus {
            output: output(8),
            bus: None,
        }
        .is_live()
    );
    for fields in [
        "payload=audio_aux_bus_level\tbus=H\tgain_millidb=0\tmuted=0",
        "payload=audio_aux_bus_level\tbus=b\tgain_millidb=0\tmuted=0",
        "payload=audio_aux_bus_level\tbus=AB\tgain_millidb=0\tmuted=0",
        "payload=audio_aux_bus_level\tbus=-\tgain_millidb=0\tmuted=0",
        "payload=audio_aux_bus_level\tbus=A\tgain_millidb=24001\tmuted=0",
        "payload=audio_aux_bus_level\tbus=A\tgain_millidb=0\tmuted=2",
        "payload=audio_aux_bus_level\tgain_millidb=0\tmuted=0",
        "payload=audio_output_bus\toutput=0\tbus=A",
        "payload=audio_output_bus\toutput=8\tbus=",
        "payload=audio_output_bus\toutput=8\tbus=M",
        "payload=audio_output_bus\toutput=8",
        "payload=audio_output_mix_minus\toutput=0\tinput=1",
        "payload=audio_output_mix_minus\toutput=8\tinput=",
        "payload=audio_output_mix_minus\toutput=8\tinput=0",
        "payload=audio_output_mix_minus\toutput=8\tinput=01",
        "payload=audio_output_mix_minus\toutput=8",
        "payload=audio_output_mix_minus\tinput=1",
        "payload=audio_aux_bus_device\tbus=H\tdevice=default",
        "payload=audio_aux_bus_device\tbus=B\tdevice=two words",
        "payload=audio_aux_bus_device\tbus=B\tdevice=-report",
        "payload=audio_aux_bus_device\tbus=B",
        "payload=audio_aux_bus_name\tbus=A\tname=%20",
        "payload=audio_aux_bus_name\tbus=Z\tname=Stage",
    ] {
        assert!(
            decode_line(&format!("{PREFIX}\t{fields}\n")).is_err(),
            "{fields}"
        );
    }
}

#[test]
fn strip_sends_ride_the_strip_command_and_snapshot_tuples_strictly() {
    let a = fm_types::audio_bus::AuxBusNumber::new(1).unwrap();
    let sends = fm_types::audio_bus::AudioSends::MASTER.with_aux_send(
        a,
        fm_types::audio_bus::BusSendState {
            enabled: true,
            level_millidb: -6_000,
            tap: fm_types::audio_bus::SendTap::PreFader,
        },
    );
    let line = round_trip(&command(CommandPayload::SetInputAudioStrip {
        input: input(1),
        gain_millidb: 0,
        balance_basis_points: 0,
        pan_basis_points: 0,
        muted: false,
        soloed: false,
        follow_video: true,
        delay_samples: 0,
        sends,
    }));
    assert!(
        line.ends_with("\tdelay_samples=0\tsends=M%2CA%40-6000p\n"),
        "{line}"
    );
    let token = "sends=M%2CA%40-6000p";
    for (from, to) in [
        (token, "sends=A%40-6000p%2CM"),
        (token, "sends="),
        (token, "sends=M%2CH%400"),
        (token, "sends=M%2CA%40"),
        ("\tsends=M%2CA%40-6000p", ""),
    ] {
        let malformed = line.replacen(from, to, 1);
        assert!(decode_line(&malformed).is_err(), "{to}");
    }
    // An aux-only strip and a strip on no bus at all.
    let mut message = snapshot(mixer(), Vec::new());
    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!()
    };
    let g = fm_types::audio_bus::AuxBusNumber::new(7).unwrap();
    snapshot.input_audio_strips[0].sends = fm_types::audio_bus::AudioSends::MASTER
        .with_master(false)
        .with_aux(g, true);
    snapshot.input_audio_strips[1].sends =
        fm_types::audio_bus::AudioSends::MASTER.with_master(false);
    let line = round_trip(&message);
    assert!(line.contains("%3A4%3AG%2C2%3A"), "{line}");
    assert!(line.contains("%3A-%3A-\tdesired_program"), "{line}");
    let malformed = line.replacen("%3A4%3AG%2C", "%3A4%3AGAG%2C", 1);
    assert!(decode_line(&malformed).is_err());
}

#[test]
fn snapshot_carries_aux_buses_and_output_buses_strictly() {
    let mut status = mixer();
    let chain = fm_types::strip_dsp::StripDspPreset::Music.chain();
    status.aux_buses[1] = fm_protocol::AuxBusStatus {
        name: "Stage, left".into(),
        gain_millidb: -3_000,
        muted: true,
        dsp: chain,
        device: None,
        omt: true,
    };
    let mut message = snapshot(status, Vec::new());
    let WireMessage::Snapshot(snapshot) = &mut message else {
        unreachable!()
    };
    snapshot.outputs[0].aux_bus = Some(bus('B'));
    snapshot.audio_mixer.aux_buses[1].device = Some("default".into());
    let line = round_trip(&message);
    assert!(
        line.contains(&format!(
            "\taudio_aux_buses=Bus%2520A%7E0%7E0%2CStage%252C%2520left%7E-3000%7E1%7E{}\
             %2CBus%2520C%7E0%7E0%2CBus%2520D%7E0%7E0%2CBus%2520E%7E0%7E0%2CBus%2520F%7E0%7E0\
             %2CBus%2520G%7E0%7E0\taudio_aux_bus_devices=B%7Edefault\taudio_aux_bus_omt=B\tfade_to_black",
            chain.code()
        )),
        "{line}"
    );
    assert!(
        line.contains("\toutputs=7%7EOutput%25207%7EB%7E-%2C8%7EOutput%25208%7E-%7E-\t"),
        "{line}"
    );
    for (from, to) in [
        ("7%7EOutput%25207%7EB", "7%7EOutput%25207%7EH"),
        ("7%7EOutput%25207%7EB", "7%7EOutput%25207%7EBC"),
        ("7%7EOutput%25207%7EB", "7%7EOutput%25207%7E"),
        ("7%7EOutput%25207%7EB", "7%7EOutput%25207"),
        ("7%7EOutput%25207%7EB", "7%7EOutput%25207%7EB%7EB"),
        ("7%7EOutput%25207%7EB%7E-", "7%7EOutput%25207%7EB%7E0"),
        ("7%7EOutput%25207%7EB%7E-", "7%7EOutput%25207%7EB%7E01"),
        ("7%7EOutput%25207%7EB%7E-", "7%7EOutput%25207%7EB%7E1x"),
        (
            "audio_aux_bus_devices=B%7Edefault",
            "audio_aux_bus_devices=H%7Edefault",
        ),
        (
            "audio_aux_bus_devices=B%7Edefault",
            "audio_aux_bus_devices=B%7E",
        ),
        (
            "audio_aux_bus_devices=B%7Edefault",
            "audio_aux_bus_devices=B%7Edefault%7EC",
        ),
        (
            "audio_aux_bus_devices=B%7Edefault",
            "audio_aux_bus_devices=B%7Edefault%2CB%7Edefault",
        ),
        (
            "audio_aux_bus_devices=B%7Edefault",
            "audio_aux_bus_devices=B%7Etwo%2520words",
        ),
        ("audio_aux_bus_omt=B", "audio_aux_bus_omt=H"),
        ("audio_aux_bus_omt=B", "audio_aux_bus_omt=C%2CB"),
        ("audio_aux_bus_omt=B", "audio_aux_bus_omt=B%2CB"),
        ("audio_aux_bus_omt=B", "audio_aux_bus_omt=B%7E"),
        ("%2CBus%2520G%7E0%7E0\t", "\t"),
        ("Bus%2520C%7E0%7E0", "%2520%7E0%7E0"),
        ("Bus%2520C%7E0%7E0", "Bus%2520C%7E24001%7E0"),
        ("Bus%2520C%7E0%7E0", "Bus%2520C%7E0%7E2"),
    ] {
        let malformed = line.replacen(from, to, 1);
        assert_ne!(malformed, line, "{from}");
        assert!(decode_line(&malformed).is_err(), "{from} -> {to}");
    }
}

#[test]
fn applying_aux_operations_updates_the_replicated_mixer() {
    let mut status = AudioMixerStatus::default();
    status.apply(&AudioMixerOperation::RenameAuxBus {
        bus: bus('C'),
        name: "Band".into(),
    });
    status.apply(&AudioMixerOperation::SetAuxBusLevel {
        bus: bus('C'),
        gain_millidb: -1_500,
        muted: true,
    });
    status.apply(&AudioMixerOperation::SetAuxBusOmt {
        bus: bus('C'),
        omt: true,
    });
    // An output's bus lives in its output status, not the mixer status.
    let before = status.clone();
    status.apply(&AudioMixerOperation::SetOutputBus {
        output: output(3),
        bus: Some(bus('C')),
    });
    assert_eq!(status, before);
    assert_eq!(status.aux_buses[2].name, "Band");
    assert_eq!(status.aux_buses[2].gain_millidb, -1_500);
    assert!(status.aux_buses[2].muted);
    assert!(status.aux_buses[2].omt);
}
