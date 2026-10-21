//! IN-006 media lists on the wire: the `media_list` command (create, set,
//! navigate, advance), the `media_list_changed` event, the snapshot's
//! `media_list_states` count, the `media_list_state` follow-on record, and
//! (Protocol 2.91) each item's probed length with the `media_list` runtime
//! event that republishes it, and (Protocol 2.96) the transition between
//! items — exactly, and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_MEDIA_LIST_ITEMS,
    MediaListItemLength, MediaListItemSpec, MediaListNavigation, MediaListOperation,
    MediaListOptionsSpec, MediaListStateMessage, MediaListStatus, MediaListTransitionKind,
    MediaListTransitionSpec, RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity,
    WireInputId, WireMessage, decode_line, encode_line, media_list_status_fits,
};
use fm_types::FrameRate;

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
        transition: MediaListTransitionSpec {
            kind: MediaListTransitionKind::Dip,
            duration_frames: 600,
        },
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
        lengths: vec![
            Some(MediaListItemLength {
                frames: 300,
                rate: FrameRate::new(30_000, 1_001).unwrap(),
            }),
            None,
        ],
    }
}

const LENGTHS: &str = "lengths=300%4030000%2F1001%2C-";

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
            "{}\tpayload=media_list\tinput=7\toperation=create\tname=Walk-in\t{ITEMS}\tauto_next=1\tauto_first=0\tloop=1\tshuffle_seed=18446744073709551615\ttransition=dip\ttransition_frames=600\n",
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
        ("transition=dip", "transition=slide"),
        ("transition=dip", "transition=Dip"),
        ("\ttransition=dip", ""),
        ("transition_frames=600", "transition_frames=601"),
        ("transition_frames=600", "transition_frames=0"),
        ("transition_frames=600", "transition_frames=060"),
        ("\ttransition_frames=600", ""),
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

/// Every transition kind rides a `set` (Protocol 2.96), and the default
/// is a cut that keeps a 15-frame duration for a later blend.
#[test]
fn every_list_transition_round_trips() {
    assert_eq!(
        MediaListOptionsSpec::default().transition,
        MediaListTransitionSpec {
            kind: MediaListTransitionKind::Cut,
            duration_frames: 15,
        }
    );
    for kind in MediaListTransitionKind::ALL {
        assert_eq!(
            MediaListTransitionKind::from_token(kind.token()),
            Some(kind)
        );
        for duration_frames in [1, 15, 600] {
            let mut options = options();
            options.transition = MediaListTransitionSpec {
                kind,
                duration_frames,
            };
            let line = round_trip(&command(
                7,
                MediaListOperation::Set {
                    items: items(),
                    options,
                    active: Some(4),
                },
            ));
            assert!(
                line.contains(&format!(
                    "\ttransition={}\ttransition_frames={duration_frames}\t",
                    kind.token()
                )),
                "{line}"
            );
        }
    }
    // A transition outside its bounds is never written.
    for duration_frames in [0, 601] {
        let mut options = options();
        options.transition.duration_frames = duration_frames;
        assert!(
            encode_line(&command(
                7,
                MediaListOperation::Set {
                    items: items(),
                    options,
                    active: None,
                }
            ))
            .is_err(),
            "{duration_frames}"
        );
    }
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
            "media_list_state\trevision=9\tinput=7\tactive=4\tcycle=3\tposition=1\tauto_next=1\tauto_first=0\tloop=1\tshuffle_seed=18446744073709551615\ttransition=dip\ttransition_frames=600\t{ITEMS}\torder=1%2C4\t{LENGTHS}\n"
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

/// Protocol 2.91: every status carries one probed length per item, in
/// authored order (`-` while unknown), and a `media_list` runtime event
/// republishes the whole status when a length changes. Anything else is
/// refused by both the encoder and the decoder.
#[test]
fn item_lengths_ride_every_status_exactly_and_strictly() {
    let state = WireMessage::MediaListState(MediaListStateMessage {
        revision: 9,
        status: status(),
    });
    let line = round_trip(&state);
    assert!(line.ends_with(&format!("\t{LENGTHS}\n")), "{line}");
    let mut unknown = status();
    unknown.lengths = vec![None, None];
    let line = round_trip(&WireMessage::MediaListState(MediaListStateMessage {
        revision: 9,
        status: unknown,
    }));
    assert!(line.ends_with("\tlengths=-%2C-\n"), "{line}");

    let encoded = encode_line(&state).unwrap();
    for (from, to) in [
        // One per item.
        (LENGTHS, "lengths=-"),
        (LENGTHS, "lengths=-%2C-%2C-"),
        (LENGTHS, "lengths="),
        // Canonical frames within the bounds, a rate in lowest terms.
        (LENGTHS, "lengths=0%4025%2F1%2C-"),
        (LENGTHS, "lengths=0300%4025%2F1%2C-"),
        (LENGTHS, "lengths=4294967296%4025%2F1%2C-"),
        (LENGTHS, "lengths=300%4050%2F2%2C-"),
        (LENGTHS, "lengths=300%4025%2F0%2C-"),
        (LENGTHS, "lengths=300%4025%2C-"),
        (LENGTHS, "lengths=300%2C-"),
        (LENGTHS, "lengths=300%4025%2F1%40x%2C-"),
        (LENGTHS, "lengths=%2C-"),
        (LENGTHS, "lengths=unknown%2C-"),
        // Required.
        (&format!("\t{LENGTHS}"), ""),
    ] {
        let line = encoded.replacen(from, to, 1);
        assert_ne!(line, encoded, "{to}");
        assert!(decode_line(&line).is_err(), "{from} -> {to}");
    }
    // The largest length is the largest media frame.
    let line = encoded.replacen(LENGTHS, "lengths=4294967295%4025%2F1%2C-", 1);
    let WireMessage::MediaListState(decoded) = decode_line(&line).unwrap() else {
        unreachable!()
    };
    assert_eq!(
        decoded.status.item_length(1).unwrap().frames,
        u64::from(u32::MAX)
    );

    // The encoder refuses what the decoder refuses.
    for lengths in [
        vec![None],
        vec![None, None, None],
        vec![
            Some(MediaListItemLength {
                frames: 0,
                rate: FrameRate::new(25, 1).unwrap(),
            }),
            None,
        ],
        vec![
            None,
            Some(MediaListItemLength {
                frames: u64::from(u32::MAX) + 1,
                rate: FrameRate::new(25, 1).unwrap(),
            }),
        ],
    ] {
        let mut status = status();
        status.lengths = lengths;
        assert!(
            encode_line(&WireMessage::MediaListState(MediaListStateMessage {
                revision: 9,
                status,
            }))
            .is_err()
        );
    }

    // The runtime event carries the whole status.
    let runtime = WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "engine-a".to_owned(),
            project_id: "project-a".to_owned(),
            state_epoch: 3,
            log_id: "log-a".to_owned(),
        },
        revision: 12,
        generation: 2,
        sequence: 5,
        event: RuntimeLifecycleEvent::MediaList {
            status: Box::new(status()),
        },
    });
    let line = round_trip(&runtime);
    assert!(
        line.contains("\tevent=media_list\tinput=7\tactive=4\tcycle=3\tposition=1\t"),
        "{line}"
    );
    assert!(line.ends_with(&format!("\t{LENGTHS}\n")), "{line}");
    for (from, to) in [
        (format!("\t{LENGTHS}"), String::new()),
        ("order=1%2C4".to_owned(), "order=4%2C1".to_owned()),
        (LENGTHS.to_owned(), "lengths=-".to_owned()),
    ] {
        let bad = line.replacen(&from, &to, 1);
        assert!(decode_line(&bad).is_err(), "{from} -> {to}");
    }
}

/// The lengths and the list's played duration: an unmarked item plays to
/// its file's end, an out mark past the end stops at the end, and an
/// unknown length leaves the total partial.
#[test]
fn item_lengths_time_the_list() {
    let rate = FrameRate::new(25, 1).unwrap();
    let length = MediaListItemLength { frames: 250, rate };
    assert_eq!(length.played_end(None), 250);
    assert_eq!(length.played_end(Some(100)), 100);
    assert_eq!(length.played_end(Some(400)), 250);
    assert_eq!(length.played_micros(50, None), 8_000_000);
    assert_eq!(length.played_micros(300, None), 0);
    let ntsc = MediaListItemLength {
        frames: 300,
        rate: FrameRate::new(30_000, 1_001).unwrap(),
    };
    assert_eq!(ntsc.played_micros(0, None), 10_010_000);

    let mut list = status();
    // Item 1 plays 12..250 of 300 frames at 29.97: 238 frames, 7.941266 s.
    assert_eq!(list.total_micros(), (7_941_266, false));
    assert_eq!(list.item_length(4), None);
    list.lengths[1] = Some(length);
    assert_eq!(list.total_micros(), (17_941_266, true));
    assert_eq!(list.item_length(4), Some(length));
    assert_eq!(list.item_length(2), None);
    assert!(media_list_status_fits(&list));
}

/// The longest lists leave no room for their lengths in one record: the
/// authority then reports them unknown, which still fits.
#[test]
fn a_list_too_long_for_its_lengths_does_not_fit() {
    let rate = FrameRate::new(30_000, 1_001).unwrap();
    let list = |spaces: usize, known: bool| {
        let items: Vec<MediaListItemSpec> = (1..=u32::try_from(MAX_MEDIA_LIST_ITEMS).unwrap())
            .map(|id| MediaListItemSpec {
                id: 999_744 + id,
                // A space escapes twice: five bytes on the wire.
                uri: format!("asset://a{}b", " ".repeat(spaces)),
                title: None,
                mark_in: u64::from(u32::MAX) - 1,
                mark_out: Some(u64::from(u32::MAX)),
            })
            .collect();
        MediaListStatus {
            input: input(7),
            order: items.iter().map(|item| item.id).collect(),
            active: items[0].id,
            position: 0,
            cycle: u64::MAX,
            options: options(),
            lengths: vec![
                known.then_some(MediaListItemLength {
                    frames: u64::from(u32::MAX),
                    rate,
                });
                items.len()
            ],
            items,
        }
    };
    // Short file names fit with their lengths.
    assert!(media_list_status_fits(&list(0, true)));
    // Some longer names fit only without them.
    let spaces = (0..54)
        .find(|spaces| !media_list_status_fits(&list(*spaces, true)))
        .unwrap();
    let unknown = list(spaces, false);
    assert!(media_list_status_fits(&unknown));
    round_trip(&WireMessage::MediaListState(MediaListStateMessage {
        revision: u64::MAX,
        status: unknown,
    }));
}
