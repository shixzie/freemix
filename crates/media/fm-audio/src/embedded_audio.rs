//! SDI-style 16-channel embedded audio: de-embedding of groups, stereo pairs,
//! and mono channels, plus layout-preserving re-embedding (`AU-003`).
//!
//! The embedded stream is modelled as 16 planar `f32` slots arranged as four
//! groups of four channels (SMPTE ST 299-1 numbering). Group `g` channel `c`
//! (both one-based) is slot `(g - 1) * 4 + (c - 1)`. Pair 1 of a group is
//! channels 1/2 and pair 2 is channels 3/4 (one AES3 pair each).
//!
//! All operations are bit-exact copies into caller-owned buffers: samples are
//! transported, not numerically validated, and nothing allocates.

use crate::multichannel::{
    MAX_MULTICHANNEL_CHANNELS, MultichannelError, MultichannelLayout, SpeakerLabel,
    validate_output_planes, validate_planes,
};

/// Number of embedded audio groups.
pub const EMBEDDED_GROUPS: usize = 4;
/// Channels per embedded audio group.
pub const EMBEDDED_CHANNELS_PER_GROUP: usize = 4;
/// Total embedded channel slots.
pub const EMBEDDED_CHANNELS: usize = EMBEDDED_GROUPS * EMBEDDED_CHANNELS_PER_GROUP;

const _: () = assert!(EMBEDDED_CHANNELS == MAX_MULTICHANNEL_CHANNELS);

const GROUPS: u8 = 4;
const CHANNELS_PER_GROUP: u8 = 4;
const _: () = assert!(GROUPS as usize == EMBEDDED_GROUPS);
const _: () = assert!(CHANNELS_PER_GROUP as usize == EMBEDDED_CHANNELS_PER_GROUP);

/// One embedded channel slot (group 1–4, channel 1–4).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddedSlot {
    group: u8,
    channel: u8,
}

impl EmbeddedSlot {
    /// # Errors
    ///
    /// Returns an error when `group` or `channel` is outside `1..=4`.
    pub fn new(group: u8, channel: u8) -> Result<Self, MultichannelError> {
        validate_group(group)?;
        if !(1..=CHANNELS_PER_GROUP).contains(&channel) {
            return Err(MultichannelError::InvalidEmbeddedChannel(channel));
        }
        Ok(Self { group, channel })
    }

    /// Slot from a zero-based index in `0..16`.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "index < 16, so both quotient and remainder are < 4"
    )]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < EMBEDDED_CHANNELS {
            Some(Self {
                group: (index / EMBEDDED_CHANNELS_PER_GROUP) as u8 + 1,
                channel: (index % EMBEDDED_CHANNELS_PER_GROUP) as u8 + 1,
            })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn group(self) -> u8 {
        self.group
    }

    #[must_use]
    pub const fn channel(self) -> u8 {
        self.channel
    }

    /// Zero-based index into the 16 embedded planes.
    #[must_use]
    pub const fn index(self) -> usize {
        (self.group as usize - 1) * EMBEDDED_CHANNELS_PER_GROUP + (self.channel as usize - 1)
    }
}

/// One embedded stereo pair (group 1–4, pair 1–2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EmbeddedPair {
    group: u8,
    pair: u8,
}

impl EmbeddedPair {
    /// # Errors
    ///
    /// Returns an error when `group` is outside `1..=4` or `pair` outside `1..=2`.
    pub fn new(group: u8, pair: u8) -> Result<Self, MultichannelError> {
        validate_group(group)?;
        if !(1..=2).contains(&pair) {
            return Err(MultichannelError::InvalidEmbeddedPair(pair));
        }
        Ok(Self { group, pair })
    }

    #[must_use]
    pub const fn group(self) -> u8 {
        self.group
    }

    #[must_use]
    pub const fn pair(self) -> u8 {
        self.pair
    }

    #[must_use]
    pub const fn left(self) -> EmbeddedSlot {
        EmbeddedSlot {
            group: self.group,
            channel: self.pair * 2 - 1,
        }
    }

    #[must_use]
    pub const fn right(self) -> EmbeddedSlot {
        EmbeddedSlot {
            group: self.group,
            channel: self.pair * 2,
        }
    }
}

fn validate_group(group: u8) -> Result<(), MultichannelError> {
    if (1..=GROUPS).contains(&group) {
        Ok(())
    } else {
        Err(MultichannelError::InvalidEmbeddedGroup(group))
    }
}

/// Copies one embedded slot into a mono output plane.
///
/// # Errors
///
/// Returns an error unless `embedded` has 16 equal-length planes and `output`
/// has that length.
pub fn deembed_mono<I: AsRef<[f32]>>(
    embedded: &[I],
    slot: EmbeddedSlot,
    output: &mut [f32],
) -> Result<(), MultichannelError> {
    let samples = validate_planes(embedded, EMBEDDED_CHANNELS)?;
    validate_output_planes(&mut [&mut *output], 1, samples)?;
    output.copy_from_slice(embedded[slot.index()].as_ref());
    Ok(())
}

/// Copies one embedded pair into `[left, right]` output planes.
///
/// # Errors
///
/// Returns an error unless `embedded` has 16 equal-length planes and `output`
/// has two planes of that length.
pub fn deembed_pair<I: AsRef<[f32]>, O: AsMut<[f32]>>(
    embedded: &[I],
    pair: EmbeddedPair,
    output: &mut [O],
) -> Result<(), MultichannelError> {
    let samples = validate_planes(embedded, EMBEDDED_CHANNELS)?;
    validate_output_planes(output, 2, samples)?;
    output[0]
        .as_mut()
        .copy_from_slice(embedded[pair.left().index()].as_ref());
    output[1]
        .as_mut()
        .copy_from_slice(embedded[pair.right().index()].as_ref());
    Ok(())
}

/// Copies all four channels of an embedded group into four output planes.
///
/// # Errors
///
/// Returns an error for an invalid group, unless `embedded` has 16 equal-length
/// planes and `output` has four planes of that length.
pub fn deembed_group<I: AsRef<[f32]>, O: AsMut<[f32]>>(
    embedded: &[I],
    group: u8,
    output: &mut [O],
) -> Result<(), MultichannelError> {
    validate_group(group)?;
    let samples = validate_planes(embedded, EMBEDDED_CHANNELS)?;
    validate_output_planes(output, EMBEDDED_CHANNELS_PER_GROUP, samples)?;
    let first = (usize::from(group) - 1) * EMBEDDED_CHANNELS_PER_GROUP;
    for (offset, plane) in output.iter_mut().enumerate() {
        plane
            .as_mut()
            .copy_from_slice(embedded[first + offset].as_ref());
    }
    Ok(())
}

/// One layout placed into consecutive embedded slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddedAssignment {
    pub layout: MultichannelLayout,
    pub first: EmbeddedSlot,
}

impl EmbeddedAssignment {
    /// Zero-based slot range occupied by this assignment.
    #[must_use]
    pub const fn slots(&self) -> core::ops::Range<usize> {
        let first = self.first.index();
        first..first + self.layout.channel_count()
    }
}

/// A 16-slot embedding plan that records which layout occupies which slots,
/// so de-embedding restores the original channel count, order, and labels.
///
/// Multichannel assignments must start on a pair boundary (odd channel
/// number) so stereo pairs never straddle AES3 pairs; mono may use any slot.
/// Assignments may not overlap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbeddedAudioMap {
    assignments: [Option<EmbeddedAssignment>; EMBEDDED_CHANNELS],
    count: usize,
}

impl EmbeddedAudioMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves slots for `layout` starting at `first` and returns the
    /// assignment index.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout would run past slot 16, a multichannel
    /// layout starts on an even channel, or any slot is already assigned.
    pub fn assign(
        &mut self,
        layout: MultichannelLayout,
        first: EmbeddedSlot,
    ) -> Result<usize, MultichannelError> {
        let candidate = EmbeddedAssignment { layout, first };
        let range = candidate.slots();
        if range.end > EMBEDDED_CHANNELS {
            return Err(MultichannelError::EmbeddedOverflow {
                first_slot: range.start,
                channels: layout.channel_count(),
            });
        }
        if layout.channel_count() > 1 && !range.start.is_multiple_of(2) {
            return Err(MultichannelError::EmbeddedMisaligned {
                first_slot: range.start,
            });
        }
        for slot in range {
            if self.label_at(slot).is_some() {
                return Err(MultichannelError::EmbeddedOverlap { slot });
            }
        }
        let index = self.count;
        self.assignments[index] = Some(candidate);
        self.count += 1;
        Ok(index)
    }

    pub fn assignments(&self) -> impl Iterator<Item = &EmbeddedAssignment> + '_ {
        self.assignments[..self.count].iter().flatten()
    }

    #[must_use]
    pub fn assignment(&self, index: usize) -> Option<&EmbeddedAssignment> {
        self.assignments[..self.count].get(index)?.as_ref()
    }

    /// Label carried by a zero-based embedded slot, if assigned.
    #[must_use]
    pub fn label_at(&self, slot: usize) -> Option<SpeakerLabel> {
        self.assignments().find_map(|assignment| {
            let range = assignment.slots();
            range
                .contains(&slot)
                .then(|| assignment.layout.labels()[slot - range.start])
        })
    }

    /// Copies source planes (in the assignment's layout order) into their
    /// embedded slots. Other slots are untouched.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown assignment, unless `source` has the
    /// layout's channel count and `embedded` has 16 planes, all equal length.
    pub fn embed<I: AsRef<[f32]>, O: AsMut<[f32]>>(
        &self,
        assignment: usize,
        source: &[I],
        embedded: &mut [O],
    ) -> Result<(), MultichannelError> {
        let entry = *self
            .assignment(assignment)
            .ok_or(MultichannelError::UnknownEmbeddedAssignment(assignment))?;
        let samples = validate_planes(source, entry.layout.channel_count())?;
        validate_output_planes(embedded, EMBEDDED_CHANNELS, samples)?;
        for (plane, slot) in source.iter().zip(entry.slots()) {
            embedded[slot].as_mut().copy_from_slice(plane.as_ref());
        }
        Ok(())
    }

    /// Copies an assignment's slots back out in its original layout order.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown assignment, unless `embedded` has 16
    /// planes and `output` has the layout's channel count, all equal length.
    pub fn deembed<I: AsRef<[f32]>, O: AsMut<[f32]>>(
        &self,
        assignment: usize,
        embedded: &[I],
        output: &mut [O],
    ) -> Result<MultichannelLayout, MultichannelError> {
        let entry = *self
            .assignment(assignment)
            .ok_or(MultichannelError::UnknownEmbeddedAssignment(assignment))?;
        let samples = validate_planes(embedded, EMBEDDED_CHANNELS)?;
        validate_output_planes(output, entry.layout.channel_count(), samples)?;
        for (plane, slot) in output.iter_mut().zip(entry.slots()) {
            plane.as_mut().copy_from_slice(embedded[slot].as_ref());
        }
        Ok(entry.layout)
    }

    /// Writes `+0.0` into every unassigned slot.
    ///
    /// # Errors
    ///
    /// Returns an error unless `embedded` has 16 equal-length planes.
    pub fn silence_unassigned<O: AsMut<[f32]>>(
        &self,
        embedded: &mut [O],
    ) -> Result<(), MultichannelError> {
        let samples = embedded.first_mut().map_or(0, |plane| plane.as_mut().len());
        validate_output_planes(embedded, EMBEDDED_CHANNELS, samples)?;
        for (slot, plane) in embedded.iter_mut().enumerate() {
            if self.label_at(slot).is_none() {
                plane.as_mut().fill(0.0);
            }
        }
        Ok(())
    }
}
