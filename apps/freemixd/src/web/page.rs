//! The self-contained phone tally and operator pages served on the WebSocket
//! gateway port.
//!
//! Serving them from the gateway's own origin lets a page reach
//! `/v1/control` same-origin, so the gateway can require a matching `Origin`
//! for browser-carried credentials. Each page is static and carries no
//! secret: a device pairs through `/v1/pair` (or the operator enters the
//! `FREEMIXD_WEB_TOKEN`), and the page presents its token through the
//! WebSocket subprotocol list.

pub(super) const TALLY_PATH: &str = "/tally";
pub(super) const TALLY_PAGE: &str = include_str!("tally.html");
/// The phone and tablet operator surface (`RC-014`).
pub(super) const OPERATOR_PATH: &str = "/operator";
pub(super) const OPERATOR_PAGE: &str = include_str!("operator.html");

/// The page served at `path`, if any.
pub(super) fn page_for(path: &str) -> Option<&'static str> {
    match path {
        TALLY_PATH => Some(TALLY_PAGE),
        OPERATOR_PATH => Some(OPERATOR_PAGE),
        _ => None,
    }
}

/// A page answer for an allowed `host`.
///
/// The page only ever connects back to its own gateway. `'self'` covers
/// same-origin WebSockets in current browsers; the explicit `ws://` or
/// `wss://` source keeps older engines that predate that rule working.
pub(super) fn page_response(host: Option<&str>, tls: bool, page: &str, body: bool) -> Vec<u8> {
    let scheme = if tls { "wss" } else { "ws" };
    let connect = host.and_then(super::http::sanitized_host).map_or_else(
        || "'self'".to_owned(),
        |host| format!("'self' {scheme}://{host}"),
    );
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Frame-Options: DENY\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Permissions-Policy: camera=(), microphone=(), geolocation=()\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
         style-src 'unsafe-inline'; img-src data:; connect-src {connect}; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'\r\n\
         Connection: close\r\n\r\n",
        page.len()
    )
    .into_bytes();
    if body {
        response.extend_from_slice(page.as_bytes());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::{OPERATOR_PAGE, TALLY_PAGE, page_for, page_response};

    #[test]
    fn only_the_page_paths_are_pages() {
        assert_eq!(page_for("/tally"), Some(TALLY_PAGE));
        assert_eq!(page_for("/operator"), Some(OPERATOR_PAGE));
        for path in [
            "/v1/control",
            "/tally/",
            "/tallyx",
            "/operator/",
            "/operator/transition",
            "/",
        ] {
            assert_eq!(page_for(path), None, "{path:?}");
        }
    }

    #[test]
    fn head_responses_carry_headers_without_a_body() {
        let response = String::from_utf8(page_response(
            Some("127.0.0.1:4000"),
            false,
            TALLY_PAGE,
            false,
        ))
        .unwrap();
        assert!(response.ends_with("\r\n\r\n"));
        assert!(response.contains(&format!("Content-Length: {}\r\n", TALLY_PAGE.len())));
        assert!(response.contains("connect-src 'self' ws://127.0.0.1:4000;"));
        let tls = String::from_utf8(page_response(
            Some("192.168.1.20:8443"),
            true,
            OPERATOR_PAGE,
            true,
        ))
        .unwrap();
        assert!(tls.contains("connect-src 'self' wss://192.168.1.20:8443;"));
        assert!(tls.ends_with(OPERATOR_PAGE));
        let hostile =
            String::from_utf8(page_response(Some("a b"), false, OPERATOR_PAGE, true)).unwrap();
        assert!(hostile.contains("connect-src 'self'; base-uri"));
    }
}
