//! Live scene layer edits (SW-006): one layer of one engine scene changed
//! through the engine's command path.
//!
//! An [`EngineSceneLayerEdit`] names its scene directly, so a scene no Scene
//! input shows is edited like any other; every Scene input showing the scene
//! shows the change. Each edit is a project edit:
//!
//! * **Padlocked** by the project padlock and by the padlock of every Scene
//!   input showing the scene.
//! * **Undoable** with an exact inverse computed from the scene before it
//!   ([`fm_model::Scene::layer_change_inverse`]): the previous geometry,
//!   appearance, crop, mask, z-order, or position; removing an added layer;
//!   adding a removed layer back at its position with its whole design.
//!   An edit that continues a gesture ([`EngineSceneLayerEdit::continues`])
//!   folds into the latest undo entry when that entry edits the same
//!   property of the same layer and was opened by the same gesture
//!   ([`EngineSceneLayerEdit::gesture`]), so one drag or scrub undoes at
//!   once; a continuation whose gesture opened no step (its first send was
//!   refused or never arrived) opens one. A gesture that ends where it
//!   started (a designer closed mid-drag sends the layer back) leaves no
//!   step.
//! * **Checked** as a whole before the scene changes: the layer rules of
//!   [`fm_model::Scene::apply_layer_change`] on the project canvas, and for an
//!   added layer its source (an input of the show or a registered scene) and
//!   cycles (a scene may not draw itself, directly, through a nested scene,
//!   through a Scene input, or through an alias input of any of those). A
//!   scene holds at most [`MAX_SCENE_LAYERS`] layers after an add, and at
//!   most [`fm_model::Scene::MAX_RENDERED_LAYERS`] enabled layers (the
//!   compositor's limit) after an add or enable. A removal is refused
//!   while the layer is animated (the scene's clip has a track for it) or
//!   graded, so its undo restores the scene exactly; a layer whose name or
//!   effects could not be carried back by its undo (more than
//!   [`MAX_SCENE_LAYER_NAME_BYTES`] bytes) is not removed live either.

use std::collections::BTreeSet;
use std::sync::Arc;

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{LayerId, SceneLayerChange, SceneLayerEditError, SceneLayerRef, SourceRef};
use fm_types::{InputId, SceneId};

use crate::ShowState;

/// Most layers a scene holds after a live add (the wire layout bound).
pub const MAX_SCENE_LAYERS: usize = 192;
/// Longest layer name a live add carries, in UTF-8 bytes (the wire layout
/// bound), so the undo of a removal can carry the removed layer back.
pub const MAX_SCENE_LAYER_NAME_BYTES: usize = 80;

/// One live edit of layer `layer` of scene `scene`. A project edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSceneLayerEdit {
    pub scene: SceneId,
    pub layer: LayerId,
    pub change: SceneLayerChange,
    /// Whether the edit continues one gesture: it folds into the latest undo
    /// entry when that entry edits the same property ([`Self::folds_into`])
    /// of the same layer and was opened by the same [`Self::gesture`],
    /// keeping the entry's inverse, so the gesture undoes at once. Recorded
    /// entries never continue.
    pub continues: bool,
    /// The gesture the edit belongs to: a token its sender picks once per
    /// gesture (unique across sessions), or `0` outside any gesture. A
    /// continuing edit folds only into a step its own gesture opened, so a
    /// gesture whose first send was refused or lost never folds into an
    /// earlier gesture's step.
    pub gesture: u64,
}

impl EngineSceneLayerEdit {
    /// A standalone edit (not continuing a gesture).
    #[must_use]
    pub const fn new(scene: SceneId, layer: LayerId, change: SceneLayerChange) -> Self {
        Self {
            scene,
            layer,
            change,
            continues: false,
            gesture: 0,
        }
    }

    /// The edit as recorded in the history: never continuing, in no
    /// gesture.
    #[must_use]
    pub fn recorded(self) -> Self {
        Self {
            continues: false,
            gesture: 0,
            ..self
        }
    }

    /// Whether this continuing edit folds into the recorded `previous`
    /// edit, whose step gesture `previous_gesture` opened: the same nonzero
    /// gesture, same scene, same layer, and the same property (geometry,
    /// appearance, crop, mask, or z-order; positions, adds, and removals
    /// never fold).
    #[must_use]
    pub fn folds_into(&self, previous: &Self, previous_gesture: u64) -> bool {
        use core::mem::discriminant;
        self.continues
            && self.gesture != 0
            && self.gesture == previous_gesture
            && self.scene == previous.scene
            && self.layer == previous.layer
            && matches!(
                self.change,
                SceneLayerChange::Geometry(_)
                    | SceneLayerChange::Appearance { .. }
                    | SceneLayerChange::Crop(_)
                    | SceneLayerChange::Mask(_)
                    | SceneLayerChange::ZOrder(_)
            )
            && discriminant(&self.change) == discriminant(&previous.change)
    }

    /// Whether the edit refers to `input` (an added layer drawing it).
    #[must_use]
    pub fn names_input(&self, input: InputId) -> bool {
        matches!(&self.change, SceneLayerChange::Add { layer, .. }
            if layer.source == SourceRef::Input(input))
    }
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

fn edit_rejection(error: SceneLayerEditError) -> Rejection {
    let message = error.to_string();
    let (code, field) = match error {
        SceneLayerEditError::UnknownLayer(_) => (RejectionCode::NotFound, "layer"),
        SceneLayerEditError::DuplicateLayer(_)
        | SceneLayerEditError::LayerAnimated(_)
        | SceneLayerEditError::TooManyEnabledLayers => (RejectionCode::Conflict, "layer"),
        SceneLayerEditError::IndexOutOfRange { .. } => (RejectionCode::InvalidCommand, "index"),
        SceneLayerEditError::EmptyName => (RejectionCode::InvalidCommand, "spec"),
        SceneLayerEditError::InvalidGeometry => (RejectionCode::InvalidCommand, "geometry"),
        SceneLayerEditError::InvalidCrop | SceneLayerEditError::CropWouldInvalidateMask => {
            (RejectionCode::InvalidCommand, "crop")
        }
        SceneLayerEditError::InvalidMask => (RejectionCode::InvalidCommand, "mask"),
        SceneLayerEditError::TooManyEffects | SceneLayerEditError::InvalidEffect { .. } => {
            (RejectionCode::InvalidCommand, "spec")
        }
    };
    rejection(code, field, error.code(), message)
}

/// The scenes `source` draws, directly, nested, or through alias inputs,
/// in `state`.
fn reachable_scenes(state: &ShowState, source: SourceRef) -> BTreeSet<SceneId> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![source];
    while let Some(source) = pending.pop() {
        let scene = match source {
            SourceRef::Scene(scene) => scene,
            SourceRef::Input(input) => match state.scene_inputs().get(&state.input_rendered(input))
            {
                Some(scene) => *scene,
                None => continue,
            },
        };
        if !seen.insert(scene) {
            continue;
        }
        if let Some(shown) = state.scene(scene) {
            pending.extend(shown.scene.layers.iter().map(|layer| layer.source));
        }
    }
    seen
}

/// Checks an added layer's source against the show.
fn check_source(state: &ShowState, scene: SceneId, source: SourceRef) -> Result<(), Rejection> {
    match source {
        SourceRef::Input(input) if !state.inputs().contains(&input) => {
            return Err(rejection(
                RejectionCode::NotFound,
                "spec",
                "unknown_input",
                format!("layer source input {input} does not exist"),
            ));
        }
        SourceRef::Scene(source) if state.scene(source).is_none() => {
            return Err(rejection(
                RejectionCode::NotFound,
                "spec",
                "unknown_scene",
                format!("layer source scene {source} does not exist"),
            ));
        }
        SourceRef::Input(_) | SourceRef::Scene(_) => {}
    }
    if reachable_scenes(state, source).contains(&scene) {
        return Err(rejection(
            RejectionCode::InvalidCommand,
            "spec",
            "source_cycle",
            format!("the layer source would make scene {scene} draw itself"),
        ));
    }
    Ok(())
}

/// Applies one live scene layer edit to the show.
pub(crate) fn apply(state: &mut ShowState, edit: &EngineSceneLayerEdit) -> Result<(), Rejection> {
    let shown = state.scene(edit.scene).ok_or_else(|| {
        rejection(
            RejectionCode::NotFound,
            "scene",
            "unknown_scene",
            format!("scene {} does not exist", edit.scene),
        )
    })?;
    match &edit.change {
        SceneLayerChange::Add { layer, .. } => {
            if shown.scene.layers.len() >= MAX_SCENE_LAYERS {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "layer",
                    "too_many_layers",
                    format!("a scene holds at most {MAX_SCENE_LAYERS} layers"),
                ));
            }
            if layer.name.len() > MAX_SCENE_LAYER_NAME_BYTES
                || layer.name.chars().any(char::is_control)
            {
                return Err(rejection(
                    RejectionCode::InvalidCommand,
                    "spec",
                    "invalid_name",
                    format!(
                        "a layer name is at most {MAX_SCENE_LAYER_NAME_BYTES} bytes without \
                         control characters"
                    ),
                ));
            }
            check_source(state, edit.scene, layer.source)?;
        }
        SceneLayerChange::Remove => {
            if state
                .grading()
                .layers
                .contains_key(&SceneLayerRef::new(edit.scene, edit.layer))
            {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "layer",
                    "layer_graded",
                    format!(
                        "scene layer {} is graded; clear its grade first",
                        edit.layer
                    ),
                ));
            }
            if let Some(layer) = shown.scene.layer_by_id(edit.layer)
                && (layer.name.len() > MAX_SCENE_LAYER_NAME_BYTES
                    || layer.name.chars().any(char::is_control))
            {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "layer",
                    "not_restorable",
                    format!(
                        "scene layer {} has a name its undo could not carry; remove it offline",
                        edit.layer
                    ),
                ));
            }
        }
        _ => {}
    }
    let mut scene = (*shown.scene).clone();
    let (width, height) = (shown.canvas_width, shown.canvas_height);
    scene
        .apply_layer_change(edit.layer, &edit.change, width, height)
        .map_err(edit_rejection)?;
    if let Some(shared) = state.scene_mut(edit.scene) {
        shared.scene = Arc::new(scene);
    }
    Ok(())
}

/// The edit that restores what `edit` changes, or `None` when it targets
/// nothing in `state`.
pub(crate) fn inverse(
    state: &ShowState,
    edit: &EngineSceneLayerEdit,
) -> Option<EngineSceneLayerEdit> {
    let shown = state.scene(edit.scene)?;
    Some(EngineSceneLayerEdit::new(
        edit.scene,
        edit.layer,
        shown.scene.layer_change_inverse(edit.layer, &edit.change)?,
    ))
}

/// Every Scene input that shows `scene`, ascending.
#[must_use]
pub fn scene_inputs_showing(state: &ShowState, scene: SceneId) -> Vec<InputId> {
    state
        .scene_inputs()
        .iter()
        .filter(|(_, shown)| **shown == scene)
        .map(|(input, _)| *input)
        .collect()
}
