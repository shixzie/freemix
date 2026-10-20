//! Paired web devices: one outstanding pairing at a time, redeemed for a
//! per-device bearer token that is stored only as a digest.
//!
//! Like the rest of this crate, nothing here reads clocks, generates
//! randomness, or hashes: the caller injects the time, the pairing codes,
//! and the [`TokenDigest`] of each token (the daemon uses SHA-256 over 32
//! random bytes, so a fast unsalted digest is sufficient).

use std::{collections::BTreeMap, error::Error, fmt};

use crate::{
    MAX_PAIRING_FAILED_ATTEMPTS, MAX_PAIRING_LIFETIME_SECONDS, PairingCode, PairingError, Role,
};

/// The most devices a registry holds; pairing beyond it is refused.
pub const MAX_PAIRED_DEVICES: usize = 32;
/// The longest device name, in bytes.
pub const MAX_DEVICE_NAME_BYTES: usize = 48;
/// The longest device identifier, in bytes.
pub const MAX_DEVICE_ID_BYTES: usize = 32;

/// The role a paired device is granted: what the web pages offer today.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeviceRole {
    /// A tally light: watches only.
    Viewer,
    /// Mutes strips and watches.
    Audio,
    /// Switches, records, streams, and mutes.
    Operator,
}

impl DeviceRole {
    /// Every role a session of this device may ask for in its handshake:
    /// an Operator device may also open the tally page as a Viewer.
    #[must_use]
    pub const fn granted_roles(self) -> &'static [Role] {
        match self {
            Self::Viewer => &[Role::Viewer],
            Self::Audio => &[Role::Viewer, Role::Audio],
            Self::Operator => &[Role::Viewer, Role::Audio, Role::Operator],
        }
    }

    /// The stable token used on the wire and on disk.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Audio => "audio",
            Self::Operator => "operator",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "viewer" => Some(Self::Viewer),
            "audio" => Some(Self::Audio),
            "operator" => Some(Self::Operator),
            _ => None,
        }
    }
}

/// The digest of one device token. Equality is constant-time.
#[derive(Clone, Copy)]
pub struct TokenDigest([u8; 32]);

impl TokenDigest {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn matches(&self, other: &Self) -> bool {
        let difference = self
            .0
            .iter()
            .zip(other.0)
            .fold(0_u8, |difference, (left, right)| {
                std::hint::black_box(difference | (left ^ right))
            });
        difference == 0
    }
}

impl PartialEq for TokenDigest {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}

impl Eq for TokenDigest {}

impl fmt::Debug for TokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenDigest([redacted])")
    }
}

/// One paired device as stored: never its token, only the token's digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub role: DeviceRole,
    pub paired_at_ms: u64,
    pub last_seen_ms: Option<u64>,
    pub digest: TokenDigest,
}

impl PairedDevice {
    /// Checks the identifier and name bounds.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceError::InvalidId`] or [`DeviceError::InvalidName`].
    pub fn validate(&self) -> Result<(), DeviceError> {
        if !valid_device_id(&self.id) {
            return Err(DeviceError::InvalidId);
        }
        if !valid_device_name(&self.name) {
            return Err(DeviceError::InvalidName);
        }
        Ok(())
    }
}

/// `[a-z0-9-]{1,32}` starting with a letter.
#[must_use]
pub fn valid_device_id(id: &str) -> bool {
    id.len() <= MAX_DEVICE_ID_BYTES
        && id.starts_with(|first: char| first.is_ascii_lowercase())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// A non-empty name of at most [`MAX_DEVICE_NAME_BYTES`] without control
/// characters or surrounding whitespace.
#[must_use]
pub fn valid_device_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_DEVICE_NAME_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

/// Turns any submitted label into a valid device name, or `None` when
/// nothing printable is left.
#[must_use]
pub fn sanitize_device_name(name: &str) -> Option<String> {
    let mut cleaned = String::new();
    for character in name.trim().chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if cleaned.len() + character.len_utf8() > MAX_DEVICE_NAME_BYTES {
            break;
        }
        cleaned.push(character);
    }
    let cleaned = cleaned.trim().to_owned();
    valid_device_name(&cleaned).then_some(cleaned)
}

/// The outstanding pairing: a short code to type and a long code carried by
/// the QR link, both one-use and sharing one failed-attempt budget.
pub struct DevicePairing {
    short: PairingCode,
    link: PairingCode,
    role: DeviceRole,
    expires_at_ms: u64,
    failed_attempts: u32,
}

impl DevicePairing {
    #[must_use]
    pub const fn role(&self) -> DeviceRole {
        self.role
    }

    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    #[must_use]
    pub const fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// The code an operator types, for the console that started pairing.
    #[must_use]
    pub fn short_code(&self) -> &str {
        self.short.reveal()
    }

    /// The code the QR link carries, for the console that started pairing.
    #[must_use]
    pub fn link_code(&self) -> &str {
        self.link.reveal()
    }
}

impl fmt::Debug for DevicePairing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DevicePairing")
            .field("codes", &"[redacted]")
            .field("role", &self.role)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("failed_attempts", &self.failed_attempts)
            .finish_non_exhaustive()
    }
}

/// Why a device operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceError {
    InvalidId,
    InvalidName,
    DuplicateId,
    TooManyDevices,
    LifetimeTooLong,
    UnknownDevice,
}

impl fmt::Display for DeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "device id is invalid",
            Self::InvalidName => "device name is invalid",
            Self::DuplicateId => "device id is already paired",
            Self::TooManyDevices => "too many paired devices",
            Self::LifetimeTooLong => "pairing lifetime is too long",
            Self::UnknownDevice => "no such paired device",
        })
    }
}

impl Error for DeviceError {}

/// Paired devices and the one outstanding pairing.
#[derive(Debug, Default)]
pub struct DeviceRegistry {
    devices: Vec<PairedDevice>,
    pairing: Option<DevicePairing>,
}

impl DeviceRegistry {
    /// A registry holding previously stored devices.
    ///
    /// # Errors
    ///
    /// Returns a [`DeviceError`] for an invalid or duplicate device or more
    /// than [`MAX_PAIRED_DEVICES`].
    pub fn with_devices(devices: Vec<PairedDevice>) -> Result<Self, DeviceError> {
        let mut registry = Self::default();
        for device in devices {
            registry.insert(device)?;
        }
        Ok(registry)
    }

    #[must_use]
    pub fn devices(&self) -> &[PairedDevice] {
        &self.devices
    }

    /// The outstanding pairing, once expired or exhausted pairings are
    /// dropped at `now_ms`.
    pub fn pairing(&mut self, now_ms: u64) -> Option<&DevicePairing> {
        self.expire(now_ms);
        self.pairing.as_ref()
    }

    /// Starts pairing, replacing any outstanding pairing.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceError::LifetimeTooLong`] beyond
    /// [`MAX_PAIRING_LIFETIME_SECONDS`] and
    /// [`DeviceError::TooManyDevices`] when the registry is full.
    pub fn start_pairing(
        &mut self,
        short: PairingCode,
        link: PairingCode,
        role: DeviceRole,
        now_ms: u64,
        lifetime_ms: u64,
    ) -> Result<&DevicePairing, DeviceError> {
        if lifetime_ms == 0 || lifetime_ms > MAX_PAIRING_LIFETIME_SECONDS * 1_000 {
            return Err(DeviceError::LifetimeTooLong);
        }
        if self.devices.len() >= MAX_PAIRED_DEVICES {
            return Err(DeviceError::TooManyDevices);
        }
        Ok(self.pairing.insert(DevicePairing {
            short,
            link,
            role,
            expires_at_ms: now_ms.saturating_add(lifetime_ms),
            failed_attempts: 0,
        }))
    }

    /// Withdraws the outstanding pairing; `true` when there was one.
    pub fn cancel_pairing(&mut self) -> bool {
        self.pairing.take().is_some()
    }

    fn expire(&mut self, now_ms: u64) {
        if self.pairing.as_ref().is_some_and(|pairing| {
            now_ms >= pairing.expires_at_ms
                || pairing.failed_attempts >= MAX_PAIRING_FAILED_ATTEMPTS
        }) {
            self.pairing = None;
        }
    }

    /// Redeems either code of the outstanding pairing and, on success,
    /// pairs the device whose token digest the caller computed. The pairing
    /// is consumed by success and invalidated by the
    /// [`MAX_PAIRING_FAILED_ATTEMPTS`]th wrong code.
    ///
    /// `device` produces the new device's identifier and token digest; it is
    /// only called once the code matched.
    ///
    /// # Errors
    ///
    /// Returns [`RedeemError::Pairing`] with [`PairingError::Expired`] when
    /// nothing is outstanding (never started, expired, consumed, or
    /// exhausted), [`PairingError::Invalid`] or
    /// [`PairingError::AttemptsExhausted`] for a wrong code, and
    /// [`RedeemError::Device`] when the new device cannot be stored.
    pub fn redeem(
        &mut self,
        candidate: &PairingCode,
        name: &str,
        now_ms: u64,
        device: impl FnOnce() -> (String, TokenDigest),
    ) -> Result<&PairedDevice, RedeemError> {
        self.expire(now_ms);
        let Some(pairing) = self.pairing.as_mut() else {
            return Err(RedeemError::Pairing(PairingError::Expired));
        };
        // Both comparisons always run, so timing does not reveal which code
        // a guess was closer to.
        let short = pairing.short == *candidate;
        let link = pairing.link == *candidate;
        if !(short | link) {
            pairing.failed_attempts = pairing.failed_attempts.saturating_add(1);
            let exhausted = pairing.failed_attempts >= MAX_PAIRING_FAILED_ATTEMPTS;
            if exhausted {
                self.pairing = None;
            }
            return Err(RedeemError::Pairing(if exhausted {
                PairingError::AttemptsExhausted
            } else {
                PairingError::Invalid
            }));
        }
        let role = pairing.role;
        self.pairing = None;
        let (id, digest) = device();
        self.insert(PairedDevice {
            id,
            name: name.to_owned(),
            role,
            paired_at_ms: now_ms,
            last_seen_ms: None,
            digest,
        })
        .map_err(RedeemError::Device)
        .map(|index| &self.devices[index])
    }

    /// Stores `device` and returns its index.
    fn insert(&mut self, device: PairedDevice) -> Result<usize, DeviceError> {
        device.validate()?;
        if self.devices.len() >= MAX_PAIRED_DEVICES {
            return Err(DeviceError::TooManyDevices);
        }
        if self.devices.iter().any(|known| known.id == device.id) {
            return Err(DeviceError::DuplicateId);
        }
        self.devices.push(device);
        Ok(self.devices.len() - 1)
    }

    /// The device whose token has `digest`, checked against every stored
    /// digest in constant time per entry; records it as seen at `now_ms`.
    pub fn authenticate(&mut self, digest: &TokenDigest, now_ms: u64) -> Option<&PairedDevice> {
        let mut found = None;
        for (index, device) in self.devices.iter().enumerate() {
            if device.digest == *digest {
                found = Some(index);
            }
        }
        let device = &mut self.devices[found?];
        device.last_seen_ms = Some(now_ms);
        Some(device)
    }

    /// Whether `id` is still paired.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.devices.iter().any(|device| device.id == id)
    }

    /// Unpairs `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceError::UnknownDevice`] when it is not paired.
    pub fn revoke(&mut self, id: &str) -> Result<PairedDevice, DeviceError> {
        let index = self
            .devices
            .iter()
            .position(|device| device.id == id)
            .ok_or(DeviceError::UnknownDevice)?;
        Ok(self.devices.remove(index))
    }
}

/// Why redeeming a pairing code failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedeemError {
    Pairing(PairingError),
    Device(DeviceError),
}

impl fmt::Display for RedeemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pairing(error) => error.fmt(formatter),
            Self::Device(error) => error.fmt(formatter),
        }
    }
}

impl Error for RedeemError {}

/// A per-source sliding-window attempt limiter with injected time, plus a
/// global ceiling across every source.
#[derive(Debug)]
pub struct AttemptLimiter<K: Ord> {
    window_ms: u64,
    per_source: u32,
    global: u32,
    sources: BTreeMap<K, Vec<u64>>,
    all: Vec<u64>,
    max_sources: usize,
}

impl<K: Ord + Clone> AttemptLimiter<K> {
    /// At most `per_source` attempts per source and `global` attempts in
    /// total within any `window_ms`; at most `max_sources` sources are
    /// tracked (the global ceiling still bounds the rest).
    #[must_use]
    pub fn new(window_ms: u64, per_source: u32, global: u32, max_sources: usize) -> Self {
        Self {
            window_ms,
            per_source,
            global,
            sources: BTreeMap::new(),
            all: Vec::new(),
            max_sources,
        }
    }

    /// Records one attempt from `source` at `now_ms` when allowed.
    ///
    /// # Errors
    ///
    /// Returns the milliseconds until the next attempt would be allowed.
    pub fn attempt(&mut self, source: &K, now_ms: u64) -> Result<(), u64> {
        let window = self.window_ms;
        let fresh = |times: &mut Vec<u64>| times.retain(|at| now_ms < at.saturating_add(window));
        fresh(&mut self.all);
        self.sources.retain(|_, times| {
            fresh(times);
            !times.is_empty()
        });
        let retry_after = |times: &[u64]| {
            times
                .first()
                .map_or(window, |first| first.saturating_add(window) - now_ms)
        };
        if self.all.len() >= self.global as usize {
            return Err(retry_after(&self.all));
        }
        if let Some(times) = self.sources.get(source) {
            if times.len() >= self.per_source as usize {
                return Err(retry_after(times));
            }
        } else if self.sources.len() >= self.max_sources {
            return Err(window);
        }
        self.all.push(now_ms);
        self.sources.entry(source.clone()).or_default().push(now_ms);
        Ok(())
    }
}
