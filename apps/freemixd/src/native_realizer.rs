//! The native realizer's frame handoff (structural item 1).
//!
//! The control pass loop owns `fm-control`, peers, the journal, and the
//! durable project. Frame realization is moving behind a bounded,
//! latest-wins channel to a dedicated realizer thread that will own the GPU
//! runtime, the project plan, and the audio mixer. This module is that
//! channel's first form: a one-slot [`LatestSlot`] of [`NativePreviewJob`]s
//! for the Preview composite and its monitor tap, which the frame defers
//! until the pass's peer work is done. A newer frame replaces an older one,
//! so a loop that falls behind never builds a backlog of stale composites; a
//! job that cannot start before the next Program deadline stays queued for
//! the next pass.
//!
//! [`NativeFrameJob`] and [`NativeFrameOutcome`] will generalize this slot
//! when the GPU runtime, plan, and mixer move to the thread. Keeping the
//! handoff bounded and latest-wins from the start is what lets the control
//! loop hand frames over without ever blocking on a slow realizer.

use fm_engine::FrameResult;
use fm_types::InputId;

/// A Preview composite the frame deferred, with the monitor tap frame it
/// also feeds (Protocol 2.21).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativePreviewJob {
    pub frame: FrameResult,
    pub preview: InputId,
    pub tap_frame: Option<u64>,
}

/// One bounded latest-wins slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LatestSlot<T> {
    slot: Option<T>,
}

impl<T> LatestSlot<T> {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { slot: None }
    }

    /// Queues `value`, replacing any older one.
    pub(crate) fn submit(&mut self, value: T) {
        self.slot = Some(value);
    }

    /// Takes the queued value, if any.
    pub(crate) fn take(&mut self) -> Option<T> {
        self.slot.take()
    }

    /// Puts `value` back after it could not run, as the newest value.
    pub(crate) fn requeue(&mut self, value: T) {
        self.submit(value);
    }

    /// Drops the queued value.
    pub(crate) fn clear(&mut self) {
        self.slot = None;
    }
}

/// The deferred Preview composite handoff.
pub(crate) type NativePreviewQueue = LatestSlot<NativePreviewJob>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handoff_is_bounded_and_latest_wins() {
        let mut slot = LatestSlot::new();
        assert!(slot.take().is_none());
        slot.submit(1);
        slot.submit(2);
        assert_eq!(slot.take(), Some(2), "the newer value replaced 1");
        assert!(slot.take().is_none());
        slot.submit(3);
        let value = slot.take().unwrap();
        slot.requeue(value);
        assert_eq!(slot.take(), Some(3));
        slot.submit(4);
        slot.clear();
        assert!(slot.take().is_none());
    }
}
