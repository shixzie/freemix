use crate::{PlanError, Rational};

/// Minimum camera zoom (1.000x, the full set is visible).
pub const MIN_ZOOM_PERMILLE: u32 = 1000;
/// Maximum camera zoom (8.000x).
pub const MAX_ZOOM_PERMILLE: u32 = 8000;
/// Maximum easing control points per curve, including both endpoints.
pub const MAX_EASING_POINTS: usize = 16;
/// Maximum camera-move length in frames (ten minutes at 60 fps).
pub const MAX_MOVE_FRAMES: u32 = 36_000;
/// Grid onto which an interrupted move's start pose is snapped (1/1,000,000).
pub const CAMERA_STATE_QUANTUM: i128 = 1_000_000;

/// A camera preset pose expressed in integer control units.
///
/// `pan_*` is the requested window centre in set space (`0..=1000` permille)
/// and `zoom` is the magnification in permille (`1000..=8000`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CameraPose {
    pan_x: u32,
    pan_y: u32,
    zoom: u32,
}

impl CameraPose {
    /// # Errors
    ///
    /// Rejects pans above 1000 and zooms outside the supported range.
    pub fn new(x: u32, y: u32, zoom: u32) -> Result<Self, PlanError> {
        for value_permille in [x, y] {
            if value_permille > 1000 {
                return Err(PlanError::PanOutOfRange { value_permille });
            }
        }
        if !(MIN_ZOOM_PERMILLE..=MAX_ZOOM_PERMILLE).contains(&zoom) {
            return Err(PlanError::ZoomOutOfRange {
                value_permille: zoom,
                minimum: MIN_ZOOM_PERMILLE,
                maximum: MAX_ZOOM_PERMILLE,
            });
        }
        Ok(Self {
            pan_x: x,
            pan_y: y,
            zoom,
        })
    }

    /// The full set, centred, at 1x.
    #[must_use]
    pub const fn wide() -> Self {
        Self {
            pan_x: 500,
            pan_y: 500,
            zoom: 1000,
        }
    }

    #[must_use]
    pub const fn pan_x_permille(self) -> u32 {
        self.pan_x
    }

    #[must_use]
    pub const fn pan_y_permille(self) -> u32 {
        self.pan_y
    }

    #[must_use]
    pub const fn zoom_permille(self) -> u32 {
        self.zoom
    }

    #[must_use]
    pub fn state(self) -> CameraState {
        CameraState {
            pan_x: Rational::permille(i64::from(self.pan_x)),
            pan_y: Rational::permille(i64::from(self.pan_y)),
            zoom: Rational::permille(i64::from(self.zoom)),
        }
    }
}

/// An exact camera state: requested window centre and zoom in set space.
///
/// States are only produced from validated poses or interpolation between
/// them, so `0 <= pan <= 1` and `1 <= zoom <= 8` always hold.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CameraState {
    pan_x: Rational,
    pan_y: Rational,
    zoom: Rational,
}

impl CameraState {
    #[must_use]
    pub const fn pan_x(self) -> Rational {
        self.pan_x
    }

    #[must_use]
    pub const fn pan_y(self) -> Rational {
        self.pan_y
    }

    #[must_use]
    pub const fn zoom(self) -> Rational {
        self.zoom
    }

    /// Returns the state with its window centre clamped so the visible window
    /// `[centre - 1/(2 zoom), centre + 1/(2 zoom)]` stays inside the set.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on overflow.
    pub fn clamped(self) -> Result<Self, PlanError> {
        let half = Rational::ONE.checked_div(self.zoom.checked_mul(Rational::integer(2))?)?;
        let max = Rational::ONE.checked_sub(half)?;
        Ok(Self {
            pan_x: self.pan_x.clamp_to(half, max),
            pan_y: self.pan_y.clamp_to(half, max),
            zoom: self.zoom,
        })
    }

    /// Snaps every component to the [`CAMERA_STATE_QUANTUM`] grid.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on overflow.
    pub fn quantized(self) -> Result<Self, PlanError> {
        Ok(Self {
            pan_x: self.pan_x.quantize(CAMERA_STATE_QUANTUM)?,
            pan_y: self.pan_y.quantize(CAMERA_STATE_QUANTUM)?,
            zoom: self.zoom.quantize(CAMERA_STATE_QUANTUM)?,
        })
    }

    fn lerp(self, to: Self, amount: Rational) -> Result<Self, PlanError> {
        let lerp = |from: Rational, to: Rational| {
            from.checked_add(to.checked_sub(from)?.checked_mul(amount)?)
        };
        Ok(Self {
            pan_x: lerp(self.pan_x, to.pan_x)?,
            pan_y: lerp(self.pan_y, to.pan_y)?,
            zoom: lerp(self.zoom, to.zoom)?,
        })
    }
}

/// One easing control point; both axes are in permille of the move.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EasingPoint {
    pub t_permille: u32,
    pub value_permille: u32,
}

impl EasingPoint {
    #[must_use]
    pub const fn new(t_permille: u32, value_permille: u32) -> Self {
        Self {
            t_permille,
            value_permille,
        }
    }
}

/// A piecewise-linear easing curve through integer control points.
///
/// The curve starts at `(0, 0)`, ends at `(1000, 1000)`, has strictly
/// increasing `t`, and keeps values inside `0..=1000` so eased poses never
/// leave the segment between the two endpoint poses.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EasingCurve {
    points: Vec<EasingPoint>,
}

impl EasingCurve {
    /// # Errors
    ///
    /// Returns the first rule the control points violate.
    pub fn new(points: Vec<EasingPoint>) -> Result<Self, PlanError> {
        if points.len() < 2 {
            return Err(PlanError::TooFewEasingPoints {
                actual: points.len(),
            });
        }
        if points.len() > MAX_EASING_POINTS {
            return Err(PlanError::TooManyEasingPoints {
                actual: points.len(),
                maximum: MAX_EASING_POINTS,
            });
        }
        if points[0] != EasingPoint::new(0, 0)
            || points[points.len() - 1] != EasingPoint::new(1000, 1000)
        {
            return Err(PlanError::EasingEndpoints);
        }
        for (index, point) in points.iter().enumerate() {
            if point.value_permille > 1000 {
                return Err(PlanError::EasingValueOutOfRange { index });
            }
            if index > 0 && point.t_permille <= points[index - 1].t_permille {
                return Err(PlanError::EasingNotIncreasing { index });
            }
        }
        Ok(Self { points })
    }

    #[must_use]
    pub fn linear() -> Self {
        Self {
            points: vec![EasingPoint::new(0, 0), EasingPoint::new(1000, 1000)],
        }
    }

    /// Slow-in/slow-out: `(0,0) (250,100) (500,500) (750,900) (1000,1000)`.
    #[must_use]
    pub fn ease_in_out() -> Self {
        Self {
            points: vec![
                EasingPoint::new(0, 0),
                EasingPoint::new(250, 100),
                EasingPoint::new(500, 500),
                EasingPoint::new(750, 900),
                EasingPoint::new(1000, 1000),
            ],
        }
    }

    #[must_use]
    pub fn points(&self) -> &[EasingPoint] {
        &self.points
    }

    /// Evaluates the curve at `progress` (clamped to `0..=1`).
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on overflow.
    pub fn ease(&self, progress: Rational) -> Result<Rational, PlanError> {
        let progress = progress.clamp_to(Rational::ZERO, Rational::ONE);
        let t = progress.checked_mul(Rational::integer(1000))?;
        for pair in self.points.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let end_t = Rational::integer(i64::from(end.t_permille));
            if t <= end_t {
                let start_t = Rational::integer(i64::from(start.t_permille));
                let start_v = Rational::integer(i64::from(start.value_permille));
                let end_v = Rational::integer(i64::from(end.value_permille));
                let fraction = t
                    .checked_sub(start_t)?
                    .checked_div(end_t.checked_sub(start_t)?)?;
                let value =
                    start_v.checked_add(end_v.checked_sub(start_v)?.checked_mul(fraction)?)?;
                return value.checked_div(Rational::integer(1000));
            }
        }
        Ok(Rational::ONE)
    }
}

/// A preset-to-preset virtual camera move on the output frame timeline.
///
/// At `start_frame + k` the state is `from + (to - from) * ease(k / duration)`;
/// a zero-frame move is a cut that lands on `to` at `start_frame`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraMove {
    from: CameraState,
    to: CameraState,
    start_frame: u64,
    duration_frames: u32,
    easing: EasingCurve,
}

impl CameraMove {
    /// # Errors
    ///
    /// Rejects moves longer than [`MAX_MOVE_FRAMES`].
    pub fn new(
        from: CameraState,
        to: CameraState,
        start_frame: u64,
        duration_frames: u32,
        easing: EasingCurve,
    ) -> Result<Self, PlanError> {
        if duration_frames > MAX_MOVE_FRAMES {
            return Err(PlanError::MoveTooLong {
                frames: duration_frames,
                maximum: MAX_MOVE_FRAMES,
            });
        }
        Ok(Self {
            from,
            to,
            start_frame,
            duration_frames,
            easing,
        })
    }

    /// A move that holds `state` forever.
    #[must_use]
    pub fn hold(state: CameraState, start_frame: u64) -> Self {
        Self {
            from: state,
            to: state,
            start_frame,
            duration_frames: 0,
            easing: EasingCurve::linear(),
        }
    }

    #[must_use]
    pub const fn from(&self) -> CameraState {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> CameraState {
        self.to
    }

    #[must_use]
    pub const fn start_frame(&self) -> u64 {
        self.start_frame
    }

    #[must_use]
    pub const fn duration_frames(&self) -> u32 {
        self.duration_frames
    }

    #[must_use]
    pub const fn end_frame(&self) -> u64 {
        self.start_frame.saturating_add(self.duration_frames as u64)
    }

    #[must_use]
    pub const fn is_complete_at(&self, frame: u64) -> bool {
        frame >= self.end_frame()
    }

    /// The exact camera state at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::ArithmeticOverflow`] on overflow.
    pub fn state_at(&self, frame: u64) -> Result<CameraState, PlanError> {
        if self.is_complete_at(frame) {
            return Ok(self.to);
        }
        if frame <= self.start_frame {
            return Ok(self.from);
        }
        let elapsed = i128::from(frame - self.start_frame);
        let progress = Rational::new(elapsed, i128::from(self.duration_frames))?;
        let eased = self.easing.ease(progress)?;
        self.from.lerp(self.to, eased)
    }
}
