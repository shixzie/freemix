//! Fade-to-Black configuration shared by the model, switcher, and wire
//! contracts (SW-004).

/// Longest Fade-to-Black duration, in frames, any layer accepts.
pub const MAX_FADE_TO_BLACK_DURATION_FRAMES: u32 = 3_600;

/// How an automatic Fade-to-Black move eases between its endpoints:
/// `Linear` moves an equal step every frame; `Ease` is the exact integer
/// smoothstep `3t² − 2t³`, slow at both ends.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FadeToBlackCurve {
    #[default]
    Linear,
    Ease,
}

impl FadeToBlackCurve {
    /// Both curves, in operator order.
    pub const ALL: [Self; 2] = [Self::Linear, Self::Ease];

    /// The stable token used by persistence, the wire, and the CLI.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Ease => "ease",
        }
    }

    /// The curve named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|curve| curve.token() == token)
    }

    /// The exact eased offset of `distance` after `elapsed` of `duration`
    /// frames, rounded toward the start.
    ///
    /// # Panics
    ///
    /// Panics unless `0 < duration` and `elapsed <= duration`.
    #[must_use]
    pub fn offset(self, distance: u16, elapsed: u32, duration: u32) -> u16 {
        assert!(
            duration > 0 && elapsed <= duration,
            "FTB progress is bounded"
        );
        let offset = match self {
            Self::Linear => u128::from(distance) * u128::from(elapsed) / u128::from(duration),
            Self::Ease => {
                let elapsed = u128::from(elapsed);
                let duration = u128::from(duration);
                let numerator = elapsed * elapsed * (3 * duration - 2 * elapsed);
                u128::from(distance) * numerator / (duration * duration * duration)
            }
        };
        u16::try_from(offset).expect("interpolated FTB offset is within its endpoint")
    }
}

#[cfg(test)]
mod tests {
    use super::FadeToBlackCurve;

    #[test]
    fn curves_meet_both_endpoints_and_ease_is_slow_at_the_ends() {
        for curve in FadeToBlackCurve::ALL {
            assert_eq!(curve.offset(u16::MAX, 0, 30), 0);
            assert_eq!(curve.offset(u16::MAX, 30, 30), u16::MAX);
            assert_eq!(FadeToBlackCurve::from_token(curve.token()), Some(curve));
        }
        assert_eq!(FadeToBlackCurve::Linear.offset(u16::MAX, 15, 30), 32_767);
        assert_eq!(FadeToBlackCurve::Ease.offset(u16::MAX, 15, 30), 32_767);
        assert!(
            FadeToBlackCurve::Ease.offset(u16::MAX, 3, 30)
                < FadeToBlackCurve::Linear.offset(u16::MAX, 3, 30)
        );
        assert_eq!(FadeToBlackCurve::from_token("smooth"), None);
    }
}
