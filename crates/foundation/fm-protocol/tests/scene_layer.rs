//! Protocol 2.39 live scene layer edits (SW-006; the gesture token since
//! 2.41): the `scene_layer_*`
//! commands, the `scene_layout_changed` durable event carrying the edit and
//! the scene's whole new layout, and the layout record's crop, mask,
//! z-order, tilt, and anchor — exactly, and strictly. Protocol 2.43
//! (IN-020) adds each layer's key and effects to the layout record and
//! `scene_layer_key_set`/`_clear` and `scene_layer_effects_set`.

use core::num::{NonZeroU32, NonZeroU128};

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, ProtocolVersion, SceneLayerEdit,
    SceneLayerOperation, WireChromaKey, WireInputId, WireLayerCrop, WireLayerEffect,
    WireLayerGeometry, WireLayerKey, WireLayerMask, WireLumaKey, WireMessage, WireSceneId,
    WireSceneLayerId, WireSceneLayerOutline, WireSceneLayerSource, WireSceneLayout, decode_line,
    encode_line,
};

const PREFIX: &str = "command\tprotocol=2.54\tid=a\tidempotency_key=k";

fn scene(value: u128) -> WireSceneId {
    WireSceneId::new(NonZeroU128::new(value).unwrap())
}

fn layer(value: u32) -> WireSceneLayerId {
    WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
}

fn command(edit: SceneLayerEdit) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SceneLayer(edit),
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn edit(operation: SceneLayerOperation) -> SceneLayerEdit {
    SceneLayerEdit {
        scene: scene(3),
        layer: layer(2),
        operation,
        continues: false,
        gesture: 0,
    }
}

fn geometry() -> WireLayerGeometry {
    WireLayerGeometry {
        x: -40,
        y: 30,
        width: 640,
        height: 360,
        quarter_turns: 3,
        tilt_millidegrees: -12_345,
        anchor_x: 0,
        anchor_y: 10_000,
    }
}

fn outline(id: u32) -> WireSceneLayerOutline {
    let mut outline = WireSceneLayerOutline {
        id: layer(id),
        name: "Guest".into(),
        source: WireSceneLayerSource::Input(WireInputId::new(NonZeroU128::new(4).unwrap())),
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        quarter_turns: 0,
        opacity: 200,
        enabled: true,
        tilt_millidegrees: 0,
        anchor_x: 5_000,
        anchor_y: 5_000,
        z_order: -3,
        crop: Some(WireLayerCrop {
            x: 100,
            y: 50,
            width: 1_600,
            height: 900,
        }),
        mask: Some(WireLayerMask {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
            invert: true,
        }),
        key: Some(chroma()),
        effects: effects(),
    };
    outline.set_geometry(geometry());
    outline
}

fn chroma() -> WireLayerKey {
    WireLayerKey::Chroma(WireChromaKey {
        red: 0,
        green: 255,
        blue: 7,
        similarity: 64,
        smoothness: 32,
        spill: 128,
    })
}

fn luma() -> WireLayerKey {
    WireLayerKey::Luma(WireLumaKey {
        low: 16,
        high: 235,
        softness: 8,
        invert: true,
    })
}

fn effects() -> Vec<WireLayerEffect> {
    [
        ("levels", vec![16, 235]),
        ("brightness_contrast", vec![-100, 100]),
        ("saturation", vec![-50]),
        ("sharpen", vec![1_000]),
        ("flip", vec![1, 0]),
    ]
    .into_iter()
    .map(|(name, parameters)| WireLayerEffect {
        name: name.into(),
        parameters,
    })
    .collect()
}

fn operations() -> Vec<SceneLayerOperation> {
    vec![
        SceneLayerOperation::SetGeometry(geometry()),
        SceneLayerOperation::SetAppearance {
            enabled: false,
            opacity: 7,
        },
        SceneLayerOperation::SetCrop(Some(WireLayerCrop {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        })),
        SceneLayerOperation::SetCrop(None),
        SceneLayerOperation::SetMask(Some(WireLayerMask {
            x: 5,
            y: 6,
            width: 7,
            height: 8,
            invert: false,
        })),
        SceneLayerOperation::SetMask(None),
        SceneLayerOperation::SetKey(Some(chroma())),
        SceneLayerOperation::SetKey(None),
        SceneLayerOperation::SetEffects(effects()),
        SceneLayerOperation::SetZOrder { z_order: i32::MIN },
        SceneLayerOperation::Move { index: 4 },
        SceneLayerOperation::Add {
            index: 1,
            spec: Box::new(outline(2)),
        },
        SceneLayerOperation::Remove,
    ]
}

#[test]
fn protocol_is_2_43() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 54));
}

#[test]
fn every_scene_layer_command_round_trips_under_its_payload_name() {
    let names: Vec<&str> = operations()
        .iter()
        .map(SceneLayerOperation::payload)
        .collect();
    assert_eq!(names, SceneLayerOperation::PAYLOADS);
    for operation in operations() {
        let payload = operation.payload();
        for (continues, gesture) in [(false, 0), (true, 7), (true, u64::MAX)] {
            let line = round_trip(&command(SceneLayerEdit {
                continues,
                gesture,
                ..edit(operation.clone())
            }));
            assert!(
                line.starts_with(&format!(
                    "{PREFIX}\tpayload={payload}\tscene=3\tlayer=2\tcontinues={continues}\t\
                     gesture={gesture}"
                )),
                "{line}"
            );
        }
    }
}

#[test]
fn scene_layer_commands_have_an_exact_wire_form() {
    let cases = [
        (
            SceneLayerOperation::SetGeometry(geometry()),
            "payload=scene_layer_geometry_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tx=-40\ty=30\t\
             width=640\theight=360\tquarter_turns=3\ttilt_millidegrees=-12345\tanchor_x=0\t\
             anchor_y=10000\n",
        ),
        (
            SceneLayerOperation::SetAppearance {
                enabled: true,
                opacity: 255,
            },
            "payload=scene_layer_appearance_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\t\
             enabled=true\topacity=255\n",
        ),
        (
            SceneLayerOperation::SetMask(Some(WireLayerMask {
                x: 5,
                y: 6,
                width: 7,
                height: 8,
                invert: true,
            })),
            "payload=scene_layer_mask_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tx=5\ty=6\t\
             width=7\theight=8\tinvert=true\n",
        ),
        (
            SceneLayerOperation::SetCrop(None),
            "payload=scene_layer_crop_clear\tscene=3\tlayer=2\tcontinues=false\tgesture=0\n",
        ),
        (
            SceneLayerOperation::SetZOrder { z_order: -5 },
            "payload=scene_layer_z_order_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tz_order=-5\n",
        ),
        (
            SceneLayerOperation::Move { index: 0 },
            "payload=scene_layer_move\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tindex=0\n",
        ),
        (
            SceneLayerOperation::Remove,
            "payload=scene_layer_remove\tscene=3\tlayer=2\tcontinues=false\tgesture=0\n",
        ),
        (
            SceneLayerOperation::SetKey(Some(chroma())),
            "payload=scene_layer_key_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tkind=chroma\t\
             red=0\tgreen=255\tblue=7\tsimilarity=64\tsmoothness=32\tspill=128\n",
        ),
        (
            SceneLayerOperation::SetKey(Some(luma())),
            "payload=scene_layer_key_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\tkind=luma\t\
             low=16\thigh=235\tsoftness=8\tinvert=true\n",
        ),
        (
            SceneLayerOperation::SetKey(None),
            "payload=scene_layer_key_clear\tscene=3\tlayer=2\tcontinues=false\tgesture=0\n",
        ),
        (
            SceneLayerOperation::SetEffects(Vec::new()),
            "payload=scene_layer_effects_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\t\
             effects=AA\n",
        ),
        (
            // levels 16..235 is kind 0 then zigzag 32 and 470 (0xD6 0x03).
            SceneLayerOperation::SetEffects(effects()[..1].to_vec()),
            "payload=scene_layer_effects_set\tscene=3\tlayer=2\tcontinues=false\tgesture=0\t\
             effects=AQAg1gM\n",
        ),
    ];
    for (operation, tail) in cases {
        let line = round_trip(&command(edit(operation)));
        assert_eq!(line, format!("{PREFIX}\t{tail}"));
    }
}

#[test]
fn scene_layer_commands_are_strict() {
    let geometry_line = round_trip(&command(edit(SceneLayerOperation::SetGeometry(geometry()))));
    let z_line = round_trip(&command(edit(SceneLayerOperation::SetZOrder {
        z_order: -5,
    })));
    let add_line = round_trip(&command(edit(operations()[11].clone())));
    let key_line = round_trip(&command(edit(SceneLayerOperation::SetKey(Some(luma())))));
    let effects_line = round_trip(&command(edit(SceneLayerOperation::SetEffects(
        effects()[..1].to_vec(),
    ))));
    for bad in [
        // Ids are canonical and nonzero.
        geometry_line.replace("\tscene=3", "\tscene=03"),
        geometry_line.replace("\tlayer=2", "\tlayer=0"),
        geometry_line.replace("\tlayer=2", ""),
        // The gesture flag and token are required and spelled exactly.
        geometry_line.replace("\tcontinues=false", ""),
        geometry_line.replace("continues=false", "continues=0"),
        geometry_line.replace("\tgesture=0", ""),
        geometry_line.replace("gesture=0", "gesture=00"),
        geometry_line.replace("gesture=0", "gesture=-1"),
        geometry_line.replace("gesture=0", "gesture=18446744073709551616"),
        // Signed values are canonical.
        geometry_line.replace("x=-40", "x=-040"),
        z_line.replace("z_order=-5", "z_order=-0"),
        z_line.replace("z_order=-5", "z_order=2147483648"),
        // Quarter turns, tilt, and anchor are bounded; sides are nonzero.
        geometry_line.replace("quarter_turns=3", "quarter_turns=4"),
        geometry_line.replace("tilt_millidegrees=-12345", "tilt_millidegrees=45000"),
        geometry_line.replace("tilt_millidegrees=-12345", "tilt_millidegrees=-45001"),
        geometry_line.replace("anchor_y=10000", "anchor_y=10001"),
        geometry_line.replace("width=640", "width=0"),
        // Unknown and missing fields.
        geometry_line.replace("\tanchor_x=0", ""),
        geometry_line.replace('\n', "\textra=1\n"),
        // An added layer's spec names the edited layer.
        add_line.replace("\tlayer=2", "\tlayer=5"),
        // Keys name a known kind, bounded bytes, and a nonempty luma band.
        key_line.replace("kind=luma", "kind=alpha"),
        key_line.replace("low=16", "low=256"),
        key_line.replace("low=16", "low=235"),
        key_line.replace("invert=true", "invert=1"),
        key_line.replace("\tsoftness=8", ""),
        // Effects are a known kind with in-range values, no trailing bytes.
        effects_line.replace("effects=AQAg1gM", "effects=AQUg1gM"),
        effects_line.replace("effects=AQAg1gM", "effects=AQDWAyA"),
        effects_line.replace("effects=AQAg1gM", "effects=AQAg1gMA"),
        effects_line.replace("effects=AQAg1gM", "effects=CQ"),
    ] {
        assert_ne!(bad, geometry_line);
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
    // The encoder refuses what the decoder would.
    let mut tilted = geometry();
    tilted.tilt_millidegrees = 45_000;
    assert!(matches!(
        encode_line(&command(edit(SceneLayerOperation::SetGeometry(tilted)))),
        Err(CodecError::InvalidField { .. })
    ));
    let SceneLayerOperation::Add { index, mut spec } = operations()[11].clone() else {
        unreachable!()
    };
    spec.id = layer(9);
    assert!(encode_line(&command(edit(SceneLayerOperation::Add { index, spec }))).is_err());
    let bad_effect = WireLayerEffect {
        name: "blur".into(),
        parameters: vec![3],
    };
    assert!(!bad_effect.is_valid());
    assert!(
        encode_line(&command(edit(SceneLayerOperation::SetEffects(vec![
            bad_effect
        ]))))
        .is_err()
    );
    let too_many = vec![effects()[0].clone(); fm_protocol::MAX_SCENE_LAYER_EFFECTS + 1];
    assert!(encode_line(&command(edit(SceneLayerOperation::SetEffects(too_many)))).is_err());
    let empty_band = WireLayerKey::Luma(WireLumaKey {
        low: 9,
        high: 9,
        softness: 0,
        invert: false,
    });
    assert!(
        encode_line(&command(edit(SceneLayerOperation::SetKey(Some(
            empty_band
        )))))
        .is_err()
    );
}

fn layout() -> WireSceneLayout {
    WireSceneLayout {
        width: 1_920,
        height: 1_080,
        layers: vec![outline(2)],
    }
}

fn event(edit: SceneLayerEdit) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload: EventPayload::SceneLayoutChanged {
            edit,
            layout: Box::new(layout()),
        },
        edit_history: EditHistoryStatus {
            undo_depth: 1,
            redo_depth: 0,
            next_undo: Some(12),
            next_redo: None,
        },
    })
}

#[test]
fn layout_changes_carry_the_edit_and_the_whole_new_layout() {
    for operation in operations() {
        let payload = operation.payload();
        let line = round_trip(&event(SceneLayerEdit {
            continues: true,
            gesture: 42,
            ..edit(operation)
        }));
        assert!(
            line.contains(&format!(
                "\tevent=scene_layout_changed\toperation={payload}\tscene=3\tlayer=2\t\
                 continues=true\tgesture=42"
            )),
            "{line}"
        );
        assert!(line.contains("\tlayout_width=1920\tlayout_height=1080\tlayers="));
    }
    let line = round_trip(&event(edit(SceneLayerOperation::Remove)));
    for bad in [
        line.replace("\toperation=scene_layer_remove", ""),
        line.replace("operation=scene_layer_remove", "operation=scene_layer_drop"),
        line.replace("layout_height=1080", "layout_height=0"),
    ] {
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
}

#[test]
fn layout_records_carry_crop_mask_z_order_tilt_and_anchor() {
    let full = outline(2);
    let mut layout = layout();
    let plain = WireSceneLayerOutline {
        id: layer(3),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        ..full.clone()
    };
    layout.layers.push(plain);
    layout.layers.push(WireSceneLayerOutline {
        id: layer(4),
        key: Some(luma()),
        ..full.clone()
    });
    let line = round_trip(&WireMessage::SceneLayoutState(
        fm_protocol::SceneLayoutStateMessage {
            revision: 4,
            scene: scene(3),
            name: "Studio".into(),
            layout: Box::new(layout.clone()),
        },
    ));
    let WireMessage::SceneLayoutState(decoded) = decode_line(&line).unwrap() else {
        unreachable!()
    };
    let guest = &decoded.layout.layers[0];
    assert_eq!(guest.geometry(), geometry());
    assert_eq!(guest.geometry().rotation_millidegrees(), 257_655);
    assert_eq!(
        (guest.z_order, guest.crop, guest.mask),
        (-3, full.crop, full.mask)
    );
    assert_eq!(decoded.layout, Box::new(layout.clone()));
    assert_eq!((guest.key, &guest.effects), (Some(chroma()), &effects()));
    assert_eq!(decoded.layout.layers[2].key, Some(luma()));
    // A crop beyond the canvas, a mask beyond the cropped source, or an
    // anchor outside the box is refused.
    for broken in [
        WireSceneLayerOutline {
            crop: Some(WireLayerCrop {
                x: 1_000,
                y: 0,
                width: 1_000,
                height: 10,
            }),
            ..full.clone()
        },
        WireSceneLayerOutline {
            mask: Some(WireLayerMask {
                x: 0,
                y: 0,
                width: 1_601,
                height: 1,
                invert: false,
            }),
            ..full.clone()
        },
        WireSceneLayerOutline {
            anchor_x: 10_001,
            ..full.clone()
        },
        WireSceneLayerOutline {
            tilt_millidegrees: 45_000,
            ..full.clone()
        },
        WireSceneLayerOutline {
            key: Some(WireLayerKey::Luma(WireLumaKey {
                low: 200,
                high: 100,
                softness: 0,
                invert: false,
            })),
            ..full.clone()
        },
        WireSceneLayerOutline {
            effects: vec![WireLayerEffect {
                name: "levels".into(),
                parameters: vec![200, 100],
            }],
            ..full.clone()
        },
        WireSceneLayerOutline {
            effects: vec![effects()[0].clone(); fm_protocol::MAX_SCENE_LAYER_EFFECTS + 1],
            ..full.clone()
        },
    ] {
        let mut layout = layout.clone();
        layout.layers = vec![broken];
        assert!(
            encode_line(&WireMessage::SceneLayoutState(
                fm_protocol::SceneLayoutStateMessage {
                    revision: 4,
                    scene: scene(3),
                    name: "Studio".into(),
                    layout: Box::new(layout),
                },
            ))
            .is_err()
        );
    }
}
