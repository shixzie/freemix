//! RC-011: project scripts through the local CLI. Scripts are authored from
//! a file, listed with their syntax check, run by id or by name, and
//! removed in the saved project; a run applies every action on one frame;
//! a run that stops early (a syntax error, a runtime error, an exhausted
//! budget, or a refused action) names its line and column and leaves the
//! project unchanged.

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
            "freemix-cli-scripts-{}-{}-{name}",
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

    fn source(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_owned()
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

const OPENING: &str = "# Take the next camera and key the first one over it.\n\
                       let next = program + 1\n\
                       preview next\n\
                       transition fade, 4\n\
                       overlay 1, 1\n";

#[test]
fn scripts_are_authored_listed_run_and_removed_in_the_saved_project() {
    let root = Root::new("author");
    let project = root.project();
    assert_eq!(succeed(&["scripts", &project]), "Scripts=none\n");
    let source = root.source("opening.fms", OPENING);
    succeed(&["script-set", &project, "3", "Opening", &source]);
    let listed = succeed(&["scripts", &project]);
    assert!(
        listed.starts_with(&format!(
            "Script(id=3, name=\"Opening\", bytes={}, syntax=\"ok\")\n  | # Take the next camera",
            OPENING.len()
        )),
        "{listed}"
    );

    // By name: the fade takes input 2 and the overlay keys input 1; the
    // transition plays to its end before the project is saved.
    let status = succeed(&["script-run", &project, "Opening"]);
    assert!(status.contains("ScriptRan(script=3, frame="), "{status}");
    assert!(status.contains(", actions=3, instructions="), "{status}");
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    assert!(status.contains("Overlays(desired=[1:1:on"), "{status}");

    // By id: the saved Program carries into the next run, which asks for
    // an input the two-input project lacks, so the run is refused where it
    // stopped and nothing it did is kept.
    let before = succeed(&["status", &project]);
    fail(
        &["script-run", &project, "3"],
        "(reason: script_failed:refused:3:1)",
    );
    assert_eq!(succeed(&["status", &project]), before);

    succeed(&["script-remove", &project, "3"]);
    assert_eq!(succeed(&["scripts", &project]), "Scripts=none\n");
    fail(&["script-run", &project, "Opening"], "no script is named");
}

#[test]
fn a_run_that_stops_early_names_where_and_changes_nothing() {
    let root = Root::new("refused");
    let project = root.project();
    for (script, text, expected) in [
        (
            "1",
            "preview 2\nif program = 1\nend\n",
            "invalid_command: script 1 stopped at line 2:12: syntax error: use == to compare",
        ),
        (
            "2",
            "cut 2\nlet x = y\n",
            "(reason: script_failed:runtime:2:9)",
        ),
        (
            "3",
            "while true\nend\n",
            "resource_exhausted: script 3 stopped at line 1:1",
        ),
        (
            "4",
            "repeat 100\n  mute 1\nend\n",
            "(reason: script_failed:actions:2:3)",
        ),
        (
            "5",
            "cut 2\n  cut 99\n",
            "(reason: script_failed:refused:2:3)",
        ),
    ] {
        let source = root.source(&format!("{script}.fms"), text);
        succeed(&[
            "script-set",
            &project,
            script,
            &format!("S{script}"),
            &source,
        ]);
        let before = succeed(&["status", &project]);
        fail(&["script-run", &project, script], expected);
        assert_eq!(succeed(&["status", &project]), before, "{text}");
    }
    // A syntax error is listed before any run.
    let listed = succeed(&["scripts", &project]);
    assert!(
        listed.contains("syntax=\"line 2:12: syntax error: use == to compare"),
        "{listed}"
    );
    fail(
        &["script-run", &project, "9"],
        "(reason: unknown_id:script:9)",
    );
}

#[test]
fn script_edits_respect_their_bounds() {
    let root = Root::new("bounds");
    let project = root.project();
    let long = root.source("long.fms", &"#".repeat(4_097));
    fail(&["script-set", &project, "1", "Long", &long], "source");
    let short = root.source("short.fms", "cut 2\n");
    fail(&["script-set", &project, "0", "Zero", &short], "script");
    succeed(&["script-set", &project, "1", "Short", &short]);
}

/// RC-011/RC-017 (Protocol 2.60): a cue runs a project script as one step
/// after its wait and prints what it did (Protocol 2.61); a script a cue
/// runs cannot be removed, and a cue whose script fails is refused with the
/// script's typed reason.
#[test]
fn a_cue_runs_a_script_and_holds_it() {
    let root = Root::new("cue");
    let project = root.project();
    let source = root.source("opening.fms", OPENING);
    succeed(&["script-set", &project, "3", "Opening", &source]);
    fail(
        &[
            "cue-list-set",
            &project,
            "7",
            "Show",
            "10:Open:0:run_script.4",
        ],
        "(reason: unknown_id:script:4)",
    );
    succeed(&[
        "cue-list-set",
        &project,
        "7",
        "Show",
        "10:Open:0:wait.2,run_script.3;20:Again:0:run_script.3",
    ]);
    let listed = succeed(&["cue-lists", &project]);
    assert!(listed.contains("actions=wait.2,run_script.3)"), "{listed}");

    // The wait holds two frames, then the script takes input 2 and keys
    // input 1, exactly as running it directly does; the GO prints what the
    // step did (Protocol 2.61), the counts a direct run on the same show
    // prints.
    let direct = root.0.join("direct.freemix");
    let direct = direct.to_str().unwrap();
    succeed(&["new", direct]);
    succeed(&["script-set", direct, "3", "Opening", &source]);
    let ran = succeed(&["script-run", direct, "3"]);
    let counts = |output: &str| {
        let line = output
            .lines()
            .find(|line| line.starts_with("ScriptRan(script=3, frame="))
            .unwrap_or_else(|| panic!("no ScriptRan line: {output}"));
        line[line.find(", actions=").unwrap()..].to_owned()
    };
    let status = succeed(&["go", &project, "7"]);
    assert_eq!(counts(&status), counts(&ran), "{status}");
    assert_eq!(counts(&ran), ", actions=3, instructions=11)", "{ran}");
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    assert!(status.contains("Overlays(desired=[1:1:on"), "{status}");
    fail(
        &["script-remove", &project, "3"],
        "script 3 is run by cue list 7",
    );

    // On Program 2 the script asks for input 3, which the project lacks:
    // the GO is refused where the script stopped and nothing changes.
    let before = succeed(&["status", &project]);
    fail(
        &["go", &project, "7"],
        "(reason: script_failed:refused:3:1)",
    );
    assert_eq!(succeed(&["status", &project]), before);
}
