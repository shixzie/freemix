//! Paged inputs (IN-014): document steps through `page_navigate`, the
//! `paged_changed`
//! durable event, the `paged` runtime event, the `paged_state` snapshot
//! records, and the `paged_page_request` / `paged_page` exchange — exactly,
//! and strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, DocumentState, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_PAGED_MESSAGE_BYTES,
    MAX_PAGED_NOTES_BYTES, MAX_PAGED_PAGES, MAX_PAGED_THUMBNAIL_BYTES, PagedKind, PagedNavigation,
    PagedPageContent, PagedPageMessage, PagedPageRequest, PagedStateMessage, PagedStatus,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, WireInputId, WireMessage,
    decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn command(payload: CommandPayload) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload,
    })
}

fn runtime(event: RuntimeLifecycleEvent) -> WireMessage {
    WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "p".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 4,
        generation: 1,
        sequence: 9,
        event,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn status() -> PagedStatus {
    PagedStatus {
        input: input(3),
        kind: PagedKind::Document,
        page: 1,
        count: 4,
        state: DocumentState::Ready,
        title: Some("Quarterly review".to_owned()),
        message: None,
    }
}

#[test]
fn document_pages_step_with_the_generic_page_navigate_command() {
    for navigation in [
        PagedNavigation::Next,
        PagedNavigation::Previous,
        PagedNavigation::First,
        PagedNavigation::Last,
        PagedNavigation::Goto(0),
        PagedNavigation::Goto(MAX_PAGED_PAGES - 1),
    ] {
        let line = round_trip(&command(CommandPayload::PageNavigate {
            input: input(3),
            navigation,
        }));
        assert!(line.contains("\tpayload=page_navigate\t"), "{line}");
    }
    // There is no document-only page command.
    let next = round_trip(&command(CommandPayload::PageNavigate {
        input: input(3),
        navigation: PagedNavigation::Next,
    }));
    refused(&next.replace("payload=page_navigate", "payload=paged_input"));
}

#[test]
fn paged_statuses_ride_events_and_snapshot_records_exactly() {
    let changed = round_trip(&event(EventPayload::PagedChanged {
        navigation: PagedNavigation::Goto(1),
        status: Box::new(status()),
    }));
    assert!(
        changed.contains("\tevent=paged_changed\taction=goto\ttarget=1\t"),
        "{changed}"
    );
    assert!(
        changed.contains(
            "\tinput=3\tpaged_kind=document\tpage=1\tcount=4\tstate=ready\ttitle=Quarterly%20review"
        ),
        "{changed}"
    );
    let runtime_line = round_trip(&runtime(RuntimeLifecycleEvent::Paged {
        status: Box::new(PagedStatus {
            page: 0,
            count: 0,
            state: DocumentState::Unavailable,
            title: None,
            message: Some(
                "install LibreOffice on the server or export the presentation to PDF".to_owned(),
            ),
            ..status()
        }),
    }));
    assert!(runtime_line.contains("\tevent=paged\t"), "{runtime_line}");
    let record = round_trip(&WireMessage::PagedState(PagedStateMessage {
        revision: 7,
        status: status(),
    }));
    assert!(record.starts_with("paged_state\t"), "{record}");

    for state in DocumentState::ALL {
        round_trip(&WireMessage::PagedState(PagedStateMessage {
            revision: 1,
            status: PagedStatus { state, ..status() },
        }));
    }

    // A known count bounds the page; an unknown count does not.
    refused(&record.replace("page=1", "page=4"));
    round_trip(&WireMessage::PagedState(PagedStateMessage {
        revision: 7,
        status: PagedStatus {
            page: 9,
            count: 0,
            ..status()
        },
    }));
    refused(&record.replace("count=4", &format!("count={}", MAX_PAGED_PAGES + 1)));
    refused(&record.replace("paged_kind=document", "paged_kind=slideshow"));
    refused(&record.replace("state=ready", "state=done"));
    refused(&record.replace("\tpaged_kind=document", ""));
    refused(&record.replace("\tcount=4", ""));
    refused(&record.replace("title=Quarterly%20review", "title="));
    refused(&changed.replace("\ttarget=1", ""));
    refused(&changed.replace("target=1", "page=1"));

    // Over-long text and a page past its count never encode.
    for bad in [
        PagedStatus {
            message: Some("m".repeat(MAX_PAGED_MESSAGE_BYTES + 1)),
            ..status()
        },
        PagedStatus {
            title: Some("line\nbreak".to_owned()),
            ..status()
        },
        PagedStatus {
            page: 4,
            ..status()
        },
    ] {
        assert!(
            encode_line(&WireMessage::PagedState(PagedStateMessage {
                revision: 1,
                status: bad.clone(),
            }))
            .is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn page_requests_and_answers_round_trip_exactly() {
    for thumbnail in [false, true] {
        let line = round_trip(&WireMessage::PagedPageRequest(PagedPageRequest {
            request_id: "r1".to_owned(),
            input: input(3),
            page: 2,
            thumbnail,
        }));
        assert!(
            line.trim_end()
                .ends_with(&format!("\tthumbnail={}", u8::from(thumbnail))),
            "{line}"
        );
    }
    let request = round_trip(&WireMessage::PagedPageRequest(PagedPageRequest {
        request_id: "r1".to_owned(),
        input: input(3),
        page: 2,
        thumbnail: true,
    }));
    refused(&request.replace("thumbnail=1", "thumbnail=yes"));
    refused(&request.replace("\trequest_id=r1", ""));

    let notes = round_trip(&WireMessage::PagedPage(PagedPageMessage {
        request_id: "r1".to_owned(),
        input: input(3),
        page: 2,
        content: PagedPageContent::Notes("Welcome everyone.\nMention the roadmap.".to_owned()),
    }));
    assert!(notes.contains("\tcontent=notes\tnotes="), "{notes}");
    round_trip(&WireMessage::PagedPage(PagedPageMessage {
        request_id: "r2".to_owned(),
        input: input(3),
        page: 0,
        content: PagedPageContent::Notes(String::new()),
    }));
    round_trip(&WireMessage::PagedPage(PagedPageMessage {
        request_id: "r3".to_owned(),
        input: input(3),
        page: 0,
        content: PagedPageContent::Unavailable,
    }));
    let thumbnail = round_trip(&WireMessage::PagedPage(PagedPageMessage {
        request_id: "r4".to_owned(),
        input: input(3),
        page: 1,
        content: PagedPageContent::Thumbnail {
            width: 128,
            height: 72,
            data: vec![b'q', b'o', b'i', b'f', 0, 1, 2, 3],
        },
    }));
    refused(&thumbnail.replace("width=128", "width=129"));
    refused(&thumbnail.replace("height=72", "height=0"));
    refused(&thumbnail.replace("content=thumbnail", "content=picture"));

    for content in [
        PagedPageContent::Notes("n".repeat(MAX_PAGED_NOTES_BYTES + 1)),
        PagedPageContent::Notes("tab\there".to_owned()),
        PagedPageContent::Thumbnail {
            width: 64,
            height: 36,
            data: Vec::new(),
        },
        PagedPageContent::Thumbnail {
            width: 64,
            height: 36,
            data: vec![0; MAX_PAGED_THUMBNAIL_BYTES + 1],
        },
    ] {
        assert!(
            encode_line(&WireMessage::PagedPage(PagedPageMessage {
                request_id: "r".to_owned(),
                input: input(3),
                page: 0,
                content,
            }))
            .is_err()
        );
    }
}
