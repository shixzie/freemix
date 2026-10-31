//! Exact-current contract coverage for the persistent vMix TCP API session,
//! subscription fan-out, and the `XMLTEXT` `XPath` subset.

use std::num::NonZeroU128;

use fm_protocol::{CommandPayload, WireInputId};
use fm_vmix_compat::{
    CompatInput, CompatIntent, CompatState, CompatTextField, MAX_TCP_LINE_BYTES,
    MAX_TCP_MESSAGE_BYTES, MAX_TCP_SESSIONS, MAX_XPATH_BYTES, MAX_XPATH_PREDICATES,
    MAX_XPATH_STEPS, SubscriptionTopic, TcpEvent, TcpHub, TcpReply, TcpRequest, TcpRequestError,
    TcpSession, TcpSessionError, Translation, XPathError, parse_tcp_request, xml_response,
};

fn id(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn state() -> CompatState {
    let mut state = CompatState::new(
        "29.0.0.1",
        vec![
            CompatInput::new(id(1), "Camera 1", "Camera"),
            CompatInput::new(id(2), "Lower Third", "GT").with_text_fields(vec![
                CompatTextField::new("Headline.Text", "Breaking & <news>"),
                CompatTextField::new("Sub.Text", "line one\r\nline two"),
            ]),
            CompatInput::new(id(3), "Logo", "Image"),
            CompatInput::new(id(4), "Spare 'quoted'", "Colour"),
        ],
        id(1),
        id(2),
    )
    .unwrap();
    state.set_overlay(1, Some(id(3))).unwrap();
    state
}

fn respond(reply: TcpReply) -> String {
    match reply {
        TcpReply::Respond {
            payload,
            close: false,
        } => payload,
        other => panic!("expected an open response, got {other:?}"),
    }
}

fn line(session: &mut TcpSession, request: &str, state: &CompatState) -> String {
    respond(session.handle_line(request, state).unwrap())
}

#[test]
fn request_parser_recognizes_every_command() {
    assert_eq!(
        parse_tcp_request("FUNCTION Cut Input=2\r\n"),
        Ok(TcpRequest::Function {
            function: "Cut".into(),
            query: "Input=2".into()
        })
    );
    assert_eq!(
        parse_tcp_request("function Cut"),
        Ok(TcpRequest::Function {
            function: "Cut".into(),
            query: String::new()
        })
    );
    assert_eq!(parse_tcp_request("TALLY"), Ok(TcpRequest::Tally));
    assert_eq!(parse_tcp_request("xml\r\n"), Ok(TcpRequest::Xml));
    assert_eq!(
        parse_tcp_request("XMLTEXT vmix/inputs/input[1]/@title"),
        Ok(TcpRequest::XmlText {
            xpath: "vmix/inputs/input[1]/@title".into()
        })
    );
    assert_eq!(
        parse_tcp_request("SUBSCRIBE tally"),
        Ok(TcpRequest::Subscribe(SubscriptionTopic::Tally))
    );
    assert_eq!(
        parse_tcp_request("UNSUBSCRIBE ACTS"),
        Ok(TcpRequest::Unsubscribe(SubscriptionTopic::Acts))
    );
    assert_eq!(
        parse_tcp_request("ACTS Input Camera 1"),
        Ok(TcpRequest::Acts {
            activator: "Input".into(),
            input: Some("Camera 1".into())
        })
    );
    assert_eq!(
        parse_tcp_request("ACTS Recording"),
        Ok(TcpRequest::Acts {
            activator: "Recording".into(),
            input: None
        })
    );
    assert_eq!(parse_tcp_request("VERSION"), Ok(TcpRequest::Version));
    assert_eq!(parse_tcp_request("QUIT\r\n"), Ok(TcpRequest::Quit));
}

#[test]
fn request_parser_rejects_malformed_lines() {
    assert_eq!(
        parse_tcp_request("HELLO"),
        Err(TcpRequestError::UnknownCommand)
    );
    assert_eq!(parse_tcp_request(""), Err(TcpRequestError::UnknownCommand));
    assert_eq!(
        parse_tcp_request("TALLY\nXML"),
        Err(TcpRequestError::EmbeddedLineEnding)
    );
    assert_eq!(
        parse_tcp_request("TALLY\r\n\r\n"),
        Err(TcpRequestError::EmbeddedLineEnding)
    );
    let at_limit = format!("XMLTEXT {}", "a".repeat(MAX_TCP_LINE_BYTES - 8));
    assert_eq!(at_limit.len(), MAX_TCP_LINE_BYTES);
    assert!(parse_tcp_request(&at_limit).is_ok());
    assert_eq!(
        parse_tcp_request(&format!("{at_limit}a")),
        Err(TcpRequestError::LineTooLong)
    );
    for (input, command) in [
        ("TALLY now", "TALLY"),
        ("XML extra", "XML"),
        ("QUIT now", "QUIT"),
        ("VERSION 1", "VERSION"),
        ("XMLTEXT", "XMLTEXT"),
        ("FUNCTION", "FUNCTION"),
        ("SUBSCRIBE", "SUBSCRIBE"),
        ("UNSUBSCRIBE XML", "UNSUBSCRIBE"),
        ("ACTS", "ACTS"),
    ] {
        match parse_tcp_request(input) {
            Err(TcpRequestError::InvalidArguments {
                command: actual, ..
            }) => {
                assert_eq!(actual, command, "{input}");
            }
            other => panic!("{input}: {other:?}"),
        }
    }
}

#[test]
fn error_responses_are_framed_in_band() {
    let state = state();
    let mut session = TcpSession::new();
    assert_eq!(
        line(&mut session, "HELLO", &state),
        "ERROR Unknown command\r\n"
    );
    assert_eq!(
        line(&mut session, "TALLY extra", &state),
        "TALLY ER Command takes no arguments\r\n"
    );
    assert_eq!(
        line(&mut session, "SUBSCRIBE XML", &state),
        "SUBSCRIBE ER Unknown subscription\r\n"
    );
    assert_eq!(
        line(&mut session, "A\nB", &state),
        "ERROR Embedded line ending\r\n"
    );
    assert!(!session.is_closed());
}

#[test]
fn tally_xml_and_version_responses_are_exact() {
    let state = state();
    let mut session = TcpSession::new();
    assert_eq!(line(&mut session, "TALLY\r\n", &state), "TALLY OK 1210\r\n");
    let xml = line(&mut session, "XML\r\n", &state);
    let expected_xml = state.xml();
    assert_eq!(xml, format!("XML {}\r\n{expected_xml}", expected_xml.len()));
    assert_eq!(xml, xml_response(&state));
    let (header, payload) = xml.split_once("\r\n").unwrap();
    let length: usize = header.strip_prefix("XML ").unwrap().parse().unwrap();
    assert_eq!(length, payload.len());
    assert!(payload.ends_with("</vmix>"));
    assert_eq!(
        line(&mut session, "VERSION", &state),
        "VERSION OK 29.0.0.1\r\n"
    );
}

#[test]
fn xml_length_counts_utf8_bytes() {
    let state = CompatState::new(
        "29",
        vec![CompatInput::new(id(9), "Kamera \u{e9}\u{1f3a5}", "Camera")],
        id(9),
        id(9),
    )
    .unwrap();
    let response = xml_response(&state);
    let (header, payload) = response.split_once("\r\n").unwrap();
    assert_eq!(header, format!("XML {}", payload.len()));
    assert!(payload.len() > payload.chars().count());
}

#[test]
fn function_dispatch_requires_completion_in_order() {
    let state = state();
    let mut session = TcpSession::new();
    assert_eq!(
        session.handle_line("FUNCTION PreviewInput Input=Logo\r\n", &state),
        Ok(TcpReply::Dispatch(Translation::Protocol(
            CommandPayload::SelectPreview { input: id(3) }
        )))
    );
    assert!(session.function_pending());
    assert_eq!(
        session.handle_line("TALLY", &state),
        Err(TcpSessionError::FunctionPending)
    );
    assert_eq!(
        session.complete_function(Ok(())),
        Ok("FUNCTION OK Completed\r\n".to_owned())
    );
    assert_eq!(
        session.complete_function(Ok(())),
        Err(TcpSessionError::NoPendingFunction)
    );

    assert_eq!(
        session.handle_line("FUNCTION Fade Input=4&Duration=500", &state),
        Ok(TcpReply::Dispatch(Translation::Compat(
            CompatIntent::Fade {
                input: Some(id(4)),
                duration_millis: 500,
            }
        )))
    );
    assert_eq!(
        session.complete_function(Err("engine\r\nrejected")),
        Ok("FUNCTION ER engine  rejected\r\n".to_owned())
    );
    assert!(!session.function_pending());
}

#[test]
fn function_errors_and_unsupported_answer_immediately() {
    let state = state();
    let mut session = TcpSession::new();
    assert_eq!(
        line(&mut session, "FUNCTION Nope", &state),
        "FUNCTION ER unknown function: Nope\r\n"
    );
    assert_eq!(
        line(&mut session, "FUNCTION PreviewInput Input=99", &state),
        "FUNCTION ER input not found: 99\r\n"
    );
    assert_eq!(
        line(&mut session, "FUNCTION AddInput", &state),
        "FUNCTION ER the function has no semantics in the current compatibility core\r\n"
    );
    assert_eq!(
        line(&mut session, "FUNCTION Cut Mix=1", &state),
        "FUNCTION ER only the main mix (0) has a native switcher command\r\n"
    );
    assert!(!session.function_pending());

    let long_name = "N".repeat(MAX_TCP_MESSAGE_BYTES * 2);
    let response = line(&mut session, &format!("FUNCTION {long_name}"), &state);
    assert!(response.starts_with("FUNCTION ER unknown function: NNN"));
    assert!(response.ends_with("\r\n"));
    assert_eq!(
        response.len(),
        "FUNCTION ER ".len() + MAX_TCP_MESSAGE_BYTES + 2
    );
}

#[test]
fn acts_queries_resolve_inputs_and_report_values() {
    let mut state = state();
    state.recording = true;
    let mut session = TcpSession::new();
    let cases = [
        ("ACTS Input 1", "ACTS OK Input 1 1\r\n"),
        ("ACTS Input 2", "ACTS OK Input 2 0\r\n"),
        ("ACTS Input -1", "ACTS OK Input 1 1\r\n"),
        ("ACTS InputPreview 0", "ACTS OK InputPreview 2 1\r\n"),
        (
            "ACTS InputPreview Lower Third",
            "ACTS OK InputPreview 2 1\r\n",
        ),
        ("ACTS Overlay1 Logo", "ACTS OK Overlay1 3 1\r\n"),
        ("ACTS Overlay2 3", "ACTS OK Overlay2 3 0\r\n"),
        (
            "ACTS Overlay8 00000000-0000-0000-0000-000000000004",
            "ACTS OK Overlay8 4 0\r\n",
        ),
        ("ACTS Recording", "ACTS OK Recording 1\r\n"),
        ("ACTS Streaming", "ACTS OK Streaming 0\r\n"),
        ("ACTS Input", "ACTS ER Missing input\r\n"),
        ("ACTS Input 9", "ACTS ER input not found: 9\r\n"),
        ("ACTS Recording 1", "ACTS ER Activator takes no input\r\n"),
        ("ACTS Overlay9 1", "ACTS ER Unknown activator\r\n"),
        ("ACTS input 1", "ACTS ER Unknown activator\r\n"),
    ];
    for (request, expected) in cases {
        assert_eq!(line(&mut session, request, &state), expected, "{request}");
    }
}

#[test]
fn subscriptions_are_idempotent_and_emit_only_changes() {
    let mut state = state();
    let mut session = TcpSession::new();
    assert_eq!(session.events(&state), Vec::<String>::new());
    assert_eq!(
        line(&mut session, "SUBSCRIBE TALLY", &state),
        "SUBSCRIBE OK TALLY\r\n"
    );
    assert!(session.is_subscribed(SubscriptionTopic::Tally));
    assert!(!session.is_subscribed(SubscriptionTopic::Acts));
    assert_eq!(session.events(&state), Vec::<String>::new());

    state.active = id(2);
    state.preview = id(4);
    // Re-subscribing does not reset the baseline.
    assert_eq!(
        line(&mut session, "subscribe tally", &state),
        "SUBSCRIBE OK TALLY\r\n"
    );
    assert_eq!(session.events(&state), vec!["TALLY OK 0112\r\n".to_owned()]);
    assert_eq!(session.events(&state), Vec::<String>::new());

    assert_eq!(
        line(&mut session, "UNSUBSCRIBE TALLY", &state),
        "UNSUBSCRIBE OK TALLY\r\n"
    );
    assert_eq!(
        line(&mut session, "UNSUBSCRIBE TALLY", &state),
        "UNSUBSCRIBE OK TALLY\r\n"
    );
    state.active = id(1);
    assert_eq!(session.events(&state), Vec::<String>::new());
}

#[test]
fn acts_events_have_deterministic_order() {
    let mut state = state();
    let mut session = TcpSession::new();
    assert_eq!(
        line(&mut session, "SUBSCRIBE ACTS", &state),
        "SUBSCRIBE OK ACTS\r\n"
    );
    assert_eq!(
        line(&mut session, "SUBSCRIBE TALLY", &state),
        "SUBSCRIBE OK TALLY\r\n"
    );

    // Cut 1 -> 2 with preview moving to 1, overlay 1 moving 3 -> 4,
    // overlay 2 gaining 1, recording and streaming starting.
    state.active = id(2);
    state.preview = id(1);
    state.set_overlay(1, Some(id(4))).unwrap();
    state.set_overlay(2, Some(id(1))).unwrap();
    state.recording = true;
    state.streaming = true;
    assert_eq!(
        session.events(&state),
        vec![
            "TALLY OK 1101\r\n",
            "ACTS OK Input 1 0\r\n",
            "ACTS OK Input 2 1\r\n",
            "ACTS OK InputPreview 2 0\r\n",
            "ACTS OK InputPreview 1 1\r\n",
            "ACTS OK Overlay1 3 0\r\n",
            "ACTS OK Overlay1 4 1\r\n",
            "ACTS OK Overlay2 1 1\r\n",
            "ACTS OK Recording 1\r\n",
            "ACTS OK Streaming 1\r\n",
        ]
    );
    assert_eq!(session.events(&state), Vec::<String>::new());

    // Activators are ordinal-based: removing inputs deactivates vacated
    // ordinals, while Preview staying on ordinal 1 emits nothing.
    let shrunk = CompatState::new(
        "29.0.0.1",
        vec![CompatInput::new(id(2), "Lower Third", "GT")],
        id(2),
        id(2),
    )
    .unwrap();
    assert_eq!(
        session.events(&shrunk),
        vec![
            "TALLY OK 1\r\n",
            "ACTS OK Input 2 0\r\n",
            "ACTS OK Input 1 1\r\n",
            "ACTS OK Overlay1 4 0\r\n",
            "ACTS OK Overlay2 1 0\r\n",
            "ACTS OK Recording 0\r\n",
            "ACTS OK Streaming 0\r\n",
        ]
    );
}

#[test]
fn quit_closes_session_and_clears_subscriptions() {
    let state = state();
    let mut session = TcpSession::new();
    line(&mut session, "SUBSCRIBE TALLY", &state);
    assert_eq!(
        session.handle_line("QUIT\r\n", &state),
        Ok(TcpReply::Respond {
            payload: "QUIT OK Bye\r\n".to_owned(),
            close: true
        })
    );
    assert!(session.is_closed());
    assert!(!session.is_subscribed(SubscriptionTopic::Tally));
    assert_eq!(
        session.handle_line("TALLY", &state),
        Err(TcpSessionError::Closed)
    );
}

#[test]
fn xmltext_selects_values_from_the_state_document() {
    let mut state = state();
    state.streaming = true;
    let cases = [
        ("vmix/version", "29.0.0.1"),
        ("/vmix/active", "1"),
        ("vmix/preview", "2"),
        ("vmix/recording", "False"),
        ("vmix/streaming", "True"),
        ("vmix/inputs/input[1]/@title", "Camera 1"),
        ("vmix/inputs/input[2]/@type", "GT"),
        ("vmix/inputs/input[3]/@number", "3"),
        (
            "vmix/inputs/input[4]/@key",
            "00000000-0000-0000-0000-000000000004",
        ),
        ("vmix/inputs/input[1]", "Camera 1"),
        ("vmix/inputs/input[@title='Logo']/@number", "3"),
        ("vmix/inputs/input[@title=\"Spare 'quoted'\"]/@number", "4"),
        (
            "vmix/inputs/input[@key='00000000-0000-0000-0000-000000000002']/text[1]",
            "Breaking & <news>",
        ),
        ("vmix/inputs/input[2]/text[@name='Sub.Text']/@index", "1"),
        ("vmix/inputs/input[@type='GT'][1]/@title", "Lower Third"),
        ("vmix/inputs/input/@title", "Camera 1"),
        ("vmix/overlays/overlay[1]", "3"),
        ("vmix/overlays/overlay[@number='2']", ""),
        ("vmix/overlays/overlay[8]/@number", "8"),
    ];
    for (xpath, expected) in cases {
        assert_eq!(state.xml_text(xpath).as_deref(), Ok(expected), "{xpath}");
    }
    // Element string-value concatenates the title and descendant field text.
    assert_eq!(
        state.xml_text("vmix/inputs/input[2]").unwrap(),
        "Lower ThirdBreaking & <news>line one\r\nline two"
    );
}

#[test]
fn xmltext_errors_are_distinct_and_bounded() {
    let state = state();
    for xpath in [
        "vmix/inputs/input[5]",
        "vmix/inputs/input[@title='Nope']",
        "vmix/missing",
        "other/version",
        "vmix/inputs/input[1]/@missing",
        "vmix/inputs/input[@type='GT'][2]",
    ] {
        assert_eq!(state.xml_text(xpath), Err(XPathError::NotFound), "{xpath}");
    }
    for xpath in [
        "vmix/",
        "vmix/inputs/input[0]",
        "vmix/inputs/input[",
        "vmix/inputs/input[1",
        "vmix/inputs/input[@title='x]",
        "vmix/inputs/input[@title=x]",
        "vmix/@x/y",
        "vmix/text(1)",
        "vmix/inputs/input[1]/text()/@x",
        "@x",
    ] {
        assert!(
            matches!(state.xml_text(xpath), Err(XPathError::Syntax(_))),
            "{xpath}: {:?}",
            state.xml_text(xpath)
        );
    }
    assert_eq!(state.xml_text(""), Err(XPathError::Empty));
    assert_eq!(
        state.xml_text(&"a".repeat(MAX_XPATH_BYTES + 1)),
        Err(XPathError::TooLong)
    );
    let deep = vec!["a"; MAX_XPATH_STEPS + 1].join("/");
    assert_eq!(state.xml_text(&deep), Err(XPathError::TooComplex));
    let at_step_limit = vec!["a"; MAX_XPATH_STEPS].join("/");
    assert_eq!(state.xml_text(&at_step_limit), Err(XPathError::NotFound));
    let many_predicates = format!("vmix{}", "[1]".repeat(MAX_XPATH_PREDICATES + 1));
    assert_eq!(
        state.xml_text(&many_predicates),
        Err(XPathError::TooComplex)
    );
    let at_predicate_limit = format!("vmix{}/version", "[1]".repeat(MAX_XPATH_PREDICATES));
    assert_eq!(
        state.xml_text(&at_predicate_limit).as_deref(),
        Ok("29.0.0.1")
    );
}

#[test]
fn xmltext_responses_stay_single_line() {
    let state = state();
    let mut session = TcpSession::new();
    assert_eq!(
        line(&mut session, "XMLTEXT vmix/inputs/input[2]/text[2]", &state),
        "XMLTEXT OK line one  line two\r\n"
    );
    assert_eq!(
        line(&mut session, "XMLTEXT vmix/inputs/input[1]/@title", &state),
        "XMLTEXT OK Camera 1\r\n"
    );
    assert_eq!(
        line(&mut session, "XMLTEXT vmix/overlays/overlay[3]", &state),
        "XMLTEXT OK \r\n"
    );
    assert_eq!(
        line(&mut session, "XMLTEXT vmix/nothing", &state),
        "XMLTEXT ER XPath selected no node\r\n"
    );
    assert_eq!(
        line(&mut session, "XMLTEXT //input[1]/@title", &state),
        "XMLTEXT OK Camera 1\r\n"
    );
}

#[test]
fn hub_bounds_sessions_and_fans_out_in_id_order() {
    let mut state = state();
    let mut hub = TcpHub::new();
    assert!(hub.is_empty());
    let ids: Vec<_> = (0..MAX_TCP_SESSIONS).map(|_| hub.open().unwrap()).collect();
    assert_eq!(hub.len(), MAX_TCP_SESSIONS);
    assert_eq!(hub.open(), Err(TcpSessionError::TooManySessions));
    assert_eq!(ids[0].get(), 1);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));

    // Free all but three sessions; IDs are never reused.
    for id in &ids[3..] {
        assert!(hub.close(*id));
    }
    assert!(!hub.close(ids[5]));
    let late = hub.open().unwrap();
    assert_eq!(late.get(), u64::try_from(MAX_TCP_SESSIONS).unwrap() + 1);

    let (a, b, c) = (ids[0], ids[1], ids[2]);
    respond(hub.handle_line(late, "SUBSCRIBE TALLY", &state).unwrap());
    respond(hub.handle_line(a, "SUBSCRIBE ACTS", &state).unwrap());
    respond(hub.handle_line(a, "SUBSCRIBE TALLY", &state).unwrap());
    respond(hub.handle_line(c, "SUBSCRIBE TALLY", &state).unwrap());
    assert!(hub.session(b).is_some_and(|session| {
        !session.is_subscribed(SubscriptionTopic::Tally)
            && !session.is_subscribed(SubscriptionTopic::Acts)
    }));
    assert_eq!(hub.publish(&state), Vec::<TcpEvent>::new());

    state.recording = true;
    state.preview = id(3);
    let event = |session, payload: &str| TcpEvent {
        session,
        payload: payload.to_owned(),
    };
    assert_eq!(
        hub.publish(&state),
        vec![
            event(a, "TALLY OK 1010\r\n"),
            event(a, "ACTS OK InputPreview 2 0\r\n"),
            event(a, "ACTS OK InputPreview 3 1\r\n"),
            event(a, "ACTS OK Recording 1\r\n"),
            event(c, "TALLY OK 1010\r\n"),
            event(late, "TALLY OK 1010\r\n"),
        ]
    );
    assert_eq!(hub.publish(&state), Vec::<TcpEvent>::new());

    // FUNCTION completion through the hub.
    assert!(matches!(
        hub.handle_line(b, "FUNCTION Cut", &state),
        Ok(TcpReply::Dispatch(Translation::Protocol(
            CommandPayload::Cut
        )))
    ));
    assert_eq!(
        hub.complete_function(b, Ok(())),
        Ok("FUNCTION OK Completed\r\n".to_owned())
    );

    // QUIT removes the session from the hub.
    assert_eq!(
        hub.handle_line(c, "QUIT", &state),
        Ok(TcpReply::Respond {
            payload: "QUIT OK Bye\r\n".to_owned(),
            close: true
        })
    );
    assert!(hub.session(c).is_none());
    assert_eq!(
        hub.handle_line(c, "TALLY", &state),
        Err(TcpSessionError::UnknownSession)
    );
    assert_eq!(
        hub.complete_function(c, Ok(())),
        Err(TcpSessionError::UnknownSession)
    );
    assert_eq!(hub.len(), 3);
}
