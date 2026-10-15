use core::num::NonZeroU128;

use fm_animation::{
    AnimationPreset, Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value,
};
use fm_protocol::{TitlePreviewSample, WireInputId};
use fm_types::FrameRate;

use super::*;
use crate::animation_editor::{
    DragStart, Modifiers, Pointer, PreviewCompletion, RetimeHandle, TargetBounds, TargetKind,
};

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
