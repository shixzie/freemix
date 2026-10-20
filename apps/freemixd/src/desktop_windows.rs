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
//! Sound ([`audio_plan`]) is captured by a PowerShell helper running an
//! inline C# WASAPI client ([`AUDIO_SCRIPT`]) that writes raw 48 kHz
//! stereo `f32le` to its standard output for the daemon's audio tap
//! ([`fm_io_linux::capture_audio`]):
//!
//! - system audio is shared-mode loopback capture of the default render
//!   endpoint (`AUDCLNT_STREAMFLAGS_LOOPBACK`, converted to 48 kHz stereo
//!   float by `AUTOCONVERTPCM`), polled every 10 ms; silent packets are
//!   written as zeros, and no packets arrive while nothing plays (the tap
//!   pads that with silence);
//! - application audio is process loopback
//!   (`ActivateAudioInterfaceAsync` on `VAD\Process_Loopback` with
//!   `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` and
//!   `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`), event driven,
//!   for the window's process and its children. It needs Windows 10
//!   version 2004 (build 19041); an older build reports `unsupported` and
//!   exits with the tap's unsupported status.
//!
//! The helper reports `FMAUDIO` status lines on standard error; a changed
//! default output ends it with an error, and the tap restarts it.
//!
//! The record parser and the helper arguments are plain text handling and
//! are tested on every platform; only the child processes are
//! Windows-only.

#![cfg_attr(not(windows), allow(dead_code))]

use std::io::BufRead;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::DeviceInput;
use fm_io_linux::capture_audio::{ProducerCommand, TapSource};
use fm_io_linux::screen_capture::{CaptureRegion, capture_rate};
use fm_protocol::{ScreenCaptureAudio, ScreenCaptureConfig, ScreenCaptureInventory, WindowInfo};
use fm_types::FrameRate;

use crate::desktop_capture::AudioPlan;

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

/// The longest listing line kept, in bytes; a longer line is skipped.
pub(crate) const MAX_LINE_BYTES: usize = 4_096;
/// The longest answer read before the child is judged broken, in lines.
pub(crate) const MAX_ANSWER_LINES: usize = 4_096;
/// How long one answer may take before the child is judged hung and is
/// killed and restarted.
pub(crate) const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// One line the child wrote, as its reader thread hands it on.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Line {
    /// A line of at most [`MAX_LINE_BYTES`], without its line ending.
    Text(String),
    /// A longer line, read and discarded without being kept.
    Overlong,
}

/// Reads one line of at most `max` bytes from `reader`, discarding the
/// rest of a longer one without keeping it; `None` at the end of input.
pub(crate) fn read_bounded_line(
    reader: &mut impl BufRead,
    max: usize,
) -> std::io::Result<Option<Line>> {
    let mut kept = Vec::new();
    let mut overlong = false;
    let mut read_any = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        read_any = true;
        let newline = buffer.iter().position(|&byte| byte == b'\n');
        let chunk = &buffer[..newline.unwrap_or(buffer.len())];
        if !overlong {
            if kept.len() + chunk.len() > max {
                overlong = true;
                kept = Vec::new();
            } else {
                kept.extend_from_slice(chunk);
            }
        }
        let used = newline.map_or(buffer.len(), |end| end + 1);
        reader.consume(used);
        if newline.is_some() {
            break;
        }
    }
    if !read_any {
        return Ok(None);
    }
    if overlong {
        return Ok(Some(Line::Overlong));
    }
    if kept.last() == Some(&b'\r') {
        kept.pop();
    }
    Ok(Some(Line::Text(
        String::from_utf8_lossy(&kept).into_owned(),
    )))
}

/// Forwards `reader`'s bounded lines to `lines` until the input ends, a
/// read fails, or the receiver is gone. Runs on the child's reader thread.
pub(crate) fn forward_lines(mut reader: impl BufRead, lines: &SyncSender<Line>) {
    while let Ok(Some(line)) = read_bounded_line(&mut reader, MAX_LINE_BYTES) {
        if lines.send(line).is_err() {
            return;
        }
    }
}

/// Why an answer could not be read.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum AnswerError {
    /// The child's output ended.
    Exited,
    /// No `END` line arrived within the timeout.
    TimedOut,
    /// The answer ran past [`MAX_ANSWER_LINES`].
    TooLong,
}

/// The answer the child sends on `lines`: every line up to the `END` line,
/// overlong lines skipped, waiting at most `timeout` in all.
pub(crate) fn await_answer(
    lines: &Receiver<Line>,
    timeout: Duration,
) -> Result<String, AnswerError> {
    let deadline = Instant::now() + timeout;
    let mut answer = String::new();
    for _ in 0..MAX_ANSWER_LINES {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(Line::Text(line)) if line.trim_end() == "END" => return Ok(answer),
            Ok(Line::Text(line)) => {
                answer.push_str(&line);
                answer.push('\n');
            }
            Ok(Line::Overlong) => {}
            Err(RecvTimeoutError::Timeout) => return Err(AnswerError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => return Err(AnswerError::Exited),
        }
    }
    Err(AnswerError::TooLong)
}

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

/// The environment the audio helper keeps beside the tap's base set:
/// PowerShell and `Add-Type` (which compiles C# through the .NET
/// Framework) need the user's profile folders.
const AUDIO_ENVIRONMENT: &[&str] = &[
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "PSModulePath",
    "COMPUTERNAME",
    "USERNAME",
];

/// Why application audio cannot be captured without the window's process.
pub(crate) const UNKNOWN_PROCESS: &str = "the window's process is unknown";

/// The WASAPI capture helper: an inline C# client compiled by `Add-Type`.
/// [`audio_arguments`] appends the call that runs it, so no argument is
/// parsed by PowerShell.
///
/// `[FreemixAudio]::Run('system', 0)` captures the default render
/// endpoint's loopback; `[FreemixAudio]::Run('app', <pid>)` the process
/// loopback of `<pid>` and its children. Both write interleaved 48 kHz
/// stereo `f32le` to standard output and `FMAUDIO` lines to standard
/// error; exit 3 means unsupported here.
pub(crate) const AUDIO_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Win32;

[StructLayout(LayoutKind.Sequential, Pack = 1)]
public struct FreemixWaveFormat {
  public ushort FormatTag; public ushort Channels; public uint SamplesPerSec; public uint AvgBytesPerSec;
  public ushort BlockAlign; public ushort BitsPerSample; public ushort Size;
  public ushort ValidBitsPerSample; public uint ChannelMask; public Guid SubFormat;
}

[StructLayout(LayoutKind.Sequential)]
public struct FreemixActivationParams { public int ActivationType; public uint TargetProcessId; public int ProcessLoopbackMode; }

[StructLayout(LayoutKind.Sequential)]
public struct FreemixBlobVariant { public ushort Vt; public ushort Reserved1; public ushort Reserved2; public ushort Reserved3; public uint Size; public IntPtr Data; }

[ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixDeviceEnumerator {
  void EnumAudioEndpoints(int dataFlow, int stateMask, out IntPtr devices);
  void GetDefaultAudioEndpoint(int dataFlow, int role, out IFreemixDevice device);
}

[ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixDevice {
  void Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object client);
}

[ComImport, Guid("1CB9AD4C-DBFA-4c32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixAudioClient {
  void Initialize(int shareMode, uint streamFlags, long bufferDuration, long periodicity, IntPtr format, IntPtr sessionGuid);
  void GetBufferSize(out uint frames);
  void GetStreamLatency(out long latency);
  void GetCurrentPadding(out uint frames);
  void IsFormatSupported(int shareMode, IntPtr format, out IntPtr closest);
  void GetMixFormat(out IntPtr format);
  void GetDevicePeriod(out long defaultPeriod, out long minimumPeriod);
  void Start();
  void Stop();
  void Reset();
  void SetEventHandle(IntPtr handle);
  void GetService(ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object service);
}

[ComImport, Guid("C8ADBD64-E71E-48a0-A4DE-185C395CD317"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixCaptureClient {
  void GetBuffer(out IntPtr data, out uint frames, out uint flags, out ulong devicePosition, out ulong counterPosition);
  void ReleaseBuffer(uint frames);
  void GetNextPacketSize(out uint frames);
}

[ComImport, Guid("72A22D78-CDE4-431D-B8CC-843A71199B6D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixActivateOperation {
  void GetActivateResult(out int result, [MarshalAs(UnmanagedType.IUnknown)] out object client);
}

[ComImport, Guid("41D949AB-9862-444A-80F6-C261334DA5EB"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixActivateHandler {
  void ActivateCompleted(IFreemixActivateOperation operation);
}

// IAgileObject: the completion arrives on a worker thread.
[ComImport, Guid("94ea2b94-e9cc-49e0-c0ff-ee64ca8f5b90"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IFreemixAgileObject { }

[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
public class FreemixDeviceEnumerator { }

public class FreemixActivateHandler : IFreemixActivateHandler, IFreemixAgileObject {
  public readonly ManualResetEvent Done = new ManualResetEvent(false);
  public int Result = -1;
  public object Client;
  public void ActivateCompleted(IFreemixActivateOperation operation) {
    try { operation.GetActivateResult(out Result, out Client); } catch (Exception) { Result = -1; }
    Done.Set();
  }
}

public static class FreemixAudio {
  const int ShareModeShared = 0;
  const uint Loopback = 0x00020000, EventCallback = 0x00040000;
  const uint SrcDefaultQuality = 0x08000000, AutoConvertPcm = 0x80000000;
  const uint BufferSilent = 0x2;
  const int ProcessLoopback = 1, IncludeTargetProcessTree = 0;
  const int UnsupportedExit = 3;
  static readonly Guid AudioClientId = new Guid("1CB9AD4C-DBFA-4c32-B178-C2F568A703B2");
  static readonly Guid CaptureClientId = new Guid("C8ADBD64-E71E-48a0-A4DE-185C395CD317");
  static readonly Guid FloatFormat = new Guid("00000003-0000-0010-8000-00aa00389b71");

  [DllImport("Mmdevapi.dll", ExactSpelling = true, PreserveSig = false)]
  static extern void ActivateAudioInterfaceAsync([MarshalAs(UnmanagedType.LPWStr)] string path, ref Guid iid,
    IntPtr activationParams, IFreemixActivateHandler handler, out IFreemixActivateOperation operation);

  static void Status(string state, string detail) {
    Console.Error.WriteLine("FMAUDIO\t" + state + (detail.Length > 0 ? "\t" + detail.Replace('\t', ' ').Replace('\n', ' ').Replace('\r', ' ') : ""));
    Console.Error.Flush();
  }

  static int Build() {
    try {
      using (RegistryKey key = Registry.LocalMachine.OpenSubKey(@"SOFTWARE\Microsoft\Windows NT\CurrentVersion")) {
        return int.Parse((string)key.GetValue("CurrentBuildNumber"));
      }
    } catch (Exception) { return Environment.OSVersion.Version.Build; }
  }

  static IntPtr Format() {
    FreemixWaveFormat format = new FreemixWaveFormat();
    format.FormatTag = 0xFFFE; format.Channels = 2; format.SamplesPerSec = 48000;
    format.BitsPerSample = 32; format.BlockAlign = 8; format.AvgBytesPerSec = 48000 * 8;
    format.Size = 22; format.ValidBitsPerSample = 32; format.ChannelMask = 3; format.SubFormat = FloatFormat;
    IntPtr pointer = Marshal.AllocCoTaskMem(Marshal.SizeOf(typeof(FreemixWaveFormat)));
    Marshal.StructureToPtr(format, pointer, false);
    return pointer;
  }

  static IFreemixAudioClient SystemClient() {
    IFreemixDeviceEnumerator enumerator = (IFreemixDeviceEnumerator)new FreemixDeviceEnumerator();
    IFreemixDevice device;
    enumerator.GetDefaultAudioEndpoint(0, 0, out device);
    Guid iid = AudioClientId;
    object client;
    device.Activate(ref iid, 23, IntPtr.Zero, out client);
    return (IFreemixAudioClient)client;
  }

  static IFreemixAudioClient ProcessClient(uint process) {
    FreemixActivationParams parameters = new FreemixActivationParams();
    parameters.ActivationType = ProcessLoopback;
    parameters.TargetProcessId = process;
    parameters.ProcessLoopbackMode = IncludeTargetProcessTree;
    int size = Marshal.SizeOf(typeof(FreemixActivationParams));
    IntPtr blob = Marshal.AllocHGlobal(size);
    IntPtr variant = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(FreemixBlobVariant)));
    try {
      Marshal.StructureToPtr(parameters, blob, false);
      FreemixBlobVariant value = new FreemixBlobVariant();
      value.Vt = 65; value.Size = (uint)size; value.Data = blob;
      Marshal.StructureToPtr(value, variant, false);
      FreemixActivateHandler handler = new FreemixActivateHandler();
      Guid iid = AudioClientId;
      IFreemixActivateOperation operation;
      ActivateAudioInterfaceAsync("VAD\\Process_Loopback", ref iid, variant, handler, out operation);
      if (!handler.Done.WaitOne(10000)) { throw new Exception("process loopback activation timed out"); }
      if (handler.Result != 0 || handler.Client == null) {
        throw new Exception("process loopback activation failed (0x" + handler.Result.ToString("X8") + ")");
      }
      return (IFreemixAudioClient)handler.Client;
    } finally {
      Marshal.FreeHGlobal(variant);
      Marshal.FreeHGlobal(blob);
    }
  }

  public static int Run(string mode, uint process) {
    try {
      bool application = mode == "app";
      if (application && Build() < 19041) {
        Status("unsupported", "per-application audio needs Windows 10 version 2004 or later");
        return UnsupportedExit;
      }
      IFreemixAudioClient client = application ? ProcessClient(process) : SystemClient();
      IntPtr format = Format();
      EventWaitHandle ready = null;
      try {
        if (application) {
          client.Initialize(ShareModeShared, Loopback | EventCallback | AutoConvertPcm | SrcDefaultQuality,
            200000, 0, format, IntPtr.Zero);
          ready = new EventWaitHandle(false, EventResetMode.AutoReset);
          client.SetEventHandle(ready.SafeWaitHandle.DangerousGetHandle());
        } else {
          client.Initialize(ShareModeShared, Loopback | AutoConvertPcm | SrcDefaultQuality,
            2000000, 0, format, IntPtr.Zero);
        }
      } finally {
        Marshal.FreeCoTaskMem(format);
      }
      Guid captureId = CaptureClientId;
      object service;
      client.GetService(ref captureId, out service);
      IFreemixCaptureClient capture = (IFreemixCaptureClient)service;
      Stream output = Console.OpenStandardOutput();
      byte[] bytes = new byte[48000 * 8];
      client.Start();
      Status("live", "");
      while (true) {
        if (ready != null) { ready.WaitOne(100); } else { Thread.Sleep(10); }
        uint packet;
        capture.GetNextPacketSize(out packet);
        while (packet > 0) {
          IntPtr data; uint frames; uint flags; ulong devicePosition; ulong counterPosition;
          capture.GetBuffer(out data, out frames, out flags, out devicePosition, out counterPosition);
          int length = (int)frames * 8;
          if (length > bytes.Length) { bytes = new byte[length]; }
          if ((flags & BufferSilent) != 0 || data == IntPtr.Zero) { Array.Clear(bytes, 0, length); }
          else { Marshal.Copy(data, bytes, 0, length); }
          capture.ReleaseBuffer(frames);
          output.Write(bytes, 0, length);
          capture.GetNextPacketSize(out packet);
        }
        output.Flush();
      }
    } catch (IOException) {
      // The tap closed its end: capture is over.
      return 0;
    } catch (Exception error) {
      Status("error", error.Message);
      return 1;
    }
  }
}
'@
"#;

/// The PowerShell arguments running [`AUDIO_SCRIPT`] for `audio`: the
/// whole system output, or the process `pid` and its children.
pub(crate) fn audio_arguments(audio: ScreenCaptureAudio, pid: u32) -> Vec<String> {
    let (mode, pid) = match audio {
        ScreenCaptureAudio::Application => ("app", pid),
        ScreenCaptureAudio::None | ScreenCaptureAudio::System => ("system", 0),
    };
    vec![
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-Command".to_owned(),
        format!("{AUDIO_SCRIPT}exit ([FreemixAudio]::Run('{mode}', {pid}))\n"),
    ]
}

/// How a Windows capture's sound is captured: the WASAPI helper on the
/// whole system output, or on the window's process tree.
pub(crate) fn audio_plan(audio: ScreenCaptureAudio, window: Option<&WindowInfo>) -> AudioPlan {
    let pid = match audio {
        ScreenCaptureAudio::None => return AudioPlan::Silent,
        ScreenCaptureAudio::System => 0,
        ScreenCaptureAudio::Application => {
            match window.and_then(|window| window.pid).filter(|pid| *pid != 0) {
                Some(pid) => pid,
                None => return AudioPlan::Unsupported(UNKNOWN_PROCESS.to_owned()),
            }
        }
    };
    AudioPlan::Tap(TapSource::Command(ProducerCommand {
        program: "powershell.exe".into(),
        args: audio_arguments(audio, pid),
        environment: AUDIO_ENVIRONMENT
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
    }))
}

#[cfg(windows)]
pub(crate) use process::WindowsLister;

#[cfg(windows)]
mod process {
    use std::io::{BufReader, Write};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{Receiver, sync_channel};

    use fm_io_linux::screen_capture::DesktopListing;
    use fm_protocol::ScreenCaptureInventory;

    use super::{ANSWER_TIMEOUT, LIST_SCRIPT, Line, await_answer, forward_lines, parse_listing};

    /// The child, its input, and the lines its reader thread forwards.
    /// Dropping it kills the child, which ends the reader thread.
    struct Helper {
        child: Child,
        input: ChildStdin,
        lines: Receiver<Line>,
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
                    // A broken or hung child is killed and started again.
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
        let (sender, lines) = sync_channel(256);
        let reader = std::thread::Builder::new()
            .name("freemixd-desktop-list-reader".into())
            .spawn(move || forward_lines(BufReader::new(output), &sender));
        if let Err(error) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Helper {
            child,
            input,
            lines,
        })
    }

    fn ask(helper: &mut Helper) -> std::io::Result<String> {
        helper.input.write_all(b"list\n")?;
        helper.input.flush()?;
        await_answer(&helper.lines, ANSWER_TIMEOUT)
            .map_err(|error| std::io::Error::other(format!("{error:?}")))
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::{ScreenCaptureConfig, ScreenCaptureTarget, WindowRule};

    use std::io::{BufReader, Cursor};
    use std::sync::mpsc::sync_channel;

    use super::*;

    #[test]
    fn listing_lines_are_bounded_and_overlong_ones_skipped() {
        let long = "x".repeat(MAX_LINE_BYTES + 1);
        let fit = "y".repeat(MAX_LINE_BYTES);
        let text = format!("S\t0\r\n{long}\n{fit}\nlast");
        // A tiny buffer splits every line across reads.
        let mut reader = BufReader::with_capacity(7, Cursor::new(text));
        let mut read = || read_bounded_line(&mut reader, MAX_LINE_BYTES).unwrap();
        assert_eq!(read(), Some(Line::Text("S\t0".into())));
        assert_eq!(read(), Some(Line::Overlong));
        assert_eq!(read(), Some(Line::Text(fit.clone())));
        assert_eq!(read(), Some(Line::Text("last".into())));
        assert_eq!(read(), None);
    }

    #[test]
    fn an_answer_ends_at_end_and_skips_overlong_lines() {
        let long = "x".repeat(MAX_LINE_BYTES * 3);
        let text = format!("S\t0\t1\t0\t0\t1920\t1080\tD1\n{long}\nEND\n");
        let (sender, lines) = sync_channel(4);
        let reader = std::thread::spawn(move || forward_lines(Cursor::new(text), &sender));
        let answer = await_answer(&lines, Duration::from_secs(5)).unwrap();
        assert_eq!(answer, "S\t0\t1\t0\t0\t1920\t1080\tD1\n");
        assert_eq!(parse_listing(&answer).screens.len(), 1);
        assert_eq!(
            await_answer(&lines, Duration::from_secs(5)),
            Err(AnswerError::Exited)
        );
        reader.join().unwrap();
    }

    #[test]
    fn a_hung_child_times_out_instead_of_blocking_forever() {
        let (sender, lines) = sync_channel(4);
        sender.send(Line::Text("S\t0".into())).unwrap();
        let started = Instant::now();
        assert_eq!(
            await_answer(&lines, Duration::from_millis(50)),
            Err(AnswerError::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(sender);
    }

    #[test]
    fn an_endless_answer_is_refused() {
        let (sender, lines) = sync_channel(MAX_ANSWER_LINES + 1);
        for _ in 0..=MAX_ANSWER_LINES {
            sender.send(Line::Text("W".into())).unwrap();
        }
        assert_eq!(
            await_answer(&lines, Duration::from_secs(5)),
            Err(AnswerError::TooLong)
        );
    }

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

    #[test]
    fn audio_plans_run_the_wasapi_helper_on_the_right_target() {
        let window = WindowInfo {
            id: 66_334,
            app: "chrome".into(),
            title: "Video".into(),
            pid: Some(4_120),
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        };
        assert_eq!(
            audio_plan(ScreenCaptureAudio::None, Some(&window)),
            AudioPlan::Silent
        );
        let AudioPlan::Tap(TapSource::Command(app)) =
            audio_plan(ScreenCaptureAudio::Application, Some(&window))
        else {
            panic!("application audio runs the helper");
        };
        assert_eq!(app.program, std::path::PathBuf::from("powershell.exe"));
        assert_eq!(
            app.args[..5],
            [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command"
            ]
        );
        assert!(app.args[5].ends_with("exit ([FreemixAudio]::Run('app', 4120))\n"));
        assert!(app.environment.contains(&"USERPROFILE".to_owned()));
        let AudioPlan::Tap(TapSource::Command(system)) =
            audio_plan(ScreenCaptureAudio::System, None)
        else {
            panic!("system audio runs the helper");
        };
        assert!(system.args[5].ends_with("exit ([FreemixAudio]::Run('system', 0))\n"));
        let unknown = WindowInfo {
            pid: None,
            ..window
        };
        assert_eq!(
            audio_plan(ScreenCaptureAudio::Application, Some(&unknown)),
            AudioPlan::Unsupported(UNKNOWN_PROCESS.to_owned())
        );
    }

    #[test]
    fn the_audio_helper_uses_wasapi_loopback_and_reports_its_state() {
        for needle in [
            "Initialize(ShareModeShared",
            "GetDefaultAudioEndpoint",
            "ActivateAudioInterfaceAsync",
            "VAD\\\\Process_Loopback",
            "IncludeTargetProcessTree = 0",
            "ProcessLoopback = 1",
            "Loopback = 0x00020000",
            "AutoConvertPcm = 0x80000000",
            "SamplesPerSec = 48000",
            "19041",
            "FMAUDIO",
            "UnsupportedExit = 3",
            "OpenStandardOutput",
            "BufferSilent",
        ] {
            assert!(AUDIO_SCRIPT.contains(needle), "{needle}");
        }
    }
}
