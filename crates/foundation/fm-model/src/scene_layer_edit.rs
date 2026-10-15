//! Live scene layer edits (SW-006): one layer of a scene, addressed by its
//! stable [`LayerId`], changed in place on a scene whose sources are
//! `canvas_width` x `canvas_height` (the project canvas).
//!
//! Every [`SceneLayerChange`] is checked as a whole before the scene changes,
//! so a refused change leaves it exactly as it was, and every accepted change
//! has an exact inverse ([`Scene::layer_change_inverse`]) computed from the
//! scene before it. A layer's source references and cycles are the
//! project's to check (they span scenes and inputs); everything about the
//! scene itself is checked here.
//!
//! Removing a layer is refused while the scene's animation has a track for
//! it ([`SceneLayerEditError::LayerAnimated`]), so its inverse (adding the
//! same layer back at the same position) restores the scene exactly.

use core::fmt;

use crate::{
    CropRect, Layer, LayerEffect, LayerEffectError, LayerGeometry, LayerId, RectMask, Scene,
};

/// One change to one scene layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneLayerChange {
    /// Replaces the layer's geometry (box, quarter turns, tilt, anchor).
    Geometry(LayerGeometry),
    /// Sets whether the layer is drawn and its opacity.
    Appearance { enabled: bool, opacity: u8 },
    /// Sets (`Some`) or clears the crop, in source pixels.
    Crop(Option<CropRect>),
    /// Sets (`Some`) or clears the mask, in post-crop source pixels.
    Mask(Option<RectMask>),
    /// Sets the compositor z-order.
    ZOrder(i32),
    /// Moves the layer to stacking (list) position `index`.
    Move { index: usize },
    /// Inserts `layer` (whose id is the edited id) at list position `index`.
    Add { index: usize, layer: Box<Layer> },
    /// Removes the layer.
    Remove,
}

/// Why a [`SceneLayerChange`] was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneLayerEditError {
    /// The scene has no layer with this id.
    UnknownLayer(LayerId),
    /// An added layer's id is already taken, or differs from the edited id.
    DuplicateLayer(LayerId),
    /// A move or add position beyond the layer list.
    IndexOutOfRange { index: usize, length: usize },
    /// An added layer has a blank name.
    EmptyName,
    /// A zero or oversized side, a tilt outside its stored range, or an
    /// anchor outside the box.
    InvalidGeometry,
    /// An empty crop or one beyond the source.
    InvalidCrop,
    /// The new crop would leave the layer's mask outside it.
    CropWouldInvalidateMask,
    /// An empty mask or one beyond the cropped source.
    InvalidMask,
    /// An added layer carries more than [`LayerEffect::MAX_EFFECTS_PER_LAYER`]
    /// effects.
    TooManyEffects,
    /// One effect of an added layer breaks the descriptor bounds.
    InvalidEffect {
        effect_index: usize,
        reason: LayerEffectError,
    },
    /// The scene's animation has a track for the layer being removed.
    LayerAnimated(LayerId),
}

impl SceneLayerEditError {
    /// A stable code for receipts.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownLayer(_) => "unknown_layer",
            Self::DuplicateLayer(_) => "duplicate_layer",
            Self::IndexOutOfRange { .. } => "out_of_range",
            Self::EmptyName => "invalid_name",
            Self::InvalidGeometry => "invalid_geometry",
            Self::InvalidCrop => "invalid_crop",
            Self::CropWouldInvalidateMask => "crop_invalidates_mask",
            Self::InvalidMask => "invalid_mask",
            Self::TooManyEffects | Self::InvalidEffect { .. } => "invalid_effect",
            Self::LayerAnimated(_) => "layer_animated",
        }
    }
}

impl fmt::Display for SceneLayerEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLayer(layer) => write!(formatter, "unknown scene layer {layer}"),
            Self::DuplicateLayer(layer) => {
                write!(formatter, "scene layer id {layer} is already taken")
            }
            Self::IndexOutOfRange { index, length } => write!(
                formatter,
                "layer position {index} is out of range for {length} layers"
            ),
            Self::EmptyName => formatter.write_str("layer name must not be blank"),
            Self::InvalidGeometry => formatter.write_str(
                "layer geometry needs nonzero sides within the video limits, a tilt in \
                 -45000..45000 millidegrees, and an anchor inside the box",
            ),
            Self::InvalidCrop => formatter.write_str("crop must be nonempty and inside the source"),
            Self::CropWouldInvalidateMask => {
                formatter.write_str("the crop would leave the layer's mask outside it")
            }
            Self::InvalidMask => {
                formatter.write_str("mask must be nonempty and inside the cropped source")
            }
            Self::TooManyEffects => write!(
                formatter,
                "a layer carries at most {} effects",
                LayerEffect::MAX_EFFECTS_PER_LAYER
            ),
            Self::InvalidEffect {
                effect_index,
                reason,
            } => write!(formatter, "effect {effect_index}: {reason}"),
            Self::LayerAnimated(layer) => write!(
                formatter,
                "scene layer {layer} is animated; delete its animation tracks first"
            ),
        }
    }
}

impl std::error::Error for SceneLayerEditError {}

fn rect_fits(x: u32, y: u32, width: u32, height: u32, limit: (u32, u32)) -> bool {
    width != 0
        && height != 0
        && x.checked_add(width).is_some_and(|right| right <= limit.0)
        && y.checked_add(height)
            .is_some_and(|bottom| bottom <= limit.1)
}

/// The size of `layer`'s cropped source on a `canvas`.
fn cropped_size(crop: Option<CropRect>, canvas: (u32, u32)) -> (u32, u32) {
    crop.map_or(canvas, |crop| (crop.width, crop.height))
}

fn check_crop(crop: CropRect, canvas: (u32, u32)) -> Result<(), SceneLayerEditError> {
    if rect_fits(crop.x, crop.y, crop.width, crop.height, canvas) {
        Ok(())
    } else {
        Err(SceneLayerEditError::InvalidCrop)
    }
}

fn check_mask(mask: RectMask, source: (u32, u32)) -> Result<(), SceneLayerEditError> {
    if rect_fits(mask.x, mask.y, mask.width, mask.height, source) {
        Ok(())
    } else {
        Err(SceneLayerEditError::InvalidMask)
    }
}

/// Checks a whole layer as an add stores it on a `canvas`.
///
/// # Errors
///
/// [`SceneLayerEditError::EmptyName`], [`SceneLayerEditError::InvalidGeometry`],
/// [`SceneLayerEditError::InvalidCrop`], [`SceneLayerEditError::InvalidMask`],
/// [`SceneLayerEditError::TooManyEffects`], or
/// [`SceneLayerEditError::InvalidEffect`].
pub fn check_scene_layer(layer: &Layer, canvas: (u32, u32)) -> Result<(), SceneLayerEditError> {
    if layer.name.trim().is_empty() {
        return Err(SceneLayerEditError::EmptyName);
    }
    if !layer.geometry.is_storable() {
        return Err(SceneLayerEditError::InvalidGeometry);
    }
    if let Some(crop) = layer.crop {
        check_crop(crop, canvas)?;
    }
    if let Some(mask) = layer.mask {
        check_mask(mask, cropped_size(layer.crop, canvas))?;
    }
    if layer.effects.len() > LayerEffect::MAX_EFFECTS_PER_LAYER {
        return Err(SceneLayerEditError::TooManyEffects);
    }
    for (effect_index, effect) in layer.effects.iter().enumerate() {
        effect
            .validate()
            .map_err(|reason| SceneLayerEditError::InvalidEffect {
                effect_index,
                reason,
            })?;
    }
    Ok(())
}

impl Scene {
    /// The list position of layer `id`.
    #[must_use]
    pub fn layer_index(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    /// Applies `change` to layer `id` (for [`SceneLayerChange::Add`], the id
    /// of the new layer) on a `canvas_width` x `canvas_height` source.
    ///
    /// # Errors
    ///
    /// [`SceneLayerEditError`]; the scene is unchanged on error.
    pub fn apply_layer_change(
        &mut self,
        id: LayerId,
        change: &SceneLayerChange,
        canvas_width: u32,
        canvas_height: u32,
    ) -> Result<(), SceneLayerEditError> {
        let canvas = (canvas_width, canvas_height);
        if let SceneLayerChange::Add { index, layer } = change {
            if layer.id != id || self.layer_by_id(id).is_some() {
                return Err(SceneLayerEditError::DuplicateLayer(id));
            }
            let length = self.layers.len();
            if *index > length {
                return Err(SceneLayerEditError::IndexOutOfRange {
                    index: *index,
                    length,
                });
            }
            check_scene_layer(layer, canvas)?;
            self.layers.insert(*index, (**layer).clone());
            return Ok(());
        }
        let position = self
            .layer_index(id)
            .ok_or(SceneLayerEditError::UnknownLayer(id))?;
        let length = self.layers.len();
        let layer = &mut self.layers[position];
        match change {
            SceneLayerChange::Geometry(geometry) => {
                if !geometry.is_storable() {
                    return Err(SceneLayerEditError::InvalidGeometry);
                }
                layer.geometry = *geometry;
            }
            SceneLayerChange::Appearance { enabled, opacity } => {
                layer.enabled = *enabled;
                layer.opacity = *opacity;
            }
            SceneLayerChange::Crop(crop) => {
                if let Some(crop) = crop {
                    check_crop(*crop, canvas)?;
                }
                if let Some(mask) = layer.mask
                    && check_mask(mask, cropped_size(*crop, canvas)).is_err()
                {
                    return Err(SceneLayerEditError::CropWouldInvalidateMask);
                }
                layer.crop = *crop;
            }
            SceneLayerChange::Mask(mask) => {
                if let Some(mask) = mask {
                    check_mask(*mask, cropped_size(layer.crop, canvas))?;
                }
                layer.mask = *mask;
            }
            SceneLayerChange::ZOrder(z_order) => layer.z_order = *z_order,
            SceneLayerChange::Move { index } => {
                if *index >= length {
                    return Err(SceneLayerEditError::IndexOutOfRange {
                        index: *index,
                        length,
                    });
                }
                let moved = self.layers.remove(position);
                self.layers.insert(*index, moved);
            }
            SceneLayerChange::Remove => {
                if self
                    .animation
                    .clip
                    .tracks()
                    .iter()
                    .any(|track| track.key.target == id)
                {
                    return Err(SceneLayerEditError::LayerAnimated(id));
                }
                self.layers.remove(position);
            }
            SceneLayerChange::Add { .. } => unreachable!("handled above"),
        }
        Ok(())
    }

    /// The change that restores layer `id` as it is now after `change`, or
    /// `None` when `change` targets no layer (an add of a taken id, or an
    /// edit of a missing layer).
    #[must_use]
    pub fn layer_change_inverse(
        &self,
        id: LayerId,
        change: &SceneLayerChange,
    ) -> Option<SceneLayerChange> {
        if matches!(change, SceneLayerChange::Add { .. }) {
            return self
                .layer_by_id(id)
                .is_none()
                .then_some(SceneLayerChange::Remove);
        }
        let index = self.layer_index(id)?;
        let layer = &self.layers[index];
        Some(match change {
            SceneLayerChange::Geometry(_) => SceneLayerChange::Geometry(layer.geometry),
            SceneLayerChange::Appearance { .. } => SceneLayerChange::Appearance {
                enabled: layer.enabled,
                opacity: layer.opacity,
            },
            SceneLayerChange::Crop(_) => SceneLayerChange::Crop(layer.crop),
            SceneLayerChange::Mask(_) => SceneLayerChange::Mask(layer.mask),
            SceneLayerChange::ZOrder(_) => SceneLayerChange::ZOrder(layer.z_order),
            SceneLayerChange::Move { .. } => SceneLayerChange::Move { index },
            SceneLayerChange::Remove => SceneLayerChange::Add {
                index,
                layer: Box::new(layer.clone()),
            },
            SceneLayerChange::Add { .. } => unreachable!("handled above"),
        })
    }
}
