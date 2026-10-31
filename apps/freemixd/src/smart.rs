//! SMART health of the drive holding the replay root (OR-014, Protocol 2.139).
//!
//! Linux resolves the block device behind the root from
//! `/proc/self/mountinfo`, then asks `smartctl -j -H -A` for the drive's
//! JSON health. A query can take seconds, so the replay recorder starts it on
//! a worker thread and reads the result on a later tick; nothing here touches
//! the frame loop. A root on a filesystem with no resolvable block device, a
//! host without `smartctl`, and an unsupported drive all report nothing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `smartctl` may run before the probe kills it.
const SMART_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest `smartctl` output accepted.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// A drive's self-assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SmartHealth {
    Passed,
    /// The drive passed but reported a warning message.
    Warning,
    Failed,
    /// The drive answered without a verdict.
    Unknown,
}

/// One sampled SMART status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SmartSample {
    pub health: SmartHealth,
    pub temperature_celsius: Option<u16>,
    pub percentage_used: Option<u8>,
}

/// Samples the SMART status of the drive holding `directory`, or `None` when
/// the device cannot be resolved or `smartctl` reports nothing usable.
pub(crate) fn sample(directory: &Path) -> Option<SmartSample> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (device_number, _) = mount_for(directory, &mountinfo)?;
    let device = device_for(&device_number, Path::new("/sys/dev/block"))?;
    let output = run_smartctl(Path::new("smartctl"), &device)?;
    parse(&output)
}

/// The `major:minor` device number and mount point of the filesystem holding
/// `path`, from mountinfo text. The longest matching mount point wins.
fn mount_for(path: &Path, mountinfo: &str) -> Option<(String, String)> {
    let canonical = canonical_existing(path);
    let mut best: Option<(usize, String, String)> = None;
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        let Some(_id) = fields.next() else { continue };
        let Some(_parent) = fields.next() else {
            continue;
        };
        let Some(number) = fields.next() else {
            continue;
        };
        let Some(_root) = fields.next() else { continue };
        let Some(mount_point) = fields.next() else {
            continue;
        };
        if !number.contains(':') {
            continue;
        }
        let mount_point = unescape_mount_point(mount_point);
        let mount = Path::new(&mount_point);
        if !canonical.starts_with(mount) {
            continue;
        }
        let length = mount_point.len();
        if best
            .as_ref()
            .is_none_or(|(current, _, _)| length > *current)
        {
            best = Some((length, number.to_owned(), mount_point));
        }
    }
    best.map(|(_, number, mount_point)| (number, mount_point))
}

/// Canonicalizes `path`, or its deepest existing ancestor followed by the
/// components that do not exist yet: a replay root may be removed while the
/// recorder still holds it.
fn canonical_existing(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(mut canonical) = std::fs::canonicalize(&current) {
            for component in suffix.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                suffix.push(name.to_owned());
                current = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Undoes mountinfo's octal escapes (`\040` for a space and friends).
fn unescape_mount_point(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4].iter().all(u8::is_ascii_digit)
        {
            let digits = std::str::from_utf8(&bytes[index + 1..index + 4]).ok();
            if let Some(octal) = digits.and_then(|digits| u8::from_str_radix(digits, 8).ok()) {
                out.push(char::from(octal));
                index += 4;
                continue;
            }
        }
        out.push(char::from(bytes[index]));
        index += 1;
    }
    out
}

/// The `/dev` node for one `major:minor` pair, resolving the sysfs block
/// symlink to the device's name.
fn device_for(number: &str, sys_block: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(sys_block.join(number)).ok()?;
    let name = target.file_name()?.to_str()?;
    if name.is_empty() || name.contains('/') || name == ".." || name == "." {
        return None;
    }
    Some(Path::new("/dev").join(name))
}

/// Runs `smartctl` against `device`, bounded in time and output.
fn run_smartctl(program: &Path, device: &Path) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .arg("-j")
        .arg("-H")
        .arg("-A")
        .arg(device)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SMART_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = Vec::new();
    child
        .stdout
        .take()?
        .take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut output)
        .ok()?;
    if output.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    Some(output)
}

/// Parses `smartctl`'s JSON health output.
fn parse(bytes: &[u8]) -> Option<SmartSample> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let warning = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("severity").and_then(serde_json::Value::as_str) == Some("warning")
            })
        });
    let health = match value
        .get("smart_status")
        .and_then(|status| status.get("passed"))
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) if warning => SmartHealth::Warning,
        Some(true) => SmartHealth::Passed,
        Some(false) => SmartHealth::Failed,
        None => SmartHealth::Unknown,
    };
    let temperature_celsius = value
        .pointer("/temperature/current")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value <= 200);
    let percentage_used = value
        .pointer("/nvme_smart_health_information_log/percentage_used")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 100);
    Some(SmartSample {
        health,
        temperature_celsius,
        percentage_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
36 35 8:1 / / rw,relatime - ext4 /dev/sda1 rw
37 36 8:2 / /srv rw,relatime - ext4 /dev/sda2 rw
38 36 0:53 / /srv/replay rw,nosuid - tmpfs none rw
39 36 8:3 / /srv/my\\040show rw - ext4 /dev/sda3 rw
";

    #[test]
    fn the_longest_mount_point_wins_and_spaces_unescape() {
        assert_eq!(
            mount_for(Path::new("/srv/replay/segments"), MOUNTINFO),
            Some(("0:53".to_owned(), "/srv/replay".to_owned()))
        );
        assert_eq!(
            mount_for(Path::new("/srv/other"), MOUNTINFO),
            Some(("8:2".to_owned(), "/srv".to_owned()))
        );
        assert_eq!(
            mount_for(Path::new("/srv/my show/one"), MOUNTINFO),
            Some(("8:3".to_owned(), "/srv/my show".to_owned()))
        );
        // A sibling whose name only shares a prefix is not inside it.
        assert_eq!(
            mount_for(Path::new("/srv/replay2"), MOUNTINFO),
            Some(("8:2".to_owned(), "/srv".to_owned()))
        );
        // Anything not under a longer mount still belongs to the root.
        assert_eq!(
            mount_for(Path::new("/elsewhere"), MOUNTINFO),
            Some(("8:1".to_owned(), "/".to_owned()))
        );
    }

    #[test]
    fn a_sysfs_block_symlink_names_the_device() {
        let root = std::env::temp_dir().join(format!(
            "fm-smart-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let devices = root.join("devices/block/sda");
        std::fs::create_dir_all(&devices).unwrap();
        std::os::unix::fs::symlink(&devices, root.join("8:1")).unwrap();
        assert_eq!(device_for("8:1", &root), Some(PathBuf::from("/dev/sda")));
        assert_eq!(device_for("8:9", &root), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn health_temperature_and_endurance_parse() {
        assert_eq!(
            parse(
                br#"{"smart_status": {"passed": true},
                     "temperature": {"current": 35},
                     "nvme_smart_health_information_log": {"percentage_used": 2}}"#
            ),
            Some(SmartSample {
                health: SmartHealth::Passed,
                temperature_celsius: Some(35),
                percentage_used: Some(2),
            })
        );
        assert_eq!(
            parse(br#"{"smart_status": {"passed": false}}"#),
            Some(SmartSample {
                health: SmartHealth::Failed,
                temperature_celsius: None,
                percentage_used: None,
            })
        );
        assert_eq!(
            parse(br#"{"smart_status": {"passed": true}, "messages": [{"severity": "warning"}]}"#),
            Some(SmartSample {
                health: SmartHealth::Warning,
                temperature_celsius: None,
                percentage_used: None,
            })
        );
        assert_eq!(
            parse(br#"{"messages": [{"severity": "information"}]}"#),
            Some(SmartSample {
                health: SmartHealth::Unknown,
                temperature_celsius: None,
                percentage_used: None,
            })
        );
        assert_eq!(parse(b"not json"), None);
    }
}
