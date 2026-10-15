//! Portable source, sink, discovery, lifecycle, and health contracts.
//!
//! This crate deliberately contains no platform APIs. Native adapters expose
//! their capabilities through these contracts; tests can use the deterministic
//! fake adapters in [`fake`]. Display enumeration, fullscreen display-output
//! configuration, and the display hot-plug state machine live in [`display`].

mod contract;
pub mod display;
pub mod fake;

pub use contract::{
    ClockCapability, DeliveryStatus, DeviceId, Discovery, DiscoveryEvent, DiscoveryEventKind,
    DiscoverySnapshot, DriverState, EndpointCapabilities, EndpointHealth, EndpointHealthState,
    FallbackKind, IoError, LifecycleState, MediaSink, MediaSource, MediaTransfer, MediaUnit,
    OpenOptions, PermissionState, Remediation, SignalLossPolicy, SinkDescriptor, SinkId,
    SinkOutcome, SourceDescriptor, SourceId, TimestampCapabilities, TimestampQuality,
    TimestampValidationError, TimestampValidator, TransferLimits, WriteError, deliver_isolated,
};

pub use fm_capabilities::FormatDescriptor;
pub use fm_frame::{ClockDomainId, MediaTiming};
pub use fm_types::MemoryDomain;
