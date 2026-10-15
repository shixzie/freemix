use std::num::{NonZeroU32, NonZeroU128};

use fm_grade::{CubeKind, Grade, GradeLut, builtin_preset};
use fm_model::{
    EntityRef, GradeEditError, GradeId, GradeSource, Input, InputKind, LutId, MAX_GRADE_NAME_BYTES,
    MAX_PROJECT_GRADES, MAX_PROJECT_LUTS, Project, ProjectGrade, ProjectGrading, ProjectLut,
    ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo, ValidationErrorKind,
    lut_asset_key, lut_asset_uri, lut_id_from_uri,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn grade_id(value: u32) -> GradeId {
    GradeId::new(NonZeroU32::new(value).unwrap())
}

fn lut_id(value: u32) -> LutId {
    LutId::new(NonZeroU32::new(value).unwrap())
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    let settings = ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(64, 36).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::Rgba8,
            scan: ScanMode::Progressive,
            color: ColorMetadata::default(),
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
    };
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Graded",
        settings,
    );
    for id in [1, 2] {
        project.add_input(Input {
            id: input_id(id),
            name: format!("Input {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project
}

fn lut(id: u32, name: &str) -> ProjectLut {
    ProjectLut {
        id: lut_id(id),
        name: name.into(),
        kind: CubeKind::ThreeD,
        size: 17,
    }
}

fn warm() -> Grade {
    builtin_preset("warm").unwrap().grade.clone()
}

fn with_lut(id: u32) -> Grade {
    Grade {
        lut: Some(GradeLut::new(lut_asset_uri(lut_id(id)), 750)),
        ..warm()
    }
}

/// Ids go above the highest in use, never back into a gap, and wrap to the
/// lowest free id only once `u32::MAX` is taken.
#[test]
fn ids_are_allocated_above_the_highest_and_wrap_after_u32_max() {
    let ids = |values: &[u32]| {
        values
            .iter()
            .map(|value| NonZeroU32::new(*value).unwrap())
            .collect::<Vec<_>>()
    };
    let id = |value: u32| NonZeroU32::new(value);
    assert_eq!(fm_model::next_free_id(ids(&[]), |_| false), id(1));
    assert_eq!(fm_model::next_free_id(ids(&[1, 4]), |_| false), id(5));
    assert_eq!(
        fm_model::next_free_id(ids(&[1, 4]), |candidate| candidate.get() == 5),
        id(6)
    );
    assert_eq!(
        fm_model::next_free_id(ids(&[1, u32::MAX]), |_| false),
        id(2)
    );
    assert_eq!(
        fm_model::next_free_id(ids(&[1, u32::MAX - 1]), |candidate| candidate.get() <= 2
            || candidate.get() == u32::MAX),
        id(3)
    );
}

#[test]
fn lut_uris_name_bundled_files_by_id() {
    assert_eq!(lut_asset_uri(lut_id(7)), "asset://luts/7.cube");
    assert_eq!(lut_asset_key(lut_id(7)), "luts/7.cube");
    assert_eq!(lut_id_from_uri("asset://luts/42.cube"), Some(lut_id(42)));
    for other in [
        "asset://luts/0.cube",
        "asset://luts/07.cube",
        "asset://luts/7.CUBE",
        "asset://luts/x.cube",
        "asset://luts/.cube",
        "asset://luts/sub/7.cube",
        "asset://luts/99999999999.cube",
        "asset://media/7.cube",
    ] {
        assert_eq!(lut_id_from_uri(other), None, "{other}");
    }
}

#[test]
fn inputs_and_program_take_library_or_inline_grades() {
    let mut project = project();
    project.add_lut(lut(1, "Film")).unwrap();
    project
        .add_grade(ProjectGrade {
            id: grade_id(1),
            name: "Warm film".into(),
            grade: with_lut(1),
        })
        .unwrap();
    project
        .set_input_grade(input_id(1), Some(GradeSource::Library(grade_id(1))))
        .unwrap();
    project
        .set_program_grade(Some(GradeSource::Inline(Box::new(Grade {
            exposure_millistops: 500,
            ..Grade::IDENTITY
        }))))
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
    let grading = project.grading();
    assert_eq!(
        grading.resolve(project.input_grade(input_id(1)).unwrap()),
        Some(&with_lut(1))
    );
    assert_eq!(project.input_grade(input_id(2)), None);
    assert_eq!(grading.referenced_luts(), vec![lut_id(1)]);
    assert!(grading.lut_in_use(lut_id(1)));
    assert!(grading.grade_in_use(grade_id(1)));

    // In-use library grades and LUTs cannot be removed.
    assert_eq!(
        project.remove_lut(lut_id(1)),
        Err(GradeEditError::LutInUse(lut_id(1)))
    );
    assert_eq!(
        project.remove_grade(grade_id(1)),
        Err(GradeEditError::GradeInUse(grade_id(1)))
    );

    // Replacing a library grade changes every target that references it.
    project.replace_grade(grade_id(1), warm()).unwrap();
    assert_eq!(
        project
            .grading()
            .resolve(project.input_grade(input_id(1)).unwrap()),
        Some(&warm())
    );
    project.remove_lut(lut_id(1)).unwrap();

    // Clearing the assignment frees the grade; removing the input drops it.
    project.set_input_grade(input_id(1), None).unwrap();
    project.remove_grade(grade_id(1)).unwrap();
    project
        .set_input_grade(input_id(2), Some(GradeSource::Inline(Box::new(warm()))))
        .unwrap();
    project.remove_input(input_id(2)).unwrap();
    assert!(project.grading().inputs.is_empty());
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn checked_edits_refuse_bad_references_names_and_bounds() {
    let mut project = project();
    let before = project.clone();
    assert_eq!(
        project.set_input_grade(input_id(9), Some(GradeSource::Inline(Box::new(warm())))),
        Err(GradeEditError::UnknownInput(input_id(9)))
    );
    assert_eq!(
        project.set_program_grade(Some(GradeSource::Library(grade_id(3)))),
        Err(GradeEditError::UnknownGrade(grade_id(3)))
    );
    assert_eq!(
        project.set_program_grade(Some(GradeSource::Inline(Box::new(with_lut(4))))),
        Err(GradeEditError::UnregisteredLut(
            "asset://luts/4.cube".into()
        ))
    );
    let hot = Grade {
        exposure_millistops: 4001,
        ..Grade::IDENTITY
    };
    assert!(matches!(
        project.set_input_grade(input_id(1), Some(GradeSource::Inline(Box::new(hot)))),
        Err(GradeEditError::InvalidGrade(_))
    ));
    for name in ["", "  ", "tab\there"] {
        assert!(
            project
                .add_grade(ProjectGrade {
                    id: grade_id(1),
                    name: name.into(),
                    grade: warm(),
                })
                .is_err(),
            "{name:?}"
        );
    }
    assert!(matches!(
        project.add_grade(ProjectGrade {
            id: grade_id(1),
            name: "x".repeat(MAX_GRADE_NAME_BYTES + 1),
            grade: warm(),
        }),
        Err(GradeEditError::NameTooLong { .. })
    ));
    for size in [1, 66] {
        assert_eq!(
            project.add_lut(ProjectLut {
                size,
                ..lut(1, "Bad")
            }),
            Err(GradeEditError::LutSizeOutOfRange {
                kind: CubeKind::ThreeD,
                size
            })
        );
    }
    assert!(
        project
            .add_lut(ProjectLut {
                kind: CubeKind::OneD,
                size: 65_536,
                ..lut(1, "Long 1D")
            })
            .is_ok()
    );
    assert_eq!(
        project.add_lut(lut(1, "Other")),
        Err(GradeEditError::DuplicateLutId(lut_id(1)))
    );
    assert_eq!(
        project.add_lut(lut(2, "Long 1D")),
        Err(GradeEditError::DuplicateName("Long 1D".into()))
    );
    assert_ne!(project, before);
}

#[test]
fn library_and_lut_counts_are_bounded() {
    let mut project = project();
    for id in 1..=u32::try_from(MAX_PROJECT_LUTS).unwrap() {
        project.add_lut(lut(id, &format!("LUT {id}"))).unwrap();
    }
    assert_eq!(
        project.add_lut(lut(1000, "One more")),
        Err(GradeEditError::TooManyLuts {
            maximum: MAX_PROJECT_LUTS
        })
    );
    for id in 1..=u32::try_from(MAX_PROJECT_GRADES).unwrap() {
        project
            .add_grade(ProjectGrade {
                id: grade_id(id),
                name: format!("Grade {id}"),
                grade: Grade::IDENTITY,
            })
            .unwrap();
    }
    assert_eq!(
        project.add_grade(ProjectGrade {
            id: grade_id(1000),
            name: "One more".into(),
            grade: Grade::IDENTITY,
        }),
        Err(GradeEditError::TooManyGrades {
            maximum: MAX_PROJECT_GRADES
        })
    );
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn validation_reports_every_unchecked_grading_problem() {
    let mut project = project();
    let mut inputs = std::collections::BTreeMap::new();
    inputs.insert(input_id(9), GradeSource::Inline(Box::new(warm())));
    inputs.insert(input_id(1), GradeSource::Library(grade_id(5)));
    project.set_grading(ProjectGrading {
        grades: vec![
            ProjectGrade {
                id: grade_id(1),
                name: "Same".into(),
                grade: with_lut(3),
            },
            ProjectGrade {
                id: grade_id(1),
                name: "Same".into(),
                grade: Grade {
                    lut: Some(GradeLut::new("asset://luts/film.cube", 1000)),
                    ..Grade::IDENTITY
                },
            },
        ],
        luts: vec![
            ProjectLut {
                size: 99,
                ..lut(1, "")
            },
            lut(1, "Dup"),
        ],
        inputs,
        layers: std::collections::BTreeMap::new(),
        program: Some(GradeSource::Inline(Box::new(Grade {
            saturation_permille: 3001,
            ..Grade::IDENTITY
        }))),
    });
    let errors = project.validate().unwrap_err();
    let has = |field: &str, kind: ValidationErrorKind| {
        errors
            .iter()
            .any(|error| error.field == field && error.kind == kind)
    };
    assert!(has("luts.name", ValidationErrorKind::Empty));
    assert!(has("luts.size", ValidationErrorKind::OutOfRange));
    assert!(has("luts.id", ValidationErrorKind::DuplicateId));
    assert!(has("grades.id", ValidationErrorKind::DuplicateId));
    assert!(has("grades.name", ValidationErrorKind::DuplicateName));
    assert!(has(
        "grades.grade",
        ValidationErrorKind::MissingReference(EntityRef::Lut(lut_id(3)))
    ));
    assert!(has("grades.grade", ValidationErrorKind::FormatMismatch));
    assert!(has(
        "input_grades.input",
        ValidationErrorKind::MissingReference(EntityRef::Input(input_id(9)))
    ));
    assert!(has(
        "input_grades.grade",
        ValidationErrorKind::MissingReference(EntityRef::Grade(grade_id(5)))
    ));
    assert!(has("program_grade", ValidationErrorKind::OutOfRange));
}

fn scene_id(value: u128) -> fm_types::SceneId {
    fm_types::SceneId::new(NonZeroU128::new(value).unwrap())
}

fn layer_ref(scene: u128, layer: u32) -> fm_model::SceneLayerRef {
    fm_model::SceneLayerRef::new(scene_id(scene), fm_model::LayerId::new(layer).unwrap())
}

/// `project()` plus scene 5 (shown by Scene input 9) with layers 1 and 2,
/// both showing input 1.
fn scene_project() -> Project {
    let mut project = project();
    project
        .add_scene_input_checked(scene_id(5), "Two up".into(), input_id(9), "Two up".into())
        .unwrap();
    for (id, name) in [(1, "Left"), (2, "Right")] {
        project
            .add_layer_to_scene(
                scene_id(5),
                fm_model::Layer {
                    id: fm_model::LayerId::new(id).unwrap(),
                    name: name.into(),
                    source: fm_model::SourceRef::Input(input_id(1)),
                    enabled: true,
                    geometry: fm_model::LayerGeometry::new(0, 0, 32, 18, fm_model::Rotation::Deg0),
                    crop: None,
                    mask: None,
                    key: None,
                    effects: Vec::new(),
                    opacity: u8::MAX,
                    z_order: 0,
                },
            )
            .unwrap();
    }
    project
}

#[test]
fn scene_layers_take_library_or_inline_grades_that_follow_their_layer() {
    let mut project = scene_project();
    project.add_lut(lut(3, "Film")).unwrap();
    project
        .add_grade(ProjectGrade {
            id: grade_id(1),
            name: "Night".into(),
            grade: with_lut(3),
        })
        .unwrap();
    project
        .set_layer_grade(layer_ref(5, 1), Some(GradeSource::Library(grade_id(1))))
        .unwrap();
    project
        .set_layer_grade(layer_ref(5, 2), Some(GradeSource::Inline(Box::new(warm()))))
        .unwrap();
    assert_eq!(project.validate(), Ok(()));
    assert_eq!(
        project.layer_grade(layer_ref(5, 1)),
        Some(&GradeSource::Library(grade_id(1)))
    );
    // A layer grade keeps its library grade and (through it) its LUT in use.
    assert!(project.grading().grade_in_use(grade_id(1)));
    assert!(project.grading().lut_in_use(lut_id(3)));
    assert_eq!(project.grading().referenced_luts(), vec![lut_id(3)]);
    assert_eq!(
        project.remove_grade(grade_id(1)),
        Err(GradeEditError::GradeInUse(grade_id(1)))
    );

    // Unknown scenes, layers, and library grades are refused.
    assert_eq!(
        project.set_layer_grade(layer_ref(6, 1), Some(GradeSource::Inline(Box::new(warm())))),
        Err(GradeEditError::UnknownScene(scene_id(6)))
    );
    assert_eq!(
        project.set_layer_grade(layer_ref(5, 3), Some(GradeSource::Inline(Box::new(warm())))),
        Err(GradeEditError::UnknownLayer(layer_ref(5, 3)))
    );
    assert_eq!(
        project.set_layer_grade(layer_ref(5, 1), Some(GradeSource::Library(grade_id(8)))),
        Err(GradeEditError::UnknownGrade(grade_id(8)))
    );

    // Duplicating the scene copies its layers' grades to the copy.
    project
        .duplicate_scene_input_checked(
            scene_id(5),
            scene_id(6),
            "Copy".into(),
            input_id(10),
            "Copy".into(),
        )
        .unwrap();
    assert_eq!(
        project.layer_grade(layer_ref(6, 2)),
        Some(&GradeSource::Inline(Box::new(warm())))
    );

    // Removing a layer drops its grade; removing a scene drops all of its.
    project.remove_layer_from_scene(scene_id(5), 0).unwrap();
    assert_eq!(project.layer_grade(layer_ref(5, 1)), None);
    assert!(project.layer_grade(layer_ref(5, 2)).is_some());
    project.remove_input(input_id(10)).unwrap();
    project.remove_scene(scene_id(6)).unwrap();
    assert!(
        project
            .grading()
            .layers
            .keys()
            .all(|target| target.scene == scene_id(5))
    );
    // Clearing the last use frees the library grade.
    assert!(!project.grading().grade_in_use(grade_id(1)));
    project.set_layer_grade(layer_ref(5, 2), None).unwrap();
    assert!(project.grading().layers.is_empty());
    assert_eq!(project.validate(), Ok(()));
}

#[test]
fn layer_grades_are_bounded_and_validated() {
    let mut grading = ProjectGrading::default();
    for layer in 1..=u32::try_from(fm_model::MAX_PROJECT_LAYER_GRADES).unwrap() {
        grading
            .set_layer(
                layer_ref(5, layer),
                Some(GradeSource::Inline(Box::new(warm()))),
            )
            .unwrap();
    }
    assert_eq!(
        grading.set_layer(
            layer_ref(5, 9_999),
            Some(GradeSource::Inline(Box::new(warm())))
        ),
        Err(GradeEditError::TooManyLayerGrades {
            maximum: fm_model::MAX_PROJECT_LAYER_GRADES
        })
    );
    // Replacing an existing layer's grade is not a new one.
    grading
        .set_layer(layer_ref(5, 1), Some(GradeSource::Inline(Box::new(warm()))))
        .unwrap();

    // Validation reports dangling scenes, layers, and grades.
    let mut project = scene_project();
    let mut grading = ProjectGrading::default();
    grading
        .layers
        .insert(layer_ref(7, 1), GradeSource::Inline(Box::new(warm())));
    grading
        .layers
        .insert(layer_ref(5, 4), GradeSource::Library(grade_id(2)));
    project.set_grading(grading);
    let errors = project.validate().unwrap_err();
    let has = |field: &str, kind: ValidationErrorKind| {
        errors
            .iter()
            .any(|error| error.field == field && error.kind == kind)
    };
    assert!(has(
        "layer_grades.scene",
        ValidationErrorKind::MissingReference(EntityRef::Scene(scene_id(7)))
    ));
    assert!(has(
        "layer_grades.layer",
        ValidationErrorKind::MissingReference(EntityRef::Scene(scene_id(5)))
    ));
    assert!(has(
        "layer_grades.grade",
        ValidationErrorKind::MissingReference(EntityRef::Grade(grade_id(2)))
    ));
}
