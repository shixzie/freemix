//! Windowed statistics over caller-supplied monotonic timestamps.
//!
//! Every window covers the half-open interval `(now - window_millis, now]`.
//! Inputs must be non-decreasing in time; queries must not precede the latest
//! input. Retention is bounded by an explicit capacity, and a window reports
//! `saturated` whenever capacity eviction removed an entry that would still
//! fall inside the queried window, so undercounts are never silent.

use std::collections::VecDeque;

use crate::registry::{BucketHistogram, HistogramBuckets, StatisticsError, as_f64};

fn in_window(time: u64, now: u64, window_millis: u64) -> bool {
    time <= now && now - time < window_millis
}

/// Bounded log shared by the frame and depth windows.
#[derive(Clone, Debug)]
struct TimedLog<T> {
    window_millis: u64,
    capacity: usize,
    entries: VecDeque<(u64, T)>,
    latest: Option<u64>,
    last_evicted: Option<u64>,
}

impl<T> TimedLog<T> {
    fn new(window_millis: u64, capacity: usize) -> Result<Self, StatisticsError> {
        if window_millis == 0 || capacity == 0 {
            return Err(StatisticsError::InvalidWindow);
        }
        Ok(Self {
            window_millis,
            capacity,
            entries: VecDeque::new(),
            latest: None,
            last_evicted: None,
        })
    }

    fn check_time(&self, time: u64) -> Result<(), StatisticsError> {
        if self.latest.is_some_and(|latest| time < latest) {
            Err(StatisticsError::OutOfOrder)
        } else {
            Ok(())
        }
    }

    fn push(&mut self, time: u64, value: T) -> Result<(), StatisticsError> {
        self.check_time(time)?;
        self.latest = Some(time);
        while self
            .entries
            .front()
            .is_some_and(|(entry, _)| !in_window(*entry, time, self.window_millis))
        {
            self.entries.pop_front();
        }
        if self.entries.len() == self.capacity
            && let Some((evicted, _)) = self.entries.pop_front()
        {
            self.last_evicted = Some(evicted);
        }
        self.entries.push_back((time, value));
        Ok(())
    }

    fn window(&self, now: u64) -> Result<impl Iterator<Item = &(u64, T)>, StatisticsError> {
        self.check_time(now)?;
        let window_millis = self.window_millis;
        Ok(self
            .entries
            .iter()
            .filter(move |(time, _)| in_window(*time, now, window_millis)))
    }

    fn saturated(&self, now: u64) -> bool {
        self.last_evicted
            .is_some_and(|time| in_window(time, now, self.window_millis))
    }
}

/// Statistics for one frame window query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameWindowStats {
    /// Completed frames inside the window.
    pub frames: u64,
    /// Dropped frames inside the window.
    pub dropped: u64,
    /// `(frames - 1) * 1000 / (last - first)` over completion timestamps in
    /// the window; `None` with fewer than two frames or a zero span.
    pub fps: Option<f64>,
    /// `dropped / (frames + dropped)`; `None` when both are zero.
    pub drop_rate: Option<f64>,
    /// Capacity eviction removed an in-window entry, so counts are low.
    pub saturated: bool,
}

#[derive(Clone, Copy, Debug)]
enum FrameEvent {
    Completed,
    Dropped(u64),
}

/// Rolling frame rate and drop rate from frame-completion timestamps.
#[derive(Clone, Debug)]
pub struct FrameWindow {
    log: TimedLog<FrameEvent>,
}

impl FrameWindow {
    /// Creates a window retaining at most `capacity` events.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidWindow`] for a zero window or capacity.
    pub fn new(window_millis: u64, capacity: usize) -> Result<Self, StatisticsError> {
        Ok(Self {
            log: TimedLog::new(window_millis, capacity)?,
        })
    }

    /// Records one frame completion.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if older than the latest input.
    pub fn record_frame(&mut self, monotonic_millis: u64) -> Result<(), StatisticsError> {
        self.log.push(monotonic_millis, FrameEvent::Completed)
    }

    /// Records `count` dropped frames at one time.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if older than the latest input.
    pub fn record_drops(
        &mut self,
        monotonic_millis: u64,
        count: u64,
    ) -> Result<(), StatisticsError> {
        if count == 0 {
            return self.log.check_time(monotonic_millis);
        }
        self.log.push(monotonic_millis, FrameEvent::Dropped(count))
    }

    /// Summarizes `(now - window, now]`.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if `now` precedes the latest input.
    pub fn stats(&self, now: u64) -> Result<FrameWindowStats, StatisticsError> {
        let mut frames = 0_u64;
        let mut dropped = 0_u64;
        let mut first = None;
        let mut last = None;
        for (time, event) in self.log.window(now)? {
            match event {
                FrameEvent::Completed => {
                    frames += 1;
                    first.get_or_insert(*time);
                    last = Some(*time);
                }
                FrameEvent::Dropped(count) => dropped = dropped.saturating_add(*count),
            }
        }
        let fps = match (first, last) {
            (Some(first), Some(last)) if frames >= 2 && last > first => {
                Some(as_f64(frames - 1) * 1_000.0 / as_f64(last - first))
            }
            _ => None,
        };
        let total = frames.saturating_add(dropped);
        let drop_rate = (total > 0).then(|| as_f64(dropped) / as_f64(total));
        Ok(FrameWindowStats {
            frames,
            dropped,
            fps,
            drop_rate,
            saturated: self.log.saturated(now),
        })
    }
}

/// Statistics for one queue-depth window query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthWindowStats {
    pub samples: usize,
    /// Latest depth inside the window.
    pub current: Option<u64>,
    /// Maximum depth inside the window.
    pub maximum: Option<u64>,
    pub saturated: bool,
}

/// Rolling maximum queue depth from timestamped depth samples.
#[derive(Clone, Debug)]
pub struct QueueDepthWindow {
    log: TimedLog<u64>,
}

impl QueueDepthWindow {
    /// Creates a window retaining at most `capacity` samples.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidWindow`] for a zero window or capacity.
    pub fn new(window_millis: u64, capacity: usize) -> Result<Self, StatisticsError> {
        Ok(Self {
            log: TimedLog::new(window_millis, capacity)?,
        })
    }

    /// Records a depth sample.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if older than the latest input.
    pub fn record(&mut self, monotonic_millis: u64, depth: u64) -> Result<(), StatisticsError> {
        self.log.push(monotonic_millis, depth)
    }

    /// Summarizes `(now - window, now]`.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if `now` precedes the latest input.
    pub fn stats(&self, now: u64) -> Result<DepthWindowStats, StatisticsError> {
        let mut samples = 0;
        let mut current = None;
        let mut maximum: Option<u64> = None;
        for (_, depth) in self.log.window(now)? {
            samples += 1;
            current = Some(*depth);
            maximum = Some(maximum.map_or(*depth, |maximum| maximum.max(*depth)));
        }
        Ok(DepthWindowStats {
            samples,
            current,
            maximum,
            saturated: self.log.saturated(now),
        })
    }
}

/// Fixed-bucket histogram over a sliding window of time slices.
///
/// Time is divided into slices `[k * slice_millis, (k + 1) * slice_millis)`.
/// A query at `now` merges the slice containing `now` and the preceding
/// `slice_count - 1` slices, so the window spans between
/// `(slice_count - 1) * slice_millis` and `slice_count * slice_millis`.
/// At most `slice_count` slices are retained.
#[derive(Clone, Debug)]
pub struct WindowedHistogram {
    buckets: HistogramBuckets,
    slice_millis: u64,
    slice_count: u64,
    slices: VecDeque<(u64, BucketHistogram)>,
    latest: Option<u64>,
}

impl WindowedHistogram {
    /// Creates an empty windowed histogram.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidWindow`] for a zero slice length or count.
    pub fn new(
        buckets: HistogramBuckets,
        slice_millis: u64,
        slice_count: u64,
    ) -> Result<Self, StatisticsError> {
        if slice_millis == 0 || slice_count == 0 {
            return Err(StatisticsError::InvalidWindow);
        }
        Ok(Self {
            buckets,
            slice_millis,
            slice_count,
            slices: VecDeque::new(),
            latest: None,
        })
    }

    /// Records one observation.
    ///
    /// # Errors
    ///
    /// Rejects out-of-order time and non-finite values or sums.
    pub fn observe(&mut self, monotonic_millis: u64, value: f64) -> Result<(), StatisticsError> {
        if self.latest.is_some_and(|latest| monotonic_millis < latest) {
            return Err(StatisticsError::OutOfOrder);
        }
        if !value.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        let slice = monotonic_millis / self.slice_millis;
        if self.slices.back().is_none_or(|(index, _)| *index != slice) {
            let mut histogram = BucketHistogram::new(self.buckets.clone());
            histogram.observe(value)?;
            self.slices.push_back((slice, histogram));
        } else if let Some((_, histogram)) = self.slices.back_mut() {
            histogram.observe(value)?;
        }
        self.latest = Some(monotonic_millis);
        let oldest_kept = slice.saturating_sub(self.slice_count - 1);
        while self
            .slices
            .front()
            .is_some_and(|(index, _)| *index < oldest_kept)
        {
            self.slices.pop_front();
        }
        Ok(())
    }

    /// Merges the slices inside the window ending at `now`.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::OutOfOrder`] if `now` precedes the latest
    /// observation, or a merge error if the merged sum is non-finite.
    pub fn snapshot(&self, now: u64) -> Result<BucketHistogram, StatisticsError> {
        if self.latest.is_some_and(|latest| now < latest) {
            return Err(StatisticsError::OutOfOrder);
        }
        let current = now / self.slice_millis;
        let oldest = current.saturating_sub(self.slice_count - 1);
        let mut merged = BucketHistogram::new(self.buckets.clone());
        for (_, histogram) in self.slices.iter().filter(|(index, _)| *index >= oldest) {
            merged.merge(histogram)?;
        }
        Ok(merged)
    }

    /// Windowed percentile using [`BucketHistogram::percentile`] interpolation.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::snapshot`] errors.
    pub fn percentile(&self, now: u64, percentile: u8) -> Result<Option<f64>, StatisticsError> {
        Ok(self.snapshot(now)?.percentile(percentile))
    }
}
