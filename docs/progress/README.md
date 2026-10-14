# Progress screenshots

Captured from the native `freemix-studio` console running headless on this
development host (Linux aarch64, Xvfb + Mesa lavapipe software Vulkan). The
supervised daemon runs in simulated mode, so the mixer header reports
`METERS UNAVAILABLE` with unlit meter wells. The Preview/Program monitors show
the daemon's encoded-fallback `monitor_frame` images (Protocol 2.21, at most
640x360) letterboxed into each 16:9 well: in simulated mode each frame is a
render of the realized simulated source, badged `SIMULATED` and captioned
`SIMULATED RENDER · {w}×{h} encoded preview`; its tooltip states that
overlays, scenes, and Fade to Black are not composited. A tap with no frame
(for example a scene input, which the simulated tap does not render) keeps the
routing placeholder captioned `NO VIDEO FEED · waiting for encoded preview`. A
frame older than one second, or a simulated frame whose input no longer
matches the realized source, is dimmed and badged `STALE`. With a native
daemon, Program frames are GPU-downscaled CPU readbacks captioned
`ENCODED PREVIEW {w}×{h} · CPU readback`, and Preview reads
`NO PREVIEW TAP · native Preview image not produced`.

Input tiles have no per-input thumbnail feed. A tile borrows the Preview or
Program tap frame only while that frame is current (under one second old) and
shows exactly that tile's input as the tap's realized source; the picture is
tagged `PVW TAP` / `PGM TAP` (with `· SIM` for simulated renders). Every other
tile keeps its dim ordinal and reads `no thumbnail`. When a frame goes stale
the tile drops back to the ordinal while the monitor well dims it.

The console layout: a menu bar (File / Edit / View / Inputs / Output / Help);
a top bar (show name, connection pill, revision, `REC` and `LIVE` on-air
badges, UTC clock); Preview and Program monitors with tally
headers around the transition column (Cut/Fade/Wipe/Alpha/Slide/Zoom keys,
shared duration, vertical manual T-bar with a travel readout in its header
and its start/commit/cancel keys, a manual-transition status card naming the
desired kind, `from ▶ to` inputs, travel percent, and `SYNCING` while the
daemon's realized state differs, and Fade-to-Black with a realized on-air
status line: `PROGRAM LIVE`, `FADING TO BLACK n%`, `PROGRAM BLACK`,
`RESTORING · n% BLACK`, plus `· BLACK/LIVE REQUESTED` until the daemon adopts
a new target); the input bank as tally-bordered tiles with a tally strip
along the top edge (click = Preview, double-click or `CUT` = Preview then Cut,
`···` or right-click = Preview/Cut/Rename/Move menu); the audio mixer with vertical
channel strips whose meters and faders grow with the window (segmented RMS meters with a peak line, a peak-hold marker that
holds 1.5 s then falls 20 dB/s, and a clip lamp latched 3 s at 0 dBFS; fader,
gain/balance/delay, M/S/AFV) and a pinned Master meter strip; a side panel
whose outputs card lists one fixed-height status row per recorder slot and
per stream target, above Overlays/Stingers tabs; and a bottom status bar with
the latest result/notice and keyboard hints.

Every status surface shares one tone vocabulary: a lamp plus a fixed-width
chip that is solid red only when on air (recording, a stream reported live,
an overlay confirmed on Program), green for ready (Preview, a READY stinger),
amber for requested-but-not-realized or degraded state, error red for
failures, and an unlit outline when idle or unknown. An output row is a lamp,
the name (ellipsized, full name on hover), a status chip with a one-line
detail beside it, and exactly one `START`/`STOP` key. Recorder rows show
`RECORDING`/`STOPPED` from the replicated desired flag (the protocol reports no
realized file or duration, so none is shown) and `NO STATE` before
synchronization. Stream rows combine the desired flag with the latest
transport sample or last-confirmed realized state: `LIVE` (red, reported live),
`NOT LIVE` (amber: desired on, daemon reports stopped), `STARTING`,
`RECONNECTING`, `CONGESTED`, `FAILED`, or `OFF`; the detail line carries sent
bytes, dropped pairs, HLS segments and any failure code, or says that no
transport sample has arrived. The section headers summarize `2 SLOTS` /
`1 / 2 RECORDING` and `2 TARGETS` / `1 / 2 LIVE`. The Stingers tab is a grid of
fixed-size slot cards (slot, readiness lamp, media name or `Empty slot`):
clicking a READY slot fires it when transitions are allowed, clicking any other
slot selects it for the Configure card below, which shows the selected slot's
readiness chip, slot and media pickers, `ASSIGN`/`CLEAR`, and its cut point,
preload, audio policy, and fallback (for an empty slot, the defaults `ASSIGN`
would apply, marked `(default)`).

Menu items map only to existing Studio intents under their usual gates (the
Edit menu acts on the Preview input). The Output menu's recorder, stream, and
Stinger slot items show the target's live status in their right-hand column
in the same tone colours (`STOPPED`, `RECORDING`, `NOT LIVE`, `OFF`, `READY`,
`UNCONFIGURED`), and long names in menus end in an ellipsis. Output › Streams
lists one Start/Stop item per configured target (`stream_start`/`stream_stop`),
and Output › Stingers assigns the Preview input to a slot (`configure_stinger`) or clears a
configured slot (`remove_stinger`). Stream and Stinger actions require the
`transition` permission, as the daemon admits them; a stream key also waits
for that target's earlier command to resolve (`PENDING`), and Stinger
configuration is blocked during a manual T-bar transition. Assigning media to a
configured slot keeps its preload, cut point, audio policy, and fallback; an
unconfigured slot is preloaded, mixes with Program audio, falls back to Cut,
and cuts at half the shared transition duration. The protocol has no
add/remove/duplicate-input command, so none is offered.
View toggles the input bank, mixer, side panel, and status bar. Every gated
control, menu item, and tile action shows its action and shortcut as a tooltip
and, when disabled, the first blocking gate ("Unavailable: this session lacks
the edit_project permission"). Input tiles have hover, pressed, and keyboard
focus states (Tab to focus, Enter/Space selects Preview) and a right-click menu
whose header shows the input's key, ellipsized name, and tally badge, and
whose footer explains each disabled group in one line (for example "Rename and
Move unavailable: this session lacks the edit_project permission"). Tile
names, monitor header names, the show name, and the monitor placeholder title
are single-line with an ellipsis; the full text is in the tooltip. When the
console is not Ready a centered card over the monitors reports connecting /
synchronizing / reconnecting (with attempt and retry delay) / protocol mismatch
/ failure, the last connection error in a tinted box, and a footer stating that
controls are disabled. Regions without synchronized state say so rather than
showing defaults: `NO STATE` recorder and overlay chips, `— SOURCES` /
`— TARGETS`, centered "No input state" / "No audio state" empty states, and
"No synchronized show" in the top bar. The Help › Keyboard shortcuts modal
leads with a lamp stating whether the keys currently act (and why not), lists
fixed-width key caps beside their actions, and ends with how to close it. The
side panel is at most a quarter of the window wide (minimum 320 px), so the
monitors keep their width in compact windows. Command rejections, notices, and
terminal-uncertainty notices raise bounded, auto-expiring toasts at the
bottom-right while the console is Ready; the status bar keeps the latest
message. The meters stay unlit in these simulated-mode captures, so peak hold
and clip lamps are not visible here.

| File | What it shows |
|---|---|
| `studio-console-monitors.png` | 1920x1080 after `CUT` and a Camera 1 Wide tile click: both wells show `SIMULATED` encoded frames (Camera 1 Wide on Preview, Camera 2 Close on Program), the two tiles borrow those frames, and the Program tooltip names what the simulated render omits. |
| `studio-console-monitors-scene.png` | 1920x1080 with the Main Scene input on Program: Preview shows a simulated frame of Camera 2 Close and Program the simulated composite of the scene's layers. The outputs card shows both recorders `STOPPED`, the HLS ladder amber `NOT LIVE` (desired on, no transport sample), and RTMP Backup `OFF`. |
| `studio-console-overview.png` | 1920x1080 overview: REC 2 recording (red lamp, solid `RECORDING` chip, red-outlined row, header `1 / 2 RECORDING`, red `REC` badge lit), overlay O1 taken from Preview and confirmed `ON AIR` (also badged on the Program header), the HLS ladder `NOT LIVE` with its `STOP` key (amber `LIVE` outline in the top bar), the stopped RTMP target with `START`. |
| `studio-console-manual-transition.png` | 1920x1080 after manual `FADE` and a T-bar drag: header `T-BAR 27%`, the amber status card `FADE · Camera 2 Close ▶ Camera 1 Wide · 27%`, and the active `COMMIT`/`CANCEL` keys while automatic transitions are blocked. |
| `studio-console-fade-to-black.png` | 1920x1080 after `FTB`: the lit `FTB ON` key, the realized status line `PROGRAM BLACK` with its full red level bar, and the `FTB` badge on the Program header. |
| `studio-console-streams.png` | 1920x1080 after clicking `START` on the RTMP Backup row: both targets desired on with `STOP` keys and amber `NOT LIVE` chips (simulated mode reports no transport), and the Output menu with `NOT LIVE` in the right-hand column of both Stop items and `STOPPED` beside the recorder items. |
| `studio-console-stingers.png` | 1920x1080, Stingers tab after choosing Playback Clip in the Configure media picker and clicking `ASSIGN` for S1: the S1 card lit green `READY` with its media name and accent selection outline, the Configure card's green `READY` chip, enabled `CLEAR`, and the replicated cut point (15 fr), preload, audio, and fallback. REC 2 is still recording from the overview. |
| `studio-console-top.png` | The default 1600x940 window, idle: both recorder rows `STOPPED` with `START` keys, stream rows with their keys, overlays collapsed to one row per channel. |
| `studio-console-stopped.png` | The compact 1280x800 layout: the side panel narrows to 320 px so the monitors keep their width; the input bank and mixer scroll inside their cards and the Master strip stays pinned. |
| `studio-console-compact-720.png` | 1280x720: the same compact layout at the smallest supported height; long output names and details end in an ellipsis. |
| `studio-console-recording.png` | 1600x940 with independent per-slot recording (REC 1 `STOPPED` / `START`, REC 2 `RECORDING` / `STOP`) and the Stingers tab: eight empty slot cards (S1 outlined as the slot being configured), the shared duration line, and the Configure card listing the defaults `ASSIGN` would apply. |
| `studio-console-menu.png` | 1920x1080 with the Output menu open: transitions with their `C`/`F` keys, Fade to black `B`, the disabled manual T-bar Commit/Cancel, recorder items with `STOPPED`, stream items with `NOT LIVE`/`OFF`, and the Stingers submenus (Clear disabled with no configured slot). |
| `studio-console-shortcuts.png` | Help › Keyboard shortcuts: the green `Keys active` lamp, the reference generated from the same table the console's key handler is tested against, with fixed-width key caps. |
| `studio-console-input-menu.png` | Right-click menu on the Host Laptop tile: key cap and name header, Select on Preview / Cut to Program enabled, Rename and Move disabled, and the footer note that the operator session lacks `edit_project`. |
| `studio-console-reconnecting.png` | 1600x940 Studio started with `--connect` to an address with no daemon: the Reconnecting card with attempt count, retry delay, the last error, and the disabled-controls footer over the dimmed monitors; `NO STATE` recorder and overlay chips, `— TARGETS`, `— SOURCES`, centered empty states for the input bank and mixer, and "No synchronized show" in the top bar. |

Every screenshot was regenerated in this round. Pointer hover is not
delivered to open menus on this headless Xvfb host (also with the previous
build), so the menu captures show no hover tooltips; disabled menu items still
carry their `Unavailable: …` reason as a tooltip.

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
side panel; the recording capture clicked the `STINGERS` tab. The streams
capture clicked `START` on the RTMP Backup row, then opened `Output`; the
stingers capture clicked the `STINGERS` tab, picked Playback Clip in the
Configure media picker, and clicked `ASSIGN` (the simulated daemon may drop
the session for a few seconds after this command; Studio reconnects on its
own). The compact captures resize the window to 1280x800 and 1280x720. The menu,
shortcuts, and input-menu captures click `Output`, `Help` › `Keyboard
shortcuts`, and right-click the third tile (`xdotool click 3`); keyboard input
such as `Escape` needs `xdotool windowfocus "$WIN"` first because Xvfb runs
without a window manager. The reconnecting capture runs
`freemix-studio --connect 127.0.0.1:7399 --project-id 1` with nothing listening.
The monitors captures start from the demo show (Main Scene on Program) for
`-monitors-scene.png`, then click `CUT` and the Camera 1 Wide tile and hover
the Program well for `-monitors.png`. The manual-transition capture clicks
the `MANUAL` row's `FADE`, then drags the T-bar handle upward
(`xdotool mousedown 1`, `mousemove`, `mouseup 1`); the Fade-to-Black capture
clicks `FTB`. Recording start can stall the simulated daemon for a few
seconds, so allow it to settle before capturing the overview.
