//! Process-level RC-009: the daemon serves the vMix-compatible TCP API.

#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "freemixd-vmix-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn project(&self) -> PathBuf {
        self.0.join("show.freemix")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Two solid generators on Program and Preview; 25 fps, 320x180.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(46).unwrap()),
        "vMix",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(320, 180).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    for (id, name, red) in [(1, "Solid", 12), (2, "Other", 200)] {
        project.add_input(Input {
            id: input(id),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(red, 34, 56, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(2)),
            realized_preview_id: Some(input(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

struct Daemon {
    child: Child,
    vmix: SocketAddr,
    vmix_http: SocketAddr,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--vmix-listen")
            .arg("127.0.0.1:0")
            .arg("--vmix-http-listen")
            .arg("127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let vmix = line
                    .strip_prefix("FREEMIXD_VMIX_READY\tv=1\taddress=")
                    .and_then(|address| address.parse::<SocketAddr>().ok());
                let vmix_http = line
                    .strip_prefix("FREEMIXD_VMIX_HTTP_READY\tv=1\taddress=")
                    .and_then(|address| address.parse::<SocketAddr>().ok());
                if sender.send((line, vmix, vmix_http)).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut control_ready = false;
        let mut vmix = None;
        let mut vmix_http = None;
        while Instant::now() < deadline {
            let Ok((line, tcp, http)) = receiver.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            if line.starts_with("FREEMIXD_READY") {
                control_ready = true;
            }
            if tcp.is_some() {
                vmix = tcp;
            }
            if http.is_some() {
                vmix_http = http;
            }
            if control_ready && vmix.is_some() && vmix_http.is_some() {
                break;
            }
        }
        let (Some(vmix), Some(vmix_http)) = (vmix, vmix_http) else {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("daemon did not report vMix listeners: {stderr}");
        };
        assert!(control_ready, "daemon did not report control readiness");
        Self {
            child,
            vmix,
            vmix_http,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_line(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line
}

#[test]
fn the_daemon_serves_the_vmix_tcp_api() {
    let directory = TestDirectory::new("api");
    save_project(&directory.project());
    let daemon = Daemon::start(&directory.project());
    let stream = TcpStream::connect(daemon.vmix).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);

    writer.write_all(b"FUNCTION Cut\r\n").unwrap();
    assert_eq!(read_line(&mut reader), "FUNCTION OK Completed\r\n");

    writer
        .write_all(b"FUNCTION AddInput Value=http://127.0.0.1:9/stream&Input=Extra\r\n")
        .unwrap();
    assert_eq!(read_line(&mut reader), "FUNCTION OK Completed\r\n");

    writer
        .write_all(b"FUNCTION SetBalance Input=1&Value=-0.5\r\n")
        .unwrap();
    assert_eq!(read_line(&mut reader), "FUNCTION OK Completed\r\n");
    writer
        .write_all(b"FUNCTION SetBalance Input=1&Value=2\r\n")
        .unwrap();
    assert!(
        read_line(&mut reader).starts_with("FUNCTION ER invalid parameter Value"),
        "an out-of-range balance is refused"
    );

    writer.write_all(b"FUNCTION Nope\r\n").unwrap();
    assert_eq!(
        read_line(&mut reader),
        "FUNCTION ER unknown function: Nope\r\n"
    );

    writer.write_all(b"XML\r\n").unwrap();
    let header = read_line(&mut reader);
    let length: usize = header
        .strip_prefix("XML ")
        .unwrap()
        .trim_end()
        .parse()
        .unwrap();
    let mut xml = vec![0; length];
    reader.read_exact(&mut xml).unwrap();
    let xml = String::from_utf8(xml).unwrap();
    assert!(xml.starts_with("<vmix>"), "{xml}");
    assert!(xml.contains("<inputs><input key="), "{xml}");
    assert!(xml.contains("title=\"Extra\""), "{xml}");
    assert!(xml.contains("<active>"), "{xml}");

    // MoveInput puts the added input at the front of the vMix order.
    writer
        .write_all(b"FUNCTION MoveInput Input=Extra&Value=1\r\n")
        .unwrap();
    assert_eq!(read_line(&mut reader), "FUNCTION OK Completed\r\n");
    writer.write_all(b"XML\r\n").unwrap();
    let header = read_line(&mut reader);
    let length: usize = header
        .strip_prefix("XML ")
        .unwrap()
        .trim_end()
        .parse()
        .unwrap();
    let mut xml = vec![0; length];
    reader.read_exact(&mut xml).unwrap();
    let xml = String::from_utf8(xml).unwrap();
    let extra = xml.find("title=\"Extra\"").unwrap();
    let solid = xml.find("title=\"Solid\"").unwrap();
    assert!(extra < solid, "{xml}");

    // The added input can be removed again by name once nothing references
    // it.
    writer
        .write_all(b"FUNCTION RemoveInput Input=Extra\r\n")
        .unwrap();
    assert_eq!(read_line(&mut reader), "FUNCTION OK Completed\r\n");

    writer.write_all(b"XML\r\n").unwrap();
    let header = read_line(&mut reader);
    let length: usize = header
        .strip_prefix("XML ")
        .unwrap()
        .trim_end()
        .parse()
        .unwrap();
    let mut xml = vec![0; length];
    reader.read_exact(&mut xml).unwrap();
    let xml = String::from_utf8(xml).unwrap();
    assert!(!xml.contains("title=\"Extra\""), "{xml}");
}

#[test]
fn the_daemon_serves_the_vmix_http_api() {
    let directory = TestDirectory::new("http-api");
    save_project(&directory.project());
    let daemon = Daemon::start(&directory.project());

    let mut stream = TcpStream::connect(daemon.vmix_http).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(b"GET /?Function=Cut HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.ends_with("\r\n\r\nOK"), "{response}");

    let mut stream = TcpStream::connect(daemon.vmix_http).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("Content-Type: text/xml"), "{response}");
    assert!(response.contains("<vmix>"), "{response}");
}
