use core::fmt;
use std::sync::Arc;

use fm_gpu::{
    NativeContext, NativeFullscreenBlend, NativeFullscreenDraw, NativeFullscreenLoadOp,
    NativeFullscreenPipeline, NativeFullscreenPipelineOptions, NativeGpuError,
    NativeSourceExtentPolicy, NativeTexture, ShaderDescriptor, ShaderLanguage, ShaderSource,
    ShaderStage, TextureFormat,
};
use fm_grade::CompiledGrade;
use fm_video::{
    CropRect, Rotation, Transform, TransitionGeometry, TransitionShape, transition_extent,
};

use crate::native_grade::{NativeGradeCache, NativeGradeError, NativeGradeRenderer};
use crate::native_layer::{LayerPreparation, NativeLayerRenderer, native_effect};
use crate::rotate::Tilt;
use crate::{
    CompositionPlan, FadeToBlackPlan, RectMask, SourceId, StingerBase, StingerFramePlan,
    StingerFrameRole, TransitionKind, TransitionPlan,
};

/// Maximum width or height accepted for a native layer transform.
///
/// This keeps nearest-neighbor coordinate products within portable WGSL `u32`
/// arithmetic when combined with device-bounded source dimensions.
pub const MAX_NATIVE_TRANSFORM_DIMENSION: u32 = 16_384;

const COMPOSITION_UNIFORM_SIZE: usize = 96;
const TRANSITION_UNIFORM_SIZE: usize = 64;

const COMPOSITION_FRAGMENT_SHADER: &str = r"
struct LayerUniform {
    translation_x: u32,
    translation_y: u32,
    scale_width: u32,
    scale_height: u32,
    rotation: u32,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    opacity: u32,
    visible: u32,
    rotated_width: u32,
    rotated_height: u32,
    mask_left: u32,
    mask_top: u32,
    mask_right: u32,
    mask_bottom: u32,
    mask_enabled: u32,
    mask_invert: u32,
    border_width: u32,
    tilted: u32,
    tilt_sin: f32,
    tilt_cos: f32,
    padding_0: u32,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var unused_source_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> layer: LayerUniform;

// One premultiplied texel of the untilted footprint (the scaled,
// quarter-turned box with its border) at footprint pixel `rotated`, before
// opacity; transparent outside the footprint.
fn footprint_texel(rotated_signed: vec2<i32>) -> vec4<f32> {
    if rotated_signed.x < 0 || rotated_signed.y < 0 {
        return vec4<f32>(0.0);
    }
    let rotated = vec2<u32>(rotated_signed);
    if rotated.x >= layer.rotated_width || rotated.y >= layer.rotated_height {
        return vec4<f32>(0.0);
    }
    if layer.border_width != 0u && (
        rotated.x < layer.border_width ||
        rotated.y < layer.border_width ||
        layer.rotated_width - rotated.x <= layer.border_width ||
        layer.rotated_height - rotated.y <= layer.border_width
    ) {
        return vec4<f32>(1.0);
    }

    var scaled: vec2<u32>;
    switch layer.rotation {
        case 0u: {
            scaled = rotated;
        }
        case 1u: {
            scaled = vec2<u32>(rotated.y, layer.scale_height - 1u - rotated.x);
        }
        case 2u: {
            scaled = vec2<u32>(
                layer.scale_width - 1u - rotated.x,
                layer.scale_height - 1u - rotated.y,
            );
        }
        default: {
            scaled = vec2<u32>(layer.scale_width - 1u - rotated.y, rotated.x);
        }
    }

    let cropped_coordinates = vec2<u32>(
        scaled.x * layer.crop_width / layer.scale_width,
        scaled.y * layer.crop_height / layer.scale_height,
    );
    let inside_mask =
        cropped_coordinates.x >= layer.mask_left &&
        cropped_coordinates.y >= layer.mask_top &&
        cropped_coordinates.x < layer.mask_right &&
        cropped_coordinates.y < layer.mask_bottom;
    if layer.mask_enabled != 0u && inside_mask == (layer.mask_invert != 0u) {
        return vec4<f32>(0.0);
    }

    let source_coordinates =
        vec2<u32>(layer.crop_x, layer.crop_y) + cropped_coordinates;
    return textureLoad(source_texture, vec2<i32>(source_coordinates), 0);
}

@fragment
fn composition_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    if layer.visible == 0u {
        return vec4<f32>(0.0);
    }
    let opacity = f32(layer.opacity) / 255.0;
    let translation = vec2<i32>(
        bitcast<i32>(layer.translation_x),
        bitcast<i32>(layer.translation_y),
    );
    let pixel = vec2<i32>(position.xy);
    if layer.tilted == 0u {
        return footprint_texel(pixel - translation) * opacity;
    }

    // Tilted: the pixel centre inverse-maps about the footprint centre into
    // the footprint, floored once to 1/256 pixel, then four footprint texels
    // mix with 8-bit weights (the CPU reference's bilinear sample).
    let size = vec2<i32>(vec2<u32>(layer.rotated_width, layer.rotated_height));
    let delta = vec2<f32>(2 * (pixel - translation) + vec2<i32>(1) - size);
    let u = layer.tilt_cos * delta.x + layer.tilt_sin * delta.y + f32(size.x - 1);
    let v = -layer.tilt_sin * delta.x + layer.tilt_cos * delta.y + f32(size.y - 1);
    let q8 = vec2<i32>(floor(vec2<f32>(u, v) * 128.0));
    let cell = q8 >> vec2<u32>(8u);
    let weight = vec2<f32>(q8 & vec2<i32>(255)) / 256.0;
    if cell.x < -1 || cell.y < -1 || cell.x >= size.x || cell.y >= size.y {
        return vec4<f32>(0.0);
    }
    let top = mix(
        footprint_texel(cell),
        footprint_texel(cell + vec2<i32>(1, 0)),
        weight.x,
    );
    let bottom = mix(
        footprint_texel(cell + vec2<i32>(0, 1)),
        footprint_texel(cell + vec2<i32>(1, 1)),
        weight.x,
    );
    return mix(top, bottom, weight.y) * opacity;
}
";

const TRANSITION_FRAGMENT_SHADER: &str = r"
// Operations: 0 Cut, 1 Fade/AlphaFade, 2 Wipe, 3 Slide, 4 Zoom,
// 5 VerticalWipe, 6 VerticalSlide, 7 Fly/FlyRotate, 8 Cube/CubeZoom,
// 9 CrossZoom, 10 Merge. Geometry mirrors fm_video::render_transition.
struct TransitionUniform {
    operation: u32,
    numerator: u32,
    denominator: u32,
    // Wipe/Slide/VerticalWipe/VerticalSlide offset, or Merge's left extent.
    boundary: u32,
    // Zoom width, or Merge's right extent.
    scaled_width: u32,
    scaled_height: u32,
    padding_0: u32,
    padding_1: u32,
    sin: f32,
    cos: f32,
    scale: f32,
    second_scale: f32,
    offset_x: f32,
    depth: f32,
    camera: f32,
    padding_2: f32,
};

@group(0) @binding(0) var from_texture: texture_2d<f32>;
@group(0) @binding(1) var to_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> transition: TransitionUniform;

// One premultiplied texel of `from` (which == 0) or `to`, transparent outside.
// `seam` 1 repeats the first column for taps left of it, 2 the last column
// for taps right of it (a cube face's crease).
fn texel(which: u32, cell_unclamped: vec2<i32>, seam: u32) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(from_texture));
    var cell = cell_unclamped;
    if seam == 1u {
        cell.x = max(cell.x, 0);
    } else if seam == 2u {
        cell.x = min(cell.x, size.x - 1);
    }
    if any(cell < vec2<i32>(0)) || any(cell >= size) {
        return vec4<f32>(0.0);
    }
    if which == 0u {
        return textureLoad(from_texture, cell, 0);
    }
    return textureLoad(to_texture, cell, 0);
}

// The CPU reference's sampler: the texel-centre coordinate floors once to
// 1/256 texel, then four texels mix with 8-bit weights.
fn bilinear(which: u32, coordinates: vec2<f32>, seam: u32) -> vec4<f32> {
    let limit = vec2<f32>(textureDimensions(from_texture)) * 256.0;
    let scaled = floor(coordinates * 256.0);
    if !(all(scaled >= vec2<f32>(-256.0)) && all(scaled < limit)) {
        return vec4<f32>(0.0);
    }
    let q8 = vec2<i32>(scaled);
    let cell = q8 >> vec2<u32>(8u);
    let weight = vec2<f32>(q8 & vec2<i32>(255)) / 256.0;
    let top = mix(
        texel(which, cell, seam),
        texel(which, cell + vec2<i32>(1, 0), seam),
        weight.x,
    );
    let bottom = mix(
        texel(which, cell + vec2<i32>(0, 1), seam),
        texel(which, cell + vec2<i32>(1, 1), seam),
        weight.x,
    );
    return mix(top, bottom, weight.y);
}

fn over(source: vec4<f32>, destination: vec4<f32>) -> vec4<f32> {
    return source + destination * (1.0 - source.a);
}

struct FaceHit {
    hit: bool,
    coordinates: vec2<f32>,
}

// The ray through `screen` against one cube face's plane (world x/z centre,
// axis, and outward normal): its face coordinates, or no hit for a back face
// or a ray that never reaches the plane.
fn cube_face(
    screen: vec2<f32>,
    center: vec2<f32>,
    axis: vec2<f32>,
    normal: vec2<f32>,
) -> FaceHit {
    var result = FaceHit(false, vec2<f32>(0.0));
    let camera = transition.camera;
    let facing = center.x * normal.x + (camera + center.y) * normal.y;
    if !(facing < 0.0) {
        return result;
    }
    let denominator = screen.x * normal.x + camera * normal.y;
    if !(denominator < 0.0) {
        return result;
    }
    let t = facing / denominator;
    let x = t * screen.x;
    let z = t * camera - camera;
    result.hit = true;
    result.coordinates = vec2<f32>(
        (x - center.x) * axis.x + (z - center.y) * axis.y,
        t * screen.y,
    );
    return result;
}

@fragment
fn transition_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let coordinates = vec2<i32>(position.xy);
    let source = textureLoad(from_texture, coordinates, 0);
    let destination = textureLoad(to_texture, coordinates, 0);
    if transition.operation == 0u || transition.numerator == transition.denominator {
        return destination;
    }
    if transition.numerator == 0u {
        return source;
    }
    let dimensions = textureDimensions(from_texture);
    let size = vec2<f32>(dimensions);
    // The pixel centre relative to the frame centre, and the frame centre in
    // texel coordinates.
    let screen = position.xy - size / 2.0;
    let texel_center = (size - vec2<f32>(1.0)) / 2.0;
    let pixel = vec2<u32>(position.xy);
    switch transition.operation {
        case 2u: {
            if pixel.x < transition.boundary {
                return destination;
            }
            return source;
        }
        case 3u: {
            let remaining = dimensions.x - transition.boundary;
            if pixel.x < remaining {
                return textureLoad(
                    from_texture,
                    vec2<i32>(i32(pixel.x + transition.boundary), coordinates.y),
                    0,
                );
            }
            return textureLoad(
                to_texture,
                vec2<i32>(i32(pixel.x - remaining), coordinates.y),
                0,
            );
        }
        case 4u: {
            if transition.scaled_width == 0u || transition.scaled_height == 0u {
                return source;
            }
            let extent = vec2<u32>(transition.scaled_width, transition.scaled_height);
            let origin = (dimensions - extent) / vec2<u32>(2u);
            if any(pixel < origin) || any(pixel >= origin + extent) {
                return source;
            }
            let relative = pixel - origin;
            let destination_coordinates = relative * dimensions / extent;
            return textureLoad(to_texture, vec2<i32>(destination_coordinates), 0);
        }
        case 5u: {
            if pixel.y < transition.boundary {
                return destination;
            }
            return source;
        }
        case 6u: {
            let remaining = dimensions.y - transition.boundary;
            if pixel.y < remaining {
                return textureLoad(
                    from_texture,
                    vec2<i32>(coordinates.x, i32(pixel.y + transition.boundary)),
                    0,
                );
            }
            return textureLoad(
                to_texture,
                vec2<i32>(coordinates.x, i32(pixel.y - remaining)),
                0,
            );
        }
        case 7u: {
            let dx = screen.x - transition.offset_x;
            let dy = screen.y;
            let a = (transition.cos * dx + transition.sin * dy) / transition.scale;
            let b = (transition.cos * dy - transition.sin * dx) / transition.scale;
            return over(bilinear(1u, vec2<f32>(a, b) + texel_center, 0u), source);
        }
        case 8u: {
            let sin = transition.sin;
            let cos = transition.cos;
            let depth = transition.depth;
            let half = size.x / 2.0;
            let front = cube_face(
                screen,
                vec2<f32>(-half * sin, depth - half * cos),
                vec2<f32>(cos, -sin),
                vec2<f32>(-sin, -cos),
            );
            let right = cube_face(
                screen,
                vec2<f32>(half * cos, depth - half * sin),
                vec2<f32>(sin, cos),
                vec2<f32>(cos, -sin),
            );
            // Both faces visible: the face on the ray's side of the crease
            // draws, extended across it.
            if front.hit && right.hit {
                if front.coordinates.x < half {
                    return bilinear(0u, front.coordinates + texel_center, 2u);
                }
                return bilinear(1u, right.coordinates + texel_center, 1u);
            }
            if front.hit {
                return bilinear(0u, front.coordinates + texel_center, 0u);
            }
            if right.hit {
                return bilinear(1u, right.coordinates + texel_center, 0u);
            }
            return vec4<f32>(0.0);
        }
        case 9u: {
            let first = bilinear(0u, screen / transition.scale + texel_center, 0u);
            let second = bilinear(1u, screen / transition.second_scale + texel_center, 0u);
            return mix(first, second, f32(transition.numerator) / f32(transition.denominator));
        }
        case 10u: {
            let left_half = dimensions.x / 2u;
            if pixel.x < transition.boundary {
                return textureLoad(
                    to_texture,
                    vec2<i32>(i32(left_half - transition.boundary + pixel.x), coordinates.y),
                    0,
                );
            }
            let right_start = dimensions.x - transition.scaled_width;
            if pixel.x >= right_start {
                return textureLoad(
                    to_texture,
                    vec2<i32>(i32(left_half + pixel.x - right_start), coordinates.y),
                    0,
                );
            }
            return source;
        }
        default: {
            return mix(source, destination, f32(transition.numerator) / f32(transition.denominator));
        }
    }
}
";

const FADE_TO_BLACK_UNIFORM_SIZE: usize = 32;

const FADE_TO_BLACK_FRAGMENT_SHADER: &str = r"
struct FadeToBlackUniform {
    start_numerator: u32,
    start_denominator: u32,
    end_numerator: u32,
    end_denominator: u32,
    progress_numerator: u32,
    progress_denominator: u32,
    padding_0: u32,
    padding_1: u32,
};

@group(0) @binding(0) var program_texture: texture_2d<f32>;
@group(0) @binding(1) var unused_program_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> ftb: FadeToBlackUniform;

fn ratio(numerator: u32, denominator: u32) -> f32 {
    return f32(numerator) / f32(denominator);
}

fn affine_scalar(source_value: f32, target_value: f32, progress: f32) -> f32 {
    return source_value + (target_value - source_value) * progress;
}

fn affine_rgba(source_value: vec4<f32>, target_value: vec4<f32>, progress: f32) -> vec4<f32> {
    return source_value + (target_value - source_value) * progress;
}

@fragment
fn fade_to_black_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let coordinates = vec2<i32>(position.xy);
    let source = textureLoad(program_texture, coordinates, 0);
    let start = ratio(ftb.start_numerator, ftb.start_denominator);
    let end = ratio(ftb.end_numerator, ftb.end_denominator);
    var fade_position = start;
    if ftb.progress_numerator == ftb.progress_denominator {
        fade_position = end;
    } else if ftb.progress_numerator != 0u {
        fade_position = affine_scalar(
            start,
            end,
            ratio(ftb.progress_numerator, ftb.progress_denominator),
        );
    }
    if fade_position <= 0.0 {
        return source;
    }
    let opaque_black = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if fade_position >= 1.0 {
        return opaque_black;
    }
    return affine_rgba(source, opaque_black, fade_position);
}
";

const STINGER_UNIFORM_SIZE: usize = 16;

const STINGER_FRAGMENT_SHADER: &str = r"
struct StingerUniform {
    padding_0: u32,
    padding_1: u32,
    padding_2: u32,
    padding_3: u32,
};

@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var media_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> unused_stinger: StingerUniform;

@fragment
fn stinger_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let coordinates = vec2<i32>(position.xy);
    let base = textureLoad(base_texture, coordinates, 0);
    let media = textureLoad(media_texture, coordinates, 0);
    return media + base * (1.0 - media.a);
}
";

/// Binds a plan source identifier to a canonical native working texture.
///
/// The texture must be `Rgba16Float` containing linear-light, premultiplied-alpha
/// working pixels. Import normalization has already canonicalized alpha, so the
/// native compositor never premultiplies again based on `PlanLayer::alpha_mode`.
#[derive(Clone, Copy)]
pub struct NativeSourceFrame<'a> {
    pub source: SourceId,
    pub texture: &'a NativeTexture,
}

impl<'a> NativeSourceFrame<'a> {
    #[must_use]
    pub const fn new(source: SourceId, texture: &'a NativeTexture) -> Self {
        Self { source, texture }
    }
}

/// Errors produced by native composition-plan execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCompositionError {
    MissingSource(SourceId),
    DuplicateSource(SourceId),
    SourceFormat {
        source: SourceId,
        actual: TextureFormat,
    },
    InvalidCrop {
        layer: usize,
        crop: CropRect,
        source_width: u32,
        source_height: u32,
    },
    InvalidTransformDimensions {
        layer: usize,
        width: u32,
        height: u32,
    },
    TransformDimensionLimit {
        layer: usize,
        width: u32,
        height: u32,
        maximum: u32,
    },
    /// A key/fill layer whose key source is not the fill source's size.
    KeyFillDimensionMismatch {
        layer: usize,
        fill_width: u32,
        fill_height: u32,
        key_width: u32,
        key_height: u32,
    },
    UnsupportedEffect {
        layer: usize,
        effect: usize,
        name: String,
    },
    UnsupportedSafeAreas {
        count: usize,
    },
    Gpu(NativeGpuError),
}

impl fmt::Display for NativeCompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSource(source) => write!(formatter, "source {} is missing", source.0),
            Self::DuplicateSource(source) => {
                write!(formatter, "source {} was supplied more than once", source.0)
            }
            Self::SourceFormat { source, actual } => write!(
                formatter,
                "source {} has native format {actual:?}; expected Rgba16Float",
                source.0
            ),
            Self::InvalidCrop {
                layer,
                crop,
                source_width,
                source_height,
            } => write!(
                formatter,
                "layer {layer} crop {crop:?} exceeds source bounds {source_width}x{source_height}"
            ),
            Self::InvalidTransformDimensions {
                layer,
                width,
                height,
            } => write!(
                formatter,
                "layer {layer} native transform dimensions must be nonzero, got {width}x{height}"
            ),
            Self::TransformDimensionLimit {
                layer,
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "layer {layer} native transform dimensions {width}x{height} exceed {maximum}"
            ),
            Self::KeyFillDimensionMismatch {
                layer,
                fill_width,
                fill_height,
                key_width,
                key_height,
            } => write!(
                formatter,
                "layer {layer} key source is {key_width}x{key_height}, but its fill is \
                 {fill_width}x{fill_height}"
            ),
            Self::UnsupportedEffect {
                layer,
                effect,
                name,
            } => write!(
                formatter,
                "layer {layer} effect {effect} ({name}) is unsupported by native composition"
            ),
            Self::UnsupportedSafeAreas { count } => write!(
                formatter,
                "native composition does not support {count} safe-area guides"
            ),
            Self::Gpu(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeCompositionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gpu(error) => Some(error),
            _ => None,
        }
    }
}

impl From<NativeGpuError> for NativeCompositionError {
    fn from(value: NativeGpuError) -> Self {
        Self::Gpu(value)
    }
}

/// Maps a grade-pass failure for `source`. Source formats are validated
/// before any grade runs, so a format error here still names the source.
fn grade_error(source: SourceId, error: NativeGradeError) -> NativeCompositionError {
    match error {
        NativeGradeError::SourceFormat { actual } => {
            NativeCompositionError::SourceFormat { source, actual }
        }
        NativeGradeError::Gpu(error) => NativeCompositionError::Gpu(error),
    }
}

/// Native executor for compiled composition plans.
///
/// Inputs are canonical `Rgba16Float`, linear-light, premultiplied-alpha working
/// textures. `PlanLayer::alpha_mode` describes pre-normalization/CPU execution
/// and does not cause additional premultiplication in this executor.
///
/// Layers with a chroma/luma key or reference effects first run a layer
/// preparation pass chain over their cropped source; layers with a
/// non-identity grade are then graded by [`NativeGradeRenderer`] (baked LUTs
/// are cached across frames).
pub struct NativeCompositionRenderer {
    pipeline: NativeFullscreenPipeline,
    layer: NativeLayerRenderer,
    grade: NativeGradeRenderer,
    grade_cache: NativeGradeCache,
}

impl NativeCompositionRenderer {
    /// Compiles the native composition pipeline on `context`.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn new(context: &NativeContext) -> Result<Self, NativeCompositionError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native composition",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "composition_fragment",
                    ShaderSource::Text(COMPOSITION_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: NativeFullscreenBlend::PremultipliedSourceOver,
                    uniform_size: COMPOSITION_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::Independent,
                },
            )
            .await?;
        let layer = NativeLayerRenderer::new(context).await?;
        let grade = NativeGradeRenderer::new(context)
            .await
            .map_err(|error| match error {
                NativeGradeError::Gpu(error) => NativeCompositionError::Gpu(error),
                NativeGradeError::SourceFormat { .. } => {
                    unreachable!("grade pipeline creation does not inspect sources")
                }
            })?;
        Ok(Self {
            pipeline,
            layer,
            grade,
            grade_cache: NativeGradeCache::default(),
        })
    }

    /// Renders `plan` into one GPU-resident `Rgba16Float` output texture.
    ///
    /// Source bindings must be unique canonical `Rgba16Float`, linear-light,
    /// premultiplied-alpha textures. Compositor-owned plan, source identifier,
    /// format, crop, and transform validation completes before output allocation;
    /// opaque GPU context ownership is validated by `fm-gpu` at submission.
    ///
    /// A layer with a chroma/luma key or a non-`passthrough` reference effect
    /// is first prepared in the CPU reference order: one pass over the crop
    /// computes the key alpha times the rect-mask alpha and chroma spill on
    /// straight 8-bit sRGB codes, then one pass per effect runs its exact
    /// integer formula (see [`crate::ReferenceEffect`]); the crop-sized result
    /// replaces the source, with crop and mask already applied. Graded layers
    /// then run one grade pass per grade (the source grade, then the layer
    /// grade) over that prepared texture, or over the whole source texture
    /// for unprepared layers (grades commute with crop and precede
    /// transform, mask, and opacity, as on the CPU path). Layers are then
    /// submitted in plan order as one bounded, premultiplied source-over
    /// render pass without readback.
    ///
    /// A key/fill layer or a layer with a key refinement first builds a
    /// crop-sized matte (raw key alpha from the layer's source or the key
    /// source, clip, choke/spread, edge softness) that the key stage then
    /// reads; see [`crate::KeyRefinement`] and [`crate::KeyFill`]. A missing
    /// or differently sized key source, unparseable effect descriptors, and
    /// safe-area guides are rejected before any GPU allocation.
    ///
    /// # Errors
    ///
    /// Returns a typed source, crop, transform, unsupported-work, or GPU error.
    pub async fn render(
        &self,
        context: &NativeContext,
        plan: &CompositionPlan,
        sources: &[NativeSourceFrame<'_>],
    ) -> Result<NativeTexture, NativeCompositionError> {
        let prepared = validate_composition(plan, sources)?;
        let mut graded = Vec::with_capacity(prepared.len());
        for layer in &prepared {
            let keyed = match &layer.preparation {
                Some(preparation) => Some(
                    self.layer
                        .prepare(context, layer.texture, layer.key_texture, preparation)
                        .await?,
                ),
                None => None,
            };
            let source = keyed.as_ref().unwrap_or(layer.texture);
            graded.push(self.grade_layer(context, layer, source).await?.or(keyed));
        }
        let output = context
            .create_rgba16_float_render_target(plan.width(), plan.height())
            .await?;
        let draws = prepared
            .iter()
            .zip(&graded)
            .map(|(layer, graded)| {
                let texture = graded.as_ref().unwrap_or(layer.texture);
                NativeFullscreenDraw::new(&self.pipeline, texture, texture, &layer.uniform)
            })
            .collect::<Vec<_>>();
        let background = plan.background();
        context
            .submit_fullscreen_pass(
                &output,
                NativeFullscreenLoadOp::ClearRgba8([
                    background.r,
                    background.g,
                    background.b,
                    background.a,
                ]),
                &draws,
            )
            .await?;
        Ok(output)
    }

    async fn grade_layer(
        &self,
        context: &NativeContext,
        layer: &PreparedLayer<'_>,
        source: &NativeTexture,
    ) -> Result<Option<NativeTexture>, NativeCompositionError> {
        let mut graded: Option<NativeTexture> = None;
        for grade in &layer.grades {
            let Some(lut) = self
                .grade_cache
                .lut(&self.grade, context, grade)
                .await
                .map_err(|error| grade_error(layer.source, error))?
            else {
                continue;
            };
            let texture = self
                .grade
                .apply(context, &lut, graded.as_ref().unwrap_or(source))
                .await
                .map_err(|error| grade_error(layer.source, error))?;
            graded = Some(texture);
        }
        Ok(graded)
    }
}

struct PreparedLayer<'a> {
    source: SourceId,
    texture: &'a NativeTexture,
    /// The key/fill key source, if any (same size as `texture`).
    key_texture: Option<&'a NativeTexture>,
    /// Non-identity grades in order: the source grade, then the layer's own.
    grades: Vec<&'a Arc<CompiledGrade>>,
    /// Key/mask/effect passes producing a crop-sized texture, if any.
    preparation: Option<LayerPreparation>,
    uniform: [u8; COMPOSITION_UNIFORM_SIZE],
}

fn validate_composition<'a>(
    plan: &'a CompositionPlan,
    sources: &'a [NativeSourceFrame<'a>],
) -> Result<Vec<PreparedLayer<'a>>, NativeCompositionError> {
    validate_unique_native_sources(sources)?;
    for source in sources {
        if source.texture.format() != TextureFormat::Rgba16Float {
            return Err(NativeCompositionError::SourceFormat {
                source: source.source,
                actual: source.texture.format(),
            });
        }
    }
    validate_supported_work(plan)?;

    let mut prepared = Vec::with_capacity(plan.layers().len());
    for (layer_index, layer) in plan.layers().iter().enumerate() {
        let texture = sources
            .iter()
            .find(|candidate| candidate.source == layer.source())
            .ok_or(NativeCompositionError::MissingSource(layer.source()))?
            .texture;
        let crop = layer
            .crop()
            .unwrap_or(CropRect::new(0, 0, texture.width(), texture.height()));
        validate_native_crop(layer_index, crop, texture.width(), texture.height())?;
        validate_native_transform(layer_index, layer.transform())?;
        let tilt = Tilt::new(layer.transform(), layer.rotation_millidegrees());
        let visible = tilt.map_or_else(
            || transform_is_visible(plan.width(), plan.height(), layer.transform()),
            |tilt| tilt.touches(plan.width(), plan.height()),
        );
        let key_texture = match layer.key_fill() {
            Some(fill) => {
                let key = sources
                    .iter()
                    .find(|candidate| candidate.source == fill.key_source)
                    .ok_or(NativeCompositionError::MissingSource(fill.key_source))?
                    .texture;
                if (key.width(), key.height()) != (texture.width(), texture.height()) {
                    return Err(NativeCompositionError::KeyFillDimensionMismatch {
                        layer: layer_index,
                        fill_width: texture.width(),
                        fill_height: texture.height(),
                        key_width: key.width(),
                        key_height: key.height(),
                    });
                }
                Some(key)
            }
            None => None,
        };
        let preparation = LayerPreparation::plan(layer_index, layer, crop)?;
        // A prepared texture is the crop with its mask already applied.
        let (crop, mask) = match &preparation {
            Some(preparation) => {
                let (width, height) = preparation.size();
                (CropRect::new(0, 0, width, height), None)
            }
            None => (crop, layer.mask()),
        };
        prepared.push(PreparedLayer {
            source: layer.source(),
            texture,
            key_texture,
            grades: layer
                .grade()
                .into_iter()
                .chain(layer.layer_grade())
                .filter(|grade| !grade.is_identity())
                .collect(),
            preparation,
            uniform: encode_composition_uniform(
                layer.transform(),
                crop,
                mask,
                layer.opacity(),
                visible,
                layer.inset_border_width(),
                tilt,
            ),
        });
    }
    Ok(prepared)
}

fn validate_unique_native_sources(
    sources: &[NativeSourceFrame<'_>],
) -> Result<(), NativeCompositionError> {
    for (index, source) in sources.iter().enumerate() {
        if sources[..index]
            .iter()
            .any(|candidate| candidate.source == source.source)
        {
            return Err(NativeCompositionError::DuplicateSource(source.source));
        }
    }
    Ok(())
}

fn validate_supported_work(plan: &CompositionPlan) -> Result<(), NativeCompositionError> {
    if !plan.safe_areas().is_empty() {
        return Err(NativeCompositionError::UnsupportedSafeAreas {
            count: plan.safe_areas().len(),
        });
    }
    for (layer_index, layer) in plan.layers().iter().enumerate() {
        for (effect_index, effect) in layer.effects().iter().enumerate() {
            native_effect(layer_index, effect_index, effect)?;
        }
    }
    Ok(())
}

fn validate_native_crop(
    layer: usize,
    crop: CropRect,
    source_width: u32,
    source_height: u32,
) -> Result<(), NativeCompositionError> {
    let valid = crop.width > 0
        && crop.height > 0
        && crop
            .x
            .checked_add(crop.width)
            .is_some_and(|right| right <= source_width)
        && crop
            .y
            .checked_add(crop.height)
            .is_some_and(|bottom| bottom <= source_height);
    if valid {
        Ok(())
    } else {
        Err(NativeCompositionError::InvalidCrop {
            layer,
            crop,
            source_width,
            source_height,
        })
    }
}

fn validate_native_transform(
    layer: usize,
    transform: Transform,
) -> Result<(), NativeCompositionError> {
    if transform.scale_width == 0 || transform.scale_height == 0 {
        return Err(NativeCompositionError::InvalidTransformDimensions {
            layer,
            width: transform.scale_width,
            height: transform.scale_height,
        });
    }
    if transform.scale_width > MAX_NATIVE_TRANSFORM_DIMENSION
        || transform.scale_height > MAX_NATIVE_TRANSFORM_DIMENSION
    {
        return Err(NativeCompositionError::TransformDimensionLimit {
            layer,
            width: transform.scale_width,
            height: transform.scale_height,
            maximum: MAX_NATIVE_TRANSFORM_DIMENSION,
        });
    }
    Ok(())
}

fn transform_is_visible(output_width: u32, output_height: u32, transform: Transform) -> bool {
    let (rotated_width, rotated_height) = rotated_dimensions(transform);
    let left = i64::from(transform.translation_x);
    let top = i64::from(transform.translation_y);
    let right = left + i64::from(rotated_width);
    let bottom = top + i64::from(rotated_height);
    left < i64::from(output_width) && top < i64::from(output_height) && right > 0 && bottom > 0
}

fn encode_composition_uniform(
    transform: Transform,
    crop: CropRect,
    mask: Option<RectMask>,
    opacity: u8,
    visible: bool,
    border_width: u32,
    tilt: Option<Tilt>,
) -> [u8; COMPOSITION_UNIFORM_SIZE] {
    let (rotated_width, rotated_height) = rotated_dimensions(transform);
    let rotation = match transform.rotation {
        Rotation::Deg0 => 0,
        Rotation::Deg90 => 1,
        Rotation::Deg180 => 2,
        Rotation::Deg270 => 3,
    };
    let (mask_left, mask_top, mask_right, mask_bottom, mask_enabled, mask_invert) =
        if let Some(mask) = mask {
            (
                mask.x,
                mask.y,
                mask.x
                    .checked_add(mask.width)
                    .expect("compiled mask right edge cannot overflow"),
                mask.y
                    .checked_add(mask.height)
                    .expect("compiled mask bottom edge cannot overflow"),
                1,
                u32::from(mask.invert),
            )
        } else {
            (0, 0, 0, 0, 0, 0)
        };
    let words = [
        transform.translation_x.cast_unsigned(),
        transform.translation_y.cast_unsigned(),
        transform.scale_width,
        transform.scale_height,
        rotation,
        crop.x,
        crop.y,
        crop.width,
        crop.height,
        u32::from(opacity),
        u32::from(visible),
        rotated_width,
        rotated_height,
        mask_left,
        mask_top,
        mask_right,
        mask_bottom,
        mask_enabled,
        mask_invert,
        border_width,
        u32::from(tilt.is_some()),
        tilt.map_or(0, |tilt| q30_to_f32(tilt.sin).to_bits()),
        tilt.map_or(0, |tilt| q30_to_f32(tilt.cos).to_bits()),
        0,
    ];
    let mut bytes = [0; COMPOSITION_UNIFORM_SIZE];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// A Q30 sine or cosine as the nearest `f32`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // |value| <= 2^30
fn q30_to_f32(value: i64) -> f32 {
    (value as f64 / fm_animation::TRIG_ONE as f64) as f32
}

const fn rotated_dimensions(transform: Transform) -> (u32, u32) {
    match transform.rotation {
        Rotation::Deg0 | Rotation::Deg180 => (transform.scale_width, transform.scale_height),
        Rotation::Deg90 | Rotation::Deg270 => (transform.scale_height, transform.scale_width),
    }
}

/// Errors produced by native Fade-to-Black application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeFadeToBlackError {
    SourceFormat { actual: TextureFormat },
    Gpu(NativeGpuError),
}

impl fmt::Display for NativeFadeToBlackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceFormat { actual } => write!(
                formatter,
                "Fade-to-Black source has native format {actual:?}; expected Rgba16Float"
            ),
            Self::Gpu(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeFadeToBlackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gpu(error) => Some(error),
            Self::SourceFormat { .. } => None,
        }
    }
}

impl From<NativeGpuError> for NativeFadeToBlackError {
    fn from(value: NativeGpuError) -> Self {
        Self::Gpu(value)
    }
}

/// Errors produced by native Stinger frame composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeStingerError {
    DimensionMismatch {
        role: StingerFrameRole,
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    ProgramFormat {
        actual: TextureFormat,
    },
    PreviewFormat {
        actual: TextureFormat,
    },
    MediaFormat {
        actual: TextureFormat,
    },
    Gpu(NativeGpuError),
}

impl fmt::Display for NativeStingerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionMismatch {
                role,
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                formatter,
                "native stinger {role:?} texture is {actual_width}x{actual_height}, expected \
                 {expected_width}x{expected_height}"
            ),
            Self::ProgramFormat { actual } => write!(
                formatter,
                "native stinger Program texture has format {actual:?}; expected Rgba16Float"
            ),
            Self::PreviewFormat { actual } => write!(
                formatter,
                "native stinger Preview texture has format {actual:?}; expected Rgba16Float"
            ),
            Self::MediaFormat { actual } => write!(
                formatter,
                "native stinger media texture has format {actual:?}; expected Rgba16Float"
            ),
            Self::Gpu(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeStingerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gpu(error) => Some(error),
            _ => None,
        }
    }
}

impl From<NativeGpuError> for NativeStingerError {
    fn from(value: NativeGpuError) -> Self {
        Self::Gpu(value)
    }
}

/// Native source-over renderer for one canonical Stinger media frame.
pub struct NativeStingerRenderer {
    pipeline: NativeFullscreenPipeline,
}

impl NativeStingerRenderer {
    /// Compiles the Stinger source-over shader on an existing native context.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn new(context: &NativeContext) -> Result<Self, NativeStingerError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native Stinger",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "stinger_fragment",
                    ShaderSource::Text(STINGER_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: NativeFullscreenBlend::Replace,
                    uniform_size: STINGER_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::MatchTarget,
                },
            )
            .await?;
        Ok(Self { pipeline })
    }

    /// Composes `media` over Program before the cut point and Preview after it.
    ///
    /// Inputs must be equally sized canonical `Rgba16Float`, linear-light,
    /// premultiplied-alpha textures. The returned texture remains GPU-resident.
    ///
    /// # Errors
    ///
    /// Returns a typed dimension or format error before output allocation, or
    /// a mapped GPU resource, context, uniform, or validation error.
    pub async fn render(
        &self,
        context: &NativeContext,
        plan: StingerFramePlan,
        program: &NativeTexture,
        preview: &NativeTexture,
        media: &NativeTexture,
    ) -> Result<NativeTexture, NativeStingerError> {
        validate_stinger_dimensions(program, preview, StingerFrameRole::Preview)?;
        validate_stinger_dimensions(program, media, StingerFrameRole::Media)?;
        validate_stinger_formats(program, preview, media)?;
        let base = match plan.base() {
            StingerBase::Program => program,
            StingerBase::Preview => preview,
        };
        let output = context
            .create_rgba16_float_render_target(program.width(), program.height())
            .await?;
        context
            .submit_fullscreen(
                &self.pipeline,
                base,
                media,
                &output,
                &[0; STINGER_UNIFORM_SIZE],
            )
            .await?;
        Ok(output)
    }
}

fn validate_stinger_dimensions(
    program: &NativeTexture,
    texture: &NativeTexture,
    role: StingerFrameRole,
) -> Result<(), NativeStingerError> {
    if texture.width() == program.width() && texture.height() == program.height() {
        return Ok(());
    }
    Err(NativeStingerError::DimensionMismatch {
        role,
        expected_width: program.width(),
        expected_height: program.height(),
        actual_width: texture.width(),
        actual_height: texture.height(),
    })
}

fn validate_stinger_formats(
    program: &NativeTexture,
    preview: &NativeTexture,
    media: &NativeTexture,
) -> Result<(), NativeStingerError> {
    if program.format() != TextureFormat::Rgba16Float {
        return Err(NativeStingerError::ProgramFormat {
            actual: program.format(),
        });
    }
    if preview.format() != TextureFormat::Rgba16Float {
        return Err(NativeStingerError::PreviewFormat {
            actual: preview.format(),
        });
    }
    if media.format() != TextureFormat::Rgba16Float {
        return Err(NativeStingerError::MediaFormat {
            actual: media.format(),
        });
    }
    Ok(())
}

/// Native Fade-to-Black executor for an already composed Program texture.
pub struct NativeFadeToBlackRenderer {
    pipeline: NativeFullscreenPipeline,
}

impl NativeFadeToBlackRenderer {
    /// Compiles the explicit 32-byte FTB uniform and replacement pipeline.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn new(context: &NativeContext) -> Result<Self, NativeFadeToBlackError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native Fade-to-Black",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "fade_to_black_fragment",
                    ShaderSource::Text(FADE_TO_BLACK_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: NativeFullscreenBlend::Replace,
                    uniform_size: FADE_TO_BLACK_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::MatchTarget,
                },
            )
            .await?;
        Ok(Self { pipeline })
    }

    /// Applies `plan` without color conversion, polling, or CPU readback.
    ///
    /// `program` must be the already composed canonical `Rgba16Float`,
    /// linear-light, premultiplied-alpha Program texture. The returned texture
    /// has the same dimensions and remains GPU-resident. Non-endpoint ratio and
    /// affine evaluation use GPU `f32` arithmetic, which a backend may contract,
    /// followed by binary16 render-target storage. This operation does not
    /// inspect or alter audio.
    ///
    /// # Errors
    ///
    /// Returns a source-format error before output allocation, or a mapped GPU
    /// resource, context, uniform, or validation error.
    pub async fn render(
        &self,
        context: &NativeContext,
        plan: FadeToBlackPlan,
        program: &NativeTexture,
    ) -> Result<NativeTexture, NativeFadeToBlackError> {
        validate_fade_to_black_format(program.format())?;
        let output = context
            .create_rgba16_float_render_target(program.width(), program.height())
            .await?;
        let uniform = encode_fade_to_black_uniform(plan);
        context
            .submit_fullscreen(&self.pipeline, program, program, &output, &uniform)
            .await?;
        Ok(output)
    }
}

fn validate_fade_to_black_format(format: TextureFormat) -> Result<(), NativeFadeToBlackError> {
    if format == TextureFormat::Rgba16Float {
        Ok(())
    } else {
        Err(NativeFadeToBlackError::SourceFormat { actual: format })
    }
}

fn encode_fade_to_black_uniform(plan: FadeToBlackPlan) -> [u8; FADE_TO_BLACK_UNIFORM_SIZE] {
    let words = [
        u32::from(plan.start().numerator()),
        u32::from(plan.start().denominator()),
        u32::from(plan.end().numerator()),
        u32::from(plan.end().denominator()),
        u32::from(plan.progress().numerator()),
        u32::from(plan.progress().denominator()),
        0,
        0,
    ];
    let mut bytes = [0; FADE_TO_BLACK_UNIFORM_SIZE];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Errors produced by the native transition renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeTransitionError {
    WidthMismatch { from: u32, to: u32 },
    HeightMismatch { from: u32, to: u32 },
    Gpu(NativeGpuError),
}

impl fmt::Display for NativeTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WidthMismatch { from, to } => {
                write!(
                    formatter,
                    "native transition widths differ: {from} and {to}"
                )
            }
            Self::HeightMismatch { from, to } => {
                write!(
                    formatter,
                    "native transition heights differ: {from} and {to}"
                )
            }
            Self::Gpu(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeTransitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gpu(error) => Some(error),
            _ => None,
        }
    }
}

impl From<NativeGpuError> for NativeTransitionError {
    fn from(value: NativeGpuError) -> Self {
        Self::Gpu(value)
    }
}

/// Native renderer for every [`TransitionPlan`] kind, containing only its compiled GPU pipeline.
///
/// It mirrors [`crate::execute_transition`] in `f32`: integer kinds copy the same texels, and
/// geometric kinds take the same Q8-floored bilinear taps with the parameters of
/// [`TransitionGeometry`].
pub struct NativeTransitionRenderer {
    pipeline: NativeFullscreenPipeline,
}

impl NativeTransitionRenderer {
    /// Compiles the compositor-owned transition fragment shader on `context`.
    /// No additional adapter or device is created.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn new(context: &NativeContext) -> Result<Self, NativeTransitionError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native transition",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "transition_fragment",
                    ShaderSource::Text(TRANSITION_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: NativeFullscreenBlend::Replace,
                    uniform_size: TRANSITION_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::MatchTarget,
                },
            )
            .await?;
        Ok(Self { pipeline })
    }

    /// Submits a GPU-resident transition of any [`TransitionPlan`] kind.
    /// This operation does not poll or read pixels back.
    ///
    /// # Errors
    ///
    /// Returns a dimension mismatch before allocation/submission, or a mapped
    /// GPU resource, context, alias, uniform, or validation error.
    pub async fn render(
        &self,
        context: &NativeContext,
        plan: TransitionPlan,
        from: &NativeTexture,
        to: &NativeTexture,
    ) -> Result<NativeTexture, NativeTransitionError> {
        validate_dimensions(from.width(), from.height(), to.width(), to.height())?;
        validate_format(from.format())?;
        validate_format(to.format())?;
        let output = context
            .create_rgba16_float_render_target(from.width(), from.height())
            .await?;
        let uniform = encode_uniform(plan, from.width(), from.height());
        context
            .submit_fullscreen(&self.pipeline, from, to, &output, &uniform)
            .await?;
        Ok(output)
    }
}

fn validate_format(format: TextureFormat) -> Result<(), NativeTransitionError> {
    if format == TextureFormat::Rgba16Float {
        Ok(())
    } else {
        Err(NativeGpuError::TextureFormatMismatch {
            expected: TextureFormat::Rgba16Float,
            actual: format,
        }
        .into())
    }
}

fn validate_dimensions(
    from_width: u32,
    from_height: u32,
    to_width: u32,
    to_height: u32,
) -> Result<(), NativeTransitionError> {
    if from_width != to_width {
        return Err(NativeTransitionError::WidthMismatch {
            from: from_width,
            to: to_width,
        });
    }
    if from_height != to_height {
        return Err(NativeTransitionError::HeightMismatch {
            from: from_height,
            to: to_height,
        });
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn encode_uniform(plan: TransitionPlan, width: u32, height: u32) -> [u8; TRANSITION_UNIFORM_SIZE] {
    let (numerator, denominator) = (plan.numerator(), plan.denominator());
    let extent = |size| transition_extent(size, numerator, denominator);
    let operation = match plan.kind() {
        TransitionKind::Cut => 0_u32,
        TransitionKind::Fade | TransitionKind::AlphaFade => 1,
        TransitionKind::Wipe => 2,
        TransitionKind::Slide => 3,
        TransitionKind::Zoom => 4,
        TransitionKind::VerticalWipe => 5,
        TransitionKind::VerticalSlide => 6,
        TransitionKind::Fly | TransitionKind::FlyRotate => 7,
        TransitionKind::Cube | TransitionKind::CubeZoom => 8,
        TransitionKind::CrossZoom => 9,
        TransitionKind::Merge => 10,
        TransitionKind::Stinger => {
            unreachable!("TransitionPlan does not compile Stinger")
        }
    };
    let (boundary, scaled_width, scaled_height) = match plan.kind() {
        TransitionKind::Wipe | TransitionKind::Slide => (extent(width), 0, 0),
        TransitionKind::VerticalWipe | TransitionKind::VerticalSlide => (extent(height), 0, 0),
        TransitionKind::Zoom => (0, extent(width), extent(height)),
        TransitionKind::Merge => (extent(width / 2), extent(width - width / 2), 0),
        _ => (0, 0, 0),
    };
    // Non-geometric kinds take the identity geometry.
    let geometry = TransitionGeometry::new(
        plan.kind().shape().unwrap_or(TransitionShape::Wipe),
        width,
        numerator,
        denominator,
    );
    let float = |value: f64| (value as f32).to_bits();
    let words = [
        operation,
        numerator,
        denominator,
        boundary,
        scaled_width,
        scaled_height,
        0,
        0,
        float(geometry.sin),
        float(geometry.cos),
        float(geometry.scale),
        float(geometry.second_scale),
        float(geometry.offset_x),
        float(geometry.depth),
        float(geometry.camera),
        0,
    ];
    let mut bytes = [0; TRANSITION_UNIFORM_SIZE];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Effect, FadeToBlackPosition, Key, LumaKey, OutputTarget, RectMask, Rgba8, SafeAreaGuide,
        Scene, SourceLayer, compile_scene,
    };

    fn composition_plan(layer: SourceLayer) -> CompositionPlan {
        let mut scene = Scene::new(8, 6, Rgba8::new(0, 0, 0, 255)).unwrap();
        scene.push_layer(layer);
        compile_scene(&scene, OutputTarget::Program).unwrap().0
    }

    fn transform(x: i32, y: i32, width: u32, height: u32, rotation: Rotation) -> Transform {
        Transform::new(x, y, width, height, rotation)
    }

    fn uniform_words(uniform: [u8; COMPOSITION_UNIFORM_SIZE]) -> [u32; 24] {
        let mut words = [0; 24];
        for (word, bytes) in words.iter_mut().zip(uniform.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        words
    }

    #[test]
    fn composition_uniform_is_twenty_four_little_endian_words_with_border_and_mask() {
        let transform = transform(-7, 9, 12, 5, Rotation::Deg90);
        let mask = RectMask::new(5, 6, 7, 8).inverted(true);
        let uniform = encode_composition_uniform(
            transform,
            CropRect::new(2, 3, 4, 6),
            Some(mask),
            128,
            true,
            3,
            None,
        );
        assert_eq!(uniform.len(), 96);
        assert_eq!(
            uniform_words(uniform),
            [
                (-7_i32).cast_unsigned(),
                9,
                12,
                5,
                1,
                2,
                3,
                4,
                6,
                128,
                1,
                5,
                12,
                5,
                6,
                12,
                14,
                1,
                1,
                3,
                0,
                0,
                0,
                0,
            ]
        );
    }

    #[test]
    fn a_tilt_fills_the_three_rotation_words_and_widens_visibility() {
        // A 4x2 footprint whose untilted box lies just off the right edge.
        let transform = transform(8, 0, 4, 2, Rotation::Deg0);
        assert!(!transform_is_visible(8, 6, transform));
        let tilt = Tilt::new(transform, 30_000).expect("30 degrees tilts");
        assert!(tilt.touches(8, 6));
        let words = uniform_words(encode_composition_uniform(
            transform,
            CropRect::new(0, 0, 1, 1),
            None,
            255,
            true,
            0,
            Some(tilt),
        ));
        assert_eq!(words[20], 1);
        assert_eq!(f32::from_bits(words[21]), 0.5);
        assert!((f32::from_bits(words[22]) - 0.866_025_4).abs() < 1e-6);
        assert_eq!(words[23], 0);
        assert_eq!(Tilt::new(transform, 360_000), None);
        assert_eq!(Tilt::new(transform, 0), None);
    }

    #[test]
    fn native_crop_validation_uses_checked_source_bounds() {
        assert_eq!(
            validate_native_crop(2, CropRect::new(1, 2, 3, 4), 4, 6),
            Ok(())
        );
        let overflow = CropRect::new(u32::MAX, 0, 2, 1);
        assert_eq!(
            validate_native_crop(3, overflow, u32::MAX, 1),
            Err(NativeCompositionError::InvalidCrop {
                layer: 3,
                crop: overflow,
                source_width: u32::MAX,
                source_height: 1,
            })
        );
        let outside = CropRect::new(3, 0, 2, 1);
        assert_eq!(
            validate_native_crop(4, outside, 4, 1),
            Err(NativeCompositionError::InvalidCrop {
                layer: 4,
                crop: outside,
                source_width: 4,
                source_height: 1,
            })
        );
    }

    #[test]
    fn native_transform_bound_and_extreme_visibility_are_prevalidated() {
        let oversized = transform(0, 0, MAX_NATIVE_TRANSFORM_DIMENSION + 1, 1, Rotation::Deg0);
        assert_eq!(
            validate_native_transform(5, oversized),
            Err(NativeCompositionError::TransformDimensionLimit {
                layer: 5,
                width: MAX_NATIVE_TRANSFORM_DIMENSION + 1,
                height: 1,
                maximum: MAX_NATIVE_TRANSFORM_DIMENSION,
            })
        );

        for translation in [i32::MIN, i32::MAX] {
            let offscreen = transform(translation, translation, 1, 1, Rotation::Deg0);
            assert!(!transform_is_visible(8, 6, offscreen));
            let words = uniform_words(encode_composition_uniform(
                offscreen,
                CropRect::new(0, 0, 1, 1),
                None,
                255,
                false,
                0,
                None,
            ));
            assert_eq!(words[0], translation.cast_unsigned());
            assert_eq!(words[1], translation.cast_unsigned());
            assert_eq!(words[10], 0);
        }
        assert!(transform_is_visible(
            8,
            6,
            transform(-1, -1, 2, 2, Rotation::Deg0)
        ));
    }

    #[test]
    fn masks_keys_effects_and_grades_are_supported_while_other_native_plan_features_return_indexed_errors()
     {
        let source = SourceId::new(1);
        let base = transform(0, 0, 1, 1, Rotation::Deg0);

        let mask = composition_plan(
            SourceLayer::new(source, 0, base).with_mask(RectMask::new(0, 0, 1, 1)),
        );
        assert_eq!(validate_supported_work(&mask), Ok(()));

        let keyed = composition_plan(
            SourceLayer::new(source, 0, base)
                .with_key(Key::Luma(LumaKey::new(1, 2, false).with_high(200)))
                .with_effect(Effect::passthrough())
                .with_effect(Effect::new("sharpen", vec![500]))
                .with_effect(Effect::new("flip", vec![1, 1])),
        );
        assert_eq!(validate_supported_work(&keyed), Ok(()));

        let refined = composition_plan(
            SourceLayer::new(source, 0, base)
                .with_key(Key::Luma(LumaKey::new(1, 2, false)))
                .with_key_refinement(crate::KeyRefinement::default().with_choke(2)),
        );
        assert_eq!(validate_supported_work(&refined), Ok(()));
        let fill = composition_plan(
            SourceLayer::new(source, 0, base)
                .with_key_fill(crate::KeyFill::linear(SourceId::new(2)))
                .with_key_refinement(crate::KeyRefinement::default().with_edge_softness(1)),
        );
        assert_eq!(validate_supported_work(&fill), Ok(()));

        for (name, parameters) in [
            ("blur", vec![1]),
            ("levels", vec![10, 10]),
            ("passthrough", vec![1]),
        ] {
            let effect = composition_plan(
                SourceLayer::new(source, 0, base)
                    .with_effect(Effect::passthrough())
                    .with_effect(Effect::new(name, parameters)),
            );
            assert_eq!(
                validate_supported_work(&effect),
                Err(NativeCompositionError::UnsupportedEffect {
                    layer: 0,
                    effect: 1,
                    name: name.to_owned(),
                })
            );
        }

        let identity = composition_plan(
            SourceLayer::new(source, 0, base)
                .with_grade(std::sync::Arc::new(fm_grade::CompiledGrade::identity())),
        );
        assert_eq!(validate_supported_work(&identity), Ok(()));
        let graded = composition_plan(
            SourceLayer::new(source, 0, base).with_grade(std::sync::Arc::new(
                fm_grade::CompiledGrade::new(
                    &fm_grade::Grade {
                        saturation_permille: 0,
                        ..fm_grade::Grade::IDENTITY
                    },
                    None,
                )
                .unwrap(),
            )),
        );
        assert_eq!(validate_supported_work(&graded), Ok(()));

        let mut scene = Scene::new(2, 2, Rgba8::new(0, 0, 0, 255)).unwrap();
        scene.push_safe_area(SafeAreaGuide::new(
            0,
            0,
            1,
            1,
            Rgba8::new(255, 255, 255, 255),
        ));
        let safe_areas = compile_scene(&scene, OutputTarget::Operator).unwrap().0;
        assert_eq!(
            validate_supported_work(&safe_areas),
            Err(NativeCompositionError::UnsupportedSafeAreas { count: 1 })
        );
    }

    #[test]
    fn transition_uniform_encodes_operation_progress_and_geometry() {
        let fade = TransitionPlan::compile(TransitionKind::Fade, 3, 7).unwrap();
        let uniform = encode_uniform(fade, 11, 7);
        assert_eq!(uniform.len(), 64);
        assert_eq!(u32::from_le_bytes(uniform[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(uniform[4..8].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(uniform[8..12].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(uniform[12..16].try_into().unwrap()), 0);

        let alpha_fade = TransitionPlan::compile(TransitionKind::AlphaFade, 2, 5).unwrap();
        let uniform = encode_uniform(alpha_fade, 11, 7);
        assert_eq!(u32::from_le_bytes(uniform[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(uniform[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(uniform[8..12].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(uniform[12..16].try_into().unwrap()), 0);

        let cut = TransitionPlan::compile(TransitionKind::Cut, 0, 1).unwrap();
        assert_eq!(
            u32::from_le_bytes(encode_uniform(cut, 11, 7)[0..4].try_into().unwrap()),
            0
        );

        let wipe = TransitionPlan::compile(TransitionKind::Wipe, 1, 2).unwrap();
        let uniform = encode_uniform(wipe, 5, 3);
        assert_eq!(u32::from_le_bytes(uniform[0..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(uniform[12..16].try_into().unwrap()), 2);

        let slide = TransitionPlan::compile(TransitionKind::Slide, 2, 3).unwrap();
        let uniform = encode_uniform(slide, 5, 3);
        assert_eq!(u32::from_le_bytes(uniform[0..4].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(uniform[12..16].try_into().unwrap()), 3);

        let zoom = TransitionPlan::compile(TransitionKind::Zoom, 1, 2).unwrap();
        let uniform = encode_uniform(zoom, 5, 3);
        assert_eq!(u32::from_le_bytes(uniform[0..4].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(uniform[16..20].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(uniform[20..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(uniform[24..28].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(uniform[28..32].try_into().unwrap()), 0);
        // Identity geometry for integer kinds.
        assert_eq!(
            transition_floats(uniform),
            [0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]
        );

        let word = |uniform: [u8; TRANSITION_UNIFORM_SIZE], index: usize| {
            u32::from_le_bytes(uniform[index * 4..index * 4 + 4].try_into().unwrap())
        };
        let encode = |kind, numerator, denominator, width, height| {
            encode_uniform(
                TransitionPlan::compile(kind, numerator, denominator).unwrap(),
                width,
                height,
            )
        };

        let vertical_wipe = encode(TransitionKind::VerticalWipe, 1, 2, 5, 7);
        assert_eq!([word(vertical_wipe, 0), word(vertical_wipe, 3)], [5, 3]);
        let vertical_slide = encode(TransitionKind::VerticalSlide, 2, 3, 5, 7);
        assert_eq!([word(vertical_slide, 0), word(vertical_slide, 3)], [6, 4]);
        let merge = encode(TransitionKind::Merge, 1, 2, 7, 3);
        assert_eq!([word(merge, 0), word(merge, 3), word(merge, 4)], [10, 1, 2]);

        let fly = encode(TransitionKind::Fly, 1, 2, 10, 3);
        assert_eq!(word(fly, 0), 7);
        assert_eq!(
            transition_floats(fly),
            [0.0, 1.0, 0.75, 1.0, 5.0, 0.0, 0.0, 0.0]
        );
        // (1 - 1/2) * 180 = 90 degrees: exact.
        let fly_rotate = encode(TransitionKind::FlyRotate, 1, 2, 10, 3);
        assert_eq!(word(fly_rotate, 0), 7);
        assert_eq!(
            transition_floats(fly_rotate),
            [1.0, 0.0, 0.75, 1.0, 5.0, 0.0, 0.0, 0.0]
        );
        let cube = encode(TransitionKind::Cube, 1, 1, 10, 3);
        assert_eq!(word(cube, 0), 8);
        assert_eq!(
            transition_floats(cube),
            [1.0, 0.0, 1.0, 1.0, 0.0, 5.0, 20.0, 0.0]
        );
        let cube_zoom = encode(TransitionKind::CubeZoom, 1, 4, 10, 3);
        assert_eq!(word(cube_zoom, 0), 8);
        let floats = transition_floats(cube_zoom);
        assert_eq!([floats[5], floats[6]], [10.0, 20.0]);
        let cross_zoom = encode(TransitionKind::CrossZoom, 1, 4, 10, 3);
        assert_eq!(
            [
                word(cross_zoom, 0),
                word(cross_zoom, 1),
                word(cross_zoom, 2)
            ],
            [9, 1, 4]
        );
        assert_eq!(
            transition_floats(cross_zoom),
            [0.0, 1.0, 1.5, 2.5, 0.0, 0.0, 0.0, 0.0]
        );
    }

    fn transition_floats(uniform: [u8; TRANSITION_UNIFORM_SIZE]) -> [f32; 8] {
        let mut floats = [0.0; 8];
        for (float, bytes) in floats.iter_mut().zip(uniform[32..].as_chunks::<4>().0) {
            *float = f32::from_le_bytes(*bytes);
        }
        floats
    }

    #[test]
    fn dimensions_are_validated_before_gpu_work() {
        assert_eq!(validate_dimensions(2, 2, 2, 2), Ok(()));
        assert_eq!(
            validate_dimensions(2, 2, 3, 2),
            Err(NativeTransitionError::WidthMismatch { from: 2, to: 3 })
        );
        assert_eq!(
            validate_dimensions(2, 2, 2, 3),
            Err(NativeTransitionError::HeightMismatch { from: 2, to: 3 })
        );
    }

    #[test]
    fn physical_format_must_be_canonical() {
        assert_eq!(validate_format(TextureFormat::Rgba16Float), Ok(()));
        assert_eq!(
            validate_format(TextureFormat::Rgba8Unorm),
            Err(NativeTransitionError::Gpu(
                NativeGpuError::TextureFormatMismatch {
                    expected: TextureFormat::Rgba16Float,
                    actual: TextureFormat::Rgba8Unorm,
                }
            ))
        );
    }

    #[test]
    fn fade_to_black_plan_has_an_explicit_bounded_uniform_layout() {
        let plan = FadeToBlackPlan::new(
            FadeToBlackPosition::compile(3, 4).unwrap(),
            FadeToBlackPosition::compile(1, 5).unwrap(),
            FadeToBlackPosition::compile(2, 3).unwrap(),
        );
        let uniform = encode_fade_to_black_uniform(plan);
        let mut words = [0_u32; 8];
        for (word, bytes) in words.iter_mut().zip(uniform.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        assert_eq!(uniform.len(), FADE_TO_BLACK_UNIFORM_SIZE);
        assert_eq!(words, [3, 4, 1, 5, 2, 3, 0, 0]);
        assert_eq!(
            validate_fade_to_black_format(TextureFormat::Rgba16Float),
            Ok(())
        );
    }

    #[test]
    fn fade_to_black_shader_uses_directional_affine_opaque_black() {
        assert_eq!(
            FADE_TO_BLACK_FRAGMENT_SHADER
                .match_indices("return source_value + (target_value - source_value) * progress;")
                .count(),
            2
        );
        assert!(
            FADE_TO_BLACK_FRAGMENT_SHADER
                .contains("fade_position = affine_scalar(\n            start,\n            end,")
        );
        assert!(
            FADE_TO_BLACK_FRAGMENT_SHADER
                .contains("let opaque_black = vec4<f32>(0.0, 0.0, 0.0, 1.0);")
        );
        assert!(
            FADE_TO_BLACK_FRAGMENT_SHADER
                .contains("return affine_rgba(source, opaque_black, fade_position);")
        );
        assert!(!FADE_TO_BLACK_FRAGMENT_SHADER.contains("mix("));
    }

    #[test]
    fn fade_to_black_rejects_noncanonical_native_inputs() {
        assert_eq!(
            validate_fade_to_black_format(TextureFormat::Rgba8Unorm),
            Err(NativeFadeToBlackError::SourceFormat {
                actual: TextureFormat::Rgba8Unorm,
            })
        );
        assert_eq!(
            validate_fade_to_black_format(TextureFormat::Rgba32Float),
            Err(NativeFadeToBlackError::SourceFormat {
                actual: TextureFormat::Rgba32Float,
            })
        );
    }
}
