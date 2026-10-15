//! The layer designer's document: one scene's layers as the server last
//! reported them, the operator's draft over them, the selection, what has
//! been sent live, and the draft's local undo history (one step per
//! gesture or field edit).
//!
//! # Live edits
//!
//! [`SceneLayoutDocument::take_live_edits`] turns what changed since the
//! last call into the live `scene_layer_*` commands, by layer id. The edits
//! of one gesture or field scrub (everything from [`SceneLayoutDocument::begin`]
//! to the next) send `continues: false` for a layer property the first
//! time and `true` after, so the server folds the whole gesture into one
//! undo step per layer. While an edit is open the draft leads; once every
//! change is sent and the edit is closed, the next replicated layout
//! becomes the draft again ([`SceneLayoutDocument::rebase`]).

use std::collections::BTreeSet;

use fm_protocol::{
    SceneLayerEdit as WireSceneLayerEdit, SceneLayerOperation, WireLayerCrop, WireLayerGeometry,
    WireLayerMask, WireSceneId, WireSceneLayerId, WireSceneLayerSource, WireSceneLayout,
};

use super::edits::SceneLayerEdit;
use super::geometry::{
    ANCHOR_MAX, CropInsets, LayerPlacement, MAX_LAYER_SIDE, MaskRect, Point, split_millidegrees,
};
use super::gesture::{ActiveGesture, millidegrees};

/// Most undo steps the draft keeps.
pub const MAX_UNDO_STEPS: usize = 200;

/// One layer of the draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesignLayer {
    pub id: WireSceneLayerId,
    pub name: String,
    pub source: WireSceneLayerSource,
    pub placement: LayerPlacement,
    pub opacity: u8,
    pub enabled: bool,
    /// Crop insets in source pixels (the source frame is the canvas size).
    pub crop: CropInsets,
    /// Rectangular mask in post-crop source pixels.
    pub mask: Option<MaskRect>,
    /// Drawing order key: layers draw by ascending z-order, ties in list
    /// order.
    pub z_order: i32,
}

/// What an undo step did, for the Undo and Redo tooltips.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditLabel {
    Move,
    Nudge,
    Scale,
    Rotate,
    Anchor,
    Crop,
    Field(Field),
    Reorder,
}

/// A numeric or switch property the inspector edits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Field {
    /// Footprint left edge (pan x), canvas pixels.
    X,
    /// Footprint top edge (pan y), canvas pixels.
    Y,
    /// Unturned width, pixels.
    Width,
    /// Unturned height, pixels.
    Height,
    /// Zoom: unturned width as a percentage of the canvas width; changing
    /// it scales both sides about the centre.
    Zoom,
    /// Clockwise rotation in degrees, to the millidegree; it turns about
    /// the anchor.
    Rotation,
    /// Anchor across the unturned width, basis points (`0..=10000`); the
    /// layer stays put.
    AnchorX,
    /// Anchor down the unturned height, basis points.
    AnchorY,
    /// Drawing order key.
    ZOrder,
    /// Opacity, `0..=255`.
    Opacity,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
    MaskX,
    MaskY,
    MaskWidth,
    MaskHeight,
    /// Whether the layer is drawn (1) or not (0).
    Enabled,
    /// Whether the layer has a mask (1) or not (0).
    Masked,
    /// Whether the mask keeps the outside (1) or the inside (0).
    MaskInverted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Step {
    label: EditLabel,
    before: Vec<DesignLayer>,
    after: Vec<DesignLayer>,
}

/// One open edit: the layers before it, so it can be committed as one
/// step or cancelled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Transaction {
    pub(super) label: EditLabel,
    pub(super) before: Vec<DesignLayer>,
}

/// One scene's layer layout under design.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLayoutDocument {
    scene: WireSceneId,
    canvas: (u32, u32),
    /// The layers as the server last reported them, bottom to top.
    base: Vec<DesignLayer>,
    /// The draft, bottom to top.
    pub(super) layers: Vec<DesignLayer>,
    /// Selected layers, in selection order.
    pub(super) selection: Vec<WireSceneLayerId>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    pub(super) transaction: Option<Transaction>,
    pub(super) gesture: Option<ActiveGesture>,
    /// Whether the server's layout changed under unsaved draft edits.
    base_changed: bool,
    /// The layers as last sent live (the base until anything is sent).
    sent: Vec<DesignLayer>,
    /// The layer properties the open gesture has sent, so later sends
    /// continue its undo step.
    live_keys: BTreeSet<(WireSceneLayerId, LiveProperty)>,
    /// Whether live edits were sent whose echo has not arrived yet (the
    /// server's layout has not matched [`Self::sent`] since).
    in_flight: bool,
}

/// A layer property one live edit sets.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum LiveProperty {
    Geometry,
    Appearance,
    Crop,
    Mask,
    ZOrder,
}

fn layers_of(layout: &WireSceneLayout) -> Vec<DesignLayer> {
    let canvas = (layout.width.max(1), layout.height.max(1));
    layout
        .layers
        .iter()
        .map(|outline| DesignLayer {
            id: outline.id,
            name: outline.name.clone(),
            source: outline.source,
            placement: LayerPlacement::new(
                outline.x,
                outline.y,
                outline.width.max(1),
                outline.height.max(1),
                outline.quarter_turns,
            )
            .with_tilt(outline.tilt_millidegrees)
            .with_anchor(outline.anchor_x, outline.anchor_y),
            opacity: outline.opacity,
            enabled: outline.enabled,
            crop: outline
                .crop
                .map_or_else(CropInsets::default, |crop| CropInsets {
                    left: crop.x,
                    top: crop.y,
                    right: canvas.0.saturating_sub(crop.x.saturating_add(crop.width)),
                    bottom: canvas.1.saturating_sub(crop.y.saturating_add(crop.height)),
                }),
            mask: outline.mask.map(|mask| MaskRect {
                x: mask.x,
                y: mask.y,
                width: mask.width,
                height: mask.height,
                inverted: mask.invert,
            }),
            z_order: outline.z_order,
        })
        .collect()
}

/// A layer's placement on the wire.
#[must_use]
pub fn wire_geometry(placement: LayerPlacement) -> WireLayerGeometry {
    WireLayerGeometry {
        x: placement.x,
        y: placement.y,
        width: placement.width,
        height: placement.height,
        quarter_turns: placement.quarter_turns % 4,
        tilt_millidegrees: placement.tilt_millidegrees,
        anchor_x: placement.anchor_x,
        anchor_y: placement.anchor_y,
    }
}

/// The least cropped size that keeps `mask` whole.
fn mask_extent(mask: Option<MaskRect>) -> (u32, u32) {
    mask.map_or((1, 1), |mask| {
        (
            mask.x.saturating_add(mask.width),
            mask.y.saturating_add(mask.height),
        )
    })
}

/// The moves (layer, destination index) that restack `from` into `to`'s
/// order: placing each layer from the bottom up, or from the top down,
/// whichever takes fewer moves (raising one layer to the top is one move
/// either way it is found).
fn restack_moves(from: &[DesignLayer], to: &[DesignLayer]) -> Vec<(WireSceneLayerId, usize)> {
    let start: Vec<WireSceneLayerId> = from.iter().map(|layer| layer.id).collect();
    let target: Vec<WireSceneLayerId> = to
        .iter()
        .map(|layer| layer.id)
        .filter(|id| start.contains(id))
        .collect();
    let pass = |indices: &mut dyn Iterator<Item = usize>| {
        let mut order = start.clone();
        let mut moves = Vec::new();
        for index in indices {
            let Some(id) = target.get(index) else {
                continue;
            };
            if let Some(position) = order.iter().position(|candidate| candidate == id)
                && position != index
            {
                let restacked = order.remove(position);
                order.insert(index, restacked);
                moves.push((*id, index));
            }
        }
        (moves, order)
    };
    let (upward, upward_order) = pass(&mut (0..target.len()));
    let (downward, downward_order) = pass(&mut (0..target.len()).rev());
    if downward.len() < upward.len() && downward_order == target {
        downward
    } else {
        debug_assert!(upward_order == target || start.len() != target.len());
        upward
    }
}

/// The live edits that turn `from` into `to` (by layer id): stacking moves
/// first, then each changed property. Each comes with the property it
/// sets, or `None` for a move.
fn live_operations(
    canvas: (u32, u32),
    from: &[DesignLayer],
    to: &[DesignLayer],
) -> Vec<(WireSceneLayerId, Option<LiveProperty>, SceneLayerOperation)> {
    let mut operations: Vec<(WireSceneLayerId, Option<LiveProperty>, SceneLayerOperation)> =
        restack_moves(from, to)
            .into_iter()
            .map(|(id, index)| {
                (
                    id,
                    None,
                    SceneLayerOperation::Move {
                        index: u16::try_from(index).unwrap_or(u16::MAX),
                    },
                )
            })
            .collect();
    for after in to {
        let Some(before) = from.iter().find(|layer| layer.id == after.id) else {
            continue;
        };
        let id = after.id;
        if before.placement != after.placement {
            operations.push((
                id,
                Some(LiveProperty::Geometry),
                SceneLayerOperation::SetGeometry(wire_geometry(after.placement)),
            ));
        }
        if before.enabled != after.enabled || before.opacity != after.opacity {
            operations.push((
                id,
                Some(LiveProperty::Appearance),
                SceneLayerOperation::SetAppearance {
                    enabled: after.enabled,
                    opacity: after.opacity,
                },
            ));
        }
        let wire_mask = |mask: Option<MaskRect>| {
            SceneLayerOperation::SetMask(mask.map(|mask| WireLayerMask {
                x: mask.x,
                y: mask.y,
                width: mask.width,
                height: mask.height,
                invert: mask.inverted,
            }))
        };
        let crop_changed = before.crop != after.crop;
        let mask_changed = before.mask != after.mask;
        // A mask must fit the crop it sits in: shrink the mask before a
        // crop that would cut it, grow it after.
        let mask_first = {
            let (width, height) = mask_extent(after.mask);
            let cropped = before.crop.cropped(canvas);
            width <= cropped.0 && height <= cropped.1
        };
        if mask_changed && mask_first {
            operations.push((id, Some(LiveProperty::Mask), wire_mask(after.mask)));
        }
        if crop_changed {
            operations.push((
                id,
                Some(LiveProperty::Crop),
                SceneLayerOperation::SetCrop(after.crop.rect(canvas).map(
                    |(x, y, width, height)| WireLayerCrop {
                        x,
                        y,
                        width,
                        height,
                    },
                )),
            ));
        }
        if mask_changed && !mask_first {
            operations.push((id, Some(LiveProperty::Mask), wire_mask(after.mask)));
        }
        if before.z_order != after.z_order {
            operations.push((
                id,
                Some(LiveProperty::ZOrder),
                SceneLayerOperation::SetZOrder {
                    z_order: after.z_order,
                },
            ));
        }
    }
    operations
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
fn to_u32(value: f64, max: u32) -> u32 {
    value.round().clamp(0.0, f64::from(max)) as u32
}

#[allow(clippy::cast_possible_truncation)] // Clamped first.
fn to_i32(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

impl SceneLayoutDocument {
    /// A clean draft of `scene`'s replicated `layout`.
    #[must_use]
    pub fn new(scene: WireSceneId, layout: &WireSceneLayout) -> Self {
        let base = layers_of(layout);
        Self {
            scene,
            canvas: (layout.width.max(1), layout.height.max(1)),
            layers: base.clone(),
            sent: base.clone(),
            base,
            selection: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            transaction: None,
            gesture: None,
            base_changed: false,
            live_keys: BTreeSet::new(),
            in_flight: false,
        }
    }

    /// Follows a newly replicated layout. A draft holding nothing unsent
    /// (every change sent live, or none made) with no edit open takes it
    /// as is, unless live edits are still in flight and the layout is not
    /// yet what they set (an echo of an earlier send: the draft keeps
    /// leading until the echo of the last arrives, or [`Self::settle`]);
    /// an open edit keeps leading; unsent offline edits are kept and flag
    /// [`Self::base_changed`].
    pub fn rebase(&mut self, layout: &WireSceneLayout) {
        let base = layers_of(layout);
        let canvas = (layout.width.max(1), layout.height.max(1));
        if base == self.base && canvas == self.canvas {
            return;
        }
        let settled = self.layers == self.sent;
        let idle = self.gesture.is_none() && self.transaction.is_none();
        self.base = base;
        self.canvas = canvas;
        if self.base == self.sent {
            self.in_flight = false;
        }
        if settled && idle && !self.in_flight {
            self.layers = self.base.clone();
            self.sent = self.base.clone();
            self.undo.clear();
            self.redo.clear();
            self.base_changed = false;
        } else if !settled && !self.in_flight && self.base != self.sent {
            // Not the echo of this draft's sends (those in flight, or all
            // of them landed): another editor changed the layout.
            self.base_changed = true;
        }
        self.retain_selection();
    }

    /// Takes the server's layout again when nothing is open or unsent (a
    /// live edit the server refused leaves no replicated change to follow).
    pub fn settle(&mut self) {
        if self.gesture.is_none() && self.transaction.is_none() && self.layers == self.sent {
            self.layers = self.base.clone();
            self.sent = self.base.clone();
            self.in_flight = false;
            self.base_changed = false;
            self.retain_selection();
        }
    }

    /// Discards the draft and every step: back to the server's layout.
    pub fn revert(&mut self) {
        self.layers = self.base.clone();
        self.sent = self.base.clone();
        self.undo.clear();
        self.redo.clear();
        self.transaction = None;
        self.gesture = None;
        self.base_changed = false;
        self.live_keys.clear();
        self.in_flight = false;
        self.retain_selection();
    }

    /// Whether the draft holds changes not yet sent live.
    #[must_use]
    pub fn has_unsent(&self) -> bool {
        self.layers != self.sent
    }

    /// The live commands for everything changed since the last call, in
    /// an order that applies cleanly. An open gesture's later sends
    /// continue its undo step.
    pub fn take_live_edits(&mut self) -> Vec<WireSceneLayerEdit> {
        if !self.has_unsent() {
            return Vec::new();
        }
        let operations = live_operations(self.canvas, &self.sent, &self.layers);
        self.sent = self.layers.clone();
        self.in_flight |= !operations.is_empty();
        operations
            .into_iter()
            .map(|(layer, property, operation)| {
                let continues =
                    property.is_some_and(|property| !self.live_keys.insert((layer, property)));
                WireSceneLayerEdit {
                    scene: self.scene,
                    layer,
                    operation,
                    continues,
                }
            })
            .collect()
    }

    /// The layers in drawing order: ascending z-order, ties in list order.
    #[must_use]
    pub fn draw_order(&self) -> Vec<&DesignLayer> {
        let mut layers: Vec<&DesignLayer> = self.layers.iter().collect();
        layers.sort_by_key(|layer| layer.z_order);
        layers
    }

    #[must_use]
    pub const fn scene(&self) -> WireSceneId {
        self.scene
    }

    /// The project canvas (and every source frame) size.
    #[must_use]
    pub const fn canvas(&self) -> (u32, u32) {
        self.canvas
    }

    /// The draft's layers, bottom to top.
    #[must_use]
    pub fn layers(&self) -> &[DesignLayer] {
        &self.layers
    }

    /// The server's layers, bottom to top.
    #[must_use]
    pub fn base(&self) -> &[DesignLayer] {
        &self.base
    }

    #[must_use]
    pub fn layer(&self, id: WireSceneLayerId) -> Option<&DesignLayer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    /// The layer's stacking position, `0` at the bottom.
    #[must_use]
    pub fn index_of(&self, id: WireSceneLayerId) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    /// Whether the draft differs from the server's layout.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.layers != self.base
    }

    /// Whether the server's layout changed under draft edits.
    #[must_use]
    pub const fn base_changed(&self) -> bool {
        self.base_changed
    }

    // Selection.

    #[must_use]
    pub fn selection(&self) -> &[WireSceneLayerId] {
        &self.selection
    }

    #[must_use]
    pub fn is_selected(&self, id: WireSceneLayerId) -> bool {
        self.selection.contains(&id)
    }

    /// Selects `id` alone, or (`additive`) toggles it in the selection.
    pub fn select(&mut self, id: WireSceneLayerId, additive: bool) {
        if self.layer(id).is_none() {
            return;
        }
        if additive {
            if let Some(position) = self.selection.iter().position(|candidate| *candidate == id) {
                self.selection.remove(position);
            } else {
                self.selection.push(id);
            }
        } else {
            self.selection = vec![id];
        }
    }

    /// Selects every layer, bottom to top.
    pub fn select_all(&mut self) {
        self.selection = self.layers.iter().map(|layer| layer.id).collect();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// The one selected layer, if exactly one is selected.
    #[must_use]
    pub fn single_selection(&self) -> Option<&DesignLayer> {
        match self.selection.as_slice() {
            [id] => self.layer(*id),
            _ => None,
        }
    }

    // Transactions and history.

    /// Opens one undo step; edits until [`Self::commit`] join it (and its
    /// live edits continue one server undo step).
    pub fn begin(&mut self, label: EditLabel) {
        if self.transaction.is_none() {
            self.live_keys.clear();
            self.transaction = Some(Transaction {
                label,
                before: self.layers.clone(),
            });
        }
    }

    /// Whether an edit (field scrub or gesture) is open.
    #[must_use]
    pub const fn editing(&self) -> bool {
        self.transaction.is_some()
    }

    /// Closes the open step; returns whether it changed anything (and so
    /// became one undo step).
    pub fn commit(&mut self) -> bool {
        let Some(transaction) = self.transaction.take() else {
            return false;
        };
        if transaction.before == self.layers {
            return false;
        }
        self.undo.push(Step {
            label: transaction.label,
            before: transaction.before,
            after: self.layers.clone(),
        });
        if self.undo.len() > MAX_UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
        true
    }

    /// Abandons the open step, restoring the layers before it.
    pub fn cancel(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            self.layers = transaction.before;
        }
        self.gesture = None;
    }

    /// The label of the step Undo would revert.
    #[must_use]
    pub fn undo_label(&self) -> Option<EditLabel> {
        self.undo.last().map(|step| step.label)
    }

    /// The label of the step Redo would reapply.
    #[must_use]
    pub fn redo_label(&self) -> Option<EditLabel> {
        self.redo.last().map(|step| step.label)
    }

    /// Reverts the latest step; `false` with nothing to undo or an edit open.
    pub fn undo(&mut self) -> bool {
        if self.transaction.is_some() {
            return false;
        }
        let Some(step) = self.undo.pop() else {
            return false;
        };
        self.layers.clone_from(&step.before);
        self.redo.push(step);
        self.retain_selection();
        true
    }

    /// Reapplies the latest undone step.
    pub fn redo(&mut self) -> bool {
        if self.transaction.is_some() {
            return false;
        }
        let Some(step) = self.redo.pop() else {
            return false;
        };
        self.layers.clone_from(&step.after);
        self.undo.push(step);
        self.retain_selection();
        true
    }

    fn retain_selection(&mut self) {
        let layers = &self.layers;
        self.selection
            .retain(|id| layers.iter().any(|layer| layer.id == *id));
    }

    // Field edits.

    /// The value `field` shows for layer `id`.
    #[must_use]
    pub fn field_value(&self, id: WireSceneLayerId, field: Field) -> Option<f64> {
        let layer = self.layer(id)?;
        let cropped = layer.crop.cropped(self.canvas);
        let mask = layer.mask;
        let placement = layer.placement;
        Some(match field {
            Field::X => f64::from(placement.x),
            Field::Y => f64::from(placement.y),
            Field::Width => f64::from(placement.width),
            Field::Height => f64::from(placement.height),
            Field::Zoom => f64::from(placement.width) * 100.0 / f64::from(self.canvas.0),
            Field::Rotation => placement.angle_degrees().rem_euclid(360.0),
            Field::AnchorX => f64::from(placement.anchor_x),
            Field::AnchorY => f64::from(placement.anchor_y),
            Field::ZOrder => f64::from(layer.z_order),
            Field::Opacity => f64::from(layer.opacity),
            Field::CropLeft => f64::from(layer.crop.left),
            Field::CropTop => f64::from(layer.crop.top),
            Field::CropRight => f64::from(layer.crop.right),
            Field::CropBottom => f64::from(layer.crop.bottom),
            Field::MaskX => f64::from(mask.map_or(0, |mask| mask.x)),
            Field::MaskY => f64::from(mask.map_or(0, |mask| mask.y)),
            Field::MaskWidth => f64::from(mask.map_or(cropped.0, |mask| mask.width)),
            Field::MaskHeight => f64::from(mask.map_or(cropped.1, |mask| mask.height)),
            Field::Enabled => f64::from(u8::from(layer.enabled)),
            Field::Masked => f64::from(u8::from(mask.is_some())),
            Field::MaskInverted => f64::from(u8::from(mask.is_some_and(|mask| mask.inverted))),
        })
    }

    /// Sets `field` to `value` on every selected layer (clamped to what the
    /// layer can store). Joins the open step, or makes one step of its own
    /// when none is open. Returns whether anything changed.
    pub fn set_field(&mut self, field: Field, value: f64) -> bool {
        let own_step = self.transaction.is_none();
        if own_step {
            self.begin(EditLabel::Field(field));
        }
        let canvas = self.canvas;
        let selection = self.selection.clone();
        let mut changed = false;
        for id in selection {
            let Some(layer) = self.layers.iter_mut().find(|layer| layer.id == id) else {
                continue;
            };
            let before = layer.clone();
            apply_field(layer, field, value, canvas);
            changed |= *layer != before;
        }
        if own_step {
            self.commit();
        }
        changed
    }

    /// Moves every selected layer by whole pixels as one step.
    pub fn nudge(&mut self, dx: i32, dy: i32) -> bool {
        if self.selection.is_empty() || self.transaction.is_some() {
            return false;
        }
        self.begin(EditLabel::Nudge);
        for id in self.selection.clone() {
            if let Some(layer) = self.layers.iter_mut().find(|layer| layer.id == id) {
                layer.placement.x = layer.placement.x.saturating_add(dx);
                layer.placement.y = layer.placement.y.saturating_add(dy);
            }
        }
        self.commit()
    }

    // Stacking order.

    /// Moves layer `id` to stacking position `to` (`0` = bottom) as one step.
    pub fn move_layer(&mut self, id: WireSceneLayerId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let to = to.min(self.layers.len().saturating_sub(1));
        if from == to || self.transaction.is_some() {
            return false;
        }
        self.begin(EditLabel::Reorder);
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
        self.commit()
    }

    /// Raises (`up`) or lowers every selected layer by one position past
    /// its unselected neighbour, as one step; the selection keeps its
    /// relative order.
    pub fn shift_selection(&mut self, up: bool) -> bool {
        if self.selection.is_empty() || self.transaction.is_some() {
            return false;
        }
        self.begin(EditLabel::Reorder);
        let selected: Vec<bool> = self
            .layers
            .iter()
            .map(|layer| self.selection.contains(&layer.id))
            .collect();
        let count = self.layers.len();
        let mut flags = selected;
        if up {
            for index in (0..count.saturating_sub(1)).rev() {
                if flags[index] && !flags[index + 1] {
                    self.layers.swap(index, index + 1);
                    flags.swap(index, index + 1);
                }
            }
        } else {
            for index in 1..count {
                if flags[index] && !flags[index - 1] {
                    self.layers.swap(index, index - 1);
                    flags.swap(index, index - 1);
                }
            }
        }
        self.commit()
    }

    /// Moves the selection to the top (`up`) or bottom of the stack as one
    /// step, keeping its relative order.
    pub fn send_selection_to_end(&mut self, up: bool) -> bool {
        if self.selection.is_empty() || self.transaction.is_some() {
            return false;
        }
        self.begin(EditLabel::Reorder);
        let (selected, rest): (Vec<DesignLayer>, Vec<DesignLayer>) = self
            .layers
            .drain(..)
            .partition(|layer| self.selection.contains(&layer.id));
        self.layers = if up {
            rest.into_iter().chain(selected).collect()
        } else {
            selected.into_iter().chain(rest).collect()
        };
        self.commit()
    }

    /// Whether any selected layer can still go up (`up`) or down.
    #[must_use]
    pub fn can_shift_selection(&self, up: bool) -> bool {
        let flags: Vec<bool> = self
            .layers
            .iter()
            .map(|layer| self.selection.contains(&layer.id))
            .collect();
        if up {
            flags.windows(2).any(|pair| pair[0] && !pair[1])
        } else {
            flags.windows(2).any(|pair| !pair[0] && pair[1])
        }
    }

    // The draft as commands.

    /// The edits that turn the server's layout into the draft, in an order
    /// that applies cleanly: stacking moves first (by the server's layer
    /// indices as they shift), then each changed layer's geometry,
    /// appearance, mask clear, crop, and mask, by final index.
    #[must_use]
    pub fn edits(&self) -> Vec<SceneLayerEdit> {
        let mut edits = Vec::new();
        let mut order: Vec<WireSceneLayerId> = self.base.iter().map(|layer| layer.id).collect();
        let target: Vec<WireSceneLayerId> = self
            .layers
            .iter()
            .map(|layer| layer.id)
            .filter(|id| order.contains(id))
            .collect();
        for (to, id) in target.iter().enumerate() {
            if let Some(from) = order.iter().position(|candidate| candidate == id)
                && from != to
            {
                let moved = order.remove(from);
                order.insert(to, moved);
                edits.push(SceneLayerEdit::Move { from, to });
            }
        }
        for (index, id) in order.iter().enumerate() {
            let (Some(before), Some(after)) = (
                self.base.iter().find(|layer| layer.id == *id),
                self.layer(*id),
            ) else {
                continue;
            };
            if before.placement != after.placement {
                edits.push(SceneLayerEdit::Geometry {
                    index,
                    placement: after.placement,
                });
            }
            if before.z_order != after.z_order {
                edits.push(SceneLayerEdit::ZOrder {
                    index,
                    z_order: after.z_order,
                });
            }
            if before.opacity != after.opacity || before.enabled != after.enabled {
                edits.push(SceneLayerEdit::Appearance {
                    index,
                    enabled: after.enabled,
                    opacity: after.opacity,
                });
            }
            let crop_changed = before.crop != after.crop;
            let mask_changed = before.mask != after.mask;
            if before.mask.is_some() && (crop_changed || mask_changed) {
                edits.push(SceneLayerEdit::Mask { index, mask: None });
            }
            if crop_changed {
                edits.push(SceneLayerEdit::Crop {
                    index,
                    rect: after.crop.rect(self.canvas),
                });
            }
            if let Some(mask) = after.mask
                && (mask_changed || (crop_changed && before.mask.is_some()))
            {
                edits.push(SceneLayerEdit::Mask {
                    index,
                    mask: Some(mask),
                });
            }
        }
        edits
    }

    /// The draft as `freemix-cli` commands for the project bundle at
    /// `project`, one command per line.
    #[must_use]
    pub fn cli_script(&self, project: &str) -> String {
        self.edits()
            .iter()
            .map(|edit| edit.cli_command(project, self.scene))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every layer's whole design as edit commands, by index: its
    /// geometry, appearance, mask clear, crop, mask, and z-order. Applied
    /// to a scene with the same layers in the same order, they reproduce
    /// the draft's layout.
    #[must_use]
    pub fn layout_edits(&self) -> Vec<SceneLayerEdit> {
        let mut edits = Vec::new();
        for (index, layer) in self.layers.iter().enumerate() {
            edits.push(SceneLayerEdit::Geometry {
                index,
                placement: layer.placement,
            });
            edits.push(SceneLayerEdit::Appearance {
                index,
                enabled: layer.enabled,
                opacity: layer.opacity,
            });
            edits.push(SceneLayerEdit::Mask { index, mask: None });
            edits.push(SceneLayerEdit::Crop {
                index,
                rect: layer.crop.rect(self.canvas),
            });
            if layer.mask.is_some() {
                edits.push(SceneLayerEdit::Mask {
                    index,
                    mask: layer.mask,
                });
            }
            edits.push(SceneLayerEdit::ZOrder {
                index,
                z_order: layer.z_order,
            });
        }
        edits
    }

    /// The draft's whole layout as `freemix-cli` commands for the project
    /// bundle at `project`, one command per line ([`Self::layout_edits`]).
    #[must_use]
    pub fn layout_script(&self, project: &str) -> String {
        self.layout_edits()
            .iter()
            .map(|edit| edit.cli_command(project, self.scene))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The draft layer's centre on the canvas.
    #[must_use]
    pub fn centre(&self, id: WireSceneLayerId) -> Option<Point> {
        self.layer(id).map(|layer| layer.placement.frame().centre)
    }
}

/// Applies one field value to `layer` on a `canvas`.
#[allow(clippy::too_many_lines)] // one arm per inspector field
fn apply_field(layer: &mut DesignLayer, field: Field, value: f64, canvas: (u32, u32)) {
    let placement = layer.placement;
    let anchor = (placement.anchor_x, placement.anchor_y);
    match field {
        Field::X => layer.placement.x = to_i32(value),
        Field::Y => layer.placement.y = to_i32(value),
        Field::Width => {
            layer.placement.width = to_u32(value, MAX_LAYER_SIDE).max(1);
        }
        Field::Height => {
            layer.placement.height = to_u32(value, MAX_LAYER_SIDE).max(1);
        }
        Field::Zoom => {
            let width = f64::from(canvas.0) * value.max(0.0) / 100.0;
            let scale = width / f64::from(placement.width);
            layer.placement = LayerPlacement::pivoted(
                placement.anchor_point(),
                f64::from(placement.width) * scale,
                f64::from(placement.height) * scale,
                placement.rotation_millidegrees(),
                anchor,
            );
        }
        Field::Rotation => {
            let (turns, tilt) = split_millidegrees(millidegrees(value));
            if (turns, tilt) != (placement.quarter_turns % 4, placement.tilt_millidegrees) {
                layer.placement = LayerPlacement::pivoted(
                    placement.anchor_point(),
                    f64::from(placement.width),
                    f64::from(placement.height),
                    millidegrees(value),
                    anchor,
                );
            }
        }
        Field::AnchorX | Field::AnchorY => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
            let basis_points = value.round().clamp(0.0, f64::from(ANCHOR_MAX)) as u16;
            let anchor = if field == Field::AnchorX {
                (basis_points, anchor.1)
            } else {
                (anchor.0, basis_points)
            };
            if anchor != (placement.anchor_x, placement.anchor_y) {
                layer.placement = placement.reanchored(anchor);
            }
        }
        Field::ZOrder => {
            #[allow(clippy::cast_possible_truncation)] // Clamped first.
            let z_order = value
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
            layer.z_order = z_order;
        }
        Field::Opacity => layer.opacity = u8::try_from(to_u32(value, 255)).unwrap_or(u8::MAX),
        Field::CropLeft | Field::CropTop | Field::CropRight | Field::CropBottom => {
            let edge = match field {
                Field::CropLeft => super::geometry::Edge::Left,
                Field::CropTop => super::geometry::Edge::Top,
                Field::CropRight => super::geometry::Edge::Right,
                _ => super::geometry::Edge::Bottom,
            };
            layer.crop.set_keeping(
                edge,
                to_u32(value, canvas.0.max(canvas.1)),
                canvas,
                mask_extent(layer.mask),
            );
        }
        Field::MaskX | Field::MaskY | Field::MaskWidth | Field::MaskHeight => {
            let cropped = layer.crop.cropped(canvas);
            let mut mask = layer.mask.unwrap_or(MaskRect {
                x: 0,
                y: 0,
                width: cropped.0,
                height: cropped.1,
                inverted: false,
            });
            let value = to_u32(value, cropped.0.max(cropped.1));
            match field {
                Field::MaskX => mask.x = value,
                Field::MaskY => mask.y = value,
                Field::MaskWidth => mask.width = value,
                _ => mask.height = value,
            }
            layer.mask = Some(mask.clamped(cropped));
        }
        Field::Enabled => layer.enabled = value >= 0.5,
        Field::Masked => {
            let cropped = layer.crop.cropped(canvas);
            layer.mask = (value >= 0.5).then(|| {
                layer.mask.unwrap_or(MaskRect {
                    x: cropped.0 / 4,
                    y: cropped.1 / 4,
                    width: (cropped.0 / 2).max(1),
                    height: (cropped.1 / 2).max(1),
                    inverted: false,
                })
            });
        }
        Field::MaskInverted => {
            if let Some(mask) = &mut layer.mask {
                mask.inverted = value >= 0.5;
            }
        }
    }
}
