//! Native GPU colour-grade pass (SW-019).
//!
//! Grades are defined on straight, sRGB-encoded BT.709 colour (see `fm_grade`
//! and the CPU compositor's [`crate::apply_grade`]). Native working textures
//! are linear-light BT.2020, premultiplied `Rgba16Float`, so the pass:
//!
//! 1. un-premultiplies (`alpha == 0` stays transparent black),
//! 2. converts working BT.2020 to linear BT.709 with the same matrix as the
//!    `fm-color` SDR output transform and clamps to `[0, 1]`,
//! 3. applies the sRGB OETF,
//! 4. samples the baked grade ([`CompiledGrade::bake`]) with manual
//!    trilinear interpolation,
//! 5. decodes with the sRGB EOTF and returns to working BT.2020 through the
//!    same BT.709 → XYZ → BT.2020 matrices as `fm-color` import
//!    normalization, clamping negative light to zero,
//! 6. re-premultiplies by the unchanged alpha.
//!
//! The bake is stored as a 2D `Rgba16Float` atlas `(edge * edge) x edge`:
//! texel `(green * edge + red, blue)` holds grid point `(red, green, blue)`,
//! which is exactly the row-major order of [`CompiledGrade::bake`]. Identity
//! grades never reach the GPU.

use core::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use fm_gpu::{
    NativeContext, NativeFullscreenPipeline, NativeFullscreenPipelineOptions, NativeGpuError,
    NativeSourceExtentPolicy, NativeTexture, ShaderDescriptor, ShaderLanguage, ShaderSource,
    ShaderStage, TextureFormat,
};
use fm_grade::{CompiledGrade, MAX_BAKE_EDGE, MIN_BAKE_EDGE};
use half::f16;

/// Default lattice edge of the baked grade atlas.
///
/// The maximum edge keeps trilinear error well inside the 8-bit parity
/// budget for strongly nonlinear grades; the atlas is `4225 x 65` texels.
pub const DEFAULT_NATIVE_GRADE_LUT_EDGE: usize = MAX_BAKE_EDGE;

const GRADE_UNIFORM_SIZE: usize = 16;
const HALF_TEXEL_BYTES: usize = 8;
/// Baked LUTs retained by [`NativeGradeCache`].
const GRADE_CACHE_CAPACITY: usize = 16;

const GRADE_FRAGMENT_SHADER: &str = r"
struct GradeUniform {
    edge: u32,
    padding_0: u32,
    padding_1: u32,
    padding_2: u32,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var lut_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> grade: GradeUniform;

fn srgb_to_linear(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

fn srgb_from_linear(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return 12.92 * linear;
    }
    return 1.055 * pow(linear, 1.0 / 2.4) - 0.055;
}

fn working_to_display(working: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        1.660491 * working.r - 0.587641 * working.g - 0.072850 * working.b,
        -0.124550 * working.r + 1.132900 * working.g - 0.008349 * working.b,
        -0.018151 * working.r - 0.100579 * working.g + 1.118730 * working.b,
    );
}

fn display_to_working(linear: vec3<f32>) -> vec3<f32> {
    let xyz = vec3<f32>(
        0.4123908 * linear.r + 0.35758433 * linear.g + 0.1804808 * linear.b,
        0.212639 * linear.r + 0.71516865 * linear.g + 0.07219232 * linear.b,
        0.01933082 * linear.r + 0.11919478 * linear.g + 0.95053214 * linear.b,
    );
    return vec3<f32>(
        1.7166512 * xyz.r + -0.35567078 * xyz.g + -0.2533663 * xyz.b,
        -0.6666843 * xyz.r + 1.6164812 * xyz.g + 0.01576855 * xyz.b,
        0.01763986 * xyz.r + -0.04277061 * xyz.g + 0.94210315 * xyz.b,
    );
}

fn lut_texel(red: u32, green: u32, blue: u32) -> vec3<f32> {
    let column = green * grade.edge + red;
    return textureLoad(lut_texture, vec2<i32>(i32(column), i32(blue)), 0).rgb;
}

fn sample_lut(encoded: vec3<f32>) -> vec3<f32> {
    let last = grade.edge - 1u;
    let position = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0)) * f32(last);
    let base = min(vec3<u32>(floor(position)), vec3<u32>(last - 1u));
    let t = position - vec3<f32>(base);
    let next = base + vec3<u32>(1u);

    let c000 = lut_texel(base.x, base.y, base.z);
    let c100 = lut_texel(next.x, base.y, base.z);
    let c010 = lut_texel(base.x, next.y, base.z);
    let c110 = lut_texel(next.x, next.y, base.z);
    let c001 = lut_texel(base.x, base.y, next.z);
    let c101 = lut_texel(next.x, base.y, next.z);
    let c011 = lut_texel(base.x, next.y, next.z);
    let c111 = lut_texel(next.x, next.y, next.z);

    let c00 = c000 + (c100 - c000) * t.x;
    let c10 = c010 + (c110 - c010) * t.x;
    let c01 = c001 + (c101 - c001) * t.x;
    let c11 = c011 + (c111 - c011) * t.x;
    let c0 = c00 + (c10 - c00) * t.y;
    let c1 = c01 + (c11 - c01) * t.y;
    return c0 + (c1 - c0) * t.z;
}

@fragment
fn grade_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let source = textureLoad(source_texture, vec2<i32>(position.xy), 0);
    if source.a <= 0.0 {
        return vec4<f32>(0.0);
    }
    let alpha = min(source.a, 1.0);
    let display = clamp(
        working_to_display(source.rgb / alpha),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let encoded = vec3<f32>(
        srgb_from_linear(display.r),
        srgb_from_linear(display.g),
        srgb_from_linear(display.b),
    );
    let graded = clamp(sample_lut(encoded), vec3<f32>(0.0), vec3<f32>(1.0));
    let linear = vec3<f32>(
        srgb_to_linear(graded.r),
        srgb_to_linear(graded.g),
        srgb_to_linear(graded.b),
    );
    let working = max(display_to_working(linear), vec3<f32>(0.0));
    return vec4<f32>(working * alpha, alpha);
}
";

/// Errors produced by the native grade pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeGradeError {
    /// The source is not a canonical `Rgba16Float` working texture.
    SourceFormat {
        actual: TextureFormat,
    },
    Gpu(NativeGpuError),
}

impl fmt::Display for NativeGradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceFormat { actual } => write!(
                formatter,
                "grade source has native format {actual:?}; expected Rgba16Float"
            ),
            Self::Gpu(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NativeGradeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gpu(error) => Some(error),
            Self::SourceFormat { .. } => None,
        }
    }
}

impl From<NativeGpuError> for NativeGradeError {
    fn from(value: NativeGpuError) -> Self {
        Self::Gpu(value)
    }
}

/// A non-identity grade baked into a GPU-resident `Rgba16Float` atlas.
///
/// Bound to the [`NativeContext`] that uploaded it; reuse it across frames
/// for as long as the grade is unchanged.
pub struct NativeGradeLut {
    texture: NativeTexture,
    edge: u32,
}

impl NativeGradeLut {
    /// Lattice points per axis.
    #[must_use]
    pub const fn edge(&self) -> u32 {
        self.edge
    }

    /// The `(edge * edge) x edge` atlas texture.
    #[must_use]
    pub const fn texture(&self) -> &NativeTexture {
        &self.texture
    }
}

impl fmt::Debug for NativeGradeLut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeGradeLut")
            .field("edge", &self.edge)
            .finish_non_exhaustive()
    }
}

/// Native colour-grade renderer containing only its compiled pipeline.
pub struct NativeGradeRenderer {
    pipeline: NativeFullscreenPipeline,
    edge: usize,
}

impl NativeGradeRenderer {
    /// Compiles the grade pass with [`DEFAULT_NATIVE_GRADE_LUT_EDGE`].
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn new(context: &NativeContext) -> Result<Self, NativeGradeError> {
        Self::with_lut_edge(context, DEFAULT_NATIVE_GRADE_LUT_EDGE).await
    }

    /// Compiles the grade pass baking grades on an `edge`³ lattice; `edge`
    /// is clamped to `MIN_BAKE_EDGE..=MAX_BAKE_EDGE` (`2..=65`).
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub async fn with_lut_edge(
        context: &NativeContext,
        edge: usize,
    ) -> Result<Self, NativeGradeError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native grade",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "grade_fragment",
                    ShaderSource::Text(GRADE_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: fm_gpu::NativeFullscreenBlend::Replace,
                    uniform_size: GRADE_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::Independent,
                },
            )
            .await?;
        Ok(Self {
            pipeline,
            edge: edge.clamp(MIN_BAKE_EDGE, MAX_BAKE_EDGE),
        })
    }

    /// Lattice points per axis used by [`Self::prepare`].
    #[must_use]
    pub const fn lut_edge(&self) -> usize {
        self.edge
    }

    /// Bakes and uploads `grade`, or returns `None` for an identity grade,
    /// which must leave pixels untouched.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU upload error.
    pub async fn prepare(
        &self,
        context: &NativeContext,
        grade: &CompiledGrade,
    ) -> Result<Option<NativeGradeLut>, NativeGradeError> {
        if grade.is_identity() {
            return Ok(None);
        }
        let edge = self.edge;
        let atlas = bake_atlas(grade, edge);
        // Edges are at most 65, so the atlas is at most 4225 x 65.
        let width = u32::try_from(edge * edge).map_err(|_| NativeGpuError::TextureSizeOverflow)?;
        let height = u32::try_from(edge).map_err(|_| NativeGpuError::TextureSizeOverflow)?;
        let texture = context
            .upload_rgba16_float(width, height, edge * edge * HALF_TEXEL_BYTES, &atlas)
            .await?;
        Ok(Some(NativeGradeLut {
            texture,
            edge: height,
        }))
    }

    /// Grades a canonical working texture with a prepared LUT into a new
    /// GPU-resident `Rgba16Float` texture of the same size. Alpha is kept.
    ///
    /// # Errors
    ///
    /// Returns a source-format error before allocation, or a mapped GPU
    /// resource, context, or validation error.
    pub async fn apply(
        &self,
        context: &NativeContext,
        lut: &NativeGradeLut,
        source: &NativeTexture,
    ) -> Result<NativeTexture, NativeGradeError> {
        validate_grade_source(source.format())?;
        let output = context
            .create_rgba16_float_render_target(source.width(), source.height())
            .await?;
        context
            .submit_fullscreen(
                &self.pipeline,
                source,
                &lut.texture,
                &output,
                &encode_grade_uniform(lut.edge),
            )
            .await?;
        Ok(output)
    }

    /// Grades a whole canonical working frame (for example an input or the
    /// Program output). Returns `None` for an identity grade: the source is
    /// then the exact result and no GPU work is submitted.
    ///
    /// This bakes the grade on every call; hold a [`NativeGradeLut`] from
    /// [`Self::prepare`] to reuse it across frames.
    ///
    /// # Errors
    ///
    /// Returns a source-format error before any GPU work, or a mapped GPU
    /// error.
    pub async fn grade_frame(
        &self,
        context: &NativeContext,
        grade: &CompiledGrade,
        source: &NativeTexture,
    ) -> Result<Option<NativeTexture>, NativeGradeError> {
        validate_grade_source(source.format())?;
        let Some(lut) = self.prepare(context, grade).await? else {
            return Ok(None);
        };
        self.apply(context, &lut, source).await.map(Some)
    }
}

/// A small bounded cache of baked LUTs keyed by grade equality, so per-frame
/// composition does not re-bake unchanged grades.
#[derive(Default)]
pub(crate) struct NativeGradeCache {
    entries: Mutex<Vec<(Arc<CompiledGrade>, Arc<NativeGradeLut>)>>,
}

impl NativeGradeCache {
    /// Returns the cached LUT for `grade`, baking and uploading on a miss.
    pub(crate) async fn lut(
        &self,
        renderer: &NativeGradeRenderer,
        context: &NativeContext,
        grade: &Arc<CompiledGrade>,
    ) -> Result<Option<Arc<NativeGradeLut>>, NativeGradeError> {
        if grade.is_identity() {
            return Ok(None);
        }
        if let Some(lut) = self.find(grade) {
            return Ok(Some(lut));
        }
        let Some(lut) = renderer.prepare(context, grade).await? else {
            return Ok(None);
        };
        let lut = Arc::new(lut);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() == GRADE_CACHE_CAPACITY {
            entries.remove(0);
        }
        entries.push((grade.clone(), lut.clone()));
        Ok(Some(lut))
    }

    fn find(&self, grade: &Arc<CompiledGrade>) -> Option<Arc<NativeGradeLut>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let index = entries
            .iter()
            .position(|(cached, _)| Arc::ptr_eq(cached, grade) || **cached == **grade)?;
        // Move the hit to the most-recently-used end.
        let entry = entries.remove(index);
        let lut = entry.1.clone();
        entries.push(entry);
        Some(lut)
    }
}

fn validate_grade_source(format: TextureFormat) -> Result<(), NativeGradeError> {
    if format == TextureFormat::Rgba16Float {
        Ok(())
    } else {
        Err(NativeGradeError::SourceFormat { actual: format })
    }
}

fn encode_grade_uniform(edge: u32) -> [u8; GRADE_UNIFORM_SIZE] {
    let mut bytes = [0; GRADE_UNIFORM_SIZE];
    bytes[..4].copy_from_slice(&edge.to_le_bytes());
    bytes
}

/// Packs [`CompiledGrade::bake`] as little-endian binary16 RGBA texels with
/// opaque alpha, in atlas row-major order.
fn bake_atlas(grade: &CompiledGrade, edge: usize) -> Vec<u8> {
    let grid = grade.bake(edge);
    let mut bytes = Vec::with_capacity(grid.len() * HALF_TEXEL_BYTES);
    for [red, green, blue] in grid {
        for component in [red, green, blue, 1.0] {
            bytes.extend_from_slice(&f16::from_f32(component).to_le_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use fm_grade::Grade;

    use super::*;

    #[test]
    fn grade_uniform_carries_the_edge_in_the_first_word() {
        let uniform = encode_grade_uniform(65);
        assert_eq!(u32::from_le_bytes(uniform[..4].try_into().unwrap()), 65);
        assert!(uniform[4..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn atlas_is_the_bake_in_row_major_half_float_order() {
        let grade = CompiledGrade::new(
            &Grade {
                saturation_permille: 0,
                exposure_millistops: 500,
                ..Grade::IDENTITY
            },
            None,
        )
        .unwrap();
        let edge = 5;
        let atlas = bake_atlas(&grade, edge);
        assert_eq!(atlas.len(), edge * edge * edge * HALF_TEXEL_BYTES);
        let grid = grade.bake(edge);
        for (red, green, blue) in [(0, 0, 0), (4, 1, 2), (2, 3, 4), (4, 4, 4)] {
            // Texel (green * edge + red, blue) in a row of edge * edge texels.
            let texel = blue * edge * edge + green * edge + red;
            let offset = texel * HALF_TEXEL_BYTES;
            let component = |index: usize| {
                let at = offset + index * 2;
                f16::from_le_bytes([atlas[at], atlas[at + 1]]).to_f32()
            };
            let expected = grid[(blue * edge + green) * edge + red];
            for (channel, value) in expected.into_iter().enumerate() {
                assert_eq!(component(channel), f16::from_f32(value).to_f32());
            }
            assert_eq!(component(3), 1.0);
        }
    }

    #[test]
    fn only_canonical_working_textures_are_graded() {
        assert_eq!(validate_grade_source(TextureFormat::Rgba16Float), Ok(()));
        assert_eq!(
            validate_grade_source(TextureFormat::Rgba8Unorm),
            Err(NativeGradeError::SourceFormat {
                actual: TextureFormat::Rgba8Unorm,
            })
        );
    }
}
