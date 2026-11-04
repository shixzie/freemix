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

// The tests are split across include! files to keep every source file under
// the repository's 5,000-line structural bound. They are textually included
// into this module, so they share its imports and helpers.

include!("tests/arguments.rs");
include!("tests/runtime.rs");
include!("tests/sessions.rs");
include!("tests/native.rs");
