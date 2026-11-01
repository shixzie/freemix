//! Health alert rules and a show's settings for them (`RC-015`).
//!
//! Both daemons evaluate one alert rule per [`HealthAlertRule`] over their
//! once-a-second statistics. Each rule compares one reading against a
//! trigger threshold, held for a time before it fires, and clears past a
//! clear threshold on the far side of a hysteresis band. A show may change
//! any rule's thresholds and hold, its severity, or switch it off, with an
//! [`AlertRuleSetting`] (schema 99); a rule without one uses its
//! [`AlertRuleDefaults`]. The reading a rule watches is fixed.
//!
//! Thresholds are the reading times 1000 (a "milli" value), as alert
//! readings are on the wire: a share of `0.9` is `900`, one dropped picture
//! a second is `1000`, and 1 GiB free is `1_073_741_824_000`.

use std::fmt;

/// One alert rule, in wire (rule ID) order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HealthAlertRule {
    /// The daemon process kept most of the machine's CPUs busy; the reading
    /// is the share of every logical CPU.
    CpuSaturated,
    /// An output filesystem's free space ran low; the reading is free bytes.
    DiskLow,
    /// Realized frames kept waking half a frame late; the reading is the
    /// late share of frames.
    FrameLate,
    /// Outputs kept dropping pictures; the reading is drops per second.
    FramesDropped,
    /// The GPU utilization proxy stayed near full; the reading is its share.
    GpuSaturated,
    /// A stream target or recorder slot latched a failure; the reading is
    /// the number of failed outputs.
    OutputFailed,
    /// An encoder's queue stayed saturated; the reading is the deepest queue
    /// depth.
    EncoderSaturated,
    /// Stream outputs kept reconnecting; the reading is the number of
    /// reconnecting outputs.
    StreamReconnecting,
    /// A live source's clock sync stayed degraded; the reading is the worst
    /// drift.
    SyncDegraded,
}

impl HealthAlertRule {
    /// Every rule in wire order.
    pub const ALL: [Self; 9] = [
        Self::CpuSaturated,
        Self::DiskLow,
        Self::FrameLate,
        Self::FramesDropped,
        Self::GpuSaturated,
        Self::OutputFailed,
        Self::EncoderSaturated,
        Self::StreamReconnecting,
        Self::SyncDegraded,
    ];

    /// The wire token, which is also the daemon's rule ID.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::CpuSaturated => "cpu_saturated",
            Self::DiskLow => "disk_low",
            Self::FrameLate => "frame_late",
            Self::FramesDropped => "frames_dropped",
            Self::GpuSaturated => "gpu_saturated",
            Self::OutputFailed => "output_failed",
            Self::EncoderSaturated => "encoder_saturated",
            Self::StreamReconnecting => "stream_reconnecting",
            Self::SyncDegraded => "sync_degraded",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.token() == token)
    }

    /// What the rule's reading measures.
    #[must_use]
    pub const fn reading(self) -> AlertReading {
        match self {
            Self::CpuSaturated | Self::FrameLate | Self::GpuSaturated | Self::EncoderSaturated => {
                AlertReading::Share
            }
            Self::DiskLow => AlertReading::Bytes,
            Self::FramesDropped => AlertReading::PerSecond,
            Self::OutputFailed | Self::StreamReconnecting | Self::SyncDegraded => {
                AlertReading::Count
            }
        }
    }

    /// Whether the rule fires on a reading at or above its trigger (else at
    /// or below it).
    #[must_use]
    pub const fn fires_above(self) -> bool {
        !matches!(self, Self::DiskLow)
    }

    /// The rule's severity, thresholds, and hold when the show sets none.
    #[must_use]
    pub const fn defaults(self) -> AlertRuleDefaults {
        let (severity, trigger_milli, clear_milli, hold_ms) = match self {
            Self::CpuSaturated => (AlertSeverity::Warning, 900, 750, 10_000),
            Self::DiskLow => (AlertSeverity::Critical, GIB_MILLI, 2 * GIB_MILLI, 0),
            Self::FrameLate => (AlertSeverity::Warning, 250, 50, 5_000),
            Self::FramesDropped => (AlertSeverity::Warning, 1_000, 0, 3_000),
            Self::GpuSaturated => (AlertSeverity::Warning, 900, 700, 5_000),
            Self::OutputFailed => (AlertSeverity::Critical, 1_000, 0, 0),
            Self::EncoderSaturated => (AlertSeverity::Warning, 900, 600, 5_000),
            Self::StreamReconnecting | Self::SyncDegraded => {
                (AlertSeverity::Warning, 1_000, 0, 10_000)
            }
        };
        AlertRuleDefaults {
            severity,
            trigger_milli,
            clear_milli,
            hold_ms,
        }
    }

    /// The smallest and largest threshold the rule takes, in milli units.
    #[must_use]
    pub const fn threshold_range(self) -> (i64, i64) {
        match self.reading() {
            AlertReading::Share => (0, 1_000),
            // 1 MiB to 64 TiB free.
            AlertReading::Bytes => (MIB_MILLI, 64 * 1024 * 1024 * MIB_MILLI),
            AlertReading::PerSecond | AlertReading::Count => (0, 1_000_000),
        }
    }
}

const MIB_MILLI: i64 = 1024 * 1024 * 1000;
const GIB_MILLI: i64 = 1024 * MIB_MILLI;

/// How urgent an alert rule's notifications are.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

impl AlertSeverity {
    /// Every severity, least urgent first, which is also the wire order.
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Critical];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// The wire token, for logs and support text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.token()
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|severity| severity.token() == token)
    }
}

impl fmt::Display for AlertSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

/// Longest hold a rule takes before it fires: one hour.
pub const MAX_ALERT_HOLD_MS: u32 = 3_600_000;

/// What an alert rule's reading measures, which decides how a threshold is
/// shown.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AlertReading {
    /// A share from 0 to 1, shown as a percentage.
    Share,
    /// Free bytes, shown in GiB.
    Bytes,
    /// Events per second.
    PerSecond,
    /// A number of outputs or sources.
    Count,
}

impl AlertReading {
    /// A threshold (the reading times 1000) in the unit consoles show it:
    /// a percentage, free GiB, events a second, or a count.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // thresholds are far below 2^53
    pub fn shown(self, milli: i64) -> f64 {
        let value = milli as f64 / 1000.0;
        match self {
            Self::Share => value * 100.0,
            Self::Bytes => value / GIB,
            Self::PerSecond | Self::Count => value,
        }
    }

    /// [`Self::shown`]'s inverse, rounded to a whole milli unit and
    /// saturating at the `i64` range.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // saturating float-to-int conversion
    pub fn milli(self, shown: f64) -> i64 {
        let value = match self {
            Self::Share => shown / 100.0,
            Self::Bytes => shown * GIB,
            Self::PerSecond | Self::Count => shown,
        };
        (value * 1000.0).round() as i64
    }

    /// The unit's short name: `%`, `GiB`, `/s`, or empty for a count.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Share => "%",
            Self::Bytes => "GiB",
            Self::PerSecond => "/s",
            Self::Count => "",
        }
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// A rule's severity, thresholds, and hold when the show sets none.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AlertRuleDefaults {
    pub severity: AlertSeverity,
    pub trigger_milli: i64,
    pub clear_milli: i64,
    pub hold_ms: u32,
}

/// A show's setting for one alert rule.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AlertRuleSetting {
    pub rule: HealthAlertRule,
    /// `false` switches the rule off: it never fires.
    pub enabled: bool,
    /// How urgent the rule's notifications are.
    pub severity: AlertSeverity,
    /// The reading (times 1000) at which the rule starts to fire.
    pub trigger_milli: i64,
    /// The reading (times 1000) past which a firing rule clears, on the far
    /// side of the trigger.
    pub clear_milli: i64,
    /// How long the trigger must hold before the rule fires.
    pub hold_ms: u32,
}

impl AlertRuleSetting {
    /// The setting a rule has when the show sets none.
    #[must_use]
    pub const fn default_for(rule: HealthAlertRule) -> Self {
        let defaults = rule.defaults();
        Self {
            rule,
            enabled: true,
            severity: defaults.severity,
            trigger_milli: defaults.trigger_milli,
            clear_milli: defaults.clear_milli,
            hold_ms: defaults.hold_ms,
        }
    }

    /// Whether the setting is the rule's default (so a show need not keep
    /// it).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default_for(self.rule)
    }

    /// Checks the setting's bounds and hysteresis band.
    ///
    /// # Errors
    ///
    /// The first [`AlertRuleError`] found.
    pub const fn check(&self) -> Result<(), AlertRuleError> {
        let (low, high) = self.rule.threshold_range();
        if self.trigger_milli < low
            || self.trigger_milli > high
            || self.clear_milli < low
            || self.clear_milli > high
        {
            return Err(AlertRuleError::OutOfRange);
        }
        let banded = if self.rule.fires_above() {
            self.clear_milli < self.trigger_milli
        } else {
            self.clear_milli > self.trigger_milli
        };
        if !banded {
            return Err(AlertRuleError::Hysteresis);
        }
        if self.hold_ms > MAX_ALERT_HOLD_MS {
            return Err(AlertRuleError::Hold);
        }
        Ok(())
    }
}

/// The setting each rule has in a show: its own, else the default.
#[must_use]
pub fn effective_setting(settings: &[AlertRuleSetting], rule: HealthAlertRule) -> AlertRuleSetting {
    settings
        .iter()
        .find(|setting| setting.rule == rule)
        .copied()
        .unwrap_or_else(|| AlertRuleSetting::default_for(rule))
}

/// Checks a show's alert rule settings: each valid, at most one per rule,
/// in rule order, none equal to its rule's default.
///
/// # Errors
///
/// The first [`AlertRuleError`] found.
pub fn check_settings(settings: &[AlertRuleSetting]) -> Result<(), AlertRuleError> {
    for (index, setting) in settings.iter().enumerate() {
        setting.check()?;
        if setting.is_default() {
            return Err(AlertRuleError::Default);
        }
        if index > 0 && settings[index - 1].rule >= setting.rule {
            return Err(AlertRuleError::Order);
        }
    }
    Ok(())
}

/// Puts `setting` into `settings`, kept in rule order, dropping it when it
/// is the rule's default.
pub fn put_setting(settings: &mut Vec<AlertRuleSetting>, setting: AlertRuleSetting) {
    settings.retain(|kept| kept.rule != setting.rule);
    if !setting.is_default() {
        settings.push(setting);
        settings.sort_by_key(|kept| kept.rule);
    }
}

/// Why an alert rule setting is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlertRuleError {
    /// A threshold lies outside its rule's
    /// [`HealthAlertRule::threshold_range`].
    OutOfRange,
    /// The clear threshold is not past the trigger, away from the firing
    /// side.
    Hysteresis,
    /// The hold is longer than [`MAX_ALERT_HOLD_MS`].
    Hold,
    /// A show keeps a setting equal to its rule's default.
    Default,
    /// The settings repeat a rule or are not in rule order.
    Order,
}

impl fmt::Display for AlertRuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutOfRange => "an alert rule threshold is outside the rule's range",
            Self::Hysteresis => {
                "an alert rule's clear threshold must lie past its trigger, away from the firing side"
            }
            Self::Hold => "an alert rule's hold must be at most one hour",
            Self::Default => "a show keeps no alert rule setting equal to the rule's default",
            Self::Order => "alert rule settings must name each rule at most once, in rule order",
        })
    }
}

impl std::error::Error for AlertRuleError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_tokens_round_trip() {
        for rule in HealthAlertRule::ALL {
            let setting = AlertRuleSetting::default_for(rule);
            assert_eq!(setting.check(), Ok(()), "{rule:?}");
            assert!(setting.is_default());
            assert_eq!(HealthAlertRule::from_token(rule.token()), Some(rule));
        }
        assert_eq!(HealthAlertRule::from_token("disk_full"), None);
    }

    #[test]
    fn settings_need_a_band_bounds_and_order() {
        let cpu = AlertRuleSetting::default_for(HealthAlertRule::CpuSaturated);
        let stricter = AlertRuleSetting {
            trigger_milli: 800,
            clear_milli: 600,
            ..cpu
        };
        assert_eq!(stricter.check(), Ok(()));
        // Changing only the severity is a valid, non-default setting.
        let louder = AlertRuleSetting {
            severity: AlertSeverity::Critical,
            ..cpu
        };
        assert_eq!(louder.check(), Ok(()));
        assert!(!louder.is_default());
        for severity in AlertSeverity::ALL {
            assert_eq!(AlertSeverity::from_token(severity.token()), Some(severity));
        }
        assert_eq!(AlertSeverity::from_token("fatal"), None);
        // Above-firing rules clear below the trigger, disk_low above it.
        assert_eq!(
            AlertRuleSetting {
                clear_milli: 800,
                ..stricter
            }
            .check(),
            Err(AlertRuleError::Hysteresis)
        );
        let disk = AlertRuleSetting::default_for(HealthAlertRule::DiskLow);
        assert_eq!(
            AlertRuleSetting {
                clear_milli: disk.trigger_milli - 1,
                ..disk
            }
            .check(),
            Err(AlertRuleError::Hysteresis)
        );
        assert_eq!(
            AlertRuleSetting {
                trigger_milli: 1_001,
                ..stricter
            }
            .check(),
            Err(AlertRuleError::OutOfRange)
        );
        assert_eq!(
            AlertRuleSetting {
                trigger_milli: 0,
                clear_milli: 2 * GIB_MILLI,
                ..disk
            }
            .check(),
            Err(AlertRuleError::OutOfRange)
        );
        assert_eq!(
            AlertRuleSetting {
                hold_ms: MAX_ALERT_HOLD_MS + 1,
                ..stricter
            }
            .check(),
            Err(AlertRuleError::Hold)
        );

        // A show keeps each changed rule once, in order, never a default.
        let off = AlertRuleSetting {
            enabled: false,
            ..AlertRuleSetting::default_for(HealthAlertRule::OutputFailed)
        };
        let mut settings = Vec::new();
        put_setting(&mut settings, off);
        put_setting(&mut settings, stricter);
        assert_eq!(settings, [stricter, off]);
        assert_eq!(check_settings(&settings), Ok(()));
        assert_eq!(
            effective_setting(&settings, HealthAlertRule::GpuSaturated),
            AlertRuleSetting::default_for(HealthAlertRule::GpuSaturated)
        );
        assert_eq!(
            effective_setting(&settings, HealthAlertRule::OutputFailed),
            off
        );
        put_setting(&mut settings, cpu);
        assert_eq!(settings, [off]);
        assert_eq!(check_settings(&[off, off]), Err(AlertRuleError::Order));
        assert_eq!(check_settings(&[cpu]), Err(AlertRuleError::Default));
    }
}
