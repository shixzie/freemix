//! SW-019 stage 3: grade edits are engine commands on the shared undo/redo
//! history, guarded by the edit padlocks.

use core::num::{NonZeroU32, NonZeroU128};
use std::sync::Arc;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineGradeEdit, ShowState,
};
use fm_grade::{CubeKind, Grade, GradeLut, builtin_preset};
use fm_model::{
    GradeId, GradeSource, LutId, MAX_LUT_FILE_BYTES, ProjectGrade, ProjectGrading, lut_asset_uri,
};
use fm_types::{FrameRate, InputId};

const INVERT_CUBE: &str = "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn grade_id(value: u32) -> GradeId {
    GradeId::new(NonZeroU32::new(value).unwrap())
}

fn lut_id(value: u32) -> LutId {
    LutId::new(NonZeroU32::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "grade show",
        [1, 2, 3]
            .into_iter()
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let envelope =
        CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command);
    let outcome = engine.execute(envelope, 0).unwrap();
    engine.tick().unwrap();
    outcome
}

fn grade(edit: EngineGradeEdit) -> EngineCommand {
    EngineCommand::Grade(edit)
}

const UNDO: EngineCommand = EngineCommand::Undo {
    expected_edit: None,
};
const REDO: EngineCommand = EngineCommand::Redo {
    expected_edit: None,
};

fn accepted(outcome: &EngineCommandOutcome) {
    assert!(
        outcome.receipt.accepted().is_some(),
        "expected acceptance, got {:?}",
        outcome.receipt
    );
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

fn warm() -> Grade {
    builtin_preset("warm").unwrap().grade.clone()
}

fn cool() -> Grade {
    builtin_preset("cool").unwrap().grade.clone()
}

fn with_lut(lut: u32) -> Grade {
    Grade {
        lut: Some(GradeLut::new(lut_asset_uri(lut_id(lut)), 750)),
        ..warm()
    }
}

fn grading(engine: &Engine) -> &ProjectGrading {
    engine.show().grading()
}

fn import(id: u32, name: &str, source: &str) -> EngineCommand {
    grade(EngineGradeEdit::ImportLut {
        id: lut_id(id),
        name: name.to_owned(),
        source: Arc::from(source),
    })
}

fn add(id: u32, name: &str, value: Grade) -> EngineCommand {
    grade(EngineGradeEdit::AddGrade {
        grade: Box::new(ProjectGrade {
            id: grade_id(id),
            name: name.to_owned(),
            grade: value,
        }),
        index: None,
    })
}

#[test]
fn grade_edits_are_project_edits() {
    for edit in [
        EngineGradeEdit::SetInput {
            input: input(1),
            source: None,
        },
        EngineGradeEdit::SetProgram { source: None },
        EngineGradeEdit::RemoveGrade { id: grade_id(1) },
        EngineGradeEdit::RemoveLut { id: lut_id(1) },
    ] {
        assert_eq!(grade(edit).kind(), EngineCommandKind::ProjectEdit);
    }
}

#[test]
fn input_and_program_grades_undo_and_redo_with_events() {
    let mut engine = engine();
    let inline = GradeSource::Inline(Box::new(warm()));
    let outcome = run(
        &mut engine,
        "set-input",
        grade(EngineGradeEdit::SetInput {
            input: input(2),
            source: Some(inline.clone()),
        }),
    );
    accepted(&outcome);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::GradingChanged(EngineGradeEdit::SetInput {
            input: input(2),
            source: Some(inline.clone()),
        })
    );
    assert_eq!(grading(&engine).inputs.get(&input(2)), Some(&inline));
    let compiled = engine.show().compiled_grades().unwrap();
    assert!(compiled.input(input(2)).is_some());
    assert!(compiled.program().is_none());

    accepted(&run(
        &mut engine,
        "set-program",
        grade(EngineGradeEdit::SetProgram {
            source: Some(GradeSource::Inline(Box::new(cool()))),
        }),
    ));
    assert!(engine.show().compiled_grades().unwrap().program().is_some());

    // Undo restores the program, then the input, each with its event.
    let outcome = run(&mut engine, "undo-1", UNDO);
    accepted(&outcome);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::GradingChanged(EngineGradeEdit::SetProgram { source: None })
    );
    assert_eq!(grading(&engine).program, None);
    let outcome = run(&mut engine, "undo-2", UNDO);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::GradingChanged(EngineGradeEdit::SetInput {
            input: input(2),
            source: None,
        })
    );
    assert_eq!(grading(&engine).inputs.len(), 0);

    accepted(&run(&mut engine, "redo-1", REDO));
    accepted(&run(&mut engine, "redo-2", REDO));
    assert_eq!(grading(&engine).inputs.get(&input(2)), Some(&inline));
    assert_eq!(
        grading(&engine).program,
        Some(GradeSource::Inline(Box::new(cool())))
    );

    // Replacing a grade undoes to the previous one, not to "ungraded".
    accepted(&run(
        &mut engine,
        "replace",
        grade(EngineGradeEdit::SetInput {
            input: input(2),
            source: Some(GradeSource::Inline(Box::new(cool()))),
        }),
    ));
    accepted(&run(&mut engine, "undo-3", UNDO));
    assert_eq!(grading(&engine).inputs.get(&input(2)), Some(&inline));
}

#[test]
fn library_edits_undo_to_their_exact_previous_state() {
    let mut engine = engine();
    accepted(&run(&mut engine, "add-a", add(1, "A", warm())));
    accepted(&run(&mut engine, "add-b", add(2, "B", cool())));
    // Inserted at an explicit index.
    accepted(&run(
        &mut engine,
        "add-c",
        grade(EngineGradeEdit::AddGrade {
            grade: Box::new(ProjectGrade {
                id: grade_id(3),
                name: "C".into(),
                grade: warm(),
            }),
            index: Some(0),
        }),
    ));
    let ids = |engine: &Engine| {
        grading(engine)
            .grades
            .iter()
            .map(|grade| grade.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&engine), [grade_id(3), grade_id(1), grade_id(2)]);

    accepted(&run(
        &mut engine,
        "update",
        grade(EngineGradeEdit::UpdateGrade {
            id: grade_id(1),
            name: "A2".into(),
            grade: Box::new(cool()),
        }),
    ));
    accepted(&run(
        &mut engine,
        "remove",
        grade(EngineGradeEdit::RemoveGrade { id: grade_id(3) }),
    ));
    assert_eq!(ids(&engine), [grade_id(1), grade_id(2)]);

    // Undo the removal: C returns at index 0.
    accepted(&run(&mut engine, "undo-remove", UNDO));
    assert_eq!(ids(&engine), [grade_id(3), grade_id(1), grade_id(2)]);
    // Undo the update: A has its old name and values.
    accepted(&run(&mut engine, "undo-update", UNDO));
    let a = grading(&engine).grade(grade_id(1)).unwrap();
    assert_eq!((a.name.as_str(), &a.grade), ("A", &warm()));
    // Undo the appended add of B; redo appends it at the same index.
    accepted(&run(&mut engine, "undo-c", UNDO));
    accepted(&run(&mut engine, "undo-b", UNDO));
    assert_eq!(ids(&engine), [grade_id(1)]);
    accepted(&run(&mut engine, "redo-b", REDO));
    accepted(&run(&mut engine, "redo-c", REDO));
    assert_eq!(ids(&engine), [grade_id(3), grade_id(1), grade_id(2)]);

    // A grade in use cannot be removed; duplicates conflict.
    accepted(&run(
        &mut engine,
        "use",
        grade(EngineGradeEdit::SetInput {
            input: input(1),
            source: Some(GradeSource::Library(grade_id(2))),
        }),
    ));
    let outcome = run(
        &mut engine,
        "remove-used",
        grade(EngineGradeEdit::RemoveGrade { id: grade_id(2) }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    let outcome = run(&mut engine, "dup-id", add(2, "Other", warm()));
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    let outcome = run(&mut engine, "dup-name", add(9, "B", warm()));
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
}

#[test]
fn lut_import_is_validated_undoable_and_removal_forgets_history() {
    let mut engine = engine();
    let outcome = run(
        &mut engine,
        "bad",
        import(1, "Bad", "LUT_3D_SIZE 2\n0 0 0\n"),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    let oversized = "#".repeat(usize::try_from(MAX_LUT_FILE_BYTES).unwrap() + 1);
    let outcome = run(&mut engine, "huge", import(1, "Huge", &oversized));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    // A grade naming an unimported LUT is refused.
    let outcome = run(&mut engine, "early", add(1, "Early", with_lut(1)));
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);

    let outcome = run(&mut engine, "import", import(1, "Invert", INVERT_CUBE));
    accepted(&outcome);
    let lut = grading(&engine).lut(lut_id(1)).unwrap();
    assert_eq!((lut.kind, lut.size), (CubeKind::ThreeD, 2));
    assert_eq!(&**engine.show().luts()[&lut_id(1)].source(), INVERT_CUBE);

    // Undo removes the import; redo restores its exact text.
    accepted(&run(&mut engine, "undo-import", UNDO));
    assert_eq!(grading(&engine).luts.len(), 0);
    assert_eq!(engine.show().luts().len(), 0);
    accepted(&run(&mut engine, "redo-import", REDO));
    assert_eq!(&**engine.show().luts()[&lut_id(1)].source(), INVERT_CUBE);

    // A graded input compiles through the LUT; the LUT is then in use.
    accepted(&run(
        &mut engine,
        "grade",
        grade(EngineGradeEdit::SetInput {
            input: input(3),
            source: Some(GradeSource::Inline(Box::new(with_lut(1)))),
        }),
    ));
    let compiled = engine.show().compiled_grades().unwrap();
    assert_eq!(compiled.missing_luts().len(), 0);
    let outcome = run(
        &mut engine,
        "remove-used",
        grade(EngineGradeEdit::RemoveLut { id: lut_id(1) }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    accepted(&run(
        &mut engine,
        "clear",
        grade(EngineGradeEdit::SetInput {
            input: input(3),
            source: None,
        }),
    ));

    // Removing a LUT is accepted but not undoable, and forgets every history
    // entry that names it, so nothing can re-reference the removed text.
    accepted(&run(
        &mut engine,
        "remove",
        grade(EngineGradeEdit::RemoveLut { id: lut_id(1) }),
    ));
    assert_eq!(grading(&engine).luts.len(), 0);
    assert_eq!(engine.show().luts().len(), 0);
    // Undoing everything that is left never restores the LUT or leaves a
    // grade naming it.
    for attempt in 0..8 {
        let _ = run(&mut engine, &format!("undo-after-remove-{attempt}"), UNDO);
        assert_eq!(grading(&engine).luts.len(), 0);
        assert_eq!(grading(&engine).referenced_luts().len(), 0);
        assert!(engine.show().compiled_grades().is_ok());
    }
    assert!(
        engine.undo_entries().next().is_none(),
        "the history drains without restoring the LUT"
    );
}

#[test]
fn padlocks_guard_grade_edits() {
    let mut engine = engine();
    accepted(&run(
        &mut engine,
        "lock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(2)),
            locked: true,
        },
    ));
    let set = |value: u128| {
        grade(EngineGradeEdit::SetInput {
            input: input(value),
            source: Some(GradeSource::Inline(Box::new(warm()))),
        })
    };
    let outcome = run(&mut engine, "locked-input", set(2));
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    accepted(&run(&mut engine, "other-input", set(3)));
    accepted(&run(
        &mut engine,
        "program",
        grade(EngineGradeEdit::SetProgram {
            source: Some(GradeSource::Inline(Box::new(cool()))),
        }),
    ));

    accepted(&run(
        &mut engine,
        "lock-project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    for (key, command) in [
        ("p-input", set(3)),
        (
            "p-program",
            grade(EngineGradeEdit::SetProgram { source: None }),
        ),
        ("p-add", add(1, "A", warm())),
        ("p-import", import(1, "Invert", INVERT_CUBE)),
    ] {
        let outcome = run(&mut engine, key, command);
        assert_eq!(
            EditRejectionKind::of(rejection(&outcome)),
            Some(EditRejectionKind::ProjectLocked),
            "{key}"
        );
    }
}

/// Regression: a library grade a padlocked input uses is that input's look,
/// so updating the library grade is covered by the input's padlock (as an
/// animation edit is by the padlock of every title sharing the template).
#[test]
fn padlocks_guard_library_grades_a_locked_input_uses() {
    let mut engine = engine();
    accepted(&run(&mut engine, "add", add(1, "A", warm())));
    accepted(&run(
        &mut engine,
        "use",
        grade(EngineGradeEdit::SetInput {
            input: input(2),
            source: Some(GradeSource::Library(grade_id(1))),
        }),
    ));
    accepted(&run(
        &mut engine,
        "lock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(2)),
            locked: true,
        },
    ));
    let outcome = run(
        &mut engine,
        "update",
        grade(EngineGradeEdit::UpdateGrade {
            id: grade_id(1),
            name: "A".into(),
            grade: Box::new(cool()),
        }),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    assert_eq!(
        engine.show().grading().grade(grade_id(1)).unwrap().grade,
        warm()
    );

    // A library grade no locked input uses stays editable.
    accepted(&run(&mut engine, "add-b", add(2, "B", warm())));
    accepted(&run(
        &mut engine,
        "update-b",
        grade(EngineGradeEdit::UpdateGrade {
            id: grade_id(2),
            name: "B".into(),
            grade: Box::new(cool()),
        }),
    ));
}

#[test]
fn unknown_inputs_and_removed_inputs_drop_grades() {
    let mut engine = engine();
    let outcome = run(
        &mut engine,
        "unknown",
        grade(EngineGradeEdit::SetInput {
            input: input(9),
            source: Some(GradeSource::Inline(Box::new(warm()))),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);

    accepted(&run(
        &mut engine,
        "grade-3",
        grade(EngineGradeEdit::SetInput {
            input: input(3),
            source: Some(GradeSource::Inline(Box::new(warm()))),
        }),
    ));
    accepted(&run(
        &mut engine,
        "remove-3",
        EngineCommand::RemoveInput { input: input(3) },
    ));
    assert_eq!(grading(&engine).inputs.len(), 0);
}

/// Regression: LUT removal forgets the history entries that name the LUT,
/// which can leave an input add on top of the stack while the added input
/// still carries a grade those forgotten entries set. Undoing the add then
/// removed the input with its grade, and redo re-added it ungraded. Undoing
/// an add is refused as history drift while the added input is graded.
#[test]
fn undoing_an_input_add_never_drops_its_grade_after_a_lut_removal() {
    let mut engine = engine();
    accepted(&run(
        &mut engine,
        "import",
        import(1, "Invert", INVERT_CUBE),
    ));
    accepted(&run(
        &mut engine,
        "add-4",
        EngineCommand::AddInput {
            input: input(4),
            name: "Input 4".to_owned(),
            source: fm_engine::EngineInputSource::Bars,
        },
    ));
    accepted(&run(
        &mut engine,
        "grade-lut",
        grade(EngineGradeEdit::SetInput {
            input: input(4),
            source: Some(GradeSource::Inline(Box::new(with_lut(1)))),
        }),
    ));
    accepted(&run(
        &mut engine,
        "grade-cool",
        grade(EngineGradeEdit::SetInput {
            input: input(4),
            source: Some(GradeSource::Inline(Box::new(cool()))),
        }),
    ));
    accepted(&run(
        &mut engine,
        "remove-lut",
        grade(EngineGradeEdit::RemoveLut { id: lut_id(1) }),
    ));
    // Both grade entries named the LUT and were forgotten; the add is on top.
    let outcome = run(&mut engine, "undo-add", UNDO);
    let refused = rejection(&outcome);
    assert_eq!(refused.code, RejectionCode::Conflict, "{refused:?}");
    assert_eq!(
        EditRejectionKind::of(refused),
        Some(EditRejectionKind::HistoryDrift)
    );
    assert!(engine.show().inputs().contains(&input(4)));
    assert_eq!(
        grading(&engine).inputs.get(&input(4)),
        Some(&GradeSource::Inline(Box::new(cool())))
    );

    // Clearing the grade first makes the add undoable and redoable again
    // (the clear is undone first, so it is cleared again by hand here).
    accepted(&run(
        &mut engine,
        "clear",
        grade(EngineGradeEdit::SetInput {
            input: input(4),
            source: None,
        }),
    ));
    accepted(&run(&mut engine, "undo-clear", UNDO));
    let outcome = run(&mut engine, "undo-add-graded", UNDO);
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert!(grading(&engine).inputs.contains_key(&input(4)));
}

fn layer_engine() -> Engine {
    use fm_model::{
        Layer, LayerGeometry, LayerId, Rgba8, Rotation, Scene, SceneAnimation, SourceRef,
    };
    let mut show = ShowState::new(
        "layer grade show",
        [1, 2, 3, 4, 5]
            .into_iter()
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    // Inputs 3 and 4 show scene 7 (layers 1 and 2); input 5 shows scene 8;
    // no Scene input shows scene 9.
    let scene = |id: u128| Scene {
        id: fm_types::SceneId::new(NonZeroU128::new(id).unwrap()),
        name: format!("Scene {id}"),
        background: Rgba8::OPAQUE_BLACK,
        layers: [1, 2]
            .into_iter()
            .map(|layer| Layer {
                id: LayerId::new(layer).unwrap(),
                name: format!("Layer {layer}"),
                source: SourceRef::Input(input(1)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 16, 9, Rotation::Deg0),
                crop: None,
                mask: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 0,
            })
            .collect(),
        animation: SceneAnimation::default(),
    };
    for (value, id) in [(3, 7), (4, 7), (5, 8)] {
        show.set_scene_input(input(value), scene(id), 32, 18)
            .unwrap();
    }
    show.set_scene(scene(9), 32, 18);
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn layer_ref(scene: u128, layer: u32) -> fm_model::SceneLayerRef {
    fm_model::SceneLayerRef::new(
        fm_types::SceneId::new(NonZeroU128::new(scene).unwrap()),
        fm_model::LayerId::new(layer).unwrap(),
    )
}

fn scene_id(value: u128) -> fm_types::SceneId {
    fm_types::SceneId::new(NonZeroU128::new(value).unwrap())
}

fn set_layer(scene: u128, layer: u32, source: Option<GradeSource>) -> EngineCommand {
    grade(EngineGradeEdit::SetLayer {
        scene: scene_id(scene),
        layer: fm_model::LayerId::new(layer).unwrap(),
        source,
    })
}

/// SW-019: a scene layer takes a library or inline grade addressed by its
/// scene; the edit is undoable, emits its event, and compiles into the
/// show's layer grades.
#[test]
#[allow(clippy::too_many_lines)]
fn scene_layer_grades_undo_redo_and_compile() {
    let mut engine = layer_engine();
    assert_eq!(set_layer(3, 1, None).kind(), EngineCommandKind::ProjectEdit);
    accepted(&run(&mut engine, "add", add(1, "Night", cool())));
    let library = GradeSource::Library(grade_id(1));
    let outcome = run(&mut engine, "set", set_layer(7, 1, Some(library.clone())));
    accepted(&outcome);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::GradingChanged(EngineGradeEdit::SetLayer {
            scene: scene_id(7),
            layer: fm_model::LayerId::new(1).unwrap(),
            source: Some(library.clone()),
        })
    );
    assert_eq!(
        grading(&engine).layers.get(&layer_ref(7, 1)),
        Some(&library)
    );
    let compiled = engine.show().compiled_grades().unwrap();
    assert!(compiled.layer(layer_ref(7, 1)).is_some());
    assert!(compiled.layer(layer_ref(7, 2)).is_none());
    assert!(compiled.input(input(3)).is_none());

    // The library grade is now in use by the layer.
    let outcome = run(
        &mut engine,
        "remove-used",
        grade(EngineGradeEdit::RemoveGrade { id: grade_id(1) }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);

    // Editing again replaces the grade; undo restores the library grade,
    // then nothing.
    let inline = GradeSource::Inline(Box::new(warm()));
    accepted(&run(
        &mut engine,
        "replace",
        set_layer(7, 1, Some(inline.clone())),
    ));
    assert_eq!(grading(&engine).layers.get(&layer_ref(7, 1)), Some(&inline));
    accepted(&run(&mut engine, "undo-1", UNDO));
    assert_eq!(
        grading(&engine).layers.get(&layer_ref(7, 1)),
        Some(&library)
    );
    let outcome = run(&mut engine, "undo-2", UNDO);
    accepted(&outcome);
    assert!(grading(&engine).layers.is_empty());
    assert!(
        engine
            .show()
            .compiled_grades()
            .unwrap()
            .layer(layer_ref(7, 1))
            .is_none()
    );
    accepted(&run(&mut engine, "redo-1", REDO));
    accepted(&run(&mut engine, "redo-2", REDO));
    assert_eq!(grading(&engine).layers.get(&layer_ref(7, 1)), Some(&inline));

    // Scene 8 is a different scene; scene 9, which no Scene input shows,
    // is graded the same way.
    accepted(&run(
        &mut engine,
        "other",
        set_layer(8, 2, Some(library.clone())),
    ));
    accepted(&run(
        &mut engine,
        "unshown",
        set_layer(9, 1, Some(inline.clone())),
    ));
    assert_eq!(grading(&engine).layers.len(), 3);
    assert!(
        engine
            .show()
            .compiled_grades()
            .unwrap()
            .layer(layer_ref(9, 1))
            .is_some()
    );
    accepted(&run(&mut engine, "undo-unshown", UNDO));
    assert!(!grading(&engine).layers.contains_key(&layer_ref(9, 1)));
    accepted(&run(&mut engine, "clear", set_layer(7, 1, None)));
    assert_eq!(
        grading(&engine).layers.keys().copied().collect::<Vec<_>>(),
        [layer_ref(8, 2)]
    );

    // Unknown targets are refused without effect.
    for (key, command, field, issue) in [
        (
            "not-scene",
            set_layer(99, 1, Some(inline.clone())),
            "scene",
            "unknown_scene",
        ),
        (
            "no-layer",
            set_layer(7, 9, Some(inline.clone())),
            "layer",
            "unknown_layer",
        ),
        (
            "no-grade",
            set_layer(7, 1, Some(GradeSource::Library(grade_id(9)))),
            "grade",
            "unknown_grade",
        ),
    ] {
        let outcome = run(&mut engine, key, command);
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, RejectionCode::NotFound, "{key}");
        assert!(
            rejection
                .fields
                .iter()
                .any(|field_issue| field_issue.field == field && field_issue.code == issue),
            "{key}: {rejection:?}"
        );
    }
    assert_eq!(grading(&engine).layers.len(), 1);
}

/// A layer grade is part of every Scene input showing the scene: each of
/// their padlocks guards it, as does the padlock of an input whose layers
/// use a library grade being updated. A scene no Scene input shows is
/// guarded by the project padlock alone.
#[test]
fn padlocks_of_every_scene_input_guard_its_layer_grades() {
    let mut engine = layer_engine();
    accepted(&run(&mut engine, "add", add(1, "Night", cool())));
    accepted(&run(
        &mut engine,
        "use",
        set_layer(7, 2, Some(GradeSource::Library(grade_id(1)))),
    ));
    accepted(&run(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(4)),
            locked: true,
        },
    ));
    for (key, command) in [
        (
            "shared",
            set_layer(7, 1, Some(GradeSource::Inline(Box::new(warm())))),
        ),
        ("shared-clear", set_layer(7, 1, None)),
        (
            "update-library",
            grade(EngineGradeEdit::UpdateGrade {
                id: grade_id(1),
                name: "Night".into(),
                grade: Box::new(warm()),
            }),
        ),
    ] {
        let outcome = run(&mut engine, key, command);
        assert_eq!(
            EditRejectionKind::of(rejection(&outcome)),
            Some(EditRejectionKind::InputLocked),
            "{key}"
        );
    }
    assert_eq!(grading(&engine).layers.len(), 1);
    // Scenes 8 and 9 share nothing with the locked input.
    accepted(&run(
        &mut engine,
        "other-scene",
        set_layer(8, 1, Some(GradeSource::Inline(Box::new(warm())))),
    ));
    accepted(&run(
        &mut engine,
        "unshown-scene",
        set_layer(9, 1, Some(GradeSource::Inline(Box::new(warm())))),
    ));
    accepted(&run(
        &mut engine,
        "lock-project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    let outcome = run(&mut engine, "unshown-locked", set_layer(9, 1, None));
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
}
