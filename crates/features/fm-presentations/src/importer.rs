//! The document importer: PDFs and presentation packages into the
//! normalized presentation model (IN-014).
//!
//! A document becomes one deck with one slide per page, each carrying its
//! presenter notes. Slide pictures are not embedded: a document input
//! renders each page on demand at the size it is shown
//! ([`crate::PdfDocument::render_page`]), so the report lists slide images
//! as downgraded. Build steps, links, timed advance, and looping are not
//! read from any format and are reported as dropped.

use core::num::NonZeroU128;

use fm_types::paged::DocumentFormat;

use crate::{
    Deck, DeckId, Document, DocumentError, DocumentId, FeatureLoss, ImportError, ImportLossKind,
    ImportOutcome, ImportReport, ImportSource, PresentationFeature, PresentationImporter,
    PresenterNotes, Slide, SlideId,
};

/// Imports PDFs, `.pptx`, and `.odp` files by name and bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocumentImporter;

impl PresentationImporter for DocumentImporter {
    fn import(&self, source: ImportSource<'_>) -> Result<ImportOutcome, ImportError> {
        let format = DocumentFormat::from_file_name(source.name).ok_or_else(|| {
            ImportError::UnsupportedFormat {
                format: source
                    .name
                    .rsplit_once('.')
                    .map_or_else(String::new, |(_, extension)| extension.to_owned()),
            }
        })?;
        let info = crate::probe(format, source.bytes).map_err(|error| match error {
            DocumentError::Unsupported(format) => ImportError::UnsupportedFormat {
                format: format.token().to_owned(),
            },
            DocumentError::Malformed(message) => ImportError::Malformed { message },
            other => ImportError::Importer {
                message: other.to_string(),
            },
        })?;
        let slides = (0..info.page_count)
            .map(|page| Slide {
                id: SlideId::new(
                    NonZeroU128::new(u128::from(page) + 1).expect("page ids start at one"),
                ),
                image: None,
                presenter_notes: info
                    .notes(page)
                    .map(|notes| PresenterNotes(notes.to_owned())),
                build_steps: Vec::new(),
                links: Vec::new(),
                auto_advance: None,
            })
            .collect();
        let document = Document {
            id: DocumentId::new(NonZeroU128::MIN),
            title: info.title.clone().unwrap_or_default(),
            decks: vec![Deck {
                id: DeckId::new(NonZeroU128::MIN),
                title: info.title.unwrap_or_default(),
                slides,
                looping: false,
            }],
        };
        let mut supported = vec![PresentationFeature::SlideImages];
        if format != DocumentFormat::Pdf {
            supported.push(PresentationFeature::PresenterNotes);
        }
        let dropped = |feature, detail: &str| FeatureLoss {
            feature,
            kind: ImportLossKind::Dropped,
            detail: detail.to_owned(),
            affected_slides: Vec::new(),
        };
        let losses = vec![
            FeatureLoss {
                feature: PresentationFeature::SlideImages,
                kind: ImportLossKind::Downgraded,
                detail: "pages render on demand at the size they are shown".to_owned(),
                affected_slides: Vec::new(),
            },
            dropped(
                PresentationFeature::BuildSteps,
                "animations and builds show as the finished slide",
            ),
            dropped(PresentationFeature::Links, "links are not followed"),
            dropped(
                PresentationFeature::TimedAutoAdvance,
                "slide timings are not read; the operator advances",
            ),
        ];
        ImportOutcome::new(document, ImportReport::new(supported, losses))
    }
}
