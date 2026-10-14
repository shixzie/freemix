//! Deterministic alert rule engine over a [`MetricRegistry`].
//!
//! Rules are evaluated only when the caller invokes [`AlertEngine::evaluate`]
//! with a caller-supplied monotonic time; nothing reads a system clock.
//!
//! Each rule maps its reading to one of four conditions: *trigger*, *clear*,
//! *hold* (inside the hysteresis band), or *unknown* (no reading yet). States
//! move as follows:
//!
//! * `Inactive`/`Resolved` + trigger: `Firing` when `for_millis == 0`,
//!   otherwise `Pending` since now.
//! * `Pending` + trigger: `Firing` once `now - since >= for_millis`.
//! * `Pending` + hold or clear: back to `Inactive` (the trigger condition must
//!   hold continuously).
//! * `Firing` + clear: `Resolved`; hold or trigger keeps it `Firing`.
//! * unknown: no change.
//!
//! Notifications are deduplicated per firing episode: one `Firing`
//! notification is delivered per episode (late, after a silence expires, if it
//! was suppressed), one `Resolved` notification only if the `Firing` one was
//! delivered, and one `Reminder` when an acknowledgement expires while the
//! rule is still firing. Undrained notifications for the same rule and kind
//! coalesce in the bounded queue.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt,
};

use crate::{
    AlertPolicy, ThresholdDirection,
    registry::{MetricRegistry, SeriesKey, SeriesKind, SeriesValue, valid_identifier},
};

/// Stable alert rule identifier, using the metric ID grammar.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuleId(String);

impl RuleId {
    /// Validates a rule identifier.
    ///
    /// # Errors
    ///
    /// Returns [`AlertRuleError::InvalidRuleId`] unless the value matches
    /// `[a-z][a-z0-9_.]*` (no trailing or doubled `.`) within 64 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, AlertRuleError> {
        let value = value.into();
        if valid_identifier(&value) {
            Ok(Self(value))
        } else {
            Err(AlertRuleError::InvalidRuleId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Alert importance, ordered from least to most severe.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

impl AlertSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Scalar a rule reads from its series.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleReading {
    /// Counter total or gauge value.
    Value,
    /// Histogram observation count.
    Count,
    /// Histogram percentile in `1..=100`, interpolated from buckets.
    Percentile(u8),
}

impl RuleReading {
    const fn accepts(self, kind: SeriesKind) -> bool {
        match self {
            Self::Value => matches!(kind, SeriesKind::Counter | SeriesKind::Gauge),
            Self::Count | Self::Percentile(_) => matches!(kind, SeriesKind::Histogram),
        }
    }
}

/// What a rule tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RuleCondition {
    /// Reading crosses `policy.trigger`, clears past `policy.clear`.
    Threshold(AlertPolicy),
    /// Per-second rate between consecutive distinct series updates crosses
    /// `policy.trigger`, clears past `policy.clear`. The latest rate persists
    /// until the series is updated again; pair with [`Self::Absence`] to
    /// detect stalls.
    RateOfChange(AlertPolicy),
    /// Series missing, or `now - updated_millis > max_age_millis`.
    Absence { max_age_millis: u64 },
}

/// One alert rule.
#[derive(Clone, Debug, PartialEq)]
pub struct AlertRule {
    pub id: RuleId,
    pub severity: AlertSeverity,
    pub series: SeriesKey,
    pub reading: RuleReading,
    pub condition: RuleCondition,
    /// Time the trigger condition must hold before firing.
    pub for_millis: u64,
}

impl AlertRule {
    #[must_use]
    pub const fn new(
        id: RuleId,
        severity: AlertSeverity,
        series: SeriesKey,
        reading: RuleReading,
        condition: RuleCondition,
    ) -> Self {
        Self {
            id,
            severity,
            series,
            reading,
            condition,
            for_millis: 0,
        }
    }

    #[must_use]
    pub const fn for_duration(mut self, for_millis: u64) -> Self {
        self.for_millis = for_millis;
        self
    }
}

/// Lifecycle state of a rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleState {
    Inactive,
    Pending { since_millis: u64 },
    Firing { since_millis: u64 },
    Resolved { at_millis: u64 },
}

impl RuleState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Pending { .. } => "pending",
            Self::Firing { .. } => "firing",
            Self::Resolved { .. } => "resolved",
        }
    }

    /// Time the current state began; `None` while inactive.
    #[must_use]
    pub const fn since_millis(self) -> Option<u64> {
        match self {
            Self::Inactive => None,
            Self::Pending { since_millis } | Self::Firing { since_millis } => Some(since_millis),
            Self::Resolved { at_millis } => Some(at_millis),
        }
    }
}

/// Kind of a queued notification.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NotificationKind {
    Firing,
    Resolved,
    /// An acknowledgement expired while the rule was still firing.
    Reminder,
}

impl NotificationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Resolved => "resolved",
            Self::Reminder => "reminder",
        }
    }
}

/// One deduplicated operator notification.
#[derive(Clone, Debug, PartialEq)]
pub struct AlertNotification {
    /// Assigned when first queued; coalescing keeps the original sequence.
    pub sequence: u64,
    pub rule: RuleId,
    pub severity: AlertSeverity,
    pub kind: NotificationKind,
    /// Time of the latest coalesced occurrence.
    pub at_millis: u64,
    /// Reading at the latest occurrence, if any.
    pub value: Option<f64>,
    /// Number of coalesced occurrences, at least one.
    pub occurrences: u32,
}

/// Bounded FIFO of notifications with severity-aware eviction.
///
/// A push matching an undrained entry's rule and kind coalesces into it. When
/// full, the oldest entry of the lowest severity present is evicted if its
/// severity does not exceed the new entry's; otherwise the new entry is
/// dropped. Both outcomes count in [`Self::dropped`].
#[derive(Clone, Debug)]
pub struct NotificationQueue {
    capacity: usize,
    entries: VecDeque<AlertNotification>,
    next_sequence: u64,
    dropped: u64,
    coalesced: u64,
}

impl NotificationQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
            next_sequence: 0,
            dropped: 0,
            coalesced: 0,
        }
    }

    fn push(
        &mut self,
        rule: &RuleId,
        severity: AlertSeverity,
        kind: NotificationKind,
        at_millis: u64,
        value: Option<f64>,
    ) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|entry| entry.rule == *rule && entry.kind == kind)
        {
            existing.occurrences = existing.occurrences.saturating_add(1);
            existing.at_millis = at_millis;
            existing.value = value;
            self.coalesced = self.coalesced.saturating_add(1);
            return;
        }
        if self.entries.len() == self.capacity {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(index, entry)| (entry.severity, *index))
                .map(|(index, entry)| (index, entry.severity));
            self.dropped = self.dropped.saturating_add(1);
            match victim {
                Some((index, victim)) if victim <= severity => {
                    self.entries.remove(index);
                }
                _ => return,
            }
        }
        self.entries.push_back(AlertNotification {
            sequence: self.next_sequence,
            rule: rule.clone(),
            severity,
            kind,
            at_millis,
            value,
            occurrences: 1,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Notifications evicted or refused because the queue was full.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Pushes merged into an undrained entry.
    #[must_use]
    pub const fn coalesced(&self) -> u64 {
        self.coalesced
    }

    /// Iterates undrained notifications oldest first.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &AlertNotification> {
        self.entries.iter()
    }
}

/// Engine bounds; every field must be non-zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlertEngineLimits {
    pub max_rules: usize,
    pub max_silences: usize,
    pub notification_capacity: usize,
}

impl Default for AlertEngineLimits {
    fn default() -> Self {
        Self {
            max_rules: 128,
            max_silences: 32,
            notification_capacity: 64,
        }
    }
}

/// Handle returned by [`AlertEngine::silence`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SilenceId(pub u64);

/// Active silence for one rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Silence {
    pub rule: RuleId,
    /// Exclusive expiry: the silence is inactive at `now >= until_millis`.
    pub until_millis: u64,
}

/// Observable status of one rule.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleStatus {
    pub rule: AlertRule,
    pub state: RuleState,
    /// Latest reading (rate for rate-of-change, age for absence).
    pub last_value: Option<f64>,
    /// Exclusive acknowledgement expiry while firing.
    pub acknowledged_until_millis: Option<u64>,
    /// Number of times this rule has entered `Firing`.
    pub episodes: u64,
}

/// One state change reported by [`AlertEngine::evaluate`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleTransition {
    pub rule: RuleId,
    pub from: RuleState,
    pub to: RuleState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Condition {
    Trigger,
    Clear,
    Hold,
    Unknown,
}

/// Last sample a rate-of-change rule measured from.
#[derive(Clone, Copy, Debug)]
struct RateBaseline {
    time: u64,
    value: f64,
    updates: u64,
    generation: u64,
}

#[derive(Clone, Debug)]
struct RuleRuntime {
    status: RuleStatus,
    rate_baseline: Option<RateBaseline>,
    notified_firing: bool,
}

/// Caller-owned, bounded alert rule engine.
#[derive(Clone, Debug)]
pub struct AlertEngine {
    limits: AlertEngineLimits,
    rules: BTreeMap<RuleId, RuleRuntime>,
    silences: BTreeMap<SilenceId, Silence>,
    next_silence: u64,
    queue: NotificationQueue,
    suppressed: u64,
    last_evaluation: Option<u64>,
}

impl AlertEngine {
    /// Creates an empty engine.
    ///
    /// # Errors
    ///
    /// Returns [`AlertRuleError::InvalidLimits`] when any limit is zero.
    pub fn new(limits: AlertEngineLimits) -> Result<Self, AlertRuleError> {
        if limits.max_rules == 0 || limits.max_silences == 0 || limits.notification_capacity == 0 {
            return Err(AlertRuleError::InvalidLimits);
        }
        Ok(Self {
            limits,
            rules: BTreeMap::new(),
            silences: BTreeMap::new(),
            next_silence: 0,
            queue: NotificationQueue::new(limits.notification_capacity),
            suppressed: 0,
            last_evaluation: None,
        })
    }

    /// Adds a rule after validating it against the registry descriptor.
    ///
    /// # Errors
    ///
    /// Rejects duplicate IDs, a full engine, unregistered metrics, readings
    /// that do not fit the metric kind, out-of-range percentiles, and a zero
    /// absence age.
    pub fn add_rule(
        &mut self,
        rule: AlertRule,
        registry: &MetricRegistry,
    ) -> Result<(), AlertRuleError> {
        if self.rules.contains_key(&rule.id) {
            return Err(AlertRuleError::DuplicateRule);
        }
        if self.rules.len() >= self.limits.max_rules {
            return Err(AlertRuleError::RuleLimit);
        }
        let descriptor = registry
            .descriptor(&rule.series.id)
            .ok_or(AlertRuleError::UnknownMetric)?;
        if !rule.reading.accepts(descriptor.kind()) {
            return Err(AlertRuleError::ReadingMismatch);
        }
        if let RuleReading::Percentile(percentile) = rule.reading
            && !(1..=100).contains(&percentile)
        {
            return Err(AlertRuleError::InvalidPercentile);
        }
        if matches!(rule.condition, RuleCondition::Absence { max_age_millis: 0 }) {
            return Err(AlertRuleError::InvalidAbsence);
        }
        self.rules.insert(
            rule.id.clone(),
            RuleRuntime {
                status: RuleStatus {
                    rule,
                    state: RuleState::Inactive,
                    last_value: None,
                    acknowledged_until_millis: None,
                    episodes: 0,
                },
                rate_baseline: None,
                notified_firing: false,
            },
        );
        Ok(())
    }

    /// Removes a rule and its silences.
    ///
    /// # Errors
    ///
    /// Returns [`AlertRuleError::UnknownRule`] if absent.
    pub fn remove_rule(&mut self, id: &RuleId) -> Result<AlertRule, AlertRuleError> {
        let runtime = self.rules.remove(id).ok_or(AlertRuleError::UnknownRule)?;
        self.silences.retain(|_, silence| silence.rule != *id);
        Ok(runtime.status.rule)
    }

    /// Evaluates every rule in ID order at `now`.
    ///
    /// # Errors
    ///
    /// Returns [`AlertRuleError::OutOfOrder`] if `now` precedes the previous
    /// evaluation; no state changes in that case.
    pub fn evaluate(
        &mut self,
        registry: &MetricRegistry,
        now: u64,
    ) -> Result<Vec<RuleTransition>, AlertRuleError> {
        if self.last_evaluation.is_some_and(|last| now < last) {
            return Err(AlertRuleError::OutOfOrder);
        }
        self.last_evaluation = Some(now);
        self.silences
            .retain(|_, silence| silence.until_millis > now);
        let mut transitions = Vec::new();
        for (id, runtime) in &mut self.rules {
            let silenced = self.silences.values().any(|silence| silence.rule == *id);
            let severity = runtime.status.rule.severity;

            if let Some(until) = runtime.status.acknowledged_until_millis
                && until <= now
            {
                runtime.status.acknowledged_until_millis = None;
                if matches!(runtime.status.state, RuleState::Firing { .. })
                    && runtime.notified_firing
                {
                    if silenced {
                        self.suppressed = self.suppressed.saturating_add(1);
                    } else {
                        let value = runtime.status.last_value;
                        self.queue
                            .push(id, severity, NotificationKind::Reminder, now, value);
                    }
                }
            }

            let condition = runtime.condition(registry, now);
            let from = runtime.status.state;
            let for_millis = runtime.status.rule.for_millis;
            let to = match (from, condition) {
                (RuleState::Inactive | RuleState::Resolved { .. }, Condition::Trigger) => {
                    if for_millis == 0 {
                        RuleState::Firing { since_millis: now }
                    } else {
                        RuleState::Pending { since_millis: now }
                    }
                }
                (RuleState::Pending { since_millis }, Condition::Trigger) => {
                    if now - since_millis >= for_millis {
                        RuleState::Firing { since_millis: now }
                    } else {
                        from
                    }
                }
                (RuleState::Pending { .. }, Condition::Hold | Condition::Clear) => {
                    RuleState::Inactive
                }
                (RuleState::Firing { .. }, Condition::Clear) => {
                    RuleState::Resolved { at_millis: now }
                }
                // Unknown readings, firing on hold/trigger, and idle on
                // hold/clear keep their state.
                _ => from,
            };
            if to != from {
                runtime.status.state = to;
                transitions.push(RuleTransition {
                    rule: id.clone(),
                    from,
                    to,
                });
                match to {
                    RuleState::Firing { .. } => {
                        runtime.status.episodes = runtime.status.episodes.saturating_add(1);
                        runtime.notified_firing = false;
                        if silenced {
                            self.suppressed = self.suppressed.saturating_add(1);
                        }
                    }
                    RuleState::Resolved { .. } => {
                        runtime.status.acknowledged_until_millis = None;
                        if runtime.notified_firing {
                            let value = runtime.status.last_value;
                            self.queue
                                .push(id, severity, NotificationKind::Resolved, now, value);
                        } else {
                            self.suppressed = self.suppressed.saturating_add(1);
                        }
                        runtime.notified_firing = false;
                    }
                    RuleState::Inactive | RuleState::Pending { .. } => {}
                }
            }
            if matches!(runtime.status.state, RuleState::Firing { .. })
                && !runtime.notified_firing
                && !silenced
            {
                runtime.notified_firing = true;
                let value = runtime.status.last_value;
                self.queue
                    .push(id, severity, NotificationKind::Firing, now, value);
            }
        }
        Ok(transitions)
    }

    /// Acknowledges a firing rule until `until_millis` (exclusive).
    ///
    /// The acknowledgement ends when the rule resolves. If it expires while
    /// the rule still fires, the next evaluation queues a `Reminder`.
    ///
    /// # Errors
    ///
    /// Rejects unknown rules, rules not firing, and `until_millis <= now`.
    pub fn acknowledge(
        &mut self,
        id: &RuleId,
        now: u64,
        until_millis: u64,
    ) -> Result<(), AlertRuleError> {
        let runtime = self.rules.get_mut(id).ok_or(AlertRuleError::UnknownRule)?;
        if !matches!(runtime.status.state, RuleState::Firing { .. }) {
            return Err(AlertRuleError::NotFiring);
        }
        if until_millis <= now {
            return Err(AlertRuleError::InvalidExpiry);
        }
        runtime.status.acknowledged_until_millis = Some(until_millis);
        Ok(())
    }

    /// Silences notifications for one rule until `until_millis` (exclusive).
    /// State tracking continues while silenced.
    ///
    /// # Errors
    ///
    /// Rejects unknown rules, `until_millis <= now`, and a full silence table.
    pub fn silence(
        &mut self,
        id: &RuleId,
        now: u64,
        until_millis: u64,
    ) -> Result<SilenceId, AlertRuleError> {
        if !self.rules.contains_key(id) {
            return Err(AlertRuleError::UnknownRule);
        }
        if until_millis <= now {
            return Err(AlertRuleError::InvalidExpiry);
        }
        self.silences
            .retain(|_, silence| silence.until_millis > now);
        if self.silences.len() >= self.limits.max_silences {
            return Err(AlertRuleError::SilenceLimit);
        }
        let silence_id = SilenceId(self.next_silence);
        self.next_silence = self
            .next_silence
            .checked_add(1)
            .ok_or(AlertRuleError::SilenceLimit)?;
        self.silences.insert(
            silence_id,
            Silence {
                rule: id.clone(),
                until_millis,
            },
        );
        Ok(silence_id)
    }

    /// Removes a silence early.
    ///
    /// # Errors
    ///
    /// Returns [`AlertRuleError::UnknownSilence`] if absent or expired.
    pub fn unsilence(&mut self, id: SilenceId) -> Result<Silence, AlertRuleError> {
        self.silences
            .remove(&id)
            .ok_or(AlertRuleError::UnknownSilence)
    }

    #[must_use]
    pub fn status(&self, id: &RuleId) -> Option<&RuleStatus> {
        self.rules.get(id).map(|runtime| &runtime.status)
    }

    /// Iterates rule statuses in ID order.
    pub fn statuses(&self) -> impl ExactSizeIterator<Item = &RuleStatus> {
        self.rules.values().map(|runtime| &runtime.status)
    }

    /// Iterates silences retained since the last expiry sweep, in ID order.
    pub fn silences(&self) -> impl ExactSizeIterator<Item = (SilenceId, &Silence)> {
        self.silences.iter().map(|(id, silence)| (*id, silence))
    }

    #[must_use]
    pub const fn notifications(&self) -> &NotificationQueue {
        &self.queue
    }

    /// Removes and returns all queued notifications, oldest first.
    pub fn drain_notifications(&mut self) -> Vec<AlertNotification> {
        self.queue.entries.drain(..).collect()
    }

    /// Notification edges withheld by silences.
    #[must_use]
    pub const fn suppressed(&self) -> u64 {
        self.suppressed
    }

    #[must_use]
    pub const fn last_evaluation(&self) -> Option<u64> {
        self.last_evaluation
    }
}

impl RuleRuntime {
    fn condition(&mut self, registry: &MetricRegistry, now: u64) -> Condition {
        let rule = &self.status.rule;
        let entry = registry.get(&rule.series);
        match rule.condition {
            RuleCondition::Absence { max_age_millis } => {
                let Some(entry) = entry else {
                    self.status.last_value = None;
                    return Condition::Trigger;
                };
                let age = now.saturating_sub(entry.updated_millis);
                self.status.last_value = Some(crate::registry::as_f64(age));
                if age > max_age_millis {
                    Condition::Trigger
                } else {
                    Condition::Clear
                }
            }
            RuleCondition::Threshold(policy) => {
                let Some(reading) = entry.and_then(|entry| read(&entry.value, rule.reading)) else {
                    return Condition::Unknown;
                };
                self.status.last_value = Some(reading);
                classify(policy, reading)
            }
            RuleCondition::RateOfChange(policy) => {
                let Some((entry, reading)) = entry.and_then(|entry| {
                    read(&entry.value, rule.reading).map(|reading| (entry, reading))
                }) else {
                    // A missing series has no rate; a later recreation must
                    // not be compared against the removed series' baseline.
                    self.rate_baseline = None;
                    self.status.last_value = None;
                    return Condition::Unknown;
                };
                let current = RateBaseline {
                    time: entry.updated_millis,
                    value: reading,
                    updates: entry.updates,
                    generation: entry.generation,
                };
                match self.rate_baseline {
                    // The series was removed and recreated, or a monotonic
                    // reading went backwards (counter reset): restart the
                    // rate from the current sample instead of reporting a
                    // spurious rate across the discontinuity.
                    Some(base)
                        if base.generation != current.generation
                            || (monotonic_reading(&entry.value, rule.reading)
                                && current.value < base.value) =>
                    {
                        self.rate_baseline = Some(current);
                        self.status.last_value = None;
                    }
                    Some(base) if base.updates != current.updates => {
                        if current.time > base.time {
                            let rate = (current.value - base.value) * 1_000.0
                                / crate::registry::as_f64(current.time - base.time);
                            self.status.last_value = Some(rate);
                            self.rate_baseline = Some(current);
                        } else {
                            // Same timestamp: keep the baseline time, adopt
                            // the newer value for the next rate.
                            self.rate_baseline = Some(RateBaseline {
                                time: base.time,
                                ..current
                            });
                        }
                    }
                    Some(_) => {}
                    None => self.rate_baseline = Some(current),
                }
                match self.status.last_value {
                    Some(rate) if self.rate_baseline.is_some() && rate.is_finite() => {
                        classify(policy, rate)
                    }
                    _ => Condition::Unknown,
                }
            }
        }
    }
}

/// Readings that can only grow while a series lives; a decrease means reset.
const fn monotonic_reading(value: &SeriesValue, reading: RuleReading) -> bool {
    matches!(
        (value, reading),
        (SeriesValue::Counter(_), RuleReading::Value)
            | (SeriesValue::Histogram(_), RuleReading::Count)
    )
}

fn read(value: &SeriesValue, reading: RuleReading) -> Option<f64> {
    match (value, reading) {
        (SeriesValue::Counter(total), RuleReading::Value) => Some(crate::registry::as_f64(*total)),
        (SeriesValue::Gauge(value), RuleReading::Value) => Some(*value),
        (SeriesValue::Histogram(histogram), RuleReading::Count) => {
            Some(crate::registry::as_f64(histogram.count()))
        }
        (SeriesValue::Histogram(histogram), RuleReading::Percentile(percentile)) => {
            histogram.percentile(percentile)
        }
        _ => None,
    }
}

fn classify(policy: AlertPolicy, value: f64) -> Condition {
    let (trigger, clear) = match policy.direction() {
        ThresholdDirection::Above => (value >= policy.trigger(), value <= policy.clear()),
        ThresholdDirection::Below => (value <= policy.trigger(), value >= policy.clear()),
    };
    if trigger {
        Condition::Trigger
    } else if clear {
        Condition::Clear
    } else {
        Condition::Hold
    }
}

/// Errors from the alert rule engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlertRuleError {
    InvalidRuleId,
    InvalidLimits,
    DuplicateRule,
    RuleLimit,
    UnknownRule,
    UnknownMetric,
    ReadingMismatch,
    InvalidPercentile,
    InvalidAbsence,
    NotFiring,
    InvalidExpiry,
    SilenceLimit,
    UnknownSilence,
    OutOfOrder,
}

impl fmt::Display for AlertRuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRuleId => "rule id must match [a-z][a-z0-9_.]*",
            Self::InvalidLimits => "alert engine limits must be non-zero",
            Self::DuplicateRule => "rule id is already registered",
            Self::RuleLimit => "alert rule limit reached",
            Self::UnknownRule => "rule is not registered",
            Self::UnknownMetric => "rule targets an unregistered metric",
            Self::ReadingMismatch => "rule reading does not fit the metric kind",
            Self::InvalidPercentile => "rule percentile must be within 1..=100",
            Self::InvalidAbsence => "absence age must be non-zero",
            Self::NotFiring => "only a firing rule can be acknowledged",
            Self::InvalidExpiry => "expiry must be later than now",
            Self::SilenceLimit => "silence limit reached",
            Self::UnknownSilence => "silence is not active",
            Self::OutOfOrder => "evaluation time is older than the previous evaluation",
        })
    }
}

impl Error for AlertRuleError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_queue_evicts_lowest_severity_and_refuses_lower_ones() {
        let mut queue = NotificationQueue::new(2);
        let rule = |name: &str| RuleId::new(name).unwrap();
        queue.push(
            &rule("a"),
            AlertSeverity::Warning,
            NotificationKind::Firing,
            1,
            None,
        );
        queue.push(
            &rule("b"),
            AlertSeverity::Critical,
            NotificationKind::Firing,
            2,
            None,
        );
        queue.push(
            &rule("c"),
            AlertSeverity::Info,
            NotificationKind::Firing,
            3,
            None,
        );
        assert_eq!(queue.dropped(), 1);
        queue.push(
            &rule("d"),
            AlertSeverity::Critical,
            NotificationKind::Firing,
            4,
            None,
        );
        assert_eq!(queue.dropped(), 2);
        let rules: Vec<_> = queue
            .iter()
            .map(|entry| entry.rule.as_str().to_owned())
            .collect();
        assert_eq!(rules, ["b", "d"]);
    }
}
