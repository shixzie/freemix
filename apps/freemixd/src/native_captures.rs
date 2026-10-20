//! Native realization of local camera and capture-card inputs (IN-001).
//!
//! A native daemon with a capture backend (V4L2 through `ffmpeg -f v4l2`
//! on Linux, or the diagnostic fake backend of
//! `--diagnostic-capture-inventory`) owns one [`NativeCaptures`]: a
//! [`CaptureMonitor`] that enumerates devices off the loop (hot-plug,
//! format changes, signal flags), and one [`CaptureSupervisor`] per capture
//! input. Once per engine frame the session loop pulls every input
//! ([`NativeCaptures::pull`]): the supervisor follows the latest
//! enumeration (acquiring, renegotiating, or dropping its device) and hands
//! over the frame's picture, dated on the output timeline, and its audio,
//! which the Master takes as pushed audio. Nothing waits on a device.
//!
//! A capture input starts on a transparent black picture. When its signal
//! is lost (device unplugged, stalled, no input signal, or failing) it
//! shows its loss slate (the last picture, or opaque black) and silence,
//! and the device is reacquired under the same input ID when it returns,
//! at the same or another node. A closed input (IN-026) stops acquiring
//! and shows a blank picture until it reopens. `input_capture_set` changes
//! apply live; a changed format or audio choice reopens the device.
//!
//! Each input's [`CaptureStatus`] reaches clients as `capture_status`
//! runtime events ([`NativeCaptures::statuses`]): at once when its state
//! changes, otherwise at most once a second. The device inventory reaches
//! them as `capture_devices` whenever it changes
//! ([`NativeCaptures::inventory`]). Every hot-plug event and state change is
//! logged as a `FREEMIXD_CAPTURE` record.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_frame::ClockDomainId;
use fm_io_linux::{
    Acquirer, CaptureMonitor, CaptureSupervisor, DeviceSnapshot, FakeAcquirer, FakeCaptureWorld,
    FakeProvider, HotplugEvent, PulledPicture, SupervisorTiming, diff,
};
use fm_types::capture::{CaptureInventory, CaptureSettings, CaptureStatus, CaptureTarget};
use fm_types::{FrameRate, InputId, SampleRate};

use crate::native_files::blank_picture;
use crate::native_files::output_timing;
use crate::native_streams::{Format, PulledStream};
use crate::native_sync::{SourceKind, SyncLog};

/// Longest a changed-but-same-state status waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// How often the device list is enumerated.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often the diagnostic fake inventory file is re-read.
const FAKE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The capture backend a native daemon uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CaptureBackend {
    /// This system's devices: V4L2 on Linux.
    System,
    /// The diagnostic fake devices listed in an inventory file.
    Fake(PathBuf),
}

impl CaptureBackend {
    /// The backend of this platform without diagnostics, if it has one.
    pub(crate) fn platform() -> Option<Self> {
        cfg!(target_os = "linux").then_some(Self::System)
    }
}

/// One capture input a native daemon realizes.
pub(crate) struct CaptureInput {
    pub(crate) input: InputId,
    pub(crate) stable_key: String,
    pub(crate) settings: CaptureSettings,
    /// The input is closed (IN-026): nothing is acquired until it reopens.
    pub(crate) closed: bool,
}

struct Capture {
    supervisor: CaptureSupervisor,
    closed: bool,
    /// The input closed and its blank picture is still to be presented.
    blank_pending: bool,
    reported: Option<(CaptureStatus, Instant)>,
    /// When its clock drift was last logged (AU-004).
    sync_log: SyncLog,
}

/// The capture inputs of one native daemon and their device backend.
pub(crate) struct NativeCaptures {
    backend: &'static str,
    monitor: CaptureMonitor,
    acquirer: Arc<dyn Acquirer>,
    captures: BTreeMap<InputId, Capture>,
    format: Format,
    snapshot: Arc<DeviceSnapshot>,
    reported_generation: Option<u64>,
    /// The engine frame last pulled for.
    last_frame: Option<u64>,
}

impl NativeCaptures {
    /// Starts the backend's device monitor and a supervisor for every
    /// input.
    ///
    /// # Errors
    ///
    /// Returns why the backend cannot start (no provider on this platform,
    /// or the monitor thread cannot be spawned).
    pub(crate) fn start(
        backend: &CaptureBackend,
        inputs: Vec<CaptureInput>,
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
        sample_rate: SampleRate,
    ) -> Result<Self, String> {
        let (monitor, acquirer): (CaptureMonitor, Arc<dyn Acquirer>) = match backend {
            CaptureBackend::Fake(path) => {
                let world = FakeCaptureWorld::from_file(path.clone());
                (
                    CaptureMonitor::spawn(
                        Box::new(FakeProvider::new(Arc::clone(&world))),
                        FAKE_POLL_INTERVAL,
                    )
                    .map_err(|error| format!("capture monitor: {error}"))?,
                    Arc::new(FakeAcquirer::new(world)),
                )
            }
            CaptureBackend::System => {
                let provider = fm_io_linux::system_provider()
                    .ok_or_else(|| "this platform has no capture backend".to_owned())?;
                (
                    CaptureMonitor::spawn(provider, POLL_INTERVAL)
                        .map_err(|error| format!("capture monitor: {error}"))?,
                    Arc::new(fm_io_linux::FfmpegAcquirer::v4l2()),
                )
            }
        };
        let snapshot = monitor.snapshot();
        let mut captures = Self {
            backend: monitor.backend(),
            monitor,
            acquirer,
            captures: BTreeMap::new(),
            format: Format {
                rate,
                clock_domain,
                dimensions,
                sample_rate,
            },
            snapshot,
            reported_generation: None,
            last_frame: None,
        };
        for input in inputs {
            captures.add(input);
        }
        Ok(captures)
    }

    fn supervisor(&self, stable_key: String, settings: CaptureSettings) -> CaptureSupervisor {
        CaptureSupervisor::new(
            stable_key,
            settings,
            CaptureTarget {
                width: self.format.dimensions.0,
                height: self.format.dimensions.1,
                rate: self.format.rate,
            },
            SupervisorTiming::default(),
        )
        .with_sample_rate(self.format.sample_rate.hertz())
    }

    fn add(&mut self, input: CaptureInput) {
        let mut supervisor = self.supervisor(input.stable_key, input.settings);
        supervisor.set_closed(input.closed);
        eprintln!(
            "FREEMIXD_CAPTURE\tv=1\tinput={}\tbackend={}\tdevice={}",
            input.input,
            self.backend,
            record_text(supervisor.key())
        );
        self.captures.insert(
            input.input,
            Capture {
                supervisor,
                closed: input.closed,
                blank_pending: false,
                reported: None,
                sync_log: SyncLog::default(),
            },
        );
    }

    /// Every capture input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.captures.keys().copied().collect()
    }

    /// Follows the project: adds, removes, reconfigures, closes, and
    /// reopens inputs as `inputs` (input, stable key, settings) and
    /// `closed` say. A changed stable key is a new source: the old device
    /// is released.
    pub(crate) fn sync(
        &mut self,
        inputs: &[(InputId, String, CaptureSettings)],
        closed: impl Fn(InputId) -> bool,
    ) {
        self.captures
            .retain(|input, _| inputs.iter().any(|(candidate, ..)| candidate == input));
        for (input, stable_key, settings) in inputs {
            let closed = closed(*input);
            let replace = self
                .captures
                .get(input)
                .is_none_or(|capture| capture.supervisor.key() != stable_key);
            if replace {
                self.captures.remove(input);
                self.add(CaptureInput {
                    input: *input,
                    stable_key: stable_key.clone(),
                    settings: settings.clone(),
                    closed,
                });
                continue;
            }
            let capture = self.captures.get_mut(input).expect("checked above");
            capture.supervisor.set_settings(settings.clone());
            if closed != capture.closed {
                capture.closed = closed;
                capture.blank_pending = closed;
                capture.supervisor.set_closed(closed);
            }
        }
    }

    /// Follows the latest device enumeration, logging its hot-plug events.
    fn refresh(&mut self) {
        let snapshot = self.monitor.snapshot();
        if snapshot.generation == self.snapshot.generation {
            return;
        }
        for event in diff(&self.snapshot.devices, &snapshot.devices) {
            let (kind, key, path) = match &event {
                HotplugEvent::Added(device) => ("added", &device.stable_key, &device.path),
                HotplugEvent::Removed(device) => ("removed", &device.stable_key, &device.path),
                HotplugEvent::Changed { after, .. } => {
                    ("changed", &after.info.stable_key, &after.info.path)
                }
            };
            eprintln!(
                "FREEMIXD_CAPTURE\tv=1\tevent={kind}\tdevice={}\tpath={}",
                record_text(key),
                record_text(path)
            );
        }
        self.snapshot = snapshot;
    }

    /// Pulls every input's picture and audio for engine frame `frame`,
    /// once per frame. Never blocks on a device.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        if self.last_frame.is_some_and(|last| frame <= last) || self.captures.is_empty() {
            return Vec::new();
        }
        self.last_frame = Some(frame);
        self.refresh();
        let now = Instant::now();
        let format = self.format;
        let mut pulled = Vec::with_capacity(self.captures.len());
        for (&input, capture) in &mut self.captures {
            if capture.closed {
                if std::mem::take(&mut capture.blank_pending) {
                    pulled.push(PulledStream {
                        input,
                        video: output_timing(frame, format.rate, format.clock_domain)
                            .and_then(|timing| blank_picture(format.dimensions, timing)),
                        audio: None,
                    });
                }
                continue;
            }
            capture
                .supervisor
                .update(self.acquirer.as_ref(), &self.snapshot.devices, now);
            let pull = capture.supervisor.pull(now);
            let video = match pull.picture {
                PulledPicture::Keep => None,
                PulledPicture::New(bytes) => format.picture(frame, bytes),
                PulledPicture::Black => format.black(frame),
            };
            let audio = pull
                .audio
                .filter(|planes| planes.len() == 2)
                .and_then(|planes| format.audio(frame, planes))
                .or_else(|| format.silence(frame));
            pulled.push(PulledStream {
                input,
                video,
                audio,
            });
        }
        pulled
    }

    /// The statuses due for a report at `now`: a changed state at once,
    /// other changes at most once a second.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, CaptureStatus)> {
        let mut due = Vec::new();
        for (&input, capture) in &mut self.captures {
            capture.sync_log.report(
                SourceKind::Capture,
                input,
                capture.supervisor.sync_telemetry().as_ref(),
                now,
            );
            let status = capture.supervisor.status().clone();
            let report = match &capture.reported {
                None => true,
                Some((reported, at)) => {
                    reported.state != status.state
                        || reported.reason != status.reason
                        || reported.device != status.device
                        || reported.format != status.format
                        || reported.last_error != status.last_error
                        || (*reported != status
                            && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
                }
            };
            if !report {
                continue;
            }
            if capture.reported.as_ref().is_none_or(|(reported, _)| {
                reported.state != status.state || reported.reason != status.reason
            }) {
                eprintln!(
                    "FREEMIXD_CAPTURE\tv=1\tinput={input}\tstate={}\treason={}\tnode={}\tformat={}\tlosses={}\trecoveries={}\trenegotiations={}",
                    status.state.token(),
                    status.reason.map_or("-", |reason| reason.token()),
                    status
                        .device
                        .as_deref()
                        .map_or_else(|| "-".to_owned(), record_text),
                    status
                        .format
                        .map_or_else(|| "-".to_owned(), |format| format.token()),
                    status.losses,
                    status.recoveries,
                    status.renegotiations,
                );
            }
            capture.reported = Some((status.clone(), now));
            due.push((input, status));
        }
        due
    }

    /// The device inventory, when it changed since the last call.
    pub(crate) fn inventory(&mut self) -> Option<CaptureInventory> {
        self.refresh();
        if self.reported_generation == Some(self.snapshot.generation) {
            return None;
        }
        self.reported_generation = Some(self.snapshot.generation);
        Some(self.snapshot.inventory(self.backend))
    }
}

/// Record-safe text: control characters replaced.
fn record_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect()
}
