//! End-to-end colour grading through the local CLI: grade commands edit the
//! saved project, renders apply input grades before the layer transform and
//! the program grade after composition, and grades survive save and restore.

use std::{
    fs,
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_grade::{CompiledGrade, Grade, GradeLut, builtin_preset};
use fm_model::{GradeId, GradeSource, LutId, lut_asset_uri};
use fm_persistence::ProjectStore;
use fm_types::InputId;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const INVERT_CUBE: &str =
    "TITLE \"Invert\"\nLUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

/// The colours `freemix-cli new` gives inputs 1 and 2.
const INPUT_1: [u8; 3] = [73, 151, 199];
const INPUT_2: [u8; 3] = [146, 46, 142];

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemix-cli-grading-{}-{}-{name}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invoke(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
        .args(arguments)
        .output()
        .unwrap()
}

fn succeed(arguments: &[&str]) -> String {
    let output = invoke(arguments);
    assert!(
        output.status.success(),
        "{arguments:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fail(arguments: &[&str], expected: &str) {
    let output = invoke(arguments);
    assert!(
        !output.status.success(),
        "{arguments:?} unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "{arguments:?}: stderr did not contain `{expected}`: {stderr}"
    );
}

/// Creates a project and shrinks it to 4x4 so scene renders compose 1:1.
fn new_project(root: &Root) -> PathBuf {
    let project = root.path("show.freemix");
    succeed(&["new", project.to_str().unwrap()]);
    let manifest = project.join("project.json");
    let source = fs::read_to_string(&manifest)
        .unwrap()
        .replacen("\"width\": 1920,", "\"width\": 4,", 1)
        .replacen("\"height\": 1080,", "\"height\": 4,", 1);
    fs::write(&manifest, source).unwrap();
    project
}

fn render(project: &Path, output: &Path) -> Vec<u8> {
    succeed(&[
        "render",
        project.to_str().unwrap(),
        output.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    fs::read(output).unwrap()
}

fn solid(frame: &[u8]) -> [u8; 3] {
    let header = b"P6\n4 4\n255\n";
    assert!(frame.starts_with(header));
    let pixels = &frame[header.len()..];
    assert_eq!(pixels.len(), 4 * 4 * 3);
    let first = [pixels[0], pixels[1], pixels[2]];
    assert!(
        pixels.chunks(3).all(|pixel| pixel == first),
        "frame is not solid"
    );
    first
}

fn compiled(grade: &Grade) -> CompiledGrade {
    CompiledGrade::new(grade, None).unwrap()
}

fn input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

#[test]
fn input_and_program_grades_render_and_round_trip_through_the_saved_project() {
    let root = Root::new("inline");
    let project = new_project(&root);
    let path = project.to_str().unwrap();

    let ungraded = render(&project, &root.path("ungraded.ppm"));
    assert_eq!(solid(&ungraded), INPUT_1);

    // An inline input grade: the warm preset with one stop more exposure.
    let output = succeed(&[
        "grade-set",
        path,
        "1",
        "--preset",
        "warm",
        "--exposure",
        "1000",
    ]);
    assert!(output.contains("grade-set: graded input 1"), "{output}");
    let warm = Grade {
        exposure_millistops: 1000,
        ..builtin_preset("warm").unwrap().grade.clone()
    };
    let graded = render(&project, &root.path("graded.ppm"));
    assert_ne!(graded, ungraded, "a graded render differs from ungraded");
    let expected_input = compiled(&warm).apply_rgb8(INPUT_1);
    assert_eq!(solid(&graded), expected_input);

    // The program grade colours the picture after the input grade.
    succeed(&["grade-set", path, "program", "--preset", "bw"]);
    let bw = builtin_preset("bw").unwrap().grade.clone();
    let both = render(&project, &root.path("both.ppm"));
    let expected_program = compiled(&bw).apply_rgb8(expected_input);
    assert_eq!(solid(&both), expected_program);
    assert_eq!(expected_program[0], expected_program[1]);

    // Both grades are in the saved project and restore exactly.
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(
        stored.project().input_grade(input_id(1)),
        Some(&GradeSource::Inline(Box::new(warm.clone())))
    );
    assert_eq!(
        stored.project().program_grade(),
        Some(&GradeSource::Inline(Box::new(bw)))
    );
    assert!(
        fs::read_to_string(project.join("project.json"))
            .unwrap()
            .contains("\"schema_version\": 63")
    );
    let listed = succeed(&["grade-list", path]);
    assert!(listed.contains("input 1: inline enabled=true exposure=1000 temperature=250"));
    assert!(listed.contains("program: inline"));
    let restored = render(&project, &root.path("restored.ppm"));
    assert_eq!(restored, both, "a restored project renders identically");

    // Clearing both grades restores the ungraded render exactly.
    succeed(&["grade-clear", path, "1"]);
    succeed(&["grade-clear", path, "program"]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert!(stored.project().grading().inputs.is_empty());
    assert_eq!(stored.project().program_grade(), None);
    assert!(succeed(&["grade-list", path]).contains("program: ungraded"));
    assert_eq!(render(&project, &root.path("cleared.ppm")), ungraded);
}

#[test]
fn imported_luts_and_library_grades_colour_scene_layers() {
    let root = Root::new("lut");
    let project = new_project(&root);
    let path = project.to_str().unwrap();
    let cube = root.path("Invert.cube");
    fs::write(&cube, INVERT_CUBE).unwrap();

    let imported = succeed(&["grade-import-lut", path, cube.to_str().unwrap()]);
    assert!(
        imported.contains("LUT 1 \"Invert\" ThreeD size 2"),
        "{imported}"
    );
    assert!(imported.contains("asset://luts/1.cube"), "{imported}");
    assert_eq!(
        fs::read_to_string(project.join("assets/luts/1.cube")).unwrap(),
        INVERT_CUBE
    );

    // A library grade using the LUT, assigned to input 2 by reference.
    let saved = succeed(&[
        "grade-set",
        path,
        "2",
        "--lut",
        "1",
        "--save-as",
        "Inverted",
    ]);
    assert!(saved.contains("saved library grade 1"), "{saved}");

    // Input 2 on a scene layer: the layer carries its input's grade.
    succeed(&["scene-input-add", path, "3", "11", "Program"]);
    succeed(&["scene-layer-add", path, "11", "2", "0", "Layer"]);
    succeed(&["preview", path, "3"]);
    succeed(&["cut", path]);
    let inverted = INPUT_2.map(|channel| 255 - channel);
    assert_eq!(solid(&render(&project, &root.path("scene.ppm"))), inverted);

    // Input 1 takes the same library grade; the program darkens the result.
    succeed(&["grade-set", path, "1", "--grade", "1"]);
    succeed(&["grade-set", path, "program", "--exposure", "-1000"]);
    let darker = Grade {
        exposure_millistops: -1000,
        ..Grade::IDENTITY
    };
    assert_eq!(
        solid(&render(&project, &root.path("scene-program.ppm"))),
        compiled(&darker).apply_rgb8(inverted)
    );

    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let grade_one = GradeId::new(NonZeroU32::new(1).unwrap());
    for input in [1, 2] {
        assert_eq!(
            stored.project().input_grade(input_id(input)),
            Some(&GradeSource::Library(grade_one))
        );
    }
    let library = &stored.project().grades()[0];
    assert_eq!(library.name, "Inverted");
    assert_eq!(
        library.grade.lut,
        Some(GradeLut::new(
            lut_asset_uri(LutId::new(NonZeroU32::new(1).unwrap())),
            1000
        ))
    );
    let listed = succeed(&["grade-list", path]);
    for line in [
        "lut id=1 name=\"Invert\" kind=ThreeD size=2 uri=asset://luts/1.cube",
        "grade id=1 name=\"Inverted\"",
        "input 1: library 1",
        "input 2: library 1",
        "program: inline",
    ] {
        assert!(listed.contains(line), "missing `{line}` in {listed}");
    }

    // A missing bundled LUT degrades the render instead of failing it.
    fs::remove_file(project.join("assets/luts/1.cube")).unwrap();
    let output = succeed(&[
        "render",
        path,
        root.path("degraded.ppm").to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert!(output.contains("degraded: LUT 1 bypassed"), "{output}");
    assert_eq!(
        solid(&fs::read(root.path("degraded.ppm")).unwrap()),
        compiled(&darker).apply_rgb8(INPUT_2)
    );
}

#[test]
fn refused_grade_edits_leave_the_project_unchanged() {
    let root = Root::new("refused");
    let project = new_project(&root);
    let path = project.to_str().unwrap();
    let manifest = project.join("project.json");
    let before = fs::read_to_string(&manifest).unwrap();

    fail(
        &["grade-set", path, "9", "--preset", "warm"],
        "input 9 does not exist",
    );
    fail(
        &["grade-set", path, "1", "--preset", "teal"],
        "unknown grade preset `teal`",
    );
    fail(
        &["grade-set", path, "1", "--exposure", "4001"],
        "invalid grade",
    );
    fail(
        &["grade-set", path, "1", "--lut", "7"],
        "not an imported LUT",
    );
    fail(
        &["grade-set", path, "1", "--grade", "3"],
        "grade 3 does not exist",
    );
    fail(
        &["grade-set", path, "1", "--grade", "1", "--exposure", "10"],
        "--grade cannot be combined",
    );
    fail(
        &["grade-set", path, "1", "--lut-strength", "10"],
        "--lut-strength requires --lut",
    );
    fail(&["grade-set", path, "nope"], "invalid grade target");

    // Hostile and invalid LUTs are refused and leave no bundled file.
    let oversized = root.path("edge.cube");
    fs::write(&oversized, "LUT_3D_SIZE 66\n").unwrap();
    fail(
        &["grade-import-lut", path, oversized.to_str().unwrap()],
        "invalid .cube LUT",
    );
    let huge = root.path("huge.cube");
    fs::File::create(&huge)
        .unwrap()
        .set_len(fm_model::MAX_LUT_FILE_BYTES + 1)
        .unwrap();
    fail(
        &["grade-import-lut", path, huge.to_str().unwrap()],
        "exceeds the",
    );
    assert!(!project.join("assets/luts/1.cube").exists());

    // The project padlock refuses grade edits.
    let locked = before.replacen(
        "\"edit_locks\": {\"project\": false",
        "\"edit_locks\": {\"project\": true",
        1,
    );
    assert_ne!(locked, before);
    fs::write(&manifest, &locked).unwrap();
    fail(
        &["grade-set", path, "program", "--preset", "warm"],
        "project is locked",
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), locked);
    fs::write(&manifest, &before).unwrap();

    assert_eq!(fs::read_to_string(&manifest).unwrap(), before);
}

/// Regression: a client may take `u32::MAX` as a LUT or library grade id,
/// after which the CLI's `max + 1` allocation failed for good. Allocation
/// takes the smallest unused id above the highest, wrapping to the lowest
/// free one; a LUT id whose bundled file exists (a removed LUT, whose older
/// manifests and autosaves still name that file) is never reused.
#[test]
fn ids_wrap_to_the_lowest_free_one_after_the_highest_is_taken() {
    let root = Root::new("exhausted");
    let project = new_project(&root);
    let path = project.to_str().unwrap();
    let store = ProjectStore::new(&project).unwrap();
    let stored = store.load().unwrap();
    let mut edited = stored.project().clone();
    let top = NonZeroU32::new(u32::MAX).unwrap();
    edited
        .add_lut(fm_model::ProjectLut {
            id: LutId::new(top),
            name: "Top".into(),
            kind: fm_grade::CubeKind::ThreeD,
            size: 2,
        })
        .unwrap();
    edited
        .add_grade(fm_model::ProjectGrade {
            id: GradeId::new(top),
            name: "Top".into(),
            grade: Grade::IDENTITY,
        })
        .unwrap();
    store.save(&stored.with_project(edited).unwrap()).unwrap();
    store
        .write_lut_source(LutId::new(top), INVERT_CUBE)
        .unwrap();
    // LUT 1 was imported and removed earlier: its file stays, so does its id.
    store
        .write_lut_source(LutId::new(NonZeroU32::new(1).unwrap()), INVERT_CUBE)
        .unwrap();

    let cube = root.path("Second.cube");
    fs::write(&cube, INVERT_CUBE).unwrap();
    let imported = succeed(&["grade-import-lut", path, cube.to_str().unwrap()]);
    assert!(imported.contains("asset://luts/2.cube"), "{imported}");
    let saved = succeed(&["grade-set", path, "1", "--save-as", "Low"]);
    assert!(saved.contains("saved library grade 1"), "{saved}");
}

/// SW-019: `grade-set <show> layer <scene> <layer>` grades one scene layer
/// after its input's grade; the grade is listed, saved, rendered, guarded by
/// the padlock of the Scene input showing the scene, cleared, and dropped
/// with its layer.
#[test]
fn scene_layer_grades_run_after_the_input_grade_and_follow_their_layer() {
    let root = Root::new("layer");
    let project = new_project(&root);
    let path = project.to_str().unwrap();
    let manifest = project.join("project.json");
    succeed(&["scene-input-add", path, "3", "11", "Program"]);
    succeed(&["scene-layer-add", path, "11", "2", "0", "Layer"]);
    succeed(&["preview", path, "3"]);
    succeed(&["cut", path]);
    assert_eq!(solid(&render(&project, &root.path("plain.ppm"))), INPUT_2);

    let darker = Grade {
        exposure_millistops: -1000,
        ..Grade::IDENTITY
    };
    let set = succeed(&["grade-set", path, "layer", "11", "1", "--exposure", "-1000"]);
    assert!(set.contains("scene 11 layer 1"), "{set}");
    assert_eq!(
        solid(&render(&project, &root.path("layer.ppm"))),
        compiled(&darker).apply_rgb8(INPUT_2)
    );
    // The input grade runs first, then the layer's.
    succeed(&["grade-set", path, "2", "--preset", "warm"]);
    let warm = &builtin_preset("warm").unwrap().grade;
    assert_eq!(
        solid(&render(&project, &root.path("both.ppm"))),
        compiled(&darker).apply_rgb8(compiled(warm).apply_rgb8(INPUT_2))
    );
    let listed = succeed(&["grade-list", path]);
    assert!(
        listed.contains("scene 11 layer 1: inline"),
        "missing layer grade in {listed}"
    );
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let target = fm_model::SceneLayerRef::new(
        fm_types::SceneId::new(NonZeroU128::new(11).unwrap()),
        fm_model::LayerId::new(1).unwrap(),
    );
    assert_eq!(
        stored.project().layer_grade(target),
        Some(&GradeSource::Inline(Box::new(darker)))
    );

    // Unknown scenes and layers are refused; so is a layer of a scene whose
    // Scene input is padlocked.
    let before = fs::read_to_string(&manifest).unwrap();
    fail(
        &["grade-set", path, "layer", "12", "1", "--preset", "warm"],
        "scene 12 does not exist",
    );
    fail(
        &["grade-set", path, "layer", "11", "5", "--preset", "warm"],
        "scene 11 layer 5 does not exist",
    );
    let locked = before.replacen(
        "\"edit_locks\": {\"project\": false, \"inputs\": [",
        "\"edit_locks\": {\"project\": false, \"inputs\": [3",
        1,
    );
    assert_ne!(locked, before);
    fs::write(&manifest, &locked).unwrap();
    fail(
        &["grade-clear", path, "layer", "11", "1"],
        "input 3 is locked",
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), locked);
    fs::write(&manifest, &before).unwrap();

    succeed(&["grade-clear", path, "layer", "11", "1"]);
    assert_eq!(
        solid(&render(&project, &root.path("cleared.ppm"))),
        compiled(warm).apply_rgb8(INPUT_2)
    );

    // Removing the layer drops its grade.
    succeed(&["grade-set", path, "layer", "11", "1", "--preset", "cool"]);
    succeed(&["scene-layer-remove", path, "11", "0"]);
    let listed = succeed(&["grade-list", path]);
    assert!(!listed.contains("scene 11 layer"), "{listed}");
}

/// SW-019: a Scene input's own grade colours the composed scene it shows on
/// Program, after its layers' input and layer grades and before the program
/// grade.
#[test]
fn scene_input_grades_colour_the_composed_scene_after_its_layers() {
    let root = Root::new("scene-input");
    let project = new_project(&root);
    let path = project.to_str().unwrap();
    succeed(&["scene-input-add", path, "3", "11", "Program"]);
    succeed(&["scene-layer-add", path, "11", "2", "0", "Layer"]);
    succeed(&["preview", path, "3"]);
    succeed(&["cut", path]);
    succeed(&["grade-set", path, "layer", "11", "1", "--exposure", "-1000"]);
    succeed(&["grade-set", path, "3", "--preset", "warm"]);
    let darker = Grade {
        exposure_millistops: -1000,
        ..Grade::IDENTITY
    };
    let warm = &builtin_preset("warm").unwrap().grade;
    let scene = compiled(warm).apply_rgb8(compiled(&darker).apply_rgb8(INPUT_2));
    assert_ne!(scene, compiled(&darker).apply_rgb8(INPUT_2));
    assert_eq!(solid(&render(&project, &root.path("scene.ppm"))), scene);
    // The program grade runs last.
    succeed(&["grade-set", path, "program", "--preset", "cool"]);
    let cool = &builtin_preset("cool").unwrap().grade;
    assert_eq!(
        solid(&render(&project, &root.path("program.ppm"))),
        compiled(cool).apply_rgb8(scene)
    );
}
