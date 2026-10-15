//! GX-011 animation preset library: saving, renaming, and deleting presets
//! are undoable project edits under the project padlock, and applying a
//! preset to a title element or a scene layer is an animation edit recorded
//! as the exact clip it made (so undo never needs the preset).
//!
//! Input 3 is a title whose template has one 8x8 box element; input 4 shows
//! a scene with one 16x8 layer. Both animations start with a 10-frame In
//! phase at 30/1.

use core::num::{NonZeroU32, NonZeroU128};

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineAnimationPresetEdit, EngineCommand,
    EngineCommandKind, EngineCommandOutcome, EngineEvent, EngineSceneAnimationEdit, EngineTitle,
    EngineTitleAnimationEdit, SceneAnimationChange, ShowState, TitleAnimationChange,
};
use fm_model::{
    AnimationPresetId, AnimationPresetLibrary, Layer, LayerGeometry, LayerId, Rgba8, Rotation,
    Scene, SceneAnimation, SourceRef, UserAnimationPreset,
    fm_animation::{AnimationPreset, Phase, PhaseMotion, Property, TrackKey, Value},
};
use fm_titles::{
    Bounds, Color, Element, ElementId, ElementKind, Style, TemplateId, TitleAnimation,
    TitleFieldSet, TitleTemplate,
};
use fm_types::{FrameRate, InputId, SceneId};

const PROGRAM: u128 = 1;
const PREVIEW: u128 = 2;
const TITLE: u128 = 3;
const SCENE: u128 = 4;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn preset_id(value: u32) -> AnimationPresetId {
    AnimationPresetId::new(NonZeroU32::new(value).unwrap())
}

fn element() -> ElementId {
    ElementId::new(nonzero(1))
}

fn layer() -> LayerId {
    LayerId::new(1).unwrap()
}

fn template() -> TitleTemplate {
    let mut template = TitleTemplate {
        id: TemplateId::new(nonzero(9)),
        name: "Lower third".into(),
        width: 32,
        height: 18,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: vec![Element {
            id: element(),
            name: "box".into(),
            bounds: Bounds {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            },
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::new(0, 0, 255, 255),
                ..Style::default()
            },
            color_field: None,
            kind: ElementKind::Rectangle,
        }],
        animation: TitleAnimation::default(),
    };
    template
        .edit_animation(|clip| clip.set_phase_frames_keeping_keys(Phase::In, 10))
        .unwrap();
    template
}

fn scene() -> Scene {
    let mut scene = Scene {
        id: SceneId::new(nonzero(1)),
        name: "Scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: layer(),
            name: "Box".into(),
            source: SourceRef::Input(input(PROGRAM)),
            enabled: true,
            geometry: LayerGeometry::new(4, 4, 16, 8, Rotation::Deg0),
            crop: None,
            mask: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: SceneAnimation::default(),
    };
    scene
        .edit_animation(|clip| clip.set_phase_frames_keeping_keys(Phase::In, 10))
        .unwrap();
    scene
}

/// A 20-frame wipe captured from an 8 px wide box.
fn wipe() -> PhaseMotion {
    let mut template = template();
    template
        .apply_animation_preset(element(), Phase::In, AnimationPreset::Wipe, 20)
        .unwrap();
    template
        .capture_animation_motion(element(), Phase::In)
        .unwrap()
}

fn preset(id: u32, name: &str) -> UserAnimationPreset {
    UserAnimationPreset {
        id: preset_id(id),
        name: name.into(),
        phase: Phase::In,
        motion: wipe(),
    }
}

fn engine() -> Engine {
    let mut show = ShowState::new(
        "presets",
        (PROGRAM..=SCENE)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    let template = template();
    show.set_title(
        input(TITLE),
        EngineTitle::new(
            "Lower third",
            TitleFieldSet::from_template(&template).unwrap(),
        )
        .with_template("titles/lower.fmtitle", template),
    )
    .unwrap();
    show.set_scene_input(input(SCENE), scene(), 32, 18).unwrap();
    show.restore_animation_presets(
        AnimationPresetLibrary::checked(vec![preset(1, "Wipe")]).unwrap(),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(30, 1).unwrap(),
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

fn names(engine: &Engine) -> Vec<String> {
    engine
        .show()
        .animation_presets()
        .presets()
        .iter()
        .map(|preset| preset.name.clone())
        .collect()
}

fn library(edit: EngineAnimationPresetEdit) -> EngineCommand {
    EngineCommand::AnimationPreset(edit)
}

fn undo() -> EngineCommand {
    EngineCommand::Undo {
        expected_edit: None,
    }
}

fn redo() -> EngineCommand {
    EngineCommand::Redo {
        expected_edit: None,
    }
}

#[test]
fn library_edits_are_undoable_project_edits() {
    let mut engine = engine();
    let add = library(EngineAnimationPresetEdit::Add {
        preset: Box::new(preset(2, "Second")),
        index: Some(0),
    });
    assert_eq!(add.kind(), EngineCommandKind::ProjectEdit);
    let outcome = accept(&mut engine, "add", add);
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event.payload, EngineEvent::AnimationPresetsChanged(_)))
    );
    assert_eq!(names(&engine), ["Second", "Wipe"]);
    accept(
        &mut engine,
        "rename",
        library(EngineAnimationPresetEdit::Rename {
            id: preset_id(1),
            name: "Reveal".into(),
        }),
    );
    accept(
        &mut engine,
        "remove",
        library(EngineAnimationPresetEdit::Remove { id: preset_id(2) }),
    );
    assert_eq!(names(&engine), ["Reveal"]);
    accept(&mut engine, "undo remove", undo());
    assert_eq!(names(&engine), ["Second", "Reveal"]);
    assert_eq!(
        engine.show().animation_presets().get(preset_id(2)),
        Some(&preset(2, "Second"))
    );
    accept(&mut engine, "undo rename", undo());
    assert_eq!(names(&engine), ["Second", "Wipe"]);
    accept(&mut engine, "undo add", undo());
    assert_eq!(names(&engine), ["Wipe"]);
    accept(&mut engine, "redo add", redo());
    accept(&mut engine, "redo rename", redo());
    accept(&mut engine, "redo remove", redo());
    assert_eq!(names(&engine), ["Reveal"]);
}

#[test]
fn refused_library_edits_name_their_field() {
    let mut engine = engine();
    let duplicate = execute(
        &mut engine,
        "duplicate",
        library(EngineAnimationPresetEdit::Add {
            preset: Box::new(preset(2, "Wipe")),
            index: None,
        }),
    );
    assert_eq!(rejection(&duplicate).code, RejectionCode::Conflict);
    assert_eq!(rejection(&duplicate).fields[0].code, "duplicate_name");
    let unknown = execute(
        &mut engine,
        "unknown",
        library(EngineAnimationPresetEdit::Remove { id: preset_id(7) }),
    );
    assert_eq!(rejection(&unknown).code, RejectionCode::NotFound);
    assert_eq!(rejection(&unknown).fields[0].code, "unknown_preset");
    let blank = execute(
        &mut engine,
        "blank",
        library(EngineAnimationPresetEdit::Rename {
            id: preset_id(1),
            name: " ".into(),
        }),
    );
    assert_eq!(rejection(&blank).fields[0].code, "invalid_name");
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let locked = execute(
        &mut engine,
        "locked",
        library(EngineAnimationPresetEdit::Remove { id: preset_id(1) }),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&locked)),
        Some(EditRejectionKind::ProjectLocked)
    );
}

fn crop_right(keys: &[fm_model::fm_animation::Keyframe]) -> Vec<(u32, Value)> {
    keys.iter().map(|key| (key.frame, key.value)).collect()
}

#[test]
fn a_preset_applies_to_a_title_element_and_undo_restores_the_clip() {
    let mut engine = engine();
    let template = |engine: &Engine| {
        engine
            .show()
            .title(input(TITLE))
            .unwrap()
            .template
            .clone()
            .unwrap()
    };
    let before = template(&engine).animation().clone();
    accept(
        &mut engine,
        "apply",
        EngineCommand::TitleAnimation(EngineTitleAnimationEdit {
            input: input(TITLE),
            base_revision: Some(before.revision),
            change: TitleAnimationChange::Motion {
                element: element(),
                phase: Phase::Out,
                preset: preset_id(1),
                frames: 10,
            },
        }),
    );
    let after = template(&engine).animation().clone();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.clip.out_frames(), 10);
    let track = after
        .clip
        .track(TrackKey::new(element(), Phase::Out, Property::CropRight))
        .unwrap();
    assert_eq!(
        crop_right(&track.keys),
        [(0, Value::Scalar(8)), (10, Value::Scalar(0))]
    );
    // Deleting the preset leaves the animation alone, and undo of the
    // application still restores the exact clip.
    accept(
        &mut engine,
        "remove",
        library(EngineAnimationPresetEdit::Remove { id: preset_id(1) }),
    );
    assert_eq!(template(&engine).animation().clip, after.clip);
    accept(&mut engine, "undo remove", undo());
    accept(&mut engine, "undo apply", undo());
    assert_eq!(template(&engine).animation().clip, before.clip);
}

#[test]
fn a_preset_applies_to_a_scene_layer_sized_to_its_box() {
    let mut engine = engine();
    let animation = |engine: &Engine| {
        engine
            .show()
            .scene_input(input(SCENE))
            .unwrap()
            .animation()
            .clone()
    };
    let unknown = execute(
        &mut engine,
        "unknown preset",
        EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
            input: input(SCENE),
            base_revision: Some(1),
            change: SceneAnimationChange::Motion {
                layer: layer(),
                phase: Phase::In,
                preset: preset_id(5),
                frames: 10,
            },
        }),
    );
    assert_eq!(rejection(&unknown).code, RejectionCode::NotFound);
    assert_eq!(rejection(&unknown).fields[0].code, "unknown_preset");
    let collision = execute(
        &mut engine,
        "collision",
        EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
            input: input(SCENE),
            base_revision: Some(1),
            change: SceneAnimationChange::Motion {
                layer: layer(),
                phase: Phase::In,
                preset: preset_id(1),
                frames: 0,
            },
        }),
    );
    assert_eq!(rejection(&collision).fields[0].code, "key_collision");
    accept(
        &mut engine,
        "apply",
        EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
            input: input(SCENE),
            base_revision: Some(1),
            change: SceneAnimationChange::Motion {
                layer: layer(),
                phase: Phase::In,
                preset: preset_id(1),
                frames: 30,
            },
        }),
    );
    let after = animation(&engine);
    assert_eq!(after.revision, 2);
    assert_eq!(after.clip.in_frames(), 30);
    let track = after
        .clip
        .track(TrackKey::new(layer(), Phase::In, Property::CropRight))
        .unwrap();
    // The 8 px wipe covers the 16 px layer.
    assert_eq!(
        crop_right(&track.keys),
        [(0, Value::Scalar(16)), (30, Value::Scalar(0))]
    );
    accept(&mut engine, "undo", undo());
    assert_eq!(animation(&engine).clip, scene().animation.clip);
}
