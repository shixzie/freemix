//! The self-contained phone tally, operator, and title editor pages served
//! on the WebSocket gateway port.
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
/// The browser title editor (`GX-006`): edits a title input's fields and
/// takes it on or off air through the title commands.
pub(super) const TITLES_PATH: &str = "/titles";
pub(super) const TITLES_PAGE: &str = include_str!("titles.html");

/// The start page at `/`, where the mDNS name (`https://<name>.local`)
/// lands: links to the pages and, with the local CA, to its root
/// certificate with install steps.
pub(super) const INDEX_PATH: &str = "/";

/// The start page; `ca` adds the certificate section.
pub(super) fn index_page(ca: bool) -> String {
    let certificate = if ca {
        format!(
            "<section><h2>Trust this computer</h2>\
             <p>Install the FreeMix certificate once so this phone opens the pages without a \
             warning.</p><p><a class=\"button\" href=\"{}\" download>Download the certificate</a></p>\
             <p><b>iPhone, iPad:</b> open the download, then Settings &rsaquo; Profile Downloaded \
             &rsaquo; Install, then Settings &rsaquo; General &rsaquo; About &rsaquo; Certificate \
             Trust Settings and turn on full trust for it.</p>\
             <p><b>Android:</b> Settings &rsaquo; Security &rsaquo; Encryption &amp; credentials \
             &rsaquo; Install a certificate &rsaquo; CA certificate, then pick the download.</p>\
             <p>Compare the fingerprint Studio shows before trusting it.</p></section>",
            fm_protocol::WEB_CA_PATH
        )
    } else {
        String::new()
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>FreeMix</title><style>body{{font:16px system-ui,sans-serif;margin:0 auto;\
         max-width:32rem;padding:1rem;background:#111;color:#eee}}a{{color:#7cf}}\
         nav a,.button{{display:block;margin:.5rem 0;padding:.9rem;border:1px solid #555;\
         border-radius:.5rem;text-decoration:none;text-align:center}}</style></head><body>\
         <h1>FreeMix</h1><nav aria-label=\"Pages\"><a href=\"{OPERATOR_PATH}\">Operator</a>\
         <a href=\"{TALLY_PATH}\">Tally light</a><a href=\"{TITLES_PATH}\">Title editor</a></nav>\
         {certificate}</body></html>"
    )
}

/// The local root certificate (DER) for a phone to install. It is public,
/// so no credential is needed; `attachment` makes browsers hand it to the
/// system's certificate installer instead of showing it.
pub(super) fn ca_response(der: &[u8], body: bool) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/x-x509-ca-cert\r\n\
         Content-Disposition: attachment; filename=\"freemix-ca.crt\"\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        der.len()
    )
    .into_bytes();
    if body {
        response.extend_from_slice(der);
    }
    response
}

/// The page served at `path`, if any.
pub(super) fn page_for(path: &str) -> Option<&'static str> {
    match path {
        TALLY_PATH => Some(TALLY_PAGE),
        OPERATOR_PATH => Some(OPERATOR_PAGE),
        TITLES_PATH => Some(TITLES_PAGE),
        _ => None,
    }
}

/// A page answer for an allowed `host`.
///
/// The page only ever connects back to its own gateway. `'self'` covers
/// same-origin `WebSockets` in current browsers; the explicit `ws://` or
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
    use super::{
        OPERATOR_PAGE, TALLY_PAGE, TITLES_PAGE, ca_response, index_page, page_for, page_response,
    };

    #[test]
    fn the_start_page_links_the_pages_and_offers_the_root() {
        let plain = index_page(false);
        for path in ["/operator", "/tally", "/titles"] {
            assert!(plain.contains(&format!("href=\"{path}\"")), "{path}");
        }
        assert!(!plain.contains("freemix-ca.crt"));
        let with_ca = index_page(true);
        assert!(with_ca.contains("href=\"/freemix-ca.crt\" download"));
        assert!(
            !with_ca.contains("<script"),
            "the start page runs no script"
        );
        let response = String::from_utf8(ca_response(b"DER", true)).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: application/x-x509-ca-cert\r\n"));
        assert!(response.contains("Content-Length: 3\r\n"));
        assert!(response.ends_with("\r\n\r\nDER"));
        assert!(
            String::from_utf8(ca_response(b"DER", false))
                .unwrap()
                .ends_with("\r\n\r\n")
        );
    }

    #[test]
    fn only_the_page_paths_are_pages() {
        assert_eq!(page_for("/tally"), Some(TALLY_PAGE));
        assert_eq!(page_for("/operator"), Some(OPERATOR_PAGE));
        assert_eq!(page_for("/titles"), Some(TITLES_PAGE));
        for path in [
            "/titles/",
            "/titles/1",
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
