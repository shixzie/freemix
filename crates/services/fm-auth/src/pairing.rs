use std::{error::Error, fmt, num::NonZeroU64};

/// Maximum accepted pairing-code lifetime: ten minutes.
pub const MAX_PAIRING_LIFETIME_SECONDS: u64 = 600;

/// Maximum incorrect guesses tolerated per outstanding code. The guess that
/// reaches this limit invalidates the code permanently.
pub const MAX_PAIRING_FAILED_ATTEMPTS: u32 = 5;

/// Caller-supplied opaque pairing secret.
///
/// Equality is constant-time over equal-length encodings: every byte is
/// compared without an early exit on the first difference. Codes of different
/// lengths are unequal without inspecting their contents.
#[derive(Clone)]
pub struct PairingCode(String);

impl PairingCode {
    /// Wraps an externally generated opaque code.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPairingCode`] when the value is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidPairingCode> {
        let value = value.into();
        if value.is_empty() {
            Err(InvalidPairingCode)
        } else {
            Ok(Self(value))
        }
    }

    fn matches(&self, candidate: &Self) -> bool {
        let expected = self.0.as_bytes();
        let candidate = candidate.0.as_bytes();
        if expected.len() != candidate.len() {
            return false;
        }
        let difference = expected
            .iter()
            .zip(candidate)
            .fold(0_u8, |difference, (left, right)| {
                // `black_box` keeps the optimizer from turning the fold into a
                // short-circuiting comparison.
                std::hint::black_box(difference | (left ^ right))
            });
        difference == 0
    }
}

impl PartialEq for PairingCode {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}

impl Eq for PairingCode {}

impl fmt::Debug for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingCode([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPairingCode;

impl fmt::Display for InvalidPairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("pairing code is empty")
    }
}

impl Error for InvalidPairingCode {}

/// One-use pairing-code validator with caller-injected time and secret.
///
/// At most [`MAX_PAIRING_FAILED_ATTEMPTS`] incorrect guesses are tolerated
/// before the code is invalidated, bounding online brute force.
pub struct PairingCodeValidator {
    expected: PairingCode,
    expires_at: u64,
    consumed: bool,
    failed_attempts: u32,
}

impl PairingCodeValidator {
    /// Creates a validator without reading clocks or generating randomness.
    ///
    /// # Errors
    ///
    /// Returns [`PairingConfigurationError`] when the lifetime is longer than
    /// ten minutes or the expiry timestamp overflows.
    pub fn new(
        expected: PairingCode,
        issued_at: u64,
        lifetime_seconds: NonZeroU64,
    ) -> Result<Self, PairingConfigurationError> {
        if lifetime_seconds.get() > MAX_PAIRING_LIFETIME_SECONDS {
            return Err(PairingConfigurationError::LifetimeTooLong {
                provided: lifetime_seconds.get(),
                maximum: MAX_PAIRING_LIFETIME_SECONDS,
            });
        }
        let expires_at = issued_at
            .checked_add(lifetime_seconds.get())
            .ok_or(PairingConfigurationError::ExpiryOverflow)?;
        Ok(Self {
            expected,
            expires_at,
            consumed: false,
            failed_attempts: 0,
        })
    }

    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    #[must_use]
    pub const fn is_consumed(&self) -> bool {
        self.consumed
    }

    /// Number of incorrect guesses recorded against this code.
    #[must_use]
    pub const fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// True once incorrect guesses reached [`MAX_PAIRING_FAILED_ATTEMPTS`];
    /// the code can then never be consumed.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.failed_attempts >= MAX_PAIRING_FAILED_ATTEMPTS
    }

    /// Validates and consumes the code at an injected timestamp.
    ///
    /// A correct attempt consumes the code. Expired attempts neither consume
    /// the code nor count as failures. Each incorrect attempt counts as a
    /// failure; the attempt that reaches [`MAX_PAIRING_FAILED_ATTEMPTS`] and
    /// every later attempt, including a correct one, return
    /// [`PairingError::AttemptsExhausted`].
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when already consumed, invalidated by too many
    /// failed attempts, expired, or incorrect.
    pub fn consume(&mut self, candidate: &PairingCode, now: u64) -> Result<(), PairingError> {
        if self.consumed {
            return Err(PairingError::AlreadyConsumed);
        }
        if self.is_exhausted() {
            return Err(PairingError::AttemptsExhausted);
        }
        if now >= self.expires_at {
            return Err(PairingError::Expired);
        }
        if !self.expected.matches(candidate) {
            self.failed_attempts = self.failed_attempts.saturating_add(1);
            return Err(if self.is_exhausted() {
                PairingError::AttemptsExhausted
            } else {
                PairingError::Invalid
            });
        }
        self.consumed = true;
        Ok(())
    }
}

impl fmt::Debug for PairingCodeValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingCodeValidator")
            .field("expected", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("consumed", &self.consumed)
            .field("failed_attempts", &self.failed_attempts)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingConfigurationError {
    LifetimeTooLong { provided: u64, maximum: u64 },
    ExpiryOverflow,
}

impl fmt::Display for PairingConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LifetimeTooLong { provided, maximum } => write!(
                formatter,
                "pairing lifetime {provided}s exceeds the {maximum}s maximum"
            ),
            Self::ExpiryOverflow => formatter.write_str("pairing expiry timestamp overflowed"),
        }
    }
}

impl Error for PairingConfigurationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingError {
    Invalid,
    Expired,
    AlreadyConsumed,
    /// Too many incorrect guesses; the code is permanently invalidated.
    AttemptsExhausted,
}

impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "pairing code is invalid",
            Self::Expired => "pairing code has expired",
            Self::AlreadyConsumed => "pairing code was already consumed",
            Self::AttemptsExhausted => {
                "pairing code was invalidated after too many failed attempts"
            }
        })
    }
}

impl Error for PairingError {}
