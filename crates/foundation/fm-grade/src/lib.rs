//! Pure, deterministic colour-grading model and CPU reference evaluator.
//!
//! A [`Grade`] is an all-integer, bounded description of a primary colour
//! correction: white balance and exposure, lift/gamma/gain/offset wheels,
//! contrast, saturation, RGB curves, hue/saturation curves, and an optional
//! `.cube` LUT reference. [`CompiledGrade`] turns a validated grade into
//! lookup tables and samplers that evaluate it on straight (not
//! premultiplied) 8-bit RGB without touching alpha.
//!
//! # Evaluation contract
//!
//! Every stage works on `f32` samples. Stages whose parameters are neutral
//! are skipped exactly, and a disabled or identity grade returns the source
//! bytes unchanged.
//!
//! 1. Normalize: `x = v / 255`.
//! 2. White balance and exposure, in linear light: `L = srgb_eotf(x)`
//!    ([`srgb_eotf`]), per-channel gains `g_r = 2^(T/2)`, `g_g = 2^(-N/2)`,
//!    `g_b = 2^(-T/2)` with `T = temperature / 1000` and `N = tint / 1000`,
//!    divided by their BT.709 luma so neutral grey keeps its luminance, then
//!    multiplied by `2^(exposure_millistops / 1000)`; re-encoded with
//!    [`srgb_oetf`] (negative light maps to `0`, light above `1` extends the
//!    power law).
//! 3. Wheels ([`reference::wheels`]): each wheel's `(x, y)` is a `(Cb, Cr)`
//!    direction `u = x / 1000`, `v = y / 1000` whose per-channel RGB
//!    contribution is `d = (1.5748 v, -0.1873 u - 0.4681 v, 1.8556 u)`, and
//!    `m = master / 1000`. With `lift_c = 0.25 m_lift + 0.10 d_lift`,
//!    `gain_c = 2^m_gain (1 + 0.25 d_gain)`, and
//!    `gamma_c = 2^(-m_gamma - 0.25 d_gamma)`:
//!    `t = gain_c (x + lift_c (1 - x))`, `y = t` when `t <= 0` else
//!    `t^gamma_c`, then `y += 0.25 m_offset + 0.10 d_offset`.
//! 4. Contrast: `y = (x - p)(1 + amount / 1000) + p` with
//!    `p = pivot_bp / 10000`.
//! 5. Saturation around BT.709 luma `Y`: `c' = Y + s (c - Y)` with
//!    `s = saturation_permille / 1000`.
//! 6. RGB curves: `c' = curve_c(curve_master(clamp01(c)))`, each curve a
//!    Fritsch–Carlson monotone cubic ([`MonotoneCubic`]).
//! 7. Hue curves in HSV on `clamp01` input (pixels with `S = 0` are left
//!    unchanged): hue-vs-hue rotates hue by `value / 1000 * 180°`,
//!    hue-vs-saturation and luma-vs-saturation multiply saturation by
//!    `max(0, 1 + value / 1000)`; the product is clamped to `[0, 1]`. Hue
//!    curves are periodic Catmull–Rom ([`PeriodicCatmullRom`]) and the luma
//!    curve is Fritsch–Carlson over BT.709 luma.
//! 8. LUT: input clamped to the LUT domain, sampled per channel (1D) or
//!    trilinearly (3D), then `c' = c + strength (lut(c) - c)`.
//! 9. Output: `clamp01`, then `round(v * 255)` with ties away from zero.
//!
//! Stages 1–4 are per channel, so [`CompiledGrade`] stores them as three
//! exact 256-entry tables equal to direct evaluation.

mod compiled;
mod cube;
mod model;
mod presets;
pub mod reference;
mod spline;

pub use compiled::{CompiledGrade, MAX_BAKE_EDGE, MIN_BAKE_EDGE};
pub use cube::{
    CubeError, CubeKind, CubeLut, CubeWarning, MAX_CUBE_1D_SIZE, MAX_CUBE_3D_EDGE,
    MAX_CUBE_INPUT_BYTES, MIN_CUBE_SIZE, parse_cube,
};
pub use model::{
    Contrast, Curve, CurveChannel, CurvePoint, Grade, GradeError, GradeLut, HueCurve, HueCurveKind,
    HueCurves, HuePoint, LUT_URI_PREFIX, MAX_CONTRAST_AMOUNT, MAX_CURVE_COORDINATE,
    MAX_CURVE_POINTS, MAX_EXPOSURE_MILLISTOPS, MAX_HUE_CURVE_POINTS, MAX_HUE_CURVE_VALUE,
    MAX_HUE_POSITION, MAX_LUMA_POSITION, MAX_LUT_STRENGTH_PERMILLE, MAX_LUT_URI_BYTES,
    MAX_PIVOT_BP, MAX_SATURATION_PERMILLE, MAX_WHEEL_MASTER, MAX_WHEEL_RADIUS, MAX_WHITE_BALANCE,
    MIN_CURVE_POINTS, RgbCurves, Wheel, WheelKind, Wheels, WhiteBalance,
};
pub use presets::{BUILTIN_PRESETS, GradePreset, builtin_preset};
pub use reference::{srgb_eotf, srgb_oetf};
pub use spline::{MonotoneCubic, PeriodicCatmullRom};
