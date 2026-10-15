//! Scene layer animation (GX-011 stage 5): one [`fm_animation::Clip`] per
//! scene, keyed by stable [`LayerId`], with the same In/Out phase model as
//! title animations.
//!
//! A [`Scene`] carries a [`SceneAnimation`]: an In and an Out phase in whole
//! frames of the clip's own timebase (30/1 by default) and a revision that
//! advances on every applied edit. Tracks animate one property of one layer
//! in one phase; a layer or property without a track keeps its static value.
//! Scene clips animate every [`Property`] except [`Property::Color`] (layers
//! have no fill colour): [`SCENE_ANIMATION_PROPERTIES`].
//!
//! [`Scene::posed`] samples the clip and returns the scene as the compositor
//! draws it at that point of its life. Everything is integer arithmetic and
//! rounded once, half up, so every renderer poses a frame identically:
//!
//! * `ScaleX`/`ScaleY` (basis points) scale the layer's box
//!   (`geometry.width`/`height`, before its quarter-turn rotation) about the
//!   centre of its on-screen footprint.
//! * `Rotation` (millidegrees, clockwise) is added to the static quarter
//!   turn and realized at the nearest quarter turn (half up): the compositor
//!   transform supports quarter turns only, so a rotation key between quarter
//!   turns snaps rather than drawing a tilted layer. The footprint turns
//!   about its centre.
//! * `PositionX`/`PositionY` add pixels to the footprint's top-left corner.
//! * `CropLeft`/`Top`/`Right`/`Bottom` hide that many pixels of the scaled
//!   box from each of its edges (in the layer's own frame, before rotation);
//!   the source region shrinks in proportion (rounded down at each edge), so
//!   the remaining picture does not stretch. A mask follows the cropped
//!   source and is clipped to it.
//! * `Opacity` (`0..=255`) multiplies into the static opacity.
//!
//! A layer scaled to nothing, cropped away, or whose mask no longer covers
//! any of the cropped source is disabled in the posed scene.
//!
//! On air (see `fm_engine`), a Scene input plays its In phase when it reaches
//! Program (a cut or the first frame of a transition) or Preview, holds the
//! In phase's last frame afterwards, and plays its Out phase while a timed or
//! manual transition takes it off Program. A cut has no frame to play Out on.
//! A scene shown any other way (nested, multiview, overlay) holds.

use core::{fmt, num::NonZeroU32};

use fm_animation::{
    AnimationPreset, Clip, ClipError, ClipState, Phase, PresetGeometry, Property, Timebase, Value,
    round_half_up,
};
use fm_types::FrameRate;

use crate::{CropRect, Layer, LayerGeometry, RectMask, Rotation, Scene};

/// The stable id of one layer within its scene: the target of every scene
/// animation track. Unique within a scene and kept by every layer edit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LayerId(NonZeroU32);

impl LayerId {
    /// The id `value`, or `None` for zero.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn from_non_zero(value: NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    #[must_use]
    pub const fn non_zero(self) -> NonZeroU32 {
        self.0
    }
}

impl fmt::Display for LayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The clip type every scene animation uses.
pub type SceneClip = Clip<LayerId>;

/// Every property a scene layer animates: all but [`Property::Color`].
pub const SCENE_ANIMATION_PROPERTIES: [Property; 10] = [
    Property::PositionX,
    Property::PositionY,
    Property::ScaleX,
    Property::ScaleY,
    Property::Rotation,
    Property::Opacity,
    Property::CropLeft,
    Property::CropTop,
    Property::CropRight,
    Property::CropBottom,
];

/// Whether a scene layer can animate `property`.
#[must_use]
pub fn scene_animates(property: Property) -> bool {
    SCENE_ANIMATION_PROPERTIES.contains(&property)
}

/// The timebase a new scene animation is authored in: 30 frames per second.
#[must_use]
pub fn default_scene_timebase() -> Timebase {
    Timebase::new(FrameRate::new(30, 1).unwrap_or_else(|_| unreachable!("30/1 is a valid rate")))
}

/// A scene's keyframe animation and its edit revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneAnimation {
    pub clip: SceneClip,
    /// Advances once per applied edit; an optimistic-concurrency counter.
    pub revision: u64,
}

impl Default for SceneAnimation {
    /// An empty clip of zero-length phases at [`default_scene_timebase`].
    fn default() -> Self {
        Self::empty(default_scene_timebase())
    }
}

impl SceneAnimation {
    /// An empty clip of zero-length phases at `timebase`, revision zero.
    #[must_use]
    pub fn empty(timebase: Timebase) -> Self {
        Self {
            clip: Clip::new(timebase, 0, 0)
                .unwrap_or_else(|_| unreachable!("an empty clip is valid")),
            revision: 0,
        }
    }

    #[must_use]
    pub const fn new(clip: SceneClip, revision: u64) -> Self {
        Self { clip, revision }
    }

    /// Whether no layer is animated.
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.clip.tracks().is_empty()
    }

    /// Whether this is the default animation: no tracks, zero-length phases
    /// at [`default_scene_timebase`], revision zero.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Why a scene animation (or an edit of it) was refused. A refused edit
/// changes nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneAnimationError {
    /// The scene has no layer with this id.
    UnknownLayer(LayerId),
    /// Scene layers cannot animate this property.
    UnsupportedProperty { layer: LayerId, property: Property },
    /// The clip refused the edit.
    Clip(ClipError<LayerId>),
    /// The revision counter is exhausted.
    RevisionExhausted,
}

impl fmt::Display for SceneAnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLayer(layer) => write!(formatter, "unknown scene layer {layer}"),
            Self::UnsupportedProperty { layer, property } => write!(
                formatter,
                "scene layer {layer} cannot animate {}",
                property.id()
            ),
            Self::Clip(error) => write!(formatter, "scene animation edit refused: {error}"),
            Self::RevisionExhausted => formatter.write_str("scene animation revision is exhausted"),
        }
    }
}

impl std::error::Error for SceneAnimationError {}

impl From<ClipError<LayerId>> for SceneAnimationError {
    fn from(error: ClipError<LayerId>) -> Self {
        Self::Clip(error)
    }
}

impl Scene {
    /// The layer with stable id `id`.
    #[must_use]
    pub fn layer_by_id(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    /// The smallest unused layer id above every id in the scene, wrapping to
    /// the lowest unused id after `u32::MAX`; `None` only when every id is
    /// taken.
    #[must_use]
    pub fn next_layer_id(&self) -> Option<LayerId> {
        let highest = self.layers.iter().map(|layer| layer.id.get()).max();
        let taken = |value: u32| self.layers.iter().any(|layer| layer.id.get() == value);
        highest
            .and_then(|highest| highest.checked_add(1))
            .or(highest.is_none().then_some(1))
            .and_then(LayerId::new)
            .or_else(|| {
                (1..=u32::MAX)
                    .find(|value| !taken(*value))
                    .and_then(LayerId::new)
            })
    }

    /// Checks that `clip` animates only this scene's layers and only
    /// [`SCENE_ANIMATION_PROPERTIES`], and passes every clip bound.
    ///
    /// # Errors
    ///
    /// The first violation found.
    pub fn check_animation_clip(&self, clip: &SceneClip) -> Result<(), SceneAnimationError> {
        clip.validate()?;
        for track in clip.tracks() {
            let layer = track.key.target;
            if self.layer_by_id(layer).is_none() {
                return Err(SceneAnimationError::UnknownLayer(layer));
            }
            if !scene_animates(track.key.property) {
                return Err(SceneAnimationError::UnsupportedProperty {
                    layer,
                    property: track.key.property,
                });
            }
        }
        Ok(())
    }

    /// Applies a built-in preset to one layer over `frames` frames of
    /// `phase`, growing the phase to `frames` when it is shorter (keys
    /// already in the phase stay where they are). Off-canvas distances and
    /// the wipe extent come from the layer's static footprint and the
    /// `canvas_width` x `canvas_height` project canvas. Atomic; returns the
    /// new animation revision.
    ///
    /// # Errors
    ///
    /// [`SceneAnimationError`]; nothing changes on error.
    pub fn apply_animation_preset(
        &mut self,
        layer: LayerId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
        canvas_width: u32,
        canvas_height: u32,
    ) -> Result<u64, SceneAnimationError> {
        let geometry = self
            .layer_by_id(layer)
            .map(|layer| layer.geometry)
            .ok_or(SceneAnimationError::UnknownLayer(layer))?;
        let (width, height) = footprint(geometry.width, geometry.height, geometry.rotation);
        let geometry = PresetGeometry {
            x: geometry.translation_x,
            y: geometry.translation_y,
            width,
            height,
            canvas_width,
            canvas_height,
        };
        self.edit_animation(|clip| {
            let length = clip.phase_frames(phase).max(frames);
            clip.set_phase_frames_keeping_keys(phase, length)?;
            clip.apply_preset(layer, phase, preset, frames, geometry)
        })
    }

    /// Scales every key of `phase` to a new phase length (see
    /// [`fm_animation::Clip::stretch_phase`]). Atomic; returns the new
    /// animation revision.
    ///
    /// # Errors
    ///
    /// [`SceneAnimationError`]; nothing changes on error.
    pub fn stretch_animation(
        &mut self,
        phase: Phase,
        frames: u32,
    ) -> Result<u64, SceneAnimationError> {
        self.edit_animation(|clip| clip.stretch_phase(phase, frames))
    }

    /// Runs `edit` on a copy of the clip and commits it with a new revision
    /// only when it succeeds and [`Self::check_animation_clip`] accepts the
    /// result.
    ///
    /// # Errors
    ///
    /// [`SceneAnimationError`]; nothing changes on error.
    pub fn edit_animation(
        &mut self,
        edit: impl FnOnce(&mut SceneClip) -> Result<(), ClipError<LayerId>>,
    ) -> Result<u64, SceneAnimationError> {
        let revision = self
            .animation
            .revision
            .checked_add(1)
            .ok_or(SceneAnimationError::RevisionExhausted)?;
        let mut clip = self.animation.clip.clone();
        edit(&mut clip)?;
        self.check_animation_clip(&clip)?;
        self.animation = SceneAnimation::new(clip, revision);
        Ok(revision)
    }

    /// The scene as drawn at `state` of its animation, with layer sources
    /// `source_width` x `source_height` (the project canvas): every enabled
    /// layer posed by [`pose_layer`]. [`ClipState::Static`] or a static
    /// animation returns the scene unchanged.
    #[must_use]
    pub fn posed(&self, state: ClipState, source_width: u32, source_height: u32) -> Self {
        let mut posed = self.clone();
        if matches!(state, ClipState::Static) || self.animation.is_static() {
            return posed;
        }
        for layer in &mut posed.layers {
            if layer.enabled {
                *layer = pose_layer(
                    layer,
                    &self.animation.clip,
                    state,
                    source_width,
                    source_height,
                );
            }
        }
        posed
    }
}

/// One layer's sampled animation, every property defaulting to identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerPose {
    pub dx: i64,
    pub dy: i64,
    /// Basis points.
    pub scale_x: i64,
    pub scale_y: i64,
    /// Millidegrees, clockwise.
    pub rotation: i64,
    /// `0..=255`, multiplied into the static opacity.
    pub opacity: i64,
    /// Left, top, right, bottom, in pixels, each at least zero.
    pub crop: [i64; 4],
}

impl LayerPose {
    pub const IDENTITY: Self = Self {
        dx: 0,
        dy: 0,
        scale_x: fm_animation::SCALE_IDENTITY_BP,
        scale_y: fm_animation::SCALE_IDENTITY_BP,
        rotation: 0,
        opacity: fm_animation::OPACITY_IDENTITY,
        crop: [0; 4],
    };

    /// Samples every scene property of `layer`; [`ClipState::Static`] and
    /// untracked properties are identity.
    #[must_use]
    pub fn sample(clip: &SceneClip, layer: LayerId, state: ClipState) -> Self {
        let mut pose = Self::IDENTITY;
        if matches!(state, ClipState::Static) {
            return pose;
        }
        for (property, value) in clip.evaluate(layer, state) {
            let Value::Scalar(scalar) = value else {
                continue;
            };
            match property {
                Property::PositionX => pose.dx = scalar,
                Property::PositionY => pose.dy = scalar,
                Property::ScaleX => pose.scale_x = scalar,
                Property::ScaleY => pose.scale_y = scalar,
                Property::Rotation => pose.rotation = scalar,
                Property::Opacity => pose.opacity = scalar.clamp(0, 255),
                Property::CropLeft => pose.crop[0] = scalar.max(0),
                Property::CropTop => pose.crop[1] = scalar.max(0),
                Property::CropRight => pose.crop[2] = scalar.max(0),
                Property::CropBottom => pose.crop[3] = scalar.max(0),
                Property::Color => {}
            }
        }
        pose
    }
}

/// `(width, height)` of a `width` x `height` box after `rotation`.
#[must_use]
pub const fn footprint(width: u32, height: u32, rotation: Rotation) -> (u32, u32) {
    match rotation {
        Rotation::Deg0 | Rotation::Deg180 => (width, height),
        Rotation::Deg90 | Rotation::Deg270 => (height, width),
    }
}

const fn rotation_index(rotation: Rotation) -> i64 {
    match rotation {
        Rotation::Deg0 => 0,
        Rotation::Deg90 => 1,
        Rotation::Deg180 => 2,
        Rotation::Deg270 => 3,
    }
}

const fn rotation_from_index(index: i64) -> Rotation {
    match index.rem_euclid(4) {
        1 => Rotation::Deg90,
        2 => Rotation::Deg180,
        3 => Rotation::Deg270,
        _ => Rotation::Deg0,
    }
}

fn to_u32(value: i128) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn to_i32(value: i128) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// `layer` (enabled) as drawn at `state` of `clip`, with its source
/// `source_width` x `source_height` (see the module documentation). A layer
/// the pose hides comes back disabled.
#[must_use]
pub fn pose_layer(
    layer: &Layer,
    clip: &SceneClip,
    state: ClipState,
    source_width: u32,
    source_height: u32,
) -> Layer {
    let pose = LayerPose::sample(clip, layer.id, state);
    if pose == LayerPose::IDENTITY {
        return layer.clone();
    }
    apply_pose(layer, pose, source_width, source_height)
}

/// `layer` with `pose` applied (see [`pose_layer`]).
#[must_use]
#[allow(clippy::similar_names)] // paired edges: x0/x1, y0/y1, and footprint sides
pub fn apply_pose(layer: &Layer, pose: LayerPose, source_width: u32, source_height: u32) -> Layer {
    let mut posed = layer.clone();
    let hidden = |mut layer: Layer| {
        layer.enabled = false;
        layer
    };
    let geometry = layer.geometry;
    // Scale the box (before rotation).
    let scale = |side: u32, factor: i64| {
        to_u32(round_half_up(
            i128::from(side) * i128::from(factor.max(0)),
            i128::from(fm_animation::SCALE_IDENTITY_BP),
        ))
    };
    let width = scale(geometry.width, pose.scale_x);
    let height = scale(geometry.height, pose.scale_y);
    if width == 0 || height == 0 {
        return hidden(posed);
    }
    // Crop the scaled box from each edge, in its own frame.
    let crop = pose.crop.map(|edge| i128::from(edge.max(0)));
    let [left, top, right, bottom] = crop;
    let left = left.min(i128::from(width));
    let right = right.min(i128::from(width) - left);
    let top = top.min(i128::from(height));
    let bottom = bottom.min(i128::from(height) - top);
    let box_width = i128::from(width) - left - right;
    let box_height = i128::from(height) - top - bottom;
    if box_width <= 0 || box_height <= 0 {
        return hidden(posed);
    }
    // The quarter turn realized: static plus the animated offset at the
    // nearest quarter turn, half up.
    let turns = round_half_up(i128::from(pose.rotation), 90_000);
    let rotation = rotation_from_index(
        rotation_index(geometry.rotation) + i64::try_from(turns.rem_euclid(4)).unwrap_or_default(),
    );
    // Centre of the static footprint, doubled to stay integral.
    let (static_width, static_height) =
        footprint(geometry.width, geometry.height, geometry.rotation);
    let center_x2 = 2 * i128::from(geometry.translation_x) + i128::from(static_width);
    let center_y2 = 2 * i128::from(geometry.translation_y) + i128::from(static_height);
    let (scaled_fw, scaled_fh) = footprint(width, height, rotation);
    let origin_x = (center_x2 - i128::from(scaled_fw)).div_euclid(2) + i128::from(pose.dx);
    let origin_y = (center_y2 - i128::from(scaled_fh)).div_euclid(2) + i128::from(pose.dy);
    // Where the cropped box's footprint starts inside the scaled footprint.
    let (offset_x, offset_y) = match rotation {
        Rotation::Deg0 => (left, top),
        Rotation::Deg90 => (bottom, left),
        Rotation::Deg180 => (right, bottom),
        Rotation::Deg270 => (top, right),
    };
    posed.geometry = LayerGeometry::new(
        to_i32(origin_x + offset_x),
        to_i32(origin_y + offset_y),
        to_u32(box_width),
        to_u32(box_height),
        rotation,
    );
    let opacity =
        Property::Opacity.layer_scalar(i64::from(layer.opacity), Value::Scalar(pose.opacity));
    posed.opacity = u8::try_from(opacity.clamp(0, 255)).unwrap_or(u8::MAX);
    if left == 0 && right == 0 && top == 0 && bottom == 0 {
        return posed;
    }
    // Shrink the source region in proportion, rounding each edge down.
    let region = layer
        .crop
        .unwrap_or_else(|| CropRect::new(0, 0, source_width, source_height));
    let shrink = |edge: i128, extent: u32, side: u32| edge * i128::from(extent) / i128::from(side);
    let x0 = i128::from(region.x) + shrink(left, region.width, width);
    let x1 = i128::from(region.x) + i128::from(region.width) - shrink(right, region.width, width);
    let y0 = i128::from(region.y) + shrink(top, region.height, height);
    let y1 =
        i128::from(region.y) + i128::from(region.height) - shrink(bottom, region.height, height);
    if x1 <= x0 || y1 <= y0 {
        return hidden(posed);
    }
    let cropped = CropRect::new(to_u32(x0), to_u32(y0), to_u32(x1 - x0), to_u32(y1 - y0));
    posed.crop = Some(cropped);
    if let Some(mask) = layer.mask {
        // The mask is relative to the source region; move it to the cropped
        // region and clip it there.
        let mask_x0 = i128::from(region.x) + i128::from(mask.x);
        let mask_y0 = i128::from(region.y) + i128::from(mask.y);
        let mask_x1 = mask_x0 + i128::from(mask.width);
        let mask_y1 = mask_y0 + i128::from(mask.height);
        let clip_x0 = mask_x0.max(x0);
        let clip_y0 = mask_y0.max(y0);
        let clip_x1 = mask_x1.min(x1);
        let clip_y1 = mask_y1.min(y1);
        if clip_x1 <= clip_x0 || clip_y1 <= clip_y0 {
            if mask.invert {
                posed.mask = None;
            } else {
                return hidden(posed);
            }
        } else {
            posed.mask = Some(RectMask {
                x: to_u32(clip_x0 - x0),
                y: to_u32(clip_y0 - y0),
                width: to_u32(clip_x1 - clip_x0),
                height: to_u32(clip_y1 - clip_y0),
                invert: mask.invert,
            });
        }
    }
    posed
}
