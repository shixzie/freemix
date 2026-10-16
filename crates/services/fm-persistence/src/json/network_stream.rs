//! A network stream input in `project.json` (schema 46, IN-015, IN-016,
//! IN-017).
//!
//! ```text
//! {"type": "network_stream", "url": "rtsp://user:pass@cam/stream",
//!  "transport": "auto|tcp|udp", "jitter_ms": 300,
//!  "reconnect": {"backoff_min_ms": 500, "backoff_max_ms": 10000, "give_up": "never" | 5},
//!  "timeout_ms": 5000, "stall": "freeze|black",
//!  "mode": "caller|listener|rendezvous", "latency_ms": 120, "key_length": 16|24|32,
//!  "audio": "mix|1-2|3-4|5-6|7-8", "stream_id": null | "...",
//!  "stream_key": null | "...", "passphrase": null | "..."}
//! ```
//!
//! Every field is required; bounds are those of
//! [`fm_types::network_stream::NetworkStreamConfig::validate`]. The project
//! file keeps the URL with its credentials, the RTMP stream key, and the SRT
//! passphrase: it is the operator's own configuration, and only reports
//! redact them.

use core::fmt::Write as _;
use std::num::NonZeroU32;

use fm_types::network_stream::{
    AudioChannels, GiveUp, NetworkStreamConfig, NetworkStreamSettings, ReconnectPolicy,
    RtspTransport, SrtKeyLength, StallSlate, StreamMode,
};

use super::{DecodeError, Object, Value, syntax, unknown_enum};

/// Writes `{"type": "network_stream", ...}`.
pub(crate) fn write_network_stream(output: &mut String, config: &NetworkStreamConfig) {
    output.push_str("{\"type\": \"network_stream\", \"url\": \"");
    super::escape_string(output, &config.url);
    let settings = &config.settings;
    let give_up = match settings.reconnect.give_up {
        GiveUp::Never => "\"never\"".to_owned(),
        GiveUp::After(attempts) => attempts.to_string(),
    };
    write!(
        output,
        "\", \"transport\": \"{}\", \"jitter_ms\": {}, \"reconnect\": {{\"backoff_min_ms\": {}, \
         \"backoff_max_ms\": {}, \"give_up\": {give_up}}}, \"timeout_ms\": {}, \"stall\": \"{}\", \
         \"mode\": \"{}\", \"latency_ms\": {}, \"key_length\": {}, \"audio\": \"{}\"",
        settings.transport.token(),
        settings.jitter_buffer_ms,
        settings.reconnect.backoff_min_ms,
        settings.reconnect.backoff_max_ms,
        settings.timeout_ms,
        settings.stall.token(),
        settings.mode.token(),
        settings.latency_ms,
        settings.key_length.bytes(),
        settings.audio.token(),
    )
    .expect("writing to a string cannot fail");
    for (name, value) in [
        ("stream_id", &config.stream_id),
        ("stream_key", &config.stream_key),
        ("passphrase", &config.passphrase),
    ] {
        write!(output, ", \"{name}\": ").expect("writing to a string cannot fail");
        match value {
            Some(value) => {
                output.push('"');
                super::escape_string(output, value);
                output.push('"');
            }
            None => output.push_str("null"),
        }
    }
    output.push('}');
}

fn optional_string(object: &mut Object, name: &str) -> Result<Option<String>, DecodeError> {
    match object.take(name)? {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        _ => Err(syntax(format!("field `{name}` must be null or a string"))),
    }
}

/// Parses the fields of a `network_stream` input kind after its `type`.
pub(crate) fn parse_network_stream(
    object: &mut Object,
) -> Result<NetworkStreamConfig, DecodeError> {
    let url = object.string("url")?;
    let transport = {
        let token = object.string("transport")?;
        RtspTransport::from_token(&token)
            .ok_or_else(|| unknown_enum("network stream transport", &token))?
    };
    let jitter_buffer_ms = object.u32("jitter_ms")?;
    let mut reconnect = Object::new(object.take("reconnect")?, "network stream reconnect")?;
    let backoff_min_ms = reconnect.u32("backoff_min_ms")?;
    let backoff_max_ms = reconnect.u32("backoff_max_ms")?;
    let give_up = match reconnect.take("give_up")? {
        Value::String(token) if token == "never" => GiveUp::Never,
        Value::Number(attempts) => u32::try_from(attempts)
            .ok()
            .and_then(NonZeroU32::new)
            .map(GiveUp::After)
            .ok_or_else(|| syntax("field `give_up` must be \"never\" or 1..=u32"))?,
        _ => {
            return Err(syntax(
                "field `give_up` must be \"never\" or an attempt count",
            ));
        }
    };
    reconnect.finish()?;
    let timeout_ms = object.u32("timeout_ms")?;
    let stall = {
        let token = object.string("stall")?;
        StallSlate::from_token(&token)
            .ok_or_else(|| unknown_enum("network stream stall", &token))?
    };
    let mode = {
        let token = object.string("mode")?;
        StreamMode::from_token(&token).ok_or_else(|| unknown_enum("network stream mode", &token))?
    };
    let latency_ms = object.u32("latency_ms")?;
    let key_length = {
        let bytes = object.u32("key_length")?;
        SrtKeyLength::from_token(&bytes.to_string())
            .ok_or_else(|| syntax("field `key_length` must be 16, 24, or 32"))?
    };
    let audio = {
        let token = object.string("audio")?;
        AudioChannels::from_token(&token)
            .ok_or_else(|| unknown_enum("network stream audio", &token))?
    };
    let stream_id = optional_string(object, "stream_id")?;
    let stream_key = optional_string(object, "stream_key")?;
    let passphrase = optional_string(object, "passphrase")?;
    let config = NetworkStreamConfig {
        url,
        stream_id,
        stream_key,
        passphrase,
        settings: NetworkStreamSettings {
            transport,
            jitter_buffer_ms,
            reconnect: ReconnectPolicy {
                backoff_min_ms,
                backoff_max_ms,
                give_up,
            },
            timeout_ms,
            stall,
            mode,
            latency_ms,
            key_length,
            audio,
        },
    };
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(config)
}
