//! The daemon binary's unit tests: one test module moved out of `main.rs`
//! so the composition root stays readable.

use fm_daemon_arguments::{
    DEFAULT_LISTEN, MAX_RECORD_AUDIO_BITRATE_KBPS, MAX_RECORD_VIDEO_BITRATE_KBPS,
    MIN_RECORD_AUDIO_BITRATE_KBPS, MIN_RECORD_VIDEO_BITRATE_KBPS, parse_diagnostic_duration,
};
use fm_io_api::display::DisplaySelector;
use std::{
    cell::Cell,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc::TryRecvError,
};

use fm_control::{LiveEvent, Subscription};
#[cfg(all(feature = "native-media", target_os = "macos"))]
use fm_io_macos::{CameraIdKind, deterministic_camera_id};
use fm_model::{
    Input, InputAudioStripState, InputBalanceBasisPoints, InputGainMilliDb, InputKind, Project,
    ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, StingerConfig,
    StingerSlotNumber,
};
#[cfg(feature = "native-media")]
use fm_model::{Rgba8 as ModelRgba8, Scene};
use fm_protocol::{ClientType, Role, StreamRealizedState};
#[cfg(feature = "native-media")]
use fm_types::SceneId;
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, SampleFormat, SampleRate,
    ScanMode, VideoDimensions, VideoFormat,
};
#[cfg(feature = "native-media")]
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

use super::*;

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_discovery(permission: u8) -> Vec<u8> {
    camera_discovery_for(permission, &[("fake-camera", "Fake Camera")])
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_discovery_for(permission: u8, cameras: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = b"FMCAMD2\0".to_vec();
    bytes.push(permission);
    bytes.extend_from_slice(&u32::try_from(cameras.len()).unwrap().to_le_bytes());
    for &(native_id, name) in cameras {
        for value in [native_id, name] {
            bytes.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&30_000_u32.to_le_bytes());
        bytes.extend_from_slice(&1_001_u32.to_le_bytes());
    }
    bytes
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_frame(sequence: u64, pts_micros: i64) -> Vec<u8> {
    let mut bytes = b"FMCAMF3\0".to_vec();
    bytes.extend_from_slice(&62_u32.to_le_bytes());
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&2_u64.to_le_bytes());
    bytes.extend_from_slice(&pts_micros.to_le_bytes());
    bytes.extend_from_slice(&1_000_i32.to_le_bytes());
    bytes.extend_from_slice(&1_001_i64.to_le_bytes());
    bytes.extend_from_slice(&30_000_i32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2]);
    bytes.extend_from_slice(&[3, 5, 7, 255]);
    bytes
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_slot_test_frame(sequence: u64, discontinuity: bool) -> CpuVideoFrame {
    let mut source = SimulatedVideoSource::new(
        1,
        1,
        FrameRate::new(30, 1).unwrap(),
        native_clock_domain(),
        SourcePattern::Solid(Rgba8::new(1, 2, 3, 255)),
    )
    .unwrap();
    let frame = source.next_frame().unwrap().unwrap();
    let original = frame.timing();
    let mut timing = MediaTiming::new(
        original.original_timestamp(),
        original.presentation_timestamp(),
        original.duration(),
        original.clock_domain(),
        SequenceNumber::new(sequence),
    )
    .unwrap();
    if discontinuity {
        timing = timing.with_flags(fm_frame::MediaFlags::DISCONTINUITY);
    }
    let metadata = frame.metadata();
    let frame = CpuVideoFrame::new(timing, frame.into_payload());
    if let Some(metadata) = metadata {
        frame.with_metadata(metadata).unwrap()
    } else {
        frame
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn shell_octal(bytes: &[u8]) -> String {
    use fmt::Write as _;

    let mut output = String::new();
    for byte in bytes {
        write!(output, "\\{byte:03o}").unwrap();
    }
    output
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn fake_camera_helper(directory: &Path, permission: u8) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("camera-helper.sh");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture) printf '%s\\n' \"$@\" > \"$0.capture\"; printf '%s' \"$$\" > \"$0.pid\"; printf '{}'; exec sleep 30 ;;\n  request-permission) touch \"$0.permission\"; exit 91 ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&camera_discovery(permission)),
        shell_octal(&camera_frame(7, 1_000)),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn recovering_camera_helper(
    directory: &Path,
    recover_on_capture: Option<u32>,
    initial_exit: i32,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("recovering-camera-helper.sh");
    let recovery_case = recover_on_capture.map_or_else(String::new, |capture| {
        format!(
            "  {capture}) sleep 0.05; printf '{}'; exec sleep 30 ;;\n",
            shell_octal(&camera_frame(0, 2_000))
        )
    });
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$@\" >> \"$0.capture\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$count\" in\n  1) printf '{}'; sleep 0.10; exit {initial_exit} ;;\n{recovery_case}  *) printf '{}'; sleep 0.03; exit 20 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&camera_discovery(0)),
        shell_octal(&camera_frame(7, 1_000)),
        shell_octal(b"FMCAMF3\0"),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn hanging_recovery_camera_helper(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("hanging-recovery-camera-helper.sh");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$count\" in\n      1) printf '{}'; sleep 0.10; exit 20 ;;\n      *) printf '{}'; exec sleep 30 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&camera_discovery(0)),
        shell_octal(&camera_frame(7, 1_000)),
        shell_octal(b"FMCAMF3\0"),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn malformed_runtime_camera_helper(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("malformed-runtime-camera-helper.sh");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    printf '{}'; sleep 0.15; printf '\\001\\000\\000\\000'; exec sleep 30 ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&camera_discovery(0)),
        shell_octal(&camera_frame(7, 1_000)),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn many_camera_startup_failure_helper(directory: &Path, count: usize) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("many-camera-startup-failure-helper.sh");
    let records = (0..count)
        .map(|index| {
            (
                format!("fake-camera-{index}"),
                format!("Fake Camera {index}"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = records
        .iter()
        .map(|(id, name)| (id.as_str(), name.as_str()))
        .collect::<Vec<_>>();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$2\" in\n      fake-camera-0) printf 'BADFRAME'; exec sleep 30 ;;\n      *) printf '{}'; exec sleep 30 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&camera_discovery_for(0, &borrowed)),
        shell_octal(&camera_frame(7, 1_000)),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn two_camera_helper(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("two-camera-helper.sh");
    let discovery = camera_discovery_for(
        0,
        &[
            ("fake-camera-a", "Fake Camera A"),
            ("fake-camera-b", "Fake Camera B"),
        ],
    );
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    printf '%s\\n' \"$@\" >> \"$0.capture\"\n    case \"$2\" in\n      fake-camera-a)\n        count=0\n        if test -f \"$0.a.count\"; then read count < \"$0.a.count\"; fi\n        count=$((count + 1))\n        printf '%s' \"$count\" > \"$0.a.count\"\n        case \"$count\" in\n          1) printf '{}'; sleep 0.10; exit 20 ;;\n          *) sleep 0.05; printf '{}'; exec sleep 30 ;;\n        esac ;;\n      fake-camera-b) printf '{}'; exec sleep 30 ;;\n      *) exit 89 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
        shell_octal(&discovery),
        shell_octal(&camera_frame(7, 1_000)),
        shell_octal(&camera_frame(0, 2_000)),
        shell_octal(&camera_frame(11, 1_000)),
    );
    fs::write(&helper, script).unwrap();
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions).unwrap();
    helper
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn test_camera_recovery_policy(max_attempts: u32, rearm_backoff: Duration) -> CameraRecoveryPolicy {
    CameraRecoveryPolicy {
        max_attempts,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(10),
        rearm_backoff,
        shutdown_timeout: Duration::from_secs(4),
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn helper_process_exists(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn assert_helper_processes_reaped(path: &Path) {
    let pids = fs::read_to_string(path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && pids.lines().any(helper_process_exists) {
        thread::sleep(Duration::from_millis(10));
    }
    for pid in pids.lines() {
        assert!(
            !helper_process_exists(pid),
            "camera helper {pid} survived cleanup"
        );
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_test_project(stable_key: &str) -> StoredProject {
    let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(43).unwrap()),
        "Camera Unit Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1, 1).unwrap(),
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
    project.add_input(Input {
        id: test_input_id(1),
        name: "Camera".into(),
        kind: InputKind::Device {
            stable_key: stable_key.into(),
            capture: fm_types::capture::CaptureSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Bars".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn two_camera_test_project(first_key: &str, second_key: &str) -> StoredProject {
    let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(44).unwrap()),
        "Two Camera Unit Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1, 1).unwrap(),
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
    for (value, key) in [(1, first_key), (2, second_key)] {
        project.add_input(Input {
            id: test_input_id(value),
            name: format!("Camera {value}"),
            kind: InputKind::Device {
                stable_key: key.to_owned(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn many_camera_test_project(keys: &[String]) -> StoredProject {
    let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(45).unwrap()),
        "Many Camera Unit Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1, 1).unwrap(),
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
    for (index, key) in keys.iter().enumerate() {
        let value = u128::try_from(index + 1).unwrap();
        project.add_input(Input {
            id: test_input_id(value),
            name: format!("Camera {value}"),
            kind: InputKind::Device {
                stable_key: key.clone(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

struct FailingSaver;

impl DurableStore for FailingSaver {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        Err(AppFailure("injected save failure".into()).into())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Err(AppFailure("injected save failure".into()).into())
    }
}

struct ObservingSaver<'a> {
    subscription: &'a Subscription,
    saved: RefCell<Option<StoredProject>>,
}

impl ObservingSaver<'_> {
    fn observe(&self, project: &StoredProject) {
        assert_eq!(self.subscription.try_recv(), Err(TryRecvError::Empty));
        self.saved.replace(Some(project.clone()));
    }
}

impl DurableStore for ObservingSaver<'_> {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()> {
        self.observe(updated);
        Ok(())
    }

    fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
        self.observe(project);
        Ok(())
    }
}

#[derive(Default)]
struct CountingSaver(Cell<u32>);

impl DurableStore for CountingSaver {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        self.0.set(self.0.get() + 1);
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        self.0.set(self.0.get() + 1);
        Ok(())
    }
}

struct CrashAfterSave<'a>(&'a ProjectStore);

impl CrashAfterSave<'_> {
    fn save_then_crash(&self, project: &StoredProject) -> AppResult<()> {
        ProjectStore::save(self.0, project)?;
        panic!("simulated crash after save");
    }
}

impl DurableStore for CrashAfterSave<'_> {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()> {
        self.save_then_crash(updated)
    }

    fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
        self.save_then_crash(project)
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

/// `serve show.freemix --native-media` plus the given recorder options.
fn record_args(extra: &[&str]) -> Vec<String> {
    let mut values = strings(&["serve", "show.freemix", "--native-media"]);
    values.extend(extra.iter().map(ToString::to_string));
    values
}

#[test]
fn parses_serve_options() {
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--listen",
            "127.0.0.1:9123",
            "--once",
        ]))
        .unwrap(),
        Command::Serve {
            project: "show.freemix".into(),
            listen: "127.0.0.1:9123".parse().unwrap(),
            web_listen: None,
            web: web::WebSettings::default(),
            status_listen: None,
            hls_listen: None,
            live_lan_listen: None,
            once: true,
            native_media: false,
            fullscreen_program: false,
            display_outputs: Vec::new(),
            display_inventory: None,
            presenter_fallback: false,
            camera_helper: None,
            capture_inventory: None,
            record_program: None,
            record_output: None,
            record_program_2: None,
            record_output_2: None,
            record_wav: false,
            record_segment_seconds: [None; RECORDER_SLOTS],
            record_bitrate_kbps: None,
            record_bitrate_kbps_2: None,
            record_audio_bitrate_kbps: None,
            record_audio_bitrate_kbps_2: None,
            record_fallback_dir: None,
            diagnostic_stop_after: None,
            recover_to_checkpoint: false,
        }
    );
    let web = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--web-listen",
        "127.0.0.1:9234",
    ]))
    .unwrap();
    assert!(matches!(
        web,
        Command::Serve {
            web_listen: Some(address),
            once: false,
            native_media: false,
            ..
        } if address == "127.0.0.1:9234".parse::<SocketAddr>().unwrap()
    ));
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--once",
        ]))
        .is_err()
    );
    // The native daemon serves the gateway too.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--native-media",
        ]))
        .unwrap(),
        Command::Serve {
            web_listen: Some(_),
            native_media: true,
            ..
        }
    ));
    // The windowed program daemon serves the gateway too.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--native-media",
            "--fullscreen-program",
        ]))
        .unwrap(),
        Command::Serve {
            web_listen: Some(_),
            fullscreen_program: true,
            ..
        }
    ));
}

#[test]
fn default_serve_is_ephemeral_loopback() {
    let command = parse_args(strings(&["serve", "show.freemix"])).unwrap();
    assert!(matches!(
        command,
        Command::Serve { listen, once: false, native_media: false, .. }
            if listen == DEFAULT_LISTEN.parse::<SocketAddr>().unwrap()
    ));
}

/// OR-017: `--live-lan-listen` takes any address (a LAN one included)
/// but needs native media.
#[test]
fn parses_live_lan_listen_as_a_native_option() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--live-lan-listen",
            "0.0.0.0:8090",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--live-lan-listen=0.0.0.0:8090",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                live_lan_listen: Some(address),
                native_media: true,
                ..
            } if address == "0.0.0.0:8090".parse().unwrap()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--live-lan-listen", "0.0.0.0:8090"]),
            "--live-lan-listen requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--live-lan-listen=0.0.0.0:1",
                "--live-lan-listen",
                "0.0.0.0:2",
            ]),
            "duplicate option `--live-lan-listen`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--live-lan-listen",
            ]),
            "missing value for --live-lan-listen",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_hls_listen_only_as_a_loopback_native_option() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen",
            "127.0.0.1:0",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen=127.0.0.1:9432",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                hls_listen: Some(address),
                native_media: true,
                ..
            } if address.ip().is_loopback()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen",
                "0.0.0.0:9000",
            ]),
            "--hls-listen must use a loopback address",
        ),
        (
            strings(&["serve", "show.freemix", "--hls-listen", "127.0.0.1:9000"]),
            "--hls-listen requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--hls-listen",
                "127.0.0.1:9000",
            ]),
            "--hls-listen cannot be combined with --fullscreen-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen",
                "127.0.0.1:9001",
                "--hls-listen",
                "127.0.0.1:9002",
            ]),
            "duplicate option `--hls-listen`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen=127.0.0.1:9001",
                "--hls-listen=127.0.0.1:9002",
            ]),
            "duplicate option `--hls-listen`",
        ),
        (
            strings(&["serve", "show.freemix", "--native-media", "--hls-listen"]),
            "missing value for --hls-listen",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
    // Like `--status-listen`, the `=` form parses its value directly, so an
    // empty value is an invalid address rather than a missing argument.
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen=",
        ]))
        .unwrap_err()
        .to_string()
        .contains("invalid hls listen address")
    );
}

#[test]
fn hls_token_bounds_helper_requires_bounded_ascii_graphic_bytes() {
    let short = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES - 1];
    assert!(!hls_server::valid_token(&short));
    let minimum = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES];
    assert!(hls_server::valid_token(&minimum));
    let maximum = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES];
    assert!(hls_server::valid_token(&maximum));
    let long = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES + 1];
    assert!(!hls_server::valid_token(&long));
    for byte in [b' ', b'\n', b'\r', 0x7f] {
        let mut non_graphic = minimum;
        non_graphic[0] = byte;
        assert!(!hls_server::valid_token(&non_graphic), "{byte:#04x}");
    }
}

#[cfg(feature = "native-media")]
#[test]
fn hls_roots_helper_skips_non_hls_destinations_and_keys_hls_targets() {
    let roots = hls_roots_from_destinations([
        (OutputProtocol::Rtmp, 1, PathBuf::from("/srv/rtmp")),
        (OutputProtocol::Hls, 3, PathBuf::from("/srv/hls-a")),
        (OutputProtocol::Srt, 2, PathBuf::from("/srv/srt")),
        (OutputProtocol::Hls, 4, PathBuf::from("/srv/hls-b")),
    ])
    .unwrap();
    assert_eq!(
        roots.resolve(3, "index.m3u8"),
        Ok(PathBuf::from("/srv/hls-a/index.m3u8"))
    );
    assert_eq!(
        roots.resolve(4, "7/segment-00042.ts"),
        Ok(PathBuf::from("/srv/hls-b/7/segment-00042.ts"))
    );
    assert_eq!(
        roots.resolve(1, "index.m3u8"),
        Err(hls_server::HlsRouteError::NotFound)
    );
    assert_eq!(
        roots.resolve(2, "index.m3u8"),
        Err(hls_server::HlsRouteError::NotFound)
    );
    assert!(matches!(
        hls_roots_from_destinations([
            (OutputProtocol::Hls, 5, PathBuf::from("/srv/one")),
            (OutputProtocol::Hls, 5, PathBuf::from("/srv/two")),
        ]),
        Err(hls_server::HlsRouteError::DuplicateTarget)
    ));
}

#[test]
fn parses_native_media_as_an_opt_in() {
    assert!(matches!(
        parse_args(strings(&["serve", "show.freemix", "--native-media"])).unwrap(),
        Command::Serve {
            native_media: true,
            ..
        }
    ));
}

#[test]
fn rejects_duplicate_listen_option() {
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--listen",
        "127.0.0.1:4100",
        "--listen",
        "127.0.0.1:4200",
    ]))
    .unwrap_err();
    assert_eq!(error.to_string(), "duplicate option `--listen`");
}

#[test]
fn rejects_duplicate_native_media_option() {
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--native-media",
    ]))
    .unwrap_err();
    assert_eq!(error.to_string(), "duplicate option `--native-media`");
}

#[test]
fn parses_camera_helper_only_with_native_media() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper",
            "helper-bin",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=helper-bin",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                camera_helper: Some(path),
                ..
            } if path == Path::new("helper-bin")
        ));
    }
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--camera-helper",
            "helper-bin",
        ]))
        .unwrap_err()
        .to_string(),
        "--camera-helper requires --native-media"
    );
    for arguments in [
        strings(&["serve", "show.freemix", "--native-media", "--camera-helper"]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=",
        ]),
    ] {
        assert_eq!(
            parse_args(arguments).unwrap_err().to_string(),
            "missing value for --camera-helper"
        );
    }
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=a",
            "--camera-helper",
            "b",
        ]))
        .unwrap_err()
        .to_string(),
        "duplicate option `--camera-helper`"
    );
}

#[test]
fn parses_bounded_diagnostic_stop_duration() {
    assert_eq!(
        parse_diagnostic_duration("1500ms").unwrap(),
        Duration::from_millis(1_500)
    );
    assert_eq!(
        parse_diagnostic_duration("2s").unwrap(),
        Duration::from_secs(2)
    );
    assert_eq!(
        parse_diagnostic_duration("3m").unwrap(),
        Duration::from_mins(3)
    );
    assert_eq!(
        parse_diagnostic_duration("24h").unwrap(),
        Duration::from_hours(24)
    );
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--diagnostic-stop-after=10m",
        ]))
        .unwrap(),
        Command::Serve {
            diagnostic_stop_after: Some(duration),
            ..
        } if duration == Duration::from_mins(10)
    ));
}

#[test]
fn rejects_invalid_diagnostic_stop_options() {
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--once",
                "--diagnostic-stop-after",
                "1s",
            ]),
            "--diagnostic-stop-after cannot be combined with --once",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--diagnostic-stop-after",
                "1s",
            ]),
            "--diagnostic-stop-after currently supports headless native mode only",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "0s",
            ]),
            "diagnostic duration must be nonzero",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "25h",
            ]),
            "diagnostic duration cannot exceed 24h",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "1s",
                "--diagnostic-stop-after=2s",
            ]),
            "duplicate option `--diagnostic-stop-after`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_program_recording_with_once_and_fullscreen() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--record-program",
            "capture.mp4",
            "--once",
        ]))
        .unwrap(),
        Command::Serve {
            once: true,
            native_media: true,
            fullscreen_program: true,
            record_program: Some(path),
            ..
        } if path == Path::new("capture.mp4")
    ));
}

#[test]
fn parses_equals_program_recording_with_option_like_file_name() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program=--capture.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(path),
            ..
        } if path == Path::new("--capture.mp4")
    ));
}

#[test]
fn rejects_invalid_program_recording_option_relationships() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-program", "capture.mp4"]),
            "--record-program requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "--once",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "one.mp4",
                "--record-program",
                "two.mp4",
            ]),
            "duplicate option `--record-program`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=one.mp4",
                "--record-program=two.mp4",
            ]),
            "duplicate option `--record-program`",
        ),
        (
            strings(&["serve", "show.freemix", "--native-media", "--record-wav"]),
            "--record-wav requires --record-program or --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-wav",
                "--record-wav",
            ]),
            "duplicate option `--record-wav`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_invalid_second_program_recording_options() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-program-2", "capture.mp4"]),
            "--record-program-2 requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "--once",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "one.mp4",
                "--record-program-2",
                "two.mp4",
            ]),
            "duplicate option `--record-program-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=one.mp4",
                "--record-program-2=two.mp4",
            ]),
            "duplicate option `--record-program-2`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_second_program_recording_slot() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2=backup.mp4",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_program: None,
                record_program_2: Some(path),
                ..
            } if path == Path::new("backup.mp4")
        ));
    }
    // An option-like file name stays reachable through the `=` form.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2=--backup.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program_2: Some(path),
            ..
        } if path == Path::new("--backup.mp4")
    ));
    // Both slots configure side by side from one desired flag.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-program-2",
            "backup.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(primary),
            record_program_2: Some(secondary),
            ..
        } if primary == Path::new("capture.mp4") && secondary == Path::new("backup.mp4")
    ));
}

#[test]
fn parses_wav_sidecar_for_the_second_recorder_slot() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
            "--record-wav",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: None,
            record_program_2: Some(path),
            record_wav: true,
            ..
        } if path == Path::new("backup.mp4")
    ));
}

#[test]
fn parses_program_wav_sidecar_recording() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-wav",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(path),
            record_wav: true,
            ..
        } if path == Path::new("capture.mp4")
    ));
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_wav: false,
            ..
        }
    ));
}

#[test]
fn parses_record_output_for_both_recorder_slots() {
    let output_a = fm_types::OutputId::new(NonZeroU128::new(10).unwrap());
    let output_b = fm_types::OutputId::new(NonZeroU128::new(20).unwrap());
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-output",
            "10",
            "--record-program-2",
            "backup.mp4",
            "--record-output-2",
            "20",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program=capture.mp4",
            "--record-output=10",
            "--record-program-2=backup.mp4",
            "--record-output-2=20",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_program: Some(primary),
                record_output: Some(primary_output),
                record_program_2: Some(secondary),
                record_output_2: Some(secondary_output),
                ..
            } if primary == Path::new("capture.mp4")
                && primary_output == output_a
                && secondary == Path::new("backup.mp4")
                && secondary_output == output_b
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn rejects_invalid_record_output_options_and_values() {
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-output",
                "1",
            ]),
            "--record-output requires --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output-2",
                "2",
            ]),
            "--record-output-2 requires --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output",
                "1",
            ]),
            "--record-output requires --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "--once",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "--once",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "1",
                "--record-output",
                "2",
            ]),
            "duplicate option `--record-output`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=1",
                "--record-output=2",
            ]),
            "duplicate option `--record-output`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "1",
                "--record-output-2",
                "2",
            ]),
            "duplicate option `--record-output-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=1",
                "--record-output-2=2",
            ]),
            "duplicate option `--record-output-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "0",
            ]),
            "invalid value for --record-output `0`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=abc",
            ]),
            "invalid value for --record-output `abc`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "0",
            ]),
            "invalid value for --record-output-2 `0`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=-5",
            ]),
            "invalid value for --record-output-2 `-5`; expected a positive integer output id",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_record_segment_seconds_per_slot_with_either_form_and_inclusive_bounds() {
    let seconds = |value| NonZeroU32::new(value).unwrap();
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "1",
            ]),
            [Some(seconds(1)), None],
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=86400",
            ]),
            [Some(seconds(86_400)), None],
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "1",
            ]),
            [None, Some(seconds(1))],
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=86400",
            ]),
            [None, Some(seconds(86_400))],
        ),
        (
            record_args(&[
                "--record-program=capture.mp4",
                "--record-program-2=backup.mp4",
                "--record-segment-seconds",
                "300",
                "--record-segment-seconds-2=600",
            ]),
            [Some(seconds(300)), Some(seconds(600))],
        ),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_segment_seconds,
                ..
            } if record_segment_seconds == expected
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn rejects_invalid_record_segment_seconds_options_and_values() {
    for (arguments, expected) in [
        (
            record_args(&["--record-segment-seconds", "60"]),
            "--record-segment-seconds requires --record-program",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds",
                "60",
            ]),
            "--record-segment-seconds requires --record-program",
        ),
        (
            record_args(&["--record-segment-seconds-2", "60"]),
            "--record-segment-seconds-2 requires --record-program-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds-2",
                "60",
            ]),
            "--record-segment-seconds-2 requires --record-program-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "--once",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "--once",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "10",
                "--record-segment-seconds",
                "20",
            ]),
            "duplicate option `--record-segment-seconds`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=10",
                "--record-segment-seconds=20",
            ]),
            "duplicate option `--record-segment-seconds`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "10",
                "--record-segment-seconds-2",
                "20",
            ]),
            "duplicate option `--record-segment-seconds-2`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=10",
                "--record-segment-seconds-2=20",
            ]),
            "duplicate option `--record-segment-seconds-2`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "0",
            ]),
            "invalid value for --record-segment-seconds `0`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=86401",
            ]),
            "invalid value for --record-segment-seconds `86401`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=abc",
            ]),
            "invalid value for --record-segment-seconds `abc`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "0",
            ]),
            "invalid value for --record-segment-seconds-2 `0`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=86401",
            ]),
            "invalid value for --record-segment-seconds-2 `86401`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=abc",
            ]),
            "invalid value for --record-segment-seconds-2 `abc`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=-5",
            ]),
            "invalid value for --record-segment-seconds `-5`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=-5",
            ]),
            "invalid value for --record-segment-seconds-2 `-5`; expected an integer in 1..=86400",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[cfg(feature = "native-media")]
#[test]
fn slot_segment_seconds_selects_each_slots_own_rotation_duration() {
    let first_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
        [Some(NonZeroU32::new(10).unwrap()), None];
    assert_eq!(
        slot_segment_seconds(first_only, 0).map(NonZeroU32::get),
        Some(10)
    );
    assert_eq!(slot_segment_seconds(first_only, 1), None);

    let second_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
        [None, Some(NonZeroU32::new(86_400).unwrap())];
    assert_eq!(slot_segment_seconds(second_only, 0), None);
    assert_eq!(
        slot_segment_seconds(second_only, 1).map(NonZeroU32::get),
        Some(86_400)
    );
}

#[test]
fn parses_record_bitrate_options_with_either_form_and_inclusive_bounds() {
    for (arguments, video, video_2, audio, audio_2) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "2000",
                "--record-audio-bitrate-kbps",
                "96",
            ]),
            Some(2_000),
            None,
            Some(96),
            None,
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2=3000",
                "--record-audio-bitrate-kbps-2=128",
            ]),
            None,
            Some(3_000),
            None,
            Some(128),
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=capture.mp4",
                "--record-program-2=backup.mp4",
                "--record-bitrate-kbps=1000",
                "--record-bitrate-kbps-2",
                "100000",
                "--record-audio-bitrate-kbps",
                "8",
                "--record-audio-bitrate-kbps-2=1024",
            ]),
            Some(MIN_RECORD_VIDEO_BITRATE_KBPS),
            Some(MAX_RECORD_VIDEO_BITRATE_KBPS),
            Some(MIN_RECORD_AUDIO_BITRATE_KBPS),
            Some(MAX_RECORD_AUDIO_BITRATE_KBPS),
        ),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_bitrate_kbps,
                record_bitrate_kbps_2,
                record_audio_bitrate_kbps,
                record_audio_bitrate_kbps_2,
                ..
            } if record_bitrate_kbps == video
                && record_bitrate_kbps_2 == video_2
                && record_audio_bitrate_kbps == audio
                && record_audio_bitrate_kbps_2 == audio_2
        ));
    }
}

#[test]
fn parses_and_validates_the_record_fallback_directory() {
    for flag in [
        &["--record-fallback-dir", "spare"][..],
        &["--record-fallback-dir=spare"][..],
    ] {
        let mut arguments = vec![
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
        ];
        arguments.extend_from_slice(flag);
        assert!(matches!(
            parse_args(strings(&arguments)).unwrap(),
            Command::Serve {
                record_fallback_dir: Some(directory),
                ..
            } if directory == Path::new("spare")
        ));
    }
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
            "--record-fallback-dir",
            "spare",
        ]))
        .unwrap(),
        Command::Serve {
            record_fallback_dir: Some(_),
            ..
        }
    ));
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-fallback-dir", "spare"]),
            "--record-fallback-dir requires --record-program or --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir",
            ]),
            "missing value for --record-fallback-dir",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir=",
            ]),
            "missing value for --record-fallback-dir",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir",
                "a",
                "--record-fallback-dir=b",
            ]),
            "duplicate option `--record-fallback-dir`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir",
                "spare",
            ]),
            "--record-fallback-dir currently supports headless native mode only",
        ),
    ] {
        let error = parse_args(arguments).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[cfg(feature = "native-media")]
#[test]
fn recording_sessions_refuse_invalid_stems_and_a_fallback_in_the_output_directory() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("capture.mp4");
    validate_recording_sessions([("--record-program", Some(output.as_path()))], None).unwrap();
    let spaced = directory.path().join("my show.mp4");
    let error = validate_recording_sessions([("--record-program-2", Some(spaced.as_path()))], None)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("--record-program-2 output stem must be"),
        "{error}"
    );
    let error = validate_recording_sessions(
        [("--record-program", Some(output.as_path()))],
        Some(directory.path()),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        error,
        "--record-fallback-dir must differ from the --record-program output directory"
    );
    assert!(!error.contains(&*directory.path().to_string_lossy()));
    validate_recording_sessions(
        [("--record-program", Some(output.as_path()))],
        Some(&directory.path().join("spare")),
    )
    .unwrap();
}

#[test]
fn rejects_record_bitrate_options_without_their_recorder_slot_or_value() {
    for (arguments, expected) in [
        (
            record_args(&["--record-bitrate-kbps", "2000"]),
            "--record-bitrate-kbps requires --record-program",
        ),
        (
            record_args(&["--record-bitrate-kbps-2=2000"]),
            "--record-bitrate-kbps-2 requires --record-program-2",
        ),
        (
            record_args(&["--record-audio-bitrate-kbps", "96"]),
            "--record-audio-bitrate-kbps requires --record-program",
        ),
        (
            record_args(&["--record-audio-bitrate-kbps-2=96"]),
            "--record-audio-bitrate-kbps-2 requires --record-program-2",
        ),
        (
            record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps"]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "--once",
            ]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps="]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
            ]),
            "missing value for --record-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2=",
            ]),
            "missing value for --record-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
            ]),
            "missing value for --record-audio-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps=",
            ]),
            "missing value for --record-audio-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2",
            ]),
            "missing value for --record-audio-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=",
            ]),
            "missing value for --record-audio-bitrate-kbps-2",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_out_of_range_record_bitrate_options() {
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "999",
            ]),
            "invalid value for --record-bitrate-kbps `999`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=100001",
            ]),
            "invalid value for --record-bitrate-kbps `100001`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=abc",
            ]),
            "invalid value for --record-bitrate-kbps `abc`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
                "100001",
            ]),
            "invalid value for --record-bitrate-kbps-2 `100001`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
                "7",
            ]),
            "invalid value for --record-audio-bitrate-kbps `7`; expected an integer in 8..=1024",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps=1025",
            ]),
            "invalid value for --record-audio-bitrate-kbps `1025`; expected an integer in 8..=1024",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=-5",
            ]),
            "invalid value for --record-audio-bitrate-kbps-2 `-5`; expected an integer in 8..=1024",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_duplicate_record_bitrate_options() {
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "2000",
                "--record-bitrate-kbps",
                "3000",
            ]),
            "duplicate option `--record-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=2000",
                "--record-bitrate-kbps=3000",
            ]),
            "duplicate option `--record-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
                "2000",
                "--record-bitrate-kbps-2=3000",
            ]),
            "duplicate option `--record-bitrate-kbps-2`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
                "96",
                "--record-audio-bitrate-kbps=128",
            ]),
            "duplicate option `--record-audio-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=96",
                "--record-audio-bitrate-kbps-2=128",
            ]),
            "duplicate option `--record-audio-bitrate-kbps-2`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[cfg(feature = "native-media")]
#[test]
fn segment_frame_limit_rounds_up_to_cover_requested_seconds_at_integer_and_fractional_rates() {
    let sec = |value| NonZeroU32::new(value).unwrap();
    let rate_25 = FrameRate::new(25, 1).unwrap();
    let rate_30 = FrameRate::new(30, 1).unwrap();
    let rate_60 = FrameRate::new(60, 1).unwrap();
    let rate_23_976 = FrameRate::new(24_000, 1_001).unwrap();
    let rate_29_97 = FrameRate::new(30_000, 1_001).unwrap();
    let rate_59_94 = FrameRate::new(60_000, 1_001).unwrap();

    assert_eq!(segment_frame_limit(sec(1), rate_25), 25);
    assert_eq!(segment_frame_limit(sec(2), rate_30), 60);
    assert_eq!(segment_frame_limit(sec(10), rate_60), 600);
    assert_eq!(segment_frame_limit(sec(86_400), rate_60), 86_400 * 60);

    // Fractional NTSC rates round up so the segment spans at least the
    // requested wall duration and hits an exact frame boundary at 1001 s.
    assert_eq!(segment_frame_limit(sec(1), rate_23_976), 24);
    assert_eq!(segment_frame_limit(sec(1), rate_29_97), 30);
    assert_eq!(segment_frame_limit(sec(1), rate_59_94), 60);
    assert_eq!(segment_frame_limit(sec(1_001), rate_23_976), 24_000);
    assert_eq!(segment_frame_limit(sec(1_001), rate_29_97), 30_000);
    assert_eq!(segment_frame_limit(sec(1_001), rate_59_94), 60_000);
}

#[test]
fn recording_output_selection_validation_accepts_known_and_rejects_unknown_outputs() {
    let project = test_project_with_streams([false, false]).project().clone();
    let known_output = fm_types::OutputId::new(NonZeroU128::new(902).unwrap());
    let unknown_output = fm_types::OutputId::new(NonZeroU128::new(999).unwrap());

    validate_recording_output_selections(&project, None, None).unwrap();
    validate_recording_output_selections(&project, Some(known_output), Some(known_output)).unwrap();
    assert_eq!(
        validate_recording_output_selections(&project, Some(unknown_output), None)
            .unwrap_err()
            .to_string(),
        "--record-output selects unknown project output 999"
    );
    assert_eq!(
        validate_recording_output_selections(&project, Some(known_output), Some(unknown_output))
            .unwrap_err()
            .to_string(),
        "--record-output-2 selects unknown project output 999"
    );
}

#[test]
fn resolve_project_output_entry_selects_distinct_outputs_and_defaults_to_first() {
    fn resolve<'a>(
        selected: Option<fm_types::OutputId>,
        frames: &'a [(fm_types::OutputId, &'static str)],
        unbound: Option<&'a str>,
    ) -> Option<&'a str> {
        resolve_project_output_entry(
            selected,
            frames,
            unbound,
            |(id, _)| *id,
            |(_, label)| *label,
        )
    }

    let out_1 = fm_types::OutputId::new(NonZeroU128::new(1).unwrap());
    let out_2 = fm_types::OutputId::new(NonZeroU128::new(2).unwrap());
    let out_missing = fm_types::OutputId::new(NonZeroU128::new(3).unwrap());
    let outputs = [(out_1, "program-main"), (out_2, "program-clean")];

    assert_eq!(
        resolve(None, &outputs, Some("unbound")),
        Some("program-main")
    );
    assert_eq!(
        resolve(Some(out_1), &outputs, Some("unbound")),
        Some("program-main")
    );
    assert_eq!(
        resolve(Some(out_2), &outputs, Some("unbound")),
        Some("program-clean")
    );
    assert_eq!(resolve(Some(out_missing), &outputs, Some("unbound")), None);
    assert_eq!(resolve(None, &[], Some("unbound")), Some("unbound"));
    assert_eq!(resolve(Some(out_1), &[], Some("unbound")), Some("unbound"));
    assert_eq!(resolve(None, &[], None), None);
}

#[test]
fn capability_digest_distinguishes_recorder_modes() {
    assert_eq!(
        capabilities_digest(false, false, false),
        CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, false, false),
        NATIVE_MEDIA_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, false, true),
        PROGRAM_RECORDER_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, true, false),
        FULLSCREEN_PROGRAM_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, true, true),
        FULLSCREEN_PROGRAM_RECORDER_CAPABILITIES_DIGEST
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_telemetry_diagnostic_names_scoped_metrics_and_unavailable_gpu_time() {
    let origin = Instant::now();
    let adapter = fm_gpu::NativeAdapterInfo {
        name: "Test Adapter".to_owned(),
        backend: NativeBackend::Metal,
    };
    let mut telemetry = NativeRuntimeTelemetry::new(origin, &adapter);
    telemetry.audio_retained_bytes = 10;
    telemetry.audio_peak_retained_bytes = 11;
    telemetry.audio_reservation_requests = 12;
    telemetry.audio_reserved_blocks = 13;
    telemetry.audio_peak_reserved_blocks = 14;
    telemetry.audio_reserved_samples = 15;
    telemetry.audio_peak_reserved_samples = 16;
    telemetry.audio_reserved_bytes = 17;
    telemetry.audio_peak_reserved_bytes = 18;
    telemetry.audio_source_stalls = 19;
    telemetry.audio_positioned_blocks = 20;
    telemetry.audio_positioned_samples = 21;
    telemetry.audio_leading_silence_samples = 22;
    telemetry.audio_eos_padding_blocks = 23;
    telemetry.audio_eos_padding_samples = 24;
    let deadline = origin
        .checked_sub(Duration::from_millis(10))
        .expect("test deadline is representable");
    telemetry.observe_host_lateness(deadline, origin);
    let diagnostic = telemetry.diagnostic(Some(fm_gpu::PresentationTelemetry {
        pending_depth: 1,
        peak_pending_depth: 1,
        frames_dropped: 2,
        ..fm_gpu::PresentationTelemetry::default()
    }));

    assert!(diagnostic.starts_with("FREEMIXD_TELEMETRY\tv=4\t"));
    assert!(diagnostic.contains("\thost_lateness_samples_total=1\t"));
    assert!(diagnostic.contains("\thost_lateness_samples_retained=1\t"));
    assert!(diagnostic.contains("\thost_lateness_p50_ms=10.000\t"));
    assert!(diagnostic.contains("\taudio_retained_bytes=10\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_retained_bytes=11\t"));
    assert!(diagnostic.contains("\taudio_reservation_requests=12\t"));
    assert!(diagnostic.contains("\taudio_reserved_blocks=13\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_blocks=14\t"));
    assert!(diagnostic.contains("\taudio_reserved_samples=15\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_samples=16\t"));
    assert!(diagnostic.contains("\taudio_reserved_bytes=17\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_bytes=18\t"));
    assert!(diagnostic.contains("\taudio_source_stalls=19\t"));
    assert!(diagnostic.contains("\taudio_positioned_blocks=20\t"));
    assert!(diagnostic.contains("\taudio_positioned_samples=21\t"));
    assert!(diagnostic.contains("\taudio_leading_silence_samples=22\t"));
    assert!(diagnostic.contains("\taudio_eos_padding_blocks=23\t"));
    assert!(diagnostic.contains("\taudio_eos_padding_samples=24\t"));
    assert!(diagnostic.contains("\tpresentation_active=true\t"));
    assert!(diagnostic.contains("\tpresentation_pending_depth=1\t"));
    assert!(diagnostic.contains("\tpresentation_dropped=2\t"));
    assert!(diagnostic.contains("\trecorder_configured=false\t"));
    assert!(diagnostic.contains("\tcamera_configured_sources=0\t"));
    assert!(diagnostic.contains("\tcamera_frames_received=0\t"));
    assert!(diagnostic.contains("\tcamera_frames_ingested=0\t"));
    assert!(diagnostic.contains("\tgpu_backend=Metal\t"));
    assert!(diagnostic.contains("\tgpu_adapter=Test Adapter\t"));
    assert!(diagnostic.contains("\tgpu_timing=Unsupported\t"));
    assert!(diagnostic.contains("\tgpu_pass_samples_total=0\t"));
    assert!(diagnostic.contains("\tgpu_pass_samples_retained=0\t"));
    assert!(diagnostic.contains("\tgpu_pass_p50_ms=none\t"));
    assert!(diagnostic.contains("\tmetric_errors=0\t"));
    assert!(diagnostic.contains("\tdisk_alert=unknown\t"));
    assert!(diagnostic.ends_with("\tdisk_free_bytes=0"));
}

#[cfg(feature = "native-media")]
#[test]
fn disk_alert_label_maps_alert_states() {
    assert_eq!(disk_alert_label(None), "unknown");
    assert_eq!(disk_alert_label(Some(AlertState::Inactive)), "normal");
    assert_eq!(disk_alert_label(Some(AlertState::Firing)), "low");
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_source_test_telemetry() -> NativeCameraSourceTelemetry {
    NativeCameraSourceTelemetry {
        input: test_input_id(1),
        lifecycle: LifecycleState::Running,
        health: EndpointHealthState::Healthy,
        frames_received: 20,
        frames_ingested: 6,
        native_dropped: 2,
        queue_dropped: 1,
        queue_depth: 1,
        queue_peak_depth: 2,
        continuity_rejected: 0,
        recovery_timeout_discarded: 0,
        terminal_error_discarded: 1,
        terminal_trigger_discarded: 1,
        ready_delivery_depth: 1,
        ready_delivery_discarded: 1,
        cancellation_discarded: 1,
        supervisor_slot_replaced: 3,
        supervisor_slot_depth: 1,
        ingest_failed: 1,
        preflight_depth: 1,
        preflight_discarded: 1,
        recovery_attempts: 2,
        recovery_successes: 1,
        recovery_exhausted: 0,
        recovery_worker_failures: 0,
        recovery_outcome: CameraRecoveryOutcome::Recovered,
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_source_diagnostics_are_stable_bounded_and_saturating() {
    assert_eq!(lifecycle_label(LifecycleState::Closed), "closed");
    assert_eq!(lifecycle_label(LifecycleState::Open), "open");
    assert_eq!(lifecycle_label(LifecycleState::Running), "running");
    assert_eq!(lifecycle_label(LifecycleState::Lost), "lost");
    assert_eq!(lifecycle_label(LifecycleState::Recovering), "recovering");
    assert_eq!(health_label(EndpointHealthState::Healthy), "healthy");
    assert_eq!(health_label(EndpointHealthState::Degraded), "degraded");
    assert_eq!(health_label(EndpointHealthState::SignalLost), "signal_lost");
    assert_eq!(health_label(EndpointHealthState::Failed), "failed");

    let source = camera_source_test_telemetry();
    let diagnostic = source.diagnostic();
    assert!(
        diagnostic.starts_with(
            "FREEMIXD_CAMERA_SOURCE\tv=1\tclassification=diagnostic-not-certification\t"
        )
    );
    assert!(diagnostic.contains("\tsample_phase=pre_cleanup\t"));
    assert!(diagnostic.contains("\tsample_lifecycle=running\thealth=healthy\t"));
    assert!(diagnostic.contains("\trecovery_attempts=2\t"));
    assert!(diagnostic.contains("\tsupervisor_slot_replaced=3\t"));
    assert!(diagnostic.contains("\tsupervisor_slot_depth=1\t"));
    assert!(diagnostic.contains("\tterminal_error_discarded=1\t"));
    assert!(diagnostic.contains("\tterminal_trigger_discarded=1\t"));
    assert!(diagnostic.contains("\tready_delivery_depth=1\t"));
    assert!(diagnostic.contains("\tready_delivery_discarded=1\t"));
    assert!(diagnostic.contains("\tcancellation_discarded=1\t"));
    assert!(diagnostic.contains("\tingest_failed=1\t"));
    assert!(diagnostic.contains("\tpreflight_depth=1\t"));
    assert!(diagnostic.contains("\tpreflight_discarded=1\t"));
    assert!(diagnostic.contains("\trecovery_outcome=recovered"));
    assert!(diagnostic.len() < 768);
    assert_eq!(source.frames_received, source.accounted_frames());

    let mut second = source;
    second.input = test_input_id(2);
    let mut unsorted = [second, source];
    sort_camera_source_telemetry(&mut unsorted);
    assert_eq!(
        unsorted.map(|sample| sample.input),
        [test_input_id(1), test_input_id(2)]
    );

    let mut saturated = source;
    saturated.frames_received = u64::MAX;
    saturated.frames_ingested = u64::MAX;
    saturated.native_dropped = u64::MAX;
    saturated.queue_dropped = u64::MAX;
    saturated.queue_depth = u64::MAX;
    saturated.queue_peak_depth = u64::MAX;
    saturated.continuity_rejected = u64::MAX;
    saturated.recovery_timeout_discarded = u64::MAX;
    saturated.terminal_error_discarded = u64::MAX;
    saturated.terminal_trigger_discarded = u64::MAX;
    saturated.ready_delivery_depth = u64::MAX;
    saturated.ready_delivery_discarded = u64::MAX;
    saturated.cancellation_discarded = u64::MAX;
    saturated.supervisor_slot_replaced = u64::MAX;
    saturated.supervisor_slot_depth = u64::MAX;
    saturated.ingest_failed = u64::MAX;
    saturated.preflight_depth = u64::MAX;
    saturated.preflight_discarded = u64::MAX;
    saturated.recovery_attempts = u64::MAX;
    saturated.recovery_successes = u64::MAX;
    saturated.recovery_exhausted = u64::MAX;
    saturated.recovery_worker_failures = u64::MAX;
    let aggregate = aggregate_camera_telemetry(&[saturated, source]);
    assert_eq!(aggregate.configured_sources, 2);
    assert_eq!(aggregate.frames_received, u64::MAX);
    assert_eq!(aggregate.frames_ingested, u64::MAX);
    assert_eq!(aggregate.native_dropped, u64::MAX);
    assert_eq!(aggregate.queue_dropped, u64::MAX);
    assert_eq!(aggregate.queue_depth, u64::MAX);
    assert_eq!(aggregate.queue_peak_depth, u64::MAX);
    assert_eq!(aggregate.continuity_rejected, u64::MAX);
    assert_eq!(aggregate.recovery_timeout_discarded, u64::MAX);
    assert_eq!(aggregate.terminal_error_discarded, u64::MAX);
    assert_eq!(aggregate.terminal_trigger_discarded, u64::MAX);
    assert_eq!(aggregate.ready_delivery_depth, u64::MAX);
    assert_eq!(aggregate.ready_delivery_discarded, u64::MAX);
    assert_eq!(aggregate.cancellation_discarded, u64::MAX);
    assert_eq!(aggregate.supervisor_slot_replaced, u64::MAX);
    assert_eq!(aggregate.supervisor_slot_depth, u64::MAX);
    assert_eq!(aggregate.ingest_failed, u64::MAX);
    assert_eq!(aggregate.preflight_depth, u64::MAX);
    assert_eq!(aggregate.preflight_discarded, u64::MAX);
    assert_eq!(aggregate.recovery_attempts, u64::MAX);
    assert_eq!(aggregate.recovery_successes, u64::MAX);
    assert_eq!(aggregate.recovery_exhausted, u64::MAX);
    assert_eq!(aggregate.recovery_worker_failures, u64::MAX);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_latest_slot_carries_discontinuity_across_replacement_race() {
    let mut slot = CameraFrameSlot::default();
    slot.replace(camera_slot_test_frame(8, true));
    slot.replace(camera_slot_test_frame(9, false));
    slot.replace(camera_slot_test_frame(10, false));

    let newest = slot.take().unwrap();
    assert_eq!(newest.timing().sequence().get(), 10);
    assert!(
        newest
            .timing()
            .flags()
            .contains(fm_frame::MediaFlags::DISCONTINUITY)
    );
    assert_eq!(slot.replacements, 2);
    assert!(slot.take().is_none());
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_ingest_failure_is_an_exact_accounted_drop() {
    let mut frame = CameraFrameSlot::default();
    frame.replace(camera_slot_test_frame(7, false));
    let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
        frame,
        snapshot: CameraWorkerSnapshot {
            telemetry: CameraTelemetry {
                received: 1,
                ..CameraTelemetry::default()
            },
            lifecycle: LifecycleState::Running,
            health: EndpointHealthState::Healthy,
            recovery_attempts: 0,
            recovery_successes: 0,
            recovery_exhausted: 0,
            recovery_worker_failures: 0,
            recovery_outcome: CameraRecoveryOutcome::Never,
        },
    }));
    let mut cameras = NativeCameraInputs {
        inputs: vec![NativeCameraInput {
            input: test_input_id(1),
            source: None,
            worker: None,
            supervisor,
            recovery_policy: test_camera_recovery_policy(1, Duration::from_millis(10)),
            ingested_frames: 0,
            ingest_failed: 0,
            preflight_depth: 0,
            preflight_discarded: 0,
            last_ingested_sequence: None,
            last_ingested_discontinuity: false,
        }],
        telemetry_emitted: false,
    };

    let error = cameras
        .poll_with(|_, _| Err(AppFailure("injected ingest failure".to_owned()).into()))
        .unwrap_err();
    assert_eq!(error.to_string(), "injected ingest failure");
    let telemetry = cameras.source_telemetry()[0];
    assert_eq!(telemetry.frames_received, 1);
    assert_eq!(telemetry.frames_ingested, 0);
    assert_eq!(telemetry.ingest_failed, 1);
    assert_eq!(telemetry.queue_depth, 0);
    assert_eq!(telemetry.supervisor_slot_depth, 0);
    assert_eq!(telemetry.frames_received, telemetry.accounted_frames());
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_capability_is_configured_support_not_live_health() {
    let digest = capabilities_digest(true, false, true);
    let mut policy = RecorderCapturePolicy::default();
    assert!(policy.fail("backend:failed".to_owned(), false).is_some());
    assert_eq!(digest, capabilities_digest(true, false, true));
    assert!(!policy.active());
}

#[cfg(feature = "native-media")]
#[test]
fn startup_pair_barrier_requires_mux_output_and_prioritizes_failure() {
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 0, 0, false),
        StartupPairDecision::Pending
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 0, false),
        StartupPairDecision::Pending
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 1, false),
        StartupPairDecision::Ready
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 1, true),
        StartupPairDecision::Failed
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Failed, 1, 1, true),
        StartupPairDecision::Failed
    );
}

#[cfg(feature = "native-media")]
#[test]
fn next_record_segment_path_prefers_the_original_and_skips_existing_names() {
    let directory = tempfile::tempdir().unwrap();
    let base = |name: &str| directory.path().join(name);
    let exists = |path: &Path| path.exists();

    // Segment 1 is the configured path itself.
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show.mp4")
    );
    std::fs::write(base("show.mp4"), b"").unwrap();
    // Later segments are zero-padded `<stem>-<NNN>.mp4`, first free wins.
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show-002.mp4")
    );
    std::fs::write(base("show-002.mp4"), b"").unwrap();
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show-003.mp4")
    );
    // Gaps are filled by the first free name, not the last.
    std::fs::create_dir_all(directory.path().join("nested")).unwrap();
    let nested = |name: &str| directory.path().join("nested").join(name);
    for name in ["take.mp4", "take-002.mp4", "take-004.mp4"] {
        std::fs::write(nested(name), b"").unwrap();
    }
    assert_eq!(
        next_record_segment_path(&nested("take.mp4"), exists).unwrap(),
        nested("take-003.mp4")
    );
    // Compound stems keep everything before the final extension.
    std::fs::write(base("a.b.mp4"), b"").unwrap();
    assert_eq!(
        next_record_segment_path(&base("a.b.mp4"), exists).unwrap(),
        base("a.b-002.mp4")
    );
    // Non-.mp4 outputs and unnamed paths are refused outright.
    for refused in ["show.mkv", "show", ".mp4"] {
        assert!(
            next_record_segment_path(&base(refused), exists).is_err(),
            "{refused} must be refused"
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn next_record_segment_path_refuses_exhaustion() {
    const LAST: u32 = RECORD_SEGMENT_LAST_ORDINAL;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("show.mp4");
    std::fs::write(&base, b"").unwrap();
    for ordinal in 2..=LAST {
        std::fs::write(directory.path().join(format!("show-{ordinal:03}.mp4")), b"").unwrap();
    }
    let error = next_record_segment_path(&base, Path::exists).unwrap_err();
    assert!(error.to_string().contains("exhausted"));
}

#[cfg(feature = "native-media")]
#[test]
fn second_recorder_slot_derives_segments_from_its_own_configured_path() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("program.mp4");
    let secondary = directory.path().join("backup.mp4");
    std::fs::write(&primary, b"").unwrap();
    std::fs::write(&secondary, b"").unwrap();
    let derive = |slot| {
        next_slot_segment_path(slot, Some(&primary), Some(&secondary), Path::exists).unwrap()
    };
    assert_eq!(derive(0).unwrap(), directory.path().join("program-002.mp4"));
    // Slot 1 derives from `--record-program-2` alone, so two recorders
    // writing into one directory fill their own gaps.
    assert_eq!(derive(1).unwrap(), directory.path().join("backup-002.mp4"));
    // An unconfigured slot derives nothing instead of borrowing the
    // other slot's base path.
    assert_eq!(
        next_slot_segment_path(1, Some(&primary), None, Path::exists).unwrap(),
        None
    );
    assert_eq!(
        next_slot_segment_path(0, None, Some(&secondary), Path::exists).unwrap(),
        None
    );
}

#[test]
fn recording_parent_falls_back_to_the_working_directory() {
    assert_eq!(recording_parent(Path::new("capture.mp4")), Path::new("."));
    assert_eq!(
        recording_parent(Path::new("out/capture.mp4")),
        Path::new("out")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn record_wav_sidecar_shares_each_segment_stem() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    for (segment, sidecar) in [
        ("program.mp4", "program.wav"),
        ("program-002.mp4", "program-002.wav"),
    ] {
        let planned = recording_session::plan(
            &directory.path().join(segment),
            directory.path(),
            &names,
            1,
            1,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            planned.wav_config().unwrap().segment_path(0, 1).unwrap(),
            directory.path().join(sidecar)
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn recorded_duration_is_exact_for_integer_and_fractional_rates() {
    let thirty = fm_types::FrameRate::new(30, 1).unwrap();
    assert_eq!(recorded_duration_ms(0, thirty), 0);
    assert_eq!(recorded_duration_ms(90, thirty), 3_000);
    let drop_frame = fm_types::FrameRate::new(30_000, 1_001).unwrap();
    assert_eq!(recorded_duration_ms(30, drop_frame), 1_001);
    assert_eq!(recorded_duration_ms(60, drop_frame), 2_002);
    let sixty = fm_types::FrameRate::new(60, 1).unwrap();
    assert_eq!(recorded_duration_ms(1, sixty), 16);
    assert_eq!(recorded_duration_ms(u64::MAX, thirty), u64::MAX);
}

#[cfg(feature = "native-media")]
#[test]
fn recording_start_timecode_derives_components_at_integer_and_fractional_rates() {
    let components = |sequence: u64, rate: fm_types::FrameRate| {
        recording_start_timecode(sequence, rate).map(fm_types::Timecode::components)
    };
    let thirty = fm_types::FrameRate::new(30, 1).unwrap();
    assert_eq!(components(0, thirty), Some((0, 0, 0, 0)));
    assert_eq!(components(29, thirty), Some((0, 0, 0, 29)));
    assert_eq!(components(30, thirty), Some((0, 0, 1, 0)));
    assert_eq!(components(30 * 3_600, thirty), Some((1, 0, 0, 0)));
    // The hour component wraps modulo 24 at exactly 24 hours and at 25
    // hours, whose minute, second, and frame components restart at zero.
    assert_eq!(components(30 * 3_600 * 24, thirty), Some((0, 0, 0, 0)));
    assert_eq!(components(30 * 3_600 * 25, thirty), Some((1, 0, 0, 0)));

    // Fractional NTSC rates derive the rounded nominal integer cadence.
    let rate_29_97 = fm_types::FrameRate::new(30_000, 1_001).unwrap();
    let rate_23_976 = fm_types::FrameRate::new(24_000, 1_001).unwrap();
    let rate_59_94 = fm_types::FrameRate::new(60_000, 1_001).unwrap();
    assert_eq!(components(29, rate_29_97), Some((0, 0, 0, 29)));
    assert_eq!(components(30, rate_29_97), Some((0, 0, 1, 0)));
    assert_eq!(components(23, rate_23_976), Some((0, 0, 0, 23)));
    assert_eq!(components(24, rate_23_976), Some((0, 0, 1, 0)));
    assert_eq!(components(59, rate_59_94), Some((0, 0, 0, 59)));
    assert_eq!(components(60, rate_59_94), Some((0, 0, 1, 0)));
    assert_eq!(components(30 * 3_600, rate_29_97), Some((1, 0, 0, 0)));
}

#[cfg(feature = "native-media")]
#[test]
fn stream_encoder_settings_carry_the_authored_video_bitrate() {
    use fm_model::{StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId};

    let target = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_100).unwrap()),
        "Bitrate".to_owned(),
        StreamProtocol::Rtmp,
        StreamEndpoint::parse("ingest.example.com/live").unwrap(),
        Some(StreamKey::parse("unit-secret-key").unwrap()),
        fm_types::OutputId::new(NonZeroU128::new(7_101).unwrap()),
    )
    .unwrap();
    let authored = target.clone().with_video_bitrate(9_000).unwrap();
    assert_ne!(authored.video_bitrate_kbps(), target.video_bitrate_kbps());
    assert_eq!(
        stream_encoder_settings(authored.video_bitrate_kbps()).video_bitrate_kbps,
        9_000
    );
    // The model default maps onto the sink default unchanged.
    let settings = stream_encoder_settings(target.video_bitrate_kbps());
    assert_eq!(
        settings.video_bitrate_kbps,
        EncoderSettings::default().video_bitrate_kbps
    );
    assert_eq!(
        settings.audio_bitrate_kbps,
        EncoderSettings::default().audio_bitrate_kbps
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_ladder_maps_authored_rungs_and_ignores_keyed_destinations() {
    use fm_model::{
        StreamEndpoint, StreamKey, StreamLadder, StreamLadderRung, StreamProtocol, StreamTarget,
        StreamTargetId,
    };

    let output = fm_types::OutputId::new(NonZeroU128::new(7_111).unwrap());
    let (protocol, endpoint) = StreamEndpoint::parse_url("hls:///srv/freemix/live").unwrap();
    let ladder = StreamLadder::new(vec![
        StreamLadderRung::new(1_000, 640, 360).unwrap(),
        StreamLadderRung::new(2_500, 1_280, 720).unwrap(),
    ])
    .unwrap();
    let laddered = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_110).unwrap()),
        "Ladder".to_owned(),
        protocol,
        endpoint,
        None,
        output,
    )
    .unwrap()
    .with_ladder(Some(ladder))
    .unwrap();
    let mapped = stream_ladder(&laddered).unwrap();
    assert_eq!(mapped.rungs().len(), 2);
    assert_eq!(mapped.top_bitrate_kbps(), 2_500);
    assert_eq!(mapped.rungs()[0].video_bitrate_kbps, 1_000);
    assert_eq!(mapped.rungs()[0].width, 640);
    assert_eq!(mapped.rungs()[0].height, 360);
    assert_eq!(mapped.rungs()[1].video_bitrate_kbps, 2_500);
    assert_eq!(mapped.rungs()[1].width, 1_280);
    assert_eq!(mapped.rungs()[1].height, 720);
    // The base bitrate a laddered target advertises is its top rung.
    assert_eq!(laddered.video_bitrate_kbps(), 2_500);

    let keyed = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_112).unwrap()),
        "Plain".to_owned(),
        StreamProtocol::Rtmp,
        StreamEndpoint::parse("ingest.example.com/live").unwrap(),
        Some(StreamKey::parse("unit-secret-key").unwrap()),
        output,
    )
    .unwrap();
    assert!(stream_ladder(&keyed).is_none());
}

/// Work that is not durable is never acknowledged.
///
/// The journal is the only thing standing between an accepted command and
/// its acknowledgement, so while it cannot record, every command must come
/// back refused and retryable with no revision consumed, the daemon must
/// stay up and keep serving, and once recording works again the next
/// command takes the very next revision. The fault stands in for a failing
/// or full show disk at the exact seam the daemon hands its journal; file
/// surgery under a running daemon cannot inject it, because the journal
/// database is opened once, at startup, and held for the daemon's run.
#[test]
#[allow(clippy::too_many_lines)]
fn unavailable_journal_refuses_commands_and_the_session_keeps_serving() {
    struct SwitchableJournal {
        available: Arc<AtomicBool>,
        recorded: AtomicU64,
    }

    impl DurableStore for SwitchableJournal {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _began_frame: u64,
            _previous: &StoredProject,
            _updated: &StoredProject,
        ) -> AppResult<()> {
            if !self.available.load(Ordering::SeqCst) {
                return Err(AppFailure("injected journal unavailability".into()).into());
            }
            self.recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
            if !self.available.load(Ordering::SeqCst) {
                return Err(AppFailure("injected journal unavailability".into()).into());
            }
            Ok(())
        }
    }

    fn next_command_result(reader: &mut MessageReader) -> CommandResult {
        loop {
            match reader.read_message_with_idle(|| Ok(false)).unwrap() {
                Some(WireMessage::CommandResult(result)) => return result,
                Some(_) => {}
                None => panic!("expected a command result"),
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let available = Arc::new(AtomicBool::new(true));
    let journal_available = Arc::clone(&available);
    let (served_tx, served_rx) = std::sync::mpsc::sync_channel(0);
    let server_thread = thread::spawn(move || {
        let journal = SwitchableJournal {
            available: journal_available,
            recorded: AtomicU64::new(0),
        };
        let mut durable = test_project();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        );
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        let (stream, _) = listener.accept().unwrap();
        handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap();
        served_tx.send(()).unwrap();
    });

    let stream = TcpStream::connect(address).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let (mut reader, _) = complete_test_handshake(&stream);

    write_message(
        &mut writer,
        &WireMessage::Command(test_command(
            "durable-cut",
            "durable-cut-key",
            CommandPayload::Cut,
        )),
    )
    .unwrap();
    assert!(matches!(
        next_command_result(&mut reader),
        CommandResult::Accepted { revision: 1, .. }
    ));

    available.store(false, Ordering::SeqCst);
    for id in ["refused-first", "refused-second"] {
        write_message(
            &mut writer,
            &WireMessage::Command(test_command(id, &format!("{id}-key"), CommandPayload::Cut)),
        )
        .unwrap();
        match next_command_result(&mut reader) {
            CommandResult::Rejected {
                id: rejected,
                code,
                current_revision,
                retryable,
                ..
            } => {
                assert_eq!(rejected, id);
                assert_eq!(code, "unavailable");
                assert_eq!(current_revision, 1, "a refused command takes no revision");
                assert!(retryable);
            }
            CommandResult::Accepted { revision, .. } => {
                panic!("{id} must not be acknowledged, yet it took revision {revision}")
            }
        }
    }

    available.store(true, Ordering::SeqCst);
    write_message(
        &mut writer,
        &WireMessage::Command(test_command(
            "after-repair",
            "after-repair-key",
            CommandPayload::Cut,
        )),
    )
    .unwrap();
    assert!(
        matches!(
            next_command_result(&mut reader),
            CommandResult::Accepted { revision: 2, .. }
        ),
        "the refused commands left no gap in the revision or the journal"
    );
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    while reader
        .read_message_with_idle(|| Ok(false))
        .unwrap()
        .is_some()
    {}
    served_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    server_thread.join().unwrap();
}

#[test]
fn client_socket_configuration_sets_read_and_write_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let _client = TcpStream::connect(address).unwrap();
    let (server_stream, _) = listener.accept().unwrap();

    configure_client_socket(&server_stream).unwrap();

    // The kernel may round a socket timeout up to its timer granularity
    // (jiffies on Linux, for example 4 ms ticks), so each configured value
    // is bounded by the request plus one coarse tick rather than compared
    // exactly.
    let read_timeout = server_stream.read_timeout().unwrap().unwrap();
    assert!(read_timeout >= CLIENT_READ_POLL_INTERVAL);
    assert!(read_timeout <= CLIENT_READ_POLL_INTERVAL + Duration::from_millis(10));
    let write_timeout = server_stream.write_timeout().unwrap().unwrap();
    assert!(write_timeout >= CLIENT_WRITE_TIMEOUT);
    assert!(write_timeout <= CLIENT_WRITE_TIMEOUT + Duration::from_millis(10));
}

#[test]
fn client_write_timeouts_are_disconnects() {
    for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
        let error = client_write_error(std::io::Error::from(kind));
        assert!(is_client_disconnect(error.as_ref()));
    }
}

fn complete_test_handshake(stream: &TcpStream) -> (MessageReader, ServerIdentity) {
    write_message(
        &mut stream.try_clone().unwrap(),
        &WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "control-timeout-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Operator,
            resume_cursor: None,
        }),
    )
    .unwrap();
    let mut reader = MessageReader::new(stream.try_clone().unwrap());
    let Some(WireMessage::HandshakeResponse(response)) =
        reader.read_message_with_idle(|| Ok(false)).unwrap()
    else {
        panic!("expected handshake response");
    };
    assert!(matches!(
        reader.read_message_with_idle(|| Ok(false)).unwrap(),
        Some(WireMessage::Snapshot(_))
    ));
    (reader, response.server)
}

#[test]
fn incomplete_pre_handshake_peer_releases_accept_loop() {
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(1);
    let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            heartbeat_timeout_ms: u64::try_from(HANDSHAKE_TIMEOUT.as_millis()).unwrap(),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        for client_index in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            if client_index == 0 {
                accepted_tx.send(Instant::now()).unwrap();
            }
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            if client_index == 0 {
                expired_tx.send(Instant::now()).unwrap();
            }
        }
    });

    let mut incomplete_peer = TcpStream::connect(address).unwrap();
    let accepted_at = accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline_lower_bound = accepted_at.checked_add(HANDSHAKE_TIMEOUT).unwrap();
    let mut wrote_partial_bytes = false;
    while Instant::now() < deadline_lower_bound {
        assert_eq!(incomplete_peer.write(b"x").unwrap(), 1);
        wrote_partial_bytes = true;
        thread::sleep(Duration::from_millis(10));
    }
    assert!(wrote_partial_bytes);
    let expired_at = expired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(expired_at >= deadline_lower_bound);
    let next_peer = TcpStream::connect(address).unwrap();
    next_peer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_peer);

    drop(next_peer);
    server_thread.join().unwrap();
}

#[test]
fn outbound_rate_limited_client_releases_accept_loop() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (terminated_tx, terminated_rx) = std::sync::mpsc::sync_channel(1);
    let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            outbound_messages: fm_server::RateLimit::new(2, 60_000),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        let (stream, _) = listener.accept().unwrap();
        let error = handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap_err();
        assert!(!is_client_disconnect(error.as_ref()));
        assert!(!is_client_protocol_error(error.as_ref()));
        assert!(is_client_session_termination(error.as_ref()));
        terminated_tx.send(()).unwrap();

        let (stream, _) = listener.accept().unwrap();
        handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap();
        completed_tx.send(()).unwrap();
    });

    let client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut reader, mut server) = complete_test_handshake(&client);
    server.project_id = "wrong-project".into();
    write_message(
        &mut client.try_clone().unwrap(),
        &WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: 1,
            sent_at_ms: 1_234,
            last_applied: None,
        }),
    )
    .unwrap();
    terminated_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut eof = [0_u8; 1];
    assert_eq!(reader.stream.read(&mut eof).unwrap(), 0);
    drop(client);

    let next_client = TcpStream::connect(address).unwrap();
    next_client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    next_client
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_client);
    drop(next_client);

    completed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    server_thread.join().unwrap();
}

#[test]
fn expired_tcp_session_is_reclaimed_for_next_client() {
    const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(600);
    const HEARTBEAT_DELAY: Duration = Duration::from_millis(200);
    // The server expires sessions on absolute wall-clock deadlines while
    // this measurement is monotonic, so a small system-clock step may
    // legitimately fire expiry early against this instant.
    const CLOCK_SKEW_TOLERANCE: Duration = Duration::from_millis(250);

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            heartbeat_timeout_ms: u64::try_from(HEARTBEAT_TIMEOUT.as_millis()).unwrap(),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        for client_index in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            if client_index == 0 {
                expired_tx.send(()).unwrap();
            }
        }
    });

    let client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut reader, server) = complete_test_handshake(&client);
    let original_deadline = Instant::now() + HEARTBEAT_TIMEOUT;
    assert!(matches!(
        expired_rx.recv_timeout(HEARTBEAT_DELAY),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    let heartbeat_sent_at = Instant::now();
    write_message(
        &mut client.try_clone().unwrap(),
        &WireMessage::Heartbeat(HeartbeatMessage {
            server: server.clone(),
            sequence: 1,
            sent_at_ms: 1_234,
            last_applied: None,
        }),
    )
    .unwrap();
    let Some(WireMessage::HeartbeatAcknowledgement(acknowledgement)) =
        reader.read_message_with_idle(|| Ok(false)).unwrap()
    else {
        panic!("expected heartbeat acknowledgement");
    };
    assert_eq!(acknowledgement.server, server);
    assert_eq!(acknowledgement.heartbeat_sequence, 1);

    assert!(matches!(
        expired_rx.recv_timeout(original_deadline.saturating_duration_since(Instant::now())),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(Instant::now() >= original_deadline);
    expired_rx.recv_timeout(HEARTBEAT_TIMEOUT).unwrap();
    assert!(
        heartbeat_sent_at.elapsed() + CLOCK_SKEW_TOLERANCE >= HEARTBEAT_TIMEOUT,
        "the session expired before its heartbeat deadline"
    );

    let next_client = TcpStream::connect(address).unwrap();
    next_client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_client);
    drop(next_client);

    server_thread.join().unwrap();
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_capture_failure_policy_formats_one_sanitized_notice() {
    let mut policy = RecorderCapturePolicy::default();
    let Some((failure, app_capture_failure)) =
        policy.fail("readback:first\tline\nend".to_owned(), true)
    else {
        panic!("first failure must emit a notice");
    };
    assert_eq!(
        recorder_failure_notice(2, failure, app_capture_failure),
        "FREEMIXD_RECORDER_FAILURE\tv=1\tslot=2\tapp_capture_failure=true\tfailure=readback:first line end",
    );
    assert!(policy.fail("enqueue:second".to_owned(), false).is_none());
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_report_annotates_the_slot_immediately_after_the_version() {
    let report = StopReport {
        outcome: StopOutcome::Clean,
        exit_status: Some(0),
        output: OutputFinalization::Synced,
        cleanup: CleanupStatus::Complete,
        wav_bytes: 0,
        telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
            state: RecorderState::Stopped,
            failure: None,
            accepted_pairs: 30,
            completed_pairs: 30,
            failed_pairs: 0,
            outstanding_pairs: 0,
            retained_bytes: 0,
            output_bytes: 1_024,
            wav_bytes: 512,
            stderr_tail: String::new(),
            stderr_truncated: false,
        },
    };
    assert_eq!(
        recorder_report(
            1,
            &report,
            1_000,
            2_048,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        ),
        "FREEMIXD_RECORDER\tv=1\tslot=1\tstate=Stopped\toutcome=Clean\taccepted_pairs=30\tcompleted_pairs=30\toutput_bytes=1024\twav_bytes=512\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\tduration_ms=1000\tfree_bytes=2048\toutput_finalization=Synced\tcleanup=Complete\tbacklogged_pairs=0\tbacklog_peak_bytes=0\tapp_capture_failure=false\tfailure=none",
    );
    // The second slot is annotated by number, never by position in a
    // combined record, so a mixed run keeps the two segments separable.
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        )
        .starts_with("FREEMIXD_RECORDER\tv=1\tslot=2\tstate=Stopped\t")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_report_renders_configured_and_unset_bitrates() {
    let report = StopReport {
        outcome: StopOutcome::Clean,
        exit_status: Some(0),
        output: OutputFinalization::Synced,
        cleanup: CleanupStatus::Complete,
        wav_bytes: 0,
        telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
            state: RecorderState::Stopped,
            failure: None,
            accepted_pairs: 1,
            completed_pairs: 1,
            failed_pairs: 0,
            outstanding_pairs: 0,
            retained_bytes: 0,
            output_bytes: 128,
            wav_bytes: 0,
            stderr_tail: String::new(),
            stderr_truncated: false,
        },
    };
    assert!(
        recorder_report(
            1,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            Some(2_000),
            Some(96)
        )
        .contains("\twav_bytes=0\tvideo_bitrate_kbps=2000\taudio_bitrate_kbps=96\tduration_ms=0\t")
    );
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        )
        .contains("\twav_bytes=0\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\t")
    );
    // Each field is independent: one slot can set video while keeping the
    // encoder's default audio.
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            Some(1_000),
            None
        )
        .contains("\tvideo_bitrate_kbps=1000\taudio_bitrate_kbps=unset\t")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn recording_output_requires_mp4_and_existing_directory_parent() {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        record_output_parent(&directory.path().join("capture.mov"))
            .unwrap_err()
            .to_string()
            .contains("final extension `.mp4`")
    );
    assert!(
        record_output_parent(&directory.path().join("missing/capture.mp4"))
            .unwrap_err()
            .to_string()
            .contains("existing canonical directory")
    );
    let not_directory = directory.path().join("parent-file");
    fs::write(&not_directory, b"parent").unwrap();
    assert!(
        record_output_parent(&not_directory.join("capture.mp4"))
            .unwrap_err()
            .to_string()
            .contains("existing canonical directory")
    );
    assert_eq!(
        record_output_parent(&directory.path().join("capture.mp4")).unwrap(),
        fs::canonicalize(directory.path()).unwrap()
    );
}

#[cfg(feature = "native-media")]
fn planned_session(
    path: &Path,
    names: &recording_session::SegmentNames,
) -> recording_session::PlannedSession {
    let parent = record_output_parent(path).unwrap();
    recording_session::plan(path, &parent, names, 1, 1_000_000, None).unwrap()
}

/// A recorder that refuses to start after its sessions were created must
/// not leave the video or WAV session behind: here an out-of-range
/// bitrate makes the real recorder refuse, and the next start may reuse
/// the same name. A stale WAV sidecar never reaches a start: it takes
/// the name.
#[cfg(feature = "native-media")]
#[test]
fn refused_recorder_start_removes_the_created_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("show.mp4");
    let wav = directory.path().join("show.wav");
    let mut config = RecordConfig::new(
        RecordFormat::new(
            64,
            36,
            fm_types::FrameRate::new(30, 1).unwrap(),
            fm_types::SampleRate::new(48_000).unwrap(),
            fm_types::ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap(),
    );
    config.video_bitrate_kbps = Some(1);
    let names = recording_session::SegmentNames::new(None);
    let parent = record_output_parent(&path).unwrap();
    let planned = recording_session::plan(&path, &parent, &names, 1, 1_000_000, Some(1)).unwrap();

    assert!(
        start_session_recorder(planned, config).is_err(),
        "an invalid bitrate refuses the recorder start"
    );
    assert!(!path.exists(), "the refused start left its segment behind");
    assert!(!wav.exists(), "the refused start left its sidecar behind");
    for manifest in ["show.fmsession", "show.wav.fmsession"] {
        assert!(
            !directory.path().join(manifest).exists(),
            "the refused start left {manifest} behind"
        );
    }
    assert!(!names.taken(&path), "the refused name is free again");

    fs::write(&wav, b"stale").unwrap();
    assert!(names.taken(&path), "a stale sidecar takes the name");
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("show-002.mp4")
    );
    assert_eq!(fs::read(wav).unwrap(), b"stale");
}

#[cfg(feature = "native-media")]
#[test]
fn recording_output_is_exclusive_and_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    let path = directory.path().join("capture.mp4");
    fs::write(&path, b"keep-me").unwrap();
    // Name derivation skips an existing file...
    assert!(names.taken(&path));
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("capture-002.mp4")
    );
    // ...and a session forced onto it never overwrites: segment creation
    // is exclusive, so the session moves on to its next segment name.
    let opened = recording_session::open(planned_session(&path, &names)).unwrap();
    drop(opened.output);
    let (status, config) = (&opened.status, opened.status.config().clone());
    assert_eq!(status.get().video.segments.len(), 0);
    let second = fm_record::RecordingSession::start(
        config.clone(),
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    );
    assert!(
        matches!(
            second,
            Err(fm_record::SessionError::ManifestExists { root: 0 })
        ),
        "a second session of the same name is refused while its manifest exists"
    );
    recording_session::discard_unused(status);
    let mut session = fm_record::RecordingSession::start(
        config,
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    )
    .unwrap();
    let opened = session.open_segment().unwrap();
    assert_eq!(opened.index, 2);
    assert_eq!(
        opened.path,
        fs::canonicalize(directory.path())
            .unwrap()
            .join("capture-0002.mp4")
    );
    assert_eq!(fs::read(&path).unwrap(), b"keep-me");

    let occupied_directory = directory.path().join("occupied.mp4");
    fs::create_dir(&occupied_directory).unwrap();
    assert!(names.taken(&occupied_directory));
}

#[cfg(all(feature = "native-media", unix))]
#[test]
fn recording_output_rejects_existing_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    fs::write(&target, b"keep-me").unwrap();
    let output = directory.path().join("capture.mp4");
    symlink(&target, &output).unwrap();
    let names = recording_session::SegmentNames::new(None);
    assert!(names.taken(&output));
    let opened = recording_session::open(planned_session(&output, &names)).unwrap();
    drop(opened.output);
    recording_session::discard_unused(&opened.status);
    let mut session = fm_record::RecordingSession::start(
        opened.status.config().clone(),
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    )
    .unwrap();
    let opened = session.open_segment().unwrap();
    assert_ne!(
        opened.path.file_name(),
        output.file_name(),
        "the symlinked name is never opened"
    );
    assert_eq!(fs::read(target).unwrap(), b"keep-me");
}

#[cfg(feature = "native-media")]
#[test]
fn a_started_session_reserves_its_name_until_its_manifest_exists() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    let path = directory.path().join("capture.mp4");
    let planned = planned_session(&path, &names);
    assert!(names.taken(&path), "planning reserves the name");
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("capture-002.mp4")
    );
    let sink = recording_session::open(planned).unwrap().output;
    assert!(names.taken(&path), "the manifest keeps the name taken");
    drop(sink);
    assert!(names.taken(&path), "a stopped session keeps its name");
}

#[test]
fn parses_fullscreen_program_display_opt_in() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "2",
        ]))
        .unwrap(),
        Command::Serve {
            native_media: true,
            fullscreen_program: true,
            display_outputs,
            display_inventory: None,
            ..
        } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(2))]
    ));
}

#[test]
fn fullscreen_display_defaults_to_zero() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
        ]))
        .unwrap(),
        Command::Serve {
            fullscreen_program: true,
            display_outputs,
            ..
        } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(0))]
    ));
}

#[test]
fn parses_two_independent_display_outputs() {
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-output",
        "display=name:Left,source=program,scaling=fit",
        "--display-output",
        "display=id:x:66,source=output:2,mode=1920x1080@60,scaling=1:1,hdr=auto",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(fullscreen_program);
    assert_eq!(display_inventory, None);
    assert_eq!(display_outputs.len(), 2);
    assert_eq!(
        display_outputs[1].source,
        fm_io_api::display::DisplayOutputSource::Output(fm_types::OutputId::new(
            NonZeroU128::new(2).unwrap()
        ))
    );
    assert_eq!(
        display_outputs[1].scaling,
        fm_types::OutputScaling::OneToOne
    );

    let Command::Serve {
        fullscreen_program,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-output",
        "display=0",
        "--diagnostic-display-inventory",
        "displays.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(!fullscreen_program, "the inventory host is headless");
    assert_eq!(display_inventory, Some(PathBuf::from("displays.tsv")));
}

#[test]
fn diagnostic_capture_inventory_requires_native_media_and_one_file() {
    let Command::Serve {
        capture_inventory, ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--diagnostic-capture-inventory",
        "captures.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert_eq!(capture_inventory, Some(PathBuf::from("captures.tsv")));
    for arguments in [
        &[
            "serve",
            "show.freemix",
            "--diagnostic-capture-inventory",
            "a",
        ][..],
        &[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-capture-inventory",
            "a",
            "--diagnostic-capture-inventory",
            "b",
        ][..],
        &[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-capture-inventory",
        ][..],
    ] {
        assert!(parse_args(strings(arguments)).is_err(), "{arguments:?}");
    }
}

#[test]
fn display_presenter_if_available_follows_the_build() {
    let Command::Serve {
        native_media,
        fullscreen_program,
        presenter_fallback,
        display_outputs,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    let surfaces = cfg!(feature = "display-surface");
    assert!(!native_media, "without a presenter it serves simulated");
    assert_eq!(fullscreen_program, surfaces, "the presenter opens windows");
    assert_eq!(presenter_fallback, surfaces);
    assert!(display_outputs.is_empty(), "the project's slots are kept");

    // An explicit presenter or diagnostic inventory keeps its own
    // behavior, and options the windowed daemon does not serve keep the
    // daemon headless.
    for options in [
        &["--native-media", "--diagnostic-display-inventory", "d.tsv"][..],
        &["--status-listen", "127.0.0.1:0"][..],
    ] {
        let mut arguments = vec!["serve", "show.freemix", "--display-presenter-if-available"];
        arguments.extend(options);
        let Command::Serve {
            fullscreen_program,
            presenter_fallback,
            ..
        } = parse_args(strings(&arguments)).unwrap()
        else {
            panic!("expected serve");
        };
        assert!(!fullscreen_program, "{options:?}");
        assert!(!presenter_fallback, "{options:?}");
    }
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
        "--display-presenter-if-available",
    ]))
    .unwrap_err();
    assert!(error.to_string().contains("duplicate option"), "{error}");

    // The web gateway rides along with the presenter: Studio's
    // supervised daemon serves phones and the program windows at once.
    let Command::Serve {
        fullscreen_program,
        presenter_fallback,
        web_listen,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
        "--web-listen",
        "0.0.0.0:0",
        "--web-tls",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert_eq!(fullscreen_program, surfaces);
    assert_eq!(presenter_fallback, surfaces);
    assert!(web_listen.is_some());
}

#[test]
fn display_presenter_opens_the_project_slots_in_windows() {
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-presenter",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(fullscreen_program, "the presenter opens real windows");
    assert!(display_outputs.is_empty(), "the project's slots are kept");
    assert_eq!(display_inventory, None);

    // The diagnostic inventory host presents the project's slots alone.
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--diagnostic-display-inventory",
        "displays.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(!fullscreen_program);
    assert_eq!(display_outputs, Vec::new());
}

#[test]
fn display_output_overrides_replace_the_project_slots_on_load() {
    let slot = |number| DisplayOutputSlot::new(number).unwrap();
    let stored = test_project();
    let mut project = stored.project().clone();
    project
        .set_display_output(slot(2), "display=index:4".parse().unwrap())
        .unwrap();
    let stored = stored.with_project(project).unwrap();
    let unchanged = apply_display_output_overrides(stored.clone(), &[]).unwrap();
    assert_eq!(unchanged.project().display_outputs().len(), 1);

    let overrides = ["display=index:0,source=preview"
        .parse::<DisplayOutputConfig>()
        .unwrap()];
    let replaced = apply_display_output_overrides(stored.clone(), &overrides).unwrap();
    assert_eq!(replaced.project().display_outputs().len(), 1);
    assert_eq!(
        replaced.project().display_output(slot(1)),
        Some(&overrides[0])
    );
    assert_eq!(replaced.project().display_output(slot(2)), None);

    let unknown = ["display=index:0,source=output:4242".parse().unwrap()];
    let error = apply_display_output_overrides(stored, &unknown).unwrap_err();
    assert!(
        error.to_string().contains("output 4242 does not exist"),
        "{error}"
    );
}

/// The journal times every append it makes durable, so a native daemon
/// knows how much of a frame interval a command's sync needs; it
/// reports nothing before its first append.
#[test]
fn the_journal_reports_how_long_its_recent_appends_took() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    store.save(&test_project()).unwrap();
    let mut durable = load_and_recover(&store, false).unwrap();
    let journal = DurableJournal::open(&store).unwrap();
    assert_eq!(journal.recent_append_peak(), None);
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let execution = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "wall",
            "wall-key",
            CommandPayload::SetDisplayOutput {
                slot: DisplayOutputSlot::new(1).unwrap(),
                config: Box::new("display=name:Wall".parse().unwrap()),
            },
        ),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            execution.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        execution.submission.output.result
    );
    assert!(journal.recent_append_peak().is_some());
}

/// `--display-output` overrides reach the manifest before any command
/// is journaled, so an edit accepted against the overridden slots
/// replays after a crash instead of conflicting with the old slots.
#[test]
fn display_output_overrides_are_durable_before_journaled_edits() {
    let slot = |number| DisplayOutputSlot::new(number).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let stored = test_project();
    let mut project = stored.project().clone();
    project
        .set_display_output(slot(1), "display=name:Wall".parse().unwrap())
        .unwrap();
    let stored = stored.with_project(project).unwrap();
    store.save(&stored).unwrap();

    let recovered = load_and_recover(&store, false).unwrap();
    let journal = DurableJournal::open(&store).unwrap();
    let overrides = ["display=index:0".parse::<DisplayOutputConfig>().unwrap()];
    let mut durable = persist_display_output_overrides(&journal, &recovered, &overrides).unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    // Slot 2 takes the display slot 1 named before the override.
    let execution = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "wall",
            "wall-key",
            CommandPayload::SetDisplayOutput {
                slot: slot(2),
                config: Box::new("display=name:Wall".parse().unwrap()),
            },
        ),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            execution.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        execution.submission.output.result
    );
    drop(journal);

    // A crash here recovers from the manifest and the journal.
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(
        recovered.project().display_outputs(),
        durable.project().display_outputs()
    );
    assert_eq!(
        recovered.project().display_output(slot(1)),
        Some(&overrides[0])
    );
}

#[test]
fn rejects_invalid_display_output_combinations() {
    for (arguments, expected) in [
        (
            vec!["--display-output", "display=0"],
            "--display-output requires --native-media",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0,scaling=zoom",
            ],
            "invalid --display-output",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--display-output",
                "display=1",
                "--display-output",
                "display=2",
            ],
            "at most 2",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--display-output",
                "display=index:0,source=output:2",
            ],
            "same display",
        ),
        (
            vec![
                "--native-media",
                "--fullscreen-program",
                "--display-output",
                "display=1",
            ],
            "--display-output replaces --fullscreen-program",
        ),
        (
            vec!["--diagnostic-display-inventory", "d.tsv"],
            "--diagnostic-display-inventory requires --native-media",
        ),
        (
            vec!["--display-presenter"],
            "--display-presenter requires --native-media",
        ),
        (
            vec![
                "--native-media",
                "--display-presenter",
                "--diagnostic-display-inventory",
                "d.tsv",
            ],
            "cannot be combined with --fullscreen-program or --display-presenter",
        ),
        (
            vec![
                "--native-media",
                "--fullscreen-program",
                "--diagnostic-display-inventory",
                "d.tsv",
            ],
            "cannot be combined with --fullscreen-program",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--status-listen",
                "127.0.0.1:0",
            ],
            "--status-listen cannot be combined with --fullscreen-program",
        ),
    ] {
        let mut full = strings(&["serve", "show.freemix"]);
        full.extend(arguments.iter().map(|value| (*value).to_owned()));
        let error = parse_args(full).unwrap_err().to_string();
        assert!(error.contains(expected), "{arguments:?}: {error}");
    }
}

#[test]
fn rejects_invalid_fullscreen_option_relationships_and_values() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--fullscreen-program"]),
            "--fullscreen-program requires --native-media",
        ),
        (
            strings(&["serve", "show.freemix", "--fullscreen-display", "0"]),
            "--fullscreen-display requires --fullscreen-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-display",
            ]),
            "missing value for --fullscreen-display",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "-1",
        ]))
        .unwrap_err()
        .to_string()
        .contains("invalid fullscreen display index")
    );
}

#[test]
fn rejects_duplicate_fullscreen_options() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-program",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "0",
            "--fullscreen-display",
            "1",
        ]),
    ] {
        assert!(parse_args(arguments).is_err());
    }
}

/// A Preview composite is rendered for a presenting display output that
/// shows Preview or for the Preview tap's capture, and for nothing else:
/// not for a tap frame a late loop paced out, however due display
/// outputs (none presenting) are.
#[cfg(feature = "native-media")]
#[test]
fn a_paced_out_preview_tap_frame_renders_no_preview_composite() {
    // No display shows Preview: only the tap's capture renders one.
    assert!(!super::preview_composite_due(false, true, false));
    assert!(super::preview_composite_due(false, true, true));
    assert!(super::preview_composite_due(false, false, true));
    // A display showing Preview renders it when it presents.
    assert!(super::preview_composite_due(true, true, false));
    assert!(!super::preview_composite_due(true, false, false));
}

/// A command dispatches while more of the frame interval is left than
/// recent journal appends took (half an interval before any was timed);
/// with less it waits for that frame, and once the frame was realized it
/// dispatches however little of the next interval is left, so it waits
/// at most one frame.
#[cfg(feature = "native-media")]
#[test]
fn a_command_waits_at_most_one_frame_for_room_before_its_deadline() {
    let open = |left: u64, peak: Option<u64>, frame: u64, deferred: &mut Option<FrameNumber>| {
        super::command_window_open(
            Duration::from_millis(left),
            Duration::from_millis(40),
            peak.map(Duration::from_millis),
            FrameNumber::new(frame),
            deferred,
        )
    };
    let mut deferred = None;
    // Untimed journal: half an interval is needed.
    assert!(open(20, None, 7, &mut deferred));
    assert_eq!(deferred, None);
    // Too little left: held back for frame 7, and again on a later pass
    // before frame 7 was realized.
    for left in [19, 3, 0] {
        assert!(!open(left, None, 7, &mut deferred));
        assert_eq!(deferred, Some(FrameNumber::new(7)));
    }
    // Frame 7 was realized: it goes now, even with little of frame 8's
    // interval left (a slow realization), and the hold is cleared.
    assert!(open(5, None, 8, &mut deferred));
    assert_eq!(deferred, None);
    // A quick journal needs only its peak and the staging margin.
    assert!(open(4, Some(1), 9, &mut deferred));
    assert!(!open(2, Some(1), 9, &mut deferred));
    assert_eq!(deferred, Some(FrameNumber::new(9)));
    // Room enough clears a stale hold too.
    assert!(open(30, Some(1), 9, &mut deferred));
    assert_eq!(deferred, None);
    // An overdue frame is realized first, whatever the journal.
    assert!(!open(0, Some(0), 10, &mut deferred));
    // A journal slower than a frame interval: a command always takes a
    // frame boundary, right after the frame it waited for.
    deferred = None;
    assert!(!open(39, Some(80), 11, &mut deferred));
    assert!(open(1, Some(80), 12, &mut deferred));
}

#[cfg(not(feature = "native-media"))]
#[test]
fn native_media_opt_in_fails_when_support_is_not_compiled() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let error = NativeDaemon::start(&store, &test_project(), None)
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "native-media support was not compiled in"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_mode_rejects_unimplemented_simulated_sine_audio() {
    let error = validate_native_audio_modes(&test_project()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires unsupported simulated sine audio")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_builds_retained_generators_without_assets() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let resolution = resolve_native_sources(&store, &test_project(), None).unwrap();
    let sources = resolution.sources;
    assert_eq!(sources.len(), 2);
    for (index, source) in sources.iter().enumerate() {
        let NativeResolvedSource::RetainedFrame { input, frame } = source else {
            panic!("simulated source must be retained")
        };
        assert_eq!(*input, test_input_id(u128::try_from(index + 1).unwrap()));
        assert_eq!(frame.payload().dimensions().width(), 1_280);
        assert_eq!(frame.payload().dimensions().height(), 720);
        assert_eq!(frame.timing().sequence().get(), 0);
        assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
        assert!(frame.metadata().is_some());
    }
    let NativeResolvedSource::RetainedFrame { frame, .. } = &sources[0] else {
        unreachable!()
    };
    assert_eq!(
        &frame.payload().plane(0).unwrap().bytes()[..4],
        &[7, 11, 13, 255]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_keeps_scene_inputs_out_of_physical_sources() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let resolution = resolve_native_sources(&store, &scene_test_project(), None).unwrap();

    assert_eq!(resolution.sources.len(), 1);
    assert_eq!(resolution.sources[0].input(), test_input_id(2));
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn macos_camera_resolution_is_exact_non_prompting_and_reaps_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");

    let directory = tempfile::tempdir().unwrap();
    let helper = fake_camera_helper(directory.path(), 0);
    let capture_marker = PathBuf::from(format!("{}.capture", helper.display()));
    let pid_file = PathBuf::from(format!("{}.pid", helper.display()));
    let permission_marker = PathBuf::from(format!("{}.permission", helper.display()));
    let (sources, mut cameras) =
        resolve_macos_camera_sources(&camera_test_project(&stable_key), Some(helper.as_path()))
            .unwrap();
    assert_eq!(sources.len(), 1);
    let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
        panic!("camera source must enter the live frame lane")
    };
    assert_eq!(*input, test_input_id(1));
    assert_eq!(frame.timing().sequence().get(), 7);
    assert_eq!(
        frame.timing().presentation_timestamp().as_nanos(),
        1_000_000_000
    );
    assert_eq!(frame.payload().plane(0).unwrap().bytes(), &[3, 5, 7, 255]);
    assert_eq!(
        frame.metadata().unwrap().color().transfer,
        fm_types::TransferFunction::Bt709
    );
    let pending = cameras.source_telemetry()[0];
    assert_eq!(pending.frames_received, 1);
    assert_eq!(pending.frames_ingested, 0);
    assert_eq!(pending.preflight_depth, 1);
    assert_eq!(pending.preflight_discarded, 0);
    assert_eq!(pending.frames_received, pending.preflight_depth);
    drop(sources);
    cameras.discard_preflight_frames();
    let discarded = cameras.source_telemetry()[0];
    assert_eq!(discarded.preflight_depth, 0);
    assert_eq!(discarded.preflight_discarded, 1);
    assert_eq!(discarded.frames_received, discarded.preflight_discarded);
    assert_eq!(
        fs::read_to_string(&capture_marker).unwrap(),
        "capture\nfake-camera\n1\n1\n30000\n1001\n"
    );
    assert!(!permission_marker.exists());
    let pid = fs::read_to_string(&pid_file).unwrap();
    drop(cameras);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        "camera helper {pid} survived source cleanup"
    );

    let unknown_directory = tempfile::tempdir().unwrap();
    let unknown_helper = fake_camera_helper(unknown_directory.path(), 0);
    let unknown_capture = PathBuf::from(format!("{}.capture", unknown_helper.display()));
    let error = resolve_macos_camera_sources(
        &camera_test_project("macos.avfoundation.camera.v1.999"),
        Some(unknown_helper.as_path()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("unknown camera stable key"));
    assert!(!unknown_capture.exists(), "unknown key invoked capture");

    let prompt_directory = tempfile::tempdir().unwrap();
    let prompt_helper = fake_camera_helper(prompt_directory.path(), 1);
    let prompt_capture = PathBuf::from(format!("{}.capture", prompt_helper.display()));
    let prompt_request = PathBuf::from(format!("{}.permission", prompt_helper.display()));
    let error = resolve_macos_camera_sources(
        &camera_test_project(&stable_key),
        Some(prompt_helper.as_path()),
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("camera permission is not granted")
    );
    assert!(
        !prompt_capture.exists(),
        "permission preflight invoked capture"
    );
    assert!(
        !prompt_request.exists(),
        "daemon requested camera permission"
    );
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn many_camera_startup_failure_uses_aggregate_cleanup_and_reaps_all_helpers() {
    const CAMERA_COUNT: usize = 16;

    let keys = (0..CAMERA_COUNT)
        .map(|index| {
            let source_id =
                deterministic_camera_id(CameraIdKind::Source, &format!("fake-camera-{index}"));
            format!("macos.avfoundation.camera.v1.{source_id}")
        })
        .collect::<Vec<_>>();
    let project = many_camera_test_project(&keys);
    let directory = tempfile::tempdir().unwrap();
    let helper = many_camera_startup_failure_helper(directory.path(), CAMERA_COUNT);
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));

    let started = Instant::now();
    let error = resolve_macos_camera_sources(&project, Some(helper.as_path()))
        .err()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(error.to_string().contains("failed to start"));
    assert_eq!(
        fs::read_to_string(&pid_log).unwrap().lines().count(),
        CAMERA_COUNT
    );
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_poll_into_ingests_recovered_mapped_discontinuity_while_ticks_continue() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(3), 20);
    let capture_log = PathBuf::from(format!("{}.capture", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let runtime = NativeMediaRuntime::new_blocking([platform_native_backend()]).unwrap();
    let (sources, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(3, Duration::from_millis(100)),
    )
    .unwrap();
    let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
        panic!("camera source must be live")
    };
    assert_eq!(*input, test_input_id(1));
    assert_eq!(frame.timing().sequence().get(), 7);
    let mut playback = runtime
        .preflight_resolved_source_playback_mixed_blocking(
            None,
            sources,
            native_clock_domain(),
            StreamSelector::Best,
            NativeSourceLimits::default(),
        )
        .unwrap();
    cameras.mark_preflight_frames_ingested();
    let committed = cameras.source_telemetry()[0];
    assert_eq!(committed.frames_ingested, 1);
    assert_eq!(committed.preflight_depth, 0);
    assert_eq!(committed.preflight_discarded, 0);
    assert_eq!(committed.frames_received, committed.accounted_frames());

    let mut control = test_control(&project);
    let server = test_server(&control);
    let mut rendered = 0_u64;
    let mut checkpointed = 0_u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while cameras.inputs[0].ingested_frames == 1 && Instant::now() < deadline {
        cameras.poll_into(&runtime, &mut playback).unwrap();
        control.tick(&server).unwrap();
        rendered = rendered.saturating_add(1);
        let snapshot = control.idle_engine_snapshot().unwrap();
        stored_project_checkpoint(&project, &snapshot).unwrap();
        checkpointed = checkpointed.saturating_add(1);
        thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(cameras.inputs[0].ingested_frames, 2);
    assert_eq!(cameras.inputs[0].last_ingested_sequence, Some(8));
    assert!(cameras.inputs[0].last_ingested_discontinuity);
    assert!(rendered > 10, "render loop stalled during camera recovery");
    assert_eq!(checkpointed, rendered);
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 2);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert_eq!(
        telemetry[0].frames_received,
        telemetry[0].accounted_frames()
    );
    assert_eq!(
        telemetry[0].recovery_outcome,
        CameraRecoveryOutcome::Recovered
    );
    assert!(!telemetry[0].diagnostic().contains("fake-camera"));

    let captures = fs::read_to_string(capture_log).unwrap();
    let expected = ["capture", "fake-camera", "1", "1", "30000", "1001"];
    let captures = captures.lines().collect::<Vec<_>>();
    assert_eq!(captures.len(), 18);
    for invocation in captures.chunks_exact(expected.len()) {
        assert_eq!(invocation, expected);
    }

    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_recovery_rearms_after_exhaustion_and_eventually_recovers() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(4), 20);
    let capture_count = PathBuf::from(format!("{}.count", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let mut control = test_control(&project);
    let server = test_server(&control);
    let mut rendered = 0_u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cameras.poll_with(|_, _| Ok(())).unwrap();
        control.tick(&server).unwrap();
        rendered = rendered.saturating_add(1);
        let telemetry = cameras.source_telemetry();
        if telemetry[0].recovery_successes == 1 || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 3);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 1);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert!(
        rendered > 10,
        "render loop stopped on recoverable camera loss"
    );
    assert_eq!(fs::read_to_string(capture_count).unwrap(), "4");
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn generic_camera_helper_exit_is_recoverable_but_malformed_contract_is_fatal() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(2), 21);
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while cameras.inputs[0].ingested_frames == 0 && Instant::now() < deadline {
        let poll_started = Instant::now();
        cameras.poll_with(|_, _| Ok(())).unwrap();
        assert!(
            poll_started.elapsed() < Duration::from_millis(100),
            "render-thread camera poll performed blocking cleanup"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 1);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert!(!recoverable_camera_error(&IoError::MalformedTimestamp(
        fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
    )));
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn malformed_camera_frame_bytes_are_fatal_and_never_restart_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = malformed_runtime_camera_helper(directory.path());
    let capture_count = PathBuf::from(format!("{}.count", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let error = loop {
        match cameras.poll_with(|_, _| Ok(())) {
            Ok(()) => {}
            Err(error) => break error,
        }
        assert!(
            Instant::now() < deadline,
            "malformed frame bytes were swallowed"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(
        error.downcast_ref::<IoError>(),
        Some(IoError::AdapterFailure {
            remediation: None,
            ..
        })
    ));
    assert_eq!(fs::read_to_string(capture_count).unwrap(), "1");
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 0);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 1);
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn fatal_camera_contract_error_propagates_without_counting_as_exhaustion() {
    let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
        frame: CameraFrameSlot::default(),
        snapshot: CameraWorkerSnapshot {
            telemetry: CameraTelemetry::default(),
            lifecycle: LifecycleState::Lost,
            health: EndpointHealthState::Failed,
            recovery_attempts: 0,
            recovery_successes: 0,
            recovery_exhausted: 0,
            recovery_worker_failures: 1,
            recovery_outcome: CameraRecoveryOutcome::WorkerFailed,
        },
    }));
    let handle = thread::spawn(|| CameraWorkerResult {
        failure: Some(IoError::MalformedTimestamp(
            fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
        )),
        cleanup_failure: None,
    });
    let mut cameras = NativeCameraInputs {
        inputs: vec![NativeCameraInput {
            input: test_input_id(1),
            source: None,
            worker: Some(NativeCameraWorker {
                handle: Some(handle),
                cancel: Arc::new(AtomicBool::new(false)),
            }),
            supervisor,
            recovery_policy: test_camera_recovery_policy(1, Duration::from_millis(10)),
            ingested_frames: 0,
            ingest_failed: 0,
            preflight_depth: 0,
            preflight_discarded: 0,
            last_ingested_sequence: None,
            last_ingested_discontinuity: false,
        }],
        telemetry_emitted: false,
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let error = loop {
        if let Err(error) = cameras.poll_with(|_, _| Ok(())) {
            break error;
        }
        assert!(
            Instant::now() < deadline,
            "fatal camera error was swallowed"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(error.to_string().contains("malformed timestamp"));
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 1);
    assert_eq!(
        telemetry[0].recovery_outcome,
        CameraRecoveryOutcome::WorkerFailed
    );
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn one_recovering_camera_does_not_interrupt_second_camera() {
    let first_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-a");
    let second_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-b");
    let first_key = format!("macos.avfoundation.camera.v1.{first_id}");
    let second_key = format!("macos.avfoundation.camera.v1.{second_id}");
    let project = two_camera_test_project(&first_key, &second_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = two_camera_helper(directory.path());
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut recovered_input = None;
    while recovered_input.is_none() && Instant::now() < deadline {
        cameras
            .poll_with(|input, frame| {
                if frame
                    .timing()
                    .flags()
                    .contains(fm_frame::MediaFlags::DISCONTINUITY)
                {
                    recovered_input = Some(input);
                }
                Ok(())
            })
            .unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(recovered_input, Some(test_input_id(1)));
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert_eq!(telemetry[1].lifecycle, LifecycleState::Running);
    assert_eq!(telemetry[1].recovery_attempts, 0);
    assert_eq!(telemetry[1].recovery_worker_failures, 0);
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_shutdown_during_recovery_is_aggregate_bounded_and_reaps_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = hanging_recovery_camera_helper(directory.path());
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(3, Duration::from_millis(20)),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while cameras.source_telemetry()[0].recovery_attempts == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(cameras.source_telemetry()[0].recovery_attempts, 1);

    let shutdown_started = Instant::now();
    cameras.shutdown().unwrap();
    assert!(shutdown_started.elapsed() < Duration::from_secs(4));
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_keeps_asset_resolution_errors_path_free() {
    let store = ProjectStore::new("secret-native-project-marker.freemix").unwrap();
    let error = resolve_native_sources(
        &store,
        &media_test_project("asset://secret-uri-marker/../clip.mkv"),
        None,
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(message.contains("invalid project asset URI"));
    assert!(!message.contains("secret-native-project-marker"));
    assert!(!message.contains("secret-uri-marker"));
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_routes_png_to_one_timed_retained_frame() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("still-project.freemix");
    let assets = project_path.join("assets");
    fs::create_dir_all(&assets).unwrap();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&[1, 2, 3, 4], 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(assets.join("still.data"), png).unwrap();

    let sources = resolve_native_sources(
        &ProjectStore::new(&project_path).unwrap(),
        &media_test_project("asset://still.data"),
        None,
    )
    .unwrap()
    .sources;
    assert_eq!(sources.len(), 2);
    for source in sources {
        let NativeResolvedSource::RetainedFrame { frame, .. } = source else {
            panic!("PNG signature must route to retained still decode")
        };
        // A still media file is fitted into the output raster.
        assert_eq!(
            frame.payload().dimensions(),
            VideoDimensions::new(1_280, 720).unwrap()
        );
        let plane = frame.payload().plane(0).unwrap();
        let centre = 360 * plane.stride() + 640 * 4;
        assert_eq!(&plane.bytes()[centre..centre + 4], &[1, 2, 3, 4]);
        assert_eq!(&plane.bytes()[..4], &[0, 0, 0, 0]);
        assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
        assert_eq!(frame.timing().duration().as_nanos(), 1);
        assert_eq!(frame.timing().sequence().get(), 0);
        assert_eq!(frame.timing().clock_domain(), native_clock_domain());
    }
}

#[cfg(feature = "native-media")]
#[test]
fn recognized_corrupt_png_does_not_fall_back_to_ffmpeg() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("corrupt-still.freemix");
    let assets = project_path.join("assets");
    fs::create_dir_all(&assets).unwrap();
    fs::write(assets.join("bad.bin"), b"\x89PNG\r\n\x1a\ncorrupt").unwrap();

    let error = resolve_native_sources(
        &ProjectStore::new(&project_path).unwrap(),
        &media_test_project("asset://bad.bin"),
        None,
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(message.contains("native still decode failed"));
    assert!(!message.contains("corrupt-still"));
    assert!(!message.contains("bad.bin"));
}

#[cfg(feature = "native-media")]
#[test]
fn host_pacer_offsets_use_exact_rational_deadlines() {
    let rate = FrameRate::new(30_000, 1_001).unwrap();
    let mut pacer = FramePacer::new(rate, 0);
    assert_eq!(host_deadline_offset(&pacer).unwrap(), Duration::ZERO);
    pacer.advance().unwrap();
    assert_eq!(host_deadline_offset(&pacer).unwrap().as_nanos(), 33_366_666);
    for _ in 1..30_000 {
        pacer.advance().unwrap();
    }
    assert_eq!(host_deadline_offset(&pacer).unwrap().as_secs(), 1_001);
}

/// A command that could not be made durable is refused, not acknowledged
/// and not half-applied: the engine keeps its revision, publishes nothing,
/// and installs no receipt, so the same command may simply be sent again.
#[test]
fn a_command_that_cannot_be_made_durable_is_refused() {
    let mut durable = test_project();
    let initial_engine = restore_engine(&durable).unwrap().snapshot().unwrap();
    let mut control = test_control(&durable);
    let subscription = control.subscribe().unwrap();
    let before_diagnostics = control.diagnostics();
    let before_snapshot = control.snapshot().clone();
    let server = test_server(&control);
    let command = test_command("failed-save", "failed-save-key", CommandPayload::Cut);

    let execution = execute_durable_command(
        &mut control,
        &FailingSaver,
        &mut durable,
        &operator(),
        &server,
        &command,
        0,
    )
    .expect("a durability failure is refused, not propagated");

    assert!(matches!(
        &execution.submission.output.result,
        CommandResult::Rejected {
            code,
            retryable: true,
            current_revision: 0,
            ..
        } if code == "unavailable"
    ));
    assert_eq!(execution.submission.output.events, []);
    assert!(execution.submission.accepted.is_none());
    assert!(!execution.submission.replayed);
    assert_eq!(execution.runtime_events, []);
    assert_eq!(durable, test_project());
    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(live_engine_snapshot(&mut control), initial_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    let prepared = control
        .prepare_submit(&operator(), command, 0)
        .unwrap()
        .prepared()
        .expect("failed save must not install a receipt");
    prepared.abort();
}

/// Regression: a command whose staged result the project schema refuses
/// (here a blank idempotency key, which a receipt may not carry) stopped
/// the whole daemon. It is refused, nothing is recorded or committed, and
/// the next command runs normally.
#[test]
fn a_command_whose_result_cannot_be_persisted_is_refused_not_fatal() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let before_durable = durable.clone();
    let before_engine = live_engine_snapshot(&mut control);
    let subscription = control.subscribe().unwrap();
    let saver = CountingSaver::default();

    let refused = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command("blank-key", "   ", CommandPayload::Cut),
        0,
    )
    .expect("a command-caused staging failure refuses the command");

    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected {
            id,
            code,
            retryable: false,
            current_revision: 0,
            ..
        } if id == "blank-key" && code == "invalid_command"
    ));
    assert!(refused.submission.accepted.is_none());
    assert_eq!(refused.submission.output.events, []);
    assert_eq!(refused.runtime_events, []);
    assert_eq!(saver.0.get(), 0, "nothing was journaled");
    assert_eq!(durable, before_durable);
    assert_eq!(live_engine_snapshot(&mut control), before_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

    let accepted = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command("after", "after-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    assert!(matches!(
        accepted.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert_eq!(saver.0.get(), 1);
}

/// Regression: persisting a Stinger slot whose realization had not caught
/// up stopped the daemon. It is a retryable conflict.
#[test]
fn a_divergent_stinger_slot_refuses_the_command_as_a_retryable_conflict() {
    let durable = test_project();
    let inputs = vec![test_input_id(1), test_input_id(2)];
    let mut desired =
        SwitcherState::new(inputs.clone(), test_input_id(1), test_input_id(2)).unwrap();
    let realized = SwitcherState::new(inputs, test_input_id(1), test_input_id(2)).unwrap();
    let slot = StingerSlotId::new(2).unwrap();
    desired
        .configure_stinger(
            slot,
            StingerDescriptor::new(
                test_input_id(2),
                false,
                5,
                StingerAudioPolicy::Muted,
                MissingMediaFallback::Cut,
            ),
        )
        .unwrap();
    let mut project = durable.project().clone();

    let error = sync_project_stingers(&mut project, &desired, &realized).unwrap_err();
    let refusal = staging_refusal(error).expect("divergence is not fatal");
    assert_eq!(refusal.code, RejectionCode::Conflict);
    assert!(refusal.retryable);
    assert!(refusal.message.contains("Stinger slot 2"));

    // Once realization matches, the slot persists.
    let mut settled = realized.clone();
    settled
        .configure_stinger(slot, *desired.stinger(slot).descriptor().unwrap())
        .unwrap();
    sync_project_stingers(&mut project, &desired, &settled).unwrap();
    assert_eq!(project.stingers().len(), 1);
}

/// The classification boundary: staging failures caused by the command or
/// by recoverable runtime state refuse it; failures that mean the daemon's
/// own state can no longer be trusted stay fatal.
#[test]
fn staging_failures_are_classified_into_refusals_and_fatal_errors() {
    let refused = |error: Box<dyn Error>| staging_refusal(error).unwrap();
    let fatal = |error: Box<dyn Error>| staging_refusal(error).unwrap_err().to_string();

    let in_flight = refused(
        fm_control::ControlError::Engine(fm_engine::EngineError::Snapshot(
            fm_engine::SnapshotError::WorkInFlight,
        ))
        .into(),
    );
    assert_eq!(
        (in_flight.code, in_flight.retryable),
        (RejectionCode::Conflict, true)
    );
    let unpersistable = refused(fm_persistence::ProjectValidationError::EmptyIdempotencyKey.into());
    assert_eq!(
        (unpersistable.code, unpersistable.retryable),
        (RejectionCode::InvalidCommand, false)
    );

    // An engine that cannot advance or holds stale authority.
    assert_ne!(
        fatal(fm_control::ControlError::Engine(fm_engine::EngineError::StalePreparation).into()),
        ""
    );
    assert_ne!(
        fatal(fm_control::ControlError::RuntimeSequenceExhausted.into()),
        ""
    );
    // The stored project disagreeing with the engine.
    assert_eq!(
        fatal(AppFailure("project is missing audio strip for input 9".into()).into()),
        "project is missing audio strip for input 9"
    );
}

/// An admitted command whose execution fails must still release its
/// in-flight slot; a leaked slot would eventually refuse every command.
#[test]
fn failed_command_execution_releases_its_inflight_slot() {
    let mut durable = test_project();
    let project_id = durable.project().id();
    let control = Rc::new(RefCell::new(test_control(&durable)));
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        std::net::IpAddr::from([127, 0, 0, 1]),
        CAPABILITIES_DIGEST,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
    server.mark_ready().unwrap();
    let principal = development_principal().unwrap();
    let (hello, _) = current_handshake(
        &HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "inflight-leak-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Operator,
            resume_cursor: None,
        },
        &control,
        project_id,
    );
    let mut session = server
        .handshake(&hello, &principal, now_millis().unwrap())
        .unwrap()
        .session;
    // A foreign authority makes the post-commit tick of a timed transition
    // fail, so execution returns an error after admission.
    let mut foreign = control_server_identity(&control.borrow(), project_id);
    foreign.log_id = "foreign-log".into();

    let failed = execute_session_command(
        &mut session,
        &control,
        &CountingSaver::default(),
        &mut durable,
        &principal,
        &foreign,
        &test_command(
            "failing-fade",
            "failing-fade-key",
            CommandPayload::Fade { duration_frames: 2 },
        ),
        None,
        None,
    );

    assert!(failed.is_err());
    assert_eq!(session.accounting().inbound_commands_admitted_total, 1);
    assert_eq!(session.accounting().inbound_commands_inflight, 0);
}

/// RC-004: a live client's GO binds Preview and Program on the
/// authority, whatever `bound_preview` and `bound_program` it sends;
/// only its selection is its own. (A journal replay keeps them: it
/// never passes through a session.)
#[test]
fn a_live_fire_binds_preview_and_program_on_the_authority() {
    use fm_protocol::{CueListSpec, CueOperation, WireCue, WireCueAction, WireInputId};
    let mut durable = test_project();
    let project_id = durable.project().id();
    let control = Rc::new(RefCell::new(test_control(&durable)));
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        std::net::IpAddr::from([127, 0, 0, 1]),
        CAPABILITIES_DIGEST,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
    server.mark_ready().unwrap();
    let principal = development_principal().unwrap();
    let (hello, _) = current_handshake(
        &HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "live-fire-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Admin,
            resume_cursor: None,
        },
        &control,
        project_id,
    );
    let mut session = server
        .handshake(&hello, &principal, now_millis().unwrap())
        .unwrap()
        .session;
    let server_identity = control_server_identity(&control.borrow(), project_id);
    let wire = |value| WireInputId::from_domain(test_input_id(value));
    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Key Preview".into(),
            auto_follow: false,
            actions: vec![WireCueAction::OverlayOn {
                channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
                input: fm_types::InputTarget::Preview,
            }],
        }],
    };
    let commands = [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        // The client claims Program (1) is on Preview and Preview (2)
        // on Program.
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings {
                        preview: Some(wire(1)),
                        program: Some(wire(2)),
                        selected: Some(wire(1)),
                    },
                },
            },
        ),
    ];
    for command in &commands {
        let delivery = execute_session_command(
            &mut session,
            &control,
            &CountingSaver::default(),
            &mut durable,
            &principal,
            &server_identity,
            command,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{}: {:?}",
            command.id,
            delivery.result
        );
    }
    let list = durable.project().cue_lists()[0].clone();
    assert_eq!(
        list.state.targets,
        Some(fm_model::TargetBindings {
            preview: Some(test_input_id(2)),
            program: Some(test_input_id(1)),
            selected: Some(test_input_id(1)),
        })
    );
}

#[test]
fn a_refused_manual_position_preserves_restart_state_and_stays_retryable() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "manual-start",
            "manual-start-key",
            CommandPayload::StartManualTransition {
                kind: fm_protocol::ManualTransitionKind::Wipe,
            },
        ),
        0,
    )
    .unwrap();
    let before_durable = durable.clone();
    let before_engine = control.idle_engine_snapshot().unwrap();
    let position_command = test_command(
        "manual-position",
        "manual-position-key",
        CommandPayload::SetManualTransitionPosition {
            position: fm_protocol::ManualTransitionPosition::new(6_250).unwrap(),
        },
    );

    let refused = execute_durable_command(
        &mut control,
        &FailingSaver,
        &mut durable,
        &operator(),
        &server,
        &position_command,
        0,
    )
    .expect("a durability failure is refused, not propagated");

    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected { code, retryable: true, .. } if code == "unavailable"
    ));
    assert_eq!(durable, before_durable);
    assert_eq!(control.idle_engine_snapshot().unwrap(), before_engine);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &position_command,
        0,
    )
    .unwrap();
    let manual = durable.runtime_manual_transitions();
    assert_eq!(manual.desired.unwrap().position_basis_points, 6_250);
    assert_eq!(manual.realized.unwrap().interval_start_basis_points, 6_250);
    assert_eq!(durable.idempotency_receipts().len(), 2);
}

#[test]
fn save_precedes_commit_ticks_and_returned_result() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let subscription = control.subscribe().unwrap();
    let saver = ObservingSaver {
        subscription: &subscription,
        saved: RefCell::new(None),
    };
    let server = test_server(&control);

    let execution = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "ordered-fade",
            "ordered-fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        0,
    )
    .unwrap();

    assert!(matches!(
        execution.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert_eq!(saver.saved.borrow().as_ref(), Some(&durable));
    assert_eq!(durable.position().frames_rendered, 4);
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(_)
    ));
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Runtime(_)
    ));
}

/// A cue list whose cue runs a media input's transport restores: the
/// media inputs are known to the show before its cue lists are checked
/// (RC-017).
#[test]
fn a_cue_list_with_a_media_action_restores() {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Cue Media Restore",
        baseline.project().settings().clone(),
    );
    project.add_input(Input {
        id: test_input_id(1),
        name: "Camera".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mov".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    project.set_cue_lists(vec![fm_model::CueList {
        id: 1,
        name: "Show".into(),
        cues: vec![fm_model::Cue {
            id: 1,
            name: "Roll".into(),
            actions: vec![fm_model::CueAction::Media {
                input: fm_types::InputTarget::Fixed(test_input_id(2)),
                operation: fm_model::CueMediaOperation::Play,
            }],
            auto_follow: false,
        }],
        state: fm_model::CueRunState {
            standby: Some(1),
            ..fm_model::CueRunState::default()
        },
    }]);
    let durable = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let engine = restore_engine(&durable).unwrap();
    assert_eq!(engine.show().cue_lists().len(), 1);
}

#[test]
fn projected_project_equals_committed_settled_engine() {
    let mut durable = test_project();
    let original_project = durable.project().clone();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-fade",
            "settled-fade-key",
            CommandPayload::Fade { duration_frames: 7 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 7);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(durable.position().clock_time_nanos, 240_000_000);
    let mut expected_project = original_project;
    expected_project.set_main_mix(durable.project().main_mix().unwrap());
    assert_eq!(durable.project(), &expected_project);
}

#[test]
fn queued_fade_take_next_settles_before_checkpoint() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let saver = CountingSaver::default();
    let server = test_server(&control);
    let channel = fm_protocol::WireOverlayChannelId::new(1).unwrap();
    let mut execute = |id, key, payload| {
        execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(id, key, payload),
            0,
        )
    };

    execute(
        "overlay-transition",
        "overlay-transition-key",
        CommandPayload::ConfigureOverlayTransition {
            channel,
            transition: fm_protocol::OverlayTransitionKind::Fade,
            duration_frames: 4,
        },
    )
    .unwrap();
    execute(
        "overlay-queue",
        "overlay-queue-key",
        CommandPayload::QueueOverlay {
            channel,
            source: fm_protocol::WireInputId::from_domain(test_input_id(2)),
        },
    )
    .unwrap();
    let next = execute(
        "overlay-next",
        "overlay-next-key",
        CommandPayload::TakeNextOverlay { channel },
    )
    .unwrap();
    // Dropping the closure ends its captured mutable borrows before the assertions below.
    #[allow(clippy::drop_non_drop)]
    drop(execute);

    assert!(matches!(
        next.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    assert_eq!(durable.position().frames_rendered, 6);
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    let live = live_engine_snapshot(&mut control);
    assert_eq!(live, restored);
    let channel = OverlayChannelId::new(channel.number()).unwrap();
    let desired = live.show().desired_switcher().overlay(channel);
    let realized = live.realized_switcher().overlay(channel);
    assert_eq!(desired, realized);
    assert_eq!(desired.source(), Some(test_input_id(2)));
    assert!(desired.is_active());
    assert_eq!(desired.opacity(), u8::MAX);
    assert_eq!(desired.queued_sources(), []);
}

#[test]
fn restore_engine_honors_durable_stinger_preload_intent() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(8).unwrap(),
        test_input_id(2),
        false,
        11,
        ModelStingerAudioPolicy::MixWithProgram,
        StingerMissingMediaFallback::KeepProgram,
    ));
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();

    let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
    let deferred_slot = StingerSlotId::new(8).unwrap();
    let deferred = StingerDescriptor::new(
        test_input_id(2),
        false,
        11,
        StingerAudioPolicy::MixWithProgram,
        MissingMediaFallback::KeepProgram,
    );
    let preloaded_slot = StingerSlotId::new(1).unwrap();
    let preloaded = StingerDescriptor::new(
        test_input_id(2),
        true,
        1,
        StingerAudioPolicy::Muted,
        MissingMediaFallback::Cut,
    );
    for state in [
        snapshot.show().desired_switcher(),
        snapshot.realized_switcher(),
    ] {
        assert_eq!(state.stinger(deferred_slot).descriptor(), Some(&deferred));
        assert_eq!(
            state.stinger(deferred_slot).preload_state(),
            fm_switcher::StingerPreloadState::NotRequested
        );
        assert_eq!(state.stinger(preloaded_slot).descriptor(), Some(&preloaded));
        assert_eq!(
            state.stinger(preloaded_slot).preload_state(),
            fm_switcher::StingerPreloadState::Ready
        );
    }
}

#[test]
fn missing_cut_stinger_uses_one_checkpoint_frame() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        false,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let mut durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    let execution = execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "missing-cut-stinger",
            "missing-cut-stinger-key",
            CommandPayload::Stinger {
                slot: fm_protocol::WireStingerSlotId::new(1).unwrap(),
                duration_frames: 7,
            },
        ),
        0,
    )
    .unwrap();

    assert!(matches!(
        execution.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    assert_eq!(durable.position().frames_rendered, 1);
    assert_eq!(durable.position().runtime_generation, 1);
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    for state in [
        restored.show().desired_switcher(),
        restored.realized_switcher(),
    ] {
        assert_eq!(
            (state.program(), state.preview()),
            (test_input_id(2), test_input_id(1))
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn native_video_ring_partition_preserves_one_aggregate_gpu_limit() {
    let defaults = NativeSourceLimits::default();
    let (ordinary, stingers) = partition_native_source_limits(1_920, 1_080, 1).unwrap();
    let expected_stinger =
        1_920_u64 * 1_080 * 8 * u64::from(defaults.max_video_frames_per_source.get());

    assert_eq!(stingers.max_retained_rgba16f_bytes, expected_stinger);
    assert_eq!(
        ordinary.max_retained_rgba16f_bytes + stingers.max_retained_rgba16f_bytes,
        defaults.max_retained_rgba16f_bytes
    );
    assert_eq!(
        partition_native_source_limits(1_920, 1_080, 5)
            .unwrap_err()
            .to_string(),
        format!(
            "native Stinger rings require {} retained bytes, exceeding aggregate limit {}",
            expected_stinger * 5,
            defaults.max_retained_rgba16f_bytes
        )
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_retirement_preflight_gate_is_bounded() {
    let retirements = NativeStingerRetirements::start().unwrap();
    let limit = stinger_mutation::retirement_limit_for_test();
    retirements.set_pending_for_test(limit - 1);
    assert!(retirements.can_accept());
    retirements.set_pending_for_test(limit);
    assert!(!retirements.can_accept());
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_source_validation_bounds_preload_kinds_and_audio() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        Vec::new(),
    )
    .unwrap();
    let mut source = SimulatedVideoSource::new(
        1,
        1,
        FrameRate::new(25, 1).unwrap(),
        native_clock_domain(),
        SourcePattern::Solid(Rgba8::new(0, 0, 0, 0)),
    )
    .unwrap();
    let retained = NativeResolvedSource::RetainedFrame {
        input: test_input_id(2),
        frame: source.next_frame().unwrap().unwrap(),
    };
    validate_native_stinger_sources(&durable, &[retained]).unwrap();

    let local = NativeResolvedSource::LocalVideo {
        input: test_input_id(2),
        path: PathBuf::from("/secret/stinger.mov"),
    };
    validate_native_stinger_sources(&durable, std::slice::from_ref(&local)).unwrap();
    assert!(stinger_mutation::native_stinger_requires_ffmpeg(
        &durable,
        std::slice::from_ref(&local)
    ));
    assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
        &baseline,
        std::slice::from_ref(&local)
    ));

    let mut audible_project = baseline.project().clone();
    audible_project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::StingerOnly,
        StingerMissingMediaFallback::Cut,
    ));
    let audible = StoredProject::from_project(
        audible_project,
        baseline.runtime_routing(),
        baseline.position(),
        Vec::new(),
    )
    .unwrap();
    validate_native_stinger_sources(
        &audible,
        &[NativeResolvedSource::LocalVideo {
            input: test_input_id(2),
            path: PathBuf::from("/secret/stinger.mov"),
        }],
    )
    .unwrap();

    let error = validate_native_stinger_sources(
        &durable,
        &[NativeResolvedSource::LiveFrame {
            input: test_input_id(2),
            frame: source.next_frame().unwrap().unwrap(),
        }],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("is live and cannot be preloaded"));

    let mut deferred_project = baseline.project().clone();
    deferred_project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        false,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let deferred = StoredProject::from_project(
        deferred_project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();
    validate_native_stinger_sources(&deferred, std::slice::from_ref(&local)).unwrap();
    assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
        &deferred,
        &[local]
    ));
}

#[test]
fn alpha_fade_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-alpha-fade",
            "settled-alpha-fade-key",
            CommandPayload::AlphaFade { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn slide_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-slide",
            "settled-slide-key",
            CommandPayload::Slide { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn zoom_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-zoom",
            "settled-zoom-key",
            CommandPayload::Zoom { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn fade_to_black_commands_persist_exact_settled_restart_state() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "fade-to-black",
            "fade-to-black-key",
            CommandPayload::FadeToBlack {
                active: true,
                duration_frames: 4,
            },
        ),
        0,
    )
    .unwrap();

    assert_eq!(
        durable.runtime_fade_to_black(),
        RuntimeFadeToBlack {
            desired: PersistedFadeToBlackState::BLACK,
            realized: PersistedFadeToBlackState::BLACK,
        }
    );
    let black = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert!(black.desired_fade_to_black().active);
    assert_eq!(
        black.desired_fade_to_black().position,
        fm_switcher::FadeToBlackPosition::BLACK
    );
    assert_eq!(live_engine_snapshot(&mut control), black);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "fade-from-black",
            "fade-from-black-key",
            CommandPayload::FadeToBlack {
                active: false,
                duration_frames: 3,
            },
        ),
        0,
    )
    .unwrap();

    assert_eq!(
        durable.runtime_fade_to_black(),
        RuntimeFadeToBlack::default()
    );
    let live = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(
        live.realized_fade_to_black().position,
        fm_switcher::FadeToBlackPosition::LIVE
    );
    assert_eq!(live_engine_snapshot(&mut control), live);
}

#[test]
fn idle_snapshot_checkpoint_survives_project_store_restart() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-checkpoint-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command("checkpoint-cut", "checkpoint-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    control.tick(&server).unwrap();

    let checkpoint =
        stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
    ProjectStore::new(&root).unwrap().save(&checkpoint).unwrap();
    let restarted = ProjectStore::new(&root).unwrap().load().unwrap();

    assert_eq!(restarted.project(), durable.project());
    assert_eq!(
        restarted.project().input_audio_strip(test_input_id(1)),
        durable.project().input_audio_strip(test_input_id(1))
    );
    assert_eq!(restarted.runtime_routing(), durable.runtime_routing());
    assert_eq!(
        restarted.idempotency_receipts(),
        durable.idempotency_receipts()
    );
    assert_eq!(
        restarted.position().frames_rendered,
        durable.position().frames_rendered + 1
    );
    assert_eq!(restarted.position().revision, durable.position().revision);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn audio_strip_command_checkpoints_every_control() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "audio-strip",
            "audio-strip-key",
            CommandPayload::SetInputAudioStrip {
                pan_basis_points: 0,
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                gain_millidb: -9_000,
                balance_basis_points: 2_500,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 1_200,
                sends: fm_types::audio_bus::AudioSends::MASTER,
            },
        ),
        0,
    )
    .unwrap();
    control.tick(&server).unwrap();

    let checkpoint =
        stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
    assert_eq!(
        checkpoint.project().input_audio_strip(test_input_id(1)),
        Some(InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::CENTER,
            gain: InputGainMilliDb::new(-9_000).unwrap(),
            balance: InputBalanceBasisPoints::new(2_500).unwrap(),
            muted: true,
            soloed: true,
            follow_video: false,
            delay_samples: InputDelaySamples::new(1_200).unwrap(),
            sends: fm_types::audio_bus::AudioSends::MASTER,
        })
    );
}

#[test]
fn authorization_denial_is_saved_before_becoming_replayable() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let saver = CountingSaver::default();
    let server = test_server(&control);
    let command = test_command("denied", "denied-key", CommandPayload::Cut);

    let denied = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &viewer(),
        &server,
        &command,
        0,
    )
    .unwrap();

    assert!(matches!(
        denied.submission.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(saver.0.get(), 1);
    assert_eq!(durable.idempotency_receipts().len(), 1);
    let replay = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &viewer(),
        &server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    assert_eq!(saver.0.get(), 1);
    assert_eq!(replay.runtime_events, []);

    let mut restarted = test_control(&durable);
    let restarted_server = test_server(&restarted);
    let mut restarted_durable = durable.clone();
    let replay_after_restart = execute_durable_command(
        &mut restarted,
        &saver,
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay_after_restart.submission.replayed);
    assert_eq!(
        replay_after_restart.submission.output.result,
        denied.submission.output.result
    );
    assert_eq!(saver.0.get(), 1);
    assert_eq!(restarted_durable, durable);
}

#[test]
fn crash_after_save_restarts_from_settled_receipt_without_reexecution() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let store = ProjectStore::new(&root).unwrap();
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let command = test_command(
        "crash-fade",
        "crash-fade-key",
        CommandPayload::Fade { duration_frames: 4 },
    );

    let crashed = catch_unwind(AssertUnwindSafe(|| {
        let _ = execute_durable_command(
            &mut control,
            &CrashAfterSave(&store),
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        );
    }));
    assert!(crashed.is_err());
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(durable.position().revision, 0);

    let saved = store.load().unwrap();
    assert_eq!(saved.position().revision, 1);
    assert_eq!(saved.position().frames_rendered, 4);
    assert_eq!(saved.idempotency_receipts().len(), 1);
    let mut restarted = test_control(&saved);
    let restarted_server = test_server(&restarted);
    let save_counter = CountingSaver::default();
    let mut restarted_durable = saved.clone();
    let replay = execute_durable_command(
        &mut restarted,
        &save_counter,
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    assert_eq!(replay.runtime_events, []);
    assert_eq!(save_counter.0.get(), 0);
    assert_eq!(restarted_durable, saved);

    fs::remove_dir_all(root).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn long_show_persists_a_bounded_receipt_window_that_restarts_and_accepts_work() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-receipts-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let store = ProjectStore::new(&root).unwrap();
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let commands = MAX_PERSISTED_RECEIPTS + 6;
    let mut project_bytes_at_limit = 0;
    for index in 0..commands {
        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                &format!("cut-{index}"),
                &format!("cut-key-{index:05}"),
                CommandPayload::Cut,
            ),
            0,
        )
        .unwrap();
        assert!(durable.idempotency_receipts().len() <= MAX_PERSISTED_RECEIPTS);
        if index + 1 == MAX_PERSISTED_RECEIPTS {
            store.save(&durable).unwrap();
            project_bytes_at_limit = fs::metadata(root.join("project.json")).unwrap().len();
        }
    }
    assert_eq!(durable.position().revision, commands as u64);
    assert_eq!(durable.idempotency_receipts().len(), MAX_PERSISTED_RECEIPTS);
    let keys = durable
        .idempotency_receipts()
        .iter()
        .map(IdempotencyReceipt::key)
        .collect::<std::collections::BTreeSet<_>>();
    // The oldest receipts were dropped; the newest are all retained.
    for index in 0..6 {
        assert!(!keys.contains(format!("cut-key-{index:05}").as_str()));
    }
    for index in 6..commands {
        assert!(keys.contains(format!("cut-key-{index:05}").as_str()));
    }

    // The bounded project file does not grow past the full window.
    store.save(&durable).unwrap();
    let project_bytes = fs::metadata(root.join("project.json")).unwrap().len();
    assert!(
        project_bytes <= project_bytes_at_limit + 64,
        "{project_bytes} > {project_bytes_at_limit}"
    );

    // A restart restores the window, replays a retained key, and accepts
    // new work with a fresh receipt that keeps the window bounded.
    let saved = store.load().unwrap();
    assert_eq!(saved.idempotency_receipts(), durable.idempotency_receipts());
    let mut restarted = test_control(&saved);
    let restarted_server = test_server(&restarted);
    let mut restarted_durable = saved.clone();
    let newest = format!("cut-key-{:05}", commands - 1);
    let replay = execute_durable_command(
        &mut restarted,
        &CountingSaver::default(),
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &test_command(
            &format!("cut-{}", commands - 1),
            &newest,
            CommandPayload::Cut,
        ),
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    let fresh = execute_durable_command(
        &mut restarted,
        &CountingSaver::default(),
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &test_command("after-restart", "after-restart-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    assert!(!fresh.submission.replayed);
    assert!(matches!(
        fresh.submission.output.result,
        CommandResult::Accepted { revision, .. } if revision == commands as u64 + 1
    ));
    assert_eq!(
        restarted_durable.idempotency_receipts().len(),
        MAX_PERSISTED_RECEIPTS
    );
    assert!(
        restarted_durable
            .idempotency_receipts()
            .iter()
            .any(|receipt| receipt.key() == "after-restart-key")
    );
    store.save(&restarted_durable).unwrap();
    assert_eq!(
        store.load().unwrap().idempotency_receipts().len(),
        MAX_PERSISTED_RECEIPTS
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn receipt_retention_ranks_rejections_after_their_revision_and_keeps_the_pinned_key() {
    let mut receipts = (1..=MAX_PERSISTED_RECEIPTS as u64)
        .map(|revision| {
            IdempotencyReceipt::accepted(format!("a-{revision:05}"), "id", revision, revision)
        })
        .collect::<Vec<_>>();
    // A rejection at revision 1 is newer than the accept that produced 1.
    receipts.push(IdempotencyReceipt::rejected(
        "r-1", "id", 1, "conflict", "stale", false,
    ));
    let retained = retained_receipts(receipts.clone(), None);
    assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
    assert!(!retained.iter().any(|receipt| receipt.key() == "a-00001"));
    assert!(retained.iter().any(|receipt| receipt.key() == "r-1"));

    // An older rejection drops first, unless it is the pinned key.
    receipts.push(IdempotencyReceipt::rejected(
        "r-0", "id", 0, "conflict", "stale", false,
    ));
    let retained = retained_receipts(receipts.clone(), None);
    assert!(!retained.iter().any(|receipt| receipt.key() == "r-0"));
    let retained = retained_receipts(receipts, Some("r-0"));
    assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
    assert!(retained.iter().any(|receipt| receipt.key() == "r-0"));
    // Pinning displaces the next-oldest receipt instead.
    assert!(!retained.iter().any(|receipt| receipt.key() == "r-1"));
    assert!(retained.iter().any(|receipt| receipt.key() == "a-00002"));
}

fn test_project() -> StoredProject {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(42).unwrap()),
        "Unit Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1_280, 720).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(44_100).unwrap(),
                sample_format: SampleFormat::I16,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    project.add_input(Input {
        id: test_input_id(1),
        name: "Custom solid and tone".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(7, 11, 13, 255)),
            SimulatedAudio::Sine { frequency_hz: 997 },
        )),
        required_capabilities: vec!["simulation.custom".into()],
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Bars".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    assert!(project.set_input_audio_strip(
        test_input_id(1),
        InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::CENTER,
            gain: InputGainMilliDb::new(-3_000).unwrap(),
            balance: InputBalanceBasisPoints::CENTER,
            delay_samples: InputDelaySamples::default(),
            muted: false,
            soloed: false,
            follow_video: true,
            sends: fm_types::audio_bus::AudioSends::MASTER,
        },
    ));
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn media_test_project(asset_uri: &str) -> StoredProject {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Media Unit Test",
        baseline.project().settings().clone(),
    );
    for value in [1, 2] {
        project.add_input(Input {
            id: test_input_id(value),
            name: format!("Media {value}"),
            kind: InputKind::Media {
                asset_uri: asset_uri.into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn scene_test_project() -> StoredProject {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Scene Unit Test",
        baseline.project().settings().clone(),
    );
    let scene_id = SceneId::new(NonZeroU128::new(1).unwrap());
    project.add_input(Input {
        id: test_input_id(1),
        name: "Scene".into(),
        kind: InputKind::Scene {
            scene_id,
            audio_source: Some(test_input_id(2)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Audio".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id,
        name: "Program".into(),
        background: ModelRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn test_input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

/// Live input add and remove fold into the stored project before they
/// are journaled: an added input persists with the kind its command
/// carried, and a removal the engine allows is still refused, without
/// journaling anything, while a persisted reference the engine does not
/// model (here an alias target) names the input.
#[test]
#[allow(clippy::too_many_lines)]
fn live_input_catalogue_changes_sync_the_project_and_refuse_alias_targets() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Camera".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(4),
        name: "Camera alias".into(),
        kind: InputKind::Alias {
            target: test_input_id(3),
            crop: None,
        },
        required_capabilities: Vec::new(),
    });
    let mut durable = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let saver = CountingSaver::default();
    let before = durable.clone();

    let refused = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "remove-target",
            "remove-target-key",
            CommandPayload::RemoveInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected {
            code,
            message,
            retryable: false,
            current_revision: 0,
            ..
        } if code == "conflict"
            && message.contains("cannot be removed: it is the target of alias input")
    ));
    // The persisted reference travels as a typed reason (Protocol 2.45).
    let CommandResult::Rejected { reason, .. } = &refused.submission.output.result else {
        unreachable!()
    };
    assert!(
        matches!(
            reason,
            Some(fm_protocol::RefusalReason::InputInUse {
                input: 3,
                usage: fm_protocol::InputUse::AliasTarget { .. },
            })
        ),
        "{reason:?}"
    );
    assert_eq!(saver.0.get(), 0, "a refused removal journals nothing");
    assert_eq!(durable, before);

    let added = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "add-guest",
            "add-guest-key",
            CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(5)),
                name: "Guest".into(),
                source: fm_protocol::LiveInputSource::Solid {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        added.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    let guest = durable
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == test_input_id(5))
        .expect("the added input is persisted");
    assert_eq!(guest.name, "Guest");
    assert_eq!(
        guest.kind,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
            SimulatedAudio::Silence,
        ))
    );
    assert!(
        durable
            .project()
            .input_audio_strips()
            .iter()
            .any(|strip| strip.input == test_input_id(5))
    );

    let removed = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "remove-alias",
            "remove-alias-key",
            CommandPayload::RemoveInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(4)),
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        removed.submission.output.result,
        CommandResult::Accepted { revision: 2, .. }
    ));
    assert_eq!(
        durable
            .project()
            .inputs()
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        [1, 2, 3, 5].map(test_input_id)
    );
    assert_eq!(saver.0.get(), 2);
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_preflight_rejections_are_path_free_and_retryable_by_resubmission() {
    let slot = fm_protocol::WireStingerSlotId::new(8).unwrap();
    for payload in [
        CommandPayload::ConfigureStinger {
            slot,
            media_input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
            preload: true,
            cut_point_frames: 3,
            audio_policy: fm_protocol::StingerAudioPolicy::Muted,
            missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Cut,
        },
        CommandPayload::RemoveStinger { slot },
    ] {
        let result =
            native_stinger_preflight_rejection(&test_command("native", "native-key", payload), 7);
        assert!(matches!(
            result,
            CommandResult::Rejected {
                ref code,
                ref message,
                current_revision: 7,
                retryable: false,
                ..
            } if code == "unavailable"
                && message == "native Stinger resources could not be prepared"
        ));
    }
}

fn test_control(project: &StoredProject) -> ControlService<Policy> {
    ControlService::new(
        restore_engine(project).unwrap(),
        Policy::development(),
        "unit-engine",
        "unit-log",
        ControlLimits::default(),
    )
}

fn test_server(control: &ControlService<Policy>) -> ServerIdentity {
    let engine = control.diagnostics().engine;
    ServerIdentity {
        engine_id: engine.engine_id,
        project_id: "42".into(),
        state_epoch: engine.state_epoch,
        log_id: engine.log_id,
    }
}

fn test_command(id: &str, key: &str, payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: PROTOCOL_VERSION,
        id: id.into(),
        idempotency_key: key.into(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    }
}

fn operator() -> Principal {
    development_principal().unwrap()
}

fn viewer() -> Principal {
    Principal::authenticated(
        UserId::new("unit-viewer").unwrap(),
        SessionId::new("unit-session").unwrap(),
        [AuthRole::Viewer],
    )
}

fn live_engine_snapshot(control: &mut ControlService<Policy>) -> EngineSnapshot {
    let prepared = control
        .prepare_submit(
            &viewer(),
            test_command("snapshot-probe", "snapshot-probe-key", CommandPayload::Cut),
            0,
        )
        .unwrap()
        .prepared()
        .unwrap();
    let snapshot = prepared.project(0).unwrap();
    prepared.abort();
    snapshot
}

const STREAM_TARGET_A: u128 = 7_001;
const STREAM_TARGET_B: u128 = 7_002;

/// A project with one routed output and two stream destinations, the first
/// carrying an authored desired-running intent when `running[0]` is true.
fn test_project_with_streams(running: [bool; 2]) -> StoredProject {
    use fm_model::{
        AudioBus, Output, Rgba8 as TestRgba8, Scene as TestScene, StartupPolicy as ModelStartup,
        StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::{BusId as TestBusId, OutputId as TestOutputId, SceneId as TestSceneId};

    let baseline = test_project();
    let mut project = baseline.project().clone();
    let scene_id = TestSceneId::new(NonZeroU128::new(900).unwrap());
    let bus_id = TestBusId::new(NonZeroU128::new(901).unwrap());
    let output_id = TestOutputId::new(NonZeroU128::new(902).unwrap());
    project.add_scene(TestScene {
        id: scene_id,
        name: "Program".into(),
        background: TestRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: bus_id,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id,
        name: "Program output".into(),
        video_source: scene_id,
        audio_source: bus_id,
        startup: ModelStartup::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    for (index, number) in [STREAM_TARGET_A, STREAM_TARGET_B].into_iter().enumerate() {
        let target = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(number).unwrap()),
            format!(
                "Destination {}",
                u128::from(u8::try_from(index).unwrap() + 1)
            ),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("ingest.example.com/live").unwrap(),
            Some(StreamKey::parse("unit-secret-key").unwrap()),
            output_id,
        )
        .unwrap()
        .with_startup(ModelStartup::ReconcileDesiredState)
        .set_running(running[index]);
        project.add_stream_target_checked(target).unwrap();
    }
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap()
}

#[test]
fn restore_engine_seeds_stream_inventory_and_desired_running() {
    let durable = test_project_with_streams([true, false]);
    let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();

    for switcher in [
        snapshot.show().desired_switcher(),
        snapshot.realized_switcher(),
    ] {
        assert_eq!(switcher.streams().len(), 2);
        // Project vector order is preserved.
        assert_eq!(switcher.streams()[0].name(), "Destination 1");
        assert_eq!(
            switcher.streams()[1].id(),
            SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(STREAM_TARGET_B).unwrap())
        );
        assert_eq!(switcher.streams()[1].name(), "Destination 2");
        assert_eq!(
            switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_A).unwrap()
            )),
            Some(true)
        );
        assert_eq!(
            switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_B).unwrap()
            )),
            Some(false)
        );
        assert_eq!(
            switcher.running_stream_targets(),
            &std::collections::BTreeSet::from([SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_A).unwrap()
            )])
        );
    }
    // Authored running flags survive a restore round trip unchanged.
    assert!(durable.project().stream_targets()[0].running());
    assert!(!durable.project().stream_targets()[1].running());
}

#[test]
fn stream_commands_persist_desired_running_and_reject_unknown_targets() {
    let mut durable = test_project_with_streams([false, false]);
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let wire_target =
        || fm_protocol::WireStreamTargetId::new(NonZeroU128::new(STREAM_TARGET_A).unwrap());
    let execute = |control: &mut ControlService<Policy>,
                   durable: &mut StoredProject,
                   id: &str,
                   key: &str,
                   payload| {
        execute_durable_command(
            control,
            &CountingSaver::default(),
            durable,
            &operator(),
            &server,
            &test_command(id, key, payload),
            0,
        )
    };

    let started = execute(
        &mut control,
        &mut durable,
        "stream-on",
        "stream-on-key",
        CommandPayload::StreamStart {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        started.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert!(matches!(
        started.submission.output.events.as_slice(),
        [fm_protocol::EventMessage {
            payload: fm_protocol::EventPayload::StreamsChanged { .. },
            ..
        }]
    ));
    assert_eq!(durable.position().frames_rendered, 1);
    assert!(durable.project().stream_targets()[0].running());

    let rejected = execute(
        &mut control,
        &mut durable,
        "stream-unknown",
        "stream-unknown-key",
        CommandPayload::StreamStart {
            target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(9_999).unwrap()),
        },
    )
    .unwrap();
    assert!(matches!(
        rejected.submission.output.result,
        CommandResult::Rejected { ref code, .. } if code == "not_found"
    ));
    assert!(!durable.project().stream_targets()[1].running());

    let stopped = execute(
        &mut control,
        &mut durable,
        "stream-off",
        "stream-off-key",
        CommandPayload::StreamStop {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        stopped.submission.output.result,
        CommandResult::Accepted { revision: 2, .. }
    ));
    assert!(!durable.project().stream_targets()[0].running());
    assert_eq!(durable.position().frames_rendered, 2);

    // The replayed start answers from its original receipt and changes
    // nothing about the stored project.
    let replayed = execute(
        &mut control,
        &mut durable,
        "stream-replay",
        "stream-on-key",
        CommandPayload::StreamStart {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        replayed.submission.output.result,
        CommandResult::Accepted { ref id, revision: 1, .. } if id == "stream-on"
    ));
    assert!(!durable.project().stream_targets()[0].running());

    // The persisted project alone reproduces the live engine's desired
    // stream state on restore.
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
}

/// A plain project whose authored per-slot desired recording flags are
/// `desired` (slot 1 then slot 2).
fn test_project_with_recording(desired: [bool; 2]) -> StoredProject {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    for (slot, active) in ModelRecorderSlot::ALL.into_iter().zip(desired) {
        project.set_recording_desired(slot, active);
    }
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap()
}

/// The wire recorder slot for `number`, which tests only call with 1 or 2.
fn wire_recorder(number: u8) -> fm_protocol::WireRecorderSlot {
    fm_protocol::WireRecorderSlot::new(number).expect("test recorder slots are 1 or 2")
}

#[test]
fn operator_state_text_lists_every_stream_and_both_recorder_flags() {
    let control = test_control(&test_project_with_streams([true, false]));
    let text = operator_state_text(&control, None);

    assert!(text.starts_with("streams=2\n"));
    assert!(text.contains(&format!(
        "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
    )));
    assert!(text.contains(&format!(
        "stream target={STREAM_TARGET_B} name=Destination 2 desired=false"
    )));
    assert!(text.contains("recorders=desired=false,false"));
    for leaked in ["ingest.example.com", "unit-secret-key", "/live"] {
        assert!(!text.contains(leaked), "operator state leaked {leaked}");
    }
}

#[test]
fn operator_state_text_carries_both_desired_recorder_flags() {
    let control = test_control(&test_project_with_recording([true, false]));
    let text = operator_state_text(&control, None);
    assert!(text.contains("recorders=desired=true,false"));

    let control = test_control(&test_project_with_recording([false, true]));
    let text = operator_state_text(&control, None);
    assert!(text.contains("recorders=desired=false,true"));
}

#[test]
fn operator_state_text_truncates_over_the_cap_with_a_marker() {
    let streams: Vec<StreamStatus> = (0..64)
        .map(|index| StreamStatus {
            target: WireStreamTargetId::new(NonZeroU128::new(10_000 + index).unwrap()),
            name: "D".repeat(128),
            desired_running: index % 2 == 0,
            realized: StreamRealizedState::Stopped,
            detail: None,
        })
        .collect();
    let text = render_operator_state(&streams, &[], None);
    assert!(text.len() <= OPERATOR_STATE_MAX_BYTES);
    assert!(text.ends_with(OPERATOR_STATE_TRUNCATION_MARKER));
}

#[test]
fn operator_state_publishes_only_when_a_listener_is_attached() {
    let control = Rc::new(RefCell::new(test_control(&test_project_with_streams([
        true, false,
    ]))));
    let mut process = register_process_shutdown().unwrap();
    process.publish_operator_state(None);
    assert!(process.status.operator_state().is_none());

    process.attach_control(&control);
    process.publish_operator_state(None);
    let published = process
        .status
        .operator_state()
        .expect("operator state was published");
    assert!(published.contains(&format!(
        "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
    )));
}

#[test]
fn startup_reconcile_opens_only_slots_with_a_true_desired_flag() {
    // An unconfigured slot is never selected, even when desired.
    assert_eq!(
        startup_recording_slots([false, false], [true, true]),
        [false, false]
    );
    // A configured slot whose own flag is false stays closed even when
    // the other slot is recording.
    assert_eq!(
        startup_recording_slots([true, true], [false, false]),
        [false, false]
    );
    assert_eq!(
        startup_recording_slots([true, true], [true, false]),
        [true, false]
    );
    assert_eq!(
        startup_recording_slots([true, true], [false, true]),
        [false, true]
    );
    assert_eq!(
        startup_recording_slots([true, true], [true, true]),
        [true, true]
    );
    // An unconfigured slot cannot be selected by its desired flag alone.
    assert_eq!(
        startup_recording_slots([true, false], [true, true]),
        [true, false]
    );
}

#[test]
fn recording_commands_route_to_only_the_addressed_slot() {
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStart {
            recorder: wire_recorder(1),
        }),
        Some((true, 0))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStop {
            recorder: wire_recorder(1),
        }),
        Some((false, 0))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStart {
            recorder: wire_recorder(2),
        }),
        Some((true, 1))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStop {
            recorder: wire_recorder(2),
        }),
        Some((false, 1))
    );
    // Every other command family leaves recording realization untouched.
    assert_eq!(recording_command_slot(&CommandPayload::Cut), None);
    assert_eq!(
        recording_command_slot(&CommandPayload::StreamStart {
            target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
        }),
        None
    );
}

#[test]
fn restore_engine_seeds_each_recording_desired_flag_into_both_switchers() {
    for desired_flags in [[false, false], [true, false], [false, true], [true, true]] {
        let durable = test_project_with_recording(desired_flags);
        let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
        // Idle restore demands desired/realized agreement, so BOTH
        // switchers must carry every persisted slot flag or startup would
        // fail.
        for (index, slot) in ModelRecorderSlot::ALL.into_iter().enumerate() {
            let recorder = switcher_recorder_slot(slot);
            assert_eq!(
                snapshot
                    .show()
                    .desired_switcher()
                    .recording_desired(recorder),
                desired_flags[index],
                "slot {} desired flag was not seeded into the show switcher",
                slot.get()
            );
            assert_eq!(
                snapshot.realized_switcher().recording_desired(recorder),
                desired_flags[index],
                "slot {} desired flag was not seeded into the realized switcher",
                slot.get()
            );
        }
    }
}

/// Executes `command` as the native daemon does, realizing each of its
/// frames one pacer frame at a time and noting the Program input of each.
fn native_paced_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    server: &ServerIdentity,
    command: &CommandMessage,
    realized: &mut Vec<(u64, InputId, Option<InputId>)>,
) -> CommandResult {
    execute_durable_command_with_ticks(
        control,
        store,
        durable,
        &operator(),
        server,
        command,
        0,
        None,
        Realize::All,
        |control, server| {
            let outcome = control.tick(server)?;
            realized.push((
                outcome.frame.frame.get(),
                outcome.frame.program.primary,
                outcome.frame.program.secondary,
            ));
            Ok(outcome.runtime_events)
        },
    )
    .unwrap()
    .submission
    .output
    .result
}

#[test]
#[allow(clippy::too_many_lines)]
fn playlist_clock_cuts_program_on_scheduled_frames_under_native_pacing() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use journal::DurableJournal;
    use playlist_clock::{Pacing, PlaylistClock};

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("native-playlist.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let mut clock = PlaylistClock::default();
    let start_frame;
    let mut advances = Vec::new();
    {
        let journal = DurableJournal::open(&store).unwrap();
        let entries = vec![
            PlaylistEntrySpec {
                id: 1,
                input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                duration_frames: 10,
                transition: PlaylistTransitionKind::Cut,
                transition_frames: 0,
                enabled: true,
            },
            PlaylistEntrySpec {
                id: 2,
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                duration_frames: 10,
                transition: PlaylistTransitionKind::Fade,
                transition_frames: 5,
                enabled: true,
            },
        ];
        let set = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries,
                },
            ),
            &mut realized,
        );
        assert!(matches!(set, CommandResult::Accepted { .. }), "{set:?}");
        // The native pacer realizes idle frames between commands; nothing
        // is pending, so the clock stays quiet.
        for _ in 0..3 {
            assert_eq!(clock.due(&control, Pacing::Engine), None);
            let frame = control.tick(&server).unwrap().frame;
            realized.push((
                frame.frame.get(),
                frame.program.primary,
                frame.program.secondary,
            ));
        }
        let start = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &test_command(
                "start",
                "start-key",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::AfterFrames(10),
                        from: None,
                    },
                },
            ),
            &mut realized,
        );
        assert!(matches!(start, CommandResult::Accepted { .. }), "{start:?}");
        start_frame = control
            .playlist_advance_frame()
            .expect("the start is armed");
        assert!(start_frame > control.frame_cursor());

        // One pass per pacer frame: the clock submits the advance only
        // when its boundary is the next frame to realize, else the
        // pacer realizes an idle frame.
        while control.frame_cursor() < start_frame + 40 {
            if let Some(advance) = clock.due(&control, Pacing::Engine) {
                let frame = advance.frame;
                assert_eq!(frame, control.frame_cursor(), "never early, never late");
                let result = native_paced_command(
                    &mut control,
                    &journal,
                    &mut durable,
                    &server,
                    &advance.command(),
                    &mut realized,
                );
                clock.observe(&advance, &result);
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{result:?}"
                );
                advances.push(frame);
            } else {
                let frame = control.tick(&server).unwrap().frame;
                realized.push((
                    frame.frame.get(),
                    frame.program.primary,
                    frame.program.secondary,
                ));
            }
        }
    }
    assert_eq!(
        advances,
        [start_frame, start_frame + 10, start_frame + 20],
        "entry 1 airs, entry 2 airs, the playlist finishes"
    );
    // Frames are realized once each, in order, with no gap.
    assert!(realized.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1));
    let program_at = |frame: u64| {
        realized
            .iter()
            .find(|(realized, ..)| *realized == frame)
            .map(|&(_, primary, secondary)| (primary, secondary))
            .unwrap()
    };
    let (one, two) = (test_input_id(1), test_input_id(2));
    assert_eq!(program_at(start_frame - 1), (one, None));
    // Entry 1 cuts input 2 to Program on exactly its frame.
    assert_eq!(program_at(start_frame), (two, None));
    assert_eq!(program_at(start_frame + 9), (two, None));
    // Entry 2 fades input 1 in from its frame over five frames.
    assert_eq!(program_at(start_frame + 10).1, Some(one));
    assert_eq!(program_at(start_frame + 15), (one, None));
    assert_eq!(program_at(start_frame + 30), (one, None));
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Finished { last: Some(2) }
    ));

    // The journal holds the clock's advances, so replay alone reproduces
    // the playlist and Program routing the operator saw.
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 5);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
    assert_eq!(replayed.runtime_routing(), durable.runtime_routing());
    assert_eq!(replayed.position(), durable.position());
}

#[test]
fn a_late_native_playlist_advance_replays_on_the_frame_it_ran() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use journal::DurableJournal;
    use playlist_clock::{Pacing, PlaylistClock, advance_command};

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("late-playlist.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let late_frame;
    {
        let journal = DurableJournal::open(&store).unwrap();
        for (id, payload) in [
            (
                "set",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 50,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        enabled: true,
                    }],
                },
            ),
            (
                "start",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::AfterFrames(5),
                        from: None,
                    },
                },
            ),
        ] {
            let result = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &test_command(id, &format!("{id}-key"), payload),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
        }
        let due = control.playlist_advance_frame().unwrap();
        // The pacer realizes frames past the boundary without a clock
        // pass (as the recorder startup barrier does), so the advance
        // runs on the next frame instead.
        while control.frame_cursor() < due + 7 {
            control.tick(&server).unwrap();
        }
        let mut clock = PlaylistClock::default();
        let advance = clock.due(&control, Pacing::Engine).unwrap();
        let (frame, revision) = (advance.frame, advance.revision);
        assert_eq!(frame, due + 7);
        late_frame = frame;
        let result = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &advance_command(frame, revision),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
    }
    // The entry keeps its scheduled start, so later boundaries do not
    // shift; only its cut lands late.
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, started_frame, .. }
            if started_frame == late_frame - 7
    ));
    assert_eq!(realized.last().unwrap().0, late_frame);
    assert_eq!(realized.last().unwrap().1, test_input_id(2));
    let scan = store.scan_journal().unwrap();
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
}

/// The simulated daemon's frame loop realizes every frame a command is
/// durable past, a scene's In phase as well as a timed transition,
/// although the phase is no work in flight: the scene plays to the end
/// of its phase at wall time instead of holding its first frame until
/// the next command.
#[test]
fn the_simulated_pacer_plays_a_scene_phase_to_where_the_command_settles() {
    use fm_model::{Rgba8 as TestRgba8, Scene as TestScene};
    use fm_types::SceneId as TestSceneId;

    let mut project = test_project().project().clone();
    let scene_id = TestSceneId::new(NonZeroU128::new(1).unwrap());
    project.add_input(Input {
        id: test_input_id(3),
        name: "Scene".into(),
        kind: InputKind::Scene {
            scene_id,
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(TestScene {
        id: scene_id,
        name: "Animated".into(),
        background: TestRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation {
            clip: fm_model::SceneClip::new(fm_model::default_scene_timebase(), 12, 0).unwrap(),
            revision: 0,
        },
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(3)));
    let manifest = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(3)),
            realized_preview_id: Some(test_input_id(3)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let cut = execute_durable_command_with_ticks(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command("cut", "cut-key", CommandPayload::Cut),
        0,
        None,
        Realize::First,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )
    .unwrap();
    assert!(matches!(
        cut.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    // The Cut realized its first frame; the scene's In phase runs on.
    let settled = durable.position().frames_rendered;
    assert_eq!(control.frame_cursor(), 1);
    assert!(settled > 2, "{settled}");
    assert!(!control.work_in_flight());
    assert!(control.scenes_in_motion());

    let rate = durable.project().settings().frame_rate;
    let mut pacer = sessions::SimulatedPacer::default();
    let start = std::time::Instant::now();
    pacer
        .poll(&mut control, &server, rate, settled, start)
        .unwrap();
    // Wall time paces it: one frame (40 ms at 25 fps) at a time.
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_millis(40),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), 2);
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), settled);
    assert!(!control.scenes_in_motion());
    // Settled, it renders no idle frames.
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_secs(20),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), settled);
}

/// A native command realizes its first frame and leaves the rest of a
/// timed transition to the frame loop: it is durable as the project the
/// transition ends on, a command committed while the transition runs is
/// accepted (or refused, as a Cut is while one is in flight) on that
/// frame, and journal replay ticks to the frame each recorded command
/// was committed at, landing on every recorded position.
#[test]
#[allow(clippy::too_many_lines)]
fn a_command_committed_mid_transition_replays_onto_its_frame() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("mid-transition.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let execute = |control: &mut ControlService<Policy>,
                   durable: &mut StoredProject,
                   journal: &DurableJournal,
                   id: &str,
                   payload: CommandPayload| {
        execute_durable_command_with_ticks(
            control,
            journal,
            durable,
            &operator(),
            &server,
            &test_command(id, &format!("{id}-key"), payload),
            0,
            None,
            Realize::First,
            |control, server| Ok(control.tick(server)?.runtime_events),
        )
        .unwrap()
        .submission
        .output
        .result
    };
    let mut positions = Vec::new();
    {
        let journal = DurableJournal::open(&store).unwrap();
        for _ in 0..3 {
            control.tick(&server).unwrap();
        }
        let began = control.frame_cursor();
        let fade = execute(
            &mut control,
            &mut durable,
            &journal,
            "fade",
            CommandPayload::Fade {
                duration_frames: 10,
            },
        );
        assert!(matches!(fade, CommandResult::Accepted { .. }), "{fade:?}");
        // Only the first frame was realized; the project is durable as
        // the transition ends.
        assert_eq!(control.frame_cursor(), began + 1);
        assert!(control.work_in_flight());
        assert_eq!(durable.position().frames_rendered, began + 10);
        positions.push(durable.position());
        // The frame loop realizes the transition; another client's
        // edit lands on one of its frames and settles where it ends.
        for _ in 0..3 {
            control.tick(&server).unwrap();
        }
        let renamed = execute(
            &mut control,
            &mut durable,
            &journal,
            "rename",
            CommandPayload::RenameInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                name: "Renamed mid transition".into(),
            },
        );
        assert!(
            matches!(renamed, CommandResult::Accepted { .. }),
            "{renamed:?}"
        );
        assert_eq!(durable.position().frames_rendered, began + 10);
        positions.push(durable.position());
        let cut = execute(
            &mut control,
            &mut durable,
            &journal,
            "cut",
            CommandPayload::Cut,
        );
        assert!(
            matches!(&cut, CommandResult::Rejected { code, .. } if code == "conflict"),
            "{cut:?}"
        );
        while control.work_in_flight() {
            control.tick(&server).unwrap();
        }
        assert_eq!(control.frame_cursor(), began + 10);
        assert_eq!(
            stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap())
                .unwrap()
                .position(),
            durable.position()
        );
        control.tick(&server).unwrap();
        let after = execute(
            &mut control,
            &mut durable,
            &journal,
            "cut-after",
            CommandPayload::Cut,
        );
        assert!(matches!(after, CommandResult::Accepted { .. }), "{after:?}");
        positions.push(durable.position());
    }
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), positions.len());
    for (batch, position) in scan.batches().iter().zip(&positions) {
        assert_eq!(
            journal::decode_mutation(batch.payload()).unwrap().position,
            *position
        );
    }
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.project(), durable.project());
}

/// Idle frames a native daemon realizes after its last command survive a
/// crash through the progress marker: markers land one second of frames
/// apart, replay and recovery tick idle up to the newest one and land on
/// its whole position, and a fold moves idle progress into the manifest.
#[test]
fn idle_progress_markers_replay_past_the_last_command_and_fold_into_the_manifest() {
    use fm_protocol::{PlaylistEntrySpec, PlaylistLoopMode, PlaylistTransitionKind};
    use journal::{DurableJournal, PROGRESS_FOLD_SECONDS, decode_progress, frames_in};

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("progress.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let rate = manifest.project().settings().frame_rate;
    let control = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let marker_frames = frames_in(journal::PROGRESS_MARKER_SECONDS, rate);
    {
        let journal = DurableJournal::open(&store).unwrap();
        let result = native_paced_command(
            &mut control.borrow_mut(),
            &journal,
            &mut durable,
            &server,
            &test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 50,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        enabled: true,
                    }],
                },
            ),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
        let command_end = durable.position().frames_rendered;
        let mut progress = NativeProgress::new(&durable);
        // The pacer realizes idle frames; the loop polls after each.
        let mut markers = Vec::new();
        for _ in 0..(marker_frames * 2 + 7) {
            control.borrow_mut().tick(&server).unwrap();
            progress.poll(&control, &journal, &mut durable);
            // Markers are written on the journal's worker.
            journal.flush().unwrap();
            if let Some(marker) = store_progress(&store)
                && markers.last() != Some(&marker.frames_rendered)
            {
                markers.push(marker.frames_rendered);
            }
        }
        assert_eq!(
            markers,
            [command_end + marker_frames, command_end + 2 * marker_frames],
            "one marker per second of realized frames"
        );
        assert_eq!(
            durable.position().frames_rendered,
            command_end,
            "markers leave the manifest alone"
        );
    }

    // Replay (and so startup recovery) resumes at the newest marker, on
    // its whole position, not where the last command ended.
    let scan = store.scan_journal().unwrap();
    let marker = decode_progress(scan.progress().unwrap().payload()).unwrap();
    let replayed = replay_journal(
        &manifest,
        &BundleAssets::default(),
        scan.batches(),
        Some(marker),
    )
    .unwrap();
    assert_eq!(replayed.position(), marker);
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(recovered.position(), marker);
    assert_eq!(store.load().unwrap().position(), marker);
    // The recovered manifest holds the marker, so it has nothing left to
    // replay and a second recovery changes nothing.
    assert_eq!(load_and_recover(&store, false).unwrap().position(), marker);

    // A fold checkpoints idle progress into the manifest itself.
    let control = Rc::new(RefCell::new(test_control(&recovered)));
    let server = test_server(&control.borrow());
    let mut durable = recovered;
    let journal = DurableJournal::open(&store).unwrap();
    let mut progress = NativeProgress::new(&durable);
    let fold_at = marker.frames_rendered + frames_in(PROGRESS_FOLD_SECONDS, rate);
    while control.borrow().frame_cursor() < fold_at {
        control.borrow_mut().tick(&server).unwrap();
        progress.poll(&control, &journal, &mut durable);
        // Frames come far faster than a real pacer's, so each write is
        // waited for: a poll finding the worker busy defers its write.
        journal.flush().unwrap();
    }
    assert_eq!(durable.position().frames_rendered, fold_at);
    assert_eq!(store.load().unwrap().position(), durable.position());
}

/// The early checkpoint runs on the journal's persistence worker, and a
/// command recorded right after it is handed over still lands after it:
/// the fold covers exactly the mutations recorded before it was asked
/// for, the later append stays in the journal above the new checkpoint,
/// and recovery holds every recorded command.
#[test]
fn a_background_checkpoint_folds_what_preceded_it_and_later_appends_stay_journaled() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("background.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let cut = |index: u64| {
        test_command(
            &format!("cut-{index}"),
            &format!("cut-{index}-key"),
            CommandPayload::Cut,
        )
    };
    let early = 48;
    {
        let journal = DurableJournal::open(&store).unwrap();
        for index in 1..=early {
            let result = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &cut(index),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
        }
        assert_eq!(durable.position().revision, early);
        journal.checkpoint_if_due(&durable).unwrap();
        // Handed over behind the checkpoint, whether or not it finished.
        let result = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &cut(early + 1),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
        journal.flush().unwrap();
        assert!(!journal.busy());
        assert_eq!(store.load().unwrap().position().revision, early);
        // Dropped without settling, as a crash would leave it.
    }
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.checkpoint_sequence(), early);
    assert_eq!(scan.batches().len(), 1);
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(recovered.position(), durable.position());
}

fn store_progress(store: &ProjectStore) -> Option<ProjectPosition> {
    // The newest current marker, read beside the open writer.
    store
        .scan_journal()
        .ok()?
        .progress()
        .map(|progress| journal::decode_progress(progress.payload()).unwrap())
}

/// Replay refuses, before ticking anything, a journal that asks it to
/// tick idle frames past its bound: a damaged frame number becomes a
/// clear startup error instead of a startup that never finishes.
#[test]
fn replay_refuses_idle_frames_past_its_bound() {
    let manifest = test_project();
    let rate = manifest.project().settings().frame_rate;
    let limit = journal::frames_in(journal::MAX_REPLAY_IDLE_SECONDS, rate);
    let mut budget = IdleReplayBudget::new(rate);
    assert!(budget.check(10, 10 + limit).is_ok());
    assert!(budget.check(10, 11 + limit).is_err());
    budget.charge(limit - 5);
    assert!(budget.check(0, 5).is_ok());
    assert!(budget.check(0, 6).is_err());

    let damaged = ProjectPosition {
        frames_rendered: u64::MAX / 2,
        ..manifest.position()
    };
    let started = Instant::now();
    let error = replay_journal(&manifest, &BundleAssets::default(), &[], Some(damaged))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("progress marker") && error.contains("idle frames"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused before ticking"
    );
}

/// A playlist advance refused at `revision` for a reason that cannot
/// clear by itself.
fn refused_advance(revision: u64) -> CommandResult {
    CommandResult::Rejected {
        id: "advance".into(),
        code: "invalid_command".into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    }
}

#[test]
fn playlist_clock_waits_for_a_new_revision_after_a_final_refusal() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use playlist_clock::{Pacing, PlaylistClock};

    let mut control = test_control(&test_project());
    let principal = operator();
    let set = control
        .submit(
            &principal,
            test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 10,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        enabled: true,
                    }],
                },
            ),
            0,
        )
        .unwrap();
    assert!(set.is_accepted());
    let mut clock = PlaylistClock::default();
    // Nothing pending: under native pacing no wall-clock anchor exists,
    // so snapshots report the engine cursor itself.
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    let rate = FrameRate::new(25, 1).unwrap();
    let start = control
        .submit(
            &principal,
            test_command(
                "start",
                "start-key",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::Now,
                        from: None,
                    },
                },
            ),
            0,
        )
        .unwrap();
    assert!(start.is_accepted());
    let server = test_server(&control);
    control.tick(&server).unwrap();
    let due_frame = control.playlist_advance_frame().unwrap();
    while control.frame_cursor() < due_frame {
        assert_eq!(clock.due(&control, Pacing::Engine), None, "not yet due");
        control.tick(&server).unwrap();
    }
    let advance = clock.due(&control, Pacing::Engine).unwrap();
    let (frame, revision) = (advance.frame, advance.revision);
    assert_eq!(frame, due_frame);
    assert_eq!(
        clock.observed_frame(control.frame_cursor(), rate, Instant::now()),
        control.frame_cursor()
    );
    clock.observe(&advance, &refused_advance(revision));
    // A refusal that cannot clear by itself is not retried however long
    // the show runs at this revision.
    for _ in 0..3 * clock_retry::SLOW_RETRY_FRAMES {
        control.tick(&server).unwrap();
        assert_eq!(clock.due(&control, Pacing::Engine), None);
    }
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    // Another accepted command changes the revision, so the clock tries
    // again; Stop, Pause and Set are accepted on native daemons too.
    let stop = test_command(
        "stop",
        "stop-key",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Stop,
        },
    );
    assert_eq!(
        playlist_clock::playlist_command_refusal(&stop, revision),
        None
    );
    assert!(control.submit(&principal, stop, 0).unwrap().is_accepted());
    assert_eq!(clock.due(&control, Pacing::Engine), None, "stopped");
    assert!(
        playlist_clock::playlist_command_refusal(
            &test_command(
                "client",
                "client-key",
                CommandPayload::PlaylistAdvance { frame }
            ),
            revision,
        )
        .is_some(),
        "only the clock advances the playlist"
    );
}

/// The commands that set cue list `7`, whose one cue waits `wait`
/// frames and then cuts input 1 to Program, and GO it.
fn waiting_cue_commands(wait: u32) -> [CommandMessage; 2] {
    use fm_protocol::{
        CueListSpec, CueOperation, TransitionFavoriteEffect, WireCue, WireCueAction,
    };
    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Open".into(),
            auto_follow: false,
            actions: vec![
                WireCueAction::Wait { frames: wait },
                WireCueAction::Take {
                    input: Some(fm_types::InputTarget::Fixed(
                        fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    )),
                    effect: TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                },
            ],
        }],
    };
    [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default(),
                },
            },
        ),
    ]
}

/// Submits [`waiting_cue_commands`] on `control`: returns the frame the
/// wait ends on.
fn go_waiting_cue(control: &mut ControlService<Policy>, wait: u32) -> u64 {
    for command in waiting_cue_commands(wait) {
        let id = command.id.clone();
        let submitted = control.submit(&operator(), command, 0).unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    let (list, due) = control.cue_advance_frame().expect("the cue waits");
    assert_eq!(list, 7);
    due
}

/// The bug this guards: on the simulated runtime the media clock
/// rendered idle frames past a waiting cue's due frame. A cue that rolls
/// a clip and then waits ("play, wait, take") anchors its wait to the
/// frame before its GO, while the media clock anchors to the frame after
/// it, so the media clock always ran ahead: the cue's advance named the
/// next frame on every pass, wall time never reached it, and the cut
/// never came while the clip played. The media clock now stops before a
/// waiting cue's frame, as it does before a playlist boundary, so the
/// cue clock fires the cue on exactly its due frame.
#[test]
#[allow(clippy::too_many_lines)]
fn the_media_clock_never_renders_past_a_waiting_cue_frame() {
    use fm_protocol::{
        CueListSpec, CueOperation, TransitionFavoriteEffect, WireCue, WireCueAction,
        WireCueMediaOperation,
    };
    use playlist_clock::Pacing;

    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mov".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    attach_synthetic_media(&mut control, durable.project()).unwrap();
    let server = test_server(&control);
    let rate = durable.project().settings().frame_rate;
    let frame = Duration::from_millis(40);
    let mut cues = cue_clock::CueClock::default();
    let mut media = media_clock::MediaClock::default();
    let start = Instant::now();
    // One idle pass before the GO, as the session loop runs.
    assert_eq!(
        cues.next(&mut control, Pacing::Wall { now: start, rate }, || true, 0)
            .unwrap(),
        None
    );
    media.poll(&mut control, &server, rate, start).unwrap();

    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Roll".into(),
            auto_follow: false,
            actions: vec![
                WireCueAction::Media {
                    input: fm_types::InputTarget::Fixed(fm_protocol::WireInputId::from_domain(
                        test_input_id(3),
                    )),
                    operation: WireCueMediaOperation::Play,
                },
                WireCueAction::Wait { frames: 10 },
                WireCueAction::Take {
                    input: Some(fm_types::InputTarget::Fixed(
                        fm_protocol::WireInputId::from_domain(test_input_id(2)),
                    )),
                    effect: TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                },
            ],
        }],
    };
    for command in [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default(),
                },
            },
        ),
    ] {
        let id = command.id.clone();
        // As the session loop executes a command: realizing its frames.
        let mut durable = durable.clone();
        let delivery = execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        )
        .unwrap();
        assert!(
            delivery.submission.is_accepted(),
            "{id}: {:?}",
            delivery.submission.output.result
        );
    }
    assert!(control.media_advancing(), "the clip rolls");
    let (_, due) = control.cue_advance_frame().expect("the cue waits");

    let mut fired = None;
    for pass in 1..=50 {
        let at = start + frame * pass;
        if let Some(advance) = cues
            .next(&mut control, Pacing::Wall { now: at, rate }, || true, 0)
            .unwrap()
        {
            fired = Some(advance);
            break;
        }
        media.poll(&mut control, &server, rate, at).unwrap();
        assert!(
            control.frame_cursor() < due,
            "pass {pass}: the media clock rendered frame {due}, the cue's"
        );
    }
    let advance = fired.expect("the cue fired while the clip played");
    assert_eq!((advance.list, advance.frame), (7, due), "on its frame");
    let ran = control.submit(&operator(), advance.command(), 0).unwrap();
    assert!(ran.is_accepted(), "{:?}", ran.output.result);
}

/// A client's playlist control on the simulated runtime first catches
/// the engine up to the playlist's wall-clock frame; it must stop before
/// a waiting cue's frame as the cue clock's own catch-up does, or the
/// cue's advance comes late, on whatever frame the control left.
#[test]
fn a_playlist_catch_up_never_renders_a_waiting_cue_frame() {
    use playlist_clock::{Pacing, PlaylistClock};

    let durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let rate = durable.project().settings().frame_rate;
    for command in playlist_commands(500, fm_protocol::PlaylistStart::Now) {
        assert!(
            control
                .submit(&operator(), command, 0)
                .unwrap()
                .is_accepted()
        );
    }
    control.tick(&server).unwrap();
    let start = Instant::now();
    let mut clock = PlaylistClock::default();
    assert_eq!(
        clock
            .next(&mut control, Pacing::Wall { now: start, rate }, || true, 0)
            .unwrap(),
        None
    );
    let due = go_waiting_cue(&mut control, 5);
    let boundary = control.playlist_advance_frame().unwrap();
    assert!(boundary > due + 20, "boundary {boundary}, cue due {due}");
    let later = start + Duration::from_millis(40) * u32::try_from(due + 10).unwrap();
    clock.catch_up(&mut control, &server, rate, later).unwrap();
    assert_eq!(control.frame_cursor(), due - 1, "caught up to the cue");
    assert_eq!(control.cue_advance_frame(), Some((7, due)), "not late");
}

/// A journal that fails every record while `available` is false, as a
/// failing or full show disk does, and keeps what it recorded.
/// Each record (one command's, or a whole group's) is one attempt: one
/// append and disk sync on a real journal.
struct FlakyJournal {
    available: Cell<bool>,
    attempts: Cell<u32>,
    recorded: RefCell<Vec<CommandMessage>>,
    /// Every group commit recorded, in order.
    groups: RefCell<Vec<Vec<journal::GroupRecord>>>,
}

impl FlakyJournal {
    fn new() -> Self {
        Self {
            available: Cell::new(true),
            attempts: Cell::new(0),
            recorded: RefCell::new(Vec::new()),
            groups: RefCell::new(Vec::new()),
        }
    }
}

impl DurableStore for FlakyJournal {
    fn groups_commits(&self) -> bool {
        true
    }

    fn record_group(
        &self,
        _base: &StoredProject,
        records: &[journal::GroupRecord],
        _last: &StoredProject,
    ) -> AppResult<()> {
        self.attempts.set(self.attempts.get() + 1);
        if !self.available.get() {
            return Err(AppFailure("injected journal unavailability".into()).into());
        }
        self.recorded
            .borrow_mut()
            .extend(records.iter().map(|record| record.command.clone()));
        self.groups.borrow_mut().push(records.to_vec());
        Ok(())
    }

    fn record(
        &self,
        command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        self.attempts.set(self.attempts.get() + 1);
        if !self.available.get() {
            return Err(AppFailure("injected journal unavailability".into()).into());
        }
        self.recorded.borrow_mut().push(command.clone());
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }
}

fn group_members(commands: &[CommandMessage], server: &ServerIdentity) -> Vec<GroupMember> {
    commands
        .iter()
        .map(|command| GroupMember {
            principal: operator(),
            server: server.clone(),
            command: command.clone(),
        })
        .collect()
}

/// Five clients' commands for one pass: two renames of one input whose
/// order decides its name, a cut that expects the revision before both
/// (refused as stale, so never journaled), then a fade and a cut.
fn group_commands() -> Vec<CommandMessage> {
    let rename = |name: &str| CommandPayload::RenameInput {
        input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
        name: name.into(),
    };
    let mut stale = test_command("stale", "stale-key", CommandPayload::Cut);
    stale.expected_revision = Some(0);
    vec![
        test_command("rename-first", "rename-first-key", rename("First")),
        test_command("rename-second", "rename-second-key", rename("Second")),
        stale,
        test_command(
            "fade",
            "fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        // Accepted on the fade's second frame, which the frame loop
        // would realize next (a Cut there is refused as in flight).
        test_command("rename-third", "rename-third-key", rename("Third")),
    ]
}

/// A native group commit's members all begin on the frame the pacer
/// realizes next and realize none of their own: each runs on the state
/// the one before it left (a cut during the group's fade is refused as
/// in flight, as it would be one by one), every record names that one
/// frame, the frame is due once for the group, and replaying the
/// group's journal reproduces every member's acknowledged position and
/// the routing the group left.
#[test]
fn native_group_members_begin_on_one_frame_and_replay_exactly() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("native-group.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    // Idle frames the pacer realized before the group.
    for _ in 0..3 {
        control.borrow_mut().tick(&server).unwrap();
    }
    let frame = control.borrow().frame_cursor();
    let rename = |name: &str| CommandPayload::RenameInput {
        input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
        name: name.into(),
    };
    let commands = [
        test_command("cut-1", "cut-1-key", CommandPayload::Cut),
        test_command("cut-2", "cut-2-key", CommandPayload::Cut),
        test_command("rename", "rename-key", rename("Grouped")),
        test_command("cut-3", "cut-3-key", CommandPayload::Cut),
        test_command(
            "fade",
            "fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        test_command("in-flight", "in-flight-key", CommandPayload::Cut),
    ];
    let members = group_members(&commands, &server);
    let mut durable = manifest.clone();
    let journal = DurableJournal::open(&store).unwrap();
    let mut speculation = control.borrow().speculate();
    let recorder = GroupRecorder::new(&journal);
    let mut staged = durable.clone();
    let mut frame_due = false;
    let results: Vec<CommandResult> = members
        .iter()
        .map(|member| {
            execute_native_group_member(
                &mut speculation,
                &recorder,
                &mut staged,
                member,
                0,
                None,
                &mut frame_due,
            )
            .unwrap()
            .result
        })
        .collect();
    assert!(frame_due);
    let revisions: Vec<Option<u64>> = results
        .iter()
        .map(|result| match result {
            CommandResult::Accepted { revision, .. } => Some(*revision),
            CommandResult::Rejected { .. } => None,
        })
        .collect();
    assert_eq!(
        revisions,
        [Some(1), Some(2), Some(3), Some(4), Some(5), None],
        "{results:?}"
    );
    let records = recorder.records.take();
    assert_eq!(
        records
            .iter()
            .filter_map(journal::GroupRecord::appended_revision)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5],
        "the refused cut is only a receipt"
    );
    assert!(records.iter().all(|record| record.began_frame == frame));
    assert_eq!(
        speculation.frame_cursor(),
        frame,
        "no member realized a frame"
    );

    journal.record_group(&durable, &records, &staged).unwrap();
    let _ = control.borrow_mut().adopt(speculation);
    durable = staged;
    // The group's one frame, then the fade's rest, as the pacer
    // realizes them.
    while control.borrow().work_in_flight() || control.borrow().frame_cursor() == frame {
        control.borrow_mut().tick(&server).unwrap();
    }
    drop(journal);

    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 5);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.runtime_routing(), durable.runtime_routing());
    assert_eq!(replayed.project(), durable.project());
}

/// A native group member naming host media (here an image input's
/// picture) brings it into the bundle first, as the same command alone
/// does on either daemon and in a simulated group: it is journaled with
/// the picture's `asset://` URI, not refused as naming no project asset.
#[test]
fn a_native_group_member_imports_the_host_media_it_names() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host");
    fs::create_dir_all(&host).unwrap();
    let picture = host.join("logo.png");
    image::RgbaImage::new(2, 2).save(&picture).unwrap();
    let store = ProjectStore::new(directory.path().join("native-import.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    let input = fm_protocol::WireInputId::from_domain(test_input_id(9));
    let commands = [test_command(
        "add-logo",
        "add-logo-key",
        CommandPayload::AddInput {
            input,
            name: "Logo".into(),
            source: fm_protocol::LiveInputSource::Image(Box::new(
                fm_types::still::ImageInputConfig::new(
                    picture.to_string_lossy(),
                    fm_types::OutputScaling::Fill,
                ),
            )),
        },
    )];
    let members = group_members(&commands, &server);
    let journal = DurableJournal::open(&store).unwrap();
    let mut speculation = control.borrow().speculate();
    let recorder = GroupRecorder::new(&journal);
    let mut staged = manifest.clone();
    let mut frame_due = false;
    let result = execute_native_group_member(
        &mut speculation,
        &recorder,
        &mut staged,
        &members[0],
        0,
        None,
        &mut frame_due,
    )
    .unwrap()
    .result;
    assert!(
        matches!(result, CommandResult::Accepted { revision: 1, .. }),
        "{result:?}"
    );
    let added = staged
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == test_input_id(9))
        .expect("the image input");
    assert_eq!(added.kind.file_asset_uris(), ["asset://logo.png"]);
    assert!(store.assets_root().join("logo.png").is_file());
    let records = recorder.records.take();
    let CommandPayload::AddInput {
        source: fm_protocol::LiveInputSource::Image(image),
        ..
    } = &records[0].command.payload
    else {
        panic!("{:?}", records[0].command);
    };
    assert_eq!(image.asset_uri, "asset://logo.png");
}

/// The receipts of accepted commands: a refusal's receipt rides the
/// next checkpoint, so a crash before it may lose it by design.
fn accepted_receipts(project: &StoredProject) -> Vec<&fm_persistence::IdempotencyReceipt> {
    project
        .idempotency_receipts()
        .iter()
        .filter(|receipt| {
            matches!(
                receipt.outcome(),
                fm_persistence::ReceiptOutcome::Accepted { .. }
            )
        })
        .collect()
}

fn durable_revisions(subscription: &fm_control::Subscription) -> Vec<u64> {
    std::iter::from_fn(|| subscription.try_recv().ok())
        .filter_map(|event| match event {
            fm_control::LiveEvent::Durable(event) => Some(event.cursor.revision),
            fm_control::LiveEvent::Runtime(_) => None,
        })
        .collect()
}

/// Several clients' commands in one pass become durable with one
/// append, in the order the pass took them: each executes against the
/// state the one before it left, exactly as one at a time, the refused
/// one is not journaled, and the live service and its subscribers see
/// the group only after its append.
#[test]
fn a_group_commit_records_several_clients_commands_with_one_append_in_order() {
    let commands = group_commands();
    let one_at_a_time = FlakyJournal::new();
    let mut sequential = test_project();
    let sequential_control: SharedControl = Rc::new(RefCell::new(test_control(&sequential)));
    let server = test_server(&sequential_control.borrow());
    let expected: Vec<CommandResult> = commands
        .iter()
        .map(|command| {
            execute_authority_command(
                &sequential_control,
                &one_at_a_time,
                &mut sequential,
                &operator(),
                &server,
                command,
                0,
                None,
                None,
            )
            .unwrap()
            .result
        })
        .collect();
    assert_eq!(one_at_a_time.attempts.get(), 5, "one append per command");

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let subscription = control.borrow_mut().subscribe().unwrap();
    let mut admitted_at = Vec::new();
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &group_members(&commands, &server),
        None,
        None,
        |member, speculation, _, _| {
            admitted_at.push((member, speculation.diagnostics().current_revision));
            Ok(None)
        },
    )
    .unwrap();

    assert_eq!(store.attempts.get(), 1, "one append for the whole group");
    assert_eq!(
        admitted_at,
        [(0, 0), (1, 1), (2, 2), (3, 2), (4, 3)],
        "each member runs against the state the one before it left"
    );
    let results: Vec<CommandResult> = outcomes
        .into_iter()
        .map(|outcome| {
            assert!(outcome.admitted);
            outcome.delivery.result
        })
        .collect();
    assert_eq!(results, expected);
    assert!(matches!(&results[2], CommandResult::Rejected { .. }));
    let groups = store.groups.borrow();
    let [group] = groups.as_slice() else {
        panic!("one group expected, got {}", groups.len());
    };
    let journaled: Vec<(&str, Option<u64>)> = group
        .iter()
        .map(|record| (record.command.id.as_str(), record.appended_revision()))
        .collect();
    assert_eq!(
        journaled,
        [
            ("rename-first", Some(1)),
            ("rename-second", Some(2)),
            ("stale", None),
            ("fade", Some(3)),
            ("rename-third", Some(4)),
        ]
    );
    assert_eq!(durable.position(), sequential.position());
    assert_eq!(durable.project(), sequential.project());
    assert_eq!(durable.project().inputs()[0].name, "Third");
    assert_eq!(control.borrow().diagnostics().current_revision, 4);
    assert_eq!(durable_revisions(&subscription), [1, 2, 3, 4]);
}

/// The group's append fails: no command of it applies (no revision, no
/// receipt, no event), every client is refused as not durable and may
/// retry, and the retried group runs afresh.
#[test]
fn a_group_commit_whose_append_fails_applies_none_of_its_commands() {
    let store = FlakyJournal::new();
    store.available.set(false);
    let mut durable = test_project();
    let before = durable.clone();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let subscription = control.borrow_mut().subscribe().unwrap();
    let members = group_members(&group_commands(), &server);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |_, _, _, _| Ok(None),
    )
    .unwrap();

    assert_eq!(store.attempts.get(), 1);
    assert_eq!(outcomes.len(), 5);
    for (outcome, member) in outcomes.iter().zip(&members) {
        assert!(
            matches!(
                &outcome.delivery.result,
                CommandResult::Rejected {
                    id,
                    code,
                    retryable: true,
                    current_revision: 0,
                    reason: Some(fm_protocol::RefusalReason::NotDurable),
                    ..
                } if *id == member.command.id && code == "unavailable"
            ),
            "{:?}",
            outcome.delivery.result
        );
    }
    assert_eq!(durable, before);
    assert_eq!(control.borrow().diagnostics().current_revision, 0);
    assert_eq!(durable_revisions(&subscription), Vec::<u64>::new());
    assert!(store.groups.borrow().is_empty());

    // Nothing was kept, receipts included: the retried group executes.
    store.available.set(true);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |_, _, _, _| Ok(None),
    )
    .unwrap();
    let revisions: Vec<Option<u64>> = outcomes
        .iter()
        .map(|outcome| match &outcome.delivery.result {
            CommandResult::Accepted { revision, .. } => Some(*revision),
            CommandResult::Rejected { .. } => None,
        })
        .collect();
    assert_eq!(revisions, [Some(1), Some(2), None, Some(3), Some(4)]);
    assert_eq!(store.attempts.get(), 2);
    assert_eq!(durable_revisions(&subscription), [1, 2, 3, 4]);
}

/// A group whose append fails answers only what the failure undid as not
/// durable. A retried command whose acceptance was durable before the
/// group is answered from its receipt exactly as it would be alone (it
/// was acknowledged once and must never be denied afterwards), and a
/// session's refusal names the revision the daemon is really at, not
/// one the dropped group reached.
#[test]
fn a_failed_group_commit_keeps_answers_that_did_not_depend_on_it() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let earlier = test_command("earlier", "earlier-key", CommandPayload::Cut);
    let alone = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &operator(),
        &server,
        &earlier,
        0,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        alone.result,
        CommandResult::Accepted { revision: 1, .. }
    ));

    store.available.set(false);
    let fresh = test_command("fresh", "fresh-key", CommandPayload::Cut);
    let limited = test_command("limited", "limited-key", CommandPayload::Cut);
    let members = group_members(&[fresh, earlier.clone(), limited.clone()], &server);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |member, speculation, _, _| {
            Ok((member == 2).then(|| {
                session_rejection(
                    &limited,
                    &SessionError::InboundRateLimited,
                    speculation.diagnostics().current_revision,
                )
            }))
        },
    )
    .unwrap();

    assert!(
        matches!(
            &outcomes[0].delivery.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::NotDurable),
                current_revision: 1,
                ..
            }
        ),
        "{:?}",
        outcomes[0].delivery.result
    );
    assert!(
        matches!(
            &outcomes[1].delivery.result,
            CommandResult::Accepted { id, revision: 1, .. } if *id == earlier.id
        ),
        "{:?}",
        outcomes[1].delivery.result
    );
    assert!(
        matches!(
            &outcomes[2].delivery.result,
            CommandResult::Rejected {
                current_revision: 1,
                ..
            }
        ),
        "{:?}",
        outcomes[2].delivery.result
    );
    assert_eq!(control.borrow().diagnostics().current_revision, 1);
}

/// A crash while a group's append was being written: the log holds the
/// command before the group and a torn tail of the group. Recovery lands
/// exactly on the state before the group, none of it partially applied;
/// with the group's commit whole, it lands exactly on the state after
/// the group.
#[test]
fn a_group_commit_torn_by_a_crash_recovers_whole_or_not_at_all() {
    for torn in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        store.save(&test_project()).unwrap();
        let mut durable = load_and_recover(&store, false).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
        let server = test_server(&control.borrow());
        let alone = execute_authority_command(
            &control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "alone",
                "alone-key",
                CommandPayload::Fade { duration_frames: 4 },
            ),
            0,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            alone.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        // The frame loop realizes the rest of the fade before the group.
        while control.borrow().work_in_flight() {
            control.borrow_mut().tick(&server).unwrap();
        }
        let before_group = durable.clone();
        let log = store.journal_path().join("journal.db-wal");
        let log_before = std::fs::metadata(&log).unwrap().len();
        let outcomes = execute_command_group(
            &control,
            &journal,
            &mut durable,
            &group_members(&group_commands(), &server),
            None,
            None,
            |_, _, _, _| Ok(None),
        )
        .unwrap();
        assert!(matches!(
            outcomes[4].delivery.result,
            CommandResult::Accepted { revision: 5, .. }
        ));
        // Killed: no checkpoint, no clean stop.
        drop(journal);
        let log_after = std::fs::metadata(&log).unwrap().len();
        assert!(log_after > log_before, "the group was appended to the log");
        if torn {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(log_after - 7)
                .unwrap();
        }

        let recovered = load_and_recover(&store, false).unwrap();
        let expected = if torn { &before_group } else { &durable };
        assert_eq!(recovered.position(), expected.position(), "torn: {torn}");
        assert_eq!(recovered.project(), expected.project(), "torn: {torn}");
        assert_eq!(
            accepted_receipts(&recovered),
            accepted_receipts(expected),
            "torn: {torn}"
        );
        assert_eq!(
            accepted_receipts(&recovered).len(),
            if torn { 1 } else { 5 }
        );
    }
}

/// The daemon's clock pass on the simulated runtime, through durable
/// execution: a waiting cue's advance the journal could not record
/// (refused, retryable, nothing committed) is retried on the next frame
/// with no other command, and fires once the journal records again. The
/// accepted retry is the only advance journaled, for the cue's own frame.
#[test]
#[allow(clippy::too_many_lines)]
fn cue_clock_poll_retries_an_advance_the_journal_could_not_record() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    for command in waiting_cue_commands(5) {
        let delivery = execute_authority_command(
            &control,
            &store,
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{:?}",
            delivery.result
        );
    }
    let revision = control.borrow().diagnostics().current_revision;
    let (_, due) = control.borrow().cue_advance_frame().unwrap();
    let cursor = control.borrow().frame_cursor();
    let frame = Duration::from_millis(40);
    let mut clock = cue_clock::CueClock::default();
    let mut poll = |clock: &mut cue_clock::CueClock, at: Instant, window: bool| {
        clock
            .poll(
                &control,
                &store,
                &mut durable,
                &server,
                None,
                None,
                at,
                window,
            )
            .unwrap();
    };
    let start = Instant::now();
    poll(&mut clock, start, true);
    assert_eq!(store.attempts.get(), 2, "not due yet");

    store.available.set(false);
    let reached = start + frame * u32::try_from(due - cursor).unwrap();
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "the first attempt was refused");
    assert_eq!(control.borrow().diagnostics().current_revision, revision);
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "not again on the same frame");
    poll(&mut clock, reached + frame, true);
    assert_eq!(store.attempts.get(), 4, "retried on the next frame");

    store.available.set(true);
    poll(&mut clock, reached + frame * 2, false);
    assert_eq!(store.attempts.get(), 4, "no retry without its window");
    assert_eq!(control.borrow().cue_advance_frame(), Some((7, due)));
    poll(&mut clock, reached + frame * 2, true);
    assert_eq!(
        control.borrow().diagnostics().current_revision,
        revision + 1
    );
    assert_eq!(control.borrow().cue_advance_frame(), None, "the cue ran");
    assert_eq!(durable.position().revision, revision + 1);
    let recorded = store.recorded.borrow();
    assert_eq!(recorded.len(), 3, "only the accepted retry is journaled");
    let advance = recorded.last().unwrap();
    assert_eq!(
        advance.payload,
        CommandPayload::CueAdvance {
            list: 7,
            frame: due
        }
    );
    assert_eq!(
        advance.idempotency_key,
        format!("cue-clock-{revision}-7-{due}-retry-2")
    );
}

/// The bug this guards: a cue advance refused while a playlist take was
/// held for a later frame stalled the waiting cue until some other
/// command changed the revision, although the hold clears by itself.
/// The clock now retries on the next frames: each retry is prepared and
/// dropped while the refusal stands (no receipt, no revision), and the
/// first one the show accepts fires the cue, late, on the frame it names.
#[test]
#[allow(clippy::too_many_lines)]
fn cue_clock_retries_an_advance_refused_by_a_held_playlist_take() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use cue_clock::CueClock;
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use playlist_clock::Pacing;

    let mut control = test_control(&test_project());
    let principal = operator();
    let server = test_server(&control);
    for (id, payload) in [
        (
            "set-playlist",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::Off,
                entries: [(1, 2), (2, 1)]
                    .map(|(id, input)| PlaylistEntrySpec {
                        id,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(input)),
                        duration_frames: 80,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        enabled: true,
                    })
                    .to_vec(),
            },
        ),
        (
            "start",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Start {
                    at: PlaylistStart::Now,
                    from: None,
                },
            },
        ),
    ] {
        let submitted = control
            .submit(
                &principal,
                test_command(id, &format!("{id}-key"), payload),
                0,
            )
            .unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    control.tick(&server).unwrap();
    let due = go_waiting_cue(&mut control, 5);
    let boundary = control
        .playlist_advance_frame()
        .expect("a boundary is pending");
    assert!(boundary > due + 40, "boundary {boundary}, cue due {due}");

    let mut clock = CueClock::default();
    while control.frame_cursor() < due {
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || true, 0)
                .unwrap(),
            None
        );
        control.tick(&server).unwrap();
    }
    // The playlist's take is accepted ahead, for its boundary, so the
    // show holds every other command until then.
    let take = control
        .submit(
            &principal,
            playlist_clock::advance_command(boundary, control.diagnostics().current_revision),
            0,
        )
        .unwrap();
    assert!(take.is_accepted(), "{:?}", take.output.result);
    let revision = control.diagnostics().current_revision;
    // A first attempt keeps its frame: it never asks for the window a
    // retry waits for (a client command's, under native pacing).
    let advance = clock
        .next(
            &mut control,
            Pacing::Engine,
            || panic!("a first attempt asked for a retry window"),
            0,
        )
        .unwrap()
        .expect("the wait ended");
    assert_eq!(
        (advance.list, advance.revision, advance.retry),
        (7, revision, 0)
    );
    let refused = control.submit(&principal, advance.command(), 0).unwrap();
    assert!(
        matches!(
            &refused.output.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::PlaylistTakeScheduled { frame }),
                ..
            } if *frame == boundary
        ),
        "{:?}",
        refused.output.result
    );
    clock.observe(&advance, &refused.output.result);
    // Whether the show holds a receipt for the clock's attempt `retry`
    // at `frame`: preparing the same key again replays it.
    let has_receipt = |control: &mut ControlService<Policy>, frame: u64, retry: u32| {
        let mut command = cue_clock::advance_command(7, frame, revision);
        if retry > 0 {
            command.idempotency_key = format!("{}-retry-{retry}", command.idempotency_key);
        }
        matches!(
            control.prepare_submit(&operator(), command, 0).unwrap(),
            PrepareSubmitOutcome::Replayed(_)
        )
    };
    assert!(has_receipt(&mut control, advance.frame, 0));
    // Not again on the same frame.
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );

    // Every frame until the hold clears, the retry is prepared, still
    // refused, and dropped: no receipt and no revision. A pass without a
    // retry window (a session command waiting) does not try at all.
    let mut probes = 0_u32;
    let retried = loop {
        assert!(
            control.frame_cursor() <= boundary + SLOW_RETRY_FRAMES + 1,
            "never retried"
        );
        control.tick(&server).unwrap();
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || false, 0)
                .unwrap(),
            None,
            "a retry ran without its window"
        );
        let attempting = clock.due(&control, Pacing::Engine).is_some();
        if let Some(retried) = clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap()
        {
            break retried;
        }
        assert_eq!(control.diagnostics().current_revision, revision);
        if attempting {
            probes += 1;
            let (_, frame) = control.cue_advance_frame().unwrap();
            assert!(!has_receipt(&mut control, frame, probes), "probe {probes}");
        }
    };
    assert!(control.frame_cursor() >= boundary, "the hold cleared first");
    // The fast retries ran out while the take was held, so the clock
    // backed off: far fewer probes than held frames.
    assert!(probes > FAST_RETRIES, "{probes} probes");
    assert!(
        u64::from(probes) < boundary - due,
        "{probes} probes over {} held frames",
        boundary - due
    );
    assert_eq!((retried.list, retried.revision), (7, revision));
    assert_eq!(retried.retry, probes + 1);
    // Late: the retry names the next frame, exactly as replay finds it.
    assert_eq!(control.cue_advance_frame(), Some((7, retried.frame)));
    assert!(retried.frame > boundary);
    let fired = control.submit(&principal, retried.command(), 0).unwrap();
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    clock.observe(&retried, &fired.output.result);
    control.tick(&server).unwrap();
    control.tick(&server).unwrap();
    assert_eq!(control.cue_advance_frame(), None, "the cue ran");
    assert_eq!(
        live_engine_snapshot(&mut control)
            .realized_switcher()
            .program(),
        test_input_id(1),
        "its cut is on Program"
    );
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );
}

/// A refusal that cannot clear by itself is not retried at the same
/// revision; a retryable one is retried on every frame at first and then
/// every `SLOW_RETRY_FRAMES`; another accepted command (a new revision)
/// makes the next advance a first attempt again.
#[test]
fn cue_clock_backs_off_retries_and_waits_for_a_new_revision_after_a_final_refusal() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use cue_clock::CueClock;
    use playlist_clock::Pacing;

    let refusal = |code: &str, retryable: bool| CommandResult::Rejected {
        id: "advance".into(),
        code: code.into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: 0,
        retryable,
        reason: None,
    };
    let mut control = test_control(&test_project());
    let server = test_server(&control);
    let due = go_waiting_cue(&mut control, 3);
    while control.frame_cursor() < due {
        control.tick(&server).unwrap();
    }
    let mut clock = CueClock::default();

    // Final: nothing until the revision changes.
    let first = clock
        .next(&mut control, Pacing::Engine, || true, 0)
        .unwrap()
        .unwrap();
    assert_eq!(first.retry, 0);
    clock.observe(&first, &refusal("invalid_command", false));
    for _ in 0..3 * SLOW_RETRY_FRAMES {
        control.tick(&server).unwrap();
        assert_eq!(clock.due(&control, Pacing::Engine), None);
    }
    let cut = control
        .submit(
            &operator(),
            test_command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(cut.is_accepted());
    let fresh = clock.due(&control, Pacing::Engine).expect("a new revision");
    assert_eq!((fresh.retry, fresh.revision), (0, first.revision + 1));

    // Retryable: every frame for the fast retries, then backed off.
    let fast = usize::try_from(FAST_RETRIES).unwrap();
    let mut attempt = fresh;
    let mut gaps = Vec::new();
    let mut last = control.frame_cursor();
    while gaps.len() < fast + 3 {
        clock.observe(&attempt, &refusal("unavailable", true));
        attempt = loop {
            control.tick(&server).unwrap();
            if let Some(next) = clock.due(&control, Pacing::Engine) {
                break next;
            }
        };
        gaps.push(control.frame_cursor() - last);
        last = control.frame_cursor();
        assert_eq!(attempt.retry, u32::try_from(gaps.len()).unwrap());
    }
    assert!(gaps[..fast].iter().all(|gap| *gap == 1), "{gaps:?}");
    assert!(
        gaps[fast..].iter().all(|gap| *gap == SLOW_RETRY_FRAMES),
        "{gaps:?}"
    );
}

/// The commands that set a two-entry playlist (input 2 for `frames`
/// frames, then input 1 for `frames`, cut) and start it `at`.
fn playlist_commands(frames: u64, at: fm_protocol::PlaylistStart) -> [CommandMessage; 2] {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistTransitionKind,
    };
    [
        test_command(
            "set-playlist",
            "set-playlist-key",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::Off,
                entries: [(1, 2), (2, 1)]
                    .map(|(id, input)| PlaylistEntrySpec {
                        id,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(input)),
                        duration_frames: frames,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        enabled: true,
                    })
                    .to_vec(),
            },
        ),
        test_command(
            "start",
            "start-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Start { at, from: None },
            },
        ),
    ]
}

/// Executes `commands` through durable execution, each accepted.
fn execute_accepted(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    server: &ServerIdentity,
    commands: impl IntoIterator<Item = CommandMessage>,
) {
    for command in commands {
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &operator(),
            server,
            &command,
            0,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{}: {:?}",
            command.id,
            delivery.result
        );
    }
}

/// The playlist clock's pass on the simulated runtime, through durable
/// execution: a boundary's advance the journal could not record
/// (refused, retryable, nothing committed) is retried on the next frame
/// with no other command, and takes the entry once the journal records
/// again. The accepted retry is the only advance journaled, for the
/// boundary's own frame.
#[test]
fn playlist_clock_poll_retries_an_advance_the_journal_could_not_record() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(80, fm_protocol::PlaylistStart::AfterFrames(5)),
    );
    let revision = control.borrow().diagnostics().current_revision;
    let due = control.borrow().playlist_advance_frame().unwrap();
    let cursor = control.borrow().frame_cursor();
    let frame = Duration::from_millis(40);
    let mut clock = playlist_clock::PlaylistClock::default();
    let mut poll = |clock: &mut playlist_clock::PlaylistClock, at: Instant, window: bool| {
        clock
            .poll(
                &control,
                &store,
                &mut durable,
                &server,
                None,
                None,
                at,
                window,
            )
            .unwrap();
    };
    let start = Instant::now();
    poll(&mut clock, start, true);
    assert_eq!(store.attempts.get(), 2, "not due yet");

    store.available.set(false);
    let reached = start + frame * u32::try_from(due - cursor).unwrap();
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "the first attempt was refused");
    assert_eq!(control.borrow().diagnostics().current_revision, revision);
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "not again on the same frame");
    poll(&mut clock, reached + frame, true);
    assert_eq!(store.attempts.get(), 4, "retried on the next frame");

    store.available.set(true);
    poll(&mut clock, reached + frame * 2, false);
    assert_eq!(store.attempts.get(), 4, "no retry without its window");
    assert_eq!(control.borrow().playlist_advance_frame(), Some(due));
    poll(&mut clock, reached + frame * 2, true);
    assert_eq!(
        control.borrow().diagnostics().current_revision,
        revision + 1
    );
    assert!(matches!(
        control.borrow().playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, .. }
    ));
    assert_eq!(durable.position().revision, revision + 1);
    let recorded = store.recorded.borrow();
    assert_eq!(recorded.len(), 3, "only the accepted retry is journaled");
    let advance = recorded.last().unwrap();
    assert_eq!(
        advance.payload,
        CommandPayload::PlaylistAdvance { frame: due }
    );
    assert_eq!(
        advance.idempotency_key,
        format!("playlist-clock-{revision}-{due}-retry-2")
    );
}

/// The bug this guards: a playlist advance refused while a cue's frame
/// was held for later stalled the playlist on an idle show until some
/// other command changed the revision, although the hold clears by
/// itself. The clock now retries on the next frames: each retry is
/// prepared and dropped while the refusal stands (no receipt, no
/// revision), and the first one the show accepts takes the entry, late,
/// on the frame it names.
#[test]
#[allow(clippy::too_many_lines)]
fn playlist_clock_retries_an_advance_refused_by_a_held_cue_frame() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use playlist_clock::{Pacing, PlaylistClock};

    let mut control = test_control(&test_project());
    let principal = operator();
    let server = test_server(&control);
    for command in playlist_commands(200, fm_protocol::PlaylistStart::AfterFrames(5)) {
        let id = command.id.clone();
        let submitted = control.submit(&principal, command, 0).unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    let boundary = control
        .playlist_advance_frame()
        .expect("the start is armed");
    let due = go_waiting_cue(&mut control, 80);
    assert!(due > boundary + 40, "cue due {due}, boundary {boundary}");
    // The cue's advance is accepted ahead, for its frame, so the show
    // holds every other command until then.
    let held = control
        .submit(
            &principal,
            cue_clock::advance_command(7, due, control.diagnostics().current_revision),
            0,
        )
        .unwrap();
    assert!(held.is_accepted(), "{:?}", held.output.result);
    let revision = control.diagnostics().current_revision;

    let mut clock = PlaylistClock::default();
    while control.frame_cursor() < boundary {
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || true, 0)
                .unwrap(),
            None
        );
        control.tick(&server).unwrap();
    }
    // A first attempt keeps its frame: it never asks for the window a
    // retry waits for (a client command's, under native pacing).
    let advance = clock
        .next(
            &mut control,
            Pacing::Engine,
            || panic!("a first attempt asked for a retry window"),
            0,
        )
        .unwrap()
        .expect("the boundary is reached");
    assert_eq!(
        (advance.frame, advance.revision, advance.retry),
        (boundary, revision, 0)
    );
    let refused = control.submit(&principal, advance.command(), 0).unwrap();
    assert!(
        matches!(
            &refused.output.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::CueScheduled { frame }),
                ..
            } if *frame == due
        ),
        "{:?}",
        refused.output.result
    );
    clock.observe(&advance, &refused.output.result);
    // Whether the show holds a receipt for the clock's attempt `retry`
    // at `frame`: preparing the same key again replays it.
    let has_receipt = |control: &mut ControlService<Policy>, frame: u64, retry: u32| {
        let mut command = playlist_clock::advance_command(frame, revision);
        if retry > 0 {
            command.idempotency_key = format!("{}-retry-{retry}", command.idempotency_key);
        }
        matches!(
            control.prepare_submit(&operator(), command, 0).unwrap(),
            PrepareSubmitOutcome::Replayed(_)
        )
    };
    assert!(has_receipt(&mut control, advance.frame, 0));
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None,
        "not again on the same frame"
    );

    // Every frame until the hold clears, the retry is prepared, still
    // refused, and dropped: no receipt and no revision. A pass without a
    // retry window does not try at all.
    let mut probes = 0_u32;
    let retried = loop {
        assert!(
            control.frame_cursor() <= due + SLOW_RETRY_FRAMES + 1,
            "never retried"
        );
        control.tick(&server).unwrap();
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || false, 0)
                .unwrap(),
            None,
            "a retry ran without its window"
        );
        let attempting = clock.due(&control, Pacing::Engine).is_some();
        if let Some(retried) = clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap()
        {
            break retried;
        }
        assert_eq!(control.diagnostics().current_revision, revision);
        if attempting {
            probes += 1;
            let frame = control.playlist_advance_frame().unwrap();
            assert!(!has_receipt(&mut control, frame, probes), "probe {probes}");
        }
    };
    assert!(control.frame_cursor() >= due, "the hold cleared first");
    assert!(probes > FAST_RETRIES, "{probes} probes");
    assert!(
        u64::from(probes) < due - boundary,
        "{probes} probes over {} held frames",
        due - boundary
    );
    assert_eq!(retried.revision, revision);
    assert_eq!(retried.retry, probes + 1);
    // Late: the retry names the next frame, exactly as replay finds it.
    assert_eq!(control.playlist_advance_frame(), Some(retried.frame));
    assert!(retried.frame > due);
    let taken = control.submit(&principal, retried.command(), 0).unwrap();
    assert!(taken.is_accepted(), "{:?}", taken.output.result);
    clock.observe(&retried, &taken.output.result);
    control.tick(&server).unwrap();
    control.tick(&server).unwrap();
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, .. }
    ));
    assert_eq!(
        live_engine_snapshot(&mut control)
            .realized_switcher()
            .program(),
        test_input_id(2),
        "the entry's input is on Program, after the cue's cut"
    );
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );
}

/// A retryable refusal is retried on every frame at first and then
/// every `SLOW_RETRY_FRAMES`; the policy is the cue clock's.
#[test]
fn playlist_clock_backs_off_retries() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use playlist_clock::{Pacing, PlaylistClock};

    let refusal = CommandResult::Rejected {
        id: "advance".into(),
        code: "unavailable".into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: 0,
        retryable: true,
        reason: None,
    };
    let mut control = test_control(&test_project());
    let server = test_server(&control);
    for command in playlist_commands(10, fm_protocol::PlaylistStart::AfterFrames(3)) {
        assert!(
            control
                .submit(&operator(), command, 0)
                .unwrap()
                .is_accepted()
        );
    }
    let boundary = control.playlist_advance_frame().unwrap();
    while control.frame_cursor() < boundary {
        control.tick(&server).unwrap();
    }
    let mut clock = PlaylistClock::default();
    let mut attempt = clock.due(&control, Pacing::Engine).expect("due");
    assert_eq!(attempt.retry, 0);
    let fast = usize::try_from(FAST_RETRIES).unwrap();
    let mut gaps = Vec::new();
    let mut last = control.frame_cursor();
    while gaps.len() < fast + 3 {
        clock.observe(&attempt, &refusal);
        attempt = loop {
            control.tick(&server).unwrap();
            if let Some(next) = clock.due(&control, Pacing::Engine) {
                break next;
            }
        };
        gaps.push(control.frame_cursor() - last);
        last = control.frame_cursor();
        assert_eq!(attempt.retry, u32::try_from(gaps.len()).unwrap());
        // Late: each retry names the frame the show has reached.
        assert_eq!(attempt.frame, control.frame_cursor());
    }
    assert!(gaps[..fast].iter().all(|gap| *gap == 1), "{gaps:?}");
    assert!(
        gaps[fast..].iter().all(|gap| *gap == SLOW_RETRY_FRAMES),
        "{gaps:?}"
    );
}

/// A media end trigger whose `playlist_control next` the journal could
/// not record is retried, on the simulated runtime by wall time, while
/// the airing it ends is still on, and only the accepted retry is
/// journaled. A trigger whose airing an operator ended first is dropped.
/// The playlist loops, so the last entry has a next entry too.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_the_journal_could_not_record_is_retried_while_its_airing_is_on() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{PlaylistLoopMode, PlaylistOperation, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let mut commands = playlist_commands(500, fm_protocol::PlaylistStart::Now);
    if let CommandPayload::SetPlaylist { loop_mode, .. } = &mut commands[0].payload {
        *loop_mode = PlaylistLoopMode::All;
    }
    execute_accepted(&control, &store, &mut durable, &server, commands);
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 1, take, .. } = airing(&control) else {
        panic!("entry 1 airs: {:?}", airing(&control));
    };
    let trigger = MediaEndTrigger {
        input: test_input_id(2),
        frame: control.borrow().frame_cursor(),
    };
    assert_eq!(
        media_clock::airing_take(&control.borrow(), trigger),
        Some(take)
    );
    let revision = control.borrow().diagnostics().current_revision;
    let recorded_before = store.recorded.borrow().len();

    store.available.set(false);
    let first = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &development_principal().unwrap(),
        &server,
        &media_clock::end_trigger_command(trigger, revision),
        0,
        None,
        None,
    )
    .unwrap();
    assert!(
        matches!(
            first.result,
            CommandResult::Rejected {
                retryable: true,
                ..
            }
        ),
        "{:?}",
        first.result
    );
    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &first.result,
        start,
    );
    let attempts = store.attempts.get();
    let frame = Duration::from_millis(40);
    let mut poll = |feed: &mut media_clock::EndTriggerFeed, at: Instant, window: bool| {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            at,
            window,
        )
        .unwrap();
    };
    poll(&mut feed, start, true);
    assert_eq!(
        store.attempts.get(),
        attempts,
        "not again on the same frame"
    );
    poll(&mut feed, start + frame, true);
    assert_eq!(store.attempts.get(), attempts + 1, "retried a frame later");
    store.available.set(true);
    poll(&mut feed, start + frame * 2, false);
    assert_eq!(
        store.attempts.get(),
        attempts + 1,
        "no retry without its window"
    );
    poll(&mut feed, start + frame * 2, true);
    assert_eq!(store.attempts.get(), attempts + 2);
    poll(&mut feed, start + frame * 40, true);
    assert_eq!(store.attempts.get(), attempts + 2, "fed once");
    assert!(
        matches!(airing(&control), PlaylistRunState::Playing { entry: 2, .. }),
        "{:?}",
        airing(&control)
    );
    {
        let recorded = store.recorded.borrow();
        assert_eq!(recorded.len(), recorded_before + 1);
        let next = recorded.last().unwrap();
        assert_eq!(
            next.payload,
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Next
            }
        );
        assert_eq!(
            next.idempotency_key,
            format!(
                "media-end-{}-{}-r{revision}-retry-2",
                trigger.input, trigger.frame
            )
        );
    }

    // Entry 2's end trigger is refused too, but the operator stops the
    // playlist before a retry: the trigger no longer applies.
    let PlaylistRunState::Playing { take, .. } = airing(&control) else {
        unreachable!()
    };
    let trigger = MediaEndTrigger {
        input: test_input_id(1),
        frame: control.borrow().frame_cursor(),
    };
    let revision = control.borrow().diagnostics().current_revision;
    let refused = CommandResult::Rejected {
        id: "next".into(),
        code: "conflict".into(),
        message: "held".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    };
    let later = start + frame * 50;
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &refused,
        later,
    );
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [test_command(
            "stop",
            "stop-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Stop,
            },
        )],
    );
    let attempts = store.attempts.get();
    for passes in 1..=3 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            later + frame * passes,
            true,
        )
        .unwrap();
    }
    assert_eq!(store.attempts.get(), attempts, "dropped with its airing");
    assert!(matches!(airing(&control), PlaylistRunState::Stopped));
}

/// A refused end trigger is owed only while its clip is still at its
/// end: an operator who seeks the clip back while the playlist still airs
/// the same take has it playing again, and the retry is dropped instead
/// of cutting away from it. Left at its end, the same refusal is retried
/// and advances the playlist.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_retry_is_dropped_once_its_clip_is_sought_back() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{MediaOperation, MediaSeekTarget, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = media_test_project("asset://clip.mov");
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    attach_synthetic_media(&mut control.borrow_mut(), durable.project()).unwrap();
    let server = test_server(&control.borrow());
    // Entry 1 airs media input 2; entry 2 (media input 1) is next.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(5_000, fm_protocol::PlaylistStart::Now),
    );
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 1, take, .. } = airing(&control) else {
        panic!("entry 1 airs: {:?}", airing(&control));
    };
    let clip = test_input_id(2);
    let end = control
        .borrow()
        .media_status(clip)
        .expect("a media input")
        .playable_end;
    let media = |id: &str, operation: MediaOperation| {
        test_command(
            id,
            &format!("{id}-key"),
            CommandPayload::Media {
                input: fm_protocol::WireInputId::from_domain(clip),
                operation,
            },
        )
    };
    // The clip paused inside its 20-frame end-trigger lead.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [
            media("lead", MediaOperation::EndTriggerLead(Some(20))),
            media(
                "cue-end",
                MediaOperation::Seek(MediaSeekTarget::Frame(end - 8)),
            ),
            media("play", MediaOperation::Play),
            media("pause", MediaOperation::Pause),
        ],
    );
    control.borrow_mut().take_media_end_triggers();
    assert!(media_clock::end_reached(&control.borrow(), clip));

    let refused = |revision: u64| CommandResult::Rejected {
        id: "next".into(),
        code: "conflict".into(),
        message: "held".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    };
    let trigger = MediaEndTrigger {
        input: clip,
        frame: control.borrow().frame_cursor(),
    };
    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let frame = Duration::from_millis(40);
    let observe_refusal = |feed: &mut media_clock::EndTriggerFeed, at: Instant| {
        let control = control.borrow();
        let revision = control.diagnostics().current_revision;
        let cursor = control.frame_cursor();
        feed.observe_first(
            &control,
            trigger,
            take,
            revision,
            cursor,
            &refused(revision),
            at,
        );
    };
    observe_refusal(&mut feed, start);
    assert!(feed.pending());

    // The operator rewinds the clip: it plays again, so no cut.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [media(
            "rewind",
            MediaOperation::Seek(MediaSeekTarget::Frame(0)),
        )],
    );
    assert!(!media_clock::end_reached(&control.borrow(), clip));
    let attempts = store.attempts.get();
    for pass in 1..=30 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            start + frame * pass,
            true,
        )
        .unwrap();
    }
    assert!(!feed.pending(), "dropped once the clip played again");
    assert_eq!(store.attempts.get(), attempts, "nothing executed");
    assert!(
        matches!(
            airing(&control),
            PlaylistRunState::Playing { entry: 1, take: now, .. } if now == take
        ),
        "{:?}",
        airing(&control)
    );

    // At its end again, the same refusal is retried and cuts.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [media(
            "cue-end-again",
            MediaOperation::Seek(MediaSeekTarget::Frame(end - 8)),
        )],
    );
    control.borrow_mut().take_media_end_triggers();
    assert!(media_clock::end_reached(&control.borrow(), clip));
    let later = start + frame * 40;
    observe_refusal(&mut feed, later);
    assert!(feed.pending());
    feed.poll(
        &control,
        &store,
        &mut durable,
        &server,
        None,
        None,
        later + frame,
        true,
    )
    .unwrap();
    assert!(!feed.pending());
    assert!(
        matches!(airing(&control), PlaylistRunState::Playing { entry: 2, .. }),
        "{:?}",
        airing(&control)
    );
}

/// The last entry of a playlist that does not loop has no next entry:
/// the engine refuses its end trigger's `playlist_control next` as a
/// conflict (with no reason that tells it apart from a passing one), and
/// the feed takes the playlist state's word that it cannot clear. The
/// refusal is final, so no dry-run repeats every second while the clip
/// holds, and nothing is cut later.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_with_no_next_entry_is_final() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{PlaylistOperation, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(500, fm_protocol::PlaylistStart::Now),
    );
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [test_command(
            "next",
            "next-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Next,
            },
        )],
    );
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 2, take, .. } = airing(&control) else {
        panic!("entry 2 airs: {:?}", airing(&control));
    };
    assert!(!media_clock::has_next_entry(&control.borrow()));
    let trigger = MediaEndTrigger {
        input: test_input_id(1),
        frame: control.borrow().frame_cursor(),
    };
    let revision = control.borrow().diagnostics().current_revision;
    let first = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &development_principal().unwrap(),
        &server,
        &media_clock::end_trigger_command(trigger, revision),
        0,
        None,
        None,
    )
    .unwrap();
    let CommandResult::Rejected {
        code,
        retryable,
        reason,
        ..
    } = &first.result
    else {
        panic!("no next entry: {:?}", first.result);
    };
    // What the retry policy alone would retry for as long as it airs.
    assert!(crate::clock_retry::retries(code, *retryable));
    assert_eq!(*reason, None);

    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &first.result,
        start,
    );
    assert!(!feed.pending(), "final, not retried");
    let attempts = store.attempts.get();
    for pass in 1..=60 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            start + Duration::from_millis(40) * pass,
            true,
        )
        .unwrap();
    }
    assert_eq!(store.attempts.get(), attempts);
    assert!(
        matches!(
            airing(&control),
            PlaylistRunState::Playing { entry: 2, take: now, .. } if now == take
        ),
        "{:?}",
        airing(&control)
    );
}

#[test]
fn record_commands_project_both_slot_flags_across_replay_and_checkpoint() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("record-roundtrip.freemix");
    let store = ProjectStore::new(&project_path).unwrap();
    let durable = test_project_with_recording([true, false]);
    store.save(&durable).unwrap();

    // An accepted RecordStop for slot 1 flips only that slot's durable
    // flag and stays only in the journal: a crash right now must still
    // restart with slot 1 stopped and slot 2 untouched.
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let mut durable = durable;
    {
        let journal = DurableJournal::open(&store).unwrap();
        let stopped = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "rec-stop",
                "rec-stop-key",
                CommandPayload::RecordStop {
                    recorder: wire_recorder(1),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            stopped.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        assert_eq!(durable.project().recording_desired_slots(), [false, false]);
    }
    assert_eq!(store.load().unwrap().position().revision, 0);

    // Journal replay alone reproduces the per-slot stopped state.
    let reloaded = ProjectStore::new(&project_path).unwrap();
    let manifest = reloaded.load().unwrap();
    let scan = reloaded.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 1);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position().revision, 1);
    assert_eq!(replayed.project().recording_desired_slots(), [false, false]);

    // A later RecordStart for slot 2 leaves slot 1 stopped, and both flags
    // survive a full checkpoint: the durable mirror never rewrites a slot
    // the command did not address.
    let mut control = test_control(&replayed);
    let server = test_server(&control);
    let mut durable = replayed;
    {
        let journal = DurableJournal::open(&reloaded).unwrap();
        let started = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "rec-start",
                "rec-start-key",
                CommandPayload::RecordStart {
                    recorder: wire_recorder(2),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            started.submission.output.result,
            CommandResult::Accepted { revision: 2, .. }
        ));
        assert_eq!(durable.project().recording_desired_slots(), [false, true]);
        journal.settle(&durable).unwrap();
    }
    let checkpointed = ProjectStore::new(&project_path).unwrap();
    let final_state = checkpointed.load().unwrap();
    assert_eq!(final_state.position().revision, 2);
    assert_eq!(
        checkpointed.scan_journal().unwrap().batches().len(),
        0,
        "checkpoint must have compacted the journal"
    );
    assert_eq!(
        final_state.project().recording_desired_slots(),
        [false, true]
    );
    let restored = restore_engine(&final_state).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    let desired = restored.show().desired_switcher();
    assert_eq!(
        (
            desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[0])),
            desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[1])),
        ),
        (false, true)
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_pair_ledger_advances_only_on_successful_enqueue() {
    let mut ledger = StreamPairLedger {
        next_sequence: 7,
        ..StreamPairLedger::default()
    };

    ledger.note_admitted(SequenceNumber::new(9));
    assert_eq!(ledger.enqueued_pairs, 1);
    assert_eq!(ledger.next_sequence, 10);

    // Refusals only count; they never move the admitted cursor.
    ledger.note_dropped();
    assert_eq!(ledger.dropped_pairs, 1);
    assert_eq!(ledger.next_sequence, 10);

    let mut saturated = StreamPairLedger {
        next_sequence: u64::MAX,
        enqueued_pairs: u64::MAX,
        dropped_pairs: u64::MAX,
    };
    saturated.note_admitted(SequenceNumber::new(u64::MAX));
    saturated.note_dropped();
    assert_eq!(saturated.next_sequence, u64::MAX);
    assert_eq!(saturated.enqueued_pairs, u64::MAX);
    assert_eq!(saturated.dropped_pairs, u64::MAX);
}

#[cfg(feature = "native-media")]
const MOCK_RENDITION: RenditionId = RenditionId::new(std::num::NonZeroU32::new(1).unwrap());

#[cfg(feature = "native-media")]
fn stream_runtime(
    target: u128,
    ledger: StreamPairLedger,
    congested: bool,
    first_failure: Option<OutputFailure>,
    started: bool,
    state: Option<DestinationState>,
    network: Option<NetworkTelemetry>,
) -> StreamRuntimeSnapshot {
    StreamRuntimeSnapshot {
        target: SwitcherStreamTargetId::from_non_zero(
            NonZeroU128::new(target).expect("test target is nonzero"),
        ),
        ledger,
        congested,
        first_failure,
        started,
        state,
        network,
        local: None,
        uptime_ms: 0,
        bitrate_kbps: 0,
    }
}

#[cfg(feature = "native-media")]
fn local_stream_runtime(target: u128, local: StreamLocalStatus) -> StreamRuntimeSnapshot {
    StreamRuntimeSnapshot {
        target: SwitcherStreamTargetId::from_non_zero(
            NonZeroU128::new(target).expect("test target is nonzero"),
        ),
        ledger: StreamPairLedger::default(),
        congested: false,
        first_failure: None,
        started: true,
        state: Some(DestinationState::Live),
        network: None,
        local: Some(local),
        uptime_ms: 0,
        bitrate_kbps: 0,
    }
}

#[cfg(feature = "native-media")]
#[test]
fn stream_status_maps_each_reachable_output_set_state() {
    let sample = |congested, state, network| {
        stream_status_sample(stream_runtime(
            7,
            StreamPairLedger {
                enqueued_pairs: 30,
                dropped_pairs: 4,
                ..StreamPairLedger::default()
            },
            congested,
            None,
            true,
            state,
            network,
        ))
        .expect("started runtimes always sample")
    };

    // Connecting and awaiting random access both mean "starting"; the
    // reconnected route is already connected, just not yet resuming.
    let connecting = sample(false, Some(DestinationState::Connecting), None);
    assert_eq!(connecting.realized, StreamRealizedState::Starting);
    assert!(!connecting.connected);
    let awaiting = sample(false, Some(DestinationState::AwaitingRandomAccess), None);
    assert_eq!(awaiting.realized, StreamRealizedState::Starting);
    assert!(awaiting.connected);

    // Producing: counters come from the ledger and bytes from telemetry.
    let live_set = driven_live_output_set();
    let id = DestinationId::new(1).unwrap();
    let network = live_set.telemetry(id).unwrap().clone();
    let live = sample(false, Some(DestinationState::Live), Some(network.clone()));
    assert_eq!(live.realized, StreamRealizedState::Live);
    assert!(live.connected);
    assert_eq!(
        live.muxed_bytes,
        network.bytes_sent(),
        "muxed bytes are the transport's own counter"
    );
    assert_eq!(live.enqueued_pairs, 30);
    assert_eq!(live.dropped_pairs, 4);
    assert_eq!(live.failure, None);

    // Backpressure observed this interval wins over Live.
    let congested = sample(true, Some(DestinationState::Live), Some(network));
    assert_eq!(congested.realized, StreamRealizedState::Congested);
    // The set's own congestion is congestion on the wire too.
    let transport_congested = sample(
        false,
        Some(DestinationState::Congested),
        Some(NetworkTelemetry::default()),
    );
    assert_eq!(transport_congested.realized, StreamRealizedState::Congested);

    // Waiting to reconnect is now a reachable wire state.
    let waiting = sample(
        false,
        Some(DestinationState::WaitingToReconnect {
            attempt: 2,
            retry_at_ms: 900,
        }),
        None,
    );
    assert_eq!(waiting.realized, StreamRealizedState::WaitingToReconnect);

    // Terminal states.
    let failed_by_state = sample(false, Some(DestinationState::Failed), None);
    assert_eq!(failed_by_state.realized, StreamRealizedState::Failed);
    let failing_set = driven_failing_output_set();
    let failing_network = failing_set.telemetry(id).unwrap().clone();
    let waiting_with_failure = sample(false, failing_set.state(id), Some(failing_network));
    assert_eq!(
        waiting_with_failure.realized,
        StreamRealizedState::WaitingToReconnect,
        "a recorded transient failure does not fail the target"
    );
    assert_eq!(
        waiting_with_failure.failure,
        Some(OutputFailure::new(OutputFailureCode::StreamStalled)),
        "transport failures are machine-readable codes"
    );
    let stopped = sample(false, Some(DestinationState::Stopped), None);
    assert_eq!(stopped.realized, StreamRealizedState::Stopped);
}

#[cfg(feature = "native-media")]
#[test]
fn latched_failure_is_sticky_over_live_state_and_survives_a_gone_slot() {
    let latched = Some(OutputFailure::new(OutputFailureCode::InternalError));
    // The route still streams, but the feed already latched its failure.
    let sample = stream_status_sample(stream_runtime(
        9,
        StreamPairLedger {
            enqueued_pairs: 2,
            ..StreamPairLedger::default()
        },
        false,
        latched,
        true,
        Some(DestinationState::Live),
        None,
    ))
    .expect("latched runtimes stay on the wire");
    assert_eq!(sample.realized, StreamRealizedState::Failed);
    assert_eq!(sample.failure, latched);

    // A start-time failure leaves nothing running; counters still report.
    let start_failed = stream_status_sample(stream_runtime(
        11,
        StreamPairLedger {
            dropped_pairs: 1,
            ..StreamPairLedger::default()
        },
        false,
        Some(OutputFailure::new(OutputFailureCode::FfmpegUnavailable)),
        true,
        Some(DestinationState::Stopped),
        None,
    ))
    .expect("latched runtimes stay on the wire");
    assert_eq!(start_failed.realized, StreamRealizedState::Failed);
    assert!(!start_failed.connected);
    assert_eq!(start_failed.muxed_bytes, 0);
    assert_eq!(start_failed.dropped_pairs, 1);
    assert_eq!(
        start_failed.failure,
        Some(OutputFailure::new(OutputFailureCode::FfmpegUnavailable))
    );
}

#[cfg(feature = "native-media")]
#[test]
fn inactive_targets_are_omitted_and_samples_sort_and_stay_bounded() {
    // A target that was never asked to run never appears.
    let empty = stream_status_samples([stream_runtime(
        5,
        StreamPairLedger::default(),
        false,
        None,
        false,
        Some(DestinationState::Stopped),
        None,
    )]);
    assert!(empty.is_empty(), "an all-inactive set publishes nothing");

    // Mixed set: unsorted input comes out in strictly ascending wire
    // order and inactive targets drop out of the middle.
    let latched =
        Some(OutputFailure::new(OutputFailureCode::FfmpegExited).with_exit_status(Some(1)));
    let mixed = stream_status_samples([
        stream_runtime(
            90,
            StreamPairLedger::default(),
            false,
            None,
            false,
            Some(DestinationState::Stopped),
            None,
        ),
        stream_runtime(
            40,
            StreamPairLedger {
                enqueued_pairs: 1,
                ..StreamPairLedger::default()
            },
            false,
            None,
            true,
            Some(DestinationState::Live),
            None,
        ),
        stream_runtime(
            20,
            StreamPairLedger::default(),
            false,
            latched,
            true,
            Some(DestinationState::Stopped),
            None,
        ),
        stream_runtime(
            10,
            StreamPairLedger::default(),
            false,
            None,
            false,
            None,
            None,
        ),
    ]);
    let targets: Vec<_> = mixed
        .iter()
        .map(|sample| sample.target.get().get())
        .collect();
    assert_eq!(targets, [20, 40]);

    // The bound holds even if a project ever exceeded the engine cap.
    let flooded: Vec<_> = (0..=u128::try_from(MAX_STREAM_SAMPLES).unwrap())
        .map(|index| {
            stream_runtime(
                index + 1,
                StreamPairLedger::default(),
                false,
                latched,
                true,
                Some(DestinationState::Stopped),
                None,
            )
        })
        .collect();
    assert_eq!(flooded.len(), MAX_STREAM_SAMPLES + 1);
    assert_eq!(stream_status_samples(flooded).len(), MAX_STREAM_SAMPLES);
}

/// One hand-written single-rendition media playlist with `segments` whole
/// two-second segments.
#[cfg(feature = "native-media")]
fn media_playlist(segments: u32) -> String {
    use core::fmt::Write as _;
    let mut playlist = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for index in 0..segments {
        let _ = write!(playlist, "#EXTINF:2.000000,\nsegment-{index:05}.ts\n");
    }
    playlist
}

/// One local HLS destination config over an absolute probe directory.
#[cfg(feature = "native-media")]
fn hls_destination(id: u8, directory: &str) -> DestinationConfig {
    DestinationConfig::new(
        DestinationId::new(id).unwrap(),
        OutputProtocol::Hls,
        Endpoint::local_directory(directory).unwrap(),
        None,
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::new(
            PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MULTIPLIER,
            Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_reads_a_single_rendition_playlist() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("index.m3u8"), media_playlist(3)).unwrap();
    let status = local_hls_status(directory.path()).expect("a valid playlist samples");
    assert_eq!(status.segments, 3);
    assert_eq!(status.target_duration_ms, 2_000);
    // Free space is measured on the directory itself, through a pinned
    // query: concurrent tests write to the same file system, so two
    // real queries need not agree.
    let mut queried = None;
    let pinned = local_hls_status_with(directory.path(), |ancestor| {
        queried = Some(ancestor.to_path_buf());
        Some(123_456_789)
    })
    .expect("a valid playlist samples");
    assert_eq!(queried.as_deref(), Some(directory.path()));
    assert_eq!(pinned.free_bytes, 123_456_789);
    let unavailable =
        local_hls_status_with(directory.path(), |_| None).expect("a valid playlist samples");
    assert_eq!(unavailable.free_bytes, 0, "an unavailable query is zero");
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_follows_the_first_master_variant() {
    use fm_frame::VideoDimensions;
    use fm_io_network::{HlsAbrCoordinator, HlsPlaylistType, HlsVariantMetadata};

    let directory = tempfile::tempdir().unwrap();
    let low = test_rendition(1);
    let high = test_rendition(2);
    let coordinator = HlsAbrCoordinator::new(
        vec![
            HlsVariantMetadata::new(
                low,
                "low",
                1_000_000,
                VideoDimensions::new(640, 360).unwrap(),
                "avc1.64001e,mp4a.40.2",
                "0/index.m3u8",
            )
            .unwrap(),
            HlsVariantMetadata::new(
                high,
                "high",
                2_000_000,
                VideoDimensions::new(1_280, 720).unwrap(),
                "avc1.64002a,mp4a.40.2",
                "1/index.m3u8",
            )
            .unwrap(),
        ],
        6,
        NormalizedDuration::from_nanos(2_000_000_000).unwrap(),
        0,
        6,
        HlsPlaylistType::Live,
    )
    .unwrap();
    fs::write(
        directory.path().join("index.m3u8"),
        coordinator.render_master_m3u8(),
    )
    .unwrap();

    // The first variant renders through the real playlist renderer; the
    // second carries different values to prove only the first is followed.
    let first = HlsPlaylistMetadata::parse_m3u8(&media_playlist(2), low, 1).unwrap();
    fs::create_dir(directory.path().join("0")).unwrap();
    fs::write(directory.path().join("0/index.m3u8"), first.render_m3u8()).unwrap();
    let second = HlsPlaylistMetadata::parse_m3u8(&media_playlist(5), high, 1).unwrap();
    fs::create_dir(directory.path().join("1")).unwrap();
    fs::write(directory.path().join("1/index.m3u8"), second.render_m3u8()).unwrap();

    let status = local_hls_status(directory.path()).expect("the master's first variant samples");
    assert_eq!(status.segments, 2);
    assert_eq!(status.target_duration_ms, 2_000);
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_is_none_for_missing_or_invalid_playlists() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "a missing index.m3u8 is a sample gap"
    );

    fs::write(directory.path().join("index.m3u8"), "not a playlist\n").unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "unparseable text is a sample gap"
    );

    // A segment longer than the target duration fails the bounded parser.
    fs::write(
        directory.path().join("index.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:2.000000,\nsegment-00000.ts\n",
    )
    .unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "an invalid playlist is a sample gap"
    );

    // A master whose first variant playlist is missing is also a gap.
    fs::write(
        directory.path().join("index.m3u8"),
        concat!(
            "#EXTM3U\n#EXT-X-VERSION:6\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n",
            "0/index.m3u8\n",
        ),
    )
    .unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "a missing variant is a sample gap"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_rejects_escaping_master_variants() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("hls");
    fs::create_dir(&directory).unwrap();
    // A valid playlist outside the output directory proves the escaping
    // path is refused before any read, not merely because it is missing.
    fs::write(root.path().join("escape"), media_playlist(4)).unwrap();
    for variant in ["../escape", "/absolute/index.m3u8", "0\\index.m3u8"] {
        fs::write(
            directory.join("index.m3u8"),
            format!(
                "#EXTM3U\n#EXT-X-VERSION:6\n\
                     #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n{variant}\n"
            ),
        )
        .unwrap();
        assert_eq!(local_hls_status(&directory), None, "{variant}");
    }
}

#[cfg(feature = "native-media")]
#[test]
fn cached_local_health_is_projected_only_for_hls_targets() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(hls_destination(2, "/srv/hls-probe"))
        .unwrap();
    let local = StreamLocalStatus {
        segments: 6,
        target_duration_ms: 6_000,
        free_bytes: 1_024,
    };
    let mut cache = [None; fm_switcher::MAX_STREAM_COUNT];
    cache[0] = Some(local);
    cache[1] = Some(local);
    assert_eq!(
        cached_local_hls_status(&set, &cache, DestinationId::new(1).unwrap()),
        None,
        "a network target never projects local health"
    );
    assert_eq!(
        cached_local_hls_status(&set, &cache, DestinationId::new(2).unwrap()),
        Some(local)
    );

    // The mapping carries the filtered cache entry onto the wire sample.
    let samples = stream_status_samples([
        stream_runtime(
            3,
            StreamPairLedger::default(),
            false,
            None,
            true,
            Some(DestinationState::Live),
            None,
        ),
        local_stream_runtime(4, local),
    ]);
    assert_eq!(samples[0].local, None);
    assert_eq!(samples[1].local, Some(local));
}

/// One scripted [`fm_io_network::TransportSink`] against the same public
/// trait the real `FFmpeg` transport implements. Hermetic tests drive the
/// daemon's actual output-set wiring through it: scripted connect and
/// write results, disconnect counts, and which host each attempt chose.
#[cfg(feature = "native-media")]
#[derive(Default)]
struct MockTransportSink {
    connect_results: std::collections::VecDeque<
        Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError>,
    >,
    write_results:
        std::collections::VecDeque<Result<fm_io_network::SinkWrite, fm_io_network::SinkError>>,
    hosts: Vec<String>,
    disconnects: usize,
    /// Every packet the route attempted to write, cloned on arrival, so
    /// rendition-sharing tests can compare payloads across sinks.
    written: Vec<fm_io_network::OutputPacket>,
}

#[cfg(feature = "native-media")]
impl fm_io_network::TransportSink for MockTransportSink {
    fn connect(
        &mut self,
        _config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError> {
        self.hosts.push(endpoint.host().to_owned());
        self.connect_results
            .pop_front()
            .unwrap_or_else(|| Ok(fm_io_network::ConnectionObservation::default()))
    }

    fn write(
        &mut self,
        packet: &fm_io_network::OutputPacket,
    ) -> Result<fm_io_network::SinkWrite, fm_io_network::SinkError> {
        self.written.push(packet.clone());
        self.write_results.pop_front().unwrap_or_else(|| {
            Ok(fm_io_network::SinkWrite::Sent(
                fm_io_network::SendObservation::default(),
            ))
        })
    }

    fn disconnect(&mut self) {
        self.disconnects += 1;
    }
}

#[cfg(feature = "native-media")]
fn mock_destination(id: u8) -> DestinationConfig {
    DestinationConfig::new(
        DestinationId::new(id).unwrap(),
        OutputProtocol::Rtmp,
        Endpoint::new("primary.example.test", 19_35, "/live").unwrap(),
        Some(Endpoint::new("backup.example.test", 19_35, "/live").unwrap()),
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::new(
            PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MULTIPLIER,
            Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn mock_packet(sequence: u64) -> fm_io_network::OutputPacket {
    mock_packet_for(MOCK_RENDITION, sequence, 7)
}

#[cfg(feature = "native-media")]
fn mock_packet_for(
    rendition: RenditionId,
    sequence: u64,
    payload_byte: u8,
) -> fm_io_network::OutputPacket {
    use fm_frame::{NormalizedDuration, NormalizedTimestamp};
    fm_io_network::OutputPacket::new(
        rendition,
        sequence,
        NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 33_333_333),
        NormalizedDuration::from_nanos(33_333_333).unwrap(),
        true,
        vec![payload_byte; 16],
    )
    .unwrap()
}

/// One rendition group's planner state over hermetic project-format
/// settings, mirroring what `NativeProgramStream::compile` builds.
#[cfg(feature = "native-media")]
fn test_group(
    rendition_value: u32,
    members: &[u8],
    video_bitrate_kbps: u32,
    group_index: usize,
    group_count: usize,
) -> StreamRenditionCore {
    StreamRenditionCore {
        output: fm_types::OutputId::new(NonZeroU128::new(1).expect("test output is nonzero")),
        members: members
            .iter()
            .map(|id| DestinationId::new(*id).expect("test member fits a slot"))
            .collect(),
        rendition: RenditionId::new(
            NonZeroU32::new(rendition_value).expect("test rendition is nonzero"),
        ),
        variants: pinned_rendition_variants(
            &test_stream_format(),
            video_bitrate_kbps,
            group_index,
            group_count,
        )
        .expect("test rendition variants validate"),
        next_sequence: 0,
    }
}

/// Drives one group through the exact production frame pipeline — gate,
/// plan, pack, fan out, admit — with the pack step scripted.
#[cfg(feature = "native-media")]
fn run_test_group_frame(
    core: &mut StreamRenditionCore,
    output_set: &mut OutputSet,
    streams: &mut [(DestinationId, StreamPairLedger, bool)],
    sequence: SequenceNumber,
    active: &[bool],
    pack: impl FnOnce() -> Result<fm_io_network::OutputPacket, String>,
) -> GroupFrameOutcome {
    let outcome = plan_and_pack_group_frame(
        &core.members,
        &core.variants,
        &mut core.next_sequence,
        sequence,
        active,
        pack,
    );
    if let GroupFrameOutcome::Packed { plan, packet } = outcome {
        let outcomes = output_set
            .enqueue_rendition(&plan, &packet)
            .expect("the packed rendition is always planned");
        let mut routes: Vec<(DestinationId, &mut StreamPairLedger, &mut bool)> = streams
            .iter_mut()
            .filter(|(destination, _, _)| core.members.contains(destination))
            .map(|(destination, ledger, backpressure)| (*destination, ledger, backpressure))
            .collect();
        admit_rendition_outcomes(&outcomes, sequence, &mut routes);
        return GroupFrameOutcome::Packed { plan, packet };
    }
    outcome
}

/// Drives one real output set to Live with one sent packet, so status
/// assertions read genuine telemetry instead of invented values.
#[cfg(feature = "native-media")]
fn driven_live_output_set() -> OutputSet {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(0)).unwrap();
    set.poll(id, 1, &mut sink).unwrap();
    set
}

/// Drives one real output set into a recorded transient write failure.
#[cfg(feature = "native-media")]
fn driven_failing_output_set() -> OutputSet {
    use fm_io_network::{FailureCause, FailureStage, SinkError};
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        write_results: std::collections::VecDeque::from([Err(SinkError::new(
            FailureStage::Write,
            None,
            "muxed media clock stalled",
            true,
        )
        .with_cause(FailureCause::Stalled))]),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(1)).unwrap();
    set.poll(id, 10, &mut sink).unwrap();
    set
}

#[cfg(feature = "native-media")]
#[test]
fn output_set_reaches_live_through_a_started_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();

    // Before start the slot is stopped; start arms it for connecting.
    assert_eq!(set.state(id), Some(DestinationState::Stopped));
    set.start(id).unwrap();
    assert_eq!(set.state(id), Some(DestinationState::Connecting));
    assert_eq!(
        set.poll(id, 0, &mut sink).unwrap(),
        PollEvent::Connected,
        "the scripted sink accepts the first connect"
    );
    assert_eq!(set.state(id), Some(DestinationState::Live));

    set.enqueue(id, mock_packet(0)).unwrap();
    assert_eq!(
        set.poll(id, 1, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 0 }
    );
    assert_eq!(set.telemetry(id).unwrap().packets_sent(), 1);
    assert_eq!(set.telemetry(id).unwrap().bytes_sent(), 16);
}

#[cfg(feature = "native-media")]
#[test]
fn scripted_write_failure_schedules_reconnection_on_the_backup_endpoint() {
    use fm_io_network::ConnectionTarget;
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        write_results: std::collections::VecDeque::from([Err(fm_io_network::SinkError::new(
            fm_io_network::FailureStage::Write,
            Some(104),
            "connection reset by peer",
            true,
        ))]),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(1)).unwrap();

    // The write failure costs one attempt and flips to the backup.
    let event = set.poll(id, 10, &mut sink).unwrap();
    let PollEvent::ReconnectScheduled { retry_at_ms } = event else {
        panic!("expected a scheduled reconnect, got {event:?}");
    };
    assert_eq!(retry_at_ms, 10 + PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS);
    assert_eq!(
        set.state(id),
        Some(DestinationState::WaitingToReconnect {
            attempt: 1,
            retry_at_ms
        })
    );
    assert_eq!(set.connection_target(id), Some(ConnectionTarget::Backup));

    // Backoff holds until the retry instant, then the backup endpoint is
    // contacted and the queued pair replays after it reopens.
    assert_eq!(
        set.poll(id, retry_at_ms - 1, &mut sink).unwrap(),
        PollEvent::WaitingToReconnect { retry_at_ms }
    );
    assert_eq!(
        set.poll(id, retry_at_ms, &mut sink).unwrap(),
        PollEvent::AwaitingRandomAccess {
            dropped_packets: 0,
            dropped_bytes: 0,
        },
        "a recovery replays from the next random-access packet"
    );
    assert_eq!(
        sink.hosts,
        ["primary.example.test", "backup.example.test"],
        "the first failure must move the destination to its backup"
    );
    // Connecting is not yet proof: the recovery counts and the retry
    // budget clears only once the queued pair is delivered.
    assert_eq!(set.telemetry(id).unwrap().reconnects(), 0);
    assert_eq!(
        set.poll(id, retry_at_ms, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 1 }
    );
    assert_eq!(set.telemetry(id).unwrap().reconnects(), 1);
}

/// A target that latched a failure must come back when the operator stops
/// and starts it again, exactly like a recorder slot does.
#[cfg(feature = "native-media")]
#[test]
fn operator_stop_then_start_relaunches_a_failed_stream_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let destination_id = DestinationId::new(1).unwrap();
    let sink_config = RtmpSinkConfig::new(test_rendition(1), test_stream_format());
    let queue_capacity = sink_config.limits.max_outstanding_pairs;
    let mut stream = NativeProgramStream {
        target: SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(7).unwrap()),
        destination_id,
        redacted_destination: "rtmp://primary.example.test:1935/live".to_owned(),
        sink: stream_transport::BackgroundTransport::new(
            ProgramStreamSink::Ffmpeg(FfmpegRtmpSink::new(sink_config)),
            "freemix-native-stream-test".to_owned(),
        ),
        ledger: StreamPairLedger::default(),
        first_failure: None,
        first_failure_code: None,
        enqueue_backpressure: false,
        started: false,
        meter: StreamLinkMeter::default(),
        queue_capacity,
        authored: test_authored_target(7),
    };
    stream.start(&mut set);
    stream.fail("transport:budget_exhausted", &mut set);
    assert!(!route_active(&stream, &set));

    stream.stop_requested(&mut set);
    stream.start(&mut set);

    assert_eq!(
        set.state(destination_id),
        Some(DestinationState::Connecting)
    );
    assert!(route_active(&stream, &set));
}

/// One authored RTMP destination for runtime unit tests.
#[cfg(feature = "native-media")]
fn test_authored_target(id: u128) -> fm_model::StreamTarget {
    fm_model::StreamTarget::new(
        fm_model::StreamTargetId::new(NonZeroU128::new(id).unwrap()),
        format!("Destination {id}"),
        fm_model::StreamProtocol::Rtmp,
        fm_model::StreamEndpoint::parse_for(
            fm_model::StreamProtocol::Rtmp,
            "primary.example.test:1935/live",
        )
        .unwrap(),
        Some(fm_model::StreamKey::parse("live-key-0001").unwrap()),
        fm_types::OutputId::new(NonZeroU128::new(30).unwrap()),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn test_stream_format() -> RecordFormat {
    RecordFormat::new(
        16,
        16,
        FrameRate::new(25, 1).unwrap(),
        SampleRate::new(44_100).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn test_rendition(value: u32) -> RenditionId {
    RenditionId::new(NonZeroU32::new(value).expect("test rendition is nonzero"))
}

#[cfg(feature = "native-media")]
#[test]
fn queue_full_refusals_are_dropped_once_and_reported_as_congestion() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();

    let mut core = test_group(1, &[1], 4_500, 0, 1);
    let mut rows = vec![(id, StreamPairLedger::default(), false)];

    // Capacity is one: the first pair rides, the second is refused,
    // counted as a drop, flagged as backpressure, and never retried.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(0),
        &[true],
        || Ok(mock_packet(0)),
    );
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(1),
        &[true],
        || Ok(mock_packet(1)),
    );

    let (_, ledger, backpressure) = rows[0];
    assert!(backpressure);
    assert_eq!(ledger.enqueued_pairs, 1);
    assert_eq!(ledger.dropped_pairs, 1);
    assert_eq!(set.queue_depth(id), Some(1));
    assert_eq!(set.state(id), Some(DestinationState::Congested));
    let realized = realized_stream_state(false, backpressure, set.state(id));
    assert_eq!(realized, StreamRealizedState::Congested);

    // The group consumed one sequence per frame even though the second
    // pair was refused; the member's admitted cursor advanced only once.
    assert_eq!(core.next_sequence, 2);
    assert_eq!(ledger.next_sequence, 1);
}

#[cfg(feature = "native-media")]
#[test]
fn behind_cursor_frames_are_never_packed_or_fanned_out() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();

    let mut core = test_group(1, &[1], 4_500, 0, 1);
    core.next_sequence = 9;
    let mut rows = vec![(id, StreamPairLedger::default(), false)];

    // A frame behind the shared cursor costs nothing at all: no pack, no
    // fan-out, no counter movement.
    let outcome = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(8),
        &[true],
        || panic!("a consumed sequence must never be packed again"),
    );
    assert!(matches!(outcome, GroupFrameOutcome::Idle));
    assert_eq!(sink.written.len(), 0);
    assert_eq!(set.queue_depth(id), Some(0));
    assert_eq!(rows[0].1.enqueued_pairs, 0);
    assert_eq!(rows[0].1.dropped_pairs, 0);
    assert!(!rows[0].2);
    assert_eq!(core.next_sequence, 9, "an idle frame leaves the cursor");

    // The current sequence packs and queues normally.
    let packed = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(9),
        &[true],
        || Ok(mock_packet(9)),
    );
    assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
    assert_eq!(
        sink.written.len(),
        0,
        "the pair waits for the route to poll"
    );
    assert_eq!(set.queue_depth(id), Some(1));
    assert_eq!(core.next_sequence, 10);
    assert_eq!(rows[0].1.enqueued_pairs, 1);
}

#[cfg(feature = "native-media")]
#[test]
fn stop_disconnects_exactly_once_and_freezes_the_slot() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(0)).unwrap();

    set.stop(id, &mut sink).unwrap();
    assert_eq!(sink.disconnects, 1);
    assert_eq!(set.state(id), Some(DestinationState::Stopped));
    assert_eq!(set.queue_depth(id), Some(0));
    // Telemetry survives the stop so final records keep whole-run counts.
    assert_eq!(set.telemetry(id).unwrap().packets_accepted(), 1);
}

/// Two destinations sharing their protocol and authored bitrate form ONE
/// rendition group: the expensive pack step runs exactly once per frame,
/// both sinks receive byte-identical payloads, and each member's ledger
/// advances while the group consumes one sequence per frame.
#[cfg(feature = "native-media")]
#[test]
fn same_bitrate_destinations_share_one_pack_per_frame() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];
    let mut packs = Vec::new();

    for sequence in [41_u64, 42] {
        let packed = run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(sequence),
            &[true, true],
            || {
                packs.push(sequence);
                Ok(mock_packet_for(
                    MOCK_RENDITION,
                    sequence,
                    u8::try_from(sequence).unwrap_or(u8::MAX),
                ))
            },
        );
        assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
        // Mirrors advance_stream_routes: the shared set drains each route
        // after the frame's fan-out.
        let now_ms = sequence * 33;
        set.poll(id_a, now_ms, &mut sink_a).unwrap();
        set.poll(id_b, now_ms, &mut sink_b).unwrap();
    }

    // One pack per frame feeds the whole group.
    assert_eq!(packs, [41, 42]);
    for (sink, name) in [(&sink_a, "a"), (&sink_b, "b")] {
        assert_eq!(sink.written.len(), 2, "{name}");
        for (index, expected) in [41_u64, 42].into_iter().enumerate() {
            let packet = &sink.written[index];
            let expected_byte = u8::try_from(expected).unwrap_or(u8::MAX);
            assert_eq!(packet.sequence(), expected, "{name}");
            assert_eq!(packet.rendition(), MOCK_RENDITION, "{name}");
            assert!(
                packet.payload().iter().all(|&byte| byte == expected_byte),
                "{name} frame {expected} carries the scripted payload"
            );
        }
    }
    assert_eq!(
        sink_a
            .written
            .iter()
            .map(fm_io_network::OutputPacket::payload)
            .collect::<Vec<_>>(),
        sink_b
            .written
            .iter()
            .map(fm_io_network::OutputPacket::payload)
            .collect::<Vec<_>>(),
        "both members receive the very same packed pair"
    );

    // Both members' ledgers advanced per frame; nobody dropped anything.
    for (destination, ledger, backpressure) in &rows {
        assert_eq!(ledger.enqueued_pairs, 2, "{destination}");
        assert_eq!(ledger.dropped_pairs, 0, "{destination}");
        assert!(!backpressure, "{destination}");
        assert_eq!(ledger.next_sequence, 43, "{destination}");
    }
    assert_eq!(core.next_sequence, 43, "one sequence consumed per frame");
}

/// Distinct authored bitrates stay independent: two groups pack once each
/// per frame, every plan numbers its real rendition at the compile-time
/// group id (the inert sibling rungs exist purely to hold that position),
/// and neither sink ever sees the other group's payload.
#[cfg(feature = "native-media")]
#[test]
fn distinct_bitrates_pack_into_two_independent_groups() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    // Authored order: target A at 3000 kbps is group one, target B at
    // 6000 kbps is group two.
    let mut low = test_group(1, &[1], 3_000, 0, 2);
    let mut high = test_group(2, &[2], 6_000, 1, 2);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];
    let (mut low_packs, mut high_packs) = (0_usize, 0_usize);

    let low_outcome = run_test_group_frame(
        &mut low,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true],
        || {
            low_packs += 1;
            Ok(mock_packet_for(test_rendition(1), 7, 1))
        },
    );
    let high_outcome = run_test_group_frame(
        &mut high,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true],
        || {
            high_packs += 1;
            Ok(mock_packet_for(test_rendition(2), 7, 2))
        },
    );
    assert_eq!(
        (low_packs, high_packs),
        (1, 1),
        "each group packs its own pair"
    );
    // Mirrors advance_stream_routes after the frame's fan-out.
    set.poll(id_a, 7, &mut sink_a).unwrap();
    set.poll(id_b, 7, &mut sink_b).unwrap();

    let GroupFrameOutcome::Packed { plan: low_plan, .. } = low_outcome else {
        panic!("the low group packed");
    };
    assert_eq!(
        low_plan.destinations_for(test_rendition(1)),
        Some([id_a].as_slice())
    );
    let GroupFrameOutcome::Packed {
        plan: high_plan, ..
    } = high_outcome
    else {
        panic!("the high group packed");
    };
    assert_eq!(
        high_plan.destinations_for(test_rendition(2)),
        Some([id_b].as_slice()),
        "the inert sibling rung pins the real rendition at the compile-time id"
    );

    // Every sink saw exactly its own group's payload and rendition.
    assert_eq!(sink_a.written.len(), 1);
    assert_eq!(sink_a.written[0].rendition(), test_rendition(1));
    assert!(sink_a.written[0].payload().iter().all(|&byte| byte == 1));
    assert_eq!(sink_b.written.len(), 1);
    assert_eq!(sink_b.written[0].rendition(), test_rendition(2));
    assert!(sink_b.written[0].payload().iter().all(|&byte| byte == 2));

    assert_eq!(rows[0].1.enqueued_pairs, 1);
    assert_eq!(rows[1].1.enqueued_pairs, 1);
    assert!(!rows[0].2 && !rows[1].2);
}

/// One member stopping mid-show leaves the plan, keeps its ledger frozen,
/// and never sees another packet; restarting it resumes under the same
/// group identity with the next deliverable sequences while the surviving
/// member never interrupted.
#[cfg(feature = "native-media")]
#[test]
fn stopped_member_is_skipped_and_restart_resumes_the_group() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];

    // Frame five reaches both members.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(5),
        &[true, true],
        || Ok(mock_packet(5)),
    );
    set.poll(id_a, 5, &mut sink_a).unwrap();
    set.poll(id_b, 5, &mut sink_b).unwrap();
    assert_eq!(sink_a.written.len(), 1);
    assert_eq!(sink_b.written.len(), 1);

    // Target B stops mid-show: it leaves the plan entirely, its ledger
    // freezes, and A keeps streaming without interruption.
    set.stop(id_b, &mut sink_b).unwrap();
    let packed = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(6),
        &[true, false],
        || Ok(mock_packet(6)),
    );
    let GroupFrameOutcome::Packed { plan, .. } = packed else {
        panic!("the surviving member keeps the group packing");
    };
    assert_eq!(
        plan.destinations_for(MOCK_RENDITION),
        Some([id_a].as_slice()),
        "the plan covers exactly the still-active members"
    );
    set.poll(id_a, 6, &mut sink_a).unwrap();
    assert_eq!(sink_a.written.len(), 2);
    assert_eq!(sink_b.written.len(), 1, "a stopped route receives nothing");
    assert_eq!(rows[0].1.enqueued_pairs, 2);
    assert_eq!(rows[1].1.enqueued_pairs, 1, "the stopped ledger freezes");
    assert_eq!(rows[1].1.dropped_pairs, 0);
    assert!(!rows[1].2);

    // Restarting B resumes under the same group identity with the next
    // deliverable sequences; the sink pads the gap frame six skipped.
    set.start(id_b).unwrap();
    set.poll(id_b, 10, &mut sink_b).unwrap();
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true, true],
        || Ok(mock_packet(7)),
    );
    set.poll(id_a, 7, &mut sink_a).unwrap();
    set.poll(id_b, 7, &mut sink_b).unwrap();
    assert_eq!(sink_b.written.len(), 2);
    assert_eq!(sink_b.written.last().unwrap().sequence(), 7);
    assert_eq!(rows[1].1.enqueued_pairs, 2);
    assert_eq!(rows[1].1.next_sequence, 8);
    assert_eq!(core.next_sequence, 8);
}

/// Backpressure on one member's route flags and congests only that
/// target; the other member of the same group streams clean.
#[cfg(feature = "native-media")]
#[test]
fn a_congested_route_flags_only_that_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];

    // Frame zero fills both single-slot queues.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(0),
        &[true, true],
        || Ok(mock_packet(0)),
    );

    // Only B drains between frames; A stays saturated.
    set.poll(id_b, 1, &mut sink_b).unwrap();

    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(1),
        &[true, true],
        || Ok(mock_packet(1)),
    );

    let (_, ledger_a, flag_a) = rows[0];
    let (_, ledger_b, flag_b) = rows[1];
    assert!(flag_a);
    assert!(!flag_b, "only the saturated route flags backpressure");
    assert_eq!(ledger_a.enqueued_pairs, 1);
    assert_eq!(ledger_a.dropped_pairs, 1);
    assert_eq!(ledger_b.enqueued_pairs, 2);
    assert_eq!(ledger_b.dropped_pairs, 0);

    // Congestion is per-target on the wire too.
    assert_eq!(set.state(id_a), Some(DestinationState::Congested));
    assert_eq!(
        realized_stream_state(false, flag_a, set.state(id_a)),
        StreamRealizedState::Congested
    );
    assert_eq!(
        realized_stream_state(false, flag_b, set.state(id_b)),
        StreamRealizedState::Live
    );
}

#[cfg(feature = "native-media")]
#[test]
fn exhausted_retry_budget_latches_the_target_failed() {
    use fm_io_network::{FailureStage, SinkError};
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        connect_results: (0..=PROGRAM_STREAM_RECONNECT_ATTEMPTS)
            .map(|_| {
                Err(SinkError::new(
                    FailureStage::Connect,
                    Some(111),
                    "connection refused",
                    true,
                ))
            })
            .collect(),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    let mut now_ms = 0_u64;
    loop {
        if set.poll(id, now_ms, &mut sink).unwrap() == PollEvent::Failed {
            break;
        }
        now_ms += PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS;
    }
    assert_eq!(set.state(id), Some(DestinationState::Failed));
    let telemetry = set.telemetry(id).unwrap();
    assert_eq!(
        telemetry.connect_attempts(),
        u64::from(PROGRAM_STREAM_RECONNECT_ATTEMPTS) + 1,
        "the initial attempt plus every budgeted retry ran"
    );
    assert!(
        telemetry.latest_failure().unwrap().retryable,
        "refused connections stay transient; only the budget ends them"
    );
}

#[cfg(feature = "native-media")]
#[test]
#[allow(clippy::too_many_lines)] // one case per protocol convention
fn split_destination_url_round_trips_every_protocol_convention() {
    // (authored URL, protocol, host, port, endpoint path, key tail). The
    // key is restated here because the split result deliberately cannot
    // be read back in the clear.
    for (url, protocol, host, port, path, key) in [
        (
            "rtmp://ingest.example.test:1935/live_app/live-key-0001",
            OutputProtocol::Rtmp,
            "ingest.example.test",
            1_935_u16,
            "/live_app",
            Some("live-key-0001"),
        ),
        (
            "rtmps://ingest.example.test:1936/a/b/key12345?token=abcdef",
            OutputProtocol::Rtmps,
            "ingest.example.test",
            1_936,
            "/a/b",
            Some("key12345?token=abcdef"),
        ),
        (
            "srt://ingest.example.test:9710?streamid=live-key-0001",
            OutputProtocol::Srt,
            "ingest.example.test",
            9_710,
            "/",
            Some("live-key-0001"),
        ),
        (
            "srt://ingest.example.test:9710",
            OutputProtocol::Srt,
            "ingest.example.test",
            9_710,
            "/",
            None,
        ),
        (
            "omt://0.0.0.0:6500",
            OutputProtocol::Omt,
            "0.0.0.0",
            6_500,
            "/",
            None,
        ),
        (
            "hls:///var/lib/freemix/live",
            OutputProtocol::Hls,
            // A local destination has no peer: the placeholder host and
            // port are never rendered, and the directory is the path.
            "localhost",
            1,
            "/var/lib/freemix/live",
            None,
        ),
    ] {
        let (split_protocol, endpoint, split_key) = split_destination_url(url).expect(url);
        assert_eq!(split_protocol, protocol, "{url:?}");
        assert_eq!(endpoint.host(), host);
        assert_eq!(u16::from(endpoint.port()), port);
        assert_eq!(endpoint.path(), path);
        assert_eq!(
            split_key.is_some(),
            key.is_some(),
            "{url:?} keeps its key presence"
        );

        // The convention round-trips: composing the pieces back together
        // under the transport's rules yields a URL the codec parser — the
        // single validity source — accepts again.
        let composed = match (protocol, key) {
            (OutputProtocol::Rtmp, Some(key)) => {
                format!("rtmp://{host}:{port}{path}/{key}")
            }
            (OutputProtocol::Rtmps, Some(key)) => {
                format!("rtmps://{host}:{port}{path}/{key}")
            }
            (OutputProtocol::Srt, Some(key)) => format!("srt://{host}:{port}?streamid={key}"),
            (OutputProtocol::Srt, None) => format!("srt://{host}:{port}"),
            (OutputProtocol::Hls, None) => format!("hls://{path}"),
            (OutputProtocol::Hls | OutputProtocol::Omt, Some(_)) => {
                unreachable!("local OMT and HLS destinations carry no stream key")
            }
            (OutputProtocol::Omt, None) => format!("omt://{host}:{port}"),
            (OutputProtocol::LiveLan, _) => {
                unreachable!("no daemon streaming transport for LAN outputs")
            }
            (_, None) => unreachable!("network destinations always carry a key here"),
        };
        if protocol != OutputProtocol::Omt {
            fm_codec_ffmpeg::stream::StreamDestination::parse(&composed)
                .unwrap_or_else(|error| panic!("{composed:?} does not re-parse: {error:?}"));
        }
    }

    // Shapes that cannot be represented fail compilation instead of being
    // guessed at air time.
    for url in [
        "rtmp://host.example.test/app/key12345",
        "rtmp://host.example.test:1935",
        "srt://host.example.test?param=key12345",
        "http://host.example.test:1935/app/key12345",
        "srt://host.example.test:70000?streamid=key12345",
        "hls://relative/dir",
        "hls:///srv/../etc",
        "hls:///srv//live",
        "hls:///srv/live/",
        "hls://",
    ] {
        assert!(
            split_destination_url(url).is_err(),
            "{url:?} must be refused"
        );
    }
}

/// An SRT stream id is one `?streamid=` query value, so query separators
/// in it are refused with the typed reason and never echoed; the same key
/// text stays legal as an RTMP path segment.
#[cfg(feature = "native-media")]
#[test]
fn split_destination_url_validates_the_key_for_its_protocol() {
    for url in [
        "srt://ingest.example.test:9710?streamid=live-key&latency=1",
        "srt://ingest.example.test:9710?streamid=live-key?0001",
    ] {
        let Err(error) = split_destination_url(url) else {
            panic!("{url:?} must be refused");
        };
        let message = error.to_string();
        assert!(
            message.contains("stream destination key was refused")
                && message.contains("SRT stream id"),
            "{message}"
        );
        assert!(!message.contains("live-key"), "{message}");
    }
    let (_, _, key) =
        split_destination_url("rtmp://ingest.example.test:1935/live/live-key&x=1").unwrap();
    assert!(key.is_some());
    // A key no protocol can carry keeps its own reason.
    let error = split_destination_url("srt://ingest.example.test:9710?streamid=abc")
        .unwrap_err()
        .to_string();
    assert!(error.contains("redactable"), "{error}");
}

#[cfg(feature = "native-media")]
#[test]
fn redacted_destination_url_masks_keys_and_renders_hls_paths_verbatim() {
    let network = Endpoint::new("ingest.example.test", 1_935, "/live_app").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmp, &network, true),
        "rtmp://ingest.example.test:1935/live_app/****"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmp, &network, false),
        "rtmp://ingest.example.test:1935/live_app"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmps, &network, true),
        "rtmps://ingest.example.test:1935/live_app/****"
    );
    let srt = Endpoint::new("ingest.example.test", 9_710, "/").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Srt, &srt, true),
        "srt://ingest.example.test:9710?streamid=****"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Srt, &srt, false),
        "srt://ingest.example.test:9710"
    );
    // A local HLS directory is operator-chosen storage, not a secret: it
    // renders verbatim no matter what key presence the caller reports.
    let hls = Endpoint::local_directory("/var/lib/freemix/live").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Hls, &hls, false),
        "hls:///var/lib/freemix/live"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Hls, &hls, true),
        "hls:///var/lib/freemix/live"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn pending_network_probe_skips_hls_and_maps_network_endpoints() {
    let rtmp = DestinationConfig::new(
        DestinationId::new(1).unwrap(),
        OutputProtocol::Rtmp,
        Endpoint::new("ingest.example.test", 1_935, "/live").unwrap(),
        Some(Endpoint::new("backup.example.test", 1_935, "/live").unwrap()),
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::disabled(),
    )
    .unwrap();

    let primary = pending_network_probe(&rtmp, 7, preflight::PRIMARY_ENDPOINT)
        .expect("an RTMP destination has a primary endpoint to probe");
    assert_eq!(primary.target, 7);
    assert_eq!(primary.endpoint, preflight::PRIMARY_ENDPOINT);
    assert_eq!(primary.host, "ingest.example.test");
    assert_eq!(u16::from(primary.port), 1_935);
    assert_eq!(primary.timeout, preflight::NETWORK_PREFLIGHT_TIMEOUT);

    let backup = pending_network_probe(&rtmp, 7, preflight::BACKUP_ENDPOINT)
        .expect("a configured backup endpoint is probed too");
    assert_eq!(backup.target, 7);
    assert_eq!(backup.endpoint, preflight::BACKUP_ENDPOINT);
    assert_eq!(backup.host, "backup.example.test");
    assert_eq!(u16::from(backup.port), 1_935);

    // A local HLS destination is a directory write, not a peer, so it has
    // no network endpoint to probe on either slot.
    let hls = DestinationConfig::new(
        DestinationId::new(2).unwrap(),
        OutputProtocol::Hls,
        Endpoint::local_directory("/var/lib/freemix/live").unwrap(),
        None,
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::disabled(),
    )
    .unwrap();
    assert_eq!(
        pending_network_probe(&hls, 8, preflight::PRIMARY_ENDPOINT),
        None
    );
    assert_eq!(
        pending_network_probe(&hls, 8, preflight::BACKUP_ENDPOINT),
        None
    );
}

#[cfg(feature = "native-media")]
#[test]
fn split_and_group_targets_gives_hls_its_own_rendition_group() {
    use fm_model::{
        DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
        StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output = ModelOutputId::new(NonZeroU128::new(30).unwrap());
    let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
        let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
        let key = match protocol {
            StreamProtocol::Hls | StreamProtocol::Omt => None,
            StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                Some(ModelStreamKey::parse("live-key-0001").unwrap())
            }
        };
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            protocol,
            endpoint,
            key,
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
        target(2, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
        target(3, StreamProtocol::Hls, "/var/lib/freemix/live"),
        target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();
    // One group per distinct output/protocol/bitrate identity in
    // first-appearance order: both RTMP targets share the first, both HLS
    // targets the second, and every HLS target splits with no stream key.
    assert_eq!(
        grouped.group_keys,
        vec![
            (output, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
            (output, OutputProtocol::Hls, DEFAULT_VIDEO_BITRATE_KBPS),
        ]
    );
    assert_eq!(
        grouped
            .splits
            .iter()
            .map(|split| split.group_index)
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 1]
    );
    for split in &grouped.splits {
        assert_eq!(
            split.stream_key.is_some(),
            split.protocol != OutputProtocol::Hls,
            "HLS targets carry no stream key"
        );
        assert_eq!(split.backup_endpoint, None);
    }
    assert_eq!(grouped.splits[2].endpoint.path(), "/var/lib/freemix/live");
}

#[cfg(feature = "native-media")]
#[test]
fn split_and_group_targets_partitions_rendition_groups_by_output_id() {
    use fm_model::{
        DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
        StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output_a = ModelOutputId::new(NonZeroU128::new(30).unwrap());
    let output_b = ModelOutputId::new(NonZeroU128::new(31).unwrap());
    let target = |id: u128, output: ModelOutputId, endpoint: &str| {
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            StreamProtocol::Rtmp,
            ModelStreamEndpoint::parse_for(StreamProtocol::Rtmp, endpoint).unwrap(),
            Some(ModelStreamKey::parse("live-key-0001").unwrap()),
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, output_a, "ingest-a1.example.test:1935/live"),
        target(2, output_a, "ingest-a2.example.test:1935/live"),
        target(3, output_b, "ingest-b1.example.test:1935/live"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();

    assert_eq!(
        grouped.group_keys,
        vec![
            (output_a, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
            (output_b, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
        ]
    );
    assert_eq!(
        grouped
            .splits
            .iter()
            .map(|split| split.group_index)
            .collect::<Vec<_>>(),
        vec![0, 0, 1]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn local_output_directories_extracts_the_hls_directory_of_each_authored_target() {
    use fm_model::{
        StreamEndpoint as ModelStreamEndpoint, StreamKey as ModelStreamKey, StreamProtocol,
        StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output = ModelOutputId::new(NonZeroU128::new(31).unwrap());
    let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
        let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
        let key = match protocol {
            StreamProtocol::Hls | StreamProtocol::Omt => None,
            StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                Some(ModelStreamKey::parse("live-key-0001").unwrap())
            }
        };
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            protocol,
            endpoint,
            key,
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
        target(2, StreamProtocol::Hls, "/var/lib/freemix/live"),
        target(3, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
        target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();

    // Only local HLS targets contribute a directory, in authored order,
    // and the compiled endpoint already carries it as one absolute path
    // that the sink creates later.
    assert_eq!(
        local_output_directories(&grouped.splits),
        vec![
            PathBuf::from("/var/lib/freemix/live"),
            PathBuf::from("/var/lib/freemix/live-alt"),
        ]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_status_message_sequence_starts_at_one_and_skips_empty_passes() {
    let server = ServerIdentity {
        engine_id: "engine".to_owned(),
        project_id: "project".to_owned(),
        state_epoch: 1,
        log_id: "log".to_owned(),
    };
    assert!(
        stream_status_message(&server, 0, Vec::new())
            .unwrap()
            .is_none(),
        "an empty active set publishes no record"
    );
    let first = stream_status_message(&server, 0, vec![stream_status_placeholder_sample()])
        .unwrap()
        .expect("non-empty set publishes");
    assert_eq!(first.sequence, 1);
    assert_eq!(first.server, server);
    let second = stream_status_message(
        &server,
        first.sequence,
        vec![stream_status_placeholder_sample()],
    )
    .unwrap()
    .expect("non-empty set publishes");
    assert_eq!(second.sequence, 2);
    assert!(encode_line(&WireMessage::StreamStatus(second)).is_ok());
}

#[cfg(feature = "native-media")]
fn stream_status_placeholder_sample() -> StreamStatusSample {
    StreamStatusSample {
        target: WireStreamTargetId::new(NonZeroU128::new(3).unwrap()),
        realized: StreamRealizedState::Starting,
        connected: false,
        muxed_bytes: 0,
        enqueued_pairs: 0,
        dropped_pairs: 0,
        reconnects: 0,
        uptime_ms: 0,
        bitrate_kbps: 0,
        failure: None,
        local: None,
    }
}

#[cfg(feature = "native-media")]
#[test]
fn stream_feed_builds_valid_paired_frames_from_cloned_audio() {
    // Mirrors NativeProgramStream::compile: format from project settings,
    // pairs built with each frame's audio sequence and a cloned block.
    let format = RecordFormat::new(
        16,
        16,
        FrameRate::new(25, 1).unwrap(),
        SampleRate::new(44_100).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    )
    .unwrap();
    let build_pair = |sequence: SequenceNumber| {
        // The engine's absolute-boundary audio contract, restated here
        // because the codec keeps its timing oracle private:
        // sample bounds are floor(n * rate * den / num).
        let hertz = u128::from(format.sample_rate().hertz());
        let boundary = |frames: u64| -> u128 {
            u128::from(frames) * hertz * u128::from(format.frame_rate().denominator())
                / u128::from(format.frame_rate().numerator())
        };
        let start_sample = boundary(sequence.get());
        let end_sample = boundary(sequence.get().checked_add(1).expect("sequence fits"));
        let start_nanos = start_sample * 1_000_000_000 / hertz;
        let duration_nanos = (end_sample * 1_000_000_000 / hertz) - start_nanos;
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(i64::try_from(start_sample).unwrap()),
                fm_frame::TimeBase::new(1, format.sample_rate().hertz()).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).unwrap()),
            NormalizedDuration::from_nanos(u64::try_from(duration_nanos).unwrap()).unwrap(),
            native_clock_domain(),
            sequence,
        )
        .unwrap();
        let samples = usize::try_from(end_sample - start_sample).unwrap();
        AudioBlock::new(
            timing,
            format.sample_rate(),
            format.channel_layout().clone(),
            vec![vec![0.25; samples], vec![-0.25; samples]],
        )
        .unwrap()
    };
    for sequence in [SequenceNumber::new(0), SequenceNumber::new(41)] {
        let audio = build_pair(sequence);
        let cloned = audio.clone();
        let pair = PairedFrame::new(
            &format,
            sequence,
            vec![0; format.rgba_bytes_per_frame()],
            cloned,
        )
        .expect("cloned project audio must pair");
        assert_eq!(pair.sequence(), sequence);
        assert_eq!(pair.format(), &format);
    }
}

/// Regression: the compiled grades are shared across revisions that do
/// not change them, so a cut or a fader move never re-bakes and
/// re-uploads every native grade lattice on the frame loop.
#[test]
fn grade_cache_keeps_grades_across_commands_that_do_not_change_them() {
    let project = test_project();
    let mut control = test_control(&project);
    let principal = operator();
    let graded = control
        .submit(
            &principal,
            test_command(
                "grade",
                "grade-key",
                CommandPayload::SetInputGrade {
                    input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    source: Some(fm_protocol::WireGradeSource::Inline(Box::new(
                        fm_grade::Grade {
                            saturation_permille: 0,
                            ..fm_grade::Grade::IDENTITY
                        },
                    ))),
                    gesture: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(graded.is_accepted());
    let mut cache = grading::GradeCache::default();
    let before = cache.at(&control);
    let cut = control
        .submit(
            &principal,
            test_command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(cut.is_accepted());
    let after = cache.at(&control);
    assert!(Arc::ptr_eq(&before, &after));
}

const ANIMATED_TITLE_URI: &str = "asset://titles/unit.fmtitle";

fn animated_title_template(elements: u128, clip: fm_titles::TitleClip) -> fm_titles::TitleTemplate {
    fm_titles::TitleTemplate {
        id: fm_titles::TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower".into(),
        width: 32,
        height: 18,
        background: fm_titles::Color::TRANSPARENT,
        fields: Vec::new(),
        elements: (1..=elements)
            .map(|element| fm_titles::Element {
                id: fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                name: format!("box {element}"),
                bounds: fm_titles::Bounds {
                    x: 2,
                    y: 2,
                    width: 8,
                    height: 8,
                },
                z_index: 0,
                visible: true,
                style: fm_titles::Style::default(),
                color_field: None,
                kind: fm_titles::ElementKind::Rectangle,
            })
            .collect(),
        animation: fm_titles::TitleAnimation::new(clip, 0),
    }
}

/// [`test_project`] with title input 3 naming [`ANIMATED_TITLE_URI`].
fn animated_title_project() -> StoredProject {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Lower third".into(),
        kind: InputKind::Title(fm_model::TitleInput {
            template_uri: ANIMATED_TITLE_URI.into(),
            field_revision: 0,
            fields: Vec::new(),
        }),
        required_capabilities: Vec::new(),
    });
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

/// Refuses every journal append and every discard of a staged copy.
struct UndiscardableStager {
    discarded: Cell<bool>,
}

impl DurableStore for UndiscardableStager {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        Err(AppFailure("injected journal failure".into()).into())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }

    fn discard_staged_title_animations(&self, _revision: u64, uris: &[String]) -> AppResult<()> {
        assert_eq!(uris, [ANIMATED_TITLE_URI.to_owned()]);
        self.discarded.set(true);
        Err(AppFailure("injected discard failure".into()).into())
    }
}

/// Regression: an animation edit refused after its template was staged
/// left the staged copy behind when discarding it failed. The next
/// command reuses the revision, and the checkpoint covering it committed
/// the refused animation over the template. A copy that cannot be
/// discarded now stops the daemon, whose next start discards it.
#[test]
fn a_staged_title_animation_that_cannot_be_discarded_stops_the_daemon() {
    let rate = FrameRate::new(25, 1).unwrap();
    let empty =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
            .unwrap();
    let mut durable = animated_title_project();
    let assets = BundleAssets {
        titles: [(test_input_id(3), animated_title_template(0, empty))].into(),
        luts: std::collections::BTreeMap::new(),
        assets_root: None,
    };
    let mut control = ControlService::new(
        restore_engine_with_titles(&durable, &assets).unwrap(),
        Policy::development(),
        "unit-engine",
        "unit-log",
        ControlLimits::default(),
    );
    let server = test_server(&control);
    let longer =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
            .unwrap();
    let command = test_command(
        "animate",
        "animate-key",
        CommandPayload::SetTitleAnimation {
            input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
            base_revision: 0,
            clip: Box::new(fm_control::title_clip_to_wire(&longer)),
        },
    );
    let store = UndiscardableStager {
        discarded: Cell::new(false),
    };
    let outcome = execute_durable_command(
        &mut control,
        &store,
        &mut durable,
        &operator(),
        &server,
        &command,
        0,
    );
    assert!(store.discarded.get());
    assert!(
        outcome.is_err(),
        "an undiscardable staged copy must stop the daemon"
    );
    assert_eq!(control.diagnostics().current_revision, 0);
}

/// Regression: an animation edit stays staged beside its template until
/// a checkpoint covers it, and checkpoints committed only the templates
/// a title still named. When the only title naming the template was
/// removed after the edit, the copy was never committed or discarded, and
/// a title added offline later picked the stale edit up on the next
/// start. Every staged copy now commits by the same durable-revision
/// rule, at the checkpoint and at startup recovery alike.
#[test]
fn a_staged_animation_of_a_template_no_title_names_any_more_still_commits() {
    use journal::DurableJournal;

    let rate = FrameRate::new(25, 1).unwrap();
    let empty =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
            .unwrap();
    let longer =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
            .unwrap();
    for checkpoint in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let template_path = store.assets_root().join("titles/unit.fmtitle");
        fs::create_dir_all(template_path.parent().unwrap()).unwrap();
        fs::write(
            &template_path,
            fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                template: animated_title_template(0, empty.clone()),
                fonts: Vec::new(),
                clocks: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let manifest = animated_title_project();
        store.save(&manifest).unwrap();
        let assets = BundleAssets {
            titles: [(test_input_id(3), animated_title_template(0, empty.clone()))].into(),
            luts: std::collections::BTreeMap::new(),
            assets_root: None,
        };
        let mut control = ControlService::new(
            restore_engine_with_titles(&manifest, &assets).unwrap(),
            Policy::development(),
            "unit-engine",
            "unit-log",
            ControlLimits::default(),
        );
        let server = test_server(&control);
        let mut durable = manifest.clone();
        {
            let journal = DurableJournal::open(&store).unwrap();
            for command in [
                test_command(
                    "animate",
                    "animate-key",
                    CommandPayload::SetTitleAnimation {
                        input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                        base_revision: 0,
                        clip: Box::new(fm_control::title_clip_to_wire(&longer)),
                    },
                ),
                test_command(
                    "remove",
                    "remove-key",
                    CommandPayload::RemoveInput {
                        input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                    },
                ),
            ] {
                let outcome = execute_durable_command(
                    &mut control,
                    &journal,
                    &mut durable,
                    &operator(),
                    &server,
                    &command,
                    0,
                )
                .unwrap();
                let result = &outcome.submission.output.result;
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{result:?}"
                );
            }
            if checkpoint {
                journal.settle(&durable).unwrap();
            }
        }
        if !checkpoint {
            load_and_recover(&store, false).unwrap();
        }
        let titles: Vec<String> = fs::read_dir(template_path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(titles, ["unit.fmtitle"], "checkpoint={checkpoint}");
        assert_eq!(
            store
                .load_title_template(ANIMATED_TITLE_URI)
                .unwrap()
                .template
                .animation
                .clip,
            longer,
            "checkpoint={checkpoint}"
        );
    }
}

/// Regression: a client could import a LUT under the id of a removed
/// one, replacing its bundled file, so an older manifest, journal, or
/// autosave naming that id loaded different content. Bundled LUT files
/// are immutable: the import is refused as a non-retryable conflict and
/// the file is untouched; the same text under the same id is accepted.
#[test]
fn a_lut_import_never_replaces_a_bundled_file_with_different_content() {
    use journal::DurableJournal;

    const INVERT: &str = "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";
    const IDENTITY: &str =
        "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    // LUT 1 was imported and removed before: its file stays.
    let one = NonZeroU32::new(1).unwrap();
    store
        .write_lut_source(fm_model::LutId::new(one), INVERT)
        .unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let journal = DurableJournal::open(&store).unwrap();
    let import = |key: &str, data: &str| {
        test_command(
            key,
            &format!("{key}-key"),
            CommandPayload::ImportGradeLut {
                lut: one,
                name: "Look".into(),
                data: data.into(),
            },
        )
    };

    let refused = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &import("reuse", IDENTITY),
        0,
    )
    .unwrap();
    let CommandResult::Rejected {
        code, retryable, ..
    } = &refused.submission.output.result
    else {
        panic!("{:?}", refused.submission.output.result);
    };
    assert_eq!(code, RejectionCode::Conflict.as_str());
    assert!(!retryable);
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(
        fs::read_to_string(store.assets_root().join("luts/1.cube")).unwrap(),
        INVERT
    );

    let same = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &import("same", INVERT),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            same.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        same.submission.output.result
    );
}

/// Regression: a stored template whose animation has no wire encoding
/// (beyond `MAX_TITLE_ANIMATION_BYTES`) made every snapshot's
/// `title_animation_state` unencodable, so no session could connect.
/// Like names beyond the `title_state` bounds, it refuses to start.
#[test]
fn templates_whose_animation_has_no_wire_encoding_refuse_to_start() {
    use fm_titles::fm_animation::{
        Clip, CubicBezier, Easing, Keyframe, MAX_PHASE_FRAMES, Phase, Property, Timebase, Track,
        TrackKey, Value,
    };

    let bezier = Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
    let tracks = 16_u128;
    let clip = Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        MAX_PHASE_FRAMES,
        0,
        (1..=tracks)
            .map(|element| {
                Track::new(
                    TrackKey::new(
                        fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                        Phase::In,
                        Property::PositionX,
                    ),
                    (0..256_u32)
                        .map(|frame| {
                            Keyframe::new(frame * 100, Value::Scalar(i64::from(i32::MIN)), bezier)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
    .unwrap();
    assert!(
        fm_protocol::title_clip_wire_bytes(&fm_control::title_clip_to_wire(&clip))
            > fm_protocol::MAX_TITLE_ANIMATION_BYTES
    );
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template: animated_title_template(tracks, clip),
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let path = store.assets_root().join("titles/unit.fmtitle");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, document).unwrap();

    let error = load_title_templates(&store, animated_title_project().project())
        .expect_err("an animation without a wire encoding refuses to start");
    assert!(error.to_string().contains("animation"), "{error}");
}
