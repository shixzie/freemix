use std::{
    io,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use tungstenite::{
    Error as WebSocketError, HandshakeError, Message, ServerHandshake, WebSocket,
    handshake::server::{Callback, ErrorResponse, Request, Response},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL},
    },
    protocol::WebSocketConfig,
};

use super::{
    BEARER_SUBPROTOCOL_PREFIX, CONTROL_SUBPROTOCOL, INBOUND_CAPACITY, OUTBOUND_CAPACITY, PATH,
    RELAY_POLL, SOCKET_HANDSHAKE_TIMEOUT, WEBSOCKET_MAX_WRITE_BUFFER, WEBSOCKET_MESSAGE_LIMIT,
    WEBSOCKET_READ_BUFFER, WEBSOCKET_WRITE_BUFFER, WebConnection, WebEvent, WebToken, WireMessage,
    decode_line,
    page::{self, PageOutcome},
};

/// Poll interval while an upgrade handshake waits for more request bytes.
const HANDSHAKE_POLL: Duration = Duration::from_millis(2);

pub(super) fn listener_loop(
    listener: &TcpListener,
    accepted: &SyncSender<TcpStream>,
    cancel: &Arc<AtomicBool>,
    accepting: &Arc<AtomicBool>,
) {
    while !cancel.load(Ordering::Acquire) {
        if !accepting.load(Ordering::Acquire) {
            thread::sleep(RELAY_POLL);
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if accepted.try_send(stream).is_err() {
                    // One active WebSocket is the transport bound. Drop excess peers.
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                thread::sleep(RELAY_POLL);
            }
            Err(_) => break,
        }
    }
}

#[allow(clippy::result_large_err)] // tungstenite handshake error response is externally sized
pub(super) fn worker_loop(
    accepted: &Receiver<TcpStream>,
    events: &SyncSender<WebEvent>,
    cancel: &Arc<AtomicBool>,
    token: &Arc<WebToken>,
) {
    while !cancel.load(Ordering::Acquire) {
        let stream = match accepted.recv_timeout(RELAY_POLL) {
            Ok(stream) => stream,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // BSD-derived `accept` (macOS included) hands the accepted socket the
        // non-blocking listener's `O_NONBLOCK`; the handshake and relay rely on
        // blocking reads bounded by timeouts, so restore blocking mode first.
        if stream.set_nonblocking(false).is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if let Err(error) = stream.set_read_timeout(Some(SOCKET_HANDSHAKE_TIMEOUT)) {
            let _ = stream.shutdown(Shutdown::Both);
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            continue;
        }
        if stream
            .set_write_timeout(Some(SOCKET_HANDSHAKE_TIMEOUT))
            .is_err()
        {
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        let mut stream = stream;
        if page::serve(&mut stream, SOCKET_HANDSHAKE_TIMEOUT) == PageOutcome::Handled {
            continue;
        }
        let config = WebSocketConfig::default()
            .read_buffer_size(WEBSOCKET_READ_BUFFER)
            .write_buffer_size(WEBSOCKET_WRITE_BUFFER)
            .max_write_buffer_size(WEBSOCKET_MAX_WRITE_BUFFER)
            .max_message_size(Some(WEBSOCKET_MESSAGE_LIMIT))
            .max_frame_size(Some(WEBSOCKET_MESSAGE_LIMIT))
            .accept_unmasked_frames(false);
        let callback_token = Arc::clone(token);
        let websocket = accept_within(
            stream,
            move |request: &Request, response: Response| {
                if request
                    .uri()
                    .path_and_query()
                    .map(tungstenite::http::uri::PathAndQuery::as_str)
                    != Some(PATH)
                {
                    return Err(http_error(StatusCode::NOT_FOUND));
                }
                authorize_upgrade(request.headers(), &callback_token, response)
            },
            config,
            SOCKET_HANDSHAKE_TIMEOUT,
            cancel,
        );
        let Some(mut websocket) = websocket else {
            continue;
        };
        if websocket.get_ref().set_nonblocking(false).is_err()
            || websocket
                .get_ref()
                .set_read_timeout(Some(RELAY_POLL))
                .is_err()
            || websocket
                .get_ref()
                .set_write_timeout(Some(Duration::from_millis(250)))
                .is_err()
        {
            let _ = websocket.close(None);
            continue;
        }

        let (inbound_tx, inbound_rx) = mpsc::sync_channel(INBOUND_CAPACITY);
        let (outbound_tx, outbound_rx) = mpsc::sync_channel(OUTBOUND_CAPACITY);
        let (acknowledgements_tx, acknowledgements_rx) = mpsc::sync_channel(OUTBOUND_CAPACITY);
        let peer_cancel = Arc::new(AtomicBool::new(false));
        let connection = WebConnection {
            inbound: inbound_rx,
            outbound: outbound_tx,
            acknowledgements: acknowledgements_rx,
            cancel: Arc::clone(&peer_cancel),
        };
        if events.try_send(WebEvent::Connected(connection)).is_err() {
            let _ = websocket.close(None);
            continue;
        }
        relay(
            &mut websocket,
            &inbound_tx,
            &outbound_rx,
            &acknowledgements_tx,
            &peer_cancel,
            cancel,
        );
    }
}

/// Runs the upgrade handshake under one overall deadline.
///
/// The gateway serves a single peer at a time, so a peer that trickles its
/// upgrade head must not hold the worker for longer than `timeout`. A
/// blocking socket's read timeout only bounds each individual read, and
/// tungstenite keeps reading while bytes keep arriving, so the handshake is
/// driven on a non-blocking socket and abandoned once the deadline passes.
fn accept_within<C: Callback>(
    stream: TcpStream,
    callback: C,
    config: WebSocketConfig,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Option<WebSocket<TcpStream>> {
    let deadline = Instant::now() + timeout;
    if stream.set_nonblocking(true).is_err() {
        let _ = stream.shutdown(Shutdown::Both);
        return None;
    }
    let mut handshake = ServerHandshake::start(stream, callback, Some(config)).handshake();
    loop {
        match handshake {
            Ok(websocket) => return Some(websocket),
            Err(HandshakeError::Interrupted(mid)) => {
                if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
                    let _ = mid.get_ref().get_ref().shutdown(Shutdown::Both);
                    return None;
                }
                thread::sleep(HANDSHAKE_POLL);
                handshake = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return None,
        }
    }
}

fn relay(
    websocket: &mut WebSocket<TcpStream>,
    inbound: &SyncSender<WireMessage>,
    outbound: &Receiver<Vec<u8>>,
    acknowledgements: &SyncSender<()>,
    peer_cancel: &Arc<AtomicBool>,
    gateway_cancel: &AtomicBool,
) {
    while !gateway_cancel.load(Ordering::Acquire) && !peer_cancel.load(Ordering::Acquire) {
        loop {
            match outbound.try_recv() {
                Ok(bytes) => {
                    let Ok(text) = String::from_utf8(bytes) else {
                        return;
                    };
                    if websocket.send(Message::text(text)).is_err() {
                        return;
                    }
                    if acknowledgements.try_send(()).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        match websocket.read() {
            Ok(Message::Text(text)) => {
                let line = text.as_str();
                let Ok(message) = decode_line(line) else {
                    return;
                };
                if inbound.try_send(message).is_err() {
                    return;
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) => {
                if websocket.flush().is_err() {
                    return;
                }
                return;
            }
            Err(WebSocketError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(RELAY_POLL);
            }
            Ok(Message::Binary(_) | Message::Frame(_)) | Err(_) => return,
        }
    }
    let _ = websocket.close(None);
}

/// Authorizes one upgrade request with the configured token.
///
/// Native clients present `Authorization: Bearer <token>`. Browsers cannot
/// set that header on a WebSocket, so they offer `freemix.control.v1` plus
/// exactly one `freemix.bearer.<base64url(token)>` subprotocol instead, and
/// must come from the gateway's own origin (the served tally page). Either
/// path compares the same secret in constant time; the selected subprotocol
/// is only ever `freemix.control.v1`.
#[allow(clippy::result_large_err)] // tungstenite handshake error response is externally sized
fn authorize_upgrade(
    headers: &HeaderMap,
    token: &WebToken,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    let offered = offered_subprotocols(headers);
    let control_offered = offered.contains(&CONTROL_SUBPROTOCOL);
    if let Some(authorization) = headers.get(AUTHORIZATION) {
        if !token.matches(Some(authorization)) {
            return Err(http_error(StatusCode::UNAUTHORIZED));
        }
    } else {
        if !same_origin(headers) {
            return Err(http_error(StatusCode::FORBIDDEN));
        }
        let mut credentials = offered
            .iter()
            .filter(|protocol| protocol.starts_with(BEARER_SUBPROTOCOL_PREFIX));
        let authorized = match (credentials.next(), credentials.next()) {
            (Some(credential), None) => token.matches_subprotocol(credential),
            _ => false,
        };
        if !control_offered || !authorized {
            return Err(http_error(StatusCode::UNAUTHORIZED));
        }
    }
    if control_offered {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(CONTROL_SUBPROTOCOL),
        );
    }
    Ok(response)
}

/// Every comma-separated subprotocol across all offered header lines.
fn offered_subprotocols(headers: &HeaderMap) -> Vec<&str> {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|protocol| !protocol.is_empty())
        .collect()
}

/// A browser upgrade must come from `http://<Host>`, the origin that served
/// the tally page. Requests without `Origin` are not browser-originated.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let (Ok(origin), Some(Ok(host))) =
        (origin.to_str(), headers.get(HOST).map(HeaderValue::to_str))
    else {
        return false;
    };
    let Some(host) = page::sanitized_host(host) else {
        return false;
    };
    origin
        .strip_prefix("http://")
        .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

fn http_error(status: StatusCode) -> ErrorResponse {
    Response::builder()
        .status(status)
        .body(None)
        .expect("static WebSocket HTTP error response is valid")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, atomic::AtomicBool, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use tungstenite::{
        handshake::server::Response,
        http::{HeaderMap, HeaderValue, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
    };

    use super::super::{WebEvent, WebToken, base64url};
    use super::{authorize_upgrade, worker_loop};

    const TOKEN: &[u8] = b"worker-test-token-0123456789abcdef";

    fn token() -> WebToken {
        WebToken::from_bytes(TOKEN).unwrap()
    }

    /// An accepted socket that arrives non-blocking, as every socket accepted
    /// from the non-blocking listener does on BSD-derived systems, must still
    /// complete a handshake whose request arrives after the worker picks it up.
    #[test]
    fn handshake_waits_for_a_late_request_on_a_nonblocking_accepted_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        accepted.set_nonblocking(true).unwrap();

        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_token = Arc::new(token());
        accepted_tx.send(accepted).unwrap();
        let worker = thread::spawn(move || {
            worker_loop(&accepted_rx, &events_tx, &worker_cancel, &worker_token);
        });
        // Let the worker reach the handshake before any request byte exists.
        thread::sleep(Duration::from_millis(100));

        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request = tungstenite::http::Request::builder()
            .uri(format!("ws://{address}/v1/control"))
            .header("Host", address.to_string())
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header(
                "Authorization",
                format!("Bearer {}", std::str::from_utf8(TOKEN).unwrap()),
            )
            .body(())
            .unwrap();
        let handshake = tungstenite::client::client(request, client);
        let connected = events_rx.recv_timeout(Duration::from_secs(2));

        cancel.store(true, std::sync::atomic::Ordering::Release);
        drop(accepted_tx);
        worker.join().unwrap();
        assert!(handshake.is_ok(), "the WebSocket handshake must complete");
        assert!(matches!(connected, Ok(WebEvent::Connected(_))));
    }

    /// A peer that trickles its upgrade head one byte at a time must lose the
    /// single gateway slot at the handshake deadline instead of holding it for
    /// as long as each read keeps arriving inside the per-read timeout.
    #[test]
    fn trickled_upgrade_head_is_dropped_at_the_handshake_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let (accepted, _) = listener.accept().unwrap();

        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_token = Arc::new(token());
        accepted_tx.send(accepted).unwrap();
        let worker = thread::spawn(move || {
            worker_loop(&accepted_rx, &events_tx, &worker_cancel, &worker_token);
        });

        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client.write_all(b"GET /v1/control HTTP/1.1\r\n").unwrap();
        let started = Instant::now();
        let mut closed_after = None;
        let mut buffer = [0_u8; 256];
        while started.elapsed() < Duration::from_secs(5) {
            if client.write_all(b"X").is_err() {
                closed_after = Some(started.elapsed());
                break;
            }
            // Any reply, EOF, or reset means the worker let go of the peer.
            match client.read(&mut buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Ok(_) | Err(_) => {
                    closed_after = Some(started.elapsed());
                    break;
                }
            }
            thread::sleep(Duration::from_millis(80));
        }

        cancel.store(true, std::sync::atomic::Ordering::Release);
        drop(accepted_tx);
        worker.join().unwrap();
        assert!(
            events_rx.try_recv().is_err(),
            "no connection may be admitted"
        );
        let closed_after = closed_after.expect("a trickling peer must be dropped");
        assert!(
            closed_after < Duration::from_secs(2),
            "the handshake deadline must bound the slot, held for {closed_after:?}"
        );
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn status(pairs: &[(&'static str, &str)]) -> Result<Option<String>, StatusCode> {
        authorize_upgrade(&headers(pairs), &token(), Response::new(()))
            .map(|response| {
                response
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .map(|value| value.to_str().unwrap().to_owned())
            })
            .map_err(|response| response.status())
    }

    #[test]
    fn upgrades_authorize_by_bearer_header_or_same_origin_subprotocol() {
        let bearer = format!("Bearer {}", std::str::from_utf8(TOKEN).unwrap());
        let credential = format!("freemix.bearer.{}", base64url(TOKEN));
        let offered = format!("freemix.control.v1, {credential}");
        let host = "127.0.0.1:4000";

        assert_eq!(status(&[("authorization", &bearer)]), Ok(None));
        assert_eq!(
            status(&[("authorization", "Bearer wrong-token-0123456789abcdef0123")]),
            Err(StatusCode::UNAUTHORIZED)
        );
        // A header credential is authoritative; a correct subprotocol cannot rescue it.
        assert_eq!(
            status(&[
                ("authorization", "Bearer nope"),
                ("sec-websocket-protocol", &offered)
            ]),
            Err(StatusCode::UNAUTHORIZED)
        );

        // Browser path: only the control subprotocol is ever selected.
        assert_eq!(
            status(&[
                ("host", host),
                ("origin", "http://127.0.0.1:4000"),
                ("sec-websocket-protocol", &offered),
            ]),
            Ok(Some("freemix.control.v1".to_owned()))
        );
        assert_eq!(
            status(&[
                ("sec-websocket-protocol", "freemix.control.v1"),
                ("sec-websocket-protocol", &credential),
            ]),
            Ok(Some("freemix.control.v1".to_owned()))
        );
        for rejected in [
            vec![("sec-websocket-protocol", credential.as_str())],
            vec![("sec-websocket-protocol", "freemix.control.v1")],
            vec![(
                "sec-websocket-protocol",
                "freemix.control.v1, freemix.bearer.d3Jvbmc",
            )],
            vec![(
                "sec-websocket-protocol",
                "freemix.control.v1, freemix.bearer.d3Jvbmc, freemix.bearer.x",
            )],
            vec![],
        ] {
            assert_eq!(status(&rejected), Err(StatusCode::UNAUTHORIZED));
        }
        let duplicate = format!("{offered}, {credential}");
        assert_eq!(
            status(&[("sec-websocket-protocol", &duplicate)]),
            Err(StatusCode::UNAUTHORIZED)
        );
        for origin in [
            "http://evil.example",
            "https://127.0.0.1:4000",
            "http://127.0.0.1:4001",
            "null",
        ] {
            assert_eq!(
                status(&[
                    ("host", host),
                    ("origin", origin),
                    ("sec-websocket-protocol", &offered),
                ]),
                Err(StatusCode::FORBIDDEN),
                "{origin}"
            );
        }
        assert_eq!(
            status(&[
                ("origin", "http://127.0.0.1:4000"),
                ("sec-websocket-protocol", &offered)
            ]),
            Err(StatusCode::FORBIDDEN),
            "an Origin without a Host cannot be verified"
        );
    }
}
