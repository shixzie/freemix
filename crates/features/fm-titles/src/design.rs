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
//! # Timelines
//!
//! The design owns two named timelines: [`TransitionKind::In`] plays on take
//! and [`TransitionKind::Out`] on off-air. Keyframes are frame indexed, carry
//! the [`Easing`](crate::Easing) used towards the next keyframe, and animate per-node
//! [`NodeProperty`] offsets and opacity layered on top of the node's static
//! values. A property with no track in the timeline being evaluated keeps its
//! static value.

use crate::{
    Bounds, Color, Element, ElementId, FieldDefinition, TemplateId, TitleTemplate, ValidationError,
    validation::validate_template_structure,
};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

/// Deepest permitted group nesting. A root group is depth 1.
pub const MAX_GROUP_DEPTH: usize = 8;
/// Most nodes (elements plus groups) a design may hold.
pub const MAX_DESIGN_NODES: usize = 1_024;
/// Most tracks one timeline may hold.
pub const MAX_TIMELINE_TRACKS: usize = 256;
/// Most keyframes one track may hold.
pub const MAX_TRACK_KEYFRAMES: usize = 256;
/// Longest permitted transition, in frames (ten minutes at 60 fps).
pub const MAX_TIMELINE_FRAMES: u32 = 36_000;
/// Longest permitted node or timeline name, in characters.
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
/// circle center `(cx, cy)`. Masks are hard edged (no anti-aliasing).
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
        let right = self.x + i64::from(self.width);
        let bottom = self.y + i64::from(self.height);
        if x < self.x || x >= right || y < self.y || y >= bottom {
            return false;
        }
        let radius = i64::from(self.corner_radius);
        if radius == 0 {
            return true;
        }
        let center_x = if x < self.x + radius {
            self.x + radius
        } else if x >= right - radius {
            right - radius
        } else {
            return true;
        };
        let center_y = if y < self.y + radius {
            self.y + radius
        } else if y >= bottom - radius {
            bottom - radius
        } else {
            return true;
        };
        let dx = i128::from(2 * x + 1 - 2 * center_x);
        let dy = i128::from(2 * y + 1 - 2 * center_y);
        let r = i128::from(2 * radius);
        dx * dx + dy * dy <= r * r
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

/// Which transition timeline.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TransitionKind {
    /// Plays on take.
    In,
    /// Plays on off-air.
    Out,
}

/// An animatable per-node property layered on the node's static values.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NodeProperty {
    /// Added to the node's x. Values must fit `i32`.
    OffsetX,
    /// Added to the node's y. Values must fit `i32`.
    OffsetY,
    /// Multiplied into the node's opacity. Values must be `0..=255`.
    Opacity,
}

impl NodeProperty {
    fn accepts(self, value: i64) -> bool {
        match self {
            Self::OffsetX | Self::OffsetY => i32::try_from(value).is_ok(),
            Self::Opacity => (0..=255).contains(&value),
        }
    }
}

/// A keyframe; `easing` shapes the interval towards the next keyframe. The
/// shared [`fm_animation::Keyframe`] with a raw `i64` value.
pub type TimelineKeyframe = fm_animation::Keyframe<i64>;

/// One animated property of one node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineTrack {
    pub node: ElementId,
    pub property: NodeProperty,
    /// Strictly increasing frames, all at or before the timeline duration.
    pub keyframes: Vec<TimelineKeyframe>,
}

impl TimelineTrack {
    /// Holds the first value before the first keyframe and the last after the
    /// last; between keyframes `k` and `k + 1` it is
    /// `ease_between(k.value, next.value, k.easing(progress))`.
    #[must_use]
    pub fn value_at(&self, frame: u32) -> Option<i64> {
        fm_animation::sample_frame(&self.keyframes, frame)
    }
}

/// A named transition timeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Timeline {
    pub name: String,
    pub duration_frames: u32,
    pub tracks: Vec<TimelineTrack>,
}

impl Timeline {
    #[must_use]
    pub fn new(name: impl Into<String>, duration_frames: u32) -> Self {
        Self {
            name: name.into(),
            duration_frames,
            tracks: Vec::new(),
        }
    }

    #[must_use]
    pub fn track(&self, node: ElementId, property: NodeProperty) -> Option<&TimelineTrack> {
        self.tracks
            .iter()
            .find(|track| track.node == node && track.property == property)
    }
}

/// Which timeline, at which local frame, a composition samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimelineSample {
    /// Static values only.
    Static,
    /// One timeline at a local frame.
    At {
        transition: TransitionKind,
        frame: u32,
    },
}

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
    EmptyTimelineName(TransitionKind),
    TimelineTooLong {
        transition: TransitionKind,
        maximum: u32,
    },
    TooManyTracks {
        transition: TransitionKind,
        maximum: usize,
    },
    DuplicateTrack {
        transition: TransitionKind,
        node: ElementId,
        property: NodeProperty,
    },
    TrackTargetUnknown {
        transition: TransitionKind,
        node: ElementId,
    },
    EmptyTrack {
        transition: TransitionKind,
        node: ElementId,
        property: NodeProperty,
    },
    TooManyKeyframes {
        transition: TransitionKind,
        maximum: usize,
    },
    NonIncreasingKeyframes {
        transition: TransitionKind,
        node: ElementId,
        property: NodeProperty,
    },
    KeyframeAfterEnd {
        transition: TransitionKind,
        node: ElementId,
        frame: u32,
    },
    InvalidKeyframeValue {
        transition: TransitionKind,
        node: ElementId,
        property: NodeProperty,
    },
    /// An edit named a keyframe frame that the track does not have.
    UnknownKeyframe {
        transition: TransitionKind,
        node: ElementId,
        frame: u32,
    },
    /// An edit would place two keyframes on one frame.
    KeyframeExists {
        transition: TransitionKind,
        node: ElementId,
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

    /// Composes an element's own evaluated opacity with every ancestor factor,
    /// rounding once, half up.
    #[must_use]
    pub fn compose_opacity(&self, element_opacity: u8) -> u8 {
        let numerator = u128::from(element_opacity) * self.opacity_numerator;
        let rounded = (2 * numerator + self.opacity_denominator) / (2 * self.opacity_denominator);
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Composition {
    placements: BTreeMap<ElementId, ElementPlacement>,
}

impl Composition {
    /// An empty composition: nothing is drawn but the scene background.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
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
    pub(crate) transition_in: Timeline,
    pub(crate) transition_out: Timeline,
}

impl TitleDesign {
    /// Creates an empty design with zero-length `TransitionIn` and
    /// `TransitionOut` timelines.
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
            transition_in: Timeline::new("TransitionIn", 0),
            transition_out: Timeline::new("TransitionOut", 0),
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

    #[must_use]
    pub const fn timeline(&self, transition: TransitionKind) -> &Timeline {
        match transition {
            TransitionKind::In => &self.transition_in,
            TransitionKind::Out => &self.transition_out,
        }
    }

    pub(crate) fn timeline_mut(&mut self, transition: TransitionKind) -> &mut Timeline {
        match transition {
            TransitionKind::In => &mut self.transition_in,
            TransitionKind::Out => &mut self.transition_out,
        }
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
    /// with `z_index` rewritten to the paint index. Group transforms, masks,
    /// and timelines are not part of the template; render the instantiated
    /// scene with a [`Composition`] from [`Self::compose`].
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
        }
    }

    /// Folds every ancestor into each visible element's placement for one
    /// timeline sample. See the module documentation for the exact rules.
    #[must_use]
    pub fn compose(&self, sample: TimelineSample) -> Composition {
        let timeline = match sample {
            TimelineSample::Static => None,
            TimelineSample::At { transition, frame } => Some((self.timeline(transition), frame)),
        };
        let animated = |node: ElementId, property: NodeProperty, fallback: i64| -> i64 {
            timeline
                .and_then(|(timeline, frame)| {
                    timeline
                        .track(node, property)
                        .and_then(|track| track.value_at(frame))
                })
                .unwrap_or(fallback)
        };
        let mut placements = BTreeMap::new();
        let mut stack: Vec<(ElementId, ElementPlacement)> = self
            .roots
            .iter()
            .rev()
            .map(|id| (*id, ElementPlacement::root()))
            .collect();
        while let Some((id, mut placement)) = stack.pop() {
            let dx = animated(id, NodeProperty::OffsetX, 0);
            let dy = animated(id, NodeProperty::OffsetY, 0);
            // Keyframes are 0..=255, but an overshooting bezier easing can
            // carry the interpolated value past either end; clamp it instead
            // of mapping an overshoot above 255 to fully transparent.
            let opacity = u8::try_from(animated(id, NodeProperty::Opacity, 255).clamp(0, 255))
                .unwrap_or(u8::MAX);
            match self.nodes.get(&id) {
                Some(DesignNode::Element(_)) => {
                    placement.offset_x += dx;
                    placement.offset_y += dy;
                    placement.multiply_opacity(opacity);
                    placements.insert(id, placement);
                }
                Some(DesignNode::Group(group)) => {
                    if !group.visible {
                        continue;
                    }
                    placement.offset_x += i64::from(group.transform.offset_x) + dx;
                    placement.offset_y += i64::from(group.transform.offset_y) + dy;
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
        Composition { placements }
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
        for transition in [TransitionKind::In, TransitionKind::Out] {
            self.validate_timeline(transition)?;
        }
        Ok(())
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

    fn validate_timeline(&self, transition: TransitionKind) -> Result<(), DesignError> {
        let timeline = self.timeline(transition);
        if timeline.name.trim().is_empty() {
            return Err(DesignError::EmptyTimelineName(transition));
        }
        if timeline.duration_frames > MAX_TIMELINE_FRAMES {
            return Err(DesignError::TimelineTooLong {
                transition,
                maximum: MAX_TIMELINE_FRAMES,
            });
        }
        if timeline.tracks.len() > MAX_TIMELINE_TRACKS {
            return Err(DesignError::TooManyTracks {
                transition,
                maximum: MAX_TIMELINE_TRACKS,
            });
        }
        let mut seen = BTreeSet::new();
        for track in &timeline.tracks {
            let (node, property) = (track.node, track.property);
            if !seen.insert((node, property)) {
                return Err(DesignError::DuplicateTrack {
                    transition,
                    node,
                    property,
                });
            }
            if !self.nodes.contains_key(&node) {
                return Err(DesignError::TrackTargetUnknown { transition, node });
            }
            if track.keyframes.is_empty() {
                return Err(DesignError::EmptyTrack {
                    transition,
                    node,
                    property,
                });
            }
            if track.keyframes.len() > MAX_TRACK_KEYFRAMES {
                return Err(DesignError::TooManyKeyframes {
                    transition,
                    maximum: MAX_TRACK_KEYFRAMES,
                });
            }
            if track
                .keyframes
                .windows(2)
                .any(|pair| pair[0].frame >= pair[1].frame)
            {
                return Err(DesignError::NonIncreasingKeyframes {
                    transition,
                    node,
                    property,
                });
            }
            for keyframe in &track.keyframes {
                if keyframe.frame > timeline.duration_frames {
                    return Err(DesignError::KeyframeAfterEnd {
                        transition,
                        node,
                        frame: keyframe.frame,
                    });
                }
                if !property.accepts(keyframe.value) {
                    return Err(DesignError::InvalidKeyframeValue {
                        transition,
                        node,
                        property,
                    });
                }
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
