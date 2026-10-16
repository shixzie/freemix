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
