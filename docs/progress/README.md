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

The wells follow the canvas shape. The client receives no project video
settings, so Studio takes the shape from the latest Program frame (else
Preview) and keeps it while frames lapse, starting at 16:9. A 16:9 canvas
fills the well's width exactly as before. A taller canvas (4:3, square,
portrait) grows the well into up to half the console height, so a 1080x1920
program is pillarboxed in a tall well instead of shrunk into a 16:9 box. Bars
outside the canvas are a shade lighter than the canvas black, and the
title-safe marks, `SIMULATED`/`STALE` badges, and `FTB` badge sit on the
canvas.

Input tiles have no per-input thumbnail feed. A tile borrows the Preview or
Program tap frame only while that frame is current (under one second old) and
shows exactly that tile's input as the tap's realized source; the picture is
tagged `PVW TAP` / `PGM TAP` (with `· SIM` for simulated renders). Every other
tile keeps its dim ordinal and reads `no thumbnail`. When a frame goes stale
the tile drops back to the ordinal while the monitor well dims it.

The console layout: a menu bar (File / Edit / View / Inputs / Output / Help);
a top bar (show name, connection pill, revision, the edit group of padlock,
Undo, and Redo keys, `REC` and `LIVE` on-air badges, UTC clock); Preview and Program monitors with tally
headers around the transition column (Cut/Fade/Wipe/Alpha/Slide/Zoom keys,
shared duration, vertical manual T-bar with a travel readout in its header
and its start/commit/cancel keys, a manual-transition status card naming the
desired kind, `from ▶ to` inputs, travel percent, and `SYNCING` while the
daemon's realized state differs, and Fade-to-Black with a realized on-air
status line: `PROGRAM LIVE`, `FADING TO BLACK n%`, `PROGRAM BLACK`,
`RESTORING · n% BLACK`, plus `· BLACK/LIVE REQUESTED` until the daemon adopts
a new target); the input bank as tally-bordered tiles with a tally strip
along the top edge, sized to fill the card (up to 320 px wide; the bank
scrolls only when even minimum-width tiles do not fit) (click = Preview, double-click or `CUT` = Preview then Cut,
`···` or right-click = Preview/Cut/Rename/Move menu); the audio mixer with vertical
channel strips whose meters and faders grow with the window, and a pinned
Master meter strip. Each strip has a fixed-height name plate (the input's key
cap, tally-tinted, and up to two lines of name ending in an ellipsis; full
name on hover), a peak-hold readout, a segmented meter (RMS segments, a thin
latest-peak line, a held-peak marker that holds 1.5 s then falls 20 dB/s, and
a clip lamp), scale ticks in a gutter beside the meter at 0/-6/-12/-18/-30/
-48/-60 dBFS (labelled on the Master strip, 0 in red), a console fader, the
gain/balance/delay entries, and LED keys for M (red), S (amber), and AFV
(accent): lit keys show a lit LED bar, a tinted fill, and a coloured border;
unlit keys have a dark LED and a dim label. The fader has a taper that gives
most travel to the range around unity (0 dB at 72 %), a slot with scale
marks (+10, 0, 10, 20, 40, 60, ∞; unity longer and brighter), a trailing
level fill (grey while muted), and a ridged two-tone cap with a white index
line (red while muted) and an accent outline while dragged or focused. Drags
are relative, so grabbing the cap never jumps the gain; double-click returns
to 0 dB and Up/Down nudge 0.5 dB. The clip lamp, the readout (red `CLIP`),
and the meter outline latch at 0 dBFS until the operator clicks the meter or
readout, or `RESET n CLIPS` in the mixer header. Meters use only the
per-channel peak and RMS the daemon already sends; a side panel
whose outputs card lists one fixed-height status row per recorder slot and
per stream target, above Overlays/Stingers tabs; and a bottom status bar with
the latest result/notice and keyboard hints.

Edit history and the padlock (Protocol 2.22). The top-bar edit group is a
fixed-width padlock key, `UNLOCKED` as an unlit outline with an open shackle
or `LOCKED` as an amber chip with a closed one (a deliberate hold, read like a
pending state), then compact Undo and Redo keys with painted turn-back arrows.
The keys are disabled, and their tooltips name the reason, when there is
nothing to undo or redo, the session lacks `edit_project`, or the project is
locked; the enabled tooltip names the revision the action targets and the
history depth (`Undo the project edit at REV 10  [Ctrl+Z]` / `2 steps in the
shared history`). Edit › History labels the items with that revision
(`Undo edit at REV 10`), shows the shortcut in the right-hand column or an
amber `LOCKED` when the padlock blocks the item, and shows the padlock item
with its state (`UNLOCKED` / `LOCKED`). While the project is locked: the
status bar reads `EDITS LOCKED`; the input bank header shows a padlock badge
and an `EDITS LOCKED` chip (`n LOCKED` when only inputs are locked); every
tile's name bar carries a small amber padlock badge (only the locked inputs'
tiles when just inputs are locked); Rename and Move items in the tile menu
and the Edit menu show `LOCKED` and the footer note names the padlock; the
Stinger Configure card shows a padlock badge and disables `ASSIGN`/`CLEAR`
with "the project is locked against edits", as do Output › Stingers (slot
setup is a project edit, while firing a Stinger stays live switching). Moves
that would shift a neighbour with its own padlock are disabled with "the
neighbouring input is locked against moves", and an open rename editor
closes when a padlock engages. Switching, transitions, audio, overlays, and
outputs are never locked.

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
and cuts at half the shared transition duration.

Live input catalogue (protocol 2.23 `add_input` / `remove_input`): the input
bank ends with a dashed `+` tile ("Add input", "solid colour · colour bars")
and a `n / 256` count beneath it (`ADDING name…` while an add is pending);
Inputs › Catalogue repeats it as `Add input…` and a `Remove input` submenu with
one item per input. The Add input dialog picks a kind (`SOLID COLOUR` /
`COLOUR BARS`), pre-fills a unique name ("Solid Colour 1", "Colour Bars 1", …;
editing the name stops the suggestion from following the kind), validates it
(empty, too long, or already used), and for Solid offers eight preset swatches,
an egui colour picker, and the hex value; a preview swatch shows the colour or
75 % bars tagged with the input id Studio will use (the next free id: one past
the highest, or the smallest gap at the id ceiling) and `SILENT`. Enter or
`ADD INPUT` submits. Remove input is in the tile's right-click menu
(`Remove…`) and the Inputs menu, and asks for confirmation in a dialog that
names the input and warns that removal cannot be undone. The `+` tile and
menu item are disabled with their reason when the session lacks
`edit_project`, the project is locked (padlock badge on the tile), or 256
inputs exist; Remove is disabled with a `PGM` / `PVW` / `LOCKED` / `IN USE` /
`PENDING` mark when the input is on Program or Preview, in a manual or
automatic transition, used as Stinger media or by an overlay, locked, or
already being removed. Submitting raises an amber `PENDING` toast that turns
into a green `DONE` toast once the change is reflected in synchronized state,
or into a refusal toast that names the reason code and the server's detail
(for example `Remove refused (conflict): "Camera 1 Wide" is a layer source of
scene 100. Remove that reference offline first.` or `… (input_in_use): … is on
Program`); the generic rejection toast for the same command is suppressed.

While an egui menu, popup, or the help, about, add, or remove dialog is open,
console shortcuts are suspended, so Esc closes the menu or dialog instead of
cancelling a manual transition. Repeated "Queued N command(s)" notices update
one toast instead of stacking. Audio meters go stale when no meter message has
arrived for more than a second: lit segments, peak lines, and hold markers dim,
peak readouts read `STALE`, and the mixer header shows `METERS STALE · x.y s`
(not visible in these simulated captures, which have no meters).
View toggles the input bank, mixer, side panel, and status bar. Every gated
control, menu item, and tile action shows its action and shortcut as a tooltip
and, when disabled, the first blocking gate ("Unavailable: this session lacks
the edit_project permission"). Input tiles have hover, pressed, and keyboard
focus states (Tab to focus, Enter/Space selects Preview) and a right-click menu
whose header shows the input's key, ellipsized name, and tally badge, and
whose footer explains each disabled group in one line (for example "Rename,
Move, and Remove unavailable: this session lacks the edit_project permission"). Tile
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

Window sizes: the side panel takes at most a quarter of the window (320-560
px). At 2560x1440 the monitors, the input bank tiles (four columns of about
290 px), and the mixer faders (up to 620 px of travel) grow to use the space.
At 1280x720 the switcher row keeps its full height and the console scrolls to
reach the lower row; the lower row is at least 370 px tall so the bank and
mixer cards stay level, and narrow card headers drop the gesture hint before
the counts and lock chips.

| File | What it shows |
|---|---|
| `studio-console-monitors.png` | 1920x1080 after `CUT` and a Camera 1 Wide tile click: both wells show `SIMULATED` encoded frames (Camera 1 Wide on Preview, Camera 2 Close on Program), and the two tiles borrow those frames. |
| `studio-console-monitors-scene.png` | 1920x1080 with the Main Scene input on Program: Preview shows a simulated frame of Camera 2 Close and Program the simulated composite of the scene's layers. Both recorders `STOPPED`, the HLS ladder amber `NOT LIVE` (desired on, no transport sample), RTMP Backup `OFF`. |
| `studio-console-overview.png` | 1920x1080 overview: REC 2 recording (red lamp, solid `RECORDING` chip, header `1 / 2 RECORDING`, red `REC` badge lit), overlay O1 taken from Preview and confirmed `ON AIR` (also badged on the Program header), the HLS ladder `NOT LIVE` with its `STOP` key, the stopped RTMP target with `START`. |
| `studio-console-manual-transition.png` | 1920x1080 after manual `FADE` and a T-bar drag: header `T-BAR 39%`, the amber status card `FADE · Main Scene ▶ Camera 2 Close · 39%`, and the active `COMMIT`/`CANCEL` keys while automatic transitions are blocked. |
| `studio-console-fade-to-black.png` | 1920x1080 after `FTB`: the lit `FTB ON` key, the realized status line `PROGRAM BLACK` with its full red level bar, and the `FTB` badge on the Program canvas. |
| `studio-console-streams.png` | 1920x1080 after clicking `START` on the RTMP Backup row: both targets desired on with `STOP` keys and amber `NOT LIVE` chips, and the Output menu with `NOT LIVE` beside both Stop items and `STOPPED` beside the recorder items. |
| `studio-console-stingers.png` | 1920x1080, Stingers tab after choosing Playback Clip in the Configure media picker and clicking `ASSIGN` for S1: the S1 card lit green `READY` with its media name, the Configure card's green `READY` chip, enabled `CLEAR`, and the replicated cut point (15 fr), preload, audio, and fallback. |
| `studio-console-top.png` | The default 1600x940 window, idle: the top-bar padlock (`UNLOCKED`) with disabled Undo/Redo keys (the operator session lacks `edit_project`), both recorder rows `STOPPED`, overlays collapsed to one row per channel. |
| `studio-console-stopped.png` | The compact 1280x800 layout: the side panel narrows to 320 px so the monitors keep their width; the input bank header keeps `7 SOURCES` and drops the gesture hint; the mixer scrolls inside its card and the Master strip stays pinned. |
| `studio-console-compact-720.png` | 1280x720: the same compact layout at the smallest supported height; the console scrolls to reach the lower row. |
| `studio-console-recording.png` | 1600x940 with REC 2 `RECORDING` / `STOP` and the Stingers tab: eight empty slot cards (S1 outlined), the shared duration line, and the Configure card listing the defaults `ASSIGN` would apply. |
| `studio-console-menu.png` | 1920x1080 with the Output menu open: transitions with their keys, Fade to black `B`, the disabled manual T-bar Commit/Cancel, recorder items with `STOPPED`, stream items with `NOT LIVE`/`OFF`, and the Stingers submenus. |
| `studio-console-edit-menu.png` | 1920x1080, admin session after two renames and an undo, with Host Laptop's own padlock engaged: Edit › History with `Undo edit at REV 10  Ctrl+Z` and `Redo edit at REV 11  Ctrl+Shift+Z`, `Lock project edits` with `UNLOCKED` in the state column, and under Preview input (Camera 2 Close) `Move later` disabled with an amber `LOCKED` (its neighbour is locked). The enabled Undo/Redo keys sit beside the padlock in the top bar. |
| `studio-console-locked.png` | The same session after `remote-lock on`: the amber `LOCKED` padlock with disabled Undo/Redo keys, `EDITS LOCKED` in the status bar and the input bank header (with a padlock badge), a padlock badge on every tile's name bar, and on the Stingers tab the Configure card's padlock badge with `ASSIGN`/`CLEAR` disabled. Switching, audio, and output keys stay live. |
| `studio-console-mixer.png` | Mixer crop at 1920x1080 with Camera 1 Wide at -6.0 dB, Camera 2 Close muted and balanced L25 (lit red M, red cap index line, grey fill), Guest Remote soloed at +4.5 dB with AFV off and 480 samples delay (lit amber S), Lower Third at -18.0 dB with AFV off, Playback Clip muted at -inf: two-line name plates with tally-tinted key caps, peak readouts (`--` without meter samples), meter tick gutters, fader scales with the brighter unity mark, and the Master scale. |
| `studio-console-2560.png` | 2560x1440: larger monitors, input tiles of about 290 px in four columns, and mixer faders with the full 620 px travel; the side panel stops at 560 px. |
| `studio-console-portrait.png` | 1920x1080 with the demo project's video settings changed to 1080x1920 (Main Scene layers refitted): both wells grow to half the console height and pillarbox the 202x360 portrait frames; the tiles borrowing the frames pillarbox them too. |
| `studio-console-shortcuts.png` | Help › Keyboard shortcuts: the green `Keys active` lamp, the reference generated from the same table the console's key handler is tested against, with fixed-width key caps (including Ctrl+Z / Ctrl+Shift+Z). |
| `studio-console-input-menu.png` | Right-click menu on the Host Laptop tile: key cap and name header, Select on Preview / Cut to Program enabled, Rename, Move, and Remove disabled, and the footer note that the operator session lacks `edit_project`. |
| `studio-console-add-input.png` | 1920x1080 admin session after clicking the `+` tile: the Add input dialog with `SOLID COLOUR` selected, the suggested name "Solid Colour 1", the amber preset chosen (`#f0a828`), and the preview swatch tagged `INPUT 8` / `SILENT`. |
| `studio-console-add-input-bars.png` | The same dialog for a second add with `COLOUR BARS`: the suggestion "Colour Bars 1", the bars preview tagged `INPUT 9`, no colour row. |
| `studio-console-input-added.png` | After both adds and selecting Solid Colour 1 on Preview: tiles 8 (Solid Colour 1, `PVW`) and 9 (Colour Bars 1), the `+` tile with `9 / 256`, `9 SOURCES`, and a mixer strip for the new solid input. |
| `studio-console-remove-input.png` | Remove input confirmation for Colour Bars 1 (input 9): the question, the warning that removal clears redo history and cannot be undone, and the red `REMOVE` key. |
| `studio-console-input-refused.png` | After removing Colour Bars 1 and then asking to remove Camera 1 Wide: the single refusal toast `Remove refused (conflict): "Camera 1 Wide" is a layer source of scene 100. Remove that reference offline first.` and the bank back at `8 / 256`. |
| `studio-console-inputs-menu.png` | Inputs menu with the Catalogue group: `Add input…` and the `Remove input` submenu, where Main Scene (`PGM`) and Solid Colour 1 (`PVW`) are disabled with their tally marks. |
| `studio-console-add-input-disabled.png` | Operator session hovering the dimmed `+` tile: the tooltip "Add input… A solid colour or colour bars, appended to the end of the bank. Unavailable: this session lacks the edit_project permission". |
| `studio-console-reconnecting.png` | 1600x940 Studio started with `--connect` to an address with no daemon: the Reconnecting card with attempt count, retry delay, the last error, and the disabled-controls footer over the dimmed monitors; `NO STATE` chips, `— TARGETS`, `— SOURCES`, centered empty states for the input bank and mixer, and "No synchronized show" in the top bar (no edit group without synchronized state). |

In this round the `-add-input`, `-add-input-bars`, `-input-added`,
`-remove-input`, `-input-refused`, `-inputs-menu`, and `-add-input-disabled`
captures are new, and `-monitors`, `-monitors-scene`, `-manual-transition`,
`-top`, `-stopped`, `-compact-720`, `-menu`, `-edit-menu`, `-locked`,
`-shortcuts`, and `-input-menu` were regenerated to show the `+` tile, the
Remove menu entries, and the updated shortcut text. `-overview`,
`-fade-to-black`, `-streams`, `-stingers`, `-recording`, `-2560`, and
`-portrait` are from the previous round and do not yet show the `+` tile;
`-mixer` and `-reconnecting` are unchanged. Pointer hover is not
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

The edit-menu, locked, and mixer captures run Studio with `--role admin`
(the default operator role lacks `edit_project`), then drive the daemon with
`freemix-cli remote-rename 127.0.0.1:7000 6 "Playback Clip B"`, a rename
back, `remote-rename … 3 "Host Laptop 2"`, `remote-undo`, `remote-lock … on
--input 3`, click `Edit`; then `remote-lock … on` and click the `STINGERS`
tab; then `remote-lock … off` and `remote-audio-strip` for inputs 1, 2, 4,
5, and 6 before cropping the mixer. The 2560 capture sizes the screen and
window to 2560x1440. The portrait capture edits the demo project's
`project.settings.video` to 1080x1920 (and the Main Scene layer geometry to
fit) in `project.json` before launching Studio.

The input catalogue captures also run with `--role admin`: click the `+` tile
for `-add-input.png` (pick the amber preset), click `ADD INPUT`, open it again
and choose `COLOUR BARS` for `-add-input-bars.png`, add, and click the Solid
Colour 1 tile for `-input-added.png`; right-click Colour Bars 1 and choose
`Remove…` for `-remove-input.png`, confirm, then choose Camera 1 Wide's
`Remove…` and confirm for `-input-refused.png`; open `Inputs` and hover
`Remove input` for `-inputs-menu.png`. `-add-input-disabled.png` uses the
default operator role and hovers the `+` tile.

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
