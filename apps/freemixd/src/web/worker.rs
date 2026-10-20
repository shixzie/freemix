use std::{
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustls::{ServerConfig, ServerConnection, StreamOwned};
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
    BEARER_SUBPROTOCOL_PREFIX, CONTROL_SUBPROTOCOL, ClientSlot, Gateway, INBOUND_CAPACITY,
    MAX_PENDING_CONNECTIONS, MAX_WEB_CLIENTS, OUTBOUND_CAPACITY, PATH, RELAY_POLL,
    SOCKET_HANDSHAKE_TIMEOUT, TLS_HANDSHAKE_TIMEOUT, WEBSOCKET_MAX_WRITE_BUFFER,
    WEBSOCKET_MESSAGE_LIMIT, WEBSOCKET_READ_BUFFER, WEBSOCKET_WRITE_BUFFER, WebAccess,
    WebConnection, WebEvent, WebIdentity, WireMessage,
    access::PairRefusal,
    assets, base64url_decode, decode_line,
    http::{self, MAX_REQUEST_BODY, MAX_REQUEST_HEAD, json_response, json_string},
    page,
};

/// Poll interval while a handshake or request head waits for more bytes.
const HANDSHAKE_POLL: Duration = Duration::from_millis(2);
/// How long writing one page or JSON answer may take.
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one relay write may block on a peer that stopped reading
/// before that peer (only) is dropped.
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
/// How long one relay read waits for the client before the relay writes the
/// records the scheduler queued meanwhile: the most latency the relay adds to
/// an outbound record, and how often a relay whose scheduler inbound is full
/// retries it.
const RELAY_WRITE_POLL: Duration = Duration::from_millis(5);
/// How often a relay checks that its device is still paired.
const REVOCATION_POLL: Duration = Duration::from_millis(250);

/// A gateway connection: plain TCP (loopback only) or TLS.
pub(super) enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(stream) => stream,
            Self::Tls(stream) => &stream.sock,
        }
    }

    /// Ends the connection, telling a TLS peer first.
    fn close(&mut self) {
        if let Self::Tls(stream) = self {
            stream.conn.send_close_notify();
            let _ = stream.conn.complete_io(&mut stream.sock);
        }
        let _ = self.tcp().shutdown(Shutdown::Both);
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buffer),
            Self::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buffer),
            Self::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

/// A stream whose first bytes (the request head already read) are replayed
/// to the WebSocket handshake.
pub(super) struct Prefixed {
    prefix: Vec<u8>,
    offset: usize,
    inner: Stream,
}

impl Prefixed {
    fn tcp(&self) -> &TcpStream {
        self.inner.tcp()
    }
}

impl Read for Prefixed {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.offset < self.prefix.len() {
            let copied = buffer.len().min(self.prefix.len() - self.offset);
            buffer[..copied].copy_from_slice(&self.prefix[self.offset..self.offset + copied]);
            self.offset += copied;
            if self.offset == self.prefix.len() {
                self.prefix = Vec::new();
                self.offset = 0;
            }
            return Ok(copied);
        }
        self.inner.read(buffer)
    }
}

impl Write for Prefixed {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Accepts gateway connections and serves each on its own thread.
///
/// At most [`MAX_WEB_CLIENTS`] connected clients plus
/// [`MAX_PENDING_CONNECTIONS`] connections still serving a page or
/// completing an upgrade hold a thread; a connection beyond that is closed
/// unanswered. Returns `false` when a connection thread panicked, after
/// joining every one of them once the gateway is cancelled.
pub(super) fn listener_loop(
    listener: &TcpListener,
    gateway: &Gateway,
    accepting: &Arc<AtomicBool>,
) -> bool {
    let mut connections: Vec<JoinHandle<()>> = Vec::new();
    let mut clean = true;
    while !gateway.cancel.load(Ordering::Acquire) {
        clean &= reap_finished(&mut connections);
        if !accepting.load(Ordering::Acquire) {
            thread::sleep(RELAY_POLL);
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if connections.len() >= MAX_WEB_CLIENTS + MAX_PENDING_CONNECTIONS {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let connection = gateway.clone();
                match thread::Builder::new()
                    .name("freemixd-web-connection".into())
                    .spawn(move || serve_connection(stream, &connection))
                {
                    Ok(handle) => connections.push(handle),
                    // The stream moved into the closure and is dropped (closed).
                    Err(_) => thread::sleep(RELAY_POLL),
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
    gateway.cancel.store(true, Ordering::Release);
    for handle in connections {
        clean &= handle.join().is_ok();
    }
    clean
}

/// Joins every finished connection thread; `false` if one panicked.
fn reap_finished(connections: &mut Vec<JoinHandle<()>>) -> bool {
    let mut clean = true;
    let mut index = 0;
    while index < connections.len() {
        if connections[index].is_finished() {
            clean &= connections.swap_remove(index).join().is_ok();
        } else {
            index += 1;
        }
    }
    clean
}

fn would_block(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Completes a TLS handshake on a non-blocking socket before `deadline`.
fn tls_accept(
    mut socket: TcpStream,
    config: &Arc<ServerConfig>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<StreamOwned<ServerConnection, TcpStream>> {
    let Ok(mut connection) = ServerConnection::new(Arc::clone(config)) else {
        let _ = socket.shutdown(Shutdown::Both);
        return None;
    };
    while connection.is_handshaking() {
        if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
            let _ = socket.shutdown(Shutdown::Both);
            return None;
        }
        match connection.complete_io(&mut socket) {
            Ok(_) => {}
            Err(error) if would_block(&error) => thread::sleep(HANDSHAKE_POLL),
            Err(_) => {
                // A plain-HTTP request or a broken client; rustls already
                // queued its alert.
                let _ = connection.write_tls(&mut socket);
                let _ = socket.shutdown(Shutdown::Both);
                return None;
            }
        }
    }
    Some(StreamOwned::new(connection, socket))
}

/// Reads until the end of the request head (and nothing is consumed past
/// the bytes returned). Returns every byte read and the head's length.
fn read_head(
    stream: &mut Stream,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<(Vec<u8>, usize)> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            return Some((buffer, end + 4));
        }
        if buffer.len() >= MAX_REQUEST_HEAD {
            return None;
        }
        let wanted = chunk.len().min(MAX_REQUEST_HEAD - buffer.len());
        match stream.read(&mut chunk[..wanted]) {
            Ok(0) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if would_block(&error) => {
                if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
                    return None;
                }
                thread::sleep(HANDSHAKE_POLL);
            }
            Err(_) => return None,
        }
    }
}

/// Reads a request body of exactly `length` bytes, some of which may
/// already be in `already`.
fn read_body(
    stream: &mut Stream,
    mut already: Vec<u8>,
    length: usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<Vec<u8>> {
    let mut chunk = [0; 16 * 1024];
    while already.len() < length {
        let wanted = chunk.len().min(length - already.len());
        match stream.read(&mut chunk[..wanted]) {
            Ok(0) => return None,
            Ok(read) => already.extend_from_slice(&chunk[..read]),
            Err(error) if would_block(&error) => {
                if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
                    return None;
                }
                thread::sleep(HANDSHAKE_POLL);
            }
            Err(_) => return None,
        }
    }
    already.truncate(length);
    Some(already)
}

/// Writes one complete answer and closes the connection.
fn respond(stream: &mut Stream, response: &[u8]) {
    respond_within(stream, response, RESPONSE_WRITE_TIMEOUT);
}

/// Writes one complete answer, each write blocking at most `timeout`, and
/// closes the connection.
fn respond_within(stream: &mut Stream, response: &[u8], timeout: Duration) {
    if stream.tcp().set_nonblocking(false).is_ok()
        && stream.tcp().set_write_timeout(Some(timeout)).is_ok()
        && stream
            .write_all(response)
            .and_then(|()| stream.flush())
            .is_ok()
    {
        stream.close();
    } else {
        let _ = stream.tcp().shutdown(Shutdown::Both);
    }
}

/// Serves one accepted connection: a page, a pairing or identity request,
/// or an authorized WebSocket upgrade that holds a client slot and is
/// relayed to the scheduler until either side closes.
#[allow(clippy::too_many_lines)] // one router over every gateway request
pub(super) fn serve_connection(socket: TcpStream, gateway: &Gateway) {
    let peer = socket.peer_addr().ok().map(|address| address.ip());
    // Reads and handshakes run on a non-blocking socket against deadlines,
    // so a peer that trickles bytes never holds a thread past them.
    if socket.set_nonblocking(true).is_err() {
        let _ = socket.shutdown(Shutdown::Both);
        return;
    }
    let tls = gateway.tls.is_some();
    let deadline = Instant::now()
        + if tls {
            TLS_HANDSHAKE_TIMEOUT
        } else {
            SOCKET_HANDSHAKE_TIMEOUT
        };
    let mut stream = match &gateway.tls {
        Some(config) => match tls_accept(socket, config, deadline, &gateway.cancel) {
            Some(stream) => Stream::Tls(Box::new(stream)),
            None => return,
        },
        None => Stream::Plain(socket),
    };
    let Some((buffer, head_length)) = read_head(&mut stream, deadline, &gateway.cancel) else {
        stream.close();
        return;
    };
    let Some(request) = http::Request::parse(&buffer[..head_length]) else {
        respond(&mut stream, &http::empty_response("400 Bad Request", ""));
        return;
    };
    // DNS-rebinding defence: only addresses, `localhost`, and the
    // operator's `--web-host` names are served.
    if !request
        .header("host")
        .is_some_and(|host| http::allowed_host(host, &gateway.hosts))
    {
        respond(
            &mut stream,
            &http::empty_response("421 Misdirected Request", ""),
        );
        return;
    }
    if request.path == page::INDEX_PATH || request.path == fm_protocol::WEB_CA_PATH {
        let ca = gateway.access.tls().and_then(|tls| tls.ca());
        let body = request.method == "GET";
        let response = match (request.method.as_str(), request.path.as_str(), ca) {
            ("GET" | "HEAD", page::INDEX_PATH, _) => page::page_response(
                request.header("host"),
                tls,
                &page::index_page(ca.is_some()),
                body,
            ),
            ("GET" | "HEAD", _, Some(ca)) => page::ca_response(ca.der().as_ref(), body),
            ("GET" | "HEAD", _, None) => http::empty_response("404 Not Found", ""),
            _ => http::empty_response("405 Method Not Allowed", "Allow: GET, HEAD\r\n"),
        };
        respond(&mut stream, &response);
        return;
    }
    if let Some(page) = page::page_for(&request.path) {
        let response = match request.method.as_str() {
            "GET" => page::page_response(request.header("host"), tls, page, true),
            "HEAD" => page::page_response(request.header("host"), tls, page, false),
            _ => http::empty_response("405 Method Not Allowed", "Allow: GET, HEAD\r\n"),
        };
        respond(&mut stream, &response);
        return;
    }
    match request.path.as_str() {
        "/v1/pair" => {
            if request.method != "POST" {
                respond(
                    &mut stream,
                    &http::empty_response("405 Method Not Allowed", "Allow: POST\r\n"),
                );
                return;
            }
            let response = match request.content_length() {
                Some(length) if length <= MAX_REQUEST_BODY => read_body(
                    &mut stream,
                    buffer[head_length..].to_vec(),
                    length,
                    deadline,
                    &gateway.cancel,
                )
                .map_or_else(bad_request, |body| {
                    pair(&request, &body, peer, tls, &gateway.access)
                }),
                Some(_) => json_response("413 Content Too Large", r#"{"error":"bad_request"}"#, ""),
                None => bad_request(),
            };
            respond(&mut stream, &response);
        }
        "/v1/whoami" => {
            let response = if request.method == "GET" {
                whoami(&request, &gateway.access)
            } else {
                http::empty_response("405 Method Not Allowed", "Allow: GET\r\n")
            };
            respond(&mut stream, &response);
        }
        assets::IMAGES_PATH | assets::IMAGE_PATH => {
            let response =
                serve_assets(&mut stream, &request, &buffer[head_length..], tls, gateway);
            respond_within(&mut stream, &response, assets::TRANSFER_TIMEOUT);
        }
        _ if request.is_upgrade() => upgrade(stream, buffer, gateway),
        _ => respond(&mut stream, &http::empty_response("404 Not Found", "")),
    }
}

/// The title editor's image assets (see [`assets`]): `GET` and `POST
/// /v1/assets/images`, `GET` and `HEAD /v1/assets/image`.
fn serve_assets(
    stream: &mut Stream,
    request: &http::Request,
    already: &[u8],
    tls: bool,
    gateway: &Gateway,
) -> Vec<u8> {
    let allowed = match request.path.as_str() {
        assets::IMAGES_PATH => matches!(request.method.as_str(), "GET" | "POST"),
        _ => matches!(request.method.as_str(), "GET" | "HEAD"),
    };
    if !allowed {
        return http::empty_response(
            "405 Method Not Allowed",
            if request.path == assets::IMAGES_PATH {
                "Allow: GET, POST\r\n"
            } else {
                "Allow: GET, HEAD\r\n"
            },
        );
    }
    if let Err(refusal) = assets::authorize(request, tls, &gateway.access) {
        return refusal;
    }
    let Some(root) = gateway.assets_root.as_deref() else {
        return assets::unavailable();
    };
    match (request.path.as_str(), request.method.as_str()) {
        (assets::IMAGE_PATH, method) => assets::image_response(root, request, method == "GET"),
        (_, "GET") => assets::list_response(root),
        _ => {
            let (kind, length) = match assets::upload_head(request) {
                Ok(head) => head,
                Err(refusal) => return refusal,
            };
            let Some(_permit) = assets::UploadPermit::acquire() else {
                return json_response(
                    "503 Service Unavailable",
                    r#"{"error":"busy"}"#,
                    "Retry-After: 1\r\n",
                );
            };
            let deadline = Instant::now() + assets::TRANSFER_TIMEOUT;
            let Some(body) = read_body(stream, already.to_vec(), length, deadline, &gateway.cancel)
            else {
                return bad_request();
            };
            assets::store_upload(root, request.query_param("name").as_deref(), kind, &body)
        }
    }
}

fn bad_request() -> Vec<u8> {
    json_response("400 Bad Request", r#"{"error":"bad_request"}"#, "")
}

/// `POST /v1/pair`: redeems a pairing code for a device token.
///
/// Only the page the gateway served may pair (`Origin` must match), and
/// every attempt counts against the client address's budget.
fn pair(
    request: &http::Request,
    body: &[u8],
    peer: Option<IpAddr>,
    tls: bool,
    access: &WebAccess,
) -> Vec<u8> {
    if !http::same_origin(request, tls, true) {
        return json_response("403 Forbidden", r#"{"error":"forbidden"}"#, "");
    }
    let (Some(peer), Ok(body)) = (peer, std::str::from_utf8(body)) else {
        return bad_request();
    };
    let Some(code) = http::form_field(body, "code").filter(|code| {
        (1..=fm_protocol::MAX_WEB_LINK_CODE_BYTES).contains(&code.len())
            && code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }) else {
        return bad_request();
    };
    let name = http::form_field(body, "name").unwrap_or_default();
    match access.redeem(&code, &name, peer) {
        Ok(device) => json_response(
            "200 OK",
            &format!(
                r#"{{"token":{},"role":"{}","device":{},"name":{}}}"#,
                json_string(&device.token),
                device.role.token(),
                json_string(&device.id),
                json_string(&device.name),
            ),
            "",
        ),
        Err(PairRefusal::BadRequest) => bad_request(),
        Err(PairRefusal::Invalid) => {
            json_response("401 Unauthorized", r#"{"error":"invalid"}"#, "")
        }
        Err(PairRefusal::Expired) => json_response("410 Gone", r#"{"error":"expired"}"#, ""),
        Err(PairRefusal::RateLimited { retry_after_s }) => json_response(
            "429 Too Many Requests",
            &format!(r#"{{"error":"rate_limited","retry_after_s":{retry_after_s}}}"#),
            &format!("Retry-After: {retry_after_s}\r\n"),
        ),
        Err(PairRefusal::Unavailable) => {
            json_response("503 Service Unavailable", r#"{"error":"unavailable"}"#, "")
        }
    }
}

/// `GET /v1/whoami`: what the presented token is, so a page can tell a
/// revoked device from a gateway that is merely full.
fn whoami(request: &http::Request, access: &WebAccess) -> Vec<u8> {
    let identity = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| access.authenticate(token.as_bytes()));
    match identity {
        Some(WebIdentity::StaticToken) => {
            json_response("200 OK", r#"{"kind":"token","role":"admin"}"#, "")
        }
        Some(identity @ WebIdentity::Device { .. }) => {
            let WebIdentity::Device { id, .. } = &identity else {
                unreachable!("matched a device");
            };
            match access.describe(&identity) {
                Some((name, role)) => json_response(
                    "200 OK",
                    &format!(
                        r#"{{"kind":"device","device":{},"name":{},"role":"{}"}}"#,
                        json_string(id),
                        json_string(&name),
                        role.token()
                    ),
                    "",
                ),
                None => json_response("401 Unauthorized", r#"{"error":"unauthorized"}"#, ""),
            }
        }
        None => json_response("401 Unauthorized", r#"{"error":"unauthorized"}"#, ""),
    }
}

/// Completes an authorized upgrade and relays the session.
#[allow(clippy::result_large_err)] // tungstenite handshake error response is externally sized
fn upgrade(stream: Stream, head: Vec<u8>, gateway: &Gateway) {
    let config = WebSocketConfig::default()
        .read_buffer_size(WEBSOCKET_READ_BUFFER)
        .write_buffer_size(WEBSOCKET_WRITE_BUFFER)
        .max_write_buffer_size(WEBSOCKET_MAX_WRITE_BUFFER)
        .max_message_size(Some(WEBSOCKET_MESSAGE_LIMIT))
        .max_frame_size(Some(WEBSOCKET_MESSAGE_LIMIT))
        .accept_unmasked_frames(false);
    let tls = gateway.tls.is_some();
    // The slot is taken only by an authorized upgrade, so an unauthorized
    // peer learns nothing about capacity and never holds one.
    let mut slot = None;
    let mut identity = None;
    let websocket = accept_within(
        Prefixed {
            prefix: head,
            offset: 0,
            inner: stream,
        },
        |request: &Request, response: Response| {
            if request
                .uri()
                .path_and_query()
                .map(tungstenite::http::uri::PathAndQuery::as_str)
                != Some(PATH)
            {
                return Err(http_error(StatusCode::NOT_FOUND));
            }
            let (response, who) =
                authorize_upgrade(request.headers(), &gateway.access, tls, response)?;
            slot = Some(
                ClientSlot::acquire(&gateway.clients)
                    .ok_or_else(|| http_error(StatusCode::SERVICE_UNAVAILABLE))?,
            );
            identity = Some(who);
            Ok(response)
        },
        config,
        SOCKET_HANDSHAKE_TIMEOUT,
        &gateway.cancel,
    );
    let (Some(mut websocket), Some(slot), Some(identity)) = (websocket, slot, identity) else {
        return;
    };
    if websocket.get_ref().tcp().set_nonblocking(false).is_err()
        || websocket
            .get_ref()
            .tcp()
            .set_read_timeout(Some(RELAY_WRITE_POLL))
            .is_err()
        || websocket
            .get_ref()
            .tcp()
            .set_write_timeout(Some(RELAY_WRITE_TIMEOUT))
            .is_err()
        || websocket.get_ref().tcp().set_nodelay(true).is_err()
    {
        let _ = websocket.close(None);
        return;
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
        identity: identity.clone(),
        slot,
    };
    if gateway
        .events
        .try_send(WebEvent::Connected(connection))
        .is_err()
    {
        let _ = websocket.close(None);
        return;
    }
    relay(
        &mut websocket,
        &Channels {
            inbound: &inbound_tx,
            outbound: &outbound_rx,
            acknowledgements: &acknowledgements_tx,
        },
        &peer_cancel,
        &gateway.cancel,
        |at| {
            // A revoked device loses its live sessions within a poll.
            if at.elapsed() >= REVOCATION_POLL {
                *at = Instant::now();
                return gateway.access.is_current(&identity);
            }
            true
        },
    );
}

/// Runs the upgrade handshake under one overall deadline.
///
/// A peer that trickles its upgrade head must not hold a connection thread
/// (one of the bounded pending connections) for longer than `timeout`. A
/// blocking socket's read timeout only bounds each individual read, and
/// tungstenite keeps reading while bytes keep arriving, so the handshake is
/// driven on a non-blocking socket and abandoned once the deadline passes.
fn accept_within<C: Callback>(
    stream: Prefixed,
    callback: C,
    config: WebSocketConfig,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Option<WebSocket<Prefixed>> {
    let deadline = Instant::now() + timeout;
    if stream.tcp().set_nonblocking(true).is_err() {
        let _ = stream.tcp().shutdown(Shutdown::Both);
        return None;
    }
    let mut handshake = ServerHandshake::start(stream, callback, Some(config)).handshake();
    loop {
        match handshake {
            Ok(websocket) => return Some(websocket),
            Err(HandshakeError::Interrupted(mid)) => {
                if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
                    let _ = mid.get_ref().get_ref().tcp().shutdown(Shutdown::Both);
                    return None;
                }
                thread::sleep(HANDSHAKE_POLL);
                handshake = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return None,
        }
    }
}

/// The scheduler side of one relayed session.
struct Channels<'a> {
    inbound: &'a SyncSender<WireMessage>,
    outbound: &'a Receiver<Vec<u8>>,
    acknowledgements: &'a SyncSender<()>,
}

fn relay(
    websocket: &mut WebSocket<Prefixed>,
    channels: &Channels<'_>,
    peer_cancel: &Arc<AtomicBool>,
    gateway_cancel: &AtomicBool,
    mut still_authorized: impl FnMut(&mut Instant) -> bool,
) {
    // A message the scheduler has no room for yet. While one waits, the
    // relay stops reading, so a client that sends faster than its session is
    // served meets TCP backpressure instead of being disconnected.
    let mut waiting: Option<WireMessage> = None;
    let mut checked = Instant::now();
    while !gateway_cancel.load(Ordering::Acquire) && !peer_cancel.load(Ordering::Acquire) {
        if !still_authorized(&mut checked) {
            break;
        }
        if let Some(message) = waiting.take() {
            match channels.inbound.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(message)) => waiting = Some(message),
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        loop {
            match channels.outbound.try_recv() {
                Ok(bytes) => {
                    let Ok(text) = String::from_utf8(bytes) else {
                        return;
                    };
                    if websocket.send(Message::text(text)).is_err() {
                        return;
                    }
                    if channels.acknowledgements.try_send(()).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if waiting.is_some() {
            thread::sleep(RELAY_WRITE_POLL);
            continue;
        }
        match websocket.read() {
            Ok(Message::Text(text)) => {
                let line = text.as_str();
                let Ok(message) = decode_line(line) else {
                    return;
                };
                match channels.inbound.try_send(message) {
                    Ok(()) => {}
                    Err(TrySendError::Full(message)) => waiting = Some(message),
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) => {
                if websocket.flush().is_err() {
                    return;
                }
                return;
            }
            Err(WebSocketError::Io(error)) if would_block(&error) => {
                // The read already waited its timeout; records the scheduler
                // queued meanwhile are written on the next iteration.
            }
            Ok(Message::Binary(_) | Message::Frame(_)) | Err(_) => return,
        }
    }
    let _ = websocket.close(None);
    let _ = websocket.flush();
}

/// The token a request presents: `Authorization: Bearer <token>` from a
/// native client, or exactly one `freemix.bearer.<base64url(token)>`
/// subprotocol from a browser.
enum Presented {
    Header(Vec<u8>),
    Subprotocol(Vec<u8>),
    Malformed,
    Missing,
}

fn presented_token(headers: &HeaderMap, offered: &[&str]) -> Presented {
    if let Some(authorization) = headers.get(AUTHORIZATION) {
        return authorization
            .as_bytes()
            .strip_prefix(b"Bearer ")
            .map_or(Presented::Malformed, |token| {
                Presented::Header(token.to_vec())
            });
    }
    let mut credentials = offered
        .iter()
        .filter_map(|protocol| protocol.strip_prefix(BEARER_SUBPROTOCOL_PREFIX));
    match (credentials.next(), credentials.next()) {
        (Some(encoded), None) => {
            base64url_decode(encoded).map_or(Presented::Malformed, Presented::Subprotocol)
        }
        (None, _) => Presented::Missing,
        (Some(_), Some(_)) => Presented::Malformed,
    }
}

/// Authorizes one upgrade request.
///
/// Native clients present `Authorization: Bearer <token>`. Browsers cannot
/// set that header on a WebSocket, so they offer `freemix.control.v1` plus
/// exactly one `freemix.bearer.<base64url(token)>` subprotocol instead, and
/// must come from the gateway's own origin (the served page). The token is
/// the operator's `FREEMIXD_WEB_TOKEN` (compared in constant time) or a
/// paired device's (compared by digest); the selected subprotocol is only
/// ever `freemix.control.v1`.
#[allow(clippy::result_large_err)] // tungstenite handshake error response is externally sized
fn authorize_upgrade(
    headers: &HeaderMap,
    access: &WebAccess,
    tls: bool,
    mut response: Response,
) -> Result<(Response, WebIdentity), ErrorResponse> {
    let offered = offered_subprotocols(headers);
    let control_offered = offered.contains(&CONTROL_SUBPROTOCOL);
    let token = match presented_token(headers, &offered) {
        Presented::Header(token) => token,
        Presented::Subprotocol(token) => {
            if !same_origin(headers, tls) {
                return Err(http_error(StatusCode::FORBIDDEN));
            }
            if !control_offered {
                return Err(http_error(StatusCode::UNAUTHORIZED));
            }
            token
        }
        Presented::Malformed | Presented::Missing => {
            if headers.get(AUTHORIZATION).is_none() && !same_origin(headers, tls) {
                return Err(http_error(StatusCode::FORBIDDEN));
            }
            return Err(http_error(StatusCode::UNAUTHORIZED));
        }
    };
    let identity = access
        .authenticate(&token)
        .ok_or_else(|| http_error(StatusCode::UNAUTHORIZED))?;
    if control_offered {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(CONTROL_SUBPROTOCOL),
        );
    }
    Ok((response, identity))
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

/// A browser upgrade must come from `<scheme>://<Host>`, the origin that
/// served the page. Requests without `Origin` are not browser-originated.
fn same_origin(headers: &HeaderMap, tls: bool) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let (Ok(origin), Some(Ok(host))) =
        (origin.to_str(), headers.get(HOST).map(HeaderValue::to_str))
    else {
        return false;
    };
    let Some(host) = http::sanitized_host(host) else {
        return false;
    };
    let scheme = if tls { "https://" } else { "http://" };
    origin
        .strip_prefix(scheme)
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
        net::{SocketAddr, TcpListener, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use tungstenite::{
        HandshakeError,
        handshake::server::Response,
        http::{HeaderMap, HeaderValue, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
    };

    use super::super::{Gateway, MAX_WEB_CLIENTS, WebAccess, WebConnection, WebEvent, base64url};
    use super::{authorize_upgrade, serve_connection};

    const TOKEN: &[u8] = b"worker-test-token-0123456789abcdef";

    fn access() -> Arc<WebAccess> {
        Arc::new(
            WebAccess::open(
                "http://127.0.0.1:4000".into(),
                None,
                Some(TOKEN.to_vec()),
                None,
            )
            .unwrap(),
        )
    }

    fn gateway() -> (Gateway, mpsc::Receiver<WebEvent>) {
        let (events, events_rx) = mpsc::sync_channel(MAX_WEB_CLIENTS);
        (
            Gateway {
                events,
                cancel: Arc::new(AtomicBool::new(false)),
                access: access(),
                tls: None,
                hosts: Arc::new(Vec::new()),
                clients: Arc::new(AtomicUsize::new(0)),
                assets_root: None,
            },
            events_rx,
        )
    }

    fn upgrade_request(address: SocketAddr) -> tungstenite::http::Request<()> {
        tungstenite::http::Request::builder()
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
            .unwrap()
    }

    type Client = tungstenite::WebSocket<TcpStream>;

    /// Accepts one connection on `listener` and serves it on a thread.
    fn accept(listener: &TcpListener, gateway: &Gateway) -> (TcpStream, JoinHandle<()>) {
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let gateway = gateway.clone();
        (
            client,
            thread::spawn(move || serve_connection(accepted, &gateway)),
        )
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

        let (gateway, events_rx) = gateway();
        let cancel = Arc::clone(&gateway.cancel);
        let worker = thread::spawn(move || serve_connection(accepted, &gateway));
        // Let the worker reach the handshake before any request byte exists.
        thread::sleep(Duration::from_millis(100));

        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let handshake = tungstenite::client::client(upgrade_request(address), client);
        let connected = events_rx.recv_timeout(Duration::from_secs(2));

        cancel.store(true, Ordering::Release);
        worker.join().unwrap();
        assert!(handshake.is_ok(), "the WebSocket handshake must complete");
        assert!(matches!(connected, Ok(WebEvent::Connected(_))));
    }

    /// A peer that trickles its upgrade head one byte at a time must lose its
    /// pending-connection thread at the handshake deadline instead of holding
    /// it for as long as each read keeps arriving inside the per-read timeout.
    #[test]
    fn trickled_upgrade_head_is_dropped_at_the_handshake_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (gateway, events_rx) = gateway();
        let (mut client, worker) = accept(&listener, &gateway);

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

        gateway.cancel.store(true, Ordering::Release);
        worker.join().unwrap();
        assert!(
            events_rx.try_recv().is_err(),
            "no connection may be admitted"
        );
        assert_eq!(gateway.clients.load(Ordering::Acquire), 0);
        let closed_after = closed_after.expect("a trickling peer must be dropped");
        assert!(
            closed_after < Duration::from_secs(2),
            "the handshake deadline must bound the connection, held for {closed_after:?}"
        );
    }

    /// Every connected client holds one slot until the scheduler drops its
    /// session; an authorized upgrade beyond the bound is refused with 503,
    /// and a released slot admits the next client.
    #[test]
    fn upgrades_beyond_the_client_bound_are_refused_until_a_slot_frees() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (gateway, events_rx) = gateway();
        let mut sessions = Vec::new();
        let mut workers = Vec::new();
        for _ in 0..MAX_WEB_CLIENTS {
            let (client, worker) = accept(&listener, &gateway);
            let (socket, _) = tungstenite::client::client(upgrade_request(address), client)
                .expect("a client within the bound is admitted");
            let Ok(WebEvent::Connected(connection)) =
                events_rx.recv_timeout(Duration::from_secs(2))
            else {
                panic!("an admitted client reaches the scheduler");
            };
            sessions.push((socket, connection));
            workers.push(worker);
        }
        assert_eq!(gateway.clients.load(Ordering::Acquire), MAX_WEB_CLIENTS);

        let (client, worker) = accept(&listener, &gateway);
        match tungstenite::client::client(upgrade_request(address), client) {
            Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
            other => panic!("expected 503, got {other:?}"),
        }
        worker.join().unwrap();
        assert!(events_rx.try_recv().is_err());

        // The scheduler drops one session: its slot admits the next client.
        let (_socket, connection) = sessions.pop().unwrap();
        connection.cancel.store(true, Ordering::Release);
        drop(connection);
        let (client, worker) = accept(&listener, &gateway);
        assert!(
            tungstenite::client::client(upgrade_request(address), client).is_ok(),
            "a freed slot admits the next client"
        );
        workers.push(worker);
        let admitted = events_rx.recv_timeout(Duration::from_secs(2));
        assert!(matches!(admitted, Ok(WebEvent::Connected(_))));

        gateway.cancel.store(true, Ordering::Release);
        for worker in workers {
            worker.join().unwrap();
        }
        drop(sessions);
        drop(admitted);
        assert_eq!(gateway.clients.load(Ordering::Acquire), 0);
    }

    /// A client that stops reading is dropped by its own relay without
    /// delaying another client's relay.
    #[test]
    fn a_client_that_stops_reading_never_delays_another() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (gateway, events_rx) = gateway();
        let open = || -> (Client, WebConnection, JoinHandle<()>) {
            let (client, worker) = accept(&listener, &gateway);
            let (socket, _) =
                tungstenite::client::client(upgrade_request(address), client).unwrap();
            let Ok(WebEvent::Connected(connection)) =
                events_rx.recv_timeout(Duration::from_secs(2))
            else {
                panic!("an admitted client reaches the scheduler");
            };
            (socket, connection, worker)
        };
        let (_stalled_socket, stalled, stalled_worker) = open();
        let (mut prompt_socket, prompt, prompt_worker) = open();

        // Flood the client that never reads far past every socket buffer, as
        // fast as its relay takes records, while the prompt client is served.
        let flood = thread::spawn(move || {
            let record = vec![b'x'; 256 * 1024];
            let mut sent = 0_usize;
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(10) {
                if stalled.outbound.try_send(record.clone()).is_ok() {
                    sent += 1;
                }
                if let Err(mpsc::TryRecvError::Disconnected) = stalled.acknowledgements.try_recv() {
                    return (sent, started.elapsed());
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("a client that never reads was never dropped ({sent} records queued)");
        });
        for index in 0..20 {
            let line = format!("line-{index}");
            let sent = Instant::now();
            prompt.outbound.send(line.clone().into_bytes()).unwrap();
            let tungstenite::Message::Text(text) = prompt_socket.read().unwrap() else {
                panic!("expected a text record");
            };
            assert_eq!(text.as_str(), line);
            assert!(
                sent.elapsed() < Duration::from_millis(500),
                "a stalled client delayed another by {:?}",
                sent.elapsed()
            );
            prompt
                .acknowledgements
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        let (records, dropped_after) = flood.join().unwrap();
        // One 256 KiB record can already fill the socket buffers and block
        // the relay's write past its deadline, so a single record is enough
        // to get the stalled client dropped on a loaded host.
        assert!(records >= 1, "the stalled client took {records} records");
        assert!(dropped_after < Duration::from_secs(10));
        stalled_worker.join().unwrap();

        gateway.cancel.store(true, Ordering::Release);
        drop(prompt);
        prompt_worker.join().unwrap();
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn status(pairs: &[(&'static str, &str)]) -> Result<Option<String>, StatusCode> {
        authorize_upgrade(&headers(pairs), &access(), false, Response::new(()))
            .map(|(response, _)| {
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
