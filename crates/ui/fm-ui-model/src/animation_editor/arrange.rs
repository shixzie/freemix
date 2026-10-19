//! Arranging whole element animations in time, and jumping between keys.
//!
//! [`AnimDocument::stagger`] offsets each element's animation in the shown
//! phase so they start one after another (a lower third's bar, then its
//! name, then its role), in one undo step. It moves every key of an element
//! by the same amount, so each element keeps its own timing and easing; the
//! phase grows when the last element would end past it. [`stagger_plan`] is
//! the pure arithmetic behind it.
//!
//! [`AnimDocument::previous_key_frame`] and
//! [`AnimDocument::next_key_frame`] find the nearest key on either side of
//! the playhead, for "jump to key" navigation.

use fm_animation::{KeySelection, MAX_PHASE_FRAMES};

use super::document::{AnimDocument, AnimEditorError, AnimLimit};

/// The stagger step the editor offers first, in frames.
pub const DEFAULT_STAGGER_FRAMES: u32 = 5;

/// The largest stagger step the editor accepts, in frames.
pub const MAX_STAGGER_FRAMES: u32 = 600;

/// Which element of the timeline starts first when staggering.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StaggerOrder {
    /// The top row starts first (the usual build-on order).
    #[default]
    TopFirst,
    /// The bottom row starts first (often used for an Out that unwinds a
    /// build).
    BottomFirst,
}

/// The result of [`stagger_plan`]: how far to move each element, and how
/// long the phase must be afterwards.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaggerPlan<T> {
    /// Per element, in stagger order, the frames to move all its keys by.
    pub moves: Vec<(T, i64)>,
    /// The last key frame after the moves (the shortest phase that holds
    /// them).
    pub last_frame: u64,
}

/// Plans a stagger of `spans` (`(element, first key frame, last key
/// frame)`, already in stagger order) by `step` frames: the k-th element
/// starts `k * step` frames after the earliest start among them. A `step`
/// of `0` lines every element up on the earliest start.
#[must_use]
pub fn stagger_plan<T: Copy>(spans: &[(T, u32, u32)], step: u32) -> StaggerPlan<T> {
    let base = spans.iter().map(|span| span.1).min().unwrap_or(0);
    let mut last_frame = 0_u64;
    let moves = spans
        .iter()
        .zip(0_u64..)
        .map(|(&(target, first, last), index)| {
            let start = u64::from(base) + index * u64::from(step);
            last_frame = last_frame.max(start + u64::from(last - first));
            // Starts and frames are below 2^40, so the difference fits.
            #[allow(clippy::cast_possible_wrap)]
            let delta = start as i64 - i64::from(first);
            (target, delta)
        })
        .collect();
    StaggerPlan { moves, last_frame }
}

impl<T: Copy + Ord> AnimDocument<T> {
    /// The elements [`Self::stagger`] would arrange, in row order: those
    /// with selected keys in the shown phase when there are at least two,
    /// otherwise every element animated in the shown phase.
    #[must_use]
    pub fn stagger_targets(&self) -> Vec<T> {
        let spans = self.target_spans();
        let selected: Vec<T> = spans
            .iter()
            .map(|span| span.0)
            .filter(|target| {
                self.selection
                    .iter()
                    .any(|key| key.track.target == *target && key.track.phase == self.phase)
            })
            .collect();
        if selected.len() >= 2 {
            selected
        } else {
            spans.into_iter().map(|span| span.0).collect()
        }
    }

    /// Staggers [`Self::stagger_targets`] so each starts `step` frames after
    /// the previous one in `order`, lengthening the shown phase when the
    /// last would end past it. One undo step; selects the moved keys.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::TooFewTargets`] with fewer than two animated
    /// elements, [`AnimEditorError::LimitExceeded`] when the phase would
    /// pass [`MAX_PHASE_FRAMES`], or [`AnimEditorError::DragActive`].
    pub fn stagger(&mut self, step: u32, order: StaggerOrder) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let mut targets = self.stagger_targets();
        if targets.len() < 2 {
            return Err(AnimEditorError::TooFewTargets);
        }
        if order == StaggerOrder::BottomFirst {
            targets.reverse();
        }
        let spans: Vec<(T, u32, u32)> = targets
            .iter()
            .filter_map(|target| {
                self.target_spans()
                    .into_iter()
                    .find(|span| span.0 == *target)
            })
            .collect();
        let plan = stagger_plan(&spans, step);
        let phase = self.phase;
        let last_frame = u32::try_from(plan.last_frame)
            .ok()
            .filter(|frame| *frame <= MAX_PHASE_FRAMES)
            .ok_or(AnimEditorError::LimitExceeded(AnimLimit::PhaseFrames {
                max: MAX_PHASE_FRAMES,
            }))?;
        self.edit(|clip, document| {
            if clip.phase_frames(phase) < last_frame {
                clip.set_phase_frames_keeping_keys(phase, last_frame)?;
            }
            let mut moved = KeySelection::new();
            for (target, delta) in &plan.moves {
                let keys = document.phase_keys_of(*target);
                if keys.is_empty() {
                    continue;
                }
                for key in clip.move_keys(&keys, *delta)?.iter() {
                    moved.insert(key.track, key.frame);
                }
            }
            Ok(moved)
        })
    }

    /// Every key of `target` in the shown phase.
    fn phase_keys_of(&self, target: T) -> KeySelection<T> {
        let mut keys = KeySelection::new();
        for track in self
            .draft
            .tracks()
            .iter()
            .filter(|track| track.key.target == target && track.key.phase == self.phase)
        {
            for key in &track.keys {
                keys.insert(track.key, key.frame);
            }
        }
        keys
    }

    /// The nearest key frame before the playhead in the shown phase.
    #[must_use]
    pub fn previous_key_frame(&self) -> Option<u32> {
        self.shown_key_frames()
            .filter(|frame| *frame < self.playhead)
            .max()
    }

    /// The nearest key frame after the playhead in the shown phase.
    #[must_use]
    pub fn next_key_frame(&self) -> Option<u32> {
        self.shown_key_frames()
            .filter(|frame| *frame > self.playhead)
            .min()
    }

    /// Moves the playhead to the previous (or, with `forward`, the next)
    /// key of the shown phase; pauses. Returns whether there was one.
    pub fn jump_to_key(&mut self, forward: bool) -> bool {
        if self.drag.is_some() {
            return false;
        }
        let frame = if forward {
            self.next_key_frame()
        } else {
            self.previous_key_frame()
        };
        frame.is_some_and(|frame| {
            self.set_playhead(frame);
            true
        })
    }

    fn shown_key_frames(&self) -> impl Iterator<Item = u32> + '_ {
        self.draft
            .tracks()
            .iter()
            .filter(move |track| track.key.phase == self.phase)
            .flat_map(|track| track.keys.iter().map(|key| key.frame))
    }
}

#[cfg(test)]
mod tests {
    use fm_animation::{
        Clip, Easing, KeyRef, Keyframe, Phase, Property, SnapKind, Timebase, Track, TrackKey, Value,
    };
    use fm_types::FrameRate;

    use super::*;
    use crate::animation_editor::{
        DragStart, Modifiers, Pointer, TargetBounds, TargetKind, TargetOutline,
    };

    /// Elements 1 (bar), 2 (name), and 3 (role), each fading in over
    /// frames 0..10, plus element 2 sliding over 0..10 too.
    fn lower_third(in_frames: u32) -> AnimDocument<u32> {
        let fade = |target| {
            Track::new(
                TrackKey::new(target, Phase::In, Property::Opacity),
                vec![
                    Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                    Keyframe::new(
                        10,
                        Value::Scalar(fm_animation::OPACITY_IDENTITY),
                        Easing::Linear,
                    ),
                ],
            )
        };
        let slide = Track::new(
            TrackKey::new(2, Phase::In, Property::PositionX),
            vec![
                Keyframe::new(0, Value::Scalar(-200), Easing::Linear),
                Keyframe::new(10, Value::Scalar(100), Easing::Linear),
            ],
        );
        let clip = Clip::from_tracks(
            Timebase::new(FrameRate::new(30, 1).unwrap()),
            in_frames,
            15,
            vec![fade(1), fade(2), fade(3), slide],
        )
        .unwrap();
        let mut document = AnimDocument::new(clip, 1, 1_920, 1_080);
        document.set_targets([1, 2, 3].map(|target| {
            TargetOutline::new(
                target,
                format!("Element {target}"),
                TargetKind::Shape,
                TargetBounds::new(0, 0, 100, 40),
            )
        }));
        document
    }

    fn span(document: &AnimDocument<u32>, target: u32) -> (u32, u32) {
        let (_, first, last) = document
            .target_spans()
            .into_iter()
            .find(|span| span.0 == target)
            .unwrap();
        (first, last)
    }

    #[test]
    fn plan_starts_each_span_one_step_after_the_previous() {
        let plan = stagger_plan(&[(1, 4, 10), (2, 0, 20), (3, 8, 8)], 6);
        // The earliest start (frame 0) anchors the cascade.
        assert_eq!(plan.moves, [(1, -4), (2, 6), (3, 4)]);
        assert_eq!(plan.last_frame, 26);
        let aligned = stagger_plan(&[(1, 4, 10), (2, 0, 20)], 0);
        assert_eq!(aligned.moves, [(1, -4), (2, 0)]);
        assert_eq!(aligned.last_frame, 20);
        assert_eq!(stagger_plan::<u32>(&[], 5).moves, []);
    }

    #[test]
    fn stagger_cascades_elements_keeps_their_timing_and_grows_the_phase() {
        let mut document = lower_third(20);
        document.stagger(6, StaggerOrder::TopFirst).unwrap();
        assert_eq!(span(&document, 1), (0, 10));
        assert_eq!(span(&document, 2), (6, 16));
        assert_eq!(span(&document, 3), (12, 22));
        // Both of element 2's tracks moved together.
        let slide = document
            .clip()
            .track(TrackKey::new(2, Phase::In, Property::PositionX))
            .unwrap();
        assert_eq!(
            slide.keys.iter().map(|key| key.frame).collect::<Vec<_>>(),
            [6, 16]
        );
        assert_eq!(document.phase_frames(), 22, "the phase grew to hold role");
        assert_eq!(document.selection().len(), 8, "the moved keys are selected");
        assert_eq!(document.undo_depth(), 1);
        assert!(document.is_dirty());
        document.undo().unwrap();
        assert_eq!(span(&document, 3), (0, 10));
        assert_eq!(document.phase_frames(), 20);
    }

    #[test]
    fn stagger_bottom_first_reverses_and_a_selection_scopes_it() {
        let mut document = lower_third(40);
        document.stagger(5, StaggerOrder::BottomFirst).unwrap();
        assert_eq!(
            [1, 2, 3].map(|target| span(&document, target).0),
            [10, 5, 0]
        );
        assert_eq!(document.phase_frames(), 40, "a long phase is kept");

        // Two selected elements: only they are arranged.
        let mut document = lower_third(40);
        let mut selection = fm_animation::KeySelection::new();
        selection.insert(TrackKey::new(1, Phase::In, Property::Opacity), 0);
        selection.insert(TrackKey::new(3, Phase::In, Property::Opacity), 10);
        document.set_selection(&selection);
        assert_eq!(document.stagger_targets(), [1, 3]);
        document.stagger(8, StaggerOrder::TopFirst).unwrap();
        assert_eq!(span(&document, 1), (0, 10));
        assert_eq!(span(&document, 2), (0, 10), "unselected element stays");
        assert_eq!(span(&document, 3), (8, 18));

        // One selected element is not enough to scope: all are arranged.
        let mut document = lower_third(40);
        let one: KeySelection<u32> = core::iter::once(KeyRef {
            track: TrackKey::new(2, Phase::In, Property::Opacity),
            frame: 0,
        })
        .collect();
        document.set_selection(&one);
        assert_eq!(document.stagger_targets(), [1, 2, 3]);
    }

    #[test]
    fn stagger_refuses_one_element_a_drag_and_an_overlong_phase() {
        let mut document = lower_third(20);
        for target in [2, 3] {
            let keys = document.phase_keys_of(target);
            document.set_selection(&keys);
            document.delete_selection().unwrap();
        }
        let depth = document.undo_depth();
        assert_eq!(
            document.stagger(5, StaggerOrder::TopFirst),
            Err(AnimEditorError::TooFewTargets)
        );
        assert_eq!(document.undo_depth(), depth);

        let mut document = lower_third(20);
        document
            .begin_drag(DragStart::Playhead, Pointer::at(3), Modifiers::NONE)
            .unwrap();
        assert_eq!(
            document.stagger(5, StaggerOrder::TopFirst),
            Err(AnimEditorError::DragActive)
        );
        document.commit_drag().unwrap();
        assert!(matches!(
            document.stagger(MAX_PHASE_FRAMES, StaggerOrder::TopFirst),
            Err(AnimEditorError::LimitExceeded(
                AnimLimit::PhaseFrames { .. }
            ))
        ));
        assert_eq!(
            span(&document, 3),
            (0, 10),
            "a refused stagger moves nothing"
        );
    }

    #[test]
    fn key_jumps_find_the_nearest_key_on_each_side() {
        let mut document = lower_third(40);
        document.stagger(6, StaggerOrder::TopFirst).unwrap();
        // Keys at 0, 6, 10, 12, 16, 22.
        document.set_playhead(11);
        assert_eq!(document.previous_key_frame(), Some(10));
        assert_eq!(document.next_key_frame(), Some(12));
        assert!(document.jump_to_key(true));
        assert!(document.jump_to_key(true));
        assert_eq!(document.playhead(), 16);
        document.set_playhead(22);
        assert_eq!(document.next_key_frame(), None);
        assert!(!document.jump_to_key(true));
        assert_eq!(document.playhead(), 22);
        document.set_playhead(0);
        assert!(!document.jump_to_key(false));
        assert_eq!(document.previous_key_frame(), None);
    }

    #[test]
    fn snapping_off_inverts_shift() {
        let mut document = lower_third(40);
        assert!(document.view().snapping());
        assert!(document.view().snaps_with(false) && !document.view().snaps_with(true));
        document.view_mut().set_snapping(false);
        document.set_playhead(20);
        let hit = KeyRef {
            track: TrackKey::new(1, Phase::In, Property::Opacity),
            frame: 10,
        };
        document
            .begin_drag(DragStart::Key(hit), Pointer::at(10), Modifiers::NONE)
            .unwrap();
        // Within the snap threshold of the playhead, but snapping is off.
        assert_eq!(
            document
                .update_drag(Pointer::at(19), Modifiers::NONE)
                .unwrap(),
            None
        );
        // Shift snaps while snapping is off.
        let snap = document
            .update_drag(Pointer::at(19), Modifiers::SHIFT)
            .unwrap()
            .unwrap();
        assert_eq!((snap.frame, snap.kind), (20, SnapKind::Playhead));
        document.commit_drag().unwrap();
        assert_eq!(span(&document, 1), (0, 20));
    }
}
