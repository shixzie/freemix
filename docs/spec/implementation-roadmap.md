# Step-by-step implementation roadmap

[Specification index](README.md)

## What's next

Audited against source, tests, and `parity.toml` at protocol 2.68 / schema 59
(2026-10-19); items refreshed through protocol 2.137 / schema 97 (2026-10-31,
at `efee1ebf`). Every parity row is still `planned` (or `blocked`); these are
the highest-value remaining feature items, ranked. The structural work the
2026-10-30 architecture review found (the daemon's size and its one-thread
frame loop) is ranked separately in [Structural work before more
breadth](#structural-work-before-more-breadth), and the first two items
there come before any new breadth below.

1. **Web gateway on the LAN: trust and reach (`RC-007`, `RC-014`, `RC-008`).**
   `freemixd serve` serves the pages over TLS on LAN addresses with
   Studio-driven device pairing (Protocol 2.71), and the windowed program
   daemon Studio supervises serves them too. Trust (Protocol 2.87) is a
   per-install local CA whose root phones install from `/freemix-ca.crt`
   (Studio shows it with a QR code, fingerprint, and iOS / Android steps;
   the leaf is reissued for new LAN addresses and the mDNS name), or the
   operator's own validated, hot-reloaded certificate, with the leaf
   fingerprint kept as the fallback check; the gateway advertises
   `_freemix._tcp` / `_https._tcp` over mDNS as `<name>.local`, and Studio
   lists every address with a QR code. New roots carry critical name
   constraints (private, CGNAT, link-local, and loopback ranges, `.local` /
   `localhost` / `home.arpa` / `.internal`, and the gateway's own names
   when the root was made), so a stolen root cannot mint a certificate for
   a public site; a root made earlier is kept unconstrained until the
   operator removes it. HTTPS answers for a DNS name the served certificate
   names (the `.local` name, a `--web-host`) carry a one-week
   `Strict-Transport-Security` for that host only (none for addresses,
   `localhost`, or plain HTTP). Still missing: proof on real phones (the
   pairing flow and the CA install are proven in headless Chromium and
   rustls clients only, and mDNS by unit-level records).
2. ~~**Native Preview monitor tap (`SW-001`, `SW-002`).**~~ Landed: the native
   daemon publishes a Preview `monitor_frame` tap (the incoming source during a
   transition) through nonblocking readbacks, and Studio shows it with a
   `PVW FEED` lamp. Timed transitions and title phases run on the native
   frame loop like a held T-bar (Protocol 2.86), so the tap streams through
   them too. The rows stay `planned` for their wider acceptance.
3. **Strip DSP on the live mix (`AU-005`), remaining parts.** Every input
   strip and Master now carry a persisted, undoable delay / polarity /
   high-pass / five-band EQ / gate / expander / compressor / limiter chain
   that the native daemon applies and meters (the sample-accurate delay
   stage since Protocol 2.94, schema 76), with BS.1770 / EBU R128 loudness,
   the EBU Tech 3342 loudness range, and true peak per strip and on Master,
   and an operator reset per bus (Protocol 2.92), which the Web operator
   page now shows and resets (Protocol 2.94; see the item 3 boundary
   below), and whose chains a browser with `control_audio` edits from the
   Web operator page's Strip FX disclosures (no protocol change), with each
   strip's gain reduction shown there too (Protocol 2.103); the simulated
   daemon now renders a synthetic tone through the persisted strips and
   chains and meters it, and a counting global allocator now shows the
   native daemon's Master mix entry point allocates nothing once warmed up
   with every strip and Master stage on, so hardware acceptance remains.
4. **Finish user shortcuts (`RC-004`).** Project shortcuts now persist, travel
   on the wire, fire on the authority under per-action permissions, and are
   edited and dispatched in Studio (keys and OSC), with `.fmshortcuts`
   import/export from Studio and the CLI, Studio-local per-console key
   overrides, Web operator page buttons, and panel-scoped and chorded keys
   (Protocol 2.83, schema 70), dynamic input targets (the input on
   Preview, on Program, or selected on the firing console; Protocol 2.88,
   schema 73) with a selection from the Web pages, OSC, and the CLI and a
   run's bound targets in the cue status (Protocol 2.89), mouse-button
   keys and keys scoped to a pop-out window (Protocol 2.93, schema 75), and
   dynamic value targets (a gain step, a mute toggle, and a gain read from
   the firing control's OSC value; Protocol 2.99, schema 80), and chorded
   and panel- or window-scoped console key overrides in Studio's local
   preferences, and mouse buttons as either stroke of a chord (Protocol
   2.99, schema 80), and keys scoped to the Colour, Animation, and
   Designer tool windows, with a pop-out the platform embeds in the main
   window answering its panel's and window's keys (Protocol 2.105, schema
   81). Console key profiles (built-in FreeMix and vMix style, and up to
   eight of the operator's own, carried between consoles as `.fmkeys`
   files) now choose the console's switching, transition, overlay, replay
   mark, and cue keys (see the RC-004 boundary below). This closes
   the gaps this item named; the row stays planned until
   its acceptance evidence covers its Windows and macOS profiles (see the
   RC-004 boundary below). This is a P0 row.
5. **Finish the Media List (`IN-006`) and `OR-015`.** The Media List is now
   a real input end to end (engine, protocol, persistence, both daemons,
   Studio), and the native daemon prefetches each list's next, previous, and
   first items so Auto Next, Next, Previous, and First switch files
   gaplessly, Studio's list card shows each item's own picture, and each
   list has its own transition between items
   (cut, fade, dip to black, or wipe, with a length; Protocol 2.95, schema
   77) that both daemons run on Auto Next and Next, the native one
   compositing the two items; it still lacks Windows/macOS evidence. (The slideshow,
   `IN-009`, is now wired end to end; see its boundary below.)
6. **The remaining keyer refinements (`SW-008`).** Key/fill (luma or
   alpha), clip, choke, edge softness, despill, light wrap, and polygon
   garbage mattes now run natively, persist, and are edited in Studio (the
   matte's points dragged on the canvas), chroma keys measure a perceptual
   BT.709 Cb/Cr distance, and one-click auto key (Studio's Auto key button
   and eyedropper, `scene-layer-auto-key`) sets a whole chroma key from a
   picture of the input up to 640x360 that Studio asks the server for
   (Protocol 2.97), which the CLI's remote auto key now asks for too
   (keeping the layer's matte refinement unless given new options). What
   remains: key/fill is not tied to paired SDI/OMT fill and key inputs, and
   the matte's points animate as scene properties (Protocol 2.145, schema
   102) only while the layer already has a matte with them (animation never
   adds a point or changes the invert flag or feather).
7. **Source clock drift on real devices and other OSes (`AU-004`).** Every
   live source the native daemon ingests (V4L2/ALSA capture, network
   streams, system audio) runs its audio through a drift-correcting
   `LiveSourceClock` with pictures following it, and its drift, correction,
   ratio, backlog, and recovery counts now reach clients (Protocol 2.76
   `source_sync_status` and `diagnostics_response`), Studio (a `SYNC`
   health lamp and per-strip `SYNC` keys), `freemix-cli
   remote-diagnostics`, and (Protocol 2.101) the Web operator page's
   read-only Source sync section. Sources that deliver timed
   `AudioBlock`s instead (the `fm-io-api` contract: native capture
   adapters, browser inputs, WebRTC guests) now have the same loop through
   `fm-audio`'s `LiveBlockSource` adapter, with pictures following and
   the same telemetry, tested against jittered, gapped, bursty, ±200 ppm
   synthetic clocks, plus an ignored hours-long soak harness. There is
   still no real-device soak evidence, no daemon input realizes a block
   source yet, and the Windows/macOS live paths do not exist yet.
8. **Windows and macOS ports of the Linux-only live paths (`IN-001`, `AU-011`,
   `AU-001`, `IN-011`).** Live capture hot-plug, system audio inputs, and device
   listing are Linux-only, and screen/window capture runs only on Linux (X11,
   and Wayland through its portal in `PipeWire` builds) (its Windows `gdigrab` and macOS ScreenCaptureKit backends are
   compile-checked, never run); the roadmap requires the vertical slice on all Tier-1 OSes
   before more breadth.
9. **Finish titles that start at the take and the browser title editor
   (`GX-001`, `GX-006`).** Title timelines now start on the frame a title is
   taken to Program, an overlay, or Preview, replay deterministically, and
   persist (schema 68); `/titles` edits, takes, updates, and clears titles
   from a browser paired as Graphics. Protocol 2.90 (schema 74) adds
   per-field browser exposure (set in Studio's TITLES tab), server-rendered
   draft previews and an image picker with uploads in the page, live
   `title_phases`, and a read-only In / Hold / Out strip in Studio.
   Protocol 2.96 (schema 78) adds per-field revisions, so editors of
   different fields of one title never clobber each other while a stale
   edit of the same field is refused as a conflict; a live `title_state`
   after every exposure change, so newly exposed fields reach open pages
   (and hidden ones leave them) without a reconnect; tighter image uploads;
   and a native-daemon check of the browser flow. Protocol 2.104 keeps
   hidden values out of the cue lists, shortcuts, and scripts a browser is
   sent, and its edits keep the stored values. A browser's rendered
   preview now draws hidden fields at their template defaults, never their
   stored values. Still missing: `GX-006` awaits its capability-profile
   certification.

10. **Instant replay, end to end (`OR-012`).** The show persists replay
    settings (Protocol 2.106, schema 82), the engine owns marks, Capture and
    Last 5/10/20 events, and up to two Replay players, the native daemon
    records up to eight inputs into rolling segment files and plays an event's
    angle back through a Replay input, Studio creates Replay inputs and
    edits recording in a dockable Replay panel, and the CLI has local and
    remote `replay` and `replay-set` plus `remote-replay-do`, with argument
    validation for sources, retention, and the storage root. Segments now
    record each source's own audio, which a Replay input plays at 100%
    (slow motion stays silent). Each Replay input now carries a playlist
    of up to sixteen events with their own angle and speed (Protocol 2.119),
    played back to back into the same player, in Studio, the CLI, and on the
    wire. Two Replay inputs can now be linked for A/B playback (Protocol
    2.124): cueing, playing, pausing, cue-in, and stepping one moves both at
    the same instant while each keeps its own angle, and their playlists
    advance only together. A player now shuttles at any rate from -16x to
    +16x in thousandths of real time, forwards or backwards, pausing at its
    event's ends, with the rate and shuttling state on the wire (Protocol
    2.131) and `remote-replay-do shuttle`; a player also jogs frame by frame
    (Protocol 2.114) and a finished replay returns Program to the live
    source (Protocol 2.116). Studio's Replay panel has 2x shuttle buttons and
    console keys play or pause the Replay player on Preview (else Program),
    step it a frame each way, and shuttle it at 2x each way, next to the
    marks and capture keys, and the Key profiles window binds a controller's
    transport (channel 1 notes 36–43) to the replay marks, capture,
    play/pause, stepping, and shuttling in one click. A Replay take now uses
    the show's configured transition — a cut or a fade of its own length
    (Protocol 2.137, schema 97), set in Studio's Replay panel and the CLI.
    Remaining: the
    recorder takes a source's last
    decoded picture rather than a genlocked one, the
    full typed two-profile replay controller surface (the console's MIDI
    transport now has a second profile for the second Replay input), and
    certification are absent; Studio's
    Replay panel and the CLI record up to eight inputs and render their angles
    in an adaptive grid (Protocol 2.127), now led by the input and its linked
    A/B partner as channel tiles; storage health reaches Studio and the CLI (Protocol 2.125,
    now with the drive's SMART health, Protocol 2.139; see the
    Phase 7 boundary).
11. **Highlight reels, end to end (`OR-013`).** The show persists up to eight
    reels of up to 64 clips (Protocol 2.107, schema 83), the engine compiles
    them against captured events, protects the segments they read, and
    resolves exact segment frames, the native daemon encodes a reel to an
    H.264/AAC MP4 with progress and cancellation, Studio edits and exports
    them in the Replay panel, and the CLI has local and remote reel commands.
    Exports are now remuxed to faststart MP4s and start only with room on
    the destination's disk, and a clip dissolves in from the one before for
    up to 600 frames (Protocol 2.111, schema 84), and exports carry each
    real-time clip's recorded audio (crossfaded through a dissolve; slow
    motion is silent), with an optional music bed under it, ducked while
    clip audio plays and faded at the end (Protocol 2.113, schema 86).
    Exports whose clips read a segment that is still open are queued and
    start once the recorder closes it (Protocol 2.123), holding their media
    against retention meanwhile; a recording restart fails them with a
    reason. Remaining: nothing is certified (see the Phase 7 boundary).
12. **OMT on the network, end to end (Phase 8 items 1-4).** `fm-io-omt` is
    FreeMix's own OMT 1.0 implementation: the native daemon reads an
    `omt://` network stream input (Protocol 2.121) and a stream destination
    with `stream_protocol=omt` publishes the Program pair (Protocol 2.122),
    with `_omt._tcp` DNS-SD advertisement and browsing. A receiver now
    skips a frame whose payload is corrupt while its declared length is
    intact, so one bad frame never ends the stream; the skipped frames
    count as dropped. Studio browses `_omt._tcp` and lists the senders it
    finds in the Add Input network form when the URL is an `omt://` one, so
    a discovered source fills the URL instead of being typed. Remaining: no
    VMX-compressed frames, no alpha or tally round trip proven against
    another OMT implementation, and no hardware certification (see the
    Phase 8 boundary).
13. **Aux buses through the show (`AU-002`).** Master plus aux buses A-G
    are live (Protocol 2.118, schema 90), metered on the wire (Protocol
    2.120), edited in Studio's BUSES window and the CLI, and a project
    output can hear a bus instead of Master. Every aux send now carries its
    own level and pre- or post-fader tap through the project (schema 92),
    the wire (Protocol 2.130), the native mixer, the CLI (`--sends`), and
    Studio's BUSES window. Each bus now carries a persisted DSP chain that
    the native mixer processes after its fader, the snapshot reports it, and
    Studio's BUSES window opens the FX editor on it (Protocol 2.133); the
    Web operator page lists the seven buses with their level and mute and
    edits each chain in place. A project output that hears an aux bus can
    now leave one input out of its feed (mix-minus, Protocol 2.134, schema
    94), chosen in Studio's BUSES window and the CLI, and the native mixer
    re-sums the feed without that strip exactly as the bus with the send
    disabled, chain processing included. Each bus can now play on its own
    local audio output device (Protocol 2.135, schema 95), chosen in
    Studio's BUSES window and the CLI (`audio-aux-device`), with the
    device reopened on its own after a failure. Remaining: network
    destinations are not wired, and the legacy
    project audio bus graph is unchanged (see the Phase 3 `AU-002`
    boundary).
14. **Audio plugin chains (`AU-006`).** `fm-plugin-host-api` models an
    ordered chain of up to eight bypassable, latency-reporting plugins, the
    native daemon renders the `freemix.reference.*` processors on input
    strips and Master (Protocol 2.128), the CLI sends a chain and edits a
    project's persisted chain offline, and Studio's FX window reads and
    edits it (Protocol 2.129). A chain is now a
    padlocked, undoable project edit persisted since schema 91 (now 92) for every strip
    and Master, so it survives a save, a restart, and a journal replay.
    Studio edits each reference plugin's saved parameter (the trim's gain,
    the soft clipper's threshold, and the polarity inverter's fixed action),
    and a Master plugin stage that panics is quarantined: the mixer keeps the
    summed Program bus and the next render is clean. A native daemon now
    scans `<bundle>/plugins/<id>/plugin.ini` manifests and lists their ids
    (with the built-in reference processors) in `audio_plugin_status`, so
    Studio shows what is installed. Remaining: parameters for plugins
    beyond the reference set, and a real sandboxed plugin in the chain
    (see the Phase 8 boundary).
15. **MIDI and controller surfaces (`RC-005`).** Studio reads ALSA raw MIDI
    ports on Linux, `CoreMIDI` on macOS, and `WinMM` on Windows, and binds
    notes, controls, and NRPN/RPN parameters to console actions, with LED
    and motor-fader feedback through the router. On Linux it also reads
    X-keys-style HID button panels from `/dev/input`, binds each button by
    Learn, and saves the bindings with the key profile.
    Remaining: no MIDI 2.0 or GPIO (see
    the Phase 5 boundary).

Audio-only media files (`IN-005`, `IN-003`), ranked third in this audit, have
since landed: WAV/MP3/AAC/FLAC/Ogg music beds and stings play on both daemons
(see the `IN-003` boundary).

Smaller follow-ups found by the audit: the Colour window now asks the
server for its target's source picture when no monitor shows it ungraded
(`SW-019`, see its boundary below). Headphone output device enumeration has
since landed (`AU-001`, Protocol 2.102): Studio picks the monitor bus's device
from the server's ALSA outputs and a failed or unplugged device is reopened
on its own; macOS and Windows still list and open no output device.
`fm-observability`'s `MetricRegistry` and `AlertEngine` now run in
both daemons with CPU and GPU-proxy utilization, default alert rules, and a
`health_status` view in Studio and `remote-diagnostics` (`RC-015`), and
operators acknowledge and silence alerts (Protocol 2.109) from Studio, the
CLI, and the Web operator page, which shows the daemon's health too
(Protocol 2.110), and a show sets each rule's thresholds, hold, and severity or
switches it off (Protocol 2.142, schema 99). Native daemons now group commit the
commands queued at a command turn (see Native group commit below), but the
group's disk sync still runs in front of its frame.

## Structural work before more breadth

Found by the 2026-10-30 architecture review of `8497393` (907k lines of Rust,
73 crates, 5,978 tests). The authority model, the layered crate map that
`xtask deps check` enforces, and the deterministic simulated engine are the
parts to keep; the risk is concentrated in `freemixd`. Ranked by risk removed
per week of work; items 1 and 2 land before any new feature breadth.

1. **Realize native frames on a dedicated media thread.** `sessions.rs`'s
   pass loop polls every peer, device worker, and clock, then calls
   `tick_if_due` on the same thread, with the control service held in an
   `Rc<RefCell<_>>`. A slow peer write, a group commit's disk sync (noted
   under Native group commit above), a thumbnail pass, or a TLS handshake
   on that loop delays the next frame deadline, and product contract item 9
   (audio continuity when a client disconnects) depends on the loop never
   stalling. Split `NativeDaemon` into a realizer thread that owns the
   pacer, GPU context, and audio mixer, fed by a bounded channel of
   immutable per-frame plans (`NativeProjectPlan` already exists for this),
   returning `FrameResult`s, meters, and status records. The control loop
   stays the only thread that ticks `fm-control`. Start by measuring with
   the existing `pacing_trace` so the win is shown, not assumed. The
   spec's "dedicated media threads" decision then holds for compose,
   readback, and mix, not only for decoders, cameras, and recorder pipes.

   **Measurement (2026-10-30, 25 s run, `native_pacing`'s ignored
   measurement test with a recording, two HLS streams, and about 7.5 cuts/s
   from two sessions):** on an Apple M5 (Metal), after the first 2 s, 583
   frames, p50 Program-lateness 1.76 ms, p95 5.84 ms, p99 6.38 ms, max
   6.57 ms, and no frame a frame late (40 ms interval); command frames p50
   0.69 ms, idle frames p50 2.58 ms; cut round trip p50 32.75 ms, p95
   49.81 ms. The first 2 s (50 frames) had p95 5.35 ms. On a Linux sandbox
   (Vulkan through `llvmpipe`, software rasterization) the same run after
   the first 2 s had 575 frames, p50 2.45 ms, p95 333 ms, p99 984 ms, max
   1,204 ms, and 43 frames a frame or more late; the first 2 s were still
   p95 5.19 ms with no late frame. So the frame loop is not yet the
   bottleneck on an idle host, while a software adapter competing with the
   encoders for the CPU shows the kind of stall the split must remove; the
   split must be shown on a loaded or peer-heavy run rather than assumed
   from the idle numbers. The startup pacing gate passes on both hosts (the
   software adapter's first-frame pipeline compilation is exempt from its
   wall-clock bounds; the frame loop's own CPU time and startup stalls are
   still checked there).

   **First stage (landed).** The Preview composite and its monitor tap are
   deferred out of the frame: a due composite becomes a
   `native_realizer::NativePreviewJob` in a one-slot latest-wins queue, and
   after the pass's peer work `service_deferred_preview` renders it only when
   at least 2 ms remain before the next Program deadline; otherwise the job
   stays queued and a display that shows Preview keeps its last picture.
   This removes the composite from between Program and its deadline without
   a thread yet, and establishes the bounded latest-wins handoff the
   realizer thread will consume. The native display, Preview-tap,
   transition, and pacing suites stay green.

   **Second stage (landed).** The audio mixer now runs on its own thread:
   `native_audio_realizer::NativeAudioHandle` owns the `NativeMasterRuntime`
   (mixer, decode workers, plugin racks, meters, stinger playback) behind one
   bounded, ordered channel for pushed live audio, the pre-frame coverage
   wait, catalogue and Stinger-audio commands, and one outstanding render
   reply. A frame applies its strip and mixer updates to the plan, submits
   the audio job, composes video on the control thread, then collects the
   block, live/override audio, owned meters, replay angles' rendered audio,
   and counters. The control thread reads the mixer's retention and sink
   counters from a shared snapshot instead of touching the mixer. Native
   pacing, transitions, audio-only media, strip DSP, replay, and display
   suites stay green on the M5 and the startup pacing gate passes on the
   Linux `llvmpipe` sandbox. The frame and plan are cloned per job for now;
   moving them behind an `Arc` is part of the GPU/plan stage.

   **Third stage (landed).** The Program/Preview GPU compose now runs on a
   video realizer thread (`native_video_realizer::NativeVideoHandle`): a
   bounded job carries the engine frame, the plan, and the playback and
   stinger registries; the reply returns the registries with the Program
   texture or every project output. The control thread keeps ingesting
   sources, uploading grades, tapping monitors, and presenting displays on
   the shared `Arc<NativeMediaRuntime>` while the compose runs on the
   realizer thread; the compose is still collected synchronously, so
   overlapping it with peer work is the next stage, together with moving
   ingestion, grades, and the monitor/display consumers behind the same
   handoff. Native pacing, transitions, display, Preview-tap, audio-only,
   strip-DSP, replay, and media-file suites stay green on the M5 and the
   Linux startup pacing gate passes.

   **Fourth stage (landed).** The frame is now split into begin and finish:
   `begin_frame_if_due` applies source reports and playback coverage, ticks
   `fm-control`, applies the frame's plan updates, and submits the audio and
   video jobs before returning to the pass loop; the loop then serves peer
   reads/writes, thumbnails, and shutdown checks, and `finish_frame`
   collects both realizers and runs the frame's consumers. Only the
   realization span (submission to collect plus consumers) is traced, so a
   peer wait shows as overlap rather than frame-lateness. Media-list end
   triggers now run before the frame begins, since a command between the
   two would see the playback registries held by the compose. The native
   pacing, transition, display, Preview-tap, audio-only, strip-DSP, replay,
   media-file, and media-list suites stay green on the M5; the pacing gate
   and transitions pass on the Linux sandbox (transitions alone; the
   combined run starves lavapipe when two suites run at once). Source
   ingestion and grade uploads still run on the control thread, and the
   monitor/display/stream/recorder consumers run in `finish_frame` on the
   control thread; moving them, and moving `NativeMediaRuntime` ownership,
   is the remaining stage.

   **Fifth stage (landed).** Grade uploads and the Preview composite moved
   to the video realizer. The control thread still compiles grades
   (`GradeCache::at`) but no longer uploads them: the worker caches the
   prepared `NativeProjectGrades` by the engine's compiled revision and sets
   them on each job's plan, so Program and Preview share one prepared set.
   `refresh_grades` and the control-side `prepared_grades` are gone. The
   deferred Preview job now carries its frame, plan, and grades, is submitted
   to the video realizer at the end of the pass, and is collected at the
   start of the next pass before any command or ingestion
   (`poll_deferred_preview`), so its composite and monitor tap are fully off
   the Program deadline. The native pacing, transition, Preview-tap, display
   (macOS), strip-DSP, replay, audio-only, media-file, and media-list suites
   stay green; the Linux pacing gate passes. Source ingestion,
   monitor/display/stream/recorder consumers, and single-owner
   `NativeMediaRuntime` remain.

   **Sixth stage (landed).** Slideshow slide composition (IN-009) moved to
   the video realizer: the control thread builds the projected frame's slide
   list and the ahead decodes from the engine's statuses, and the worker owns
   the `NativeSlides` cache, composes each slide with the runtime before the
   Program render, and drops a removed input's slides through the same
   bounded channel. `present_slideshows` and the control-side `slides` field
   are gone. Native stills (slideshow), pacing, transitions, display,
   Preview-tap, strip-DSP, replay, audio-only, media-file, and media-list
   suites pass.

   **Seventh stage (landed).** Replay playback moved to the video realizer.
   `NativeReplay` keeps recording, session, and retention; a new
   `ReplayPlayers` owns the per-Replay-input decode players and their wait.
   The control thread sends the projected frame and deadline through
   `present_replay` before the frame's audio render; the worker presents each
   player's recorded picture into the playback registry and returns the
   recorded audio for the control thread to push to the audio realizer before
   it renders, plus the coverage answer. Native replay, stills (slideshow),
   pacing, transitions, display, Preview-tap, strip-DSP, audio-only,
   media-file, and media-list suites pass. `NativeSourcePlayback` is still
   passed to the worker per request rather than owned by it; making that
   ownership permanent is the next stage.

   **Eighth stage (landed).** `NativeSourcePlayback` (both the source
   playback registry and the stinger registry) is now owned permanently by
   the video realizer. Control no longer holds either: live source pictures
   from titles, documents, media files, network streams, system audio,
   screen capture, capture devices, cameras, and stills travel as `ingest`
   commands; the per-frame coverage wait, replay presentation, catalogue
   insertion/removal, retained-byte limits, stinger replacement, and the
   playback budget are request/response commands over the same bounded
   channel. Control keeps only a shared `NativeLatestFrames` handle for
   thumbnails and replay recording. Cameras ingest through the worker too.
   Native pacing, transitions, replay, stills, display, Preview-tap,
   strip-DSP, audio-only, media-file, and media-list suites pass on the M5;
   the Linux pacing gate and transitions pass. `NativeMediaRuntime` is still
   shared with the monitor tap and display presentation, which are the
   remaining consumers.

   **Split plan.** The bounded channel carries a `NativeFrameJob` (the
   engine's `FrameResult`, which is plain data, plus the plan deltas the
   media side applies) to a `NativeRealizer` thread that owns `runtime`
   (already an `Arc<NativeMediaRuntime>`), `project_plan`, `master`, the
   playback and stinger registries, the output textures, and the
   stream/recorder/monitor/display consumers that read those textures.
   It returns `NativeFrameOutcome` (meters and status records, the latest
   CPU pictures for replay, and typed failures) over a single-slot result
   channel. The control thread keeps `ControlService`, peers, commands,
   the journal, and the durable project; it sends a job after
   `engine.tick()` and collects the previous outcome while peers are
   serviced. `fm-control` stays on the control thread, as required. The
   thread is being introduced in stages; the first stage must keep every
   existing native test (`native_pacing`, `native_sessions`,
   `native_transitions`, recorders, streams, displays) green.
2. **Break `freemixd` into library crates.** `apps/freemixd/src` is 157k
   lines, `main.rs` 30k and `native_media.rs` 16k, with 1,178
   `unwrap`/`expect`/`panic` sites in `main.rs` alone; any daemon change
   rebuilds the whole daemon. Extract `fm-daemon-sessions` (pass loop,
   peers, web gateway), `fm-daemon-native` (native runtime),
   `fm-daemon-journal` (recovery and checkpoints), and an arguments crate,
   as mechanical moves with no behaviour change, verified by the 43 daemon
   test targets. Target: no file over 5k lines, `main.rs` under 1k.

   **Progress (2026-10-31).** The journal and durable document pages, the
   command-line arguments and web settings, the web gateway (with the web
   title bridge, status feed, and LiveLAN viewer server), the native media
   runtime, source clock sync, and overlay layout, the shared renderers
   (grades, stills, the CPU scene reference, title rendering, and the
   simulated monitor tap), and the daemon's statistics, CPU, disk, status,
   and recorder helpers are now the library crates `fm-daemon-journal`,
   `fm-daemon-arguments`, `fm-daemon-web`, `fm-daemon-native`,
   `fm-overlay-layout`, `fm-daemon-render`, `fm-daemon-statistics`, and
   `fm-daemon-status`. Inside the binary, `NativeDaemon` moved to
   `native_daemon.rs`; the macOS cameras, recorder, stream targets, source
   resolution, telemetry, monitor taps, audio meters, recovery, restore and
   sync, command window, serve loop, client path, lifecycle, native
   startup, and transport each have their own module, and the scheduler's
   I/O is `sessions/io.rs`. `main.rs` is now 1.0k lines (from 30k) and no
   file but the 12.7k-line `tests.rs` is over 5k. `xtask deps check`
   allows a service crate to depend on the simulation composition root,
   which the extracted daemon libraries need. What remains in `main.rs` is
   the composition root: the argument-to-run wiring, capabilities, and the
   control-plane handle.
3. **Burn down panics on the frame path.** `fm-daemon-render`,
   `fm-daemon-native`, and the `freemixd` binary deny
   `clippy::unwrap_used` and `clippy::expect_used` outside tests. The
   realizer, the native-gated stream, camera, and replay paths, the session
   scheduler's dispatch and outbound paths, and the daemon's state restores
   now return typed errors or clamp a value the model already bounds; the
   metric and alert tables name their static invariants in one documented
   expectation.
4. ~~**Tier the CI matrix and cache builds.**~~ Landed: `rust-toolchain.toml`
   pins 1.97.0 for local builds and CI, `Swatinem/rust-cache` caches each OS
   target, pull requests run the Linux workspace job only, and macOS and
   Windows run on merge, nightly, and manual dispatch. A scheduled `soak` job
   installs FFmpeg and Vulkan, runs the ignored `native_soak` diagnostic, and
   runs the process target; no per-test label mechanism was needed because the
   soak test is already `#[ignore]` and the process target is a separate file.
5. **Spike one zero-copy or hardware encode path.** Every native output is
   composed on wgpu, read back to a `CpuVideoFrame`, and piped as rawvideo
   to an FFmpeg child; there is no shared-texture encode and no hardware
   encoder, so Phase 0 item 1 was never proved. Spike one platform
   (VideoToolbox through `fm-io-macos`, or VA-API on Linux) and measure
   1080p60 with two outputs before and after; the CPU path stays the
   portable fallback.
6. ~~**Mark model-only crates and rows.**~~ Landed: every `parity.toml`
   feature carries `kind = "model" | "adapter"`, `xtask` validates it, and the
   generated matrix prints a Kind column plus a legend. Eight rows are `model`
   today because their implemented surface is one of the contract-only crates
   (`fm-browser-input` IN-013; `fm-ptz` IN-028; `fm-guest`/`fm-io-webrtc`
   IN-029, RC-001, RC-002, RC-003; `fm-vmix-compat` RC-009; `fm-social`
   GX-010); the rest carry `adapter`. `crates.md` names the same crates as
   contract-only. The repository has no separate site features page, so the
   generated capability matrix is the published surface.
7. ~~**Record the runtime decisions the spec still contradicts.**~~ Landed:
   `README.md`, `architecture.md` §7.1, `crates.md`, and
   `server-client-protocol.md` now record the synchronous `std`-thread
   runtime (no Tokio), the four served HTML pages (no Rust/Wasm client), the
   absence of `fm-ui-widgets`, and the single portable wgpu backend (no
   `fm-gpu-dx12`/`metal`/`vulkan` crates).
8. **Generate the hand-written codec pairs.** Each new field touches the
   model, the protocol DTO, the persistence encoder and decoder, and their
   tests. Keep the no-serde policy (it is paying off in build time and
   dependency surface) and add an `xtask codegen` that emits the
   persistence encoder/decoder pairs from the model, with the existing
   tests as the oracle.

Version compatibility (a migrating project schema and a protocol
compatibility window) stays outside this development roadmap, as section 1
states; Phase 9 item 8 is where it is decided before the first release.

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
6. Validate FFmpeg/GStreamer redistribution and codec patent posture. No
   licensed AV-over-IP, plugin, capture, or output SDK is required: OMT,
   the plugin ABI, and the device I/O contracts are in-tree and license-free.
   Vendor adapters (NDI, DeckLink, AJA) are optional and runtime-loaded.
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
events while an Operator controls the show, and both modes serve eight raw
peers and up to 16 Web peers at once (see the item 10 web tally boundary).
Native-media acceptance is not proven on this Mac. This is
prerequisite transport infrastructure only; AU-001 daemon and Studio meter
transport remain planned. It is not HTTP/WebSocket/service/media readiness or
RC-008 completion. Expired raw TCP sessions are reclaimed. Phase 1 also
contains an optional WebSocket control listener at `/v1/control` (plain
HTTP on loopback only; LAN addresses require TLS, see the item 10 web tally
boundary), gated by `FREEMIXD_WEB_TOKEN` or paired device tokens; it relays exact-current wire records through the
same bounded scheduler and admits at most 16 Web peers at once, each its own
control session (see the item 10 web tally boundary). This is transport
groundwork only (Protocol 2.66 later gives Web pages a bounded Master-meter and
status feed, see the item 10 web tally boundary), and RC-008 remains
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
The `freemixd` gateway's self-contained `/operator` page (see the `RC-014`
boundary) is the first browser surface for these controls; the remaining
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
IPC exists, and the Preview image is a second composite read back to the CPU
(see below), not a shared handle. A platform
bridge plus output color transform, bounded shared ring, sidecar IPC, and
encoded fallback are required before item 6 can claim presentation without CPU
readback.
Protocol 2.21 adds the encoded fallback only: a `monitor_frame` record carries
a per-tap, strictly sequenced Preview or Program image of at most 640x360 as a
std-only QOI stream split into at most 32 canonical base64url parts written as
one atomic group, published latest-only to active raw-TCP Studio peers with
`view_status`. The simulated daemon renders both realized taps from simulated
sources at 5 fps or immediately on a routing change (it now composes scenes,
titles, active overlays, grades, and animation poses, but not Fade to Black;
see `apps/freemixd/src/monitor_tap.rs`); the native daemon reads back both
Program and Preview at up to 10 fps each through nonblocking GPU readbacks
(`NativeContext::begin_readback`/`poll_readback` over the pooled staging
buffers), Preview being the display Preview composite (the incoming source
during a transition), half an interval out of phase with Program, paced
like display outputs when the loop is behind, skipped while its last readback
is in flight, and composed only
while a Studio peer is connected (`apps/freemixd/tests/native_preview_tap.rs`).
Studio shows `PGM FEED` and `PVW FEED` lamps. Studio letterboxes the latest
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
before clip-local Stinger audio and Fade-to-Black. Studio's mixer (Master and
`MON` strips) and, since Protocol 2.66, the web operator and tally pages show it.
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
below, including the PFL/AFL monitor bus. Linux capture-device audio, system
audio inputs (`AU-011`), and the headphone monitor output (`AU-001`) are
described in their boundaries. There is still no microphone automix,
device-clock correction, native EQ/gate/compressor/limiter, or acceptance
evidence. Local offline audio-bus send authoring and rename are
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
manual-control model; it still has no browser renderer or network runtime
(the gateway's `/operator` page, `RC-014`, offers automatic transitions and FTB
but no manual T-bar).
Missing local audio, stills, scene silence, and
configured simulated silence produce silence; unsupported simulated sine audio
is rejected. At decoded EOS, the daemon synthesizes continuing silence for
subsequent Master intervals. Media-file inputs now have operator-selectable
end actions and an end trigger (see the `IN-003` boundary); other decoded
sources still pad with silence at EOS.

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
fixed-clock linear diagnostic resampler: there is no drift estimator, device
clock, channel conversion inside the decode synchronizer (pushed stream and
media audio is downmixed separately with the standard matrices), or strip DSP.
Persisted strip controls, the recorder, stream outputs, and the `AU-001`
headphone output now carry the Master mix out of the process.
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

Audio-only media files (`IN-005`, music beds and stings) play through the
same path with no protocol or schema change. A file without a video stream
(WAV, MP3, AAC/M4A, FLAC, Ogg Vorbis/Opus; `.opus` joins the importable
extensions) opens in `FilePlayer` when its configuration names a picture
size (`FilePlaybackConfig::audio_only_picture`; without one it is refused as
before). Its frame timeline is synthesized at the output rate from the
container duration (frame zero at the format start time,
`ceil(duration x rate)` frames), so play, seek, trim, rate, loop, stop,
hold, end trigger, track selection, and sample-exact audio placement behave
as for video, and no video window is ever decoded. Its picture is one
shared transparent-black frame (straight alpha, sRGB): taken to Program on
its own it shows black, and as a layer or overlay it is absent; an attached
cover picture is not shown. The native daemon sizes that picture to the
project, ingests it once, and pushes the audio through the input's strip to
the Master like any clip, so the strip's mix mode decides whether a bed is
heard off Program; the simulated daemon plays it over its synthetic source.
Both daemons thumbnail an input whose file name says audio
(`fm_model::is_audio_file_name`) as an audio card: a waveform glyph lit up
to the playhead over a progress bar. Studio's import and source-replacement
help names audio files. Acceptance: `fm-codec-ffmpeg --test
audio_only_media` (WAV ramp sample placement, seek, loop, stop, end trigger,
and WAV/MP3/M4A/FLAC/Ogg fixtures generated with lavfi `sine`), the
simulated playlist end-trigger test airing a WAV sting
(`freemixd --test media_playback`), and `freemixd --test
native_audio_only_media` (a WAV bed in the project and an MP3 sting imported
live from a host path: synthesized source, black on Program, tone on Master
while playing, loop, stop, and end). Gaps: a duration ffprobe estimates
(raw MP3/AAC without an index) is only as exact as that estimate; growing
audio-only files are refused; Studio's own input tile cannot tell an
audio-only media input from a video one (the wire carries no source kind),
so it shows the server's audio card rather than its system-audio meter
placeholder; and multichannel audio-only devices beyond system audio remain
open, so `IN-005` stays planned.

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
live (capture devices when live capture is unavailable, titles, colour,
alias, and Program/Preview feed
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
and Studio hover-audit coverage. Linux capture inputs are added, removed,
replaced, and closed live with hot-plug (see `IN-001`), but macOS camera
capture is still fixed at startup, title inputs cannot be added live natively,
device and network sources report their state through capture and stream
status rather than input availability, there is no relink-on-offline
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
This diagnostic path is superseded by the persisted, two-output, cross-platform
`OR-011` display outputs described below; physical scanout and visible pixels
are still not observed, real unplug/replug and induced surface loss are
policy-tested rather than hardware-induced, and HDR is absent. `fm-record` provides a private
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
while the final `FREEMIXD_RECORDER` record remains the shutdown result. Live
realized state now arrives as `recorder_status` (Protocol 2.66, with closed
failure codes since 2.67), and low disk space raises alerts (item 9); there is
still no automatic restart or fault-tolerant recording.
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
bridge, hardware encoder, or cross-platform native evidence (disk preflight
and low-space alerts, and a second recorder slot, are described below). Multi-file `fm-record` repair is necessarily
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
compositor feedback, and physical scanout. Live `stream_status` and
`recorder_status` records, disk-space alerts, and Studio's health strip now
exist, but there is no general live telemetry protocol,
component-dimensioned metric registry, CPU/GPU utilization, network
instrumentation, cross-platform native evidence, or QR/audio-loop latency
fixture. The raw TCP heartbeat acknowledgement is not service readiness,
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
Preview composite now runs after the pass's peer work in the slack before the
next Program deadline (a one-slot latest-wins job, at most one frame late)
rather than between Program and that deadline, though it still runs on the
realization thread until the media-thread split lands; and no multi-monitor
hardware, Windows, or macOS evidence has been recorded. `OR-011` remains
planned.

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
permission, focus, non-repeat, modifier, and manual-control gates allow the action. Studio's fixed key map
(`fm-ui-egui` `keymap.rs`) now also has F5–F8 favourites and Ctrl+Shift chords
for windows, recording, streams, and cue arm/disarm/stop/reset, with tests
that every binding is unique and reachable in its context. There is still no
binding editor, persistence, MIDI/controller runtime, or user-defined
shortcut acceptance evidence. When configured, native Studio also accepts the
bounded loopback OSC action contract, including zero-argument manual commit and
cancel and automatic AlphaFade, Slide, Zoom, and Wipe actions, with the same
current view, permission, and manual-transition gates, and the Cues
panel's `/freemix/cue/...` keys (see `RC-017`);
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
wholesale. That template layer remains library-only groundwork.

User shortcuts themselves are project state (schema 62, Protocol 2.72). A
project holds up to 64 shortcuts, each with an id, a name, an enabled flag, an
optional key (a key name with Ctrl/Cmd, Shift, and Alt), an optional OSC name
(`/freemix/user/<name>`), and one action: 1 to 16 cue actions run together on
one frame without waits (Program taken at most once, at most 512 bytes of
text), a cue list operation (GO of the standby or a named cue, Stop, Back,
Skip, Reset), or a project script run. Timed sequences are cues or scripts,
so shortcuts add no third clock. Keys and OSC names are unique within the
project; references to inputs, lists, and scripts are checked when fired, not
held against the project. `shortcut_set`/`shortcut_remove` are padlocked,
undoable project edits; `shortcut_fire` is a Transition-class operator control
whose authority also requires the class of every action it performs (for a GO,
the chain it arms; for a script, every verb), all or nothing, and a disabled or
unknown shortcut is refused with a typed reason. Studio's Edit ▸ Shortcuts…
window lists and edits them with "Set key" capture, flags a key the console
already uses (a table a unit test keeps equal to the key map) or another
shortcut's key or OSC name, and lists them in the keyboard reference. A key
fires under the console keys' gates (no dialog, menu, keyboard-owning panel,
or text field; key repeat never re-fires), Studio's loopback OSC listener
answers `/freemix/user/<name>`, and a fire the session cannot send is
announced in a toast like a server refusal. Keys are stored in the project, so
a show carries its keys to every console.

Since then: a shortcut set travels apart from the project as a strict
`.fmshortcuts` file (`fm-preset-file`, the project schema, actions in the
protocol's action notation), exported and imported from Edit ▸ Shortcuts…
and from the CLI (`shortcuts`, `shortcut-export`, `shortcut-import`, each
also `remote-`); an import replaces shortcuts by name, adds the rest under
fresh ids, skips identical ones, and refuses wholesale (nothing sent) a set
that would clash, swap keys, take a console key, or overflow the project,
ordering its `shortcut_set` edits so every step is valid. An operator can
give a project shortcut another key, or no key, on this console only: the
override lives in Studio's preferences (`shortcut.key.<project>.<shortcut>`),
never in the project, wins over another shortcut's project key here, and
is shown as "(this console)" in the list and the keyboard reference. The
Web operator page shows a button per project shortcut and fires it with
`shortcut_fire`, disabled (with the reason) when the session lacks the
permission of any action it runs or the shortcut is disabled, mirroring the
authority's check. Protocol 2.83 / schema 70 add chords (a second stroke
after the first, Ctrl+K then 1, which may be a key the console uses alone)
and keys scoped to one console panel (answering only while that panel,
the last clicked, has the focus); keys clash only where strokes and scopes
overlap. Protocol 2.88 / schema 73 add dynamic input targets: a shortcut or
cue action names a fixed input or the role `preview`, `program`, or
`selected` (`fm_types::InputTarget`). The role resolves on the authority when
fired: a shortcut fire binds Preview and Program as they stand before its
actions; a cue GO binds once for its whole run (kept in the list's run state,
so a step after a wait or an auto-follow acts on the inputs bound at GO), and
an arm binds when armed. "Selected" is the input selected on the firing
console (the input tile last clicked, right-clicked, or reached by keyboard in
Studio, marked by an accent bar and named in the tile's hover), sent as the
fire's `selected` binding; a fire whose cue needs a selection it lacks is
refused whole. The authority journals the Preview and Program it bound
(`bound_preview`, `bound_program`) beside the selection, so a replay acts on
the same inputs. Studio's shortcut and cue action editors offer the roles in
their input pickers (only "selected" for a take, none for title field edits,
whose field list needs a fixed title), with English and Spanish hover help,
and the standby preview reads a role as the input a GO would bind.
Protocol 2.89 puts a run's bound targets (and an armed GO's) in the wire cue
status, shown under Studio's cue list ("Bound at GO: …") and by `cue-lists`,
and lets the other surfaces select: the Web operator page selects an input
with its "Selected input" tiles (accent bar, English and Spanish hover help)
and sends it with every shortcut fire, holding a shortcut on the selected
input while none is selected; the tally page sends its own input with GO and
Arm GO; the CLI's `go`, `cue-arm`, and new `shortcut-fire` take
`--selected <input>`; and Studio's OSC takes an optional `i` input number on
`/freemix/user/<name>` and `/freemix/cue/go` (a second `i` on
`/freemix/cue/arm`), else Studio's own selection.
Protocol 2.93 / schema 75 add mouse bindings and pop-out window scopes: a
key may name the middle, back, or forward mouse button
(`MouseMiddle`, `MouseBack`, `MouseForward`) with Ctrl/Cmd, Shift, and Alt,
a single stroke never part of a chord (lifted in 2.99), and may be scoped to a panel's
pop-out window (`<panel>-window`), answering only while that native window
has the system's focus. Buttons clash with keys and chords exactly as keys
do, and a window scope overlaps the whole console and its own panel but no
other panel or window. Studio's Set key and Set console key capture a
button; the "Answers in" picker lists each panel's window (English and
Spanish, with hover help); each popped-out panel's native window now fires
the project shortcuts answering in it (unscoped, its panel's, and its
window's), and the main window never fires a window-scoped key.
Protocol 2.99 / schema 80 add dynamic value targets (`fm_types::CueGain`,
`fm_types::CueMute`): a gain action sets a level, moves the strip's gain by
a nonzero step from where it stands when the action runs (clamped to the
strip's range), or, in a shortcut only, follows the firing control's value
(`fm_types::FireValue`, 0 to 1000 per mille, mapped linearly in dB between
the action's two ends); a mute action may toggle the strip as it stands.
`shortcut_fire` carries the control's optional `value`, journaled as
sent; a fire whose actions read it without one is refused whole (field
`value`), and a cue list naming one is refused (a GO carries no value).
Studio's action editors offer the modes with English and Spanish hover
help, its OSC listener reads an `f` argument (0.0 to 1.0, after the
optional selected input) on `/freemix/user/<name>`, a key or Fire now of a
shortcut reading the control is held with its reason, the Web operator
page holds its button, and the CLI's `shortcut-fire` takes `--value`.
Studio's console key overrides (its local preferences) may now be chords,
mouse buttons, and scoped to a panel or a pop-out window like project
keys, clashing where strokes and scopes overlap; a preference line writes
`Ctrl+K, 1 @cues-window`, and the Shortcuts window's "This console" rows
add a second console key and an "Answers here in" picker with hover help.
A mouse button may now be either stroke of a chord (Protocol 2.99, schema
80; `.fmshortcuts` and console keys too), clashing as any chord does;
Studio captures a button as either stroke, and a named button press starts
or ends a waiting chord as a key press does.
Protocol 2.105 / schema 81 add tool window scopes: a key may be scoped to
the Colour window (`color-window`), the keyframe Animation editor
(`animation-window`), or the scene Designer (`designer-window`), each
overlapping only itself and the whole console; model validation, the
wire, project durability, `.fmshortcuts` files, console key preference
lines, and the CLI listing carry them. Studio's "Answers in" and "Answers
here in" pickers list them with English and Spanish hover help. A tool
window over the console (floating, docked, or embedded) takes the focus on
a press inside it, and the main window then fires its window's keys and
unscoped ones but no panel's; while the tool's own keys hold the keyboard
it reads the project's keys itself after its own, and in its own native
window it reads them there. A popped-out panel the platform embeds in the
main window takes the focus the same way and answers its panel's and its
window's keys. With this, every scope, chord, target, template, conflict,
and import/export behavior the RC-004 row names is implemented and tested
on Linux; RC-004 stays planned until its acceptance evidence covers the
Windows and macOS profiles it lists.

Console key profiles (RC-004, no protocol or schema change): the console's
own switching, transition, and cue keys come in profiles, chosen in
Preferences › Keyboard and saved with the console's preferences
(`keymap.*` lines in `studio.conf`, never sent to the daemon). Two are
built in. **FreeMix** keeps the keys the console always had (`1`–`8`
Preview, `C` Cut, `F` Fade, `B` Fade to Black, `F5`–`F8` the favourite
transitions, `G` the cue GO). **vMix style** follows the layout vMix
operators commonly give vMix's shortcut functions: `1`–`9` `PreviewInput`,
`Ctrl+1`–`9` `ActiveInput` (Preview then Cut, so the input goes straight
to Program), `Alt+1`–`9` `QuickPlay` (Preview then the Fade), `Space`
`Cut`, `Enter` `Fade`, `Ctrl+B` `FadeToBlack`, and `Ctrl+F1`–`F4`
`Transition1`–`4`, `F9`–`F12` `OverlayInput1`–`4` (the Preview input on
the overlay channel, or off while it is on), and `I`, `O`, and `E` the
replay in mark, out mark, and capture (vMix itself ships no fixed key
map; FreeMix leaves the overlay and replay actions without a key, so no
project shortcut loses its key). The Key profiles
window copies any profile into one of up to eight of the operator's own
and gives each action another key or none ("Set key" takes the next
press; a console key outside the profiles, such as the T-bar keys, the
cue step keys, the window and output chords, help, and undo, is refused,
as is another action's key), with English and Spanish hover help. Any
profile exports to a `.fmkeys` text file (its name, base, and changed
keys, at most 64 KiB) that another console imports as one of its own,
skipping keys it cannot use. Space
and Enter press a focused control while one has keyboard focus. The
keyboard reference lists the active profile's keys and names the profile,
every key hint on the console (Cut, Fade, FTB, the favourites, the input
tiles, the cue GO) names the active profile's key, and a project shortcut
may take any key the active profile leaves free but none it uses. Tested
by unit tests of the profiles, their preference lines, and the console's
key dispatch under the vMix style profile (overlay toggles and replay
marks included), a Studio preferences round
trip, a `.fmkeys` file round trip, and hover audit scenes of the Key
profiles window.

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
the persisted stack into compositor descriptors in order. Live protocol
mutation (`scene_layer_effects_set`, Protocol 2.43), Studio's EFFECTS
inspector, native GPU effect execution, and the native grade pass (`SW-019`)
have since landed, and a deinterlace layer effect (blend fields, or keep the
top or bottom field; Protocol 2.117) runs on both paths. Remaining gaps: no
Web effect surface and no motion-adaptive deinterlace. The
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
transport-free semantic controls. Device clocks, device-level drift
correction and channel-mapping realization, Windows/macOS device audio, and
hardware acceptance remain (Studio and web meters, and Linux device and
system audio, now exist). `fm-audio` separately provides the `AU-005`
reference DSP chain: exact polarity, an RBJ high-pass plus up to four peaking EQ
bands, a soft-knee compressor with makeup gain, an instant-attack ceiling
limiter, a hold/attack/release downward gate, and a peak/RMS window meter,
composed per channel by a preallocated `StripDsp` that processes planar
`AudioBlock`s in place without steady-state allocation and rejects mismatched
formats or non-finite samples before mutating state.

Current implementation boundary for item 3 strip DSP on the live mix
(`AU-005`; polarity, the expander, and loudness since Protocol 2.82 and
schema 69, the delay since Protocol 2.94 and schema 76): every input strip
and the Master bus carry an ordered chain
(`fm_types::strip_dsp::StripDsp`): a delay (0 to 1 s in microseconds,
ahead of every other stage and after a strip's sync delay), a polarity
invert, high-pass (12
dB/octave), a five-band parametric EQ (each band low shelf, peak, or high
shelf), a downward gate (threshold, range, attack, hold, release), a
downward expander (threshold, ratio, attack, release, range), a soft-knee
compressor (threshold, ratio, attack, release, knee, makeup), and a ceiling
limiter, each stage switchable, plus a chain bypass. Values are integers
(Hz, millidB, thousandths, microseconds) with an exact 143-character code. The project
persists them (`"dsp"` on each strip and `"master_audio_dsp"`, `null` for the
default chain, strictly validated); engine edits are undoable, journaled,
padlock-guarded project edits, and a knob drag's gesture token folds the
whole drag into one undo step. `fm-audio`'s `LiveStripDsp` runs a chain in
32-sample sub-blocks with no allocation after its first non-default chain:
biquads are retuned without resetting their history, parameters glide over
about 20 ms, and stage switches and the bypass crossfade over 10 ms (the
polarity invert ramps its sign through zero over the same 10 ms). The
delay is exact to the nearest sample: its history (one second plus one
maximum render block per channel) is allocated once, by the first chain
that enables it; enabling it crossfades into the history, which starts
silent, a new time crossfades between the two read positions over 10 ms,
and a delay that is skipped (off or bypassed) drops its history. A render
checkpoint saves only the delay's cursors, since the spare block of
history means a rejected render never overwrote what a restored cursor
reads. The
`MasterMixer` processes each strip's chain before routing (after the strip
delay) and the Master chain on the summed Program bus before clipping and
the Master meters; the transactional mixer pair copies DSP state without
allocating. `fm-audio`'s `LoudnessMeter` is an ITU-R BS.1770-4 / EBU R128
meter (K-weighting derived for any rate, surround weights, the LFE
excluded): momentary (400 ms), short-term (3 s), gated integrated loudness
kept in a fixed 0.05 LU histogram, the EBU Tech 3342 loudness range (3 s
short-term values every 100 ms, the -70 LUFS absolute and -20 LU relative
gates, the 10th to 95th percentiles) in a second such histogram, and a
4x-oversampled true peak per interval and since metering began or was
reset, all without allocating after construction. An operator reset
(`reset_measurement`, Protocol 2.92) restarts integrated loudness, the
range, and the maximum true peak while momentary and short-term loudness
keep running; only gating blocks and short-term windows that begin after
the reset count. `tests/loudness.rs` synthesizes EBU Tech 3341 cases 1 to
5, a surround-weight case, inter-sample true peaks, EBU Tech 3342 cases 1
to 4, and the reset. The mixer feeds one
meter per strip with the strip's post-fader contribution (silence while it
is not audible) and the daemon a Master meter with Program as it leaves,
after Fade to Black. Stinger clip-local audio now joins Program as a
`MasterBusInsert` ahead of the Master chain, so it is processed, clipped,
and metered with the strips. `freemixd --native-media` restores the chains, applies each
realized `audio_strip_dsp` edit to its ordinary and clip-local stinger mixers
(the Master chain to the Program bus only), saves them, and reports
gate/expander/compressor/limiter gain reduction in `audio_meters`
`dynamics` and loudness (with the loudness range since Protocol 2.92) in
`audio_meters` `loudness`. `audio_loudness_reset` (Protocol 2.92) is a
live, journaled audio command that changes no show state: the engine
validates the target and hands the native media plane a
`LoudnessReset` on the command's frame, and a journal replay accepts it at
the same revision (`apps/freemixd/tests/native_strip_dsp.rs` proves an EQ
boost, a compressor, a gate, the Master limiter, bypass, reconnect, undo,
a one-second strip delay, and a per-bus loudness reset on a running
daemon; `tests/process.rs` and
`fm-control`'s `loudness_reset_journal` prove the replay). Studio's `FX` key on each strip and on Master opens a
floating FX window with the EQ response curve (draggable band points and
high-pass corner, wheel for Q), the polarity invert, the delay and its
time in milliseconds, per-band and per-stage
controls, live gain-reduction meters, the chain's momentary, short-term,
integrated, loudness-range, and true-peak readings with a `Reset meter`
key (Protocol 2.92), presets (flat, voice, podcast, music, master glue),
and copy/paste between strips; drags send at most every 80 ms under one
gesture token and the worker coalesces queued sets of one drag. The CLI has
`audio-strip-dsp` / `remote-audio-strip-dsp` with presets or a chain code,
and `audio-loudness-reset` / `remote-audio-loudness-reset`.
Each mixer strip and Master
show a loudness readout under their keys (short-term per strip, integrated
on Master; amber past -1 dBTP) whose hover lists every value.
The Web operator page (Protocol 2.94) shows Program's momentary,
short-term, integrated, loudness-range, and true-peak readings with a
`Reset loudness` key and each audio strip's short-term loudness with a
`Reset` key, from the Web `audio_meters` records, which keep the loudness
of Master and at most 32 strips. Its Strip FX section edits every chain
(Master and each input) from a browser whose session has `control_audio`
(the Audio and Operator roles), with the existing `audio_strip_dsp`
command: one disclosure per chain with bypass, polarity, a preset menu
(Studio's five presets), Reset chain, and each stage's switch and sliders
(delay, high-pass, the five EQ bands with their kinds, gate, expander,
compressor, limiter), every control with English and Spanish hover help
and disabled with the reason when the session lacks the permission. The
page carries a JavaScript port of the chain codec that the daemon's tests
run under Node.js against codes the Rust codec renders (every preset,
extremes, and refused codes). Each set names the page's revision, one in
flight per chain: a slider drag sends at most ten sets a second under one
gesture token (one undo step) plus one on release; a set refused as stale
is resent once only while the chain is still the one it was based on, and
otherwise the page shows the server's chain and says why
(`tests/web_tally.rs`). Since Protocol 2.103 the Web `audio_meters`
records also keep the gain reduction of Master and at most 32 strips,
only for chains with a running gate, expander, compressor, or limiter
(a bypassed chain or one with every dynamics stage off sends none), and
each FX disclosure shows its switched-on dynamics stages' gain reduction
as a small `GR` bar (full at 24 dB) with the value in dB and English and
Spanish hover help, dimmed when the meters are stale (`web_feed` unit
tests; the page's parser and bar model run under Node.js against
records the Rust codec renders in `tests/web_tally.rs`). The simulated
daemon now renders a deterministic synthetic tone per input through the
persisted strip states, groups, solo, and the strip and Master chains, feeds
the same Master, strip, dynamics, and loudness meters as the native daemon,
and applies a loudness reset; meter demand never advances the engine
(`tests/sim_strip_dsp.rs`). An allocation-counting real-time test
(`fm-audio`'s `tests/realtime_allocations.rs`, through `stats_alloc`'s
counting wrapper of the system allocator, so the workspace keeps
forbidding `unsafe`) renders ten seconds of three strips with every stage
on (delay, polarity, high-pass, five EQ bands, gate, expander, compressor,
limiter), per-input sample delay, the Master chain, a Master bus insert,
the monitor bus, every meter, and per-strip and Program loudness through
`mix_planar_timed_into_with_monitor`, the native daemon's entry point,
and counts no allocation, reallocation, or free after a two-second warm
up. Remaining for `AU-005`: hardware acceptance.

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
certification, Windows adapters, macOS audio-device daemon/Master realization,
and screen/window/application-audio capture (Linux V4L2/ALSA capture with
hot-plug and Linux system audio inputs have since landed; see `IN-001` and
`AU-011`). Items 1 and 2, plus `IN-001`,
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

Update (2026-10-20): `SourceSync` is wired into every live source of the
native daemon. `fm-audio`'s `LiveSourceClock` wraps it for live delivery: the
connection's audio is pushed contiguously with its arrival time, the PI loop
(`DriftControlConfig::live`: ±1000 ppm, 0.01 Hz critically damped, 100 ppm/s
slew) observes a continuous-time one-pole (2 s) filtered fill instead of the
fill sampled before each pull, which aliased the delivery bursts into a slow
beat the loop chased with ratio swings of hundreds of ppm, and after any fill
jump (connect, re-prime, resync, overrun) it holds the ratio for 6 s and
re-anchors on the settled sub-burst offset so the drift estimate survives
pauses and reconnects. A read-side hard resync (declicked skip back to the
target) fires only above twice the target fill; underruns count separately.
The pictures follow the audio: the smoothed picture-queue latency is held
within ¾ frame of the audio latency by dropping one extra picture or
repeating one, which also holds for a capture card whose pictures are on the
host clock while its audio is on the card's crystal. `fm-codec-ffmpeg`'s
`NetworkReceiver` (network streams, V4L2/ALSA capture through `fm-io-linux`,
and system audio) replaces its frame-locked audio queue with it, pulls audio
while not live so the backlog stays primed, no longer asks `ffmpeg` to pad and
trim device audio against host timestamps (`aresample` without `async`), and
gives the picture buffer at least three frames above its target; `freemixd`
logs a `FREEMIXD_SOURCE_SYNC` record per source when it first runs, on every
resync or underrun, and once a minute. Measured in the sample domain
(`fm-audio --test live_source_drift`, bursty 256-sample delivery): ±500 ppm
sources over one simulated hour settle within 5 ppm after 98–100 s, end
within 0.002 ppm of the true skew, and keep the fill within 1.4× the target
with no resync or underrun, 44 picture drops (fast) or repeats (slow) against
45 skewed frames, and at most 0.6 frame of A/V error; a −300 → +700 ppm step
settles in 89 s then 111 s; a 5 s pause re-primes once and keeps the estimate
within 2.6 ppm; a +5000 ppm source (beyond range) resyncs 22 times in 10 min,
declicked, with no overrun. Remaining for `AU-004`: a wire event and Studio
view of the telemetry, an `AudioBlock`/`MediaTiming` adapter for non-receiver
sources, real-device soak evidence, and the Windows/macOS live paths.

Update (Protocol 2.76): the telemetry is on the wire. Native `freemixd`
sends raw Studio peers a lossy, latest-wins `source_sync_status` record at
most once a second while any live source has a running loop (and one empty
record after the last stops): per source its kind, loop state, both sample
rates, estimated drift and applied correction in milli-ppm (hence the
resampling ratio), backlog and target in microseconds, and the resync,
underrun, overrun, picture drop/repeat, and connection counts.
`diagnostics_response` carries the same samples on request. `fm-client`
validates and retains the latest record; Studio shows a `SYNC` lamp in the
health strip and a `SYNC` key on each live input's mixer strip (English and
Spanish hover help, covered by the hover audit), and `freemix-cli
remote-diagnostics` prints one `SourceSync(...)` line per source. The
native network stream test now waits for its stream's running loop in
`source_sync_status` and in diagnostics.

Update (Protocol 2.101): Web pages that may view status take
`source_sync_status` in their lossy feed (latest-wins, paced like a
status), and the operator page shows a read-only Source sync section
while any live source runs a loop: each source's name, state, drift and
correction in ppm, resampling ratio, buffer against its target, resyncs /
underruns / overruns, and pictures dropped / repeated, with English and
Spanish hover help on every figure (connection counts in the pictures
help) and a stale note once records stop for 3.5 s. Remaining for
`AU-004`: an `AudioBlock`/`MediaTiming` adapter for non-receiver sources,
real-device soak evidence, and the Windows/macOS live paths; `AU-004`
remains planned.

Update (2026-10-21, no wire change): the `AudioBlock`/`MediaTiming`
adapter exists. An audit of the native daemon found every live
audio-bearing input it realizes (V4L2/ALSA capture, network streams,
system audio, X11/Windows/macOS screen capture with system or application
audio, and Wayland portal capture) already behind an `ffmpeg`
`NetworkReceiver` and its `LiveSourceClock`; the sources that bypass it
are those behind the `fm-io-api` `MediaSource` contract, which deliver
timed `AudioBlock`s (`fm-io-macos`'s `MacosAudioSource`, browser inputs,
WebRTC guests) and which no daemon input realizes yet. `fm-audio`'s
`LiveBlockSource<P>` wraps a `LiveSourceClock` for them (a new
`LiveSourceClock::push_at` pushes at an explicit source sample index):
a block whose sequence number follows the previous one is contiguous
whatever its timestamp says, so host-stamped jitter and a host clock's
slow divergence from the device crystal never become gaps; a skipped
sequence (lost blocks) is placed by the time since the previous block's
end, so the converter conceals a short gap with silence, trims a short
overlap, or splices a larger jump on its declick rules; a
`DISCONTINUITY` flag or a backwards sequence splices; a new clock domain
restarts the connection and forgets the drift; blocks of another rate or
channel count are refused. Pictures queue beside the audio (bounded, the
oldest dropping on overflow), go live once they hold the latency, are
shown as the clock's picture action says, and stall after two late
frames. Its `live_telemetry()` is the receivers' `LiveSourceTelemetry`,
so a block source reaches `FREEMIXD_SOURCE_SYNC`, `source_sync_status`,
diagnostics, Studio's `SYNC` lamps, the Web Source sync section, and
`remote-diagnostics` with the same `wire_sample` (a `freemixd` unit test
checks that it reports exactly as a receiver fed the same audio).

Measured in the sample domain (`fm-audio --test live_block_drift`, 8 kHz
mono 256-sample blocks stamped with 2 ms of error, 25 fps): ±200 ppm
devices with 2 ms of delivery jitter settle (minute-averaged correction
within 5 ppm) after 110–123 s and keep the fill within 1.3× the target
with no resync, underrun, overrun, gap, or splice, and the pictures within
1.3 frames of the audio; with no jitter the estimate ends within 0.04 ppm.
The loop reads drift from the fill (1 ppm is 0.1 ms over 100 s), so
random arrival jitter moves the estimate by a few ppm per millisecond
(about ±20 ppm at 12 ms, network-like, still bounded and fault-free) —
far below audibility. Two runs of three lost blocks each are concealed to
within 4 samples with no click or resync; 70 ms delivery stalls followed
by bursts are absorbed without an underrun (the correction swings up to
~100 ppm for tens of seconds, and the estimate returns within 0.3 ppm); a
300 ms stall underruns once, declicked, and re-primes keeping its
estimate; flagged and restarted sequences splice. Every test bounds every
output sample-to-sample step by a continuous tone's plus one declick ramp,
and a 48 kHz stereo run is bit-identical twice. The ignored
`long_drift_soak` (`FM_AU004_SOAK_HOURS`, default 6) wanders the device
within ±200 ppm with jitter, lost blocks, and bursts every few minutes;
12 simulated hours run in about 40 s with no resync, underrun, overrun,
or click, the fill under 1.2× the target, quiet minutes tracking within
3 ppm, and A/V within 1.6 frames. Remaining for `AU-004`: a daemon input
realized through a block source, real-device soak evidence, and the
Windows/macOS live paths; `AU-004` remains planned.


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
checkpoint/restart tests preserve masks. Native keys, effect stacks, and live
scene edits have since landed (see `IN-020`). Feathered or non-rectangular
masks, per-output scene realization/routing, live addition of newly nested
scenes, and cross-platform or hardware certification remain. Item 4
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
independently gated. While a timed transition or scheduled take is still being
realized (its desired buses accepted, the realized ones not yet moved; Protocol
2.86), Studio disables Cut, the transitions, Preview selection, and a T-bar
start the same way, with the reason on hover; the daemon still refuses a Cut
mid transition rather than completing it, since a Cut swaps the desired buses
the transition already swapped. Reconnect handling
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
The `freemix-web` model stays transport-free; the gateway's `/operator` page
(`RC-014`) offers the action in a browser. Studio presents an
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
duration when Ready and authorized. The `freemix-web` model stays
transport-free; the gateway's `/operator` page offers the action in a browser.
Studio presents a Slide
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
when Ready and authorized. The `freemix-web` model stays transport-free; the
gateway's `/operator` page offers the action in a browser. Studio presents a
Zoom action
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
model, and the gateway's `/operator` page toggles FTB in a browser. Studio presents a dedicated native
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
leaving a state no checkpoint could persist. Fade-to-Black evidence on the
`OR-011` fullscreen outputs, Windows/Linux recording evidence, and complete
profile acceptance remain absent, so Phase
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
rotations, masks, safe areas, and multiview. Remaining gaps: a source crop
is validated against a 16384-pixel source bound rather than the actual
decoded source size, so an oversized crop reaches the compositor and shows
black beyond the picture; no model, protocol, or UI fit setting (Studio
monitor wells now follow the frame's shape); and the native GPU path is
untested. `SW-015` remains planned.


Current implementation boundary for IN-008 and IN-010: `fm-video::test_signal` generates 75%, 100%, and EBU bars plus a bars/ramp/PLUGE layout with exact BT.709/BT.601 narrow-range 8/10-bit Y'CbCr and full-range RGBA codes, and a static generator renders solid colour or bars once per setting instead of per frame. `fm-playback::image_sequence` discovers numbered sequences (padding, duplicate detection, reject/hold/skip gap policies), maps time or output frames to sequence frames exactly with Once/Loop/Hold ends, and bounds a prefetch cache; `fm-codec-image::ImageSequenceSource` decodes bounded PNG/JPEG frames of one size, preloads and pins a sequence, and suggests the first opaque frame as a stinger cut point. Still pictures as inputs (decode, ICC, fit/fill, reload) now have their own boundary under item 5 slideshows below; GPU-side generation, RP 219 raster geometry, daemon/CLI/model wiring, asynchronous decode, alpha video, and Studio controls are not implemented, so `IN-008` and `IN-010` remain planned.

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
policy, or meter ballistics. A counting global allocator
(`tests/bus_matrix_allocations.rs`) shows a warmed-up render of four strips
sending pre- and post-fader to every bus, two processed buses, and three
destinations (two mix-minus) allocates nothing. The live `MasterMixer` now
renders aux buses A–G beside Master (`mix_planar_timed_into_with_aux`):
each strip sends to a bus at its own level, pre-fader (unity, no balance,
pan, or transition envelope) or post-fader (exactly its Program
contribution); strip and group mute and follow-video cut every send while
solo never does; each bus has its own ramped fader, mute, clipping, and
peak/RMS meters (`tests/aux_buses.rs`), and the Master allocation test
renders every bus. The live show uses them (Protocol 2.118, schema 90; per-send
level and tap since Protocol 2.130, schema 92): every strip carries Master
and A–G bus assignment as a live control (vMix style `M`/`A`–`G` keys in
Studio, `--buses` in the CLI), each aux send has its own milli-dB level and
pre- or post-fader tap (`--sends M,A@-6000p` in the CLI and the BUSES
window's Send levels section in Studio), the project keeps seven named aux
buses with live faders and mutes, and a project output can hear an aux bus
instead of Master, so its recordings, streams, and LiveLAN take that bus
with the output's own Fade-to-Black trajectory. A snapshot carries each
strip's enable mask and announces one `audio_strip_send_state` record per
strip whose sends go beyond it, so the 64 KiB line budget holds; a native
test proves a −6 dB pre-fader send ignores a −6 dB strip fader while the
post-fader send hears both. The daemon
renders the buses beside Master every frame and a native test proves an
output on bus B hears an aux-only strip while Program stays silent.
Studio's BUSES window edits every bus's name, fader, and mute,
shows its level meter, and chooses which bus each project output hears,
with English and Spanish help; the CLI does the same from the shell. The
native daemon meters all seven buses on the wire (Protocol 2.120): each
lossy `audio_meters` record carries every bus's channels, and the
simulated daemon reports none. The live `MasterMixer` now gives each aux bus an optional DSP chain,
processed after its fader and mute and before its meters, and the chain
reaches clients in the snapshot's `audio_aux_buses` (Protocol 2.133):
Studio's BUSES window opens the FX editor on a bus (`audio_strip_dsp`
target `aux:a`..`aux:g`, a drag folding into one undo step), loads a
preset chain, and the CLI sets it. An output that hears an aux bus can
leave one input out of its feed (AU-002 mix-minus, Protocol 2.134, schema
94): the snapshot's output records name the excluded input, Studio's
BUSES window and the CLI choose it, and the native daemon renders a
separate feed per such output in the same mixer pass, re-summing the bus
without the strip — bit-identical to the bus with that send disabled,
through its DSP chain — and gives that feed the output's own Fade-to-Black
trajectory. Each bus also names an optional local output device (Protocol
2.135, schema 95): the native daemon's per-bus output slot plays the
rendered bus after its fader, mute, and chain on `ffmpeg`'s ALSA output,
with the same lazy open, retry, and hot-plug behavior as the monitor bus,
and Studio's BUSES window and the CLI choose it. A bus can also be published
as an OMT audio sender (Protocol 2.138, schema 98): the native daemon binds
one `fm_io_omt::Sender` per published bus on `0.0.0.0:9300`–`9306` (bus A
through G), feeds it the rendered bus's samples at every audio-meter
interval with the interval's start sample as the OMT timestamp, and starts
or stops the sender from the live mixer update; Studio's BUSES window has
the OMT toggle and the CLI has `audio-aux-omt <A-G> on|off`. Remaining gaps:
network destinations other than OMT, and
the legacy
project audio bus graph and each output's `audio_source` are unchanged.
`AU-002` remains planned.

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
Remaining gaps: the daemon uses the standard downmix matrices only for pushed media and
stream audio, and nothing reaches persisted strips, protocol, or Studio; RTMP's
FLV carries only mono or stereo, while SRT and HLS carry 7.1 and discrete
layouts as their exact ffmpeg names; there is no device ancillary-data
extraction or hardware evidence, while 5.1 folds to stereo with either the
BS.775 Lo/Ro matrix or the Dolby Surround Lt/Rt matrix (the 90° surround
phase shift is not modelled), and to mono as the composite of the stereo
fold. Project
settings now accept every channel layout the model holds, 1 through 16.
`AU-003` remains planned.

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

Protocol 2.68 (no schema change) lists the server's capture devices so the
operator picks one. `system_audio_devices_request` is answered by
`system_audio_devices`: whether the server captures system audio, why an
audio system could not be listed, and each device's audio system, name,
description, channel count, default mark, and monitor mark, exactly and
strictly bounded (48 devices). On a native Linux daemon
`fm_io_linux::system_audio` runs `pactl list sources` and `pactl info` (C
locale, the capture child's environment, killed after 2 s) and reads
`/proc/asound/cards` and `/proc/asound/pcm` (capture PCMs as
`plughw:<card id>,<device>`); the daemon lists on a worker thread off the
frame loop, reuses a listing for 2 s, and keeps one pending request per peer.
Simulated daemons and other platforms answer that they do not capture system
audio. Studio's system audio form, in Add input and now in Replace source (a
system audio input opens there at its device), asks when it opens and on
Refresh list, and offers Default device (`@DEFAULT_MONITOR@`, or ALSA's
`default`) and the listed devices of the chosen audio system, with the name
field kept for a device the list does not show; the help is in English and
Spanish and the form is in the hover audit. The CLI adds
`remote-system-audio-devices`. Tests cover the wire form and its refusals,
the `pactl` and procfs parsers and a fake `pactl` that answers, fails, and
hangs, the daemon's listing worker with a fake enumerator (off the loop,
reuse, deferral), the picker state, and the CLI output.

Remaining gaps:
- devices are listed only when asked (Studio's form asks again every five
  seconds while it stays open, but nothing reports an appearance or removal
  outside it), and no real `PulseAudio`/`PipeWire` server listing has been
  exercised;
- there is no edit command beyond replacing the source;
- capture is decoded to the project's channel layout rather than fixed
  stereo, but there is no per-input channel selection or delay;
- there is no Windows (WASAPI loopback) or macOS capture, and no
  per-application capture;
- there is no virtual audio device publishing Program or buses;
- the simulated daemon reports every system audio input as idle;
- there is no evidence from a real PulseAudio or PipeWire server.

`AU-011` remains planned.

Current implementation boundary for screen and window capture inputs
(`IN-011`): Protocol 2.78 (schema 67) adds an input kind that shows one
screen of the server's desktop or one application window. A window is
followed by a rule (application plus exact title, a title it contains, or
any title) with an id hint, so it is found again after it closes and
reopens. The input carries crop insets, a pointer toggle, a frame rate cap,
and a stall slate (hold the last picture or show black); a show holds at
most 8. The add is undoable, the source is replaceable, and the kind is
persisted exactly. `screen_capture_targets_request` lists the server's
screens and windows (bounded, exact, strict), and every input's state
reaches clients as `screen_capture_status` (with `target_gone` and
`unsupported`) and the snapshot's `screen_capture_inputs`.

The native daemon lists the desktop off the frame loop twice a second and
follows each target: a resize, move, or crop restarts the capture on the new
rectangle; a closed window reports `target_gone` and shows the slate; a
matching window that appears is captured again. Capture reuses the
supervised `FFmpeg` device receiver (jitter buffer, 500–3000 ms restart
backoff that never gives up, stall slates), letterboxed into the program
picture.

- **Linux X11** (working and tested): `x11rb` lists RandR monitors and
  client windows (EWMH client lists, or the root's mapped children with
  `WM_CLASS` when no window manager runs) and `ffmpeg -f x11grab` captures a
  window or a screen region, clipped to the visible desktop. Tests run a
  private `Xvfb`: the listing, crop, letterboxed pixels, resize detection,
  closure to `target_gone`, a reopened window found by an any-title rule
  with a stale id, a whole-screen capture, an unreachable display, and a
  native daemon process showing a window on Program, losing it to the black
  slate, and following a new window.
- **Linux Wayland** (Protocol 2.85, schema 72; working and tested against
  a headless desktop in the default `--features native-media` build, which
  needs nothing `PipeWire` to build): the xdg-desktop-portal `ScreenCast`
  interface over D-Bus in pure Rust (`zbus`): `CreateSession`,
  `SelectSources` (a monitor for a screen target, a window for a window
  target, the embedded cursor mode or hidden, `persist_mode` 2 with the
  input's restore token), `Start`, and `OpenPipeWireRemote`, whose stream is
  read as shared-memory `BGRx`/`RGBx`/`xRGB`/`xBGR` pictures (DMA-BUF is not
  offered), cropped after capture and letterboxed into the program picture
  at the capture rate. The stream is read through libpipewire
  (`libpipewire-0.3.so.0`) loaded at run time by `fm-io-pipewire`, the
  workspace's one narrow `unsafe` FFI crate: a minimal hand-written binding
  of exactly the exported functions the stream needs (`pw_init`, a
  `pw_thread_loop`, `pw_context_connect_fd` with the portal's descriptor,
  and a `pw_stream` with mapped shared-memory buffers), whose callbacks copy
  out the newest picture of each pass, and a small safe SPA pod encoder and
  decoder for the `EnumFormat` offered, the `Buffers` asked for, and the
  `Format` negotiated, checked byte for byte against libspa's own builder.
  A server where the library cannot be loaded reports a Wayland session
  `unsupported` with "install PipeWire" advice (under XWayland it lists X11
  windows as before). System audio is the `PulseAudio`/`PipeWire` default
  output's monitor exactly as on X11 (it is keyed by the audio system, not
  the window system), read by an audio-only receiver that starts when the
  share first goes live, with its own drift loop and the input's audio
  state; application audio is `unsupported` with its reason: a portal
  grant names neither the process nor the application of the picked
  window, and the restore data that might is private to each desktop's
  portal backend, so no playback stream can be matched reliably. Nothing is listed (backend
  `portal`): the desktop's own dialog picks the screen or window, so a
  window target is the picked rule (no application, no title, any title),
  and the input is `awaiting_selection` while the dialog waits on the
  server's screen. A cancelled or refused dialog, or sharing stopped on the
  desktop, is `denied` and is not asked again until a
  `screen_capture_pick_request` (Studio's tile menu: Pick target again),
  which forgets the restore token and asks anew; a shared window that
  closes is `target_gone` and asked for again with the token. Each grant's
  restore token is kept beside its input in `project.json`
  (`screen_capture_restore_tokens`, never on the wire), so a reopened
  project shares the same target without the dialog where the desktop
  allows it. Tests: the capture worker and conversion with a scripted
  portal; the D-Bus conversation against a fake portal on a private
  `dbus-daemon` (options, the dialog wait, a cancel, a missing source type,
  a dismissed dialog, the session's `Closed` signal); and, with
  `FM_WAYLAND_E2E=1`, a real share of a headless sway desktop through
  xdg-desktop-portal-wlr and PipeWire by a default build with libpipewire
  only on the loader's path (the picture's pixels, the crop, and a second
  share restored by its token); without the library the same desktop
  reports `unsupported` with the install advice and its portal is never
  asked. `fm-io-pipewire`'s tests cover the pods against libspa's bytes,
  truncated and corrupt pods, the C structure layouts, a missing or
  unusable library, and, against real libpipewire, a dead connection and
  a missing node ending the stream with the daemon's reason. The daemon's
  portal sources hear a test tone as a live share's system audio, and
  report application audio `unsupported`; a scripted run of the default
  build's `freemixd` on the headless desktop with a private PulseAudio
  playing a tone shows the share `live` with `audio_state=live` and a
  running drift loop, and the same run without libpipewire on its path
  shows `unsupported` with the install advice.
- **Windows** (compile-checked for `x86_64-pc-windows-msvc`, not run): a
  long-lived PowerShell child lists `Screen.AllScreens` and visible,
  non-minimized top-level windows (`EnumWindows`, process name, title,
  rectangle); `ffmpeg -f gdigrab -i desktop` captures the target's desktop
  rectangle (so a covering window is captured too). The record parser and the
  `gdigrab` arguments are tested on every platform.
- **macOS** (compile-checked with the stub `xcrun`, not run): `build.rs`
  compiles `ScreenCaptureHelper.swift` against ScreenCaptureKit; its `list`
  command prints the same records and its `capture` command writes
  fixed-size BGRA pictures (repeating the latest while nothing changes)
  into `ffmpeg -f rawvideo -i pipe:0` through the receiver's new feeder
  process. The feeder path is tested on Linux with `ffmpeg` standing in for
  the helper. Capture needs the Screen Recording permission.

Screen capture audio (Protocol 2.84, schema 71): each input names its
`audio` — `none`, `system` (the server's whole output), or `app` (a window
target only: the sound of the application owning the window, its process
and the processes it started). The sound feeds the input's own mixer strip
(AFV, meters, strip DSP) and is captured by the same capture child as the
picture, so picture and sound share one jitter buffer and the receiver's
drift loop (AU-004, reported as a `screen_capture` live source). The input's
status carries the audio state (`off`, `opening`, `live`, `no_stream` while
the application plays no stream yet, `unsupported` with the reason, such as
an OS version without per-application capture, and `failed`) and an
`audio_error`. The capture child decodes its audio to the project's channel
layout (up to sixteen channels), not fixed stereo.

- **Linux** (working and tested against a private PulseAudio 16 server):
  system audio is `ffmpeg -f pulse -i @DEFAULT_MONITOR@` as the capture
  child's second input. Application audio is a daemon-side tap: every half
  second `pactl list sink-inputs` lists the playback streams; each stream
  whose `application.process.id` is the window's `_NET_WM_PID` or one of its
  descendants (or, when the window system names no process, as on Wayland,
  whose application id, portal app id, or binary names the window's
  application) is recorded with `parec --monitor-stream`, which neither
  moves nor re-routes it, so the application stays audible. The streams are
  mixed behind 30 ms jitter buffers, paced on the host's monotonic clock
  (the one the picture is grabbed on), and served as 48 kHz stereo `f32le`
  on a Unix socket in an owner-only directory that the capture child reads.
  Audio is keyed only by process or application id, never by the picture
  backend. Tests: a tap follows only its application's tone (a second
  application playing at once is excluded), finds a stream a child process
  starts later, reports `no_stream` while silent, leaves both streams on
  their sink (the sink monitor still hears both), and reports a missing
  server; a native daemon process meters the window application's tone on
  its strip (silent while only another application plays), meters a
  screen's system audio, reports the drift loop, and returns to
  `no_stream` when the application stops.
- **Windows** (compile-checked, not run): a PowerShell helper with inline
  C# runs WASAPI shared-mode loopback on the default render endpoint
  (system) or process loopback with `INCLUDE_TARGET_PROCESS_TREE`
  (application, Windows 10 2004 or later; older builds report
  `unsupported`), served through the same tap on loopback TCP.
- **macOS** (compile-checked, not run): the `ScreenCaptureKit` helper's
  `audio` command (`capturesAudio`, `excludesCurrentProcessAudio`) for a
  display or one application (macOS 13 or later), served through the tap.

A simulated daemon answers a fixed simulated desktop and shows a synthetic
picture. Studio's Add input dialog has a Window / screen capture kind with a
refreshable picker grouped as Screens and Application windows (application
and title), the follow rule, crop, pointer, rate cap, and stall slate, with
English and Spanish help in the hover audit; on a Wayland server it offers
A screen or A window instead of the picker. The tile shows OPENING / PICK ON
SERVER / LIVE / TARGET GONE / REOPENING n / DENIED / UNSUPPORTED / FAILED
with its details, and its menu's Pick target again sends a
`screen_capture_pick_request`; the captured sound's state has its own badge
on the tile and on the input's mixer strip (AUDIO OPENING / AUDIO LIVE / NO
STREAM / AUDIO UNSUPPORTED / AUDIO FAILED, none while the audio is off; on
the strip beside the SYNC key), its hover giving the state, the reason, and
what it means. Replace source on a screen capture input opens the same form
at the input's current target, crop, pointer, rate cap, stall slate, and
audio, asks the server for its targets, and sends the edited capture as the
replacement source (audited in both languages). The CLI
adds `input-add-screen-capture`, `input-screen-capture-status`, their
`remote-` forms, and `remote-screen-capture-targets`.

Remaining gaps:
- capture audio: PipeWire's `pipewire-pulse` monitor-stream support and the
  Windows and macOS helpers are not run here; the tap's jitter buffer adds
  about 30–60 ms to application audio against the picture (the strip's
  delay can compensate), and the tap absorbs a stream clock's drift by
  dropping or padding at its buffer bounds rather than resampling;
- Wayland: no DMA-BUF import (the compositor copies into shared memory,
  and each picture is copied once more out of the stream's buffer), the
  cursor metadata mode is not drawn (a desktop without the embedded mode
  captures without the pointer, and says so), only one stream per input,
  no application audio on a portal capture (system audio only), and the
  headless test shares a screen through xdg-desktop-portal-wlr only (it
  cannot share windows); GNOME and KDE portals and window sharing are
  untested, and PipeWire releases other than 1.0 are not run (only
  exported functions and the stable leading structure fields are used);
- Windows and macOS paths are compile-checked only; no run on either OS, no
  Windows Graphics Capture (covered windows), and no signed macOS helper;
- X11 captures only on-screen pixels, so a covered window shows what covers
  it; minimized windows are not listed;
- coordinates are in points on macOS; HiDPI scaling is not applied.

`IN-011` remains planned.

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
white clip, choke or spread up to eight pixels, edge softness (a box mean of
the matte over up to eight pixels), and exact green or blue despill that only
lowers channels; and key/fill layers whose alpha comes from a same-size key
source's luma or alpha channel. The planner rejects conflicting keys and
refinements without a key. The native GPU path runs key/fill and every
refinement as a matte chain (pull, clip, choke, soften, then the key stage
with the mask and despill) and matches the CPU reference within two code
values (`fm-compositor` `tests/native_keys.rs`). Since Protocol 2.70 and
schema 61 each persisted layer key carries its refinement and a layer may
carry a key/fill key naming a non-Scene key input; the engine, both daemon
renderers, the CLI, and Studio's layer designer handle them, and a key/fill
layer whose key input draws no picture is not drawn. One-click auto key is a
client-side computation applied through the existing key edit, so it needs
no wire change: `fm_ui_model::auto_key` reads a layer input's picture
deterministically (integer-only, subsampled to at most 65,536 pixels),
finds the dominant green or blue backing from a chroma histogram (or seeds
from a 5x5 window under a picked point), gathers the backing by chroma
direction so an unevenly lit screen stays one cluster, and derives the key
colour (cluster median), similarity (95th-percentile distance plus 8),
smoothness, spill, and a green or blue despill whose strength follows how
much of the rest of the picture is backing-tinted. Studio's layer designer
shows the selected layer's input thumbnail in the KEY section with an Auto
key button and a Pick colour eyedropper; either applies one
`scene_layer_key_set` (one designer undo step, padlocked, undoable, and
journaled on the server) that keeps the layer's clip, choke, and edge
softness. `freemix-cli scene-layer-auto-key` samples a layer's simulated
input at the project size or a saved PPM, and `remote-scene-layer-auto-key`
sends the key derived from a saved PPM as one live edit. Since Protocol 2.97
and schema 79 every refinement also carries a light wrap (amount and a
1..=8 pixel width: each kept pixel near the layer's edge takes the mean
background of 25 taps around it, weighted by how transparent the layer is
there, after placement and before opacity, so the CPU reference composes
everything beneath the layer first and the native compositor splits its
pass at the layer to read a copy of the background) and an optional polygon
garbage matte (three to eight points normalized to `0..=1000` of the cropped
layer and not all on one line, so it always has an area, keeping the inside or the outside, tested exactly in half-pixel
integers shared by `fm_types::GarbageMatte` so CPU and GPU agree bit for
bit); both persist, travel in key edits and layouts, render in both daemons
and the CLI, and are edited in Studio's KEY section with hover help (the
matte's points as a numeric list and by dragging them on the canvas, each
edit one undo step). The chroma key distance is the larger of the BT.709
fixed-point Cb and Cr differences (on the CPU reference, natively, and in
both auto keys), and Studio's designer asks the server for the selected
layer's input picture at up to 640x360 (a `scene_preview_request` naming a
non-Scene input) and auto key samples it instead of the 160x90 thumbnail
when it has arrived. `remote-scene-layer-auto-key` without `--picture` asks
the daemon the same way (a `scene_preview_request` naming the layer's input,
fitted within 640x360) and keys the picture it renders, and without matte
options it keeps the layer's clip, choke, edge softness, light wrap, and
matte (`freemix-cli` e2e test against a fake daemon, checked by hand against
a simulated `freemixd`). Remaining gaps: the `fm-compositor` border/region
auto key remains a separate CPU
reference; key/fill is not tied to paired SDI or OMT fill and key inputs,
and a Scene cannot be a key input; garbage mattes feather up to 64 pixels
to each side (Protocol 2.115, schema 87: an 81-point integer grid per
pixel, clamped to the layer, so the native compositor matches the CPU
reference exactly, checked by `native_keys` on llvmpipe), and their points
animate as sixteen scene-layer properties (Protocol 2.145, schema 102: point
1 x through point 8 y, normalized `0..=1000`, overriding the static
coordinate; a track for a point the matte does not have, or one that would
flatten the polygon, keeps the static matte); light wrap on a tilted layer
matches the CPU reference only within the bilinear sampling difference.
`SW-008` remains planned.

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
destinations product contract. Output health is shown only in Studio's
compact panel and the web gateway's operator and tally pages (Protocol
2.66/2.67); there is no live decoder acceptance of a streamed broadcast against a
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
native output publishes MPEG-TS with `-mode caller -latency` under the
destination's own reconnect policy, and masks the passphrase in worker
diagnostics. On Unix the passphrase never reaches the child's command line
(world-readable in `/proc/<pid>/cmdline`): the child runs with
`-fpre /dev/stdin` and reads one `passphrase=...` line the streamer writes
into its stdin and closes; a child whose stdin cannot be written is killed
before it connects. On Windows it is still passed as `-passphrase`. Studio's destination dialog adds the
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
- on Windows the SRT passphrase, RTMP key, and SRT stream ID still reach
  the `ffmpeg` child's command line (on Unix the child reads them with
  `-fpre /dev/stdin`: the URL keeps only scheme and authority, the key
  travels as `rtmp_app`/`rtmp_playpath` split as `ffmpeg` splits the path,
  or as `streamid`, except an SRT stream ID holding `%` or `+`);
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
inputs never listen on one UDP port (`listen_port_in_use`). The listener's
port belongs to `libsrt` inside its `ffmpeg` child, so the receiver reports
`connecting` until the child answers an SRT induction handshake on it (the
listener answers that statelessly with a cookie), and only then
`listening`; a rendezvous reports `listening` as its child starts. The
child's `libsrt` listener closes its listening socket once it accepts a
caller, and a second child cannot take over the UDP port while the first
holds it (`SO_REUSEADDR`/`SO_REUSEPORT` would split or steal the live
session's datagrams), so the port stays closed from a session's end until
the next child binds it, about 0.1 s. That gap is not closed: an SRT
caller retransmits its handshake until its connect timeout (3 s by
default), so it delays a caller rather than refusing it (a loopback test
starts the next caller the moment the previous one leaves, once and
without retrying, and it goes live), and closing it would take a receiver-owned UDP relay that
demultiplexes SRT sessions by peer address without speaking SRT. The passphrase
is stored only in the project and is `***` on every other surface (events,
snapshot, status errors, CLI, logs). Verified over loopback with `ffmpeg`
SRT callers and listeners, encrypted with a matching and a wrong
passphrase, and a native `freemixd` receiving an encrypted caller. The
passphrase never reaches the `ffmpeg` child's command line: an encrypted SRT
child reads its input through the `concat` demuxer from a one-entry script
on its stdin (`-f concat -safe 0 -protocol_whitelist pipe,srt -i pipe:0`)
that carries the URL and every SRT option, and the loopback tests assert
that no receiver argv in `/proc` holds it.
Remaining gaps: SRT link statistics (RTT, loss, retransmits) are not
reported, since the `ffmpeg` CLI does not expose them (status keeps
bitrate, buffer, dropped and late frames, and reconnects); `fm-io-srt` is
still unused; no SRT listener or rendezvous output; tests skip without `libsrt` unless
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
native `freemixd` taking a push onto Program. The listening port is the
receiver's own: `ffmpeg -listen 1` accepts one connection and then closes
its listening socket, and a pre-spawned next child could not bind the port
the current one holds, so a child-owned port refused every publisher from
an accept (a session's end, or a refused wrong-key publisher) until the
next child bound it. The receiver instead binds the configured address once
for its whole life, reports `listening` from that bind (`connecting`
before it, and a port it cannot bind reconnects with the backoff policy),
and relays each accepted publisher byte for byte to its own child, an
`ffmpeg -listen 1` on a fresh loopback port. A relayed publisher must
deliver its first picture within the timeout plus a 5 s grace. Verified by
loopback tests in which a connection made the moment `listening` is
reported is accepted, and a publisher that pushes once, without retrying,
the moment a wrong-key publisher is refused (or while it is still being
refused) goes live. Wrong keys are throttled per peer address (an IPv6
peer by its `/64`): the third consecutive wrong key locks the address out
for 1 s, each further one doubles that up to 300 s, a locked-out
connection is closed at accept before any child exists, a publisher that
delivers a picture clears its address, and at most 1024 addresses are
remembered (each forgotten 10 min after its last failure or lockout).
Each lockout is logged as `FREEMIXD_NETWORK_LOCKOUT` (input, peer,
failures, `lockout_ms`) and named in the input's last error. The key never
reaches the `ffmpeg` child's command line: every RTMP child (and every SRT
child with a passphrase or stream ID) reads its URL and protocol options
through the `concat` demuxer from a one-entry script on its stdin (`-f
concat -safe 0 -protocol_whitelist pipe,rtmp,rtmps,tcp,tls -i pipe:0`), and
the loopback tests assert that no receiver argv in `/proc` holds it.
Remaining gaps: no `rtmps` listener (no TLS certificate handling); only one
publisher per input (a second publisher's
connection waits in the listen backlog until the first leaves); not
verified on Windows or macOS, so `IN-016` remains planned.

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
- Stream audio is decoded to the project's channel layout (up to sixteen
  channels), but one source's layout is still chosen by `ffmpeg`'s `-ac`
  downmix/upmix rather than mapped from the source labels.
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
without a directory listing or path traversal. That listener stays
loopback-only groundwork without TLS; LAN viewing, a viewer page, and egress
reporting are provided separately by LiveLAN (`OR-017`), and there is still
no per-rung persisted audio setting or codec choice. The ladder is
one destination's ABR encoder into one directory rather than independently
controlled destinations, so the five-destination bound still counts each
authored target once and a remote single-key or multi-key RTMP ladder is not
implemented. Studio and Web show the destination roster with realized
per-destination state, but no per-rung ladder view. `OR-007` and
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
existing worker FIFO as the slot-qualified protocol commands. Protocol 2.66
adds a lossy `recorder_status` record (unavailable, off, starting, recording,
or failed, with elapsed time and, from 2.67, a closed failure code); Studio and
the web pages show it, and Studio refuses Start on an unconfigured slot.

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
monitoring or bandwidth probe, and no GPU capability preflight beyond adapter
creation. Low space now stops a program recorder at the 512 MiB floor
(`storage:low_space`, reported as disk full) and a local HLS output whose
filesystem falls below the same floor, and each recorder slot reports
its output filesystem's free bytes in `recorder_status` (Protocol 2.140),
beside the local HLS destinations' free bytes in `stream_status`. `OR-001`'s space status is
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
be appended to the support bundle under its byte cap.

Both daemons now own one registry for the run and sample it once a second
from the session loop (`apps/freemixd/src/statistics.rs`): the control plane's
subscriber count and revision, and on a native daemon host frame lateness and
GPU pass time histograms, realized, late, and dropped frame counts with the
late share, audio sink, recorder, stream, monitor tap, camera, disk, and
failed-output counters and gauges under stable names. Each sample also counts
the started stream targets (active, connected, reconnecting) with their
bitrate and reported packet loss, aggregates the bounded source-sync records
(sources, degraded sources, maximum drift and backlog), and reports the
fullest recorder or stream queue's utilization. Each sample also
records `cpu.process_utilization`, the daemon process's user plus system time
from `/proc/self/stat` over wall time as a share of every logical CPU
(unavailable off Linux; child `FFmpeg` encoders are included when `/proc`
lists them), and
`gpu.utilization`, which is a proxy rather than a vendor counter: the GPU
time of the native compositor's timestamped fullscreen render passes over
wall time, so it omits uploads, readbacks, encoders, and other processes, is
a lower bound when timestamp samples drop, and is unavailable on the
simulated daemon and on adapters without timestamp queries. An `AlertEngine`
evaluates nine fixed default rules after every sample (`cpu_saturated`,
`disk_low`, `encoder_saturated`, `frame_late`, `frames_dropped`,
`gpu_saturated`, `output_failed`, `stream_reconnecting`, `sync_degraded`)
with hysteresis and hold times; each notification prints a
`FREEMIXD_ALERT` line, the support bundle carries the registry and rule
states under `## Statistics`, and `FREEMIXD_TELEMETRY` is unchanged. Protocol
2.100 sends raw Studio sessions a lossy `health_status` once a second and
adds the same sample to `diagnostics_response`: utilization permille, firing
alerts, and the 16 most recent fired, resolved, and reminder notifications.
Studio's health strip shows `CPU`, `GPU`, and alert lamps with hover help in
English and Spanish, and `freemix-cli remote-diagnostics` prints `Health`,
`Alert`, and `AlertNotification` lines. Protocol 2.109 adds operator
acknowledgement and silencing: a `health_alert_request` (needing
`transition`) acknowledges a firing alert or silences a rule for up to a
day, or ends a silence, and the daemon answers with a `health_status`
echoing the request; every sample carries each alert's acknowledgement end
and the silenced rules, the CLI's `remote-alert` sends the request and
`remote-diagnostics` prints `acknowledged_until_ms` and `AlertSilence`
lines (statistics unit test, `fm-protocol` health tests, the simulated
daemon's process test, and a CLI e2e test). Acknowledgements and silences
are runtime state, gone on restart. Studio's Output ▸ Health alerts window
lists every rule with its state (firing and how long it stays acknowledged,
silenced and for how long, or quiet) and offers Acknowledge 15 min, Silence
1 h, and Unsilence keys gated on transition authority, with English and
Spanish help (hover audit scenes for an operator and a viewer). Protocol
2.110 puts the same view on the Web: a page that may view status gets
`health_status` in its paced lossy feed, and the operator page's Health
section shows CPU and GPU use and every rule's state with Silence 1 h,
Acknowledge 15 min (while it fires), and Unsilence (while it is silenced)
buttons gated on `transition`, each sent as the page's own
`health_alert_request` and answered or refused by name (the page's parser
checked under Node.js against the Rust codec, and a gateway test where an
Operator page silences and unsilences a rule while a Viewer page is
refused). Protocol 2.112 (schema 85) lets a show set its own rules: an
undoable `alert_rule_set` project edit (needing `edit_project`) changes one
rule's trigger, clear threshold, hold, or severity or switches it off, the
project keeps only rules that differ from their defaults, the snapshot and an
`alert_rules_changed` event carry them, and both daemons restart only the
changed rules from their next statistics sample. Studio's Health alerts
window edits them under each rule in natural units, and `freemix-cli`
prints and changes them with `remote-alert-rules` and `remote-alert-rule`
(type, persistence, codec, engine undo, statistics, and process tests).
Remaining
gaps: input total, offline, and failed counts are sampled; there is no
hardware-encoder session sampling, and the simulated daemon has no network,
sync, or encoder readings (an unsupported reading stays unknown rather than
zero); a show changes each rule's thresholds, hold, severity, and switch but
not the reading it watches, and cannot add rules; the GPU reading is a
compositor-pass proxy. The
Web operator page now lists the daemon's recent health notifications
(firing, resolved, and reminder) with rule, severity, and age. `RC-015`
remains planned.


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
`recorder_status` now carries each slot's free bytes and estimated seconds
of recording left at its configured bitrate (Protocol 2.141), shown in
Studio's recording row. Remaining gaps: the fallback is refused with
fullscreen output; the daemon's own remaining-time thresholds are fixed; no
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
renders the console, the animation timeline, the colour panel, the Add input
capture form with its signal-lost tile, and the system audio form headlessly
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
`fm-engine` classifies commands into undoable project edits (input add,
remove, rename, and reorder; stinger and overlay settings; grades; title and
scene animations; scene layers; the playlist, cue lists, and scripts;
Fade-to-Black and display outputs; stream destinations; LiveLAN; network and
capture input settings) and live operations
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
logical outputs and title field values are not undoable engine edits yet
(stream destinations, display outputs, and title animations are), so `SW-017`
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
minification filtering, no GPU rasterization, no title designer, and no
animation/ticker integration with real data sources (project hosting and the
live field editor are described below). Item 1 therefore remains incomplete and `GX-001`
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
mode, tickers and scene clocks still start at frame zero rather than at the
take (keyframe animation plays from the take frame), and template field names are not localized, so
`GX-001` remains planned. Studio's TITLES tab shows the selected title's
read-only In / Hold / Out strip from its template animation, with a playhead
that follows the lossy `title_phases` record (Protocol 2.90) and runs on at
the project frame rate between records, and how long the title has been on
air or on Preview from its timeline start frame.

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
only, rendering is CPU-only, and there is no
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
animates, both daemons realize a phase on their frame loop after the
command's first frame (the simulated one at wall-clock pace), and
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
dialog); a preset picked with keys selected on several targets animates all
of them in one undo step.

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
saying so. Since Protocol 2.97 the window also asks the server for the
clean picture of a target no monitor shows ungraded (no wire change: a
`scene_preview_request` with `color-picture-` request ids, at most one in
flight, at most one per input every 3 s like the layer designer's key
picture, never while a grade drag runs or the designer's own preview
flies, and only over raw TCP with `EditProject`): an input target's own
ungraded picture (a Scene input's composed scene only while its own grade
is identity), and for a layer the picture of the input it draws (a Scene
input's composed, graded scene as is; any other input's ungraded picture
with that input's replicated grade applied locally, without a LUT the
window does not hold) or of an ungraded Scene input showing the scene it
draws. Up to 480x270 in the canvas shape, it feeds the same Corrected,
Original, Side-by-side, and wipe modes and scopes, captioned as the
server's picture (English and Spanish). Program, not an input, still
previews only its clean monitor frame, last held frame, or still.
Remaining gaps: the preview is the source picture rather than the layer
posed in its scene; the window's
before/after split and scopes are computed locally from the monitor
proxy frame or the server's reduced picture, not from the full-resolution
graded output; and native
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
error, and only choosing the device again reopens it (until Protocol
2.102, below). Program, recordings, streams, and the
monitor meters never see the headphone level. `audio_meters` reports the
device as off, playing, or failed. Studio's monitor strip adds a device
field (Enter plays, an empty field stops, Enter on a failed device retries),
the device state, a level entry, and a mute key, all with English and
Spanish hover help. `freemix-cli` adds `audio-monitor-device` and
`audio-monitor-level` and their `remote-` forms. `freemixd`'s
`native_monitor_output` test reads the PCM that reached a captured device
(unity, -6 dB, mute, undo and redo of the device), fails a missing ALSA
card through the real `FFmpeg`, and plays the real ALSA `null` device where
ALSA is available.

Protocol 2.102 lists the headphone output devices and reopens a lost one
(`AU-001`). An `audio_output_devices_request`, answered only to a session
that may edit the project, lists a native Linux daemon's ALSA outputs:
`default` (marked the default) and every playback PCM in `/proc/asound` as
`plughw:CARD=<card id>,DEV=<n>`, a name that survives the card moving to
another port or index, described as `<card>: <PCM>` with a USB card's
playback channel count; a missing `/proc/asound` is reported as the
listing's error. `freemixd` lists on a worker thread and reuses a listing
for 2 s; a simulated daemon answers a fixed three-device list for clients
and tests, and a native daemon off Linux answers unsupported (its
`fm-io-linux` lister and presence probe are portable and compile on every
platform, but nothing calls them there). A failed device is now retried on
its own: 1 s, then doubling to at most 30 s between attempts, starting over
after 10 s of play or when the device is chosen again; before each attempt
the daemon checks that its card and PCM are present and, while they are
not, only looks again every second, so a replugged headset plays within
about a second. The state stays `failed` (Studio's `PHONES FAILED`) until a
reopened device has played for 1 s. Studio's monitor strip replaces the
device field with a picker of the listed devices, meter-only, the chosen
device marked `not connected` when it is no longer listed, and `Type a
name…` (the old field, kept for names the list cannot show such as
`pulse`); it asks for the list when the picker opens and every 3 s while
shown, and a server without the list or a session that may not ask keeps
the field. Every new control has English and Spanish hover help, audited by
`check_monitor_output`. `freemix-cli remote-audio-output-devices` prints the
list. Tests cover the exact wire form and refusals, the ALSA parser and
presence probe against a fake `/proc/asound`, the listing worker, the
simulated list and its permission refusal through a real daemon, the
native daemon's answer, the back-off, unplugged, replugged, and probation
cases of the retry, and the CLI.

Remaining gaps: the device is opened and listed only through ALSA, so macOS
and Windows list nothing and report a chosen device failed; `PulseAudio`
sinks are not listed (ALSA's `pulse` PCM can be typed); channel counts are
known only for USB cards; hot-plug is found by polling, not udev events; the
monitor output carries the Master
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
freemixd serves that editor as `/titles` (Protocol 2.81): a self-contained
page that lists every title input with its air state (Program, overlay
channel, or Preview), edits text, number, colour, and image fields with the
server's bounds, shows the exact change list a preview fixes, and sends
`title_fields` (`require=off_air` to save, `on_air` to update live),
`title_take` on a chosen overlay channel, and `title_off_air`. A device
paired as Graphics (`view_status` + `edit_project`) edits; Operator takes
and clears; the static token's producer role does both. Commands
unanswered after eight seconds reconnect, and every reconnect resynchronizes
from the snapshot while keeping drafts; a stale draft shows the server's
value and needs Keep my edits or Discard. English and Spanish strings follow
Studio's catalog. Protocol 2.90 (schema 74) closes most of the remaining
gaps. Each template field carries a browser exposure flag (stored as
`"exposed": false` only when hidden), edited with a Web checkbox per field in
Studio's TITLES tab (a non-undoable project edit staged beside the template
like an animation edit). A Web session never receives a hidden field (not in
`title_state`, `title_changed`, or `title_exposure_changed`) and may not name
one or change exposure. The page shows the server's 320x180 render of the
title with its unsaved edits (`title_preview_request` with draft `fields`,
one request in flight, QOI decoded in the page); image fields get a picker
that lists the bundle's PNG and JPEG images and uploads new ones over
authenticated HTTPS endpoints (`/v1/assets/images`), which sniff the type,
cap the size at 8 MiB, decode-check the image, sanitize the name, and store it
inside the bundle under `assets/uploads/`; and lossy `title_phases` records
drive an In / Hold / Out line with an extrapolated progress bar. Protocol
2.96 (schema 78) closes three more. Each title field carries the field
revision that last changed its value (persisted as each field's
`"revision"` in `project.json`, so journal replay stays deterministic, and
sent as `title_state`'s `field_revisions`); an edit applies when its
`base_revision` is one the title has reached and none of the fields it sets
changed after it, so Studio and browsers that saw the same revision may edit
different fields, while a stale edit of a field someone else changed is
refused as `title_revision_conflict` naming the field and the revision that
changed it. The page rebases its untouched drafts automatically and marks a
draft stale only when its own field changed. A Web session's
`title_exposure_changed` is followed at once by a fresh, web-filtered
`title_state` for every input it names, so a field Studio exposes appears in
open pages with its value and revision, and a hidden one disappears, without
a reconnect. Uploads are refused unless the title renderer can draw them
(at most 16 MiB of decoded RGBA, such as 2048x2048, within 8192-pixel
edges; larger pictures were accepted before and drawn blank), a `Transfer-Encoding` body is refused rather
than stored with its framing, and a non-regular file under `assets/` is
never opened. `apps/freemixd/tests/native_web_titles.rs` drives the upload,
draft preview, per-field edits, stale refusal, and live resync against a
native-media daemon whose renderer draws the uploaded image. Protocol 2.104
keeps hidden values out of the definitions that set title fields: a Web
session's cue lists and shortcuts carry each title text or number action
aimed at a field hidden from it as a `hidden` marker (an action on
`preview`, `program`, or `selected` counts as hidden when any title hides a
field of that id), its scripts carry no source, and after an exposure change
it is re-sent every cue list and shortcut that sets a title field. Its edits
that send a marker back keep the stored action (matched by cue id, action
position, title target, field, and kind); a marker anywhere else is refused
`invalid_command` (from Studio too), and an action setting a hidden field
with its own value, or any `script_set`, is refused `permission_denied`.
`apps/freemixd/tests/web_tally.rs` scans every record a Graphics page gets.
A Web session's title preview draws each hidden field at its template
default rather than its stored value (`title_preview.rs` unit test and the
native browser-editor test, where the page's preview of a logo Studio hid
shows the template's red while Studio's shows the saved green), and a
script run's fault in its `script_state`, `script_ran`, and
`script_run_changed` records, a cue's fault at a script step in its cue list
and `cue_run_changed` records, and a refused run's command result, carry
the typed reason's text instead of the failure's message, which may quote
the source (`web_titles.rs` unit tests); neither changes the wire.
Remaining gap: the capability profiles are not certified. `GX-006` remains
planned.


Current implementation boundary for item 3: `fm-browser-input` provides a
deterministic network policy that strictly normalizes URLs (including numeric
IPv4 and IPv6 literal forms and dot segments), rejects credentials and
malformed hosts, applies scheme, private-network (also after resolution), and
wildcard host allow/deny rules where deny wins, and validates bounded redirect
chains against loops, HTTPS downgrades, and `file:` targets. Custom CSS is
accepted only after a lexical scan rejects style breakout, imports, escapes
outside strings, unsafe functions, and excess external `url()` references,
which are returned for policy checks. The fake renderer's fixtures may declare
in-page fetches (`data-fetch`), and navigation refuses a page whose fetches
the policy would block; the renderer now navigates through the full network
policy (navigation grants, host rules, URL bounds, and redirects), and maps
a zoomed surface's pointer events back into page space. Remaining
gaps: no real browser engine,
DNS, or sockets; browser audio, HTML5 media, on-disk
profile isolation, and refresh-on-activate remain
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
diagnostics. `fm-data` also provides a bounded file polling source that
re-reads one CSV or JSON file at a caller interval and emits only when its
length or modification time changes, and a bounded XML reader that turns one
document into nested typed elements (no DOCTYPE, entity expansion limited to
the five named entities and numeric references), and reads RSS 2.0 and Atom
feeds into rows of title, link, summary, published, and id, and reads an
HTML table's header and body cells into rows. Remaining gaps:
no XPath, spreadsheet,
Google Sheets, chat, or database adapters, no real network polling, and
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

Current implementation boundary for item 5 Media List inputs (`IN-006`): a
list input is a media input whose source is its active item's file, plus a
`MediaList` (items with per-item in/out marks and titles, Auto Next, Auto
First, loop, and a shuffle seed) in the show and in `project.json`. The
engine creates, edits (undoable, padlocked), navigates (next, previous,
first, last, go to a position, go to an item), and advances lists; its
shuffle is `fm-playback`'s seeded per-cycle order. Activating an item swaps
the input's source and trim (a file already open is re-cued in place) and
plays it when the list was playing. Both daemons journal an Auto Next
`advance` when the active item ends, so replay is frame-exact; host paths
are imported into the bundle; snapshots carry `media_list_state` records
(Protocol 2.74, schema 64).
The native daemon prefetches every open list's next, previous, and first
items (the one Auto Next steps to, the one Previous steps to, and the first
of the current cycle, when each is another file): a background thread opens
the file, decodes its in-mark picture and first audio block, and the window
after them. A step to that item (Auto Next, Next, Previous, or First)
switches the input's worker to the cued player before the step's frame is
projected, so that frame already shows the new item's in mark
(`native_media_lists` records Program and proves both switches hold and skip
no frame). A step to an item that was not prefetched (a jump, or a standby
still opening) still holds the previous item's last picture until the file
opens. Studio adds lists (pasted paths and M3U
playlists with relative entries and `#EXTINF` titles) and shows a list card in the Media
tab with search, drag reorder, per-item marks, the active item, Previous and
Next, and the options. Each row starts with the item's own picture
(Protocol 2.80 `input_thumbnail` records with `item`): a native daemon
decodes the item's in-mark frame from its file, the simulated daemon draws
a card per item, and an audio file gets the audio card; a drawn card keeps
the thumbnail feed subscribed. Every item's file length is probed off the
session loop (Protocol 2.91: the native daemon through its decoder, cached by
asset and probed again when the file's size or modification time changes;
the simulated daemon by the synthetic source's length), so each row's out
field shows where the item really ends and the card header totals the
list; the Web operator page and the CLI (`remote-status`,
`input-media-list`) show the same lengths, totals, and current item.
Each list carries a transition (Protocol 2.95, schema 77: `cut`, `fade`,
`dip`, or `wipe`, 1 to 600 frames, an undoable list edit) that a playing
list's step to another item runs: the engine keeps the outgoing item's
transport playing under the incoming one for the transition's frames and
reports each frame's weight, and under Auto Next the step comes that many
frames before the active item's out mark so the items overlap without
lengthening the list. The native daemon keeps the outgoing decoder running
beside the prefetched incoming one, composites their pictures on the CPU
(`fm-video`'s `blend_items`), and mixes their sound with the transition's
gains (`native_media_lists` records Program through a wipe and checks
every composited frame); a step to a file that was not prefetched still
cuts. The simulated daemon runs the timing without compositing its
synthetic pictures. Studio's list card edits the transition with hover
help. Gaps: a jump is not prefetched; the CLI steps a list
(`input-media-list-navigate` and its `remote-` form) and edits its options and
items (`input-media-list-options`, `input-media-list-item`); no vMix API
verbs, no cross-input GO bank, and Linux-only native evidence. The generic
`page_navigate` step and the cue `page` action drive a list too: a page step
naming a media list is rewritten to the list's own `navigate` (same
next/previous/first/last/goto tokens, but Next and Previous wrap under Loop
where a document page step clamps). `IN-006` stays `planned` until the
remaining gaps land.

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
Remaining gaps: the clocks wait while either daemon's frame loop realizes a
transition or title phase, so a boundary falling inside one is cut on the
first frame after it (its scheduled start, and so every later boundary, is
kept); a SIGKILL still loses up to a quarter second of idle progress (the
marker cadence), although marker and fold writes now run as background jobs on the
persistence worker, off the frame loop; simulated pacing anchors to the wall clock when a
boundary becomes pending, so it is approximate across commands and the catch-up frames
render synchronously, and controls such as Pause or Next execute at the
lagging frame cursor, so Studio's countdown is approximate on the simulated
daemon; entries hold a fixed duration or follow their input's media length,
and take any transition including a Stinger slot (Protocol 2.144, schema
101), while an end trigger of the on-air media input
advances a playing playlist with a journaled `next`; enable/disable, loop
changes, and list edits apply while the playlist runs, keeping the on-air
entry's timing (an edit that removes or
disables the on-air entry is refused); Studio's Start in field schedules a
start a chosen number of seconds ahead; no per-entry hard starts or
calendar recurrence; no vMix API. The daemon now appends every realized
Program change to `as-run.log` beside the bundle (wall clock, engine frame,
input, and name), and `freemix-cli as-run` prints it. `OR-015` remains
planned.

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
it.
Timed GO (Protocol 2.64, schema 58): `cue_arm` arms the standby cue (or a
named cue) to fire 1 ms to 24 hours after the command's frame, converted
to whole frames at the show rate and rounded up, so it is due on one exact
engine frame; `cue_disarm` drops it. A list holds one armed GO beside its
waiting cue, persisted in the run state and journaled, so a restarted
daemon fires it on the same engine frame (downtime does not count) and
restore refuses one due more than a day of frames past the cursor. The
cue clock fires it through the list's `cue_advance` exactly as a GO of the
cue on that frame (a wait of the same list due on that frame runs first,
the GO one frame later); a refused action faults the list, disarms, and
applies nothing, leaving a waiting cue waiting. Any GO that fires the
armed cue disarms it; Stop and Reset disarm; edits may change but not
remove the armed cue, and its list cannot be removed while armed. Arming
needs exactly a GO's permissions for the armed chain, the clock fires on
that authority, and edits of an armed chain need what they add. Studio's
Cues panel has a "Fire in" field, Arm GO, Disarm, an ARMED chip, and an
approximate countdown (English and Spanish hover help); `freemix-cli` has
local and `remote-` `cue-arm` and `cue-disarm`.
Data update (Protocol 2.65, schema 59): a `title_number` action sets a
title's number field to a value or steps it by a nonzero amount (a score
+1, a countdown -1), each within ±999 999 999 999, without retyping the
title's text. The value is read when the action runs, after the frame's
earlier actions, so two steps of one field on one frame add both; a text
field takes the result as decimal text, and stepping it needs it to hold
a plain whole number (blank reads as zero). A missing, image, or color
field is refused when the list is set; non-numeric text or a result past
the bound refuses the GO whole (or faults the list after a wait). It
needs `edit_project` like `title_text`; a numeric result adds no bytes to
the frame's 8192-byte title text bound. `cue_run_changed` and
`script_ran` carry each field's final value as text or a number, so Studio
and the CLI see the new score; scripts gain `title_number` and
`title_add` (`edit_project`). Studio's Cues panel adds a Title number
action (title picker, a field picker of the title's number and text
fields, Set to or Step by, and the value) with English and Spanish hover
help; `freemix-cli` takes `title_number.<input>.<field>.<set|add>.<value>`.
The web tally page (Operator role) arms the cue it shows on standby with a
"Fire in" picker (10 s to 1 h), Arm GO, and Disarm beside GO, gated like
GO, and its cue line shows the armed cue with an approximate countdown,
in English and Spanish.
External cue controls (Studio-local; no protocol or schema change):
every key of the Cues panel's transport is bindable. The keyboard has `G`
for GO, `,` for Step back (standby to the previous cue), `.` for Skip
(standby to the next cue without firing it), and exact Ctrl+Shift chords
(Cmd+Shift on macOS) M for Arm GO with the panel's "Fire in" delay, D for
Disarm, X for Stop, and E for Reset. They are listed in the keyboard
reference under Cue list, named in each panel key's English and Spanish
hover help, and covered by the key map's conflict test. Studio's loopback
OSC contract (`--osc-listen`) adds `/freemix/cue/go`, `previous`, `next`,
`stop`, `reset`, `disarm`, and `arm` (the "Fire in" delay, or one `i`
argument of 1 ms to 24 hours). Keys and messages act on the list the Cues
panel shows and are gated exactly like the panel key (GO and arming also
need the armed chain's permissions); an OSC cue message whose key is
unavailable counts as rejected. A Stream Deck, X-keys, or MIDI surface can
drive cues through an OSC bridge such as Bitfocus Companion. Studio reads
MIDI itself: raw ALSA ports on Linux and `CoreMIDI`/`WinMM` through `midir`
on macOS and Windows, so no ALSA development headers are needed for the
Linux build and `RC-005`'s device transport is in place.
Cue preview (Studio-local; no protocol or schema change): under the GO
key a "GO will:" box lists, step by step, what firing the standby cue
does across its waits and delays and every cue it auto-follows into
(`Cut to Camera 2 (now Camera 1)`, `Step Score Home by +1 to 3 (now
2)`, `Wait 2.0 s, then:`, `Run script "Intro"`, `Then cue "Close"
follows:`), and hovering any cue's state chip shows the same preview
for that cue. The walk keeps a local copy of the show current, so a
step reads what earlier steps leave (two steps of one title number add
both; a take swaps Program and Preview). Each step is marked when the
session lacks its permission (the classes the GO gate checks, including
a script step's verbs), when it changes nothing (a take of the Program
input, a strip that is already at its level, an input without an audio
strip), and when the authority would refuse it as the show stands,
using the engine's own rules and copies of `fm-model`'s arithmetic that
tests hold to the originals (Studio links no server-side crate): a missing
input, title field, script, stream destination, or Stinger slot; a
media or title action on another kind of input; a number update on an
image or colour field, on text that is not a whole number, or past the
bound; a script that waits or does not parse; Preview set to the
Program input; Program taken twice on one frame; and a switcher step
while a transition the chain started, or the operator's T-bar, is
running. A summary says whether the GO would be refused whole or the
list would fault after a wait. Preview on PVW selects the shot the
cue's first take puts on Program (or its first Preview step) on the
Preview bus through the existing `select_preview`, gated like any
Preview selection. English and Spanish, with unit tests of the
resolution and an offscreen render (`docs/progress/studio-cue-preview*.png`).
Remaining gaps: lists are not bound to an input button, and Studio has no
direct MIDI, HID, or X-keys input (only OSC, through a bridge); the cue
bindings are fixed, with no rebinding or learn UI, and act only on the
list the Cues panel shows (no per-list or per-cue key or OSC address);
the cue preview is worked out on the console, not by the authority:
it does not run a script step (values a script may change are left out
after it), cannot see a
runtime transition the chain did not start, Stinger media fallbacks, or
state that changes during a wait, and Preview on PVW selects the shot
only (no look at overlays, titles, or audio on the Preview bus); the
data update writes literal values only: no external data
source (`GX-004`) or row binding feeds it, and it updates text and number
fields, not colors or images; an armed GO takes a relative delay only
(Studio, the CLI, and the web tally convert nothing from a wall-clock
time of day or external timecode, and the daemon's frame clock, not a
house clock, decides when it fires); Studio's and the web tally's
countdowns are extrapolated and approximate, most on the simulated
daemon; the web tally offers fixed delays and arms only the standby cue
of its lowest-numbered list. `RC-017` remains planned.

Current implementation boundary for sandboxed scripting (`RC-011`): the
first slice is a purpose-built language rather than a Wasm runtime (schema
54, Protocol 2.58). `fm-script`, a dependency-free foundation crate, parses
and interprets `FreeMix` Script: integer, boolean, and string variables,
`if`/`elif`/`else`, `repeat`, `while`, `stop`, and 26 action verbs that map
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
component runtime or declared capability manifest; scripts cannot read
state beyond Program and Preview; the budget is counted in instructions,
not wall-clock time; and a first-frame refusal still folds the refused
action's typed reason into `script_failed` (its code and message are kept),
while a later part's fault keeps the reason itself (Protocol 2.63). A cue can run a script as one step (Protocol 2.60,
schema 55; see `RC-017` above for its frame, failure, recursion, removal,
and authority semantics); its `cue_run_changed` names each script step it
ran with that run's action and instruction counts (Protocol 2.61), which
Studio's Edit > Scripts window and `freemix-cli go` report as the script's
last run.

Script waits (Protocol 2.62, schema 56): `wait FRAMES` lets one run span
frames. Design choice: the parts after a wait are evaluated on their due
frame, resuming saved interpreter state, rather than planned up front,
because a script reads Program and Preview and an up-front plan would act
on a show that no longer exists when the part lands (an operator may have
taken another camera meanwhile). Each part is all or nothing on its own
frame, as a whole run was before. The cost is that the interpreter state
must be persisted, so it is small and bounded: the statement path to the
wait (branch taken, `repeat` turns left), the variables, and the budget
used. The show has one run slot (a run waiting or the fault its last part
ended with), saved with the project and journaled, so a restarted daemon
resumes the run on its due engine frame exactly like a waiting cue; the
cue clock drives it (`cue_advance` for list `0`). Bounds: 32 waits and ten
minutes of frames per run, and one run's instruction, action, variable,
and string budgets cover all of its parts. A part that fails on its frame
applies nothing of that frame and leaves a typed fault (kind, line,
column) in the slot instead of a refusal, published with
`script_run_changed`; `script_stop` (a journaled `transition` command
without undo, like a cue Stop) ends a waiting run or clears a fault.
While a run waits, another run and edits of the running script are refused
`script_running`. A direct run of a script holding `wait` needs every
action verb's permission up front, as an armed cue chain's script step
does, since later parts run unattended. A cue's `run_script` step cannot
run a script that waits (refused at edit time with `script_waits`), so a
cue chain never waits on a script; cue waits remain the way to space a
cue's script steps. Studio's Scripts window shows "(running)", the next
part's frame with Stop run, or the failed part with Clear failed run;
`freemix-cli` adds local and remote `script-stop` and prints run state in
`scripts` and `script-run`. Remaining gaps for waits: a waiting run's later
parts are not previewed anywhere; and a local `freemix-cli script-run` runs
every part on the simulated clock before it returns. `RC-011` remains
planned.

Concurrent script runs (Protocol 2.69, schema 60): the one run slot became a
bounded set, so a lower-third timer and a score update can wait side by
side. Up to 8 runs wait at once, each of a different script; a run gets an
id (the show's last issued id plus one, persisted as `last_run`) when its
first part waits, so ids survive saves, restarts, and journal replay and are
never reused. Design choice: a second run of a script that already waits is
refused `script_running` rather than allowed, because two runs of one source
act on the same titles and inputs and would fight; the operator stops the
first (or the script finishes) and runs it again. A run that would wait
while 8 do is refused `bounds_exceeded:script_runs:8`. Each run is
authorized on its own, as before. The cue clock still drives every run as
list `0` (the earliest due, the lowest id on a tie); runs due on the same
frame resume on that frame together, the later advance joining the frame
the first holds. A failed part faults its own run only, and each script
keeps its own fault. `script_stop run=<id>` ends one run or clears its
fault; `script_stop` alone ends every run and clears every fault
(`script_runs_stopped`). Wire `run` states name the run id and, while
waiting, the line of the `wait` (the run's current step). Studio's Scripts
window lists every active run with its own Stop (or Clear) and Stop all
runs; the CLI's `script-stop` takes `--run`. Remaining gaps: runs cannot be
queued behind the bound, and a stop of every run is one command, not a
selection.

Document inputs (`IN-014`, Protocol 2.77, schema 66): a PDF or a
presentation is an input shown one page at a time. Renderer choice: `hayro`,
a pure-Rust PDF rasterizer (MIT/Apache-2.0, no `unsafe`, no system library),
so every platform renders PDFs with nothing to install; PowerPoint and
OpenDocument packages are read in pure Rust (`zip` and `quick-xml`) for the
slide count (hidden slides included), title, and presenter notes. A
presentation renders after a headless LibreOffice conversion to PDF when the
server has `soffice` (on its own thread, in a throwaway sandbox directory
and profile, with a cleared environment and a timeout); without it the
input reads `unavailable` with "install LibreOffice on the server or export
the presentation to PDF" while its count and notes still work, and Keynote
always asks for a PDF export. Navigation is the generic page step slideshows
also use (`page_navigate` / `EngineCommand::PageNavigate` with
`PagedNavigation`: next, previous, first, last, goto); a document clamps
instead of wrapping, its page moves with the accepted command (answered by
`paged_changed`), and cue page actions step documents too (a moved page is
announced by a `paged` runtime event on the next frame); the engine refuses
steps while the count is unknown (retryable)
and the daemon reports counts from the asset bytes before and after every
command, so journal replay is deterministic. The page persists in
`project.json`. `freemixd` renders pages off the frame loop at the
requested size with a bounded (96 MiB) page cache: the simulated monitor
taps and input thumbnails show the page, and with `--native-media` a worker
turns the current page into a live source frame (fitted, transparent
outside the page). Clients see `paged_state`, `paged_changed`, and the
`paged` runtime event, and fetch notes and 128x72 thumbnails with
`paged_page_request`. Studio's Media tab shows each document with Previous
and Next keys, the page counter, a thumbnail strip (click to go), and the
presenter notes; documents are added through Import files… (or Replace
source…) with a path or `asset://` URI, and the cue editor's page actions
list documents next to slideshows. `freemix-cli` adds `input-page` and
`input-pages` for saved projects (`remote-page-navigate` steps a document on
a server) and document-aware
`project-import-media` and `input-import-batch`. Remaining gaps: no
PowerPoint rendering without LibreOffice (no slide builds, transitions, or
embedded media; a converted deck shows its final slide state), Keynote needs
a PDF export, scripts, OSC, and the console's slide keys step pages, the
simulated scene composer does not draw a document used as a Scene layer;
removing the input now cancels a running conversion. `IN-014` remains
planned.

Deferred fault reasons (Protocol 2.63, schema 57): a script part that fails
after a wait, and a cue action refused when its wait ends, now keep what a
refused command would have said: the refused action's typed refusal reason
(when it has one) and its English message, bounded to one line of at most
256 bytes. The fault carries both on the wire (as hex tokens inside the
script `run` and cue list `state`) and in `project.json`, so Studio's
Scripts window shows "The server said: …" for a failed later part as it
does for a first-frame refusal (a typed reason worded in the console
language), the Cues window adds the same to its fault note, and
`freemix-cli` prints `reason=` and `message=`. The Scripts window's Stop
run and Clear failed run keep a row of their own below the run status.

Current implementation boundary for still image and slideshow inputs
(`IN-008`, `IN-009`): Protocol 2.73 (schema 63) adds two input kinds.

- `fm-codec-image` decodes PNG, JPEG, WebP, GIF (first frame), BMP, TIFF,
  and SVG (rasterized with `resvg`) under byte, pixel, and dimension
  limits, applies EXIF orientation, converts embedded ICC profiles to sRGB,
  keeps straight alpha, and places the picture in the show raster with
  Fit, Fill, Stretch, or 1:1 (`decode_still_for_output`).
- An image input names one picture asset and a fit. A slideshow names an
  asset folder (every picture file directly inside it, in natural name
  order, at most 10,000) or an explicit list of up to 64 pictures with
  optional captions, a per-slide hold in frames, a cut or a fade, Ken Burns,
  ordered or seeded-random order, loop, and auto-advance; a show holds at
  most 8 slideshows. Both persist exactly (schema 63), are undoable adds,
  replaceable sources, and duplicable.
- The engine runs each slideshow on the show clock with
  `fm-playback::Slideshow` (not journaled: replaying the same commands gives
  the same slides, fades, and crops frame for frame). `page_navigate`, a
  generic paged-input step (next, previous, first, last, goto, compatible
  with `fm_types::paged`), is a live switcher operation realized on the next
  frame, also available as the cue action `page.<input>.…`. A folder's
  listing is supplied by the daemon; a listing change keeps the slide on air.
- Clients may name absolute host paths in a request; the daemon copies a
  file, or a folder's pictures, into the project's assets first and the
  show only ever holds asset URIs. Both daemons rescan a folder slideshow's
  asset folder every 2 s and on each project revision, so pictures added to
  or removed from it join or leave the show.
- The simulated daemon decodes slides and composes the fade and Ken Burns
  crop on the CPU, for Program, the monitor taps, and the input bank
  thumbnails. Native `freemixd` decodes an image once (at startup or on a
  live add) and uploads it once; a slideshow's slides are decoded on worker
  threads, each uploaded once and kept (on air, next, and a few spare), and
  each frame composes the incoming slide over the outgoing one on the GPU
  with the crop and fade. A still media file larger than the show is now
  fitted instead of stopping startup.
- Studio's Add input dialog has Picture and Slideshow kinds (path fields,
  since the host has no native file dialog, filled by dropping files or a
  folder on the window; folder or list; timing, fade, Ken Burns, order,
  loop, auto-advance, and placement). The media transport panel has a
  Slideshows section per slideshow with FIRST, previous, next, and LAST
  keys, the slide on air and the count, and a strip of slide keys (numbered
  names, fetched in windows with `slideshow_slides_request`) that go to a
  slide. The cue editor has a slideshow step action. Help is in English and
  Spanish and every new control is in the hover audit.
- The CLI adds `input-add-image` and `input-add-slideshow` (copying host
  pictures into the project), their `remote-` forms, and
  `remote-page-navigate`.
- Tests cover decoding (formats, orientation, ICC, alpha, limits, fit),
  the slideshow model, engine slideshows (holds, fades, Ken Burns, live
  navigation, folder follow, replay), persistence, the wire form and its
  refusals, client replication, the simulated daemon (host import, folder
  import and follow, stepping, slide windows, reconnect), a native daemon
  process test (an image at startup with Fit, an oversized still, a live
  Fill image, and a stepped slideshow, all read back from Program), the CLI,
  and the Studio forms and panel.

Remaining gaps:
- Studio's slide strip now shows picture thumbnails around the slide on air
  (read through the paged page worker) but still has no native file or
  folder dialog;
- captions are metadata and labels only; nothing draws them on the picture;
- only cut and fade transitions exist; OSC and scripts cannot step slides
  (console keys bound to Next slide and Previous slide can);
- a host folder is copied once, so live edits follow the project's copy,
  not the original folder; there is no filesystem watch, only a 2 s rescan;
- portable bundle export copies a slideshow folder's pictures, the asset
  audit resolves the folder itself, and relink finds and imports a same-named
  folder with pictures;
- native Ken Burns crops are whole source pixels and a random order's next
  slide is prefetched approximately; the slideshow textures are reported as
  a statistics gauge rather than inside the media registry's accounting;
- animated GIF and WebP show their first frame; there are no solid colour
  or bars changes here (`IN-010` remains separate);
- no Windows or macOS native run has been exercised (the code is portable
  and cross-checked to compile for macOS).

`IN-008` and `IN-009` remain planned.

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
take is the engine's title timeline (Protocol 2.81, schema 68): the frame
a title was taken to Program, an overlay, or Preview, which tickers, scene
clocks, and the In/Out bus animation count from; it restarts when the title
moves between air and Preview or is taken again, is identical under journal
replay because it is derived from realized engine state per frame, and is
saved in `project.json` `title_timelines` so a restored daemon continues it;
clock titles refresh at the bounded title rate. `freemix-cli title-input-add`
imports clock templates. Remaining gaps: no shared project or engine clock
registry, so title clocks are per title, not operator-controllable timers,
and not exposed through the protocol, CLI commands, or UI; no project-level
time zone (each time-of-day clock names its zone); templates cannot show
media remaining, session, or playlist clocks; designer compositions and crawls take no clock
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
LEDs and motor faders. Studio now reads MIDI controllers: on Linux it lists
the ALSA raw MIDI ports (`/dev/snd/midiC*D*`, named by their card's ALSA id)
every second, so a controller plugged in while it runs is picked up, reads
each port through the `fm-automation` parser, and a note or controller
press (a note on, or a controller reaching 64) drives any console key
profile action (switching, transitions, favourites, overlays, replay marks,
the cue GO) under that action's key gates. The Key profiles window lists
the controllers and binds each action by Learn (the next press) or Unbind,
and the bindings are saved with the console's preferences
(`keymap.midi.note.<channel>.<number> = <action>`), at most 64 (unit tests
of the bindings and the dispatch, the reader against a FIFO port, and the
preferences round trip). The same reader uses `CoreMIDI` on macOS and
`WinMM` on Windows through `midir`, so all three Tier-1 platforms list and
read MIDI controllers; the Linux raw-port path keeps no ALSA development
dependency. The same reader drives lamp feedback to bound controls: the
selected Preview and Program inputs, Fade to Black, overlays, and the Replay
player's transport light their LEDs, writes going through the raw ALSA port
on Linux and `midir` on macOS and Windows. A controller bound to a value
maps its 0–127 range onto that value (the monitor bus level), and the
console sends the current level back, so a fader or knob is no longer only a
button and a motor fader follows the console. An NRPN or RPN is learned and
driven by its 14-bit parameter number: the parser keeps each channel's
selected parameter from the select controllers (99/98 and 101/100), reports
the data entry (6) as that parameter's press, and sends a lamp state back as
the parameter select followed by the data entry. A Linux host also reads
X-keys-style HID button panels from `/dev/input`: the panel's name and key
code identify a button, Learn binds it to a console action, and the
bindings are saved with the key profile as
`keymap.hid.<len>:<device>.<code> = <action>`. Remaining gaps: no UDP
transport; the bindings drive console
actions (and so reach `freemixd` through them) but are not persisted in the
project; HID panels are not read on macOS or Windows; and MIDI 2.0 and GPIO
or network tally adapters are absent.
`RC-005` and `RC-006` remain planned.

Current implementation boundary for item 8 fader feedback (`AU-009`):
`fm-automation` adds a bounded per-control `ControlCoalescer` that keeps the
latest absolute value, sums relative deltas, preserves button edges in order,
pairs same-tick 14-bit MSB/LSB updates, and refuses rather than drops new
controls when full; coalesced ticks feed the existing soft-takeover pickup.
`FeedbackRouter` now schedules outbound feedback per tick with per-device
budgets, round-robin fairness, per-binding minimum intervals, touch
suppression with a forced resync on release, a post-release settle hold, and
a per-device byte-rate limit of encoded feedback bytes per second, and
acknowledged-level echo suppression. `fm-audio` adds a `GainSmoother` whose
per-sample ramps
have a guaranteed step bound and land bit-exactly on target across block
splits and retargets; its `GainTaper` selects a ramp that is linear in gain or
linear in decibels, the decibel ramp lengthening itself so the same linear
step bound and settling bound still hold. A 14-bit controller's MSB is now held until its LSB
arrives, so a pair that straddles a tick boundary never surfaces the
intermediate value. Remaining gaps: no daemon, Studio, CLI, or real-time
mixer wiring; and there is no end-to-end
device-to-audio test or motor-fader
hardware evidence. `AU-009` remains planned.


Current implementation boundary for item 9: `fm-ptz` encodes and parses VISCA
commands, inquiries, and replies for pan/tilt, zoom, focus, presets, and
system control with explicit range errors, translates each PTZ intent into a
complete drive set so zero components stop prior motion, frames VISCA-over-IP
payloads and control messages with a wrapping sequencer, and maps joystick and
mouse input to axis speeds through deadzones and response curves. Remaining
gaps: no serial or UDP transport, retransmit, timeout, or reply correlation,
no camera adapter or hardware validation, presets are not yet switchable
virtual inputs with thumbnails, no HID or UI binding, and no ONVIF, Panasonic, or
digital PTZ. `IN-028` remains planned.

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
The gateway reaches phones on the LAN (Protocol 2.71): plain HTTP stays
loopback-only and any other `--web-listen` address requires TLS, either
`--web-tls-cert`/`--web-tls-key` or `--web-tls`, a self-signed ECDSA P-256
certificate (rustls with the `ring` provider, rcgen) kept with mode 0600 in
the web state directory, whose SHA-256 fingerprint the daemon prints and
Studio shows. An Admin console starts a pairing for a Viewer, Audio, or
Operator device; Studio's Web access panel shows the URL, a QR code
carrying the one-time link code and fingerprint in the URL fragment, a
six-digit code to type, the paired devices, and Revoke. Codes live 120 s,
are single-use, and die after 5 wrong tries; `POST /v1/pair` is limited
per client address and globally and checks `Origin`. The page stores the
returned `fmd1_` device token in `localStorage` (no cookie, so no ambient
credential for CSRF or cross-site WebSocket hijacking) and reconnects with
it; the daemon keeps only SHA-256 digests in `web-devices.txt`, limits each
device to its pairing's roles, and ends a revoked device's sessions within
250 ms. Every request must name an IP literal, `localhost`, or a
`--web-host` name in `Host` (DNS-rebinding defence), and pages carry a CSP
whose `connect-src` is the serving origin only. Process tests cover the TLS
handshake against the generated certificate, refusal of plain HTTP on
`0.0.0.0`, pairing, role limits, revocation, cross-origin and foreign-Host
refusal, and that no token or code reaches the logs; the tally tests run
over TLS; screenshots show headless Chromium pairing over `wss` in
`docs/progress/`.
Trust and reach (Protocol 2.87, no schema change): `--web-tls` now creates a
per-install local CA (ECDSA P-256 root, ten years, path length 0, a random
suffix in its name) in the web state directory and serves a 397-day leaf it
issues for every LAN interface address, `localhost`, the `--web-host`
names, and the mDNS name; the leaf is kept across restarts and reissued by
the same root when the addresses change (checked every 15 s) or it nears
expiry, so phones that installed the root keep trusting the gateway. The
root downloads without a credential from `/freemix-ca.crt`, and a
script-free start page at `/` links the pages and the root with iOS and
Android steps. `--web-tls-cert`/`--web-tls-key` (mkcert, a company CA) are
validated before use (parse, validity window, a named host, `serverAuth`,
each link signed by the next, key match) and reloaded about a second after
the files change; a broken replacement keeps the served certificate and is
reported as `reload_failed`. Key files are written 0600 in a 0700
directory, keys other users can read are refused, and neither keys nor
their parse errors are ever printed. On a LAN address the gateway
advertises `_freemix._tcp` and `_https._tcp` on `freemix-<computer
name>.local` (`--web-mdns-name`, `--no-web-mdns`) through the pure-Rust
`mdns-sd` responder and accepts that name in `Host`. The `web_access`
answer carries the trust, the root's fingerprint, the leaf's expiry, a
certificate problem, the other origins, and the mDNS name; Studio's Web
access window shows the trust and any problem, lists every address with a
QR code of the chosen one (the mDNS name first), and offers the root with a
QR code, its fingerprint, and install steps (English and Spanish hover help,
in the hover audit). Studio's `--web-tls-cert`, `--web-tls-key`,
`--web-mdns-name`, and `--no-web-mdns` reach its supervised daemon, and the
windowed program daemon (`--display-presenter-if-available` and the other
presenter options) serves the gateway. Process tests cover a client that
trusts only the downloaded root (WebPKI name checks, no pin), the root and
leaf surviving restarts, private file modes, operator certificate reload
and refusal, the mDNS name served (or the fallback where multicast is
unavailable), and pairing through the windowed daemon under Xvfb; unit
tests cover chain validation, CA issuance and reissue, and the DNS-SD
records.
Remaining gaps: no real phone or tablet has installed the root, resolved
the `.local` name, or paired (on this development VM multicast does not
loop back on the LAN interface, so the advertisement is checked by record
contents and process state, not by a second resolver); the root carries
name constraints only when made after they landed (an older root is kept
unconstrained, and a public address or a `--web-host` added after the root
was made is left out of the leaf), HSTS covers names only (browsers
ignore it for addresses), Android Chrome may not resolve `.local`
names, the fingerprint in the QR link is that of the current leaf (a
reissue changes it), and pairing expiry is tested with an injected clock;
Web peers
receive no monitor frames, so there is no low-latency web preview; the page's
JavaScript duplicates the Rust model and is checked by page-source, wire, and
`node --check` tests and screenshots, not an automated browser test; and FTB, audio
tally, and Cut confirmation are not modelled on the tally page.

The same gateway also serves a phone and tablet operator page at
`/operator` (no protocol change; it pins the current protocol), with the tally page's
headers, token handling, and reconnect, heartbeat, and resync driver. It
shows the realized Program bus (desired-but-not-aired marked), a Preview bus
(tap to preview), `CUT`, `AUTO` with a Fade, Alpha fade, Slide, Zoom, or Wipe
picker and a 0.5-3 s duration converted to frames at the show rate, Fade to
Black with its `LIVE` / `FADING TO BLACK` / `BLACK` / `FADING UP` phase from
the realized position, both program recorders and every stream destination
(desired and realized state) with start/stop, and a mute key per input
strip. Programs, previews, Fade to Black, strips, recorders, and streams
follow switcher, playlist, cue-run, and script-run events. Every control is
gated by the granted role (Viewer, Audio, or Operator) and, when disabled,
names why (`Not connected to the show`, `This session lacks the transition
permission`, ...); the server authorizes every command again. A mute rewrites
the whole strip, so it carries the revision its values came from; one
refused as stale is retried once after the page catches up, only while the
strip is unchanged, and an `audio_group_solo` event resynchronizes. Markup
uses labelled sections, live status regions, `aria-pressed` toggles, and
key shortcuts (`C`, `A`, `B`, `1`-`9`). `freemix-web`'s `OperatorSurface`
is the transport-free model of the gating, disabled reasons, commands, and
badges that the page mirrors. The tally page now reads only the label of an
input's `id~name~lifecycle` entry (it showed the lifecycle suffix).
Remaining operator gaps: no monitor video or per-input meters for Web peers
(Protocol 2.66 below adds Master meters and realized recorder and
destination status), no T-bar,
stinger, overlay, or title keys, no Cut confirmation, and the pages are
verified by page-source and wire tests plus headless-Chromium screenshots
(`docs/progress/web-operator-*.png`, `docs/progress/web-tally-*.png`), not by
an automated browser test. `RC-014` remains planned.

Several Web clients at once (no protocol or schema change): the gateway
admits up to 16 Web clients together, so tally tablets and operator pages
share one show. Each client is its own control session, not a multiplex of
one: its own principal session id, the role it asked for in its handshake
(so a Viewer tally, an Audio page, and an Operator page hold different
permissions side by side), its own heartbeat and handshake deadlines, live
subscription, inbound and outbound rate limits, and outbound queue. That keeps
the authority model unchanged: every command is authorized against the
session that sent it, exactly as for a raw TCP peer, and nothing one client
sends is ever attributed to another. The scheduler already served raw and
Web peers as separate sessions; it now admits up to 16 Web sessions next to
its two raw peers. Idempotency keys stay engine-wide
(a reconnected session must be able to replay the result its previous session
lost), so each page draws its command ids and keys from a random per-load
nonce; authorization runs before any receipt replay, so a lower role never
obtains another session's accepted result. Every connection runs on its own
gateway thread: an accepted socket serving a page or completing an upgrade
(at most 8 at once, each bounded by the 500 ms handshake deadline; more are
closed unanswered) and then the client's relay. One non-loopback peer
address holds at most 8 of these connections at once; its next one is
closed unanswered and logged once as `FREEMIXD_WEB_LIMIT`. A relay write that blocks
for 250 ms on a peer that stopped reading drops that peer only; the scheduler
never blocks on a Web peer (one record in flight, handed over by `try_send`),
and a peer whose live subscription overflows is dropped as a slow client. An
authorized upgrade beyond 16 clients is refused with `503 Service
Unavailable` (credentials are checked first, so a wrong token learns nothing
about capacity) and the page retries with its usual backoff; a client's slot
is held until the scheduler drops its session.

Both pages are in English and Spanish: every user-visible string lives in one
`STRINGS` table per page with the same keys per language, and Spanish uses
Studio's catalog terms (`Programa`, `Previo`, `Corte`/`CORTE`, `Fundido a
negro`, `Cortina`, `Grabador`, `Transmisión`, `Silenciar`, `En espera`, `AL
AIRE`). The language follows a `#lang=en|es` fragment, then a manual
`Español`/`English` toggle saved in the browser for both pages, then the
browser's preferred languages. The operator page's recorders now say
`REQUESTED: REC` / `REQUESTED: OFF` (`PEDIDO: REC` / `PEDIDO: APAGADO`) when
the daemon sends no realized recorder state (the simulated daemon; native
daemons send `recorder_status` since Protocol 2.66, below).

Live recorder, destination, and audio status on the Web pages (Protocol
2.66, no schema change): native `freemixd` reports each recorder slot's
realized state in a new lossy `recorder_status` record (`unavailable`,
`off`, `starting`, `recording` with the time recorded so far, kept across
segment rotations, or `failed` with a sanitized short code such as `start`
or `capture:missing_output`, latched until the slot is started or stopped
again), built at most every 250 ms (`apps/freemixd/src/recorder_status.rs`).
Raw Studio peers take it in its own latest-wins slot; `fm-client` validates
and retains it like `stream_status`. Web sessions whose role may view status
(Viewer, Audio, Operator) now get a bounded lossy feed
(`apps/freemixd/src/web_feed.rs`): `audio_meters` cut to the Master channels,
`stream_status`, and `recorder_status`, each latest-wins, one record handed
to the gateway at a time and only while at most two other records wait for
the relay, meters at most every 66 ms (about 15 Hz, about 12 Hz at 25 fps)
and each status at most every 250 ms, never charged to the session's
outbound rate window. Commands, results, heartbeats, and events always go
first, and a browser that stops reading stops receiving feed records instead
of building a backlog. The operator page shows `REC 0:12:34`, `STARTING`,
`FAILED (start)`, `OFF`, or `NOT CONFIGURED` per recorder (with "start/stop
requested" while desired and realized differ), each destination's running
time, bitrate, and failure from its newest sample (a running destination's
retained failure reads "last error"), and a Program audio
section with per-channel RMS and peak bars on a -60 to +6 dBFS scale in
Studio's zones (green, amber from -18, red from -6), a 1.5 s peak hold
falling 20 dB/s, a CLIP lamp, and `METERS STALE` after a second without a
record. A daemon that sends no `recorder_status` (the simulated daemon)
keeps the requested-state badge and its note. The tally page shows chips
for what is recording, live, starting, or failed, with running times, and a
slim Program level per channel; neither page gains a control. Lossy records
repaint only the outputs and meters, never the whole page, and timers
advance between records. English and Spanish strings; process tests
(`native_sessions.rs`) follow both pages through recording, a Viewer's
refused stop, an Operator stop and restart, and check the feed rates; a
`node --check` test parses every page script when Node.js is installed.
Screenshots against a native daemon recording and streaming a media clip:
`docs/progress/web-operator-recording-meters.png`,
`docs/progress/web-operator-recording-meters-phone-es.png`, and
`docs/progress/web-tally-recording.png`.
The operator page now draws each strip's share of Program as a peak bar
(green, amber past -6 dBFS, red past -1 dBFS) from the Web meter record,
which carries the first 32 inputs' meters (no wire change; the page's
parser is checked under Node.js against the Rust codec's record). The tally
page draws a small per-input peak bar from those same meters; a tally
session opened with `#meters=off` keeps the bars off for that session
(remembered in session storage; `on` turns them back on), so a wall mount
can show its tally without the per-input audio. The simulated daemon
publishes meters from its simulated audio plane and reports both recorder
slots as unavailable, so its pages show realized state instead of only the
requested badge.

Readable output failures (Protocol 2.67, no schema change): a stream
destination's or recorder's failure now reaches every client as a stable
code plus, when `FFmpeg` exited, its exit status (`connect_failed.145`), and
never as `Debug` text (the page used to read `Reconnecting — Connect:ffmpeg
exited: ChildExited { status: Some(145) }`). `fm-io-network` tags each
transport error with a `FailureCause` it recognized in the child's redacted
stderr (`Connection refused`, `Network is unreachable`, `Name or service not
known`, `NetStream.Publish.BadName` and the other credential markers,
`certificate verify failed`, `Input/output error`/`I/O error`, `Permission
denied`, `Read-only file system`, `No space left on device`, `No such file or
directory`); the new markers only sharpen the cause, so retry behaviour is
unchanged. `apps/freemixd/src/output_failure.rs` maps transport causes and
recorder notices (plus the recorder's stderr tail) onto the 22 codes; the
daemon's own log lines keep the detail. Studio (`fm-ui-model`
`Locale::output_failure`), the operator page, and the tally page word every
code identically in English and Spanish ("Couldn't connect to the server
(FFmpeg exited with code 145)", "Server refused the stream key", "Network
unreachable", "Disk full", "Recording folder not writable"); a test checks
the pages against Studio's catalog. The operator page shows the reason on
its own line under the row (a running destination's as "Last error: …"),
disables Record on a recorder the daemon reports `NOT CONFIGURED` and names
the option that configures it (`--record-program` / `--record-program-2`).
Studio now follows `recorder_status` as well: such a slot reads `NOT
CONFIGURED` with its Start key disabled and the reason, and a failed slot
reads `FAILED` with its reason. Smaller page fixes: Preview buttons wrap like
the Program tiles, the first destination row keeps its divider, and the
`Tally` link is a full-height touch target. Screenshots:
`docs/progress/web-operator-recording-meters.png` (refreshed) and
`docs/progress/web-operator-output-failures-es.png`.


The native daemon serves several control sessions and the Web gateway (no
protocol or schema change): `freemixd serve --native-media` now admits the
same sessions the simulated daemon does, two raw peers (Studio and a CLI or
automation peer) and up to 16 Web clients, and accepts `--web-listen`
(still refused with `--fullscreen-program`), so tally tablets and an operator
phone follow a real show. Both modes run one session scheduler
(`apps/freemixd/src/sessions.rs`, formerly the non-native scheduler): every
peer keeps its own role and permissions, heartbeat and handshake deadlines,
live subscription, inbound and outbound rate limits, outbound queue, and
lossy side channels (Program monitor frames, meters, stream status, and input
thumbnails with their per-peer budgets go to raw Studio peers with
`view_status`; Web peers got none until Protocol 2.66's bounded Master-meter
and status feed). Because a native command waits for and
realizes the pacer's next frame, the native scheduler dispatches at most one
command per loop pass, round-robin across peers, and reads, answers
heartbeats, and writes every peer between commands; command throughput stays
one per frame (until native group commit, below), and no peer's commands hold another's heartbeats or records
for more than a frame each. An authorized Web upgrade beyond 16 is refused
with `503`. Process tests (`apps/freemixd/tests/native_sessions.rs`)
drive cuts from a Studio-like admin, a CLI operator, and an Operator page
while a Viewer tally is refused and every session sees every cut in revision
order; they hold a third raw peer and refuse the seventeenth Web client until
a slot frees; and they show a Studio peer that never reads delaying neither
the other sessions (round trips under a second) nor the Program (median
host lateness under half a frame, a recording holding every paced frame).
Remaining gaps: the windowed program daemon (`--fullscreen-program`,
`--display-presenter`) still serves no Web gateway.

Multi-session hardening (no protocol or schema change): both modes now serve
up to eight raw sessions, and a raw connection beyond them is refused at once
with a retryable `resource_exhausted` handshake rejection and closed (at most
eight such refusals are held at once, each for at most 500 ms; beyond that a
connection is closed unanswered) instead of waiting unaccepted with no
answer. Every raw session runs as the development principal with a session id
of its own. Web peers are pipelined: the scheduler hands a Web peer every
queued record at once and retires them as the relay acknowledges them, and
the relay polls its socket every 5 ms instead of every 50 ms, so a Viewer
tally keeps up with a CLI cutting every frame (before, it received about one
record per relay poll and was dropped as a slow client within seconds) and a
cut reaches Studio, a CLI, an Operator page, and a tally in about 30 ms
instead of about 150 ms. A peer that sends faster than it is served meets
backpressure instead of being dropped: a raw peer is not read while its
queue is full (it was closed as `inbound_overflow` past eight queued
messages) and a relay stops reading while the scheduler's inbound channel is
full (it closed the connection). The native command turn now rotates from
the peer that last took it, so peers with commands waiting alternate
strictly. A native daemon folds the journal into the manifest after 48
recorded mutations, right after a command's frame was realized (the most
time before the next deadline), instead of in front of the 65th command's
frame; the 64-mutation bound in front of an append remains as the backstop.
Measured Program host lateness under ten cuts a second from four sessions
for 25 s is unchanged by the scheduler (debug and release p50 about 2 ms,
p95 about 5 ms): its tail belongs to the recorder startup barrier (the first
frames, before any session exists, are 50–100 ms late), to occasional slow
frame realizations under host load, and to a journaled command's disk sync,
which stays in front of its frame because durability must precede the engine
commit the frame realizes and makes that frame late only when the sync
straddles a deadline (usually a few milliseconds, at worst about 100 ms on a
contended disk).

Native Program pacing (no protocol or schema change): the frame pacer is now
anchored at the first paced tick instead of at daemon start, and the startup
recorder opens on its worker (`FFmpeg` spawn and readiness off the loop)
while the loop keeps realizing frames into its bounded backlog, so readiness
no longer waits in front of frame 0. A segment whose bounded encoder queue
is full (still priming, or a running `FFmpeg` briefly descheduled or slowed
by its disk) moves the pair into that backlog instead of failing; the
backlog drains in sequence once the recorder catches up, and only a stall
that exhausts the backlog's byte budget (`enqueue:backlog_full`, reported
as fell-behind) or trips the recorder's no-progress timeout fails the
segment. `FREEMIXD_RECORDER` reports `backlogged_pairs` and
`backlog_peak_bytes` so the backpressure is visible before any loss. Streams,
LiveLAN, and HLS already started on workers. Each realized Program frame is
read back once and shared between stream packing and recorders, where each
used to issue its own blocking GPU readback. The command turn now opens only
when the time left before the next deadline covers the journal's recent
append peak plus 2 ms (half a frame before any append is timed); otherwise
the command waits behind at most one idle frame and then takes the next
turn. A command therefore still lands on the journal (append and disk sync)
before the engine commit its frame realizes. The journal and replay contract
is unchanged, and a command waits at most one frame longer. A crash test
SIGKILLs a recording, streaming, cutting daemon at varied moments and checks
that recovery holds every acknowledged cut and the matching Program.
Measured over 25 s with a recording, two HLS streams, and two sessions
cutting 7.5 times a second (llvmpipe on a host shared with other builds, so
spikes vary between runs): the first 2 s were p99 70–81 ms late, with two
frames over a frame late, and are now p99 5–7 ms with none. Command frames
after 2 s in release, over two runs, were p95 6–9 ms and p99 7–12 ms and are
now p95 0.1–0.3 ms. In the quiet run, p99 was 0.1 ms. The cut round trip
rose from p50 12–19 ms to about 37–40 ms (release). Group commit (accept a
command, sync it on a journal thread, apply it at the first frame after the
sync) was designed but not implemented here: the journal record carries the
frame it applies at, so it needs an engine API that prepares a command for a
later frame while idle frames realize in between (it later landed for the
simulated daemon only; see Group commit below). Remaining gaps at this point
(the journal fold, the progress marker, and staging-buffer allocation were
fixed by the persistence worker below): a disk sync longer than about a frame
still makes its frame late; llvmpipe frame realization still stalls 50–200 ms
under host load; displays were not measured.

Persistence worker (no protocol or schema change): the journal writer now
lives on one persistence worker thread. The worker runs every storage
operation the daemon makes (appends, checkpoints, progress markers, staged
template animations, imported files) one at a time, in the order the daemon
hands them over. It therefore performs the same sequence of writes against
the same journal state as before, so the crash windows and the replay
contract are unchanged. A command's append is still a synchronous job: it is
durable before the command is acknowledged or committed. Three writes are now
background jobs: the early journal fold into the manifest, the idle-progress
fold, and the progress marker. No frame waits for a manifest rewrite or a
marker's disk sync any more. A session command is held back while background
work is in flight, so its append does not queue behind it. It waits at most
one checkpoint, once every 48 commands, or one marker a second. A crash test
SIGKILLs the cutting, recording, and streaming daemon right after the worker
reports that a checkpoint or a marker began. In every run the kill came
before the write finished, and recovery held every acknowledged cut and the
matching Program. `fm-gpu` texture readback now reuses its mapped staging
buffers per size, and a test counts the buffers created. Same 25 s
measurement, three runs per build, before → after (llvmpipe on a shared
host):

| Build | Frames after 2 s, p99 | Frames > 1 frame late | Command frames, p99 | Cut round trip p50 / p99 |
| --- | --- | --- | --- | --- |
| debug | 74–321 → 5.1–5.2 ms | 8–21 → 0–1 | 128–321 → 0.1–8.5 ms | 46–58 / 193–518 → 33–46 / 49–99 ms |
| release | 5.0–43 → 4.9–14 ms | 0–6 → 0–1 | 0.1–47 → 0.1–28 ms | 30–55 / 59–196 → 30–46 / 48–97 ms |

The first 2 s stayed at p99 of about 5 ms, apart from one 180 ms stall
before (release). The single late frame after the change, in each build,
came in a run with a host stall. Group commit was evaluated again and not
implemented for native daemons at this point (the simulated daemon groups
commands since d62c3a1c, and native daemons later did too; see Native group
commit below). The session loop dispatches at most one command per pass, and
each journal record carries the position its realization reaches. A shared
disk sync would therefore need either several commands realized on one frame,
which changes ordering, or an engine API that prepares a command for a frame
several idle frames ahead. The engine cannot predict that frame's position
while transitions, playlists, and cue clocks move state in between, and a
missed prediction would still have to stall the frame. With appends kept off
the frame deadline by the command window, grouping would only raise
throughput past one command per frame, and the loop does not offer that.
Remaining gaps: a daemon-issued command's first attempt (playlist or cue
clock, end of media) does not wait for background work (its retries do), so its append can still queue behind a
checkpoint. A disk sync longer than about a frame still makes its frame late.
llvmpipe frame realization still stalls under host load, and displays were
not measured.

Group commit (no protocol, schema, or journal format change): a simulated
daemon dispatches one message per peer per pass, so one pass can carry
several peers' commands, and each used to pay its own append and disk sync.
Those commands now share one. They run in the pass's round-robin order on a
speculative copy of the control service (`ControlService::speculate`), which
publishes nothing, and their journal records are collected. The records are
then appended as consecutive batches in one transaction
(`JournalWriter::append_batches`), the same rows a batch at a time writes.
Only once that commit is durable does the copy replace the live service
(`ControlService::adopt`), publishing the held events in order, and are the
peers answered. If the append fails, the copy is dropped: no command
applies, no receipt is kept, and every peer in the group is refused as not
durable and may retry. A refused command still journals nothing. A crash
that tears the group's commit drops the whole group on recovery, so no
command is partly applied. LUT and host media imports, which write files
around their append, still run alone. The fairness rule is unchanged: each
peer still gets one message per pass. Measured with four clients each
pipelining 12 cuts (debug, local disk): journal appends 48 → 12, `fsync`
calls for the run (including startup) 60 → 24, all acknowledged in about
290 → 185 ms. Native daemons were not grouped at first: a native command
was dispatched alone, at most one per pass, and realized its own frame.
Its record carries that frame's position, so grouping meant either
realizing several commands on one frame or predicting the frames the
earlier members realize.

Native group commit (no protocol, schema, or journal format change): a
native daemon now takes the first option. At a pass's command turn the
commands queued at the front of every active session's queue (from the
turn's session round the list, one per session per round, each session's
in the order it sent them, at most 8: the group's events reach every live
subscription at once, and 32 cuts' overflowed its 64-event queue and
dropped the peers as slow clients) form one group if the turn's own
command can join and there are at least two. Each runs in order on the
speculative copy of the control service, on the state the one before it
left, without realizing a frame: all begin on the frame the pacer realizes
next, and each record names that frame. Replay already ticks no idle frame
between records that began on one frame, so it reproduces the group
exactly. The records go to the journal in one transaction with one disk
sync; only then is the copy adopted (publishing the members' events in
order), that one frame realized once for the group (waiting for its
deadline, as a single command's is), the native runtime's sources, titles,
documents, and stream targets synced to the project the group left, and
every session answered in its own order, before the pass drains any live
events, so each result still precedes the events of its revision. A group
that cannot be made durable realizes nothing and refuses every member as
not durable. The native catalogue budget is taken before the group, so a
member that changes inputs is vetted for every input change the group made
up to it. A session with several commands in a group has each one's
in-flight slot released before its next is admitted, as when they run one
by one. Stinger, recorder, stream, cue, script, and shortcut commands (which
hand work to the native runtime per command, and a recording start may hold
its acknowledgement) and host media and LUT imports still take a turn alone,
as does a lone command. The intermediate state of a group is not rendered:
two cuts queued together reach air only as the state both leave (every
revision is still published as an event). A client that waits for each result before it sends the next still gets one
command per frame. The group commit also now strips a live fire's Preview
and Program bindings on the simulated daemon, as a fire executed alone
always did (RC-004). Measured with four CLI sessions pipelining cuts
(debug, llvmpipe, 25 fps): 48 cuts took 48 appends and frames and 1.95 s
before (one per frame); 40 cuts now take 5 appends and frames (groups of 8)
and are all acknowledged in about 220–230 ms, and a `SIGKILL` right after the last
acknowledgement restarts with exactly those 40 cuts
(`native_command_bursts_share_passes_and_journal_appends_and_survive_sigkill`).
The disk sync still runs on the loop in front of the group's frame (behind
the same command window): the group is adopted only once durable and its
frame realizes it, so syncing off the loop would mean realizing idle frames
meanwhile and committing on a later frame than the records name, the
prediction problem above. One sync now covers the whole group, so the
loop pays it once per turn instead of once per command. The command window
and the clock retries are unchanged.

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
persistence, audit, signaling, encryption, bandwidth adaptation, or OMT
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
pending until certified adapters pass.

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
and segment, or the live edge. Remaining gaps in the library itself: space
reclamation and file deletion are caller-driven, A/B playback has no real
clock, per-event list overrides and controller mapping are absent, and no
eight-camera storage profile is certified; the end-to-end slice below wires
part of this to a server. `OR-012` and `OR-013` remain planned.

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
immediately and recover only past a hysteresis margin. Remaining gaps: Studio's
Replay panel and the CLI render up to eight recorded angles in an adaptive
grid (Protocol 2.127, decoded and composed by the native daemon), led by the
requested Replay input and its linked A/B partner as channel tiles; controller
bindings are MIDI (real device input) and persist with the key profile, and a
second transport profile drives the second Replay input from channel 2, and
repeated jog keys accelerate from one frame to thirty, though the full typed
controller surface remains; the native daemon samples
its replay root's free space and reports the predicted recording time at the
observed write rate (Protocol 2.125, shown in Studio and the CLI), and a
root below the recorder's 512 MiB floor pauses recording (`storage:low_space`)
until a later sample clears it, and a program recorder whose output filesystem
crosses the same floor stops its segment cleanly with `storage:low_space`
(reported as a disk-full failure); the daemon samples the root drive's SMART
verdict, temperature, and endurance used on a worker thread once a minute and
reports them in the same storage record (Protocol 2.139), shown in Studio and
the CLI; and no storage profile is certified. `OR-014` remains planned.

Current end-to-end slice for item 7: the show persists up to eight highlight
reels of up to 64 clips (Protocol 2.107, schema 83); each clip names a captured
replay event, its angle, trims before and after the event marks, a
25/50/75/100 percent speed, and (Protocol 2.111, schema 84) the frames it
dissolves in from the clip before (0 cuts, at most 600, and the first clip
always cuts), and reel edits are padlocked, undoable project edits.
`fm-replay` compiles a reel into an exact output timeline whose frames
resolve to one or two recorded source layers with exact mix weights (a
dissolve of `n` frames weights its `k`th frame `(k + 1) / (n + 1)` to the
incoming clip and the rest to the outgoing one, so the reel is `n` frames
shorter, and a dissolve longer than either clip is refused at export as
`dissolve_too_long`); a
music bed still plans once-through or looping playlists with linear
crossfades, sample-exact ducking, merged nearby ducks, and an end fade. An
export-during-record queue accepts only gap-free, closed segments, protects
them from retention, advances in bounded frame chunks with progress and
cancellation, and revalidates segments and source frames so an export never
resolves pruned media. The native daemon decodes each resolved segment frame
with ffmpeg, retimes it to the output grid, and encodes a fragmented H.264/AAC
MP4 to `<destination>.part`; once the recorder has stopped cleanly a stream
copy remuxes it to a faststart MP4 (`moov` ahead of `mdat`, no fragments) at
`<destination>.faststart.part`, which is renamed to the destination (a failed
remux fails `output:faststart`). An export starts only when the destination's
filesystem has room for twice a generous size budget (an eighth of a byte
per pixel per frame plus 4 KiB) above the 512 MiB recording floor, else it
fails `output:space` (`output:space_unavailable` when free space cannot be
read) before any part file exists, and the worker fails `output:space` when
free space falls below that floor between chunks. A cancel removes both part
files and releases the protection. The export blends a dissolve's two
decoded layers with their exact weights (the native test exports a red to
blue dissolve and reads red, an even mix, and blue from the file). Studio
edits and exports reels in its Replay panel, each clip with a Dissolve
field (off on the first clip, which a reorder or removal keeps a cut), and
`freemix-cli` has local and remote reel commands (`--clips` records take an
optional `/<dissolve>`) with `remote-reel-export` polling to a terminal
state. An export's audio is each real-time clip's recorded audio of its
frame, weighted like its picture, so a dissolve crossfades it; slow-motion
clips add none (the native test hears the red camera's tone before a
dissolve and silence on the silent camera after it). A reel may carry a
music bed (Protocol 2.113, schema 86): an audio file on the server played
from the first frame at its level, ramped over a quarter second to its
ducked level while real-time clip audio plays, faded over the last second,
and silent once the file ends; an export keeps the bed it was accepted
with, and an unreadable bed fails it `music:open` (the native test mixes a
mono 44.1 kHz bed under a real-time clip and a slow-motion one and hears it
ducked, alone, and faded). Studio's reel editor sets the bed and its two
levels, and `reel-set --music` does in the CLI. A clip can now play its
trimmed range backward (Protocol 2.136, schema 96): the compiled timeline
walks from the range's last frame to its first, Studio's clip row has a
Reverse toggle, and `--clips` appends `r` to the speed; reverse clips
carry no clip audio. Remaining: nothing is certified.
`OR-013` remains planned.

Current end-to-end slice for items 1, 3, and 4: the show persists replay
settings (up to eight recorded inputs in angle order, a retention of 10 seconds
to one hour, and an optional storage root; Protocol 2.106, schema 82). The
engine owns a deterministic replay runtime: marks, Capture and Last 5/10/20
build events (at most 64) over every recorded angle, events can be renamed and
deleted, and up to two Replay inputs cue an event angle, switch angles, play
at 100, 75, 50, or 25 percent, pause, and jump back to the event's in point.
Live commands are ordinary acknowledged commands, but events, marks, and
player positions are server runtime, so a restart starts with none. The
simulated daemon feeds a synthetic segment timeline; the native daemon records
each recorded input's newest decoded picture into rolling two-second H.264 MP4
segment files under `<bundle>/replay` (or the configured root), deletes
segments past retention unless an event protects them, and decodes the cued
angle with ffmpeg so a Replay input presents frame-exact source frames at the
chosen speed (each source frame shown twice at 50 percent). Studio has a
dockable Replay panel (recording, marks, events, and players, with English and
Spanish hover help on every control), creates Replay inputs from Add input,
and disables its recording settings while the project is locked; `freemix-cli`
has local and remote `replay` and `replay-set` plus `remote-replay-do`, with
typed argument validation for sources, retention, and the storage root.
Each recorded frame now carries the source's own audio of that frame
(before its strip, fader, and the mix, as stereo; a source without audio
records silence), and a Replay input playing forward at 100% plays each
shown frame's recorded audio through its own strip, while slow motion and a
paused or cued player stay silent (the unit test reads a recorded tone back
from a segment; the native test hears the camera's tone on Program at 100%
and silence in slow motion). Every segment's audio is read from its first
video frame, which the B-frame delay starts 80 ms after its sound, so
replays and reel exports keep lip sync, and a run's first segment, whose
AAC priming packet ffmpeg's fragmented MP4 muxer leaves ahead of that shift,
decodes from its second packet (the adapter skips a leading packet that
does not run into the next). Closed segments from a previous run are now
reloaded: a session writes `manifest.tsv` with every segment's input,
engine-frame and stream-frame origins, frame count, byte count, and file;
on startup the daemon restores the newest compatible session's segments
(observing every frame so they resolve exactly), continues each source's
stream frames after them, marks its manifest restored, and removes the
other stale sessions. Remaining gaps: slow motion has no audio; the recorder captures
what the source last decoded rather than a genlocked frame, so a stalled
source repeats its picture; a cued player jogs any number of frames up to 600
either way, paused (Protocol 2.114, Studio's ◀ and ▶ keys and `remote-replay-do
step`) or shuttles at a signed rate (Protocol 2.131), and with auto return
on a Replay input that plays its event to the end on Program hands Program
back to the last live source with a cut or a mix (Protocol 2.116, schema
88; the daemon's cue clock issues it on a reserved list, and the native
test hears the return on the wire); console keys play or pause the Replay
player on Preview (else Program), step it, and shuttle it at 2x each way, but
each Replay input carries a playlist of up to sixteen
events, each with its own angle and speed (Protocol 2.119): a playing
player that reaches the end of its event moves on to the next listed entry
that still exists, keeping Program on the replay until the last entry
ends, and Studio's Replay panel and `remote-replay-do playlist`,
`playlist-next`, and `playlist-prev` edit and step it. Two Replay inputs can
be linked for A/B playback (Protocol 2.124), sharing cue, play, pause,
cue-in, and step while keeping their own angles. A player shuttles at a
signed rate from -16x to +16x in thousandths of real time (Protocol 2.131,
`remote-replay-do shuttle`), pausing at either end without auto return, and
reports its rate and shuttling state on the wire; a Replay take now uses the
show's configured transition — a cut or a fade of its own length (Protocol
2.137, schema 97), set in Studio's Replay panel and the CLI — and multiview
and controller wiring remain; storage health reaches
Studio and the CLI (Protocol 2.125), now with the root drive's SMART health
on a worker-thread probe (Protocol 2.139) and a program recorder stopping at
the 512 MiB floor (`storage:low_space`, reported as disk full), and nothing
is certified. `OR-012`, `OR-013`, and `OR-014` remain planned.


Exit: Sports Show scenario passes on certified storage/hardware.

## 10. Phase 8 — professional hardware and advanced formats (ongoing, 16–24 weeks)

Parallel adapters, each with its own certification:

1. Blackmagic capture/output through the FreeMix capture/output SDK, embedded
   audio, key/fill, reference.
2. AJA capture/output through the FreeMix capture/output SDK, embedded audio,
   key/fill, reference.
3. Bluefish444 and prioritized capture families through the same SDK.
4. OMT send/receive, alpha, tally/metadata, direct record where possible.
   FreeMix implements OMT 1.0 itself (`fm-io-omt`), which stays the default
   AV-over-IP path.
5. NDI send/receive, discovery, and tally as an optional runtime-loaded
   vendor adapter (`fm-io-ndi`) that opens the NDI runtime when present.
6. Virtual camera/audio devices with signing/install/uninstall.
7. Genlock/PTP and timecode workflows.
8. Interlaced output and advanced deinterlace.
9. 10-bit HDR ingest, compose, scopes, record, stream, display.
10. FreeMix plugin host through the in-tree plugin ABI; no VST3 license or host.
11. UHD Profile B certification and multi-GPU policy if benchmarks justify it.

Current implementation boundary for items 1-5 and 10: `fm-io-omt` is
FreeMix's own OMT 1.0 implementation, written from the published protocol.
It provides the 16-byte frame header, video and audio extended headers, the
raw `UYVY`, `YUY2`, `BGRA`, `NV12`, `YV12`, `UYVA`, `P216`, and `PA16`
codecs with alpha and preview flags, planar `FPA1` audio, per-frame
metadata, the exact subscribe/preview/tally/quality/info commands, the
`_omt._tcp` DNS-SD record, and the discovery server and client registry. A
sender fans frames out only to subscribed receivers, chooses a receiver's
one-eighth preview frame, combines every receiver's tally, and reports the
strongest suggested quality; a loopback test covers two receivers, gated
video/audio, combined tally, quality, and preview, and `OmtSink` implements
`fm-io-api`'s `MediaSink` with format advertisement, lifecycle, BGRA, YUV
4:2:2 and NV12 conversion, and planar audio, and `OmtSource` implements
`fm-io-api`'s `MediaSource` with subscription, tally and quality control, and
the inverse conversion with 100 ns timing. A native daemon reads an `omt://`
network stream input through the receiver and converts BGRA, UYVY, or NV12
pictures to program RGBA and stereo audio (Protocol 2.121); a stream
destination with `stream_protocol=omt` binds an OMT sender and publishes the
same raw Program pair after converting it to BGRA and planar audio
(Protocol 2.122); no discovery catalogue entry is wired yet; a corrupt
payload with an intact declared length is skipped as a dropped frame so the
stream continues; VMX-compressed frames and hardware certification remain. FreeMix's plugin
SDK is `fm-plugin-api` with the `fm-plugin-host` sandbox (manifest,
versioned ABI, capability grants, state snapshots, quarantine); the host API
now also models an ordered plugin chain (up to eight unique plugins, bypass,
per-slot latency and its effective sum, per-plugin state snapshots), and the
native daemon renders a chain's `freemix.reference.*` processors on input
strips and the summed Master bus (Protocol 2.128), the CLI sends a chain or
edits a project's chain offline (`audio-plugin-chain <project>
<input|master> ...`), and Studio's FX window reads the open target's chain
(Protocol 2.129) and edits its PLUGINS section. Chains are padlocked, undoable project edits
persisted since schema 91 (now 92) on every strip and on Master, so a show carries them
through a save, a restart, and a journal replay; Studio edits the trim gain
and soft-clip threshold of each reference slot, and a Master plugin stage
that panics mid-render is quarantined while the Program bus keeps its sum.
There is no VST3 host; a native daemon scans `<bundle>/plugins` manifests and
lists them in `audio_plugin_status`, but parameters for plugins beyond the
reference set, loading, and plugging a real sandboxed plugin into the chain
remain. Vendor capture and output adapters keep their SDKs out of the
portable workspace and implement the in-tree `fm-io-api` contracts.

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
8. Decide and implement the version compatibility policy section 1 defers:
   a project schema the release reads from at least one schema back with a
   `freemix-cli` upgrade command, a protocol window a released Studio and
   daemon negotiate on `ServerHello`, and fixtures per released schema from
   that release on. Until then every `.freemix` project and every client
   is tied to one build.

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
environment choose the start locale. Every console panel now uses the
catalogs, the language and clock choice persist in `studio.conf`, typed
refusals and output-failure codes are worded in the console language, and the
web gateway's operator and tally pages are offered in English and Spanish.
Remaining gaps: daemon text that carries no typed refusal or failure code
stays English, there is no RTL catalog, font, or bidi pass (layout helpers
are already direction-aware), and no accessibility audit. `RC-016` remains planned.

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
