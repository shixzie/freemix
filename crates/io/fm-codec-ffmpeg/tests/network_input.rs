//! Network stream receiver integration coverage (IN-015).
//!
//! Every case serves a real stream from a local `ffmpeg` sender over
//! loopback (MPEG-TS over UDP, TCP, and HTTP, and an HLS directory served by
//! a tiny in-test HTTP server), receives it with a [`NetworkReceiver`]
//! pulled at the program rate like the session loop pulls it, and observes
//! the receive state machine: buffering into live, the stall slate and
//! reconnects when the sender dies, recovery when it returns, the jitter
//! buffer's bound, and giving up.
//!
//! Like every ffmpeg-dependent test in this crate, these cases skip silently
//! when `ffmpeg` is unavailable, unless `FM_REQUIRE_FFMPEG=1` turns the
//! absence into a hard failure.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, Picture};
use fm_types::FrameRate;
use fm_types::network_stream::{
    GiveUp, NetworkStreamConfig, NetworkStreamState, NetworkStreamStatus, ReconnectPolicy,
    StallSlate,
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;
const FPS: u32 = 25;
const FRAME_PERIOD: Duration = Duration::from_millis(40);

static SERIAL: Mutex<()> = Mutex::new(());

/// `FFmpeg` children are CPU heavy; run one scenario at a time.
fn serialize() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn tools_available() -> bool {
    let available = Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg is unavailable"
    );
    available
}

fn free_tcp_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A test-pattern MPEG-TS sender with a tone, killed on drop.
struct Sender(Child);

impl Sender {
    fn start(output_args: &[&str]) -> Self {
        let mut command = Command::new("ffmpeg");
        command
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={WIDTH}x{HEIGHT}:rate={FPS}"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-c:v",
                "mpeg2video",
                "-g",
                "10",
                "-b:v",
                "400k",
                "-c:a",
                "mp2",
                "-b:a",
                "128k",
            ])
            .args(output_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(command.spawn().expect("spawn the ffmpeg sender"))
    }

    fn mpegts(url: &str) -> Self {
        Self::start(&["-f", "mpegts", url])
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stream(url: &str) -> NetworkStreamConfig {
    let mut stream = NetworkStreamConfig::new(url);
    stream.settings.jitter_buffer_ms = 200;
    stream.settings.timeout_ms = 1_000;
    stream.settings.reconnect = ReconnectPolicy {
        backoff_min_ms: 200,
        backoff_max_ms: 1_000,
        give_up: GiveUp::Never,
    };
    stream
}

fn receiver(stream: &NetworkStreamConfig) -> NetworkReceiver {
    NetworkReceiver::start(NetworkInputConfig::new(
        stream,
        WIDTH,
        HEIGHT,
        FrameRate::new(FPS, 1).unwrap(),
    ))
    .expect("start the receiver")
}

/// What pulling at the program rate observed.
#[derive(Default)]
struct Observed {
    pictures: usize,
    loud_frames: usize,
    blacks: usize,
    states: Vec<NetworkStreamState>,
}

/// Pulls one frame per program frame until `done` holds or `limit` passes;
/// returns whether `done` held.
fn pull_until(
    receiver: &NetworkReceiver,
    limit: Duration,
    observed: &mut Observed,
    mut done: impl FnMut(&NetworkStreamStatus, &Observed) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    let mut next = Instant::now();
    while Instant::now() < deadline {
        let frame = receiver.pull();
        match &frame.picture {
            Picture::New(bytes) => {
                assert_eq!(bytes.len(), (WIDTH * HEIGHT * 4) as usize);
                observed.pictures += 1;
            }
            Picture::Black => observed.blacks += 1,
            Picture::Hold => {}
        }
        assert_eq!(frame.audio.len(), 2);
        assert!(frame.audio.iter().all(|plane| plane.len() == 1_920));
        if frame.audio[0].iter().any(|sample| sample.abs() > 0.01) {
            observed.loud_frames += 1;
        }
        let status = receiver.status();
        let token = status.state.token();
        if observed
            .states
            .last()
            .is_none_or(|last| last.token() != token)
        {
            observed.states.push(status.state);
        }
        if done(&status, observed) {
            return true;
        }
        next += FRAME_PERIOD;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    false
}

fn is_live(status: &NetworkStreamStatus) -> bool {
    status.state == NetworkStreamState::Live
}

fn is_reconnecting(status: &NetworkStreamStatus) -> bool {
    matches!(status.state, NetworkStreamState::Reconnecting { .. })
}

/// Waits for live, then for pictures and the tone to play.
fn expect_playing(receiver: &NetworkReceiver, observed: &mut Observed, what: &str) {
    assert!(
        pull_until(receiver, Duration::from_secs(20), observed, |status, _| {
            is_live(status)
        }),
        "{what}: never went live: {:?}",
        receiver.status()
    );
    let pictures = observed.pictures;
    let loud = observed.loud_frames;
    assert!(
        pull_until(receiver, Duration::from_secs(10), observed, |_, seen| {
            seen.pictures >= pictures + 25 && seen.loud_frames > loud + 10
        }),
        "{what}: live without pictures and sound: {:?}",
        receiver.status()
    );
    let status = receiver.status();
    assert!(status.bitrate_bps > 0, "{what}: no bitrate: {status:?}");
}

#[test]
fn udp_mpegts_goes_live_reconnects_when_the_sender_dies_and_recovers() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let mut stream = stream(&format!("udp://127.0.0.1:{port}"));
    stream.settings.stall = StallSlate::Black;
    let receiver = receiver(&stream);
    let sender = Sender::mpegts(&format!("udp://127.0.0.1:{port}?pkt_size=1316"));
    let mut observed = Observed::default();
    expect_playing(&receiver, &mut observed, "udp");

    drop(sender);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| { is_reconnecting(status) }
        ),
        "udp: no reconnect after the sender died: {:?}",
        receiver.status()
    );
    let reconnecting = receiver.status();
    assert!(reconnecting.reconnects >= 1);
    assert!(reconnecting.last_error.is_some());
    assert!(
        observed.states.contains(&NetworkStreamState::Stalled),
        "a live stream stalls before it reconnects: {:?}",
        observed.states
    );
    assert_eq!(observed.blacks, 1, "the black slate shows once per stall");

    let _sender = Sender::mpegts(&format!("udp://127.0.0.1:{port}?pkt_size=1316"));
    expect_playing(&receiver, &mut observed, "udp recovery");
    assert!(receiver.status().last_error.is_none());
}

#[test]
fn an_uppercase_scheme_goes_live() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let receiver = receiver(&stream(&format!("UDP://127.0.0.1:{port}")));
    let _sender = Sender::mpegts(&format!("udp://127.0.0.1:{port}?pkt_size=1316"));
    let mut observed = Observed::default();
    expect_playing(&receiver, &mut observed, "UDP://");
}

#[test]
fn tcp_mpegts_reconnects_after_the_server_restarts() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let serve = format!("tcp://127.0.0.1:{port}?listen=1");
    let sender = Sender::mpegts(&serve);
    thread::sleep(Duration::from_millis(500));
    let receiver = receiver(&stream(&format!("tcp://127.0.0.1:{port}")));
    let mut observed = Observed::default();
    expect_playing(&receiver, &mut observed, "tcp");

    drop(sender);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| { is_reconnecting(status) && status.reconnects >= 2 }
        ),
        "tcp: refused connections must keep reconnecting: {:?}",
        receiver.status()
    );
    let _sender = Sender::mpegts(&serve);
    expect_playing(&receiver, &mut observed, "tcp recovery");
}

#[test]
fn http_mpegts_goes_live() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let _sender = Sender::start(&[
        "-f",
        "mpegts",
        "-listen",
        "1",
        &format!("http://127.0.0.1:{port}/live.ts"),
    ]);
    thread::sleep(Duration::from_millis(500));
    let receiver = receiver(&stream(&format!("http://127.0.0.1:{port}/live.ts")));
    let mut observed = Observed::default();
    expect_playing(&receiver, &mut observed, "http");
}

/// Serves the files of one directory over HTTP/1.0 `GET`.
struct FileServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FileServer {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve_one(&root, stream),
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

fn serve_one(root: &Path, stream: std::net::TcpStream) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
        }
    }
    let name = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut stream = reader.into_inner();
    let body = (!name.contains('/') && !name.contains(".."))
        .then(|| std::fs::read(root.join(&name)).ok())
        .flatten();
    let _ = match body {
        Some(body) => {
            let kind = if std::path::Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m3u8"))
            {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.0 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .and_then(|()| stream.write_all(&body))
        }
        None => stream.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
    };
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn hls_playlist_served_over_http_goes_live() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let playlist = directory.path().join("live.m3u8");
    let _sender = Sender::start(&[
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_list_size",
        "6",
        "-hls_flags",
        "delete_segments",
        playlist.to_str().unwrap(),
    ]);
    let ready = Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(&playlist).map_or(0, |text| text.matches("#EXTINF").count()) < 3 {
        assert!(Instant::now() < ready, "the HLS sender wrote no playlist");
        thread::sleep(Duration::from_millis(100));
    }
    let server = FileServer::start(directory.path().to_path_buf());
    let receiver = receiver(&stream(&format!(
        "http://127.0.0.1:{}/live.m3u8",
        server.port
    )));
    let mut observed = Observed::default();
    expect_playing(&receiver, &mut observed, "hls");
}

#[test]
fn the_jitter_buffer_is_bounded_and_drops_the_oldest_pictures() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let receiver = receiver(&stream(&format!("udp://127.0.0.1:{port}")));
    let bounds = receiver.bounds();
    assert_eq!((bounds.target_frames, bounds.capacity_frames), (5, 10));
    let _sender = Sender::mpegts(&format!("udp://127.0.0.1:{port}?pkt_size=1316"));
    // Nobody pulls: the buffer fills to its capacity and then drops.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = receiver.status();
        assert!(receiver.buffered_frames() <= bounds.capacity_frames);
        assert!(status.buffer_ms <= 400, "{status:?}");
        if status.dropped_frames >= 25 {
            assert_eq!(status.state, NetworkStreamState::Live);
            assert_eq!(status.buffer_ms, 400);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the buffer never overflowed: {status:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_stream_that_never_answers_fails_once_the_policy_gives_up_without_echoing_secrets() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let mut stream = stream(&format!(
        "http://operator:hunter2@127.0.0.1:{port}/live.ts?token=s3cr3t"
    ));
    stream.settings.reconnect = ReconnectPolicy {
        backoff_min_ms: 100,
        backoff_max_ms: 100,
        give_up: GiveUp::After(NonZeroU32::new(2).unwrap()),
    };
    let receiver = receiver(&stream);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(20),
            &mut observed,
            |status, _| { status.state == NetworkStreamState::Failed }
        ),
        "never failed: {:?}",
        receiver.status()
    );
    let status = receiver.status();
    assert_eq!(
        status.reconnects, 2,
        "two reconnect attempts, then it gives up"
    );
    assert_eq!(observed.pictures, 0);
    assert_eq!(observed.loud_frames, 0);
    let error = status.last_error.expect("a failure names its last error");
    for secret in ["hunter2", "operator", "s3cr3t"] {
        assert!(!error.contains(secret), "{error}");
    }
    assert!(!format!("{receiver:?}").contains("hunter2"));
}

#[test]
fn a_stream_without_audio_plays_its_pictures_in_silence() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let receiver = receiver(&stream(&format!("udp://127.0.0.1:{port}")));
    let _sender = Sender::start(&[
        "-map",
        "0:v",
        "-f",
        "mpegts",
        &format!("udp://127.0.0.1:{port}?pkt_size=1316"),
    ]);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(25),
            &mut observed,
            |status, seen| { is_live(status) && seen.pictures >= 25 }
        ),
        "never played: {:?}",
        receiver.status()
    );
    assert_eq!(observed.loud_frames, 0);
}
