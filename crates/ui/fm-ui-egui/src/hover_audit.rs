//! Hover-help audit (parity row RC-018): every interactive widget in the
//! console explains itself on hover, in every shipped language.
//!
//! # Approach
//!
//! The audit renders the real [`StudioShell`] headlessly over rich fixtures
//! (each side tab, including the Media tab's transport bars, every dialog, the advanced controls behind each
//! disclosure, a session without permissions, a live manual transition, a
//! timed transition in flight, a
//! locked project, Fade-to-Black and a running playlist, toasts, the input
//! lifecycle (colour labels, badges, the category filter, the tile menu's
//! Category and Colour label submenus, and the Replace source, Import
//! files, and Categories dialogs), the mixer's strip labels, pan knobs,
//! group assignment, group strips (solo and parent group), the monitor
//! strip with its solo-mode keys, the Fade-to-Black output menu, the
//! Fullscreen outputs dialog (with and without a display list or a
//! presenter, with a draft, changed elsewhere, refused, applying, for a
//! viewer, and locked), the stream destinations' group and row keys with
//! the Add / Edit destination dialog and its Remove confirmation (OR-006),
//! and a disconnected console) in English
//! and Spanish, and inspects what egui itself recorded for the pass, so no
//! drawing code has to opt in and a widget added without help cannot slip
//! past:
//!
//! - [`egui::Memory::set_everything_is_visible`] makes egui treat every
//!   tooltip as hovered, every popup and menu as open, and every collapsing
//!   section as expanded, so one pass shows each widget's tooltip, including
//!   menu items and overlay details. An enabled widget shows only its
//!   `on_hover_*` help and a disabled one only its `on_disabled_hover_*`
//!   help, exactly as the operator would see them.
//! - The pass's [`egui::WidgetRects`] list every widget with its
//!   [`egui::Sense`]; anything that senses a click or drag is an interactive
//!   control. egui's own container chrome (panel and window resize edges,
//!   area drag handles, scroll bars, and drag-to-scroll regions) is
//!   recognised by the ids egui derives for it and skipped.
//! - Each tooltip is an area on layer `(Order::Tooltip, widget.with(0))`.
//!   With `debug.show_interactive_widgets` set, debug builds record the
//!   [`egui::WidgetInfo`] of every widget, including the labels inside
//!   tooltips, which yields both a readable name for the control and the
//!   tooltip's text.
//!
//! The audit then requires, per widget: a tooltip; for a disabled widget, a
//! tooltip that says why (the localized `Unavailable: …` line); and in
//! Spanish, no line that is still the English text of a catalog message.
//! It also requires each single-key console shortcut to be named in the
//! tooltip of a control that performs it.
//!
//! The keyframe animation timeline is not part of the console yet, so it is
//! audited standalone under the same rules: in Simple and Advanced modes,
//! with keys selected and an active row, with nothing selected, with a
//! refused edit showing, and with a custom curve selected (the curve
//! editor and the value graph's bezier handles, also under a lock), each
//! pinned to the audited language. The colour
//! grading panel is audited the same way: Simple and Advanced modes, fresh
//! (with its keys disabled for a reason) and edited (with a LUT, compare
//! slots, a copied grade, bypass, the wipe compare, and a hue curve), and
//! with a refused edit showing. The Studio Animation window is audited in
//! the console: Simple mode without a known design (presets applied on the
//! server), Advanced mode with a design, keys selected and an active row, a
//! conflict, a padlock, and a session without permissions. The layer
//! designer is audited in the console too: nothing selected, an edited
//! draft with one layer selected, two layers selected, a padlock, and a
//! viewer.
//!
//! The Add input dialog's camera / capture form (IN-001) is audited in the
//! console with a device inventory (a device chosen, formats filtered, an
//! ALSA device typed), and without one (not yet known, no capture backend,
//! a failed scan); the fixture's capture inputs put a SIGNAL LOST banner
//! and a LIVE badge on two tiles. [`check_capture`] audits those scenes
//! alone and requires each capture control's help to have been shown.
//!
//! The system audio form (AU-011) is audited the same way with the
//! server's device list (the default, a listed, and a typed device), a
//! failed listing, none yet, a server without system audio, and Replace
//! source editing a system audio input; [`check_system_audio`] audits those
//! scenes alone and requires each control's help to have been shown.
//!
//! The mixer's headphone device picker (AU-001, Protocol 2.102) is audited
//! with the server's output list (a listed and an unplugged device), none
//! yet, a listing that failed, and a server without output devices (the
//! typed field); [`check_monitor_output`] audits those scenes alone.
//!
//! The audits are hidden public entry points ([`check`],
//! [`check_timeline`], [`check_color_panel`], [`check_capture`],
//! [`check_system_audio`], [`check_monitor_output`]) so the
//! integration target
//! `tests/hover_audit.rs` runs them in every shipped language. They rely on
//! the widget info egui records only in debug builds.

use core::fmt::Write as _;
use core::num::NonZeroU128;
use std::collections::{BTreeMap, BTreeSet};

use egui::{Id, LayerId, Order, Rect, Vec2, WidgetRect};
use fm_animation::{
    Clip, Easing, KeyRef, KeySelection, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value,
};
use fm_protocol::{
    AudioMeterChannel, AudioMetersMessage, EditHistoryStatus, EditLockStatus, EngineIdentity,
    FadeToBlackPosition, FadeToBlackState, InputAudioMeters, InputStatus, ManualTransitionKind,
    ManualTransitionPosition, ManualTransitionState, ManualTransitionStatus, OutputStatus,
    OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind, PlaylistEntrySpec,
    PlaylistLoopMode, PlaylistRunState, PlaylistStateMessage, PlaylistTransitionKind,
    RecordingStatus, ServerIdentity, SnapshotMessage, StingerAudioPolicy,
    StingerMissingMediaFallback, StingerReadiness, StreamRealizedState, TitleFieldStatus,
    TitleFieldValue, TitleStateMessage, TitleSummary, WireInputId, WireOutputId,
    WireOverlayChannelId, WireRecorderSlot, WireStingerSlotId, WireStreamTargetId,
    WireTitleFieldId,
};
use fm_types::{FrameRate, InputId, ProjectId};
use fm_ui_model::animation_editor::{
    AnimDocument, AnimEditorError, EditorMode, TargetBounds, TargetKind, TargetOutline,
};
use fm_ui_model::i18n::{Arg, Language, Locale, Msg};
use fm_ui_model::title_editor::TitleAccess;
use fm_ui_model::{ClientModel, ProjectSnapshot};

use crate::animation_timeline::{AnimationTimelineState, AnimationTimelineWidget, TimelineAction};
use crate::color_panel::{
    COMPARE_SHORTCUT, ColorPanelState, ColorPanelWidget, GradeAction, PanelMode,
};
use crate::dock::PanelId;
use crate::{StudioConnectionStatus, StudioShell, StudioUiState};
use fm_ui_model::grade_editor::GradeTarget;

const SCREEN: Vec2 = Vec2::new(1920.0, 1080.0);
const TITLE_INPUT: u128 = 6;
/// Most lines of help a control may show besides its disabled reason.
const MAX_HELP_LINES: usize = 4;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

/// An action's fixed target (RC-004).
fn target(value: u128) -> fm_protocol::WireInputTarget {
    fm_protocol::WireInputTarget::Fixed(wire(value))
}

/// The scene the Scene input shows (Protocol 2.35 addresses layouts and
/// layer grades by scene).
const SCENE: u128 = 30;

fn scene_id() -> fm_types::SceneId {
    fm_types::SceneId::new(NonZeroU128::new(SCENE).unwrap())
}

fn wire_scene() -> fm_protocol::WireSceneId {
    fm_protocol::WireSceneId::from_domain(scene_id())
}

fn output(value: u128) -> WireOutputId {
    WireOutputId::new(NonZeroU128::new(value).unwrap())
}

/// The input lifecycle of fixture input `value` (IN-026): filed cameras
/// with colour labels, a paused guest, an offline clip, and a closed
/// slides input, so the tiles show every badge and the bank its filter.
fn lifecycle(value: u128) -> fm_protocol::InputLifecycleStatus {
    use fm_protocol::{InputColorLabel, InputLifecycleStatus, InputOfflineReason};
    match value {
        1 => InputLifecycleStatus {
            category: Some(1),
            color: Some(InputColorLabel::Red),
            ..InputLifecycleStatus::default()
        },
        2 => InputLifecycleStatus {
            category: Some(1),
            color: Some(InputColorLabel::Blue),
            ..InputLifecycleStatus::default()
        },
        3 => InputLifecycleStatus {
            closed: true,
            ..InputLifecycleStatus::default()
        },
        4 => InputLifecycleStatus {
            paused: true,
            color: Some(InputColorLabel::Teal),
            ..InputLifecycleStatus::default()
        },
        5 => InputLifecycleStatus {
            category: Some(2),
            offline: Some(InputOfflineReason::SourceMissing),
            ..InputLifecycleStatus::default()
        },
        _ => InputLifecycleStatus::default(),
    }
}

/// A synchronized show that exercises every console region: five sources
/// and a title, two outputs and stream targets, two Stinger slots, an
/// overlay on air with a queue, undo and redo history, and a playlist.
fn model(variant: Variant) -> ClientModel {
    model_with(variant, audit_shortcuts())
}

/// [`model`] with `shortcuts` as the project's shortcuts.
#[allow(clippy::too_many_lines)] // one protocol fixture
fn model_with(variant: Variant, shortcuts: Vec<fm_protocol::ShortcutStatus>) -> ClientModel {
    let Variant {
        manual,
        locked,
        black,
        playing,
        transitioning,
    } = variant;
    let project = ProjectId::new(NonZeroU128::new(1).unwrap());
    let fade = FadeToBlackState {
        target_active: black,
        position: if black {
            FadeToBlackPosition::BLACK
        } else {
            FadeToBlackPosition::LIVE
        },
    };
    let names = [
        "Camera 1",
        "Camera 2",
        "Slides",
        "Guest",
        "Clip",
        "Lower third",
    ];
    let overlays = (1..=8)
        .map(|channel| fm_protocol::OverlayStatus {
            channel: WireOverlayChannelId::new(channel).unwrap(),
            source: (channel <= 2).then(|| wire(u128::from(channel) + 2)),
            active: channel == 1,
            opacity: if channel == 1 { 255 } else { 0 },
            transition: OverlayTransitionKind::Cut,
            duration_frames: 1,
            position: OverlayPositionPreset::FullFrame,
            border: OverlayBorderPreset::None,
            queued_sources: if channel == 1 {
                vec![wire(4)]
            } else {
                Vec::new()
            },
            included_outputs: if channel == 1 {
                vec![output(1)]
            } else {
                Vec::new()
            },
        })
        .collect::<Vec<_>>();
    let manual_status = if manual {
        ManualTransitionStatus::Active(ManualTransitionState {
            kind: ManualTransitionKind::Fade,
            from: wire(1),
            to: wire(2),
            interval_start: ManualTransitionPosition::START,
            position: ManualTransitionPosition::new(4_000).unwrap(),
        })
    } else {
        ManualTransitionStatus::Inactive
    };
    let snapshot = SnapshotMessage {
        // Strips 1 and 2 share a renamed group, strip 4 sits in a muted
        // group, strip 1 is labelled, and the Clean Feed stays live under
        // Fade to Black, so group strips, labels, and the FTB output menu
        // are audited (AU-001, SW-004).
        audio_mixer: {
            let mut mixer = fm_protocol::AudioMixerStatus::default();
            mixer.groups[0].name = "Mics".into();
            mixer.groups[0].gain_millidb = -3_000;
            mixer.groups[1].muted = true;
            // Group 2 sits inside group 3 (which then gets a strip) and is
            // soloed in PFL, so the parent picker, group solo, and the
            // monitor strip's solo listen are audited (Protocol 2.44).
            mixer.groups[1].parent = Some(3);
            mixer.groups[1].soloed = true;
            mixer.solo_mode = fm_protocol::AudioSoloMode::Pfl;
            // A headphone device at its own level (Protocol 2.53).
            mixer.monitor.device = Some("hw:CARD=Headset,DEV=0".into());
            mixer.monitor.gain_millidb = -6_000;
            mixer.labels.push(fm_protocol::InputAudioLabel {
                input: wire(1),
                label: "Host Mic".into(),
            });
            mixer
        },
        // Program fades over its own duration on the eased curve, so the
        // FTB output menu's duration field and curve keys are audited.
        fade_to_black_excluded_outputs: vec![output(2)],
        fade_to_black_output_durations: vec![(output(1), 45)],
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::Ease,
        display_outputs: display_output_entries(),
        display_output_states: display_output_states(),
        // IN-015: a live stream and a reconnecting one, so the tile badges
        // and their details are audited (Protocol 2.46); the second is an
        // encrypted SRT listener (IN-017, Protocol 2.49), so its connection
        // and source audio details are too.
        network_streams: vec![
            fm_protocol::NetworkStreamEntry {
                input: wire(1),
                config: fm_protocol::NetworkStreamConfig::new("rtsp://***@cam1.local/live"),
                status: fm_protocol::NetworkStreamStatus {
                    state: fm_protocol::NetworkStreamState::Live,
                    bitrate_bps: 4_000_000,
                    buffer_ms: 280,
                    ..fm_protocol::NetworkStreamStatus::default()
                },
            },
            fm_protocol::NetworkStreamEntry {
                input: wire(2),
                config: srt_listener(),
                status: fm_protocol::NetworkStreamStatus {
                    state: fm_protocol::NetworkStreamState::Reconnecting {
                        attempt: 3,
                        next_in_ms: 2_000,
                    },
                    reconnects: 3,
                    source_channels: 8,
                    last_error: Some("Connection refused".into()),
                    ..fm_protocol::NetworkStreamStatus::default()
                },
            },
        ],
        // IN-001: a camera whose signal is lost and a live one, so the
        // SIGNAL LOST banner, the corner badge, and their details are
        // audited (Protocol 2.47).
        capture_inputs: capture_entries(),
        // AU-011: a system audio input whose ALSA device is reopening, so
        // its tile badge and details are audited (Protocol 2.55).
        system_audio_inputs: vec![fm_protocol::SystemAudioEntry {
            input: wire(5),
            config: fm_protocol::SystemAudioConfig::new(
                fm_protocol::SystemAudioBackend::Alsa,
                "hw:Loopback,1",
            ),
            status: fm_protocol::SystemAudioStatus {
                state: fm_protocol::SystemAudioState::Reopening {
                    attempt: 2,
                    next_in_ms: 1_000,
                },
                reopens: 2,
                last_error: Some("No such file or directory".into()),
            },
        }],
        // IN-011: a window capture whose window closed, so its tile badge
        // and details are audited.
        screen_capture_inputs: vec![fm_protocol::ScreenCaptureEntry {
            input: wire(6),
            config: fm_protocol::ScreenCaptureConfig::new(
                fm_protocol::ScreenCaptureTarget::Window(fm_protocol::WindowRule::exact(
                    "Keynote",
                    "Quarterly deck",
                    None,
                )),
            ),
            status: fm_protocol::ScreenCaptureStatus {
                state: fm_protocol::ScreenCaptureState::TargetGone,
                reopens: 1,
                last_error: Some("no Keynote window titled \"Quarterly deck\" is open".into()),
                ..fm_protocol::ScreenCaptureStatus::default()
            },
        }],
        // OR-006: the two roster targets' settings (a running local HLS
        // ladder and a stopped keyed RTMP destination with a backup), so
        // the rows' Retry / Edit / Remove keys and the destination dialog
        // are audited enabled and disabled.
        stream_targets: stream_target_entries(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
        alert_rules: Vec::new(),
        engine: EngineIdentity {
            engine_id: "e".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 10,
        show_name: "Evening Broadcast".into(),
        inputs: (1..=6)
            .map(|value| InputStatus {
                input: wire(value),
                name: names[usize::try_from(value).unwrap() - 1].into(),
                lifecycle: lifecycle(value),
            })
            .collect(),
        outputs: vec![
            OutputStatus {
                output: output(1),
                name: "Program Output".into(),
                aux_bus: None,
            },
            OutputStatus {
                output: output(2),
                name: "Clean Feed".into(),
                aux_bus: None,
            },
        ],
        input_audio_strips: (1..=6)
            .map(|value| fm_protocol::InputAudioStripStatus {
                pan_basis_points: if value == 1 { -2_500 } else { 0 },
                group: match value {
                    1 | 2 => Some(1),
                    4 => Some(2),
                    _ => None,
                },
                input: wire(value),
                gain_millidb: 0,
                balance_basis_points: 0,
                muted: value == 3,
                soloed: false,
                follow_video: value != 2,
                delay_samples: 0,
                buses: fm_types::audio_bus::AudioBusMask::MASTER,
            })
            .collect(),
        // A timed transition in flight has swapped the desired buses; the
        // realized ones follow when it ends.
        desired_program: wire(if transitioning { 2 } else { 1 }),
        desired_preview: wire(if transitioning { 1 } else { 2 }),
        realized_program: wire(1),
        realized_preview: wire(2),
        desired_manual_transition: manual_status,
        realized_manual_transition: manual_status,
        desired_fade_to_black: fade,
        realized_fade_to_black: fade,
        stingers: [(1, 5), (2, 4)]
            .map(|(slot, media)| fm_protocol::StingerStatus {
                slot: WireStingerSlotId::new(slot).unwrap(),
                media_input: wire(media),
                preload: true,
                cut_point_frames: 10,
                audio_policy: StingerAudioPolicy::StingerOnly,
                missing_media_fallback: StingerMissingMediaFallback::Fade,
                readiness: StingerReadiness::Ready,
            })
            .to_vec(),
        streams: vec![
            fm_protocol::StreamStatus {
                target: WireStreamTargetId::new(NonZeroU128::new(1).unwrap()),
                name: "Local HLS Ladder".into(),
                desired_running: true,
                realized: StreamRealizedState::Live,
                detail: None,
            },
            fm_protocol::StreamStatus {
                target: WireStreamTargetId::new(NonZeroU128::new(2).unwrap()),
                name: "RTMP Backup".into(),
                desired_running: false,
                realized: StreamRealizedState::Stopped,
                detail: None,
            },
        ],
        recordings: [(1, true), (2, false)]
            .map(|(slot, desired_active)| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active,
            })
            .to_vec(),
        desired_overlays: overlays.clone(),
        realized_overlays: overlays,
        edit_locks: EditLockStatus {
            project: locked,
            inputs: vec![wire(5)],
        },
        edit_history: EditHistoryStatus {
            undo_depth: 2,
            next_undo: Some(9),
            redo_depth: 1,
            next_redo: Some(8),
        },
        titles: vec![TitleSummary {
            input: wire(TITLE_INPUT),
            field_revision: 2,
        }],
        // The slides input is announced as a Scene input for the scene
        // editor of the Animation window (GX-011 stage 5).
        scenes: vec![wire(SCENE_INPUT)],
        layout_scenes: vec![wire_scene()],
        frame_rate: FrameRate::new(25, 1).unwrap(),
        playlist_follows: true,
        grade_states: 4,
        animation_preset_states: 0,
        media_states: 2,
        paged_states: 1,
        cue_list_states: 2,
        script_states: 2,
        shortcut_states: u16::try_from(shortcuts.len()).unwrap(),
        still_states: 0,
        media_list_states: 1,
        input_categories: vec![
            fm_protocol::InputCategoryStatus {
                id: 1,
                name: "Cameras".into(),
            },
            fm_protocol::InputCategoryStatus {
                id: 2,
                name: "Playback".into(),
            },
        ],
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        transitions_in_flight: Vec::new(),
    };
    let mut model = ClientModel::new(project);
    model
        .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
        .unwrap();
    let field = |value: u128| WireTitleFieldId::new(NonZeroU128::new(value).unwrap());
    model
        .apply_title_state(TitleStateMessage {
            revision: 10,
            input: wire(TITLE_INPUT),
            name: "Lower third".into(),
            field_revision: 2,
            phase: fm_protocol::TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            timeline: None,
            fields: vec![
                TitleFieldStatus {
                    field: field(1),
                    name: "Name".into(),
                    value: TitleFieldValue::Text("Ana".into()),
                    exposed: true,
                    revision: 0,
                },
                TitleFieldStatus {
                    field: field(2),
                    name: "Score".into(),
                    value: TitleFieldValue::Number(4),
                    exposed: true,
                    revision: 0,
                },
                TitleFieldStatus {
                    field: field(3),
                    name: "Bar".into(),
                    value: TitleFieldValue::Color {
                        red: 0x11,
                        green: 0x22,
                        blue: 0x33,
                        alpha: 0xFF,
                    },
                    exposed: true,
                    revision: 0,
                },
                TitleFieldStatus {
                    field: field(4),
                    name: "Logo".into(),
                    value: TitleFieldValue::Image {
                        asset_uri: "asset://logo.png".into(),
                    },
                    // Hidden from the browser title editor (GX-006).
                    exposed: false,
                    revision: 0,
                },
            ],
        })
        .unwrap();
    // The lower third's animation for the Animation window.
    model
        .apply_title_animation_state(fm_protocol::TitleAnimationStateMessage {
            revision: 10,
            input: wire(TITLE_INPUT),
            animation_revision: 4,
            clip: Box::new(title_clip()),
        })
        .unwrap();
    // The scene's animation and layers for the Animation window.
    model
        .apply_scene_animation_state(fm_protocol::SceneAnimationStateMessage {
            revision: 10,
            input: wire(SCENE_INPUT),
            scene: wire_scene(),
            animation_revision: 2,
            clip: Box::new(scene_clip()),
        })
        .unwrap();
    model
        .apply_scene_layout_state(fm_protocol::SceneLayoutStateMessage {
            revision: 10,
            scene: wire_scene(),
            name: "Slides".into(),
            layout: Box::new(scene_layout()),
        })
        .unwrap();
    // A LUT, a library grade, and a graded camera for the Colour window.
    let grade_id = |value: u32| core::num::NonZeroU32::new(value).unwrap();
    let mut night = fm_grade::Grade::IDENTITY;
    night.exposure_millistops = -600;
    night.saturation_permille = 800;
    let mut warm = fm_grade::Grade::IDENTITY;
    warm.white_balance.temperature = 400;
    for entry in [
        fm_protocol::GradeEntry::Lut {
            lut: grade_id(1),
            name: "Film".into(),
            kind: fm_grade::CubeKind::ThreeD,
            size: 17,
        },
        fm_protocol::GradeEntry::Library {
            grade: grade_id(1),
            name: "Night look".into(),
            value: Box::new(night),
        },
        fm_protocol::GradeEntry::Input {
            input: wire(2),
            source: fm_protocol::WireGradeSource::Inline(Box::new(warm)),
        },
        // The scene's host layer carries the library look (Protocol 2.35).
        fm_protocol::GradeEntry::Layer {
            scene: wire_scene(),
            layer: scene_layer(1),
            source: fm_protocol::WireGradeSource::Library(grade_id(1)),
        },
    ] {
        model
            .apply_grade_state(fm_protocol::GradeStateMessage {
                revision: 10,
                entry,
            })
            .unwrap();
    }
    let entry = |id: u64, source: u128, transition, transition_frames, enabled| PlaylistEntrySpec {
        id,
        input: wire(source),
        duration_frames: 250,
        transition,
        transition_frames,
        enabled,
    };
    model
        .apply_playlist_state(PlaylistStateMessage {
            revision: 10,
            frame: 100,
            loop_mode: PlaylistLoopMode::Off,
            entries: vec![
                entry(1, 1, PlaylistTransitionKind::Cut, 0, true),
                entry(2, 3, PlaylistTransitionKind::Fade, 12, true),
                entry(3, 4, PlaylistTransitionKind::Cut, 0, false),
            ],
            state: if playing {
                PlaylistRunState::Playing {
                    entry: 2,
                    take: 1,
                    cycle: 0,
                    started_frame: 50,
                }
            } else {
                PlaylistRunState::Stopped
            },
        })
        .unwrap();
    // IN-003: the Guest input is a finished file, cued and editable; the
    // (padlocked) Clip input plays a growing file that caught up with its
    // writer: the live badge, the waiting-for-data indicator, Finish, and a
    // two-language track menu.
    let track = |stream_index: u32, language: &str, default: bool| fm_protocol::MediaAudioTrack {
        stream_index,
        sample_rate: 48_000,
        channels: 2,
        language: Some(language.into()),
        title: None,
        default,
    };
    model
        .apply_media_state(fm_protocol::MediaStateMessage {
            revision: 10,
            frame: 100,
            status: fm_protocol::MediaStatus {
                input: wire(4),
                settings: fm_protocol::MediaSettingsStatus::default(),
                transport: fm_protocol::MediaTransportStatus {
                    available: true,
                    state: fm_protocol::MediaTransportState::Stopped,
                    position: 0,
                    position_time: fm_protocol::MediaTime::ZERO,
                    playable_end: 250,
                    frames: 250,
                    complete: true,
                    loops: 0,
                    audio_track: Some(1),
                    tracks: vec![track(1, "eng", true)],
                    source_rate: Some(FrameRate::new(25, 1).unwrap()),
                    fault: None,
                },
            },
        })
        .unwrap();
    model
        .apply_media_state(fm_protocol::MediaStateMessage {
            revision: 10,
            frame: 100,
            status: fm_protocol::MediaStatus {
                input: wire(5),
                settings: fm_protocol::MediaSettingsStatus {
                    trim_in: 25,
                    end_action: fm_protocol::MediaEndAction::Loop,
                    speed_basis_points: 15_000,
                    audio_stream: Some(2),
                    growing: Some(fm_protocol::MediaGrowingStatus {
                        live_edge_offset_frames: 12,
                        idle_completion_refreshes: 0,
                    }),
                    start_frame: 25,
                    ..fm_protocol::MediaSettingsStatus::default()
                },
                transport: fm_protocol::MediaTransportStatus {
                    available: true,
                    state: fm_protocol::MediaTransportState::Waiting,
                    position: 488,
                    position_time: fm_protocol::MediaTime {
                        numerator: 488,
                        denominator: 25,
                    },
                    playable_end: 488,
                    frames: 500,
                    complete: false,
                    loops: 0,
                    audio_track: Some(2),
                    tracks: vec![track(1, "eng", true), track(2, "spa", false)],
                    source_rate: Some(FrameRate::new(25, 1).unwrap()),
                    fault: None,
                },
            },
        })
        .unwrap();
    for list in cue_lists() {
        model
            .apply_cue_list_state(fm_protocol::CueListStateMessage {
                revision: 10,
                frame: 100,
                list,
            })
            .unwrap();
    }
    for script in audit_scripts() {
        model
            .apply_script_state(fm_protocol::ScriptStateMessage {
                revision: 10,
                script,
                run: fm_protocol::ScriptRunStatus::Idle,
            })
            .unwrap();
    }
    for shortcut in shortcuts {
        model
            .apply_shortcut_state(fm_protocol::ShortcutStateMessage {
                revision: 10,
                shortcut,
            })
            .unwrap();
    }
    // IN-006: the Guest input is a shuffled media list on its second item,
    // so the Media tab shows the list card with every row key.
    let item = |id: u32, uri: &str, title: Option<&str>| fm_protocol::MediaListItemSpec {
        id,
        uri: uri.into(),
        title: title.map(Into::into),
        mark_in: 0,
        mark_out: None,
    };
    model
        .apply_media_list_state(fm_protocol::MediaListStateMessage {
            revision: 10,
            status: fm_protocol::MediaListStatus {
                input: wire(4),
                items: vec![
                    item(1, "asset://media/opener.mov", Some("Opener")),
                    item(2, "asset://media/guest.mov", None),
                    item(3, "asset://media/bed.wav", None),
                ],
                options: fm_protocol::MediaListOptionsSpec {
                    auto_next: true,
                    auto_first: false,
                    loop_list: true,
                    shuffle_seed: Some(7),
                    // Protocol 2.95: the items cross-fade.
                    transition: fm_protocol::MediaListTransitionSpec {
                        kind: fm_protocol::MediaListTransitionKind::Fade,
                        duration_frames: 12,
                    },
                },
                active: 2,
                cycle: 0,
                order: vec![3, 2, 1],
                position: 1,
                // Protocol 2.91: two probed files and one not yet probed.
                lengths: vec![
                    Some(fm_protocol::MediaListItemLength {
                        frames: 250,
                        rate: fm_types::FrameRate::new(25, 1).unwrap(),
                    }),
                    Some(fm_protocol::MediaListItemLength {
                        frames: 1_500,
                        rate: fm_types::FrameRate::new(30_000, 1_001).unwrap(),
                    }),
                    None,
                ],
            },
        })
        .unwrap();
    // IN-014: input 6 shows page 2 of a twelve-page presentation.
    model
        .apply_paged_state(fm_protocol::PagedStateMessage {
            revision: 10,
            status: fm_protocol::PagedStatus {
                input: wire(6),
                kind: fm_protocol::PagedKind::Document,
                page: 1,
                count: 12,
                state: fm_protocol::DocumentState::Ready,
                title: Some("Quarterly review".into()),
                message: None,
            },
        })
        .unwrap();
    model
}

/// Edits the Shortcuts window's draft, when there is one.
fn edit_shortcut_draft(shell: &mut StudioShell, edit: &dyn Fn(&mut fm_protocol::ShortcutSpec)) {
    if let Some(draft) = shell.user_shortcuts.draft.as_mut() {
        edit(draft);
    }
}

/// A ready session over the audit project with its shortcuts (RC-004);
/// `operator` grants every permission.
#[cfg(test)]
pub(crate) fn shortcuts_fixture(operator: bool) -> StudioUiState {
    ready(operator, Variant::default())
}

/// [`shortcuts_fixture`] for an operator, with `extra` shortcuts after the
/// audit's.
#[cfg(test)]
pub(crate) fn shortcuts_fixture_with(extra: Vec<fm_protocol::ShortcutStatus>) -> StudioUiState {
    let mut shortcuts = audit_shortcuts();
    shortcuts.extend(extra);
    ready_over(&model_with(Variant::default(), shortcuts), true)
}

/// RC-004: the project's shortcuts. Shortcut 1 runs cue actions from F9
/// and OSC; shortcut 2 fires a GO of cue list 1 from OSC only; shortcut 3, disabled,
/// runs script 1 from Alt+J.
pub(crate) fn audit_shortcuts() -> Vec<fm_protocol::ShortcutStatus> {
    use fm_protocol::{
        ShortcutSpec, ShortcutStatus, WireCueAction, WireShortcutAction, WireShortcutCueOperation,
        WireShortcutKey,
    };
    let key = |key: &str, alt: bool| WireShortcutKey {
        key: key.into(),
        command: false,
        shift: false,
        alt,
        then: None,
        scope: None,
    };
    vec![
        ShortcutStatus {
            id: 1,
            spec: ShortcutSpec {
                name: "Lower third".into(),
                enabled: true,
                key: Some(key("F9", false)),
                osc: Some("lower-third".into()),
                action: WireShortcutAction::Actions(vec![
                    WireCueAction::OverlayOn {
                        channel: WireOverlayChannelId::new(2).unwrap(),
                        input: target(2),
                    },
                    WireCueAction::AudioMute {
                        input: target(3),
                        mute: fm_protocol::CueMute::Set(true),
                    },
                    // RC-004: an action on the console's selected input.
                    WireCueAction::AudioMute {
                        input: fm_protocol::WireInputTarget::Selected,
                        mute: fm_protocol::CueMute::Set(false),
                    },
                    // RC-004 dynamic values: a step and a toggle.
                    WireCueAction::AudioGain {
                        input: target(2),
                        gain: fm_protocol::CueGain::Step(-3_000),
                    },
                    WireCueAction::AudioMute {
                        input: target(2),
                        mute: fm_protocol::CueMute::Toggle,
                    },
                ]),
            },
        },
        ShortcutStatus {
            id: 2,
            spec: ShortcutSpec {
                name: "Open show".into(),
                enabled: true,
                key: None,
                osc: Some("open".into()),
                action: WireShortcutAction::Cue {
                    list: 1,
                    operation: WireShortcutCueOperation::Go { cue: None },
                },
            },
        },
        ShortcutStatus {
            id: 3,
            spec: ShortcutSpec {
                name: "Scores".into(),
                enabled: false,
                key: Some(key("J", true)),
                osc: None,
                action: WireShortcutAction::Script { script: 1 },
            },
        },
    ]
}

/// RC-011: the project's scripts. Script 1 is valid; script 2 has a
/// syntax error on its second line.
fn audit_scripts() -> Vec<fm_protocol::ScriptStatus> {
    let script = |id: u64, name: &str, source: &str| fm_protocol::ScriptStatus {
        id,
        spec: fm_protocol::ScriptSpec {
            name: name.into(),
            source: source.into(),
        },
        source_hidden: false,
    };
    vec![
        script(
            1,
            "Opening",
            "preview 2\ntransition fade, 25\noverlay 1, \"Camera\"\n",
        ),
        script(2, "Broken", "preview 2\nif program = 1\nend\n"),
    ]
}

/// RC-017: the show's cue lists. List 1 is mid-show (cue 10 live with every
/// action kind, cue 20 on standby, cue 30 stopped by a refused action);
/// list 2 waits on its first cue's next actions, so it cannot be edited.
#[allow(clippy::too_many_lines)] // one fixture action per kind
fn cue_lists() -> Vec<fm_protocol::CueListStatus> {
    use fm_protocol::{
        CueListSpec, CueListStatus, CuePendingStatus, CueRunStatus, WireCue, WireCueAction,
        WireCueMediaOperation,
    };
    let channel = WireOverlayChannelId::new(2).unwrap();
    let every_kind = vec![
        WireCueAction::Preview { input: target(2) },
        WireCueAction::Take {
            input: None,
            effect: fm_protocol::TransitionFavoriteEffect::Mix(
                fm_protocol::AutoTransitionKind::Fade,
            ),
            duration_frames: 25,
        },
        WireCueAction::OverlayOn {
            channel,
            input: target(3),
        },
        WireCueAction::OverlayOff { channel },
        WireCueAction::TitleOn {
            input: target(TITLE_INPUT),
            channel,
        },
        WireCueAction::TitleText {
            input: target(TITLE_INPUT),
            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
            text: "Ana Ruiz".into(),
        },
        WireCueAction::TitleNumber {
            input: target(TITLE_INPUT),
            field: WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
            add: true,
            value: 1,
        },
        WireCueAction::TitleOff {
            input: target(TITLE_INPUT),
        },
        WireCueAction::FadeToBlack {
            active: true,
            duration_frames: 25,
        },
        WireCueAction::PlaylistStart,
        WireCueAction::PlaylistStop,
        WireCueAction::AudioGain {
            input: target(1),
            gain: fm_protocol::CueGain::Set(-6_000),
        },
        WireCueAction::AudioMute {
            input: target(2),
            mute: fm_protocol::CueMute::Set(true),
        },
        WireCueAction::Record {
            recorder: WireRecorderSlot::new(1).unwrap(),
            start: true,
        },
        WireCueAction::Stream {
            target: WireStreamTargetId::new(NonZeroU128::new(1).unwrap()),
            start: false,
        },
        // RC-004: actions on the inputs on Program and Preview when the
        // GO fires.
        WireCueAction::AudioGain {
            input: fm_protocol::WireInputTarget::Program,
            gain: fm_protocol::CueGain::Set(0),
        },
        // RC-004 dynamic values: a step and a toggle (a cue never reads
        // the firing control).
        WireCueAction::AudioGain {
            input: fm_protocol::WireInputTarget::Program,
            gain: fm_protocol::CueGain::Step(3_000),
        },
        WireCueAction::AudioMute {
            input: fm_protocol::WireInputTarget::Preview,
            mute: fm_protocol::CueMute::Toggle,
        },
        WireCueAction::Media {
            input: fm_protocol::WireInputTarget::Preview,
            operation: WireCueMediaOperation::Play,
        },
        WireCueAction::Wait { frames: 50 },
    ];
    let cue = |id: u64, name: &str, auto_follow, actions| WireCue {
        id,
        name: name.into(),
        auto_follow,
        actions,
    };
    vec![
        CueListStatus {
            id: 1,
            spec: CueListSpec {
                name: "Show open".into(),
                cues: vec![
                    cue(10, "Opener", false, every_kind),
                    cue(
                        20,
                        "Guest",
                        true,
                        vec![WireCueAction::Take {
                            input: Some(target(4)),
                            effect: fm_protocol::TransitionFavoriteEffect::Cut,
                            duration_frames: 0,
                        }],
                    ),
                    // Protocol 2.51: the media transport actions and a
                    // millisecond delay, on the Guest media input.
                    cue(
                        30,
                        "Close",
                        false,
                        vec![
                            WireCueAction::Media {
                                input: target(4),
                                operation: WireCueMediaOperation::Restart,
                            },
                            WireCueAction::Delay { millis: 1_500 },
                            WireCueAction::Media {
                                input: target(4),
                                operation: WireCueMediaOperation::Seek { frame: 250 },
                            },
                            WireCueAction::Media {
                                input: target(4),
                                operation: WireCueMediaOperation::Play,
                            },
                            WireCueAction::Media {
                                input: target(4),
                                operation: WireCueMediaOperation::Pause,
                            },
                            WireCueAction::RunScript { script: 1 },
                        ],
                    ),
                ],
            },
            state: CueRunStatus {
                standby: Some(20),
                live: Some(10),
                pending: None,
                played: vec![10],
                fault: Some(fm_protocol::CueFaultStatus {
                    cue: 30,
                    action: 0,
                    detail: fm_protocol::FaultDetail {
                        reason: Some(fm_protocol::RefusalReason::StingerRealizing { slot: 2 }),
                        message: "Stinger slot 2 has not finished realizing".to_owned(),
                    },
                }),
                // Protocol 2.64: the standby cue is armed to fire later.
                // Protocol 2.89: the armed GO and the run show what their
                // dynamic targets are bound to.
                armed: Some(fm_protocol::CueArmedStatus {
                    cue: 20,
                    due_frame: 900,
                    targets: Some(fm_protocol::WireTargetBindings {
                        preview: Some(fm_protocol::WireInputId::from_domain(input(2))),
                        program: Some(fm_protocol::WireInputId::from_domain(input(1))),
                        selected: None,
                    }),
                }),
                targets: Some(fm_protocol::WireTargetBindings {
                    preview: Some(fm_protocol::WireInputId::from_domain(input(2))),
                    program: Some(fm_protocol::WireInputId::from_domain(input(1))),
                    selected: Some(fm_protocol::WireInputId::from_domain(input(3))),
                }),
            },
        },
        CueListStatus {
            id: 2,
            spec: CueListSpec {
                name: "Backup".into(),
                cues: vec![cue(
                    1,
                    "Hold",
                    false,
                    vec![
                        WireCueAction::Wait { frames: 500 },
                        WireCueAction::Preview { input: target(1) },
                    ],
                )],
            },
            state: CueRunStatus {
                standby: None,
                live: Some(1),
                pending: Some(CuePendingStatus {
                    cue: 1,
                    action: 1,
                    due_frame: 600,
                }),
                played: vec![1],
                fault: None,
                armed: None,
                targets: None,
            },
        },
    ]
}

/// Which optional live states the fixture show is in.
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // independent fixture switches
pub(crate) struct Variant {
    manual: bool,
    locked: bool,
    black: bool,
    playing: bool,
    /// A timed transition is in flight (Protocol 2.86).
    transitioning: bool,
}

/// OR-011: slot 1 shows Clean Feed on a connected display (remembered by
/// id and name, at an exact mode, filled, HDR auto) and is active; slot 2
/// shows Preview on a display named `Projector` that is unplugged.
/// The fixture's configured stream destinations (OR-006).
fn stream_target_entries() -> Vec<fm_protocol::StreamTargetEntry> {
    use fm_protocol::{
        StreamReconnect, StreamTargetConfig, StreamTargetEntry, StreamTargetProtocol,
    };
    vec![
        StreamTargetEntry {
            target: WireStreamTargetId::new(NonZeroU128::new(1).unwrap()),
            config: StreamTargetConfig {
                name: "Local HLS Ladder".into(),
                protocol: StreamTargetProtocol::Hls,
                endpoint: "/var/www/hls/show".into(),
                backup_endpoint: None,
                output: output(1),
                reconcile_on_startup: true,
                video_bitrate_kbps: 6_000,
                reconnect: StreamReconnect::DEFAULT,
                srt_latency_ms: None,
            },
            has_key: false,
            ladder_rungs: 3,
            has_passphrase: false,
        },
        StreamTargetEntry {
            target: WireStreamTargetId::new(NonZeroU128::new(2).unwrap()),
            config: StreamTargetConfig {
                name: "RTMP Backup".into(),
                protocol: StreamTargetProtocol::Rtmps,
                endpoint: "live.example.com/app".into(),
                backup_endpoint: Some("backup.example.com/app".into()),
                output: output(2),
                reconcile_on_startup: false,
                video_bitrate_kbps: 4_500,
                reconnect: StreamReconnect {
                    max_attempts: None,
                    ..StreamReconnect::DEFAULT
                },
                srt_latency_ms: None,
            },
            has_key: true,
            ladder_rungs: 0,
            has_passphrase: false,
        },
    ]
}

/// The latest transport sample of the running HLS destination.
fn stream_status() -> fm_protocol::StreamStatusMessage {
    fm_protocol::StreamStatusMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "1".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        sequence: 1,
        samples: vec![fm_protocol::StreamStatusSample {
            target: WireStreamTargetId::new(NonZeroU128::new(1).unwrap()),
            realized: StreamRealizedState::WaitingToReconnect,
            connected: false,
            muxed_bytes: 12 * 1_024 * 1_024,
            enqueued_pairs: 3,
            dropped_pairs: 17,
            failure: Some(fm_protocol::OutputFailureCode::DiskFull.into()),
            local: None,
            bitrate_kbps: 5_800,
            reconnects: 2,
            uptime_ms: 3_725_000,
        }],
    }
}

fn display_output_entries() -> Vec<fm_protocol::DisplayOutputEntry> {
    use fm_protocol::{
        DisplayId, DisplayMode, DisplayOutputConfig, DisplayOutputEntry, DisplayOutputSlot,
        DisplayOutputSource, DisplaySelector, HdrRequest, ModeRequest,
    };
    vec![
        DisplayOutputEntry {
            slot: DisplayOutputSlot::new(1).unwrap(),
            config: DisplayOutputConfig {
                display: DisplaySelector::IdOrName {
                    id: DisplayId::new("edid:left").unwrap(),
                    name: "Studio Left".into(),
                },
                source: DisplayOutputSource::Output(output(2).to_domain()),
                mode: ModeRequest::Exact(DisplayMode::new(1920, 1080, 59_940)),
                scaling: fm_types::OutputScaling::Fill,
                hdr: HdrRequest::Auto,
            },
        },
        DisplayOutputEntry {
            slot: DisplayOutputSlot::new(2).unwrap(),
            config: DisplayOutputConfig {
                source: DisplayOutputSource::Preview,
                ..DisplayOutputConfig::program_on(DisplaySelector::Name("Projector".into()))
            },
        },
    ]
}

fn display_output_states() -> Vec<fm_protocol::DisplayOutputStatus> {
    use fm_protocol::{
        DisplayId, DisplayMode, DisplayOutputSlot, DisplayOutputStatus, HdrState, WaitReason,
        WireActiveDisplay, WireDisplayOutputState,
    };
    vec![
        DisplayOutputStatus {
            slot: DisplayOutputSlot::new(1).unwrap(),
            state: WireDisplayOutputState::Active(WireActiveDisplay {
                id: DisplayId::new("edid:left").unwrap(),
                name: "Studio Left".into(),
                mode: DisplayMode::new(1920, 1080, 59_940),
                mode_fallback: false,
                hdr: HdrState::Hdr,
            }),
        },
        DisplayOutputStatus {
            slot: DisplayOutputSlot::new(2).unwrap(),
            state: WireDisplayOutputState::Waiting(WaitReason::DisplayAbsent),
        },
    ]
}

/// One capture format of the fixture devices.
fn capture_format(
    (width, height): (u32, u32),
    (numerator, denominator): (u32, u32),
    pixel_format: fm_protocol::CapturePixelFormat,
    scan: fm_protocol::CaptureScan,
) -> fm_protocol::CaptureFormat {
    fm_protocol::CaptureFormat {
        width,
        height,
        rate: FrameRate::new(numerator, denominator).unwrap(),
        pixel_format,
        scan,
    }
}

/// The capture inputs of the fixture (IN-001): Guest (input 4) lost its
/// unplugged camera, Slides (input 3) captures live.
fn capture_entries() -> Vec<fm_protocol::CaptureInputEntry> {
    use fm_protocol::{
        CaptureInputConfig, CaptureInputEntry, CaptureLossReason, CapturePixelFormat, CaptureScan,
        CaptureSignalState, CaptureStatus,
    };
    vec![
        CaptureInputEntry {
            input: wire(3),
            config: CaptureInputConfig::new("v4l2:pci-0000:03:00.0:decklink"),
            status: CaptureStatus {
                state: CaptureSignalState::Live,
                device: Some("/dev/video2".into()),
                format: Some(capture_format(
                    (1920, 1080),
                    (30_000, 1_001),
                    CapturePixelFormat::Yuyv,
                    CaptureScan::InterlacedTff,
                )),
                audio: true,
                frames: 9_000,
                ..CaptureStatus::default()
            },
        },
        CaptureInputEntry {
            input: wire(4),
            config: CaptureInputConfig::new("v4l2:usb-0000:00:14.0-1:C920"),
            status: CaptureStatus {
                state: CaptureSignalState::SignalLost,
                reason: Some(CaptureLossReason::Unplugged),
                losses: 2,
                recoveries: 1,
                renegotiations: 1,
                last_error: Some("No such device".into()),
                ..CaptureStatus::default()
            },
        },
    ]
}

/// The daemon's capture inventory of the fixture: a webcam with paired
/// audio and several formats, and a capture card without paired audio.
pub(crate) fn capture_inventory() -> fm_protocol::CaptureInventory {
    use fm_protocol::{CaptureDevice, CaptureInventory, CapturePixelFormat, CaptureScan};
    let progressive = CaptureScan::Progressive;
    CaptureInventory {
        backend: Some("v4l2".into()),
        error: None,
        devices: vec![
            CaptureDevice {
                stable_key: "v4l2:usb-0000:00:14.0-1:C920".into(),
                name: "HD Pro Webcam C920".into(),
                path: "/dev/video0".into(),
                driver: "uvcvideo".into(),
                bus: "usb-0000:00:14.0-1".into(),
                audio: Some("hw:CARD=C920,DEV=0".into()),
                formats: vec![
                    capture_format(
                        (1920, 1080),
                        (30, 1),
                        CapturePixelFormat::Mjpeg,
                        progressive,
                    ),
                    capture_format((1920, 1080), (5, 1), CapturePixelFormat::Yuyv, progressive),
                    capture_format((1280, 720), (60, 1), CapturePixelFormat::Mjpeg, progressive),
                    capture_format((640, 480), (30, 1), CapturePixelFormat::Yuyv, progressive),
                ],
            },
            CaptureDevice {
                stable_key: "v4l2:pci-0000:03:00.0:decklink".into(),
                name: "DeckLink Mini Recorder".into(),
                path: "/dev/video2".into(),
                driver: "decklink".into(),
                bus: "pci-0000:03:00.0".into(),
                audio: None,
                formats: vec![
                    capture_format((3840, 2160), (30, 1), CapturePixelFormat::Nv12, progressive),
                    capture_format(
                        (1920, 1080),
                        (30_000, 1_001),
                        CapturePixelFormat::Yuyv,
                        CaptureScan::InterlacedTff,
                    ),
                    capture_format(
                        (720, 576),
                        (25, 1),
                        CapturePixelFormat::Yuyv,
                        CaptureScan::InterlacedBff,
                    ),
                ],
            },
        ],
    }
}

/// `state` with `inventory` as the daemon's capture devices.
fn with_capture_inventory(
    mut state: StudioUiState,
    inventory: Option<fm_protocol::CaptureInventory>,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.capture_inventory = inventory;
    }
    state
}

/// The Add input dialog's camera / capture form (IN-001): a webcam chosen
/// with a filtered format and an ALSA device typed; a capture card with an
/// interlaced format, its absent paired audio, and the black loss slate;
/// and the inventory not yet known, without a capture backend, and after a
/// failed scan.
fn capture_scenes(locale: Locale) -> Vec<Scene> {
    use crate::capture_input_ui::AudioChoice;
    use crate::input_catalogue::{AddInputDraft, NewInputKind};
    let operator = ready(true, Variant::default());
    let inventory = capture_inventory();
    let shell = |configure: &dyn Fn(&mut AddInputDraft)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_add_input(&operator);
        if let Some(draft) = shell.add_input.as_mut() {
            draft.kind = NewInputKind::Capture;
            configure(draft);
        }
        shell
    };
    let webcam = inventory.devices[0].clone();
    let card = inventory.devices[1].clone();
    let failed = fm_protocol::CaptureInventory {
        backend: Some("v4l2".into()),
        error: Some("permission denied on /dev/video1".into()),
        devices: Vec::new(),
    };
    vec![
        Scene::console(
            "dialog/add-input-capture",
            shell(&|draft| {
                draft.capture.select_device(&webcam);
                draft.capture.format = Some(webcam.formats[0]);
                draft.capture.advanced = true;
                draft.capture.filter.pixel = Some(fm_protocol::CapturePixelFormat::Mjpeg);
                draft.capture.audio = AudioChoice::Alsa;
                draft.capture.alsa_device = "hw:CARD=USB,DEV=0".into();
            }),
            with_capture_inventory(operator.clone(), Some(inventory.clone())),
        ),
        Scene::console(
            "dialog/add-input-capture-card",
            shell(&|draft| {
                draft.capture.select_device(&card);
                draft.capture.format = Some(card.formats[1]);
                draft.capture.loss = fm_protocol::CaptureLossSlate::Black;
            }),
            with_capture_inventory(operator.clone(), Some(inventory.clone())),
        ),
        Scene::console(
            "dialog/add-input-capture-none-audio",
            shell(&|draft| {
                draft.capture.select_device(&webcam);
                draft.capture.audio = AudioChoice::None;
            }),
            with_capture_inventory(operator.clone(), Some(inventory.clone())),
        ),
        Scene::console(
            "dialog/add-input-capture-unknown",
            shell(&|_| {}),
            with_capture_inventory(operator.clone(), None),
        ),
        Scene::console(
            "dialog/add-input-capture-no-backend",
            shell(&|_| {}),
            with_capture_inventory(
                operator.clone(),
                Some(fm_protocol::CaptureInventory::default()),
            ),
        ),
        Scene::console(
            "dialog/add-input-capture-scan-failed",
            shell(&|_| {}),
            with_capture_inventory(operator.clone(), Some(failed)),
        ),
    ]
}

/// The server's system audio devices of the fixture (Protocol 2.68): the
/// default output's monitor, a USB microphone, and an ALSA loopback.
pub(crate) fn system_audio_inventory() -> fm_protocol::SystemAudioInventory {
    use fm_protocol::{SystemAudioBackend, SystemAudioDevice};
    let device = |backend, device: &str, description: &str, channels| SystemAudioDevice {
        backend,
        device: device.into(),
        description: description.into(),
        channels,
        default: false,
        monitor: false,
    };
    let mut monitor = device(
        SystemAudioBackend::Pulse,
        "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        "Monitor of Built-in Audio Analog Stereo",
        Some(2),
    );
    monitor.monitor = true;
    let mut microphone = device(
        SystemAudioBackend::Pulse,
        "alsa_input.usb-Blue_Yeti-00.mono-fallback",
        "Yeti Stereo Microphone Mono",
        Some(1),
    );
    microphone.default = true;
    fm_protocol::SystemAudioInventory {
        supported: true,
        devices: vec![
            microphone,
            monitor,
            device(
                SystemAudioBackend::Alsa,
                "plughw:Loopback,1",
                "Loopback: Loopback PCM",
                None,
            ),
        ],
        errors: Vec::new(),
    }
}

/// `state` with `inventory` as the server's system audio devices.
fn with_system_audio_inventory(
    mut state: StudioUiState,
    inventory: Option<fm_protocol::SystemAudioInventory>,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.system_audio_inventory = inventory;
    }
    state
}

/// The system audio form (AU-011) with the server's device list: the
/// default device, a listed device, and a typed name; a listing that failed,
/// one not yet known, and a server without system audio; and Replace source
/// editing a system audio input's device.
fn system_audio_scenes(locale: Locale) -> Vec<Scene> {
    use crate::input_catalogue::{AddInputDraft, NewInputKind};
    use fm_protocol::{SystemAudioBackend, SystemAudioBackendError, SystemAudioConfig};
    let operator = ready(true, Variant::default());
    let inventory = system_audio_inventory();
    let shell = |configure: &dyn Fn(&mut AddInputDraft)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_add_input(&operator);
        if let Some(draft) = shell.add_input.as_mut() {
            draft.kind = NewInputKind::SystemAudio;
            configure(draft);
        }
        shell
    };
    let listed = || with_system_audio_inventory(operator.clone(), Some(inventory.clone()));
    let mut failed = inventory.clone();
    failed
        .devices
        .retain(|device| device.backend == SystemAudioBackend::Alsa);
    failed.errors = vec![SystemAudioBackendError {
        backend: SystemAudioBackend::Pulse,
        error: "pactl failed: Connection refused".into(),
    }];
    let microphone = inventory.devices[0].clone();
    let mut replace = StudioShell::default();
    replace.set_locale(locale);
    replace.edit_system_audio_source(
        input(2),
        "Camera 2",
        &SystemAudioConfig::new(SystemAudioBackend::Alsa, "plughw:Loopback,1"),
    );
    vec![
        Scene::console(
            "dialog/add-input-system-audio-default",
            shell(&|_| {}),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-system-audio-listed",
            shell(&|draft| draft.system_audio.choose(&microphone)),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-system-audio-typed",
            shell(&|draft| {
                draft.system_audio.set_backend(SystemAudioBackend::Alsa);
                draft.system_audio.device = "hw:CARD=Other,DEV=0".into();
                draft.system_audio.device_edited = true;
            }),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-system-audio-failed",
            shell(&|_| {}),
            with_system_audio_inventory(operator.clone(), Some(failed)),
        ),
        Scene::console(
            "dialog/add-input-system-audio-unknown",
            shell(&|_| {}),
            with_system_audio_inventory(operator.clone(), None),
        ),
        Scene::console(
            "dialog/add-input-system-audio-unsupported",
            shell(&|_| {}),
            with_system_audio_inventory(
                operator.clone(),
                Some(fm_protocol::SystemAudioInventory::unsupported()),
            ),
        ),
        Scene::console("dialog/replace-source-system-audio", replace, listed()),
    ]
}

/// `state` with `inventory` as the server's headphone output devices and
/// `device` as the chosen headphone device.
fn with_audio_outputs(
    mut state: StudioUiState,
    inventory: Option<fm_protocol::AudioOutputInventory>,
    device: Option<&str>,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.audio_output_inventory = inventory;
        view.audio_mixer.monitor.device = device.map(str::to_owned);
    }
    state
}

/// The mixer's headphone device picker (AU-001, Protocol 2.102): a listed
/// device, an unplugged one, meter-only, the list not yet known, a listing
/// that failed, and a server without output devices (the typed field).
fn monitor_output_scenes(locale: Locale) -> Vec<Scene> {
    let operator = ready(true, Variant::default());
    let simulated = fm_protocol::AudioOutputInventory::simulated();
    let shell = || {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell
    };
    let failed = fm_protocol::AudioOutputInventory {
        supported: true,
        devices: Vec::new(),
        error: Some("no ALSA driver is loaded (no /proc/asound)".into()),
    };
    vec![
        Scene::console(
            "console/monitor-output-listed",
            shell(),
            with_audio_outputs(
                operator.clone(),
                Some(simulated.clone()),
                Some("plughw:CARD=Headset,DEV=0"),
            ),
        ),
        Scene::console(
            "console/monitor-output-unplugged",
            shell(),
            with_audio_outputs(
                operator.clone(),
                Some(simulated.clone()),
                Some("hw:CARD=Gone,DEV=0"),
            ),
        ),
        Scene::console(
            "console/monitor-output-meter-only",
            shell(),
            with_audio_outputs(operator.clone(), Some(simulated), None),
        ),
        Scene::console(
            "console/monitor-output-unknown",
            shell(),
            with_audio_outputs(operator.clone(), None, Some("default")),
        ),
        Scene::console(
            "console/monitor-output-failed-list",
            shell(),
            with_audio_outputs(operator.clone(), Some(failed), Some("default")),
        ),
        Scene::console(
            "console/monitor-output-unsupported",
            shell(),
            with_audio_outputs(
                operator,
                Some(fm_protocol::AudioOutputInventory::unsupported()),
                Some("hw:CARD=Headset,DEV=0"),
            ),
        ),
    ]
}

/// The audit's show plus slideshow input 9 ("Photos", three slides, the
/// second on air, its whole list fetched) (IN-009).
fn slideshow_model() -> ClientModel {
    use fm_protocol::{SlideshowConfig, SlideshowItem, SlideshowSource, WireStillSource};
    let mut model = model(Variant::default());
    let mut config = SlideshowConfig::new(SlideshowSource::List(vec![
        SlideshowItem::new("asset://photos/one.jpg"),
        SlideshowItem::new("asset://photos/two.jpg").with_caption("Second"),
        SlideshowItem::new("asset://photos/three.png"),
    ]));
    config.ken_burns = true;
    apply_audit_change(
        &mut model,
        fm_ui_model::DurableChange::InputAdded {
            input: input(9),
            name: "Photos".into(),
            network_stream: None,
            capture: None,
            system_audio: None,
            screen_capture: None,
            still: Some(Box::new(WireStillSource::Slideshow(config))),
        },
    );
    model
        .apply_runtime_slideshow_page(
            input(9),
            fm_protocol::SlideshowPageStatus {
                page: fm_protocol::SlideshowPage { index: 1, count: 3 },
                label: "Second".into(),
                generation: 1,
            },
        )
        .unwrap();
    model
        .apply_slideshow_slides(&fm_protocol::SlideshowSlidesMessage {
            request_id: "audit".into(),
            input: wire(9),
            generation: 1,
            offset: 0,
            total: 3,
            slides: ["one.jpg", "two.jpg", "three.png"]
                .into_iter()
                .map(|name| fm_protocol::SlideshowSlide {
                    asset_uri: format!("asset://photos/{name}"),
                    caption: (name == "two.jpg").then(|| "Second".to_owned()),
                })
                .collect(),
        })
        .unwrap();
    model
}

/// The Image and Slideshow forms of the Add input dialog (empty, filled,
/// and refused), and the Media tab's slideshow panel for an operator and a
/// viewer (IN-008, IN-009).
fn still_scenes(locale: Locale) -> Vec<Scene> {
    use crate::input_catalogue::{AddInputDraft, NewInputKind};
    let operator = ready(true, Variant::default());
    let shell = |configure: &dyn Fn(&mut AddInputDraft)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_add_input(&operator);
        if let Some(draft) = shell.add_input.as_mut() {
            configure(draft);
        }
        shell
    };
    let media = || {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.layout.dock.show(PanelId::Media);
        shell
    };
    let slideshow = slideshow_model();
    vec![
        Scene::console(
            "dialog/add-input-image-empty",
            shell(&|draft| draft.kind = NewInputKind::Image),
            operator.clone(),
        ),
        Scene::console(
            "dialog/add-input-image-filled",
            shell(&|draft| {
                draft.kind = NewInputKind::Image;
                draft.image.path = "/photos/logo.png".into();
            }),
            operator.clone(),
        ),
        Scene::console(
            "dialog/add-input-image-refused",
            shell(&|draft| {
                draft.kind = NewInputKind::Image;
                draft.image.path = "logo.txt".into();
            }),
            operator.clone(),
        ),
        Scene::console(
            "dialog/add-input-slideshow-folder",
            shell(&|draft| {
                draft.kind = NewInputKind::Slideshow;
                draft.slideshow.folder = "/photos/event".into();
            }),
            operator.clone(),
        ),
        Scene::console(
            "dialog/add-input-slideshow-list",
            shell(&|draft| {
                draft.kind = NewInputKind::Slideshow;
                draft.slideshow.folder_mode = false;
                draft.slideshow.pictures = "/photos/one.jpg\nclip.mov".into();
                draft.slideshow.fade = false;
            }),
            operator.clone(),
        ),
        Scene::console(
            "operator/media-slideshow",
            media(),
            ready_over(&slideshow, true),
        ),
        Scene::console(
            "viewer/media-slideshow",
            media(),
            ready_over(&slideshow, false),
        ),
    ]
}

/// What the fixture server can capture (IN-011): two screens and three
/// application windows, one of them untitled.
pub(crate) fn screen_capture_inventory() -> fm_protocol::ScreenCaptureInventory {
    use fm_protocol::{ScreenInfo, WindowInfo};
    let window = |id, app: &str, title: &str| WindowInfo {
        id,
        app: app.into(),
        title: title.into(),
        pid: Some(4_000 + u32::try_from(id).unwrap_or(0)),
        x: 40,
        y: 30,
        width: 1_280,
        height: 720,
    };
    fm_protocol::ScreenCaptureInventory {
        supported: true,
        backend: "x11".into(),
        screens: vec![
            ScreenInfo {
                index: 0,
                name: "DP-1".into(),
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
                primary: true,
            },
            ScreenInfo {
                index: 1,
                name: "HDMI-1".into(),
                x: 1_920,
                y: 0,
                width: 1_280,
                height: 1_024,
                primary: false,
            },
        ],
        windows: vec![
            window(41, "Keynote", "Quarterly deck"),
            window(42, "Firefox", "Live scores - Mozilla Firefox"),
            window(43, "Terminal", ""),
        ],
        error: None,
    }
}

/// Input 6 of the audit show capturing its Keynote window with its
/// application's sound in each audio state but off, the reason given for
/// the unsupported and failed states.
pub(crate) fn screen_capture_audio_entries() -> Vec<fm_protocol::ScreenCaptureEntry> {
    use fm_protocol::ScreenCaptureAudioState as Audio;
    [
        (Audio::Opening, None),
        (Audio::Live, None),
        (Audio::NoStream, None),
        (
            Audio::Unsupported,
            Some("per-application audio needs Windows 10 2004 or later"),
        ),
        (Audio::Failed, Some("parec exited with status 1")),
    ]
    .into_iter()
    .map(|(audio, error)| fm_protocol::ScreenCaptureEntry {
        input: wire(6),
        config: fm_protocol::ScreenCaptureConfig {
            audio: fm_protocol::ScreenCaptureAudio::Application,
            ..fm_protocol::ScreenCaptureConfig::new(fm_protocol::ScreenCaptureTarget::Window(
                fm_protocol::WindowRule::exact("Keynote", "Quarterly deck", Some(41)),
            ))
        },
        status: fm_protocol::ScreenCaptureStatus {
            state: fm_protocol::ScreenCaptureState::Live,
            width: 1_280,
            height: 720,
            audio,
            audio_error: error.map(str::to_owned),
            ..fm_protocol::ScreenCaptureStatus::default()
        },
    })
    .collect()
}

/// The audit console with input 6's window capture live and its
/// application's sound unsupported, and a drift loop on its strip, so its
/// tile and strip show the audio badge (the strip beside its SYNC key).
pub(crate) fn screen_capture_audio_fixture() -> StudioUiState {
    let mut state = ready(true, Variant::default());
    let entry = screen_capture_audio_entries().swap_remove(3);
    if let Some(view) = state.view.as_mut() {
        view.screen_capture_inputs = vec![entry];
    }
    if let Some(sync) = state.source_sync.as_mut()
        && let Some(sample) = sync.sources.first().copied()
    {
        sync.sources.push(fm_protocol::SourceSyncSample {
            input: wire(6),
            kind: fm_protocol::SourceSyncKind::ScreenCapture,
            ..sample
        });
    }
    state
}

/// `state` with `inventory` as what the server can capture.
fn with_screen_capture_inventory(
    mut state: StudioUiState,
    inventory: Option<fm_protocol::ScreenCaptureInventory>,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.screen_capture_inventory = inventory;
    }
    state
}

/// The screen capture form (IN-011) with the server's targets: a screen,
/// a picked window followed by its exact title, by a title text, and by
/// its application; a target that is not open; a listing not yet known;
/// a server that cannot capture its desktop; and a Wayland server whose own
/// dialog picks a screen or a window (one that cannot share windows).
#[allow(clippy::too_many_lines)] // one list of scenes
fn screen_capture_scenes(locale: Locale) -> Vec<Scene> {
    use crate::input_catalogue::{AddInputDraft, NewInputKind};
    use fm_protocol::TitleMatch;
    let operator = ready(true, Variant::default());
    let inventory = screen_capture_inventory();
    let shell = |configure: &dyn Fn(&mut AddInputDraft)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_add_input(&operator);
        if let Some(draft) = shell.add_input.as_mut() {
            draft.kind = NewInputKind::ScreenCapture;
            configure(draft);
        }
        shell
    };
    let listed = || with_screen_capture_inventory(operator.clone(), Some(inventory.clone()));
    let deck = inventory.windows[0].clone();
    // Replace source on a screen capture input opens at its capture.
    let replace = |config: fm_protocol::ScreenCaptureConfig| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.edit_screen_capture_source(input(2), "Camera 2", &config);
        shell
    };
    let follow = |title_match: TitleMatch| {
        let deck = deck.clone();
        move |draft: &mut AddInputDraft| {
            draft.screen_capture.choose_window(&deck);
            if let Some(rule) = draft.screen_capture.rule_mut() {
                rule.title_match = title_match;
            }
        }
    };
    vec![
        Scene::console(
            "dialog/add-input-screen-capture-screen",
            shell(&|_| {}),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-window",
            shell(&follow(TitleMatch::Exact)),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-window-app-audio",
            shell(&|draft| {
                follow(TitleMatch::Exact)(draft);
                draft.screen_capture.config.audio = fm_protocol::ScreenCaptureAudio::Application;
            }),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-system-audio",
            shell(&|draft| {
                draft.screen_capture.config.audio = fm_protocol::ScreenCaptureAudio::System;
            }),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-contains",
            shell(&|draft| {
                follow(TitleMatch::Contains)(draft);
                if let Some(rule) = draft.screen_capture.rule_mut() {
                    rule.title = "deck".into();
                    rule.window_id = None;
                }
                draft.screen_capture.config.crop.top = 28;
                draft.screen_capture.config.slate = fm_types::network_stream::StallSlate::Black;
            }),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-any",
            shell(&follow(TitleMatch::Any)),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-missing",
            shell(&|draft| {
                draft.screen_capture.config.target = fm_protocol::ScreenCaptureTarget::Window(
                    fm_protocol::WindowRule::exact("Slides", "Opening", None),
                );
            }),
            listed(),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-unknown",
            shell(&|_| {}),
            with_screen_capture_inventory(operator.clone(), None),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-unsupported",
            shell(&|_| {}),
            with_screen_capture_inventory(
                operator.clone(),
                Some(fm_protocol::ScreenCaptureInventory::unsupported(
                    "Wayland session: silent screen capture needs X11; log in to an X11 session",
                )),
            ),
        ),
        // The capture audio badge (IN-011) on input 6's tile and mixer
        // strip, beside its SYNC key, and every audio state's badge.
        Scene::console(
            "console/screen-capture-audio-badge",
            {
                let mut shell = StudioShell::default();
                shell.set_locale(locale);
                shell
            },
            screen_capture_audio_fixture(),
        ),
        Scene {
            name: "badge/screen-capture-audio-states".into(),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                for entry in screen_capture_audio_entries() {
                    crate::screen_capture_ui::audio_badge_key(
                        ui,
                        locale,
                        &entry,
                        egui::vec2(120.0, 16.0),
                    );
                }
            }),
        },
        Scene::console(
            "dialog/replace-source-screen-capture-window",
            replace(fm_protocol::ScreenCaptureConfig {
                audio: fm_protocol::ScreenCaptureAudio::Application,
                cursor: false,
                fps: 15,
                slate: fm_types::network_stream::StallSlate::Black,
                ..fm_protocol::ScreenCaptureConfig::new(fm_protocol::ScreenCaptureTarget::Window(
                    fm_protocol::WindowRule::exact("Keynote", "Quarterly deck", Some(41)),
                ))
            }),
            listed(),
        ),
        Scene::console(
            "dialog/replace-source-screen-capture-screen",
            replace(fm_protocol::ScreenCaptureConfig::new(
                fm_protocol::ScreenCaptureTarget::Screen { index: 1 },
            )),
            listed(),
        ),
    ]
    .into_iter()
    .chain(screen_capture_portal_scenes(locale))
    .collect()
}

/// The screen capture form on a Wayland server whose own dialog picks a
/// screen or a window (one that cannot share windows).
fn screen_capture_portal_scenes(locale: Locale) -> Vec<Scene> {
    use crate::input_catalogue::{AddInputDraft, NewInputKind};
    let operator = ready(true, Variant::default());
    let shell = |configure: &dyn Fn(&mut AddInputDraft)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_add_input(&operator);
        if let Some(draft) = shell.add_input.as_mut() {
            draft.kind = NewInputKind::ScreenCapture;
            configure(draft);
        }
        shell
    };
    vec![
        Scene::console(
            "dialog/add-input-screen-capture-portal-screen",
            shell(&|_| {}),
            with_screen_capture_inventory(operator.clone(), Some(portal_inventory(None))),
        ),
        Scene::console(
            "dialog/add-input-screen-capture-portal-window",
            shell(&|draft| draft.screen_capture.pick_on_server(true)),
            with_screen_capture_inventory(
                operator.clone(),
                Some(portal_inventory(Some(
                    "this desktop's sharing dialog cannot share single windows; capture a \
                     screen instead",
                ))),
            ),
        ),
    ]
}

/// What a Wayland server (IN-011, Protocol 2.85) can capture: nothing
/// listed, its own sharing dialog picks; `limit` is what it cannot share.
fn portal_inventory(limit: Option<&str>) -> fm_protocol::ScreenCaptureInventory {
    fm_protocol::ScreenCaptureInventory {
        supported: true,
        backend: fm_protocol::PORTAL_BACKEND.into(),
        screens: Vec::new(),
        windows: Vec::new(),
        error: limit.map(str::to_owned),
    }
}

/// `state` with the daemon's display inventory: two displays, or a daemon
/// without a presenter.
fn with_display_inventory(mut state: StudioUiState, presenter: bool) -> StudioUiState {
    use fm_protocol::{
        DisplayId, DisplayInventory, DisplayInventoryEntry, DisplayMode, HdrCapability,
    };
    let mode = DisplayMode::new;
    let displays = if presenter {
        vec![
            DisplayInventoryEntry {
                id: DisplayId::new("edid:left").unwrap(),
                name: "Studio Left".into(),
                x: 0,
                y: 0,
                current_mode: mode(1920, 1080, 59_940),
                modes: vec![mode(3840, 2160, 60_000), mode(1920, 1080, 59_940)],
                hdr: HdrCapability::Hdr,
            },
            DisplayInventoryEntry {
                id: DisplayId::new("edid:wall").unwrap(),
                name: "Wall".into(),
                x: 1920,
                y: 0,
                current_mode: mode(1280, 720, 50_000),
                modes: vec![mode(1280, 720, 50_000)],
                hdr: HdrCapability::Sdr,
            },
        ]
    } else {
        Vec::new()
    };
    if let Some(view) = state.view.as_mut() {
        view.display_inventory = Some(DisplayInventory {
            presenter,
            error: None,
            displays,
        });
    }
    state
}

/// `state` with the daemon's web access answer `answer`.
fn with_web_access(
    mut state: StudioUiState,
    answer: Option<fm_protocol::WebAccessMessage>,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.web_access = answer;
    }
    state
}

/// A TLS gateway with a pairing under way and two paired devices.
pub(crate) fn web_access_answer() -> fm_protocol::WebAccessMessage {
    use fm_protocol::{WebDeviceEntry, WebDeviceRole, WebGatewayStatus, WebPairing};
    fm_protocol::WebAccessMessage {
        request_id: "audit".into(),
        refusal: None,
        gateway: Some(WebGatewayStatus {
            url: "https://192.168.1.20:8443".into(),
            tls: true,
            fingerprint: Some(
                "3F:9A:1C:77:0B:E2:54:D8:61:AF:20:C3:9E:4B:7D:15:\
                 88:02:F6:3B:CA:19:E5:70:4D:A2:6B:31:DE:8C:05:97"
                    .replace(' ', ""),
            ),
            trust: fm_protocol::WebTrust::LocalCa,
            ca_fingerprint: Some(
                "C4:1E:07:9B:52:A8:3D:F0:16:6C:E9:24:B7:5A:08:D3:\
                 71:FE:2B:94:0C:65:A1:3E:D8:47:B2:19:6F:E0:53:8A"
                    .replace(' ', ""),
            ),
            cert_not_after_ms: Some(1_826_000_000_000),
            cert_problem: None,
            urls: vec![
                "https://freemix-studio.local:8443".into(),
                "https://10.0.0.9:8443".into(),
            ],
            mdns: Some("freemix-studio.local".into()),
            pairing: Some(WebPairing {
                code: "042137".into(),
                link_code: "q8Zr2pVfK3mN9xTb7LwYc4".into(),
                role: WebDeviceRole::Operator,
                expires_in_ms: 94_200,
            }),
            devices: vec![
                WebDeviceEntry {
                    id: "dev-1a2b".into(),
                    name: "Floor manager phone".into(),
                    role: WebDeviceRole::Operator,
                    paired_at_ms: 1_792_400_000_000,
                    last_seen_ms: Some(1_792_403_600_000),
                },
                WebDeviceEntry {
                    id: "dev-9c8d".into(),
                    name: "Camera 2 tally".into(),
                    role: WebDeviceRole::Viewer,
                    paired_at_ms: 1_792_400_100_000,
                    last_seen_ms: None,
                },
            ],
        }),
    }
}

/// `state` with `LiveLAN` configured as `config` and reporting `status`.
fn with_live_lan(
    mut state: StudioUiState,
    config: fm_protocol::LiveLanEntry,
    status: &fm_protocol::LiveLanStatus,
) -> StudioUiState {
    if let Some(view) = state.view.as_mut() {
        view.live_lan = config;
        view.live_lan_status = status.for_config(config.enabled);
    }
    state
}

fn meters() -> AudioMetersMessage {
    let channel = |peak| AudioMeterChannel {
        peak_millionths: peak,
        rms_millionths: peak / 2,
    };
    AudioMetersMessage {
        aux: Vec::new(),
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "1".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        sequence: 1,
        frame: 1,
        start_sample: 0,
        end_sample: 1_600,
        // Full scale latches the clip lamps on Master and input 1.
        master: vec![channel(1_000_000), channel(400_000)],
        inputs: (1..=6)
            .map(|value| InputAudioMeters {
                input: wire(value),
                channels: vec![channel(if value == 1 { 1_000_000 } else { 300_000 }); 2],
            })
            .collect(),
        // The headphone device failed, so its retry help is audited.
        monitor: Some(fm_protocol::AudioMonitorMeters {
            solo: true,
            output: fm_protocol::AudioMonitorOutputState::Failed,
            channels: vec![channel(300_000); 2],
        }),
        // The Master compressor and limiter and input 1's gate are working
        // (AU-005), so the FX window's meters show a reduction.
        dynamics: vec![
            fm_protocol::AudioDynamicsMeter {
                target: fm_protocol::AudioDspTarget::Master,
                gate_millidb: None,
                expander_millidb: None,
                compressor_millidb: Some(4_500),
                limiter_millidb: Some(1_200),
            },
            fm_protocol::AudioDynamicsMeter {
                target: fm_protocol::AudioDspTarget::Input(wire(1)),
                gate_millidb: Some(30_000),
                expander_millidb: Some(6_000),
                compressor_millidb: Some(14_000),
                limiter_millidb: None,
            },
        ],
        // Master and input 1 report loudness (AU-005); input 1 has not yet
        // filled its 3 s window, and other strips report none, so the
        // idle readout's help is audited too.
        loudness: vec![
            fm_protocol::AudioLoudnessMeter {
                target: fm_protocol::AudioDspTarget::Master,
                momentary_millilufs: Some(-22_400),
                short_term_millilufs: Some(-23_100),
                integrated_millilufs: Some(-23_000),
                true_peak_millidbtp: Some(-2_300),
                max_true_peak_millidbtp: Some(-800),
                loudness_range_millilu: Some(7_400),
            },
            fm_protocol::AudioLoudnessMeter {
                target: fm_protocol::AudioDspTarget::Input(wire(1)),
                momentary_millilufs: Some(-28_000),
                short_term_millilufs: None,
                integrated_millilufs: None,
                true_peak_millidbtp: Some(-9_000),
                max_true_peak_millidbtp: Some(-9_000),
                loudness_range_millilu: None,
            },
        ],
    }
}

fn title_element(value: u128) -> fm_protocol::WireTitleElementId {
    fm_protocol::WireTitleElementId::new(NonZeroU128::new(value).unwrap())
}

/// The lower third's animation: the bar slides and fades in, the name
/// fades in after it, and both fade out.
fn title_clip() -> fm_protocol::WireTitleClip {
    let keys = |from: i64, to: i64, end: u32| {
        vec![
            Keyframe::new(0, Value::Scalar(from), Easing::Linear),
            Keyframe::new(end, Value::Scalar(to), Easing::CubicOut),
        ]
    };
    let track = |element: u128, phase, property, keys| {
        Track::new(TrackKey::new(title_element(element), phase, property), keys)
    };
    Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        20,
        10,
        vec![
            track(1, Phase::In, Property::Opacity, keys(0, 255, 12)),
            track(1, Phase::In, Property::PositionX, keys(-400, 0, 16)),
            track(2, Phase::In, Property::Opacity, keys(0, 255, 20)),
            track(1, Phase::Out, Property::Opacity, keys(255, 0, 10)),
            track(2, Phase::Out, Property::Opacity, keys(255, 0, 8)),
        ],
    )
    .unwrap()
}

/// The lower third's design: its bar and name.
/// The Scene input of the audit (the slides input).
const SCENE_INPUT: u128 = 3;

fn scene_layer(value: u32) -> fm_protocol::WireSceneLayerId {
    fm_protocol::WireSceneLayerId::new(core::num::NonZeroU32::new(value).unwrap())
}

/// The scene's animation: the host layer slides in from the left.
fn scene_clip() -> fm_protocol::WireSceneClip {
    let mut clip = fm_protocol::WireSceneClip::new(
        fm_animation::Timebase::new(FrameRate::new(30, 1).unwrap()),
        0,
        0,
    )
    .unwrap();
    clip.set_phase_frames_keeping_keys(Phase::In, 15).unwrap();
    clip.apply_preset(
        scene_layer(1),
        Phase::In,
        fm_animation::AnimationPreset::SlideFromLeft,
        15,
        fm_animation::PresetGeometry {
            x: 0,
            y: 270,
            width: 960,
            height: 540,
            canvas_width: 1_920,
            canvas_height: 1_080,
        },
    )
    .unwrap();
    clip
}

/// Two side-by-side layers on a 1080p canvas: the host chroma-keyed with
/// one effect of every kind, the guest luma-keyed (IN-020).
fn scene_layout() -> fm_protocol::WireSceneLayout {
    let outline = |id: u32, name: &str, x: i32| fm_protocol::WireSceneLayerOutline {
        id: scene_layer(id),
        name: name.into(),
        x,
        y: 270,
        width: 960,
        height: 540,
        quarter_turns: 0,
        opacity: 255,
        enabled: true,
        tilt_millidegrees: 0,
        anchor_x: 5_000,
        anchor_y: 5_000,
        z_order: 0,
        crop: None,
        key: None,
        effects: Vec::new(),
        mask: None,
        source: fm_protocol::WireSceneLayerSource::Input(wire(u128::from(id))),
    };
    let mut host = outline(1, "Host", 0);
    host.key = Some(fm_protocol::WireLayerKey::Chroma(
        fm_ui_model::scene_designer::CHROMA_GREEN,
    ));
    host.effects = fm_protocol::SCENE_LAYER_EFFECT_KINDS
        .iter()
        .filter_map(|(name, _)| fm_ui_model::scene_designer::default_effect(name))
        .collect();
    let mut guest = outline(2, "Guest", 960);
    guest.key = Some(fm_protocol::WireLayerKey::Luma(
        fm_ui_model::scene_designer::LUMA_BLACK,
    ));
    fm_protocol::WireSceneLayout {
        width: 1_920,
        height: 1_080,
        layers: vec![host, guest],
    }
}

fn title_design() -> fm_ui_model::title_animation::TitleDesign {
    fm_ui_model::title_animation::TitleDesign {
        canvas: (1_920, 1_080),
        elements: vec![
            TargetOutline::new(
                title_element(1),
                "Bar",
                TargetKind::Shape,
                TargetBounds::new(120, 840, 900, 140),
            ),
            TargetOutline::new(
                title_element(2),
                "Name",
                TargetKind::Text,
                TargetBounds::new(160, 860, 700, 60),
            ),
            TargetOutline::new(
                title_element(3),
                "Logo",
                TargetKind::Image,
                TargetBounds::new(1_040, 840, 140, 140),
            ),
        ],
    }
}

/// A ready session over [`model`]; `operator` grants every permission.
pub(crate) fn ready(operator: bool, variant: Variant) -> StudioUiState {
    ready_over(&model(variant), operator)
}

/// A ready session over `model`; `operator` grants every permission.
fn ready_over(model: &ClientModel, operator: bool) -> StudioUiState {
    let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
        .with_view(model.view().unwrap())
        .with_switcher_permissions(operator, operator)
        .with_audio_permission(operator)
        .with_edit_project_permission(operator);
    state.project_state = model.state().cloned();
    state.title_access = TitleAccess::new(true, operator, operator);
    state.recording_desired = [true, false];
    state
        .streams
        .clone_from(&state.view.as_ref().unwrap().streams);
    state.stream_status = Some(stream_status());
    state.audio_meters = Some(meters());
    state.source_sync = Some(source_sync());
    state.health = Some(Box::new(daemon_health()));
    state.replay = Some(Box::new(replay_status()));
    state.set_reel_status(reel_status());
    // GX-006: the lower third plays its In phase on air.
    state.title_phases = Some(fm_protocol::TitlePhasesMessage {
        frame: 900,
        titles: vec![fm_protocol::TitlePhaseStatus {
            input: wire(TITLE_INPUT),
            phase: fm_protocol::TitlePhase::In,
            phase_frame: 5,
            phase_frames: 20,
            timeline: Some(fm_protocol::TitleTimelineStatus {
                shown: fm_protocol::TitleShownOn::Air,
                since_frame: 895,
            }),
        }],
    });
    // IN-014: the document card's notes and two of its thumbnails arrived.
    let page = |page: u32, thumbnail: bool| fm_protocol::PagedPageKey {
        input: wire(6),
        page,
        thumbnail,
    };
    state.paged_pages.insert(
        page(1, false),
        fm_protocol::PagedPageAnswer::Notes("Welcome everyone.\nIntroduce the panel.".into()),
    );
    for number in [0, 1] {
        state.paged_pages.insert(
            page(number, true),
            fm_protocol::PagedPageAnswer::Thumbnail {
                width: 4,
                height: 2,
                rgba: vec![200; 4 * 2 * 4].into(),
            },
        );
    }
    state
}

/// Daemon health (RC-015): the CPU lamp reads busy, the GPU lamp has no
/// estimate, and a critical output alert fires, so the console renders every
/// daemon lamp (hover-only lamps; their help is checked in
/// `daemon_health_ui`).
fn daemon_health() -> fm_protocol::HealthStatusMessage {
    fm_protocol::HealthStatusMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "1".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        sequence: 1,
        health: fm_protocol::DaemonHealth {
            uptime_ms: 30_000,
            cpu_permille: Some(810),
            gpu_permille: None,
            alerts: vec![fm_protocol::HealthAlert {
                rule: fm_protocol::HealthAlertRule::OutputFailed,
                severity: fm_protocol::HealthSeverity::Critical,
                since_ms: 25_000,
                value_milli: Some(1_000),
                acknowledged_until_ms: None,
            }],
            notifications: vec![fm_protocol::HealthNotification {
                sequence: 2,
                rule: fm_protocol::HealthAlertRule::OutputFailed,
                severity: fm_protocol::HealthSeverity::Critical,
                kind: fm_protocol::HealthNotificationKind::Firing,
                at_ms: 25_000,
                value_milli: Some(1_000),
            }],
            silences: Vec::new(),
        },
        request_id: None,
    }
}

/// OR-012: two recorded sources, a mark, a complete and a partial event,
/// and two Replay inputs, one playing and one with nothing cued.
pub(crate) fn replay_status() -> fm_protocol::ReplayStatusMessage {
    use fm_protocol::{
        ReplayEventStatus, ReplayFeed, ReplayPlayerStatus, ReplaySettings, ReplaySourceStatus,
        ReplaySpeed,
    };
    fm_protocol::ReplayStatusMessage {
        server: fm_protocol::ServerIdentity {
            engine_id: "audit".into(),
            project_id: "audit".into(),
            state_epoch: 1,
            log_id: "audit".into(),
        },
        sequence: 4,
        request_id: None,
        feed: ReplayFeed::Recorded,
        settings: ReplaySettings {
            sources: vec![wire(1).to_domain(), wire(2).to_domain()],
            retention_seconds: 600,
            storage_root: None,
            auto_return: None,
        },
        root: "/var/lib/freemix/replay".into(),
        live_frame: Some(3_000),
        mark_in: Some(2_900),
        mark_out: None,
        recorded: vec![
            ReplaySourceStatus {
                input: wire(1),
                segments: 60,
                bytes: 90_000_000,
                first_frame: Some(0),
                last_frame: Some(3_000),
            },
            ReplaySourceStatus {
                input: wire(2),
                segments: 1,
                bytes: 1_500_000,
                first_frame: Some(2_950),
                last_frame: Some(3_000),
            },
        ],
        events: vec![
            ReplayEventStatus {
                id: 1,
                name: "Goal".into(),
                in_frame: 2_500,
                out_frame: 2_750,
                angles: vec![wire(1), wire(2)],
                complete: true,
            },
            ReplayEventStatus {
                id: 2,
                name: "Last 5 s".into(),
                in_frame: 2_875,
                out_frame: 3_000,
                angles: vec![wire(1), wire(2)],
                complete: false,
            },
        ],
        players: vec![
            ReplayPlayerStatus {
                input: wire(4),
                event: Some(1),
                angle: Some(wire(2)),
                speed: ReplaySpeed::Half,
                playing: true,
                frame: Some(2_600),
                playlist: vec![
                    fm_protocol::ReplayPlaylistEntry {
                        event: 1,
                        angle: Some(wire(2)),
                        speed: ReplaySpeed::Half,
                    },
                    fm_protocol::ReplayPlaylistEntry {
                        event: 2,
                        angle: None,
                        speed: ReplaySpeed::Full,
                    },
                ],
                playlist_index: Some(0),
                linked: Some(wire(5)),
            },
            ReplayPlayerStatus {
                input: wire(5),
                event: None,
                angle: None,
                speed: ReplaySpeed::Full,
                playing: false,
                frame: None,
                playlist: Vec::new(),
                playlist_index: None,
                linked: Some(wire(4)),
            },
        ],
    }
}

/// One OR-013 export fixture row.
#[allow(clippy::too_many_arguments)] // one field per wire field
fn export_status(
    job: u64,
    reel: u64,
    destination: &str,
    state: fm_protocol::ReelExportState,
    completed_frames: u64,
    total_frames: Option<u64>,
    bytes: Option<u64>,
    failure: Option<&str>,
) -> fm_protocol::ReelExportStatus {
    fm_protocol::ReelExportStatus {
        job,
        reel,
        destination: destination.into(),
        state,
        completed_frames,
        total_frames,
        bytes,
        failure: failure.map(ToOwned::to_owned),
    }
}

/// OR-013: three reels (a complete one, one that does not compile, and one
/// whose length is still unknown) and one export job in every state, so the
/// REELS list, clip editors, and export rows are all audited.
#[allow(clippy::too_many_lines)] // one fixture row per reel and export state
pub(crate) fn reel_status() -> fm_protocol::ReelStatusMessage {
    use fm_protocol::{ReelClip, ReelExportState, ReelSpec, ReelStatus};
    let clip = |event, angle: u128, trim_in, trim_out, speed| ReelClip {
        event,
        angle: wire(angle).to_domain(),
        trim_in_frames: trim_in,
        trim_out_frames: trim_out,
        speed_percent: speed,
        dissolve_frames: 0,
    };
    let reel = |id, name: &str, clips, total_frames, error: Option<&str>| ReelStatus {
        id,
        spec: ReelSpec {
            id,
            name: name.into(),
            clips,
            music: None,
        },
        total_frames,
        error: error.map(ToOwned::to_owned),
    };
    // The goal reel plays a music bed (schema 86), so its levels draw.
    let scored = |reel: ReelStatus| ReelStatus {
        spec: ReelSpec {
            music: Some(fm_protocol::ReelMusic {
                path: "/srv/music/anthem.wav".into(),
                level_percent: 80,
                duck_percent: 25,
            }),
            ..reel.spec
        },
        ..reel
    };
    fm_protocol::ReelStatusMessage {
        server: fm_protocol::ServerIdentity {
            engine_id: "audit".into(),
            project_id: "audit".into(),
            state_epoch: 1,
            log_id: "audit".into(),
        },
        sequence: 5,
        request_id: None,
        reels: vec![
            scored(reel(
                1,
                "Goal reel",
                vec![
                    clip(1, 1, 0, 0, 100),
                    ReelClip {
                        dissolve_frames: 8,
                        ..clip(1, 2, 12, 0, 50)
                    },
                ],
                Some(250),
                None,
            )),
            reel(
                2,
                "Last plays",
                vec![clip(2, 1, 0, 25, 75)],
                None,
                Some("event 2 is only partly recorded"),
            ),
            reel(3, "Warm-up", vec![clip(1, 2, 0, 0, 25)], Some(0), None),
        ],
        exports: vec![
            export_status(
                7,
                1,
                "/srv/exports/goal-deferred.mp4",
                ReelExportState::Deferred,
                0,
                Some(250),
                None,
                None,
            ),
            export_status(
                1,
                1,
                "/srv/exports/goal-queued.mp4",
                ReelExportState::Queued,
                0,
                None,
                None,
                None,
            ),
            export_status(
                2,
                1,
                "/srv/exports/goal-running.mp4",
                ReelExportState::Running,
                120,
                Some(250),
                Some(1_048_576),
                None,
            ),
            export_status(
                3,
                2,
                "/srv/exports/last-finalizing.mp4",
                ReelExportState::Finalizing,
                250,
                Some(250),
                Some(2_097_152),
                None,
            ),
            export_status(
                4,
                2,
                "/srv/exports/last-completed.mp4",
                ReelExportState::Completed,
                250,
                Some(250),
                Some(2_097_152),
                None,
            ),
            export_status(
                5,
                3,
                "/srv/exports/warm-up-failed.mp4",
                ReelExportState::Failed,
                100,
                Some(250),
                Some(524_288),
                Some("destination is full"),
            ),
            export_status(
                6,
                3,
                "/srv/exports/warm-up-cancelled.mp4",
                ReelExportState::Cancelled,
                50,
                Some(250),
                Some(262_144),
                None,
            ),
        ],
    }
}

/// Live source drift telemetry (AU-004): input 2's loop runs clean, input
/// 3's recovers after underruns, so both `SYNC` keys and the amber health
/// lamp are audited.
fn source_sync() -> fm_protocol::SourceSyncStatusMessage {
    let sample = |id, state, underruns| fm_protocol::SourceSyncSample {
        input: wire(id),
        kind: fm_protocol::SourceSyncKind::Stream,
        state,
        input_rate_hz: 44_100,
        output_rate_hz: 48_000,
        drift_mppm: -12_345,
        correction_mppm: 12_001,
        buffer_us: 81_250,
        target_us: 80_000,
        resyncs: 0,
        underruns,
        overruns: 0,
        picture_drops: 4,
        picture_repeats: 0,
        restarts: 1,
    };
    fm_protocol::SourceSyncStatusMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "1".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        sequence: 1,
        sources: vec![
            sample(2, fm_protocol::SourceClockState::Running, 0),
            sample(3, fm_protocol::SourceClockState::Recovering, 2),
        ],
    }
}

/// Puts the console in one configuration before a scene renders.
type Configure<'a> = &'a dyn Fn(&mut StudioShell);

/// One audited configuration: a name and what draws it each pass.
struct Scene {
    name: String,
    draw: Box<dyn FnMut(&mut egui::Ui)>,
}

impl Scene {
    /// The console in `shell`'s configuration over `state`.
    fn console(name: impl Into<String>, mut shell: StudioShell, state: StudioUiState) -> Self {
        Self {
            name: name.into(),
            draw: Box::new(move |ui| {
                let _intents = shell.draw(ui, &state);
            }),
        }
    }
}

#[allow(clippy::too_many_lines)] // one list of scenes
fn scenes(language: Language) -> Vec<Scene> {
    let locale = Locale::new(language);
    let shell = |configure: &dyn Fn(&mut StudioShell)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        configure(&mut shell);
        shell
    };
    let mut scenes = Vec::new();
    let tabs = [
        ("overlays", PanelId::Overlays),
        ("stingers", PanelId::Stingers),
        ("titles", PanelId::Titles),
        ("playlist", PanelId::Playlist),
        ("media", PanelId::Media),
        ("cues", PanelId::Cues),
        ("replay", PanelId::Replay),
    ];
    for (operator, role) in [(true, "operator"), (false, "viewer")] {
        for (tab_name, tab) in tabs {
            scenes.push(Scene::console(
                format!("{role}/{tab_name}"),
                shell(&|shell| shell.layout.dock.show(tab)),
                ready(operator, Variant::default()),
            ));
        }
        // The advanced controls behind the mixer and manual T-bar
        // disclosures.
        scenes.push(Scene::console(
            format!("{role}/details"),
            shell(&|shell| {
                shell.layout.show_mixer_details = true;
                shell.layout.show_manual_details = true;
            }),
            ready(operator, Variant::default()),
        ));
    }
    // OR-013: the Replay panel under the project padlock (its reel edits
    // are disabled with the padlock reason while its exports stay live),
    // and the Delete confirmation open on the first reel.
    scenes.push(Scene::console(
        "operator/replay-locked",
        shell(&|shell| shell.layout.dock.show(PanelId::Replay)),
        ready(
            true,
            Variant {
                locked: true,
                ..Variant::default()
            },
        ),
    ));
    {
        let mut confirm = StudioShell::default();
        confirm.set_locale(locale);
        confirm.layout.dock.show(PanelId::Replay);
        let confirm_state = ready(true, Variant::default());
        scenes.push(Scene {
            name: "operator/replay-delete-confirm".into(),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                ui.data_mut(|data| {
                    data.insert_temp(crate::replay_ui::reel_delete_id(), 1_u64);
                });
                let _intents = confirm.draw(ui, &confirm_state);
            }),
        });
    }
    // OR-013: only terminal exports remain and an absolute destination is
    // typed, so Export is enabled (its live help, not just its refusal, is
    // audited) and every export row offers Forget.
    {
        let mut export = StudioShell::default();
        export.set_locale(locale);
        export.layout.dock.show(PanelId::Replay);
        let mut export_state = ready(true, Variant::default());
        export_state.reel_exports.retain(|row| {
            !matches!(
                row.state,
                fm_protocol::ReelExportState::Deferred
                    | fm_protocol::ReelExportState::Queued
                    | fm_protocol::ReelExportState::Running
                    | fm_protocol::ReelExportState::Finalizing
            )
        });
        scenes.push(Scene {
            name: "operator/replay-export-ready".into(),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                ui.data_mut(|data| {
                    data.insert_temp(
                        crate::replay_ui::reel_destination_id(1),
                        "/srv/exports/reel-1.mp4".to_owned(),
                    );
                });
                let _intents = export.draw(ui, &export_state);
            }),
        });
    }
    // IN-006: the media list card with item pictures: a decoded one, a
    // simulated card, and one still to arrive.
    scenes.push(Scene::console(
        "operator/media-item-thumbnails",
        shell(&|shell| shell.layout.dock.show(PanelId::Media)),
        {
            let mut state = ready(true, Variant::default());
            state.item_thumbnails = item_thumbnails();
            state
        },
    ));
    scenes.push(Scene::console(
        "operator/playlist-entry",
        shell(&|shell| {
            shell.layout.dock.show(PanelId::Playlist);
            let state = ready(true, Variant::default());
            shell.playlist.sync(&state, std::time::Instant::now());
            if let Some(playlist) = state.project_state.as_ref().and_then(|p| p.playlist()) {
                shell
                    .playlist
                    .select(&playlist.entries[1], FrameRate::new(25, 1).unwrap());
            }
        }),
        ready(true, Variant::default()),
    ));
    // RC-017: the live list with its every-kind cue expanded, and the
    // waiting list (editing refused) with its cue expanded, for an operator
    // and a viewer.
    for (operator, role) in [(true, "operator"), (false, "viewer")] {
        for (name, list, cue) in [
            ("cues-actions", 1, 10),
            ("cues-media", 1, 30),
            ("cues-waiting", 2, 1),
        ] {
            scenes.push(Scene::console(
                format!("{role}/{name}"),
                shell(&|shell| {
                    shell.layout.dock.show(PanelId::Cues);
                    shell.cues.select(list, Some(cue));
                }),
                ready(operator, Variant::default()),
            ));
        }
    }
    // AU-005: the FX window on a strip (with a copied chain to paste) and
    // on Master, for an operator and a viewer.
    for (operator, role) in [(true, "operator"), (false, "viewer")] {
        for (name, target) in [
            (
                "strip-fx",
                fm_types::strip_dsp::StripDspTarget::Input(input(1)),
            ),
            ("master-fx", fm_types::strip_dsp::StripDspTarget::Master),
        ] {
            scenes.push(Scene::console(
                format!("{role}/{name}"),
                shell(&|shell| {
                    shell.strip_fx.open(target);
                    shell
                        .strip_fx
                        .copy(fm_types::strip_dsp::StripDspPreset::Voice.chain());
                }),
                ready(operator, Variant::default()),
            ));
        }
    }
    scenes.push(Scene::console(
        "operator/manual-transition",
        shell(&|_| {}),
        ready(
            true,
            Variant {
                manual: true,
                ..Variant::default()
            },
        ),
    ));
    scenes.push(Scene::console(
        "operator/timed-transition",
        shell(&|_| {}),
        ready(
            true,
            Variant {
                transitioning: true,
                ..Variant::default()
            },
        ),
    ));
    // The Cues panel's standby preview marks the GO frame's switcher steps
    // refused while the timed transition runs.
    scenes.push(Scene::console(
        "operator/cues-timed-transition",
        shell(&|shell| shell.layout.dock.show(PanelId::Cues)),
        ready(
            true,
            Variant {
                transitioning: true,
                ..Variant::default()
            },
        ),
    ));
    scenes.push(Scene::console(
        "operator/locked",
        shell(&|_| {}),
        ready(
            true,
            Variant {
                locked: true,
                ..Variant::default()
            },
        ),
    ));
    scenes.push(Scene::console(
        "operator/black-and-playing",
        shell(&|shell| shell.layout.dock.show(PanelId::Playlist)),
        ready(
            true,
            Variant {
                black: true,
                playing: true,
                ..Variant::default()
            },
        ),
    ));
    scenes.push(Scene::console(
        "operator/bank-filtered",
        shell(&|shell| {
            shell.lifecycle.filter = crate::input_lifecycle::BankFilter::Category(1);
        }),
        ready(true, Variant::default()),
    ));
    scenes.push(Scene::console("operator/notices", shell(&|_| {}), {
        let mut state = ready(true, Variant::default());
        state.notice = Some("saved".into());
        state.error = Some("refused".into());
        state.recording_pending = [true, false];
        state
    }));
    let operator = ready(true, Variant::default());
    let dialogs: [(&str, Configure); 17] = [
        ("shortcuts", &|shell| shell.show_shortcuts = true),
        ("about", &|shell| shell.show_about = true),
        ("preferences", &|shell| shell.show_preferences = true),
        ("add-input", &|shell| shell.open_add_input(&operator)),
        // IN-015: the network stream form with an RTSP URL, so the
        // transport keys are enabled.
        ("add-input-network", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::Network;
                draft.stream.url = "rtsp://operator:s3cret@cam.local/live".into();
            }
        }),
        // The same form with every Advanced group open, the URL's password
        // shown, and the retry waits crossed, so the field's red problem
        // line and the footer's pointer to it are audited.
        ("add-input-network-invalid", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::Network;
                draft.stream.url = "rtsp://operator:s3cret@cam.local/live".into();
                draft.stream.reveal.url = true;
                draft.stream.advanced = crate::network_stream_ui::StreamAdvanced {
                    source: true,
                    connection: true,
                    recovery: true,
                };
                draft.stream.settings.reconnect.backoff_min_ms = 20_000;
                draft.stream.settings.reconnect.backoff_max_ms = 1_000;
            }
        }),
        // IN-016: an RTMP listener with a stream key, so the mode keys and
        // the key field are audited.
        ("add-input-rtmp", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::Network;
                draft.stream.url = "rtmp://0.0.0.0:1935/live".into();
                draft.stream.settings.mode = fm_protocol::StreamMode::Listener;
                draft.stream.stream_key = "obs-key".into();
                draft.stream.advanced.connection = true;
            }
        }),
        // IN-017: an encrypted SRT listener on a stereo pair, so the
        // passphrase, key length, stream ID, latency, and audio keys are
        // audited enabled.
        ("add-input-srt", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::Network;
                draft.stream.url = "srt://0.0.0.0:9000".into();
                draft.stream.settings.mode = fm_protocol::StreamMode::Listener;
                draft.stream.settings.audio = fm_protocol::AudioChannels::Pair34;
                draft.stream.passphrase = "correct-horse-battery".into();
                draft.stream.stream_id = "cam1".into();
                draft.stream.reveal.passphrase = true;
                draft.stream.advanced = crate::network_stream_ui::StreamAdvanced {
                    source: true,
                    connection: true,
                    recovery: true,
                };
            }
        }),
        // AU-011: the system audio form on ALSA with a typed device, so
        // the audio system keys and the device field are audited.
        ("add-input-system-audio", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::SystemAudio;
                draft
                    .system_audio
                    .set_backend(fm_protocol::SystemAudioBackend::Alsa);
            }
        }),
        ("remove-input", &|shell| {
            shell.open_remove_input(input(4), "Guest");
        }),
        ("rename", &|shell| {
            shell.rename_draft = Some(crate::RenameDraft::new(input(2), "Camera 2"));
        }),
        // IN-026: Replace source on a colour, then on a typed media path
        // (its text field), batch import with and without paths, and the
        // category list with a typed new name.
        ("replace-source", &|shell| {
            shell.open_replace_source(input(2), "Camera 2");
        }),
        ("replace-source-media", &|shell| {
            shell.open_replace_source(input(2), "Camera 2");
            if let Some(draft) = shell.lifecycle.replace.as_mut() {
                draft.kind = crate::input_lifecycle::SourceKind::Media;
                draft.text = "/media/opener.mov".into();
            }
        }),
        // IN-006: Add input → Media list with typed files.
        ("add-input-media-list", &|shell| {
            shell.open_add_input(&operator);
            if let Some(draft) = shell.add_input.as_mut() {
                draft.kind = crate::input_catalogue::NewInputKind::MediaList;
                draft.media_list.text = "/media/a.mov\n/media/b.wav".into();
            }
        }),
        ("import-files", &|shell| shell.open_import(&operator)),
        ("import-files-listed", &|shell| {
            shell.open_import(&operator);
            if let Some(draft) = shell.lifecycle.import.as_mut() {
                draft.text = "/media/a.mov\n/media/b.mov".into();
            }
        }),
        ("categories", &|shell| {
            shell.open_categories(&operator);
            if let Some(draft) = shell.lifecycle.categories.as_mut() {
                draft.new_name = "Graphics".into();
            }
        }),
    ];
    for (name, open) in dialogs {
        scenes.push(Scene::console(
            format!("dialog/{name}"),
            shell(open),
            operator.clone(),
        ));
    }
    // OR-006: the destination dialog adding (fresh, then with a bad server
    // and the key shown), editing the stopped keyed destination, editing the
    // running HLS one (Save refused, the key and backup disabled), and the
    // Remove confirmation.
    let destinations: [(&str, Configure); 7] = [
        ("stream-add", &|shell| {
            shell.stream_targets.open_add(locale, &operator);
        }),
        // Protocol 2.54: an encrypted SRT caller with its latency and a
        // masked passphrase, then one with Encrypt unticked (the passphrase
        // field disabled with its reason).
        ("stream-add-srt", &|shell| {
            shell.stream_targets.open_add(locale, &operator);
            if let Some(draft) = shell.stream_targets.dialog.as_mut() {
                draft.set_protocol(fm_protocol::StreamTargetProtocol::Srt);
                draft.endpoint = "srt.example.com:9000".into();
                draft.key = "typed-key".into();
                draft.encrypt = true;
                draft.passphrase = "typed-passphrase".into();
            }
        }),
        ("stream-add-srt-clear", &|shell| {
            shell.stream_targets.open_add(locale, &operator);
            if let Some(draft) = shell.stream_targets.dialog.as_mut() {
                draft.set_protocol(fm_protocol::StreamTargetProtocol::Srt);
                draft.endpoint = "srt.example.com:9000".into();
                draft.srt_latency_ms = 10;
            }
        }),
        ("stream-add-invalid", &|shell| {
            shell.stream_targets.open_add(locale, &operator);
            if let Some(draft) = shell.stream_targets.dialog.as_mut() {
                draft.endpoint = "rtmp://live.example.com".into();
                draft.key = "typed-key".into();
                draft.reveal_key = true;
                draft.initial_delay_ms = 5_000;
                draft.max_delay_ms = 1_000;
                draft.unlimited = true;
            }
        }),
        ("stream-edit", &|shell| {
            let target = WireStreamTargetId::new(NonZeroU128::new(2).unwrap());
            shell.stream_targets.open_edit(&operator, target);
        }),
        ("stream-edit-running", &|shell| {
            let entry = &stream_target_entries()[0];
            shell.stream_targets.dialog = Some(crate::stream_targets::TargetDraft::edit(entry));
        }),
        ("stream-remove", &|shell| {
            let target = WireStreamTargetId::new(NonZeroU128::new(2).unwrap());
            shell.stream_targets.open_remove(&operator, target);
        }),
    ];
    for (name, open) in destinations {
        scenes.push(Scene::console(
            format!("dialog/{name}"),
            shell(open),
            operator.clone(),
        ));
    }
    // OR-006: in a narrow dock each destination row folds its Retry / Edit
    // / Remove keys into one `···` menu key.
    let narrow = operator.clone();
    scenes.push(Scene {
        name: "operator/streams-narrow".into(),
        draw: Box::new(move |ui| {
            crate::l10n::set_locale(ui.ctx(), locale);
            let mut dialogs = crate::stream_targets::StreamTargetsUi::default();
            let mut intents = Vec::new();
            ui.set_max_width(260.0);
            crate::outputs::draw_streams(ui, &narrow, &mut dialogs, &mut intents);
        }),
    });
    // The Colour window: Simple on a graded camera, Advanced on Program
    // with a library grade chosen and a name and LUT path typed, on a graded
    // scene layer, and locked by a missing permission and by the project
    // padlock.
    let colour: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "operator/colour",
            &|shell| shell.open_color(Some(GradeTarget::Input(input(2)))),
            ready(true, Variant::default()),
        ),
        (
            "operator/colour-advanced",
            &|shell| {
                shell.open_color(Some(GradeTarget::Program));
                shell.color.panel.mode = PanelMode::Advanced;
                // Copy to… with an input ticked.
                shell.color.copy_ticked.insert(GradeTarget::Input(input(1)));
                shell.color.library_pick = core::num::NonZeroU32::new(1);
                shell.color.library_name = "Evening".into();
                shell.color.lut_path = "/luts/warm.cube".into();
                // A grade export waiting to replace or keep a sibling LUT.
                shell.color.lut_replace = Some(crate::preset_files::PendingLutReplace {
                    request: crate::preset_files::PresetFileRequest::ExportGrade {
                        path: "/grades/evening.fmgrade".into(),
                        name: "Evening".into(),
                        grade: Box::new(fm_grade::Grade::IDENTITY),
                        lut: None,
                        replace_lut: false,
                    },
                    lut: "/grades/evening.cube".into(),
                });
            },
            ready(true, Variant::default()),
        ),
        (
            "operator/colour-layer",
            &|shell| {
                shell.open_color(Some(GradeTarget::Layer {
                    scene: scene_id(),
                    layer: scene_layer(1),
                }));
            },
            ready(true, Variant::default()),
        ),
        (
            "viewer/colour",
            &|shell| shell.open_color(Some(GradeTarget::Input(input(1)))),
            ready(false, Variant::default()),
        ),
        (
            "operator/colour-locked",
            &|shell| {
                shell.open_color(Some(GradeTarget::Input(input(2))));
                shell.color.panel.mode = PanelMode::Advanced;
            },
            ready(
                true,
                Variant {
                    locked: true,
                    ..Variant::default()
                },
            ),
        ),
    ];
    for (name, open, state) in colour {
        scenes.push(Scene::console(name, shell(open), state));
    }
    // OR-011: the Fullscreen outputs dialog with the display list, with an
    // unsaved draft, with a draft changed elsewhere and a refused change,
    // while a change is applying, before the list arrives, on a daemon
    // without a presenter, for a viewer, and under the padlock.
    let fullscreen: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "dialog/fullscreen-outputs",
            &|shell| shell.display_outputs.open = true,
            with_display_inventory(ready(true, Variant::default()), true),
        ),
        (
            "dialog/fullscreen-outputs-draft",
            &|shell| {
                shell.display_outputs.open = true;
                let mut draft = display_output_entries()[1].config.clone();
                draft.scaling = fm_types::OutputScaling::OneToOne;
                shell.display_outputs.drafts[1] = Some(crate::display_outputs::SlotDraft {
                    config: draft,
                    base: Some(display_output_entries()[1].config.clone()),
                });
            },
            with_display_inventory(ready(true, Variant::default()), true),
        ),
        (
            // Slot 2 was changed elsewhere since its draft started (Reload
            // and Overwrite), and the server refused slot 1's last change.
            "dialog/fullscreen-outputs-conflict-refused",
            &|shell| {
                shell.display_outputs.open = true;
                let mut draft = display_output_entries()[1].config.clone();
                draft.scaling = fm_types::OutputScaling::OneToOne;
                let mut base = display_output_entries()[1].config.clone();
                base.hdr = fm_protocol::HdrRequest::Auto;
                shell.display_outputs.drafts[1] = Some(crate::display_outputs::SlotDraft {
                    config: draft,
                    base: Some(base),
                });
                let mut refused = display_output_entries()[0].config.clone();
                refused.scaling = fm_types::OutputScaling::Stretch;
                shell.display_outputs.drafts[0] = Some(crate::display_outputs::SlotDraft {
                    config: refused,
                    base: Some(display_output_entries()[0].config.clone()),
                });
                shell.display_outputs.refusals[0] = Some(
                    "Edit refused (project_locked): the project is locked. Unlock the padlock in the top bar to edit; switching still works."
                        .to_owned(),
                );
            },
            with_display_inventory(ready(true, Variant::default()), true),
        ),
        (
            // Slot 2's change is sent and not answered yet.
            "dialog/fullscreen-outputs-applying",
            &|shell| {
                shell.display_outputs.open = true;
                let mut draft = display_output_entries()[1].config.clone();
                draft.scaling = fm_types::OutputScaling::OneToOne;
                shell.display_outputs.drafts[1] = Some(crate::display_outputs::SlotDraft {
                    config: draft.clone(),
                    base: Some(display_output_entries()[1].config.clone()),
                });
                shell.display_outputs.awaiting[1] =
                    Some(crate::display_outputs::Awaiting::Set(draft));
            },
            with_display_inventory(ready(true, Variant::default()), true),
        ),
        (
            "dialog/fullscreen-outputs-unlisted",
            &|shell| shell.display_outputs.open = true,
            ready(true, Variant::default()),
        ),
        (
            "dialog/fullscreen-outputs-no-presenter",
            &|shell| shell.display_outputs.open = true,
            with_display_inventory(ready(true, Variant::default()), false),
        ),
        (
            "viewer/fullscreen-outputs",
            &|shell| shell.display_outputs.open = true,
            with_display_inventory(ready(false, Variant::default()), true),
        ),
        (
            "operator/fullscreen-outputs-locked",
            &|shell| shell.display_outputs.open = true,
            with_display_inventory(
                ready(
                    true,
                    Variant {
                        locked: true,
                        ..Variant::default()
                    },
                ),
                true,
            ),
        ),
    ];
    for (name, open, state) in fullscreen {
        scenes.push(Scene::console(name, shell(open), state));
    }
    // OR-017: the LiveLAN viewers window live with a PIN stored and one
    // typed, off, waiting for a listener on an output, failed, for a
    // viewer, and under the padlock.
    let live = fm_protocol::LiveLanStatus {
        state: fm_protocol::LiveLanState::Live,
        url: "http://192.168.1.20:8090/".into(),
        viewers: 3,
        egress_kbps: 7_800,
    };
    let sharing = fm_protocol::LiveLanEntry {
        enabled: true,
        source: fm_protocol::LiveLanSource::Program,
        max_viewers: 8,
        has_pin: true,
    };
    let on_output = fm_protocol::LiveLanEntry {
        source: fm_protocol::LiveLanSource::Output(fm_types::OutputId::new(
            NonZeroU128::new(2).unwrap(),
        )),
        has_pin: false,
        ..sharing
    };
    let live_lan: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "dialog/live-lan",
            &|shell| {
                shell.live_lan.open = true;
                shell.live_lan.pin = "4321".into();
            },
            with_live_lan(ready(true, Variant::default()), sharing, &live),
        ),
        (
            "dialog/live-lan-off",
            &|shell| shell.live_lan.open = true,
            ready(true, Variant::default()),
        ),
        (
            "dialog/live-lan-unavailable",
            &|shell| shell.live_lan.open = true,
            with_live_lan(
                ready(true, Variant::default()),
                on_output,
                &fm_protocol::LiveLanStatus::default(),
            ),
        ),
        (
            "dialog/live-lan-failed",
            &|shell| shell.live_lan.open = true,
            with_live_lan(
                ready(true, Variant::default()),
                on_output,
                &fm_protocol::LiveLanStatus {
                    state: fm_protocol::LiveLanState::Failed,
                    ..fm_protocol::LiveLanStatus::default()
                },
            ),
        ),
        (
            "viewer/live-lan",
            &|shell| shell.live_lan.open = true,
            with_live_lan(ready(false, Variant::default()), sharing, &live),
        ),
        (
            "operator/live-lan-locked",
            &|shell| shell.live_lan.open = true,
            with_live_lan(
                ready(
                    true,
                    Variant {
                        locked: true,
                        ..Variant::default()
                    },
                ),
                sharing,
                &live,
            ),
        ),
    ];
    for (name, open, state) in live_lan {
        scenes.push(Scene::console(name, shell(open), state));
    }
    // RC-007: the Web access window with a TLS gateway, a pairing under
    // way, and two paired devices; a plain loopback gateway with nothing
    // paired; refused for a non-admin session; and before any answer.
    let web_access: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "dialog/web-access",
            &|shell| shell.web_access.open = true,
            with_web_access(ready(true, Variant::default()), Some(web_access_answer())),
        ),
        (
            "dialog/web-access-install",
            &|shell| {
                shell.web_access.open = true;
                shell.web_access.install_open = true;
                shell.web_access.reach = Some("https://10.0.0.9:8443".into());
            },
            with_web_access(ready(true, Variant::default()), Some(web_access_answer())),
        ),
        (
            "dialog/web-access-operator-certificate",
            &|shell| shell.web_access.open = true,
            with_web_access(
                ready(true, Variant::default()),
                Some({
                    let mut answer = web_access_answer();
                    if let Some(gateway) = answer.gateway.as_mut() {
                        gateway.trust = fm_protocol::WebTrust::Operator;
                        gateway.ca_fingerprint = None;
                        gateway.cert_problem = Some(fm_protocol::WebCertProblem::ReloadFailed);
                        gateway.mdns = None;
                        gateway.urls.clear();
                    }
                    answer
                }),
            ),
        ),
        (
            "dialog/web-access-plain",
            &|shell| shell.web_access.open = true,
            with_web_access(
                ready(true, Variant::default()),
                Some(fm_protocol::WebAccessMessage {
                    request_id: "audit".into(),
                    refusal: None,
                    gateway: Some(fm_protocol::WebGatewayStatus {
                        url: "http://127.0.0.1:8443".into(),
                        tls: false,
                        fingerprint: None,
                        trust: fm_protocol::WebTrust::Plain,
                        ca_fingerprint: None,
                        cert_not_after_ms: None,
                        cert_problem: None,
                        urls: Vec::new(),
                        mdns: None,
                        pairing: None,
                        devices: Vec::new(),
                    }),
                }),
            ),
        ),
        (
            "dialog/web-access-refused",
            &|shell| shell.web_access.open = true,
            with_web_access(
                ready(true, Variant::default()),
                Some(fm_protocol::WebAccessMessage {
                    request_id: "audit".into(),
                    refusal: Some(fm_protocol::WebAccessRefusal::PermissionDenied),
                    gateway: None,
                }),
            ),
        ),
        (
            "dialog/web-access-waiting",
            &|shell| shell.web_access.open = true,
            ready(true, Variant::default()),
        ),
    ];
    for (name, open, state) in web_access {
        scenes.push(Scene::console(name, shell(open), state));
    }
    // RC-015: the Health alerts window with a firing, acknowledged alert and
    // a silenced rule, for an operator and for a viewer whose keys are off.
    let alerted = |operator: bool| {
        let mut state = ready(operator, Variant::default());
        if let Some(status) = state.health.as_mut() {
            if let Some(alert) = status.health.alerts.first_mut() {
                alert.acknowledged_until_ms = Some(600_000);
            }
            status.health.silences = vec![fm_protocol::HealthSilence {
                rule: fm_protocol::HealthAlertRule::DiskLow,
                until_ms: 3_600_000,
            }];
        }
        state
    };
    for (name, operator) in [
        ("dialog/health-alerts", true),
        ("dialog/health-alerts-viewer", false),
    ] {
        scenes.push(Scene::console(
            name,
            shell(&|shell| shell.health_alerts.open = true),
            alerted(operator),
        ));
    }
    // RC-004 key profiles: a built-in profile, read only, and one of the
    // operator's own with a changed key and a refused key change.
    // The keyboard reference and the console's key hints under the vMix
    // style profile.
    scenes.push(Scene::console(
        "dialog/shortcuts-vmix",
        shell(&|shell| shell.show_shortcuts = true),
        {
            let mut state = ready(true, Variant::default());
            state.console_keys = Some(crate::BuiltinProfile::Vmix.keymap());
            state
        },
    ));
    scenes.push(Scene::console(
        "dialog/key-profiles",
        shell(&|shell| shell.open_key_profiles()),
        ready(true, Variant::default()),
    ));
    scenes.push(Scene::console(
        "dialog/key-profiles-custom",
        shell(&|shell| {
            let mut profiles = crate::ConsoleKeyProfiles::default();
            let choice = profiles
                .add(
                    "Late show",
                    crate::ProfileChoice::Builtin(crate::BuiltinProfile::Vmix),
                )
                .expect("a free profile slot");
            profiles
                .set_key(
                    0,
                    crate::ConsoleAction::Cut,
                    Some(crate::ConsoleStroke::plain(egui::Key::X)),
                )
                .expect("a free key");
            profiles.choose(choice);
            shell.key_profiles = profiles;
            shell.open_key_profiles();
            shell.key_profiles_window.refusal = Some("T is a console key".into());
        }),
        ready(true, Variant::default()),
    ));
    let scripts: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "dialog/scripts",
            &|shell| shell.scripts.open = true,
            ready(true, Variant::default()),
        ),
        (
            "dialog/scripts-syntax-error",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(2));
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/scripts-run-stopped",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
                shell.scripts.failure = Some(crate::scripts::RunFailure {
                    script: 1,
                    failure: fm_protocol::ScriptFailureKind::Refused,
                    line: 3,
                    column: 1,
                    since: None,
                    detail: None,
                });
            },
            ready(true, Variant::default()),
        ),
        // Protocol 2.61: the last run a cue's script step made.
        (
            "dialog/scripts-ran-by-cue",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
            },
            cue_ran_script_fixture(),
        ),
        // Protocol 2.62: a run paused at a wait, and a part that failed.
        (
            "dialog/scripts-running",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
            },
            script_run_fixture(true, paused_run()),
        ),
        (
            "dialog/scripts-running-other",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(2));
            },
            script_run_fixture(true, paused_run()),
        ),
        (
            "viewer/scripts-running",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
            },
            script_run_fixture(false, paused_run()),
        ),
        // Protocol 2.69: several runs at once, each with its own Stop.
        (
            "dialog/scripts-concurrent-runs",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(2));
            },
            concurrent_runs_fixture(true),
        ),
        (
            "viewer/scripts-concurrent-runs",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
            },
            concurrent_runs_fixture(false),
        ),
        (
            "dialog/scripts-run-faulted",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
            },
            script_run_fixture(
                true,
                fm_protocol::ScriptRunStatus::Faulted {
                    run: 3,
                    frame: 160,
                    failure: fm_protocol::ScriptFailureKind::Refused,
                    line: 3,
                    column: 1,
                    detail: fm_protocol::FaultDetail {
                        reason: None,
                        message: "the show has no input named \"Sponsor Loop\"".to_owned(),
                    },
                },
            ),
        ),
        (
            "dialog/scripts-new",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.new_draft(shell.locale, &audit_scripts());
            },
            ready(true, Variant::default()),
        ),
        (
            "viewer/scripts",
            &|shell| shell.scripts.open = true,
            ready(false, Variant::default()),
        ),
        (
            "operator/scripts-locked",
            &|shell| {
                shell.scripts.open = true;
                shell.scripts.select(&audit_scripts(), Some(1));
                shell.scripts.source.push_str("cut\n");
            },
            ready(
                true,
                Variant {
                    locked: true,
                    ..Variant::default()
                },
            ),
        ),
    ];
    for (name, open, state) in scripts {
        scenes.push(Scene::console(name, shell(open), state));
    }
    // RC-004: the Shortcuts window on each kind of action, capturing a
    // key, with a key and an OSC name that clash, for a viewer, and locked.
    let user_shortcuts: Vec<(&str, Configure, StudioUiState)> = vec![
        (
            "dialog/user-shortcuts",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-cue",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(2));
                edit_shortcut_draft(shell, &|draft| {
                    draft.action = fm_protocol::WireShortcutAction::Cue {
                        list: 1,
                        operation: fm_protocol::WireShortcutCueOperation::Go { cue: Some(20) },
                    };
                });
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-script",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(3));
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-file",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                shell.user_shortcuts.file_path = "desk.fmshortcuts".into();
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-capturing",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                shell.user_shortcuts.capturing = true;
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-chord",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                edit_shortcut_draft(shell, &|draft| {
                    draft.key = Some(fm_protocol::WireShortcutKey {
                        then: Some(Box::new(fm_protocol::WireShortcutKey::stroke(
                            "1", false, false, false,
                        ))),
                        scope: Some("cues".into()),
                        ..fm_protocol::WireShortcutKey::stroke("K", true, false, false)
                    });
                });
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-mouse-window",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                edit_shortcut_draft(shell, &|draft| {
                    draft.key = Some(fm_protocol::WireShortcutKey {
                        scope: Some("mixer-window".into()),
                        ..fm_protocol::WireShortcutKey::stroke("MouseBack", true, false, false)
                    });
                });
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-chord-capturing",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                shell.user_shortcuts.capturing = true;
                shell.user_shortcuts.capturing_then = true;
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-console-key",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                let project = ProjectId::new(NonZeroU128::new(1).unwrap());
                shell.shortcut_keys.set(
                    project,
                    1,
                    Some(fm_protocol::WireShortcutKey {
                        key: "F2".into(),
                        command: true,
                        shift: false,
                        alt: false,
                        then: None,
                        scope: None,
                    }),
                );
                shell.shortcut_keys.set(project, 3, None);
            },
            ready(false, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-console-capturing",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(3));
                shell.user_shortcuts.capturing_local = true;
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-console-taken",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(3));
                shell.user_shortcuts.local_refusal = Some(
                    crate::user_shortcuts::LocalRefusal::Taken("Lower third".into()),
                );
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-clash",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
                edit_shortcut_draft(shell, &|draft| {
                    draft.key = Some(fm_protocol::WireShortcutKey {
                        key: "B".into(),
                        command: false,
                        shift: false,
                        alt: false,
                        then: None,
                        scope: None,
                    });
                    draft.osc = Some("open".into());
                });
            },
            ready(true, Variant::default()),
        ),
        (
            "dialog/user-shortcuts-new",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell
                    .user_shortcuts
                    .new_draft(shell.locale, &audit_shortcuts());
            },
            ready(true, Variant::default()),
        ),
        (
            "viewer/user-shortcuts",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(1));
            },
            ready(false, Variant::default()),
        ),
        (
            "operator/user-shortcuts-locked",
            &|shell| {
                shell.user_shortcuts.open = true;
                shell.user_shortcuts.select(&audit_shortcuts(), Some(2));
            },
            ready(
                true,
                Variant {
                    locked: true,
                    ..Variant::default()
                },
            ),
        ),
    ];
    for (name, open, state) in user_shortcuts {
        scenes.push(Scene::console(name, shell(open), state));
    }
    scenes.extend(animation_scenes(locale));
    scenes.extend(designer_scenes(locale));
    scenes.extend(transition_menu_scenes(locale));
    scenes.extend(dock_scenes(locale));
    scenes.extend(capture_scenes(locale));
    scenes.extend(system_audio_scenes(locale));
    scenes.extend(monitor_output_scenes(locale));
    scenes.extend(still_scenes(locale));
    scenes.extend(screen_capture_scenes(locale));
    scenes.push(Scene::console(
        "disconnected",
        shell(&|_| {}),
        StudioUiState::new(StudioConnectionStatus::Backoff),
    ));
    scenes
}

/// The docking chrome: tab bars (with their grips and right-click menus,
/// which the audit opens), pop-out, restore, and close keys, splitters, a
/// maximised tile, popped-out panel windows with their grips, Dock back
/// to… menus, and Dock back keys, tool windows, View › Layouts with saved
/// layouts, the Save and Rename layout dialogs, and an empty workspace.
fn dock_scenes(locale: Locale) -> Vec<Scene> {
    use crate::dock::{DropZone, LayoutPreset, NamedLayout, ToolWindow};
    let shell = |configure: &dyn Fn(&mut StudioShell)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.layout.saved_layouts = vec![
            NamedLayout {
                name: "Interview".into(),
                layout: LayoutPreset::Compact.layout(),
            },
            NamedLayout {
                name: "Late show".into(),
                layout: LayoutPreset::AudioFocus.layout(),
            },
        ];
        configure(&mut shell);
        shell
    };
    let rearranged = |shell: &mut StudioShell| {
        let dock = &mut shell.layout.dock;
        dock.move_panel(PanelId::Mixer, PanelId::InputBank, DropZone::Center);
        dock.move_panel(PanelId::Titles, PanelId::Program, DropZone::Right);
        dock.pop_out(PanelId::Outputs, None);
        dock.pop_out(PanelId::Playlist, None);
    };
    let mut designer_state = ready(true, Variant::default());
    designer_state.title_previews = true;
    let mut scenes = vec![
        Scene::console(
            "dock/rearranged-and-popped",
            shell(&rearranged),
            ready(true, Variant::default()),
        ),
        Scene::console(
            "dock/save-layout",
            shell(&|shell| {
                shell.open_layout_name_dialog(None);
                if let Some(dialog) = &mut shell.dock_ui.name_dialog {
                    dialog.name = "Bad|name".into();
                }
            }),
            ready(true, Variant::default()),
        ),
        Scene::console(
            "dock/rename-layout",
            shell(&|shell| shell.open_layout_name_dialog(Some(1))),
            ready(true, Variant::default()),
        ),
        Scene::console(
            "dock/maximized",
            shell(&|shell| shell.dock_ui.maximized = Some(PanelId::Mixer)),
            ready(true, Variant::default()),
        ),
        Scene::console(
            "dock/all-hidden",
            shell(&|shell| {
                for panel in PanelId::ALL {
                    shell.layout.dock.close(panel);
                }
            }),
            ready(true, Variant::default()),
        ),
    ];
    let mut tools = shell(&|shell| {
        for tool in ToolWindow::ALL {
            shell.layout.set_tool_popped(tool, true);
        }
        shell.open_color(None);
    });
    tools.open_scene_designer(scene_id());
    tools.designer.sync(
        &egui::Context::default(),
        &designer_state,
        core::time::Duration::ZERO,
    );
    scenes.push(Scene::console("dock/tool-windows", tools, designer_state));
    let mut animation = shell(&|shell| {
        shell.layout.set_tool_popped(ToolWindow::Animation, true);
        shell.open_animation(Some(input(TITLE_INPUT)));
    });
    let state = ready(true, Variant::default());
    animation.animation.sync(&egui::Context::default(), &state);
    scenes.push(Scene::console("dock/animation-window", animation, state));
    scenes
}

/// Script 1 paused at its second wait, resuming on frame 150.
fn paused_run() -> fm_protocol::ScriptRunStatus {
    fm_protocol::ScriptRunStatus::Waiting {
        run: 3,
        started_frame: 100,
        due_frame: 150,
        waits: 2,
        wait_frames: 50,
        line: 2,
    }
}

/// Protocol 2.69: script 1 paused (run 3) while script 2's run 5 failed,
/// for an operator or a viewer.
pub(crate) fn concurrent_runs_fixture(operator: bool) -> StudioUiState {
    let mut model = script_run_model(paused_run());
    apply_audit_change(
        &mut model,
        fm_ui_model::DurableChange::ScriptRunChanged {
            script: 2,
            frame: 140,
            run: fm_protocol::ScriptRunStatus::Faulted {
                run: 5,
                frame: 140,
                failure: fm_protocol::ScriptFailureKind::Runtime,
                line: 2,
                column: 4,
                detail: fm_protocol::FaultDetail {
                    reason: None,
                    message: "wait needs 1 or more frames, not 0".to_owned(),
                },
            },
        },
    );
    ready_over(&model, operator)
}

/// The audit's show with script 1's run in state `run` (Protocol 2.62),
/// for an operator or a viewer.
pub(crate) fn script_run_fixture(
    operator: bool,
    run: fm_protocol::ScriptRunStatus,
) -> StudioUiState {
    ready_over(&script_run_model(run), operator)
}

fn script_run_model(run: fm_protocol::ScriptRunStatus) -> ClientModel {
    let mut model = model(Variant::default());
    apply_audit_change(
        &mut model,
        fm_ui_model::DurableChange::ScriptRunChanged {
            script: 1,
            frame: 130,
            run,
        },
    );
    model
}

fn apply_audit_change(model: &mut ClientModel, change: fm_ui_model::DurableChange) {
    let state = model.state().unwrap().clone();
    let cursor = model.reconnect_cursor().unwrap().clone();
    model
        .apply_event(fm_ui_model::DurableProjectEvent {
            cursor: fm_ui_model::ProjectCursor {
                revision: cursor.revision.checked_next().unwrap(),
                ..cursor
            },
            change,
            edit_history: state.edit_history(),
        })
        .unwrap();
}

/// The audit's show with script 1's run in state `run`, after script 1's
/// source was then replaced by `source` (Protocol 2.62), for an operator.
#[cfg(test)]
pub(crate) fn script_edited_after_run_fixture(
    run: fm_protocol::ScriptRunStatus,
    source: &str,
) -> StudioUiState {
    let mut model = script_run_model(run);
    let mut script = audit_scripts().remove(0);
    script.spec.source = source.into();
    apply_audit_change(
        &mut model,
        fm_ui_model::DurableChange::ScriptChanged(Box::new(script)),
    );
    ready_over(&model, true)
}

/// The audit's show once cue list 1 ("Show open") ran script 1 ("Opening")
/// as a step on frame 120, reported as 3 actions in 7 instructions
/// (Protocol 2.61), for an operator.
pub(crate) fn cue_ran_script_fixture() -> StudioUiState {
    let mut model = model(Variant::default());
    let state = model.state().unwrap().clone();
    let switcher = state.switcher();
    let update = fm_ui_model::CueRunUpdate {
        list: 1,
        frame: 120,
        state: state.cue_lists()[0].state.clone(),
        fired: Vec::new(),
        selection: switcher.desired,
        manual_transition: switcher.desired_manual_transition,
        fade_to_black: switcher.desired_fade_to_black,
        overlays: state.desired_overlays().to_vec(),
        input_audio_strips: state.input_audio_strips().to_vec(),
        streams: state.streams().to_vec(),
        recordings: state.recordings().to_vec(),
        playlist: state
            .playlist()
            .map_or(PlaylistRunState::Stopped, |playlist| playlist.state),
        titles: Vec::new(),
        media: Vec::new(),
        scripts: vec![fm_protocol::CueScriptRun {
            script: 1,
            actions: 3,
            instructions: 7,
        }],
    };
    let cursor = model.reconnect_cursor().unwrap().clone();
    model
        .apply_event(fm_ui_model::DurableProjectEvent {
            cursor: fm_ui_model::ProjectCursor {
                revision: cursor.revision.checked_next().unwrap(),
                ..cursor
            },
            change: fm_ui_model::DurableChange::CueRunChanged(Box::new(update)),
            edit_history: state.edit_history(),
        })
        .unwrap();
    ready_over(&model, true)
}

/// The audit's show for an operator, for the input bank's layout tests.
#[cfg(test)]
pub(crate) fn bank_fixture() -> StudioUiState {
    ready(true, Variant::default())
}

/// The audit's show (Scene input 3 showing scene 30, whose layers are Host
/// and Guest) with exact previews, for the layer designer's tests.
#[cfg(test)]
pub(crate) fn designer_fixture(operator: bool, locked: bool) -> (StudioUiState, fm_types::SceneId) {
    let mut state = ready(
        operator,
        Variant {
            locked,
            ..Variant::default()
        },
    );
    state.title_previews = true;
    (state, scene_id())
}

/// The layer designer (SW-006): nothing selected, one layer selected with
/// a crop and an inverted mask in the draft (every field and the draft
/// buttons enabled, a chroma key, and one effect of every kind), the luma
/// keyed layer with a full effect list (Add effect refused), the same layer
/// key/filled from another input with green despill, light wrap, and a
/// garbage matte (SW-008), the host
/// layer with its input's green-screen picture for auto key (SW-008),
/// with the Pick colour eyedropper on, two layers selected (their effects
/// differ), the project padlock, and a viewer.
fn designer_scenes(locale: Locale) -> Vec<Scene> {
    use fm_ui_model::scene_designer::Field;
    let prepare = |state: &StudioUiState, configure: &dyn Fn(&mut StudioShell)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.open_scene_designer(scene_id());
        shell
            .designer
            .sync(&egui::Context::default(), state, core::time::Duration::ZERO);
        configure(&mut shell);
        shell
    };
    let unchanged = |_: &mut StudioShell| {};
    let edited = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(1), false);
        document.set_field(Field::CropLeft, 40.0);
        document.set_field(Field::Masked, 1.0);
        document.set_field(Field::MaskInverted, 1.0);
        document.undo();
        document.set_field(Field::MaskInverted, 1.0);
    };
    let luma = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(2), false);
        while document.can_add_effect() {
            document.add_effect("flip");
        }
    };
    let fill = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(2), false);
        document.set_key_fill(wire(1));
        document.set_field(Field::KeyDespill, 1.0);
        document.set_field(Field::KeyLightWrap, 128.0);
        document.set_field(Field::Matte, 1.0);
    };
    let both = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(1), false);
        document.select(scene_layer(2), true);
    };
    let host = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(1), false);
    };
    let picking = |shell: &mut StudioShell| {
        host(shell);
        shell.designer.eyedropper = true;
    };
    let pictured = || {
        let mut state = ready(true, Variant::default());
        state.title_previews = true;
        state.input_thumbnails = green_screen_thumbnail(1);
        state
    };
    let operator = || {
        let mut state = ready(true, Variant::default());
        state.title_previews = true;
        state
    };
    let locked = ready(
        true,
        Variant {
            locked: true,
            ..Variant::default()
        },
    );
    let scenes: Vec<(&str, Configure, StudioUiState)> = vec![
        ("operator/designer", &unchanged, operator()),
        ("operator/designer-edited", &edited, operator()),
        ("operator/designer-luma", &luma, operator()),
        ("operator/designer-fill", &fill, operator()),
        ("operator/designer-autokey", &host, pictured()),
        ("operator/designer-eyedropper", &picking, pictured()),
        ("operator/designer-multi", &both, operator()),
        ("operator/designer-locked", &edited, locked),
        ("viewer/designer", &both, ready(false, Variant::default())),
    ];
    scenes
        .into_iter()
        .map(|(name, configure, state)| Scene::console(name, prepare(&state, configure), state))
        .collect()
}

/// A 16x9 green-screen thumbnail of input `input` with a subject, the
/// picture the designer's auto key samples (SW-008).
pub(crate) fn green_screen_thumbnail(
    input: u128,
) -> BTreeMap<WireInputId, (fm_protocol::InputThumbnailImage, std::time::Instant)> {
    let mut rgba = Vec::new();
    for index in 0..16 * 9 {
        let pixel = if (6..10).contains(&(index % 16)) && index / 16 >= 3 {
            [210, 60, 50, 255]
        } else {
            [30, 190, 60, 255]
        };
        rgba.extend_from_slice(&pixel);
    }
    let image = fm_protocol::InputThumbnailImage {
        input: wire(input),
        item: None,
        sequence: 1,
        origin: fm_protocol::MonitorFrameOrigin::Simulated,
        age_ms: 0,
        width: 16,
        height: 9,
        rgba: rgba.into(),
    };
    BTreeMap::from([(wire(input), (image, std::time::Instant::now()))])
}

/// Pictures of the Guest media list's items 1 (decoded) and 3 (a
/// simulated card); item 2 has none yet.
pub(crate) fn item_thumbnails()
-> BTreeMap<(WireInputId, u32), (fm_protocol::InputThumbnailImage, std::time::Instant)> {
    [
        (1, fm_protocol::MonitorFrameOrigin::Native),
        (3, fm_protocol::MonitorFrameOrigin::Simulated),
    ]
    .into_iter()
    .map(|(item, origin)| {
        let (_, (mut image, received)) = green_screen_thumbnail(4).pop_first().unwrap();
        image.item = Some(item);
        image.origin = origin;
        ((wire(4), item), (image, received))
    })
    .collect()
}

/// The right-click menus of the transition column and overlay channels,
/// which open at the pointer and so are drawn on their own: a favourite's
/// `Assign…` menu (with Stinger slots) and an overlay channel's transition
/// picker, each enabled and refused.
fn transition_menu_scenes(locale: Locale) -> Vec<Scene> {
    let stingers = [
        WireStingerSlotId::new(1).unwrap(),
        WireStingerSlotId::new(3).unwrap(),
    ];
    let mut scenes = Vec::new();
    for (variant, enabled) in [("operator", true), ("viewer", false)] {
        let reason = Msg::ReasonNoTransition.en();
        scenes.push(Scene {
            name: format!("{variant}/favourite-assign"),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                let mut intents = Vec::new();
                crate::transitions::draw_assign_menu(
                    ui,
                    (2, 30),
                    &stingers,
                    (enabled, reason),
                    &mut intents,
                );
            }),
        });
        scenes.push(Scene {
            name: format!("{variant}/favourite-t-bar"),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                let mut intents = Vec::new();
                let mut kind = fm_protocol::ManualTransitionKind::Fade;
                crate::transitions::draw_favorite_t_bar_start(
                    ui,
                    (
                        3,
                        fm_protocol::TransitionFavoriteStatus {
                            effect: fm_protocol::TransitionFavoriteEffect::Stinger(stingers[1]),
                            duration_frames: 45,
                        },
                    ),
                    (enabled, reason),
                    &mut kind,
                    &mut intents,
                );
            }),
        });
        scenes.push(Scene {
            name: format!("{variant}/overlay-transition-menu"),
            draw: Box::new(move |ui| {
                crate::l10n::set_locale(ui.ctx(), locale);
                let mut intents = Vec::new();
                crate::transitions::draw_overlay_transition_menu(
                    ui,
                    (
                        WireOverlayChannelId::new(1).unwrap(),
                        OverlayTransitionKind::Cube,
                        30,
                    ),
                    (enabled, reason),
                    &mut intents,
                );
            }),
        });
    }
    scenes
}

/// The Studio Animation window on the lower third: Simple mode without and
/// with a design (kind icons, presets for an unanimated element), docked,
/// Advanced mode with a design, a selection, and an active row, a conflict,
/// a padlock, and a viewer.
fn animation_scenes(locale: Locale) -> Vec<Scene> {
    let title = input(TITLE_INPUT);
    let bar = TrackKey::new(title_element(1), Phase::In, Property::Opacity);
    let prepare = |state: &StudioUiState, configure: &dyn Fn(&mut StudioShell)| {
        let mut shell = StudioShell::default();
        shell.set_locale(locale);
        shell.layout.dock.show(PanelId::Titles);
        shell.open_animation(Some(title));
        shell.animation.sync(&egui::Context::default(), state);
        configure(&mut shell);
        shell
    };
    let advanced = |shell: &mut StudioShell| {
        shell.set_title_design(title, title_design());
        let binding = shell.animation_binding_mut(title).unwrap();
        let document = binding.document_mut();
        document.set_mode(EditorMode::Advanced);
        document.set_selection(&KeySelection::from_iter([KeyRef {
            track: bar,
            frame: 12,
        }]));
        document.set_playhead(8);
        document.copy_selection().unwrap();
        shell.animation.timeline.active_row = Some(bar);
    };
    let operator = || {
        let mut state = ready(true, Variant::default());
        state.title_previews = true;
        state
    };
    let locked = ready(
        true,
        Variant {
            locked: true,
            ..Variant::default()
        },
    );
    let simple_design = |shell: &mut StudioShell| shell.set_title_design(title, title_design());
    let docked = |shell: &mut StudioShell| {
        simple_design(shell);
        shell.layout.animation_docked = true;
    };
    let unchanged = |_: &mut StudioShell| {};
    let conflict = |shell: &mut StudioShell| {
        advanced(shell);
        let binding = shell.animation_binding_mut(title).unwrap();
        binding.document_mut().extend_phase(Phase::In, 30).unwrap();
        binding.refused(fm_ui_model::title_animation::AnimationRefusal::Conflict);
    };
    let scene_input = input(SCENE_INPUT);
    let scene_simple = |shell: &mut StudioShell| {
        shell.open_scene_animation_window(scene_input);
        shell.animation.sync(&egui::Context::default(), &operator());
    };
    let scene_advanced = |shell: &mut StudioShell| {
        scene_simple(shell);
        let host = TrackKey::new(scene_layer(1), Phase::In, Property::PositionX);
        let binding = shell.scene_animation_binding_mut(scene_input).unwrap();
        let document = binding.document_mut();
        document.set_mode(EditorMode::Advanced);
        document.set_selection(&KeySelection::from_iter([KeyRef {
            track: host,
            frame: 0,
        }]));
        shell.animation.scene_timeline.active_row = Some(host);
    };
    let scenes: Vec<(&str, Configure, StudioUiState)> = vec![
        ("operator/animation", &unchanged, operator()),
        ("operator/animation-scene", &scene_simple, operator()),
        (
            "operator/animation-scene-advanced",
            &scene_advanced,
            operator(),
        ),
        ("operator/animation-design", &simple_design, operator()),
        ("operator/animation-docked", &docked, operator()),
        ("operator/animation-advanced", &advanced, operator()),
        ("operator/animation-conflict", &conflict, operator()),
        ("operator/animation-locked", &advanced, locked),
        (
            "viewer/animation",
            &unchanged,
            ready(false, Variant::default()),
        ),
    ];
    scenes
        .into_iter()
        .map(|(name, configure, state)| Scene::console(name, prepare(&state, configure), state))
        .collect()
}

/// IN 50 frames / OUT 25 frames at 25 fps with two elements: a lower
/// third animated on opacity, position, and colour, and an unanimated logo.
fn animation_document() -> AnimDocument<u32> {
    let track = |property| TrackKey::new(1_u32, Phase::In, property);
    let keys = |from: i64, to: i64| {
        vec![
            Keyframe::new(0, Value::Scalar(from), Easing::Linear),
            Keyframe::new(20, Value::Scalar(to), Easing::CubicOut),
        ]
    };
    let colour = |r| Value::Color(fm_animation::Rgba::new(r, 40, 40, 255));
    let clip = Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        50,
        25,
        vec![
            Track::new(track(Property::Opacity), keys(0, 255)),
            Track::new(track(Property::PositionX), keys(-100, 0)),
            Track::new(
                track(Property::Color),
                vec![
                    Keyframe::new(0, colour(10), Easing::Hold),
                    Keyframe::new(30, colour(200), Easing::Linear),
                ],
            ),
        ],
    )
    .unwrap();
    let mut document = AnimDocument::new(clip, 1, 1_920, 1_080);
    document.set_targets([
        TargetOutline::new(
            1,
            "Lower third",
            TargetKind::Shape,
            TargetBounds::new(100, 800, 900, 160),
        ),
        TargetOutline::new(
            2,
            "Logo",
            TargetKind::Image,
            TargetBounds::new(1_700, 60, 160, 160),
        ),
    ]);
    document
}

/// The animation timeline drawn standalone, in `language`.
#[allow(clippy::too_many_lines)] // one block per audited variant
fn timeline_scenes(language: Language) -> Vec<Scene> {
    let locale = Locale::new(language);
    let opacity = TrackKey::new(1_u32, Phase::In, Property::Opacity);
    let mut scenes = Vec::new();
    for (mode_name, mode) in [
        ("simple", EditorMode::Simple),
        ("advanced", EditorMode::Advanced),
    ] {
        for (variant, selected, refused) in [
            ("selection", true, false),
            ("empty", false, false),
            ("refused", true, true),
        ] {
            let mut document = animation_document();
            document.set_mode(mode);
            let mut state = AnimationTimelineState::default();
            if selected {
                document.set_selection(&KeySelection::from_iter([
                    KeyRef {
                        track: opacity,
                        frame: 20,
                    },
                    KeyRef {
                        track: TrackKey::new(1, Phase::In, Property::PositionX),
                        frame: 20,
                    },
                ]));
                document.set_playhead(10);
                if mode == EditorMode::Advanced {
                    state.active_row = Some(opacity);
                }
                document.copy_selection().unwrap();
            }
            if refused {
                state.show_error(&AnimEditorError::RetimeCollision {
                    track: opacity,
                    min_len: 7,
                });
            }
            scenes.push(Scene {
                name: format!("timeline/{mode_name}-{variant}"),
                draw: Box::new(move |ui| {
                    let _output = AnimationTimelineWidget::new(&mut document, &mut state)
                        .locale(locale)
                        .show(ui);
                }),
            });
        }
    }
    // The custom curve editor and the value graph's bezier handles: the
    // lower third's first opacity key shaped by a custom curve, selected,
    // with opacity the active row; then the same under a lock.
    for (variant, lock) in [
        ("curve", None),
        ("curve-locked", Some(Msg::AnimReasonSaving.en())),
    ] {
        use fm_ui_model::animation_editor::{CurvePreset, CurveTarget};
        let mut document = animation_document();
        document.set_mode(EditorMode::Advanced);
        document.set_selection(&KeySelection::from_iter([KeyRef {
            track: opacity,
            frame: 0,
        }]));
        document
            .apply_curve_preset(CurveTarget::Selection, CurvePreset::Back)
            .unwrap();
        let mut state = AnimationTimelineState::default();
        state.active_row = Some(opacity);
        state.lock_reason = lock;
        scenes.push(Scene {
            name: format!("timeline/advanced-{variant}"),
            draw: Box::new(move |ui| {
                let _output = AnimationTimelineWidget::new(&mut document, &mut state)
                    .locale(locale)
                    .show(ui);
            }),
        });
    }
    // The preset library: two saved presets (one picked, with a name and a
    // path typed) in both modes, so the "My presets" row and the Simple
    // picker's user section show; then the same under a library lock.
    for (variant, mode, lock) in [
        ("simple-presets", EditorMode::Simple, None),
        ("advanced-presets", EditorMode::Advanced, None),
        (
            "advanced-presets-locked",
            EditorMode::Advanced,
            Some(Msg::AnimReasonPresetsPending.en()),
        ),
    ] {
        use crate::animation_timeline::library::UserPresetItem;
        let mut document = animation_document();
        document.set_mode(mode);
        document.set_selection(&KeySelection::from_iter([KeyRef {
            track: opacity,
            frame: 20,
        }]));
        let motion = std::sync::Arc::new(document.capture_motion(1, Phase::In).unwrap());
        let mut state = AnimationTimelineState::default();
        state.active_row = Some(opacity);
        state.user_presets = ["Swoosh", "Drift"]
            .into_iter()
            .zip(1_u32..)
            .map(|(name, id)| UserPresetItem {
                preset: core::num::NonZeroU32::new(id).unwrap(),
                name: name.to_owned(),
                phase: Phase::In,
                motion: motion.clone(),
            })
            .collect();
        state.library.name = "Bounce".into();
        state.library.pick = core::num::NonZeroU32::new(1);
        state.library.path = "/presets/swoosh.fmpreset".into();
        state.library_reason = lock;
        scenes.push(Scene {
            name: format!("timeline/{variant}"),
            draw: Box::new(move |ui| {
                let _output = AnimationTimelineWidget::new(&mut document, &mut state)
                    .locale(locale)
                    .show(ui);
            }),
        });
    }
    scenes
}

/// The colour panel drawn standalone, in `language`.
fn color_panel_scenes(language: Language) -> Vec<Scene> {
    use fm_grade::{CurveChannel, GradeLut, HueCurveKind, Wheel, WheelKind};
    use fm_ui_model::grade_editor::{
        CompareMode, CurveTarget, GradeDocument, GradeEditorError, GradeParam, ScopeKind,
        sample_still,
    };

    let locale = Locale::new(language);
    let still = sample_still(320, 180);
    let mut scenes = Vec::new();
    for (mode_name, mode) in [
        ("simple", PanelMode::Simple),
        ("advanced", PanelMode::Advanced),
    ] {
        for (variant, edited, refused) in [
            ("fresh", false, false),
            ("edited", true, false),
            ("refused", true, true),
        ] {
            let mut document = GradeDocument::default();
            let mut state = ColorPanelState::with_source(&still);
            state.mode = mode;
            if edited {
                document.set_param(GradeParam::Exposure, 400).unwrap();
                document
                    .set_wheel(WheelKind::Gain, Wheel::new(200, -150, 100))
                    .unwrap();
                document
                    .add_curve_point(CurveTarget::Rgb(CurveChannel::Master), [0.4, 0.5])
                    .unwrap();
                document
                    .add_curve_point(CurveTarget::Hue(HueCurveKind::HueVsSaturation), [0.1, 0.8])
                    .unwrap();
                document
                    .set_lut(Some(GradeLut::new("asset://luts/film.cube", 600)))
                    .unwrap();
                document.copy();
                document.toggle_ab().unwrap();
                document.set_bypass(true).unwrap();
                document.undo().unwrap();
                state.lut_choices = vec![
                    "asset://luts/film.cube".to_owned(),
                    "asset://luts/night.cube".to_owned(),
                ];
                state.compare = CompareMode::Wipe;
                state.scope = ScopeKind::Vectorscope;
                state.curve = CurveTarget::Hue(HueCurveKind::HueVsSaturation);
            }
            if refused {
                state.compare = CompareMode::SideBySide;
                state.scope = ScopeKind::Histogram;
                state.show_error(&GradeEditorError::CurveFull { max: 16 });
            }
            scenes.push(Scene {
                name: format!("color/{mode_name}-{variant}"),
                draw: Box::new(move |ui| {
                    let _output = ColorPanelWidget::new(&mut document, &mut state)
                        .locale(locale)
                        .show(ui);
                }),
            });
        }
    }
    scenes
}

/// One interactive widget and the help it showed.
#[derive(Clone, Debug)]
struct Control {
    scene: String,
    /// The widget's id path (debug builds), for diagnostics.
    path: String,
    /// Kind and label from egui's widget info, when the widget reports one.
    label: String,
    rect: Rect,
    enabled: bool,
    tooltip: Option<String>,
}

impl Control {
    fn describe(&self) -> String {
        format!(
            "[{}] {} {}at ({:.0},{:.0}) id {}",
            self.scene,
            self.label,
            if self.enabled { "" } else { "(disabled) " },
            self.rect.min.x,
            self.rect.min.y,
            self.path,
        )
    }
}

/// Renders `scene` with every tooltip forced open and collects each
/// interactive widget with the text of its tooltip. Submenus open only
/// under the pointer, so each submenu button found is then hovered in turn
/// and its items collected too.
fn audit_scene(scene: Scene) -> Vec<Control> {
    let Scene { name, mut draw } = scene;
    let context = egui::Context::default();
    let mut time = 1.0;
    let mut pass = |draw: &mut dyn FnMut(&mut egui::Ui), events: Vec<egui::Event>| {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            time: Some(time),
            events,
            ..egui::RawInput::default()
        });
        time += 0.1;
        let mut ui = egui::Ui::new(
            context.clone(),
            Id::new("hover-audit"),
            egui::UiBuilder::new()
                .layer_id(LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        draw(&mut ui);
        drop(ui);
        let _output = context.end_pass();
        collect(&context, &name)
    };
    // The first pass installs the console style; widget info is recorded
    // from the next one, and a third settles popup and area sizes.
    pass(&mut draw, Vec::new());
    context.all_styles_mut(|style| {
        style.debug.show_interactive_widgets = true;
        // Selectable text is not a control; a label that acts on a click
        // still senses it and stays audited.
        style.interaction.selectable_labels = false;
    });
    context.memory_mut(|memory| memory.set_everything_is_visible(true));
    pass(&mut draw, Vec::new());
    let mut controls = pass(&mut draw, Vec::new());
    let submenus: Vec<Rect> = controls
        .iter()
        .filter(|control| control.enabled && control.label.ends_with("⏵\""))
        .map(|control| control.rect)
        .collect();
    for rect in submenus {
        let hover = vec![egui::Event::PointerMoved(rect.center())];
        pass(&mut draw, hover);
        controls.extend(pass(&mut draw, Vec::new()));
    }
    controls
}

/// Every interactive control of the last pass with its tooltip text.
fn collect(context: &egui::Context, scene: &str) -> Vec<Control> {
    context.viewport(|viewport| {
        let widgets = &viewport.prev_pass.widgets;
        let all: Vec<WidgetRect> = widgets
            .layers()
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect();
        // egui's colour picker popup is egui's own widget, not a console
        // control; the colour button that opens it is audited.
        let colour_popups: egui::IdSet = all
            .iter()
            .filter(|widget| {
                widgets
                    .info(widget.id)
                    .is_some_and(|info| info.typ == egui::WidgetType::ColorButton)
            })
            .map(|widget| widget.id.with("popup"))
            .collect();
        let tooltip_text = |owner: Id| -> Option<String> {
            let mut lines = Vec::new();
            let mut found = false;
            for count in 0..4_usize {
                let layer = LayerId::new(Order::Tooltip, owner.with(count));
                let mut any = false;
                for widget in widgets.get_layer(layer) {
                    any = true;
                    if let Some(info) = widgets.info(widget.id)
                        && let Some(label) = &info.label
                        && !label.is_empty()
                    {
                        lines.push(label.clone());
                    }
                }
                if !any {
                    break;
                }
                found = true;
            }
            found.then(|| lines.join("\n"))
        };
        all.iter()
            .filter(|widget| widget.sense.interactive())
            .filter(|widget| widget.layer_id.order != Order::Tooltip)
            .filter(|widget| !colour_popups.contains(&widget.layer_id.id))
            .filter(|widget| !is_egui_chrome(widget, widgets.info(widget.id)))
            .map(|widget| {
                let label = widgets.info(widget.id).map_or_else(
                    || "custom widget".to_owned(),
                    |info| format!("{:?} {:?}", info.typ, info.label.as_deref().unwrap_or("")),
                );
                Control {
                    scene: scene.to_owned(),
                    path: format!("{:?}", widget.id),
                    label,
                    rect: widget.rect,
                    enabled: widget.enabled,
                    tooltip: tooltip_text(widget.id),
                }
            })
            .collect()
    })
}

/// Whether `widget` is interaction egui adds for a container rather than an
/// operator control: an area's own surface, a modal's backdrop and frame,
/// panel and window resize edges, window title bars, scroll bars, and
/// drag-to-scroll regions. Recognised by widget type and by the ids egui
/// derives for them (debug builds keep each id's derivation).
fn is_egui_chrome(widget: &WidgetRect, info: Option<&egui::WidgetInfo>) -> bool {
    const CHROME_SUFFIXES: [&str; 14] = [
        ".with(\"__resize\")",
        ".with(\"__resize_corner\")",
        ".with(\"resize\")",
        ".with(\"move\")",
        ".with(\"area\")",
        ".with(\"__title_click\")",
        ".with(\"right\")",
        ".with(\"left\")",
        ".with(\"top\")",
        ".with(\"bottom\")",
        ".with(\"right_bottom\")",
        ".with(\"right_top\")",
        ".with(\"left_bottom\")",
        ".with(\"left_top\")",
    ];
    if widget.id == widget.layer_id.id
        || info.is_some_and(|info| info.typ == egui::WidgetType::ScrollBar)
    {
        return true;
    }
    let path = format!("{:?}", widget.id);
    if CHROME_SUFFIXES.iter().any(|suffix| path.ends_with(suffix)) {
        return true;
    }
    // A modal's backdrop and the click-absorbing scope around its frame are
    // the area's first-level child `Ui`s: `area.with("child").with(n)`.
    let area_child = format!(
        "{:?}.with(IdSalt::new(\"child\")).with(",
        widget.layer_id.id
    );
    path.strip_prefix(&area_child)
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|salt| salt.chars().all(|c| c.is_ascii_digit()))
}

/// Every check's failures for one language.
#[derive(Debug, Default)]
struct Findings {
    controls: usize,
    missing: BTreeMap<String, String>,
    no_reason: BTreeMap<String, String>,
    no_action: BTreeMap<String, String>,
    too_long: BTreeMap<String, String>,
    untranslated: BTreeMap<String, String>,
}

fn audit(language: Language, scenes: Vec<Scene>) -> (Findings, Vec<Control>) {
    let locale = Locale::new(language);
    let unavailable = locale.format(Msg::Unavailable, &[("reason", Arg::Str(""))]);
    let unavailable = unavailable.trim();
    let mut findings = Findings::default();
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    for scene in scenes {
        for control in audit_scene(scene) {
            let key = format!("{}|{}", control.path, control.enabled);
            let first = seen.insert(key.clone());
            if first {
                findings.controls += 1;
            }
            match &control.tooltip {
                None => {
                    findings
                        .missing
                        .entry(key)
                        .or_insert_with(|| control.describe());
                }
                Some(text) => {
                    if !control.enabled && !text.contains(unavailable) {
                        findings
                            .no_reason
                            .entry(key.clone())
                            .or_insert_with(|| format!("{} -> {text:?}", control.describe()));
                    }
                    // Help stays short: a line or two, and never more than
                    // the input tile's name, tally, picture, and gestures.
                    let help_lines = text
                        .lines()
                        .filter(|line| !line.trim().starts_with(unavailable))
                        .count();
                    if help_lines > MAX_HELP_LINES {
                        findings
                            .too_long
                            .entry(key.clone())
                            .or_insert_with(|| format!("{} -> {text:?}", control.describe()));
                    }
                    let mut lines: Vec<&str> = text.lines().collect();
                    lines.sort_unstable();
                    if lines.windows(2).any(|pair| pair[0] == pair[1]) {
                        findings.too_long.entry(key.clone()).or_insert_with(|| {
                            format!("{} repeats a line -> {text:?}", control.describe())
                        });
                    }
                    // A disabled control still says what it would do.
                    if !control.enabled
                        && text
                            .lines()
                            .all(|line| line.trim().starts_with(unavailable))
                    {
                        findings
                            .no_action
                            .entry(key.clone())
                            .or_insert_with(|| format!("{} -> {text:?}", control.describe()));
                    }
                    if language != Language::English {
                        for line in text.lines() {
                            if locale.tr(line) != line {
                                findings.untranslated.entry(key.clone()).or_insert_with(|| {
                                    format!("{} -> {line:?}", control.describe())
                                });
                            }
                        }
                    }
                }
            }
            all.push(control);
        }
    }
    (findings, all)
}

fn report(language: Language, findings: &Findings) -> String {
    let mut out = format!(
        "{language:?}: {} interactive widgets audited; {} without hover help, {} disabled without a reason, {} disabled without their action, {} too long, {} with untranslated help\n",
        findings.controls,
        findings.missing.len(),
        findings.no_reason.len(),
        findings.no_action.len(),
        findings.too_long.len(),
        findings.untranslated.len()
    );
    for (title, entries) in [
        ("no hover help", &findings.missing),
        ("disabled without a reason", &findings.no_reason),
        ("disabled without its action", &findings.no_action),
        ("longer than four lines", &findings.too_long),
        ("untranslated", &findings.untranslated),
    ] {
        for entry in entries.values() {
            let _ = writeln!(out, "  {title}: {entry}");
        }
    }
    out
}

/// `FM_HOVER_AUDIT_DUMP=dir` writes every audited control with its help,
/// one file per audit and language, for reviewing the wording.
fn dump(file: &str, language: Language, controls: &[Control]) {
    if let Some(dir) = std::env::var_os("FM_HOVER_AUDIT_DUMP") {
        let mut dump = String::new();
        let mut seen = BTreeSet::new();
        for control in controls {
            if seen.insert((control.path.clone(), control.enabled)) {
                let _ = writeln!(
                    dump,
                    "{}\n    {}",
                    control.describe(),
                    control
                        .tooltip
                        .as_deref()
                        .unwrap_or("<none>")
                        .replace('\n', "\n    ")
                );
            }
        }
        let path = std::path::Path::new(&dir).join(format!("{file}-{}.txt", language.code()));
        std::fs::write(path, dump).unwrap();
    }
}

fn locale_text(language: Language, msg: Msg) -> String {
    Locale::new(language).text(msg).to_owned()
}

/// Whether any rule failed.
fn failed(findings: &Findings) -> bool {
    !(findings.missing.is_empty()
        && findings.no_reason.is_empty()
        && findings.no_action.is_empty()
        && findings.too_long.is_empty()
        && findings.untranslated.is_empty())
}

/// Audits every console scene in `language` and panics with a report of
/// every control whose help is missing, unexplained, too long, or
/// untranslated.
pub fn check(language: Language) {
    let (findings, controls) = audit(language, scenes(language));
    dump("hover", language, &controls);
    println!("{}", report(language, &findings));
    assert!(
        findings.controls > 500,
        "the audit found only {} widgets; is the fixture still rich?",
        findings.controls
    );
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    // Each single-key console shortcut is named on a control that performs it.
    for entry in crate::STUDIO_SHORTCUTS {
        if entry.key.is_none() || entry.keys.chars().count() != 1 {
            continue;
        }
        let hint = format!("[{}]", entry.keys);
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|text| text.contains(&hint))
        {
            let _ = writeln!(failures, "no tooltip names the {hint} shortcut");
        }
    }
    // Both live source clock keys explain themselves (AU-004).
    for state in [Msg::SyncStateRunning, Msg::SyncStateRecovering] {
        let state = locale_text(language, state);
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|text| text.contains(&format!(", {state})")))
        {
            let _ = writeln!(failures, "no SYNC key tooltip shows the {state:?} state");
        }
    }
    // So is each window and output chord, in the audited language's names.
    let locale = Locale::new(language);
    for chord in crate::keymap::Chord::ALL {
        let hint = format!("[{}]", locale.keys(chord.keys()));
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|text| text.contains(&hint))
        {
            let _ = writeln!(failures, "no tooltip names the {hint} shortcut");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the camera / capture form of the Add input dialog and the
/// capture tile badges (IN-001) in `language` under the console rules, and
/// requires each capture control's help (device, filters, formats,
/// Automatic, audio, ALSA device, loss slate, Rescan) and the SIGNAL LOST
/// and LIVE tile details to have been shown.
pub fn check_capture(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, capture_scenes(locale));
    dump("hover-capture", language, &controls);
    println!("{}", report(language, &findings));
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let inventory = capture_inventory();
    let mut required: Vec<String> = [
        Msg::KindCaptureHover,
        Msg::CapRefreshHover,
        Msg::CapFilterSizeHover,
        Msg::CapFilterRateHover,
        Msg::CapFilterPixelHover,
        Msg::CapFormatAutoHover,
        Msg::CapAudioNoneHover,
        Msg::CapAudioPairedHover,
        Msg::CapAudioAlsaHover,
        Msg::CapAlsaDeviceHover,
        Msg::CapLossHoldHover,
        Msg::CapLossBlackHover,
    ]
    .into_iter()
    .map(|msg| locale.text(msg).to_owned())
    .collect();
    // A device, an interlaced format, and both capture tiles' details.
    required.push(inventory.devices[0].path.clone());
    required.push(crate::capture_input_ui::format_hover(
        locale,
        &inventory.devices[1].formats[1],
    ));
    required.push(locale.format(
        Msg::CapDetailLost,
        &[
            ("state", Arg::Str(locale.text(Msg::CapBadgeSignalLost))),
            ("reason", Arg::Str(locale.text(Msg::CapLostUnplugged))),
        ],
    ));
    required.push(locale.format(
        Msg::CapDetailDevice,
        &[
            ("device", Arg::Str("/dev/video2")),
            ("state", Arg::Str(locale.text(Msg::Live))),
        ],
    ));
    for text in required {
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|tooltip| tooltip.contains(&text))
        {
            let _ = writeln!(failures, "no capture tooltip shows {text:?}");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the Image and Slideshow forms of the Add input dialog and the
/// Media tab's slideshow panel (IN-008, IN-009) in `language` under the
/// console rules, and requires the help of every form key and field and of
/// every slideshow key to have been shown.
pub fn check_stills(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, still_scenes(locale));
    dump("hover-stills", language, &controls);
    println!("{}", report(language, &findings));
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let mut required: Vec<String> = [
        Msg::KindImageHover,
        Msg::KindSlideshowHover,
        Msg::StPictureHover,
        Msg::StFitFitHover,
        Msg::StFitFillHover,
        Msg::StFitStretchHover,
        Msg::StFitOneToOneHover,
        Msg::StSourceFolderHover,
        Msg::StSourceListHover,
        Msg::StFolderHover,
        Msg::StPicturesHover,
        Msg::StSlideSecondsHover,
        Msg::StCutHover,
        Msg::StFadeHover,
        Msg::StFadeSecondsHover,
        Msg::StKenBurnsHover,
        Msg::StRandomHover,
        Msg::StLoopHover,
        Msg::StAutoAdvanceHover,
        Msg::StFirstHover,
        Msg::StPreviousHover,
        Msg::StNextHover,
        Msg::StLastHover,
    ]
    .into_iter()
    .map(|msg| locale.text(msg).to_owned())
    .collect();
    required.push(locale.format(
        Msg::StSlideKeyHover,
        &[("page", Arg::from(2_u64)), ("name", Arg::Str("Second"))],
    ));
    for text in required {
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|tooltip| tooltip.contains(&text))
        {
            let _ = writeln!(failures, "no still tooltip shows {text:?}");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the system audio form of the Add input and Replace source
/// dialogs (AU-011, Protocol 2.68) in `language` under the console rules,
/// and requires the help of its audio system keys, Refresh list, the device
/// picker with its Default device and a listed device, and the name field
/// to have been shown.
pub fn check_system_audio(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, system_audio_scenes(locale));
    dump("hover-system-audio", language, &controls);
    println!("{}", report(language, &findings));
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let mut required: Vec<String> = [
        Msg::KindSystemAudioHover,
        Msg::SaBackendPulseHover,
        Msg::SaBackendAlsaHover,
        Msg::SaRefreshHover,
        Msg::SaPickerHover,
        Msg::SaDeviceHover,
    ]
    .into_iter()
    .map(|msg| locale.text(msg).to_owned())
    .collect();
    required.push(locale.format(
        Msg::SaPickDefaultPulseHover,
        &[("device", Arg::Str("@DEFAULT_MONITOR@"))],
    ));
    required.push(locale.format(
        Msg::SaPickDefaultAlsaHover,
        &[("device", Arg::Str("default"))],
    ));
    for device in system_audio_inventory().devices {
        required.push(locale.format(
            Msg::SaDeviceOptionHover,
            &[("device", Arg::Str(&device.device))],
        ));
    }
    for text in required {
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|tooltip| tooltip.contains(&text))
        {
            let _ = writeln!(failures, "no system audio tooltip shows {text:?}");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the mixer's headphone device picker (AU-001, Protocol 2.102) in
/// `language` under the console rules, and requires the help of the picker,
/// meter-only, every listed device, an unplugged device, typing a name, the
/// listing and failed-listing rows, and the typed device field to have been
/// shown.
pub fn check_monitor_output(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, monitor_output_scenes(locale));
    dump("hover-monitor-output", language, &controls);
    println!("{}", report(language, &findings));
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let mut required: Vec<String> = [
        Msg::MonitorPickerHover,
        Msg::MonitorPickNoneHover,
        Msg::MonitorPickTypedHover,
        Msg::MonitorPickListingHover,
    ]
    .into_iter()
    .map(|msg| locale.text(msg).to_owned())
    .collect();
    required.push(locale.format(
        Msg::MonitorDeviceShownHover,
        &[("device", Arg::Str("hw:CARD=Headset,DEV=0"))],
    ));
    required.push(locale.format(
        Msg::MonitorPickMissingHover,
        &[("device", Arg::Str("hw:CARD=Gone,DEV=0"))],
    ));
    required.push(locale.format(
        Msg::MonitorPickListErrorHover,
        &[(
            "error",
            Arg::Str("no ALSA driver is loaded (no /proc/asound)"),
        )],
    ));
    for device in fm_protocol::AudioOutputInventory::simulated().devices {
        required.push(locale.format(
            Msg::MonitorPickItemHover,
            &[
                ("name", Arg::Str(&device.description)),
                ("device", Arg::Str(&device.device)),
            ],
        ));
    }
    for text in required {
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|tooltip| tooltip.contains(&text))
        {
            let _ = writeln!(failures, "no headphone device tooltip shows {text:?}");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the screen and window capture form of the Add input dialog
/// (IN-011) in `language` under the console rules, and requires the help
/// of the kind key, Refresh targets, every listed screen and window, the
/// three ways to follow a window, the title field, the crop, the pointer,
/// the rate cap, both stall slates, the three audio choices (with the
/// reasons Application audio is unavailable for a screen and on a Wayland
/// server), and a Wayland server's screen and window choices to have been
/// shown.
pub fn check_screen_capture(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, screen_capture_scenes(locale));
    dump("hover-screen-capture", language, &controls);
    println!("{}", report(language, &findings));
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let mut required: Vec<String> = [
        Msg::KindScreenCaptureHover,
        Msg::ScRefreshHover,
        Msg::ScMatchExactHover,
        Msg::ScMatchContainsHover,
        Msg::ScMatchAnyHover,
        Msg::ScTitleHover,
        Msg::ScCropHover,
        Msg::ScCursorHover,
        Msg::ScFpsHover,
        Msg::ScStallFreezeHover,
        Msg::ScStallBlackHover,
        Msg::ScAudioNoneHover,
        Msg::ScAudioSystemHover,
        Msg::ScAudioAppHover,
        Msg::ScPortalScreenHover,
        Msg::ScPortalWindowHover,
        Msg::ScAudioBadgeOpeningHover,
        Msg::ScAudioBadgeLiveHover,
        Msg::ScAudioBadgeNoStreamHover,
        Msg::ScAudioBadgeUnsupportedHover,
        Msg::ScAudioBadgeFailedHover,
    ]
    .into_iter()
    .map(|msg| locale.text(msg).to_owned())
    .collect();
    for reason in [Msg::ScAudioAppScreenHover, Msg::ScAudioAppPortalHover] {
        required.push(crate::l10n::disabled_text(
            locale,
            locale.text(Msg::ScAudioAppHover),
            reason.en(),
        ));
    }
    let inventory = screen_capture_inventory();
    for screen in &inventory.screens {
        required.push(locale.format(
            Msg::ScScreenOptionHover,
            &[
                ("number", Arg::from(u64::from(screen.index) + 1)),
                (
                    "size",
                    Arg::Str(&format!("{}x{}", screen.width, screen.height)),
                ),
                ("x", Arg::Str(&screen.x.to_string())),
                ("y", Arg::Str(&screen.y.to_string())),
            ],
        ));
    }
    for window in &inventory.windows {
        required.push(locale.format(
            Msg::ScWindowOptionHover,
            &[
                ("app", Arg::Str(&window.app)),
                ("title", Arg::Str(&window.title)),
                (
                    "size",
                    Arg::Str(&format!("{}x{}", window.width, window.height)),
                ),
            ],
        ));
    }
    for text in required {
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|tooltip| tooltip.contains(&text))
        {
            let _ = writeln!(failures, "no screen capture tooltip shows {text:?}");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the standalone animation timeline in `language` under the same
/// rules, and requires every transport shortcut to be named on its button.
pub fn check_timeline(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, timeline_scenes(language));
    dump("hover-timeline", language, &controls);
    println!("{}", report(language, &findings));
    assert!(
        findings.controls > 40,
        "the timeline audit found only {} widgets; is the fixture still rich?",
        findings.controls
    );
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    for action in TimelineAction::ALL {
        let hint = format!("[{}]", locale.keys(action.shortcut(locale)));
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|text| text.contains(&hint))
        {
            let _ = writeln!(failures, "no timeline tooltip names the {hint} shortcut");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

/// Audits the standalone colour panel in `language` under the same rules,
/// and requires every action shortcut to be named on its key.
pub fn check_color_panel(language: Language) {
    let locale = Locale::new(language);
    let (findings, controls) = audit(language, color_panel_scenes(language));
    dump("hover-color", language, &controls);
    println!("{}", report(language, &findings));
    assert!(
        findings.controls > 60,
        "the colour panel audit found only {} widgets; is the fixture still rich?",
        findings.controls
    );
    let mut failures = String::new();
    if failed(&findings) {
        failures.push_str(&report(language, &findings));
    }
    let hints = GradeAction::ALL
        .into_iter()
        .filter_map(|action| action.shortcut(locale))
        .chain([COMPARE_SHORTCUT]);
    for key in hints {
        let hint = format!("[{}]", locale.keys(key));
        if !controls
            .iter()
            .filter_map(|control| control.tooltip.as_deref())
            .any(|text| text.contains(&hint))
        {
            let _ = writeln!(
                failures,
                "no colour panel tooltip names the {hint} shortcut"
            );
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

#[test]
fn the_audit_detects_a_widget_without_help_and_ignores_egui_chrome() {
    // A bare button is reported; a helped one and a scroll area are not.
    let context = egui::Context::default();
    context.memory_mut(|memory| memory.set_everything_is_visible(true));
    context.all_styles_mut(|style| {
        style.debug.show_interactive_widgets = true;
        style.interaction.selectable_labels = false;
    });
    let pass = || {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(300.0))),
            ..egui::RawInput::default()
        });
        let mut root = egui::Ui::new(
            context.clone(),
            Id::new("hover-audit-self-test"),
            egui::UiBuilder::new()
                .layer_id(LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        egui::CentralPanel::default().show(&mut root, |ui| {
            let _ = ui.button("bare");
            let _ = ui.button("helped").on_hover_text("does a thing");
            let _ = ui
                .add_enabled(false, egui::Button::new("off"))
                .on_disabled_hover_text("Unavailable: because");
            egui::ScrollArea::vertical()
                .max_height(40.0)
                .show(ui, |ui| {
                    for row in 0..20 {
                        ui.label(format!("row {row}"));
                    }
                });
        });
        drop(root);
        let _ = context.end_pass();
    };
    pass();
    pass();
    context.viewport(|viewport| {
        let widgets = &viewport.prev_pass.widgets;
        let all: Vec<WidgetRect> = widgets
            .layers()
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect();
        let interactive: Vec<&WidgetRect> = all
            .iter()
            .filter(|widget| widget.sense.interactive())
            .filter(|widget| widget.layer_id.order != Order::Tooltip)
            .filter(|widget| !is_egui_chrome(widget, widgets.info(widget.id)))
            .collect();
        let labels: Vec<String> = interactive
            .iter()
            .filter_map(|widget| widgets.info(widget.id)?.label.clone())
            .collect();
        assert_eq!(labels, ["bare", "helped", "off"], "{interactive:?}");
        let has_tooltip = |label: &str| {
            let widget = interactive
                .iter()
                .find(|widget| {
                    widgets
                        .info(widget.id)
                        .and_then(|info| info.label.as_deref())
                        == Some(label)
                })
                .unwrap();
            widgets
                .get_layer(LayerId::new(Order::Tooltip, widget.id.with(0_usize)))
                .next()
                .is_some()
        };
        assert!(!has_tooltip("bare"));
        assert!(has_tooltip("helped"));
        assert!(has_tooltip("off"));
    });
}

/// An encrypted SRT listener on the third and fourth channels, as a client
/// sees it (passphrase masked).
fn srt_listener() -> fm_protocol::NetworkStreamConfig {
    let mut config = fm_protocol::NetworkStreamConfig::new("srt://0.0.0.0:9000");
    config.settings.mode = fm_protocol::StreamMode::Listener;
    config.settings.audio = fm_protocol::AudioChannels::Pair34;
    config.passphrase = Some("***".into());
    config.stream_id = Some("cam1".into());
    config
}

/// Renders the audited console scenes whose names contain one of the
/// comma-separated `$FM_SCENE_RENDER` words to `$FM_SCENE_RENDER_DIR`, at
/// 1920x1080 and 1280x720, in English and Spanish, for visual review
/// (`FM_SCENE_RENDER_DIR=<dir> FM_SCENE_RENDER=strip-fx,colour cargo test
/// -p fm-ui-egui --lib render_scenes_for_review -- --ignored`).
#[test]
#[ignore = "writes review screenshots"]
fn render_scenes_for_review() {
    use crate::offscreen::{Canvas, Textures, encode_png};
    let Ok(dir) = std::env::var("FM_SCENE_RENDER_DIR") else {
        return;
    };
    let wanted = std::env::var("FM_SCENE_RENDER").unwrap_or_default();
    let wanted: Vec<&str> = wanted.split(',').filter(|word| !word.is_empty()).collect();
    for language in [Language::English, Language::Spanish] {
        for size in [Vec2::new(1920.0, 1080.0), Vec2::new(1280.0, 720.0)] {
            for Scene { name, mut draw } in scenes(language) {
                if !wanted.is_empty() && !wanted.iter().any(|word| name.contains(word)) {
                    continue;
                }
                let context = egui::Context::default();
                let mut textures = Textures::default();
                let mut last = None;
                for step in 0..30 {
                    context.begin_pass(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(1.0 + f64::from(step) * 0.1),
                        ..egui::RawInput::default()
                    });
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        Id::new("scene-review"),
                        egui::UiBuilder::new()
                            .layer_id(LayerId::background())
                            .max_rect(context.viewport_rect()),
                    );
                    draw(&mut ui);
                    drop(ui);
                    let output = context.end_pass();
                    textures.apply(&output);
                    last = Some(output);
                }
                let full = last.unwrap();
                let primitives = context.tessellate(full.shapes, full.pixels_per_point);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let mut canvas = Canvas::new(
                    size.x as usize,
                    size.y as usize,
                    crate::theme::graphite_color(),
                );
                canvas.paint(&primitives, &textures, full.pixels_per_point);
                let (width, height, rgba) =
                    canvas.crop_rgba8(Rect::from_min_size(egui::Pos2::ZERO, size));
                let file = format!(
                    "{dir}/{}-{width}x{height}-{}.png",
                    name.replace('/', "-"),
                    language.code()
                );
                std::fs::write(file, encode_png(width, height, &rgba)).unwrap();
            }
        }
    }
}
