//! Project grades compiled for scene builders.

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU128},
    sync::Arc,
};

use fm_engine::{CompiledProjectGrades, GradeCompileError, GradeTarget};
use fm_grade::{CompiledGrade, CubeKind, Grade, GradeLut, builtin_preset, parse_cube};
use fm_model::{
    GradeId, GradeSource, Input, InputKind, LutId, Project, ProjectGrade, ProjectGrading,
    ProjectLut, ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo, lut_asset_uri,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

const INVERT_CUBE: &str = "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

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
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Graded",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 9).unwrap(),
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
        },
    );
    for id in 1..=4 {
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

fn warm() -> Grade {
    builtin_preset("warm").unwrap().grade.clone()
}

#[test]
fn an_ungraded_project_compiles_to_nothing() {
    let grades = CompiledProjectGrades::compile(&project(), &BTreeMap::new()).unwrap();
    assert!(grades.is_empty());
    assert_eq!(grades.program(), None);
    assert_eq!(grades.missing_luts().len(), 0);
}

#[test]
fn inputs_and_program_compile_and_share_library_grades() {
    let mut project = project();
    project
        .add_grade(ProjectGrade {
            id: grade_id(1),
            name: "Warm".into(),
            grade: warm(),
        })
        .unwrap();
    project
        .add_grade(ProjectGrade {
            id: grade_id(2),
            name: "Neutral".into(),
            grade: Grade::IDENTITY,
        })
        .unwrap();
    for input in [1, 2] {
        project
            .set_input_grade(input_id(input), Some(GradeSource::Library(grade_id(1))))
            .unwrap();
    }
    // Neutral and disabled grades are omitted, so those inputs render as before.
    project
        .set_input_grade(input_id(3), Some(GradeSource::Library(grade_id(2))))
        .unwrap();
    project
        .set_input_grade(
            input_id(4),
            Some(GradeSource::Inline(Box::new(Grade {
                enabled: false,
                ..warm()
            }))),
        )
        .unwrap();
    let program = Grade {
        exposure_millistops: -1000,
        ..Grade::IDENTITY
    };
    project
        .set_program_grade(Some(GradeSource::Inline(Box::new(program.clone()))))
        .unwrap();

    let grades = CompiledProjectGrades::compile(&project, &BTreeMap::new()).unwrap();
    let first = grades.input(input_id(1)).unwrap();
    let second = grades.input(input_id(2)).unwrap();
    assert!(Arc::ptr_eq(first, second), "a library grade compiles once");
    assert_eq!(**first, CompiledGrade::new(&warm(), None).unwrap());
    assert_eq!(grades.input(input_id(3)), None);
    assert_eq!(grades.input(input_id(4)), None);
    assert_eq!(
        grades.inputs().keys().copied().collect::<Vec<_>>(),
        vec![input_id(1), input_id(2)]
    );
    let compiled_program = grades.program().unwrap();
    assert_eq!(compiled_program.grade(), &program);
    // One stop down darkens neutral grey and keeps it neutral.
    let darker = compiled_program.apply_rgb8([128, 128, 128]);
    assert!(darker[0] < 128 && darker == [darker[0]; 3], "{darker:?}");
}

#[test]
fn luts_are_attached_and_missing_ones_are_bypassed_and_reported() {
    let mut project = project();
    project
        .add_lut(ProjectLut {
            id: lut_id(1),
            name: "Invert".into(),
            kind: CubeKind::ThreeD,
            size: 2,
        })
        .unwrap();
    let lut_grade = Grade {
        lut: Some(GradeLut::new(lut_asset_uri(lut_id(1)), 1000)),
        ..Grade::IDENTITY
    };
    project
        .set_input_grade(input_id(1), Some(GradeSource::Inline(Box::new(lut_grade))))
        .unwrap();

    let (invert, _) = parse_cube(INVERT_CUBE).unwrap();
    let mut luts = BTreeMap::new();
    luts.insert(lut_id(1), Arc::new(invert));
    let grades = CompiledProjectGrades::compile(&project, &luts).unwrap();
    let grade = grades.input(input_id(1)).unwrap();
    assert!(!grade.lut_missing());
    assert_eq!(grade.apply_rgb8([0, 255, 64]), [255, 0, 191]);
    assert_eq!(grades.missing_luts().len(), 0);

    // Without the parsed LUT the stage is bypassed (an identity grade here)
    // and the LUT is reported.
    let grades = CompiledProjectGrades::compile(&project, &BTreeMap::new()).unwrap();
    assert_eq!(grades.input(input_id(1)), None);
    assert_eq!(grades.missing_luts(), &[lut_id(1)]);
}

#[test]
fn dangling_and_out_of_bounds_grades_are_typed_errors() {
    let mut project = project();
    let mut inputs = BTreeMap::new();
    inputs.insert(input_id(1), GradeSource::Library(grade_id(7)));
    project.set_grading(ProjectGrading {
        inputs,
        ..ProjectGrading::default()
    });
    assert_eq!(
        CompiledProjectGrades::compile(&project, &BTreeMap::new()),
        Err(GradeCompileError::UnknownGrade(grade_id(7)))
    );
    project.set_grading(ProjectGrading {
        program: Some(GradeSource::Inline(Box::new(Grade {
            saturation_permille: 3001,
            ..Grade::IDENTITY
        }))),
        ..ProjectGrading::default()
    });
    assert!(matches!(
        CompiledProjectGrades::compile(&project, &BTreeMap::new()),
        Err(GradeCompileError::Invalid {
            target: GradeTarget::Program,
            ..
        })
    ));
}
