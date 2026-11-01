# FreeMix SDKs

[Specification index](README.md)

FreeMix ships its own versioned SDKs instead of depending on licensed
vendor SDKs. The portable workspace builds, tests, and runs their reference
paths without any third-party SDK, header, or runtime library. Optional vendor
adapters (section 2) open their vendor's runtime only at run time.

## 1. Plugin SDK

| Crate/app | Role |
|---|---|
| `fm-plugin-api` | Dependency-free, `no_std` records for the manifest, status codes, bounded values, handles, and ABI version negotiation. Every record has fixed-width fields so a generated WIT or C ABI carries the same model. |
| `fm-plugin-host-api` | Engine-side lifecycle and command-only mutation: `PluginHost`, manifest validation, capability grants, state snapshots, crash reports, quarantine, and the plugin-to-engine exchange protocol. |
| `fm-plugin-host` | Process supervisor: discovery, IPC, sandboxing, and crash quarantine. |
| `freemix-plugin-host` | Standalone host process a plugin runs inside. |

A plugin declares a `PluginManifest` with an `ApiVersion`, requests
capabilities, and exchanges bounded `DataMessage`/`EventMessage` batches. The
engine applies mutations only as ordinary commands with idempotency keys,
revisions, and durable events; it never links a plugin into its address space.
A failed or quarantined plugin is reported and restarted without taking the
show down.

A plugin chain (`PluginChain`, `PluginSlot`) is the ordered processing path of
one audio or video bus: up to eight unique plugins, each bypassable, each
reporting up to 60,000 frames of latency, and each holding its plugin's last
`StateSnapshot`. `effective_latency_frames` sums the active slots so the
engine can delay parallel paths to match; bypassing a slot drops its latency
and removes it from `active_plugins`. Insert, move, remove, bypass, latency,
and snapshot edits are typed and bounded, and a snapshot from another plugin
is refused. A chain is project state: setting one is padlocked and undoable,
and the show persists every strip's and Master's chain at the current schema (since 91), so the
chain survives a save, a restart, and a journal replay.

The in-tree reference processors are `freemix.reference.trim` (a fader whose
snapshot is a little-endian `i32` in millidB), `freemix.reference.polarity`,
and `freemix.reference.softclip` (a cubic knee whose snapshot is its
threshold in millidB). `ReferenceRack` builds one chain's processors, reports
its latency, and processes planar blocks; the native daemon runs input-strip
racks before the mixer and the Master rack inside it after the Master DSP
chain, before clipping and the Master meter.

VST3, AU, and LV2 are not part of the contract and no VST3 license or host is
required. The plugin ABI is the supported extension surface; a future adapter
may host another format out of tree without changing the portable crates.

## 2. Device I/O SDK

`fm-io-api` is the contract every capture, output, and network media adapter
implements: discovery, formats, clocks, health, hot-plug, source/sink
factories, and bounded error reporting. Platform crates implement it directly
(`fm-io-linux`, `fm-io-macos`, `fm-io-network`, `fm-io-srt`, `fm-io-webrtc`).

Vendor adapters (NDI, Blackmagic, AJA, Bluefish444, and similar) are
optional: each implements the same `fm-io-api` contracts in its own crate.
An adapter may live in this workspace when it opens the vendor's shared
library at run time
([platform-portability.md](platform-portability.md) section 4), so the
build needs no vendor SDK and a missing runtime reports the capability
unsupported; otherwise it lives out of tree in its own runner. Either way
vendor types and runtime dependencies stay out of the core crates. The
capability matrix records which adapter paths are built and certified; a
missing vendor adapter or runtime never prevents a build.

## 3. OMT transport

`fm-io-omt` is FreeMix's own implementation of Open Media Transport (OMT) 1.0,
written from the published protocol (the protocol reference is MIT licensed):
fully open, royalty-free, and in-tree.

- **Frames:** the 16-byte header, 32-byte video and 24-byte audio extended
  headers, little-endian fields, bounded payloads, and the raw `UYVY`, `YUY2`,
  `BGRA`, `NV12`, `YV12`, `UYVA`, `P216`, and `PA16` codecs with alpha,
  premultiplied alpha, high-bit-depth, preview, and interlaced flags; planar
  32-bit float `FPA1` audio; UTF-8 null-terminated per-frame metadata.
- **Control metadata:** the exact `OMTSubscribe`, `OMTSettings Preview`,
  `OMTSettings Quality`, `OMTInfo`, and `OMTTally` strings, including the
  historical `Program=="false"` spelling the protocol keeps for compatibility.
- **Discovery:** `_omt._tcp` names of the form
  `HOSTNAME (Source Name)._omt._tcp.local` advertised over DNS-SD
  (`OmtAdvertiser` and `browse`), `OMTAddress` register/removal records, and
  a discovery server that replaces reported addresses with the observed
  client address, repeats every registration to every client, and sends
  removals on deregistration or disconnect.
- **Sender:** one TCP listener, a session per receiver, subscription gating,
  receiver-requested one-eighth preview frames, the combined tally from every
  receiver, the strongest suggested quality, and sender information.
- **Receiver:** subscribe, preview, tally, quality, and frame reception with
  timeouts.
- **Device I/O adapters:** `OmtSink` implements `fm_io_api::MediaSink`,
  advertising one concrete `video.omt` format and converting CPU YUV 4:2:2,
  BGRA, or NV12 frames and planar audio to the OMT frames a subscribed
  receiver sees. `OmtSource` implements `fm_io_api::MediaSource`, subscribes
  to the same format, and converts received frames back to CPU payloads and
  planar audio with 100 ns-derived timing. Lifecycle, health, tally, quality,
  and write errors follow the shared `fm-io-api` contract.

Every decode checks its version, frame type, lengths, extended headers, codec,
alpha flags, channel count, active-channel mask, and null termination before
returning data. The loopback tests cover two receivers, gated video and audio,
combined tally, quality, preview selection, discovery registration/removal, and
malformed frames.

The daemon registers the sink as an `omt://` stream destination (Protocol
2.122): the shared output set packs the same raw RGBA/audio pair as every
stream and the OMT transport converts it to BGRA and planar audio for
subscribed receivers. Not yet wired: a discovery catalogue entry, the
VMX-compressed codec, corrupt-frame recovery statistics, and hardware
certification. The roadmap tracks them under `IN-018`/`IN-019`.

## 4. Versioning and change policy

- One current contract per SDK: no compatibility projections, old-version
  fixtures, or migration paths during current development.
- Breaking a record replaces it in place and bumps its schema or ABI version;
  every consumer updates together.
- SDK records are bounded and validated at every boundary; unknown or oversized
  input is refused with a typed status, never truncated silently.
- Vendor adapters implement these contracts in-tree (runtime-loaded) or out of
  tree, but never leak vendor types, headers, or link-time runtime
  dependencies into portable crates.
