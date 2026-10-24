//! Protocol 2.28 grading: grade commands, `grade_state` records, and
//! `grade_changed` events round-trip exactly and refuse everything outside
//! the contract.

use core::num::{NonZeroU32, NonZeroU128};

use fm_grade::{CubeKind, Grade, GradeLut, builtin_preset};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, GradeChange, GradeEntry,
    GradeStateMessage, MAX_GRADE_LUT_IMPORT_BYTES, MAX_GRADE_NAME_BYTES, MAX_GRADE_STATES,
    ProtocolVersion, WireGradeSource, WireInputId, WireMessage, decode_line, encode_line,
};

fn id(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

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

const PREFIX: &str = "command\tprotocol=2.107\tid=a\tidempotency_key=k";

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

fn event(change: GradeChange) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: cursor(),
        payload: EventPayload::GradeChanged(change),
        edit_history: EditHistoryStatus::EMPTY,
    })
}

fn state(entry: GradeEntry) -> WireMessage {
    WireMessage::GradeState(GradeStateMessage {
        revision: 12,
        entry,
    })
}

/// A grade touching every section, including a LUT reference.
fn rich() -> Grade {
    Grade {
        lut: Some(GradeLut::new("asset://luts/7.cube", 650)),
        exposure_millistops: -1_250,
        ..builtin_preset("punchy").unwrap().grade.clone()
    }
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[((value >> (18 - 6 * index)) & 63) as usize],
            ));
        }
    }
    out
}

#[test]
fn protocol_version_is_2_39() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 107));
}

#[test]
fn identity_grade_and_simple_commands_have_an_exact_wire_form() {
    let identity = "1;0,0;0;0,5000;1000;0,0,0;0,0,0;0,0,0;0,0,0;0:0,10000:10000;0:0,10000:10000;0:0,10000:10000;0:0,10000:10000;;;;-";
    let escaped = identity
        .replace(';', "%3B")
        .replace(',', "%2C")
        .replace(':', "%3A");
    let cases = [
        (
            CommandPayload::SetInputGrade {
                input: input(4),
                source: Some(WireGradeSource::Inline(Box::new(Grade::IDENTITY))),
                gesture: None,
            },
            format!("\tpayload=grade_input_set\tinput=4\tgrade={escaped}\n"),
        ),
        (
            CommandPayload::SetInputGrade {
                input: input(4),
                source: Some(WireGradeSource::Library(id(3))),
                gesture: None,
            },
            "\tpayload=grade_input_set\tinput=4\tlibrary=3\n".to_owned(),
        ),
        (
            CommandPayload::SetInputGrade {
                input: input(4),
                source: None,
                gesture: None,
            },
            "\tpayload=grade_input_clear\tinput=4\n".to_owned(),
        ),
        (
            CommandPayload::SetProgramGrade {
                source: Some(WireGradeSource::Library(id(9))),
                gesture: None,
            },
            "\tpayload=grade_program_set\tlibrary=9\n".to_owned(),
        ),
        (
            CommandPayload::SetProgramGrade {
                source: None,
                gesture: None,
            },
            "\tpayload=grade_program_clear\n".to_owned(),
        ),
        (
            CommandPayload::RemoveLibraryGrade { grade: id(2) },
            "\tpayload=grade_library_remove\tgrade_id=2\n".to_owned(),
        ),
        (
            CommandPayload::RemoveGradeLut { lut: id(5) },
            "\tpayload=grade_lut_remove\tlut_id=5\n".to_owned(),
        ),
        (
            CommandPayload::ImportGradeLut {
                lut: id(5),
                name: "Film".into(),
                data: "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n".into(),
            },
            format!(
                "\tpayload=grade_lut_import\tlut_id=5\tname=Film\tdata={}\n",
                base64url(b"LUT_1D_SIZE 2\n0 0 0\n1 1 1\n")
            ),
        ),
    ];
    for (payload, tail) in cases {
        let message = command(payload);
        let line = round_trip(&message);
        assert_eq!(line, format!("{PREFIX}{tail}"));
    }
}

#[test]
fn every_grade_command_state_and_change_round_trips() {
    let messages = vec![
        command(CommandPayload::SetInputGrade {
            input: input(u128::MAX),
            source: Some(WireGradeSource::Inline(Box::new(rich()))),
            gesture: None,
        }),
        command(CommandPayload::SetProgramGrade {
            source: Some(WireGradeSource::Inline(Box::new(rich()))),
            gesture: None,
        }),
        command(CommandPayload::AddLibraryGrade {
            grade: id(u32::MAX),
            name: "Warm; look, 2".into(),
            value: Box::new(rich()),
            index: Some(255),
        }),
        command(CommandPayload::AddLibraryGrade {
            grade: id(1),
            name: "Cool".into(),
            value: Box::new(builtin_preset("cool").unwrap().grade.clone()),
            index: None,
        }),
        command(CommandPayload::UpdateLibraryGrade {
            grade: id(1),
            name: "Faded".into(),
            value: Box::new(builtin_preset("faded").unwrap().grade.clone()),
        }),
        state(GradeEntry::Lut {
            lut: id(7),
            name: "Film".into(),
            kind: CubeKind::ThreeD,
            size: 65,
        }),
        state(GradeEntry::Library {
            grade: id(1),
            name: "Bw".into(),
            value: Box::new(builtin_preset("bw").unwrap().grade.clone()),
        }),
        state(GradeEntry::Input {
            input: input(3),
            source: WireGradeSource::Library(id(1)),
        }),
        state(GradeEntry::Program {
            source: WireGradeSource::Inline(Box::new(rich())),
        }),
        event(GradeChange::LutImported {
            lut: id(7),
            name: "Film".into(),
            kind: CubeKind::OneD,
            size: 65_536,
        }),
        event(GradeChange::LutRemoved { lut: id(7) }),
        event(GradeChange::Library {
            grade: id(1),
            index: 0,
            name: "Bw".into(),
            value: Box::new(rich()),
        }),
        event(GradeChange::LibraryRemoved { grade: id(1) }),
        event(GradeChange::Input {
            input: input(3),
            source: None,
        }),
        event(GradeChange::Input {
            input: input(3),
            source: Some(WireGradeSource::Library(id(4))),
        }),
        event(GradeChange::Program {
            source: Some(WireGradeSource::Inline(Box::new(Grade {
                enabled: false,
                ..rich()
            }))),
        }),
        event(GradeChange::Program { source: None }),
    ];
    for message in &messages {
        round_trip(message);
    }
}

#[test]
fn lut_import_is_capped_and_strictly_base64url() {
    let at_cap = "#".repeat(MAX_GRADE_LUT_IMPORT_BYTES);
    let message = command(CommandPayload::ImportGradeLut {
        lut: id(1),
        name: "Max".into(),
        data: at_cap.clone(),
    });
    let line = round_trip(&message);
    assert!(line.len() <= fm_protocol::MAX_LINE_BYTES);

    // The encoder refuses one byte more; so does the decoder.
    let over = command(CommandPayload::ImportGradeLut {
        lut: id(1),
        name: "Max".into(),
        data: format!("{at_cap}#"),
    });
    assert!(encode_line(&over).is_err());
    let data = base64url(format!("{at_cap}#").as_bytes());
    let line = format!("{PREFIX}\tpayload=grade_lut_import\tlut_id=1\tname=Max\tdata={data}\n");
    assert!(decode_line(&line).is_err());

    for data in [
        // Padding, a foreign alphabet character, a non-canonical trailing
        // bit, an impossible length, and invalid UTF-8.
        "TFVU%3D".to_owned(),
        "TF%2BU".to_owned(),
        "TFVV".to_owned() + "B",
        "TFVUX".to_owned(),
        "TFV".to_owned(),
        base64url(&[0xff, 0xfe]),
    ] {
        let line = format!("{PREFIX}\tpayload=grade_lut_import\tlut_id=1\tname=X\tdata={data}\n");
        assert!(decode_line(&line).is_err(), "{data}");
    }
}

#[test]
fn grade_sources_need_exactly_one_form() {
    let identity = encode_line(&command(CommandPayload::SetProgramGrade {
        source: Some(WireGradeSource::Inline(Box::new(Grade::IDENTITY))),
        gesture: None,
    }))
    .unwrap();
    let grade = identity
        .trim_end()
        .split('\t')
        .find(|field| field.starts_with("grade="))
        .unwrap()
        .to_owned();
    for tail in [
        // Both forms.
        format!("\tpayload=grade_program_set\tlibrary=1\t{grade}\n"),
        // Neither form on a set.
        "\tpayload=grade_program_set\n".to_owned(),
        // A source on a clear.
        "\tpayload=grade_input_clear\tinput=1\tlibrary=1\n".to_owned(),
        // Non-canonical and zero ids.
        "\tpayload=grade_program_set\tlibrary=01\n".to_owned(),
        "\tpayload=grade_program_set\tlibrary=0\n".to_owned(),
        "\tpayload=grade_library_remove\tgrade_id=4294967296\n".to_owned(),
        "\tpayload=grade_lut_remove\tlut_id=-1\n".to_owned(),
        // An unknown grade payload.
        "\tpayload=grade_everything\n".to_owned(),
    ] {
        let line = format!("{PREFIX}{tail}");
        assert!(decode_line(&line).is_err(), "{line}");
    }
}

#[test]
fn grade_values_are_strictly_bounded_and_canonical() {
    let identity = "1;0,0;0;0,5000;1000;0,0,0;0,0,0;0,0,0;0,0,0;0:0,10000:10000;0:0,10000:10000;0:0,10000:10000;0:0,10000:10000;;;;-";
    let line = |grade: &str| {
        let grade = grade
            .replace(';', "%3B")
            .replace(',', "%2C")
            .replace(':', "%3A");
        format!("{PREFIX}\tpayload=grade_program_set\tgrade={grade}\n")
    };
    assert!(decode_line(&line(identity)).is_ok());
    for bad in [
        // Saturation above 3000, exposure beyond four stops.
        identity.replacen(";1000;", ";3001;", 1),
        identity.replacen(";0;0,5000", ";4001;0,5000", 1),
        // Negative zero and a leading zero.
        identity.replacen(";0;0,5000", ";-0;0,5000", 1),
        identity.replacen(";0;0,5000", ";00;0,5000", 1),
        // Too few and too many sections.
        identity.rsplit_once(';').unwrap().0.to_owned(),
        format!("{identity};-"),
        // Enabled must be 0 or 1.
        identity.replacen('1', "2", 1),
        // A LUT reference with a zero id or strength above 1000.
        identity.replace(";-", ";0:500"),
        identity.replace(";-", ";7:1001"),
        // A curve that is not increasing in x.
        identity.replacen("0:0,10000:10000", "10000:10000,0:0", 1),
    ] {
        assert!(decode_line(&line(&bad)).is_err(), "{bad}");
    }

    // The encoder refuses a grade the decoder would refuse.
    let invalid = Grade {
        saturation_permille: 3_001,
        ..Grade::IDENTITY
    };
    assert!(
        encode_line(&command(CommandPayload::SetProgramGrade {
            source: Some(WireGradeSource::Inline(Box::new(invalid))),
            gesture: None,
        }))
        .is_err()
    );
    // A LUT reference must be a canonical imported-LUT URI.
    let foreign = Grade {
        lut: Some(GradeLut::new("asset://other/7.cube", 500)),
        ..Grade::IDENTITY
    };
    assert!(
        encode_line(&command(CommandPayload::SetProgramGrade {
            source: Some(WireGradeSource::Inline(Box::new(foreign))),
            gesture: None,
        }))
        .is_err()
    );
}

#[test]
fn names_indices_and_lut_metadata_are_bounded() {
    let grade = Box::new(Grade::IDENTITY);
    for (name, ok) in [
        ("A".repeat(MAX_GRADE_NAME_BYTES), true),
        ("A".repeat(MAX_GRADE_NAME_BYTES + 1), false),
        ("   ".to_owned(), false),
        ("Tab\there".to_owned(), false),
    ] {
        let result = encode_line(&command(CommandPayload::AddLibraryGrade {
            grade: id(1),
            name: name.clone(),
            value: grade.clone(),
            index: None,
        }));
        assert_eq!(result.is_ok(), ok, "{name:?}");
    }
    assert!(
        encode_line(&command(CommandPayload::AddLibraryGrade {
            grade: id(1),
            name: "A".into(),
            value: grade,
            index: Some(256),
        }))
        .is_err()
    );
    for (kind, size, ok) in [
        (CubeKind::ThreeD, 2, true),
        (CubeKind::ThreeD, 65, true),
        (CubeKind::ThreeD, 66, false),
        (CubeKind::ThreeD, 1, false),
        (CubeKind::OneD, 65_536, true),
        (CubeKind::OneD, 65_537, false),
    ] {
        let result = encode_line(&state(GradeEntry::Lut {
            lut: id(1),
            name: "L".into(),
            kind,
            size,
        }));
        assert_eq!(result.is_ok(), ok, "{kind:?} {size}");
    }
    let line = "grade_state\trevision=1\tentry=lut\tlut_id=1\tname=L\tkind=2d\tsize=2\n";
    assert!(decode_line(line).is_err());
    let line = "grade_state\trevision=1\tentry=scene\n";
    assert!(decode_line(line).is_err());
}

#[test]
fn snapshot_grade_state_count_is_bounded() {
    assert_eq!(MAX_GRADE_STATES, 64 + 256 + 256 + 256 + 1);
    assert_eq!(fm_protocol::MAX_GRADE_LAYERS, 256);
    let fits = |count: u16| -> Result<(), CodecError> {
        let WireMessage::Snapshot(mut snapshot) = sample_snapshot() else {
            unreachable!()
        };
        snapshot.grade_states = count;
        let message = WireMessage::Snapshot(snapshot);
        let line = encode_line(&message)?;
        assert_eq!(decode_line(&line)?, message);
        Ok(())
    };
    fits(0).unwrap();
    assert!(fits(MAX_GRADE_STATES).is_ok());
    assert!(fits(MAX_GRADE_STATES + 1).is_err());

    let line = encode_line(&sample_snapshot()).unwrap();
    assert!(
        line.ends_with(&format!("\tgrade_states=0\tanimation_preset_states=0\ttransition_favorites=fade%3A30%3Bwipe%3A30%3Bfly%3A30%3Bcube%3A30\tmedia_states=0\tpaged_states=0\tcue_list_states=0\tscript_states=0\tshortcut_states=0\tstill_states=0\tmedia_list_states=0\tinput_categories=\taudio_groups=Group%25201%7E0%7E0%7E0%7E-%2CGroup%25202%7E0%7E0%7E0%7E-%2CGroup%25203%7E0%7E0%7E0%7E-%2CGroup%25204%7E0%7E0%7E0%7E-\taudio_solo_mode=sip\taudio_monitor=%7E0%7E0\tinput_audio_labels=\taudio_master_dsp={}\taudio_strip_dsp_states=0\tfade_to_black_excluded_outputs=\tfade_to_black_output_durations=\tfade_to_black_curve=linear\tdisplay_outputs=\tdisplay_output_states=\tnetwork_streams=\tcapture_inputs=\tsystem_audio_inputs=\tscreen_capture_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\ttransitions_in_flight=\n", fm_types::strip_dsp::StripDsp::DEFAULT.code())),
        "{line}"
    );
    let over = line.replace(
        "\tgrade_states=0\t",
        &format!("\tgrade_states={}\t", MAX_GRADE_STATES + 1),
    );
    assert!(decode_line(&over).is_err());
    let missing = line.replace("\tgrade_states=0\t", "\t");
    assert!(decode_line(&missing).is_err());
}

fn sample_snapshot() -> WireMessage {
    use fm_protocol::{
        EditLockStatus, FadeToBlackPosition, FadeToBlackState, InputAudioStripStatus, InputStatus,
        ManualTransitionStatus, OverlayStatus, RecordingStatus, SnapshotMessage, WireRecorderSlot,
    };
    WireMessage::Snapshot(SnapshotMessage {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs: Vec::new(),
        screen_capture_inputs: Vec::new(),
        capture_inputs: Vec::new(),
        engine: cursor().engine,
        revision: 12,
        show_name: "Show".to_owned(),
        inputs: vec![InputStatus {
            input: input(1),
            name: "Cam".into(),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        }],
        outputs: Vec::new(),
        input_audio_strips: vec![InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: input(1),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        }],
        desired_program: input(1),
        desired_preview: input(1),
        realized_program: input(1),
        realized_preview: input(1),
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
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
        transitions_in_flight: Vec::new(),
    })
}

fn layer(value: u32) -> fm_protocol::WireSceneLayerId {
    fm_protocol::WireSceneLayerId::new(id(value))
}

fn scene(value: u128) -> fm_protocol::WireSceneId {
    fm_protocol::WireSceneId::new(core::num::NonZeroU128::new(value).unwrap())
}

/// Protocol 2.35: a scene layer's grade is set and cleared by scene id
/// (whether or not a Scene input shows the scene) and replicated
/// (`entry=layer`, `change=layer`) by scene.
#[test]
fn scene_layer_grades_have_an_exact_wire_form() {
    let cases = [
        (
            command(CommandPayload::SetLayerGrade {
                scene: scene(4),
                layer: layer(2),
                source: Some(WireGradeSource::Library(id(3))),
                gesture: None,
            }),
            format!("{PREFIX}\tpayload=grade_layer_set\tscene=4\tlayer=2\tlibrary=3\n"),
        ),
        (
            command(CommandPayload::SetLayerGrade {
                scene: scene(4),
                layer: layer(2),
                source: None,
                gesture: None,
            }),
            format!("{PREFIX}\tpayload=grade_layer_clear\tscene=4\tlayer=2\n"),
        ),
    ];
    for (message, expected) in cases {
        assert_eq!(round_trip(&message), expected);
    }
    let state_line = round_trip(&state(GradeEntry::Layer {
        scene: scene(9),
        layer: layer(7),
        source: WireGradeSource::Library(id(1)),
    }));
    assert!(
        state_line.ends_with("\tentry=layer\tscene=9\tlayer=7\tlibrary=1\n"),
        "{state_line}"
    );
    let cleared = round_trip(&event(GradeChange::Layer {
        scene: scene(3),
        layer: layer(7),
        source: None,
    }));
    assert!(
        cleared.contains("\tchange=layer\tscene=3\tlayer=7"),
        "{cleared}"
    );
    for message in [
        command(CommandPayload::SetLayerGrade {
            scene: scene(u128::MAX),
            layer: layer(u32::MAX),
            source: Some(WireGradeSource::Inline(Box::new(rich()))),
            gesture: None,
        }),
        state(GradeEntry::Layer {
            scene: scene(1),
            layer: layer(1),
            source: WireGradeSource::Inline(Box::new(rich())),
        }),
        event(GradeChange::Layer {
            scene: scene(u128::MAX),
            layer: layer(1),
            source: Some(WireGradeSource::Inline(Box::new(rich()))),
        }),
    ] {
        round_trip(&message);
    }

    // Scene and layer ids are canonical and nonzero; a set needs exactly
    // one source; a clear carries none; the 2.33 `input` addressing is gone.
    for tail in [
        "\tpayload=grade_layer_set\tscene=4\tlayer=0\tlibrary=3\n",
        "\tpayload=grade_layer_set\tscene=4\tlayer=02\tlibrary=3\n",
        "\tpayload=grade_layer_set\tscene=0\tlayer=2\tlibrary=3\n",
        "\tpayload=grade_layer_set\tscene=04\tlayer=2\tlibrary=3\n",
        "\tpayload=grade_layer_set\tscene=4\tlibrary=3\n",
        "\tpayload=grade_layer_set\tscene=4\tlayer=2\n",
        "\tpayload=grade_layer_set\tlayer=2\tlibrary=3\n",
        "\tpayload=grade_layer_set\tinput=4\tlayer=2\tlibrary=3\n",
        "\tpayload=grade_layer_clear\tscene=4\tlayer=2\tlibrary=3\n",
        "\tpayload=grade_layer_clear\tscene=4\tlayer=4294967296\n",
    ] {
        let line = format!("{PREFIX}{tail}");
        assert!(decode_line(&line).is_err(), "{line}");
    }
    for (from, to) in [
        ("scene=9", "scene=0"),
        ("scene=9", "inputs=9"),
        ("\tscene=9", ""),
        ("layer=7", "layer=0"),
    ] {
        let bad = state_line.replacen(from, to, 1);
        assert_ne!(bad, state_line);
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
}

/// Protocol 2.56: grade sets and clears carry the drag they belong to as
/// `gesture` (canonical nonzero) and `continues` (`true`/`false`), both or
/// neither.
#[test]
fn grade_drags_carry_their_gesture_exactly() {
    let gesture = |token: u64, continues| {
        Some(fm_protocol::GradeGesture {
            token: core::num::NonZeroU64::new(token).unwrap(),
            continues,
        })
    };
    let cases = [
        (
            CommandPayload::SetInputGrade {
                input: input(4),
                source: Some(WireGradeSource::Library(id(3))),
                gesture: gesture(18_446_744_073_709_551_615, false),
            },
            "\tpayload=grade_input_set\tinput=4\tlibrary=3\tgesture=18446744073709551615\tcontinues=false\n",
        ),
        (
            CommandPayload::SetInputGrade {
                input: input(4),
                source: None,
                gesture: gesture(7, true),
            },
            "\tpayload=grade_input_clear\tinput=4\tgesture=7\tcontinues=true\n",
        ),
        (
            CommandPayload::SetLayerGrade {
                scene: scene(9),
                layer: layer(2),
                source: Some(WireGradeSource::Library(id(1))),
                gesture: gesture(8, true),
            },
            "\tpayload=grade_layer_set\tscene=9\tlayer=2\tlibrary=1\tgesture=8\tcontinues=true\n",
        ),
        (
            CommandPayload::SetLayerGrade {
                scene: scene(9),
                layer: layer(2),
                source: None,
                gesture: gesture(8, false),
            },
            "\tpayload=grade_layer_clear\tscene=9\tlayer=2\tgesture=8\tcontinues=false\n",
        ),
        (
            CommandPayload::SetProgramGrade {
                source: Some(WireGradeSource::Library(id(9))),
                gesture: gesture(1, true),
            },
            "\tpayload=grade_program_set\tlibrary=9\tgesture=1\tcontinues=true\n",
        ),
        (
            CommandPayload::SetProgramGrade {
                source: None,
                gesture: gesture(2, false),
            },
            "\tpayload=grade_program_clear\tgesture=2\tcontinues=false\n",
        ),
    ];
    for (payload, tail) in cases {
        let line = round_trip(&command(payload));
        assert_eq!(line, format!("{PREFIX}{tail}"));
    }
    for tail in [
        // A zero, non-canonical, or out-of-range token.
        "\tpayload=grade_program_clear\tgesture=0\tcontinues=false\n",
        "\tpayload=grade_program_clear\tgesture=07\tcontinues=false\n",
        "\tpayload=grade_program_clear\tgesture=18446744073709551616\tcontinues=false\n",
        // One without the other.
        "\tpayload=grade_program_clear\tgesture=7\n",
        "\tpayload=grade_program_clear\tcontinues=true\n",
        "\tpayload=grade_input_set\tinput=4\tlibrary=3\tcontinues=false\n",
        // Not a boolean.
        "\tpayload=grade_layer_clear\tscene=9\tlayer=2\tgesture=7\tcontinues=1\n",
        // Library edits take no gesture.
        "\tpayload=grade_library_remove\tgrade_id=2\tgesture=7\tcontinues=true\n",
    ] {
        let line = format!("{PREFIX}{tail}");
        assert!(decode_line(&line).is_err(), "{line}");
    }
}
