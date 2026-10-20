//! One-click automatic chroma key setup (SW-008).
//!
//! [`auto_key`] reads one straight RGBA8 picture of a layer's input (a
//! Studio input thumbnail, or a frame the CLI rendered or read) and derives
//! a whole chroma key from it: the key colour, the similarity (tolerance)
//! and smoothness, the chroma spill, and a green or blue despill. The
//! result is applied by the caller as one ordinary key edit
//! (`scene_layer_key_set`), so it is padlocked, undoable, and journaled
//! like any other key change; nothing here touches the wire or the show.
//!
//! Every step is integer-exact and deterministic: the same picture and
//! request always give the same key.
//!
//! # Sampling
//!
//! A picture of `total` pixels is read in raster order; when `total`
//! exceeds [`MAX_AUTO_KEY_SAMPLES`] only every
//! `ceil(total / MAX_AUTO_KEY_SAMPLES)`-th pixel (starting with the first)
//! is a sample. Alpha is ignored. Each sample's chroma is the full-range
//! BT.709 fixed-point `cb = (32896 - 29 r - 99 g + 128 b) / 256`,
//! `cr = (32896 + 128 r - 116 g - 12 b) / 256` (the compositor's formula),
//! read as the vector `(cb - 128, cr - 128)`; its saturation is the larger
//! absolute component.
//!
//! # The backing seed
//!
//! - [`AutoKeySample::Dominant`] finds the picture's dominant green or blue
//!   background: among samples with `cr < 128` (the green and blue
//!   quadrants) and saturation at least [`MIN_BACKING_SATURATION`], it
//!   counts chroma in 8x8-code bins, takes the fullest bin (ties go to the
//!   smallest `cb` bin, then the smallest `cr` bin), and seeds with the
//!   lower medians of `cb` and `cr` over the candidates in that bin and its
//!   eight neighbours.
//! - [`AutoKeySample::Point`] seeds with the lower medians of `cb` and
//!   `cr` over the `(2 * PICK_RADIUS + 1)` square window around the picked
//!   pixel (clamped to the picture), which must reach
//!   [`MIN_BACKING_SATURATION`]. Any hue may be picked.
//!
//! # The backing cluster
//!
//! A sample belongs to the backing when its chroma points the seed's way
//! however bright or dark it is, which is what keeps an unevenly lit screen
//! one cluster: its saturation is at least [`MIN_CLUSTER_SATURATION`] and
//! at least a quarter of the seed's chroma length (`16 |v|^2 >= |u|^2`),
//! and the angle between the seed `u` and the sample `v` is at most about
//! 20 degrees (`u . v > 0` and `1000 (u . v)^2 >= 883 |u|^2 |v|^2`). A
//! dominant backing must hold at least a tenth of the samples. A point
//! pick whose cluster holds no sample (the subsampling skipped a tiny
//! patch) uses the window's pixels.
//!
//! # The key
//!
//! - The key colour is the per-channel lower median RGB of the cluster.
//! - Distances are the compositor's chroma distance, the largest
//!   per-channel difference from the key colour. The similarity is the
//!   cluster's 95th-percentile distance plus [`SIMILARITY_MARGIN`] (and, for
//!   a point pick, at least the picked colour's distance plus the margin),
//!   at most [`MAX_SIMILARITY`]. The smoothness is the 99th minus the 95th
//!   percentile plus 16, clamped to `16..=64`.
//! - The backing is green when the key colour's green is above its red and
//!   at least its blue, blue when its blue is above both, and otherwise
//!   another colour. A dominant backing is green or blue by construction
//!   (green when green is at least blue).
//! - Spill: for a green or blue backing, a sample outside the cluster is
//!   spill-tinted when its backing channel exceeds `(other + other + 1) / 2`
//!   by at least [`SPILL_TINT_THRESHOLD`]; with `t` tinted of `n` such
//!   samples the strength is `128 + (127 t + n / 2) / n` (`128` when
//!   `n == 0`). The chroma key's spill and a despill of that backing both
//!   take the strength. Another backing gets spill `128` and no despill.

use core::fmt;

use fm_protocol::{WireChromaKey, WireKeyDespill, WireKeyRefinement, WireLayerKey};

/// Most pixels one auto key reads; larger pictures are subsampled.
pub const MAX_AUTO_KEY_SAMPLES: usize = 65_536;
/// Half-size of the square window a point pick reads, in pixels.
pub const PICK_RADIUS: u32 = 2;
/// Least seed saturation (`max(|cb - 128|, |cr - 128|)`) of a backing.
pub const MIN_BACKING_SATURATION: u8 = 24;
/// Least saturation of a sample in the backing cluster.
pub const MIN_CLUSTER_SATURATION: u8 = 12;
/// Added to the 95th-percentile distance to form the similarity.
pub const SIMILARITY_MARGIN: u8 = 8;
/// Largest similarity an auto key sets.
pub const MAX_SIMILARITY: u8 = 128;
/// Least excess of the backing channel that marks a spill-tinted sample.
pub const SPILL_TINT_THRESHOLD: u8 = 8;

/// `cos^2(20 degrees)` in thousandths.
const COS2_THOUSANDTHS: i64 = 883;
/// Chroma bin width of the dominant search, in codes (a power of two).
const BIN_SHIFT: u8 = 3;
const BINS: usize = 256 >> BIN_SHIFT;

/// What the auto key samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoKeySample {
    /// The picture's dominant green or blue background.
    Dominant,
    /// The colour under one picked pixel (eyedropper), in picture pixels.
    Point { x: u32, y: u32 },
}

/// The backing colour family an auto key found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoKeyBacking {
    Green,
    Blue,
    /// A picked colour that is neither green nor blue: keyed, not despilled.
    Other,
}

/// One straight RGBA8 picture, row-major and tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct AutoKeyPicture<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
}

impl<'a> AutoKeyPicture<'a> {
    /// # Errors
    ///
    /// [`AutoKeyError::EmptyPicture`] for a zero side and
    /// [`AutoKeyError::PictureSize`] when `rgba` is not exactly
    /// `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: &'a [u8]) -> Result<Self, AutoKeyError> {
        if width == 0 || height == 0 {
            return Err(AutoKeyError::EmptyPicture);
        }
        let expected = u64::from(width) * u64::from(height) * 4;
        if u64::try_from(rgba.len()).ok() != Some(expected) {
            return Err(AutoKeyError::PictureSize {
                width,
                height,
                bytes: rgba.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let index = (u64::from(y) * u64::from(self.width) + u64::from(x)) * 4;
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        [self.rgba[index], self.rgba[index + 1], self.rgba[index + 2]]
    }
}

/// A derived chroma key and the evidence behind it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoKey {
    pub backing: AutoKeyBacking,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub similarity: u8,
    pub smoothness: u8,
    pub spill: u8,
    pub despill: WireKeyDespill,
    /// Samples read.
    pub samples: u32,
    /// Samples in the backing cluster.
    pub agreeing: u32,
}

impl AutoKey {
    /// The chroma key to apply. A layer's existing clip, choke, and edge
    /// softness (`previous`, whatever its kind) are kept; the despill is
    /// the auto key's.
    #[must_use]
    pub fn chroma_key(&self, previous: Option<WireLayerKey>) -> WireChromaKey {
        let refinement = previous.map_or(WireKeyRefinement::NONE, |key| key.refinement());
        WireChromaKey {
            red: self.red,
            green: self.green,
            blue: self.blue,
            similarity: self.similarity,
            smoothness: self.smoothness,
            spill: self.spill,
            refinement: WireKeyRefinement {
                despill: self.despill,
                ..refinement
            },
        }
    }

    /// [`Self::chroma_key`] as a layer key.
    #[must_use]
    pub fn layer_key(&self, previous: Option<WireLayerKey>) -> WireLayerKey {
        WireLayerKey::Chroma(self.chroma_key(previous))
    }
}

/// Why no key could be derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoKeyError {
    EmptyPicture,
    PictureSize {
        width: u32,
        height: u32,
        bytes: usize,
    },
    PointOutside {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// The picked colour is too grey to key.
    Unsaturated {
        saturation: u8,
    },
    /// No green or blue background covers a tenth of the picture.
    NoBacking {
        agreeing: u32,
        samples: u32,
    },
}

impl fmt::Display for AutoKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPicture => formatter.write_str("the picture is empty"),
            Self::PictureSize {
                width,
                height,
                bytes,
            } => write!(
                formatter,
                "a {width}x{height} RGBA picture cannot hold {bytes} bytes"
            ),
            Self::PointOutside {
                x,
                y,
                width,
                height,
            } => write!(
                formatter,
                "point {x},{y} is outside the {width}x{height} picture"
            ),
            Self::Unsaturated { saturation } => write!(
                formatter,
                "the picked colour is too grey to key (saturation {saturation}, at least \
                 {MIN_BACKING_SATURATION} needed)"
            ),
            Self::NoBacking { agreeing, samples } => write!(
                formatter,
                "no green or blue background found ({agreeing} of {samples} samples)"
            ),
        }
    }
}

impl std::error::Error for AutoKeyError {}

/// Full-range BT.709 fixed-point chroma, as the compositor computes it.
#[must_use]
pub fn chroma(rgb: [u8; 3]) -> (u8, u8) {
    let [r, g, b] = rgb.map(i32::from);
    let cb = (32_896 - 29 * r - 99 * g + 128 * b) / 256;
    let cr = (32_896 + 128 * r - 116 * g - 12 * b) / 256;
    (
        u8::try_from(cb.clamp(0, 255)).unwrap_or(u8::MAX),
        u8::try_from(cr.clamp(0, 255)).unwrap_or(u8::MAX),
    )
}

#[derive(Clone, Copy)]
struct Sample {
    rgb: [u8; 3],
    cb: u8,
    cr: u8,
}

impl Sample {
    fn new(rgb: [u8; 3]) -> Self {
        let (cb, cr) = chroma(rgb);
        Self { rgb, cb, cr }
    }

    fn saturation(self) -> u8 {
        self.cb.abs_diff(128).max(self.cr.abs_diff(128))
    }

    fn vector(self) -> (i64, i64) {
        (i64::from(self.cb) - 128, i64::from(self.cr) - 128)
    }
}

/// Derives a chroma key from `picture` (see the module documentation).
///
/// # Errors
///
/// [`AutoKeyError::PointOutside`] for a pick outside the picture,
/// [`AutoKeyError::Unsaturated`] for a grey pick, and
/// [`AutoKeyError::NoBacking`] when no dominant green or blue background
/// covers a tenth of the samples.
pub fn auto_key(
    picture: AutoKeyPicture<'_>,
    sample: AutoKeySample,
) -> Result<AutoKey, AutoKeyError> {
    let samples = collect(picture);
    let count = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let (seed, window) = match sample {
        AutoKeySample::Dominant => (dominant_seed(&samples, count)?, Vec::new()),
        AutoKeySample::Point { x, y } => {
            if x >= picture.width || y >= picture.height {
                return Err(AutoKeyError::PointOutside {
                    x,
                    y,
                    width: picture.width,
                    height: picture.height,
                });
            }
            let window = pick_window(picture, x, y);
            let seed = (
                lower_median(window.iter().map(|sample| sample.cb)),
                lower_median(window.iter().map(|sample| sample.cr)),
            );
            let saturation = seed.0.abs_diff(128).max(seed.1.abs_diff(128));
            if saturation < MIN_BACKING_SATURATION {
                return Err(AutoKeyError::Unsaturated { saturation });
            }
            (seed, window)
        }
    };
    let seed_vector = (i64::from(seed.0) - 128, i64::from(seed.1) - 128);
    let (mut cluster, outside): (Vec<Sample>, Vec<Sample>) = samples
        .iter()
        .partition(|sample| agrees(seed_vector, **sample));
    let agreeing = u32::try_from(cluster.len()).unwrap_or(u32::MAX);
    if matches!(sample, AutoKeySample::Dominant) && u64::from(agreeing) * 10 < u64::from(count) {
        return Err(AutoKeyError::NoBacking {
            agreeing,
            samples: count,
        });
    }
    if cluster.is_empty() {
        cluster.clone_from(&window);
    }

    let colour = [0, 1, 2].map(|channel| lower_median(cluster.iter().map(|s| s.rgb[channel])));
    let mut distances = [0_u32; 256];
    for sample in &cluster {
        distances[usize::from(distance(sample.rgb, colour))] += 1;
    }
    let total = u32::try_from(cluster.len()).unwrap_or(u32::MAX);
    let p95 = percentile(&distances, total, 95);
    let p99 = percentile(&distances, total, 99);
    let mut similarity = p95.saturating_add(SIMILARITY_MARGIN);
    if !window.is_empty() {
        let picked = [0, 1, 2].map(|channel| lower_median(window.iter().map(|s| s.rgb[channel])));
        similarity = similarity.max(distance(picked, colour).saturating_add(SIMILARITY_MARGIN));
    }
    let similarity = similarity.min(MAX_SIMILARITY);
    let smoothness = p99.saturating_sub(p95).saturating_add(16).clamp(16, 64);

    let [red, green, blue] = colour;
    let backing = match sample {
        AutoKeySample::Dominant if green >= blue => AutoKeyBacking::Green,
        AutoKeySample::Dominant => AutoKeyBacking::Blue,
        AutoKeySample::Point { .. } if green > red && green >= blue => AutoKeyBacking::Green,
        AutoKeySample::Point { .. } if blue > red && blue > green => AutoKeyBacking::Blue,
        AutoKeySample::Point { .. } => AutoKeyBacking::Other,
    };
    let (spill, despill) = match backing {
        AutoKeyBacking::Green => {
            let strength = spill_strength(&outside, 1);
            (strength, WireKeyDespill::Green { strength })
        }
        AutoKeyBacking::Blue => {
            let strength = spill_strength(&outside, 2);
            (strength, WireKeyDespill::Blue { strength })
        }
        AutoKeyBacking::Other => (128, WireKeyDespill::Off),
    };
    Ok(AutoKey {
        backing,
        red,
        green,
        blue,
        similarity,
        smoothness,
        spill,
        despill,
        samples: count,
        agreeing,
    })
}

fn collect(picture: AutoKeyPicture<'_>) -> Vec<Sample> {
    let total = u64::from(picture.width) * u64::from(picture.height);
    let maximum = u64::try_from(MAX_AUTO_KEY_SAMPLES).unwrap_or(u64::MAX);
    let step = usize::try_from(total.div_ceil(maximum).max(1)).unwrap_or(usize::MAX);
    picture
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .step_by(step)
        .map(|pixel| Sample::new([pixel[0], pixel[1], pixel[2]]))
        .collect()
}

fn pick_window(picture: AutoKeyPicture<'_>, x: u32, y: u32) -> Vec<Sample> {
    let rows =
        y.saturating_sub(PICK_RADIUS)..=y.saturating_add(PICK_RADIUS).min(picture.height - 1);
    let columns =
        x.saturating_sub(PICK_RADIUS)..=x.saturating_add(PICK_RADIUS).min(picture.width - 1);
    rows.flat_map(|row| columns.clone().map(move |column| (column, row)))
        .map(|(column, row)| Sample::new(picture.pixel(column, row)))
        .collect()
}

fn dominant_seed(samples: &[Sample], count: u32) -> Result<(u8, u8), AutoKeyError> {
    let candidates = samples
        .iter()
        .filter(|sample| sample.cr < 128 && sample.saturation() >= MIN_BACKING_SATURATION);
    let mut bins = vec![0_u32; BINS * BINS];
    for sample in candidates.clone() {
        bins[bin_index(*sample)] += 1;
    }
    let mut best = 0;
    for (index, bucket) in bins.iter().enumerate() {
        if *bucket > bins[best] {
            best = index;
        }
    }
    if bins[best] == 0 {
        return Err(AutoKeyError::NoBacking {
            agreeing: 0,
            samples: count,
        });
    }
    let (peak_blue, peak_red) = (best / BINS, best % BINS);
    let near = |sample: &&Sample| {
        let index = bin_index(**sample);
        (index / BINS).abs_diff(peak_blue) <= 1 && (index % BINS).abs_diff(peak_red) <= 1
    };
    let neighbourhood: Vec<Sample> = candidates.filter(near).copied().collect();
    Ok((
        lower_median(neighbourhood.iter().map(|sample| sample.cb)),
        lower_median(neighbourhood.iter().map(|sample| sample.cr)),
    ))
}

fn bin_index(sample: Sample) -> usize {
    usize::from(sample.cb >> BIN_SHIFT) * BINS + usize::from(sample.cr >> BIN_SHIFT)
}

fn agrees(seed: (i64, i64), sample: Sample) -> bool {
    if sample.saturation() < MIN_CLUSTER_SATURATION {
        return false;
    }
    let vector = sample.vector();
    let dot = seed.0 * vector.0 + seed.1 * vector.1;
    let seed_length = seed.0 * seed.0 + seed.1 * seed.1;
    let length = vector.0 * vector.0 + vector.1 * vector.1;
    dot > 0
        && 16 * length >= seed_length
        && 1_000 * dot * dot >= COS2_THOUSANDTHS * seed_length * length
}

fn distance(a: [u8; 3], b: [u8; 3]) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

/// The smallest value whose cumulative count reaches `(n + 1) / 2`.
fn lower_median(values: impl Iterator<Item = u8>) -> u8 {
    let mut histogram = [0_u32; 256];
    let mut count = 0_u64;
    for value in values {
        histogram[usize::from(value)] += 1;
        count += 1;
    }
    let target = count.div_ceil(2).max(1);
    let mut cumulative = 0_u64;
    for (value, bucket) in (0..=u8::MAX).zip(histogram) {
        cumulative += u64::from(bucket);
        if cumulative >= target {
            return value;
        }
    }
    u8::MAX
}

/// The smallest distance `d` with at least `percent`% of `total` at most `d`.
fn percentile(histogram: &[u32; 256], total: u32, percent: u64) -> u8 {
    let mut cumulative = 0_u64;
    for (value, bucket) in (0..=u8::MAX).zip(histogram) {
        cumulative += u64::from(*bucket);
        if cumulative * 100 >= u64::from(total) * percent {
            return value;
        }
    }
    u8::MAX
}

/// `128 + (127 t + n / 2) / n` over the `n` samples outside the cluster,
/// `t` of them tinted in `channel` (`1` green, `2` blue).
fn spill_strength(outside: &[Sample], channel: usize) -> u8 {
    let others = [0, 1, 2].into_iter().filter(|other| *other != channel);
    let tinted = outside
        .iter()
        .filter(|sample| {
            let limit = others
                .clone()
                .map(|other| u16::from(sample.rgb[other]))
                .sum::<u16>()
                .div_ceil(2);
            u16::from(sample.rgb[channel]) >= limit + u16::from(SPILL_TINT_THRESHOLD)
        })
        .count();
    let total = outside.len();
    if total == 0 {
        return 128;
    }
    let extra = (127 * tinted + total / 2) / total;
    128_u8.saturating_add(u8::try_from(extra).unwrap_or(127))
}

#[cfg(test)]
mod tests;
