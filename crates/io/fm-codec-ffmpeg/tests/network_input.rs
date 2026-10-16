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

#[test]
fn a_stream_that_gains_audio_is_heard_after_it_reconnects() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let url = format!("udp://127.0.0.1:{port}?pkt_size=1316");
    let receiver = receiver(&stream(&format!("udp://127.0.0.1:{port}")));
    let sender = Sender::start(&["-map", "0:v", "-f", "mpegts", &url]);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(25),
            &mut observed,
            |status, seen| { is_live(status) && seen.pictures >= 25 }
        ),
        "video-only never played: {:?}",
        receiver.status()
    );
    assert_eq!(observed.loud_frames, 0);

    drop(sender);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| { is_reconnecting(status) }
        ),
        "no reconnect after the video-only sender died: {:?}",
        receiver.status()
    );
    // The sender comes back with a tone: the reconnect asks for audio again.
    let _sender = Sender::mpegts(&url);
    expect_playing(&receiver, &mut observed, "audio gained on reconnect");
}

/// Writes a short local HLS rendition into `directory` and returns its
/// segment names.
fn local_hls_segments(directory: &Path) -> Vec<String> {
    let playlist = directory.join("local.m3u8");
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={WIDTH}x{HEIGHT}:rate={FPS}:duration=6"),
            "-c:v",
            "mpeg2video",
            "-g",
            "10",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
            "-hls_segment_filename",
        ])
        .arg(directory.join("seg%d.ts"))
        .arg(&playlist)
        .stdin(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
    std::fs::read_to_string(&playlist)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_served_playlist_cannot_reach_local_files() {
    use std::fmt::Write as _;

    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let local = tempfile::tempdir().unwrap();
    let segments = local_hls_segments(local.path());
    assert!(segments.len() >= 3, "{segments:?}");
    // A remote playlist whose segments are files on the receiving machine.
    let remote = tempfile::tempdir().unwrap();
    let mut playlist = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for segment in &segments {
        let _ = write!(
            playlist,
            "#EXTINF:1.0,\nfile://{}\n",
            local.path().join(segment).display()
        );
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    std::fs::write(remote.path().join("evil.m3u8"), playlist).unwrap();
    let server = FileServer::start(remote.path().to_path_buf());
    let mut config = stream(&format!("http://127.0.0.1:{}/evil.m3u8", server.port));
    config.settings.reconnect.give_up = GiveUp::After(NonZeroU32::new(1).unwrap());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    pull_until(
        &receiver,
        Duration::from_secs(10),
        &mut observed,
        |status, _| status.state == NetworkStreamState::Failed,
    );
    let status = receiver.status();
    assert_eq!(
        observed.pictures, 0,
        "a local file played through a served playlist: {status:?}"
    );
    assert_ne!(status.state, NetworkStreamState::Live);
}

#[test]
fn a_served_session_description_cannot_open_rtp_ports() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    // An RTP sender and its SDP, served over HTTP: without a protocol
    // whitelist the child would follow the SDP onto RTP/UDP and play it.
    let directory = tempfile::tempdir().unwrap();
    let sdp = directory.path().join("live.sdp");
    let port = free_udp_port() & !1;
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
            "-c:v",
            "mpeg2video",
            "-g",
            "10",
            "-f",
            "rtp",
            "-sdp_file",
        ])
        .arg(&sdp)
        .arg(format!("rtp://127.0.0.1:{port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _sender = Sender(command.spawn().expect("spawn the RTP sender"));
    let ready = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&sdp).map_or(true, |text| !text.contains("m=video")) {
        assert!(Instant::now() < ready, "the RTP sender wrote no SDP");
        thread::sleep(Duration::from_millis(50));
    }
    let server = FileServer::start(directory.path().to_path_buf());
    let mut config = stream(&format!("http://127.0.0.1:{}/live.sdp", server.port));
    config.settings.reconnect.give_up = GiveUp::After(NonZeroU32::new(1).unwrap());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    pull_until(
        &receiver,
        Duration::from_secs(10),
        &mut observed,
        |status, _| status.state == NetworkStreamState::Failed,
    );
    let status = receiver.status();
    assert_eq!(
        observed.pictures, 0,
        "an HTTP stream reached RTP through its SDP: {status:?}"
    );
}

/// Not a test: the stand-in daemon of
/// `a_killed_parent_takes_its_stream_child_with_it`. It receives a UDP
/// stream nobody sends, prints that it is ready, and waits to be killed.
#[test]
#[ignore = "helper process of a_killed_parent_takes_its_stream_child_with_it"]
fn killed_parent_helper() {
    let Ok(port) = std::env::var("FM_NETWORK_PARENT_HELPER_PORT") else {
        return;
    };
    let mut config = stream(&format!("udp://127.0.0.1:{port}"));
    config.settings.timeout_ms = 60_000;
    let _receiver = receiver(&config);
    println!("helper-ready");
    std::io::stdout().flush().unwrap();
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

/// The `(pid, comm)` of every child of `parent`.
#[cfg(target_os = "linux")]
fn children_of(parent: u32) -> Vec<(u32, String)> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid ...`; `comm` may hold spaces.
        let Some((head, tail)) = stat.rsplit_once(") ") else {
            continue;
        };
        let comm = head
            .split_once(" (")
            .map_or("", |(_, comm)| comm)
            .to_owned();
        let mut fields = tail.split_whitespace();
        let _state = fields.next();
        if fields.next().and_then(|ppid| ppid.parse::<u32>().ok()) == Some(parent) {
            children.push((pid, comm));
        }
    }
    children
}

/// Whether `pid` is gone (or only a zombie waiting for its new parent).
#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rsplit_once(") ")
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .is_none_or(|state| state == "Z" || state == "X")
    })
}

#[cfg(target_os = "linux")]
#[test]
fn a_killed_parent_takes_its_stream_child_with_it() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_udp_port();
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "killed_parent_helper",
            "--nocapture",
        ])
        .env("FM_NETWORK_PARENT_HELPER_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the helper");
    let mut lines = BufReader::new(helper.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|line| line.is_ok_and(|line| line.contains("helper-ready"))),
        "the helper never started its receiver"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let child = loop {
        if let Some((pid, _)) = children_of(helper.id())
            .into_iter()
            .find(|(_, comm)| comm == "ffmpeg")
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the helper spawned no ffmpeg");
        thread::sleep(Duration::from_millis(20));
    };
    // The child holds the UDP port while it waits for a sender.
    let deadline = Instant::now() + Duration::from_secs(5);
    while UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
        assert!(Instant::now() < deadline, "the child never bound its port");
        thread::sleep(Duration::from_millis(20));
    }

    // SIGKILL: the helper runs no cleanup at all.
    helper.kill().unwrap();
    helper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !process_gone(child) {
        assert!(
            Instant::now() < deadline,
            "ffmpeg {child} outlived its killed parent"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
        assert!(Instant::now() < deadline, "the UDP port stayed held");
        thread::sleep(Duration::from_millis(20));
    }
}
