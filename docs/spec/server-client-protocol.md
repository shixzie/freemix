# Server/client protocol

[Specification index](README.md)

## 1. Transport surface

| Purpose | Transport |
|---|---|
| Discovery | mDNS/DNS-SD locally; explicit URL and optional rendezvous remotely |
| Capabilities, projects, assets, administration | HTTPS REST |
| Commands, results, ordered events, telemetry | Authenticated WebSocket |
| Remote previews and guest media | WebRTC |
| Local native previews | Shared GPU/image handles plus authenticated local IPC when supported |
| Metrics/health | HTTPS; OpenTelemetry/Prometheus adapters where configured |

The semantic protocol is independent of transport. A future QUIC transport may
reuse the same messages. Full-resolution production frames are never serialized
into control messages.

## 2. Connection sequence

1. Client resolves the engine and validates TLS identity.
2. Client sends the current protocol identifier, build, client type, desired role, and cached
   cursor `(engine_id, state_epoch, log_id, revision)`.
3. Server authenticates, requires the exact current protocol, and returns permissions,
   capabilities digest, engine identity, and current revision.
4. Server sends missing ordered events or a fresh snapshot.
5. Client builds its read model and subscribes to selected telemetry.
6. Client requests only visible preview renditions.
7. Heartbeats carry last applied revision and clock sample. After the server
   validates a heartbeat, it returns the session server identity, the exact
   client heartbeat sequence, and the server receive time.
8. Reconnect resumes only when all cursor identity fields match; project
   replacement, restore, log compaction, or engine identity change forces a
   snapshot and a new cursor.

The current Protocol 2.32 implementation uses bounded newline-delimited raw TCP.
Studio keeps one expected heartbeat sequence and waits for its matching
acknowledgement within the bounded peer wait. EOF, timeout, wrong server
identity, or wrong sequence enters the existing reconnect backoff. The server
does not acknowledge an invalid heartbeat. This exchange gives bidirectional
control-plane peer liveness only. It is not service readiness, production
authentication, media health, or the planned HTTP/WebSocket transport.

The optional loopback WebSocket gateway at `/v1/control` relays the same
records. It accepts `FREEMIXD_WEB_TOKEN` either as an `Authorization: Bearer`
header or, for browsers that cannot set that header, as a second
`Sec-WebSocket-Protocol` value `freemix.bearer.<base64url(token)>` offered
alongside `freemix.control.v1`. The subprotocol credential is accepted only
when the `Origin` matches the gateway's own host, is compared in constant time,
and is never echoed; the server selects only `freemix.control.v1`. A present
but wrong Bearer header is rejected even if the subprotocol credential is
correct. The same listener serves the self-contained `/tally` page for `GET`
and `HEAD`; roles and permissions still come from the protocol handshake.

Snapshots carry bounded project-order input and output catalogs as exact ID/name pairs.
The canonical persisted name labels input tiles and mixer strips; clients do not
invent ordinal display names. The `input_renamed` and `input_order_changed`
events carry durable exact-current name and order edits; since Protocol 2.23
the `input_added` and `input_removed` events carry live catalog additions and
removals (section 3), so clients no longer need a fresh snapshot for them.
Protocol 2.16 additionally replicates one
validated streaming-destination roster: every configured stream target with its
canonical name, desired running flag, and last reported realized state.
`stream_start` and `stream_stop` mutate exactly one target per command, are
authorized as transition operations, and are acknowledged durably before any
runtime realization; each acceptance emits one durable `streams_changed` event
carrying the full bounded roster so clients resync from any cursor.

## 3. Command semantics

Conceptual envelope:

```json
{
  "protocol": 1,
  "id": "01K...",
  "idempotency_key": "operator-7:01K...",
  "expected_revision": 1842,
  "deadline_ms": 500,
  "payload": {
    "type": "transition",
    "mix_id": "019...",
    "effect": "fade",
    "duration_ms": 300
  }
}
```

The result is accepted with a revision and optional scheduled media timestamp,
or rejected with a stable error code, field details, current revision, and
retryability.

Rules:

- Duplicate idempotency keys return the original result.
- `expected_revision` protects editor-style updates; immediate live commands
  may omit it when their semantics are commutative or last-intent-wins.
- Transactions either commit all durable state or none.
- Frame-boundary commands acknowledge state acceptance separately from runtime
  realization. Later lifecycle events report `preparing`, `scheduled`,
  per-domain `realized`, `failed`, or `superseded` with runtime generation.
- High-rate fader/PTZ updates use a coalescible stream with a final committed
  value. Intermediate intents do not advance durable revision; the final
  commit does.
- Command permissions are checked before validation reveals sensitive details.
- The current input-audio mutation replaces one complete Master strip
  atomically: gain in `-96000..=24000` milli-dB, stereo balance in
  `-10000..=10000` basis points, mute, follow-video, and delay in
  `0..=48000` samples. Negative balance attenuates the right destination,
  positive balance attenuates the left destination, and non-stereo destination
  labels remain at unity. Snapshots and durable events always carry one complete
  strip for every show input; partial audio-strip patches are not accepted.

Protocol 2.22 adds session undo/redo and the edit padlock:

- `payload=undo` and `payload=redo` take an optional `expected_edit` (the
  nonzero revision of the edit the operator expects to revert or reapply;
  canonical decimal, `0` is rejected). A mismatch is refused with
  `history_conflict` instead of reverting a concurrent editor's newer edit.
  Undoable edits are input rename/reorder, stinger slot configure/remove, and
  overlay transition/appearance/output-inclusion configuration; live switching,
  audio, streams, recording, and the padlock itself are not recorded. History
  holds at most 100 edits, is per engine session, and is empty after restart,
  restore, or project replacement. An accepted undo/redo publishes the ordinary
  durable event of the edit it resolved to (`input_renamed`,
  `stinger_slots_changed`, ...) and realizes in that edit's domain. The journal
  records the resolved edit, not the undo request, so recovery reproduces the
  state without session history.
- `payload=edit_lock` carries `scope=project|input` (`input=` only with
  `scope=input`) and `locked=0|1`. It publishes the durable
  `event=edit_lock_changed` with the same fields and realizes in `project`.
  A locked project refuses every undoable edit and undo/redo; a locked input
  refuses its rename and any reorder that moves it. Live switching (cut, transitions, T-bar, FTB,
  stingers, overlay take/off), audio, streams, and recording are never locked.
- Refusals use top-level `conflict` or `not_found` with a stable first field
  issue code: `project_locked`, `input_locked` (field `edit_lock`),
  `nothing_to_undo`, `nothing_to_redo`, `history_conflict`, or
  `history_drift` (field `history`). Clients key UI messages on the field code.
- Every durable `event` record ends with the post-revision history summary
  `undo_depth`, optional `undo_edit`, `redo_depth`, optional `redo_edit`
  (depth `0..=100`; an edit revision is present exactly when its depth is
  nonzero, lies in `1..=revision`, and undo/redo edits differ). Snapshots carry
  the same summary plus `project_locked=0|1` and `locked_inputs` (a strictly
  ascending, possibly empty, comma list of snapshot input IDs). Command IDs are
  not exposed. The padlock is project state: `project.json` (schema 26)
  carries `project.edit_locks` as `{"project": bool, "inputs": [...]}` with
  the locked input IDs strictly ascending, nonzero, and naming existing
  inputs. The daemon folds every accepted `edit_lock` into the stored project
  and seeds it into the engine on restore, so journal replay and checkpoint
  restore reach the same lock state and a restart keeps refusing locked edits.
  Removing an input, offline or live (Protocol 2.23), drops its lock.

Protocol 2.23 adds live input add and remove:

- `payload=input_add` carries `input=<id> name=<name> kind=solid
  color=<rrggbbaa>` or `input=<id> name=<name> kind=bars`, in that field
  order. The client chooses the canonical nonzero input ID; the name must be
  non-blank and at most 128 bytes; `color` is exactly eight lowercase hex
  digits and is present exactly with `kind=solid`. Only simulated solid and
  bars sources, with silent audio, can be added live, because those are what
  the simulated daemon realizes today; every other `kind` (media, device,
  network, scene, alias, sine audio, ...) is rejected by the codec and must
  be added offline (`freemix-cli input-add`, `media-input-add`, ...). The
  input is appended to the end of the input order with a default Master
  strip. The durable `event=input_added` carries `input` and `name` (not the
  kind) and realizes in `project`; the new input then renders in the
  simulated monitor taps once it is routed and its strip is present in the
  next snapshot.
- `payload=input_remove` carries `input=<id>` and publishes
  `event=input_removed` with the same field, realizing in `project`. The
  input's strip and input lock are dropped and a settled-off overlay that
  still names it as its source is released.
- Both are authorized as project edits (`EditProject`: Graphics, Admin) and
  obey the padlock: a locked project refuses both, and a locked input refuses
  its own removal.
- Refusals use the stable first field issue codes `duplicate_input`,
  `too_many_inputs` (at most 256 inputs), `unknown_input`, `input_in_use`
  (field `input`), and `invalid_name`, `duplicate_name` (field `name`).
  `input_in_use` (top-level `conflict`) refuses removing an input that is on
  desired or realized Program or Preview, is the target of an in-flight or
  manual transition, is the source of an on-air or transitioning overlay or
  queued on one, or is a Stinger slot's media. The daemon additionally
  refuses, as `conflict` with the reason in the message, removing an input
  the persisted project still references as a Scene layer source, a Scene
  input's audio source, or an alias input's target.
- History: an accepted `input_add` is undoable (its inverse is the removal of
  the same input; undo is refused with `history_drift` once the input is no
  longer the last in the order or was renamed, and with `input_in_use` while
  it is on air). `input_remove` is not undoable: its inverse would have to
  restore the input's full persisted kind and capabilities, which the engine
  does not hold. An accepted removal therefore clears the redo stack and
  forgets every undo entry naming the removed input.
- Durability: the daemon folds both into the stored project before journaling
  them; an addition persists as a simulated solid or bars input with silent
  audio and no required capabilities, exactly as the offline CLI commands do.
  Journal replay and checkpoint restore reach the same catalogue.
- A native-media daemon refuses both with `unavailable` (not retryable): the
  native runtime builds its source graph at startup and cannot add or retire
  one while running, so the catalogue may only change offline there.

Protocol 2.24 adds live title field state:

- Field lists use a counted encoding inside the record's ordinary percent
  escaping, so arbitrary text never needs a second escaping layer:
  `fields=<item>,<item>,...` with items in strictly ascending nonzero field
  ID order, each one of `<id>:t:<byte-len>:<text>`, `<id>:n:<i64>`,
  `<id>:c:<rrggbbaa>` (eight lowercase hex digits, straight alpha) and
  `<id>:i:<byte-len>:<asset-uri>` (non-blank). Numbers and IDs are
  canonical decimal. A list holds at most 128 fields, each text at most 4096
  characters, and all text plus image URIs together at most 12288 UTF-8
  bytes; anything else, including trailing data or an empty item, is
  rejected by the codec.
- `payload=title_fields` carries `input`, `base_revision`,
  `require=off_air|on_air` and `fields` (the changed fields). It is a
  project edit (`EditProject`: Graphics, Admin), realizes in `project`, and
  publishes `event=title_changed`.
- `payload=title_take` carries `input`, `channel=1..=8`, `base_revision` and
  `fields` (possibly empty). It applies the fields to an off-air title and
  takes it onto the overlay channel with that channel's configured
  transition, as one revision. It is a transition (Operator, Admin); with a
  non-empty field list it additionally requires `EditProject`, so in the
  production role matrix only Admin may edit fields while taking. It realizes
  in `switcher` and publishes `event=title_changed`.
- `payload=title_off_air` carries `input` and takes the title off the single
  desired overlay channel that shows it. It is a transition, realizes in
  `switcher`, and publishes `event=desired_switcher` like `overlay_off`.
- `event=title_changed` carries `input`, `field_revision`, the title's
  complete `fields` list (every template field, ascending) and the full
  desired `overlays` (same encoding as `desired_switcher`).
- Refusals use the top-level codes `conflict` (stale `base_revision`, on or
  off air against `require`, on the Program bus, on several overlay
  channels), `not_found` (unknown or non-title input), `invalid_command`
  (unknown, duplicate or mistyped field, blank image URI, text or content
  bounds) and `resource_exhausted` (field revision exhausted).
- Snapshots end with `titles=<input>:<field_revision>,...`, ascending
  snapshot input IDs, possibly empty. A snapshot only announces titles: the
  server follows every `snapshot` record immediately, before any event,
  with exactly one `kind=title_state` record per announced title in that
  order, carrying `revision` (the snapshot revision), `input`, `name` (the
  template name, non-blank, at most 256 bytes), `field_revision`, the
  on-air animation `phase=idle|in|hold|out` with `phase_frame` and
  `phase_frames` (Protocol 2.27, below), the complete `fields`, and `field_names=<byte-len>:<name>,...` positionally
  naming each field (non-blank, together at most 4096 bytes). A client stays
  synchronizing until every announced title is complete; a `title_state`
  that does not complete an announced title at the snapshot revision, one
  arriving at any other time, or an event arriving before the titles are
  complete forces a fresh snapshot, and an incomplete client never resumes.
  Resumed event streams carry no title states; `title_changed` keeps
  replicated titles current.
- Durability: the daemon folds every title's field values and revision into
  the stored project before journaling and restores them against each
  title's stored template on start. A template that does not load, whose
  names exceed the `title_state` bounds, or whose persisted values do not
  match it stops the daemon at startup naming the input.
- Clients apply no optimistic change for title commands.

Protocol 2.27 reports where each title's on-air animation is (`GX-011`
stage 3):

- `title_state` carries `phase`, `phase_frame`, and `phase_frames` right
  after `field_revision`, sampled at the engine's frame cursor when the
  record is written. `idle` (not on air through a title command) and `hold`
  (resting pose) carry `0/0`; `in` and `out` carry the engine frame inside
  the phase and the phase length in engine frames, with
  `phase_frame < phase_frames`. Anything else is refused as
  `invalid_field` naming `phase` or `phase_frame`.
- Phase lengths are the template clip's In/Out frames resampled exactly to
  the engine rate (`ceil(frames * clip period / engine period)`); engine
  frame `k` of a phase samples the clip at the exact rational position
  `k * engine period / clip period`.
- `take_title` with a nonzero In phase cuts the overlay in at full opacity
  on the take frame and plays In from frame 0, then holds; a zero In phase
  keeps the channel transition. `title_off_air` with a nonzero Out phase
  plays Out from the off-air frame (abandoning any unfinished In) and
  removes the overlay after exactly `phase_frames`; a zero Out phase keeps
  the channel transition over the Hold pose. A take on the same channel
  during Out restarts In at frame 0 (the pose jumps, it does not blend);
  a take on another channel during Out is refused as on air. The pose is a
  pure function of the frame since the phase start, so dropped frames land
  on the exact pose. A title command's execution spans its whole phase.
- Generic overlay commands and Program cuts show titles in the Hold pose.

Protocol 2.25 adds the frame-driven project playlist (`OR-015` slice):

- Every playlist time is an engine frame: `duration_frames` (1..=604800000),
  `transition_frames`, and the frames in `start` and `state`. Entries are
  `<id>:<input>:<duration frames>:<cut|fade|alpha_fade|wipe|slide|zoom>:<transition frames>:<0|1>`
  (the last item is `enabled`), comma-separated, unique IDs, at most 256. A
  cut has zero transition frames; every other kind 1..=3600, no longer than
  its entry.
- `payload=playlist_set` carries `loop=off|all|one` and `entries` (possibly
  empty). It replaces the list while the playlist is stopped or finished, is
  a project edit (`EditProject`) without an undo entry, and realizes in
  `project`. Entries must name show inputs, and an input a playlist entry
  names cannot be removed.
- `payload=playlist_start start=now|after:<frames>|at:<frame> from=-|<entry>`,
  `playlist_stop`, `playlist_pause`, `playlist_resume`, `playlist_next`,
  `playlist_previous`, and `playlist_jump entry=<id>` are transitions
  (Operator, Admin) taking effect on the command's target frame. An
  operation that puts an entry on air selects its input and takes it to
  Program with the entry's transition on that frame, as the same revision.
- `payload=playlist_advance frame=<frame>` is the authority's playlist clock:
  it processes every boundary up to `frame` and takes the entry then on air
  to Program on exactly that frame. The engine accepts it only for the due
  boundary (or the next frame once that has passed), ahead of time, and
  accepts no other command until it is realized. Clients may not send it:
  `freemixd` refuses it with `unavailable`. The simulated daemon's clock
  anchors the engine frame cursor to wall time once a boundary is pending,
  journals the advance under the key `playlist-clock-<revision>-<frame>` when
  wall time reaches the boundary, and renders the frames up to it and
  through the entry's transition, so journal replay reproduces the exact
  frames. A native-media daemon's clock is its frame pacer: it journals the
  advance under the same key when the boundary is the next frame the pacer
  realizes (or, when frames passed the boundary without a clock pass, on the
  next frame) and realizes that frame and the entry's transition through
  the native renderer, so Program cuts on exactly the boundary frame. The
  native pacer also realizes idle frames between commands, so journal replay
  ticks the engine idle up to the frame each recorded command began at (its
  recorded end frame minus the frames it realizes) before re-executing it.
  Idle frames after the last command are made durable by a progress marker:
  a single overwritten `journal_progress` row in the bundle's journal
  database naming the journal head (sequence and revision) it was written
  above and the full project position reached there. A native daemon writes
  it once per second of realized frames, from the session loop right after a
  paced frame and only at an idle frame boundary, and folds idle progress
  into the manifest (a checkpoint) once per five minutes of frames; neither
  runs on the frame path, and a failed write is reported on stderr
  (`FREEMIXD_JOURNAL progress=failed`) without stopping the show. Recovery
  ticks idle from the last recorded command up to a current marker and
  requires it to land on the marker's whole position, so a SIGKILL loses at
  most one second of idle progress; a marker written above an older head is
  stale and ignored, and `--recover-to-checkpoint` discards it. Replay ticks
  at most 24 hours of idle frames (at the project frame rate) per recovery
  and refuses, before ticking, a journal that asks for more as damaged. The
  clock also runs inside the native recorder startup barrier, so a restored
  playlist cuts on schedule while a configured recorder starts. Frames a
  client transition command (or a Stinger mutation) realizes run inside that
  command's execution, where no other journaled command can be ordered, so a
  boundary that falls inside them is cut on the first frame after the
  command (the entry keeps its scheduled start).
  After a refusal either clock retries only once another command changed the
  revision. Both runtimes accept `playlist_set` and every playlist control.
- Every accepted playlist command publishes `event=playlist_changed` with
  `frame` (its target frame), `loop`, the complete `entries`, `state`
  (`stopped`, `armed:<start frame>:<from|->`,
  `playing:<entry>:<take>:<cycle>:<started frame>`,
  `paused:<entry>:<take>:<cycle>:<elapsed frames>`, or `finished:<last|->`),
  and the desired `program` and `preview`.
- Snapshots carry the playlist from Protocol 2.26 (below).
- Durability: the project (schema 28) persists the playlist with its
  automation state, and a restored daemon resumes it on the same frames.
- Clients apply no optimistic change for playlist commands.

Protocol 2.26 replicates the playlist to late-joining clients:

- Snapshots end with `frame_rate=<numerator>/<denominator>` (both nonzero
  canonical decimals in lowest terms: the engine frame rate that every
  playlist frame counts) and `playlist=0|1`.
- `playlist=1` announces exactly one `kind=playlist_state` record that
  follows the snapshot after its `title_state` records and before any
  event, carrying `revision` (the snapshot revision), `frame` (the engine's
  frame cursor, its next unrendered frame, when the record was written, so a
  client can place the state's frames in time), `loop`, the complete
  `entries`, and `state`, encoded exactly as in `playlist_changed`.
  `playlist=0` means the playlist is empty, stopped, and does not loop; the
  server announces every other playlist.
- The playlist rides its own record rather than the snapshot so it never
  competes with the snapshot for the 64 KiB line budget: the snapshot alone
  already grows with the input catalogue (names, audio strips, titles),
  while a maximal 256-entry playlist with the widest identifiers, durations,
  and transitions encodes to under 27 KiB, so one `playlist_state` always
  fits a record. The existing entry bounds (count, duration, transition
  frames) are enforced by model validation and wire decode; entries carry no
  names.
- A client stays synchronizing until the announced `playlist_state` arrives.
  One at another revision, unannounced, before the announced title states,
  naming an input the snapshot lacks, arriving at any other time, or an
  event arriving before it forces a fresh snapshot, and an incomplete client
  never resumes. Resumed event streams carry no playlist state;
  `playlist_changed` keeps the replicated playlist current.
- 2.25 is refused.

Protocol 2.28 edits and replicates project colour grading (`SW-019`):

- Grade commands, all `EditProject` (the Graphics role, like title field
  saves), undoable through the shared edit history, and padlocked like other
  project edits (an input padlock also covers that input's grade, and so
  refuses `grade_library_update` of a library grade the locked input uses):
  - `grade_input_set` (`input` plus exactly one of `library=<grade id>` or
    `grade=<grade>`) and `grade_input_clear` (`input`). The input must exist.
  - `grade_program_set` (exactly one of `library` or `grade`) and
    `grade_program_clear`. The program grade colours the composed Program
    picture after overlays and before Fade-to-Black.
  - `grade_library_add` (`grade_id`, `name`, `grade`, optional
    `index` below 256, else appended), `grade_library_update` (`grade_id`,
    `name`, `grade`; every target referencing the grade follows), and
    `grade_library_remove` (`grade_id`; refused while an input or the
    program uses it).
  - `grade_lut_import` (`lut_id`, `name`, `data`) and `grade_lut_remove`
    (`lut_id`; refused while any grade references it).
- Ids are chosen by the client, like `input_add`: grade and LUT ids are
  canonical nonzero 32-bit decimals, and an id already in use is refused.
  A LUT id is never reused within a project bundle: removing a LUT keeps
  its bundled file (older manifests and autosaves still name it), and a
  `grade_lut_import` whose id already has a file with different text is
  refused as a non-retryable `conflict` (the same text is accepted).
  Clients allocate above the highest id they have seen, wrapping to the
  lowest free id after `4294967295`. Names are nonblank, at most 128 bytes, free of control characters, and
  unique among library grades (or among LUTs).
- `grade` is seventeen `;`-separated sections,
  `enabled;temperature,tint;exposure;contrast,pivot;saturation;lift;gamma;gain;offset;master;red;green;blue;hue_vs_hue;hue_vs_saturation;luma_vs_saturation;lut`:
  `enabled` is `0|1`; wheels are `x,y,master`; RGB curves are `x:y` points
  joined by `,`; hue curves are empty or `position:value` points joined by
  `,`; `lut` is `-` or `<lut id>:<strength permille>` naming the imported
  LUT `asset://luts/<lut id>.cube`. Numbers are canonical decimals (`-` only
  on nonzero values) and every grade bound is enforced on encode, decode,
  and in the engine; a grade naming a LUT that was not imported is refused.
- LUT import size limit: there is no chunked client-to-server upload, so
  `data` carries the whole `.cube` text in one command field as unpadded
  base64url (canonical trailing bits, valid UTF-8) and is capped at
  **32 KiB of `.cube` text** (about 43 KiB encoded, inside the 48 KiB field
  and 64 KiB line limits). That covers 1D LUTs and 3D LUTs up to roughly
  edge 17. Larger LUTs (up to the 16 MiB project limit) are imported offline
  with `freemix-cli grade-import-lut`. The engine parses and validates the
  text (`fm_grade::parse_cube`); `kind` and `size` come from the parse.
  The daemon writes the exact text to `assets/luts/<lut id>.cube` before
  the command's journal batch, so a durable import always finds its file.
  Removing a LUT is not undoable (a project-loaded LUT may exceed what one
  journal record can carry); undoing an import removes it.
- Every accepted grade command publishes `event=grade_changed` with
  `change=lut_imported` (`lut_id`, `name`, `kind=1d|3d`, `size`),
  `lut_removed` (`lut_id`), `library` (`grade_id`, `name`, `grade`, and its
  library `index`; an add or update), `library_removed` (`grade_id`),
  `input` (`input` and, unless cleared, `library` or `grade`), or `program`
  (`library` or `grade` unless cleared).
- Snapshots end with `grade_states=<count>` (at most 577: 64 LUTs, 256
  library grades, 256 graded inputs, and the program). The count announces
  that many `kind=grade_state` records, each carrying `revision` (the
  snapshot revision) and `entry=lut|library|input|program` with the fields
  of the matching `grade_changed` change (no library `index`: library
  records arrive in library order). They follow the `playlist_state` record
  and precede any event, ordered LUTs (import order), library grades,
  graded inputs (ascending id), then the program grade.
- A client stays synchronizing until every announced `grade_state` has
  arrived. A record at another revision, beyond the count, out of order,
  naming an unknown input, library grade, or LUT, arriving before the title
  and playlist states, or at any other time forces a fresh snapshot.
  Resumed event streams carry no grade states; `grade_changed` keeps the
  replicated grading current. Clients apply no optimistic change for grade
  commands.
- Durability: grades are project state (schema 31); the daemon renders
  input grades on each input wherever it is used and the program grade on
  each Program output, in both the simulated monitor path and the native
  GPU path, and restores them on restart.
- 2.27 is refused.

Protocol 2.29 edits, replicates, and previews title animations (`GX-011`):

- Animation commands, all `EditProject` (the Graphics role, like title
  field saves and title template edits), undoable through the shared edit
  history, and padlocked like other project edits. A title template is
  shared by every title input that loads the same `template_uri`, so an edit
  through one input changes them all and an input padlock on any of them
  refuses it. Each carries `input` (a title input with a loaded template)
  and `base_revision`, the template's animation revision the edit was made
  against; a mismatch is refused with `code=conflict` and field code
  `revision_conflict` (optimistic concurrency, nothing applied):
  - `title_animation_set` (`clip`): replaces the whole clip.
  - `title_animation_preset` (`element`, `phase=in|out`, `preset`,
    `frames`): `fm_titles` `apply_animation_preset`. `preset` is an
    `fm_animation::AnimationPreset` id (`fade`, `slide-from-left`,
    `slide-from-right`, `slide-from-top`, `slide-from-bottom`, `pop`,
    `wipe`, `drop`).
  - `title_animation_stretch` (`phase`, `frames`): `stretch_animation`.
  `frames` is at most `fm_animation::MAX_PHASE_FRAMES`. Every applied edit,
  undo and redo included, advances the animation revision by exactly one;
  undo and redo restore the whole previous clip (the journal records them as
  revision-checked `title_animation_set` commands). An edit (undo and redo
  included) whose resulting clip would not fit `MAX_TITLE_ANIMATION_BYTES`
  is refused with `code=invalid_command` and field code `too_large`, so
  every `title_animation_changed` and `title_animation_state` stays
  encodable; a daemon refuses to start on a stored template beyond it.
- `clip` is one unpadded base64url field of a compact binary encoding, at
  most **36 KiB raw** (`MAX_TITLE_ANIMATION_BYTES`, 48 KiB encoded, the field
  limit). Integers are minimal LEB128 varints, signed values zigzag-mapped:
  `clip = rate_num rate_den in_frames out_frames track_count track*`;
  `track = element(nonzero) phase(byte 0 in, 1 out) property(byte, index in
  fm_animation::Property::ALL) key_count key*`; `key = frame value easing`
  with `value` four `r g b a` bytes for colour and a zigzag varint
  otherwise, and `easing` a tag in `fm_animation::Easing` declaration order
  (`8` is a cubic bezier followed by `x1 y1 x2 y2` in Q16, `y` zigzag).
  Tracks are strictly ascending by (element, phase, property) and keys by
  frame, so each clip has exactly one encoding; decoding enforces every
  `fm_animation` bound (`MAX_CLIP_TRACKS`, `MAX_TRACK_KEYS`, `MAX_CLIP_KEYS`,
  `MAX_PHASE_FRAMES`, property ranges) and a lowest-terms frame rate.
- Every accepted animation command publishes
  `event=title_animation_changed` with `inputs` (every title input sharing
  the template, ascending), `animation_revision`, and the resulting `clip`.
- After the `grade_state` records, a snapshot sends one
  `kind=title_animation_state` record per announced title, in the order of
  the snapshot's `titles` list (no new count: the title list is the count),
  each carrying `revision` (the snapshot revision), `input`,
  `animation_revision`, and `clip`. A title without a loaded template
  reports the empty clip at animation revision 0. A record at another
  revision, for an unannounced input, out of order, twice, or before the
  grade states forces a fresh snapshot; clients stay synchronizing until
  every title's animation has arrived. Clients apply no optimistic change.
- Exact preview: `kind=title_preview_request` (`protocol`, `request_id`,
  `input`, `phase=static|in|hold|out`, `frame` (0 for `static` and `hold`,
  else at most the phase length), `width` 1..=640, `height` 1..=360, and an
  optional unsaved draft `clip` rendered in place of the template's
  animation) needs `EditProject` and is rate limited per session (default 10
  per second, `title_previews`); over the limit it is refused with
  `resource_exhausted`. The server renders the input with its persisted
  field values through `fm_titles` `render_sampled` at the template size,
  nearest-neighbour scaled, straight alpha. The answer is one four-channel
  QOI stream of straight RGBA8 split into `kind=title_preview_frame`
  records (server identity, `request_id`, `input`, `animation_revision`,
  `draft=0|1`, `width`, `height`, `part`, `parts`, base64url `data`), each
  part at most **32 KiB** of QOI, at most **36 parts** (a 640x360 worst
  case). The parts of one preview are written back to back with nothing
  interleaved, through a latest-wins lossy slot: a newer preview replaces
  one not yet started, so clients key previews by `request_id`. Refusals
  (`permission_denied`, `not_found`, `invalid_request` for a frame past the
  phase or a draft the template refuses, `render_failed`) are `kind=error`
  records carrying the `request_id`. Only raw TCP sessions receive previews;
  WebSocket sessions are refused with `unsupported`.
- Durability: the animation lives in the template file
  (`assets/titles/...`). Before an edit's journal batch the daemon stages
  the edited template beside it as `<template>.staged-<revision>`; each
  journal checkpoint and startup recovery commit the newest staged copy at
  or below the durable revision over the template and remove older ones,
  and recovery discards copies past the recovered revision. A failed
  append discards its staged copy. Simulated and native daemons both
  render the edited animation on air and restore it on restart.
- 2.28 is refused.

Protocol 2.30 replicates each title template's element layout (`GX-011`), so
a remote animation editor can list, outline, and apply presets to every
element, animated or not:

- After the `title_animation_state` records, a snapshot sends one
  `kind=title_layout_state` record per announced title, in the order of the
  snapshot's `titles` list (no new count), each carrying `revision` (the
  snapshot revision), `input`, the template canvas `width` and `height`,
  and `elements`. A title without a loaded template reports the empty
  layout: `width=0`, `height=0`, and no elements; otherwise both sides are
  in `1..=8192` (`MAX_TITLE_CANVAS_SIDE`). A record at another revision,
  for an unannounced input, twice, or before the animation states forces a
  fresh snapshot; clients stay synchronizing until every title's layout has
  arrived. No command edits a template's layout, so it changes only with a
  new snapshot; there is no layout event.
- `elements` is one unpadded base64url field of a compact binary encoding
  (the clip codec's minimal LEB128 varints, signed values zigzag-mapped),
  at most **36 KiB** raw (`MAX_TITLE_LAYOUT_BYTES`): `elements = count
  element*`; `element = id(nonzero) kind(byte: 0 text, 1 rectangle, 2
  image, 3 clock, 4 production_clock, 5 ticker) flags(byte: bit 0 visible,
  other bits zero) name_len name(UTF-8) x(zigzag) y(zigzag) width height
  opacity(byte) r g b a(bytes)`. Elements keep template order (so each
  layout has exactly one encoding), at most **192**
  (`MAX_TITLE_LAYOUT_ELEMENTS`) with unique ids; names are at most **96**
  UTF-8 bytes (`MAX_TITLE_ELEMENT_NAME_BYTES`) without control characters
  and may be empty. Bounds, opacity, and fill are the element's static
  design values (`style.fill`, `style.opacity`) in template canvas pixels;
  templates carry no static rotation. The worst-case layout fits one line.
- The server derives each layout from the `fm_titles` template
  (`fm_control::title_layout_to_wire`): element names have control
  characters replaced by spaces and are cut at a character boundary, and a
  template with more elements lists its first 192 in template order.
- 2.29 is refused.

Protocol 2.31 animates scene layers with the title animation model
(`GX-011` stage 5, project schema 32): every scene carries one
`fm_animation` clip keyed by stable layer id (`fm_model::LayerId`, unique
within its scene and kept by every layer edit), with the same In and Out
phases, presets, and animation revision as a title template:

- The snapshot gains `scenes` after `titles`: the Scene inputs, strictly
  ascending, each also in `inputs`, at most `MAX_LIST_ITEMS`, comma-joined
  (empty when there are none).
- Commands (each needs `EditProject`, is a project edit with undo and redo,
  and is refused as a conflict unless the scene's animation revision is
  `base_revision`; `input` is any Scene input showing the scene):
  - `scene_animation_set` (`input`, `base_revision`, `clip`): replaces the
    whole clip.
  - `scene_animation_preset` (`layer`, `phase=in|out`, `preset`, `frames`):
    `fm_model::Scene::apply_animation_preset` with the title presets.
    Off-canvas distances and the wipe extent come from the layer's static
    on-screen footprint and the project canvas.
  - `scene_animation_stretch` (`phase`, `frames`): scales every key of the
    phase.
  An edit whose clip names a layer the scene lacks or animates `color`
  (layers have no fill) is refused with `invalid_command`; one whose clip
  would not fit `MAX_SCENE_ANIMATION_BYTES` is refused with field code
  `too_large`, and a daemon refuses to start on a stored scene beyond it.
  Undo and redo restore the whole previous clip. The animation lives in
  `project.json` and is journaled like every project edit.
- `clip` uses the title clip encoding with `layer` (a nonzero varint, at
  most `u32::MAX`) in place of `element`, at most **36 KiB** raw
  (`MAX_SCENE_ANIMATION_BYTES`); decoding refuses any `color` track.
- Every accepted scene animation command publishes
  `event=scene_animation_changed` with `inputs` (every Scene input showing
  the scene, ascending), `animation_revision`, and the resulting `clip`.
- After the `title_layout_state` records, a snapshot sends one
  `kind=scene_animation_state` record per announced scene input (in the
  order of `scenes`; `revision`, `input`, `animation_revision`, `clip`),
  then one `kind=scene_layout_state` per announced scene input (`revision`,
  `input`, the project canvas `width` and `height`, and `layers`). A record
  at another revision, for an unannounced input, twice, or out of order
  forces a fresh snapshot; clients stay synchronizing until every scene's
  animation and layout has arrived. Scene layers are edited offline only,
  so a layout changes only with a new snapshot; there is no layout event.
- `layers` is one unpadded base64url field (the clip codec's varints), at
  most **36 KiB** raw (`MAX_SCENE_LAYOUT_BYTES`): `layers = count layer*`;
  `layer = id(nonzero u32) flags(byte: bit 0 enabled, other bits zero)
  quarter_turns(byte 0..=3) name_len name(UTF-8) x(zigzag) y(zigzag) width
  height opacity(byte)`. Layers keep scene order, at most **256**
  (`MAX_SCENE_LAYOUT_LAYERS`) with unique ids; names are at most **96**
  UTF-8 bytes without control characters (the server replaces control
  characters by spaces and cuts at a character boundary). `x`/`y` are the
  top-left of the layer's on-screen footprint and `width`/`height` its box
  before the quarter turns, in project canvas pixels.
- Posing (`fm_model::Scene::posed`, integer arithmetic rounded once, half
  up, identical in every renderer): position moves the footprint; scale
  scales the box about the footprint's centre; rotation (millidegrees,
  positive clockwise) is added to the static quarter turn and realized at
  **any angle**: the nearest quarter turn (half up) is the compositor
  transform and the remainder, within ±45°, is a residual tilt the
  compositor draws about the layer's centre with exact Q30 trigonometry and
  bilinear, premultiplied sampling (CPU reference and native GPU agree
  within 2/255 per channel); crop hides pixels of the scaled box with the
  source cropped in proportion, and a tilted cropped box turns about its
  uncropped footprint's centre; opacity multiplies the static opacity. A
  layer scaled or cropped to nothing is not drawn. The tilt exists only on
  posed scenes: a stored layer's rotation stays whole quarter turns.
- On air the engine plays a scene's In phase when its Scene input reaches
  Program (a cut or the first frame of a transition) or Preview, holds the
  In end afterwards, and plays the Out phase while a timed or manual
  transition takes it off Program. **A cut plays no Out**: there is no
  frame to play it on. A scene shown any other way (nested in another
  scene, a multiview tile, an overlay) holds. Phase playback is runtime
  state: it is not persisted, does not block snapshots, and a snapshot
  replicates no phase. The simulated daemon realizes a command's scene
  phases within that command's ticks; the native daemon recompiles the
  scene composition every frame while a scene is mid-phase.
- Editors draw the replicated layout as outlines posed by the draft clip
  (Studio's Animation window) and, from Protocol 2.32, under them the
  server's exact scene preview. `freemix-cli scene-render-frame` renders
  any point of a scene's animation locally.
- 2.30 is refused.

Protocol 2.32 adds exact scene previews (`GX-011`):

- `kind=scene_preview_request` carries exactly a `title_preview_request`'s
  fields (`protocol`, `request_id`, `input`, `phase`, `frame`, `width`
  1..=640, `height` 1..=360) with an optional unsaved draft `clip` in the
  scene clip encoding (`layer` targets, no `color`), composed in place of
  the scene's animation. `input` must be a Scene input. It is not a
  command: it needs `EditProject`, shares the session's preview rate
  (`title_previews`, default 10 per second, title and scene previews
  together) and is refused over it with `resource_exhausted`.
- The server poses the scene the engine holds (with the draft, if any) at
  the sample and composes it with the CPU reference compositor and the same
  project-to-grid mapping its simulated monitor uses, directly at
  `width` x `height`, any rotation angle included. A layer showing a
  nested scene (directly or through a Scene input) draws that scene
  composed on its own canvas in its Hold pose and then used as the layer's
  source, at most 16 nesting levels and 64 distinct nested scenes per
  preview. A simulated daemon draws the project's simulated sources at the
  engine frame cursor. A native daemon draws, for every source, the newest
  CPU frame its media pipeline holds, snapshotted when the request is
  resolved without copying pixels, touching the GPU, or waiting on the
  output clock: the last frame of the newest decoded window of a media
  file (at most the source ring, eight frames by default, ahead of
  Program), the last ingested live frame (titles, cameras), or the retained
  frame (stills, generators); each is scaled nearest-neighbor to the
  preview and converted from its labeled colorimetry to sRGB Rec.709 full
  range, the colorimetry of the native Program readback. The 8-bit CPU
  composition matches the native GPU composition (linear light, 16-bit
  float) within the difference of the two pipelines, not bit for bit, and
  per-input grades are not applied. The answer is one
  four-channel QOI stream of straight RGBA8 split into
  `kind=scene_preview_frame` records with exactly a
  `title_preview_frame`'s fields and limits (`animation_revision` being
  the scene's), written back to back through the same latest-wins lossy
  slot as title previews: a newer title or scene preview replaces one not
  yet started.
- Refusals are `kind=error` records carrying the `request_id`:
  `permission_denied`, `not_found` (not a Scene input), `invalid_request`
  (a frame past the phase or a draft the scene refuses), and
  `render_failed` for a scene the reference compositor cannot compose
  exactly (a layer whose source has no picture on that daemon, such as a
  non-simulated input on a simulated daemon or a source a native daemon
  holds no frame of, nesting past the bounds, or an effect the reference
  rejects) rather than an approximation. Only raw
  TCP sessions receive previews; WebSocket sessions are refused with
  `unsupported`.
- 2.31 is refused.

Command results are ordered before any event bearing the same accepted revision
on that connection. State events are globally ordered within one
`(state_epoch, log_id)`. Runtime realization events reference a durable revision
but use their own per-generation sequence because independent clock domains may
complete at different times.

Declarative state mutations and edge-triggered actions have different recovery
semantics. The server persists idempotency results for at least the project
recovery window and never replays an irreversible action after restart. A client
whose result was lost queries the idempotency key and receives the original
receipt and latest realization outcome.

Diagnostics are a read-only query, separate from the durable command envelope. A
`diagnostics_request` carries only `protocol` and non-blank `request_id`; it is not
a command, does not mutate state, and has no idempotency or revision precondition.
The matching `diagnostics_response` carries request ID, engine identity,
`current_revision`, optional retained revision bounds, and subscriber limits and
counts. Exact fields are `oldest_retained_revision`, `newest_retained_revision`,
`subscriber_count`, `retained_events_limit`, `subscriber_limit`, and
`subscriber_queue_limit`. These fields do not claim readiness, health,
capabilities, authentication state, session counters, paths, or error details.

## 4. Events and telemetry

Durable ordered events include project edits, routing, switcher state, overlay
state, output desired state, shortcut changes, and authorization-relevant
operations.

Ephemeral streams include:

- audio meters;
- clocks and playheads;
- preview statistics/thumbnails;
- input/output health;
- GPU/CPU/disk/network metrics;
- pointer/hover previews and telestrator cursors.

Protocol 2.15 defines `audio_meters` as a lossy, non-resumable record with
server identity, an independent sequence, one native Program frame/sample
interval, fixed-point linear Master levels, and fixed-point per-input levels in
strict `InputId` order. Levels use millionths of linear full scale and can
exceed unity. The codec validates only that the sample interval is positive;
the native timeline owns frame/sample coherence. The record does not advance or
carry a durable revision. Native `freemixd` publishes only completed mixer
intervals to active Studio peers. Each peer retains at most the current partial
record and one latest replacement. Control records have priority unless a meter
record has already started, in which case that newline-delimited record finishes
first. The Master reading is after strip processing and mixer clipping but
before clip-local Stinger audio and Fade-to-Black.

Protocol 2.17 defines `stream_status` with the same lossy, non-resumable
semantics: server identity, an independent strictly increasing sequence, and at
most one sample set per frame interval carrying every active streaming target's
realized state, connectivity, cumulative enqueue/drop counters, and a sanitized
failure code in strict target order. Peers retain only the latest record;
violations of identity or sequence disconnect the session exactly like meter
violations. The record never advances or carries a durable revision, and native
`freemixd` omits it entirely while no stream runtime is active.

Protocol 2.20 makes recording control per slot. `record_start` and `record_stop`
address one of two recorder slots with an explicit `recorder` field, are
authorized as transition operations, and every acceptance emits one durable
`recording_changed { recorder, active }` event carrying the addressed slot and
its resulting flag — including a command that leaves the flag unchanged, so the
revision, event sequence, and runtime generation keep advancing together exactly
like the stream roster events; idempotent replays return the original receipt
without a new event. Snapshots carry the fixed two-entry `recordings` roster
with each slot's `desired_active` flag. Realization is
daemon-side: a configured recorder reconciles at readiness against the
persisted intent, live stops drain the current segment before its final
report, and live starts open deterministically derived segment files.

Protocol 2.19 extends each `stream_status` sample with local HLS output health.
A sample for a local `hls://` destination may carry one trailing
`local=<segments>:<target_duration_ms>:<free_bytes>` field holding the
published playlist's segment count, its target duration in whole milliseconds,
and the free bytes on the output filesystem; `segments` is bounded at 4096 and
`target_duration_ms` at 3,600,000. A sample without local health — every
network target, and a local output whose playlist is currently unreadable —
ends at the `failure` field exactly as protocol 2.18 did, so `local=none` is
never encoded. The local fields are lossy and latest-wins like the rest of the
record: never durable, never resumable, and never projected into snapshots or
durable events.

Protocol 2.21 defines `monitor_frame`, the encoded-fallback Preview/Program
monitor image, with the same lossy, non-resumable semantics as `audio_meters`.
Each record carries server identity, `tap=preview|program`, a per-tap strictly
increasing `sequence`, the realized `frame`, the realized source `input`,
`origin=simulated|native`, `width` (1–640) and `height` (1–360), and one
`part` of `parts` (1–32). The frame payload is one standard QOI stream (three
channels, sRGB colorspace, exact end marker) split into contiguous parts of at
most 32 KiB raw bytes, each carried as canonical unpadded base64url `data`
(padding, the standard `+`/`/` alphabet, and nonzero trailing bits are
rejected). Every part repeats the header fields and shares the frame's
sequence. A frame group is atomic on the wire: once its first part is written,
its remaining parts follow before any other record, and a client treats any
interleaving, a group not starting at part 0, a skipped part, a header change
within a group, a non-increasing sequence, a foreign server identity, or a
completed stream that fails strict QOI decoding as a protocol violation and
disconnects. Clients retain only the latest decoded image per tap and clear it
on disconnect. The 640x360 cap keeps the record a bounded preview consistent
with the thumbnail and loopback encoded-preview fallback rules here; it never
carries full-resolution frames, and the worst-case 640x360 stream fits in 29
parts. As a deviation from the requested-cadence model below, there is not yet
a subscription command: eligibility is implicit, like meters, and covers active
non-Web Studio peers permitted to view status. The record never advances or
carries a durable revision.

What a simulated (`origin=simulated`) monitor frame depicts is fixed without
any wire change. The daemon renders on a dedicated worker thread, never on the
session loop, and publishes nothing for a picture it cannot render honestly,
so a client keeps its placeholder instead of seeing invented video:

- A realized Preview or Program simulated input renders its pattern, and a
  Scene input whose enabled layers all name simulated inputs or nested
  scenes that do (at most 16 levels, each nested scene composed on its own
  canvas in its Hold pose) is composed with the CPU reference compositor at
  the monitor size.
- A realized Preview or Program title input renders its persisted field
  values: the stored template is instantiated, the input's field values are
  applied, the frame is rendered at the template size by the `fm_titles`
  reference renderer, scaled nearest-neighbor onto the fitted monitor size,
  and shown over black. Templates and their fonts load once at daemon
  startup; a template that does not load renders nothing, and a family whose
  font does not load draws no text. Image assets load on demand when a render
  first needs them, including an image URI first set at runtime: at most
  32 MiB encoded and the `fm_titles` decoded-image bound, PNG or JPEG decoded
  by `fm-codec-image`, at most 64 kept per template. Only `native-media`
  builds link that decoder; any other build, and any image that does not
  resolve, read, or decode within those bounds, draws the renderer's image
  placeholder. Renders are cached per title input, template, field revision, and field
  values; the frame cursor joins the key only for templates with clock,
  ticker, or animated elements. Title scene time is the frame cursor times
  the project frame duration, an approximation: the simulated daemon has no
  title playback state, so every title is treated as starting at frame zero.
- The Program tap then composites every realized active overlay (in channel
  order, highest channel on top) over the Program picture with its realized
  opacity, position preset, and border preset, using the native overlay
  geometry evaluated at the monitor size. An overlay's source may be a
  simulated, Scene, or title input (a title keeps its template alpha); any
  other source is skipped. Overlay output inclusion is deliberately ignored:
  overlays include no outputs by default, and the Program tap shows the
  program bus rather than any one output, so it shows every active overlay.
  Preview never shows overlays.

Native daemons (`--native-media`) realize every title input as a live
source frame at the project video size: the template is rendered as above
(field values, on-demand images, nearest-neighbor scale), converted from
premultiplied to straight-alpha sRGB RGBA8, and held as that input's latest
frame, so a title's transparency survives on Program (over black), in Scene
layers, and as an overlay source on the project outputs that include it.
Each title's first frame renders at startup, before readiness; a title whose
template does not load or whose first frame does not render refuses
startup. Afterwards rendering never runs on the session loop: one title
worker re-renders a title when its persisted template, field revision, or
field values change, and re-renders clock, ticker, or animated templates at
most once per frame interval and never more often than every 40 ms (25 Hz),
always at the latest published frame cursor (the same frame-zero scene time
approximation as above). Handoff is latest-wins in both directions, so a
slow render lowers the refresh rate instead of queueing. A failed re-render
keeps the previous frame on air and is reported once on stderr. A title
cannot be preloaded as Stinger media, because it is a live source. Native
monitor frames show titles exactly as Program realizes them.

Each telemetry class has requested cadence, aggregation, and maximum bandwidth.
Slow clients receive coalesced latest data, never unbounded queues.

## 5. Versioning

- Protocol uses one explicit current-development major/minor identifier.
- Any wire change may replace that identifier and contract in place.
- Client and server accept only the exact current identifier; there is no
  downgrade negotiation or upgrade window during current development.
- Unknown fields, events, and message kinds are rejected.
- Tests generate current records through the current encoder, round-trip them,
  and mutate them to cover malformed input. No historical wire fixtures or
  cross-version matrix is retained.
- Domain models do not derive their wire schema directly.

## 6. Authentication and authorization

Local first run uses a one-time pairing secret exchanged through protected local
IPC. Remote pairing displays a short-lived code on an already trusted client or
engine console. Production deployments may use local accounts, OIDC, or mTLS.

Suggested roles:

- `viewer`: health, tally, permitted previews;
- `graphics`: title/data changes and assigned overlays;
- `audio`: mixer/bus controls;
- `replay`: replay record/events/playback;
- `operator`: switcher and show controls;
- `admin`: projects, devices, users, secrets, plugins, updates.

Undo, redo, the edit padlock, live input add/remove, and title field saves
require `edit_project`; title takes and off-airs require `transition`, and a
take that also edits fields requires both. Undo and redo
additionally require the class of the edit they target (for example
`transition` for a stinger slot edit), so history never escalates a role. The
padlock is an operator guard against accidental edits, not access control:
anyone who may edit may engage or release it, and roles remain the
access-control mechanism.

Tokens are short-lived and scoped. Streaming keys, TURN credentials, and vendor
tokens are returned only as redacted references.

## 7. Preview protocol

Remote previews use WebRTC simulcast/SVC where available:

- multiview-first to limit decoder count;
- optional individual input subscriptions for focused inspection;
- operator-configured maximum latency/quality;
- labels and tally may be composited server-side or sent as metadata;
- preview congestion never backpressures Program.

Local studio uses shared textures only after same-user, engine-generation, GPU
adapter, and handle-duplication validation. Each frame lease transfers a ready
fence, sequence and resize generation and requires release acknowledgement; the
engine reclaims outstanding leases on client death. It falls back to a loopback
low-latency encoded preview on any incompatibility.

## 8. Compatibility API

An optional `fm-vmix-compat` service maps the documented vMix HTTP function and
persistent TCP/tally model to FreeMix commands/events. It:

- resolves input ordinals/names to stable IDs at command time;
- exposes the complete documented vMix 29 XML state and HTTP function surface;
- maps tally/activator subscriptions;
- never weakens authentication by default; and
- passes exact-current contract coverage for the persistent TCP command/response
  and subscription semantics.

This adapter is isolated from the native protocol and is not the basis of new
features. FreeMix-only capabilities use a separate namespace and do not alter
the compatibility surface.

## 9. SDKs

Generate supported clients from the transport schema, then wrap them with
handwritten domain-friendly APIs:

- Rust client for studio, CLI, plugins, and integrators;
- TypeScript client for browser integrations even though the shipped web app is
  Rust/Wasm;
- small C ABI for hardware controllers;
- OpenAPI and message-schema documentation.

All examples connect through the public API. There is no privileged internal
mutation path for the studio UI.
