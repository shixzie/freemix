//! Local graded previews: an RGBA8 image type, a cached preview engine over
//! [`CompiledGrade`], before/after compositing, and a debounced latest-wins
//! recompute scheduler.

use core::time::Duration;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use fm_grade::{
    CompiledGrade, CubeLut, Grade, GradeError, MAX_WHITE_BALANCE, WhiteBalance, srgb_eotf,
};

/// Default number of graded images the engine keeps.
pub const DEFAULT_PREVIEW_CACHE: usize = 12;
/// Default quiet time before a preview recompute.
pub const DEFAULT_RECOMPUTE_DEBOUNCE: Duration = Duration::from_millis(30);
/// Default longest a continuously changing grade waits for a recompute.
pub const DEFAULT_RECOMPUTE_MAX_WAIT: Duration = Duration::from_millis(100);

/// Straight (not premultiplied) packed RGBA8 pixels, row-major from the top.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaImage {
    /// `None` when `pixels` is not exactly `width * height * 4` bytes.
    #[must_use]
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (pixels.len() == expected).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    /// An image filled with one colour.
    #[must_use]
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let count = width as usize * height as usize;
        Self {
            width,
            height,
            pixels: rgba.repeat(count),
        }
    }

    /// An image whose pixel `(x, y)` is `pixel(x, y)`.
    #[must_use]
    pub fn from_fn(width: u32, height: u32, mut pixel: impl FnMut(u32, u32) -> [u8; 4]) -> Self {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// The pixel at `(x, y)`; `None` outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let start = (y as usize * self.width as usize + x as usize) * 4;
        let mut pixel = [0; 4];
        pixel.copy_from_slice(&self.pixels[start..start + 4]);
        Some(pixel)
    }

    /// A box-filtered copy that fits `max_width` x `max_height` keeping the
    /// aspect ratio; an image that already fits is returned as is.
    #[must_use]
    pub fn fit_within(&self, max_width: u32, max_height: u32) -> Self {
        if self.width <= max_width && self.height <= max_height || self.width == 0 {
            return self.clone();
        }
        let scale = (f64::from(max_width) / f64::from(self.width))
            .min(f64::from(max_height) / f64::from(self.height));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = |length: u32| ((f64::from(length) * scale).round() as u32).max(1);
        let (width, height) = (size(self.width), size(self.height));
        Self::from_fn(width, height, |x, y| {
            let x0 = x * self.width / width;
            let x1 = ((x + 1) * self.width / width).max(x0 + 1);
            let y0 = y * self.height / height;
            let y1 = ((y + 1) * self.height / height).max(y0 + 1);
            let mut sum = [0_u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let pixel = self.pixel(sx, sy).unwrap_or_default();
                    for (total, channel) in sum.iter_mut().zip(pixel) {
                        *total += u32::from(channel);
                    }
                }
            }
            let count = (x1 - x0) * (y1 - y0);
            // An average of bytes fits a byte.
            sum.map(|total| u8::try_from((total + count / 2) / count).unwrap_or(u8::MAX))
        })
    }
}

/// 75% colour bars (white, yellow, cyan, green, magenta, red, blue), the
/// reference pattern for scopes.
pub const BARS_75: [[u8; 3]; 7] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
];

/// Seven equal-width 75% colour bars ([`BARS_75`]).
#[must_use]
pub fn bars(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, _| {
        let index = (x as usize * BARS_75.len()) / width.max(1) as usize;
        let [r, g, b] = BARS_75[index.min(BARS_75.len() - 1)];
        [r, g, b, 255]
    })
}

/// A colourful synthetic still for previews when no frame is available: a
/// sky gradient, a warm subject, foliage, and a grey-scale strip, so every
/// grading control has something visible to act on.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
pub fn sample_still(width: u32, height: u32) -> RgbaImage {
    let full_width = width.max(1) as f32;
    let full_height = height.max(1) as f32;
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    RgbaImage::from_fn(width, height, |x, y| {
        let u = (x as f32 + 0.5) / full_width;
        let v = (y as f32 + 0.5) / full_height;
        let rgb = if v > 0.88 {
            // Grey-scale strip, black to white.
            let level = (u * 10.0).floor() / 9.0;
            [level; 3]
        } else if ((u - 0.84) / 0.06).powi(2) + ((v - 0.17) / 0.107).powi(2) < 1.0 {
            // A low yellow sun.
            [1.0, 0.86, 0.42]
        } else if ((u - 0.62) / 0.16).powi(2) + ((v - 0.45) / 0.24).powi(2) < 1.0 {
            // A warm, skin-toned subject lit from the upper left.
            let light = 1.05 - 0.9 * ((u - 0.56).powi(2) + (v - 0.36).powi(2)).sqrt();
            [0.93 * light, 0.68 * light, 0.55 * light]
        } else if (0.1..0.3).contains(&u) && (0.42..0.78).contains(&v) {
            // A red box with a darker side.
            let side = if u > 0.24 { 0.7 } else { 1.0 };
            [0.82 * side, 0.14 * side, 0.12 * side]
        } else if v > 0.62 {
            // Foliage with soft light and shade.
            let texture = 0.07 * (u * 11.0 + v * 4.0).sin() * (v * 9.0 - u * 3.0).cos();
            let depth = (v - 0.62) * 0.6;
            [
                0.16 + texture - depth * 0.2,
                0.44 + texture * 1.4 - depth,
                0.13,
            ]
        } else {
            // Sky: deep blue at the top to a pale horizon.
            let t = v / 0.62;
            [0.20 + 0.55 * t, 0.42 + 0.40 * t, 0.85 - 0.05 * t]
        };
        let [r, g, b] = rgb.map(byte);
        [r, g, b, 255]
    })
}

/// The white balance that makes `image`'s average colour neutral grey
/// ("grey world"), clamped to the model's range; `None` when no pixel is
/// usable. Averages are taken in linear light over pixels that are neither
/// near black nor clipped.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn neutral_white_balance(image: &RgbaImage) -> Option<WhiteBalance> {
    let mut sum = [0.0_f64; 3];
    let mut count = 0_u32;
    for pixel in image.pixels().as_chunks::<4>().0 {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let min = pixel[0].min(pixel[1]).min(pixel[2]);
        if max >= 250 || max <= 8 || min >= 250 {
            continue;
        }
        for (total, &code) in sum.iter_mut().zip(pixel) {
            *total += f64::from(srgb_eotf(f32::from(code) / 255.0));
        }
        count += 1;
    }
    if count == 0 || sum.iter().any(|&total| total <= 0.0) {
        return None;
    }
    let [red, green, blue] = sum.map(f64::log2);
    // Gains 2^(T/2) on red and 2^(-T/2) on blue equalize them when
    // 2^T = B / R; green's 2^(-N/2) then meets their geometric mean.
    let temperature = 1000.0 * (blue - red);
    let tint = 1000.0 * (2.0 * green - red - blue);
    let bound = f64::from(MAX_WHITE_BALANCE);
    // Clamped to the i16 model range before the cast.
    let clamp = |value: f64| value.round().clamp(-bound, bound) as i16;
    Some(WhiteBalance::new(clamp(temperature), clamp(tint)))
}

/// How the preview shows the graded picture against the source.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CompareMode {
    /// Only the graded picture.
    #[default]
    Off,
    /// Source left of the wipe line, graded right of it.
    Wipe,
    /// Source and graded pictures next to each other.
    SideBySide,
}

impl CompareMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::Wipe, Self::SideBySide];
}

/// Source columns left of `position * width` and graded columns from there
/// on. Images of different sizes compose over the smaller one's area.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn compose_wipe(before: &RgbaImage, after: &RgbaImage, position: f32) -> RgbaImage {
    let width = before.width.min(after.width);
    let height = before.height.min(after.height);
    let position = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.5
    };
    // Within 0..=width after the clamp.
    let split = (position * width as f32).round() as u32;
    RgbaImage::from_fn(width, height, |x, y| {
        let image = if x < split { before } else { after };
        image.pixel(x, y).unwrap_or_default()
    })
}

/// `before` and `after` next to each other (source left), each at its own
/// size, on a transparent canvas as tall as the taller one.
#[must_use]
pub fn compose_side_by_side(before: &RgbaImage, after: &RgbaImage) -> RgbaImage {
    let width = before.width + after.width;
    let height = before.height.max(after.height);
    RgbaImage::from_fn(width, height, |x, y| {
        if x < before.width {
            before.pixel(x, y)
        } else {
            after.pixel(x - before.width, y)
        }
        .unwrap_or_default()
    })
}

/// Grades a source image locally with caching.
///
/// Graded results are cached by the full [`Grade`] value (and the source),
/// so returning to an earlier grade, A/B flips, undo, and preset thumbnails
/// cost a lookup instead of a recompute. The last compiled grade is reused
/// when only the source changes. LUT assets are supplied by the caller;
/// a grade whose LUT is not supplied previews without its LUT stage.
#[derive(Clone, Debug)]
pub struct GradePreview {
    source: Option<Arc<RgbaImage>>,
    source_id: u64,
    compiled: Option<CompiledGrade>,
    cache: VecDeque<(Grade, Arc<RgbaImage>)>,
    capacity: usize,
    luts: BTreeMap<String, Arc<CubeLut>>,
    hits: u64,
    misses: u64,
}

impl Default for GradePreview {
    fn default() -> Self {
        Self::new(DEFAULT_PREVIEW_CACHE)
    }
}

impl GradePreview {
    /// An engine caching up to `capacity` graded images (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            source: None,
            source_id: 0,
            compiled: None,
            cache: VecDeque::new(),
            capacity: capacity.max(1),
            luts: BTreeMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Replaces the source picture and drops cached results.
    pub fn set_source(&mut self, image: RgbaImage) {
        self.source = Some(Arc::new(image));
        self.source_id = self.source_id.wrapping_add(1);
        self.cache.clear();
    }

    #[must_use]
    pub fn source(&self) -> Option<&Arc<RgbaImage>> {
        self.source.as_ref()
    }

    /// Changes whenever the source is replaced.
    #[must_use]
    pub const fn source_id(&self) -> u64 {
        self.source_id
    }

    /// Supplies the parsed LUT for `asset_uri` (dropping cached results).
    pub fn set_lut(&mut self, asset_uri: impl Into<String>, lut: Arc<CubeLut>) {
        self.luts.insert(asset_uri.into(), lut);
        self.cache.clear();
        self.compiled = None;
    }

    /// Whether the LUT `asset_uri` was supplied.
    #[must_use]
    pub fn has_lut(&self, asset_uri: &str) -> bool {
        self.luts.contains_key(asset_uri)
    }

    /// The parsed LUT supplied for `asset_uri`.
    #[must_use]
    pub fn lut(&self, asset_uri: &str) -> Option<&Arc<CubeLut>> {
        self.luts.get(asset_uri)
    }

    /// The URIs of every supplied LUT.
    pub fn lut_uris(&self) -> impl Iterator<Item = &str> {
        self.luts.keys().map(String::as_str)
    }

    /// Whether `grade` uses a LUT that was not supplied.
    #[must_use]
    pub fn lut_missing(&self, grade: &Grade) -> bool {
        grade.enabled
            && grade
                .lut
                .as_ref()
                .is_some_and(|lut| lut.strength_permille > 0 && !self.has_lut(&lut.asset_uri))
    }

    /// `(hits, misses)` of the graded-image cache.
    #[must_use]
    pub const fn cache_stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// The source graded by `grade`; `None` without a source.
    ///
    /// # Errors
    ///
    /// The first violated grade bound.
    pub fn graded(&mut self, grade: &Grade) -> Result<Option<Arc<RgbaImage>>, GradeError> {
        let Some(source) = self.source.clone() else {
            return Ok(None);
        };
        if let Some(index) = self.cache.iter().position(|(cached, _)| cached == grade) {
            self.hits += 1;
            // Most recently used at the back.
            let entry = self.cache.remove(index).unwrap_or_else(|| unreachable!());
            let image = Arc::clone(&entry.1);
            self.cache.push_back(entry);
            return Ok(Some(image));
        }
        self.misses += 1;
        let image = if grade.is_identity() {
            source
        } else {
            let compiled = self.compile(grade)?;
            let mut pixels = source.pixels().to_vec();
            compiled.apply_rgba8_in_place(&mut pixels);
            Arc::new(RgbaImage {
                width: source.width,
                height: source.height,
                pixels,
            })
        };
        self.cache.push_back((grade.clone(), Arc::clone(&image)));
        while self.cache.len() > self.capacity {
            self.cache.pop_front();
        }
        Ok(Some(image))
    }

    fn compile(&mut self, grade: &Grade) -> Result<&CompiledGrade, GradeError> {
        if self
            .compiled
            .as_ref()
            .is_none_or(|compiled| compiled.grade() != grade)
        {
            let lut = grade
                .lut
                .as_ref()
                .and_then(|lut| self.luts.get(&lut.asset_uri))
                .cloned();
            self.compiled = Some(CompiledGrade::new(grade, lut)?);
        }
        Ok(self.compiled.as_ref().unwrap_or_else(|| unreachable!()))
    }
}

/// Debounced, latest-wins scheduling of a local recompute keyed by `K` (for
/// example a document generation). Time is a caller-supplied monotonic
/// clock.
///
/// A new key replaces any pending one and restarts the debounce, but a key
/// never waits longer than `max_wait` after the first unserved request, so a
/// continuous drag still refreshes regularly.
#[derive(Clone, Debug)]
pub struct RecomputeScheduler<K> {
    debounce: Duration,
    max_wait: Duration,
    pending: Option<(K, Duration, Duration)>,
    /// The pending key skips the debounce.
    immediate: bool,
    current: Option<K>,
}

impl<K: Copy + Eq> Default for RecomputeScheduler<K> {
    fn default() -> Self {
        Self::new(DEFAULT_RECOMPUTE_DEBOUNCE, DEFAULT_RECOMPUTE_MAX_WAIT)
    }
}

impl<K: Copy + Eq> RecomputeScheduler<K> {
    #[must_use]
    pub const fn new(debounce: Duration, max_wait: Duration) -> Self {
        Self {
            debounce,
            max_wait,
            pending: None,
            immediate: false,
            current: None,
        }
    }

    /// Wants `key` computed. A key already current needs nothing.
    pub fn request(&mut self, key: K, now: Duration) {
        if self.current == Some(key) {
            self.pending = None;
            return;
        }
        if self.pending.is_some_and(|(pending, _, _)| pending == key) {
            return;
        }
        self.pending = match self.pending {
            Some((_, _, first)) => Some((key, now, first)),
            None => Some((key, now, now)),
        };
        self.immediate = false;
    }

    /// Wants `key` computed now, skipping the debounce (for example the
    /// first preview, or after a discrete click).
    pub fn request_now(&mut self, key: K) {
        if self.current == Some(key) {
            self.pending = None;
        } else {
            self.pending = Some((key, Duration::ZERO, Duration::ZERO));
            self.immediate = true;
        }
    }

    /// The key to compute now, if its wait is over. The caller computes it
    /// synchronously; it becomes [`Self::current`].
    pub fn poll(&mut self, now: Duration) -> Option<K> {
        let deadline = self.next_deadline()?;
        if now < deadline {
            return None;
        }
        let (key, _, _) = self.pending.take()?;
        self.immediate = false;
        self.current = Some(key);
        Some(key)
    }

    /// When [`Self::poll`] will next return a key.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.immediate {
            return self.pending.map(|_| Duration::ZERO);
        }
        self.pending.map(|(_, since, first)| {
            since
                .saturating_add(self.debounce)
                .min(first.saturating_add(self.max_wait))
        })
    }

    #[must_use]
    pub fn pending(&self) -> Option<K> {
        self.pending.map(|(key, _, _)| key)
    }

    /// The last key handed out by [`Self::poll`].
    #[must_use]
    pub const fn current(&self) -> Option<K> {
        self.current
    }

    /// Forgets everything (for example after the source changed).
    pub fn reset(&mut self) {
        self.pending = None;
        self.immediate = false;
        self.current = None;
    }
}
