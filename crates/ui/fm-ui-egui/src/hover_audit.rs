//! Hover-help audit (parity row RC-018): every interactive widget in the
//! console explains itself on hover, in every shipped language.
//!
//! # Approach
//!
//! The audit renders the real [`StudioShell`] headlessly over rich fixtures
//! (each side tab, every dialog, the advanced controls behind each
//! disclosure, a session without permissions, a live manual transition, a
//! locked project, Fade-to-Black and a running playlist, toasts, and a
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
//! with keys selected and an active row, with nothing selected, and with a
//! refused edit showing, each pinned to the audited language. The colour
//! grading panel is audited the same way: Simple and Advanced modes, fresh
//! (with its keys disabled for a reason) and edited (with a LUT, compare
//! slots, a copied grade, bypass, the wipe compare, and a hue curve), and
//! with a refused edit showing.

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
use crate::panels::SideTab;
use crate::{StudioConnectionStatus, StudioShell, StudioUiState};

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

fn output(value: u128) -> WireOutputId {
    WireOutputId::new(NonZeroU128::new(value).unwrap())
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
        frame_rate: FrameRate::new(25, 1).unwrap(),
        playlist_follows: true,
        grade_states: 0,
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
    model
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
        ("overlays", SideTab::Overlays),
        ("stingers", SideTab::Stingers),
        ("titles", SideTab::Titles),
        ("playlist", SideTab::Playlist),
    ];
    for (operator, role) in [(true, "operator"), (false, "viewer")] {
        for (tab_name, tab) in tabs {
            scenes.push(Scene::console(
                format!("{role}/{tab_name}"),
                shell(&|shell| shell.side_tab = tab),
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
            shell.side_tab = SideTab::Playlist;
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
        shell(&|shell| shell.side_tab = SideTab::Playlist),
        ready(
            true,
            Variant {
                black: true,
                playing: true,
                ..Variant::default()
            },
        ),
    ));
    scenes.push(Scene::console("operator/notices", shell(&|_| {}), {
        let mut state = ready(true, Variant::default());
        state.notice = Some("saved".into());
        state.error = Some("refused".into());
        state.recording_pending = [true, false];
        state
    }));
    let operator = ready(true, Variant::default());
    let dialogs: [(&str, Configure); 6] = [
        ("shortcuts", &|shell| shell.show_shortcuts = true),
        ("about", &|shell| shell.show_about = true),
        ("preferences", &|shell| shell.show_preferences = true),
        ("add-input", &|shell| shell.open_add_input(&operator)),
        ("remove-input", &|shell| {
            shell.open_remove_input(input(4), "Guest");
        }),
        ("rename", &|shell| {
            shell.rename_draft = Some(crate::RenameDraft::new(input(2), "Camera 2"));
        }),
    ];
    for (name, open) in dialogs {
        scenes.push(Scene::console(
            format!("dialog/{name}"),
            shell(open),
            operator.clone(),
        ));
    }
    scenes.push(Scene::console(
        "disconnected",
        shell(&|_| {}),
        StudioUiState::new(StudioConnectionStatus::Backoff),
    ));
    scenes
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
fn check(language: Language) {
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

#[test]
fn every_interactive_widget_explains_itself_in_english() {
    check(Language::English);
}

#[test]
fn every_interactive_widget_explains_itself_in_spanish() {
    check(Language::Spanish);
}

/// Audits the standalone animation timeline in `language` under the same
/// rules, and requires every transport shortcut to be named on its button.
fn check_timeline(language: Language) {
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

#[test]
fn every_animation_timeline_widget_explains_itself_in_english() {
    check_timeline(Language::English);
}

#[test]
fn every_animation_timeline_widget_explains_itself_in_spanish() {
    check_timeline(Language::Spanish);
}

/// Audits the standalone colour panel in `language` under the same rules,
/// and requires every action shortcut to be named on its key.
fn check_color_panel(language: Language) {
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
fn every_color_panel_widget_explains_itself_in_english() {
    check_color_panel(Language::English);
}

#[test]
fn every_color_panel_widget_explains_itself_in_spanish() {
    check_color_panel(Language::Spanish);
}

#[test]
fn every_shipped_language_is_audited() {
    // A new catalog language needs its own audit test above.
    assert_eq!(Language::ALL, [Language::English, Language::Spanish]);
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
