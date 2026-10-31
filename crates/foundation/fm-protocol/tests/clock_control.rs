//! SW-013: production-clock control requests and answers round-trip exactly
//! and strictly (Protocol 2.150).

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClockControlOperation, ClockControlRequest, ClockControlResponse,
    ProductionClockKind, ProductionClockState, ProductionClockStatus, WireMessage, decode_line,
    encode_line,
};

fn request(operation: ClockControlOperation) -> WireMessage {
    WireMessage::ClockControlRequest(ClockControlRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "clock-1".to_owned(),
        name: "Countdown".to_owned(),
        operation,
    })
}

fn reading() -> ProductionClockStatus {
    ProductionClockStatus {
        name: "Countdown".to_owned(),
        kind: ProductionClockKind::Timer,
        state: ProductionClockState::Running,
        text: "00:05".to_owned(),
        value_ms: Some(5_000),
        expired: false,
        zone: None,
    }
}

#[test]
fn clock_control_requests_round_trip_exactly() {
    let line = encode_line(&request(ClockControlOperation::Start)).unwrap();
    assert_eq!(
        line,
        "clock_control_request\tprotocol=2.150\trequest_id=clock-1\tname=Countdown\t\
         operation=start\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    let line = encode_line(&request(ClockControlOperation::SetValue(5_000_000_000))).unwrap();
    assert_eq!(
        line,
        "clock_control_request\tprotocol=2.150\trequest_id=clock-1\tname=Countdown\t\
         operation=set_value\tvalue_ns=5000000000\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    for operation in [
        ClockControlOperation::Pause,
        ClockControlOperation::Reset,
        ClockControlOperation::Adjust(-1_500_000_000),
        ClockControlOperation::SetPreset(300_000_000_000),
    ] {
        let line = encode_line(&request(operation)).unwrap();
        assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);
    }
}

#[test]
fn clock_control_responses_round_trip_exactly() {
    let accepted = WireMessage::ClockControlResponse(ClockControlResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "clock-1".to_owned(),
        name: "Countdown".to_owned(),
        accepted: true,
        error: None,
        reading: Some(reading()),
    });
    let line = encode_line(&accepted).unwrap();
    assert!(
        line.starts_with(
            "clock_control_response\tprotocol=2.150\trequest_id=clock-1\tname=Countdown\t\
             accepted=1\treading="
        ),
        "{line}"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    let refused = WireMessage::ClockControlResponse(ClockControlResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "clock-1".to_owned(),
        name: "Countdown".to_owned(),
        accepted: false,
        error: Some("clock `Countdown` is not a timer".to_owned()),
        reading: None,
    });
    let line = encode_line(&refused).unwrap();
    assert_eq!(
        line,
        "clock_control_response\tprotocol=2.150\trequest_id=clock-1\tname=Countdown\t\
         accepted=0\terror=clock%20%60Countdown%60%20is%20not%20a%20timer\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);
}

#[test]
fn clock_control_requests_are_strict() {
    for line in [
        // Missing the operation, or a value with an operation that takes none.
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\toperation=start\t\
         value_ns=1",
        // A value-taking operation needs its value, and only a canonical one.
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\t\
         operation=set_value",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\t\
         operation=set_value\tvalue_ns=05000",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\t\
         operation=set_value\tvalue_ns=+5",
        // An unknown operation, a blank or invalid name, and an unknown field.
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\toperation=stop",
        "clock_control_request\tprotocol=2.150\trequest_id=\tname=Countdown\toperation=start",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Count down\toperation=start",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=\toperation=start",
        "clock_control_request\tprotocol=2.150\trequest_id=r\tname=Countdown\toperation=start\t\
         x=1",
    ] {
        assert!(decode_line(&format!("{line}\n")).is_err(), "{line}");
    }
}

#[test]
fn clock_control_responses_are_strict() {
    for line in [
        // An accepted answer cannot carry an error, and a refusal needs one.
        "clock_control_response\tprotocol=2.150\trequest_id=r\tname=Countdown\taccepted=1\t\
         error=no",
        // A non-canonical flag.
        "clock_control_response\tprotocol=2.150\trequest_id=r\tname=Countdown\taccepted=2",
        // An invalid name, an unknown field, and an over-long reading list.
        "clock_control_response\tprotocol=2.150\trequest_id=r\tname=Count down\taccepted=1",
        "clock_control_response\tprotocol=2.150\trequest_id=r\tname=Countdown\taccepted=1\tx=1",
        "clock_control_response\tprotocol=2.150\trequest_id=r\tname=Countdown\taccepted=1\t\
         reading=not-a-reading",
    ] {
        assert!(decode_line(&format!("{line}\n")).is_err(), "{line}");
    }
}
