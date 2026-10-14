//! Deterministic send-side bandwidth adaptation for guest return video.
//!
//! Receiver estimates (REMB) and transport-wide feedback estimates (TWCC)
//! arrive as plain numbers; the controller takes the minimum of the fresh
//! estimates, applies headroom, and walks a resolution ladder capped at
//! 1920×1080 (or 1080×1920 for a vertical return feed) and 4 Mbps. Downgrades are immediate, upgrades require the next
//! rung's floor plus a margin to hold continuously for a configured time, and
//! high loss steps down at most once per hold period.

use core::fmt;

use crate::sdp::MAX_VIDEO_BITRATE_BPS;

/// Maximum rungs in a ladder.
pub const MAX_LADDER_RUNGS: usize = 8;
/// Largest permitted landscape rung width; a portrait rung swaps the axes.
pub const MAX_RUNG_WIDTH: u32 = 1920;
/// Largest permitted landscape rung height; a portrait rung swaps the axes.
pub const MAX_RUNG_HEIGHT: u32 = 1080;
/// Largest permitted rung frame rate.
pub const MAX_RUNG_FRAME_RATE: u16 = 60;
/// Largest accepted input estimate (10 Gbps); larger values are rejected.
pub const MAX_ESTIMATE_BPS: u64 = 10_000_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BandwidthError {
    EmptyLadder,
    TooManyRungs { maximum: usize },
    InvalidRung { index: usize, reason: &'static str },
    LadderNotAscending { index: usize },
    InvalidConfig(&'static str),
    InvalidEstimate(&'static str),
    NonMonotonicTime { previous_ms: u64, actual_ms: u64 },
    RungOutOfRange { index: usize, rungs: usize },
}

impl fmt::Display for BandwidthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLadder => formatter.write_str("bitrate ladder is empty"),
            Self::TooManyRungs { maximum } => write!(formatter, "ladder exceeds {maximum} rungs"),
            Self::InvalidRung { index, reason } => write!(formatter, "rung {index}: {reason}"),
            Self::LadderNotAscending { index } => {
                write!(
                    formatter,
                    "rung {index} does not ascend from the previous rung"
                )
            }
            Self::InvalidConfig(reason) => write!(formatter, "invalid controller config: {reason}"),
            Self::InvalidEstimate(reason) => write!(formatter, "invalid estimate: {reason}"),
            Self::NonMonotonicTime {
                previous_ms,
                actual_ms,
            } => write!(
                formatter,
                "estimate time {actual_ms} precedes {previous_ms}"
            ),
            Self::RungOutOfRange { index, rungs } => {
                write!(formatter, "rung {index} outside a {rungs}-rung ladder")
            }
        }
    }
}

impl std::error::Error for BandwidthError {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LadderRung {
    pub width: u32,
    pub height: u32,
    pub frame_rate: u16,
    /// Usable bandwidth below which this rung is abandoned.
    pub min_bitrate_bps: u64,
    /// Encoder ceiling for this rung.
    pub max_bitrate_bps: u64,
}

impl LadderRung {
    #[must_use]
    pub const fn new(
        width: u32,
        height: u32,
        frame_rate: u16,
        min_bitrate_bps: u64,
        max_bitrate_bps: u64,
    ) -> Self {
        Self {
            width,
            height,
            frame_rate,
            min_bitrate_bps,
            max_bitrate_bps,
        }
    }
}

/// Validated rungs ordered from lowest to highest quality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BitrateLadder {
    rungs: Vec<LadderRung>,
}

impl BitrateLadder {
    /// # Errors
    /// Rejects empty or oversize ladders, rungs above 1080p60/4 Mbps, odd or
    /// zero dimensions, `min > max`, and non-ascending rungs. The 1080p bound
    /// is orientation independent: a landscape or square rung may be at most
    /// 1920x1080 and a portrait rung at most 1080x1920.
    pub fn new(rungs: Vec<LadderRung>) -> Result<Self, BandwidthError> {
        if rungs.is_empty() {
            return Err(BandwidthError::EmptyLadder);
        }
        if rungs.len() > MAX_LADDER_RUNGS {
            return Err(BandwidthError::TooManyRungs {
                maximum: MAX_LADDER_RUNGS,
            });
        }
        for (index, rung) in rungs.iter().enumerate() {
            let invalid = |reason| BandwidthError::InvalidRung { index, reason };
            if rung.width == 0 || rung.height == 0 || rung.frame_rate == 0 {
                return Err(invalid("dimensions and frame rate must be nonzero"));
            }
            if rung.width % 2 != 0 || rung.height % 2 != 0 {
                return Err(invalid("dimensions must be even"));
            }
            let (long_side, short_side) = if rung.height > rung.width {
                (rung.height, rung.width)
            } else {
                (rung.width, rung.height)
            };
            if long_side > MAX_RUNG_WIDTH || short_side > MAX_RUNG_HEIGHT {
                return Err(invalid("exceeds 1920x1080"));
            }
            if rung.frame_rate > MAX_RUNG_FRAME_RATE {
                return Err(invalid("exceeds 60 fps"));
            }
            if rung.min_bitrate_bps == 0 || rung.min_bitrate_bps > rung.max_bitrate_bps {
                return Err(invalid("bitrate range is empty"));
            }
            if rung.max_bitrate_bps > MAX_VIDEO_BITRATE_BPS {
                return Err(invalid("exceeds 4 Mbps"));
            }
            if let Some(previous) = index.checked_sub(1).map(|previous| rungs[previous]) {
                let pixels = u64::from(rung.width) * u64::from(rung.height);
                let previous_pixels = u64::from(previous.width) * u64::from(previous.height);
                if pixels < previous_pixels
                    || rung.min_bitrate_bps <= previous.min_bitrate_bps
                    || rung.max_bitrate_bps < previous.max_bitrate_bps
                {
                    return Err(BandwidthError::LadderNotAscending { index });
                }
            }
        }
        Ok(Self { rungs })
    }

    /// 180p15 → 360p30 → 540p30 → 720p30 → 1080p30 at up to 4 Mbps.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            rungs: vec![
                LadderRung::new(320, 180, 15, 150_000, 300_000),
                LadderRung::new(640, 360, 30, 400_000, 800_000),
                LadderRung::new(960, 540, 30, 800_000, 1_500_000),
                LadderRung::new(1280, 720, 30, 1_500_000, 2_500_000),
                LadderRung::new(1920, 1080, 30, 2_500_000, 4_000_000),
            ],
        }
    }

    #[must_use]
    pub fn rungs(&self) -> &[LadderRung] {
        &self.rungs
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EstimateSource {
    /// Receiver-side REMB estimate.
    Remb,
    /// Sender-side estimate derived from transport-wide CC feedback.
    Twcc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthEstimate {
    pub at_ms: u64,
    pub source: EstimateSource,
    pub bitrate_bps: u64,
    /// Fraction lost in the reporting interval, 0–1000.
    pub loss_permille: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerConfig {
    /// Share of the estimate considered usable, 1–1000.
    pub headroom_permille: u16,
    /// Extra margin above the next rung's floor required to upgrade.
    pub upgrade_margin_permille: u16,
    /// How long upgrade conditions must hold continuously.
    pub upgrade_hold_ms: u64,
    /// Loss at or above this steps down one rung (at most once per hold).
    pub high_loss_permille: u16,
    /// Loss at or above this blocks upgrades.
    pub moderate_loss_permille: u16,
    /// An estimate source older than this is ignored.
    pub stale_after_ms: u64,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            headroom_permille: 900,
            upgrade_margin_permille: 150,
            upgrade_hold_ms: 5_000,
            high_loss_permille: 100,
            moderate_loss_permille: 20,
            stale_after_ms: 3_000,
        }
    }
}

impl ControllerConfig {
    fn validate(&self) -> Result<(), BandwidthError> {
        if self.headroom_permille == 0 || self.headroom_permille > 1000 {
            return Err(BandwidthError::InvalidConfig(
                "headroom must be 1-1000 permille",
            ));
        }
        if self.upgrade_margin_permille > 1000 {
            return Err(BandwidthError::InvalidConfig(
                "upgrade margin above 1000 permille",
            ));
        }
        if self.upgrade_hold_ms == 0 || self.stale_after_ms == 0 {
            return Err(BandwidthError::InvalidConfig(
                "hold and staleness must be nonzero",
            ));
        }
        if self.moderate_loss_permille == 0
            || self.moderate_loss_permille >= self.high_loss_permille
            || self.high_loss_permille > 1000
        {
            return Err(BandwidthError::InvalidConfig(
                "loss thresholds must satisfy 0 < moderate < high <= 1000",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChangeReason {
    InsufficientBandwidth,
    HighLoss,
    SustainedHeadroom,
    OperatorCap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RungChange {
    pub from: usize,
    pub to: usize,
    pub reason: ChangeReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthDecision {
    pub rung: usize,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u16,
    /// Encoder target: usable bandwidth clamped to the rung's range.
    pub target_bitrate_bps: u64,
    /// Minimum of fresh estimates after headroom.
    pub usable_bitrate_bps: u64,
    pub change: Option<RungChange>,
}

#[derive(Clone, Debug)]
pub struct BandwidthController {
    ladder: BitrateLadder,
    config: ControllerConfig,
    current: usize,
    max_rung: usize,
    last_at_ms: Option<u64>,
    remb: Option<(u64, u64)>,
    twcc: Option<(u64, u64)>,
    upgrade_since_ms: Option<u64>,
    last_loss_step_ms: Option<u64>,
}

impl BandwidthController {
    /// Starts on the lowest rung.
    ///
    /// # Errors
    /// Returns [`BandwidthError::InvalidConfig`] for inconsistent settings.
    pub fn new(ladder: BitrateLadder, config: ControllerConfig) -> Result<Self, BandwidthError> {
        config.validate()?;
        let max_rung = ladder.rungs.len() - 1;
        Ok(Self {
            ladder,
            config,
            current: 0,
            max_rung,
            last_at_ms: None,
            remb: None,
            twcc: None,
            upgrade_since_ms: None,
            last_loss_step_ms: None,
        })
    }

    #[must_use]
    pub const fn current_rung(&self) -> usize {
        self.current
    }

    #[must_use]
    pub fn ladder(&self) -> &BitrateLadder {
        &self.ladder
    }

    /// Limits the highest selectable rung (`None` removes the cap). Takes
    /// effect on the next estimate.
    ///
    /// # Errors
    /// Returns [`BandwidthError::RungOutOfRange`] for an unknown rung.
    pub fn set_max_rung(&mut self, max_rung: Option<usize>) -> Result<(), BandwidthError> {
        let rungs = self.ladder.rungs.len();
        self.max_rung = match max_rung {
            Some(index) if index >= rungs => {
                return Err(BandwidthError::RungOutOfRange { index, rungs });
            }
            Some(index) => index,
            None => rungs - 1,
        };
        self.upgrade_since_ms = None;
        Ok(())
    }

    /// Feeds one estimate and returns the resulting decision.
    ///
    /// # Errors
    /// Rejects time going backwards, loss above 1000 permille, and estimates
    /// above [`MAX_ESTIMATE_BPS`]; rejected input leaves state unchanged.
    pub fn on_estimate(
        &mut self,
        estimate: BandwidthEstimate,
    ) -> Result<BandwidthDecision, BandwidthError> {
        if let Some(previous_ms) = self.last_at_ms
            && estimate.at_ms < previous_ms
        {
            return Err(BandwidthError::NonMonotonicTime {
                previous_ms,
                actual_ms: estimate.at_ms,
            });
        }
        if estimate.loss_permille > 1000 {
            return Err(BandwidthError::InvalidEstimate("loss above 1000 permille"));
        }
        if estimate.bitrate_bps > MAX_ESTIMATE_BPS {
            return Err(BandwidthError::InvalidEstimate("bitrate above 10 Gbps"));
        }
        let now = estimate.at_ms;
        self.last_at_ms = Some(now);
        let sample = Some((now, estimate.bitrate_bps));
        match estimate.source {
            EstimateSource::Remb => self.remb = sample,
            EstimateSource::Twcc => self.twcc = sample,
        }
        let stale_after = self.config.stale_after_ms;
        let effective = [self.remb, self.twcc]
            .into_iter()
            .flatten()
            .filter(|(at, _)| now - at <= stale_after)
            .map(|(_, bps)| bps)
            .min()
            .unwrap_or(estimate.bitrate_bps);
        let usable = effective * u64::from(self.config.headroom_permille) / 1000;

        let from = self.current;
        let rungs = &self.ladder.rungs;
        let mut reason = None;
        if self.current > self.max_rung {
            self.current = self.max_rung;
            reason = Some(ChangeReason::OperatorCap);
            self.upgrade_since_ms = None;
        }
        let loss_step_allowed = self
            .last_loss_step_ms
            .is_none_or(|at| now - at >= self.config.upgrade_hold_ms);
        if estimate.loss_permille >= self.config.high_loss_permille
            && self.current > 0
            && loss_step_allowed
        {
            self.current -= 1;
            self.last_loss_step_ms = Some(now);
            self.upgrade_since_ms = None;
            reason = Some(ChangeReason::HighLoss);
        }
        if usable < rungs[self.current].min_bitrate_bps {
            let fitting = rungs[..self.current]
                .iter()
                .rposition(|rung| rung.min_bitrate_bps <= usable)
                .unwrap_or(0);
            if fitting != self.current {
                self.current = fitting;
                reason = Some(ChangeReason::InsufficientBandwidth);
            }
            self.upgrade_since_ms = None;
        } else if reason.is_none()
            && self.current < self.max_rung
            && estimate.loss_permille < self.config.moderate_loss_permille
            && usable
                >= rungs[self.current + 1].min_bitrate_bps
                    * (1000 + u64::from(self.config.upgrade_margin_permille))
                    / 1000
        {
            let since = *self.upgrade_since_ms.get_or_insert(now);
            if now - since >= self.config.upgrade_hold_ms {
                self.current += 1;
                self.upgrade_since_ms = None;
                reason = Some(ChangeReason::SustainedHeadroom);
            }
        } else {
            self.upgrade_since_ms = None;
        }

        let rung = rungs[self.current];
        Ok(BandwidthDecision {
            rung: self.current,
            width: rung.width,
            height: rung.height,
            frame_rate: rung.frame_rate,
            target_bitrate_bps: usable.clamp(rung.min_bitrate_bps, rung.max_bitrate_bps),
            usable_bitrate_bps: usable,
            change: reason
                .filter(|_| self.current != from)
                .map(|reason| RungChange {
                    from,
                    to: self.current,
                    reason,
                }),
        })
    }
}
