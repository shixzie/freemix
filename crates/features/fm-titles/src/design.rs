//! Title designer document: a bounded tree of elements and groups, group
//! transforms and clip masks, and the named transition timelines played by
//! [`crate::TitlePlayback`].
//!
//! # Tree
//!
//! Every node - element or group - has a unique [`ElementId`]. Roots and each
//! group's children are ordered; that order is the paint order (depth first,
//! earlier first). [`TitleDesign::to_template`] flattens the tree into a
//! [`TitleTemplate`] whose element `z_index` is rewritten to the paint index,
//! so an element's own `z_index` is ignored inside a design. Groups nest at
//! most [`MAX_GROUP_DEPTH`] deep and a design holds at most
//! [`MAX_DESIGN_NODES`] nodes.
//!
//! # Exact composition
//!
//! A node's placement is the fold of every ancestor from the root down:
//!
//! * Offsets are integer sums carried in `i64`. With at most
//!   `MAX_GROUP_DEPTH + 1` nodes each contributing an `i32` static and an
//!   `i32` animated offset, the sum cannot overflow, so nesting is exact:
//!   two nested groups offset by `a` and `b` place children exactly as one
//!   group offset by `a + b`. Only the final canvas coordinate is clamped to
//!   `i32`.
//! * Opacity is a product of per-node factors carried as an exact rational
//!   and rounded **once**, half up, together with the element's own evaluated
//!   opacity when it is drawn. A group's factor is
//!   `round(static * animated / 255)`; an element node's factor is its
//!   animated opacity. A factor of 255 is the identity exactly.
//! * A group's [`ClipMask`] is expressed in its children's coordinate space and
//!   moves with the group's own offset; nested masks intersect.
//!
//! # Animation
//!
//! The design owns one [`TitleClip`](crate::TitleClip): an In phase played on
//! take and an Out phase played on off-air, in whole frames of the clip's
//! timebase, with tracks keyed by node, phase, and
//! [`Property`](fm_animation::Property). Group tracks may animate only
//! `PositionX`, `PositionY`, and `Opacity`, which [`TitleDesign::compose`]
//! folds onto every descendant exactly like the static transform. Element
//! tracks may animate every property; they travel with the flattened template
//! ([`TitleDesign::to_template`]) and are applied per element by the renderer
//! at the composition's sample. A property with no track keeps its static
//! value.

use crate::{
    Bounds, Color, Element, ElementId, FieldDefinition, TemplateId, TitleAnimation, TitleClip,
    TitleTemplate, ValidationError, default_title_timebase,
    validation::validate_template_structure,
};
use core::fmt;
use fm_animation::{ClipError, ClipState, Property, TrackKey, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Deepest permitted group nesting. A root group is depth 1.
pub const MAX_GROUP_DEPTH: usize = 8;
/// Most nodes (elements plus groups) a design may hold.
pub const MAX_DESIGN_NODES: usize = 1_024;
/// Longest permitted node name, in characters.
pub const MAX_DESIGN_NAME_CHARS: usize = 128;

/// A static group transform. The identity is offset zero, opacity 255.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupTransform {
    pub offset_x: i32,
    pub offset_y: i32,
    pub opacity: u8,
}

impl GroupTransform {
    pub const IDENTITY: Self = Self {
        offset_x: 0,
        offset_y: 0,
        opacity: u8::MAX,
    };

    #[must_use]
    pub const fn offset(offset_x: i32, offset_y: i32) -> Self {
        Self {
            offset_x,
            offset_y,
            opacity: u8::MAX,
        }
    }
}

impl Default for GroupTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A rectangular clip, optionally with rounded corners.
///
/// A pixel is inside when its center is: for a pixel `(x, y)` that is
/// `x <= px < x + width` and `y <= py < y + height`, and, in a corner square of
/// side `r = min(corner_radius, width / 2, height / 2)`, the exact integer test
/// `(2px + 1 - 2cx)^2 + (2py + 1 - 2cy)^2 <= (2r)^2` against that corner's
/// circle center `(cx, cy)`. In the renderer the mask reports a 4x4
/// subpixel coverage so a rounded corner fades instead of stepping; the
/// `contains` test above remains the exact pixel-center rule used by
/// callers that need a binary answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipMask {
    pub bounds: Bounds,
    pub corner_radius: u32,
}

impl ClipMask {
    #[must_use]
    pub const fn rectangle(bounds: Bounds) -> Self {
        Self {
            bounds,
            corner_radius: 0,
        }
    }

    #[must_use]
    pub const fn rounded(bounds: Bounds, corner_radius: u32) -> Self {
        Self {
            bounds,
            corner_radius,
        }
    }
}

/// A [`ClipMask`] translated into canvas coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedClip {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    /// Already limited to half the shorter side.
    pub corner_radius: u32,
}

impl ResolvedClip {
    fn new(mask: ClipMask, offset_x: i64, offset_y: i64) -> Self {
        let bounds = mask.bounds;
        Self {
            x: i64::from(bounds.x) + offset_x,
            y: i64::from(bounds.y) + offset_y,
            width: bounds.width,
            height: bounds.height,
            corner_radius: mask
                .corner_radius
                .min(bounds.width / 2)
                .min(bounds.height / 2),
        }
    }

    /// Whether canvas pixel `(x, y)` is inside; see [`ClipMask`].
    #[must_use]
    pub fn contains(&self, x: i64, y: i64) -> bool {
        self.contains_scaled(
            x.saturating_mul(8).saturating_add(4),
            y.saturating_mul(8).saturating_add(4),
        )
    }

    /// Whether the point at `(x8, y8)`, in eighths of a canvas pixel, is
    /// inside. Integer-only so every renderer agrees.
    fn contains_scaled(&self, x8: i64, y8: i64) -> bool {
        let left = self.x.saturating_mul(8);
        let top = self.y.saturating_mul(8);
        let right = self
            .x
            .saturating_add(i64::from(self.width))
            .saturating_mul(8);
        let bottom = self
            .y
            .saturating_add(i64::from(self.height))
            .saturating_mul(8);
        if x8 < left || x8 >= right || y8 < top || y8 >= bottom {
            return false;
        }
        let radius = i64::from(self.corner_radius).saturating_mul(8);
        if radius == 0 {
            return true;
        }
        let center_x = if x8 < left + radius {
            left + radius
        } else if x8 >= right - radius {
            right - radius
        } else {
            return true;
        };
        let center_y = if y8 < top + radius {
            top + radius
        } else if y8 >= bottom - radius {
            bottom - radius
        } else {
            return true;
        };
        let dx = i128::from(x8 - center_x);
        let dy = i128::from(y8 - center_y);
        let r = i128::from(radius);
        dx * dx + dy * dy <= r * r
    }

    /// The fraction of canvas pixel `(x, y)` inside, `0..=255`, sampled on a
    /// 4x4 grid so rounded corners and any non-pixel-aligned edge fade out
    /// instead of ending in a staircase.
    #[must_use]
    pub fn coverage(&self, x: i64, y: i64) -> u8 {
        let left = self.x.saturating_mul(8);
        let top = self.y.saturating_mul(8);
        let right = self
            .x
            .saturating_add(i64::from(self.width))
            .saturating_mul(8);
        let bottom = self
            .y
            .saturating_add(i64::from(self.height))
            .saturating_mul(8);
        let x8 = x.saturating_mul(8);
        let y8 = y.saturating_mul(8);
        // A pixel wholly outside (or wholly inside) costs one test per side.
        if x8 >= right || y8 >= bottom || x8 + 8 <= left || y8 + 8 <= top {
            return 0;
        }
        let corners = [(x8, y8), (x8 + 8, y8), (x8, y8 + 8), (x8 + 8, y8 + 8)];
        if corners
            .iter()
            .all(|(cx, cy)| self.contains_scaled(*cx, *cy))
        {
            return 255;
        }
        let mut inside = 0_u16;
        for sample_y in [1, 3, 5, 7] {
            for sample_x in [1, 3, 5, 7] {
                if self.contains_scaled(x8 + sample_x, y8 + sample_y) {
                    inside += 1;
                }
            }
        }
        u8::try_from(inside * 255 / 16).unwrap_or(0)
    }
}

/// A group node. Its children are owned by the design tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Group {
    pub id: ElementId,
    pub name: String,
    pub visible: bool,
    pub transform: GroupTransform,
    pub mask: Option<ClipMask>,
}

impl Group {
    #[must_use]
    pub fn new(id: ElementId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            visible: true,
            transform: GroupTransform::IDENTITY,
            mask: None,
        }
    }
}

/// One node of the design tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesignNode {
    Element(Element),
    Group(Group),
}

impl DesignNode {
    #[must_use]
    pub const fn id(&self) -> ElementId {
        match self {
            Self::Element(element) => element.id,
            Self::Group(group) => group.id,
        }
    }
}

/// The properties a group track may animate: they fold onto descendants.
pub const GROUP_PROPERTIES: [Property; 3] =
    [Property::PositionX, Property::PositionY, Property::Opacity];

/// Every reason a design or an edit is refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesignError {
    TooManyNodes {
        maximum: usize,
    },
    DuplicateNode(ElementId),
    UnknownNode(ElementId),
    NotAGroup(ElementId),
    /// An element-only edit named a group.
    NotAnElement(ElementId),
    /// A node is listed under more than one parent, or a parent lists itself
    /// as a descendant.
    NodeReachedTwice(ElementId),
    /// A node exists but no root reaches it.
    OrphanNode(ElementId),
    GroupTooDeep {
        group: ElementId,
        maximum: usize,
    },
    EmptyName(ElementId),
    NameTooLong(ElementId),
    EmptyMask(ElementId),
    /// The flattened template failed structural validation.
    Template(Vec<ValidationError>),
    /// The animation clip refused the document or an edit.
    Animation(ClipError<ElementId>),
    /// An animation track targets a node the design does not have.
    TrackTargetUnknown(ElementId),
    /// A group track animates a property groups cannot fold onto their
    /// children; see [`GROUP_PROPERTIES`].
    UnsupportedGroupProperty {
        node: ElementId,
        property: Property,
    },
    /// An edit named a keyframe frame that the track does not have.
    UnknownKeyframe {
        track: TrackKey<ElementId>,
        frame: u32,
    },
    /// An edit would place two keyframes on one frame.
    KeyframeExists {
        track: TrackKey<ElementId>,
        frame: u32,
    },
    /// A child index past the end of its parent's child list.
    IndexOutOfRange {
        index: usize,
        length: usize,
    },
    /// A node cannot be reparented under itself or its own descendant.
    ReparentIntoSelf(ElementId),
    NothingToUndo,
    NothingToRedo,
    RevisionExhausted,
}

impl fmt::Display for DesignError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Template(errors) => write!(
                formatter,
                "flattened template has {} validation error(s)",
                errors.len()
            ),
            other => write!(formatter, "invalid title design edit: {other:?}"),
        }
    }
}

impl std::error::Error for DesignError {}

impl From<ClipError<ElementId>> for DesignError {
    fn from(error: ClipError<ElementId>) -> Self {
        Self::Animation(error)
    }
}

/// Where an element sits on the canvas after its ancestors are folded in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElementPlacement {
    pub offset_x: i64,
    pub offset_y: i64,
    opacity_numerator: u128,
    opacity_denominator: u128,
    clips: Vec<ResolvedClip>,
}

impl ElementPlacement {
    fn root() -> Self {
        Self {
            offset_x: 0,
            offset_y: 0,
            opacity_numerator: 1,
            opacity_denominator: 1,
            clips: Vec::new(),
        }
    }

    fn multiply_opacity(&mut self, factor: u8) {
        self.opacity_numerator *= u128::from(factor);
        self.opacity_denominator *= 255;
    }

    /// Composes an element's own static opacity with every ancestor factor,
    /// rounding once, half up.
    #[must_use]
    pub fn compose_opacity(&self, element_opacity: u8) -> u8 {
        self.compose_opacity_with(element_opacity, u8::MAX)
    }

    /// Composes an element's static opacity, its animated opacity factor
    /// (`255` is identity), and every ancestor factor, rounding once, half
    /// up.
    #[must_use]
    pub fn compose_opacity_with(&self, element_opacity: u8, animated: u8) -> u8 {
        let numerator = u128::from(element_opacity) * u128::from(animated) * self.opacity_numerator;
        let denominator = 255 * self.opacity_denominator;
        let rounded = (2 * numerator + denominator) / (2 * denominator);
        u8::try_from(rounded).unwrap_or(u8::MAX)
    }

    /// The canvas-space masks this element is clipped by, outermost first.
    #[must_use]
    pub fn clips(&self) -> &[ResolvedClip] {
        &self.clips
    }
}

/// Every drawable element's placement for one sample. Elements absent from
/// the composition (hidden by a group) are not drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Composition {
    placements: BTreeMap<ElementId, ElementPlacement>,
    sample: ClipState,
}

impl Default for Composition {
    fn default() -> Self {
        Self::empty()
    }
}

impl Composition {
    /// An empty composition: nothing is drawn but the scene background.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            placements: BTreeMap::new(),
            sample: ClipState::Static,
        }
    }

    /// The animation sample this composition was folded at; the renderer
    /// samples element tracks at the same point.
    #[must_use]
    pub const fn sample(&self) -> ClipState {
        self.sample
    }

    #[must_use]
    pub fn placement(&self, element: ElementId) -> Option<&ElementPlacement> {
        self.placements.get(&element)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.placements.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

/// The title designer document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleDesign {
    pub(crate) id: TemplateId,
    pub(crate) name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) background: Color,
    pub(crate) fields: Vec<FieldDefinition>,
    pub(crate) nodes: BTreeMap<ElementId, DesignNode>,
    pub(crate) roots: Vec<ElementId>,
    pub(crate) children: BTreeMap<ElementId, Vec<ElementId>>,
    pub(crate) animation: TitleClip,
}

impl TitleDesign {
    /// Creates an empty design whose animation has zero-length In and Out
    /// phases at [`default_title_timebase`].
    ///
    /// # Errors
    ///
    /// Returns [`DesignError::Template`] when the canvas, name, or fields fail
    /// template validation.
    pub fn new(
        id: TemplateId,
        name: impl Into<String>,
        width: u32,
        height: u32,
        background: Color,
        fields: Vec<FieldDefinition>,
    ) -> Result<Self, DesignError> {
        let design = Self {
            id,
            name: name.into(),
            width,
            height,
            background,
            fields,
            nodes: BTreeMap::new(),
            roots: Vec::new(),
            children: BTreeMap::new(),
            animation: TitleAnimation::empty(default_title_timebase()).clip,
        };
        design.validate()?;
        Ok(design)
    }

    #[must_use]
    pub const fn id(&self) -> TemplateId {
        self.id
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn node(&self, id: ElementId) -> Option<&DesignNode> {
        self.nodes.get(&id)
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn roots(&self) -> &[ElementId] {
        &self.roots
    }

    /// A group's ordered children; `None` for an element or unknown id.
    #[must_use]
    pub fn children(&self, group: ElementId) -> Option<&[ElementId]> {
        self.children.get(&group).map(Vec::as_slice)
    }

    /// The parent group, or `None` for a root or unknown node.
    #[must_use]
    pub fn parent(&self, node: ElementId) -> Option<ElementId> {
        self.children
            .iter()
            .find(|(_, children)| children.contains(&node))
            .map(|(parent, _)| *parent)
    }

    /// The design's In/Out animation, group and element tracks alike.
    #[must_use]
    pub const fn animation(&self) -> &TitleClip {
        &self.animation
    }

    /// Elements in paint order (depth first through the tree).
    #[must_use]
    pub fn paint_order(&self) -> Vec<ElementId> {
        let mut order = Vec::new();
        let mut stack: Vec<ElementId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            match self.nodes.get(&id) {
                Some(DesignNode::Element(_)) => order.push(id),
                Some(DesignNode::Group(_)) => {
                    if let Some(children) = self.children.get(&id) {
                        stack.extend(children.iter().rev().copied());
                    }
                }
                None => {}
            }
        }
        order
    }

    /// Flattens the tree into a template whose elements are in paint order
    /// with `z_index` rewritten to the paint index. The template carries the
    /// design's animation clip (timebase and phase lengths) with only the
    /// element tracks, at revision zero; group transforms, masks, and group
    /// tracks are not part of the template. Render the instantiated scene with
    /// a [`Composition`] from [`Self::compose`].
    #[must_use]
    pub fn to_template(&self) -> TitleTemplate {
        let elements = self
            .paint_order()
            .into_iter()
            .enumerate()
            .filter_map(|(index, id)| match self.nodes.get(&id) {
                Some(DesignNode::Element(element)) => {
                    let mut element = element.clone();
                    element.z_index = i32::try_from(index).unwrap_or(i32::MAX);
                    Some(element)
                }
                _ => None,
            })
            .collect();
        TitleTemplate {
            id: self.id,
            name: self.name.clone(),
            width: self.width,
            height: self.height,
            background: self.background,
            fields: self.fields.clone(),
            elements,
            animation: TitleAnimation::new(self.element_clip(), 0),
        }
    }

    /// The animation without its group tracks.
    fn element_clip(&self) -> TitleClip {
        let mut clip = self.animation.clone();
        let group_tracks: Vec<_> = clip
            .tracks()
            .iter()
            .map(|track| track.key)
            .filter(|key| matches!(self.nodes.get(&key.target), Some(DesignNode::Group(_))))
            .collect();
        for key in group_tracks {
            clip.remove_track(key);
        }
        clip
    }

    /// Folds every group into each visible element's placement for one
    /// animation sample. See the module documentation for the exact rules.
    /// Element tracks are not folded here: the renderer applies them at
    /// [`Composition::sample`].
    #[must_use]
    pub fn compose(&self, sample: ClipState) -> Composition {
        let animated = |node: ElementId, property: Property, fallback: i64| -> i64 {
            match self.animation.sample(node, property, sample) {
                Some(Value::Scalar(value)) => value,
                _ => fallback,
            }
        };
        let mut placements = BTreeMap::new();
        let mut stack: Vec<(ElementId, ElementPlacement)> = self
            .roots
            .iter()
            .rev()
            .map(|id| (*id, ElementPlacement::root()))
            .collect();
        while let Some((id, mut placement)) = stack.pop() {
            match self.nodes.get(&id) {
                Some(DesignNode::Element(_)) => {
                    placements.insert(id, placement);
                }
                Some(DesignNode::Group(group)) => {
                    if !group.visible {
                        continue;
                    }
                    let dx = animated(id, Property::PositionX, 0);
                    let dy = animated(id, Property::PositionY, 0);
                    // The clip clamps samples to the property range, so an
                    // overshooting easing cannot leave 0..=255.
                    let opacity = u8::try_from(animated(id, Property::Opacity, 255).clamp(0, 255))
                        .unwrap_or(u8::MAX);
                    placement.offset_x = placement
                        .offset_x
                        .saturating_add(i64::from(group.transform.offset_x))
                        .saturating_add(dx);
                    placement.offset_y = placement
                        .offset_y
                        .saturating_add(i64::from(group.transform.offset_y))
                        .saturating_add(dy);
                    placement.multiply_opacity(multiply_u8(group.transform.opacity, opacity));
                    if let Some(mask) = group.mask {
                        placement.clips.push(ResolvedClip::new(
                            mask,
                            placement.offset_x,
                            placement.offset_y,
                        ));
                    }
                    if let Some(children) = self.children.get(&id) {
                        stack.extend(
                            children
                                .iter()
                                .rev()
                                .map(|child| (*child, placement.clone())),
                        );
                    }
                }
                None => {}
            }
        }
        Composition { placements, sample }
    }

    /// Checks the whole document. Every edit runs this on its candidate before
    /// the candidate replaces the current document.
    ///
    /// # Errors
    ///
    /// Returns the first problem found.
    pub fn validate(&self) -> Result<(), DesignError> {
        if self.nodes.len() > MAX_DESIGN_NODES {
            return Err(DesignError::TooManyNodes {
                maximum: MAX_DESIGN_NODES,
            });
        }
        self.validate_tree()?;
        let errors = validate_template_structure(&self.to_template());
        if !errors.is_empty() {
            return Err(DesignError::Template(errors));
        }
        self.validate_animation()
    }

    fn validate_tree(&self) -> Result<(), DesignError> {
        for (key, node) in &self.nodes {
            if *key != node.id() {
                return Err(DesignError::UnknownNode(node.id()));
            }
            let name = match node {
                DesignNode::Element(element) => &element.name,
                DesignNode::Group(group) => {
                    if let Some(mask) = group.mask
                        && (mask.bounds.width == 0 || mask.bounds.height == 0)
                    {
                        return Err(DesignError::EmptyMask(group.id));
                    }
                    &group.name
                }
            };
            if name.trim().is_empty() {
                return Err(DesignError::EmptyName(*key));
            }
            if name.chars().count() > MAX_DESIGN_NAME_CHARS {
                return Err(DesignError::NameTooLong(*key));
            }
        }
        for parent in self.children.keys() {
            if !matches!(self.nodes.get(parent), Some(DesignNode::Group(_))) {
                return Err(DesignError::NotAGroup(*parent));
            }
        }
        let mut seen = BTreeSet::new();
        let mut stack: Vec<(ElementId, usize)> = self.roots.iter().map(|id| (*id, 1)).collect();
        while let Some((id, depth)) = stack.pop() {
            if !seen.insert(id) {
                return Err(DesignError::NodeReachedTwice(id));
            }
            match self.nodes.get(&id) {
                None => return Err(DesignError::UnknownNode(id)),
                Some(DesignNode::Element(_)) => {}
                Some(DesignNode::Group(_)) => {
                    if depth > MAX_GROUP_DEPTH {
                        return Err(DesignError::GroupTooDeep {
                            group: id,
                            maximum: MAX_GROUP_DEPTH,
                        });
                    }
                    if let Some(children) = self.children.get(&id) {
                        stack.extend(children.iter().map(|child| (*child, depth + 1)));
                    }
                }
            }
        }
        if let Some(orphan) = self.nodes.keys().find(|id| !seen.contains(id)) {
            return Err(DesignError::OrphanNode(*orphan));
        }
        Ok(())
    }

    fn validate_animation(&self) -> Result<(), DesignError> {
        self.animation.validate()?;
        for track in self.animation.tracks() {
            let node = track.key.target;
            match self.nodes.get(&node) {
                None => return Err(DesignError::TrackTargetUnknown(node)),
                Some(DesignNode::Group(_)) if !GROUP_PROPERTIES.contains(&track.key.property) => {
                    return Err(DesignError::UnsupportedGroupProperty {
                        node,
                        property: track.key.property,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// `round(left * right / 255)`, matching the renderer's channel multiply.
fn multiply_u8(left: u8, right: u8) -> u8 {
    let product = u16::from(left) * u16::from(right) + 127;
    u8::try_from(product / 255).unwrap_or(u8::MAX)
}
