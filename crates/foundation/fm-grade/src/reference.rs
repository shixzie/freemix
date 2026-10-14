//! Direct, uncached reference evaluation of every grading stage.
//!
//! These functions define the evaluation contract documented at the crate
//! root. [`crate::CompiledGrade`] runs the same code paths with prepared
//! tables and samplers, so its output is bit-identical to
//! [`evaluate_pixel`].

use crate::cube::CubeLut;
use crate::model::{
    Contrast, Grade, HueCurve, HueCurves, MAX_CURVE_COORDINATE, MAX_HUE_POSITION,
    MAX_LUMA_POSITION, RgbCurves, Wheel, Wheels, WhiteBalance,
};
use crate::spline::{MonotoneCubic, PeriodicCatmullRom};

/// BT.709 luma weight of red.
pub const LUMA_R: f32 = 0.2126;
/// BT.709 luma weight of green.
pub const LUMA_G: f32 = 0.7152;
/// BT.709 luma weight of blue.
pub const LUMA_B: f32 = 0.0722;

/// Exponent (in stops) per unit of normalized temperature or tint.
pub const WHITE_BALANCE_STOPS_PER_UNIT: f32 = 0.5;

/// BT.709 `Cr → R` coefficient of a wheel direction.
pub const WHEEL_CR_TO_R: f32 = 1.5748;
/// BT.709 `Cb → G` coefficient of a wheel direction.
pub const WHEEL_CB_TO_G: f32 = -0.1873;
/// BT.709 `Cr → G` coefficient of a wheel direction.
pub const WHEEL_CR_TO_G: f32 = -0.4681;
/// BT.709 `Cb → B` coefficient of a wheel direction.
pub const WHEEL_CB_TO_B: f32 = 1.8556;
/// Lift per unit of lift master.
pub const LIFT_MASTER_SCALE: f32 = 0.25;
/// Lift per unit of lift chroma direction.
pub const LIFT_CHROMA_SCALE: f32 = 0.10;
/// Relative gain per unit of gain chroma direction.
pub const GAIN_CHROMA_SCALE: f32 = 0.25;
/// Gamma exponent stops per unit of gamma chroma direction.
pub const GAMMA_CHROMA_SCALE: f32 = 0.25;
/// Offset per unit of offset master.
pub const OFFSET_MASTER_SCALE: f32 = 0.25;
/// Offset per unit of offset chroma direction.
pub const OFFSET_CHROMA_SCALE: f32 = 0.10;

/// Hue rotation, in turns, per unit of hue-vs-hue value (`±1000` → `±180°`).
pub const HUE_SHIFT_TURNS_PER_UNIT: f32 = 0.5;

const PER_MILLE: f32 = 1000.0;
const BASIS_POINTS: f32 = 10_000.0;

/// sRGB EOTF (encoded → linear); the same formula as
/// `fm_color::srgb_to_linear`.
#[must_use]
pub fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB inverse EOTF (linear → encoded); the same formula as
/// `fm_color::srgb_from_linear` for non-negative light. Negative light
/// encodes as `0` and light above `1` extends the power law.
#[must_use]
pub fn srgb_oetf(linear: f32) -> f32 {
    if linear <= 0.0 {
        0.0
    } else if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// Stage 9 quantization: `clamp01`, then `round(v * 255)` with ties away
/// from zero. NaN quantizes to `0`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn quantize(value: f32) -> u8 {
    // In range by construction: the clamp bounds the product to [0, 255].
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Stage 1 normalization.
#[must_use]
pub fn normalize(code: u8) -> f32 {
    f32::from(code) / 255.0
}

#[must_use]
pub fn clamp01(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|value| value.clamp(0.0, 1.0))
}

#[must_use]
pub fn luma(rgb: [f32; 3]) -> f32 {
    LUMA_R * rgb[0] + LUMA_G * rgb[1] + LUMA_B * rgb[2]
}

/// Stage 2: white balance and exposure in linear light.
#[must_use]
pub fn white_balance_exposure(
    rgb: [f32; 3],
    white_balance: WhiteBalance,
    exposure_millistops: i16,
) -> [f32; 3] {
    match linear_gains(white_balance, exposure_millistops) {
        Some(gains) => apply_linear_gains(rgb, gains),
        None => rgb,
    }
}

/// Stage 3: lift, gamma, gain, and offset wheels.
#[must_use]
pub fn wheels(rgb: [f32; 3], wheels: &Wheels) -> [f32; 3] {
    match WheelCoefficients::new(wheels) {
        Some(coefficients) => coefficients.apply(rgb),
        None => rgb,
    }
}

/// Stage 4: linear contrast around the pivot.
#[must_use]
pub fn contrast(rgb: [f32; 3], contrast: Contrast) -> [f32; 3] {
    match contrast_coefficients(contrast) {
        Some(coefficients) => apply_contrast(rgb, coefficients),
        None => rgb,
    }
}

/// Stage 5: saturation around BT.709 luma.
#[must_use]
pub fn saturation(rgb: [f32; 3], saturation_permille: u16) -> [f32; 3] {
    match saturation_factor(saturation_permille) {
        Some(factor) => apply_saturation(rgb, factor),
        None => rgb,
    }
}

/// Stage 6: master then per-channel monotone RGB curves.
#[must_use]
pub fn rgb_curves(rgb: [f32; 3], curves: &RgbCurves) -> [f32; 3] {
    match CurveSamplers::new(curves) {
        Some(samplers) => samplers.apply(rgb),
        None => rgb,
    }
}

/// Stage 7: hue-vs-hue, hue-vs-saturation, and luma-vs-saturation in HSV.
#[must_use]
pub fn hue_curves(rgb: [f32; 3], curves: &HueCurves) -> [f32; 3] {
    match HueSamplers::new(curves) {
        Some(samplers) => samplers.apply(rgb),
        None => rgb,
    }
}

/// Stage 8: LUT sampled with the input clamped to its domain, mixed by
/// `strength_permille / 1000`.
#[must_use]
pub fn lut(rgb: [f32; 3], lut: &CubeLut, strength_permille: u16) -> [f32; 3] {
    if strength_permille == 0 {
        return rgb;
    }
    apply_lut(rgb, lut, f32::from(strength_permille) / PER_MILLE)
}

/// Stages 2–8 followed by `clamp01`, on continuous input in `[0, 1]`.
///
/// A disabled or identity grade returns `clamp01(rgb)`. The LUT stage runs
/// only when `lut` is supplied and the grade references a LUT with nonzero
/// strength.
#[must_use]
pub fn evaluate(grade: &Grade, lut: Option<&CubeLut>, rgb: [f32; 3]) -> [f32; 3] {
    if grade.is_identity() {
        return clamp01(rgb);
    }
    let prepared = Prepared::new(grade);
    clamp01(prepared.secondary(prepared.primary(rgb), active_lut(grade, lut)))
}

/// Grades one straight 8-bit RGB pixel through stages 1–9. A disabled or
/// identity grade returns the input bytes exactly.
#[must_use]
pub fn evaluate_pixel(grade: &Grade, lut: Option<&CubeLut>, rgb: [u8; 3]) -> [u8; 3] {
    if grade.is_identity() {
        return rgb;
    }
    let prepared = Prepared::new(grade);
    let graded = prepared.secondary(prepared.primary(rgb.map(normalize)), active_lut(grade, lut));
    graded.map(quantize)
}

pub(crate) fn active_lut<'a>(
    grade: &Grade,
    lut: Option<&'a CubeLut>,
) -> Option<(&'a CubeLut, f32)> {
    let reference = grade.lut.as_ref()?;
    if reference.strength_permille == 0 {
        return None;
    }
    lut.map(|lut| (lut, f32::from(reference.strength_permille) / PER_MILLE))
}

/// Precomputed coefficients and samplers for every stage of one grade.
#[derive(Clone, Debug)]
pub(crate) struct Prepared {
    gains: Option<[f32; 3]>,
    wheels: Option<WheelCoefficients>,
    contrast: Option<(f32, f32)>,
    saturation: Option<f32>,
    curves: Option<CurveSamplers>,
    hue: Option<HueSamplers>,
}

impl Prepared {
    pub(crate) fn new(grade: &Grade) -> Self {
        Self {
            gains: linear_gains(grade.white_balance, grade.exposure_millistops),
            wheels: WheelCoefficients::new(&grade.wheels),
            contrast: contrast_coefficients(grade.contrast),
            saturation: saturation_factor(grade.saturation_permille),
            curves: CurveSamplers::new(&grade.curves),
            hue: HueSamplers::new(&grade.hue_curves),
        }
    }

    /// Stages 2–4 (each channel independent of the others).
    pub(crate) fn primary(&self, mut rgb: [f32; 3]) -> [f32; 3] {
        if let Some(gains) = self.gains {
            rgb = apply_linear_gains(rgb, gains);
        }
        if let Some(wheels) = &self.wheels {
            rgb = wheels.apply(rgb);
        }
        if let Some(coefficients) = self.contrast {
            rgb = apply_contrast(rgb, coefficients);
        }
        rgb
    }

    /// True when stages 5–7 are all neutral.
    pub(crate) const fn secondary_is_neutral(&self) -> bool {
        self.saturation.is_none() && self.curves.is_none() && self.hue.is_none()
    }

    pub(crate) const fn primary_is_neutral(&self) -> bool {
        self.gains.is_none() && self.wheels.is_none() && self.contrast.is_none()
    }

    /// Stages 5–8.
    pub(crate) fn secondary(&self, mut rgb: [f32; 3], lut: Option<(&CubeLut, f32)>) -> [f32; 3] {
        if let Some(factor) = self.saturation {
            rgb = apply_saturation(rgb, factor);
        }
        if let Some(curves) = &self.curves {
            rgb = curves.apply(rgb);
        }
        if let Some(hue) = &self.hue {
            rgb = hue.apply(rgb);
        }
        if let Some((lut, strength)) = lut {
            rgb = apply_lut(rgb, lut, strength);
        }
        rgb
    }
}

fn linear_gains(white_balance: WhiteBalance, exposure_millistops: i16) -> Option<[f32; 3]> {
    if white_balance.is_neutral() && exposure_millistops == 0 {
        return None;
    }
    let temperature = f32::from(white_balance.temperature) / PER_MILLE;
    let tint = f32::from(white_balance.tint) / PER_MILLE;
    let raw = [
        (WHITE_BALANCE_STOPS_PER_UNIT * temperature).exp2(),
        (-WHITE_BALANCE_STOPS_PER_UNIT * tint).exp2(),
        (-WHITE_BALANCE_STOPS_PER_UNIT * temperature).exp2(),
    ];
    let normalization = luma(raw);
    let exposure = (f32::from(exposure_millistops) / PER_MILLE).exp2();
    Some(raw.map(|gain| gain / normalization * exposure))
}

fn apply_linear_gains(rgb: [f32; 3], gains: [f32; 3]) -> [f32; 3] {
    core::array::from_fn(|channel| srgb_oetf(srgb_eotf(rgb[channel]) * gains[channel]))
}

/// Per-channel RGB contribution of a wheel's `(Cb, Cr)` direction.
fn chroma_direction(wheel: Wheel) -> [f32; 3] {
    let u = f32::from(wheel.x) / PER_MILLE;
    let v = f32::from(wheel.y) / PER_MILLE;
    [
        WHEEL_CR_TO_R * v,
        WHEEL_CB_TO_G * u + WHEEL_CR_TO_G * v,
        WHEEL_CB_TO_B * u,
    ]
}

#[derive(Clone, Copy, Debug)]
struct WheelCoefficients {
    lift: [f32; 3],
    gain: [f32; 3],
    gamma: [f32; 3],
    offset: [f32; 3],
}

impl WheelCoefficients {
    fn new(wheels: &Wheels) -> Option<Self> {
        if wheels.is_neutral() {
            return None;
        }
        let master = |wheel: Wheel| f32::from(wheel.master) / PER_MILLE;
        let lift_direction = chroma_direction(wheels.lift);
        let gain_direction = chroma_direction(wheels.gain);
        let gamma_direction = chroma_direction(wheels.gamma);
        let offset_direction = chroma_direction(wheels.offset);
        let lift_master = master(wheels.lift);
        let gain_master = master(wheels.gain).exp2();
        let gamma_master = master(wheels.gamma);
        let offset_master = master(wheels.offset);
        Some(Self {
            lift: lift_direction.map(|d| LIFT_MASTER_SCALE * lift_master + LIFT_CHROMA_SCALE * d),
            gain: gain_direction.map(|d| gain_master * (1.0 + GAIN_CHROMA_SCALE * d)),
            gamma: gamma_direction.map(|d| (-gamma_master - GAMMA_CHROMA_SCALE * d).exp2()),
            offset: offset_direction
                .map(|d| OFFSET_MASTER_SCALE * offset_master + OFFSET_CHROMA_SCALE * d),
        })
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        core::array::from_fn(|channel| {
            let x = rgb[channel];
            let t = self.gain[channel] * (x + self.lift[channel] * (1.0 - x));
            let y = if t <= 0.0 {
                t
            } else {
                t.powf(self.gamma[channel])
            };
            y + self.offset[channel]
        })
    }
}

fn contrast_coefficients(contrast: Contrast) -> Option<(f32, f32)> {
    if contrast.is_neutral() {
        return None;
    }
    Some((
        1.0 + f32::from(contrast.amount) / PER_MILLE,
        f32::from(contrast.pivot_bp) / BASIS_POINTS,
    ))
}

fn apply_contrast(rgb: [f32; 3], (slope, pivot): (f32, f32)) -> [f32; 3] {
    rgb.map(|value| (value - pivot) * slope + pivot)
}

fn saturation_factor(saturation_permille: u16) -> Option<f32> {
    (saturation_permille != 1000).then(|| f32::from(saturation_permille) / PER_MILLE)
}

fn apply_saturation(rgb: [f32; 3], factor: f32) -> [f32; 3] {
    let y = luma(rgb);
    rgb.map(|value| y + factor * (value - y))
}

#[derive(Clone, Debug)]
struct CurveSamplers {
    master: Option<MonotoneCubic>,
    channels: [Option<MonotoneCubic>; 3],
}

impl CurveSamplers {
    /// `None` when every curve is the identity.
    fn new(curves: &RgbCurves) -> Option<Self> {
        if curves.is_identity() {
            return None;
        }
        Some(Self {
            master: monotone_curve(&curves.master),
            channels: [
                monotone_curve(&curves.red),
                monotone_curve(&curves.green),
                monotone_curve(&curves.blue),
            ],
        })
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        core::array::from_fn(|channel| {
            let mut value = rgb[channel].clamp(0.0, 1.0);
            if let Some(master) = &self.master {
                value = master.evaluate(value);
            }
            if let Some(curve) = &self.channels[channel] {
                value = curve.evaluate(value);
            }
            value
        })
    }
}

fn monotone_curve(curve: &crate::model::Curve) -> Option<MonotoneCubic> {
    if curve.is_identity() {
        return None;
    }
    let scale = f32::from(MAX_CURVE_COORDINATE);
    let knots: Vec<(f32, f32)> = curve
        .points()
        .iter()
        .map(|point| (f32::from(point.x) / scale, f32::from(point.y) / scale))
        .collect();
    MonotoneCubic::new(&knots)
}

#[derive(Clone, Debug)]
struct HueSamplers {
    hue_vs_hue: Option<PeriodicCatmullRom>,
    hue_vs_saturation: Option<PeriodicCatmullRom>,
    luma_vs_saturation: Option<MonotoneCubic>,
}

impl HueSamplers {
    fn new(curves: &HueCurves) -> Option<Self> {
        if curves.is_identity() {
            return None;
        }
        Some(Self {
            hue_vs_hue: periodic_curve(&curves.hue_vs_hue),
            hue_vs_saturation: periodic_curve(&curves.hue_vs_saturation),
            luma_vs_saturation: luma_curve(&curves.luma_vs_saturation),
        })
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = clamp01(rgb);
        let Some((hue, saturation, value)) = rgb_to_hsv(rgb) else {
            return rgb;
        };
        let shift = self.hue_vs_hue.as_ref().map_or(0.0, |curve| {
            curve.evaluate(hue).clamp(-1.0, 1.0) * HUE_SHIFT_TURNS_PER_UNIT
        });
        let hue_multiplier = self.hue_vs_saturation.as_ref().map_or(1.0, |curve| {
            (1.0 + curve.evaluate(hue).clamp(-1.0, 1.0)).max(0.0)
        });
        let luma_multiplier = self.luma_vs_saturation.as_ref().map_or(1.0, |curve| {
            (1.0 + curve.evaluate(luma(rgb)).clamp(-1.0, 1.0)).max(0.0)
        });
        let mut hue = (hue + shift).rem_euclid(1.0);
        if hue >= 1.0 {
            hue = 0.0;
        }
        let saturation = (saturation * hue_multiplier * luma_multiplier).clamp(0.0, 1.0);
        hsv_to_rgb(hue, saturation, value)
    }
}

fn periodic_curve(curve: &HueCurve) -> Option<PeriodicCatmullRom> {
    if curve.is_identity() {
        return None;
    }
    let period = f32::from(MAX_HUE_POSITION) + 1.0;
    let knots: Vec<(f32, f32)> = curve
        .points()
        .iter()
        .map(|point| {
            (
                f32::from(point.position) / period,
                f32::from(point.value) / PER_MILLE,
            )
        })
        .collect();
    PeriodicCatmullRom::new(&knots, 1.0)
}

fn luma_curve(curve: &HueCurve) -> Option<MonotoneCubic> {
    if curve.is_identity() {
        return None;
    }
    let scale = f32::from(MAX_LUMA_POSITION);
    let knots: Vec<(f32, f32)> = curve
        .points()
        .iter()
        .map(|point| {
            (
                f32::from(point.position) / scale,
                f32::from(point.value) / PER_MILLE,
            )
        })
        .collect();
    MonotoneCubic::new(&knots)
}

/// Hue in turns `[0, 1)`, saturation, and value; `None` when `S == 0`.
// `maximum` is bitwise one of the channels, so exact comparison selects it.
#[allow(clippy::float_cmp)]
fn rgb_to_hsv([r, g, b]: [f32; 3]) -> Option<(f32, f32, f32)> {
    let maximum = r.max(g).max(b);
    let minimum = r.min(g).min(b);
    let delta = maximum - minimum;
    if delta <= 0.0 || maximum <= 0.0 {
        return None;
    }
    let sector = if maximum == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if maximum == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    let mut hue = sector / 6.0;
    if hue >= 1.0 {
        hue = 0.0;
    }
    Some((hue, delta / maximum, maximum))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let scaled = hue * 6.0;
    let sector = scaled.floor();
    let fraction = scaled - sector;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));
    // `hue` is in [0, 1), so the sector is 0..=5 (6 only via rounding).
    match (sector as u32) % 6 {
        0 => [value, t, p],
        1 => [q, value, p],
        2 => [p, value, t],
        3 => [p, q, value],
        4 => [t, p, value],
        _ => [value, p, q],
    }
}

fn apply_lut(rgb: [f32; 3], lut: &CubeLut, strength: f32) -> [f32; 3] {
    let sampled = lut.sample(rgb);
    core::array::from_fn(|channel| rgb[channel] + strength * (sampled[channel] - rgb[channel]))
}
