//! Portable PTZ intents, presets, command queues, and adapter contracts.
//!
//! This crate deliberately contains no transport or device SDK integration.
//! Adapters translate these domain contracts to a protocol at the boundary;
//! the pure VISCA and VISCA-over-IP codecs here produce and parse bytes only.

mod adapter;
mod control_mapping;
mod domain;
mod fake;
mod queue;
mod visca;
mod visca_ip;

pub use adapter::{AdapterError, CommandOutcome, PtzAdapter};
pub use control_mapping::{ControlVector, MouseDragMapping, ResponseCurve, SpeedMapping};
pub use domain::{
    AbsoluteMove, Axis, AxisCapabilities, AxisLimits, CameraCapabilities, CameraDescriptor,
    CameraId, CameraIdError, CameraPosition, ConnectionState, ContinuousMove, ContinuousSource,
    DisconnectReason, MovementId, MovementKind, MovementState, Preset, PresetId,
    PresetVirtualInputId, PtzIntent, RecoveryTelemetry, RelativeMove, Telemetry,
};
pub use fake::{FakeViscaAdapter, FakeViscaCamera};
pub use queue::{CoalescingQueue, PushOutcome, QueueError, QueuedIntent};
pub use visca::{
    FocusMode, PresetAction, VISCA_MAX_LENS_SPEED, VISCA_MAX_MESSAGE_LEN, VISCA_MAX_PAN_SPEED,
    VISCA_MAX_PRESET, VISCA_MAX_TILT_SPEED, VISCA_TERMINATOR, ViscaAddress, ViscaCommand,
    ViscaEncodeError, ViscaErrorCode, ViscaInquiry, ViscaMoveSpeeds, ViscaParseError, ViscaReply,
    ViscaReplyDecoder, decode_lens_position, decode_pan_tilt_position, visca_commands_for_intent,
};
pub use visca_ip::{
    VISCA_IP_HEADER_LEN, ViscaIpControl, ViscaIpError, ViscaIpPacket, ViscaIpPayloadType,
    ViscaIpSequencer,
};
