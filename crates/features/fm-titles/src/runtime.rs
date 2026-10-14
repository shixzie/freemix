use fm_clock::production::{ClockValue, DurationFormat, Rounding, format_duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockDirection {
    CountUp,
    CountDown,
}

impl ClockDirection {
    /// The production-clock rounding for this direction: a countdown rounds
    /// up so `00:00` appears exactly at zero, a count-up rounds down.
    #[must_use]
    pub const fn rounding(self) -> Rounding {
        match self {
            Self::CountUp => Rounding::Floor,
            Self::CountDown => Rounding::Ceil,
        }
    }
}

/// The scene-clock display presets. Each is a name for the matching
/// [`DurationFormat`], whose formatter draws every title clock, so scene
/// clocks, scoreboard clocks, and production clocks cannot diverge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockFormat {
    MinutesSeconds,
    HoursMinutesSeconds,
}

impl ClockFormat {
    #[must_use]
    pub const fn duration_format(self) -> DurationFormat {
        match self {
            Self::MinutesSeconds => DurationFormat::MinutesSeconds,
            Self::HoursMinutesSeconds => DurationFormat::HoursMinutesSeconds,
        }
    }
}

/// Formats a non-negative millisecond clock value with the production-clock
/// formatter. Values beyond [`ClockValue::MAX`] display as that maximum.
pub(crate) fn format_clock_ms(
    value_ms: u64,
    format: ClockFormat,
    direction: ClockDirection,
) -> String {
    let value = i64::try_from(value_ms)
        .ok()
        .and_then(|millis| ClockValue::from_millis(millis).ok())
        .unwrap_or(ClockValue::MAX);
    format_duration(value, format.duration_format(), direction.rounding())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockSpec {
    pub direction: ClockDirection,
    pub start_value_ms: u64,
    pub starts_at_ms: u64,
    pub format: ClockFormat,
}

impl ClockSpec {
    #[must_use]
    pub fn value_ms(self, scene_time_ms: u64) -> u64 {
        let elapsed = scene_time_ms.saturating_sub(self.starts_at_ms);
        match self.direction {
            ClockDirection::CountUp => self.start_value_ms.saturating_add(elapsed),
            ClockDirection::CountDown => self.start_value_ms.saturating_sub(elapsed),
        }
    }
}

/// Evaluates a clock at scene time in whole seconds, through the
/// production-clock formatter: a count-up rounds down and a countdown rounds
/// up, so a countdown shows `00:00` only once it has reached zero.
#[must_use]
pub fn evaluate_clock(spec: ClockSpec, scene_time_ms: u64) -> String {
    format_clock_ms(spec.value_ms(scene_time_ms), spec.format, spec.direction)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickerDirection {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickerSpec {
    pub field: crate::FieldId,
    pub direction: TickerDirection,
    pub pixels_per_second: u32,
    pub gap_px: u32,
    pub starts_at_ms: u64,
}

/// Returns the ticker's x position relative to its viewport.
///
/// A left-moving item starts at the right edge and wraps after it has cleared
/// the left edge plus its gap. Right-moving behavior is the mirror image.
#[must_use]
pub fn evaluate_ticker_position(
    spec: TickerSpec,
    scene_time_ms: u64,
    viewport_width: u32,
    content_width: u32,
) -> i64 {
    let cycle = u64::from(viewport_width)
        .saturating_add(u64::from(content_width))
        .saturating_add(u64::from(spec.gap_px))
        .max(1);
    let elapsed = scene_time_ms.saturating_sub(spec.starts_at_ms);
    let distance = elapsed.saturating_mul(u64::from(spec.pixels_per_second)) / 1_000 % cycle;
    match spec.direction {
        TickerDirection::Left => {
            i64::from(viewport_width) - i64::try_from(distance).unwrap_or(i64::MAX)
        }
        TickerDirection::Right => {
            -i64::from(content_width) + i64::try_from(distance).unwrap_or(i64::MAX)
        }
    }
}
