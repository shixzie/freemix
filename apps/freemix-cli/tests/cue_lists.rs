//! RC-017: operator cue lists through the local CLI. Lists are authored,
//! listed, and removed in the saved project; `go` fires the cue on standby,
//! runs its waits on the simulated cue clock, and follows auto-follow cues;
//! the run state persists so successive GOs walk the list; and refused edits
//! leave the project unchanged.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemix-cli-cues-{}-{}-{name}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn project(&self) -> String {
        let project = self.0.join("show.freemix");
        let project = project.to_str().unwrap().to_owned();
        succeed(&["new", &project]);
        project
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

const CUES: &str = "10:Open:0:take.2.fade.5,wait.3,overlay_on.1.1;\
                    20:Close:1:take.1.cut.0;30:End:0:fade_to_black.1.4";

#[test]
fn cue_lists_are_authored_listed_and_removed_in_the_saved_project() {
    let root = Root::new("author");
    let project = root.project();
    assert_eq!(succeed(&["cue-lists", &project]), "CueLists=none\n");
    succeed(&["cue-list-set", &project, "7", "Show", CUES]);
    assert_eq!(
        succeed(&["cue-lists", &project]),
        "CueList(id=7, name=\"Show\", standby=10, live=-, pending=-, played=[], fault=-)\n\
         \x20 Cue(id=10, name=\"Open\", follow=false, actions=take.2.fade.5,wait.3,overlay_on.1.1)\n\
         \x20 Cue(id=20, name=\"Close\", follow=true, actions=take.1.cut.0)\n\
         \x20 Cue(id=30, name=\"End\", follow=false, actions=fade_to_black.1.4)\n"
    );
    succeed(&["cue-list-remove", &project, "7"]);
    assert_eq!(succeed(&["cue-lists", &project]), "CueLists=none\n");
}

#[test]
fn go_runs_waits_and_auto_follow_and_successive_gos_walk_the_list() {
    let root = Root::new("go");
    let project = root.project();
    succeed(&["cue-list-set", &project, "7", "Show", CUES]);

    // Cue 10: the fade takes input 2, the wait holds three frames, then the
    // overlay comes on; the clock runs the cue to its end.
    let status = succeed(&["go", &project, "7"]);
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    assert!(status.contains("Overlays(desired=[1:1:on"), "{status}");
    let lists = succeed(&["cue-lists", &project]);
    assert!(
        lists.starts_with(
            "CueList(id=7, name=\"Show\", standby=20, live=10, pending=-, played=[10], fault=-)"
        ),
        "{lists}"
    );

    // Cue 20 cuts back to input 1 and auto-follows into cue 30's FTB.
    let status = succeed(&["go", &project, "7"]);
    assert!(
        status.contains("Program(desired=1, realized=1)"),
        "{status}"
    );
    assert!(status.contains("FTB(desired=black@65535/65535"), "{status}");
    let lists = succeed(&["cue-lists", &project]);
    assert!(
        lists.starts_with(
            "CueList(id=7, name=\"Show\", standby=-, live=30, pending=-, played=[10, 20, 30], fault=-)"
        ),
        "{lists}"
    );

    // The list is played out: GO has nothing on standby until a reset.
    fail(&["go", &project, "7"], "no_cue_on_standby");
    succeed(&["cue-reset", &project, "7"]);
    succeed(&["cue-skip", &project, "7"]);
    succeed(&["cue-standby", &project, "7", "30"]);
    succeed(&["cue-back", &project, "7"]);
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains("standby=20"), "{lists}");
    succeed(&["cue-stop", &project, "7"]);
}

#[test]
fn refused_cue_edits_leave_the_project_unchanged() {
    let root = Root::new("refused");
    let project = root.project();
    succeed(&["cue-list-set", &project, "7", "Show", CUES]);
    let before = succeed(&["cue-lists", &project]);

    fail(
        &[
            "cue-list-set",
            &project,
            "7",
            "Show",
            "10:Open:0:take.99.fade.5",
        ],
        "names input 99",
    );
    fail(
        &["cue-list-set", &project, "7", "Show", "10:Open:0:bogus"],
        "invalid cue action value `bogus`",
    );
    fail(
        &["cue-list-set", &project, "7", "Show", "10:Open:maybe:"],
        "invalid cue value",
    );
    fail(&["cue-list-remove", &project, "8"], "cue list 8");
    fail(&["go", &project, "8"], "cue list 8");
    // An input a cue names cannot be removed.
    fail(&["input-remove", &project, "2"], "cannot remove input 2");
    assert_eq!(succeed(&["cue-lists", &project]), before);
}

/// A stream destination a cue starts cannot be removed: the refusal names
/// the cue and the project is unchanged.
#[test]
fn a_stream_destination_a_cue_names_cannot_be_removed() {
    let root = Root::new("stream");
    let project = root.project();
    succeed(&["scene-input-add", &project, "10", "10", "Program"]);
    succeed(&["audio-bus-add", &project, "20", "Master"]);
    succeed(&["output-add", &project, "30", "10", "20", "Primary"]);
    succeed(&[
        "stream-add",
        &project,
        "40",
        "30",
        "rtmps://ingest.example.test/live",
        "live-7c2a-secret-key",
        "Main",
    ]);
    succeed(&[
        "cue-list-set",
        &project,
        "7",
        "Show",
        "10:Live:0:stream.40.1",
    ]);
    let before = fs::read(PathBuf::from(&project).join("project.json")).unwrap();
    fail(
        &["stream-remove", &project, "40"],
        "stream destination 40 is in use by cue 10 of cue list 7",
    );
    assert_eq!(
        fs::read(PathBuf::from(&project).join("project.json")).unwrap(),
        before
    );
    // Once no cue names it, it can go.
    succeed(&["cue-list-remove", &project, "7"]);
    succeed(&["stream-remove", &project, "40"]);
}

/// An auto-follow that would take Program twice on one frame is refused
/// at authoring, naming both cues.
#[test]
fn a_waitless_follow_into_a_second_take_is_refused() {
    let root = Root::new("follow-takes");
    let project = root.project();
    fail(
        &[
            "cue-list-set",
            &project,
            "7",
            "Show",
            "10:Open:1:take.2.cut.0;20:Close:0:take.1.cut.0",
        ],
        "cue 10 auto-follows into cue 20 and both take Program on one frame",
    );
    succeed(&[
        "cue-list-set",
        &project,
        "7",
        "Show",
        "10:Open:1:take.2.cut.0;20:Close:0:wait.1,take.1.cut.0",
    ]);
}

/// A `.fmtitle` source document that needs no font (see `e2e.rs`).
const SHAPE_TITLE_SOURCE: &str = r##"{
  "schema_version": 54,
  "template": {
    "id": 7,
    "name": "Score bar",
    "width": 1920,
    "height": 1080,
    "background": "#00000000",
    "fields": [
      {"id": 2, "name": "Bar color", "default": {"type": "color", "value": "#112233FF"}},
      {"id": 1, "name": "Score", "default": {"type": "number", "value": -4}}
    ],
    "elements": [
      {"id": 1, "name": "Bar", "bounds": {"x": 0, "y": 900, "width": 1920, "height": 120}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": null, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": 2, "kind": {"type": "rectangle"}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": []
}
"##;

/// Title actions name title inputs, which the local engine knows: a list
/// with title cues is authored and fired, and the project still loads.
#[test]
fn title_cues_are_authored_and_fired_locally() {
    let root = Root::new("titles");
    let project = root.project();
    let source = root.0.join("score-bar.fmtitle");
    fs::write(&source, SHAPE_TITLE_SOURCE).unwrap();
    succeed(&[
        "title-input-add",
        &project,
        "--id",
        "9",
        "--name",
        "Score bar",
        "--template",
        source.to_str().unwrap(),
    ]);
    succeed(&[
        "cue-list-set",
        &project,
        "1",
        "Titles",
        "1:On:0:title_on.9.2;2:Off:0:title_off.9",
    ]);
    let status = succeed(&["go", &project, "1"]);
    assert!(status.contains(",2:9:on"), "{status}");
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains("standby=2, live=1"), "{lists}");
    succeed(&["go", &project, "1"]);
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains("standby=-, live=2"), "{lists}");
}
