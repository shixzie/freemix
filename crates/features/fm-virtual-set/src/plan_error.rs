use core::fmt;

use crate::{LayerId, PresetId, TalentId};

/// Errors from virtual-camera moves, layer-stack planning, and preset recall.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    ZeroDenominator,
    ArithmeticOverflow,
    PanOutOfRange {
        value_permille: u32,
    },
    ZoomOutOfRange {
        value_permille: u32,
        minimum: u32,
        maximum: u32,
    },
    TooFewEasingPoints {
        actual: usize,
    },
    TooManyEasingPoints {
        actual: usize,
        maximum: usize,
    },
    EasingEndpoints,
    EasingNotIncreasing {
        index: usize,
    },
    EasingValueOutOfRange {
        index: usize,
    },
    MoveTooLong {
        frames: u32,
        maximum: u32,
    },
    EmptyStack,
    TooManyStackLayers {
        actual: usize,
        maximum: usize,
    },
    TooManyTalentSlots {
        actual: usize,
        maximum: usize,
    },
    MissingBackground,
    DuplicateStackLayer(LayerId),
    DuplicateTalentSlot(TalentId),
    DuplicateBandZOrder {
        z_order: i32,
        first: LayerId,
        second: LayerId,
    },
    InvalidSetRect(LayerId),
    ParallaxOutOfRange(LayerId),
    OpacityOutOfRange(LayerId),
    HookOnBackground(LayerId),
    ReflectionAxisInsideLayer(LayerId),
    InvalidReflectionFade(LayerId),
    ShadowOffsetOutOfRange(LayerId),
    ShadowBlurTooLarge {
        layer_id: LayerId,
        radius_px: u32,
        maximum: u32,
    },
    InvalidOutputSize {
        width: u32,
        height: u32,
    },
    EmptyPresetName,
    PresetNameTooLong {
        bytes: usize,
        maximum: usize,
    },
    DuplicatePreset(PresetId),
    TooManyPresets {
        maximum: usize,
    },
    UnknownPreset(PresetId),
    RecallInPast {
        requested: u64,
        earliest: u64,
    },
    NonMonotonicFrame {
        requested: u64,
        last: u64,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "virtual-set plan error: {self:?}")
    }
}

impl std::error::Error for PlanError {}
