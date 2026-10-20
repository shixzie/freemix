//! IN-006 media lists on the wire: the `media_list` command (create, set,
//! navigate, advance), the `media_list_changed` event, the snapshot's
//! `media_list_states` count, and the `media_list_state` follow-on record —
//! exactly, and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_MEDIA_LIST_ITEMS,
    MediaListItemSpec, MediaListNavigation, MediaListOperation, MediaListOptionsSpec,
    MediaListStateMessage, MediaListStatus, WireInputId, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn prefix() -> String {
    format!("command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid=a\tidempotency_key=k")
}

fn command(input_id: u128, operation: MediaListOperation) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::MediaList {
            input: input(input_id),
            operation,
        },
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn items() -> Vec<MediaListItemSpec> {
    vec![
        MediaListItemSpec {
            id: 1,
            uri: "asset://media/intro clip.mp4".to_owned(),
            title: Some("Intro, ~cold~".to_owned()),
            mark_in: 12,
            mark_out: Some(250),
        },
        MediaListItemSpec {
            id: 4,
            uri: "/home/show/bed.wav".to_owned(),
            title: Some("-".to_owned()),
            mark_in: 0,
            mark_out: None,
        },
    ]
}

fn options() -> MediaListOptionsSpec {
    MediaListOptionsSpec {
        auto_next: true,
        auto_first: false,
        loop_list: true,
        shuffle_seed: Some(u64::MAX),
    }
}

fn status() -> MediaListStatus {
    MediaListStatus {
        input: input(7),
        items: items(),
        options: options(),
        active: 4,
        cycle: 3,
        order: vec![1, 4],
        position: 1,
    }
}

const ITEMS: &str = "items=1%7Easset%253A%252F%252Fmedia%252Fintro%2520clip.mp4%7EIntro%252C%2520%257Ecold%257E%7E12%7E250%2C4%7E%252Fhome%252Fshow%252Fbed.wav%7E%252D%7E0%7E-";

#[test]
fn media_list_commands_round_trip_exactly() {
    let create = round_trip(&command(
        7,
        MediaListOperation::Create {
            name: "Walk-in".to_owned(),
            items: items(),
            options: options(),
        },
    ));
    assert_eq!(
        create,
        format!(
            "{}\tpayload=media_list\tinput=7\toperation=create\tname=Walk-in\t{ITEMS}\tauto_next=1\tauto_first=0\tloop=1\tshuffle_seed=18446744073709551615\n",
            prefix()
        )
    );
    round_trip(&command(
        7,
        MediaListOperation::Set {
            items: items(),
            options: MediaListOptionsSpec::default(),
            active: None,
        },
    ));
    for navigation in [
        MediaListNavigation::Next,
        MediaListNavigation::Previous,
        MediaListNavigation::First,
        MediaListNavigation::Last,
        MediaListNavigation::Goto(0),
        MediaListNavigation::Item(4),
    ] {
        let line = round_trip(&command(7, MediaListOperation::Navigate(navigation)));
        assert!(
            line.contains(&format!("\tnavigation={}", navigation.token())),
            "{line}"
        );
        assert_eq!(
            line.contains("\tindex="),
            navigation.index().is_some(),
            "{line}"
        );
    }
    assert_eq!(
        round_trip(&command(7, MediaListOperation::Advance { from: 4 })),
        format!(
            "{}\tpayload=media_list\tinput=7\toperation=advance\tfrom=4\n",
            prefix()
        )
    );
}

#[test]
fn malformed_media_list_commands_are_refused() {
    let base = format!(
        "{}\tpayload=media_list\tinput=7\toperation=navigate\tnavigation=next\n",
        prefix()
    );
    assert!(decode_line(&base).is_ok());
    for (from, to) in [
        ("navigation=next", "navigation=forward"),
        ("navigation=next", "navigation=goto"),
        ("navigation=next", "navigation=next\tindex=2"),
        ("operation=navigate", "operation=jump"),
        ("input=7", "input=07"),
        ("\tnavigation=next", ""),
    ] {
        let line = base.replacen(from, to, 1);
        assert!(decode_line(&line).is_err(), "{line}");
    }
    let advance = format!(
        "{}\tpayload=media_list\tinput=7\toperation=advance\tfrom=0\n",
        prefix()
    );
    assert!(decode_line(&advance).is_err());

    let create = encode_line(&command(
        7,
        MediaListOperation::Create {
            name: "Walk-in".to_owned(),
            items: items(),
            options: options(),
        },
    ))
    .unwrap();
    for (from, to) in [
        ("items=1%7E", "items=0%7E"),
        ("%2C4%7E", "%2C1%7E"),
        ("%7E12%7E250", "%7E12%7E12"),
        ("%7E12%7E250", "%7E12%7E250%7E9"),
        ("\tauto_first=0", ""),
        ("loop=1", "loop=yes"),
        (
            "shuffle_seed=18446744073709551615",
            "shuffle_seed=18446744073709551616",
        ),
        ("name=Walk-in", "name=%20"),
        ("\tname=Walk-in", ""),
    ] {
        let line = create.replacen(from, to, 1);
        assert_ne!(line, create, "{from}");
        assert!(decode_line(&line).is_err(), "{from} -> {to}");
    }

    let too_many = (1..=u32::try_from(MAX_MEDIA_LIST_ITEMS).unwrap() + 1)
        .map(|id| MediaListItemSpec {
            id,
            uri: "asset://a.mp4".to_owned(),
            title: None,
            mark_in: 0,
            mark_out: None,
        })
        .collect();
    assert!(matches!(
        encode_line(&command(
            7,
            MediaListOperation::Set {
                items: too_many,
                options: options(),
                active: None,
            }
        )),
        Err(CodecError::TooManyItems("items"))
    ));
    assert!(
        encode_line(&command(
            7,
            MediaListOperation::Set {
                items: Vec::new(),
                options: options(),
                active: None,
            }
        ))
        .is_err()
    );
}

#[test]
fn the_state_record_and_event_carry_the_whole_list() {
    let state = WireMessage::MediaListState(MediaListStateMessage {
        revision: 9,
        status: status(),
    });
    assert_eq!(
        round_trip(&state),
        format!(
            "media_list_state\trevision=9\tinput=7\tactive=4\tcycle=3\tposition=1\tauto_next=1\tauto_first=0\tloop=1\tshuffle_seed=18446744073709551615\t{ITEMS}\torder=1%2C4\n"
        )
    );
    let event = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload: EventPayload::MediaListChanged {
            frame: 40,
            operation: "create",
            created: Some("Walk-in".to_owned()),
            status: Box::new(status()),
        },
        edit_history: EditHistoryStatus::EMPTY,
    });
    let line = round_trip(&event);
    assert!(line.contains("\tevent=media_list_changed\tframe=40\toperation=create\tcreated=Walk-in\tinput=7\tactive=4"), "{line}");
    let navigate = WireMessage::Event(EventMessage {
        payload: EventPayload::MediaListChanged {
            frame: 41,
            operation: "navigate",
            created: None,
            status: Box::new(status()),
        },
        ..match &event {
            WireMessage::Event(message) => message.clone(),
            _ => unreachable!(),
        }
    });
    assert!(round_trip(&navigate).contains("\tcreated=-\t"));

    // The play order must be a permutation of the items with the active
    // item at its position.
    for (from, to) in [
        ("order=1%2C4", "order=4%2C1"),
        ("order=1%2C4", "order=1"),
        ("order=1%2C4", "order=1%2C4%2C4"),
        ("active=4", "active=1"),
        ("position=1", "position=2"),
        ("\tcycle=3", ""),
    ] {
        let line = encode_line(&state).unwrap().replacen(from, to, 1);
        assert!(decode_line(&line).is_err(), "{from} -> {to}");
    }
    let mut unordered = status();
    unordered.order = vec![4, 1];
    assert!(
        encode_line(&WireMessage::MediaListState(MediaListStateMessage {
            revision: 9,
            status: unordered,
        }))
        .is_err()
    );
}

/// A `set` carries an optional `active` item (Protocol 2.79), one of its
/// items: an undone edit that dropped the active item is journaled with it,
/// so a replay restores which item was active.
#[test]
fn set_carries_an_optional_active_item_exactly() {
    let set = |active| {
        command(
            7,
            MediaListOperation::Set {
                items: items(),
                options: options(),
                active,
            },
        )
    };
    let without = round_trip(&set(None));
    assert!(!without.contains("\tactive="), "{without}");
    let line = round_trip(&set(Some(4)));
    assert!(line.ends_with("\tactive=4\n"), "{line}");
    for bad in ["active=2", "active=04", "active=0", "active=", "active=x"] {
        let line = line.replacen("active=4", bad, 1);
        assert!(decode_line(&line).is_err(), "{line}");
    }
    // The encoder refuses what the decoder refuses.
    assert!(encode_line(&set(Some(2))).is_err());
    assert!(encode_line(&set(Some(0))).is_err());
    // Only a set carries it.
    let create = round_trip(&command(
        7,
        MediaListOperation::Create {
            name: "Walk-in".to_owned(),
            items: items(),
            options: options(),
        },
    ));
    assert!(decode_line(&create.replacen('\n', "\tactive=4\n", 1)).is_err());
}
