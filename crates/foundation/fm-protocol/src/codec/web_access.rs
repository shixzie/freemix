//! Web access codec (Protocol 2.71); see [`crate::WebAccessMessage`].

use core::fmt::Write as _;

use crate::{
    CodecError, MAX_WEB_DEVICES, MAX_WEB_LINK_CODE_BYTES, MAX_WEB_URL_BYTES,
    MIN_WEB_LINK_CODE_BYTES, WEB_PAIRING_CODE_DIGITS, WEB_PAIRING_LIFETIME_MS, WebAccessAction,
    WebAccessRefusal, WebDeviceEntry, WebDeviceRole, WebGatewayStatus, WebPairing,
    valid_web_device_id, valid_web_device_name, valid_web_fingerprint,
};

use super::media::{FieldSource, Push};
use super::value::parse_canonical_u64;

const DEVICES: &str = "devices";
const PAIRING: &str = "pairing";

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn role(field: &'static str, value: &str) -> Result<WebDeviceRole, CodecError> {
    WebDeviceRole::from_token(value).ok_or_else(|| invalid(field, value))
}

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn canonical(field: &'static str, value: &str) -> Result<u64, CodecError> {
    parse_canonical_u64(value).ok_or_else(|| invalid(field, value))
}

/// Encodes a request's `action` and its argument.
pub(super) fn encode_action(
    action: &WebAccessAction,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match action {
        WebAccessAction::Status => push("action", "status".to_owned()),
        WebAccessAction::Pair { role } => {
            push("action", "pair".to_owned())?;
            push("role", role.token().to_owned())
        }
        WebAccessAction::CancelPairing => push("action", "cancel_pairing".to_owned()),
        WebAccessAction::Revoke { device } => {
            if !valid_web_device_id(device) {
                return Err(invalid("device", device.clone()));
            }
            push("action", "revoke".to_owned())?;
            push("device", device.clone())
        }
    }
}

/// Decodes a request's `action`.
pub(super) fn decode_action(fields: &mut dyn FieldSource) -> Result<WebAccessAction, CodecError> {
    let action = fields.required("action")?;
    Ok(match action.as_str() {
        "status" => WebAccessAction::Status,
        "pair" => WebAccessAction::Pair {
            role: role("role", &fields.required("role")?)?,
        },
        "cancel_pairing" => WebAccessAction::CancelPairing,
        "revoke" => {
            let device = fields.required("device")?;
            if !valid_web_device_id(&device) {
                return Err(invalid("device", device));
            }
            WebAccessAction::Revoke { device }
        }
        _ => return Err(invalid("action", action)),
    })
}

fn check_url(url: &str) -> Result<(), CodecError> {
    let scheme_ok = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    if scheme_ok
        && url.len() <= MAX_WEB_URL_BYTES
        && url.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(invalid("url", url))
    }
}

fn check_pairing(pairing: &WebPairing) -> Result<(), CodecError> {
    let code_ok = pairing.code.len() == WEB_PAIRING_CODE_DIGITS
        && pairing.code.bytes().all(|byte| byte.is_ascii_digit());
    let link_ok = (MIN_WEB_LINK_CODE_BYTES..=MAX_WEB_LINK_CODE_BYTES)
        .contains(&pairing.link_code.len())
        && pairing
            .link_code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if code_ok && link_ok && pairing.expires_in_ms <= WEB_PAIRING_LIFETIME_MS {
        Ok(())
    } else {
        Err(invalid(PAIRING, "[redacted]"))
    }
}

fn check_devices(devices: &[WebDeviceEntry]) -> Result<(), CodecError> {
    if devices.len() > MAX_WEB_DEVICES {
        return Err(CodecError::TooManyItems(DEVICES));
    }
    for (index, device) in devices.iter().enumerate() {
        if !valid_web_device_id(&device.id) || !valid_web_device_name(&device.name) {
            return Err(invalid(DEVICES, device.id.clone()));
        }
        if index > 0 && devices[index - 1].id >= device.id {
            return Err(invalid(DEVICES, device.id.clone()));
        }
    }
    Ok(())
}

/// Encodes a gateway status after `gateway=1`.
pub(super) fn encode_gateway(
    gateway: &WebGatewayStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_url(&gateway.url)?;
    match (&gateway.fingerprint, gateway.tls) {
        (Some(fingerprint), true) if valid_web_fingerprint(fingerprint) => {}
        (None, false) => {}
        (fingerprint, _) => {
            return Err(invalid(
                "fingerprint",
                fingerprint.clone().unwrap_or_default(),
            ));
        }
    }
    check_devices(&gateway.devices)?;
    push("url", gateway.url.clone())?;
    push("tls", if gateway.tls { "1" } else { "0" }.to_owned())?;
    if let Some(fingerprint) = &gateway.fingerprint {
        push("fingerprint", fingerprint.clone())?;
    }
    if let Some(pairing) = &gateway.pairing {
        check_pairing(pairing)?;
        push(
            PAIRING,
            format!(
                "{}~{}~{}~{}",
                pairing.code,
                pairing.link_code,
                pairing.role.token(),
                pairing.expires_in_ms
            ),
        )?;
    }
    let mut encoded = String::new();
    for device in &gateway.devices {
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{}~{}~{}~{}:{}",
            device.id,
            device.role.token(),
            device.paired_at_ms,
            device
                .last_seen_ms
                .map_or_else(|| "-".to_owned(), |at| at.to_string()),
            device.name.len(),
            device.name,
        )
        .expect("writing to a string cannot fail");
    }
    push(DEVICES, encoded)
}

/// Decodes a gateway status, the exact inverse of [`encode_gateway`].
pub(super) fn decode_gateway(fields: &mut dyn FieldSource) -> Result<WebGatewayStatus, CodecError> {
    let url = fields.required("url")?;
    check_url(&url)?;
    let tls = flag("tls", &fields.required("tls")?)?;
    let fingerprint = if tls {
        let fingerprint = fields.required("fingerprint")?;
        if !valid_web_fingerprint(&fingerprint) {
            return Err(invalid("fingerprint", fingerprint));
        }
        Some(fingerprint)
    } else {
        None
    };
    let pairing = fields
        .optional(PAIRING)
        .map(|text| decode_pairing(&text))
        .transpose()?;
    let devices_text = fields.required(DEVICES)?;
    let mut devices = Vec::new();
    let mut rest = devices_text.as_str();
    while !rest.is_empty() {
        if !devices.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(DEVICES, devices_text.clone()))?;
        }
        if devices.len() == MAX_WEB_DEVICES {
            return Err(CodecError::TooManyItems(DEVICES));
        }
        let (device, after) = split_device(rest)?;
        devices.push(device);
        rest = after;
    }
    check_devices(&devices)?;
    Ok(WebGatewayStatus {
        url,
        tls,
        fingerprint,
        pairing,
        devices,
    })
}

fn decode_pairing(text: &str) -> Result<WebPairing, CodecError> {
    let bad = || invalid(PAIRING, "[redacted]");
    let mut parts = text.split('~');
    let (Some(code), Some(link_code), Some(role_token), Some(expires), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(bad());
    };
    let pairing = WebPairing {
        code: code.to_owned(),
        link_code: link_code.to_owned(),
        role: WebDeviceRole::from_token(role_token).ok_or_else(bad)?,
        expires_in_ms: parse_canonical_u64(expires)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(bad)?,
    };
    check_pairing(&pairing)?;
    Ok(pairing)
}

fn split_device(text: &str) -> Result<(WebDeviceEntry, &str), CodecError> {
    let bad = || invalid(DEVICES, text);
    let mut parts = [""; 4];
    let mut after = text;
    for part in &mut parts {
        let (value, tail) = after.split_once('~').ok_or_else(bad)?;
        *part = value;
        after = tail;
    }
    let [id, role_token, paired_at, last_seen] = parts;
    let (length, after) = after.split_once(':').ok_or_else(bad)?;
    let length = parse_canonical_u64(length)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= crate::MAX_WEB_DEVICE_NAME_BYTES)
        .ok_or_else(bad)?;
    let name = after.get(..length).ok_or_else(bad)?;
    let device = WebDeviceEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        role: role(DEVICES, role_token)?,
        paired_at_ms: canonical(DEVICES, paired_at)?,
        last_seen_ms: if last_seen == "-" {
            None
        } else {
            Some(canonical(DEVICES, last_seen)?)
        },
    };
    Ok((device, &after[length..]))
}

/// Encodes an optional refusal.
pub(super) fn refusal_token(refusal: Option<WebAccessRefusal>) -> Option<&'static str> {
    refusal.map(WebAccessRefusal::token)
}

/// Decodes an optional refusal.
pub(super) fn decode_refusal(
    value: Option<String>,
) -> Result<Option<WebAccessRefusal>, CodecError> {
    value
        .map(|token| WebAccessRefusal::from_token(&token).ok_or_else(|| invalid("refusal", token)))
        .transpose()
}
