use core::fmt;
use std::collections::BTreeMap;

use fm_compositor::{
    CompositionPlan, CpuExecutionError, CpuSourceFrame, Effect, OutputTarget, PlanError, RectMask,
    Rotation, Scene, SceneError, SourceId, SourceLayer, Transform, apply_grade, compile_scene,
    execute_cpu,
};
use fm_engine::CompiledProjectGrades;
use fm_model::{
    Input, InputKind, MixFeedHistory, MixSelection, Project, RenderSource, ResolveError,
    SimulatedVideo, SourceRef, SourceResolver,
};
use fm_types::{InputId, SceneId};
use fm_video::{CropRect, FrameError, ImageFrame, Rgba8, scale_nearest, solid_color};

#[derive(Debug)]
pub(crate) enum SceneRenderError {
    UnknownScene(SceneId),
    UnknownInput(InputId),
    SceneSource(SceneId),
    Resolve(ResolveError),
    AliasCropUnsupported {
        input: InputId,
    },
    UnsupportedInput {
        input: InputId,
    },
    Frame(FrameError),
    CropBoundsOverflow {
        layer: usize,
    },
    CropOutOfBounds {
        layer: usize,
        width: u32,
        height: u32,
    },
    MaskBoundsOverflow {
        layer: usize,
    },
    MaskOutOfBounds {
        layer: usize,
        width: u32,
        height: u32,
    },
    Scene(SceneError),
    Plan(PlanError),
    Source(fm_sim::RenderError),
    Cpu(CpuExecutionError),
    Grade(FrameError),
    Scale(FrameError),
}

impl fmt::Display for SceneRenderError {
    #[allow(clippy::match_same_arms)] // distinct error types share one Display
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "scene {scene} does not exist"),
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::SceneSource(scene) => write!(formatter, "scene source {scene} is unsupported"),
            Self::Resolve(error) => error.fmt(formatter),
            Self::AliasCropUnsupported { input } => write!(
                formatter,
                "alias input {input} has a crop, which scene rendering does not support"
            ),
            Self::Frame(error) => error.fmt(formatter),
            Self::UnsupportedInput { input } => {
                write!(
                    formatter,
                    "input {input} is unsupported for scene rendering"
                )
            }
            Self::CropBoundsOverflow { layer } => {
                write!(formatter, "layer {layer} crop bounds overflow")
            }
            Self::CropOutOfBounds {
                layer,
                width,
                height,
            } => write!(
                formatter,
                "layer {layer} crop exceeds source bounds {width}x{height}"
            ),
            Self::MaskBoundsOverflow { layer } => {
                write!(formatter, "layer {layer} mask bounds overflow")
            }
            Self::MaskOutOfBounds {
                layer,
                width,
                height,
            } => write!(
                formatter,
                "layer {layer} mask exceeds source bounds {width}x{height}"
            ),
            Self::Scene(error) => error.fmt(formatter),
            Self::Plan(error) => error.fmt(formatter),
            Self::Source(error) => error.fmt(formatter),
            Self::Cpu(error) => error.fmt(formatter),
            Self::Grade(error) => write!(formatter, "grade failed: {error}"),
            Self::Scale(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SceneRenderError {}

/// Renders one flat simulated scene and scales it to `width`x`height`.
///
/// Layer geometry, crops and masks are authored in the project coordinate
/// space, so the scene is composed at the pipeline dimensions, which the
/// caller sets to the project video dimensions. Sources are rendered at that
/// size, crops and masks are checked against it, and the finished frame is
/// resampled to the output with nearest-neighbor sampling. Composing first and
/// scaling once keeps every authored rectangle exact in project pixels; when
/// the output divides the project size evenly (1920x1080 to 640x360, for
/// example) each authored edge lands on an exact output pixel.
///
/// Each layer carries its input's grade and then its own scene layer grade
/// (both applied by the compositor after the layer's effects and before its
/// transform). `shown_by`, the Scene input showing the scene (`None` for a
/// scene rendered by id), grades the composed scene with its input grade,
/// and the program grade then colours the composed picture before it is
/// scaled.
///
/// `pose` is the point of the scene's animation drawn
/// ([`fm_model::Scene::posed`] with the pipeline size as the canvas).
#[allow(clippy::too_many_arguments)] // one render: scene, pose, sources, size
#[allow(clippy::too_many_lines)] // one pass over the scene graph
pub(crate) fn render_scene(
    project: &Project,
    scene: SceneId,
    pose: fm_model::fm_animation::ClipState,
    pipeline: &fm_sim::SimulatedPipeline,
    grades: &CompiledProjectGrades,
    shown_by: Option<InputId>,
    frame_number: u64,
    width: u32,
    height: u32,
) -> Result<ImageFrame, SceneRenderError> {
    let model = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)
        .ok_or(SceneRenderError::UnknownScene(scene))?
        .posed(pose, pipeline.width(), pipeline.height());
    let model = &model;
    let mut composition = Scene::new(
        pipeline.width(),
        pipeline.height(),
        Rgba8::new(
            model.background.red,
            model.background.green,
            model.background.blue,
            model.background.alpha,
        ),
    )
    .map_err(SceneRenderError::Scene)?;
    let resolver = SourceResolver::new(project);
    let history = MixFeedHistory::<ImageFrame>::new();
    let selection = shown_by.or_else(|| project.inputs().first().map(|input| input.id));
    let context = match selection {
        Some(input) => Some(
            resolver
                .context(
                    MixSelection {
                        program: input,
                        preview: input,
                    },
                    &history,
                )
                .map_err(SceneRenderError::Resolve)?,
        ),
        None => None,
    };
    // Resolves one layer source through its alias chain: a concrete leaf
    // draws normally, a feed draws black before its first commit, and an
    // alias's own crop is not composable into a scene layer yet (IN-024).
    let resolve_layer = |input: InputId| -> Result<(InputId, Option<InputId>), SceneRenderError> {
        let Some(context) = context.as_ref() else {
            return Err(SceneRenderError::UnknownInput(input));
        };
        let chosen = context
            .resolve_input(input)
            .map_err(SceneRenderError::Resolve)?;
        if chosen.crop.is_some() {
            return Err(SceneRenderError::AliasCropUnsupported { input });
        }
        Ok(match &chosen.source {
            RenderSource::Input(concrete) => (*concrete, Some(*concrete)),
            RenderSource::Scene(scene) => return Err(SceneRenderError::SceneSource(*scene)),
            RenderSource::PreviousFrame { .. } | RenderSource::Black { .. } => (input, None),
        })
    };
    let mut sources: BTreeMap<InputId, (SourceId, Option<InputId>)> = BTreeMap::new();

    for (index, layer) in model.layers.iter().enumerate() {
        if !layer.enabled {
            continue;
        }
        if composition.layers().len() == CompositionPlan::MAX_LAYERS {
            return Err(SceneRenderError::Plan(PlanError::TooManyLayers {
                actual: composition.layers().len() + 1,
                maximum: CompositionPlan::MAX_LAYERS,
            }));
        }
        let input = input_for_layer(project, layer.source)?;
        let (source_input, rendered_input) = resolve_layer(input.id)?;
        if let Some(render) = rendered_input {
            validate_input(
                project
                    .inputs()
                    .iter()
                    .find(|candidate| candidate.id == render)
                    .ok_or(SceneRenderError::UnknownInput(render))?,
            )?;
        }
        validate_bounds(
            index,
            layer.crop,
            layer.mask,
            pipeline.width(),
            pipeline.height(),
        )?;
        let token = scene_source_token(&mut sources, source_input, rendered_input);
        // A key/fill key's input (SW-008) renders like a layer source.
        let key_token = match layer.key.and_then(fm_model::LayerKey::key_input) {
            Some(key_input) => {
                let key_input = input_for_layer(project, SourceRef::Input(key_input))?;
                let (key_source, key_render) = resolve_layer(key_input.id)?;
                if let Some(render) = key_render {
                    validate_input(
                        project
                            .inputs()
                            .iter()
                            .find(|candidate| candidate.id == render)
                            .ok_or(SceneRenderError::UnknownInput(render))?,
                    )?;
                }
                Some(scene_source_token(&mut sources, key_source, key_render))
            }
            None => None,
        };
        let mut source_layer = compositor_layer(token, layer, key_token);
        if let Some(grade) = grades.input(rendered_input.unwrap_or(source_input)) {
            source_layer = source_layer.with_grade(grade.clone());
        }
        if let Some(grade) = grades.layer(fm_model::SceneLayerRef::new(scene, layer.id)) {
            source_layer = source_layer.with_layer_grade(grade.clone());
        }
        composition.push_layer(source_layer);
    }

    let (plan, _) =
        compile_scene(&composition, OutputTarget::Program).map_err(SceneRenderError::Plan)?;
    let frames = sources
        .into_iter()
        .map(|(_, (source, rendered))| match rendered {
            Some(input) => pipeline
                .render_source(input, frame_number)
                .map(|frame| (source, frame))
                .map_err(SceneRenderError::Source),
            None => solid_color(
                pipeline.width(),
                pipeline.height(),
                Rgba8::new(0, 0, 0, u8::MAX),
            )
            .map(|frame| (source, frame))
            .map_err(SceneRenderError::Frame),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bindings = frames
        .iter()
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect::<Vec<_>>();
    let mut composed = execute_cpu(&plan, &bindings).map_err(SceneRenderError::Cpu)?;
    if let Some(grade) = shown_by.and_then(|input| grades.input(input)) {
        composed = apply_grade(&composed, grade).map_err(SceneRenderError::Grade)?;
    }
    if let Some(grade) = grades.program() {
        composed = apply_grade(&composed, grade).map_err(SceneRenderError::Grade)?;
    }
    if composed.width() == width && composed.height() == height {
        return Ok(composed);
    }
    scale_nearest(&composed, width, height).map_err(SceneRenderError::Scale)
}

/// Tokens one layer source by its resolved key, sharing a texture between
/// aliases of the same concrete input.
fn scene_source_token(
    sources: &mut BTreeMap<InputId, (SourceId, Option<InputId>)>,
    key: InputId,
    rendered: Option<InputId>,
) -> SourceId {
    if let Some((token, _)) = sources.get(&key) {
        return *token;
    }
    let token = SourceId::new(u64::try_from(sources.len()).unwrap_or(u64::MAX));
    sources.insert(key, (token, rendered));
    token
}

fn input_for_layer(project: &Project, source: SourceRef) -> Result<&Input, SceneRenderError> {
    let input = match source {
        SourceRef::Input(input) => input,
        SourceRef::Scene(scene) => return Err(SceneRenderError::SceneSource(scene)),
    };
    project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .ok_or(SceneRenderError::UnknownInput(input))
}

fn validate_input(input: &Input) -> Result<(), SceneRenderError> {
    if matches!(
        &input.kind,
        InputKind::Simulated(simulated)
            if matches!(simulated.video, SimulatedVideo::Bars | SimulatedVideo::Solid(_))
    ) {
        return Ok(());
    }
    Err(SceneRenderError::UnsupportedInput { input: input.id })
}

fn validate_bounds(
    layer: usize,
    crop: Option<fm_model::CropRect>,
    mask: Option<fm_model::RectMask>,
    source_width: u32,
    source_height: u32,
) -> Result<(), SceneRenderError> {
    let (width, height) = crop.map_or(Ok((source_width, source_height)), |crop| {
        let right = crop
            .x
            .checked_add(crop.width)
            .ok_or(SceneRenderError::CropBoundsOverflow { layer })?;
        let bottom = crop
            .y
            .checked_add(crop.height)
            .ok_or(SceneRenderError::CropBoundsOverflow { layer })?;
        if right > source_width || bottom > source_height {
            return Err(SceneRenderError::CropOutOfBounds {
                layer,
                width: source_width,
                height: source_height,
            });
        }
        Ok((crop.width, crop.height))
    })?;
    let Some(mask) = mask else { return Ok(()) };
    let right = mask
        .x
        .checked_add(mask.width)
        .ok_or(SceneRenderError::MaskBoundsOverflow { layer })?;
    let bottom = mask
        .y
        .checked_add(mask.height)
        .ok_or(SceneRenderError::MaskBoundsOverflow { layer })?;
    if right > width || bottom > height {
        return Err(SceneRenderError::MaskOutOfBounds {
            layer,
            width,
            height,
        });
    }
    Ok(())
}

fn compositor_layer(
    token: SourceId,
    layer: &fm_model::Layer,
    key_token: Option<SourceId>,
) -> SourceLayer {
    let geometry = layer.geometry;
    let rotation = match geometry.rotation {
        fm_model::Rotation::Deg0 => Rotation::Deg0,
        fm_model::Rotation::Deg90 => Rotation::Deg90,
        fm_model::Rotation::Deg180 => Rotation::Deg180,
        fm_model::Rotation::Deg270 => Rotation::Deg270,
    };
    let mut result = SourceLayer::new(
        token,
        layer.z_order,
        Transform::new(
            geometry.translation_x,
            geometry.translation_y,
            geometry.width,
            geometry.height,
            rotation,
        ),
    )
    .with_rotation_millidegrees(geometry.tilt_millidegrees)
    .with_opacity(layer.opacity);
    if let Some(crop) = layer.crop {
        result = result.with_crop(CropRect::new(crop.x, crop.y, crop.width, crop.height));
    }
    if let Some(mask) = layer.mask {
        result = result.with_mask(
            RectMask::new(mask.x, mask.y, mask.width, mask.height).inverted(mask.invert),
        );
    }
    if let Some(key) = layer.key {
        result = with_layer_key(result, key, key_token);
    }
    for effect in &layer.effects {
        result = result.with_effect(Effect::new(effect.name.clone(), effect.parameters.clone()));
    }
    result
}

/// `layer` keyed by `key` (IN-020, SW-008): a chroma or luma key, or a
/// key/fill key whose matte is `key_token`'s picture, with the key's
/// refinement when it is not the identity. A key/fill key without a key
/// token leaves the layer undrawn.
fn with_layer_key(
    layer: SourceLayer,
    key: fm_model::LayerKey,
    key_token: Option<SourceId>,
) -> SourceLayer {
    let layer = match key {
        fm_model::LayerKey::Chroma(chroma) => {
            layer.with_key(fm_compositor::Key::Chroma(fm_compositor::ChromaKey::new(
                fm_compositor::Rgba8::new(chroma.red, chroma.green, chroma.blue, u8::MAX),
                chroma.similarity,
                chroma.smoothness,
                chroma.spill,
            )))
        }
        fm_model::LayerKey::Luma(luma) => layer.with_key(fm_compositor::Key::Luma(
            fm_compositor::LumaKey::new(luma.low, luma.softness, luma.invert).with_high(luma.high),
        )),
        fm_model::LayerKey::Fill(fill) => {
            let Some(key_token) = key_token else {
                return layer.with_enabled(false);
            };
            layer.with_key_fill(
                fm_compositor::KeyFill::new(
                    key_token,
                    fm_compositor::LumaKey::new(fill.low, fill.softness, fill.invert)
                        .with_high(fill.high),
                )
                .with_channel(match fill.channel {
                    fm_model::KeyChannel::Luma => fm_compositor::KeyChannel::Luma,
                    fm_model::KeyChannel::Alpha => fm_compositor::KeyChannel::Alpha,
                }),
            )
        }
    };
    let refinement = key.refinement();
    if refinement.is_identity() {
        return layer;
    }
    let despill = match refinement.despill {
        fm_model::KeyDespill::Off => None,
        fm_model::KeyDespill::Green { strength } => Some(fm_compositor::Despill::new(
            fm_compositor::Backing::Green,
            strength,
        )),
        fm_model::KeyDespill::Blue { strength } => Some(fm_compositor::Despill::new(
            fm_compositor::Backing::Blue,
            strength,
        )),
    };
    let mut compositor = fm_compositor::KeyRefinement::new()
        .with_clip(refinement.clip_black, refinement.clip_white)
        .with_choke(refinement.choke)
        .with_edge_softness(refinement.edge_softness);
    if refinement.light_wrap != 0 {
        compositor = compositor.with_light_wrap(fm_compositor::LightWrap::new(
            refinement.light_wrap,
            refinement.light_wrap_width,
        ));
    }
    if let Some(matte) = refinement.garbage_matte {
        compositor = compositor.with_garbage_matte(matte);
    }
    if let Some(despill) = despill {
        compositor = compositor.with_despill(despill);
    }
    layer.with_key_refinement(compositor)
}
