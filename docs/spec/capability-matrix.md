# Development capability matrix

Generated from `parity.toml`; run `cargo run -p xtask -- matrix write` to regenerate.
No capability profile is certified. This matrix reports ledger status and
local acceptance-test evidence only.

Kind `model` marks a feature whose implemented surface is a deterministic
model with no device, socket, process, or browser adapter; its acceptance test
covers contracts only, so the row is not a usable feature. Kind `adapter` marks
a feature with at least one real integration.

## windows-tier1

Total: 106 features. Status: planned=106. Priority: P0=24, P1=73, P2=9.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-001 | adapter | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (present) |
| IN-002 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | adapter | P0 | 2 | planned | codecs-network-recording | accept-in-003 (present) |
| IN-004 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | adapter | P1 | 3 | planned | audio-dsp | accept-in-005 (present) |
| IN-006 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-007 | adapter | P2 | 9 | planned | codecs-network-recording | accept-in-007 (planned) |
| IN-008 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (present) |
| IN-009 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (present) |
| IN-011 | adapter | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (present) |
| IN-012 | adapter | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | model | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (present) |
| IN-015 | adapter | P0 | 4 | planned | codecs-network-recording | accept-in-015 (present) |
| IN-016 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-016 (present) |
| IN-017 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-017 (present) |
| IN-018 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (present) |
| IN-021 | adapter | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | adapter | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | adapter | P1 | 3 | planned | media-core-clocks | accept-in-024 (present) |
| IN-025 | adapter | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (present) |
| IN-026 | adapter | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (present) |
| IN-027 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | model | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | model | P2 | 6 | planned | replay-guests | accept-in-029 (planned) |
| IN-030 | adapter | P2 | 9 | planned | platform-professional-hardware | accept-in-030 (planned) |
| SW-001 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (present) |
| SW-002 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (present) |
| SW-003 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (present) |
| SW-004 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (present) |
| SW-005 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (present) |
| SW-006 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (present) |
| SW-007 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | adapter | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (present) |
| SW-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | adapter | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (present) |
| SW-016 | adapter | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (present) |
| SW-019 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-019 (present) |
| GX-001 | adapter | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | adapter | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | model | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-010 (present) |
| GX-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-011 (present) |
| AU-001 | adapter | P0 | 3 | planned | audio-dsp | accept-au-001 (present) |
| AU-002 | adapter | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | adapter | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | adapter | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | adapter | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | adapter | P1 | 8 | planned | audio-dsp | accept-au-006 (present) |
| AU-007 | adapter | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | adapter | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | adapter | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | adapter | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | adapter | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (present) |
| OR-001 | adapter | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-003 (present) |
| OR-004 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-006 (present) |
| OR-007 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | adapter | P1 | 8 | planned | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | adapter | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (present) |
| OR-012 | adapter | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | adapter | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | adapter | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | adapter | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | adapter | P1 | 4 | planned | studio-web-ux | accept-or-017 (present) |
| RC-001 | model | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | model | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | model | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | adapter | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | adapter | P1 | 4 | planned | studio-web-ux | accept-rc-007 (present) |
| RC-008 | adapter | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | model | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (present) |
| RC-012 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | adapter | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | adapter | P2 | 9 | planned | studio-web-ux | accept-rc-016 (present) |
| RC-017 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (present) |
| RC-018 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-018 (present) |

## macos-tier1

Total: 104 features. Status: planned=104. Priority: P0=24, P1=73, P2=7.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-001 | adapter | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (present) |
| IN-002 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | adapter | P0 | 2 | planned | codecs-network-recording | accept-in-003 (present) |
| IN-004 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | adapter | P1 | 3 | planned | audio-dsp | accept-in-005 (present) |
| IN-006 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-008 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (present) |
| IN-009 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (present) |
| IN-011 | adapter | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (present) |
| IN-012 | adapter | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | model | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (present) |
| IN-015 | adapter | P0 | 4 | planned | codecs-network-recording | accept-in-015 (present) |
| IN-016 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-016 (present) |
| IN-017 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-017 (present) |
| IN-018 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (present) |
| IN-021 | adapter | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | adapter | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | adapter | P1 | 3 | planned | media-core-clocks | accept-in-024 (present) |
| IN-025 | adapter | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (present) |
| IN-026 | adapter | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (present) |
| IN-027 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | model | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | model | P2 | 6 | planned | replay-guests | accept-in-029 (planned) |
| SW-001 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (present) |
| SW-002 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (present) |
| SW-003 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (present) |
| SW-004 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (present) |
| SW-005 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (present) |
| SW-006 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (present) |
| SW-007 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | adapter | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (present) |
| SW-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | adapter | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (present) |
| SW-016 | adapter | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (present) |
| SW-019 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-019 (present) |
| GX-001 | adapter | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | adapter | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | model | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-010 (present) |
| GX-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-011 (present) |
| AU-001 | adapter | P0 | 3 | planned | audio-dsp | accept-au-001 (present) |
| AU-002 | adapter | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | adapter | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | adapter | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | adapter | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | adapter | P1 | 8 | planned | audio-dsp | accept-au-006 (present) |
| AU-007 | adapter | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | adapter | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | adapter | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | adapter | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | adapter | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (present) |
| OR-001 | adapter | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-003 (present) |
| OR-004 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-006 (present) |
| OR-007 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | adapter | P1 | 8 | planned | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | adapter | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (present) |
| OR-012 | adapter | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | adapter | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | adapter | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | adapter | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | adapter | P1 | 4 | planned | studio-web-ux | accept-or-017 (present) |
| RC-001 | model | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | model | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | model | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | adapter | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | adapter | P1 | 4 | planned | studio-web-ux | accept-rc-007 (present) |
| RC-008 | adapter | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | model | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (present) |
| RC-012 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | adapter | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | adapter | P2 | 9 | planned | studio-web-ux | accept-rc-016 (present) |
| RC-017 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (present) |
| RC-018 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-018 (present) |

## linux-tier1

Total: 104 features. Status: planned=104. Priority: P0=24, P1=73, P2=7.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-001 | adapter | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (present) |
| IN-002 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | adapter | P0 | 2 | planned | codecs-network-recording | accept-in-003 (present) |
| IN-004 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | adapter | P1 | 3 | planned | audio-dsp | accept-in-005 (present) |
| IN-006 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-008 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (present) |
| IN-009 | adapter | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (present) |
| IN-011 | adapter | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (present) |
| IN-012 | adapter | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | model | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (present) |
| IN-015 | adapter | P0 | 4 | planned | codecs-network-recording | accept-in-015 (present) |
| IN-016 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-016 (present) |
| IN-017 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-017 (present) |
| IN-018 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (present) |
| IN-021 | adapter | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | adapter | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | adapter | P1 | 3 | planned | media-core-clocks | accept-in-024 (present) |
| IN-025 | adapter | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (present) |
| IN-026 | adapter | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (present) |
| IN-027 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | model | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | model | P2 | 6 | planned | replay-guests | accept-in-029 (planned) |
| SW-001 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (present) |
| SW-002 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (present) |
| SW-003 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (present) |
| SW-004 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (present) |
| SW-005 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (present) |
| SW-006 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (present) |
| SW-007 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | adapter | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (present) |
| SW-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | adapter | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | adapter | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (present) |
| SW-016 | adapter | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (present) |
| SW-019 | adapter | P1 | 5 | planned | gpu-compositor-color | accept-sw-019 (present) |
| GX-001 | adapter | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | adapter | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | adapter | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | model | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-010 (present) |
| GX-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-011 (present) |
| AU-001 | adapter | P0 | 3 | planned | audio-dsp | accept-au-001 (present) |
| AU-002 | adapter | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | adapter | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | adapter | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | adapter | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | adapter | P1 | 8 | planned | audio-dsp | accept-au-006 (present) |
| AU-007 | adapter | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | adapter | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | adapter | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | adapter | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | adapter | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (present) |
| OR-001 | adapter | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-003 (present) |
| OR-004 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-006 (present) |
| OR-007 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | adapter | P1 | 8 | planned | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | adapter | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (present) |
| OR-012 | adapter | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | adapter | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | adapter | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | adapter | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | adapter | P1 | 4 | planned | studio-web-ux | accept-or-017 (present) |
| RC-001 | model | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | model | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | model | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | adapter | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | adapter | P1 | 4 | planned | studio-web-ux | accept-rc-007 (present) |
| RC-008 | adapter | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | model | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (present) |
| RC-012 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | adapter | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | adapter | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | adapter | P2 | 9 | planned | studio-web-ux | accept-rc-016 (present) |
| RC-017 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (present) |
| RC-018 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-018 (present) |

## browser-control

Total: 11 features. Status: planned=11. Priority: P0=1, P1=8, P2=2.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| GX-006 | adapter | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | adapter | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-010 | model | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-010 (present) |
| OR-017 | adapter | P1 | 4 | planned | studio-web-ux | accept-or-017 (present) |
| RC-001 | model | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | model | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | model | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-007 | adapter | P1 | 4 | planned | studio-web-ux | accept-rc-007 (present) |
| RC-008 | adapter | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-014 | adapter | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-016 | adapter | P2 | 9 | planned | studio-web-ux | accept-rc-016 (present) |

## profile-a-hd

Total: 24 features. Status: planned=24. Priority: P0=23, P1=1.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-001 | adapter | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (present) |
| IN-003 | adapter | P0 | 2 | planned | codecs-network-recording | accept-in-003 (present) |
| IN-008 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (present) |
| IN-015 | adapter | P0 | 4 | planned | codecs-network-recording | accept-in-015 (present) |
| IN-020 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (present) |
| IN-026 | adapter | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (present) |
| SW-001 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (present) |
| SW-002 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (present) |
| SW-004 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (present) |
| SW-005 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (present) |
| SW-006 | adapter | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (present) |
| SW-007 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-011 | adapter | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| GX-001 | adapter | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| AU-001 | adapter | P0 | 3 | planned | audio-dsp | accept-au-001 (present) |
| AU-002 | adapter | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | adapter | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | adapter | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | adapter | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| OR-001 | adapter | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-005 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-011 | adapter | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (present) |
| RC-004 | adapter | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |

## profile-b-uhd

Total: 5 features. Status: planned=5. Priority: P1=5.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-004 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-017 | adapter | P1 | 4 | planned | codecs-network-recording | accept-in-017 (present) |
| SW-009 | adapter | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (present) |
| SW-010 | adapter | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| OR-007 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |

## profile-c-replay

Total: 4 features. Status: planned=4. Priority: P1=4.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| OR-004 | adapter | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-012 | adapter | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | adapter | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | adapter | P1 | 7 | planned | replay-guests | accept-or-014 (present) |

## vendor-certified

Total: 12 features. Status: planned=12. Priority: P1=9, P2=3.

| ID | Kind | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|---|
| IN-002 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-002 (planned) |
| IN-018 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | adapter | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-028 | model | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | model | P2 | 6 | planned | replay-guests | accept-in-029 (planned) |
| GX-009 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | model | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-010 (present) |
| AU-006 | adapter | P1 | 8 | planned | audio-dsp | accept-au-006 (present) |
| OR-008 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | adapter | P1 | 8 | planned | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | adapter | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-016 | adapter | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
