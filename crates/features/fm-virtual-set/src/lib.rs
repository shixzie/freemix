//! Backend-neutral virtual-set scenes, shot bindings, and render descriptions.

mod camera_move;
mod geometry;
mod id;
mod model;
mod plan_error;
mod presets;
mod rational;
mod render;
mod stack;
mod validation;

pub use camera_move::{
    CAMERA_STATE_QUANTUM, CameraMove, CameraPose, CameraState, EasingCurve, EasingPoint,
    MAX_EASING_POINTS, MAX_MOVE_FRAMES, MAX_ZOOM_PERMILLE, MIN_ZOOM_PERMILLE,
};
pub use geometry::{NormalizedCorners, NormalizedPlane, NormalizedPoint};
pub use id::{CameraId, LayerId, PresetId, SetId, ShotId, TalentId};
pub use model::{
    BackgroundBinding, CameraPreset, ForegroundBinding, KeyBinding, KeyRequirement, Layer,
    LayerKind, Shot, ShotBindings, TalentBinding, TransitionIntent, TransitionKind,
    VirtualSetScene, WipeDirection,
};
pub use plan_error::PlanError;
pub use presets::{
    CameraDirector, MAX_PRESET_NAME_BYTES, MAX_PRESETS, PendingRecall, PresetStore,
    VirtualSetPreset,
};
pub use rational::Rational;
pub use render::{RenderBinding, RenderCamera, RenderDescription, RenderLayer, compile};
pub use stack::{
    FramePlan, KeyReference, LayerStack, MAX_OUTPUT_DIMENSION, MAX_SHADOW_BLUR_PX,
    MAX_SHADOW_OFFSET_PERMILLE, MAX_STACK_LAYERS, MAX_TALENT_SLOTS, OutputSize, PixelBounds,
    PixelRect, PlannedElement, PlannedLayer, ReflectionHook, SetRect, ShadowHook, StackLayer,
    StackRole, plan_frame,
};
pub use validation::{
    BindingKind, MAX_LAYERS, ValidationError, ValidationErrors, validate_scene, validate_shot,
};
