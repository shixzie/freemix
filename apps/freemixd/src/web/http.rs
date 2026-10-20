//! The small HTTP/1.1 surface of the gateway: request heads, the `Host`
//! allowlist (DNS-rebinding defence), the same-origin rule, and the JSON
//! answers of `/v1/pair` and `/v1/whoami`.

use std::net::IpAddr;

/// The longest request head the gateway reads.
pub(super) const MAX_REQUEST_HEAD: usize = 8 * 1024;
/// The longest request body (`/v1/pair` form) the gateway reads.
pub(super) const MAX_REQUEST_BODY: usize = 1024;

/// One parsed request head.
#[derive(Debug)]
pub(super) struct Request {
    pub(super) method: String,
    pub(super) path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    /// Parses a complete head (through the blank line).
    pub(super) fn parse(head: &[u8]) -> Option<Self> {
        let head = std::str::from_utf8(head).ok()?;
        let mut lines = head.split("\r\n");
        let mut parts = lines.next()?.split(' ');
        let method = parts.next()?.to_owned();
        let target = parts.next()?;
        let version = parts.next()?;
        if parts.next().is_some() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
            return None;
        }
        if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return None;
        }
        let path = target.split_once('?').map_or(target, |(path, _)| path);
        if !path.starts_with('/') {
            return None;
        }
        let mut headers = Vec::new();
        for line in lines.take_while(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':')?;
            if name.is_empty() || name.contains(' ') {
                return None;
            }
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
        Some(Self {
            method,
            path: path.to_owned(),
            headers,
        })
    }

    /// The single value of `name`; `None` when absent or repeated.
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        let mut values = self
            .headers
            .iter()
            .filter(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str());
        let value = values.next()?;
        values.next().is_none().then_some(value)
    }

    pub(super) fn content_length(&self) -> Option<usize> {
        self.header("content-length")?.parse().ok()
    }

    /// Whether this is a WebSocket upgrade request.
    pub(super) fn is_upgrade(&self) -> bool {
        self.headers
            .iter()
            .any(|(name, value)| name == "upgrade" && value.eq_ignore_ascii_case("websocket"))
    }
}

/// The `Host` header value when it is a plain `host[:port]` authority.
pub(super) fn sanitized_host(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 255
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        }))
    .then_some(value)
}

/// The host name of a `host[:port]` authority, brackets removed.
fn host_name(authority: &str) -> Option<&str> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (address, port) = rest.split_once(']')?;
        return (port.is_empty() || port.strip_prefix(':').is_some_and(valid_port))
            .then_some(address);
    }
    match authority.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => valid_port(port).then_some(name),
        Some(_) => None,
        None => Some(authority),
    }
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether the gateway answers a request for `host` (the `Host` header).
///
/// A DNS-rebinding page reaches the gateway under its own domain name, so
/// only IP literals, `localhost`, and the operator's `--web-host` names are
/// served; anything else is refused before any page, pairing, or upgrade.
pub(super) fn allowed_host(host: &str, extra: &[String]) -> bool {
    let Some(authority) = sanitized_host(host) else {
        return false;
    };
    let Some(name) = host_name(authority) else {
        return false;
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name.parse::<IpAddr>().is_ok()
        || name == "localhost"
        || extra.iter().any(|allowed| *allowed == name)
}

/// Whether a browser request came from the page the gateway served:
/// `Origin` must be `<scheme>://<Host>`. A request without `Origin` is not
/// browser-originated and passes (`require` makes it mandatory).
pub(super) fn same_origin(request: &Request, tls: bool, require: bool) -> bool {
    let Some(origin) = request.header("origin") else {
        return !require;
    };
    let Some(host) = request.header("host").and_then(sanitized_host) else {
        return false;
    };
    let scheme = if tls { "https://" } else { "http://" };
    origin
        .strip_prefix(scheme)
        .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

/// One `application/x-www-form-urlencoded` field.
pub(super) fn form_field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(key)? == name)
            .then(|| percent_decode(value))
            .flatten()
    })
}

fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let high = char::from(input.next()?).to_digit(16)?;
                let low = char::from(input.next()?).to_digit(16)?;
                bytes.push(u8::try_from(high * 16 + low).ok()?);
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes).ok()
}

/// Escapes `text` as the inside of a JSON string.
pub(super) fn json_string(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('"');
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '<' => escaped.push_str("\\u003c"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

/// A JSON answer that is never cached or embedded elsewhere.
pub(super) fn json_response(status: &str, body: &str, extra_headers: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         {extra_headers}\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// An empty answer.
pub(super) fn empty_response(status: &str, extra_headers: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nCache-Control: no-store\r\n{extra_headers}Connection: close\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{Request, allowed_host, form_field, json_string, same_origin, sanitized_host};

    fn request(head: &str) -> Request {
        Request::parse(head.as_bytes()).unwrap()
    }

    #[test]
    fn heads_parse_strictly() {
        let parsed = request("GET /tally?input=1 HTTP/1.1\r\nHost: 10.0.0.2:4000\r\nX: y\r\n\r\n");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/tally");
        assert_eq!(parsed.header("host"), Some("10.0.0.2:4000"));
        for head in [
            "GET /tally HTTP/2\r\n\r\n",
            "GET tally HTTP/1.1\r\n\r\n",
            "get /tally HTTP/1.1\r\n\r\n",
            "GET /tally HTTP/1.1 x\r\n\r\n",
            "GET /tally HTTP/1.1\r\nBad Header: x\r\n\r\n",
        ] {
            assert!(Request::parse(head.as_bytes()).is_none(), "{head:?}");
        }
        let twice = request("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n");
        assert_eq!(twice.header("host"), None, "a repeated Host is ambiguous");
    }

    #[test]
    fn only_addresses_localhost_and_configured_names_are_served() {
        let extra = ["studio.local".to_owned()];
        for host in [
            "127.0.0.1:4000",
            "192.168.1.20:8443",
            "[::1]:4000",
            "[fe80::1]",
            "localhost:4000",
            "LOCALHOST",
            "studio.local:8443",
            "Studio.Local.",
        ] {
            assert!(allowed_host(host, &extra), "{host}");
        }
        for host in [
            "evil.example",
            "evil.example:4000",
            "127.0.0.1.nip.io:4000",
            "localhost.evil.example",
            "studio.local.evil.example",
            "a b",
            "",
            "::1",
            "[::1]x",
            "127.0.0.1:port",
        ] {
            assert!(!allowed_host(host, &extra), "{host}");
        }
        assert_eq!(sanitized_host("evil; script-src *"), None);
    }

    #[test]
    fn origins_must_match_the_scheme_and_host() {
        let head = |origin: &str| {
            request(&format!(
                "POST /v1/pair HTTP/1.1\r\nHost: 192.168.1.20:8443\r\nOrigin: {origin}\r\n\r\n"
            ))
        };
        assert!(same_origin(&head("https://192.168.1.20:8443"), true, true));
        assert!(!same_origin(&head("http://192.168.1.20:8443"), true, true));
        assert!(!same_origin(&head("https://evil.example"), true, true));
        assert!(!same_origin(&head("null"), true, true));
        let bare = request("POST /v1/pair HTTP/1.1\r\nHost: 192.168.1.20:8443\r\n\r\n");
        assert!(!same_origin(&bare, true, true));
        assert!(same_origin(&bare, true, false));
    }

    #[test]
    fn forms_and_json_are_escaped() {
        assert_eq!(
            form_field("code=042137&name=Cam%C3%A1ra+2", "name").as_deref(),
            Some("Camára 2")
        );
        assert_eq!(form_field("code=1", "name"), None);
        assert_eq!(form_field("name=%zz", "name"), None);
        assert_eq!(json_string("a\"<b>\n"), "\"a\\\"\\u003cb>\\u000a\"");
    }
}
