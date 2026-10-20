//! Document sources for document inputs (IN-014): PDF rendering and the
//! metadata every document format reports.
//!
//! PDFs render through [`hayro`], a pure-Rust rasterizer, so no system PDF
//! library or tool is needed on Linux, macOS, or Windows. A page renders
//! straight to the size it is shown at (never scaled from a smaller
//! render): it is fitted inside the target with its aspect ratio kept,
//! centred, drawn opaque on the page's white, and the bands outside the
//! page are transparent, so a page shows over the compositor's black and
//! also works as an overlay or Scene layer.
//!
//! Presentations render after conversion to PDF ([`crate::convert`]); their
//! slide count, title, and presenter notes are read without converting
//! ([`crate::office`]). A Keynote file is never rendered: Keynote exports
//! PDF itself (File > Export To > PDF).

use std::fmt;

use fm_types::paged::{
    DocumentFormat, DocumentState, MAX_PAGED_MESSAGE_BYTES, MAX_PAGED_PAGES, MAX_PAGED_TITLE_BYTES,
    bounded_paged_text,
};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

/// Largest document file a document input reads.
pub const MAX_DOCUMENT_BYTES: u64 = 512 * 1024 * 1024;
/// Largest rendered page side, in pixels.
pub const MAX_RENDER_SIDE: u32 = 8_192;

/// What a presentation needs on a server that cannot render it.
pub const CONVERTER_UNAVAILABLE: &str =
    "install LibreOffice on the server or export the presentation to PDF";
/// What a Keynote file needs.
pub const KEYNOTE_UNSUPPORTED: &str =
    "export the Keynote presentation to PDF (File > Export To > PDF)";

/// A document's page count, title, and per-page presenter notes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentInfo {
    pub format: DocumentFormat,
    /// At least one and at most [`MAX_PAGED_PAGES`].
    pub page_count: u32,
    pub title: Option<String>,
    /// One entry per page (empty for a page without notes); a PDF has none.
    pub notes: Vec<String>,
}

impl DocumentInfo {
    /// The notes of zero-based `page`, if it has any.
    #[must_use]
    pub fn notes(&self, page: u32) -> Option<&str> {
        self.notes
            .get(usize::try_from(page).ok()?)
            .map(String::as_str)
            .filter(|notes| !notes.is_empty())
    }
}

/// Why a document cannot be read, converted, or rendered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    /// The format is never rendered here (Keynote).
    Unsupported(DocumentFormat),
    /// No converter is installed for a presentation.
    ConverterUnavailable,
    /// The file is not a readable document of its format.
    Malformed(String),
    /// The PDF is encrypted.
    Encrypted,
    /// More pages than [`MAX_PAGED_PAGES`].
    TooManyPages(u32),
    /// The file is larger than [`MAX_DOCUMENT_BYTES`].
    TooLarge,
    /// The page does not exist.
    NoSuchPage(u32),
    /// The requested render size is empty or too large.
    InvalidSize,
    /// The converter failed or produced no PDF.
    ConversionFailed(String),
    /// The converter did not finish in time and was stopped.
    ConversionTimedOut,
    /// Reading the file failed.
    Io(String),
}

impl DocumentError {
    /// The state a document input in this error shows.
    #[must_use]
    pub const fn state(&self) -> DocumentState {
        match self {
            Self::Unsupported(_) | Self::ConverterUnavailable => DocumentState::Unavailable,
            _ => DocumentState::Failed,
        }
    }

    /// The error as a bounded status message.
    #[must_use]
    pub fn message(&self) -> String {
        bounded_paged_text(&self.to_string(), MAX_PAGED_MESSAGE_BYTES, false)
    }
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(DocumentFormat::Keynote) => formatter.write_str(KEYNOTE_UNSUPPORTED),
            Self::Unsupported(format) => {
                write!(formatter, "{} documents cannot be shown", format.token())
            }
            Self::ConverterUnavailable => formatter.write_str(CONVERTER_UNAVAILABLE),
            Self::Malformed(detail) => write!(formatter, "the document is damaged: {detail}"),
            Self::Encrypted => formatter.write_str("the PDF is password protected"),
            Self::TooManyPages(count) => write!(
                formatter,
                "the document has {count} pages; at most {MAX_PAGED_PAGES} are shown"
            ),
            Self::TooLarge => write!(
                formatter,
                "the document is larger than {} MiB",
                MAX_DOCUMENT_BYTES / (1024 * 1024)
            ),
            Self::NoSuchPage(page) => write!(formatter, "page {} does not exist", page + 1),
            Self::InvalidSize => formatter.write_str("the page size is empty or too large"),
            Self::ConversionFailed(detail) => {
                write!(formatter, "conversion to PDF failed: {detail}")
            }
            Self::ConversionTimedOut => formatter.write_str("conversion to PDF timed out"),
            Self::Io(detail) => write!(formatter, "the document cannot be read: {detail}"),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Reads a document's page count, title, and notes without rendering it:
/// a PDF is parsed, a presentation package's XML is read.
///
/// # Errors
///
/// [`DocumentError::Unsupported`] for Keynote, and the format's read errors.
pub fn probe(format: DocumentFormat, bytes: &[u8]) -> Result<DocumentInfo, DocumentError> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(DocumentError::TooLarge);
    }
    match format {
        DocumentFormat::Pdf => PdfDocument::open(bytes.to_vec()).map(|pdf| pdf.info()),
        DocumentFormat::Pptx | DocumentFormat::Odp => {
            crate::office::probe_presentation(format, bytes)
        }
        DocumentFormat::Keynote => Err(DocumentError::Unsupported(format)),
    }
}

/// One rendered page: `width` x `height` straight-alpha RGBA8, rows packed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RenderedPage {
    /// The straight RGBA of pixel (`x`, `y`).
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (usize::try_from(y).ok()? * usize::try_from(self.width).ok()?
            + usize::try_from(x).ok()?)
            * 4;
        self.pixels
            .get(offset..offset + 4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
    }
}

/// A parsed PDF that renders pages on demand.
pub struct PdfDocument {
    pdf: Pdf,
}

impl fmt::Debug for PdfDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PdfDocument")
            .field("pages", &self.pdf.pages().len())
            .finish_non_exhaustive()
    }
}

impl PdfDocument {
    /// Parses a PDF.
    ///
    /// # Errors
    ///
    /// [`DocumentError::Encrypted`] for a password-protected file,
    /// [`DocumentError::Malformed`] for one that does not parse or has no
    /// pages, and [`DocumentError::TooManyPages`].
    pub fn open(bytes: Vec<u8>) -> Result<Self, DocumentError> {
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge);
        }
        let pdf = Pdf::new(bytes).map_err(|error| match error {
            LoadPdfError::Decryption(_) => DocumentError::Encrypted,
            LoadPdfError::Invalid => DocumentError::Malformed("not a readable PDF".into()),
        })?;
        let count = pdf.pages().len();
        if count == 0 {
            return Err(DocumentError::Malformed("the PDF has no pages".into()));
        }
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        if count > MAX_PAGED_PAGES {
            return Err(DocumentError::TooManyPages(count));
        }
        Ok(Self { pdf })
    }

    /// The number of pages (at least one).
    #[must_use]
    pub fn page_count(&self) -> u32 {
        u32::try_from(self.pdf.pages().len()).unwrap_or(u32::MAX)
    }

    /// The document information dictionary's title, if it has one.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        let raw = self.pdf.metadata().title.as_deref()?;
        let text = decode_pdf_text(raw);
        let title = bounded_paged_text(text.trim(), MAX_PAGED_TITLE_BYTES, false);
        (!title.is_empty()).then_some(title)
    }

    /// The page count and title (a PDF carries no presenter notes).
    #[must_use]
    pub fn info(&self) -> DocumentInfo {
        DocumentInfo {
            format: DocumentFormat::Pdf,
            page_count: self.page_count(),
            title: self.title(),
            notes: Vec::new(),
        }
    }

    /// Renders zero-based `page` fitted and centred inside `width` x
    /// `height`, rasterized at that size; see the module documentation.
    ///
    /// # Errors
    ///
    /// [`DocumentError::NoSuchPage`] and [`DocumentError::InvalidSize`].
    pub fn render_page(
        &self,
        page: u32,
        width: u32,
        height: u32,
    ) -> Result<RenderedPage, DocumentError> {
        if width == 0 || height == 0 || width > MAX_RENDER_SIDE || height > MAX_RENDER_SIDE {
            return Err(DocumentError::InvalidSize);
        }
        let pages = self.pdf.pages();
        let source = pages
            .get(usize::try_from(page).map_err(|_| DocumentError::NoSuchPage(page))?)
            .ok_or(DocumentError::NoSuchPage(page))?;
        let (page_width, page_height) = source.render_dimensions();
        let (fit_width, fit_height) = fit(page_width, page_height, width, height)?;
        #[allow(clippy::cast_precision_loss)]
        let settings = PixmapSettings {
            x_scale: fit_width as f32 / page_width,
            y_scale: fit_height as f32 / page_height,
            bg_color: WHITE,
        };
        let cache = RenderCache::new();
        let pixmap = hayro::render(
            source,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &settings,
        );
        let (rendered_width, rendered_height) =
            (u32::from(pixmap.width()), u32::from(pixmap.height()));
        let rendered = pixmap.data_as_u8_slice();
        let mut pixels = vec![0_u8; to_usize(width)? * to_usize(height)? * 4];
        let left = (width - rendered_width.min(width)) / 2;
        let top = (height - rendered_height.min(height)) / 2;
        let row_bytes = to_usize(rendered_width.min(width))? * 4;
        for row in 0..rendered_height.min(height) {
            let from = to_usize(row)? * to_usize(rendered_width)? * 4;
            let to = (to_usize(row + top)? * to_usize(width)? + to_usize(left)?) * 4;
            pixels[to..to + row_bytes].copy_from_slice(&rendered[from..from + row_bytes]);
        }
        unpremultiply(&mut pixels);
        Ok(RenderedPage {
            width,
            height,
            pixels,
        })
    }
}

fn to_usize(value: u32) -> Result<usize, DocumentError> {
    usize::try_from(value).map_err(|_| DocumentError::InvalidSize)
}

/// A page of `page_width` x `page_height` points fitted inside `width` x
/// `height` pixels, aspect kept, each side at least one pixel.
fn fit(
    page_width: f32,
    page_height: f32,
    width: u32,
    height: u32,
) -> Result<(u32, u32), DocumentError> {
    if !(page_width.is_finite() && page_height.is_finite())
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return Err(DocumentError::Malformed("the page has no size".into()));
    }
    let (page_width, page_height) = (f64::from(page_width), f64::from(page_height));
    let (target_width, target_height) = (f64::from(width), f64::from(height));
    let scale = (target_width / page_width).min(target_height / page_height);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let fitted = |side: f64, limit: u32| ((side * scale).round() as u32).clamp(1, limit);
    Ok((fitted(page_width, width), fitted(page_height, height)))
}

fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let straight = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = u8::try_from(straight.min(255)).unwrap_or(u8::MAX);
        }
    }
}

/// A PDF text string: UTF-16BE with a byte order mark, UTF-8 with one, or
/// (approximately) `PDFDocEncoding` as Latin-1.
fn decode_pdf_text(raw: &[u8]) -> String {
    if let Some(utf16) = raw.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = utf16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(utf8) = raw.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(utf8).into_owned();
    }
    raw.iter().map(|byte| char::from(*byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_fit_inside_the_target_with_their_aspect() {
        assert_eq!(fit(400.0, 300.0, 1920, 1080).unwrap(), (1440, 1080));
        assert_eq!(fit(1600.0, 900.0, 1920, 1080).unwrap(), (1920, 1080));
        assert_eq!(fit(100.0, 1000.0, 160, 90).unwrap(), (9, 90));
        assert!(fit(0.0, 10.0, 10, 10).is_err());
    }

    #[test]
    fn pdf_text_strings_decode() {
        assert_eq!(decode_pdf_text(b"Plain"), "Plain");
        assert_eq!(decode_pdf_text(&[0xfe, 0xff, 0, b'O', 0, b'K']), "OK");
        assert_eq!(decode_pdf_text(&[0xe9]), "\u{e9}");
    }

    #[test]
    fn keynote_is_never_rendered_and_says_so() {
        let error = probe(DocumentFormat::Keynote, b"PK").unwrap_err();
        assert_eq!(error.state(), DocumentState::Unavailable);
        assert!(error.message().contains("PDF"));
    }
}
