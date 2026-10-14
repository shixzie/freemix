//! Deterministic mapping from normalized joystick/mouse input to PTZ speeds.

use crate::{AxisCapabilities, CameraCapabilities, ContinuousMove, ContinuousSource};

/// Shape applied to deflection after the deadzone is removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResponseCurve {
    #[default]
    Linear,
    /// `t^2`: finer control near center.
    Quadratic,
    /// `t^3`: finest control near center.
    Cubic,
}

impl ResponseCurve {
    fn apply(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::Quadratic => t * t,
            Self::Cubic => t * t * t,
        }
    }
}

/// Normalized controller deflection. Each component is clamped to `-1..=1`;
/// non-finite values are treated as zero.
///
/// Positive pan is right, positive tilt is up, positive zoom is tele, and
/// positive focus is far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlVector {
    pub pan: f64,
    pub tilt: f64,
    pub zoom: f64,
    pub focus: f64,
}

/// Per-axis deadzone and response curve.
///
/// Inside the deadzone an axis maps to zero. Outside it, deflection is
/// rescaled to `0..=1`, curved, and mapped to `1..=max_speed` with
/// round-half-away-from-zero, so any deflection beyond the deadzone moves the
/// camera and full deflection reaches the axis maximum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeedMapping {
    deadzone: f64,
    curve: ResponseCurve,
}

impl Default for SpeedMapping {
    fn default() -> Self {
        Self {
            deadzone: 0.0,
            curve: ResponseCurve::Linear,
        }
    }
}

impl SpeedMapping {
    /// Creates a mapping with a deadzone in `0.0..1.0`.
    #[must_use]
    pub fn new(deadzone: f64, curve: ResponseCurve) -> Option<Self> {
        if deadzone.is_finite() && (0.0..1.0).contains(&deadzone) {
            Some(Self { deadzone, curve })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn deadzone(&self) -> f64 {
        self.deadzone
    }

    #[must_use]
    pub const fn curve(&self) -> ResponseCurve {
        self.curve
    }

    /// Maps one normalized axis value to a signed speed in `-max..=max`.
    /// A nonpositive `max_speed` always yields zero.
    #[must_use]
    pub fn map_axis(&self, value: f64, max_speed: i32) -> i32 {
        if max_speed <= 0 || !value.is_finite() {
            return 0;
        }
        let value = value.clamp(-1.0, 1.0);
        let magnitude = value.abs();
        if magnitude <= self.deadzone {
            return 0;
        }
        let t = ((magnitude - self.deadzone) / (1.0 - self.deadzone)).clamp(0.0, 1.0);
        let curved = self.curve.apply(t);
        let span = f64::from(max_speed - 1);
        // `curved * span` lies in `0..=span`, which fits in i32 by construction.
        #[allow(clippy::cast_possible_truncation)]
        let speed = 1 + (curved * span).round() as i32;
        let speed = speed.clamp(1, max_speed);
        if value.is_sign_negative() {
            -speed
        } else {
            speed
        }
    }

    /// Maps a vector to a continuous move within the camera's speed limits.
    /// Axes the camera lacks map to zero.
    #[must_use]
    pub fn map(
        &self,
        source: ContinuousSource,
        vector: ControlVector,
        capabilities: &CameraCapabilities,
    ) -> ContinuousMove {
        let speed = |value: f64, axis: Option<AxisCapabilities>| {
            axis.map_or(0, |axis| self.map_axis(value, axis.max_speed))
        };
        ContinuousMove {
            source,
            pan: speed(vector.pan, Some(capabilities.pan)),
            tilt: speed(vector.tilt, Some(capabilities.tilt)),
            zoom: speed(vector.zoom, capabilities.zoom),
            focus: speed(vector.focus, capabilities.focus),
        }
    }
}

/// Converts a mouse drag in screen pixels to a normalized vector.
///
/// A drag of `radius_px` in any direction is full deflection. Screen Y grows
/// downward, so dragging up tilts up. Wheel notches map to zoom, one notch
/// per `wheel_notches_full_speed` reaching full speed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseDragMapping {
    radius_px: u32,
    wheel_notches_full_speed: u32,
}

impl MouseDragMapping {
    /// Creates a mapping; both scales must be nonzero.
    #[must_use]
    pub const fn new(radius_px: u32, wheel_notches_full_speed: u32) -> Option<Self> {
        if radius_px == 0 || wheel_notches_full_speed == 0 {
            None
        } else {
            Some(Self {
                radius_px,
                wheel_notches_full_speed,
            })
        }
    }

    #[must_use]
    pub fn vector(
        &self,
        horizontal_px: i32,
        vertical_px: i32,
        wheel_notches: i32,
    ) -> ControlVector {
        let radius = f64::from(self.radius_px);
        let notches = f64::from(self.wheel_notches_full_speed);
        ControlVector {
            pan: (f64::from(horizontal_px) / radius).clamp(-1.0, 1.0),
            tilt: (-f64::from(vertical_px) / radius).clamp(-1.0, 1.0),
            zoom: (f64::from(wheel_notches) / notches).clamp(-1.0, 1.0),
            focus: 0.0,
        }
    }
}
