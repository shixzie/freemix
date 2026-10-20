//! Document inputs on the daemon (IN-014): PDFs and presentations shown
//! page by page.
//!
//! [`DocumentPages`] is the one place a document's bytes are read. It is
//! shared (behind an [`Arc`]) by everything that needs a page: the session
//! loop (status and presenter notes), the simulated monitor tap and the
//! input thumbnail worker (their pictures), and the native page worker (the
//! live source frame).
//!
//! A document asset is read once per URI and probed synchronously: a PDF is
//! parsed with the pure-Rust `hayro` renderer, a PowerPoint or
//! `OpenDocument` presentation's XML package is read for its slide count,
//! title, and presenter notes (no converter needed). The probe depends only
//! on the file's bytes, so the page count it reports to the engine
//! ([`ControlService::set_paged_source`]) is the same when the journal
//! replays a show as when the show ran: [`sync`] reports every document
//! before and after each command, so a replayed page step lands where it
//! first did.
//!
//! Showing a presentation needs a PDF: when LibreOffice is installed the
//! presentation converts headlessly on its own thread, in a private sandbox
//! and with a timeout (see [`fm_presentations::OfficeConverter`]), and the
//! input reads `converting` until it is `ready`. Without a converter it
//! reads `unavailable` with the instruction to install LibreOffice or
//! export to PDF, while its page count and notes still work. Keynote
//! packages are never converted: the operator exports them to PDF.
//!
//! Pages render on demand at the size the caller asks for (the project's
//! picture size, the monitor size, a thumbnail), never on the session loop,
//! and the most recent renders are kept in a bounded cache.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use fm_auth::Policy;
use fm_control::{ControlService, PagedSourceReport};
use fm_presentations::{
    DEFAULT_CONVERSION_TIMEOUT, DocumentError, DocumentInfo, MAX_DOCUMENT_BYTES, OfficeConverter,
    PdfDocument, RenderedPage,
};
use fm_protocol::{
    DocumentFormat, DocumentState, MAX_PAGED_NOTES_BYTES, MAX_PAGED_THUMBNAIL_BYTES,
    MAX_PAGED_THUMBNAIL_HEIGHT, MAX_PAGED_THUMBNAIL_WIDTH, PagedPageContent, PagedPageMessage,
    PagedPageRequest, ServerIdentity, bounded_paged_text, encode_qoi_rgb,
};
use fm_sim::ImageFrame;

use crate::AppResult;

/// The most rendered page bytes kept for reuse.
const PAGE_CACHE_BYTES: usize = 96 * 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What one document asset can show.
enum Renderable {
    /// A presentation converting to PDF on its own thread.
    Converting,
    /// A PDF (the asset itself or a presentation's conversion). The parser
    /// renders one page at a time.
    Ready(Arc<Mutex<PdfDocument>>),
    /// Nothing renders; the error says why.
    Failed(DocumentError),
}

/// One document asset, read once.
struct Source {
    /// The page count, title, and notes, or why the asset cannot be read.
    probe: Result<DocumentInfo, DocumentError>,
    renderable: Arc<Mutex<Renderable>>,
}

impl Source {
    fn failed(error: DocumentError) -> Self {
        Self {
            probe: Err(error.clone()),
            renderable: Arc::new(Mutex::new(Renderable::Failed(error))),
        }
    }

    /// The status the engine and clients see.
    fn report(&self) -> PagedSourceReport {
        let info = match &self.probe {
            Ok(info) => info,
            Err(error) => {
                return PagedSourceReport {
                    count: None,
                    state: error.state(),
                    title: None,
                    message: Some(error.message()),
                };
            }
        };
        let (state, message) = match &*lock(&self.renderable) {
            Renderable::Converting => (DocumentState::Converting, None),
            Renderable::Ready(_) => (DocumentState::Ready, None),
            Renderable::Failed(error) => (error.state(), Some(error.message())),
        };
        PagedSourceReport {
            count: Some(info.page_count),
            state,
            title: info.title.clone(),
            message,
        }
    }

    fn pdf(&self) -> Option<Arc<Mutex<PdfDocument>>> {
        match &*lock(&self.renderable) {
            Renderable::Ready(pdf) => Some(Arc::clone(pdf)),
            Renderable::Converting | Renderable::Failed(_) => None,
        }
    }
}

/// Identity of one rendered page.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PageKey {
    uri: String,
    page: u32,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Inner {
    sources: BTreeMap<String, Arc<Source>>,
    /// Most recently used last.
    cache: VecDeque<(PageKey, Arc<RenderedPage>)>,
    cache_bytes: usize,
}

/// Reads, converts, and renders document assets; see the module
/// documentation.
pub(crate) struct DocumentPages {
    assets_root: PathBuf,
    /// Converts presentations to PDF, when installed.
    converter: Option<OfficeConverter>,
    inner: Mutex<Inner>,
    /// Set when a source's status changed off the session loop (a
    /// conversion finished), so the loop reports it again.
    changed: Arc<AtomicBool>,
}

impl Default for DocumentPages {
    /// Pages of nothing: every document is missing.
    fn default() -> Self {
        Self::new(PathBuf::new(), None)
    }
}

impl DocumentPages {
    /// Pages of the assets under `assets_root`, converting presentations
    /// with `converter`.
    pub(crate) fn new(assets_root: impl Into<PathBuf>, converter: Option<OfficeConverter>) -> Self {
        Self {
            assets_root: assets_root.into(),
            converter,
            inner: Mutex::new(Inner::default()),
            changed: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Pages of the assets under `assets_root`, converting presentations
    /// with the LibreOffice this host has (`FREEMIX_SOFFICE`, then
    /// `soffice` on `PATH`).
    pub(crate) fn with_host_converter(assets_root: impl Into<PathBuf>) -> Self {
        Self::new(assets_root, OfficeConverter::locate())
    }

    /// Whether a source changed off the session loop since the last call.
    pub(crate) fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// The status of document asset `uri`, reading it on first use.
    pub(crate) fn report(&self, uri: &str) -> PagedSourceReport {
        self.source(uri).report()
    }

    /// The presenter notes of zero-based `page` of `uri` (empty when it
    /// has none), bounded for the wire; `None` when the page does not exist.
    pub(crate) fn notes(&self, uri: &str, page: u32) -> Option<String> {
        let source = self.source(uri);
        let info = source.probe.as_ref().ok()?;
        if page >= info.page_count {
            return None;
        }
        Some(
            info.notes(page)
                .map(|notes| bounded_paged_text(notes, MAX_PAGED_NOTES_BYTES, true))
                .unwrap_or_default(),
        )
    }

    /// Zero-based `page` of `uri` fitted and centred in `width` x `height`
    /// (straight-alpha RGBA8, transparent outside the page), rendering it
    /// unless a recent render matches. `None` while nothing renders. Never
    /// call this on the session loop: a render takes as long as the page.
    pub(crate) fn render(
        &self,
        uri: &str,
        page: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<RenderedPage>> {
        let key = PageKey {
            uri: uri.to_owned(),
            page,
            width,
            height,
        };
        {
            let mut inner = lock(&self.inner);
            if let Some(index) = inner.cache.iter().position(|(cached, _)| *cached == key) {
                let entry = inner.cache.remove(index)?;
                let page = Arc::clone(&entry.1);
                inner.cache.push_back(entry);
                return Some(page);
            }
        }
        let pdf = self.source(uri).pdf()?;
        let rendered = Arc::new(lock(&pdf).render_page(page, width, height).ok()?);
        let mut inner = lock(&self.inner);
        inner.cache_bytes += rendered.pixels.len();
        inner.cache.push_back((key, Arc::clone(&rendered)));
        while inner.cache_bytes > PAGE_CACHE_BYTES && inner.cache.len() > 1 {
            if let Some((_, evicted)) = inner.cache.pop_front() {
                inner.cache_bytes -= evicted.pixels.len();
            }
        }
        Some(rendered)
    }

    /// Forgets every asset no input names any more.
    fn retain(&self, uris: &BTreeSet<&str>) {
        let mut inner = lock(&self.inner);
        inner.sources.retain(|uri, _| uris.contains(uri.as_str()));
        let mut freed = 0;
        inner.cache.retain(|(key, page)| {
            let keep = uris.contains(key.uri.as_str());
            if !keep {
                freed += page.pixels.len();
            }
            keep
        });
        inner.cache_bytes -= freed;
    }

    /// The source of `uri`, reading and probing it on first use.
    fn source(&self, uri: &str) -> Arc<Source> {
        if let Some(source) = lock(&self.inner).sources.get(uri) {
            return Arc::clone(source);
        }
        let loaded = Arc::new(self.load(uri));
        Arc::clone(
            lock(&self.inner)
                .sources
                .entry(uri.to_owned())
                .or_insert(loaded),
        )
    }

    fn load(&self, uri: &str) -> Source {
        let Some(format) = DocumentFormat::from_file_name(uri) else {
            return Source::failed(DocumentError::Malformed("not a document".into()));
        };
        if format == DocumentFormat::Keynote {
            return Source::failed(DocumentError::Unsupported(format));
        }
        let bytes = match self.read(uri) {
            Ok(bytes) => bytes,
            Err(error) => return Source::failed(error),
        };
        if format == DocumentFormat::Pdf {
            return match PdfDocument::open(bytes) {
                Ok(pdf) => Source {
                    probe: Ok(pdf.info()),
                    renderable: Arc::new(Mutex::new(Renderable::Ready(Arc::new(Mutex::new(pdf))))),
                },
                Err(error) => Source::failed(error),
            };
        }
        let probe = fm_presentations::probe(format, &bytes);
        let Some(converter) = self.converter.clone().filter(|_| probe.is_ok()) else {
            return Source {
                probe,
                renderable: Arc::new(Mutex::new(Renderable::Failed(
                    DocumentError::ConverterUnavailable,
                ))),
            };
        };
        let renderable = Arc::new(Mutex::new(Renderable::Converting));
        let slot = Arc::clone(&renderable);
        let changed = Arc::clone(&self.changed);
        let asset = uri.to_owned();
        let spawned = thread::Builder::new()
            .name("freemixd-document-convert".into())
            .spawn(move || {
                let converted = converter
                    .convert(format, &bytes, DEFAULT_CONVERSION_TIMEOUT)
                    .and_then(PdfDocument::open);
                let result = match converted {
                    Ok(pdf) => Renderable::Ready(Arc::new(Mutex::new(pdf))),
                    Err(error) => {
                        eprintln!(
                            "FREEMIXD_DOCUMENT\tv=1\tasset={asset}\tconversion=failed\terror={error}"
                        );
                        Renderable::Failed(error)
                    }
                };
                *lock(&slot) = result;
                changed.store(true, Ordering::Release);
            });
        if spawned.is_err() {
            *lock(&renderable) = Renderable::Failed(DocumentError::ConversionFailed(
                "the conversion thread could not start".into(),
            ));
        }
        Source { probe, renderable }
    }

    /// Reads a document asset, bounded by [`MAX_DOCUMENT_BYTES`].
    fn read(&self, uri: &str) -> Result<Vec<u8>, DocumentError> {
        let path = self
            .asset_path(uri)
            .ok_or_else(|| DocumentError::Io("not a bundle asset".into()))?;
        let metadata = fs::metadata(&path).map_err(|error| DocumentError::Io(error.to_string()))?;
        if metadata.len() > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge);
        }
        fs::read(&path).map_err(|error| DocumentError::Io(error.to_string()))
    }

    /// The bundle path of document asset `uri`.
    fn asset_path(&self, uri: &str) -> Option<PathBuf> {
        fm_model::validate_media_asset_uri(uri).ok()?;
        let key = uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)?;
        Some(self.assets_root.join(key))
    }
}

/// Reports every document input's source to `control` (its page count to
/// the engine, its state, title, and message to clients) and forgets assets
/// no input names. A report that changed nothing publishes nothing.
///
/// # Errors
///
/// A server identity mismatch or an exhausted runtime sequence.
pub(crate) fn sync(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    pages: &DocumentPages,
) -> AppResult<()> {
    let documents = control.document_inputs();
    for (input, uri) in &documents {
        let report = pages.report(uri);
        control.set_paged_source(server, *input, &report)?;
    }
    pages.retain(&documents.iter().map(|(_, uri)| uri.as_str()).collect());
    Ok(())
}

/// A rendered page as the opaque picture it shows over black (as Program
/// shows a source with transparency).
pub(crate) fn page_over_black(page: &RenderedPage) -> Option<ImageFrame> {
    let mut pixels = page.pixels.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = u8::try_from((u16::from(*channel) * alpha + 127) / 255).unwrap_or(u8::MAX);
        }
        pixel[3] = u8::MAX;
    }
    let stride = usize::try_from(page.width).ok()?.checked_mul(4)?;
    ImageFrame::new(page.width, page.height, stride, pixels).ok()
}

/// A rendered page as a premultiplied picture keeping its transparency.
#[cfg(feature = "native-media")]
pub(crate) fn page_premultiplied(page: &RenderedPage) -> Option<ImageFrame> {
    let mut pixels = page.pixels.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = u8::try_from((u16::from(*channel) * alpha + 127) / 255).unwrap_or(u8::MAX);
        }
    }
    let stride = usize::try_from(page.width).ok()?.checked_mul(4)?;
    ImageFrame::new(page.width, page.height, stride, pixels).ok()
}

/// Answers one `paged_page_request` for document asset `uri` (`None`
/// when the input is not a document): the page's notes, or its thumbnail
/// fitted in the largest page thumbnail and shown over black. Renders, so
/// it runs on the page worker.
pub(crate) fn page_answer(
    pages: &DocumentPages,
    uri: Option<&str>,
    request: PagedPageRequest,
) -> PagedPageMessage {
    let content = uri
        .and_then(|uri| {
            if request.thumbnail {
                page_thumbnail(pages, uri, request.page)
            } else {
                pages.notes(uri, request.page).map(PagedPageContent::Notes)
            }
        })
        .unwrap_or(PagedPageContent::Unavailable);
    PagedPageMessage {
        request_id: request.request_id,
        input: request.input,
        page: request.page,
        content,
    }
}

fn page_thumbnail(pages: &DocumentPages, uri: &str, page: u32) -> Option<PagedPageContent> {
    let (width, height) = (MAX_PAGED_THUMBNAIL_WIDTH, MAX_PAGED_THUMBNAIL_HEIGHT);
    let rendered = pages.render(uri, page, u32::from(width), u32::from(height))?;
    let picture = page_over_black(&rendered)?;
    let data = encode_qoi_rgb(width, height, picture.pixels(), picture.stride(), 4)
        .ok()
        .filter(|data| data.len() <= MAX_PAGED_THUMBNAIL_BYTES)?;
    Some(PagedPageContent::Thumbnail {
        width,
        height,
        data,
    })
}

/// Most page requests one peer may have waiting; an older one is dropped
/// (a client asks again for what it still shows).
const MAX_PENDING_PER_PEER: usize = 32;

struct PageJob {
    peer: u64,
    uri: Option<String>,
    request: PagedPageRequest,
}

#[derive(Default)]
struct PageQueue {
    pending: VecDeque<PageJob>,
    finished: Vec<(u64, PagedPageMessage)>,
    shutdown: bool,
}

/// Answers `paged_page_request`s off the session loop, in order, at most
/// [`MAX_PENDING_PER_PEER`] waiting per peer.
pub(crate) struct PageWorker {
    shared: Arc<(Mutex<PageQueue>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PageWorker {
    /// Starts the worker answering from `pages`.
    ///
    /// # Errors
    ///
    /// The operating system error when the thread cannot start.
    pub(crate) fn spawn(pages: Arc<DocumentPages>) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(PageQueue::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("freemixd-document-pages".into())
            .spawn(move || {
                let (queue, wake) = &*worker_shared;
                loop {
                    let job = {
                        let mut queue = lock(queue);
                        loop {
                            if queue.shutdown {
                                return;
                            }
                            if let Some(job) = queue.pending.pop_front() {
                                break job;
                            }
                            queue = wake.wait(queue).unwrap_or_else(PoisonError::into_inner);
                        }
                    };
                    let answer = page_answer(&pages, job.uri.as_deref(), job.request);
                    lock(queue).finished.push((job.peer, answer));
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Queues `peer`'s request for document asset `uri`. Never waits.
    pub(crate) fn submit(&self, peer: u64, uri: Option<String>, request: PagedPageRequest) {
        let (queue, wake) = &*self.shared;
        let mut queue = lock(queue);
        if queue.pending.iter().filter(|job| job.peer == peer).count() >= MAX_PENDING_PER_PEER
            && let Some(oldest) = queue.pending.iter().position(|job| job.peer == peer)
        {
            queue.pending.remove(oldest);
        }
        queue.pending.push_back(PageJob { peer, uri, request });
        wake.notify_one();
    }

    /// The answers finished since the last call, in order.
    pub(crate) fn take_finished(&self) -> Vec<(u64, PagedPageMessage)> {
        std::mem::take(&mut lock(&self.shared.0).finished)
    }
}

impl Drop for PageWorker {
    fn drop(&mut self) {
        let (queue, wake) = &*self.shared;
        lock(queue).shutdown = true;
        wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use fm_presentations::sample::{SampleSlide, SolidPage, pptx, solid_pages_pdf};

    use super::*;

    fn bundle(files: &[(&str, Vec<u8>)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            fs::write(root.path().join(name), bytes).unwrap();
        }
        root
    }

    fn deck() -> Vec<u8> {
        solid_pages_pdf(
            Some("Quarterly review"),
            &[
                SolidPage {
                    width: 400,
                    height: 300,
                    rgb: [255, 0, 0],
                },
                SolidPage {
                    width: 400,
                    height: 300,
                    rgb: [0, 0, 255],
                },
            ],
        )
    }

    #[test]
    fn a_pdf_reports_its_pages_and_renders_them_cached() {
        let root = bundle(&[("deck.pdf", deck())]);
        let pages = DocumentPages::new(root.path(), None);
        let report = pages.report("asset://deck.pdf");
        assert_eq!(report.count, Some(2));
        assert_eq!(report.state, DocumentState::Ready);
        assert_eq!(report.title.as_deref(), Some("Quarterly review"));
        assert_eq!(pages.notes("asset://deck.pdf", 1).as_deref(), Some(""));
        assert_eq!(pages.notes("asset://deck.pdf", 2), None);

        let blue = pages.render("asset://deck.pdf", 1, 160, 90).unwrap();
        // A 4:3 page pillarboxed in 16:9: blue inside, transparent outside.
        assert_eq!(blue.pixel(80, 45), Some([0, 0, 255, 255]));
        assert_eq!(blue.pixel(2, 45).map(|pixel| pixel[3]), Some(0));
        let again = pages.render("asset://deck.pdf", 1, 160, 90).unwrap();
        assert!(Arc::ptr_eq(&blue, &again), "a repeated render is cached");
        let flat = page_over_black(&blue).unwrap();
        assert_eq!(&flat.pixels()[..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn missing_damaged_and_keynote_documents_report_why() {
        let root = bundle(&[("broken.pdf", b"%PDF-1.7 nothing".to_vec())]);
        let pages = DocumentPages::new(root.path(), None);
        let missing = pages.report("asset://gone.pdf");
        assert_eq!(
            (missing.count, missing.state),
            (None, DocumentState::Failed)
        );
        assert!(missing.message.is_some());
        let broken = pages.report("asset://broken.pdf");
        assert_eq!(broken.state, DocumentState::Failed);
        let keynote = pages.report("asset://talk.key");
        assert_eq!(keynote.state, DocumentState::Unavailable);
        assert!(keynote.message.unwrap().contains("PDF"));
        assert!(pages.render("asset://gone.pdf", 0, 16, 9).is_none());
    }

    #[test]
    fn a_presentation_without_libreoffice_still_counts_and_has_notes() {
        let slides = [
            SampleSlide {
                notes: Some("Welcome everyone."),
                hidden: false,
            },
            SampleSlide {
                notes: None,
                hidden: false,
            },
        ];
        let root = bundle(&[("talk.pptx", pptx(Some("Launch"), &slides))]);
        let pages = DocumentPages::new(root.path(), None);
        let report = pages.report("asset://talk.pptx");
        assert_eq!(report.count, Some(2));
        assert_eq!(report.state, DocumentState::Unavailable);
        assert_eq!(
            report.message.as_deref(),
            Some(fm_presentations::CONVERTER_UNAVAILABLE)
        );
        assert_eq!(
            pages.notes("asset://talk.pptx", 0).as_deref(),
            Some("Welcome everyone.")
        );
        assert!(pages.render("asset://talk.pptx", 0, 16, 9).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_presentation_converts_off_the_loop_with_a_converter() {
        use std::os::unix::fs::PermissionsExt;

        let slides = [SampleSlide {
            notes: None,
            hidden: false,
        }];
        let root = bundle(&[
            (
                "talk.odp",
                fm_presentations::sample::odp(Some("Talk"), &slides),
            ),
            ("converted.pdf", deck()),
        ]);
        // A stand-in converter that "converts" by copying a PDF into the
        // output directory it is given.
        let script = root.path().join("soffice");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nwhile [ \"$1\" != \"--outdir\" ]; do shift; done\ncp '{}' \"$2/presentation.pdf\"\n",
                root.path().join("converted.pdf").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let pages = DocumentPages::new(root.path(), Some(OfficeConverter::with_program(&script)));
        assert!(pages.take_changed());
        let first = pages.report("asset://talk.odp");
        assert_eq!(
            first.count,
            Some(1),
            "the slide count comes from the package"
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !pages.take_changed() {
            assert!(Instant::now() < deadline, "the conversion never finished");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pages.report("asset://talk.odp").state, DocumentState::Ready);
        assert!(pages.render("asset://talk.odp", 0, 64, 36).is_some());
    }

    #[test]
    fn page_requests_answer_notes_thumbnails_or_unavailable() {
        let root = bundle(&[("deck.pdf", deck())]);
        let pages = Arc::new(DocumentPages::new(root.path(), None));
        let request = |page, thumbnail| PagedPageRequest {
            request_id: format!("r{page}{}", u8::from(thumbnail)),
            input: fm_protocol::WireInputId::new(core::num::NonZeroU128::new(3).unwrap()),
            page,
            thumbnail,
        };
        let notes = page_answer(&pages, Some("asset://deck.pdf"), request(0, false));
        assert_eq!(notes.content, PagedPageContent::Notes(String::new()));
        let thumbnail = page_answer(&pages, Some("asset://deck.pdf"), request(1, true));
        let PagedPageContent::Thumbnail { width, height, .. } = &thumbnail.content else {
            panic!("expected a thumbnail, got {:?}", thumbnail.content);
        };
        assert_eq!((*width, *height), (128, 72));
        assert!(fm_protocol::encode_line(&fm_protocol::WireMessage::PagedPage(thumbnail)).is_ok());
        for (uri, page) in [(None, 0), (Some("asset://deck.pdf"), 9)] {
            assert_eq!(
                page_answer(&pages, uri, request(page, true)).content,
                PagedPageContent::Unavailable
            );
        }

        let worker = PageWorker::spawn(Arc::clone(&pages)).unwrap();
        worker.submit(7, Some("asset://deck.pdf".into()), request(0, true));
        worker.submit(7, Some("asset://deck.pdf".into()), request(1, false));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut answers = Vec::new();
        while answers.len() < 2 {
            assert!(Instant::now() < deadline, "the page worker never answered");
            answers.extend(worker.take_finished());
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(answers[0].0, 7);
        assert_eq!(answers[0].1.request_id, "r01");
        assert_eq!(answers[1].1.request_id, "r10");
    }
}
