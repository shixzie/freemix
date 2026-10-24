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
        "CueList(id=7, name=\"Show\", standby=10, live=-, pending=-, played=[], fault=-, armed=-)\n\
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
            "CueList(id=7, name=\"Show\", standby=20, live=10, pending=-, played=[10], fault=-, armed=-)"
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
            "CueList(id=7, name=\"Show\", standby=-, live=30, pending=-, played=[10, 20, 30], fault=-, armed=-)"
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

/// Protocol 2.64 locally: `cue-arm` arms a GO that the simulated cue clock
/// fires on its frame (a local command runs the clock to its end, as for a
/// GO's waits), and `cue-disarm` takes an armed GO back.
#[test]
fn an_armed_go_fires_on_the_local_clock() {
    let root = Root::new("arm");
    let project = root.project();
    succeed(&["cue-list-set", &project, "7", "Show", CUES]);
    fail(&["cue-arm", &project, "7", "0"], "milliseconds");
    fail(&["cue-arm", &project, "7", "200", "99"], "cue 99");
    let status = succeed(&["cue-arm", &project, "7", "200", "30"]);
    assert!(status.contains("FTB(desired=black@65535/65535"), "{status}");
    let lists = succeed(&["cue-lists", &project]);
    assert!(
        lists.starts_with(
            "CueList(id=7, name=\"Show\", standby=-, live=30, pending=-, played=[30], fault=-, armed=-)"
        ),
        "{lists}"
    );
    succeed(&["cue-disarm", &project, "7"]);
    // A GO armed past the longest cue wait (two hours at 59.94 fps) still
    // settles on the local clock instead of being refused as runaway.
    succeed(&["cue-arm", &project, "7", "7200000", "20"]);
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains("played=[20, 30]"), "{lists}");
    assert!(lists.contains("armed=-)"), "{lists}");
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
  "schema_version": 82,
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

/// Protocol 2.65, schema 59: a cue's data update steps and sets a title's
/// number field, saved in the project; a color field is refused when the
/// list is set, leaving the project unchanged.
#[test]
fn title_number_cues_step_and_set_a_score_locally() {
    let root = Root::new("score");
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
    let manifest = || fs::read_to_string(format!("{project}/project.json")).unwrap();
    fail(
        &[
            "cue-list-set",
            &project,
            "1",
            "Score",
            "1:Goal:0:title_number.9.2.add.1",
        ],
        "not a number or text field",
    );
    assert!(!manifest().contains("title_number"));
    succeed(&[
        "cue-list-set",
        &project,
        "1",
        "Score",
        "1:Goal:0:title_number.9.1.add.1,title_number.9.1.add.2;2:Reset:0:title_number.9.1.set.0",
    ]);
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains("title_number.9.1.add.2"), "{lists}");
    succeed(&["go", &project, "1"]);
    // -4 + 1 + 2.
    assert!(
        manifest().contains("{\"id\": 1, \"type\": \"number\", \"value\": -1, \"revision\": 2}"),
        "{}",
        manifest()
    );
    succeed(&["go", &project, "1"]);
    assert!(
        manifest().contains("{\"id\": 1, \"type\": \"number\", \"value\": 0, \"revision\": 3}"),
        "{}",
        manifest()
    );
}

/// A saved waiting cue (as the daemon saves one) is due within the wait
/// before its next action of the frame cursor: such a project restores and
/// a local cue command plays the wait out. A hand-edited one due further
/// ahead is refused on load instead of holding the cue clock (and the
/// command) for ever.
#[test]
fn a_saved_waiting_cue_restores_and_one_out_of_reach_is_refused() {
    let root = Root::new("reach");
    let project = root.project();
    succeed(&[
        "cue-list-set",
        &project,
        "8",
        "Hold",
        "10:Hold:0:wait.5,take.2.cut.0",
    ]);
    let manifest = PathBuf::from(&project).join("project.json");
    let saved = fs::read_to_string(&manifest).unwrap();
    let prefix = "\"frames_rendered\": ";
    let start = saved.find(prefix).unwrap() + prefix.len();
    let cursor: u64 = saved[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap();
    let idle = "\"state\": {\"standby\": 10, \"live\": null, \"pending\": null, \
                \"played\": [], \"fault\": null, \"armed\": null, \"targets\": null}";
    assert!(saved.contains(idle), "{saved}");
    let waiting = |due: u64| {
        saved.replace(
            idle,
            &format!(
                "\"state\": {{\"standby\": null, \"live\": 10, \"pending\": \
                 {{\"cue\": 10, \"action\": 1, \"due_frame\": {due}}}, \"played\": [10], \
                 \"fault\": null, \"armed\": null, \"targets\": null}}"
            ),
        )
    };

    // One frame past the wait: refused, and the project is left alone.
    let edited = waiting(cursor + 6);
    fs::write(&manifest, &edited).unwrap();
    fail(&["cue-lists", &project], "further past cursor");
    fail(&["cue-standby", &project, "8", "10"], "further past cursor");
    assert_eq!(fs::read_to_string(&manifest).unwrap(), edited);

    // Within the wait: restored, and a local cue command plays it out.
    fs::write(&manifest, waiting(cursor + 5)).unwrap();
    let lists = succeed(&["cue-lists", &project]);
    assert!(
        lists.contains(&format!("pending=10.1@{}", cursor + 5)),
        "{lists}"
    );
    let status = succeed(&["cue-standby", &project, "8", "10"]);
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    let lists = succeed(&["cue-lists", &project]);
    assert!(
        lists.contains("standby=10, live=10, pending=-, played=[10], fault=-"),
        "{lists}"
    );
}

/// RC-004: `go --selected <input>` binds the selected input for the run;
/// without it a cue acting on the selected input is refused, and the list
/// reports the inputs its run bound (Protocol 2.89).
#[test]
fn go_binds_the_selected_input_and_the_list_reports_it() {
    let root = Root::new("selected");
    let project = root.project();
    succeed(&[
        "cue-list-set",
        &project,
        "7",
        "Show",
        "10:Take:0:take.selected.cut.0",
    ]);
    fail(&["go", &project, "7"], "selected");
    let lists = succeed(&["cue-lists", &project]);
    assert!(!lists.contains("bound="), "{lists}");
    let status = succeed(&["go", &project, "7", "--selected", "2"]);
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    let lists = succeed(&["cue-lists", &project]);
    assert!(lists.contains(",selected:2)\n"), "{lists}");
    assert!(lists.contains(", bound=preview:"), "{lists}");
}
