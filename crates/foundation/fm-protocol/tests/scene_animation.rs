//! Protocol 2.31 scene animation and 2.32 scene previews: the three scene
//! edit commands, the `scene_animation_state` and `scene_layout_state`
//! records, the `scene_animation_changed` event, and the
//! `scene_preview_request` / `scene_preview_frame` pair round-trip exactly in
//! the title clip encoding (keyed by nonzero `u32` layer ids) and refuse
//! everything outside the contract, including colour tracks.

use core::num::{NonZeroU32, NonZeroU128};

use fm_animation::{
    AnimationPreset, Clip, Easing, Keyframe, MAX_PHASE_FRAMES, Phase, Property, Rgba, Timebase,
    Track, TrackKey, Value,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_LINE_BYTES,
    MAX_SCENE_ANIMATION_BYTES, MAX_SCENE_CANVAS_SIDE, MAX_SCENE_LAYER_NAME_BYTES,
    MAX_SCENE_LAYOUT_LAYERS, MAX_SCENE_NAME_BYTES, ProtocolVersion, SceneAnimationStateMessage,
    SceneLayoutStateMessage, WireInputId, WireMessage, WireSceneClip, WireSceneId,
    WireSceneLayerId, WireSceneLayerOutline, WireSceneLayerSource, WireSceneLayout, decode_line,
    encode_line, scene_clip_wire_bytes,
};
use fm_types::FrameRate;

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn layer(value: u32) -> WireSceneLayerId {
    WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
}

fn timebase() -> Timebase {
    Timebase::new(FrameRate::new(25, 1).unwrap())
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

const PREFIX: &str = "command\tprotocol=2.98\tid=a\tidempotency_key=k";

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(word >> (18 - 6 * index) & 0x3f) as usize],
            ));
        }
    }
    out
}

/// Layer 2 slides in from x -100; layer `u32::MAX` fades out.
fn clip() -> WireSceneClip {
    Clip::from_tracks(
        timebase(),
        10,
        5,
        vec![
            Track::new(
                TrackKey::new(layer(2), Phase::In, Property::PositionX),
                vec![
                    Keyframe::new(0, Value::Scalar(-100), Easing::Linear),
                    Keyframe::new(10, Value::Scalar(0), Easing::Linear),
                ],
            ),
            Track::new(
                TrackKey::new(layer(u32::MAX), Phase::Out, Property::Opacity),
                vec![Keyframe::new(5, Value::Scalar(0), Easing::Step)],
            ),
        ],
    )
    .unwrap()
}

/// The exact binary encoding of [`clip`]: the title clip encoding with
/// layer ids as targets.
fn clip_bytes() -> Vec<u8> {
    let mut bytes = vec![25, 1, 10, 5, 2];
    // layer 2, In, position-x (0), two keys: frame 0 zigzag(-100) = 199
    // (0xc7 0x01) linear (0); frame 10, 0, linear.
    bytes.extend_from_slice(&[2, 0, 0, 2, 0, 0xc7, 0x01, 0, 10, 0, 0]);
    // layer u32::MAX: five bytes; Out, opacity (5), one key: frame 5, 0,
    // step (1).
    bytes.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f, 1, 5, 1, 5, 0, 1]);
    bytes
}

#[test]
fn protocol_is_2_39() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 98));
}

#[test]
fn scene_clips_use_the_title_encoding_keyed_by_layer() {
    let encoded = base64url(&clip_bytes());
    assert_eq!(scene_clip_wire_bytes(&clip()), clip_bytes().len());
    const { assert!(MAX_SCENE_ANIMATION_BYTES <= 36 * 1024) };
    let line = round_trip(&command(CommandPayload::SetSceneAnimation {
        input: input(7),
        base_revision: 3,
        clip: Box::new(clip()),
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=scene_animation_set\tinput=7\tbase_revision=3\tclip={encoded}\n"
        )
    );

    // A layer id above u32::MAX is refused.
    let mut wide = vec![25, 1, 1, 0, 1];
    wide.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x10, 0, 0, 1, 0, 0, 0]);
    // Layer 0 is refused.
    let zero = vec![25, 1, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0];
    // Colour is refused for scene layers.
    let colour = vec![25, 1, 1, 0, 1, 1, 0, 10, 1, 0, 1, 2, 3, 4, 0];
    for bytes in [wide, zero, colour] {
        let line = format!(
            "{PREFIX}\tpayload=scene_animation_set\tinput=7\tbase_revision=3\tclip={}\n",
            base64url(&bytes)
        );
        assert!(
            matches!(
                decode_line(&line),
                Err(CodecError::InvalidField { field: "clip", .. })
            ),
            "{bytes:?}"
        );
    }
    let coloured = Clip::from_tracks(
        timebase(),
        1,
        0,
        vec![Track::new(
            TrackKey::new(layer(1), Phase::In, Property::Color),
            vec![Keyframe::new(
                0,
                Value::Color(Rgba::new(1, 2, 3, 4)),
                Easing::Linear,
            )],
        )],
    )
    .unwrap();
    assert!(matches!(
        encode_line(&command(CommandPayload::SetSceneAnimation {
            input: input(7),
            base_revision: 3,
            clip: Box::new(coloured),
        })),
        Err(CodecError::InvalidField { field: "clip", .. })
    ));
}

#[test]
fn preset_and_stretch_commands_are_exact() {
    let line = round_trip(&command(CommandPayload::ApplySceneAnimationPreset {
        input: input(7),
        base_revision: 3,
        layer: layer(12),
        phase: Phase::Out,
        preset: AnimationPreset::SlideFromLeft,
        frames: 15,
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=scene_animation_preset\tinput=7\tbase_revision=3\tlayer=12\t\
             phase=out\tpreset=slide-from-left\tframes=15\n"
        )
    );
    let line = round_trip(&command(CommandPayload::StretchSceneAnimation {
        input: input(7),
        base_revision: 3,
        phase: Phase::In,
        frames: MAX_PHASE_FRAMES,
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=scene_animation_stretch\tinput=7\tbase_revision=3\tphase=in\t\
             frames=36000\n"
        )
    );
    for (bad, field) in [
        ("layer=0\tphase=in\tpreset=fade\tframes=1", "layer"),
        ("layer=4294967296\tphase=in\tpreset=fade\tframes=1", "layer"),
        ("layer=01\tphase=in\tpreset=fade\tframes=1", "layer"),
        ("layer=1\tphase=hold\tpreset=fade\tframes=1", "phase"),
        ("layer=1\tphase=in\tpreset=spin\tframes=1", "preset"),
        ("layer=1\tphase=in\tpreset=fade\tframes=36001", "frames"),
    ] {
        let line =
            format!("{PREFIX}\tpayload=scene_animation_preset\tinput=7\tbase_revision=3\t{bad}\n");
        match decode_line(&line) {
            Err(CodecError::InvalidField { field: actual, .. }) => assert_eq!(actual, field),
            other => panic!("{bad}: {other:?}"),
        }
    }
    let unknown = format!("{PREFIX}\tpayload=scene_animation_spin\tinput=7\tbase_revision=3\n");
    assert!(decode_line(&unknown).is_err());
}

#[test]
fn state_records_and_change_events_round_trip() {
    let encoded = base64url(&clip_bytes());
    let line = round_trip(&WireMessage::SceneAnimationState(
        SceneAnimationStateMessage {
            revision: 12,
            input: input(7),
            scene: scene(3),
            animation_revision: 5,
            clip: Box::new(clip()),
        },
    ));
    assert_eq!(
        line,
        format!(
            "scene_animation_state\trevision=12\tinput=7\tscene=3\tanimation_revision=5\t\
             clip={encoded}\n"
        )
    );
    // Protocol 2.35: the shown scene is required and canonical.
    for bad in [
        line.replace("\tscene=3", ""),
        line.replace("scene=3", "scene=0"),
        line.replace("scene=3", "scene=03"),
    ] {
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
    let event = |inputs: Vec<WireInputId>| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "engine-a".to_owned(),
                    state_epoch: 7,
                    log_id: "log-a".to_owned(),
                },
                revision: 12,
            },
            payload: EventPayload::SceneAnimationChanged {
                inputs,
                animation_revision: 6,
                clip: Box::new(clip()),
            },
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                redo_depth: 0,
                next_undo: Some(12),
                next_redo: None,
            },
        })
    };
    let line = round_trip(&event(vec![input(7), input(9)]));
    assert!(line.contains(&format!(
        "\tevent=scene_animation_changed\tinputs=7%2C9\tanimation_revision=6\tclip={encoded}\t"
    )));
    for inputs in [Vec::new(), vec![input(9), input(7)]] {
        assert!(matches!(
            encode_line(&event(inputs)),
            Err(CodecError::InvalidField {
                field: "inputs",
                ..
            })
        ));
    }
    assert!(decode_line(&line.replace("inputs=7%2C9", "inputs=9%2C7")).is_err());
}

fn scene(value: u128) -> WireSceneId {
    WireSceneId::new(core::num::NonZeroU128::new(value).unwrap())
}

/// An enabled layer "Cam" drawing input 4 at (-2, 5) sized 300x40, turned
/// once.
fn cam() -> WireSceneLayerOutline {
    WireSceneLayerOutline {
        id: layer(1),
        name: "Cam".into(),
        source: WireSceneLayerSource::Input(input(4)),
        x: -2,
        y: 5,
        width: 300,
        height: 40,
        quarter_turns: 1,
        opacity: 200,
        enabled: true,
        tilt_millidegrees: 0,
        anchor_x: 5_000,
        anchor_y: 5_000,
        z_order: 0,
        crop: None,
        key: None,
        effects: Vec::new(),
        mask: None,
    }
}

const CAM_BYTES: [u8; 22] = [
    1, // id
    1, // flags: enabled
    1, // quarter turns
    3, b'C', b'a', b'm', // name
    0, 4,  // source: input 4
    3,  // x = -2, zigzag
    10, // y = 5, zigzag
    0xAC, 0x02, // width 300
    40,   // height
    200,  // opacity
    0,    // tilt (Protocol 2.39)
    0x88, 0x27, // anchor x 5000
    0x88, 0x27, // anchor y 5000
    0,    // z-order
    0,    // no effects (Protocol 2.43)
];

fn layout_state(layers: Vec<WireSceneLayerOutline>) -> WireMessage {
    WireMessage::SceneLayoutState(SceneLayoutStateMessage {
        revision: 12,
        scene: scene(3),
        name: "Studio".into(),
        layout: Box::new(WireSceneLayout {
            width: 1_920,
            height: 1_080,
            layers,
        }),
    })
}

#[test]
fn layout_records_are_exact_and_strict() {
    let mut bytes = vec![1];
    bytes.extend_from_slice(&CAM_BYTES);
    let line = round_trip(&layout_state(vec![cam()]));
    assert_eq!(
        line,
        format!(
            "scene_layout_state\trevision=12\tscene=3\tname=Studio\twidth=1920\theight=1080\t\
             layers={}\n",
            base64url(&bytes)
        )
    );
    assert_eq!(
        round_trip(&layout_state(Vec::new())),
        "scene_layout_state\trevision=12\tscene=3\tname=Studio\twidth=1920\theight=1080\t\
         layers=AA\n"
    );
    // A layer drawing a scene directly (Protocol 2.35).
    let direct = WireSceneLayerOutline {
        source: WireSceneLayerSource::Scene(scene(u128::MAX)),
        ..cam()
    };
    round_trip(&layout_state(vec![direct]));
    // Scene names are bounded and free of control characters.
    for name in ["a\nb".to_owned(), "x".repeat(MAX_SCENE_NAME_BYTES + 1)] {
        let WireMessage::SceneLayoutState(mut named) = layout_state(Vec::new()) else {
            unreachable!()
        };
        named.name = name;
        assert!(matches!(
            encode_line(&WireMessage::SceneLayoutState(named)),
            Err(CodecError::InvalidField { field: "name", .. })
        ));
    }

    // Encoder refusals.
    let duplicate = vec![cam(), cam()];
    let mut turned = cam();
    turned.quarter_turns = 4;
    let mut named = cam();
    named.name = "a\nb".into();
    let mut long = cam();
    long.name = "x".repeat(MAX_SCENE_LAYER_NAME_BYTES + 1);
    for layers in [duplicate, vec![turned], vec![named], vec![long]] {
        assert!(matches!(
            encode_line(&layout_state(layers)),
            Err(CodecError::InvalidField {
                field: "layers",
                ..
            })
        ));
    }
    let WireMessage::SceneLayoutState(mut wide) = layout_state(Vec::new()) else {
        unreachable!()
    };
    wide.layout.width = MAX_SCENE_CANVAS_SIDE + 1;
    assert!(matches!(
        encode_line(&WireMessage::SceneLayoutState(wide)),
        Err(CodecError::InvalidField { field: "width", .. })
    ));

    // Decoder refusals: bad flags, trailing bytes, truncated, zero id.
    let tamper = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&CAM_BYTES);
        edit(&mut bytes);
        line.replace(
            line.rsplit_once("layers=").unwrap().1.trim_end(),
            &base64url(&bytes),
        )
    };
    for bad in [
        tamper(&|bytes| bytes[2] = 2),
        tamper(&|bytes| bytes.push(0)),
        tamper(&|bytes| {
            bytes.pop();
        }),
        tamper(&|bytes| bytes[1] = 0),
        // An unknown source kind, and a zero source id.
        tamper(&|bytes| bytes[8] = 2),
        tamper(&|bytes| bytes[9] = 0),
        line.replace("height=1080", "height=0"),
        line.replace("scene=3", "input=3"),
        line.replace("\tname=Studio", ""),
    ] {
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
}

#[test]
fn the_worst_case_layout_fits_one_line() {
    let layers = (1..=u32::try_from(MAX_SCENE_LAYOUT_LAYERS).unwrap())
        .map(|id| WireSceneLayerOutline {
            id: layer(u32::MAX - id),
            name: "\u{00e9}".repeat(MAX_SCENE_LAYER_NAME_BYTES / 2),
            source: WireSceneLayerSource::Scene(scene(u128::MAX)),
            x: i32::MIN,
            y: i32::MIN,
            width: u32::MAX,
            height: u32::MAX,
            quarter_turns: 3,
            opacity: 255,
            enabled: true,
            tilt_millidegrees: -45_000,
            anchor_x: 10_000,
            anchor_y: 10_000,
            z_order: i32::MIN,
            crop: Some(fm_protocol::WireLayerCrop {
                x: 128,
                y: 128,
                width: 1_792,
                height: 952,
            }),
            // Protocol 2.43: the longest key and the longest effect list
            // (since 2.70 a key/fill key with a refinement).
            key: Some(fm_protocol::WireLayerKey::Fill(fm_protocol::WireFillKey {
                key_input: fm_protocol::WireInputId::new(NonZeroU128::MAX),
                alpha_channel: true,
                low: 254,
                high: 255,
                softness: 255,
                invert: true,
                refinement: fm_protocol::WireKeyRefinement {
                    clip_black: 254,
                    clip_white: 255,
                    choke: -8,
                    edge_softness: 8,
                    despill: fm_protocol::WireKeyDespill::Green { strength: 255 },
                    light_wrap: 255,
                    light_wrap_width: 8,
                    garbage_matte: Some(
                        fm_types::GarbageMatte::new(
                            &[fm_types::MattePoint::new(1_000, 1_000); 8],
                            true,
                        )
                        .unwrap(),
                    ),
                },
            })),
            effects: vec![
                fm_protocol::WireLayerEffect {
                    name: "brightness_contrast".into(),
                    parameters: vec![-100, 100],
                };
                fm_protocol::MAX_SCENE_LAYER_EFFECTS
            ],
            mask: Some(fm_protocol::WireLayerMask {
                x: 128,
                y: 128,
                width: 1_664,
                height: 824,
                invert: true,
            }),
        })
        .collect();
    let line = round_trip(&layout_state(layers));
    assert!(line.len() <= MAX_LINE_BYTES, "{}", line.len());
    let too_many = (1..=u32::try_from(MAX_SCENE_LAYOUT_LAYERS + 1).unwrap())
        .map(|id| WireSceneLayerOutline {
            id: layer(id),
            ..cam()
        })
        .collect();
    assert!(encode_line(&layout_state(too_many)).is_err());
}

fn preview_request(
    sample: fm_protocol::TitlePreviewSample,
    clip: Option<WireSceneClip>,
) -> WireMessage {
    WireMessage::ScenePreviewRequest(fm_protocol::ScenePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "anim-preview-3".to_owned(),
        input: input(12),
        sample,
        width: 640,
        height: 360,
        clip: clip.map(Box::new),
    })
}

/// Protocol 2.32: a scene preview request carries the title preview's
/// fields and an optional scene draft clip (layer-keyed, colour refused).
#[test]
fn scene_preview_requests_round_trip_and_are_bounded() {
    use fm_protocol::TitlePreviewSample;

    let line = round_trip(&preview_request(TitlePreviewSample::In { frame: 4 }, None));
    assert_eq!(
        line,
        "scene_preview_request\tprotocol=2.98\trequest_id=anim-preview-3\tinput=12\tphase=in\t\
         frame=4\twidth=640\theight=360\n"
    );
    let with_clip = round_trip(&preview_request(TitlePreviewSample::Hold, Some(clip())));
    assert!(
        with_clip.ends_with(&format!("\tclip={}\n", base64url(&clip_bytes()))),
        "{with_clip}"
    );
    round_trip(&preview_request(
        TitlePreviewSample::Out {
            frame: MAX_PHASE_FRAMES,
        },
        None,
    ));
    for (bad, field) in [
        ("phase=static\tframe=1\twidth=640\theight=360", "frame"),
        ("phase=in\tframe=36001\twidth=640\theight=360", "frame"),
        ("phase=in\tframe=0\twidth=641\theight=360", "width"),
        ("phase=in\tframe=0\twidth=64\theight=0", "height"),
    ] {
        let line = format!("scene_preview_request\tprotocol=2.98\trequest_id=p\tinput=7\t{bad}\n");
        match decode_line(&line) {
            Err(CodecError::InvalidField { field: actual, .. }) => {
                assert_eq!(actual, field, "{bad}");
            }
            other => panic!("{bad}: {other:?}"),
        }
    }
    // A scene draft may not animate colour, on either side of the wire.
    let colour = Clip::from_tracks(
        timebase(),
        2,
        0,
        vec![Track::new(
            TrackKey::new(layer(1), Phase::In, Property::Color),
            vec![Keyframe::new(
                0,
                Value::Color(Rgba::new(1, 2, 3, 4)),
                Easing::Linear,
            )],
        )],
    )
    .unwrap();
    assert!(encode_line(&preview_request(TitlePreviewSample::Hold, Some(colour))).is_err());
    let WireMessage::ScenePreviewRequest(mut oversized) =
        preview_request(TitlePreviewSample::Hold, None)
    else {
        unreachable!()
    };
    oversized.height = 361;
    assert!(encode_line(&WireMessage::ScenePreviewRequest(oversized)).is_err());
}

/// Protocol 2.32: scene preview parts are title preview parts under their
/// own record kind, reassembled by the same assembler.
#[test]
fn scene_preview_frames_reuse_the_title_preview_parts() {
    use fm_protocol::{
        ServerIdentity, TitlePreviewAssembler, encode_qoi_rgba, title_preview_parts,
    };

    let server = ServerIdentity {
        engine_id: "engine-a".into(),
        project_id: "project-9".into(),
        state_epoch: 7,
        log_id: "log-a".into(),
    };
    let (width, height) = (32_u16, 18_u16);
    let rgba: Vec<u8> = (0..usize::from(width) * usize::from(height))
        .flat_map(|index| [u8::try_from(index % 251).unwrap(), 40, 90, 255])
        .collect();
    let qoi = encode_qoi_rgba(width, height, &rgba).unwrap();
    let parts = title_preview_parts(
        &server,
        "anim-preview-3",
        input(12),
        9,
        false,
        width,
        height,
        &qoi,
    );
    let mut assembler = TitlePreviewAssembler::new();
    let mut image = None;
    for part in parts {
        let line = round_trip(&WireMessage::ScenePreviewFrame(part));
        assert!(
            line.starts_with("scene_preview_frame\tengine_id=engine-a\t"),
            "{line}"
        );
        let WireMessage::ScenePreviewFrame(decoded) = decode_line(&line).unwrap() else {
            unreachable!()
        };
        image = assembler.push(&decoded).unwrap();
    }
    let image = image.unwrap();
    assert_eq!(image.input, input(12));
    assert_eq!(image.animation_revision, 9);
    assert!(!image.draft);
    assert_eq!(&image.rgba[..], &rgba[..]);
}
