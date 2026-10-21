# Security and reliability

[Specification index](README.md)

## 1. Assets and trust boundaries

Protected assets:

- control authority over on-air outputs;
- stream keys, guest links, vendor tokens, TURN credentials, and certificates;
- camera/microphone/screen content;
- project media, chat, recordings, and replay;
- plugin execution and update channel;
- audit and recovery records.

Trust boundaries exist at every remote client, guest browser, network source,
media parser, native SDK, plugin, project bundle, and update package.

## 2. Secure defaults

- Bind control services to loopback until remote access is enabled.
- Use TLS for all non-loopback control and signaling.
- Pair explicitly; no default password.
- Use short-lived scoped access tokens.
- Store secrets in OS credential storage or an encrypted service vault.
- Redact secrets and private media paths from logs/support bundles.
- Disable browser input navigation to local files/internal networks by default.
- Treat media files, captions, fonts, title packages, and project bundles as
  untrusted input.
- Verify signed updates and plugins before load.

The Web gateway's per-install local root CA (`--web-tls`) carries critical
X.509 name constraints that permit only the private, carrier-grade NAT,
link-local, and loopback address ranges, the `.local`, `localhost`,
`home.arpa`, and `.internal` names, and the host names and addresses the
gateway had when the root was made. A phone that installed the root
therefore rejects any certificate a stolen root key mints for a public site
(WebPKI-verified in unit tests). Roots made before the constraints are kept,
never silently replaced, and stay unconstrained until the operator removes
them. Over HTTPS the gateway sends a one-week `Strict-Transport-Security`
(this host only, no `includeSubDomains` or `preload`) for DNS names its
certificate names, never for addresses, `localhost`, or plain HTTP, and its
pages forbid framing, MIME sniffing, and referrers.

Network-facing listeners bound their abuse. An RTMP ingest listener locks out
a peer address (an IPv6 peer by its `/64` prefix) that presents a wrong stream
key three times in a row, for 1 s doubling per further failure up to 300 s,
closes a locked-out connection before starting any decoder, remembers at most
1024 addresses, and logs each lockout (`FREEMIXD_NETWORK_LOCKOUT`) without the
key tried. The Web gateway lets one non-loopback peer address hold at most 8
of its 24 connection threads at once and closes the next connection before any
handshake.

Stream keys, SRT stream IDs, and SRT passphrases never appear on a media
child's command line on Unix, where any local user can read it
(`/proc/<pid>/cmdline`, `ps`): network input children read their URL from a
`concat` script on stdin, and stream output children read the secret protocol
options from a preset on stdin (`-fpre /dev/stdin`). Windows, which does not
show other users' command lines, still passes the output key in argv.

## 3. Roles and dangerous actions

Authorization is command-level and resource-aware. A graphics operator may
change assigned titles but not outputs. A replay operator may control replay
without reading stream keys.

Dangerous actions include:

- stopping all outputs;
- deleting replay/recording media;
- replacing an on-air project;
- installing/enabling native plugins;
- changing network exposure, users, or certificates;
- overwriting an existing portable bundle.

They require both permission and the configured confirmation policy. Automation
can receive narrowly scoped preauthorization; it is auditable and revocable.

Automation never escalates a session's authority. Running a project script
(`RC-011`) or firing a cue (`RC-017`) requires, besides its own `transition`
permission, the permission every action it will perform would need as a
direct command, checked before anything is applied and refused
`permission_denied` naming the missing permission. A script is checked
against the run planned on a copy of the show, so its dynamic branches are
covered; a cue GO is checked for the whole chain it arms (waits and
auto-follows), which the daemon's cue clock later runs on that up-front
authority; and an edit to a list whose chain is armed needs whatever
permission it adds to that chain. A cue step that runs a script may run on a
later frame whose show (and so the script's branches) is unknown when the
chain is armed, so the GO needs the permission of every action verb the
script holds in any branch; an edit of a script an armed chain runs needs
whatever permission its new verbs add, and a script a cue runs cannot be
removed. A script that waits (Protocol 2.62) runs its later parts on the
authority's clock against a show unknown when it starts, so a direct run of
it likewise needs the permission of every action verb its source holds, up
front; a running script cannot be edited until its run ends or is stopped.
Scripts cannot fire cues or other scripts, so automation never
nests deeper than a cue running one script.

## 4. Guest and browser isolation

Guest invitations are random, short-lived, single-slot credentials. Admission,
device permissions, chat, return feeds, and data-channel controls are separate
authorizations. WebRTC uses encrypted media; TURN credentials are ephemeral.

Browser inputs run in isolated profiles/processes. Their network policy can
block loopback, RFC1918, metadata-service, and file access. Downloads, popups,
camera/microphone, clipboard, and arbitrary protocol handlers are denied unless
explicitly allowed.

## 5. Plugin isolation

Plugin classes:

1. Wasm components for automation/data/control with declared filesystem,
   network, clock, and command capabilities; memory, fuel, and deadline limits.
2. Validated WGSL shader packages with bounded resources and no arbitrary native
   GPU API.
3. Native device/codec plugins behind a versioned C ABI, out of process by
   default, with platform shared-surface bridges when zero-copy is required.
4. Real-time DSP plugins in a dedicated shared-memory host with a fixed
   one-audio-block latency, preallocated rings, deadline reporting, crash bypass
   ramp, and plugin-latency compensation. Audited built-in DSP may run in
   process; third-party in-process mode is an explicit unsafe low-latency
   deployment profile, never the default.

Plugins submit commands; they cannot mutate engine state directly. Plugin state
is namespaced and versioned. A crash, timeout, protocol violation, or excessive
resource use quarantines only that plugin and activates bypass/fallback.

## 6. Project and media safety

- Project manifests and journals are checksummed and size-limited.
- Bundle extraction rejects traversal, symlink escape, device files, and zip
  bombs.
- Media probing/decoding runs with resource limits; risky parsers may run out of
  process.
- Fonts and title packages are validated before production use.
- Asset URLs have allowlists, timeouts, response-size limits, and cache policy.
- Database/data-source credentials remain secret references.

## 7. Recording durability

- Warn by projected time-to-full, not only free-space threshold.
- Write recovery metadata to a separate small journal.
- Use fault-tolerant/fragmented containers or short segments where appropriate.
- Flush indexes/metadata at a configurable interval.
- Never delete a user recording automatically.
- Replay rolling deletion follows retention policy and protects referenced
  events.
- A repair tool works without starting the full engine.

## 8. Watchdogs and recovery

Internal heartbeats monitor audio callback progress, render ticks, encoder
progress, recorder write progress, and control responsiveness. A process
supervisor restarts a dead engine only under explicit service policy; it does
not blindly restart-loop into device contention.

Recovery levels:

1. restart failed source/sink;
2. bypass failed effect/plugin;
3. rebuild graph plan;
4. recreate GPU device;
5. controlled engine restart with project journal;
6. manual intervention after a bounded attempt count.

All recovery steps emit a reason, attempt number, downtime, and outcome.

## 9. Audit and privacy

Audit events record actor, command class, target IDs, accepted revision, time,
and result. They do not store secret values or unnecessary guest content.
Retention and export are configurable. The product documents what optional
telemetry leaves the machine and keeps it disabled without consent.

## 10. Security release gate

Before public release:

- threat model updated for the release surface;
- dependency and license audit complete;
- fuzzing covers protocol, project, title/data, and media metadata parsers;
- TLS/auth/pairing and authorization integration tests pass;
- plugins and updates verify signatures;
- support bundle redaction tests pass; and
- external penetration test covers remote control, guest join, browser input,
  project bundles, and plugin installation.
