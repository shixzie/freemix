//! Project grading at schema 31: strict `project.json` encoding of the grade
//! library, imported LUTs, and per-input and program grades; `.cube` import
//! into `assets/luts/<id>.cube`; and LUT loading.

use std::{
    collections::BTreeMap,
    fs,
    num::{NonZeroU32, NonZeroU128},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_grade::{
    Contrast, CubeError, CubeKind, Curve, CurvePoint, Grade, GradeLut, HueCurve, HueCurves,
    HuePoint, RgbCurves, Wheel, Wheels, WhiteBalance, parse_cube,
};
use fm_model::{
    GradeId, GradeSource, Input, InputKind, LutId, MAX_LUT_FILE_BYTES, Project, ProjectGrade,
    ProjectGrading, ProjectLut, ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo,
    lut_asset_uri,
};
use fm_persistence::{
    BundleError, LutError, ProjectPosition, ProjectStore, ProjectValidationError, RuntimeRouting,
    StoreError, StoredProject,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-grading-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(name)).unwrap()
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A 2x2x2 LUT that inverts every channel.
const INVERT_CUBE: &str = "TITLE \"Invert\"\nLUT_3D_SIZE 2\n\
1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

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
        ProjectId::new(NonZeroU128::new(5).unwrap()),
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

fn stored(project: Project) -> StoredProject {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

/// A grade that exercises every stage with non-default values.
fn full_grade() -> Grade {
    Grade {
        enabled: true,
        white_balance: WhiteBalance::new(-120, 45),
        exposure_millistops: -750,
        contrast: Contrast::new(200, 4500),
        saturation_permille: 1300,
        wheels: Wheels {
            lift: Wheel::new(10, -20, 30),
            gamma: Wheel::new(-40, 50, -60),
            gain: Wheel::new(70, 80, 90),
            offset: Wheel::new(0, 0, -100),
        },
        curves: RgbCurves {
            master: Curve::new(vec![
                CurvePoint::new(0, 500),
                CurvePoint::new(5000, 5200),
                CurvePoint::new(10_000, 9500),
            ]),
            red: Curve::IDENTITY,
            green: Curve::IDENTITY,
            blue: Curve::new(vec![CurvePoint::new(0, 0), CurvePoint::new(10_000, 9000)]),
        },
        hue_curves: HueCurves {
            hue_vs_hue: HueCurve::new(vec![HuePoint::new(1200, -300)]),
            hue_vs_saturation: HueCurve::EMPTY,
            luma_vs_saturation: HueCurve::new(vec![
                HuePoint::new(0, -500),
                HuePoint::new(10_000, 200),
            ]),
        },
        lut: Some(GradeLut::new(lut_asset_uri(lut_id(3)), 600)),
    }
}

fn graded_project() -> Project {
    let mut project = project();
    let mut inputs = BTreeMap::new();
    inputs.insert(
        input_id(2),
        GradeSource::Inline(Box::new(Grade {
            exposure_millistops: 1000,
            ..Grade::IDENTITY
        })),
    );
    inputs.insert(input_id(1), GradeSource::Library(grade_id(9)));
    project.set_grading(ProjectGrading {
        grades: vec![ProjectGrade {
            id: grade_id(9),
            name: "Night \"blue\"".into(),
            grade: full_grade(),
        }],
        luts: vec![ProjectLut {
            id: lut_id(3),
            name: "Film".into(),
            kind: CubeKind::ThreeD,
            size: 33,
        }],
        inputs,
        program: Some(GradeSource::Library(grade_id(9))),
    });
    project
}

const FULL_GRADE_JSON: &str = "{\"enabled\": true, \"white_balance\": {\"temperature\": -120, \"tint\": 45}, \"exposure_millistops\": -750, \"contrast\": {\"amount\": 200, \"pivot_bp\": 4500}, \"saturation_permille\": 1300, \"wheels\": {\"lift\": {\"x\": 10, \"y\": -20, \"master\": 30}, \"gamma\": {\"x\": -40, \"y\": 50, \"master\": -60}, \"gain\": {\"x\": 70, \"y\": 80, \"master\": 90}, \"offset\": {\"x\": 0, \"y\": 0, \"master\": -100}}, \"curves\": {\"master\": [{\"x\": 0, \"y\": 500}, {\"x\": 5000, \"y\": 5200}, {\"x\": 10000, \"y\": 9500}], \"red\": [{\"x\": 0, \"y\": 0}, {\"x\": 10000, \"y\": 10000}], \"green\": [{\"x\": 0, \"y\": 0}, {\"x\": 10000, \"y\": 10000}], \"blue\": [{\"x\": 0, \"y\": 0}, {\"x\": 10000, \"y\": 9000}]}, \"hue_curves\": {\"hue_vs_hue\": [{\"position\": 1200, \"value\": -300}], \"hue_vs_saturation\": [], \"luma_vs_saturation\": [{\"position\": 0, \"value\": -500}, {\"position\": 10000, \"value\": 200}]}, \"lut\": {\"asset_uri\": \"asset://luts/3.cube\", \"strength_permille\": 600}}";

#[test]
fn grading_round_trips_exactly_and_is_strict_at_schema_31() {
    let temp = TestDirectory::new("round-trip");
    let store = temp.store("show.freemix");
    let expected = stored(graded_project());
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.starts_with("{\n  \"schema_version\": 31,\n"));
    let grading = format!(
        "\n    \"luts\": [{{\"id\": 3, \"name\": \"Film\", \"kind\": \"3d\", \"size\": 33}}],\
         \n    \"grades\": [{{\"id\": 9, \"name\": \"Night \\\"blue\\\"\", \"grade\": {FULL_GRADE_JSON}}}],\
         \n    \"input_grades\": [{{\"input\": 1, \"grade\": {{\"type\": \"library\", \"grade\": 9}}}}, \
         {{\"input\": 2, \"grade\": {{\"type\": \"inline\", \"grade\": {{\"enabled\": true, "
    );
    assert!(
        encoded.contains(&grading),
        "unexpected grading in {encoded}"
    );
    assert!(
        encoded.contains(
            "\n    \"program_grade\": {\"type\": \"library\", \"grade\": 9}\n  },\n  \"runtime\": {"
        ),
        "unexpected program grade in {encoded}"
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.project().grading(), graded_project().grading());

    // Every field is required, typed, and closed.
    for malformed in [
        encoded.replacen("\n    \"luts\": [", "\n    \"lutz\": [", 1),
        encoded.replacen(",\n    \"program_grade\": {\"type\": \"library\", \"grade\": 9}", "", 1),
        encoded.replacen("\"kind\": \"3d\"", "\"kind\": \"4d\"", 1),
        encoded.replacen("\"type\": \"library\"", "\"type\": \"preset\"", 1),
        encoded.replacen("\"exposure_millistops\": -750", "\"exposure_millistops\": 40000", 1),
        encoded.replacen("\"exposure_millistops\": -750", "\"exposure_millistops\": \"-750\"", 1),
        encoded.replacen("\"pivot_bp\": 4500", "\"pivot_bp\": -1", 1),
        encoded.replacen("\"enabled\": true, \"white", "\"enabled\": 1, \"white", 1),
        encoded.replacen("{\"x\": 0, \"y\": 500}", "{\"x\": 0, \"y\": 500, \"z\": 1}", 1),
        encoded.replacen("\"strength_permille\": 600}", "\"strength_permille\": 600, \"x\": 1}", 1),
        encoded.replacen("\"size\": 33}", "\"size\": 33, \"bytes\": 1}", 1),
        encoded.replacen("{\"id\": 3,", "{\"id\": 0,", 1),
        encoded.replacen(
            "{\"input\": 2, \"grade\":",
            "{\"input\": 1, \"grade\": {\"type\": \"library\", \"grade\": 9}}, {\"input\": 2, \"grade\":",
            1,
        ),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let result = store.load();
        assert!(
            matches!(result, Err(StoreError::MalformedManifest { .. })),
            "accepted or misreported malformed grading: {result:?}"
        );
    }

    // Bounds, names, and references are domain errors.
    for invalid in [
        encoded.replacen(
            "\"exposure_millistops\": -750",
            "\"exposure_millistops\": 4001",
            1,
        ),
        encoded.replacen(
            "\"asset_uri\": \"asset://luts/3.cube\"",
            "\"asset_uri\": \"asset://luts/4.cube\"",
            1,
        ),
        encoded.replacen(
            "\"asset_uri\": \"asset://luts/3.cube\"",
            "\"asset_uri\": \"asset://luts/film.cube\"",
            1,
        ),
        encoded.replacen(
            "{\"type\": \"library\", \"grade\": 9}\n",
            "{\"type\": \"library\", \"grade\": 8}\n",
            1,
        ),
        encoded.replacen("\"size\": 33", "\"size\": 66", 1),
        encoded.replacen("\"name\": \"Film\"", "\"name\": \" \"", 1),
        encoded.replacen("{\"input\": 2, \"grade\":", "{\"input\": 3, \"grade\":", 1),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), &invalid).unwrap();
        let result = store.load();
        assert!(
            matches!(
                result,
                Err(StoreError::Validation(
                    ProjectValidationError::DomainProject(_)
                ))
            ),
            "accepted or misreported invalid grading: {result:?}"
        );
    }
}

#[test]
fn a_schema_30_manifest_is_refused() {
    let temp = TestDirectory::new("schema-30");
    let store = temp.store("show.freemix");
    store.save(&stored(graded_project())).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();

    // The previous schema is refused outright; there is no migration path.
    fs::write(
        store.manifest_path(),
        encoded.replacen("\"schema_version\": 31", "\"schema_version\": 30", 1),
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::Validation(
            ProjectValidationError::UnsupportedSchema {
                found: 30,
                supported: 31
            }
        ))
    ));
}

#[test]
fn an_ungraded_project_writes_empty_grading() {
    let temp = TestDirectory::new("ungraded");
    let store = temp.store("show.freemix");
    store.save(&stored(project())).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains(
        ",\n    \"luts\": [],\n    \"grades\": [],\n    \"input_grades\": [],\n    \"program_grade\": null\n  },"
    ));
    assert_eq!(store.load().unwrap(), stored(project()));
}

#[test]
fn lut_import_copies_the_parsed_bytes_and_registers_by_id() {
    let temp = TestDirectory::new("import");
    let store = temp.store("show.freemix");
    let mut project = project();
    let source = temp.file("invert.cube", INVERT_CUBE.as_bytes());

    let imported = store.import_lut(&mut project, &source, "Invert").unwrap();
    assert_eq!(
        imported.lut,
        ProjectLut {
            id: lut_id(1),
            name: "Invert".into(),
            kind: CubeKind::ThreeD,
            size: 2,
        }
    );
    assert_eq!(imported.bytes, INVERT_CUBE.len() as u64);
    assert_eq!(imported.warnings, Vec::new());
    let bundled = store.assets_root().join("luts/1.cube");
    assert_eq!(fs::read_to_string(&bundled).unwrap(), INVERT_CUBE);
    assert_eq!(project.luts(), std::slice::from_ref(&imported.lut));
    assert_eq!(imported.lut.asset_uri(), "asset://luts/1.cube");

    // The bundled LUT loads to exactly what the source parses to.
    let loaded = store.load_lut(&imported.lut).unwrap();
    assert_eq!(*loaded, parse_cube(INVERT_CUBE).unwrap().0);

    // A second import takes the next id; an orphaned file is never reused.
    fs::write(store.assets_root().join("luts/2.cube"), "orphan").unwrap();
    let second = store.import_lut(&mut project, &source, "Invert 2").unwrap();
    assert_eq!(second.lut.id, lut_id(3));
    assert_eq!(
        fs::read_to_string(store.assets_root().join("luts/2.cube")).unwrap(),
        "orphan"
    );

    // Only LUTs a grade references are loaded.
    assert!(store.load_grade_luts(&project).unwrap().is_empty());
    project
        .set_program_grade(Some(GradeSource::Inline(Box::new(Grade {
            lut: Some(GradeLut::new(lut_asset_uri(lut_id(3)), 1000)),
            ..Grade::IDENTITY
        }))))
        .unwrap();
    let luts = store.load_grade_luts(&project).unwrap();
    assert_eq!(luts.keys().copied().collect::<Vec<_>>(), vec![lut_id(3)]);

    // The project saves and reloads with its LUT table.
    store.save(&stored(project.clone())).unwrap();
    assert_eq!(store.load().unwrap().project(), &project);
}

#[test]
fn lut_import_refuses_hostile_and_invalid_files_without_leaving_any() {
    let temp = TestDirectory::new("hostile");
    let store = temp.store("show.freemix");
    let mut project = project();
    let before = project.clone();

    let huge = temp.file("huge.cube", b"");
    fs::File::options()
        .write(true)
        .open(&huge)
        .unwrap()
        .set_len(MAX_LUT_FILE_BYTES + 1)
        .unwrap();
    assert!(matches!(
        store.import_lut(&mut project, &huge, "Huge"),
        Err(LutError::TooLarge { maximum }) if maximum == MAX_LUT_FILE_BYTES
    ));

    let oversized_edge = temp.file("edge.cube", b"LUT_3D_SIZE 66\n0 0 0\n");
    assert!(matches!(
        store.import_lut(&mut project, &oversized_edge, "Edge"),
        Err(LutError::Parse(CubeError::SizeOutOfRange { .. }))
    ));
    let garbage = temp.file("garbage.cube", b"LUT_3D_SIZE 2\n0 0 zero\n");
    assert!(matches!(
        store.import_lut(&mut project, &garbage, "Garbage"),
        Err(LutError::Parse(_))
    ));
    let binary = temp.file("binary.cube", &[0xff, 0xfe, 0x00]);
    assert!(matches!(
        store.import_lut(&mut project, &binary, "Binary"),
        Err(LutError::NotUtf8)
    ));
    assert!(matches!(
        store.import_lut(&mut project, temp.0.as_path(), "Directory"),
        Err(LutError::NotRegularFile(_) | LutError::Io(_))
    ));
    let good = temp.file("good.cube", INVERT_CUBE.as_bytes());
    assert!(matches!(
        store.import_lut(&mut project, &good, "  "),
        Err(LutError::Project(_))
    ));
    assert_eq!(project, before);
    let luts = store.assets_root().join("luts");
    assert!(
        fs::read_dir(&luts).map_or(true, |mut entries| entries.next().is_none()),
        "a refused import left a file behind"
    );
}

#[test]
fn loading_refuses_a_bundled_lut_that_no_longer_matches() {
    let temp = TestDirectory::new("mismatch");
    let store = temp.store("show.freemix");
    let mut project = project();
    let source = temp.file("invert.cube", INVERT_CUBE.as_bytes());
    let lut = store
        .import_lut(&mut project, &source, "Invert")
        .unwrap()
        .lut;

    fs::write(
        store.assets_root().join("luts/1.cube"),
        "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n",
    )
    .unwrap();
    assert!(matches!(
        store.load_lut(&lut),
        Err(LutError::Mismatch {
            expected: (CubeKind::ThreeD, 2),
            found: (CubeKind::OneD, 2)
        })
    ));
    fs::remove_file(store.assets_root().join("luts/1.cube")).unwrap();
    assert!(matches!(store.load_lut(&lut), Err(LutError::Asset(_))));
}

#[test]
fn portable_bundles_carry_imported_luts() {
    let temp = TestDirectory::new("bundle");
    let source = temp.store("show.freemix");
    let mut project = project();
    let file = temp.file("invert.cube", INVERT_CUBE.as_bytes());
    source.import_lut(&mut project, &file, "Invert").unwrap();
    project
        .set_input_grade(
            input_id(1),
            Some(GradeSource::Inline(Box::new(Grade {
                lut: Some(GradeLut::new(lut_asset_uri(lut_id(1)), 500)),
                ..Grade::IDENTITY
            }))),
        )
        .unwrap();
    source.save(&stored(project.clone())).unwrap();

    let destination = temp.store("copy.freemix");
    let export = source.export_portable_bundle(&destination).unwrap();
    assert_eq!(export.project.project(), &project);
    assert!(
        export
            .assets
            .iter()
            .any(|asset| asset.bundled_uri == "asset://luts/1.cube")
    );
    assert_eq!(
        fs::read_to_string(destination.assets_root().join("luts/1.cube")).unwrap(),
        INVERT_CUBE
    );
    let reopened = destination.load().unwrap();
    assert_eq!(reopened.project(), &project);
    assert!(destination.load_grade_luts(reopened.project()).is_ok());

    // A registered LUT whose file is gone refuses the export.
    fs::remove_file(source.assets_root().join("luts/1.cube")).unwrap();
    let missing = temp.store("missing.freemix");
    assert!(matches!(
        source.export_portable_bundle(&missing),
        Err(BundleError::UnresolvedLut { uri }) if uri == "asset://luts/1.cube"
    ));
    assert!(!missing.root().exists());
}
