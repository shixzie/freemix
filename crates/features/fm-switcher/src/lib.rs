//! Deterministic preview/program switching primitives.

mod command;
mod fade_to_black;
mod logical_output;
mod mix_bank;
mod overlay;
mod recorder;
mod state;
mod stinger;
mod stream;
mod transition;

pub use command::{SwitcherCommand, SwitcherError, SwitcherEvent};
pub use fade_to_black::{
    FADE_TO_BLACK_POSITION_DENOMINATOR, FadeToBlackAdvance, FadeToBlackController,
    FadeToBlackError, FadeToBlackFrame, FadeToBlackPosition, FadeToBlackRequest,
    FadeToBlackStarted, FadeToBlackTarget, MAX_FADE_TO_BLACK_DURATION_FRAMES,
};
pub use logical_output::{
    AudioMap, AudioSource, BackgroundPlan, CellFeed, FeedLayer, LOGICAL_OUTPUT_COUNT,
    LogicalOutputConfig, LogicalOutputSlot, LogicalOutputs, MAX_OUTPUT_MULTIVIEW_CELLS,
    MultiViewCell, MultiViewCellPlan, OutputRenderPlan, OutputRoutingError, OutputRoutingFrame,
    OutputSource, OutputVideo, OverlayLayer, OverlayPolicy, OverlaySet, RoutingContext,
};
pub use mix_bank::{
    MAX_MIX_BANK_INPUTS, MAX_MIX_TRANSITION_DURATION_FRAMES, MAX_PENDING_MIX_EFFECTS,
    MAX_SECONDARY_MIXES, MIX_BANK_CAPACITY, MixBank, MixBankCommand, MixBankError, MixBankEvent,
    MixBankFrame, MixConfig, MixEffect, MixFrame, MixId, MixLayer, MixMode, MixSet, MixSource,
    MixState, MixTransition,
};
pub use overlay::{
    MAX_OVERLAY_QUEUE_DEPTH, MAX_OVERLAY_TRANSITION_DURATION_FRAMES, OVERLAY_CHANNEL_COUNT,
    OverlayBorderPreset, OverlayChannelId, OverlayChannelState, OverlayPositionPreset,
    OverlayTransitionAdvance, OverlayTransitionKind,
};
pub use recorder::{RECORDER_SLOT_COUNT, RecorderSlot};
pub use state::{ProgramFrame, SwitcherState};
pub use stinger::{
    MissingMediaFallback, STINGER_SLOT_COUNT, StingerAudioPolicy, StingerDescriptor,
    StingerPlaybackDecision, StingerPreloadState, StingerSlotId, StingerSlotState,
};
pub use stream::{DesiredStream, MAX_STREAM_COUNT, MAX_STREAM_NAME_BYTES, StreamTargetId};
pub use transition::{TBarPosition, TBarState, TransitionKind, TransitionState};
