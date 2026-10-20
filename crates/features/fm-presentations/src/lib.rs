//! Presentation import contracts, deterministic navigation, and document
//! sources (IN-014).
//!
//! The normalized model and navigator are parser-neutral: format support is
//! supplied through [`PresentationImporter`], which returns the model and an
//! explicit fidelity report. [`DocumentImporter`] is the built-in importer
//! for PDFs and presentation packages.
//!
//! Document inputs show PDFs and presentations: [`PdfDocument`] renders PDF
//! pages in pure Rust, [`probe`] reads any document's page count, title, and
//! presenter notes (presentation packages without an office suite), and
//! [`OfficeConverter`] turns `.pptx`/`.odp` into PDF through a headless
//! `LibreOffice` when one is installed. Keynote files are exported to PDF
//! by the operator.

pub mod convert;
mod document;
mod import;
mod importer;
mod model;
mod navigation;
pub mod office;
pub mod sample;

pub use convert::{CONVERTER_ENV, DEFAULT_CONVERSION_TIMEOUT, OfficeConverter};
pub use document::{
    CONVERTER_UNAVAILABLE, DocumentError, DocumentInfo, KEYNOTE_UNSUPPORTED, MAX_DOCUMENT_BYTES,
    MAX_RENDER_SIDE, PdfDocument, RenderedPage, probe,
};
pub use import::{
    FeatureLoss, ImportError, ImportLossKind, ImportOutcome, ImportReport, ImportSource,
    LegalScope, LegalScopeReport, PresentationFeature, PresentationImporter, UnsupportedSurface,
    UnsupportedSurfaceReport,
};
pub use importer::DocumentImporter;
pub use model::{
    BuildStep, Deck, DeckId, Document, DocumentId, IdError, LinkTarget, ModelError, PresenterNotes,
    Slide, SlideId, SlideImage, SlideLink,
};
pub use navigation::{NavigationError, NavigationEvent, PresentationNavigator};
