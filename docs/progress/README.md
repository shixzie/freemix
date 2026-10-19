# Progress screenshots

Captured from the native `freemix-studio` console running headless on this
development host (Linux aarch64, Xvfb + Mesa lavapipe software Vulkan). The
supervised daemon runs in simulated mode and sends no audio meters, so a
normal build reports `METERS UNAVAILABLE` with unlit meter wells. Every
console capture (regenerated on 2026-10-15/16 at Protocol 2.41, with the later additions at Protocol 2.48 and the round 5 additions at Protocol 2.52 and the round 6 additions at Protocol 2.62, see below) on the simulated daemon was taken
with a synthetic meter feed (see Reproducing): those meter levels are a
Studio-side test signal, not daemon audio. The
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

Since Protocol 2.59 every input tile carries its own live thumbnail while the
input bank is visible: Studio subscribes with `input_thumbnail_subscription`
and the daemon sends each input with a picture at most 160x90, about one to
two pictures a second, under a shared bandwidth budget, tagged `FEED` (or
`FEED · SIM` for simulated renders). In simulated mode, scenes, titles, and
simulated sources are real renders, network-stream and capture inputs show a
synthetic tinted card with a sweeping band, and media inputs a card whose
bottom bar follows the playhead. A native daemon samples each source's newest
frame and composes its Scene inputs from those frames; a picture with
transparency shows over black, as Program shows it alone. A thumbnail older than three
seconds is dimmed with an amber `STALE {age}` tag. A system-audio input has no
picture and shows an `audio only` meter placeholder. A Preview or Program tap
frame still wins over the tile's own thumbnail while it is current and shows
that tile's input (`PVW TAP` / `PGM TAP`). A tile with neither keeps its dim
ordinal and reads `no thumbnail`.

Since Protocol 2.36 the transition column also carries a `MORE` key (a menu
of the other mix kinds: V-Wipe, V-Slide, Fly, Fly Turn, Cube, Cube Zoom,
X-Zoom, Merge), a `FAVOURITES` row of four keys (F5-F8, right-click to
assign Cut, any mix, or a Stinger), and, under Fade to Black, an
`Outputs n/m` selector of the outputs Fade to Black affects (Protocol
2.40). The input bank has a toolbar of category filter chips with
`Import files…` and `Categories…`, tiles carry a colour strip and
`PAUSED`/`CLOSED`/`SOURCE OFFLINE` badges, every mixer strip has a `PAN`
knob, strips can carry a label and belong to one of four VCA groups, and
the side panel has a fifth `MEDIA` tab with a transport card per media-file
input (Protocol 2.37-2.40).

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
channel strips that share the panel (wider, up to 150 px, and taller, up
to 1000 px of meter and fader, with finer scale marks from 360 px), the
Monitor and Master strips pinned at the right, and horizontal scrolling only
when compact 82 px strips do not fit. Each strip has a fixed-height name plate (the input's key
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
The console is a docking layout (2026-10-15). Each of the eleven panels
(Preview, Transitions, Program, Input bank, Audio mixer, Recording & streams,
Overlays, Stingers, Titles, Playlist, Media) is a tab in a tile; dragging a
tab onto another tile shows a blue drop preview and re-docks it into that
tab group (middle of the tile or its tab bar) or splits that tile on the
edge it is dropped on, the gaps between tiles drag to resize them, and every
tile keeps a minimum size (its body scrolls below that) while the monitors
keep the canvas aspect. Each tab bar has pop-out and close keys, and a tab's
right-click menu has Pop out and Close panel: a popped-out panel moves into
its own native window titled `<Panel> — FreeMix Studio` with a Dock back key;
closing that window or pressing Dock back returns the panel to its home
place. Console keys work in popped windows too. View › Own windows offers the
same for the Colour grade, Animation, and Layer designer tool windows. View ›
Panels shows or hides each panel, View › Layouts applies the built-in
Default, Compact, and Audio focus layouts or a saved one, saves the current
layout under a name (up to 16, renamed or deleted from the same submenu),
and View › Reset layout (enabled whenever the layout differs from the
default) restores Default and docks every window back. Polish of
2026-10-16: each tab bar starts with a drag grip (hover: "Drag to move;
drop on an edge to split, or in the middle to join its tabs") that moves the
whole tab group at once; double-clicking a tab maximises its tile over the
console (a Restore key joins the tab bar; Esc or another double-click
restores, and Esc still cancels a manual transition first); a popped-out
panel's window header has a grip that, dragged over the console window,
shows the same blue drop preview and docks the panel where it is released
(where the platform reports window positions, so not on Wayland), and a
`Dock back to…` menu listing every docked tile with Into its tab group,
To its left/right, Above it, and Below it, previewed in the console while
hovered. Panel contents follow their tiles: the transition column widens
with its tile (200-360 points, centred beyond that), the monitors are
letterboxed and centred both ways at the canvas shape, the mixer's meter key
ends in an ellipsis (full text on hover) instead of running under its
title, and the input strips start at the left with a rule setting the
pinned Monitor and Master strips apart. The Default layout's columns split
at one height, so both rows' tab bars line up across the console at
1920x1080 and 1280x720, and Preview and Program stay the same width at
both. Studio remembers the
layout in `studio.conf`: `layout.dock` (the tile tree, for example
`h765(v500(…),v500([outputs],[overlays|stingers|titles|playlist|media]))`),
`layout.popouts` and `layout.tool_windows` (each popped window with its size
and position, `program:700x460@1180,520`, and `+max` after a maximised
window's un-maximised size and place, so it reopens maximised), `layout.saved
= name | tree | popouts` per saved layout, plus the status bar, disclosure,
and animation keys. A tree or pop-out list this version does not write falls
back to the Default layout with a notice. A window saved where no monitor
shows at least 64x32 points of its top strip (a monitor was unplugged or
rearranged) reopens in the middle of the primary monitor, shrunk to fit. A
panel popped out of a maximised tile takes the size its tile has among the
others. Showing a popped panel again (View › Panels › Bring to front, or a
menu entry that shows the panel) raises its window. The file is written once
the layout has stayed unchanged for half a second, and on exit, rather than
every frame of a splitter or window drag. On Wayland the compositor neither
reports nor honours window positions, so only popped windows' sizes are
restored there.
Changing only the layout never writes back a
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

Window sizes: (before the docking layout) the side panel took at most a
quarter of the window (320-560 px). At 2560x1440 the monitors, the input bank tiles (four columns of about
290 px), and the mixer faders (up to 1000 px of travel) grow to use the space.
At 1280x720 the switcher row keeps its full height and the console scrolls to
reach the lower row; the lower row is at least 370 px tall so the bank and
mixer cards stay level, and narrow card headers drop the gesture hint before
the counts and lock chips.

| File | What it shows |
|---|---|
| `studio-console-monitors.png` | 1920x1080 after `CUT` and a Camera 1 Wide tile click, hovering the Program well: both wells show `SIMULATED` frames (Camera 1 Wide on Preview, Camera 2 Close on Program) under the solid `PREVIEW`/`PROGRAM` tally chips, the two tiles borrow those frames, and the tooltip names the desired and realized Program source and states that Fade to Black is not composited. |
| `studio-console-monitors-scene.png` | 1920x1080, the demo show as launched: Main Scene on Program (the simulated composite of its layers), Camera 2 Close on Preview, the transition column with `MORE`, the four `FAVOURITES` keys (Fade, Wipe, Fly, Cube), and the Fade to Black `Outputs 1/1` selector, neutral `START` keys on both stopped recorders, the HLS ladder `NOT LIVE` with an amber-outlined `STOP`, the input bank toolbar (`All inputs`, `Import files…`, `Categories…`), the mixer strips with their `PAN` knobs, and Guest Remote's latched `CLIP` with `RESET CLIP` in the mixer header. |
| `studio-console-production.png` | 1920x1080 `--role admin`, a realistic production state (recaptured at Protocol 2.48, schema 45): input categories (Cameras, Remote, Graphics) as filter chips over the bank and colour strips down each tile's leading edge; the looping playlist running on the Playlist tab (`PLAYING`, red `REMAINING -00:08:38:30` of `00:10:00:00`, entry 01 Guest Remote `ON AIR`, entry 02 `NEXT`) has cut Guest Remote to Program, where it airs with the saved library grade "Guest warm" (the white bar reads beige); the animated Lower Third ("Ana Torres", input 8) was previewed and taken to O1 and sits composited over it (`O1` badge on the Program header); REC 2 is `RECORDING`; the mixer shows the strip labels `Host Mic` and `Guest Line`, Camera 1 and 2 panned `L30`/`R30`, the `MON` strip with its `SIP`/`PFL`/`AFL` keys beside Master, and Colour Fill (strip 5, muted) clipped under the pinned `MON` strip. |
| `studio-console-overview.png` | 1920x1080 overview: REC 2 recording (red lamp, solid `RECORDING` chip, lit red `STOP` key, header `1 / 2 RECORDING`, red `REC` badge lit), overlay O1 taken from Preview (Camera 1 Wide) and confirmed `ON AIR` with its `TAKE OFF` key and the `O1` badge on the Program header, the HLS ladder `NOT LIVE` with its amber `STOP`, the stopped RTMP target with a neutral `START`. The manual transition block and the mixer strips show their simpler default behind the `▸ DETAILS` and `▸ GAIN · BAL · DLY` disclosure keys. |
| `studio-console-hover-help.png` | The same session hovering `FADE`: the tooltip says what the key does in operator language and names its shortcut (`Fade Preview to Program over the shared duration  [F]`). Every interactive control carries help like this (RC-018). |
| `studio-console-manual-transition.png` | 1920x1080 after manual `FADE` and a T-bar drag: the amber `T-BAR 69%` header, and, although `▸ DETAILS` is still collapsed, the position field and the active `COMMIT`/`CANCEL` keys take the place of the manual start keys while the transition is in flight, so the input bank and mixer below do not move; the automatic transition, `MORE`, and favourite keys are dimmed. |
| `studio-console-fade-to-black.png` | 1920x1080 after `FTB` (with REC 2 recording and O1 on air): the lit red `FTB ON` key, the realized status line `PROGRAM BLACK` with its full red level bar, and the `FTB` badge on the Program canvas. |
| `studio-console-streams.png` | 1920x1080 after clicking `START` on the RTMP Backup row: both targets desired on with amber `NOT LIVE` chips and amber-outlined `STOP` keys, and the Output menu with `NOT LIVE` beside both Stop items and `RECORDING` beside Stop recorder 2. |
| `studio-console-stingers.png` | 1920x1080, Stingers tab after choosing Playback Clip in the Configure media picker and clicking `ASSIGN` for S1: the S1 card lit green `READY` with its media name, the Configure card's green `READY` chip, enabled `CLEAR`, and the replicated cut point (15 fr), preload, audio, and fallback (both stream targets desired on from the previous step). |
| `studio-console-titles.png` | 1920x1080 `--role admin`, Titles tab listing Lower Third (taken to O1, red `ON AIR` badge, its red bar composited into the simulated Program frame) and Score Bug (`OFF AIR`); the Lower Third editor after typing Role "Host, Evening Broadcast" (the bar colour was set to `#1E5AB4FF` and back to `#C8202CFF`), both fields marked `EDITED` with per-field `Discard` keys, the `PREVIEWED` card showing the draft values to air, the `CHANNEL` picker, `UPDATE LIVE` and `OFF AIR` keys, and the disabled `TAKE` tooltip `Unavailable: the title is on air`. |
| `studio-console-playlist.png` | 1920x1080 `--role admin`, Playlist tab with the looping playlist running in the simulated daemon: the `PLAYING` chip, the red `REMAINING` countdown `-00:00:43:17` of `00:00:45:00`, `ON AIR · Guest Remote` and the green `Next: Main Scene` line, loop `ALL`, the `START`/`STOP`/`PAUSE`/`PREVIOUS`/`NEXT` keys (Start disabled while running), five entries with transition and monospace duration, entry 04 lit red `ON AIR` with the same countdown, entry 05 lit green `NEXT`, the disabled entry 03 marked `SKIPPED`, and the New entry form disabled with "Unavailable: the playlist is running; stop it to change the list". |
| `studio-console-top.png` | The default 1600x940 window, idle: the top-bar padlock (`UNLOCKED`) with disabled Undo/Redo keys (the operator session lacks `edit_project`), both recorder rows `STOPPED` with neutral `START` keys, the transition column with `MORE` and the favourites row, overlays collapsed to one row per channel, the five side tabs (the first reads `OVERL…`), and the console's and mixer's scroll bars visible while idle so the clipped bank rows and mixer strips are discoverable. |
| `studio-console-stopped.png` | The compact 1280x800 layout: the side panel narrows to 320 px so the monitors keep their width, and its five tabs ellipsize (`OVER…`, `STING…`, `PLAYL…`); the input bank header keeps `7 SOURCES` and drops the gesture hint; the mixer scrolls inside its card and the Master strip stays pinned. |
| `studio-console-compact-720.png` | 1280x720: the same compact layout at the smallest supported height; the console scrolls to reach the lower row. |
| `studio-console-recording.png` | 1920x1080 with REC 2 `RECORDING` / lit `STOP`, both streams `NOT LIVE`, and the Stingers tab: eight empty slot cards (S1 outlined), the shared duration line, and the Configure card listing the defaults `ASSIGN` would apply. |
| `studio-console-menu.png` | 1920x1080 with the Output menu open: transitions with their keys, Fade to black `B`, the disabled manual T-bar Commit/Cancel, recorder items with `STOPPED`, stream items with `NOT LIVE`/`OFF`, the `Fade to Black outputs  Outputs 1/1` submenu, the Stingers submenus, and `Stinger slots…`. |
| `studio-console-edit-menu.png` | 1920x1080, admin session after two renames and an undo, with Host Laptop's own padlock engaged: Edit › History with `Undo edit at REV 10  Ctrl+Z` and `Redo edit at REV 11  Ctrl+Shift+Z`, `Lock project edits` with `UNLOCKED` in the state column, and under Preview input (Camera 2 Close) `Move later` disabled with an amber `LOCKED` (its neighbour is locked). The enabled Undo/Redo keys sit beside the padlock in the top bar. |
| `studio-console-locked.png` | The same session after `remote-lock on`: the amber `LOCKED` padlock with disabled Undo/Redo keys, `EDITS LOCKED` in the status bar and the input bank header (with a padlock badge), a padlock badge on every tile's name bar, and on the Stingers tab the Configure card's padlock badge with `ASSIGN`/`CLEAR` disabled. Switching, audio, and output keys stay live. |
| `studio-console-mixer.png` | Mixer crop at 1920x1080 (admin) after opening the header's `▾ GAIN · BAL · DLY` disclosure, with Camera 1 Wide at -6.0 dB, Camera 2 Close muted and balanced L25 (lit red M, red cap index line, grey fill), Guest Remote soloed at +4.5 dB with AFV off and 480 samples delay (lit amber S), Lower Third at -18.0 dB with AFV off, Playback Clip muted at -inf, on the synthetic meter feed. Each strip now has its `PAN` knob above the keys and, in the disclosure, the label field (placeholder: the input name) and the group picker (`No group`); Main Scene scrolls under the pinned Master strip. The synthetic levels ignore strip gain and mute. |
| `studio-console-2560.png` | 2560x1440: larger monitors, input tiles in four columns, mixer faders with the full travel; the side panel stops at 560 px. |
| `studio-console-portrait.png` | 1920x1080 with the demo project's video settings changed to 1080x1920 (Main Scene layers refitted): both wells grow to half the console height and pillarbox the 202x360 portrait frames; the tiles borrowing the frames pillarbox them too. |
| `studio-console-shortcuts.png` | The keyboard reference opened with `?` (1920x1080, `--role admin`; recaptured with the complete key map): the green `Keys active` lamp and two columns of groups generated from the table the key handlers are tested against: Switching, Transitions, Editing, and Input bank (the long tile-menu action wraps in its column) on the left; Cue list (`G`), Recording and streaming (`Ctrl+Shift+R` REC 1, `Ctrl+Shift+O` start all streams, `Ctrl+Shift+K` stop all streams), Windows (`Ctrl+G` Colour, `Ctrl+Shift+A` Animation, `Ctrl+Shift+L` layer designer, `Ctrl+Shift+S` Scripts, `Ctrl+Shift+N` LiveLAN viewers), and Console (`? · F1`, `Esc`, `Ctrl+,`, the scale keys) on the right. Windows narrower than about 1070 px keep one scrolling column. |
| `studio-console-spanish-shortcuts.png` | The same reference in Spanish, opened with F1 (`--lang es`): `Atajos de teclado`, `Teclas activas`, `Grabación y emisión` (`Iniciar o detener la grabadora REC 1`), `Ventanas` (`Abrir o cerrar la ventana Guiones`, `… Espectadores LiveLAN`), `Consola`, and key names in Spanish (`Ctrl+Mayús+R`, `Ctrl+Mayús+S`, `Fin`, `Tab · Intro`). |
| `studio-console-health-strip.png` | 1920x1080 demo show hovering the `TX —` lamp: the status bar's health strip (`PGM FEED` and `METERS` green, `TX —` amber because the HLS ladder is desired but not live and the simulated daemon sends no transport samples) and its tooltip `Streams: 0 of 2 live · no recent transport samples to measure the rate · 0 A/V pairs dropped`. |
| `studio-console-layout-restored.png` | Studio restarted on a `studio.conf` saved after dragging the side panel edge to 480 px and hiding the mixer from View: the restored layout (input bank across the full width, 480 px side panel) with View open (`Colour grade…  Ctrl+G`, `Animate title…  Ctrl+Shift+A`, `Design scene layout…  Ctrl+Shift+L`, the Panels checkboxes, the Advanced controls toggles, the five-way Side panel tab choice including `Media`, Language, 12-hour clock), and the enabled `Reset layout` item with its tooltip. |
| `studio-default-layout.png` | 1920x1080, the Default layout as a fresh `studio.conf` opens it on the demo show (recaptured at Protocol 2.48, schema 45): Preview, Transitions, and Program along the top with Recording & streams beside them, the input bank, audio mixer, and the Overlays/Stingers/Titles/Playlist/Media tab group along the bottom. Both rows' tab bars line up across the console and each starts with its drag grip; Preview and Program are the same width with their 16:9 wells letterboxed in the middle of their tiles; the transition column fits its tile with `MORE`, the `FAVOURITES` row, the collapsed `▸ DETAILS` manual block, and the Fade to Black duration and `Outputs 1/1` keys; the mixer's input strips start at the left beside the pinned `MON` strip (`= MASTER`, `SIP` lit, `METER ONLY`) and Master. |
| `studio-docking.png` | The same session while the side tab group's grip is dragged onto the left edge of the input bank: the blue drop preview covers the bank tile's left half, where releasing would split it, and the chip beside the pointer names every panel moving together (`Overlays · Stingers · Titles · Playlist · Media`). |
| `studio-popout.png` | The Program panel popped out into its own native window (`Program — FreeMix Studio`, moved to 1180,520 and resized to 700x460; no window manager, so no decorations) with the grip, `Dock back to…`, and Dock back key in its header. Its grip is held over the console window: the main window shows the drop preview on the right half of the input bank tile with a `Program` chip, and releasing docked it there. The transition column is centred at its widest (360 points) in the tile Program left. |
| `studio-popout-dock-menu.png` | The same popped Program window with `Dock back to…` open: every docked tile (Preview, Transitions, Input bank, Audio mixer, Recording & streams, and the side tab group) with Into its tab group / To its left / To its right / Above it / Below it. Hovering Preview › To its right previews that place in the console and shows the item's hover help; clicking it docked Program beside Preview. |
| `studio-console-input-menu.png` | Operator right-click menu on the Host Laptop tile: key cap and name header, Select on Preview / Cut to Program enabled, and `Grade…`, Duplicate, Replace source…, Category, Colour label, Pause/Close input, Rename, Move, and Remove disabled, with the footer note that the operator session lacks `edit_project`. |
| `studio-console-add-input.png` | 1920x1080 admin session after clicking the `+` tile: the Add input dialog with `SOLID COLOUR` selected, the suggested name "Solid Colour 1", the amber preset chosen (`#f0a828`), and the preview swatch tagged `INPUT 8` / `SILENT`. |
| `studio-console-add-input-bars.png` | The same dialog for a second add with `COLOUR BARS`: the suggestion "Colour Bars 1", the bars preview tagged `INPUT 9`, no colour row. |
| `studio-console-input-added.png` | After both adds, selecting Solid Colour 1 on Preview and scrolling the bank: tiles 8 (Solid Colour 1, `PVW`) and 9 (Colour Bars 1), the `+` tile with `9 / 256`, `9 SOURCES`, and a mixer strip for the new solid input. |
| `studio-console-remove-input.png` | Admin session after both adds: the Remove input confirmation for Colour Bars 1 (input 9, position 9), the question, the warning that removal clears redo history and cannot be undone, and the red `REMOVE` key (tooltip hint `[Enter]`, Cancel `[Esc]`). |
| `studio-console-input-refused.png` | After removing Colour Bars 1 and then asking to remove Camera 1 Wide from Inputs › Remove input: the single refusal toast `Remove refused (conflict): "Camera 1 Wide" is a layer source of scene 100. Remove that reference offline first.` and the bank back at `8 / 256`. |
| `studio-console-inputs-menu.png` | Inputs menu with the Catalogue group: `Add input…` and the `Remove input` submenu, where Main Scene (`PGM`) and Solid Colour 1 (`PVW`) are disabled with their tally marks. |
| `studio-console-add-input-disabled.png` | Operator session hovering the dimmed `+` tile: the tooltip "Add input… A solid colour or colour bars, appended to the end of the bank. Unavailable: this session lacks the edit_project permission". |
| `studio-console-reconnecting.png` | 1600x940 Studio started with `--connect` to an address with no daemon: the Reconnecting card with attempt count, retry delay, the last error in operator language (`The daemon refused the connection: it may not be running yet`), and the disabled-controls footer over the dimmed monitors; `NO STATE` chips, `— TARGETS`, `— SOURCES`, centered empty states for the input bank and mixer, and "No synchronized show" in the top bar (no edit group without synchronized state); the health strip's unlit `NO PGM FEED` and `NO METERS` lamps. |
| `studio-console-spanish.png` | 1920x1080 demo show with Studio started with `--lang es`, REC 2 recording, and O1 taken and opened: the `PREVIO`/`PROGRAMA` tally chips, `MÁS`, `FAVORITAS` (`FUNDIDO`, `CORTINA`, `VUELO`, `CUBO`), `Salidas 1/1`, the lit red `DETENER` key of REC 2, neutral `INICIAR` keys, the overlay keys `TOMAR` / `RETIRAR` beside the `APAGADO` / `AL AIRE` chips, the open O1 card's Spanish diagnostic line, the side tabs `TÍTULOS`, `LISTA`, `MEDIOS`, the bank toolbar (`Todas`, `Importar archivos…`, `Categorías…`), and the Spanish mixer (`PANO C`, `RMS · RETENCIÓN · CLIP`, `BORRAR CLIP`). |
| `studio-console-spanish-menu.png` | The same session with `Salida` open: every transition, Fade to black with `Salidas del fundido a negro  Salidas 1/1 ›`, T-bar, recorder (`Detener grabador 2` / `GRABANDO`), stream, and Stinger item worded in Spanish. |
| `studio-console-spanish-input-refused.png` | 1920x1080 `--lang es --role admin` after adding "Color sólido 1" and asking to remove Camera 1 Wide from Entradas › Quitar entrada: the localized refusal toast `Baja rechazada (conflict): «Camera 1 Wide» está en uso: a layer source of scene 100. Quite esa referencia sin conexión primero.` (the daemon's own reference detail is quoted as sent). |
| `studio-console-overlay-details.png` | 1920x1080 with O1 on air and its card open: `TAKE` / `TAKE OFF`, the UPDATE/queue/NEXT keys, the overlay transition key (`CUT`), the position and border keys, OUTPUTS, and the localized diagnostic line naming the source instead of its raw id. |
| `studio-console-spanish-transition.png` | 1920x1080 `--lang es` (REC 2 recording, O1 on air) after manual `FUNDIDO` and a T-bar drag: `T-BAR 62 %`, `POS` with the basis-point field, `COMPLETAR`/`CANCELAR`, and the T-bar's Spanish hover help (`T-bar manual: arrastre la palanca (abajo = inicio, arriba = fin) y luego COMPLETAR o CANCELAR  [Esc]`). |
| `studio-console-spanish-stingers.png` | 1920x1080 `--lang es` (REC 2 recording, O1 on air) with Fade to Black on (`FTB ACTIVO`, `PROGRAMA EN NEGRO`, `FTB` badge) and the Stingers tab: the slot cards, the Configure card (`ASIGNAR`/`VACIAR`, cut point in `fr`, preload, audio, fallback), and the S4 card tooltip. |
| `studio-console-spanish-titles.png` | 1920x1080 `--lang es --role admin`, Titles tab after a second Studio (`--connect`) sent `UPDATE LIVE` with Name "M. Ruiz" while this console held the draft "Marta Ruiz Torres": the `CONFLICTO` row badge and banner, the `MÍO`/`SUYO` comparison for Name, `REBASAR` and `DESCARTAR TODO`, the server's "M. Ruiz" on the Program frame, and the disabled `ACTUALIZAR EN VIVO` tooltip naming the server-side change. |
| `studio-console-spanish-playlist.png` | 1920x1080 `--lang es --role admin`, the Lista tab after `remote-playlist … pause` and a Studio restart: `EN PAUSA`, the amber `RESTANTE` countdown held at `-00:00:37:04`, `REANUDAR` lit amber in place of Pausa, `AL AIRE`/`SIGUIENTE`/`OMITIDO` row badges, and the Spanish editor and unavailable reason. |
| `studio-console-spanish-compact.png` | 1280x800 `--lang es` (REC 2 recording, O1 on air, Fade to Black on) with the Stingers tab: long Spanish labels and details ellipsize inside fixed-width keys, chips, and tabs (`OVER…`, `STING…`, `TÍTUL…`) instead of overflowing; tooltips carry the full text. |
| `studio-console-spanish-add-input.png` | 1920x1080 `--lang es --role admin`: the Añadir entrada dialog with `COLOR SÓLIDO`, the suggested name "Color sólido 1", the amber preset, and the preview tagged `ENTRADA 8` / `SIN AUDIO`. |
| `studio-console-spanish-remove-input.png` | The Quitar entrada confirmation for "Color sólido 1" (entrada 8, posición 8) with the Spanish warning and the red `QUITAR` key. |
| `studio-console-spanish-reconnecting.png` | 1600x940 `--lang es --connect` with no daemon: the Spanish Reconectando card (attempt, retry delay, the localized last error `El daemon rechazó la conexión: puede que aún no esté en marcha`, disabled-controls footer), the `TRANSMISIONES` heading, and dimmed `TOMAR` keys. |
| `studio-console-preferences.png` | 1920x1080, File › Preferences… (or Ctrl+,): Language (each language named in itself), Clock (`24-hour` selected), Display with the UI scale keys (`90%` to `150%`, `100%` selected), the High-contrast theme checkbox, the note that the choices live in `studio.conf` and that `--lang`/`--clock` win for their session, and `Reset to defaults` and `Done`. |
| `studio-console-high-contrast.png` | 1920x1080 with `ui.high_contrast = on` in `studio.conf` (REC 2 recording, O1 on air): pure black panels, cards, keys, and fields told apart by 2 px light-grey outlines, near-white text and muted text, captions, field labels, chips, and tab labels 1.5 pt larger, tile ordinals and `no thumbnail` in the palette's muted text instead of near-black grey, while the tally and status hues (red Program, `RECORDING`, `ON AIR`, green Preview, amber `NOT LIVE`) are unchanged. |
| `studio-console-spanish-high-contrast.png` | The same high-contrast session with `--lang es`: the longer Spanish keys (`FUNDIDO`, `CORTINA`, `DETENER`, `RETIRAR`) fit their outlined keys at the larger label size. |
| `studio-console-spanish-hover-help.png` | `--lang es`, REC 2 recording and O1 taken (`TOMAR`), hovering `FUNDIDO`: the Spanish help line with its shortcut (`Fundir Previo a Programa con la duración compartida  [F]`); the `▸ DETALLES` and `▸ GAN · BAL · RET` disclosure keys stand in for the hidden advanced controls. |
| `studio-media-transport.png` | 1920x1080 `--role admin`, the MEDIA side tab (IN-003, Protocol 2.37) on a show with two media-file inputs (`media-input-add … asset://media/intro.mp4` and `…/sponsor-loop.mov`; the simulated daemon opens each on a synthetic 10 s source): Intro Clip `PLAYING` at `00:00:05:29 / 00:00:09:59` and Sponsor Loop `PAUSED` at `00:00:05:33` after `remote-input-media-seek … 4s`, play and pause; each card has its scrubber, `PLAY`/`PAUSE`/`STOP` keys, trim `In`/`Out` fields, the speed combo (`1×`), the end action (`Hold last frame` / `Loop`), and the audio track (`Default track`). The bank behind shows the IN-026 tile state of the same show (category chips, colour strips, `PAUSED` on Host Laptop, `CLOSED` on Playback Clip). |
| `studio-favourites.png` | 1920x1080 `--role admin` (SW-002/SW-005, Protocol 2.36) with favourites set offline to Fade, Cube Zoom (20 fr), Cross Zoom (45 fr), and Stinger 1 (`transition-favorite-set`; S1 holds a media input): the `FAVOURITES` row (`FADE`, `CUBE ZOOM`, `X-ZOOM`, `STING 1`, F5-F8) and the fourth key's right-click `Assign…` submenu listing Cut, every mix kind, and `STING 1`. The long labels widen the transition column, so the Program well is pushed under the side panel (its header reads `Main Sce`, the `SIMULATED` badge is cut) and the mixer header's `GAIN · BAL · DLY` key is clipped; see the review notes below. |
| `studio-input-lifecycle.png` | 1920x1080 `--role admin` (IN-026, Protocol 2.38) on a show with categories Cameras, Remote, Playback, Graphics and a colour label on every input (`input-categories`, `input-category`, `input-color`), Host Laptop paused and Playback Clip closed (`input-pause`, `input-close`): the bank toolbar's filter chips (`All inputs`, one per category, `Uncategorised`), `Import files…` and `Categories…`; each tile's colour strip down its leading edge; the amber `PAUSED` and grey `CLOSED` badges; and Guest Remote's tile menu with the Category and Colour label submenus (`No colour`, Red … Pink, Orange highlighted), Duplicate, Replace source…, Pause input, Close input. |
| `studio-mixer-groups.png` | 1920x1080 `--role admin` (AU-001, SW-004, Protocol 2.40) on a show with three outputs (Program Output, Venue Screens, Confidence Monitor excluded with `ftb-output … 3 off`), Camera 1/2 panned L60/R60 and in group 1 "Cameras" at -6 dB, Host Laptop and Guest Remote labelled `Host Mic` / `Guest Line` (Guest Line panned R30) and in group 2 "Talkback" (muted): the mixer scrolled to show the `PAN R60`/`PAN R30` knobs, the labels on the name plates, the two VCA group strips (`Cameras`, `Talkback` with its red muted cap) between the input strips and MASTER, and the Fade to Black selector open (`Outputs 2/3` with Program Output and Venue Screens ticked, Confidence Monitor unticked). |
| `color-panel.png` | The standalone colour grading panel (SW-019), rendered offscreen from the real widget by `cargo test -p fm-ui-egui write_color_panel_screenshot -- --ignored` (CPU rasterizer, no window). Top, Simple mode with the Warm look plus contrast: live look thumbnails of the preview picture, the gradient sliders, auto balance, the graded preview, and the RGB parade. Bottom, Advanced mode: the basic sliders with pivot, Lift/Gamma/Gain/Offset wheels with masters and reset keys, the master curve, the LUT slot (the LUT asset is not loaded, so previews say so), the wipe compare with its handle, and the vectorscope with 75% bar targets. |
| `animation-curve-editor.png` | The keyframe timeline in Advanced mode with a custom easing curve (GX-011), rendered offscreen from the real widget by `cargo test -p fm-ui-egui write_animation_curve_editor_screenshot -- --ignored` (CPU rasterizer, no window; re-rendered 2026-10-16, now with the `MY PRESETS` row: preset name, Save as preset…, the preset picker, Rename, Delete, and the `.fmpreset` Export/Import). The lower third's first Position X key is selected and shaped by an overshooting cubic bezier (0.30, 1.45, 0.50, 1.00): the easing picker reads `Custom curve`; the inspector's curve editor shows the unit square with the two draggable handles, the live preview dot, the `Start from` shapes, and the four numbers as fields. Below, the value graph shows the selected segment's handles in time and value. |
| `studio-color-copy-to.png` | 1920x1080 `--role admin` (SW-019, Protocol 2.56), Guest Remote on Program, its tile menu `Grade…`, the Warm look clicked, then the Temperature slider dragged from +25 to +93 (seven live `grade_input_set` sends during the drag, REV 8 to 15, folded by the server into one undo step: the top-bar Undo reads `2 steps in the shared history`, and one Undo returned the slider to +25). The new `Copy to…` key in the target row is open: `COPY THIS GRADE TO`, the `Every input` / `Untick all` keys, one checkbox per other target (Program output, every input, each scene layer `Main Scene › Wide Shot`), Camera 1 Wide, Camera 2 Close, and the Wide Shot layer ticked, and the hovered `Copy to 3 targets` key with its help line. |
| `studio-color-copied.png` | The same session after `Copy to 3 targets`: three standalone grade sets (REV 15 to 18), the `NOTICE` toast `Grade copied to 3 targets (one undo step each)`, the Preview monitor's Camera 2 Close (purple) now showing the server-applied warm grade, and the top-bar Undo hover `5 steps in the shared history` (Warm, the folded drag, and one per copied target). |
| `studio-color.png` | 1920x1080 `--role admin` on the demo show, Guest Remote (colour bars) cut to Program, its tile menu `Grade…` (also View › Colour grade… or Ctrl+G) opening the Studio Colour window bound to input 4, then the Warm look clicked: the grade went to freemixd as a `grade_input_set` command (REV 10 to 11), the target now reads `04 · Guest Remote · graded` with the `ON AIR` status, the Program monitor shows the server-applied warm grade, and the panel preview (held last ungraded frame, so the grade is not applied twice) shows the draft applied locally on CPU, with the Waveform scope below. Above the panel: the Library combo/Apply, Save to library, LUT import path with its 32 KiB limit caption, and the `.fmgrade` Export/Import row. |
| `studio-animation.png` | 1920x1080 `--role admin` (GX-011), the demo show plus title input 8 "Lower Third" imported from a four-element `lower-third.fmtitle` (Bar, Accent, Name, Role) whose In phase was built with `freemix-cli title-animation-preset` (slide-from-left, wipe, fade on Bar, Accent, Name; Role left unanimated) and `title-animation-stretch` to 30 frames: Titles tab, the row's `Animate…` opening the Studio Animation window at its default size. The ruler was clicked near frame 10, then Role's `Preset…` picker opened and the pointer rested on `Slide from bottom`: the canvas shows `PRESET PREVIEW`, the server's exact preview of the hovered preset at mid-phase (frame 15), the element outlines and onion skins drawn over it, and the item's hover help. Nothing is applied until the item is clicked, which sends one `title_animation_preset` (one undo step). |
| `studio-animation-stagger-menu.png` | 1920x1080 `--role admin` (GX-011), captured 2026-10-19 (re-captured for the stagger preview): the animation capture's `lower-third.fmtitle` with all four elements animating In together over 15 frames (`title-animation-preset … 1 slide-from-left`, `2 wipe`, `3 fade`, `4 slide-from-bottom`, each `--frames 15`), Titles tab, `Animate…`, the window enlarged by its corner, `Fit` and two Ctrl+wheel steps out, then the header's `Stagger…` menu opened: `Frames apart` (5 fr), the new `Spread to fill phase` toggle (off), `Top first`/`Bottom first`, the order the elements will start in (Bar · Accent · Name · Role), and the pointer resting on `Apply stagger` with its hover help. While it rests there the rows ghost the result in amber: Accent, Name, and Role's bars 5, 10, and 15 frames later, and a dashed line at frame 30 where IN would grow to. The header also shows the `Snapping` toggle (on; it is saved in `studio.conf` as `animation.snapping`), and the transport the previous/next key keys (Ctrl+Left/Right; previous is disabled at frame 0). |
| `studio-animation-staggered.png` | The same session after `Apply stagger` (one undo step, auto-saved as one `title_animation_set`: the console revision went from 4 to 5 and the badge reads `SAVED`) and `Fit`: the four bars cascade 5 frames apart and IN grew from 15 to 30 frames to hold Role. The ruler was clicked at frame 20; the canvas shows the server's exact preview there (Bar and Accent in, Name arriving, Role still sliding up) and the pointer rests on the next-key key, whose help names `[Ctrl+Right arrow]`. |
| `studio-scene-designer.png` | 1920x1080 `--role admin`, live (2026-10-16): the Layer designer opened with Ctrl+Shift+L (also View › Design scene layout…, Edit › Preview input › Design layout…, or Design… on a Scene tile's menu) on the demo show's Main Scene, which is on Program. Guest PiP (left crop 240 px) was selected in the layer list and dragged on the canvas; the drag streamed Protocol 2.39 `scene_layer_*` commands to freemixd (the chip read `SENDING EDITS` during the drag and returns to green `LIVE EDITS`), the canvas behind the outlines is the server's own picture of the scene (`SERVER PICTURE` badge, top right), and the Program tile shows the moved PiP. Undo is enabled and is the engine's. Left, the layer list with Forward/Backward/To front/To back; right, the numeric inspector (position 1163, 658 px, size 480x270, zoom 25 %, rotation with quarter-turn keys, opacity, visible, crop L 240 px, mask, stack `2 of 2`, z-order, anchor 50 %/50 %). No offline banner: the session holds `edit_project` and a Scene input shows the scene. |
| `studio-network-stream-form.png` | 1920x1080 `--role admin` against a native daemon (IN-015, Protocol 2.46): the `+` tile's Add input dialog with `NETWORK STREAM` chosen, the name "Stage Left RTSP" (`Name is free`), the URL `rtsp://operator:s3cret@10.0.0.42:554/stream1` (the form shows what the operator typed; the daemon redacts the credentials everywhere it reports the URL, see the next row), `TCP` transport, the jitter, retry, give-up, and timeout fields, `HOLD LAST FRAME`, and the sample tagged `INPUT 12` / `CONNECTING` / `SILENT`. The longest retry wait was set to 400 ms, below the 500 ms first retry, so the red validation line `the first retry wait must not exceed the longest one` shows and `ADD INPUT` is unavailable with the same reason in the footer. |
| `studio-network-stream-tile.png` | The native session's input bank: Field Feed (input 8, a loopback `ffmpeg` MPEG-TS sender on `udp://127.0.0.1:7591`) with a green `LIVE` badge, Venue Camera RTSP (input 9, `rtsp://operator:s3cret@127.0.0.1:7592/live`, nothing listening) with an amber `RECONNECTING 35` badge, and the two capture tiles `LIVE`; hovering Field Feed shows the tile tooltip `Network stream udp://127.0.0.1:7591: LIVE · 2,136 kb/s, 400 ms buffered · 3,866 dropped, 0 late frames, 0 reconnects` (the dropped count reflects the overloaded software-rendering host). Hovering the RTSP tile instead reads `rtsp://***@127.0.0.1:7592/live: RECONNECTING 42 · … · Next attempt in 1 s · Last error: Error opening input files: Connection refused`. Program shows the native daemon's CPU readback of Main Scene; Preview reads `NO PREVIEW TAP`. |
| `studio-capture-picker.png` | The same dialog with `CAMERA / CAPTURE` chosen (IN-001, Protocol 2.47): `2 capture devices found (fake)` from the daemon's `--diagnostic-capture-inventory` fake backend (Fake Cam `/dev/video0`, Spare Cam `/dev/video2`), Fake Cam selected, the format picker with Picture size / Rate / Pixel format filters (`Any`), `Automatic (nearest the program)`, the formats grouped `UHD` / `HD` (`1920x1080 30p MJPEG` selected), the `NONE` / `PAIRED DEVICE AUDIO` / `ALSA DEVICE` audio keys (`Paired audio: none paired`), `HOLD LAST PICTURE` / `BLACK`, and the sample tagged `STARTING`. |
| `studio-capture-signal-lost.png` | The native session after the fake inventory file was rewritten without Spare Cam (an unplug): input 11's tile carries the red `SIGNAL LOST` badge across its picture and a red border, and its tooltip reads `Capture v4l2:usb-0000:00:14.0-2#Spare Cam: SIGNAL LOST (device unplugged) · 1280x720 60p MJPEG, audio off · 1 losses, 0 recoveries, 0 renegotiations · Last error: device not present`. Desk Cam stays `LIVE` and the RTSP input keeps `RECONNECTING n`. |
| `studio-ftb-outputs.png` | 1920x1080 `--role admin`, simulated daemon (SW-004, Protocol 2.48) on a show with three outputs (`ftb-output-duration … 1 45`, `… 2 90`, `ftb-output … 3 off`, `ftb-curve … ease`): the Fade to Black `Outputs 2/3` popover with Program Output and Venue Screens ticked, each with `Own duration` ticked and its frames (`45 fr`, `90 fr`), Confidence Monitor unticked, and the `CURVE` keys `Linear` / `Ease` (Ease lit). |
| `studio-tbar-cube.png` | The native session after selecting Field Feed on Preview, `MANUAL` › `MORE` › `Cube zoom`, dragging the T-bar to 53 %, and opening `▾ DETAILS`: the amber `T-BAR 53%` header, `POS 5379 bp`, `COMMIT`/`CANCEL`, the status card `CUBE ZOOM 53%` / `Main Scene ▶ Field Feed`, and the Program monitor showing the native daemon's mid-transition frame (Main Scene and the Field Feed test pattern on two faces of the zooming cube, `ENCODED PREVIEW 640×360 · CPU readback`); the Main Scene tile borrows the same frame. |
| `studio-layer-keys.png` | 1920x1080 `--role admin`, simulated daemon (IN-020, Protocol 2.43): the Layer designer (Ctrl+Shift+L, window enlarged) on Main Scene with Guest PiP selected after `scene-layer-key … 100 1 chroma 0 177 64 90 24 128` and `scene-layer-effect-add` `saturation 40` and `sharpen 300`. The canvas is the server picture with the bars' green column keyed out; the inspector shows `KEY` (`Chroma`, the key swatch and its `Green`/`Blue` presets, R/G/B 0/177/64, similarity 90, smoothness 24, spill 128) and `EFFECTS` (`1. Saturation` amount 40, `2. Sharpen` amount 30.0 %, each with its reorder and remove keys, and `Add effect`). |
| `studio-mixer-monitor.png` | The native session's audio mixer maximised (double-click its tab) with `▾ GAIN · BAL · DLY` open (AU-001, Protocol 2.44): solo mode `PFL` lit on the `MON` strip, Field Feed (the only input with audio, a 440 Hz tone over UDP) soloed at -3.0 dB with its lit amber `S`, so `MON` reads `SOLO LISTEN` and meters the pre-fader solo at -21.0 while Master reads -24.0; the group strips Cameras (-6 dB, holding Camera 1 and 2), Talkback (Host Mic and Guest Line), and the parent group All Mics, with Talkback's parent picker reading `In All Mics` and the others `Top level`. These are daemon meters (no synthetic feed). |
| `studio-spanish-refusal.png` | 1920x1080 `--lang es --role admin`, simulated daemon: after right-clicking Camera 1 Wide, `Quitar…`, and `QUITAR`, the typed refusal toast `ERROR · Baja rechazada (input_in_use): «Camera 1 Wide» es una fuente de capa de la escena 100. Quite esa capa de la escena primero.`, now entirely in Spanish (the earlier capture quoted the daemon's English detail). |
| `studio-fullscreen-outputs.png` | 1920x1080 `--role admin` against a native daemon started with `--diagnostic-display-inventory` listing two displays (DELL U2720Q, HDR, three modes; Venue LED Wall 1920x1080@50): Output › `Fullscreen outputs…` (OR-011, Protocol 2.42) with `2 displays connected` and `Refresh displays`; Output 1 `STANDBY`, `Waiting: the display is not connected`, targeting `name:Projector (not connected)` with Program bus / Current mode / Fit inside / Off (SDR); Output 2 `ACTIVE`, `On Venue LED Wall at 1920×1080 @ 50 Hz; SDR`, showing Program Output with Fill; each slot with `Save and apply`, `Discard changes`, and `Clear`. |
| `studio-workspace-docked.png` | 1920x1080 `--role admin`, round 6 (Protocol 2.62): the full docked Default workspace on the "Evening Broadcast" show mid-run. The Evening Run cue list has played Pre-show standby and Show open (Camera 1 Wide faded to Program, Main Scene on Preview, REC 2 `RECORDING`, the top bar's `REC` and amber `LIVE`), so the Cues tab shows Show open `LIVE` and Host intro `STANDBY`; Recording & streams lists all five destinations at once, with YouTube Main, Twitch Backup, and Venue SRT Feed started (amber `NOT LIVE`, `Started · no stats`, amber-outlined `STOP`, `RETRY` lit, `EDIT…` and `✕` disabled) and Facebook Live and Local HLS Archive `OFF`. The input bank has live thumbnails (`FEED · SIM` pictures, the `PGM TAP · SIM` / `PVW TAP · SIM` tap frames, Host Laptop's synthetic stream card) and Desktop Audio's `audio only` meter placeholder; the `+` tile reads `colour · bars · stream · camera · audio`; the mixer's `MON` strip shows the headphone device as `hw:CARD=H…` (ellipsized, full name on hover), `PHONES IDLE`, and `-6.0 dB`. Replaces the Protocol 2.59 capture (recaptured after the round 6 fixes). |
| `studio-cues.png` | The Cues panel popped out (900x720 window capture) on the same session (RC-017): the list picker with `New list` / `Delete list`, the full-width `GO` key, `Standby: Host intro`, `Step back` / `Skip` / `Stop` / `Reset`, the editable list name, and eight cue rows: Pre-show standby `PLAYED`, Show open `LIVE` (red border) opened with its three actions (Take Preview with Fade 30 frames, Stream YouTube Main On, Stream Twitch Backup On) and `Add action`, Host intro `STANDBY` (green border), Guest interview with `Follow` ticked, and per-row standby (`●`), go-this-cue (`▶`), move, remove, and action-count keys. |
| `studio-cues-script.png` | Round 6: the Cues panel popped out (900x640 crop of the window, RC-017/RC-011, Protocol 2.60) after `GO` on Host intro and Guest interview: Guest interview `LIVE` and opened, its first action a `Script` step naming the project script `Guest in` (`run_script.2`), then `Take` `(Preview)` `Wipe` `20 frames`; Guest off `STANDBY`, the earlier cues `PLAYED`. |
| `studio-cue-preview.png` | The Cues panel maximised (760x1000 offscreen render of the hover-audit show, RC-017, no protocol change; `FM_CUE_RENDER_DIR=<dir> cargo test -p fm-ui-egui --lib render_the_cue_preview_for_review -- --ignored`): the 760-point top bar sheds detail instead of overlapping (the STUDIO word goes and the padlock is its glyph, so the show name, REV, Undo and Redo, REC, LIVE, and the UTC clock all fit). The armed note reads `Armed: "Guest" fires in 0:32, at <time> UTC`; it never counts below zero (due: `is due now; waiting for the server to fire it`, more than a second late: an amber `is 0:04 overdue`), and the engine frame is in its hover. Under the GO key, the `GO will:` box previews the standby cue Guest and the cue it follows into: `Cut to Guest (now Camera 1)`, `Then cue "Close" follows:`, `Restart Guest from its first frame`, `Wait 1.5 s, then:`, the seek, play, and pause, and `Run script "Opening"`, with `Preview on PVW` (which only selects Guest on Preview) and the hint to hover a state chip. The fault note names the cue (`Cue "Close" stopped at action 1. The server said: ...`), the list name field is labelled, the armed cue's row is outlined amber like its ARMED chip, and each row has painted standby, fire, move, and delete keys and a `16 actions` disclosure key (one width on every row, so the keys line up). |
| `studio-cue-preview-hover.png` | The same render with the pointer on the Opener's `LIVE` chip: its tooltip leads with `Firing "Opener" will:` with every step resolved against the show (`Put Camera 2 on Preview (now Camera 2)` marked `already on Preview: no change`, `Fade to Camera 2 over 25 frames (now Camera 1)`, overlay, title text and number steps with their current values, fader `-6.0 dB (now +0.0 dB)`, `Start recording on REC 1` marked `no change: it is already so`), `Start the playlist (it takes Program)` marked red `Would be refused: the transition started earlier is still running then`, the trailing `Wait 50 frames (2.0 s), then:` closed by `The cue ends`, the summary `As the show stands, the server would refuse this GO and nothing would run.`, and the state help as a caption under it. |
| `studio-scripts-failed.png` | Round 6: Edit › Scripts… (RC-011, Protocol 2.62), cropped to the dialog: Break bumper (`transition`, `wait 50`, `cut "Sponsor Loop"`) ran its first part, waited 50 frames, and its second part was refused (the show has no Sponsor Loop input): line 4 is marked red in the editor, the red status reads `The run failed on frame 105 with a refused action at line 4, column 1; nothing of that frame was applied.` beside `Clear failed run`, and `Last run: 1 action in 5 instructions, on frame 55`. Captured at Protocol 2.62, when a part after a wait was reported from a replicated fault that carried no reason; since Protocol 2.63 the fault carries the refusal's reason and message, so the status also ends `The server said: …` (as a run refused on its first frame always did) and `Clear failed run` sits on its own row below it. Recaptured after the round 6 fixes: the window keeps its size. |
| `studio-scripts-cue-run.png` | Round 6: the same window on Guest in after cue 4 of Evening Run ran it: `Last run by cue list Evening Run: 3 actions in 8 instructions, on frame 111` (Protocol 2.61); `Delete script` is disabled because a cue runs the script. Recaptured after the round 6 fixes. |
| `studio-scripts-waiting.png` | Round 6: Camera rotation (`let`, `repeat 4`, `if program == 1` … `else` … `end`, `wait 1500`) run from the CLI and paused at its first wait: the list reads `Camera rotation (running)`, the blue status `Running: the next part runs on frame 1636 (wait 1 of this run; its waits total 1,500 frames so far, this one included)`, `Stop run`, and `Last run: 1 action in 18 instructions, on frame 136`; `Save script`, `Run script`, and `Delete script` are disabled. Recaptured after the round 6 fixes. |
| `studio-livelan-unavailable.png` | Round 6: Output › LiveLAN viewers… (OR-017, Protocol 2.57), cropped, on the simulated daemon with LiveLAN enabled in the project (`Share on the local network` ticked, `Viewers see` Program, `Viewer limit` 8, no PIN): the amber `UNAVAILABLE` chip hovered: its tooltip is the one-line `The LiveLAN stream's state as the server reports it`, while the paragraph under the chip explains that the server has no LiveLAN listener and how to start one (`freemixd serve <show.freemix> --native-media --live-lan-listen 0.0.0.0:8090`); `Viewer address` and `No address yet` are spaced apart. Recaptured after the round 6 fixes. |
| `studio-mixer-phones.png` | Round 6: the docked audio mixer (580x475 crop, AU-001 headphone output, Protocol 2.53) with the `MON` strip's headphone controls: device `hw:CARD=Headset,DEV=0` (set offline) shown as a key reading `hw:CARD=H…` (click to edit; full name on hover), `PHONES IDLE` (the simulated daemon renders no audio) hovered, its tooltip beside the state to the left instead of over the level and mute keys, headphone level `-6.0 dB`, and the headphone mute `M` lit red. Synthetic meter feed, so `MON` reads `--`. Recaptured after the round 6 fixes. |
| `studio-mixer-maximised-before.png` | Before (a build of 341da97a): the audio mixer maximised (double-click its tab) at 1920x1080 on the demo show with the synthetic meter feed: seven 82 px strips on the left, faders stopped at 620 px, the pinned `MON` and `MASTER` strips at the far right, and most of the panel empty between them and below the strips. |
| `studio-mixer-maximised.png` | After: the same maximised mixer. The strips share the panel up to 150 px wide, so names fit on one line, the meters have wider bars and the faders a larger cap; meters and faders take the panel height (806 px here) and switch to the fine scales (fader `+20 +10 +5 0 5 10 15 20 30 40 50 60`, Master `0 -3 -6 -9 -12 -15 -18 -21 -24 -30 -36 -48 -60`); `MON` and `MASTER` stay aligned at the right and nothing scrolls. Docked, the same mixer keeps compact 82 px strips and scrolls them horizontally. |
| `studio-output-menu-shortcuts.png` | Output menu (900x820 crop) hovering the new `Start all streams` item: its hover help ends `[Ctrl+Shift+O]`; `Stop all streams` (`Ctrl+Shift+K`, disabled: none running) and `LiveLAN viewers…` (`Ctrl+Shift+N`) show their keys in the shortcut column, as Edit › `Scripts…` shows `Ctrl+Shift+S`. |
| `studio-add-input-system-audio.png` | Round 6: the `+` tile's Add input dialog (AU-011) (Protocol 2.55) with `SYSTEM AUDIO` chosen: suggested name "System Audio 1", Audio system `PulseAudio / PipeWire` (or `ALSA`), Source device `@DEFAULT_MONITOR@` with its hint, and the `audio only` / `SILENT` sample tagged `INPUT 8`; the intro now names system audio. Behind the dialog, Desktop Audio (input 6, a PulseAudio system audio input) shows its `audio only` meter placeholder tile and the `+` tile reads `colour · bars · stream · camera · audio`. Recaptured after the round 6 fixes. |
| `studio-streams-five.png` | The Recording & streams panel popped out (820x610 window capture, OR-006) with five destinations, each one compact row with its `RETRY` / `EDIT…` / remove (`✕`) keys inside the row left of `START`/`STOP`: YouTube Main (RTMPS with backup), Twitch Backup (RTMP), and Venue SRT Feed (SRT) started and amber `NOT LIVE` with amber-outlined `STOP` keys and enabled `RETRY`; Facebook Live (RTMPS) and Local HLS Archive `OFF` with `START`, `EDIT…`, and remove enabled. `EDIT…` and remove are disabled on running destinations (remove also while a cue names the destination); `ADD…` is disabled at five destinations. In a narrow dock the three keys fold into one `···` menu key (`RETRY`, `EDIT…`, `Remove destination…`). Recaptured after the layout fixes (no cue lists in that show). |
| `studio-stream-status-hover.png` | The same window hovering YouTube Main's status chip: the status tooltip with `Desired on · realized not live`, `State: NOT LIVE`, the destination and backup URLs, `Output: Program Output · target 6,000 kb/s`, `Stream key: stored (never shown)`, and `No transport sample yet` (the simulated daemon sends no stream samples, so the bitrate / dropped / reconnects / uptime line is absent). |
| `studio-stream-edit-dialog.png` | 1920x1080, `EDIT…` on Facebook Live: the Edit destination dialog with the Destination section (Name, `RTMP` / `RTMPS` / `SRT` / `HLS` with RTMPS lit, Server `live-api-s.facebook.com:443/rtmp` and the resolved `rtmps://…` URL under it, the Backup server placeholder), a replacement stream key being typed and shown as dots beside the eye toggle, Output and video (Program Output, 4000 kb/s, `Start on launch`), Reconnect (250 ms / 2000 ms / 40 attempts, `Unlimited`), the undo note, and `CANCEL` / `SAVE`. |
| `studio-stream-remove.png` | Remove (`✕`) on Facebook Live: the Remove destination confirmation naming the destination and its URL (never the key), the note that recording and the other destinations are not affected, and `CANCEL` / the red `REMOVE`. |
| `studio-stream-add-dialog.png` | After the removal, `ADD…`: the Add destination dialog filled in as Facebook Live, RTMPS, `live-api-s.facebook.com:443/rtmp`, with a typed (fake) stream key masked as dots and the eye toggle's hover help `Show what is typed here; secrets are masked by default`; the suggested name was "Destination 5", the bitrate defaults to 6000 kb/s, and `ADD` is enabled. |
| `studio-add-input-solid.png` | 1920x1080 `--role admin`, the `+` tile (which now reads `colour · bars · stream · camera`): the sectioned Add input dialog with the four kinds, `SOLID COLOUR` chosen, the suggested name "Solid Colour 1", the `SAMPLE` swatch tagged `INPUT 8` / `SILENT` beside the colour presets, `input 8 · position 8`, and `CANCEL` / `ADD INPUT`. |
| `studio-add-input-network.png` | The same dialog with `NETWORK STREAM`: name "Stage Left Camera" (`Name is free`), the Stream source section with the URL `rtsp://operator:••••••••@10.0.0.42:554/stream1` (the password in the URL is masked as typed, with an eye toggle), `RTSP stream`, the Connection section with the RTSP transport keys, Recovery (`HOLD LAST FRAME` / `BLACK`, give up after attempts), each section with its `▸ ADVANCED` disclosure, and the `CONNECTING` sample. |
| `web-operator-phone.png` | Browser operator page (`/operator` on the `freemixd --web-listen` gateway, RC-014), headless Chromium at 390x844 as Operator with an English browser, refreshed as the fourth simultaneous Web client of the session below: the realized Program tile (Camera 2 Close) in red, the desired Preview (Remote Guest) in green, CUT/AUTO, the transition and duration pickers, Fade to Black `LIVE`, the `Español` toggle in the header, recorder 1 `REQUESTED: REC` and recorder 2 `REQUESTED: OFF` with the note that the daemon does not tell web pages whether a file is being written, YouTube `Stopped (start requested)` (the simulated daemon never realizes an RTMP push), and Slides `MUTED` (muted from the Audio page). |
| `web-operator-phone-es.png` | The same page at 390x844 as Operator in a Spanish (`es-ES`) browser, which it follows: `EN VIVO`, `Cambiar token`, `English`, `PROGRAMA`, `PREVIO`, `TRANSICIÓN` with `CORTE`/`AUTO`, `Fundido`, `Fundido alfa`, `Deslizar`, `Zoom`, `Cortina` (picked) and `0,5 s`...`3 s` (`1,5 s` picked), `Fundido a negro VIVO`, `GRABACIÓN Y TRANSMISIÓN` with `Grabador 1` `PEDIDO: REC` / `Detener`, `Grabador 2` `PEDIDO: APAGADO` / `Grabar`, the requested-state note, YouTube `Detenido (inicio pedido)`, Facebook `Detenido` / `Iniciar`, `AUDIO` with `Silenciar` keys and Slides `SILENCIADA`, and the status `En vivo — Evening News como operador · Iniciar YouTube: aceptado`. |
| `web-operator-audio-tablet.png` | A third simultaneous client: the operator page at 1180x820 with the Audio role (English browser), its own session beside the Operator page and the tally: Preview, CUT/AUTO, FTB, recorders, and streams disabled with `This session lacks the select_preview permission` / `This session lacks the transition permission`, the live mute keys (Slides `MUTED`, pressed here), and `Live — Evening News as audio · Mute Slides accepted`. |
| `web-tally-on-air.png` | The tally page (`/tally`, Viewer) at 390x844 connected at the same time as both operator pages, following Camera 2 Close: red `ON AIR` after the Spanish page's AUTO, with the `ES` language toggle beside `Inputs`. |
| `web-tally-es.png` | The same tally after tapping `ES`: `Entradas`, `EN`, `Pantalla completa`, `AL AIRE`, and `En vivo — Evening News como observador`; the choice is saved for both pages. |
| `web-operator-tablet-ftb.png` | The same page at 1180x820 (tablet landscape) after `FTB`: the badge reads `BLACK` once the realized position reaches black, and the key turns into `FROM BLACK`; Record and stream and Audio sit beside Transition. |
| `web-operator-audio-role.png` | 390x844 with the Audio role: Preview, CUT/AUTO, FTB, recorders and streams are disabled, each section naming why (`This session lacks the select_preview permission`, `This session lacks the transition permission`), while the mute keys stay live. |
| `web-operator-reconnecting.png` | 390x844 after the daemon is killed: the `RECONNECTING` pill, `Reconnect now`, empty buses (never stale Program), every control disabled with `Not connected to the show`, Fade to Black `UNKNOWN`, and the backoff countdown. |

Web operator captures (2026-10-19, Protocol 2.59): Playwright Chromium
(headless, device scale 2) against a simulated `freemixd serve --web-listen`
show with five simulated inputs, a scene input, one output, and two stopped
RTMP destinations (fake keys). The page was driven by clicks; only one Web
peer was admitted at a time then, so each capture is its own session.
`web-operator-tablet-ftb.png`, `web-operator-audio-role.png`, and
`web-operator-reconnecting.png` predate the language toggle and the
`REQUESTED:` recorder wording.

Several Web clients and Spanish (2026-10-19, Protocol 2.60, no protocol
change): `web-operator-phone.png` (refreshed), `web-operator-phone-es.png`,
`web-operator-audio-tablet.png`, `web-tally-on-air.png`, and
`web-tally-es.png` come from one Playwright Chromium run (headless, device
scale 2) against the same simulated show, with the Viewer tally, the
Spanish Operator page, the Audio tablet page, and finally the English
Operator page all connected at once, each its own session and role.

Readable output failures (2026-10-19, Protocol 2.67): `web-operator-recording-meters.png`
(refreshed) and `web-operator-output-failures-es.png` are Playwright Chromium
(headless, 1000x1300, device scale 1) captures of the operator page against a
native `freemixd serve --native-media --web-listen --record-program …` show
(640x360 at 30 fps, two solid inputs, a clip, a scene) in English and Spanish.
Recorder 1 records; recorder 2 reads `NOT CONFIGURED` / `SIN CONFIGURAR` with a
disabled Record key and the `--record-program-2` hint; the HLS destination is
live; and the RTMP destination to a closed local port reads `Reconnecting` with
`Server refused the connection (FFmpeg exited with code 145)` (Spanish:
`El servidor rechazó la conexión (FFmpeg terminó con código 145)`) instead of
the former Rust debug text. The stream key is a fake and never shown. The
meters read silence in this capture.

Round 6: captures of 2026-10-19 at Protocol 2.62, schema 56 (commit
b9ce83dd). Added: `studio-scripts-failed.png`, `studio-scripts-cue-run.png`,
and `studio-scripts-waiting.png` (RC-011 project scripts: a failed later
part with its marked line, the last run reported by a cue list, and a run
paused at a `wait` with `Stop run`), `studio-cues-script.png` (a cue's
Script action), `studio-livelan-unavailable.png` (OR-017),
`studio-mixer-phones.png` (the headphone device, level, and mute on the
`MON` strip), and `studio-add-input-system-audio.png` (AU-011);
`studio-workspace-docked.png` was recaptured on a realistic mid-show state
with five destinations and the cue list. All use the simulated daemon and
the synthetic meter feed; every stream key is a fake (`demo-…-not-real`).
The Scripts and LiveLAN captures are crops of the dialog, the Cues capture a
crop of the popped-out window (`import -window`). The Colour `Copy to…`
captures (Protocol 2.56) were already current and were not retaken.

Review notes from these captures (problems seen; each is marked fixed
where it has been, and the affected captures were retaken on 2026-10-19
with the fixes, see Reproducing):

- Edit › Scripts… grows taller on every frame it is drawn: within about a
  second of opening it fills the screen and keeps growing, with its title,
  list, and editor pushed above the top edge (the vertical separator
  between the list and the editor takes the full available height inside
  the auto-sized modal, so the modal grows with it). egui remembers the
  size, so reopening it in the same session shows it already oversized.
  The three Scripts captures were each taken in a fresh Studio about half
  a second after opening the window (hence the empty band above the
  reference text), with the script of interest made script 1 so it was
  selected on opening (see Reproducing). This makes the window unusable in
  practice and is the main finding of this round. (Fixed: the divider is
  painted to the height of the list and editor row instead of a separator
  taking the available height, and a test draws the window for 120 frames
  and checks that its rectangle stays the same.)
- Scripts: while Camera rotation waits, `Delete script` still looks
  enabled although the window's own rules say a paused script cannot be
  deleted; `1 actions` is not pluralized correctly; and `1500 frames waited
  so far` is shown at the very start of the wait (it is the frames the
  run's waits add up to, not the frames already waited). The failed-run
  line names only `a refused action`, not why it was refused (the CLI shows
  `the show has no input …`). (Fixed: `Delete script` is disabled with
  `this script is running; stop its run first`; the counts use plural forms
  in English and Spanish (`1 action`); the wait line reads `its waits total
  1,500 frames so far, this one included`; a run refused on its first frame
  adds the server's reason, `The server said: …`. A part that fails after a
  wait is reported from the replicated fault, which carries only its kind,
  line, and column, so its reason cannot be shown without a protocol
  change; its hover now says so.)
- LiveLAN: the `UNAVAILABLE` tooltip repeats the paragraph already shown
  under the chip and spills out of the dialog; `Viewer address No address
  yet` runs the label into a monospace value with no separation. (Fixed:
  the chip's hover is the one-line state help, and the address has a gap
  after its label.)
- Mixer `MON` strip: the headphone device field is too narrow for an ALSA
  name and shows `hw:CARD=Hea` (full name only on hover); the PHONES state
  tooltip covers the level and mute keys below it. Maximising the mixer
  leaves most of the panel empty (the strips keep their width). (Fixed:
  the set device shows as a key ending in an ellipsis with the full name
  on hover, and a click opens the field; the PHONES help opens beside the
  state, growing upwards. Maximising now widens and lengthens every strip:
  `studio-mixer-maximised-before.png` / `studio-mixer-maximised.png`.)
- Add input: the `+` tile still reads `colour · bars · stream · camera` and
  the dialog intro says "a live generator, network stream, or camera input",
  neither mentioning system audio; the sample says `Audio only` while the
  bank tile says `audio only`. (Fixed: the tile reads `colour · bars ·
  stream · camera · audio`, the intro names system audio, and the sample
  says `audio only`.)
- Recording & streams: each started destination's detail reads
  `Requested · no trans…`, cut off before the useful part. (Fixed: it reads
  `Started · no stats`, and a detail cut short leads the row's hover.)
- The Edit menu's `Undo` is disabled at launch although the show has a
  revision history from offline edits (only edits made in this session can
  be undone; there is no hint saying so). (Fixed: the disabled reason reads
  `there is no project edit to undo; only edits made since the server
  opened the show can be undone`; the history is the server's, kept in
  memory and empty when it opens a show.)

Round 5: captures of 2026-10-16 at Protocol 2.52, schema 49 (commit
a821e6d). Added: `studio-workspace-docked.png` (the full docked workspace),
`studio-cues.png` (RC-017 cue list with standby and GO),
`studio-streams-five.png`, `studio-stream-status-hover.png`,
`studio-stream-edit-dialog.png`, `studio-stream-remove.png`, and
`studio-stream-add-dialog.png` (OR-006 five destinations with independent
control and a masked key), and `studio-add-input-solid.png` /
`studio-add-input-network.png` (the sectioned Add input dialog with its
secret toggle). All use the simulated daemon and the synthetic meter feed.
`studio-cues.png` and the two stream panel captures are window captures of
the popped-out panel (`import -window`), so the rows are legible; the rest
are full-screen. The simulated daemon sends no stream transport samples, so
started destinations read `NOT LIVE` and the status hover has no bitrate
line. Every stream key in the demo show is a fake (`demo-…-not-real`).

Review notes from these captures (problems seen, not fixed here):

- Recording & streams at the Default layout fits only about three and a
  half destination rows; with five destinations the rest (and the HLS row)
  sit below a scroll (the round 5 `studio-workspace-docked.png`). The `1 / 2 RECORDING`
  and `5 TARGETS` header counts run into the panel's right edge / scroll
  bar. (Fixed since: rows are 38 px with the keys inside, so all five fit
  at the Default layout, and the counts keep a right gutter.)
- Each destination's `RETRY` / `EDIT…` / `✕` keys sit on a second line
  below the row's card instead of inside it, so a row reads as two
  (fixed since: they sit inside the row, or behind a `···` menu key in a
  narrow dock; `studio-streams-five.png` is the recapture).
- Docked Cues tab at the Default width: the cue rows are wider than the
  side panel, so the action-count key (`2 +`) and the right edge of each
  row are clipped behind a horizontal scroll bar; an opened cue's action
  rows wrap, putting the first action's move-down and remove keys on a line
  of their own (background of `studio-add-input-solid.png`).
- Cue rows keep a fixed name field, so longer names are cut off (`Close
  and stop str`, cut without an ellipsis), even in the wide popped-out window,
  where the rows also do not grow to the window width (`studio-cues.png`).
  Cues that have not played show an empty outlined chip rather than a
  label.
- The `GO` key label is left-aligned in a key that spans the whole panel.
- Destination dialog: the resolved URL hint under Server sits flush against
  the `Backup server` label with no spacing; the Backup server placeholder
  is the generic `live.example.com/app` even for a Facebook RTMPS server.
  The masked key shows one dot per character, which reveals its length.
  Typing a key removes the `A key is stored…` caption and leaves an empty
  gap above `OUTPUT AND VIDEO` (`studio-stream-edit-dialog.png`).
- The eye toggle's hover help spills outside the dialog over the Recording
  & streams panel (`studio-stream-add-dialog.png`).
- Add input (network stream): `Give up after (attempts)` shows `0` with no
  hint that it means never give up (`studio-add-input-network.png`).
- Without a window manager a popped-out panel stays above the console's
  modal dialogs (seen while capturing; the windows were moved aside first).

`studio-cues.png` and `studio-workspace-docked.png` were recaptured after
the Cues fixes that followed (same steps, Facebook Live not removed and
re-added): cue rows fill the panel width and their names take the width
the keys leave, ending in `…` with the full name on hover; in the docked
tab each row puts its follow box and keys on a second line so nothing
hides behind a scroll bar; an opened cue's action rows keep their move and
remove keys in a column on the first line and shrink or wrap their
pickers; cues not yet played read `QUEUED`; and `GO` is centred. Add input
now notes `0 = never give up` beside `Give up after (attempts)`.

Captures of 2026-10-16 at Protocol 2.48, schema 45 (commit 64f800a). Added:
`studio-network-stream-form.png` and `studio-network-stream-tile.png`
(IN-015), `studio-capture-picker.png` and `studio-capture-signal-lost.png`
(IN-001), `studio-ftb-outputs.png` (SW-004 per-output durations and curve),
`studio-tbar-cube.png` (a manual Cube Zoom), `studio-layer-keys.png`
(IN-020 keys and effects), `studio-mixer-monitor.png` (AU-001 PFL, nested
groups, `MON`), `studio-spanish-refusal.png`, and
`studio-fullscreen-outputs.png` (OR-011); `studio-default-layout.png` and
`studio-console-production.png` were recaptured. The network stream,
capture, T-bar, mixer, and fullscreen captures come from a native daemon
(`--features native-media`, `--native-media`) on Mesa lavapipe, so their
Program monitor is the native CPU readback, Preview reads `NO PREVIEW TAP`,
and the meters are the daemon's own; the other captures use the simulated
daemon and the synthetic meter feed, which carries no monitor-bus level, so
their `MON` strip reads `--`. The simulated daemon's Program render does not
show transitions in progress (it renders the realized source), which is why
the Cube Zoom capture uses the native daemon. This host was also running
other builds, so the native daemon fell behind: Program frames and meters
went stale for seconds at a time and the Field Feed tooltip counts thousands
of dropped frames; each capture was taken while its monitor was current.

Review notes from these captures (problems seen, not fixed here):

- The Layer designer's effect rows draw their move-up, move-down, and
  remove keys as empty boxes: the `▲`, `▼`, and `✕` glyphs are missing from
  the UI font (`studio-layer-keys.png`). The same panel shows Saturation's
  amount as a bare `40` but Sharpen's as `30.0 %`.
- The `+` tile still reads "solid colour · colour bars" although the Add
  input dialog now also offers network streams and cameras (every capture
  showing the `+` tile).
- Network stream form: the retry-wait error appears under the STREAM URL
  field instead of beside the retry fields, and the footer's `Unavailable:
  the first retry wait must not exceed the l…` runs under the `CANCEL` key.
  The dialog also re-centres (jumps vertically) whenever a hint line appears
  or disappears (`studio-network-stream-form.png`).
- The amber `RECONNECTING n` tile badge sits over the tile's `no thumbnail`
  caption, leaving `no` peeking out at its left (`studio-network-stream-tile.png`,
  `studio-capture-signal-lost.png`; captured before Protocol 2.59 live input
  thumbnails).
- Mixer detail rows on labelled strips (Host Mic, Guest Line) sit about 3 px
  higher than on the other strips (`studio-mixer-monitor.png`). At the
  Default layout the fifth input strip is a clipped sliver under the pinned
  `MON` strip (`studio-default-layout.png`, `studio-console-production.png`).
- With `▾ DETAILS` open during a manual transition, the transition tile
  scrolls and the Fade to Black block is pushed below its bottom edge
  (`studio-tbar-cube.png`).
- Fullscreen outputs: a waiting slot shows its raw selector
  (`name:Projector (not connected)`) and a `STANDBY` chip beside
  `Waiting: …`; while the console is reconnecting the dialog shows every
  slot `UNUSED` / `Not configured` instead of an unknown state.
- The Spanish refusal toast is now fully translated, but still shows the
  raw reason code `(input_in_use)`; the Spanish console keeps `SIP`, `PFL`,
  `AFL`, `MON`, `CLIP`, and `REC` in English and abbreviates `DESLIZ`.
- Not a capture artifact but seen while capturing: with two active
  fullscreen display outputs on the diagnostic display inventory, the native
  daemon dropped every Studio session within seconds
  (`FREEMIXD_SESSION … closed=write_failed`), so the capture uses one active
  and one waiting slot; even then Studio showed `NO PGM FEED` and
  `METERS UNAVAILABLE` while a display output was active.

Recapture of 2026-10-15/16 (Protocol 2.41, schema 39). Every capture in
the table was regenerated from one build of HEAD (commit 1bf8dd2) on the
synthetic meter feed, so every console capture now shows the `MORE` key,
the `FAVOURITES` row, the Fade to Black `Outputs 1/1` selector, the five
side tabs including `MEDIA`, the bank toolbar, and the mixer `PAN` knobs.
Added: `studio-media-transport.png` (IN-003), `studio-favourites.png`
(SW-002/SW-005), `studio-input-lifecycle.png` (IN-026), and
`studio-mixer-groups.png` (AU-001/SW-004); `studio-scene-designer.png`
is now a live capture of the Layer designer against freemixd instead of
the offscreen test render, and `studio-console-production.png` adds input
categories, colour strips, strip labels, and pans. `color-panel.png`
re-rendered byte-identical; `animation-curve-editor.png` was re-rendered
with the new presets row. The titles captures import schema 39 templates.

Review notes from this recapture (problems seen, not fixed here):

- Long favourite labels widen the transition column: with Cube Zoom,
  Cross Zoom, and Stinger 1 assigned (`studio-favourites.png`) the Program
  well slides under the side panel (header `Main Sce`, `SIMULATED` badge
  cut) and the mixer header's `GAIN · BAL · DLY` key is clipped.
- Mix names differ between surfaces: the Output menu lists `AlphaFade`
  (no space; Spanish says `Fundido alfa`), while the column key says
  `ALPHA`; `MORE`, the favourite Assign menu, and the overlay transition
  picker use `V-WIPE`, `FLY TURN`, `X-ZOOM`, and `STING 1`, and the
  overlay picker opens over its own row (`studio-console-overlay-details`
  session).
- The Layer designer's `SERVER PICTURE` badge and on-canvas layer names
  are green/blue text with no backing, nearly unreadable over a light
  canvas (`studio-scene-designer.png`).
- The keyboard reference's Console group starts its action column about
  8 px (Spanish 16 px) right of the other groups because of the wide
  `Ctrl+= · Ctrl+-` cap (`studio-console-shortcuts.png`,
  `studio-console-spanish-shortcuts.png`).
- At 1280 px and 1600 px wide the five side tabs ellipsize (`OVERL…`,
  `OVER…`, `STING…`, `PLAYL…`, `TÍTUL…`), so tab names are only in the
  tooltips (`-top`, `-stopped`, `-spanish-compact`).
- The mixer group strips (`Cameras`, `Talkback`) show no meter, so a
  group's level reads only from its fader (`studio-mixer-groups.png`).
- The Spanish refusal toast still mixes in the daemon's English detail
  (`a layer source of scene 100`), as before.

Screenshot-review fixes (2026-10-15, same protocol). These captures were taken
again after the fixes: `-top` (visible scroll bars, a one-line Master
label, dark rings behind the `SIMULATED` badges), `-input-menu` (`Grade…`
disabled for the operator), `-shortcuts` and `-spanish-shortcuts` (key caps
grow with their text; Spanish names the keys `Mayús` and `Intro`),
`-reconnecting` and `-spanish-reconnecting` (a friendly, localized last error
instead of `Io(ConnectionRefused)`; dimmed `CUT`/`TAKE` keys),
`-manual-transition` (the lower row no longer moves), and `-preferences`
(`--lang`/`--clock` in monospace). Other captures predate these fixes.

Recapture of 2026-10-15 (Protocol 2.30, after UI round 11 and the
Protocol 2.27-2.30 title phases, grading, Colour window, and Animation window
work). Every `studio-console-*.png` capture was regenerated from one build
of the current tree on the synthetic meter feed, so they all show the
collapsed `▸ DETAILS` manual block and `▸ GAIN · BAL · DLY` mixer header
(the manual and mixer captures show them expanded, as they are in use), the
View menu's `Colour grade…` / `Animate title…` items and Advanced controls
group, the tile menu's `Grade…` item, the Output menu's `Stinger slots…`
item, the `Animate…` key on each Titles row, and the `Ctrl+G` /
`Ctrl+Shift+A` rows of the keyboard reference. `-spanish-playlist` now shows
the signed `RESTANTE` countdown. `studio-color.png` was recaptured on the
demo show (with its stream targets) and `studio-console-production.png` was
added: a graded input on air from a running playlist, an animated lower
third on O1, and REC 2 recording. `color-panel.png` (re-rendered by its
test, byte-identical) and `studio-animation.png` (captured at Protocol 2.30)
were already current. The titles captures now import schema 31 templates
(see Reproducing).

UI round 11 (RC-018, discoverable operator UI) gave every interactive
control hover help in every shipped language: one line saying what it does
in operator language, its keyboard shortcut in brackets when it has one
(from the same table as the `?` reference), and, while it is disabled, the
standard `Unavailable: …` reason beneath that line. Menus, menu items,
submenus, side panel tabs, tile context menus, combo-box options, text and
number fields, colour swatches, and dialog keys gained the help they lacked.
A headless audit test in `fm-ui-egui` (`hover_audit.rs`) renders the full
console in English and Spanish across some twenty scenes (each side tab
as operator and viewer, the advanced controls opened, a manual transition in
flight, a locked project, Fade to Black with a running playlist, toasts,
every dialog, and a disconnected console), forces every tooltip, menu, and
submenu open, and fails if any of the ~600 interactive widgets lacks help, a
disabled control lacks its reason or its action, help runs past four lines,
or the Spanish text falls back to English. Before this round it found 109
controls without help and 64 disabled controls that showed only the reason.
The round also introduced progressive disclosure: the per-strip
gain/balance/delay fields of the audio mixer sit behind a `▸ GAIN · BAL ·
DLY` key in the mixer header, and the manual transition's position,
`COMMIT`/`CANCEL`, and status card behind a `▸ DETAILS` key beside
`MANUAL` (they still appear on their own while a manual transition is in
flight). Both start hidden, are also offered under View › Advanced controls,
and persist in `studio.conf` as `layout.mixer_details` and
`layout.manual_details`. Overlay channel rows already open their detail
cards on demand. This round regenerated `-overview` and added `-hover-help`
and `-spanish-hover-help`; the 2026-10-15 recapture below brought every other
console capture up to the disclosure keys.

UI round 10 made the high-contrast theme a distinct accessibility mode. It
now draws every surface pure black and separates panels, cards, keys, and
fields with 2 px light-grey outlines (`#c8c8c8`, `#f5f5f5` when
emphasized); text is white, secondary text `#f2f2f2`, and muted text
`#d6d6d6`; the focus ring is 3 px in a brighter accent; and small text
(captions, field labels, section labels, chips, badges, anything under
12 pt) grows by 1.5 pt. Dim marks that were hard-coded or faded (tile
ordinals, `no thumbnail`, the add tile's hint, monitor footnotes) follow
the palette instead. The side panel tabs take an exact quarter of the row
each, so four tabs never overflow a 320 px panel at 1280x720 in either
language. The palette test now also checks that the high-contrast surfaces
are black, outlines reach at least 3:1 on every surface (10:1 on black),
and muted text at least 4.5:1 on every surface. The Playlist tab shows
every countdown with a leading minus, as playout clocks do, so the status
card's `REMAINING` clock and the on-air row read the same
`-HH:MM:SS:FF`; durations are unsigned. Selecting a playlist row moves the
keyboard focus to the entry form's input picker, Enter in the duration
field adds the entry (or updates the selected one), and selecting a title
row focuses its first field when the session may edit. This round
regenerated `-high-contrast` and `-playlist` and added
`-spanish-high-contrast`; `-spanish-playlist` is from round 9 and still
shows its paused `RESTANTE` clock unsigned.

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
# The 2026-10-15/16 captures built `git archive HEAD` in a scratch tree with a
# separate target dir (CARGO_TARGET_DIR=/tmp/shots3-target) and the synthetic
# meter feed (see below), on DISPLAY :83 with --listen 127.0.0.1:7383.
cargo build -p freemix-cli -p freemixd -p freemix-studio \
    --features freemix-studio/capture-meter-signal

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

The docking captures of 2026-10-16 (Xvfb with no window manager, DISPLAY
:187, `--listen 127.0.0.1:7387`, a fresh `FREEMIX_STUDIO_PREFS` file, and
the synthetic meter feed) capture the Default layout as launched
(`studio-default-layout.png`); press the side tab group's grip
(`xdotool mousemove 1476 580`, `mousedown 1`), `mousemove` in 14 steps to
50,820, and capture while holding (`studio-docking.png`); release over the
group's own tile, which changes nothing. They then click the Program tile's
pop-out key (1422,93), move and resize the new window with `xdotool
windowmove`/`windowsize` to 1180,520 at 700x460, press its header grip
(1196,535), move in 14 steps to 845,760 over the console window, and
capture while holding (`studio-popout.png`); `mouseup 1` docked Program
right of the input bank. Popped out again, the window's `Dock back to…` key
(1790,535) opens the menu; hovering Preview, then its `To its right` item,
gives `studio-popout-dock-menu.png`, and a click docked Program there.
Studio's SIGTERM leaves the supervised `freemixd` running; stop it too. The
older layout capture (before docking)
dragged the side panel's left edge (`xdotool mousemove 1550
700`, `mousedown 1`, `mousemove` in steps to x 1430, `mouseup 1`), unticks
View › Audio mixer, restarts Studio with the same `FREEMIX_STUDIO_PREFS`
file, opens View, and hovers `Reset layout`. The health-strip capture hovers
the `TX` lamp at the right end of the status bar.

The maximised-mixer and shortcut captures (2026-10-19, Protocol 2.63, no
protocol change) run Studio with `--role admin` and the synthetic meter
feed on DISPLAY :191 (before, a build of the previous head) and :192
(after) with a fresh `FREEMIX_STUDIO_PREFS` file. The mixer is maximised by
double-clicking its `AUDIO MIXER` tab (`xdotool mousemove 945 583`, `click
--repeat 2 --delay 40 1`). The reference is opened with `xdotool key
question` after `Esc` restores the tile (Spanish: `--lang es` and `key
F1`); the menu capture clicks `Output` (234,12) and hovers `Start all
streams` (262,577).

The edit-menu, locked, and mixer captures run Studio with `--role admin`
(the default operator role lacks `edit_project`), then drive the daemon with
`freemix-cli remote-rename 127.0.0.1:7000 6 "Playback Clip B"`, a rename
back, `remote-rename … 3 "Host Laptop 2"`, `remote-undo`, `remote-lock … on
--input 3`, click `Edit`; then `remote-lock … on` and click the `STINGERS`
tab; then `remote-lock … off` and `remote-audio-strip` for inputs 1, 2, 4,
5, and 6, click the mixer header's `▸ GAIN · BAL · DLY` key, and crop the
mixer (760x555 at +785+488). The 2560 capture sizes the screen and
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

and click the `PLAYLIST` tab (x 1865, y 380 at 1920x1080 with the demo's
stream targets). The round 10 English capture was taken about 50 seconds
after `start` without a restart. The Spanish capture was taken after Studio
was restarted on the same project while the playlist ran, so the tab shows
the `playlist_state` that follows the snapshot; it first runs `remote-playlist 127.0.0.1:7000 pause` and adds `--lang
es`. Until the screenshot-review fixes below, the simulated daemon executed
Pause at its frame cursor, which lagged wall time between playlist
boundaries, so this capture's paused entry shows only its transition's 24
frames elapsed (`-00:00:44:36`) although it had played for about 43 s; a
playlist control now first catches the simulated engine up to the show's
wall-clock frame.

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
and two title inputs imported from `.fmtitle` sources (the document
carries `"schema_version": 39` (it must equal the current project schema),
`"clocks": []` beside `template` and `fonts`, the template an
`animation` block such as `{"timebase": "30/1", "revision": 0,
"in_frames": 0, "out_frames": 0, "tracks": []}`, and elements no
`animations` key) drawn with
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
Name draft (ASCII: `xdotool type` drops characters such as `ó` that the Xvfb
keymap lacks), then starts a second Studio with `--connect 127.0.0.1:7000
--project-id <the project id in project.json> --role admin` (with its own
`FREEMIX_STUDIO_PREFS`), changes Name there, clicks `UPDATE LIVE`, closes it,
and hovers `ACTUALIZAR EN VIVO` in the first. The Preferences capture presses
Ctrl+, (after `xdotool windowfocus`), clicks `12-hour (AM/PM)`, and presses
Tab eight times; the high-contrast capture writes `ui.high_contrast = on` to
the scratch `FREEMIX_STUDIO_PREFS` file before launching Studio, then starts
REC 2 and takes O1; `-spanish-high-contrast.png` repeats it with `--lang es`
(clicking `TOMAR`). Typing into Studio's fields with `xdotool type` also needs
`xdotool windowfocus` first.

The animation capture imports a schema 39 `lower-third.fmtitle` (a 1920x1080
template: rectangles Bar (fill from a Bar colour field) and Accent, and texts
Name and Role in DejaVu Sans Bold and DejaVu Sans) as input 8, animates it
offline, and runs Studio with `--role admin`:

```sh
freemix-cli title-input-add demo.freemix --id 8 --name "Lower Third" \
    --template lower-third.fmtitle \
    --font "DejaVu Sans=$D/DejaVuSans.ttf" --font "DejaVu Sans Bold=$D/DejaVuSans-Bold.ttf"
T=asset://titles/lower-third.fmtitle
freemix-cli title-animation-preset demo.freemix $T 1 --phase in --preset slide-from-left --frames 18
freemix-cli title-animation-preset demo.freemix $T 2 --phase in --preset wipe --frames 12
freemix-cli title-animation-preset demo.freemix $T 3 --phase in --preset fade --frames 24
freemix-cli title-animation-stretch demo.freemix $T --phase in --frames 30
freemix-cli title-animation-preset demo.freemix $T 1 --phase out --preset fade --frames 15
```

It clicks `TITLES` and the row's `Animate…`, drags the window's bottom-right
corner from (1382, 673) to (1700, 1000) in small steps with a pause after
each (`xdotool mousedown`, several `mousemove`s, `mouseup`; a single jump is
not seen as a drag), clicks the ruler at frame 10 (x 813, y 667), clicks
Role's `Preset…` (x 710, y 789), rests the pointer on `Slide from bottom`
(x 730, y 931, nudged by a pixel), and waits about four seconds for the
exact preview and the hover help.

The stagger captures (2026-10-19, DISPLAY :173, `--listen 127.0.0.1:7473`,
a fresh `FREEMIX_STUDIO_PREFS`) import the same template (its
`schema_version` set to the current project schema) and animate all four
elements In over the same 15 frames:

```sh
freemix-cli title-animation-preset demo.freemix $T 1 --phase in --preset slide-from-left --frames 15
freemix-cli title-animation-preset demo.freemix $T 2 --phase in --preset wipe --frames 15
freemix-cli title-animation-preset demo.freemix $T 3 --phase in --preset fade --frames 15
freemix-cli title-animation-preset demo.freemix $T 4 --phase in --preset slide-from-bottom --frames 15
freemix-cli title-animation-preset demo.freemix $T 1 --phase out --preset fade --frames 15
```

They click `TITLES` (1634, 581) and `Animate…` (1866, 648), drag the
window corner as above, click `Stagger…` (1068, 614), and rest the pointer
on `Apply stagger` (1079, 718) for `-stagger-menu`; then click it, click
`Fit` (937, 614), click the ruler at frame 20 (1336, 668), and rest the
pointer on the next-key key (772, 642) for `-staggered`. The re-captured
`-stagger-menu` (DISPLAY :181, `--listen 127.0.0.1:7481`, the template's
`schema_version` 52) instead clicks `Fit` (937, 614) after the corner drag,
zooms out with `xdotool windowfocus`, then Ctrl+wheel down three times and
twice more and up once at (776, 745), clicks `Stagger…` (1068, 614), and
rests the pointer on `Apply stagger` (1079, 746) for three seconds.

The production capture (`studio-console-production.png`) builds the demo
show with input 5 renamed "Colour Fill", the animation capture's template
imported as input 8 with a Role In preset added and a longer In phase, and a
library grade on Guest Remote:

```sh
T=asset://titles/lower-third.fmtitle
freemix-cli title-animation-preset demo.freemix $T 4 --phase in --preset slide-from-bottom --frames 24
freemix-cli title-animation-stretch demo.freemix $T --phase in --frames 150
freemix-cli grade-set demo.freemix 4 --preset warm --saturation 1100 --save-as "Guest warm"
```

It runs Studio with `--role admin`, sets and starts a looping playlist whose
first entry airs Guest Remote for ten minutes
(`remote-playlist … set all 1:4:36000:cut:0:1,2:2:1200:fade:30:1,3:7:1800:fade:45:1,4:1:1500:wipe:20:1`,
then `start`), starts REC 2, clicks `TITLES`, `PREVIEW TITLE`, and `TAKE`
(O1), then clicks the `PLAYLIST` tab. Frames grabbed every 0.4 s after
`TAKE` all showed the title at its final pose, so the capture holds the
taken state.

The 2026-10-16 production capture also runs, before launching Studio,

```sh
freemix-cli input-categories demo.freemix 1=Cameras 2=Remote 3=Graphics
freemix-cli input-category demo.freemix 1 1   # 2 1, 3 2, 4 2, 5 3, 8 3
freemix-cli input-color demo.freemix 1 blue   # 2 blue, 3 green, 4 orange, 5 purple, 6 teal, 7 red, 8 yellow
freemix-cli audio-label demo.freemix 3 "Host Mic"
freemix-cli audio-label demo.freemix 4 "Guest Line"
freemix-cli audio-strip demo.freemix 1 -3000 0 off off on 0 --pan -3000
freemix-cli audio-strip demo.freemix 2 -3000 0 off off on 0 --pan 3000
freemix-cli audio-strip demo.freemix 5 0 0 on off off 0
freemix-cli audio-strip demo.freemix 8 0 0 on off off 0
```

and clicks `PREVIEW TITLE` before `TAKE`.

The new feature captures start from the demo show with `--role admin`:

- `studio-media-transport.png` and `studio-input-lifecycle.png` add
  `media-input-add demo.freemix 8 "Intro Clip" asset://media/intro.mp4` and
  `… 9 "Sponsor Loop" asset://media/sponsor-loop.mov` (no file needed: the
  simulated daemon opens media inputs on a synthetic source),
  `input-categories demo.freemix 1=Cameras 2=Remote 3=Playback 4=Graphics`,
  `input-category` and `input-color` for every input, `input-pause … 3 on`,
  `input-close … 6 on`, and `input-media-end-action … 9 loop`; the
  lifecycle capture hides the mixer from View, right-clicks Guest Remote and
  hovers `Colour label`; the media capture runs
  `remote-input-media-play … 8`, `remote-input-media-seek … 9 4s`,
  `remote-input-media-play … 9`, `remote-input-media-pause … 9`, and clicks
  `MEDIA`.
- `studio-favourites.png` adds the media input as input 8,
  `stinger-configure demo.freemix 1 8 true 15 mix-with-program cut`, and
  `transition-favorite-set demo.freemix 2 cube-zoom 20` (`3 cross-zoom 45`,
  `4 stinger-1 30`), right-clicks the fourth favourite key and hovers
  `Assign…`.
- `studio-mixer-groups.png` adds `output-add demo.freemix 2 100 1 "Venue
  Screens"` and `… 3 … "Confidence Monitor"`, `ftb-output demo.freemix 3
  off`, `audio-strip … --pan` for inputs 1, 2, and 4, `audio-label` for 3
  and 4, `audio-group-name demo.freemix 1 "Cameras"` / `2 "Talkback"`,
  `audio-group` 1 1, 2 1, 3 2, 4 2, `audio-group-level demo.freemix 1 -6000
  off` and `2 0 on`, drags the mixer's scroll bar right, and clicks
  `Outputs 2/3`.
- `studio-scene-designer.png` adds `scene-layer-crop demo.freemix 100 1 240
  0 1680 1080`, presses Ctrl+Shift+L, clicks Guest PiP in the layer list,
  and drags the PiP on the canvas up and left in small steps.

The hover-help captures take O1, move the pointer onto the `FADE` (or
`FUNDIDO`) key with `xdotool mousemove`, nudge it by a pixel so egui sees a
hover, and wait about three seconds for the tooltip before `import`.

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
`freemix-studio --connect 127.0.0.1:7399 --project-id 1` with nothing listening
(add `--lang es` for the Spanish one). The Spanish transition capture hovers
the T-bar handle after the drag; the Spanish Stingers capture clicks `FTB`,
the `STINGERS` tab, and hovers the S4 card.
The monitors captures start from the demo show (Main Scene on Program) for
`-monitors-scene.png`, then click `CUT` and the Camera 1 Wide tile and hover
the Program well for `-monitors.png`. The manual-transition capture clicks
the `MANUAL` row's `FADE`, then drags the T-bar handle upward
(`xdotool mousedown 1`, `mousemove`, `mouseup 1`); the Fade-to-Black capture
clicks `FTB`. Recording start can stall the simulated daemon for a few
seconds, so allow it to settle before capturing the overview.

The captures of 2026-10-16 at Protocol 2.48 built `git archive HEAD` in
`/tmp/shots4-src` with `CARGO_TARGET_DIR=/tmp/shots4-target`, on Xvfb
`:91` (1920x1080x24) with port `127.0.0.1:7391`, a fresh
`FREEMIX_STUDIO_PREFS` file per launch, and

```sh
cargo build -p freemix-cli -p freemixd -p freemix-studio \
    --features freemix-studio/capture-meter-signal,freemixd/native-media
```

(`native-media` builds on Linux without FFmpeg development packages; the
`ffmpeg` binary is still needed at run time for network streams).

- `studio-default-layout.png`: the demo show as above, Studio supervising
  freemixd, captured as launched.
- `studio-console-production.png`: as described above, with the
  `lower-third.fmtitle` template's `"schema_version"` raised to 45; the
  Titles tab's `TAKE` key now sits below the fold, so the capture scrolls
  the side panel before clicking it.
- `studio-ftb-outputs.png` and `studio-layer-keys.png` (simulated daemon,
  `--role admin`): the demo show plus `output-add demo.freemix 2 100 1 "Venue
  Screens"` and `… 3 … "Confidence Monitor"`, `ftb-output demo.freemix 3
  off`, `ftb-output-duration demo.freemix 1 45` and `… 2 90`, `ftb-curve
  demo.freemix ease`, `scene-layer-key demo.freemix 100 1 chroma 0 177 64 90
  24 128`, `scene-layer-effect-add demo.freemix 100 1 saturation 40`, and
  `… sharpen 300`. Click `Outputs 2/3` for the first; press Ctrl+Shift+L,
  click Guest PiP in the layer list, and drag the designer window's corner
  to the bottom-right of the screen for the second.
- `studio-spanish-refusal.png`: the demo show with `--lang es --role
  admin`; right-click Camera 1 Wide, `Quitar…`, `QUITAR`.
- The native captures (network stream, capture, T-bar, mixer, fullscreen)
  run freemixd on its own and connect Studio to it with `--connect
  127.0.0.1:7391 --project-id <id> --role admin`, without
  `FREEMIX_STUDIO_CAPTURE_METERS`. The native runtime refuses simulated sine
  audio, so the demo show is built without stream targets and inputs 1, 2,
  3, 4, and 6 are replaced with silent solids (`input-replace-solid
  demo.freemix 1 70 110 160 255`, …). It then adds

  ```sh
  freemix-cli input-add-stream demo.freemix 8 "Field Feed" udp://127.0.0.1:7591 \
      --jitter-ms 200 --backoff-min-ms 500 --backoff-max-ms 2000
  freemix-cli input-add-stream demo.freemix 9 "Venue Camera RTSP" \
      rtsp://operator:s3cret@127.0.0.1:7592/live --transport tcp \
      --backoff-min-ms 500 --backoff-max-ms 2000 --timeout-ms 1500
  freemix-cli input-add-capture demo.freemix 10 "Desk Cam" "v4l2:usb-0000:00:14.0-1#Fake Cam" --loss black
  freemix-cli input-add-capture demo.freemix 11 "Spare Cam" "v4l2:usb-0000:00:14.0-2#Spare Cam" --loss black
  freemix-cli audio-label demo.freemix 3 "Host Mic"      # and 4 "Guest Line"
  freemix-cli audio-group-name demo.freemix 1 "Cameras"  # 2 "Talkback", 3 "All Mics"
  freemix-cli audio-group demo.freemix 1 1               # 2 1, 3 2, 4 2
  freemix-cli audio-group-parent demo.freemix 2 3
  freemix-cli audio-group-level demo.freemix 1 -6000 off
  freemix-cli audio-solo-mode demo.freemix pfl
  freemix-cli audio-strip demo.freemix 8 -3000 0 off on off 0
  ```

  starts the loopback sender `ffmpeg -re -f lavfi -i
  testsrc2=size=640x360:rate=25 -f lavfi -i sine=frequency=440 -c:v
  mpeg2video -c:a mp2 -f mpegts "udp://127.0.0.1:7591?pkt_size=1316"`, and
  runs `freemixd serve demo.freemix --listen 127.0.0.1:7391 --native-media
  --diagnostic-capture-inventory capture-inventory.txt`. The capture
  inventory has one tab-separated line per device, for example
  `key=v4l2:usb-0000:00:14.0-1#Fake Cam  name=Fake Cam  path=/dev/video0
  formats=640x360@25/1:yuyv:p,1920x1080@30/1:mjpeg:p,…  rgba=c83c3cff`;
  rewriting it (atomically, through a rename) without Spare Cam's line
  unplugs that device. The tile captures hover the Field Feed tile (and the
  RTSP tile) for about four seconds; the form captures click the `+` tile,
  `NETWORK STREAM`, type the name and URL, click `TCP`, and set the longest
  retry wait to 400, then choose `CAMERA / CAPTURE`, rename the input, click
  Fake Cam and `1920x1080 30p MJPEG`. The signal-lost capture rewrites the
  inventory, scrolls the bank, and hovers Spare Cam. The T-bar capture
  clicks Field Feed, `MANUAL` › `MORE` › `Cube zoom`, drags the T-bar handle
  from (862, 281) to (862, 210) in 16 steps, and clicks `▸ DETAILS`. The
  mixer capture double-clicks the `AUDIO MIXER` tab and opens `▸ GAIN · BAL
  · DLY`.
- `studio-fullscreen-outputs.png` restarts that daemon with
  `--diagnostic-display-inventory displays.tsv` after
  `display-output-set demo.freemix 1 "display=name:Projector,source=program,scaling=fit"`
  and `display-output-set demo.freemix 2
  "display=id:right,source=output:1,mode=current,scaling=fill"`, where
  `displays.tsv` holds `id=left  name=DELL U2720Q  x=0  y=0
  mode=1920x1080@60  modes=3840x2160@60;2560x1440@60;1920x1080@60  hdr=hdr`
  and `id=right  name=Venue LED Wall  x=3840  y=0  mode=1920x1080@50
  modes=1920x1080@50;1280x720@50  hdr=sdr` (tab-separated). With both slots
  active the daemon kept dropping the Studio session (see the review notes),
  hence the waiting `Projector` slot. It opens Output › `Fullscreen
  outputs…`.

The round 5 captures (2026-10-16, Protocol 2.52, schema 49) built `git
archive HEAD` in `/tmp/shots5-src` with `CARGO_TARGET_DIR=/tmp/shots4-target`
and `--features freemix-studio/capture-meter-signal`, on Xvfb `:95`
(1920x1080x24) with `--listen 127.0.0.1:7395`, `--role admin`, a fresh
`FREEMIX_STUDIO_PREFS` file, and `FREEMIX_STUDIO_CAPTURE_METERS=1`. The show
is the demo show above, but with five stream destinations (fake keys) and
two cue lists in place of the two stream targets:

```sh
freemix-cli stream-add demo.freemix 1 1 rtmps://a.rtmp.youtube.com/live2 demo-yt-key-not-real "YouTube Main" \
    --backup rtmps://b.rtmp.youtube.com/live2 --video-bitrate-kbps 6000
freemix-cli stream-add demo.freemix 2 1 rtmp://live.twitch.tv/app demo-twitch-key-not-real "Twitch Backup" \
    --video-bitrate-kbps 4500 --reconnect 500:5000:20
freemix-cli stream-add demo.freemix 3 1 rtmps://live-api-s.facebook.com:443/rtmp demo-fb-key-not-real "Facebook Live" \
    --video-bitrate-kbps 4000
freemix-cli stream-add demo.freemix 4 1 srt://srt.example.com:9000 demo-srt-passphrase "Venue SRT Feed" \
    --video-bitrate-kbps 8000 --reconnect 250:2000:none
freemix-cli stream-add demo.freemix 5 1 hls:///tmp/hls "Local HLS Archive" --rung 2500:1280x720 --rung 6000:1920x1080
freemix-cli cue-list-set demo.freemix 1 "Evening Run" "1:Pre-show standby:0:preview.1,record.2.1;\
2:Show open:0:take.-.fade.30,stream.1.1,stream.2.1;3:Host intro:0:preview.3,take.-.fade.20;\
4:Guest interview:1:take.4.wipe.20,overlay_on.1.5;5:Lower third off:0:overlay_off.1;\
6:Break to black:0:fade_to_black.1.45;7:Back from break:0:fade_to_black.0.30,take.1.cut.0;\
8:Close and stop streams:0:fade_to_black.1.60,stream.1.0,stream.2.0,record.2.0"
freemix-cli cue-list-set demo.freemix 2 "Contingency" \
    "1:Technical difficulties:0:take.5.cut.0,audio_mute.4.1;2:Resume programme:0:audio_mute.4.0,take.7.fade.30"
```

(the cue string is one argument; it is wrapped here for width). The steps:
click the side group's `CUES` tab and `GO` twice (cue 1, then cue 2);
maximise Recording & streams (double-click its tab), click Venue SRT Feed's
`START`, and restore it; click its pop-out key, `xdotool windowmove` /
`windowsize` the new window to 560,150 at 820x610, and `import -window` it
(`studio-streams-five.png`), then hover YouTube Main's `NOT LIVE` chip for
about four seconds (`studio-stream-status-hover.png`). Click Facebook Live's
`EDIT…`, move the popped window out of the way (with no window manager it
stays above the console's dialogs), click the Stream key field, focus the
console window, and `xdotool type` a fake key
(`studio-stream-edit-dialog.png`); `CANCEL`, then the window's dock-back key.
Click Facebook Live's `✕` (`studio-stream-remove.png`), `REMOVE`, `ADD…`,
type the name, click `RTMPS`, type the server and a fake key, and hover the
eye toggle (`studio-stream-add-dialog.png`); `ADD`. Pop out the Cues tab,
size it to 900x720, open Show open's `3 +` action key, and `import -window`
it (`studio-cues.png`); close the actions and dock it back. Click the `+`
tile (`studio-add-input-solid.png`), then `NETWORK STREAM`, type the name
and `rtsp://operator:demo-pass@10.0.0.42:554/stream1`
(`studio-add-input-network.png`), `CANCEL`, and capture the console with the
pointer parked on the status bar (the round 5 `studio-workspace-docked.png`,
since replaced; see below).

`studio-workspace-docked.png` was recaptured at Protocol 2.59 for live input
bank thumbnails on a smaller show built from the CLI demo, with a fresh
`studio.conf`, a 1920x1080 window, and the pointer parked on the status bar
about four seconds after the window appeared:

```sh
freemix-cli demo demo.freemix program.ppm
freemix-cli project-rename demo.freemix "Evening Broadcast"
freemix-cli rename demo.freemix 1 "Camera 1 Wide"
freemix-cli rename demo.freemix 2 "Camera 2 Close"
freemix-cli input-add-stream demo.freemix 3 "Host Laptop" "rtsp://10.0.0.42:554/stream1"
freemix-cli input-add demo.freemix 4 "Guest Remote"
freemix-cli simulated-solid-input-add demo.freemix 5 "Lower Third" 20 60 140 255
freemix-cli input-add-system-audio demo.freemix 6 "Desktop Audio" pulse "@DEFAULT_MONITOR@"
freemix-cli scene-input-add demo.freemix 7 100 "Main Scene"
freemix-cli scene-layer-add demo.freemix 100 1 0 "Wide Shot"
freemix-cli scene-layer-add demo.freemix 100 4 1 "Guest PiP"
freemix-cli scene-layer-geometry demo.freemix 100 0 0 0 1920 1080 0
freemix-cli scene-layer-geometry demo.freemix 100 1 1360 740 480 270 0
freemix-cli audio-bus-add demo.freemix 1 "Program Bus"
freemix-cli output-add demo.freemix 1 100 1 "Program Output"
freemix-cli stream-add demo.freemix 1 1 hls:///tmp/fmshots/hls "Local HLS Ladder" \
    --startup reconcile-desired-state --rung 2500:1280x720 --rung 6000:1920x1080
freemix-cli stream-add demo.freemix 2 1 rtmp://live.example.com/app demo-stream-key-not-real "RTMP Backup"
freemix-cli preview demo.freemix 7
freemix-cli cut demo.freemix
freemix-cli preview demo.freemix 2
```

The round 6 captures (2026-10-19, Protocol 2.62, schema 56) built `git
archive HEAD` in `/tmp/shots6-src` with `CARGO_TARGET_DIR=/tmp/shots4-target`
and `--features freemix-studio/capture-meter-signal`, on Xvfb `:96`
(1920x1080x24) with `--listen 127.0.0.1:7396`, `--role admin`, a fresh
`FREEMIX_STUDIO_PREFS` file per launch, and `FREEMIX_STUDIO_CAPTURE_METERS=1`.
The show is the Protocol 2.59 workspace show above with the round 5 five
destinations (Venue SRT Feed's passphrase `demo-srt-passphrase-not-real`,
Local HLS Archive writing under the scratch directory) in place of its two
stream targets, plus

```sh
freemix-cli audio-label demo.freemix 3 "Host Mic"
freemix-cli audio-label demo.freemix 4 "Guest Line"
freemix-cli script-set demo.freemix 1 "Camera rotation" rotate.fms
freemix-cli script-set demo.freemix 2 "Guest in" guest.fms
freemix-cli script-set demo.freemix 3 "Break bumper" bumper.fms
freemix-cli cue-list-set demo.freemix 1 "Evening Run" "1:Pre-show standby:0:preview.1,record.2.1;\
2:Show open:0:take.-.fade.30,stream.1.1,stream.2.1;3:Host intro:0:preview.3,take.-.fade.20;\
4:Guest interview:0:run_script.2,take.-.wipe.20;5:Guest off:0:overlay_off.1;\
6:Break to black:0:fade_to_black.1.45;7:Back from break:0:fade_to_black.0.30,take.1.cut.0;\
8:Close and stop streams:0:fade_to_black.1.60,stream.1.0,stream.2.0,record.2.0"
freemix-cli cue-list-set demo.freemix 2 "Contingency" \
    "1:Technical difficulties:0:take.5.cut.0,audio_mute.4.1;2:Resume programme:0:audio_mute.4.0,take.7.fade.30"
freemix-cli audio-monitor-device demo.freemix "hw:CARD=Headset,DEV=0"
freemix-cli audio-monitor-level demo.freemix -6000 off
freemix-cli live-lan-set demo.freemix --enable --source program --max-viewers 8
```

(the cue string is one argument), where `rotate.fms` is

```text
# Rotate the two cameras every minute until someone stops the run.
let shots = 0
repeat 4
  if program == 1
    transition fade, 25, "Camera 2 Close"
  else
    transition fade, 25, "Camera 1 Wide"
  end
  let shots = shots + 1
  wait 1500
end
```

`guest.fms` is `preview "Guest Remote"`, `overlay 1, "Guest Remote"`, and
`gain "Guest Remote", -3000`, and `bumper.fms` is `transition fade, 25,
"Camera 1 Wide"`, `wait 50`, and `cut "Sponsor Loop"` (no such input, so
its second part is refused), each with a leading `#` comment line. The
steps: click the side group's `CUES` tab, `GO` twice (allowing a few seconds
for the recorder), and `START` on Venue SRT Feed, then capture the console
(`studio-workspace-docked.png`). Open Output › `LiveLAN viewers…` and rest
the pointer on the `UNAVAILABLE` chip (`studio-livelan-unavailable.png`);
`Escape`, click the headphone `M` on the `MON` strip, and crop the docked
mixer (`studio-mixer-phones.png`); click the `+` tile and `SYSTEM AUDIO`
(`studio-add-input-system-audio.png`), `CANCEL`. Pop out the Cues tab, size
it to 900x760, click `GO` twice more (Host intro, Guest interview) and
Guest interview's `2 +` action key (`studio-cues-script.png`).

The first round 6 Scripts captures were taken while the window still grew
on every frame: each started a fresh Studio with `--connect 127.0.0.1:7396
--project-id <id> --role admin` against the same daemon, opened Edit ›
`Scripts…` (moving the pointer onto the item before clicking; a jump and
click in one step is not seen), and kept a grab about half a second after
opening. The recaptures after the round 6 fixes (Xvfb `:97`, `--listen
127.0.0.1:7397`, the same show) open the window once and keep it open
through the three stages, waiting several seconds before each grab, and
crop it (800 px wide). Their other captures repeat the steps above in one
Studio, which was restarted with `--connect` before the mixer capture;
the mixer capture clicks the headphone `M` and rests the pointer on
`PHONES IDLE`. The script to show is made script 1, the one selected on
opening, with `remote-script-set`:

- failed run: `remote-script-set … 1 "Break bumper" bumper.fms` and `… 3
  "Camera rotation" rotate.fms`, `remote-script-run … 1`, wait for the
  fault (`remote-scripts` shows `state=faulted … line=4`), then open the
  window (`studio-scripts-failed.png`);
- cue run: `remote-script-set … 1 "Guest in" guest.fms`, re-set Evening Run
  with cue 4's action `run_script.1`, then `… 2 "Break bumper" bumper.fms`
  (a script that waits cannot replace one a cue runs); put another input
  on Program (`remote-preview … 2`, `remote-cut`, `remote-preview … 1`) so
  the script's `preview "Guest Remote"` is allowed, start the fresh Studio,
  run `remote-go … 1 4` while it is connected (the cue's script report is an
  event, not part of the snapshot), and open the window
  (`studio-scripts-cue-run.png`);
- paused run: restore 2 "Guest in", cue 4 `run_script.2`, 1 "Camera
  rotation", and 3 "Break bumper", start the fresh Studio, run
  `remote-script-run … 1`, and open the window
  (`studio-scripts-waiting.png`).
