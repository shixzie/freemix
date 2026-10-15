//! GX-011 stage 5: scene layer animation edits are engine project edits —
//! revision-checked, padlocked, undoable, shared by every Scene input showing
//! the same scene — and scenes play their In and Out phases on air.
//!
//! Inputs 3 and 4 show scene 1; input 5 shows scene 2. Each scene has one
//! layer (id 1) at (4, 4) 8x8 on a 32x18 canvas, and an In phase of 10 and
//! an Out phase of 6 frames at the default 30/1 timebase. The engine runs at
//! 25 fps, so a phase of `n` clip frames lasts `ceil(n * 25 / 30)` frames.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineSceneAnimationEdit, SceneAnimationChange, ShowState,
    scene_inputs_sharing,
};
use fm_model::{
    Layer, LayerGeometry, LayerId, Rgba8, Rotation, Scene, SceneAnimation, SceneClip, SourceRef,
    fm_animation::{
        AnimationPreset, ClipPosition, ClipState, Easing, Keyframe, Phase, Property, Track,
        TrackKey, Value,
    },
};
use fm_titles::TitleAirPhase;
use fm_types::{FrameRate, InputId, SceneId};

const PROGRAM: u128 = 1;
const PREVIEW: u128 = 2;
const SHARED_A: u128 = 3;
const SHARED_B: u128 = 4;
const OTHER: u128 = 5;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn layer() -> LayerId {
    LayerId::new(1).unwrap()
}

fn scene(id: u128) -> Scene {
    let mut scene = Scene {
        id: SceneId::new(nonzero(id)),
        name: format!("Scene {id}"),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: layer(),
            name: "Box".into(),
            source: SourceRef::Input(input(PROGRAM)),
            enabled: true,
            geometry: LayerGeometry::new(4, 4, 8, 8, Rotation::Deg0),
            crop: None,
            mask: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: SceneAnimation::default(),
    };
    scene
        .edit_animation(|clip| {
            clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
            clip.set_phase_frames_keeping_keys(Phase::Out, 6)
        })
        .unwrap();
    scene
}

fn engine() -> Engine {
    let mut show = ShowState::new(
        "scene animation",
        (PROGRAM..=OTHER)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    for (value, id) in [(SHARED_A, 1), (SHARED_B, 1), (OTHER, 2)] {
        show.set_scene_input(input(value), scene(id), 32, 18)
            .unwrap();
    }
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

fn animation(engine: &Engine, value: u128) -> SceneAnimation {
    engine
        .show()
        .scene_input(input(value))
        .unwrap()
        .animation()
        .clone()
}

fn edit(value: u128, base_revision: u64, change: SceneAnimationChange) -> EngineCommand {
    EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
        input: input(value),
        base_revision: Some(base_revision),
        change,
    })
}

fn faded_clip() -> SceneClip {
    let mut clip = scene(1).animation.clip;
    clip.set_track(Track::new(
        TrackKey::new(layer(), Phase::In, Property::Opacity),
        vec![
            Keyframe::new(0, Value::Scalar(0), Easing::Linear),
            Keyframe::new(10, Value::Scalar(255), Easing::Linear),
        ],
    ))
    .unwrap();
    clip.set_track(Track::new(
        TrackKey::new(layer(), Phase::Out, Property::Opacity),
        vec![
            Keyframe::new(0, Value::Scalar(255), Easing::Linear),
            Keyframe::new(6, Value::Scalar(0), Easing::Linear),
        ],
    ))
    .unwrap();
    clip
}

#[test]
fn scene_animation_edits_are_project_edits_shared_by_the_scene() {
    let mut engine = engine();
    let command = edit(
        SHARED_A,
        1,
        SceneAnimationChange::Set(Box::new(faded_clip())),
    );
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(
        scene_inputs_sharing(engine.show(), input(SHARED_B)),
        vec![input(SHARED_A), input(SHARED_B)]
    );
    let outcome = accept(&mut engine, "set", command);
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>(),
        vec![EngineEvent::SceneAnimationChanged {
            input: input(SHARED_A)
        }]
    );
    let shared = animation(&engine, SHARED_A);
    assert_eq!(shared, SceneAnimation::new(faded_clip(), 2));
    assert_eq!(animation(&engine, SHARED_B), shared);
    assert_eq!(animation(&engine, OTHER), scene(2).animation);
    assert_eq!(engine.undo_entries().len(), 1);

    // A stale base revision conflicts through either sharing input.
    let outcome = execute(
        &mut engine,
        "stale",
        edit(
            SHARED_B,
            1,
            SceneAnimationChange::Stretch {
                phase: Phase::In,
                frames: 20,
            },
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(rejection(&outcome).fields[0].code, "revision_conflict");
    assert_eq!(animation(&engine, SHARED_A).revision, 2);
}

#[test]
fn presets_and_refusals_are_checked_against_the_scene() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preset",
        edit(
            OTHER,
            1,
            SceneAnimationChange::Preset {
                layer: layer(),
                phase: Phase::Out,
                preset: AnimationPreset::SlideFromRight,
                frames: 12,
            },
        ),
    );
    let mut expected = scene(2);
    expected
        .apply_animation_preset(
            layer(),
            Phase::Out,
            AnimationPreset::SlideFromRight,
            12,
            32,
            18,
        )
        .unwrap();
    assert_eq!(animation(&engine, OTHER), expected.animation);

    let outcome = execute(
        &mut engine,
        "unknown layer",
        edit(
            OTHER,
            2,
            SceneAnimationChange::Preset {
                layer: LayerId::new(9).unwrap(),
                phase: Phase::In,
                preset: AnimationPreset::Fade,
                frames: 5,
            },
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(rejection(&outcome).fields[0].code, "unknown_layer");

    let mut colored = scene(2).animation.clip;
    colored
        .set_track(Track::new(
            TrackKey::new(layer(), Phase::In, Property::Color),
            vec![Keyframe::new(
                0,
                Value::Color(fm_model::fm_animation::Rgba::new(1, 2, 3, 4)),
                Easing::Linear,
            )],
        ))
        .unwrap();
    let outcome = execute(
        &mut engine,
        "color",
        edit(OTHER, 2, SceneAnimationChange::Set(Box::new(colored))),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].code, "unsupported_property");

    let outcome = execute(
        &mut engine,
        "not a scene",
        edit(
            PROGRAM,
            0,
            SceneAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(animation(&engine, OTHER).revision, 2);
}

#[test]
fn a_clip_bound_refuses_edits_it_cannot_carry() {
    let mut engine = engine();
    engine.set_scene_clip_bound(|clip| clip.tracks().len() < 2);
    let outcome = execute(
        &mut engine,
        "too large",
        edit(
            SHARED_A,
            1,
            SceneAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert_eq!(rejection(&outcome).fields[0].code, "too_large");
}

#[test]
fn undo_and_redo_restore_exact_clips_and_advance_the_revision() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preset",
        edit(
            SHARED_A,
            1,
            SceneAnimationChange::Preset {
                layer: layer(),
                phase: Phase::In,
                preset: AnimationPreset::Pop,
                frames: 15,
            },
        ),
    );
    let edited = animation(&engine, SHARED_A);
    assert_eq!(edited.revision, 2);
    accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    let undone = animation(&engine, SHARED_B);
    assert_eq!(undone.clip, scene(1).animation.clip);
    assert_eq!(undone.revision, 3);
    accept(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    let redone = animation(&engine, SHARED_A);
    assert_eq!(redone.clip, edited.clip);
    assert_eq!(redone.revision, 4);
}

#[test]
fn padlocks_cover_every_input_showing_the_scene() {
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
        edit(
            SHARED_A,
            1,
            SceneAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    accept(
        &mut engine,
        "other",
        edit(OTHER, 1, SceneAnimationChange::Set(Box::new(faded_clip()))),
    );
    accept(
        &mut engine,
        "project lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "project locked",
        edit(OTHER, 2, SceneAnimationChange::Set(Box::new(faded_clip()))),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
}

/// The pose of `value` on each of the next `frames` frames.
fn poses(
    engine: &mut Engine,
    value: u128,
    frames: usize,
) -> Vec<Option<(TitleAirPhase, ClipState)>> {
    (0..frames)
        .map(|_| {
            let frame = engine.tick().unwrap();
            frame
                .scenes
                .iter()
                .find(|scene| scene.input == input(value))
                .map(|scene| (scene.frame.phase, scene.frame.sample))
        })
        .collect()
}

fn in_at(numerator: u128, denominator: u128) -> (TitleAirPhase, ClipState) {
    (
        TitleAirPhase::In,
        ClipState::In(ClipPosition::new(numerator, denominator)),
    )
}

#[test]
fn preview_and_program_play_the_in_phase_with_exact_cross_rate_timing() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(SHARED_A)),
    );
    // Clip frame k * 30 / 25 on output frame k: 10 clip frames take 9.
    let shown = poses(&mut engine, SHARED_A, 10);
    assert_eq!(shown[0], Some(in_at(0, 1)));
    assert_eq!(shown[1], Some(in_at(6, 5)));
    assert_eq!(shown[8], Some(in_at(48, 5)));
    assert_eq!(shown[9], Some((TitleAirPhase::Hold, ClipState::Hold)));
    assert_eq!(engine.scene_pose(input(SHARED_A)), ClipState::Hold);
    // The engine snapshots mid-phase: scene phases are runtime only.
    accept(&mut engine, "cut", EngineCommand::Cut);
    let shown = poses(&mut engine, SHARED_A, 2);
    // A cut to Program restarts the In phase there.
    assert_eq!(shown[0], Some(in_at(0, 1)));
    assert!(engine.scenes_in_motion());
    assert!(engine.snapshot().is_ok());
    // The old Program (a plain input) has nothing to animate; scene 1 is
    // not on Preview, so only Program tracks it.
    assert_eq!(engine.scenes_on_air().len(), 1);
}

#[test]
fn a_transition_plays_the_outgoing_out_phase_and_a_cut_does_not() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(SHARED_A)),
    );
    accept(&mut engine, "cut", EngineCommand::Cut);
    poses(&mut engine, SHARED_A, 12);
    accept(
        &mut engine,
        "preview other",
        EngineCommand::SelectPreview(input(OTHER)),
    );
    poses(&mut engine, OTHER, 12);
    accept(
        &mut engine,
        "fade",
        EngineCommand::Fade {
            duration_frames: 10,
        },
    );
    let mut outgoing = Vec::new();
    let mut incoming = Vec::new();
    for _ in 0..10 {
        let frame = engine.tick().unwrap();
        let pose = |value| {
            frame
                .scenes
                .iter()
                .find(|scene| scene.input == input(value))
                .map(|scene| (scene.frame.phase, scene.frame.sample))
        };
        outgoing.push(pose(SHARED_A));
        incoming.push(pose(OTHER));
    }
    // Out (6 clip frames, 5 output frames) from the transition's first frame,
    // then its last frame held while the fade still shows it.
    assert_eq!(
        outgoing[0],
        Some((TitleAirPhase::Out, ClipState::Out(ClipPosition::frame(0))))
    );
    assert_eq!(
        outgoing[4],
        Some((TitleAirPhase::Out, ClipState::Out(ClipPosition::new(24, 5))))
    );
    assert_eq!(
        outgoing[5],
        Some((TitleAirPhase::Out, ClipState::Out(ClipPosition::frame(6))))
    );
    // The incoming scene plays In from the same frame.
    assert_eq!(incoming[0], Some(in_at(0, 1)));
    // Once the take completes, the old Program lands on Preview and plays
    // its In phase there.
    let after = poses(&mut engine, SHARED_A, 1);
    assert_eq!(after[0].map(|pose| pose.0), Some(TitleAirPhase::In));

    // A cut swaps instantly: the outgoing scene shows no Out frame.
    let mut engine = engine_on_program(SHARED_A);
    accept(
        &mut engine,
        "preview plain",
        EngineCommand::SelectPreview(input(PREVIEW)),
    );
    poses(&mut engine, SHARED_A, 12);
    accept(&mut engine, "cut away", EngineCommand::Cut);
    let shown = poses(&mut engine, SHARED_A, 1);
    // Straight to Preview, entering again.
    assert_eq!(shown[0], Some(in_at(0, 1)));
}

fn engine_on_program(value: u128) -> Engine {
    let mut engine = engine();
    accept(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(value)),
    );
    accept(&mut engine, "cut", EngineCommand::Cut);
    engine
}

#[test]
fn an_edit_changes_the_next_phase_and_a_restored_engine_holds() {
    let mut engine = engine_on_program(SHARED_A);
    poses(&mut engine, SHARED_A, 12);
    assert_eq!(engine.scene_phase_frames(input(SHARED_A), Phase::In), 9);
    accept(
        &mut engine,
        "stretch",
        edit(
            SHARED_B,
            1,
            SceneAnimationChange::Stretch {
                phase: Phase::In,
                frames: 30,
            },
        ),
    );
    assert_eq!(engine.scene_phase_frames(input(SHARED_A), Phase::In), 25);
    engine.tick().unwrap();
    let restored = Engine::restore(engine.snapshot().unwrap()).unwrap();
    assert_eq!(restored.scenes_on_air(), Vec::new());
    assert_eq!(restored.scene_pose(input(SHARED_A)), ClipState::Hold);
    assert_eq!(animation(&engine, SHARED_A).clip.in_frames(), 30);
}
