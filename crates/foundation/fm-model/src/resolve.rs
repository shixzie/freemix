//! Maps any input to the concrete thing a renderer draws for it.
//!
//! Two input kinds do not name pixels directly:
//!
//! - an [`InputKind::Alias`] renders whatever its target chain ends at, with
//!   its own crop and audio strip;
//! - an [`InputKind::MixFeed`] renders the Program or Preview frame committed
//!   at the end of the *previous* tick.
//!
//! The one-frame delay is structural: [`MixFeedHistory::commit`] needs the
//! history mutably, so no frame of the current tick can be committed while a
//! [`FrameContext`] (which borrows the history) is resolving sources for that
//! tick. A mix feed is therefore a terminal of every walk here: resolving it
//! never expands the bus it reads, so Program-as-input on Program is a finite,
//! deterministic recurrence over ticks rather than an unbounded recursion.
//! Before any frame has been committed (or after [`MixFeedHistory::reset`])
//! a mix feed resolves to [`RenderSource::Black`].

use std::collections::{HashMap, HashSet};

use fm_types::{InputId, SceneId};

use crate::{CropRect, InputKind, MixFeed, Project, SourceRef};

/// The inputs the switcher currently has on Program and Preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixSelection {
    pub program: InputId,
    pub preview: InputId,
}

impl MixSelection {
    #[must_use]
    pub const fn input(self, feed: MixFeed) -> InputId {
        match feed {
            MixFeed::Program => self.program,
            MixFeed::Preview => self.preview,
        }
    }
}

/// The Program and Preview frames committed by the last completed tick.
///
/// `F` is whatever frame handle the renderer uses; the model never inspects
/// it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MixFeedHistory<F> {
    program: Option<F>,
    preview: Option<F>,
}

impl<F> Default for MixFeedHistory<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F> MixFeedHistory<F> {
    /// A history with no committed frame: every mix feed renders black.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            program: None,
            preview: None,
        }
    }

    /// The frame a mix feed of `feed` shows during the current tick.
    #[must_use]
    pub fn previous(&self, feed: MixFeed) -> Option<&F> {
        match feed {
            MixFeed::Program => self.program.as_ref(),
            MixFeed::Preview => self.preview.as_ref(),
        }
    }

    /// Records the frames the tick that just finished composed for Program
    /// and Preview. They become visible to mix feeds on the next tick.
    pub fn commit(&mut self, program: F, preview: F) {
        self.program = Some(program);
        self.preview = Some(preview);
    }

    /// Forgets both frames, for example across a discontinuity or project
    /// reload, so mix feeds render black until the next commit.
    pub fn reset(&mut self) {
        self.program = None;
        self.preview = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveError {
    UnknownInput(InputId),
    UnknownScene(SceneId),
    /// The alias chain starting at this input never reaches a non-alias.
    AliasCycle(InputId),
    /// This scene reaches itself through layers, scene inputs, or aliases.
    SceneCycle(SceneId),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::AliasCycle(input) => write!(formatter, "alias chain of input {input} is a cycle"),
            Self::SceneCycle(scene) => write!(formatter, "scene {scene} reaches itself"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// What a renderer draws for one input on the current tick.
#[derive(Debug, Eq, PartialEq)]
pub enum RenderSource<'f, F> {
    /// A leaf the input's adapter produces: color, media, device, network,
    /// or simulated.
    Input(InputId),
    /// A composition of this scene's layers; see [`FrameContext::resolve_scene`].
    Scene(SceneId),
    /// The bus frame committed by the previous tick.
    PreviousFrame { feed: MixFeed, frame: &'f F },
    /// A mix feed before any frame has been committed.
    Black { feed: MixFeed },
}

impl<F> Clone for RenderSource<'_, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<F> Copy for RenderSource<'_, F> {}

/// One input resolved to its concrete render source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInput<'f, F> {
    pub requested: InputId,
    /// Aliases followed in order, starting with `requested` when it is an
    /// alias; empty for any other input.
    pub alias_chain: Vec<InputId>,
    /// The non-alias input the chain ends at (`requested` when not an alias).
    pub concrete: InputId,
    /// The requested alias's own crop. Crops on intermediate aliases are not
    /// inherited.
    pub crop: Option<CropRect>,
    pub source: RenderSource<'f, F>,
    /// The source is a mix feed whose bus currently contains a mix feed of
    /// that same bus, so the bus shows its own delayed picture (a recursive
    /// "hall of mirrors" that advances one level per tick).
    pub feedback: bool,
}

/// What a layer of a resolved scene draws.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayerSource<'f, F> {
    /// A scene referenced directly by the layer.
    Scene(SceneId),
    Input(ResolvedInput<'f, F>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLayer<'f, F> {
    /// Position of the layer in the scene's persisted layer list.
    pub index: usize,
    pub source: LayerSource<'f, F>,
}

/// One scene level resolved; nested scenes stay as [`SceneId`]s so a planner
/// renders them first, in [`FrameContext::scenes_in_render_order`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedScene<'f, F> {
    pub scene: SceneId,
    /// Enabled layers only, in persisted order.
    pub layers: Vec<ResolvedLayer<'f, F>>,
}

/// Where an input's audio comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioRoute {
    /// The audio produced by this leaf input's adapter.
    Input(InputId),
    /// No audio: a scene without an audio source, or a mix feed, whose
    /// delayed picture has no matching program audio to re-enter.
    Silence,
}

/// Project lookup tables built once and shared across ticks.
#[derive(Debug)]
pub struct SourceResolver<'p> {
    project: &'p Project,
    inputs: HashMap<InputId, usize>,
    scenes: HashMap<SceneId, usize>,
}

impl<'p> SourceResolver<'p> {
    #[must_use]
    pub fn new(project: &'p Project) -> Self {
        let mut inputs = HashMap::new();
        for (position, input) in project.inputs().iter().enumerate() {
            inputs.entry(input.id).or_insert(position);
        }
        let mut scenes = HashMap::new();
        for (position, scene) in project.scenes().iter().enumerate() {
            scenes.entry(scene.id).or_insert(position);
        }
        Self {
            project,
            inputs,
            scenes,
        }
    }

    /// Fixes the switcher selection and previous frames for one tick.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] when a selected input, or anything its
    /// picture is built from, is unknown or cyclic.
    pub fn context<'r, 'f, F>(
        &'r self,
        selection: MixSelection,
        history: &'f MixFeedHistory<F>,
    ) -> Result<FrameContext<'r, 'p, 'f, F>, ResolveError> {
        let program_feedback = self.contains_feed(selection.program, MixFeed::Program)?;
        let preview_feedback = self.contains_feed(selection.preview, MixFeed::Preview)?;
        Ok(FrameContext {
            resolver: self,
            history,
            program_feedback,
            preview_feedback,
        })
    }

    /// Follows `input`'s audio through aliases and scene audio sources.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::UnknownInput`] for a missing input and
    /// [`ResolveError::AliasCycle`] when the audio chain reaches itself.
    pub fn resolve_audio(&self, input: InputId) -> Result<AudioRoute, ResolveError> {
        let mut seen = HashSet::new();
        let mut current = input;
        loop {
            if !seen.insert(current) {
                return Err(ResolveError::AliasCycle(input));
            }
            current = match self.input_kind(current)? {
                InputKind::Alias { target, .. } => *target,
                InputKind::Scene {
                    audio_source: Some(source),
                    ..
                } => *source,
                InputKind::Scene {
                    audio_source: None, ..
                }
                | InputKind::MixFeed(_)
                | InputKind::Document(_)
                | InputKind::Title(_) => return Ok(AudioRoute::Silence),
                InputKind::Color
                | InputKind::Media { .. }
                | InputKind::Device { .. }
                | InputKind::NetworkStream(_)
                | InputKind::Replay
                | InputKind::SystemAudio(_)
                | InputKind::Image(_)
                | InputKind::Slideshow(_)
                | InputKind::ScreenCapture(_)
                | InputKind::Simulated(_) => return Ok(AudioRoute::Input(current)),
            };
        }
    }

    fn input_kind(&self, input: InputId) -> Result<&'p InputKind, ResolveError> {
        self.inputs
            .get(&input)
            .map(|&position| &self.project.inputs()[position].kind)
            .ok_or(ResolveError::UnknownInput(input))
    }

    fn scene(&self, scene: SceneId) -> Result<&'p crate::Scene, ResolveError> {
        self.scenes
            .get(&scene)
            .map(|&position| &self.project.scenes()[position])
            .ok_or(ResolveError::UnknownScene(scene))
    }

    /// Follows aliases from `input`, returning the chain and the end.
    fn follow_aliases(&self, input: InputId) -> Result<(Vec<InputId>, InputId), ResolveError> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = input;
        while let InputKind::Alias { target, .. } = self.input_kind(current)? {
            if !seen.insert(current) {
                return Err(ResolveError::AliasCycle(input));
            }
            chain.push(current);
            current = *target;
        }
        Ok((chain, current))
    }

    /// The scene a non-alias input composes, if it is a scene input.
    fn scene_of(&self, concrete: InputId) -> Result<Option<SceneId>, ResolveError> {
        Ok(match self.input_kind(concrete)? {
            InputKind::Scene { scene_id, .. } => Some(*scene_id),
            InputKind::Color
            | InputKind::Media { .. }
            | InputKind::Device { .. }
            | InputKind::NetworkStream(_)
            | InputKind::Replay
            | InputKind::SystemAudio(_)
            | InputKind::Image(_)
            | InputKind::Slideshow(_)
            | InputKind::Document(_)
            | InputKind::ScreenCapture(_)
            | InputKind::Simulated(_)
            | InputKind::MixFeed(_)
            | InputKind::Title(_)
            | InputKind::Alias { .. } => None,
        })
    }

    /// Whether the picture of `root` uses a mix feed of `feed`, looking
    /// through aliases, scene inputs, and enabled scene layers, but never
    /// through a mix feed itself.
    fn contains_feed(&self, root: InputId, feed: MixFeed) -> Result<bool, ResolveError> {
        let mut pending_inputs = vec![root];
        let mut pending_scenes = Vec::new();
        let mut seen_inputs = HashSet::new();
        let mut seen_scenes = HashSet::new();
        loop {
            if let Some(input) = pending_inputs.pop() {
                if !seen_inputs.insert(input) {
                    continue;
                }
                let (_, concrete) = self.follow_aliases(input)?;
                if let InputKind::MixFeed(found) = self.input_kind(concrete)?
                    && *found == feed
                {
                    return Ok(true);
                }
                if let Some(scene) = self.scene_of(concrete)? {
                    pending_scenes.push(scene);
                }
            } else if let Some(scene) = pending_scenes.pop() {
                if !seen_scenes.insert(scene) {
                    continue;
                }
                for layer in self
                    .scene(scene)?
                    .layers
                    .iter()
                    .filter(|layer| layer.enabled)
                {
                    match layer.source {
                        SourceRef::Input(input) => pending_inputs.push(input),
                        SourceRef::Scene(nested) => pending_scenes.push(nested),
                    }
                }
            } else {
                return Ok(false);
            }
        }
    }
}

/// Source resolution for one tick: a fixed selection and previous frames.
#[derive(Debug)]
pub struct FrameContext<'r, 'p, 'f, F> {
    resolver: &'r SourceResolver<'p>,
    history: &'f MixFeedHistory<F>,
    program_feedback: bool,
    preview_feedback: bool,
}

impl<'f, F> FrameContext<'_, '_, 'f, F> {
    /// Whether `feed`'s bus currently shows a mix feed of itself.
    #[must_use]
    pub const fn feedback(&self, feed: MixFeed) -> bool {
        match feed {
            MixFeed::Program => self.program_feedback,
            MixFeed::Preview => self.preview_feedback,
        }
    }

    /// Resolves one input to its concrete render source.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::UnknownInput`] for a missing input or alias
    /// target and [`ResolveError::AliasCycle`] for a cyclic alias chain.
    pub fn resolve_input(&self, input: InputId) -> Result<ResolvedInput<'f, F>, ResolveError> {
        let resolver = self.resolver;
        let crop = match resolver.input_kind(input)? {
            InputKind::Alias { crop, .. } => *crop,
            InputKind::Color
            | InputKind::Media { .. }
            | InputKind::Device { .. }
            | InputKind::NetworkStream(_)
            | InputKind::Replay
            | InputKind::SystemAudio(_)
            | InputKind::Image(_)
            | InputKind::Slideshow(_)
            | InputKind::Document(_)
            | InputKind::ScreenCapture(_)
            | InputKind::Scene { .. }
            | InputKind::Simulated(_)
            | InputKind::MixFeed(_)
            | InputKind::Title(_) => None,
        };
        let (alias_chain, concrete) = resolver.follow_aliases(input)?;
        let (source, feedback) = match resolver.input_kind(concrete)? {
            InputKind::MixFeed(feed) => {
                let source = match self.history.previous(*feed) {
                    Some(frame) => RenderSource::PreviousFrame { feed: *feed, frame },
                    None => RenderSource::Black { feed: *feed },
                };
                (source, self.feedback(*feed))
            }
            InputKind::Scene { scene_id, .. } => (RenderSource::Scene(*scene_id), false),
            InputKind::Color
            | InputKind::Media { .. }
            | InputKind::Device { .. }
            | InputKind::NetworkStream(_)
            | InputKind::Replay
            | InputKind::SystemAudio(_)
            | InputKind::Image(_)
            | InputKind::Slideshow(_)
            | InputKind::Document(_)
            | InputKind::ScreenCapture(_)
            | InputKind::Simulated(_)
            | InputKind::Title(_) => (RenderSource::Input(concrete), false),
            InputKind::Alias { .. } => unreachable!("alias chains end at a non-alias input"),
        };
        Ok(ResolvedInput {
            requested: input,
            alias_chain,
            concrete,
            crop,
            source,
            feedback,
        })
    }

    /// Resolves every enabled layer of one scene, one level deep.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] for an unknown scene or a layer input that
    /// fails [`Self::resolve_input`].
    pub fn resolve_scene(&self, scene: SceneId) -> Result<ResolvedScene<'f, F>, ResolveError> {
        let model = self.resolver.scene(scene)?;
        let layers = model
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.enabled)
            .map(|(index, layer)| {
                let source = match layer.source {
                    SourceRef::Scene(nested) => {
                        self.resolver.scene(nested)?;
                        LayerSource::Scene(nested)
                    }
                    SourceRef::Input(input) => LayerSource::Input(self.resolve_input(input)?),
                };
                Ok(ResolvedLayer { index, source })
            })
            .collect::<Result<_, ResolveError>>()?;
        Ok(ResolvedScene { scene, layers })
    }

    /// Every scene `input`'s picture composes, dependencies first, each once.
    ///
    /// The walk is iterative and never enters a mix feed, so its length is
    /// bounded by the scene count even when the input feeds its own bus.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::SceneCycle`] when a scene reaches itself and
    /// the [`Self::resolve_input`] errors for broken references.
    pub fn scenes_in_render_order(&self, input: InputId) -> Result<Vec<SceneId>, ResolveError> {
        let resolver = self.resolver;
        let (_, concrete) = resolver.follow_aliases(input)?;
        let Some(root) = resolver.scene_of(concrete)? else {
            return Ok(Vec::new());
        };
        let mut order = Vec::new();
        let mut done = HashSet::new();
        let mut on_path = HashSet::new();
        // Each frame is a scene and its enabled dependency scenes.
        let mut work: Vec<(SceneId, Vec<SceneId>)> = Vec::new();
        on_path.insert(root);
        work.push((root, self.scene_dependencies(root)?));
        while let Some((scene, dependencies)) = work.last_mut() {
            let scene = *scene;
            if let Some(next) = dependencies.pop() {
                if done.contains(&next) {
                    continue;
                }
                if !on_path.insert(next) {
                    return Err(ResolveError::SceneCycle(next));
                }
                let next_dependencies = self.scene_dependencies(next)?;
                work.push((next, next_dependencies));
                continue;
            }
            work.pop();
            on_path.remove(&scene);
            done.insert(scene);
            order.push(scene);
        }
        Ok(order)
    }

    /// Scenes one level below `scene`, in reverse layer order so popping
    /// visits them in layer order.
    fn scene_dependencies(&self, scene: SceneId) -> Result<Vec<SceneId>, ResolveError> {
        let resolver = self.resolver;
        let mut dependencies = Vec::new();
        for layer in resolver
            .scene(scene)?
            .layers
            .iter()
            .filter(|layer| layer.enabled)
        {
            let dependency = match layer.source {
                SourceRef::Scene(nested) => {
                    resolver.scene(nested)?;
                    Some(nested)
                }
                SourceRef::Input(input) => {
                    let (_, concrete) = resolver.follow_aliases(input)?;
                    resolver.scene_of(concrete)?
                }
            };
            dependencies.extend(dependency);
        }
        dependencies.reverse();
        Ok(dependencies)
    }
}
