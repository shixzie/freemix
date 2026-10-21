//! Flatten layers (SW-018): precompose nested scenes at explicit graph
//! boundaries.
//!
//! A [`SceneGraph`] holds compositor scenes keyed by [`SourceId`]. A layer
//! whose source names another scene of the graph draws that scene, and the
//! reference meaning of such a layer is fixed: the nested scene is composed on
//! its own canvas (its background, its layers, clipped to its size) into a
//! premultiplied RGBA8 picture, and the layer then treats that picture as its
//! source. Rendering every reference anew, recursively, is the *unflattened*
//! composition.
//!
//! [`plan_flatten`] turns one root scene into a deterministic, bounded
//! schedule that produces exactly the same pixels with less work:
//!
//! - **Hoisting.** A reference that is provably pixel-exact to inline is
//!   replaced by the nested scene's single layer, with no intermediate
//!   target. See [`plan_flatten`] for the exact conditions.
//! - **Shared intermediates.** Every other referenced scene is precomposed
//!   once into an intermediate target, kept alive until its last consuming
//!   scene has rendered, and then released. A scene used by many layers or
//!   many parent scenes is still rendered exactly once per frame.
//! - **Ordering.** Dependencies of a scene are rendered in descending order
//!   of their working-set surplus (peak bytes needed to produce the
//!   dependency minus the bytes it then occupies), the classic
//!   Sethi-Ullman ordering, so large subtrees are produced before small
//!   results pile up. Ties keep first-use (plan layer) order.
//! - **Bounds.** Nesting depth, reachable scene count, simultaneously live
//!   intermediates, and peak intermediate bytes are checked against
//!   [`FlattenLimits`] at plan time; the executor measures the same peak.
//!
//! The plan reports each intermediate's memory cost and quality cost: how
//! many consumers resample it a second time with nearest-neighbour scaling
//! and how many reinterpret its premultiplied pixels as straight alpha.
//! Flattening happens inside the tick that consumes it, so it adds no frame
//! of delay; [`execute_flatten_cpu_frame`] carries the caller's output
//! timing through unchanged.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use fm_frame::{CpuVideoFrame, MediaTiming};
use fm_video::{ImageFrame, Rgba8, Rotation};

use crate::AlphaMode;
use crate::cpu::{
    CpuExecutionError, CpuSourceFrame, execute_cpu, image_from_cpu_frame, video_frame_from_image,
};
use crate::plan::{CompositionPlan, PlanError, PlanLayer, compile_scene};
use crate::scene::{OutputTarget, Scene, SourceId};

/// How a scene behaves when another scene references it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FlattenMode {
    /// The planner may hoist the scene into its consumer when that is exact
    /// and otherwise precomposes it into a shared intermediate.
    #[default]
    Auto,
    /// A deliberate graph boundary: always precomposed into an intermediate,
    /// never hoisted.
    Precompose,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GraphNode {
    scene: Scene,
    mode: FlattenMode,
}

/// Compositor scenes addressable as layer sources.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SceneGraph {
    nodes: BTreeMap<SourceId, GraphNode>,
}

impl SceneGraph {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
        }
    }

    /// Adds a scene with [`FlattenMode::Auto`].
    ///
    /// # Errors
    /// Returns [`FlattenError::DuplicateScene`] when `id` is already used.
    pub fn insert(&mut self, id: SourceId, scene: Scene) -> Result<(), FlattenError> {
        self.insert_with_mode(id, scene, FlattenMode::Auto)
    }

    /// Adds a scene with an explicit flatten mode.
    ///
    /// # Errors
    /// Returns [`FlattenError::DuplicateScene`] when `id` is already used.
    pub fn insert_with_mode(
        &mut self,
        id: SourceId,
        scene: Scene,
        mode: FlattenMode,
    ) -> Result<(), FlattenError> {
        if self.nodes.contains_key(&id) {
            return Err(FlattenError::DuplicateScene(id));
        }
        self.nodes.insert(id, GraphNode { scene, mode });
        Ok(())
    }

    #[must_use]
    pub fn scene(&self, id: SourceId) -> Option<&Scene> {
        self.nodes.get(&id).map(|node| &node.scene)
    }

    #[must_use]
    pub fn mode(&self, id: SourceId) -> Option<FlattenMode> {
        self.nodes.get(&id).map(|node| node.mode)
    }

    #[must_use]
    pub fn contains(&self, id: SourceId) -> bool {
        self.nodes.contains_key(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Plan-time bounds for one flattened composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlattenLimits {
    /// Longest chain of scene references below the root (the root alone is
    /// depth `0`).
    pub max_depth: usize,
    /// Scenes reachable from the root, the root included.
    pub max_scenes: usize,
    /// Intermediate targets alive at the same time.
    pub max_live_intermediates: usize,
    /// Peak bytes of simultaneously live RGBA8 intermediate targets. The
    /// root output and the CPU executor's per-layer scratch are not counted.
    pub max_intermediate_bytes: u64,
}

impl FlattenLimits {
    pub const DEFAULT_MAX_DEPTH: usize = 16;
    pub const DEFAULT_MAX_SCENES: usize = 64;
    pub const DEFAULT_MAX_LIVE_INTERMEDIATES: usize = 32;
    pub const DEFAULT_MAX_INTERMEDIATE_BYTES: u64 = 512 * 1024 * 1024;
}

impl Default for FlattenLimits {
    fn default() -> Self {
        Self {
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_scenes: Self::DEFAULT_MAX_SCENES,
            max_live_intermediates: Self::DEFAULT_MAX_LIVE_INTERMEDIATES,
            max_intermediate_bytes: Self::DEFAULT_MAX_INTERMEDIATE_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlattenError {
    DuplicateScene(SourceId),
    UnknownScene(SourceId),
    SceneCycle {
        scene: SourceId,
    },
    DepthExceeded {
        scene: SourceId,
        depth: usize,
        maximum: usize,
    },
    TooManyScenes {
        maximum: usize,
    },
    Plan {
        scene: SourceId,
        error: PlanError,
    },
    LiveIntermediatesExceeded {
        required: usize,
        maximum: usize,
    },
    IntermediateBytesExceeded {
        required: u64,
        maximum: u64,
    },
}

impl fmt::Display for FlattenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateScene(scene) => {
                write!(formatter, "scene {} is already in the graph", scene.0)
            }
            Self::UnknownScene(scene) => write!(formatter, "scene {} is not in the graph", scene.0),
            Self::SceneCycle { scene } => write!(formatter, "scene {} reaches itself", scene.0),
            Self::DepthExceeded {
                scene,
                depth,
                maximum,
            } => write!(
                formatter,
                "scene {} nests {depth} levels deep; maximum is {maximum}",
                scene.0
            ),
            Self::TooManyScenes { maximum } => write!(
                formatter,
                "more than {maximum} scenes are reachable from the root"
            ),
            Self::Plan { scene, error } => write!(formatter, "scene {}: {error}", scene.0),
            Self::LiveIntermediatesExceeded { required, maximum } => write!(
                formatter,
                "flattening needs {required} live intermediates; maximum is {maximum}"
            ),
            Self::IntermediateBytesExceeded { required, maximum } => write!(
                formatter,
                "flattening needs {required} intermediate bytes; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for FlattenError {}

/// One step of a flattened composition, in execution order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlattenStep {
    /// Precompose `scene` into intermediate slot `slot`.
    Render { scene: SourceId, slot: usize },
    /// The last consumer of `scene` has rendered; `slot` is free again.
    Release { scene: SourceId, slot: usize },
    /// Compose the root scene into the output.
    RenderRoot,
}

/// Memory and quality cost of one precomposed scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlattenedScene {
    pub scene: SourceId,
    pub width: u32,
    pub height: u32,
    /// Packed RGBA8 bytes the intermediate occupies while alive.
    pub bytes: u64,
    pub slot: usize,
    /// Layers (across all rendered scenes) that read the intermediate as a
    /// fill or as a key/fill key source.
    pub consumer_layers: usize,
    /// Distinct rendered scenes that read the intermediate.
    pub consumer_scenes: usize,
    /// Consumer layers whose transform size differs from the intermediate,
    /// so its already resampled pixels are nearest-resampled again.
    pub rescaled_uses: usize,
    /// Consumer layers declaring straight alpha, which re-premultiply the
    /// premultiplied intermediate.
    pub straight_alpha_uses: usize,
}

/// A layer that was replaced by the single layer of the scene it drew.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HoistedLayer {
    /// The rendered scene that now draws the hoisted layer.
    pub consumer: SourceId,
    /// The consumer layer's index in its scene's layer list.
    pub layer: usize,
    /// The scene that needed no intermediate.
    pub bypassed: SourceId,
}

/// A deterministic, bounded schedule for one flattened root scene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlattenPlan {
    root: SourceId,
    target: OutputTarget,
    plans: BTreeMap<SourceId, CompositionPlan>,
    scene_ids: BTreeSet<SourceId>,
    steps: Vec<FlattenStep>,
    intermediates: Vec<FlattenedScene>,
    hoisted: Vec<HoistedLayer>,
    peak_live_bytes: u64,
    peak_live_intermediates: usize,
}

impl FlattenPlan {
    #[must_use]
    pub const fn root(&self) -> SourceId {
        self.root
    }

    #[must_use]
    pub const fn target(&self) -> OutputTarget {
        self.target
    }

    /// The executable plan of a rendered scene (the root or an
    /// intermediate), with hoisted layers already substituted.
    #[must_use]
    pub fn scene_plan(&self, scene: SourceId) -> Option<&CompositionPlan> {
        self.plans.get(&scene)
    }

    #[must_use]
    pub fn steps(&self) -> &[FlattenStep] {
        &self.steps
    }

    /// Precomposed scenes in render order.
    #[must_use]
    pub fn intermediates(&self) -> &[FlattenedScene] {
        &self.intermediates
    }

    #[must_use]
    pub fn hoisted(&self) -> &[HoistedLayer] {
        &self.hoisted
    }

    #[must_use]
    pub const fn peak_live_bytes(&self) -> u64 {
        self.peak_live_bytes
    }

    #[must_use]
    pub const fn peak_live_intermediates(&self) -> usize {
        self.peak_live_intermediates
    }

    /// Packed RGBA8 bytes of the root output, which is not an intermediate.
    #[must_use]
    pub fn output_bytes(&self) -> u64 {
        self.plans.get(&self.root).map_or(0, plan_bytes)
    }

    /// Every scene the root reaches, including hoisted-away ones. Leaf
    /// sources must not reuse these identifiers.
    #[must_use]
    pub fn scene_ids(&self) -> &BTreeSet<SourceId> {
        &self.scene_ids
    }
}

/// Plans the flattened composition of `root` for `target`.
///
/// Every reachable scene is compiled with [`compile_scene`] for `target`.
/// A layer drawing scene `S` is hoisted, replacing it with `S`'s only layer,
/// exactly when all of these hold, because then the precomposed picture of
/// `S` is that layer composed over transparent black (exact: `x` over
/// transparent black is `x` with the integer source-over) and the consumer
/// copies it pixel for pixel:
///
/// - `S` is [`FlattenMode::Auto`], has a fully transparent background,
///   exactly one planned layer, and no planned safe-area guides;
/// - that layer's transformed rectangle lies inside `S`'s canvas, so
///   precomposition clips nothing, and it has no light wrap, whose edges
///   take the light of `S`'s transparent background;
/// - the consumer layer is premultiplied, fully opaque, unrotated, sized
///   exactly to `S`, has no crop, mask, key, key/fill, refinement, effects,
///   or border, and the summed translation does not overflow.
///
/// Every other referenced scene becomes one shared intermediate.
///
/// # Errors
/// Returns a typed error for unknown roots, cycles, depth or scene-count
/// overruns, malformed scenes, or schedules exceeding `limits`.
pub fn plan_flatten(
    graph: &SceneGraph,
    root: SourceId,
    target: OutputTarget,
    limits: FlattenLimits,
) -> Result<FlattenPlan, FlattenError> {
    if !graph.contains(root) {
        return Err(FlattenError::UnknownScene(root));
    }
    let mut compiler = Compiler {
        graph,
        target,
        limits,
        path: Vec::new(),
        compiled: BTreeMap::new(),
        heights: BTreeMap::new(),
    };
    compiler.visit(root, 0)?;
    let reachable = compiler.compiled;
    let scene_ids = reachable.keys().copied().collect::<BTreeSet<_>>();

    // Substitute hoistable layers, rendering-scene by rendering-scene.
    let mut plans = BTreeMap::new();
    let mut hoisted = Vec::new();
    let mut pending = vec![root];
    while let Some(scene) = pending.pop() {
        if plans.contains_key(&scene) {
            continue;
        }
        let plan = &reachable[&scene];
        let layers = plan
            .layers()
            .iter()
            .map(|layer| hoist(graph, &reachable, scene, layer, &mut hoisted))
            .collect::<Vec<_>>();
        let effective = plan.with_layers(layers);
        let mut dependencies = dependencies(graph, &effective);
        dependencies.reverse();
        pending.extend(dependencies);
        plans.insert(scene, effective);
    }

    let mut costs = intermediate_costs(graph, &plans);

    let mut needs = BTreeMap::new();
    let mut orders = BTreeMap::new();
    working_set(graph, root, root, &plans, &mut needs, &mut orders);

    let mut schedule = Schedule {
        remaining: costs
            .iter()
            .map(|(&scene, cost)| (scene, cost.consumer_scenes))
            .collect(),
        ..Schedule::default()
    };
    schedule.visit(graph, root, root, &plans, &orders, &mut costs);

    if schedule.peak_live > limits.max_live_intermediates {
        return Err(FlattenError::LiveIntermediatesExceeded {
            required: schedule.peak_live,
            maximum: limits.max_live_intermediates,
        });
    }
    if schedule.peak_bytes > limits.max_intermediate_bytes {
        return Err(FlattenError::IntermediateBytesExceeded {
            required: schedule.peak_bytes,
            maximum: limits.max_intermediate_bytes,
        });
    }
    let intermediates = schedule.rendered.iter().map(|scene| costs[scene]).collect();
    Ok(FlattenPlan {
        root,
        target,
        plans,
        scene_ids,
        steps: schedule.steps,
        intermediates,
        hoisted,
        peak_live_bytes: schedule.peak_bytes,
        peak_live_intermediates: schedule.peak_live,
    })
}

/// Memory and quality cost of every scene some rendered plan reads.
fn intermediate_costs(
    graph: &SceneGraph,
    plans: &BTreeMap<SourceId, CompositionPlan>,
) -> BTreeMap<SourceId, FlattenedScene> {
    let mut costs = BTreeMap::<SourceId, FlattenedScene>::new();
    for plan in plans.values() {
        for dependency in dependencies(graph, plan) {
            let dependency_plan = &plans[&dependency];
            let cost = costs.entry(dependency).or_insert(FlattenedScene {
                scene: dependency,
                width: dependency_plan.width(),
                height: dependency_plan.height(),
                bytes: plan_bytes(dependency_plan),
                slot: 0,
                consumer_layers: 0,
                consumer_scenes: 0,
                rescaled_uses: 0,
                straight_alpha_uses: 0,
            });
            cost.consumer_scenes += 1;
        }
        for layer in plan.layers() {
            if let Some(cost) = costs.get_mut(&layer.source()) {
                cost.consumer_layers += 1;
                let transform = layer.transform();
                if transform.scale_width != cost.width || transform.scale_height != cost.height {
                    cost.rescaled_uses += 1;
                }
                if layer.alpha_mode() == AlphaMode::Straight {
                    cost.straight_alpha_uses += 1;
                }
            }
        }
    }
    // Key/fill key sources are counted after fills so that a dependency
    // first seen as a key source still exists in `costs`.
    for plan in plans.values() {
        for layer in plan.layers() {
            if let Some(fill) = layer.key_fill()
                && let Some(cost) = costs.get_mut(&fill.key_source)
            {
                cost.consumer_layers += 1;
            }
        }
    }
    costs
}

struct Compiler<'g> {
    graph: &'g SceneGraph,
    target: OutputTarget,
    limits: FlattenLimits,
    path: Vec<SourceId>,
    compiled: BTreeMap<SourceId, CompositionPlan>,
    heights: BTreeMap<SourceId, usize>,
}

impl Compiler<'_> {
    /// Compiles every scene reachable from `scene`, reached at `depth`, and
    /// returns the height of its reference tree. Recursion is bounded by
    /// `limits.max_depth`.
    fn visit(&mut self, scene: SourceId, depth: usize) -> Result<usize, FlattenError> {
        if self.path.contains(&scene) {
            return Err(FlattenError::SceneCycle { scene });
        }
        if let Some(&height) = self.heights.get(&scene) {
            check_depth(scene, depth + height, self.limits)?;
            return Ok(height);
        }
        check_depth(scene, depth, self.limits)?;
        if self.compiled.len() >= self.limits.max_scenes {
            return Err(FlattenError::TooManyScenes {
                maximum: self.limits.max_scenes,
            });
        }
        let node = self
            .graph
            .nodes
            .get(&scene)
            .ok_or(FlattenError::UnknownScene(scene))?;
        let (plan, _) = compile_scene(&node.scene, self.target)
            .map_err(|error| FlattenError::Plan { scene, error })?;
        let children = dependencies(self.graph, &plan);
        self.compiled.insert(scene, plan);
        self.path.push(scene);
        let mut height = 0;
        for child in children {
            height = height.max(self.visit(child, depth + 1)? + 1);
        }
        self.path.pop();
        self.heights.insert(scene, height);
        Ok(height)
    }
}

fn check_depth(scene: SourceId, depth: usize, limits: FlattenLimits) -> Result<(), FlattenError> {
    if depth > limits.max_depth {
        return Err(FlattenError::DepthExceeded {
            scene,
            depth,
            maximum: limits.max_depth,
        });
    }
    Ok(())
}

/// Graph scenes a plan reads, as fills or key sources, in first-use order.
fn dependencies(graph: &SceneGraph, plan: &CompositionPlan) -> Vec<SourceId> {
    let mut found = Vec::new();
    for layer in plan.layers() {
        let key_source = layer.key_fill().map(|fill| fill.key_source);
        for source in core::iter::once(layer.source()).chain(key_source) {
            if graph.contains(source) && !found.contains(&source) {
                found.push(source);
            }
        }
    }
    found
}

fn hoist(
    graph: &SceneGraph,
    compiled: &BTreeMap<SourceId, CompositionPlan>,
    consumer: SourceId,
    layer: &PlanLayer,
    hoisted: &mut Vec<HoistedLayer>,
) -> PlanLayer {
    let mut current = layer.clone();
    // Terminates: the graph below `consumer` is acyclic and depth-bounded.
    while let Some(node) = graph.nodes.get(&current.source()) {
        let Some(next) = hoisted_layer(node.mode, &compiled[&current.source()], &current) else {
            break;
        };
        hoisted.push(HoistedLayer {
            consumer,
            layer: layer.scene_index(),
            bypassed: current.source(),
        });
        current = next;
    }
    current
}

fn hoisted_layer(
    mode: FlattenMode,
    nested: &CompositionPlan,
    outer: &PlanLayer,
) -> Option<PlanLayer> {
    let transform = outer.transform();
    let outer_is_plain_copy = outer.alpha_mode() == AlphaMode::Premultiplied
        && outer.opacity() == u8::MAX
        && outer.crop().is_none()
        && outer.mask().is_none()
        && outer.key().is_none()
        && outer.key_fill().is_none()
        && outer.key_refinement().is_none()
        && outer.effects().is_empty()
        && outer.grade().is_none()
        && outer.layer_grade().is_none()
        && outer.inset_border_width() == 0
        && !outer.is_tilted()
        && transform.rotation == Rotation::Deg0
        && transform.scale_width == nested.width()
        && transform.scale_height == nested.height();
    if mode != FlattenMode::Auto
        || !outer_is_plain_copy
        || nested.background() != Rgba8::new(0, 0, 0, 0)
        || !nested.safe_areas().is_empty()
    {
        return None;
    }
    let [inner] = nested.layers() else {
        return None;
    };
    // A tilted inner layer can reach past the nested scene's edges, which
    // the nested render clips; hoisting would not.
    if inner.is_tilted() {
        return None;
    }
    // A light wrap takes the light of what lies beneath its layer: inside
    // the nested scene that is its transparent background, hoisted it
    // would be the consumer's layers.
    if inner
        .key_refinement()
        .is_some_and(|refinement| refinement.light_wrap.is_some())
    {
        return None;
    }
    let inner_transform = inner.transform();
    let (width, height) = match inner_transform.rotation {
        Rotation::Deg0 | Rotation::Deg180 => {
            (inner_transform.scale_width, inner_transform.scale_height)
        }
        Rotation::Deg90 | Rotation::Deg270 => {
            (inner_transform.scale_height, inner_transform.scale_width)
        }
    };
    let inside = inner_transform.translation_x >= 0
        && inner_transform.translation_y >= 0
        && i64::from(inner_transform.translation_x) + i64::from(width) <= i64::from(nested.width())
        && i64::from(inner_transform.translation_y) + i64::from(height)
            <= i64::from(nested.height());
    if !inside {
        return None;
    }
    let translation_x = transform
        .translation_x
        .checked_add(inner_transform.translation_x)?;
    let translation_y = transform
        .translation_y
        .checked_add(inner_transform.translation_y)?;
    Some(outer.hoisted_through(inner, translation_x, translation_y))
}

fn plan_bytes(plan: &CompositionPlan) -> u64 {
    u64::from(plan.width()) * u64::from(plan.height()) * 4
}

/// Bytes `scene` occupies once rendered; the root output is not counted.
fn resident_bytes(
    root: SourceId,
    scene: SourceId,
    plans: &BTreeMap<SourceId, CompositionPlan>,
) -> u64 {
    if scene == root {
        0
    } else {
        plan_bytes(&plans[&scene])
    }
}

/// Peak intermediate bytes needed to render `scene` in isolation, choosing
/// each scene's dependency order by descending `need - resident` surplus.
/// Sharing between subtrees is ignored here; the schedule simulation below
/// measures the exact peak of the order chosen.
fn working_set(
    graph: &SceneGraph,
    root: SourceId,
    scene: SourceId,
    plans: &BTreeMap<SourceId, CompositionPlan>,
    needs: &mut BTreeMap<SourceId, u64>,
    orders: &mut BTreeMap<SourceId, Vec<SourceId>>,
) -> u64 {
    if let Some(&need) = needs.get(&scene) {
        return need;
    }
    let children = dependencies(graph, &plans[&scene]);
    let mut ranked = children
        .iter()
        .enumerate()
        .map(|(first_use, &child)| {
            let need = working_set(graph, root, child, plans, needs, orders);
            let resident = resident_bytes(root, child, plans);
            (
                need.saturating_sub(resident),
                first_use,
                child,
                need,
                resident,
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut held = 0_u64;
    let mut peak = 0_u64;
    for &(_, _, _, need, resident) in &ranked {
        peak = peak.max(held + need);
        held += resident;
    }
    peak = peak.max(held + resident_bytes(root, scene, plans));
    needs.insert(scene, peak);
    orders.insert(
        scene,
        ranked
            .into_iter()
            .map(|(_, _, child, _, _)| child)
            .collect(),
    );
    peak
}

#[derive(Default)]
struct Schedule {
    remaining: BTreeMap<SourceId, usize>,
    done: BTreeSet<SourceId>,
    rendered: Vec<SourceId>,
    slots: Vec<Option<SourceId>>,
    live_bytes: u64,
    peak_bytes: u64,
    peak_live: usize,
    steps: Vec<FlattenStep>,
}

impl Schedule {
    fn visit(
        &mut self,
        graph: &SceneGraph,
        root: SourceId,
        scene: SourceId,
        plans: &BTreeMap<SourceId, CompositionPlan>,
        orders: &BTreeMap<SourceId, Vec<SourceId>>,
        costs: &mut BTreeMap<SourceId, FlattenedScene>,
    ) {
        for &child in &orders[&scene] {
            if !self.done.contains(&child) {
                self.visit(graph, root, child, plans, orders, costs);
            }
        }
        if scene == root {
            self.steps.push(FlattenStep::RenderRoot);
        } else {
            let slot = if let Some(slot) = self.slots.iter().position(Option::is_none) {
                slot
            } else {
                self.slots.push(None);
                self.slots.len() - 1
            };
            self.slots[slot] = Some(scene);
            self.live_bytes += resident_bytes(root, scene, plans);
            self.peak_bytes = self.peak_bytes.max(self.live_bytes);
            self.peak_live = self
                .peak_live
                .max(self.slots.iter().filter(|slot| slot.is_some()).count());
            if let Some(cost) = costs.get_mut(&scene) {
                cost.slot = slot;
            }
            self.rendered.push(scene);
            self.steps.push(FlattenStep::Render { scene, slot });
        }
        self.done.insert(scene);
        for dependency in dependencies(graph, &plans[&scene]) {
            let Some(remaining) = self.remaining.get_mut(&dependency) else {
                continue;
            };
            *remaining -= 1;
            if *remaining == 0
                && let Some(slot) = self.slots.iter().position(|slot| *slot == Some(dependency))
            {
                self.slots[slot] = None;
                self.live_bytes -= resident_bytes(root, dependency, plans);
                self.steps.push(FlattenStep::Release {
                    scene: dependency,
                    slot,
                });
            }
        }
    }
}

#[derive(Debug)]
pub enum FlattenExecutionError {
    /// A leaf binding reuses the identifier of a scene in the plan.
    AmbiguousSource(SourceId),
    /// A portable input frame could not be read.
    Source(CpuExecutionError),
    /// Composing `scene` (an intermediate or the root) failed.
    Scene {
        scene: SourceId,
        error: CpuExecutionError,
    },
}

impl fmt::Display for FlattenExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousSource(source) => write!(
                formatter,
                "source {} is bound as a leaf but names a scene",
                source.0
            ),
            Self::Source(error) => error.fmt(formatter),
            Self::Scene { scene, error } => write!(formatter, "scene {}: {error}", scene.0),
        }
    }
}

impl std::error::Error for FlattenExecutionError {}

/// The composed root plus what execution actually did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlattenOutput {
    pub frame: ImageFrame,
    /// Intermediates composed, in order; the root is not listed.
    pub renders: Vec<SourceId>,
    /// Measured peak bytes of simultaneously held intermediate frames.
    pub peak_live_bytes: u64,
}

/// Executes a flatten plan with the deterministic CPU reference compositor.
///
/// `sources` binds leaf sources only; every scene is produced by the plan.
///
/// # Errors
/// Returns [`FlattenExecutionError`] for ambiguous bindings or any
/// composition failure, tagged with the scene being composed.
pub fn execute_flatten_cpu(
    plan: &FlattenPlan,
    sources: &[CpuSourceFrame<'_>],
) -> Result<FlattenOutput, FlattenExecutionError> {
    if let Some(source) = sources
        .iter()
        .find(|source| plan.scene_ids.contains(&source.source))
    {
        return Err(FlattenExecutionError::AmbiguousSource(source.source));
    }
    let mut slots: Vec<Option<(SourceId, ImageFrame)>> =
        (0..plan.peak_live_intermediates).map(|_| None).collect();
    let mut renders = Vec::new();
    let mut live_bytes = 0_u64;
    let mut peak_live_bytes = 0_u64;
    let mut output = None;
    for step in &plan.steps {
        match *step {
            FlattenStep::Render { scene, slot } => {
                let frame = render_scene(plan, scene, sources, &slots)?;
                live_bytes += frame.pixels().len() as u64;
                peak_live_bytes = peak_live_bytes.max(live_bytes);
                slots[slot] = Some((scene, frame));
                renders.push(scene);
            }
            FlattenStep::Release { slot, .. } => {
                if let Some((_, frame)) = slots[slot].take() {
                    live_bytes -= frame.pixels().len() as u64;
                }
            }
            FlattenStep::RenderRoot => {
                output = Some(render_scene(plan, plan.root, sources, &slots)?);
            }
        }
    }
    let frame = output.ok_or(FlattenExecutionError::Scene {
        scene: plan.root,
        error: CpuExecutionError::MissingSource(plan.root),
    })?;
    Ok(FlattenOutput {
        frame,
        renders,
        peak_live_bytes,
    })
}

/// Executes a flatten plan against portable timed frames. Flattening runs
/// inside the consuming tick, so the output carries `output_timing`
/// unchanged and no frame of delay is added.
///
/// # Errors
/// Returns [`FlattenExecutionError`] for unreadable inputs or any
/// composition failure.
pub fn execute_flatten_cpu_frame(
    plan: &FlattenPlan,
    sources: &[(SourceId, &CpuVideoFrame)],
    output_timing: MediaTiming,
) -> Result<CpuVideoFrame, FlattenExecutionError> {
    let images = sources
        .iter()
        .map(|(source, frame)| Ok((*source, image_from_cpu_frame(frame)?)))
        .collect::<Result<Vec<_>, CpuExecutionError>>()
        .map_err(FlattenExecutionError::Source)?;
    let bindings = images
        .iter()
        .map(|(source, frame)| CpuSourceFrame::new(*source, frame))
        .collect::<Vec<_>>();
    let output = execute_flatten_cpu(plan, &bindings)?;
    video_frame_from_image(&output.frame, output_timing).map_err(|error| {
        FlattenExecutionError::Scene {
            scene: plan.root,
            error,
        }
    })
}

fn render_scene(
    plan: &FlattenPlan,
    scene: SourceId,
    sources: &[CpuSourceFrame<'_>],
    slots: &[Option<(SourceId, ImageFrame)>],
) -> Result<ImageFrame, FlattenExecutionError> {
    let mut bindings = sources.to_vec();
    bindings.extend(
        slots
            .iter()
            .flatten()
            .map(|(id, frame)| CpuSourceFrame::new(*id, frame)),
    );
    execute_cpu(&plan.plans[&scene], &bindings)
        .map_err(|error| FlattenExecutionError::Scene { scene, error })
}
