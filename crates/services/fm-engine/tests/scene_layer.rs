//! SW-006: live scene layer edits are engine project edits — padlocked by
//! the project and by every Scene input showing the scene, undoable with an
//! exact inverse, folded into one undo step per layer property while a
//! gesture continues, and checked against the show (sources, cycles, the
//! scene's animation) before the scene changes.
//!
//! Inputs 3 and 4 show scene 1; scene 2 is registered without a Scene
//! input. Both scenes sit on a 1920x1080 canvas; scene 1 holds layer 1
//! (input 1, full frame) and layer 2 (input 2, a picture-in-picture).

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineSceneLayerEdit, ShowState, scene_inputs_showing,
};
use fm_model::{
    CropRect, Layer, LayerAnchor, LayerGeometry, LayerId, RectMask, Rgba8, Rotation, Scene,
    SceneAnimation, SceneLayerChange, SourceRef,
    fm_animation::{Easing, Keyframe, Phase, Property, Track, TrackKey, Value},
};
use fm_types::{FrameRate, InputId, SceneId};

const PROGRAM: u128 = 1;
const PREVIEW: u128 = 2;
const SHARED_A: u128 = 3;
const SHARED_B: u128 = 4;
const SPARE: u128 = 5;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(nonzero(value))
}

fn layer_id(value: u32) -> LayerId {
    LayerId::new(value).unwrap()
}

fn layer(id: u32, source: u128, geometry: LayerGeometry) -> Layer {
    Layer {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: SourceRef::Input(input(source)),
        enabled: true,
        geometry,
        crop: None,
        mask: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: 0,
    }
}

fn scene(id: u128) -> Scene {
    Scene {
        id: scene_id(id),
        name: format!("Scene {id}"),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(
                1,
                PROGRAM,
                LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0),
            ),
            layer(
                2,
                PREVIEW,
                LayerGeometry::new(1_360, 740, 480, 270, Rotation::Deg0),
            ),
        ],
        animation: SceneAnimation::default(),
    }
}

fn engine() -> Engine {
    let mut show = ShowState::new(
        "scene layers",
        (PROGRAM..=SPARE)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    for value in [SHARED_A, SHARED_B] {
        show.set_scene_input(input(value), scene(1), 1_920, 1_080)
            .unwrap();
    }
    show.set_scene(scene(2), 1_920, 1_080);
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(nonzero(100)),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = execute(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
    outcome
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

fn edit(scene: u128, layer: u32, change: SceneLayerChange) -> EngineCommand {
    EngineCommand::SceneLayer(EngineSceneLayerEdit::new(
        scene_id(scene),
        layer_id(layer),
        change,
    ))
}

fn continuing(scene: u128, layer: u32, change: SceneLayerChange) -> EngineCommand {
    EngineCommand::SceneLayer(EngineSceneLayerEdit {
        continues: true,
        ..EngineSceneLayerEdit::new(scene_id(scene), layer_id(layer), change)
    })
}

fn shown(engine: &Engine, scene: u128) -> Scene {
    (*engine.show().scene(scene_id(scene)).unwrap().scene).clone()
}

fn undo(engine: &mut Engine, key: &str) {
    accept(
        engine,
        key,
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
}

fn redo(engine: &mut Engine, key: &str) {
    accept(
        engine,
        key,
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
}

#[test]
fn every_live_edit_is_an_undoable_project_edit_of_the_shared_scene() {
    let mut engine = engine();
    assert_eq!(
        scene_inputs_showing(engine.show(), scene_id(1)),
        vec![input(SHARED_A), input(SHARED_B)]
    );
    let geometry = LayerGeometry::new(1_200, 600, 640, 360, Rotation::Deg90)
        .with_tilt(-12_500)
        .with_anchor(LayerAnchor::new(0, 10_000));
    let mut added = layer(
        7,
        SPARE,
        LayerGeometry::new(10, 10, 100, 100, Rotation::Deg0),
    );
    added.crop = Some(CropRect::new(0, 0, 960, 540));
    let changes: Vec<(u32, SceneLayerChange)> = vec![
        (2, SceneLayerChange::Geometry(geometry)),
        (
            2,
            SceneLayerChange::Appearance {
                enabled: false,
                opacity: 64,
            },
        ),
        (
            2,
            SceneLayerChange::Crop(Some(CropRect::new(100, 50, 1_600, 900))),
        ),
        (
            2,
            SceneLayerChange::Mask(Some(RectMask {
                x: 10,
                y: 20,
                width: 300,
                height: 200,
                invert: true,
            })),
        ),
        (2, SceneLayerChange::ZOrder(5)),
        (2, SceneLayerChange::Move { index: 0 }),
        (
            7,
            SceneLayerChange::Add {
                index: 1,
                layer: Box::new(added.clone()),
            },
        ),
        (1, SceneLayerChange::Remove),
    ];
    let mut states = vec![shown(&engine, 1)];
    for (index, (layer, change)) in changes.iter().enumerate() {
        let command = edit(1, *layer, change.clone());
        assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
        let outcome = accept(&mut engine, &format!("edit-{index}"), command);
        assert_eq!(
            outcome
                .events
                .iter()
                .map(|event| event.payload.clone())
                .collect::<Vec<_>>(),
            vec![EngineEvent::SceneLayoutChanged(EngineSceneLayerEdit::new(
                scene_id(1),
                layer_id(*layer),
                change.clone(),
            ))]
        );
        states.push(shown(&engine, 1));
    }
    let last = shown(&engine, 1);
    let order: Vec<u32> = last.layers.iter().map(|layer| layer.id.get()).collect();
    assert_eq!(order, [2, 7]);
    let pip = &last.layers[0];
    assert_eq!(pip.geometry, geometry);
    assert_eq!((pip.enabled, pip.opacity, pip.z_order), (false, 64, 5));
    assert_eq!(pip.crop, Some(CropRect::new(100, 50, 1_600, 900)));
    assert!(pip.mask.is_some_and(|mask| mask.invert));
    assert_eq!(last.layers[1], added);
    // Both Scene inputs show the one scene.
    assert_eq!(
        engine
            .show()
            .scene_input(input(SHARED_B))
            .unwrap()
            .scene
            .layers,
        last.layers
    );
    assert_eq!(engine.undo_entries().len(), changes.len());
    // Undo walks back through every exact state, redo forward again.
    for (step, expected) in states.iter().rev().skip(1).enumerate() {
        undo(&mut engine, &format!("undo-{step}"));
        assert_eq!(&shown(&engine, 1), expected, "undo {step}");
    }
    for (step, expected) in states.iter().skip(1).enumerate() {
        redo(&mut engine, &format!("redo-{step}"));
        assert_eq!(&shown(&engine, 1), expected, "redo {step}");
    }
    // A scene no Scene input shows is edited the same way.
    accept(
        &mut engine,
        "other",
        edit(2, 1, SceneLayerChange::ZOrder(-1)),
    );
    assert_eq!(shown(&engine, 2).layers[0].z_order, -1);
}

#[test]
fn a_gesture_folds_into_one_undo_step_per_layer_property() {
    let mut engine = engine();
    let original = shown(&engine, 1);
    let at =
        |x: i32| SceneLayerChange::Geometry(LayerGeometry::new(x, 740, 480, 270, Rotation::Deg0));
    let both =
        |x: i32| SceneLayerChange::Geometry(LayerGeometry::new(x, 0, 1_920, 1_080, Rotation::Deg0));
    // A two-layer drag: each layer's first send opens its step, the later
    // ones fold into it.
    accept(&mut engine, "a0", edit(1, 2, at(1_300)));
    accept(&mut engine, "b0", edit(1, 1, both(-60)));
    for (step, x) in [1_250, 1_200, 1_150].into_iter().enumerate() {
        accept(
            &mut engine,
            &format!("a{}", step + 1),
            continuing(1, 2, at(x)),
        );
        accept(
            &mut engine,
            &format!("b{}", step + 1),
            continuing(1, 1, both(x - 1_360)),
        );
    }
    assert_eq!(engine.undo_entries().len(), 2);
    let moved = shown(&engine, 1);
    assert_eq!(moved.layers[1].geometry.translation_x, 1_150);
    assert_eq!(moved.layers[0].geometry.translation_x, -210);
    // A continuing edit of another property opens its own step.
    accept(
        &mut engine,
        "opacity",
        continuing(
            1,
            2,
            SceneLayerChange::Appearance {
                enabled: true,
                opacity: 9,
            },
        ),
    );
    assert_eq!(engine.undo_entries().len(), 3);
    undo(&mut engine, "undo-opacity");
    assert_eq!(shown(&engine, 1), moved);
    // A continuing edit never folds across an undo: redo is cleared and a
    // new step opens.
    accept(&mut engine, "after-undo", continuing(1, 2, at(1_100)));
    assert_eq!(engine.undo_entries().len(), 3);
    undo(&mut engine, "undo-after");
    assert_eq!(shown(&engine, 1), moved);
    // Each layer's whole drag undoes in one step, back to the start.
    undo(&mut engine, "undo-b");
    undo(&mut engine, "undo-a");
    assert_eq!(shown(&engine, 1), original);
    redo(&mut engine, "redo-a");
    redo(&mut engine, "redo-b");
    assert_eq!(shown(&engine, 1), moved);
}

#[test]
fn padlocks_cover_the_project_and_every_input_showing_the_scene() {
    let mut engine = engine();
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(SHARED_B)),
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "locked",
        edit(1, 2, SceneLayerChange::ZOrder(1)),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    assert_eq!(shown(&engine, 1), scene(1));
    // Scene 2 is shown by no locked input.
    accept(&mut engine, "free", edit(2, 2, SceneLayerChange::ZOrder(1)));
    accept(
        &mut engine,
        "project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "project-locked",
        edit(2, 2, SceneLayerChange::ZOrder(2)),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
}

#[test]
#[allow(clippy::too_many_lines)] // one table of refusals
fn refusals_name_the_problem_and_leave_the_scene_alone() {
    let mut engine = engine();
    let mut animated = scene(1);
    animated
        .edit_animation(|clip| {
            clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
            clip.set_track(Track::new(
                TrackKey::new(layer_id(2), Phase::In, Property::Opacity),
                vec![Keyframe::new(0, Value::Scalar(0), Easing::Linear)],
            ))
        })
        .unwrap();
    engine = {
        let mut show = engine.show().clone();
        show.set_scene_input(input(SHARED_A), animated, 1_920, 1_080)
            .unwrap();
        Engine::new(
            show,
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(nonzero(100)),
        )
    };
    let before = shown(&engine, 1);
    let long_name = {
        let mut long = layer(8, SPARE, LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0));
        long.name = "n".repeat(81);
        long
    };
    let cases: Vec<(EngineCommand, RejectionCode, &str)> = vec![
        (
            edit(9, 1, SceneLayerChange::ZOrder(0)),
            RejectionCode::NotFound,
            "unknown_scene",
        ),
        (
            edit(1, 9, SceneLayerChange::ZOrder(0)),
            RejectionCode::NotFound,
            "unknown_layer",
        ),
        (
            edit(
                1,
                2,
                SceneLayerChange::Geometry(
                    LayerGeometry::new(0, 0, 10, 10, Rotation::Deg0).with_tilt(45_000),
                ),
            ),
            RejectionCode::InvalidCommand,
            "invalid_geometry",
        ),
        (
            edit(1, 2, SceneLayerChange::Remove),
            RejectionCode::Conflict,
            "layer_animated",
        ),
        (
            edit(
                1,
                8,
                SceneLayerChange::Add {
                    index: 0,
                    layer: Box::new(layer(8, 77, LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0))),
                },
            ),
            RejectionCode::NotFound,
            "unknown_input",
        ),
        // Scene input 4 shows scene 1: a layer of it would draw itself.
        (
            edit(
                1,
                8,
                SceneLayerChange::Add {
                    index: 0,
                    layer: Box::new(layer(
                        8,
                        SHARED_B,
                        LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0),
                    )),
                },
            ),
            RejectionCode::InvalidCommand,
            "source_cycle",
        ),
        (
            edit(
                1,
                8,
                SceneLayerChange::Add {
                    index: 0,
                    layer: Box::new(long_name),
                },
            ),
            RejectionCode::InvalidCommand,
            "invalid_name",
        ),
    ];
    for (index, (command, code, issue)) in cases.into_iter().enumerate() {
        let outcome = execute(&mut engine, &format!("refused-{index}"), command);
        let refused = rejection(&outcome);
        assert_eq!(refused.code, code, "{issue}: {refused:?}");
        assert_eq!(refused.fields[0].code, issue, "{refused:?}");
        assert_eq!(shown(&engine, 1), before, "{issue}");
    }
    // A nested scene drawing scene 1 may not be added to scene 1 either,
    // while scene 2 may draw scene 1.
    let nested = |id: u32, source: u128| {
        let mut nested = layer(id, PROGRAM, LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0));
        nested.source = SourceRef::Scene(scene_id(source));
        nested
    };
    accept(
        &mut engine,
        "nest",
        edit(
            2,
            9,
            SceneLayerChange::Add {
                index: 2,
                layer: Box::new(nested(9, 1)),
            },
        ),
    );
    let outcome = execute(
        &mut engine,
        "cycle",
        edit(
            1,
            9,
            SceneLayerChange::Add {
                index: 2,
                layer: Box::new(nested(9, 2)),
            },
        ),
    );
    assert_eq!(rejection(&outcome).fields[0].code, "source_cycle");
}
