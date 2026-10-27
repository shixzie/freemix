//! The bounded lossy status feed of one Web session (Protocol 2.66).
//!
//! Web peers (the operator and tally pages) with `view_status` receive the
//! show's live status beside their durable records: Master/Program
//! `audio_meters` (Master only: no per-input or monitor meters; since
//! Protocol 2.94 with the loudness of Master and at most
//! [`WEB_LOUDNESS_INPUTS`] strips), the
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
//!
//! Protocol 2.90 adds the `title_phases` record (paced like a status) and
//! the answer to the page's own exact title preview request: its
//! `title_preview_frame` parts are handed over one record at a time ahead of
//! every status, never interleaved with another answer, and a newer answer
//! replaces one that has not started. The session's title preview rate
//! bounds them (a 320x180 preview is at most nine 32 KiB parts).
//!
//! Protocol 2.103 adds the gain reduction of Master and at most
//! [`WEB_LOUDNESS_INPUTS`] strips to the meter record, only for chains
//! with a running dynamics stage (gate, expander, compressor, or limiter),
//! which the operator page's Strip FX section shows.
//!
//! Protocol 2.101 adds the live source drift telemetry
//! (`source_sync_status`, AU-004), paced like a status; the daemon builds
//! it at most once a second, so it rarely waits for its interval (at most
//! 64 sources, a few kilobytes).
//!
//! Protocol 2.110 adds the daemon's health (`health_status`, RC-015),
//! paced like a status; the daemon builds it about once a second (at most
//! six alerts, six silences, and 16 notifications, well under a kilobyte).
//! The answer to the page's own alert action is a queued reply instead,
//! so [`WebFeed::discard_health_status`] drops an older sample still
//! waiting here first, keeping the page's `sequence` increasing.

use std::collections::VecDeque;
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
    /// Whether a record waits and its interval has passed at `now`.
    fn is_due(&self, now: Instant) -> bool {
        self.latest.is_some() && self.due.is_none_or(|due| now >= due)
    }

    fn take(&mut self, now: Instant, interval: Duration) -> Option<Vec<u8>> {
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
    /// The `title_phases` record (Protocol 2.90), paced like a status.
    phases: Slot,
    /// The `source_sync_status` record (Protocol 2.101), paced like a
    /// status.
    sync: Slot,
    /// The `health_status` record (Protocol 2.110), paced like a status.
    health: Slot,
    /// The parts of the exact title preview being handed over, one record
    /// each, in order (Protocol 2.90).
    preview: VecDeque<Vec<u8>>,
    /// The newest preview answer waiting for the one in flight: a newer
    /// answer replaces it, so a page typing fast gets only the latest.
    next_preview: Option<Vec<Vec<u8>>>,
}

impl WebFeed {
    pub(crate) fn offer_meters(&mut self, bytes: Vec<u8>) {
        self.meters.latest = Some(bytes);
    }

    /// Offers the newest `title_phases` record.
    pub(crate) fn offer_title_phases(&mut self, bytes: Vec<u8>) {
        self.phases.latest = Some(bytes);
    }

    /// Offers an exact title preview answer: its `title_preview_frame`
    /// parts (or the error its render met), one line each. Parts of an
    /// answer already started are all handed over first, so a page never
    /// sees two answers interleave.
    pub(crate) fn offer_preview(&mut self, lines: Vec<Vec<u8>>) {
        if self.preview.is_empty() {
            self.preview = lines.into();
        } else {
            self.next_preview = Some(lines);
        }
    }

    pub(crate) fn offer_stream_status(&mut self, bytes: Vec<u8>) {
        self.streams.latest = Some(bytes);
    }

    pub(crate) fn offer_recorder_status(&mut self, bytes: Vec<u8>) {
        self.recorders.latest = Some(bytes);
    }

    /// Offers the newest `source_sync_status` record.
    pub(crate) fn offer_source_sync_status(&mut self, bytes: Vec<u8>) {
        self.sync.latest = Some(bytes);
    }

    /// Offers the newest `health_status` sample.
    pub(crate) fn offer_health_status(&mut self, bytes: Vec<u8>) {
        self.health.latest = Some(bytes);
    }

    /// Drops a `health_status` sample not handed over yet, before the
    /// answer to the page's alert action is queued behind it.
    pub(crate) fn discard_health_status(&mut self) {
        self.health.latest = None;
    }

    /// The next record due at `now`: of the kinds due, the one that has
    /// been due the longest (one never sent first), ties going to meters,
    /// then recorders, then streams. A peer that takes records more slowly
    /// than the meter interval therefore still gets every kind in turn
    /// instead of only meters.
    pub(crate) fn next(&mut self, now: Instant) -> Option<Vec<u8>> {
        // A preview the page asked for goes first, part after part; its
        // rate is the session's preview rate.
        if self.preview.is_empty()
            && let Some(next) = self.next_preview.take()
        {
            self.preview = next.into();
        }
        if let Some(part) = self.preview.pop_front() {
            return Some(part);
        }
        let (slot, interval) = [
            (&mut self.meters, WEB_METER_INTERVAL),
            (&mut self.recorders, WEB_STATUS_INTERVAL),
            (&mut self.streams, WEB_STATUS_INTERVAL),
            (&mut self.phases, WEB_STATUS_INTERVAL),
            (&mut self.sync, WEB_STATUS_INTERVAL),
            (&mut self.health, WEB_STATUS_INTERVAL),
        ]
        .into_iter()
        .filter(|(slot, _)| slot.is_due(now))
        // `None` (never sent) orders before every instant; `min_by_key`
        // keeps the first of equal keys.
        .min_by_key(|(slot, _)| slot.due)?;
        slot.take(now, interval)
    }
}

/// The most strips whose loudness a Web meter record carries (Protocol
/// 2.94), the lowest input ids first: only strips heard on Program have a
/// reading, and the cap keeps the record a few kilobytes however many
/// inputs the show has.
pub(crate) const WEB_LOUDNESS_INPUTS: usize = 32;

/// The Master/Program part of `meters` a Web page shows: the same interval
/// and Master channels, the per-input meters of the first
/// [`WEB_LOUDNESS_INPUTS`] inputs (each strip's share of Program, which the
/// operator page draws as a level bar on its strip), and no monitor meters,
/// so the record stays small however many inputs the show has. Loudness (Protocol 2.94)
/// is kept for Master and the first [`WEB_LOUDNESS_INPUTS`] metered strips,
/// which the operator page shows with each bus's loudness reset; gain
/// reduction (Protocol 2.103) likewise for Master and the first
/// [`WEB_LOUDNESS_INPUTS`] strips whose chain has a running dynamics stage
/// (a bypassed chain or one with every dynamics stage off has no entry).
pub(crate) fn web_meters(meters: &AudioMetersMessage) -> AudioMetersMessage {
    use fm_protocol::AudioDspTarget;
    let master = meters
        .loudness
        .iter()
        .filter(|meter| meter.target == AudioDspTarget::Master);
    let strips = meters
        .loudness
        .iter()
        .filter(|meter| meter.target != AudioDspTarget::Master)
        .take(WEB_LOUDNESS_INPUTS);
    let running = |meter: &&fm_protocol::AudioDynamicsMeter| {
        meter.gate_millidb.is_some()
            || meter.expander_millidb.is_some()
            || meter.compressor_millidb.is_some()
            || meter.limiter_millidb.is_some()
    };
    let master_dynamics = meters
        .dynamics
        .iter()
        .filter(running)
        .filter(|meter| meter.target == AudioDspTarget::Master);
    let strip_dynamics = meters
        .dynamics
        .iter()
        .filter(running)
        .filter(|meter| meter.target != AudioDspTarget::Master)
        .take(WEB_LOUDNESS_INPUTS);
    AudioMetersMessage {
        server: meters.server.clone(),
        sequence: meters.sequence,
        frame: meters.frame,
        start_sample: meters.start_sample,
        end_sample: meters.end_sample,
        master: meters.master.clone(),
        inputs: meters
            .inputs
            .iter()
            .take(WEB_LOUDNESS_INPUTS)
            .cloned()
            .collect(),
        monitor: None,
        dynamics: master_dynamics.chain(strip_dynamics).copied().collect(),
        loudness: master.chain(strips).copied().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Web meter record keeps Master's channels and the loudness of
    /// Master and the first strips (Protocol 2.94), nothing per input else.
    #[test]
    fn web_meters_keep_master_and_capped_strip_loudness() {
        use fm_protocol::{
            AudioDspTarget, AudioLoudnessMeter, AudioMeterChannel, InputAudioMeters,
            ServerIdentity, WireInputId,
        };
        let loudness = |target| AudioLoudnessMeter {
            target,
            momentary_millilufs: Some(-23_000),
            short_term_millilufs: Some(-22_500),
            integrated_millilufs: None,
            true_peak_millidbtp: Some(-3_000),
            max_true_peak_millidbtp: Some(-1_500),
            loudness_range_millilu: Some(4_200),
        };
        let strip = |value: u128| {
            AudioDspTarget::Input(WireInputId::new(
                core::num::NonZeroU128::new(value).unwrap(),
            ))
        };
        let channel = AudioMeterChannel {
            peak_millionths: 500_000,
            rms_millionths: 250_000,
        };
        let mut all = vec![loudness(AudioDspTarget::Master)];
        all.extend((1..=40).map(|value| loudness(strip(value))));
        let meters = AudioMetersMessage {
            server: ServerIdentity {
                engine_id: "engine".to_owned(),
                project_id: "project".to_owned(),
                state_epoch: 1,
                log_id: "log".to_owned(),
            },
            sequence: 7,
            frame: 9,
            start_sample: 100,
            end_sample: 200,
            master: vec![channel],
            inputs: vec![InputAudioMeters {
                input: WireInputId::new(core::num::NonZeroU128::new(1).unwrap()),
                channels: vec![channel],
            }],
            monitor: None,
            dynamics: Vec::new(),
            loudness: all.clone(),
        };
        let web = web_meters(&meters);
        assert_eq!(web.master, meters.master);
        assert_eq!(web.inputs, meters.inputs);
        assert_eq!(web.loudness, all[..=WEB_LOUDNESS_INPUTS]);
        // The record still encodes as a valid wire line.
        assert!(fm_protocol::encode_line(&fm_protocol::WireMessage::AudioMeters(web)).is_ok());
    }

    /// Gain reduction (Protocol 2.103) reaches a Web page only for chains
    /// with a running dynamics stage: Master and at most
    /// [`WEB_LOUDNESS_INPUTS`] strips, the lowest input ids first.
    #[test]
    fn web_meters_keep_running_dynamics_of_master_and_capped_strips() {
        use fm_protocol::{AudioDspTarget, AudioDynamicsMeter, ServerIdentity, WireInputId};
        let strip = |value: u128| {
            AudioDspTarget::Input(WireInputId::new(
                core::num::NonZeroU128::new(value).unwrap(),
            ))
        };
        let meter = |target, compressor: Option<u32>| AudioDynamicsMeter {
            target,
            gate_millidb: None,
            expander_millidb: None,
            compressor_millidb: compressor,
            limiter_millidb: None,
        };
        let record = |dynamics: Vec<AudioDynamicsMeter>| AudioMetersMessage {
            server: ServerIdentity {
                engine_id: "engine".to_owned(),
                project_id: "project".to_owned(),
                state_epoch: 1,
                log_id: "log".to_owned(),
            },
            sequence: 1,
            frame: 1,
            start_sample: 0,
            end_sample: 480,
            master: vec![fm_protocol::AudioMeterChannel {
                peak_millionths: 1,
                rms_millionths: 1,
            }],
            inputs: Vec::new(),
            monitor: None,
            dynamics,
            loudness: Vec::new(),
        };
        // Master with every stage running, then 50 strips of which every
        // fifth has no running stage (as a bypassed chain would read).
        let master = AudioDynamicsMeter {
            target: AudioDspTarget::Master,
            gate_millidb: Some(0),
            expander_millidb: Some(1_500),
            compressor_millidb: Some(4_250),
            limiter_millidb: Some(200_000),
        };
        let mut all = vec![master];
        all.extend((1..=50).map(|value| {
            meter(
                strip(value),
                (value % 5 != 0).then_some(u32::try_from(value).unwrap() * 100),
            )
        }));
        let web = web_meters(&record(all.clone()));
        let running: Vec<_> = all
            .iter()
            .filter(|meter| {
                meter.target == AudioDspTarget::Master || meter.compressor_millidb.is_some()
            })
            .copied()
            .collect();
        assert_eq!(web.dynamics.len(), 1 + WEB_LOUDNESS_INPUTS);
        assert_eq!(web.dynamics, running[..=WEB_LOUDNESS_INPUTS]);
        assert!(web.dynamics[1..].iter().all(|meter| {
            meter.target != AudioDspTarget::Master && meter.compressor_millidb.is_some()
        }));
        assert!(fm_protocol::encode_line(&fm_protocol::WireMessage::AudioMeters(web)).is_ok());

        // Without a running Master chain only strips are sent, and a show
        // with no running dynamics sends none.
        let web = web_meters(&record(vec![
            meter(AudioDspTarget::Master, None),
            meter(strip(3), Some(2_000)),
        ]));
        assert_eq!(web.dynamics, [meter(strip(3), Some(2_000))]);
        assert_eq!(web_meters(&record(Vec::new())).dynamics, Vec::new());
    }

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

    /// A preview answer's parts go out back to back, ahead of statuses; a
    /// newer answer waits for the one started and replaces an older one
    /// still waiting.
    #[test]
    fn preview_parts_go_first_in_order_and_newest_answer_wins() {
        let mut feed = WebFeed::default();
        let now = Instant::now();
        feed.offer_title_phases(b"phases".to_vec());
        feed.offer_preview(vec![b"a1".to_vec(), b"a2".to_vec()]);
        assert_eq!(feed.next(now).as_deref(), Some(&b"a1"[..]));
        // Two newer answers arrive mid-answer: only the newest is kept.
        feed.offer_preview(vec![b"b1".to_vec()]);
        feed.offer_preview(vec![b"c1".to_vec(), b"c2".to_vec()]);
        assert_eq!(feed.next(now).as_deref(), Some(&b"a2"[..]));
        assert_eq!(feed.next(now).as_deref(), Some(&b"c1"[..]));
        assert_eq!(feed.next(now).as_deref(), Some(&b"c2"[..]));
        assert_eq!(feed.next(now).as_deref(), Some(&b"phases"[..]));
        assert!(feed.next(now).is_none());
        // Phases are paced like a status.
        feed.offer_title_phases(b"p2".to_vec());
        assert!(feed.next(now + Duration::from_millis(10)).is_none());
        assert_eq!(
            feed.next(now + WEB_STATUS_INTERVAL).as_deref(),
            Some(&b"p2"[..])
        );
    }

    /// AU-004: the live source drift telemetry is latest-wins and paced
    /// like a status.
    #[test]
    fn source_sync_is_latest_wins_and_paced_like_a_status() {
        let mut feed = WebFeed::default();
        let now = Instant::now();
        feed.offer_source_sync_status(b"y1".to_vec());
        feed.offer_source_sync_status(b"y2".to_vec());
        feed.offer_recorder_status(b"r1".to_vec());
        assert_eq!(feed.next(now).as_deref(), Some(&b"r1"[..]));
        assert_eq!(feed.next(now).as_deref(), Some(&b"y2"[..]));
        assert!(feed.next(now).is_none());
        feed.offer_source_sync_status(b"y3".to_vec());
        assert!(feed.next(now + Duration::from_millis(10)).is_none());
        assert_eq!(
            feed.next(now + WEB_STATUS_INTERVAL).as_deref(),
            Some(&b"y3"[..])
        );
    }

    /// RC-015 (Protocol 2.110): the daemon's health is latest-wins and
    /// paced like a status, and a waiting sample is dropped ahead of the
    /// answer to the page's own alert action.
    #[test]
    fn health_is_latest_wins_paced_and_discarded_before_an_answer() {
        let mut feed = WebFeed::default();
        let now = Instant::now();
        feed.offer_health_status(b"h1".to_vec());
        feed.offer_health_status(b"h2".to_vec());
        assert_eq!(feed.next(now).as_deref(), Some(&b"h2"[..]));
        feed.offer_health_status(b"h3".to_vec());
        assert!(feed.next(now + Duration::from_millis(10)).is_none());
        feed.discard_health_status();
        assert!(feed.next(now + WEB_STATUS_INTERVAL).is_none());
        feed.offer_health_status(b"h4".to_vec());
        assert_eq!(
            feed.next(now + WEB_STATUS_INTERVAL).as_deref(),
            Some(&b"h4"[..])
        );
    }

    /// A peer whose relay takes a record only every 100 ms (slower than the
    /// meter interval) still gets recorder and stream status: meters do not
    /// win every free turn.
    #[test]
    fn a_slow_peer_still_gets_every_kind() {
        let mut feed = WebFeed::default();
        let start = Instant::now();
        let (mut meters, mut recorders, mut streams) = (0, 0, 0);
        for step in 0..30 {
            let now = start + Duration::from_millis(100 * step);
            // The daemon offers fresh records of every kind meanwhile.
            feed.offer_meters(b"m".to_vec());
            feed.offer_recorder_status(b"r".to_vec());
            feed.offer_stream_status(b"s".to_vec());
            match feed.next(now).as_deref() {
                Some(b"m") => meters += 1,
                Some(b"r") => recorders += 1,
                Some(b"s") => streams += 1,
                other => panic!("{other:?} at step {step}"),
            }
        }
        assert!(meters >= 9, "{meters} meter records");
        assert!(recorders >= 6, "{recorders} recorder records");
        assert!(streams >= 6, "{streams} stream records");
        // Each kind keeps its own interval.
        assert!(recorders <= 12 && streams <= 12, "{recorders} {streams}");
    }
}
