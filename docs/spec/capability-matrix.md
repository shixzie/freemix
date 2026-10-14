# Development capability matrix

Generated from `parity.toml`; run `cargo run -p xtask -- matrix write` to regenerate.
No capability profile is certified. This matrix reports ledger status and
local acceptance-test evidence only.

## windows-tier1

Total: 103 features. Status: planned=96, blocked=7. Priority: P0=24, P1=70, P2=9.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-001 | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (planned) |
| IN-002 | P1 | 8 | blocked | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | P0 | 2 | planned | codecs-network-recording | accept-in-003 (planned) |
| IN-004 | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | P1 | 3 | planned | audio-dsp | accept-in-005 (planned) |
| IN-006 | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-007 | P2 | 9 | planned | codecs-network-recording | accept-in-007 (planned) |
| IN-008 | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (planned) |
| IN-009 | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (planned) |
| IN-011 | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (planned) |
| IN-012 | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (planned) |
| IN-015 | P0 | 4 | planned | codecs-network-recording | accept-in-015 (planned) |
| IN-016 | P1 | 4 | planned | codecs-network-recording | accept-in-016 (planned) |
| IN-017 | P1 | 4 | planned | codecs-network-recording | accept-in-017 (planned) |
| IN-018 | P1 | 8 | blocked | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (planned) |
| IN-021 | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | P1 | 3 | planned | media-core-clocks | accept-in-024 (planned) |
| IN-025 | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (planned) |
| IN-026 | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (planned) |
| IN-027 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | P2 | 6 | blocked | replay-guests | accept-in-029 (planned) |
| IN-030 | P2 | 9 | planned | platform-professional-hardware | accept-in-030 (planned) |
| SW-001 | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (planned) |
| SW-002 | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (planned) |
| SW-003 | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (planned) |
| SW-004 | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (planned) |
| SW-005 | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (planned) |
| SW-006 | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (planned) |
| SW-007 | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (planned) |
| SW-010 | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (planned) |
| SW-016 | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (planned) |
| GX-001 | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | P2 | 9 | blocked | graphics-data-automation-plugins | accept-gx-010 (present) |
| AU-001 | P0 | 3 | planned | audio-dsp | accept-au-001 (planned) |
| AU-002 | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | P1 | 8 | blocked | audio-dsp | accept-au-006 (planned) |
| AU-007 | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (planned) |
| OR-001 | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | P1 | 4 | planned | codecs-network-recording | accept-or-003 (planned) |
| OR-004 | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | P1 | 4 | planned | codecs-network-recording | accept-or-006 (planned) |
| OR-007 | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | P1 | 8 | blocked | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | P1 | 8 | blocked | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (planned) |
| OR-012 | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | P1 | 4 | planned | studio-web-ux | accept-or-017 (planned) |
| RC-001 | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | P1 | 4 | planned | studio-web-ux | accept-rc-007 (planned) |
| RC-008 | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (planned) |
| RC-012 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | P2 | 9 | planned | studio-web-ux | accept-rc-016 (planned) |
| RC-017 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (planned) |

## macos-tier1

Total: 101 features. Status: planned=94, blocked=7. Priority: P0=24, P1=70, P2=7.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-001 | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (planned) |
| IN-002 | P1 | 8 | blocked | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | P0 | 2 | planned | codecs-network-recording | accept-in-003 (planned) |
| IN-004 | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | P1 | 3 | planned | audio-dsp | accept-in-005 (planned) |
| IN-006 | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-008 | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (planned) |
| IN-009 | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (planned) |
| IN-011 | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (planned) |
| IN-012 | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (planned) |
| IN-015 | P0 | 4 | planned | codecs-network-recording | accept-in-015 (planned) |
| IN-016 | P1 | 4 | planned | codecs-network-recording | accept-in-016 (planned) |
| IN-017 | P1 | 4 | planned | codecs-network-recording | accept-in-017 (planned) |
| IN-018 | P1 | 8 | blocked | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (planned) |
| IN-021 | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | P1 | 3 | planned | media-core-clocks | accept-in-024 (planned) |
| IN-025 | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (planned) |
| IN-026 | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (planned) |
| IN-027 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | P2 | 6 | blocked | replay-guests | accept-in-029 (planned) |
| SW-001 | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (planned) |
| SW-002 | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (planned) |
| SW-003 | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (planned) |
| SW-004 | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (planned) |
| SW-005 | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (planned) |
| SW-006 | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (planned) |
| SW-007 | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (planned) |
| SW-010 | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (planned) |
| SW-016 | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (planned) |
| GX-001 | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | P2 | 9 | blocked | graphics-data-automation-plugins | accept-gx-010 (present) |
| AU-001 | P0 | 3 | planned | audio-dsp | accept-au-001 (planned) |
| AU-002 | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | P1 | 8 | blocked | audio-dsp | accept-au-006 (planned) |
| AU-007 | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (planned) |
| OR-001 | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | P1 | 4 | planned | codecs-network-recording | accept-or-003 (planned) |
| OR-004 | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | P1 | 4 | planned | codecs-network-recording | accept-or-006 (planned) |
| OR-007 | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | P1 | 8 | blocked | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | P1 | 8 | blocked | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (planned) |
| OR-012 | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | P1 | 4 | planned | studio-web-ux | accept-or-017 (planned) |
| RC-001 | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | P1 | 4 | planned | studio-web-ux | accept-rc-007 (planned) |
| RC-008 | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (planned) |
| RC-012 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | P2 | 9 | planned | studio-web-ux | accept-rc-016 (planned) |
| RC-017 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (planned) |

## linux-tier1

Total: 101 features. Status: planned=94, blocked=7. Priority: P0=24, P1=70, P2=7.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-001 | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (planned) |
| IN-002 | P1 | 8 | blocked | platform-professional-hardware | accept-in-002 (planned) |
| IN-003 | P0 | 2 | planned | codecs-network-recording | accept-in-003 (planned) |
| IN-004 | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-005 | P1 | 3 | planned | audio-dsp | accept-in-005 (planned) |
| IN-006 | P1 | 5 | planned | media-core-clocks | accept-in-006 (present) |
| IN-008 | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (planned) |
| IN-009 | P1 | 5 | planned | media-core-clocks | accept-in-009 (present) |
| IN-010 | P1 | 3 | planned | gpu-compositor-color | accept-in-010 (planned) |
| IN-011 | P1 | 3 | planned | platform-professional-hardware | accept-in-011 (planned) |
| IN-012 | P1 | 6 | planned | replay-guests | accept-in-012 (planned) |
| IN-013 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-013 (present) |
| IN-014 | P2 | 9 | planned | graphics-data-automation-plugins | accept-in-014 (planned) |
| IN-015 | P0 | 4 | planned | codecs-network-recording | accept-in-015 (planned) |
| IN-016 | P1 | 4 | planned | codecs-network-recording | accept-in-016 (planned) |
| IN-017 | P1 | 4 | planned | codecs-network-recording | accept-in-017 (planned) |
| IN-018 | P1 | 8 | blocked | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-020 | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (planned) |
| IN-021 | P1 | 9 | planned | gpu-compositor-color | accept-in-021 (present) |
| IN-022 | P1 | 4 | planned | media-core-clocks | accept-in-022 (present) |
| IN-023 | P1 | 3 | planned | gpu-compositor-color | accept-in-023 (present) |
| IN-024 | P1 | 3 | planned | media-core-clocks | accept-in-024 (planned) |
| IN-025 | P1 | 3 | planned | engine-control-persistence-security | accept-in-025 (planned) |
| IN-026 | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (planned) |
| IN-027 | P1 | 5 | planned | graphics-data-automation-plugins | accept-in-027 (present) |
| IN-028 | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | P2 | 6 | blocked | replay-guests | accept-in-029 (planned) |
| SW-001 | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (planned) |
| SW-002 | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (planned) |
| SW-003 | P1 | 3 | planned | gpu-compositor-color | accept-sw-003 (planned) |
| SW-004 | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (planned) |
| SW-005 | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (planned) |
| SW-006 | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (planned) |
| SW-007 | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-009 | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (planned) |
| SW-010 | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| SW-011 | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| SW-012 | P1 | 4 | planned | gpu-compositor-color | accept-sw-012 (present) |
| SW-013 | P1 | 3 | planned | studio-web-ux | accept-sw-013 (present) |
| SW-014 | P1 | 3 | planned | studio-web-ux | accept-sw-014 (present) |
| SW-015 | P1 | 5 | planned | gpu-compositor-color | accept-sw-015 (planned) |
| SW-016 | P1 | 3 | planned | media-core-clocks | accept-sw-016 (present) |
| SW-017 | P1 | 5 | planned | engine-control-persistence-security | accept-sw-017 (present) |
| SW-018 | P1 | 5 | planned | gpu-compositor-color | accept-sw-018 (planned) |
| GX-001 | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| GX-002 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-002 (present) |
| GX-003 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-003 (present) |
| GX-004 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-004 (present) |
| GX-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-005 (present) |
| GX-006 | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-008 | P2 | 9 | planned | graphics-data-automation-plugins | accept-gx-008 (planned) |
| GX-009 | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | P2 | 9 | blocked | graphics-data-automation-plugins | accept-gx-010 (present) |
| AU-001 | P0 | 3 | planned | audio-dsp | accept-au-001 (planned) |
| AU-002 | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| AU-006 | P1 | 8 | blocked | audio-dsp | accept-au-006 (planned) |
| AU-007 | P1 | 3 | planned | audio-dsp | accept-au-007 (present) |
| AU-008 | P1 | 3 | planned | audio-dsp | accept-au-008 (present) |
| AU-009 | P1 | 5 | planned | audio-dsp | accept-au-009 (present) |
| AU-010 | P1 | 4 | planned | audio-dsp | accept-au-010 (present) |
| AU-011 | P1 | 4 | planned | platform-professional-hardware | accept-au-011 (planned) |
| OR-001 | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-002 | P1 | 4 | planned | codecs-network-recording | accept-or-002 (present) |
| OR-003 | P1 | 4 | planned | codecs-network-recording | accept-or-003 (planned) |
| OR-004 | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-005 | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-006 | P1 | 4 | planned | codecs-network-recording | accept-or-006 (planned) |
| OR-007 | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |
| OR-008 | P1 | 8 | blocked | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | P1 | 8 | blocked | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-011 | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (planned) |
| OR-012 | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | P1 | 7 | planned | replay-guests | accept-or-014 (present) |
| OR-015 | P1 | 5 | planned | graphics-data-automation-plugins | accept-or-015 (present) |
| OR-016 | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
| OR-017 | P1 | 4 | planned | studio-web-ux | accept-or-017 (planned) |
| RC-001 | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-004 | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |
| RC-005 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-005 (present) |
| RC-006 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-006 (present) |
| RC-007 | P1 | 4 | planned | studio-web-ux | accept-rc-007 (planned) |
| RC-008 | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-009 | P2 | 9 | planned | engine-control-persistence-security | accept-rc-009 (present) |
| RC-010 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-010 (present) |
| RC-011 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-011 (planned) |
| RC-012 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-012 (present) |
| RC-013 | P1 | 5 | planned | engine-control-persistence-security | accept-rc-013 (present) |
| RC-014 | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-015 | P1 | 4 | planned | engine-control-persistence-security | accept-rc-015 (present) |
| RC-016 | P2 | 9 | planned | studio-web-ux | accept-rc-016 (planned) |
| RC-017 | P1 | 5 | planned | graphics-data-automation-plugins | accept-rc-017 (planned) |

## browser-control

Total: 11 features. Status: planned=10, blocked=1. Priority: P0=1, P1=8, P2=2.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| GX-006 | P1 | 5 | planned | studio-web-ux | accept-gx-006 (present) |
| GX-007 | P1 | 5 | planned | graphics-data-automation-plugins | accept-gx-007 (present) |
| GX-010 | P2 | 9 | blocked | graphics-data-automation-plugins | accept-gx-010 (present) |
| OR-017 | P1 | 4 | planned | studio-web-ux | accept-or-017 (planned) |
| RC-001 | P1 | 6 | planned | replay-guests | accept-rc-001 (present) |
| RC-002 | P1 | 6 | planned | replay-guests | accept-rc-002 (present) |
| RC-003 | P1 | 6 | planned | replay-guests | accept-rc-003 (present) |
| RC-007 | P1 | 4 | planned | studio-web-ux | accept-rc-007 (planned) |
| RC-008 | P0 | 1 | planned | engine-control-persistence-security | accept-rc-008 (present) |
| RC-014 | P1 | 5 | planned | studio-web-ux | accept-rc-014 (present) |
| RC-016 | P2 | 9 | planned | studio-web-ux | accept-rc-016 (planned) |

## profile-a-hd

Total: 24 features. Status: planned=24. Priority: P0=23, P1=1.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-001 | P0 | 3 | planned | platform-professional-hardware | accept-in-001 (planned) |
| IN-003 | P0 | 2 | planned | codecs-network-recording | accept-in-003 (planned) |
| IN-008 | P0 | 2 | planned | gpu-compositor-color | accept-in-008 (planned) |
| IN-015 | P0 | 4 | planned | codecs-network-recording | accept-in-015 (planned) |
| IN-020 | P0 | 3 | planned | gpu-compositor-color | accept-in-020 (planned) |
| IN-026 | P0 | 2 | planned | engine-control-persistence-security | accept-in-026 (planned) |
| SW-001 | P0 | 2 | planned | gpu-compositor-color | accept-sw-001 (planned) |
| SW-002 | P0 | 3 | planned | gpu-compositor-color | accept-sw-002 (planned) |
| SW-004 | P0 | 3 | planned | gpu-compositor-color | accept-sw-004 (planned) |
| SW-005 | P0 | 3 | planned | gpu-compositor-color | accept-sw-005 (planned) |
| SW-006 | P0 | 2 | planned | gpu-compositor-color | accept-sw-006 (planned) |
| SW-007 | P0 | 3 | planned | gpu-compositor-color | accept-sw-007 (present) |
| SW-008 | P0 | 3 | planned | gpu-compositor-color | accept-sw-008 (present) |
| SW-011 | P0 | 3 | planned | gpu-compositor-color | accept-sw-011 (present) |
| GX-001 | P0 | 5 | planned | graphics-data-automation-plugins | accept-gx-001 (present) |
| AU-001 | P0 | 3 | planned | audio-dsp | accept-au-001 (planned) |
| AU-002 | P0 | 3 | planned | audio-dsp | accept-au-002 (present) |
| AU-003 | P0 | 3 | planned | audio-dsp | accept-au-003 (present) |
| AU-004 | P0 | 3 | planned | media-core-clocks | accept-au-004 (present) |
| AU-005 | P0 | 3 | planned | audio-dsp | accept-au-005 (present) |
| OR-001 | P0 | 2 | planned | codecs-network-recording | accept-or-001 (present) |
| OR-005 | P1 | 4 | planned | codecs-network-recording | accept-or-005 (present) |
| OR-011 | P0 | 2 | planned | platform-professional-hardware | accept-or-011 (planned) |
| RC-004 | P0 | 3 | planned | graphics-data-automation-plugins | accept-rc-004 (present) |

## profile-b-uhd

Total: 5 features. Status: planned=5. Priority: P1=5.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-004 | P1 | 8 | planned | codecs-network-recording | accept-in-004 (planned) |
| IN-017 | P1 | 4 | planned | codecs-network-recording | accept-in-017 (planned) |
| SW-009 | P1 | 8 | planned | gpu-compositor-color | accept-sw-009 (planned) |
| SW-010 | P1 | 3 | planned | gpu-compositor-color | accept-sw-010 (present) |
| OR-007 | P1 | 4 | planned | codecs-network-recording | accept-or-007 (present) |

## profile-c-replay

Total: 4 features. Status: planned=4. Priority: P1=4.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| OR-004 | P1 | 4 | planned | codecs-network-recording | accept-or-004 (present) |
| OR-012 | P1 | 7 | planned | replay-guests | accept-or-012 (present) |
| OR-013 | P1 | 7 | planned | replay-guests | accept-or-013 (present) |
| OR-014 | P1 | 7 | planned | replay-guests | accept-or-014 (present) |

## vendor-certified

Total: 12 features. Status: planned=5, blocked=7. Priority: P1=9, P2=3.

| ID | Pri | Phase | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| IN-002 | P1 | 8 | blocked | platform-professional-hardware | accept-in-002 (planned) |
| IN-018 | P1 | 8 | blocked | platform-professional-hardware | accept-in-018 (planned) |
| IN-019 | P1 | 8 | planned | codecs-network-recording | accept-in-019 (planned) |
| IN-028 | P1 | 5 | planned | platform-professional-hardware | accept-in-028 (present) |
| IN-029 | P2 | 6 | blocked | replay-guests | accept-in-029 (planned) |
| GX-009 | P1 | 8 | planned | platform-professional-hardware | accept-gx-009 (planned) |
| GX-010 | P2 | 9 | blocked | graphics-data-automation-plugins | accept-gx-010 (present) |
| AU-006 | P1 | 8 | blocked | audio-dsp | accept-au-006 (planned) |
| OR-008 | P1 | 8 | blocked | platform-professional-hardware | accept-or-008 (planned) |
| OR-009 | P1 | 8 | blocked | codecs-network-recording | accept-or-009 (planned) |
| OR-010 | P1 | 8 | planned | platform-professional-hardware | accept-or-010 (planned) |
| OR-016 | P2 | 8 | planned | codecs-network-recording | accept-or-016 (planned) |
