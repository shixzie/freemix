//! MIDI controller input (RC-005): hands the console each note and
//! controller message from the platform's MIDI inputs.
//!
//! Linux reads the ALSA raw MIDI ports (`/dev/snd/midiC<card>D<device>`)
//! directly, so no ALSA development package is needed. macOS and Windows use
//! `CoreMIDI` and `WinMM` through `midir`. A scanner thread lists the ports once
//! a second, so a controller plugged in while Studio runs is read within a
//! second; each port is read through [`NoteParser`]: the MIDI 1.0 note and
//! control change messages (running status and interleaved real-time bytes
//! included; Studio links no server-side crate, so not `fm-automation`'s
//! full parser). On Linux a port is named by its card's ALSA id
//! (`/proc/asound/card<n>/id`) when the system reports one. The console's
//! bindings decide what a message does (see `fm_ui_egui::MidiBindings`).

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use eframe::egui;
use fm_ui_egui::{MidiControlKind, MidiFeedback, MidiInput, MidiTrigger};

/// Where Linux lists its raw MIDI ports.
pub(crate) const MIDI_ROOT: &str = "/dev/snd";
/// How often the ports are listed again.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Messages queued for the console before newer ones are dropped.
const EVENT_CAPACITY: usize = 1_024;

/// What the reader hands the console.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MidiEvent {
    /// The ports read now, by name.
    Devices(Vec<String>),
    /// A note or controller message.
    Input(MidiInput),
}

/// The running reader; dropping it stops listing new ports.
pub(crate) struct MidiReader {
    events: Receiver<MidiEvent>,
    stop: Arc<Mutex<bool>>,
}

impl MidiReader {
    /// Starts reading the platform's MIDI ports; `repaint` wakes the console
    /// when a message arrives. `root` is the Linux raw-MIDI directory and is
    /// ignored elsewhere.
    #[cfg_attr(not(target_os = "linux"), allow(clippy::needless_pass_by_value))]
    pub(crate) fn start(root: PathBuf, repaint: egui::Context) -> Self {
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let stop = Arc::new(Mutex::new(false));
        let scanner_stop = Arc::clone(&stop);
        let _ = thread::Builder::new()
            .name("freemix-midi-scan".to_owned())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                scan(&root, &sender, &scanner_stop, &repaint);
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = root;
                    scan_midi(&sender, &scanner_stop, &repaint);
                }
            });
        Self { events, stop }
    }

    pub(crate) fn try_recv(&self) -> Result<MidiEvent, TryRecvError> {
        self.events.try_recv()
    }
}

impl Drop for MidiReader {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// The raw MIDI ports under `root`, sorted.
#[cfg(target_os = "linux")]
fn ports(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut ports: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("midiC") && name.contains('D'))
        })
        .collect();
    ports.sort();
    ports
}

/// A port's name: its card's ALSA id and the port, or the port alone.
#[cfg(target_os = "linux")]
fn port_name(path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let card = file
        .strip_prefix("midiC")
        .and_then(|rest| rest.split('D').next())
        .and_then(|card| card.parse::<u32>().ok());
    card.and_then(|card| fs::read_to_string(format!("/proc/asound/card{card}/id")).ok())
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .map_or_else(|| file.clone(), |id| format!("{id} ({file})"))
}

#[cfg(target_os = "linux")]
fn scan(
    root: &Path,
    sender: &SyncSender<MidiEvent>,
    stop: &Arc<Mutex<bool>>,
    repaint: &egui::Context,
) {
    let open: Arc<Mutex<BTreeSet<PathBuf>>> = Arc::default();
    let mut announced: Option<Vec<String>> = None;
    while !*stop.lock().unwrap_or_else(PoisonError::into_inner) {
        for port in ports(root) {
            let mut reading = open.lock().unwrap_or_else(PoisonError::into_inner);
            if reading.contains(&port) {
                continue;
            }
            let Ok(file) = File::open(&port) else {
                continue;
            };
            reading.insert(port.clone());
            let (sender, open, repaint) = (sender.clone(), Arc::clone(&open), repaint.clone());
            let spawned = thread::Builder::new()
                .name("freemix-midi-port".to_owned())
                .spawn(move || {
                    read_port(file, &sender, &repaint);
                    open.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&port);
                });
            if spawned.is_err() {
                break;
            }
        }
        let names: Vec<String> = open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|port| port_name(port))
            .collect();
        if announced.as_ref() != Some(&names) {
            if sender.send(MidiEvent::Devices(names.clone())).is_err() {
                return;
            }
            repaint.request_repaint();
            announced = Some(names);
        }
        thread::sleep(SCAN_INTERVAL);
    }
}

/// Reads one port until it ends (the controller is unplugged).
#[cfg(target_os = "linux")]
fn read_port(mut file: File, sender: &SyncSender<MidiEvent>, repaint: &egui::Context) {
    let mut parser = NoteParser::default();
    let mut buffer = [0_u8; 256];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut sent = false;
        for input in parser.feed(&buffer[..read]) {
            // A full queue drops the message rather than stalling the port.
            sent |= sender.try_send(MidiEvent::Input(input)).is_ok();
        }
        if sent {
            repaint.request_repaint();
        }
    }
}

/// `CoreMIDI` (macOS) and `WinMM` (Windows): list the inputs once a second and
/// keep one connection per input, dropping one that disappears and opening
/// one that appears.
#[cfg(not(target_os = "linux"))]
fn scan_midi(sender: &SyncSender<MidiEvent>, stop: &Arc<Mutex<bool>>, repaint: &egui::Context) {
    let mut connections: Vec<(String, midir::MidiInputConnection<()>)> = Vec::new();
    let mut announced: Option<Vec<String>> = None;
    while !*stop.lock().unwrap_or_else(PoisonError::into_inner) {
        let Ok(probe) = midir::MidiInput::new("FreeMix Studio") else {
            return;
        };
        let names: Vec<String> = probe
            .ports()
            .into_iter()
            .filter_map(|port| probe.port_name(&port).ok())
            .collect();
        // A controller unplugged and plugged back in is a new connection.
        connections.retain(|(name, _)| names.contains(name));
        for name in &names {
            if connections.iter().any(|(connected, _)| connected == name) {
                continue;
            }
            let Ok(midi) = midir::MidiInput::new("FreeMix Studio") else {
                break;
            };
            let Some(port) = midi
                .ports()
                .into_iter()
                .find(|port| midi.port_name(port).ok().as_deref() == Some(name.as_str()))
            else {
                continue;
            };
            let sender = sender.clone();
            let repaint = repaint.clone();
            let mut parser = NoteParser::default();
            let connected = midi.connect(
                &port,
                "freemix-studio",
                move |_timestamp, message, ()| {
                    let mut sent = false;
                    for input in parser.feed(message) {
                        sent |= sender.try_send(MidiEvent::Input(input)).is_ok();
                    }
                    if sent {
                        repaint.request_repaint();
                    }
                },
                (),
            );
            if let Ok(connection) = connected {
                connections.push((name.clone(), connection));
            }
        }
        if announced.as_ref() != Some(&names) {
            if sender.send(MidiEvent::Devices(names.clone())).is_err() {
                return;
            }
            repaint.request_repaint();
            announced = Some(names);
        }
        thread::sleep(SCAN_INTERVAL);
    }
    drop(connections);
}

/// One channel's selected NRPN and RPN parameter numbers.
#[derive(Clone, Copy, Debug, Default)]
struct Parameters {
    /// The 14-bit parameter number the NRPN select controllers last set.
    nrpn: u16,
    /// The 14-bit parameter number the RPN select controllers last set.
    rpn: u16,
    /// The kind the last parameter select message named.
    selected: Option<MidiControlKind>,
}

/// A MIDI 1.0 byte stream's note on, note off, and control change
/// messages: running status is kept, real-time bytes (`F8`–`FF`) pass
/// through anywhere, and every other message (`SysEx`, system common,
/// program change, pressure, pitch bend) is skipped.
///
/// An NRPN or RPN is two parameter select controllers (99/98 and 101/100)
/// followed by a data entry (6); the parser keeps each channel's selected
/// parameter and reports the data entry as a [`MidiControlKind::Nrpn`] or
/// [`MidiControlKind::Rpn`] input numbered by that 14-bit parameter. The
/// data entry's least significant byte (38) is not reported: a press is
/// the high byte crossing 64, as with any other controller.
#[derive(Debug, Default)]
pub(crate) struct NoteParser {
    /// The running channel-voice status, while one is in force.
    status: Option<u8>,
    data: [u8; 2],
    len: usize,
    /// Each channel's selected NRPN and RPN.
    parameters: [Parameters; 16],
}

impl NoteParser {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<MidiInput> {
        let mut inputs = Vec::new();
        for &byte in bytes {
            if byte >= 0xF8 {
                continue;
            }
            if byte >= 0x80 {
                // A channel-voice status starts a message; anything else
                // (SysEx and system common) cancels running status.
                self.status = (byte < 0xF0).then_some(byte);
                self.len = 0;
                continue;
            }
            let Some(status) = self.status else {
                continue;
            };
            self.data[self.len] = byte;
            self.len += 1;
            let needed = match status & 0xF0 {
                0xC0 | 0xD0 => 1,
                _ => 2,
            };
            if self.len < needed {
                continue;
            }
            self.len = 0;
            let channel = (status & 0x0F) + 1;
            let [number, value] = self.data;
            let (kind, number, value) = match status & 0xF0 {
                0x80 => (MidiControlKind::Note, u16::from(number), 0),
                0x90 => (MidiControlKind::Note, u16::from(number), value),
                0xB0 => {
                    let parameter = &mut self.parameters[usize::from(channel - 1)];
                    match number {
                        // A parameter select: 99/98 pick an NRPN, 101/100 an
                        // RPN, the higher byte first.
                        99 | 98 => {
                            let shift = if number == 99 { 7 } else { 0 };
                            let mask = if number == 99 { 0x007F } else { 0x3F80 };
                            parameter.nrpn = (parameter.nrpn & mask) | (u16::from(value) << shift);
                            parameter.selected = Some(MidiControlKind::Nrpn);
                            continue;
                        }
                        101 | 100 => {
                            let shift = if number == 101 { 7 } else { 0 };
                            let mask = if number == 101 { 0x007F } else { 0x3F80 };
                            parameter.rpn = (parameter.rpn & mask) | (u16::from(value) << shift);
                            parameter.selected = Some(MidiControlKind::Rpn);
                            continue;
                        }
                        // Data entry: the selected parameter's new value. RPN
                        // 16383 is the null parameter (nothing is selected).
                        6 => match parameter.selected {
                            Some(MidiControlKind::Nrpn) => {
                                (MidiControlKind::Nrpn, parameter.nrpn, value)
                            }
                            Some(MidiControlKind::Rpn) if parameter.rpn != 16_383 => {
                                (MidiControlKind::Rpn, parameter.rpn, value)
                            }
                            _ => continue,
                        },
                        _ => (MidiControlKind::Control, u16::from(number), value),
                    }
                }
                _ => continue,
            };
            if let Some(trigger) = MidiTrigger::new(kind, channel, number) {
                inputs.push(MidiInput { trigger, value });
            }
        }
        inputs
    }
}

/// Sends bound controls' lamp states back to the controllers (RC-005).
pub(crate) struct MidiOutput {
    feedback: SyncSender<MidiFeedback>,
    stop: Arc<Mutex<bool>>,
}

impl MidiOutput {
    /// Starts the feedback writer; controllers are listed once a second.
    pub(crate) fn start() -> Self {
        let (feedback, receiver) = mpsc::sync_channel(256);
        let stop = Arc::new(Mutex::new(false));
        let writer_stop = Arc::clone(&stop);
        let _ = thread::Builder::new()
            .name("freemix-midi-out".to_owned())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                write_linux(&receiver, &writer_stop);
                #[cfg(not(target_os = "linux"))]
                write_midir(&receiver, &writer_stop);
            });
        Self { feedback, stop }
    }

    /// Queues one lamp state; a full queue drops it rather than delaying the
    /// console.
    pub(crate) fn send(&self, feedback: MidiFeedback) {
        let _ = self.feedback.try_send(feedback);
    }
}

impl Drop for MidiOutput {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// The controller bytes that set a bound control's lamp: a note on or
/// control change, or (for an NRPN or RPN) the parameter select controllers
/// followed by the data entry.
fn feedback_bytes(feedback: MidiFeedback) -> Vec<u8> {
    let channel = (feedback.trigger.channel - 1) & 0x0F;
    let number = feedback.trigger.number.to_le_bytes()[0];
    match feedback.trigger.kind {
        MidiControlKind::Note => vec![0x90 | channel, number, feedback.value],
        MidiControlKind::Control => vec![0xB0 | channel, number, feedback.value],
        MidiControlKind::Nrpn | MidiControlKind::Rpn => {
            let (select, entry) = match feedback.trigger.kind {
                MidiControlKind::Nrpn => (99, 98),
                _ => (101, 100),
            };
            let high = (feedback.trigger.number >> 7).to_le_bytes()[0];
            let low = (feedback.trigger.number & 0x7F).to_le_bytes()[0];
            vec![
                0xB0 | channel,
                select,
                high,
                0xB0 | channel,
                entry,
                low,
                0xB0 | channel,
                6,
                feedback.value,
            ]
        }
    }
}

#[cfg(target_os = "linux")]
fn write_linux(receiver: &Receiver<MidiFeedback>, stop: &Arc<Mutex<bool>>) {
    use std::collections::BTreeMap;
    use std::time::Instant;
    let mut open: BTreeMap<PathBuf, std::fs::File> = BTreeMap::new();
    let mut scanned = Instant::now()
        .checked_sub(SCAN_INTERVAL)
        .unwrap_or_else(Instant::now);
    loop {
        if *stop.lock().unwrap_or_else(PoisonError::into_inner) {
            return;
        }
        if scanned.elapsed() >= SCAN_INTERVAL {
            scanned = Instant::now();
            let current: Vec<PathBuf> = ports(Path::new(MIDI_ROOT));
            open.retain(|path, _| current.contains(path));
            for path in current {
                if open.contains_key(&path) {
                    continue;
                }
                if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
                    open.insert(path, file);
                }
            }
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(feedback) => {
                use std::io::Write as _;
                let bytes = feedback_bytes(feedback);
                for file in open.values_mut() {
                    // A port that refuses feedback is left open: the reader
                    // still uses it, and a later write may succeed.
                    let _ = file.write_all(&bytes);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn write_midir(receiver: &Receiver<MidiFeedback>, stop: &Arc<Mutex<bool>>) {
    let mut connections: Vec<(String, midir::MidiOutputConnection)> = Vec::new();
    let mut scanned = std::time::Instant::now()
        .checked_sub(SCAN_INTERVAL)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if *stop.lock().unwrap_or_else(PoisonError::into_inner) {
            return;
        }
        if scanned.elapsed() >= SCAN_INTERVAL {
            scanned = std::time::Instant::now();
            let Ok(probe) = midir::MidiOutput::new("FreeMix Studio feedback") else {
                return;
            };
            let names: Vec<String> = probe
                .ports()
                .into_iter()
                .filter_map(|port| probe.port_name(&port).ok())
                .collect();
            connections.retain(|(name, _)| names.contains(name));
            for name in &names {
                if connections.iter().any(|(connected, _)| connected == name) {
                    continue;
                }
                let Ok(output) = midir::MidiOutput::new("FreeMix Studio feedback") else {
                    break;
                };
                let Some(port) = output
                    .ports()
                    .into_iter()
                    .find(|port| output.port_name(port).ok().as_deref() == Some(name.as_str()))
                else {
                    continue;
                };
                if let Ok(connection) = output.connect(&port, "freemix-feedback") {
                    connections.push((name.clone(), connection));
                }
            }
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(feedback) => {
                let bytes = feedback_bytes(feedback);
                connections.retain_mut(|(_, connection)| connection.send(&bytes).is_ok());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;

    #[test]
    fn feedback_bytes_are_note_on_and_control_change() {
        let note = MidiFeedback {
            trigger: MidiTrigger::new(MidiControlKind::Note, 2, 60).unwrap(),
            value: 127,
        };
        assert_eq!(feedback_bytes(note), [0x91, 60, 127]);
        let control = MidiFeedback {
            trigger: MidiTrigger::new(MidiControlKind::Control, 16, 7).unwrap(),
            value: 0,
        };
        assert_eq!(feedback_bytes(control), [0xBF, 7, 0]);
    }

    #[test]
    fn feedback_bytes_select_a_parameter_before_its_value() {
        let nrpn = MidiFeedback {
            trigger: MidiTrigger::new(MidiControlKind::Nrpn, 3, 0x1234).unwrap(),
            value: 64,
        };
        assert_eq!(
            feedback_bytes(nrpn),
            [0xB2, 99, 0x24, 0xB2, 98, 0x34, 0xB2, 6, 64]
        );
        let rpn = MidiFeedback {
            trigger: MidiTrigger::new(MidiControlKind::Rpn, 1, 0).unwrap(),
            value: 127,
        };
        assert_eq!(
            feedback_bytes(rpn),
            [0xB0, 101, 0, 0xB0, 100, 0, 0xB0, 6, 127]
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::time::Instant;

    use super::*;

    /// A FIFO stands in for a controller's port: the reader lists it and
    /// reads its note and controller messages (running status too). (A FIFO
    /// cannot stand in for an unplug: reopening one waits for a writer.)
    #[test]
    fn a_port_is_listed_and_read() {
        let directory =
            std::env::temp_dir().join(format!("freemix-midi-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let port = directory.join("midiC0D0");
        let made = std::process::Command::new("mkfifo")
            .arg(&port)
            .status()
            .unwrap();
        assert!(made.success());
        let reader = MidiReader::start(directory.clone(), egui::Context::default());
        // The writer keeps the port open until the listing was seen: a port
        // is listed while it is read, and closing it right after the write
        // ends its reader before the scanner lists it.
        let (release, released) = mpsc::channel::<()>();
        let writer = thread::spawn({
            let port = port.clone();
            move || {
                let mut fifo = fs::OpenOptions::new().write(true).open(&port).unwrap();
                // Note on 60, then (running status) note 62 at velocity 0,
                // then controller 7 = 127 on channel 2.
                fifo.write_all(&[0x90, 60, 100, 62, 0, 0xB1, 7, 127])
                    .unwrap();
                let _ = released.recv();
            }
        });
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match reader.try_recv() {
                Ok(event) => events.push(event),
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
            let inputs = events
                .iter()
                .filter(|event| matches!(event, MidiEvent::Input(_)))
                .count();
            let listed = events
                .iter()
                .any(|event| matches!(event, MidiEvent::Devices(names) if names.len() == 1));
            if inputs == 3 && listed {
                break;
            }
        }
        drop(release);
        writer.join().unwrap();
        let input = |kind, channel, number, value| {
            MidiEvent::Input(MidiInput {
                trigger: MidiTrigger::new(kind, channel, number).unwrap(),
                value,
            })
        };
        let inputs: Vec<&MidiEvent> = events
            .iter()
            .filter(|event| matches!(event, MidiEvent::Input(_)))
            .collect();
        assert_eq!(
            inputs,
            [
                &input(MidiControlKind::Note, 1, 60, 100),
                &input(MidiControlKind::Note, 1, 62, 0),
                &input(MidiControlKind::Control, 2, 7, 127),
            ],
            "{events:?}"
        );
        // Listed by its card's ALSA id when this machine has a card 0.
        assert!(
            events.iter().any(|event| matches!(
                event,
                MidiEvent::Devices(names) if names.len() == 1 && names[0].contains("midiC0D0")
            )),
            "{events:?}"
        );
        drop(reader);
        let _ = fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn only_notes_and_controllers_reach_the_console() {
        let input = |kind, channel, number, value| MidiInput {
            trigger: MidiTrigger::new(kind, channel, number).unwrap(),
            value,
        };
        let mut parser = NoteParser::default();
        // Program change, note off on channel 16, a clock byte inside a
        // running-status note on, SysEx, then a data byte with no status.
        let inputs = parser.feed(&[
            0xC0, 5, 0x8F, 10, 64, 0x90, 60, 0xF8, 100, 61, 0, 0xF0, 1, 2, 0xF7, 7,
        ]);
        assert_eq!(
            inputs,
            [
                input(MidiControlKind::Note, 16, 10, 0),
                input(MidiControlKind::Note, 1, 60, 100),
                input(MidiControlKind::Note, 1, 61, 0),
            ]
        );
        // A message split across reads completes on the next one.
        assert_eq!(parser.feed(&[0xB2, 7]), Vec::new());
        assert_eq!(
            parser.feed(&[127]),
            [input(MidiControlKind::Control, 3, 7, 127)]
        );
    }

    #[test]
    fn parameter_selects_number_the_data_entry() {
        let input = |kind, channel, number: u16, value| MidiInput {
            trigger: MidiTrigger::new(kind, channel, number).unwrap(),
            value,
        };
        let mut parser = NoteParser::default();
        // NRPN 0x1234 on channel 1, then data entry 100.
        assert_eq!(
            parser.feed(&[0xB0, 99, 0x24, 0xB0, 98, 0x34, 0xB0, 6, 100]),
            [input(MidiControlKind::Nrpn, 1, 0x1234, 100)]
        );
        // The selected parameter stays until another select; a data entry
        // below 64 releases it.
        assert_eq!(
            parser.feed(&[0xB0, 6, 10]),
            [input(MidiControlKind::Nrpn, 1, 0x1234, 10)]
        );
        // RPN 0 on another channel; selects are per channel.
        assert_eq!(
            parser.feed(&[0xB1, 101, 0, 0xB1, 100, 0, 0xB1, 6, 127]),
            [input(MidiControlKind::Rpn, 2, 0, 127)]
        );
        assert_eq!(
            parser.feed(&[0xB0, 6, 64]),
            [input(MidiControlKind::Nrpn, 1, 0x1234, 64)]
        );
        // RPN 16383 is the null parameter: its data entry is ignored.
        assert_eq!(
            parser.feed(&[0xB1, 101, 127, 0xB1, 100, 127, 0xB1, 6, 100]),
            Vec::new()
        );
        // A data entry with no parameter selected yet is not a parameter.
        let mut fresh = NoteParser::default();
        assert_eq!(fresh.feed(&[0xB0, 6, 100]), Vec::new());
        assert_eq!(
            fresh.feed(&[0xB0, 7, 100]),
            [input(MidiControlKind::Control, 1, 7, 100)]
        );
    }
}
