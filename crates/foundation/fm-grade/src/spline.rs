//! Interpolating splines used by curves.

/// Fritsch–Carlson monotone cubic Hermite interpolation.
///
/// For knots `(x_k, y_k)`, `k = 0..n`, with strictly increasing `x`:
///
/// 1. Secants `δ_k = (y_{k+1} - y_k) / (x_{k+1} - x_k)`.
/// 2. Initial tangents: `m_0 = δ_0`, `m_{n-1} = δ_{n-2}`, and for interior
///    knots `m_k = (δ_{k-1} + δ_k) / 2` when `δ_{k-1} δ_k > 0`, else `0`.
/// 3. For each segment in order: when `δ_k == 0` both `m_k` and `m_{k+1}`
///    become `0`; otherwise with `α = m_k / δ_k` and `β = m_{k+1} / δ_k`,
///    when `α² + β² > 9` both are scaled by `τ = 3 / sqrt(α² + β²)`.
/// 4. Evaluation uses the cubic Hermite basis on the containing segment and
///    clamps the result to that segment's `[min(y_k, y_{k+1}),
///    max(y_k, y_{k+1})]`, so rounding can never overshoot.
///
/// Outside `[x_0, x_{n-1}]` the curve is constant (`y_0` / `y_{n-1}`); a
/// single knot is a constant curve. Each segment is monotone, so the curve
/// passes every knot exactly and never leaves the range of its neighbours.
#[derive(Clone, Debug, PartialEq)]
pub struct MonotoneCubic {
    xs: Vec<f32>,
    ys: Vec<f32>,
    tangents: Vec<f32>,
}

impl MonotoneCubic {
    /// Returns `None` for no knots, non-finite input, non-increasing `x`, or
    /// knots whose slopes overflow `f32`.
    #[must_use]
    pub fn new(knots: &[(f32, f32)]) -> Option<Self> {
        if knots.is_empty()
            || knots.iter().any(|(x, y)| !x.is_finite() || !y.is_finite())
            || knots.windows(2).any(|pair| pair[1].0 <= pair[0].0)
        {
            return None;
        }
        let xs: Vec<f32> = knots.iter().map(|knot| knot.0).collect();
        let ys: Vec<f32> = knots.iter().map(|knot| knot.1).collect();
        let n = knots.len();
        if n == 1 {
            return Some(Self {
                xs,
                ys,
                tangents: vec![0.0],
            });
        }
        let secants: Vec<f32> = (0..n - 1)
            .map(|k| (ys[k + 1] - ys[k]) / (xs[k + 1] - xs[k]))
            .collect();
        let mut tangents = vec![0.0_f32; n];
        tangents[0] = secants[0];
        tangents[n - 1] = secants[n - 2];
        for k in 1..n - 1 {
            tangents[k] = if secants[k - 1] * secants[k] > 0.0 {
                f32::midpoint(secants[k - 1], secants[k])
            } else {
                0.0
            };
        }
        for (k, &secant) in secants.iter().enumerate() {
            if secant == 0.0 {
                tangents[k] = 0.0;
                tangents[k + 1] = 0.0;
                continue;
            }
            let alpha = tangents[k] / secant;
            let beta = tangents[k + 1] / secant;
            let magnitude = alpha * alpha + beta * beta;
            if magnitude > 9.0 {
                let tau = 3.0 / magnitude.sqrt();
                tangents[k] = tau * alpha * secant;
                tangents[k + 1] = tau * beta * secant;
            }
        }
        // Overflowing slopes would evaluate to NaN or infinity.
        if !tangents
            .iter()
            .chain(&secants)
            .all(|value| value.is_finite())
        {
            return None;
        }
        Some(Self { xs, ys, tangents })
    }

    /// Evaluates the curve; NaN input evaluates as the first knot.
    #[must_use]
    pub fn evaluate(&self, x: f32) -> f32 {
        let n = self.xs.len();
        if n == 1 || x.is_nan() || x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let k = (self.xs.partition_point(|&knot| knot <= x) - 1).min(n - 2);
        hermite(
            x,
            self.xs[k],
            self.xs[k + 1],
            self.ys[k],
            self.ys[k + 1],
            self.tangents[k],
            self.tangents[k + 1],
        )
    }
}

/// Periodic Catmull–Rom interpolation for cyclic (hue) curves.
///
/// Knot positions lie in `[0, period)` and wrap: knot `k - 1` of the first
/// knot is the last knot shifted by `-period`, and knot `k + 1` of the last
/// knot is the first shifted by `+period`. Tangents are the non-uniform
/// Catmull–Rom finite differences
/// `m_k = (v_{k+1} - v_{k-1}) / (p_{k+1} - p_{k-1})`, and each segment is a
/// cubic Hermite, so the curve is C¹ continuous across the wrap. A single
/// knot is a constant curve. Unlike [`MonotoneCubic`] the curve may
/// overshoot between knots; callers clamp to their value range.
#[derive(Clone, Debug, PartialEq)]
pub struct PeriodicCatmullRom {
    period: f32,
    positions: Vec<f32>,
    values: Vec<f32>,
    tangents: Vec<f32>,
}

impl PeriodicCatmullRom {
    /// Returns `None` for no knots, a non-positive or non-finite period,
    /// positions outside `[0, period)`, non-finite values, non-increasing
    /// positions, or a period or tangents that overflow `f32`.
    #[must_use]
    pub fn new(knots: &[(f32, f32)], period: f32) -> Option<Self> {
        if knots.is_empty()
            || !period.is_finite()
            || period <= 0.0
            || knots.iter().any(|(position, value)| {
                !position.is_finite()
                    || !value.is_finite()
                    || *position < 0.0
                    || *position >= period
            })
            || knots.windows(2).any(|pair| pair[1].0 <= pair[0].0)
            // The wrap-around segment ends at `first + period`.
            || !(knots[0].0 + period).is_finite()
        {
            return None;
        }
        let positions: Vec<f32> = knots.iter().map(|knot| knot.0).collect();
        let values: Vec<f32> = knots.iter().map(|knot| knot.1).collect();
        let n = knots.len();
        let tangents = (0..n)
            .map(|k| {
                if n == 1 {
                    return 0.0;
                }
                let (previous_position, previous_value) = if k == 0 {
                    (positions[n - 1] - period, values[n - 1])
                } else {
                    (positions[k - 1], values[k - 1])
                };
                let (next_position, next_value) = if k == n - 1 {
                    (positions[0] + period, values[0])
                } else {
                    (positions[k + 1], values[k + 1])
                };
                (next_value - previous_value) / (next_position - previous_position)
            })
            .collect::<Vec<f32>>();
        if !tangents.iter().all(|tangent| tangent.is_finite()) {
            return None;
        }
        Some(Self {
            period,
            positions,
            values,
            tangents,
        })
    }

    /// Evaluates the curve at `x`, wrapped into the period. Non-finite input
    /// (and input so far out that it cannot be wrapped) evaluates as the
    /// first knot.
    #[must_use]
    pub fn evaluate(&self, x: f32) -> f32 {
        let n = self.positions.len();
        let first = self.positions[0];
        let offset = x - first;
        if n == 1 || !offset.is_finite() {
            return self.values[0];
        }
        let mut wrapped = first + offset.rem_euclid(self.period);
        if wrapped >= first + self.period {
            wrapped = first;
        }
        let k = self.positions.partition_point(|&knot| knot <= wrapped) - 1;
        let (end_position, end_value) = if k == n - 1 {
            (first + self.period, self.values[0])
        } else {
            (self.positions[k + 1], self.values[k + 1])
        };
        let end_tangent = self.tangents[(k + 1) % n];
        let width = end_position - self.positions[k];
        hermite_basis(
            (wrapped - self.positions[k]) / width,
            width,
            self.values[k],
            end_value,
            self.tangents[k],
            end_tangent,
        )
    }
}

fn hermite(
    x: f32,
    start: f32,
    end: f32,
    start_value: f32,
    end_value: f32,
    start_tangent: f32,
    end_tangent: f32,
) -> f32 {
    let width = end - start;
    let t = (x - start) / width;
    hermite_basis(t, width, start_value, end_value, start_tangent, end_tangent)
        .clamp(start_value.min(end_value), start_value.max(end_value))
}

/// Cubic Hermite on a segment of `width` at normalized parameter `t`.
fn hermite_basis(
    t: f32,
    width: f32,
    start_value: f32,
    end_value: f32,
    start_tangent: f32,
    end_tangent: f32,
) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * start_value + h10 * width * start_tangent + h01 * end_value + h11 * width * end_tangent
}
