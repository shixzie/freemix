//! Scene layer animation (GX-011 stage 5): a scene's keyframe animation
//! edited through the engine's command path and played on air when its
//! Scene input reaches Program or Preview.
//!
//! A Scene input restored with its scene ([`ShowState::set_scene_input`])
//! names the scene by id; every Scene input naming the same scene shows one
//! animation, so an edit through any of them changes all of them. Each
//! [`EngineSceneAnimationEdit`] is a project edit with exactly the title
//! animation rules: revision-checked (`base_revision`, conflict otherwise),
//! padlocked (the project padlock and the padlock of every Scene input
//! sharing the scene), undoable (recorded as the exact clip produced, the
//! inverse as the exact clip replaced; every applied edit, undo and redo
//! included, advances the revision by one), and bounded by an optional
//! [`SceneClipBound`].
//!
//! # On air
//!
//! [`SceneAir`] plays the phases against engine frames exactly like a title
//! take ([`fm_titles::TitleAir`]: the same exact rational timing, so a 25p
//! clip on a 29.97 project keeps its keyframe timing):
//!
//! * A Scene input that reaches Program (a cut, or the first frame of a
//!   timed or manual transition that brings it in) plays its In phase from
//!   that frame, then holds the In phase's last frame.
//! * A Scene input that reaches Preview while not on Program plays its In
//!   phase there too, so the operator sees the entrance before taking it.
//! * A Scene input that a timed or manual transition takes off Program plays
//!   its Out phase from the transition's first frame for as long as the
//!   transition shows it; an Out phase shorter than the transition holds its
//!   last frame. A cut leaves no frame to play Out on: the outgoing scene is
//!   simply gone. A transition that is cancelled (a manual transition pulled
//!   back) brings the scene back to Program and restarts its In phase.
//! * After a take the old Program usually lands on Preview, where it plays
//!   its In phase again.
//! * A scene shown any other way (as a layer of another scene, on a
//!   multiview, or on an overlay channel) holds; a restored engine starts
//!   with every scene holding.
//!
//! Scene phases are runtime state, never snapshotted. Like a title's, a
//! scene mid-phase makes the engine time-dependent: snapshots wait for every
//! In and Out phase to settle.
//!
//! [`ShowState::set_scene_input`]: crate::ShowState::set_scene_input

use std::sync::Arc;

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{
    LayerId, Scene, SceneAnimation, SceneAnimationError, SceneClip,
    fm_animation::{AnimationPreset, ClipError, ClipPosition, ClipState, Phase},
};
use fm_titles::{TitleAir, TitleAirFrame, TitleAirPhase, TitleAirTiming};
use fm_types::{FrameRate, InputId, SceneId};

use crate::ShowState;

/// One scene the engine animates, shared by every Scene input naming it:
/// the scene itself (layers and animation) and the project canvas its
/// presets measure against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineScene {
    pub scene: Arc<Scene>,
    pub canvas_width: u32,
    pub canvas_height: u32,
}

impl EngineScene {
    #[must_use]
    pub fn new(scene: Scene, canvas_width: u32, canvas_height: u32) -> Self {
        Self {
            scene: Arc::new(scene),
            canvas_width,
            canvas_height,
        }
    }

    #[must_use]
    pub fn id(&self) -> SceneId {
        self.scene.id
    }

    /// The scene's animation and its revision.
    #[must_use]
    pub fn animation(&self) -> &SceneAnimation {
        &self.scene.animation
    }

    /// The In and Out phase lengths a scene plays on air.
    #[must_use]
    pub fn timing(&self) -> TitleAirTiming {
        TitleAirTiming {
            timebase: self.scene.animation.clip.timebase(),
            in_frames: self.scene.animation.clip.in_frames(),
            out_frames: self.scene.animation.clip.out_frames(),
        }
    }
}

/// Whether a scene clip may be the result of an animation edit (see
/// [`crate::Engine::set_scene_clip_bound`]).
pub type SceneClipBound = fn(&SceneClip) -> bool;

/// What one scene animation edit changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneAnimationChange {
    /// Replaces the whole clip.
    Set(Box<SceneClip>),
    /// Applies a built-in preset to one layer over `frames` frames of
    /// `phase` (see [`Scene::apply_animation_preset`]).
    Preset {
        layer: LayerId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
    },
    /// Scales every key of `phase` to `frames` frames (see
    /// [`Scene::stretch_animation`]).
    Stretch { phase: Phase, frames: u32 },
}

/// One edit of the animation of Scene input `input`'s scene. A project
/// edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSceneAnimationEdit {
    pub input: InputId,
    /// The animation revision the edit was based on. `None` only for the
    /// clips undo and redo apply.
    pub base_revision: Option<u64>,
    pub change: SceneAnimationChange,
}

impl EngineSceneAnimationEdit {
    /// The same edit with no base revision, as the history stores it.
    #[must_use]
    pub(crate) fn unchecked(&self) -> Self {
        Self {
            base_revision: None,
            ..self.clone()
        }
    }
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

/// The scene of Scene input `input`.
fn scene_of(state: &ShowState, input: InputId) -> Result<&EngineScene, Rejection> {
    state.scene_input(input).ok_or_else(|| {
        rejection(
            RejectionCode::NotFound,
            "input",
            "unknown_scene",
            format!("input {input} is not a scene"),
        )
    })
}

fn edit_rejection(error: &SceneAnimationError) -> Rejection {
    let message = error.to_string();
    match error {
        SceneAnimationError::UnknownLayer(_) => {
            rejection(RejectionCode::NotFound, "layer", "unknown_layer", message)
        }
        SceneAnimationError::UnsupportedProperty { .. } => rejection(
            RejectionCode::InvalidCommand,
            "clip",
            "unsupported_property",
            message,
        ),
        SceneAnimationError::RevisionExhausted => rejection(
            RejectionCode::ResourceExhausted,
            "base_revision",
            "revision_exhausted",
            message,
        ),
        SceneAnimationError::Clip(clip) => {
            let (field, issue) = match clip {
                ClipError::PhaseTooLong { .. } | ClipError::ZeroSpan => ("frames", "invalid"),
                ClipError::TooManyTracks { .. }
                | ClipError::TooManyTrackKeys { .. }
                | ClipError::TooManyKeys { .. } => ("clip", "too_large"),
                ClipError::RetimeCollision { .. } | ClipError::KeyCollision { .. } => {
                    ("frames", "key_collision")
                }
                _ => ("clip", "invalid"),
            };
            rejection(RejectionCode::InvalidCommand, field, issue, message)
        }
    }
}

/// The scene `edit` produces from the current state, checked against its
/// base revision. Nothing in `state` changes.
pub(crate) fn edited_scene(
    state: &ShowState,
    edit: &EngineSceneAnimationEdit,
) -> Result<Scene, Rejection> {
    let current = scene_of(state, edit.input)?;
    let actual = current.animation().revision;
    if let Some(expected) = edit.base_revision
        && expected != actual
    {
        return Err(rejection(
            RejectionCode::Conflict,
            "base_revision",
            "revision_conflict",
            format!(
                "animation edit is based on revision {expected}, but the animation is at \
                 revision {actual}"
            ),
        ));
    }
    let mut scene = (*current.scene).clone();
    let result = match &edit.change {
        SceneAnimationChange::Set(clip) => scene.edit_animation(|draft| {
            draft.clone_from(clip);
            Ok(())
        }),
        SceneAnimationChange::Preset {
            layer,
            phase,
            preset,
            frames,
        } => scene.apply_animation_preset(
            *layer,
            *phase,
            *preset,
            *frames,
            current.canvas_width,
            current.canvas_height,
        ),
        SceneAnimationChange::Stretch { phase, frames } => scene.stretch_animation(*phase, *frames),
    };
    result.map_err(|error| edit_rejection(&error))?;
    Ok(scene)
}

/// `edit` with its change made explicit as the exact clip it produces on
/// `state`. An edit that would be refused is returned unchanged.
pub(crate) fn normalized(
    state: &ShowState,
    edit: &EngineSceneAnimationEdit,
) -> EngineSceneAnimationEdit {
    match edited_scene(state, edit) {
        Ok(scene) => EngineSceneAnimationEdit {
            change: SceneAnimationChange::Set(Box::new(scene.animation.clip)),
            ..edit.clone()
        },
        Err(_) => edit.clone(),
    }
}

/// The edit that restores the clip `edit`'s scene holds now, or `None` when
/// `edit` targets no scene.
pub(crate) fn inverse(
    state: &ShowState,
    edit: &EngineSceneAnimationEdit,
) -> Option<EngineSceneAnimationEdit> {
    let current = scene_of(state, edit.input).ok()?;
    Some(EngineSceneAnimationEdit {
        input: edit.input,
        base_revision: None,
        change: SceneAnimationChange::Set(Box::new(current.animation().clip.clone())),
    })
}

/// Every Scene input that shows `input`'s scene, ascending; empty when
/// `input` is not a Scene input.
#[must_use]
pub fn scene_inputs_sharing(state: &ShowState, input: InputId) -> Vec<InputId> {
    let Some(scene) = state.scene_inputs().get(&input).copied() else {
        return Vec::new();
    };
    state
        .scene_inputs()
        .iter()
        .filter(|(_, candidate)| **candidate == scene)
        .map(|(input, _)| *input)
        .collect()
}

/// Applies one checked edit to the shared scene.
pub(crate) fn apply(
    state: &mut ShowState,
    edit: &EngineSceneAnimationEdit,
    bound: Option<SceneClipBound>,
) -> Result<(), Rejection> {
    let scene = edited_scene(state, edit)?;
    if let Some(fits) = bound
        && !fits(&scene.animation.clip)
    {
        return Err(rejection(
            RejectionCode::InvalidCommand,
            "clip",
            "too_large",
            "the edited animation is too large to carry".into(),
        ));
    }
    if let Some(shared) = state.scene_mut(scene.id) {
        shared.scene = Arc::new(scene);
    }
    Ok(())
}

/// Where a Scene input is shown, for its animation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SceneBus {
    Program,
    Preview,
}

/// A Scene input's on-air animation: which bus started it and the
/// In/Hold/Out life (the title take timing model).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneAir {
    pub(crate) bus: SceneBus,
    pub(crate) air: TitleAir,
}

/// One Scene input's animation at one realized frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineSceneAir {
    pub input: InputId,
    /// Where the scene is in its In/Hold/Out life and the pose to render.
    /// After an Out phase ends while a transition still shows the scene, it
    /// holds the Out phase's last frame (`phase_frames` zero).
    pub frame: TitleAirFrame,
}

/// What one frame of the realized switcher shows of a Scene input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SceneRole {
    /// On Program, or being brought in by a transition.
    Program,
    /// Being taken off Program by a transition.
    Leaving,
    /// On Preview and not on Program.
    Preview,
}

fn role(
    input: InputId,
    program: &fm_switcher::ProgramFrame,
    preview: InputId,
) -> Option<SceneRole> {
    if program.secondary.is_some() && program.primary == input && program.secondary != Some(input) {
        Some(SceneRole::Leaving)
    } else if program.primary == input || program.secondary == Some(input) {
        Some(SceneRole::Program)
    } else if preview == input {
        Some(SceneRole::Preview)
    } else {
        None
    }
}

/// Advances every Scene input's animation to `frame`, given what the
/// realized switcher shows on it. Called once per engine frame, before the
/// frame's pose is sampled.
pub(crate) fn step_scene_air(
    scenes: &mut std::collections::BTreeMap<InputId, SceneAir>,
    state: &ShowState,
    program: &fm_switcher::ProgramFrame,
    preview: InputId,
    frame: u64,
    rate: FrameRate,
) {
    let mut next = std::collections::BTreeMap::new();
    for (&input, &scene) in state.scene_inputs() {
        let Some(timing) = state.scene(scene).map(EngineScene::timing) else {
            continue;
        };
        let current = scenes.get(&input).copied();
        let updated = match role(input, program, preview) {
            None => None,
            Some(SceneRole::Program) => Some(match current {
                Some(air) if air.bus == SceneBus::Program => SceneAir {
                    air: air.air.retake(frame),
                    ..air
                },
                _ => SceneAir {
                    bus: SceneBus::Program,
                    air: TitleAir::take(timing, rate, frame),
                },
            }),
            Some(SceneRole::Leaving) => Some(match current {
                Some(air) if air.bus == SceneBus::Program => SceneAir {
                    air: air.air.off_air(frame),
                    ..air
                },
                // Not seen entering (a restored engine): leave from the hold.
                _ => SceneAir {
                    bus: SceneBus::Program,
                    air: TitleAir::take(timing, rate, frame).off_air(frame),
                },
            }),
            Some(SceneRole::Preview) => Some(match current {
                Some(air) if air.bus == SceneBus::Preview => air,
                _ => SceneAir {
                    bus: SceneBus::Preview,
                    air: TitleAir::take(timing, rate, frame),
                },
            }),
        };
        if let Some(air) = updated {
            next.insert(input, air);
        }
    }
    *scenes = next;
}

/// Every Scene input's animation at `frame`, ascending by input: only
/// scenes playing In or Out or holding after a phase; untracked scenes hold.
pub(crate) fn scene_air_frames(
    scenes: &std::collections::BTreeMap<InputId, SceneAir>,
    frame: u64,
) -> Vec<EngineSceneAir> {
    scenes
        .iter()
        .map(|(input, scene)| EngineSceneAir {
            input: *input,
            frame: scene.air.at(frame).unwrap_or(TitleAirFrame {
                // The Out phase ended while a transition still shows it.
                phase: TitleAirPhase::Out,
                phase_frame: 0,
                phase_frames: 0,
                sample: ClipState::Out(ClipPosition::frame(scene.air.timing().out_frames)),
            }),
        })
        .collect()
}

/// Whether any scene is mid In or Out phase at `frame`.
pub(crate) fn scenes_in_motion(
    scenes: &std::collections::BTreeMap<InputId, SceneAir>,
    frame: u64,
) -> bool {
    scene_air_frames(scenes, frame)
        .iter()
        .any(|scene| scene.frame.phase_frames != 0)
}
