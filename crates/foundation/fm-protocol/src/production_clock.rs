//! Production clock readings for `diagnostics_response` (SW-013, Protocol
//! 2.149).
//!
//! `production_clocks` is one
//! `<name-hex>,<kind>,<state>,<text-hex>,<value-ms|none>,<expired 0|1>,<zone-hex|none>`
//! entry per clock in strictly ascending name order, `;`-separated, and
//! empty when the show defines none. Names, text, and zones are lowercase
//! hex so a label can never contain a delimiter.

use crate::CodecError;

/// Maximum clocks in one diagnostics response.
pub const MAX_PRODUCTION_CLOCK_STATUSES: usize = 256;
/// Longest clock name, in bytes.
pub const MAX_PRODUCTION_CLOCK_NAME_BYTES: usize = 64;
/// Longest display text, in bytes.
pub const MAX_PRODUCTION_CLOCK_TEXT_BYTES: usize = 64;
/// Longest zone abbreviation, in bytes.
pub const MAX_PRODUCTION_CLOCK_ZONE_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProductionClockKind {
    Timer,
    TimeOfDay,
    MediaRemaining,
    Stream,
    Recording,
    Replay,
    MultiCorder,
    External,
}

impl ProductionClockKind {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::TimeOfDay => "time_of_day",
            Self::MediaRemaining => "media_remaining",
            Self::Stream => "stream",
            Self::Recording => "recording",
            Self::Replay => "replay",
            Self::MultiCorder => "multicorder",
            Self::External => "external",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "timer" => Some(Self::Timer),
            "time_of_day" => Some(Self::TimeOfDay),
            "media_remaining" => Some(Self::MediaRemaining),
            "stream" => Some(Self::Stream),
            "recording" => Some(Self::Recording),
            "replay" => Some(Self::Replay),
            "multicorder" => Some(Self::MultiCorder),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProductionClockState {
    Idle,
    Running,
    Paused,
    Expired,
    Stopped,
    Live,
    Unavailable,
}

impl ProductionClockState {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Expired => "expired",
            Self::Stopped => "stopped",
            Self::Live => "live",
            Self::Unavailable => "unavailable",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "idle" => Some(Self::Idle),
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "expired" => Some(Self::Expired),
            "stopped" => Some(Self::Stopped),
            "live" => Some(Self::Live),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// One production clock's reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionClockStatus {
    pub name: String,
    pub kind: ProductionClockKind,
    pub state: ProductionClockState,
    pub text: String,
    pub value_ms: Option<i64>,
    pub expired: bool,
    pub zone: Option<String>,
}

impl ProductionClockStatus {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let name = &self.name;
        !name.is_empty()
            && name.len() <= MAX_PRODUCTION_CLOCK_NAME_BYTES
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            && !self.text.is_empty()
            && self.text.len() <= MAX_PRODUCTION_CLOCK_TEXT_BYTES
            && !self.text.bytes().any(|byte| byte.is_ascii_control())
            && self.zone.as_ref().is_none_or(|zone| {
                !zone.is_empty()
                    && zone.len() <= MAX_PRODUCTION_CLOCK_ZONE_BYTES
                    && !zone.bytes().any(|byte| byte.is_ascii_control())
            })
    }
}

/// Encodes a status list into a diagnostics field.
///
/// # Errors
///
/// Returns [`CodecError::TooManyItems`] past the bound and
/// [`CodecError::InvalidField`] for an invalid or out-of-order status.
pub fn encode_statuses(statuses: &[ProductionClockStatus]) -> Result<String, CodecError> {
    if statuses.len() > MAX_PRODUCTION_CLOCK_STATUSES {
        return Err(CodecError::TooManyItems("production_clocks"));
    }
    let mut previous: Option<&str> = None;
    let entries = statuses
        .iter()
        .map(|status| {
            if !status.is_valid() || previous.is_some_and(|name| status.name.as_str() <= name) {
                return Err(invalid(&status.name));
            }
            previous = Some(status.name.as_str());
            Ok(format!(
                "{},{},{},{},{},{},{}",
                hex(&status.name),
                status.kind.token(),
                status.state.token(),
                hex(&status.text),
                status
                    .value_ms
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                u8::from(status.expired),
                status
                    .zone
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |zone| hex(zone)),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries.join(";"))
}

/// Decodes a status list from a diagnostics field.
///
/// # Errors
///
/// Returns [`CodecError::TooManyItems`] past the bound and
/// [`CodecError::InvalidField`] for anything but the exact contract.
pub fn decode_statuses(value: &str) -> Result<Vec<ProductionClockStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_PRODUCTION_CLOCK_STATUSES {
        return Err(CodecError::TooManyItems("production_clocks"));
    }
    let mut previous: Option<String> = None;
    entries
        .into_iter()
        .map(|entry| {
            let bad = || invalid(entry);
            let parts: Vec<&str> = entry.split(',').collect();
            let [name, kind, state, text, value_ms, expired, zone] = parts[..] else {
                return Err(bad());
            };
            let name = unhex(name).ok_or_else(bad)?;
            if previous.as_ref().is_some_and(|last| name <= *last) {
                return Err(bad());
            }
            previous = Some(name.clone());
            let text = unhex(text).ok_or_else(bad)?;
            let value_ms = match value_ms {
                "none" => None,
                value => Some(value.parse().map_err(|_| bad())?),
            };
            let expired = match expired {
                "0" => false,
                "1" => true,
                _ => return Err(bad()),
            };
            let zone = match zone {
                "none" => None,
                zone => Some(unhex(zone).ok_or_else(bad)?),
            };
            let status = ProductionClockStatus {
                name,
                kind: ProductionClockKind::from_token(kind).ok_or_else(bad)?,
                state: ProductionClockState::from_token(state).ok_or_else(bad)?,
                text,
                value_ms,
                expired,
                zone,
            };
            if status.is_valid() {
                Ok(status)
            } else {
                Err(bad())
            }
        })
        .collect()
}

fn invalid(value: &str) -> CodecError {
    CodecError::InvalidField {
        field: "production_clocks",
        value: value.to_owned(),
    }
}

fn hex(text: &str) -> String {
    use core::fmt::Write as _;
    let mut output = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn unhex(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for chunk in text.as_bytes().chunks(2) {
        let pair = core::str::from_utf8(chunk).ok()?;
        bytes.push(u8::from_str_radix(pair, 16).ok()?);
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &str) -> ProductionClockStatus {
        ProductionClockStatus {
            name: name.into(),
            kind: ProductionClockKind::Timer,
            state: ProductionClockState::Running,
            text: "00:59".into(),
            value_ms: Some(59_000),
            expired: false,
            zone: None,
        }
    }

    #[test]
    fn statuses_round_trip_and_are_strict() {
        let statuses = vec![
            status("Countdown"),
            ProductionClockStatus {
                name: "Local".into(),
                kind: ProductionClockKind::TimeOfDay,
                state: ProductionClockState::Live,
                text: "12:00:00".into(),
                value_ms: None,
                expired: false,
                zone: Some("UTC".into()),
            },
        ];
        let encoded = encode_statuses(&statuses).unwrap();
        assert_eq!(decode_statuses(&encoded).unwrap(), statuses);

        for malformed in [
            "Countdown,timer,running,00:59,59000,0,none".to_owned(),
            "436f756e74646f776e,timer,running,30303a3539,59000,2,none".to_owned(),
            "436f756e74646f776e,timer,running,30303a3539,59000,0".to_owned(),
        ] {
            assert!(decode_statuses(&malformed).is_err(), "{malformed}");
        }
        assert!(
            decode_statuses(&format!(
                "{};{}",
                encode_statuses(&[status("b")]).unwrap(),
                encode_statuses(&[status("a")]).unwrap()
            ))
            .is_err()
        );
    }
}

/// A timer operation a client sends to one production clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockControlOperation {
    Start,
    Pause,
    Reset,
    SetValue(i64),
    Adjust(i64),
    SetPreset(i64),
}

impl ClockControlOperation {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Pause => "pause",
            Self::Reset => "reset",
            Self::SetValue(_) => "set_value",
            Self::Adjust(_) => "adjust",
            Self::SetPreset(_) => "set_preset",
        }
    }

    #[must_use]
    pub const fn value_ns(self) -> Option<i64> {
        match self {
            Self::Start | Self::Pause | Self::Reset => None,
            Self::SetValue(value) | Self::Adjust(value) | Self::SetPreset(value) => Some(value),
        }
    }

    #[must_use]
    pub const fn operation(value: i64) -> Self {
        Self::SetValue(value)
    }

    #[must_use]
    pub fn from_parts(token: &str, value_ns: Option<i64>) -> Option<Self> {
        match (token, value_ns) {
            ("start", None) => Some(Self::Start),
            ("pause", None) => Some(Self::Pause),
            ("reset", None) => Some(Self::Reset),
            ("set_value", Some(value)) => Some(Self::SetValue(value)),
            ("adjust", Some(value)) => Some(Self::Adjust(value)),
            ("set_preset", Some(value)) => Some(Self::SetPreset(value)),
            _ => None,
        }
    }
}

/// A client request to control one production timer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockControlRequest {
    pub protocol: crate::ProtocolVersion,
    pub request_id: String,
    pub name: String,
    pub operation: ClockControlOperation,
}

/// The daemon's answer to one production-clock control request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockControlResponse {
    pub protocol: crate::ProtocolVersion,
    pub request_id: String,
    pub name: String,
    pub accepted: bool,
    /// The typed refusal, when `accepted` is false.
    pub error: Option<String>,
    /// The clock's reading after an accepted operation.
    pub reading: Option<ProductionClockStatus>,
}

/// Whether `name` is a valid production clock name.
#[must_use]
pub fn clock_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PRODUCTION_CLOCK_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
