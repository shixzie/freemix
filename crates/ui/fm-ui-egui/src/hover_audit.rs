//! Hover-help audit (parity row RC-018): every interactive widget in the
//! console explains itself on hover, in every shipped language.
//!
//! # Approach
//!
//! The audit renders the real [`StudioShell`] headlessly over rich fixtures
//! (each side tab, including the Media tab's transport bars, every dialog, the advanced controls behind each
//! disclosure, a session without permissions, a live manual transition, a
//! locked project, Fade-to-Black and a running playlist, toasts, the input
//! lifecycle (colour labels, badges, the category filter, the tile menu's
//! Category and Colour label submenus, and the Replace source, Import
//! files, and Categories dialogs), the mixer's strip labels, pan knobs,
//! group assignment, group strips (solo and parent group), the monitor
//! strip with its solo-mode keys, the Fade-to-Black output menu, the
//! Fullscreen outputs dialog (with and without a display list or a
//! presenter, with a draft, changed elsewhere, refused, applying, for a
//! viewer, and locked), and a
//! disconnected console) in English
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
//! The audits are hidden public entry points ([`check`],
//! [`check_timeline`], [`check_color_panel`], [`check_capture`]) so the
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
#[allow(clippy::too_many_lines)] // one protocol fixture
fn model(variant: Variant) -> ClientModel {
    let Variant {
        manual,
        locked,
        black,
        playing,
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
            },
            OutputStatus {
                output: output(2),
                name: "Clean Feed".into(),
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
            })
            .collect(),
        desired_program: wire(1),
        desired_preview: wire(2),
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
        cue_list_states: 2,
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
            fields: vec![
                TitleFieldStatus {
                    field: field(1),
                    name: "Name".into(),
                    value: TitleFieldValue::Text("Ana".into()),
                },
                TitleFieldStatus {
                    field: field(2),
                    name: "Score".into(),
                    value: TitleFieldValue::Number(4),
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
                },
                TitleFieldStatus {
                    field: field(4),
                    name: "Logo".into(),
                    value: TitleFieldValue::Image {
                        asset_uri: "asset://logo.png".into(),
                    },
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
    model
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
        WireCueAction::Preview { input: wire(2) },
        WireCueAction::Take {
            input: None,
            effect: fm_protocol::TransitionFavoriteEffect::Mix(
                fm_protocol::AutoTransitionKind::Fade,
            ),
            duration_frames: 25,
        },
        WireCueAction::OverlayOn {
            channel,
            input: wire(3),
        },
        WireCueAction::OverlayOff { channel },
        WireCueAction::TitleOn {
            input: wire(TITLE_INPUT),
            channel,
        },
        WireCueAction::TitleText {
            input: wire(TITLE_INPUT),
            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
            text: "Ana Ruiz".into(),
        },
        WireCueAction::TitleOff {
            input: wire(TITLE_INPUT),
        },
        WireCueAction::FadeToBlack {
            active: true,
            duration_frames: 25,
        },
        WireCueAction::PlaylistStart,
        WireCueAction::PlaylistStop,
        WireCueAction::AudioGain {
            input: wire(1),
            gain_millidb: -6_000,
        },
        WireCueAction::AudioMute {
            input: wire(2),
            muted: true,
        },
        WireCueAction::Record {
            recorder: WireRecorderSlot::new(1).unwrap(),
            start: true,
        },
        WireCueAction::Stream {
            target: WireStreamTargetId::new(NonZeroU128::new(1).unwrap()),
            start: false,
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
                            input: Some(wire(4)),
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
                                input: wire(4),
                                operation: WireCueMediaOperation::Restart,
                            },
                            WireCueAction::Delay { millis: 1_500 },
                            WireCueAction::Media {
                                input: wire(4),
                                operation: WireCueMediaOperation::Seek { frame: 250 },
                            },
                            WireCueAction::Media {
                                input: wire(4),
                                operation: WireCueMediaOperation::Play,
                            },
                            WireCueAction::Media {
                                input: wire(4),
                                operation: WireCueMediaOperation::Pause,
                            },
                        ],
                    ),
                ],
            },
            state: CueRunStatus {
                standby: Some(20),
                live: Some(10),
                pending: None,
                played: vec![10],
                fault: Some((30, 0)),
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
                        WireCueAction::Preview { input: wire(1) },
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
            },
        },
    ]
}

/// Which optional live states the fixture show is in.
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // independent fixture switches
struct Variant {
    manual: bool,
    locked: bool,
    black: bool,
    playing: bool,
}

/// OR-011: slot 1 shows Clean Feed on a connected display (remembered by
/// id and name, at an exact mode, filled, HDR auto) and is active; slot 2
/// shows Preview on a display named `Projector` that is unplugged.
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

fn meters() -> AudioMetersMessage {
    let channel = |peak| AudioMeterChannel {
        peak_millionths: peak,
        rms_millionths: peak / 2,
    };
    AudioMetersMessage {
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
        monitor: Some(fm_protocol::AudioMonitorMeters {
            solo: true,
            channels: vec![channel(300_000); 2],
        }),
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
fn ready(operator: bool, variant: Variant) -> StudioUiState {
    let model = model(variant);
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
    state.audio_meters = Some(meters());
    state
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
    let dialogs: [(&str, Configure); 15] = [
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
    scenes.extend(animation_scenes(locale));
    scenes.extend(designer_scenes(locale));
    scenes.extend(transition_menu_scenes(locale));
    scenes.extend(dock_scenes(locale));
    scenes.extend(capture_scenes(locale));
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
/// keyed layer with a full effect list (Add effect refused), two layers
/// selected (their effects differ), the project padlock, and a viewer.
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
    let both = |shell: &mut StudioShell| {
        let document = shell.scene_layout_draft_mut(scene_id()).unwrap();
        document.select(scene_layer(1), false);
        document.select(scene_layer(2), true);
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
        ("operator/designer-multi", &both, operator()),
        ("operator/designer-locked", &edited, locked),
        ("viewer/designer", &both, ready(false, Variant::default())),
    ];
    scenes
        .into_iter()
        .map(|(name, configure, state)| Scene::console(name, prepare(&state, configure), state))
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
        let hint = format!("[{}]", action.shortcut(locale));
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
        let hint = format!("[{key}]");
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
