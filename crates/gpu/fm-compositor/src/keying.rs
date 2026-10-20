//! Deterministic CPU-reference keying extensions: auto key sampling, exact
//! green/blue despill, matte clip/choke refinement, and key/fill layers.
//!
//! Every formula here is integer-exact so that the CPU reference can serve as
//! a pixel oracle for later native implementations.

use core::fmt;

use fm_video::{CropRect, FrameError, ImageFrame, Rgba8};

use crate::cpu::{apply_spill, key_factor, mask_factor, multiply_u8, straight_key_color};
use crate::{AlphaMode, ChromaKey, Key, LumaKey, RectMask, SourceId};

/// Maximum number of pixels an auto key reads from one frame. Larger regions
/// are subsampled deterministically (see [`sample_auto_key`]).
pub const MAX_AUTO_KEY_SAMPLES: usize = 65_536;

/// Maximum absolute matte choke/spread radius in pixels.
pub const MAX_CHOKE_RADIUS: u8 = 8;

/// Maximum matte edge-softness (box blur) radius in pixels.
pub const MAX_EDGE_SOFTNESS: u8 = 8;

/// The backing color family recognized by the auto key and despill.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backing {
    Green,
    Blue,
}

/// Exact spill suppression for a green or blue backing.
///
/// For a green backing `limit = (r + b + 1) / 2` and
/// `excess = g.saturating_sub(limit)`; the output green is
/// `g - (excess * strength + 127) / 255`. A blue backing uses
/// `limit = (r + g + 1) / 2` and reduces blue the same way. Red, the other
/// channel, and alpha are never modified, and a channel is only ever lowered,
/// so premultiplied input stays premultiplied. `strength == 0` is a no-op;
/// `strength == 255` clamps the dominant channel to the limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Despill {
    pub backing: Backing,
    pub strength: u8,
}

impl Despill {
    #[must_use]
    pub const fn new(backing: Backing, strength: u8) -> Self {
        Self { backing, strength }
    }

    /// Applies the documented despill formula to one pixel.
    #[must_use]
    pub fn apply(self, pixel: Rgba8) -> Rgba8 {
        let suppress = |dominant: u8, first: u8, second: u8| {
            let limit = (u16::from(first) + u16::from(second)).div_ceil(2);
            let excess = u16::from(dominant).saturating_sub(limit);
            let reduction = (excess * u16::from(self.strength) + 127) / 255;
            u8::try_from(u16::from(dominant) - reduction).unwrap_or(dominant)
        };
        let mut output = pixel;
        match self.backing {
            Backing::Green => output.g = suppress(pixel.g, pixel.r, pixel.b),
            Backing::Blue => output.b = suppress(pixel.b, pixel.r, pixel.g),
        }
        output
    }
}

/// Manual matte controls applied to a layer's key alpha before the mask.
///
/// Order per layer: the raw key alpha (chroma, luma, or key/fill matte) is
/// clipped, then choked, then softened, then multiplied with the rectangular
/// (garbage) mask.
///
/// Clip: alpha `<= clip_black` becomes `0`, alpha `>= clip_white` becomes
/// `255`, and otherwise
/// `((alpha - clip_black) * 255 + (clip_white - clip_black) / 2) / (clip_white - clip_black)`.
/// The identity is `clip_black = 0, clip_white = 255`.
///
/// Choke: a positive value erodes the matte with the minimum over a
/// `(2 * choke + 1)` square window; a negative value spreads it with the
/// maximum over a `(2 * |choke| + 1)` window. Windows are clamped to the
/// (cropped) layer frame, so out-of-frame pixels never contribute.
///
/// Edge softness: a nonzero radius `r` replaces each matte value with the
/// rounded mean `(sum + count / 2) / count` of the choked matte over the
/// `(2 * r + 1)` square window clamped to the layer frame (`count` is the
/// number of in-frame pixels in the window).
///
/// Chroma spill suppression from [`ChromaKey::spill`] uses the refined key
/// alpha. The optional [`Despill`] is applied to every pixel's color before
/// alpha scaling, independent of the matte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyRefinement {
    pub clip_black: u8,
    pub clip_white: u8,
    pub choke: i8,
    pub edge_softness: u8,
    pub despill: Option<Despill>,
}

impl Default for KeyRefinement {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRefinement {
    /// The identity refinement: no clip, no choke, no softness, no despill.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            clip_black: 0,
            clip_white: u8::MAX,
            choke: 0,
            edge_softness: 0,
            despill: None,
        }
    }

    #[must_use]
    pub const fn with_clip(mut self, clip_black: u8, clip_white: u8) -> Self {
        self.clip_black = clip_black;
        self.clip_white = clip_white;
        self
    }

    #[must_use]
    pub const fn with_choke(mut self, choke: i8) -> Self {
        self.choke = choke;
        self
    }

    #[must_use]
    pub const fn with_edge_softness(mut self, radius: u8) -> Self {
        self.edge_softness = radius;
        self
    }

    #[must_use]
    pub const fn with_despill(mut self, despill: Despill) -> Self {
        self.despill = Some(despill);
        self
    }

    /// # Errors
    /// Returns an error when the clip range is empty, the choke radius
    /// exceeds [`MAX_CHOKE_RADIUS`], or the edge softness exceeds
    /// [`MAX_EDGE_SOFTNESS`].
    pub fn validate(&self) -> Result<(), KeyRefinementError> {
        if self.clip_black >= self.clip_white {
            return Err(KeyRefinementError::EmptyClipRange {
                clip_black: self.clip_black,
                clip_white: self.clip_white,
            });
        }
        if self.choke.unsigned_abs() > MAX_CHOKE_RADIUS {
            return Err(KeyRefinementError::ChokeOutOfRange {
                choke: self.choke,
                maximum: MAX_CHOKE_RADIUS,
            });
        }
        if self.edge_softness > MAX_EDGE_SOFTNESS {
            return Err(KeyRefinementError::EdgeSoftnessOutOfRange {
                radius: self.edge_softness,
                maximum: MAX_EDGE_SOFTNESS,
            });
        }
        Ok(())
    }

    pub(crate) fn clip(self, alpha: u8) -> u8 {
        if alpha <= self.clip_black {
            return 0;
        }
        if alpha >= self.clip_white {
            return u8::MAX;
        }
        let range = u16::from(self.clip_white - self.clip_black);
        let offset = u16::from(alpha - self.clip_black);
        u8::try_from((offset * 255 + range / 2) / range).unwrap_or(u8::MAX)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyRefinementError {
    EmptyClipRange { clip_black: u8, clip_white: u8 },
    ChokeOutOfRange { choke: i8, maximum: u8 },
    EdgeSoftnessOutOfRange { radius: u8, maximum: u8 },
}

impl fmt::Display for KeyRefinementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClipRange {
                clip_black,
                clip_white,
            } => write!(
                formatter,
                "clip black {clip_black} must be below clip white {clip_white}"
            ),
            Self::ChokeOutOfRange { choke, maximum } => {
                write!(formatter, "choke {choke} exceeds radius {maximum}")
            }
            Self::EdgeSoftnessOutOfRange { radius, maximum } => {
                write!(formatter, "edge softness {radius} exceeds radius {maximum}")
            }
        }
    }
}

impl std::error::Error for KeyRefinementError {}

/// Which channel of a key/fill key source drives the matte.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeyChannel {
    /// The key pixel's BT.709 fixed-point luma (a classic linear key).
    #[default]
    Luma,
    /// The key pixel's stored alpha (a key source carrying an alpha channel).
    Alpha,
}

/// A key/fill pair: the layer's own source is the fill and the alpha comes
/// from the luma (or alpha) of `key_source`.
///
/// The key source frame must have exactly the fill frame's dimensions; the
/// layer crop is applied identically to both. With [`KeyChannel::Luma`] the
/// key source's own alpha is ignored and its RGB is read as stored; with
/// [`KeyChannel::Alpha`] only its stored alpha is read. The matte is
/// [`LumaKey`]'s ramp (threshold and softness act as clip and gain, `high`
/// as an upper bound, `invert` inverts) of the key pixel's BT.709
/// fixed-point luma or of its alpha;
/// [`KeyFill::linear`] uses `threshold = 0, softness = 255`, which maps luma
/// to alpha exactly (`alpha == luma`). With straight fill alpha only the
/// alpha channel is scaled; with premultiplied fill all four channels are
/// scaled by `(channel * matte + 127) / 255`, so the result stays
/// premultiplied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyFill {
    pub key_source: SourceId,
    pub matte: LumaKey,
    pub channel: KeyChannel,
}

impl KeyFill {
    #[must_use]
    pub const fn new(key_source: SourceId, matte: LumaKey) -> Self {
        Self {
            key_source,
            matte,
            channel: KeyChannel::Luma,
        }
    }

    /// The same key/fill reading `channel` of the key source.
    #[must_use]
    pub const fn with_channel(mut self, channel: KeyChannel) -> Self {
        self.channel = channel;
        self
    }

    /// The matte value of one key source pixel.
    pub(crate) fn matte_alpha(self, pixel: Rgba8) -> u8 {
        let pixel = match self.channel {
            KeyChannel::Luma => pixel,
            // A gray pixel's fixed-point luma is exactly its level.
            KeyChannel::Alpha => Rgba8::new(pixel.a, pixel.a, pixel.a, u8::MAX),
        };
        key_factor(pixel, Key::Luma(self.matte))
    }

    /// Linear key: alpha equals the key source luma.
    #[must_use]
    pub const fn linear(key_source: SourceId) -> Self {
        Self::new(key_source, LumaKey::new(0, u8::MAX, false))
    }
}

/// The pixels an auto key samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoKeyRegion {
    /// Every pixel within `thickness` pixels of any frame edge, visited row by
    /// row, left to right.
    Border { thickness: u32 },
    /// Every pixel in a rectangle that must lie inside the frame, visited row
    /// by row, left to right.
    Rect(CropRect),
}

/// Parameters for [`sample_auto_key`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoKeyOptions {
    pub region: AutoKeyRegion,
    /// Maximum Chebyshev Cb/Cr distance from the median for a sample to agree.
    pub agreement_radius: u8,
    /// Minimum `max(|Cb - 128|, |Cr - 128|)` of the median backing.
    pub min_saturation: u8,
    /// Added (saturating) to the 95th-percentile RGB distance to form tolerance.
    pub tolerance_margin: u8,
    pub softness: u8,
    pub spill: u8,
    pub despill_strength: u8,
}

impl AutoKeyOptions {
    /// Options with `agreement_radius = 24`, `min_saturation = 24`,
    /// `tolerance_margin = 8`, `softness = 32`, `spill = 0`, and
    /// `despill_strength = 255`.
    #[must_use]
    pub const fn new(region: AutoKeyRegion) -> Self {
        Self {
            region,
            agreement_radius: 24,
            min_saturation: 24,
            tolerance_margin: 8,
            softness: 32,
            spill: 0,
            despill_strength: u8::MAX,
        }
    }
}

/// The result of an auto key: manual chroma key parameters plus evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoKey {
    pub backing: Backing,
    /// Per-channel lower median RGB of the agreeing samples, alpha `255`.
    pub color: Rgba8,
    pub median_cb: u8,
    pub median_cr: u8,
    pub samples: u32,
    pub agreeing: u32,
    pub key: ChromaKey,
    pub despill: Despill,
}

impl AutoKey {
    /// The manual layer key produced by this auto key.
    #[must_use]
    pub const fn layer_key(&self) -> Key {
        Key::Chroma(self.key)
    }

    /// A refinement carrying the auto key's despill and no clip or choke.
    #[must_use]
    pub const fn refinement(&self) -> KeyRefinement {
        KeyRefinement::new().with_despill(self.despill)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoKeyError {
    EmptyRegion,
    RegionOutOfBounds {
        region: CropRect,
        width: u32,
        height: u32,
    },
    NoConsensus {
        agreeing: u32,
        samples: u32,
    },
    Unsaturated {
        saturation: u8,
        minimum: u8,
    },
    UnsupportedBacking {
        cb: u8,
        cr: u8,
    },
}

impl fmt::Display for AutoKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRegion => formatter.write_str("auto key sample region is empty"),
            Self::RegionOutOfBounds {
                region,
                width,
                height,
            } => write!(
                formatter,
                "auto key region {}x{} at {},{} exceeds the {width}x{height} frame",
                region.width, region.height, region.x, region.y
            ),
            Self::NoConsensus { agreeing, samples } => write!(
                formatter,
                "only {agreeing} of {samples} auto key samples agree with the median backing"
            ),
            Self::Unsaturated {
                saturation,
                minimum,
            } => write!(
                formatter,
                "backing chroma saturation {saturation} is below {minimum}"
            ),
            Self::UnsupportedBacking { cb, cr } => write!(
                formatter,
                "median backing Cb {cb} Cr {cr} is neither green nor blue"
            ),
        }
    }
}

impl std::error::Error for AutoKeyError {}

/// Full-range BT.709 fixed-point chroma of an RGB pixel:
/// `cb = (32896 - 29 r - 99 g + 128 b) / 256` and
/// `cr = (32896 + 128 r - 116 g - 12 b) / 256`, clamped to `255`.
#[must_use]
pub fn chroma_cb_cr(pixel: Rgba8) -> (u8, u8) {
    let r = i32::from(pixel.r);
    let g = i32::from(pixel.g);
    let b = i32::from(pixel.b);
    let cb = (32_896 - 29 * r - 99 * g + 128 * b) / 256;
    let cr = (32_896 + 128 * r - 116 * g - 12 * b) / 256;
    (
        u8::try_from(cb.clamp(0, 255)).unwrap_or(u8::MAX),
        u8::try_from(cr.clamp(0, 255)).unwrap_or(u8::MAX),
    )
}

/// Samples a backing color and derives manual chroma key parameters.
///
/// The region holds `total` pixels; when `total` exceeds
/// [`MAX_AUTO_KEY_SAMPLES`] only every `ceil(total / MAX_AUTO_KEY_SAMPLES)`-th
/// pixel in visit order (starting with the first) is read. Pixel alpha is
/// ignored. The median Cb and Cr are lower medians (the smallest value whose
/// cumulative count reaches `(n + 1) / 2`). A sample agrees when both its Cb
/// and Cr are within `agreement_radius` of the medians; more than half of the
/// samples must agree. The median backing must reach `min_saturation` and is
/// green when `cb < 128 && cr < 128`, blue when `cb > 128 && cr < 128`, and
/// rejected otherwise. The key color is the per-channel lower median RGB of
/// the agreeing samples. Tolerance is the smallest max-channel RGB distance
/// `d` from the key color such that at least 95% of agreeing samples are
/// within `d`, plus `tolerance_margin` (saturating).
///
/// # Errors
/// Returns a typed error for an empty or out-of-frame region, no majority
/// consensus, an unsaturated backing, or a backing that is not green or blue.
pub fn sample_auto_key(
    frame: &ImageFrame,
    options: &AutoKeyOptions,
) -> Result<AutoKey, AutoKeyError> {
    let samples = collect_samples(frame, options.region)?;
    let count = u32::try_from(samples.len()).unwrap_or(u32::MAX);

    let mut blue_difference_counts = [0_u32; 256];
    let mut red_difference_counts = [0_u32; 256];
    for &(_, cb, cr) in &samples {
        blue_difference_counts[usize::from(cb)] += 1;
        red_difference_counts[usize::from(cr)] += 1;
    }
    let blue_difference = lower_median(&blue_difference_counts, count);
    let red_difference = lower_median(&red_difference_counts, count);

    let radius = options.agreement_radius;
    let agreeing_samples = samples
        .iter()
        .filter(|(_, cb, cr)| {
            cb.abs_diff(blue_difference) <= radius && cr.abs_diff(red_difference) <= radius
        })
        .map(|(pixel, _, _)| *pixel)
        .collect::<Vec<_>>();
    let agreeing = u32::try_from(agreeing_samples.len()).unwrap_or(u32::MAX);
    if u64::from(agreeing) * 2 <= u64::from(count) {
        return Err(AutoKeyError::NoConsensus {
            agreeing,
            samples: count,
        });
    }

    let saturation = blue_difference
        .abs_diff(128)
        .max(red_difference.abs_diff(128));
    if saturation < options.min_saturation {
        return Err(AutoKeyError::Unsaturated {
            saturation,
            minimum: options.min_saturation,
        });
    }
    let backing = match (blue_difference.cmp(&128), red_difference < 128) {
        (core::cmp::Ordering::Less, true) => Backing::Green,
        (core::cmp::Ordering::Greater, true) => Backing::Blue,
        _ => {
            return Err(AutoKeyError::UnsupportedBacking {
                cb: blue_difference,
                cr: red_difference,
            });
        }
    };

    let mut red = [0_u32; 256];
    let mut green = [0_u32; 256];
    let mut blue = [0_u32; 256];
    for pixel in &agreeing_samples {
        red[usize::from(pixel.r)] += 1;
        green[usize::from(pixel.g)] += 1;
        blue[usize::from(pixel.b)] += 1;
    }
    let color = Rgba8::new(
        lower_median(&red, agreeing),
        lower_median(&green, agreeing),
        lower_median(&blue, agreeing),
        u8::MAX,
    );

    let mut distances = [0_u32; 256];
    for pixel in &agreeing_samples {
        let distance = pixel
            .r
            .abs_diff(color.r)
            .max(pixel.g.abs_diff(color.g))
            .max(pixel.b.abs_diff(color.b));
        distances[usize::from(distance)] += 1;
    }
    let mut cumulative = 0_u64;
    let mut percentile = u8::MAX;
    for (distance, bucket) in (0..=u8::MAX).zip(distances) {
        cumulative += u64::from(bucket);
        if cumulative * 100 >= u64::from(agreeing) * 95 {
            percentile = distance;
            break;
        }
    }
    let tolerance = percentile.saturating_add(options.tolerance_margin);

    Ok(AutoKey {
        backing,
        color,
        median_cb: blue_difference,
        median_cr: red_difference,
        samples: count,
        agreeing,
        key: ChromaKey::new(color, tolerance, options.softness, options.spill),
        despill: Despill::new(backing, options.despill_strength),
    })
}

fn lower_median(histogram: &[u32; 256], count: u32) -> u8 {
    let target = u64::from(count).div_ceil(2).max(1);
    let mut cumulative = 0_u64;
    for (value, bucket) in (0..=u8::MAX).zip(histogram) {
        cumulative += u64::from(*bucket);
        if cumulative >= target {
            return value;
        }
    }
    u8::MAX
}

fn collect_samples(
    frame: &ImageFrame,
    region: AutoKeyRegion,
) -> Result<Vec<(Rgba8, u8, u8)>, AutoKeyError> {
    let width = frame.width();
    let height = frame.height();
    // Row spans (y, x_start, x_end) in visit order.
    let mut spans: Vec<(u32, u32, u32)> = Vec::new();
    match region {
        AutoKeyRegion::Border { thickness } => {
            if thickness == 0 || width == 0 || height == 0 {
                return Err(AutoKeyError::EmptyRegion);
            }
            let vertical = thickness.min(height);
            let horizontal = thickness.min(width);
            for y in 0..height {
                let band = y < vertical || y >= height - vertical;
                if band || horizontal.saturating_mul(2) >= width {
                    spans.push((y, 0, width));
                } else {
                    spans.push((y, 0, horizontal));
                    spans.push((y, width - horizontal, width));
                }
            }
        }
        AutoKeyRegion::Rect(rect) => {
            if rect.width == 0 || rect.height == 0 {
                return Err(AutoKeyError::EmptyRegion);
            }
            let inside = rect.x.checked_add(rect.width).is_some_and(|r| r <= width)
                && rect.y.checked_add(rect.height).is_some_and(|b| b <= height);
            if !inside {
                return Err(AutoKeyError::RegionOutOfBounds {
                    region: rect,
                    width,
                    height,
                });
            }
            for y in rect.y..rect.y + rect.height {
                spans.push((y, rect.x, rect.x + rect.width));
            }
        }
    }
    let total = spans
        .iter()
        .map(|(_, start, end)| u64::from(end - start))
        .sum::<u64>();
    if total == 0 {
        return Err(AutoKeyError::EmptyRegion);
    }
    let maximum = u64::try_from(MAX_AUTO_KEY_SAMPLES).unwrap_or(u64::MAX);
    let step = total.div_ceil(maximum).max(1);
    let mut samples = Vec::with_capacity(usize::try_from(total.min(maximum)).unwrap_or(0));
    let mut ordinal = 0_u64;
    for (y, start, end) in spans {
        for x in start..end {
            if ordinal.is_multiple_of(step)
                && let Some(pixel) = frame.pixel(x, y)
            {
                let (cb, cr) = chroma_cb_cr(pixel);
                samples.push((pixel, cb, cr));
            }
            ordinal += 1;
        }
    }
    Ok(samples)
}

/// Applies the key/fill or refined key stage of one CPU layer.
///
/// `key_fill` carries the already-cropped key source and its [`KeyFill`] when
/// the layer has one; it must match `source` dimensions (checked by the
/// caller).
pub(crate) fn apply_layer_keying(
    source: &ImageFrame,
    key_fill: Option<(&ImageFrame, KeyFill)>,
    key: Option<Key>,
    refinement: KeyRefinement,
    mask: Option<RectMask>,
    alpha_mode: AlphaMode,
) -> Result<ImageFrame, FrameError> {
    let width = source.width();
    let height = source.height();
    let width_usize = usize::try_from(width).map_err(|_| FrameError::LayoutOverflow)?;
    let height_usize = usize::try_from(height).map_err(|_| FrameError::LayoutOverflow)?;
    let area = width_usize
        .checked_mul(height_usize)
        .ok_or(FrameError::LayoutOverflow)?;

    let mut matte = Vec::with_capacity(area);
    for y in 0..height {
        for x in 0..width {
            let alpha = if let Some((key_frame, fill)) = key_fill {
                let pixel = key_frame.pixel(x, y).ok_or(FrameError::LayoutOverflow)?;
                fill.matte_alpha(pixel)
            } else {
                let pixel = source.pixel(x, y).ok_or(FrameError::LayoutOverflow)?;
                key.map_or(u8::MAX, |key| {
                    key_factor(straight_key_color(pixel, alpha_mode), key)
                })
            };
            matte.push(refinement.clip(alpha));
        }
    }
    if refinement.choke != 0 {
        matte = choke_matte(&matte, width_usize, height_usize, refinement.choke);
    }
    if refinement.edge_softness != 0 {
        matte = soften_matte(&matte, width_usize, height_usize, refinement.edge_softness);
    }

    let mut bytes = Vec::with_capacity(area * 4);
    for (index, alpha) in matte.into_iter().enumerate() {
        let x = u32::try_from(index % width_usize).map_err(|_| FrameError::LayoutOverflow)?;
        let y = u32::try_from(index / width_usize).map_err(|_| FrameError::LayoutOverflow)?;
        let pixel = source.pixel(x, y).ok_or(FrameError::LayoutOverflow)?;
        let mut adjusted = if key_fill.is_some() {
            pixel
        } else {
            apply_spill(pixel, key, alpha)
        };
        if let Some(despill) = refinement.despill {
            adjusted = despill.apply(adjusted);
        }
        let mask_alpha = mask.map_or(u8::MAX, |mask| mask_factor(x, y, mask));
        let factor = multiply_u8(alpha, mask_alpha);
        match alpha_mode {
            AlphaMode::Straight => adjusted.a = multiply_u8(adjusted.a, factor),
            AlphaMode::Premultiplied => {
                adjusted.r = multiply_u8(adjusted.r, factor);
                adjusted.g = multiply_u8(adjusted.g, factor);
                adjusted.b = multiply_u8(adjusted.b, factor);
                adjusted.a = multiply_u8(adjusted.a, factor);
            }
        }
        bytes.extend_from_slice(&adjusted.to_bytes());
    }
    ImageFrame::new(width, height, width_usize * 4, bytes)
}

/// Separable square min (choke > 0) or max (choke < 0) filter.
fn choke_matte(matte: &[u8], width: usize, height: usize, choke: i8) -> Vec<u8> {
    let radius = usize::from(choke.unsigned_abs());
    let erode = choke > 0;
    let pick = |values: &mut dyn Iterator<Item = u8>| {
        if erode {
            values.min().unwrap_or(0)
        } else {
            values.max().unwrap_or(0)
        }
    };
    let mut horizontal = vec![0_u8; matte.len()];
    for y in 0..height {
        let row = &matte[y * width..(y + 1) * width];
        for x in 0..width {
            let start = x.saturating_sub(radius);
            let end = (x + radius + 1).min(width);
            horizontal[y * width + x] = pick(&mut row[start..end].iter().copied());
        }
    }
    let mut output = vec![0_u8; matte.len()];
    for y in 0..height {
        let start = y.saturating_sub(radius);
        let end = (y + radius + 1).min(height);
        for x in 0..width {
            output[y * width + x] = pick(&mut (start..end).map(|row| horizontal[row * width + x]));
        }
    }
    output
}

/// Square box mean over windows clamped to the frame, rounded to nearest.
fn soften_matte(matte: &[u8], width: usize, height: usize, radius: u8) -> Vec<u8> {
    let radius = usize::from(radius);
    let mut horizontal = vec![0_u32; matte.len()];
    for y in 0..height {
        let row = &matte[y * width..(y + 1) * width];
        for x in 0..width {
            let start = x.saturating_sub(radius);
            let end = (x + radius + 1).min(width);
            horizontal[y * width + x] = row[start..end].iter().map(|&value| u32::from(value)).sum();
        }
    }
    let mut output = vec![0_u8; matte.len()];
    for y in 0..height {
        let rows = y.saturating_sub(radius)..(y + radius + 1).min(height);
        let row_count = rows.len();
        for x in 0..width {
            let columns = (x + radius + 1).min(width) - x.saturating_sub(radius);
            let count = u32::try_from(row_count * columns).unwrap_or(u32::MAX);
            let sum: u32 = rows.clone().map(|row| horizontal[row * width + x]).sum();
            output[y * width + x] = u8::try_from((sum + count / 2) / count).unwrap_or(u8::MAX);
        }
    }
    output
}
