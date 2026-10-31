//! Web access management (Protocol 2.71, trust and reach in 2.87): the
//! exact request and answer records, the pairing and CA links, and strict
//! refusal of malformed records.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ProtocolVersion, WebAccessAction, WebAccessMessage, WebAccessRefusal,
    WebAccessRequest, WebCertProblem, WebDeviceEntry, WebDeviceRole, WebGatewayStatus, WebPairing,
    WebTrust, WireMessage, decode_line, encode_line,
};

const FINGERPRINT: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:\
AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";
const CA_FINGERPRINT: &str = "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:\
01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF";

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn request(action: WebAccessAction) -> WireMessage {
    WireMessage::WebAccessRequest(WebAccessRequest {
        request_id: "wa-1".into(),
        action,
    })
}

fn gateway() -> WebGatewayStatus {
    WebGatewayStatus {
        url: "https://192.168.1.20:8443".into(),
        tls: true,
        fingerprint: Some(FINGERPRINT.into()),
        trust: WebTrust::LocalCa,
        ca_fingerprint: Some(CA_FINGERPRINT.into()),
        cert_not_after_ms: Some(1_734_000_000_000),
        cert_problem: None,
        urls: vec![
            "https://freemix-studio.local:8443".into(),
            "https://[fd00::20]:8443".into(),
        ],
        mdns: Some("freemix-studio.local".into()),
        pairing: Some(WebPairing {
            code: "042137".into(),
            link_code: "Zm9vYmFyYmF6cXV4MTIzNDU2".into(),
            role: WebDeviceRole::Operator,
            expires_in_ms: 119_500,
        }),
        devices: vec![
            WebDeviceEntry {
                id: "dev-0a1b".into(),
                name: "Floor phone".into(),
                role: WebDeviceRole::Operator,
                paired_at_ms: 1_700_000_000_000,
                last_seen_ms: Some(1_700_000_100_000),
            },
            WebDeviceEntry {
                id: "dev-ff00".into(),
                name: "Cámara 2: tally".into(),
                role: WebDeviceRole::Viewer,
                paired_at_ms: 1_700_000_000_500,
                last_seen_ms: None,
            },
        ],
    }
}

#[test]
fn the_current_protocol_is_2_71() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 141));
}

#[test]
fn requests_are_exact() {
    assert_eq!(
        round_trip(&request(WebAccessAction::Status)),
        "web_access_request\trequest_id=wa-1\taction=status\n"
    );
    assert_eq!(
        round_trip(&request(WebAccessAction::Pair {
            role: WebDeviceRole::Audio
        })),
        "web_access_request\trequest_id=wa-1\taction=pair\trole=audio\n"
    );
    assert_eq!(
        round_trip(&request(WebAccessAction::CancelPairing)),
        "web_access_request\trequest_id=wa-1\taction=cancel_pairing\n"
    );
    assert_eq!(
        round_trip(&request(WebAccessAction::Revoke {
            device: "dev-0a1b".into()
        })),
        "web_access_request\trequest_id=wa-1\taction=revoke\tdevice=dev-0a1b\n"
    );
    for line in [
        "web_access_request\trequest_id=wa-1\taction=pair\n",
        "web_access_request\trequest_id=wa-1\taction=pair\trole=admin\n",
        "web_access_request\trequest_id=wa-1\taction=status\trole=viewer\n",
        "web_access_request\trequest_id=wa-1\taction=revoke\tdevice=Dev\n",
        "web_access_request\trequest_id=wa-1\taction=list\n",
        "web_access_request\taction=status\n",
    ] {
        refused(line);
    }
    assert!(
        encode_line(&request(WebAccessAction::Revoke {
            device: "../etc".into()
        }))
        .is_err()
    );
}

#[test]
fn answers_are_exact_and_round_trip() {
    let answer = WireMessage::WebAccess(WebAccessMessage {
        request_id: "wa-1".into(),
        refusal: None,
        gateway: Some(gateway()),
    });
    let line = round_trip(&answer);
    assert!(
        line.starts_with(
            "web_access\trequest_id=wa-1\tgateway=1\turl=https%3A%2F%2F192.168.1.20%3A8443\ttls=1\t"
        ),
        "{line}"
    );
    assert!(
        line.contains("\tpairing=042137%7EZm9vYmFyYmF6cXV4MTIzNDU2%7Eoperator%7E119500\t"),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "\ttrust=local_ca\tca_fingerprint={}\tcert_not_after_ms=1734000000000\t\
             urls=https%3A%2F%2Ffreemix-studio.local%3A8443%2Chttps%3A%2F%2F%5Bfd00%3A%3A20%5D%3A8443\t\
             mdns=freemix-studio.local\t",
            CA_FINGERPRINT.replace(':', "%3A")
        )),
        "{line}"
    );
    let plain = WireMessage::WebAccess(WebAccessMessage {
        request_id: "wa-2".into(),
        refusal: Some(WebAccessRefusal::UnknownDevice),
        gateway: Some(WebGatewayStatus {
            url: "http://127.0.0.1:4000".into(),
            tls: false,
            fingerprint: None,
            trust: WebTrust::Plain,
            ca_fingerprint: None,
            cert_not_after_ms: None,
            cert_problem: None,
            urls: Vec::new(),
            mdns: None,
            pairing: None,
            devices: Vec::new(),
        }),
    });
    let plain_line = round_trip(&plain);
    assert!(
        plain_line.contains("\ttls=0\ttrust=plain\turls=\tdevices=\n"),
        "{plain_line}"
    );
    // An operator certificate has no local root, and may carry a problem.
    let mut operator = gateway();
    operator.trust = WebTrust::Operator;
    operator.ca_fingerprint = None;
    operator.cert_problem = Some(WebCertProblem::ReloadFailed);
    operator.mdns = None;
    operator.urls.clear();
    let operator_line = round_trip(&WireMessage::WebAccess(WebAccessMessage {
        request_id: "wa-3".into(),
        refusal: None,
        gateway: Some(operator),
    }));
    assert!(
        operator_line.contains(
            "\ttrust=operator\tcert_not_after_ms=1734000000000\tcert_problem=reload_failed\turls=\t"
        ),
        "{operator_line}"
    );
    for problem in [
        WebCertProblem::ReloadFailed,
        WebCertProblem::HostMismatch,
        WebCertProblem::Expiring,
    ] {
        assert_eq!(WebCertProblem::from_token(problem.token()), Some(problem));
    }
    for refusal in [
        WebAccessRefusal::PermissionDenied,
        WebAccessRefusal::WebSession,
        WebAccessRefusal::Unavailable,
        WebAccessRefusal::TooManyDevices,
        WebAccessRefusal::StorageFailed,
    ] {
        assert_eq!(
            round_trip(&WireMessage::WebAccess(WebAccessMessage {
                request_id: "r".into(),
                refusal: Some(refusal),
                gateway: None,
            })),
            format!(
                "web_access\trequest_id=r\trefusal={}\tgateway=0\n",
                refusal.token()
            )
        );
    }
}

#[test]
fn the_pairing_link_carries_codes_only_in_the_fragment() {
    let status = gateway();
    assert_eq!(
        status.pairing_link().unwrap(),
        format!(
            "https://192.168.1.20:8443/operator#pair=Zm9vYmFyYmF6cXV4MTIzNDU2&fp={}",
            FINGERPRINT.replace(':', "")
        )
    );
    let mut viewer = status.clone();
    viewer.pairing.as_mut().unwrap().role = WebDeviceRole::Viewer;
    assert!(viewer.pairing_link().unwrap().contains("/tally#pair="));
    // The local root downloads from the main origin; the mDNS origin is
    // the one listed under the advertised name.
    assert_eq!(
        status.ca_link().as_deref(),
        Some("https://192.168.1.20:8443/freemix-ca.crt")
    );
    assert_eq!(status.mdns_url(), Some("https://freemix-studio.local:8443"));
    let mut operator = status.clone();
    operator.trust = WebTrust::Operator;
    assert_eq!(operator.ca_link(), None);
    // The Debug form never shows either code.
    let debug = format!("{status:?}");
    assert!(
        !debug.contains("042137") && !debug.contains("Zm9vYmFy"),
        "{debug}"
    );
}

#[test]
fn malformed_gateways_are_refused() {
    let encode = |gateway: WebGatewayStatus| {
        encode_line(&WireMessage::WebAccess(WebAccessMessage {
            request_id: "r".into(),
            refusal: None,
            gateway: Some(gateway),
        }))
    };
    let mut no_fingerprint = gateway();
    no_fingerprint.fingerprint = None;
    assert!(encode(no_fingerprint).is_err());
    let mut lower = gateway();
    lower.fingerprint = Some(FINGERPRINT.to_lowercase());
    assert!(encode(lower).is_err());
    let mut path = gateway();
    path.url = "https://host/operator".into();
    assert!(encode(path).is_err());
    let mut unordered = gateway();
    unordered.devices.reverse();
    assert!(encode(unordered).is_err());
    let mut short = gateway();
    short.pairing.as_mut().unwrap().code = "12345".into();
    assert!(encode(short).is_err());
    let mut long_lived = gateway();
    long_lived.pairing.as_mut().unwrap().expires_in_ms = 600_000;
    assert!(encode(long_lived).is_err());
    // Trust agrees with TLS and the certificate fields.
    let mut plain_trust = gateway();
    plain_trust.trust = WebTrust::Plain;
    assert!(encode(plain_trust).is_err());
    let mut no_root = gateway();
    no_root.ca_fingerprint = None;
    assert!(encode(no_root).is_err());
    let mut operator_root = gateway();
    operator_root.trust = WebTrust::Operator;
    assert!(encode(operator_root).is_err());
    let mut no_expiry = gateway();
    no_expiry.cert_not_after_ms = None;
    assert!(encode(no_expiry).is_err());
    // Other origins are distinct origins, never the main URL.
    let mut repeated = gateway();
    repeated.urls.push(repeated.url.clone());
    assert!(encode(repeated).is_err());
    let mut with_path = gateway();
    with_path.urls.push("https://host:1/tally".into());
    assert!(encode(with_path).is_err());
    let mut too_many = gateway();
    too_many.urls = (0..9).map(|port| format!("https://h:{port}")).collect();
    assert!(encode(too_many).is_err());
    for name in ["studio", "Studio.local", ".local", "a.b.local", "-x.local"] {
        let mut bad = gateway();
        bad.mdns = Some(name.into());
        assert!(encode(bad).is_err(), "{name}");
    }

    let line = encode_line(&WireMessage::WebAccess(WebAccessMessage {
        request_id: "r".into(),
        refusal: None,
        gateway: Some(gateway()),
    }))
    .unwrap();
    refused(&line.replace("tls=1", "tls=2"));
    refused(&line.replace("trust=local_ca", "trust=self_signed"));
    refused(&line.replace("\tmdns=freemix-studio.local", "\tmdns=freemix"));
    refused(&line.replace("\tcert_not_after_ms=1734000000000", ""));
    refused(&line.replace(
        "\tcert_not_after_ms=1734000000000",
        "\tcert_not_after_ms=01",
    ));
    refused(&line.replace("\ttrust=local_ca", ""));
    refused(&line.replace(
        "\tcert_not_after_ms=1734000000000",
        "\tcert_not_after_ms=1734000000000\tcert_problem=stale",
    ));
    refused(&line.replace("gateway=1", "gateway=0"));
    refused(&line.replace("%7Eoperator%7E", "%7Eadmin%7E"));
    refused(&line.replace("11%3AFloor", "12%3AFloor"));
    refused("web_access\trequest_id=r\trefusal=nope\tgateway=0\n");
}
