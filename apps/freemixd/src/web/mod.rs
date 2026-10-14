use std::{
    env,
    net::{SocketAddr, TcpListener},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fm_protocol::{MAX_LINE_BYTES, WireMessage, decode_line};
use tungstenite::http::HeaderValue;

use super::AppResult;

const PATH: &str = "/v1/control";
const TOKEN_MIN_BYTES: usize = 32;
const TOKEN_MAX_BYTES: usize = 256;
const SOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(500);
const RELAY_POLL: Duration = Duration::from_millis(25);
const WEBSOCKET_READ_BUFFER: usize = 8 * 1024;
const WEBSOCKET_WRITE_BUFFER: usize = 8 * 1024;
const WEBSOCKET_MAX_WRITE_BUFFER: usize = 2 * MAX_LINE_BYTES;
const WEBSOCKET_MESSAGE_LIMIT: usize = MAX_LINE_BYTES;

const INBOUND_CAPACITY: usize = 8;
const OUTBOUND_CAPACITY: usize = 32;

pub(super) struct WebGateway {
    address: SocketAddr,
    events: Receiver<WebEvent>,
    cancel: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    worker_thread: Option<JoinHandle<()>>,
}

pub(super) enum WebEvent {
    Connected(WebConnection),
}

pub(super) struct WebConnection {
    pub(super) inbound: Receiver<WireMessage>,
    pub(super) outbound: SyncSender<Vec<u8>>,
    pub(super) acknowledgements: Receiver<()>,
    pub(super) cancel: Arc<AtomicBool>,
}

/// Subprotocol a browser offers, and the gateway selects, for control.
const CONTROL_SUBPROTOCOL: &str = "freemix.control.v1";
/// Browsers cannot set `Authorization` on a WebSocket, so the page presents
/// the same token as `freemix.bearer.<unpadded base64url(token)>` in its
/// offered subprotocol list. The gateway never selects or echoes it.
const BEARER_SUBPROTOCOL_PREFIX: &str = "freemix.bearer.";
const BEARER_SUBPROTOCOL_MAX_BYTES: usize =
    BEARER_SUBPROTOCOL_PREFIX.len() + (TOKEN_MAX_BYTES * 4).div_ceil(3);

struct WebToken {
    bytes: [u8; TOKEN_MAX_BYTES],
    length: usize,
    subprotocol: [u8; BEARER_SUBPROTOCOL_MAX_BYTES],
    subprotocol_length: usize,
}

impl WebToken {
    fn from_environment() -> AppResult<Self> {
        let value = env::var("FREEMIXD_WEB_TOKEN")
            .map_err(|_| "FREEMIXD_WEB_TOKEN is required when --web-listen is enabled")?;
        Self::from_bytes(value.as_bytes())
            .ok_or_else(|| "FREEMIXD_WEB_TOKEN must be 32..=256 ASCII graphic bytes".into())
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if !(TOKEN_MIN_BYTES..=TOKEN_MAX_BYTES).contains(&bytes.len())
            || !bytes.iter().all(u8::is_ascii_graphic)
        {
            return None;
        }
        let mut token = [0; TOKEN_MAX_BYTES];
        token[..bytes.len()].copy_from_slice(bytes);
        let encoded = format!("{BEARER_SUBPROTOCOL_PREFIX}{}", base64url(bytes));
        let mut subprotocol = [0; BEARER_SUBPROTOCOL_MAX_BYTES];
        subprotocol[..encoded.len()].copy_from_slice(encoded.as_bytes());
        Some(Self {
            bytes: token,
            length: bytes.len(),
            subprotocol,
            subprotocol_length: encoded.len(),
        })
    }

    fn matches(&self, authorization: Option<&HeaderValue>) -> bool {
        let presented = authorization
            .map(HeaderValue::as_bytes)
            .and_then(|value| value.strip_prefix(b"Bearer "))
            .unwrap_or_default();
        constant_time_eq(&self.bytes, self.length, presented)
    }

    /// Compares one offered `freemix.bearer.*` subprotocol in constant time.
    fn matches_subprotocol(&self, presented: &str) -> bool {
        constant_time_eq(
            &self.subprotocol,
            self.subprotocol_length,
            presented.as_bytes(),
        )
    }
}

fn constant_time_eq<const N: usize>(expected: &[u8; N], length: usize, presented: &[u8]) -> bool {
    let mut candidate = [0; N];
    let copied = presented.len().min(N);
    candidate[..copied].copy_from_slice(&presented[..copied]);
    let mut difference = length ^ presented.len();
    for (expected, actual) in expected.iter().zip(candidate) {
        difference |= usize::from(*expected ^ actual);
    }
    difference == 0
}

/// Unpadded RFC 4648 base64url.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            let sextet = (value >> (18 - 6 * index)) & 0x3f;
            encoded.push(char::from(ALPHABET[sextet as usize]));
        }
    }
    encoded
}

impl WebGateway {
    pub(super) fn bind(listener: TcpListener) -> AppResult<Self> {
        let address = listener.local_addr()?;
        let token = Arc::new(WebToken::from_environment()?);
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let accepting = Arc::new(AtomicBool::new(false));

        let listener_cancel = Arc::clone(&cancel);
        let listener_accepting = Arc::clone(&accepting);
        let listener_thread = thread::Builder::new()
            .name("freemixd-web-listener".into())
            .spawn(move || {
                listener_loop(
                    &listener,
                    &accepted_tx,
                    &listener_cancel,
                    &listener_accepting,
                );
            })?;

        let worker_cancel = Arc::clone(&cancel);
        let worker_token = Arc::clone(&token);
        let worker_thread = match thread::Builder::new()
            .name("freemixd-web-worker".into())
            .spawn(move || worker_loop(&accepted_rx, &events_tx, &worker_cancel, &worker_token))
        {
            Ok(handle) => handle,
            Err(error) => {
                cancel.store(true, Ordering::Release);
                let _ = listener_thread.join();
                return Err(error.into());
            }
        };

        Ok(Self {
            address,
            events: events_rx,
            cancel,
            accepting,
            listener_thread: Some(listener_thread),
            worker_thread: Some(worker_thread),
        })
    }

    pub(super) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(super) fn start_accepting(&self) {
        self.accepting.store(true, Ordering::Release);
    }

    pub(super) fn try_event(&self) -> Result<Option<WebEvent>, ()> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    pub(super) fn shutdown(mut self) -> AppResult<()> {
        self.cancel.store(true, Ordering::Release);
        let mut panic = None;
        for handle in [&mut self.listener_thread, &mut self.worker_thread] {
            if let Some(handle) = handle.take()
                && handle.join().is_err()
            {
                panic = Some("freemixd WebSocket gateway thread panicked");
            }
        }
        panic.map_or(Ok(()), |message| Err(message.into()))
    }
}

impl Drop for WebGateway {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.worker_thread.take() {
            let _ = handle.join();
        }
    }
}

mod page;
mod worker;
use worker::{listener_loop, worker_loop};

#[cfg(test)]
mod tests {
    use super::{WebToken, base64url};

    #[test]
    fn base64url_is_unpadded_rfc4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
            ("\u{3f}\u{3f}>", "Pz8-"),
            ("??/", "Pz8v"),
            ("\u{7e}\u{7e}\u{7f}", "fn5_"),
        ] {
            assert_eq!(base64url(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn subprotocol_credentials_match_only_the_exact_encoded_token() {
        let token = WebToken::from_bytes(b"subprotocol-token/+=0123456789abcdef").unwrap();
        let encoded = format!(
            "freemix.bearer.{}",
            base64url(b"subprotocol-token/+=0123456789abcdef")
        );
        assert!(token.matches_subprotocol(&encoded));
        assert!(!token.matches_subprotocol(&encoded[..encoded.len() - 1]));
        assert!(!token.matches_subprotocol(&format!("{encoded}A")));
        assert!(!token.matches_subprotocol("freemix.bearer."));
        assert!(!token.matches_subprotocol(&"x".repeat(4096)));
        assert!(WebToken::from_bytes(b"short").is_none());
        assert!(WebToken::from_bytes(&[b' '; 40]).is_none());
        let longest = WebToken::from_bytes(&[b'~'; 256]).unwrap();
        assert!(
            longest.matches_subprotocol(&format!("freemix.bearer.{}", base64url(&[b'~'; 256])))
        );
    }
}
