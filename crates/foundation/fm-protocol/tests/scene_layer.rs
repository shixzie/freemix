//! Protocol 2.39 live scene layer edits (SW-006): the `scene_layer_*`
//! commands, the `scene_layout_changed` durable event carrying the edit and
//! the scene's whole new layout, and the layout record's crop, mask,
//! z-order, tilt, and anchor — exactly, and strictly.

use core::num::{NonZeroU32, NonZeroU128};

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, ProtocolVersion, SceneLayerEdit,
    SceneLayerOperation, WireInputId, WireLayerCrop, WireLayerEffect, WireLayerGeometry,
    WireLayerMask, WireMessage, WireSceneId, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayerSource, WireSceneLayerSpec, WireSceneLayout, decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.39\tid=a\tidempotency_key=k";

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
    };
    outline.set_geometry(geometry());
    outline
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
        SceneLayerOperation::SetZOrder { z_order: i32::MIN },
        SceneLayerOperation::Move { index: 4 },
        SceneLayerOperation::Add {
            index: 1,
            spec: Box::new(WireSceneLayerSpec {
                layer: outline(2),
                effects: vec![
                    WireLayerEffect {
                        name: "blur".into(),
                        parameters: vec![3, -4],
                    },
                    WireLayerEffect {
                        name: "sharpen".into(),
                        parameters: Vec::new(),
                    },
                ],
            }),
        },
        SceneLayerOperation::Remove,
    ]
}

#[test]
fn protocol_is_2_39() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 39));
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
        for continues in [false, true] {
            let line = round_trip(&command(SceneLayerEdit {
                continues,
                ..edit(operation.clone())
            }));
            assert!(
                line.starts_with(&format!(
                    "{PREFIX}\tpayload={payload}\tscene=3\tlayer=2\tcontinues={continues}"
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
            "payload=scene_layer_geometry_set\tscene=3\tlayer=2\tcontinues=false\tx=-40\ty=30\t\
             width=640\theight=360\tquarter_turns=3\ttilt_millidegrees=-12345\tanchor_x=0\t\
             anchor_y=10000\n",
        ),
        (
            SceneLayerOperation::SetAppearance {
                enabled: true,
                opacity: 255,
            },
            "payload=scene_layer_appearance_set\tscene=3\tlayer=2\tcontinues=false\t\
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
            "payload=scene_layer_mask_set\tscene=3\tlayer=2\tcontinues=false\tx=5\ty=6\t\
             width=7\theight=8\tinvert=true\n",
        ),
        (
            SceneLayerOperation::SetCrop(None),
            "payload=scene_layer_crop_clear\tscene=3\tlayer=2\tcontinues=false\n",
        ),
        (
            SceneLayerOperation::SetZOrder { z_order: -5 },
            "payload=scene_layer_z_order_set\tscene=3\tlayer=2\tcontinues=false\tz_order=-5\n",
        ),
        (
            SceneLayerOperation::Move { index: 0 },
            "payload=scene_layer_move\tscene=3\tlayer=2\tcontinues=false\tindex=0\n",
        ),
        (
            SceneLayerOperation::Remove,
            "payload=scene_layer_remove\tscene=3\tlayer=2\tcontinues=false\n",
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
    let add_line = round_trip(&command(edit(operations()[8].clone())));
    for bad in [
        // Ids are canonical and nonzero.
        geometry_line.replace("\tscene=3", "\tscene=03"),
        geometry_line.replace("\tlayer=2", "\tlayer=0"),
        geometry_line.replace("\tlayer=2", ""),
        // The gesture flag is required and spelled exactly.
        geometry_line.replace("\tcontinues=false", ""),
        geometry_line.replace("continues=false", "continues=0"),
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
    let SceneLayerOperation::Add { index, mut spec } = operations()[8].clone() else {
        unreachable!()
    };
    spec.layer.id = layer(9);
    assert!(encode_line(&command(edit(SceneLayerOperation::Add { index, spec }))).is_err());
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
            ..edit(operation)
        }));
        assert!(
            line.contains(&format!(
                "\tevent=scene_layout_changed\toperation={payload}\tscene=3\tlayer=2\t\
                 continues=true"
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
        ..full.clone()
    };
    layout.layers.push(plain);
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
