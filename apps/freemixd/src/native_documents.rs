//! Native realization of document inputs (IN-014).
//!
//! Every document input of a native project is a live source frame: its
//! current page rendered by the pure-Rust PDF renderer (see
//! [`crate::documents`]) fitted and centred at the project's picture size,
//! transparent outside the page (so the compositor's black frames it, and
//! it keys cleanly as an overlay or a Scene layer). The input starts on a
//! blank picture like a network stream; the page arrives from the worker.
//!
//! Rendering never runs on the session loop: [`NativeDocumentSources`] owns
//! one worker thread. The loop hands it the document inputs and their pages
//! after every command ([`NativeDocumentSources::observe`], a value
//! comparison) and collects finished frames
//! ([`NativeDocumentSources::take_completed`]) for ingestion. Handoff is
//! latest-wins in both directions: a page stepped twice before the worker
//! sees it renders once, and a frame not yet ingested is replaced by a
//! newer one. A page that cannot render yet (a presentation still
//! converting) is tried again every [`RETRY_INTERVAL`] while it is wanted.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use fm_frame::{ClockDomainId, CpuVideoFrame};
use fm_model::Project;
use fm_types::{FrameRate, InputId};

use crate::documents::{DocumentPages, page_premultiplied};
use crate::native_files::timing_at;
use crate::native_titles::title_source_frame;

/// How often a page that did not render is tried again.
pub(crate) const RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// One document input's wanted page.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Wanted {
    uri: String,
    page: u32,
}

struct Work {
    pages: Arc<DocumentPages>,
    wanted: BTreeMap<InputId, Wanted>,
    size: (u32, u32),
    frame_rate: FrameRate,
}

#[derive(Default)]
struct Slot {
    work: Option<Work>,
    changed: bool,
    shutdown: bool,
    finished: BTreeMap<InputId, CpuVideoFrame>,
}

struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The page worker of a native daemon; see the module documentation.
pub(crate) struct NativeDocumentSources {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    observed: Option<BTreeMap<InputId, Wanted>>,
}

impl NativeDocumentSources {
    /// Starts the worker; frames carry timing in `clock_domain`.
    ///
    /// # Errors
    ///
    /// The operating system error when the thread cannot start.
    pub(crate) fn start(clock_domain: ClockDomainId) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::default()),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("freemixd-native-documents".into())
            .spawn(move || run_worker(&worker_shared, clock_domain))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            observed: None,
        })
    }

    /// Hands the worker every document input of `project` and its page,
    /// when they changed since the last call.
    pub(crate) fn observe(&mut self, project: &Project, pages: &Arc<DocumentPages>) {
        let wanted = project
            .documents()
            .map(|(input, document)| {
                (
                    input,
                    Wanted {
                        uri: document.asset_uri.clone(),
                        page: document.page,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if self.observed.as_ref() == Some(&wanted) {
            return;
        }
        self.observed = Some(wanted.clone());
        let dimensions = project.settings().video.dimensions;
        let mut slot = self.shared.lock();
        slot.work = Some(Work {
            pages: Arc::clone(pages),
            wanted,
            size: (dimensions.width(), dimensions.height()),
            frame_rate: project.settings().frame_rate,
        });
        slot.changed = true;
        self.shared.wake.notify_one();
    }

    /// The frames rendered since the last call, one per input (the newest).
    pub(crate) fn take_completed(&self) -> Vec<(InputId, CpuVideoFrame)> {
        std::mem::take(&mut self.shared.lock().finished)
            .into_iter()
            .collect()
    }
}

impl Drop for NativeDocumentSources {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(shared: &Shared, clock_domain: ClockDomainId) {
    // What each input last showed, and its next frame sequence (the blank
    // startup picture is sequence 0).
    let mut shown: BTreeMap<InputId, Wanted> = BTreeMap::new();
    let mut sequences: BTreeMap<InputId, u64> = BTreeMap::new();
    let mut pending = false;
    loop {
        let (pages, wanted, size, frame_rate) = {
            let mut slot = shared.lock();
            loop {
                if slot.shutdown {
                    return;
                }
                if slot.changed {
                    break;
                }
                if pending {
                    let (next, timeout) = shared
                        .wake
                        .wait_timeout(slot, RETRY_INTERVAL)
                        .unwrap_or_else(PoisonError::into_inner);
                    slot = next;
                    if timeout.timed_out() {
                        break;
                    }
                } else {
                    slot = shared
                        .wake
                        .wait(slot)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            slot.changed = false;
            let Some(work) = &slot.work else {
                continue;
            };
            (
                Arc::clone(&work.pages),
                work.wanted.clone(),
                work.size,
                work.frame_rate,
            )
        };
        shown.retain(|input, _| wanted.contains_key(input));
        sequences.retain(|input, _| wanted.contains_key(input));
        pending = false;
        for (input, page) in &wanted {
            if shown.get(input) == Some(page) {
                continue;
            }
            let sequence = sequences.entry(*input).or_insert(0);
            *sequence += 1;
            let frame = pages
                .render(&page.uri, page.page, size.0, size.1)
                .and_then(|rendered| page_premultiplied(&rendered))
                .and_then(|image| {
                    title_source_frame(&image, timing_at(*sequence, frame_rate, clock_domain)?)
                });
            let Some(frame) = frame else {
                pending = true;
                continue;
            };
            shown.insert(*input, page.clone());
            shared.lock().finished.insert(*input, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;
    use std::time::Instant;

    use fm_model::{DocumentInput, Input, InputKind, ProjectSettings};
    use fm_presentations::sample::{SolidPage, solid_pages_pdf};
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };

    use super::*;

    fn project(page: u32) -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(9).unwrap()),
            "Documents",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(64, 36).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::F32,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(Input {
            id: InputId::new(NonZeroU128::new(4).unwrap()),
            name: "Deck".into(),
            kind: InputKind::Document(DocumentInput {
                asset_uri: "asset://deck.pdf".into(),
                page,
            }),
            required_capabilities: Vec::new(),
        });
        project
    }

    fn wait(sources: &NativeDocumentSources) -> Vec<(InputId, CpuVideoFrame)> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let completed = sources.take_completed();
            if !completed.is_empty() {
                return completed;
            }
            assert!(Instant::now() < deadline, "no page rendered");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn centre(frame: &CpuVideoFrame) -> [u8; 4] {
        let plane = frame.payload().plane(0).unwrap();
        let offset = 18 * plane.stride() + 32 * 4;
        plane.bytes()[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn each_page_step_renders_one_straight_alpha_frame_off_the_loop() {
        let root = tempfile::tempdir().unwrap();
        let page = |rgb| SolidPage {
            width: 160,
            height: 90,
            rgb,
        };
        std::fs::write(
            root.path().join("deck.pdf"),
            solid_pages_pdf(None, &[page([255, 0, 0]), page([0, 0, 255])]),
        )
        .unwrap();
        let pages = Arc::new(DocumentPages::new(root.path(), None));
        let domain = ClockDomainId::new(NonZeroU128::new(1).unwrap());
        let mut sources = NativeDocumentSources::start(domain).unwrap();
        sources.observe(&project(0), &pages);
        let first = wait(&sources);
        assert_eq!(first.len(), 1);
        assert_eq!(centre(&first[0].1), [255, 0, 0, 255]);
        // An unchanged page renders nothing more.
        sources.observe(&project(0), &pages);
        thread::sleep(Duration::from_millis(50));
        assert!(sources.take_completed().is_empty());
        sources.observe(&project(1), &pages);
        let second = wait(&sources);
        assert_eq!(centre(&second[0].1), [0, 0, 255, 255]);
        assert!(
            second[0].1.timing().sequence() > first[0].1.timing().sequence(),
            "frames are timed in order"
        );
    }
}
