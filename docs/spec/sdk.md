# FreeMix SDKs

[Specification index](README.md)

FreeMix ships its own versioned SDKs instead of depending on licensed
vendor SDKs. The portable workspace builds, tests, and runs their reference
paths without any third-party SDK, header, or runtime library.

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

VST3, AU, and LV2 are not part of the contract and no VST3 license or host is
required. The plugin ABI is the supported extension surface; a future adapter
may host another format out of tree without changing the portable crates.

## 2. Device I/O SDK

`fm-io-api` is the contract every capture, output, and network media adapter
implements: discovery, formats, clocks, health, hot-plug, source/sink
factories, and bounded error reporting. Platform crates implement it directly
(`fm-io-linux`, `fm-io-macos`, `fm-io-network`, `fm-io-srt`, `fm-io-webrtc`).

Vendor hardware adapters (Blackmagic, AJA, Bluefish444, and similar) are
optional and outside the portable workspace: they implement the same
`fm-io-api` contracts in their own crate and runner, and keep vendor headers
and runtime dependencies out of the core crates. The capability matrix records
which adapter paths are built and certified; a missing vendor adapter never
prevents a build.

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
  `HOSTNAME (Source Name)._omt._tcp.local`, `OMTAddress` register/removal
  records, and a discovery server that replaces reported addresses with the
  observed client address, repeats every registration to every client, and
  sends removals on deregistration or disconnect.
- **Sender:** one TCP listener, a session per receiver, subscription gating,
  receiver-requested one-eighth preview frames, the combined tally from every
  receiver, the strongest suggested quality, and sender information.
- **Receiver:** subscribe, preview, tally, quality, and frame reception with
  timeouts.

Every decode checks its version, frame type, lengths, extended headers, codec,
alpha flags, channel count, active-channel mask, and null termination before
returning data. The loopback tests cover two receivers, gated video and audio,
combined tally, quality, preview selection, discovery registration/removal, and
malformed frames.

Not yet wired: `fm-io-api` source/sink adapters and a daemon input/output, the
VMX-compressed codec, DNS-SD multicast advertisement, corrupt-frame recovery
statistics, and hardware certification. The roadmap tracks them under
`IN-018`/`IN-019`.

## 4. Versioning and change policy

- One current contract per SDK: no compatibility projections, old-version
  fixtures, or migration paths during current development.
- Breaking a record replaces it in place and bumps its schema or ABI version;
  every consumer updates together.
- SDK records are bounded and validated at every boundary; unknown or oversized
  input is refused with a typed status, never truncated silently.
- Vendor SDKs may implement these contracts out of tree but never leak headers,
  symbol tables, or runtime dependencies into portable crates.
