//! `LiveLAN` codec (Protocol 2.57, OR-017).
//!
//! * `live_lan_set` carries `enabled` (`0|1`), `source` (`program` or
//!   `output:<id>`), `max_viewers` (`1..=32`), and an optional `pin`:
//!   absent keeps the stored PIN, empty clears it, and 4 to 12 ASCII digits
//!   replace it. `***` and every other value are refused, and a refused PIN
//!   is reported as `***`.
//! * `live_lan_changed` carries `enabled`, `source`, `max_viewers`, and
//!   `has_pin`.
//! * `live_lan_status` (runtime) carries `state`, `url`, `viewers`, and
//!   `egress_kbps`.
//! * A snapshot's `live_lan` is `<enabled>~<source>~<max_viewers>~<has_pin>`
//!   and its `live_lan_status` is `<state>~<viewers>~<egress_kbps>~<url>`,
//!   the URL last and empty when nothing is served. A URL is printable ASCII
//!   without spaces or commas, so it needs no escaping inside the list.

use crate::{
    CodecError, LiveLanEntry, LiveLanSource, LiveLanState, LiveLanStatus, MAX_LIVE_LAN_VIEWERS,
    MIN_LIVE_LAN_VIEWERS, SecretEdit, live_lan_pin_is_valid, live_lan_url_is_valid,
};

use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

/// What a refused PIN is reported as.
const REDACTED_PIN: &str = "***";

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_source(field: &'static str, value: &str) -> Result<LiveLanSource, CodecError> {
    value
        .parse::<LiveLanSource>()
        .map_err(|_| invalid(field, value))
}

fn check_viewers(field: &'static str, viewers: u8) -> Result<u8, CodecError> {
    if (MIN_LIVE_LAN_VIEWERS..=MAX_LIVE_LAN_VIEWERS).contains(&viewers) {
        Ok(viewers)
    } else {
        Err(invalid(field, viewers.to_string()))
    }
}

fn parse_number<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|value| T::try_from(value).ok())
        .ok_or_else(|| invalid(field, value))
}

fn parse_viewers(field: &'static str, value: &str) -> Result<u8, CodecError> {
    check_viewers(field, parse_number(field, value)?)
}

fn push_entry(entry: &LiveLanEntry, push: &mut Push<'_>) -> Result<(), CodecError> {
    push("enabled", u8::from(entry.enabled).to_string())?;
    push("source", entry.source.to_string())?;
    push(
        "max_viewers",
        check_viewers("max_viewers", entry.max_viewers)?.to_string(),
    )
}

/// Encodes `live_lan_set`'s fields after `payload`; the entry's `has_pin`
/// is not sent.
pub(super) fn encode_set(
    entry: &LiveLanEntry,
    pin: &SecretEdit,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push_entry(entry, push)?;
    match pin {
        SecretEdit::Keep => Ok(()),
        SecretEdit::Clear => push("pin", String::new()),
        SecretEdit::Set(pin) if pin == REDACTED_PIN || !live_lan_pin_is_valid(pin) => {
            Err(invalid("pin", REDACTED_PIN))
        }
        SecretEdit::Set(pin) => push("pin", pin.clone()),
    }
}

/// Decodes `live_lan_set`'s fields. The entry's `has_pin` is `false`.
pub(super) fn decode_set(
    fields: &mut dyn FieldSource,
) -> Result<(LiveLanEntry, SecretEdit), CodecError> {
    let entry = LiveLanEntry {
        enabled: parse_flag("enabled", &fields.required("enabled")?)?,
        source: parse_source("source", &fields.required("source")?)?,
        max_viewers: parse_viewers("max_viewers", &fields.required("max_viewers")?)?,
        has_pin: false,
    };
    let pin = match fields.optional("pin") {
        None => SecretEdit::Keep,
        Some(pin) if pin.is_empty() => SecretEdit::Clear,
        Some(pin) if pin == REDACTED_PIN || !live_lan_pin_is_valid(&pin) => {
            return Err(invalid("pin", REDACTED_PIN));
        }
        Some(pin) => SecretEdit::Set(pin),
    };
    Ok((entry, pin))
}

/// Encodes `live_lan_changed`'s fields after `event`.
pub(super) fn encode_changed(entry: &LiveLanEntry, push: &mut Push<'_>) -> Result<(), CodecError> {
    push_entry(entry, push)?;
    push("has_pin", u8::from(entry.has_pin).to_string())
}

/// Decodes `live_lan_changed`'s fields.
pub(super) fn decode_changed(fields: &mut dyn FieldSource) -> Result<LiveLanEntry, CodecError> {
    Ok(LiveLanEntry {
        enabled: parse_flag("enabled", &fields.required("enabled")?)?,
        source: parse_source("source", &fields.required("source")?)?,
        max_viewers: parse_viewers("max_viewers", &fields.required("max_viewers")?)?,
        has_pin: parse_flag("has_pin", &fields.required("has_pin")?)?,
    })
}

fn check_url(field: &'static str, url: &str) -> Result<(), CodecError> {
    if live_lan_url_is_valid(url) {
        Ok(())
    } else {
        Err(invalid(field, url))
    }
}

fn parse_state(field: &'static str, value: &str) -> Result<LiveLanState, CodecError> {
    LiveLanState::from_token(value).ok_or_else(|| invalid(field, value))
}

/// Encodes a `live_lan_status` runtime event's fields after `event`.
pub(super) fn encode_status(status: &LiveLanStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    check_url("url", &status.url)?;
    push("state", status.state.token().to_owned())?;
    push("url", status.url.clone())?;
    push("viewers", status.viewers.to_string())?;
    push("egress_kbps", status.egress_kbps.to_string())
}

/// Decodes a `live_lan_status` runtime event's fields.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<LiveLanStatus, CodecError> {
    let state = parse_state("state", &fields.required("state")?)?;
    let url = fields.required("url")?;
    check_url("url", &url)?;
    Ok(LiveLanStatus {
        state,
        url,
        viewers: parse_number("viewers", &fields.required("viewers")?)?,
        egress_kbps: parse_number("egress_kbps", &fields.required("egress_kbps")?)?,
    })
}

/// Encodes a snapshot's `live_lan`.
pub(super) fn encode_entry(entry: &LiveLanEntry) -> Result<String, CodecError> {
    Ok(format!(
        "{}~{}~{}~{}",
        u8::from(entry.enabled),
        entry.source,
        check_viewers("live_lan", entry.max_viewers)?,
        u8::from(entry.has_pin)
    ))
}

/// Decodes a snapshot's `live_lan`, the exact inverse of [`encode_entry`].
pub(super) fn decode_entry(value: &str) -> Result<LiveLanEntry, CodecError> {
    let parts: Vec<&str> = value.split('~').collect();
    let [enabled, source, max_viewers, has_pin] = parts.as_slice() else {
        return Err(invalid("live_lan", value));
    };
    Ok(LiveLanEntry {
        enabled: parse_flag("live_lan", enabled)?,
        source: parse_source("live_lan", source)?,
        max_viewers: parse_viewers("live_lan", max_viewers)?,
        has_pin: parse_flag("live_lan", has_pin)?,
    })
}

/// Encodes a snapshot's `live_lan_status`.
pub(super) fn encode_snapshot_status(status: &LiveLanStatus) -> Result<String, CodecError> {
    check_url("live_lan_status", &status.url)?;
    Ok(format!(
        "{}~{}~{}~{}",
        status.state.token(),
        status.viewers,
        status.egress_kbps,
        status.url
    ))
}

/// Decodes a snapshot's `live_lan_status`, the exact inverse of
/// [`encode_snapshot_status`].
pub(super) fn decode_snapshot_status(value: &str) -> Result<LiveLanStatus, CodecError> {
    let parts: Vec<&str> = value.split('~').collect();
    let [state, viewers, egress_kbps, url] = parts.as_slice() else {
        return Err(invalid("live_lan_status", value));
    };
    check_url("live_lan_status", url)?;
    Ok(LiveLanStatus {
        state: parse_state("live_lan_status", state)?,
        url: (*url).to_owned(),
        viewers: parse_number("live_lan_status", viewers)?,
        egress_kbps: parse_number("live_lan_status", egress_kbps)?,
    })
}
