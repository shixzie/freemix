//! Stable, sorted text export of registry series and alert state.
//!
//! Format (`fm-statistics-v1`), one record per line, each section sorted:
//!
//! ```text
//! # fm-statistics-v1
//! metric <id>{<key>="<value>",...} counter value=<u64> updated_ms=<u64> updates=<u64>
//! metric <id>{...} gauge value=<f64> updated_ms=<u64> updates=<u64>
//! metric <id>{...} histogram count=<u64> sum=<f64> min=<f64|-> max=<f64|-> p50=<f64|-> p95=<f64|-> p99=<f64|-> underflow=<u64> overflow=<u64> buckets=<upper>:<count>,... updated_ms=<u64> updates=<u64>
//! alert <rule> severity=<severity> state=<state> since_ms=<u64|-> value=<f64|-> acknowledged_until_ms=<u64|-> episodes=<u64>
//! silence <id> rule=<rule> until_ms=<u64>
//! notifications pending=<n> capacity=<n> dropped=<n> coalesced=<n> suppressed=<n>
//! notification <sequence> rule=<rule> kind=<kind> severity=<severity> at_ms=<u64> occurrences=<n>
//! ```
//!
//! Metrics sort by ID then label set, alerts by rule ID, silences by ID, and
//! notifications by queue order. Numbers use Rust's shortest round-trip `f64`
//! formatting. Label values pass through the [`Redactor`]; identifiers and
//! label keys are restricted grammars that cannot carry secrets or paths.

use std::fmt::Write;

use crate::{
    BundleExport, Redactor, SupportBundle,
    registry::{LabelSet, MetricRegistry, SeriesValue},
    rules::AlertEngine,
};

/// Heading used when the snapshot is appended to a support bundle.
pub const STATISTICS_HEADING: &str = "## Statistics";

/// Borrowed point-in-time view of a registry and optional alert engine.
#[derive(Clone, Copy, Debug)]
pub struct StatisticsSnapshot<'a> {
    registry: &'a MetricRegistry,
    alerts: Option<&'a AlertEngine>,
    redactor: Redactor,
}

impl<'a> StatisticsSnapshot<'a> {
    #[must_use]
    pub const fn new(registry: &'a MetricRegistry) -> Self {
        Self {
            registry,
            alerts: None,
            redactor: Redactor,
        }
    }

    #[must_use]
    pub const fn with_alerts(mut self, alerts: &'a AlertEngine) -> Self {
        self.alerts = Some(alerts);
        self
    }

    #[must_use]
    pub const fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    /// Renders the deterministic `fm-statistics-v1` text.
    #[must_use]
    pub fn render(self) -> String {
        let mut output = String::from("# fm-statistics-v1\n");
        for (key, entry) in self.registry.iter() {
            let _ = write!(output, "metric {}", key.id);
            push_labels(&mut output, &key.labels, self.redactor);
            match &entry.value {
                SeriesValue::Counter(total) => {
                    let _ = write!(output, " counter value={total}");
                }
                SeriesValue::Gauge(value) => {
                    let _ = write!(output, " gauge value={value}");
                }
                SeriesValue::Histogram(histogram) => {
                    let _ = write!(
                        output,
                        " histogram count={} sum={}",
                        histogram.count(),
                        histogram.sum()
                    );
                    for (name, value) in [
                        ("min", histogram.minimum()),
                        ("max", histogram.maximum()),
                        ("p50", histogram.percentile(50)),
                        ("p95", histogram.percentile(95)),
                        ("p99", histogram.percentile(99)),
                    ] {
                        let _ = write!(output, " {name}=");
                        push_optional(&mut output, value);
                    }
                    let _ = write!(
                        output,
                        " underflow={} overflow={} buckets=",
                        histogram.underflow(),
                        histogram.overflow()
                    );
                    let edges = histogram.buckets().edges();
                    for (index, count) in histogram.counts().iter().enumerate() {
                        if index != 0 {
                            output.push(',');
                        }
                        let _ = write!(output, "{}:{count}", edges[index + 1]);
                    }
                }
            }
            let _ = writeln!(
                output,
                " updated_ms={} updates={}",
                entry.updated_millis, entry.updates
            );
        }
        if let Some(alerts) = self.alerts {
            render_alerts(&mut output, alerts);
        }
        output
    }

    /// Exports `bundle` with this snapshot appended under
    /// [`STATISTICS_HEADING`], after any caller-sanitized `operator_state`.
    ///
    /// The combined text travels through
    /// [`SupportBundle::with_operator_state`], so the bundle's byte cap and
    /// truncation marker apply to it unchanged.
    #[must_use]
    pub fn include_in_bundle(
        self,
        bundle: SupportBundle<'_>,
        operator_state: Option<&str>,
        maximum_bytes: usize,
    ) -> BundleExport {
        let mut appended = String::new();
        if let Some(operator_state) = operator_state {
            appended.push_str(operator_state);
            if !operator_state.ends_with('\n') {
                appended.push('\n');
            }
            appended.push('\n');
        }
        appended.push_str(STATISTICS_HEADING);
        appended.push('\n');
        appended.push_str(&self.render());
        bundle.with_operator_state(&appended).export(maximum_bytes)
    }
}

fn render_alerts(output: &mut String, alerts: &AlertEngine) {
    for status in alerts.statuses() {
        let _ = write!(
            output,
            "alert {} severity={} state={} since_ms=",
            status.rule.id,
            status.rule.severity.as_str(),
            status.state.as_str()
        );
        push_optional_u64(output, status.state.since_millis());
        output.push_str(" value=");
        push_optional(output, status.last_value);
        output.push_str(" acknowledged_until_ms=");
        push_optional_u64(output, status.acknowledged_until_millis);
        let _ = writeln!(output, " episodes={}", status.episodes);
    }
    for (id, silence) in alerts.silences() {
        let _ = writeln!(
            output,
            "silence {} rule={} until_ms={}",
            id.0, silence.rule, silence.until_millis
        );
    }
    let queue = alerts.notifications();
    let _ = writeln!(
        output,
        "notifications pending={} capacity={} dropped={} coalesced={} suppressed={}",
        queue.len(),
        queue.capacity(),
        queue.dropped(),
        queue.coalesced(),
        alerts.suppressed()
    );
    for notification in queue.iter() {
        let _ = writeln!(
            output,
            "notification {} rule={} kind={} severity={} at_ms={} occurrences={}",
            notification.sequence,
            notification.rule,
            notification.kind.as_str(),
            notification.severity.as_str(),
            notification.at_millis,
            notification.occurrences
        );
    }
}

fn push_labels(output: &mut String, labels: &LabelSet, redactor: Redactor) {
    output.push('{');
    for (index, (key, value)) in labels.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        let value = redactor.redact_field(key, value);
        let _ = write!(output, "{key}=\"{value}\"");
    }
    output.push('}');
}

fn push_optional(output: &mut String, value: Option<f64>) {
    match value {
        Some(value) => {
            let _ = write!(output, "{value}");
        }
        None => output.push('-'),
    }
}

fn push_optional_u64(output: &mut String, value: Option<u64>) {
    match value {
        Some(value) => {
            let _ = write!(output, "{value}");
        }
        None => output.push('-'),
    }
}
