//! Screen and window capture through `ScreenCaptureKit` (IN-011).
//!
//! `build.rs` compiles `native/ScreenCaptureHelper.swift` into
//! `freemix-screen-capture-helper`. Its `list` command prints the
//! shareable screens and on-screen application windows as the tab-separated
//! records [`ScreenCaptureInventory::from_records`] reads; its `capture`
//! command writes raw BGRA pictures of a fixed size to standard output,
//! which the daemon pipes into `ffmpeg -f rawvideo`. The arguments of one
//! capture are built by [`capture_arguments`]. Its `audio` command
//! ([`audio_arguments`]) captures sound with `ScreenCaptureKit`
//! (`capturesAudio`, macOS 13 or later, excluding the helper's own
//! process): a display's whole system output, or one application's sound,
//! written as raw interleaved 48 kHz stereo `f32le` to standard output with
//! `FMAUDIO` status lines on standard error, for the daemon's audio tap.
//!
//! Capturing needs the Screen Recording permission (System Settings,
//! Privacy & Security) for the process that runs the server; without it
//! the listing reports the refusal.

use fm_types::screen_capture::ScreenCaptureInventory;

#[cfg(target_os = "macos")]
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// The token macOS listings name their backend with.
pub const BACKEND: &str = "screencapturekit";

/// The pixel format the helper writes, as `ffmpeg` names it.
pub const PIXEL_FORMAT: &str = "bgra";

/// The longest listing read from the helper.
#[cfg(target_os = "macos")]
const MAX_LISTING_BYTES: u64 = 256 * 1024;

/// What one helper capture reads: a screen by its listed index or a window
/// by its id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperTarget {
    Screen(u16),
    Window(u64),
}

/// The helper arguments capturing the `width` x `height` rectangle at
/// (`x`, `y`) of `target` (in the target's points) at `fps`, with or
/// without the cursor.
#[must_use]
pub fn capture_arguments(
    target: HelperTarget,
    (x, y, width, height): (u32, u32, u32, u32),
    fps: u32,
    cursor: bool,
) -> Vec<String> {
    let (kind, id) = match target {
        HelperTarget::Screen(index) => ("display", u64::from(index)),
        HelperTarget::Window(id) => ("window", id),
    };
    vec![
        "capture".to_owned(),
        kind.to_owned(),
        id.to_string(),
        x.to_string(),
        y.to_string(),
        width.to_string(),
        height.to_string(),
        fps.to_string(),
        u8::from(cursor).to_string(),
    ]
}

/// What one helper audio capture reads: the whole system output through a
/// display (by its listed index), or one application (by its process id).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperAudio {
    Display(u16),
    App(u32),
}

/// The helper arguments capturing `audio`.
#[must_use]
pub fn audio_arguments(audio: HelperAudio) -> Vec<String> {
    let (flag, id) = match audio {
        HelperAudio::Display(index) => ("--display", u64::from(index)),
        HelperAudio::App(pid) => ("--app", u64::from(pid)),
    };
    vec!["audio".to_owned(), flag.to_owned(), id.to_string()]
}

/// The inventory one helper listing describes. A listing without its
/// closing `END` line (a helper that failed) reports `error` instead.
#[must_use]
pub fn parse_listing(listing: &str, error: &str) -> ScreenCaptureInventory {
    if !listing.lines().any(|line| line.trim_end() == "END") {
        let mut inventory = ScreenCaptureInventory::from_records("", BACKEND);
        inventory.error = Some(error.to_owned());
        return inventory.fitted();
    }
    ScreenCaptureInventory::from_records(listing, BACKEND)
}

/// The helper `build.rs` compiled, when it is present.
#[cfg(target_os = "macos")]
#[must_use]
pub fn helper() -> Option<&'static Path> {
    let helper = Path::new(env!("FREEMIX_SCREEN_CAPTURE_HELPER"));
    helper.is_file().then_some(helper)
}

/// Lists the desktop through `helper`, waiting at most 15 seconds.
#[cfg(target_os = "macos")]
#[must_use]
pub fn list(helper: &Path) -> ScreenCaptureInventory {
    let mut child = match Command::new(helper)
        .arg("list")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return parse_listing(
                "",
                &format!("the screen capture helper did not start: {error}"),
            );
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return parse_listing("", "the screen capture helper has no output");
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut listing = String::new();
        let mut problem = String::new();
        let _ = stdout.take(MAX_LISTING_BYTES).read_to_string(&mut listing);
        let _ = stderr.take(4_096).read_to_string(&mut problem);
        let _ = sender.send((listing, problem));
    });
    let answer = receiver.recv_timeout(Duration::from_secs(15));
    let _ = child.kill();
    let _ = child.wait();
    match answer {
        Ok((listing, problem)) => {
            let problem = problem.trim();
            let error = if problem.is_empty() {
                "the screen capture helper failed"
            } else {
                problem
            };
            parse_listing(&listing, error)
        }
        Err(_) => parse_listing("", "listing the desktop timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_arguments_name_the_target_rectangle_rate_and_cursor() {
        assert_eq!(
            capture_arguments(HelperTarget::Window(4_242), (8, 4, 640, 360), 30, true),
            [
                "capture", "window", "4242", "8", "4", "640", "360", "30", "1"
            ]
        );
        assert_eq!(
            capture_arguments(HelperTarget::Screen(1), (0, 0, 1920, 1080), 60, false),
            [
                "capture", "display", "1", "0", "0", "1920", "1080", "60", "0"
            ]
        );
    }

    #[test]
    fn audio_arguments_name_the_display_or_application() {
        assert_eq!(
            audio_arguments(HelperAudio::Display(1)),
            ["audio", "--display", "1"]
        );
        assert_eq!(
            audio_arguments(HelperAudio::App(4_120)),
            ["audio", "--app", "4120"]
        );
    }

    #[test]
    fn listings_need_their_end_line() {
        let inventory = parse_listing(
            "S\t0\t1\t0\t0\t1512\t982\tDisplay 1\nW\t77\t501\t10\t40\t800\t600\tKeynote\tDeck\nEND\n",
            "unused",
        );
        assert_eq!(inventory.backend, BACKEND);
        assert_eq!(inventory.screens.len(), 1);
        assert_eq!(inventory.windows[0].app, "Keynote");
        assert_eq!(inventory.error, None);

        let refused = parse_listing("S\t0\t1\t0\t0\t1512\t982\tDisplay 1\n", "not permitted");
        assert!(refused.supported);
        assert_eq!(refused.screens.len(), 0);
        assert_eq!(refused.error.as_deref(), Some("not permitted"));
    }
}
