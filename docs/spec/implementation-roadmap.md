# Step-by-step implementation roadmap

[Specification index](README.md)

## 1. Program expectations

Full vMix Pro/Max-class parity is a multi-year broadcast product program, not a
single MVP. A realistic broad-parity effort is roughly 12–18 experienced
engineers over 30–42 months, plus product/design, QA/hardware lab, release,
security, legal/licensing, and vendor relationships. A smaller team should keep
the same phase order but narrow certified devices and overlap less.

Phases ship vertical slices. Do not build every input before one end-to-end
camera-to-display-to-record path is reliable.

During current development, the repository carries one project schema, one wire
protocol, and one exact plugin ABI/state-snapshot version only. Breaking changes
replace those contracts in place: do not add schema or snapshot migrations,
protocol downgrade projections, plugin ABI ranges, legacy API facades,
compatibility fixtures, or backward-regression gates. Delete superseded paths
and update current-contract coverage. Version compatibility policy is outside
this development roadmap.

## 2. Phase 0 — feasibility and constraints (6–10 weeks)

Build disposable prototypes, not production abstractions.

1. Prove one 1080p60 camera/file frame can enter D3D12, Metal, and Vulkan wgpu
   paths, composite, display, and export to a hardware encoder.
2. Measure copy count, latency, GPU/CPU use, and device-loss behavior.
3. Prototype 48 kHz capture/mix/output on all three OSes.
4. Record and stream one hardware-encoded rendition with recoverable container.
5. Prototype engine/studio process separation and local shared preview.
6. Validate FFmpeg/GStreamer/vendor SDK redistribution and codec patent posture.
7. Acquire representative GPU, capture, audio, controller, and storage hardware.
8. Freeze the vMix 29 feature-parity ledger and tag every row with acceptance
   owner and applicable platform.

Exit:

- architecture decision records for media stack, GPU interop, wire schema, UI,
  and plugin ABI;
- measured prototype report on all Tier-1 OSes;
- approved licensing plan;
- no unresolved blocker to a two-frame-class HD path.

## 3. Phase 1 — foundation and simulated engine (8–10 weeks)

1. Create workspace, dependency policy, CI, `xtask`, and binary skeletons.
2. Implement `fm-types`, IDs, formats, time/rates, color/audio metadata.
3. Implement the current project schema, journal, and atomic save.
4. Implement commands, events, revisions, idempotency, transactions.
5. Implement capability registry and compatibility report.
6. Materialize `parity.toml` from the ledger with phase, owning workstream,
   dependencies, platform/capability profile, and acceptance-test IDs; CI rejects
   missing or duplicate mappings.
7. Implement editable graph, validation, plan representation, bounded budgets.
8. Implement deterministic clock, fake video/audio sources/sinks, and scheduler.
9. Implement exact-current server handshake, snapshot/event resume, auth
   development mode.
10. Implement UI replicated model and a diagnostic client.
11. Add current-contract protocol/project integration and simulated end-to-end
    tests.

Exit: a headless simulated production can be controlled by CLI and client,
saved, restarted, resumed, and tested deterministically.

Current Phase 1 control boundary: the daemon, protocol, and client exercise a
versioned raw TCP session with handshake, snapshot/resume, commands, events,
development authentication, bounded bidirectional heartbeat acknowledgement,
and deterministic model tests. Protocol 2.13 added one bounded, read-only
control-plane diagnostics query, authorized by `ViewStatus`; it reports the
point-in-time control diagnostics and does not mutate engine or durable state.
It is not telemetry, health, or readiness, and RC-015 remains planned.
Protocol 2.14 adds the ordered configured output ID/name roster required for
exact overlay-output inclusion controls. The diagnostic client now reports that
roster and the authoritative ordered input ID/name roster from its validated
snapshot. This is control inventory only, not output delivery or service
readiness; RC-008 and RC-015 remain planned.
Protocol 2.15 defines bounded fixed-point Master and per-input meter records;
`fm-client` validates identity and monotonic sequence without changing the
durable cursor. Native `freemixd` publishes the latest completed frame to
active Studio peers through one bounded, lossy scheduler slot. Studio consumes
one validated latest record and displays compact fixed-point peak/RMS values;
native-media acceptance is not proved.
Protocol 2.12 retains the protocol 2.10 rule
that the daemon acknowledges only a validated heartbeat with the session server identity, the exact client
heartbeat sequence, and the server receive time. Studio accepts one expected
sequence and uses its bounded peer wait before the existing reconnect backoff.
Incomplete pre-handshake peers expire on the same absolute heartbeat deadline.
The one-shot CLI has finite connect/read/write deadlines and accepts only exact
newline-terminated records up to 64 KiB.
While the CLI waits for a command, it applies valid peer events but still
requires the accepted revision's durable event and realized runtime event.
Studio diagnose checks one bounded raw-TCP heartbeat acknowledgement and keeps
waiting when validated durable or runtime state arrives first, then reports
validated point-in-time control diagnostics. RC-015 remains planned.
The default simulated daemon cooperatively stops after Unix SIGINT/SIGTERM, Windows Ctrl-C, or a post-readiness `--diagnostic-stop-after` deadline through bounded listener polling; this does not complete RC-008 or RC-015.
Simulated CLI PPM output uses same-directory write/sync/replace, not broader media or disk certification.
Local CLI loads reject unapplied journal batches without recovering or mutating journal state; torn-final-only state remains readable.
This is control-plane peer liveness. It is not service readiness, production
authentication, media health, HTTP resources, WebSocket event subscriptions,
or transport-level production rate limits. Native and non-native raw TCP now
share the bounded single-thread scheduler: an idle Viewer can receive live
events while an Operator controls the show, and native mode remains limited to
one peer. Native-media acceptance is not proven on this Mac. This is
prerequisite transport infrastructure only; AU-001 daemon and Studio meter
transport remain planned. It is not HTTP/WebSocket/service/media readiness or
RC-008 completion. Expired raw TCP sessions are reclaimed. Phase 1 also
contains an optional loopback-only WebSocket control listener at `/v1/control`,
gated by `FREEMIXD_WEB_TOKEN`; it relays exact-current wire records through the
same bounded scheduler and admits at most one Web peer. This is transport
groundwork only: Web peers do not receive audio meters, and RC-008 remains
planned until process and browser acceptance evidence exists.
Native Studio command, event, and reconnect flows can now use the same bounded
loopback WebSocket transport and current-session reducer. Native Studio
one-shot diagnose uses this transport and validation. RC-008 remains planned;
this does not prove browser, TLS, production authentication, service/media
readiness, general multi-peer subscriptions, or meters.

## 4. Phase 2 — GPU playback switcher (10–14 weeks)

1. Build wgpu context, resource pools, shader validation, render graph.
2. Add image, color/bars, and file playback through the codec adapter.
3. Implement preview/program, Cut/Fade, frame-boundary command realization.
4. Implement transforms, crop, opacity, layers, and basic color conversion.
5. Build native studio shell, input tiles, Program/Preview, transition controls.
6. Present local previews without CPU readback where possible.
7. Add planar float audio playback, Master bus, meters, gain/mute/follow-video.
8. Add fullscreen output and one fault-tolerant program recorder.
9. Instrument latency, queue depth, frame drops, and GPU time.
10. Pass the first one-hour Basic Show scenario on each Tier-1 OS.

Current implementation boundary for item 4: `fm-compositor` executes bounded
native `CompositionPlan` layers with crop, nearest scaling, quarter-turn
rotation, translation, opacity, stable z-order, premultiplied source-over, and
canonical RGBA16F inputs. It also executes hard-edged rectangular masks in
half-open post-crop source space, with exact CPU-oracle coverage and ignored
native Metal readback coverage that remains opt-in and is not claimed without
an adapter. The current schema persists each scene background and each layer's explicit
input/scene source, geometry, crop, optional hard-edged rectangular mask,
opacity, z-order, and ordered reference-effect stack. Mask
bounds are validated against the effective
post-crop source dimensions before native planning; inversion does not change
planner capacity or transient-resource accounting.
`InputKind::Scene` explicitly routes a `SceneId` plus an optional audio
`InputId`, while every persisted output explicitly routes a video `SceneId` and
audio `BusId`; outputs are preserved as declared and are not inferred. Native
`freemixd` now consumes this visual model,
including the persisted rectangular mask, through the bounded scene planner
described under Phase 3 item 4 instead of rejecting scene inputs. Feathered or
non-rectangular masks, keys, effects, per-output realization, live
edits/replanning, and cross-platform or hardware certification remain outside
this item, so its parity rows remain planned; the bounded local CLI can
construct, route, locally name, set startup policy for, and remove buses and outputs
for the next-open project only, without runtime output realization; SW-012 remains planned
for runtime start/reconcile, output delivery, clean feed/fullscreen/health/recovery, and live acceptance.

Current implementation boundary for item 5: `freemix-studio` opens a native
`eframe`/wgpu shell by default with responsive Program/Preview monitor wells,
stable-ID input tiles, realized/desired tally, and permission-gated Cut/Fade/Wipe
controls. Manual Fade/Wipe/AlphaFade/Slide T-bar controls require transition permission,
synchronized state, and Ready lifecycle. The manual panel displays
replicated desired and realized kind, routing, and exact integer basis-point
position. Bounded worker channels preserve order across retained non-coalesced action
and input boundaries while recovering, and later retained intents cannot overtake
unresolved commands. Native Studio locally retains bounded unsent intents and uses
latest-value coalescing for adjacent manual-position values and same-input audio-strip
field edits; this can suppress intermediate unsent manual or audio effects.
`fm-client` retains a
bounded terminal command history (256 by default, configurable through 65,536),
while replay-receipt collisions mark affected sent commands terminal-uncertain,
force authoritative snapshot resynchronization, and remain visibly sticky in
Studio's bounded eight-entry ledger until Studio is restarted.

Blocking TCP and daemon supervision remain off the render thread. The worker
reconnects after bounded backoff, negotiates durable resume or an authoritative
snapshot, resumes unresolved command sequences, and requests a snapshot when
runtime realization becomes uncertain. TCP establishment is finite, using one
attempt of up to the configured timeout, but cancellation is checked only
before and after that attempt. Supervisor readiness and connected protocol
read, write, and flush waits are polled and cancellable; deferred intents are
capped, and supervised daemon shutdown or restart performs bounded
process-group/job and descendant cleanup. The supervised stdout readiness record
is byte-bounded; this does not add service/media readiness or RC-008 completion.
Studio UI state is latest-state and
cannot block control I/O; this adds no preview delivery, service readiness, or
transport support, and RC-008 remains planned. Studio Program/Preview wells display
replicated desired and realized routing input names and, when available, the
latest encoded monitor frame described under item 6; control state alone is not
output-video proof.
An owned supervised daemon exit detected while Studio is idle enters the existing
bounded reconnect/restart path; protocol mismatch, restart-limit, and fatal supervisor
errors stay terminal, and this does not complete item 5 or RC-008 or prove service, media, or cross-platform readiness.
`freemix-web` likewise declares only the current protocol in its client configuration.
Its transport-free semantic presentation model preserves the existing
permission- and protocol-gated Cut/Fade/Wipe controls and adds manual Fade/Wipe/AlphaFade/Slide
Start, exact basis-point Position, Commit, and Cancel controls. Manual controls
derive availability from Ready state, transition permission, and separate authoritative desired and realized
manual projections. It also models protocol-gated FTB actions and exact state.
Web still has no browser renderer or network runtime, and the remaining
transition families plus cross-platform output acceptance keep item 5 and
RC-007 planned.

Current implementation boundary for item 6: `fm-frame` defines a bounded,
portable local-preview contract for shared-image versus encoded fallback,
stream/engine/adapter identity, image metadata, opaque handle references,
ready/release synchronization, monotonic frame and resize generations, exact
release acknowledgement, timeout reclamation, and client-death reclamation.
Outstanding resources and handle references cannot be reissued before release.
This is lifecycle groundwork, not preview delivery. The daemon still owns
non-shareable opaque wgpu 30 textures in a separate process, Studio currently
renders through eframe's wgpu 29 device, no authenticated native-handle/fence
IPC exists, and the engine produces no separate Preview image tap. A platform
bridge plus output color transform, bounded shared ring, sidecar IPC, and
encoded fallback are required before item 6 can claim presentation without CPU
readback.
Protocol 2.21 adds the encoded fallback only: a `monitor_frame` record carries
a per-tap, strictly sequenced Preview or Program image of at most 640x360 as a
std-only QOI stream split into at most 32 canonical base64url parts written as
one atomic group, published latest-only to active raw-TCP Studio peers with
`view_status`. The simulated daemon renders both realized taps from simulated
sources at 5 fps or immediately on a routing change, without overlay, scene,
or Fade-to-Black compositing; the native daemon reads back Program on the CPU
at up to 10 fps and still has no Preview tap. Studio letterboxes the latest
frame, labels simulated and native frames, and dims stale or mismatched ones.
There is no subscription or cadence negotiation, Web peers receive no frames,
and shared-image presentation without CPU readback remains absent, so item 6
and `RC-008` remain planned.

Current implementation boundary for item 7: `fm-codec-ffmpeg` provides a
transactional sequential local-audio cursor with global block sequence,
sample-contiguous timing, sticky EOS, and explicit per-page operation
block/sample/byte limits. Cursor progress no longer spends prior pages against
those limits, and bounded metadata-only positioning can skip complete blocks to
a restored sample without decoding their PCM prefix. The sequential cursor now
keeps a source-fingerprint- and stream-bound metadata suffix with cumulative
sample offsets plus periodic exact checkpoints. Each extension asks ffprobe to
resume at an absolute checkpoint PTS, accepts it only after the checkpoint and
retained overlap reproduce exactly, and uses a packet budget bounded by the
checkpoint interval, page size, and fixed preroll slack. Retained records,
estimated bytes, checkpoints, and resume attempts have explicit limits and
expose probe, packet-budget, reuse, recomputation, eviction, and invalidation
telemetry. This makes metadata discovery amortized O(total blocks) on demuxers
that honor a reproducible interval seek. FFprobe does not guarantee that for
every container: when none of the bounded retained checkpoints can be
reproduced, the operation returns incomplete metadata transactionally rather
than restarting an ordinal-sized prefix scan. PCM decoding separately uses a
bounded from-start correction or a fixed validated seek-anchor stack; source
changes and process failures leave both cursor and index content coherent.
Native restore still allows at most 4,096 skipped blocks, and fixed
metadata-output/decode bounds plus subprocess timeouts can reject deep playback
or restore transactionally.

`fm-audio` provides a deterministic reference Master with planar F32 mapping,
gain/stereo-balance/mute/solo/follow-video, meters, timed canonical blocks,
sample-count timing validation, and transactional gain and balance ramps.
Allocating reference outputs include deterministic per-input post-strip peak
and RMS readings in `InputId` order after delay, channel mapping, operator
gates, gain, balance, and transition gain. Silent configured strips remain
visible with zero readings. An opt-in allocation-free planar render API writes
Master and per-input readings into exact-size caller-owned flat buffers without
allocation in the mixer. The native daemon uses those buffers and publishes one
latest-only Protocol 2.15 sample to active Studio peers. The Master reading is
before clip-local Stinger audio and Fade-to-Black. Studio does not show it.
Its bounded
`ClockMappedAudioSynchronizer` is now connected to native `freemixd` local-file
audio. The daemon accepts source rates such as 44.1 kHz and linearly resamples
them to the 48 kHz project Master while preserving absolute source and Master
cadence origins. Master intervals derive directly from absolute engine frame
numbers, including deep restored cursors and fractional video rates. Initial
positioning compares audio and video on their relative media timeline: early
audio is trimmed and delayed audio produces bounded leading silence. Every
decoded source, including an inactive one, advances on every Master interval so
later switching does not replay stale audio.

The current schema also persists one exact per-input audio strip as bounded
integer milli-dB gain (`-96000..=24000`), stereo balance
(`-10000..=10000` basis points), mute, solo, follow-video, and a bounded
0–48,000-sample delay. Native daemon preflight transactionally maps those
records to `fm-audio::Gain` and `fm-audio::Balance` and constructs both Master
mixer copies with the target gain, balance, and delay applied immediately.
Protocol 2.12 carries authoritative input IDs and canonical names plus full-strip
status in snapshots. It adds an EditProject-authorized input rename command and
a small durable rename event without changing the snapshot shape. Other durable
events carry full-strip state plus one permission-gated atomic live strip command. The engine schedules
accepted gain, balance, mute, solo, follow-video, and delay changes together at a frame boundary, and
native realization updates active and pending Master/Stinger mixers and the
checkpoint project atomically. Live gain and balance changes then ramp linearly
over 240 samples (5 ms at the 48 kHz project Master rate) while mute,
solo, follow-video, and delay take effect at the next rendered sample; restored strips
remain immediate. Balance is linear after channel mapping: negative values
attenuate the right destination, positive values attenuate the left destination,
and non-stereo destination labels remain at unity.
In the default `sip` solo mode (see the `AU-001` boundary for PFL/AFL), solo is solo-in-place across the logical Master strips: if any strip is soloed,
every non-soloed strip is gated. Mute and follow-video remain independent gates,
so a muted or video-inactive soloed strip can intentionally produce silence.
Local and remote CLI commands, Studio controls,
and the Web semantic control model expose the same bounded mutation. Restart
restores engine desired state from the canonical strips and checkpoints it back
to the project. Studio resolves coalesced adjacent audio field edits against the latest
confirmed strip before dispatch. Current-contract persistence, generated-audio, AFV
selected/inactive, mute, solo, immediate startup gain/balance/delay, live full-strip realization,
and failed-preflight no-partial-state tests cover this slice.

Studio input tiles and mixer strips use exact persisted input names carried by
the snapshot rather than generated ordinal labels. The client reducer validates
each rename against the current input set, the 128-byte limit, and exact name
uniqueness, then changes only the matching name at the durable revision. It also
carries an authoritative full input reorder through the EditProject command and
durable input-order event; the daemon persists that vector order and restores
the order after restart. Studio now has a compact permission-gated rename editor.
The local `input-add`, `simulated-solid-input-add`, `media-input-add`, `scene-input-add`, `scene-input-duplicate`, `input-duplicate`, `input-remove`,
`input-replace-simulated`, and `input-replace-solid` commands persist deterministic input state for the next project open only. Both replacement commands
have no live, native, protocol, or IN-026 acceptance; IN-026 remains planned. `scene-input-add` creates an empty opaque-black scene with no layers and no
audio source. `scene-input-audio-source` and `scene-input-audio-source-clear`
persist the scene input audio route. `inputs` reports each Scene input's exact `scene_id` and configured `audio_source` ID or `none`; it provides no render or live proof. `scene-layer-add`, `scene-layer-remove`, `scene-layer-duplicate`, `scene-layer-copy`, `scene-layer-rename`, `scene-layer-appearance`,
`scene-layer-geometry`, `scene-layer-crop`, `scene-layer-crop-clear`,
`scene-layer-mask`, `scene-layer-mask-clear`, `scene-layer-z-order`, and
`scene-background` (premultiplied RGBA), `scene-layer-source-input`,
`scene-layer-source-scene`, and `scene-layer-add-scene` are next-open local
authoring commands. The offline CPU/PPM path renders a settled Scene Program
from direct simulated layers only. It maps background, enabled layers,
translation, scale, quarter rotation, crop, hard rectangular mask and invert,
opacity, z-order, and layer vector tie-order. It rejects nested scenes,
enabled non-simulated layers, and any transition with a Scene endpoint before
PPM publication. It does not provide keys, effects, audio, output, live,
native, or cross-platform certification. `scene-layer-copy` is next-open local
cross-scene authoring only and rejects cycles; `IN-020` and `SW-006` remain
planned. `scene-layer-move` changes next-open layer vector tie-order only;
`SW-004` remains planned.
`scene-remove` is local next-open scene removal only; it does not add cascade
behavior, rendering, live editing, daemon/Studio/protocol mutation, output
realization, or acceptance.
`scene-input-remove` removes a local next-open Scene input and its linked Scene only when external references reject; it adds no cascade, live, or protocol acceptance, and IN-020/IN-026 remain planned.
`scene-rename` preserves the exact supplied name, rejects blank names, and
enforces case-insensitive uniqueness; it adds no daemon/Studio/protocol/live-session,
render, or output behavior.
`project-rename` is local next-open name metadata only; it does not affect project identity, runtime, protocol, or output behavior, and `RC-012`/`IN-026` remain planned.
The rename and reorder commands carry input identity; audio and routing state
remain separate.
`media-input-add` and `media-input-relink` are local next-open media-input configuration footholds. They store strict
`asset://` URIs without requiring the assets to exist; native decode, live add/relink,
offline lifecycle, batch import, Studio, remote acceptance, and IN-008/IN-026 remain planned.
`input-replace-media` and `input-replace-scene` are local next-open source replacement only; scene replacement uses an existing scene and rejects scene/input graph cycles. They add no live, render, or protocol acceptance, and IN-020/IN-026 remain planned.
`simulated-solid-input-add` is a local next-open simulated solid configuration foothold with
straight RGBA bytes; live add, Studio/remote control, native acceptance, and IN-008/IN-026 remain planned.
Neither command applies an optimistic change or changes current switcher state.
Studio exposes compact Up/Down controls
for one-step reorder, gated by the current Ready session and EditProject
permission; the native worker resolves each move against the latest confirmed
order and the daemon remains final authority. Live duplicate, categories,
colours, pause, close, offline, source replacement, and batch import are
described in the `IN-026` boundary below.
Studio now has a compact permission-gated gain fader with exact milli-dB entry
and a latest-only compact peak/RMS meter display for Master and current inputs.
Strip labels, pan, and VCA groups are described in the `AU-001` boundary
below, including the PFL/AFL monitor bus. There is still no microphone automix,
device-audio path, device-clock correction, native EQ/gate/compressor/limiter,
or acceptance evidence. Local offline audio-bus send authoring and rename are
next-open configuration only; they do not change IDs, sends, output routes,
runtime state, or live audio, and `AU-002` remains planned. Phase 2 item 7
is partial, Phase 3 item 3 is partial, and
`AU-001`/`AU-007` remain planned.

The worker and synchronizer retain bounded blocks, samples, and bytes. Refills
reserve capacity before nonblocking dispatch, prioritize uncovered sources, and
commit a completed batch only after every returned page validates. EOS silence
is staged and preflighted across all affected sources before any cursor changes;
render preflights every source commit and sink admission before advancing the
absolute frame cursor. Synchronizer/source PCM buffers, per-terminal render
planes, and EOS staging scratch are preallocated. The canonical returned block
and bounded fake-sink clone still allocate per frame; event-bearing
`FrameResult`s and other control-path allocations can also remain. Exact source
channel layout is enforced across pages and must map by matching labels to
Master; there is no channel conversion.

Scene inputs recursively route audio through explicit `audio_source` links to a
physical leaf or explicit silence. Cut keeps one source at unity; Fade, Wipe, AlphaFade, Slide, and Zoom
crossfade two sources with sample-linear gains from each interval's explicit
start/end mix endpoints. Physical terminals render once per interval, but
distinct logical strips that share one terminal remain independent mixer
submissions with their own gain/balance/mute/solo/follow-video state and transition
coefficients. Truly identical logical Program IDs submit once at unity.
Automatic Fade and held or reversed Fade, Wipe, AlphaFade, and Slide T-bar movement propagate exact
endpoints. The manual-transition core is exposed through `EngineCommand` and
the current protocol, and the current schema preserves exact desired and realized
manual state across replay-safe daemon restart. The CLI exposes local and remote manual Start, exact integer
position, Commit, and Cancel commands; its local path restores and saves the
current engine state. Studio exposes permission-gated manual Fade/Wipe/AlphaFade/Slide
controls while Ready. Web now has only a current-contract transport-free semantic
manual-control model; it still has no browser renderer or network runtime.
Missing local audio, stills, scene silence, and
configured simulated silence produce silence; unsupported simulated sine audio
is rejected. At decoded EOS, the daemon synthesizes continuing silence for
subsequent Master intervals. It exposes no media-completion/end trigger and has
no stop, loop, or operator-selectable EOS policy.

`FREEMIXD_TELEMETRY` v4 reports current and observed-peak retained
blocks/samples/bytes; reservation requests and current/peak reserved
blocks/samples/bytes; source stalls; positioned blocks/samples; leading-silence
samples; EOS-padding blocks/samples; and fake-sink current depth, peak depth, and
drops. Evidence is split by media type. The simulated-silence
`phase2_native_diagnostic_soak` exercises Master cadence, fake-sink pressure,
checkpoint/restart, and telemetry only; it does not exercise decoded-audio
resampling, refill, positioning, or EOS. Dedicated ignored integrations
separately cover daemon refill/checkpoint
(`native_media_daemon_refills_beyond_startup_prefix_and_checkpoints_once`),
44.1-to-48 kHz recording
(`native_44_1k_local_audio_resamples_to_48k_master_recording`), and deep restored
FFmpeg positioning/resampling
(`deep_restored_44_1k_nonzero_pts_positions_without_pcm_prefix_and_preserves_phase`);
deterministic native-runtime tests cover EOS padding. The path remains a
fixed-clock linear diagnostic resampler feeding a fake sink: there is no drift
estimator, device clock or OS audio device, channel conversion, persisted
buses/output routing or strip controls, DSP, or externally delivered audio.
Dedicated Wipe audio-policy tests cover exact interval endpoints, sample-linear
rendering, unity Cut completion, and held, reversed, and committed manual T-bar
movement. A protocol-driven macOS/Metal recording acceptance now covers manual
video progression, reversal, cancel, and commit, but does not distinguish
T-bar audio sources or certify other platforms and outputs. The Web runtime
operator path also remains incomplete. Item 7 and the related parity rows
therefore remain incomplete and planned.

Current implementation boundary for video-file and growing-file playback
(`IN-003`): `fm-playback::MediaPlayback` is a backend-independent rational
transport. Its playhead is an exact source-frame fraction advanced per output
tick by `rate x source_rate / output_rate`, so 0.25x-4x rates drop or
duplicate frames by one floor rule and differing source/output rates never
drift. It owns play, pause, resume, stop, exact frame and rational-time seek,
inclusive-in/exclusive-out trim, looping of the trimmed (or whole) range, end
actions (stop and cue, hold last frame, loop) with `Ended`, `Looped`, and a
configurable lead `EndTrigger` event, audio-track selection by stream index
with mid-play switching at the exact sample, and an audio speed policy (mute,
or linear varispeed of exactly the covered source span; normal-rate audio is
sample-contiguous). A growing source exposes `available - live_edge_offset`
frames, waits (holding the last frame) at that live edge instead of ending,
resumes without skipping when more frames arrive, and refuses a shrinking
frame count. `fm-codec-ffmpeg::FilePlayer` drives it over one local file: a
bounded ffprobe packet scan builds a sorted presentation-timestamp and keyframe
index (constant frame rate is required), each video window seeks to the
keyframe at or before the target, decodes forward with `-copyts`, selects by
exact timestamp, and accepts output only when a `showinfo` side chain reports
exactly the indexed timestamps (retrying from earlier keyframes, then from the
start). Audio windows trim by exact sample timestamp and verify the first
sample with `ashowinfo`; accuracy is bounded by the container's audio
timestamps (sample-exact for NUT/MP4/WAV, +-0.5 ms in Matroska). Growing files
are followed by size polling and full index rescans; a smaller file, a
different file at the path, changed leading bytes, or changed indexed
timestamps are refused with typed errors, and a complete source must not
change at all. Acceptance (`accept-in-003`) asserts frame numbers from decoded
pixels and sample indices from decoded ramps for Matroska/NUT/MPEG-TS with
B-frames and 12-frame GOPs, and plays an MPEG-TS that is appended in three
segments while playing.

The transport is wired end to end. Schema 36 gives every media-file input
persisted `playback` settings: trim in/out, end action (stop, hold, loop),
end-trigger lead, speed (2500-40000 basis points), audio speed policy,
optional audio stream, a growing flag with live-edge offset and idle
completion count, and a start frame inside the trim range. The engine owns
one transport per media input, ticked once per engine frame; transport
commands (play, pause, stop, frame or rational-time seek, complete source)
are operator live operations and settings commands are padlock-honouring
project edits. Protocol 2.37 carries them as `media_*` commands, the
`media_changed` event, one `media_state` follow-on record per media input
after a snapshot, and `media` runtime events (seeked, end trigger, ended,
looped, live edge reached, resumed, source grew/completed, audio track
changed, faulted, plus a once-a-second position sample); `FilePlaybackError`
maps to typed input faults. An end trigger of the input on air in a playing
playlist journals a playlist `next`, which replays after a restart. The
simulated daemon drives each input over a synthetic 10-second source (two
audio tracks) in wall time. The native daemon (`--features native-media`
with `--native-media`) runs one `FilePlayer` per media input on its own
worker thread at the program rate: the worker presents the engine's tick
(`FilePlayer::present`) and decodes the next window ahead (`prefetch`), the
render thread never decodes, a late picture holds the previous frame and is
counted (`FREEMIXD_MEDIA_FILES ... late_frames`), video replaces the input's
live frame in the compositor, and audio is pushed per frame into the Master
mix (resampled linearly to the frame's sample count and channel-mapped, not
through the per-source decode synchronizer the other inputs use). Native
media files must match the project size and carry colour tags, as native
import already requires. The CLI edits settings offline
(`input-media-trim|rate|end-action|end-trigger|audio-track|audio-policy`,
`input-media-list`) and drives a running daemon
(`remote-input-media-<operation>`); Studio's Media tab gives each media
input a transport bar (play, pause, stop, finish for a growing file,
scrubber, trim handles, 0.25x-4x speed menu, end action, audio-track menu
with language names, live and waiting-for-data badges) with English and
Spanish hover help. Acceptance adds model, persistence, protocol, engine,
client, CLI, simulated daemon process, and Studio worker coverage, and
`freemixd --test native_media_files` decodes a Matroska fixture with two
tracks end to end when built with `--features native-media`
(`FM_REQUIRE_FFMPEG=1` and `FM_REQUIRE_NATIVE_MEDIA=1` make missing tools a
failure). A native daemon also takes media files added live (batch import,
duplicate, source replacement, and undo/redo): the worker opens the file
off the render thread and the input airs once it reports it; a file that
does not open, does not match the project size, or lacks colour tags
faults its input instead of stopping the daemon (`freemixd --test
native_live_inputs`). There is still no persistent decoder process (each window is a
bounded tool run), no reverse play, no variable-frame-rate source support,
no pitch-preserving time stretch, and no certification on the Windows,
macOS, or profile-a-hd profiles, so `IN-003` remains planned.

Current implementation boundary for the input lifecycle (`IN-026`): schema
37 adds a project input category list (at most 32, unique nonzero ids,
unique case-insensitive names of at most 48 bytes) and a per-input
lifecycle (category, one of eight colour labels, paused, closed), stored
only for inputs that differ from the default, validated against the inputs
and categories, and dropped with the input. The engine owns the same state
plus each input's replaceable source (generator, media asset, device, or
network locator) and executes duplicate, replace source, category, colour,
pause, close, category list, batch import, and multi-input removal as
padlock-honouring, undoable project edits (an import or duplicate is undone
by removing exactly the inputs it appended; redo appends them again). Batch
import plans each item in order and refuses items individually. Protocol
2.38 carries them as `input_*` commands, `input_lifecycle_changed`, the
snapshot's per-input lifecycle and category list, and
`input_availability` runtime events; the client reducer replicates all of
it. A paused input holds its frame (the media transport stops while keeping
its play state) and a closed input releases its source: the simulated
daemon reopens a synthetic source when the input reopens, and the native
daemon stops the media-file worker's decoder on close (the input then shows
transparent black and no sound) and reopens the file on reopen. A closed
input is never taken to Preview or air (`input_closed`), directly or as a
scene layer, and an input that an on-air scene shows cannot be closed. The
native daemon starts without opening closed inputs' files, starts an input
whose file is missing or unplayable offline, faults an input whose worker
fails, and downmixes media-file audio onto the Master layout with the
ITU-R BS.775 matrices. Media faults map to offline reasons, published once and cleared
when the input plays again. `freemixd` validates an import or a media
replacement first, then copies the host media paths the command keeps into
the bundle before executing and journaling it (removing any copy it does
not keep). The native daemon follows add, duplicate, import, removal,
multi-input removal, source replacement, and their undo and redo live:
after the command commits and before the next frame it retires every
removed input (or replaced source) from the source registry and the Master
mixer, stops its media-file worker off the render thread, and starts every
new one on a transparent black picture and silence until its worker
delivers (a generator renders its picture at once; a media file opens and
decodes on its own worker, which reports the file, or its fault, through
the input's status; a still decodes on its own thread; a network stream's
receiver connects; a Scene input only recompiles the native project plan).
Before staging, the native runtime vets the catalogue a command would
leave and refuses, typed `runtime_unsupported`, kinds it does not realize
live (capture devices, titles, colour, alias, and Program/Preview feed
inputs, a generator test tone, and a change to a Stinger clip input it
decodes since startup), and, typed `bounds_exceeded` on `inputs`, a
catalogue past 64 native inputs or the retained GPU frame budget. The CLI edits categories, colour, pause, and close locally through
the engine (`input-categories`, `input-category`, `input-color`,
`input-pause`, `input-close`), imports a file list locally
(`input-import-batch`), duplicates with labels (`input-duplicate`), and
drives a running daemon with `remote-input-duplicate`,
`remote-input-replace`, `remote-input-categories`, `remote-input-category`,
`remote-input-color`, `remote-input-pause`, `remote-input-close`, and
`remote-input-import-batch`. Studio shows a colour strip and offline,
paused, and closed badges on each input tile, filters the input bank by
category chips, and offers Duplicate, Replace source, Category, Colour,
Pause/Resume, and Close/Reopen in the tile menu plus a batch import path
list, with English and Spanish hover help. Acceptance (`accept-in-026`)
adds model, persistence, protocol, engine, simulated daemon process, native
media-file and live input catalogue (`--features native-media`,
`freemixd --test native_live_inputs`: a colour, an imported clip, a
duplicated scene, and a loopback network stream added, cut to Program,
heard on Master, undone, redone, and removed while the daemon runs), CLI,
and Studio hover-audit coverage. Capture devices are not paused, released,
added, or removed natively (camera capture is macOS-only and the device
graph is fixed at startup), title inputs cannot be added live natively,
device and
network sources report no availability yet, there is no relink-on-offline
workflow beyond `project-relink`, and no certification on the Windows,
macOS, or profile-a-hd profiles, so `IN-026` remains planned.

Current implementation boundary for item 8: `fm-gpu` provides a portable,
bounded latest-frame presentation policy plus opaque, context-bound native
surface configuration, acquisition, submission, presentation dispatch, and
same-context recreation. Configure/acquire/present validation is captured
without exposing native handles; a failed configure poisons that surface instead
of retaining stale state. `fm-color::NativeSdrOutputTransform` writes canonical
premultiplied linear BT.2020 RGBA16F directly into non-sRGB BGRA8/RGBA8 surface
targets as opaque, hard-clipped sRGB Rec.709 with black aspect-fit bars and no
CPU readback. CPU oracles and explicitly invoked ignored Metal tests cover
Rec.709, Display-P3, and BT.2020 source primaries across sRGB, BT.709, and
BT.1886 transfer functions, over-range and fractional-alpha pixels, and
two-dimensional fitting. With the
opt-in `macos-program-surface` feature and `--fullscreen-program`, `freemixd`
owns one borderless macOS Program window on its main-thread winit loop while one
worker retains all media and GPU submission. `--fullscreen-display` selects a
zero-based deterministic monitor entry. The selected winit monitor identity is
retained across inventory changes; removal suspends and hides output, reconnect
retargets it, resize updates coalesce, and surface loss has one
generation-correlated same-context recreation path. An ignored generated-bars
subprocess test requires at least one validated queue-presentation dispatch.
This remains a single macOS/Metal diagnostic output, not display certification:
the display choice is not persisted, physical scanout and visible pixels are not
observed, real unplug/replug and induced surface loss are policy-tested rather
than hardware-induced, and no second output, HDR, exclusive mode, or
cross-platform surface adapter exists. `fm-record` provides a private
CRC-protected packet spool with bounded queues and records, durable action
receipts and directory publication, segmentation, corruption isolation,
validate-before-repair torn-tail recovery, and a bounded subprocess-kill test.
It is not the playable recorder. `fm-codec-ffmpeg` owns a separate bounded
startup-only recorder that pairs tight SDR RGBA8 Program readback with the exact
planar F32 Master block, sends both over authenticated loopback HTTP inputs to
FFmpeg, and writes fragmented MP4 with H.264/libx264 and AAC into a caller-owned
exclusive empty file. Queue admission, retained bytes, connection/no-progress,
stop/kill, child reaping, output flush/sync, and sticky failures are bounded. A
paced pre-readiness barrier requires both a completed raw pair and observed mux
output; the first capture, backend, readback, frame, or enqueue failure emits a
sanitized immediate `FREEMIXD_RECORDER_FAILURE` record before cancellation,
while the final `FREEMIXD_RECORDER` record remains the shutdown result with no
live telemetry, alerting, restart, or fault-tolerant recording.
`--record-program` enables this path only with
`--native-media`; Unix signals and Windows Ctrl-C stop acceptance, settle any
remaining Fade control state without rendering shutdown-only frames, checkpoint,
and finalize without publishing false runtime-realized events. Separate
required-tool macOS integrations start recording from an unaligned restored
frame and decode the resulting H.264/AAC file, while a generator integration
covers signal shutdown during a 3,600-frame Fade. Protocol 2.18 makes the
configured recorder runtime-controllable: field-free `record_start` and
`record_stop` commands carry an engine-owned desired flag authorized as a
transition operation, schema 20 persists the project's recording intent so a
restart reconciles against it, live stop drains the segment on a finalizer
worker with its full `FREEMIXD_RECORDER` report, and live start derives the
next segment deterministically from the configured path
(`<stem>-NNN.mp4`, `002..=999`, skipping existing names) before re-running the
bounded startup pair barrier. This remains a diagnostic recorder: Program capture uses
synchronous GPU-to-CPU readback, encoding is software-only and fixed-format,
parent path traversal is not file-capability based, and there is no zero-copy
bridge, hardware encoder, disk-space status, second simultaneous recorder, or
cross-platform native evidence. Multi-file `fm-record` repair is necessarily
sequential after validation and assumes exclusive ownership of each recorder
directory. Item 8 and its parity rows therefore remain incomplete.

Current implementation boundary for item 9: `fm-gpu` presentation telemetry
now reports current and peak occupancy for its one-slot latest-frame queue in
addition to saturating accepted, presented, replaced, dropped, acquisition, and
recovery counters. Native contexts request `TIMESTAMP_QUERY` only when the
adapter advertises it and use a four-slot nonblocking readback ring to measure
real fullscreen render-pass duration. Unsupported adapters continue without
profiling; ring pressure or unavailable timestamp results increment saturating
counters instead of blocking or failing rendering. `fm-observability` exposes a
distinct bounded `gpu_time_ms` histogram rather than mislabeling CPU wall time or
GPU utilization. Opt-in native daemon mode aggregates up to 256 host-deadline
lateness and GPU-pass samples plus sampled current and observed-peak audio
retention, exact fake-sink and presentation high watermarks/drops, and sampled
recorder outstanding-pair/retained-byte pressure. Cooperative daemon teardown
emits one idempotent final `FREEMIXD_TELEMETRY` record with lifetime and retained
sample populations, p50/p95/p99 values, capability state, pending/lost samples,
and metric errors; native Metal and daemon tests require actual GPU samples where
supported. A forced fullscreen worker timeout cannot guarantee that final worker
diagnostic before process exit. Host lateness means time past the
native frame deadline immediately before realization, not capture or
glass-to-glass latency. GPU duration excludes queue wait, presentation,
compositor feedback, and physical scanout. There is no live telemetry protocol,
component-dimensioned metric registry, alerting/UI, CPU/GPU utilization, disk or
network instrumentation, cross-platform native evidence, or QR/audio-loop
latency fixture. The raw TCP heartbeat acknowledgement is not service readiness,
telemetry, or media health. Item 9 therefore remains incomplete and the broader
RC-015 parity row remains planned.

Current implementation boundary for item 10: headless native mode accepts a
bounded `--diagnostic-stop-after` duration after readiness and exits through the
normal cooperative checkpoint and telemetry path. The ignored
`phase2_native_diagnostic_soak` integration creates two small generated video
inputs with silent audio, alternates Cut and four-frame Fade commands, validates
accepted command targets plus durable and runtime events, and then checks
bounded telemetry, persisted receipts/routing/clock progress, and continued
rendering after restart. Because its audio is simulated silence, it exercises
Master cadence, the fake sink, checkpoint/restart, and telemetry, not decoded
resampling, refill, positioning, or EOS. Its default duration is 60 seconds;
environment flags can require native startup and GPU timestamps, and its report
is explicitly classified `diagnostic-not-certification` with
`basic_show_complete=false`. A required local 60-second macOS/Metal run rendered
all 1,500 requested frames, completed 59 commands, reported 1,333 lifetime GPU
timing samples, resumed to 1,575 total frames after a separate three-second
restart, and exposed 1,492 bounded fake audio-sink drops rather than hiding them.
This is useful lifecycle and pressure evidence, not the universal Basic Show
scenario. That scenario requires four cameras, two clips, a title, a browser
source, a two-box scene, streaming, and recording for one hour on every Tier-1
OS. Cameras and persisted scenes are scheduled for Phase 3, streaming for Phase
4, and title/browser inputs for Phase 5; no Windows/DX12 or Linux/Vulkan one-hour
evidence exists. Item 10 is therefore incomplete and is structurally blocked by
capabilities scheduled after Phase 2.

Current implementation boundary for fullscreen display outputs (`OR-011`):
`fm-io-api::display` defines portable display enumeration (`DisplayProvider`,
`DisplayInfo` with a platform display id, name, desktop origin, current and
advertised modes, and `HdrCapability`), a validated `DisplayOutputSet` of one or
two outputs (display selector by id, name, or index; source Program, Preview,
or a project output; `current` or exact mode; Fit, Fill, Stretch, or 1:1
scaling; `off`/`auto` HDR), and the `DisplayOutputTracker` hot-plug state
machine. Outputs bind by selector once, then follow their display by id and
then by unique name (rebinding a renumbered display), never jump by index, wait
with an explicit reason (absent, in use by the other output, ambiguous name,
enumeration failed), and reattach when the display returns; an unsupported
exact mode falls back to the current mode and is reported. `fm-types` owns the
four scaling placements as signed viewports and `fm-color` applies them in the
SDR output transform for surfaces and off-screen targets. `freemixd
--native-media --display-output <spec>` (repeatable, at most two;
`--fullscreen-program [--fullscreen-display N]` is shorthand for one Program
output) opens one borderless, cursor-hidden winit window per output under the
`display-surface` feature on every platform (`macos-program-surface` remains an
alias); each window has its own surface and presentation lifecycle on one GPU
context, presents its own source with its own scaling, uses exclusive
fullscreen only when an exact supported mode differs from the current one, and
emits `FREEMIXD_DISPLAY_OUTPUT` on every state change plus
`FREEMIXD_DISPLAY_OUTPUTS` at shutdown. Display ids come from
`CGDirectDisplayID` (macOS), the GDI device name (Windows), and the
`RandR`/Wayland output id (Linux). `--diagnostic-display-inventory FILE` presents
the same outputs headlessly into off-screen rasters sized to a file-described
inventory, which the native daemon test uses to prove two independent sources
(Program with an overlay, and a clean-feed project output) and unplug, replug,
and renumbered-display recovery on a software Vulkan adapter; a real Xvfb
window test covers the windowed path where a window system exists. The slots
are project configuration (schema 40 `display_outputs`: at most two, one
display per slot, source outputs that exist and cannot be removed while
shown); `--display-output` replaces the project's slots on load, and
`--display-presenter` opens windows for the project's own slots. Protocol 2.42
adds the padlocked, undoable `set_display_output`/`clear_display_output`
project edits, applied live (a source, mode, scaling, or HDR change keeps the
binding; a different display rebinds), the `display_output_changed` event,
per-slot `display_output_status` (active display, mode, mode fallback, HDR
state, or waiting reason) in runtime events and the snapshot, and the
`display_inventory` query and event (ids, names, origins, modes, HDR
capability). The native daemon composites Preview through the Program
compositor path (Preview's cut, live Fade to Black, no overlays or titles)
only while an active slot shows it, after the Program consumers, so
`source=preview` is accepted; a slot the windowed presenter has not applied
yet publishes no status until it has. Studio supervises its daemon with
`--display-presenter-if-available`, which opens the presenter in a
`display-surface` build and otherwise (or without a window system or GPU)
serves as before with the slots waiting. Studio's Output menu opens a Fullscreen outputs dialog (per slot
display from the inventory, source, mode, scaling, HDR, live status; English
and Spanish hover help covered by the hover audit), and `freemix-cli`
provides `display-output-set|clear|list` locally and remotely plus
`remote-display-inventory`. A daemon without a presenter (simulated or
headless) keeps the slots as configuration that waits with `no-presenter`.
Remaining gaps: presentation is SDR-only and winit exposes no HDR capability,
so `hdr=auto` reports `sdr-capability-unknown` or `sdr-presenter-limited`
rather than driving HDR; window placement happens on one event-loop thread
polled every 500 ms rather than on platform hot-plug notifications; the
Preview composite is rendered on the realization thread rather than off the
Program critical path; and no multi-monitor hardware, Windows, or macOS
evidence has been recorded. `OR-011` remains planned.

Exit: a useful offline/file-based switcher, with no engine dependency on UI
lifecycle.

## 5. Phase 3 — live capture and professional core (12–16 weeks)

1. Add OS camera, audio, and screen/window capture adapters.
2. Implement source hot-plug, signal loss/fallback, timestamp validation.
3. Implement audio device clocks, drift correction, channel mapping, delay,
   native EQ/gate/compressor/limiter.
4. Add scenes with background plus ten layers, keys, masks, and effect stack.
5. Complete standard transition families, AlphaFade, FTB, and T-bar operator
   controls and acceptance evidence.
6. Add eight overlay channels, stinger preload/cut point, per-output inclusion.
7. Add two multiviews, labels, tally, audio meters, safe areas.
8. Add scopes and professional color correction.
9. Add shortcuts and initial MIDI/OSC/controller support.
10. Certify Profile A without network contribution.

Current implementation boundary for item 4 (`IN-020`): every scene layer has a
crop, a transform (position, size, quarter turns, tilt, anchor), a mask, an
optional key, and an ordered effect list. A key is a chroma key (key colour,
similarity, smoothness, spill) or a luma key (`low < high` band, softness,
invert); a layer holds at most eight effects, each one of the reference
`levels`, `brightness_contrast`, `saturation`, `sharpen`, or `flip` with
exactly its bounded parameters (schema 41: each layer's `key` and strict
`effects`). The simulated CPU compositor and the native GPU compositor (a
per-layer prep pass: crop, key times mask, effects on straight colour, before
the grade passes) apply them identically per layer before the input and layer
grades and the transform, within the 64-enabled-layer limit; the native path
matches the CPU reference within a few codes. Protocol 2.43 carries keys and
effects in every layout layer and adds the padlocked, undoable, gesture-folding
`scene_layer_key_set`/`scene_layer_key_clear`/`scene_layer_effects_set` live
edits. Studio's Layer designer inspector has KEY (None, Chroma, Luma) and
EFFECTS (add, remove, reorder, per-effect parameters) sections with English
and Spanish hover help covered by the hover audit, and `freemix-cli` provides
`scene-layer-key` and the `scene-layer-effect-*` commands locally plus
`remote-scene-layer-key` and `remote-scene-layer-effects`. The acceptance test
(`cargo test -p freemix-cli --test scene_layers`) authors one background plus
ten positioned foreground layers through the CLI, each with its own crop, 2x
transform, chroma or luma key, and effect, renders them through the compositor,
and checks every pixel outside the boundary columns against an analytic
reference. Remaining gaps: key-fill (external matte) and key refinement remain
CPU-only and are not authorable on scene layers; effects are limited to the
five reference kinds (no blur, LUT-per-effect, or third-party effects); the
chroma key colour is chosen with a colour button, presets, and RGB fields (no
eyedropper, since the designer shows only the server's already-keyed preview);
and no live-camera or Profile A performance evidence with ten keyed layers has been
recorded. `IN-020` remains planned.

Current implementation boundary for item 9: native Studio maps unmodified keys
1–8 to Preview selection for the first eight replicated inputs, C/F to Cut/Fade
with the current transition duration, B to toggle FTB with its dedicated duration,
and Escape to cancel a desired active manual T-bar, while its current Ready, view,
permission, focus, non-repeat, modifier, and manual-control gates allow the action. This adds no binding editor,
chords, scopes, persistence, MIDI/controller runtime, conflict handling, or
shortcut acceptance evidence. When configured, native Studio also accepts the
bounded loopback OSC action contract, including zero-argument manual commit and
cancel and automatic AlphaFade, Slide, Zoom, and Wipe actions, with the same
current view, permission, and manual-transition gates;
malformed, rejected, and overflow counts are visible to
the operator. `fm-automation` now also carries a bounded shortcut template layer
beside the existing chords, scopes, and conflict detection. Each template pairs
its id, name, scope, chord, and delivery intent with a dynamic target that
resolves a 1-based roster slot, an explicit full-width input identity, or a
caller-supplied value inside a validated range at press time. The template
registry reuses the exact/prefix conflict and local-over-global resolution rules,
press results are deterministic and never mutate the registry, and a versioned
tab-separated profile with a caller-supplied command codec round-trips every
field exactly while rejecting malformed, duplicated, or oversized documents
wholesale. This is model and import/export groundwork only: no durable shortcut
state, binding editor, shortcut wire payload, runtime dispatch, default key map,
or resolved engine binding exists in the project schema, protocol, engine,
Studio, or Web, so RC-004 remains planned.

Current implementation boundary for item 8: `fm-scopes` now provides a bounded
reference color-monitoring session on top of its scope math: an exact
monitoring point (program, preview, or a full-width input ID), a bounded ordered
waveform/parade/vectorscope/histogram selection, a validated integer video/scope
split layout, a 1–3,600-frame analysis cadence that skips non-cadence frames
without touching them, an exhaustive false-color luma band map, and
deterministic reports that reuse the existing reference scope functions.
`fm-compositor` now executes a typed reference effect set on its CPU oracle:
white/black levels, brightness/contrast, BT.601 saturation, per-mille unsharp
sharpen, and horizontal/vertical flip. Each effect validates its parameter
schema, leaves alpha untouched, runs in declared order after key/mask and before
premultiplication and transform, and documents exact integer rounding; known
names with bad parameters report a distinct invalid-effect error while unknown
names still fail as unsupported, and the native backend still rejects every
non-passthrough effect. Effects are now authored and persisted: schema 24
carries an ordered, bounded stack on every scene layer, the CLI authors it
next-open only through effect add/set/remove/clear and prints it in scene
status, and both the offline PPM renderer and the native project planner map
the persisted stack into compositor descriptors in order. Remaining gaps: no
live or protocol effect mutation, no Studio/Web effect surface, no GPU/WGSL
effect execution, no deinterlace, and no `fm-color` grading integration. The
scope session is still unsurfaced: no scope renderer, monitoring-point frame
tap, overlay, telemetry, or scope persistence exists. Item 8 therefore remains
incomplete, and `SW-007`/`SW-010` remain planned.

Current implementation boundary for item 7: `fm-compositor` now carries a
bounded reference multiview layout. A validated definition selects program,
preview, or full-width input cells into a 1x1 through 4x4 grid with a
confidence mode (none, inputs-only, or one selected input) and label, tally,
clock, and audio-meter placement intent. Planning uses exact integer division
with deterministic leftmost/topmost remainder allocation, emits row-major
half-open cells that tile the canvas without gaps or overlaps, and hit-tests a
pixel back to its cell for click-to-preview. The compiler resolves every cell to
a compositor source and builds an ordinary operator scene with one
inset-bordered layer per cell, so the existing CPU and native scene paths
rasterize the tiles; unresolved cells are rejected instead of silently skipped.
This is a definition and planning model, not the product: labels, tally, clocks,
and meters are non-rasterized intent, and the Mix selector, a second independent
multiview, selection persistence, protocol or wire state, daemon multiview
outputs, confidence-mode behavior, and click/touch dispatch to the engine all
remain absent, so item 7 and `SW-011` remain planned.
`fm-compositor` also generates exact integer safe-area guides: centered 93%
action and 90% title presets, an SMPTE preset that emits both, and
caller-positioned custom guides in basis points, all with documented truncating
geometry, half-open bounds, a sixteen-guide plan bound, and operator-only
rasterization. Aspect-ratio guide sets, explicit non-operator routing,
persistence, wire state, and an operator UI for choosing guides remain absent,
so `SW-014` remains planned.

Current implementation boundary for item 6: the switcher and engine now own
eight independently addressed overlay channels with a retained source, exact
on/off state, source update, a deduplicated set of included output IDs, and an
independent Cut/Fade transition with a bounded 1–3,600-frame duration.
Overlay commands are frame-boundary mutations independent of an active Program
transition, and desired/realized overlay arrays are included in engine frames,
durable switcher events, snapshots, client projections, and idle checkpoint
validation. Fade realization advances channel opacity deterministically, keeps
fade-out channels active until their zero-opacity endpoint, and rejects idle
snapshots while any channel is moving. Every channel also owns a bounded
64-source FIFO queue and deterministic full-frame, top-left, top-right,
bottom-left, and bottom-right position presets plus none/thin-white/thick-white
inset-border presets. Schema 17 persists the complete desired and realized
arrays, appearance, queue state, and exact per-input audio strips, and accepts schema
17 only. Protocol 2.12 carries canonical input names, durable input renames, opacity, transition kind,
duration, Take, Update,
Off, output inclusion, transition/appearance configuration, Queue, Take Next,
and atomic per-input audio-strip commands and state, and accepts protocol 2.12 only.

Control authorization treats overlay mutations as transition operations. The
client/UI reducer validates exactly eight unique channels, active-source
references, and output inclusion uniqueness. CLI local and remote commands
cover Take, Update, Off, inclusion, Queue, Take Next, and Cut/Fade/appearance
configuration; status prints both desired and realized arrays with opacity,
transition, appearance, and queue state. Web exposes transport-free semantic
controls, and Studio exposes Take Preview, Update Preview on active channels,
Queue Preview, Take Next, Off, and per-channel Cut/Fade/position/border
controls plus one persisted-name output inclusion toggle for every configured
output. Studio displays desired and last-confirmed realized source, activity,
opacity, queue, and appearance summaries for all eight channels; its output
toggles reflect the latest confirmed desired inclusion state. This is not
output-video evidence. Queued
Studio overlay toggle and cycle actions resolve from the latest confirmed desired
channel, so repeated clicks remain cumulative before dispatching full protocol commands.
The native daemon derives a stable realization for every configured project output,
unions their active overlay sources into one scene-execution closure, renders the
authoritative Program transition or Stinger base once, and then produces and
retains one independent GPU texture per output. Each output composites only its
included channels source-over in channel order before Fade-to-Black, using the
exact per-frame opacity, deterministic one-third-frame PiP geometry, and
scale-aware inset white border presets implemented by both the CPU oracle and
native WGSL compositor. Resource planning charges the shared base, per-output
composition and final targets, and the prior retained output set. The existing
fullscreen and recorder consumers select the first configured output
deterministically; an output-less headless project retains its unbound Program
path.

This completes the independent source, on/off, per-output-inclusion,
Cut/Fade-duration, position/border, bounded FIFO queue, and simultaneous native
output-realization slices. Profile-wide hardware acceptance evidence remains,
so `SW-005` stays planned.

Current implementation boundary for the transition family and favourite
buttons (`SW-002`, with overlay motion for `SW-005`): `fm-video` owns one CPU
reference per geometric shape (`render_transition`): left-to-right and
top-to-bottom wipes, horizontal and vertical pushes, centred Zoom, Fly (Preview
flies in from the right, growing), Fly-Rotate (Fly with a half turn), Cube (a
perspective cube turn), Cube-Zoom (the turn pulls back mid-way), Cross-Zoom (a
zoom-through crossfade), and Merge. Wipes, pushes, and Merge copy whole texels at
floored integer extents; Zoom samples nearest-neighbour; the projective kinds
sample bilinearly at 1/256-texel precision, and every shape returns exact
endpoint clones. Two substitutions are deliberate: Merge is a deterministic
split-converge (Preview's halves slide in from both sides and meet in the
centre) rather than any particular vendor's pixel blend, and Cube splits the
faces at a hard crease instead of drawing the nearer face over the farther, so
no translucent seam appears at the edge. `fm-compositor` executes the same
shapes on the CPU and in one native WGSL pass; the lavapipe parity test
(`native_transitions`) measures at most 1.633/255 maximum and 0.311/255 mean
absolute error per kind over several progress points (Fly-Rotate's maximum,
Cross-Zoom's mean) and skips without an adapter unless
`FM_REQUIRE_NATIVE_MEDIA=1`. `fm-sim` renders every geometric mix through the
same reference, so the CLI `render` and `transition-render-frame` goldens and
the simulated daemon match the native compositor's oracle. Protocol 2.36 takes
any automatic mix by kind (`transition`), and the native daemon maps every mix
to its compositor kind with a sample-linear audio crossfade. The manual T-bar
still offers only Fade, Wipe, AlphaFade, and Slide, and playlist entries keep
their existing kinds.

Overlay channels accept the whole family in and out: Cut and Fade keep their
opacity semantics, and every motion kind keeps the channel opaque while the
switcher reports exact `OverlayMotion` progress, starting at zero on the take
frame. The native daemon draws the layer alone over transparency at its
position and border, runs the compositor transition from transparency to the
layer (entering) or back (leaving), and composes the result source-over in
channel order. Monitor taps and the simulated preview still draw a moving
overlay at its settled geometry; they do not animate the motion.

Schema 35 persists four operator-assignable favourite transition buttons
(Cut, any mix, or a Stinger slot, each with a 1–3600 frame duration; Fade,
Wipe, Fly, and Cube at 30 frames by default). Protocol 2.36 assigns one with
`transition_favorite_set` (a transition-class, undoable project edit honouring
the padlock), replicates all four inline in the snapshot, and announces each
change with `transition_favorite_changed`. Studio shows the four keys under the
automatic transitions (click or F5–F8 takes one; right-click opens `Assign…`
at the shared duration), a MORE menu with the eight new mixes, and an overlay
transition key that cycles the family with a right-click picker; the CLI lists
kinds and favourites, takes and assigns them locally and remotely, and renders
any mix at an exact progress. Profile-wide hardware acceptance evidence
remains, so `SW-002` and `SW-005` stay planned.

Current implementation boundary for item 3: `fm-audio` provides a bounded,
deterministic planar-F32 sample-delay primitive with immutable channel count and
exact nonnegative sample delay, transactional block validation, caller-owned
output, allocation-free steady-state processing, and explicit reset. The
reference `MasterMixer` now gives every logical strip independent raw-planar
delay history before channel mapping and gains, advances that history with
submitted PCM or silence on every successful Master interval, and bounds total
retained history per mixer. Schema 17 gives every persisted input strip exact
bounded gain, stereo balance, mute, solo, follow-video, and 0–48,000-sample delay
values and rejects missing, wrong-typed, or out-of-range fields.
Native project compilation carries that value into physical and scene-alias
strips, and native Master/Stinger preflight applies it transactionally to both
active and pending mixers before gain, balance, mute, solo, follow-video, and source envelopes.
The engine owns the live desired full-strip map and emits frame-boundary
realization updates. Protocol 2.12 snapshots pair every input ID with its
canonical persisted name and replicate the complete strip map; its
`SetInputAudioStrip` command atomically carries gain, balance, mute, solo,
follow-video, and delay and is authorized by the dedicated audio-control
permission. The daemon applies each update transactionally to every active and
pending ordinary/Stinger mixer, with 240-sample linear live gain and balance
ramps and next-sample mute/solo/follow-video/delay changes, before checkpointing the
canonical project. The local and remote CLI expose the command, exact status,
and input labels; Studio renders persisted names with Ready- and
permission-gated per-input controls; Web exposes the equivalent
transport-free semantic controls. Device audio and device clocks, device-level
drift correction and channel-mapping realization, meter/UI delivery, and
hardware acceptance remain. `fm-audio` separately provides the `AU-005`
reference DSP chain: exact polarity, an RBJ high-pass plus up to four peaking EQ
bands, a soft-knee compressor with makeup gain, an instant-attack ceiling
limiter, a hold/attack/release downward gate, and a peak/RMS window meter,
composed per channel by a preallocated `StripDsp` that processes planar
`AudioBlock`s in place without steady-state allocation and rejects mismatched
formats or non-finite samples before mutating state. No persisted strip DSP
state, protocol command, daemon mixer realization, Studio/Web surface, or
hardware acceptance consumes those primitives yet. This is still a partial item
3 slice and does not complete or change the status of any `AU-*` parity row.

Current implementation boundary for item 1: `fm-io-macos` is the first native
platform leaf. An isolated Swift helper uses `AVFoundation` to enumerate camera
video and microphone devices without prompting, report their independent
authorization states, and expose exact
normalized rational-rate formats, and capture selected CPU-backed BGRA frames
with original `CoreMedia` timestamps. Rust communicates through bounded binary discovery and
stream protocols, derives collision-checked stable device/source IDs, implements
the `fm-io-api` discovery and source lifecycle contracts, and never substitutes
a different device. Discovery is capped at 64 devices, 256 formats per device,
256 KiB total metadata, 3840x2160 at 60 fps, and 4 KiB identifier/name fields.
Capture records are capped at 64 MiB and feed a negotiated queue of at most
eight frames with drop-oldest accounting; native `AVFoundation` drops are
reported separately. The v3 capture protocol requires source-derived color
primaries and transfer attachments on every delivered CoreVideo buffer. It
accepts Rec.709, Display-P3 D65, or BT.2020 primaries paired with sRGB or BT.709
transfer, derives identity matrix and full-range RGB semantics from the
negotiated BGRA representation, verifies opaque active pixels, and rejects
missing, malformed, unknown, or unsupported metadata instead of substituting
defaults. Startup waits for a running-session
marker, stop/reap and worker cleanup have explicit deadlines and retained ownership, and helper
interruptions, disconnects, malformed frames, no-frame deadlines, Hold/Stop
fallback, and recovery are observable. Capture magic is emitted only after the
session is running and before the delegate is attached; Rust revalidates the
negotiated dimensions, positive CoreMedia timescales, and the 60 fps bound.
Swift compilation targets the Cargo
architecture with a macOS 13 deployment minimum. Applications can supply a
packaged helper path; the embedded Cargo `OUT_DIR` path is developer-only.
`freemix-capture-node cameras` now selects this adapter and emits bounded,
versioned, one-line records with stable IDs, adapter-qualified project
`stable_key` values, permission state, format counts, and percent-escaped names.
The v2 key is bounded, derived from the collision-checked source ID, and supports
exact adapter lookup without display-name or discovery-order substitution. The
command never prompts unless `--request-permission` is
present when using the built-in helper; `--helper` trusts an
application-packaged executable. `camera-smoke` selects one exact snapshot source
and advertised format, acquires 1 to 300 frames under a 1 to 60 second acquisition
deadline, reports source timestamps plus native/queue drops, and always attempts
bounded stop/close cleanup. It never requests permission. A hermetic helper test
acquires two framed samples, verifies telemetry and helper-process disappearance,
and proves `prompt-required` preflight never invokes capture. The v2 discovery
protocol carries normalized frame-rate numerators and denominators. The helper
advertises supported candidate rates through 60 fps: every integer rate plus
24000/1001, 30000/1001, and 60000/1001, with common rates prioritized within
the existing 256-format bound. Selection and `CoreMedia` frame duration use the
exact rational value rather than floating-point or nearest-rate substitution.

At this adapter boundary, camera recovery is adapter-local, caller-driven, and
bounded. Hold continues returning the last delivered frame through signal loss
and rejected recovery attempts. A restarted helper retains source identity and
must produce the same clock with a strictly newer PTS before recovery completes;
the first accepted frame is marked discontinuous. Sequence remapping creates one
globally monotonic stream while preserving native sequence gaps, including gaps
from bounded queue drops before the first recovered delivery. Received,
queue-drop, native-drop, continuity-rejection, and timeout-discard counters plus
peak depth are cumulative across attempts; current queue depth is instantaneous.
Recovery startup, first-frame waits, timeout discard, and late-producer
exclusion are bounded. Foreground helper cleanup attempts return by a bounded
deadline. If child or worker completion misses it, ownership is retained and
handed to a background reaper on final teardown rather than detached; blocking
reap/join completion itself is not guaranteed within a bounded time. Hermetic
helper-process evidence covers malformed, regressed, recovered, timed-out, and
truncated attempts.

The same helper now has separate `discover-audio`, `request-audio-permission`,
and `capture-audio` commands, preserving the camera D2/F3 protocol. `FMAUDD1`
discovery reports independently identified microphone endpoints and exact native
formats up to 192 kHz, accepting mono or a two-channel format only when Core
Audio explicitly labels it stereo. `FMAUDF1` carries bounded interleaved
little-endian F32 records with original CoreMedia PTS, sample rate, channel
count, sample count, and independent sequence telemetry. Rust validates all
length arithmetic before allocating, limits each block to 16,384 samples and
128 KiB, rejects non-finite samples, derives duration from the exact sample
span, and deinterleaves into planar `AudioBlock`s. Each microphone has a
collision-checked `macos.avfoundation.audio.v1` stable key and source-specific
clock domain; no equivalence to a camera or Master clock is claimed.
`MacosAudioSource` implements the standard lifecycle with a queue of at most 32
blocks. Queue overflow is sticky failure rather than silent sample loss, and
stop, recovery, drop, worker, and helper ownership are bounded. The capture node
adds `audio-inputs` and `audio-smoke`; discovery prompts only with explicit
`--request-permission`, while smoke requires the reported stable key plus exact
sample rate/channel count and never substitutes a newly indexed or default
device. Its v1 smoke diagnostic reports timing, samples, signal peak, queue
pressure, and explicitly marks native drop measurement unavailable. Hermetic
protocol and process tests prove PCM layout, finite-sample rejection, exact
arguments, no permission invocation, and helper reaping. Non-prompting local
discovery found two audio inputs with four and one bounded formats respectively
and `prompt-required` microphone permission. The developer helper embeds camera
and microphone usage descriptions, but remains an unsigned Cargo `OUT_DIR`
artifact rather than application packaging.

Current implementation boundary for item 2: `freemixd --native-media` realizes
macOS `Device` inputs by exact persisted `stable_key`; `--camera-helper` supplies
an application-packaged helper path. Startup discovery does not prompt, requires
already-granted permission, rejects duplicate or unknown keys, and opens only an
advertised BGRA mode matching the project dimensions and exact rational frame
rate. Camera helpers start concurrently under one initial-frame deadline with a
one-frame adapter queue and Hold fallback.

After preflight, each camera has a persistent supervisor worker that owns its
adapter lifecycle independently of the render loop. Recoverable signal loss and
runtime helper exits enter bounded retry with capped backoff; exhausted attempts
enter a bounded rearm wait and retry again. Every attempt retains the exact
source identity, clock, and mode. A one-frame latest slot reports replacement
pressure and carries a pending discontinuity onto its replacement, so a dropped
first recovery frame cannot hide the handoff. Malformed framing and other media
contract failures are fatal rather than retried. During recovery, continuity
timestamp regressions are instead rejected, counted, and skipped while Hold
continues; if no valid frame arrives before the first-frame deadline, timeout
returns to daemon retry. Daemon shutdown requests cancellation for all workers
before waiting against one aggregate deadline.

`FREEMIXD_TELEMETRY` v4 and `FREEMIXD_CAMERA_SOURCE` v1 include recovery
attempt/success/exhaustion/failure state, adapter queue and ready-delivery state,
terminal/cancellation discards, latest-slot replacement/depth, preflight state,
ingest failures, and successful live GPU ingests. Per-source tests assert exact
conservation of every adapter-received frame across ingestion, explicit discard
or replacement classes, and instantaneous outstanding slots; daemon aggregates
are folded from the same snapshots. Telemetry v4 additionally exposes native
audio current/peak retention and reservation blocks, samples, and bytes;
reservation requests, stalls, positioned blocks/samples, leading-silence and
EOS-padding samples, and fake-sink depth/peak/drop counters. Diagnostics remain local
`diagnostic-not-certification` records and omit hardware identifiers, names,
stable keys, paths, health details, clocks, and media.

Hermetic daemon-level evidence covers exact helper arguments and mode, recovery
while rendering/checkpointing continues, retry exhaustion and rearm, generic
runtime exit recovery, fatal malformed contracts without restart, one recovering
camera alongside one uninterrupted camera, aggregate multi-camera startup
cleanup and shutdown cancellation, frame conservation, and helper reaping. The
hermetic daemon process exercises generated camera metadata plus selected GPU
ingest, frame conservation, source timing, checkpointing, and cleanup. Separate
protocol and capture-node tests cover metadata boundaries, and the current schema
persistence round-trips Display-P3/BT.709. Separate native Metal readback tests
compare color conversions across supported primary/transfer combinations
against CPU oracles; this does not claim that every combination traverses and
is pixel-validated through the full daemon path.

Portable protocol/lifecycle and capture-node process tests pass; required local
D2 discovery enumerated two real macOS camera endpoints with 252 and 126 bounded
candidate modes and `prompt-required` permission, and the real smoke command
stopped at permission preflight without prompting. No real camera
frame-acquisition or daemon hardware evidence therefore
exists; the passing daemon run is hermetic rather than device certification. The
slice also omits paired/synchronized camera audio, signed helper packaging,
source-clock scheduling and drift correction, live per-source telemetry
subscription, HDR transfers, ICC-only profiles, gamma-tag fallback, configurable
unknown-metadata policy, autonomous inventory hot-plug, real hardware recovery
certification, Windows/Linux adapters, audio-device daemon/Master realization,
and screen/window/application-audio capture. Items 1 and 2, plus `IN-001`,
`IN-005`, and `IN-011`, therefore remain incomplete and planned.

Current implementation boundary for item 3 sample-rate conversion (`AU-004`):
`fm-audio` adds a polyphase windowed-sinc resampler into the 48 kHz engine
clock for 8–192 kHz sources, with exact integer ratio accounting so a
zero-correction run consumes exactly `floor(rendered * in / out)` input
samples. Measured fixed-ratio THD+N is at least 105.9 dB from 44.1 kHz and
about 150 dB from 48/96 kHz, with at least 107 dB alias rejection. A per-source
`SourceSync` with a bounded buffer converts planar blocks through a
critically damped PI drift controller on buffer fill (±500 ppm range,
slew-limited); simulated ±100 ppm clocks run 30 s without underrun or overrun
and settle within 0.2 ppm. Gaps up to 100 ms are filled with silence, overlaps
are trimmed, larger jumps splice or flush per policy, underruns and overruns
recover with 1 ms fades, and allocation-free telemetry reports drift,
correction, fill, and every recovery count. Remaining gaps: nothing wires
`SourceSync` into capture backends, the mixer, or `freemixd`; there is no
`AudioBlock`/`MediaTiming` adapter, shared filter tables, keyed source set,
quality measurement under active drift, jitter or bursty-delivery coverage,
crossfaded splices, or real-device evidence. `AU-004` remains planned.


Current implementation boundary for item 4: native `freemixd` now realizes
current-schema scene inputs through an immutable `NativeProjectPlan` compiled before
opening media or GPU resources. The planner rejects missing references and
video/audio cycles, bounds reachable scenes and total enabled layers at 64 each,
and enforces a default 512 MiB peak transient RGBA16F budget. It maps full-width
128-bit input/scene identities to generated compositor tokens without narrowing,
orders nested and shareable scenes dependency-first, and preserves background,
enabled state, translation/size, crop, opacity, signed z-order, and 0/90/180/270
degree rotation. Per frame it derives the selected Program transition roots,
renders only their dependency closure once, releases non-root scene textures
after their final consumer, and composites both scene endpoints before applying
Cut, Fade, or Wipe.

Scene audio recursively follows each explicit `audio_source` to one physical
leaf or explicit silence; two scene routes that resolve to the same terminal are
mixed once at unity. One GPU completion-fenced project-frame slot prevents the
next frame from reclaiming transient scene textures before prior submissions
complete, and the planner charges the worst selected closure plus transition and
previous-Program targets against its peak budget. CPU planner tests cover bounds,
cycles, full-width-ID collisions, visual mapping, selected closure, recursive
audio, and sharing. Native Metal tests cover nested shared scenes before Fade and
Wipe plus 96 completion-fenced frames without production readback; a daemon test
checkpoints and restarts from scene Program/Preview routes. These are macOS/Metal
and hermetic diagnostics, not cross-platform certification. Independently,
`fm-compositor` executes hard-edged rectangular masks in half-open post-crop
source space. Schema v6 persists an optional mask per layer and native
`freemixd` maps it into the immutable plan after strict post-crop bounds
validation. CPU-oracle tests cover crop, half-open edges, inversion, rotation,
translation, and stable resource accounting; ignored native Metal readback
coverage remains opt-in and is not claimed without an adapter. Daemon
checkpoint/restart tests preserve masks. Feathered or non-rectangular masks,
keys, effect stacks, a ten-layer
product limit, per-output scene realization/routing, live scene
edits/replanning, and cross-platform or hardware certification remain. Item 4
and its parity rows therefore remain incomplete and planned.

Current implementation boundary for item 5: horizontal Wipe now flows through
local and remote CLI commands, `fm-control`, `EngineCommand`, the switcher,
`fm-sim`, the native compositor, and daemon rendering/checkpointing. The current
protocol carries `CommandPayload::Wipe`. Exact
rational progress selects `floor(width * numerator / denominator)` replacement
columns and preserves identical start/end frames, with exact CPU and Metal
coverage of endpoints and pixel boundaries. `freemix-studio` enables its Wipe
button when transition permission allows it; Fade and Wipe share one bounded
duration. Recovery preserves strict intent FIFO, so an unresolved Wipe is not
bypassed by later commands. Bounded
terminal history, collision-triggered authoritative resync, and Studio's sticky
terminal-uncertainty ledger cover ambiguous replay receipts. `freemix-web`
declares only the current protocol in its client configuration. Its transport-free
semantic presentation model preserves permission- and protocol-gated
Cut/Fade/Wipe and adds manual Fade/Wipe/AlphaFade/Slide Start, exact basis-point Position,
Commit, and Cancel controls derived from separate authoritative desired and
realized projections. No browser renderer or network runtime exists.

The T-bar control core supports Fade, Wipe, AlphaFade, and Slide through `fm-switcher`,
`EngineCommand`, `fm-control`, and the current protocol with exact held,
reversed, committed, and cancelled progress. The current schema persists
distinct desired and realized manual state, and daemon process tests cover
replay-safe restart through commit and cancel. The CLI exposes local and
remote Start, integer `0..=10_000` basis-point position, Commit, and Cancel
commands; local command processes restore/mutate/save the engine checkpoint,
and remote commands use the current contract. Studio presents replicated desired and realized kind, routing, and
exact position without treating widget state as engine truth. Its manual
controls require Ready state and transition permission. While either replicated
manual state is active, Studio disables automatic Program/Preview transitions and
input-bank Preview selection, but leaves Fade-to-Black and overlay controls
independently gated. Reconnect handling
preserves strict worker FIFO. Web's transport-free semantic model
retains the manual controls and adds FTB live/black actions, a bounded duration,
exact separate desired and realized state, reversal through the opposite target,
and protocol/permission/readiness/completeness gates. It still has no browser
renderer or network runtime. A hardware-gated macOS recording acceptance covers
protocol-driven native manual Fade progression, reversal, cancel, and commit on
one configured output.

AlphaFade groundwork preserves the switcher's existing distinct transition kind
through the simulated pipeline, compositor CPU plan, native wgpu plan, daemon
video planner, and sample-linear Master-audio transition plan. The selected
contract independently interpolates every premultiplied RGBA channel, including
alpha. Exact CPU tests cover transparent intermediate pixels, daemon unit tests
cover video mapping and continuous Master-audio endpoints, and an opt-in Metal
test compares AlphaFade output with the CPU linear-frame oracle on a real
adapter. The current standard Fade uses the same channel-wise reference math, so
these primitives do not yet prove a visible product distinction between Fade and
AlphaFade.

The automatic path now continues through a bounded `EngineCommand`,
target-free transition authorization, runtime lifecycle tracking, and a
current-contract `AlphaFade` command. The client preserves the exact duration.
Daemon durable execution
projects all requested frames before saving, then a focused checkpoint test
restores the settled Program/Preview routing and engine counters exactly. The
CLI now exposes matching local and remote `alpha-fade ... <frames>` commands.
Local execution settles and saves the bundle with replay-safe idempotency;
remote execution preserves the exact duration. Web exposes a semantic
AlphaFade action with the shared bounded duration when Ready and authorized.
It remains a transport-free model with no browser network runtime. Studio presents an
AlphaFade action alongside Fade/Wipe using the same bounded duration. Its
availability requires Ready state, replicated view, and transition permission.
The worker maps the typed intent to the exact command, and a loopback worker test observes the duration, envelope, durable
event, and runtime realization ordering. A hardware-gated macOS/Metal process
acceptance now sends protocol AlphaFade to a real configured Program recorder
using static opaque-white and transparent-black generators, observes an ordered
opaque/intermediate/transparent recording, decodes the result, and verifies the
settled persisted routing.

The authoritative manual T-bar core now accepts AlphaFade alongside Fade and
Wipe, preserving exact held and reversed basis-point intervals through engine
snapshots and feeding the existing AlphaFade native video plus sample-linear
Master-audio plans. The current protocol carries the new manual kind. The
current schema persists the kind, and a
daemon restart acceptance verifies replay-safe commit and cancel from the
restored held state. CLI manual AlphaFade start is available locally and
remotely with local restart-safe idempotent replay. The Web semantic control
surface exposes a manual AlphaFade start control, and
preserves authoritative AlphaFade projections. Studio gates its manual AlphaFade
start button independently from the existing manual
controls, and carries AlphaFade intents through the worker into authoritative
desired and realized presentation. A hardware-gated macOS/Metal process
acceptance drives manual AlphaFade to 75%, reverses to 25%, cancels, then fully
progresses and commits on a configured Program recorder. It requires the
corresponding ordered decoded-luma sequence: initial white, forward gray,
reversed bright, cancelled white, and committed black. It also verifies the
inactive committed checkpoint.

Horizontal Slide rendering groundwork now moves Program left while Preview
enters from the right at the exact integer offset
`floor(width * numerator / denominator)`. CPU and simulated paths cover exact
endpoints and odd-width pixels; a required Metal readback matches the CPU
oracle. Native planning preserves the Slide kind, and Master audio uses the
existing sample-linear two-source crossfade. Automatic Slide is now
command-reachable through the engine and target-free transition authorization.
The current protocol carries its exact duration. Daemon durable
execution settles every requested frame before checkpointing; focused unit and
process restart acceptances restore exact Program/Preview routing, counters,
receipt history, and resume position. CLI now exposes matching local and remote
`slide ... <frames>` commands. Local execution settles and saves the bundle
with replay-safe idempotency; remote execution preserves the exact duration.
Web exposes an accessible semantic Slide action with the shared bounded
duration when Ready and authorized. It remains a
transport-free model with no browser network runtime. Studio presents a Slide
action alongside the existing automatic transitions using the shared bounded
duration. Its availability requires Ready state, a replicated view, and
transition permission. The worker maps the typed intent to the exact command,
and a loopback worker test observes the duration,
envelope, durable event, and runtime realization ordering. A hardware-gated
macOS/Metal process acceptance now sends
protocol Slide through a real configured Program recorder, decodes stable
white, a Slide-specific white-left/black-right intermediate frame, then stable
black, and verifies the settled persisted routing.

Manual Slide now uses the existing T-bar contract and the existing Slide
compositor. Exact held, forward, reversed, cancelled, committed, and restored
basis-point intervals flow through the switcher, engine, persistence, protocol,
durable control/client projections, local and remote CLI, Studio, and the Web
semantic model. The daemon maps those intervals to the existing Slide video
plan and sample-linear two-source audio crossfade. Focused non-native unit and
process tests cover authority, projection, checkpoint restart, and terminal
actions. This is control/runtime evidence only. It does not add a browser
transport or renderer, and it does not certify fullscreen, hardware, or
cross-platform output. Phase 3 item 5 and `SW-002`/`SW-004` remain planned.

Centered Zoom rendering groundwork now overlays Preview on Program with
independently floored extents
`floor(width * numerator / denominator)` and
`floor(height * numerator / denominator)`, a left/top bias for an odd centering
remainder, and deterministic nearest-neighbor sampling from the full Preview
frame. CPU and simulated paths cover byte-exact endpoints plus odd 5×3
intermediates. The native wgpu path carries the same explicit geometry in its
uniform, and a required Metal readback matches the CPU oracle at five progress
points. Native daemon video planning now preserves Zoom, while Master audio
uses the existing sample-linear two-source crossfade. The source-only
audio planner rejects Stinger without project configuration; the project-aware
Master path applies the configured policies described below. Automatic Zoom is
now command-reachable through the engine
and target-free transition authorization. The current protocol carries its
exact duration. Daemon durable execution
settles every requested frame before checkpointing. Focused unit and process
restart acceptances restore exact Program/Preview routing, counters, receipt
history, and resume position. CLI now exposes matching local and remote
`zoom ... <frames>` commands. Local execution settles and saves the bundle with
replay-safe idempotency; remote execution preserves the exact duration. Web
exposes an accessible semantic Zoom action with the shared bounded duration
when Ready and authorized. It remains a transport-free model with no browser
network runtime. Studio presents a Zoom action
alongside the existing automatic transitions using the shared bounded duration.
Its availability requires Ready state, a replicated view, and transition
permission. The worker maps the typed intent to the exact command, and a
loopback worker test observes the duration,
envelope, durable event, and runtime realization ordering. A hardware-gated
macOS/Metal process acceptance now sends protocol Zoom through a real configured
Program recorder, decodes stable white, a Zoom-specific white perimeter with a
black center, then stable black on a 3×3 luma grid, and verifies the settled
persisted routing.

Stinger groundwork now defines a validated zero-based media-frame plan
independent of the generic two-source transition plan. The configured cut point
is the first media frame drawn over Preview; earlier frames are drawn over
Program, and a cut point equal to the media length defers the routing swap until
playback completes. The CPU oracle source-over composites same-size
premultiplied RGBA, while a dedicated native wgpu renderer applies the same
equation without CPU readback or a third shader binding. Required Metal oracles
match the CPU core and the compiled daemon project path before and at the cut.
The current schema durably stores up to eight unique slots with media input,
preload intent, cut point, audio policy, and missing-media fallback. Daemon
restore configures identical
desired and realized slot state, and idle restore rejects divergence.

The engine now rejects unconfigured slots and cut points beyond the requested
duration, applies ready Stingers on exact frame boundaries, and settles
Cut/Fade/KeepProgram missing-media fallbacks without stale busy state. The
current protocol carries a one-through-eight slot plus exact duration. The
native compiled project realizes retained
single-frame Stinger media through exact video base selection and three explicit
Master policies: base-only `Muted`, media-only `StingerOnly`, and unity
media-plus-base `MixWithProgram`. Three independent scene roots are included in
the preflight GPU bound. Restore now honors the durable preload intent:
unrequested slots remain `NotRequested` and take their configured fallback
instead of being promoted to Ready.

The FFmpeg adapter now admits an explicit bounded set of straight-alpha YUVA,
GBRAP, RGBA, and luma-alpha pixel formats into its existing RGBA decode
contract. Native startup gives every requested local-video Stinger a second
video playback instance: its decoder worker, GPU ring, and clip-local deadline
are independent from the same input's ordinary show timeline. The ring retains
at most eight frames, evicts only before the
clip-local floor anchor, refills in bounded pages, holds the final frame only
after confirmed EOS, and restarts its FFmpeg cursor at ordinal zero when a new
trigger requests a deadline before the retained window. Startup partitions the
existing 512 MiB RGBA16F source budget between ordinary and unique requested
Stinger rings, rejecting configurations whose full bounded rings would exceed
that one aggregate limit instead of silently doubling the budget. Replacement
pages reserve only their additional charge, and both registries must match the
project output dimensions before readiness. The daemon caches one read-only
authority projection per upcoming frame so preparation does not repeatedly
clone authority state while polling, and asserts that projection matches the
realized frame.

A hardware-gated Metal/FFmpeg oracle decodes a tagged twelve-frame alpha clip, pages
beyond the initial eight-frame GPU prefix without exceeding it, observes
Program/media/Preview composition across the configured cut, proves the
ordinary input ring was not changed, and verifies a byte-identical retrigger.
A separate native-daemon process acceptance persists the same twelve-frame
asset, sends two immediately consecutive current-protocol Stingers through durable and runtime
realization, records the ordered white/media/black/media/white result, verifies
the restored routing and revision, then starts a second recording daemon from
that checkpoint, fires a third Stinger, decodes its recording, and verifies
revision three plus the opposite settled routing. Live sources and local video
remain rejected path-free until deterministic live capture exists.

An additional required native-daemon process acceptance configures three
preload-disabled slots and sends all three missing-readiness policies over
the current protocol. It verifies two `KeepProgram` commands leave white and black
Program routing unchanged, the `Fade` fallback records an intermediate frame
before settling on black, the `Cut` fallback returns directly to white, and all
four accepted revisions checkpoint the final routing without requiring the
deferred media source.

The local and remote CLI now expose an exact `stinger <slot> <frames>` action,
restore persisted slot state before local mutation, and use the current contract
for remote writes. The offline CLI can also atomically configure, replace,
or remove any of the eight slots with full-width media input, preload intent,
cut point, audio policy, and fallback. It validates the canonical project before
save, preserves routing, manual-transition, Fade-to-Black, revision, frame,
runtime-generation, and receipt state, and projects every persisted slot field
in status output. Web exposes typed one-through-eight slot controls and preserves
the exact requested duration. Studio exposes eight accessible numbered controls with a
Ready replicated transition-capable session, displays authoritative
Unconfigured/Not Requested/Ready/Missing state for every slot, carries typed
slots into the wire payload, and keeps unresolved work ordered in its reconnect FIFO. Loopback
worker evidence observes exact slot, duration,
pending-command state, durable routing, and runtime realization ordering.
The current protocol snapshots project every configured slot field plus the
realized `NotRequested`, `Ready`, or `Missing` preload state. Clients reject an
omitted or invalid projection, and the replicated model validates unique bounded slots and media
input references. Web and Studio require the selected slot's authoritative
`Ready` state before enabling its fire action; Studio loopback evidence observes
the exact projected slot before dispatch. The FFmpeg video and audio cursor
adapters can restart at clip-local ordinal/sample zero while retaining their
fixed source identity and bounded audio metadata index; byte/sample oracles
verify each replay against the original leading decode. The independent native
Stinger video ring now consumes the video restart primitive for bounded paging
and retriggering. The current protocol carries live
configure-or-replace and remove mutations for any slot, including the complete
canonical configuration. The
transition-authorized authority validates the media input, rejects automatic
or manual transition conflicts, settles desired and realized readiness at an
idle frame boundary, emits the projected slot event, and checkpoints the exact
replacement or removal. A non-native daemon process acceptance configures then
immediately fires a slot, verifies the settled routing, restarts on the
replacement descriptor, removes it, and verifies a second empty restart.
Native-media sessions now perform the same mutations without rebuilding the
ordinary show runtime. They classify and abort an initial prepared authority
submission, preflight the complete candidate Stinger project/video/audio pools
on a background worker while native frames continue ticking, then prepare and
revalidate again before any save or commit. Failed resource preflight returns a
path-free `unavailable` result without a receipt, revision, file, projection, or
runtime change. After the authoritative idle frame realizes successfully, the
daemon atomically swaps only the Stinger plan and independent pools, re-limits
future ordinary GPU refills inside the existing aggregate bound, and transfers
old decoder/GPU/audio ownership to a bounded two-worker/two-queued-pool
retirement owner. Retirement never joins on the render thread or daemon
shutdown. A native generator-process acceptance covers Ready configure, fire,
Ready replacement, second fire, removal, exact routing, persistence, and
restart; a separate aggregate-limit rejection acceptance covers
manifest/receipt/revision rollback and continued show scheduling. An opt-in
Metal/FFmpeg acceptance hot replaces an audible local-media lane, fires it,
removes it, and checks aggregate audio telemetry. Cross-platform/fullscreen
evidence and complete `SW-003`/`SW-004` acceptance remain pending; parity
therefore stays planned.

Clip-local Stinger audio is now realized by a second, independently clocked
Master lane for each unique requested audible media input. Native startup
reserves half of the existing aggregate retained-audio block, sample, and byte
caps for ordinary show playback and half for current or future hot Stinger
lanes, then divides the latter across its per-media runtimes. This fixed
reservation prevents adding the first audible slot from rebuilding or
relimiting ordinary synchronizers, cursors, or strip-delay history. Requested
video-only slots use explicit clip silence inside the reserved Stinger side.
Only the selected media runtime is serviced or rendered, so another
slot's decoder, EOS, or stall cannot delay the active clip. Decoder requests
remain single-flight and nonblocking, and a retrigger invalidates stale
generations, restores bounded pre-video positioning, clears synchronizer and
strip-delay history, and restarts the retained FFmpeg decoder at sample zero.
Aggregate retention, reservations, stalls, padding, positioning, and peak
telemetry include every Stinger lane.

The realized policy contract is explicit:
`Muted` suppresses only the Stinger clip and keeps the selected base,
`StingerOnly` suppresses the base, and `MixWithProgram` sums both at unity
before the existing Master output stages. Program is the base before the
configured video cut and Preview is the base at and after it; that base change
does not restart or fade the clip. The media input's persisted strip state
applies to its Stinger bus. Audio before video frame zero is trimmed, delayed
audio produces clip-local leading silence, clip EOS produces silence or
base-only output for the remainder of the transition, and a shorter transition
truncates the clip. Every trigger reanchors sample zero independently while the
ordinary input cursor, strip-delay history, and inactive-source advancement
continue on the show timeline. The clip cadence is reanchored to the
authoritative global trigger frame, so fractional-rate frames consume the exact
global output sample count without losing clip-local sample zero. Unit oracles
cover all policies, the exact video cut, 59.94 fps cadence phase, selected-media
isolation, aggregate retention, and FTB after the Stinger mix. A required
FFmpeg PCM oracle proves byte-identical first-frame replay for early, delayed,
and negative-origin audio, including bounded pre-video repositioning and
sample-boundary phase preservation. A separate required
Metal/FFmpeg daemon acceptance records frequency-separated Program, Preview,
leading-clip, and trailing-clip signals through all three policies, two
replays, checkpoint restore, process restart, and a restored replay; it decodes
both recordings and verifies revisions and settled routing.

Item 5, item 6, and RC-007 remain planned.

FTB groundwork now includes a switcher-owned bounded automatic control core
alongside the compositor plan. The controller moves from its current exact
fixed-rational position to live or black over 1–3,600 frames, supports
no-jump reversal and idempotent repeated targets, and exposes each interval plus
exact trajectory progress without cumulative floating-point drift. It advances
orthogonally to automatic and manual Program/Preview transitions. The engine accepts the same
bounded in-memory intent, commits the desired endpoint immediately, advances a
separate realized trajectory on frame boundaries, exposes that exact interval
in each `FrameResult`, and permits Program transitions concurrently. Idle
snapshots reject partial FTB motion and validate identical settled desired and
realized endpoints. The compositor separately admits a bounded exact-rational
start, end, and progress plan, including hold and reverse trajectories. Its CPU
oracle and native wgpu path apply the plan after canonical RGBA16F Program
composition by mixing premultiplied linear RGBA toward opaque black, without
color conversion, audio work, or production readback.

The current protocol carries a bounded `FadeToBlack` command plus exact desired and
realized target/position state in snapshots, durable switcher events, and
runtime confirmations.
`fm-control` authorizes FTB as a transition, tracks it independently from
Program transitions, preserves monotonic runtime generation/sequence ordering
through overlap and reversal, and emits deterministic supersession. The client
and UI reducer require complete state on the exact current protocol and retain
exact desired and realized FTB projections by durable revision.

The current schema persists only settled live or black checkpoints and rejects
partial or desired/realized-divergent FTB state. Daemon and local CLI
checkpoint/restore paths preserve the exact
endpoint; daemon tests cover live-to-black and black-to-live commands across
engine reconstruction. The production native realizer now applies each engine
frame after Program scene/transition composition: video renders the exact FTB
interval endpoint into a final canonical RGBA16F target, while Master audio
ramps the inverse fixed-rational position across the same sample interval after
the complete Program mix. Native project planning accounts for that additional
in-flight target. Unit coverage exercises forward, reverse, held-black, and
post-Master behavior, and focused Metal tests validate both the compositor
oracle and the scene/transition/FTB ordering on a real adapter. The CLI now
exposes explicit local and remote `ftb ... <live|black> <frames>` commands,
settles and persists local moves, and prints separate exact desired and realized
target and position state. Web has the corresponding transport-free semantic
model, but no renderer or network runtime. Studio presents a dedicated native
panel with exact separate desired and realized
target/position labels, a separately bounded duration, and live/black actions
gated by Ready state, transition permission, and replicated
state. The desired target action is disabled while the opposite action remains
available for reversal. Its worker maps the typed intent to the current command,
preserves FIFO behavior, and a loopback protocol test
observes exact black then live desired and realized state. A hardware-gated
macOS daemon acceptance now sends current-protocol live-to-black and black-to-live
commands while Program recording is configured, decodes the H.264/AAC result,
requires ordered live/black/live video plus a sustained Master-audio silence
interval, and verifies the final live checkpoint. This proves the native FTB
path reaches one configured output on the exercised Metal/FFmpeg host. A
companion acceptance records a protocol-driven manual Fade at 75% forward,
25% reversed, cancelled, then fully progressed and committed; decoded video
must contain the corresponding ordered Program/Preview blend sequence, and the
checkpoint must be inactive with committed routing. Together these exercise
both halves of `SW-004` on one macOS recording output. Selecting the input
that is already the desired Program source as Preview is refused with
`invalid_command` before any state changes, so the persisted main-mix
projection can never carry equal Program and Preview; automation bindings and
shortcuts that target the Program input are refused the same way instead of
leaving a state no checkpoint could persist. Fullscreen presentation,
Windows/Linux adapters, and complete profile acceptance remain absent, so Phase
3 item 5 and `SW-002`/`SW-004` remain planned.

Current implementation boundary for logical outputs (`SW-012`): `fm-switcher`
provides a deterministic resolver for four logical output slots. Each slot
selects Program, Preview, an input, a mix, or a MultiView of up to 16 cells
(which may show other outputs), with all, clean, chosen-subset, or routed
overlays and a master, bus, or follow-video audio map. Edits are
all-or-nothing and reject duplicate project outputs, disabled dependencies, and
direct or chained MultiView recursion; resolution isolates live failures per
output, follows timed/T-bar transitions and FTB only on Program feeds, follows
secondary mixes through `MixBank` frames, and emits a render order that places
every output before the MultiViews showing it. Remaining gaps: the routing is
not persisted in `fm-model`/`fm-persistence`, has no switcher command,
protocol, engine, compositor, or delivery wiring, does not drive `MixBank` Main
from the bank itself, has no MultiView grid geometry for mixes or outputs, and
switches follow-video audio only at take completion. `SW-012` remains planned.


Current implementation boundary for layer flattening (`SW-018`):
`fm-compositor` plans and executes on the CPU path a flattening of nested scenes
held in a `SceneGraph`. A scene reference is inlined only when provably exact
(one fully contained layer under an untransformed, opaque, full-size
reference); `Precompose` marks a deliberate boundary; every other referenced
scene, including key/fill key sources, renders once per frame into a reusable
intermediate slot. Dependencies render most-memory-first and the reported peak
is simulated exactly and matched by the executor; depth, scene count, live
slots, and peak bytes are bounded with typed errors; each intermediate reports
its bytes, consumers, rescales, and straight-alpha reads; and output timing
passes through unchanged. Results are pixel-identical to recursive nested
composition across crops, masks, keys, effects, rotations, and Program,
Preview, and Operator outputs. Remaining gaps: no mapping from the project
model, no flatten flag in the model, persistence, or protocol, no freemixd, CLI,
or engine use, no GPU path, no cross-tick caching or memory fallback, and
counts rather than numeric error in the quality report. `SW-018` remains
planned.


Current implementation boundary for vertical and arbitrary canvases
(`SW-015`): the CPU compositor carries no 16:9 assumption, and `fm-compositor`
adds fit, fill, and stretch source placement with exact integer center-cut
geometry and quarter-turn handling, a center-cut safe-area guide that outlines
exactly what a portrait fill keeps, and an aspect-preserving multiview compile
so a portrait Program is not distorted in a landscape multiview. The simulated
pipeline and WebRTC 1080p ladder caps are orientation-independent, and project
validation requires even dimensions for 4:2:0 and 4:2:2 formats. Pixel-exact
CPU tests cover portrait, square, and odd canvases with letterbox, fill crops,
rotations, masks, safe areas, and multiview. Remaining gaps: model crops are
validated against project rather than source size, so a vertical crop of a
landscape camera on a portrait project is refused; no model, protocol, or UI
fit setting; Studio monitor wells are fixed 16:9; 8K stream ladders refuse
portrait rungs; and the native GPU path is untested. `SW-015` remains
planned.


Current implementation boundary for IN-008 and IN-010: `fm-video::test_signal` generates 75%, 100%, and EBU bars plus a bars/ramp/PLUGE layout with exact BT.709/BT.601 narrow-range 8/10-bit Y'CbCr and full-range RGBA codes, and a static generator renders solid colour or bars once per setting instead of per frame. `fm-playback::image_sequence` discovers numbered sequences (padding, duplicate detection, reject/hold/skip gap policies), maps time or output frames to sequence frames exactly with Once/Loop/Hold ends, and bounds a prefetch cache; `fm-codec-image::ImageSequenceSource` decodes bounded PNG/JPEG frames of one size, preloads and pins a sequence, and suggests the first opaque frame as a stinger cut point. ICC conversion, still fit/fill and reload, GPU-side generation, RP 219 raster geometry, daemon/CLI/model wiring, asynchronous decode, alpha video, and Studio controls are not implemented, so `IN-008` and `IN-010` remain planned.

Exit: `P0` switcher, composition, audio, display, record, and control rows pass.

Current implementation boundary for automix and monitoring (`AU-007`,
`AU-008`): `fm-audio` provides a deterministic gain-sharing microphone
automixer with per-channel weight and priority ducking, sample-domain
attack/release ramps, last-mic hold, optional NOM attenuation of the summed
mix, allocation-free processing, and per-block share and gain telemetry. A
monitor bus selects Master, PFL, or one of eight aux sources with level and a
fixed dim that can engage during talkback; talkback routes reach only guest or
camera destinations through read-only mono feeds that the type system keeps
out of the program Master, proved by a compile-fail doctest. Remaining gaps:
neither is wired into `MasterMixer`, the engine, persistence, protocol, or UI;
level detection is per-block mean square without hold time or adaptive
threshold; audio-follow-video automix is not modelled; `MasterMixer`'s
own PFL/AFL monitor bus (`AU-001`) is separate from this monitor-bus
selector; device selection is an opaque ID without I/O; and
talkback is not delivered to guest or camera transports. Both rows remain
planned.

Current implementation boundary for auxiliary buses (`AU-002`): `fm-audio`
provides a deterministic `BusMatrix` with Master plus aux buses A–G. Each strip
has a per-bus send enable, level, and pre- or post-fader tap, and strip mute
cuts every send; each bus has a unique name, fader, mute, and optional
`StripDsp` processing. One render validates everything first, then writes
every bus into caller-owned planes with per-bus peak/RMS meters. Destinations
are unique opaque device, network, or record IDs, and mix-minus feeds re-sum
without the excluded strip instead of subtracting, so each feed is
bit-identical to the bus with that send disabled, including through bus
processing. Remaining gaps: the matrix is not wired into `MasterMixer`, the
engine, persistence, protocol, or Studio and does not consume the existing
bus-send authoring; destinations perform no I/O; strips and buses share one
channel count without mapping or balance; there are no gain ramps, clipping
policy, or meter ballistics; and allocation-free rendering is by construction
rather than tested. `AU-002` remains planned.

Current implementation boundary for multichannel audio (`AU-003`): `fm-audio`
provides a 1–16 channel layout catalog (1.0, 2.0, 2.1, 5.1, 7.1, and 16
discrete) in SMPTE/ITU order with short labels, and a validated sparse N-to-M
gain matrix that renders planar f32 into caller buffers without allocation and
copies unity routes bit-exactly. Documented-coefficient downmixes cover
ITU-R BS.775 5.1-to-stereo with selectable LFE handling and clip-preventing
normalization, 7.1-to-5.1, and stereo-to-mono, alongside upmix by
duplication. SDI-style 16-channel groups de-embed by mono, pair, or group, and
an embedded-audio map re-embeds layouts while preserving labels. `fm-types`
channels now include rear surrounds (`Lrs`/`Rrs`) and discrete `1..=16`
labels with layouts capped at 16 channels, so speaker-label conversions are
total and 7.1 and 16-channel blocks travel through `AudioBlock`, the channel
mapping (now up to 16 channels, buildable from any gain matrix), and the
Master mixer, including gain-matrix downmixes to a stereo Master. The project
JSON contract names them `left_rear_surround`, `right_rear_surround`, and
`discrete_1`..`discrete_16`; recordings map them to exact ffmpeg layouts
(7.1 as `FL FR FC LFE BL BR SL SR`, discrete as unspecified-order `{n}C`).
Remaining gaps: project settings still allow at most 8 audio channels;
nothing reaches persisted strips, protocol, daemon, or Studio; streaming
outputs refuse 7.1 and discrete layouts; there is no device ancillary-data
extraction, composite or Lt/Rt downmix, or hardware evidence. `AU-003`
remains planned.

Current implementation boundary for system audio inputs (`AU-011`):
Protocol 2.55 (schema 52) adds an audio-only input kind that captures the
server's own sound. The operator names a PulseAudio/PipeWire source (for
example `@DEFAULT_MONITOR@` or `<sink>.monitor`) or an ALSA PCM (for example
`hw:Loopback,1`), checked against the same device rule on every surface; a
show holds at most 8. The input is an undoable add, a replaceable source, and
persisted exactly. The native daemon on Linux opens each device through an
audio-only `FFmpeg` receiver (`-f pulse|alsa -i <device>`) that keeps only
`PATH`, `HOME`, `XDG_RUNTIME_DIR`, and the `PULSE_*` variables, paces 2x2
pictures from the audio, and mixes the input on Program like any other.
A device that does not open, or goes away, reopens with backoff
(500–5000 ms, never giving up). Each input's state reaches clients as
`system_audio_status` and the snapshot's `system_audio_inputs`. Other
platforms refuse a live add with `runtime_unsupported`. Studio's Add input
dialog has a System audio kind with audio-system keys and a device field;
the tile shows OPENING / LIVE / REOPENING n / FAILED with its details. The CLI
adds `input-add-system-audio`, `input-system-audio-status`, their `remote-`
forms, and the `system-audio:<backend>:<device>` replacement source.
A codec test reads the real ALSA `null` device and a missing PCM. A native
process test plays a paced tone through a wrapped `FFmpeg` onto Master, checks
the arguments and the environment the child keeps, and adds a missing card
live, which keeps reopening with the real error.

Remaining gaps:
- the server's audio devices are not listed, so the device is typed;
- there is no edit command beyond replacing the source;
- capture is stereo only, with no channel selection or per-input delay;
- there is no Windows (WASAPI loopback) or macOS capture, and no
  per-application capture;
- there is no virtual audio device publishing Program or buses;
- the simulated daemon reports every system audio input as idle;
- there is no evidence from a real PulseAudio or PipeWire server.

`AU-011` remains planned.

Current implementation boundary for secondary mixes (`IN-023`): `fm-switcher`
provides a deterministic `MixBank` with Main plus fifteen addressable
secondary mixes, each with its own preview/program, cuts, timed transitions,
and output-only mode, and every command is validated before any state
changes. Mix-to-mix routing refuses cycles through program, preview, or a
running transition and follows one timing rule: a read of another mix sees
that mix's previous frame, frame zero reads black, and delays accumulate along
a chain. Overlay and stinger effects target one or several mixes and apply all
or nothing at the next frame boundary. Remaining gaps: the bank is not wired
to `fm-engine`, the protocol, persistence, the compositor, or Studio, so no
secondary-mix output renders; stinger effects do not check slot readiness or
fallback; and overlay fades, mix-sourced overlays, T-bar, Fade-to-Black, and
audio are absent. `IN-023` remains planned.

Current implementation boundary for keying (`SW-008`): beyond the existing
chroma and luma keys, rectangular masks, and straight or premultiplied alpha,
the `fm-compositor` CPU reference adds a bounded auto key that samples a border
or region, takes a fixed-point BT.709 chroma median, classifies green or blue
backing, and yields manual chroma parameters; matte refinement with black and
white clip, choke or spread up to eight pixels, and exact green or blue
despill that only lowers channels; and key/fill layers whose alpha comes from
a same-size key source's luma. The planner rejects conflicting keys and
refinements without a key. Remaining gaps: auto key is a one-shot sample
assuming an opaque backing, the native GPU path refuses every key, nothing is
persisted or exposed in Studio or the daemon, key/fill is not tied to paired
SDI or NDI inputs, distance is per RGB channel rather than perceptual, and
edge blur, garbage mattes, and light wrap are absent. `SW-008` remains
planned.

Current implementation boundary for freeze and snapshot (`SW-016`): `fm-video`
provides a per-input freeze controller that latches the first source frame at
or after a requested index, repeats it on the output cadence while holding at
most two frames, and resumes live with output timestamps of
`max(map(source_pts), last_output_pts + d)` on a grid anchored at the first
source timestamp, so output never regresses. A std-only PNG encoder writes
exact RGBA8 and RGBA16 stills with stored DEFLATE blocks, CRC32, and Adler32,
checked by golden bytes and a structural decoder. Remaining gaps: no engine,
scheduler, simulator, protocol, daemon, or UI calls either piece; timestamps
are bare ticks rather than `fm-frame` timing types; audio and clock-domain
conversion are absent; and stills carry no color metadata or compression.
`SW-016` remains planned.

Current implementation boundary for video delay (`IN-022`): `fm-video`
provides a `VideoDelayLine` timed purely by media timestamps that presents the
newest frame at or before `now - delay`, rounding fractional-frame delays up
and never moving backwards; a one-hour 29.97 run at a five-frame delay selects
every frame exactly, and 29.97/59.94 conversion works both ways. Startup holds
black (or nothing) until filled, live delay changes apply immediately or slew
at a bounded rate, late or duplicate frames within the skew limit of the
newest buffered frame are dropped and counted without clearing the buffer,
flagged discontinuities and timestamp jumps beyond the skew limit reset and
re-anchor, and frame-count, byte, and duration bounds evict oldest-first and reject
oversized frames. The effective delay converts to audio samples, aligning with
`fm-audio` `SampleDelay` within one sample at 48 kHz. Remaining gaps: no
engine, input, command, protocol, persistence, or UI wiring; no loop, freeze
integration, or clip save; `SampleDelay` cannot follow live changes or exceed
one second; no source/output clock drift compensation; and memory accounting
relies on caller-reported sizes. `IN-022` remains planned.

Current implementation boundary for virtual and mix-feed inputs (`IN-024`,
`IN-025`): project schema 25 added alias inputs with their own id,
name, position, audio strip, and crop over a target input, and Program/Preview
feed inputs that show the previous tick's committed bus frame (black before the
first commit). Validation rejects missing, self, and cyclic alias targets and
closes scene and audio cycle checks through aliases, while mix feeds add no
cycle edges because the one-frame delay is the explicit break. `fm-model`'s
`SourceResolver` maps any input to its concrete source with feedback detection
and a finite dependency render order, and its borrow structure forbids
committing a feed frame while a tick is still resolving. Remaining gaps: no
renderer (native, simulated, CLI, or monitor tap) consumes the resolver, so
these inputs are refused or skipped at runtime; there is no CLI, protocol, or
Studio authoring; feedback ignores the second input of an active transition;
and aliases do not yet carry independent effects, shortcuts, or layer state.
`IN-024` and `IN-025` remain planned.

## 6. Phase 4 — output, stream, and headless reliability (10–14 weeks)

1. Add RTMP/RTMPS and SRT outputs with independent state.
2. Add five simultaneous destinations and shared-rendition planning.
3. Add multi-bitrate output and local HLS/LiveLAN equivalent.
4. Add second program recorder, segment rules, WAV/multichannel audio.
5. Add ISO/MultiCorder graph taps and timecode manifest.
6. Implement the per-user `freemix-capture-node` and certify service-to-session
   screen/camera/application-audio permission, clock, reconnect, and logout
   behavior.
7. Implement engine OS service mode, health/readiness, support bundles.
   The non-native scheduler gives established peers a bounded retryable shutdown
   notice before it returns an internal command error.
8. Implement remote browser control, WebRTC multiview preview, roles/pairing.
9. Add disk/network/GPU preflight and alert policy.
10. Run process-kill, disk-full, network impairment, and 24-hour soak. (needs to be in a VM or something else, don't kill my pc)
11. Publish the first supported hardware/capability matrix.

Current implementation boundary for item 1: the transport-independent
`fm-io-network` output state machine starts each configured destination on its
primary endpoint. A retryable primary connection or write failure retains the
bounded packet queue, keeps the existing retry budget and delay, and selects
the configured backup endpoint for subsequent attempts. After a reconnect, it
removes queued interframes before the first random-access packet and waits for
one when the queue has none. The first incoming random-access packet has queue
priority when that recovery queue is full only with interframes. This recovery
is transport-neutral queue policy; it does not request a keyframe. Backup
failures do not alternate back to the primary, while a manual stop and start
selects the primary again.
Non-retryable failures remain terminal.

Protocol 2.16 adds runtime control for configured streaming destinations. The
engine owns bounded desired stream inventory (at most eight targets) with
exact per-target start/stop commands authorized as transition operations, one
durable `streams_changed` event per accepted command, replay-safe receipts,
and idle-snapshot validation; schema 19 persists each target's desired running
flag, so restarts restore desired state through journal replay, and the daemon
mirrors engine desired state into the durable project before acknowledgement.
Native `freemixd --native-media` compiles every configured RTMP/RTMPS target
into its own readback owner and `fm-codec-ffmpeg` streamer at startup,
reconciles desired-running targets whose startup policy is
`ReconcileDesiredState`, feeds each from the Program capture site after the
recorder with cloned audio and independent sequence ledgers, realizes live
starts at the next frame boundary, retires stopped streams off the render
path, emits sanitized per-target `FREEMIXD_STREAM` finalization records, and
adds enqueue/drop counters to telemetry; startup or feed failures latch
per-target and degrade rather than abort the show. The CLI exposes local and
remote `stream-start`/`stream-stop`, and status prints the validated roster.
Native sessions now route every target through the `fm-io-network` state
machine: one shared `OutputSet` with per-target sinks and renditions, so a
retryable failure retains the bounded queue and fails over from the persisted
primary to the backup endpoint under a bounded reconnect budget (250 ms
doubling to 2 s across at most 40 attempts), refused frames are dropped rather
than retried, `WaitingToReconnect` and `Congested` are reachable wire states,
and final `FREEMIXD_STREAM` records derive from state-machine telemetry plus
the per-target ledger. Connection accounting is delivery-proven: an attempt
clears the retry budget, and a scheduled reconnect counts as a recovery, only
once its session has actually handed media to the transport's muxer, so a
spawned-but-refused attempt (for example an FFmpeg child that exits because the
ingest rejected its connection) keeps consuming the budget and an unreachable
destination latches failed after its bounded attempts instead of reconnecting
forever. The stream inventory is bounded at five everywhere —
authoring, engine, wire, and transport agree — matching the five-simultaneous
destinations product contract. There is no output-health UI beyond Studio's
compact panel, no live decoder acceptance of a streamed broadcast against a
real receiver on this host (the ffmpeg listener integration skips without
ffmpeg/libsrt), and no hardware encoder, so item 1 and `OR-005` remain
planned. The model and sink accept SRT alongside RTMP/RTMPS:
`srt://host[:port]` endpoints compose `?streamid=` URLs with the same key
redaction, the FFmpeg sink muxes MPEG-TS with its own channel-layout rules,
and a real-ffmpeg integration receives an SRT broadcast and probes H.264/AAC.
Schema 20 persists each target's authored video bitrate and native sessions
thread it into the encoder. Destinations whose authored project `OutputId`,
scheme, and authored bitrate match share one GPU readback of that project
output's rendered texture (preserving its per-output overlay inclusion), one
packed raw pair per frame, and one group sequence cursor, fanned out through
the rendition planner over exactly the group's active members with per-route
results mapping back to per-target ledger drops and congestion flags; distinct
project outputs or bitrates stay independent groups, a stopped member leaves
its plan, and a restart resumes the same group past the gap. Protocol 2.17 adds the
lossy latest-wins `stream_status` peer record beside audio meters: native
`freemixd` publishes per-target realized state, counters, and sanitized
failures each frame interval; sessions validate identity and monotonic
sequence with meter semantics and retain the latest record for operator
status; Studio swallows the records beside meters and renders the roster with
live samples. Snapshots still project only desired state; the lossy record is
transport state and never durable.

Current implementation boundary for item 2 (`OR-006`): five simultaneous
destinations with independent control (schema 49, Protocol 2.52). Every
destination persists its own reconnect policy: first delay, maximum delay,
and an attempt budget or unlimited, bounded 100 ms–300 s and 1–10,000
attempts, with a default of 250 ms / 2 s / 40. The native output set backs
off exponentially (doubling) within that policy and then latches the
destination `Failed`. Each destination keeps its own FFmpeg child and a
bounded four-pair queue that drops when full, so a dead ingest never stalls
Program, the recorders, or the other destinations. Destinations with the
same encoder identity still share one readback and packed pair.
Protocol 2.52 adds:
- `stream_retry`, which is refused unless the destination is desired
  running. It stops the destination, clears a latched failure, and restarts
  it on the next frame with a fresh budget.
- `stream_start_all` and `stream_stop_all`. These change only destination
  desired state and are never seen by a recorder.
- `stream_target_set` and `stream_target_remove`. These are undoable,
  padlock-guarded project edits, refused while the destination is desired
  running and for a sixth destination, a duplicate name, an unknown output,
  or (for removal) a destination a cue list uses. The stream key is a
  `SecretEdit`: absent keeps it, empty clears it, and `***` is refused.
- The snapshot's `stream_targets` and the durable `stream_target_changed`
  event. Both carry each configuration with only `has_key`, never the key.
- Reconnect count, current-session uptime, and the encoded bitrate over a
  two-second window in each `stream_status` sample.

Native `freemixd` realizes destination edits live. A removed or edited
destination's runtime retires. A new or changed one takes a free output-set
slot and joins its encoder-identity group (or an emptied or new group),
while running destinations keep their rendition ids. Failure text in a
sample is clamped to the wire's detail bound.

Studio's Recording & streams panel lists every destination with:
- per-row start, stop, and retry;
- a status chip whose hover shows state, bitrate, dropped frames,
  reconnects, uptime, and last error;
- edit and remove, disabled while the destination runs;
- Start all and Stop all;
- an add/edit dialog whose stream key is masked behind an eye toggle.

All of this has English and Spanish hover help. `freemix-cli` has local
`stream-start-all`/`stream-stop-all` and `--reconnect` on
`stream-add`/`stream-update`, plus `remote-stream-retry`,
`remote-stream-start-all`, `remote-stream-stop-all`, `remote-stream-set`,
`remote-stream-remove`, and `remote-streams`.

A native process test covers the full run:
- four FFmpeg RTMP listeners plus a local HLS destination run at once, two
  of them added live;
- one killed ingest gives up within its budget while the others and a
  concurrent recording keep going;
- a retry brings it back;
- a group stop leaves the recording running;
- no key appears on the wire, in snapshots, or on stderr.

The `--hls-listen` route table follows live destination edits: a local HLS
destination added while the daemon runs is served at once, and a removed or
moved one stops being served. A native process test covers both.

Protocol 2.54 (schema 51) adds SRT caller options. An `srt://` destination
persists a latency (20–8000 ms, default 120) and an optional AES passphrase
(10–79 printable bytes). The passphrase is a `SecretEdit` like the stream
key, stored only in the bundle and reported only as `has_passphrase`. The
native output publishes MPEG-TS with `-mode caller -latency` and, when set,
`-passphrase`, under the destination's own reconnect policy, and masks the
passphrase in worker diagnostics. Studio's destination dialog adds the
latency, an Encrypt checkbox, and a masked passphrase field;
`stream-add`/`stream-update` take `--srt-latency-ms` and `--srt-passphrase`,
and `remote-stream-set` also takes `--clear-srt-passphrase`. A native
process test streams an encrypted caller into an encrypted `ffmpeg` SRT
listener, probes H.264/AAC, and reconnects after the listener restarts. It
skips without `libsrt` unless `FM_REQUIRE_FFMPEG_SRT=1`.
`stream_target_set` also carries an optional `position`. The undo of a
removal uses it, so the destination returns to its old place, live, after a
journal replay, and in the saved project. A native process test covers
this; before the fix it came back last.

Remaining gaps:
- the SRT passphrase reaches the `ffmpeg` child's command line;
- there is no SRT listener or rendezvous output, and no SRT link
  statistics for destinations;
- a running destination must be stopped before it can be edited;
- there is no per-destination encoder override beyond bitrate;
- the simulated daemon accepts every command but realizes no transport.

`OR-006` remains planned.

Current implementation boundary for SRT sessions (`IN-017`): `fm-io-srt` is a
deterministic, socket-free SRT configuration and session contract with caller,
listener, and rendezvous modes (listener stream-ID selection, rendezvous peer
checks), validated latency, encryption, and reconnect settings, secret
references redacted from debug output, bounded reconnect backoff, bounded
packet queues, and RTT/loss/retransmit/bandwidth/queue-drop statistics. No
other crate uses it yet. The only real SRT path is the FFmpeg output sink in
caller mode, verified against a real listener as MPEG-TS with H.264/AAC.
SRT input now runs through the network stream input (see the IN-015
boundary below): a `srt://` stream input (schema 46, Protocol 2.49) is a
caller, listener, or rendezvous with a receive latency (20–8000 ms), an
optional AES passphrase (10–79 bytes) with a 16/24/32-byte key length, and
an optional stream ID, received through `ffmpeg`'s `libsrt` as MPEG-TS and
decoded like any other stream input. Up to eight source audio channels are
downmixed or one stereo pair (1-2, 3-4, 5-6, 7-8) is picked; status reports
the source channel count. A listener reports `listening` while it waits and
listens again when its caller leaves, without counting reconnects; two
inputs never listen on one UDP port (`listen_port_in_use`). The passphrase
is stored only in the project and is `***` on every other surface (events,
snapshot, status errors, CLI, logs). Verified over loopback with `ffmpeg`
SRT callers and listeners, encrypted with a matching and a wrong
passphrase, and a native `freemixd` receiving an encrypted caller.
Remaining gaps: SRT link statistics (RTT, loss, retransmits) are not
reported, since the `ffmpeg` CLI does not expose them (status keeps
bitrate, buffer, dropped and late frames, and reconnects); `fm-io-srt` is
still unused; the passphrase reaches the `ffmpeg` child's command line; no
SRT listener or rendezvous output; tests skip without `libsrt` unless
`FM_REQUIRE_FFMPEG_SRT=1`; not verified on Windows or macOS or at UHD, so
`IN-017` remains planned.

Current implementation boundary for RTMP inputs (`IN-016`): an `rtmp://`
or `rtmps://` stream input (schema 46, Protocol 2.49) either calls a server
(pull, the optional stream key appended to the application path) or, as an
`rtmp://` listener, accepts a push (OBS-style) on its configured port
(1024 or above) and application; a publisher using another stream key is
dropped with "a publisher used another stream key", since `ffmpeg`'s
listener only warns. The listener reports `listening`, goes live on the
push, and listens again when the publisher leaves, without counting
reconnects; two inputs never listen on one TCP port
(`listen_port_in_use`). The stream key is stored only in the project and
is `***` everywhere else, the URL path after the application likewise.
Studio's Add input stream form gains mode keys and the key field with
hover help; `freemix-cli` gains `--mode` and `--stream-key`. Verified over
loopback with an `ffmpeg` publisher pushing to the listener (right and
wrong key, relisten) and an `ffmpeg` RTMP server for the caller, and a
native `freemixd` taking a push onto Program. Remaining gaps: no `rtmps`
listener (no TLS certificate handling); the key reaches the `ffmpeg`
child's command line; the listener's `listening` state is reported as its
child starts, slightly before the port is bound; only one publisher per
input; not verified on Windows or macOS, so `IN-016` remains planned.

Current implementation boundary for network stream inputs (`IN-015`): a
`network_stream` input kind (schema 43, at most 16 per show) holds an RTSP,
RTP, UDP, TCP, HTTP(S) MPEG-TS, or HLS URL with an RTSP transport hint, a
40–5000 ms jitter buffer, a reconnect backoff with an optional give-up count,
a timeout, and a freeze or black stall slate. URL credentials and secret query
values are stored only in the project and redacted in every snapshot, event,
status, CLI listing, and log line. Protocol 2.46 adds it to `add_input`,
edits it with `input_stream_set`, and reports per-input receive state
(connecting, buffering, live, stalled, reconnecting with attempt and next
delay, failed) with bitrate, buffer depth, dropped and late frames,
reconnects, and the last error. `fm-codec-ffmpeg` receives each stream
through an `ffmpeg` child that decodes RGBA video scaled to the program
format and f32 stereo audio, into a jitter buffer that holds the configured
latency and never more than twice it (dropping the oldest frame and its
audio). One frame is pulled per engine frame, so the audio follows the engine
clock. On a stall the input holds its last frame or shows black and its audio
fades to silence. The worker reconnects with backoff after a timeout or EOF and
fails once the give-up count is reached. None of this blocks the Program
render thread. A native `freemixd` opens the project's stream inputs at
startup, connects a stream input added live (or by undo/redo) without a
restart, stops a removed one's receiver, restarts a receiver when its
settings change, and publishes status (`freemixd --test native_live_inputs`
adds, airs, hears, and removes one over loopback UDP).
The Studio Add input dialog has a validated stream form, and input tiles show
a LIVE/BUFFERING/RECONNECTING n/FAILED badge. `freemix-cli` has
`input-add-stream`, `input-stream-set`, and `input-stream-status` with
`remote-` forms. Verified with loopback UDP, TCP, HTTP, and HLS sources, sender
kill and recovery, and the jitter bound.

Remaining gaps:
- Any settings edit reconnects the stream.
- The simulated daemon reports every stream idle.
- `ffmpeg` is required at runtime. Without it the input stays reconnecting
  with "ffmpeg unavailable".
- Audio is received as stereo.
- A stream found without audio is treated as video-only until its input
  reopens.
- The decoded-byte cap (256 MiB) can shorten the jitter target at large
  formats.
- Pictures are stretched to the program format.
- There is no hardware decode, and RTSP authentication is limited to URL
  credentials.
- Not verified on Windows or macOS hardware or at the profile-a-hd load.

`IN-015` therefore remains planned.

Current implementation boundary for local capture inputs (`IN-001`, Linux):
a capture input (schema 44, `InputKind::Device`, at most 16 per show) names a
device by its adapter-qualified stable key and carries an optional requested
format (size, rate, YUYV/NV12/MJPEG, progressive or top/bottom-field-first
interlaced), an audio choice (none, the device's paired ALSA card, or a named
ALSA device), and a hold-last or black loss slate. Protocol 2.47 adds it to
`add_input` (`kind=capture`), edits it with `input_capture_set`, carries
`capture_inputs` in the snapshot, reports per-input `capture_status` (idle,
starting, live, signal lost with unplugged/stalled/no-signal/error reason,
failed; node, negotiated format, audio, frames, losses, recoveries,
renegotiations, last error) and the device inventory as `capture_devices`
runtime events, and answers a `capture_devices_request`. The new
`fm-io-linux` crate enumerates V4L2 nodes through the pure-Rust `linuxvideo`
binding (capture-capable nodes only; YUYV, NV12, and MJPEG with discrete,
stepwise, and continuous sizes and intervals; field order), keys each device
`v4l2:<bus>#<card>` with an ordinal for identical cards on one bus, pairs it
with the ALSA card on the same USB/PCI parent through sysfs, and polls for
hot-plug. A per-input supervisor uses the requested format when offered,
else the nearest one (same scan, smallest size covering the request or the
program, closest rate, then uncompressed formats first), and acquires through an `ffmpeg -f v4l2`
child (plus `-f alsa` audio, falling back to video-only when the audio device
cannot open) that deinterlaces with `yadif` and scales to the program format,
and detects unplug, stalls (1 s without pictures), and no-signal. On loss
the input holds its last picture or shows black with silent audio, and it
reacquires the same device (stable key, then same card and ordinal, then
node path) under the same input ID, at another node if it re-enumerates
there; a change in the device's formats renegotiates. A native `freemixd`
on Linux opens the project's capture inputs at startup, and adds, removes,
replaces, and re-configures them live. `--diagnostic-capture-inventory FILE`
substitutes a deterministic fake backend driven by a rewritable inventory
file (`freemixd --test native_capture` covers go-live, the device's picture
on Program, unplug to the black slate, replug at another node with the same
input ID, renegotiation, inventory requests, a settings edit, and a live add
and remove). The Studio Add input dialog offers Camera/capture with the
daemon's devices and a format picker, and input tiles show a SIGNAL LOST
badge. `freemix-cli` has `input-add-capture`, `input-capture-set`,
`input-capture-status`, and their `remote-` forms, plus
`remote-capture-devices`.

Remaining gaps:
- No real V4L2 frame acquisition, hot-plug, or ALSA evidence has been
  recorded; `fm-io-linux --test v4l2_real` only runs on a host with a
  capture device (`FM_REQUIRE_V4L2=1` makes its absence a failure).
- Acquisition needs `ffmpeg` at runtime and decodes through it in software;
  there is no zero-copy or hardware path, and pictures are stretched to the
  program format.
- Audio is stereo and follows the engine clock without drift correction.
- macOS keeps the camera-helper path from startup only; its live capture
  add/remove stays refused, and there is no Windows adapter.
- The simulated daemon reports every capture input idle and has no device
  inventory.
- Not verified at SD/UHD rates on hardware or at the profile-a-hd load.

`IN-001` therefore remains planned.

Current implementation boundary for item 3: the authored model now carries a
local HLS destination and an optional multi-bitrate HLS ladder.
`StreamProtocol::Hls` is a keyless protocol whose endpoint is one absolute
local directory; the model refuses a stream key or backup endpoint for it, and
schema 22 persists `"key": null` for an HLS target while network protocols keep
requiring a key. An HLS target may additionally carry a ladder of two through
eight rungs, each an even width and height with a video bitrate, requiring
strictly increasing bitrates and nondecreasing dimensions; a ladder is refused
on RTMP/RTMPS/SRT, and the destination's reported bitrate becomes its top rung.
`fm-codec-ffmpeg` muxes the same bounded raw-pair loopback inputs and per-target
admission, padding, progress, stderr, and stop/kill contract through the FFmpeg
`hls` muxer. A single-rendition destination writes `<directory>/index.m3u8`
plus `segment-%05d.ts`; a laddered one encodes every rung in the same child
through a split-and-scale filter graph with per-rung video and audio encodes,
publishes the master playlist at `<directory>/index.m3u8`, and writes variant
playlists and segments under pre-created `<directory>/<rung>` directories
through FFmpeg's `var_stream_map`. Segment duration follows the encoder's
keyframe interval, every playlist keeps a bounded six-segment window with
`delete_segments+independent_segments+temp_file`, HLS follows the MPEG-TS
channel-layout rules, and startup creates each missing leaf directory before
any child spawns while refusing a path that is not a directory.

The transport adds `Endpoint::local_directory` for `OutputProtocol::Hls` and
the sink composes `hls://<absolute/dir>`, so an HLS target — single-rendition
or laddered — obtains the same `OutputSet` queue, congestion, retry, and stop
semantics as every network destination even though it has no peer to fail over
to. Native `freemixd` accepts the authored URL in its destination splitter,
maps the authored ladder into the sink's rung type, compiles each HLS target
into its own `(protocol, bitrate)` rendition group with one GPU readback and
one FFmpeg child, and renders the path verbatim in operator text because a
local directory is not a secret. The CLI authors `hls:///absolute/dir <name>`
without a key positional, refuses `--backup` for it, authors repeatable
`--rung <bitrate_kbps>:<width>x<height>` options under the model's ladder
rules, and prints the path, authored bitrate, and ladder. No protocol change
was needed: protocol 2.18 snapshots already carry target identity, name, and
desired state, and the lossy `stream_status` record carries the realized state.

This remains one local HLS output per destination. `fm-io-network` now provides
bounded deterministic M3U8 rendering and parsing for both media playlists
(`HlsPlaylistMetadata::render_m3u8` / `parse_m3u8`) and master playlists
(`HlsAbrCoordinator::render_master_m3u8` / `parse_master_m3u8`), validating
FFmpeg-formatted master and variant playlists through `HlsAbrCoordinator`'s
sequence and independent-segment alignment rules, and the native daemon now
consumes those parsers for local output health: every five seconds it reads
each HLS destination's `index.m3u8` (following a master's first in-directory
variant under the same route allowlist the HTTP listener uses), parses the
media playlist, and publishes the segment count, target duration, and the
directory's free bytes through the lossy `stream_status` record as protocol
2.19's optional local fields; unreadable, malformed, or escaping playlists
publish no local health and never latch. Studio appends
`local=<segments>seg/<ms>ms/<free>B` to a row that carries it. An optional
token-guarded HTTP
listener (`--hls-listen` plus `FREEMIXD_HLS_TOKEN`, loopback-only, no TLS)
serves those outputs at `/v1/hls/<target-id>/<relative>` with a fixed route
allowlist — `index.m3u8`, `segment-NNNNN.ts`, and their `0..=7` ladder-rung
variants — exact content types and lengths, `HEAD` support, 401/404/405/431
handling, bounded request heads, bounded concurrent connections, and
per-phase deadlines, so a player can pull the live playlists and segments
without a directory listing or path traversal. That listener is serving
groundwork only: there is no LAN binding or TLS, no Web Controller page or
renderer-visible viewer, no per-viewer or capacity bandwidth reporting, and
no per-rung persisted audio setting or codec choice. The ladder is
one destination's ABR encoder into one directory rather than independently
controlled destinations, so the five-destination bound still counts each
authored target once and a remote single-key or multi-key RTMP ladder is not
implemented. Studio and Web show the existing roster only. `OR-007` and
`OR-017` therefore remain planned, and item 3 remains incomplete.

Current implementation boundary for `OR-017` (LiveLAN, Protocol 2.57,
schema 53): LiveLAN is project configuration (`enabled`, Program or one
output, a 1..=32 viewer limit, and an optional 4..=12-digit PIN stored like
a stream key) edited by the padlocked, undoable `live_lan_set` command from
Studio's Output › LiveLAN viewers… window and the CLI's `live-lan-set` /
`remote-live-lan-set` (`live-lan` / `remote-live-lan` print it, never the
PIN). Native `freemixd` with `--live-lan-listen <addr:port>` (any address,
LAN included) serves a viewer page, PIN-gated cookie sessions with the
viewer limit, and a one-second-segment H.264/AAC HLS stream of fragmented-MP4
(CMAF) segments of the chosen source, encoded off the render loop into a
private temporary directory; it reports `live_lan_status` (state, viewer
URL, viewers, egress kbps). The page hands the playlist to browsers that play
HLS themselves (Safari, iOS, Android, recent Chromium) and otherwise plays it
through its own small Media Source Extensions player (desktop Firefox, and
Chrome or Edge without native HLS): it fetches the playlist, `init.mp4`,
and the media segments with the session cookie, starts near the live edge,
follows it, skips
missing segments, and joins again when its session ends (a source change or
an encoder restart ends the sessions served the old stream). Playback of
both paths was confirmed in headless Chromium, the MSE path by forcing it;
Firefox, Safari, and mobile browsers were not exercised. The PIN is compared
in constant time including its length; wrong PINs lock an IPv4 address or
IPv6 `/64` out for a minute after each run of five, doubling per further run
up to an hour (with `Retry-After`). On Linux, a new daemon removes temporary
directories left by daemons that are gone. There is no TLS, no per-viewer
bandwidth breakdown, no measured latency class, and the supervised Studio
daemon does not start a listener. `OR-017` therefore remains planned.

Current implementation boundary for item 4: native `freemixd` runs two
independently configured program recorder destinations with per-slot project
output selection. Slot one is `--record-program` (with optional
`--record-output <output-id>`) and slot two is `--record-program-2` (with
optional `--record-output-2 <output-id>`); both require native media, need not
be configured together, and each keeps its own output path, selected project
`OutputId`, segment derivation, exclusive files, finalizer worker,
capture-failure latch, and sanitized `FREEMIXD_RECORDER` report, now prefixed
`slot=1|2`. Startup validates any selected `OutputId` fail-closed against the
project's configured outputs before opening a recorder. When a slot names an
`OutputId`, it captures that output's independently realized GPU texture (so
slot one and slot two can record different overlay inclusions, such as Program
with overlays and a clean feed, concurrently); when omitted, the slot defaults
to the first configured project output or the unbound Program texture. The two
slots share the engine's existing desired recording flag, so an accepted
`RecordStart`/`RecordStop` opens or drains both; one slot's start, prime, or
finalization failure latches and reports only that slot and never aborts the
show or the other slot. Each slot taps the Master audio block, cloning the
audio only when both slots run and resolve an output texture, and each owns its
own readback. Startup remains fail-closed for any configured recorder,
`FREEMIXD_TELEMETRY` aggregates both slots' outstanding and retained values,
and disk preflight and free-space sampling cover both parents. Each slot can
also write a split WAV sidecar: `--record-wav` now requires at least one
recorder and applies to every configured slot, making the same bounded FFmpeg
child mux a linear-PCM `pcm_s16le` WAV output for the same captured audio at
the project's sample rate and channel layout, from mono through 5.1, into an
exclusive file that shares the segment's stem, so `program-002.mp4` is
accompanied by `program-002.wav`. The final recorder report carries
`wav_bytes`, `video_bitrate_kbps` and `audio_bitrate_kbps` (each `unset` when
the slot keeps encoder defaults), `duration_ms` (the exact completed-pair span
at the project frame rate), and `free_bytes` (the segment parent sampled when
the report is frozen, zero when unreadable); a killed child may leave a
truncated sidecar, and only a clean stop finalizes it. Each slot also carries
optional video and audio encoder bitrates (`--record-bitrate-kbps[-2]` bounded
1,000 through 100,000 kbps and `--record-audio-bitrate-kbps[-2]` bounded 8
through 1,024 kbps, each requiring its slot's recorder); a configured bitrate
becomes the child's `-b:v`/`-maxrate`/`-bufsize` or `-b:a`, while an unset
value keeps the encoder defaults and the previous byte-identical argv. Every
segment also embeds a non-drop-frame start timecode derived from its absolute
first frame index and the project's nominal frame rate (`-timecode`), so each
segment, including every automatically rotated one, carries its own timeline
position. Protocol 2.20 makes recording control per slot: `record_start` and
`record_stop` address one of the two recorder slots, every acceptance emits one
durable `recording_changed { recorder, active }` event carrying that slot's
resulting flag, and snapshots carry the fixed two-slot `recordings` roster;
schema 23 persists one desired flag per slot. An accepted command opens or
drains only the addressed slot, startup reconciliation starts each configured
slot whose restored flag is true, and one slot's start, prime, or finalization
failure latches and reports only that slot. The CLI takes `--recorder 1|2`
(default 1) on local and remote record commands and prints both flags, and
Studio renders a two-row permission-gated `RECORDING` panel — one row per slot,
each showing that slot's replicated desired flag with exactly one Start or Stop
action behind the Ready, view, and transition-permission gates, disabled only
while that slot's own record intent is unresolved, and dispatched through the
existing worker FIFO as the slot-qualified protocol commands.

Program segments also rotate automatically at exact frame boundaries.
`--record-segment-seconds <1..=86400>` makes every configured slot close its
current segment once its accepted-pair count reaches
`ceil(seconds * rate.numerator / rate.denominator)` for the project frame rate.
The completed segment is handed to a background finalizer, the next
deterministic `<stem>-NNN.mp4` (with its `<stem>-NNN.wav` sidecar when
`--record-wav` is set) opens at the boundary frame with the same dimensions,
frame rate, sample rate, and channel layout and `first_sequence` equal to that
frame, and the boundary frame is then captured into the new segment, so no
frame is dropped or duplicated across the seam and finished segments drain and
report off the render path. A rotation that cannot derive or open its next
segment latches only that slot and reports a sanitized `rotate:` notice while
the show and the other slot continue.

The two slots are independently controllable but still share the project
encoding format and Master audio: there is no per-recorder codec choice,
resolution scaling, audio-map, or bus/channel selection, no frame- or
byte-based segment policy in the daemon (each slot carries its own duration
rule through `--record-segment-seconds` and `--record-segment-seconds-2`, while
frames/bytes policies live in the unwired `fm-record` spool), no
multiple-track or metadata handling, and no authored recorder roster beyond the
fixed two slots — no names, add/remove, or per-slot startup policy. The
codec/resolution/audio-map configuration and `AU-010` bus selection remain
planned; `AU-010` still depends on the planned `AU-002`/`AU-003`, so `OR-001`,
`OR-002`, and `AU-010` are unchanged and item 4 remains incomplete.

Current implementation boundary for item 5: `fm-record`'s crash-safe packet
spool now carries ISO/MultiCorder tap and timecode manifest metadata plus
duration-based segmentation. `SegmentPolicy` gains `max_duration` through
`SegmentPolicy::by_duration`; the spool rotates when the next media event would
push the segment past the limit and at least one media frame is already in it,
`RecorderSnapshot.written_duration_nanos` and `RecoveredSegment.duration_nanos`
report exact recorded spans, and crash reconciliation restores the active
segment's duration by scanning media timing from a bounded prefix of each
record before validating and repairing torn tails. `RecorderConfig.tap`
(`IsoTapMetadata`) records optional video and audio source `InputId`s, a
`TimecodeSource` of `SharedMaster` or `SourceOriginal`, and an optional
`Timecode`, all stored in the CRC-protected `manifest.fmr` start record and
restored across restart; unknown timecode sources, invalid timecodes, zero
input ids, and truncated or checksum-corrupt records are refused
transactionally. The crate now also provides `IsoTapSession`, a bounded
orchestration layer that starts up to eight selected raw input feeds as
independent crash-safe recorders under one session root: recorder and
video-input identity are unique and bounded per tap, an optional audio input
maps embedded audio, each tap resolves a shared-master or source-original start
timecode, the whole roster is validated before any file exists, and a later
start or manifest failure rolls already-started taps back. The session writes a
deterministic bounded `iso-manifest.txt` carrying the ordered roster, resolved
timecodes, and per-tap segment and queue bounds, and parses it back exactly
while rejecting malformed, duplicated, or out-of-bounds documents wholesale.
This is still a reference orchestration and manifest layer, not daemon
integration: no daemon path selects per-input taps, no editing-format
(EDL/XML-class) interchange is written, no graph tap or per-camera storage root
exists, no wire, Studio, or Web projection exposes tap or timecode state, and no
hardware or cross-platform acceptance exists, so item 5 remains incomplete.

Current implementation boundary for item 7: native `freemixd` can expose a
bounded loopback operator status listener (`--status-listen`, guarded by
`FREEMIXD_STATUS_TOKEN`) that answers `/healthz` and `/readyz` from an atomic
snapshot the control loop republishes every pass, and serves a capped
`/v1/support-bundle`. The bundle carries retained observability events, metric
series, health checks, and capability state, and now also a bounded operator
state section: every configured stream target's id, sanitized name, and desired
flag plus its native realized label; both recorder slots' desired flags and, in
native mode, which slots are configured and which are running; and the disk
alert state with free bytes. The control loop publishes that text with one
bounded copy at most once per second, never blocking the render thread, and
oversized text truncates with an explicit marker; no stream URL, key, path,
host, or device identity reaches the bundle. The listener owns no engine state,
its accept thread never touches the bundle cell, and probes read only the
atomic snapshot. This is health and diagnostics, not service mode: there is no
systemd/launchd unit, no TLS or non-loopback binding, no media- or
service-level readiness claim beyond process readiness, no bundle upload or
retention policy, and no authentication beyond the bearer token. Item 7
remains incomplete.

Current implementation boundary for item 9: native `freemixd` performs a
fail-closed disk preflight for configured program recording and reports
free-space alert edges while recording. Any configured recorder output
(`--record-program` or `--record-program-2`) whose parent filesystem cannot be
queried or has fewer than 512 MiB available refuses daemon startup with a
sanitized error that carries the byte count and I/O kind but never the path.
While recording is configured, the render loop samples each output parent's
free space at most once every five seconds, with the first sample evaluating
immediately so a low value present at startup fires at the first frame. The samples drive one hysteretic `fm-observability` alert
that fires at or below 1 GiB and clears at 2 GiB, printing exactly one
`FREEMIXD_DISK` record per edge with its state and free bytes. The final
`FREEMIXD_TELEMETRY` v4 record gains `disk_alert=unknown|normal|low` and
`disk_free_bytes`. A failed sample stays silent and leaves the last value and
state; a daemon that has never sampled reports `unknown` with zero bytes. The
same preflight and alerting now cover every authored local HLS output: startup
fails closed when an HLS directory's nearest existing ancestor cannot be
queried or falls below the same floor, naming the stream target id and byte
count but never the path, and the five-second sampler takes the minimum free
space across every configured recording parent and every local output
directory, so one `FREEMIXD_DISK` edge covers whichever configured local
output crosses the threshold. No path is created by the preflight; the HLS sink still creates its
own leaf later. Configured network stream endpoints are additionally probed
once at native startup: each primary and configured backup endpoint gets one
bounded TCP reachability attempt (two seconds total across all probes, one
worker each, unanswered probes reported as unresolved rather than blocking),
and every probed endpoint prints exactly one sanitized `FREEMIXD_PREFLIGHT`
record carrying the target id, the primary/backup label, and
`reachable|refused|timed-out|unresolved`. That preflight never refuses startup
and never changes destination state or reconnection, because an ingest may
legitimately come up later. This covers program recording, local HLS outputs,
and one-shot network reachability only: there is no continuous network
monitoring or bandwidth probe, no GPU capability preflight beyond adapter
creation, low space never stops or fails a running recording or output on its
own, and no per-destination disk telemetry exists. `OR-001`'s space status is
now observable on the recording path, but the row still awaits its other
acceptance items, and item 9 remains incomplete.

Current implementation boundary for item 11: `docs/spec/capability-matrix.md`
is the first published capability matrix. It is generated deterministically
from `parity.toml` by `cargo run -p xtask -- matrix write` and verified by
`cargo run -p xtask -- matrix check`, which fails when the committed file is
stale. The matrix renders one section per capability profile in registry order,
with derived status and priority counts, and one row per feature carrying its
identifier, priority, phase, ledger status, owner, and every acceptance-test
identifier annotated with that test's ledger state. It uses only recorded
ledger data and states explicitly that no capability profile is certified and
that it reports ledger status plus local acceptance-test evidence only. It is
not a hardware compatibility, driver, or vendor certification list: because
ledger feature statuses are release states, most rows read planned or blocked
even where development slices exist, and the acceptance column is where local
evidence appears. Publishing measured capability support for certified devices
therefore remains future work, and item 11 remains incomplete.

Current implementation boundary for recording audio flexibility (`AU-010`):
`fm-record` provides a validated per-recorder track map that selects Master
and aux bus channels into ordered mono, stereo, or multichannel tracks under
explicit track and channel limits, and routes interleaved bus audio into
per-track or combined buffers. A WAV writer emits exact RIFF headers for
16/24/32-bit PCM and 32-bit float with extensible layouts, bounded whole-frame
buffering, optional BWF `bext` metadata with exact timecode-to-sample
references, size patching on checkpoint and finalize, and automatic RF64 past
4 GiB through a reserved chunk; recovery rebuilds an unfinalized file's sizes
from its data length. A track recorder writes interleaved or split per-track
files after validating every file up front. Remaining gaps: nothing connects
this to `fm-audio` buses, the daemon recorder slots, persistence, or UI; WAV
output is separate from the crash-safe packet spool and has no time or size
rollover; and metadata stops at version-1 `bext` without iXML, loudness, or
UMID. `AU-010` remains planned.

Current implementation boundary for statistics and alerts (`RC-015`):
`fm-observability` provides a deterministic, clock-free statistics layer: a
bounded registry of counters, gauges, and fixed-bucket histograms keyed by
metric ID and validated labels; windowed FPS, drop rate, maximum queue depth,
and documented interpolated percentiles that report saturation instead of
undercounting; and an alert engine with threshold, rate-of-change, and
staleness rules, hysteresis, hold durations, pending, firing, and resolved
states, deduplicated notifications in a bounded severity-aware queue, and
expiring acknowledgements and silences. A redacted, sorted text snapshot can
be appended to the support bundle under its byte cap. Remaining gaps: the
daemon, engine, outputs, and Studio do not record into it or drain
notifications; there is no GPU, CPU, disk, network, sync, or encoder-capacity
sampling; no protocol or UI view; and no persisted rules or silences.
`RC-015` remains planned.


Current implementation boundary for fault-tolerant recording (`OR-003`):
`fm-record` scans fragmented MP4 files with bounded memory, classifies them as
finalized, complete, torn, or unrecoverable, reports exact per-track coverage
and loss, and repairs a torn tail by truncating to the last complete fragment
without touching unrecoverable files. A `RecordingSession` names segments in an
atomically rewritten, CRC-checked manifest mirrored in every root before
creating them, disables full, read-only, or missing roots at once, retries
other errors a bounded number of times, records failures in a bounded ring,
and fails loudly when no root remains. `ResilientFmp4Writer` writes whole
fragments with per-fragment sync and replays a failed fragment on the next
root so nothing is lost while any root works. Recovery trusts the newest valid
manifest, adopts orphan segments, and is idempotent. Free-space estimates, a
hysteretic monitor, and a preflight report remaining time. FFmpeg-gated tests
decode across a failover, a mid-fragment cut, and a killed recorder. Native
`freemixd` now records through this path: FFmpeg output passes through the
fragment splitter into the resilient writer, `--record-fallback-dir` provides
failover, a free-space preflight runs before each segment, and startup recovers
sessions a crash left open before any recorder starts; FFmpeg-gated daemon tests
cover a primary made read-only mid-show and a killed daemon with a torn tail.
The `--record-wav` sidecar now streams from FFmpeg into the daemon and is
written by a resilient WAV writer in its own session beside the video: it
fails over to the fallback root with no lost audio, patches RIFF sizes on
close, and startup repairs a sidecar a crash left open.
A live session and a running recovery hold an advisory lock file per root
(`.<manifest>.lock`), so a second session or recovery of the same name, in
this or another process, is refused with a typed `Locked` error, and daemon
startup recovery reports a session another daemon still records as
`status=locked` without touching it. A root disabled for lack of space is
probed while recording and re-enabled only once it reports the minimum plus a
margin, and the margin more than when it was disabled, and takes a probe
write; the writers then rotate back to it, `FREEMIXD_RECORDER_RESTORED`
reports the return, and read-only, missing, or forbidden roots are never
re-enabled. Native `freemixd` samples the active directory every five seconds
and emits a hysteretic `FREEMIXD_RECORDER_REMAINING` record when estimated
remaining time crosses 30, 10, or 2 minutes and when it clears.
Remaining gaps: the fallback is refused with fullscreen output; the
remaining-time thresholds are fixed and reach no protocol or UI view; no
daemon test runs two `freemixd` processes against one directory (the lock is
proven across processes in `fm-record`); repaired files are not
re-finalized; corrupt mid-file fragments end recovery; and only fragmented MP4
is handled. `OR-003` remains planned.


Exit: a remote-controlled headless production can stream and record
independently with tested recovery.

## 7. Phase 5 — graphics, data, automation, and ecosystem controls (12–16 weeks)

1. Implement title scene model, text shaping, images/shapes, animation/tickers.
2. Build title designer and live field editor.
3. Add isolated browser input with audio, interaction, custom CSS, and network
   policy.
4. Add data sources and typed field mapping.
5. Implement PlayList scheduling and per-input programmed GO.
6. Implement the complete trigger catalog, macro transactions, timers,
   conditions, cancellation.
7. Add Wasm automation/data plugins with capabilities and limits.
8. Complete controller learn/templates, dynamic values, activators, tally.
9. Add PTZ protocols, presets as virtual inputs, mouse/joystick control.
10. Add telestrator and web monitor.
11. Add project append, bundle, relink, import/export, templates, undo/lock.
12. Build the color grading suite (`SW-019`): color wheels, curves, white
    balance, LUT import, presets, and a live before/after split preview with
    scopes, per input, scene layer, and program.
13. Build the keyframe animation editor (`GX-011`): one visual timeline for
    title elements and scene layers with easing presets, a curve editor,
    proportional retiming, scrubbed preview, and In/Out presets.
14. Make every operator control discoverable (`RC-018`): simple defaults with
    progressive disclosure and localized hover help naming the action, its
    shortcut, and why it is disabled.

Current implementation boundary for item 14 (`RC-018`): every interactive
Studio console control carries hover help naming its action and, where one
exists, its single-key shortcut, and disabled controls add the localized
`Unavailable: …` reason. The mixer's per-strip gain/balance/delay fields and
the manual transition's details sit behind disclosure keys (also under View ›
Advanced controls) that persist in `studio.conf`. `fm-ui-egui`'s hover audit
renders the console, the animation timeline, and the colour panel headlessly
in English and Spanish and fails on missing help, a disabled control without
its reason or action, help longer than four lines, untranslated Spanish
lines, or an unnamed shortcut; `cargo test -p fm-ui-egui --test hover_audit`
runs it. Remaining gaps: the audit covers only the egui Studio console, not
the web controller, CLI, or daemon messages; it relies on debug-build widget
info; and only English and Spanish ship, so `RC-018` remains planned.

The current persistence slice audits stored media asset references and
provides a secret-free settings export with an import planner (`RC-013`):
outputs, buses, stream targets, and video/audio settings export as one exact
contract whose secrets serialize only as placeholders, and imports preview
per-key changes, merge by replace, prefer-incoming, prefer-current, or
per-section choice, report secret re-entry, unknown references, and ID
collisions, and apply atomically only when resolved against an unchanged base.
Applied snapshots are not yet turned into project mutations or engine
commands, nothing in the daemon, protocol, CLI, or UI calls the planner,
secrets are excluded rather than separately encrypted. For `SW-017`,
`fm-engine` classifies commands into undoable project edits (input rename and
reorder, stinger slot configuration, overlay settings) and live operations
(preview, cut, transitions, overlays on/off, FTB, audio strips, streams,
records) that are never recorded, undone, or locked. Each edit stores its exact
inverse command in a shared linear history of 100 entries; undo and redo run as
ordinary idempotent edits, a new edit clears redo, an `expected_edit` mismatch
is a `history_conflict`, and a failed inverse leaves history untouched.
Durable project and per-input padlocks reject edits (including undo/redo) with
stable reason codes while live switching continues; history resets on restore.
Protocol 2.22 wires undo/redo and the padlock through control, the daemon,
the CLI, and Studio, and project schema 26 persists the project lock and the
locked input set in `project.json` (`project.edit_locks`), so locks survive
restart, journal replay, and checkpoint restore; removing an input offline
drops its lock. Remaining undo/lock gaps: there is no per-author history, and
outputs, stream targets, and titles are not engine edits yet, so `SW-017`
remains planned. For `RC-012`, `fm-persistence` adds rotated atomic autosave
generations whose recovery returns the newest same-project generation that
loads cleanly and reports every skipped one; a canonical-path recent-projects
document with bounded size and missing-entry reporting; deduplicating media
import into `assets/`; a plan-then-apply relink that searches the bundle and
caller-ordered directories by file name, reports ambiguity instead of guessing,
and refuses stale plans; staged portable bundle export that copies only
referenced assets, renames them to be portable, and never overwrites an
existing destination; template instantiation with a fresh identity and no
receipts; and append with id remapping and name-conflict reports. Remaining
gaps: append skips outputs, buses, stream targets, and stingers; relink matches
by name only because assets carry no size or hash; external media references
are not in the current schema; there is no cross-process lock on recent
projects, crash-temp cleanup, cancellation, or progress. `freemix-cli` exposes
every operation as a local command (`project-export-bundle`,
`project-from-template`, `project-append`, `project-import-media`,
`project-relink` with preview by default, `project-autosave`,
`project-recover` with journal-checkpoint guards, and `recent-projects*` on an
explicit file), but nothing autosaves on a timer and the daemon, Studio, and
protocol do not call these operations. `RC-012` remains planned.

Current implementation boundary for item 1: `fm-titles` provides dependent,
bounded title scenes and a deterministic CPU renderer. Templates carry typed
text, number, color, and image fields with optimistic scene updates; elements
support text, rectangles, images, clocks, and tickers with style, alignment,
z-order, and visibility, animated by one shared `fm-animation` clip per
template (see the `GX-011` boundary below). Structural validation
checks field types, element bounds, and referenced assets. Text is real glyph
rasterization from caller-supplied font bytes through `ab_glyph` with kerned
advances, greedy word wrapping, horizontal and vertical alignment,
anti-aliased premultiplied RGBA8 coverage, and frame/box clipping. Image
elements render caller-supplied decoded RGBA8 frames with per-element
stretch, contain, cover, and native-size fit modes placed by the element
alignment using exact integer sizing, and with nearest-neighbor or
pixel-center bilinear filtering of premultiplied channels using 8-bit
fixed-point weights and clamp-to-edge addressing. Drawing is clipped to the
canvas, element box, and placed image, then composited source-over with
element opacity. Unknown or name-only assets keep the placeholder path and are
reported. All work is capped by output size, font size, characters, lines,
glyphs, and canvas pixels, so a huge placed image costs only the canvas
intersection, and per-element degradation is reported instead of failing the
frame. Remaining gaps: no grapheme-level shaping or bidi, no file
decode/orientation/color-profile handling, no mipmapped or higher-order
minification filtering, no GPU rasterization, no title designer or live field
editor, no project-schema instantiation, and no animation/ticker integration
with real data sources. Item 1 therefore remains incomplete and `GX-001`
remains planned.

Current implementation boundary for item 1 project hosting: project schema
27 adds title inputs that reference a bundled `.fmtitle` template and carry a
persisted field revision plus typed text, number, colour, and image field
values within bounded counts, text length, and content bytes. `fm-persistence`
reads and writes templates strictly (every element kind, 1 MiB cap; schema 29
adds a `clocks` table that defines exactly the production clocks the
template's elements show; schema 30 replaces per-element animation tracks
with one template `animation` clip), imports fonts with dedupe and parse validation, checks
project fields against their templates, audits missing template, font, and
image assets, and copies title assets on export and append, refusing rather
than renaming keys. `freemix-cli title-input-add` imports a template with
`--font` files. The engine keeps per-title field state with bounded
compare-and-set saves, takes that apply fields and an overlay take in one
revision, and off-air on the single showing channel, with air checks over
desired and realized state including playing stinger media. Protocol 2.24
carries `title_fields`, `title_take`, `title_off_air`, `title_changed`, and a
`title_state` record after each snapshot; Graphics may save, Operators may
take and clear, and a take with fields needs both. freemixd restores and
persists title values, and its simulated monitors render title inputs and
composite active overlays on Program. Native freemixd realizes each title
input as a live source frame rendered by the reference renderer on a title
worker off the session loop, re-rendered after field changes (and at a
bounded refresh of at most 25 Hz for clock, ticker, and animated templates),
so titles reach Program, Scenes, and project-output overlays. Production
clock elements render in both simulated monitors and native titles from a
per-render clock registry in the daemon's clock domain (see the `SW-013`
boundary). Image fields
load their PNG/JPEG assets on demand, including URIs first set at runtime, in
`native-media` builds; a build without `fm-codec-image` keeps the image
placeholder. The title editor core and its protocol bridge live in
`fm-ui-model`, shared by the web editor and Studio's TITLES tab, which edits
typed fields with conflict rebase/discard, previews, saves, takes on a chosen
overlay channel, updates live, and takes titles off air. Remaining gaps: no
grapheme shaping or bidi, no template designer persistence, titles cannot be
stinger media, runtime-added title inputs are not rendered in simulated
mode, scene time (animations, tickers, scene clocks) still starts at frame
zero rather than at the take, and template field names are not localized, so
`GX-001` remains planned.

Current implementation boundary for item 1 scoreboards and tickers
(`GX-003`): `fm-titles` provides a deterministic ticker crawl (horizontal
crawl and vertical roll) driven only by the engine frame index and an exact
rational pixels-per-second speed against the frame rate, with separator wrap,
bounded item lists, and live updates spliced in at the next unseen item so
visible text never jumps; crawls render above scene elements through the CPU
reference renderer with exact pixel tests. A typed scoreboard covers team
names, abbreviations, scores, a featured player, and the period with atomic
revision-checked action batches, saturating increment/decrement shortcuts,
and a frame-exact count-up or count-down game clock whose expiry is reported
once; changed values publish onto template fields in one scene revision, and
a strict key/value parser maps external data onto actions. Remaining gaps:
crawls are runtime overlays outside template z-order and the project schema,
with fixed directions and no per-item styling, logos, or easing; the
scoreboard has two teams, no rosters, sport presets, shot clocks, tenths, or
persistence; and there are no feed transports, shortcut or control-surface
wiring, or engine output. `GX-003` remains planned.

Current implementation boundary for item 2: `fm-titles` provides a designer
document model with a bounded tree of elements and nested groups whose offsets
compose exactly, whose opacity multiplies through ancestors and rounds once,
and whose rectangular or rounded-rect clip masks apply per pixel in the CPU
renderer. The design's In and Out animation is one `fm-animation` clip
(fixed-point easing library with polynomial, sine, back, bounce, and a
bisection-solved cubic-bezier with a documented error bound) under a take,
hold, and off-air playback state machine driven only by engine frame indices;
group tracks fold position and opacity onto descendants and element tracks
travel with the flattened template. A designer edit API maps keyframe add,
remove, retime, phase stretch, and every other clip edit onto atomic
`fm-animation` edits and validates the whole document, with bounded snapshot
undo and redo. Remaining gaps: group transforms are translation and opacity
only, mask edges are not anti-aliased, rendering is CPU-only, and there is no
designer UI, live field editor, design persistence, protocol, or engine
playback. `GX-002` remains planned.

Current implementation boundary for the keyframe animation editor (`GX-011`):
`fm-animation` is the shared, pure model: Q16 easing presets with stable ids
and a cubic-bezier curve, clip timebases with exact cross-rate positions,
bounded In/Out clips of property tracks (position, scale, rotation, opacity,
crop, color), atomic set/remove/move/retime/stretch/easing/copy-paste edits,
snapping, and Fade/Slide/Pop/Wipe/Drop presets. Titles use it end to end:
every `fm-titles` template carries one clip keyed by element with an edit
revision, and `ReferenceRenderer::render_sampled` draws any In/Hold/Out sample
with per-element position, scale about the box centre (text laid out again in
the scaled box at a scaled, capped font size), crop, colour override,
opacity, and rotation (offscreen layer resampled with Q30 fixed-point sine
and cosine and 8-bit bilinear weights; quarter turns are exact pixel
permutations; boxes over 4 Mpx draw unrotated and are reported). Project
schema 30 stores the clip strictly in `.fmtitle` (`timebase`, `revision`,
phase lengths, canonical track order, preset or `cubic_bezier` easing, every
clip limit). `freemix-cli title-animation-preset`, `title-animation-stretch`,
and `title-render-frame` apply presets, retime a phase proportionally, and
render exact frames. Takes and off-airs play the clip on air (stage 3):
`fm-titles::TitleAir` maps engine frames to In/Hold/Out samples with exact
rational resampling between the clip timebase and the engine rate; the
engine realizes `take_title` as an overlay cut held at full opacity while In
plays from the take frame, and `title_off_air` as an overlay held for exactly
the Out length before it is removed. Off-air during In starts Out at frame 0
on the off-air frame; a re-take on the same channel during Out restarts In at
frame 0 (the pose jumps, as in title playback); zero-length phases keep the
channel transition; overlay commands and Program show the Hold pose. Poses are
a pure function of the frame since the phase start, so cuts and dropped frames
land on the exact pose. Snapshots report work in flight while a title
animates, the simulated daemon realizes a whole phase inside its command, and
native freemixd renders every projected pose and holds a frame until its pose
is ingested (bounded wait, late poses counted). Protocol 2.27 reports the
phase in `title_state`. The daemon refreshes only clock and ticker titles on
time otherwise. Protocol 2.29 (stage 4a) edits animations remotely:
`title_animation_set` (whole clip), `title_animation_preset`, and
`title_animation_stretch` are revision-checked `EditProject` engine edits
(a stale `base_revision` is a `conflict`) on the template every sharing input
uses, padlocked across those inputs and undoable through the shared history;
each applied edit advances the animation revision. `title_animation_changed`
and one `title_animation_state` per snapshot title replicate the clip in a
strict canonical binary encoding (36 KiB, every `fm-animation` limit).
freemixd stages the edited template per journal revision and commits it at
checkpoints and recovery, so an edit survives a kill and restart. A
rate-limited `title_preview_request` renders the saved animation or an
unsaved draft clip at an In/Hold/Out/static sample through `render_sampled`
(at most 640x360, QOI parts over a latest-wins lossy slot, raw sessions
only). `fm-client` replicates the clip and assembles previews, and
`fm-ui-model` adapts the headless `AnimDocument` to the authority (reset,
saved, or conflict) and turns its exact preview requests into wire
requests. Studio's Animation window (stage 4b; Titles row `Animate…`, View
menu, Ctrl+Shift+A) binds one draft per title to the replicated clip: edits
auto-save on release as `title_animation_set` on their base revision, a
change under unsaved edits offers Reload theirs or Overwrite, padlocks and
missing `EditProject` disable editing with reasons, and the canvas shows the
debounced, latest-wins exact preview of the draft at the playhead (badged
exact or fast). Protocol 2.30 replicates each template's element layout
(`title_layout_state` after the animation states: canvas size and, per
element, id, name, kind, bounds, opacity, fill, and visibility, in a strict
36 KiB binary field of at most 192 elements with 96-byte names, derived by
`fm-control` from the `fm-titles` template). Studio hands each layout to the
Animation window as its design: every element is listed by its real name
with a kind icon, unanimated elements take local presets, and proxy quads
and onion skins are drawn over the exact preview. In Simple mode, resting on
a preset previews it before it is applied (its outlines play the motion at
once and a debounced exact preview of the hovered draft follows); a click
applies it as one undo step. The window floats resizable or docks along the
bottom of the console, and its size, dock height, and placement persist in
`studio.conf` (`layout.animation`, `layout.animation_size`,
`layout.animation_dock_height`). Stage 5 animates scene layers with the
same model and editor: project schema 32 gives every scene layer a stable
`LayerId` and every scene one `SceneAnimation` clip keyed by it (position,
scale, rotation, crop, and opacity; colour is refused), validated against
the scene's layers and stored in `project.json`. `Scene::posed` samples it
with integer arithmetic (scale about the footprint centre, crop with the
source cropped in proportion, opacity multiplied, rotation at any angle:
the nearest quarter turn is the layer transform and the residual tilt within
±45° is drawn about the layer centre, a cropped box turning about its
uncropped footprint's centre; the stored layer rotation stays whole quarter
turns). `fm-compositor` draws a tilted layer (`SourceLayer::
with_rotation_millidegrees`, positive clockwise) exactly on the CPU with Q30
fixed-point trigonometry shared with `fm-titles` (`fm_animation::
sin_cos_q30`) and bilinear premultiplied sampling, and in the native GPU
composition pass by the same inverse mapping and Q8 bilinear weights
(GPU-vs-CPU parity within 2/255 per channel on lavapipe/llvmpipe).
The engine plays a scene's In phase when its Scene input reaches Program or
Preview, holds, and plays Out while a timed or manual transition takes it
off Program (a cut plays no Out; nested, multiview, and overlay scenes
hold); phases are runtime state that never blocks a snapshot. The monitor
tap, the simulated scene render, and native freemixd (recompiling the scene
composition per frame while a scene is mid-phase) pose every frame.
Protocol 2.31 adds the snapshot `scenes` list, revision-checked
`scene_animation_set`, `scene_animation_preset`, and
`scene_animation_stretch` (undoable `EditProject` edits, 36 KiB clip bound),
`scene_animation_changed`, and the follow-on `scene_animation_state` and
`scene_layout_state` records (canvas and per layer id, name, footprint,
quarter turns, opacity, enabled). The Animation window's target picker
lists titles and scenes, lists each scene's layers by name, applies
presets, and draws posed layer outlines. Protocol 2.32 adds the exact scene
preview: `scene_preview_request` (a title preview's fields with an optional
draft scene clip; `EditProject`, the shared per-session preview rate, raw
TCP only) is composed off the session loop by the title preview worker with
the CPU reference compositor at the requested size from the project's
simulated sources and answered as QOI `scene_preview_frame` parts through
the same latest-wins lossy slot. Nested scenes (a layer naming a scene
directly or through a Scene input) are composed on their own canvas in their
Hold pose, within 16 levels and 64 scenes. A simulated daemon draws its
simulated sources at the engine frame cursor; a native daemon draws the
newest CPU frame its media pipeline already holds per source (the last frame
of each decoded window, the last ingested live title or camera frame, or the
retained still), snapshotted as shared frames on the session loop without
GPU work or waiting on the output clock, converted from its labeled
colorimetry to sRGB Rec.709 full range through the `fm-color` CPU reference.
Native previews therefore match the GPU composition within the difference
between the 8-bit CPU and 16-bit float GPU pipelines (not bit for bit) and
may show decoded media up to the source ring (eight frames) ahead of air.
Both daemons grade previews like their monitors (`SW-019`). A scene with a layer whose source has no picture
yet, or nesting past the bounds, is refused with `render_failed`. `fm-client` assembles scene previews and Studio's
Animation window shows them under the layer outlines like a title's.
`freemix-cli scene-animation-preset`, `scene-animation-stretch`, and
`scene-render-frame --phase in|hold|out` author and render scene
animations locally, and local scene and title animation edits refuse a clip
without a wire encoding (beyond the 36 KiB bound `fm-control` enforces)
instead of saving it. `title-render-frame` renders through
`fm-title-render`, the renderer freemixd draws titles with: the bundle's
fonts, PNG and JPEG images decoded by `fm-codec-image` (an image that does
not load keeps the placeholder and is reported), and production clocks at a
deterministic time (`--at YYYY-MM-DDTHH:MM:SSZ` for wall-clock UTC, default
the Unix epoch; since-take clocks read the clip position, the title taken at
In frame 0). Every title the daemon holds carries its loaded template:
freemixd refuses to start when any title template in the bundle fails to
load or validate, title inputs are added only through the bundle (runtime
`AddInput` creates solid and bars sources), and template URIs are
bundle-local `asset://` paths, so the layout-less fallback (only already
animated elements, presets applied on the server) is reached only by an
`fm-control` host without templates, never by freemixd.
Reusable user presets (project schema 34, Protocol 2.34): a preset is one
phase of one target's animation captured relative to its resting pose
(`fm_animation::PhaseMotion`: positions, scale, rotation, and opacity as
offsets, crop measured against the target's box, colour kept), so it
applies to any title element or scene layer. Applying one replaces the
target's tracks of the preset's properties in that phase, retimed
proportionally to the requested length, with crop keys scaled to the new
target's box (scene layers skip colour). The project keeps an ordered
library (`animation_presets` in `project.json`: at most 64, unique ids and
names of at most 128 bytes, strict like the title clip), edited by undoable
`EngineAnimationPresetEdit` add/rename/remove project edits and applied by
the title and scene animation `UserPreset` edits. Protocol 2.34 carries
`animation_preset_add|rename|remove`, `title_animation_user_preset`,
`scene_animation_user_preset`, `event=animation_preset_changed`, and the
`animation_preset_state` records that follow a snapshot last; freemixd
journals, saves, and restores the library, and `fm-client`/`fm-ui-model`
replicate it. In Studio's Animation window the Simple picker lists the
presets saved from the shown phase under "My presets" with the same hover
preview as the built-ins (the preset's motion played on the outlines, then
the exact preview), and the Advanced inspector's "My presets" row saves the
selected target's phase as a preset ("Save as preset…"), renames, deletes,
and exports or imports a `.fmpreset` file by path. `.fmpreset` files are
strict JSON versioned by the project schema (`fm-preset-file`, a pure
foundation crate holding the shared JSON reader and the motion and grade
encodings, so the project store and Studio read and write them identically
while Studio links no server crates) (`kind: "animation_preset"`;
any other schema, kind, or key is refused); an imported preset whose name is
taken becomes `Name (2)`. `freemix-cli preset-save`, `preset-export`, and
`preset-import` do the same offline, and `title-animation-preset` and
`scene-animation-preset` accept `--preset user:<id>`. The timeline ruler
labels steps that divide the frame rate (at 30 fps: 0:00, 0:10, 0:20,
1:00), never a frame count that drifts across seconds. The timeline
header's `Stagger…` menu cascades whole element animations in the shown
phase (`AnimDocument::stagger`: the k-th element, top or bottom first,
starts k steps after the earliest start, each keeping its own timing and
easing; two or more elements with selected keys scope it, and the phase
grows to hold the last one) as one undo step that auto-saves through the
existing `title_animation_set`/`scene_animation_set` edits, so it needs no
protocol change. `Spread to fill phase` spaces the starts evenly instead
(`spread_plan`) so the last element ends on the phase end, and resting the
pointer on `Apply stagger` ghosts the result over the rows
(`AnimDocument::plan_stagger`). A `Snapping` toggle turns drag snapping off
(Shift then snaps instead of freeing, and the drag help says so); it is a
console preference saved in `studio.conf` (`animation.snapping`, debounced
like the layout). The transport's previous/next key keys (Ctrl+Left/Right)
jump the playhead between the keys of the rows with selected keys, else the
active Advanced row, else every row of the phase. Undo and redo select only
restored keys of the shown phase, so an edit undone from the other phase
leaves no hidden keys for Delete to reach. Remaining
limitations: Studio imports and exports by typed path (there is no file
dialog), and a preset applies to one target at a time.

Current implementation boundary for the color grading suite (`SW-019`):
`fm-grade` is the pure, all-integer grade model (white balance, exposure,
lift/gamma/gain/offset wheels, contrast, saturation, RGB curves,
hue/saturation curves, and a `.cube` LUT stage with strength) with a CPU
reference evaluator, exact `CompiledGrade` tables, a bounded `.cube` parser
(16 MiB input, 65^3 3D edge, 65 536-entry 1D), and stable built-in presets
(neutral, warm, cool, punchy, faded, bw). `fm-compositor` applies
`SourceLayer::with_grade` after a layer's effects and before its transform,
on the CPU and in the native GPU pass through a baked 65^3 LUT atlas. Project
schema 31 makes grades project state: `project.json` strictly stores imported
LUTs (`luts`: id, name, kind, size; at most 64), a named grade library
(`grades`: at most 256, names at most 128 bytes), per-input grades
(`input_grades`) and the program output grade (`program_grade`), each either
a library reference or an inline grade; schema 30 is refused. Validation
checks every grade bound, unique ids and names, and that a grade's LUT URI is
exactly `asset://luts/<id>.cube` of an imported LUT; removing an input drops
its grade, and in-use library grades and LUTs cannot be removed.
`fm-persistence` imports a `.cube` file by reading it once under the size
cap, parsing it, and writing exactly those bytes to `assets/luts/<id>.cube`;
loading re-parses under the same cap and refuses a file whose kind or size
no longer matches. Portable bundles and templates copy every imported LUT.
`fm-engine` compiles a project's grades for scene builders (library grades
compiled once and shared, identity grades omitted, missing LUTs bypassed
and reported). `freemix-cli` adds `grade-set` (preset plus exposure, white
balance, contrast, saturation, and LUT overrides, inline or saved to the
library with `--save-as`, or `--grade` by reference), `grade-clear`,
`grade-import-lut`, and `grade-list`, honouring the project and input
padlocks; `render` grades each input before the transition or scene-layer
transform and grades the composed program picture. Grade edits are engine
commands (`fm-engine` `EngineGradeEdit`): set/clear an input or the program
grade, add/update/remove a library grade, and import/remove a LUT whose
caller-supplied `.cube` text the engine validates with `fm_grade::parse_cube`
under the 16 MiB cap. They are project edits on the shared undo/redo history
(an input padlock covers that input's grade, the project padlock all of
them; removing a LUT is not undoable, undoing an import removes it) and are
`EditProject` (Graphics role) in `fm-control`. Protocol 2.28 carries the
nine `grade_*` commands, `event=grade_changed`, and `grade_state` records
announced by the snapshot's `grade_states` count after the playlist state,
which `fm-client`/`fm-ui-model` replicate into a minimal `GradingStatus`
view. With no chunked upload, a LUT import over the protocol is capped at
32 KiB of `.cube` text (base64url in one field); larger LUTs are imported
offline with `grade-import-lut`. `freemixd` journals grade edits, writes an
imported LUT's file before its journal batch, saves grades through the
normal project save, restores them (and the bundled LUTs) on restart, and
renders them: the simulated monitor path grades each simulated input and
the composed Program, and the native path grades each leaf input before
mixes, scene layers, and overlays and each Program output after overlays and
before Fade-to-Black through `NativeGradeRenderer` LUTs uploaded once per
revision. Project append keeps appended inputs' grades, appending the
library grades they use and copying the LUTs they reference under fresh
ids (renamed on a clash). Project schema 33 adds per-scene-layer grades
(`layer_grades`: `scene`, `layer`, and a library or inline `grade`; at most
256, between `input_grades` and `program_grade`; schema 32 is
refused). Validation requires the scene and its layer to exist; removing a
layer or a scene drops its grades, duplicating a scene input copies them to
the new scene, and appending a project carries them to the fresh scene ids.
`fm-compositor` applies `SourceLayer::with_layer_grade` after the source's
input grade and before the layer transform (CPU: one exact table pass per
grade; native: one LUT pass per grade), so a layer grade colours only that
layer. `EngineGradeEdit::SetLayer` addresses a layer by its scene id
(Protocol 2.35), whether or not a Scene input shows the scene (the engine
holds every project scene): it is undoable, and the padlock of every Scene
input showing the scene covers it (and a `grade_library_update` of a
library grade such a layer uses); an unshown scene's layers answer to the
project padlock only. Protocol 2.35 carries
`grade_layer_set`/`grade_layer_clear` (`scene`, `layer`), and
`change=layer` and `entry=layer` records keyed by `scene` for every
project scene (layer records follow the input records, ordered by scene
then layer; at most 256), replicated by `fm-client`/`fm-ui-model`. The
snapshot's `layout_scenes` lists every project scene and one
`scene_layout_state` per scene (keyed by `scene`, with the scene's name and
each layer outline's source input or nested scene) follows the scene
animation states, so clients know every scene's layers whether or not a
Scene input shows it. Studio's Colour window lists every layer of every
project scene as a target, labelled `Scene › Layer` (with "no Scene input"
for a scene no input shows), and `freemix-cli grade-set|grade-clear <show>
layer <scene> <layer>` edits it offline and `render` shows it. The
simulated monitor path grades scene layers (input grade, then layer grade),
title pictures and keyed title overlays (their input grade), and the
program, consistently with the native path; simulated media inputs have no
picture to grade. The native path also grades Stinger media by its media
input's grade. A Scene input's own input grade colours the scene picture
it shows, after the layer grades and the composition and before the
picture is used as Program, Preview, an overlay, or a layer of another
scene (a layer drawing the Scene input carries that grade under its own
layer grade; a layer drawing the scene directly does not): the simulated
monitors (`scene_render` `Composer`), the native GPU composition (one LUT
pass over the composed scene texture per graded Scene input), exact scene
previews (the requested Scene input's grade over the whole preview), and
`freemix-cli render` apply it the same way. Exact scene previews
(`scene_preview_request`) on both simulated and native daemons grade with
the compiled grades of the engine revision the request is resolved at:
each leaf layer's input grade then its layer grade, a layer drawing a
nested scene its own layer grade over the nested scene's graded layers
(and the Scene input's grade when drawn through one). The Colour window
previews a layer target on its live picture: the layer's clean picture is
its source as a monitor shows it (the source input with its input grade,
or for a layer drawing a nested scene any ungraded Scene input showing that
scene), so when a fresh Preview monitor frame, or a Program frame while the
program grade is identity, shows that source, the window grades it
locally with the draft layer grade; otherwise it shows the last clean
frame it held for the target, and before any a test still with a caption
saying so. Remaining gaps: the Colour window does not request an exact
scene preview of a layer's source, so a layer whose source is on neither
monitor previews on its last held frame or a still, and the preview is the
source picture rather than the layer posed in its scene; the window's
before/after split and scopes are computed locally from the monitor
proxy frame, not from the full-resolution graded output; and native
graded scene previews match the GPU only within the 8-bit CPU and 16-bit
float GPU pipeline difference, so `SW-019` remains planned. Grades travel
between
projects as `.fmgrade` files (schema 37): strict JSON (`kind: "grade"`)
holding the grade's name and values without its project LUT reference, and
its LUT by name and strength with the `.cube` text embedded when it is at
most 32 KiB, otherwise written to a sibling `.cube` file the document names
(a plain file name; an exporter that cannot read the LUT names the sibling
for the operator to supply). Importing registers the LUT under a fresh id
(`assets/luts/<id>.cube`, renamed on a name clash) and adds the grade to
the library (`Name (2)` on a clash). `freemix-cli grade-export` and
`grade-import` work offline on any LUT size; Studio's Colour window exports
the edited grade and imports a file by typed path through the protocol's
`grade_lut_import` and `grade_library_add`, so it refuses LUTs over the
32 KiB wire cap with a pointer to `grade-import`. Protocol 2.56 makes a
grade drag one step of the shared undo history: the input, layer, and
program grade sets and clears carry an optional `gesture` token with
`continues`, the Colour window's binding picks a fresh token per drag
(first set `continues=false`, later sets and the release `true`), and
`fm-engine` folds a continuing set into the newest undo entry when that
entry sets the same target and was opened by the same token (keeping the
entry's inverse; a drag back to its start leaves no entry; an edit in
between, an undo, or a drag whose first set was lost opens a new entry).
Recorded entries and journals carry no gesture. The Colour window's
`Copy to…` menu ticks other targets (Program, inputs, scene layers; locked
ones disabled with the padlock reason; `Every input` and `Untick all`
keys) and sends the draft to each as one standalone set (a linked library
grade by reference, a neutral draft as a clear, targets that already
carry it skipped), so each target is its own undo step.

Current implementation boundary for the layer designer (`SW-006`):
Protocol 2.39 edits every scene layer property live: `scene_layer_*`
commands set a layer's geometry (box, quarter turns, a free tilt in
millidegrees, and an anchor in basis points), appearance, crop, mask, and
z-order, move it in scene order, and add or remove it, each a padlocked,
journaled project edit with an exact undo, announced by
`scene_layout_changed` with the scene's whole layout. Layout records
carry each layer's crop, mask, z-order, tilt, and anchor. The model stores
the tilt and anchor (schema 38); the simulated renderer, the native
daemon, the offline CLI render, and scene animation poses all turn and
scale about the anchor, animated rotation adding to the stored turn.
`fm-ui-model::scene_designer` is the headless designer over that layout:
exact turned-frame geometry and hit-testing, move (the whole selection),
corner and edge scale about the anchor (Shift keeps the aspect), free
rotation about the anchor (Shift steps 15 degrees), anchor drag (Alt on
the anchor, snapping to edges and centre), and crop (Alt on an edge, never
cutting into the mask); numeric fields for position, size, zoom, rotation
to a thousandth of a degree, z-order, anchor, opacity, visibility, crop,
mask and invert; list restacking and forward/backward/front/back; and
snapping to canvas edges and centre, action-safe (93%) and title-safe
(90%) areas, and the other layers' edges and centres. The draft diffs
itself against what it last sent and yields the live edits (moves first);
Studio's Layer designer window (View › Design scene layout…, Edit ›
Preview input › Design layout…, the Animation window's scene mode,
Ctrl+Shift+L, and Design… on a Scene tile's menu) sends them at most every
80 ms while a drag lasts and once on release, every edit after the first
of a gesture marked `continues`, so the engine folds the gesture into one
undo step per layer; its Undo and Redo are the engine's. A padlocked draft
is reverted rather than sent, and the server's layout replaces an idle,
fully sent draft. `Copy as commands` remains a secondary export of the
draft as next-open `freemix-cli scene-layer-*` commands
(`scene-layer-geometry` takes degrees and `--anchor`). Remaining gaps: a
drag of several selected layers is one undo step per layer, not one per
gesture; a scene draws at most 64 enabled layers (the engine, the model's
layer edits, and `freemix-cli` refuse an add or enable past it, and the
designer disables Visible with the reason); a live layer's source cycle is
followed through alias inputs; gesture edits carry a per-gesture token
(Protocol 2.41), so a gesture whose first send was refused or lost opens
its own undo step and a gesture back where it started leaves none; the
native daemon binds a leaf source a live edit starts drawing on the next
frame and rechecks its 64 active-layer bound on every refresh, but a layer
that starts drawing an unplanned nested scene is drawn after the next
start; masks stay rectangular and hard-edged; and `SW-006`
remains planned until its cross-platform capability profiles are
certified.

Current implementation boundary for the mixer strip (`AU-001`) and Fade to
Black outputs (`SW-004`): Protocol 2.40 adds strip pan, labels, and four
VCA strip groups, and the choice of outputs Fade to Black fades. `fm-audio`
pans a mono strip with a -3 dB constant-power law (`gL = sqrt(2)*cos`,
`gR = sqrt(2)*sin` of `(pan+1)*pi/4`: unity per side at centre, +3 dB on
the hard side) and keeps balance for stereo strips; a group member's gain
is its strip gain plus the group gain in dB, ramped like a strip fader, and
a muted group silences its members without touching their own mute. The
model stores pan, labels, groups, and the excluded outputs (schema 39);
labels, membership, and group names are padlocked, undoable project edits,
the group fader and mute are live audio operations, and the output choice
is an undoable project edit. The switcher keeps the excluded outputs (every
output fades by default, including outputs added later); the simulated
renderer and the native daemon draw an excluded logical output, the
fullscreen output, and recordings and streams that follow it unfaded, and
the native master hands those captures the mix before the fade. Studio's
mixer shows the label on each strip's name plate with a label field and a
group menu in the strip details, a pan knob beside balance, and one strip
per used group (rename, fader, mute); the transition panel and the Output
menu carry an `Outputs n/m` menu of output checkboxes. `freemix-cli`
provides `audio-label`, `audio-group`, `audio-group-name`,
`audio-group-level`, `ftb-output`, `audio-strip --pan`, and their `remote-`
forms. Protocol 2.44 (schema 42) adds a show-wide solo mode, group solo,
and nested groups. `sip` keeps solo-in-place on Program; `pfl` and `afl`
leave Program untouched and feed the soloed strips to a monitor bus that
`MasterMixer` renders alongside Master (PFL after delay and mapping, before
fader, groups, balance, pan, mute, and follow-video; AFL after the faders,
balance, and pan), following Master when nothing is soloed. A soloed group
solos its members; a group may nest in a parent (cycle-free, so a chain is
at most four groups) and a member follows the whole chain: gains multiply,
any muted group mutes, any soloed group solos. The solo mode and parents
are padlocked, undoable project edits; group solo is a live operation and
persists like the group fader. `audio_meters` carries the monitor bus
meters; Studio's mixer adds an M/S pair and a parent menu on each group
strip and a pinned monitor strip with SIP/PFL/AFL keys and its meter.
`freemix-cli` adds `audio-group-solo`, `audio-group-parent`,
`audio-solo-mode`, and their `remote-` forms. Per-output fade durations
arrive with Protocol 2.48 below.

Protocol 2.53 (schema 50) makes the monitor bus audible on headphones
(`AU-001`). The project's monitor output names a local ALSA output device
(or none, meter-only) and its own level and mute; the device is a
padlocked, undoable project edit (choosing it again reopens it without an
undo entry) and the
level a live audio operation, both replicated in the snapshot. `freemixd`
plays the rendered monitor bus (Master, or the PFL/AFL solo) through an
`FFmpeg` child reading interleaved `f32le` PCM on its standard input and
writing `-f alsa <device>`; the render loop interleaves each block with the
level ramped linearly across it, clamps it to full scale, and hands it to a
writer thread through a two-block queue that replaces its oldest block, so
a slow or stalled device drops blocks instead of stalling Program. Latency
never accumulates: the writer drops blocks older than 100 ms and, after a
write blocked on a full input pipe (as after `FFmpeg` starts, or once a slow
device clock fills it), skips blocks until the pipe backlog is about 40 ms.
A device that stops reading for 3 s is failed and its child killed; the
failure is logged with a bounded, redacted tail of the child's standard
error, and only choosing the device again reopens it. Program, recordings, streams, and the
monitor meters never see the headphone level. `audio_meters` reports the
device as off, playing, or failed. Studio's monitor strip adds a device
field (Enter plays, an empty field stops, Enter on a failed device retries),
the device state, a level entry, and a mute key, all with English and
Spanish hover help. `freemix-cli` adds `audio-monitor-device` and
`audio-monitor-level` and their `remote-` forms. `freemixd`'s
`native_monitor_output` test reads the PCM that reached a captured device
(unity, -6 dB, mute, undo and redo of the device), fails a missing ALSA
card through the real `FFmpeg`, and plays the real ALSA `null` device where
ALSA is available. Remaining gaps: the device is opened only through ALSA,
so macOS and Windows report it failed; there is no device enumeration (the
operator types the ALSA name); the monitor output carries the Master
channel layout without its own channel mapping or downmix; the ALSA buffer
`FFmpeg` sizes and its internal queues add fixed device latency, and
draining a backlog assumes the 64 KiB Linux pipe and leaves a short gap; the automix/talkback `MonitorBus`
(`AU-008`) is not yet the source; and `AU-001` remains planned until its
capability profiles are certified.

Current implementation boundary for Fade to Black and the T-bar (`SW-004`):
Protocol 2.48 (schema 45) gives each output an optional Fade-to-Black
duration that overrides the global one and adds a show-wide curve (`linear`
or the exact integer smoothstep `ease`); both are padlocked, undoable
project edits replicated in the snapshot, and the switcher fades every
affected output over its own duration on that curve. The T-bar drives every
timed kind manually: all thirteen mixes and Stingers, whose position picks
the clip frame (a manual Stinger names its clip length and needs its slot
configured with ready media). Moving back reverses the mix exactly, and
commit and cancel complete at the next frame boundary. Studio's T-bar runs
the kind selected on the FADE/WIPE/SLIDE/ALPHA keys or the MORE menu (other
mixes and configured Stingers), and a favourite's context menu starts it on
the T-bar; the keyboard drives it with `T` (start), `PgUp`/`PgDn` (10%
steps), `End` (commit), and `Esc` (cancel), and an OSC controller with
`/freemix/switcher/manual/start/<kind>` and
`/freemix/switcher/manual/position <0..10000>`. One gesture model feeds the
lever, keys, and controller, and `freemix-studio`'s `tbar_consistency` test
renders identical frame sequences through the lever, keyboard, controller,
and API for every mix. The Fade-to-Black outputs popover adds an own
duration per output and the curve keys, all with English and Spanish hover
help. `freemix-cli` adds `ftb-output-duration`, `ftb-curve`, their
`remote-` forms, and every kind on `tbar-start`. Remaining gaps: the
web control surface still starts only its fixed manual kinds, no hardware
T-bar controller profile ships beyond OSC, and `SW-004` remains planned
until its capability profiles are certified.

Current implementation boundary for item 2 browser title control (`GX-006`):
`freemix-web` provides a transport-free title editor model over a narrow server
port, reusing `fm-titles` field identities, types, and values. Per-field drafts
validate text length, numbers, colors, and image references; previews return
exactly the values a take or update would send; and save, take, update-live,
and off-air are each gated by connection, role permission (`edit_project` for
edits, `transition` for take/off-air), preview freshness, and the server's
on-air state, which the server re-checks atomically. Every value-carrying
command carries the title's base revision; a conflict keeps the user's drafts,
marks the title stale, shows base, server, and draft values per field, and
requires an explicit rebase or discard, so nothing is silently overwritten.
Remaining gaps: the protocol carries no title commands or snapshots and the
daemon hosts no titles, templates have no per-field exposure flag, revisions
are per title rather than per field, previews render no image, and there is no
browser UI, request timeout, or reconnect resync. `GX-006` remains planned.


Current implementation boundary for item 3: `fm-browser-input` provides a
deterministic network policy that strictly normalizes URLs (including numeric
IPv4 and IPv6 literal forms and dot segments), rejects credentials and
malformed hosts, applies scheme, private-network (also after resolution), and
wildcard host allow/deny rules where deny wins, and validates bounded redirect
chains against loops, HTTPS downgrades, and `file:` targets. Custom CSS is
accepted only after a lexical scan rejects style breakout, imports, escapes
outside strings, unsafe functions, and excess external `url()` references,
which are returned for policy checks. Remaining gaps: no real browser engine,
DNS, or sockets; the fake renderer still navigates through the older policy;
in-page fetches are not policed; and browser audio, HTML5 media, on-disk
profile isolation, refresh-on-activate, and zoomed input translation remain
contracts or fakes. `IN-013` remains planned.

Current implementation boundary for item 4: `fm-data` parses bounded CSV and
JSON from in-memory bytes into exact typed rows. CSV supports quoting, embedded
newlines, every line-ending form, a BOM, custom delimiters, and headerless
columns; JSON keeps integers and decimals exact without floating point and
rejects duplicate keys and trailing data. Byte, row, column, field, depth, and
value limits fail with located errors. A row cursor navigates with stop or
wrap edges, stays valid across refresh, and selects by exact key. A validated
title mapper coerces values onto text, number, color, and image fields with
explicit rounding and color rules, fallbacks, optional fields, and per-field
diagnostics. Remaining gaps: no XML/XPath, RSS, HTML-table, spreadsheet,
Google Sheets, chat, or database adapters, no real HTTP or file polling, and
mapped values do not yet feed `fm-titles` templates, the project schema, or
ticker updates. Item 4 remains incomplete and `GX-004` remains planned.

Current implementation boundary for item 4 title binding (`GX-005`):
`fm-data` binds rows onto text, integer, color, validated image URI (scheme
allow-list, length limit, required host for HTTP), and visibility fields
(explicit truth values, non-empty, exact numeric comparison, inversion).
Formatted fields render exact fixed-decimal numbers with selectable rounding,
separators, sign style, and affixes without floating point, and dates from
ISO-8601 text or Unix seconds at a caller-supplied fixed UTC offset through a
bounded strftime-like pattern. Bindings can follow the cursor-selected row. A
preview state stages a result with a per-field change list against live values
and commits atomically with a revision counter, stale-stage detection, and a
choice between refusing errored results and keeping live values for failed
fields. Remaining gaps: bound values do not reach `fm-titles`, the renderer,
the project schema, the protocol, or a UI preview; there is no time-zone
database or localization; image URIs are never fetched; and preview state is
in memory only. `GX-005` remains planned.

Current implementation boundary for item 5: `fm-playback` provides a
fixture-backed `ListPlayer` that advances once per monotonic engine frame tick
with ordered auto-next, loop, auto-first, per-item in/out marks, exact
durations that trim or hold, and a seeded deterministic shuffle per loop cycle.
Programmed GO (Play, Next, Previous, First, Stop) lands at an absolute or
relative frame, is idempotent by ID, and records executed, ignored, or
cancelled outcomes under explicit item, pending, and history limits. Remaining
gaps: no daemon, protocol, persistence, or vMix API wiring, no cross-input GO
bank, no M3U or file-list import, no transitions or variable speed between
items, and no real-media acceptance. `IN-006` and `OR-015` remain planned.

Current implementation boundary for item 5 playlist automation:
`fm-automation` provides a caller-clocked playlist engine that sequences whole
inputs with fixed or media-length durations, typed transitions validated
against each entry, loop off/all/one, and start now, after a delay, or at a
wall time with reject, start-now, or join-in-progress late policies. Pause,
resume, next, previous, jump-to, stop, enable/disable, and take-guarded
media-ended reports run against a scratch copy, so failed commands change
nothing. Entry boundaries fire at their exact scheduled times regardless of
poll rate, with gaps collapsing into bounded skipped/looped events. A versioned
tab-separated snapshot round-trips, and restore rejects inconsistent state.
The product slice drives it from the engine frame cursor: the project
(schema 28) persists one playlist with its automation state; `fm-engine` runs
playlist edits, controls, and a clock-driven `PlaylistAdvance` as ordinary
idempotent commands, so an entry that comes on air selects its input and cuts
or transitions Program (cut, fade, alpha fade, wipe, slide, zoom) on exactly
its scheduled frame; Protocol 2.25 carries `playlist_set`, the
`playlist_start/stop/pause/resume/next/previous/jump` controls (project-edit
and transition permissions), the clock's `playlist_advance`, and the
`playlist_changed` event that `fm-ui-model` reduces; and simulated `freemixd`
journals the clock's advances as wall time reaches each boundary, so replay and
restart reproduce the exact frames (`freemix-cli remote-playlist` drives it).
Native-media `freemixd` runs the same clock from its frame pacer: once a
boundary is the next frame the pacer realizes, the session loop submits the
journaled advance (one comparison per loop pass, nothing on the frame path),
which realizes that frame and the entry's transition through the native
renderer, so Program cuts on exactly the scheduled frame; playlist start,
stop, pause, resume, next, previous, and jump are accepted there too. Journal
replay ticks idle frames up to where each recorded command began, so the
idle frames the native pacer realizes between commands no longer make
recovery refuse a native journal, and a playlist playing at a SIGKILL
resumes on restart from the last journaled frame (`native_playlist`
covers both under lavapipe). Idle frames after the last command are durable
too: the native session loop writes a journal progress marker (one
overwritten `journal_progress` row: head sequence, revision, and full
position) once per second of realized frames at an idle boundary, and folds
idle progress into the manifest every five minutes of frames, so recovery
ticks idle up to the newest marker and a SIGKILL loses at most a second of
idle progress; replay refuses, before ticking, a journal asking for more
than 24 hours of idle frames. The playlist clock also runs inside the
recorder startup barrier.
Protocol 2.26 completes snapshots with the playlist: a snapshot carries the
show's `frame_rate` and announces with `playlist=1` a `playlist_state` record
(revision, frame, loop mode, entries, run state) that follows its title
states, and clients stay synchronizing until it arrives. Studio's Playlist
side tab lists the entries with input, transition, and timecode duration,
lights the on-air entry red and the next one green, counts the on-air entry
down, offers Start/Stop/Pause-Resume/Previous/Next and a per-row jump or
start-from key under the transition permission, and edits the list (add,
remove, reorder, enable/skip, duration, transition, loop mode) under
`edit_project` while the playlist is stopped or finished.
Remaining gaps: frames a client transition command (or Stinger mutation)
realizes run inside that command's execution, where no other journaled
command can be ordered, so a boundary falling inside them is cut on the first
frame after the command (its scheduled start, and so every later boundary, is
kept); a SIGKILL still loses up to one second of idle progress (the marker
cadence), and marker and fold writes run on the session thread between paced
frames rather than on a separate writer; simulated pacing anchors to the wall clock when a
boundary becomes pending, so it is approximate across commands and the catch-up frames
render synchronously, and controls such as Pause or Next execute at the
lagging frame cursor, so Studio's countdown is approximate on the simulated
daemon; fixed durations only, with no media
length, end-of-media detection, or media-ended reports; no stinger
transitions; no enable/disable, loop change, or list editing while the
playlist runs; no scheduled starts from Studio; no per-entry hard starts or
calendar recurrence; no vMix API or as-run log. `OR-015` remains planned.

Current implementation boundary for item 5 programmed GO (`RC-017`): the
operator cue list is implemented end to end (schema 48, Protocol 2.51). A
project persists up to eight bounded cue lists of named cues with typed
actions (preview; cut, mix, or stinger takes; overlay on and off; title on,
text, and off; Fade to Black; playlist start and stop; strip fader and mute;
recorder and stream start and stop; media play, pause, seek, and restart
through the media transport; frame waits; and millisecond delays converted
to whole frames at the show rate, rounded up), optional auto-follow,
and each list's run state (standby, live, pending wait, played, fault).
Every referenced input and stream target is validated, and removing an
input a cue names is refused with the `cue_action` usage. `fm-automation`'s
`CueStack` steps standby, GO, back, skip, jump, stop, and reset; the engine
plans a GO as one revision whose actions up to the first wait apply
atomically on one frame (a refused action refuses the GO), and `freemixd`'s
cue clock submits each wait's `cue_advance` for exactly its due frame,
journaled and replayed after restart; a restart restores each list's
standby pointer and pending wait and still ends the wait on its due engine
frame (downtime does not count), the native recorder startup barrier runs
the cue clock, and on the simulated daemon a client's cue control catches
up to the show's wall-clock frame like a playlist control. On the native
runtime a cue's record and stream actions start and stop the same recorder
segments and stream workers as the direct commands. List edits are undoable,
padlock-guarded project edits; GO and the other controls are journaled
operator transitions outside the undo history. A GO never acts with more
authority than the session that fires it: before anything is applied,
`fm-control` also requires the permission of every action of the chain the
GO arms (the fired cue's actions across its waits and every cue its
auto-follows reach: `select_preview` for preview, `control_audio` for strip
gain and mute, `edit_project` for title text, `transition` for the rest),
refusing `permission_denied` with the missing permission named; the cue
clock then runs those waits and follows on the authority checked when the
chain was armed, and an edit (or undo/redo) of a list whose cue is waiting
also needs whatever permission it adds to the armed chain. The same
classes gate the web tally's GO and `freemix-server` session scopes, and
Studio disables GO and a cue's fire key with a reason when the session
lacks one. A cue can run a project script as one step (`run_script`,
Protocol 2.60, schema 55): the script runs on the step's frame against the
show the frame's earlier actions left, all or nothing and sharing the
frame's one take of Program; a failed script fails the step with its typed
`script_failed` reason (a GO is refused whole; a step after a wait sets the
list's fault and applies nothing of its frame). A script cannot fire a cue
or run a script, so nesting stops at one level; a list runs at most eight
scripts; a script a cue runs cannot be removed; and since a step may run
later against an unknown show, a GO needs the permission of every verb the
script holds in any branch, and an edit of a script an armed chain runs
needs whatever permission its new verbs add. Studio docks a Cues panel
(list picker, inline cue and action editor covering every action kind, a big GO key with the standby
cue, Back, Skip, Stop, Reset, per-cue standby and fire keys, and `G` for
GO) with English and Spanish hover help; the phone tally page adds GO and
Standby; and `freemix-cli` has local and `remote-` `cue-lists`,
`cue-list-set` (backslash-escaped `;`, `:`, `,` in names and title text),
`cue-list-remove`, `go`, `cue-standby`, `cue-back`, `cue-skip`, `cue-stop`,
and `cue-reset`. The older per-input `GoEngine` was removed from
`fm-automation` and `fm-control`'s `AutomationPlane`; cue lists supersede
it. Remaining gaps: lists are not bound to an input button or previewed
before GO; there is no data-source update action and no timecode delays;
the web tally page offers only GO and Standby. `RC-017` remains planned.

Current implementation boundary for sandboxed scripting (`RC-011`): the
first slice is a purpose-built language rather than a Wasm runtime (schema
54, Protocol 2.58). `fm-script`, a dependency-free foundation crate, parses
and interprets `FreeMix` Script: integer, boolean, and string variables,
`if`/`elif`/`else`, `repeat`, `while`, `stop`, and 24 action verbs that map
onto cue actions. The language has no file, network, process,
clock, or randomness access at all; its only host interface reads the
Program and Preview input numbers and performs one show action at a time.
Runs are deterministic and bounded by an instruction budget (time), an
action budget, and variable and string budgets (memory); the parser bounds
source size, lines, block nesting, and expression nesting. A project stores
up to sixteen named scripts; edits are padlocked, undoable project edits,
and a run is a journaled operator command whose actions go through the cue
GO machinery, so each is checked and refused exactly as the matching
command would be, and the whole run lands on one frame or is refused with a
typed `script_failed` reason naming the failure kind, line, and column.
A run never acts with more authority than the session that starts it: the
authority plans it on a copy of the show first, and the session must also
hold the permission of every action the planned run asks for (so the
branches it takes on the current show are covered), or the run is refused
`permission_denied`, naming the missing permission, before anything runs.
Studio adds Edit ▸ Scripts… (a list, a name field, a monospace editor that
marks the first syntax error's line as the operator types and the line a
refused run stopped at, Save, Discard, Run, Delete, and the last run's
action and instruction counts) with English and Spanish hover help, and
`freemix-cli` has local and `remote-` `scripts`, `script-set`,
`script-remove`, and `script-run` (by id or name). Remaining gaps: no Wasm
component runtime or declared capability manifest; scripts cannot wait,
loop across frames, or read state beyond Program and Preview (a `wait`
statement was not added: spanning frames would need a persisted per-run
pending action list and its own clock and fault handling, so a script that
must span frames is split into script steps of a cue separated by cue
waits); the budget is counted in instructions, not wall-clock time; and a
refused action's own typed reason is folded into `script_failed` (its code
and message are kept). A cue can run a script as one step (Protocol 2.60,
schema 55; see `RC-017` above for its frame, failure, recursion, removal,
and authority semantics). `RC-011` remains planned.

Current implementation boundary for item 5 slideshows (`IN-009`):
`fm-playback` provides a deterministic slideshow with per-slide frame
durations, ordered or seeded-random cycles without repeats, cut or crossfade
frames with exact basis-point weights, exact rational Ken Burns crops that
edits cannot disturb on air, live insert, remove, and move that refuse to drop
on-air slides, large-set thumbnail suppression with filename labels, and
natural-sorted, extension-filtered folder listing from caller-supplied names.
Remaining gaps: nothing decodes images or produces thumbnails, the compositor
does not consume crops or mix weights, there is no filesystem scan or watch,
no engine, protocol, persistence, daemon, or Studio wiring, and only cut and
crossfade transitions exist. `IN-009` remains planned.

Current implementation boundary for item 6: `fm-automation` provides a typed
per-input trigger catalog (transition and overlay in/out, completion,
countdown and playback-time thresholds, call state, active speaker, and replay
completion) that edge-detects changes between caller-supplied observations,
fires forward threshold crossings once, resets on flagged discontinuities, and
schedules bounded ordered actions atomically. A macro script engine installs
macro batches atomically with call-cycle, depth, and size checks and runs
commands, atomic multi-command transactions, waits, timed conditions, if/else,
calls, and variables on a per-run logical clock with preconditions, bounded
concurrency and step budgets, cancellation, and a bounded log. Remaining gaps:
nothing in the engine, daemon, or protocol produces observations, executes
dispatches, or persists bindings and macros; macro permissions are not
enforced; scripts have no loops or arithmetic and cannot recall dispatched
commands on cancellation. `IN-027` and `RC-010` remain planned.

Current implementation boundary for item 6 production clocks: `fm-clock`
provides deterministic count-up and countdown timers driven by caller-supplied
monotonic readings, with stop-or-overrun expiry delivered once at the exact
nanosecond, all-or-nothing typed commands, and drift-free pause cycles. It
formats broadcast presets including `HH:MM:SS:FF` at fractional frame rates,
resolves time of day through fixed offsets or POSIX `TZ` rules with exact DST
transitions, reports media remaining time with mark-in/out, accumulates
stream/record/replay session timers, and exposes a bounded named-clock registry
whose readings flatten into title binding values. `fm-titles` clock elements
can reference a named production clock with an optional per-element preset
through a caller-supplied clock source, degrading to a background-only element
when unresolved, and scene and scoreboard clocks share the same formatter.
Title templates define the clocks they show (schema 29 `.fmtitle` `clocks`
table, strictly decoded with canonical POSIX zones and whole-second UTC
targets): time of day in a POSIX zone with its DST rules, a countdown to a
UTC instant that holds at zero or overruns negative, and a stopwatch since
the title's take. freemixd builds a registry from those definitions for every
title render: the monotonic clock is the rendered engine frame on the project
frame grid, time of day and countdowns read the system wall clock, and the
take is the first frame the title is on Program or a realized overlay
(projected frames in native mode, monitor polls in simulated mode); clock
titles refresh at the bounded title rate. `freemix-cli title-input-add`
imports clock templates. Remaining gaps: no shared project or engine clock
registry, so title clocks are per title, not operator-controllable timers,
and not exposed through the protocol, CLI commands, or UI; no project-level
time zone (each time-of-day clock names its zone); templates cannot show
media remaining, session, or playlist clocks; the simulated take is dated at
monitor-poll granularity; designer compositions and crawls take no clock
source; no IANA tz database, leap seconds, drop-frame timecode,
playback-speed-aware media remaining, or locale formats. `SW-013` remains
planned.


Current implementation boundary for item 8: `fm-automation` provides a
bounded MIDI 1.0 stream parser (running status, interleaved real-time bytes,
bounded SysEx capture, 14-bit controller pairs, pitch bend) and an OSC 1.0
codec with typed arguments, bounded bundles, and address-pattern matching,
both pinned by exact byte fixtures. A control map binds device-keyed buttons,
absolute faders, and relative encoders to typed actions with velocity ranges,
deadband, inversion, and soft takeover, offers a learn mode with conflict
policies, and re-associates bindings on reconnect by device key. A feedback
router turns tally and level state into coalesced MIDI and OSC messages for
LEDs and motor faders. Remaining gaps: no MIDI port enumeration, hotplug, UDP
transport, or raw HID parsing; nothing is wired into `freemixd`, Studio, or
persistence; there is no learn UI; and NRPN/RPN, MIDI 2.0, and GPIO or
network tally adapters are absent. `RC-005` and `RC-006` remain planned.

Current implementation boundary for item 8 fader feedback (`AU-009`):
`fm-automation` adds a bounded per-control `ControlCoalescer` that keeps the
latest absolute value, sums relative deltas, preserves button edges in order,
pairs same-tick 14-bit MSB/LSB updates, and refuses rather than drops new
controls when full; coalesced ticks feed the existing soft-takeover pickup.
`FeedbackRouter` now schedules outbound feedback per tick with per-device
budgets, round-robin fairness, per-binding minimum intervals, touch
suppression with a forced resync on release, and acknowledged-level echo
suppression. `fm-audio` adds a `GainSmoother` whose per-sample linear ramps
have a guaranteed step bound and land bit-exactly on target across block
splits and retargets. Remaining gaps: no daemon, Studio, CLI, or real-time
mixer wiring; cross-tick 14-bit glitches are only smoothed, not held; ramps are
linear in gain with no fader taper; and there is no release-settle hold,
per-device byte-rate limit, end-to-end device-to-audio test, or motor-fader
hardware evidence. `AU-009` remains planned.


Current implementation boundary for item 9: `fm-ptz` encodes and parses VISCA
commands, inquiries, and replies for pan/tilt, zoom, focus, presets, and
system control with explicit range errors, translates each PTZ intent into a
complete drive set so zero components stop prior motion, frames VISCA-over-IP
payloads and control messages with a wrapping sequencer, and maps joystick and
mouse input to axis speeds through deadzones and response curves. Remaining
gaps: no serial or UDP transport, retransmit, timeout, or reply correlation,
no camera adapter or hardware validation, presets are not yet switchable
virtual inputs with thumbnails, no HID or UI binding, and no ONVIF, Panasonic,
NDI, or digital PTZ. `IN-028` remains planned.

Current implementation boundary for item 10: `fm-telestrator` provides a
sequenced, bounded collaborative board with normalized strokes, per-author and
global undo/redo with bounded history, redo invalidation on new edits, and
explicit undo/redo conflicts instead of silent corruption. A freehand eraser
removes coverage from earlier strokes only, per-author laser pointers expire
against a caller-supplied clock outside the undoable operation log, an author
limit bounds participants, and ten fixed shortcut slots carry validated tool
presets. Deterministic CPU rendering exports straight or premultiplied RGBA.
Remaining gaps: no text primitives or custom image stamps, no GPU renderer or
persistence, no protocol, browser, or device input integration, no
low-latency program/input preview, no web monitor, and no routing of the layer
into the compositor. Laser pointers are not replicated, so item 10 remains
incomplete and `GX-007` remains planned.

Current implementation boundary for item 10 web tally (`RC-014`):
`freemix-web` provides a transport-free `TallyLink` over the current client
reducer with realized-state program/preview/idle lights (both sides of an
active manual transition count as on air), an offline state that never shows
stale red, capped exponential reconnect, heartbeat and sync timeouts, terminal
handling of role denial and protocol mismatch, and permission-gated
full-screen, Preview, and Cut shortcuts. The `freemixd` WebSocket gateway serves
a self-contained `/tally` page with `no-store`, strict CSP, `nosniff`, and
frame-denial headers. Because browsers cannot set `Authorization`, the page
presents the same `FREEMIXD_WEB_TOKEN` as a `freemix.bearer.<base64url>`
WebSocket subprotocol, accepted only from the serving origin, compared in
constant time, and never echoed; roles still come from the handshake.
Remaining gaps: the gateway is loopback-only, simulated-mode-only, without TLS,
pairing, or per-device tokens, and admits one Web peer; Web peers receive no
monitor frames, so there is no low-latency web preview; the page's JavaScript
duplicates the Rust model and is only manually browser-verified; and FTB, audio
tally, and Cut confirmation are not modelled. `RC-014` remains planned.


Exit: graphics/data/automation/operator ecosystem `P1` rows pass.

## 8. Phase 6 — guests and collaboration (10–14 weeks)

1. Build guest signaling, lobby, invitations, device preflight, reconnect.
2. Implement up to eight WebRTC guest inputs.
3. Implement return video selection, mix-minus, talkback, and latency health.
4. Add chat and guest manager roles.
5. Add remote desktop capture node with tally and optional audio.
6. Harden NAT/TURN, bandwidth adaptation, echo, and permission UX.
7. Add optional authorized Zoom adapter as a separate licensed workstream.
8. Run eight-guest network impairment and long-call tests.

Current implementation boundary for items 1, 3, and 4: `fm-guest` is a
deterministic, transport-free planning model for up to eight guests. Each
guest selects return video and, independently, return audio from Master,
Headphones, Outputs 1–4, or aux buses A–G; mix-minus plans give each guest the
clean bed of that source plus the other guests' microphones and never their
own microphone or manager talkback. Microphone mute and gain apply to every
route carrying that microphone and survive reconnect, expired reconnect
windows end their sessions and free slots, and manager commands, per-guest
health, and chat review are checked against role permissions before any state
changes. Remaining gaps: no audio engine, WebRTC adapter, daemon, or protocol
consumes these plans; clean return beds are an engine contract only; there is
no per-guest return level, echo or latency measurement, authentication,
persistence, audit, signaling, encryption, bandwidth adaptation, or NDI
exposure. `RC-001`, `RC-002`, and `RC-003` remain planned.

Current implementation boundary for items 2 and 6: `fm-io-webrtc` provides a
deterministic, network-free SDP subset for encrypted browser guests that
requires `UDP/TLS/RTP/SAVPF`, a SHA-256 DTLS fingerprint, ICE credentials,
setup role, rtcp-mux, and one BUNDLE group, keeps Opus, H.264, and VP8, renders
canonically, and builds answers by deterministic codec intersection with video
capped at 4 Mbps. A signaling session resolves offer glare by polite and
impolite roles, bounds trickled and early candidates, rejects stale ICE
generations, and performs bounded ICE restarts with fresh credentials. A
bandwidth controller walks a ladder capped at 1080p, 60 fps, and 4 Mbps from
the lower of fresh REMB and TWCC estimates, stepping down immediately and up
only after a held margin, with loss-driven step-down. Remaining gaps: no real
WebRTC, DTLS, or ICE stack, no `fm-server` signaling transport or `fm-guest`
wiring, no candidate gathering or fingerprint verification, simulcast and data
channels are rejected, header extensions such as transport-wide-cc are
dropped, and H.264 matching ignores levels. `RC-001` remains planned.

Exit: Remote Show scenario passes; named proprietary rows remain explicitly
pending until licensed adapters pass.

## 9. Phase 7 — replay and sports (14–20 weeks)

1. Implement rolling segment storage and per-camera storage roots.
2. Synchronize and record up to eight sources with four-channel audio.
3. Add event database, marks, last-N presets, tags, notes, lists/folders.
4. Add Replay A/B, live/recorded modes, angle selection, linked channels.
5. Add jog/shuttle, variable speed, transitions, auto-return, audio policy.
6. Add replay multiview and four-angle quad view.
7. Add highlights, background music, export during record.
8. Add replay shortcuts, activators, and dual-controller profiles.
9. Validate storage prediction, retention, low-space behavior, and recovery.
10. Certify Profile C.

Current implementation boundary for items 1–5: `fm-replay` keeps an in-memory
rolling segment catalog with deterministic overlap tie-breaks and gap
reporting, per-camera source timelines with binary-search frame resolution,
bounded observation counts, and pruning that keeps the cutoff instant
resolvable. Event capture atomically verifies gap-free coverage and at least
one frame for every angle before protecting those segments; removal releases
the protection and detaches the event from every list. Last-N presets (5, 10,
and 20 seconds) anchor at the live edge, and events, lists, folders, tags,
names, and notes have explicit limits with typed errors. Replay channels switch
event angles at the same timeline instant and resolve the exact source frame
and segment, or the live edge. Remaining gaps: nothing is persisted or
connected to `fm-record`, space reclamation and file deletion are
caller-driven, frame resolution does not decode or present media, A/B
playback has no real clock, per-event list overrides and controller mapping
are absent, and no eight-camera storage profile is certified. `OR-012` and
`OR-013` remain planned.

Current implementation boundary for items 6, 8, and 9: `fm-replay` plans a
synchronized Quad View or a multiview of up to eight distinct angles plus A/B
channel tiles in a near-square row-major grid, resolving each tile's exact
frame through the channel resolver without mutating decks and labeling tiles
LIVE or REPLAY with reported skew. Two independent controller profiles map
shared keyboard keys and per-device buttons and MIDI notes to typed replay
actions (marks, play and shuttle rates, bounded jog, angle and channel select,
live toggle), refuse cross-profile keyboard conflicts, and apply actions
atomically to decks and marks. A disk-health monitor converts configured
bitrates into per-camera and per-root byte rates and predicts remaining
recording time with warning, critical, and full levels that worsen
immediately and recover only past a hysteresis margin. Remaining gaps: tiles
are not decoded, scaled, or composited; controller bindings have no real
device input, persistence, or jog acceleration; disk samples are caller
supplied without OS or SMART queries or `fm-record` low-space wiring; and no
storage profile is certified. `OR-014` remains planned.

Current implementation boundary for item 7: `fm-replay` compiles highlight
reels of ordered event clips (one angle each, trims, exact rational speeds,
cuts or dissolves) into an exact output timeline whose frames resolve to one
or two recorded source layers with exact mix weights and sample-exact audio
positions. A music bed plans once-through or looping playlists with linear
crossfades, sample-exact ducking under program-audio clips, merged nearby
ducks, and an end fade using integer gains. An export-during-record queue
accepts only gap-free, closed segments, protects them from retention, advances
in bounded frame chunks with progress and cancellation, and revalidates
segments and source frames so an export never resolves pruned media.
Remaining gaps: nothing is decoded, mixed, encoded, or written; dissolve is the
only transition and clips play forward only; the queue is not wired to
`fm-record`, persistence, protocol, or UI; and exports needing an open segment
are refused rather than deferred. `OR-013` remains planned.


Exit: Sports Show scenario passes on certified storage/hardware.

## 10. Phase 8 — professional hardware and advanced formats (ongoing, 16–24 weeks)

Parallel adapters, each with its own certification:

1. Blackmagic capture/output, embedded audio, key/fill, reference.
2. AJA capture/output, embedded audio, key/fill, reference.
3. Bluefish444 and prioritized capture families.
4. NDI and OMT send/receive, alpha, tally/metadata, direct record where possible.
5. Virtual camera/audio devices with signing/install/uninstall.
6. Genlock/PTP and timecode workflows.
7. Interlaced output and advanced deinterlace.
8. 10-bit HDR ingest, compose, scopes, record, stream, display.
9. VST3 host and optional AU/LV2 compatibility.
10. UHD Profile B certification and multi-GPU policy if benchmarks justify it.

Exit: each adapter ships only with a platform-specific conformance report.

## 11. Phase 9 — full parity and release certification (12–20 weeks)

1. Close remaining `P2` rows: presentation/DVD/import surfaces according
   to legal platform scope.
2. Add vMix HTTP/TCP/tally compatibility adapter.
3. Complete virtual sets, advanced title import, social moderation adapters.
4. Complete localization and accessibility audit.
5. Exercise every universal acceptance scenario on each applicable Tier-1 OS.
6. Run 72-hour release soak and disaster-recovery drills.
7. External security, broadcast-operator, and hardware conformance review.

Current implementation boundary for item 2: `fm-vmix-compat` translates a
bounded set of HTTP functions and now frames the persistent TCP API:
FUNCTION with a server-acknowledged completion handshake, TALLY, XML with an
exact UTF-8 byte length, XMLTEXT through a bounded `vmix`-rooted XPath subset,
SUBSCRIBE/UNSUBSCRIBE for TALLY and ACTS, VERSION, and QUIT. A hub of at most
64 sessions fans out per-session change events in deterministic order.
Remaining gaps: no sockets or running-server wiring, a partial function and
state-document surface, no descendant or wildcard XPath, a partial ACTS set,
no authentication, and no byte comparison against a real vMix 29 instance.
`RC-009` remains planned.

Current implementation boundary for item 3 (social moderation): `fm-social`
provides a deterministic moderation desk over its in-memory aggregator.
Operator commands are gated by Viewer, Moderator, Producer, and Admin roles
and recorded in a bounded audit log with monotonic sequences; approved
messages can be hidden, pinned on air, or rotated round-robin on a
caller-supplied clock, and the on-air message is written into typed
`fm-titles` fields atomically at a scene revision. Remaining gaps: no real
provider adapters, transport, or authorization flows, no browser approval UI
or control-protocol wiring, no persistence of desk, audit, or operator state,
no cross-provider de-duplication, no unbounded-store limits in the
aggregator, and no authenticated operator identity. `GX-010` remains blocked
on exact authorized integrations.

Current implementation boundary for virtual sets (`IN-021`): `fm-virtual-set`
plans exact, deterministic virtual-camera moves between presets with easing
curves defined by integer control points, and a layer-stack planner orders
background, keyed-talent, and foreground desk layers at independent parallax
depths into output-pixel rectangles. Reflections and shadows are emitted as
derived layer entries (a mirrored rectangle with fade rows, or an offset matte
with zoom-scaled blur), and a bounded preset store recalls snapshots atomically
at a scheduled frame. Remaining gaps: no compositor draws the plan, presets
hold only camera pose and timing, the planner is separate from the existing
`VirtualSetScene` model, layers are axis-aligned rather than four-corner
planes, and nothing reaches the engine, commands, persistence, or UI.
`IN-021` remains planned.

Current implementation boundary for item 4 (localization, `RC-016`):
`fm_ui_model::i18n` declares typed message keys with English inline and a
complete Spanish catalog that falls back to English per key, named
arguments, one/other plurals, and a validator for missing keys, malformed
templates, and argument or plural-shape mismatches. Locale helpers format
grouped numbers, decimals, percent, dB, bytes, durations, 24h/12h clocks,
and dates, and parse locale decimals. Studio's top bar, menus, toasts, input
bank, status bar, and disabled-control reasons use the catalogs; View ›
Language switches at runtime and `--lang`/`--clock` or the POSIX locale
environment choose the start locale. Remaining gaps: transition, monitor,
mixer, overlay, stinger, and add/remove-input panels plus daemon messages
stay English, the choice is not persisted, there is no RTL catalog, font,
or bidi pass, and no accessibility audit. `RC-016` remains planned.

Exit: no unclassified vMix 29 public feature and no incomplete P0/P1/P2 row
without a documented legal/platform exception.

## 12. Workstream ownership

Suggested durable teams:

- media core and clocks;
- GPU/compositor/color;
- audio/DSP;
- codecs/network/recording;
- platform and professional hardware;
- engine/control/persistence/security;
- studio/web UX;
- graphics/data/automation/plugins;
- replay/guests;
- QA automation and hardware lab.

Crate ownership follows these boundaries. Cross-team APIs are reviewed through
small executable conformance tests, not a giant shared facade.

## 13. First backlog, in order

1. Create capability ledger machine-readable schema.
2. Write ADTs for frame rate, timebase, color, channel layout, memory domain.
3. Write command/event/revision state-machine tests.
4. Write simulated clock and frame generator.
5. Write graph validation and bounded resource planner.
6. Build one platform camera/file adapter behind `fm-io-api`.
7. Build one wgpu composite-to-display path.
8. Build one audio capture-to-output path.
9. Connect Cut/Fade command through server to a scheduled frame.
10. Save/restart/recover the project.
11. Add one program recorder and process-kill recovery test.
12. Port the vertical slice to the other two Tier-1 OSes before adding breadth.

## 14. Scope control

At each phase:

- update the parity ledger;
- publish measured capability support;
- carry no “temporary” unbounded queues or UI-owned engine state;
- retire prototypes before stabilizing public APIs;
- require a fallback/failure policy for every new source and sink; and
- prioritize a reliable vertical path over a long list of half-integrated
  adapters.

## 15. Machine-checkable traceability

`feature-parity.md` is the reader-facing contract. Before Phase 1 closes,
`parity.toml` becomes the executable ledger. Each `IN-*`, `SW-*`, `GX-*`,
`AU-*`, `OR-*`, and `RC-*` ID must define:

```toml
id = "IN-013"
priority = "P1"
phase = 5
owner = "graphics-data-automation"
dependencies = ["gpu.browser-surface", "security.browser-sandbox"]
capability_profiles = ["windows-tier1", "macos-tier1", "linux-tier1"]
tests = ["browser-input-html5-audio", "browser-input-css", "browser-input-crash"]
```

`xtask parity check` fails when a Markdown ID lacks a record, a test ID is
unknown, a phase exit claims an incomplete dependency, or a release includes an
unwaived applicable row. Named proprietary integrations may be legally blocked
but cannot be marked parity-complete through a generic substitute.

Acceptance-test evidence progresses from `planned` to `present` to `verified`.
`present` records local test material and resolvable command targets without
claiming the feature contract is complete. Every feature must reference its
owned `accept-<feature-id>` record. A feature may be `verified` only when every
referenced acceptance test is also `verified`, has both local test paths and
resolvable test commands, and declares the test file resolved by each command.
