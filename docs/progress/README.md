# Progress screenshots

Captured from the native `freemix-studio` console running headless on this
development host (Linux aarch64, Xvfb + Mesa lavapipe software Vulkan). The
supervised daemon runs in simulated mode and sends no audio meters, so a
normal build reports `METERS UNAVAILABLE` with unlit meter wells. The UI
round 7 captures were taken with a synthetic meter feed (see Reproducing):
those meter levels are a Studio-side test signal, not daemon audio. The
Preview/Program monitors show
the daemon's encoded-fallback `monitor_frame` images (Protocol 2.21, at most
640x360) letterboxed into each 16:9 well: in simulated mode each frame is a
render of the realized simulated source, badged `SIMULATED` and captioned
`SIMULATED RENDER · {w}×{h} encoded preview`; the daemon composites scene
layers and, on Program, active overlays into it, and its tooltip states that
Fade to Black is not composited. A tap with no frame
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
name on hover), a peak-hold readout, a broadcast-style segmented meter, a
console fader, the gain/balance/delay entries, and LED keys for M (red), S (amber), and AFV
(accent): lit keys show a lit LED bar, a tinted fill, and a coloured border;
unlit keys have a dark LED and a dim label. The fader has a taper that gives
most travel to the range around unity (0 dB at 72 %), a slot with scale
marks (+10, 0, 10, 20, 40, 60, ∞; unity longer and brighter), a trailing
level fill (grey while muted), and a ridged two-tone cap with a white index
line (red while muted) and an accent outline while dragged or focused. Drags
are relative, so grabbing the cap never jumps the gain; double-click returns
to 0 dB and Up/Down nudge 0.5 dB. Meters use only the per-channel peak and RMS the daemon already sends.

The meters (UI round 7) use an expanded dBFS scale like a programme meter:
-60 to -24 dBFS takes the lower third and -24 to 0 dBFS the upper two
thirds, with scale ticks at 0, -6, -12, -18, -24, -36, -48, and -60 in a
gutter beside every meter and signed labels (`0` in red) on the Master
strip; on a short meter the crowded middle labels drop out while `0` and
`-60` stay. The segment count follows the meter height (one segment per
about 4 px). Segments are green below -18 dBFS, amber from -18, and red
from -6; unlit segments keep a faint zone tint so the zones read on a quiet
meter. Each bar lights its RMS segments at full brightness and the segments
between RMS and the latest peak dimly, with a thin latest-peak line; the
held-peak marker holds 1.5 s, then falls 20 dB/s. The clip lamp, the
readout (red `CLIP`), and the meter outline latch at 0 dBFS until clicked
(or `RESET n CLIPS`). The scale mapping, segment lighting, label
thinning, hold, decay, and latch logic are unit-tested.

The console layout continues with a side panel
whose outputs card lists one fixed-height status row per recorder slot and
per stream target, above Overlays/Stingers/Titles/Playlist tabs; and a bottom status
bar with the latest result/notice, keyboard hints, and a health strip.

The Overlays tab (UI round 8) draws its eight channels as compact 30 px rows
(channel, state chip, source, `TAKE`, `TAKE OFF`), so all eight fit beside
the outputs card; a row grows only while its details are open.

Health strip (UI round 8). The trailing end of the status bar carries small
status lamps derived only from data Studio already receives, each with a
tooltip: `PGM FEED` (green while a Program monitor frame is under one second
old, amber `PGM FEED 2.3 s` once it lapses, unlit `NO PGM FEED`; the tooltip
names the frame size and age), `METERS` (green / amber `METERS STALE` /
unlit `NO METERS`), and, when the show has stream targets, `TX {rate}`: the
total muxed bitrate measured by Studio from the growth of each target's
`muxed_bytes` counter between transport samples (`TX —` until two samples a
fifth of a second or more apart exist, or after five seconds without one),
red while a target is live, amber while desired but not live. `DROP n`
(amber) appears once the stream outputs report dropped A/V pairs and `HLS
DISK 1.5 GB` shows the lowest free space a local HLS output reports (amber
below 2 GB, red below 512 MB). The protocol reports no round-trip time,
recorder file size, or recorder disk space, so the strip shows none. The key
hints read `… · ? ALL SHORTCUTS` and shorten to `? SHORTCUTS` (full hints in
the tooltip) when the leading message needs the width.

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
detail beside it, and exactly one `START`/`STOP` key. Red is reserved for
on air (UI round 7): an idle `START` key is a neutral console key, the
`STOP` key of a recording recorder or live stream is lit red like its lamp,
and the `STOP` key of a requested-but-not-live stream has an amber outline.
The input tiles' `CUT` keys and the transition column's `CUT` key are
neutral (the take key is the brightest neutral key), and `FTB` is an unlit
key until Program is going or gone black, when it lights solid red. Each
monitor header names its bus on a solid tally chip (`PREVIEW` black on
green, `PROGRAM` white on red) with the source name in larger type, legible
across the room. Recorder rows show
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
(not visible in these captures, whose synthetic feed never stalls).
View toggles the input bank, mixer, side panel, and status bar; the side
panel's left edge drags to resize it (300 px up to a quarter of the window,
at most 560 px). Studio remembers the shown panels and the dragged width in
`studio.conf` (`layout.input_bank = shown`, `layout.mixer = hidden`, …,
`layout.side_panel_width = 480` or `auto`) and restores them at the next
launch; a narrower window clamps the drawn width without forgetting the
choice. View › Reset layout (enabled whenever the layout differs from the
default; its tooltip says what it restores) shows every panel and returns the
side panel to its default width, and a note under it says the layout is
remembered on this console. Changing only the layout never writes back a
`--lang`/`--clock` session override, and vice versa. Every gated
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
(also `?` or F1 from anywhere, in any connection state, unless a text field,
menu, or other dialog has the keyboard; the same key closes it) leads with a lamp stating whether the keys currently act (and why not), lists
fixed-width key caps beside their actions in five groups (Switching,
Transitions, Editing, Input bank, Console: `? · F1` and `Esc`; short windows
such as 1280x720 scroll the key list), and ends with how to close it. The
side panel is at most a quarter of the window wide (minimum 320 px), so the
monitors keep their width in compact windows. Command rejections, notices, and
terminal-uncertainty notices raise bounded, auto-expiring toasts at the
bottom-right while the console is Ready; the status bar keeps the latest
message. With the synthetic meter feed, peak-hold markers, the dim peak
band, and the latched clip lamp of Guest Remote (strip 4 peaks into 0 dBFS
every six seconds) are visible in the round 7 captures.

Window sizes: the side panel takes at most a quarter of the window (320-560
px). At 2560x1440 the monitors, the input bank tiles (four columns of about
290 px), and the mixer faders (up to 620 px of travel) grow to use the space.
At 1280x720 the switcher row keeps its full height and the console scrolls to
reach the lower row; the lower row is at least 370 px tall so the bank and
mixer cards stay level, and narrow card headers drop the gesture hint before
the counts and lock chips.

| File | What it shows |
|---|---|
| `studio-console-monitors.png` | 1920x1080 after `CUT` and a Camera 1 Wide tile click, hovering the Program well: both wells show `SIMULATED` frames (Camera 1 Wide on Preview, Camera 2 Close on Program) under the solid `PREVIEW`/`PROGRAM` tally chips, the two tiles borrow those frames, and the tooltip states that Fade to Black is not composited. |
| `studio-console-monitors-scene.png` | 1920x1080, the demo show as launched: Main Scene on Program (the simulated composite of its layers), Camera 2 Close on Preview, the neutral `CUT` and `FTB` keys, neutral `START` keys on both stopped recorders, the HLS ladder `NOT LIVE` with an amber-outlined `STOP`, and the lit meter bridge with Guest Remote's latched `CLIP` and `RESET CLIP` in the mixer header. |
| `studio-console-overview.png` | 1920x1080 overview: REC 2 recording (red lamp, solid `RECORDING` chip, lit red `STOP` key, header `1 / 2 RECORDING`, red `REC` badge lit), overlay O1 taken from Preview and confirmed `ON AIR` with its `TAKE OFF` key (composited into the Program frame and badged on the Program header), the HLS ladder `NOT LIVE` with its amber `STOP`, the stopped RTMP target with a neutral `START`. |
| `studio-console-manual-transition.png` | 1920x1080 after manual `FADE` and a T-bar drag: the amber `T-BAR 46%` header, the amber status card naming the transition, sources, and position, and the active `COMMIT`/`CANCEL` keys while automatic transitions are blocked. |
| `studio-console-fade-to-black.png` | 1920x1080 after `FTB` (with REC 2 recording and O1 on air): the lit red `FTB ON` key, the realized status line `PROGRAM BLACK` with its full red level bar, and the `FTB` badge on the Program canvas. |
| `studio-console-streams.png` | 1920x1080 after clicking `START` on the RTMP Backup row: both targets desired on with amber `NOT LIVE` chips and amber-outlined `STOP` keys, and the Output menu with `NOT LIVE` beside both Stop items and `RECORDING` beside Stop recorder 2. |
| `studio-console-stingers.png` | 1920x1080, Stingers tab after choosing Playback Clip in the Configure media picker and clicking `ASSIGN` for S1: the S1 card lit green `READY` with its media name, the Configure card's green `READY` chip, enabled `CLEAR`, and the replicated cut point (15 fr), preload, audio, and fallback. |
| `studio-console-titles.png` | 1920x1080 `--role admin`, Titles tab (Protocol 2.24) listing Lower Third (taken to O1, red `ON AIR` badge, its bar composited into the simulated Program frame) and Score Bug (`OFF AIR`); the Lower Third editor with text, colour (swatch + `#RRGGBBAA`), and number fields, the edited Role and Bar colour fields marked `EDITED` with per-field `Discard` keys and an accent marker, the `PREVIEWED` frame showing the draft values to air, the `CHANNEL` picker, `UPDATE LIVE` and `OFF AIR` keys, and the disabled `TAKE` tooltip `Unavailable: the title is on air`. |
| `studio-console-playlist.png` | 1920x1080 `--role admin`, Playlist tab (Protocol 2.26) after Studio restarted against a playlist already running in the simulated daemon (the late-join `playlist_state`): the `PLAYING` chip, the red `REMAINING` countdown in timecode with `of 00:00:45:00`, `ON AIR · Guest Remote` and the green `Next: Main Scene` line, loop `ALL`, the `START`/`STOP`/`PAUSE`/`PREVIOUS`/`NEXT` keys (Start disabled while running), five entries with transition and monospace duration, entry 04 lit red `ON AIR` with its row countdown, entry 05 lit green `NEXT`, the disabled entry 03 marked `SKIPPED`, and the New entry form disabled with "Unavailable: the playlist is running; stop it to change the list". |
| `studio-console-top.png` | The default 1600x940 window, idle: the top-bar padlock (`UNLOCKED`) with disabled Undo/Redo keys (the operator session lacks `edit_project`), both recorder rows `STOPPED` with neutral `START` keys, overlays collapsed to one row per channel. |
| `studio-console-stopped.png` | The compact 1280x800 layout: the side panel narrows to 320 px so the monitors keep their width; the input bank header keeps `7 SOURCES` and drops the gesture hint; the mixer scrolls inside its card and the Master strip stays pinned, its short scale dropping the crowded `-48` label. |
| `studio-console-compact-720.png` | 1280x720: the same compact layout at the smallest supported height; the console scrolls to reach the lower row. |
| `studio-console-recording.png` | 1920x1080 with REC 2 `RECORDING` / lit `STOP`, both streams `NOT LIVE`, and the Stingers tab: eight empty slot cards (S1 outlined), the shared duration line, and the Configure card listing the defaults `ASSIGN` would apply. |
| `studio-console-menu.png` | 1920x1080 with the Output menu open: transitions with their keys, Fade to black `B`, the disabled manual T-bar Commit/Cancel, recorder items with `STOPPED`, stream items with `NOT LIVE`/`OFF`, and the Stingers submenus. |
| `studio-console-edit-menu.png` | 1920x1080, admin session after two renames and an undo, with Host Laptop's own padlock engaged: Edit › History with `Undo edit at REV 10  Ctrl+Z` and `Redo edit at REV 11  Ctrl+Shift+Z`, `Lock project edits` with `UNLOCKED` in the state column, and under Preview input (Camera 2 Close) `Move later` disabled with an amber `LOCKED` (its neighbour is locked). The enabled Undo/Redo keys sit beside the padlock in the top bar. |
| `studio-console-locked.png` | The same session after `remote-lock on`: the amber `LOCKED` padlock with disabled Undo/Redo keys, `EDITS LOCKED` in the status bar and the input bank header (with a padlock badge), a padlock badge on every tile's name bar, and on the Stingers tab the Configure card's padlock badge with `ASSIGN`/`CLEAR` disabled. Switching, audio, and output keys stay live. |
| `studio-console-mixer.png` | Mixer crop at 1920x1080 (admin) with Camera 1 Wide at -6.0 dB, Camera 2 Close muted and balanced L25 (lit red M, red cap index line, grey fill), Guest Remote soloed at +4.5 dB with AFV off and 480 samples delay (lit amber S), Lower Third at -18.0 dB with AFV off, Playback Clip muted at -inf, on the synthetic meter feed: expanded-scale meters with green/amber/red zones, bright RMS and dim peak segments, held-peak markers, Guest Remote's latched clip lamps and red `CLIP` readout, `RESET CLIP`, and the signed Master scale `0 … -60`. The synthetic levels ignore strip gain and mute. |
| `studio-console-2560.png` | 2560x1440: larger monitors, input tiles of about 290 px in four columns, mixer faders with the full 620 px travel, and 120-segment meters; the side panel stops at 560 px. |
| `studio-console-portrait.png` | 1920x1080 with the demo project's video settings changed to 1080x1920 (Main Scene layers refitted): both wells grow to half the console height and pillarbox the 202x360 portrait frames; the tiles borrowing the frames pillarbox them too. |
| `studio-console-shortcuts.png` | The keyboard reference opened with `?`: the green `Keys active` lamp, the reference generated from the same table the console's key handler is tested against, with fixed-width key caps (including Ctrl+Z / Ctrl+Shift+Z) and the new Console group (`? · F1`, `Esc`). |
| `studio-console-spanish-shortcuts.png` | The same reference in Spanish, opened with F1 during the `--lang es` session (REC 2 recording, O1 on air and open): `Atajos de teclado`, `Teclas activas`, the `Consola` group. |
| `studio-console-health-strip.png` | 1920x1080 demo show hovering the `TX —` lamp: the status bar's health strip (`PGM FEED` and `METERS` green, `TX —` amber because the HLS ladder is desired but not live and the simulated daemon sends no transport samples) and its tooltip `Streams: 0 of 2 live · no recent transport samples to measure the rate · 0 A/V pairs dropped`. The key hints end in `? ALL SHORTCUTS`. |
| `studio-console-layout-restored.png` | Studio restarted on a `studio.conf` saved after dragging the side panel edge to 480 px and hiding the mixer from View: the restored layout (input bank across the full width, 480 px side panel) with View open, the enabled `Reset layout` item and its tooltip, and the layout note. |
| `studio-console-input-menu.png` | Right-click menu on the Host Laptop tile: key cap and name header, Select on Preview / Cut to Program enabled, Rename, Move, and Remove disabled, and the footer note that the operator session lacks `edit_project`. |
| `studio-console-add-input.png` | 1920x1080 admin session after clicking the `+` tile: the Add input dialog with `SOLID COLOUR` selected, the suggested name "Solid Colour 1", the amber preset chosen (`#f0a828`), and the preview swatch tagged `INPUT 8` / `SILENT`. |
| `studio-console-add-input-bars.png` | The same dialog for a second add with `COLOUR BARS`: the suggestion "Colour Bars 1", the bars preview tagged `INPUT 9`, no colour row. |
| `studio-console-input-added.png` | After both adds and selecting Solid Colour 1 on Preview: tiles 8 (Solid Colour 1, `PVW`) and 9 (Colour Bars 1), the `+` tile with `9 / 256`, `9 SOURCES`, and a mixer strip for the new solid input. |
| `studio-console-remove-input.png` | Admin session after both adds: the Remove input confirmation for Colour Bars 1 (input 9, position 9), the question, the warning that removal clears redo history and cannot be undone, and the red `REMOVE` key (tooltip hint `[Enter]`, Cancel `[Esc]`). |
| `studio-console-input-refused.png` | After removing Colour Bars 1 and then asking to remove Camera 1 Wide: the single refusal toast `Remove refused (conflict): "Camera 1 Wide" is a layer source of scene 100. Remove that reference offline first.` and the bank back at `8 / 256`. |
| `studio-console-inputs-menu.png` | Inputs menu with the Catalogue group: `Add input…` and the `Remove input` submenu, where Main Scene (`PGM`) and Solid Colour 1 (`PVW`) are disabled with their tally marks. |
| `studio-console-add-input-disabled.png` | Operator session hovering the dimmed `+` tile: the tooltip "Add input… A solid colour or colour bars, appended to the end of the bank. Unavailable: this session lacks the edit_project permission". |
| `studio-console-reconnecting.png` | 1600x940 Studio started with `--connect` to an address with no daemon: the Reconnecting card with attempt count, retry delay, the last error, and the disabled-controls footer over the dimmed monitors; `NO STATE` chips, `— TARGETS`, `— SOURCES`, centered empty states for the input bank and mixer, and "No synchronized show" in the top bar (no edit group without synchronized state); the health strip's unlit `NO PGM FEED` and `NO METERS` lamps. |
| `studio-console-spanish.png` | 1920x1080 demo show with Studio started with `--lang es`, REC 2 recording, and O1 taken and opened: the `PREVIO`/`PROGRAMA` tally chips, the lit red `DETENER` key of REC 2, neutral `INICIAR` keys, the overlay keys `TOMAR` / `RETIRAR` beside the `APAGADO` / `AL AIRE` state chips (button and state are distinct catalog keys since round 7), and the open O1 card's Spanish diagnostic line (`O1 · pedido: Camera 2 Close, activo, opacidad 255, cola 0, completo, sin borde · confirmado: …`), plus the Spanish mixer header (`RMS · RETENCIÓN · CLIP`, `BORRAR CLIP`). |
| `studio-console-spanish-menu.png` | The same session with `Salida` open: every transition, Fade to black, T-bar, recorder (`Detener grabador 2` / `GRABANDO`), stream, and Stinger item worded in Spanish. |
| `studio-console-spanish-input-refused.png` | 1920x1080 `--lang es --role admin` after asking to remove Camera 1 Wide: the localized refusal toast `Baja rechazada (conflict): «Camera 1 Wide» está en uso: a layer source of scene 100. Quite esa referencia sin conexión primero.` (the daemon's own reference detail is quoted as sent). |
| `studio-console-overlay-details.png` | 1920x1080 with O1 on air and its card open: `TAKE` / `TAKE OFF`, the UPDATE/queue/transition/position/border keys, OUTPUTS, and the localized diagnostic line `O1 · desired: Camera 2 Close, active, opacity 255, queue 0, full, no border · confirmed: …` naming the source instead of its raw id. |
| `studio-console-spanish-transition.png` | 1920x1080 `--lang es` after manual `FUNDIDO` and a T-bar drag: the Spanish status card, `COMPLETAR`/`CANCELAR`, and the hover tooltip with requested/realized position. |
| `studio-console-spanish-stingers.png` | 1920x1080 `--lang es` with Fade to Black on (`PROGRAMA EN NEGRO`) and the Stingers tab: the slot cards, the Configure card (`ASIGNAR`/`VACIAR`, cut point in `fr`, preload, audio, fallback), and the S4 card tooltip. |
| `studio-console-spanish-titles.png` | 1920x1080 `--lang es --role admin`, Titles tab after a second Studio (`--connect`) sent `UPDATE LIVE` with Name "M. Ruiz" while this console held the draft "Marta Ruiz López": the `CONFLICTO` banner, the `MÍO`/`SUYO` comparison for Name, `REBASAR` and `DESCARTAR TODO`, and the disabled `ACTUALIZAR EN VIVO` tooltip naming the server-side change. |
| `studio-console-spanish-playlist.png` | 1920x1080 `--lang es --role admin`, the Lista tab after `remote-playlist … pause` and a Studio restart: `EN PAUSA`, the amber `RESTANTE` countdown held, `REANUDAR` lit amber in place of Pausa, `AL AIRE`/`SIGUIENTE`/`OMITIDO` row badges, and the Spanish editor and unavailable reason. |
| `studio-console-spanish-compact.png` | 1280x800 `--lang es` with the Stingers tab: long Spanish labels and details ellipsize inside fixed-width keys and chips instead of overflowing; tooltips carry the full text. |
| `studio-console-spanish-add-input.png` | 1920x1080 `--lang es --role admin`: the Añadir entrada dialog with `COLOR SÓLIDO`, the suggested name "Color sólido 1", the amber preset, and the preview tagged `ENTRADA 8` / `SIN AUDIO`. |
| `studio-console-spanish-remove-input.png` | The Quitar entrada confirmation for "Color sólido 1" (entrada 8, posición 8) with the Spanish warning and the red `QUITAR` key. |
| `studio-console-spanish-reconnecting.png` | 1600x940 `--lang es --connect` with no daemon: the Spanish Reconectando card (attempt, retry delay, last error, disabled-controls footer). |
| `studio-console-preferences.png` | 1920x1080, File › Preferences… (or Ctrl+,) after choosing the 12-hour clock (the top-bar clock reads `10:20:35 PM`) and pressing Tab eight times: Language (each language named in itself), Clock, Display with the UI scale keys (`90%` to `150%`, `100%` selected) and the keyboard focus ring on `125%`, the High-contrast theme checkbox, the note that the choices live in `studio.conf` and that `--lang`/`--clock` win for their session, and the enabled `Reset to defaults` and `Done` keys. |
| `studio-console-high-contrast.png` | 1920x1080 with `ui.high_contrast = on` in `studio.conf` (REC 2 recording, O1 on air): black and near-black surfaces, white and light-grey text, and light borders around every card and key, while the tally and status hues (red Program, `RECORDING`, `ON AIR`, green Preview, amber `NOT LIVE`) are unchanged. |

UI round 9 added a Preferences window and a high-contrast theme, and
audited keyboard access. File › Preferences… (Ctrl+, or Cmd+, anywhere a
text field does not have the keyboard) sets the language, the 12/24-hour
clock, the UI scale (90, 100, 110, 125, or 150 %, applied at once through
egui's zoom; Ctrl+= / Ctrl+- step through those scales and Ctrl+0 returns to
100 %, with a toast naming the new scale), and the high-contrast theme;
`Reset to defaults` returns to the system language, the 24-hour clock, 100 %,
and the standard theme. Studio keeps the choices in `studio.conf`
(`ui.scale = 125`, `ui.high_contrast = on`; a hand-edited scale is clamped
into 90-150 %, and unparsable values are ignored) and now writes each
preference only when that very preference changes, so a `--lang` or
`--clock` override is never written back when the operator changes the
clock, the scale, the theme, or the layout. The high-contrast palette uses
black and near-black surfaces, white and light-grey text, and light borders;
a unit test computes the WCAG contrast ratio of every text colour against
every surface (at least 4.5:1) and of borders and the focus ring (at least
3:1). Tally and status hues are shared by both palettes. Every focused
button, key, and field now shows a 2 px accent focus ring (also on the
custom-painted console keys whose own fill hid egui's focused style);
buttons take focus only from Tab, so a click never leaves a ring. Dialog
close keys are 24 x 24 targets that come last in the Tab order (Keyboard
shortcuts, About, Add input, Remove input, Preferences); Add input tabs
Cancel then Add and Remove input tabs Cancel then Remove, left to right as
drawn, and Remove input opens with Cancel focused so a stray Enter never
removes an input; an overlay row tabs `TAKE` then `TAKE OFF`; the toast
dismiss key is a 24 x 24 target; and the overlay disclosure triangle has a
tooltip. The keyboard reference's Console group lists `Ctrl+,`,
`Ctrl+= · Ctrl+-`, and `Ctrl+0`. Every new string is a catalog key with
Spanish. This round regenerated `-monitors`, `-overview`,
`-manual-transition`, `-fade-to-black`, `-streams`, `-stingers`,
`-titles`, `-recording`, `-menu`, `-mixer`, `-2560`, `-portrait`,
`-input-menu`, `-overlay-details`, `-spanish-menu`,
`-spanish-input-refused`, and `-spanish-titles`, and added `-preferences`
and `-high-contrast`. The captures from round 8 predate the focus ring, the
24 x 24 dialog close keys, and Remove input's focused Cancel key.

UI round 8 added the status bar health strip, the `?`/F1 keyboard reference
with its Console group, a remembered panel layout (shown panels and the
dragged side panel width in `studio.conf`, View › Reset layout), and compact
overlay rows. Every new string is a catalog key with Spanish. This round
regenerated `-monitors-scene`, `-top`, `-stopped`, `-compact-720`,
`-shortcuts`, `-spanish`, `-edit-menu`, `-locked`, the Add/Remove input
captures (`-add-input`, `-add-input-bars`, `-input-added`, `-remove-input`,
`-input-refused`, `-inputs-menu`, `-add-input-disabled`), `-reconnecting`,
and `-spanish-transition`, `-spanish-stingers`, `-spanish-compact`,
`-spanish-add-input`, `-spanish-remove-input`, and `-spanish-reconnecting`,
and added `-health-strip`, `-layout-restored`, and `-spanish-shortcuts`.
Those captures are from round 8; round 9 regenerated the rest (see above).

UI round 7 reviewed every capture as an operator would. The main fixes:
red is reserved for on air (neutral `START`, tile `CUT`, transition `CUT`,
and idle `FTB` keys; a `STOP` key lit red only while its output is on air),
monitor headers name their bus on a solid tally chip, the meters became
broadcast-grade (expanded dBFS scale with the 0/-6/-12/-18/-24/-36/-48/-60
tick set, zone tints, a dim peak band, label thinning on short meters), the
overlay `OFF` button became `TAKE OFF` (`RETIRAR`), distinct from the `OFF`
(`APAGADO`) state chip, and the overlay diagnostic line, the stream
transport tooltip, the Add/Remove input refusal toasts, and the fallback
`Input {id}` name are now localized. The monitor tooltip now says what the
simulated tap composites (scene layers and Program overlays, never Fade to
Black). This round regenerated `-monitors`, `-monitors-scene`, `-overview`,
`-manual-transition`, `-fade-to-black`, `-streams`, `-stingers`, `-top`,
`-stopped`, `-compact-720`, `-recording`, `-menu`, `-mixer`, `-2560`,
`-portrait`, `-shortcuts`, `-input-menu`, `-spanish`, and `-spanish-menu`
on the synthetic meter feed, and added `-overlay-details` and
`-spanish-input-refused`. `-edit-menu`, `-locked`, the Add/Remove input
captures, `-reconnecting`, and the other Spanish captures are from round 6
or earlier and still show the old red `CUT`/`START`/`FTB` keys, plain bus
labels, and unlit meters.

UI round 6 moved the transition column, Fade to Black, monitor captions,
mixer labels, overlays, Stinger configuration, the connection card, the
Add/Remove input dialogs, and the output-row tooltips into the typed message
catalogs with Spanish translations, and polished those panels: numeric
readouts (T-bar position, durations, dB, peaks) are monospace, fixed-width
keys and chips are sized for the longer Spanish words or truncate with the
full text in the tooltip, and tooltips name the keyboard shortcut in brackets
(`[C]`, `[F]`, `[B]`, `[Esc]`, `[Enter]`, `[↑/↓ ±0.5 dB]`). This round
regenerated `-overview`, `-manual-transition`, `-fade-to-black`, `-stingers`,
`-top`, `-recording`, `-reconnecting`, `-add-input`, `-remove-input`,
`-spanish`, and `-spanish-menu`, and added `-spanish-transition`,
`-spanish-stingers`, `-spanish-compact`, `-spanish-add-input`,
`-spanish-remove-input`, and `-spanish-reconnecting`. The other captures are
from earlier rounds.

In the previous round the `-add-input`, `-add-input-bars`, `-input-added`,
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

The Spanish captures add `--lang es` to the Studio command above (the
language can also be switched at runtime from View › Language, and `--clock
12h` selects the 12-hour clock); `-spanish-menu.png` then clicks `Salida`.
Studio remembers the language and clock chosen in the View menu in
`studio.conf` in the platform config directory (`$XDG_CONFIG_HOME/freemix/`
or `~/.config/freemix/` on Linux); `--lang` and `--clock` override the saved
choice for one session without rewriting it. Point `FREEMIX_STUDIO_PREFS` at
a scratch file when capturing so the captures do not depend on, or change,
your own preferences. The Spanish transition, Stingers, compact, add/remove,
and reconnecting captures repeat the English steps with `--lang es`; the
Spanish shortcuts capture focuses the window (`xdotool windowfocus "$WIN"`)
and presses F1, and the English one presses `?` (`xdotool key question`).

The layout capture drags the side panel's left edge (`xdotool mousemove 1550
700`, `mousedown 1`, `mousemove` in steps to x 1430, `mouseup 1`), unticks
View › Audio mixer, restarts Studio with the same `FREEMIX_STUDIO_PREFS`
file, opens View, and hovers `Reset layout`. The health-strip capture hovers
the `TX` lamp at the right end of the status bar.

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

The Playlist captures run Studio with `--role admin` against the demo show,
then set and start a looping playlist from the CLI (entry 3 disabled):

```sh
freemix-cli remote-playlist 127.0.0.1:7000 set all \
    1:1:1800:cut:0:1,2:2:1200:fade:30:1,3:3:900:wipe:20:0,4:4:2700:slide:24:1,5:7:3600:fade:45:1
freemix-cli remote-playlist 127.0.0.1:7000 start
```

and click the `PLAYLIST` tab (x 1865, y 326 at 1920x1080). For both
captures Studio was restarted on the same project while the playlist ran, so
the tab shows the `playlist_state` that follows the snapshot; the Spanish
capture first runs `remote-playlist 127.0.0.1:7000 pause` and adds `--lang
es`. The simulated daemon executes Pause at its frame cursor, which lags wall
time between playlist boundaries, so a paused entry can show less elapsed
time than actually played.

The UI round 7 captures build Studio with the `capture-meter-signal` feature
(`cargo build -p freemix-studio --features capture-meter-signal`) and run it
with `FREEMIX_STUDIO_CAPTURE_METERS=1`. Studio then feeds its mixer a
deterministic synthetic meter message (each strip at its own level with
slow movement, strip 4 peaking into 0 dBFS every six seconds, Master below
the loudest strip) whenever the daemon sends none; daemon meters always
win. Without both the feature and the variable no synthetic level is ever
shown, and the test suite never sets the variable. The synthetic levels do
not follow strip gain or mute. The Spanish refusal capture runs
`--lang es --role admin`, right-clicks Camera 1 Wide, chooses `Quitar…`, and
confirms with `QUITAR`; the overlay-details capture clicks O1 `TAKE` and
then O1's disclosure triangle.

The titles captures use the demo show with input 5 renamed "Colour Fill"
and two title inputs imported from `.fmtitle` sources (schema 28) drawn with
DejaVu Sans: `lower-third.fmtitle` (fields Name, Role, Bar colour, Score; a
bar whose fill follows Bar colour, with the Name and Role text on it) and
`score-bug.fmtitle` (Home, Away, Home score, Away score):

```sh
D=/usr/share/fonts/truetype/dejavu
freemix-cli rename demo.freemix 5 "Colour Fill"
freemix-cli title-input-add demo.freemix --id 8 --name "Lower Third" \
    --template lower-third.fmtitle \
    --font "DejaVu Sans=$D/DejaVuSans.ttf" --font "DejaVu Sans Bold=$D/DejaVuSans-Bold.ttf"
freemix-cli title-input-add demo.freemix --id 9 --name "Score Bug" \
    --template score-bug.fmtitle --font "DejaVu Sans Bold=$D/DejaVuSans-Bold.ttf"
```

Both run Studio with `--role admin`. The English capture clicks `TITLES`,
`PREVIEW TITLE`, and `TAKE`, types a new Role and Bar colour, previews again,
and hovers `TAKE`. The Spanish capture takes the title the same way, types a
Name draft, then starts a second Studio with `--connect 127.0.0.1:7000
--project-id <the project id in project.json> --role admin` (with its own
`FREEMIX_STUDIO_PREFS`), changes Name there, clicks `UPDATE LIVE`, closes it,
and hovers `ACTUALIZAR EN VIVO` in the first. The Preferences capture presses
Ctrl+, (after `xdotool windowfocus`), clicks `12-hour (AM/PM)`, and presses
Tab eight times; the high-contrast capture writes `ui.high_contrast = on` to
the scratch `FREEMIX_STUDIO_PREFS` file before launching Studio, then starts
REC 2 and takes O1. Typing into Studio's fields with `xdotool type` also needs
`xdotool windowfocus` first.

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
