//! RC-004: shortcut sets through the local CLI. A `.fmshortcuts` file is
//! imported into a project (new names added, known names redefined, an
//! identical set left alone), listed, exported, and carried into another
//! project; an import that would clash on a key or OSC name, or one into a
//! locked project, imports nothing.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_preset_file::SCHEMA_VERSION;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemix-cli-shortcut-sets-{}-{}-{name}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn project(&self, name: &str) -> String {
        let project = self.0.join(name);
        let project = project.to_str().unwrap().to_owned();
        succeed(&["new", &project]);
        project
    }

    fn file(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
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

fn set(shortcuts: &str) -> String {
    format!(
        "{{\"schema_version\": {SCHEMA_VERSION}, \"kind\": \"shortcut_set\", \
         \"name\": \"Desk\", \"shortcuts\": [{shortcuts}]}}\n"
    )
}

const INTRO: &str = "{\"name\": \"Intro\", \"enabled\": true, \
     \"key\": {\"key\": \"F9\", \"command\": false, \"shift\": true, \"alt\": false, \
     \"then\": null, \"scope\": null}, \
     \"osc\": \"intro\", \"action\": \"actions:overlay_on.1.1/take.2.cut.0\"}";
/// Ctrl+K then 1, in the Cues panel.
const OPEN: &str = "{\"name\": \"Open\", \"enabled\": false, \"key\": {\"key\": \"K\", \
     \"command\": true, \"shift\": false, \"alt\": false, \"then\": {\"key\": \"1\", \
     \"command\": false, \"shift\": false, \"alt\": false}, \"scope\": \"cues\"}, \
     \"osc\": null, \"action\": \"cue:1:go:-\"}";

/// The shortcut lines of `shortcuts` output.
fn listed(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.starts_with("Shortcut"))
        .collect()
}

#[test]
fn a_shortcut_set_is_imported_listed_exported_and_carried_to_another_project() {
    let root = Root::new("round-trip");
    let first = root.project("first.freemix");
    assert_eq!(succeed(&["shortcuts", &first]), "Shortcuts=none\n");
    fail(
        &["shortcut-export", &first, &root.path("none.fmshortcuts")],
        "the project has no shortcuts",
    );

    let desk = root.file("desk.fmshortcuts", &set(&format!("{INTRO}, {OPEN}")));
    let output = succeed(&["shortcut-import", &first, &desk]);
    assert!(
        output.starts_with("shortcut-import: added=2 replaced=0 unchanged=0\n"),
        "{output}"
    );
    let expected = [
        "Shortcut(id=1, name=\"Intro\", enabled=true, key=Shift+F9, \
         osc=/freemix/user/intro, action=\"actions:overlay_on.1.1/take.2.cut.0\")",
        "Shortcut(id=2, name=\"Open\", enabled=false, key=Ctrl+K,1@cues, osc=-, \
         action=\"cue:1:go:-\")",
    ];
    assert_eq!(listed(&succeed(&["shortcuts", &first])), expected);

    // The same set again changes nothing; a redefined one keeps its id.
    let output = succeed(&["shortcut-import", &first, &desk]);
    assert!(
        output.starts_with("shortcut-import: added=0 replaced=0 unchanged=2\n"),
        "{output}"
    );
    let moved = root.file(
        "moved.fmshortcuts",
        &set(&INTRO.replace("\"F9\"", "\"F10\"")),
    );
    let output = succeed(&["shortcut-import", &first, &moved]);
    assert!(
        output.starts_with("shortcut-import: added=0 replaced=1 unchanged=0\n"),
        "{output}"
    );
    assert!(
        listed(&succeed(&["shortcuts", &first]))[0]
            .contains("id=1, name=\"Intro\", enabled=true, key=Shift+F10,"),
    );

    // Exported and imported into a fresh project, the set arrives whole.
    let exported = root.path("exported.fmshortcuts");
    assert_eq!(
        succeed(&["shortcut-export", &first, &exported, "--name", "Studio"]),
        format!("shortcut-export: 2 shortcuts as \"Studio\" to {exported}\n")
    );
    let text = fs::read_to_string(&exported).unwrap();
    assert!(text.contains("\"name\": \"Studio\""), "{text}");
    let second = root.project("second.freemix");
    succeed(&["shortcut-import", &second, &exported]);
    assert_eq!(
        listed(&succeed(&["shortcuts", &second])),
        listed(&succeed(&["shortcuts", &first]))
    );
}

/// Keys scoped to Studio's tool windows (the Colour window, the Animation
/// editor, the scene Designer; schema 82) import, list with their scope,
/// and export unchanged; one tool window's key does not clash with
/// another's.
#[test]
fn tool_window_scoped_keys_import_list_and_export() {
    let root = Root::new("tool-windows");
    let project = root.project("tools.freemix");
    let tool = |name: &str, scope: &str| {
        format!(
            "{{\"name\": \"{name}\", \"enabled\": true, \"key\": {{\"key\": \"F9\", \
             \"command\": false, \"shift\": false, \"alt\": false, \"then\": null, \
             \"scope\": \"{scope}\"}}, \"osc\": null, \"action\": \"cue:1:go:-\"}}"
        )
    };
    let tools = root.file(
        "tools.fmshortcuts",
        &set(&format!(
            "{}, {}, {}",
            tool("Grade", "color-window"),
            tool("Keys", "animation-window"),
            tool("Layers", "designer-window")
        )),
    );
    let output = succeed(&["shortcut-import", &project, &tools]);
    assert!(
        output.starts_with("shortcut-import: added=3 replaced=0 unchanged=0\n"),
        "{output}"
    );
    let listing = succeed(&["shortcuts", &project]);
    let lines = listed(&listing);
    for (line, scope) in lines
        .iter()
        .zip(["color-window", "animation-window", "designer-window"])
    {
        assert!(line.contains(&format!(" key=F9@{scope}, ")), "{line}");
    }
    let exported = root.path("exported.fmshortcuts");
    succeed(&["shortcut-export", &project, &exported]);
    let text = fs::read_to_string(&exported).unwrap();
    assert!(text.contains("\"scope\": \"designer-window\""), "{text}");
    // A tool's name alone is no scope.
    let bare = root.file("bare.fmshortcuts", &set(&tool("Grade", "color")));
    let output = invoke(&["shortcut-import", &project, &bare]);
    assert!(!output.status.success());
}

#[test]
fn a_clashing_set_or_a_locked_project_imports_nothing() {
    let root = Root::new("refused");
    let project = root.project("show.freemix");
    let desk = root.file("desk.fmshortcuts", &set(INTRO));
    succeed(&["shortcut-import", &project, &desk]);
    let before = succeed(&["shortcuts", &project]);

    // Another name on Intro's key.
    let clash = root.file(
        "clash.fmshortcuts",
        &set(&format!(
            "{}, {OPEN}",
            INTRO
                .replace("\"Intro\"", "\"Break\"")
                .replace("\"intro\"", "\"break\"")
        )),
    );
    fail(
        &["shortcut-import", &project, &clash],
        "imported shortcut \"Break\" uses the key of shortcut \"Intro\"",
    );
    // A file that does not decode.
    let broken = root.file("broken.fmshortcuts", &set("{\"name\": \"X\"}"));
    fail(&["shortcut-import", &project, &broken], "missing field");
    assert_eq!(succeed(&["shortcuts", &project]), before);

    // The project padlock refuses an import.
    let manifest = PathBuf::from(&project).join("project.json");
    let unlocked = fs::read_to_string(&manifest).unwrap();
    let locked = unlocked.replacen(
        "\"edit_locks\": {\"project\": false",
        "\"edit_locks\": {\"project\": true",
        1,
    );
    assert_ne!(locked, unlocked);
    fs::write(&manifest, &locked).unwrap();
    let open = root.file("open.fmshortcuts", &set(OPEN));
    fail(&["shortcut-import", &project, &open], "project is locked");
    assert_eq!(succeed(&["shortcuts", &project]), before);
}

/// RC-004: `shortcut-fire` fires a project shortcut locally. A shortcut
/// acting on the selected input is refused without `--selected`, and acts on
/// the input it names with it.
#[test]
fn a_shortcut_fires_locally_on_the_selected_input() {
    let root = Root::new("fire");
    let project = root.project("show.freemix");
    let take = "{\"name\": \"Take selected\", \"enabled\": true, \"key\": null, \
         \"osc\": null, \"action\": \"actions:take.selected.cut.0\"}";
    let desk = root.file("desk.fmshortcuts", &set(take));
    succeed(&["shortcut-import", &project, &desk]);
    fail(&["shortcut-fire", &project, "1"], "selected");
    let status = succeed(&["shortcut-fire", &project, "1", "--selected", "2"]);
    assert!(
        status.contains("Program(desired=2, realized=2)"),
        "{status}"
    );
    fail(
        &["shortcut-fire", &project, "9", "--selected", "2"],
        "shortcut",
    );
}
