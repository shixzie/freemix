//! Protocol 2.40 mixer strip labels, VCA-style strip groups, mono pan, and
//! the Fade-to-Black output selection (AU-001, SW-004): the commands, the
//! `audio_mixer_changed` and `fade_to_black_output_changed` durable events,
//! and the snapshot fields — exactly, and strictly.

use fm_protocol::{
    AudioGroupStatus, AudioMixerOperation, AudioMixerStatus, CURRENT_PROTOCOL_VERSION,
    CommandMessage, CommandPayload, EditHistoryStatus, EditLockStatus, EngineIdentity, EventCursor,
    EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState, InputAudioLabel,
    InputAudioStripStatus, InputStatus, ManualTransitionStatus, OverlayStatus, ProtocolVersion,
    RecordingStatus, SnapshotMessage, WireInputId, WireMessage, WireOutputId, WireRecorderSlot,
    decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.40\tid=a\tidempotency_key=k";

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
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        audio_mixer,
        fade_to_black_excluded_outputs: excluded,
    })
}

fn mixer() -> AudioMixerStatus {
    let mut status = AudioMixerStatus::default();
    status.groups[1] = AudioGroupStatus {
        name: "Mics, wireless".into(),
        gain_millidb: -12_000,
        muted: true,
    };
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
fn the_current_protocol_is_2_40() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 40));
}

#[test]
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
fn snapshot_carries_groups_labels_strip_groups_and_ftb_exclusions() {
    let line = round_trip(&snapshot(mixer(), vec![output(8)]));
    assert!(
        line.contains("\tinput_audio_strips=1%3A-6000%3A2500%3A-10000%3A0%3A0%3A1%3A0%3A4%2C2%3A-6000%3A2500%3A0%3A0%3A0%3A1%3A0%3A-\t"),
        "{line}"
    );
    assert!(
        line.ends_with(
            "\taudio_groups=Group%25201%7E0%7E0%2CMics%252C%2520wireless%7E-12000%7E1%2CGroup%25203%7E0%7E0%2CGroup%25204%7E0%7E0\tinput_audio_labels=1%7EHost%257Emic%2C2%7EGuest\tfade_to_black_excluded_outputs=8\n"
        ),
        "{line}"
    );
    // Output ids span the full 128-bit range.
    let wide = 18_446_744_073_709_552_017_u128;
    let line = round_trip(&snapshot(
        AudioMixerStatus::default(),
        vec![output(8), output(wide)],
    ));
    assert!(
        line.ends_with(&format!("\tfade_to_black_excluded_outputs=8%2C{wide}\n")),
        "{line}"
    );
    let line = round_trip(&snapshot(AudioMixerStatus::default(), Vec::new()));
    assert!(
        line.ends_with("\tinput_audio_labels=\tfade_to_black_excluded_outputs=\n"),
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
            "input_audio_labels=1%7EHost%257Emic%2C2%7EGuest",
            "input_audio_labels=2%7EGuest%2C1%7EHost",
        ),
        (
            "input_audio_labels=1%7EHost%257Emic%2C2%7EGuest",
            "input_audio_labels=1%7E",
        ),
        ("%2CGroup%25204%7E0%7E0\t", "\t"),
        (
            "%2CGroup%25204%7E0%7E0\t",
            "%2CGroup%25204%7E0%7E0%2CGroup%25205%7E0%7E0\t",
        ),
        ("Group%25203%7E0%7E0", "%7E0%7E0"),
    ] {
        assert!(valid.contains(from), "{from}");
        let malformed = valid.replace(from, to);
        assert!(decode_line(&malformed).is_err(), "{to}");
    }
    let missing = valid.replace("\tfade_to_black_excluded_outputs=7%2C8", "");
    assert!(decode_line(&missing).is_err());
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
    assert_eq!(
        status.groups[2],
        AudioGroupStatus {
            name: "Band".into(),
            gain_millidb: -3_000,
            muted: true,
        }
    );
    assert!(fm_protocol::fade_to_black_affects(&[output(8)], output(7)));
    assert!(!fm_protocol::fade_to_black_affects(&[output(8)], output(8)));
}
