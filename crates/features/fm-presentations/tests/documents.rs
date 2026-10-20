//! Document sources (IN-014): PDF rendering, presentation metadata and
//! notes without an office suite, the `LibreOffice` conversion worker, and
//! the importer into the navigation model.

use std::ffi::OsString;
use std::time::Duration;

use fm_presentations::sample::{self, SampleSlide, SolidPage};
use fm_presentations::{
    CONVERTER_UNAVAILABLE, DocumentError, DocumentImporter, ImportSource, OfficeConverter,
    PdfDocument, PresentationFeature, PresentationImporter, PresentationNavigator, probe,
};
use fm_types::paged::{DocumentFormat, DocumentState, PagedNavigation};

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

fn three_pages() -> Vec<u8> {
    sample::solid_pages_pdf(
        Some("Quarterly (review)"),
        &[
            SolidPage {
                width: 400,
                height: 300,
                rgb: RED,
            },
            SolidPage {
                width: 400,
                height: 300,
                rgb: GREEN,
            },
            SolidPage {
                width: 160,
                height: 90,
                rgb: BLUE,
            },
        ],
    )
}

fn assert_rgb(actual: Option<[u8; 4]>, expected: [u8; 3], label: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{label}: outside the page"));
    assert_eq!(actual[3], 255, "{label}: opaque");
    for channel in 0..3 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= 2,
            "{label}: {actual:?} is not {expected:?}"
        );
    }
}

#[test]
fn a_pdf_reports_its_pages_and_title() {
    let pdf = PdfDocument::open(three_pages()).unwrap();
    assert_eq!(pdf.page_count(), 3);
    assert_eq!(pdf.title().as_deref(), Some("Quarterly (review)"));
    let info = probe(DocumentFormat::Pdf, &three_pages()).unwrap();
    assert_eq!(info.page_count, 3);
    assert_eq!(info.notes(0), None);
}

#[test]
fn pages_render_at_the_target_size_with_their_colours_and_transparent_bands() {
    let pdf = PdfDocument::open(three_pages()).unwrap();
    // A 4:3 page in a 16:9 frame is pillarboxed: 1440x1080 centred.
    let page = pdf.render_page(0, 1920, 1080).unwrap();
    assert_eq!((page.width, page.height), (1920, 1080));
    assert_eq!(page.pixels.len(), 1920 * 1080 * 4);
    assert_rgb(page.pixel(960, 540), RED, "centre");
    assert_rgb(page.pixel(241, 1), RED, "page's top-left corner");
    assert_rgb(page.pixel(1678, 1078), RED, "page's bottom-right corner");
    assert_eq!(page.pixel(100, 540).unwrap()[3], 0, "left band");
    assert_eq!(page.pixel(1819, 540).unwrap()[3], 0, "right band");

    assert_rgb(
        pdf.render_page(1, 320, 240).unwrap().pixel(1, 1),
        GREEN,
        "page 2",
    );
    // A 16:9 page fills a 16:9 frame edge to edge.
    let full = pdf.render_page(2, 640, 360).unwrap();
    assert_rgb(full.pixel(0, 0), BLUE, "top-left");
    assert_rgb(full.pixel(639, 359), BLUE, "bottom-right");

    assert_eq!(
        pdf.render_page(3, 64, 64).unwrap_err(),
        DocumentError::NoSuchPage(3)
    );
    assert_eq!(
        pdf.render_page(0, 0, 64).unwrap_err(),
        DocumentError::InvalidSize
    );
}

#[test]
fn rendering_is_deterministic() {
    let pdf = PdfDocument::open(three_pages()).unwrap();
    assert_eq!(
        pdf.render_page(0, 320, 180).unwrap(),
        pdf.render_page(0, 320, 180).unwrap()
    );
}

#[test]
fn damaged_and_empty_pdfs_fail_with_a_reason() {
    let error = PdfDocument::open(b"not a pdf at all".to_vec()).unwrap_err();
    assert_eq!(error.state(), DocumentState::Failed);
    assert!(matches!(
        PdfDocument::open(sample::solid_pages_pdf(None, &[])).unwrap_err(),
        DocumentError::Malformed(_)
    ));
}

#[test]
fn pptx_notes_title_and_slide_count_read_without_an_office_suite() {
    let bytes = sample::pptx(
        Some("Launch & plan"),
        &[
            SampleSlide {
                notes: Some("Welcome everyone\nIntroduce the team"),
                hidden: false,
            },
            SampleSlide {
                notes: None,
                hidden: false,
            },
            SampleSlide {
                notes: Some("Hidden backup slide"),
                hidden: true,
            },
            SampleSlide {
                notes: Some("Questions <and> answers"),
                hidden: false,
            },
        ],
    );
    let info = probe(DocumentFormat::Pptx, &bytes).unwrap();
    // The hidden slide is skipped, as the PDF export skips it.
    assert_eq!(info.page_count, 3);
    assert_eq!(info.title.as_deref(), Some("Launch & plan"));
    assert_eq!(info.notes(0), Some("Welcome everyone\nIntroduce the team"));
    assert_eq!(info.notes(1), None);
    assert_eq!(info.notes(2), Some("Questions <and> answers"));
}

#[test]
fn odp_notes_read_without_an_office_suite() {
    let bytes = sample::odp(
        Some("Roadmap"),
        &[
            SampleSlide {
                notes: Some("First point"),
                hidden: false,
            },
            SampleSlide {
                notes: None,
                hidden: false,
            },
        ],
    );
    let info = probe(DocumentFormat::Odp, &bytes).unwrap();
    assert_eq!(info.page_count, 2);
    assert_eq!(info.title.as_deref(), Some("Roadmap"));
    assert_eq!(info.notes(0), Some("First point"));
    assert_eq!(info.notes(1), None);
}

#[test]
fn a_non_package_presentation_is_malformed() {
    assert!(matches!(
        probe(DocumentFormat::Pptx, b"plain text").unwrap_err(),
        DocumentError::Malformed(_)
    ));
}

#[test]
fn without_libreoffice_a_presentation_says_what_to_do() {
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        OfficeConverter::locate_in(None, Some(empty.path().as_os_str().to_owned())),
        None
    );
    let error = DocumentError::ConverterUnavailable;
    assert_eq!(error.state(), DocumentState::Unavailable);
    assert_eq!(error.message(), CONVERTER_UNAVAILABLE);
    assert!(CONVERTER_UNAVAILABLE.contains("install LibreOffice"));
    assert!(CONVERTER_UNAVAILABLE.contains("export the presentation to PDF"));
}

#[cfg(unix)]
mod conversion {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    /// A stand-in `soffice` that copies `pdf` to `<outdir>/<stem>.pdf`
    /// after checking it was called headless with a sandboxed profile, and
    /// records its environment.
    fn fake_soffice(directory: &Path, body: &str) -> PathBuf {
        let program = directory.join("soffice");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
        program
    }

    #[test]
    fn a_converter_on_path_turns_a_presentation_into_renderable_pdf() {
        let directory = tempfile::tempdir().unwrap();
        let pdf = directory.path().join("converted.pdf");
        std::fs::write(&pdf, three_pages()).unwrap();
        let env_log = directory.path().join("env.txt");
        let args_log = directory.path().join("args.txt");
        fake_soffice(
            directory.path(),
            &format!(
                r#"echo "$@" > {args}
env > {env}
outdir=""; last=""
while [ $# -gt 0 ]; do
  case "$1" in --outdir) outdir="$2"; shift;; esac
  last="$1"; shift
done
cp {pdf} "$outdir/presentation.pdf""#,
                args = args_log.display(),
                env = env_log.display(),
                pdf = pdf.display()
            ),
        );
        let converter =
            OfficeConverter::locate_in(None, Some(OsString::from(directory.path().as_os_str())))
                .expect("soffice on PATH");
        let pdf_bytes = converter
            .convert(
                DocumentFormat::Pptx,
                &sample::pptx(None, &[]),
                Duration::from_secs(20),
            )
            .unwrap();
        assert_eq!(PdfDocument::open(pdf_bytes).unwrap().page_count(), 3);
        let args = std::fs::read_to_string(&args_log).unwrap();
        for flag in [
            "--headless",
            "--norestore",
            "--convert-to pdf",
            "-env:UserInstallation=file://",
        ] {
            assert!(args.contains(flag), "{flag} missing from {args}");
        }
        let env = std::fs::read_to_string(&env_log).unwrap();
        assert!(
            !env.contains("CARGO_"),
            "the converter's environment is cleared: {env}"
        );
        assert!(env.contains("HOME=") && env.contains("freemix-convert-"));
    }

    #[test]
    fn a_hung_converter_is_stopped_at_the_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let program = fake_soffice(directory.path(), "exec sleep 30");
        let started = std::time::Instant::now();
        let error = OfficeConverter::with_program(program)
            .convert(DocumentFormat::Odp, b"x", Duration::from_millis(300))
            .unwrap_err();
        assert_eq!(error, DocumentError::ConversionTimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(error.state(), DocumentState::Failed);
    }

    #[test]
    fn a_failing_converter_reports_its_first_diagnostic_line() {
        let directory = tempfile::tempdir().unwrap();
        let program = fake_soffice(
            directory.path(),
            "echo 'Error: source file could not be loaded' >&2; exit 1",
        );
        let error = OfficeConverter::with_program(program)
            .convert(DocumentFormat::Pptx, b"x", Duration::from_secs(20))
            .unwrap_err();
        assert_eq!(
            error,
            DocumentError::ConversionFailed("Error: source file could not be loaded".into())
        );
    }

    #[test]
    fn an_explicit_converter_override_is_the_only_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let program = fake_soffice(directory.path(), "exit 0");
        assert_eq!(
            OfficeConverter::locate_in(Some(program.clone().into_os_string()), None),
            Some(OfficeConverter::with_program(program))
        );
        assert_eq!(
            OfficeConverter::locate_in(
                Some(directory.path().join("missing").into_os_string()),
                Some(OsString::from(directory.path().as_os_str()))
            ),
            None
        );
    }
}

#[test]
fn imported_documents_navigate_like_paged_inputs() {
    let bytes = sample::pptx(
        Some("Deck"),
        &[
            SampleSlide {
                notes: Some("one"),
                hidden: false,
            },
            SampleSlide {
                notes: Some("two"),
                hidden: false,
            },
            SampleSlide {
                notes: None,
                hidden: false,
            },
        ],
    );
    let outcome = DocumentImporter
        .import(ImportSource {
            name: "deck.pptx",
            bytes: &bytes,
        })
        .unwrap();
    assert!(outcome.report.supports(PresentationFeature::PresenterNotes));
    let deck = &outcome.document.decks[0];
    assert_eq!(deck.slides.len(), 3);
    let mut navigator = PresentationNavigator::new(&outcome.document, deck.id).unwrap();
    let mut page = 0_u32;
    let count = 3;
    for step in [
        PagedNavigation::Next,
        PagedNavigation::Next,
        PagedNavigation::Previous,
        PagedNavigation::Next,
    ] {
        page = step.target(page, count);
        match step {
            PagedNavigation::Next => {
                navigator.next();
            }
            _ => {
                navigator.previous();
            }
        }
        let expected = &deck.slides[usize::try_from(page).unwrap()];
        assert_eq!(navigator.current_slide().id, expected.id);
        assert_eq!(
            navigator.presenter_notes(),
            expected.presenter_notes.as_ref()
        );
    }
    assert_eq!(page, 2);

    let pdf = DocumentImporter
        .import(ImportSource {
            name: "slides.PDF",
            bytes: &three_pages(),
        })
        .unwrap();
    assert_eq!(pdf.document.title, "Quarterly (review)");
    assert_eq!(pdf.document.decks[0].slides.len(), 3);
    assert!(
        DocumentImporter
            .import(ImportSource {
                name: "talk.key",
                bytes: b"PK"
            })
            .is_err()
    );
}
