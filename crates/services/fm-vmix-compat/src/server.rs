//! A nonblocking loopback/lAN server for the vMix TCP API (RC-009).
//!
//! [`TcpHub`] is transport-independent and [`TcpSession`] frames one
//! connection. This module is the socket boundary: it binds a listener,
//! accepts bounded connections, reads CRLF lines with a cap, writes
//! responses without blocking, fans subscription events out to their
//! sessions, and calls a caller-supplied executor for each dispatched
//! translation.
//!
//! The caller drives [`VmixTcpServer::poll`] from its own event loop; the
//! server never spawns a thread and never blocks past a socket readiness
//! check.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
};

use crate::{CompatState, MAX_TCP_LINE_BYTES, TcpHub, TcpReply, TcpSessionError, TcpSessionId, Translation};

/// Most response bytes buffered for one slow connection before it is
/// dropped instead of growing without bound.
pub const MAX_TCP_WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Why the server could not accept, read, or write.
#[derive(Debug)]
pub enum VmixServerError {
    Io(io::Error),
    Session(TcpSessionError),
}

impl std::fmt::Display for VmixServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "vMix TCP server: {error}"),
            Self::Session(error) => write!(formatter, "vMix TCP session: {error}"),
        }
    }
}

impl std::error::Error for VmixServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Session(error) => Some(error),
        }
    }
}

impl From<io::Error> for VmixServerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<TcpSessionError> for VmixServerError {
    fn from(error: TcpSessionError) -> Self {
        Self::Session(error)
    }
}

struct Connection {
    id: TcpSessionId,
    stream: TcpStream,
    read: Vec<u8>,
    write: Vec<u8>,
    close_after_write: bool,
}

/// A nonblocking vMix TCP API listener.
pub struct VmixTcpServer {
    listener: TcpListener,
    hub: TcpHub,
    connections: Vec<Connection>,
}

impl std::fmt::Debug for VmixTcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmixTcpServer")
            .field("connections", &self.connections.len())
            .finish_non_exhaustive()
    }
}

impl VmixTcpServer {
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
            hub: TcpHub::new(),
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

    /// Open sessions, including ones whose close is pending.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.hub.len()
    }

    /// Accepts, reads, dispatches, writes, and publishes one readiness pass.
    ///
    /// `execute` applies one dispatched [`Translation`] to `state` and
    /// returns the message for `FUNCTION ER` on refusal. A translation the
    /// caller accepts is answered with `FUNCTION OK Completed`.
    ///
    /// # Errors
    ///
    /// Returns an accept, read, or write failure for the listener itself; a
    /// per-connection socket failure only closes that connection.
    #[allow(clippy::too_many_lines)] // one pass over accept, read, write, publish
    pub fn poll(
        &mut self,
        state: &mut CompatState,
        execute: &mut dyn FnMut(&mut CompatState, Translation) -> Result<(), String>,
    ) -> Result<(), VmixServerError> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.hub.len() >= crate::MAX_TCP_SESSIONS {
                        continue;
                    }
                    stream.set_nonblocking(true)?;
                    let id = self.hub.open()?;
                    self.connections.push(Connection {
                        id,
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
                        if connection.read.len() > MAX_TCP_LINE_BYTES * 4 {
                            connection.write.extend_from_slice(b"ERROR Line too long\r\n");
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

            while let Some(newline) = connection.read.iter().position(|byte| *byte == b'\n') {
                let mut line = connection.read.drain(..=newline).collect::<Vec<_>>();
                if line.len() > MAX_TCP_LINE_BYTES + 2 {
                    connection.write.extend_from_slice(b"ERROR Line too long\r\n");
                    connection.close_after_write = true;
                    break;
                }
                let Ok(text) = std::str::from_utf8(&line) else {
                    connection.write.extend_from_slice(b"ERROR Invalid encoding\r\n");
                    connection.close_after_write = true;
                    break;
                };
                let text = text.strip_suffix('\n').unwrap_or(text);
                let text = text.strip_suffix('\r').unwrap_or(text);
                let reply = match self.hub.handle_line(connection.id, text, state) {
                    Ok(reply) => reply,
                    Err(error) => {
                        let payload = format!("ERROR {error}\r\n");
                        connection.write.extend_from_slice(payload.as_bytes());
                        connection.close_after_write = true;
                        break;
                    }
                };
                match reply {
                    TcpReply::Respond { payload, close } => {
                        connection.write.extend_from_slice(payload.as_bytes());
                        connection.close_after_write |= close;
                    }
                    TcpReply::Dispatch(translation) => {
                        let result = execute(state, translation);
                        let payload = match &result {
                            Ok(()) => self.hub.complete_function(connection.id, Ok(())),
                            Err(message) => self
                                .hub
                                .complete_function(connection.id, Err(message.as_str())),
                        }?;
                        connection.write.extend_from_slice(payload.as_bytes());
                    }
                }
                line.clear();
            }

            if connection.write.len() > MAX_TCP_WRITE_BUFFER_BYTES {
                connection.write.clear();
                connection.close_after_write = true;
            }
        }

        for event in self.hub.publish(state) {
            if let Some(connection) = self
                .connections
                .iter_mut()
                .find(|connection| connection.id == event.session)
            {
                connection.write.extend_from_slice(event.payload.as_bytes());
            }
        }

        // Flush both the line responses and the subscription events this
        // pass produced; a nonblocking write keeps the remainder for the
        // next pass.
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
            if connection.close_after_write && connection.write.is_empty() {
                connection.stream.shutdown(std::net::Shutdown::Both).ok();
            }
        }

        let closed = self
            .connections
            .iter()
            .filter(|connection| connection.close_after_write && connection.write.is_empty())
            .map(|connection| connection.id)
            .collect::<Vec<_>>();
        for id in closed {
            self.hub.close(id);
        }
        self.connections
            .retain(|connection| !(connection.close_after_write && connection.write.is_empty()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpStream,
        time::Duration,
    };

    fn client(address: SocketAddr) -> BufReader<TcpStream> {
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        BufReader::new(stream)
    }

    fn read_line(reader: &mut BufReader<TcpStream>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    fn state() -> CompatState {
        use crate::{CompatInput, WireInputId};
        use std::num::NonZeroU128;
        CompatState::new(
            "29",
            vec![
                CompatInput::new(
                    WireInputId::new(NonZeroU128::new(1).unwrap()),
                    "Camera",
                    "Camera",
                ),
                CompatInput::new(
                    WireInputId::new(NonZeroU128::new(2).unwrap()),
                    "News",
                    "GT",
                ),
            ],
            WireInputId::new(NonZeroU128::new(1).unwrap()),
            WireInputId::new(NonZeroU128::new(2).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn serves_version_functions_subscriptions_and_quit() {
        let mut server = VmixTcpServer::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let mut state = state();
        let mut client = client(address);
        let executed = std::cell::RefCell::new(Vec::new());
        let mut execute = |state: &mut CompatState, translation: Translation| {
            if let Translation::Protocol(fm_protocol::CommandPayload::SelectPreview { input }) = translation {
                state.preview = input;
                executed.borrow_mut().push(translation);
                Ok(())
            } else {
                Err("unsupported test translation".to_owned())
            }
        };
        server.poll(&mut state, &mut execute).unwrap();
        let mut writer = client.get_ref().try_clone().unwrap();
        writeln!(writer, "VERSION").unwrap();
        writeln!(writer, "SUBSCRIBE TALLY").unwrap();
        writeln!(writer, "FUNCTION PreviewInput Input=1").unwrap();
        for _ in 0..3 {
            server.poll(&mut state, &mut execute).unwrap();
        }
        let version = read_line(&mut client);
        let subscribed = read_line(&mut client);
        let function = read_line(&mut client);
        assert!(version.starts_with("VERSION OK "), "{version:?}");
        assert!(subscribed.starts_with("SUBSCRIBE OK TALLY"), "{subscribed:?}");
        assert_eq!(function, "FUNCTION OK Completed\r\n");
        assert_eq!(executed.borrow().len(), 1);
        assert_eq!(state.preview.get().get(), 1);
        // The FUNCTION moved preview from 2 to 1, so the subscription
        // emitted one event; another state change emits the next.
        let first_event = read_line(&mut client);
        assert!(first_event.starts_with("TALLY OK "), "{first_event:?}");
        state.preview = fm_protocol::WireInputId::new(std::num::NonZeroU128::new(2).unwrap());
        server.poll(&mut state, &mut execute).unwrap();
        let second_event = read_line(&mut client);
        assert!(second_event.starts_with("TALLY OK "), "{second_event:?}");
        writeln!(writer, "QUIT").unwrap();
        server.poll(&mut state, &mut execute).unwrap();
        // Closing the client is the socket-level EOF case; the hub drops the
        // session on the next pass.
        drop(writer);
        drop(client);
        for _ in 0..2 {
            server.poll(&mut state, &mut execute).unwrap();
        }
        assert_eq!(server.session_count(), 0);
    }
}
