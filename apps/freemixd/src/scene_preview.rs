//! Exact scene previews (GX-011, Protocol 2.32).
//!
//! A `scene_preview_request` asks the authority to compose one Scene input
//! exactly as it shows it: the scene the engine holds now (or with the
//! request's unsaved draft clip instead of its animation), posed at one
//! point of its animation ([`fm_model::Scene::posed`], arbitrary rotation
//! included), composed by the same CPU reference compositor and project-to-
//! grid mapping the simulated monitor tap uses
//! ([`scene_render::render_scene_model`]) directly at the requested size.
//! Nested scenes (a layer naming a scene, directly or through a Scene
//! input) are composed on their own canvas in their Hold pose, within
//! [`scene_render::MAX_NESTED_SCENE_DEPTH`] levels and
//! [`scene_render::MAX_NESTED_SCENES`] scenes.
//!
//! Leaf pictures depend on the daemon:
//!
//! - A simulated daemon draws the project's simulated sources at the engine
//!   frame cursor.
//! - A native daemon draws the newest CPU frame its media pipeline holds for
//!   each source ([`LiveSourceFrames`], snapshotted on the session loop when
//!   the request is resolved: shared frames, no copy, no GPU work, nothing
//!   that waits on the output clock): the last frame of each decoded window
//!   (at most the source ring, eight frames by default, ahead of air), the
//!   last ingested live frame (titles, cameras), or the retained frame
//!   (stills, simulated generators). Off the loop, each is scaled
//!   nearest-neighbor onto the preview grid and converted from its labeled
//!   colorimetry to the sRGB Rec.709 full-range picture the native Program
//!   readback produces, through the `fm-color` CPU reference. The CPU
//!   compositor then composes those 8-bit pictures, so a preview matches the
//!   native GPU composition (linear light, 16-bit float) within the
//!   difference between those two pipelines, not bit for bit.
//!
//! Both daemons grade the preview as their monitors and the native Program
//! do (SW-019): each leaf layer's input grade, then each layer's own scene
//! layer grade, applied by the compositor after the layer's effects and
//! before its transform, from the grades of the engine revision the
//! request was resolved at. A layer drawing a nested scene through a Scene
//! input carries that input's grade and then its layer grade (one naming
//! the scene directly only its layer grade); the nested scene's layers
//! carry their own. The requested Scene input's own grade then colours the
//! composed preview, as it colours that input on the monitors.
//!
//! The answer is one four-channel QOI stream of straight RGBA8 pixels (at
//! most 640x360) split into `scene_preview_frame` parts, rendered off the
//! session loop by the same latest-wins worker, lossy slot, and per-session
//! preview rate as title previews ([`crate::title_preview`]). A scene the
//! reference compositor cannot draw honestly (a layer naming a source with
//! no picture, nesting past the bounds, an effect it rejects) is refused
//! with `render_failed` rather than approximated.

#[cfg(feature = "native-media")]
use std::collections::BTreeMap;
use std::sync::Arc;

use fm_auth::Policy;
use fm_control::{ControlService, scene_clip_from_wire};
use fm_engine::CompiledProjectGrades;
use fm_model::fm_animation::ClipState;
use fm_model::{Input, Project};
use fm_protocol::{
    ScenePreviewFrameMessage, ScenePreviewRequest, ServerIdentity, StructuredError, WireInputId,
    encode_qoi_rgba, title_preview_parts,
};
use fm_sim::SimulatedPipeline;
use fm_types::InputId;
use fm_video::ImageFrame;

use crate::scene_render::{self, SceneSources};
use crate::title_preview::{refusal, sample_pose};

/// The newest frame of each source a native daemon's media pipeline holds,
/// snapshotted for one preview (see the module documentation). Empty on a
/// build without native media.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSourceFrames {
    #[cfg(feature = "native-media")]
    frames: BTreeMap<InputId, Arc<fm_frame::CpuVideoFrame>>,
}

impl LiveSourceFrames {
    /// A snapshot of `latest` as of now.
    #[cfg(feature = "native-media")]
    pub(crate) fn snapshot(latest: &freemixd::native_media::NativeLatestFrames) -> Self {
        Self {
            frames: latest.snapshot(),
        }
    }

    /// The snapshot of exactly `frames`.
    #[cfg(all(test, feature = "native-media"))]
    pub(crate) fn from_frames(frames: BTreeMap<InputId, Arc<fm_frame::CpuVideoFrame>>) -> Self {
        Self { frames }
    }

    #[cfg(feature = "native-media")]
    fn holds(&self, input: InputId) -> bool {
        self.frames.contains_key(&input)
    }

    #[cfg(not(feature = "native-media"))]
    #[allow(clippy::unused_self)]
    const fn holds(&self, _input: InputId) -> bool {
        false
    }

    #[cfg(feature = "native-media")]
    fn picture(&self, input: InputId, width: u32, height: u32) -> Option<ImageFrame> {
        live_picture(self.frames.get(&input)?, width, height)
    }

    #[cfg(not(feature = "native-media"))]
    #[allow(clippy::unused_self)]
    const fn picture(&self, _input: InputId, _width: u32, _height: u32) -> Option<ImageFrame> {
        None
    }
}

/// A native daemon's leaves: each source's newest frame, and the simulated
/// pipeline for a simulated input the snapshot lacks.
struct LiveSources<'a> {
    live: &'a LiveSourceFrames,
    simulated: &'a SimulatedPipeline,
}

impl SceneSources for LiveSources<'_> {
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
            self.live.picture(input.id, width, height)
        } else {
            self.simulated.leaf(input, frame_number, width, height)
        }
    }
}

/// One labeled straight-alpha RGBA8/BGRA8 source frame as the straight
/// RGBA8 sRGB Rec.709 full-range picture a scene layer reads, scaled
/// nearest-neighbor to `width`x`height`. `None` for a frame the native
/// importer would not accept (unlabeled, not packed RGBA).
#[cfg(feature = "native-media")]
fn live_picture(frame: &fm_frame::CpuVideoFrame, width: u32, height: u32) -> Option<ImageFrame> {
    use fm_types::{
        AlphaMode, ColorMetadata, ColorPrimaries, MatrixCoefficients, SignalRange, TransferFunction,
    };

    let metadata = frame.metadata()?;
    let image = fm_compositor::image_from_cpu_frame(frame).ok()?;
    let scaled = if (image.width(), image.height()) == (width, height) {
        image
    } else {
        fm_video::scale_nearest(&image, width, height).ok()?
    };
    let display = ColorMetadata {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferFunction::Srgb,
        matrix: MatrixCoefficients::Identity,
        range: SignalRange::Full,
        ..metadata.color()
    };
    let alpha = metadata.alpha_mode().unwrap_or(AlphaMode::Straight);
    if metadata.color() == display && alpha == AlphaMode::Straight {
        return Some(scaled);
    }
    let mut pipeline = fm_color::ColorPipeline::new(metadata.color(), display);
    pipeline.source_alpha = alpha;
    pipeline.output_alpha = AlphaMode::Straight;
    pipeline
        .convert_image(&scaled)
        .ok()
        .map(|converted| converted.image)
}

/// One exact scene preview, resolved on the session loop and composed off
/// it ([`ScenePreviewJob::render`]).
pub(crate) struct ScenePreviewJob {
    server: ServerIdentity,
    pub(crate) request_id: String,
    wire_input: WireInputId,
    input: InputId,
    /// The project whose inputs feed the layers.
    project: Project,
    /// The scene as the engine holds it, with any draft clip.
    scene: fm_model::Scene,
    pose: ClipState,
    frame: u64,
    /// The engine's compiled grades when the request was resolved.
    grades: Arc<CompiledProjectGrades>,
    /// The native pipeline's newest source frames; `None` on a simulated
    /// daemon, whose leaves are its simulated sources.
    live: Option<LiveSourceFrames>,
    animation_revision: u64,
    draft: bool,
    width: u16,
    height: u16,
}

impl ScenePreviewJob {
    /// Composes the preview and splits it into the `scene_preview_frame`
    /// parts its server sends.
    ///
    /// # Errors
    ///
    /// A `render_failed` refusal when the scene cannot be composed exactly
    /// or its encoding fails.
    pub(crate) fn render(&self) -> Result<Vec<ScenePreviewFrameMessage>, StructuredError> {
        let qoi = self.compose()?;
        Ok(title_preview_parts(
            &self.server,
            &self.request_id,
            self.wire_input,
            self.animation_revision,
            self.draft,
            self.width,
            self.height,
            &qoi,
        ))
    }

    /// The composed preview as one QOI stream.
    fn compose(&self) -> Result<Vec<u8>, StructuredError> {
        let input = self.input;
        let unrenderable = || {
            refusal(
                "render_failed",
                format!(
                    "scene {input} cannot be composed exactly (a layer's source has no picture \
                     on this daemon, or its nesting or effects are beyond the reference \
                     compositor)"
                ),
            )
        };
        let size = (u32::from(self.width), u32::from(self.height));
        let simulated = scene_render::simulated_pipeline(&self.project, size.0, size.1)
            .ok_or_else(unrenderable)?;
        let live;
        let sources: &dyn SceneSources = match &self.live {
            None => &simulated,
            Some(frames) => {
                live = LiveSources {
                    live: frames,
                    simulated: &simulated,
                };
                &live
            }
        };
        let image = scene_render::render_scene_model(
            &self.project,
            &self.scene,
            sources,
            size,
            self.frame,
            self.pose,
            Some(&self.grades),
            Some(self.input),
        )
        .ok_or_else(unrenderable)?;
        let rgba = straight_rgba(image.pixels(), image.stride(), self.width, self.height)
            .ok_or_else(unrenderable)?;
        encode_qoi_rgba(self.width, self.height, &rgba)
            .map_err(|error| refusal("render_failed", error.to_string()))
    }
}

/// The composed premultiplied picture as tightly packed straight RGBA8
/// (each channel `round(c * 255 / a)`; transparent pixels stay zero).
fn straight_rgba(pixels: &[u8], stride: usize, width: u16, height: u16) -> Option<Vec<u8>> {
    let row_bytes = usize::from(width) * 4;
    let mut rgba = Vec::with_capacity(row_bytes * usize::from(height));
    for row in pixels.chunks(stride).take(usize::from(height)) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            let alpha = pixel[3];
            let straight = |channel: u8| match alpha {
                0 => 0,
                u8::MAX => channel,
                _ => {
                    let value =
                        (u16::from(channel) * 255 + u16::from(alpha) / 2) / u16::from(alpha);
                    u8::try_from(value).unwrap_or(u8::MAX)
                }
            };
            rgba.extend_from_slice(&[
                straight(pixel[0]),
                straight(pixel[1]),
                straight(pixel[2]),
                alpha,
            ]);
        }
    }
    (rgba.len() == row_bytes * usize::from(height)).then_some(rgba)
}

/// Resolves the exact scene preview `request` asks for: everything but the
/// composition, which [`ScenePreviewJob::render`] does off the session
/// loop, graded with `grades` (the engine's compiled grades now). `live` is
/// a native daemon's snapshot of its newest source frames (`None` on a
/// simulated daemon).
///
/// # Errors
///
/// A `not_found` refusal for an input that is not a Scene input, and
/// `invalid_request` for a draft clip the scene refuses or a frame past its
/// phase.
pub(crate) fn prepare_scene_preview(
    control: &ControlService<Policy>,
    project: &Project,
    server: &ServerIdentity,
    request: &ScenePreviewRequest,
    grades: Arc<CompiledProjectGrades>,
    live: Option<LiveSourceFrames>,
) -> Result<ScenePreviewJob, StructuredError> {
    let input = request.input.to_domain();
    let engine = control
        .scene_input(input)
        .ok_or_else(|| refusal("not_found", format!("input {input} is not a Scene input")))?;
    let animation_revision = engine.animation().revision;
    let mut scene = (*engine.scene).clone();
    if let Some(draft) = &request.clip {
        let draft = scene_clip_from_wire(draft)
            .map_err(|error| refusal("invalid_request", error.to_string()))?;
        scene
            .edit_animation(|clip| {
                *clip = draft;
                Ok(())
            })
            .map_err(|error| refusal("invalid_request", error.to_string()))?;
    }
    let pose = sample_pose(request.sample, &scene.animation.clip)?;
    Ok(ScenePreviewJob {
        server: server.clone(),
        request_id: request.request_id.clone(),
        wire_input: request.input,
        input,
        project: project.clone(),
        scene,
        pose,
        frame: control.frame_cursor(),
        grades,
        live,
        animation_revision,
        draft: request.clip.is_some(),
        width: request.width,
        height: request.height,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_model::fm_animation::ClipState;
    use fm_model::{
        Input, InputKind, Layer, LayerGeometry, Project, ProjectSettings, Rotation, SimulatedAudio,
        SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    };
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
        SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
    };
    use fm_video::ImageFrame;

    use super::straight_rgba;
    use crate::scene_render::{self, MAX_NESTED_SCENE_DEPTH, SceneSources};

    pub(super) const WIDTH: u32 = 64;
    pub(super) const HEIGHT: u32 = 36;
    pub(super) const SOLID: [u8; 4] = [7, 11, 13, 255];
    pub(super) const BACKGROUND: [u8; 4] = [20, 30, 40, 255];

    pub(super) fn input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    pub(super) fn scene_id(value: u128) -> SceneId {
        SceneId::new(NonZeroU128::new(value).unwrap())
    }

    /// A 64x36 project whose input 1 is a solid simulated source.
    pub(super) fn project() -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Preview",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(WIDTH, HEIGHT).unwrap(),
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
                SimulatedVideo::Solid(SolidColor::new(SOLID[0], SOLID[1], SOLID[2], SOLID[3])),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project
    }

    pub(super) fn layer(id: u32, source: SourceRef, x: i32, width: u32) -> Layer {
        Layer {
            id: fm_model::LayerId::new(id).unwrap(),
            name: format!("Layer {id}"),
            source,
            enabled: true,
            geometry: LayerGeometry::new(x, 0, width, HEIGHT, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: 255,
            z_order: i32::try_from(id).unwrap(),
        }
    }

    /// Adds scene `scene` over the opaque [`BACKGROUND`], shown by Scene
    /// input `input`.
    pub(super) fn add_scene(project: &mut Project, scene: u128, input: u128, layers: Vec<Layer>) {
        project.add_scene(fm_model::Scene {
            id: scene_id(scene),
            name: format!("Scene {scene}"),
            background: fm_model::Rgba8::new(
                BACKGROUND[0],
                BACKGROUND[1],
                BACKGROUND[2],
                BACKGROUND[3],
            ),
            layers,
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_input(Input {
            id: input_id(input),
            name: format!("Scene input {input}"),
            kind: InputKind::Scene {
                scene_id: scene_id(scene),
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
    }

    pub(super) fn render(
        project: &Project,
        scene: u128,
        sources: &dyn SceneSources,
    ) -> Option<ImageFrame> {
        let model = project.scene(scene_id(scene)).unwrap();
        scene_render::render_scene_model(
            project,
            model,
            sources,
            (WIDTH, HEIGHT),
            4,
            ClipState::Hold,
            None,
            None,
        )
    }

    pub(super) fn pixel(frame: &ImageFrame, x: u32, y: u32) -> [u8; 4] {
        let pixel = frame.pixel(x, y).unwrap();
        [pixel.r, pixel.g, pixel.b, pixel.a]
    }

    /// GX-011: a layer naming a nested scene, directly or through its Scene
    /// input, draws that scene composed on its own canvas (background
    /// included) and scaled into the layer like any source.
    #[test]
    fn nested_scenes_compose_on_their_own_canvas() {
        let mut project = project();
        // Scene 1: the solid over the left half of its canvas.
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(1, SourceRef::Input(input_id(1)), 0, WIDTH / 2)],
        );
        // Scene 2: scene 1 through its Scene input on the left half, and
        // directly on the right half.
        add_scene(
            &mut project,
            2,
            11,
            vec![
                layer(2, SourceRef::Input(input_id(10)), 0, WIDTH / 2),
                layer(3, SourceRef::Scene(scene_id(1)), 32, WIDTH / 2),
            ],
        );
        let pipeline = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        let composed = render(&project, 2, &pipeline).unwrap();
        for y in [0, HEIGHT / 2, HEIGHT - 1] {
            for x in 0..WIDTH {
                // Each half shows scene 1 at half width: the solid on its
                // left quarter, scene 1's background on the next.
                let expected = if x % 32 < 16 { SOLID } else { BACKGROUND };
                assert_eq!(pixel(&composed, x, y), expected, "({x}, {y})");
            }
        }
        // The simulated monitor tap composes nested scenes the same way.
        let tap = scene_render::render_scene(
            &project,
            input_id(11),
            &pipeline,
            4,
            ClipState::Hold,
            &fm_engine::CompiledProjectGrades::default(),
        )
        .unwrap();
        assert_eq!(tap.pixels(), composed.pixels());
    }

    pub(super) fn exposure(millistops: i16) -> fm_grade::Grade {
        fm_grade::Grade {
            exposure_millistops: millistops,
            ..fm_grade::Grade::IDENTITY
        }
    }

    pub(super) fn graded(grade: &fm_grade::Grade, [red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
        let [red, green, blue] = fm_grade::CompiledGrade::new(grade, None)
            .unwrap()
            .apply_rgb8([red, green, blue]);
        [red, green, blue, alpha]
    }

    /// SW-019: an exact preview grades like the monitor: a leaf layer shows
    /// its input grade then its layer grade; a layer drawing a nested scene
    /// through a Scene input carries that input's grade then its own layer
    /// grade over the nested scene's graded layers, a layer naming the
    /// scene directly only its layer grade; and the Scene input a preview
    /// shows grades the composed picture with its own grade.
    #[test]
    fn previews_grade_leaf_and_nested_scene_layers() {
        use fm_model::{GradeSource, SceneLayerRef};

        let mut project = project();
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(1, SourceRef::Input(input_id(1)), 0, WIDTH)],
        );
        add_scene(
            &mut project,
            2,
            11,
            vec![
                layer(2, SourceRef::Input(input_id(10)), 0, WIDTH / 2),
                layer(3, SourceRef::Scene(scene_id(1)), 32, WIDTH / 2),
            ],
        );
        let warm = fm_grade::builtin_preset("warm").unwrap().grade.clone();
        let inline = |grade: &fm_grade::Grade| Some(GradeSource::Inline(Box::new(grade.clone())));
        let layer_ref = |scene: u128, layer: u32| {
            SceneLayerRef::new(scene_id(scene), fm_model::LayerId::new(layer).unwrap())
        };
        project.set_input_grade(input_id(1), inline(&warm)).unwrap();
        project
            .set_layer_grade(layer_ref(1, 1), inline(&exposure(-1_000)))
            .unwrap();
        project
            .set_layer_grade(layer_ref(2, 2), inline(&exposure(-500)))
            .unwrap();
        project
            .set_input_grade(input_id(10), inline(&exposure(2_000)))
            .unwrap();
        let grades =
            fm_engine::CompiledProjectGrades::compile(&project, &std::collections::BTreeMap::new())
                .unwrap();
        let pipeline = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        let model = project.scene(scene_id(2)).unwrap();
        let compose = |shown_by: Option<InputId>| {
            scene_render::render_scene_model(
                &project,
                model,
                &pipeline,
                (WIDTH, HEIGHT),
                4,
                ClipState::Hold,
                Some(&grades),
                shown_by,
            )
            .unwrap()
        };
        let composed = compose(None);
        let nested = graded(&exposure(-1_000), graded(&warm, SOLID));
        let through_input = graded(&exposure(-500), graded(&exposure(2_000), nested));
        assert_ne!(nested, SOLID);
        assert_ne!(through_input, nested);
        for y in [0, HEIGHT / 2, HEIGHT - 1] {
            for x in 0..WIDTH {
                let expected = if x < WIDTH / 2 { through_input } else { nested };
                assert_eq!(pixel(&composed, x, y), expected, "({x}, {y})");
            }
        }
        // Shown by Scene input 11, the composed picture carries its grade.
        project
            .set_input_grade(input_id(11), inline(&exposure(-2_000)))
            .unwrap();
        let grades =
            fm_engine::CompiledProjectGrades::compile(&project, &std::collections::BTreeMap::new())
                .unwrap();
        let model = project.scene(scene_id(2)).unwrap();
        let shown = scene_render::render_scene_model(
            &project,
            model,
            &pipeline,
            (WIDTH, HEIGHT),
            4,
            ClipState::Hold,
            Some(&grades),
            Some(input_id(11)),
        )
        .unwrap();
        for x in [0, WIDTH / 2] {
            assert_eq!(
                pixel(&shown, x, 0),
                graded(&exposure(-2_000), pixel(&composed, x, 0))
            );
        }
        // The simulated monitor tap composes the same graded picture.
        let tap = scene_render::render_scene(
            &project,
            input_id(11),
            &pipeline,
            4,
            ClipState::Hold,
            &grades,
        )
        .unwrap();
        assert_eq!(tap.pixels(), shown.pixels());
        // Ungraded, the leaf shows its source as is.
        let ungraded = render(&project, 2, &pipeline).unwrap();
        assert_eq!(pixel(&ungraded, 0, 0), SOLID);
    }

    /// Nesting is bounded: a chain of exactly
    /// [`MAX_NESTED_SCENE_DEPTH`] scenes below the composed one draws, one
    /// more is refused; a scene whose leaf the sources cannot draw is
    /// refused at any depth.
    #[test]
    fn nesting_is_bounded_and_undrawable_leaves_refuse() {
        let mut project = project();
        add_scene(
            &mut project,
            1,
            1_001,
            vec![layer(1, SourceRef::Input(input_id(1)), 0, WIDTH)],
        );
        let deepest = u128::try_from(MAX_NESTED_SCENE_DEPTH).unwrap() + 2;
        for scene in 2..=deepest {
            add_scene(
                &mut project,
                scene,
                1_000 + scene,
                vec![layer(1, SourceRef::Scene(scene_id(scene - 1)), 0, WIDTH)],
            );
        }
        let pipeline = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        let bounded = render(&project, deepest - 1, &pipeline).unwrap();
        assert_eq!(pixel(&bounded, 5, 5), SOLID);
        assert_eq!(render(&project, deepest, &pipeline), None);

        // A layer naming an input the simulated pipeline cannot draw.
        project.add_input(Input {
            id: input_id(2),
            name: "Clip".into(),
            kind: InputKind::Media {
                asset_uri: "asset://media/clip.mov".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
        add_scene(
            &mut project,
            100,
            1_100,
            vec![layer(1, SourceRef::Input(input_id(2)), 0, WIDTH)],
        );
        add_scene(
            &mut project,
            101,
            1_101,
            vec![layer(1, SourceRef::Scene(scene_id(100)), 0, WIDTH)],
        );
        assert_eq!(render(&project, 100, &pipeline), None);
        assert_eq!(render(&project, 101, &pipeline), None);
    }

    #[test]
    fn premultiplied_pixels_are_sent_straight() {
        let pixels = [255, 0, 0, 255, 64, 32, 0, 128, 0, 0, 0, 0, 9, 9, 9, 9];
        assert_eq!(
            straight_rgba(&pixels, 8, 2, 2).unwrap(),
            vec![
                255, 0, 0, 255, 128, 64, 0, 128, 0, 0, 0, 0, 255, 255, 255, 9
            ]
        );
        assert_eq!(straight_rgba(&pixels, 8, 3, 2), None);
    }
}

/// Native previews compose from the media pipeline's newest source frames.
#[cfg(all(test, feature = "native-media"))]
mod native_tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU128;
    use std::sync::Arc;

    use fm_frame::{
        AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
        CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
        NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
        SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
    };
    use fm_model::{Input, InputKind, SourceRef, TitleInput};

    use super::tests::{
        BACKGROUND, HEIGHT, SOLID, WIDTH, add_scene, exposure, graded, input_id, layer, pixel,
        project, render, scene_id,
    };
    use super::{LiveSourceFrames, LiveSources};
    use crate::scene_render;

    const FRAME_NANOS: i64 = 40_000_000;

    fn color(transfer: TransferFunction) -> ColorMetadata {
        ColorMetadata {
            primaries: ColorPrimaries::Bt709,
            transfer,
            matrix: MatrixCoefficients::Identity,
            range: SignalRange::Full,
            chroma_location: ChromaLocation::Center,
        }
    }

    /// One labeled straight-alpha 64x36 frame of `format` in clock
    /// `domain`, every pixel `bytes`.
    fn frame(
        domain: ClockDomainId,
        sequence: u64,
        format: PixelFormat,
        transfer: TransferFunction,
        bytes: [u8; 4],
    ) -> CpuVideoFrame {
        let pts = i64::try_from(sequence).unwrap() * FRAME_NANOS;
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(pts),
                TimeBase::new(1, 1_000_000_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(pts),
            NormalizedDuration::from_nanos(FRAME_NANOS.unsigned_abs()).unwrap(),
            domain,
            SequenceNumber::new(sequence),
        )
        .unwrap();
        let pixels = usize::try_from(WIDTH * HEIGHT).unwrap();
        let payload = CpuVideoPayload::new(
            format,
            VideoDimensions::new(WIDTH, HEIGHT).unwrap(),
            vec![
                CpuVideoPlane::new(usize::try_from(WIDTH).unwrap() * 4, bytes.repeat(pixels))
                    .unwrap(),
            ],
        )
        .unwrap();
        CpuVideoFrame::new(timing, payload)
            .with_metadata(VideoFrameMetadata::new(
                color(transfer),
                Some(AlphaMode::Straight),
            ))
            .unwrap()
    }

    fn domain(value: u128) -> ClockDomainId {
        ClockDomainId::new(NonZeroU128::new(value).unwrap())
    }

    /// Input 2 is a media clip and input 3 a title: sources only a native
    /// pipeline realizes.
    fn native_project() -> fm_model::Project {
        let mut project = project();
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
            kind: InputKind::Title(TitleInput {
                template_uri: "asset://titles/lower-third.fmtitle".into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
            required_capabilities: Vec::new(),
        });
        // Scene 1: the clip on the left half. Scene 2: scene 1 on the left
        // half, the solid on the third quarter, and the title over the
        // whole canvas.
        add_scene(
            &mut project,
            1,
            10,
            vec![layer(1, SourceRef::Input(input_id(2)), 0, WIDTH / 2)],
        );
        add_scene(
            &mut project,
            2,
            11,
            vec![
                layer(1, SourceRef::Scene(scene_id(1)), 0, WIDTH / 2),
                layer(2, SourceRef::Input(input_id(1)), 32, WIDTH / 4),
                layer(3, SourceRef::Input(input_id(3)), 0, WIDTH),
            ],
        );
        project
    }

    /// GX-011: a native daemon's exact scene preview draws non-simulated
    /// sources (decoded media, live titles) from their newest frames,
    /// converted from their labeled colorimetry to sRGB, inside nested
    /// scenes too; a source with no frame refuses the preview.
    #[test]
    fn live_frames_compose_media_titles_and_nested_scenes() {
        let project = native_project();
        let simulated = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        // The clip is sRGB green; the title a transparent BGRA frame.
        let mut frames = BTreeMap::new();
        frames.insert(
            input_id(2),
            Arc::new(frame(
                domain(7),
                3,
                PixelFormat::Rgba8,
                TransferFunction::Srgb,
                [0, 200, 0, 255],
            )),
        );
        frames.insert(
            input_id(3),
            Arc::new(frame(
                domain(8),
                1,
                PixelFormat::Bgra8,
                TransferFunction::Bt709,
                [128, 0, 0, 0],
            )),
        );
        let live = LiveSourceFrames::from_frames(frames.clone());
        let sources = LiveSources {
            live: &live,
            simulated: &simulated,
        };
        let composed = render(&project, 2, &sources).unwrap();
        // The transparent title leaves everything below it.
        for y in [0, HEIGHT - 1] {
            for x in 0..WIDTH {
                let expected = match x {
                    0..16 => [0, 200, 0, 255],
                    32..48 => SOLID,
                    _ => BACKGROUND,
                };
                assert_eq!(pixel(&composed, x, y), expected, "({x}, {y})");
            }
        }

        // An opaque BT.709-labeled title: blue code 128 is re-encoded as
        // sRGB, not passed through.
        frames.insert(
            input_id(3),
            Arc::new(frame(
                domain(8),
                2,
                PixelFormat::Bgra8,
                TransferFunction::Bt709,
                [128, 0, 0, 255],
            )),
        );
        let live = LiveSourceFrames::from_frames(frames.clone());
        let sources = LiveSources {
            live: &live,
            simulated: &simulated,
        };
        let titled = render(&project, 2, &sources).unwrap();
        let [red, green, blue, alpha] = pixel(&titled, 40, 10);
        assert_eq!((red, green, alpha), (0, 0, 255));
        assert_ne!(blue, 128, "BT.709 code values are converted to sRGB");
        assert!((130..=150).contains(&blue), "{blue}");
        let srgb = super::live_picture(
            &frame(
                domain(9),
                0,
                PixelFormat::Bgra8,
                TransferFunction::Srgb,
                [128, 0, 0, 255],
            ),
            WIDTH / 2,
            HEIGHT / 2,
        )
        .unwrap();
        assert_eq!((srgb.width(), srgb.height()), (WIDTH / 2, HEIGHT / 2));
        assert_eq!(pixel(&srgb, 0, 0), [0, 0, 128, 255], "sRGB passes through");

        // Without the clip's frame (or on a simulated daemon) the scene,
        // and every scene nesting it, is refused.
        frames.remove(&input_id(2));
        let live = LiveSourceFrames::from_frames(frames);
        let sources = LiveSources {
            live: &live,
            simulated: &simulated,
        };
        assert_eq!(render(&project, 1, &sources), None);
        assert_eq!(render(&project, 2, &sources), None);
        assert_eq!(render(&project, 2, &simulated), None);
    }

    /// SW-019: live leaves are graded like simulated ones, inside nested
    /// scenes too: the clip by its input grade, the solid by its layer
    /// grade.
    #[test]
    fn live_frames_are_graded_like_simulated_sources() {
        let project = native_project();
        let simulated = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        let mut frames = BTreeMap::new();
        frames.insert(
            input_id(2),
            Arc::new(frame(
                domain(7),
                3,
                PixelFormat::Rgba8,
                TransferFunction::Srgb,
                [0, 200, 0, 255],
            )),
        );
        frames.insert(
            input_id(3),
            Arc::new(frame(
                domain(8),
                1,
                PixelFormat::Bgra8,
                TransferFunction::Bt709,
                [128, 0, 0, 0],
            )),
        );
        let live = LiveSourceFrames::from_frames(frames);
        let sources = LiveSources {
            live: &live,
            simulated: &simulated,
        };
        let mut graded_project = project.clone();
        let inline = |grade: fm_grade::Grade| Some(fm_model::GradeSource::Inline(Box::new(grade)));
        graded_project
            .set_input_grade(input_id(2), inline(exposure(-1_000)))
            .unwrap();
        let warm = fm_grade::builtin_preset("warm").unwrap().grade.clone();
        graded_project
            .set_layer_grade(
                fm_model::SceneLayerRef::new(scene_id(2), fm_model::LayerId::new(2).unwrap()),
                inline(warm.clone()),
            )
            .unwrap();
        let grades =
            fm_engine::CompiledProjectGrades::compile(&graded_project, &BTreeMap::new()).unwrap();
        let graded_picture = scene_render::render_scene_model(
            &graded_project,
            graded_project.scene(scene_id(2)).unwrap(),
            &sources,
            (WIDTH, HEIGHT),
            4,
            fm_model::fm_animation::ClipState::Hold,
            Some(&grades),
            None,
        )
        .unwrap();
        let clip = graded(&exposure(-1_000), [0, 200, 0, 255]);
        assert_ne!(clip, [0, 200, 0, 255]);
        assert_eq!(pixel(&graded_picture, 5, 5), clip);
        assert_eq!(pixel(&graded_picture, 40, 5), graded(&warm, SOLID));
        assert_eq!(pixel(&graded_picture, 20, 5), BACKGROUND);
    }

    /// End to end on a native adapter: the frames a playback retains and
    /// ingests are the frames a preview snapshot composes, and a snapshot
    /// does not change with later frames. Skips where no adapter exists
    /// unless `FM_REQUIRE_NATIVE_MEDIA=1`.
    #[test]
    fn native_playback_frames_feed_scene_previews() {
        use freemixd::native_media::{
            NativeMediaRuntime, NativeResolvedSource, NativeSourceLimits,
        };

        let runtime = match NativeMediaRuntime::new_blocking([crate::platform_native_backend()]) {
            Ok(runtime) => runtime,
            Err(error) => {
                assert!(
                    std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                    "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
                );
                eprintln!("skipping: no native adapter ({error})");
                return;
            }
        };
        let project = native_project();
        let playback_domain = crate::native_clock_domain();
        // Retained frames live in the playback's clock domain at sequence 0.
        let clip = frame(
            playback_domain,
            0,
            PixelFormat::Rgba8,
            TransferFunction::Srgb,
            [0, 200, 0, 255],
        );
        let transparent = frame(
            domain(8),
            1,
            PixelFormat::Bgra8,
            TransferFunction::Srgb,
            [0, 0, 0, 0],
        );
        let mut playback = runtime
            .preflight_resolved_source_playback_mixed_blocking(
                None,
                [
                    NativeResolvedSource::RetainedFrame {
                        input: input_id(2),
                        frame: clip.clone(),
                    },
                    NativeResolvedSource::LiveFrame {
                        input: input_id(3),
                        frame: transparent.clone(),
                    },
                ],
                playback_domain,
                fm_codec_ffmpeg::StreamSelector::Best,
                NativeSourceLimits::default(),
            )
            .unwrap();
        let latest = playback.latest_frames();
        assert_eq!(latest.get(input_id(2)).as_deref(), Some(&clip));
        assert_eq!(latest.get(input_id(3)).as_deref(), Some(&transparent));
        let before = LiveSourceFrames::snapshot(&latest);

        // A newer live title frame (opaque sRGB blue) replaces the seed.
        let blue = frame(
            domain(8),
            2,
            PixelFormat::Bgra8,
            TransferFunction::Srgb,
            [200, 0, 0, 255],
        );
        runtime
            .ingest_live_video_frame_blocking(&mut playback, input_id(3), blue.clone())
            .unwrap();
        assert_eq!(latest.get(input_id(3)).as_deref(), Some(&blue));
        let after = LiveSourceFrames::snapshot(&latest);

        let simulated = scene_render::simulated_pipeline(&project, WIDTH, HEIGHT).unwrap();
        let compose = |live: &LiveSourceFrames| {
            render(
                &project,
                2,
                &LiveSources {
                    live,
                    simulated: &simulated,
                },
            )
            .unwrap()
        };
        let earlier = compose(&before);
        assert_eq!(pixel(&earlier, 5, 5), [0, 200, 0, 255]);
        assert_eq!(pixel(&earlier, 40, 5), SOLID);
        let later = compose(&after);
        assert_eq!(pixel(&later, 5, 5), [0, 0, 200, 255]);
        assert_eq!(pixel(&later, 40, 5), [0, 0, 200, 255]);
    }
}
