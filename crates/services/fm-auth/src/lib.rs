//! Dependency-free authentication and authorization primitives.

mod devices;
mod id;
mod pairing;
mod policy;

pub use devices::{
    AttemptLimiter, DeviceError, DevicePairing, DeviceRegistry, DeviceRole, MAX_DEVICE_ID_BYTES,
    MAX_DEVICE_NAME_BYTES, MAX_PAIRED_DEVICES, PairedDevice, RedeemError, TokenDigest,
    sanitize_device_name, valid_device_id, valid_device_name,
};
pub use id::{InvalidIdentity, SessionId, UserId};
pub use pairing::{
    InvalidPairingCode, MAX_PAIRING_FAILED_ATTEMPTS, MAX_PAIRING_LIFETIME_SECONDS, PairingCode,
    PairingCodeValidator, PairingConfigurationError, PairingError,
};
pub use policy::{
    AuthorizationDenial, CommandClass, DenialReason, Permission, Policy, Principal, PrincipalKind,
    Role,
};
