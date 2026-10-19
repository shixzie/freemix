//! Lists the server's system audio capture devices (AU-011, Protocol 2.68).
//!
//! [`SystemAudioLister::list`] asks both audio systems a system audio
//! input can open:
//!
//! - `PulseAudio` (or `PipeWire`'s `pipewire-pulse`): `pactl list sources`
//!   for every source with its description, channel count, and whether it
//!   is a sink's monitor, and `pactl info` for the default source. `pactl`
//!   runs with the C locale and only the audio-server environment the
//!   capture child keeps (`PATH`, `HOME`, `XDG_RUNTIME_DIR`, `PULSE_*`), and
//!   is killed when it does not answer within a bound.
//! - ALSA: `/proc/asound/cards` and `/proc/asound/pcm` for every PCM with a
//!   capture stream, named `plughw:<card id>,<device>` so the device
//!   converts to the rate and channels the capture asks for.
//!
//! An audio system that cannot be listed is reported with why, next to the
//! devices the other offers. Listing blocks for up to the `pactl` bound, so
//! a caller runs it off any frame loop. The parsers are pure and portable.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fm_types::system_audio::{
    SystemAudioBackend, SystemAudioBackendError, SystemAudioDevice, SystemAudioInventory,
};

/// Most bytes of `pactl` output read; a larger answer is cut off.
const MAX_PACTL_OUTPUT_BYTES: u64 = 1024 * 1024;

/// The environment `pactl` keeps besides `PATH`, matching the system audio
/// capture child: `PulseAudio` finds its server and cookie through these.
const AUDIO_SERVER_ENVIRONMENT: &[&str] = &[
    "HOME",
    "XDG_RUNTIME_DIR",
    "PULSE_SERVER",
    "PULSE_COOKIE",
    "PULSE_RUNTIME_PATH",
];

/// Lists the capture devices of `PulseAudio` and ALSA.
#[derive(Clone, Debug)]
pub struct SystemAudioLister {
    /// The `pactl` program, found on `PATH` by default.
    pub pactl: OsString,
    /// The ALSA procfs directory, `/proc/asound` by default.
    pub asound: PathBuf,
    /// The longest one `pactl` call may take.
    pub timeout: Duration,
}

impl Default for SystemAudioLister {
    fn default() -> Self {
        Self {
            pactl: OsString::from("pactl"),
            asound: PathBuf::from("/proc/asound"),
            timeout: Duration::from_secs(2),
        }
    }
}

impl SystemAudioLister {
    /// Lists every capture device, fitted for the wire
    /// ([`SystemAudioInventory::fitted`]).
    #[must_use]
    pub fn list(&self) -> SystemAudioInventory {
        let mut devices = Vec::new();
        let mut errors = Vec::new();
        for (backend, listed) in [
            (SystemAudioBackend::Pulse, self.pulse()),
            (SystemAudioBackend::Alsa, self.alsa()),
        ] {
            match listed {
                Ok(listed) => devices.extend(listed),
                Err(error) => errors.push(SystemAudioBackendError { backend, error }),
            }
        }
        SystemAudioInventory {
            supported: true,
            devices,
            errors,
        }
        .fitted()
    }

    fn pulse(&self) -> Result<Vec<SystemAudioDevice>, String> {
        let sources = run_pactl(&self.pactl, &["list", "sources"], self.timeout)?;
        // Without the server's defaults the sources are still listed.
        let default = run_pactl(&self.pactl, &["info"], self.timeout)
            .ok()
            .and_then(|info| parse_pactl_default_source(&info));
        let mut devices = parse_pactl_sources(&sources);
        for device in &mut devices {
            device.default = default.as_deref() == Some(device.device.as_str());
        }
        Ok(devices)
    }

    fn alsa(&self) -> Result<Vec<SystemAudioDevice>, String> {
        let cards = match std::fs::read_to_string(self.asound.join("cards")) {
            Ok(cards) => cards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("no ALSA driver is loaded (no /proc/asound)".to_owned());
            }
            Err(error) => return Err(format!("cannot read the ALSA cards: {error}")),
        };
        // A system without PCMs has no `pcm` file.
        let pcm = std::fs::read_to_string(self.asound.join("pcm")).unwrap_or_default();
        Ok(parse_alsa(&cards, &pcm))
    }
}

/// Runs `pactl` with `args`, returning its standard output, or why it
/// failed: not installed, no server, or no answer within `timeout`.
fn run_pactl(program: &OsString, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    for name in AUDIO_SERVER_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "pactl is not installed".to_owned()
        } else {
            format!("pactl did not start: {error}")
        }
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_PACTL_OUTPUT_BYTES).read_to_end(&mut text);
            }
            String::from_utf8_lossy(&text).into_owned()
        })
    };
    let stdout = read(stdout.map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = read(stderr.map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "pactl did not answer within {} ms",
                    timeout.as_millis()
                ));
            }
            Err(error) => return Err(format!("pactl failed: {error}")),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        let reason = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map_or_else(|| status.to_string(), str::to_owned);
        Err(format!("pactl failed: {reason}"))
    }
}

/// Parses `pactl list sources` (C locale): one device per `Source #`
/// block, named by its `Name`, described by its `Description`, with the
/// channels of its `Sample Specification` and whether it is a sink's
/// monitor. Never marks a default.
#[must_use]
pub fn parse_pactl_sources(text: &str) -> Vec<SystemAudioDevice> {
    let mut devices = Vec::new();
    let mut current: Option<SystemAudioDevice> = None;
    for line in text.lines() {
        if line.starts_with("Source #") {
            devices.extend(current.take().filter(|device| !device.device.is_empty()));
            current = Some(SystemAudioDevice {
                backend: SystemAudioBackend::Pulse,
                ..SystemAudioDevice::default()
            });
            continue;
        }
        // Only the block's own keys sit one tab in.
        let (Some(device), Some(entry)) = (current.as_mut(), line.strip_prefix('\t')) else {
            continue;
        };
        if entry.starts_with(['\t', ' ']) {
            continue;
        }
        let Some((key, value)) = entry.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Name" => value.clone_into(&mut device.device),
            "Description" => value.clone_into(&mut device.description),
            "Sample Specification" => {
                device.channels = value
                    .split_whitespace()
                    .find_map(|word| word.strip_suffix("ch")?.parse().ok());
            }
            "Monitor of Sink" => device.monitor = value != "n/a",
            _ => {}
        }
    }
    devices.extend(current.filter(|device| !device.device.is_empty()));
    for device in &mut devices {
        device.monitor |= device.device.ends_with(".monitor");
    }
    devices
}

/// The `Default Source` of `pactl info` (C locale).
#[must_use]
pub fn parse_pactl_default_source(info: &str) -> Option<String> {
    info.lines()
        .find_map(|line| line.strip_prefix("Default Source:"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Parses `/proc/asound/cards` and `/proc/asound/pcm` into one
/// `plughw:<card id>,<device>` device per PCM with a capture stream,
/// described as `<card name>: <PCM name>`.
#[must_use]
pub fn parse_alsa(cards: &str, pcm: &str) -> Vec<SystemAudioDevice> {
    // ` 0 [PCH            ]: HDA-Intel - HDA Intel PCH`
    let mut card_names = BTreeMap::new();
    for line in cards.lines() {
        let Some((number, rest)) = line.trim_start().split_once(" [") else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        let Some((id, rest)) = rest.split_once(']') else {
            continue;
        };
        let name = rest
            .trim_start_matches(':')
            .split_once(" - ")
            .map_or(rest, |(_, name)| name)
            .trim();
        card_names.insert(number, (id.trim().to_owned(), name.to_owned()));
    }
    // `00-00: ALC892 Analog : ALC892 Analog : playback 1 : capture 1`
    let mut devices = Vec::new();
    for line in pcm.lines() {
        let parts: Vec<&str> = line.split(" : ").map(str::trim).collect();
        if !parts.iter().any(|part| part.starts_with("capture ")) {
            continue;
        }
        let Some((address, _)) = parts.first().and_then(|first| first.split_once(':')) else {
            continue;
        };
        let Some((card, device)) = address.split_once('-') else {
            continue;
        };
        let (Ok(card), Ok(device)) = (card.parse::<u32>(), device.parse::<u32>()) else {
            continue;
        };
        let Some((id, card_name)) = card_names.get(&card) else {
            continue;
        };
        let pcm_name = parts.get(1).copied().unwrap_or_default();
        devices.push(SystemAudioDevice {
            backend: SystemAudioBackend::Alsa,
            device: format!("plughw:{id},{device}"),
            description: if pcm_name.is_empty() {
                card_name.clone()
            } else {
                format!("{card_name}: {pcm_name}")
            },
            channels: None,
            default: false,
            monitor: false,
        });
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &str = "Source #53\n\
\tState: SUSPENDED\n\
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo.monitor\n\
\tDescription: Monitor of Built-in Audio Analog Stereo\n\
\tDriver: PipeWire\n\
\tSample Specification: s32le 2ch 48000Hz\n\
\tChannel Map: front-left,front-right\n\
\tVolume: front-left: 65536 / 100% / 0.00 dB\n\
\t        balance 0.00\n\
\tMonitor of Sink: alsa_output.pci-0000_00_1f.3.analog-stereo\n\
\tProperties:\n\
\t\tdevice.description = \"Built-in Audio\"\n\
\t\tName: not-a-key\n\
\n\
Source #54\n\
\tState: RUNNING\n\
\tName: alsa_input.usb-Blue_Yeti-00.mono-fallback\n\
\tDescription: Yeti Stereo Microphone Mono\n\
\tSample Specification: s16le 1ch 48000Hz\n\
\tMonitor of Sink: n/a\n";

    #[test]
    fn pactl_sources_are_parsed_with_channels_and_monitors() {
        let devices = parse_pactl_sources(SOURCES);
        assert_eq!(
            devices,
            vec![
                SystemAudioDevice {
                    backend: SystemAudioBackend::Pulse,
                    device: "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor".into(),
                    description: "Monitor of Built-in Audio Analog Stereo".into(),
                    channels: Some(2),
                    default: false,
                    monitor: true,
                },
                SystemAudioDevice {
                    backend: SystemAudioBackend::Pulse,
                    device: "alsa_input.usb-Blue_Yeti-00.mono-fallback".into(),
                    description: "Yeti Stereo Microphone Mono".into(),
                    channels: Some(1),
                    default: false,
                    monitor: false,
                },
            ]
        );
        assert_eq!(parse_pactl_sources(""), Vec::<SystemAudioDevice>::new());
        assert_eq!(
            parse_pactl_sources("Source #1\n\tState: IDLE\n"),
            Vec::<SystemAudioDevice>::new()
        );
    }

    #[test]
    fn pactl_info_names_the_default_source() {
        let info = "Server String: /run/user/1000/pulse/native\n\
                    Default Sink: alsa_output.pci.analog-stereo\n\
                    Default Source: alsa_input.usb-Blue_Yeti-00.mono-fallback\n";
        assert_eq!(
            parse_pactl_default_source(info).as_deref(),
            Some("alsa_input.usb-Blue_Yeti-00.mono-fallback")
        );
        assert_eq!(parse_pactl_default_source("Default Source: \n"), None);
    }

    #[test]
    fn alsa_capture_pcms_are_listed_by_card_id() {
        let cards = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n\
                     \x20                     HDA Intel PCH at 0xf7f10000 irq 31\n\
                     \x201 [Loopback       ]: Loopback - Loopback\n\
                     \x20                     Loopback 1\n";
        let pcm = "00-00: ALC892 Analog : ALC892 Analog : playback 1 : capture 1\n\
                   00-03: HDMI 0 : HDMI 0 : playback 1\n\
                   01-01: Loopback PCM : Loopback PCM : playback 8 : capture 8\n\
                   07-00: Ghost : Ghost : capture 1\n";
        let devices = parse_alsa(cards, pcm);
        let named: Vec<(&str, &str)> = devices
            .iter()
            .map(|device| (device.device.as_str(), device.description.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("plughw:PCH,0", "HDA Intel PCH: ALC892 Analog"),
                ("plughw:Loopback,1", "Loopback: Loopback PCM"),
            ]
        );
        assert!(
            devices
                .iter()
                .all(|device| device.backend == SystemAudioBackend::Alsa)
        );
        assert_eq!(
            parse_alsa("--- no soundcards ---\n", ""),
            Vec::<SystemAudioDevice>::new()
        );
    }

    /// A `pactl` that answers, fails, or hangs: its sources are listed with
    /// the default marked, its failure is reported, and a hang is cut off.
    #[cfg(unix)]
    #[test]
    fn pactl_is_run_bounded_with_the_c_locale() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("fm-io-linux-pactl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.into_os_string()
        };
        std::fs::write(dir.join("sources.txt"), SOURCES).unwrap();
        // A script just written can be briefly busy to exec: another test
        // thread forking in that window inherits its write descriptor until
        // the fork execs (ETXTBSY), so a busy start is tried again.
        let list = |lister: &SystemAudioLister| {
            for _ in 0..200 {
                let inventory = lister.list();
                let busy = inventory
                    .error_of(SystemAudioBackend::Pulse)
                    .is_some_and(|error| error.contains("Text file busy"));
                if !busy {
                    return inventory;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the fake pactl stayed busy");
        };
        let answering = script(
            "pactl-ok",
            &format!(
                "[ \"$LC_ALL\" = C ] || exit 3\n\
                 if [ \"$1\" = info ]; then echo 'Default Source: \
                 alsa_input.usb-Blue_Yeti-00.mono-fallback'; else cat '{}'; fi",
                dir.join("sources.txt").display()
            ),
        );
        let lister = SystemAudioLister {
            pactl: answering,
            asound: dir.join("no-asound"),
            timeout: Duration::from_secs(5),
        };
        let inventory = list(&lister);
        let pulse: Vec<(&str, bool)> = inventory
            .devices_of(SystemAudioBackend::Pulse)
            .map(|device| (device.device.as_str(), device.default))
            .collect();
        assert_eq!(
            pulse,
            [
                ("alsa_input.usb-Blue_Yeti-00.mono-fallback", true),
                ("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor", false),
            ]
        );
        assert_eq!(inventory.error_of(SystemAudioBackend::Pulse), None);

        let refused = SystemAudioLister {
            pactl: script(
                "pactl-refused",
                "echo 'Connection failure: Connection refused' >&2; exit 1",
            ),
            ..lister.clone()
        };
        assert_eq!(
            list(&refused).error_of(SystemAudioBackend::Pulse),
            Some("pactl failed: Connection failure: Connection refused")
        );
        let hanging = SystemAudioLister {
            pactl: script("pactl-hang", "exec sleep 30"),
            timeout: Duration::from_millis(200),
            ..lister
        };
        let started = Instant::now();
        assert_eq!(
            list(&hanging).error_of(SystemAudioBackend::Pulse),
            Some("pactl did not answer within 200 ms")
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_audio_system_is_reported_next_to_the_other() {
        let asound = std::env::temp_dir().join(format!(
            "fm-io-linux-asound-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&asound).unwrap();
        std::fs::write(
            asound.join("cards"),
            " 1 [Loopback       ]: Loopback - Loopback\n",
        )
        .unwrap();
        std::fs::write(
            asound.join("pcm"),
            "01-01: Loopback PCM : Loopback PCM : playback 8 : capture 8\n",
        )
        .unwrap();
        let lister = SystemAudioLister {
            pactl: OsString::from("fm-io-linux-no-such-pactl"),
            asound: asound.clone(),
            timeout: Duration::from_secs(2),
        };
        let inventory = lister.list();
        std::fs::remove_dir_all(&asound).unwrap();
        assert!(inventory.supported);
        assert_eq!(
            inventory.error_of(SystemAudioBackend::Pulse),
            Some("pactl is not installed")
        );
        assert_eq!(inventory.devices.len(), 1);
        assert_eq!(inventory.devices[0].device, "plughw:Loopback,1");
        let none = SystemAudioLister {
            asound: asound.join("missing"),
            ..lister
        };
        assert_eq!(
            none.list().error_of(SystemAudioBackend::Alsa),
            Some("no ALSA driver is loaded (no /proc/asound)")
        );
    }
}
