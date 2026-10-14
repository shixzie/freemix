use std::collections::{BTreeMap, BTreeSet};

use fm_types::InputId;

use crate::{CameraState, LayerId, PlanError, Rational, TalentId};

/// Maximum layers in one composition stack (hooks do not count).
pub const MAX_STACK_LAYERS: usize = 32;
/// Maximum keyed talent slots in one stack.
pub const MAX_TALENT_SLOTS: usize = 8;
/// Maximum authored shadow blur radius in output pixels at 1x zoom.
pub const MAX_SHADOW_BLUR_PX: u32 = 64;
/// Maximum absolute shadow offset on either axis, in set permille.
pub const MAX_SHADOW_OFFSET_PERMILLE: i32 = 250;
/// Maximum output width or height in pixels.
pub const MAX_OUTPUT_DIMENSION: u32 = 16_384;

/// An axis-aligned rectangle in set space, in permille (`0..=1000`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SetRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl SetRect {
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[must_use]
    pub const fn full() -> Self {
        Self::new(0, 0, 1000, 1000)
    }

    const fn is_valid(self) -> bool {
        0 <= self.left
            && self.left < self.right
            && self.right <= 1000
            && 0 <= self.top
            && self.top < self.bottom
            && self.bottom <= 1000
    }
}

/// Where a talent slot's matte comes from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyReference {
    /// The fill source already carries alpha (for example a keyed input).
    FillAlpha,
    /// A separate matte source supplies the key.
    ExternalMatte(InputId),
}

/// The band a layer belongs to; bands always draw background, talent, foreground.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StackRole {
    Background,
    Talent {
        talent_id: TalentId,
        key: KeyReference,
    },
    Foreground,
}

impl StackRole {
    const fn band(self) -> u8 {
        match self {
            Self::Background => 0,
            Self::Talent { .. } => 1,
            Self::Foreground => 2,
        }
    }

    const fn matte(self) -> Option<KeyReference> {
        match self {
            Self::Talent { key, .. } => Some(key),
            Self::Background | Self::Foreground => None,
        }
    }
}

/// A floor reflection: the layer mirrored about the horizontal line
/// `axis_y_permille`, starting at `opacity_permille` of the owner's opacity
/// and fading linearly to zero over `fade_permille` of the reflected height.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReflectionHook {
    pub axis_y_permille: i32,
    pub opacity_permille: u32,
    pub fade_permille: u32,
}

/// A drop shadow: the layer's matte offset in set space and blurred.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ShadowHook {
    pub offset_x_permille: i32,
    pub offset_y_permille: i32,
    pub blur_radius_px: u32,
    pub opacity_permille: u32,
}

/// One authored layer of a virtual-set composition.
///
/// `parallax_permille` is how strongly the layer follows the virtual camera:
/// 1000 locks it to the set, 0 locks it to the screen.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StackLayer {
    pub id: LayerId,
    pub role: StackRole,
    pub source: InputId,
    pub z_order: i32,
    pub rect: SetRect,
    pub parallax_permille: u32,
    pub opacity_permille: u32,
    pub reflection: Option<ReflectionHook>,
    pub shadow: Option<ShadowHook>,
}

impl StackLayer {
    /// A fully opaque, set-locked layer without hooks.
    #[must_use]
    pub const fn new(
        id: LayerId,
        role: StackRole,
        source: InputId,
        z_order: i32,
        rect: SetRect,
    ) -> Self {
        Self {
            id,
            role,
            source,
            z_order,
            rect,
            parallax_permille: 1000,
            opacity_permille: 1000,
            reflection: None,
            shadow: None,
        }
    }

    #[must_use]
    pub const fn with_parallax(mut self, parallax_permille: u32) -> Self {
        self.parallax_permille = parallax_permille;
        self
    }

    #[must_use]
    pub const fn with_opacity(mut self, opacity_permille: u32) -> Self {
        self.opacity_permille = opacity_permille;
        self
    }

    #[must_use]
    pub const fn with_reflection(mut self, reflection: ReflectionHook) -> Self {
        self.reflection = Some(reflection);
        self
    }

    #[must_use]
    pub const fn with_shadow(mut self, shadow: ShadowHook) -> Self {
        self.shadow = Some(shadow);
        self
    }

    fn validate(&self) -> Result<(), PlanError> {
        let id = self.id;
        if !self.rect.is_valid() {
            return Err(PlanError::InvalidSetRect(id));
        }
        if self.parallax_permille > 1000 {
            return Err(PlanError::ParallaxOutOfRange(id));
        }
        if self.opacity_permille > 1000 {
            return Err(PlanError::OpacityOutOfRange(id));
        }
        let has_hook = self.reflection.is_some() || self.shadow.is_some();
        if has_hook && self.role == StackRole::Background {
            return Err(PlanError::HookOnBackground(id));
        }
        if let Some(reflection) = self.reflection {
            if reflection.axis_y_permille < self.rect.bottom || reflection.axis_y_permille > 1000 {
                return Err(PlanError::ReflectionAxisInsideLayer(id));
            }
            if reflection.fade_permille == 0 || reflection.fade_permille > 1000 {
                return Err(PlanError::InvalidReflectionFade(id));
            }
            if reflection.opacity_permille > 1000 {
                return Err(PlanError::OpacityOutOfRange(id));
            }
        }
        if let Some(shadow) = self.shadow {
            let bound = -MAX_SHADOW_OFFSET_PERMILLE..=MAX_SHADOW_OFFSET_PERMILLE;
            if !bound.contains(&shadow.offset_x_permille)
                || !bound.contains(&shadow.offset_y_permille)
            {
                return Err(PlanError::ShadowOffsetOutOfRange(id));
            }
            if shadow.blur_radius_px > MAX_SHADOW_BLUR_PX {
                return Err(PlanError::ShadowBlurTooLarge {
                    layer_id: id,
                    radius_px: shadow.blur_radius_px,
                    maximum: MAX_SHADOW_BLUR_PX,
                });
            }
            if shadow.opacity_permille > 1000 {
                return Err(PlanError::OpacityOutOfRange(id));
            }
        }
        Ok(())
    }
}

/// A validated composition stack in deterministic draw order:
/// `(band, z_order, layer id)` with bands background < talent < foreground.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerStack {
    layers: Vec<StackLayer>,
}

impl LayerStack {
    /// # Errors
    ///
    /// Returns the first structural or per-layer violation found.
    pub fn new(mut layers: Vec<StackLayer>) -> Result<Self, PlanError> {
        if layers.is_empty() {
            return Err(PlanError::EmptyStack);
        }
        if layers.len() > MAX_STACK_LAYERS {
            return Err(PlanError::TooManyStackLayers {
                actual: layers.len(),
                maximum: MAX_STACK_LAYERS,
            });
        }
        let talent_slots = layers
            .iter()
            .filter(|layer| matches!(layer.role, StackRole::Talent { .. }))
            .count();
        if talent_slots > MAX_TALENT_SLOTS {
            return Err(PlanError::TooManyTalentSlots {
                actual: talent_slots,
                maximum: MAX_TALENT_SLOTS,
            });
        }
        if !layers
            .iter()
            .any(|layer| layer.role == StackRole::Background)
        {
            return Err(PlanError::MissingBackground);
        }
        let mut ids = BTreeSet::new();
        let mut talents = BTreeSet::new();
        let mut z_orders = BTreeMap::new();
        for layer in &layers {
            layer.validate()?;
            if !ids.insert(layer.id) {
                return Err(PlanError::DuplicateStackLayer(layer.id));
            }
            if let StackRole::Talent { talent_id, .. } = layer.role
                && !talents.insert(talent_id)
            {
                return Err(PlanError::DuplicateTalentSlot(talent_id));
            }
            if let Some(first) = z_orders.insert((layer.role.band(), layer.z_order), layer.id) {
                return Err(PlanError::DuplicateBandZOrder {
                    z_order: layer.z_order,
                    first,
                    second: layer.id,
                });
            }
        }
        layers.sort_by_key(|layer| (layer.role.band(), layer.z_order, layer.id));
        Ok(Self { layers })
    }

    #[must_use]
    pub fn layers(&self) -> &[StackLayer] {
        &self.layers
    }
}

/// Output raster size in pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutputSize {
    width: u32,
    height: u32,
}

impl OutputSize {
    /// # Errors
    ///
    /// Rejects zero sizes and sizes above [`MAX_OUTPUT_DIMENSION`].
    pub const fn new(width: u32, height: u32) -> Result<Self, PlanError> {
        if width == 0
            || height == 0
            || width > MAX_OUTPUT_DIMENSION
            || height > MAX_OUTPUT_DIMENSION
        {
            return Err(PlanError::InvalidOutputSize { width, height });
        }
        Ok(Self { width, height })
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// An exact rectangle in output pixel space (may extend past the raster).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PixelRect {
    pub left: Rational,
    pub top: Rational,
    pub right: Rational,
    pub bottom: Rational,
}

/// Whole-pixel bounds obtained by rounding a [`PixelRect`] half-up.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PixelBounds {
    pub left: i128,
    pub top: i128,
    pub right: i128,
    pub bottom: i128,
}

impl PixelRect {
    #[must_use]
    pub const fn rounded(self) -> PixelBounds {
        PixelBounds {
            left: self.left.round_half_up(),
            top: self.top.round_half_up(),
            right: self.right.round_half_up(),
            bottom: self.bottom.round_half_up(),
        }
    }
}

/// What a planned layer draws.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PlannedElement {
    Background,
    Talent {
        talent_id: TalentId,
        key: KeyReference,
    },
    Foreground,
    /// The owner's matte, offset and blurred, drawn in black.
    Shadow {
        matte: Option<KeyReference>,
        blur_radius_px: Rational,
    },
    /// The owner flipped vertically; opacity falls linearly from the planned
    /// opacity at `fade_start_y` to zero at `fade_end_y` (output pixels).
    Reflection {
        matte: Option<KeyReference>,
        fade_start_y: Rational,
        fade_end_y: Rational,
    },
}

/// One draw in a frame plan, in output pixel space.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlannedLayer {
    pub layer_id: LayerId,
    pub source: InputId,
    pub element: PlannedElement,
    pub rect: PixelRect,
    pub opacity: Rational,
    pub flip_vertical: bool,
}

/// The complete, ordered draw list for one output frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FramePlan {
    pub frame: u64,
    pub camera: CameraState,
    pub output: OutputSize,
    pub layers: Vec<PlannedLayer>,
}

/// Maps set space to output pixels for one parallax depth.
struct Projection {
    origin_x: Rational,
    origin_y: Rational,
    scale_x: Rational,
    scale_y: Rational,
    zoom: Rational,
}

impl Projection {
    fn new(
        camera: CameraState,
        parallax_permille: u32,
        output: OutputSize,
    ) -> Result<Self, PlanError> {
        let parallax = Rational::permille(i64::from(parallax_permille));
        let half = Rational::new(1, 2)?;
        // Effective zoom 1 + (z - 1) p and centre 1/2 + (c - 1/2) p.
        let zoom = Rational::ONE.checked_add(
            camera
                .zoom()
                .checked_sub(Rational::ONE)?
                .checked_mul(parallax)?,
        )?;
        let centre =
            |value: Rational| half.checked_add(value.checked_sub(half)?.checked_mul(parallax)?);
        let half_window = Rational::ONE.checked_div(zoom.checked_mul(Rational::integer(2))?)?;
        Ok(Self {
            origin_x: centre(camera.pan_x())?.checked_sub(half_window)?,
            origin_y: centre(camera.pan_y())?.checked_sub(half_window)?,
            scale_x: zoom.checked_mul(Rational::integer(i64::from(output.width())))?,
            scale_y: zoom.checked_mul(Rational::integer(i64::from(output.height())))?,
            zoom,
        })
    }

    fn x(&self, permille: i32) -> Result<Rational, PlanError> {
        Rational::permille(i64::from(permille))
            .checked_sub(self.origin_x)?
            .checked_mul(self.scale_x)
    }

    fn y(&self, permille: i32) -> Result<Rational, PlanError> {
        Rational::permille(i64::from(permille))
            .checked_sub(self.origin_y)?
            .checked_mul(self.scale_y)
    }

    fn rect(&self, rect: SetRect) -> Result<PixelRect, PlanError> {
        Ok(PixelRect {
            left: self.x(rect.left)?,
            top: self.y(rect.top)?,
            right: self.x(rect.right)?,
            bottom: self.y(rect.bottom)?,
        })
    }
}

/// Plans one output frame: for every layer in stack order, its shadow, then
/// its reflection, then the layer itself, each as an exact pixel-space draw.
///
/// The camera is clamped (see [`CameraState::clamped`]) before projection.
///
/// # Errors
///
/// Returns [`PlanError::ArithmeticOverflow`] on overflow.
pub fn plan_frame(
    stack: &LayerStack,
    camera: CameraState,
    output: OutputSize,
    frame: u64,
) -> Result<FramePlan, PlanError> {
    let camera = camera.clamped()?;
    let mut planned = Vec::with_capacity(stack.layers.len() * 3);
    for layer in &stack.layers {
        let projection = Projection::new(camera, layer.parallax_permille, output)?;
        let opacity = Rational::permille(i64::from(layer.opacity_permille));
        let matte = layer.role.matte();

        if let Some(shadow) = layer.shadow {
            let rect = SetRect::new(
                layer.rect.left + shadow.offset_x_permille,
                layer.rect.top + shadow.offset_y_permille,
                layer.rect.right + shadow.offset_x_permille,
                layer.rect.bottom + shadow.offset_y_permille,
            );
            planned.push(PlannedLayer {
                layer_id: layer.id,
                source: layer.source,
                element: PlannedElement::Shadow {
                    matte,
                    blur_radius_px: Rational::integer(i64::from(shadow.blur_radius_px))
                        .checked_mul(projection.zoom)?,
                },
                rect: projection.rect(rect)?,
                opacity: opacity
                    .checked_mul(Rational::permille(i64::from(shadow.opacity_permille)))?,
                flip_vertical: false,
            });
        }

        if let Some(reflection) = layer.reflection {
            let axis = reflection.axis_y_permille;
            let mirrored = SetRect::new(
                layer.rect.left,
                2 * axis - layer.rect.bottom,
                layer.rect.right,
                2 * axis - layer.rect.top,
            );
            let rect = projection.rect(mirrored)?;
            let fade = rect
                .bottom
                .checked_sub(rect.top)?
                .checked_mul(Rational::permille(i64::from(reflection.fade_permille)))?;
            planned.push(PlannedLayer {
                layer_id: layer.id,
                source: layer.source,
                element: PlannedElement::Reflection {
                    matte,
                    fade_start_y: rect.top,
                    fade_end_y: rect.top.checked_add(fade)?,
                },
                rect,
                opacity: opacity
                    .checked_mul(Rational::permille(i64::from(reflection.opacity_permille)))?,
                flip_vertical: true,
            });
        }

        let element = match layer.role {
            StackRole::Background => PlannedElement::Background,
            StackRole::Talent { talent_id, key } => PlannedElement::Talent { talent_id, key },
            StackRole::Foreground => PlannedElement::Foreground,
        };
        planned.push(PlannedLayer {
            layer_id: layer.id,
            source: layer.source,
            element,
            rect: projection.rect(layer.rect)?,
            opacity,
            flip_vertical: false,
        });
    }
    Ok(FramePlan {
        frame,
        camera,
        output,
        layers: planned,
    })
}
