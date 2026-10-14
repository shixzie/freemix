use core::fmt;
use std::borrow::Cow;

/// Inclusive magnitude bound for white-balance temperature and tint.
pub const MAX_WHITE_BALANCE: i16 = 1000;
/// Inclusive magnitude bound for exposure in thousandths of a stop.
pub const MAX_EXPOSURE_MILLISTOPS: i16 = 4000;
/// Inclusive magnitude bound for the contrast amount (per mille).
pub const MAX_CONTRAST_AMOUNT: i16 = 1000;
/// Inclusive upper bound for the contrast pivot in basis points.
pub const MAX_PIVOT_BP: u16 = 10_000;
/// Inclusive upper bound for saturation (per mille; `1000` is neutral).
pub const MAX_SATURATION_PERMILLE: u16 = 3000;
/// Inclusive bound on a wheel's `sqrt(x² + y²)`.
pub const MAX_WHEEL_RADIUS: i16 = 1000;
/// Inclusive magnitude bound for a wheel master.
pub const MAX_WHEEL_MASTER: i16 = 1000;
/// Inclusive upper bound for curve coordinates in basis points.
pub const MAX_CURVE_COORDINATE: u16 = 10_000;
/// Minimum number of control points in an RGB curve.
pub const MIN_CURVE_POINTS: usize = 2;
/// Maximum number of control points in an RGB curve.
pub const MAX_CURVE_POINTS: usize = 16;
/// Maximum number of control points in a hue or luma curve.
pub const MAX_HUE_CURVE_POINTS: usize = 16;
/// Inclusive upper bound for hue positions in tenths of a degree.
pub const MAX_HUE_POSITION: u16 = 3599;
/// Inclusive upper bound for luma-curve positions in basis points.
pub const MAX_LUMA_POSITION: u16 = 10_000;
/// Inclusive magnitude bound for hue and luma curve values (per mille).
pub const MAX_HUE_CURVE_VALUE: i16 = 1000;
/// Required prefix of a LUT asset URI.
pub const LUT_URI_PREFIX: &str = "asset://luts/";
/// Inclusive upper bound for the LUT asset URI length in bytes.
pub const MAX_LUT_URI_BYTES: usize = 1024;
/// Inclusive upper bound for LUT strength (per mille).
pub const MAX_LUT_STRENGTH_PERMILLE: u16 = 1000;

const IDENTITY_CURVE_POINTS: [CurvePoint; 2] = [
    CurvePoint::new(0, 0),
    CurvePoint::new(MAX_CURVE_COORDINATE, MAX_CURVE_COORDINATE),
];

/// White balance in per-mille units; `0` is neutral.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct WhiteBalance {
    /// Blue (negative) to amber (positive), `-1000..=1000`.
    pub temperature: i16,
    /// Green (negative) to magenta (positive; lowers green), `-1000..=1000`.
    pub tint: i16,
}

impl WhiteBalance {
    pub const NEUTRAL: Self = Self {
        temperature: 0,
        tint: 0,
    };

    #[must_use]
    pub const fn new(temperature: i16, tint: i16) -> Self {
        Self { temperature, tint }
    }

    #[must_use]
    pub const fn is_neutral(self) -> bool {
        self.temperature == 0 && self.tint == 0
    }
}

/// Linear contrast around a pivot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Contrast {
    /// Per-mille slope change, `-1000..=1000`; `0` is neutral.
    pub amount: i16,
    /// Pivot in basis points, `0..=10000`; default `5000`.
    pub pivot_bp: u16,
}

impl Contrast {
    pub const NEUTRAL: Self = Self {
        amount: 0,
        pivot_bp: 5000,
    };

    #[must_use]
    pub const fn new(amount: i16, pivot_bp: u16) -> Self {
        Self { amount, pivot_bp }
    }

    /// A zero amount is neutral regardless of the pivot.
    #[must_use]
    pub const fn is_neutral(self) -> bool {
        self.amount == 0
    }
}

impl Default for Contrast {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// One colour wheel: a `(Cb, Cr)` direction plus a master offset.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Wheel {
    /// Cb direction, per mille. `x² + y² <= 1000²`.
    pub x: i16,
    /// Cr direction, per mille. `x² + y² <= 1000²`.
    pub y: i16,
    /// Master, `-1000..=1000`.
    pub master: i16,
}

impl Wheel {
    pub const NEUTRAL: Self = Self {
        x: 0,
        y: 0,
        master: 0,
    };

    #[must_use]
    pub const fn new(x: i16, y: i16, master: i16) -> Self {
        Self { x, y, master }
    }

    #[must_use]
    pub const fn is_neutral(self) -> bool {
        self.x == 0 && self.y == 0 && self.master == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WheelKind {
    Lift,
    Gamma,
    Gain,
    Offset,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Wheels {
    pub lift: Wheel,
    pub gamma: Wheel,
    pub gain: Wheel,
    pub offset: Wheel,
}

impl Wheels {
    pub const NEUTRAL: Self = Self {
        lift: Wheel::NEUTRAL,
        gamma: Wheel::NEUTRAL,
        gain: Wheel::NEUTRAL,
        offset: Wheel::NEUTRAL,
    };

    #[must_use]
    pub const fn is_neutral(self) -> bool {
        self.lift.is_neutral()
            && self.gamma.is_neutral()
            && self.gain.is_neutral()
            && self.offset.is_neutral()
    }

    #[must_use]
    pub const fn get(self, kind: WheelKind) -> Wheel {
        match kind {
            WheelKind::Lift => self.lift,
            WheelKind::Gamma => self.gamma,
            WheelKind::Gain => self.gain,
            WheelKind::Offset => self.offset,
        }
    }
}

/// An RGB-curve control point in basis points (`0..=10000` on both axes).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CurvePoint {
    pub x: u16,
    pub y: u16,
}

impl CurvePoint {
    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A monotone-cubic transfer curve over `[0, 1]`.
///
/// Valid curves have `2..=16` points with strictly increasing `x`, first
/// `x == 0`, last `x == 10000`, and every coordinate `<= 10000`. The
/// identity is `[(0, 0), (10000, 10000)]`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Curve(Cow<'static, [CurvePoint]>);

impl Curve {
    pub const IDENTITY: Self = Self(Cow::Borrowed(&IDENTITY_CURVE_POINTS));

    /// Wraps owned points; bounds are checked by [`Grade::validate`].
    #[must_use]
    pub fn new(points: Vec<CurvePoint>) -> Self {
        Self(Cow::Owned(points))
    }

    /// Wraps static points in a `const` context; bounds are checked by
    /// [`Grade::validate`].
    #[must_use]
    pub const fn from_static(points: &'static [CurvePoint]) -> Self {
        Self(Cow::Borrowed(points))
    }

    #[must_use]
    pub fn points(&self) -> &[CurvePoint] {
        &self.0
    }

    /// True only for the canonical two-point identity.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self.0 == IDENTITY_CURVE_POINTS
    }
}

impl Default for Curve {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CurveChannel {
    Master,
    Red,
    Green,
    Blue,
}

/// Master curve applied first, then the per-channel curve.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct RgbCurves {
    pub master: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
}

impl RgbCurves {
    pub const IDENTITY: Self = Self {
        master: Curve::IDENTITY,
        red: Curve::IDENTITY,
        green: Curve::IDENTITY,
        blue: Curve::IDENTITY,
    };

    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.master.is_identity()
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
    }

    #[must_use]
    pub const fn get(&self, channel: CurveChannel) -> &Curve {
        match channel {
            CurveChannel::Master => &self.master,
            CurveChannel::Red => &self.red,
            CurveChannel::Green => &self.green,
            CurveChannel::Blue => &self.blue,
        }
    }
}

/// A hue- or luma-curve control point.
///
/// `position` is tenths of a degree (`0..=3599`) for hue curves and basis
/// points (`0..=10000`) for the luma curve; `value` is per mille,
/// `-1000..=1000`, with `0` neutral.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HuePoint {
    pub position: u16,
    pub value: i16,
}

impl HuePoint {
    #[must_use]
    pub const fn new(position: u16, value: i16) -> Self {
        Self { position, value }
    }
}

/// `0..=16` points with strictly increasing positions; empty is identity.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct HueCurve(Cow<'static, [HuePoint]>);

impl HueCurve {
    pub const EMPTY: Self = Self(Cow::Borrowed(&[]));

    /// Wraps owned points; bounds are checked by [`Grade::validate`].
    #[must_use]
    pub fn new(points: Vec<HuePoint>) -> Self {
        Self(Cow::Owned(points))
    }

    #[must_use]
    pub const fn from_static(points: &'static [HuePoint]) -> Self {
        Self(Cow::Borrowed(points))
    }

    #[must_use]
    pub fn points(&self) -> &[HuePoint] {
        &self.0
    }

    /// Empty or all-zero curves are the identity.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.0.iter().all(|point| point.value == 0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HueCurveKind {
    HueVsHue,
    HueVsSaturation,
    LumaVsSaturation,
}

impl HueCurveKind {
    /// Inclusive upper bound for this curve's positions.
    #[must_use]
    pub const fn max_position(self) -> u16 {
        match self {
            Self::HueVsHue | Self::HueVsSaturation => MAX_HUE_POSITION,
            Self::LumaVsSaturation => MAX_LUMA_POSITION,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct HueCurves {
    pub hue_vs_hue: HueCurve,
    pub hue_vs_saturation: HueCurve,
    pub luma_vs_saturation: HueCurve,
}

impl HueCurves {
    pub const IDENTITY: Self = Self {
        hue_vs_hue: HueCurve::EMPTY,
        hue_vs_saturation: HueCurve::EMPTY,
        luma_vs_saturation: HueCurve::EMPTY,
    };

    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.hue_vs_hue.is_identity()
            && self.hue_vs_saturation.is_identity()
            && self.luma_vs_saturation.is_identity()
    }

    #[must_use]
    pub const fn get(&self, kind: HueCurveKind) -> &HueCurve {
        match kind {
            HueCurveKind::HueVsHue => &self.hue_vs_hue,
            HueCurveKind::HueVsSaturation => &self.hue_vs_saturation,
            HueCurveKind::LumaVsSaturation => &self.luma_vs_saturation,
        }
    }
}

/// Reference to a `.cube` asset and its mix strength.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GradeLut {
    /// `asset://luts/<path>`, at most 1024 bytes.
    pub asset_uri: String,
    /// `0..=1000`; `0` disables the LUT stage.
    pub strength_permille: u16,
}

impl GradeLut {
    #[must_use]
    pub fn new(asset_uri: impl Into<String>, strength_permille: u16) -> Self {
        Self {
            asset_uri: asset_uri.into(),
            strength_permille,
        }
    }
}

/// A complete, bounded primary grade. See the crate documentation for the
/// evaluation contract.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Grade {
    pub enabled: bool,
    pub white_balance: WhiteBalance,
    /// `-4000..=4000` thousandths of a stop.
    pub exposure_millistops: i16,
    pub contrast: Contrast,
    /// `0..=3000`; `1000` is neutral and `0` is monochrome.
    pub saturation_permille: u16,
    pub wheels: Wheels,
    pub curves: RgbCurves,
    pub hue_curves: HueCurves,
    pub lut: Option<GradeLut>,
}

impl Grade {
    pub const IDENTITY: Self = Self {
        enabled: true,
        white_balance: WhiteBalance::NEUTRAL,
        exposure_millistops: 0,
        contrast: Contrast::NEUTRAL,
        saturation_permille: 1000,
        wheels: Wheels::NEUTRAL,
        curves: RgbCurves::IDENTITY,
        hue_curves: HueCurves::IDENTITY,
        lut: None,
    };

    /// True when evaluation is the exact identity: the grade is disabled, or
    /// every stage is neutral (a LUT counts only with nonzero strength).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        !self.enabled
            || (self.white_balance.is_neutral()
                && self.exposure_millistops == 0
                && self.contrast.is_neutral()
                && self.saturation_permille == 1000
                && self.wheels.is_neutral()
                && self.curves.is_identity()
                && self.hue_curves.is_identity()
                && self
                    .lut
                    .as_ref()
                    .is_none_or(|lut| lut.strength_permille == 0))
    }

    /// Checks every documented bound.
    ///
    /// # Errors
    /// Returns the first violated bound as a typed [`GradeError`].
    pub fn validate(&self) -> Result<(), GradeError> {
        let wb = self.white_balance;
        if !in_symmetric(wb.temperature, MAX_WHITE_BALANCE) {
            return Err(GradeError::Temperature(wb.temperature));
        }
        if !in_symmetric(wb.tint, MAX_WHITE_BALANCE) {
            return Err(GradeError::Tint(wb.tint));
        }
        if !in_symmetric(self.exposure_millistops, MAX_EXPOSURE_MILLISTOPS) {
            return Err(GradeError::Exposure(self.exposure_millistops));
        }
        if !in_symmetric(self.contrast.amount, MAX_CONTRAST_AMOUNT) {
            return Err(GradeError::ContrastAmount(self.contrast.amount));
        }
        if self.contrast.pivot_bp > MAX_PIVOT_BP {
            return Err(GradeError::ContrastPivot(self.contrast.pivot_bp));
        }
        if self.saturation_permille > MAX_SATURATION_PERMILLE {
            return Err(GradeError::Saturation(self.saturation_permille));
        }
        for kind in [
            WheelKind::Lift,
            WheelKind::Gamma,
            WheelKind::Gain,
            WheelKind::Offset,
        ] {
            validate_wheel(kind, self.wheels.get(kind))?;
        }
        for channel in [
            CurveChannel::Master,
            CurveChannel::Red,
            CurveChannel::Green,
            CurveChannel::Blue,
        ] {
            validate_curve(channel, self.curves.get(channel).points())?;
        }
        for kind in [
            HueCurveKind::HueVsHue,
            HueCurveKind::HueVsSaturation,
            HueCurveKind::LumaVsSaturation,
        ] {
            validate_hue_curve(kind, self.hue_curves.get(kind).points())?;
        }
        if let Some(lut) = &self.lut {
            validate_lut_uri(&lut.asset_uri)?;
            if lut.strength_permille > MAX_LUT_STRENGTH_PERMILLE {
                return Err(GradeError::LutStrength(lut.strength_permille));
            }
        }
        Ok(())
    }
}

impl Default for Grade {
    fn default() -> Self {
        Self::IDENTITY
    }
}

const fn in_symmetric(value: i16, bound: i16) -> bool {
    value >= -bound && value <= bound
}

fn validate_wheel(kind: WheelKind, wheel: Wheel) -> Result<(), GradeError> {
    let radius = i32::from(MAX_WHEEL_RADIUS);
    let x = i32::from(wheel.x);
    let y = i32::from(wheel.y);
    if x * x + y * y > radius * radius {
        return Err(GradeError::WheelRadius {
            wheel: kind,
            x: wheel.x,
            y: wheel.y,
        });
    }
    if !in_symmetric(wheel.master, MAX_WHEEL_MASTER) {
        return Err(GradeError::WheelMaster {
            wheel: kind,
            master: wheel.master,
        });
    }
    Ok(())
}

fn validate_curve(channel: CurveChannel, points: &[CurvePoint]) -> Result<(), GradeError> {
    if !(MIN_CURVE_POINTS..=MAX_CURVE_POINTS).contains(&points.len()) {
        return Err(GradeError::CurvePointCount {
            channel,
            count: points.len(),
        });
    }
    for (index, point) in points.iter().enumerate() {
        if point.x > MAX_CURVE_COORDINATE || point.y > MAX_CURVE_COORDINATE {
            return Err(GradeError::CurveCoordinate { channel, index });
        }
        if index > 0 && point.x <= points[index - 1].x {
            return Err(GradeError::CurveNotIncreasing { channel, index });
        }
    }
    if points[0].x != 0 {
        return Err(GradeError::CurveStart { channel });
    }
    if points[points.len() - 1].x != MAX_CURVE_COORDINATE {
        return Err(GradeError::CurveEnd { channel });
    }
    Ok(())
}

fn validate_hue_curve(kind: HueCurveKind, points: &[HuePoint]) -> Result<(), GradeError> {
    if points.len() > MAX_HUE_CURVE_POINTS {
        return Err(GradeError::HueCurvePointCount {
            curve: kind,
            count: points.len(),
        });
    }
    for (index, point) in points.iter().enumerate() {
        if point.position > kind.max_position() {
            return Err(GradeError::HueCurvePosition {
                curve: kind,
                index,
                position: point.position,
            });
        }
        if !in_symmetric(point.value, MAX_HUE_CURVE_VALUE) {
            return Err(GradeError::HueCurveValue {
                curve: kind,
                index,
                value: point.value,
            });
        }
        if index > 0 && point.position <= points[index - 1].position {
            return Err(GradeError::HueCurveNotIncreasing { curve: kind, index });
        }
    }
    Ok(())
}

fn validate_lut_uri(uri: &str) -> Result<(), GradeError> {
    if uri.len() > MAX_LUT_URI_BYTES {
        return Err(GradeError::LutUriTooLong { bytes: uri.len() });
    }
    let Some(path) = uri.strip_prefix(LUT_URI_PREFIX) else {
        return Err(GradeError::LutUriScheme);
    };
    let valid = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if valid {
        Ok(())
    } else {
        Err(GradeError::LutUriPath)
    }
}

/// A violated [`Grade`] bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeError {
    Temperature(i16),
    Tint(i16),
    Exposure(i16),
    ContrastAmount(i16),
    ContrastPivot(u16),
    Saturation(u16),
    WheelRadius {
        wheel: WheelKind,
        x: i16,
        y: i16,
    },
    WheelMaster {
        wheel: WheelKind,
        master: i16,
    },
    CurvePointCount {
        channel: CurveChannel,
        count: usize,
    },
    CurveCoordinate {
        channel: CurveChannel,
        index: usize,
    },
    CurveNotIncreasing {
        channel: CurveChannel,
        index: usize,
    },
    CurveStart {
        channel: CurveChannel,
    },
    CurveEnd {
        channel: CurveChannel,
    },
    HueCurvePointCount {
        curve: HueCurveKind,
        count: usize,
    },
    HueCurvePosition {
        curve: HueCurveKind,
        index: usize,
        position: u16,
    },
    HueCurveValue {
        curve: HueCurveKind,
        index: usize,
        value: i16,
    },
    HueCurveNotIncreasing {
        curve: HueCurveKind,
        index: usize,
    },
    LutUriTooLong {
        bytes: usize,
    },
    LutUriScheme,
    LutUriPath,
    LutStrength(u16),
}

impl fmt::Display for GradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Temperature(value) => write!(
                formatter,
                "white-balance temperature {value} is outside ±{MAX_WHITE_BALANCE}"
            ),
            Self::Tint(value) => write!(
                formatter,
                "white-balance tint {value} is outside ±{MAX_WHITE_BALANCE}"
            ),
            Self::Exposure(value) => write!(
                formatter,
                "exposure {value} millistops is outside ±{MAX_EXPOSURE_MILLISTOPS}"
            ),
            Self::ContrastAmount(value) => write!(
                formatter,
                "contrast amount {value} is outside ±{MAX_CONTRAST_AMOUNT}"
            ),
            Self::ContrastPivot(value) => write!(
                formatter,
                "contrast pivot {value} exceeds {MAX_PIVOT_BP} basis points"
            ),
            Self::Saturation(value) => write!(
                formatter,
                "saturation {value} exceeds {MAX_SATURATION_PERMILLE} per mille"
            ),
            Self::WheelRadius { wheel, x, y } => write!(
                formatter,
                "{wheel:?} wheel direction ({x}, {y}) exceeds radius {MAX_WHEEL_RADIUS}"
            ),
            Self::WheelMaster { wheel, master } => write!(
                formatter,
                "{wheel:?} wheel master {master} is outside ±{MAX_WHEEL_MASTER}"
            ),
            Self::CurvePointCount { channel, count } => write!(
                formatter,
                "{channel:?} curve has {count} points; expected {MIN_CURVE_POINTS}..={MAX_CURVE_POINTS}"
            ),
            Self::CurveCoordinate { channel, index } => write!(
                formatter,
                "{channel:?} curve point {index} exceeds {MAX_CURVE_COORDINATE} basis points"
            ),
            Self::CurveNotIncreasing { channel, index } => write!(
                formatter,
                "{channel:?} curve point {index} x is not strictly increasing"
            ),
            Self::CurveStart { channel } => {
                write!(formatter, "{channel:?} curve must start at x = 0")
            }
            Self::CurveEnd { channel } => write!(
                formatter,
                "{channel:?} curve must end at x = {MAX_CURVE_COORDINATE}"
            ),
            Self::HueCurvePointCount { curve, count } => write!(
                formatter,
                "{curve:?} curve has {count} points; maximum is {MAX_HUE_CURVE_POINTS}"
            ),
            Self::HueCurvePosition {
                curve,
                index,
                position,
            } => write!(
                formatter,
                "{curve:?} curve point {index} position {position} exceeds {}",
                curve.max_position()
            ),
            Self::HueCurveValue {
                curve,
                index,
                value,
            } => write!(
                formatter,
                "{curve:?} curve point {index} value {value} is outside ±{MAX_HUE_CURVE_VALUE}"
            ),
            Self::HueCurveNotIncreasing { curve, index } => write!(
                formatter,
                "{curve:?} curve point {index} position is not strictly increasing"
            ),
            Self::LutUriTooLong { bytes } => write!(
                formatter,
                "LUT asset URI has {bytes} bytes; maximum is {MAX_LUT_URI_BYTES}"
            ),
            Self::LutUriScheme => {
                write!(formatter, "LUT asset URI must start with {LUT_URI_PREFIX}")
            }
            Self::LutUriPath => formatter.write_str("LUT asset URI path is invalid"),
            Self::LutStrength(value) => write!(
                formatter,
                "LUT strength {value} exceeds {MAX_LUT_STRENGTH_PERMILLE} per mille"
            ),
        }
    }
}

impl std::error::Error for GradeError {}
