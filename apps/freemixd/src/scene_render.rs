//! CPU-reference composition of simulated Scene inputs for the monitor tap.
//!
//! Adapted from the CLI's scene renderer: layer geometry, crops, and masks are
//! authored in project pixels, so a scene is composed at the project video
//! dimensions from sources rendered at that size and only then resampled to
//! the requested output with nearest-neighbor sampling. Every layer must name
//! an enabled simulated input; nested scenes, non-simulated inputs, effects the
//! CPU reference rejects, or out-of-bounds crops and masks yield `None` rather
//! than an invented picture.

use std::collections::BTreeMap;

use fm_compositor::{
    CompositionPlan, CpuSourceFrame, CropRect, Effect, ImageFrame, OutputTarget, RectMask, Rgba8,
    Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_model::{Input, InputKind, Project, SimulatedVideo, SourceRef};
use fm_sim::SimulatedPipeline;
use fm_types::{InputId, SceneId};
use fm_video::scale_nearest;

/// Returns the scene a Scene input depicts, or `None` for any other input.
pub(crate) fn scene_of_input(project: &Project, input: InputId) -> Option<SceneId> {
    project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .and_then(|input| match input.kind {
            InputKind::Scene { scene_id, .. } => Some(scene_id),
            _ => None,
        })
}

/// Composes `scene` at the `pipeline` dimensions (the project size) and scales
/// the result to `width`x`height`. Returns `None` when the scene cannot be
/// rendered honestly.
pub(crate) fn render_scene(
    project: &Project,
    scene: SceneId,
    pipeline: &SimulatedPipeline,
    frame_number: u64,
    width: u32,
    height: u32,
) -> Option<ImageFrame> {
    let model = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)?;
    let background = model.background;
    let mut composition = Scene::new(
        pipeline.width(),
        pipeline.height(),
        Rgba8::new(
            background.red,
            background.green,
            background.blue,
            background.alpha,
        ),
    )
    .ok()?;
    let mut sources = BTreeMap::new();
    for layer in &model.layers {
        if !layer.enabled {
            continue;
        }
        if composition.layers().len() == CompositionPlan::MAX_LAYERS {
            return None;
        }
        let input = input_for_layer(project, layer.source)?;
        if !is_simulated(input)
            || !bounds_fit(layer.crop, layer.mask, pipeline.width(), pipeline.height())
        {
            return None;
        }
        let next = SourceId::new(u64::try_from(sources.len()).ok()?);
        let token = *sources.entry(input.id).or_insert(next);
        composition.push_layer(compositor_layer(token, layer));
    }

    let (plan, _) = compile_scene(&composition, OutputTarget::Program).ok()?;
    let frames = sources
        .into_iter()
        .map(|(input, source)| {
            pipeline
                .render_source(input, frame_number)
                .ok()
                .map(|frame| (source, frame))
        })
        .collect::<Option<Vec<_>>>()?;
    let bindings = frames
        .iter()
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect::<Vec<_>>();
    let composed = execute_cpu(&plan, &bindings).ok()?;
    if composed.width() == width && composed.height() == height {
        return Some(composed);
    }
    scale_nearest(&composed, width, height).ok()
}

fn input_for_layer(project: &Project, source: SourceRef) -> Option<&Input> {
    // Nested scenes are not composed, matching the CLI reference renderer.
    let SourceRef::Input(input) = source else {
        return None;
    };
    project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
}

fn is_simulated(input: &Input) -> bool {
    matches!(
        &input.kind,
        InputKind::Simulated(simulated)
            if matches!(simulated.video, SimulatedVideo::Bars | SimulatedVideo::Solid(_))
    )
}

/// Crops must lie inside the source and masks inside the cropped source.
fn bounds_fit(
    crop: Option<fm_model::CropRect>,
    mask: Option<fm_model::RectMask>,
    source_width: u32,
    source_height: u32,
) -> bool {
    let inside = |x: u32, y: u32, width: u32, height: u32, limit_width: u32, limit_height: u32| {
        x.checked_add(width)
            .is_some_and(|right| right <= limit_width)
            && y.checked_add(height)
                .is_some_and(|bottom| bottom <= limit_height)
    };
    let (width, height) = match crop {
        Some(crop) => {
            if !inside(
                crop.x,
                crop.y,
                crop.width,
                crop.height,
                source_width,
                source_height,
            ) {
                return false;
            }
            (crop.width, crop.height)
        }
        None => (source_width, source_height),
    };
    mask.is_none_or(|mask| inside(mask.x, mask.y, mask.width, mask.height, width, height))
}

fn compositor_layer(token: SourceId, layer: &fm_model::Layer) -> SourceLayer {
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
    .with_opacity(layer.opacity);
    if let Some(crop) = layer.crop {
        result = result.with_crop(CropRect::new(crop.x, crop.y, crop.width, crop.height));
    }
    if let Some(mask) = layer.mask {
        result = result.with_mask(
            RectMask::new(mask.x, mask.y, mask.width, mask.height).inverted(mask.invert),
        );
    }
    for effect in &layer.effects {
        result = result.with_effect(Effect::new(effect.name.clone(), effect.parameters.clone()));
    }
    result
}
