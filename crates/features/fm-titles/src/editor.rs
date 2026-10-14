//! Atomic designer edits with a bounded undo/redo history.
//!
//! [`DesignEditor::apply`] performs an edit on a copy of the document, runs
//! [`TitleDesign::validate`] on the copy, and only then swaps it in. A refused
//! edit leaves the document, the history, and the revision untouched. History
//! entries are whole-document snapshots: memory is bounded by the undo depth
//! (at most [`MAX_UNDO_DEPTH`]) times one bounded document.

use crate::{
    Bounds, ClipMask, DesignError, DesignNode, Element, ElementId, Group, GroupTransform,
    TitleDesign,
};
use fm_animation::{AnimationEdit, KeySelection, Keyframe, Phase, TrackKey};
use std::collections::VecDeque;

/// Deepest undo history an editor may keep.
pub const MAX_UNDO_DEPTH: usize = 256;
/// Undo depth used by [`DesignEditor::new`].
pub const DEFAULT_UNDO_DEPTH: usize = 64;

/// Where a node goes: under `parent` (`None` is the root list) at `index`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeSlot {
    pub parent: Option<ElementId>,
    pub index: usize,
}

impl NodeSlot {
    #[must_use]
    pub const fn root(index: usize) -> Self {
        Self {
            parent: None,
            index,
        }
    }

    #[must_use]
    pub const fn under(parent: ElementId, index: usize) -> Self {
        Self {
            parent: Some(parent),
            index,
        }
    }
}

/// One designer edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesignEdit {
    /// Inserts an element; `slot.index` may equal the sibling count (append).
    AddElement {
        slot: NodeSlot,
        element: Element,
    },
    /// Inserts an empty group.
    AddGroup {
        slot: NodeSlot,
        group: Group,
    },
    /// Moves a node to `index` among its current siblings (paint order).
    Reorder {
        node: ElementId,
        index: usize,
    },
    /// Moves a node, with its subtree, under another parent. `slot.index` is
    /// interpreted after the node is removed from its old parent.
    Reparent {
        node: ElementId,
        slot: NodeSlot,
    },
    /// Repositions an element on the canvas.
    SetBounds {
        element: ElementId,
        bounds: Bounds,
    },
    SetGroupTransform {
        group: ElementId,
        transform: GroupTransform,
    },
    SetGroupMask {
        group: ElementId,
        mask: Option<ClipMask>,
    },
    SetGroupVisible {
        group: ElementId,
        visible: bool,
    },
    /// Deletes a node and its whole subtree, and every animation track that
    /// targets a deleted node.
    Delete {
        node: ElementId,
    },
    /// Changes a phase length without moving keys
    /// ([`AnimationEdit::SetPhaseFrames`]).
    SetPhaseFrames {
        phase: Phase,
        frames: u32,
    },
    /// Scales every key of a phase to a new length
    /// ([`AnimationEdit::StretchPhase`]).
    StretchPhase {
        phase: Phase,
        frames: u32,
    },
    /// Adds a keyframe, creating the track if needed
    /// ([`AnimationEdit::SetKey`]). Refused when the track already has a
    /// keyframe on that frame.
    AddKeyframe {
        track: TrackKey<ElementId>,
        keyframe: Keyframe,
    },
    /// Removes a keyframe; a track left empty is removed
    /// ([`AnimationEdit::RemoveKeys`]).
    RemoveKeyframe {
        track: TrackKey<ElementId>,
        frame: u32,
    },
    /// Moves a keyframe to another frame of its phase, keeping its value and
    /// easing ([`AnimationEdit::MoveKeys`]). Landing on an occupied frame or
    /// past the phase end is refused.
    RetimeKeyframe {
        track: TrackKey<ElementId>,
        from: u32,
        to: u32,
    },
    /// Any other atomic clip edit: retime a selection, set easing, paste.
    Animation(AnimationEdit<ElementId>),
}

/// A design under edit with bounded undo/redo.
#[derive(Clone, Debug)]
pub struct DesignEditor {
    document: TitleDesign,
    undo: VecDeque<TitleDesign>,
    redo: Vec<TitleDesign>,
    depth: usize,
    revision: u64,
}

impl DesignEditor {
    /// Wraps a document with [`DEFAULT_UNDO_DEPTH`] history.
    ///
    /// # Errors
    ///
    /// Returns the document's validation error.
    pub fn new(document: TitleDesign) -> Result<Self, DesignError> {
        Self::with_undo_depth(document, DEFAULT_UNDO_DEPTH)
    }

    /// Wraps a document; `depth` is clamped to [`MAX_UNDO_DEPTH`]. A depth of
    /// zero keeps no history.
    ///
    /// # Errors
    ///
    /// Returns the document's validation error.
    pub fn with_undo_depth(document: TitleDesign, depth: usize) -> Result<Self, DesignError> {
        document.validate()?;
        Ok(Self {
            document,
            undo: VecDeque::new(),
            redo: Vec::new(),
            depth: depth.min(MAX_UNDO_DEPTH),
            revision: 0,
        })
    }

    #[must_use]
    pub const fn document(&self) -> &TitleDesign {
        &self.document
    }

    /// Advances on every applied edit, undo, and redo.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    #[must_use]
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Applies one edit atomically and clears the redo stack.
    ///
    /// # Errors
    ///
    /// Returns why the edit or the resulting document is invalid; nothing
    /// changes.
    pub fn apply(&mut self, edit: DesignEdit) -> Result<u64, DesignError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(DesignError::RevisionExhausted)?;
        let mut candidate = self.document.clone();
        apply_edit(&mut candidate, edit)?;
        candidate.validate()?;
        let previous = core::mem::replace(&mut self.document, candidate);
        if self.depth > 0 {
            if self.undo.len() == self.depth {
                self.undo.pop_front();
            }
            self.undo.push_back(previous);
        }
        self.redo.clear();
        self.revision = revision;
        Ok(revision)
    }

    /// Restores the document before the last applied edit.
    ///
    /// # Errors
    ///
    /// Returns [`DesignError::NothingToUndo`] when the history is empty.
    pub fn undo(&mut self) -> Result<u64, DesignError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(DesignError::RevisionExhausted)?;
        let previous = self.undo.pop_back().ok_or(DesignError::NothingToUndo)?;
        let current = core::mem::replace(&mut self.document, previous);
        self.redo.push(current);
        self.revision = revision;
        Ok(revision)
    }

    /// Re-applies the last undone edit.
    ///
    /// # Errors
    ///
    /// Returns [`DesignError::NothingToRedo`] when nothing was undone since
    /// the last applied edit.
    pub fn redo(&mut self) -> Result<u64, DesignError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(DesignError::RevisionExhausted)?;
        let next = self.redo.pop().ok_or(DesignError::NothingToRedo)?;
        let current = core::mem::replace(&mut self.document, next);
        // Redo entries were themselves undone from this history, so the undo
        // stack has room for them again.
        if self.depth > 0 {
            if self.undo.len() == self.depth {
                self.undo.pop_front();
            }
            self.undo.push_back(current);
        }
        self.revision = revision;
        Ok(revision)
    }
}

fn siblings_mut(
    design: &mut TitleDesign,
    parent: Option<ElementId>,
) -> Result<&mut Vec<ElementId>, DesignError> {
    match parent {
        None => Ok(&mut design.roots),
        Some(parent) => match design.nodes.get(&parent) {
            Some(DesignNode::Group(_)) => Ok(design.children.entry(parent).or_default()),
            Some(DesignNode::Element(_)) => Err(DesignError::NotAGroup(parent)),
            None => Err(DesignError::UnknownNode(parent)),
        },
    }
}

fn insert_node(
    design: &mut TitleDesign,
    slot: NodeSlot,
    node: DesignNode,
) -> Result<(), DesignError> {
    let id = node.id();
    if design.nodes.contains_key(&id) {
        return Err(DesignError::DuplicateNode(id));
    }
    let siblings = siblings_mut(design, slot.parent)?;
    if slot.index > siblings.len() {
        return Err(DesignError::IndexOutOfRange {
            index: slot.index,
            length: siblings.len(),
        });
    }
    siblings.insert(slot.index, id);
    design.nodes.insert(id, node);
    Ok(())
}

/// Detaches a node from its parent list, returning the old parent.
fn detach(design: &mut TitleDesign, node: ElementId) -> Result<Option<ElementId>, DesignError> {
    if !design.nodes.contains_key(&node) {
        return Err(DesignError::UnknownNode(node));
    }
    let parent = design.parent(node);
    let siblings = siblings_mut(design, parent)?;
    siblings.retain(|id| *id != node);
    Ok(parent)
}

fn subtree(design: &TitleDesign, node: ElementId) -> Vec<ElementId> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        out.push(id);
        if let Some(children) = design.children.get(&id) {
            stack.extend(children.iter().copied());
        }
    }
    out
}

fn group_mut(design: &mut TitleDesign, id: ElementId) -> Result<&mut Group, DesignError> {
    match design.nodes.get_mut(&id) {
        Some(DesignNode::Group(group)) => Ok(group),
        Some(DesignNode::Element(_)) => Err(DesignError::NotAGroup(id)),
        None => Err(DesignError::UnknownNode(id)),
    }
}

fn apply_edit(design: &mut TitleDesign, edit: DesignEdit) -> Result<(), DesignError> {
    match edit {
        DesignEdit::AddElement { slot, element } => {
            insert_node(design, slot, DesignNode::Element(element))
        }
        DesignEdit::AddGroup { slot, group } => insert_node(design, slot, DesignNode::Group(group)),
        DesignEdit::Reorder { node, index } => {
            let parent = detach(design, node)?;
            let siblings = siblings_mut(design, parent)?;
            if index > siblings.len() {
                return Err(DesignError::IndexOutOfRange {
                    index,
                    length: siblings.len() + 1,
                });
            }
            siblings.insert(index, node);
            Ok(())
        }
        DesignEdit::Reparent { node, slot } => {
            if let Some(parent) = slot.parent
                && subtree(design, node).contains(&parent)
            {
                return Err(DesignError::ReparentIntoSelf(node));
            }
            detach(design, node)?;
            let siblings = siblings_mut(design, slot.parent)?;
            if slot.index > siblings.len() {
                return Err(DesignError::IndexOutOfRange {
                    index: slot.index,
                    length: siblings.len(),
                });
            }
            siblings.insert(slot.index, node);
            Ok(())
        }
        DesignEdit::SetBounds { element, bounds } => match design.nodes.get_mut(&element) {
            Some(DesignNode::Element(target)) => {
                target.bounds = bounds;
                Ok(())
            }
            Some(DesignNode::Group(_)) => Err(DesignError::NotAnElement(element)),
            None => Err(DesignError::UnknownNode(element)),
        },
        DesignEdit::SetGroupTransform { group, transform } => {
            group_mut(design, group)?.transform = transform;
            Ok(())
        }
        DesignEdit::SetGroupMask { group, mask } => {
            group_mut(design, group)?.mask = mask;
            Ok(())
        }
        DesignEdit::SetGroupVisible { group, visible } => {
            group_mut(design, group)?.visible = visible;
            Ok(())
        }
        DesignEdit::Delete { node } => {
            detach(design, node)?;
            let removed = subtree(design, node);
            for id in &removed {
                design.nodes.remove(id);
                design.children.remove(id);
            }
            let orphaned: Vec<_> = design
                .animation
                .tracks()
                .iter()
                .map(|track| track.key)
                .filter(|key| removed.contains(&key.target))
                .collect();
            for key in orphaned {
                design.animation.remove_track(key);
            }
            Ok(())
        }
        DesignEdit::SetPhaseFrames { phase, frames } => {
            apply_animation(design, AnimationEdit::SetPhaseFrames { phase, frames })
        }
        DesignEdit::StretchPhase { phase, frames } => {
            apply_animation(design, AnimationEdit::StretchPhase { phase, frames })
        }
        DesignEdit::Animation(edit) => apply_animation(design, edit),
        keyframe_edit => apply_keyframe_edit(design, &keyframe_edit),
    }
}

fn apply_keyframe_edit(design: &mut TitleDesign, edit: &DesignEdit) -> Result<(), DesignError> {
    match *edit {
        DesignEdit::AddKeyframe { track, keyframe } => {
            if !design.nodes.contains_key(&track.target) {
                return Err(DesignError::UnknownNode(track.target));
            }
            if design.animation.key(track, keyframe.frame).is_some() {
                return Err(DesignError::KeyframeExists {
                    track,
                    frame: keyframe.frame,
                });
            }
            apply_animation(
                design,
                AnimationEdit::SetKey {
                    track,
                    key: keyframe,
                },
            )
        }
        DesignEdit::RemoveKeyframe { track, frame } => {
            if design.animation.key(track, frame).is_none() {
                return Err(DesignError::UnknownKeyframe { track, frame });
            }
            apply_animation(design, AnimationEdit::RemoveKeys(selection(track, frame)))
        }
        DesignEdit::RetimeKeyframe { track, from, to } => {
            if design.animation.key(track, from).is_none() {
                return Err(DesignError::UnknownKeyframe { track, frame: from });
            }
            if from == to {
                return Ok(());
            }
            if design.animation.key(track, to).is_some() {
                return Err(DesignError::KeyframeExists { track, frame: to });
            }
            if to > design.animation.phase_frames(track.phase) {
                return Err(DesignError::Animation(
                    fm_animation::ClipError::KeyOutsidePhase { track, frame: to },
                ));
            }
            apply_animation(
                design,
                AnimationEdit::MoveKeys {
                    selection: selection(track, from),
                    delta: i64::from(to) - i64::from(from),
                },
            )
        }
        _ => Ok(()),
    }
}

fn selection(track: TrackKey<ElementId>, frame: u32) -> KeySelection<ElementId> {
    let mut selection = KeySelection::new();
    selection.insert(track, frame);
    selection
}

/// Applies one clip edit; the clip itself is atomic, and the design is
/// validated as a whole by the caller.
fn apply_animation(
    design: &mut TitleDesign,
    edit: AnimationEdit<ElementId>,
) -> Result<(), DesignError> {
    design.animation.apply(edit)?;
    Ok(())
}
