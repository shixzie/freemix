//! Layer geometry in project canvas pixels: placements, turned frames,
//! handles, and hit-testing.
//!
//! A stored layer is a `width` x `height` box turned clockwise by whole
//! quarter turns, its quarter-turned footprint's top-left at `(x, y)`, then
//! tilted by up to half a quarter turn either way about its anchor (a point
//! of the box in basis points, the centre by default) ([`LayerPlacement`]),
//! exactly as the compositor draws it. The handle math works on a
//! [`LayerFrame`]: the drawn box about its centre at its whole angle, so
//! every handle, the pointer's position in the layer's own axes, and
//! hit-testing are exact for any rotation.

use fm_protocol::WireSceneLayerId;

/// A point in project canvas pixels (y down).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// An axis-aligned rectangle in project canvas pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bounds {
    #[must_use]
    pub const fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[must_use]
    pub fn width(self) -> f64 {
        self.right - self.left
    }

    #[must_use]
    pub fn height(self) -> f64 {
        self.bottom - self.top
    }

    #[must_use]
    pub fn centre(self) -> Point {
        Point::new(
            f64::midpoint(self.left, self.right),
            f64::midpoint(self.top, self.bottom),
        )
    }

    /// Whether `point` lies inside or on the edge.
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.left
            && point.x <= self.right
            && point.y >= self.top
            && point.y <= self.bottom
    }

    /// The smallest bounds holding both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    /// Whether the two overlap (touching edges count).
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.left <= other.right
            && other.left <= self.right
            && self.top <= other.bottom
            && other.top <= self.bottom
    }

    /// The bounds moved by `(dx, dy)`.
    #[must_use]
    pub fn translated(self, dx: f64, dy: f64) -> Self {
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right + dx,
            self.bottom + dy,
        )
    }
}

/// Largest stored layer side, matching the project video limit.
pub const MAX_LAYER_SIDE: u32 = fm_protocol::MAX_SCENE_CANVAS_SIDE;

/// The far edge of an anchor coordinate, in basis points.
pub const ANCHOR_MAX: u16 = fm_protocol::SCENE_LAYER_ANCHOR_MAX;
/// The centre, in basis points: the default anchor.
pub const ANCHOR_CENTER: u16 = fm_protocol::SCENE_LAYER_ANCHOR_CENTER;
/// The stored tilt range, in millidegrees (`MIN..MAX`).
pub const MIN_TILT_MILLIDEGREES: i32 = fm_protocol::MIN_SCENE_LAYER_TILT_MILLIDEGREES;
pub const MAX_TILT_MILLIDEGREES: i32 = fm_protocol::MAX_SCENE_LAYER_TILT_MILLIDEGREES;

/// A clockwise angle in millidegrees as the nearest quarter turn (`0..=3`,
/// halves rounding up) and the remaining tilt in
/// [`MIN_TILT_MILLIDEGREES`]`..`[`MAX_TILT_MILLIDEGREES`], as the server
/// stores it.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Reduced first.
pub const fn split_millidegrees(millidegrees: i64) -> (u8, i32) {
    let turn = millidegrees.rem_euclid(360_000);
    let quarters = (turn + 45_000).div_euclid(90_000);
    ((quarters % 4) as u8, (turn - quarters * 90_000) as i32)
}

/// `point` turned clockwise (y down) by `degrees`.
fn turned(point: Point, degrees: f64) -> Point {
    let (sin, cos) = degrees.to_radians().sin_cos();
    Point::new(point.x * cos - point.y * sin, point.x * sin + point.y * cos)
}

/// A stored layer's placement: its unturned size, its clockwise quarter
/// turns, the top-left of its quarter-turned footprint, its tilt, and its
/// anchor (the scale and rotation pivot).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Clockwise quarter turns, `0..=3`.
    pub quarter_turns: u8,
    /// Clockwise tilt past the quarter turns, millidegrees, in
    /// [`MIN_TILT_MILLIDEGREES`]`..`[`MAX_TILT_MILLIDEGREES`]; it turns the
    /// box about its anchor.
    pub tilt_millidegrees: i32,
    /// The anchor in basis points of the unturned box from its top-left,
    /// each `0..=`[`ANCHOR_MAX`].
    pub anchor_x: u16,
    pub anchor_y: u16,
}

impl LayerPlacement {
    /// A quarter-turned placement with no tilt, anchored at its centre.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32, quarter_turns: u8) -> Self {
        Self {
            x,
            y,
            width,
            height,
            quarter_turns: quarter_turns % 4,
            tilt_millidegrees: 0,
            anchor_x: ANCHOR_CENTER,
            anchor_y: ANCHOR_CENTER,
        }
    }

    /// The placement tilted by `millidegrees` (clamped to the stored range).
    #[must_use]
    pub const fn with_tilt(mut self, millidegrees: i32) -> Self {
        self.tilt_millidegrees = if millidegrees < MIN_TILT_MILLIDEGREES {
            MIN_TILT_MILLIDEGREES
        } else if millidegrees >= MAX_TILT_MILLIDEGREES {
            MAX_TILT_MILLIDEGREES - 1
        } else {
            millidegrees
        };
        self
    }

    /// The placement anchored at (`x`, `y`) basis points (clamped).
    #[must_use]
    pub const fn with_anchor(mut self, x: u16, y: u16) -> Self {
        self.anchor_x = if x > ANCHOR_MAX { ANCHOR_MAX } else { x };
        self.anchor_y = if y > ANCHOR_MAX { ANCHOR_MAX } else { y };
        self
    }

    /// The whole clockwise rotation in millidegrees, `-45_000..315_000`.
    #[must_use]
    pub const fn rotation_millidegrees(self) -> i64 {
        (self.quarter_turns % 4) as i64 * 90_000 + self.tilt_millidegrees as i64
    }

    /// The anchor's offset from the box centre along the unturned axes,
    /// pixels, for a `width` x `height` box.
    fn anchor_offset_for(self, width: f64, height: f64) -> Point {
        let max = f64::from(ANCHOR_MAX);
        Point::new(
            (f64::from(self.anchor_x) / max - 0.5) * width,
            (f64::from(self.anchor_y) / max - 0.5) * height,
        )
    }

    /// The anchor's offset from the box centre along the unturned axes.
    #[must_use]
    pub fn anchor_offset(self) -> Point {
        self.anchor_offset_for(f64::from(self.width), f64::from(self.height))
    }

    /// Where the anchor sits on the canvas: the point turning and zooming
    /// keep still.
    #[must_use]
    pub fn anchor_point(self) -> Point {
        let centre = self.footprint().centre();
        let offset = turned(
            self.anchor_offset(),
            f64::from(self.quarter_turns % 4) * 90.0,
        );
        Point::new(centre.x + offset.x, centre.y + offset.y)
    }

    /// The placement of a `width` x `height` box turned clockwise by
    /// `millidegrees` and anchored at `anchor` (basis points) whose anchor
    /// sits at `pivot`, rounded to whole pixels (each side at least one
    /// pixel and at most [`MAX_LAYER_SIDE`]).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
    pub fn pivoted(
        pivot: Point,
        width: f64,
        height: f64,
        millidegrees: i64,
        anchor: (u16, u16),
    ) -> Self {
        let side = |value: f64| value.round().clamp(1.0, f64::from(MAX_LAYER_SIDE)) as u32;
        let (quarter_turns, tilt) = split_millidegrees(millidegrees);
        let mut placement = Self::new(0, 0, side(width), side(height), quarter_turns)
            .with_tilt(tilt)
            .with_anchor(anchor.0, anchor.1);
        let offset = turned(placement.anchor_offset(), f64::from(quarter_turns) * 90.0);
        let (footprint_width, footprint_height) = placement.footprint_size();
        let coordinate = |value: f64| {
            value
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        placement.x = coordinate(pivot.x - offset.x - f64::from(footprint_width) / 2.0);
        placement.y = coordinate(pivot.y - offset.y - f64::from(footprint_height) / 2.0);
        placement
    }

    /// The placement whose drawn box is centred on `centre`, `width` x
    /// `height`, turned clockwise by `millidegrees`, and anchored at
    /// `anchor`, rounded like [`Self::pivoted`].
    #[must_use]
    pub fn framed(
        centre: Point,
        width: f64,
        height: f64,
        millidegrees: i64,
        anchor: (u16, u16),
    ) -> Self {
        #[allow(clippy::cast_precision_loss)] // Angles are far below 2^52.
        let degrees = millidegrees as f64 / 1_000.0;
        let probe = Self::new(0, 0, 1, 1, 0).with_anchor(anchor.0, anchor.1);
        let offset = turned(
            probe.anchor_offset_for(
                width.round().clamp(1.0, f64::from(MAX_LAYER_SIDE)),
                height.round().clamp(1.0, f64::from(MAX_LAYER_SIDE)),
            ),
            degrees,
        );
        Self::pivoted(
            Point::new(centre.x + offset.x, centre.y + offset.y),
            width,
            height,
            millidegrees,
            anchor,
        )
    }

    /// The same drawn box anchored at `anchor` instead (the box stays put,
    /// up to whole-pixel rounding).
    #[must_use]
    pub fn reanchored(self, anchor: (u16, u16)) -> Self {
        let frame = self.frame();
        Self::framed(
            frame.centre,
            f64::from(self.width),
            f64::from(self.height),
            self.rotation_millidegrees(),
            anchor,
        )
    }

    /// The footprint's size on the canvas: a quarter turn swaps the sides.
    #[must_use]
    pub const fn footprint_size(self) -> (u32, u32) {
        if self.quarter_turns % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// The footprint on the canvas.
    #[must_use]
    pub fn footprint(self) -> Bounds {
        let (width, height) = self.footprint_size();
        let left = f64::from(self.x);
        let top = f64::from(self.y);
        Bounds::new(left, top, left + f64::from(width), top + f64::from(height))
    }

    /// The whole clockwise rotation in degrees.
    #[must_use]
    pub fn angle_degrees(self) -> f64 {
        f64::from(self.quarter_turns % 4) * 90.0 + f64::from(self.tilt_millidegrees) / 1_000.0
    }

    /// The placement as drawn: the box about its centre at its whole angle
    /// (the tilt turns it about the anchor).
    #[must_use]
    pub fn frame(self) -> LayerFrame {
        let anchor = self.anchor_offset();
        let pivot = self.anchor_point();
        let offset = turned(anchor, self.angle_degrees());
        LayerFrame {
            centre: Point::new(pivot.x - offset.x, pivot.y - offset.y),
            half_width: f64::from(self.width) / 2.0,
            half_height: f64::from(self.height) / 2.0,
            angle_degrees: self.angle_degrees(),
            anchor,
        }
    }

    /// The placement of an unturned `width` x `height` box turned by
    /// `quarter_turns` and centred on `centre`, rounded to whole pixels
    /// (each side at least one pixel and at most [`MAX_LAYER_SIDE`]).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
    pub fn centred(centre: Point, width: f64, height: f64, quarter_turns: u8) -> Self {
        Self::framed(
            centre,
            width,
            height,
            i64::from(quarter_turns % 4) * 90_000,
            (ANCHOR_CENTER, ANCHOR_CENTER),
        )
    }
}

/// A box turned clockwise by any angle about its centre: the frame the
/// handle math works in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerFrame {
    pub centre: Point,
    /// Half the unturned width and height.
    pub half_width: f64,
    pub half_height: f64,
    /// Clockwise rotation in degrees.
    pub angle_degrees: f64,
    /// The anchor's offset from the centre along the unturned axes.
    pub anchor: Point,
}

impl LayerFrame {
    /// The canvas point at `local` (pixels from the centre along the
    /// layer's own unturned axes).
    #[must_use]
    pub fn to_world(self, local: Point) -> Point {
        let (sin, cos) = self.angle_degrees.to_radians().sin_cos();
        Point::new(
            self.centre.x + local.x * cos - local.y * sin,
            self.centre.y + local.x * sin + local.y * cos,
        )
    }

    /// `world` in the layer's own unturned axes, relative to its centre.
    #[must_use]
    pub fn to_local(self, world: Point) -> Point {
        let (sin, cos) = self.angle_degrees.to_radians().sin_cos();
        let dx = world.x - self.centre.x;
        let dy = world.y - self.centre.y;
        Point::new(dx * cos + dy * sin, -dx * sin + dy * cos)
    }

    /// Whether `world` lies inside the turned box, grown by `margin`.
    #[must_use]
    pub fn contains(self, world: Point, margin: f64) -> bool {
        let local = self.to_local(world);
        local.x.abs() <= self.half_width + margin && local.y.abs() <= self.half_height + margin
    }

    /// The corners in the layer's own order: top-left, top-right,
    /// bottom-right, bottom-left (before turning).
    #[must_use]
    pub fn corners(self) -> [Point; 4] {
        let (w, h) = (self.half_width, self.half_height);
        [
            self.to_world(Point::new(-w, -h)),
            self.to_world(Point::new(w, -h)),
            self.to_world(Point::new(w, h)),
            self.to_world(Point::new(-w, h)),
        ]
    }

    /// The axis-aligned bounds of the turned box.
    #[must_use]
    pub fn bounds(self) -> Bounds {
        let corners = self.corners();
        let mut bounds = Bounds::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y);
        for corner in &corners[1..] {
            bounds = bounds.union(Bounds::new(corner.x, corner.y, corner.x, corner.y));
        }
        bounds
    }

    /// Where `handle` sits on the canvas; the rotate handle stands
    /// `rotate_offset` pixels beyond the layer's own top edge.
    #[must_use]
    pub fn handle_position(self, handle: Handle, rotate_offset: f64) -> Point {
        let (w, h) = (self.half_width, self.half_height);
        let local = match handle {
            Handle::Corner(corner) => {
                let (sx, sy) = corner.signs();
                Point::new(sx * w, sy * h)
            }
            Handle::Edge(edge) => {
                let (sx, sy) = edge.signs();
                Point::new(sx * w, sy * h)
            }
            Handle::Rotate => Point::new(0.0, -h - rotate_offset),
            Handle::Anchor => self.anchor,
        };
        self.to_world(local)
    }
}

/// A corner of a layer in its own (unturned) axes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];

    /// The corner's direction from the centre, each `-1` or `1`.
    #[must_use]
    pub const fn signs(self) -> (f64, f64) {
        match self {
            Self::TopLeft => (-1.0, -1.0),
            Self::TopRight => (1.0, -1.0),
            Self::BottomRight => (1.0, 1.0),
            Self::BottomLeft => (-1.0, 1.0),
        }
    }
}

/// An edge of a layer in its own (unturned) axes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Top, Self::Right, Self::Bottom, Self::Left];

    /// The edge midpoint's direction from the centre: one component `±1`,
    /// the other `0`.
    #[must_use]
    pub const fn signs(self) -> (f64, f64) {
        match self {
            Self::Top => (0.0, -1.0),
            Self::Right => (1.0, 0.0),
            Self::Bottom => (0.0, 1.0),
            Self::Left => (-1.0, 0.0),
        }
    }
}

/// A grip on a selected layer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Handle {
    /// Scales both sides, the opposite corner fixed.
    Corner(Corner),
    /// Scales one side, the opposite edge fixed; with Alt it crops that
    /// edge instead.
    Edge(Edge),
    /// Turns the layer about its anchor.
    Rotate,
    /// Moves the anchor (the scale and rotation pivot) within the layer;
    /// the layer stays put.
    Anchor,
}

impl Handle {
    /// Every handle, the rotate and anchor handles first (they win a tie).
    pub const ALL: [Self; 10] = [
        Self::Rotate,
        Self::Anchor,
        Self::Corner(Corner::TopLeft),
        Self::Corner(Corner::TopRight),
        Self::Corner(Corner::BottomRight),
        Self::Corner(Corner::BottomLeft),
        Self::Edge(Edge::Top),
        Self::Edge(Edge::Right),
        Self::Edge(Edge::Bottom),
        Self::Edge(Edge::Left),
    ];
}

/// What a pointer press lands on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget {
    /// A handle of a selected layer.
    Handle(WireSceneLayerId, Handle),
    /// A layer's body (the topmost one under the pointer).
    Body(WireSceneLayerId),
}

/// One layer as hit-testing sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitLayer {
    pub id: WireSceneLayerId,
    pub frame: LayerFrame,
    pub selected: bool,
}

/// What `point` lands on among `layers` (listed bottom to top): a handle of
/// the one selected layer within `tolerance` pixels first (the nearest),
/// then the topmost layer body. With several layers selected no handle is
/// offered: a multi-selection moves as one.
#[must_use]
pub fn hit_test(
    layers: &[HitLayer],
    point: Point,
    tolerance: f64,
    rotate_offset: f64,
) -> Option<HitTarget> {
    let selected = layers.iter().filter(|layer| layer.selected).count();
    if selected == 1 {
        let mut best: Option<(f64, HitTarget)> = None;
        for layer in layers.iter().filter(|layer| layer.selected) {
            for handle in Handle::ALL {
                let distance = layer
                    .frame
                    .handle_position(handle, rotate_offset)
                    .distance(point);
                if distance <= tolerance && best.is_none_or(|(nearest, _)| distance < nearest) {
                    best = Some((distance, HitTarget::Handle(layer.id, handle)));
                }
            }
        }
        if let Some((_, target)) = best {
            return Some(target);
        }
    }
    layers
        .iter()
        .rev()
        .find(|layer| layer.frame.contains(point, 0.0))
        .map(|layer| HitTarget::Body(layer.id))
}

/// Crop insets of a layer, in pixels of its source frame (the project
/// canvas size: every source is composed at the canvas size).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CropInsets {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl CropInsets {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }

    /// The inset on `edge`.
    #[must_use]
    pub const fn get(self, edge: Edge) -> u32 {
        match edge {
            Edge::Top => self.top,
            Edge::Right => self.right,
            Edge::Bottom => self.bottom,
            Edge::Left => self.left,
        }
    }

    /// Sets the inset on `edge`, keeping at least one source pixel between
    /// it and the opposite inset of `source` (width, height).
    pub fn set(&mut self, edge: Edge, value: u32, source: (u32, u32)) {
        let (width, height) = (source.0.max(1), source.1.max(1));
        match edge {
            Edge::Left => self.left = value.min(width - 1 - self.right.min(width - 1)),
            Edge::Right => self.right = value.min(width - 1 - self.left.min(width - 1)),
            Edge::Top => self.top = value.min(height - 1 - self.bottom.min(height - 1)),
            Edge::Bottom => self.bottom = value.min(height - 1 - self.top.min(height - 1)),
        }
    }

    /// Sets the inset on `edge` like [`Self::set`], also keeping the
    /// cropped size at least `least` (width, height): a crop never cuts
    /// into the layer's mask.
    pub fn set_keeping(&mut self, edge: Edge, value: u32, source: (u32, u32), least: (u32, u32)) {
        let (width, height) = (source.0.max(1), source.1.max(1));
        let room = |size: u32, opposite: u32, least: u32| {
            size.saturating_sub(opposite).saturating_sub(least.max(1))
        };
        let value = match edge {
            Edge::Left => value.min(room(width, self.right, least.0)),
            Edge::Right => value.min(room(width, self.left, least.0)),
            Edge::Top => value.min(room(height, self.bottom, least.1)),
            Edge::Bottom => value.min(room(height, self.top, least.1)),
        };
        self.set(edge, value, source);
    }

    /// The stored crop rectangle `(x, y, width, height)` for a `source`
    /// frame, or `None` when nothing is cropped.
    #[must_use]
    pub fn rect(self, source: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
        if self.is_empty() {
            return None;
        }
        let width = source
            .0
            .saturating_sub(self.left)
            .saturating_sub(self.right)
            .max(1);
        let height = source
            .1
            .saturating_sub(self.top)
            .saturating_sub(self.bottom)
            .max(1);
        Some((self.left, self.top, width, height))
    }

    /// The cropped source size.
    #[must_use]
    pub fn cropped(self, source: (u32, u32)) -> (u32, u32) {
        self.rect(source)
            .map_or(source, |(_, _, width, height)| (width, height))
    }
}

/// A hard-edged rectangular mask in post-crop source pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaskRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Keep what lies outside the rectangle instead of inside.
    pub inverted: bool,
}

impl MaskRect {
    /// The mask clamped inside a `cropped` (width, height) area, at least
    /// one pixel each way.
    #[must_use]
    pub fn clamped(self, cropped: (u32, u32)) -> Self {
        let (width, height) = (cropped.0.max(1), cropped.1.max(1));
        let x = self.x.min(width - 1);
        let y = self.y.min(height - 1);
        Self {
            x,
            y,
            width: self.width.clamp(1, width - x),
            height: self.height.clamp(1, height - y),
            inverted: self.inverted,
        }
    }
}
