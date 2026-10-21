//! Polygon garbage mattes of a layer key (SW-008).
//!
//! A garbage matte is a polygon of [`MIN_GARBAGE_MATTE_POINTS`] to
//! [`MAX_GARBAGE_MATTE_POINTS`] points in coordinates normalized to the
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
//! layers up to 16384 pixels a side, so CPU and GPU agree exactly.

use core::fmt;

/// Fewest points of a garbage matte polygon.
pub const MIN_GARBAGE_MATTE_POINTS: usize = 3;
/// Most points of a garbage matte polygon.
pub const MAX_GARBAGE_MATTE_POINTS: usize = 8;
/// The normalized coordinate of a layer's right or bottom edge.
pub const GARBAGE_MATTE_UNIT: u16 = 1_000;

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
}

/// Why a garbage matte is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GarbageMatteError {
    /// Fewer than [`MIN_GARBAGE_MATTE_POINTS`] or more than
    /// [`MAX_GARBAGE_MATTE_POINTS`] points.
    PointCount(usize),
    /// A coordinate beyond [`GARBAGE_MATTE_UNIT`].
    OutOfRange(MattePoint),
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
    /// [`GarbageMatteError`] for a point count or coordinate out of bounds.
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
        let mut stored = [MattePoint::default(); MAX_GARBAGE_MATTE_POINTS];
        stored[..points.len()].copy_from_slice(points);
        Ok(Self {
            points: stored,
            len: u8::try_from(points.len()).unwrap_or(u8::MAX),
            invert,
        })
    }

    /// A quadrilateral inset `margin` units from every edge of the layer.
    #[must_use]
    pub fn inset(margin: u16) -> Self {
        let near = margin.min(GARBAGE_MATTE_UNIT / 2);
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

/// The even-odd test of pixel `(x, y)`'s centre against `points` in
/// half-pixel units (see the module documentation).
#[must_use]
pub fn polygon_contains(points: &[(i32, i32)], x: u32, y: u32) -> bool {
    let px = i64::from(x) * 2 + 1;
    let py = i64::from(y) * 2 + 1;
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
        let matte = GarbageMatte::new(&[point; 8], true).unwrap();
        assert_eq!(matte.points().len(), 8);
        assert!(matte.invert());
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
