//! Presentation to PDF conversion through `LibreOffice` (IN-014).
//!
//! Full `PowerPoint` rendering is out of scope; a presentation renders after
//! a headless `LibreOffice` (`soffice`) converts it to PDF. The converter is
//! optional: [`OfficeConverter::locate`] finds it (the `FREEMIX_SOFFICE`
//! program path, then `soffice` or `libreoffice` on `PATH`, then the usual
//! macOS and Windows install locations), and without it a presentation input
//! reports [`crate::DocumentError::ConverterUnavailable`]: install
//! `LibreOffice` or export the presentation to PDF.
//!
//! Each conversion is sandboxed in a fresh private temporary directory that
//! is removed afterwards: the source is copied in under a fixed name, the
//! converter gets its own throwaway user profile there
//! (`-env:UserInstallation`) so it never touches or locks the operator's
//! profile and never shows a recovery dialog, its environment is cleared
//! except `PATH` (and `HOME`/`TMPDIR` point into the sandbox), and it is
//! killed when it runs past the timeout. On Unix it leads its own process
//! group, and every process it started is killed when it finishes or
//! times out (killing `soffice` alone would orphan `soffice.bin`).
//! Conversion runs on the caller's
//! thread; callers run it on a worker, never on a frame loop.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fm_types::paged::DocumentFormat;

use crate::document::{DocumentError, MAX_DOCUMENT_BYTES};

/// The environment variable naming the converter program explicitly.
pub const CONVERTER_ENV: &str = "FREEMIX_SOFFICE";
/// How long one conversion may run by default.
pub const DEFAULT_CONVERSION_TIMEOUT: Duration = Duration::from_mins(2);
/// Most converter diagnostic bytes kept for an error message.
const MAX_DIAGNOSTIC_BYTES: u64 = 4 * 1024;

/// A located `LibreOffice` program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficeConverter {
    program: PathBuf,
}

impl OfficeConverter {
    /// A converter that runs `program`.
    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The program this converter runs.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Finds the converter: `FREEMIX_SOFFICE` (when set it is the only
    /// candidate; an empty value disables conversion), `soffice` or
    /// `libreoffice` on `PATH`, then the default install locations.
    #[must_use]
    pub fn locate() -> Option<Self> {
        Self::locate_in(std::env::var_os(CONVERTER_ENV), std::env::var_os("PATH"))
    }

    /// [`Self::locate`] with an explicit override and search path.
    #[must_use]
    pub fn locate_in(explicit: Option<OsString>, path: Option<OsString>) -> Option<Self> {
        if let Some(explicit) = explicit {
            let program = PathBuf::from(explicit);
            return is_program(&program).then(|| Self::with_program(program));
        }
        let names: &[&str] = if cfg!(windows) {
            &["soffice.exe", "soffice.com"]
        } else {
            &["soffice", "libreoffice"]
        };
        if let Some(path) = path {
            for directory in std::env::split_paths(&path) {
                for name in names {
                    let candidate = directory.join(name);
                    if is_program(&candidate) {
                        return Some(Self::with_program(candidate));
                    }
                }
            }
        }
        let defaults: &[&str] = if cfg!(target_os = "macos") {
            &["/Applications/LibreOffice.app/Contents/MacOS/soffice"]
        } else if cfg!(windows) {
            &[
                "C:\\Program Files\\LibreOffice\\program\\soffice.exe",
                "C:\\Program Files (x86)\\LibreOffice\\program\\soffice.exe",
            ]
        } else {
            &[]
        };
        defaults
            .iter()
            .map(PathBuf::from)
            .find(|candidate| is_program(candidate))
            .map(Self::with_program)
    }

    /// Converts presentation `bytes` of `format` to PDF bytes, stopping the
    /// converter after `timeout`.
    ///
    /// # Errors
    ///
    /// [`DocumentError::ConversionTimedOut`], and
    /// [`DocumentError::ConversionFailed`] when the converter cannot start,
    /// fails, or writes no PDF.
    pub fn convert(
        &self,
        format: DocumentFormat,
        bytes: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, DocumentError> {
        self.convert_cancellable(format, bytes, timeout, None)
    }

    /// [`Self::convert`], stopping the converter when `cancelled` turns true
    /// (the input went away, so its result is no longer wanted).
    ///
    /// # Errors
    ///
    /// [`DocumentError::ConversionTimedOut`],
    /// [`DocumentError::ConversionCancelled`], and
    /// [`DocumentError::ConversionFailed`] when the converter cannot start,
    /// fails, or writes no PDF.
    pub fn convert_cancellable(
        &self,
        format: DocumentFormat,
        bytes: &[u8],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<u8>, DocumentError> {
        if !format.needs_conversion() {
            return Err(DocumentError::Unsupported(format));
        }
        let sandbox = Sandbox::create()?;
        let source = sandbox
            .path
            .join(format!("presentation.{}", format.token()));
        let output = sandbox.path.join("out");
        let profile = sandbox.path.join("profile");
        let home = sandbox.path.join("home");
        for directory in [&output, &profile, &home] {
            fs::create_dir(directory).map_err(failed)?;
        }
        fs::write(&source, bytes).map_err(failed)?;
        let diagnostics = sandbox.path.join("converter.log");
        let log = fs::File::create(&diagnostics).map_err(failed)?;
        let mut command = Command::new(&self.program);
        command
            .arg("--headless")
            .arg("--invisible")
            .arg("--norestore")
            .arg("--nologo")
            .arg("--nodefault")
            .arg("--nolockcheck")
            .arg(format!("-env:UserInstallation={}", file_url(&profile)))
            .arg("--convert-to")
            .arg("pdf")
            .arg("--outdir")
            .arg(&output)
            .arg(&source)
            .current_dir(&sandbox.path)
            .env_clear()
            .env("HOME", &home)
            .env("TMPDIR", &sandbox.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        #[cfg(windows)]
        for name in ["SystemRoot", "TEMP", "TMP", "USERPROFILE", "APPDATA"] {
            if name == "TEMP" || name == "TMP" {
                command.env(name, &sandbox.path);
            } else if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // The converter leads its own process group, so everything it
        // starts (`soffice` runs `oosplash`, which runs `soffice.bin`) is
        // stopped with it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| DocumentError::ConversionFailed(format!("cannot start: {error}")))?;
        let status = finish(child, timeout, cancelled)?;
        let pdf = output.join("presentation.pdf");
        if !status.success() || !pdf.is_file() {
            let mut detail = String::new();
            if let Ok(file) = fs::File::open(&diagnostics) {
                let _ = file.take(MAX_DIAGNOSTIC_BYTES).read_to_string(&mut detail);
            }
            let detail = detail
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map_or_else(
                    || format!("the converter exited with {status}"),
                    str::to_owned,
                );
            return Err(DocumentError::ConversionFailed(detail));
        }
        let mut converted = Vec::new();
        fs::File::open(&pdf)
            .and_then(|file| {
                file.take(MAX_DOCUMENT_BYTES + 1)
                    .read_to_end(&mut converted)
            })
            .map_err(failed)?;
        if converted.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge);
        }
        Ok(converted)
    }
}

/// Waits for the converter `child` for at most `timeout`, then stops it
/// and everything it started.
fn finish(
    mut child: std::process::Child,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<std::process::ExitStatus, DocumentError> {
    let started = Instant::now();
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            stop_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            return Err(DocumentError::ConversionCancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // A helper the converter left behind is stopped too.
                stop_group(&child);
                return Ok(status);
            }
            Ok(None) if started.elapsed() >= timeout => {
                stop_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(DocumentError::ConversionTimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                stop_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed(error));
            }
        }
    }
}

/// Kills every process of the converter's process group (on Unix; on
/// other platforms only the converter itself is stopped).
fn stop_group(child: &std::process::Child) {
    #[cfg(unix)]
    if let Ok(id) = i32::try_from(child.id())
        && id > 0
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(id),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(not(unix))]
    let _ = child;
}

fn failed(error: impl std::fmt::Display) -> DocumentError {
    DocumentError::ConversionFailed(error.to_string())
}

fn is_program(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A `file://` URL for a local directory (forward slashes, percent-encoded
/// where a URL needs it).
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

/// A private temporary directory removed on drop.
struct Sandbox {
    path: PathBuf,
}

impl Sandbox {
    fn create() -> Result<Self, DocumentError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        for _ in 0..16 {
            let path = std::env::temp_dir().join(format!(
                "freemix-convert-{}-{}-{nanos}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(failed(error)),
            }
        }
        Err(DocumentError::ConversionFailed(
            "no private temporary directory could be created".into(),
        ))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_absolute_and_escaped() {
        assert_eq!(file_url(Path::new("/tmp/a b")), "file:///tmp/a%20b");
        assert_eq!(file_url(Path::new("C:\\Users\\x")), "file:///C:/Users/x");
    }

    #[test]
    fn an_empty_override_disables_conversion() {
        assert_eq!(
            OfficeConverter::locate_in(Some(OsString::new()), Some(OsString::from("/usr/bin"))),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_conversion_stops_the_converter() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("fm-convert-cancel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let script = directory.join("soffice");
        fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let converter = OfficeConverter::with_program(&script);
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                converter.convert_cancellable(
                    DocumentFormat::Pptx,
                    b"not a presentation",
                    Duration::from_secs(30),
                    Some(&cancelled),
                )
            });
            std::thread::sleep(Duration::from_millis(200));
            cancelled.store(true, Ordering::Release);
            handle.join().unwrap()
        });
        assert_eq!(result, Err(DocumentError::ConversionCancelled));
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_search_path_without_office_finds_nothing() {
        let empty = std::env::temp_dir().join("freemix-no-office-here");
        let found = OfficeConverter::locate_in(None, Some(empty.into_os_string()));
        if !cfg!(any(target_os = "macos", windows)) {
            assert_eq!(found, None);
        }
    }
}
