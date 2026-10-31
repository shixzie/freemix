//! HID button-panel input (RC-005): X-keys and similar HID button panels
//! hand the console each button press.
//!
//! Linux reads `/dev/input/event*` devices whose sysfs name looks like an
//! X-keys panel (`X-keys`, `XK-`, or `xkeys`); a scanner thread lists them
//! once a second, so a panel plugged in while Studio runs is read within a
//! second, and each panel is read on its own thread. The parser keeps Linux
//! `EV_KEY` presses and releases; the console's bindings decide what a button
//! does (see `fm_ui_egui::HidBindings`). Other platforms read no panels; the
//! console then reports none.

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use eframe::egui;
use fm_ui_egui::HidInput;
#[cfg(target_os = "linux")]
use fm_ui_egui::HidTrigger;

/// Where Linux lists its input devices.
pub(crate) const HID_ROOT: &str = "/dev/input";
/// How often the panels are listed again.
#[cfg(target_os = "linux")]
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Events queued for the console before newer ones are dropped.
const EVENT_CAPACITY: usize = 1_024;
/// Linux's `EV_KEY` event type.
#[cfg(any(target_os = "linux", test))]
const EV_KEY: u16 = 1;
/// Linux `input_event` size: a 16-byte `timeval` then type, code, and value
/// on a 64-bit target, an 8-byte one on a 32-bit target.
#[cfg(any(target_os = "linux", test))]
#[cfg(target_pointer_width = "64")]
const EVENT_BYTES: usize = 24;
#[cfg(any(target_os = "linux", test))]
#[cfg(not(target_pointer_width = "64"))]
const EVENT_BYTES: usize = 16;

/// What the reader hands the console.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HidEvent {
    /// The panels read now, by name.
    Devices(Vec<String>),
    /// One button press or release.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Input(HidInput),
}

/// The running reader; dropping it stops reading the panels.
pub(crate) struct HidReader {
    events: Receiver<HidEvent>,
    stop: Arc<Mutex<bool>>,
}

impl HidReader {
    /// Starts reading the platform's HID button panels; `repaint` wakes the
    /// console when a button changes. `root` is the Linux input directory and
    /// is ignored elsewhere.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(clippy::needless_pass_by_value, unused_variables)
    )]
    pub(crate) fn start(root: PathBuf, repaint: egui::Context) -> Self {
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let stop = Arc::new(Mutex::new(false));
        let scanner_stop = Arc::clone(&stop);
        let _ = thread::Builder::new()
            .name("freemix-hid-scan".to_owned())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                scan(&root, &sender, &scanner_stop, &repaint);
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = root;
                    let _ = sender.send(HidEvent::Devices(Vec::new()));
                }
            });
        Self { events, stop }
    }

    /// The next event, if one is queued.
    pub(crate) fn try_recv(&self) -> Result<HidEvent, TryRecvError> {
        self.events.try_recv()
    }
}

impl Drop for HidReader {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// Parses whole Linux `input_event` records into `(type, code, value)`.
#[cfg(test)]
fn parse_events(bytes: &[u8]) -> Vec<(u16, u16, i32)> {
    let type_offset = EVENT_BYTES - 8;
    bytes
        .chunks_exact(EVENT_BYTES)
        .map(|record| {
            let kind = u16::from_ne_bytes([record[type_offset], record[type_offset + 1]]);
            let code = u16::from_ne_bytes([record[type_offset + 2], record[type_offset + 3]]);
            let value = i32::from_ne_bytes([
                record[type_offset + 4],
                record[type_offset + 5],
                record[type_offset + 6],
                record[type_offset + 7],
            ]);
            (kind, code, value)
        })
        .collect()
}

/// Whether a device name names an X-keys button panel.
#[cfg(target_os = "linux")]
fn is_panel(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("x-keys") || lower.contains("xkeys") || lower.contains("xk-")
}

#[cfg(target_os = "linux")]
fn scan(
    root: &std::path::Path,
    events: &SyncSender<HidEvent>,
    stop: &Arc<Mutex<bool>>,
    repaint: &egui::Context,
) {
    let mut opened = BTreeSet::new();
    let mut last_devices = Vec::new();
    while !*stop.lock().unwrap_or_else(PoisonError::into_inner) {
        let mut names = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.filter_map(Result::ok) {
                let file_name = entry.file_name();
                let Some(file_name) = file_name.to_str() else {
                    continue;
                };
                if !file_name.starts_with("event") {
                    continue;
                }
                let path = entry.path();
                let name = fs::read_to_string(format!("/sys/class/input/{file_name}/device/name"))
                    .map(|name| name.trim().to_owned())
                    .unwrap_or_else(|_| file_name.to_owned());
                if !is_panel(&name) {
                    continue;
                }
                names.push(name.clone());
                if !opened.insert(path.clone()) {
                    continue;
                }
                let events = events.clone();
                let stop = Arc::clone(stop);
                let repaint = repaint.clone();
                let _ = thread::Builder::new()
                    .name(format!("freemix-hid-{file_name}"))
                    .spawn(move || read_panel(path, name, &events, &stop, &repaint));
            }
        }
        names.sort();
        names.dedup();
        if names != last_devices {
            last_devices.clone_from(&names);
            let _ = events.try_send(HidEvent::Devices(names));
            repaint.request_repaint();
        }
        let started = Instant::now();
        while started.elapsed() < SCAN_INTERVAL {
            if *stop.lock().unwrap_or_else(PoisonError::into_inner) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "linux")]
fn read_panel(
    path: PathBuf,
    name: String,
    events: &SyncSender<HidEvent>,
    stop: &Arc<Mutex<bool>>,
    repaint: &egui::Context,
) {
    let Ok(mut file) = File::open(&path) else {
        return;
    };
    let type_offset = EVENT_BYTES - 8;
    let mut record = [0_u8; EVENT_BYTES];
    while !*stop.lock().unwrap_or_else(PoisonError::into_inner) {
        if file.read_exact(&mut record).is_err() {
            return;
        }
        let kind = u16::from_ne_bytes([record[type_offset], record[type_offset + 1]]);
        if kind != EV_KEY {
            continue;
        }
        let code = u16::from_ne_bytes([record[type_offset + 2], record[type_offset + 3]]);
        let value = i32::from_ne_bytes([
            record[type_offset + 4],
            record[type_offset + 5],
            record[type_offset + 6],
            record[type_offset + 7],
        ]);
        // 2 is an autorepeat; only the first press and the release matter.
        if value != 0 && value != 1 {
            continue;
        }
        let Some(trigger) = HidTrigger::new(name.clone(), code) else {
            continue;
        };
        if events
            .try_send(HidEvent::Input(HidInput {
                trigger,
                pressed: value == 1,
            }))
            .is_ok()
        {
            repaint.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_events_parse_type_code_and_value() {
        let mut record = vec![0_u8; EVENT_BYTES];
        let offset = EVENT_BYTES - 8;
        record[offset..offset + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        record[offset + 2..offset + 4].copy_from_slice(&59_u16.to_ne_bytes());
        record[offset + 4..offset + 8].copy_from_slice(&1_i32.to_ne_bytes());
        assert_eq!(parse_events(&record), [(EV_KEY, 59, 1)]);
        // A partial record is ignored.
        assert!(parse_events(&record[..EVENT_BYTES - 1]).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_x_keys_like_names_are_panels() {
        assert!(is_panel("X-keys Stick"));
        assert!(is_panel("XK-24"));
        assert!(is_panel("xkeys-128"));
        assert!(!is_panel("AT Translated Set 2 keyboard"));
        assert!(!is_panel("USB Optical Mouse"));
    }
}
