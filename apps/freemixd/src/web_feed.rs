//! The bounded lossy status feed of one Web session (Protocol 2.66).
//!
//! Web peers (the operator and tally pages) with `view_status` receive the
//! show's live status beside their durable records: Master/Program
//! `audio_meters` (Master only: no per-input or monitor meters), the
//! `stream_status` destination samples, and the `recorder_status` slots.
//! Each kind is latest-wins: a newer record replaces one that has not been
//! handed to the gateway yet. The scheduler hands at most one feed record to
//! the relay at a time, only while the session's outbound queue is nearly
//! idle (so commands, results, heartbeats, and events always go first and a
//! slow browser simply gets fewer records), and each kind at most once per
//! its interval: meters at about 15 Hz, statuses at 4 Hz. A feed record is
//! not charged to the session's outbound rate window, so meters can never
//! rate-limit a command result; its bandwidth is bounded by these intervals
//! instead (a Master meter record is a few hundred bytes, a status record at
//! most a few kilobytes).

use std::time::{Duration, Instant};

use fm_protocol::AudioMetersMessage;

/// Shortest spacing of two meter records to one Web peer (about 15 Hz).
pub(crate) const WEB_METER_INTERVAL: Duration = Duration::from_millis(66);
/// Shortest spacing of two `stream_status` or two `recorder_status` records
/// to one Web peer.
pub(crate) const WEB_STATUS_INTERVAL: Duration = Duration::from_millis(250);
/// A feed record is handed over only while at most this many records are
/// queued for the peer and not yet acknowledged by its relay.
pub(crate) const WEB_FEED_MAX_QUEUED: usize = 2;

/// One kind of feed record.
#[derive(Default)]
struct Slot {
    latest: Option<Vec<u8>>,
    /// When the next record of this kind may be handed over.
    due: Option<Instant>,
}

impl Slot {
    fn take_due(&mut self, now: Instant, interval: Duration) -> Option<Vec<u8>> {
        if self.due.is_some_and(|due| now < due) {
            return None;
        }
        let bytes = self.latest.take()?;
        self.due = now.checked_add(interval);
        Some(bytes)
    }
}

/// The latest record of each kind waiting for one Web peer.
#[derive(Default)]
pub(crate) struct WebFeed {
    meters: Slot,
    recorders: Slot,
    streams: Slot,
}

impl WebFeed {
    pub(crate) fn offer_meters(&mut self, bytes: Vec<u8>) {
        self.meters.latest = Some(bytes);
    }

    pub(crate) fn offer_stream_status(&mut self, bytes: Vec<u8>) {
        self.streams.latest = Some(bytes);
    }

    pub(crate) fn offer_recorder_status(&mut self, bytes: Vec<u8>) {
        self.recorders.latest = Some(bytes);
    }

    /// The next record due at `now`: meters first, then recorders, then
    /// streams.
    pub(crate) fn next(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.meters
            .take_due(now, WEB_METER_INTERVAL)
            .or_else(|| self.recorders.take_due(now, WEB_STATUS_INTERVAL))
            .or_else(|| self.streams.take_due(now, WEB_STATUS_INTERVAL))
    }
}

/// The Master/Program part of `meters` a Web page shows: the same interval
/// and Master channels, without per-input or monitor meters, so the record
/// stays small however many inputs the show has.
pub(crate) fn web_meters(meters: &AudioMetersMessage) -> AudioMetersMessage {
    AudioMetersMessage {
        server: meters.server.clone(),
        sequence: meters.sequence,
        frame: meters.frame,
        start_sample: meters.start_sample,
        end_sample: meters.end_sample,
        master: meters.master.clone(),
        inputs: Vec::new(),
        monitor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_is_latest_wins_and_paced() {
        let mut feed = WebFeed::default();
        let start = Instant::now();
        assert!(feed.next(start).is_none());
        feed.offer_meters(b"m1".to_vec());
        feed.offer_meters(b"m2".to_vec());
        feed.offer_stream_status(b"s1".to_vec());
        feed.offer_recorder_status(b"r1".to_vec());
        // Meters first (only the newest), then recorders, then streams.
        assert_eq!(feed.next(start).as_deref(), Some(&b"m2"[..]));
        assert_eq!(feed.next(start).as_deref(), Some(&b"r1"[..]));
        assert_eq!(feed.next(start).as_deref(), Some(&b"s1"[..]));
        assert!(feed.next(start).is_none());
        // A meter offered within the interval waits for it.
        feed.offer_meters(b"m3".to_vec());
        feed.offer_stream_status(b"s2".to_vec());
        assert!(feed.next(start + Duration::from_millis(10)).is_none());
        let later = start + WEB_METER_INTERVAL;
        assert_eq!(feed.next(later).as_deref(), Some(&b"m3"[..]));
        assert!(feed.next(later).is_none());
        assert_eq!(
            feed.next(start + WEB_STATUS_INTERVAL).as_deref(),
            Some(&b"s2"[..])
        );
        // At most one meter record per interval however many are offered.
        let mut sent = 0;
        let mut now = start + WEB_STATUS_INTERVAL;
        let end = now + Duration::from_secs(1);
        while now < end {
            feed.offer_meters(b"m".to_vec());
            if feed.next(now).is_some() {
                sent += 1;
            }
            now += Duration::from_millis(5);
        }
        assert!((14..=16).contains(&sent), "{sent} meter records in 1 s");
    }
}
