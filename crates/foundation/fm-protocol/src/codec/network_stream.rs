//! Network stream input codec (Protocol 2.46, IN-015; 2.49, IN-016/IN-017).
//!
//! * Config fields: `url`, `transport`, `jitter_ms`, `backoff_min_ms`,
//!   `backoff_max_ms`, `give_up` (`never` or a canonical count), `timeout_ms`,
//!   `stall`, `mode` (`caller|listener|rendezvous`), `latency_ms`,
//!   `key_length` (`16|24|32`), `audio` (`mix|1-2|3-4|5-6|7-8`), and the
//!   optional `stream_id`, `stream_key`, and `passphrase`. Each config is
//!   validated on both sides. A command carries the secrets as typed; a
//!   report (an event) carries `***` for each secret that is set.
//! * `input_stream_set` carries the optional `url` (absent keeps it), the
//!   settings, the optional `stream_id`, and `stream_key` and `passphrase`
//!   edits: absent keeps the stored secret, empty clears it, and a value
//!   replaces it.
//! * Status fields: `state`, `attempt` and `next_in_ms` (only for
//!   `reconnecting`), `bitrate_bps`, `buffer_ms`, `dropped_frames`,
//!   `late_frames`, `reconnects`, `source_channels`, and an optional
//!   `last_error`.
//! * A snapshot's `network_streams` is a comma-separated, strictly ascending
//!   list of `<input>~<transport>~<jitter_ms>~<backoff_min_ms>~
//!   <backoff_max_ms>~<give_up>~<timeout_ms>~<stall>~<mode>~<latency_ms>~
//!   <key_length>~<audio>~<key_set>~<passphrase_set>~<state>~<attempt>~
//!   <next_in_ms>~<bitrate_bps>~<buffer_ms>~<dropped>~<late>~<reconnects>~
//!   <source_channels>~<length>:<stream_id><length>:<url>`: the stream ID
//!   (empty for none) and the redacted URL are written raw after their
//!   canonical byte lengths, so the line encoding escapes them exactly once;
//!   `key_set` and `passphrase_set` are `0` or `1`.

use core::fmt::Write as _;

use fm_types::network_stream::{
    AudioChannels, GiveUp, MAX_NETWORK_STREAM_INPUTS, MAX_SRT_STREAM_ID_BYTES,
    MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES, NetworkStreamConfig, NetworkStreamSettings,
    NetworkStreamState, NetworkStreamStatus, REDACTED, ReconnectPolicy, RtspTransport, SecretEdit,
    SrtKeyLength, StallSlate, StreamMode, StreamUrl,
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

fn mode(field: &'static str, value: &str) -> Result<StreamMode, CodecError> {
    StreamMode::from_token(value).ok_or_else(|| invalid(field, value))
}

fn key_length(field: &'static str, value: &str) -> Result<SrtKeyLength, CodecError> {
    SrtKeyLength::from_token(value).ok_or_else(|| invalid(field, value))
}

fn audio(field: &'static str, value: &str) -> Result<AudioChannels, CodecError> {
    AudioChannels::from_token(value).ok_or_else(|| invalid(field, value))
}

/// Whether a config travels with its secrets (a command) or masked (a
/// report).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Secrets {
    Exact,
    Redacted,
}

fn check_config(config: &NetworkStreamConfig, secrets: Secrets) -> Result<(), CodecError> {
    match secrets {
        Secrets::Exact => config.validate(),
        Secrets::Redacted => config.validate_redacted(),
    }
    .map(|_| ())
    .map_err(|error| invalid(error.field(), error.code()))
}

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
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
    push("stall", settings.stall.token().to_owned())?;
    push("mode", settings.mode.token().to_owned())?;
    push("latency_ms", settings.latency_ms.to_string())?;
    push("key_length", settings.key_length.token().to_owned())?;
    push("audio", settings.audio.token().to_owned())
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
        mode: mode("mode", &fields.required("mode")?)?,
        latency_ms: canonical("latency_ms", &fields.required("latency_ms")?)?,
        key_length: key_length("key_length", &fields.required("key_length")?)?,
        audio: audio("audio", &fields.required("audio")?)?,
    };
    check_settings(&settings)?;
    Ok(settings)
}

/// Encodes a whole config: `url`, the settings fields, and the optional
/// `stream_id`, `stream_key`, and `passphrase`.
pub(super) fn encode_config(
    config: &NetworkStreamConfig,
    secrets: Secrets,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_config(config, secrets)?;
    push("url", config.url.clone())?;
    encode_settings(&config.settings, push)?;
    for (name, value) in [
        ("stream_id", &config.stream_id),
        ("stream_key", &config.stream_key),
        ("passphrase", &config.passphrase),
    ] {
        if let Some(value) = value {
            push(name, value.clone())?;
        }
    }
    Ok(())
}

/// Decodes a whole config.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
    secrets: Secrets,
) -> Result<NetworkStreamConfig, CodecError> {
    let url = fields.required("url")?;
    let config = NetworkStreamConfig {
        url,
        settings: decode_settings(fields)?,
        stream_id: fields.optional("stream_id"),
        stream_key: fields.optional("stream_key"),
        passphrase: fields.optional("passphrase"),
    };
    check_config(&config, secrets)?;
    Ok(config)
}

/// The fields of `input_stream_set` besides its input: the new URL (if
/// any), the settings, the stream ID, and the secret edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SetFields {
    pub url: Option<StreamUrl>,
    pub settings: NetworkStreamSettings,
    pub stream_id: Option<String>,
    pub stream_key: SecretEdit,
    pub passphrase: SecretEdit,
}

fn encode_secret(
    name: &'static str,
    edit: &SecretEdit,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match edit {
        SecretEdit::Keep => Ok(()),
        SecretEdit::Clear => push(name, String::new()),
        SecretEdit::Set(secret) if secret.is_empty() || secret == REDACTED => {
            Err(invalid(name, REDACTED))
        }
        SecretEdit::Set(secret) => push(name, secret.clone()),
    }
}

fn decode_secret(
    name: &'static str,
    fields: &mut dyn FieldSource,
) -> Result<SecretEdit, CodecError> {
    Ok(match fields.optional(name) {
        None => SecretEdit::Keep,
        Some(secret) if secret.is_empty() => SecretEdit::Clear,
        Some(secret) if secret == REDACTED => return Err(invalid(name, REDACTED)),
        Some(secret) => SecretEdit::Set(secret),
    })
}

fn check_stream_id(stream_id: Option<&str>) -> Result<(), CodecError> {
    match stream_id {
        Some(id) if id.is_empty() || id.len() > MAX_SRT_STREAM_ID_BYTES => {
            Err(invalid("stream_id", id))
        }
        _ => Ok(()),
    }
}

/// Encodes `input_stream_set`'s fields after `payload`.
pub(super) fn encode_set(
    input: WireInputId,
    set: &SetFields,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("input", input.to_string())?;
    if let Some(url) = &set.url {
        fm_types::network_stream::validate_stream_url(url.as_str())
            .map_err(|error| invalid("url", error.code()))?;
        push("url", url.as_str().to_owned())?;
    }
    encode_settings(&set.settings, push)?;
    check_stream_id(set.stream_id.as_deref())?;
    if let Some(stream_id) = &set.stream_id {
        push("stream_id", stream_id.clone())?;
    }
    encode_secret("stream_key", &set.stream_key, push)?;
    encode_secret("passphrase", &set.passphrase, push)
}

/// Decodes `input_stream_set`'s fields after its input.
pub(super) fn decode_set(fields: &mut dyn FieldSource) -> Result<SetFields, CodecError> {
    let url = fields.optional("url");
    if let Some(url) = &url {
        fm_types::network_stream::validate_stream_url(url)
            .map_err(|error| invalid("url", error.code()))?;
    }
    let settings = decode_settings(fields)?;
    let stream_id = fields.optional("stream_id");
    check_stream_id(stream_id.as_deref())?;
    Ok(SetFields {
        url: url.map(StreamUrl::new),
        settings,
        stream_id,
        stream_key: decode_secret("stream_key", fields)?,
        passphrase: decode_secret("passphrase", fields)?,
    })
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
        "listening" => NetworkStreamState::Listening,
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
    push("source_channels", status.source_channels.to_string())?;
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
        source_channels: canonical("source_channels", &fields.required("source_channels")?)?,
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
        check_config(&entry.config, Secrets::Redacted)?;
        let settings = &entry.config.settings;
        let status = &entry.status;
        let (state, attempt, next_in_ms) = state_parts(status.state);
        if state == "reconnecting" && attempt == 0 {
            return Err(invalid(FIELD, "reconnecting attempt 0"));
        }
        if !encoded.is_empty() {
            encoded.push(',');
        }
        let stream_id = entry.config.stream_id.as_deref().unwrap_or_default();
        write!(
            encoded,
            "{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}~{}:{}{}:{}",
            entry.input,
            settings.transport.token(),
            settings.jitter_buffer_ms,
            settings.reconnect.backoff_min_ms,
            settings.reconnect.backoff_max_ms,
            settings.reconnect.give_up.token(),
            settings.timeout_ms,
            settings.stall.token(),
            settings.mode.token(),
            settings.latency_ms,
            settings.key_length.token(),
            settings.audio.token(),
            u8::from(entry.config.stream_key.is_some()),
            u8::from(entry.config.passphrase.is_some()),
            state,
            attempt,
            next_in_ms,
            status.bitrate_bps,
            status.buffer_ms,
            status.dropped_frames,
            status.late_frames,
            status.reconnects,
            status.source_channels,
            stream_id.len(),
            stream_id,
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
        let (parts, stream_id, url, after) =
            split_entry(rest).ok_or_else(|| invalid(FIELD, rest))?;
        let [
            input,
            transport_token,
            jitter,
            backoff_min,
            backoff_max,
            give_up_token,
            timeout,
            stall_token,
            mode_token,
            latency,
            key_length_token,
            audio_token,
            key_set,
            passphrase_set,
            state,
            attempt,
            next_in_ms,
            bitrate,
            buffer,
            dropped,
            late,
            reconnects,
            source_channels,
        ] = parts.as_slice()
        else {
            return Err(invalid(FIELD, rest));
        };
        let masked = |set: bool| set.then(|| REDACTED.to_owned());
        let input = parse_canonical_input(input).ok_or_else(|| invalid(FIELD, rest))?;
        if entries
            .last()
            .is_some_and(|previous| previous.input.get() >= input.get())
        {
            return Err(invalid(FIELD, rest));
        }
        let config = NetworkStreamConfig {
            url: url.to_owned(),
            stream_id: (!stream_id.is_empty()).then(|| stream_id.to_owned()),
            stream_key: masked(flag(FIELD, key_set)?),
            passphrase: masked(flag(FIELD, passphrase_set)?),
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
                mode: mode(FIELD, mode_token)?,
                latency_ms: canonical(FIELD, latency)?,
                key_length: key_length(FIELD, key_length_token)?,
                audio: audio(FIELD, audio_token)?,
            },
        };
        check_config(&config, Secrets::Redacted)?;
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
            source_channels: canonical(FIELD, source_channels)?,
            last_error: None,
        };
        rest = after;
        entries.push(NetworkStreamEntry {
            input,
            config,
            status,
        });
    }
    Ok(entries)
}

/// Splits one snapshot entry off the front of `text`: its 23 `~`-ended
/// parts, its stream ID, its URL, and what follows the entry.
fn split_entry(text: &str) -> Option<(Vec<&str>, &str, &str, &str)> {
    let mut parts = Vec::with_capacity(23);
    let mut after = text;
    for _ in 0..23 {
        let (part, tail) = after.split_once('~')?;
        parts.push(part);
        after = tail;
    }
    let (stream_id, after) = length_prefixed(after, MAX_SRT_STREAM_ID_BYTES)?;
    let (url, after) = length_prefixed(after, MAX_STREAM_URL_BYTES)?;
    Some((parts, stream_id, url, after))
}

/// Splits `<length>:<value>` off the front of `text`, the value at most
/// `max` bytes, returning the value and what follows it.
fn length_prefixed(text: &str, max: usize) -> Option<(&str, &str)> {
    let (length, after) = text.split_once(':')?;
    let length = parse_canonical_u64(length)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= max)?;
    let value = after.get(..length)?;
    Some((value, &after[length..]))
}
