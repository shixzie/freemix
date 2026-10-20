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
//!   thumbnail size and converted to sRGB, then graded with the input grade
//!   and shown over black (as Program shows a source with transparency).
//!   Its Scene inputs are composed at the thumbnail size by the CPU
//!   reference compositor from those same sampled frames, as an exact scene
//!   preview composes them.
//! - A simulated daemon renders simulated inputs, title inputs, and Scene
//!   inputs through the CPU reference (as its monitor tap does), and
//!   synthesizes a moving test pattern for network stream and capture
//!   inputs and a card with the playhead's position for media inputs, which
//!   have no real source there; those records say `origin=simulated`. Its
//!   Scene inputs compose from those same pictures (a remote's synthetic
//!   card included), so a scene with a live layer still has a thumbnail.
//! - An audio-only media input (a music bed or sting whose file name says
//!   audio, [`fm_model::is_audio_file_name`]; IN-005) gets an audio card on
//!   both daemons: a waveform glyph whose played part lights up with the
//!   playhead, over the same progress bar a simulated media card draws.
//!   Its own picture is transparent black, which would be a black tile.
//! - System audio, and anything else without a picture, gets no thumbnail.
//!   An input without a picture is tried again at most every
//!   [`THUMBNAIL_INTERVAL`], like one with a picture.
//!
//! Each subscribed peer drains its thumbnails through a [`ThumbnailOutbox`]:
//! one pending record per input (a newer one replaces an unsent one), served
//! round-robin, admitted by a per-peer [`TokenBucket`] of
//! [`THUMBNAIL_BYTES_PER_SECOND`], and moved into the peer's lossy thumbnail
//! slot only once the previous record was fully written, so a slow peer
//! holds at most one record per input and never stalls anything else.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_engine::CompiledProjectGrades;
use fm_model::{Input, InputKind, Project};
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
use crate::scene_render::{self, SceneSources};
use crate::stills::{SlidesShown, StillPictures};
use crate::title_render::{self, TitleAssets, TitleTime};

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
    /// The animation pose of every title on air; others hold.
    pub(crate) titles: Vec<(InputId, AnimSample)>,
    /// Each media input's playhead and the end of its playable range, as
    /// the engine holds them (a simulated daemon draws them).
    pub(crate) media: Vec<(InputId, u64, u64)>,
    /// What every slideshow input shows (a simulated daemon draws it).
    pub(crate) slideshows: Vec<(InputId, SlidesShown)>,
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
    /// The title templates, fonts, and images title inputs draw with.
    titles: Arc<TitleAssets>,
    /// The bundle's documents, whose current pages document inputs show
    /// (IN-014).
    documents: Arc<crate::documents::DocumentPages>,
    /// The simulated pipeline at the fitted thumbnail size, per revision.
    pipeline: Option<(u64, Option<SimulatedPipeline>)>,
    entries: BTreeMap<InputId, Entry>,
    /// Decoded image and slide pictures at the thumbnail size.
    stills: StillPictures,
    /// When each input was last tried, with or without a picture, so an
    /// input without one is not tried again on every worker step.
    tried: BTreeMap<InputId, Instant>,
    sequence: u64,
    epoch: Instant,
    /// Pictures attempted, observed by tests to prove pacing.
    #[cfg(test)]
    attempts: u64,
}

impl ThumbnailRenderer {
    pub(crate) fn new(server: ServerIdentity, epoch: Instant, titles: Arc<TitleAssets>) -> Self {
        Self {
            server,
            titles,
            documents: Arc::default(),
            pipeline: None,
            entries: BTreeMap::new(),
            stills: StillPictures::default(),
            tried: BTreeMap::new(),
            sequence: 0,
            epoch,
            #[cfg(test)]
            attempts: 0,
        }
    }

    /// This renderer, drawing document inputs from `documents` (IN-014).
    pub(crate) fn with_documents(self, documents: Arc<crate::documents::DocumentPages>) -> Self {
        Self { documents, ..self }
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
        // Scene inputs compose on this grid on either daemon.
        if self
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
                || self.tried.get(&input.id).is_none_or(|tried| {
                    now.saturating_duration_since(*tried) >= THUMBNAIL_INTERVAL
                });
            if !due {
                continue;
            }
            self.tried.insert(input.id, now);
            #[cfg(test)]
            {
                self.attempts += 1;
            }
            let Some((width, height, qoi)) = self
                .picture(input, work, now)
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
        let known = |input: &InputId| project.inputs().iter().any(|known| known.id == *input);
        self.entries.retain(|input, _| known(input));
        self.tried.retain(|input, _| known(input));
    }

    /// Forgets every rendered picture (nobody is subscribed any more).
    pub(crate) fn idle(&mut self) {
        self.entries.clear();
        self.tried.clear();
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

    /// The picture of one input, graded with its input grade and shown over
    /// black (straight colour, opaque), or `None` when it has none on this
    /// daemon.
    #[allow(clippy::too_many_lines)] // one arm per input kind
    fn picture(&mut self, input: &Input, work: &ThumbnailWork, now: Instant) -> Option<ImageFrame> {
        let id = input.id;
        let pipeline = self
            .pipeline
            .as_ref()
            .and_then(|(_, pipeline)| pipeline.as_ref());
        if let InputKind::Scene { scene_id, .. } = &input.kind {
            let pipeline = pipeline?;
            let scene = work
                .project
                .scenes()
                .iter()
                .find(|scene| scene.id == *scene_id)?;
            let simulated;
            let live;
            let sources: &dyn SceneSources = match &work.live {
                None => {
                    simulated = SimulatedLeaves {
                        leaf: LeafContext {
                            pipeline,
                            titles: &self.titles,
                            documents: &self.documents,
                            work,
                            elapsed: now.saturating_duration_since(self.epoch),
                        },
                        stills: RefCell::new(&mut self.stills),
                    };
                    &simulated
                }
                Some(frames) => {
                    live = SampledSources {
                        live: frames,
                        simulated: pipeline,
                    };
                    &live
                }
            };
            // The scene renderer applies the Scene input's own grade; its
            // premultiplied colour is the picture over black.
            return scene_render::render_scene_model(
                &work.project,
                scene,
                sources,
                (pipeline.width(), pipeline.height()),
                work.frame,
                pose_of(&work.scenes, id),
                Some(&work.grades),
                Some(id),
            )
            .map(|composed| opaque(&composed));
        }
        if let InputKind::Media { asset_uri, .. } = &input.kind
            && fm_model::is_audio_file_name(asset_uri)
        {
            // An audio-only file has no picture: its card stands in, ungraded.
            let (width, height) = pipeline.map_or(
                (
                    u32::from(MAX_INPUT_THUMBNAIL_WIDTH),
                    u32::from(MAX_INPUT_THUMBNAIL_HEIGHT),
                ),
                |pipeline| (pipeline.width(), pipeline.height()),
            );
            return audio_media_picture(width, height, media_playhead(work, id));
        }
        let picture = if let Some(live) = &work.live {
            live.thumbnail(id, MAX_INPUT_THUMBNAIL_WIDTH, MAX_INPUT_THUMBNAIL_HEIGHT)?
        } else {
            let leaf = LeafContext {
                pipeline: pipeline?,
                titles: &self.titles,
                documents: &self.documents,
                work,
                elapsed: now.saturating_duration_since(self.epoch),
            };
            leaf.picture(input, work.frame, &mut self.stills, false)?
        };
        let graded = match work.grades.input(id) {
            Some(grade) => fm_compositor::apply_grade(&picture, grade).ok()?,
            None => picture,
        };
        Some(over_black(&graded))
    }
}

/// What a simulated daemon draws an input with: the pipeline at the fitted
/// thumbnail size, the title assets, and the worker's work.
struct LeafContext<'a> {
    pipeline: &'a SimulatedPipeline,
    titles: &'a TitleAssets,
    /// The bundle's document pages (IN-014).
    documents: &'a crate::documents::DocumentPages,
    work: &'a ThumbnailWork,
    /// How long the renderer has run, which moves the synthetic live cards.
    elapsed: Duration,
}

impl LeafContext<'_> {
    /// Whether [`Self::picture`] draws `input` on a simulated daemon.
    fn draws(input: &Input) -> bool {
        matches!(
            input.kind,
            InputKind::Simulated(_)
                | InputKind::Title(_)
                | InputKind::NetworkStream(_)
                | InputKind::Device { .. }
                | InputKind::Image(_)
                | InputKind::Slideshow(_)
                | InputKind::Document(_)
                | InputKind::Media { .. }
        )
    }

    /// The ungraded straight-alpha picture of non-scene `input` at
    /// `frame`, at the pipeline size: a simulated generator's own picture, a
    /// title in its pose, a still or the slides shown, and the synthetic
    /// cards a network stream, capture, or media input stands in with.
    /// `in_scene` draws it as a scene layer reads it: a title keeps its
    /// transparency (on its own it is shown over black), and an audio-only
    /// file is transparent (on its own its audio card stands in instead).
    fn picture(
        &self,
        input: &Input,
        frame: u64,
        stills: &mut StillPictures,
        in_scene: bool,
    ) -> Option<ImageFrame> {
        let (id, work) = (input.id, self.work);
        let (width, height) = (self.pipeline.width(), self.pipeline.height());
        match &input.kind {
            InputKind::Simulated(_) => self.pipeline.render_source(id, frame).ok(),
            InputKind::Title(title) => {
                // As the monitor tap draws a title: its pose on air (or
                // Hold), keyed.
                let keyed = title_render::render_title_at(
                    self.titles,
                    id,
                    title,
                    TitleTime::now(
                        frame,
                        work.project.settings().frame_rate,
                        None,
                        pose_of(&work.titles, id),
                    ),
                    width,
                    height,
                )?;
                if in_scene {
                    straight(&keyed)
                } else {
                    title_render::flatten_over_black(&keyed)
                }
            }
            InputKind::NetworkStream(_) | InputKind::Device { .. } => {
                synthetic_picture(id, width, height, self.elapsed)
            }
            InputKind::Image(image) => stills.image(
                self.titles.store()?,
                &image.asset_uri,
                image.fit,
                (width, height),
            ),
            InputKind::Slideshow(_) => {
                let shown = work
                    .slideshows
                    .iter()
                    .find(|(slideshow, _)| *slideshow == id)
                    .map(|(_, shown)| shown)?;
                stills.slides(self.titles.store()?, shown, (width, height))
            }
            // A document shows its current page, opaque, as it does on
            // Program (IN-014).
            InputKind::Document(document) => {
                let page =
                    self.documents
                        .render(&document.asset_uri, document.page, width, height)?;
                crate::documents::page_over_black(&page)
            }
            InputKind::Media { asset_uri, .. } if fm_model::is_audio_file_name(asset_uri) => {
                let bytes = usize::try_from(width).ok()?.checked_mul(4)?;
                let rows = usize::try_from(height).ok()?;
                ImageFrame::new(width, height, bytes, vec![0; bytes.checked_mul(rows)?]).ok()
            }
            InputKind::Media { .. } => {
                synthetic_media_picture(id, width, height, media_playhead(work, id))
            }
            _ => None,
        }
    }
}

/// A simulated daemon's Scene thumbnail leaves: every layer input drawn as
/// its own thumbnail draws it, the synthetic cards of live sources and
/// media files included, so a scene of cameras and a remote shows its
/// layout instead of no picture at all (its record says
/// `origin=simulated`).
struct SimulatedLeaves<'a> {
    leaf: LeafContext<'a>,
    stills: RefCell<&'a mut StillPictures>,
}

impl SceneSources for SimulatedLeaves<'_> {
    fn draws(&self, input: &Input) -> bool {
        LeafContext::draws(input)
    }

    fn leaf(
        &self,
        input: &Input,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Option<ImageFrame> {
        if (self.leaf.pipeline.width(), self.leaf.pipeline.height()) != (width, height) {
            return None;
        }
        let mut stills = self.stills.try_borrow_mut().ok()?;
        self.leaf.picture(input, frame_number, &mut stills, true)
    }
}

/// A premultiplied picture as straight alpha.
fn straight(picture: &ImageFrame) -> Option<ImageFrame> {
    let mut pixels = picture.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = fm_title_render::unpremultiply(*pixel);
    }
    ImageFrame::new(picture.width(), picture.height(), picture.stride(), pixels).ok()
}

/// Media input `input`'s playhead and the end of its playable range.
fn media_playhead(work: &ThumbnailWork, input: InputId) -> Option<(u64, u64)> {
    work.media
        .iter()
        .find(|(media, _, _)| *media == input)
        .map(|(_, position, end)| (*position, *end))
}

/// The animation pose of `input` in `poses`, or Hold.
fn pose_of(poses: &[(InputId, AnimSample)], input: InputId) -> AnimSample {
    poses
        .iter()
        .find(|(candidate, _)| *candidate == input)
        .map_or(AnimSample::Hold, |(_, pose)| *pose)
}

/// A native daemon's Scene thumbnail leaves: each source's newest frame
/// sampled straight onto the grid (never copied whole), and the simulated
/// pipeline for a simulated input the snapshot lacks.
struct SampledSources<'a> {
    live: &'a LiveSourceFrames,
    simulated: &'a SimulatedPipeline,
}

impl SceneSources for SampledSources<'_> {
    fn draws(&self, input: &Input) -> bool {
        self.live.holds(input.id) || self.simulated.draws(input)
    }

    fn leaf(
        &self,
        input: &Input,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Option<ImageFrame> {
        if self.live.holds(input.id) {
            self.live.sampled(input.id, width, height)
        } else {
            self.simulated.leaf(input, frame_number, width, height)
        }
    }
}

/// A premultiplied picture as the opaque picture it shows over black.
fn opaque(picture: &ImageFrame) -> ImageFrame {
    let mut pixels = picture.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = u8::MAX;
    }
    ImageFrame::new(picture.width(), picture.height(), picture.stride(), pixels)
        .unwrap_or_else(|_| picture.clone())
}

/// A straight-alpha picture composited over black: each colour channel
/// scaled by its alpha (rounded), alpha opaque. A thumbnail carries no
/// alpha, so a source with transparency (a title, a still with a
/// transparent background) shows as Program shows it alone, not as the
/// undefined straight colour under its transparent pixels.
pub(crate) fn over_black(picture: &ImageFrame) -> ImageFrame {
    let mut pixels = picture.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let scaled = (u16::from(*channel) * alpha + 127) / 255;
            *channel = u8::try_from(scaled).unwrap_or(u8::MAX);
        }
        pixel[3] = u8::MAX;
    }
    ImageFrame::new(picture.width(), picture.height(), picture.stride(), pixels)
        .unwrap_or_else(|_| picture.clone())
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

/// A simulated daemon's stand-in picture for a media input it cannot
/// decode: a dark tinted card with a progress bar along the bottom showing
/// the playhead within the playable range (empty when unknown), so it moves
/// exactly while the engine plays the clip.
fn synthetic_media_picture(
    input: InputId,
    width: u32,
    height: u32,
    playhead: Option<(u64, u64)>,
) -> Option<ImageFrame> {
    let shift = u8::try_from(input.get().get() % 5).ok()? * 6;
    let tint = [52_u8, 40, 72].map(|channel| channel.saturating_add(shift));
    let columns = usize::try_from(width).ok()?;
    let rows = usize::try_from(height).ok()?;
    let filled = played_columns(width, playhead);
    let bar = (rows / 12).max(2);
    let mut pixels = Vec::with_capacity(columns * rows * 4);
    for y in 0..rows {
        for x in 0..columns {
            let [red, green, blue] = if y + bar >= rows {
                if x < filled {
                    [220, 180, 60]
                } else {
                    [24, 24, 28]
                }
            } else {
                tint
            };
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    ImageFrame::new(width, height, columns * 4, pixels).ok()
}

/// The columns of `width` the playhead has passed within the playable
/// range (none when unknown).
fn played_columns(width: u32, playhead: Option<(u64, u64)>) -> usize {
    playhead.map_or(0, |(position, end)| {
        if end == 0 {
            return 0;
        }
        let fraction = u128::from(position.min(end)) * u128::from(width) / u128::from(end);
        usize::try_from(fraction).unwrap_or(usize::MAX)
    })
}

/// An audio-only media input's card (IN-005): a dark teal card with a
/// symmetric waveform glyph across its middle, lit up to the playhead and
/// dim after it, and a progress bar along the bottom, so it reads as audio
/// and moves exactly while the engine plays the file.
fn audio_media_picture(
    width: u32,
    height: u32,
    playhead: Option<(u64, u64)>,
) -> Option<ImageFrame> {
    const CARD: [u8; 3] = [20, 44, 48];
    const PLAYED: [u8; 3] = [120, 220, 200];
    const UNPLAYED: [u8; 3] = [56, 96, 100];
    const TRACK: [u8; 3] = [24, 24, 28];
    const BAR: [u8; 3] = [220, 180, 60];
    let columns = usize::try_from(width).ok()?;
    let rows = usize::try_from(height).ok()?;
    let filled = played_columns(width, playhead);
    let bar = (rows / 12).max(2);
    let middle = rows.saturating_sub(bar) / 2;
    let reach = rows.saturating_sub(bar) * 3 / 8;
    // Glyph bars: two lit columns, one gap, with a deterministic envelope.
    let pitch = (columns / 40).max(3);
    let margin = columns / 10;
    let mut pixels = Vec::with_capacity(columns * rows * 4);
    for y in 0..rows {
        for x in 0..columns {
            let [red, green, blue] = if y + bar >= rows {
                if x < filled { BAR } else { TRACK }
            } else if x >= margin && x + margin < columns && (x - margin) % pitch + 1 < pitch {
                let stroke = (x - margin) / pitch;
                // A smooth swell with a little variation, 25..100 % of reach.
                let shape = [4_usize, 7, 10, 8, 12, 9, 6, 11, 5, 8];
                let swell = 12 - (stroke % 24).abs_diff(12) / 2;
                let level = (shape[stroke % shape.len()] + swell).clamp(4, 16);
                let half = reach * level / 16;
                if y.abs_diff(middle) <= half {
                    if x < filled { PLAYED } else { UNPLAYED }
                } else {
                    CARD
                }
            } else {
                CARD
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
    pub(crate) fn spawn(
        server: ServerIdentity,
        titles: Arc<TitleAssets>,
        documents: Arc<crate::documents::DocumentPages>,
    ) -> std::io::Result<Self> {
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
        let mut renderer =
            ThumbnailRenderer::new(server, Instant::now(), titles).with_documents(documents);
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
            titles: Vec::new(),
            media: Vec::new(),
            slideshows: Vec::new(),
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
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
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
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
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
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
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
        let mut worker = ThumbnailWorker::spawn(server(), Arc::default(), Arc::default()).unwrap();
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
                titles: Vec::new(),
                media: Vec::new(),
                slideshows: Vec::new(),
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

    /// An input without a picture (system audio here) is tried at most once
    /// per interval, not on every worker step.
    #[test]
    fn pictureless_inputs_are_tried_once_per_interval() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
        let audio = input(
            4,
            InputKind::SystemAudio(fm_types::system_audio::SystemAudioConfig::new(
                fm_types::system_audio::SystemAudioBackend::Pulse,
                "default.monitor",
            )),
        );
        let work = work(project(vec![audio]), 1);
        for millis in (0..500).step_by(50) {
            let sent = step(
                &mut renderer,
                &work,
                start + Duration::from_millis(millis),
                false,
            );
            assert_eq!(sent, Vec::new());
        }
        assert_eq!(renderer.attempts, 1);
        step(
            &mut renderer,
            &work,
            start + Duration::from_millis(500),
            false,
        );
        assert_eq!(renderer.attempts, 2);
    }

    /// A thumbnail has no alpha: a straight-alpha picture shows over black,
    /// as Program shows it, never as the colour under transparent pixels.
    #[test]
    fn straight_alpha_pictures_show_over_black() {
        let picture = ImageFrame::new(
            3,
            1,
            12,
            vec![255, 255, 255, 0, 200, 100, 50, 128, 9, 8, 7, 255],
        )
        .unwrap();
        assert_eq!(
            over_black(&picture).pixels(),
            &[0, 0, 0, 255, 100, 50, 25, 255, 9, 8, 7, 255]
        );
    }

    const TITLE_URI: &str = "asset://titles/bar.fmtitle";

    /// A 1920x1080 transparent title template with one rectangle at
    /// (300, 150) sized 600x300 in `colour`.
    fn title_assets(colour: [u8; 3]) -> Arc<TitleAssets> {
        use fm_titles::{
            Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
            Style, TemplateId, TitleTemplate,
        };
        let field = FieldId::new(NonZeroU128::new(1).unwrap());
        let template = TitleTemplate {
            id: TemplateId::new(NonZeroU128::new(1).unwrap()),
            name: "Bar".into(),
            width: 1_920,
            height: 1_080,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field,
                name: "Bar".into(),
                default: FieldValue::Color(Color::new(colour[0], colour[1], colour[2], 255)),
            }],
            elements: vec![Element {
                id: ElementId::new(NonZeroU128::new(1).unwrap()),
                name: "Bar".into(),
                bounds: Bounds {
                    x: 300,
                    y: 150,
                    width: 600,
                    height: 300,
                },
                z_index: 0,
                visible: true,
                style: Style::default(),
                color_field: Some(field),
                kind: ElementKind::Rectangle,
            }],
            animation: fm_titles::TitleAnimation::default(),
        };
        let mut assets = TitleAssets::default();
        assets.insert(
            TITLE_URI,
            title_render::LoadedTitleTemplate::new(template, fm_titles::AssetCatalog::new()),
        );
        Arc::new(assets)
    }

    fn pixel(message: &InputThumbnailMessage, x: usize, y: usize) -> [u8; 4] {
        let image = decode_input_thumbnail(message).unwrap();
        let offset = (y * usize::from(image.width) + x) * 4;
        image.rgba[offset..offset + 4].try_into().unwrap()
    }

    /// A simulated daemon draws a title input as its monitor tap does: the
    /// template over black at the thumbnail size.
    #[test]
    fn simulated_titles_get_their_rendered_picture() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start, title_assets([200, 40, 20]));
        let title = input(
            5,
            InputKind::Title(fm_model::TitleInput {
                template_uri: TITLE_URI.into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
        );
        let sent = step(&mut renderer, &work(project(vec![title]), 1), start, false);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.origin, MonitorFrameOrigin::Simulated);
        assert_eq!((sent[0].1.width, sent[0].1.height), (160, 90));
        // The bar covers (25..75, 12..38) of the 160x90 grid.
        assert_eq!(pixel(&sent[0].1, 50, 25), [200, 40, 20, 255]);
        assert_eq!(pixel(&sent[0].1, 5, 5), [0, 0, 0, 255]);
        assert_eq!(pixel(&sent[0].1, 120, 70), [0, 0, 0, 255]);
    }

    /// A simulated daemon composes a Scene input's thumbnail from what each
    /// layer input's own thumbnail draws, so a scene with a network stream
    /// layer (which only has a synthetic card there) still has a picture: a
    /// solid on the left half, the stream's card on the right half, and a
    /// title on top whose transparent parts show the layers under it.
    #[test]
    fn simulated_scene_inputs_compose_their_layers_synthetic_cards_included() {
        use fm_model::{Layer, LayerGeometry, Rotation, SourceRef};
        let layer = |id: u32, source: u128, geometry: LayerGeometry, z_order: i32| Layer {
            id: fm_model::LayerId::new(id).unwrap(),
            name: format!("Layer {id}"),
            source: SourceRef::Input(input_id(source)),
            enabled: true,
            geometry,
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: 255,
            z_order,
        };
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start, title_assets([200, 40, 20]));
        let stream = input(
            3,
            InputKind::NetworkStream(fm_types::network_stream::NetworkStreamConfig::new(
                "rtsp://camera.example/live",
            )),
        );
        let title = input(
            5,
            InputKind::Title(fm_model::TitleInput {
                template_uri: TITLE_URI.into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
        );
        let mut project = project(vec![solid(1, 200), stream, title]);
        let scene = fm_types::SceneId::new(NonZeroU128::new(1).unwrap());
        project.add_scene(fm_model::Scene {
            id: scene,
            name: "Two-shot + remote".into(),
            background: fm_model::Rgba8::new(60, 60, 60, 255),
            layers: vec![
                layer(1, 1, LayerGeometry::new(0, 0, 960, 1080, Rotation::Deg0), 1),
                layer(
                    2,
                    3,
                    LayerGeometry::new(960, 0, 960, 1080, Rotation::Deg0),
                    1,
                ),
                layer(
                    3,
                    5,
                    LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
                    2,
                ),
            ],
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_input(input(
            7,
            InputKind::Scene {
                scene_id: scene,
                audio_source: None,
            },
        ));
        let sent = step(&mut renderer, &work(project, 1), start, false);
        let scene = sent
            .iter()
            .find(|(input, _)| *input == input_id(7))
            .map(|(_, message)| message)
            .expect("the scene has a thumbnail");
        assert_eq!(scene.origin, MonitorFrameOrigin::Simulated);
        assert_eq!((scene.width, scene.height), (160, 90));
        // The title's bar covers (25..75, 12..38); outside it the title is
        // transparent and the solid shows.
        assert_eq!(pixel(scene, 50, 25), [200, 40, 20, 255]);
        assert_eq!(pixel(scene, 10, 80), [200, 0, 0, 255]);
        // The right half is the stream's card, not the scene's background.
        let card = pixel(scene, 120, 60);
        assert_ne!(card, [60, 60, 60, 255]);
        assert_ne!(card, [0, 0, 0, 255]);
        assert_eq!(card[3], 255);
    }

    /// A simulated daemon draws a media input as a card whose bottom bar
    /// follows the engine's playhead.
    #[test]
    fn simulated_media_cards_follow_the_playhead() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
        let clip = input(
            6,
            InputKind::Media {
                asset_uri: "asset://media/clip.mov".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
        );
        let mut paused = work(project(vec![clip]), 1);
        paused.media = vec![(input_id(6), 0, 100)];
        let first = step(&mut renderer, &paused, start, false);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1.origin, MonitorFrameOrigin::Simulated);
        assert_eq!(pixel(&first[0].1, 10, 89), [24, 24, 28, 255]);
        // Still paused: nothing changes, so nothing is sent before the
        // keepalive.
        let unchanged = step(
            &mut renderer,
            &paused,
            start + Duration::from_millis(500),
            false,
        );
        assert_eq!(unchanged, Vec::new());
        let mut halfway = paused;
        halfway.media = vec![(input_id(6), 50, 100)];
        let moved = step(
            &mut renderer,
            &halfway,
            start + Duration::from_millis(1_000),
            false,
        );
        assert_eq!(moved.len(), 1);
        assert_eq!(pixel(&moved[0].1, 10, 89), [220, 180, 60, 255]);
        assert_eq!(pixel(&moved[0].1, 100, 89), [24, 24, 28, 255]);
    }

    /// An audio-only media input (IN-005) gets an audio card: a waveform
    /// glyph lit up to the playhead, over the progress bar.
    #[test]
    fn audio_only_media_inputs_get_an_audio_card() {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(server(), start, Arc::default());
        let bed = input(
            7,
            InputKind::Media {
                asset_uri: "asset://media/bed.wav".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
        );
        let mut halfway = work(project(vec![bed]), 1);
        halfway.media = vec![(input_id(7), 50, 100)];
        let sent = step(&mut renderer, &halfway, start, false);
        assert_eq!(sent.len(), 1);
        let card = &sent[0].1;
        assert_eq!((card.width, card.height), (160, 90));
        // A glyph stroke before the playhead is lit, one after it dim, the
        // gap between strokes is the card, and the bar shows the playhead.
        assert_eq!(pixel(card, 17, 41), [120, 220, 200, 255]);
        assert_eq!(pixel(card, 141, 41), [56, 96, 100, 255]);
        assert_eq!(pixel(card, 19, 41), [20, 44, 48, 255]);
        assert_eq!(pixel(card, 10, 89), [220, 180, 60, 255]);
        assert_eq!(pixel(card, 100, 89), [24, 24, 28, 255]);
    }
}

/// A native daemon's thumbnails come from the media pipeline's newest
/// source frames.
#[cfg(all(test, feature = "native-media"))]
mod native_tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU128;

    use fm_frame::{
        AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
        CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
        NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
        SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
    };
    use fm_model::{Layer, LayerGeometry, Rotation, SourceRef};
    use fm_protocol::{LineDecoder, decode_input_thumbnail};

    use super::*;

    fn input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    /// One labeled straight-alpha sRGB 320x180 BGRA frame, every pixel
    /// `bgra`.
    fn frame(bgra: [u8; 4]) -> Arc<CpuVideoFrame> {
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(0),
                TimeBase::new(1, 1_000_000_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(0),
            NormalizedDuration::from_nanos(40_000_000).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            SequenceNumber::new(0),
        )
        .unwrap();
        let payload = CpuVideoPayload::new(
            PixelFormat::Bgra8,
            VideoDimensions::new(320, 180).unwrap(),
            vec![CpuVideoPlane::new(320 * 4, bgra.repeat(320 * 180)).unwrap()],
        )
        .unwrap();
        Arc::new(
            CpuVideoFrame::new(timing, payload)
                .with_metadata(VideoFrameMetadata::new(
                    ColorMetadata {
                        primaries: ColorPrimaries::Bt709,
                        transfer: TransferFunction::Srgb,
                        matrix: MatrixCoefficients::Identity,
                        range: SignalRange::Full,
                        chroma_location: ChromaLocation::Center,
                    },
                    Some(AlphaMode::Straight),
                ))
                .unwrap(),
        )
    }

    /// A 1920x1080 project: input 2 a media clip, input 3 a title, input 4
    /// an audio-only music bed, and Scene input 11 showing scene 1 (the clip
    /// over the left half on an opaque grey background).
    fn project() -> Project {
        let frame_rate = fm_types::FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            fm_types::ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Native thumbnails",
            fm_model::ProjectSettings {
                frame_rate,
                video: fm_types::VideoFormat {
                    dimensions: fm_types::VideoDimensions::new(1920, 1080).unwrap(),
                    frame_rate,
                    pixel_format: fm_types::PixelFormat::Rgba8,
                    scan: fm_types::ScanMode::Progressive,
                    color: fm_types::ColorMetadata::default(),
                },
                audio: fm_types::AudioFormat {
                    sample_rate: fm_types::SampleRate::new(48_000).unwrap(),
                    sample_format: fm_types::SampleFormat::I16,
                    channels: fm_types::ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(Input {
            id: input_id(2),
            name: "Clip".into(),
            kind: InputKind::Media {
                asset_uri: "asset://media/clip.mov".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: input_id(3),
            name: "Lower third".into(),
            kind: InputKind::Title(fm_model::TitleInput {
                template_uri: "asset://titles/lower-third.fmtitle".into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: input_id(4),
            name: "Bed".into(),
            kind: InputKind::Media {
                asset_uri: "asset://media/bed.mp3".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
        let scene = fm_types::SceneId::new(NonZeroU128::new(1).unwrap());
        project.add_scene(fm_model::Scene {
            id: scene,
            name: "Scene".into(),
            background: fm_model::Rgba8::new(60, 60, 60, 255),
            layers: vec![Layer {
                id: fm_model::LayerId::new(1).unwrap(),
                name: "Clip".into(),
                source: SourceRef::Input(input_id(2)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 960, 1080, Rotation::Deg0),
                crop: None,
                mask: None,
                key: None,
                effects: Vec::new(),
                opacity: 255,
                z_order: 1,
            }],
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_input(Input {
            id: input_id(11),
            name: "Scene".into(),
            kind: InputKind::Scene {
                scene_id: scene,
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
        project
    }

    fn thumbnails(
        frames: BTreeMap<InputId, Arc<CpuVideoFrame>>,
    ) -> BTreeMap<InputId, fm_protocol::InputThumbnailImage> {
        let start = Instant::now();
        let mut renderer = ThumbnailRenderer::new(
            ServerIdentity {
                engine_id: "engine".to_owned(),
                project_id: "42".to_owned(),
                state_epoch: 1,
                log_id: "log".to_owned(),
            },
            start,
            Arc::default(),
        );
        let work = ThumbnailWork {
            revision: 1,
            frame: 0,
            project: Arc::new(project()),
            grades: Arc::default(),
            scenes: Vec::new(),
            titles: Vec::new(),
            media: Vec::new(),
            slideshows: Vec::new(),
            live: Some(LiveSourceFrames::from_frames(frames)),
        };
        let mut out = BTreeMap::new();
        renderer.step(&work, start, false, &mut |input, bytes| {
            let messages = LineDecoder::new().push(&bytes).unwrap();
            let [WireMessage::InputThumbnail(message)] = messages.as_slice() else {
                panic!("one input_thumbnail record");
            };
            assert_eq!(message.origin, MonitorFrameOrigin::Native);
            out.insert(input, decode_input_thumbnail(message).unwrap());
        });
        out
    }

    fn pixel(image: &fm_protocol::InputThumbnailImage, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * usize::from(image.width) + x) * 4;
        image.rgba[offset..offset + 4].try_into().unwrap()
    }

    /// A native title (or a still with transparency) shows over black: its
    /// transparent pixels are black whatever straight colour they carry, and
    /// a half-transparent pixel is its colour scaled by its alpha.
    #[test]
    fn native_transparent_sources_show_over_black() {
        let mut frames = BTreeMap::new();
        // BGRA: white, fully transparent.
        frames.insert(input_id(3), frame([255, 255, 255, 0]));
        // BGRA: (r 200, g 100, b 50) at alpha 128.
        frames.insert(input_id(2), frame([50, 100, 200, 128]));
        let images = thumbnails(frames);
        assert_eq!(pixel(&images[&input_id(3)], 10, 10), [0, 0, 0, 255]);
        assert_eq!(pixel(&images[&input_id(2)], 10, 10), [100, 50, 25, 255]);
    }

    /// A native daemon thumbnails its Scene inputs: the scene composed at
    /// the thumbnail size from the sources' newest frames.
    #[test]
    fn native_scene_inputs_compose_from_live_frames() {
        let mut frames = BTreeMap::new();
        // BGRA: opaque (r 10, g 200, b 30).
        frames.insert(input_id(2), frame([30, 200, 10, 255]));
        let images = thumbnails(frames);
        let scene = &images[&input_id(11)];
        assert_eq!((scene.width, scene.height), (160, 90));
        assert_eq!(pixel(scene, 10, 45), [10, 200, 30, 255]);
        assert_eq!(pixel(scene, 150, 45), [60, 60, 60, 255]);

        // Without the clip's frame the scene cannot be composed honestly.
        assert!(!thumbnails(BTreeMap::new()).contains_key(&input_id(11)));
    }

    /// A native audio-only media input's transparent picture is not its
    /// thumbnail: its audio card is (IN-005).
    #[test]
    fn native_audio_only_media_inputs_get_an_audio_card() {
        let mut frames = BTreeMap::new();
        frames.insert(input_id(4), frame([0, 0, 0, 0]));
        let images = thumbnails(frames);
        let card = &images[&input_id(4)];
        assert_eq!((card.width, card.height), (160, 90));
        assert_eq!(pixel(card, 17, 41), [56, 96, 100, 255]);
        assert_eq!(pixel(card, 19, 41), [20, 44, 48, 255]);
        // Even before the file presents its picture.
        assert!(thumbnails(BTreeMap::new()).contains_key(&input_id(4)));
    }
}
