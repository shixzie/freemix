//! Device enumeration providers, hot-plug diffing, and the polling
//! monitor.
//!
//! A [`CaptureProvider`] enumerates the capture devices of one backend
//! (V4L2, or the fake provider of tests and diagnostics). The
//! [`CaptureMonitor`] polls it on its own thread, so enumeration never
//! blocks the session loop, and publishes a [`DeviceSnapshot`] whose
//! generation changes exactly when the device list (or the enumeration
//! error) changes: an added, removed, or changed device (new node, new
//! formats, signal change) is a new generation. [`diff`] names the
//! hot-plug events between two snapshots.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use fm_types::capture::{
    CaptureDevice, CaptureInventory, MAX_CAPTURE_DEVICES, MAX_CAPTURE_ERROR_BYTES,
    bounded_capture_text,
};

use crate::probe::{ProbedDevice, RawNode, devices_from_nodes};
use crate::sysfs::{SysfsView, paired_alsa_device};

/// Enumerates the capture devices of one backend.
pub trait CaptureProvider: Send {
    /// The backend token (`v4l2`, `fake`).
    fn backend(&self) -> &'static str;

    /// The devices present now, in stable-key order.
    ///
    /// # Errors
    ///
    /// Returns why enumeration failed as a whole.
    fn enumerate(&mut self) -> Result<Vec<ProbedDevice>, String>;
}

/// Lists and probes V4L2 nodes.
pub trait V4l2Probe: Send {
    /// The `/dev/video*` node paths.
    fn node_paths(&self) -> Vec<String>;

    /// Probes one node.
    ///
    /// # Errors
    ///
    /// Returns why the node cannot be queried.
    fn probe(&self, path: &str) -> Result<RawNode, String>;
}

#[cfg(target_os = "linux")]
impl V4l2Probe for crate::v4l2::SystemV4l2 {
    fn node_paths(&self) -> Vec<String> {
        Self::node_paths(self)
    }

    fn probe(&self, path: &str) -> Result<RawNode, String> {
        Self::probe(self, path)
    }
}

/// The V4L2 provider over a node probe and sysfs.
pub struct V4l2Provider<P, S> {
    probe: P,
    sysfs: S,
}

impl<P: V4l2Probe, S: SysfsView + Send> V4l2Provider<P, S> {
    pub const fn new(probe: P, sysfs: S) -> Self {
        Self { probe, sysfs }
    }
}

#[cfg(target_os = "linux")]
impl V4l2Provider<crate::v4l2::SystemV4l2, crate::sysfs::SystemSysfs> {
    /// The provider of this system's devices.
    #[must_use]
    pub const fn system() -> Self {
        Self::new(crate::v4l2::SystemV4l2, crate::sysfs::SystemSysfs)
    }
}

impl<P: V4l2Probe, S: SysfsView + Send> CaptureProvider for V4l2Provider<P, S> {
    fn backend(&self) -> &'static str {
        "v4l2"
    }

    fn enumerate(&mut self) -> Result<Vec<ProbedDevice>, String> {
        // A node that cannot be opened (busy for exclusive drivers, or
        // gone between listing and opening) is skipped.
        let nodes = self
            .probe
            .node_paths()
            .iter()
            .filter_map(|path| self.probe.probe(path).ok())
            .collect::<Vec<_>>();
        Ok(devices_from_nodes(&nodes, |node| {
            paired_alsa_device(&self.sysfs, &node.path)
        }))
    }
}

/// A device enumeration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceSnapshot {
    /// Changes exactly when `devices` or `error` changes.
    pub generation: u64,
    pub devices: Vec<ProbedDevice>,
    /// Why the last enumeration failed, if it did (the devices are then
    /// the last ones enumerated).
    pub error: Option<String>,
}

impl DeviceSnapshot {
    /// The wire inventory of this snapshot from `backend`.
    #[must_use]
    pub fn inventory(&self, backend: &str) -> CaptureInventory {
        CaptureInventory {
            backend: Some(backend.to_owned()),
            error: self
                .error
                .as_deref()
                .map(|error| bounded_capture_text(error, MAX_CAPTURE_ERROR_BYTES)),
            devices: self
                .devices
                .iter()
                .take(MAX_CAPTURE_DEVICES)
                .map(|device| device.info.clone())
                .collect(),
        }
    }
}

/// A hot-plug event between two enumerations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HotplugEvent {
    Added(CaptureDevice),
    Removed(CaptureDevice),
    /// Same stable key, but another node, other formats, or another
    /// signal state.
    Changed {
        before: Box<ProbedDevice>,
        after: Box<ProbedDevice>,
    },
}

/// The hot-plug events from `before` to `after`, by stable key.
#[must_use]
pub fn diff(before: &[ProbedDevice], after: &[ProbedDevice]) -> Vec<HotplugEvent> {
    let mut events = Vec::new();
    for old in before {
        match after
            .iter()
            .find(|new| new.info.stable_key == old.info.stable_key)
        {
            None => events.push(HotplugEvent::Removed(old.info.clone())),
            Some(new) if new != old => events.push(HotplugEvent::Changed {
                before: Box::new(old.clone()),
                after: Box::new(new.clone()),
            }),
            Some(_) => {}
        }
    }
    for new in after {
        if !before
            .iter()
            .any(|old| old.info.stable_key == new.info.stable_key)
        {
            events.push(HotplugEvent::Added(new.info.clone()));
        }
    }
    events
}

struct MonitorState {
    snapshot: Arc<DeviceSnapshot>,
    shutdown: bool,
    rescan: bool,
}

struct Shared {
    state: Mutex<MonitorState>,
    wake: Condvar,
}

/// Polls a provider on its own thread.
pub struct CaptureMonitor {
    backend: &'static str,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for CaptureMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureMonitor")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl CaptureMonitor {
    /// Enumerates once (so the first snapshot is complete), then polls
    /// `provider` every `interval` on a thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the thread cannot be spawned.
    pub fn spawn(
        mut provider: Box<dyn CaptureProvider>,
        interval: Duration,
    ) -> std::io::Result<Self> {
        let backend = provider.backend();
        let mut first = DeviceSnapshot {
            generation: 1,
            ..DeviceSnapshot::default()
        };
        match provider.enumerate() {
            Ok(devices) => first.devices = devices,
            Err(error) => first.error = Some(error),
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(MonitorState {
                snapshot: Arc::new(first),
                shutdown: false,
                rescan: false,
            }),
            wake: Condvar::new(),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("freemix-capture-monitor".into())
                .spawn(move || poll(provider.as_mut(), &shared, interval))?
        };
        Ok(Self {
            backend,
            shared,
            thread: Some(thread),
        })
    }

    /// The backend token.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// The latest enumeration.
    #[must_use]
    pub fn snapshot(&self) -> Arc<DeviceSnapshot> {
        Arc::clone(
            &self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .snapshot,
        )
    }

    /// Enumerates again now rather than at the next interval.
    pub fn rescan(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rescan = true;
        self.shared.wake.notify_all();
    }
}

impl Drop for CaptureMonitor {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn poll(provider: &mut dyn CaptureProvider, shared: &Shared, interval: Duration) {
    loop {
        {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state.rescan && !state.shutdown {
                state = shared
                    .wake
                    .wait_timeout(state, interval)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            if state.shutdown {
                return;
            }
            state.rescan = false;
        }
        let result = provider.enumerate();
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let current = &state.snapshot;
        let (devices, error) = match result {
            Ok(devices) => (devices, None),
            Err(error) => (current.devices.clone(), Some(error)),
        };
        if devices != current.devices || error != current.error {
            state.snapshot = Arc::new(DeviceSnapshot {
                generation: current.generation.wrapping_add(1),
                devices,
                error,
            });
        }
    }
}
