//! IN-008 and IN-009 on the wire: image and slideshow `input_add` sources
//! (a client may name an absolute host path for the server to import; every
//! record the server sends names project assets only), `page_navigate`, the
//! `page_navigated` durable event, the `slideshow_page` runtime event, the
//! snapshot's `still_state` records, the `slideshow_slides` window
//! exchange, and the cue `page.` action — exactly, and strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CueListSpec, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, ImageInputConfig, LiveInputSource,
    MAX_SLIDESHOW_SLIDES_PAGE, PagedNavigation, RuntimeEventMessage, RuntimeLifecycleEvent,
    ServerIdentity, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem,
    SlideshowPage, SlideshowPageStatus, SlideshowSlide, SlideshowSlidesMessage,
    SlideshowSlidesRequest, SlideshowSource, StillStateMessage, WireCue, WireCueAction,
    WireInputId, WireMessage, WireStillSource, decode_line, encode_line,
};
use fm_types::OutputScaling;

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

fn slideshow() -> SlideshowConfig {
    let mut config = SlideshowConfig::new(SlideshowSource::List(vec![
        SlideshowItem::new("asset://a.png").with_caption("Opening: one, two"),
        SlideshowItem::new("asset://b c.jpg"),
    ]));
    config.slide_frames = 125;
    config.transition = SlideTransitionStyle::Fade;
    config.transition_frames = 12;
    config.ken_burns = true;
    config.order = SlideOrder::Random { seed: 7 };
    config.loop_show = false;
    config.auto_advance = false;
    config.fit = OutputScaling::Fill;
    config
}

fn prefix() -> String {
    format!("command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid=a\tidempotency_key=k")
}

const LIST: &str = "125%7Efade%7E12%7E1%7Erandom.7%7E0%7E0%7Efill%7Elist%7E13%3Aasset%3A%2F%2Fa.png%7E17%3AOpening%3A%20one%2C%20two%2C15%3Aasset%3A%2F%2Fb%20c.jpg%7E-";

#[test]
fn image_and_slideshow_adds_encode_exactly_and_strictly() {
    let prefix = prefix();
    let image = command(CommandPayload::AddInput {
        input: input(5),
        name: "Logo".into(),
        source: LiveInputSource::Image(Box::new(ImageInputConfig::new(
            "/srv/show/logo.png",
            OutputScaling::OneToOne,
        ))),
    });
    assert_eq!(
        round_trip(&image),
        format!(
            "{prefix}\tpayload=input_add\tinput=5\tname=Logo\tkind=image\turi=%2Fsrv%2Fshow%2Flogo.png\tfit=1%3A1\n"
        )
    );
    let slides = command(CommandPayload::AddInput {
        input: input(6),
        name: "Slides".into(),
        source: LiveInputSource::Slideshow(Box::new(slideshow())),
    });
    assert_eq!(
        round_trip(&slides),
        format!(
            "{prefix}\tpayload=input_add\tinput=6\tname=Slides\tkind=slideshow\tslideshow={LIST}\n"
        )
    );
    let folder = command(CommandPayload::AddInput {
        input: input(7),
        name: "Folder".into(),
        source: LiveInputSource::Slideshow(Box::new(SlideshowConfig::new(
            SlideshowSource::Folder {
                asset_uri: "asset://Event Slides".into(),
            },
        ))),
    });
    assert_eq!(
        round_trip(&folder),
        format!(
            "{prefix}\tpayload=input_add\tinput=7\tname=Folder\tkind=slideshow\tslideshow=150%7Efade%7E30%7E0%7Eordered%7E1%7E1%7Efit%7Efolder%7E20%3Aasset%3A%2F%2FEvent%20Slides\n"
        )
    );

    let image = format!("{prefix}\tpayload=input_add\tinput=5\tname=Logo\tkind=image");
    for tail in [
        // A relative path, a non-picture, an unknown placement.
        "\turi=logo.png\tfit=fit",
        "\turi=%2Fsrv%2Fnotes.txt\tfit=fit",
        "\turi=asset%3A%2F%2F..%2Fx.png\tfit=fit",
        "\turi=%2Fsrv%2Flogo.png\tfit=zoom",
        // Missing, duplicated, and unknown fields.
        "\turi=%2Fsrv%2Flogo.png",
        "\tfit=fit",
        "\turi=%2Fsrv%2Flogo.png\tfit=fit\tfit=fill",
        "\turi=%2Fsrv%2Flogo.png\tfit=fit\textra=1",
    ] {
        refused(&format!("{image}{tail}\n"));
    }
    let slideshow =
        format!("{prefix}\tpayload=input_add\tinput=6\tname=Slides\tkind=slideshow\tslideshow=");
    for settings in [
        // No frames, an unknown transition, a zero fade, an unknown order.
        LIST.replacen("125", "0", 1),
        LIST.replacen("fade", "wipe", 1),
        LIST.replacen("%7E12%7E", "%7E0%7E", 1),
        LIST.replacen("random.7", "shuffle", 1),
        // A length prefix that does not match its text.
        LIST.replacen("13%3Aasset", "12%3Aasset", 1),
        // A relative picture, an escaping folder, an empty list.
        LIST.replacen("13%3Aasset%3A%2F%2Fa.png", "5%3Aa.png", 1),
        "150%7Efade%7E30%7E0%7Eordered%7E1%7E1%7Efit%7Efolder%7E12%3Aasset%3A%2F%2F..".into(),
        "150%7Efade%7E30%7E0%7Eordered%7E1%7E1%7Efit%7Elist%7E".into(),
        // Trailing text after the last item.
        format!("{LIST}%7E"),
    ] {
        refused(&format!("{slideshow}{settings}\n"));
    }
}

#[test]
fn page_navigation_command_event_and_cue_action_encode_exactly() {
    let prefix = prefix();
    let page = command(CommandPayload::PageNavigate {
        input: input(6),
        navigation: PagedNavigation::Goto(3),
    });
    assert_eq!(
        round_trip(&page),
        format!("{prefix}\tpayload=page_navigate\tinput=6\taction=goto\tpage=3\n")
    );
    for action in ["next", "previous", "first", "last"] {
        let line = format!("{prefix}\tpayload=page_navigate\tinput=6\taction={action}\n");
        assert_eq!(round_trip(&decode_line(&line).unwrap()), line);
        refused(&format!(
            "{prefix}\tpayload=page_navigate\tinput=6\taction={action}\tpage=1\n"
        ));
    }
    for tail in [
        "action=goto",
        "action=skip",
        "action=goto\tpage=-1",
        "page=1",
    ] {
        refused(&format!(
            "{prefix}\tpayload=page_navigate\tinput=6\t{tail}\n"
        ));
    }

    let navigated = event(EventPayload::PageNavigated {
        input: input(6),
        frame: 40,
        navigation: PagedNavigation::Next,
    });
    assert_eq!(
        round_trip(&navigated),
        "event\tengine_id=e\tstate_epoch=1\tlog_id=l\trevision=4\tevent=page_navigated\tinput=6\tframe=40\taction=next\tundo_depth=0\tredo_depth=0\n"
    );

    let cue = command(CommandPayload::SetCueList {
        list: 1,
        spec: Box::new(CueListSpec {
            name: "Show".into(),
            cues: vec![WireCue {
                id: 1,
                name: "Slides".into(),
                auto_follow: false,
                actions: vec![
                    WireCueAction::Page {
                        input: input(6),
                        navigation: PagedNavigation::Next,
                    },
                    WireCueAction::Page {
                        input: input(6),
                        navigation: PagedNavigation::Goto(4),
                    },
                ],
            }],
        }),
    });
    let cues = "\tpayload=cue_list_set\tlist=1\tname=Show\tcues=1%7E536c69646573%7E0%7E";
    assert_eq!(
        round_trip(&cue),
        format!("{prefix}{cues}page.6.next%2Fpage.6.goto.4\n")
    );
    for action in ["page.6.goto", "page.6.skip", "page.6.next.1", "page.0.next"] {
        refused(&format!("{prefix}{cues}{action}\n"));
    }
}

#[test]
fn still_states_and_slideshow_pages_name_project_assets_only() {
    let state = WireMessage::StillState(StillStateMessage {
        revision: 4,
        input: input(6),
        source: WireStillSource::Slideshow(slideshow()),
        status: SlideshowPageStatus {
            page: SlideshowPage { index: 0, count: 2 },
            label: "Opening: one, two".into(),
            generation: 1,
        },
    });
    let line = round_trip(&state);
    assert_eq!(
        line,
        format!(
            "still_state\trevision=4\tinput=6\tkind=slideshow\tslideshow={LIST}\tpage=0\tcount=2\tlisting=1\tlabel=Opening%3A%20one%2C%20two\n"
        )
    );
    let image = WireMessage::StillState(StillStateMessage {
        revision: 4,
        input: input(5),
        source: WireStillSource::Image(ImageInputConfig::new(
            "asset://logo.png",
            OutputScaling::Fit,
        )),
        status: SlideshowPageStatus::default(),
    });
    let image_line = round_trip(&image);
    // The server names imported assets: a host path never comes back.
    refused(&image_line.replace("asset%3A%2F%2Flogo.png", "%2Fsrv%2Flogo.png"));
    refused(&line.replace("13%3Aasset%3A%2F%2Fa.png", "10%3A%2Fsrv%2Fa.png"));
    let added = round_trip(&event(EventPayload::InputAdded {
        input: input(5),
        name: "Logo".into(),
        network_stream: None,
        system_audio: None,
        capture: None,
        still: Some(Box::new(WireStillSource::Image(ImageInputConfig::new(
            "asset://logo.png",
            OutputScaling::Fit,
        )))),
    }));
    refused(&added.replace("asset%3A%2F%2Flogo.png", "%2Fsrv%2Flogo.png"));
    // The slide on air lies inside the listing; the label is bounded.
    refused(&line.replace("\tpage=0\t", "\tpage=2\t"));
    refused(&line.replace("\tlisting=1\t", "\tgeneration=1\t"));
    refused(&line.replace(
        "\tlabel=Opening%3A%20one%2C%20two",
        &format!(
            "\tlabel={}",
            "x".repeat(fm_protocol::MAX_SLIDE_LABEL_BYTES + 1)
        ),
    ));

    let status = runtime(RuntimeLifecycleEvent::SlideshowPage {
        input: input(6),
        status: Box::new(SlideshowPageStatus {
            page: SlideshowPage { index: 1, count: 2 },
            label: "b c.jpg".into(),
            generation: 3,
        }),
    });
    let line = round_trip(&status);
    assert_eq!(
        line,
        "runtime_event\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\trevision=4\tgeneration=1\tsequence=9\tevent=slideshow_page\tinput=6\tpage=1\tcount=2\tlisting=3\tlabel=b%20c.jpg\n"
    );
    refused(&line.replace("\tpage=1\t", "\tpage=2\t"));
    refused(&line.replace("\tlisting=3", ""));
    // An empty slideshow shows page zero of zero.
    let empty = line
        .replace("\tpage=1\tcount=2", "\tpage=0\tcount=0")
        .replace("b%20c.jpg", "");
    round_trip(&decode_line(&empty).unwrap());
}

#[test]
fn slide_windows_are_requested_and_answered_within_bounds() {
    let request = WireMessage::SlideshowSlidesRequest(SlideshowSlidesRequest {
        request_id: "r1".into(),
        input: input(6),
        offset: 0,
        limit: MAX_SLIDESHOW_SLIDES_PAGE,
    });
    let line = round_trip(&request);
    assert_eq!(
        line,
        "slideshow_slides_request\trequest_id=r1\tinput=6\toffset=0\tlimit=64\n"
    );
    refused(&line.replace("limit=64", "limit=65"));
    refused(&line.replace("limit=64", "limit=0"));
    refused(&line.replace("\toffset=0", ""));

    let answer = WireMessage::SlideshowSlides(SlideshowSlidesMessage {
        request_id: "r1".into(),
        input: input(6),
        generation: 1,
        offset: 0,
        total: 2,
        slides: vec![
            SlideshowSlide {
                asset_uri: "asset://a.png".into(),
                caption: Some("Opening".into()),
            },
            SlideshowSlide {
                asset_uri: "asset://b c.jpg".into(),
                caption: None,
            },
        ],
    });
    let line = round_trip(&answer);
    assert_eq!(
        line,
        "slideshow_slides\trequest_id=r1\tinput=6\tgeneration=1\toffset=0\ttotal=2\tslides=13%3Aasset%3A%2F%2Fa.png%7E7%3AOpening%2C15%3Aasset%3A%2F%2Fb%20c.jpg%7E-\n"
    );
    // More slides than the window holds, or past the total.
    refused(&line.replace("\ttotal=2\t", "\ttotal=1\t"));
    refused(&line.replace("13%3Aasset%3A%2F%2Fa.png", "10%3A%2Fsrv%2Fa.png"));
}
