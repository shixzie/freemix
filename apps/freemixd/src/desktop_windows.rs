//! The Windows desktop a native daemon captures (IN-011).
//!
//! Listing runs one long-lived `powershell` child that answers each `list`
//! line on its standard input with the attached screens
//! (`System.Windows.Forms.Screen`) and the visible, non-minimized top-level
//! windows (`EnumWindows`, with each window's owning process name, title,
//! and rectangle), one tab-separated record per line and an `END` line
//! after each answer. Keeping the child alive avoids starting PowerShell
//! twice a second.
//!
//! Capture is `ffmpeg -f gdigrab -i desktop` with an offset and size: a
//! window is captured as the desktop rectangle it occupies (so a window
//! covering it is captured too), which keeps duplicate titles, and titles
//! that change, from confusing `gdigrab`'s own `title=` lookup. The window
//! rectangle is re-listed twice a second, so moving or resizing a window
//! restarts the capture on the new rectangle.
//!
//! The record parser is plain text handling and is tested on every
//! platform; only the child process is Windows-only.

#![cfg_attr(not(windows), allow(dead_code))]

use fm_codec_ffmpeg::network_input::DeviceInput;
use fm_io_linux::screen_capture::{CaptureRegion, capture_rate};
use fm_protocol::{ScreenCaptureConfig, ScreenCaptureInventory};
use fm_types::FrameRate;

/// The token Windows listings name their backend with.
pub(crate) const BACKEND: &str = "gdigrab";

/// The PowerShell listing loop: one answer per `list` line on standard
/// input, ending with `END`; it exits when standard input closes.
pub(crate) const LIST_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class FreemixWindows {
  public delegate bool EnumProc(IntPtr window, IntPtr data);
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr data);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr window);
  [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr window, uint command);
  [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr window);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int size);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out Rect rect);
  public static List<string> List() {
    var lines = new List<string>();
    EnumWindows((window, data) => {
      if (!IsWindowVisible(window) || IsIconic(window) || GetWindow(window, 4) != IntPtr.Zero) return true;
      int length = GetWindowTextLength(window);
      if (length == 0) return true;
      var text = new StringBuilder(length + 1);
      GetWindowText(window, text, text.Capacity);
      Rect rect;
      if (!GetWindowRect(window, out rect)) return true;
      uint process;
      GetWindowThreadProcessId(window, out process);
      string app = "";
      try { app = System.Diagnostics.Process.GetProcessById((int)process).ProcessName; } catch { }
      string title = text.ToString().Replace('\t', ' ').Replace('\r', ' ').Replace('\n', ' ');
      lines.Add("W\t" + window.ToInt64() + "\t" + process + "\t" + rect.Left + "\t" + rect.Top + "\t"
        + (rect.Right - rect.Left) + "\t" + (rect.Bottom - rect.Top) + "\t" + app + "\t" + title);
      return true;
    }, IntPtr.Zero);
    return lines;
  }
}
'@
while ($null -ne ($request = [Console]::In.ReadLine())) {
  $index = 0
  foreach ($screen in [System.Windows.Forms.Screen]::AllScreens) {
    $bounds = $screen.Bounds
    $primary = if ($screen.Primary) { 1 } else { 0 }
    [Console]::Out.WriteLine("S`t$index`t$primary`t$($bounds.X)`t$($bounds.Y)`t$($bounds.Width)`t$($bounds.Height)`t$($screen.DeviceName)")
    $index++
  }
  foreach ($line in [FreemixWindows]::List()) { [Console]::Out.WriteLine($line) }
  [Console]::Out.WriteLine('END')
  [Console]::Out.Flush()
}
"#;

/// The inventory one answer of [`LIST_SCRIPT`] describes; malformed
/// records are skipped.
pub(crate) fn parse_listing(answer: &str) -> ScreenCaptureInventory {
    ScreenCaptureInventory::from_records(answer, BACKEND)
}

/// The `ffmpeg -f gdigrab` input capturing `region` of the desktop at
/// `config`'s rate (capped by `program`) with or without the cursor,
/// letterboxed into the program picture.
pub(crate) fn gdigrab_device(
    region: &CaptureRegion,
    config: &ScreenCaptureConfig,
    program: FrameRate,
) -> DeviceInput {
    let offset = |origin: i32, inner: u32| {
        i64::from(origin)
            .saturating_add(i64::from(inner))
            .to_string()
    };
    DeviceInput {
        format: "gdigrab".to_owned(),
        options: vec![
            (
                "framerate".to_owned(),
                capture_rate(config, program).to_string(),
            ),
            ("draw_mouse".to_owned(), u8::from(config.cursor).to_string()),
            ("offset_x".to_owned(), offset(region.origin_x, region.x)),
            ("offset_y".to_owned(), offset(region.origin_y, region.y)),
            (
                "video_size".to_owned(),
                format!("{}x{}", region.width, region.height),
            ),
        ],
        path: "desktop".to_owned(),
        field_order: None,
        audio: None,
        audio_only: false,
        letterbox: true,
        environment: Vec::new(),
        feeder: None,
    }
}

#[cfg(windows)]
pub(crate) use process::WindowsLister;

#[cfg(windows)]
mod process {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    use fm_io_linux::screen_capture::DesktopListing;
    use fm_protocol::ScreenCaptureInventory;

    use super::{LIST_SCRIPT, parse_listing};

    /// The longest answer read before the child is judged broken.
    const MAX_ANSWER_LINES: usize = 4_096;

    struct Helper {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Lists the Windows desktop through a long-lived PowerShell child,
    /// restarted when it fails.
    #[derive(Default)]
    pub(crate) struct WindowsLister {
        helper: Option<Helper>,
    }

    impl WindowsLister {
        /// The desktop now. Windows reads covered windows' rectangles, so
        /// no clip to the desktop applies.
        pub(crate) fn list(&mut self) -> DesktopListing {
            for _ in 0..2 {
                if self.helper.is_none() {
                    match spawn() {
                        Ok(helper) => self.helper = Some(helper),
                        Err(error) => return failed(&format!("powershell: {error}")),
                    }
                }
                let Some(helper) = self.helper.as_mut() else {
                    continue;
                };
                match ask(helper) {
                    Ok(answer) => {
                        return DesktopListing {
                            inventory: parse_listing(&answer),
                            desktop: None,
                        };
                    }
                    Err(_) => self.helper = None,
                }
            }
            failed("powershell stopped answering")
        }
    }

    fn failed(error: &str) -> DesktopListing {
        DesktopListing {
            inventory: ScreenCaptureInventory {
                supported: true,
                backend: super::BACKEND.to_owned(),
                screens: Vec::new(),
                windows: Vec::new(),
                error: Some(error.to_owned()),
            }
            .fitted(),
            desktop: None,
        }
    }

    fn spawn() -> std::io::Result<Helper> {
        let mut child = Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                LIST_SCRIPT,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("no pipes"));
        };
        Ok(Helper {
            child,
            input,
            output: BufReader::new(output),
        })
    }

    fn ask(helper: &mut Helper) -> std::io::Result<String> {
        helper.input.write_all(b"list\n")?;
        helper.input.flush()?;
        let mut answer = String::new();
        for _ in 0..MAX_ANSWER_LINES {
            let mut line = String::new();
            if helper.output.read_line(&mut line)? == 0 {
                return Err(std::io::Error::other("powershell exited"));
            }
            if line.trim_end() == "END" {
                return Ok(answer);
            }
            answer.push_str(&line);
        }
        Err(std::io::Error::other("answer too long"))
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::{ScreenCaptureConfig, ScreenCaptureTarget, WindowRule};

    use super::*;

    #[test]
    fn a_listing_answer_names_the_gdigrab_backend() {
        let inventory = parse_listing("S\t0\t1\t0\t0\t1920\t1080\t\\\\.\\DISPLAY1\r\n");
        assert_eq!(inventory.backend, BACKEND);
        assert_eq!(inventory.screens[0].name, "DISPLAY1");
    }

    #[test]
    fn gdigrab_captures_the_desktop_rectangle_of_the_target() {
        let config = ScreenCaptureConfig {
            target: ScreenCaptureTarget::Window(WindowRule::exact("POWERPNT", "Deck", Some(7))),
            cursor: true,
            fps: 60,
            ..ScreenCaptureConfig::default()
        };
        let region = CaptureRegion {
            window_id: Some(7),
            screen: None,
            origin_x: -1_280,
            origin_y: 10,
            target_width: 800,
            target_height: 600,
            x: 16,
            y: 4,
            width: 640,
            height: 480,
        };
        let device = gdigrab_device(&region, &config, FrameRate::new(30, 1).unwrap());
        assert_eq!(device.format, "gdigrab");
        assert_eq!(device.path, "desktop");
        assert!(device.letterbox);
        let option = |name: &str| {
            device
                .options
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(option("framerate"), Some("30"));
        assert_eq!(option("draw_mouse"), Some("1"));
        assert_eq!(option("offset_x"), Some("-1264"));
        assert_eq!(option("offset_y"), Some("14"));
        assert_eq!(option("video_size"), Some("640x480"));
    }
}
