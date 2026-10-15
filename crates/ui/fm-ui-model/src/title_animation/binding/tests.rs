use core::num::NonZeroU128;

use fm_animation::{
    AnimationPreset, Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value,
};
use fm_protocol::{TitlePreviewSample, WireInputId, WireTitleClip};
use fm_types::FrameRate;

use super::*;
use crate::animation_editor::{
    DragStart, Modifiers, Pointer, PresetSource, PreviewCompletion, RetimeHandle, TargetBounds,
    TargetKind,
};
use crate::title_animation::TitleAnimationStatus;

const MS: fn(u64) -> Duration = Duration::from_millis;

fn input() -> InputId {
    InputId::new(NonZeroU128::new(8).unwrap())
}

fn element(value: u128) -> WireTitleElementId {
    WireTitleElementId::new(NonZeroU128::new(value).unwrap())
}

/// A clip whose In phase fades element 1 in over `in_frames`.
fn clip(in_frames: u32) -> WireTitleClip {
    WireTitleClip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        in_frames,
        10,
        vec![Track::new(
            TrackKey::new(element(1), Phase::In, Property::Opacity),
            vec![
                Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                Keyframe::new(in_frames, Value::Scalar(255), Easing::Linear),
            ],
        )],
    )
    .unwrap()
}

fn status(revision: u64, clip: WireTitleClip) -> TitleAnimationStatus {
    TitleAnimationStatus {
        revision,
        clip: Arc::new(clip),
    }
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

fn saved_clip(payload: &CommandPayload) -> (u64, WireTitleClip) {
    let CommandPayload::SetTitleAnimation {
        input: wire,
        base_revision,
        clip,
    } = payload
    else {
        panic!("expected title_animation_set, got {payload:?}");
    };
    assert_eq!(*wire, WireInputId::from_domain(input()));
    (*base_revision, (**clip).clone())
}

#[test]
fn edits_save_with_their_base_revision_and_the_echo_marks_them_saved() {
    let mut binding = TitleAnimationBinding::new(input(), &status(4, clip(10)));
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);
    assert_eq!(binding.poll_save(true), None, "a clean draft sends nothing");

    // Another editor's change reaches the clean draft.
    assert_eq!(binding.sync(&status(5, clip(12))), AuthoritySync::Reset);
    assert_eq!(binding.document().base_revision(), 5);

    binding.document_mut().extend_phase(Phase::In, 20).unwrap();
    assert_eq!(binding.status(), AnimationSaveStatus::Unsaved);
    // A padlock or missing permission holds the save back.
    assert_eq!(binding.poll_save(false), None);
    let payload = binding.poll_save(true).expect("auto-save sends the edit");
    let (base, sent) = saved_clip(&payload);
    assert_eq!(base, 5);
    assert_eq!(&sent, binding.document().clip());
    assert_eq!(binding.status(), AnimationSaveStatus::Saving);
    assert_eq!(binding.poll_save(true), None, "one save in flight");

    // The authority reports this save: the draft is clean, history kept.
    assert_eq!(binding.sync(&status(6, sent)), AuthoritySync::Saved);
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);
    assert_eq!(binding.document().base_revision(), 6);
    assert!(binding.document().can_undo());
    assert_eq!(binding.sync(&status(6, clip(20))), AuthoritySync::Unchanged);
}

#[test]
fn edits_made_while_a_save_travels_build_on_it() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    binding.document_mut().extend_phase(Phase::In, 20).unwrap();
    let (_, first) = saved_clip(&binding.poll_save(true).unwrap());
    binding.document_mut().extend_phase(Phase::In, 30).unwrap();
    // Our first save comes back while the draft moved on: not a conflict.
    assert_eq!(binding.sync(&status(2, first)), AuthoritySync::Saved);
    assert_eq!(binding.status(), AnimationSaveStatus::Unsaved);
    let (base, second) = saved_clip(&binding.poll_save(true).unwrap());
    assert_eq!(base, 2, "the next save names the revision of the first");
    assert_eq!(second.in_frames(), 30);
}

#[test]
fn a_change_under_a_dirty_draft_is_a_conflict_until_reloaded_or_overwritten() {
    let mut binding = TitleAnimationBinding::new(input(), &status(3, clip(10)));
    binding.document_mut().extend_phase(Phase::In, 20).unwrap();
    binding.set_auto_save(false);
    assert_eq!(binding.poll_save(true), None, "auto-save off holds it");

    assert_eq!(binding.sync(&status(4, clip(40))), AuthoritySync::Conflict);
    assert_eq!(binding.status(), AnimationSaveStatus::Conflict);
    binding.request_save();
    assert_eq!(binding.poll_save(true), None, "a conflict is never sent");
    assert_eq!(binding.document().clip().in_frames(), 20, "draft kept");

    // Overwrite re-bases the draft onto their revision and sends it.
    binding.overwrite();
    let (base, sent) = saved_clip(&binding.poll_save(true).unwrap());
    assert_eq!((base, sent.in_frames()), (4, 20));
    // The server still refuses it as a conflict (someone saved again).
    binding.refused(AnimationRefusal::Conflict);
    assert_eq!(binding.status(), AnimationSaveStatus::Conflict);
    assert_eq!(binding.sync(&status(5, clip(50))), AuthoritySync::Conflict);

    // Reload theirs drops the draft for the authority's animation.
    binding.reload_theirs();
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);
    assert_eq!(binding.document().clip(), &clip(50));
    assert_eq!(binding.document().base_revision(), 5);
    assert!(!binding.document().can_undo());
}

#[test]
fn refusals_wait_for_an_edit_or_an_explicit_save() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    binding.document_mut().extend_phase(Phase::In, 20).unwrap();
    assert!(binding.poll_save(true).is_some());
    binding.refused(AnimationRefusal::Other);
    assert_eq!(binding.status(), AnimationSaveStatus::Refused);
    assert_eq!(binding.poll_save(true), None, "no resend loop");
    binding.request_save();
    assert!(binding.poll_save(true).is_some(), "Save retries");
    binding.refused(AnimationRefusal::Other);
    binding.document_mut().extend_phase(Phase::In, 25).unwrap();
    assert_eq!(binding.status(), AnimationSaveStatus::Unsaved);
    assert!(binding.poll_save(true).is_some(), "a new edit retries");
    // A lost answer leaves the draft dirty for the next poll.
    binding.abandon_in_flight();
    assert!(binding.poll_save(true).is_some());
}

#[test]
fn drags_save_on_release_and_defer_authority_updates() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    let document = binding.document_mut();
    document
        .begin_drag(
            DragStart::Retime(RetimeHandle::PhaseEnd),
            Pointer::at(10),
            Modifiers::NONE,
        )
        .unwrap();
    document
        .update_drag(Pointer::at(20), Modifiers::SHIFT)
        .unwrap();
    assert_eq!(binding.poll_save(true), None, "nothing leaves mid-drag");
    assert_eq!(
        binding.status(),
        AnimationSaveStatus::Unsaved,
        "the drag moved a key"
    );
    // Another editor's change waits until the drag ends.
    assert_eq!(binding.sync(&status(2, clip(10))), AuthoritySync::Unchanged);
    assert!(binding.document().drag().is_some());
    binding.document_mut().commit_drag().unwrap();
    assert_eq!(binding.sync(&status(2, clip(10))), AuthoritySync::Conflict);
    binding.overwrite();
    let (base, sent) = saved_clip(&binding.poll_save(true).unwrap());
    assert_eq!((base, sent.in_frames()), (2, 20));
}

#[test]
fn undoing_back_to_the_saved_animation_sends_nothing() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    binding.document_mut().extend_phase(Phase::In, 20).unwrap();
    binding.document_mut().undo().unwrap();
    assert!(binding.document().is_dirty());
    assert_eq!(binding.poll_save(true), None);
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);
}

#[test]
fn designs_place_presets_on_the_template_canvas() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    assert!(!binding.has_design());
    assert_eq!(binding.document().canvas(), DEFAULT_CANVAS);
    binding.set_design(&TitleDesign {
        canvas: (1_280, 720),
        ..design()
    });
    assert!(binding.has_design());
    assert_eq!(binding.document().canvas(), (1_280, 720));
    binding
        .document_mut()
        .apply_preset(element(2), Phase::In, AnimationPreset::SlideFromLeft, 10)
        .unwrap();
    let (_, sent) = saved_clip(&binding.poll_save(true).unwrap());
    assert!(
        sent.tracks()
            .iter()
            .any(|track| track.key == TrackKey::new(element(2), Phase::In, Property::PositionX))
    );
}

#[test]
fn previews_are_debounced_paced_and_matched_to_their_answers() {
    let mut binding = TitleAnimationBinding::new(input(), &status(1, clip(10)));
    binding.set_design(&design());
    binding.document_mut().set_playhead(5);
    binding.want_preview(MS(0));
    assert_eq!(binding.poll_preview(MS(50), true), None, "debounced");
    assert_eq!(binding.next_preview_deadline(), Some(MS(120)));
    assert_eq!(binding.poll_preview(MS(120), false), None, "not permitted");
    let first = binding.poll_preview(MS(120), true).unwrap();
    assert_eq!(first.sample, TitlePreviewSample::In { frame: 5 });
    assert_eq!((first.width, first.height), (640, 360));
    assert_eq!(
        first.clip, None,
        "a clean draft previews the saved animation"
    );
    assert!(binding.awaits_preview(&first.request_id));

    // Scrubbing on: the newest frame waits for the answer in flight.
    binding.document_mut().set_playhead(9);
    binding.want_preview(MS(130));
    assert_eq!(binding.poll_preview(MS(400), true), None, "one in flight");
    assert_eq!(binding.receive_preview("anim-preview-99"), None);
    assert_eq!(
        binding.receive_preview(&first.request_id),
        Some(PreviewCompletion::Superseded)
    );
    assert!(!binding.exact_preview_current());
    let second = binding.poll_preview(MS(400), true).unwrap();
    assert_eq!(second.sample, TitlePreviewSample::In { frame: 9 });
    assert_eq!(
        binding.receive_preview(&second.request_id),
        Some(PreviewCompletion::Current)
    );
    assert!(binding.exact_preview_current());

    // A dirty draft travels with its preview; a quick follow-up is paced.
    binding.document_mut().extend_phase(Phase::In, 30).unwrap();
    binding.want_preview(MS(400));
    assert!(binding.poll_preview(MS(520), true).unwrap().clip.is_some());
    binding.preview_failed("anim-preview-3");
    binding.document_mut().set_playhead(3);
    binding.want_preview(MS(521));
    assert_eq!(binding.poll_preview(MS(600), true), None, "paced");
    let fourth = binding.poll_preview(MS(641), true).unwrap();

    // A lost answer is given up after the timeout.
    assert!(binding.awaits_preview(&fourth.request_id));
    binding.document_mut().set_playhead(4);
    binding.want_preview(MS(700));
    assert_eq!(binding.poll_preview(MS(2_000), true), None);
    assert!(binding.poll_preview(MS(2_641), true).is_some());
    assert!(!binding.awaits_preview(&fourth.request_id));

    binding.reset_previews();
    assert_eq!(binding.shown_preview(), None);
}

#[test]
fn preview_sizes_keep_the_canvas_shape_within_the_protocol_cap() {
    assert_eq!(preview_size((1_920, 1_080)), (640, 360));
    assert_eq!(preview_size((1_080, 1_920)), (203, 360));
    assert_eq!(preview_size((800, 800)), (360, 360));
    assert_eq!(preview_size((320, 180)), (640, 360));
    assert_eq!(preview_size((10_000, 1)), (640, 1));
}

#[test]
fn replicated_layouts_become_designs() {
    use fm_protocol::{WireTitleElementKind, WireTitleElementOutline, WireTitleLayout};

    let outline = |id: u128, name: &str, kind, visible| WireTitleElementOutline {
        id: element(id),
        name: name.into(),
        kind,
        x: -10,
        y: 20,
        width: 300,
        height: 40,
        opacity: 180,
        fill: fm_animation::Rgba::new(1, 2, 3, 4),
        visible,
    };
    let layout = WireTitleLayout {
        width: 1_280,
        height: 720,
        elements: vec![
            outline(4, "Bar", WireTitleElementKind::Rectangle, true),
            outline(2, "Name", WireTitleElementKind::Text, true),
            outline(9, "Logo", WireTitleElementKind::Image, false),
            outline(5, "Clock", WireTitleElementKind::ProductionClock, true),
            outline(6, "", WireTitleElementKind::Ticker, true),
        ],
    };
    let design = TitleDesign::from_layout(&layout).unwrap();
    assert_eq!(design.canvas, (1_280, 720));
    let summary: Vec<_> = design
        .elements
        .iter()
        .map(|outline| {
            (
                outline.target,
                outline.name.as_str(),
                outline.kind,
                outline.opacity,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (element(4), "Bar", TargetKind::Shape, 180),
            (element(2), "Name", TargetKind::Text, 180),
            // Hidden elements keep their outline at zero opacity.
            (element(9), "Logo", TargetKind::Image, 0),
            (element(5), "Clock", TargetKind::Text, 180),
            (element(6), "", TargetKind::Text, 180),
        ]
    );
    let bar = &design.elements[0];
    assert_eq!(bar.bounds, TargetBounds::new(-10, 20, 300, 40));
    assert_eq!(bar.color, fm_animation::Rgba::new(1, 2, 3, 4));
    assert_eq!(bar.rotation_mdeg, 0);
    // A title without a template has no design.
    assert_eq!(TitleDesign::from_layout(&WireTitleLayout::default()), None);
}

#[test]
fn hovered_presets_preview_without_applying_and_apply_as_one_step() {
    let mut binding = TitleAnimationBinding::new(input(), &status(4, clip(20)));
    binding.set_design(&design());
    let hover = PresetHover {
        target: element(2),
        phase: Phase::In,
        preset: PresetSource::BuiltIn(AnimationPreset::SlideFromLeft),
        frames: 20,
    };
    binding.set_preset_hover(Some(hover.clone()), MS(1_000));
    assert_eq!(binding.preset_hover(), Some(hover.clone()));
    // The draft is untouched; the hovered draft moves the unanimated name.
    assert!(!binding.document().is_dirty());
    assert_eq!(binding.document().clip(), &clip(20));
    let hovered = binding.preset_hover_document().unwrap();
    assert!(
        hovered
            .clip()
            .tracks()
            .iter()
            .any(|track| track.key.target == element(2))
    );
    assert_eq!(
        hovered.playhead(),
        10,
        "the exact preview samples mid-phase"
    );
    let hovered_clip = hovered.clip().clone();

    // The exact preview waits for the hover to rest, then carries the
    // hovered draft at the middle of the phase.
    binding.want_preview(MS(1_100));
    assert_eq!(binding.poll_preview(MS(1_400), true), None);
    binding.want_preview(MS(1_300));
    let request = binding
        .poll_preview(MS(1_500), true)
        .expect("the rested hover is previewed");
    assert_eq!(request.sample, TitlePreviewSample::In { frame: 10 });
    assert_eq!(request.clip.as_deref(), Some(&hovered_clip));
    assert!(!binding.hover_preview_current());
    assert!(binding.receive_preview(&request.request_id).is_some());
    assert!(binding.hover_preview_current());
    assert!(binding.shown_preview_is_hover());
    assert!(!binding.exact_preview_current(), "not the draft's frame");
    // Repeating the hover keeps it; nothing is saved meanwhile.
    binding.set_preset_hover(Some(hover.clone()), MS(1_600));
    assert!(binding.hover_preview_current());
    assert_eq!(binding.poll_save(true), None);

    // Leaving the picker ends the preview; the draft's own preview follows.
    binding.set_preset_hover(None, MS(1_700));
    assert!(binding.preset_hover_document().is_none());
    binding.want_preview(MS(1_700));
    let request = binding.poll_preview(MS(2_000), true).unwrap();
    assert_eq!(request.clip, None, "the clean draft previews the saved one");

    // Applying the preset is one undo step that saves exactly the clip
    // the hover previewed.
    binding
        .document_mut()
        .apply_preset(element(2), Phase::In, AnimationPreset::SlideFromLeft, 20)
        .unwrap();
    assert_eq!(binding.document().undo_depth(), 1);
    assert_eq!(binding.document().clip(), &hovered_clip);
    let (base, saved) = saved_clip(&binding.poll_save(true).unwrap());
    assert_eq!((base, saved), (4, hovered_clip));
    binding.document_mut().undo().unwrap();
    assert_eq!(binding.document().clip(), &clip(20));

    // Elements without an outline preview nothing.
    let mut unoutlined = TitleAnimationBinding::new(input(), &status(4, clip(20)));
    unoutlined.set_preset_hover(Some(hover), MS(0));
    assert_eq!(unoutlined.preset_hover(), None);
}

/// GX-011 user presets (Protocol 2.34): an element's In phase saved as a
/// motion previews and applies on another element like a built-in, retimed
/// to the requested length; the hover never touches the draft.
#[test]
fn user_presets_capture_preview_and_apply_retimed() {
    let mut binding = TitleAnimationBinding::new(input(), &status(4, clip(20)));
    binding.set_design(&design());
    let motion = binding
        .document()
        .capture_motion(element(1), Phase::In)
        .expect("element 1 fades in");
    assert_eq!(motion.frames(), 20);
    assert!(motion.properties().eq([Property::Opacity]));
    // Nothing to save on an element that does not move.
    assert!(
        binding
            .document()
            .capture_motion(element(2), Phase::In)
            .is_err()
    );

    let source = PresetSource::User {
        preset: core::num::NonZeroU32::new(1).unwrap(),
        name: "Fade up".to_owned(),
        motion: std::sync::Arc::new(motion),
    };
    let hover = PresetHover {
        target: element(2),
        phase: Phase::In,
        preset: source.clone(),
        frames: 10,
    };
    binding.set_preset_hover(Some(hover.clone()), MS(0));
    assert_eq!(binding.preset_hover(), Some(hover));
    assert!(!binding.document().is_dirty());
    let hovered = binding.preset_hover_document().unwrap().clip().clone();
    let track = hovered
        .track(TrackKey::new(element(2), Phase::In, Property::Opacity))
        .expect("the hovered draft fades element 2 in");
    assert_eq!(track.keys.last().unwrap().frame, 10, "retimed to 10 frames");

    binding.set_preset_hover(None, MS(10));
    binding
        .document_mut()
        .apply_preset_source(element(2), Phase::In, &source, 10, &Property::ALL)
        .unwrap();
    assert_eq!(binding.document().undo_depth(), 1);
    assert_eq!(binding.document().clip(), &hovered);
    // The phase keeps its length; element 1's own fade is untouched.
    assert_eq!(binding.document().clip().phase_frames(Phase::In), 20);
    assert_eq!(
        binding
            .document()
            .clip()
            .track(TrackKey::new(element(1), Phase::In, Property::Opacity)),
        clip(20).track(TrackKey::new(element(1), Phase::In, Property::Opacity))
    );
}

/// GX-011 stage 5: a scene editor is the same binding keyed by layer: its
/// design comes from the replicated `scene_layout_state` (a quarter turn
/// swaps the footprint), a local preset measures the same footprint the
/// authority does, and a save is a revision-checked `scene_animation_set`.
#[test]
fn scene_bindings_design_from_layouts_and_save_scene_clips() {
    use core::num::NonZeroU32;

    use fm_protocol::{WireSceneClip, WireSceneLayerId, WireSceneLayerOutline, WireSceneLayout};

    use crate::title_animation::{SCENE_LAYER_PROPERTIES, SceneAnimationStatus};

    let layer = |value: u32| WireSceneLayerId::new(NonZeroU32::new(value).unwrap());
    let outline = |id: u32, quarter_turns: u8, enabled: bool| WireSceneLayerOutline {
        id: layer(id),
        name: format!("Layer {id}"),
        x: 100,
        y: 40,
        width: 640,
        height: 360,
        quarter_turns,
        opacity: 200,
        enabled,
        source: fm_protocol::WireSceneLayerSource::Input(fm_protocol::WireInputId::new(
            core::num::NonZeroU128::new(1).unwrap(),
        )),
    };
    let design = SceneDesign::from_layout(&WireSceneLayout {
        width: 1_920,
        height: 1_080,
        layers: vec![outline(3, 1, true), outline(4, 2, false)],
    });
    assert_eq!(design.canvas, (1_920, 1_080));
    assert_eq!(
        design.elements[0].bounds,
        TargetBounds::new(100, 40, 360, 640)
    );
    assert_eq!(design.elements[0].kind, TargetKind::Video);
    assert_eq!(design.elements[0].opacity, 200);
    assert_eq!(
        design.elements[1].bounds,
        TargetBounds::new(100, 40, 640, 360)
    );
    assert_eq!(design.elements[1].opacity, 0);
    assert!(!SCENE_LAYER_PROPERTIES.contains(&Property::Color));

    let status = SceneAnimationStatus {
        revision: 4,
        clip: Arc::new(
            WireSceneClip::new(Timebase::new(FrameRate::new(30, 1).unwrap()), 0, 0).unwrap(),
        ),
    };
    let mut binding = SceneAnimationBinding::new(input(), &status);
    binding.set_design(&design);
    binding
        .document_mut()
        .apply_preset(layer(3), Phase::In, AnimationPreset::SlideFromLeft, 15)
        .unwrap();
    let Some(CommandPayload::SetSceneAnimation {
        input: saved,
        base_revision,
        clip,
    }) = binding.poll_save(true)
    else {
        panic!("a dirty scene draft saves as scene_animation_set");
    };
    assert_eq!(saved, WireInputId::from_domain(input()));
    assert_eq!(base_revision, 4);
    assert_eq!(&*clip, binding.document().clip());
    assert_eq!(binding.status(), AnimationSaveStatus::Saving);
    // The authority reporting the save back marks the draft saved.
    binding.sync(&SceneAnimationStatus {
        revision: 5,
        clip: Arc::from(clip),
    });
    assert_eq!(binding.status(), AnimationSaveStatus::Saved);

    // Scenes preview exactly (Protocol 2.32): the saved scene, then a
    // dirty draft travelling with its request.
    binding.document_mut().set_playhead(4);
    binding.want_preview(MS(0));
    let saved = binding.poll_preview(MS(120), true).unwrap();
    assert_eq!(saved.input, WireInputId::from_domain(input()));
    assert_eq!(saved.sample, TitlePreviewSample::In { frame: 4 });
    assert_eq!(
        (saved.width, saved.height),
        preview_size(binding.document().canvas())
    );
    assert_eq!(saved.clip, None);
    assert_eq!(
        binding.receive_preview(&saved.request_id),
        Some(PreviewCompletion::Current)
    );
    binding.document_mut().extend_phase(Phase::In, 30).unwrap();
    binding.want_preview(MS(200));
    let draft = binding.poll_preview(MS(400), true).unwrap();
    assert_eq!(draft.clip.as_deref(), Some(binding.document().clip()));
}

/// GX-011 user presets on scene layers: crop keys are pixels of a layer's
/// own (unrotated) frame, so a quarter-turned layer saves and takes a
/// motion against its `width` x `height`, not its swapped footprint, as
/// the authority measures it.
#[test]
fn scene_user_presets_measure_crop_in_the_layer_frame() {
    use core::num::{NonZeroU32, NonZeroU128};

    use fm_animation::PhaseMotion;
    use fm_protocol::{WireSceneClip, WireSceneLayerId, WireSceneLayerOutline, WireSceneLayout};

    use crate::title_animation::{SCENE_LAYER_PROPERTIES, SceneAnimationStatus};

    let layer = |value: u32| WireSceneLayerId::new(NonZeroU32::new(value).unwrap());
    let outline = |id: u32, quarter_turns: u8| WireSceneLayerOutline {
        id: layer(id),
        name: format!("Layer {id}"),
        source: fm_protocol::WireSceneLayerSource::Input(fm_protocol::WireInputId::new(
            NonZeroU128::new(1).unwrap(),
        )),
        x: 0,
        y: 0,
        width: 640,
        height: 360,
        quarter_turns,
        opacity: 255,
        enabled: true,
    };
    let design = SceneDesign::from_layout(&WireSceneLayout {
        width: 1_920,
        height: 1_080,
        layers: vec![outline(3, 1), outline(4, 0)],
    });
    // Layer 3 (turned a quarter) wipes half its own width away.
    let timebase = Timebase::new(FrameRate::new(30, 1).unwrap());
    let wipe = WireSceneClip::from_tracks(
        timebase,
        10,
        0,
        vec![Track::new(
            TrackKey::new(layer(3), Phase::In, Property::CropLeft),
            vec![
                Keyframe::new(0, Value::Scalar(320), Easing::Linear),
                Keyframe::new(10, Value::Scalar(0), Easing::Linear),
            ],
        )],
    )
    .unwrap();
    let status = SceneAnimationStatus {
        revision: 1,
        clip: Arc::new(wipe),
    };
    let mut binding = SceneAnimationBinding::new(input(), &status);
    binding.set_design(&design);
    let motion = binding
        .document()
        .capture_motion(layer(3), Phase::In)
        .expect("layer 3 wipes in");
    assert_eq!((motion.width(), motion.height()), (640, 360));

    // A motion measured on a 640 x 360 frame keeps its crop on the turned
    // 640 x 360 layer.
    let measured = PhaseMotion::new(motion.clip().clone(), 640, 360).unwrap();
    binding
        .document_mut()
        .apply_motion(layer(3), Phase::In, &measured, 10, &SCENE_LAYER_PROPERTIES)
        .unwrap();
    let track = binding
        .document()
        .clip()
        .track(TrackKey::new(layer(3), Phase::In, Property::CropLeft))
        .unwrap();
    assert_eq!(track.keys[0].value, Value::Scalar(320));
}
