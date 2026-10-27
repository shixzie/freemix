//! Polygon garbage mattes of a layer key (SW-008).
//!
//! A garbage matte is a polygon of [`MIN_GARBAGE_MATTE_POINTS`] to
//! [`MAX_GARBAGE_MATTE_POINTS`] points, not all on one line (a polygon
//! without area keeps nothing, so it would silently remove the whole
//! layer), in coordinates normalized to the
//! keyed (cropped) layer: `0` is its left or top edge and
//! [`GARBAGE_MATTE_UNIT`] its right or bottom edge. The matte keeps the
//! pixels inside the polygon and removes the rest, or the reverse when
//! inverted. It multiplies the key's matte with the layer's rectangular
//! mask, after clip, choke, and edge softness.
//!
//! # Exact pixel coverage
//!
//! On a `width` x `height` layer each point maps to half-pixel units
//! `X = (x * 2 * width + UNIT / 2) / UNIT` and
//! `Y = (y * 2 * height + UNIT / 2) / UNIT`, and pixel `(px, py)` is tested
//! at its centre `(2 px + 1, 2 py + 1)` with the even-odd rule: it is
//! inside when a ray to its right crosses an odd number of edges, where the
//! edge from `a` to `b` is crossed when `(a.y > Y) != (b.y > Y)` and
//! `(X - a.x) * (b.y - a.y)` is below `(Y - a.y) * (b.x - a.x)` (with the
//! comparison reversed when `b.y < a.y`). Every product fits in 32 bits for
//! layers up to [`MAX_GARBAGE_MATTE_LAYER_SIDE`] pixels a side, so CPU and GPU
//! agree exactly; the native compositor refuses a matte over a larger layer.
//!
//! # Feather
//!
//! A matte with a feather of `f` pixels (at most
//! [`MAX_GARBAGE_MATTE_FEATHER`]) softens its edge: pixel `(px, py)` is
//! tested at the 81 points `(2 px + 1 + c * f / 2, 2 py + 1 + r * f / 2)`
//! for `c` and `r` in `-4..=4` (integer division rounding toward zero), a
//! grid `f` pixels to each side of the centre, each clamped to the layer's
//! pixel centres (`1..=2 width - 1` and `1..=2 height - 1`, so an edge on the
//! layer's own border stays hard), and keeps
//! `(kept * 255 + 40) / 81` of the layer, `kept` being the points the
//! matte keeps. Every point is tested exactly as above, so the ramp is as
//! exact as the edge; sample points stay within 32-bit products for the
//! largest layer and feather. A feather of 0 keeps the hard edge.

use core::fmt;

/// Fewest points of a garbage matte polygon.
pub const MIN_GARBAGE_MATTE_POINTS: usize = 3;
/// Most points of a garbage matte polygon.
pub const MAX_GARBAGE_MATTE_POINTS: usize = 8;
/// Widest or tallest layer whose coverage products all fit in 32 bits
/// (see the module documentation); a 32-bit implementation (the native
/// shader) refuses a matte over a larger layer.
pub const MAX_GARBAGE_MATTE_LAYER_SIDE: u32 = 16_384;
/// The normalized coordinate of a layer's right or bottom edge.
pub const GARBAGE_MATTE_UNIT: u16 = 1_000;
/// Widest garbage matte feather, in pixels to each side of the edge.
pub const MAX_GARBAGE_MATTE_FEATHER: u8 = 64;
/// Sample points per side of the feather grid (see the module
/// documentation).
const FEATHER_SAMPLES: i64 = 9;

/// One garbage matte point, normalized to the keyed layer
/// (`0..=GARBAGE_MATTE_UNIT` on both axes).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MattePoint {
    pub x: u16,
    pub y: u16,
}

impl MattePoint {
    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A polygon garbage matte; see the module documentation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GarbageMatte {
    points: [MattePoint; MAX_GARBAGE_MATTE_POINTS],
    len: u8,
    invert: bool,
    /// Pixels the edge softens over to each side; 0 is a hard edge.
    feather: u8,
}

/// Why a garbage matte is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GarbageMatteError {
    /// Fewer than [`MIN_GARBAGE_MATTE_POINTS`] or more than
    /// [`MAX_GARBAGE_MATTE_POINTS`] points.
    PointCount(usize),
    /// A coordinate beyond [`GARBAGE_MATTE_UNIT`].
    OutOfRange(MattePoint),
    /// Every point lies on one line (or on one spot), so the polygon has
    /// no area.
    Flat,
    /// A feather wider than [`MAX_GARBAGE_MATTE_FEATHER`].
    Feather(u8),
}

impl fmt::Display for GarbageMatteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PointCount(count) => write!(
                formatter,
                "a garbage matte has {MIN_GARBAGE_MATTE_POINTS} to {MAX_GARBAGE_MATTE_POINTS} points, not {count}"
            ),
            Self::OutOfRange(point) => write!(
                formatter,
                "garbage matte point {},{} is beyond {GARBAGE_MATTE_UNIT}",
                point.x, point.y
            ),
            Self::Flat => formatter
                .write_str("a garbage matte's points all lie on one line, so it has no area"),
            Self::Feather(feather) => write!(
                formatter,
                "a garbage matte feathers at most {MAX_GARBAGE_MATTE_FEATHER} pixels, not {feather}"
            ),
        }
    }
}

impl std::error::Error for GarbageMatteError {}

impl GarbageMatte {
    /// A matte of `points` that keeps their inside (or outside when
    /// `invert`).
    ///
    /// # Errors
    ///
    /// [`GarbageMatteError`] for a point count or coordinate out of bounds,
    /// or points that all lie on one line.
    pub fn new(points: &[MattePoint], invert: bool) -> Result<Self, GarbageMatteError> {
        if !(MIN_GARBAGE_MATTE_POINTS..=MAX_GARBAGE_MATTE_POINTS).contains(&points.len()) {
            return Err(GarbageMatteError::PointCount(points.len()));
        }
        if let Some(point) = points
            .iter()
            .find(|point| point.x > GARBAGE_MATTE_UNIT || point.y > GARBAGE_MATTE_UNIT)
        {
            return Err(GarbageMatteError::OutOfRange(*point));
        }
        if is_flat(points) {
            return Err(GarbageMatteError::Flat);
        }
        let mut stored = [MattePoint::default(); MAX_GARBAGE_MATTE_POINTS];
        stored[..points.len()].copy_from_slice(points);
        Ok(Self {
            points: stored,
            len: u8::try_from(points.len()).unwrap_or(u8::MAX),
            invert,
            feather: 0,
        })
    }

    /// A quadrilateral inset `margin` units from every edge of the layer
    /// (at most one unit short of the centre, so it keeps an area).
    #[must_use]
    pub fn inset(margin: u16) -> Self {
        let near = margin.min(GARBAGE_MATTE_UNIT / 2 - 1);
        let far = GARBAGE_MATTE_UNIT - near;
        let mut points = [MattePoint::default(); MAX_GARBAGE_MATTE_POINTS];
        points[..4].copy_from_slice(&[
            MattePoint::new(near, near),
            MattePoint::new(far, near),
            MattePoint::new(far, far),
            MattePoint::new(near, far),
        ]);
        Self {
            points,
            len: 4,
            invert: false,
            feather: 0,
        }
    }

    /// The polygon's points in order.
    #[must_use]
    pub fn points(&self) -> &[MattePoint] {
        &self.points[..usize::from(self.len)]
    }

    /// Whether the matte keeps the outside instead of the inside.
    #[must_use]
    pub const fn invert(&self) -> bool {
        self.invert
    }

    /// The same matte, keeping the outside when `invert`.
    #[must_use]
    pub const fn with_invert(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }

    /// Pixels the edge softens over to each side; 0 is a hard edge.
    #[must_use]
    pub const fn feather(&self) -> u8 {
        self.feather
    }

    /// The same matte feathered over `feather` pixels.
    ///
    /// # Errors
    ///
    /// [`GarbageMatteError::Feather`] beyond
    /// [`MAX_GARBAGE_MATTE_FEATHER`].
    pub const fn with_feather(mut self, feather: u8) -> Result<Self, GarbageMatteError> {
        if feather > MAX_GARBAGE_MATTE_FEATHER {
            return Err(GarbageMatteError::Feather(feather));
        }
        self.feather = feather;
        Ok(self)
    }

    /// How much of pixel `(x, y)` of a `width` x `height` layer the matte
    /// keeps, 0 to 255 (see the module documentation).
    #[must_use]
    pub fn coverage(&self, x: u32, y: u32, width: u32, height: u32) -> u8 {
        polygon_coverage(
            &self.half_pixel_points(width, height),
            (self.invert, self.feather),
            (width, height),
            x,
            y,
        )
    }

    /// The polygon's points in half-pixel units of a `width` x `height`
    /// layer (see the module documentation).
    #[must_use]
    pub fn half_pixel_points(&self, width: u32, height: u32) -> Vec<(i32, i32)> {
        let scale = |value: u16, size: u32| {
            let scaled = (u64::from(value) * 2 * u64::from(size)
                + u64::from(GARBAGE_MATTE_UNIT / 2))
                / u64::from(GARBAGE_MATTE_UNIT);
            i32::try_from(scaled).unwrap_or(i32::MAX)
        };
        self.points()
            .iter()
            .map(|point| (scale(point.x, width), scale(point.y, height)))
            .collect()
    }

    /// Whether the matte keeps pixel `(x, y)` of a `width` x `height` layer.
    #[must_use]
    pub fn keeps(&self, x: u32, y: u32, width: u32, height: u32) -> bool {
        polygon_contains(&self.half_pixel_points(width, height), x, y) != self.invert
    }
}

/// Whether every one of `points` lies on the line through the first and
/// another of them (or all coincide).
fn is_flat(points: &[MattePoint]) -> bool {
    let Some((first, rest)) = points.split_first() else {
        return true;
    };
    let offset = |point: &MattePoint| {
        (
            i64::from(point.x) - i64::from(first.x),
            i64::from(point.y) - i64::from(first.y),
        )
    };
    let Some(direction) = rest.iter().map(offset).find(|&offset| offset != (0, 0)) else {
        return true;
    };
    rest.iter()
        .map(offset)
        .all(|(x, y)| x * direction.1 == y * direction.0)
}

/// The even-odd test of pixel `(x, y)`'s centre against `points` in
/// half-pixel units (see the module documentation).
#[must_use]
pub fn polygon_contains(points: &[(i32, i32)], x: u32, y: u32) -> bool {
    contains_point(points, i64::from(x) * 2 + 1, i64::from(y) * 2 + 1)
}

/// How much of pixel `(x, y)` of a `width` x `height` layer a matte of
/// `points` (in half-pixel units), keeping the outside when `invert`, keeps
/// with a `feather` (see the module documentation).
#[must_use]
pub fn polygon_coverage(
    points: &[(i32, i32)],
    (invert, feather): (bool, u8),
    (width, height): (u32, u32),
    x: u32,
    y: u32,
) -> u8 {
    let px = i64::from(x) * 2 + 1;
    let py = i64::from(y) * 2 + 1;
    if feather == 0 {
        return if contains_point(points, px, py) == invert {
            0
        } else {
            u8::MAX
        };
    }
    let feather = i64::from(feather);
    let (right, bottom) = (
        (i64::from(width) * 2 - 1).max(1),
        (i64::from(height) * 2 - 1).max(1),
    );
    let half = FEATHER_SAMPLES / 2;
    let mut kept = 0_i64;
    for row in -half..=half {
        for column in -half..=half {
            let sample_x = (px + column * feather / 2).clamp(1, right);
            let sample_y = (py + row * feather / 2).clamp(1, bottom);
            if contains_point(points, sample_x, sample_y) != invert {
                kept += 1;
            }
        }
    }
    let total = FEATHER_SAMPLES * FEATHER_SAMPLES;
    u8::try_from((kept * 255 + total / 2) / total).unwrap_or(u8::MAX)
}

/// The even-odd test of the point `(px, py)` in half-pixel units.
fn contains_point(points: &[(i32, i32)], px: i64, py: i64) -> bool {
    let mut inside = false;
    for (index, &(ax, ay)) in points.iter().enumerate() {
        let (bx, by) = points[(index + 1) % points.len()];
        let (ax, ay, bx, by) = (i64::from(ax), i64::from(ay), i64::from(bx), i64::from(by));
        if (ay > py) == (by > py) {
            continue;
        }
        let left = (px - ax) * (by - ay);
        let right = (py - ay) * (bx - ax);
        let crosses = if by > ay { left < right } else { left > right };
        if crosses {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_is_bounded() {
        let point = MattePoint::new(0, 0);
        assert_eq!(
            GarbageMatte::new(&[point; 2], false),
            Err(GarbageMatteError::PointCount(2))
        );
        assert_eq!(
            GarbageMatte::new(&[point; 9], false),
            Err(GarbageMatteError::PointCount(9))
        );
        let far = MattePoint::new(1_001, 0);
        assert_eq!(
            GarbageMatte::new(&[point, point, far], false),
            Err(GarbageMatteError::OutOfRange(far))
        );
        let corner = MattePoint::new(1_000, 0);
        let matte = GarbageMatte::new(
            &[
                point,
                corner,
                MattePoint::new(0, 1_000),
                point,
                corner,
                point,
                corner,
                point,
            ],
            true,
        )
        .unwrap();
        assert_eq!(matte.points().len(), 8);
        assert!(matte.invert());
    }

    /// Points on one spot or one line enclose nothing, so such a matte
    /// would remove the whole layer: it is refused, inverted or not.
    #[test]
    fn a_matte_without_area_is_refused() {
        let at = |x, y| MattePoint::new(x, y);
        for points in [
            vec![at(500, 500); 3],
            vec![at(0, 0), at(500, 500), at(1_000, 1_000)],
            vec![at(100, 300), at(100, 300), at(700, 300), at(400, 300)],
            vec![at(250, 0), at(250, 1_000), at(250, 500), at(250, 0)],
            vec![at(0, 0), at(300, 100), at(600, 200), at(900, 300), at(0, 0)],
        ] {
            for invert in [false, true] {
                assert_eq!(
                    GarbageMatte::new(&points, invert),
                    Err(GarbageMatteError::Flat),
                    "{points:?}"
                );
            }
        }
        // One point off the line gives it an area; so does a bow tie, whose
        // signed area cancels but whose halves the even-odd rule keeps.
        assert!(GarbageMatte::new(&[at(0, 0), at(500, 500), at(1_000, 999)], false).is_ok());
        let bow_tie = GarbageMatte::new(
            &[at(0, 0), at(1_000, 1_000), at(1_000, 0), at(0, 1_000)],
            false,
        )
        .unwrap();
        assert!(bow_tie.keeps(0, 3, 8, 8) && !bow_tie.keeps(3, 1, 8, 8));
        // The widest inset still keeps a middle.
        let widest = GarbageMatte::inset(u16::MAX);
        assert_eq!(GarbageMatte::new(widest.points(), false), Ok(widest));
    }

    /// A feather ramps coverage across the edge: whole well inside, none
    /// well outside, part way on the edge, never decreasing toward the
    /// inside; no feather is the hard edge; more than 64 is refused.
    #[test]
    fn a_feather_ramps_coverage_across_the_edge() {
        // The left half of a 40x10 layer.
        let half = GarbageMatte::new(
            &[
                MattePoint::new(0, 0),
                MattePoint::new(500, 0),
                MattePoint::new(500, 1_000),
                MattePoint::new(0, 1_000),
            ],
            false,
        )
        .unwrap();
        for x in 0..40 {
            assert_eq!(
                half.coverage(x, 5, 40, 10),
                if half.keeps(x, 5, 40, 10) { 255 } else { 0 },
                "{x}"
            );
        }
        let soft = half.with_feather(4).unwrap();
        assert_eq!(soft.feather(), 4);
        let row: Vec<u8> = (0..40).map(|x| soft.coverage(x, 5, 40, 10)).collect();
        assert_eq!(row[10], 255);
        assert_eq!(row[30], 0);
        assert!((100..=155).contains(&row[20]), "{row:?}");
        assert!(row.windows(2).all(|pair| pair[0] >= pair[1]), "{row:?}");
        // The ramp spans the feather to each side of the edge: four pixels.
        assert_eq!(row[15], 255);
        assert_eq!(row[24], 0);
        // An edge on the layer's own border stays hard.
        assert_eq!(row[0], 255);
        // Inverted, it is the complement up to rounding.
        let inverted = soft.with_invert(true);
        for x in 0..40 {
            let sum =
                u16::from(soft.coverage(x, 5, 40, 10)) + u16::from(inverted.coverage(x, 5, 40, 10));
            assert!((254..=256).contains(&sum), "{x}: {sum}");
        }
        assert_eq!(
            half.with_feather(MAX_GARBAGE_MATTE_FEATHER + 1),
            Err(GarbageMatteError::Feather(MAX_GARBAGE_MATTE_FEATHER + 1))
        );
        assert!(half.with_feather(MAX_GARBAGE_MATTE_FEATHER).is_ok());
    }

    #[test]
    fn coverage_is_exact_on_known_pixels() {
        // A right triangle over the left-top half of a 4x4 layer:
        // (0,0) (1,0) (0,1) normalized, i.e. (0,0) (8,0) (0,8) half pixels.
        let matte = GarbageMatte::new(
            &[
                MattePoint::new(0, 0),
                MattePoint::new(GARBAGE_MATTE_UNIT, 0),
                MattePoint::new(0, GARBAGE_MATTE_UNIT),
            ],
            false,
        )
        .unwrap();
        let rows: Vec<String> = (0..4)
            .map(|y| {
                (0..4)
                    .map(|x| if matte.keeps(x, y, 4, 4) { '#' } else { '.' })
                    .collect()
            })
            .collect();
        // Centres on the diagonal (x + y == 3) sit exactly on the edge and
        // fall outside.
        assert_eq!(rows, ["###.", "##..", "#...", "...."]);
        let inverted = matte.with_invert(true);
        assert!(!inverted.keeps(0, 0, 4, 4));
        assert!(inverted.keeps(3, 3, 4, 4));
        // An inset keeps its inside only.
        let inset = GarbageMatte::inset(250);
        assert!(inset.keeps(1, 1, 4, 4) && inset.keeps(2, 2, 4, 4));
        assert!(!inset.keeps(0, 1, 4, 4) && !inset.keeps(3, 2, 4, 4));
    }
}
