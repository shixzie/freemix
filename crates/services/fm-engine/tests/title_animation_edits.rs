//! GX-011 stage 4a: title animation edits are engine project edits —
//! revision-checked, padlocked, undoable, and shared by every title input
//! naming the same template.
//!
//! Inputs 3 and 4 share the template keyed `titles/lower.fmtitle`; input 5
//! has its own template under another key. The template is a 32x18 canvas
//! with one box element.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineTitle, EngineTitleAnimationEdit, ShowState,
    TitleAnimationChange, title_template_inputs,
};
use fm_titles::fm_animation::{
    AnimationPreset, Easing, Keyframe, Phase, Property, Track, TrackKey, Value,
};
use fm_titles::{
    Bounds, Color, Element, ElementId, ElementKind, Style, TemplateId, TitleAirTiming,
    TitleAnimation, TitleClip, TitleFieldSet, TitleTemplate,
};
use fm_types::{FrameRate, InputId};

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

fn element() -> ElementId {
    ElementId::new(nonzero(1))
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

fn engine() -> Engine {
    let template = template();
    let mut show = ShowState::new(
        "animation edits",
        (PROGRAM..=OTHER)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    for (value, key) in [
        (SHARED_A, "titles/lower.fmtitle"),
        (SHARED_B, "titles/lower.fmtitle"),
        (OTHER, "titles/other.fmtitle"),
    ] {
        show.set_title(
            input(value),
            EngineTitle::new(
                "Lower third",
                TitleFieldSet::from_template(&template).unwrap(),
            )
            .with_template(key, template.clone()),
        )
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

fn animation(engine: &Engine, value: u128) -> TitleAnimation {
    engine
        .show()
        .title(input(value))
        .unwrap()
        .animation()
        .unwrap()
        .clone()
}

fn edit(value: u128, base_revision: u64, change: TitleAnimationChange) -> EngineCommand {
    EngineCommand::TitleAnimation(EngineTitleAnimationEdit {
        input: input(value),
        base_revision: Some(base_revision),
        change,
    })
}

fn faded_clip() -> TitleClip {
    let mut clip = template().animation.clip;
    clip.set_track(Track::new(
        TrackKey::new(element(), Phase::In, Property::Opacity),
        vec![
            Keyframe::new(0, Value::Scalar(0), Easing::Linear),
            Keyframe::new(10, Value::Scalar(255), Easing::Linear),
        ],
    ))
    .unwrap();
    clip
}

#[test]
fn animation_edits_are_project_edits_shared_by_the_template() {
    let mut engine = engine();
    let command = edit(
        SHARED_A,
        1,
        TitleAnimationChange::Set(Box::new(faded_clip())),
    );
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(
        title_template_inputs(engine.show(), input(SHARED_B)),
        vec![input(SHARED_A), input(SHARED_B)]
    );

    let outcome = accept(&mut engine, "set", command);
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>(),
        vec![EngineEvent::TitleAnimationChanged {
            input: input(SHARED_A)
        }]
    );
    let shared = animation(&engine, SHARED_A);
    assert_eq!(shared, TitleAnimation::new(faded_clip(), 2));
    assert_eq!(animation(&engine, SHARED_B), shared);
    assert_eq!(animation(&engine, OTHER), template().animation);
    assert_eq!(engine.undo_entries().len(), 1);
}

#[test]
fn a_stale_base_revision_is_a_conflict() {
    let mut engine = engine();
    accept(
        &mut engine,
        "stretch",
        edit(
            SHARED_A,
            1,
            TitleAnimationChange::Stretch {
                phase: Phase::In,
                frames: 20,
            },
        ),
    );
    assert_eq!(animation(&engine, SHARED_B).revision, 2);
    assert_eq!(animation(&engine, SHARED_B).clip.in_frames(), 20);
    assert_eq!(
        engine.show().title(input(SHARED_B)).unwrap().timing,
        TitleAirTiming::of(&animation(&engine, SHARED_B))
    );

    // A second editor still based on revision 1 conflicts, through either
    // sharing input.
    let outcome = execute(
        &mut engine,
        "stale",
        edit(
            SHARED_B,
            1,
            TitleAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    let refused = rejection(&outcome);
    assert_eq!(refused.code, RejectionCode::Conflict);
    assert_eq!(refused.fields[0].code, "revision_conflict");
    assert_eq!(animation(&engine, SHARED_A).revision, 2);
    assert_eq!(engine.undo_entries().len(), 1);
}

#[test]
fn presets_and_refusals_are_checked_against_the_template() {
    let mut engine = engine();
    accept(
        &mut engine,
        "preset",
        edit(
            OTHER,
            1,
            TitleAnimationChange::Preset {
                element: element(),
                phase: Phase::Out,
                preset: AnimationPreset::Fade,
                frames: 12,
            },
        ),
    );
    let mut expected = template();
    expected
        .apply_animation_preset(element(), Phase::Out, AnimationPreset::Fade, 12)
        .unwrap();
    assert_eq!(animation(&engine, OTHER), expected.animation);
    assert_eq!(animation(&engine, SHARED_A), template().animation);

    let outcome = execute(
        &mut engine,
        "unknown element",
        edit(
            OTHER,
            2,
            TitleAnimationChange::Preset {
                element: ElementId::new(nonzero(77)),
                phase: Phase::In,
                preset: AnimationPreset::Fade,
                frames: 5,
            },
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(rejection(&outcome).fields[0].code, "unknown_element");

    let outcome = execute(
        &mut engine,
        "not a title",
        edit(
            PROGRAM,
            0,
            TitleAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(animation(&engine, OTHER).revision, 2);
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
            TitleAnimationChange::Preset {
                element: element(),
                phase: Phase::In,
                preset: AnimationPreset::SlideFromLeft,
                frames: 15,
            },
        ),
    );
    let edited = animation(&engine, SHARED_A);
    assert_eq!(edited.revision, 2);

    let outcome = accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>(),
        vec![EngineEvent::TitleAnimationChanged {
            input: input(SHARED_A)
        }]
    );
    let undone = animation(&engine, SHARED_B);
    assert_eq!(undone.clip, template().animation.clip);
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

    // A client edit after the undo bases itself on the latest revision.
    accept(
        &mut engine,
        "after",
        edit(
            SHARED_B,
            4,
            TitleAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    accept(
        &mut engine,
        "undo again",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(animation(&engine, SHARED_A).clip, edited.clip);
}

#[test]
fn padlocks_cover_every_input_sharing_the_template() {
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
            TitleAnimationChange::Set(Box::new(faded_clip())),
        ),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    // The other template is not covered.
    accept(
        &mut engine,
        "other",
        edit(OTHER, 1, TitleAnimationChange::Set(Box::new(faded_clip()))),
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
        edit(OTHER, 2, TitleAnimationChange::Set(Box::new(faded_clip()))),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(animation(&engine, SHARED_A).revision, 1);
    assert_eq!(animation(&engine, OTHER).revision, 2);
}
