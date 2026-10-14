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
//! reference compositor at the project size and downscaled to the fitted
//! monitor size; each composition is cached per realized revision, frame
//! cursor, and input so the steady cadence never recomposes an unchanged
//! picture. Any other realized input (or a scene the CPU reference cannot
//! render honestly) produces no frame, so a client keeps its placeholder
//! instead of seeing invented video.

use std::sync::Arc;
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
    pipeline: Option<SimulatedPipeline>,
    built_revision: Option<u64>,
    /// Project-size pipeline for scene composition, built only on demand.
    scene_pipeline: Option<SimulatedPipeline>,
    scene_built_revision: Option<u64>,
    composed: [Option<ComposedScene>; 2],
    /// Scene compositions performed, observed by tests to prove caching.
    #[cfg(test)]
    compositions: u64,
    last_key: Option<(u64, InputId, InputId)>,
    last_emit: Option<Instant>,
    sequences: [u64; 2],
}

impl SimulatedMonitorTap {
    /// Returns each tap's encoded frame group due at `now`, if any.
    pub(crate) fn poll(
        &mut self,
        control: &ControlService<Policy>,
        project: &Project,
        server: &ServerIdentity,
        now: Instant,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        if !demand {
            self.idle();
            return Vec::new();
        }
        self.poll_state(
            RealizedMonitorState::from_control(control),
            project,
            server,
            now,
            true,
        )
    }

    /// Cadence and rendering against an explicit realized state.
    pub(crate) fn poll_state(
        &mut self,
        state: RealizedMonitorState,
        project: &Project,
        server: &ServerIdentity,
        now: Instant,
        demand: bool,
    ) -> Vec<(MonitorTap, Vec<u8>)> {
        if !demand {
            self.idle();
            return Vec::new();
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
            return Vec::new();
        }
        self.last_emit = Some(now);
        self.last_key = Some(state.change_key());
        let Some((width, height)) = self.pipeline.as_ref().and_then(|pipeline| {
            Some((
                u16::try_from(pipeline.width()).ok()?,
                u16::try_from(pipeline.height()).ok()?,
            ))
        }) else {
            return Vec::new();
        };
        let mut frames = Vec::with_capacity(MonitorTap::ALL.len());
        for tap in MonitorTap::ALL {
            let input = state.input(tap);
            let image = if scene_render::scene_of_input(project, input).is_some() {
                self.scene_frame(tap, state, project, width, height)
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
                frames.push((tap, bytes));
            }
        }
        frames
    }

    /// Returns the tap's scene picture at the fitted size, composing it only
    /// when no cached composition matches the realized revision, frame
    /// cursor, and input. The other tap's matching composition is shared.
    fn scene_frame(
        &mut self,
        tap: MonitorTap,
        state: RealizedMonitorState,
        project: &Project,
        width: u16,
        height: u16,
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
        if self.scene_built_revision != Some(state.revision) {
            self.scene_pipeline = simulated_pipeline_at(
                project,
                project.settings().video.dimensions.width(),
                project.settings().video.dimensions.height(),
            );
            self.scene_built_revision = Some(state.revision);
        }
        #[cfg(test)]
        {
            self.compositions += 1;
        }
        let image = scene_render::scene_of_input(project, key.input).and_then(|scene| {
            scene_render::render_scene(
                project,
                scene,
                self.scene_pipeline.as_ref()?,
                state.frame,
                u32::from(width),
                u32::from(height),
            )
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
    fn scene_layers_compose_at_project_size_and_scale_to_the_monitor() {
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
}
