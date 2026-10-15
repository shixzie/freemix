//! Video scopes computed from an RGBA8 frame as small numeric grids the UI
//! paints: luma waveform, RGB parade, BT.709 vectorscope, and histogram.
//!
//! Values are taken from the 8-bit code values as they are (gamma-encoded
//! `R'G'B'`), as on a hardware scope. Luma is BT.709
//! `Y' = 0.2126 R' + 0.7152 G' + 0.0722 B'`, and the vectorscope plots
//! `Cb = (B' - Y') / 1.8556` against `Cr = (R' - Y') / 1.5748`, both in
//! `-0.5..=0.5`.

use fm_grade::reference::{LUMA_B, LUMA_G, LUMA_R, WHEEL_CB_TO_B, WHEEL_CR_TO_R};

use super::preview::RgbaImage;

/// One scope display.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ScopeKind {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

impl ScopeKind {
    pub const ALL: [Self; 4] = [
        Self::Waveform,
        Self::Parade,
        Self::Vectorscope,
        Self::Histogram,
    ];
}

/// Scope resolution. Larger frames are sampled on a grid of at most
/// `max_samples_x` x `max_samples_y` pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopeConfig {
    /// Waveform and parade columns.
    pub columns: usize,
    /// Waveform and parade level rows (black at row `0`).
    pub levels: usize,
    /// Vectorscope grid edge.
    pub vectorscope: usize,
    /// Histogram bins.
    pub bins: usize,
    pub max_samples_x: usize,
    pub max_samples_y: usize,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            columns: 128,
            levels: 64,
            vectorscope: 64,
            bins: 64,
            max_samples_x: 256,
            max_samples_y: 144,
        }
    }
}

/// A `width` x `height` grid of hit counts, row `0` at the bottom.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScopeGrid {
    width: usize,
    height: usize,
    cells: Vec<u32>,
}

impl ScopeGrid {
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![0; width * height],
        }
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Row-major cells, row `0` (the bottom) first.
    #[must_use]
    pub fn cells(&self) -> &[u32] {
        &self.cells
    }

    /// The count at column `x`, row `y` (`0` outside the grid).
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> u32 {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x]
        } else {
            0
        }
    }

    /// The largest count.
    #[must_use]
    pub fn max(&self) -> u32 {
        self.cells.iter().copied().max().unwrap_or(0)
    }

    /// Total hits.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.cells.iter().map(|&count| u64::from(count)).sum()
    }

    fn add(&mut self, x: usize, y: usize) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] += 1;
        }
    }
}

/// Per-channel histograms of `bins` buckets over `0..=255`.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Histogram {
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
}

impl Histogram {
    /// The largest bucket over every channel.
    #[must_use]
    pub fn max(&self) -> u32 {
        [&self.red, &self.green, &self.blue, &self.luma]
            .into_iter()
            .flat_map(|bins| bins.iter().copied())
            .max()
            .unwrap_or(0)
    }
}

/// Every scope of one frame.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Scopes {
    /// Luma level per column.
    pub waveform: ScopeGrid,
    /// Red, green, and blue levels per column.
    pub parade: [ScopeGrid; 3],
    /// `Cb` across (left `-0.5`), `Cr` up (bottom `-0.5`).
    pub vectorscope: ScopeGrid,
    pub histogram: Histogram,
    /// Pixels sampled.
    pub samples: usize,
}

/// BT.709 luma of normalized `R'G'B'`.
#[must_use]
pub fn luma(rgb: [f32; 3]) -> f32 {
    LUMA_R * rgb[0] + LUMA_G * rgb[1] + LUMA_B * rgb[2]
}

/// BT.709 `(Cb, Cr)` of normalized `R'G'B'`, each in `-0.5..=0.5`.
#[must_use]
pub fn chroma(rgb: [f32; 3]) -> [f32; 2] {
    let y = luma(rgb);
    [(rgb[2] - y) / WHEEL_CB_TO_B, (rgb[0] - y) / WHEEL_CR_TO_R]
}

/// The vectorscope cell `(column, row)` of `(Cb, Cr)` on a `size` grid.
#[must_use]
pub fn vectorscope_cell(cb_cr: [f32; 2], size: usize) -> (usize, usize) {
    let [cb, cr] = cb_cr;
    (bucket(cb + 0.5, size), bucket(cr + 0.5, size))
}

/// The bucket of normalized `value` among `count` equal buckets.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn bucket(value: f32, count: usize) -> usize {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((value * count as f32) as usize).min(count.saturating_sub(1))
}

/// Computes every scope of `image`.
#[must_use]
pub fn compute_scopes(image: &RgbaImage, config: ScopeConfig) -> Scopes {
    let columns = config.columns.max(1);
    let levels = config.levels.max(1);
    let bins = config.bins.max(1);
    let mut scopes = Scopes {
        waveform: ScopeGrid::new(columns, levels),
        parade: core::array::from_fn(|_| ScopeGrid::new(columns, levels)),
        vectorscope: ScopeGrid::new(config.vectorscope.max(1), config.vectorscope.max(1)),
        histogram: Histogram {
            red: vec![0; bins],
            green: vec![0; bins],
            blue: vec![0; bins],
            luma: vec![0; bins],
        },
        samples: 0,
    };
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return scopes;
    }
    let step_x = width.div_ceil(config.max_samples_x.max(1));
    let step_y = height.div_ceil(config.max_samples_y.max(1));
    let pixels = image.pixels();
    for y in (0..height).step_by(step_y) {
        for x in (0..width).step_by(step_x) {
            let start = (y * width + x) * 4;
            let rgb = [pixels[start], pixels[start + 1], pixels[start + 2]]
                .map(|code| f32::from(code) / 255.0);
            let column = x * columns / width;
            let y_level = luma(rgb);
            scopes.waveform.add(column, bucket(y_level, levels));
            for (grid, value) in scopes.parade.iter_mut().zip(rgb) {
                grid.add(column, bucket(value, levels));
            }
            let (cx, cy) = vectorscope_cell(chroma(rgb), config.vectorscope.max(1));
            scopes.vectorscope.add(cx, cy);
            let histogram = &mut scopes.histogram;
            histogram.red[bucket(rgb[0], bins)] += 1;
            histogram.green[bucket(rgb[1], bins)] += 1;
            histogram.blue[bucket(rgb[2], bins)] += 1;
            histogram.luma[bucket(y_level, bins)] += 1;
            scopes.samples += 1;
        }
    }
    scopes
}
