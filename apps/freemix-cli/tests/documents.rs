//! End-to-end coverage of document inputs in a local `.freemix` bundle
//! (IN-014): importing a PDF or a presentation adds a document input, and
//! `input-page` steps it against the page count read from the asset.

use std::{
    fs,
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_model::InputKind;
use fm_persistence::ProjectStore;
use fm_presentations::sample::{SampleSlide, SolidPage, pptx, solid_pages_pdf};
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
            "freemix-cli-documents-{}-{nonce}-{sequence}",
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

/// The stored page of document input `id`.
fn stored_page(show: &Path, id: u128) -> u32 {
    let stored = ProjectStore::new(show).unwrap().load().unwrap();
    let input = InputId::new(NonZeroU128::new(id).unwrap());
    stored.project().document(input).unwrap().page
}

fn page(show: &Path, input: &str, action: &[&str]) -> Output {
    let mut arguments = vec!["input-page", text(show), input];
    arguments.extend_from_slice(action);
    run(&arguments)
}

#[test]
fn imported_documents_step_through_their_pages_and_keep_them() {
    let root = TempRoot::new();
    let show = root.path("show.freemix");
    assert_success(&run(&["new", text(&show), "--name", "Show"]));
    let deck = root.path("Quarterly review.pdf");
    let solid = |rgb| SolidPage {
        width: 160,
        height: 90,
        rgb,
    };
    fs::write(
        &deck,
        solid_pages_pdf(
            Some("Quarterly review"),
            &[solid([255, 0, 0]), solid([0, 255, 0]), solid([0, 0, 255])],
        ),
    )
    .unwrap();
    let talk = root.path("talk.pptx");
    let slide = SampleSlide {
        notes: Some("Welcome everyone"),
        hidden: false,
    };
    fs::write(&talk, pptx(Some("Talk"), &[slide, slide])).unwrap();

    for (file, input) in [(&deck, "5"), (&talk, "6")] {
        assert_success(&run(&[
            "project-import-media",
            text(&show),
            text(file),
            "--input",
            input,
        ]));
    }
    let stored = ProjectStore::new(&show).unwrap().load().unwrap();
    let kinds = stored
        .project()
        .inputs()
        .iter()
        .map(|input| (input.id.get().get(), &input.kind))
        .collect::<Vec<_>>();
    assert!(
        kinds.iter().any(|(id, kind)| *id == 5
            && matches!(kind, InputKind::Document(document)
                if document.asset_uri == "asset://Quarterly review.pdf" && document.page == 0)),
        "{kinds:?}"
    );
    assert!(
        kinds
            .iter()
            .any(|(id, kind)| *id == 6 && matches!(kind, InputKind::Document(_))),
        "{kinds:?}"
    );

    // A presentation's page count is read without a converter.
    let pages = assert_success(&run(&["input-pages", text(&show)]));
    assert!(pages.contains("input=5 page=1 count=3"), "{pages}");
    assert!(pages.contains("input=6 page=1 count=2"), "{pages}");

    // Steps clamp to the document and persist with the project.
    for _ in 0..3 {
        assert_success(&page(&show, "5", &["next"]));
    }
    assert_eq!(stored_page(&show, 5), 2);
    assert_success(&page(&show, "5", &["previous"]));
    assert_eq!(stored_page(&show, 5), 1);
    assert_success(&page(&show, "5", &["first"]));
    assert_eq!(stored_page(&show, 5), 0);
    assert_success(&page(&show, "5", &["goto", "99"]));
    assert_eq!(stored_page(&show, 5), 2);
    assert_success(&page(&show, "6", &["last"]));
    assert_eq!(stored_page(&show, 6), 1);
    let pages = assert_success(&run(&["input-pages", text(&show)]));
    assert!(pages.contains("input=5 page=3 count=3"), "{pages}");

    // Pages are one-based on the command line.
    assert_failure_contains(&page(&show, "5", &["goto", "0"]), "page");
    assert_failure_contains(&page(&show, "5", &["sideways"]), "page action");
    // Only a document input has pages.
    assert_failure_contains(&page(&show, "1", &["next"]), "not a paged input");

    // A document whose asset cannot be read has no page count, so a step
    // is refused rather than guessed.
    fs::remove_file(show.join("assets/Quarterly review.pdf")).unwrap();
    let pages = assert_success(&run(&["input-pages", text(&show)]));
    assert!(pages.contains("input=5 page=3 count=unknown"), "{pages}");
    assert_failure_contains(&page(&show, "5", &["previous"]), "pages are not known yet");
    assert_eq!(stored_page(&show, 5), 2);
}
