//! Exact colour-bar test signals with defined BT.709 and BT.601 colorimetry.
//!
//! Every pattern is defined first as nominal non-linear R'G'B' levels (1 =
//! 100 %, values may be negative for sub-black PLUGE steps). Levels are exact
//! rationals, and every quantization below uses exact integer arithmetic with
//! round-half-away-from-zero, so the produced code values are identical on
//! every platform.
//!
//! * Narrow-range `Y'CbCr` code values follow the ITU-R BT.709-6 (Part 2, items
//!   3.2–3.5 and 4.6) and ITU-R BT.601-7 (Table 3) derivations:
//!   `E'Y = Kr·R' + (1−Kr−Kb)·G' + Kb·B'`, `E'Cb = (B'−E'Y)/(2(1−Kb))`,
//!   `E'Cr = (R'−E'Y)/(2(1−Kr))`, `DY = round((219·E'Y + 16)·2^(n−8))`,
//!   `DC = round((224·E'C + 128)·2^(n−8))`, with BT.709 `Kr = 0.2126`,
//!   `Kb = 0.0722` and BT.601 `Kr = 0.299`, `Kb = 0.114`. Results are clipped
//!   to the BT.709/BT.601 permitted range (`1..=254` at 8 bits and `4..=1019`
//!   at 10 bits), which keeps timing-reference codes unused.
//! * Full-range R'G'B' 8-bit values are `round(255·E')` clipped to `0..=255`.
//!   This representation cannot carry sub-black or super-white levels, so
//!   negative PLUGE steps render as black there; use the `Y'CbCr` renderer when
//!   the PLUGE must be measurable.
//!
//! Bar colours and levels are those of the standard 75 %, 100 % and EBU
//! 100/0/75/0 bar signals. The geometry of [`BarsPattern::BarsPlugeRamp`] is a
//! `FreeMix` definition (documented on the variant); it is not the SMPTE RP 219
//! raster layout.

use core::fmt;

use fm_types::{
    ChromaLocation, ColorMetadata, ColorPrimaries, MatrixCoefficients, SignalRange,
    TransferFunction,
};

use crate::{FrameError, ImageFrame, Rgba8};

/// Exact nominal R'G'B' levels sharing one positive denominator.
///
/// `r / denominator == 1` is 100 % (reference white for that component).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NominalRgb {
    r: i64,
    g: i64,
    b: i64,
    denominator: i64,
}

impl NominalRgb {
    /// Creates exact levels. Returns `None` when `denominator` is not positive.
    #[must_use]
    pub const fn new(r: i64, g: i64, b: i64, denominator: i64) -> Option<Self> {
        if denominator <= 0 {
            return None;
        }
        Some(Self {
            r,
            g,
            b,
            denominator,
        })
    }

    /// Integer-percent levels.
    #[must_use]
    pub const fn percent(r: i64, g: i64, b: i64) -> Self {
        Self {
            r,
            g,
            b,
            denominator: 100,
        }
    }

    /// An achromatic level of `percent` on every component.
    #[must_use]
    pub const fn grey_percent(percent: i64) -> Self {
        Self::percent(percent, percent, percent)
    }

    #[must_use]
    pub const fn components(self) -> (i64, i64, i64, i64) {
        (self.r, self.g, self.b, self.denominator)
    }
}

/// Colorimetry that defines the luma/colour-difference matrix and tagging.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BarsColorimetry {
    /// ITU-R BT.709-6 (HD).
    Bt709,
    /// ITU-R BT.601-7 (SD).
    Bt601,
}

impl BarsColorimetry {
    /// `(Kr, Kb)` as exact parts per 10 000.
    const fn coefficients(self) -> (i128, i128) {
        match self {
            Self::Bt709 => (2_126, 722),
            Self::Bt601 => (2_990, 1_140),
        }
    }

    /// Color metadata describing the narrow-range `Y'CbCr` output.
    #[must_use]
    pub const fn ycbcr_metadata(self) -> ColorMetadata {
        match self {
            Self::Bt709 => ColorMetadata {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Bt709,
                matrix: MatrixCoefficients::Bt709,
                range: SignalRange::Limited,
                chroma_location: ChromaLocation::Left,
            },
            Self::Bt601 => ColorMetadata {
                primaries: ColorPrimaries::Bt601,
                transfer: TransferFunction::Bt709,
                matrix: MatrixCoefficients::Bt601,
                range: SignalRange::Limited,
                chroma_location: ChromaLocation::Left,
            },
        }
    }

    /// Color metadata describing the full-range R'G'B' output.
    #[must_use]
    pub const fn rgb_metadata(self) -> ColorMetadata {
        let mut metadata = self.ycbcr_metadata();
        metadata.matrix = MatrixCoefficients::Identity;
        metadata.range = SignalRange::Full;
        metadata.chroma_location = ChromaLocation::Center;
        metadata
    }
}

/// Bit depth of narrow-range `Y'CbCr` code values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum YcbcrBitDepth {
    Eight,
    Ten,
}

impl YcbcrBitDepth {
    const fn scale(self) -> i128 {
        match self {
            Self::Eight => 1,
            Self::Ten => 4,
        }
    }

    /// Inclusive permitted code range; reserved timing-reference codes excluded.
    #[must_use]
    pub const fn permitted_range(self) -> (u16, u16) {
        match self {
            Self::Eight => (1, 254),
            Self::Ten => (4, 1_019),
        }
    }
}

/// One narrow-range `Y'CbCr` sample.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct YcbcrCode {
    pub y: u16,
    pub cb: u16,
    pub cr: u16,
}

impl YcbcrCode {
    #[must_use]
    pub const fn new(y: u16, cb: u16, cr: u16) -> Self {
        Self { y, cb, cr }
    }
}

/// Quantizes exact nominal levels to narrow-range `Y'CbCr` code values.
#[must_use]
pub fn quantize_ycbcr(
    level: NominalRgb,
    colorimetry: BarsColorimetry,
    depth: YcbcrBitDepth,
) -> YcbcrCode {
    const UNIT: i128 = 10_000;
    let (kr, kb) = colorimetry.coefficients();
    let kg = UNIT - kr - kb;
    let red = i128::from(level.r);
    let green = i128::from(level.g);
    let blue = i128::from(level.b);
    let denominator = i128::from(level.denominator);
    let scale = depth.scale();
    // E'Y = luma_sum / (UNIT·denominator)
    let luma_sum = kr * red + kg * green + kb * blue;
    let luma = round_ratio(
        (219 * luma_sum + 16 * UNIT * denominator) * scale,
        UNIT * denominator,
    );
    // E'Cb = (UNIT·blue − luma_sum) / (2·denominator·(UNIT − kb)); E'Cr likewise.
    let blue_difference = colour_difference(UNIT * blue - luma_sum, denominator, UNIT - kb, scale);
    let red_difference = colour_difference(UNIT * red - luma_sum, denominator, UNIT - kr, scale);
    let (low, high) = depth.permitted_range();
    YcbcrCode::new(
        clip_code(luma, low, high),
        clip_code(blue_difference, low, high),
        clip_code(red_difference, low, high),
    )
}

/// Quantizes exact nominal levels to opaque full-range 8-bit R'G'B'.
#[must_use]
pub fn quantize_rgb8_full(level: NominalRgb) -> Rgba8 {
    let d = i128::from(level.denominator);
    let channel = |value: i64| -> u8 {
        let code = round_ratio(255 * i128::from(value), d).clamp(0, 255);
        u8::try_from(code).unwrap_or(u8::MAX)
    };
    Rgba8::new(
        channel(level.r),
        channel(level.g),
        channel(level.b),
        u8::MAX,
    )
}

/// `round((224·difference/(2·denominator·weight) + 128)·scale)`.
fn colour_difference(difference: i128, denominator: i128, weight: i128, scale: i128) -> i128 {
    let divisor = 2 * denominator * weight;
    round_ratio((224 * difference + 128 * divisor) * scale, divisor)
}

fn clip_code(value: i128, low: u16, high: u16) -> u16 {
    u16::try_from(value.clamp(i128::from(low), i128::from(high))).unwrap_or(high)
}

/// Rounds `numerator / denominator` half away from zero; `denominator > 0`.
fn round_ratio(numerator: i128, denominator: i128) -> i128 {
    if numerator >= 0 {
        (2 * numerator + denominator) / (2 * denominator)
    } else {
        -((-2 * numerator + denominator) / (2 * denominator))
    }
}

/// Defined test patterns.
///
/// In every band of `n` equal-width columns over `w` pixels, pixel `x` lies in
/// column `⌊x·n/w⌋`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BarsPattern {
    /// Eight equal-width full-height bars at 75/0/75/0: white, yellow, cyan,
    /// green, magenta, red, blue, black.
    Bars75,
    /// Eight equal-width full-height bars at 100/0/100/0.
    Bars100,
    /// EBU 100/0/75/0 bars: 100 % white followed by 75 % yellow, cyan,
    /// green, magenta, red, blue and 0 % black.
    Ebu100_75,
    /// A `FreeMix` three-band layout using standard levels:
    ///
    /// * rows `[0, ⌊2h/3⌋)`: seven equal-width 75 % bars (white, yellow,
    ///   cyan, green, magenta, red, blue);
    /// * rows `[⌊2h/3⌋, ⌊3h/4⌋)`: a grey ramp with column `x` at exactly
    ///   `x / (w − 1)` (0 % at the left edge, 100 % at the right edge; a
    ///   one-pixel-wide frame is 0 %);
    /// * rows `[⌊3h/4⌋, h)`: nine equal-width PLUGE/reference columns at
    ///   0 %, 100 %, 0 %, −2 %, 0 %, +2 %, 0 %, +4 %, 0 %.
    BarsPlugeRamp,
}

const BARS_75: [NominalRgb; 8] = [
    NominalRgb::percent(75, 75, 75),
    NominalRgb::percent(75, 75, 0),
    NominalRgb::percent(0, 75, 75),
    NominalRgb::percent(0, 75, 0),
    NominalRgb::percent(75, 0, 75),
    NominalRgb::percent(75, 0, 0),
    NominalRgb::percent(0, 0, 75),
    NominalRgb::percent(0, 0, 0),
];

const BARS_100: [NominalRgb; 8] = [
    NominalRgb::percent(100, 100, 100),
    NominalRgb::percent(100, 100, 0),
    NominalRgb::percent(0, 100, 100),
    NominalRgb::percent(0, 100, 0),
    NominalRgb::percent(100, 0, 100),
    NominalRgb::percent(100, 0, 0),
    NominalRgb::percent(0, 0, 100),
    NominalRgb::percent(0, 0, 0),
];

const EBU_100_75: [NominalRgb; 8] = [
    NominalRgb::percent(100, 100, 100),
    NominalRgb::percent(75, 75, 0),
    NominalRgb::percent(0, 75, 75),
    NominalRgb::percent(0, 75, 0),
    NominalRgb::percent(75, 0, 75),
    NominalRgb::percent(75, 0, 0),
    NominalRgb::percent(0, 0, 75),
    NominalRgb::percent(0, 0, 0),
];

/// PLUGE/reference columns of [`BarsPattern::BarsPlugeRamp`] in percent.
pub const PLUGE_LEVELS_PERCENT: [i64; 9] = [0, 100, 0, -2, 0, 2, 0, 4, 0];

/// Errors from test-signal rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestSignalError {
    /// Width or height is zero, or the raster exceeds the frame byte bound.
    Frame(FrameError),
    PlaneTooLarge,
}

impl fmt::Display for TestSignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "invalid test-signal raster: {error}"),
            Self::PlaneTooLarge => formatter.write_str("test-signal planes exceed the size bound"),
        }
    }
}

impl std::error::Error for TestSignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::PlaneTooLarge => None,
        }
    }
}

impl From<FrameError> for TestSignalError {
    fn from(value: FrameError) -> Self {
        Self::Frame(value)
    }
}

/// Column `x` of `count` equal columns over `width` pixels:
/// pixel `x` lies in column `⌊x·count/w⌋`, so column `i` covers
/// `[⌈i·w/count⌉, ⌈(i+1)·w/count⌉)`.
fn column_index(x: u32, width: u32, count: usize) -> usize {
    let count = u64::try_from(count).unwrap_or(u64::MAX);
    let index = (u64::from(x) * count) / u64::from(width);
    usize::try_from(index).unwrap_or(usize::MAX)
}

/// Exact nominal level of pattern pixel `(x, y)` in a `width × height` raster.
///
/// Returns `None` for an empty raster or a coordinate outside it.
#[must_use]
pub fn test_signal_level(
    pattern: BarsPattern,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Option<NominalRgb> {
    if width == 0 || height == 0 || x >= width || y >= height {
        return None;
    }
    let bars = match pattern {
        BarsPattern::Bars75 => &BARS_75,
        BarsPattern::Bars100 => &BARS_100,
        BarsPattern::Ebu100_75 => &EBU_100_75,
        BarsPattern::BarsPlugeRamp => {
            let height64 = u64::from(height);
            let y64 = u64::from(y);
            let bars_end = height64 * 2 / 3;
            let ramp_end = height64 * 3 / 4;
            return Some(if y64 < bars_end {
                BARS_75[column_index(x, width, 7)]
            } else if y64 < ramp_end {
                if width == 1 {
                    NominalRgb::grey_percent(0)
                } else {
                    let level = i64::from(x);
                    let denominator = i64::from(width - 1);
                    NominalRgb {
                        r: level,
                        g: level,
                        b: level,
                        denominator,
                    }
                }
            } else {
                NominalRgb::grey_percent(
                    PLUGE_LEVELS_PERCENT[column_index(x, width, PLUGE_LEVELS_PERCENT.len())],
                )
            });
        }
    };
    Some(bars[column_index(x, width, bars.len())])
}

/// A complete pattern definition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TestSignalSpec {
    pub pattern: BarsPattern,
    pub colorimetry: BarsColorimetry,
}

/// Renders a pattern as opaque full-range R'G'B' 8-bit (straight alpha 255).
///
/// The raster's colorimetry is [`BarsColorimetry::rgb_metadata`].
///
/// # Errors
///
/// Returns [`TestSignalError::Frame`] for an invalid or oversized raster.
pub fn render_test_signal_rgba8(
    spec: TestSignalSpec,
    width: u32,
    height: u32,
) -> Result<ImageFrame, TestSignalError> {
    let mut frame = ImageFrame::packed(width, height)?;
    let stride = frame.stride();
    let mut row = vec![0_u8; stride];
    for y in 0..height {
        for (x, pixel) in (0..width).zip(row.as_chunks_mut::<4>().0) {
            let level = test_signal_level(spec.pattern, width, height, x, y)
                .ok_or(TestSignalError::PlaneTooLarge)?;
            *pixel = quantize_rgb8_full(level).to_bytes();
        }
        let start = usize::try_from(y).map_err(|_| TestSignalError::PlaneTooLarge)? * stride;
        frame.pixels_mut()[start..start + stride].copy_from_slice(&row);
    }
    Ok(frame)
}

/// Planar 4:4:4 narrow-range `Y'CbCr` code values, row-major and packed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YcbcrPlanes {
    width: u32,
    height: u32,
    depth: YcbcrBitDepth,
    metadata: ColorMetadata,
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
}

impl YcbcrPlanes {
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn depth(&self) -> YcbcrBitDepth {
        self.depth
    }

    #[must_use]
    pub const fn metadata(&self) -> ColorMetadata {
        self.metadata
    }

    #[must_use]
    pub fn y(&self) -> &[u16] {
        &self.y
    }

    #[must_use]
    pub fn cb(&self) -> &[u16] {
        &self.cb
    }

    #[must_use]
    pub fn cr(&self) -> &[u16] {
        &self.cr
    }

    /// The sample at `(x, y)`, or `None` outside the raster.
    #[must_use]
    pub fn sample(&self, x: u32, y: u32) -> Option<YcbcrCode> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(u64::from(y) * u64::from(self.width) + u64::from(x)).ok()?;
        Some(YcbcrCode::new(
            self.y[index],
            self.cb[index],
            self.cr[index],
        ))
    }
}

/// Renders a pattern as planar 4:4:4 narrow-range `Y'CbCr` code values.
///
/// # Errors
///
/// Returns an error for a zero dimension or when the three planes would
/// exceed [`ImageFrame::MAX_BUFFER_BYTES`] in total.
pub fn render_test_signal_ycbcr444(
    spec: TestSignalSpec,
    width: u32,
    height: u32,
    depth: YcbcrBitDepth,
) -> Result<YcbcrPlanes, TestSignalError> {
    if width == 0 {
        return Err(FrameError::ZeroWidth.into());
    }
    if height == 0 {
        return Err(FrameError::ZeroHeight.into());
    }
    let samples = u64::from(width) * u64::from(height);
    if samples.saturating_mul(6) > u64::try_from(ImageFrame::MAX_BUFFER_BYTES).unwrap_or(u64::MAX) {
        return Err(TestSignalError::PlaneTooLarge);
    }
    let samples = usize::try_from(samples).map_err(|_| TestSignalError::PlaneTooLarge)?;
    let mut planes = YcbcrPlanes {
        width,
        height,
        depth,
        metadata: spec.colorimetry.ycbcr_metadata(),
        y: Vec::with_capacity(samples),
        cb: Vec::with_capacity(samples),
        cr: Vec::with_capacity(samples),
    };
    for y in 0..height {
        for x in 0..width {
            let level = test_signal_level(spec.pattern, width, height, x, y)
                .ok_or(TestSignalError::PlaneTooLarge)?;
            let code = quantize_ycbcr(level, spec.colorimetry, depth);
            planes.y.push(code.y);
            planes.cb.push(code.cb);
            planes.cr.push(code.cr);
        }
    }
    Ok(planes)
}

/// Content of a static generated source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedContent {
    Solid(Rgba8),
    TestSignal(TestSignalSpec),
}

/// Raster and content of a static generated source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeneratedSourceSpec {
    pub width: u32,
    pub height: u32,
    pub content: GeneratedContent,
}

/// Renders a static generated source once per configuration.
///
/// Solid colours and test signals do not change over time, so a source tick
/// returns the shared frame rendered for the current specification and only a
/// specification change renders again. [`Self::renders`] counts renders so
/// callers can assert that steady-state ticks do no per-frame pixel work.
#[derive(Debug, Default)]
pub struct StaticGenerator {
    current: Option<(GeneratedSourceSpec, std::sync::Arc<ImageFrame>)>,
    renders: u64,
}

impl StaticGenerator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: None,
            renders: 0,
        }
    }

    /// Returns the frame for `spec`, rendering only when `spec` changed.
    ///
    /// # Errors
    ///
    /// Returns a raster error; the previously rendered frame is kept.
    pub fn frame(
        &mut self,
        spec: GeneratedSourceSpec,
    ) -> Result<std::sync::Arc<ImageFrame>, TestSignalError> {
        if let Some((current, frame)) = &self.current
            && *current == spec
        {
            return Ok(std::sync::Arc::clone(frame));
        }
        let frame = std::sync::Arc::new(match spec.content {
            GeneratedContent::Solid(color) => {
                crate::generate::solid_color(spec.width, spec.height, color)?
            }
            GeneratedContent::TestSignal(signal) => {
                render_test_signal_rgba8(signal, spec.width, spec.height)?
            }
        });
        self.renders += 1;
        self.current = Some((spec, std::sync::Arc::clone(&frame)));
        Ok(frame)
    }

    /// Number of renders performed since construction.
    #[must_use]
    pub const fn renders(&self) -> u64 {
        self.renders
    }
}
