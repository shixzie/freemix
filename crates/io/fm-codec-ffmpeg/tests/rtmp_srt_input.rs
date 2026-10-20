//! RTMP and SRT receiver integration coverage (IN-016, IN-017).
//!
//! Every case runs a real `ffmpeg` peer over loopback: an encoder pushing
//! RTMP to the receiver's listener (with the right and a wrong stream key),
//! an `ffmpeg` RTMP server the receiver pulls from as a caller, and SRT
//! callers and listeners with AES passphrases, one of them sending eight
//! audio channels whose third pair alone carries a tone.
//!
//! The cases skip when `ffmpeg` is unavailable unless `FM_REQUIRE_FFMPEG=1`,
//! and the SRT cases skip when `ffmpeg` lacks `libsrt` unless
//! `FM_REQUIRE_FFMPEG_SRT=1`.

use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, Picture};
use fm_types::FrameRate;
use fm_types::network_stream::{
    AudioChannels, GiveUp, NetworkStreamConfig, NetworkStreamState, NetworkStreamStatus,
    ReconnectPolicy, SrtKeyLength, StreamMode,
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;
const FPS: u32 = 25;
const FRAME_PERIOD: Duration = Duration::from_millis(40);
const KEY: &str = "sk_live_7Hq2";
const PASSPHRASE: &str = "correct-horse-battery-staple";

static SERIAL: Mutex<()> = Mutex::new(());

fn serialize() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ffmpeg_output(args: &[&str]) -> Option<String> {
    let output = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn tools_available() -> bool {
    let available = ffmpeg_output(&["-version"]).is_some();
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg is unavailable"
    );
    available
}

/// Whether `ffmpeg` was built with `libsrt`.
fn srt_available() -> bool {
    if !tools_available() {
        return false;
    }
    let available = ffmpeg_output(&["-hide_banner", "-protocols"])
        .is_some_and(|protocols| protocols.lines().any(|line| line.trim() == "srt"));
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG_SRT").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG_SRT=1 but ffmpeg has no libsrt"
    );
    available
}

/// The H.264 encoder when `ffmpeg` has one, else FLV's Sorenson codec.
fn video_codec() -> &'static str {
    if ffmpeg_output(&["-hide_banner", "-encoders"])
        .is_some_and(|encoders| encoders.contains("libx264"))
    {
        "libx264"
    } else {
        "flv"
    }
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

/// A test-pattern peer with a tone, killed on drop.
struct Peer(Child);

impl Peer {
    /// `audio` is the lavfi audio graph; `output` the encoder and output
    /// arguments.
    fn start(audio: &str, output: &[&str]) -> Self {
        let codec = video_codec();
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
                audio,
                "-c:v",
                codec,
                "-g",
                "10",
            ])
            .args(if codec == "libx264" {
                &["-preset", "ultrafast", "-tune", "zerolatency"][..]
            } else {
                &[][..]
            })
            .args(["-c:a", "aac", "-b:a", "128k"])
            .args(output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(command.spawn().expect("spawn the ffmpeg peer"))
    }

    fn tone(output: &[&str]) -> Self {
        Self::start("sine=frequency=440:sample_rate=48000", output)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stream(url: &str) -> NetworkStreamConfig {
    let mut stream = NetworkStreamConfig::new(url);
    stream.settings.jitter_buffer_ms = 200;
    stream.settings.timeout_ms = 1_500;
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

#[derive(Default)]
struct Observed {
    pictures: usize,
    loud_frames: usize,
    states: Vec<NetworkStreamState>,
}

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
        if let Picture::New(bytes) = &frame.picture {
            assert_eq!(bytes.len(), (WIDTH * HEIGHT * 4) as usize);
            observed.pictures += 1;
        }
        if frame.audio[0].iter().any(|sample| sample.abs() > 0.01) {
            observed.loud_frames += 1;
        }
        let status = receiver.status();
        if observed
            .states
            .last()
            .is_none_or(|last| last.token() != status.state.token())
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

fn expect_live(receiver: &NetworkReceiver, observed: &mut Observed, what: &str) {
    assert!(
        pull_until(receiver, Duration::from_secs(20), observed, |status, _| {
            status.state == NetworkStreamState::Live
        }),
        "{what}: never went live: {:?}",
        receiver.status()
    );
    let pictures = observed.pictures;
    assert!(
        pull_until(receiver, Duration::from_secs(10), observed, |_, seen| {
            seen.pictures >= pictures + 25
        }),
        "{what}: live without pictures: {:?}",
        receiver.status()
    );
    assert!(receiver.status().bitrate_bps > 0, "{what}: no bitrate");
}

/// Pulls for `span` and returns how many frames were loud.
fn loud_frames_over(receiver: &NetworkReceiver, span: Duration) -> usize {
    let mut observed = Observed::default();
    pull_until(receiver, span, &mut observed, |_, _| false);
    observed.loud_frames
}

/// Asserts that the receiver's encrypted SRT child (the only process whose
/// argv names the `pipe,srt` whitelist) is running and that `secret` is not
/// in its argv, which any local user can read in `/proc/<pid>/cmdline`. The
/// test's own `ffmpeg` peer carries the passphrase in its URL, so it is
/// told apart by that whitelist.
#[cfg(target_os = "linux")]
fn assert_child_argv_free_of(secret: &str) {
    let children = std::fs::read_dir("/proc")
        .expect("read /proc")
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path().join("cmdline")).ok())
        .map(|cmdline| String::from_utf8_lossy(&cmdline).replace('\0', " "))
        .filter(|cmdline| cmdline.contains("-protocol_whitelist pipe,srt -i pipe:0"))
        .collect::<Vec<_>>();
    assert!(!children.is_empty(), "the receiver's child is running");
    for cmdline in children {
        assert!(!cmdline.contains(secret), "argv leaks the passphrase");
    }
}

#[cfg(not(target_os = "linux"))]
fn assert_child_argv_free_of(_secret: &str) {}

fn assert_no_secret(status: &NetworkStreamStatus, secrets: &[&str]) {
    if let Some(error) = &status.last_error {
        for secret in secrets {
            assert!(!error.contains(secret), "{error} leaks {secret}");
        }
    }
}

#[test]
fn an_rtmp_listener_takes_an_encoder_push_refuses_a_wrong_key_and_listens_again() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let mut config = stream(&format!("rtmp://127.0.0.1:{port}/live"));
    config.settings.mode = StreamMode::Listener;
    config.stream_key = Some(KEY.to_owned());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    // `listening` means the port accepts: a connection made the moment it
    // is reported is never refused.
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| status.state == NetworkStreamState::Listening
        ),
        "the listener listens: {:?}",
        receiver.status()
    );
    drop(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("a listening port accepts"));
    pull_until(&receiver, Duration::from_secs(3), &mut observed, |_, _| {
        false
    });
    assert_eq!(
        receiver.status().state,
        NetworkStreamState::Listening,
        "a listener waits for its publisher without timing out"
    );
    assert_eq!(receiver.status().reconnects, 0);

    // A publisher with another stream key is dropped, never shown.
    let wrong = Peer::tone(&["-f", "flv", &format!("rtmp://127.0.0.1:{port}/live/wrong")]);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| {
                status
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("another stream key"))
            }
        ),
        "a wrong stream key is refused: {:?}",
        receiver.status()
    );
    assert_eq!(observed.pictures, 0, "a refused publisher is never shown");
    assert_no_secret(&receiver.status(), &[KEY, "wrong"]);
    drop(wrong);

    // The port never closes: a publisher pushing the moment the wrong one
    // is refused, once and without retrying, goes live.
    let publisher = Peer::tone(&["-f", "flv", &format!("rtmp://127.0.0.1:{port}/live/{KEY}")]);
    expect_live(&receiver, &mut observed, "rtmp listener");
    assert!(loud_frames_over(&receiver, Duration::from_secs(2)) > 10);
    assert_eq!(receiver.status().source_channels, 1);

    drop(publisher);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(15),
            &mut observed,
            |status, _| { status.state == NetworkStreamState::Listening }
        ),
        "the listener listens again once its publisher leaves: {:?}",
        receiver.status()
    );
    let _again = Peer::tone(&["-f", "flv", &format!("rtmp://127.0.0.1:{port}/live/{KEY}")]);
    expect_live(&receiver, &mut observed, "rtmp listener, second push");
    assert_no_secret(&receiver.status(), &[KEY]);
}

#[test]
fn an_rtmp_listener_takes_a_publisher_that_connects_while_another_is_refused() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let mut config = stream(&format!("rtmp://127.0.0.1:{port}/live"));
    config.settings.mode = StreamMode::Listener;
    config.stream_key = Some(KEY.to_owned());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| status.state == NetworkStreamState::Listening
        ),
        "the listener listens: {:?}",
        receiver.status()
    );
    // The right publisher connects right behind the wrong one, before the
    // wrong one is refused: it waits for the port, then goes live, once
    // and without retrying.
    let wrong = Peer::tone(&["-f", "flv", &format!("rtmp://127.0.0.1:{port}/live/wrong")]);
    thread::sleep(Duration::from_millis(300));
    let _publisher = Peer::tone(&["-f", "flv", &format!("rtmp://127.0.0.1:{port}/live/{KEY}")]);
    expect_live(&receiver, &mut observed, "rtmp listener after a refusal");
    drop(wrong);
    assert!(loud_frames_over(&receiver, Duration::from_secs(2)) > 10);
    assert_no_secret(&receiver.status(), &[KEY, "wrong"]);
}

#[test]
fn an_rtmp_caller_pulls_from_an_ffmpeg_rtmp_server() {
    let _serial = serialize();
    if !tools_available() {
        return;
    }
    let port = free_tcp_port();
    let _server = Peer::tone(&[
        "-f",
        "flv",
        "-listen",
        "1",
        &format!("rtmp://127.0.0.1:{port}/live/{KEY}"),
    ]);
    thread::sleep(Duration::from_millis(700));
    let mut config = stream(&format!("rtmp://127.0.0.1:{port}/live"));
    config.stream_key = Some(KEY.to_owned());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    expect_live(&receiver, &mut observed, "rtmp caller");
    assert!(loud_frames_over(&receiver, Duration::from_secs(2)) > 10);
    assert!(
        !format!("{receiver:?}").contains(KEY),
        "the receiver's Debug masks the stream key"
    );
}

#[test]
fn an_srt_listener_with_a_passphrase_hears_the_chosen_pair_of_eight_channels() {
    let _serial = serialize();
    if !srt_available() {
        return;
    }
    let port = free_udp_port();
    let mut config = stream(&format!("srt://127.0.0.1:{port}"));
    config.settings.mode = StreamMode::Listener;
    config.settings.key_length = SrtKeyLength::Aes256;
    config.settings.latency_ms = 80;
    config.settings.audio = AudioChannels::Pair56;
    config.passphrase = Some(PASSPHRASE.to_owned());
    config.stream_id = Some("cam1".to_owned());
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    pull_until(&receiver, Duration::from_secs(1), &mut observed, |_, _| {
        false
    });
    assert_eq!(receiver.status().state, NetworkStreamState::Listening);

    // Eight channels: a tone on channels 5 and 6 only.
    let eight = "sine=frequency=440:sample_rate=48000,\
                 pan=7.1|c0=0*c0|c1=0*c0|c2=0*c0|c3=0*c0|c4=c0|c5=c0|c6=0*c0|c7=0*c0";
    let caller_url = |port: u16| {
        format!(
            "srt://127.0.0.1:{port}?mode=caller&passphrase={PASSPHRASE}&pbkeylen=32&streamid=cam1"
        )
    };
    let _caller = Peer::start(eight, &["-f", "mpegts", &caller_url(port)]);
    expect_live(&receiver, &mut observed, "srt listener");
    assert_child_argv_free_of(PASSPHRASE);
    assert_eq!(receiver.status().source_channels, 8);
    assert!(
        loud_frames_over(&receiver, Duration::from_secs(2)) > 10,
        "pair 5-6 carries the tone"
    );
    assert_no_secret(&receiver.status(), &[PASSPHRASE]);
    drop(receiver);

    // The same source heard as pair 1-2 is silent.
    let port = free_udp_port();
    config.url = format!("srt://127.0.0.1:{port}");
    config.settings.audio = AudioChannels::Pair12;
    let receiver = self::receiver(&config);
    let _caller = Peer::start(eight, &["-f", "mpegts", &caller_url(port)]);
    let mut observed = Observed::default();
    expect_live(&receiver, &mut observed, "srt listener, pair 1-2");
    assert!(
        loud_frames_over(&receiver, Duration::from_secs(2)) <= 2,
        "pair 1-2 is silent"
    );
}

#[test]
fn an_srt_listener_refuses_a_wrong_passphrase() {
    let _serial = serialize();
    if !srt_available() {
        return;
    }
    let port = free_udp_port();
    let mut config = stream(&format!("srt://127.0.0.1:{port}"));
    config.settings.mode = StreamMode::Listener;
    config.passphrase = Some(PASSPHRASE.to_owned());
    let receiver = receiver(&config);
    let _caller = Peer::tone(&[
        "-f",
        "mpegts",
        &format!("srt://127.0.0.1:{port}?mode=caller&passphrase=not-the-passphrase"),
    ]);
    let mut observed = Observed::default();
    pull_until(&receiver, Duration::from_secs(6), &mut observed, |_, _| {
        false
    });
    assert_eq!(observed.pictures, 0, "a wrong passphrase never connects");
    assert_eq!(
        receiver.status().state,
        NetworkStreamState::Listening,
        "the listener keeps listening"
    );
    assert_no_secret(&receiver.status(), &[PASSPHRASE]);
}

#[test]
fn an_srt_listener_takes_a_new_caller_once_one_leaves() {
    let _serial = serialize();
    if !srt_available() {
        return;
    }
    let port = free_udp_port();
    let mut config = stream(&format!("srt://127.0.0.1:{port}"));
    config.settings.mode = StreamMode::Listener;
    config.settings.latency_ms = 80;
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    // `listening` is reported once the child answers on its port.
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| status.state == NetworkStreamState::Listening
        ),
        "the SRT listener listens: {:?}",
        receiver.status()
    );
    let caller_url = format!("srt://127.0.0.1:{port}?mode=caller");
    let caller = Peer::tone(&["-f", "mpegts", &caller_url]);
    expect_live(&receiver, &mut observed, "srt listener");
    drop(caller);
    // The next caller starts the moment the first leaves, once and without
    // retrying: it retransmits its handshake while the receiver's next
    // child binds the port.
    let _next = Peer::tone(&["-f", "mpegts", &caller_url]);
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |status, _| status.state != NetworkStreamState::Live
        ),
        "the first caller's session ends: {:?}",
        receiver.status()
    );
    expect_live(&receiver, &mut observed, "srt listener, next caller");
}

#[test]
fn an_srt_caller_pulls_from_an_encrypted_ffmpeg_listener() {
    let _serial = serialize();
    if !srt_available() {
        return;
    }
    let port = free_udp_port();
    let _server = Peer::tone(&[
        "-f",
        "mpegts",
        &format!("srt://127.0.0.1:{port}?mode=listener&passphrase={PASSPHRASE}&pbkeylen=16"),
    ]);
    thread::sleep(Duration::from_millis(500));
    let mut config = stream(&format!("srt://127.0.0.1:{port}"));
    config.passphrase = Some(PASSPHRASE.to_owned());
    config.settings.key_length = SrtKeyLength::Aes128;
    let receiver = receiver(&config);
    let mut observed = Observed::default();
    expect_live(&receiver, &mut observed, "srt caller");
    assert_child_argv_free_of(PASSPHRASE);
    assert!(loud_frames_over(&receiver, Duration::from_secs(2)) > 10);
    assert!(!format!("{receiver:?}").contains(PASSPHRASE));
}
