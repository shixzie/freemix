//! RC-015 statistics and alerts slice: registry, windows, rules, and export.

use fm_capabilities::CapabilityRegistry;
use fm_observability::{
    AlertEngine, AlertEngineLimits, AlertPolicy, AlertRule, AlertRuleError, AlertSeverity,
    BucketHistogram, EventLog, FrameWindow, HealthRegistry, HistogramBuckets, LabelSet, MAX_LABELS,
    MetricDescriptor, MetricId, MetricRegistry, MetricStore, NotificationKind, QueueDepthWindow,
    RegistryLimits, RuleCondition, RuleId, RuleReading, RuleState, SeriesKey, SeriesKind,
    SeriesValue, StatisticsError, StatisticsSnapshot, SupportBundle, ThresholdDirection,
    WindowedHistogram,
};

fn id(value: &str) -> MetricId {
    MetricId::new(value).unwrap()
}

fn rule_id(value: &str) -> RuleId {
    RuleId::new(value).unwrap()
}

fn labels(pairs: &[(&str, &str)]) -> LabelSet {
    LabelSet::from_pairs(pairs.iter().copied()).unwrap()
}

fn registry() -> MetricRegistry {
    let mut registry = MetricRegistry::new(RegistryLimits::default()).unwrap();
    registry
        .register(id("output.frames_dropped"), MetricDescriptor::Counter)
        .unwrap();
    registry
        .register(id("engine.queue_depth"), MetricDescriptor::Gauge)
        .unwrap();
    registry
        .register(
            id("render.latency_ms"),
            MetricDescriptor::Histogram(HistogramBuckets::new(&[0.0, 10.0, 20.0, 40.0]).unwrap()),
        )
        .unwrap();
    registry
}

fn approx(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("value present");
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn identifiers_labels_and_buckets_are_validated() {
    for bad in ["", "Upper", "1abc", "a..b", "a.", "a-b", &"a".repeat(65)] {
        assert_eq!(MetricId::new(bad), Err(StatisticsError::InvalidMetricId));
    }
    assert_eq!(
        LabelSet::new().with("Bad", "x"),
        Err(StatisticsError::InvalidLabelKey)
    );
    for bad in ["", "a\"b", "a=b", "a{b", "tab\t", &"v".repeat(129)] {
        assert_eq!(
            LabelSet::new().with("key", bad),
            Err(StatisticsError::InvalidLabelValue)
        );
    }
    assert_eq!(
        LabelSet::new().with("a", "1").unwrap().with("a", "2"),
        Err(StatisticsError::DuplicateLabel)
    );
    let mut full = LabelSet::new();
    for index in 0..MAX_LABELS {
        full = full.with(format!("k{index}"), "v").unwrap();
    }
    assert_eq!(full.with("extra", "v"), Err(StatisticsError::TooManyLabels));

    for bad in [
        &[1.0][..],
        &[1.0, 1.0],
        &[2.0, 1.0],
        &[0.0, f64::INFINITY],
        &[0.0, f64::NAN],
    ] {
        assert_eq!(
            HistogramBuckets::new(bad),
            Err(StatisticsError::InvalidBuckets)
        );
    }
    assert_eq!(
        HistogramBuckets::new(&(0..34).map(f64::from).collect::<Vec<_>>()),
        Err(StatisticsError::InvalidBuckets)
    );
}

#[test]
fn registry_enforces_kinds_bounds_order_and_overflow() {
    let mut registry = MetricRegistry::new(RegistryLimits {
        max_metrics: 2,
        max_series: 3,
        max_series_per_metric: 2,
    })
    .unwrap();
    let counter = id("a.counter");
    let gauge = id("b.gauge");
    registry
        .register(counter.clone(), MetricDescriptor::Counter)
        .unwrap();
    registry
        .register(counter.clone(), MetricDescriptor::Counter)
        .unwrap();
    assert_eq!(
        registry.register(counter.clone(), MetricDescriptor::Gauge),
        Err(StatisticsError::DescriptorConflict)
    );
    registry
        .register(gauge.clone(), MetricDescriptor::Gauge)
        .unwrap();
    assert_eq!(
        registry.register(id("c.extra"), MetricDescriptor::Gauge),
        Err(StatisticsError::MetricLimit)
    );
    assert_eq!(
        MetricRegistry::new(RegistryLimits {
            max_metrics: 0,
            ..RegistryLimits::default()
        })
        .err(),
        Some(StatisticsError::InvalidLimits)
    );

    let none = LabelSet::new();
    assert_eq!(
        registry.set_gauge(&counter, &none, 1, 1.0),
        Err(StatisticsError::KindMismatch {
            expected: SeriesKind::Gauge,
            actual: SeriesKind::Counter,
        })
    );
    assert_eq!(
        registry.increment_counter(&id("missing"), &none, 1, 1),
        Err(StatisticsError::UnknownMetric)
    );
    assert_eq!(
        registry.set_gauge(&gauge, &none, 1, f64::NAN),
        Err(StatisticsError::NonFinite)
    );
    assert_eq!(registry.series_len(), 0, "rejected inputs create no series");

    registry.increment_counter(&counter, &none, 5, 2).unwrap();
    registry.increment_counter(&counter, &none, 5, 3).unwrap();
    assert_eq!(
        registry.increment_counter(&counter, &none, 4, 1),
        Err(StatisticsError::OutOfOrder)
    );
    assert_eq!(
        registry.increment_counter(&counter, &none, 6, u64::MAX),
        Err(StatisticsError::CounterOverflow)
    );
    let entry = registry
        .get(&SeriesKey::new(counter.clone(), none.clone()))
        .unwrap();
    assert_eq!(entry.value, SeriesValue::Counter(5));
    assert_eq!((entry.updated_millis, entry.updates), (5, 2));

    let host = labels(&[("host", "a")]);
    registry.increment_counter(&counter, &host, 1, 1).unwrap();
    assert_eq!(
        registry.increment_counter(&counter, &labels(&[("host", "b")]), 1, 1),
        Err(StatisticsError::SeriesLimit),
        "per-metric series bound"
    );
    registry.set_gauge(&gauge, &none, 1, 1.0).unwrap();
    assert_eq!(
        registry.set_gauge(&gauge, &host, 1, 1.0),
        Err(StatisticsError::SeriesLimit),
        "global series bound"
    );
    assert!(registry.remove_series(&SeriesKey::new(counter, host.clone())));
    registry.set_gauge(&gauge, &host, 1, 1.0).unwrap();
}

#[test]
fn histogram_percentiles_use_documented_interpolation() {
    let buckets = HistogramBuckets::new(&[0.0, 10.0, 20.0]).unwrap();
    let mut histogram = BucketHistogram::new(buckets.clone());
    assert_eq!(histogram.percentile(50), None);
    for value in [1.0, 2.0, 3.0, 4.0, 10.0, 11.0, 12.0, 13.0, 14.0, 20.0] {
        histogram.observe(value).unwrap();
    }
    assert_eq!(histogram.counts(), [5, 5]);
    // rank = 10 * p / 100; bucket 0 = [0, 10] holds 5, bucket 1 = (10, 20] holds 5.
    approx(histogram.percentile(50), 10.0);
    approx(histogram.percentile(95), 10.0 + 10.0 * 4.5 / 5.0);
    approx(histogram.percentile(99), 10.0 + 10.0 * 4.9 / 5.0);
    approx(histogram.percentile(10), 2.0);
    assert_eq!(histogram.percentile(0), None);
    assert_eq!(histogram.percentile(101), None);
    approx(histogram.mean(), 9.0);

    let mut clamped = BucketHistogram::new(buckets.clone());
    clamped.observe(-5.0).unwrap();
    clamped.observe(100.0).unwrap();
    assert_eq!((clamped.underflow(), clamped.overflow()), (1, 1));
    approx(clamped.percentile(50), 0.0);
    approx(clamped.percentile(100), 20.0);
    assert_eq!(
        clamped.observe(f64::INFINITY),
        Err(StatisticsError::NonFinite)
    );
    assert_eq!(clamped.observe(f64::MAX), Ok(()));
    assert_eq!(clamped.observe(f64::MAX), Err(StatisticsError::NonFinite));
    assert_eq!(
        clamped.count(),
        3,
        "failed observation leaves state unchanged"
    );

    let other = BucketHistogram::new(HistogramBuckets::new(&[0.0, 1.0]).unwrap());
    assert_eq!(
        histogram.merge(&other),
        Err(StatisticsError::BucketMismatch)
    );
}

#[test]
fn frame_window_reports_rolling_fps_drop_rate_and_saturation() {
    let mut window = FrameWindow::new(1_000, 128).unwrap();
    for frame in 0..=30_u64 {
        window.record_frame(frame * 40).unwrap();
    }
    window.record_drops(1_200, 2).unwrap();
    // Window (200, 1200]: frames at 240..=1200 => 25 frames over 960 ms.
    let stats = window.stats(1_200).unwrap();
    assert_eq!(stats.frames, 25);
    assert_eq!(stats.dropped, 2);
    approx(stats.fps, 24.0 * 1_000.0 / 960.0);
    approx(stats.drop_rate, 2.0 / 27.0);
    assert!(!stats.saturated);

    assert_eq!(window.record_frame(10), Err(StatisticsError::OutOfOrder));
    assert_eq!(window.stats(1_000), Err(StatisticsError::OutOfOrder));
    let idle = window.stats(5_000).unwrap();
    assert_eq!((idle.frames, idle.fps, idle.drop_rate), (0, None, None));

    let mut small = FrameWindow::new(1_000, 4).unwrap();
    for time in [100, 200, 300, 400, 500] {
        small.record_frame(time).unwrap();
    }
    let saturated = small.stats(500).unwrap();
    assert_eq!(saturated.frames, 4);
    assert!(saturated.saturated);
    assert!(!small.stats(1_100).unwrap().saturated);
    assert_eq!(
        FrameWindow::new(0, 1).err(),
        Some(StatisticsError::InvalidWindow)
    );
}

#[test]
fn depth_window_and_windowed_histogram_slide_with_caller_time() {
    let mut depth = QueueDepthWindow::new(1_000, 16).unwrap();
    for (time, value) in [(0, 3), (400, 9), (800, 2), (1_300, 1)] {
        depth.record(time, value).unwrap();
    }
    let stats = depth.stats(1_300).unwrap();
    assert_eq!(
        (stats.samples, stats.current, stats.maximum),
        (3, Some(1), Some(9))
    );
    // At 1400 the sample at 400 has aged out of (400, 1400].
    assert_eq!(depth.stats(1_400).unwrap().maximum, Some(2));

    let buckets = HistogramBuckets::new(&[0.0, 10.0, 100.0]).unwrap();
    let mut windowed = WindowedHistogram::new(buckets, 100, 3).unwrap();
    windowed.observe(50, 90.0).unwrap();
    windowed.observe(150, 5.0).unwrap();
    windowed.observe(250, 5.0).unwrap();
    assert_eq!(windowed.snapshot(250).unwrap().count(), 3);
    // At 300 the window holds slices 1..=3, excluding the slow slice-0 sample.
    let later = windowed.snapshot(300).unwrap();
    assert_eq!(later.count(), 2);
    approx(windowed.percentile(300, 99).unwrap(), 9.9);
    assert_eq!(windowed.observe(10, 1.0), Err(StatisticsError::OutOfOrder));
}

fn engine() -> AlertEngine {
    AlertEngine::new(AlertEngineLimits::default()).unwrap()
}

#[test]
fn threshold_rule_moves_through_pending_firing_resolved_with_hysteresis() {
    let mut registry = registry();
    let gauge = id("engine.queue_depth");
    let none = LabelSet::new();
    let mut alerts = engine();
    let rule = rule_id("queue.deep");
    alerts
        .add_rule(
            AlertRule::new(
                rule.clone(),
                AlertSeverity::Warning,
                SeriesKey::new(gauge.clone(), none.clone()),
                RuleReading::Value,
                RuleCondition::Threshold(
                    AlertPolicy::new(ThresholdDirection::Above, 10.0, 5.0).unwrap(),
                ),
            )
            .for_duration(200),
            &registry,
        )
        .unwrap();

    // No series yet: reading unknown, state unchanged.
    assert_eq!(alerts.evaluate(&registry, 0).unwrap(), []);

    registry.set_gauge(&gauge, &none, 100, 12.0).unwrap();
    alerts.evaluate(&registry, 100).unwrap();
    assert_eq!(
        alerts.status(&rule).unwrap().state,
        RuleState::Pending { since_millis: 100 }
    );
    // Dropping into the hysteresis band cancels pending.
    registry.set_gauge(&gauge, &none, 150, 7.0).unwrap();
    alerts.evaluate(&registry, 150).unwrap();
    assert_eq!(alerts.status(&rule).unwrap().state, RuleState::Inactive);

    registry.set_gauge(&gauge, &none, 200, 11.0).unwrap();
    alerts.evaluate(&registry, 200).unwrap();
    alerts.evaluate(&registry, 399).unwrap();
    assert!(matches!(
        alerts.status(&rule).unwrap().state,
        RuleState::Pending { .. }
    ));
    let transitions = alerts.evaluate(&registry, 400).unwrap();
    assert_eq!(transitions.len(), 1);
    assert_eq!(transitions[0].to, RuleState::Firing { since_millis: 400 });

    // Inside the band the firing alert holds; repeated evaluation dedups.
    registry.set_gauge(&gauge, &none, 500, 7.0).unwrap();
    alerts.evaluate(&registry, 500).unwrap();
    alerts.evaluate(&registry, 600).unwrap();
    assert!(matches!(
        alerts.status(&rule).unwrap().state,
        RuleState::Firing { .. }
    ));
    registry.set_gauge(&gauge, &none, 700, 5.0).unwrap();
    alerts.evaluate(&registry, 700).unwrap();
    assert_eq!(
        alerts.status(&rule).unwrap().state,
        RuleState::Resolved { at_millis: 700 }
    );

    let notifications = alerts.drain_notifications();
    let kinds: Vec<_> = notifications.iter().map(|entry| entry.kind).collect();
    assert_eq!(
        kinds,
        [NotificationKind::Firing, NotificationKind::Resolved]
    );
    assert_eq!(notifications[0].value, Some(11.0));
    assert_eq!(notifications[0].severity, AlertSeverity::Warning);
    assert_eq!(alerts.status(&rule).unwrap().episodes, 1);
    assert_eq!(
        alerts.evaluate(&registry, 600),
        Err(AlertRuleError::OutOfOrder)
    );
}

#[test]
fn rate_of_change_and_absence_rules_follow_series_updates() {
    let mut registry = registry();
    let drops = id("output.frames_dropped");
    let none = LabelSet::new();
    let mut alerts = engine();
    let rate = rule_id("drops.rate");
    let stale = rule_id("drops.stale");
    let key = SeriesKey::new(drops.clone(), none.clone());
    alerts
        .add_rule(
            AlertRule::new(
                rate.clone(),
                AlertSeverity::Critical,
                key.clone(),
                RuleReading::Value,
                RuleCondition::RateOfChange(
                    AlertPolicy::new(ThresholdDirection::Above, 10.0, 2.0).unwrap(),
                ),
            ),
            &registry,
        )
        .unwrap();
    alerts
        .add_rule(
            AlertRule::new(
                stale.clone(),
                AlertSeverity::Info,
                key,
                RuleReading::Value,
                RuleCondition::Absence {
                    max_age_millis: 1_000,
                },
            ),
            &registry,
        )
        .unwrap();

    // Missing series is absent immediately.
    alerts.evaluate(&registry, 0).unwrap();
    assert!(matches!(
        alerts.status(&stale).unwrap().state,
        RuleState::Firing { .. }
    ));

    registry.increment_counter(&drops, &none, 1_000, 0).unwrap();
    alerts.evaluate(&registry, 1_000).unwrap();
    assert_eq!(alerts.status(&rate).unwrap().state, RuleState::Inactive);
    assert!(matches!(
        alerts.status(&stale).unwrap().state,
        RuleState::Resolved { .. }
    ));

    // 30 drops over 2 s = 15/s, above the 10/s trigger.
    registry
        .increment_counter(&drops, &none, 3_000, 30)
        .unwrap();
    alerts.evaluate(&registry, 3_000).unwrap();
    assert!(matches!(
        alerts.status(&rate).unwrap().state,
        RuleState::Firing { .. }
    ));
    approx(alerts.status(&rate).unwrap().last_value, 15.0);
    // 10 drops over 2 s = 5/s: inside the band, still firing.
    registry
        .increment_counter(&drops, &none, 5_000, 10)
        .unwrap();
    alerts.evaluate(&registry, 5_000).unwrap();
    assert!(matches!(
        alerts.status(&rate).unwrap().state,
        RuleState::Firing { .. }
    ));
    // 2 drops over 2 s = 1/s: clears.
    registry.increment_counter(&drops, &none, 7_000, 2).unwrap();
    alerts.evaluate(&registry, 7_000).unwrap();
    assert!(matches!(
        alerts.status(&rate).unwrap().state,
        RuleState::Resolved { .. }
    ));

    // No updates for more than 1 s: absence fires again.
    alerts.evaluate(&registry, 8_001).unwrap();
    assert!(matches!(
        alerts.status(&stale).unwrap().state,
        RuleState::Firing { .. }
    ));
    assert_eq!(alerts.status(&stale).unwrap().episodes, 2);
}

#[test]
fn rule_validation_and_limits_are_explicit() {
    let registry = registry();
    let mut alerts = AlertEngine::new(AlertEngineLimits {
        max_rules: 1,
        max_silences: 1,
        notification_capacity: 1,
    })
    .unwrap();
    let policy = AlertPolicy::new(ThresholdDirection::Above, 2.0, 1.0).unwrap();
    let make = |name: &str, metric: &str, reading| {
        AlertRule::new(
            rule_id(name),
            AlertSeverity::Info,
            SeriesKey::new(id(metric), LabelSet::new()),
            reading,
            RuleCondition::Threshold(policy),
        )
    };
    assert_eq!(
        alerts.add_rule(make("a", "missing.metric", RuleReading::Value), &registry),
        Err(AlertRuleError::UnknownMetric)
    );
    assert_eq!(
        alerts.add_rule(
            make("a", "render.latency_ms", RuleReading::Value),
            &registry
        ),
        Err(AlertRuleError::ReadingMismatch)
    );
    assert_eq!(
        alerts.add_rule(
            make("a", "render.latency_ms", RuleReading::Percentile(0)),
            &registry
        ),
        Err(AlertRuleError::InvalidPercentile)
    );
    let mut absence = make("a", "engine.queue_depth", RuleReading::Value);
    absence.condition = RuleCondition::Absence { max_age_millis: 0 };
    assert_eq!(
        alerts.add_rule(absence, &registry),
        Err(AlertRuleError::InvalidAbsence)
    );
    alerts
        .add_rule(
            make("a", "render.latency_ms", RuleReading::Percentile(95)),
            &registry,
        )
        .unwrap();
    assert_eq!(
        alerts.add_rule(
            make("a", "engine.queue_depth", RuleReading::Value),
            &registry
        ),
        Err(AlertRuleError::DuplicateRule)
    );
    assert_eq!(
        alerts.add_rule(
            make("b", "engine.queue_depth", RuleReading::Value),
            &registry
        ),
        Err(AlertRuleError::RuleLimit)
    );
    assert_eq!(
        alerts.acknowledge(&rule_id("a"), 0, 10),
        Err(AlertRuleError::NotFiring)
    );
    assert_eq!(
        alerts.silence(&rule_id("a"), 10, 10),
        Err(AlertRuleError::InvalidExpiry)
    );
    alerts.silence(&rule_id("a"), 0, 10).unwrap();
    assert_eq!(
        alerts.silence(&rule_id("a"), 0, 10),
        Err(AlertRuleError::SilenceLimit)
    );
    assert_eq!(RuleId::new("Bad"), Err(AlertRuleError::InvalidRuleId));
    assert_eq!(
        AlertEngine::new(AlertEngineLimits {
            notification_capacity: 0,
            ..AlertEngineLimits::default()
        })
        .err(),
        Some(AlertRuleError::InvalidLimits)
    );
}

#[test]
fn silences_and_acknowledgements_expire_deterministically() {
    let mut registry = registry();
    let gauge = id("engine.queue_depth");
    let none = LabelSet::new();
    let mut alerts = engine();
    let rule = rule_id("queue.deep");
    alerts
        .add_rule(
            AlertRule::new(
                rule.clone(),
                AlertSeverity::Critical,
                SeriesKey::new(gauge.clone(), none.clone()),
                RuleReading::Value,
                RuleCondition::Threshold(
                    AlertPolicy::new(ThresholdDirection::Above, 10.0, 5.0).unwrap(),
                ),
            ),
            &registry,
        )
        .unwrap();

    let silence = alerts.silence(&rule, 0, 100).unwrap();
    registry.set_gauge(&gauge, &none, 10, 20.0).unwrap();
    alerts.evaluate(&registry, 10).unwrap();
    assert!(matches!(
        alerts.status(&rule).unwrap().state,
        RuleState::Firing { .. }
    ));
    assert!(
        alerts.notifications().is_empty(),
        "silenced firing is withheld"
    );
    assert_eq!(alerts.suppressed(), 1);

    // Silence expiry (exclusive) delivers the withheld firing notification once.
    alerts.evaluate(&registry, 100).unwrap();
    alerts.evaluate(&registry, 150).unwrap();
    let delivered = alerts.drain_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].kind, NotificationKind::Firing);
    assert_eq!(delivered[0].at_millis, 100);
    assert_eq!(
        alerts.unsilence(silence),
        Err(AlertRuleError::UnknownSilence)
    );

    // Acknowledgement expiry while still firing queues one reminder.
    alerts.acknowledge(&rule, 150, 300).unwrap();
    assert_eq!(
        alerts.status(&rule).unwrap().acknowledged_until_millis,
        Some(300)
    );
    alerts.evaluate(&registry, 299).unwrap();
    assert!(alerts.notifications().is_empty());
    alerts.evaluate(&registry, 300).unwrap();
    alerts.evaluate(&registry, 301).unwrap();
    let reminder = alerts.drain_notifications();
    assert_eq!(reminder.len(), 1);
    assert_eq!(reminder[0].kind, NotificationKind::Reminder);
    assert_eq!(
        alerts.status(&rule).unwrap().acknowledged_until_millis,
        None
    );

    // Resolution clears an acknowledgement.
    alerts.acknowledge(&rule, 400, 10_000).unwrap();
    registry.set_gauge(&gauge, &none, 500, 1.0).unwrap();
    alerts.evaluate(&registry, 500).unwrap();
    assert_eq!(
        alerts.status(&rule).unwrap().acknowledged_until_millis,
        None
    );
    assert_eq!(
        alerts.drain_notifications()[0].kind,
        NotificationKind::Resolved
    );
}

#[test]
fn notification_queue_coalesces_flaps_and_bounds_by_severity() {
    let mut registry = registry();
    let gauge = id("engine.queue_depth");
    let none = LabelSet::new();
    let mut alerts = AlertEngine::new(AlertEngineLimits {
        notification_capacity: 2,
        ..AlertEngineLimits::default()
    })
    .unwrap();
    let rule = rule_id("queue.deep");
    alerts
        .add_rule(
            AlertRule::new(
                rule,
                AlertSeverity::Warning,
                SeriesKey::new(gauge.clone(), none.clone()),
                RuleReading::Value,
                RuleCondition::Threshold(
                    AlertPolicy::new(ThresholdDirection::Above, 10.0, 5.0).unwrap(),
                ),
            ),
            &registry,
        )
        .unwrap();
    for (time, value) in [(1, 20.0), (2, 1.0), (3, 20.0), (4, 1.0), (5, 20.0)] {
        registry.set_gauge(&gauge, &none, time, value).unwrap();
        alerts.evaluate(&registry, time).unwrap();
    }
    let queue = alerts.notifications();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.coalesced(), 3);
    assert_eq!(queue.dropped(), 0);
    let entries: Vec<_> = queue
        .iter()
        .map(|entry| (entry.kind, entry.occurrences, entry.at_millis))
        .collect();
    assert_eq!(
        entries,
        [
            (NotificationKind::Firing, 3, 5),
            (NotificationKind::Resolved, 2, 4)
        ]
    );
}

#[test]
fn snapshot_is_sorted_stable_redacted_and_bundle_includable() {
    let mut registry = registry();
    let none = LabelSet::new();
    registry
        .set_gauge(
            &id("engine.queue_depth"),
            &labels(&[("queue", "video"), ("peer", "10.0.0.8")]),
            3,
            4.0,
        )
        .unwrap();
    registry
        .set_gauge(
            &id("engine.queue_depth"),
            &labels(&[("queue", "audio")]),
            3,
            2.5,
        )
        .unwrap();
    registry
        .increment_counter(
            &id("output.frames_dropped"),
            &labels(&[("token", "hunter2")]),
            4,
            7,
        )
        .unwrap();
    for value in [5.0, 15.0, 15.0, 30.0] {
        registry
            .observe_histogram(&id("render.latency_ms"), &none, 5, value)
            .unwrap();
    }
    let mut alerts = engine();
    alerts
        .add_rule(
            AlertRule::new(
                rule_id("latency.p95"),
                AlertSeverity::Critical,
                SeriesKey::new(id("render.latency_ms"), none),
                RuleReading::Percentile(95),
                RuleCondition::Threshold(
                    AlertPolicy::new(ThresholdDirection::Above, 25.0, 20.0).unwrap(),
                ),
            ),
            &registry,
        )
        .unwrap();
    alerts.evaluate(&registry, 6).unwrap();

    let snapshot = StatisticsSnapshot::new(&registry).with_alerts(&alerts);
    let text = snapshot.render();
    assert_eq!(text, snapshot.render());
    let expected = "\
# fm-statistics-v1
metric engine.queue_depth{peer=\"[REDACTED_IP]\",queue=\"video\"} gauge value=4 updated_ms=3 updates=1
metric engine.queue_depth{queue=\"audio\"} gauge value=2.5 updated_ms=3 updates=1
metric output.frames_dropped{token=\"[REDACTED_SECRET]\"} counter value=7 updated_ms=4 updates=1
metric render.latency_ms{} histogram count=4 sum=65 min=5 max=30 p50=15 p95=36 p99=39.2 underflow=0 overflow=0 buckets=10:1,20:2,40:1 updated_ms=5 updates=4
alert latency.p95 severity=critical state=firing since_ms=6 value=36 acknowledged_until_ms=- episodes=1
notifications pending=1 capacity=64 dropped=0 coalesced=0 suppressed=0
notification 0 rule=latency.p95 kind=firing severity=critical at_ms=6 occurrences=1
";
    assert_eq!(text, expected);
    for leaked in ["10.0.0.8", "hunter2"] {
        assert!(!text.contains(leaked));
    }

    let events = EventLog::new(4);
    let metrics = MetricStore::new(2);
    let health = HealthRegistry::new();
    let capabilities = CapabilityRegistry::new();
    let bundle = SupportBundle::new(&events, &metrics, &health, &capabilities);
    let export = snapshot.include_in_bundle(bundle, Some("streams=0"), usize::MAX);
    assert!(!export.truncated);
    let operator = export.text.find("streams=0").unwrap();
    let statistics = export
        .text
        .find("## Statistics\n# fm-statistics-v1")
        .unwrap();
    assert!(operator < statistics);
    assert!(export.text.ends_with(expected));

    let capped = snapshot.include_in_bundle(bundle, None, 256);
    assert!(capped.truncated);
    assert!(capped.text.len() <= 256);
}
