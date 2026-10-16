//! The status bar's health strip: Program feed age, audio meter freshness,
//! stream throughput and drops, and local HLS disk space.
//!
//! Everything is derived from state the console already receives: the
//! Program monitor frame and when it arrived, the audio meter message
//! sequence, and the latest stream transport samples. Stream throughput is
//! measured here from the growth of each target's muxed byte counter between
//! transport samples; the protocol itself reports no bitrate, latency, or
//! recorder disk space, so the strip shows none.

use std::time::{Duration, Instant};

use fm_protocol::{MonitorTap, StreamRealizedState, WireStreamTargetId};

use crate::StudioUiState;
use crate::l10n::{Arg, Locale, Msg};
use crate::monitors::MONITOR_STALE_AFTER;
use crate::panels::latest_stream_sample;
use crate::theme::Tone;

/// Meter messages older than this read as stale, like the mixer header.
const METERS_STALE_AFTER: Duration =
    Duration::from_millis(crate::meter_ballistics::METER_STALE_AFTER_MS as u64);
/// Throughput is unknown once no transport sample has arrived for this long.
const RATE_STALE_AFTER: Duration = Duration::from_secs(5);
/// Samples closer together than this are too noisy to measure a rate from.
const MIN_RATE_WINDOW: Duration = Duration::from_millis(200);
/// Free HLS disk space below this reads amber.
const DISK_LOW_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Free HLS disk space below this reads as a fault.
const DISK_CRITICAL_BYTES: u64 = 512 * 1024 * 1024;

/// Byte-counter history of one stream target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TargetRate {
    target: WireStreamTargetId,
    bytes: u64,
    at: Instant,
    bits_per_second: Option<u64>,
}

/// Arrival times the strip needs across frames.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HealthTracker {
    meters: Option<(u64, Instant)>,
    stream_sequence: Option<u64>,
    stream_at: Option<Instant>,
    rates: Vec<TargetRate>,
}

impl HealthTracker {
    /// Records this frame's meter and stream messages observed at `now`.
    pub(crate) fn observe(&mut self, state: &StudioUiState, now: Instant) {
        match &state.audio_meters {
            Some(meters) if self.meters.map(|(sequence, _)| sequence) != Some(meters.sequence) => {
                self.meters = Some((meters.sequence, now));
            }
            Some(_) => {}
            None => self.meters = None,
        }
        let Some(status) = &state.stream_status else {
            self.stream_sequence = None;
            self.stream_at = None;
            self.rates.clear();
            return;
        };
        if self.stream_sequence == Some(status.sequence) {
            return;
        }
        self.stream_sequence = Some(status.sequence);
        self.stream_at = Some(now);
        let mut next = Vec::with_capacity(status.samples.len());
        for sample in &status.samples {
            let previous = self
                .rates
                .iter()
                .find(|rate| rate.target == sample.target)
                .copied();
            next.push(match previous {
                // Too soon to measure: keep the earlier reference point.
                Some(previous)
                    if sample.muxed_bytes >= previous.bytes
                        && now.saturating_duration_since(previous.at) < MIN_RATE_WINDOW =>
                {
                    previous
                }
                Some(previous) if sample.muxed_bytes >= previous.bytes => {
                    let elapsed = now.saturating_duration_since(previous.at);
                    let bits = u128::from(sample.muxed_bytes - previous.bytes) * 8;
                    let rate = bits * 1_000 / elapsed.as_millis().max(1);
                    TargetRate {
                        target: sample.target,
                        bytes: sample.muxed_bytes,
                        at: now,
                        bits_per_second: Some(u64::try_from(rate).unwrap_or(u64::MAX)),
                    }
                }
                // A new target, or a counter that restarted: no rate yet.
                _ => TargetRate {
                    target: sample.target,
                    bytes: sample.muxed_bytes,
                    at: now,
                    bits_per_second: None,
                },
            });
        }
        self.rates = next;
    }

    /// Total measured throughput across targets, while transport samples
    /// are current and at least one target has a measured rate.
    fn total_bits_per_second(&self, now: Instant) -> Option<u64> {
        let current = self
            .stream_at
            .is_some_and(|at| now.saturating_duration_since(at) < RATE_STALE_AFTER);
        if !current {
            return None;
        }
        self.rates
            .iter()
            .filter_map(|rate| rate.bits_per_second)
            .reduce(u64::saturating_add)
    }
}

/// One status lamp of the strip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthItem {
    pub(crate) label: String,
    pub(crate) tone: Tone,
    pub(crate) hover: String,
}

/// `850 kb/s` below one megabit, else `4.6 Mb/s` (decimal units, as
/// broadcast encoders quote bitrates).
pub(crate) fn format_bitrate(l: Locale, bits_per_second: u64) -> String {
    if bits_per_second < 1_000_000 {
        l.format(
            Msg::BitrateKbps,
            &[("value", Arg::Count(bits_per_second / 1_000))],
        )
    } else {
        #[allow(clippy::cast_precision_loss)] // display only
        let megabits = bits_per_second as f64 / 1_000_000.0;
        l.format(
            Msg::BitrateMbps,
            &[("value", Arg::Str(&l.decimal(megabits, 1)))],
        )
    }
}

fn age_text(l: Locale, age: Duration) -> String {
    l.seconds_tenths(u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

/// The strip's lamps in reading order. Stream and disk lamps appear only
/// when the show has stream targets or a local HLS output reports space.
pub(crate) fn health_items(
    l: Locale,
    state: &StudioUiState,
    tracker: &HealthTracker,
    now: Instant,
) -> Vec<HealthItem> {
    let mut items = vec![feed_item(l, state, now), meters_item(l, tracker, now)];
    if !state.streams.is_empty() {
        items.extend(stream_items(l, state, tracker, now));
    }
    if let Some(item) = disk_item(l, state) {
        items.push(item);
    }
    items
}

fn feed_item(l: Locale, state: &StudioUiState, now: Instant) -> HealthItem {
    let Some((image, at)) = &state.monitor_images[MonitorTap::Program.index()] else {
        return HealthItem {
            label: l.text(Msg::HealthFeedNone).to_owned(),
            tone: Tone::Idle,
            hover: l.text(Msg::HealthFeedNoneHover).to_owned(),
        };
    };
    let age = now.saturating_duration_since(*at);
    let age_text = age_text(l, age);
    let lapsed = age >= MONITOR_STALE_AFTER;
    HealthItem {
        label: if lapsed {
            l.format(Msg::HealthFeedStale, &[("age", Arg::Str(&age_text))])
        } else {
            l.text(Msg::HealthFeedLive).to_owned()
        },
        tone: if lapsed { Tone::Pending } else { Tone::Ready },
        hover: l.format(
            Msg::HealthFeedHover,
            &[
                ("width", Arg::Plain(u64::from(image.width))),
                ("height", Arg::Plain(u64::from(image.height))),
                ("age", Arg::Str(&age_text)),
            ],
        ),
    }
}

fn meters_item(l: Locale, tracker: &HealthTracker, now: Instant) -> HealthItem {
    let Some((_, at)) = tracker.meters else {
        return HealthItem {
            label: l.text(Msg::HealthMetersNone).to_owned(),
            tone: Tone::Idle,
            hover: l.text(Msg::MetersUnavailableHover).to_owned(),
        };
    };
    let age = now.saturating_duration_since(at);
    let stale = age > METERS_STALE_AFTER;
    HealthItem {
        label: l
            .text(if stale {
                Msg::HealthMetersStale
            } else {
                Msg::HealthMetersLive
            })
            .to_owned(),
        tone: if stale { Tone::Pending } else { Tone::Ready },
        hover: l.format(
            Msg::HealthMetersHover,
            &[("age", Arg::Str(&age_text(l, age)))],
        ),
    }
}

fn stream_items(
    l: Locale,
    state: &StudioUiState,
    tracker: &HealthTracker,
    now: Instant,
) -> Vec<HealthItem> {
    let realized = |stream: &fm_ui_model::StreamStatus| {
        latest_stream_sample(state.stream_status.as_ref(), stream.target)
            .map_or(stream.realized, |sample| sample.realized)
    };
    let live = state
        .streams
        .iter()
        .filter(|stream| realized(stream) == StreamRealizedState::Live)
        .count();
    let desired = state.streams.iter().any(|stream| stream.desired_running);
    let dropped: u64 = state.stream_status.as_ref().map_or(0, |status| {
        status
            .samples
            .iter()
            .map(|sample| sample.dropped_pairs)
            .fold(0, u64::saturating_add)
    });
    let rate = tracker.total_bits_per_second(now);
    let rate_text = rate.map(|rate| format_bitrate(l, rate));
    let rate_detail = rate_text.as_ref().map_or_else(
        || l.text(Msg::HealthRateUnknown).to_owned(),
        |rate| l.format(Msg::HealthRateMeasured, &[("rate", Arg::Str(rate))]),
    );
    let tone = if live > 0 {
        Tone::OnAir
    } else if desired {
        Tone::Pending
    } else {
        Tone::Idle
    };
    let mut items = vec![HealthItem {
        label: rate_text.as_ref().map_or_else(
            || l.text(Msg::HealthStreamUnknown).to_owned(),
            |rate| l.format(Msg::HealthStream, &[("rate", Arg::Str(rate))]),
        ),
        tone,
        hover: l.format(
            Msg::HealthStreamHover,
            &[
                ("live", Arg::from(live)),
                ("total", Arg::from(state.streams.len())),
                ("rate", Arg::Str(&rate_detail)),
                ("dropped", Arg::Count(dropped)),
            ],
        ),
    }];
    if dropped > 0 {
        items.push(HealthItem {
            label: l.format(Msg::HealthDropped, &[("count", Arg::Count(dropped))]),
            tone: Tone::Pending,
            hover: l.format(Msg::HealthDroppedHover, &[("count", Arg::Count(dropped))]),
        });
    }
    items
}

fn disk_item(l: Locale, state: &StudioUiState) -> Option<HealthItem> {
    let free = state
        .stream_status
        .as_ref()?
        .samples
        .iter()
        .filter_map(|sample| sample.local)
        .map(|local| local.free_bytes)
        // Zero means the daemon could not query the filesystem.
        .filter(|free| *free > 0)
        .min()?;
    let text = l.bytes(free);
    Some(HealthItem {
        label: l.format(Msg::HealthDisk, &[("free", Arg::Str(&text))]),
        tone: if free < DISK_CRITICAL_BYTES {
            Tone::Fault
        } else if free < DISK_LOW_BYTES {
            Tone::Pending
        } else {
            Tone::Ready
        },
        hover: l.format(Msg::HealthDiskHover, &[("free", Arg::Str(&text))]),
    })
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;
    use std::sync::Arc;

    use fm_protocol::{
        AudioMetersMessage, MonitorFrameOrigin, MonitorImage, ServerIdentity, StreamLocalStatus,
        StreamStatusMessage, StreamStatusSample, WireInputId,
    };
    use fm_ui_model::i18n::Language;

    use super::*;
    use crate::StudioConnectionStatus;

    fn server() -> ServerIdentity {
        ServerIdentity {
            engine_id: "e".to_owned(),
            project_id: "1".to_owned(),
            state_epoch: 1,
            log_id: "l".to_owned(),
        }
    }

    fn target(id: u128) -> WireStreamTargetId {
        WireStreamTargetId::new(NonZeroU128::new(id).unwrap())
    }

    fn sample(id: u128, realized: StreamRealizedState, bytes: u64) -> StreamStatusSample {
        StreamStatusSample {
            target: target(id),
            realized,
            connected: true,
            muxed_bytes: bytes,
            enqueued_pairs: 0,
            dropped_pairs: 0,
            failure: None,
            local: None,
            bitrate_kbps: 0,
            reconnects: 0,
            uptime_ms: 0,
        }
    }

    fn status(sequence: u64, samples: Vec<StreamStatusSample>) -> StreamStatusMessage {
        StreamStatusMessage {
            server: server(),
            sequence,
            samples,
        }
    }

    fn stream(id: u128, desired_running: bool) -> fm_ui_model::StreamStatus {
        fm_ui_model::StreamStatus {
            target: NonZeroU128::new(id).unwrap(),
            name: format!("Stream {id}"),
            desired_running,
            realized: StreamRealizedState::Stopped,
            detail: None,
        }
    }

    #[test]
    fn bitrates_read_in_kilobits_then_megabits() {
        let spanish = Locale::new(Language::Spanish);
        assert_eq!(format_bitrate(Locale::ENGLISH, 0), "0 kb/s");
        assert_eq!(format_bitrate(Locale::ENGLISH, 850_400), "850 kb/s");
        assert_eq!(format_bitrate(Locale::ENGLISH, 4_560_000), "4.6 Mb/s");
        assert_eq!(format_bitrate(spanish, 4_560_000), "4,6 Mb/s");
    }

    #[test]
    fn throughput_is_measured_between_transport_samples() {
        let start = Instant::now();
        let mut tracker = HealthTracker::default();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.stream_status = Some(status(1, vec![sample(1, StreamRealizedState::Live, 0)]));
        tracker.observe(&state, start);
        // One sample: nothing to measure against yet.
        assert_eq!(tracker.total_bits_per_second(start), None);
        // The same message again is not a new sample.
        tracker.observe(&state, start + Duration::from_secs(1));
        assert_eq!(tracker.total_bits_per_second(start), None);
        // 500 000 bytes in one second is 4 Mb/s.
        state.stream_status = Some(status(
            2,
            vec![
                sample(1, StreamRealizedState::Live, 500_000),
                sample(2, StreamRealizedState::Live, 10),
            ],
        ));
        let second = start + Duration::from_secs(1);
        tracker.observe(&state, second);
        assert_eq!(tracker.total_bits_per_second(second), Some(4_000_000));
        state.streams = vec![stream(1, true)];
        let items = health_items(Locale::ENGLISH, &state, &tracker, second);
        assert_eq!(items[2].label, "TX 4.0 Mb/s");
        assert_eq!(items[2].tone, Tone::OnAir);
        assert!(
            items[2]
                .hover
                .starts_with("Streams: 1 of 1 live · 4.0 Mb/s muxed in total"),
            "{}",
            items[2].hover
        );
        // Target 2 joins the total once it has two samples.
        state.stream_status = Some(status(
            3,
            vec![
                sample(1, StreamRealizedState::Live, 750_000),
                sample(2, StreamRealizedState::Live, 125_010),
            ],
        ));
        let third = second + Duration::from_millis(500);
        tracker.observe(&state, third);
        assert_eq!(tracker.total_bits_per_second(third), Some(6_000_000));
        // A restarted counter drops that target's rate.
        state.stream_status = Some(status(
            4,
            vec![
                sample(1, StreamRealizedState::Live, 0),
                sample(2, StreamRealizedState::Live, 125_010),
            ],
        ));
        let fourth = third + Duration::from_secs(1);
        tracker.observe(&state, fourth);
        assert_eq!(tracker.total_bits_per_second(fourth), Some(0));
        // Samples that stop arriving make the rate unknown.
        assert_eq!(
            tracker.total_bits_per_second(fourth + RATE_STALE_AFTER),
            None
        );
        // Without a transport message the history is dropped.
        state.stream_status = None;
        tracker.observe(&state, fourth);
        assert_eq!(tracker, HealthTracker::default());
    }

    #[test]
    fn samples_too_close_together_keep_the_earlier_reference() {
        let start = Instant::now();
        let mut tracker = HealthTracker::default();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.stream_status = Some(status(1, vec![sample(1, StreamRealizedState::Live, 0)]));
        tracker.observe(&state, start);
        state.stream_status = Some(status(2, vec![sample(1, StreamRealizedState::Live, 1_000)]));
        tracker.observe(&state, start + Duration::from_millis(50));
        assert_eq!(tracker.total_bits_per_second(start), None);
        state.stream_status = Some(status(
            3,
            vec![sample(1, StreamRealizedState::Live, 125_000)],
        ));
        let later = start + Duration::from_secs(1);
        tracker.observe(&state, later);
        assert_eq!(tracker.total_bits_per_second(later), Some(1_000_000));
    }

    #[test]
    fn idle_console_shows_feed_and_meter_lamps_unlit() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        let now = Instant::now();
        let items = health_items(Locale::ENGLISH, &state, &HealthTracker::default(), now);
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["NO PGM FEED", "NO METERS"]);
        assert!(items.iter().all(|item| item.tone == Tone::Idle));
    }

    #[test]
    fn feed_and_meters_turn_amber_when_they_stop_arriving() {
        let start = Instant::now();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.monitor_images[MonitorTap::Program.index()] = Some((
            MonitorImage {
                tap: MonitorTap::Program,
                sequence: 1,
                frame: 1,
                input: WireInputId::new(NonZeroU128::new(1).unwrap()),
                origin: MonitorFrameOrigin::Simulated,
                width: 640,
                height: 360,
                rgba: Arc::from(vec![0_u8; 4]),
            },
            start,
        ));
        state.audio_meters = Some(AudioMetersMessage {
            server: server(),
            sequence: 9,
            frame: 0,
            start_sample: 0,
            end_sample: 0,
            master: Vec::new(),
            inputs: Vec::new(),
            monitor: None,
        });
        let mut tracker = HealthTracker::default();
        tracker.observe(&state, start);
        let fresh = health_items(Locale::ENGLISH, &state, &tracker, start);
        assert_eq!(fresh[0].label, "PGM FEED");
        assert_eq!(fresh[0].tone, Tone::Ready);
        assert!(fresh[0].hover.contains("640×360"), "{}", fresh[0].hover);
        assert_eq!(fresh[1].label, "METERS");
        assert_eq!(fresh[1].tone, Tone::Ready);
        let later = start + Duration::from_millis(2_300);
        tracker.observe(&state, later);
        let lapsed = health_items(Locale::ENGLISH, &state, &tracker, later);
        assert_eq!(lapsed[0].label, "PGM FEED 2.3 s");
        assert_eq!(lapsed[0].tone, Tone::Pending);
        assert_eq!(lapsed[1].label, "METERS STALE");
        assert_eq!(lapsed[1].tone, Tone::Pending);
        let spanish = health_items(Locale::new(Language::Spanish), &state, &tracker, later);
        assert_eq!(spanish[0].label, "SEÑAL PGM 2,3 s");
        assert_eq!(spanish[1].label, "MEDIDORES OBSOLETOS");
    }

    #[test]
    fn stream_lamps_report_live_drops_and_low_disk() {
        let now = Instant::now();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.streams = vec![stream(1, true), stream(2, false)];
        let tracker = HealthTracker::default();
        // Desired but not yet live, no samples: amber with an unknown rate.
        let items = health_items(Locale::ENGLISH, &state, &tracker, now);
        assert_eq!(items[2].label, "TX —");
        assert_eq!(items[2].tone, Tone::Pending);
        assert_eq!(
            items[2].hover,
            "Streams: 0 of 2 live · no recent transport samples to measure the rate · 0 A/V pairs dropped"
        );
        assert_eq!(items.len(), 3);

        let mut live = sample(1, StreamRealizedState::Live, 0);
        live.dropped_pairs = 1_204;
        live.local = Some(StreamLocalStatus {
            segments: 4,
            target_duration_ms: 2_000,
            free_bytes: 1_610_612_736,
        });
        let mut unknown_disk = sample(2, StreamRealizedState::Stopped, 0);
        unknown_disk.local = Some(StreamLocalStatus {
            segments: 0,
            target_duration_ms: 0,
            free_bytes: 0,
        });
        state.stream_status = Some(status(1, vec![live, unknown_disk]));
        let items = health_items(Locale::ENGLISH, &state, &tracker, now);
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "NO PGM FEED",
                "NO METERS",
                "TX —",
                "DROP 1,204",
                "HLS DISK 1.5 GB"
            ]
        );
        assert_eq!(items[2].tone, Tone::OnAir);
        assert_eq!(items[3].tone, Tone::Pending);
        // 1.5 GB is below the 2 GB warning but above the critical floor.
        assert_eq!(items[4].tone, Tone::Pending);
    }
}
