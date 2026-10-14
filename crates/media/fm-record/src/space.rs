//! Free-space preflight and remaining-recording-time estimation.
//!
//! Estimates are pure arithmetic over a free-byte sample and a bitrate, so the
//! caller owns the platform free-space query (through [`SpaceProbe`]) and the
//! sampling cadence. [`SpaceMonitor`] adds hysteresis so a level that has
//! worsened only improves again after the remaining time clears the level's
//! threshold by a margin.

use std::{io, path::Path, time::Duration};

use crate::session::SpaceProbe;

/// Thresholds on estimated remaining recording time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceThresholds {
    /// At or below this remaining time the level is [`SpaceLevel::Warning`].
    pub warning: Duration,
    /// At or below this remaining time the level is [`SpaceLevel::Critical`].
    pub critical: Duration,
    /// Bytes held back from the estimate for the filesystem and the manifest.
    pub reserve_bytes: u64,
    /// Extra remaining time needed before a worsened level improves.
    pub clear_margin: Duration,
}

impl Default for SpaceThresholds {
    fn default() -> Self {
        Self {
            warning: Duration::from_mins(30),
            critical: Duration::from_mins(5),
            reserve_bytes: 256 * 1024 * 1024,
            clear_margin: Duration::from_mins(2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpaceLevel {
    Normal,
    Warning,
    Critical,
    /// No usable bytes remain above the reserve.
    Exhausted,
}

/// Remaining-time estimate for one directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceEstimate {
    pub available_bytes: u64,
    /// Available bytes above the reserve.
    pub usable_bytes: u64,
    pub bitrate_bits_per_second: u64,
    /// `None` when the bitrate is zero (nothing is consumed).
    pub remaining: Option<Duration>,
    pub level: SpaceLevel,
}

/// Estimates how long a recording at `bitrate_bits_per_second` (all
/// streams plus container overhead) can continue in `available_bytes`.
#[must_use]
pub fn estimate_recording_space(
    available_bytes: u64,
    bitrate_bits_per_second: u64,
    thresholds: &SpaceThresholds,
) -> SpaceEstimate {
    let usable_bytes = available_bytes.saturating_sub(thresholds.reserve_bytes);
    let remaining = (bitrate_bits_per_second > 0).then(|| {
        let nanos =
            u128::from(usable_bytes) * 8 * 1_000_000_000 / u128::from(bitrate_bits_per_second);
        u64::try_from(nanos).map_or(Duration::MAX, Duration::from_nanos)
    });
    let level = level_for(usable_bytes, remaining, thresholds, Duration::ZERO);
    SpaceEstimate {
        available_bytes,
        usable_bytes,
        bitrate_bits_per_second,
        remaining,
        level,
    }
}

fn level_for(
    usable_bytes: u64,
    remaining: Option<Duration>,
    thresholds: &SpaceThresholds,
    margin: Duration,
) -> SpaceLevel {
    if usable_bytes == 0 {
        return SpaceLevel::Exhausted;
    }
    match remaining {
        Some(remaining) if remaining <= thresholds.critical.saturating_add(margin) => {
            SpaceLevel::Critical
        }
        Some(remaining) if remaining <= thresholds.warning.saturating_add(margin) => {
            SpaceLevel::Warning
        }
        Some(_) | None => SpaceLevel::Normal,
    }
}

/// Average bitrate of `bytes` written over `duration`, or `None` for an
/// empty span.
#[must_use]
pub fn observed_bitrate(bytes: u64, duration: Duration) -> Option<u64> {
    let nanos = duration.as_nanos();
    (nanos > 0).then(|| {
        let bits = u128::from(bytes) * 8 * 1_000_000_000 / nanos;
        u64::try_from(bits).unwrap_or(u64::MAX)
    })
}

/// A level change reported by [`SpaceMonitor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceTransition {
    pub from: Option<SpaceLevel>,
    pub to: SpaceLevel,
    pub estimate: SpaceEstimate,
}

/// Hysteretic level tracker over successive estimates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceMonitor {
    thresholds: SpaceThresholds,
    level: Option<SpaceLevel>,
}

impl SpaceMonitor {
    #[must_use]
    pub fn new(thresholds: SpaceThresholds) -> Self {
        Self {
            thresholds,
            level: None,
        }
    }

    #[must_use]
    pub fn level(&self) -> Option<SpaceLevel> {
        self.level
    }

    /// Evaluates a new free-space sample and returns the transition, if any.
    /// Worsening applies at once; improving requires clearing the better
    /// level's threshold by `clear_margin`.
    pub fn observe(
        &mut self,
        available_bytes: u64,
        bitrate_bits_per_second: u64,
    ) -> Option<SpaceTransition> {
        let estimate =
            estimate_recording_space(available_bytes, bitrate_bits_per_second, &self.thresholds);
        let next = match self.level {
            Some(current) if estimate.level < current => {
                // Improving: re-evaluate with the margin and never jump past
                // what the margin allows.
                level_for(
                    estimate.usable_bytes,
                    estimate.remaining,
                    &self.thresholds,
                    self.thresholds.clear_margin,
                )
                .min(current)
            }
            _ => estimate.level,
        };
        if self.level == Some(next) {
            return None;
        }
        let transition = SpaceTransition {
            from: self.level,
            to: next,
            estimate,
        };
        self.level = Some(next);
        Some(transition)
    }
}

/// Most remaining-time thresholds a [`RemainingTimeAlarm`] tracks.
pub const MAX_REMAINING_THRESHOLDS: usize = 8;

/// A threshold edge reported by [`RemainingTimeAlarm`]: the tightest crossed
/// threshold before and after, `None` meaning above every threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemainingTimeTransition {
    pub from: Option<Duration>,
    pub to: Option<Duration>,
    /// The remaining time that caused the edge; `None` is unbounded.
    pub remaining: Option<Duration>,
}

impl RemainingTimeTransition {
    /// Whether remaining time got worse (a tighter threshold was crossed).
    #[must_use]
    pub fn worsened(&self) -> bool {
        match (self.from, self.to) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(from), Some(to)) => to < from,
        }
    }
}

/// Hysteretic edge detector over a descending list of remaining-time
/// thresholds (for example 30, 10, and 2 minutes). Crossing a threshold
/// downward reports at once; returning above one requires clearing it by
/// `clear_margin`, so an estimate wobbling around a threshold reports one
/// edge, not a flap. Unbounded remaining time (nothing consumed) is above
/// every threshold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemainingTimeAlarm {
    thresholds: Vec<Duration>,
    clear_margin: Duration,
    /// How many thresholds are crossed.
    crossed: usize,
}

impl RemainingTimeAlarm {
    /// `thresholds` are sorted descending and de-duplicated; at most
    /// [`MAX_REMAINING_THRESHOLDS`] are kept (the largest ones).
    #[must_use]
    pub fn new(thresholds: &[Duration], clear_margin: Duration) -> Self {
        let mut thresholds = thresholds.to_vec();
        thresholds.sort_unstable_by(|left, right| right.cmp(left));
        thresholds.dedup();
        thresholds.truncate(MAX_REMAINING_THRESHOLDS);
        Self {
            thresholds,
            clear_margin,
            crossed: 0,
        }
    }

    #[must_use]
    pub fn thresholds(&self) -> &[Duration] {
        &self.thresholds
    }

    /// The tightest crossed threshold, `None` above every threshold.
    #[must_use]
    pub fn crossed(&self) -> Option<Duration> {
        self.crossed
            .checked_sub(1)
            .map(|index| self.thresholds[index])
    }

    /// Evaluates one remaining-time estimate and returns the edge, if any.
    pub fn observe(&mut self, remaining: Option<Duration>) -> Option<RemainingTimeTransition> {
        let worse = remaining.map_or(0, |remaining| {
            self.thresholds
                .iter()
                .take_while(|threshold| remaining <= **threshold)
                .count()
        });
        let next = if worse > self.crossed {
            worse
        } else {
            // Improving: a crossed threshold clears only past its margin.
            let held = remaining.map_or(0, |remaining| {
                self.thresholds
                    .iter()
                    .take_while(|threshold| {
                        remaining <= threshold.saturating_add(self.clear_margin)
                    })
                    .count()
            });
            held.min(self.crossed)
        };
        if next == self.crossed {
            return None;
        }
        let from = self.crossed();
        self.crossed = next;
        Some(RemainingTimeTransition {
            from,
            to: self.crossed(),
            remaining,
        })
    }
}

/// Preflight result for one root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootPreflight {
    pub root: usize,
    pub estimate: Option<SpaceEstimate>,
    /// Set when the free-space query failed.
    pub probe_error: Option<io::ErrorKind>,
    /// Whether the estimate covers `minimum` (unknown space never passes).
    pub sufficient: bool,
}

/// Preflight result over a session's roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpacePreflight {
    pub roots: Vec<RootPreflight>,
    pub minimum: Duration,
}

impl SpacePreflight {
    /// Whether the primary root alone covers the minimum duration.
    #[must_use]
    pub fn primary_sufficient(&self) -> bool {
        self.roots.first().is_some_and(|root| root.sufficient)
    }

    /// Index of the first root that covers the minimum duration.
    #[must_use]
    pub fn first_sufficient_root(&self) -> Option<usize> {
        self.roots
            .iter()
            .find(|root| root.sufficient)
            .map(|root| root.root)
    }

    /// Worst level among roots with a known estimate.
    #[must_use]
    pub fn worst_level(&self) -> Option<SpaceLevel> {
        self.roots
            .iter()
            .filter_map(|root| root.estimate.map(|estimate| estimate.level))
            .max()
    }
}

/// Probes every root and checks whether it can hold `minimum` of recording
/// at `bitrate_bits_per_second`. Roots on the same filesystem are not
/// de-duplicated, so per-root results must not be summed.
pub fn preflight_recording_space<P: SpaceProbe + ?Sized>(
    probe: &mut P,
    roots: &[impl AsRef<Path>],
    bitrate_bits_per_second: u64,
    minimum: Duration,
    thresholds: &SpaceThresholds,
) -> SpacePreflight {
    let roots = roots
        .iter()
        .enumerate()
        .map(
            |(root, directory)| match probe.available_bytes(directory.as_ref()) {
                Ok(available) => {
                    let estimate =
                        estimate_recording_space(available, bitrate_bits_per_second, thresholds);
                    let sufficient = estimate.level != SpaceLevel::Exhausted
                        && estimate
                            .remaining
                            .is_none_or(|remaining| remaining >= minimum);
                    RootPreflight {
                        root,
                        estimate: Some(estimate),
                        probe_error: None,
                        sufficient,
                    }
                }
                Err(error) => RootPreflight {
                    root,
                    estimate: None,
                    probe_error: Some(error.kind()),
                    sufficient: false,
                },
            },
        )
        .collect();
    SpacePreflight { roots, minimum }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn estimates_remaining_time_from_bitrate_and_reserve() {
        let thresholds = SpaceThresholds {
            reserve_bytes: 100 * MIB,
            ..SpaceThresholds::default()
        };
        // 10 Mbit/s = 1.25 MB/s; 1.25 GB usable lasts exactly 1000 s.
        let estimate = estimate_recording_space(1_250_000_000 + 100 * MIB, 10_000_000, &thresholds);
        assert_eq!(estimate.usable_bytes, 1_250_000_000);
        assert_eq!(estimate.remaining, Some(Duration::from_secs(1000)));
        assert_eq!(estimate.level, SpaceLevel::Warning);
        let exhausted = estimate_recording_space(50 * MIB, 10_000_000, &thresholds);
        assert_eq!(exhausted.level, SpaceLevel::Exhausted);
        assert_eq!(exhausted.remaining, Some(Duration::ZERO));
        let idle = estimate_recording_space(200 * MIB, 0, &thresholds);
        assert_eq!((idle.remaining, idle.level), (None, SpaceLevel::Normal));
        let huge = estimate_recording_space(u64::MAX, 1, &thresholds);
        assert_eq!(huge.remaining, Some(Duration::MAX));
    }

    #[test]
    fn levels_follow_thresholds_inclusively() {
        let thresholds = SpaceThresholds {
            warning: Duration::from_secs(100),
            critical: Duration::from_secs(10),
            reserve_bytes: 0,
            clear_margin: Duration::from_secs(5),
        };
        // 8 bit/s = 1 byte per second.
        let level = |bytes| estimate_recording_space(bytes, 8, &thresholds).level;
        assert_eq!(level(101), SpaceLevel::Normal);
        assert_eq!(level(100), SpaceLevel::Warning);
        assert_eq!(level(11), SpaceLevel::Warning);
        assert_eq!(level(10), SpaceLevel::Critical);
        assert_eq!(level(0), SpaceLevel::Exhausted);
    }

    #[test]
    fn monitor_worsens_at_once_and_improves_with_margin() {
        let thresholds = SpaceThresholds {
            warning: Duration::from_secs(100),
            critical: Duration::from_secs(10),
            reserve_bytes: 0,
            clear_margin: Duration::from_secs(5),
        };
        let mut monitor = SpaceMonitor::new(thresholds);
        let first = monitor.observe(500, 8).unwrap();
        assert_eq!((first.from, first.to), (None, SpaceLevel::Normal));
        assert_eq!(monitor.observe(400, 8), None);
        assert_eq!(monitor.observe(9, 8).unwrap().to, SpaceLevel::Critical);
        // Clears critical's 10 s but not 10 s + 5 s margin.
        assert_eq!(monitor.observe(14, 8), None);
        assert_eq!(monitor.observe(15, 8), None);
        let improved = monitor.observe(16, 8).unwrap();
        assert_eq!(
            (improved.from, improved.to),
            (Some(SpaceLevel::Critical), SpaceLevel::Warning)
        );
        assert_eq!(monitor.observe(104, 8), None);
        assert_eq!(monitor.observe(106, 8).unwrap().to, SpaceLevel::Normal);
        assert_eq!(monitor.observe(0, 8).unwrap().to, SpaceLevel::Exhausted);
    }

    #[test]
    fn remaining_time_alarm_reports_each_threshold_once_without_flapping() {
        let mins = Duration::from_mins;
        let mut alarm = RemainingTimeAlarm::new(&[mins(2), mins(30), mins(10)], mins(1));
        assert_eq!(alarm.thresholds(), &[mins(30), mins(10), mins(2)]);
        // Plenty of time, or nothing consumed: no edge.
        assert_eq!(alarm.observe(Some(mins(90))), None);
        assert_eq!(alarm.observe(None), None);
        let warned = alarm.observe(Some(mins(30))).unwrap();
        assert_eq!((warned.from, warned.to), (None, Some(mins(30))));
        assert!(warned.worsened());
        // Wobbling around 30 minutes inside the margin stays quiet.
        assert_eq!(
            alarm.observe(Some(mins(30) + Duration::from_secs(30))),
            None
        );
        assert_eq!(alarm.observe(Some(mins(29))), None);
        assert_eq!(alarm.observe(Some(mins(31))), None);
        // Skipping straight past two thresholds reports the tightest one.
        let jumped = alarm.observe(Some(mins(1))).unwrap();
        assert_eq!((jumped.from, jumped.to), (Some(mins(30)), Some(mins(2))));
        assert_eq!(alarm.observe(Some(mins(3))), None);
        let eased = alarm
            .observe(Some(mins(3) + Duration::from_secs(1)))
            .unwrap();
        assert_eq!((eased.from, eased.to), (Some(mins(2)), Some(mins(10))));
        assert!(!eased.worsened());
        let cleared = alarm.observe(None).unwrap();
        assert_eq!((cleared.from, cleared.to), (Some(mins(10)), None));
        assert_eq!(alarm.crossed(), None);
    }

    #[test]
    fn observed_bitrate_handles_empty_and_exact_spans() {
        assert_eq!(observed_bitrate(10, Duration::ZERO), None);
        assert_eq!(
            observed_bitrate(1_250_000, Duration::from_secs(1)),
            Some(10_000_000)
        );
        assert_eq!(
            observed_bitrate(1_250_000, Duration::from_millis(500)),
            Some(20_000_000)
        );
    }

    #[test]
    fn preflight_reports_each_root_and_never_passes_unknown_space() {
        let thresholds = SpaceThresholds {
            reserve_bytes: 0,
            ..SpaceThresholds::default()
        };
        let mut probe = |path: &Path| match path.to_str() {
            Some("small") => Ok(1_000),
            Some("large") => Ok(10 * 1024 * MIB),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        };
        let report = preflight_recording_space(
            &mut probe,
            &["small", "gone", "large"],
            8_000_000,
            Duration::from_secs(600),
            &thresholds,
        );
        assert!(!report.primary_sufficient());
        assert_eq!(report.first_sufficient_root(), Some(2));
        assert_eq!(report.roots[1].probe_error, Some(io::ErrorKind::NotFound));
        assert!(!report.roots[1].sufficient);
        assert_eq!(report.worst_level(), Some(SpaceLevel::Critical));
    }
}
