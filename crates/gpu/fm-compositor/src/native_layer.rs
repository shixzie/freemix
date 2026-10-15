//! Native GPU layer preparation: per-layer chroma/luma keys, rect masks, and
//! the CPU reference effects ([`ReferenceEffect`]).
//!
//! The CPU reference ([`crate::execute_cpu`]) keys and filters a layer on
//! straight 8-bit sRGB-encoded BT.709 codes after crop. This module mirrors
//! that order with one fullscreen pipeline whose uniform describes a single
//! operation; a layer runs one pass per operation into crop-sized
//! `Rgba16Float` textures:
//!
//! 1. the *key stage* (when the layer has a key, or a mask together with
//!    effects): key alpha (`key_factor`) times rect-mask alpha, chroma spill,
//!    and `alpha = alpha * factor / 255`, all with the CPU's exact integer
//!    formulas;
//! 2. one pass per non-`passthrough` effect, in order, with the exact integer
//!    formula of [`crate::apply_reference_effect`] (`sharpen` reads
//!    edge-clamped neighbours, `flip` mirrors whole pixels including alpha).
//!
//! The first pass reads the canonical working texture (linear BT.2020,
//! premultiplied) at the crop offset and decodes it like the grade pass:
//! un-premultiply, BT.2020 → BT.709, clamp, sRGB OETF, round to integer codes.
//! Intermediate passes store straight codes as `code / 255` (exact through
//! binary16), so chained effects see the same codes the CPU would; the last
//! pass encodes back to the working space (sRGB EOTF, BT.709 → BT.2020, clamp
//! negative light, premultiply).
//!
//! The decode loses the colour of fully transparent source pixels (premultiplied
//! storage holds none), so they read as transparent black; this only differs
//! from a straight-alpha CPU layer where `sharpen` samples such a neighbour.
//! Keys run with straight-alpha semantics on the straight codes (spill on
//! straight colour, only alpha scaled), which equals the CPU straight-alpha
//! path exactly and the premultiplied path up to its premultiplied rounding.

use fm_gpu::{
    NativeContext, NativeFullscreenBlend, NativeFullscreenPipeline,
    NativeFullscreenPipelineOptions, NativeGpuError, NativeSourceExtentPolicy, NativeTexture,
    ShaderDescriptor, ShaderLanguage, ShaderSource, ShaderStage, TextureFormat,
};
use fm_video::CropRect;

use crate::{Effect, Key, NativeCompositionError, PlanLayer, RectMask, ReferenceEffect};

pub(crate) const LAYER_UNIFORM_SIZE: usize = 96;

const OPERATION_KEY: u32 = 0;
const OPERATION_LEVELS: u32 = 1;
const OPERATION_BRIGHTNESS_CONTRAST: u32 = 2;
const OPERATION_SATURATION: u32 = 3;
const OPERATION_SHARPEN: u32 = 4;
const OPERATION_FLIP: u32 = 5;

/// The pass reads the canonical working texture (otherwise straight codes).
const FLAG_DECODE_INPUT: u32 = 1;
/// The pass writes the canonical working texture (otherwise straight codes).
const FLAG_ENCODE_OUTPUT: u32 = 2;

const KEY_NONE: u32 = 0;
const KEY_CHROMA: u32 = 1;
const KEY_LUMA: u32 = 2;

const LAYER_FRAGMENT_SHADER: &str = r"
// Operations: 0 key stage, 1 levels, 2 brightness_contrast, 3 saturation,
// 4 sharpen, 5 flip. Flags: 1 decode working input, 2 encode working output.
struct LayerOperation {
    operation: u32,
    flags: u32,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    parameter_0: u32,
    parameter_1: u32,
    key_kind: u32,
    key_red: u32,
    key_green: u32,
    key_blue: u32,
    key_low: u32,
    key_softness: u32,
    key_spill: u32,
    key_high: u32,
    key_invert: u32,
    mask_enabled: u32,
    mask_invert: u32,
    mask_left: u32,
    mask_top: u32,
    mask_right: u32,
    mask_bottom: u32,
    padding_0: u32,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var unused_source_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> op: LayerOperation;

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

// Premultiplied working texel to straight sRGB-encoded BT.709 8-bit codes.
fn decode_working(texel: vec4<f32>) -> vec4<i32> {
    if texel.a <= 0.0 {
        return vec4<i32>(0);
    }
    let alpha = min(texel.a, 1.0);
    let display = clamp(
        working_to_display(texel.rgb / alpha),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let encoded = vec3<f32>(
        srgb_from_linear(display.r),
        srgb_from_linear(display.g),
        srgb_from_linear(display.b),
    );
    let codes = round(vec4<f32>(encoded, alpha) * 255.0);
    return clamp(vec4<i32>(codes), vec4<i32>(0), vec4<i32>(255));
}

// Straight codes back to a premultiplied working texel.
fn encode_working(codes: vec4<i32>) -> vec4<f32> {
    if codes.a <= 0 {
        return vec4<f32>(0.0);
    }
    let alpha = f32(codes.a) / 255.0;
    let encoded = vec3<f32>(codes.rgb) / 255.0;
    let linear = vec3<f32>(
        srgb_to_linear(encoded.r),
        srgb_to_linear(encoded.g),
        srgb_to_linear(encoded.b),
    );
    let working = max(display_to_working(linear), vec3<f32>(0.0));
    return vec4<f32>(working * alpha, alpha);
}

fn load_codes(pixel: vec2<u32>) -> vec4<i32> {
    let coordinates = pixel + vec2<u32>(op.offset_x, op.offset_y);
    let texel = textureLoad(source_texture, vec2<i32>(coordinates), 0);
    if (op.flags & 1u) != 0u {
        return decode_working(texel);
    }
    return clamp(vec4<i32>(round(texel * 255.0)), vec4<i32>(0), vec4<i32>(255));
}

fn multiply_u8(left: i32, right: i32) -> i32 {
    return (left * right + 127) / 255;
}

// The key ramp over `distance` past the threshold (or below the high bound).
fn ramp(distance: i32, softness: i32) -> i32 {
    if distance <= 0 {
        return 0;
    }
    if softness == 0 || distance >= softness {
        return 255;
    }
    return (distance * 255 + softness / 2) / softness;
}

fn key_factor(pixel: vec4<i32>) -> i32 {
    let softness = i32(op.key_softness);
    let low = i32(op.key_low);
    if op.key_kind == 1u {
        let key = vec3<i32>(i32(op.key_red), i32(op.key_green), i32(op.key_blue));
        let delta = abs(pixel.rgb - key);
        let distance = max(delta.r, max(delta.g, delta.b));
        return ramp(distance - low, softness);
    }
    if op.key_kind == 2u {
        let luma = (54 * pixel.r + 183 * pixel.g + 19 * pixel.b + 128) / 256;
        var factor = ramp(luma - low, softness);
        let high = i32(op.key_high);
        if high < 255 {
            factor = min(factor, ramp(high - luma, softness));
        }
        if op.key_invert != 0u {
            return 255 - factor;
        }
        return factor;
    }
    return 255;
}

fn mask_factor(pixel: vec2<u32>) -> i32 {
    if op.mask_enabled == 0u {
        return 255;
    }
    let inside =
        pixel.x >= op.mask_left &&
        pixel.y >= op.mask_top &&
        pixel.x < op.mask_right &&
        pixel.y < op.mask_bottom;
    if inside == (op.mask_invert != 0u) {
        return 0;
    }
    return 255;
}

fn interpolate(value: i32, neutral: i32, amount: i32) -> i32 {
    return (value * (255 - amount) + neutral * amount + 127) / 255;
}

fn key_stage(pixel: vec2<u32>) -> vec4<i32> {
    var codes = load_codes(pixel);
    let key_alpha = key_factor(codes);
    let factor = multiply_u8(key_alpha, mask_factor(pixel));
    let spill = i32(op.key_spill);
    if op.key_kind == 1u && spill != 0 && key_alpha != 255 {
        let amount = multiply_u8(spill, 255 - key_alpha);
        if op.key_green >= op.key_red && op.key_green >= op.key_blue {
            codes.g = interpolate(codes.g, max(codes.r, codes.b), amount);
        } else if op.key_blue >= op.key_red {
            codes.b = interpolate(codes.b, max(codes.r, codes.g), amount);
        } else {
            codes.r = interpolate(codes.r, max(codes.g, codes.b), amount);
        }
    }
    codes.a = multiply_u8(codes.a, factor);
    return codes;
}

fn clamp_code(value: i32) -> i32 {
    return clamp(value, 0, 255);
}

// `value / divisor` rounded to nearest with halves away from zero (WGSL
// integer division and remainder truncate toward zero, as in Rust).
fn round_div(value: i32, divisor: i32) -> i32 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    let half = divisor / 2;
    if remainder >= half {
        return quotient + 1;
    }
    if remainder <= -half {
        return quotient - 1;
    }
    return quotient;
}

fn levels_channel(input: i32, black: i32, span: i32) -> i32 {
    return clamp_code(((input - black) * 255 + span / 2) / span);
}

fn levels(pixel: vec2<u32>) -> vec4<i32> {
    let codes = load_codes(pixel);
    let black = bitcast<i32>(op.parameter_0);
    let span = bitcast<i32>(op.parameter_1) - black;
    return vec4<i32>(
        levels_channel(codes.r, black, span),
        levels_channel(codes.g, black, span),
        levels_channel(codes.b, black, span),
        codes.a,
    );
}

fn brightness_contrast_channel(input: i32, brightness: i32, contrast: i32) -> i32 {
    return clamp_code(round_div((input - 128) * (100 + contrast), 100) + 128 + brightness);
}

fn brightness_contrast(pixel: vec2<u32>) -> vec4<i32> {
    let codes = load_codes(pixel);
    let brightness = bitcast<i32>(op.parameter_0);
    let contrast = bitcast<i32>(op.parameter_1);
    return vec4<i32>(
        brightness_contrast_channel(codes.r, brightness, contrast),
        brightness_contrast_channel(codes.g, brightness, contrast),
        brightness_contrast_channel(codes.b, brightness, contrast),
        codes.a,
    );
}

fn saturation(pixel: vec2<u32>) -> vec4<i32> {
    let codes = load_codes(pixel);
    let luma = (77 * codes.r + 150 * codes.g + 29 * codes.b + 128) / 256;
    let scale = 100 + bitcast<i32>(op.parameter_0);
    return vec4<i32>(
        clamp_code(luma + round_div((codes.r - luma) * scale, 100)),
        clamp_code(luma + round_div((codes.g - luma) * scale, 100)),
        clamp_code(luma + round_div((codes.b - luma) * scale, 100)),
        codes.a,
    );
}

fn sharpen(pixel: vec2<u32>) -> vec4<i32> {
    let center = load_codes(pixel);
    let up = load_codes(vec2<u32>(pixel.x, max(pixel.y, 1u) - 1u));
    let down = load_codes(vec2<u32>(pixel.x, min(pixel.y + 1u, op.height - 1u)));
    let left = load_codes(vec2<u32>(max(pixel.x, 1u) - 1u, pixel.y));
    let right = load_codes(vec2<u32>(min(pixel.x + 1u, op.width - 1u), pixel.y));
    let blur = (4 * center.rgb + up.rgb + down.rgb + left.rgb + right.rgb) / vec3<i32>(8);
    let amount = bitcast<i32>(op.parameter_0);
    let difference = (center.rgb - blur) * amount;
    return vec4<i32>(
        clamp_code(center.r + round_div(difference.r, 1000)),
        clamp_code(center.g + round_div(difference.g, 1000)),
        clamp_code(center.b + round_div(difference.b, 1000)),
        center.a,
    );
}

fn flip(pixel: vec2<u32>) -> vec4<i32> {
    var mirrored = pixel;
    if op.parameter_0 != 0u {
        mirrored.x = op.width - 1u - pixel.x;
    }
    if op.parameter_1 != 0u {
        mirrored.y = op.height - 1u - pixel.y;
    }
    return load_codes(mirrored);
}

@fragment
fn layer_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(position.xy);
    var codes: vec4<i32>;
    switch op.operation {
        case 0u: {
            codes = key_stage(pixel);
        }
        case 1u: {
            codes = levels(pixel);
        }
        case 2u: {
            codes = brightness_contrast(pixel);
        }
        case 3u: {
            codes = saturation(pixel);
        }
        case 4u: {
            codes = sharpen(pixel);
        }
        default: {
            codes = flip(pixel);
        }
    }
    if (op.flags & 2u) != 0u {
        return encode_working(codes);
    }
    return vec4<f32>(codes) / 255.0;
}
";

/// Parses one layer effect for native execution: `None` for the
/// parameterless `passthrough` no-op, the reference effect otherwise.
///
/// # Errors
///
/// Returns [`NativeCompositionError::UnsupportedEffect`] when the descriptor
/// is not a valid [`ReferenceEffect`].
pub(crate) fn native_effect(
    layer: usize,
    effect_index: usize,
    effect: &Effect,
) -> Result<Option<ReferenceEffect>, NativeCompositionError> {
    if effect.name() == "passthrough" && effect.parameters().is_empty() {
        return Ok(None);
    }
    ReferenceEffect::from_descriptor(effect)
        .map(Some)
        .map_err(|_| NativeCompositionError::UnsupportedEffect {
            layer,
            effect: effect_index,
            name: effect.name().to_owned(),
        })
}

/// The preparation passes of one layer, built before any GPU allocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LayerPreparation {
    width: u32,
    height: u32,
    passes: Vec<[u8; LAYER_UNIFORM_SIZE]>,
}

impl LayerPreparation {
    /// Plans `layer`'s key/mask stage and effect passes over `crop`, or
    /// returns `None` when the layer has neither a key nor a
    /// non-`passthrough` effect (its mask then stays in the composition
    /// pass).
    ///
    /// # Errors
    ///
    /// Returns [`NativeCompositionError::UnsupportedEffect`] for an effect
    /// descriptor that does not parse.
    pub(crate) fn plan(
        layer_index: usize,
        layer: &PlanLayer,
        crop: CropRect,
    ) -> Result<Option<Self>, NativeCompositionError> {
        let mut effects = Vec::with_capacity(layer.effects().len());
        for (effect_index, effect) in layer.effects().iter().enumerate() {
            if let Some(effect) = native_effect(layer_index, effect_index, effect)? {
                effects.push(effect);
            }
        }
        if layer.key().is_none() && effects.is_empty() {
            return Ok(None);
        }
        let mut operations = Vec::with_capacity(effects.len() + 1);
        if layer.key().is_some() || layer.mask().is_some() {
            operations.push(Operation::Key {
                key: layer.key(),
                mask: layer.mask(),
            });
        }
        operations.extend(effects.into_iter().map(Operation::Effect));
        let last = operations.len() - 1;
        let passes = operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| {
                let first = index == 0;
                let mut flags = 0;
                if first {
                    flags |= FLAG_DECODE_INPUT;
                }
                if index == last {
                    flags |= FLAG_ENCODE_OUTPUT;
                }
                let offset = if first { (crop.x, crop.y) } else { (0, 0) };
                encode_layer_uniform(operation, flags, offset, crop.width, crop.height)
            })
            .collect();
        Ok(Some(Self {
            width: crop.width,
            height: crop.height,
            passes,
        }))
    }

    /// Width and height of the prepared texture (the crop size).
    pub(crate) const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Key {
        key: Option<Key>,
        mask: Option<RectMask>,
    },
    Effect(ReferenceEffect),
}

fn encode_layer_uniform(
    operation: Operation,
    flags: u32,
    (offset_x, offset_y): (u32, u32),
    width: u32,
    height: u32,
) -> [u8; LAYER_UNIFORM_SIZE] {
    let mut words = [0_u32; LAYER_UNIFORM_SIZE / 4];
    words[1] = flags;
    words[2] = offset_x;
    words[3] = offset_y;
    words[4] = width;
    words[5] = height;
    let signed = |value: i16| i32::from(value).cast_unsigned();
    match operation {
        Operation::Key { key, mask } => {
            words[0] = OPERATION_KEY;
            match key {
                None => words[8] = KEY_NONE,
                Some(Key::Chroma(chroma)) => {
                    words[8] = KEY_CHROMA;
                    words[9] = u32::from(chroma.color.r);
                    words[10] = u32::from(chroma.color.g);
                    words[11] = u32::from(chroma.color.b);
                    words[12] = u32::from(chroma.tolerance);
                    words[13] = u32::from(chroma.softness);
                    words[14] = u32::from(chroma.spill);
                    words[15] = u32::from(u8::MAX);
                }
                Some(Key::Luma(luma)) => {
                    words[8] = KEY_LUMA;
                    words[12] = u32::from(luma.threshold);
                    words[13] = u32::from(luma.softness);
                    words[15] = u32::from(luma.high);
                    words[16] = u32::from(luma.invert);
                }
            }
            if let Some(mask) = mask {
                words[17] = 1;
                words[18] = u32::from(mask.invert);
                words[19] = mask.x;
                words[20] = mask.y;
                words[21] = mask.x.saturating_add(mask.width);
                words[22] = mask.y.saturating_add(mask.height);
            }
        }
        Operation::Effect(effect) => match effect {
            ReferenceEffect::Levels { black, white } => {
                words[0] = OPERATION_LEVELS;
                words[6] = u32::from(black);
                words[7] = u32::from(white);
            }
            ReferenceEffect::BrightnessContrast {
                brightness,
                contrast,
            } => {
                words[0] = OPERATION_BRIGHTNESS_CONTRAST;
                words[6] = signed(brightness);
                words[7] = signed(contrast);
            }
            ReferenceEffect::Saturation { amount } => {
                words[0] = OPERATION_SATURATION;
                words[6] = signed(amount);
            }
            ReferenceEffect::Sharpen { amount } => {
                words[0] = OPERATION_SHARPEN;
                words[6] = u32::from(amount);
            }
            ReferenceEffect::Flip {
                horizontal,
                vertical,
            } => {
                words[0] = OPERATION_FLIP;
                words[6] = u32::from(horizontal);
                words[7] = u32::from(vertical);
            }
        },
    }
    let mut bytes = [0; LAYER_UNIFORM_SIZE];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Native layer-preparation renderer containing only its compiled pipeline.
pub(crate) struct NativeLayerRenderer {
    pipeline: NativeFullscreenPipeline,
}

impl NativeLayerRenderer {
    /// Compiles the layer-preparation pass.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU shader or pipeline validation error.
    pub(crate) async fn new(context: &NativeContext) -> Result<Self, NativeGpuError> {
        let pipeline = context
            .create_fullscreen_pipeline_with_options(
                ShaderDescriptor::new(
                    "fm-compositor native layer preparation",
                    ShaderStage::Fragment,
                    ShaderLanguage::Wgsl,
                    "layer_fragment",
                    ShaderSource::Text(LAYER_FRAGMENT_SHADER.to_owned()),
                ),
                NativeFullscreenPipelineOptions {
                    target_format: TextureFormat::Rgba16Float,
                    blend: NativeFullscreenBlend::Replace,
                    uniform_size: LAYER_UNIFORM_SIZE,
                    source_extent_policy: NativeSourceExtentPolicy::Independent,
                },
            )
            .await?;
        Ok(Self { pipeline })
    }

    /// Runs `preparation`'s passes over the canonical working `source`,
    /// returning a crop-sized canonical working texture.
    ///
    /// # Errors
    ///
    /// Returns a mapped GPU resource, context, or validation error.
    pub(crate) async fn prepare(
        &self,
        context: &NativeContext,
        source: &NativeTexture,
        preparation: &LayerPreparation,
    ) -> Result<NativeTexture, NativeGpuError> {
        // `plan` never builds an empty pass list; the first pass reads the
        // source, each later pass the previous output.
        let (first, rest) = preparation
            .passes
            .split_first()
            .expect("a layer preparation has at least one pass");
        let mut current = self.pass(context, source, preparation, first).await?;
        for uniform in rest {
            current = self.pass(context, &current, preparation, uniform).await?;
        }
        Ok(current)
    }

    async fn pass(
        &self,
        context: &NativeContext,
        input: &NativeTexture,
        preparation: &LayerPreparation,
        uniform: &[u8; LAYER_UNIFORM_SIZE],
    ) -> Result<NativeTexture, NativeGpuError> {
        let output = context
            .create_rgba16_float_render_target(preparation.width, preparation.height)
            .await?;
        context
            .submit_fullscreen(&self.pipeline, input, input, &output, uniform)
            .await?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChromaKey, LumaKey, OutputTarget, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform,
        compile_scene,
    };

    fn layer(layer: SourceLayer) -> PlanLayer {
        let mut scene = Scene::new(8, 6, Rgba8::new(0, 0, 0, 255)).unwrap();
        scene.push_layer(layer);
        compile_scene(&scene, OutputTarget::Program)
            .unwrap()
            .0
            .layers()[0]
            .clone()
    }

    fn words(uniform: &[u8; LAYER_UNIFORM_SIZE]) -> Vec<u32> {
        uniform
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect()
    }

    fn base() -> SourceLayer {
        SourceLayer::new(
            SourceId::new(1),
            0,
            Transform::new(0, 0, 4, 4, Rotation::Deg0),
        )
    }

    #[test]
    fn plain_masked_and_passthrough_layers_need_no_preparation() {
        let crop = CropRect::new(0, 0, 4, 4);
        for plain in [
            base(),
            base().with_mask(RectMask::new(0, 0, 1, 1)),
            base().with_effect(Effect::passthrough()),
        ] {
            assert_eq!(LayerPreparation::plan(0, &layer(plain), crop), Ok(None));
        }
    }

    #[test]
    fn key_mask_and_effects_become_ordered_passes_with_decode_and_encode_ends() {
        let crop = CropRect::new(2, 3, 5, 4);
        let keyed = layer(
            base()
                .with_crop(crop)
                .with_key(Key::Chroma(ChromaKey::new(
                    Rgba8::new(10, 200, 30, 255),
                    40,
                    20,
                    128,
                )))
                .with_mask(RectMask::new(1, 1, 2, 2).inverted(true))
                .with_effect(Effect::passthrough())
                .with_effect(Effect::new("brightness_contrast", vec![-20, 35]))
                .with_effect(Effect::new("flip", vec![1, 0])),
        );
        let preparation = LayerPreparation::plan(3, &keyed, crop).unwrap().unwrap();
        assert_eq!(preparation.size(), (5, 4));
        assert_eq!(preparation.passes.len(), 3);
        let key = words(&preparation.passes[0]);
        assert_eq!(
            key,
            [
                0, 1, 2, 3, 5, 4, 0, 0, 1, 10, 200, 30, 40, 20, 128, 255, 0, 1, 1, 1, 1, 3, 3, 0
            ]
        );
        let contrast = words(&preparation.passes[1]);
        assert_eq!(
            contrast[..8],
            [2, 0, 0, 0, 5, 4, (-20_i32).cast_unsigned(), 35]
        );
        let flip = words(&preparation.passes[2]);
        assert_eq!(flip[..8], [5, 2, 0, 0, 5, 4, 1, 0]);

        let luma = layer(
            base()
                .with_key(Key::Luma(LumaKey::new(30, 10, true).with_high(200)))
                .with_crop(CropRect::new(0, 0, 4, 4)),
        );
        let preparation = LayerPreparation::plan(0, &luma, CropRect::new(0, 0, 4, 4))
            .unwrap()
            .unwrap();
        let luma = words(&preparation.passes[0]);
        assert_eq!(luma[..2], [0, 3]);
        assert_eq!(luma[8..18], [2, 0, 0, 0, 30, 10, 0, 200, 1, 0]);
    }

    #[test]
    fn invalid_effects_are_rejected_with_their_index() {
        let invalid = layer(
            base()
                .with_effect(Effect::passthrough())
                .with_effect(Effect::new("levels", vec![200, 100])),
        );
        assert_eq!(
            LayerPreparation::plan(4, &invalid, CropRect::new(0, 0, 4, 4)),
            Err(NativeCompositionError::UnsupportedEffect {
                layer: 4,
                effect: 1,
                name: "levels".to_owned(),
            })
        );
    }
}
