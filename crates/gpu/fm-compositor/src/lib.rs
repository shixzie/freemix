//! Backend-neutral scene planning and deterministic CPU reference composition.

mod cpu;
mod effects;
mod fade_to_black;
mod fit;
mod flatten;
mod keying;
mod multiview;
#[cfg(feature = "native-wgpu")]
mod native;
#[cfg(feature = "native-wgpu")]
mod native_grade;
#[cfg(feature = "native-wgpu")]
mod native_layer;
mod plan;
mod rotate;
mod safe_area;
mod scene;
mod stinger;
mod transition;

pub use cpu::{
    CpuColorSourceFrame, CpuExecutionError, CpuSourceFrame, apply_grade, execute_cpu,
    execute_cpu_frame, execute_cpu_with_color, image_from_cpu_frame,
};
pub use effects::{ReferenceEffect, ReferenceEffectError, apply_reference_effect};
pub use fade_to_black::{
    FadeToBlackPlan, FadeToBlackPlanError, FadeToBlackPosition, MAX_FADE_TO_BLACK_DENOMINATOR,
    execute_fade_to_black_cpu,
};
pub use fit::{CanvasRect, FitError, FitPolicy, SourcePlacement, center_cut, place_source};
pub use flatten::{
    FlattenError, FlattenExecutionError, FlattenLimits, FlattenMode, FlattenOutput, FlattenPlan,
    FlattenStep, FlattenedScene, HoistedLayer, SceneGraph, execute_flatten_cpu,
    execute_flatten_cpu_frame, plan_flatten,
};
pub use fm_types::{GARBAGE_MATTE_UNIT, GarbageMatte, MattePoint};
pub use keying::{
    AutoKey, AutoKeyError, AutoKeyOptions, AutoKeyRegion, Backing, Despill, KeyChannel, KeyFill,
    KeyRefinement, KeyRefinementError, LightWrap, MAX_AUTO_KEY_SAMPLES, MAX_CHOKE_RADIUS,
    MAX_EDGE_SOFTNESS, MAX_LIGHT_WRAP_WIDTH, chroma_cb_cr, chroma_distance, sample_auto_key,
};
pub use multiview::{
    ConfidenceMode, GridLayout, MAX_MULTIVIEW_CELLS, MAX_MULTIVIEW_COLUMNS,
    MAX_MULTIVIEW_NAME_BYTES, MAX_MULTIVIEW_ROWS, MultiviewCell, MultiviewDefinition,
    MultiviewError, MultiviewOverlays, MultiviewPlan, MultiviewSource, MultiviewSourceFrame,
    compile_multiview_scene, compile_multiview_scene_fitted, plan_multiview,
};
#[cfg(feature = "native-wgpu")]
pub use native::{
    MAX_NATIVE_TRANSFORM_DIMENSION, NativeCompositionError, NativeCompositionRenderer,
    NativeFadeToBlackError, NativeFadeToBlackRenderer, NativeSourceFrame, NativeStingerError,
    NativeStingerRenderer, NativeTransitionError, NativeTransitionRenderer,
};
#[cfg(feature = "native-wgpu")]
pub use native_grade::{
    DEFAULT_NATIVE_GRADE_LUT_EDGE, NativeGradeError, NativeGradeLut, NativeGradeRenderer,
};
pub use plan::{
    CompilationReport, CompositionPlan, PlanError, PlanLayer, ReportEntry, compile_scene,
};
pub use safe_area::{
    MAX_SAFE_AREA_GUIDES, SafeAreaError, SafeAreaPreset, preset_guides, push_preset_guides,
};
pub use scene::{
    ChromaKey, Effect, InclusionError, Key, LumaKey, OutputInclusion, OutputTarget, RectMask,
    SafeAreaGuide, Scene, SceneError, SourceId, SourceLayer,
};
pub use stinger::{
    StingerBase, StingerFramePlan, StingerFrameRole, StingerPlanError, StingerRenderError,
    execute_stinger_frame,
};
pub use transition::{TransitionError, TransitionKind, TransitionPlan, execute_transition};

pub use fm_frame::AlphaMode;
pub use fm_grade::CompiledGrade;
pub use fm_video::{CropRect, ImageFrame, Rgba8, Rotation, Transform};
