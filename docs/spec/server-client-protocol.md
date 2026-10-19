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

The current Protocol 2.59 implementation uses bounded newline-delimited raw TCP.
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
  - Every set and clear (input, scene layer, and program) may carry
    `gesture` and `continues` (Protocol 2.56): a drag's live sets fold into
    one undo step per target.
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
  **32 KiB of `.cube` text** (about 43 KiB encoded, inside the 56 KiB field
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
  most **36 KiB raw** (`MAX_TITLE_ANIMATION_BYTES`, 48 KiB encoded, inside the
  56 KiB field limit). Integers are minimal LEB128 varints, signed values zigzag-mapped:
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
  order of `scenes`; `revision`, `input`, `animation_revision`, `clip`, and
  since 2.35 the `scene` the input shows), then one
  `kind=scene_layout_state` per project scene (since 2.35: in the order of
  the snapshot's `layout_scenes`; `revision`, `scene`, `name`, the project
  canvas `width` and `height`, and `layers`). A record at another
  revision, for an unannounced input or scene, twice, or out of order
  forces a fresh snapshot; clients stay synchronizing until every scene's
  animation and layout has arrived. Scene layers are edited offline only,
  so a layout changes only with a new snapshot; there is no layout event.
- `layers` is one unpadded base64url field (the clip codec's varints), at
  most **36 KiB** raw (`MAX_SCENE_LAYOUT_BYTES`): `layers = count layer*`;
  `layer = id(nonzero u32) flags(byte: bit 0 enabled, other bits zero)
  quarter_turns(byte 0..=3) name_len name(UTF-8) source x(zigzag)
  y(zigzag) width height opacity(byte)`, `source = kind(byte: 0 input, 1
  scene) id(nonzero u128 varint)` (since 2.35). Layers keep scene order, at
  most **256** (`MAX_SCENE_LAYOUT_LAYERS`) with unique ids; names are at
  most **80** UTF-8 bytes (since 2.35) without control characters (the server replaces control
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
  float) within the difference of the two pipelines, not bit for bit. Since
  Protocol 2.33 both daemons grade the preview as their monitors do: each
  leaf layer's input grade then its scene layer grade, and a layer drawing
  a nested scene only its layer grade over the nested scene's graded
  layers; since 2.35 a nested scene drawn through a Scene input also
  carries that input's grade, and the whole preview carries the grade of
  the requested Scene input (see Protocol 2.35). The answer is one
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

Protocol 2.33 grades scene layers (`SW-019`; Protocol 2.35 replaced the
`input` and `inputs` fields below by `scene`):

- `grade_layer_set` (`input`, `layer`, plus exactly one of `library` or
  `grade`) and `grade_layer_clear` (`input`, `layer`) set or clear the grade
  of layer `layer` (a canonical nonzero 32-bit decimal) of the scene Scene
  input `input` shows. Like scene animation edits they address the scene
  through any Scene input showing it, so every such input shows the change;
  they are `EditProject`, undoable, and refused while the project or any
  Scene input showing the scene is padlocked (`grade_library_update` of a
  library grade a layer uses is likewise refused while one of those inputs
  is padlocked). An input that is not a Scene input is `not_found`
  (`unknown_scene`), as is a layer the scene lacks (`unknown_layer`). The
  layer grade colours that layer after its source's input grade and before
  the layer transform, on the native and simulated monitor paths.
- `event=grade_changed` gains `change=layer` (`inputs`, the ascending
  nonempty list of Scene inputs showing the scene, `layer`, and `library` or
  `grade` unless cleared).
- `grade_state` gains `entry=layer` with the same fields. Layer records
  follow the input records, ordered by scene and then layer id, and are sent
  only for scenes some Scene input shows; at most 256 layer grades, so
  `grade_states` is at most 833 (64 LUTs, 256 library grades, 256 inputs,
  256 layers, and the program). A layer record naming an input the
  snapshot lacks, repeating a layer of one input, or arriving after the
  program record forces a fresh snapshot.
- Durability: layer grades are project state (schema 33, `layer_grades`).
- 2.32 is refused.

Protocol 2.34 adds the project animation preset library (`GX-011`):

- A preset is one phase of one target's animation relative to its resting
  pose: `preset_id` (nonzero 32-bit), `name` (1 to 128 UTF-8 bytes, not
  blank, no control characters, unique in the library), `phase` (`in` or
  `out`, the phase it was saved from), and `motion`, one base64url field
  holding the reference box (`width`, `height` as varints, both nonzero)
  followed by the title clip encoding with the single target `1`, tracks in
  the In phase only and an empty Out phase (at most 1 024 keys and 36 KiB).
  A library holds at most 64 presets.
- `animation_preset_add` (`preset_id`, `name`, `phase`, `motion`, optional
  `index` below 64, the end when absent), `animation_preset_rename`
  (`preset_id`, `name`), and `animation_preset_remove` (`preset_id`) are
  `EditProject` project edits on the shared undo history, refused while the
  project is padlocked. A taken id or name, or a full library, is a
  `conflict` (`duplicate_preset`, `duplicate_name`, `too_many_presets`); an
  unknown preset is `not_found` (`unknown_preset`); an index past the end
  is `invalid_command` (`out_of_range`). They carry no base revision.
- `title_animation_user_preset` (`input`, `base_revision`, `element`,
  `phase`, `preset_id`, `frames`) and `scene_animation_user_preset`
  (`input`, `base_revision`, `layer`, `phase`, `preset_id`, `frames`) apply
  a library preset like `title_animation_preset` and
  `scene_animation_preset` apply a built-in one: the preset's tracks replace
  the target's tracks of the same properties in `phase`, retimed
  proportionally to `frames` (the phase grows to fit), positions kept in
  pixels and crop keys scaled from the reference box to the target's box;
  a scene layer skips colour tracks. They are revision-checked like the
  other animation edits and publish `title_animation_changed` or
  `scene_animation_changed`.
- `event=animation_preset_changed` carries `change=set` (`index`, then the
  whole preset: an addition at `index`, or a rename that keeps its index)
  or `change=removed` (`preset_id`). Undo and redo publish the same event.
- The snapshot gains `animation_preset_states` (at most 64, encoded last),
  the count of `animation_preset_state` records (`revision` and the whole
  preset, in library order) that follow after every other follow-on record
  (after the last `scene_layout_state`) and before any event. A record at
  another revision, beyond the count, before the earlier records, or
  repeating an id or name forces a fresh snapshot.
- Durability: the library is project state (schema 34,
  `animation_presets`).
- 2.33 is refused.

Protocol 2.35 addresses scene layers by scene and grades Scene inputs over
their composed scene (`SW-019`):

- `grade_layer_set` (`scene`, `layer`, plus exactly one of `library` or
  `grade`) and `grade_layer_clear` (`scene`, `layer`) name the project
  scene (a canonical nonzero decimal) directly, whether or not a Scene
  input shows it. An unknown scene is `not_found` (`unknown_scene`), an
  unknown layer `not_found` (`unknown_layer`). They are refused while the
  project or any Scene input showing the scene is padlocked; a scene no
  Scene input shows answers to the project padlock only. Undo and redo
  restore the previous layer grade by scene.
- `event=grade_changed` `change=layer` and `grade_state` `entry=layer`
  carry `scene` in place of `inputs`. Layer records follow the input
  records ordered by scene then layer id, for every project scene; a
  record repeating or going back in that order, or arriving after the
  program record, forces a fresh snapshot.
- The snapshot gains `layout_scenes` (after `scenes`): every project
  scene, strictly ascending, at most 256 (possibly empty). One
  `scene_layout_state` per listed scene follows the last
  `scene_animation_state`, keyed by `scene` with the scene's `name` (at
  most **128** UTF-8 bytes, no control characters; the server replaces
  control characters by spaces and cuts at a character boundary), and each
  layer outline gains its `source` (the input or the nested scene it
  draws). `scene_animation_state` gains `scene`, the scene its input
  shows, so a client joins Scene inputs to their scene's layout. Layer
  names are now at most 80 bytes so a 256-layer layout stays within 36 KiB.
- A Scene input's own input grade (`grade_set` on a Scene input) colours
  the scene picture that input shows: after every layer's grade and the
  composition, and before the picture is used as Program, Preview, an
  overlay, a multiview tile, or a layer of another scene (a layer drawing
  the Scene input carries its grade, then its own layer grade). A layer
  drawing the scene directly (`source` scene) carries no Scene input
  grade. The simulated monitors, exact scene previews (the requested
  Scene input's grade over the whole preview), the native GPU composition,
  and `freemix-cli render` apply it the same way.
- 2.34 is refused.

Protocol 2.36 completes the transition family and adds favourite transition
buttons (`SW-002`, `SW-005`):

- `transition` (`kind`, `duration_frames`) takes any automatic mix by kind:
  `fade`, `alpha_fade`, `wipe`, `vertical_wipe`, `slide`, `vertical_slide`,
  `zoom`, `fly`, `fly_rotate`, `cube`, `cube_zoom`, `cross_zoom`, or `merge`.
  It is a transition-class live operation with the same duration bounds,
  in-flight refusal, and desired-state Cut projection as `fade`; a Stinger is
  still taken with `stinger`, and the dedicated `fade`, `alpha_fade`,
  `wipe`, `slide`, and `zoom` commands remain. The manual T-bar kinds and
  playlist entry kinds are unchanged.
- `overlay_transition` and the replicated `overlays` carry the same
  geometric tokens besides `cut` and `fade` for an overlay channel. A motion
  kind keeps the channel at opacity 255 and moves the layer in from
  transparency on take, and out to transparency on off, over the channel's
  duration; `overlay_take`, `overlay_off`, and `overlay_next` realize after
  that duration.
- `transition_favorite_set` (`slot` 1 through 4 with no leading zero,
  `favorite`) assigns one favourite button. `favorite` is
  `<effect>:<duration frames>`: `effect` is `cut`, a mix token, or
  `stinger_<slot 1..8>`, and the duration is a canonical decimal from 1
  through 3600. It is a transition-class project edit: refused while the
  project is padlocked, undoable, and realized on the `project` domain.
- `event=transition_favorite_changed` (`slot`, `favorite`) announces an
  assignment and its undo or redo.
- The snapshot gains `transition_favorites` (encoded last): exactly four
  `favorite` values joined by `;`, slot 1 first.
- Durability: the favourites are project state (schema 35,
  `transition_favorites`); a new project starts with Fade, Wipe, Fly, and
  Cube at 30 frames.
- 2.35 is refused.

Protocol 2.37 plays media-file inputs (`IN-003`: video files and growing
files with seek, pause, resume, loop, trim, speed change, end triggers, audio
track selection, and frame-accurate position):

- Commands, each naming its media `input`:
  - transport (operator class, realized on the `media` domain):
    `media_play`, `media_pause`, `media_stop` (stops and cues the trim in
    point), `media_seek` (exactly one of `frame=<source frame>` or
    `time=<numerator>/<denominator>` seconds, the frame displayed at that
    time), and `media_complete_source` (declares a growing file finished);
  - playback settings (project edits, refused under the project or the
    input's padlock, persisted): `media_trim` (`trim_in`, `trim_out`
    `<frame>` or `-` for the file's end, greater than `trim_in`),
    `media_rate` (`speed_basis_points` 2500 through 40000, 0.25x to 4x),
    `media_end_action` (`end_action` `stop|hold|loop`),
    `media_end_trigger_lead` (`end_trigger_lead` 0 through 3600 frames or
    `-` for none), `media_audio_track` (`audio_track` a stream index the
    file lists, or `-` for its default track), and `media_audio_policy`
    (`audio_policy` `mute|resample`, the audio at a speed other than 1x).
- `event=media_changed` (`frame`, `operation` and its fields prefixed
  `operation_`, then the input's complete status) announces each accepted
  media command.
- A media status is the input and its settings (`trim_in`, `trim_out`,
  `end_action`, `end_trigger_lead`, `speed_basis_points`, `audio_policy`,
  `audio_stream`, `growing` `-` or `<live-edge offset frames>:<idle
  refreshes before completion>`, `start_frame`) then its transport
  (`available`, `state` `stopped|playing|paused|waiting|ended`, `position`
  source frame, `position_time` exact seconds in lowest terms,
  `playable_end`, `frames`, `complete`, `loops`, `audio_track`, `tracks`
  with each stream's sample rate, channels, default flag, language, and
  title, `source_rate`, and `fault`). `waiting` is a growing file starved at
  its live edge.
- The snapshot gains `media_states` (at most 256); one `kind=media_state`
  record per media input (ascending input) follows the animation preset
  states and completes the snapshot.
- `runtime_event=media` (`frame`, `media_events`, then the complete status)
  reports what happened on a frame: `seeked`, `end_trigger` (remaining
  frames before the end), `ended` (with the end action), `looped`,
  `live_edge_reached`, `resumed`, `source_grew`, `source_completed`,
  `audio_track_changed`, and `faulted` (`source_missing`,
  `source_truncated`, `source_replaced`, `source_rewritten`,
  `source_changed`, `unsupported`, `decode_failed`, `tool_unavailable`). It
  is also sent once a second as a position sample while the transport
  advances.
- An end trigger of the input on air in a playing playlist advances the
  playlist (a journaled `playlist_control` `next`), so a clip's end can
  drive the running order.
- Durability: the settings are project state (schema 36, each media input's
  `playback`); the transport itself is runtime state and a restored show
  cues each input at its `start_frame`, stopped.
- 2.36 is refused.

Protocol 2.38 completes the input lifecycle (`IN-026`: duplicate,
categorize, colour-label, pause, close, offline, replace source, and batch
import, besides the existing add, remove, rename, and reorder):

- Commands, all project edits (`EditProject`, undoable, realized on the
  `project` domain, refused under the project padlock and, for edits naming
  an input, that input's padlock):
  - `input_duplicate` (`source`, `input`, optional `name`; without a name
    the source name takes the next free ` 2`, ` 3`, ... suffix) appends a
    copy with the source's kind, audio strip, category, colour label, media
    settings, title, and scene; the copy starts neither paused nor closed.
  - `input_replace_source` (`input`, `source`) replaces a generator, media,
    device, or network input's source: `solid:<rrggbbaa>[:<tone hz>]`,
    `bars[:<tone hz>]`, `media:<asset URI>`, `device:<stable key>`, or
    `network:<endpoint>` (locators escaped, at most 1024 bytes). A media
    source must be a strict `asset://` URI with a media file extension;
    replacing into or out of media adds or drops the media settings.
  - `input_category_set` (`input`, `category` `<id>` or `-`),
    `input_color_set` (`input`, `color`
    `red|orange|yellow|green|teal|blue|purple|pink` or `-`),
    `input_pause_set` (`input`, `paused`), and `input_close_set` (`input`,
    `closed`). Closing an input on air (Program, a transition, an
    overlay, or an enabled layer of a scene on air) is refused
    (`input_in_use`). A closed input never reaches Preview or air: a
    command that would bring it, or a scene showing it, there (Preview
    selection, cut, transition, overlay, playlist cue) is refused
    (`input_closed`). Closing the input on Preview is allowed; it is then
    not taken until it reopens.
  - `input_categories_set` (`categories`, `<id>~<name>` joined by `,`, at
    most 32, ids nonzero and unique, names nonblank, at most 48 bytes, and
    unique ignoring case) replaces the project's category list; dropping a
    category still assigned is refused (`category_in_use`).
  - `input_import_batch` (`items`, at most 64 `<input>~<name or
    ->~<uri>`) appends one media input per acceptable item. Items are
    planned in order and refused individually (`invalid_uri`,
    `unsupported_type`, `unavailable` for a source that is not an
    `asset://` URI, `duplicate_source`, `duplicate_input`, `invalid_name`,
    `duplicate_name`, `too_many_inputs`); a batch with no acceptable item
    is refused. A name derives from the file name, made unique. `freemixd`
    first copies each absolute host path to a media file into the bundle
    (`assets/`) and executes and journals the command with the resulting
    `asset://` URIs; the same applies to a `media:` replacement source.
    The command is validated before anything is copied: a refused command
    or item copies nothing (it is refused for its own reason), and a copy
    the executed command does not keep is removed.
  - `inputs_remove` (`inputs`) removes several inputs at once; it is how
    a duplicate or an import is undone, and it forgets the edit history
    naming the removed inputs.
- `event=input_lifecycle_changed` (`operation` and its fields, `added`
  `<input>~<name>` joined by `,` in append order, `refused`
  `<item index>:<refusal>` joined by `,`) announces each accepted edit and
  its undo or redo.
- Each snapshot input is `<id>~<name>~<lifecycle>` where the lifecycle is
  `<category or ->:<colour or ->:<paused 0|1>:<closed 0|1>:<offline reason
  or ->`; the snapshot gains `input_categories` (encoded last).
- `runtime_event=input_availability` (`input`, `offline` reason or `-`)
  reports a source going offline or coming back: `source_missing`,
  `source_changed`, `unsupported`, `decode_failed`, `tool_unavailable`
  (mapped from media faults; a media input that ticks again is back), and
  `device_unavailable`, `network_unavailable` for the media plane. Offline
  is runtime state and never persisted.
- Realization: a paused input holds its last frame (its media transport
  stops moving while it keeps its play state); a closed input releases its
  source (the engine drops the media transport and reports one `media`
  event without a tick; the native daemon stops the file's decoder worker,
  shows no picture for the input (transparent black) and no sound, and
  reopens the file when the input reopens). The native daemon does not open
  a closed input's file at startup, and an open input whose file is missing
  or does not play starts offline (`source_missing`, `unsupported`, ...)
  instead of stopping startup; closing and reopening it retries the file. A
  media-file worker that fails takes its input offline (`decode_failed`). The native daemon realizes
  `add_input`, `remove_input`, `input_duplicate`, `input_import_batch`,
  `inputs_remove`, `input_replace_source`, and their undo and redo live,
  starting new sources off the render thread; a source that fails to start
  reports through the input's status. It refuses (`unavailable`,
  `runtime_unsupported`) a change that would add or replace a capture,
  title, colour, alias, or Program/Preview feed input or a generator test
  tone, or remove a capture input or a Stinger clip input it decodes since
  startup, and refuses (`resource_exhausted`, `bounds_exceeded` on
  `inputs`) a catalogue past its native source bound.
- Durability: categories and per-input lifecycle are project state
  (schema 37, `input_categories` and `input_lifecycle`).
- 2.37 is refused.

Protocol 2.39 makes the layer designer live (`SW-006`): every scene layer
property is editable on a running show, addressed by scene id and stable
layer id whether or not a Scene input shows the scene.

- Commands, all project edits (`EditProject`, undoable with an exact
  inverse, journaled, realized on the `project` domain, refused under the
  project padlock and the padlock of every Scene input showing the scene).
  Each carries `scene`, `layer`, and `continues` (`true|false`, required)
  and then:
  - `scene_layer_geometry_set`: `x`, `y` (top-left of the quarter-turned,
    untilted footprint), `width`, `height` (nonzero, before the quarter
    turns), `quarter_turns` (`0..=3`), `tilt_millidegrees`
    (`-45000..45000`, a further clockwise turn about the anchor; the free
    rotation is `quarter_turns * 90000 + tilt_millidegrees`), `anchor_x`,
    `anchor_y` (the scale and rotation pivot in basis points of the box,
    `0..=10000`, centre `5000`).
  - `scene_layer_appearance_set`: `enabled`, `opacity` (`0..=255`).
  - `scene_layer_crop_set` (`x`, `y`, `width`, `height`: nonempty and
    inside the canvas) and `scene_layer_crop_clear`.
  - `scene_layer_mask_set` (`x`, `y`, `width`, `height` in post-crop source
    pixels, nonempty and inside the cropped source, and `invert`) and
    `scene_layer_mask_clear`. A crop that would leave the mask outside the
    cropped source is refused.
  - `scene_layer_z_order_set` (`z_order`, any `i32`) and `scene_layer_move`
    (`index`, the layer's new place in scene order).
  - `scene_layer_add` (`index`, `spec`: one complete layer in the layout
    layer encoding followed by its effects) and `scene_layer_remove`. A
    scene source must not make the scene contain itself.
  Each edit is checked as a whole before the scene changes: an unknown
  scene or layer is `not_found`; a duplicate layer id, an out-of-range
  index, a blank name, or an invalid geometry, crop, mask, or effect is
  refused with the field; an added layer's source must be an input of the
  show or a registered scene and must not make the scene draw itself; a
  scene holds at most 192 layers, and at most 64 enabled layers (the
  compositor's limit; adding an enabled layer to, or enabling a layer of, a
  full scene is `conflict` with `too_many_enabled_layers`); removing a layer
  is refused while the scene's animation has a track for it or the layer is
  graded, so its undo restores the scene exactly. A source cycle is
  followed through alias inputs too (`source_cycle`).
- `continues=true` marks an edit that continues a gesture (a drag): when
  the redo stack is empty and the newest history entries (at most 32,
  skipping only edits of other layers of the same scene) hold an edit of the
  same scene, layer, and property (geometry, appearance, crop, mask, or z
  order), the edit folds into that entry, so a whole drag is one undo step
  per layer that returns to the state before the drag. Undo and redo
  journal the exact inverse or forward edit with `continues=false`.
- `event=scene_layout_changed` (the edit's fields, then the scene's whole
  `layout`: `width`, `height`, `layers`) announces each accepted edit and
  its undo or redo; every Scene input showing the scene shows the change on
  the next frame, in the simulated and native renderers alike (the native
  daemon recompiles the scene, binding a leaf input the scene was not
  planned with on the next frame; a layer that starts drawing a nested
  scene the scene was not planned with is drawn after the next start, and
  an edit that would take the native plan past its 64 active layers keeps
  the scene's last composition until it is back within the bound).
- Each layout layer (in `scene_layout_state`, `scene_layout_changed`, and a
  scene input's layout) now carries the whole design: flags gain bit 1
  crop, bit 2 mask, bit 3 mask inverted, and after the opacity come the
  tilt (zigzag), `anchor_x`, `anchor_y`, the z order (zigzag), then the
  crop and the mask when flagged.
- Scene animation poses compose with the stored design: an animated
  rotation adds to the stored free rotation, and animated scale and
  rotation pivot about the anchor.
- Durability: tilt and anchor are project state (schema 38, each layer
  geometry's `tilt_millidegrees`, `anchor_x`, and `anchor_y`).
- 2.38 is refused.

Protocol 2.40 completes the per-input mixer strip (`AU-001`: pan, labels,
and VCA groups) and lets Fade to Black fade only chosen outputs (`SW-004`).

- `input_audio_strip` gains `pan_basis_points` (`-10000..=10000`, after
  `balance_basis_points`). Pan places a mono strip with a -3 dB
  constant-power law (0 dB per side at centre, `+3 dB` on the hard side,
  silent on the other); balance trims the sides of a stereo strip without
  moving it. Pan is ignored for strips with more than one channel.
- Strip tuples (`input_audio_strips` in the snapshot and
  `desired_switcher`) are `input:gain:balance:pan:muted:soloed:afv:delay:group`,
  `group` being `-` or `1..=4`.
- Mixer commands, `payload=` then fields; `ControlAudio` permission:
  - `audio_strip_label` (`input`, `label`: at most 32 bytes, no control
    characters; empty clears it and Studio shows the input name),
    `audio_strip_group` (`input`, `group`: `1..=4` or `-`), and
    `audio_group_name` (`group`, `name`: non-blank, at most 32 bytes) are
    project edits: undoable with an exact inverse, journaled, realized on
    the `project` domain, and refused under the project padlock and (for
    the strip edits) the strip input's padlock.
  - `audio_group_level` (`group`, `gain_millidb`: `-96000..=24000`,
    `muted`: `0|1`) is a live operation like a strip fader, realized on
    the `audio` domain and not part of the undo history.
  A group is a VCA: a member's effective gain is its strip gain plus the
  group gain in dB, and a muted group silences every member without
  changing the member strips' own mute. Groups carry no audio of their own.
- `fade_to_black_output` (`output`, `affected`: `0|1`; `Transition`
  permission) chooses whether Fade to Black fades one project output. It is
  an undoable project edit realized on the `project` domain; an unknown
  output is `not_found`. Every output is affected by default, including
  outputs added later; an excluded output keeps showing and sounding the
  unfaded Program while the others fade (logical outputs, fullscreen,
  recordings, and stream targets that follow it alike).
- `event=audio_mixer_changed` (`operation` naming the command payload, then
  its fields) and `event=fade_to_black_output_changed` (`output`,
  `affected`) announce each accepted edit and its undo or redo.
- The snapshot appends, after `input_categories`: `audio_groups` (four
  escaped `name~gain_millidb~muted` entries, group 1 first),
  `input_audio_labels` (sparse escaped `input~label`, strictly ascending
  by input), and `fade_to_black_excluded_outputs` (strictly ascending
  output ids).
- Durability: pan, labels, groups, group levels, and the excluded outputs
  are project state (schema 39).
- 2.39 is refused.

Protocol 2.41 tracks live layer gestures (`SW-006`).

- Every `scene_layer_*` command and `scene_layout_changed` event carries
  `gesture` after `continues` (a canonical `u64`, required): a token the
  client picks once per gesture, unique across sessions (Studio picks a
  random one), or `0` outside any gesture.
- `continues=true` folds only into a history entry its own `gesture`
  opened (and never with `gesture=0`), so a gesture whose first edit was
  refused or never arrived opens a new undo step instead of folding into an
  earlier gesture of the same layer property. A gesture that ends exactly
  where it started (a designer closed mid-drag sends the layer back) leaves
  no undo step.
- Undo and redo journal the exact inverse or forward edit with
  `continues=false` and `gesture=0`.
- 2.40 is refused.

Protocol 2.42 makes fullscreen display outputs project configuration
(`OR-011`).

- A display-output configuration is the fields `display` (`id:<id>`,
  `id:<id>|name:<name>` — the id, falling back to a unique display with
  the name — `name:<name>`, or `index:<n>` in desktop order), `source`
  (`program`, `preview`, or `output:<output id>`), `mode` (`current` or
  `<w>x<h>@<millihertz>`), `scaling` (`fit`, `fill`, `stretch`, `1:1`), and
  `hdr` (`off`, `auto`). Display ids and names are non-empty, at most 256
  bytes, and contain no control characters or commas.
- `set_display_output` (`slot`: `1|2`, then the configuration) and
  `clear_display_output` (`slot`) are project edits (`EditProject`:
  Graphics, Admin): undoable with an exact inverse, journaled, realized on
  the `project` domain, and refused under the project padlock. An unknown
  source output is `not_found`, a display the other slot already claims is
  `conflict`, and an unusable selector or mode is `invalid_command`. A
  project output shown by a slot cannot be removed. The daemon applies an
  accepted edit live: a source, mode, scaling, or HDR change on the same
  display keeps the window bound; a different display rebinds it.
- `event=display_output_changed` (`slot`, `configured` `0|1`, then the
  configuration fields when configured) announces each accepted edit and
  its undo or redo.
- Runtime events: `display_output_status` (`slot`, `state=active` with the
  bound display's `id`, `name`, `mode`, `mode_fallback` (`0|1`: an exact
  mode the display does not offer runs at its current mode) and HDR state
  (`sdr`, `hdr`, `sdr-display-not-capable`, `sdr-capability-unknown`,
  `sdr-presenter-limited`), or `state=waiting` with `reason`
  `display-absent|display-in-use|ambiguous-name|enumeration-failed|no-presenter`)
  is published when a configured slot's state changes, including hot-plug;
  `display_inventory` (`presenter` `0|1`, optional `error` of at most 256
  bytes, and up to 16 `displays`, each
  `<id>~<name>~<x>~<y>~<current mode>~<modes ;-separated, at most 128>~<hdr capability unknown|sdr|hdr>`)
  is published when the connected displays change.
- `display_inventory_request` (`request_id`) is a read-only query like
  `diagnostics_request`; any role may send it, and the server answers
  `display_inventory` with the same `request_id` and the inventory fields.
  A daemon without a display presenter answers `presenter=0` and no
  displays, and its configured slots wait with `no-presenter`.
- The snapshot appends, after `fade_to_black_excluded_outputs`:
  `display_outputs` (comma-separated
  `<slot>~<display>~<source>~<mode>~<scaling>~<hdr>`, strictly ascending
  by slot) and `display_output_states` (comma-separated
  `<slot>~waiting~<reason>` or
  `<slot>~active~<id>~<name>~<mode>~<fallback>~<hdr state>` for every
  configured slot, strictly ascending). List text is percent-escaped.
- Durability: the slots are project state (schema 40, `display_outputs`
  in `project.json`). `freemixd serve --display-output <spec>` (up to two)
  replaces the project's slots when the project loads.
- 2.41 is refused.

Protocol 2.43 gives every scene layer a key and a bounded effect list
(`IN-020`): one background plus ten or more positioned foreground layers,
each crop, transform, key, and effect capable.

- A layer key is a chroma key (`red`, `green`, `blue` key colour,
  `similarity`, `smoothness`, `spill`; every field `0..=255`) or a luma key
  (`low`, `high`, `softness` in `0..=255` with `low < high`, and `invert`).
  A chroma key keeps pixels whose largest channel distance from the key
  colour exceeds `similarity`, ramping over `smoothness`, and pulls `spill`
  of the key colour out of what it keeps; a luma key (BT.709 luma) keeps
  `low < luma < high` (`high = 255` keeps everything above `low`), ramping
  over `softness`, and `invert` swaps kept and removed.
- A layer holds at most **8** effects, applied in order. Each effect is one
  of `levels` (`black`, `white`: `0..=255`, `black < white`),
  `brightness_contrast` (`brightness`, `contrast`: `-100..=100`),
  `saturation` (`amount`: `-100..=100`), `sharpen` (`amount`: `0..=1000`),
  or `flip` (`horizontal`, `vertical`: `0|1`) with exactly its parameters.
- Every renderer (the simulated CPU compositor and the native GPU
  compositor) applies, per layer: crop, then the key times the mask, then
  the effects on straight colour, then the input and layer grades, then
  the transform and composition. The 64-enabled-layer limit is unchanged.
- Each layout layer gains flag bit 4 (key) and, after the crop and the
  mask, the key when flagged (`0` then the six chroma bytes, or `1` then
  `low`, `high`, `softness`, and `invert` as bytes) and then
  `effect_count` (at most 8) and the effects, each a kind byte (index into
  `levels`, `brightness_contrast`, `saturation`, `sharpen`, `flip`) and its
  parameters as zigzag varints. `scene_layer_add`'s `spec` is now exactly
  one layout layer (the effects are inside it). `MAX_SCENE_LAYOUT_BYTES`
  is now **42 KiB** raw, and a command or event field holds at most
  **56 KiB** encoded so the worst-case layout still fits one 64 KiB line.
- New layer edits (same envelope, permission, padlocks, undo, and
  `scene_layout_changed` event as every `scene_layer_*` edit):
  `scene_layer_key_set` (`kind=chroma` with `red`, `green`, `blue`,
  `similarity`, `smoothness`, `spill`, or `kind=luma` with `low`, `high`,
  `softness`, `invert`), `scene_layer_key_clear`, and
  `scene_layer_effects_set` (`effects`: unpadded base64url of
  `effect_count effect*` in the layout encoding; the empty list is `AA`),
  which replaces the whole ordered list (adding, removing, reordering, and
  changing parameters are all one edit). A key or an effect list is a
  gesture property like geometry: `continues=true` with the same nonzero
  `gesture` folds a scrub into one undo step. An invalid key is refused
  with field `key` code `invalid_key`; an invalid list with field
  `effects` code `invalid_effect`.
- Durability: keys are project state (schema 41, each layer's `key`:
  `null`, `{"type": "chroma", ...}` or `{"type": "luma", ...}`), and stored
  effects must be one of the five kinds above.
- 2.42 is refused.

Protocol 2.44 adds a solo mode with a monitor bus, group solo, and nested
VCA groups (`AU-001`).

- New mixer commands (`ControlAudio` permission, announced by
  `event=audio_mixer_changed` like every mixer operation):
  - `audio_group_solo` (`group`, `soloed`: `0|1`) is a live operation like
    `audio_group_level`: realized on the `audio` domain and not undoable.
    A soloed group solos every member strip.
  - `audio_group_parent` (`group`, `parent`: `1..=4` or `-`) nests a group
    inside another. A member then follows every group on its parent chain:
    the group gains add in dB (the linear gain is the product along the
    chain), any muted group mutes, and any soloed group solos. A parent that
    would form a cycle (including the group itself) is rejected, so a chain
    is at most four groups.
  - `audio_solo_mode` (`mode`: `sip`, `pfl`, or `afl`) chooses what a solo
    does. `sip` (solo-in-place, the default) gates Program as before. `pfl`
    and `afl` leave Program untouched and send the soloed strips to the
    monitor bus: `pfl` taps after delay and channel mapping and before the
    fader, groups, balance, pan, mute, and follow-video; `afl` taps after
    the strip and group faders, balance, and pan, still ignoring mute and
    follow-video. With nothing soloed, or in `sip`, the monitor bus follows
    Master.
  `audio_group_parent` and `audio_solo_mode` are project edits: undoable
  with an exact inverse, journaled, realized on the `project` domain, and
  refused under the project padlock.
- Group entries in the snapshot's `audio_groups` are
  `name~gain_millidb~muted~soloed~parent` (`parent` being `-` or a group
  number), and the snapshot appends `audio_solo_mode` after
  `audio_groups`.
- `audio_meters` appends `monitor`: `-` when the server meters no monitor
  bus, or `master~<channels>` / `solo~<channels>` (whether the bus
  currently carries the solo listen or follows Master) with exactly as
  many channels as `master`, in the same encoding.
- Durability: group solos, group parents, and the solo mode are project
  state (schema 42: each `audio_groups` entry's `soloed` and `parent`, and
  the top-level `audio_solo_mode`). `freemixd` meters the monitor bus but
  has no headphone device path yet, so the bus is not audible.
- 2.43 is refused.

Protocol 2.45 adds typed refusal reasons and encodes mixer labels once.

- A rejected `command_result` may append `reason` after `retryable`: a
  colon-separated token list, the kind first, then its typed parameters
  (canonical decimals; slots and channels nonzero). `code` stays the coarse
  class and `message` the English text, now only a fallback: a client words
  the refusal from `reason` in its own language and shows `message` when
  `reason` is absent. A refusal without a typed reason omits the field.
  - `permission_denied`, `deadline_exceeded`, `duplicate_name`,
    `project_locked`, `nothing_to_undo`, `nothing_to_redo`,
    `history_conflict`, `history_drift`, `not_durable` (the daemon could not
    make the command durable), `rate_limited`, `runtime_unsupported` (the
    running media runtime cannot do it, such as a native daemon's live input
    catalogue).
  - `revision_conflict:<current revision>`.
  - `unknown_id:<kind>:<id>` and `duplicate_id:<kind>:<id>`, `<kind>` one
    of `input`, `output`, `scene`, `layer`, `category`, `grade`, `lut`,
    `preset`, `title`, `playlist_entry`.
  - `invalid_name:<max bytes>`, `input_locked:<input>`,
    `display_in_use:<slot>` (the other display-output slot claims the
    display), `stinger_realizing:<slot>`,
    `playlist_take_scheduled:<frame>`.
  - `input_in_use:<input>:<usage>`, `<usage>` one of `program`, `preview`,
    `transition`, `manual_transition`, `scene_on_air`,
    `overlay:<channel>`, `overlay_queue:<channel>`, `stinger:<slot>`,
    `scene_layer:<scene>`, `scene_audio_source:<scene input>`,
    `alias_target:<alias input>`, `playlist_entry:<entry>`.
  - `bounds_exceeded:<field>:<max>`, `<field>` a `[a-z0-9_]` token of at
    most 64 bytes (such as `inputs`, `layers`, `effects`, `entries`).
  Any other kind or parameter shape is refused by the decoder.
- The engine, the control service, and `freemixd` attach a reason at every
  refusal that has one: the padlock and undo history, unknown and duplicate
  ids, names, input usage on the switcher, in a playlist, or in a persisted
  scene layer, scene audio source, or alias, display-slot conflicts, list
  bounds, a scheduled playlist take, a Stinger slot still realizing, a
  durability failure, session permission and rate limits, and a native
  daemon's live catalogue. Persisted idempotency receipts keep code and
  message only, so a receipt replayed after a restart carries no reason.
- The snapshot's `input_audio_labels` entries are `<input>~<length>:<label>`
  with `<length>` the label's canonical decimal UTF-8 byte length and the
  label raw, so the line encoding escapes it exactly once. 256 strips with
  32-byte labels that escape to three bytes per byte fit one field, and a
  256-input snapshot labelling every strip fits one line.
- Studio words typed refusals in English and Spanish (toasts, the status
  bar, and input add/remove refusals); `freemix-cli` prints
  `<code>: <message> (reason: <reason>)`.
- 2.44 is refused.

Protocol 2.46 adds network stream inputs (IN-015).

- A network stream configuration is `url`, `transport` (`auto`, `tcp`,
  `udp`; anything but `auto` only for `rtsp://`), `jitter_ms`
  (40..=5000), `backoff_min_ms` and `backoff_max_ms` (100..=60000, minimum
  not above maximum), `give_up` (`never` or a canonical 1..=10000
  attempt count), `timeout_ms` (500..=60000), and `stall` (`freeze` holds
  the last picture, `black` shows black; audio fades to silence either
  way). The URL is at most 512 bytes of printable ASCII with a host, and its
  scheme is `rtsp`, `rtp`, `udp`, `tcp`, `http`, or `https` (a `.m3u8`
  path is HLS). Both sides validate every field; the engine also refuses a
  transport hint on a non-RTSP URL.
- `add_input` takes `kind=network_stream` with the configuration fields and
  the full URL. `payload=input_stream_set` with `input`, an optional `url`,
  and every setting edits a network stream input; without `url` the input
  keeps its URL. Editing an input that is not a network stream is refused
  `invalid_command`, and an unknown input `not_found`.
- Secrets never leave the daemon: every snapshot, event, status, and log
  carries the URL redacted, its user information and the values of secret
  query parameters (`password`, `token`, `key`, `signature`, ...) replaced
  by `***`. Only the project file keeps the full URL.
- `input_added` for a network stream appends `kind=network_stream` and its
  redacted configuration; `event=network_stream_changed` carries `input`
  and the edited redacted configuration.
- The runtime event `event=network_stream_status` carries `input`,
  `state` (`idle`, `connecting`, `buffering`, `live`, `stalled`,
  `reconnecting`, `failed`), `attempt` and `next_in_ms` only while
  `reconnecting`, `bitrate_bps`, `buffer_ms`, `dropped_frames`,
  `late_frames`, `reconnects`, and an optional `last_error` (at most 160
  bytes, redacted). `freemixd` sends a state change at once and other
  changes at most once a second.
- The snapshot's `network_streams` lists every network stream input in
  ascending input order as
  `<input>~<transport>~<jitter_ms>~<backoff_min_ms>~<backoff_max_ms>~<give_up>~<timeout_ms>~<stall>~<state>~<attempt>~<next_in_ms>~<bitrate_bps>~<buffer_ms>~<dropped>~<late>~<reconnects>~<length>:<url>`,
  the redacted URL written raw after its canonical byte length.
- Durability: a network stream input is an input kind of the project
  (schema 43, `kind.network_stream`), at most 16 per show.
- 2.45 is refused.

Protocol 2.47 adds capture device inputs (IN-001).

- A capture input names its device by an adapter-qualified stable key
  (`device`, 1 to 256 bytes without control characters) and carries
  settings: `format` (`-` picks the device format nearest the program size
  and rate, otherwise `<w>x<h>@<num>/<den>:<pixel>:<scan>` with the rate
  reduced, `<pixel>` one of `yuyv`, `nv12`, `mjpeg`, `<scan>` one of `p`,
  `tff`, `bff`, sides 16..=8192 and at most 240 frames per second),
  `audio` (`none`, `paired` for the device's own audio, or
  `alsa:<device>` with 1 to 64 bytes of printable ASCII), and `loss`
  (`hold` keeps the last picture, `black` shows black while the signal is
  lost). Both sides validate every field. At most 16 capture inputs per
  show; adding, duplicating, or replacing a source to a seventeenth is
  refused `conflict` with reason `too_many_capture_inputs`.
- `add_input` takes `kind=capture` with `device`, `format`, `audio`, and
  `loss`. `payload=input_capture_set` with `input`, `format`, `audio`, and
  `loss` replaces a capture input's settings; editing an input that is not
  a capture input is refused `invalid_command`, and an unknown input
  `not_found`. The edit is undoable and honours padlocks. Replacing a
  capture input's device keeps its settings, and duplicating one copies
  them.
- `input_added` for a capture input appends `kind=capture` and its
  configuration; `event=capture_settings_changed` carries `input` and the
  edited settings.
- The runtime event `event=capture_status` carries `input`, `state`
  (`idle`, `starting`, `live`, `signal_lost`, `failed`), `reason`
  (`unplugged`, `stalled`, `no_signal`, `error`, or `-`), `device` (the
  acquired node, at most 128 bytes, or `-`), `format` (the negotiated
  format or `-`), `audio` (`0` or `1`), `frames`, `losses`, `recoveries`,
  `renegotiations`, and `error` (at most 160 bytes, or `-`).
- `capture_devices_request` carries `protocol` and a non-blank
  `request_id`; any role may send it. The `capture_devices` reply echoes
  `request_id` and carries `backend` (a lowercase token such as `v4l2`, or
  `-` when the daemon realizes no capture inputs), `error` (why the last
  enumeration failed, or `-`), and `devices`. The runtime event
  `event=capture_devices` carries the same three fields when the inventory
  changes. `devices` joins at most 16 entries in ascending stable-key
  order with `,`; an entry is
  `<key>~<name>~<path>~<driver>~<bus>~<audio|->~<format>;<format>...`
  with at most 128 formats. Inside the list `%`, `~`, `,`, and `;` are
  written `%25`, `%7E`, `%2C`, and `%3B`, and a text that is exactly `-`
  is written `%2D`; decoding accepts only that form. A daemon drops formats
  from the longest lists, then devices, until the reply fits one line.
- The snapshot's `capture_inputs` lists every capture input in ascending
  input order as
  `<input>~<device>~<format|->~<audio>~<loss>~<state>~<reason|->~<node|->~<active format|->~<0|1>~<frames>~<losses>~<recoveries>~<renegotiations>`,
  escaped as in `devices`.
- Durability: a capture input is the project's `device` input kind, which
  gains a `capture` object with `format` (token or `null`), `audio`, and
  `loss` (schema 44).
- `freemix-cli` adds `input-add-capture`, `input-capture-set`,
  `input-capture-status`, their `remote-` forms, and
  `remote-capture-devices`, which prints each device and a table of its
  formats.
- 2.46 is refused.

Protocol 2.48 completes Fade to Black and the T-bar (SW-004).

- `payload=fade_to_black_output_duration` with `output` and
  `duration_frames` (1..=3600, or `-` to follow the global duration) gives
  one output its own Fade-to-Black duration; an unknown output is refused
  `not_found`. `payload=fade_to_black_curve` with `curve` (`linear` or
  `ease`, the exact integer smoothstep `3t^2 - 2t^3`) shapes every
  automatic Fade-to-Black move. Both are transition-class, undoable project
  edits that honour padlocks. `event=fade_to_black_output_duration_changed`
  (`output`, `duration_frames`) and `event=fade_to_black_curve_changed`
  (`curve`) replicate them.
- The snapshot adds `fade_to_black_output_durations`, the overrides in
  ascending output order as `<output>:<frames>` joined with `,` (empty for
  none), and `fade_to_black_curve`.
- `payload=manual_start` takes any timed kind as `transition`:
  every mix token (`fade`, `wipe`, `slide`, `alpha_fade`, `vertical_wipe`,
  `cube_zoom`, ...) or `stinger_<slot>` with a nonzero `duration_frames`
  naming the clip length. A manual Stinger needs its configured slot with
  ready media; the T-bar position picks the clip frame. Manual status
  fields add `desired_manual_duration_frames`,
  `realized_manual_duration_frames`, or `manual_duration_frames` for a
  Stinger. Moving the T-bar back reverses the mix exactly; commit and
  cancel complete at the next frame boundary.
- Durability: the project gains `fade_to_black_output_durations` (a list of
  `{"output", "duration_frames"}`) and `fade_to_black_curve`, and a
  persisted manual transition carries `clip_frames` (0 for a mix) (schema
  45).
- `freemix-cli` adds `ftb-output-duration <output> <frames|none>`,
  `ftb-curve <linear|ease>`, and their `remote-` forms; `tbar-start` takes
  any mix token or `stinger-<n> <frames>`.
- 2.47 is refused.

Protocol 2.49 adds RTMP and SRT network stream inputs (IN-016, IN-017).

- The URL scheme may also be `rtmp`, `rtmps` (an application path segment
  is required), or `srt` (a port is required; the path is empty or `/`).
- A network stream configuration adds `mode` (`caller`, `listener`, or
  `rendezvous`; RTMP takes `caller` or `listener`, `rtmps` only `caller`,
  SRT all three, other protocols only `caller`), `latency_ms` (SRT receive
  latency, 20..=8000, default 120), `key_length` (SRT AES key length in
  bytes, `16`, `24`, or `32`), `audio` (`mix` downmixes every source
  channel; `1-2`, `3-4`, `5-6`, `7-8` pick one stereo pair of up to eight
  source channels, a missing pair is silent), and the optional
  `stream_id` (SRT, 1..=512 printable bytes), `stream_key` (RTMP, 1..=256
  printable bytes; a listener refuses a publisher using another key, a
  caller appends it to the URL path), and `passphrase` (SRT, 10..=79
  printable bytes; with it the stream is AES-encrypted). A listener or
  rendezvous URL needs an explicit port of 1024 or above (RTMP defaults to
  1935); its host is the local address to bind.
- Two inputs never listen on one port: an `add_input`, `input_stream_set`,
  or source replacement whose listener would bind the TCP (RTMP) or UDP
  (SRT) port another input listens on is refused `conflict` with field
  `url` code `listen_port_in_use`.
- `payload=input_stream_set` adds the optional `stream_id` (absent clears
  it) and secret edits `stream_key` and `passphrase`: absent keeps the
  stored secret, empty clears it, any other value replaces it (`***` is
  refused).
- Secrets never leave the daemon: `input_added`, `network_stream_changed`,
  and the snapshot carry `***` for a set stream key or passphrase; an RTMP
  URL path after the application is redacted as `/app/***`; status errors
  and logs are scrubbed of both secrets.
- `event=network_stream_status` adds the state `listening` (a listener
  waiting for its publisher; it listens again when the publisher leaves and
  counts no reconnect) and `source_channels`, the audio channel count of
  the received stream (0 until known).
- The snapshot's `network_streams` entry becomes
  `<input>~<transport>~<jitter_ms>~<backoff_min_ms>~<backoff_max_ms>~<give_up>~<timeout_ms>~<stall>~<mode>~<latency_ms>~<key_length>~<audio>~<key_set>~<passphrase_set>~<state>~<attempt>~<next_in_ms>~<bitrate_bps>~<buffer_ms>~<dropped>~<late>~<reconnects>~<source_channels>~<length>:<stream_id><length>:<url>`,
  `key_set` and `passphrase_set` being `0` or `1` and the stream ID empty
  for none.
- Durability: `kind.network_stream` adds `mode`, `latency_ms`,
  `key_length`, `audio`, and `stream_id`, `stream_key`, `passphrase`
  (`null` or a string; the project keeps the secrets) (schema 46).
- `freemix-cli` adds `--mode`, `--latency-ms`, `--key-length`, `--audio`,
  `--stream-id`, `--stream-key`, and `--passphrase` to `input-add-stream`
  and `input-stream-set` (and their `remote-` forms); the set commands also
  take `--clear-stream-id`, `--clear-stream-key`, and `--clear-passphrase`.
- 2.48 is refused.

Protocol 2.50 adds operator cue lists with a GO key (RC-017).

- A project holds up to 8 cue lists (ids 1 and up), each with a name of 1 to
  64 bytes and up to 64 cues; a cue has an id unique in its list, a name,
  an `auto_follow` flag, and up to 16 actions (256 per list, at most 2048
  bytes of title text per list). Actions are the tokens of `cues` below:
  preview, take (cut, a mix kind with 1..=3600 frames, or a stinger slot,
  of an input or of Preview), overlay on and off (channels 1..=8), title
  on, title text (up to 128 bytes), title off, Fade to Black, playlist
  start and stop, a strip fader level or mute, recorder 1 or 2 start or
  stop, stream target start or stop, and `wait` (1..=216000 frames). A cue
  takes Program at most once between waits, and a cue that auto-follows
  with no trailing wait shares its last frame with the next cue's actions
  up to that cue's first wait, so that run takes Program at most once too:
  a list breaking either rule is refused `invalid_command` (field `cues`,
  code `conflicting_follow_takes` naming both cues for the auto-follow
  case).
- `payload=cue_list_set` with `list`, `name`, and `cues` creates or replaces
  one list; `payload=cue_list_remove` with `list` deletes one. Both are
  `edit_project`-class, undoable project edits that honour the project
  padlock; every input and stream target a cue names must exist
  (`not_found`), a title action must name a title input
  (`invalid_command`), and a cue that is waiting on its next actions cannot
  be changed or removed (`conflict`, reason `cue_running:<cue>`). An edit
  keeps the run state of the cues it keeps; a new list puts its first cue
  on standby. A standby cue the edit removes passes to the new cue now at
  its position, else to the next surviving cue; a list with nothing on
  standby (played out or empty) puts the first cue added after its last
  surviving cue on standby. Removing an input a cue names is refused
  `conflict` with the usage `cue_action:<list>:<cue>`; a stream target a
  cue names cannot be removed from the project either.
- `payload=cue_go` with `list` and an optional `cue` fires the cue on
  standby (or `cue`): its actions up to the first `wait` run in order on
  one frame, as one revision; standby moves to the next cue. With nothing on
  standby GO is refused `conflict` (reason `no_cue_on_standby`).
  `payload=cue_standby` (`list`, `cue`), `cue_back`, `cue_skip`, `cue_stop`
  (drops the waiting cue's remaining actions), and `cue_reset` (clears the
  played cues and stands by the first) are the other controls. All are
  `transition`-class operator controls in the `switcher` domain, journaled
  but never undoable. A refused action refuses the whole GO.
- Automation authority: a cue never acts with more authority than the
  session that fires it. Each cue action needs the permission of its
  direct command: `preview` needs `select_preview`; `audio_gain` and
  `audio_mute` need `control_audio`; `title_text` needs `edit_project`;
  takes, overlays, title on/off, Fade to Black, the playlist, recorders,
  streams, and media transport need `transition`; waits and delays need
  nothing. A `cue_go` additionally needs the permission of every action of
  the chain it arms, checked up front before anything is applied: the
  fired cue's actions across all of its waits, and, while the cue that
  ends auto-follows, every action of the next cue in list order. A
  session lacking one is refused `permission_denied` (reason
  `permission_denied`, the message naming the missing permission) and
  nothing fires. The daemon's clock then runs the armed chain's waits and
  follows (`cue_advance`) on that authority: it never runs an action the
  firing session was not checked for. Because an edit can change what an
  armed chain will run, a `cue_list_set` (or an undo or redo restoring
  one) of a list whose cue is waiting additionally needs every permission
  the edited chain needs that the armed chain did not. Standby, back,
  skip, stop, and reset fire nothing and need only `transition`; a list
  edit that arms nothing is a plain `edit_project` edit.
- `payload=cue_advance` with `list` and `frame` runs a waiting cue's next
  actions; only the daemon's cue clock submits it, on exactly the due frame
  (a client's is refused `unavailable`), and a cue that auto-follows fires
  the next cue on the same frame. While an accepted advance waits for its
  frame, other commands are refused `conflict` (reason
  `cue_scheduled:<frame>`). An action the show refuses when its wait ends
  stops the cue and records the fault.
- `event=cue_list_changed` carries a whole list: `list`, `name`, `cues`,
  and `state`; `event=cue_list_removed` carries `list`. They follow every
  list edit and its undo and redo.
- `event=cue_run_changed` follows every control and advance: `list`,
  `frame`, `state`, `fired` (the cue ids fired, `.`-separated), the
  resulting `program`, `preview`, unqualified manual-transition and
  Fade-to-Black status, `overlays`, the input audio strips, `streams`,
  `recordings`, `playlist` (the playlist run-state token), and `titles`
  (each title field the cue set, `/`-separated
  `<input>.<field revision>.<field>.<text hex>`).
- `cues` is a comma-separated list of `<id>~<name hex>~<0|1>~<actions>`,
  `<actions>` a `/`-separated list of `.`-separated tokens:
  `preview.<input>`, `take.<input|->.<effect>.<frames>`,
  `overlay_on.<channel>.<input>`, `overlay_off.<channel>`,
  `title_on.<input>.<channel>`, `title_text.<input>.<field>.<text hex>`,
  `title_off.<input>`, `fade_to_black.<0|1>.<frames>`, `playlist_start`,
  `playlist_stop`, `audio_gain.<input>.<millidB>`,
  `audio_mute.<input>.<0|1>`, `record.<recorder>.<0|1>`,
  `stream.<target>.<0|1>`, `wait.<frames>`; hex is lowercase UTF-8.
  `state` is `<standby>:<live>:<pending>:<played>:<fault>`: cue ids or
  `-`, `pending` `<cue>.<next action>.<due frame>`, `played` ascending
  `.`-separated cue ids, and `fault` `<cue>.<action>`.
- The snapshot adds `cue_list_states` (0..=8); that many
  `cue_list_state` records (`revision`, `frame`, `list`, `name`, `cues`,
  `state`) follow its media states, in list order, and complete it.
- Durability: the project gains `cue_lists`, each with its `id`, `name`,
  `cues` (`id`, `name`, `auto_follow`, typed `actions`), and run `state`
  (schema 47).
- `freemix-cli` adds `cue-lists`, `cue-list-set <list> <name> <cues>`
  (`id:name:0|1:action,...` joined with `;`, the action tokens above with
  plain title text), `cue-list-remove`, `go <list> [cue]`, `cue-standby`,
  `cue-back`, `cue-skip`, `cue-stop`, `cue-reset`, and their `remote-`
  forms; a local `go` runs the cue's waits on the simulated clock.
- 2.49 is refused.

Protocol 2.51 adds media transport actions and millisecond delays to cue
lists (RC-017).

- New action tokens: `media_play.<input>`, `media_pause.<input>`,
  `media_seek.<input>.<source frame>` (clamped to the clip's trimmed range),
  `media_restart.<input>` (a seek to the trimmed start, then play), and
  `delay.<millis>` (1..=3600000). A media action must name a media input
  (`invalid_command`) and runs through the media transport exactly like the
  matching `media_*` command; a delay separates frames like a `wait`.
- A delay is converted to whole frames at the show's frame rate when its cue
  fires: `ceil(millis * rate / 1000)` frames, at least one, so a delay never
  ends early (1001 ms at 30000/1001 fps is 30 frames, 1002 ms is 31). The
  pending wait's due frame is persisted, so a restart does not re-round it.
- `event=cue_run_changed` adds `media`: the transport of every media input
  the run's actions addressed, ascending, `/`-separated
  `<input>.<state>.<position>.<time numerator>.<time denominator>` (state
  a `media_state` transport token, time the exact playhead time in lowest
  terms); empty when none. Clients fold it into their media states.
- Durability: cue actions gain `media_play`, `media_pause`, and
  `media_restart` (`input`), `media_seek` (`input`, `frame`), and `delay`
  (`millis`) (schema 48).
- `freemix-cli` `cue-list-set` takes the new tokens, and a backslash
  escapes the next character in names and title text, so `\;`, `\:`,
  `\,`, and `\\` stand for `;`, `:`, `,`, and `\`.
- On the native runtime a cue's record and stream actions start and stop the
  same recorder segments and stream workers as `record_start`,
  `record_stop`, `stream_start`, and `stream_stop` (a cue's acknowledgement
  is not held for a segment's mux readiness). On the simulated daemon a
  client's cue control first catches the engine up to the show's
  wall-clock frame while a cue waits, and `cue_list_state.frame` reports that
  frame, as for playlists. A restarted daemon restores every list's standby
  pointer and pending wait and still ends the wait on its due engine frame
  (time the daemon was down does not count toward a wait); the native
  recorder startup barrier runs the cue clock too.
- 2.50 is refused.

Protocol 2.52 adds five simultaneous stream destinations with independent
control (OR-006).

- `payload=stream_retry` (`target`) reconnects one destination now, with a
  fresh reconnect budget and any latched failure cleared. It is refused
  `conflict` (`stream_not_running`) unless the destination is desired
  running, and `not_found` for an unknown target.
- `payload=stream_start_all` and `payload=stream_stop_all` set every
  destination's desired running flag at once.
  - Retry, start-all, and stop-all are authorized as transition operations,
    like `stream_start`, and each acceptance emits one `streams_changed`.
  - None of them ever touches a recorder.
- `payload=stream_target_set` adds or reconfigures one destination. It
  carries `target` plus these configuration fields:
  - `name`;
  - `stream_protocol` (`rtmp`, `rtmps`, `srt`, `hls`);
  - `endpoint` (scheme-relative `host[:port]/app[/path]`, `host[:port]` for
    SRT, or an absolute directory for HLS);
  - optional `backup_endpoint`;
  - `output`;
  - `startup` (`stopped`, `reconcile`);
  - `video_bitrate_kbps` (1000..=100000);
  - `reconnect` (`<initial ms>:<max ms>:<attempts or none>`, delays
    100..=300000 with max >= initial, attempts 1..=10000);
  - optional `stream_key`: absent keeps the stored key, empty clears it,
    `***` is refused, and the key is at most 512 bytes.
- `payload=stream_target_remove` (`target`) removes one destination.
  - Set and remove are undoable, padlock-guarded project edits.
  - They are refused while the destination is desired running
    (`stream_running`), and for:
    - a sixth destination (`too_many_streams`);
    - a duplicate name (`duplicate_name`);
    - an unknown output;
    - an invalid endpoint, key, bitrate, or policy (field issues
      `endpoint`, `backup_endpoint`, `stream_key`, `video_bitrate_kbps`,
      `reconnect`);
    - for removal, a destination a cue list starts or stops
      (`stream_in_use`).
- The snapshot adds `stream_targets`, a comma-separated list of
  `<target>~<name>~<protocol>~<endpoint>~<backup>~<output>~<startup>~<kbps>~<reconnect>~<has_key>~<ladder rungs>`
  in authored order.
- `event=stream_target_changed` carries `target`, `configured` (`0|1`), the
  configuration fields with `has_key` and `ladder_rungs` when configured,
  and the resulting `streams` roster.
- No snapshot, event, or refusal ever carries a stored stream key.
- Each `stream_status` sample is now
  `target:realized:connected:muxed:enqueued:dropped:reconnects:uptime_ms:bitrate_kbps:failure`,
  with the optional `:local=` health after it:
  - `reconnects` counts proven recoveries;
  - `uptime_ms` is how long the current session has delivered media;
  - `bitrate_kbps` is the encoded output over the last two seconds.
  The native daemon clamps failure text to the 64-byte detail bound.
- Durability: each stream target gains a required `reconnect` object
  (`initial_delay_ms`, `max_delay_ms`, `max_attempts` or `null`), with a
  default of 250 / 2000 / 40 (schema 49).
- 2.51 is refused.

Protocol 2.53 adds the headphone output (AU-001): the monitor bus plays on a
local audio output device at its own level and mute, independent of Program.

- `payload=audio_monitor_device` (`device`: an ALSA output name of 1 to 64
  bytes of printable ASCII without spaces, not starting with `-`, or empty
  for none) chooses the device. It is an undoable, padlock-guarded project
  edit authorized as an audio operation; choosing a device again, even the
  current one, reopens it, so a failed device is retried. Choosing the
  current device again changes nothing durable and records no undo entry.
  Only choosing the device again reopens a failed device: a later sample
  rate or channel change of the bus does not.
- `payload=audio_monitor_level` (`gain_millidb` -96000..=24000, `muted`
  `0|1`) moves the headphone feed's level. It is a live audio operation:
  never recorded, never undone, never padlock-guarded. It changes neither
  Program, recordings, streams, nor the monitor meters.
- Both are announced by `event=audio_mixer_changed` with
  `operation=audio_monitor_device|audio_monitor_level` and the same fields.
- The snapshot adds `audio_monitor=<device>~<gain_millidb>~<muted>` after
  `audio_solo_mode`, the device percent-escaped and empty for none.
- `audio_meters` `monitor` becomes
  `<master|solo>~<off|playing|failed>~<channels>`: `off` while no device is
  chosen or no audio is rendered, `playing` while the bus reaches the
  device, `failed` once the device could not be opened, stopped playing, or
  stopped reading (a write blocked for 3 s). The failure's cause and a
  bounded, redacted tail of the device child's standard error are logged
  by `freemixd` as one `FREEMIXD_MONITOR_OUTPUT_FAILURE` record; the wire
  carries only the state.
- Durability: the project gains a required `audio_monitor_output` object
  (`device` string or `null`, `gain_milli_db`, `muted`), meter-only at
  unity by default (schema 50).
- 2.52 is refused.

Protocol 2.54 adds SRT caller options to stream destinations (OR-006) and
keeps a destination's place in the authored list across undo.

- `payload=stream_target_set` gains:
  - `srt_latency_ms` (20..=8000), present exactly when `stream_protocol=srt`
    (default 120 for a new SRT destination in Studio and the CLI);
  - optional `srt_passphrase`, a secret edited like `stream_key`: absent
    keeps the stored passphrase, empty clears it, `***` is refused, and a
    set passphrase is 10..=79 bytes of printable ASCII without spaces
    (libsrt's bounds). Setting one on another protocol is refused (field
    issue `srt_passphrase`); a latency off SRT or out of range is refused
    (field issue `srt_latency_ms`);
  - optional `position` (0..=4, canonical decimal): the destination's index
    in the authored list, clamped to its end. Absent keeps an existing
    destination where it is and appends a new one. The undo of a removal
    carries the removed destination's old index, so removing B from A, B, C
    and undoing restores A, B, C live, in the journal, and in the saved
    project.
- `event=stream_target_changed` adds `srt_latency_ms` (SRT only) and a
  required `has_passphrase` (`0|1`, `1` only for SRT) when configured.
- Each snapshot `stream_targets` item gains two fields:
  `...~<ladder rungs>~<srt latency ms or empty>~<has_passphrase>`.
- No snapshot, event, refusal, log line, or `FFmpeg` stderr ever carries a
  stored passphrase; the native daemon masks it in worker diagnostics as it
  does stream keys.
- The native daemon publishes an SRT destination as MPEG-TS in caller mode
  with the authored latency and, when stored, the passphrase (AES
  encryption negotiated by libsrt), under the same per-destination
  reconnect policy as RTMP.
- Durability: each stream target gains required `srt_latency_ms` (a number
  exactly for SRT, otherwise `null`) and `srt_passphrase` (string or `null`,
  stored in the bundle under the same trust model as `stream_key`)
  (schema 51).
- 2.53 is refused.

Protocol 2.55 adds system audio inputs (AU-011): an audio-only input that
captures the server's own sound through a PulseAudio/PipeWire source or an
ALSA PCM.

- `payload=input_add` gains `kind=system_audio` with required
  `backend=pulse|alsa` and `device`: 1..=128 bytes of printable ASCII, not
  starting with `-` or a space and not ending with a space. At most 8 per
  show (field issue `too_many_system_audio_inputs`, `conflict`); a bad device
  is refused with field issue `device` (`invalid_device`).
- `event=input_added` carries the same `kind=system_audio`, `backend`, and
  `device`; one kind at most.
- `input_replace_source` accepts the source
  `system_audio:<backend>:<device>`; an unusable device is refused.
- `runtime_event` `event=system_audio_status` carries `input`,
  `state=idle|opening|live|reopening|failed`, `attempt` and `next_in_ms`
  exactly when `reopening` (`attempt` at least 1), `reopens`, and an optional
  one-line `last_error` of at most 160 bytes.
- Each snapshot gains `system_audio_inputs`, after `capture_inputs`: a
  comma-separated, strictly ascending list of
  `<input>~<backend>~<state>~<attempt>~<next_in_ms>~<reopens>~<length>:<device>`
  without error text. An input nothing captures is `idle`.
- A daemon that cannot capture system audio on its platform refuses a live
  add with `runtime_unsupported`; the native daemon captures on Linux only,
  and reports a stored input elsewhere as `failed` with the error
  `system audio capture is not supported on this platform`.
- Durability: an input kind `{"type": "system_audio", "backend", "device"}`
  (schema 52).
- 2.54 is refused.

Protocol 2.56 folds a grade drag into one undo step (SW-019).

- `grade_input_set|clear`, `grade_layer_set|clear`, and
  `grade_program_set|clear` gain an optional `gesture` (a canonical nonzero
  decimal token the client picks once per drag, unique across sessions)
  with a required `continues=true|false` beside it; neither appears outside
  a drag, and `continues` without `gesture` is refused. The first set of a
  drag sends `continues=false`; later sets and the set on release send
  `continues=true`.
- The server folds a continuing set into the newest undo step when that
  step sets the same target (one input, one scene layer, or the program)
  and was opened by the same token: the step keeps its inverse (the grade
  before the drag) and records the latest set without its gesture, so Undo
  reverts the whole drag. A drag that ends where it started leaves no step.
  Any other newest step (another edit in between, an undo, or a drag whose
  first set was refused or lost) makes the set open a new step.
- Studio's Colour window sends a drag's sets at most every 120 ms with one
  token, and its `Copy to…` sends the draft to several targets as one
  standalone set each (one undo step per target).
- No durability change: journals and history record sets without gestures.
- 2.55 is refused.

Protocol 2.57 adds LiveLAN (OR-017): a browser viewer stream of Program or
one project output for people on the local network, optionally behind a
PIN.

- `payload=live_lan_set` carries required `enabled=0|1`,
  `source=program|output:<id>`, and `max_viewers` (1..=32), and an optional
  `pin` edit: absent keeps the stored PIN, empty clears it, and 4..=12 ASCII
  digits replace it. Any other PIN is refused as field `pin` with the value
  reported as `***`; `***` itself is refused. It is a padlocked, undoable
  `edit_project` command; an unknown output is `not_found` and a viewer
  limit out of range `invalid_command` (field issue `max_viewers`).
- `event=live_lan_changed` carries `enabled`, `source`, `max_viewers`, and
  `has_pin=0|1`; no event, snapshot, refusal, or log line ever carries the
  PIN.
- `runtime_event` `event=live_lan_status` carries
  `state=off|unavailable|starting|live|failed`, `url` (empty, or
  `http://` plus at most 64 bytes of graphic ASCII without `,` or `~`,
  ending in `/`), `viewers`, and `egress_kbps`.
- Each snapshot gains, after `stream_targets`,
  `live_lan=<enabled>~<source>~<max_viewers>~<has_pin>` and
  `live_lan_status=<state>~<viewers>~<egress_kbps>~<url>`. A disabled
  LiveLAN is `off` with nothing else; an enabled one that no listener
  reports is `unavailable`, and a disabled one forgets its last report.
- An output LiveLAN streams cannot be removed (`conflict`).
- The native daemon serves LiveLAN only with `--live-lan-listen <addr:port>`
  (any address, LAN included, and `--native-media`), announced as
  `FREEMIXD_LIVE_LAN_READY\tv=1\taddress=<addr>`. `GET /` is the viewer
  page; `POST /v1/live/session` with body `pin=<digits>` (empty without a
  PIN) answers `204` and an `HttpOnly; SameSite=Strict` session cookie, `401`
  for a wrong PIN, `429` after five wrong PINs from one address within a
  minute, and `503` while LiveLAN is off or the viewer limit is watching;
  `GET /live/index.m3u8` and `GET /live/segment-<digits>.ts` need the
  cookie. The encoder writes one-second H.264/AAC HLS segments into a
  private temporary directory, so the delay is a few seconds. A change of
  source or PIN, or disabling, ends every session. A viewer is a session
  that fetched media within ten seconds.
- Durability: the project gains required
  `"live_lan": {"enabled", "source", "max_viewers", "pin"}` with `pin` a
  digit string or `null`, stored in the bundle under the same trust model
  as stream keys (schema 53).
- 2.56 is refused.

Protocol 2.58 adds project scripts (RC-011): named `FreeMix` Script sources
stored in the project and run on the authority.

- `payload=script_set` carries `script` (nonzero), `name` (1..=64 bytes,
  no control characters), and `source` (lowercase hex of at most 4096
  UTF-8 bytes whose only control characters are newline, tab, and carriage
  return). `payload=script_remove` carries `script`. Both are padlocked,
  undoable `edit_project` commands; a 17th script is a `conflict` (bounds
  field `scripts`), and an unknown script is `not_found`
  (`unknown_id:script:<id>`).
- `payload=script_run` carries `script`. It is a journaled `transition`
  command outside the undo history. The authority parses and runs the
  saved source with an instruction budget of 10000 (every statement, loop
  turn, and expression node costs one), at most 64 actions, 32 variables,
  and 256-byte strings. Each action is an ordinary show command (the cue
  action set: preview, takes, overlays, titles, Fade to Black, playlist,
  strip gain and mute, recorders, streams, and media transport) checked
  against the show as the actions before it left it, and all of them land
  on the command's target frame in one revision, or none does. A run that
  stops early is refused with
  `reason=script_failed:<syntax|runtime|instructions|actions|variables|string_bytes|refused>:<line>:<column>`;
  its code is the refused action's own code for `refused`,
  `resource_exhausted` for a budget, and `invalid_command` otherwise.
  A run never acts with more authority than the session that starts it:
  before anything is applied, the authority plans the run on a copy of the
  show exactly as it will execute it, and the session must also hold the
  permission of every action the planned run asks for (the cue action
  permissions above: `select_preview` for preview, `control_audio` for
  gain and mute, `edit_project` for title text, `transition` for the
  rest). The check follows the branches the script takes on the current
  show, and covers the actions a run that would stop early asked for up to
  and including the refused one. A session lacking one is refused
  `permission_denied` (reason `permission_denied`, the message naming the
  missing permission) and nothing runs.
- `event=script_changed` carries `script`, `name`, and `source`;
  `event=script_removed` carries `script`; `event=script_ran` carries
  `script`, `frame`, `actions`, `instructions`, and the desired show state
  fields of `cue_run_changed` (without the cue list run state).
- Each snapshot gains `script_states=<count>` after `cue_list_states`, and
  that many `script_state` records (`revision`, then the `script_changed`
  fields) follow the cue list states, in project order. A record may now
  carry up to 96 fields (previously 64), which the snapshot needs.
- Durability: the project gains required
  `"scripts": [{"id", "name", "source"}]` after `"live_lan"` (schema 54).
- 2.57 is refused.

Protocol 2.59 adds live input bank thumbnails: a small, periodic picture of
every input, sent only to sessions that ask for it.

- `input_thumbnail_subscription` carries only `enabled=1|0`. It is not a
  command: it has no envelope, idempotency key, or revision, and nothing
  answers it on success. Only the peers that may receive `monitor_frame`
  (active non-Web Studio peers permitted to view status) may subscribe;
  any other peer gets `error` with code `input_thumbnails_unavailable`.
  Studio subscribes while its input bank is drawn and unsubscribes when it
  is hidden, and subscribes again after a reconnect. A subscription lasts
  only for its connection.
- `input_thumbnail` carries server identity, `input`, a strictly increasing
  `sequence` (one counter shared by all inputs), `origin=simulated|native`,
  `age_ms` (how long the picture has been unchanged, `u32`), `width` (1–160)
  and `height` (1–90), and `data`: one standard QOI stream (three channels,
  sRGB, exact end marker) of at most 40 KiB raw bytes as canonical unpadded
  base64url, so a record always fits one line. A client treats a foreign
  server identity, a sequence that does not increase for that input, or a
  stream that fails strict QOI decoding at the stated size as a protocol
  violation and disconnects. Clients keep only the latest decoded picture
  per input (at most 256 inputs), only for inputs the replicated project
  still has, and clear them on unsubscribe and disconnect; a valid record
  still in flight after unsubscribing, or for an input just removed, is
  dropped rather than treated as a violation.
- The record is lossy, latest-wins, never durable, never resumable, and
  never projected into snapshots. The daemon renders thumbnails on a
  dedicated worker thread, never on the session loop: each input with a
  picture is fitted inside 160x90 (falling back to 128x72 if the QOI stream
  would exceed the byte cap), graded with its input grade, re-rendered at
  most every 500 ms, and sent when it changed or every 2 s as a keepalive,
  plus once for every input when a peer subscribes. Each subscribed peer
  has a latest-wins slot per input and a 256 KiB/s token bucket (one line
  of burst); pending inputs are sent round-robin, so a slow peer gets
  fewer, newer pictures and never delays Program, monitor frames, or
  control replies.
- What a thumbnail depicts: a native daemon samples every live source it
  holds (network streams, capture devices, media files, titles, and
  simulated sources) from its latest CPU frame, and composes its Scene
  inputs at the thumbnail size from those same frames as an exact scene
  preview does (`origin=native`). A simulated daemon (`origin=simulated`)
  renders simulated inputs, titles, and scenes as its monitor tap does,
  shows network-stream and capture inputs as a synthetic tinted card with
  a moving band, and media inputs as a card whose bottom bar follows the
  playhead. A picture with transparency is shown over black, as Program
  shows it alone. Inputs with no picture, such as system audio, get no
  thumbnail (and are tried again at most every 500 ms).
- 2.58 is refused.

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
