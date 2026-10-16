//! Pairing a V4L2 node with the ALSA capture device of the same hardware.
//!
//! A USB camera exposes its pictures and its microphone as separate
//! interfaces of one USB device: `/sys/class/video4linux/video0/device`
//! resolves to `.../usb1/1-4/1-4:1.0` and `/sys/class/sound/card2/device`
//! to `.../usb1/1-4/1-4:1.2`. A `PCIe` capture card exposes both on the same
//! PCI function. [`paired_alsa_device`] strips the USB interface component
//! from both paths and pairs a node with the first card whose device is the
//! same, if that card has a capture PCM: `hw:CARD=<id>,DEV=<n>` with the
//! lowest capture PCM number `n`.
//!
//! Sysfs access goes through [`SysfsView`] so the pairing is tested from
//! fixtures.

use std::collections::BTreeMap;

/// Read-only access to sysfs.
pub trait SysfsView {
    /// The canonical target of `path` (following symbolic links).
    fn canonical(&self, path: &str) -> Option<String>;
    /// The trimmed text content of the file at `path`.
    fn read(&self, path: &str) -> Option<String>;
    /// The entry names of the directory at `path`.
    fn entries(&self, path: &str) -> Vec<String>;
}

/// The real sysfs.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSysfs;

impl SysfsView for SystemSysfs {
    fn canonical(&self, path: &str) -> Option<String> {
        std::fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_owned())
    }

    fn entries(&self, path: &str) -> Vec<String> {
        let mut names = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// A sysfs fixture: symbolic link targets, file contents, and directory
/// entries by path.
#[derive(Clone, Debug, Default)]
pub struct FixtureSysfs {
    pub links: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
    pub dirs: BTreeMap<String, Vec<String>>,
}

impl SysfsView for FixtureSysfs {
    fn canonical(&self, path: &str) -> Option<String> {
        self.links.get(path).cloned()
    }

    fn read(&self, path: &str) -> Option<String> {
        self.files.get(path).map(|text| text.trim().to_owned())
    }

    fn entries(&self, path: &str) -> Vec<String> {
        self.dirs.get(path).cloned().unwrap_or_default()
    }
}

/// `path` without a trailing USB interface component (`1-4:1.0`).
fn hardware_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((parent, last)) if is_usb_interface(last) => parent,
        _ => path,
    }
}

fn is_usb_interface(name: &str) -> bool {
    let Some((port, config)) = name.split_once(':') else {
        return false;
    };
    let Some((bus, ports)) = port.split_once('-') else {
        return false;
    };
    !bus.is_empty()
        && bus.bytes().all(|byte| byte.is_ascii_digit())
        && !ports.is_empty()
        && ports
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && config
            .split_once('.')
            .is_some_and(|(configuration, interface)| {
                !configuration.is_empty()
                    && !interface.is_empty()
                    && configuration.bytes().all(|byte| byte.is_ascii_digit())
                    && interface.bytes().all(|byte| byte.is_ascii_digit())
            })
}

/// The ALSA capture device of the hardware behind video node `node` (such
/// as `/dev/video0`), if it has one.
#[must_use]
pub fn paired_alsa_device(sysfs: &dyn SysfsView, node: &str) -> Option<String> {
    let name = node.rsplit('/').next()?;
    let video = sysfs.canonical(&format!("/sys/class/video4linux/{name}/device"))?;
    let video = hardware_path(&video);
    for card in sysfs.entries("/sys/class/sound") {
        let Some(number) = card.strip_prefix("card") else {
            continue;
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let base = format!("/sys/class/sound/{card}");
        let Some(device) = sysfs.canonical(&format!("{base}/device")) else {
            continue;
        };
        if hardware_path(&device) != video {
            continue;
        }
        let prefix = format!("pcmC{number}D");
        let Some(pcm) = sysfs
            .entries(&base)
            .iter()
            .filter_map(|entry| {
                entry
                    .strip_prefix(&prefix)?
                    .strip_suffix('c')?
                    .parse::<u32>()
                    .ok()
            })
            .min()
        else {
            continue;
        };
        let id = sysfs
            .read(&format!("{base}/id"))
            .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_graphic()))
            .unwrap_or_else(|| number.to_owned());
        return Some(format!("hw:CARD={id},DEV={pcm}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_interfaces_are_recognized() {
        assert!(is_usb_interface("1-4:1.0"));
        assert!(is_usb_interface("3-1.2.4:1.12"));
        assert!(!is_usb_interface("1-4"));
        assert!(!is_usb_interface("0000:03:00.0"));
        assert!(!is_usb_interface("usb1"));
    }
}
