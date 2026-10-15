//! Snapping for the layer designer: canvas edges and centre, the action-
//! and title-safe areas, and the other layers' edges and centres.
//!
//! A moving box offers its left edge, centre, and right edge (and top,
//! middle, bottom) to every guide line of the matching axis; the closest
//! pair within the threshold wins and the box moves by the difference. A
//! dragged handle offers just its own point. The guides the result lines
//! up with are returned so the canvas can draw them.

use super::geometry::{Bounds, Point};

/// Share of the canvas the action-safe area keeps (93%).
pub const ACTION_SAFE: f64 = 0.93;
/// Share of the canvas the title-safe area keeps (90%).
pub const TITLE_SAFE: f64 = 0.90;
/// Snap distance in screen points; the designer converts it to canvas
/// pixels at the current zoom.
pub const SNAP_DISTANCE_POINTS: f64 = 6.0;
/// Two positions this close (in pixels) count as lined up.
const ALIGNED: f64 = 0.01;

/// The orientation of a guide line.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Axis {
    /// A vertical line at an `x` position.
    Vertical,
    /// A horizontal line at a `y` position.
    Horizontal,
}

/// What a guide line marks, in tie-break order (earlier wins).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GuideKind {
    CanvasCentre,
    CanvasEdge,
    ActionSafe,
    TitleSafe,
    LayerCentre,
    LayerEdge,
}

/// One guide line the snapped result lines up with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub axis: Axis,
    pub position: f64,
    pub kind: GuideKind,
}

/// Which families of guides snapping uses.
#[allow(clippy::struct_excessive_bools)] // independent toggles
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapSettings {
    /// Master switch (the toolbar toggle).
    pub enabled: bool,
    /// Canvas edges and centre.
    pub canvas: bool,
    /// Action-safe (93%) and title-safe (90%) areas.
    pub safe_areas: bool,
    /// The other layers' edges and centres.
    pub layers: bool,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            canvas: true,
            safe_areas: true,
            layers: true,
        }
    }
}

/// The safe area keeping `share` of a `canvas`, centred.
#[must_use]
pub fn safe_area(canvas: (u32, u32), share: f64) -> Bounds {
    let (width, height) = (f64::from(canvas.0), f64::from(canvas.1));
    let inset_x = width * (1.0 - share) / 2.0;
    let inset_y = height * (1.0 - share) / 2.0;
    Bounds::new(inset_x, inset_y, width - inset_x, height - inset_y)
}

/// Every guide line snapping may use, by axis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapTargets {
    pub vertical: Vec<(f64, GuideKind)>,
    pub horizontal: Vec<(f64, GuideKind)>,
}

impl SnapTargets {
    /// The guides of a `canvas` with `others` (the layers not being moved)
    /// under `settings`; empty when snapping is off.
    #[must_use]
    pub fn new(canvas: (u32, u32), others: &[Bounds], settings: SnapSettings) -> Self {
        let mut targets = Self::default();
        if !settings.enabled {
            return targets;
        }
        let whole = Bounds::new(0.0, 0.0, f64::from(canvas.0), f64::from(canvas.1));
        if settings.canvas {
            targets.add_box(whole, GuideKind::CanvasEdge, GuideKind::CanvasCentre);
        }
        if settings.safe_areas {
            for (share, kind) in [
                (ACTION_SAFE, GuideKind::ActionSafe),
                (TITLE_SAFE, GuideKind::TitleSafe),
            ] {
                let area = safe_area(canvas, share);
                targets
                    .vertical
                    .extend([(area.left, kind), (area.right, kind)]);
                targets
                    .horizontal
                    .extend([(area.top, kind), (area.bottom, kind)]);
            }
        }
        if settings.layers {
            for other in others {
                targets.add_box(*other, GuideKind::LayerEdge, GuideKind::LayerCentre);
            }
        }
        targets
    }

    fn add_box(&mut self, bounds: Bounds, edge: GuideKind, centre: GuideKind) {
        let middle = bounds.centre();
        self.vertical.extend([
            (bounds.left, edge),
            (middle.x, centre),
            (bounds.right, edge),
        ]);
        self.horizontal.extend([
            (bounds.top, edge),
            (middle.y, centre),
            (bounds.bottom, edge),
        ]);
    }

    /// Whether there is nothing to snap to.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vertical.is_empty() && self.horizontal.is_empty()
    }
}

/// The shift that lines one of `candidates` up with the nearest of `lines`
/// within `threshold`, or `None` when none is close enough.
fn best_shift(candidates: &[f64], lines: &[(f64, GuideKind)], threshold: f64) -> Option<f64> {
    let mut best: Option<(f64, GuideKind)> = None;
    for &candidate in candidates {
        for &(line, kind) in lines {
            let shift = line - candidate;
            if shift.abs() > threshold {
                continue;
            }
            let better = best.is_none_or(|(current, current_kind)| {
                shift.abs() < current.abs() - ALIGNED
                    || ((shift.abs() - current.abs()).abs() <= ALIGNED && kind < current_kind)
            });
            if better {
                best = Some((shift, kind));
            }
        }
    }
    best.map(|(shift, _)| shift)
}

/// The guides of `axis` that `positions` line up with, best kind first,
/// one per position.
fn aligned_guides(axis: Axis, positions: &[f64], lines: &[(f64, GuideKind)]) -> Vec<Guide> {
    let mut guides: Vec<Guide> = Vec::new();
    for &position in positions {
        let found = lines
            .iter()
            .filter(|(line, _)| (line - position).abs() <= ALIGNED)
            .map(|&(_, kind)| kind)
            .min();
        if let Some(kind) = found
            && !guides
                .iter()
                .any(|guide| (guide.position - position).abs() <= ALIGNED)
        {
            guides.push(Guide {
                axis,
                position,
                kind,
            });
        }
    }
    guides
}

/// A snapped move: the offset to add to the dragged delta and the guides
/// the moved box lines up with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapped {
    pub dx: f64,
    pub dy: f64,
    pub guides: Vec<Guide>,
}

/// Snaps a moving box: its edges and centre line up with the nearest guide
/// on each axis within `threshold` pixels.
#[must_use]
pub fn snap_bounds(moving: Bounds, targets: &SnapTargets, threshold: f64) -> Snapped {
    let middle = moving.centre();
    let xs = [moving.left, middle.x, moving.right];
    let ys = [moving.top, middle.y, moving.bottom];
    let dx = best_shift(&xs, &targets.vertical, threshold).unwrap_or(0.0);
    let dy = best_shift(&ys, &targets.horizontal, threshold).unwrap_or(0.0);
    let moved = moving.translated(dx, dy);
    let middle = moved.centre();
    let mut guides = aligned_guides(
        Axis::Vertical,
        &[moved.left, middle.x, moved.right],
        &targets.vertical,
    );
    guides.extend(aligned_guides(
        Axis::Horizontal,
        &[moved.top, middle.y, moved.bottom],
        &targets.horizontal,
    ));
    Snapped { dx, dy, guides }
}

/// Snaps one dragged point on the axes it moves along (`x`, `y`).
#[must_use]
pub fn snap_point(
    point: Point,
    targets: &SnapTargets,
    threshold: f64,
    axes: (bool, bool),
) -> (Point, Vec<Guide>) {
    let mut snapped = point;
    let mut guides = Vec::new();
    if axes.0
        && let Some(shift) = best_shift(&[point.x], &targets.vertical, threshold)
    {
        snapped.x += shift;
        guides.extend(aligned_guides(
            Axis::Vertical,
            &[snapped.x],
            &targets.vertical,
        ));
    }
    if axes.1
        && let Some(shift) = best_shift(&[point.y], &targets.horizontal, threshold)
    {
        snapped.y += shift;
        guides.extend(aligned_guides(
            Axis::Horizontal,
            &[snapped.y],
            &targets.horizontal,
        ));
    }
    (snapped, guides)
}
