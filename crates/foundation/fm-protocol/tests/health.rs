//! Protocol 2.99 `health_status` and the `diagnostics_response` health
//! fields: the exact lossy daemon utilization and alert record (RC-015).

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, DaemonHealth, DiagnosticsResponse, EngineIdentity,
    HealthAlert, HealthAlertRule, HealthNotification, HealthNotificationKind, HealthSeverity,
    HealthStatusMessage, MAX_HEALTH_NOTIFICATIONS, ServerIdentity, WireMessage, decode_line,
    encode_line,
};

const HEAD: &str = "health_status\tengine_id=engine-a\tproject_id=project-a\tstate_epoch=7\tlog_id=log-a\tsequence=12";

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".into(),
        project_id: "project-a".into(),
        state_epoch: 7,
        log_id: "log-a".into(),
    }
}

fn notification(sequence: u64, kind: HealthNotificationKind) -> HealthNotification {
    HealthNotification {
        sequence,
        rule: HealthAlertRule::OutputFailed,
        severity: HealthSeverity::Critical,
        kind,
        at_ms: 4_000 + sequence,
        value_milli: Some(1_000),
    }
}

fn health() -> DaemonHealth {
    DaemonHealth {
        uptime_ms: 60_000,
        cpu_permille: Some(925),
        gpu_permille: Some(0),
        alerts: vec![
            HealthAlert {
                rule: HealthAlertRule::CpuSaturated,
                severity: HealthSeverity::Warning,
                since_ms: 50_000,
                value_milli: Some(925),
            },
            HealthAlert {
                rule: HealthAlertRule::FramesDropped,
                severity: HealthSeverity::Warning,
                since_ms: 59_000,
                value_milli: None,
            },
        ],
        notifications: vec![
            notification(3, HealthNotificationKind::Firing),
            HealthNotification {
                rule: HealthAlertRule::DiskLow,
                severity: HealthSeverity::Info,
                kind: HealthNotificationKind::Reminder,
                value_milli: Some(-5),
                ..notification(4, HealthNotificationKind::Firing)
            },
            notification(9, HealthNotificationKind::Resolved),
        ],
    }
}

fn message(health: DaemonHealth) -> WireMessage {
    WireMessage::HealthStatus(HealthStatusMessage {
        server: server(),
        sequence: 12,
        health,
    })
}

const ALERTS: &str =
    "cpu_saturated%3Awarning%3A50000%3A925%3Bframes_dropped%3Awarning%3A59000%3Anone";
const NOTIFICATIONS: &str = "3%3Aoutput_failed%3Acritical%3Afiring%3A4003%3A1000%3B4%3Adisk_low%3Ainfo%3Areminder%3A4004%3A-5%3B9%3Aoutput_failed%3Acritical%3Aresolved%3A4009%3A1000";

#[test]
fn health_status_is_exact() {
    let full = message(health());
    let line = encode_line(&full).unwrap();
    assert_eq!(
        line,
        format!(
            "{HEAD}\tuptime_ms=60000\tcpu_permille=925\tgpu_permille=0\talerts={ALERTS}\tnotifications={NOTIFICATIONS}\n"
        )
    );
    assert_eq!(decode_line(&line).unwrap(), full);

    // Unavailable utilization omits its field; no alert and no
    // notification leave empty lists.
    let quiet = message(DaemonHealth {
        uptime_ms: 0,
        ..DaemonHealth::default()
    });
    let line = encode_line(&quiet).unwrap();
    assert_eq!(
        line,
        format!("{HEAD}\tuptime_ms=0\talerts=\tnotifications=\n")
    );
    assert_eq!(decode_line(&line).unwrap(), quiet);

    for rule in HealthAlertRule::ALL {
        assert_eq!(HealthAlertRule::from_token(rule.token()), Some(rule));
    }
    let mut sorted = HealthAlertRule::ALL;
    sorted.sort_unstable();
    assert_eq!(sorted, HealthAlertRule::ALL);
    assert_eq!(health().worst_severity(), Some(HealthSeverity::Warning));
    assert!(health().alert(HealthAlertRule::CpuSaturated).is_some());
    assert!(health().alert(HealthAlertRule::DiskLow).is_none());
}

#[test]
fn health_status_refuses_anything_but_the_contract() {
    let line = encode_line(&message(health())).unwrap();
    let refused = |from: &str, to: &str| {
        let changed = line.replacen(from, to, 1);
        assert_ne!(changed, line, "{from} not found");
        decode_line(&changed).unwrap_err()
    };
    let invalid = |field: &'static str| {
        move |error: CodecError| {
            assert!(
                matches!(error, CodecError::InvalidField { field: refused, .. } if refused == field),
                "{error:?}"
            );
        }
    };
    invalid("cpu_permille")(refused("cpu_permille=925", "cpu_permille=1001"));
    invalid("cpu_permille")(refused("cpu_permille=925", "cpu_permille=0925"));
    invalid("gpu_permille")(refused("gpu_permille=0", "gpu_permille=-1"));
    invalid("uptime_ms")(refused("uptime_ms=60000", "uptime_ms=6e4"));
    // An alert or notification after the uptime clock.
    invalid("alerts")(refused("uptime_ms=60000", "uptime_ms=55000"));
    invalid("notifications")(refused("resolved%3A4009", "resolved%3A60001"));
    invalid("alerts")(refused("cpu_saturated%3A", "cpu_overheated%3A"));
    invalid("alerts")(refused("warning%3A50000", "loud%3A50000"));
    invalid("alerts")(refused("%3A925%3B", "%3A+925%3B"));
    invalid("alerts")(refused("%3Anone", "%3Anone%3A1"));
    invalid("notifications")(refused("%3Areminder%3A", "%3Asnoozed%3A"));
    invalid("notifications")(refused("%3A-5%3B", "%3A-0%3B"));
    // Out-of-order entries.
    let swapped = ALERTS.split("%3B").collect::<Vec<_>>();
    invalid("alerts")(refused(ALERTS, &format!("{}%3B{}", swapped[1], swapped[0])));
    invalid("notifications")(refused("4%3Adisk_low", "3%3Adisk_low"));
    // Every field but the optional utilization is required.
    for field in ["uptime_ms", "alerts", "notifications"] {
        let start = line.find(&format!("\t{field}=")).unwrap();
        let end = line[start + 1..].find(['\t', '\n']).unwrap() + start + 1;
        let missing = format!("{}{}", &line[..start], &line[end..]);
        assert_eq!(decode_line(&missing), Err(CodecError::MissingField(field)));
    }

    // Too many notifications are refused on both sides.
    let mut crowded = health();
    crowded.notifications = (1..=u64::try_from(MAX_HEALTH_NOTIFICATIONS + 1).unwrap())
        .map(|sequence| notification(sequence, HealthNotificationKind::Firing))
        .collect();
    assert_eq!(
        encode_line(&message(crowded.clone())),
        Err(CodecError::TooManyItems("notifications"))
    );
    crowded.notifications.pop();
    let line = encode_line(&message(crowded)).unwrap();
    let extra =
        line.replace('\n', "") + "%3B99%3Aoutput_failed%3Acritical%3Afiring%3A4099%3Anone\n";
    assert_eq!(
        decode_line(&extra),
        Err(CodecError::TooManyItems("notifications"))
    );
    // A sample breaking the invariants is never encoded.
    let mut unsorted = health();
    unsorted.alerts.reverse();
    assert!(encode_line(&message(unsorted)).is_err());
}

#[test]
fn diagnostics_response_carries_the_health_fields() {
    let response = |health| {
        WireMessage::DiagnosticsResponse(DiagnosticsResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "diag-1".into(),
            engine: EngineIdentity {
                engine_id: "engine-a".into(),
                state_epoch: 7,
                log_id: "log-a".into(),
            },
            current_revision: 3,
            oldest_retained_revision: Some(1),
            newest_retained_revision: Some(3),
            subscriber_count: 1,
            retained_events_limit: 128,
            subscriber_limit: 16,
            subscriber_queue_limit: 64,
            source_sync: Vec::new(),
            health,
        })
    };
    let full = response(health());
    let line = encode_line(&full).unwrap();
    assert!(
        line.ends_with(&format!(
            "\tsource_sync=\tuptime_ms=60000\tcpu_permille=925\tgpu_permille=0\talerts={ALERTS}\tnotifications={NOTIFICATIONS}\n"
        )),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), full);
    let quiet = response(DaemonHealth::default());
    let line = encode_line(&quiet).unwrap();
    assert!(
        line.ends_with("\tsource_sync=\tuptime_ms=0\talerts=\tnotifications=\n"),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), quiet);
    let missing = line.replace("\talerts=", "");
    assert_eq!(
        decode_line(&missing),
        Err(CodecError::MissingField("alerts"))
    );
}
