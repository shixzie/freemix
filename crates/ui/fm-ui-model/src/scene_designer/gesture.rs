//! Pointer gestures on the designer canvas, in project pixels.
//!
//! A press ([`SceneLayoutDocument::press`]) hit-tests the canvas and opens
//! one undo step; each pointer move ([`SceneLayoutDocument::drag`]) re-poses
//! the affected layers from where they were at the press (so a gesture
//! never accumulates rounding); the release ([`SceneLayoutDocument::release`])
//! closes the step. Escape ([`SceneLayoutDocument::cancel`]) restores the
//! layers.
//!
//! - **Move** (a layer body): every selected layer moves by the pointer's
//!   travel. The moving group snaps its edges and centre to the guides;
//!   Shift (or Ctrl/Cmd) moves freely.
//! - **Scale** (a corner or edge handle): the opposite corner or edge stays
//!   put; Shift keeps the aspect ratio. The dragged handle snaps; Ctrl/Cmd
//!   moves it freely.
//! - **Rotate** (the handle above the top edge): the layer turns about its
//!   centre; Shift steps the free angle by 15°. Stored layers hold whole
//!   quarter turns, so the result is the nearest quarter turn.
//! - **Crop** (Alt on an edge handle): that edge's crop inset follows the
//!   pointer in source pixels; the layer's box stays put.

use fm_protocol::WireSceneLayerId;

use super::document::{DesignLayer, EditLabel, SceneLayoutDocument};
use super::geometry::{Edge, Handle, HitLayer, HitTarget, LayerPlacement, Point, hit_test};
use super::snapping::{Guide, SnapSettings, SnapTargets, snap_bounds, snap_point};

/// The rotate step with Shift held, in degrees.
pub const ROTATE_STEP_DEGREES: f64 = 15.0;

/// Modifier keys held during a gesture.
#[allow(clippy::struct_excessive_bools)] // independent keys
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GestureModifiers {
    pub shift: bool,
    pub alt: bool,
    /// Ctrl, or Cmd on macOS.
    pub command: bool,
}

/// What a gesture changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureKind {
    Move,
    Scale(Handle),
    Rotate,
    Crop(Edge),
}

/// Canvas scale facts a gesture needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasMetrics {
    /// Handle grab distance, canvas pixels.
    pub tolerance: f64,
    /// How far above the top edge the rotate handle stands, canvas pixels.
    pub rotate_offset: f64,
    /// Snap distance, canvas pixels.
    pub snap_distance: f64,
}

/// A live readout of the gesture for the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Readout {
    Position { x: i32, y: i32 },
    Size { width: u32, height: u32 },
    Angle { free: f64, stored: f64 },
    Crop { edge: Edge, pixels: u32 },
}

/// What the canvas draws over a running gesture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GestureFeedback {
    pub guides: Vec<Guide>,
    pub readout: Option<Readout>,
}

/// The gesture in progress.
#[allow(clippy::struct_excessive_bools)] // independent gesture facts
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ActiveGesture {
    kind: GestureKind,
    layer: WireSceneLayerId,
    start: Point,
    /// The layers at the press.
    origin: Vec<DesignLayer>,
    targets: SnapTargets,
    metrics: CanvasMetrics,
    snapping: bool,
    moved: bool,
    /// Whether the press added `layer` to the selection (a click without
    /// travel on an already selected layer selects it alone).
    was_selected: bool,
    additive: bool,
}

impl SceneLayoutDocument {
    /// What a press at `point` would grab, without grabbing it.
    #[must_use]
    pub fn hit(&self, point: Point, metrics: CanvasMetrics) -> Option<HitTarget> {
        let layers: Vec<HitLayer> = self
            .layers
            .iter()
            .map(|layer| HitLayer {
                id: layer.id,
                frame: layer.placement.frame(),
                selected: self.selection.contains(&layer.id),
            })
            .collect();
        hit_test(&layers, point, metrics.tolerance, metrics.rotate_offset)
    }

    /// A press at `point`: selects and starts a gesture. A press on empty
    /// canvas clears the selection (unless additive) and starts nothing.
    /// Returns the gesture started.
    pub fn press(
        &mut self,
        point: Point,
        modifiers: GestureModifiers,
        metrics: CanvasMetrics,
        snapping: SnapSettings,
    ) -> Option<GestureKind> {
        if self.gesture.is_some() || self.transaction.is_some() {
            return None;
        }
        let Some(target) = self.hit(point, metrics) else {
            if !modifiers.shift {
                self.clear_selection();
            }
            return None;
        };
        let (layer, kind) = match target {
            HitTarget::Handle(layer, Handle::Rotate) => (layer, GestureKind::Rotate),
            HitTarget::Handle(layer, Handle::Edge(edge)) if modifiers.alt => {
                (layer, GestureKind::Crop(edge))
            }
            HitTarget::Handle(layer, handle) => (layer, GestureKind::Scale(handle)),
            HitTarget::Body(layer) => (layer, GestureKind::Move),
        };
        let was_selected = self.is_selected(layer);
        if !was_selected {
            self.select(layer, modifiers.shift);
        }
        let moving: Vec<WireSceneLayerId> = if kind == GestureKind::Move {
            self.selection.clone()
        } else {
            vec![layer]
        };
        let others: Vec<_> = self
            .layers
            .iter()
            .filter(|candidate| !moving.contains(&candidate.id) && candidate.enabled)
            .map(|candidate| candidate.placement.footprint())
            .collect();
        let label = match kind {
            GestureKind::Move => EditLabel::Move,
            GestureKind::Scale(_) => EditLabel::Scale,
            GestureKind::Rotate => EditLabel::Rotate,
            GestureKind::Crop(_) => EditLabel::Crop,
        };
        self.begin(label);
        self.gesture = Some(ActiveGesture {
            kind,
            layer,
            start: point,
            origin: self.layers.clone(),
            targets: SnapTargets::new(self.canvas(), &others, snapping),
            metrics,
            snapping: snapping.enabled,
            moved: false,
            was_selected,
            additive: modifiers.shift,
        });
        Some(kind)
    }

    /// The gesture in progress, if any.
    #[must_use]
    pub fn gesture_kind(&self) -> Option<GestureKind> {
        self.gesture.as_ref().map(|gesture| gesture.kind)
    }

    /// The pointer moved to `point` during a gesture: re-poses the layers
    /// from the press and returns what to draw.
    pub fn drag(&mut self, point: Point, modifiers: GestureModifiers) -> GestureFeedback {
        let Some(mut gesture) = self.gesture.take() else {
            return GestureFeedback::default();
        };
        if point.distance(gesture.start) > 0.0 {
            gesture.moved = true;
        }
        let feedback = match gesture.kind {
            GestureKind::Move => self.drag_move(&gesture, point, modifiers),
            GestureKind::Scale(handle) => self.drag_scale(&gesture, handle, point, modifiers),
            GestureKind::Rotate => self.drag_rotate(&gesture, point, modifiers),
            GestureKind::Crop(edge) => self.drag_crop(&gesture, edge, point),
        };
        self.gesture = Some(gesture);
        feedback
    }

    /// Ends the gesture: one undo step when anything changed. A click
    /// without travel on a layer of a multi-selection selects it alone
    /// (unless additive). Returns whether a step was committed.
    pub fn release(&mut self) -> bool {
        let Some(gesture) = self.gesture.take() else {
            return false;
        };
        let committed = self.commit();
        if !gesture.moved && gesture.was_selected && gesture.kind == GestureKind::Move {
            self.select(gesture.layer, gesture.additive);
        }
        committed
    }

    fn origin_of(gesture: &ActiveGesture, id: WireSceneLayerId) -> Option<&DesignLayer> {
        gesture.origin.iter().find(|layer| layer.id == id)
    }

    fn set_placement(&mut self, id: WireSceneLayerId, placement: LayerPlacement) {
        if let Some(layer) = self.layers.iter_mut().find(|layer| layer.id == id) {
            layer.placement = placement;
        }
    }

    fn drag_move(
        &mut self,
        gesture: &ActiveGesture,
        point: Point,
        modifiers: GestureModifiers,
    ) -> GestureFeedback {
        let mut dx = point.x - gesture.start.x;
        let mut dy = point.y - gesture.start.y;
        let moving: Vec<&DesignLayer> = gesture
            .origin
            .iter()
            .filter(|layer| self.selection.contains(&layer.id))
            .collect();
        let Some(first) = moving.first() else {
            return GestureFeedback::default();
        };
        let group = moving
            .iter()
            .fold(first.placement.footprint(), |bounds, layer| {
                bounds.union(layer.placement.footprint())
            });
        let mut guides = Vec::new();
        if gesture.snapping && !modifiers.shift && !modifiers.command {
            let snapped = snap_bounds(
                group.translated(dx.round(), dy.round()),
                &gesture.targets,
                gesture.metrics.snap_distance,
            );
            dx = dx.round() + snapped.dx;
            dy = dy.round() + snapped.dy;
            guides = snapped.guides;
        }
        #[allow(clippy::cast_possible_truncation)] // Canvas travel is far inside i32.
        let (step_x, step_y) = (dx.round() as i32, dy.round() as i32);
        let updates: Vec<(WireSceneLayerId, LayerPlacement)> = moving
            .iter()
            .map(|layer| {
                let mut placement = layer.placement;
                placement.x = placement.x.saturating_add(step_x);
                placement.y = placement.y.saturating_add(step_y);
                (layer.id, placement)
            })
            .collect();
        for (id, placement) in updates {
            self.set_placement(id, placement);
        }
        let anchor = self
            .layer(gesture.layer)
            .map_or((0, 0), |layer| (layer.placement.x, layer.placement.y));
        GestureFeedback {
            guides,
            readout: Some(Readout::Position {
                x: anchor.0,
                y: anchor.1,
            }),
        }
    }

    fn drag_scale(
        &mut self,
        gesture: &ActiveGesture,
        handle: Handle,
        point: Point,
        modifiers: GestureModifiers,
    ) -> GestureFeedback {
        let Some(origin) = Self::origin_of(gesture, gesture.layer) else {
            return GestureFeedback::default();
        };
        let placement = origin.placement;
        let frame = placement.frame();
        let (sx, sy) = match handle {
            Handle::Corner(corner) => corner.signs(),
            Handle::Edge(edge) => edge.signs(),
            Handle::Rotate => return GestureFeedback::default(),
        };
        // The world axes the handle moves along.
        let along = |local: Point| {
            let world = frame.to_world(local);
            (
                (world.x - frame.centre.x).abs() > 1e-6,
                (world.y - frame.centre.y).abs() > 1e-6,
            )
        };
        let axes_x = along(Point::new(sx, 0.0));
        let axes_y = along(Point::new(0.0, sy));
        let axes = (axes_x.0 || axes_y.0, axes_x.1 || axes_y.1);
        let (point, guides) = if gesture.snapping && !modifiers.command {
            snap_point(point, &gesture.targets, gesture.metrics.snap_distance, axes)
        } else {
            (point, Vec::new())
        };
        let local = frame.to_local(point);
        let (w0, h0) = (frame.half_width * 2.0, frame.half_height * 2.0);
        let anchor = Point::new(-sx * frame.half_width, -sy * frame.half_height);
        let mut width = if sx == 0.0 {
            w0
        } else {
            ((local.x - anchor.x) * sx).max(1.0)
        };
        let mut height = if sy == 0.0 {
            h0
        } else {
            ((local.y - anchor.y) * sy).max(1.0)
        };
        if modifiers.shift {
            let scale = if sx == 0.0 {
                height / h0
            } else if sy == 0.0 {
                width / w0
            } else {
                (width / w0).max(height / h0)
            };
            width = (w0 * scale).max(1.0);
            height = (h0 * scale).max(1.0);
        }
        let centre_local = Point::new(
            if sx == 0.0 {
                0.0
            } else {
                anchor.x + sx * width / 2.0
            },
            if sy == 0.0 {
                0.0
            } else {
                anchor.y + sy * height / 2.0
            },
        );
        let scaled = LayerPlacement::centred(
            frame.to_world(centre_local),
            width,
            height,
            placement.quarter_turns,
        );
        self.set_placement(gesture.layer, scaled);
        GestureFeedback {
            guides,
            readout: Some(Readout::Size {
                width: scaled.width,
                height: scaled.height,
            }),
        }
    }

    fn drag_rotate(
        &mut self,
        gesture: &ActiveGesture,
        point: Point,
        modifiers: GestureModifiers,
    ) -> GestureFeedback {
        let Some(origin) = Self::origin_of(gesture, gesture.layer) else {
            return GestureFeedback::default();
        };
        let placement = origin.placement;
        let centre = placement.footprint().centre();
        let angle = |at: Point| (at.y - centre.y).atan2(at.x - centre.x).to_degrees();
        let free = rotation_angle(
            placement.angle_degrees() + angle(point) - angle(gesture.start),
            modifiers.shift,
        );
        let turns = quarter_turns(free);
        let turned = LayerPlacement::centred(
            centre,
            f64::from(placement.width),
            f64::from(placement.height),
            turns,
        );
        self.set_placement(gesture.layer, turned);
        GestureFeedback {
            guides: Vec::new(),
            readout: Some(Readout::Angle {
                free,
                stored: f64::from(turns) * 90.0,
            }),
        }
    }

    fn drag_crop(&mut self, gesture: &ActiveGesture, edge: Edge, point: Point) -> GestureFeedback {
        let Some(origin) = Self::origin_of(gesture, gesture.layer) else {
            return GestureFeedback::default();
        };
        let canvas = self.canvas();
        let frame = origin.placement.frame();
        let local = frame.to_local(point);
        let (sx, sy) = edge.signs();
        // Inward travel from the edge in box pixels, then in source pixels
        // (the cropped source fills the box).
        let cropped = origin.crop.cropped(canvas);
        let (inward, factor) = if sx == 0.0 {
            (
                (sy * frame.half_height - local.y) * sy,
                f64::from(cropped.1) / (frame.half_height * 2.0),
            )
        } else {
            (
                (sx * frame.half_width - local.x) * sx,
                f64::from(cropped.0) / (frame.half_width * 2.0),
            )
        };
        let start = f64::from(origin.crop.get(edge));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
        let value = (start + inward * factor)
            .round()
            .clamp(0.0, f64::from(canvas.0.max(canvas.1))) as u32;
        let mut crop = origin.crop;
        crop.set(edge, value, canvas);
        if let Some(layer) = self
            .layers
            .iter_mut()
            .find(|layer| layer.id == gesture.layer)
        {
            layer.crop = crop;
            if let Some(mask) = origin.mask {
                layer.mask = Some(mask.clamped(crop.cropped(canvas)));
            }
        }
        GestureFeedback {
            guides: Vec::new(),
            readout: Some(Readout::Crop {
                edge,
                pixels: crop.get(edge),
            }),
        }
    }
}

/// A free rotation angle normalized to `0..360`, stepped by
/// [`ROTATE_STEP_DEGREES`] when `stepped`.
#[must_use]
pub fn rotation_angle(degrees: f64, stepped: bool) -> f64 {
    let angle = if stepped {
        (degrees / ROTATE_STEP_DEGREES).round() * ROTATE_STEP_DEGREES
    } else {
        degrees
    };
    let normalized = angle.rem_euclid(360.0);
    if (normalized - 360.0).abs() < 1e-9 {
        0.0
    } else {
        normalized
    }
}

/// The quarter turn nearest a clockwise angle.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rem_euclid(4).
pub fn quarter_turns(degrees: f64) -> u8 {
    (degrees / 90.0).round().rem_euclid(4.0) as u8
}
