//! Colour wheel and slider drag math.
//!
//! A wheel's `(x, y)` is a `(Cb, Cr)` direction in per mille inside the
//! unit disc. On screen the puck sits at `x` to the right and `y` upwards,
//! the same orientation as the vectorscope, so pushing a wheel towards a hue
//! moves the picture towards the same spot on the scope.
//!
//! Drags are relative: the puck follows the pointer's motion from where it
//! was, not the pointer's absolute position, so grabbing anywhere never
//! jumps the grade. Holding the fine-adjust modifier scales motion by
//! [`FINE_FACTOR`]; toggling it mid-drag re-anchors so the value does not
//! jump either.

use fm_grade::reference::{
    LUMA_B, LUMA_G, LUMA_R, WHEEL_CB_TO_B, WHEEL_CB_TO_G, WHEEL_CR_TO_G, WHEEL_CR_TO_R,
};
use fm_grade::{MAX_WHEEL_RADIUS, Wheel};

/// Motion scale while the fine-adjust modifier is held.
pub const FINE_FACTOR: f32 = 0.1;

const RADIUS: f32 = MAX_WHEEL_RADIUS as f32;

/// The puck's position for `wheel` in the unit disc: `x` right, `y` up.
#[must_use]
pub fn puck_offset(wheel: Wheel) -> [f32; 2] {
    [f32::from(wheel.x) / RADIUS, f32::from(wheel.y) / RADIUS]
}

/// The wheel direction for a puck at `offset` (unit disc, `y` up), clamped
/// to the disc and rounded so the result always validates.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn direction_from_offset(offset: [f32; 2]) -> (i16, i16) {
    let [mut u, mut v] = offset.map(|value| if value.is_finite() { value } else { 0.0 });
    let length = u.hypot(v);
    if length > 1.0 {
        u /= length;
        v /= length;
    }
    // |u|, |v| <= 1 so both fit i16 after scaling.
    let mut x = (u * RADIUS).round() as i16;
    let mut y = (v * RADIUS).round() as i16;
    let bound = i32::from(MAX_WHEEL_RADIUS).pow(2);
    while i32::from(x).pow(2) + i32::from(y).pow(2) > bound {
        // Rounding can land one unit outside; step back towards the centre.
        x -= x.signum();
        y -= y.signum();
    }
    (x, y)
}

/// A relative drag of a wheel's puck.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelDrag {
    anchor_offset: [f32; 2],
    anchor_pointer: [f32; 2],
    radius_px: f32,
    fine: bool,
    last: [f32; 2],
}

impl WheelDrag {
    /// Starts dragging `wheel`'s puck from screen point `pointer` (pixels,
    /// `y` down) on a wheel drawn `radius_px` pixels in radius.
    #[must_use]
    pub fn new(wheel: Wheel, pointer: [f32; 2], radius_px: f32, fine: bool) -> Self {
        let offset = puck_offset(wheel);
        Self {
            anchor_offset: offset,
            anchor_pointer: pointer,
            radius_px: radius_px.max(1.0),
            fine,
            last: offset,
        }
    }

    /// The wheel direction with the pointer at `pointer`.
    pub fn update(&mut self, pointer: [f32; 2], fine: bool) -> (i16, i16) {
        if fine != self.fine {
            self.anchor_offset = self.last;
            self.anchor_pointer = pointer;
            self.fine = fine;
        }
        let scale = if fine { FINE_FACTOR } else { 1.0 } / self.radius_px;
        let mut offset = [
            self.anchor_offset[0] + (pointer[0] - self.anchor_pointer[0]) * scale,
            // Screen y grows downwards; wheel y grows upwards.
            self.anchor_offset[1] - (pointer[1] - self.anchor_pointer[1]) * scale,
        ];
        let length = offset[0].hypot(offset[1]);
        if length > 1.0 {
            offset = [offset[0] / length, offset[1] / length];
        }
        self.last = offset;
        direction_from_offset(offset)
    }
}

/// A relative horizontal drag of a scalar control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueDrag {
    anchor_value: f32,
    anchor_pointer: f32,
    units_per_px: f32,
    range: (i32, i32),
    fine: bool,
    last: f32,
}

impl ValueDrag {
    /// Starts dragging a control at `value` with range `range` over a track
    /// `track_px` pixels long, from pointer coordinate `pointer`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Grade ranges are tiny.
    pub fn new(value: i32, range: (i32, i32), track_px: f32, pointer: f32, fine: bool) -> Self {
        let span = (range.1 - range.0) as f32;
        Self {
            anchor_value: value as f32,
            anchor_pointer: pointer,
            units_per_px: span / track_px.max(1.0),
            range,
            fine,
            last: value as f32,
        }
    }

    /// The value with the pointer at `pointer`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn update(&mut self, pointer: f32, fine: bool) -> i32 {
        if fine != self.fine {
            self.anchor_value = self.last;
            self.anchor_pointer = pointer;
            self.fine = fine;
        }
        let scale = if fine { FINE_FACTOR } else { 1.0 };
        let value = (self.anchor_value
            + (pointer - self.anchor_pointer) * self.units_per_px * scale)
            .clamp(self.range.0 as f32, self.range.1 as f32);
        self.last = value;
        // Clamped to an i32 range above.
        value.round() as i32
    }
}

/// The display colour of the wheel at unit-disc point `offset` (`y` up):
/// mid grey pushed in that `(Cb, Cr)` direction, so the wheel shows which
/// way each direction tints the picture.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn wheel_color(offset: [f32; 2]) -> [u8; 3] {
    let [u, v] = offset;
    let luma = 0.55;
    let chroma = 0.42;
    let rgb = [
        luma + chroma * WHEEL_CR_TO_R * v,
        luma + chroma * (WHEEL_CB_TO_G * u + WHEEL_CR_TO_G * v),
        luma + chroma * WHEEL_CB_TO_B * u,
    ];
    // Keep the wheel's brightness even around the rim.
    let shift = luma - (LUMA_R * rgb[0] + LUMA_G * rgb[1] + LUMA_B * rgb[2]);
    // Clamped to 0..=255 before the cast.
    rgb.map(|channel| ((channel + shift).clamp(0.0, 1.0) * 255.0).round() as u8)
}
