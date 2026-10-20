//! Sharing user animation presets and grades between projects through the
//! local CLI (GX-011, SW-019): `.fmpreset` and `.fmgrade` files exported
//! from one project and imported into another, user presets applied to
//! scene layers and title elements as `user:<id>`, and strict refusals.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::fm_animation::{Phase, Property, TrackKey, Value};
use fm_persistence::{ProjectStore, read_animation_preset_file};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const INVERT_CUBE: &str =
    "TITLE \"Invert\"\nLUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

/// A slide in from 400 px left with a crop that opens over 20 frames,
/// measured against a 400x100 box.
const SWOOSH: &str = r#"{"schema_version": 61, "kind": "animation_preset", "name": "Swoosh",
 "phase": "in", "timebase": "30/1", "frames": 20, "width": 400, "height": 100,
 "tracks": [
  {"property": "position_x", "keys": [{"frame": 0, "value": -400, "easing": "cubic-out"},
                                      {"frame": 20, "value": 0, "easing": "linear"}]},
  {"property": "crop_right", "keys": [{"frame": 0, "value": 400, "easing": "linear"},
                                      {"frame": 20, "value": 0, "easing": "linear"}]}]}
"#;

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemix-cli-presets-{}-{}-{name}",
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

/// The first `lines` lines of `output` (mutations also print the project
/// status after their own lines).
fn head(output: &str, lines: usize) -> String {
    output.split_inclusive('\n').take(lines).collect()
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

/// A new project with Scene input 3 showing scene 10, whose layer 1 (input
/// 1) covers the top-left quarter of the canvas (960x540).
fn scene_project(root: &Root, name: &str) -> PathBuf {
    let project = root.path(name);
    let path = project.to_str().unwrap();
    succeed(&["new", path]);
    succeed(&["scene-input-add", path, "3", "10", "Cam"]);
    succeed(&["scene-layer-add", path, "10", "1", "0", "L1"]);
    succeed(&[
        "scene-layer-geometry",
        path,
        "10",
        "0",
        "0",
        "0",
        "960",
        "540",
        "0",
    ]);
    project
}

fn load(project: &Path) -> fm_persistence::StoredProject {
    ProjectStore::new(project).unwrap().load().unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // one golden: import, apply, export, share, refuse
fn animation_presets_travel_between_projects_and_apply_to_scene_layers() {
    let root = Root::new("animation");
    let first = scene_project(&root, "first.freemix");
    let second = scene_project(&root, "second.freemix");
    let (first_path, second_path) = (first.to_str().unwrap(), second.to_str().unwrap());
    let shared = root.path("swoosh.fmpreset");
    fs::write(&shared, SWOOSH).unwrap();
    let file = shared.to_str().unwrap();

    // Imports join the library; a taken name gets a suffix.
    assert_eq!(
        head(&succeed(&["preset-import", first_path, file]), 1),
        "preset-import: preset 1 \"Swoosh\" phase=in frames=20\n"
    );
    assert_eq!(
        head(&succeed(&["preset-import", first_path, file]), 1),
        "preset-import: preset 2 \"Swoosh (2)\" phase=in frames=20\n"
    );
    assert_eq!(
        head(
            &succeed(&["preset-import", first_path, file, "--name", "Whoosh"]),
            1
        ),
        "preset-import: preset 3 \"Whoosh\" phase=in frames=20\n"
    );
    let library = load(&first).project().animation_presets().clone();
    assert_eq!(library.len(), 3);

    // Applied to the 960x540 layer over 10 frames: retimed, crop scaled
    // from the 400 px box to the layer, positions kept in pixels.
    assert_eq!(
        succeed(&[
            "scene-animation-preset",
            first_path,
            "10",
            "1",
            "--phase",
            "in",
            "--preset",
            "user:1",
            "--frames",
            "10",
        ]),
        "scene-animation-preset: scene 10 revision=1 in_frames=10 out_frames=0 tracks=2\n"
    );
    let stored = load(&first);
    let scene = stored
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id.get().get() == 10)
        .unwrap();
    let layer = scene.layers[0].id;
    let keys = |property| {
        scene
            .animation
            .clip
            .track(TrackKey::new(layer, Phase::In, property))
            .unwrap()
            .keys
            .iter()
            .map(|key| match key.value {
                Value::Scalar(value) => (key.frame, value),
                Value::Color(_) => panic!("a scalar track"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(Property::PositionX), [(0, -400), (10, 0)]);
    assert_eq!(keys(Property::CropRight), [(0, 960), (10, 0)]);

    // Exported and imported into another project, the preset is the same.
    let exported = root.path("exported.fmpreset");
    assert_eq!(
        succeed(&["preset-export", first_path, "3", exported.to_str().unwrap()]),
        format!(
            "preset-export: preset 3 \"Whoosh\" to {}\n",
            exported.display()
        )
    );
    let read = read_animation_preset_file(&exported).unwrap();
    assert_eq!(read.name, "Whoosh");
    assert_eq!(read.motion, library.presets()[2].motion);
    succeed(&["preset-import", second_path, exported.to_str().unwrap()]);
    let imported = load(&second).project().animation_presets().clone();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported.presets()[0].name, "Whoosh");
    assert_eq!(imported.presets()[0].motion, library.presets()[2].motion);

    // Strict refusals: unknown ids, another schema, a locked project.
    fail(
        &["preset-export", first_path, "9", exported.to_str().unwrap()],
        "animation preset 9 does not exist",
    );
    fail(
        &[
            "scene-animation-preset",
            first_path,
            "10",
            "1",
            "--phase",
            "in",
            "--preset",
            "user:9",
            "--frames",
            "10",
        ],
        "animation preset 9 does not exist",
    );
    for (schema, expected) in [("47", "schema"), ("59", "schema"), ("61", "schema")] {
        let old = root.path(&format!("schema-{schema}.fmpreset"));
        fs::write(
            &old,
            SWOOSH.replacen(
                "\"schema_version\": 61",
                &format!("\"schema_version\": {schema}"),
                1,
            ),
        )
        .unwrap();
        fail(
            &["preset-import", first_path, old.to_str().unwrap()],
            expected,
        );
    }
    let unknown = root.path("unknown.fmpreset");
    fs::write(
        &unknown,
        SWOOSH.replacen("\"frames\": 20,", "\"frames\": 20, \"extra\": 1,", 1),
    )
    .unwrap();
    fail(
        &["preset-import", first_path, unknown.to_str().unwrap()],
        "extra",
    );
    let manifest = second.join("project.json");
    let locked = fs::read_to_string(&manifest).unwrap().replacen(
        "\"edit_locks\": {\"project\": false",
        "\"edit_locks\": {\"project\": true",
        1,
    );
    fs::write(&manifest, locked).unwrap();
    fail(&["preset-import", second_path, file], "project is locked");
    assert_eq!(load(&second).project().animation_presets().len(), 1);
}

#[test]
fn grades_travel_between_projects_with_their_luts() {
    let root = Root::new("grade");
    let first = scene_project(&root, "first.freemix");
    let second = scene_project(&root, "second.freemix");
    let (first_path, second_path) = (first.to_str().unwrap(), second.to_str().unwrap());
    let cube = root.path("Invert.cube");
    fs::write(&cube, INVERT_CUBE).unwrap();
    succeed(&["grade-import-lut", first_path, cube.to_str().unwrap()]);
    succeed(&[
        "grade-set",
        first_path,
        "2",
        "--saturation",
        "1300",
        "--lut",
        "1",
        "--lut-strength",
        "600",
        "--save-as",
        "Inverted",
    ]);

    // The second project already has a LUT and a grade by those names.
    succeed(&["grade-import-lut", second_path, cube.to_str().unwrap()]);
    succeed(&["grade-set", second_path, "1", "--save-as", "Inverted"]);

    let shared = root.path("inverted.fmgrade");
    let file = shared.to_str().unwrap();
    assert_eq!(
        succeed(&["grade-export", first_path, "1", file]),
        format!("grade-export: grade 1 \"Inverted\" to {file} (LUT embedded)\n")
    );
    let text = fs::read_to_string(&shared).unwrap();
    assert!(text.contains("\"kind\": \"grade\""), "{text}");
    assert!(
        text.contains("LUT_3D_SIZE 2"),
        "the small LUT is embedded: {text}"
    );

    let imported = succeed(&["grade-import", second_path, file]);
    assert_eq!(
        head(&imported, 2),
        "grade-import: LUT 2 \"Invert (2)\" as asset://luts/2.cube\n\
         grade-import: library grade 2 \"Inverted (2)\"\n"
    );
    assert_eq!(
        fs::read_to_string(second.join("assets/luts/2.cube")).unwrap(),
        INVERT_CUBE
    );
    let stored = load(&second);
    let grade = &stored.project().grades()[1];
    assert_eq!(grade.name, "Inverted (2)");
    assert_eq!(grade.grade.saturation_permille, 1300);
    let lut = grade.grade.lut.as_ref().unwrap();
    assert_eq!(
        (lut.asset_uri.as_str(), lut.strength_permille),
        ("asset://luts/2.cube", 600)
    );
    let first_stored = load(&first);
    let source = &first_stored.project().grades()[0].grade;
    let mut without_lut = grade.grade.clone();
    without_lut.lut = None;
    let mut expected = source.clone();
    expected.lut = None;
    assert_eq!(without_lut, expected, "the values travel exactly");

    fail(
        &["grade-export", first_path, "7", file],
        "library grade 7 does not exist",
    );
    let other = root.path("other.fmgrade");
    fs::write(
        &other,
        text.replacen("\"kind\": \"grade\"", "\"kind\": \"animation_preset\"", 1),
    )
    .unwrap();
    fail(
        &["grade-import", second_path, other.to_str().unwrap()],
        "file is of kind `animation_preset`, not `grade`",
    );
}

/// A taken name at the byte bound still imports: the suffix replaces the
/// end of the name (on a character boundary) instead of overflowing it.
#[test]
fn imports_rename_long_taken_names_within_the_bound() {
    let root = Root::new("long-names");
    let project = root.path("show.freemix");
    let path = project.to_str().unwrap();
    succeed(&["new", path]);
    let long = "é".repeat(64); // 128 bytes, the grade name bound
    succeed(&["grade-set", path, "1", "--save-as", &long]);
    let shared = root.path("long.fmgrade");
    let file = shared.to_str().unwrap();
    succeed(&["grade-export", path, "1", file]);
    succeed(&["grade-import", path, file]);
    let stored = load(&project);
    let names = stored
        .project()
        .grades()
        .iter()
        .map(|grade| grade.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[1], format!("{} (2)", "é".repeat(62)));
}

/// Importing an already-suffixed name counts on from its number instead of
/// stacking suffixes (`Look (2)` becomes `Look (3)`, never `Look (2) (2)`).
#[test]
fn imports_count_on_from_an_existing_suffix() {
    let root = Root::new("suffixes");
    let project = root.path("show.freemix");
    let path = project.to_str().unwrap();
    succeed(&["new", path]);
    succeed(&["grade-set", path, "1", "--save-as", "Look (2)"]);
    let shared = root.path("look.fmgrade");
    let file = shared.to_str().unwrap();
    succeed(&["grade-export", path, "1", file]);
    succeed(&["grade-import", path, file]);
    succeed(&["grade-import", path, file]);
    let names = load(&project)
        .project()
        .grades()
        .iter()
        .map(|grade| grade.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Look (2)", "Look (3)", "Look (4)"]);
}

/// A large LUT goes beside the `.fmgrade`; an existing `.cube` there is
/// only replaced with `--replace-lut`.
#[test]
fn grade_exports_ask_before_replacing_a_sibling_lut() {
    let root = Root::new("sibling");
    let project = root.path("show.freemix");
    let path = project.to_str().unwrap();
    succeed(&["new", path]);
    let mut large = INVERT_CUBE.to_owned();
    while large.len() <= 32 * 1024 {
        large.insert_str(0, "# padding to grow the LUT past the embed limit\n");
    }
    let cube = root.path("source.cube");
    fs::write(&cube, &large).unwrap();
    succeed(&["grade-import-lut", path, cube.to_str().unwrap()]);
    succeed(&["grade-set", path, "1", "--lut", "1", "--save-as", "Big"]);

    let shared = root.path("big.fmgrade");
    let file = shared.to_str().unwrap();
    let sibling = root.path("big.cube");
    fs::write(&sibling, "unrelated").unwrap();
    fail(
        &["grade-export", path, "1", file],
        &format!(
            "LUT file {} already exists; pass --replace-lut to overwrite it",
            sibling.display()
        ),
    );
    assert!(!shared.exists());
    assert_eq!(fs::read_to_string(&sibling).unwrap(), "unrelated");

    assert_eq!(
        succeed(&["grade-export", path, "1", file, "--replace-lut"]),
        format!(
            "grade-export: grade 1 \"Big\" to {file} (LUT written to {})\n",
            sibling.display()
        )
    );
    assert_eq!(fs::read_to_string(&sibling).unwrap(), large);
}
