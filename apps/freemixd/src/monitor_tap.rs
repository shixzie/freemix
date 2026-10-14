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
//! an unchanged picture. Any other realized input (or a scene the CPU reference cannot
//! render honestly) produces no frame, so a client keeps its placeholder
//! instead of seeing invented video.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_auth::Policy;
use fm_control::ControlService;
use fm_model::{InputKind, Project, SimulatedVideo};
use fm_protocol::{
    MAX_MONITOR_FRAME_HEIGHT, MAX_MONITOR_FRAME_WIDTH, MonitorFrameMessage, MonitorFrameOrigin,
    MonitorTap, ServerIdentity, WireInputId, WireMessage, encode_line, encode_qoi_rgb,
    monitor_frame_parts,
};
use fm_sim::{ImageFrame, Rgba8, SimulatedPipeline, SimulatedSource, SourcePattern};
use fm_types::InputId;

use crate::scene_render;

/// The order taps render within one cadence step: the cheap, latency-visible
/// Preview first, so it is never queued behind a Program Scene composition.
const MONITOR_TAP_ORDER: [MonitorTap; 2] = [MonitorTap::Preview, MonitorTap::Program];

/// Steady simulated monitor cadence (5 fps).
pub(crate) const SIMULATED_MONITOR_INTERVAL: Duration = Duration::from_millis(200);
/// Smallest gap between two simulated frames, even after a state change.
pub(crate) const SIMULATED_MONITOR_MIN_GAP: Duration = Duration::from_millis(50);

/// Scales `width`x`height` down (never up) to fit `max_width`x`max_height`
/// while preserving aspect ratio. The scaled side is floored and every side
/// is at least one pixel. Returns `None` for an empty source.
pub(crate) fn fit_within(
    width: u32,
    height: u32,
    max_width: u16,
    max_height: u16,
) -> Option<(u16, u16)> {
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
pub(crate) fn fit_monitor(width: u32, height: u32) -> Option<(u16, u16)> {
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
pub(crate) fn encode_group(messages: Vec<MonitorFrameMessage>) -> Option<Vec<u8>> {
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
pub(crate) struct MonitorPicture<'a> {
    pub(crate) tap: MonitorTap,
    pub(crate) sequence: u64,
    pub(crate) frame: u64,
    pub(crate) input: InputId,
    pub(crate) origin: MonitorFrameOrigin,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) pixels: &'a [u8],
    pub(crate) stride: usize,
    pub(crate) bytes_per_pixel: usize,
}

/// QOI-encodes one picture and returns its atomic `monitor_frame` group.
pub(crate) fn encode_picture(
    server: &ServerIdentity,
    picture: &MonitorPicture<'_>,
) -> Option<Vec<u8>> {
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

/// The realized state a simulated monitor frame depicts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RealizedMonitorState {
    pub(crate) revision: u64,
    pub(crate) frame: u64,
    pub(crate) program: InputId,
    pub(crate) preview: InputId,
}

impl RealizedMonitorState {
    pub(crate) fn from_control(control: &ControlService<Policy>) -> Self {
        let snapshot = &control.snapshot().snapshot;
        Self {
            revision: snapshot.revision,
            frame: control.frame_cursor(),
            program: snapshot.realized_program.to_domain(),
            preview: snapshot.realized_preview.to_domain(),
        }
    }

    const fn input(self, tap: MonitorTap) -> InputId {
        match tap {
            MonitorTap::Preview => self.preview,
            MonitorTap::Program => self.program,
        }
    }

    /// The fields whose change publishes a frame immediately; the frame
    /// cursor alone only advances on the steady cadence.
    const fn change_key(self) -> (u64, InputId, InputId) {
        (self.revision, self.program, self.preview)
    }
}

/// Identity of one composed scene picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ComposeKey {
    revision: u64,
    frame: u64,
    input: InputId,
}

/// One tap's most recent scene composition; `image` is `None` when the scene
/// could not be rendered, so a failing scene is not retried every poll.
struct ComposedScene {
    key: ComposeKey,
    image: Option<Arc<ImageFrame>>,
}

/// Renders realized Preview and Program from simulated inputs on a bounded
/// wall-clock cadence while at least one eligible peer wants frames.
#[derive(Default)]
pub(crate) struct SimulatedMonitorTap {
    /// Every simulated input at the fitted monitor size; scenes compose
    /// from these same sources.
    pipeline: Option<SimulatedPipeline>,
    built_revision: Option<u64>,
    composed: [Option<ComposedScene>; 2],
    /// Scene compositions performed, observed by tests to prove caching.
    #[cfg(test)]
    compositions: u64,
    last_key: Option<(u64, InputId, InputId)>,
    last_emit: Option<Instant>,
    sequences: [u64; 2],
}

impl SimulatedMonitorTap {
    /// Cadence and rendering against an explicit realized state.
    #[cfg(test)]
    pub(crate) fn poll_state(
        &mut self,
        state: RealizedMonitorState,
        project: &Project,
        server: &ServerIdentity,
        now: Instant,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let mut frames = Vec::new();
        self.poll_state_into(state, project, server, now, demand, &mut |tap, bytes| {
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
    pub(crate) fn poll_state_into(
        &mut self,
        state: RealizedMonitorState,
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
        let changed = self.last_key != Some(state.change_key());
        let due = self.last_emit.is_none_or(|last| {
            let elapsed = now.saturating_duration_since(last);
            elapsed >= SIMULATED_MONITOR_INTERVAL
                || (changed && elapsed >= SIMULATED_MONITOR_MIN_GAP)
        });
        if !due {
            return;
        }
        self.last_emit = Some(now);
        self.last_key = Some(state.change_key());
        let Some((width, height)) = self.pipeline.as_ref().and_then(|pipeline| {
            Some((
                u16::try_from(pipeline.width()).ok()?,
                u16::try_from(pipeline.height()).ok()?,
            ))
        }) else {
            return;
        };
        for tap in MONITOR_TAP_ORDER {
            let input = state.input(tap);
            let image = if scene_render::scene_of_input(project, input).is_some() {
                self.scene_frame(tap, state, project)
            } else {
                // Non-simulated inputs are never registered, so they render nothing.
                self.pipeline
                    .as_ref()
                    .and_then(|pipeline| pipeline.render_source(input, state.frame).ok())
                    .map(Arc::new)
            };
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
    }

    /// Returns the tap's scene picture at the fitted size, composing it only
    /// when no cached composition matches the realized revision, frame
    /// cursor, and input. The other tap's matching composition is shared.
    fn scene_frame(
        &mut self,
        tap: MonitorTap,
        state: RealizedMonitorState,
        project: &Project,
    ) -> Option<Arc<ImageFrame>> {
        let key = ComposeKey {
            revision: state.revision,
            frame: state.frame,
            input: state.input(tap),
        };
        if let Some(cached) = self
            .composed
            .iter()
            .flatten()
            .find(|composed| composed.key == key)
        {
            let image = cached.image.clone();
            self.composed[tap.index()] = Some(ComposedScene {
                key,
                image: image.clone(),
            });
            return image;
        }
        #[cfg(test)]
        {
            self.compositions += 1;
        }
        let image = scene_render::scene_of_input(project, key.input).and_then(|scene| {
            scene_render::render_scene(project, scene, self.pipeline.as_ref()?, state.frame)
        });
        let image = image.map(Arc::new);
        self.composed[tap.index()] = Some(ComposedScene {
            key,
            image: image.clone(),
        });
        image
    }

    /// Without demand nothing renders; the next demanded poll is immediate.
    fn idle(&mut self) {
        self.last_emit = None;
        self.last_key = None;
    }
}

/// What the composition worker renders: the realized state and the project
/// it depicts, or `None` when no peer wants frames.
type MonitorWork = Option<(RealizedMonitorState, Arc<Project>)>;

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
pub(crate) struct SimulatedMonitorWorker {
    shared: Arc<WorkerShared>,
    worker: Option<JoinHandle<()>>,
    project: Option<(u64, Arc<Project>)>,
    published: Published,
}

/// The last work handed to the worker, so unchanged polls skip the lock.
#[derive(Clone, Copy, Eq, PartialEq)]
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
    pub(crate) fn spawn(server: ServerIdentity) -> std::io::Result<Self> {
        let mut tap = SimulatedMonitorTap::default();
        Self::spawn_with(Box::new(move |work, now, emit| {
            if let Some((state, project)) = work {
                tap.poll_state_into(state, &project, &server, now, true, emit);
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
        })
    }

    /// Publishes the current realized state when it changed and returns the
    /// encoded groups the worker has finished since the last call. Never
    /// waits for a composition.
    pub(crate) fn poll(
        &mut self,
        control: &ControlService<Policy>,
        project: &Project,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let state = demand.then(|| RealizedMonitorState::from_control(control));
        self.poll_state(state, project)
    }

    fn poll_state(
        &mut self,
        state: Option<RealizedMonitorState>,
        project: &Project,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        let published = state.map_or(Published::Idle, Published::State);
        let work = (self.published != published)
            .then(|| state.map(|state| (state, self.project_at(state.revision, project))));
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

    /// Shares one project copy per realized revision with the worker.
    fn project_at(&mut self, revision: u64, project: &Project) -> Arc<Project> {
        match &self.project {
            Some((cached, shared)) if *cached == revision => Arc::clone(shared),
            _ => {
                let shared = Arc::new(project.clone());
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
            name: "Layer".into(),
            source,
            enabled: true,
            geometry: LayerGeometry::new(x, y, width, height, Rotation::Deg0),
            crop: None,
            mask: None,
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
        // Seven equal bars across 640 pixels; sample the middle of each.
        let bars = [
            [191, 191, 191, 255],
            [191, 191, 0, 255],
            [0, 191, 191, 255],
            [0, 191, 0, 255],
            [191, 0, 191, 255],
            [191, 0, 0, 255],
            [0, 0, 191, 255],
        ];
        for (bar, expected) in bars.iter().enumerate() {
            let x = (bar * 640 + 320) / 7;
            assert_eq!(pixel(&preview, x, 0), *expected, "bar {bar}");
        }
        // The one-pixel frame marker sits on the last row at frame % width.
        assert_eq!(pixel(&preview, 4, 359), [0, 0, 0, 255]);

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
                let fast = scene_render::render_scene(&project, scene, &monitor, 4).unwrap();
                let reference = scene_render::render_scene_at_project_size(
                    &project,
                    scene,
                    &full,
                    4,
                    monitor_width,
                    monitor_height,
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
        scene_render::render_scene(&project, scene, &monitor, 4).unwrap();
        let fast = started.elapsed();
        let started = Instant::now();
        scene_render::render_scene_at_project_size(&project, scene, &full, 4, 640, 360).unwrap();
        let reference = started.elapsed();
        // The pixel work is ~9x smaller; demand a conservative 3x.
        assert!(
            fast * 3 < reference,
            "monitor-size redraw took {fast:?}, project-size {reference:?}"
        );
    }
}
