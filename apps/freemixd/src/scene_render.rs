//! CPU-reference composition of Scene inputs for the monitor tap and exact
//! scene previews.
//!
//! Adapted from the CLI's scene renderer. Layer geometry, crops, and masks are
//! authored in project pixels, but a monitor picture is at most 640x360, so
//! the scene is composed directly at the monitor size: sources are rendered at
//! the monitor size and every project-pixel coordinate is mapped onto the
//! monitor grid first. Composing at the project size and downscaling cost
//! roughly the project/monitor area ratio more (9x at 1080p, seconds per
//! picture in a debug build) for a picture that nearest-neighbor downscaling
//! then mostly threw away.
//!
//! Leaf pictures come from a [`SceneSources`]: the simulated pipeline draws
//! enabled simulated inputs (the monitor tap and a simulated daemon's
//! previews), and a native daemon's exact previews draw the latest frames
//! its media pipeline holds ([`crate::scene_preview`]). A layer naming a
//! nested scene (directly or through a Scene input) draws that scene
//! composed on its own canvas at the same grid in its Hold pose, as the
//! compositor's reference meaning of a nested scene defines it
//! (`fm_compositor::flatten`): background, layers, clipped to the canvas,
//! and the premultiplied picture then used as the layer's source. Nesting is
//! bounded ([`MAX_NESTED_SCENE_DEPTH`] levels, [`MAX_NESTED_SCENES`]
//! distinct scenes per composition, each composed once). A source the
//! [`SceneSources`] cannot draw, effects the CPU reference rejects, crops and
//! masks outside the project-size source, or nesting past those bounds yield
//! `None` rather than an invented picture, exactly as at the project size.
//!
//! A composition grades like the native path (SW-019) when given the
//! project's compiled grades: each leaf layer's source carries its input
//! grade and then the layer's own scene layer grade, both applied by the
//! compositor after the layer's effects and before its transform. A Scene
//! input's own input grade colours the composed scene picture it shows: a
//! layer drawing a nested scene through a Scene input carries that input's
//! grade and then its own layer grade over the nested scene's graded
//! layers (a layer naming the scene directly carries only its layer
//! grade), and a scene composed for a Scene input ([`render_scene`],
//! [`render_scene_model`] with `shown_by`) is graded with that input's
//! grade after its layers are composed.

use std::cell::RefCell;
use std::collections::BTreeMap;

use fm_compositor::{
    CompositionPlan, CpuSourceFrame, CropRect, Effect, FlattenLimits, ImageFrame, OutputTarget,
    RectMask, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform, compile_scene, execute_cpu,
};
use fm_engine::CompiledProjectGrades;
use fm_model::fm_animation::ClipState;
use fm_model::{Input, InputKind, Project, SceneLayerRef, SimulatedVideo, SourceRef};
use fm_sim::SimulatedPipeline;
use fm_types::{InputId, SceneId};

/// Deepest chain of nested scenes below the composed scene (the composed
/// scene alone is depth 0), as the compositor's flatten planner bounds it.
pub(crate) const MAX_NESTED_SCENE_DEPTH: usize = FlattenLimits::DEFAULT_MAX_DEPTH;

/// Most distinct nested scenes one composition draws, each composed once.
pub(crate) const MAX_NESTED_SCENES: usize = FlattenLimits::DEFAULT_MAX_SCENES;

/// Where a composed scene's leaf (non-scene) pictures come from.
pub(crate) trait SceneSources {
    /// Whether leaf input `input` can be drawn at all. Decided per input,
    /// independent of the grid, so a layer too thin to cover a pixel still
    /// refuses a scene whose source cannot be drawn.
    fn draws(&self, input: &Input) -> bool;

    /// The picture of leaf input `input` at `frame_number`, `width`x`height`
    /// (the composition grid, the whole project canvas mapped onto it), in
    /// the straight-alpha RGBA8 a scene layer reads, or `None` when it
    /// cannot be drawn honestly.
    fn leaf(&self, input: &Input, frame_number: u64, width: u32, height: u32)
    -> Option<ImageFrame>;
}

/// The simulated pipeline draws enabled simulated inputs at its own size.
impl SceneSources for SimulatedPipeline {
    fn draws(&self, input: &Input) -> bool {
        is_simulated(input)
    }

    fn leaf(
        &self,
        input: &Input,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Option<ImageFrame> {
        if !is_simulated(input) || (self.width(), self.height()) != (width, height) {
            return None;
        }
        self.render_source(input.id, frame_number).ok()
    }
}

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

/// Composes the scene Scene input `input` shows at the `pipeline`
/// dimensions (the fitted monitor size), mapping the scene's project-pixel
/// geometry onto that grid, with every layer in the animation `pose` (see
/// [`fm_model::Scene::posed`]) and graded with `grades` (each leaf layer's
/// input grade, then its own layer grade, then `input`'s grade over the
/// composed picture). Returns `None` when the scene cannot be rendered
/// honestly.
///
/// Edges map with the same rule nearest-neighbor downscaling samples by
/// (output pixel `x` shows project pixel `floor(x * project / output)`), so a
/// layer covers the same monitor pixels it would after composing at the
/// project size and downscaling; a layer too thin to cover any monitor pixel
/// is skipped, as downscaling would have dropped it.
pub(crate) fn render_scene(
    project: &Project,
    input: InputId,
    pipeline: &SimulatedPipeline,
    frame_number: u64,
    pose: ClipState,
    grades: &CompiledProjectGrades,
) -> Option<ImageFrame> {
    let scene = scene_of_input(project, input)?;
    let model = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)?;
    render_scene_model(
        project,
        model,
        pipeline,
        (pipeline.width(), pipeline.height()),
        frame_number,
        pose,
        Some(grades),
        Some(input),
    )
}

/// [`render_scene`] of a scene model rather than one of `project`'s (an
/// exact scene preview renders the engine's scene, possibly with a draft
/// animation) at the `size` grid, its leaves drawn by `sources`; its
/// layers' inputs and nested scenes still come from `project`. `grades`,
/// when given, grades the layers as [`render_scene`] describes and, with
/// `shown_by`, the composed picture with that Scene input's grade.
#[allow(clippy::too_many_arguments)] // one composition: scene, grid, time, grades
pub(crate) fn render_scene_model(
    project: &Project,
    scene: &fm_model::Scene,
    sources: &dyn SceneSources,
    (width, height): (u32, u32),
    frame_number: u64,
    pose: ClipState,
    grades: Option<&CompiledProjectGrades>,
    shown_by: Option<InputId>,
) -> Option<ImageFrame> {
    let dimensions = project.settings().video.dimensions;
    let grid = MonitorGrid {
        project_width: dimensions.width(),
        project_height: dimensions.height(),
        width,
        height,
    };
    let place = |layer: &fm_model::Layer| grid.layer(layer);
    let composed = Composer::new(project, sources, (width, height), frame_number, &place)
        .with_grades(grades)
        .compose(scene, pose, 0)?;
    match grades
        .zip(shown_by)
        .and_then(|(grades, input)| grades.input(input))
    {
        Some(grade) => grade_premultiplied(&composed, grade),
        None => Some(composed),
    }
}

/// Grades a composed (premultiplied RGBA8) picture on its straight colour,
/// as the compositor grades a premultiplied layer: straightened (rounded),
/// graded, and premultiplied again, alpha unchanged. Opaque pixels are
/// graded exactly as they are.
pub(crate) fn grade_premultiplied(
    image: &ImageFrame,
    grade: &fm_compositor::CompiledGrade,
) -> Option<ImageFrame> {
    let width = usize::try_from(image.width()).ok()?;
    let row_bytes = width.checked_mul(4)?;
    let height = usize::try_from(image.height()).ok()?;
    let mut straight = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for row in image.pixels().chunks(image.stride()).take(height) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            let alpha = pixel[3];
            let unpremultiply = |channel: u8| match alpha {
                0 => 0,
                u8::MAX => channel,
                _ => u8::try_from(
                    (u16::from(channel) * 255 + u16::from(alpha) / 2) / u16::from(alpha),
                )
                .unwrap_or(u8::MAX),
            };
            straight.extend_from_slice(&[
                unpremultiply(pixel[0]),
                unpremultiply(pixel[1]),
                unpremultiply(pixel[2]),
                alpha,
            ]);
        }
    }
    let straight = ImageFrame::new(image.width(), image.height(), row_bytes, straight).ok()?;
    let graded = fm_compositor::apply_grade(&straight, grade).ok()?;
    fm_video::premultiply_alpha(&graded).ok()
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
    let scene = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)?;
    let place = |layer: &fm_model::Layer| Some(Placement::Draw(layer.clone()));
    let composed = Composer::new(
        project,
        project_pipeline,
        (project_pipeline.width(), project_pipeline.height()),
        frame_number,
        &place,
    )
    .compose(scene, pose, 0)?;
    if composed.width() == width && composed.height() == height {
        return Some(composed);
    }
    fm_video::scale_nearest(&composed, width, height).ok()
}

/// A pipeline at exactly `width`x`height` holding every simulated input of
/// `project` (as the monitor tap builds its own), or `None` when the size
/// cannot form a picture.
pub(crate) fn simulated_pipeline(
    project: &Project,
    width: u32,
    height: u32,
) -> Option<SimulatedPipeline> {
    let mut pipeline = SimulatedPipeline::new(width, height).ok()?;
    for input in project.inputs() {
        let InputKind::Simulated(simulated) = &input.kind else {
            continue;
        };
        let pattern = match simulated.video {
            SimulatedVideo::Bars => fm_sim::SourcePattern::Bars,
            SimulatedVideo::Solid(color) => fm_sim::SourcePattern::Solid(Rgba8::new(
                color.red,
                color.green,
                color.blue,
                color.alpha,
            )),
        };
        pipeline
            .register(fm_sim::SimulatedSource::new(input.id, pattern))
            .ok()?;
    }
    Some(pipeline)
}

/// Where one project-pixel layer lands on the composition grid.
enum Placement {
    Draw(fm_model::Layer),
    /// The layer covers no output pixel.
    Skip,
}

/// What one layer draws: a leaf input's picture or a nested scene's.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Feed {
    Leaf(InputId),
    Scene(SceneId),
}

/// Maps one project-pixel layer onto the grid; `None` refuses the scene.
type Place<'a> = &'a dyn Fn(&fm_model::Layer) -> Option<Placement>;

/// One composition: its grid, its leaf sources, the grades it applies, and
/// the nested scenes it already composed (each at most once,
/// [`MAX_NESTED_SCENES`] at most).
struct Composer<'a> {
    project: &'a Project,
    sources: &'a dyn SceneSources,
    grades: Option<&'a CompiledProjectGrades>,
    width: u32,
    height: u32,
    frame_number: u64,
    place: Place<'a>,
    nested: RefCell<BTreeMap<SceneId, Option<ImageFrame>>>,
}

impl<'a> Composer<'a> {
    fn new(
        project: &'a Project,
        sources: &'a dyn SceneSources,
        (width, height): (u32, u32),
        frame_number: u64,
        place: Place<'a>,
    ) -> Self {
        Self {
            project,
            sources,
            grades: None,
            width,
            height,
            frame_number,
            place,
            nested: RefCell::new(BTreeMap::new()),
        }
    }

    /// Grades every layer with `grades` (SW-019); `None` composes ungraded.
    fn with_grades(mut self, grades: Option<&'a CompiledProjectGrades>) -> Self {
        self.grades = grades;
        self
    }

    /// `layer` of `scene` graded: the input grade of the input it names (a
    /// leaf's, or a Scene input's over the nested scene's picture, whose
    /// layers were graded when it was composed), then the layer's own
    /// grade. A layer naming a scene directly carries only its layer grade.
    fn graded(
        &self,
        mut layer: SourceLayer,
        scene: SceneId,
        source: &fm_model::Layer,
    ) -> SourceLayer {
        let Some(grades) = self.grades else {
            return layer;
        };
        if let SourceRef::Input(input) = source.source
            && let Some(grade) = grades.input(input)
        {
            layer = layer.with_grade(grade.clone());
        }
        if let Some(grade) = grades.layer(SceneLayerRef::new(scene, source.id)) {
            layer = layer.with_layer_grade(grade.clone());
        }
        layer
    }

    fn input(&self, input: InputId) -> Option<&'a Input> {
        self.project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
    }

    /// What `source` draws: a nested scene for a scene reference or a
    /// Scene input, a leaf otherwise; `None` for a missing input.
    fn feed(&self, source: SourceRef) -> Option<Feed> {
        match source {
            SourceRef::Scene(scene) => Some(Feed::Scene(scene)),
            SourceRef::Input(input) => Some(match self.input(input)?.kind {
                InputKind::Scene { scene_id, .. } => Feed::Scene(scene_id),
                _ => Feed::Leaf(input),
            }),
        }
    }

    /// The picture `feed` draws at the grid size, for a scene `depth`
    /// scenes below the composed one.
    fn picture(&self, feed: Feed, depth: usize) -> Option<ImageFrame> {
        let scene = match feed {
            Feed::Leaf(input) => {
                return self.sources.leaf(
                    self.input(input)?,
                    self.frame_number,
                    self.width,
                    self.height,
                );
            }
            Feed::Scene(scene) => scene,
        };
        if depth >= MAX_NESTED_SCENE_DEPTH {
            return None;
        }
        if let Some(composed) = self.nested.borrow().get(&scene) {
            return composed.clone();
        }
        if self.nested.borrow().len() >= MAX_NESTED_SCENES {
            return None;
        }
        // Reserve the slot first: a scene reached again while it is being
        // composed (a cycle validation refuses) draws nothing instead of
        // recursing.
        self.nested.borrow_mut().insert(scene, None);
        let model = self
            .project
            .scenes()
            .iter()
            .find(|candidate| candidate.id == scene)?;
        // Nested scenes hold: the engine plays In and Out only for the
        // scene a Scene input shows on Program or Preview.
        let composed = self.compose(model, ClipState::Hold, depth + 1);
        self.nested.borrow_mut().insert(scene, composed.clone());
        composed
    }

    /// Composes `scene` posed at `pose` on the grid, `depth` scenes below
    /// the composed one.
    fn compose(
        &self,
        scene: &fm_model::Scene,
        pose: ClipState,
        depth: usize,
    ) -> Option<ImageFrame> {
        let dimensions = self.project.settings().video.dimensions;
        // The scene as the engine poses it at this frame (GX-011 stage 5).
        let model = scene.posed(pose, dimensions.width(), dimensions.height());
        let model = &model;
        let background = model.background;
        let mut composition = Scene::new(
            self.width,
            self.height,
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
            let feed = self.feed(layer.source)?;
            // Acceptance is decided in project pixels, independent of the grid.
            if let Feed::Leaf(input) = feed
                && !self.sources.draws(self.input(input)?)
            {
                return None;
            }
            if !bounds_fit(
                layer.crop,
                layer.mask,
                dimensions.width(),
                dimensions.height(),
            ) {
                return None;
            }
            let Placement::Draw(placed) = (self.place)(layer)? else {
                continue;
            };
            let next = SourceId::new(u64::try_from(sources.len()).ok()?);
            let token = *sources.entry(feed).or_insert(next);
            composition.push_layer(self.graded(compositor_layer(token, &placed), model.id, layer));
        }

        let (plan, _) = compile_scene(&composition, OutputTarget::Program).ok()?;
        let frames = sources
            .into_iter()
            .map(|(feed, source)| self.picture(feed, depth).map(|frame| (source, frame)))
            .collect::<Option<Vec<_>>>()?;
        let bindings = frames
            .iter()
            .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
            .collect::<Vec<_>>();
        execute_cpu(&plan, &bindings).ok()
    }
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
        )
        .with_tilt(geometry.tilt_millidegrees);
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
        result = result.with_key(compositor_key(key));
    }
    for effect in &layer.effects {
        result = result.with_effect(Effect::new(effect.name.clone(), effect.parameters.clone()));
    }
    result
}

/// `key` as the compositor's chroma or luma key (IN-020).
fn compositor_key(key: fm_model::LayerKey) -> fm_compositor::Key {
    match key {
        fm_model::LayerKey::Chroma(chroma) => {
            fm_compositor::Key::Chroma(fm_compositor::ChromaKey::new(
                fm_compositor::Rgba8::new(chroma.red, chroma.green, chroma.blue, u8::MAX),
                chroma.similarity,
                chroma.smoothness,
                chroma.spill,
            ))
        }
        fm_model::LayerKey::Luma(luma) => fm_compositor::Key::Luma(
            fm_compositor::LumaKey::new(luma.low, luma.softness, luma.invert).with_high(luma.high),
        ),
    }
}
