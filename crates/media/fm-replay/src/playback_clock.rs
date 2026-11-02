//! A/B deck playback driven by a caller-supplied monotonic clock (OR-012).
//!
//! [`ReplayPlaybackClock`] converts monotonic nanoseconds into the elapsed
//! interval a caller applies to [`ReplayDecks`]. It owns no timer: the daemon,
//! engine, or test supplies the timestamp, so playback is deterministic and
//! does not read the wall clock inside the library.
//!
//! The first tick, and the first after [`ReplayPlaybackClock::reset`], only
//! records the time. A tick at or before the previous time applies nothing and
//! re-anchors at the new time, so a clock that steps backwards never moves a
//! deck backwards. Linked decks advance once; unlinked decks each advance by
//! the same interval.

use fm_frame::NormalizedTimestamp;

use crate::{ReplayChannelId, ReplayDecks};

/// Accumulates monotonic timestamps into per-tick replay playback intervals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplayPlaybackClock {
    last: Option<u64>,
}

impl ReplayPlaybackClock {
    #[must_use]
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// Forgets the last timestamp; the next tick applies nothing.
    pub const fn reset(&mut self) {
        self.last = None;
    }

    /// The last timestamp a tick observed.
    #[must_use]
    pub const fn last_nanos(&self) -> Option<u64> {
        self.last
    }

    /// Advances the decks to `now_nanos` and returns the applied interval.
    ///
    /// A first tick, a repeated timestamp, or a backwards timestamp returns
    /// zero. Linked decks are advanced once through channel A; otherwise each
    /// channel advances by the same interval.
    pub fn tick(
        &mut self,
        now_nanos: u64,
        decks: &mut ReplayDecks,
        live_edge: NormalizedTimestamp,
    ) -> u64 {
        let elapsed = self.last.map_or(0, |last| now_nanos.saturating_sub(last));
        self.last = Some(now_nanos);
        if elapsed == 0 {
            return 0;
        }
        decks.advance(ReplayChannelId::A, elapsed, live_edge);
        if !decks.linked() {
            decks.advance(ReplayChannelId::B, elapsed, live_edge);
        }
        elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CameraId, ChannelMode, ChannelTransport, EventId, PlaybackRate, ReplayEvent, TimelineRange,
    };
    use core::num::NonZeroU128;
    use std::collections::BTreeSet;

    const SECOND: u64 = 1_000_000_000;
    const SECOND_NS: i64 = 1_000_000_000;

    fn ts(nanos: i64) -> NormalizedTimestamp {
        NormalizedTimestamp::from_nanos(nanos)
    }

    fn camera(value: u128) -> CameraId {
        CameraId::new(NonZeroU128::new(value).unwrap())
    }

    fn event() -> ReplayEvent {
        ReplayEvent {
            id: EventId::new(NonZeroU128::new(1).unwrap()),
            name: "Clip".to_owned(),
            timeline: TimelineRange::new(ts(SECOND_NS), ts(2 * SECOND_NS)).unwrap(),
            angles: BTreeSet::from([camera(1), camera(2)]),
            preferred_angle: camera(1),
            tags: BTreeSet::new(),
            note: String::new(),
            folders: BTreeSet::new(),
        }
    }

    fn cue(decks: &mut ReplayDecks) {
        let event = event();
        decks.cue_event(ReplayChannelId::A, &event);
        decks.cue_event(ReplayChannelId::B, &event);
    }

    #[test]
    fn unlinked_decks_advance_together_and_backwards_ticks_reanchor() {
        let live = ts(5 * SECOND_NS);
        let mut decks = ReplayDecks::new(camera(1), camera(2), live);
        cue(&mut decks);
        decks.play(ReplayChannelId::A, PlaybackRate::FORWARD_1X);
        decks.play(ReplayChannelId::B, PlaybackRate::FORWARD_1X);
        let mut clock = ReplayPlaybackClock::new();
        assert_eq!(clock.tick(1_000, &mut decks, live), 0);
        assert_eq!(clock.tick(1_000 + SECOND / 4, &mut decks, live), SECOND / 4);
        assert_eq!(
            decks.channel(ReplayChannelId::A).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 4
        );
        assert_eq!(
            decks.channel(ReplayChannelId::B).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 4
        );

        // A clock that steps backwards teaches the new origin and moves
        // nothing.
        assert_eq!(clock.tick(1_000, &mut decks, live), 0);
        assert_eq!(
            decks.channel(ReplayChannelId::A).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 4
        );
        assert_eq!(clock.tick(1_000 + SECOND / 2, &mut decks, live), SECOND / 2);
        assert_eq!(
            decks.channel(ReplayChannelId::A).cursor.as_nanos(),
            SECOND_NS + 3 * SECOND_NS / 4
        );
    }

    #[test]
    fn linked_decks_advance_once_and_paused_decks_hold() {
        let live = ts(5 * SECOND_NS);
        let mut decks = ReplayDecks::new(camera(1), camera(2), live);
        cue(&mut decks);
        decks.set_linked(true);
        decks.play(ReplayChannelId::A, PlaybackRate::FORWARD_1X);
        let mut clock = ReplayPlaybackClock::new();
        assert_eq!(clock.tick(0, &mut decks, live), 0);
        assert_eq!(clock.tick(SECOND / 2, &mut decks, live), SECOND / 2);
        assert_eq!(
            decks.channel(ReplayChannelId::A).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 2,
            "linked playback is not applied twice"
        );
        assert_eq!(
            decks.channel(ReplayChannelId::B).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 2
        );

        decks.pause(ReplayChannelId::A);
        clock.reset();
        assert_eq!(clock.tick(0, &mut decks, live), 0);
        assert_eq!(clock.last_nanos(), Some(0));
        assert_eq!(clock.tick(SECOND, &mut decks, live), SECOND);
        assert_eq!(
            decks.channel(ReplayChannelId::A).cursor.as_nanos(),
            SECOND_NS + SECOND_NS / 2,
            "a paused deck holds its cursor"
        );
    }

    #[test]
    fn crossing_the_event_end_auto_returns_to_live() {
        let live = ts(5 * SECOND_NS);
        let mut decks = ReplayDecks::new(camera(1), camera(2), live);
        cue(&mut decks);
        decks.set_auto_return(ReplayChannelId::A, true);
        decks.play(ReplayChannelId::A, PlaybackRate::FORWARD_1X);
        let mut clock = ReplayPlaybackClock::new();
        assert_eq!(clock.tick(0, &mut decks, live), 0);
        assert_eq!(clock.tick(SECOND / 2, &mut decks, live), SECOND / 2);
        assert!(matches!(
            decks.channel(ReplayChannelId::A).mode,
            ChannelMode::Recorded { .. }
        ));
        assert_eq!(clock.tick(2 * SECOND, &mut decks, live), SECOND * 3 / 2);
        assert!(matches!(
            decks.channel(ReplayChannelId::A).mode,
            ChannelMode::Live
        ));
        assert_eq!(decks.channel(ReplayChannelId::A).cursor, live);
        assert_eq!(
            decks.channel(ReplayChannelId::A).transport,
            ChannelTransport::Stopped
        );
    }
}
