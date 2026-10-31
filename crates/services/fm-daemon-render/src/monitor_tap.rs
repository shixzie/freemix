//! Encoded-fallback Preview/Program monitor taps (Protocol 2.21).
//!
//! Monitor frames are bounded (at most 640x360) QOI images carried as lossy,
//! latest-wins `monitor_frame` part groups. Every part line of one frame is
//! concatenated into a single buffer so the session writer emits the whole
//! group atomically: once started it finishes before any other record.
//!
//! The simulated daemon has no frame clock, so [`SimulatedMonitorTap`] renders
//! the current realized Preview and Program inputs on a wall-clock cadence.
//! Simulated inputs render directly at the fitted monitor size. Scene inputs
//! whose enabled layers all name simulated inputs are composed with the CPU
//! reference compositor directly at the fitted monitor size (see
//! [`scene_render::render_scene`]); each composition is cached per realized
//! revision, frame cursor, and input so the steady cadence never recomposes
//! an unchanged picture. A scene is composed in the animation pose the
//! engine reports for its input at the frame cursor (GX-011 stage 5); a
//! simulated command realizes every scene phase it starts, so between
//! commands a simulated scene holds. Title inputs render their persisted field values
//! through the `fm_titles` reference renderer over black (see
//! [`title_render`]) in the animation pose the engine reports for the frame
//! cursor, cached per title state and pose and, only for clock or ticker
//! templates, frame cursor. A simulated title take realizes its whole In
//! phase within the command, so the engine reports the Hold pose right
//! after it; the tap replays an In phase it saw start over the following
//! wall time at the show's rate (see [`SimulatedMonitorTap::replay_titles`]),
//! so the monitor shows the title animating on as the native output does.
//! An off-air realizes its whole Out phase within the command and leaves no
//! trace of it afterwards (a plain overlay off looks the same), so a
//! simulated title still leaves the monitor at once.
//!
//! The Program tap then composites every realized active overlay over the
//! Program picture with its opacity, position, and border preset, using the
//! native overlay geometry ([`overlay_transform`]) evaluated at the monitor
//! size. Overlays are drawn whatever their output inclusion: the tap shows
//! the program bus, not one output. An overlay source may be a simulated,
//! scene, or title input; any other source is skipped.
//!
//! Any other realized input (or a scene or title the CPU reference cannot
//! render honestly) produces no frame, so a client keeps its placeholder
//! instead of seeing invented video.
//!
//! Grades (SW-019) apply as on the native path: a simulated input and a
//! title input carry their input grade (a title overlay is graded on its
//! straight colour, its alpha unchanged), each scene layer carries its
//! source's input grade and then its own layer grade, and the program grade
//! colours the composed Program picture after its overlays. A Scene input's
//! own input grade colours the composed scene it shows (after its layers'
//! grades), wherever that input shows: Program, Preview, an overlay, or a
//! layer of another scene. Media inputs have no simulated picture, so
//! there is nothing of theirs to grade.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_auth::Policy;
use fm_compositor::{
    AlphaMode, CpuSourceFrame, OutputTarget, Rotation, Scene, SourceId, SourceLayer, Transform,
    compile_scene, execute_cpu,
};
use fm_control::ControlService;
use fm_engine::CompiledProjectGrades;
use fm_model::{InputKind, Project, SimulatedVideo};
use fm_overlay_layout::{overlay_border_width, overlay_transform};
use fm_protocol::{
    MAX_MONITOR_FRAME_HEIGHT, MAX_MONITOR_FRAME_WIDTH, MonitorFrameMessage, MonitorFrameOrigin,
    MonitorTap, OverlayBorderPreset as WireOverlayBorder,
    OverlayPositionPreset as WireOverlayPosition, OverlayStatus, ServerIdentity, WireInputId,
    WireMessage, encode_line, encode_qoi_rgb, monitor_frame_parts,
};
use fm_sim::{ImageFrame, Rgba8, SimulatedPipeline, SimulatedSource, SourcePattern};
use fm_switcher::{OverlayBorderPreset, OverlayPositionPreset};
use fm_titles::{AnimSample, TitleAir};
use fm_types::{InputId, VideoDimensions};

use crate::grading::GradeCache;
use crate::scene_render;
use crate::stills::{SlidesShown, StillPictures};
use crate::title_render::{self, TitleAssets, TitleKey};
use fm_daemon_journal::documents::DocumentPages;

/// The order taps render within one cadence step: the cheap, latency-visible
/// Preview first, so it is never queued behind a Program Scene composition.
const MONITOR_TAP_ORDER: [MonitorTap; 2] = [MonitorTap::Preview, MonitorTap::Program];

/// Steady simulated monitor cadence (5 fps).
pub const SIMULATED_MONITOR_INTERVAL: Duration = Duration::from_millis(200);
/// Smallest gap between two simulated frames, even after a state change.
pub const SIMULATED_MONITOR_MIN_GAP: Duration = Duration::from_millis(50);

/// Scales `width`x`height` down (never up) to fit `max_width`x`max_height`
/// while preserving aspect ratio. The scaled side is floored and every side
/// is at least one pixel. Returns `None` for an empty source.
pub fn fit_within(width: u32, height: u32, max_width: u16, max_height: u16) -> Option<(u16, u16)> {
    if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
        return None;
    }
    let (width, height) = (u64::from(width), u64::from(height));
    let (max_width, max_height) = (u64::from(max_width), u64::from(max_height));
    let (fitted_width, fitted_height) = if width <= max_width && height <= max_height {
        (width, height)
    } else if width * max_height >= height * max_width {
        (max_width, (height * max_width / width).max(1))
    } else {
        ((width * max_height / height).max(1), max_height)
    };
    Some((
        u16::try_from(fitted_width).ok()?,
        u16::try_from(fitted_height).ok()?,
    ))
}

/// Fits project dimensions to the protocol's monitor frame cap.
pub fn fit_monitor(width: u32, height: u32) -> Option<(u16, u16)> {
    fit_within(
        width,
        height,
        MAX_MONITOR_FRAME_WIDTH,
        MAX_MONITOR_FRAME_HEIGHT,
    )
}

/// Encodes every part of one frame into one contiguous byte buffer of wire
/// lines. Returns `None` when there are no parts or any part fails to encode,
/// so a partially encodable group is never published.
pub fn encode_group(messages: Vec<MonitorFrameMessage>) -> Option<Vec<u8>> {
    if messages.is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    for message in messages {
        let line = encode_line(&WireMessage::MonitorFrame(message)).ok()?;
        bytes.extend_from_slice(line.as_bytes());
    }
    Some(bytes)
}

/// One frame's pixel source and identity, ready to encode.
pub struct MonitorPicture<'a> {
    pub tap: MonitorTap,
    pub sequence: u64,
    pub frame: u64,
    pub input: InputId,
    pub origin: MonitorFrameOrigin,
    pub width: u16,
    pub height: u16,
    pub pixels: &'a [u8],
    pub stride: usize,
    pub bytes_per_pixel: usize,
}

/// QOI-encodes one picture and returns its atomic `monitor_frame` group.
pub fn encode_picture(server: &ServerIdentity, picture: &MonitorPicture<'_>) -> Option<Vec<u8>> {
    let qoi = encode_qoi_rgb(
        picture.width,
        picture.height,
        picture.pixels,
        picture.stride,
        picture.bytes_per_pixel,
    )
    .ok()?;
    encode_group(monitor_frame_parts(
        server,
        picture.tap,
        picture.sequence,
        picture.frame,
        WireInputId::from_domain(picture.input),
        picture.origin,
        picture.width,
        picture.height,
        &qoi,
    ))
}

/// One realized active overlay the Program tap composites, in channel order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealizedOverlay {
    pub source: InputId,
    pub opacity: u8,
    pub position: OverlayPositionPreset,
    pub border: OverlayBorderPreset,
}

impl RealizedOverlay {
    /// The overlay a realized channel status shows, or `None` when the
    /// channel is inactive or has no source.
    fn from_status(status: &OverlayStatus) -> Option<Self> {
        if !status.active {
            return None;
        }
        Some(Self {
            source: status.source?.to_domain(),
            opacity: status.opacity,
            position: match status.position {
                WireOverlayPosition::FullFrame => OverlayPositionPreset::FullFrame,
                WireOverlayPosition::TopLeft => OverlayPositionPreset::TopLeft,
                WireOverlayPosition::TopRight => OverlayPositionPreset::TopRight,
                WireOverlayPosition::BottomLeft => OverlayPositionPreset::BottomLeft,
                WireOverlayPosition::BottomRight => OverlayPositionPreset::BottomRight,
            },
            border: match status.border {
                WireOverlayBorder::None => OverlayBorderPreset::None,
                WireOverlayBorder::ThinWhite => OverlayBorderPreset::ThinWhite,
                WireOverlayBorder::ThickWhite => OverlayBorderPreset::ThickWhite,
            },
        })
    }
}

/// The realized state a simulated monitor frame depicts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RealizedMonitorState {
    pub revision: u64,
    pub frame: u64,
    pub program: InputId,
    pub preview: InputId,
    /// Every realized active overlay, in channel order (lowest channel
    /// composites first, so the highest channel is on top).
    pub overlays: Vec<RealizedOverlay>,
    /// The animation pose of every title a title take put on air, or that
    /// plays its In or Out phase on Program or Preview (GX-001), ascending
    /// by input; any other title renders its Hold pose.
    pub titles: Vec<(InputId, AnimSample)>,
    /// The frame each title input shown on Program, an overlay channel, or
    /// Preview started its timeline on, ascending by input: the engine's
    /// own dating, so tickers and clocks count from the take exactly as
    /// journal replay reproduces it (GX-001).
    pub shown: Vec<(InputId, u64)>,
    /// The animation pose of every Scene input mid or after an In or Out
    /// phase on Program or Preview (GX-011 stage 5), ascending by input;
    /// any other scene renders its Hold pose.
    pub scenes: Vec<(InputId, AnimSample)>,
    /// The In or Out phase each of those titles is in (where it started and
    /// how long it plays), ascending by input. The simulated engine realizes
    /// a whole phase within the take or off-air command, so `titles` already
    /// holds; the tap replays a phase it saw start over the following wall
    /// time (see [`SimulatedMonitorTap::replay_titles`]).
    pub airs: Vec<(InputId, TitleAir)>,
    /// What every slideshow input shows, ascending by input (IN-009).
    pub slideshows: Vec<(InputId, SlidesShown)>,
}

/// The realized fields whose change publishes a frame immediately.
type ChangeKey = (
    u64,
    InputId,
    InputId,
    Vec<RealizedOverlay>,
    Vec<(InputId, AnimSample)>,
    Vec<(InputId, AnimSample)>,
    Vec<(InputId, SlidesShown)>,
);

impl RealizedMonitorState {
    pub fn from_control(control: &ControlService<Policy>) -> Self {
        let snapshot = &control.snapshot().snapshot;
        Self {
            revision: snapshot.revision,
            frame: control.frame_cursor(),
            program: snapshot.realized_program.to_domain(),
            preview: snapshot.realized_preview.to_domain(),
            overlays: snapshot
                .realized_overlays
                .iter()
                .filter_map(RealizedOverlay::from_status)
                .collect(),
            titles: title_poses(control),
            shown: control
                .titles_shown()
                .into_iter()
                .map(|title| (title.input, title.timeline.since_frame))
                .collect(),
            scenes: control
                .scenes_on_air()
                .into_iter()
                .map(|scene| (scene.input, scene.frame.sample))
                .collect(),
            airs: control
                .titles_on_air()
                .into_iter()
                .map(|title| (title.input, title.air))
                .collect(),
            slideshows: SlidesShown::all(control),
        }
    }

    /// The animation pose `input` renders in.
    fn pose(&self, input: InputId) -> AnimSample {
        self.titles
            .iter()
            .find(|(title, _)| *title == input)
            .map_or(AnimSample::Hold, |(_, pose)| *pose)
    }

    /// The frame `input`'s timeline started on, while it is shown.
    fn taken_frame(&self, input: InputId) -> Option<u64> {
        self.shown
            .iter()
            .find(|(title, _)| *title == input)
            .map(|(_, since)| *since)
    }

    fn scene_pose(&self, input: InputId) -> AnimSample {
        self.scenes
            .iter()
            .find(|(scene, _)| *scene == input)
            .map_or(AnimSample::Hold, |(_, pose)| *pose)
    }

    const fn input(&self, tap: MonitorTap) -> InputId {
        match tap {
            MonitorTap::Preview => self.preview,
            MonitorTap::Program => self.program,
        }
    }

    /// The fields whose change publishes a frame immediately; the frame
    /// cursor alone only advances on the steady cadence.
    fn change_key(&self) -> ChangeKey {
        (
            self.revision,
            self.program,
            self.preview,
            self.overlays.clone(),
            self.titles.clone(),
            self.scenes.clone(),
            self.slideshows.clone(),
        )
    }
}

/// Identity of one composed scene picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ComposeKey {
    revision: u64,
    frame: u64,
    input: InputId,
}

/// One scene composition; `image` is `None` when the scene could not be
/// rendered, so a failing scene is not retried every poll.
struct ComposedScene {
    key: ComposeKey,
    image: Option<Arc<ImageFrame>>,
}

/// One title input's most recent render at the fitted monitor size.
struct RenderedTitle {
    key: TitleKey,
    /// Premultiplied, keeping the template's alpha (the overlay source).
    keyed: Option<Arc<ImageFrame>>,
    /// The same picture over opaque black (the Preview/Program picture).
    flat: Option<Arc<ImageFrame>>,
}

/// Identity of one Program picture with overlays composited.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ProgramKey {
    revision: u64,
    frame: u64,
    program: InputId,
    overlays: Vec<RealizedOverlay>,
    /// The title state of Program and of each overlay source.
    titles: Vec<Option<TitleKey>>,
}

/// A picture to layer over Program and how its alpha is stored.
struct OverlaySource {
    image: Arc<ImageFrame>,
    alpha: AlphaMode,
    /// The input grade the compositor applies (a keyed title's), or `None`
    /// when `image` is already graded.
    grade: Option<Arc<fm_compositor::CompiledGrade>>,
}

/// Renders realized Preview and Program from simulated inputs on a bounded
/// wall-clock cadence while at least one eligible peer wants frames.
#[derive(Default)]
pub struct SimulatedMonitorTap {
    /// Every simulated input at the fitted monitor size; scenes compose
    /// from these same sources.
    pipeline: Option<SimulatedPipeline>,
    built_revision: Option<u64>,
    /// Title templates and fonts loaded at startup.
    titles: Arc<TitleAssets>,
    /// The bundle's documents, whose current pages document inputs show
    /// (IN-014).
    documents: Arc<DocumentPages>,
    /// The per-input and program grades of the depicted revision.
    grades: Arc<CompiledProjectGrades>,
    /// Scene compositions used by the most recent cadence step.
    composed: Vec<ComposedScene>,
    /// The latest render of each title input, keyed by its full title state.
    rendered_titles: BTreeMap<InputId, RenderedTitle>,
    /// The title phase each title on air is in, and the instant this tap saw
    /// that phase start (`None` when it did not: the phase began before the
    /// tap was watching, or its replay has caught up with the engine).
    title_replays: BTreeMap<InputId, (TitleAir, Option<Instant>)>,
    /// Whether the previous step rendered for a demanded state, so a phase
    /// first seen now started while this tap was watching.
    watching: bool,
    /// Decoded image and slide pictures at the fitted size (IN-008,
    /// IN-009).
    stills: StillPictures,
    /// The latest Program picture with overlays composited.
    program_overlaid: Option<(ProgramKey, Option<Arc<ImageFrame>>)>,
    /// Scene compositions performed, observed by tests to prove caching.
    #[cfg(test)]
    compositions: u64,
    /// Title renders performed, observed by tests to prove caching.
    #[cfg(test)]
    title_renders: u64,
    last_key: Option<ChangeKey>,
    last_emit: Option<Instant>,
    sequences: [u64; 2],
}

impl SimulatedMonitorTap {
    /// A tap rendering title inputs from `titles`.
    pub fn with_titles(titles: Arc<TitleAssets>) -> Self {
        Self {
            titles,
            ..Self::default()
        }
    }

    /// This tap, rendering document inputs from `documents` (IN-014).
    pub fn with_documents(self, documents: Arc<DocumentPages>) -> Self {
        Self { documents, ..self }
    }

    /// Cadence and rendering against an explicit realized state. Takes the
    /// state by value so tests can build it inline.
    #[cfg(test)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn poll_state(
        &mut self,
        state: RealizedMonitorState,
        project: &Project,
        server: &ServerIdentity,
        now: Instant,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let mut frames = Vec::new();
        self.poll_state_into(&state, project, server, now, demand, &mut |tap, bytes| {
            frames.push((tap, bytes));
        });
        frames
    }

    /// Cadence and rendering against an explicit realized state, handing
    /// each tap's encoded group to `emit` as soon as it is ready.
    ///
    /// Preview renders and is emitted before Program, so a slow Program
    /// Scene composition never holds back the Preview picture of the same
    /// step.
    pub fn poll_state_into(
        &mut self,
        state: &RealizedMonitorState,
        project: &Project,
        server: &ServerIdentity,
        now: Instant,
        demand: bool,
        emit: &mut dyn FnMut(MonitorTap, Vec<u8>),
    ) {
        if !demand {
            self.idle();
            return;
        }
        if self.built_revision != Some(state.revision) {
            self.pipeline = simulated_pipeline(project);
            self.built_revision = Some(state.revision);
        }
        let replayed = self.replay_titles(state, project.settings().frame_rate, now);
        let state = &replayed;
        let change_key = state.change_key();
        let changed = self.last_key.as_ref() != Some(&change_key);
        let due = self.last_emit.is_none_or(|last| {
            let elapsed = now.saturating_duration_since(last);
            elapsed >= SIMULATED_MONITOR_INTERVAL
                || (changed && elapsed >= SIMULATED_MONITOR_MIN_GAP)
        });
        if !due {
            return;
        }
        self.last_emit = Some(now);
        self.last_key = Some(change_key);
        let Some((width, height)) = self.pipeline.as_ref().and_then(|pipeline| {
            Some((
                u16::try_from(pipeline.width()).ok()?,
                u16::try_from(pipeline.height()).ok()?,
            ))
        }) else {
            return;
        };
        let mut used_scenes = Vec::new();
        for tap in MONITOR_TAP_ORDER {
            let input = state.input(tap);
            let mut image = self.input_picture(input, state, project, &mut used_scenes);
            if tap == MonitorTap::Program && !state.overlays.is_empty() {
                image = image.and_then(|program| {
                    self.overlaid_program(program, state, project, &mut used_scenes)
                });
            }
            // The program grade colours the composed Program picture.
            if tap == MonitorTap::Program
                && let Some(grade) = self.grades.program()
            {
                image = image
                    .and_then(|program| fm_compositor::apply_grade(&program, grade).ok())
                    .map(Arc::new);
            }
            let Some(image) = image else {
                continue;
            };
            let Some(sequence) = self.sequences[tap.index()].checked_add(1) else {
                continue;
            };
            let picture = MonitorPicture {
                tap,
                sequence,
                frame: state.frame,
                input,
                origin: MonitorFrameOrigin::Simulated,
                width,
                height,
                pixels: image.pixels(),
                stride: image.stride(),
                bytes_per_pixel: 4,
            };
            if let Some(bytes) = encode_picture(server, &picture) {
                self.sequences[tap.index()] = sequence;
                emit(tap, bytes);
            }
        }
        // Keep only the compositions this step showed: memory stays bounded
        // by the scenes on air, and an unchanged step reuses all of them.
        self.composed
            .retain(|composed| used_scenes.contains(&composed.key));
        self.rendered_titles.retain(|input, _| {
            state.program == *input
                || state.preview == *input
                || state
                    .overlays
                    .iter()
                    .any(|overlay| overlay.source == *input)
        });
        if state.overlays.is_empty() {
            self.program_overlaid = None;
        }
    }

    /// The opaque fitted-size picture of one input shown on a tap: a
    /// simulated source, a composed scene, or a title over black. `None`
    /// for anything that cannot be rendered honestly.
    fn input_picture(
        &mut self,
        input: InputId,
        state: &RealizedMonitorState,
        project: &Project,
        used_scenes: &mut Vec<ComposeKey>,
    ) -> Option<Arc<ImageFrame>> {
        if scene_render::scene_of_input(project, input).is_some() {
            return self.scene_frame(input, state, project, used_scenes);
        }
        if title_render::title_of_input(project, input).is_some() {
            // A title over black carries its input grade like any input.
            let flat = self
                .title_frame(input, state, project)
                .and_then(|rendered| rendered.flat.clone())?;
            return match self.grades.input(input) {
                Some(grade) => fm_compositor::apply_grade(&flat, grade).ok().map(Arc::new),
                None => Some(flat),
            };
        }
        // A document input shows its current page over black (IN-014).
        if let Some(document) = project.document(input) {
            let pipeline = self.pipeline.as_ref()?;
            let page = self.documents.render(
                &document.asset_uri,
                document.page,
                pipeline.width(),
                pipeline.height(),
            )?;
            let picture = fm_daemon_journal::documents::page_over_black(&page)?;
            return match self.grades.input(input) {
                Some(grade) => fm_compositor::apply_grade(&picture, grade).ok(),
                None => Some(picture),
            }
            .map(Arc::new);
        }
        // Other non-simulated inputs are never registered, so they render
        // nothing. A simulated input carries its input grade.
        let source = match self.still_picture(input, state, project) {
            Some(still) => still,
            None => self
                .pipeline
                .as_ref()
                .and_then(|pipeline| pipeline.render_source(input, state.frame).ok())?,
        };
        match self.grades.input(input) {
            Some(grade) => fm_compositor::apply_grade(&source, grade).ok(),
            None => Some(source),
        }
        .map(Arc::new)
    }

    /// An image input's picture, or what a slideshow input shows, over
    /// black at the fitted size; `None` for any other input or a picture
    /// that does not decode.
    fn still_picture(
        &mut self,
        input: InputId,
        state: &RealizedMonitorState,
        project: &Project,
    ) -> Option<ImageFrame> {
        let pipeline = self.pipeline.as_ref()?;
        let size = (pipeline.width(), pipeline.height());
        let store = self.titles.store()?;
        match &project
            .inputs()
            .iter()
            .find(|known| known.id == input)?
            .kind
        {
            InputKind::Image(image) => self.stills.image(store, &image.asset_uri, image.fit, size),
            InputKind::Slideshow(_) => {
                let shown = state
                    .slideshows
                    .iter()
                    .find(|(slideshow, _)| *slideshow == input)
                    .map(|(_, shown)| shown)?;
                self.stills.slides(store, shown, size)
            }
            _ => None,
        }
    }

    /// Returns the scene picture of `input` at the fitted size, composing it
    /// only when no cached composition matches the realized revision, frame
    /// cursor, and input. Every tap and overlay showing it shares one
    /// composition.
    fn scene_frame(
        &mut self,
        input: InputId,
        state: &RealizedMonitorState,
        project: &Project,
        used_scenes: &mut Vec<ComposeKey>,
    ) -> Option<Arc<ImageFrame>> {
        let key = ComposeKey {
            revision: state.revision,
            frame: state.frame,
            input,
        };
        if !used_scenes.contains(&key) {
            used_scenes.push(key);
        }
        if let Some(cached) = self.composed.iter().find(|composed| composed.key == key) {
            return cached.image.clone();
        }
        #[cfg(test)]
        {
            self.compositions += 1;
        }
        let image = self
            .pipeline
            .as_ref()
            .and_then(|pipeline| {
                scene_render::render_scene(
                    project,
                    input,
                    pipeline,
                    state.frame,
                    state.scene_pose(input),
                    &self.grades,
                )
            })
            .map(Arc::new);
        self.composed.push(ComposedScene {
            key,
            image: image.clone(),
        });
        image
    }

    /// Returns the title picture of `input` at the fitted size, rendering it
    /// only when its template, field revision, field values, animation pose,
    /// or (for a time-dependent template) frame changed.
    fn title_frame(
        &mut self,
        input: InputId,
        state: &RealizedMonitorState,
        project: &Project,
    ) -> Option<&RenderedTitle> {
        let title = title_render::title_of_input(project, input)?;
        let (frame, pose) = (state.frame, state.pose(input));
        let taken_frame = state.taken_frame(input);
        let key = title_render::title_key(&self.titles, title, frame, taken_frame, pose)?;
        let cached = self
            .rendered_titles
            .get(&input)
            .is_some_and(|rendered| rendered.key == key);
        if !cached {
            #[cfg(test)]
            {
                self.title_renders += 1;
            }
            let pipeline = self.pipeline.as_ref()?;
            let keyed = title_render::render_title_at(
                &self.titles,
                input,
                title,
                title_render::TitleTime::now(
                    frame,
                    project.settings().frame_rate,
                    taken_frame,
                    pose,
                ),
                pipeline.width(),
                pipeline.height(),
            );
            let flat = keyed
                .as_ref()
                .and_then(title_render::flatten_over_black)
                .map(Arc::new);
            self.rendered_titles.insert(
                input,
                RenderedTitle {
                    key,
                    keyed: keyed.map(Arc::new),
                    flat,
                },
            );
        }
        self.rendered_titles.get(&input)
    }

    /// One overlay's fitted-size source picture, or `None` when the source
    /// cannot be rendered (the overlay is then skipped, never invented).
    fn overlay_source(
        &mut self,
        input: InputId,
        state: &RealizedMonitorState,
        project: &Project,
        used_scenes: &mut Vec<ComposeKey>,
    ) -> Option<OverlaySource> {
        if title_render::title_of_input(project, input).is_some() {
            let image = self.title_frame(input, state, project)?.keyed.clone()?;
            return Some(OverlaySource {
                image,
                alpha: AlphaMode::Premultiplied,
                grade: self.grades.input(input).cloned(),
            });
        }
        // Simulated and scene pictures are opaque and already graded.
        let image = self.input_picture(input, state, project, used_scenes)?;
        Some(OverlaySource {
            image,
            alpha: AlphaMode::Straight,
            grade: None,
        })
    }

    /// Composites every renderable realized active overlay over the Program
    /// picture, caching the result per revision, frame cursor, Program input,
    /// and overlay set. `None` when the composition itself fails.
    fn overlaid_program(
        &mut self,
        program: Arc<ImageFrame>,
        state: &RealizedMonitorState,
        project: &Project,
        used_scenes: &mut Vec<ComposeKey>,
    ) -> Option<Arc<ImageFrame>> {
        let key = ProgramKey {
            revision: state.revision,
            frame: state.frame,
            program: state.program,
            overlays: state.overlays.clone(),
            titles: std::iter::once(state.program)
                .chain(state.overlays.iter().map(|overlay| overlay.source))
                .map(|input| {
                    title_render::title_of_input(project, input).and_then(|title| {
                        title_render::title_key(
                            &self.titles,
                            title,
                            state.frame,
                            state.taken_frame(input),
                            state.pose(input),
                        )
                    })
                })
                .collect(),
        };
        if let Some((cached, image)) = &self.program_overlaid
            && *cached == key
        {
            // Keep the scenes the cached picture was composed from.
            for overlay in &state.overlays {
                let scene = ComposeKey {
                    revision: state.revision,
                    frame: state.frame,
                    input: overlay.source,
                };
                if self.composed.iter().any(|composed| composed.key == scene)
                    && !used_scenes.contains(&scene)
                {
                    used_scenes.push(scene);
                }
            }
            return image.clone();
        }
        let mut sources = Vec::with_capacity(state.overlays.len());
        for overlay in &state.overlays {
            if let Some(source) = self.overlay_source(overlay.source, state, project, used_scenes) {
                sources.push((*overlay, source));
            }
        }
        let image = if sources.is_empty() {
            Some(program)
        } else {
            composite_overlays(&program, &sources).map(Arc::new)
        };
        self.program_overlaid = Some((key, image.clone()));
        image
    }

    /// Without demand nothing renders; the next demanded poll is immediate.
    fn idle(&mut self) {
        self.last_emit = None;
        self.last_key = None;
        self.watching = false;
        self.title_replays.clear();
    }

    /// `state` with each title's pose replayed in wall time.
    ///
    /// The simulated engine has no frame clock: a title take (or off-air)
    /// renders its whole In (or Out) phase within the command, so every
    /// state after it reports the settled pose and the monitor never showed
    /// the animation. A phase this tap saw start is instead sampled at the
    /// frame wall time has reached since then, at the show's rate, until
    /// that catches up with the engine's frame cursor; from there on the
    /// engine's own pose is shown. A phase that began before the tap was
    /// watching is not replayed: it shows the engine's pose at once.
    fn replay_titles(
        &mut self,
        state: &RealizedMonitorState,
        rate: fm_types::FrameRate,
        now: Instant,
    ) -> RealizedMonitorState {
        let watching = std::mem::replace(&mut self.watching, true);
        self.title_replays
            .retain(|input, _| state.airs.iter().any(|(on_air, _)| on_air == input));
        let mut replayed = state.clone();
        for (input, air) in &state.airs {
            let replay = self
                .title_replays
                .entry(*input)
                .or_insert((*air, watching.then_some(now)));
            if replay.0 != *air {
                // A new take, off-air, or re-take: its phase starts now.
                *replay = (*air, watching.then_some(now));
            }
            let Some(since) = replay.1 else {
                continue;
            };
            let frame = air
                .start_frame()
                .saturating_add(wall_frames(now.saturating_duration_since(since), rate));
            if frame >= state.frame {
                replay.1 = None;
                continue;
            }
            let Some(sample) = air.at(frame).map(|frame| frame.sample) else {
                continue;
            };
            match replayed
                .titles
                .binary_search_by_key(input, |(title, _)| *title)
            {
                Ok(index) => replayed.titles[index].1 = sample,
                Err(index) => replayed.titles.insert(index, (*input, sample)),
            }
        }
        replayed
    }
}

/// The pose of every title a title take animates on its overlay channel or
/// that plays its In or Out phase on Program or Preview, ascending by
/// input: a take's pose wins over the bus pose (GX-001).
fn title_poses(control: &ControlService<Policy>) -> Vec<(InputId, AnimSample)> {
    let mut poses: BTreeMap<InputId, AnimSample> = control
        .titles_shown()
        .into_iter()
        .filter_map(|title| Some((title.input, title.bus?.sample)))
        .collect();
    for title in control.titles_on_air() {
        poses.insert(title.input, title.frame.sample);
    }
    poses.into_iter().collect()
}

/// Whole frames of a show at `rate` in `elapsed` wall time.
fn wall_frames(elapsed: Duration, rate: fm_types::FrameRate) -> u64 {
    let frames = elapsed.as_nanos() * u128::from(rate.numerator())
        / (u128::from(rate.denominator()) * 1_000_000_000);
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// Layers each overlay source over `program` with the native overlay
/// geometry evaluated at the program picture's size.
fn composite_overlays(
    program: &ImageFrame,
    overlays: &[(RealizedOverlay, OverlaySource)],
) -> Option<ImageFrame> {
    let (width, height) = (program.width(), program.height());
    let dimensions = VideoDimensions::new(width, height)?;
    let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, u8::MAX)).ok()?;
    scene.push_layer(SourceLayer::new(
        SourceId::new(0),
        0,
        Transform::new(0, 0, width, height, Rotation::Deg0),
    ));
    for (index, (overlay, source)) in overlays.iter().enumerate() {
        let token = SourceId::new(u64::try_from(index + 1).ok()?);
        let transform = overlay_transform(dimensions, overlay.position)?;
        let mut layer = SourceLayer::new(token, i32::try_from(index + 1).ok()?, transform)
            .with_opacity(overlay.opacity)
            .with_alpha_mode(source.alpha)
            .with_inset_border(overlay_border_width(overlay.border, transform));
        // The compositor grades a premultiplied title on its straight
        // colour and never touches its alpha.
        if let Some(grade) = &source.grade {
            layer = layer.with_grade(Arc::clone(grade));
        }
        scene.push_layer(layer);
    }
    let (plan, _) = compile_scene(&scene, OutputTarget::Program).ok()?;
    let mut bindings = Vec::with_capacity(overlays.len() + 1);
    bindings.push(CpuSourceFrame::new(SourceId::new(0), program));
    for (index, (_, source)) in overlays.iter().enumerate() {
        bindings.push(CpuSourceFrame::new(
            SourceId::new(u64::try_from(index + 1).ok()?),
            &source.image,
        ));
    }
    execute_cpu(&plan, &bindings).ok()
}

/// What the composition worker renders: the realized state and the project
/// it depicts, or `None` when no peer wants frames.
type MonitorWork = Option<(RealizedMonitorState, Arc<MonitorProject>)>;

/// The project a realized state depicts and its compiled grades.
pub struct MonitorProject {
    project: Project,
    grades: Arc<CompiledProjectGrades>,
}

/// Renders one cadence step of `MonitorWork` at `Instant`, handing each
/// finished tap group to the sink as soon as it is encoded.
type MonitorRenderer =
    Box<dyn FnMut(MonitorWork, Instant, &mut dyn FnMut(MonitorTap, Vec<u8>)) + Send>;

/// Longest the worker sleeps between cadence steps while frames are wanted.
const MONITOR_WORKER_STEP: Duration = Duration::from_millis(10);

/// The latest work published by the session loop, and the latest finished
/// group per tap awaiting it.
struct WorkerSlot {
    work: MonitorWork,
    changed: bool,
    shutdown: bool,
    /// One latest-wins group per tap (indexed by `MonitorTap::index`).
    /// Monitor frames are lossy: a newer group replaces an uncollected one,
    /// so the worker never blocks, memory stays bounded at one group per tap,
    /// and a burst of Program groups can never crowd out a Preview group.
    finished: [Option<Vec<u8>>; 2],
}

struct WorkerShared {
    slot: Mutex<WorkerSlot>,
    wake: Condvar,
}

impl WorkerShared {
    fn lock(&self) -> MutexGuard<'_, WorkerSlot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs [`SimulatedMonitorTap`] on a dedicated thread.
///
/// Composing a Scene input is CPU work that can still take a noticeable time
/// in a debug build. It must never run on the daemon's session loop: a
/// command accepted while a composition ran would wait for it before its
/// result could be written, and a client waiting for that result treats the
/// silence as a dead connection and reconnects. The session loop only
/// publishes the latest realized state (latest wins) and collects whichever
/// encoded groups are already finished.
pub struct SimulatedMonitorWorker {
    shared: Arc<WorkerShared>,
    worker: Option<JoinHandle<()>>,
    project: Option<(u64, Arc<MonitorProject>)>,
    published: Published,
    grade_cache: GradeCache,
    /// The grades of the latest demanded poll.
    grades: Arc<CompiledProjectGrades>,
}

/// The last work handed to the worker, so unchanged polls skip the lock.
#[derive(Clone, Eq, PartialEq)]
enum Published {
    Nothing,
    Idle,
    State(RealizedMonitorState),
}

impl SimulatedMonitorWorker {
    /// Starts the worker rendering frames that carry `server`'s identity.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub fn spawn(
        server: ServerIdentity,
        titles: Arc<TitleAssets>,
        documents: Arc<DocumentPages>,
    ) -> std::io::Result<Self> {
        let mut tap = SimulatedMonitorTap::with_titles(titles).with_documents(documents);
        Self::spawn_with(Box::new(move |work, now, emit| {
            if let Some((state, work)) = work {
                tap.grades = Arc::clone(&work.grades);
                tap.poll_state_into(&state, &work.project, &server, now, true, emit);
            } else {
                tap.idle();
            }
        }))
    }

    fn spawn_with(mut render: MonitorRenderer) -> std::io::Result<Self> {
        let shared = Arc::new(WorkerShared {
            slot: Mutex::new(WorkerSlot {
                work: None,
                changed: false,
                shutdown: false,
                finished: [None, None],
            }),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("freemixd-monitor".to_owned())
            .spawn(move || run_monitor_worker(&worker_shared, &mut render))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            project: None,
            published: Published::Nothing,
            grade_cache: GradeCache::default(),
            grades: Arc::default(),
        })
    }

    /// Publishes the current realized state when it changed and returns the
    /// encoded groups the worker has finished since the last call. Never
    /// waits for a composition.
    pub fn poll(
        &mut self,
        control: &ControlService<Policy>,
        project: &Project,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let state = demand.then(|| RealizedMonitorState::from_control(control));
        if demand {
            self.grades = self.grade_cache.at(control);
        }
        self.poll_state(state, project)
    }

    fn poll_state(
        &mut self,
        state: Option<RealizedMonitorState>,
        project: &Project,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let published = state.clone().map_or(Published::Idle, Published::State);
        let work = (self.published != published).then(|| {
            state.map(|state| {
                let project = self.project_at(state.revision, project);
                (state, project)
            })
        });
        let mut slot = self.shared.lock();
        if let Some(work) = work {
            slot.work = work;
            slot.changed = true;
            self.published = published;
            self.shared.wake.notify_one();
        }
        MONITOR_TAP_ORDER
            .into_iter()
            .filter_map(|tap| Some((tap, slot.finished[tap.index()].take()?)))
            .collect()
    }

    /// Shares one project copy, with its compiled grades, per realized
    /// revision with the worker.
    fn project_at(&mut self, revision: u64, project: &Project) -> Arc<MonitorProject> {
        match &self.project {
            Some((cached, shared)) if *cached == revision => Arc::clone(shared),
            _ => {
                let shared = Arc::new(MonitorProject {
                    project: project.clone(),
                    grades: Arc::clone(&self.grades),
                });
                self.project = Some((revision, Arc::clone(&shared)));
                shared
            }
        }
    }
}

impl Drop for SimulatedMonitorWorker {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        // An idle worker exits at once; a composition still in progress
        // finishes on its own detached thread so shutdown never waits for it.
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

fn run_monitor_worker(shared: &WorkerShared, render: &mut MonitorRenderer) {
    let mut work: MonitorWork = None;
    loop {
        {
            let mut slot = shared.lock();
            if !slot.changed && !slot.shutdown {
                // With demand the cadence needs periodic steps; without it
                // nothing renders until the session loop publishes again.
                slot = if work.is_some() {
                    shared
                        .wake
                        .wait_timeout(slot, MONITOR_WORKER_STEP)
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
                if work.is_none() {
                    // Nobody wants frames any more; drop what was finished.
                    slot.finished = [None, None];
                }
            }
        }
        render(work.clone(), Instant::now(), &mut |tap, bytes| {
            // Each tap's group is visible to the session loop the moment it
            // is encoded, before the next tap starts composing.
            shared.lock().finished[tap.index()] = Some(bytes);
        });
    }
}

/// Builds a pipeline at the fitted project dimensions holding every simulated
/// input. Returns `None` when the project cannot form a monitor image.
fn simulated_pipeline(project: &Project) -> Option<SimulatedPipeline> {
    let dimensions = project.settings().video.dimensions;
    let (width, height) = fit_monitor(dimensions.width(), dimensions.height())?;
    simulated_pipeline_at(project, u32::from(width), u32::from(height))
}

/// Builds a pipeline at exactly `width`x`height` holding every simulated input.
fn simulated_pipeline_at(project: &Project, width: u32, height: u32) -> Option<SimulatedPipeline> {
    let mut pipeline = SimulatedPipeline::new(width, height).ok()?;
    for input in project.inputs() {
        let InputKind::Simulated(simulated) = &input.kind else {
            continue;
        };
        let pattern = match simulated.video {
            SimulatedVideo::Bars => SourcePattern::Bars,
            SimulatedVideo::Solid(color) => {
                SourcePattern::Solid(Rgba8::new(color.red, color.green, color.blue, color.alpha))
            }
        };
        pipeline
            .register(SimulatedSource::new(input.id, pattern))
            .ok()?;
    }
    Some(pipeline)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;
    use std::sync::mpsc;

    use fm_model::{
        Input, Layer, LayerGeometry, ProjectSettings, Rotation, SimulatedAudio, SimulatedInput,
        SolidColor, SourceRef,
    };
    use fm_protocol::{LineDecoder, MonitorFrameAssembler, MonitorImage};
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
    };

    use super::*;

    const NO_FRAMES: [(MonitorTap, Vec<u8>); 0] = [];

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

    /// Input 1 is a solid, input 2 is bars, input 3 is a non-simulated color.
    fn project(width: u32, height: u32) -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Monitor",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(width, height).unwrap(),
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
        project.add_input(Input {
            id: input_id(1),
            name: "Solid".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(7, 11, 13, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: input_id(2),
            name: "Bars".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: input_id(3),
            name: "Color".into(),
            kind: InputKind::Color,
            required_capabilities: Vec::new(),
        });
        project
    }

    fn layer(source: SourceRef, x: i32, y: i32, width: u32, height: u32) -> Layer {
        Layer {
            id: {
                // Unique per call, so helper-built layers never share an id.
                static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
                fm_model::LayerId::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    .unwrap()
            },
            name: "Layer".into(),
            source,
            enabled: true,
            geometry: LayerGeometry::new(x, y, width, height, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: 255,
            z_order: 0,
        }
    }

    /// Adds scene `scene` with a dark background, exposed as input `input`.
    fn add_scene(project: &mut Project, scene: u128, input: u128, layers: Vec<Layer>) {
        let scene_id = SceneId::new(NonZeroU128::new(scene).unwrap());
        project.add_scene(fm_model::Scene {
            id: scene_id,
            name: format!("Scene {scene}"),
            background: fm_model::Rgba8::new(20, 30, 40, 255),
            layers,
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_input(Input {
            id: input_id(input),
            name: format!("Scene input {input}"),
            kind: InputKind::Scene {
                scene_id,
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
    }

    fn state(revision: u64, program: u128, preview: u128) -> RealizedMonitorState {
        RealizedMonitorState {
            revision,
            frame: 4,
            program: input_id(program),
            preview: input_id(preview),
            overlays: Vec::new(),
            titles: Vec::new(),
            shown: Vec::new(),
            scenes: Vec::new(),
            airs: Vec::new(),
            slideshows: Vec::new(),
        }
    }

    fn decode(bytes: &[u8]) -> MonitorImage {
        let messages = LineDecoder::new().push(bytes).unwrap();
        let mut assembler = MonitorFrameAssembler::new();
        let count = messages.len();
        for (index, message) in messages.into_iter().enumerate() {
            let WireMessage::MonitorFrame(part) = message else {
                panic!("monitor groups contain only monitor_frame records");
            };
            let image = assembler.push(&part).unwrap();
            if index + 1 == count {
                return image.expect("the last part completes the frame");
            }
            assert!(image.is_none());
        }
        panic!("empty monitor group");
    }

    fn pixel(image: &MonitorImage, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * usize::from(image.width) + x) * 4;
        image.rgba[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn simulated_tap_renders_exact_solid_and_bars_pixels_at_fitted_size() {
        let project = project(1_920, 1_080);
        let mut tap = SimulatedMonitorTap::default();
        let frames = tap.poll_state(state(1, 1, 2), &project, &server(), Instant::now(), true);
        assert_eq!(
            frames.iter().map(|(tap, _)| *tap).collect::<Vec<_>>(),
            vec![MonitorTap::Preview, MonitorTap::Program]
        );

        let preview = decode(&frames[0].1);
        assert_eq!((preview.width, preview.height), (640, 360));
        assert_eq!(preview.tap, MonitorTap::Preview);
        assert_eq!(preview.origin, MonitorFrameOrigin::Simulated);
        assert_eq!(preview.input, WireInputId::from_domain(input_id(2)));
        assert_eq!((preview.sequence, preview.frame), (1, 4));
        // Eight exact 75% BT.709 bars, 80 pixels each, on every row; the
        // picture is static, so no pixel encodes the frame number.
        let bars = [
            [191, 191, 191, 255],
            [191, 191, 0, 255],
            [0, 191, 191, 255],
            [0, 191, 0, 255],
            [191, 0, 191, 255],
            [191, 0, 0, 255],
            [0, 0, 191, 255],
            [0, 0, 0, 255],
        ];
        for (bar, expected) in bars.iter().enumerate() {
            for x in [bar * 80, bar * 80 + 79] {
                for y in [0, 359] {
                    assert_eq!(pixel(&preview, x, y), *expected, "bar {bar} at ({x}, {y})");
                }
            }
        }

        let program = decode(&frames[1].1);
        assert_eq!(program.tap, MonitorTap::Program);
        assert_eq!(program.input, WireInputId::from_domain(input_id(1)));
        assert_eq!((program.width, program.height), (640, 360));
        assert!(
            program
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [7, 11, 13, 255])
        );
    }

    #[test]
    fn simulated_tap_paces_at_five_fps_and_publishes_changes_promptly() {
        let project = project(1_280, 720);
        let server = server();
        let mut tap = SimulatedMonitorTap::default();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let sequences = |frames: &[(MonitorTap, Vec<u8>)]| {
            frames
                .iter()
                .map(|(_, bytes)| decode(bytes).sequence)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            sequences(&tap.poll_state(state(1, 1, 2), &project, &server, at(0), true)),
            [1, 1]
        );
        assert_eq!(
            tap.poll_state(state(1, 1, 2), &project, &server, at(100), true),
            NO_FRAMES
        );
        assert_eq!(
            tap.poll_state(state(1, 1, 2), &project, &server, at(199), true),
            NO_FRAMES
        );
        assert_eq!(
            sequences(&tap.poll_state(state(1, 1, 2), &project, &server, at(200), true)),
            [2, 2]
        );
        // A realized change inside the minimum gap waits for the gap.
        assert_eq!(
            tap.poll_state(state(2, 2, 1), &project, &server, at(220), true),
            NO_FRAMES
        );
        let changed = tap.poll_state(state(2, 2, 1), &project, &server, at(250), true);
        assert_eq!(sequences(&changed), [3, 3]);
        assert_eq!(
            decode(&changed[1].1).input,
            WireInputId::from_domain(input_id(2))
        );
        // The steady cadence restarts from the change.
        assert_eq!(
            tap.poll_state(state(2, 2, 1), &project, &server, at(400), true),
            NO_FRAMES
        );
        assert_eq!(
            sequences(&tap.poll_state(state(2, 2, 1), &project, &server, at(450), true)),
            [4, 4]
        );
    }

    #[test]
    fn simulated_tap_renders_nothing_without_demand_and_resumes_immediately() {
        let project = project(1_280, 720);
        let server = server();
        let mut tap = SimulatedMonitorTap::default();
        let start = Instant::now();
        assert_eq!(
            tap.poll_state(state(1, 1, 2), &project, &server, start, true)
                .len(),
            2
        );
        for millis in [0, 200, 400, 1_000] {
            assert_eq!(
                tap.poll_state(
                    state(1, 1, 2),
                    &project,
                    &server,
                    start + Duration::from_millis(millis),
                    false
                ),
                NO_FRAMES
            );
        }
        // Demand returning publishes at once, continuing each tap's sequence.
        let frames = tap.poll_state(
            state(1, 1, 2),
            &project,
            &server,
            start + Duration::from_millis(1_010),
            true,
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(decode(&frames[0].1).sequence, 2);
    }

    #[test]
    fn non_simulated_inputs_produce_no_frame() {
        let project = project(1_280, 720);
        let server = server();
        let mut tap = SimulatedMonitorTap::default();
        let frames = tap.poll_state(state(1, 3, 2), &project, &server, Instant::now(), true);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, MonitorTap::Preview);
        let frames = tap.poll_state(
            state(2, 3, 99),
            &project,
            &server,
            Instant::now() + Duration::from_secs(1),
            true,
        );
        assert_eq!(frames, NO_FRAMES);
    }

    #[test]
    fn scene_layers_compose_at_the_fitted_monitor_size() {
        let mut project = project(1_920, 1_080);
        // The solid input covers exactly the top-left quarter of 1920x1080.
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(SourceRef::Input(input_id(1)), 0, 0, 960, 540)],
        );
        let mut tap = SimulatedMonitorTap::default();
        let frames = tap.poll_state(state(1, 10, 2), &project, &server(), Instant::now(), true);
        assert_eq!(
            frames.iter().map(|(tap, _)| *tap).collect::<Vec<_>>(),
            vec![MonitorTap::Preview, MonitorTap::Program]
        );
        let program = decode(&frames[1].1);
        assert_eq!(program.tap, MonitorTap::Program);
        assert_eq!(program.input, WireInputId::from_domain(input_id(10)));
        assert_eq!(program.origin, MonitorFrameOrigin::Simulated);
        assert_eq!((program.width, program.height), (640, 360));
        for y in 0..360 {
            for x in 0..640 {
                let expected = if x < 320 && y < 180 {
                    [7, 11, 13, 255]
                } else {
                    [20, 30, 40, 255]
                };
                assert_eq!(pixel(&program, x, y), expected, "pixel ({x}, {y})");
            }
        }
        assert_eq!(tap.compositions, 1);
    }

    #[test]
    fn scene_compositions_are_cached_per_revision_frame_and_input() {
        let mut project = project(1_280, 720);
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(SourceRef::Input(input_id(2)), 0, 0, 640, 360)],
        );
        let server = server();
        let mut tap = SimulatedMonitorTap::default();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        // Both taps show the same scene: one composition serves both.
        assert_eq!(
            tap.poll_state(state(1, 10, 10), &project, &server, at(0), true)
                .len(),
            2
        );
        assert_eq!(tap.compositions, 1);
        // Steady cadence with an unchanged frame cursor reuses the cache.
        for millis in [200, 400, 600] {
            assert_eq!(
                tap.poll_state(state(1, 10, 10), &project, &server, at(millis), true)
                    .len(),
                2
            );
        }
        assert_eq!(tap.compositions, 1);
        // A new frame cursor or revision recomposes once more.
        let mut advanced = state(1, 10, 10);
        advanced.frame = 5;
        assert_eq!(
            tap.poll_state(advanced, &project, &server, at(800), true)
                .len(),
            2
        );
        assert_eq!(tap.compositions, 2);
        assert_eq!(
            tap.poll_state(state(2, 10, 1), &project, &server, at(1_000), true)
                .len(),
            2
        );
        assert_eq!(tap.compositions, 3);
    }

    #[test]
    fn unsupported_scenes_produce_no_frame() {
        let mut project = project(1_280, 720);
        // A non-simulated layer source.
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(SourceRef::Input(input_id(3)), 0, 0, 640, 360)],
        );
        // A nested scene layer.
        add_scene(
            &mut project,
            2,
            11,
            vec![layer(
                SourceRef::Scene(SceneId::new(NonZeroU128::new(1).unwrap())),
                0,
                0,
                640,
                360,
            )],
        );
        // A crop outside the project-size source.
        let mut cropped = layer(SourceRef::Input(input_id(1)), 0, 0, 640, 360);
        cropped.crop = Some(fm_model::CropRect {
            x: 1_000,
            y: 0,
            width: 400,
            height: 100,
        });
        add_scene(&mut project, 3, 12, vec![cropped]);
        // A layer naming a missing input, and a scene input with no scene.
        add_scene(
            &mut project,
            4,
            13,
            vec![layer(SourceRef::Input(input_id(99)), 0, 0, 640, 360)],
        );
        let server = server();
        let mut tap = SimulatedMonitorTap::default();
        let start = Instant::now();
        for (index, program) in [10, 11, 12, 13].into_iter().enumerate() {
            let millis = u64::try_from(index).unwrap() * 1_000;
            let frames = tap.poll_state(
                state(1, program, 1),
                &project,
                &server,
                start + Duration::from_millis(millis),
                true,
            );
            assert_eq!(
                frames.iter().map(|(tap, _)| *tap).collect::<Vec<_>>(),
                vec![MonitorTap::Preview],
                "scene input {program}"
            );
        }
    }

    const TITLE_URI: &str = "asset://titles/lower-third.fmtitle";
    const CLOCK_TITLE_URI: &str = "asset://titles/clock.fmtitle";
    const FADE_TITLE_URI: &str = "asset://titles/fade.fmtitle";

    fn field(value: u128) -> fm_titles::FieldId {
        fm_titles::FieldId::new(NonZeroU128::new(value).unwrap())
    }

    /// A 1920x1080 template with a transparent background and one rectangle
    /// at (300, 150) sized 600x300, tinted by color field 1. With `clock`, a
    /// font-less scene clock makes the template time-dependent (it draws no
    /// text without a font, so the picture itself does not change).
    fn title_template(clock: bool) -> fm_titles::TitleTemplate {
        use fm_titles::{
            Bounds, ClockDirection, ClockFormat, ClockSpec, Color, Element, ElementId, ElementKind,
            FieldDefinition, FieldValue, FontStyle, Style, TemplateId, TitleTemplate,
        };
        let element = |id: u128, kind: ElementKind, font: Option<FontStyle>| Element {
            id: ElementId::new(NonZeroU128::new(id).unwrap()),
            name: format!("Element {id}"),
            bounds: Bounds {
                x: 300,
                y: 150,
                width: 600,
                height: 300,
            },
            z_index: 0,
            visible: true,
            style: Style {
                font,
                ..Style::default()
            },
            color_field: None,
            kind,
        };
        let mut bar = element(1, ElementKind::Rectangle, None);
        bar.color_field = Some(field(1));
        let mut elements = vec![bar];
        if clock {
            elements.push(element(
                2,
                ElementKind::Clock(ClockSpec {
                    direction: ClockDirection::CountUp,
                    start_value_ms: 0,
                    starts_at_ms: 0,
                    format: ClockFormat::MinutesSeconds,
                }),
                Some(FontStyle {
                    family: "Unavailable".into(),
                    size_px: 32,
                }),
            ));
        }
        TitleTemplate {
            id: TemplateId::new(NonZeroU128::new(1).unwrap()),
            name: "Lower third".into(),
            width: 1_920,
            height: 1_080,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field(1),
                name: "Bar".into(),
                default: FieldValue::Color(Color::new(1, 2, 3, 255)),
                exposed: true,
            }],
            elements,
            animation: fm_titles::TitleAnimation::default(),
        }
    }

    fn title_assets() -> Arc<TitleAssets> {
        let mut assets = TitleAssets::default();
        assets.insert(
            TITLE_URI,
            title_render::LoadedTitleTemplate::new(
                title_template(false),
                fm_titles::AssetCatalog::new(),
            ),
        );
        assets.insert(
            CLOCK_TITLE_URI,
            title_render::LoadedTitleTemplate::new(
                title_template(true),
                fm_titles::AssetCatalog::new(),
            ),
        );
        let mut fade = title_template(false);
        let bar = fade.elements[0].id;
        fade.edit_animation(|clip| {
            use fm_titles::fm_animation::{
                Easing, Keyframe, Phase, Property, Track, TrackKey, Value,
            };
            clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
            clip.set_track(Track::new(
                TrackKey::new(bar, Phase::In, Property::Opacity),
                vec![
                    Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                    Keyframe::new(10, Value::Scalar(255), Easing::Linear),
                ],
            ))
        })
        .unwrap();
        assets.insert(
            FADE_TITLE_URI,
            title_render::LoadedTitleTemplate::new(fade, fm_titles::AssetCatalog::new()),
        );
        Arc::new(assets)
    }

    fn bar_color(red: u8, green: u8, blue: u8) -> Vec<fm_model::TitleFieldEntry> {
        vec![fm_model::TitleFieldEntry {
            id: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
            value: fm_model::TitleFieldValue::Color(fm_model::Rgba8::new(red, green, blue, 255)),
            revision: 0,
        }]
    }

    fn add_title(project: &mut Project, input: u128, uri: &str, bar: (u8, u8, u8)) {
        project.add_input(Input {
            id: input_id(input),
            name: format!("Title {input}"),
            kind: InputKind::Title(fm_model::TitleInput {
                template_uri: uri.into(),
                field_revision: 0,
                fields: bar_color(bar.0, bar.1, bar.2),
            }),
            required_capabilities: Vec::new(),
        });
    }

    /// Asserts `image` is `inside` exactly on the monitor rectangle
    /// `x0..x1` x `y0..y1` and `outside` everywhere else.
    fn assert_rectangle(
        image: &MonitorImage,
        (x0, y0, x1, y1): (usize, usize, usize, usize),
        inside: [u8; 4],
        outside: [u8; 4],
    ) {
        for y in 0..usize::from(image.height) {
            for x in 0..usize::from(image.width) {
                let expected = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                    inside
                } else {
                    outside
                };
                assert_eq!(pixel(image, x, y), expected, "pixel ({x}, {y})");
            }
        }
    }

    /// The template rectangle (300, 150, 600, 300) at 1920x1080 lands on
    /// (100, 50)..(300, 150) of the 640x360 monitor.
    const TITLE_BAR: (usize, usize, usize, usize) = (100, 50, 300, 150);

    #[test]
    fn title_inputs_render_their_persisted_fields_on_program_and_preview() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        add_title(&mut project, 21, TITLE_URI, (5, 150, 250));
        let server = server();
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        let start = Instant::now();
        let frames = tap.poll_state(state(1, 20, 21), &project, &server, start, true);
        assert_eq!(
            frames.iter().map(|(tap, _)| *tap).collect::<Vec<_>>(),
            vec![MonitorTap::Preview, MonitorTap::Program]
        );
        let preview = decode(&frames[0].1);
        assert_eq!(preview.input, WireInputId::from_domain(input_id(21)));
        assert_eq!(preview.origin, MonitorFrameOrigin::Simulated);
        assert_eq!((preview.width, preview.height), (640, 360));
        // The transparent template background is shown over black.
        assert_rectangle(&preview, TITLE_BAR, [5, 150, 250, 255], [0, 0, 0, 255]);
        let program = decode(&frames[1].1);
        assert_eq!(program.input, WireInputId::from_domain(input_id(20)));
        assert_rectangle(&program, TITLE_BAR, [200, 40, 10, 255], [0, 0, 0, 255]);
        assert_eq!(tap.title_renders, 2);

        // A new field value shows on the next picture.
        project
            .set_title_fields(input_id(20), 1, bar_color(9, 99, 199))
            .unwrap();
        let frames = tap.poll_state(
            state(2, 20, 21),
            &project,
            &server,
            start + Duration::from_millis(200),
            true,
        );
        let program = decode(&frames[1].1);
        assert_rectangle(&program, TITLE_BAR, [9, 99, 199, 255], [0, 0, 0, 255]);
        // Only the changed title rendered again.
        assert_eq!(tap.title_renders, 3);
    }

    #[test]
    fn static_titles_are_cached_and_clock_titles_rerender_per_frame() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        add_title(&mut project, 22, CLOCK_TITLE_URI, (200, 40, 10));
        let server = server();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);

        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        // Both taps show the static title: one render serves both, and new
        // frame cursors and revisions reuse it.
        for (step, (revision, frame)) in [(1, 4), (1, 5), (2, 6), (3, 7)].into_iter().enumerate() {
            let mut realized = state(revision, 20, 20);
            realized.frame = frame;
            let millis = u64::try_from(step).unwrap() * 200;
            assert_eq!(
                tap.poll_state(realized, &project, &server, at(millis), true)
                    .len(),
                2
            );
            assert_eq!(tap.title_renders, 1, "static title, step {step}");
        }

        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        for (step, frame) in [4, 4, 5, 6].into_iter().enumerate() {
            let mut realized = state(1, 22, 22);
            realized.frame = frame;
            let millis = u64::try_from(step).unwrap() * 200;
            assert_eq!(
                tap.poll_state(realized, &project, &server, at(millis), true)
                    .len(),
                2
            );
        }
        // Frames 4, 5, and 6 each render once; the repeated frame 4 hits.
        assert_eq!(tap.title_renders, 3);
    }

    #[test]
    fn titles_render_the_engine_pose_and_rerender_only_when_it_changes() {
        use fm_titles::fm_animation::{ClipPosition, ClipState};

        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, FADE_TITLE_URI, (200, 40, 10));
        let server = server();
        let start = Instant::now();
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        let mut program_at = |step: u64, pose: Option<AnimSample>| {
            let mut realized = state(1, 20, 1);
            realized.frame = 4 + step;
            realized.titles = pose.map(|pose| (input_id(20), pose)).into_iter().collect();
            let frames = tap.poll_state(
                realized,
                &project,
                &server,
                start + Duration::from_millis(step * 200),
                true,
            );
            let program = decode(&frames.last().unwrap().1);
            (pixel(&program, 200, 100), tap.title_renders)
        };
        // In frame 0: fully transparent, so only the black backdrop shows.
        assert_eq!(
            program_at(0, Some(ClipState::In(ClipPosition::frame(0)))),
            ([0, 0, 0, 255], 1)
        );
        // Half way through the In phase.
        let (half, renders) = program_at(1, Some(ClipState::In(ClipPosition::frame(5))));
        assert!((90..=110).contains(&half[0]), "{half:?}");
        assert_eq!(renders, 2);
        // Hold, and an untracked title, are the same pose: one render.
        assert_eq!(
            program_at(2, Some(ClipState::Hold)),
            ([200, 40, 10, 255], 3)
        );
        assert_eq!(program_at(3, None), ([200, 40, 10, 255], 3));
    }

    #[test]
    fn a_simulated_title_take_plays_its_in_phase_on_the_monitor_in_wall_time() {
        use fm_titles::fm_animation::Phase;
        use fm_titles::{TitleAir, TitleAirTiming};

        // The simulated engine realizes a take's whole In phase within the
        // command, so every state after it reports the Hold pose. The tap
        // plays the phase it saw start over the following wall time.
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, FADE_TITLE_URI, (200, 40, 10));
        let rate = project.settings().frame_rate;
        let timing = TitleAirTiming::of(
            &title_assets()
                .template(FADE_TITLE_URI)
                .unwrap()
                .current()
                .animation,
        );
        let in_frames = timing.output_frames(Phase::In, rate);
        assert!(in_frames >= 8, "{in_frames}");
        let frame_time = |frames: u64| {
            Duration::from_nanos(
                frames * 1_000_000_000 * u64::from(rate.denominator())
                    / u64::from(rate.numerator()),
            )
        };
        let server = server();
        let start = Instant::now();
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        // Watching before the take: Program shows another input.
        let watched = tap.poll_state(state(1, 1, 1), &project, &server, start, true);
        assert_eq!(watched.len(), 2);
        let taken = |frame_after_take: u64| {
            // The title alone on Program, so its picture is over black.
            let mut realized = state(2, 20, 1);
            realized.frame = 10 + in_frames + frame_after_take;
            realized.titles = vec![(input_id(20), AnimSample::Hold)];
            realized.airs = vec![(input_id(20), TitleAir::take(timing, rate, 10))];
            realized
        };
        let mut bar_at = |at: Instant| {
            let frames = tap.poll_state(taken(0), &project, &server, at, true);
            let program = frames
                .iter()
                .rev()
                .find(|(tap, _)| *tap == MonitorTap::Program)
                .map(|(_, bytes)| decode(bytes))
                .expect("a Program frame");
            pixel(&program, 200, 100)
        };
        let take = start + Duration::from_millis(400);
        // The first picture after the take is the In phase's first frame.
        let first = bar_at(take);
        assert!(first[0] < 40, "{first:?}");
        // Half way through the In phase in wall time, half faded in.
        let half = bar_at(take + frame_time(in_frames / 2));
        assert!((60..=150).contains(&half[0]), "{half:?}");
        // Once the phase's wall time has passed, the title holds.
        assert_eq!(bar_at(take + frame_time(in_frames + 2)), [200, 40, 10, 255]);

        // A tap that starts watching after the take shows the hold at once:
        // it never saw the take, so it does not replay it.
        let mut late = SimulatedMonitorTap::with_titles(title_assets());
        let frames = late.poll_state(taken(0), &project, &server, take, true);
        let program = decode(&frames.last().unwrap().1);
        assert_eq!(pixel(&program, 200, 100), [200, 40, 10, 255]);
    }

    #[test]
    fn titles_are_dated_by_the_engine_timeline_not_by_when_the_tap_sees_them() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        add_title(&mut project, 22, CLOCK_TITLE_URI, (200, 40, 10));
        let assets = title_assets();
        let title = title_render::title_of_input(&project, input_id(22)).unwrap();
        // The engine took the clock title to air on frame 4; a tap that
        // first looks on frame 9 still counts from frame 4.
        let mut realized = state(1, 22, 1);
        realized.frame = 9;
        realized.shown = vec![(input_id(22), 4)];
        assert_eq!(realized.taken_frame(input_id(22)), Some(4));
        assert_eq!(realized.taken_frame(input_id(20)), None);
        let key = |frame, taken| {
            title_render::title_key(&assets, title, frame, taken, AnimSample::Hold).unwrap()
        };
        assert_eq!(key(9, realized.taken_frame(input_id(22))), key(9, Some(4)));
        // The take frame is part of a scene-timed title's key: the same
        // frame on another timeline is another picture.
        assert_ne!(key(9, Some(4)), key(9, Some(5)));
        let mut tap = SimulatedMonitorTap::with_titles(Arc::clone(&assets));
        assert_eq!(
            tap.poll_state(realized, &project, &server(), Instant::now(), true)
                .len(),
            2
        );
    }

    #[test]
    fn titles_without_loaded_templates_produce_no_frame() {
        let mut project = project(1_920, 1_080);
        add_title(
            &mut project,
            20,
            "asset://titles/missing.fmtitle",
            (1, 1, 1),
        );
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        let frames = tap.poll_state(state(1, 20, 1), &project, &server(), Instant::now(), true);
        assert_eq!(
            frames.iter().map(|(tap, _)| *tap).collect::<Vec<_>>(),
            vec![MonitorTap::Preview]
        );
    }

    fn overlay(
        source: u128,
        opacity: u8,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    ) -> RealizedOverlay {
        RealizedOverlay {
            source: input_id(source),
            opacity,
            position,
            border,
        }
    }

    /// `round(left * right / 255)`, the compositor's 8-bit product.
    fn mul(left: u8, right: u8) -> u8 {
        u8::try_from((u16::from(left) * u16::from(right) + 127) / 255).unwrap()
    }

    #[test]
    fn half_opacity_top_right_overlay_blends_in_its_exact_rectangle() {
        let mut project = project(1_920, 1_080);
        project.add_input(Input {
            id: input_id(5),
            name: "Overlay".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(240, 120, 60, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        let mut realized = state(1, 1, 2);
        realized.overlays = vec![
            overlay(
                5,
                128,
                OverlayPositionPreset::TopRight,
                OverlayBorderPreset::None,
            ),
            // An unrenderable source (a Color input) is skipped.
            overlay(
                3,
                255,
                OverlayPositionPreset::FullFrame,
                OverlayBorderPreset::None,
            ),
        ];
        let mut tap = SimulatedMonitorTap::default();
        let frames = tap.poll_state(realized, &project, &server(), Instant::now(), true);
        let program = decode(&frames[1].1);
        assert_eq!(program.tap, MonitorTap::Program);
        assert_eq!(program.input, WireInputId::from_domain(input_id(1)));
        // Premultiplied source-over at opacity 128 of (240, 120, 60) on
        // (7, 11, 13): source * 128/255 + destination * (255 - 128)/255.
        let blend = |source: u8, destination: u8| mul(source, 128) + mul(destination, 127);
        let blended = [blend(240, 7), blend(120, 11), blend(60, 13), 255];
        assert_eq!(blended, [123, 65, 36, 255]);
        // TopRight at 640x360: a 213x120 box inset 11 pixels from the corner.
        assert_rectangle(&program, (416, 11, 629, 131), blended, [7, 11, 13, 255]);
        // Preview never shows overlays.
        let preview = decode(&frames[0].1);
        assert_eq!(&preview.rgba[..4], &[191, 191, 191, 255]);
    }

    #[test]
    fn title_overlay_composites_over_a_solid_program() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        let mut realized = state(1, 1, 2);
        realized.overlays = vec![overlay(
            20,
            255,
            OverlayPositionPreset::FullFrame,
            OverlayBorderPreset::None,
        )];
        let server = server();
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        let start = Instant::now();
        let frames = tap.poll_state(realized.clone(), &project, &server, start, true);
        let program = decode(&frames[1].1);
        assert_eq!(program.input, WireInputId::from_domain(input_id(1)));
        // The title's transparent background keeps the program picture.
        assert_rectangle(&program, TITLE_BAR, [200, 40, 10, 255], [7, 11, 13, 255]);

        // The overlaid picture is cached while nothing changes.
        assert_eq!(tap.title_renders, 1);
        let frames = tap.poll_state(
            realized,
            &project,
            &server,
            start + Duration::from_millis(200),
            true,
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(tap.title_renders, 1);

        // Taking the overlay off publishes the plain program promptly.
        let frames = tap.poll_state(
            state(1, 1, 2),
            &project,
            &server,
            start + Duration::from_millis(250),
            true,
        );
        let program = decode(&frames[1].1);
        assert_rectangle(&program, TITLE_BAR, [7, 11, 13, 255], [7, 11, 13, 255]);
    }

    #[test]
    fn overlay_borders_are_drawn_inside_the_overlay_box() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        let mut realized = state(1, 1, 2);
        realized.overlays = vec![overlay(
            1,
            255,
            OverlayPositionPreset::BottomLeft,
            OverlayBorderPreset::ThickWhite,
        )];
        // Program is bars; the solid overlay sits at (11, 229) sized 213x120
        // with a 2-pixel white inset border.
        realized.program = input_id(2);
        let mut tap = SimulatedMonitorTap::default();
        let frames = tap.poll_state(realized, &project, &server(), Instant::now(), true);
        let program = decode(&frames[1].1);
        assert_eq!(pixel(&program, 11, 229), [255, 255, 255, 255]);
        assert_eq!(pixel(&program, 12, 230), [255, 255, 255, 255]);
        assert_eq!(pixel(&program, 13, 231), [7, 11, 13, 255]);
        assert_eq!(pixel(&program, 222, 348), [255, 255, 255, 255]);
        assert_eq!(pixel(&program, 220, 346), [7, 11, 13, 255]);
        // Just outside the box is the bars' first column (75% white).
        assert_eq!(pixel(&program, 10, 300), [191, 191, 191, 255]);
    }

    #[test]
    fn encoded_groups_are_contiguous_part_lines() {
        let width = 640_u16;
        let height = 360_u16;
        // Incompressible noise forces a multi-part group.
        let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
        let mut seed = 0x1234_5678_u32;
        for _ in 0..usize::from(width) * usize::from(height) * 3 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            pixels.push(seed.to_le_bytes()[0]);
        }
        let picture = MonitorPicture {
            tap: MonitorTap::Program,
            sequence: 9,
            frame: 10,
            input: input_id(1),
            origin: MonitorFrameOrigin::Native,
            width,
            height,
            pixels: &pixels,
            stride: usize::from(width) * 3,
            bytes_per_pixel: 3,
        };
        let bytes = encode_picture(&server(), &picture).unwrap();
        let parts = LineDecoder::new().push(&bytes).unwrap().len();
        assert!(parts > 1, "noise should span several parts");
        let image = decode(&bytes);
        assert_eq!(image.sequence, 9);
        assert_eq!(&image.rgba[..3], &pixels[..3]);
        assert!(encode_group(Vec::new()).is_none());
    }

    #[test]
    fn fit_within_preserves_aspect_inside_the_monitor_cap() {
        assert_eq!(fit_monitor(1920, 1080), Some((640, 360)));
        assert_eq!(fit_monitor(1440, 1080), Some((480, 360)));
        assert_eq!(fit_monitor(1080, 1920), Some((202, 360)));
        assert_eq!(fit_monitor(3840, 2160), Some((640, 360)));
        assert_eq!(fit_monitor(320, 240), Some((320, 240)));
        assert_eq!(fit_monitor(640, 360), Some((640, 360)));
        assert_eq!(fit_monitor(7680, 1), Some((640, 1)));
        assert_eq!(fit_monitor(1, 4320), Some((1, 360)));
        assert_eq!(fit_monitor(0, 1080), None);
        assert_eq!(fit_within(100, 100, 10, 20), Some((10, 10)));
    }

    #[test]
    fn monitor_worker_never_holds_the_session_loop_during_a_composition() {
        let project = project(1_920, 1_080);
        let (rendering, rendering_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let mut blocked_once = false;
        let mut worker = SimulatedMonitorWorker::spawn_with(Box::new(move |work, _, emit| {
            let Some((state, _)) = work else {
                return;
            };
            let _ = rendering.send(state.revision);
            if !blocked_once {
                blocked_once = true;
                // Stands in for a slow Scene composition.
                release_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("the test releases the composition");
            }
            emit(MonitorTap::Program, state.revision.to_be_bytes().to_vec());
        }))
        .unwrap();

        assert_eq!(worker.poll_state(Some(state(1, 1, 2)), &project), NO_FRAMES);
        assert_eq!(rendering_rx.recv_timeout(Duration::from_secs(5)), Ok(1));
        // The composition of revision 1 is now blocked. Every poll returns at
        // once and the newest state replaces the older pending one.
        assert_eq!(worker.poll_state(Some(state(2, 2, 1)), &project), NO_FRAMES);
        assert_eq!(worker.poll_state(Some(state(3, 1, 2)), &project), NO_FRAMES);
        assert_eq!(rendering_rx.try_recv(), Err(mpsc::TryRecvError::Empty));

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut revisions = Vec::new();
        while !revisions.contains(&3) {
            assert!(
                Instant::now() < deadline,
                "worker never rendered the latest state"
            );
            revisions.extend(
                worker
                    .poll_state(Some(state(3, 1, 2)), &project)
                    .into_iter()
                    .map(|(_, bytes)| u64::from_be_bytes(bytes.try_into().unwrap())),
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !revisions.contains(&2),
            "a superseded state is never rendered"
        );
        // The renderer only ever saw the blocked state and the latest one.
        let rendered = rendering_rx.try_iter().collect::<Vec<_>>();
        assert!(rendered.iter().all(|revision| *revision == 3));
    }

    /// Regression: one cadence step rendered both taps before publishing
    /// either, and finished groups shared one small backlog, so a slow
    /// Program Scene composition held back (or crowded out) the Preview
    /// picture of the same step.
    #[test]
    fn preview_frames_are_never_held_behind_a_program_composition() {
        let project = project(1_920, 1_080);
        let (program_started, program_started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let mut worker = SimulatedMonitorWorker::spawn_with(Box::new(move |work, _, emit| {
            let Some((state, _)) = work else {
                return;
            };
            emit(
                MonitorTap::Preview,
                vec![b'p', state.revision.to_be_bytes()[7]],
            );
            let _ = program_started.send(state.revision);
            // Every Program composition is slow until released.
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
            emit(
                MonitorTap::Program,
                vec![b'P', state.revision.to_be_bytes()[7]],
            );
        }))
        .unwrap();

        assert_eq!(worker.poll_state(Some(state(1, 1, 2)), &project), NO_FRAMES);
        assert_eq!(
            program_started_rx.recv_timeout(Duration::from_secs(5)),
            Ok(1)
        );
        // Program is still composing, yet the Preview group is already out.
        assert_eq!(
            worker.poll_state(Some(state(1, 1, 2)), &project),
            vec![(MonitorTap::Preview, vec![b'p', 1])]
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "Program never arrived");
            let frames = worker.poll_state(Some(state(1, 1, 2)), &project);
            if frames.contains(&(MonitorTap::Program, vec![b'P', 1])) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        // Releasing every later composition lets the worker shut down.
        drop(release);
    }

    /// Scenes covering crops, masks, rotation, opacity, partial and
    /// off-frame layers, thin layers, and both simulated patterns, laid out
    /// for a 1920x1080 canvas and stretched onto the project's own size.
    fn equivalence_scenes(project: &mut Project) -> Vec<u128> {
        let dimensions = project.settings().video.dimensions;
        let (width, height) = (dimensions.width(), dimensions.height());
        let x = |value: u32| value * width / 1_920;
        let y = |value: u32| value * height / 1_080;
        let left = |value: i32| value * i32::try_from(width).unwrap() / 1_920;
        let top = |value: i32| value * i32::try_from(height).unwrap() / 1_080;
        let mut cropped = layer(
            SourceRef::Input(input_id(2)),
            left(101),
            top(57),
            x(777),
            y(433),
        );
        cropped.crop = Some(fm_model::CropRect::new(x(300), y(120), x(900), y(600)));
        let mut masked = layer(
            SourceRef::Input(input_id(1)),
            left(640),
            top(360),
            x(960),
            y(540),
        );
        masked.crop = Some(fm_model::CropRect::new(x(10), y(20), x(1_500), y(900)));
        masked.mask = Some(fm_model::RectMask {
            x: x(211),
            y: y(97),
            width: x(1_003),
            height: y(377),
            invert: true,
        });
        masked.opacity = 160;
        masked.z_order = 2;
        let mut rotated = layer(
            SourceRef::Input(input_id(2)),
            left(-150),
            top(400),
            x(641),
            y(359),
        );
        rotated.geometry.rotation = fm_model::Rotation::Deg90;
        rotated.z_order = 3;
        let mut upside_down = layer(
            SourceRef::Input(input_id(2)),
            left(1_700),
            top(-90),
            x(400),
            y(300),
        );
        upside_down.geometry.rotation = fm_model::Rotation::Deg180;
        let thin = layer(SourceRef::Input(input_id(1)), left(913), 0, 1, height);
        add_scene(
            project,
            1,
            10,
            vec![layer(SourceRef::Input(input_id(2)), 0, 0, width, height)],
        );
        add_scene(
            project,
            2,
            11,
            vec![cropped, masked, rotated, upside_down, thin],
        );
        vec![10, 11]
    }

    /// Compares the monitor-size composition with the project-size reference
    /// it replaced (compose at project size, then nearest-neighbor
    /// downscale). Pictures may differ only along resampled edges.
    #[test]
    fn monitor_size_scenes_match_the_project_size_reference() {
        // Integer (1080p), portrait, and non-integer (1000x750) ratios.
        for (width, height) in [(1_920, 1_080), (1_080, 1_920), (1_000, 750)] {
            let mut project = project(width, height);
            let scenes = equivalence_scenes(&mut project);
            let (monitor_width, monitor_height) = fit_monitor(width, height).unwrap();
            let (monitor_width, monitor_height) =
                (u32::from(monitor_width), u32::from(monitor_height));
            let monitor = simulated_pipeline(&project).unwrap();
            let full = simulated_pipeline_at(&project, width, height).unwrap();
            for input in scenes {
                let scene = scene_render::scene_of_input(&project, input_id(input)).unwrap();
                let fast = scene_render::render_scene(
                    &project,
                    input_id(input),
                    &monitor,
                    4,
                    AnimSample::Hold,
                    &CompiledProjectGrades::default(),
                )
                .unwrap();
                let reference = scene_render::render_scene_at_project_size(
                    &project,
                    scene,
                    &full,
                    4,
                    monitor_width,
                    monitor_height,
                    AnimSample::Hold,
                )
                .unwrap();
                assert_eq!(
                    (fast.width(), fast.height()),
                    (reference.width(), reference.height())
                );
                let mut differing = 0_u64;
                let mut total_error = 0_u64;
                for y in 0..monitor_height {
                    for x in 0..monitor_width {
                        let a = fast.pixel(x, y).unwrap();
                        let b = reference.pixel(x, y).unwrap();
                        let error = [
                            a.r.abs_diff(b.r),
                            a.g.abs_diff(b.g),
                            a.b.abs_diff(b.b),
                            a.a.abs_diff(b.a),
                        ];
                        if error.iter().any(|channel| *channel != 0) {
                            differing += 1;
                        }
                        total_error += error.iter().map(|channel| u64::from(*channel)).sum::<u64>();
                    }
                }
                let pixels = u64::from(monitor_width * monitor_height);
                // At most 1% of pixels (edge columns/rows) differ, and the mean
                // per-channel error stays under one code value.
                assert!(
                    differing * 100 <= pixels,
                    "{width}x{height} scene {input}: {differing} of {pixels} pixels differ"
                );
                assert!(
                    total_error < pixels * 4,
                    "{width}x{height} scene {input}: total error {total_error} over {pixels} pixels"
                );
            }
        }
    }

    /// A key's pixel-sized reaches are authored in project pixels: on a
    /// one-third-size monitor a light wrap 8 pixels wide reaches as far into
    /// the picture as Program's (downscaled) does, not three times further.
    #[test]
    fn monitor_size_key_reaches_scale_with_the_grid() {
        let mut project = project(1_920, 1_080);
        let bars = layer(SourceRef::Input(input_id(2)), 0, 0, 1_920, 1_080);
        let mut keyed = layer(SourceRef::Input(input_id(1)), 960, 0, 960, 1_080);
        keyed.z_order = 1;
        keyed.key = Some(fm_model::LayerKey::Luma(fm_model::LumaLayerKey {
            low: 0,
            high: 255,
            softness: 0,
            invert: false,
            refinement: fm_model::KeyRefinement {
                light_wrap: 255,
                light_wrap_width: 8,
                ..fm_model::KeyRefinement::NONE
            },
        }));
        add_scene(&mut project, 1, 10, vec![bars, keyed]);
        let scene = scene_render::scene_of_input(&project, input_id(10)).unwrap();
        let monitor = simulated_pipeline(&project).unwrap();
        let full = simulated_pipeline_at(&project, 1_920, 1_080).unwrap();
        let fast = scene_render::render_scene(
            &project,
            input_id(10),
            &monitor,
            4,
            AnimSample::Hold,
            &CompiledProjectGrades::default(),
        )
        .unwrap();
        let reference = scene_render::render_scene_at_project_size(
            &project,
            scene,
            &full,
            4,
            fast.width(),
            fast.height(),
            AnimSample::Hold,
        )
        .unwrap();
        assert_eq!((fast.width(), fast.height()), (640, 360));
        // Columns from the layer's left edge (monitor column 320) whose
        // solid colour the wrap changed.
        let reach = |image: &ImageFrame| {
            (320..640)
                .take_while(|&x| {
                    let pixel = image.pixel(x, 180).unwrap();
                    (pixel.r, pixel.g, pixel.b) != (7, 11, 13)
                })
                .count()
        };
        assert_eq!(reach(&reference), 3);
        assert_eq!(reach(&fast), reach(&reference));
    }

    /// Regression for multi-second debug-build redraws: a 1080p Scene redraw
    /// does a fraction of the project-size composition's work.
    #[test]
    fn monitor_size_scene_redraw_is_much_cheaper_than_project_size() {
        let mut project = project(1_920, 1_080);
        let scenes = equivalence_scenes(&mut project);
        let scene = scene_render::scene_of_input(&project, input_id(scenes[1])).unwrap();
        let monitor = simulated_pipeline(&project).unwrap();
        let full = simulated_pipeline_at(&project, 1_920, 1_080).unwrap();
        let started = Instant::now();
        scene_render::render_scene(
            &project,
            input_id(scenes[1]),
            &monitor,
            4,
            AnimSample::Hold,
            &CompiledProjectGrades::default(),
        )
        .unwrap();
        let fast = started.elapsed();
        let started = Instant::now();
        scene_render::render_scene_at_project_size(
            &project,
            scene,
            &full,
            4,
            640,
            360,
            AnimSample::Hold,
        )
        .unwrap();
        let reference = started.elapsed();
        // The pixel work is ~9x smaller; demand a conservative 3x.
        assert!(
            fast * 3 < reference,
            "monitor-size redraw took {fast:?}, project-size {reference:?}"
        );
    }

    fn exposure(millistops: i16) -> fm_grade::Grade {
        fm_grade::Grade {
            exposure_millistops: millistops,
            ..fm_grade::Grade::IDENTITY
        }
    }

    fn graded(grade: &fm_grade::Grade, rgb: [u8; 3]) -> [u8; 4] {
        let [red, green, blue] = fm_grade::CompiledGrade::new(grade, None)
            .unwrap()
            .apply_rgb8(rgb);
        [red, green, blue, 255]
    }

    fn compile_grades(tap: &mut SimulatedMonitorTap, project: &Project) {
        tap.grades = Arc::new(CompiledProjectGrades::compile(project, &BTreeMap::new()).unwrap());
    }

    /// SW-019: a scene layer shows its source's input grade and then its own
    /// layer grade, like the native compositor; the background is ungraded.
    #[test]
    fn scene_layers_carry_their_input_grade_then_their_layer_grade() {
        let mut project = project(1_920, 1_080);
        let quarter = layer(SourceRef::Input(input_id(1)), 0, 0, 960, 540);
        let target =
            fm_model::SceneLayerRef::new(SceneId::new(NonZeroU128::new(1).unwrap()), quarter.id);
        add_scene(&mut project, 1, 10, vec![quarter]);
        let warm = fm_grade::builtin_preset("warm").unwrap().grade.clone();
        project
            .set_input_grade(
                input_id(1),
                Some(fm_model::GradeSource::Inline(Box::new(warm.clone()))),
            )
            .unwrap();
        project
            .set_layer_grade(
                target,
                Some(fm_model::GradeSource::Inline(Box::new(exposure(-1_000)))),
            )
            .unwrap();
        let mut tap = SimulatedMonitorTap::default();
        compile_grades(&mut tap, &project);
        let frames = tap.poll_state(state(1, 10, 1), &project, &server(), Instant::now(), true);
        let program = decode(&frames[1].1);
        let [red, green, blue, _] = graded(&warm, [7, 11, 13]);
        let expected = graded(&exposure(-1_000), [red, green, blue]);
        assert_ne!(expected, [7, 11, 13, 255]);
        assert_rectangle(&program, (0, 0, 320, 180), expected, [20, 30, 40, 255]);
        // Preview shows input 1 with only its input grade.
        let preview = decode(&frames[0].1);
        assert_eq!(pixel(&preview, 5, 5), graded(&warm, [7, 11, 13]));
    }

    /// SW-019: a Scene input's own grade colours the composed scene it
    /// shows (background included) after its layers' grades, on Program and
    /// as an overlay over Program.
    #[test]
    fn scene_input_grade_colours_the_composed_scene() {
        let mut project = project(1_920, 1_080);
        let quarter = layer(SourceRef::Input(input_id(1)), 0, 0, 960, 540);
        let target =
            fm_model::SceneLayerRef::new(SceneId::new(NonZeroU128::new(1).unwrap()), quarter.id);
        add_scene(&mut project, 1, 10, vec![quarter]);
        project
            .set_layer_grade(
                target,
                Some(fm_model::GradeSource::Inline(Box::new(exposure(-1_000)))),
            )
            .unwrap();
        project
            .set_input_grade(
                input_id(10),
                Some(fm_model::GradeSource::Inline(Box::new(exposure(500)))),
            )
            .unwrap();
        let mut tap = SimulatedMonitorTap::default();
        compile_grades(&mut tap, &project);
        let frames = tap.poll_state(state(1, 10, 1), &project, &server(), Instant::now(), true);
        let [red, green, blue, _] = graded(&exposure(-1_000), [7, 11, 13]);
        let layer = graded(&exposure(500), [red, green, blue]);
        let background = graded(&exposure(500), [20, 30, 40]);
        assert_ne!(background, [20, 30, 40, 255]);
        assert_rectangle(&decode(&frames[1].1), (0, 0, 320, 180), layer, background);

        // As a full-frame overlay the Scene input shows graded the same way.
        let mut realized = state(2, 1, 1);
        realized.overlays = vec![overlay(
            10,
            255,
            OverlayPositionPreset::FullFrame,
            OverlayBorderPreset::None,
        )];
        let frames = tap.poll_state(
            realized,
            &project,
            &server(),
            Instant::now() + Duration::from_millis(250),
            true,
        );
        assert_rectangle(&decode(&frames[1].1), (0, 0, 320, 180), layer, background);
    }

    /// SW-019: a title input's picture carries its input grade, flat on
    /// Program and Preview as keyed over Program.
    #[test]
    fn title_pictures_and_keyed_title_overlays_carry_their_input_grade() {
        let mut project = project(1_920, 1_080);
        add_title(&mut project, 20, TITLE_URI, (200, 40, 10));
        project
            .set_input_grade(
                input_id(20),
                Some(fm_model::GradeSource::Inline(Box::new(exposure(-1_000)))),
            )
            .unwrap();
        let darker = graded(&exposure(-1_000), [200, 40, 10]);
        assert_ne!(darker, [200, 40, 10, 255]);
        let server = server();
        let mut tap = SimulatedMonitorTap::with_titles(title_assets());
        compile_grades(&mut tap, &project);
        let frames = tap.poll_state(state(1, 20, 1), &project, &server, Instant::now(), true);
        assert_rectangle(&decode(&frames[1].1), TITLE_BAR, darker, [0, 0, 0, 255]);

        let mut realized = state(2, 1, 2);
        realized.overlays = vec![overlay(
            20,
            255,
            OverlayPositionPreset::FullFrame,
            OverlayBorderPreset::None,
        )];
        let frames = tap.poll_state(
            realized,
            &project,
            &server,
            Instant::now() + Duration::from_millis(250),
            true,
        );
        // The transparent background stays transparent under the grade.
        assert_rectangle(&decode(&frames[1].1), TITLE_BAR, darker, [7, 11, 13, 255]);
    }
}
