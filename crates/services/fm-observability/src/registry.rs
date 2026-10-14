//! Typed metric registry keyed by stable metric IDs and bounded label sets.
//!
//! The registry is caller-owned and clock-free: every update carries a
//! caller-supplied monotonic millisecond timestamp. Series, metrics, labels,
//! and histogram buckets are all bounded, and every rejected input returns an
//! explicit [`StatisticsError`].

use std::{collections::BTreeMap, error::Error, fmt};

/// Maximum byte length of a [`MetricId`] or rule identifier.
pub const MAX_ID_BYTES: usize = 64;
/// Maximum number of labels in one [`LabelSet`].
pub const MAX_LABELS: usize = 8;
/// Maximum byte length of one label key.
pub const MAX_LABEL_KEY_BYTES: usize = 32;
/// Maximum byte length of one label value.
pub const MAX_LABEL_VALUE_BYTES: usize = 128;
/// Maximum number of edges in [`HistogramBuckets`] (so at most 32 buckets).
pub const MAX_BUCKET_EDGES: usize = 33;

/// Validates the shared identifier grammar: `[a-z][a-z0-9_.]*`, no trailing
/// or doubled `.`, at most [`MAX_ID_BYTES`] bytes.
pub(crate) fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID_BYTES
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'.'
        })
        && !value.ends_with('.')
        && !value.contains("..")
}

/// Stable metric identifier such as `output.frames_dropped`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MetricId(String);

impl MetricId {
    /// Validates a stable metric identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidMetricId`] unless the value matches
    /// `[a-z][a-z0-9_.]*` (no trailing or doubled `.`) within
    /// [`MAX_ID_BYTES`] bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, StatisticsError> {
        let value = value.into();
        if valid_identifier(&value) {
            Ok(Self(value))
        } else {
            Err(StatisticsError::InvalidMetricId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MetricId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Bounded, sorted set of label key/value pairs.
///
/// Keys match `[a-z][a-z0-9_]*` within [`MAX_LABEL_KEY_BYTES`]. Values are
/// 1..=[`MAX_LABEL_VALUE_BYTES`] bytes of printable ASCII excluding `"`, `\`,
/// `{`, `}`, `,`, and `=` so the text exporter never needs escaping. Values
/// are still passed through the [`crate::Redactor`] at export time.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LabelSet(BTreeMap<String, String>);

impl LabelSet {
    #[must_use]
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Builds a label set from pairs.
    ///
    /// # Errors
    ///
    /// Propagates the first [`Self::with`] failure.
    pub fn from_pairs<K, V>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, StatisticsError>
    where
        K: Into<String>,
        V: Into<String>,
    {
        pairs
            .into_iter()
            .try_fold(Self::new(), |labels, (key, value)| labels.with(key, value))
    }

    /// Adds one label.
    ///
    /// # Errors
    ///
    /// Rejects invalid keys or values, duplicate keys, and more than
    /// [`MAX_LABELS`] labels.
    pub fn with(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, StatisticsError> {
        let key = key.into();
        let value = value.into();
        let key_bytes = key.as_bytes();
        let key_valid = !key_bytes.is_empty()
            && key_bytes.len() <= MAX_LABEL_KEY_BYTES
            && key_bytes[0].is_ascii_lowercase()
            && key_bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
        if !key_valid {
            return Err(StatisticsError::InvalidLabelKey);
        }
        let value_valid = !value.is_empty()
            && value.len() <= MAX_LABEL_VALUE_BYTES
            && value.bytes().all(|byte| {
                (0x20..0x7f).contains(&byte)
                    && !matches!(byte, b'"' | b'\\' | b'{' | b'}' | b',' | b'=')
            });
        if !value_valid {
            return Err(StatisticsError::InvalidLabelValue);
        }
        if self.0.contains_key(&key) {
            return Err(StatisticsError::DuplicateLabel);
        }
        if self.0.len() == MAX_LABELS {
            return Err(StatisticsError::TooManyLabels);
        }
        self.0.insert(key, value);
        Ok(self)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates labels in ascending key order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Unique identity of one series: metric ID plus label set.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SeriesKey {
    pub id: MetricId,
    pub labels: LabelSet,
}

impl SeriesKey {
    #[must_use]
    pub const fn new(id: MetricId, labels: LabelSet) -> Self {
        Self { id, labels }
    }
}

/// Type of a registered metric.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SeriesKind {
    Counter,
    Gauge,
    Histogram,
}

impl SeriesKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

/// Fixed, strictly increasing, finite histogram bucket edges.
///
/// With edges `e0 < e1 < … < eN`, bucket 0 is the closed interval `[e0, e1]`
/// and bucket `i > 0` is the half-open interval `(e_i, e_{i+1}]`. Values below
/// `e0` count as underflow and values above `eN` count as overflow.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramBuckets {
    edges: Vec<f64>,
}

impl HistogramBuckets {
    /// Validates bucket edges.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidBuckets`] unless there are
    /// 2..=[`MAX_BUCKET_EDGES`] finite, strictly increasing edges.
    pub fn new(edges: &[f64]) -> Result<Self, StatisticsError> {
        let valid = (2..=MAX_BUCKET_EDGES).contains(&edges.len())
            && edges.iter().all(|edge| edge.is_finite())
            && edges.windows(2).all(|pair| pair[0] < pair[1]);
        if valid {
            Ok(Self {
                edges: edges.to_vec(),
            })
        } else {
            Err(StatisticsError::InvalidBuckets)
        }
    }

    #[must_use]
    pub fn edges(&self) -> &[f64] {
        &self.edges
    }

    /// Number of finite buckets (edges minus one).
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.edges.len() - 1
    }

    fn locate(&self, value: f64) -> Slot {
        let first = self.edges[0];
        let last = self.edges[self.edges.len() - 1];
        if value < first {
            return Slot::Underflow;
        }
        if value > last {
            return Slot::Overflow;
        }
        // First bucket whose upper edge is >= value.
        let index = self.edges[1..].partition_point(|upper| *upper < value);
        Slot::Bucket(index)
    }
}

enum Slot {
    Underflow,
    Bucket(usize),
    Overflow,
}

/// Fixed-bucket histogram with exact counts, sum, and extrema.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketHistogram {
    buckets: HistogramBuckets,
    counts: Vec<u64>,
    underflow: u64,
    overflow: u64,
    count: u64,
    sum: f64,
    minimum: Option<f64>,
    maximum: Option<f64>,
}

impl BucketHistogram {
    #[must_use]
    pub fn new(buckets: HistogramBuckets) -> Self {
        let counts = vec![0; buckets.bucket_count()];
        Self {
            buckets,
            counts,
            underflow: 0,
            overflow: 0,
            count: 0,
            sum: 0.0,
            minimum: None,
            maximum: None,
        }
    }

    /// Records one finite observation.
    ///
    /// # Errors
    ///
    /// Rejects non-finite values, a sum that would become non-finite, and a
    /// count that would overflow `u64`. The histogram is unchanged on error.
    pub fn observe(&mut self, value: f64) -> Result<(), StatisticsError> {
        if !value.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        let sum = self.sum + value;
        if !sum.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        let count = self
            .count
            .checked_add(1)
            .ok_or(StatisticsError::CounterOverflow)?;
        let slot = match self.buckets.locate(value) {
            Slot::Underflow => &mut self.underflow,
            Slot::Overflow => &mut self.overflow,
            Slot::Bucket(index) => &mut self.counts[index],
        };
        // `count` bounds every slot, so this cannot overflow once `count` did not.
        *slot += 1;
        self.count = count;
        self.sum = sum;
        self.minimum = Some(self.minimum.map_or(value, |minimum| minimum.min(value)));
        self.maximum = Some(self.maximum.map_or(value, |maximum| maximum.max(value)));
        Ok(())
    }

    /// Adds another histogram's counts into this one.
    ///
    /// # Errors
    ///
    /// Rejects different bucket layouts, count overflow, and a non-finite sum.
    /// The histogram is unchanged on error.
    pub fn merge(&mut self, other: &Self) -> Result<(), StatisticsError> {
        if self.buckets != other.buckets {
            return Err(StatisticsError::BucketMismatch);
        }
        let count = self
            .count
            .checked_add(other.count)
            .ok_or(StatisticsError::CounterOverflow)?;
        let sum = self.sum + other.sum;
        if !sum.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        for (mine, theirs) in self.counts.iter_mut().zip(&other.counts) {
            *mine += theirs;
        }
        self.underflow += other.underflow;
        self.overflow += other.overflow;
        self.count = count;
        self.sum = sum;
        self.minimum = merge_extreme(self.minimum, other.minimum, f64::min);
        self.maximum = merge_extreme(self.maximum, other.maximum, f64::max);
        Ok(())
    }

    #[must_use]
    pub const fn buckets(&self) -> &HistogramBuckets {
        &self.buckets
    }

    /// Per-bucket counts aligned with [`HistogramBuckets::bucket_count`].
    #[must_use]
    pub fn counts(&self) -> &[u64] {
        &self.counts
    }

    #[must_use]
    pub const fn underflow(&self) -> u64 {
        self.underflow
    }

    #[must_use]
    pub const fn overflow(&self) -> u64 {
        self.overflow
    }

    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub const fn sum(&self) -> f64 {
        self.sum
    }

    #[must_use]
    pub const fn minimum(&self) -> Option<f64> {
        self.minimum
    }

    #[must_use]
    pub const fn maximum(&self) -> Option<f64> {
        self.maximum
    }

    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / as_f64(self.count))
    }

    /// Estimates a percentile from bucket counts by linear interpolation.
    ///
    /// Returns `None` when the histogram is empty or `percentile` is not in
    /// `1..=100`. With `n` total observations (underflow and overflow
    /// included), the target rank is `r = n * percentile / 100` as an `f64`.
    /// Walking underflow, then buckets in edge order, with `C` the cumulative
    /// count before a slot:
    ///
    /// * if `r <= underflow`, the result is the lowest edge `e0`;
    /// * for the first bucket `[lo, hi]` with count `c > 0` and `r <= C + c`,
    ///   the result is `lo + (hi - lo) * (r - C) / c`;
    /// * otherwise (the rank lands in overflow) the result is the highest edge.
    ///
    /// The estimate is therefore always clamped to `[e0, eN]`.
    #[must_use]
    pub fn percentile(&self, percentile: u8) -> Option<f64> {
        if self.count == 0 || !(1..=100).contains(&percentile) {
            return None;
        }
        let rank = as_f64(self.count) * f64::from(percentile) / 100.0;
        let edges = self.buckets.edges();
        let mut cumulative = as_f64(self.underflow);
        if rank <= cumulative {
            return Some(edges[0]);
        }
        for (index, count) in self.counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let count = as_f64(*count);
            if rank <= cumulative + count {
                let lower = edges[index];
                let upper = edges[index + 1];
                return Some(lower + (upper - lower) * (rank - cumulative) / count);
            }
            cumulative += count;
        }
        Some(edges[edges.len() - 1])
    }
}

fn merge_extreme(left: Option<f64>, right: Option<f64>, pick: fn(f64, f64) -> f64) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(pick(left, right)),
        (value, None) | (None, value) => value,
    }
}

/// Lossless for counts below 2^53, which bounded deterministic use never reaches.
#[allow(clippy::cast_precision_loss)]
pub(crate) const fn as_f64(value: u64) -> f64 {
    value as f64
}

/// Registration-time shape of a metric.
#[derive(Clone, Debug, PartialEq)]
pub enum MetricDescriptor {
    Counter,
    Gauge,
    Histogram(HistogramBuckets),
}

impl MetricDescriptor {
    #[must_use]
    pub const fn kind(&self) -> SeriesKind {
        match self {
            Self::Counter => SeriesKind::Counter,
            Self::Gauge => SeriesKind::Gauge,
            Self::Histogram(_) => SeriesKind::Histogram,
        }
    }
}

/// Current value of one series.
#[derive(Clone, Debug, PartialEq)]
pub enum SeriesValue {
    /// Monotonic integer total.
    Counter(u64),
    /// Latest finite value.
    Gauge(f64),
    Histogram(BucketHistogram),
}

impl SeriesValue {
    #[must_use]
    pub const fn kind(&self) -> SeriesKind {
        match self {
            Self::Counter(_) => SeriesKind::Counter,
            Self::Gauge(_) => SeriesKind::Gauge,
            Self::Histogram(_) => SeriesKind::Histogram,
        }
    }
}

/// One series' value plus update bookkeeping.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesEntry {
    pub value: SeriesValue,
    /// Caller-supplied time of the latest accepted update.
    pub updated_millis: u64,
    /// Number of accepted updates.
    pub updates: u64,
}

/// Registry bounds; every field must be non-zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryLimits {
    pub max_metrics: usize,
    pub max_series: usize,
    pub max_series_per_metric: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            max_metrics: 64,
            max_series: 1_024,
            max_series_per_metric: 64,
        }
    }
}

/// Caller-owned typed metric registry with deterministic sorted iteration.
#[derive(Clone, Debug)]
pub struct MetricRegistry {
    limits: RegistryLimits,
    descriptors: BTreeMap<MetricId, MetricDescriptor>,
    series: BTreeMap<SeriesKey, SeriesEntry>,
    series_per_metric: BTreeMap<MetricId, usize>,
}

impl MetricRegistry {
    /// Creates an empty registry.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::InvalidLimits`] when any limit is zero.
    pub fn new(limits: RegistryLimits) -> Result<Self, StatisticsError> {
        if limits.max_metrics == 0 || limits.max_series == 0 || limits.max_series_per_metric == 0 {
            return Err(StatisticsError::InvalidLimits);
        }
        Ok(Self {
            limits,
            descriptors: BTreeMap::new(),
            series: BTreeMap::new(),
            series_per_metric: BTreeMap::new(),
        })
    }

    #[must_use]
    pub const fn limits(&self) -> RegistryLimits {
        self.limits
    }

    /// Registers a metric. Re-registering an identical descriptor is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StatisticsError::DescriptorConflict`] for a different
    /// descriptor under an existing ID and [`StatisticsError::MetricLimit`]
    /// when the registry is full.
    pub fn register(
        &mut self,
        id: MetricId,
        descriptor: MetricDescriptor,
    ) -> Result<(), StatisticsError> {
        if let Some(existing) = self.descriptors.get(&id) {
            return if *existing == descriptor {
                Ok(())
            } else {
                Err(StatisticsError::DescriptorConflict)
            };
        }
        if self.descriptors.len() >= self.limits.max_metrics {
            return Err(StatisticsError::MetricLimit);
        }
        self.descriptors.insert(id, descriptor);
        Ok(())
    }

    #[must_use]
    pub fn descriptor(&self, id: &MetricId) -> Option<&MetricDescriptor> {
        self.descriptors.get(id)
    }

    /// Adds `amount` to a counter series, creating it at zero if needed.
    ///
    /// # Errors
    ///
    /// Rejects unknown metrics, kind mismatches, out-of-order time, series
    /// limits, and `u64` overflow.
    pub fn increment_counter(
        &mut self,
        id: &MetricId,
        labels: &LabelSet,
        monotonic_millis: u64,
        amount: u64,
    ) -> Result<(), StatisticsError> {
        let entry = self.entry_mut(id, labels, SeriesKind::Counter, monotonic_millis)?;
        let SeriesValue::Counter(total) = &mut entry.value else {
            unreachable!("kind checked by entry_mut");
        };
        *total = total
            .checked_add(amount)
            .ok_or(StatisticsError::CounterOverflow)?;
        touch(entry, monotonic_millis);
        Ok(())
    }

    /// Sets a gauge series to a finite value.
    ///
    /// # Errors
    ///
    /// Rejects non-finite values, unknown metrics, kind mismatches,
    /// out-of-order time, and series limits.
    pub fn set_gauge(
        &mut self,
        id: &MetricId,
        labels: &LabelSet,
        monotonic_millis: u64,
        value: f64,
    ) -> Result<(), StatisticsError> {
        if !value.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        let entry = self.entry_mut(id, labels, SeriesKind::Gauge, monotonic_millis)?;
        entry.value = SeriesValue::Gauge(value);
        touch(entry, monotonic_millis);
        Ok(())
    }

    /// Records one histogram observation.
    ///
    /// # Errors
    ///
    /// Rejects non-finite values or sums, unknown metrics, kind mismatches,
    /// out-of-order time, and series limits.
    pub fn observe_histogram(
        &mut self,
        id: &MetricId,
        labels: &LabelSet,
        monotonic_millis: u64,
        value: f64,
    ) -> Result<(), StatisticsError> {
        if !value.is_finite() {
            return Err(StatisticsError::NonFinite);
        }
        let entry = self.entry_mut(id, labels, SeriesKind::Histogram, monotonic_millis)?;
        let SeriesValue::Histogram(histogram) = &mut entry.value else {
            unreachable!("kind checked by entry_mut");
        };
        histogram.observe(value)?;
        touch(entry, monotonic_millis);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, key: &SeriesKey) -> Option<&SeriesEntry> {
        self.series.get(key)
    }

    /// Removes a series, freeing its slot. Returns whether it existed.
    pub fn remove_series(&mut self, key: &SeriesKey) -> bool {
        if self.series.remove(key).is_none() {
            return false;
        }
        if let Some(count) = self.series_per_metric.get_mut(&key.id) {
            *count -= 1;
            if *count == 0 {
                self.series_per_metric.remove(&key.id);
            }
        }
        true
    }

    /// Iterates series sorted by metric ID, then label set.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&SeriesKey, &SeriesEntry)> {
        self.series.iter()
    }

    #[must_use]
    pub fn series_len(&self) -> usize {
        self.series.len()
    }

    #[must_use]
    pub fn metric_len(&self) -> usize {
        self.descriptors.len()
    }

    fn entry_mut(
        &mut self,
        id: &MetricId,
        labels: &LabelSet,
        expected: SeriesKind,
        monotonic_millis: u64,
    ) -> Result<&mut SeriesEntry, StatisticsError> {
        let descriptor = self
            .descriptors
            .get(id)
            .ok_or(StatisticsError::UnknownMetric)?;
        let actual = descriptor.kind();
        if actual != expected {
            return Err(StatisticsError::KindMismatch { expected, actual });
        }
        let key = SeriesKey::new(id.clone(), labels.clone());
        if !self.series.contains_key(&key) {
            if self.series.len() >= self.limits.max_series {
                return Err(StatisticsError::SeriesLimit);
            }
            let per_metric = self.series_per_metric.get(id).copied().unwrap_or(0);
            if per_metric >= self.limits.max_series_per_metric {
                return Err(StatisticsError::SeriesLimit);
            }
            let value = match descriptor {
                MetricDescriptor::Counter => SeriesValue::Counter(0),
                MetricDescriptor::Gauge => SeriesValue::Gauge(0.0),
                MetricDescriptor::Histogram(buckets) => {
                    SeriesValue::Histogram(BucketHistogram::new(buckets.clone()))
                }
            };
            // A freshly created series starts with zero updates; the caller's
            // first successful update stamps its time. Validation of the value
            // happens before creation, so an inserted series is always updated.
            self.series.insert(
                key.clone(),
                SeriesEntry {
                    value,
                    updated_millis: monotonic_millis,
                    updates: 0,
                },
            );
            *self.series_per_metric.entry(id.clone()).or_insert(0) += 1;
        }
        let entry = self.series.get_mut(&key).expect("series inserted above");
        if entry.updates > 0 && monotonic_millis < entry.updated_millis {
            return Err(StatisticsError::OutOfOrder);
        }
        Ok(entry)
    }
}

fn touch(entry: &mut SeriesEntry, monotonic_millis: u64) {
    entry.updated_millis = monotonic_millis;
    entry.updates = entry.updates.saturating_add(1);
}

/// Errors from the metric registry, histograms, and windowed statistics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatisticsError {
    InvalidMetricId,
    InvalidLabelKey,
    InvalidLabelValue,
    DuplicateLabel,
    TooManyLabels,
    InvalidBuckets,
    BucketMismatch,
    InvalidLimits,
    InvalidWindow,
    UnknownMetric,
    DescriptorConflict,
    KindMismatch {
        expected: SeriesKind,
        actual: SeriesKind,
    },
    MetricLimit,
    SeriesLimit,
    NonFinite,
    CounterOverflow,
    OutOfOrder,
}

impl fmt::Display for StatisticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMetricId => formatter.write_str("metric id must match [a-z][a-z0-9_.]*"),
            Self::InvalidLabelKey => formatter.write_str("label key must match [a-z][a-z0-9_]*"),
            Self::InvalidLabelValue => {
                formatter.write_str("label value must be bounded printable ascii")
            }
            Self::DuplicateLabel => formatter.write_str("label key is already present"),
            Self::TooManyLabels => formatter.write_str("label set exceeds its bound"),
            Self::InvalidBuckets => {
                formatter.write_str("histogram edges must be finite and strictly increasing")
            }
            Self::BucketMismatch => formatter.write_str("histogram bucket layouts differ"),
            Self::InvalidLimits => formatter.write_str("statistics limits must be non-zero"),
            Self::InvalidWindow => {
                formatter.write_str("window duration and capacity must be non-zero")
            }
            Self::UnknownMetric => formatter.write_str("metric is not registered"),
            Self::DescriptorConflict => {
                formatter.write_str("metric is already registered with a different shape")
            }
            Self::KindMismatch { expected, actual } => write!(
                formatter,
                "metric is a {}, not a {}",
                actual.as_str(),
                expected.as_str()
            ),
            Self::MetricLimit => formatter.write_str("metric registry is full"),
            Self::SeriesLimit => formatter.write_str("series limit reached"),
            Self::NonFinite => formatter.write_str("statistic value must be finite"),
            Self::CounterOverflow => formatter.write_str("counter would overflow"),
            Self::OutOfOrder => formatter.write_str("time is older than the latest update"),
        }
    }
}

impl Error for StatisticsError {}
