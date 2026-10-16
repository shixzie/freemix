//! Network stream input codec (Protocol 2.46, IN-015).
//!
//! * Config fields: `url`, `transport`, `jitter_ms`, `backoff_min_ms`,
//!   `backoff_max_ms`, `give_up` (`never` or a canonical count), `timeout_ms`,
//!   `stall`. Each config is validated on both sides.
//! * Status fields: `state`, `attempt` and `next_in_ms` (only for
//!   `reconnecting`), `bitrate_bps`, `buffer_ms`, `dropped_frames`,
//!   `late_frames`, `reconnects`, and an optional `last_error`.
//! * A snapshot's `network_streams` is a comma-separated, strictly ascending
//!   list of `<input>~<transport>~<jitter_ms>~<backoff_min_ms>~
//!   <backoff_max_ms>~<give_up>~<timeout_ms>~<stall>~<state>~<attempt>~
//!   <next_in_ms>~<bitrate_bps>~<buffer_ms>~<dropped>~<late>~<reconnects>~
//!   <length>:<url>`: the redacted URL is written raw after its canonical
//!   byte length, so the line encoding escapes it exactly once.

use core::fmt::Write as _;

use fm_types::network_stream::{
    GiveUp, MAX_NETWORK_STREAM_INPUTS, MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES,
    NetworkStreamConfig, NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus,
    ReconnectPolicy, RtspTransport, StallSlate, StreamUrl,
};

use crate::{CodecError, NetworkStreamEntry, WireInputId};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn canonical<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| T::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

fn transport(field: &'static str, value: &str) -> Result<RtspTransport, CodecError> {
    RtspTransport::from_token(value).ok_or_else(|| invalid(field, value))
}

fn stall(field: &'static str, value: &str) -> Result<StallSlate, CodecError> {
    StallSlate::from_token(value).ok_or_else(|| invalid(field, value))
}

fn give_up(field: &'static str, value: &str) -> Result<GiveUp, CodecError> {
    GiveUp::from_token(value).ok_or_else(|| invalid(field, value))
}

fn check_config(config: &NetworkStreamConfig) -> Result<(), CodecError> {
    config
        .validate()
        .map(|_| ())
        .map_err(|error| invalid(error.field(), error.code()))
}

fn check_settings(settings: &NetworkStreamSettings) -> Result<(), CodecError> {
    settings
        .validate()
        .map_err(|error| invalid(error.field(), error.code()))
}

/// Encodes the settings fields.
pub(super) fn encode_settings(
    settings: &NetworkStreamSettings,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_settings(settings)?;
    push("transport", settings.transport.token().to_owned())?;
    push("jitter_ms", settings.jitter_buffer_ms.to_string())?;
    push(
        "backoff_min_ms",
        settings.reconnect.backoff_min_ms.to_string(),
    )?;
    push(
        "backoff_max_ms",
        settings.reconnect.backoff_max_ms.to_string(),
    )?;
    push("give_up", settings.reconnect.give_up.token())?;
    push("timeout_ms", settings.timeout_ms.to_string())?;
    push("stall", settings.stall.token().to_owned())
}

/// Decodes the settings fields.
pub(super) fn decode_settings(
    fields: &mut dyn FieldSource,
) -> Result<NetworkStreamSettings, CodecError> {
    let settings = NetworkStreamSettings {
        transport: transport("transport", &fields.required("transport")?)?,
        jitter_buffer_ms: canonical("jitter_ms", &fields.required("jitter_ms")?)?,
        reconnect: ReconnectPolicy {
            backoff_min_ms: canonical("backoff_min_ms", &fields.required("backoff_min_ms")?)?,
            backoff_max_ms: canonical("backoff_max_ms", &fields.required("backoff_max_ms")?)?,
            give_up: give_up("give_up", &fields.required("give_up")?)?,
        },
        timeout_ms: canonical("timeout_ms", &fields.required("timeout_ms")?)?,
        stall: stall("stall", &fields.required("stall")?)?,
    };
    check_settings(&settings)?;
    Ok(settings)
}

/// Encodes a whole config: `url` and the settings fields.
pub(super) fn encode_config(
    config: &NetworkStreamConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_config(config)?;
    push("url", config.url.clone())?;
    encode_settings(&config.settings, push)
}

/// Decodes a whole config.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<NetworkStreamConfig, CodecError> {
    let url = fields.required("url")?;
    let config = NetworkStreamConfig {
        url,
        settings: decode_settings(fields)?,
    };
    check_config(&config)?;
    Ok(config)
}

/// Encodes `input_stream_set`'s fields after `payload`.
pub(super) fn encode_set(
    input: WireInputId,
    url: Option<&StreamUrl>,
    settings: &NetworkStreamSettings,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("input", input.to_string())?;
    if let Some(url) = url {
        fm_types::network_stream::validate_stream_url(url.as_str())
            .map_err(|error| invalid("url", error.code()))?;
        push("url", url.as_str().to_owned())?;
    }
    encode_settings(settings, push)
}

/// Decodes `input_stream_set`'s `url` (if present) and settings.
pub(super) fn decode_set(
    fields: &mut dyn FieldSource,
) -> Result<(Option<StreamUrl>, NetworkStreamSettings), CodecError> {
    let url = fields.optional("url");
    if let Some(url) = &url {
        fm_types::network_stream::validate_stream_url(url)
            .map_err(|error| invalid("url", error.code()))?;
    }
    Ok((url.map(StreamUrl::new), decode_settings(fields)?))
}

fn state_parts(state: NetworkStreamState) -> (&'static str, u32, u32) {
    match state {
        NetworkStreamState::Reconnecting {
            attempt,
            next_in_ms,
        } => ("reconnecting", attempt, next_in_ms),
        other => (other.token(), 0, 0),
    }
}

fn parse_state(
    field: &'static str,
    token: &str,
    attempt: u32,
    next_in_ms: u32,
) -> Result<NetworkStreamState, CodecError> {
    let state = match token {
        "idle" => NetworkStreamState::Idle,
        "connecting" => NetworkStreamState::Connecting,
        "buffering" => NetworkStreamState::Buffering,
        "live" => NetworkStreamState::Live,
        "stalled" => NetworkStreamState::Stalled,
        "failed" => NetworkStreamState::Failed,
        "reconnecting" if attempt > 0 => {
            return Ok(NetworkStreamState::Reconnecting {
                attempt,
                next_in_ms,
            });
        }
        _ => return Err(invalid(field, token)),
    };
    if attempt != 0 || next_in_ms != 0 {
        return Err(invalid(field, token));
    }
    Ok(state)
}

fn check_error(error: Option<&str>) -> Result<(), CodecError> {
    match error {
        Some(error)
            if error.is_empty()
                || error.len() > MAX_STREAM_ERROR_BYTES
                || error.chars().any(char::is_control) =>
        {
            Err(invalid("last_error", error))
        }
        _ => Ok(()),
    }
}

/// Encodes a `network_stream_status` runtime event's fields after `event`.
pub(super) fn encode_status(
    input: WireInputId,
    status: &NetworkStreamStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_error(status.last_error.as_deref())?;
    let (state, attempt, next_in_ms) = state_parts(status.state);
    push("input", input.to_string())?;
    push("state", state.to_owned())?;
    if state == "reconnecting" {
        if attempt == 0 {
            return Err(invalid("attempt", "0"));
        }
        push("attempt", attempt.to_string())?;
        push("next_in_ms", next_in_ms.to_string())?;
    }
    push("bitrate_bps", status.bitrate_bps.to_string())?;
    push("buffer_ms", status.buffer_ms.to_string())?;
    push("dropped_frames", status.dropped_frames.to_string())?;
    push("late_frames", status.late_frames.to_string())?;
    push("reconnects", status.reconnects.to_string())?;
    if let Some(error) = &status.last_error {
        push("last_error", error.clone())?;
    }
    Ok(())
}

/// Decodes a `network_stream_status` runtime event's fields.
pub(super) fn decode_status(
    fields: &mut dyn FieldSource,
) -> Result<(WireInputId, NetworkStreamStatus), CodecError> {
    let input_text = fields.required("input")?;
    let input = parse_canonical_input(&input_text).ok_or_else(|| invalid("input", input_text))?;
    let state = fields.required("state")?;
    let (attempt, next_in_ms) = if state == "reconnecting" {
        (
            canonical("attempt", &fields.required("attempt")?)?,
            canonical("next_in_ms", &fields.required("next_in_ms")?)?,
        )
    } else {
        (0, 0)
    };
    let status = NetworkStreamStatus {
        state: parse_state("state", &state, attempt, next_in_ms)?,
        bitrate_bps: canonical("bitrate_bps", &fields.required("bitrate_bps")?)?,
        buffer_ms: canonical("buffer_ms", &fields.required("buffer_ms")?)?,
        dropped_frames: canonical("dropped_frames", &fields.required("dropped_frames")?)?,
        late_frames: canonical("late_frames", &fields.required("late_frames")?)?,
        reconnects: canonical("reconnects", &fields.required("reconnects")?)?,
        last_error: fields.optional("last_error"),
    };
    check_error(status.last_error.as_deref())?;
    Ok((input, status))
}

/// Encodes a snapshot's `network_streams`.
pub(super) fn encode_entries(entries: &[NetworkStreamEntry]) -> Result<String, CodecError> {
    const FIELD: &str = "network_streams";
    if entries.len() > MAX_NETWORK_STREAM_INPUTS {
        return Err(CodecError::TooManyItems(FIELD));
    }
    let mut previous = None;
    let mut encoded = String::new();
    for entry in entries {
        if previous.is_some_and(|previous| previous >= entry.input.get()) {
            return Err(invalid(FIELD, entry.input.to_string()));
        }
        previous = Some(entry.input.get());
        check_config(&entry.config)?;
        let settings = &entry.config.settings;
        let status = &entry.status;
        let (state, attempt, next_in_ms) = state_parts(status.state);
        if state == "reconnecting" && attempt == 0 {
            return Err(invalid(FIELD, "reconnecting attempt 0"));
        }
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}:{}",
            entry.input,
            settings.transport.token(),
            settings.jitter_buffer_ms,
            settings.reconnect.backoff_min_ms,
            settings.reconnect.backoff_max_ms,
            settings.reconnect.give_up.token(),
            settings.timeout_ms,
            settings.stall.token(),
            state,
            attempt,
            next_in_ms,
            status.bitrate_bps,
            status.buffer_ms,
            status.dropped_frames,
            status.late_frames,
            status.reconnects,
            entry.config.url.len(),
            entry.config.url,
        )
        .expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Decodes a snapshot's `network_streams`, the exact inverse of
/// [`encode_entries`].
pub(super) fn decode_entries(value: &str) -> Result<Vec<NetworkStreamEntry>, CodecError> {
    const FIELD: &str = "network_streams";
    let mut entries: Vec<NetworkStreamEntry> = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if !entries.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(FIELD, value))?;
        }
        if entries.len() == MAX_NETWORK_STREAM_INPUTS {
            return Err(CodecError::TooManyItems(FIELD));
        }
        let mut parts = Vec::with_capacity(16);
        let mut after = rest;
        for _ in 0..16 {
            let (part, tail) = after.split_once('~').ok_or_else(|| invalid(FIELD, rest))?;
            parts.push(part);
            after = tail;
        }
        let (length, after) = after.split_once(':').ok_or_else(|| invalid(FIELD, rest))?;
        let url = parse_canonical_u64(length)
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length <= MAX_STREAM_URL_BYTES)
            .and_then(|length| after.get(..length))
            .ok_or_else(|| invalid(FIELD, rest))?;
        let [
            input,
            transport_token,
            jitter,
            backoff_min,
            backoff_max,
            give_up_token,
            timeout,
            stall_token,
            state,
            attempt,
            next_in_ms,
            bitrate,
            buffer,
            dropped,
            late,
            reconnects,
        ] = parts.as_slice()
        else {
            return Err(invalid(FIELD, rest));
        };
        let input = parse_canonical_input(input).ok_or_else(|| invalid(FIELD, rest))?;
        if entries
            .last()
            .is_some_and(|previous| previous.input.get() >= input.get())
        {
            return Err(invalid(FIELD, rest));
        }
        let config = NetworkStreamConfig {
            url: url.to_owned(),
            settings: NetworkStreamSettings {
                transport: transport(FIELD, transport_token)?,
                jitter_buffer_ms: canonical(FIELD, jitter)?,
                reconnect: ReconnectPolicy {
                    backoff_min_ms: canonical(FIELD, backoff_min)?,
                    backoff_max_ms: canonical(FIELD, backoff_max)?,
                    give_up: give_up(FIELD, give_up_token)?,
                },
                timeout_ms: canonical(FIELD, timeout)?,
                stall: stall(FIELD, stall_token)?,
            },
        };
        check_config(&config)?;
        let status = NetworkStreamStatus {
            state: parse_state(
                FIELD,
                state,
                canonical(FIELD, attempt)?,
                canonical(FIELD, next_in_ms)?,
            )?,
            bitrate_bps: canonical(FIELD, bitrate)?,
            buffer_ms: canonical(FIELD, buffer)?,
            dropped_frames: canonical(FIELD, dropped)?,
            late_frames: canonical(FIELD, late)?,
            reconnects: canonical(FIELD, reconnects)?,
            last_error: None,
        };
        rest = &after[url.len()..];
        entries.push(NetworkStreamEntry {
            input,
            config,
            status,
        });
    }
    Ok(entries)
}
