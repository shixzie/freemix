//! CPU-reference composition of simulated Scene inputs for the monitor tap.
//!
//! Adapted from the CLI's scene renderer. Layer geometry, crops, and masks are
//! authored in project pixels, but a monitor picture is at most 640x360, so
//! the scene is composed directly at the monitor size: sources are rendered at
//! the monitor size and every project-pixel coordinate is mapped onto the
//! monitor grid first. Composing at the project size and downscaling cost
//! roughly the project/monitor area ratio more (9x at 1080p, seconds per
//! picture in a debug build) for a picture that nearest-neighbor downscaling
//! then mostly threw away. Every layer must name an enabled simulated input;
//! nested scenes, non-simulated inputs, effects the CPU reference rejects, or
//! crops and masks outside the project-size source yield `None` rather than
//! an invented picture, exactly as at the project size.

use std::collections::BTreeMap;

use fm_compositor::{
    CompositionPlan, CpuSourceFrame, CropRect, Effect, ImageFrame, OutputTarget, RectMask, Rgba8,
    Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_model::fm_animation::ClipState;
use fm_model::{Input, InputKind, Project, SimulatedVideo, SourceRef};
use fm_sim::SimulatedPipeline;
use fm_types::{InputId, SceneId};

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

/// Composes `scene` at the `pipeline` dimensions (the fitted monitor size),
/// mapping the scene's project-pixel geometry onto that grid, with every
/// layer in the animation `pose` (see [`fm_model::Scene::posed`]). Returns `None`
/// when the scene cannot be rendered honestly.
///
/// Edges map with the same rule nearest-neighbor downscaling samples by
/// (output pixel `x` shows project pixel `floor(x * project / output)`), so a
/// layer covers the same monitor pixels it would after composing at the
/// project size and downscaling; a layer too thin to cover any monitor pixel
/// is skipped, as downscaling would have dropped it.
pub(crate) fn render_scene(
    project: &Project,
    scene: SceneId,
    pipeline: &SimulatedPipeline,
    frame_number: u64,
    pose: ClipState,
) -> Option<ImageFrame> {
    let dimensions = project.settings().video.dimensions;
    let grid = MonitorGrid {
        project_width: dimensions.width(),
        project_height: dimensions.height(),
        width: pipeline.width(),
        height: pipeline.height(),
    };
    compose(project, scene, pipeline, frame_number, pose, |layer| {
        grid.layer(layer)
    })
}

/// Composes at the project size and downscales: the reference the monitor
/// path is compared against, and what it replaced.
#[cfg(test)]
pub(crate) fn render_scene_at_project_size(
    project: &Project,
    scene: SceneId,
    project_pipeline: &SimulatedPipeline,
    frame_number: u64,
    width: u32,
    height: u32,
    pose: ClipState,
) -> Option<ImageFrame> {
    let composed = compose(
        project,
        scene,
        project_pipeline,
        frame_number,
        pose,
        |layer| Some(Placement::Draw(layer.clone())),
    )?;
    if composed.width() == width && composed.height() == height {
        return Some(composed);
    }
    fm_video::scale_nearest(&composed, width, height).ok()
}

/// Where one project-pixel layer lands on the composition grid.
enum Placement {
    Draw(fm_model::Layer),
    /// The layer covers no output pixel.
    Skip,
}

/// Composes `scene` at the `pipeline` size. `place` maps one project-pixel
/// layer onto the pipeline grid; `None` refuses the scene.
fn compose(
    project: &Project,
    scene: SceneId,
    pipeline: &SimulatedPipeline,
    frame_number: u64,
    pose: ClipState,
    place: impl Fn(&fm_model::Layer) -> Option<Placement>,
) -> Option<ImageFrame> {
    let dimensions = project.settings().video.dimensions;
    // The scene as the engine poses it at this frame (GX-011 stage 5).
    let model = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)?
        .posed(pose, dimensions.width(), dimensions.height());
    let model = &model;
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
    let mut enabled = 0_usize;
    for layer in &model.layers {
        if !layer.enabled {
            continue;
        }
        if enabled == CompositionPlan::MAX_LAYERS {
            return None;
        }
        enabled += 1;
        let input = input_for_layer(project, layer.source)?;
        // Acceptance is decided in project pixels, independent of the grid.
        if !is_simulated(input)
            || !bounds_fit(
                layer.crop,
                layer.mask,
                dimensions.width(),
                dimensions.height(),
            )
        {
            return None;
        }
        let Placement::Draw(placed) = place(layer)? else {
            continue;
        };
        let next = SourceId::new(u64::try_from(sources.len()).ok()?);
        let token = *sources.entry(input.id).or_insert(next);
        composition.push_layer(compositor_layer(token, &placed));
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
    execute_cpu(&plan, &bindings).ok()
}

/// Maps project-pixel coordinates onto a smaller monitor grid of the same
/// picture.
#[derive(Clone, Copy)]
struct MonitorGrid {
    project_width: u32,
    project_height: u32,
    width: u32,
    height: u32,
}

impl MonitorGrid {
    /// The first monitor column whose downscaled sample lies at or after
    /// project column `x`: `ceil(x * width / project_width)`.
    fn column(self, x: i64) -> Option<i64> {
        map_edge(x, self.width, self.project_width)
    }

    fn row(self, y: i64) -> Option<i64> {
        map_edge(y, self.height, self.project_height)
    }

    /// Maps one layer onto the monitor grid.
    fn layer(self, layer: &fm_model::Layer) -> Option<Placement> {
        let geometry = layer.geometry;
        if geometry.width == 0 || geometry.height == 0 {
            // Unmapped, so the compositor refuses it exactly as before.
            return Some(Placement::Draw(layer.clone()));
        }
        // The transform's footprint on the output: a quarter turn swaps it.
        let quarter_turn = matches!(
            geometry.rotation,
            fm_model::Rotation::Deg90 | fm_model::Rotation::Deg270
        );
        let (footprint_width, footprint_height) = if quarter_turn {
            (geometry.height, geometry.width)
        } else {
            (geometry.width, geometry.height)
        };
        let left = i64::from(geometry.translation_x);
        let top = i64::from(geometry.translation_y);
        let x0 = self.column(left)?;
        let x1 = self.column(left + i64::from(footprint_width))?;
        let y0 = self.row(top)?;
        let y1 = self.row(top + i64::from(footprint_height))?;
        if x1 <= x0 || y1 <= y0 {
            return Some(Placement::Skip);
        }
        let mapped_width = u32::try_from(x1 - x0).ok()?;
        let mapped_height = u32::try_from(y1 - y0).ok()?;
        let (width, height) = if quarter_turn {
            (mapped_height, mapped_width)
        } else {
            (mapped_width, mapped_height)
        };
        let mut placed = layer.clone();
        placed.geometry = fm_model::LayerGeometry::new(
            i32::try_from(x0).ok()?,
            i32::try_from(y0).ok()?,
            width,
            height,
            geometry.rotation,
        );
        // Sources render at the monitor size, so crops and masks (authored
        // against the project-size source) map onto the same grid. Each
        // keeps at least one pixel so a thin crop or mask stays a crop or
        // mask instead of becoming invalid. `crop` is the project-size source
        // region and `mapped` its monitor counterpart.
        let (crop, mapped) = match layer.crop {
            Some(crop) => {
                let (x, width) = self.columns(crop.x, crop.width)?;
                let (y, height) = self.rows(crop.y, crop.height)?;
                let mapped = fm_model::CropRect::new(x, y, width, height);
                placed.crop = Some(mapped);
                (crop, mapped)
            }
            None => (
                fm_model::CropRect::new(0, 0, self.project_width, self.project_height),
                fm_model::CropRect::new(0, 0, self.width, self.height),
            ),
        };
        if let Some(mask) = layer.mask {
            // Mask coordinates are relative to the cropped source.
            let (x, width) = self.columns(crop.x.checked_add(mask.x)?, mask.width)?;
            let (y, height) = self.rows(crop.y.checked_add(mask.y)?, mask.height)?;
            // Clamp into the mapped crop, still keeping one pixel.
            let x = x
                .saturating_sub(mapped.x)
                .min(mapped.width.saturating_sub(1));
            let y = y
                .saturating_sub(mapped.y)
                .min(mapped.height.saturating_sub(1));
            let width = width.min(mapped.width.saturating_sub(x));
            let height = height.min(mapped.height.saturating_sub(y));
            placed.mask = Some(fm_model::RectMask {
                x,
                y,
                width,
                height,
                ..mask
            });
        }
        Some(Placement::Draw(placed))
    }

    /// Maps the in-bounds project column span `start..start + length` onto
    /// the monitor grid as `(start, length)`, keeping at least one pixel. An
    /// empty span stays empty so the compositor refuses it as before.
    fn columns(self, start: u32, length: u32) -> Option<(u32, u32)> {
        span(start, length, self.width, |value| self.column(value))
    }

    fn rows(self, start: u32, length: u32) -> Option<(u32, u32)> {
        span(start, length, self.height, |value| self.row(value))
    }
}

fn span(
    start: u32,
    length: u32,
    limit: u32,
    map: impl Fn(i64) -> Option<i64>,
) -> Option<(u32, u32)> {
    let limit = i64::from(limit);
    let first = map(i64::from(start))?.clamp(0, limit - 1);
    if length == 0 {
        return Some((u32::try_from(first).ok()?, 0));
    }
    let end = map(i64::from(start) + i64::from(length))?.clamp(first + 1, limit);
    Some((u32::try_from(first).ok()?, u32::try_from(end - first).ok()?))
}

/// `ceil(value * to / from)` for a signed coordinate.
fn map_edge(value: i64, to: u32, from: u32) -> Option<i64> {
    let scaled = value.checked_mul(i64::from(to))?;
    let from = i64::from(from);
    if from == 0 {
        return None;
    }
    Some(scaled.div_euclid(from) + i64::from(scaled.rem_euclid(from) != 0))
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
