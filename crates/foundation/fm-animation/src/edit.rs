//! Atomic keyframe edits: insert, remove, move, retime, stretch, easing, and
//! copy/paste.
//!
//! Every edit runs on a copy of the clip and commits only when the result
//! validates, so a refused edit leaves the clip unchanged. Frames that scale
//! are rounded once, half up: `round_half_up(x) = floor(x + 1/2)`.

use crate::{
    clip::{Clip, ClipError, MAX_PHASE_FRAMES},
    easing::Easing,
    property::Property,
    track::{Keyframe, Phase, Track, TrackKey},
    value::Value,
};
use std::collections::{BTreeMap, BTreeSet};

/// One keyframe, identified by its track and frame.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyRef<T> {
    pub track: TrackKey<T>,
    pub frame: u32,
}

/// A set of selected keyframes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeySelection<T> {
    keys: BTreeSet<KeyRef<T>>,
}

impl<T: Copy + Ord> Default for KeySelection<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Ord> KeySelection<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
        }
    }

    pub fn insert(&mut self, track: TrackKey<T>, frame: u32) -> bool {
        self.keys.insert(KeyRef { track, frame })
    }

    pub fn remove(&mut self, track: TrackKey<T>, frame: u32) -> bool {
        self.keys.remove(&KeyRef { track, frame })
    }

    #[must_use]
    pub fn contains(&self, track: TrackKey<T>, frame: u32) -> bool {
        self.keys.contains(&KeyRef { track, frame })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = KeyRef<T>> + '_ {
        self.keys.iter().copied()
    }

    /// The earliest selected frame: the default retime anchor.
    #[must_use]
    pub fn first_frame(&self) -> Option<u32> {
        self.keys.iter().map(|key| key.frame).min()
    }

    /// The latest selected frame.
    #[must_use]
    pub fn last_frame(&self) -> Option<u32> {
        self.keys.iter().map(|key| key.frame).max()
    }

    fn by_track(&self) -> BTreeMap<TrackKey<T>, BTreeSet<u32>> {
        let mut grouped: BTreeMap<TrackKey<T>, BTreeSet<u32>> = BTreeMap::new();
        for key in &self.keys {
            grouped.entry(key.track).or_default().insert(key.frame);
        }
        grouped
    }
}

impl<T: Copy + Ord> FromIterator<KeyRef<T>> for KeySelection<T> {
    fn from_iter<I: IntoIterator<Item = KeyRef<T>>>(iter: I) -> Self {
        Self {
            keys: iter.into_iter().collect(),
        }
    }
}

/// One copied keyframe, relative to the earliest copied frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardKey<T> {
    pub target: T,
    pub property: Property,
    /// Frames after the earliest copied key.
    pub offset: u32,
    pub value: Value,
    pub easing: Easing,
}

/// Copied keyframes, pasteable at any frame of either phase.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeyClipboard<T> {
    pub keys: Vec<ClipboardKey<T>>,
}

/// An edit the timeline editor can apply atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationEdit<T> {
    /// Inserts a keyframe, creating the track if needed and replacing any
    /// key already at that frame.
    SetKey { track: TrackKey<T>, key: Keyframe },
    /// Removes the selected keys; emptied tracks are removed.
    RemoveKeys(KeySelection<T>),
    /// Moves the selection by `delta` frames, clamped so every selected key
    /// stays inside its phase.
    MoveKeys {
        selection: KeySelection<T>,
        delta: i64,
    },
    /// Scales every key of `phase` to a new phase length.
    StretchPhase { phase: Phase, frames: u32 },
    /// Changes a phase length without moving keys.
    SetPhaseFrames { phase: Phase, frames: u32 },
    /// Scales the selected keys about `anchor` by `new_span / old_span`.
    RetimeSelection {
        selection: KeySelection<T>,
        anchor: u32,
        old_span: u32,
        new_span: u32,
    },
    /// Sets the easing of every selected key.
    SetEasing {
        selection: KeySelection<T>,
        easing: Easing,
    },
    /// Pastes copied keys into `phase` with the earliest at frame `at`.
    Paste {
        clipboard: KeyClipboard<T>,
        phase: Phase,
        at: u32,
    },
}

/// `round_half_up(numerator / denominator)` for a positive denominator.
#[must_use]
pub fn round_half_up(numerator: i128, denominator: i128) -> i128 {
    (2 * numerator + denominator).div_euclid(2 * denominator)
}

/// `round_half_up(frame * new_len / old_len)`; `0` when `old_len == 0`.
#[must_use]
pub fn stretch_frame(frame: u32, old_len: u32, new_len: u32) -> u32 {
    if old_len == 0 {
        return 0;
    }
    let value = round_half_up(i128::from(frame) * i128::from(new_len), i128::from(old_len));
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// `anchor + round_half_up((frame - anchor) * new_span / old_span)`, clamped
/// to `0..=limit`. `old_span` must be nonzero.
#[must_use]
pub fn retime_frame(frame: u32, anchor: u32, old_span: u32, new_span: u32, limit: u32) -> u32 {
    let distance = i128::from(frame) - i128::from(anchor);
    let scaled = round_half_up(distance * i128::from(new_span), i128::from(old_span.max(1)));
    let value = (i128::from(anchor) + scaled).clamp(0, i128::from(limit));
    u32::try_from(value).unwrap_or(limit)
}

impl<T: Copy + Ord> Clip<T> {
    /// Applies one edit atomically and returns the selection that follows it
    /// (moved, retimed, or pasted keys; unchanged for easing; empty
    /// otherwise).
    ///
    /// # Errors
    ///
    /// Any [`ClipError`]; the clip is unchanged on error.
    pub fn apply(&mut self, edit: AnimationEdit<T>) -> Result<KeySelection<T>, ClipError<T>> {
        match edit {
            AnimationEdit::SetKey { track, key } => {
                self.set_key(track, key)?;
                Ok(std::iter::once(KeyRef {
                    track,
                    frame: key.frame,
                })
                .collect())
            }
            AnimationEdit::RemoveKeys(selection) => {
                self.remove_keys(&selection)?;
                Ok(KeySelection::new())
            }
            AnimationEdit::MoveKeys { selection, delta } => self.move_keys(&selection, delta),
            AnimationEdit::StretchPhase { phase, frames } => {
                self.stretch_phase(phase, frames)?;
                Ok(KeySelection::new())
            }
            AnimationEdit::SetPhaseFrames { phase, frames } => {
                self.set_phase_frames_keeping_keys(phase, frames)?;
                Ok(KeySelection::new())
            }
            AnimationEdit::RetimeSelection {
                selection,
                anchor,
                old_span,
                new_span,
            } => self.retime_selection(&selection, anchor, old_span, new_span),
            AnimationEdit::SetEasing { selection, easing } => {
                self.set_easing(&selection, easing)?;
                Ok(selection)
            }
            AnimationEdit::Paste {
                clipboard,
                phase,
                at,
            } => self.paste(&clipboard, phase, at),
        }
    }

    /// Inserts a keyframe, creating the track if needed and replacing any key
    /// already at that frame.
    ///
    /// # Errors
    ///
    /// Any [`ClipError`] the result would violate.
    pub fn set_key(&mut self, track: TrackKey<T>, key: Keyframe) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            let mut keys = clip
                .track(track)
                .map(|t| t.keys.clone())
                .unwrap_or_default();
            upsert(&mut keys, key);
            clip.put_track(Track::new(track, keys));
            Ok(())
        })
    }

    /// Removes the selected keys; tracks left empty are removed.
    ///
    /// # Errors
    ///
    /// [`ClipError::UnknownKey`] when a selected key does not exist.
    pub fn remove_keys(&mut self, selection: &KeySelection<T>) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            clip.require(selection)?;
            for (track, frames) in selection.by_track() {
                let Some(existing) = clip.track(track) else {
                    continue;
                };
                let keys: Vec<Keyframe> = existing
                    .keys
                    .iter()
                    .copied()
                    .filter(|key| !frames.contains(&key.frame))
                    .collect();
                if keys.is_empty() {
                    clip.remove_track(track);
                } else {
                    clip.put_track(Track::new(track, keys));
                }
            }
            Ok(())
        })
    }

    /// Sets the easing of every selected key.
    ///
    /// # Errors
    ///
    /// [`ClipError::UnknownKey`] when a selected key does not exist.
    pub fn set_easing(
        &mut self,
        selection: &KeySelection<T>,
        easing: Easing,
    ) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            clip.require(selection)?;
            clip.map_selected(selection, |key| key.easing = easing);
            Ok(())
        })
    }

    /// Moves the selection by `delta` frames. The delta is first clamped so
    /// every selected key stays inside its phase, keeping the selection's
    /// shape. Returns the moved selection.
    ///
    /// # Errors
    ///
    /// [`ClipError::KeyCollision`] when a moved key lands on an unselected
    /// one; [`ClipError::UnknownKey`] for a missing key.
    pub fn move_keys(
        &mut self,
        selection: &KeySelection<T>,
        delta: i64,
    ) -> Result<KeySelection<T>, ClipError<T>> {
        self.require(selection)?;
        let mut low = i64::MIN;
        let mut high = i64::MAX;
        for key in selection.iter() {
            let frame = i64::from(key.frame);
            low = low.max(-frame);
            high = high.min(i64::from(self.phase_frames(key.track.phase)) - frame);
        }
        let delta = if selection.is_empty() {
            0
        } else {
            delta.clamp(low, high)
        };
        let mut moved = KeySelection::new();
        self.try_edit(|clip| {
            for (track, frames) in selection.by_track() {
                let Some(existing) = clip.track(track) else {
                    continue;
                };
                let mut keys = Vec::with_capacity(existing.keys.len());
                for key in &existing.keys {
                    let mut key = *key;
                    if frames.contains(&key.frame) {
                        key.frame = offset_frame(key.frame, delta);
                        moved.insert(track, key.frame);
                    }
                    keys.push(key);
                }
                clip.put_track(Track::new(track, sorted_without_duplicates(track, keys)?));
            }
            Ok(())
        })?;
        Ok(moved)
    }

    /// Scales every key of `phase` to a new phase length:
    /// `frame' = round_half_up(frame * frames / old_len)`.
    ///
    /// # Errors
    ///
    /// [`ClipError::RetimeCollision`] when two keys of a track would land on
    /// one frame, with `min_len` the shortest longer length that does not
    /// collide; [`ClipError::PhaseTooLong`] above [`MAX_PHASE_FRAMES`].
    pub fn stretch_phase(&mut self, phase: Phase, frames: u32) -> Result<(), ClipError<T>> {
        if frames > MAX_PHASE_FRAMES {
            return Err(ClipError::PhaseTooLong { phase, frames });
        }
        let old_len = self.phase_frames(phase);
        let mapping = |length: u32| move |frame: u32| stretch_frame(frame, old_len, length);
        if let Some(track) = self.first_stretch_collision(phase, mapping(frames)) {
            let upper = old_len.max(frames);
            let min_len = (frames.saturating_add(1)..=upper)
                .find(|&length| {
                    self.first_stretch_collision(phase, mapping(length))
                        .is_none()
                })
                .unwrap_or(upper);
            return Err(ClipError::RetimeCollision { track, min_len });
        }
        self.try_edit(|clip| {
            for track in clip.tracks_mut() {
                if track.key.phase == phase {
                    for key in &mut track.keys {
                        key.frame = stretch_frame(key.frame, old_len, frames);
                    }
                }
            }
            clip.set_phase_frames(phase, frames);
            Ok(())
        })
    }

    fn first_stretch_collision(
        &self,
        phase: Phase,
        mapping: impl Fn(u32) -> u32,
    ) -> Option<TrackKey<T>> {
        self.tracks()
            .iter()
            .filter(|track| track.key.phase == phase)
            .find(|track| {
                track
                    .keys
                    .windows(2)
                    .any(|pair| mapping(pair[1].frame) <= mapping(pair[0].frame))
            })
            .map(|track| track.key)
    }

    /// Scales the selected keys about `anchor` (typically the first selected
    /// key or the playhead) by `new_span / old_span`, clamping into the
    /// phase. Unselected keys stay put. Returns the retimed selection.
    ///
    /// # Errors
    ///
    /// [`ClipError::ZeroSpan`] when `old_span == 0`;
    /// [`ClipError::RetimeCollision`] when two keys of a track would share a
    /// frame, with `min_len` the shortest span longer than `new_span` that
    /// does not collide (`old_span` when growing collides).
    pub fn retime_selection(
        &mut self,
        selection: &KeySelection<T>,
        anchor: u32,
        old_span: u32,
        new_span: u32,
    ) -> Result<KeySelection<T>, ClipError<T>> {
        if old_span == 0 {
            return Err(ClipError::ZeroSpan);
        }
        self.require(selection)?;
        if let Some(track) = self.first_retime_collision(selection, anchor, old_span, new_span) {
            let min_len = if new_span < old_span {
                (new_span + 1..=old_span)
                    .find(|&span| {
                        self.first_retime_collision(selection, anchor, old_span, span)
                            .is_none()
                    })
                    .unwrap_or(old_span)
            } else {
                old_span
            };
            return Err(ClipError::RetimeCollision { track, min_len });
        }
        let mut retimed = KeySelection::new();
        self.try_edit(|clip| {
            for (track, frames) in selection.by_track() {
                let limit = clip.phase_frames(track.phase);
                let Some(existing) = clip.track(track) else {
                    continue;
                };
                let mut keys: Vec<Keyframe> = existing.keys.clone();
                for key in &mut keys {
                    if frames.contains(&key.frame) {
                        key.frame = retime_frame(key.frame, anchor, old_span, new_span, limit);
                        retimed.insert(track, key.frame);
                    }
                }
                clip.put_track(Track::new(track, sorted_without_duplicates(track, keys)?));
            }
            Ok(())
        })?;
        Ok(retimed)
    }

    fn first_retime_collision(
        &self,
        selection: &KeySelection<T>,
        anchor: u32,
        old_span: u32,
        new_span: u32,
    ) -> Option<TrackKey<T>> {
        for (track, frames) in selection.by_track() {
            let limit = self.phase_frames(track.phase);
            let Some(existing) = self.track(track) else {
                continue;
            };
            let mut mapped: Vec<u32> = existing
                .keys
                .iter()
                .map(|key| {
                    if frames.contains(&key.frame) {
                        retime_frame(key.frame, anchor, old_span, new_span, limit)
                    } else {
                        key.frame
                    }
                })
                .collect();
            mapped.sort_unstable();
            if mapped.windows(2).any(|pair| pair[0] == pair[1]) {
                return Some(track);
            }
        }
        None
    }

    /// Copies the selected keys with offsets relative to the earliest one.
    ///
    /// # Errors
    ///
    /// [`ClipError::UnknownKey`] when a selected key does not exist.
    pub fn copy_keys(&self, selection: &KeySelection<T>) -> Result<KeyClipboard<T>, ClipError<T>> {
        self.require(selection)?;
        let origin = selection.first_frame().unwrap_or(0);
        let keys = selection
            .iter()
            .filter_map(|reference| {
                let key = self.key(reference.track, reference.frame)?;
                Some(ClipboardKey {
                    target: reference.track.target,
                    property: reference.track.property,
                    offset: reference.frame - origin,
                    value: key.value,
                    easing: key.easing,
                })
            })
            .collect();
        Ok(KeyClipboard { keys })
    }

    /// Pastes copied keys into `phase` with the earliest at frame `at`,
    /// replacing keys already at those frames. Returns the pasted keys.
    ///
    /// # Errors
    ///
    /// [`ClipError::KeyOutsidePhase`] when a pasted key falls past the phase
    /// end, or a limit violation.
    pub fn paste(
        &mut self,
        clipboard: &KeyClipboard<T>,
        phase: Phase,
        at: u32,
    ) -> Result<KeySelection<T>, ClipError<T>> {
        let mut pasted = KeySelection::new();
        self.try_edit(|clip| {
            for entry in &clipboard.keys {
                let track = TrackKey::new(entry.target, phase, entry.property);
                let frame = at.saturating_add(entry.offset);
                let mut keys = clip
                    .track(track)
                    .map(|t| t.keys.clone())
                    .unwrap_or_default();
                upsert(&mut keys, Keyframe::new(frame, entry.value, entry.easing));
                clip.put_track(Track::new(track, keys));
                pasted.insert(track, frame);
            }
            Ok(())
        })?;
        Ok(pasted)
    }

    fn require(&self, selection: &KeySelection<T>) -> Result<(), ClipError<T>> {
        for reference in selection.iter() {
            if self.key(reference.track, reference.frame).is_none() {
                return Err(ClipError::UnknownKey {
                    track: reference.track,
                    frame: reference.frame,
                });
            }
        }
        Ok(())
    }

    fn map_selected(&mut self, selection: &KeySelection<T>, mut update: impl FnMut(&mut Keyframe)) {
        let grouped = selection.by_track();
        for track in self.tracks_mut() {
            if let Some(frames) = grouped.get(&track.key) {
                for key in &mut track.keys {
                    if frames.contains(&key.frame) {
                        update(key);
                    }
                }
            }
        }
    }
}

fn upsert(keys: &mut Vec<Keyframe>, key: Keyframe) {
    match keys.binary_search_by_key(&key.frame, |existing| existing.frame) {
        Ok(index) => keys[index] = key,
        Err(index) => keys.insert(index, key),
    }
}

fn offset_frame(frame: u32, delta: i64) -> u32 {
    let value = i64::from(frame).saturating_add(delta).max(0);
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn sorted_without_duplicates<T: Copy>(
    track: TrackKey<T>,
    mut keys: Vec<Keyframe>,
) -> Result<Vec<Keyframe>, ClipError<T>> {
    keys.sort_by_key(|key| key.frame);
    if let Some(pair) = keys.windows(2).find(|pair| pair[0].frame == pair[1].frame) {
        return Err(ClipError::KeyCollision {
            track,
            frame: pair[0].frame,
        });
    }
    Ok(keys)
}
