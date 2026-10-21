//! What a delay ring holds, so a mirror of it copies only what changed.
//!
//! The native daemon renders each frame into a pending mixer synchronized
//! from the committed one, and swaps them when the render succeeds
//! ([`crate::MasterMixer::copy_runtime_state_from`]). Copying every delay
//! history whole each frame costs up to the full ring (hundreds of
//! kilobytes per stereo strip) per frame. A [`RingVersion`] names a ring's
//! content and records the one contiguous run of positions written since
//! the content it was last synchronized to, so a mirror that still holds
//! that earlier content copies only the run: one block's worth of samples.

use core::sync::atomic::{AtomicU64, Ordering};

/// A fresh content name, never handed out before in this process.
fn next_version() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A ring's content name and the run written since its base content.
///
/// Every write to the ring must be recorded ([`Self::wrote`] or
/// [`Self::rewrote`]); two rings of the same length whose versions are equal
/// hold the same samples.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RingVersion {
    /// Names the ring's content.
    current: u64,
    /// The content the ring held before the writes in the run.
    base: u64,
    /// The run's first position (per channel).
    start: usize,
    /// How many positions from `start` (wrapping) were written.
    len: usize,
    /// Everything may have changed since `base`.
    all: bool,
}

/// Bookkeeping, not state: two rings compare by their samples.
impl PartialEq for RingVersion {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl RingVersion {
    /// A newly filled ring: content no other ring is known to share.
    pub(crate) fn fresh() -> Self {
        let version = next_version();
        Self {
            current: version,
            base: version,
            start: 0,
            len: 0,
            all: false,
        }
    }

    /// Records `count` positions written from `at` in each channel's ring
    /// of `ring_len` positions.
    pub(crate) fn wrote(&mut self, at: usize, count: usize, ring_len: usize) {
        if count == 0 || ring_len == 0 {
            return;
        }
        self.current = next_version();
        if self.all {
            return;
        }
        let at = at % ring_len;
        if self.len == 0 {
            self.start = at;
            self.len = count.min(ring_len);
        } else if (self.start + self.len) % ring_len == at {
            self.len = (self.len + count).min(ring_len);
        } else {
            self.all = true;
        }
        if self.len == ring_len {
            self.all = true;
        }
    }

    /// Records a change anywhere in the ring (a clear).
    pub(crate) fn rewrote(&mut self) {
        self.current = next_version();
        self.all = true;
    }

    /// Makes `ring` (described by `self`) hold `source` (described by
    /// `of`), `per_channel` positions per channel ring: nothing when it
    /// already does, only `of`'s run when `ring` holds the content that run
    /// was written over, the whole ring otherwise. Returns how many samples
    /// were copied.
    pub(crate) fn mirror(
        &mut self,
        ring: &mut Vec<f32>,
        of: &Self,
        source: &[f32],
        per_channel: usize,
    ) -> usize {
        let copied = if ring.len() != source.len() {
            ring.clear();
            ring.extend_from_slice(source);
            source.len()
        } else if self.current == of.current {
            0
        } else if of.base == self.current && !of.all && per_channel != 0 {
            let first = of.len.min(per_channel - of.start);
            let wrapped = of.len - first;
            for (to, from) in ring
                .chunks_exact_mut(per_channel)
                .zip(source.chunks_exact(per_channel))
            {
                to[of.start..of.start + first].copy_from_slice(&from[of.start..of.start + first]);
                to[..wrapped].copy_from_slice(&from[..wrapped]);
            }
            of.len * (ring.len() / per_channel)
        } else {
            ring.copy_from_slice(source);
            source.len()
        };
        // The mirror's own run starts empty over the content it now holds.
        *self = Self {
            current: of.current,
            base: of.current,
            start: 0,
            len: 0,
            all: false,
        };
        copied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A committed ring and its mirror, alternately written and synchronized
    /// as the native daemon's mixer pair is: each synchronization copies
    /// only the last write's run, and the mirror always equals the source.
    #[test]
    fn a_mirror_copies_only_the_run_written_since_it_last_matched() {
        let per_channel = 10;
        let (mut a, mut b) = (
            vec![0.0_f32; 2 * per_channel],
            vec![0.0_f32; 2 * per_channel],
        );
        let (mut version_a, mut version_b) = (RingVersion::fresh(), RingVersion::default());
        assert_eq!(version_b.mirror(&mut b, &version_a, &a, per_channel), 20);
        let mut cursor = 0;
        let mut value = 1.0;
        for round in 0..12 {
            // `b` renders four samples per channel, then becomes committed.
            for channel in 0..2 {
                for offset in 0..4 {
                    b[channel * per_channel + (cursor + offset) % per_channel] = value;
                    value += 1.0;
                }
            }
            version_b.wrote(cursor, 4, per_channel);
            cursor = (cursor + 4) % per_channel;
            core::mem::swap(&mut a, &mut b);
            core::mem::swap(&mut version_a, &mut version_b);
            // The next frame synchronizes the pending ring from it.
            let copied = version_b.mirror(&mut b, &version_a, &a, per_channel);
            assert_eq!(copied, 8, "round {round}");
            assert_eq!(a, b, "round {round}");
            // Synchronizing again copies nothing.
            assert_eq!(version_b.mirror(&mut b, &version_a, &a, per_channel), 0);
        }
        // A write the mirror did not see the base of copies everything.
        b[0] = -1.0;
        version_b.wrote(0, 1, per_channel);
        version_a.rewrote();
        assert_eq!(version_b.mirror(&mut b, &version_a, &a, per_channel), 20);
        assert_eq!(a, b);
    }
}
