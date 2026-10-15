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
fn ready(edit: bool, locked: bool, animation_revision: u64) -> StudioUiState {
    let project = ProjectId::new(NonZeroU128::new(1).unwrap());
    let live = FadeToBlackState {
        target_active: false,
        position: FadeToBlackPosition::LIVE,
    };
    let strip = |value| fm_protocol::InputAudioStripStatus {
        input: wire(value),
        gain_millidb: 0,
        balance_basis_points: 0,
        muted: false,
        soloed: false,
        follow_video: true,
        delay_samples: 0,
    };
    let snapshot = SnapshotMessage {
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
            },
            InputStatus {
                input: wire(TITLE),
                name: "Lower third".into(),
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
        frame_rate: FrameRate::new(25, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
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
        preset: AnimationPreset::SlideFromLeft,
        frames: 20,
    };
    shell.animation.timeline.preset_requests.push(pick);
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
