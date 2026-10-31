//! End-to-end coverage of the local `.freemix` bundle commands.

use std::{
    fs,
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_model::InputKind;
use fm_persistence::{CURRENT_SCHEMA_VERSION, ProjectStore, StoredProject};
use fm_types::InputId;

static TEST_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = TEST_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "freemix-cli-bundles-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn assert_failure_contains(output: &Output, expected: &str) {
    assert!(
        !output.status.success(),
        "process unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr did not contain `{expected}`: {stderr}"
    );
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn new_project(path: &Path, name: &str) {
    assert_success(&run(&["new", text(path), "--name", name]));
}

fn load(path: &Path) -> StoredProject {
    ProjectStore::new(path).unwrap().load().unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn media_uri(project: &StoredProject, id: u128) -> String {
    let input = project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input(id))
        .unwrap_or_else(|| panic!("input {id} is missing"));
    match &input.kind {
        InputKind::Media { asset_uri, .. } => asset_uri.clone(),
        other => panic!("input {id} is not media: {other:?}"),
    }
}

fn import(project: &Path, file: &Path, input: &str) {
    assert_success(&run(&[
        "project-import-media",
        text(project),
        text(file),
        "--input",
        input,
    ]));
}

#[test]
fn import_media_copies_into_assets_and_optionally_adds_an_input() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Show");
    let clip = root.path("Opening Clip.ppm");
    fs::write(&clip, b"clip-bytes").unwrap();

    let bare = assert_success(&run(&["project-import-media", text(&show), text(&clip)]));
    assert!(
        bare.contains("copied")
            && bare.contains("asset://Opening Clip.ppm")
            && bare.contains("bytes=10"),
        "{bare}"
    );
    assert_eq!(
        fs::read(show.join("assets/Opening Clip.ppm")).unwrap(),
        b"clip-bytes"
    );
    assert_eq!(load(&show).project().inputs().len(), 2);

    let again = assert_success(&run(&[
        "project-import-media",
        text(&show),
        text(&clip),
        "--input",
        "9",
        "--name",
        "Opener",
    ]));
    assert!(
        again.contains("reused identical asset://Opening Clip.ppm"),
        "{again}"
    );
    let stored = load(&show);
    assert_eq!(media_uri(&stored, 9), "asset://Opening Clip.ppm");
    assert_eq!(
        stored
            .project()
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input(9))
            .unwrap()
            .name,
        "Opener"
    );

    let other = root.path("other").join("Opening Clip.ppm");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, b"different").unwrap();
    import(&show, &other, "10");
    assert_eq!(media_uri(&load(&show), 10), "asset://Opening Clip-2.ppm");
    assert_eq!(
        fs::read(show.join("assets/Opening Clip.ppm")).unwrap(),
        b"clip-bytes"
    );

    let manifest = fs::read(show.join("project.json")).unwrap();
    assert_failure_contains(
        &run(&[
            "project-import-media",
            text(&show),
            text(&clip),
            "--input",
            "9",
        ]),
        "input 9 already exists",
    );
    assert_failure_contains(
        &run(&[
            "project-import-media",
            text(&show),
            text(&root.path("absent.ppm")),
        ]),
        "media import failed",
    );
    assert_failure_contains(
        &run(&["project-import-media", text(&show), text(&root.0)]),
        "is not a regular file",
    );
    assert_failure_contains(
        &run(&[
            "project-import-media",
            text(&show),
            text(&clip),
            "--name",
            "x",
        ]),
        "--name requires --input",
    );
    assert_eq!(fs::read(show.join("project.json")).unwrap(), manifest);

    let not_a_bundle = root.path("missing.freemix");
    assert!(
        !run(&["project-import-media", text(&not_a_bundle), text(&clip)])
            .status
            .success()
    );
    assert!(!not_a_bundle.exists());
}

#[test]
fn relink_previews_by_default_and_applies_only_with_flag() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Show");
    let clip = root.path("clip.ppm");
    fs::write(&clip, b"clip").unwrap();
    import(&show, &clip, "9");
    let lost = root.path("lost.ppm");
    fs::write(&lost, b"lost").unwrap();
    import(&show, &lost, "10");
    fs::remove_file(show.join("assets/clip.ppm")).unwrap();
    fs::remove_file(show.join("assets/lost.ppm")).unwrap();

    let search = root.path("search");
    fs::create_dir_all(search.join("nested")).unwrap();
    fs::write(search.join("nested/clip.ppm"), b"clip").unwrap();

    let audit = run(&["asset-audit", text(&show)]);
    assert!(!audit.status.success());

    let manifest = fs::read(show.join("project.json")).unwrap();
    let preview = assert_success(&run(&[
        "project-relink",
        text(&show),
        "--search",
        text(&search),
    ]));
    assert!(
        preview.contains("input=9 asset=asset://clip.ppm reason=missing-asset import "),
        "{preview}"
    );
    assert!(
        preview.contains("input=10 asset=asset://lost.ppm reason=missing-asset not-found"),
        "{preview}"
    );
    assert!(
        preview.contains("preview only; 1 of 2 input(s)"),
        "{preview}"
    );
    assert_eq!(fs::read(show.join("project.json")).unwrap(), manifest);
    assert!(!show.join("assets/clip.ppm").exists());

    let applied = assert_success(&run(&[
        "project-relink",
        text(&show),
        "--search",
        text(&search),
        "--apply",
    ]));
    assert!(
        applied.contains("relinked=1 unresolved=1 imported_bytes=4"),
        "{applied}"
    );
    assert_eq!(fs::read(show.join("assets/clip.ppm")).unwrap(), b"clip");

    fs::write(show.join("assets/lost.ppm"), b"lost").unwrap();
    let clean = assert_success(&run(&["project-relink", text(&show), "--apply"]));
    assert_eq!(clean.trim(), "project-relink: no unresolved media");
    assert_success(&run(&["asset-audit", text(&show)]));

    assert_failure_contains(
        &run(&[
            "project-relink",
            text(&show),
            "--search",
            text(&root.path("nowhere")),
        ]),
        "is not a directory",
    );
    assert_failure_contains(
        &run(&["project-relink", text(&show), "--bogus"]),
        "unknown option",
    );
}

#[test]
fn export_bundle_copies_referenced_media_and_never_overwrites() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Show");
    let clip = root.path("clip.ppm");
    fs::write(&clip, b"clip").unwrap();
    import(&show, &clip, "9");
    fs::write(show.join("assets/unused.ppm"), b"unused").unwrap();
    assert_success(&run(&["project-autosave", text(&show)]));

    let portable = root.path("portable.freemix");
    let exported = assert_success(&run(&[
        "project-export-bundle",
        text(&show),
        text(&portable),
    ]));
    assert!(exported.contains("assets=1 bytes=4"), "{exported}");
    assert!(
        exported.contains("asset asset://clip.ppm bytes=4"),
        "{exported}"
    );
    assert_eq!(load(&portable), load(&show));
    assert_eq!(fs::read(portable.join("assets/clip.ppm")).unwrap(), b"clip");
    assert!(!portable.join("assets/unused.ppm").exists());
    assert!(!portable.join("autosave").exists());

    let before = fs::read(portable.join("project.json")).unwrap();
    assert_failure_contains(
        &run(&["project-export-bundle", text(&show), text(&portable)]),
        "already exists and is never overwritten",
    );
    assert_eq!(fs::read(portable.join("project.json")).unwrap(), before);

    assert_failure_contains(
        &run(&[
            "project-export-bundle",
            text(&show),
            text(&show.join("nested.freemix")),
        ]),
        "lies inside the source bundle",
    );

    fs::remove_file(show.join("assets/clip.ppm")).unwrap();
    let broken = root.path("broken.freemix");
    let failure = run(&["project-export-bundle", text(&show), text(&broken)]);
    assert_failure_contains(&failure, "input=9 reason=missing-asset");
    assert_failure_contains(&failure, "project-relink");
    assert!(!broken.exists());
}

#[test]
fn template_instantiation_gets_fresh_identity_and_requires_a_name() {
    let root = TempRoot::new();
    let template = root.path("template.freemix");
    new_project(&template, "Template");
    let clip = root.path("bumper.ppm");
    fs::write(&clip, b"bumper").unwrap();
    import(&template, &clip, "9");
    assert_success(&run(&["cut", text(&template)]));

    let show = root.path("show.freemix");
    let created = assert_success(&run(&[
        "project-from-template",
        text(&template),
        text(&show),
        "--name",
        "Friday Show",
    ]));
    assert!(created.contains("name=\"Friday Show\""), "{created}");
    let source = load(&template);
    let fresh = load(&show);
    assert_ne!(fresh.project().id(), source.project().id());
    assert!(created.contains(&format!("project_id={}", fresh.project().id())));
    assert_eq!(fresh.project().name(), "Friday Show");
    assert_eq!(fresh.position().revision, 0);
    assert_eq!(fresh.idempotency_receipts(), []);
    assert_eq!(
        fresh.project().inputs().len(),
        source.project().inputs().len()
    );
    assert_eq!(fs::read(show.join("assets/bumper.ppm")).unwrap(), b"bumper");
    assert_eq!(load(&template), source);

    assert_failure_contains(
        &run(&[
            "project-from-template",
            text(&template),
            text(&root.path("x.freemix")),
        ]),
        "missing value for --name",
    );
    assert_failure_contains(
        &run(&[
            "project-from-template",
            text(&template),
            text(&root.path("x.freemix")),
            "--name",
            "  ",
        ]),
        "must not be blank",
    );
    assert_failure_contains(
        &run(&[
            "project-from-template",
            text(&template),
            text(&show),
            "--name",
            "Again",
        ]),
        "already exists",
    );
}

#[test]
fn append_remaps_inputs_copies_media_and_saves_the_target() {
    let root = TempRoot::new();
    let target = root.path("target.freemix");
    new_project(&target, "Target");
    let source = root.path("source.freemix");
    new_project(&source, "Source");
    let clip = root.path("guest.ppm");
    fs::write(&clip, b"guest").unwrap();
    import(&source, &clip, "9");
    assert_success(&run(&["cut", text(&target)]));
    let before = load(&target);

    let appended = assert_success(&run(&["project-append", text(&target), text(&source)]));
    assert!(
        appended.contains("appended 3 input(s) and 0 scene(s)"),
        "{appended}"
    );
    assert!(appended.contains("input 9 -> 5"), "{appended}");
    assert!(
        appended.contains("renamed input 1: \"Input 1\" -> \"Input 1 (2)\""),
        "{appended}"
    );
    assert!(
        appended.contains("asset asset://guest.ppm -> asset://guest.ppm copied bytes=5"),
        "{appended}"
    );
    assert!(appended.contains("main_mix=true"), "{appended}");

    let after = load(&target);
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(after.project().inputs().len(), 5);
    assert_eq!(media_uri(&after, 5), "asset://guest.ppm");
    assert_eq!(fs::read(target.join("assets/guest.ppm")).unwrap(), b"guest");
    assert_success(&run(&["asset-audit", text(&target)]));
    assert_success(&run(&["status", text(&target)]));

    let manifest = fs::read(target.join("project.json")).unwrap();
    assert_failure_contains(
        &run(&[
            "project-append",
            text(&target),
            text(&root.path("absent.freemix")),
        ]),
        "append failed",
    );
    assert_eq!(fs::read(target.join("project.json")).unwrap(), manifest);
}

#[test]
fn autosave_rotates_and_recover_reports_then_restores() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Show");

    let empty = assert_success(&run(&["project-recover", text(&show)]));
    assert!(empty.contains("0 autosave generation(s)"), "{empty}");
    assert!(empty.contains("no usable autosave"), "{empty}");
    assert_failure_contains(
        &run(&["project-recover", text(&show), "--restore"]),
        "no usable autosave to restore",
    );

    for _ in 0..3 {
        assert_success(&run(&["project-autosave", text(&show), "--keep", "2"]));
    }
    let names: Vec<String> = {
        let mut names: Vec<String> = fs::read_dir(show.join("autosave"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names,
        [
            "autosave-00000000000000000002.json",
            "autosave-00000000000000000003.json"
        ]
    );
    assert_failure_contains(
        &run(&["project-autosave", text(&show), "--keep", "0"]),
        "outside 1..=64",
    );

    let saved = load(&show);
    assert_success(&run(&["project-rename", text(&show), "Edited"]));
    fs::write(
        show.join("autosave/autosave-00000000000000000004.json"),
        b"{ damaged",
    )
    .unwrap();

    let report = assert_success(&run(&["project-recover", text(&show)]));
    assert!(report.contains("3 autosave generation(s)"), "{report}");
    assert!(
        report.contains("rejected generation=4 reason=unreadable"),
        "{report}"
    );
    assert!(
        report.contains("recoverable generation=3 revision=0"),
        "{report}"
    );
    assert!(report.contains("differs_from_manifest=true"), "{report}");
    assert!(report.contains("rerun with --restore"), "{report}");
    assert_eq!(load(&show).project().name(), "Edited");

    let restored = assert_success(&run(&["project-recover", text(&show), "--restore"]));
    assert!(
        restored.contains("restored generation 3 to project.json"),
        "{restored}"
    );
    assert_eq!(load(&show), saved);

    let unchanged = assert_success(&run(&["project-recover", text(&show), "--restore"]));
    assert!(unchanged.contains("nothing restored"), "{unchanged}");

    // A corrupt manifest is recoverable when no journal exists.
    fs::write(show.join("project.json"), b"not json").unwrap();
    let corrupt = assert_success(&run(&["project-recover", text(&show), "--restore"]));
    assert!(corrupt.contains("manifest unusable"), "{corrupt}");
    assert_eq!(load(&show), saved);
}

#[test]
fn recover_refuses_to_restore_behind_the_journal_checkpoint() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Show");
    assert_success(&run(&["project-autosave", text(&show)]));
    assert_success(&run(&["cut", text(&show)]));
    assert_success(&run(&["journal-recover", text(&show)]));
    let manifest = fs::read(show.join("project.json")).unwrap();

    assert_failure_contains(
        &run(&["project-recover", text(&show), "--restore"]),
        "older than the journal checkpoint revision 1",
    );
    assert_eq!(fs::read(show.join("project.json")).unwrap(), manifest);
}

#[test]
fn recent_projects_record_list_prune_and_remove() {
    let root = TempRoot::new();
    let recent = root.path("state/recent.json");
    let first = root.path("first.freemix");
    let second = root.path("second.freemix");
    new_project(&first, "First");
    new_project(&second, "Second");

    let empty = assert_success(&run(&["recent-projects", text(&recent)]));
    assert_eq!(empty.trim(), "recent-projects: none");
    assert!(!recent.exists());

    assert_success(&run(&["recent-projects-add", text(&recent), text(&first)]));
    assert_success(&run(&["recent-projects-add", text(&recent), text(&second)]));
    let relative_alias = first.join("..").join("first.freemix");
    assert_success(&run(&[
        "recent-projects-add",
        text(&recent),
        text(&relative_alias),
    ]));
    let listed = assert_success(&run(&["recent-projects", text(&recent)]));
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 2, "{listed}");
    assert!(lines[0].contains("name=\"First\""), "{listed}");
    assert!(lines[0].ends_with("status=available"), "{listed}");
    assert!(lines[1].contains("name=\"Second\""), "{listed}");

    let capped = assert_success(&run(&[
        "recent-projects-add",
        text(&recent),
        text(&second),
        "--capacity",
        "1",
    ]));
    assert!(capped.contains("1 of 1 entries"), "{capped}");
    assert_success(&run(&["recent-projects-add", text(&recent), text(&first)]));

    fs::remove_dir_all(&second).unwrap();
    let listed = assert_success(&run(&["recent-projects", text(&recent)]));
    assert!(
        listed.lines().nth(1).unwrap().contains("name=\"Second\"")
            && listed.lines().nth(1).unwrap().ends_with("status=missing"),
        "{listed}"
    );

    let pruned = assert_success(&run(&["recent-projects-prune", text(&recent)]));
    assert!(pruned.contains("removed 1 unavailable entry"), "{pruned}");
    let listed = assert_success(&run(&["recent-projects", text(&recent)]));
    assert_eq!(listed.lines().count(), 1, "{listed}");

    assert_success(&run(&[
        "recent-projects-remove",
        text(&recent),
        text(&first),
    ]));
    assert_eq!(
        assert_success(&run(&["recent-projects", text(&recent)])).trim(),
        "recent-projects: none"
    );
    assert_failure_contains(
        &run(&["recent-projects-remove", text(&recent), text(&first)]),
        "is not in the recent-projects list",
    );
    assert_failure_contains(
        &run(&[
            "recent-projects-add",
            text(&recent),
            text(&root.path("absent.freemix")),
        ]),
        "error:",
    );
    assert_failure_contains(
        &run(&[
            "recent-projects-add",
            text(&recent),
            text(&first),
            "--capacity",
            "0",
        ]),
        "capacity 0 is outside 1..=100",
    );

    fs::write(&recent, b"{\"contract\":\"other\"}").unwrap();
    assert!(!run(&["recent-projects", text(&recent)]).status.success());
}

#[test]
fn settings_export_and_import_preview_the_project() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    new_project(&show, "Settings show");
    let export = root.path("settings.json");
    let out = assert_success(&run(&["settings-export", text(&show), text(&export)]));
    assert_eq!(out.trim(), "settings-export: 7 entries");
    let json = fs::read_to_string(&export).unwrap();
    assert!(
        json.contains("\"contract\": \"freemix.settings-export\""),
        "{json}"
    );
    assert!(json.contains("\"format.frame_rate.numerator\""), "{json}");
    let preview = assert_success(&run(&["settings-import", text(&show), text(&export)]));
    assert_eq!(
        preview.trim(),
        "settings-import: 0 added, 0 changed, 0 removed; nothing applied"
    );
    let changed = json.replace(
        "\"format.audio.channel_count\": {\"integer\": 2}",
        "\"format.audio.channel_count\": {\"integer\": 3}",
    );
    assert_ne!(changed, json);
    let changed_path = root.path("changed.json");
    fs::write(&changed_path, changed).unwrap();
    let preview = assert_success(&run(&["settings-import", text(&show), text(&changed_path)]));
    assert!(
        preview.contains("changed format.audio.channel_count"),
        "{preview}"
    );
    assert!(
        preview.contains("settings-import: 0 added, 1 changed, 0 removed; nothing applied"),
        "{preview}"
    );
}

#[test]
fn title_design_check_validates_a_designer_document() {
    let root = TempRoot::new();
    let design = root.path("empty.fmdesign");
    let document = format!(
        r##"{{
  "contract": "freemix.title-design",
  "schema_version": {CURRENT_SCHEMA_VERSION},
  "id": 1,
  "name": "Empty",
  "width": 64,
  "height": 32,
  "background": "#00000000",
  "fields": [],
  "nodes": [],
  "roots": [],
  "children": [],
  "animation": {{"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}}
}}"##
    );
    fs::write(&design, document).unwrap();
    let out = assert_success(&run(&["title-design-check", text(&design)]));
    assert_eq!(
        out.trim(),
        "title-design: id=1 name=Empty nodes=0 roots=0 fields=0"
    );
    fs::write(&design, "{\"contract\": \"nope\"}").unwrap();
    assert!(!run(&["title-design-check", text(&design)]).status.success());
}
