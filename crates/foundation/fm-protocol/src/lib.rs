//! Transport-neutral wire DTOs and newline-delimited control codec.

mod codec;
mod dto;
mod monitor_image;
mod version;

pub use codec::{
    CodecError, LineDecoder, MAX_BATCH_EVENTS, MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES,
    MAX_FIELDS_PER_MESSAGE, MAX_LINE_BYTES, MAX_LIST_ITEMS, MAX_MESSAGES_PER_PUSH,
    MAX_STREAM_LOCAL_SEGMENTS, MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES,
    decode_line, encode_line,
};
pub use dto::{
    AUDIO_METER_LEVEL_SCALE, AudioMeterChannel, AudioMetersMessage, CapabilityReportMessage,
    CapabilityReportSummary, ClientType, CommandMessage, CommandPayload, CommandResult,
    DiagnosticsRequest, DiagnosticsResponse, DurableEvent, DurableEventBatch, DurableGap,
    EngineIdentity, ErrorMessage, EventCursor, EventMessage, EventPayload, FadeToBlackPosition,
    FadeToBlackState, FieldIssue, HandshakeOutcome, HandshakeRequest, HandshakeResponse,
    HeartbeatAcknowledgementMessage, HeartbeatMessage, InputAudioMeters, InputAudioStripStatus,
    InputStatus, MAX_AUDIO_METER_CHANNELS, MAX_MONITOR_FRAME_HEIGHT, MAX_MONITOR_FRAME_PART_BYTES,
    MAX_MONITOR_FRAME_PARTS, MAX_MONITOR_FRAME_WIDTH, ManualTransitionKind,
    ManualTransitionPosition, ManualTransitionState, ManualTransitionStatus, MonitorFrameMessage,
    MonitorFrameOrigin, MonitorImage, MonitorTap, OutputStatus, OverlayBorderPreset,
    OverlayPositionPreset, OverlayStatus, OverlayTransitionKind, RecordingStatus, ResumeCursor,
    Role, RuntimeDomainBoundary, RuntimeEventMessage, RuntimeFailureDisposition,
    RuntimeLifecycleEvent, ServerHello, ServerIdentity, SnapshotMessage, SnapshotReason,
    StingerAudioPolicy, StingerMissingMediaFallback, StingerReadiness, StingerStatus,
    StreamLocalStatus, StreamRealizedState, StreamStatus, StreamStatusMessage, StreamStatusSample,
    StructuredError, WireInputId, WireMessage, WireOutputId, WireOverlayChannelId,
    WireRecorderSlot, WireStingerSlotId, WireStreamTargetId, choose_handshake_outcome,
};
pub use monitor_image::{
    MonitorFrameAssembler, MonitorImageError, decode_qoi_rgb, encode_qoi_rgb, monitor_frame_parts,
};
pub use version::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};
