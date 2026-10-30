//! The OMT sender: a TCP listener, one session per receiver, subscription
//! gating, the combined tally, and frame fan-out.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::metadata::{Command, Quality, SenderInfo, Tally};
use crate::wire::{read_metadata, write_metadata};
use crate::{AudioFrame, Frame, FrameKind, MetadataFrame, OmtError, VideoFrame};

const COMMAND_TIMEOUT: Duration = Duration::from_millis(200);

#[allow(clippy::struct_excessive_bools)] // one subscription per media kind
#[derive(Clone, Copy, Debug, Default)]
struct PeerState {
    video: bool,
    audio: bool,
    metadata: bool,
    preview: bool,
    tally: Tally,
    quality: Quality,
}

struct Peer {
    id: u64,
    write: Arc<Mutex<TcpStream>>,
    state: Arc<Mutex<PeerState>>,
}

/// One OMT source: receivers connect, subscribe, and receive frames.
pub struct Sender {
    address: SocketAddr,
    running: Arc<AtomicBool>,
    peers: Arc<Mutex<Vec<Peer>>>,
    info: Arc<Mutex<Option<SenderInfo>>>,
    accept: Option<JoinHandle<()>>,
}

impl Sender {
    /// Listens on `address` (port 0 picks an ephemeral one) and serves
    /// receivers.
    ///
    /// # Errors
    ///
    /// The address must be bindable.
    pub fn bind(address: impl ToSocketAddrs) -> Result<Self, OmtError> {
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let running = Arc::new(AtomicBool::new(true));
        let peers = Arc::new(Mutex::new(Vec::new()));
        let info: Arc<Mutex<Option<SenderInfo>>> = Arc::new(Mutex::new(None));
        let accept = {
            let running = Arc::clone(&running);
            let peers = Arc::clone(&peers);
            let info = Arc::clone(&info);
            let mut next = 1_u64;
            thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let Ok(write) = stream.try_clone() else {
                                continue;
                            };
                            let id = next;
                            next = next.wrapping_add(1);
                            let state = Arc::new(Mutex::new(PeerState::default()));
                            let write = Arc::new(Mutex::new(write));
                            peers.lock().expect("peers lock").push(Peer {
                                id,
                                write: Arc::clone(&write),
                                state: Arc::clone(&state),
                            });
                            {
                                let mut stream = write.lock().expect("peer lock");
                                if let Some(info) = info.lock().expect("info lock").as_ref() {
                                    let _ = write_metadata(&mut stream, 0, &info.command());
                                }
                                let _ = write_metadata(
                                    &mut stream,
                                    0,
                                    combined_tally(&peers).command(),
                                );
                            }
                            let running = Arc::clone(&running);
                            let peers = Arc::clone(&peers);
                            thread::spawn(move || {
                                serve_peer(stream, id, &state, &running, &peers);
                            });
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
            info,
            accept: Some(accept),
        })
    }

    /// The bound address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// The number of connected receivers.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.peers.lock().expect("peers lock").len()
    }

    /// How many receivers subscribed to `kind`.
    #[must_use]
    pub fn subscribers(&self, kind: FrameKind) -> usize {
        self.peers
            .lock()
            .expect("peers lock")
            .iter()
            .filter(|peer| {
                let state = peer.state.lock().expect("state lock");
                match kind {
                    FrameKind::Video => state.video,
                    FrameKind::Audio => state.audio,
                    FrameKind::Metadata => state.metadata,
                }
            })
            .count()
    }

    /// Whether any receiver asked for one-eighth preview frames.
    #[must_use]
    pub fn preview_requested(&self) -> bool {
        self.peers
            .lock()
            .expect("peers lock")
            .iter()
            .any(|peer| peer.state.lock().expect("state lock").preview)
    }

    /// The tally every receiver reports, combined.
    #[must_use]
    pub fn tally(&self) -> Tally {
        combined_tally(&self.peers)
    }

    /// The strongest compression quality any receiver suggests.
    #[must_use]
    pub fn quality(&self) -> Quality {
        self.peers
            .lock()
            .expect("peers lock")
            .iter()
            .fold(Quality::Default, |quality, peer| {
                quality.strongest(peer.state.lock().expect("state lock").quality)
            })
    }

    /// Announces `info` to current and future receivers.
    ///
    /// # Errors
    ///
    /// A connected receiver must still be writable.
    pub fn set_info(&self, info: &SenderInfo) -> Result<(), OmtError> {
        *self.info.lock().expect("info lock") = Some(info.clone());
        for peer in self.peers.lock().expect("peers lock").iter() {
            let mut stream = peer.write.lock().expect("peer lock");
            write_metadata(&mut stream, 0, &info.command())?;
        }
        Ok(())
    }

    /// Sends `frame` to every receiver subscribed to video, sending the
    /// one-eighth `preview` instead to receivers that asked for it. A
    /// receiver that asked for preview is skipped when `preview` is `None`.
    ///
    /// # Errors
    ///
    /// Every frame must encode.
    pub fn send_video(
        &self,
        frame: &VideoFrame,
        preview: Option<&VideoFrame>,
    ) -> Result<usize, OmtError> {
        let full = Frame::Video(frame.clone());
        let preview = preview.map(|frame| Frame::Video(frame.clone()));
        self.send_matching(FrameKind::Video, &full, preview.as_ref())
    }

    /// Sends `frame` to every receiver subscribed to audio.
    ///
    /// # Errors
    ///
    /// Every frame must encode.
    pub fn send_audio(&self, frame: &AudioFrame) -> Result<usize, OmtError> {
        self.send_matching(FrameKind::Audio, &Frame::Audio(frame.clone()), None)
    }

    /// Sends `xml` as a metadata frame to every receiver subscribed to
    /// metadata.
    ///
    /// # Errors
    ///
    /// The metadata must be valid.
    pub fn send_metadata(&self, timestamp: i64, xml: &str) -> Result<usize, OmtError> {
        let frame = Frame::Metadata(MetadataFrame::new(xml)?.at(timestamp));
        self.send_matching(FrameKind::Metadata, &frame, None)
    }

    fn send_matching(
        &self,
        kind: FrameKind,
        full: &Frame,
        preview: Option<&Frame>,
    ) -> Result<usize, OmtError> {
        // Encode once, whatever the number of receivers.
        let full_bytes = full.encode()?;
        let preview_bytes = preview.map(Frame::encode).transpose()?;
        let mut sent = 0;
        for peer in self.peers.lock().expect("peers lock").iter() {
            let state = *peer.state.lock().expect("state lock");
            let subscribed = match kind {
                FrameKind::Video => state.video,
                FrameKind::Audio => state.audio,
                FrameKind::Metadata => state.metadata,
            };
            if !subscribed {
                continue;
            }
            let bytes = if kind == FrameKind::Video && state.preview {
                let Some(bytes) = &preview_bytes else {
                    continue;
                };
                bytes
            } else {
                &full_bytes
            };
            let mut stream = peer.write.lock().expect("peer lock");
            if stream.write_all(bytes).is_ok() {
                sent += 1;
            }
        }
        Ok(sent)
    }

    /// Stops accepting, closes every receiver, and waits for the accept
    /// thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for peer in self.peers.lock().expect("peers lock").iter() {
            let stream = peer.write.lock().expect("peer lock");
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        let _ = TcpStream::connect(self.address);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve_peer(
    mut stream: TcpStream,
    id: u64,
    state: &Mutex<PeerState>,
    running: &AtomicBool,
    peers: &Mutex<Vec<Peer>>,
) {
    while running.load(Ordering::Relaxed) {
        match read_metadata(&mut stream, Some(COMMAND_TIMEOUT)) {
            Ok(Some(xml)) => {
                let (changed, subscribed) = match Command::parse(&xml) {
                    Command::Subscribe(kind) => {
                        let mut state = state.lock().expect("state lock");
                        match kind {
                            FrameKind::Video => state.video = true,
                            FrameKind::Audio => state.audio = true,
                            FrameKind::Metadata => state.metadata = true,
                        }
                        (true, true)
                    }
                    Command::Preview(preview) => {
                        state.lock().expect("state lock").preview = preview;
                        (false, false)
                    }
                    Command::Tally(tally) => {
                        state.lock().expect("state lock").tally = tally;
                        (true, false)
                    }
                    Command::Quality(quality) => {
                        state.lock().expect("state lock").quality = quality;
                        (false, false)
                    }
                    Command::Info(_) | Command::Metadata(_) => (false, false),
                };
                if subscribed {
                    send_control(peers, id);
                }
                if changed {
                    broadcast_tally(peers);
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    peers
        .lock()
        .expect("peers lock")
        .retain(|peer| peer.id != id);
    broadcast_tally(peers);
}

fn send_control(peers: &Mutex<Vec<Peer>>, id: u64) {
    let tally = combined_tally(peers).command();
    for peer in peers.lock().expect("peers lock").iter() {
        if peer.id == id {
            let mut stream = peer.write.lock().expect("peer lock");
            let _ = write_metadata(&mut stream, 0, tally);
        }
    }
}

fn broadcast_tally(peers: &Mutex<Vec<Peer>>) {
    let tally = combined_tally(peers).command();
    for peer in peers.lock().expect("peers lock").iter() {
        let mut stream = peer.write.lock().expect("peer lock");
        let _ = write_metadata(&mut stream, 0, tally);
    }
}

fn combined_tally(peers: &Mutex<Vec<Peer>>) -> Tally {
    let mut tally = Tally::default();
    for peer in peers.lock().expect("peers lock").iter() {
        tally = tally.or(peer.state.lock().expect("state lock").tally);
    }
    tally
}
