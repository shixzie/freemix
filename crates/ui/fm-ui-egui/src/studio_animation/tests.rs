use core::num::NonZeroU128;

use fm_animation::{AnimationPreset, Easing, Keyframe, Property, Timebase, Track, TrackKey, Value};
use fm_protocol::{
    EngineIdentity, FadeToBlackPosition, FadeToBlackState, InputStatus, ManualTransitionStatus,
    OverlayStatus, RecordingStatus, SnapshotMessage, TitleAnimationStateMessage, TitleFieldStatus,
    TitleFieldValue, TitleStateMessage, TitleSummary, WireRecorderSlot, WireTitleClip,
    WireTitleFieldId,
};
use fm_types::{FrameRate, ProjectId};
use fm_ui_model::animation_editor::{TargetBounds, TargetKind, TargetOutline};
use fm_ui_model::{ClientModel, ProjectSnapshot};

use super::*;
use crate::StudioConnectionStatus;
use crate::animation_timeline::PresetRequest;

const TITLE: u128 = 6;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

fn element(value: u128) -> WireTitleElementId {
    WireTitleElementId::new(NonZeroU128::new(value).unwrap())
}

/// Element 1 (the bar) fades in over 20 frames; Out is 10 frames.
fn clip() -> WireTitleClip {
    WireTitleClip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        20,
        10,
        vec![Track::new(
            TrackKey::new(element(1), Phase::In, Property::Opacity),
            vec![
                Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                Keyframe::new(20, Value::Scalar(255), Easing::Linear),
            ],
        )],
    )
    .unwrap()
}

fn design() -> TitleDesign {
    TitleDesign {
        canvas: (1_920, 1_080),
        elements: vec![
            TargetOutline::new(
                element(1),
                "Bar",
                TargetKind::Shape,
                TargetBounds::new(120, 840, 900, 140),
            ),
            TargetOutline::new(
                element(2),
                "Name",
                TargetKind::Text,
                TargetBounds::new(160, 860, 700, 60),
            ),
        ],
    }
}

/// Camera 1 and title input 6 (a lower third) whose animation is at
/// `animation_revision`; `edit` grants `EditProject`, `locked` engages the
/// project padlock.
#[allow(clippy::too_many_lines)] // one literal snapshot
fn ready(edit: bool, locked: bool, animation_revision: u64) -> StudioUiState {
    let project = ProjectId::new(NonZeroU128::new(1).unwrap());
    let live = FadeToBlackState {
        target_active: false,
        position: FadeToBlackPosition::LIVE,
    };
    let strip = |value| fm_protocol::InputAudioStripStatus {
        pan_basis_points: 0,
        group: None,
        input: wire(value),
        gain_millidb: 0,
        balance_basis_points: 0,
        muted: false,
        soloed: false,
        follow_video: true,
        delay_samples: 0,
    };
    let snapshot = SnapshotMessage {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        capture_inputs: Vec::new(),
        engine: EngineIdentity {
            engine_id: "e".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 10,
        show_name: "Show".into(),
        inputs: vec![
            InputStatus {
                input: wire(1),
                name: "Cam 1".into(),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            },
            InputStatus {
                input: wire(TITLE),
                name: "Lower third".into(),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            },
        ],
        outputs: Vec::new(),
        input_audio_strips: vec![strip(1), strip(TITLE)],
        desired_program: wire(1),
        desired_preview: wire(1),
        realized_program: wire(1),
        realized_preview: wire(1),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: live,
        realized_fade_to_black: live,
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: [1, 2]
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: false,
            })
            .to_vec(),
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks: fm_protocol::EditLockStatus {
            project: locked,
            inputs: Vec::new(),
        },
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        titles: vec![TitleSummary {
            input: wire(TITLE),
            field_revision: 0,
        }],
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: FrameRate::new(25, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        cue_list_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
    };
    let mut model = ClientModel::new(project);
    model
        .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
        .unwrap();
    model
        .apply_title_state(TitleStateMessage {
            revision: 10,
            input: wire(TITLE),
            name: "Lower third".into(),
            field_revision: 0,
            phase: fm_protocol::TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            fields: vec![TitleFieldStatus {
                field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                name: "Name".into(),
                value: TitleFieldValue::Text("Ana".into()),
            }],
        })
        .unwrap();
    model
        .apply_title_animation_state(TitleAnimationStateMessage {
            revision: 10,
            input: wire(TITLE),
            animation_revision,
            clip: Box::new(clip()),
        })
        .unwrap();
    let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
        .with_view(model.view().unwrap())
        .with_switcher_permissions(true, true)
        .with_edit_project_permission(edit);
    state.project_state = model.state().cloned();
    state
}

fn draw_at(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<StudioIntent> {
    context.begin_pass(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(1600.0, 1000.0),
        )),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    });
    let mut ui = Ui::new(
        context.clone(),
        egui::Id::new("fm-ui-egui-animation-test"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let intents = shell.draw(&mut ui, state);
    let _ = context.end_pass();
    intents
}

fn draw(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
) -> Vec<StudioIntent> {
    draw_at(context, shell, state, 0.0, Vec::new())
}

fn saves(intents: &[StudioIntent]) -> Vec<&CommandPayload> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            StudioIntent::TitleAnimation(payload) => Some(payload),
            _ => None,
        })
        .collect()
}

fn shortcut() -> egui::Event {
    egui::Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
    }
}

#[test]
fn the_shortcut_opens_and_closes_the_window_on_a_title() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let state = ready(true, false, 4);
    draw(&context, &mut shell, &state);
    assert!(!shell.animation_open());
    draw_at(&context, &mut shell, &state, 0.1, vec![shortcut()]);
    assert!(shell.animation_open());
    // With no title chosen, the window shows the first one.
    assert_eq!(shell.animation_title(), Some(input(TITLE)));
    assert!(shell.animation_binding(input(TITLE)).is_some());
    draw_at(&context, &mut shell, &state, 0.2, vec![shortcut()]);
    assert!(!shell.animation_open());
}

#[test]
fn edits_save_on_the_animation_revision_unless_locked() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let state = ready(true, false, 4);
    shell.open_animation_window(input(TITLE));
    assert_eq!(
        saves(&draw(&context, &mut shell, &state)),
        Vec::<&CommandPayload>::new()
    );
    let binding = shell.animation_binding_mut(input(TITLE)).unwrap();
    assert_eq!(
        binding.document().clip(),
        &clip(),
        "starts from the saved animation"
    );
    binding.document_mut().extend_phase(Phase::In, 30).unwrap();
    let intents = draw(&context, &mut shell, &state);
    let [
        CommandPayload::SetTitleAnimation {
            input: sent,
            base_revision,
            clip: draft,
        },
    ] = saves(&intents)[..]
    else {
        panic!("expected one title_animation_set: {intents:?}");
    };
    assert_eq!(
        (*sent, *base_revision, draft.in_frames()),
        (wire(TITLE), 4, 30)
    );
    assert_eq!(
        shell.animation_binding(input(TITLE)).unwrap().status(),
        AnimationSaveStatus::Saving
    );

    // A padlock holds a new edit back and explains itself in the timeline.
    let locked = ready(true, true, 4);
    shell.abandon_animation_requests();
    assert_eq!(saves(&draw(&context, &mut shell, &locked)).len(), 0);
    assert_eq!(
        shell.animation.timeline.lock_reason,
        Some(PROJECT_LOCKED_REASON)
    );
    let viewer = ready(false, false, 4);
    assert_eq!(
        animation_lock_reason(&viewer, input(TITLE)),
        Some(Msg::ReasonNoEditProject.en())
    );
    // Unlocked again, the held draft is sent.
    assert_eq!(saves(&draw(&context, &mut shell, &state)).len(), 1);
}

#[test]
fn a_conflict_is_resolved_by_reloading_theirs_or_overwriting() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let state = ready(true, false, 4);
    shell.open_animation_window(input(TITLE));
    draw(&context, &mut shell, &state);
    let binding = shell.animation_binding_mut(input(TITLE)).unwrap();
    binding.document_mut().extend_phase(Phase::In, 30).unwrap();
    assert_eq!(saves(&draw(&context, &mut shell, &state)).len(), 1);

    // The save is refused as stale: the window shows the conflict.
    let mut refused = ready(true, false, 4);
    refused.title_animation_refusal = Some(TitleAnimationRefusal {
        command_id: "c1".into(),
        input: input(TITLE),
        conflict: true,
    });
    assert_eq!(saves(&draw(&context, &mut shell, &refused)).len(), 0);
    let binding = shell.animation_binding_mut(input(TITLE)).unwrap();
    assert_eq!(binding.status(), AnimationSaveStatus::Conflict);
    assert_eq!(binding.document().clip().in_frames(), 30, "draft kept");

    // Their newer animation arrives; Overwrite saves ours on top of it.
    let mut theirs = ready(true, false, 7);
    theirs.title_animation_refusal = refused.title_animation_refusal.clone();
    draw(&context, &mut shell, &theirs);
    shell
        .animation_binding_mut(input(TITLE))
        .unwrap()
        .overwrite();
    let intents = draw(&context, &mut shell, &theirs);
    assert!(matches!(
        saves(&intents)[..],
        [CommandPayload::SetTitleAnimation {
            base_revision: 7,
            ..
        }]
    ));

    // Reload theirs drops the draft for the saved animation.
    let binding = shell.animation_binding_mut(input(TITLE)).unwrap();
    binding.refused(AnimationRefusal::Conflict);
    binding.reload_theirs();
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);
    assert_eq!(binding.document().clip(), &clip());
}

#[test]
fn exact_previews_are_requested_and_shown_on_the_canvas() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let mut state = ready(true, false, 4);
    shell.open_animation_window(input(TITLE));
    // Without a raw TCP session no preview is asked for.
    for time in [0.0, 1.0] {
        let intents = draw_at(&context, &mut shell, &state, time, Vec::new());
        assert!(
            !intents
                .iter()
                .any(|intent| matches!(intent, StudioIntent::TitlePreview(_)))
        );
    }
    state.title_previews = true;
    let mut request = None;
    for time in [2.0, 2.5, 3.0] {
        for intent in draw_at(&context, &mut shell, &state, time, Vec::new()) {
            if let StudioIntent::TitlePreview(sent) = intent {
                request = Some(sent);
            }
        }
    }
    let request = request.expect("a preview is requested");
    assert_eq!(request.input, wire(TITLE));
    assert!(
        request.clip.is_none(),
        "a clean draft previews the saved animation"
    );
    assert!(!shell.animation_preview_shown(input(TITLE)));

    let (width, height) = (request.width, request.height);
    state.title_preview = Some(TitlePreviewImage {
        request_id: request.request_id.clone(),
        input: wire(TITLE),
        animation_revision: 4,
        draft: false,
        width,
        height,
        rgba: vec![200; usize::from(width) * usize::from(height) * 4].into(),
    });
    draw_at(&context, &mut shell, &state, 3.1, Vec::new());
    assert!(shell.animation_preview_shown(input(TITLE)));
    assert!(
        shell
            .animation_binding(input(TITLE))
            .unwrap()
            .exact_preview_current()
    );
}

#[test]
fn elements_without_a_design_take_presets_from_the_server() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let state = ready(true, false, 4);
    shell.open_animation_window(input(TITLE));
    draw(&context, &mut shell, &state);
    assert_eq!(shell.animation.timeline.outside_presets, Some(Ok(())));
    let pick = PresetRequest {
        target: element(1),
        phase: Phase::In,
        preset: fm_ui_model::animation_editor::PresetSource::BuiltIn(
            AnimationPreset::SlideFromLeft,
        ),
        frames: 20,
    };
    shell.animation.timeline.preset_requests.push(pick.clone());
    let intents = draw(&context, &mut shell, &state);
    assert_eq!(
        saves(&intents),
        vec![&CommandPayload::ApplyTitleAnimationPreset {
            input: wire(TITLE),
            base_revision: 4,
            element: element(1),
            phase: Phase::In,
            preset: AnimationPreset::SlideFromLeft,
            frames: 20,
        }]
    );
    // One at a time: the next waits for the new revision.
    shell.animation.timeline.preset_requests.push(pick);
    assert_eq!(saves(&draw(&context, &mut shell, &state)).len(), 0);
    assert_eq!(
        shell.animation.timeline.outside_presets,
        Some(Err(Msg::AnimReasonSaving.en()))
    );
    draw(&context, &mut shell, &ready(true, false, 5));
    assert_eq!(shell.animation.timeline.outside_presets, Some(Ok(())));
    // Unsaved local edits must be saved or reverted first.
    shell
        .animation_binding_mut(input(TITLE))
        .unwrap()
        .set_auto_save(false);
    shell
        .animation_binding_mut(input(TITLE))
        .unwrap()
        .document_mut()
        .extend_phase(Phase::In, 40)
        .unwrap();
    draw(&context, &mut shell, &ready(true, false, 5));
    assert_eq!(
        shell.animation.timeline.outside_presets,
        Some(Err(Msg::AnimReasonServerPreset.en()))
    );

    // A known design gives every element local presets and outlines.
    shell.set_title_design(input(TITLE), design());
    draw(&context, &mut shell, &ready(true, false, 5));
    assert_eq!(shell.animation.timeline.outside_presets, None);
    let binding = shell.animation_binding(input(TITLE)).unwrap();
    assert!(binding.has_design());
    assert_eq!(binding.document().targets().len(), 2);
}

fn pointer(at: egui::Pos2, pressed: Option<bool>) -> Vec<egui::Event> {
    let mut events = vec![egui::Event::PointerMoved(at)];
    if let Some(pressed) = pressed {
        events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    events
}

fn previews(intents: &[StudioIntent]) -> Vec<fm_protocol::TitlePreviewRequest> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            StudioIntent::TitlePreview(request) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // one gesture: open, rest, preview, apply
fn resting_on_a_preset_previews_it_on_the_canvas_before_it_is_applied() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let mut state = ready(true, false, 4);
    state.title_previews = true;
    shell.open_animation_window(input(TITLE));
    shell.set_title_design(input(TITLE), design());
    let mut time = 0.0;
    let mut frame = |shell: &mut StudioShell, state: &StudioUiState, events| {
        time += 0.05;
        draw_at(&context, shell, state, time, events)
    };
    frame(&mut shell, &state, Vec::new());
    frame(&mut shell, &state, Vec::new());
    // The unanimated "Name" (row 2) is listed and gets a preset picker.
    let label = shell.animation.tracks.unwrap().label_rect(1);
    let picker = pos2(label.right() - 50.0, label.center().y);
    frame(&mut shell, &state, pointer(picker, Some(true)));
    frame(&mut shell, &state, pointer(picker, Some(false)));
    frame(&mut shell, &state, Vec::new());
    let mut rest = None;
    for step in 1..120_u16 {
        let at = pos2(picker.x, picker.y + 4.0 * f32::from(step));
        frame(&mut shell, &state, pointer(at, None));
        frame(&mut shell, &state, Vec::new());
        if shell
            .animation_binding(input(TITLE))
            .unwrap()
            .preset_hover()
            .is_some()
        {
            rest = Some(at);
            break;
        }
    }
    let at = rest.expect("the pointer rests on a preset");
    let binding = shell.animation_binding(input(TITLE)).unwrap();
    let hover = binding.preset_hover().unwrap();
    assert_eq!(hover.target, element(2));
    // Previewed, not applied: the draft and its history are untouched.
    assert_eq!(binding.document().undo_depth(), 0);
    assert!(!binding.document().is_dirty());
    let hovered = binding.preset_hover_document().unwrap().clip().clone();
    assert!(
        hovered
            .tracks()
            .iter()
            .any(|track| track.key.target == element(2))
    );

    // While the pointer rests, the exact preview of the hovered draft is
    // asked for (once the earlier draft preview is answered or given up);
    // nothing is saved.
    let mut request = None;
    for _ in 0..120 {
        let intents = frame(&mut shell, &state, Vec::new());
        assert_eq!(saves(&intents).len(), 0);
        if let Some(sent) = previews(&intents)
            .into_iter()
            .find(|sent| sent.clip.as_deref() == Some(&hovered))
        {
            request = Some(sent);
            break;
        }
    }
    let request = request.expect("the hovered preset is previewed exactly");
    assert_eq!(
        request.sample,
        fm_protocol::TitlePreviewSample::In {
            frame: hovered.in_frames() / 2
        }
    );
    state.title_preview = Some(TitlePreviewImage {
        request_id: request.request_id.clone(),
        input: wire(TITLE),
        animation_revision: 4,
        draft: true,
        width: request.width,
        height: request.height,
        rgba: vec![90; usize::from(request.width) * usize::from(request.height) * 4].into(),
    });
    frame(&mut shell, &state, Vec::new());
    let binding = shell.animation_binding(input(TITLE)).unwrap();
    assert!(binding.hover_preview_current());
    assert!(shell.animation_preview_shown(input(TITLE)));

    // A click applies exactly the previewed preset as one undo step and
    // saves it.
    frame(&mut shell, &state, pointer(at, Some(true)));
    let intents = frame(&mut shell, &state, pointer(at, Some(false)));
    let binding = shell.animation_binding(input(TITLE)).unwrap();
    assert_eq!(binding.preset_hover(), None);
    assert_eq!(binding.document().undo_depth(), 1);
    assert_eq!(binding.document().clip(), &hovered);
    let mut saved = saves(&intents).len();
    if saved == 0 {
        saved = saves(&frame(&mut shell, &state, Vec::new())).len();
    }
    assert_eq!(saved, 1, "auto-save sends the applied preset");
}

#[test]
fn the_window_docks_and_remembers_its_sizes_in_the_layout() {
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let state = ready(true, false, 4);
    shell.open_animation_window(input(TITLE));
    shell.set_title_design(input(TITLE), design());
    draw(&context, &mut shell, &state);
    assert!(!shell.animation_docked());
    // Docked, it draws as a panel along the bottom of the console.
    let mut layout = shell.layout().clone();
    layout.animation_docked = true;
    layout.animation_dock_height = Some(360);
    shell.set_layout(layout);
    draw(&context, &mut shell, &state);
    draw(&context, &mut shell, &state);
    assert!(shell.animation_docked());
    let panel = egui::PanelState::load(&context, egui::Id::new(DOCK_ID)).unwrap();
    assert!((panel.size().y - 360.0).abs() < 1.0, "{:?}", panel.size());
    assert!(
        (panel.outer_rect.bottom() - 1000.0).abs() < 30.0,
        "{:?}",
        panel.outer_rect
    );
    // Both choices persist as preferences.
    let entries = shell.layout().preference_entries();
    assert!(entries.contains(&("layout.animation", "docked".to_owned())));
    assert!(entries.contains(&("layout.animation_dock_height", "360".to_owned())));
    let mut read = crate::ConsoleLayout::default();
    assert!(read.apply_preference("layout.animation_size", "900x700"));
    assert_eq!(read.animation_size, Some((900, 700)));
    assert!(!read.apply_preference("layout.animation_size", "100x700"));
    assert!(!read.apply_preference("layout.animation_size", "900"));
    assert!(!read.apply_preference("layout.animation", "sideways"));
    assert!(!read.apply_preference("layout.animation_dock_height", "100"));
}

/// GX-011 stage 5: scene layer animation in the same window.
mod scenes {
    use core::num::NonZeroU32;

    use fm_protocol::{
        SceneAnimationStateMessage, SceneLayoutStateMessage, WireSceneClip, WireSceneLayerId,
        WireSceneLayerOutline, WireSceneLayout,
    };

    use super::*;

    const SCENE: u128 = 7;

    /// The scene the Scene input shows (Protocol 2.35).
    fn wire_scene() -> fm_protocol::WireSceneId {
        fm_protocol::WireSceneId::new(NonZeroU128::new(70).unwrap())
    }

    fn layer(value: u32) -> WireSceneLayerId {
        WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
    }

    /// Camera 1, Scene input 7 (layers "Host" and "Guest") whose animation
    /// is at revision 3, and with `title` also title input 6; `locked`
    /// engages the project padlock.
    #[allow(clippy::too_many_lines)] // one fixture: snapshot and its records
    fn scene_ready(locked: bool, title: bool) -> StudioUiState {
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let live = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        let strip = |value| fm_protocol::InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input: wire(value),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        };
        let mut inputs = vec![(1, "Cam 1")];
        if title {
            inputs.push((TITLE, "Lower third"));
        }
        inputs.push((SCENE, "Two up"));
        let snapshot = SnapshotMessage {
            audio_mixer: fm_protocol::AudioMixerStatus::default(),
            fade_to_black_excluded_outputs: Vec::new(),
            fade_to_black_output_durations: Vec::new(),
            fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
            display_outputs: Vec::new(),
            display_output_states: Vec::new(),
            network_streams: Vec::new(),
            capture_inputs: Vec::new(),
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 10,
            show_name: "Show".into(),
            inputs: inputs
                .iter()
                .map(|(value, name)| InputStatus {
                    input: wire(*value),
                    name: (*name).into(),
                    lifecycle: fm_protocol::InputLifecycleStatus::default(),
                })
                .collect(),
            outputs: Vec::new(),
            input_audio_strips: inputs.iter().map(|(value, _)| strip(*value)).collect(),
            desired_program: wire(1),
            desired_preview: wire(1),
            realized_program: wire(1),
            realized_preview: wire(1),
            desired_manual_transition: ManualTransitionStatus::Inactive,
            realized_manual_transition: ManualTransitionStatus::Inactive,
            desired_fade_to_black: live,
            realized_fade_to_black: live,
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| RecordingStatus {
                    recorder: WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus {
                project: locked,
                inputs: Vec::new(),
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: if title {
                vec![TitleSummary {
                    input: wire(TITLE),
                    field_revision: 0,
                }]
            } else {
                Vec::new()
            },
            scenes: vec![wire(SCENE)],
            layout_scenes: vec![wire_scene()],
            frame_rate: FrameRate::new(25, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            cue_list_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        };
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        if title {
            model
                .apply_title_animation_state(TitleAnimationStateMessage {
                    revision: 10,
                    input: wire(TITLE),
                    animation_revision: 4,
                    clip: Box::new(clip()),
                })
                .unwrap();
        }
        model
            .apply_scene_animation_state(SceneAnimationStateMessage {
                revision: 10,
                input: wire(SCENE),
                scene: wire_scene(),
                animation_revision: 3,
                clip: Box::new(
                    WireSceneClip::new(Timebase::new(FrameRate::new(30, 1).unwrap()), 0, 0)
                        .unwrap(),
                ),
            })
            .unwrap();
        let outline = |id: u32, name: &str, x: i32| WireSceneLayerOutline {
            id: layer(id),
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
            source: fm_protocol::WireSceneLayerSource::Input(wire(1)),
        };
        model
            .apply_scene_layout_state(SceneLayoutStateMessage {
                revision: 10,
                scene: wire_scene(),
                name: "Two up".into(),
                layout: Box::new(WireSceneLayout {
                    width: 1_920,
                    height: 1_080,
                    layers: vec![outline(1, "Host", 0), outline(2, "Guest", 960)],
                }),
            })
            .unwrap();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(model.view().unwrap())
            .with_switcher_permissions(true, true)
            .with_edit_project_permission(true);
        state.project_state = model.state().cloned();
        state
    }

    fn scene_saves(intents: &[StudioIntent]) -> Vec<&CommandPayload> {
        intents
            .iter()
            .filter_map(|intent| match intent {
                StudioIntent::SceneAnimation(payload) => Some(payload),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn without_titles_the_window_opens_on_a_scene_with_its_layers() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = scene_ready(false, false);
        draw(&context, &mut shell, &state);
        draw_at(&context, &mut shell, &state, 0.1, vec![shortcut()]);
        assert!(shell.animation_open());
        assert_eq!(shell.animation_title(), None);
        assert_eq!(shell.animation_scene(), Some(input(SCENE)));
        draw_at(&context, &mut shell, &state, 0.2, Vec::new());
        let binding = shell.scene_animation_binding(input(SCENE)).unwrap();
        assert!(binding.has_design());
        let names: Vec<&str> = binding
            .document()
            .targets()
            .iter()
            .map(|outline| outline.name.as_str())
            .collect();
        assert_eq!(names, ["Host", "Guest"]);
        assert_eq!(binding.document().canvas(), (1_920, 1_080));
        // Layers have no colour to animate.
        assert_eq!(
            shell.animation.scene_timeline.properties,
            &SCENE_LAYER_PROPERTIES
        );
    }

    #[test]
    fn scene_edits_save_as_scene_animation_set_unless_locked() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = scene_ready(false, false);
        shell.open_scene_animation_window(input(SCENE));
        assert_eq!(
            scene_saves(&draw(&context, &mut shell, &state)),
            Vec::<&CommandPayload>::new()
        );
        let binding = shell.scene_animation_binding_mut(input(SCENE)).unwrap();
        binding
            .document_mut()
            .apply_preset(layer(2), Phase::In, AnimationPreset::SlideFromRight, 15)
            .unwrap();
        let clip = binding.document().clip().clone();
        let intents = draw(&context, &mut shell, &state);
        assert_eq!(
            scene_saves(&intents),
            vec![&CommandPayload::SetSceneAnimation {
                input: wire(SCENE),
                base_revision: 3,
                clip: Box::new(clip),
            }]
        );
        assert!(
            intents.iter().all(|intent| !matches!(
                intent,
                StudioIntent::TitlePreview(_) | StudioIntent::ScenePreview(_)
            )),
            "without a raw TCP session no exact preview is asked for"
        );

        let mut shell = StudioShell::default();
        let locked = scene_ready(true, false);
        shell.open_scene_animation_window(input(SCENE));
        draw(&context, &mut shell, &locked);
        shell
            .scene_animation_binding_mut(input(SCENE))
            .unwrap()
            .document_mut()
            .apply_preset(layer(1), Phase::Out, AnimationPreset::Fade, 10)
            .unwrap();
        assert_eq!(
            scene_saves(&draw(&context, &mut shell, &locked)),
            Vec::<&CommandPayload>::new()
        );
        assert_eq!(
            animation_lock_reason(&locked, input(SCENE)),
            Some(PROJECT_LOCKED_REASON)
        );
    }

    #[test]
    fn scenes_are_previewed_exactly_on_the_canvas() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let mut state = scene_ready(false, false);
        state.title_previews = true;
        shell.open_scene_animation_window(input(SCENE));
        let mut request = None;
        for time in [0.0, 0.5, 1.0] {
            for intent in draw_at(&context, &mut shell, &state, time, Vec::new()) {
                assert!(!matches!(intent, StudioIntent::TitlePreview(_)));
                if let StudioIntent::ScenePreview(sent) = intent {
                    request = Some(sent);
                }
            }
        }
        let request = request.expect("a scene preview is requested");
        assert_eq!(request.input, wire(SCENE));
        assert_eq!((request.width, request.height), (640, 360));
        assert!(
            request.clip.is_none(),
            "a clean draft previews the saved animation"
        );
        assert!(!shell.animation_preview_shown(input(SCENE)));

        // A refusal frees the scheduler for the next request.
        let mut refused = state.clone();
        refused.title_preview_failed = Some(request.request_id.clone());
        draw_at(&context, &mut shell, &refused, 1.1, Vec::new());
        assert!(
            !shell
                .scene_animation_binding(input(SCENE))
                .unwrap()
                .awaits_preview(&request.request_id)
        );
        let mut request = None;
        for time in [1.5, 2.0] {
            for intent in draw_at(&context, &mut shell, &refused, time, Vec::new()) {
                if let StudioIntent::ScenePreview(sent) = intent {
                    request = Some(sent);
                }
            }
        }
        let request = request.expect("the scene preview is asked for again");

        refused.title_preview = Some(TitlePreviewImage {
            request_id: request.request_id.clone(),
            input: wire(SCENE),
            animation_revision: 3,
            draft: false,
            width: request.width,
            height: request.height,
            rgba: vec![90; usize::from(request.width) * usize::from(request.height) * 4].into(),
        });
        draw_at(&context, &mut shell, &refused, 2.1, Vec::new());
        assert!(shell.animation_preview_shown(input(SCENE)));
        assert!(
            shell
                .scene_animation_binding(input(SCENE))
                .unwrap()
                .exact_preview_current()
        );
    }

    #[test]
    fn titles_and_scenes_share_the_window() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = scene_ready(false, true);
        // A title is shown first; a scene can be picked instead.
        draw_at(&context, &mut shell, &state, 0.1, vec![shortcut()]);
        assert_eq!(shell.animation_title(), Some(input(TITLE)));
        assert_eq!(shell.animation_scene(), None);
        shell.open_scene_animation_window(input(SCENE));
        draw(&context, &mut shell, &state);
        assert_eq!(shell.animation_scene(), Some(input(SCENE)));
        assert!(shell.scene_animation_binding(input(SCENE)).is_some());
        // Opening a title switches back.
        shell.open_animation_window(input(TITLE));
        draw(&context, &mut shell, &state);
        assert_eq!(shell.animation_scene(), None);
        assert_eq!(shell.animation_title(), Some(input(TITLE)));
    }
}
