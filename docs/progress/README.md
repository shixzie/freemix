# Progress screenshots

Captured from the native `freemix-studio` console running headless on this
development host (Linux aarch64, Xvfb + Mesa lavapipe software Vulkan). The
supervised daemon runs in simulated mode, so the mixer header reports
`METERS UNAVAILABLE` with unlit meter wells, and the Preview/Program monitors
show replicated routing state over a dark 16:9 well with the explicit
`NO VIDEO FEED · preview delivery pending` caption rather than output video.

The console layout: a menu bar (File / Edit / View / Inputs / Output / Help);
a top bar (show name, connection pill, revision, `REC` and `LIVE` on-air
badges, UTC clock); Preview and Program monitors with tally
headers around the transition column (Cut/Fade/Wipe/Alpha/Slide/Zoom keys,
shared duration, vertical manual T-bar with its start/commit/cancel keys, and
Fade-to-Black); the input bank as tally-bordered tiles (click = Preview,
double-click or `CUT` = Preview then Cut); the audio mixer with vertical
channel strips (segmented RMS meters with a peak line, a peak-hold marker that
holds 1.5 s then falls 20 dB/s, and a clip lamp latched 3 s at 0 dBFS; fader,
gain/balance/delay, M/S/AFV) and a pinned Master meter strip; a side panel
with recording and stream status above Overlays/Stingers tabs; and a bottom
status bar with the latest result/notice and keyboard hints.

Menu items map only to existing Studio intents under their usual gates (the
Edit menu acts on the Preview input; there is no add/remove-input or stream
start/stop intent, so none is offered, and the Streams item explains that).
View toggles the input bank, mixer, side panel, and status bar. Every gated
control, menu item, and tile action shows its action and shortcut as a tooltip
and, when disabled, the first blocking gate ("Unavailable: this session lacks
the edit_project permission"). Input tiles have hover, pressed, and keyboard
focus states (Tab to focus, Enter/Space selects Preview) and a right-click menu.
When the console is not Ready a centered card over the monitors reports
connecting / synchronizing / reconnecting (with attempt and retry delay) /
protocol mismatch / failure. Command rejections, notices, and
terminal-uncertainty notices raise bounded, auto-expiring toasts at the
bottom-right while the console is Ready; the status bar keeps the latest
message. The meters stay unlit in these simulated-mode captures, so peak hold
and clip lamps are not visible here.

| File | What it shows |
|---|---|
| `studio-console-overview.png` | 1920x1080 overview: REC 2 recording (red `REC` badge lit), overlay O1 taken from Preview and confirmed `ON AIR` (also badged on the Program header), the HLS ladder desired running but not reported live (amber `LIVE` outline), Camera 2 Close on Preview and Main Scene on Program. |
| `studio-console-top.png` | The default 1600x940 window, idle: both recorder slots `STOPPED` with their `START` keys, overlays collapsed to one row per channel. |
| `studio-console-stopped.png` | The compact 1280x800 layout with both recorder slots `STOPPED`; the input bank and mixer scroll inside their cards and the Master strip stays pinned. |
| `studio-console-recording.png` | 1600x940 with independent per-slot recording (`REC 1 STOPPED` / `START` beside `REC 2 RECORDING` / `STOP`) and the Stingers tab showing all eight unconfigured slots. |
| `studio-console-menu.png` | 1920x1080 with the Output menu open (transitions with their `C`/`F` keys, Fade to black `B`, manual T-bar Commit/Cancel `Esc`, per-slot recorder Start, and the read-only Streams note), hovering the disabled Commit item to show its reason. |
| `studio-console-shortcuts.png` | Help › Keyboard shortcuts: the modal reference generated from the same table the console's key handler is tested against. |
| `studio-console-input-menu.png` | Right-click menu on the Host Laptop tile: Select on Preview / Cut to Program enabled; Rename and Move disabled because the operator session lacks `edit_project`. |
| `studio-console-reconnecting.png` | 1600x940 Studio started with `--connect` to an address with no daemon: the Reconnecting card with its attempt count and retry delay over the dimmed monitors, every control disabled, and the full error in the status bar. |

The demo show used for the captures ("Evening Broadcast") has six simulated
inputs, one scene input ("Main Scene") with two picture-in-picture layers, an
audio bus, an output, a two-rung local HLS ladder target (desired running), an
RTMP target (stopped), the scene routed to Program and Camera 2 Close on
Preview.

## Reproducing

```sh
cargo build -p freemix-cli -p freemixd -p freemix-studio

# A show with inputs, a scene, an output, and stream targets.
freemix-cli demo demo.freemix program.ppm
freemix-cli project-rename demo.freemix "Evening Broadcast"
freemix-cli rename demo.freemix 1 "Camera 1 Wide"
freemix-cli rename demo.freemix 2 "Camera 2 Close"
freemix-cli input-add demo.freemix 3 "Host Laptop"
freemix-cli input-add demo.freemix 4 "Guest Remote"
freemix-cli simulated-solid-input-add demo.freemix 5 "Lower Third" 20 60 140 255
freemix-cli input-add demo.freemix 6 "Playback Clip"
freemix-cli scene-input-add demo.freemix 7 100 "Main Scene"
freemix-cli scene-layer-add demo.freemix 100 1 0 "Wide Shot"
freemix-cli scene-layer-add demo.freemix 100 4 1 "Guest PiP"
freemix-cli scene-layer-geometry demo.freemix 100 0 0 0 1920 1080 0
freemix-cli scene-layer-geometry demo.freemix 100 1 1360 740 480 270 0
freemix-cli audio-bus-add demo.freemix 1 "Program Bus"
freemix-cli output-add demo.freemix 1 100 1 "Program Output"
freemix-cli stream-add demo.freemix 1 1 hls:///tmp/hls "Local HLS Ladder" \
    --startup reconcile-desired-state --rung 2500:1280x720 --rung 6000:1920x1080
freemix-cli stream-add demo.freemix 2 1 rtmp://live.example.com/app demo-stream-key "RTMP Backup"
freemix-cli stream-start demo.freemix 1
freemix-cli preview demo.freemix 7 && freemix-cli cut demo.freemix
freemix-cli preview demo.freemix 2

Xvfb :99 -screen 0 1920x1080x24 &
DISPLAY=:99 \
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json \
WGPU_BACKEND=vulkan \
LIBGL_ALWAYS_SOFTWARE=1 \
  freemix-studio --project demo.freemix --daemon freemixd --listen 127.0.0.1:7000 &

# No window manager: size the window to the screen and park the pointer.
WIN=$(DISPLAY=:99 xdotool search --onlyvisible --name "FreeMix Studio" | head -1)
DISPLAY=:99 xdotool windowmove "$WIN" 0 0 windowsize "$WIN" 1920 1080
DISPLAY=:99 xdotool mousemove 960 1076

# Optional: recorder slot 2 for the recording state.
freemix-cli remote-record-start 127.0.0.1:7000 --recorder 2

DISPLAY=:99 import -window root shot.png
```

Software Vulkan is slow; allow roughly ten seconds after the window maps
before capturing. The overview's overlay take was a click on O1 `TAKE` in the
side panel; the recording capture clicked the `STINGERS` tab. The menu,
shortcuts, and input-menu captures click `Output`, `Help` › `Keyboard
shortcuts`, and right-click the third tile (`xdotool click 3`); keyboard input
such as `Escape` needs `xdotool windowfocus "$WIN"` first because Xvfb runs
without a window manager. The reconnecting capture runs
`freemix-studio --connect 127.0.0.1:7399 --project-id 1` with nothing listening.
