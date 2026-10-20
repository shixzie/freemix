//! IN-006 through the control service: creating and editing a media list is
//! a Graphics project edit, stepping through it an Operator control; events,
//! snapshots, and `media_list_state` records carry the whole list; an item
//! that ends under Auto Next is queued for the authority, which journals an
//! `advance` from it.

use fm_protocol::{
    MediaListItemSpec, MediaListNavigation, MediaListOperation, MediaListOptionsSpec,
    MediaListStatus, MediaOperation,
};

use super::*;

const LIST: u128 = 9;

fn list() -> WireInputId {
    WireInputId::from_domain(input(LIST))
}

fn items() -> Vec<MediaListItemSpec> {
    vec![
        MediaListItemSpec {
            id: 1,
            uri: "asset://media/a.mp4".to_owned(),
            title: Some("Opener".to_owned()),
            mark_in: 10,
            mark_out: Some(15),
        },
        MediaListItemSpec {
            id: 2,
            uri: "asset://media/b.wav".to_owned(),
            title: None,
            mark_in: 0,
            mark_out: Some(5),
        },
    ]
}

fn payload(operation: MediaListOperation) -> CommandPayload {
    CommandPayload::MediaList {
        input: list(),
        operation,
    }
}

/// Opens a synthetic source on every media input lacking one, as the
/// simulated daemon does.
fn attach(control: &mut ControlService) {
    let inputs: Vec<InputId> = control
        .engine
        .show()
        .media_inputs()
        .keys()
        .copied()
        .collect();
    for media in inputs {
        if !control.engine.media_source_open(media) {
            let settings = *control.engine.show().media_settings(media).unwrap();
            let rate = control.engine.frame_rate();
            control
                .engine
                .set_media_source(
                    media,
                    Some(fm_engine::EngineMediaSource::synthetic(&settings, rate)),
                )
                .unwrap();
        }
    }
}

fn submit(
    control: &mut ControlService,
    role: Role,
    key: &str,
    payload: CommandPayload,
) -> CommandSubmission {
    let submission = control
        .submit(&principal(role), command(key, key, payload), 0)
        .unwrap();
    attach(control);
    submission
}

fn changed(submission: &CommandSubmission) -> (u64, &'static str, Option<String>, MediaListStatus) {
    let events = &submission.output.events;
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0].payload {
        EventPayload::MediaListChanged {
            frame,
            operation,
            created,
            status,
        } => (*frame, *operation, created.clone(), (**status).clone()),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn media_lists_are_authorized_replicated_and_auto_advanced_exactly() {
    let mut control = service(64, 256);
    let create = payload(MediaListOperation::Create {
        name: "Walk-in".to_owned(),
        items: items(),
        options: MediaListOptionsSpec {
            auto_next: true,
            ..MediaListOptionsSpec::default()
        },
    });
    assert_eq!(
        control.required_classes(&create).primary(),
        CommandClass::EditProject
    );
    let next = payload(MediaListOperation::Navigate(MediaListNavigation::Next));
    assert_eq!(
        control.required_classes(&next).primary(),
        CommandClass::Transition
    );
    let denied = submit(&mut control, Role::Operator, "denied", create.clone());
    assert_eq!(rejection_code(&denied), Some("permission_denied"));

    let created = submit(&mut control, Role::Graphics, "create", create);
    assert!(created.is_accepted(), "{:?}", created.output.result);
    assert_wire_exact(&control, &created);
    let (_, operation, name, status) = changed(&created);
    assert_eq!((operation, name.as_deref()), ("create", Some("Walk-in")));
    assert_eq!((status.active, status.order.clone()), (1, vec![1, 2]));
    assert_eq!(status.items, items());
    assert_eq!(control.snapshot().snapshot.media_list_states, 1);
    let records = control.media_list_states();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, status);
    assert!(
        control
            .snapshot()
            .snapshot
            .inputs
            .iter()
            .any(|entry| entry.input == list() && entry.name == "Walk-in")
    );

    // Play the opener; when it reaches its out mark the end is queued for
    // the authority, naming the item that ended.
    let play = submit(
        &mut control,
        Role::Operator,
        "play",
        CommandPayload::Media {
            input: list(),
            operation: MediaOperation::Play,
        },
    );
    assert!(play.is_accepted(), "{:?}", play.output.result);
    let mut ends = Vec::new();
    for _ in 0..20 {
        control.tick(&server_identity()).unwrap();
        ends.extend(control.take_media_list_ends());
        if !ends.is_empty() {
            break;
        }
    }
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!((ends[0].input, ends[0].item), (input(LIST), 1));

    let advanced = submit(
        &mut control,
        Role::Operator,
        "advance",
        payload(MediaListOperation::Advance { from: 1 }),
    );
    assert!(advanced.is_accepted(), "{:?}", advanced.output.result);
    assert_wire_exact(&control, &advanced);
    let (_, operation, name, status) = changed(&advanced);
    assert_eq!((operation, name, status.active), ("advance", None, 2));
    assert_eq!(
        control.engine.show().input_source(input(LIST)),
        Some(&fm_model::InputSourceSpec::Media {
            asset_uri: "asset://media/b.wav".to_owned()
        })
    );
    let stale = submit(
        &mut control,
        Role::Operator,
        "stale",
        payload(MediaListOperation::Advance { from: 1 }),
    );
    assert_eq!(rejection_code(&stale), Some("conflict"));
    let CommandResult::Rejected { fields, .. } = &stale.output.result else {
        unreachable!()
    };
    assert_eq!(fields[0].code, "stale_advance");

    // Going back is an operator control; editing the list a project edit.
    let previous = submit(
        &mut control,
        Role::Operator,
        "previous",
        payload(MediaListOperation::Navigate(MediaListNavigation::Previous)),
    );
    assert!(previous.is_accepted(), "{:?}", previous.output.result);
    assert_eq!(changed(&previous).3.active, 1);
    let mut reordered = items();
    reordered.reverse();
    let set = submit(
        &mut control,
        Role::Graphics,
        "set",
        payload(MediaListOperation::Set {
            items: reordered.clone(),
            options: MediaListOptionsSpec::default(),
            active: None,
        }),
    );
    assert!(set.is_accepted(), "{:?}", set.output.result);
    let (_, operation, _, status) = changed(&set);
    assert_eq!((operation, status.order), ("set", vec![2, 1]));
    assert_eq!(status.items, reordered);

    // Undoing the edit restores the list; undoing the creation removes the
    // input and its list.
    let undo = submit(
        &mut control,
        Role::Graphics,
        "undo-set",
        CommandPayload::Undo {
            expected_edit: None,
        },
    );
    assert!(undo.is_accepted(), "{:?}", undo.output.result);
    assert_eq!(changed(&undo).3.items, items());
    assert_eq!(control.media_list_states()[0].status.items, items());
}

/// A list whose files and titles are written in a non-Latin script: every
/// byte is escaped twice on the wire (once in the item, once in the line).
fn escaped_items(count: u32, stem_chars: usize, title_chars: usize) -> Vec<MediaListItemSpec> {
    (0..count)
        .map(|index| MediaListItemSpec {
            id: fm_protocol::MAX_MEDIA_LIST_ITEM_ID - index,
            uri: format!("asset://media/{index}{}.mp4", "動".repeat(stem_chars)),
            title: (title_chars > 0).then(|| "題".repeat(title_chars)),
            mark_in: fm_model::MAX_MEDIA_FRAME - 1,
            mark_out: Some(fm_model::MAX_MEDIA_FRAME),
        })
        .collect()
}

#[test]
fn every_accepted_list_fits_its_wire_records_and_one_that_cannot_is_refused() {
    // 100 items of 40-character Japanese names hold under 16 KiB of text
    // but escape to far more than one protocol line.
    let mut control = service(64, 256);
    let oversized = escaped_items(100, 40, 0);
    let text: usize = oversized.iter().map(|item| item.uri.len()).sum();
    assert!(text <= fm_protocol::MAX_MEDIA_LIST_TEXT_BYTES, "{text}");
    let refused = submit(
        &mut control,
        Role::Graphics,
        "oversized",
        payload(MediaListOperation::Create {
            name: "Walk-in".to_owned(),
            items: oversized,
            options: MediaListOptionsSpec::default(),
        }),
    );
    let CommandResult::Rejected { fields, .. } = &refused.output.result else {
        panic!(
            "accepted a list whose state record cannot be encoded: {:?}",
            control
                .media_list_states()
                .into_iter()
                .map(|state| fm_protocol::encode_line(&WireMessage::MediaListState(state)))
                .collect::<Vec<_>>()
        );
    };
    assert_eq!(fields[0].code, "invalid_items");

    // The largest list the model accepts, with the longest name, ids, and
    // marks, still encodes as its event and its state record.
    let largest = escaped_items(256, 3, 5);
    let wire: usize = largest
        .iter()
        .map(|item| {
            fm_model::media_list_wire_text_bytes(&item.uri)
                + fm_model::media_list_wire_text_bytes(item.title.as_deref().unwrap_or_default())
        })
        .sum();
    assert!(
        (fm_model::MAX_MEDIA_LIST_WIRE_TEXT_BYTES - 1024
            ..=fm_model::MAX_MEDIA_LIST_WIRE_TEXT_BYTES)
            .contains(&wire),
        "{wire}"
    );
    let created = submit(
        &mut control,
        Role::Graphics,
        "largest",
        payload(MediaListOperation::Create {
            name: "é".repeat(fm_types::MAX_INPUT_NAME_BYTES / 2),
            items: largest,
            options: MediaListOptionsSpec {
                shuffle_seed: Some(u64::MAX),
                ..MediaListOptionsSpec::default()
            },
        }),
    );
    assert!(created.is_accepted(), "{:?}", created.output.result);
    assert_wire_exact(&control, &created);
    for state in control.media_list_states() {
        let message = WireMessage::MediaListState(state);
        let line = fm_protocol::encode_line(&message).unwrap();
        assert_eq!(fm_protocol::decode_line(&line).unwrap(), message);
    }
}

/// A list on item 2 whose `set` then dropped item 2 (cueing item 1).
fn dropped_active() -> ControlService {
    let mut control = service(64, 256);
    let steps = [
        payload(MediaListOperation::Create {
            name: "Walk-in".to_owned(),
            items: items(),
            options: MediaListOptionsSpec::default(),
        }),
        payload(MediaListOperation::Navigate(MediaListNavigation::Next)),
        payload(MediaListOperation::Set {
            items: items()[..1].to_vec(),
            options: MediaListOptionsSpec::default(),
            active: None,
        }),
    ];
    for (index, step) in steps.into_iter().enumerate() {
        let submission = submit(&mut control, Role::Admin, &format!("step-{index}"), step);
        assert!(submission.is_accepted(), "{:?}", submission.output.result);
        control.tick(&server_identity()).unwrap();
    }
    assert_eq!(control.media_list_states()[0].status.active, 1);
    control
}

/// An undone `set` that dropped the active item is journaled with that
/// item as `active` (Protocol 2.77): the journal line, decoded and replayed
/// on a restored authority, restores which item was active.
#[test]
fn an_undone_set_is_journaled_with_the_item_it_dropped_as_active() {
    let mut control = dropped_active();
    let message = undo("undo-set", None);
    let staged = prepared(
        control
            .prepare_submit(&principal(Role::Admin), message.clone(), 0)
            .unwrap(),
    );
    let journaled = staged.replay_command(&message);
    assert_eq!(
        journaled.payload,
        payload(MediaListOperation::Set {
            items: items(),
            options: MediaListOptionsSpec::default(),
            active: Some(2),
        })
    );
    let undone = staged.commit().unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(changed(&undone).3.active, 2);

    let line = fm_protocol::encode_line(&WireMessage::Command(journaled.clone())).unwrap();
    assert!(line.contains("\tactive=2"), "{line}");
    let WireMessage::Command(decoded) = fm_protocol::decode_line(&line).unwrap() else {
        panic!("a journal line is a command");
    };
    assert_eq!(decoded, journaled);
    let mut replayed = dropped_active();
    let submission = replayed
        .submit(&principal(Role::Admin), decoded, 0)
        .unwrap();
    assert!(submission.is_accepted(), "{:?}", submission.output.result);
    assert_eq!(
        replayed.media_list_states()[0].status,
        control.media_list_states()[0].status
    );
    assert_eq!(replayed.media_list_states()[0].status.active, 2);
}
