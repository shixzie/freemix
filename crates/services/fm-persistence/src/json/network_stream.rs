//! A network stream input in `project.json` (schema 43, IN-015).
//!
//! ```text
//! {"type": "network_stream", "url": "rtsp://user:pass@cam/stream",
//!  "transport": "auto|tcp|udp", "jitter_ms": 300,
//!  "reconnect": {"backoff_min_ms": 500, "backoff_max_ms": 10000, "give_up": "never" | 5},
//!  "timeout_ms": 5000, "stall": "freeze|black"}
//! ```
//!
//! Every field is required; bounds are those of
//! [`fm_types::network_stream::NetworkStreamConfig::validate`]. The project
//! file keeps the URL with its credentials: it is the operator's own
//! configuration, and only reports redact it.

use core::fmt::Write as _;
use std::num::NonZeroU32;

use fm_types::network_stream::{
    GiveUp, NetworkStreamConfig, NetworkStreamSettings, ReconnectPolicy, RtspTransport, StallSlate,
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
         \"backoff_max_ms\": {}, \"give_up\": {give_up}}}, \"timeout_ms\": {}, \"stall\": \"{}\"}}",
        settings.transport.token(),
        settings.jitter_buffer_ms,
        settings.reconnect.backoff_min_ms,
        settings.reconnect.backoff_max_ms,
        settings.timeout_ms,
        settings.stall.token(),
    )
    .expect("writing to a string cannot fail");
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
    let config = NetworkStreamConfig {
        url,
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
        },
    };
    config
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(config)
}
