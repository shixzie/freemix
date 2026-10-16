//! Stream destination codec (Protocol 2.52, OR-006).
//!
//! * A configuration is the fields `name`, `stream_protocol` (`rtmp`, `rtmps`,
//!   `srt`, `hls`), `endpoint`, optional `backup_endpoint`, `output`,
//!   `startup` (`stopped` or `reconcile`), `video_bitrate_kbps`, and
//!   `reconnect` (`<initial ms>:<max ms>:<attempts or none>`).
//! * `stream_target_set` carries `target`, a configuration, and an optional
//!   `stream_key`: absent keeps the stored key, empty clears it, and any
//!   other value replaces it. `***` is refused so a redacted echo can never
//!   be stored as a key.
//! * `stream_targets` (snapshot) is a comma-separated list of
//!   `<target>~<name>~<protocol>~<endpoint>~<backup>~<output>~<startup>~<kbps>~<reconnect>~<has_key 0|1>~<ladder rungs>`
//!   in authored order, with text percent-escaped and an empty backup for
//!   none.
//! * `stream_target_changed` carries `target`, `configured` (`0|1`), the
//!   configuration plus `has_key` and `ladder_rungs` when configured, and the
//!   resulting `streams` roster.
//!
//! No field ever carries a stored stream key, and a refused key is reported
//! as `***`.

use core::num::NonZeroU128;

use crate::display_output::invalid;
use crate::{
    CodecError, MAX_STREAM_TARGET_LADDER_RUNGS, MAX_STREAM_TARGETS, SecretEdit, StreamReconnect,
    StreamTargetConfig, StreamTargetEntry, StreamTargetProtocol, WireOutputId, WireStreamTargetId,
    stream_key_is_valid,
};

use super::media::{FieldSource, Push};
use super::value::{escape_bounded, parse_canonical_u64, unescape};

/// What a refused stream key is reported as.
const REDACTED_KEY: &str = "***";

fn parse_target(field: &'static str, value: &str) -> Result<WireStreamTargetId, CodecError> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(field, value));
    }
    value
        .parse::<u128>()
        .ok()
        .and_then(NonZeroU128::new)
        .map(WireStreamTargetId::new)
        .ok_or_else(|| invalid(field, value))
}

fn parse_output(field: &'static str, value: &str) -> Result<WireOutputId, CodecError> {
    let target = parse_target(field, value)?;
    Ok(WireOutputId::new(target.get()))
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid(field, value))
}

/// Encodes a reconnect policy as `<initial>:<max>:<attempts|none>`.
pub(super) fn encode_reconnect(reconnect: StreamReconnect) -> Result<String, CodecError> {
    let text = format!(
        "{}:{}:{}",
        reconnect.initial_delay_ms,
        reconnect.max_delay_ms,
        reconnect
            .max_attempts
            .map_or_else(|| "none".to_owned(), |attempts| attempts.to_string())
    );
    if reconnect.is_valid() {
        Ok(text)
    } else {
        Err(invalid("reconnect", text))
    }
}

/// Decodes a canonical `<initial>:<max>:<attempts|none>` policy.
pub(super) fn parse_reconnect(value: &str) -> Result<StreamReconnect, CodecError> {
    let error = || invalid("reconnect", value);
    let mut parts = value.split(':');
    let (Some(initial), Some(max), Some(attempts), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(error());
    };
    let reconnect = StreamReconnect {
        initial_delay_ms: parse_u32("reconnect", initial).map_err(|_| error())?,
        max_delay_ms: parse_u32("reconnect", max).map_err(|_| error())?,
        max_attempts: if attempts == "none" {
            None
        } else {
            Some(parse_u32("reconnect", attempts).map_err(|_| error())?)
        },
    };
    if reconnect.is_valid() {
        Ok(reconnect)
    } else {
        Err(error())
    }
}

fn startup_token(reconcile: bool) -> &'static str {
    if reconcile { "reconcile" } else { "stopped" }
}

fn parse_startup(value: &str) -> Result<bool, CodecError> {
    match value {
        "reconcile" => Ok(true),
        "stopped" => Ok(false),
        _ => Err(invalid("startup", value)),
    }
}

fn check_config(config: &StreamTargetConfig) -> Result<(), CodecError> {
    if config.is_valid() {
        Ok(())
    } else {
        Err(invalid("stream_target", config.name.clone()))
    }
}

/// Encodes a configuration's fields.
pub(super) fn encode_config(
    config: &StreamTargetConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_config(config)?;
    push("name", config.name.clone())?;
    push("stream_protocol", config.protocol.token().to_owned())?;
    push("endpoint", config.endpoint.clone())?;
    if let Some(backup) = &config.backup_endpoint {
        push("backup_endpoint", backup.clone())?;
    }
    push("output", config.output.to_string())?;
    push(
        "startup",
        startup_token(config.reconcile_on_startup).to_owned(),
    )?;
    push("video_bitrate_kbps", config.video_bitrate_kbps.to_string())?;
    push("reconnect", encode_reconnect(config.reconnect)?)
}

/// Decodes a configuration's fields.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<StreamTargetConfig, CodecError> {
    let protocol_value = fields.required("stream_protocol")?;
    let config = StreamTargetConfig {
        name: fields.required("name")?,
        protocol: StreamTargetProtocol::from_token(&protocol_value)
            .ok_or_else(|| invalid("stream_protocol", protocol_value))?,
        endpoint: fields.required("endpoint")?,
        backup_endpoint: fields.optional("backup_endpoint"),
        output: parse_output("output", &fields.required("output")?)?,
        reconcile_on_startup: parse_startup(&fields.required("startup")?)?,
        video_bitrate_kbps: parse_u32(
            "video_bitrate_kbps",
            &fields.required("video_bitrate_kbps")?,
        )?,
        reconnect: parse_reconnect(&fields.required("reconnect")?)?,
    };
    check_config(&config)?;
    Ok(config)
}

/// Encodes `stream_target_set`'s fields after `payload`.
pub(super) fn encode_set(
    target: WireStreamTargetId,
    config: &StreamTargetConfig,
    stream_key: &SecretEdit,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("target", target.to_string())?;
    encode_config(config, push)?;
    match stream_key {
        SecretEdit::Keep => Ok(()),
        SecretEdit::Clear => push("stream_key", String::new()),
        SecretEdit::Set(key) if key == REDACTED_KEY || !stream_key_is_valid(key) => {
            Err(invalid("stream_key", REDACTED_KEY))
        }
        SecretEdit::Set(key) => push("stream_key", key.clone()),
    }
}

/// Decodes `stream_target_set`'s fields.
pub(super) fn decode_set(
    fields: &mut dyn FieldSource,
) -> Result<(WireStreamTargetId, StreamTargetConfig, SecretEdit), CodecError> {
    let target = decode_target(fields)?;
    let config = decode_config(fields)?;
    let stream_key = match fields.optional("stream_key") {
        None => SecretEdit::Keep,
        Some(key) if key.is_empty() => SecretEdit::Clear,
        Some(key) if key == REDACTED_KEY || !stream_key_is_valid(&key) => {
            return Err(invalid("stream_key", REDACTED_KEY));
        }
        Some(key) => SecretEdit::Set(key),
    };
    Ok((target, config, stream_key))
}

/// Decodes a `target` field.
pub(super) fn decode_target(
    fields: &mut dyn FieldSource,
) -> Result<WireStreamTargetId, CodecError> {
    parse_target("target", &fields.required("target")?)
}

fn check_entry(entry: &StreamTargetEntry) -> Result<(), CodecError> {
    check_config(&entry.config)?;
    if entry.has_key != entry.config.protocol.carries_key() {
        return Err(invalid("has_key", u8::from(entry.has_key).to_string()));
    }
    if entry.ladder_rungs > MAX_STREAM_TARGET_LADDER_RUNGS
        || (entry.ladder_rungs > 0 && entry.config.protocol != StreamTargetProtocol::Hls)
    {
        return Err(invalid("ladder_rungs", entry.ladder_rungs.to_string()));
    }
    Ok(())
}

/// Encodes `stream_target_changed`'s target and entry fields.
pub(super) fn encode_changed(
    target: WireStreamTargetId,
    entry: Option<&StreamTargetEntry>,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("target", target.to_string())?;
    push("configured", u8::from(entry.is_some()).to_string())?;
    let Some(entry) = entry else {
        return Ok(());
    };
    if entry.target != target {
        return Err(invalid("target", entry.target.to_string()));
    }
    check_entry(entry)?;
    encode_config(&entry.config, push)?;
    push("has_key", u8::from(entry.has_key).to_string())?;
    push("ladder_rungs", entry.ladder_rungs.to_string())
}

/// Decodes `stream_target_changed`'s target and entry fields.
pub(super) fn decode_changed(
    fields: &mut dyn FieldSource,
) -> Result<(WireStreamTargetId, Option<StreamTargetEntry>), CodecError> {
    let target = decode_target(fields)?;
    if !parse_flag("configured", &fields.required("configured")?)? {
        return Ok((target, None));
    }
    let config = decode_config(fields)?;
    let entry = StreamTargetEntry {
        target,
        config,
        has_key: parse_flag("has_key", &fields.required("has_key")?)?,
        ladder_rungs: u8::try_from(parse_u32(
            "ladder_rungs",
            &fields.required("ladder_rungs")?,
        )?)
        .map_err(|_| invalid("ladder_rungs", "out of range"))?,
    };
    check_entry(&entry)?;
    Ok((target, Some(entry)))
}

/// Encodes a snapshot's `stream_targets`.
pub(super) fn encode_entries(entries: &[StreamTargetEntry]) -> Result<String, CodecError> {
    if entries.len() > MAX_STREAM_TARGETS {
        return Err(CodecError::TooManyItems("stream_targets"));
    }
    let mut seen = Vec::with_capacity(entries.len());
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        if seen.contains(&entry.target) {
            return Err(invalid("stream_targets", entry.target.to_string()));
        }
        seen.push(entry.target);
        check_entry(entry)?;
        let config = &entry.config;
        items.push(format!(
            "{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}",
            entry.target,
            escape_bounded(&config.name)?,
            config.protocol.token(),
            escape_bounded(&config.endpoint)?,
            match &config.backup_endpoint {
                Some(backup) => escape_bounded(backup)?,
                None => String::new(),
            },
            config.output,
            startup_token(config.reconcile_on_startup),
            config.video_bitrate_kbps,
            encode_reconnect(config.reconnect)?,
            u8::from(entry.has_key),
            entry.ladder_rungs,
        ));
    }
    Ok(items.join(","))
}

/// Decodes a snapshot's `stream_targets`.
pub(super) fn decode_entries(value: &str) -> Result<Vec<StreamTargetEntry>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let items = value.split(',').collect::<Vec<_>>();
    if items.len() > MAX_STREAM_TARGETS {
        return Err(CodecError::TooManyItems("stream_targets"));
    }
    let mut entries: Vec<StreamTargetEntry> = Vec::with_capacity(items.len());
    for item in items {
        let error = || invalid("stream_targets", item);
        let parts = item.split('~').collect::<Vec<_>>();
        let [
            target,
            name,
            protocol,
            endpoint,
            backup,
            output,
            startup,
            kbps,
            reconnect,
            has_key,
            ladder,
        ] = parts.as_slice()
        else {
            return Err(error());
        };
        let entry = StreamTargetEntry {
            target: parse_target("stream_targets", target)?,
            config: StreamTargetConfig {
                name: unescape(name)?,
                protocol: StreamTargetProtocol::from_token(protocol).ok_or_else(error)?,
                endpoint: unescape(endpoint)?,
                backup_endpoint: if backup.is_empty() {
                    None
                } else {
                    Some(unescape(backup)?)
                },
                output: parse_output("stream_targets", output)?,
                reconcile_on_startup: parse_startup(startup)?,
                video_bitrate_kbps: parse_u32("stream_targets", kbps)?,
                reconnect: parse_reconnect(reconnect)?,
            },
            has_key: parse_flag("stream_targets", has_key)?,
            ladder_rungs: u8::try_from(parse_u32("stream_targets", ladder)?)
                .map_err(|_| error())?,
        };
        if entries
            .iter()
            .any(|existing| existing.target == entry.target)
        {
            return Err(error());
        }
        check_entry(&entry)?;
        entries.push(entry);
    }
    Ok(entries)
}
