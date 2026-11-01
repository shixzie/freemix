//! Engine-side plugin lifecycle and command-only mutation protocol.
//!
//! This crate deliberately models protocol and durable engine authority only.
//! Loading a plugin here means advancing the engine-side lifecycle; process and
//! runtime management belong to a host implementation.

mod catalog;
mod chain;
mod exchange;
mod host;
mod reference;
mod types;

pub use catalog::{
    CatalogError, InstalledPlugin, MANIFEST_FILE, MAX_CAPABILITIES, MAX_IDENTIFIER_BYTES,
    MAX_INSTALLED_PLUGINS, MAX_MANIFEST_BYTES, MAX_NAME_BYTES, PluginCatalog, SkippedPlugin,
    parse_manifest,
};
pub use chain::{
    MAX_PLUGIN_CHAIN_SLOTS, MAX_PLUGIN_LATENCY_FRAMES, MAX_PLUGIN_PARAMETER_NAME_BYTES,
    MAX_PLUGIN_PARAMETERS, PluginChain, PluginChainError, PluginSlot,
};
pub use exchange::{
    DataBatch, DataMessage, EventBatch, EventMessage, ExchangeError, HeartbeatMessage,
    MutationIntent, PluginToEngine, ProtocolLimits,
};
pub use fm_command::{
    ApplyOutcome, CommandEnvelope, CommandId, CommandReceipt, Deadline, DurableEvent,
    IdempotencyKey, Rejection, RejectionCode, Revision, StateEpoch,
};
pub use fm_plugin_api as plugin_api;
pub use host::{
    HostError, PluginCommand, PluginCommandResult, PluginEvent, PluginHost, PluginRecord,
};
pub use reference::{
    MAX_REFERENCE_CLIP_MILLIDB, MAX_REFERENCE_TRIM_MILLIDB, MIN_REFERENCE_CLIP_MILLIDB,
    MIN_REFERENCE_TRIM_MILLIDB, REFERENCE_PLUGINS, REFERENCE_POLARITY, REFERENCE_SOFTCLIP,
    REFERENCE_TRIM, ReferenceError, ReferenceRack, is_reference_plugin, state_bytes,
};
pub use types::{
    ApiVersion, CapabilityDecision, CapabilityId, CrashReport, PluginId, PluginManifest,
    PluginState, QuarantineReason, StateSnapshot, StateVersion,
};
