//! A deterministic fake capture backend for tests and diagnostics.
//!
//! A [`FakeCaptureWorld`] holds fake devices: their identity and formats,
//! the colour their pictures show, and whether they are stalled or report
//! no signal. [`FakeProvider`] enumerates them and [`FakeAcquirer`]
//! acquires them: each pull of a live, unstalled device yields one solid
//! picture at the program size; an unplugged device fails its acquisition.
//!
//! A world may follow an inventory file ([`FakeCaptureWorld::from_file`]),
//! re-read at every enumeration, so a process test rewrites the file to
//! plug, unplug, stall, or change a device. One device per line, fields
//! separated by tabs (shown as `<TAB>`), each `name=value`:
//!
//! ```text
//! key=v4l2:usb-1#Fake Cam<TAB>name=Fake Cam<TAB>path=/dev/video0<TAB>formats=1280x720@30/1:yuyv:p,1920x1080@30/1:mjpeg:p<TAB>rgba=ff0000ff
//! ```
//!
//! `key`, `path`, and `formats` are required; `name` defaults to the key's
//! card, `driver` to `fake`, `bus` to the key's bus, `rgba` to opaque grey;
//! `audio=<alsa device>`, `stalled=1`, and `no_signal=1` are optional.
//! Blank lines and lines starting with `#` are ignored. A missing file
//! enumerates no devices.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use fm_types::capture::{CaptureDevice, CaptureFormat};

use crate::acquire::{AcquireRequest, AcquiredFrame, Acquirer, Acquisition, AcquisitionHealth};
use crate::monitor::CaptureProvider;
use crate::probe::{ProbedDevice, V4l2Key};

/// One fake device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeDevice {
    pub info: CaptureDevice,
    pub rgba: [u8; 4],
    /// The device is present but delivers no pictures.
    pub stalled: bool,
    pub no_signal: bool,
}

#[derive(Debug, Default)]
struct WorldState {
    devices: Vec<FakeDevice>,
    /// Node paths with a running acquisition, in start order.
    acquiring: Vec<String>,
    starts: u64,
}

/// The fake devices shared by a [`FakeProvider`] and a [`FakeAcquirer`].
#[derive(Debug, Default)]
pub struct FakeCaptureWorld {
    state: Mutex<WorldState>,
    file: Option<PathBuf>,
}

impl FakeCaptureWorld {
    /// A world without devices.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A world following the inventory file at `path`.
    #[must_use]
    pub fn from_file(path: PathBuf) -> Arc<Self> {
        let world = Self {
            state: Mutex::default(),
            file: Some(path),
        };
        let _ = world.reload();
        Arc::new(world)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorldState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the devices.
    pub fn set_devices(&self, devices: Vec<FakeDevice>) {
        self.lock().devices = devices;
    }

    /// Changes the device with `key`.
    pub fn update(&self, key: &str, change: impl FnOnce(&mut FakeDevice)) {
        if let Some(device) = self
            .lock()
            .devices
            .iter_mut()
            .find(|device| device.info.stable_key == key)
        {
            change(device);
        }
    }

    /// Removes the device with `key` (an unplug).
    pub fn unplug(&self, key: &str) {
        self.lock()
            .devices
            .retain(|device| device.info.stable_key != key);
    }

    /// Adds a device (a plug).
    pub fn plug(&self, device: FakeDevice) {
        let mut state = self.lock();
        state
            .devices
            .retain(|existing| existing.info.stable_key != device.info.stable_key);
        state.devices.push(device);
    }

    /// The node paths acquired now.
    #[must_use]
    pub fn acquiring(&self) -> Vec<String> {
        self.lock().acquiring.clone()
    }

    /// Acquisitions started so far.
    #[must_use]
    pub fn starts(&self) -> u64 {
        self.lock().starts
    }

    /// Re-reads the inventory file, if the world follows one.
    ///
    /// # Errors
    ///
    /// Returns why the file is malformed (the devices are then kept).
    pub fn reload(&self) -> Result<(), String> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        let devices = match std::fs::read_to_string(path) {
            Ok(text) => parse_inventory(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        self.set_devices(devices);
        Ok(())
    }

    fn device_at(&self, path: &str) -> Option<FakeDevice> {
        self.lock()
            .devices
            .iter()
            .find(|device| device.info.path == path)
            .cloned()
    }
}

/// Parses a fake inventory file (see the module documentation).
///
/// # Errors
///
/// Returns the first malformed line.
pub fn parse_inventory(text: &str) -> Result<Vec<FakeDevice>, String> {
    let mut devices = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("fake capture inventory line {}: {what}", number + 1);
        let mut key = None;
        let mut name = None;
        let mut path = None;
        let mut driver = "fake".to_owned();
        let mut bus = None;
        let mut audio = None;
        let mut formats = None;
        let mut rgba = [128, 128, 128, 255];
        let mut stalled = false;
        let mut no_signal = false;
        for field in line.split('\t') {
            let (field, value) = field
                .split_once('=')
                .ok_or_else(|| bad(&format!("field `{field}` has no value")))?;
            let flag = || match value {
                "0" => Ok(false),
                "1" => Ok(true),
                _ => Err(bad(&format!("`{field}` must be 0 or 1"))),
            };
            match field {
                "key" => key = Some(value.to_owned()),
                "name" => name = Some(value.to_owned()),
                "path" => path = Some(value.to_owned()),
                "driver" => value.clone_into(&mut driver),
                "bus" => bus = Some(value.to_owned()),
                "audio" => audio = Some(value.to_owned()),
                "formats" => {
                    formats = Some(
                        value
                            .split(',')
                            .map(|token| {
                                CaptureFormat::from_token(token)
                                    .map_err(|_| bad(&format!("bad format `{token}`")))
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
                "rgba" => {
                    let parsed = u32::from_str_radix(value, 16)
                        .ok()
                        .filter(|_| value.len() == 8)
                        .ok_or_else(|| bad("`rgba` must be 8 hex digits"))?;
                    rgba = parsed.to_be_bytes();
                }
                "stalled" => stalled = flag()?,
                "no_signal" => no_signal = flag()?,
                _ => return Err(bad(&format!("unknown field `{field}`"))),
            }
        }
        let key = key.ok_or_else(|| bad("missing `key`"))?;
        let parsed = V4l2Key::parse(&key);
        devices.push(FakeDevice {
            info: CaptureDevice {
                name: name
                    .or_else(|| parsed.as_ref().map(|key| key.card.clone()))
                    .unwrap_or_else(|| key.clone()),
                bus: bus
                    .or_else(|| parsed.as_ref().map(|key| key.bus.clone()))
                    .unwrap_or_default(),
                stable_key: key,
                path: path.ok_or_else(|| bad("missing `path`"))?,
                driver,
                audio,
                formats: formats.ok_or_else(|| bad("missing `formats`"))?,
            },
            rgba,
            stalled,
            no_signal,
        });
    }
    devices.sort_by(|a, b| a.info.stable_key.cmp(&b.info.stable_key));
    Ok(devices)
}

/// Enumerates a fake world.
#[derive(Debug)]
pub struct FakeProvider {
    world: Arc<FakeCaptureWorld>,
}

impl FakeProvider {
    #[must_use]
    pub const fn new(world: Arc<FakeCaptureWorld>) -> Self {
        Self { world }
    }
}

impl CaptureProvider for FakeProvider {
    fn backend(&self) -> &'static str {
        "fake"
    }

    fn enumerate(&mut self) -> Result<Vec<ProbedDevice>, String> {
        self.world.reload()?;
        let mut devices = self
            .world
            .lock()
            .devices
            .iter()
            .map(|device| ProbedDevice {
                info: device.info.clone(),
                no_signal: device.no_signal,
            })
            .collect::<Vec<_>>();
        devices.sort_by(|a, b| a.info.stable_key.cmp(&b.info.stable_key));
        Ok(devices)
    }
}

/// Acquires fake devices.
#[derive(Debug)]
pub struct FakeAcquirer {
    world: Arc<FakeCaptureWorld>,
}

impl FakeAcquirer {
    #[must_use]
    pub const fn new(world: Arc<FakeCaptureWorld>) -> Self {
        Self { world }
    }
}

impl Acquirer for FakeAcquirer {
    fn start(&self, request: &AcquireRequest) -> Result<Box<dyn Acquisition>, String> {
        let path = request.device.path.clone();
        if self.world.device_at(&path).is_none() {
            return Err(format!("{path}: No such device"));
        }
        let bytes = usize::try_from(u64::from(request.width) * u64::from(request.height) * 4)
            .map_err(|_| "picture too large".to_owned())?;
        {
            let mut state = self.world.lock();
            state.acquiring.push(path.clone());
            state.starts = state.starts.saturating_add(1);
        }
        Ok(Box::new(FakeAcquisition {
            world: Arc::clone(&self.world),
            path,
            bytes,
            delivered: false,
            failed: None,
        }))
    }
}

struct FakeAcquisition {
    world: Arc<FakeCaptureWorld>,
    path: String,
    bytes: usize,
    delivered: bool,
    failed: Option<String>,
}

impl Acquisition for FakeAcquisition {
    fn pull(&mut self) -> AcquiredFrame {
        if self.failed.is_some() {
            return AcquiredFrame::default();
        }
        let Some(device) = self.world.device_at(&self.path) else {
            self.failed = Some(format!("{}: No such device", self.path));
            return AcquiredFrame::default();
        };
        if device.stalled || self.bytes == 0 {
            return AcquiredFrame::default();
        }
        self.delivered = true;
        let mut picture = vec![0_u8; self.bytes];
        for pixel in picture.as_chunks_mut::<4>().0 {
            *pixel = device.rgba;
        }
        AcquiredFrame {
            picture: Some(picture),
            audio: None,
        }
    }

    fn health(&self) -> AcquisitionHealth {
        match &self.failed {
            Some(error) => AcquisitionHealth::Failed(error.clone()),
            None if self.delivered => AcquisitionHealth::Live,
            None => AcquisitionHealth::Starting,
        }
    }

    fn audio_active(&self) -> bool {
        false
    }

    fn last_error(&self) -> Option<String> {
        self.failed.clone()
    }
}

impl Drop for FakeAcquisition {
    fn drop(&mut self) {
        let mut state = self.world.lock();
        if let Some(index) = state.acquiring.iter().position(|path| *path == self.path) {
            state.acquiring.remove(index);
        }
    }
}
