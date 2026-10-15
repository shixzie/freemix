//! Curve point editing in normalized coordinates.
//!
//! Every curve is edited on a unit square: `x` runs `0..=1` left to right
//! and `y` runs `0..=1` bottom to top. For RGB curves `y` is the output
//! level; for hue and luma curves `y = 0.5` is neutral and the edges are the
//! `±1000` per-mille extremes (a half-turn hue shift, or saturation from
//! none to double).
//!
//! Edits keep every [`fm_grade::Grade::validate`] bound: points stay
//! strictly increasing with at least a minimum gap, RGB curve endpoints
//! keep their `x` and cannot be deleted, and no curve grows beyond its point
//! limit. Adding the first point to an empty hue curve seeds neutral anchors
//! (six evenly spaced hues, or both ends of the luma curve) so the new point
//! bends the curve locally instead of shifting every colour at once.

use fm_grade::{
    Curve, CurveChannel, CurvePoint, Grade, HueCurve, HueCurveKind, HuePoint, MAX_CURVE_COORDINATE,
    MAX_CURVE_POINTS, MAX_HUE_CURVE_POINTS, MAX_HUE_CURVE_VALUE, MAX_HUE_POSITION, MonotoneCubic,
    PeriodicCatmullRom,
};

use super::document::GradeEditorError;

/// Smallest `x` distance between RGB curve points, in basis points (1%).
pub const MIN_RGB_GAP: u16 = 100;
/// Smallest distance between hue-curve points, in tenths of a degree (5°).
pub const MIN_HUE_GAP: u16 = 50;
/// Smallest distance between luma-curve points, in basis points (1%).
pub const MIN_LUMA_GAP: u16 = 100;
/// Neutral anchors seeded into an empty hue curve: the primaries and
/// secondaries, in tenths of a degree.
pub const HUE_ANCHORS: [u16; 6] = [0, 600, 1200, 1800, 2400, 3000];

const HUE_PERIOD: f32 = 3600.0;

/// One editable curve of a grade.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CurveTarget {
    Rgb(CurveChannel),
    Hue(HueCurveKind),
}

impl CurveTarget {
    /// The RGB curves in tab order.
    pub const RGB: [Self; 4] = [
        Self::Rgb(CurveChannel::Master),
        Self::Rgb(CurveChannel::Red),
        Self::Rgb(CurveChannel::Green),
        Self::Rgb(CurveChannel::Blue),
    ];
    /// The hue and luma curves in tab order.
    pub const HUE: [Self; 3] = [
        Self::Hue(HueCurveKind::HueVsHue),
        Self::Hue(HueCurveKind::HueVsSaturation),
        Self::Hue(HueCurveKind::LumaVsSaturation),
    ];
    /// Every curve in tab order.
    pub const ALL: [Self; 7] = [
        Self::RGB[0],
        Self::RGB[1],
        Self::RGB[2],
        Self::RGB[3],
        Self::HUE[0],
        Self::HUE[1],
        Self::HUE[2],
    ];

    /// Most points the curve holds.
    #[must_use]
    pub const fn max_points(self) -> usize {
        match self {
            Self::Rgb(_) => MAX_CURVE_POINTS,
            Self::Hue(_) => MAX_HUE_CURVE_POINTS,
        }
    }

    /// Whether `x` wraps around (hue on the horizontal axis).
    #[must_use]
    pub const fn is_periodic(self) -> bool {
        matches!(
            self,
            Self::Hue(HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation)
        )
    }

    /// Resets the curve to its identity.
    pub fn reset(self, grade: &mut Grade) {
        match self {
            Self::Rgb(channel) => *rgb_curve_mut(grade, channel) = Curve::IDENTITY,
            Self::Hue(kind) => *hue_curve_mut(grade, kind) = HueCurve::EMPTY,
        }
    }

    /// Whether the curve leaves the picture unchanged.
    #[must_use]
    pub fn is_identity(self, grade: &Grade) -> bool {
        match self {
            Self::Rgb(channel) => grade.curves.get(channel).is_identity(),
            Self::Hue(kind) => grade.hue_curves.get(kind).is_identity(),
        }
    }

    /// Number of control points.
    #[must_use]
    pub fn len(self, grade: &Grade) -> usize {
        match self {
            Self::Rgb(channel) => grade.curves.get(channel).points().len(),
            Self::Hue(kind) => grade.hue_curves.get(kind).points().len(),
        }
    }

    /// Whether the curve has no control points (only hue curves can).
    #[must_use]
    pub fn is_empty(self, grade: &Grade) -> bool {
        self.len(grade) == 0
    }

    /// Control points in normalized coordinates.
    #[must_use]
    pub fn points(self, grade: &Grade) -> Vec<[f32; 2]> {
        match self {
            Self::Rgb(channel) => grade
                .curves
                .get(channel)
                .points()
                .iter()
                .map(|point| [bp(point.x), bp(point.y)])
                .collect(),
            Self::Hue(kind) => grade
                .hue_curves
                .get(kind)
                .points()
                .iter()
                .map(|point| [position_to_x(kind, point.position), value_to_y(point.value)])
                .collect(),
        }
    }

    /// The curve's normalized `y` at `count` evenly spaced `x` from `0` to
    /// `1` inclusive, evaluated exactly as the grade does.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Sample counts are small.
    pub fn sample(self, grade: &Grade, count: usize) -> Vec<f32> {
        let count = count.max(2);
        let xs = (0..count).map(|index| index as f32 / (count - 1) as f32);
        match self {
            Self::Rgb(channel) => {
                let knots: Vec<(f32, f32)> = grade
                    .curves
                    .get(channel)
                    .points()
                    .iter()
                    .map(|point| (bp(point.x), bp(point.y)))
                    .collect();
                match MonotoneCubic::new(&knots) {
                    Some(spline) => xs.map(|x| spline.evaluate(x).clamp(0.0, 1.0)).collect(),
                    None => xs.collect(),
                }
            }
            Self::Hue(kind) => {
                let points = grade.hue_curves.get(kind).points();
                let knots = |scale: f32| -> Vec<(f32, f32)> {
                    points
                        .iter()
                        .map(|point| {
                            (
                                f32::from(point.position) / scale,
                                f32::from(point.value) / f32::from(MAX_HUE_CURVE_VALUE),
                            )
                        })
                        .collect()
                };
                let evaluate: Box<dyn Fn(f32) -> f32> = if points.is_empty() {
                    Box::new(|_| 0.0)
                } else if self.is_periodic() {
                    match PeriodicCatmullRom::new(&knots(HUE_PERIOD), 1.0) {
                        Some(spline) => {
                            Box::new(move |x: f32| spline.evaluate(if x >= 1.0 { 0.0 } else { x }))
                        }
                        None => Box::new(|_| 0.0),
                    }
                } else {
                    match MonotoneCubic::new(&knots(f32::from(MAX_CURVE_COORDINATE))) {
                        Some(spline) => Box::new(move |x| spline.evaluate(x)),
                        None => Box::new(|_| 0.0),
                    }
                };
                xs.map(|x| 0.5 + 0.5 * evaluate(x).clamp(-1.0, 1.0))
                    .collect()
            }
        }
    }

    /// The point nearest `at` within `tolerance` (normalized, per axis).
    #[must_use]
    pub fn hit(self, grade: &Grade, at: [f32; 2], tolerance: [f32; 2]) -> Option<usize> {
        let tolerance = [
            tolerance[0].max(f32::EPSILON),
            tolerance[1].max(f32::EPSILON),
        ];
        self.points(grade)
            .into_iter()
            .enumerate()
            .filter_map(|(index, point)| {
                let dx = (point[0] - at[0]) / tolerance[0];
                let dy = (point[1] - at[1]) / tolerance[1];
                let distance = dx * dx + dy * dy;
                (distance <= 1.0).then_some((index, distance))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
    }

    /// Whether the point at `index` may be deleted.
    #[must_use]
    pub fn can_delete(self, grade: &Grade, index: usize) -> bool {
        let len = self.len(grade);
        match self {
            Self::Rgb(_) => index > 0 && index + 1 < len,
            Self::Hue(_) => index < len,
        }
    }

    /// Inserts a point at normalized `at` and returns its index.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::CurveFull`] at the point limit, or
    /// [`GradeEditorError::PointTaken`] within the minimum gap of another
    /// point (including an RGB endpoint).
    pub fn add_point(self, grade: &mut Grade, at: [f32; 2]) -> Result<usize, GradeEditorError> {
        match self {
            Self::Rgb(channel) => {
                let curve = rgb_curve_mut(grade, channel);
                let mut points = curve.points().to_vec();
                if points.len() >= MAX_CURVE_POINTS {
                    return Err(GradeEditorError::CurveFull {
                        max: MAX_CURVE_POINTS,
                    });
                }
                let x = to_bp(at[0]);
                if points.iter().any(|point| point.x.abs_diff(x) < MIN_RGB_GAP) {
                    return Err(GradeEditorError::PointTaken);
                }
                let index = points.partition_point(|point| point.x < x);
                points.insert(index, CurvePoint::new(x, to_bp(at[1])));
                *curve = Curve::new(points);
                Ok(index)
            }
            Self::Hue(kind) => {
                let gap = gap(kind);
                let position = x_to_position(kind, at[0]);
                let value = y_to_value(at[1]);
                let curve = hue_curve_mut(grade, kind);
                let mut points = curve.points().to_vec();
                if points.is_empty() {
                    points = anchors(kind);
                    points.retain(|point| point.position.abs_diff(position) >= gap);
                } else if points.len() >= MAX_HUE_CURVE_POINTS {
                    return Err(GradeEditorError::CurveFull {
                        max: MAX_HUE_CURVE_POINTS,
                    });
                } else if points
                    .iter()
                    .any(|point| hue_distance(kind, point.position, position) < gap)
                {
                    return Err(GradeEditorError::PointTaken);
                }
                let index = points.partition_point(|point| point.position < position);
                points.insert(index, HuePoint::new(position, value));
                *curve = HueCurve::new(points);
                Ok(index)
            }
        }
    }

    /// Moves the point at `index` towards normalized `to`, clamped between
    /// its neighbours (RGB endpoints keep their `x`).
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::UnknownPoint`] for a bad index.
    pub fn move_point(
        self,
        grade: &mut Grade,
        index: usize,
        to: [f32; 2],
    ) -> Result<(), GradeEditorError> {
        match self {
            Self::Rgb(channel) => {
                let curve = rgb_curve_mut(grade, channel);
                let mut points = curve.points().to_vec();
                let last = points.len().checked_sub(1);
                if last.is_none_or(|last| index > last) {
                    return Err(GradeEditorError::UnknownPoint);
                }
                let x = if index == 0 || Some(index) == last {
                    points[index].x
                } else {
                    let low = points[index - 1].x.saturating_add(MIN_RGB_GAP);
                    let high = points[index + 1].x.saturating_sub(MIN_RGB_GAP);
                    to_bp(to[0]).clamp(low, high.max(low))
                };
                points[index] = CurvePoint::new(x, to_bp(to[1]));
                *curve = Curve::new(points);
                Ok(())
            }
            Self::Hue(kind) => {
                let gap = gap(kind);
                let curve = hue_curve_mut(grade, kind);
                let mut points = curve.points().to_vec();
                if index >= points.len() {
                    return Err(GradeEditorError::UnknownPoint);
                }
                let low = if index == 0 {
                    0
                } else {
                    points[index - 1].position.saturating_add(gap)
                };
                let high = points.get(index + 1).map_or(kind.max_position(), |next| {
                    next.position.saturating_sub(gap)
                });
                let position = x_to_position(kind, to[0]).clamp(low, high.max(low));
                points[index] = HuePoint::new(position, y_to_value(to[1]));
                *curve = HueCurve::new(points);
                Ok(())
            }
        }
    }

    /// Deletes the point at `index`.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::EndpointLocked`] for an RGB endpoint, or
    /// [`GradeEditorError::UnknownPoint`] for a bad index.
    pub fn delete_point(self, grade: &mut Grade, index: usize) -> Result<(), GradeEditorError> {
        if index >= self.len(grade) {
            return Err(GradeEditorError::UnknownPoint);
        }
        if !self.can_delete(grade, index) {
            return Err(GradeEditorError::EndpointLocked);
        }
        match self {
            Self::Rgb(channel) => {
                let curve = rgb_curve_mut(grade, channel);
                let mut points = curve.points().to_vec();
                points.remove(index);
                *curve = if points == Curve::IDENTITY.points() {
                    Curve::IDENTITY
                } else {
                    Curve::new(points)
                };
            }
            Self::Hue(kind) => {
                let curve = hue_curve_mut(grade, kind);
                let mut points = curve.points().to_vec();
                points.remove(index);
                *curve = HueCurve::new(points);
            }
        }
        Ok(())
    }
}

fn rgb_curve_mut(grade: &mut Grade, channel: CurveChannel) -> &mut Curve {
    match channel {
        CurveChannel::Master => &mut grade.curves.master,
        CurveChannel::Red => &mut grade.curves.red,
        CurveChannel::Green => &mut grade.curves.green,
        CurveChannel::Blue => &mut grade.curves.blue,
    }
}

fn hue_curve_mut(grade: &mut Grade, kind: HueCurveKind) -> &mut HueCurve {
    match kind {
        HueCurveKind::HueVsHue => &mut grade.hue_curves.hue_vs_hue,
        HueCurveKind::HueVsSaturation => &mut grade.hue_curves.hue_vs_saturation,
        HueCurveKind::LumaVsSaturation => &mut grade.hue_curves.luma_vs_saturation,
    }
}

const fn gap(kind: HueCurveKind) -> u16 {
    match kind {
        HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation => MIN_HUE_GAP,
        HueCurveKind::LumaVsSaturation => MIN_LUMA_GAP,
    }
}

fn anchors(kind: HueCurveKind) -> Vec<HuePoint> {
    match kind {
        HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation => HUE_ANCHORS
            .iter()
            .map(|&position| HuePoint::new(position, 0))
            .collect(),
        HueCurveKind::LumaVsSaturation => {
            vec![HuePoint::new(0, 0), HuePoint::new(MAX_CURVE_COORDINATE, 0)]
        }
    }
}

/// Distance between two positions, wrapping around for hue.
const fn hue_distance(kind: HueCurveKind, left: u16, right: u16) -> u16 {
    let direct = left.abs_diff(right);
    match kind {
        HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation => {
            let wrapped = MAX_HUE_POSITION + 1 - direct;
            if wrapped < direct { wrapped } else { direct }
        }
        HueCurveKind::LumaVsSaturation => direct,
    }
}

fn bp(value: u16) -> f32 {
    f32::from(value) / f32::from(MAX_CURVE_COORDINATE)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_bp(value: f32) -> u16 {
    let value = if value.is_finite() { value } else { 0.0 };
    // Clamped to 0..=10000 before the cast.
    (value.clamp(0.0, 1.0) * f32::from(MAX_CURVE_COORDINATE)).round() as u16
}

fn position_to_x(kind: HueCurveKind, position: u16) -> f32 {
    match kind {
        HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation => f32::from(position) / HUE_PERIOD,
        HueCurveKind::LumaVsSaturation => bp(position),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn x_to_position(kind: HueCurveKind, x: f32) -> u16 {
    let x = if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    match kind {
        HueCurveKind::HueVsHue | HueCurveKind::HueVsSaturation => {
            // 0..=3600 before the wrap.
            ((x * HUE_PERIOD).round() as u16) % (MAX_HUE_POSITION + 1)
        }
        HueCurveKind::LumaVsSaturation => to_bp(x),
    }
}

fn value_to_y(value: i16) -> f32 {
    0.5 + 0.5 * f32::from(value) / f32::from(MAX_HUE_CURVE_VALUE)
}

#[allow(clippy::cast_possible_truncation)]
fn y_to_value(y: f32) -> i16 {
    let y = if y.is_finite() {
        y.clamp(0.0, 1.0)
    } else {
        0.5
    };
    // Within ±1000 after the clamp.
    ((y - 0.5) * 2.0 * f32::from(MAX_HUE_CURVE_VALUE)).round() as i16
}
