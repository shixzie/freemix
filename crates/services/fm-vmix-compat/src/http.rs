//! A nonblocking HTTP boundary for the vMix-compatible API (RC-009).
//!
//! vMix's HTTP API is a plain `GET` endpoint: a request with a `Function`
//! query parameter executes that function and answers `OK` (or
//! `Error: <reason>`), while a request without one returns the XML state.
//! This module parses bounded requests, calls the same
//! [`parse_http_query`] and executor the TCP boundary uses, and never blocks
//! the caller's event loop.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
};

use crate::{CompatState, ParseError, Translation, parse_http_query};

/// Largest accepted request (request line plus headers).
pub const MAX_HTTP_REQUEST_BYTES: usize = 16 * 1024;
/// Most concurrent HTTP connections.
pub const MAX_HTTP_SESSIONS: usize = 32;
/// Most response bytes buffered for one slow connection.
pub const MAX_HTTP_WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Why the HTTP listener failed.
#[derive(Debug)]
pub enum VmixHttpServerError {
    Io(io::Error),
}

impl std::fmt::Display for VmixHttpServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "vMix HTTP server: {error}"),
        }
    }
}

impl std::error::Error for VmixHttpServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for VmixHttpServerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Connection {
    stream: TcpStream,
    read: Vec<u8>,
    write: Vec<u8>,
    close_after_write: bool,
}

/// A nonblocking vMix HTTP API listener.
pub struct VmixHttpServer {
    listener: TcpListener,
    connections: Vec<Connection>,
}

impl std::fmt::Debug for VmixHttpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmixHttpServer")
            .field("connections", &self.connections.len())
            .finish_non_exhaustive()
    }
}

impl VmixHttpServer {
    /// Binds `address` and switches it to nonblocking operation.
    ///
    /// # Errors
    ///
    /// Returns the bind or nonblocking-mode failure.
    pub fn bind(address: impl ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            connections: Vec::new(),
        })
    }

    /// The bound address.
    ///
    /// # Errors
    ///
    /// Returns the listener's address-lookup failure.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Open connections, including ones whose close is pending.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.connections.len()
    }

    /// Accepts, reads, dispatches, writes, and closes one readiness pass.
    ///
    /// `execute` applies one dispatched [`Translation`] to `state` and
    /// returns the message for an `Error:` answer.
    ///
    /// # Errors
    ///
    /// Returns an accept failure for the listener itself; a per-connection
    /// socket failure only closes that connection.
    pub fn poll(
        &mut self,
        state: &mut CompatState,
        execute: &mut dyn FnMut(&mut CompatState, Translation) -> Result<(), String>,
    ) -> Result<(), VmixHttpServerError> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.connections.len() >= MAX_HTTP_SESSIONS {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    stream.set_nonblocking(true)?;
                    self.connections.push(Connection {
                        stream,
                        read: Vec::new(),
                        write: Vec::new(),
                        close_after_write: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }

        for connection in &mut self.connections {
            if connection.close_after_write {
                continue;
            }
            let mut buffer = [0_u8; 4_096];
            loop {
                match connection.stream.read(&mut buffer) {
                    Ok(0) => {
                        connection.close_after_write = true;
                        break;
                    }
                    Ok(bytes) => {
                        connection.read.extend_from_slice(&buffer[..bytes]);
                        if connection.read.len() > MAX_HTTP_REQUEST_BYTES {
                            connection.write.extend_from_slice(&response(
                                413,
                                "Payload Too Large",
                                "text/plain",
                                "Error: request too large",
                            ));
                            connection.close_after_write = true;
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        connection.close_after_write = true;
                        break;
                    }
                }
            }
            if connection.read.is_empty() {
                continue;
            }
            let Some(end) = find_headers_end(&connection.read) else {
                continue;
            };
            let request = connection.read.drain(..end).collect::<Vec<_>>();
            connection.read.clear();
            connection
                .write
                .extend_from_slice(&handle_request(&request, state, execute));
            connection.close_after_write = true;
        }

        for connection in &mut self.connections {
            if !connection.write.is_empty() {
                match connection.stream.write(&connection.write) {
                    Ok(bytes) => {
                        connection.write.drain(..bytes);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        connection.write.clear();
                        connection.close_after_write = true;
                    }
                }
            }
            if connection.write.len() > MAX_HTTP_WRITE_BUFFER_BYTES {
                connection.write.clear();
                connection.close_after_write = true;
            }
        }

        self.connections
            .retain(|connection| !(connection.close_after_write && connection.write.is_empty()));
        Ok(())
    }
}

fn find_headers_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn handle_request(
    request: &[u8],
    state: &mut CompatState,
    execute: &mut dyn FnMut(&mut CompatState, Translation) -> Result<(), String>,
) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(request) else {
        return response(400, "Bad Request", "text/plain", "Error: invalid encoding");
    };
    let Some(line) = text.split("\r\n").next() else {
        return response(400, "Bad Request", "text/plain", "Error: empty request");
    };
    let mut parts = line.split(' ');
    let Some(method) = parts.next() else {
        return response(400, "Bad Request", "text/plain", "Error: malformed request");
    };
    let Some(target) = parts.next() else {
        return response(400, "Bad Request", "text/plain", "Error: malformed request");
    };
    let Some(version) = parts.next() else {
        return response(400, "Bad Request", "text/plain", "Error: malformed request");
    };
    if parts.next().is_some() || !version.starts_with("HTTP/") {
        return response(400, "Bad Request", "text/plain", "Error: malformed request");
    }
    if method != "GET" {
        return response(
            405,
            "Method Not Allowed",
            "text/plain",
            "Error: only GET is supported",
        );
    }
    let Some(path) = target.strip_prefix('/') else {
        return response(400, "Bad Request", "text/plain", "Error: malformed request");
    };
    let query = path.split_once('?').map_or("", |(_, query)| query);
    match parse_http_query(query, state) {
        Ok(translation) => match execute(state, translation) {
            Ok(()) => response(200, "OK", "text/plain; charset=utf-8", "OK"),
            Err(error) => response(
                200,
                "OK",
                "text/plain; charset=utf-8",
                &format!("Error: {}", single_line(&error)),
            ),
        },
        Err(ParseError::MissingFunction) => {
            response(200, "OK", "text/xml; charset=utf-8", &state.xml())
        }
        Err(error) => response(
            200,
            "OK",
            "text/plain; charset=utf-8",
            &format!("Error: {}", single_line(&error.to_string())),
        ),
    }
}

fn response(status: u16, reason: &str, content_type: &str, body: &str) -> Vec<u8> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut output = Vec::with_capacity(head.len() + body.len());
    output.extend_from_slice(head.as_bytes());
    output.extend_from_slice(body.as_bytes());
    output
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\r' | '\n') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_protocol::WireInputId;

    use super::*;
    use crate::CompatInput;

    fn id(value: u128) -> WireInputId {
        WireInputId::new(NonZeroU128::new(value).unwrap())
    }

    fn state() -> CompatState {
        CompatState::new(
            "29.0.0.0",
            vec![CompatInput::new(id(1), "One", "Input")],
            id(1),
            id(1),
        )
        .unwrap()
    }

    #[test]
    fn requests_are_bounded_and_only_get_is_accepted() {
        let mut state = state();
        let mut executed = 0;
        let mut execute = |_: &mut CompatState, _: Translation| {
            executed += 1;
            Ok(())
        };
        let ok = handle_request(
            b"GET /?Function=Cut HTTP/1.1\r\nHost: x\r\n\r\n",
            &mut state,
            &mut execute,
        );
        let text = String::from_utf8(ok).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.ends_with("\r\n\r\nOK"));
        assert_eq!(executed, 1);

        let post = handle_request(b"POST / HTTP/1.1\r\n\r\n", &mut state, &mut execute);
        assert!(
            String::from_utf8(post)
                .unwrap()
                .starts_with("HTTP/1.1 405 ")
        );
    }

    #[test]
    fn a_request_without_a_function_returns_the_xml_state() {
        let mut state = state();
        let mut execute = |_: &mut CompatState, _: Translation| Ok(());
        let response = handle_request(b"GET / HTTP/1.1\r\n\r\n", &mut state, &mut execute);
        let text = String::from_utf8(response).unwrap();
        assert!(text.contains("Content-Type: text/xml"));
        assert!(text.contains("<vmix>"));
        assert!(text.contains("<input key="));
    }

    #[test]
    fn a_function_refusal_is_an_error_body() {
        let mut state = state();
        let mut execute = |_: &mut CompatState, _: Translation| Err("no".to_owned());
        let response = handle_request(
            b"GET /?Function=Cut HTTP/1.1\r\n\r\n",
            &mut state,
            &mut execute,
        );
        let text = String::from_utf8(response).unwrap();
        assert!(text.ends_with("\r\n\r\nError: no"));
    }
}
