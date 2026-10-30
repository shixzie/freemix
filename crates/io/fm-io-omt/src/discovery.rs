//! OMT 1.0 discovery: `_omt._tcp` names and the TCP discovery server.
//!
//! Senders register an [`OmtAddress`] with a discovery server (port 6399 by
//! default); the server replaces the addresses the sender listed with the
//! address it sees the connection come from, repeats every registration to
//! every client, and sends a removal when a client leaves. DNS-SD advertises
//! the same records as `HOSTNAME (Source Name)._omt._tcp.local`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{Frame, MetadataFrame, OmtError};

/// The DNS-SD service type every OMT sender advertises.
pub const OMT_SERVICE_TYPE: &str = "_omt._tcp";
/// The discovery server's default TCP port.
pub const DISCOVERY_PORT: u16 = 6399;
/// The first source port OMT implementations try.
pub const SOURCE_PORT_START: u16 = 6400;
/// The last source port OMT implementations try.
pub const SOURCE_PORT_END: u16 = 6600;

/// One registered source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmtAddress {
    /// `HOSTNAME (Source Name)`.
    pub name: String,
    /// The source's TCP port.
    pub port: u16,
    /// The addresses the source is reachable at. The discovery server
    /// replaces these with the address it observes.
    pub addresses: Vec<IpAddr>,
    /// Whether this record removes the source.
    pub removed: bool,
}

impl OmtAddress {
    /// A registration for `source` on `hostname` at `port`, with no
    /// addresses yet.
    #[must_use]
    pub fn new(hostname: &str, source: &str, port: u16) -> Self {
        Self {
            name: format!("{hostname} ({source})"),
            port,
            addresses: Vec::new(),
            removed: false,
        }
    }

    /// The DNS-SD instance name (`HOSTNAME (Source Name)._omt._tcp.local`).
    #[must_use]
    pub fn service_name(&self) -> String {
        format!("{}.{OMT_SERVICE_TYPE}.local", self.name)
    }

    /// Whether the name is usable: nonempty, bounded, no control characters.
    #[must_use]
    pub fn name_is_valid(name: &str) -> bool {
        !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
    }

    /// The registration or removal message, exactly as OMT writes it.
    #[must_use]
    pub fn to_xml(&self) -> String {
        let mut addresses = String::new();
        for address in &self.addresses {
            addresses.push_str("<Address>");
            addresses.push_str(&address.to_string());
            addresses.push_str("</Address>");
        }
        let removed = if self.removed {
            "<Removed>True</Removed>"
        } else {
            ""
        };
        format!(
            "<OMTAddress><Name>{}</Name><Port>{}</Port>\
             <Addresses>{addresses}</Addresses>{removed}</OMTAddress>",
            escape(&self.name),
            self.port
        )
    }

    /// Parses an `<OMTAddress>…</OMTAddress>` record.
    #[must_use]
    pub fn from_xml(xml: &str) -> Option<Self> {
        let xml = xml.trim();
        if !xml.starts_with("<OMTAddress") {
            return None;
        }
        let name = unescape(&element(xml, "Name")?);
        let port: u16 = element(xml, "Port")?.trim().parse().ok()?;
        if !Self::name_is_valid(&name) {
            return None;
        }
        let mut addresses = Vec::new();
        let mut rest = xml;
        while let Some(start) = rest.find("<Address>") {
            let after = &rest[start + "<Address>".len()..];
            let Some(end) = after.find("</Address>") else {
                break;
            };
            if let Ok(address) = after[..end].trim().parse() {
                addresses.push(address);
            }
            rest = &after[end + "</Address>".len()..];
        }
        let removed = xml.contains("<Removed>True</Removed>");
        Some(Self {
            name,
            port,
            addresses,
            removed,
        })
    }
}

/// The discovery server's registry: who registered what, seen from which
/// client.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryRegistry {
    clients: BTreeMap<u64, Vec<OmtAddress>>,
}

impl DiscoveryRegistry {
    /// Registers `address` for `client`, using `observed` (the client's
    /// connection address) as its only reachable address.
    pub fn register(
        &mut self,
        client: u64,
        mut address: OmtAddress,
        observed: IpAddr,
    ) -> OmtAddress {
        address.addresses = vec![observed];
        address.removed = false;
        let entries = self.clients.entry(client).or_default();
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.name == address.name && entry.port == address.port)
        {
            *existing = address.clone();
        } else {
            entries.push(address.clone());
        }
        address
    }

    /// Removes `name` for `client` and returns the removal record.
    pub fn deregister(&mut self, client: u64, name: &str) -> Option<OmtAddress> {
        let entries = self.clients.get_mut(&client)?;
        let index = entries.iter().position(|entry| entry.name == name)?;
        let mut removed = entries.remove(index);
        if entries.is_empty() {
            self.clients.remove(&client);
        }
        removed.removed = true;
        Some(removed)
    }

    /// Removes every registration of `client`, as its disconnect does.
    pub fn remove_client(&mut self, client: u64) -> Vec<OmtAddress> {
        self.clients
            .remove(&client)
            .unwrap_or_default()
            .into_iter()
            .map(|mut address| {
                address.removed = true;
                address
            })
            .collect()
    }

    /// Every current registration, in client and registration order.
    #[must_use]
    pub fn records(&self) -> Vec<OmtAddress> {
        self.clients
            .values()
            .flat_map(|entries| entries.iter().cloned())
            .collect()
    }

    /// How many sources are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clients.values().map(Vec::len).sum()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why a discovery connection failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    /// The transport failed.
    Io(String),
    /// A peer sent a frame that is not an OMT metadata record.
    Protocol(&'static str),
    /// The peer closed its connection.
    Closed,
}

impl core::fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(reason) => write!(formatter, "OMT discovery transport error: {reason}"),
            Self::Protocol(reason) => write!(formatter, "invalid OMT discovery record: {reason}"),
            Self::Closed => formatter.write_str("the OMT discovery peer closed"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

impl From<std::io::Error> for DiscoveryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<OmtError> for DiscoveryError {
    fn from(error: OmtError) -> Self {
        match error {
            OmtError::Closed => Self::Closed,
            other => Self::Io(other.to_string()),
        }
    }
}

struct Peer {
    id: u64,
    stream: Arc<Mutex<TcpStream>>,
}

/// The OMT discovery server: repeats registrations to every connected
/// client and removals when clients leave.
pub struct DiscoveryServer {
    address: SocketAddr,
    running: Arc<AtomicBool>,
    peers: Arc<Mutex<Vec<Peer>>>,
    registry: Arc<Mutex<DiscoveryRegistry>>,
    accept: Option<JoinHandle<()>>,
}

impl DiscoveryServer {
    /// Listens on `address` (port 0 picks one) and starts serving.
    ///
    /// # Errors
    ///
    /// The address must be bindable.
    pub fn bind(address: impl ToSocketAddrs) -> Result<Self, DiscoveryError> {
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let running = Arc::new(AtomicBool::new(true));
        let peers = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(Mutex::new(DiscoveryRegistry::default()));
        let next_client = Arc::new(AtomicU64::new(1));
        let accept = {
            let running = Arc::clone(&running);
            let peers = Arc::clone(&peers);
            let registry = Arc::clone(&registry);
            let next_client = Arc::clone(&next_client);
            thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, remote)) => {
                            let id = next_client.fetch_add(1, Ordering::Relaxed);
                            let observed = remote.ip();
                            if let Ok(clone) = stream.try_clone() {
                                let shared = Arc::new(Mutex::new(clone));
                                let current = {
                                    let registry = registry.lock().expect("registry lock");
                                    registry.records()
                                };
                                // A new client first hears every current
                                // registration.
                                for record in current {
                                    let _ = send_record(&stream, &record);
                                }
                                peers.lock().expect("peers lock").push(Peer {
                                    id,
                                    stream: Arc::clone(&shared),
                                });
                                let running = Arc::clone(&running);
                                let peers = Arc::clone(&peers);
                                let registry = Arc::clone(&registry);
                                thread::spawn(move || {
                                    serve_client(stream, id, observed, &running, &peers, &registry);
                                });
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Ok(Self {
            address,
            running,
            peers,
            registry,
            accept: Some(accept),
        })
    }

    /// The bound address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// The current registry.
    #[must_use]
    pub fn registry(&self) -> DiscoveryRegistry {
        self.registry.lock().expect("registry lock").clone()
    }

    /// Stops accepting, closes every client, and waits for the threads.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for peer in self.peers.lock().expect("peers lock").iter() {
            let stream = peer.stream.lock().expect("peer lock");
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        // Wake the nonblocking accept loop.
        let _ = TcpStream::connect(self.address);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for DiscoveryServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve_client(
    mut stream: TcpStream,
    id: u64,
    observed: IpAddr,
    running: &AtomicBool,
    peers: &Mutex<Vec<Peer>>,
    registry: &Mutex<DiscoveryRegistry>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    while running.load(Ordering::Relaxed) {
        match read_record(&mut stream) {
            Ok(None) => {}
            Ok(Some(xml)) => {
                let Some(address) = OmtAddress::from_xml(&xml) else {
                    continue;
                };
                let record = if address.removed {
                    let mut registry = registry.lock().expect("registry lock");
                    match registry.deregister(id, &address.name) {
                        Some(removed) => removed,
                        None => continue,
                    }
                } else {
                    registry
                        .lock()
                        .expect("registry lock")
                        .register(id, address, observed)
                };
                broadcast(peers, &record);
            }
            Err(DiscoveryError::Closed | DiscoveryError::Io(_) | DiscoveryError::Protocol(_)) => {
                break;
            }
        }
    }
    let removals = registry.lock().expect("registry lock").remove_client(id);
    for removal in removals {
        broadcast(peers, &removal);
    }
    peers
        .lock()
        .expect("peers lock")
        .retain(|peer| peer.id != id);
}

fn broadcast(peers: &Mutex<Vec<Peer>>, record: &OmtAddress) {
    for peer in peers.lock().expect("peers lock").iter() {
        let stream = peer.stream.lock().expect("peer lock");
        let _ = send_record(&stream, record);
    }
}

fn send_record(mut stream: &TcpStream, record: &OmtAddress) -> Result<(), DiscoveryError> {
    let frame = Frame::Metadata(MetadataFrame::new(record.to_xml())?);
    let bytes = frame.encode()?;
    stream.write_all(&bytes)?;
    Ok(())
}

/// Reads one metadata record, `None` when the read timed out before a
/// header byte.
fn read_record(stream: &mut TcpStream) -> Result<Option<String>, DiscoveryError> {
    let mut header = [0_u8; crate::FRAME_HEADER_BYTES];
    let mut read = 0;
    while read < header.len() {
        match stream.read(&mut header[read..]) {
            Ok(0) => return Err(DiscoveryError::Closed),
            Ok(n) => read += n,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return if read == 0 {
                    Ok(None)
                } else {
                    Err(DiscoveryError::Io(
                        "a frame header was cut short".to_owned(),
                    ))
                };
            }
            Err(error) => return Err(DiscoveryError::Io(error.to_string())),
        }
    }
    let length = crate::frame::frame_len(&header)?;
    let mut payload = vec![0_u8; length - crate::FRAME_HEADER_BYTES];
    stream.read_exact(&mut payload)?;
    let mut frame = header.to_vec();
    frame.extend_from_slice(&payload);
    match Frame::decode(&frame)? {
        Frame::Metadata(metadata) => Ok(Some(metadata.xml)),
        _ => Err(DiscoveryError::Protocol(
            "a discovery frame is not metadata",
        )),
    }
}

/// A discovery client: registers one source and receives every record.
pub struct DiscoveryClient {
    stream: TcpStream,
    client_id: u64,
}

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

impl DiscoveryClient {
    /// Connects to a discovery server.
    ///
    /// # Errors
    ///
    /// The server must accept the connection.
    pub fn connect(address: impl ToSocketAddrs) -> Result<Self, DiscoveryError> {
        let stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            stream,
            client_id: NEXT_CLIENT.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// A client-local id (the server assigns its own).
    #[must_use]
    pub const fn client_id(&self) -> u64 {
        self.client_id
    }

    /// Registers `address`; the server replaces its addresses with the
    /// observed client address and repeats it to every client.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn register(&mut self, address: &OmtAddress) -> Result<(), DiscoveryError> {
        self.send(address)
    }

    /// Removes `name`.
    ///
    /// # Errors
    ///
    /// The connection must still be writable.
    pub fn deregister(&mut self, name: &str) -> Result<(), DiscoveryError> {
        let address = OmtAddress {
            name: name.to_owned(),
            port: 0,
            addresses: Vec::new(),
            removed: true,
        };
        self.send(&address)
    }

    fn send(&mut self, address: &OmtAddress) -> Result<(), DiscoveryError> {
        let frame = Frame::Metadata(MetadataFrame::new(address.to_xml())?);
        self.stream.write_all(&frame.encode()?)?;
        Ok(())
    }

    /// Reads the next record, if one arrived within the read timeout.
    ///
    /// # Errors
    ///
    /// The record must be valid and the connection open.
    pub fn receive(&mut self) -> Result<Option<OmtAddress>, DiscoveryError> {
        match read_record(&mut self.stream)? {
            Some(xml) => OmtAddress::from_xml(&xml)
                .map(Some)
                .ok_or(DiscoveryError::Protocol("an address record is malformed")),
            None => Ok(None),
        }
    }

    /// Reads every record available now.
    ///
    /// # Errors
    ///
    /// Every record must be valid and the connection open.
    pub fn drain(&mut self) -> Result<Vec<OmtAddress>, DiscoveryError> {
        let mut records = Vec::new();
        while let Some(record) = self.receive()? {
            records.push(record);
        }
        Ok(records)
    }
}

fn element(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_owned())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
