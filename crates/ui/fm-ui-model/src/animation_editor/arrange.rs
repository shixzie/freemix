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

use fm_animation::{KeySelection, MAX_PHASE_FRAMES, TrackKey};

use super::EditorMode;
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

/// How [`AnimDocument::stagger`] spaces the element starts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StaggerSpacing {
    /// Each element starts this many frames after the previous one.
    Frames(u32),
    /// The starts are spread evenly from the earliest one so the last
    /// element to start ends on the phase end.
    FillPhase,
}

/// Plans a stagger of `spans` (`(element, first key frame, last key
/// frame)`, already in stagger order) by `step` frames: the k-th element
/// starts `k * step` frames after the earliest start among them. A `step`
/// of `0` lines every element up on the earliest start.
#[must_use]
pub fn stagger_plan<T: Copy>(spans: &[(T, u32, u32)], step: u32) -> StaggerPlan<T> {
    let base = u64::from(spans.iter().map(|span| span.1).min().unwrap_or(0));
    plan_with_starts(spans, |index| base + index * u64::from(step))
}

/// Plans a stagger of `spans` (as for [`stagger_plan`]) that spreads the
/// starts evenly from the earliest one, rounded to whole frames, so the
/// last element ends on `phase_end`. When the last element is too long for
/// that (it would have to start before the earliest start), every element
/// starts together; an element longer than the last can also end past
/// `phase_end`. [`StaggerPlan::last_frame`] says how long the phase must
/// then be.
#[must_use]
pub fn spread_plan<T: Copy>(spans: &[(T, u32, u32)], phase_end: u32) -> StaggerPlan<T> {
    let base = u64::from(spans.iter().map(|span| span.1).min().unwrap_or(0));
    let gaps = u64::try_from(spans.len().saturating_sub(1)).unwrap_or(u64::MAX);
    let last_start = spans.last().map_or(base, |&(_, first, last)| {
        u64::from(phase_end)
            .saturating_sub(u64::from(last - first))
            .max(base)
    });
    let room = last_start - base;
    plan_with_starts(spans, |index| {
        if gaps == 0 {
            base
        } else {
            // The nearest whole frame; `room` is below 2^32 and `index`
            // below `gaps`, so nothing overflows.
            base + (2 * index * room + gaps) / (2 * gaps)
        }
    })
}

/// Moves each span to start at `start(index)`.
fn plan_with_starts<T: Copy>(
    spans: &[(T, u32, u32)],
    start: impl Fn(u64) -> u64,
) -> StaggerPlan<T> {
    let mut last_frame = 0_u64;
    let moves = spans
        .iter()
        .zip(0_u64..)
        .map(|(&(target, first, last), index)| {
            let start = start(index);
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

    /// What [`Self::stagger`] would do with `spacing` and `order`, without
    /// doing it: how far each element moves, and the last key frame after
    /// (the phase grows to it when it is past the phase end). For a preview.
    ///
    /// # Errors
    ///
    /// As [`Self::stagger`].
    pub fn plan_stagger(
        &self,
        spacing: StaggerSpacing,
        order: StaggerOrder,
    ) -> Result<StaggerPlan<T>, AnimEditorError<T>> {
        self.ensure_idle()?;
        let all = self.target_spans();
        let mut spans: Vec<(T, u32, u32)> = self
            .stagger_targets()
            .into_iter()
            .filter_map(|target| all.iter().find(|span| span.0 == target).copied())
            .collect();
        if spans.len() < 2 {
            return Err(AnimEditorError::TooFewTargets);
        }
        if order == StaggerOrder::BottomFirst {
            spans.reverse();
        }
        let plan = match spacing {
            StaggerSpacing::Frames(step) => stagger_plan(&spans, step),
            StaggerSpacing::FillPhase => spread_plan(&spans, self.phase_frames()),
        };
        if plan.last_frame > u64::from(MAX_PHASE_FRAMES) {
            return Err(AnimEditorError::LimitExceeded(AnimLimit::PhaseFrames {
                max: MAX_PHASE_FRAMES,
            }));
        }
        Ok(plan)
    }

    /// Staggers [`Self::stagger_targets`] so each starts after the previous
    /// one in `order`, spaced by `spacing`, lengthening the shown phase when
    /// the last would end past it. One undo step; selects the moved keys.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::TooFewTargets`] with fewer than two animated
    /// elements, [`AnimEditorError::LimitExceeded`] when the phase would
    /// pass [`MAX_PHASE_FRAMES`], or [`AnimEditorError::DragActive`].
    pub fn stagger(
        &mut self,
        spacing: StaggerSpacing,
        order: StaggerOrder,
    ) -> Result<(), AnimEditorError<T>> {
        let plan = self.plan_stagger(spacing, order)?;
        let phase = self.phase;
        // The plan checked it against MAX_PHASE_FRAMES.
        let last_frame = u32::try_from(plan.last_frame).unwrap_or(MAX_PHASE_FRAMES);
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

    /// The rows key jumps move between: the rows holding selected keys of
    /// the shown phase, else (in Advanced mode) `active_row` when it is a
    /// row of the shown phase, else none, which stands for every row.
    #[must_use]
    pub fn key_jump_rows(&self, active_row: Option<TrackKey<T>>) -> Vec<TrackKey<T>> {
        let mut rows: Vec<TrackKey<T>> = self
            .selection
            .iter()
            .map(|key| key.track)
            .filter(|track| track.phase == self.phase)
            .collect();
        rows.dedup();
        if rows.is_empty()
            && self.mode() == EditorMode::Advanced
            && let Some(row) =
                active_row.filter(|row| row.phase == self.phase && self.draft.track(*row).is_some())
        {
            rows.push(row);
        }
        rows
    }

    /// The nearest key frame before the playhead in the shown phase, on
    /// `rows` (every row when empty).
    #[must_use]
    pub fn previous_key_frame(&self, rows: &[TrackKey<T>]) -> Option<u32> {
        self.shown_key_frames(rows)
            .filter(|frame| *frame < self.playhead)
            .max()
    }

    /// The nearest key frame after the playhead in the shown phase, on
    /// `rows` (every row when empty).
    #[must_use]
    pub fn next_key_frame(&self, rows: &[TrackKey<T>]) -> Option<u32> {
        self.shown_key_frames(rows)
            .filter(|frame| *frame > self.playhead)
            .min()
    }

    /// Moves the playhead to the previous (or, with `forward`, the next)
    /// key on `rows` (every row when empty) of the shown phase; pauses.
    /// Returns whether there was one.
    pub fn jump_to_key(&mut self, forward: bool, rows: &[TrackKey<T>]) -> bool {
        if self.drag.is_some() {
            return false;
        }
        let frame = if forward {
            self.next_key_frame(rows)
        } else {
            self.previous_key_frame(rows)
        };
        frame.is_some_and(|frame| {
            self.set_playhead(frame);
            true
        })
    }

    fn shown_key_frames<'a>(&'a self, rows: &'a [TrackKey<T>]) -> impl Iterator<Item = u32> + 'a {
        self.draft
            .tracks()
            .iter()
            .filter(move |track| {
                track.key.phase == self.phase && (rows.is_empty() || rows.contains(&track.key))
            })
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
        document
            .stagger(StaggerSpacing::Frames(6), StaggerOrder::TopFirst)
            .unwrap();
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
        document
            .stagger(StaggerSpacing::Frames(5), StaggerOrder::BottomFirst)
            .unwrap();
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
        document
            .stagger(StaggerSpacing::Frames(8), StaggerOrder::TopFirst)
            .unwrap();
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
            document.stagger(StaggerSpacing::Frames(5), StaggerOrder::TopFirst),
            Err(AnimEditorError::TooFewTargets)
        );
        assert_eq!(document.undo_depth(), depth);

        let mut document = lower_third(20);
        document
            .begin_drag(DragStart::Playhead, Pointer::at(3), Modifiers::NONE)
            .unwrap();
        assert_eq!(
            document.stagger(StaggerSpacing::Frames(5), StaggerOrder::TopFirst),
            Err(AnimEditorError::DragActive)
        );
        document.commit_drag().unwrap();
        assert!(matches!(
            document.stagger(
                StaggerSpacing::Frames(MAX_PHASE_FRAMES),
                StaggerOrder::TopFirst
            ),
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
        document
            .stagger(StaggerSpacing::Frames(6), StaggerOrder::TopFirst)
            .unwrap();
        // Keys at 0, 6, 10, 12, 16, 22.
        document.set_playhead(11);
        assert_eq!(document.previous_key_frame(&[]), Some(10));
        assert_eq!(document.next_key_frame(&[]), Some(12));
        assert!(document.jump_to_key(true, &[]));
        assert!(document.jump_to_key(true, &[]));
        assert_eq!(document.playhead(), 16);
        document.set_playhead(22);
        assert_eq!(document.next_key_frame(&[]), None);
        assert!(!document.jump_to_key(true, &[]));
        assert_eq!(document.playhead(), 22);
        document.set_playhead(0);
        assert!(!document.jump_to_key(false, &[]));
        assert_eq!(document.previous_key_frame(&[]), None);
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

    #[test]
    fn spread_plan_ends_the_last_element_on_the_phase_end() {
        // Three 10-frame spans in a 40-frame phase: starts 0, 15, 30.
        let plan = spread_plan(&[(1, 0, 10), (2, 0, 10), (3, 0, 10)], 40);
        assert_eq!(plan.moves, [(1, 0), (2, 15), (3, 30)]);
        assert_eq!(plan.last_frame, 40);
        // Odd room rounds each start to the nearest frame, from a late base.
        let plan = spread_plan(&[(1, 3, 7), (2, 5, 9), (3, 4, 8), (4, 9, 13)], 20);
        // Base 3, last start 16, room 13 over 3 gaps: 3, 7.33, 11.67, 16.
        assert_eq!(plan.moves, [(1, 0), (2, 2), (3, 8), (4, 7)]);
        assert_eq!(plan.last_frame, 20);
        // A last element too long for the phase starts with the others.
        let plan = spread_plan(&[(1, 0, 5), (2, 2, 32)], 20);
        assert_eq!(plan.moves, [(1, 0), (2, -2)]);
        assert_eq!(plan.last_frame, 30);
        // A longer earlier element still grows the phase.
        let plan = spread_plan(&[(1, 0, 30), (2, 0, 5)], 20);
        assert_eq!(plan.moves, [(1, 0), (2, 15)]);
        assert_eq!(plan.last_frame, 30);
        assert_eq!(spread_plan::<u32>(&[], 20).moves, []);
        assert_eq!(spread_plan(&[(1, 4, 6)], 20).moves, [(1, -4 + 4)]);
    }

    #[test]
    fn spreading_fills_the_phase_in_one_undo_step() {
        let mut document = lower_third(40);
        let plan = document
            .plan_stagger(StaggerSpacing::FillPhase, StaggerOrder::BottomFirst)
            .unwrap();
        assert_eq!(plan.moves, [(3, 0), (2, 15), (1, 30)]);
        assert_eq!(document.undo_depth(), 0, "a plan changes nothing");
        document
            .stagger(StaggerSpacing::FillPhase, StaggerOrder::BottomFirst)
            .unwrap();
        assert_eq!(
            [1, 2, 3].map(|target| span(&document, target)),
            [(30, 40), (15, 25), (0, 10)]
        );
        assert_eq!(
            document.phase_frames(),
            40,
            "the phase is filled, not grown"
        );
        assert_eq!(document.undo_depth(), 1);
        // Applying it again is a no-op: no second undo step.
        document
            .stagger(StaggerSpacing::FillPhase, StaggerOrder::BottomFirst)
            .unwrap();
        assert_eq!(document.undo_depth(), 1);
    }

    #[test]
    fn the_plan_is_what_stagger_does() {
        let mut document = lower_third(20);
        let plan = document
            .plan_stagger(StaggerSpacing::Frames(7), StaggerOrder::TopFirst)
            .unwrap();
        assert_eq!(plan.moves, [(1, 0), (2, 7), (3, 14)]);
        assert_eq!(plan.last_frame, 24);
        document
            .stagger(StaggerSpacing::Frames(7), StaggerOrder::TopFirst)
            .unwrap();
        assert_eq!(span(&document, 3), (14, 24));
        assert_eq!(u64::from(document.phase_frames()), plan.last_frame);
        let mut document = lower_third(20);
        document
            .begin_drag(DragStart::Playhead, Pointer::at(3), Modifiers::NONE)
            .unwrap();
        assert_eq!(
            document.plan_stagger(StaggerSpacing::Frames(7), StaggerOrder::TopFirst),
            Err(AnimEditorError::DragActive)
        );
    }

    #[test]
    fn stagger_works_on_the_out_phase_alone_from_odd_frames() {
        let mut document = lower_third(20);
        document.set_phase(Phase::Out);
        assert_eq!(
            document.stagger(StaggerSpacing::Frames(3), StaggerOrder::TopFirst),
            Err(AnimEditorError::TooFewTargets),
            "nothing is animated in OUT yet"
        );
        let mut clip = document.clip().clone();
        for (target, first) in [(1, 9), (2, 3), (3, 7)] {
            for frame in [first, first + 5] {
                clip.set_key(
                    TrackKey::new(target, Phase::Out, Property::Opacity),
                    Keyframe::new(frame, Value::Scalar(0), Easing::Linear),
                )
                .unwrap();
            }
        }
        document.reset_from_authority(clip, 2);
        document
            .stagger(StaggerSpacing::Frames(3), StaggerOrder::BottomFirst)
            .unwrap();
        // Bottom first from the earliest start (3): role, name, bar.
        assert_eq!(
            [1, 2, 3].map(|target| span(&document, target)),
            [(9, 14), (6, 11), (3, 8)]
        );
        assert_eq!(document.phase_frames(), 15, "OUT kept its length");
        document.set_phase(Phase::In);
        assert_eq!(span(&document, 3), (0, 10), "IN is untouched");
        assert_eq!(document.phase_frames(), 20);
    }

    #[test]
    fn undoing_a_stagger_from_the_other_phase_selects_nothing_hidden() {
        let mut document = lower_third(40);
        let mut selection = KeySelection::new();
        selection.insert(TrackKey::new(1, Phase::In, Property::Opacity), 0);
        selection.insert(TrackKey::new(3, Phase::In, Property::Opacity), 0);
        document.set_selection(&selection);
        document
            .stagger(StaggerSpacing::Frames(4), StaggerOrder::TopFirst)
            .unwrap();
        document.set_phase(Phase::Out);
        document.undo().unwrap();
        let role = TrackKey::new(3, Phase::In, Property::Opacity);
        assert_eq!(document.clip().track(role).unwrap().keys[0].frame, 0);
        // The IN keys selected before the stagger are not shown, so a
        // Delete must not reach them.
        assert!(document.selection().is_empty());
        assert_eq!(
            document.delete_selection(),
            Err(AnimEditorError::EmptySelection)
        );
        document.redo().unwrap();
        assert!(document.selection().is_empty());
        document.set_phase(Phase::In);
        assert_eq!(span(&document, 3), (4, 14));
    }

    #[test]
    fn stagger_after_a_remote_change_uses_the_new_animation() {
        let mut document = lower_third(40);
        let plan = document
            .plan_stagger(StaggerSpacing::Frames(5), StaggerOrder::TopFirst)
            .unwrap();
        assert_eq!(plan.moves.len(), 3);
        // Another console deletes the role's animation while the menu is
        // open.
        let mut clip = document.clip().clone();
        let role = document.phase_keys_of(3);
        clip.remove_keys(&role).unwrap();
        document.reset_from_authority(clip, 2);
        document
            .stagger(StaggerSpacing::Frames(5), StaggerOrder::TopFirst)
            .unwrap();
        assert_eq!(span(&document, 2), (5, 15));
        assert!(document.target_spans().iter().all(|span| span.0 != 3));
        assert_eq!(document.undo_depth(), 1);
    }

    #[test]
    fn key_jumps_keep_to_the_selected_or_active_rows() {
        let mut document = lower_third(40);
        document
            .stagger(StaggerSpacing::Frames(6), StaggerOrder::TopFirst)
            .unwrap();
        // Keys: bar 0/10, name 6/16, role 12/22.
        document.clear_selection();
        let name = TrackKey::new(2, Phase::In, Property::PositionX);
        let role = TrackKey::new(3, Phase::In, Property::Opacity);
        assert!(document.key_jump_rows(Some(role)).is_empty(), "Simple mode");
        document.set_mode(EditorMode::Advanced);
        assert_eq!(document.key_jump_rows(Some(role)), [role]);
        assert!(document.key_jump_rows(None).is_empty(), "every row");
        // A row of the other phase, or one without a track, is no scope.
        for row in [
            TrackKey::new(3, Phase::Out, Property::Opacity),
            TrackKey::new(1, Phase::In, Property::PositionY),
        ] {
            assert!(document.key_jump_rows(Some(row)).is_empty(), "{row:?}");
        }
        document.set_playhead(0);
        let rows = document.key_jump_rows(Some(role));
        assert!(document.jump_to_key(true, &rows));
        assert_eq!(document.playhead(), 12);
        assert!(document.jump_to_key(true, &rows));
        assert_eq!(document.playhead(), 22);
        assert!(
            !document.jump_to_key(true, &rows),
            "no key after on the row"
        );
        assert_eq!(document.next_key_frame(&rows), None);
        assert_eq!(document.previous_key_frame(&rows), Some(12));
        // A selection outranks the active row.
        let selected: KeySelection<u32> = [
            KeyRef {
                track: name,
                frame: 6,
            },
            KeyRef {
                track: name,
                frame: 16,
            },
        ]
        .into_iter()
        .collect();
        document.set_selection(&selected);
        let rows = document.key_jump_rows(Some(role));
        assert_eq!(rows, [name]);
        assert!(document.jump_to_key(false, &rows));
        assert_eq!(document.playhead(), 16);
        assert!(document.jump_to_key(false, &rows));
        assert_eq!(document.playhead(), 6);
        assert!(!document.jump_to_key(false, &rows));
        // With no keys at all, nothing to jump to anywhere.
        let mut empty = lower_third(40);
        let everything = {
            empty.select_all();
            empty.selection().clone()
        };
        empty.set_selection(&everything);
        empty.delete_selection().unwrap();
        assert_eq!(empty.next_key_frame(&[]), None);
        assert_eq!(empty.previous_key_frame(&[]), None);
        assert!(!empty.jump_to_key(true, &[]));
    }
}
