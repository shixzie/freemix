//! Bounded operational events, metrics, health, alerts, and support bundles.
//!
//! All storage is caller-owned. The crate does not install a global recorder or
//! spawn background work, which keeps it suitable for engine, service, and test
//! composition roots alike.

mod alert;
mod bundle;
mod event;
mod health;
mod metric;
mod redact;
mod registry;
mod rules;
mod snapshot;
mod window;

pub use alert::{
    AlertPolicy, AlertPolicyError, AlertState, AlertTransition, ThresholdAlert, ThresholdDirection,
};
pub use bundle::{BundleExport, SupportBundle};
pub use event::{
    Category, EventField, EventLog, EventRecord, EventValue, SequenceExhausted, Severity,
};
pub use health::{ComponentHealth, HealthAggregate, HealthCheck, HealthRegistry};
pub use metric::{
    CounterSummary, GaugeSummary, HistogramSummary, Metric, MetricError, MetricKind, MetricPoint,
    MetricSeries, MetricStore,
};
pub use redact::Redactor;
pub use registry::{
    BucketHistogram, HistogramBuckets, LabelSet, MAX_BUCKET_EDGES, MAX_ID_BYTES,
    MAX_LABEL_KEY_BYTES, MAX_LABEL_VALUE_BYTES, MAX_LABELS, MetricDescriptor, MetricId,
    MetricRegistry, RegistryLimits, SeriesEntry, SeriesKey, SeriesKind, SeriesValue,
    StatisticsError,
};
pub use rules::{
    AlertEngine, AlertEngineLimits, AlertNotification, AlertRule, AlertRuleError, AlertSeverity,
    NotificationKind, NotificationQueue, RuleCondition, RuleId, RuleReading, RuleState, RuleStatus,
    RuleTransition, Silence, SilenceId,
};
pub use snapshot::{STATISTICS_HEADING, StatisticsSnapshot};
pub use window::{
    DepthWindowStats, FrameWindow, FrameWindowStats, QueueDepthWindow, WindowedHistogram,
};
