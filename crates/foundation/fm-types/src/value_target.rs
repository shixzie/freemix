//! Dynamic value targets (RC-004): an action's value fixed, relative to the
//! show when it runs, or taken from the control that fired it.
//!
//! A shortcut or cue action that sets a mixer strip's gain names a
//! [`CueGain`]: a fixed gain, a step added to the strip's gain when the
//! action runs (clamped to the strip's range), or the value of the firing
//! control mapped onto a range. A mute action names a [`CueMute`]: mute,
//! unmute, or toggle the strip's mute as it stands when the action runs.
//!
//! The value of the firing control ([`FireValue`]) is a position from 0 to
//! [`FireValue::MAX`] (per mille of its travel): an OSC float argument from
//! 0.0 to 1.0, a fader, or a typed number. Only a shortcut fire carries one;
//! an action reading it in a cue list is refused at definition, and a fire
//! without a value of a shortcut reading one is refused whole. The value is
//! sent with the fire and journaled with it, so a replay sets the same gain.
//! A step or toggle reads the show when the action runs, which a replay
//! reproduces.

use core::fmt;

/// Lowest strip gain an action sets, in millidecibels.
pub const CUE_GAIN_MIN_MILLIDB: i32 = -96_000;
/// Highest strip gain an action sets, in millidecibels.
pub const CUE_GAIN_MAX_MILLIDB: i32 = 24_000;

/// The value of the control that fired a shortcut: a position from 0 to
/// [`Self::MAX`], per mille of the control's travel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FireValue(u16);

impl FireValue {
    /// The end of a control's travel (1.0 on an OSC float).
    pub const MAX: u16 = 1_000;

    /// The value, `None` past [`Self::MAX`].
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        if value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The value of a control position from 0.0 to 1.0, rounded to the
    /// nearest per mille; `None` for a position outside that range or not
    /// a number.
    #[must_use]
    pub fn from_unit(position: f32) -> Option<Self> {
        if !(0.0..=1.0).contains(&position) {
            return None;
        }
        // In range, so the rounded per mille fits.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self::new((position * f32::from(Self::MAX)).round() as u16)
    }

    /// The position, per mille.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for FireValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// How a gain action sets its strip's gain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CueGain {
    /// Sets the gain, in millidecibels.
    Set(i32),
    /// Adds the nonzero step (millidecibels, at most the whole range) to
    /// the strip's gain when the action runs, clamped to
    /// [`CUE_GAIN_MIN_MILLIDB`]`..=`[`CUE_GAIN_MAX_MILLIDB`].
    Step(i32),
    /// Sets the gain from the firing control's value: 0 is `min`,
    /// [`FireValue::MAX`] is `max`, linear in decibels between them and
    /// rounded to the nearest millidecibel. `min` and `max` are distinct
    /// gains in range; `min` above `max` turns the control around.
    Fired { min: i32, max: i32 },
}

impl CueGain {
    /// Whether the gains and the step are in range.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        const fn gain(value: i32) -> bool {
            value >= CUE_GAIN_MIN_MILLIDB && value <= CUE_GAIN_MAX_MILLIDB
        }
        match self {
            Self::Set(value) => gain(value),
            Self::Step(step) => {
                step != 0
                    && step.unsigned_abs() <= CUE_GAIN_MAX_MILLIDB.abs_diff(CUE_GAIN_MIN_MILLIDB)
            }
            Self::Fired { min, max } => gain(min) && gain(max) && min != max,
        }
    }

    /// Whether the gain is the firing control's value.
    #[must_use]
    pub const fn is_fired(self) -> bool {
        matches!(self, Self::Fired { .. })
    }

    /// The gain the action sets on a strip at `current`, given the fire's
    /// `value`; `None` for [`Self::Fired`] without a value.
    #[must_use]
    pub fn apply(self, current: i32, value: Option<FireValue>) -> Option<i32> {
        match self {
            Self::Set(value) => Some(value),
            Self::Step(step) => Some(
                current
                    .saturating_add(step)
                    .clamp(CUE_GAIN_MIN_MILLIDB, CUE_GAIN_MAX_MILLIDB),
            ),
            Self::Fired { min, max } => value.map(|value| fired_gain(min, max, value)),
        }
    }

    /// The gain fixed by the fire's `value`: [`Self::Fired`] becomes the
    /// [`Self::Set`] it maps to; anything else is unchanged. `None` for
    /// [`Self::Fired`] without a value.
    #[must_use]
    pub fn with_value(self, value: Option<FireValue>) -> Option<Self> {
        match self {
            Self::Fired { min, max } => value.map(|value| Self::Set(fired_gain(min, max, value))),
            other => Some(other),
        }
    }
}

fn fired_gain(min: i32, max: i32, value: FireValue) -> i32 {
    let span = i64::from(max) - i64::from(min);
    let offset = (span * i64::from(value.get()) * 2 + i64::from(FireValue::MAX) * span.signum())
        / (i64::from(FireValue::MAX) * 2);
    // Between min and max, both i32.
    i32::try_from(i64::from(min) + offset).unwrap_or(min)
}

impl fmt::Display for CueGain {
    /// `-6.000 dB`, `+3.000 dB step`, `fired -60.000..0.000 dB`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let db = |value: i32| {
            let sign = if value < 0 { "-" } else { "" };
            let magnitude = value.unsigned_abs();
            format!("{sign}{}.{:03}", magnitude / 1000, magnitude % 1000)
        };
        match *self {
            Self::Set(value) => write!(formatter, "{} dB", db(value)),
            Self::Step(step) => {
                let sign = if step > 0 { "+" } else { "" };
                write!(formatter, "{sign}{} dB step", db(step))
            }
            Self::Fired { min, max } => write!(formatter, "fired {}..{} dB", db(min), db(max)),
        }
    }
}

/// How a mute action sets its strip's mute.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CueMute {
    /// Mutes (`true`) or unmutes the strip.
    Set(bool),
    /// Mutes an unmuted strip and unmutes a muted one, as it stands when
    /// the action runs.
    Toggle,
}

impl CueMute {
    /// Whether the strip is muted after the action, from `current`.
    #[must_use]
    pub const fn apply(self, current: bool) -> bool {
        match self {
            Self::Set(muted) => muted,
            Self::Toggle => !current,
        }
    }
}

impl From<bool> for CueMute {
    fn from(muted: bool) -> Self {
        Self::Set(muted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(per_mille: u16) -> FireValue {
        FireValue::new(per_mille).expect("in range")
    }

    #[test]
    fn fire_values_are_per_mille() {
        assert_eq!(FireValue::new(1_000).map(FireValue::get), Some(1_000));
        assert_eq!(FireValue::new(1_001), None);
        assert_eq!(FireValue::from_unit(0.5), Some(value(500)));
        assert_eq!(FireValue::from_unit(1.0), Some(value(1_000)));
        assert_eq!(FireValue::from_unit(0.0004), Some(value(0)));
        assert_eq!(FireValue::from_unit(-0.1), None);
        assert_eq!(FireValue::from_unit(1.1), None);
        assert_eq!(FireValue::from_unit(f32::NAN), None);
    }

    #[test]
    fn gains_set_step_and_follow_the_control() {
        assert_eq!(CueGain::Set(-6_000).apply(0, None), Some(-6_000));
        assert_eq!(CueGain::Step(3_000).apply(-6_000, None), Some(-3_000));
        assert_eq!(CueGain::Step(3_000).apply(23_000, None), Some(24_000));
        assert_eq!(CueGain::Step(-3_000).apply(-95_000, None), Some(-96_000));
        let fader = CueGain::Fired {
            min: -60_000,
            max: 0,
        };
        assert_eq!(fader.apply(5, None), None);
        assert_eq!(fader.apply(5, Some(value(0))), Some(-60_000));
        assert_eq!(fader.apply(5, Some(value(1_000))), Some(0));
        assert_eq!(fader.apply(5, Some(value(500))), Some(-30_000));
        assert_eq!(
            fader.with_value(Some(value(250))),
            Some(CueGain::Set(-45_000))
        );
        let odd = CueGain::Fired { min: 0, max: 1 };
        assert_eq!(odd.apply(0, Some(value(500))), Some(1));
        assert_eq!(odd.apply(0, Some(value(499))), Some(0));
        let reversed = CueGain::Fired { min: 1, max: 0 };
        assert_eq!(reversed.apply(0, Some(value(500))), Some(0));
        assert_eq!(reversed.apply(0, Some(value(501))), Some(0));
        assert_eq!(reversed.apply(0, Some(value(0))), Some(1));
        assert_eq!(CueGain::Set(1).with_value(None), Some(CueGain::Set(1)));
    }

    #[test]
    fn gain_bounds_are_checked() {
        assert!(CueGain::Set(CUE_GAIN_MIN_MILLIDB).is_valid());
        assert!(!CueGain::Set(CUE_GAIN_MAX_MILLIDB + 1).is_valid());
        assert!(!CueGain::Step(0).is_valid());
        assert!(CueGain::Step(-120_000).is_valid());
        assert!(!CueGain::Step(120_001).is_valid());
        assert!(!CueGain::Fired { min: 0, max: 0 }.is_valid());
        assert!(
            !CueGain::Fired {
                min: -97_000,
                max: 0
            }
            .is_valid()
        );
        assert_eq!(CueGain::Step(3_500).to_string(), "+3.500 dB step");
        assert_eq!(CueGain::Set(-250).to_string(), "-0.250 dB");
    }

    #[test]
    fn mutes_set_or_toggle() {
        assert!(CueMute::Set(true).apply(false));
        assert!(!CueMute::Set(false).apply(true));
        assert!(CueMute::Toggle.apply(false));
        assert!(!CueMute::Toggle.apply(true));
        assert_eq!(CueMute::from(true), CueMute::Set(true));
    }
}
