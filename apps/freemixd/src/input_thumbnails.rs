//! Live input bank thumbnails (Protocol 2.59).
//!
//! A raw Studio peer eligible for monitor frames subscribes with
//! `input_thumbnail_subscription` while its input bank is visible. While any
//! peer is subscribed, a dedicated worker thread renders one small picture
//! (fitted inside 160x90) of every input that has one, at most every
//! [`THUMBNAIL_INTERVAL`] per input, QOI-encodes it, and hands the encoded
//! `input_thumbnail` record to the session loop through a latest-wins slot
//! (one record per input). Nothing of this runs on the session loop or the
//! native render loop: the loop only publishes the latest work (a project
//! snapshot and, on a native daemon, the shared newest source frames) and
//! collects finished records.
//!
//! A picture whose encoded bytes did not change since its last record is not
//! sent again until [`THUMBNAIL_KEEPALIVE`] has passed, so a still source
//! costs almost nothing; every record carries how long ago its picture last
//! changed (`age_ms`), which is how a client tells a frozen live source from
//! a late record.
//!
//! What has a picture:
//!
//! - A native daemon thumbnails the newest CPU frame of every source its
//!   media pipeline holds (network streams, captures, media files, stills,
//!   titles, simulated generators), sampled nearest-neighbor straight to the
//!   thumbnail size and converted to sRGB, then graded with the input grade.
//! - A simulated daemon renders simulated inputs and Scene inputs through
//!   the CPU reference (as its monitor tap does), and synthesizes a moving
//!   test pattern for network stream and capture inputs, which have no real
//!   source there; those records say `origin=simulated`.
//! - System audio, media files on a simulated daemon, titles on a simulated
//!   daemon, and anything else without a picture get no thumbnail.
//!
//! Each subscribed peer drains its thumbnails through a [`ThumbnailOutbox`]:
//! one pending record per input (a newer one replaces an unsent one), served
//! round-robin, admitted by a per-peer [`TokenBucket`] of
//! [`THUMBNAIL_BYTES_PER_SECOND`], and moved into the peer's lossy thumbnail
//! slot only once the previous record was fully written, so a slow peer
//! holds at most one record per input and never stalls anything else.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_engine::CompiledProjectGrades;
use fm_model::{InputKind, Project};
use fm_protocol::{
    InputThumbnailMessage, MAX_INPUT_THUMBNAIL_BYTES, MAX_INPUT_THUMBNAIL_HEIGHT,
    MAX_INPUT_THUMBNAIL_WIDTH, MAX_LINE_BYTES, MonitorFrameOrigin, ServerIdentity, WireInputId,
    WireMessage, encode_line, encode_qoi_rgb,
};
use fm_sim::{ImageFrame, SimulatedPipeline};
use fm_titles::AnimSample;
use fm_types::InputId;

use crate::monitor_tap::fit_within;
use crate::scene_preview::LiveSourceFrames;
use crate::scene_render;

/// Shortest gap between two renders of one input (at most 2 fps).
pub(crate) const THUMBNAIL_INTERVAL: Duration = Duration::from_millis(500);
/// An unchanged picture is sent again after this long, so its age and the
/// client's freshness keep advancing.
pub(crate) const THUMBNAIL_KEEPALIVE: Duration = Duration::from_secs(2);
/// Sustained thumbnail bandwidth one peer may receive.
pub(crate) const THUMBNAIL_BYTES_PER_SECOND: u64 = 256 * 1024;
/// Burst one peer may receive at once: exactly one largest record.
pub(crate) const THUMBNAIL_BURST_BYTES: u64 = MAX_LINE_BYTES as u64;
/// The size a picture falls back to when its full-size encoding exceeds the
/// record bound; a 128x72 QOI stream always fits.
const FALLBACK_SIZE: (u16, u16) = (128, 72);
/// How often the session loop republishes work while subscribed, so the
/// worker sees new source frames and revisions.
const REPUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Longest the worker sleeps between steps while there is work.
const WORKER_STEP: Duration = Duration::from_millis(50);

/// What one worker step renders from.
pub(crate) struct ThumbnailWork {
    pub(crate) revision: u64,
    pub(crate) frame: u64,
    pub(crate) project: Arc<Project>,
    pub(crate) grades: Arc<CompiledProjectGrades>,
    /// The animation pose of every Scene input on air; others hold.
    pub(crate) scenes: Vec<(InputId, AnimSample)>,
    /// A native daemon's newest source frames; `None` on a simulated one.
    pub(crate) live: Option<LiveSourceFrames>,
}

/// One input's latest rendered thumbnail.
struct Entry {
    qoi: Vec<u8>,
    width: u16,
    height: u16,
    changed_at: Instant,
    rendered_at: Instant,
    emitted_at: Instant,
}

/// Renders and paces thumbnails; see the module documentation.
pub(crate) struct ThumbnailRenderer {
    server: ServerIdentity,
    /// The simulated pipeline at the fitted thumbnail size, per revision.
    pipeline: Option<(u64, Option<SimulatedPipeline>)>,
    entries: BTreeMap<InputId, Entry>,
    sequence: u64,
    epoch: Instant,
}

impl ThumbnailRenderer {
    pub(crate) fn new(server: ServerIdentity, epoch: Instant) -> Self {
        Self {
            server,
            pipeline: None,
            entries: BTreeMap::new(),
            sequence: 0,
            epoch,
        }
    }

    /// Renders every input due at `now` and hands each record to send to
    /// `emit`. `refresh` renders and sends every input with a picture at
    /// once (a peer just subscribed).
    pub(crate) fn step(
        &mut self,
        work: &ThumbnailWork,
        now: Instant,
        refresh: bool,
        emit: &mut dyn FnMut(InputId, Vec<u8>),
    ) {
        if work.live.is_none()
            && self
                .pipeline
                .as_ref()
                .is_none_or(|(revision, _)| *revision != work.revision)
        {
            let dimensions = work.project.settings().video.dimensions;
            let pipeline = fit_within(
                dimensions.width(),
                dimensions.height(),
                MAX_INPUT_THUMBNAIL_WIDTH,
                MAX_INPUT_THUMBNAIL_HEIGHT,
            )
            .and_then(|(width, height)| {
                scene_render::simulated_pipeline(&work.project, u32::from(width), u32::from(height))
            });
            self.pipeline = Some((work.revision, pipeline));
        }
        for input in work.project.inputs() {
            let due = refresh
                || self.entries.get(&input.id).is_none_or(|entry| {
                    now.saturating_duration_since(entry.rendered_at) >= THUMBNAIL_INTERVAL
                });
            if !due {
                continue;
            }
            let Some((width, height, qoi)) = self
                .picture(input.id, &input.kind, work, now)
                .and_then(|picture| encode_thumbnail(&picture))
            else {
                self.entries.remove(&input.id);
                continue;
            };
            let send = if let Some(entry) = self.entries.get_mut(&input.id) {
                let changed = entry.qoi != qoi || (entry.width, entry.height) != (width, height);
                if changed {
                    entry.qoi = qoi;
                    entry.width = width;
                    entry.height = height;
                    entry.changed_at = now;
                }
                entry.rendered_at = now;
                changed
                    || refresh
                    || now.saturating_duration_since(entry.emitted_at) >= THUMBNAIL_KEEPALIVE
            } else {
                self.entries.insert(
                    input.id,
                    Entry {
                        qoi,
                        width,
                        height,
                        changed_at: now,
                        rendered_at: now,
                        emitted_at: now,
                    },
                );
                true
            };
            if send && let Some(bytes) = self.record(input.id, now, work.live.is_some()) {
                emit(input.id, bytes);
            }
        }
        let project = &work.project;
        self.entries
            .retain(|input, _| project.inputs().iter().any(|known| known.id == *input));
    }

    /// Forgets every rendered picture (nobody is subscribed any more).
    pub(crate) fn idle(&mut self) {
        self.entries.clear();
        self.pipeline = None;
    }

    /// The encoded `input_thumbnail` line for `input`'s current entry.
    fn record(&mut self, input: InputId, now: Instant, native: bool) -> Option<Vec<u8>> {
        let entry = self.entries.get_mut(&input)?;
        let sequence = self.sequence.checked_add(1)?;
        let age = now.saturating_duration_since(entry.changed_at).as_millis();
        let message = InputThumbnailMessage {
            server: self.server.clone(),
            input: WireInputId::from_domain(input),
            sequence,
            origin: if native {
                MonitorFrameOrigin::Native
            } else {
                MonitorFrameOrigin::Simulated
            },
            age_ms: u32::try_from(age).unwrap_or(u32::MAX),
            width: entry.width,
            height: entry.height,
            data: entry.qoi.clone(),
        };
        let line = encode_line(&WireMessage::InputThumbnail(message)).ok()?;
        self.sequence = sequence;
        entry.emitted_at = now;
        Some(line.into_bytes())
    }

    /// The picture of one input, graded with its input grade, or `None`
    /// when it has none on this daemon.
    fn picture(
        &self,
        input: InputId,
        kind: &InputKind,
        work: &ThumbnailWork,
        now: Instant,
    ) -> Option<ImageFrame> {
        let picture = if let Some(live) = &work.live {
            live.thumbnail(input, MAX_INPUT_THUMBNAIL_WIDTH, MAX_INPUT_THUMBNAIL_HEIGHT)?
        } else {
            let pipeline = self.pipeline.as_ref()?.1.as_ref()?;
            match kind {
                InputKind::Simulated(_) => pipeline.render_source(input, work.frame).ok()?,
                InputKind::Scene { .. } => {
                    let pose = work
                        .scenes
                        .iter()
                        .find(|(scene, _)| *scene == input)
                        .map_or(AnimSample::Hold, |(_, pose)| *pose);
                    // The scene renderer applies the Scene input's own grade.
                    return scene_render::render_scene(
                        &work.project,
                        input,
                        pipeline,
                        work.frame,
                        pose,
                        &work.grades,
                    );
                }
                InputKind::NetworkStream(_) | InputKind::Device { .. } => synthetic_picture(
                    input,
                    pipeline.width(),
                    pipeline.height(),
                    now.saturating_duration_since(self.epoch),
                )?,
                _ => return None,
            }
        };
        match work.grades.input(input) {
            Some(grade) => fm_compositor::apply_grade(&picture, grade).ok(),
            None => Some(picture),
        }
    }
}

/// A simulated daemon's stand-in picture for a live source it cannot
/// receive: a tinted card (hue from the input id) with a bright band that
/// sweeps across it, so it visibly moves like a live feed.
fn synthetic_picture(
    input: InputId,
    width: u32,
    height: u32,
    elapsed: Duration,
) -> Option<ImageFrame> {
    const TINTS: [[u8; 3]; 6] = [
        [36, 72, 128],
        [32, 104, 84],
        [112, 64, 36],
        [92, 44, 108],
        [104, 92, 32],
        [40, 92, 112],
    ];
    let tint = TINTS[usize::try_from(input.get().get() % 6).ok()?];
    let columns = usize::try_from(width).ok()?;
    let rows = usize::try_from(height).ok()?;
    let band = (columns / 10).max(2);
    let travel = columns + band;
    let position = usize::try_from(elapsed.as_millis() / 25).ok()? % travel;
    let mut pixels = Vec::with_capacity(columns * rows * 4);
    for y in 0..rows {
        for x in 0..columns {
            let lit = x + band >= position && x < position;
            // A darker lower third stands in for a caption strip.
            let shade = if y * 3 >= rows * 2 { 2 } else { 1 };
            let [red, green, blue] = if lit {
                [230, 230, 230]
            } else {
                tint.map(|channel| channel / shade)
            };
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    ImageFrame::new(width, height, columns * 4, pixels).ok()
}

/// QOI-encodes a picture fitted inside the thumbnail bounds, falling back
/// to [`FALLBACK_SIZE`] when the full-size stream exceeds
/// [`MAX_INPUT_THUMBNAIL_BYTES`].
pub(crate) fn encode_thumbnail(picture: &ImageFrame) -> Option<(u16, u16, Vec<u8>)> {
    let encode = |image: &ImageFrame| {
        let width = u16::try_from(image.width()).ok()?;
        let height = u16::try_from(image.height()).ok()?;
        let qoi = encode_qoi_rgb(width, height, image.pixels(), image.stride(), 4).ok()?;
        Some((width, height, qoi))
    };
    let fitted = |max: (u16, u16)| {
        let (width, height) = fit_within(picture.width(), picture.height(), max.0, max.1)?;
        if (u32::from(width), u32::from(height)) == (picture.width(), picture.height()) {
            return encode(picture);
        }
        encode(&fm_video::scale_nearest(picture, u32::from(width), u32::from(height)).ok()?)
    };
    let full = fitted((MAX_INPUT_THUMBNAIL_WIDTH, MAX_INPUT_THUMBNAIL_HEIGHT))?;
    if full.2.len() <= MAX_INPUT_THUMBNAIL_BYTES {
        return Some(full);
    }
    fitted(FALLBACK_SIZE).filter(|(_, _, qoi)| qoi.len() <= MAX_INPUT_THUMBNAIL_BYTES)
}

/// A per-peer byte budget: refills at `rate` bytes per second up to
/// `capacity`, and admits a record only while the whole record fits.
#[derive(Debug)]
pub(crate) struct TokenBucket {
    rate: u64,
    capacity: u64,
    tokens: u64,
    refilled_at: Instant,
}

impl TokenBucket {
    /// A full bucket.
    pub(crate) const fn new(rate: u64, capacity: u64, now: Instant) -> Self {
        Self {
            rate,
            capacity,
            tokens: capacity,
            refilled_at: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let earned =
            u64::try_from(elapsed.as_micros().saturating_mul(u128::from(self.rate)) / 1_000_000)
                .unwrap_or(u64::MAX);
        if earned > 0 {
            self.tokens = self.tokens.saturating_add(earned).min(self.capacity);
            self.refilled_at = now;
        }
    }

    /// Takes `bytes` when they fit the current budget.
    pub(crate) fn try_take(&mut self, bytes: u64, now: Instant) -> bool {
        self.refill(now);
        if bytes > self.tokens {
            return false;
        }
        self.tokens -= bytes;
        true
    }
}

/// One subscribed peer's pending thumbnails; see the module documentation.
#[derive(Debug)]
pub(crate) struct ThumbnailOutbox {
    pending: BTreeMap<InputId, Vec<u8>>,
    /// The input served last, so the next one is served after it.
    cursor: Option<InputId>,
    bucket: TokenBucket,
}

impl ThumbnailOutbox {
    pub(crate) const fn new(now: Instant) -> Self {
        Self {
            pending: BTreeMap::new(),
            cursor: None,
            bucket: TokenBucket::new(THUMBNAIL_BYTES_PER_SECOND, THUMBNAIL_BURST_BYTES, now),
        }
    }

    /// Queues `input`'s newest record, replacing an unsent older one.
    pub(crate) fn offer(&mut self, input: InputId, bytes: Vec<u8>) {
        self.pending.insert(input, bytes);
    }

    /// Records waiting, at most one per input.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    /// The next record to write, round-robin after the last one served, if
    /// the peer's byte budget admits it now. A record the budget refuses
    /// stays pending (and may still be replaced by a newer one).
    pub(crate) fn next(&mut self, now: Instant) -> Option<Vec<u8>> {
        let input = self
            .cursor
            .and_then(|cursor| {
                self.pending
                    .range((
                        std::ops::Bound::Excluded(cursor),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .map(|(input, _)| *input)
            })
            .or_else(|| self.pending.keys().next().copied())?;
        let size = u64::try_from(self.pending.get(&input)?.len()).unwrap_or(u64::MAX);
        if !self.bucket.try_take(size, now) {
            return None;
        }
        self.cursor = Some(input);
        self.pending.remove(&input)
    }
}

/// The latest work published to the worker and its finished records.
struct Slot {
    work: Option<Arc<ThumbnailWork>>,
    changed: bool,
    refresh: bool,
    shutdown: bool,
    /// One latest-wins record per input.
    finished: BTreeMap<InputId, Vec<u8>>,
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

/// Runs a [`ThumbnailRenderer`] on its own thread; the session loop only
/// publishes work and collects finished records, never waiting for a
/// render.
pub(crate) struct ThumbnailWorker {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    published_at: Option<Instant>,
    project: Option<(u64, Arc<Project>)>,
}

impl ThumbnailWorker {
    /// Starts the worker rendering records that carry `server`'s identity.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub(crate) fn spawn(server: ServerIdentity) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                work: None,
                changed: false,
                refresh: false,
                shutdown: false,
                finished: BTreeMap::new(),
            }),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let mut renderer = ThumbnailRenderer::new(server, Instant::now());
        let worker = thread::Builder::new()
            .name("freemixd-thumbnails".to_owned())
            .spawn(move || run_worker(&worker_shared, &mut renderer))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            published_at: None,
            project: None,
        })
    }

    /// One project copy per revision, shared with the worker.
    pub(crate) fn project_at(&mut self, revision: u64, project: &Project) -> Arc<Project> {
        match &self.project {
            Some((cached, shared)) if *cached == revision => Arc::clone(shared),
            _ => {
                let shared = Arc::new(project.clone());
                self.project = Some((revision, Arc::clone(&shared)));
                shared
            }
        }
    }

    /// Publishes work while `demand` lasts (built by `build` at most every
    /// [`REPUBLISH_INTERVAL`], or at once on `refresh`), stops the worker
    /// when it ends, and returns every record finished since the last call.
    pub(crate) fn poll(
        &mut self,
        demand: bool,
        refresh: bool,
        now: Instant,
        build: impl FnOnce(&mut Self) -> ThumbnailWork,
    ) -> Vec<(InputId, Vec<u8>)> {
        let publish = if demand {
            let due = refresh
                || self.published_at.is_none_or(|published| {
                    now.saturating_duration_since(published) >= REPUBLISH_INTERVAL
                });
            due.then(|| {
                self.published_at = Some(now);
                Some(Arc::new(build(self)))
            })
        } else if self.published_at.take().is_some() {
            self.project = None;
            Some(None)
        } else {
            None
        };
        let mut slot = self.shared.lock();
        if let Some(work) = publish {
            slot.work = work;
            slot.changed = true;
            slot.refresh |= refresh;
            self.shared.wake.notify_one();
        }
        if !demand {
            slot.finished.clear();
        }
        std::mem::take(&mut slot.finished).into_iter().collect()
    }
}

impl Drop for ThumbnailWorker {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

fn run_worker(shared: &Shared, renderer: &mut ThumbnailRenderer) {
    let mut work: Option<Arc<ThumbnailWork>> = None;
    loop {
        let refresh;
        {
            let mut slot = shared.lock();
            if !slot.changed && !slot.shutdown {
                slot = if work.is_some() {
                    shared
                        .wake
                        .wait_timeout(slot, WORKER_STEP)
                        .map_or_else(|error| error.into_inner().0, |(slot, _)| slot)
                } else {
                    shared
                        .wake
                        .wait(slot)
                        .unwrap_or_else(PoisonError::into_inner)
                };
            }
            if slot.shutdown {
                return;
            }
            if slot.changed {
                work.clone_from(&slot.work);
                slot.changed = false;
            }
            refresh = std::mem::take(&mut slot.refresh);
        }
        let Some(current) = &work else {
            renderer.idle();
            continue;
        };
        renderer.step(current, Instant::now(), refresh, &mut |input, bytes| {
            // Visible to the session loop as soon as it is encoded.
            shared.lock().finished.insert(input, bytes);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_model::{
        Input, ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
    };
    use fm_protocol::{LineDecoder, decode_input_thumbnail};
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };

    use super::*;

    fn input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn server() -> ServerIdentity {
        ServerIdentity {
            engine_id: "engine".to_owned(),
            project_id: "42".to_owned(),
            state_epoch: 1,
            log_id: "log".to_owned(),
        }
    }

    fn project(inputs: Vec<Input>) -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Thumbnails",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(1920, 1080).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::I16,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        for input in inputs {
            project.add_input(input);
        }
        project
    }

    fn input(id: u128, kind: InputKind) -> Input {
        Input {
            id: input_id(id),
            name: format!("Input {id}"),
            kind,
            required_capabilities: Vec::new(),
        }
    }

    fn solid(id: u128, red: u8) -> Input {
        input(
            id,
            InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(red, 0, 0, 255)),
                SimulatedAudio::Silence,
            )),
        )
    }

    fn work(project: Project, revision: u64) -> ThumbnailWork {
        ThumbnailWork {
            revision,
            frame: 0,
            project: Arc::new(project),
            grades: Arc::default(),
            scenes: Vec::new(),
            live: None,
        }
    }

    fn step(
        renderer: &mut ThumbnailRenderer,
        work: &ThumbnailWork,
        now: Instant,
        refresh: bool,
    ) -> Vec<(InputId, InputThumbnailMessage)> {
        let mut out = Vec::new();
        renderer.step(work, now, refresh, &mut |input, bytes| {
            let mut decoder = LineDecoder::new();
            let messages = decoder.push(&bytes).unwrap();
            let [WireMessage::InputThumbnail(message)] = messages.as_slice() else {
                panic!("one input_thumbnail record");
            };
            out.push((input, message.clone()));
        });
        out
    }

    #[test]
    fn renders_fitted_thumbnails_paced_and_kept_alive() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start);
        let work = work(project(vec![solid(1, 200), solid(2, 50)]), 1);

        let first = step(&mut renderer, &work, start, false);
        assert_eq!(first.len(), 2);
        let image = decode_input_thumbnail(&first[0].1).unwrap();
        assert_eq!((image.width, image.height), (160, 90));
        assert_eq!(image.origin, MonitorFrameOrigin::Simulated);
        assert_eq!(&image.rgba[..4], &[200, 0, 0, 255]);
        assert!(first[1].1.sequence > first[0].1.sequence);

        // Within the interval nothing renders; an unchanged picture is not
        // re-sent until the keepalive passes, then carries its age.
        assert_eq!(
            step(
                &mut renderer,
                &work,
                start + Duration::from_millis(100),
                false
            ),
            Vec::new()
        );
        assert_eq!(
            step(
                &mut renderer,
                &work,
                start + Duration::from_millis(600),
                false
            ),
            Vec::new()
        );
        let kept = step(
            &mut renderer,
            &work,
            start + Duration::from_millis(2_100),
            false,
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].1.age_ms, 2_100);

        // A refresh sends every picture at once.
        let refreshed = step(
            &mut renderer,
            &work,
            start + Duration::from_millis(2_200),
            true,
        );
        assert_eq!(refreshed.len(), 2);
    }

    #[test]
    fn a_changed_picture_is_sent_at_the_next_interval_with_age_zero() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start);
        step(
            &mut renderer,
            &work(project(vec![solid(1, 200)]), 1),
            start,
            false,
        );
        let changed = work(project(vec![solid(1, 20)]), 2);
        assert_eq!(
            step(
                &mut renderer,
                &changed,
                start + Duration::from_millis(200),
                false
            ),
            Vec::new()
        );
        let sent = step(
            &mut renderer,
            &changed,
            start + Duration::from_millis(500),
            false,
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.age_ms, 0);
        let image = decode_input_thumbnail(&sent[0].1).unwrap();
        assert_eq!(&image.rgba[..4], &[20, 0, 0, 255]);
    }

    #[test]
    fn simulated_live_sources_get_a_moving_synthetic_picture_and_audio_none() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start);
        let stream = input(
            3,
            InputKind::NetworkStream(fm_types::network_stream::NetworkStreamConfig::new(
                "rtsp://camera.example/live",
            )),
        );
        let audio = input(
            4,
            InputKind::SystemAudio(fm_types::system_audio::SystemAudioConfig::new(
                fm_types::system_audio::SystemAudioBackend::Pulse,
                "default.monitor",
            )),
        );
        let work = work(project(vec![stream, audio]), 1);
        let first = step(&mut renderer, &work, start, false);
        assert_eq!(
            first.iter().map(|(input, _)| *input).collect::<Vec<_>>(),
            [input_id(3)]
        );
        let later = step(
            &mut renderer,
            &work,
            start + Duration::from_millis(500),
            false,
        );
        assert_eq!(later.len(), 1, "the synthetic picture moves");
        assert_ne!(later[0].1.data, first[0].1.data);
        assert_eq!(later[0].1.age_ms, 0);
    }

    #[test]
    fn oversized_pictures_fall_back_to_the_smaller_size() {
        // Noise defeats QOI, so the 160x90 stream exceeds the record bound.
        let mut state = 0x1234_5678_u32;
        let mut pixels = Vec::new();
        for _ in 0..160 * 90 {
            for _ in 0..3 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                pixels.push(state.to_le_bytes()[0]);
            }
            pixels.push(255);
        }
        let noise = ImageFrame::new(160, 90, 160 * 4, pixels).unwrap();
        let (width, height, qoi) = encode_thumbnail(&noise).unwrap();
        assert_eq!((width, height), FALLBACK_SIZE);
        assert!(qoi.len() <= MAX_INPUT_THUMBNAIL_BYTES);
        let record = InputThumbnailMessage {
            server: server(),
            input: WireInputId::from_domain(input_id(1)),
            sequence: 1,
            origin: MonitorFrameOrigin::Native,
            age_ms: u32::MAX,
            width,
            height,
            data: qoi,
        };
        assert!(encode_line(&WireMessage::InputThumbnail(record)).is_ok());
    }

    #[test]
    fn token_bucket_admits_whole_records_at_the_sustained_rate() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(1_000, 1_500, start);
        assert!(bucket.try_take(1_000, start));
        assert!(!bucket.try_take(1_000, start), "only 500 bytes are left");
        assert!(!bucket.try_take(1_000, start + Duration::from_millis(400)));
        assert!(bucket.try_take(1_000, start + Duration::from_millis(500)));
        // Refill never exceeds the burst capacity.
        assert!(!bucket.try_take(1_600, start + Duration::from_secs(60)));
        assert!(bucket.try_take(1_500, start + Duration::from_secs(60)));
    }

    #[test]
    fn outbox_keeps_one_latest_record_per_input_and_serves_round_robin() {
        let start = Instant::now();
        let mut outbox = ThumbnailOutbox::new(start);
        outbox.offer(input_id(2), b"two-old".to_vec());
        outbox.offer(input_id(1), b"one".to_vec());
        outbox.offer(input_id(2), b"two-new".to_vec());
        outbox.offer(input_id(3), b"three".to_vec());
        assert_eq!(outbox.len(), 3, "a newer record replaces an unsent one");
        assert_eq!(outbox.next(start).unwrap(), b"one");
        // Input 1 offers again before 2 and 3 are served; they still go first.
        outbox.offer(input_id(1), b"one-again".to_vec());
        assert_eq!(outbox.next(start).unwrap(), b"two-new");
        assert_eq!(outbox.next(start).unwrap(), b"three");
        assert_eq!(outbox.next(start).unwrap(), b"one-again");
        assert!(outbox.next(start).is_none());
    }

    #[test]
    fn outbox_backpressure_holds_records_until_the_budget_refills() {
        let start = Instant::now();
        let mut outbox = ThumbnailOutbox::new(start);
        let record = vec![0_u8; 40 * 1024];
        for id in 1..=8 {
            outbox.offer(input_id(id), record.clone());
        }
        // The burst admits one 40 KiB record; the rest wait for refill.
        assert!(outbox.next(start).is_some());
        assert!(outbox.next(start).is_none());
        assert_eq!(outbox.len(), 7);
        // Over one second at most the sustained rate (plus the burst) leaves.
        let mut sent = 0_u64;
        for millis in (0..=1_000).step_by(10) {
            while let Some(bytes) = outbox.next(start + Duration::from_millis(millis)) {
                sent += bytes.len() as u64;
            }
        }
        assert!(sent <= THUMBNAIL_BYTES_PER_SECOND, "{sent}");
        assert!(sent >= THUMBNAIL_BYTES_PER_SECOND - 2 * 40 * 1024, "{sent}");
    }

    #[test]
    fn worker_renders_off_the_loop_and_stops_without_demand() {
        let mut worker = ThumbnailWorker::spawn(server()).unwrap();
        let project = project(vec![solid(1, 200)]);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut records = Vec::new();
        while records.is_empty() && Instant::now() < deadline {
            records = worker.poll(true, false, Instant::now(), |worker| ThumbnailWork {
                revision: 1,
                frame: 0,
                project: worker.project_at(1, &project),
                grades: Arc::default(),
                scenes: Vec::new(),
                live: None,
            });
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, input_id(1));
        // Without demand nothing is collected any more.
        assert_eq!(
            worker.poll(false, false, Instant::now(), |_| unreachable!()),
            Vec::new()
        );
    }
}
