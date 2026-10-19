//! The self-contained phone tally and operator pages served on the WebSocket
//! gateway port.
//!
//! Serving them from the gateway's own origin lets a page reach
//! `/v1/control` same-origin, so the gateway can require a matching `Origin`
//! for browser-carried credentials. Each page is static and carries no secret:
//! the operator enters the `FREEMIXD_WEB_TOKEN` in the page, which presents it
//! through the WebSocket subprotocol list. Requests for any other path fall
//! through untouched to the WebSocket handshake.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    thread,
    time::{Duration, Instant},
};

pub(super) const TALLY_PATH: &str = "/tally";
pub(super) const TALLY_PAGE: &str = include_str!("tally.html");
/// The phone and tablet operator surface (`RC-014`).
pub(super) const OPERATOR_PATH: &str = "/operator";
pub(super) const OPERATOR_PAGE: &str = include_str!("operator.html");

const MAX_REQUEST_LINE: usize = 2 * 1024;
const MAX_REQUEST_HEAD: usize = 8 * 1024;
const PEEK_POLL: Duration = Duration::from_millis(2);

/// What [`serve`] did with an accepted socket.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum PageOutcome {
    /// Not a page request; the request bytes are still unread.
    NotPage,
    /// Answered (or abandoned); the socket must not be reused.
    Handled,
}

/// Answers a page request without consuming any other request.
///
/// Only the request line is peeked before deciding, so a WebSocket upgrade
/// reaches the handshake with every byte intact.
pub(super) fn serve(stream: &mut TcpStream, timeout: Duration) -> PageOutcome {
    let deadline = Instant::now() + timeout;
    let request = match peek_request_line(stream, deadline) {
        Peeked::Line(request) => request,
        Peeked::NotPage => return PageOutcome::NotPage,
        Peeked::Gone => return PageOutcome::Handled,
    };
    let Some((method, page)) = page_method(&request) else {
        return PageOutcome::NotPage;
    };
    let Some(head) = read_head(stream, deadline) else {
        let _ = stream.shutdown(Shutdown::Both);
        return PageOutcome::Handled;
    };
    let response = match method {
        Method::Get => page_response(&head, page, true),
        Method::Head => page_response(&head, page, false),
        Method::Other => {
            b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    if stream
        .write_all(&response)
        .and_then(|()| stream.flush())
        .is_ok()
    {
        let _ = stream.shutdown(Shutdown::Write);
    }
    PageOutcome::Handled
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Method {
    Get,
    Head,
    Other,
}

/// The peeked first line of a request.
enum Peeked {
    Line(String),
    /// The bytes cannot be a page request; leave them for the handshake.
    NotPage,
    /// The peer went away or stayed silent past the deadline.
    Gone,
}

fn peek_request_line(stream: &TcpStream, deadline: Instant) -> Peeked {
    let mut buffer = [0; MAX_REQUEST_LINE];
    loop {
        let peeked = match stream.peek(&mut buffer) {
            Ok(0) => return Peeked::Gone,
            Ok(peeked) => peeked,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Peeked::Gone,
        };
        if let Some(end) = buffer[..peeked].windows(2).position(|pair| pair == b"\r\n") {
            return std::str::from_utf8(&buffer[..end])
                .map_or(Peeked::NotPage, |line| Peeked::Line(line.to_owned()));
        }
        if peeked == buffer.len() {
            return Peeked::NotPage;
        }
        if Instant::now() >= deadline {
            return Peeked::Gone;
        }
        thread::sleep(PEEK_POLL);
    }
}

/// Recognizes `<METHOD> /tally[?query] HTTP/1.x` (or `/operator`) and
/// returns the method with the page it names.
fn page_method(request_line: &str) -> Option<(Method, &'static str)> {
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return None;
    }
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let page = match path {
        TALLY_PATH => TALLY_PAGE,
        OPERATOR_PATH => OPERATOR_PAGE,
        _ => return None,
    };
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => Method::Other,
    };
    Some((method, page))
}

/// Consumes the full request head so closing never resets unread bytes.
fn read_head(stream: &mut TcpStream, deadline: Instant) -> Option<String> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() >= MAX_REQUEST_HEAD || Instant::now() >= deadline {
            return None;
        }
        let wanted = chunk.len().min(MAX_REQUEST_HEAD - head.len());
        match stream.read(&mut chunk[..wanted]) {
            Ok(0) => return None,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(head).ok()
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

fn host_header(head: &str) -> Option<&str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("host").then_some(value)
    })
}

fn page_response(head: &str, page: &str, body: bool) -> Vec<u8> {
    // The page only ever connects back to its own gateway. `'self'` covers
    // same-origin WebSockets in current browsers; the explicit `ws://` source
    // keeps older engines that predate that rule working.
    let connect = host_header(head)
        .and_then(sanitized_host)
        .map_or_else(|| "'self'".to_owned(), |host| format!("'self' ws://{host}"));
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Frame-Options: DENY\r\n\
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
    use super::{Method, OPERATOR_PAGE, TALLY_PAGE, page_method, page_response, sanitized_host};

    #[test]
    fn only_the_page_paths_are_page_requests() {
        assert_eq!(
            page_method("GET /tally HTTP/1.1"),
            Some((Method::Get, TALLY_PAGE))
        );
        assert_eq!(
            page_method("GET /tally?input=7 HTTP/1.1"),
            Some((Method::Get, TALLY_PAGE))
        );
        assert_eq!(
            page_method("HEAD /tally HTTP/1.0"),
            Some((Method::Head, TALLY_PAGE))
        );
        assert_eq!(
            page_method("POST /tally HTTP/1.1"),
            Some((Method::Other, TALLY_PAGE))
        );
        assert_eq!(
            page_method("GET /operator HTTP/1.1"),
            Some((Method::Get, OPERATOR_PAGE))
        );
        assert_eq!(
            page_method("HEAD /operator?x=1 HTTP/1.1"),
            Some((Method::Head, OPERATOR_PAGE))
        );
        for line in [
            "GET /v1/control HTTP/1.1",
            "GET /tally/ HTTP/1.1",
            "GET /tallyx HTTP/1.1",
            "GET /operator/ HTTP/1.1",
            "GET /operator/transition HTTP/1.1",
            "GET / HTTP/1.1",
            "GET /tally HTTP/2",
            "GET /tally HTTP/1.1 extra",
            "GET /tally",
            "",
        ] {
            assert_eq!(page_method(line), None, "{line:?}");
        }
    }

    #[test]
    fn hosts_are_restricted_to_plain_authorities() {
        assert_eq!(sanitized_host(" 127.0.0.1:9000"), Some("127.0.0.1:9000"));
        assert_eq!(sanitized_host("[::1]:80"), Some("[::1]:80"));
        assert_eq!(sanitized_host("evil; script-src *"), None);
        assert_eq!(sanitized_host(""), None);
    }

    #[test]
    fn head_responses_carry_headers_without_a_body() {
        let head = "HEAD /tally HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n";
        let response = String::from_utf8(page_response(head, TALLY_PAGE, false)).unwrap();
        assert!(response.ends_with("\r\n\r\n"));
        assert!(response.contains(&format!("Content-Length: {}\r\n", TALLY_PAGE.len())));
        assert!(response.contains("connect-src 'self' ws://127.0.0.1:4000;"));
        let hostile = "GET /tally HTTP/1.1\r\nHost: a b\r\n\r\n";
        let response = String::from_utf8(page_response(hostile, OPERATOR_PAGE, true)).unwrap();
        assert!(response.contains("connect-src 'self'; base-uri"));
        assert!(response.contains(&format!("Content-Length: {}\r\n", OPERATOR_PAGE.len())));
        assert!(response.ends_with(OPERATOR_PAGE));
    }
}
