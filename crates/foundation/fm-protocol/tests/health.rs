//! Protocol 2.100 `health_status` and the `diagnostics_response` health
//! fields: the exact lossy daemon utilization and alert record (RC-015).

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, DaemonHealth, DiagnosticsResponse, EngineIdentity,
    HealthAlert, HealthAlertRule, HealthNotification, HealthNotificationKind, HealthSeverity,
    HealthStatusMessage, MAX_HEALTH_NOTIFICATIONS, ProtocolVersion, ServerIdentity, WireMessage,
    decode_line, encode_line,
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
                acknowledged_until_ms: None,
            },
            HealthAlert {
                rule: HealthAlertRule::FramesDropped,
                severity: HealthSeverity::Warning,
                since_ms: 59_000,
                value_milli: None,
                acknowledged_until_ms: None,
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
        silences: Vec::new(),
    }
}

fn message(health: DaemonHealth) -> WireMessage {
    WireMessage::HealthStatus(HealthStatusMessage {
        server: server(),
        sequence: 12,
        request_id: None,
        health,
    })
}

fn diagnostics(health: DaemonHealth) -> WireMessage {
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
}

const ALERTS: &str =
    "cpu_saturated%3Awarning%3A50000%3A925%3Anone%3Bframes_dropped%3Awarning%3A59000%3Anone%3Anone";
const NOTIFICATIONS: &str = "3%3Aoutput_failed%3Acritical%3Afiring%3A4003%3A1000%3B4%3Adisk_low%3Ainfo%3Areminder%3A4004%3A-5%3B9%3Aoutput_failed%3Acritical%3Aresolved%3A4009%3A1000";

#[test]
fn health_status_is_exact() {
    let full = message(health());
    let line = encode_line(&full).unwrap();
    assert_eq!(
        line,
        format!(
            "{HEAD}\tuptime_ms=60000\tcpu_permille=925\tgpu_permille=0\talerts={ALERTS}\tnotifications={NOTIFICATIONS}\tsilences=\n"
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
        format!("{HEAD}\tuptime_ms=0\talerts=\tnotifications=\tsilences=\n")
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
    invalid("alerts")(refused("%3A925%3Anone", "%3A+925%3Anone"));
    invalid("alerts")(refused("%3Anone", "%3Anone%3A1"));
    invalid("notifications")(refused("%3Areminder%3A", "%3Asnoozed%3A"));
    invalid("notifications")(refused("%3A-5%3B", "%3A-0%3B"));
    // Out-of-order entries.
    let swapped = ALERTS.split("%3B").collect::<Vec<_>>();
    invalid("alerts")(refused(ALERTS, &format!("{}%3B{}", swapped[1], swapped[0])));
    invalid("notifications")(refused("4%3Adisk_low", "3%3Adisk_low"));
    // Every field but the optional utilization is required.
    for field in ["uptime_ms", "alerts", "notifications", "silences"] {
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
    let extra = line.replace(
        "\tsilences=",
        "%3B99%3Aoutput_failed%3Acritical%3Afiring%3A4099%3Anone\tsilences=",
    );
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
    let full = diagnostics(health());
    let line = encode_line(&full).unwrap();
    assert!(
        line.ends_with(&format!(
            "\tsource_sync=\tuptime_ms=60000\tcpu_permille=925\tgpu_permille=0\talerts={ALERTS}\tnotifications={NOTIFICATIONS}\tsilences=\n"
        )),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), full);
    let quiet = diagnostics(DaemonHealth::default());
    let line = encode_line(&quiet).unwrap();
    assert!(
        line.ends_with("\tsource_sync=\tuptime_ms=0\talerts=\tnotifications=\tsilences=\n"),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), quiet);
    let missing = line.replace("\talerts=", "");
    assert_eq!(
        decode_line(&missing),
        Err(CodecError::MissingField("alerts"))
    );
}

#[test]
fn new_alert_rule_tokens_round_trip() {
    for (rule, token) in [
        (HealthAlertRule::EncoderSaturated, "encoder_saturated"),
        (HealthAlertRule::StreamReconnecting, "stream_reconnecting"),
        (HealthAlertRule::SyncDegraded, "sync_degraded"),
    ] {
        assert_eq!(rule.token(), token);
        assert_eq!(HealthAlertRule::from_token(token), Some(rule));
    }
    for unknown in [
        "encoder_overheated",
        "stream_reconnect",
        "sync_degraded ",
        "",
    ] {
        assert_eq!(HealthAlertRule::from_token(unknown), None);
    }
}

#[test]
fn all_alert_rules_stay_sorted_and_unique() {
    assert_eq!(HealthAlertRule::ALL.len(), 9);
    assert!(
        HealthAlertRule::ALL
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    let mut sorted = HealthAlertRule::ALL;
    sorted.sort_unstable();
    assert_eq!(sorted, HealthAlertRule::ALL);
    for (index, rule) in HealthAlertRule::ALL.iter().enumerate() {
        assert!(
            HealthAlertRule::ALL[index + 1..]
                .iter()
                .all(|other| other.token() != rule.token()),
            "{}",
            rule.token()
        );
    }
}

const NEW_ALERTS: &str = "encoder_saturated%3Awarning%3A61000%3A3500%3Anone%3Bstream_reconnecting%3Awarning%3A62000%3A1000%3Anone%3Bsync_degraded%3Awarning%3A63000%3Anone%3Anone";
const NEW_NOTIFICATIONS: &str = "1%3Async_degraded%3Awarning%3Afiring%3A63000%3Anone";

fn new_rule_health() -> DaemonHealth {
    DaemonHealth {
        uptime_ms: 64_000,
        cpu_permille: Some(500),
        gpu_permille: None,
        alerts: vec![
            HealthAlert {
                rule: HealthAlertRule::EncoderSaturated,
                severity: HealthSeverity::Warning,
                since_ms: 61_000,
                value_milli: Some(3_500),
                acknowledged_until_ms: None,
            },
            HealthAlert {
                rule: HealthAlertRule::StreamReconnecting,
                severity: HealthSeverity::Warning,
                since_ms: 62_000,
                value_milli: Some(1_000),
                acknowledged_until_ms: None,
            },
            HealthAlert {
                rule: HealthAlertRule::SyncDegraded,
                severity: HealthSeverity::Warning,
                since_ms: 63_000,
                value_milli: None,
                acknowledged_until_ms: None,
            },
        ],
        notifications: vec![HealthNotification {
            sequence: 1,
            rule: HealthAlertRule::SyncDegraded,
            severity: HealthSeverity::Warning,
            kind: HealthNotificationKind::Firing,
            at_ms: 63_000,
            value_milli: None,
        }],
        silences: Vec::new(),
    }
}

#[test]
fn health_status_and_diagnostics_carry_the_new_alert_rules() {
    let health = new_rule_health();
    for rule in [
        HealthAlertRule::EncoderSaturated,
        HealthAlertRule::StreamReconnecting,
        HealthAlertRule::SyncDegraded,
    ] {
        assert!(health.alert(rule).is_some(), "{}", rule.token());
    }
    let full = message(health.clone());
    let line = encode_line(&full).unwrap();
    assert_eq!(
        line,
        format!(
            "{HEAD}\tuptime_ms=64000\tcpu_permille=500\talerts={NEW_ALERTS}\tnotifications={NEW_NOTIFICATIONS}\tsilences=\n"
        )
    );
    assert_eq!(decode_line(&line).unwrap(), full);
    let misspelled = line.replacen("sync_degraded%3A", "sync_impaired%3A", 1);
    assert!(
        matches!(
            decode_line(&misspelled),
            Err(CodecError::InvalidField {
                field: "alerts",
                ..
            })
        ),
        "{misspelled}"
    );

    let full = diagnostics(health);
    let line = encode_line(&full).unwrap();
    assert!(
        line.ends_with(&format!(
            "\tsource_sync=\tuptime_ms=64000\tcpu_permille=500\talerts={NEW_ALERTS}\tnotifications={NEW_NOTIFICATIONS}\tsilences=\n"
        )),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), full);
}

#[test]
fn alerts_carry_one_entry_per_rule_up_to_nine() {
    let mut full = health();
    full.uptime_ms = 60_000;
    full.alerts = HealthAlertRule::ALL
        .iter()
        .copied()
        .map(|rule| HealthAlert {
            rule,
            severity: HealthSeverity::Warning,
            since_ms: 50_000,
            value_milli: None,
            acknowledged_until_ms: None,
        })
        .collect();
    assert!(full.is_valid());
    let full = message(full);
    let line = encode_line(&full).unwrap();
    assert_eq!(decode_line(&line).unwrap(), full);
    let extra = line.replace(
        "\tnotifications=",
        "%3Bcpu_saturated%3Awarning%3A50000%3Anone%3Anone\tnotifications=",
    );
    assert_eq!(decode_line(&extra), Err(CodecError::TooManyItems("alerts")));
}

#[test]
fn protocol_is_current() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 113));
}

/// Protocol 2.109: an acknowledged alert carries its end, silences are
/// listed per rule, and both must end after `uptime_ms`.
#[test]
fn acknowledgements_and_silences_are_exact() {
    let mut acknowledged = health();
    acknowledged.alerts[0].acknowledged_until_ms = Some(660_000);
    acknowledged.silences = vec![
        fm_protocol::HealthSilence {
            rule: HealthAlertRule::DiskLow,
            until_ms: 120_000,
        },
        fm_protocol::HealthSilence {
            rule: HealthAlertRule::OutputFailed,
            until_ms: 60_001,
        },
    ];
    assert!(acknowledged.is_valid());
    assert_eq!(
        acknowledged
            .silence(HealthAlertRule::DiskLow)
            .map(|s| s.until_ms),
        Some(120_000)
    );
    let full = message(acknowledged.clone());
    let line = encode_line(&full).unwrap();
    assert!(
        line.contains("\talerts=cpu_saturated%3Awarning%3A50000%3A925%3A660000%3B"),
        "{line}"
    );
    assert!(
        line.ends_with("\tsilences=disk_low%3A120000%3Boutput_failed%3A60001\n"),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), full);

    let refused = |from: &str, to: &str| {
        let changed = line.replacen(from, to, 1);
        assert_ne!(changed, line, "{from} not found");
        decode_line(&changed).unwrap_err()
    };
    let field_of = |error: CodecError| match error {
        CodecError::InvalidField { field, .. } => field,
        error => panic!("{error:?}"),
    };
    // An end at or before the uptime clock, or not canonical.
    assert_eq!(field_of(refused("%3A660000%3B", "%3A60000%3B")), "alerts");
    assert_eq!(field_of(refused("%3A660000%3B", "%3A0660000%3B")), "alerts");
    assert_eq!(field_of(refused("%3A60001\n", "%3A60000\n")), "silences");
    // Unknown, repeated, or unordered rules, and malformed entries.
    assert_eq!(
        field_of(refused("silences=disk_low%3A", "silences=disk_full%3A")),
        "silences"
    );
    assert_eq!(
        field_of(refused("output_failed%3A60001", "disk_low%3A60001")),
        "silences"
    );
    assert_eq!(
        field_of(refused(
            "disk_low%3A120000%3Boutput_failed%3A60001",
            "output_failed%3A60001%3Bdisk_low%3A120000"
        )),
        "silences"
    );
    assert_eq!(
        field_of(refused("disk_low%3A120000", "disk_low")),
        "silences"
    );
    // A sample breaking the invariants is never encoded.
    let mut expired = acknowledged;
    expired.silences[1].until_ms = expired.uptime_ms;
    assert!(!expired.is_valid());
    assert!(encode_line(&message(expired)).is_err());

    // The answer to a request echoes its id.
    let answer = WireMessage::HealthStatus(HealthStatusMessage {
        request_id: Some("ack-7".into()),
        ..HealthStatusMessage {
            server: server(),
            sequence: 12,
            request_id: None,
            health: health(),
        }
    });
    let line = encode_line(&answer).unwrap();
    assert!(
        line.starts_with(&format!("{HEAD}\trequest_id=ack-7\t")),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), answer);
}

/// Protocol 2.109: `health_alert_request` carries a hold exactly for the
/// actions that hold, and only within a day.
#[test]
fn health_alert_request_is_exact() {
    use fm_protocol::{HealthAlertAction, HealthAlertRequest, MAX_HEALTH_ALERT_HOLD_MS};
    let request = |action| {
        WireMessage::HealthAlertRequest(HealthAlertRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "ack-1".into(),
            rule: HealthAlertRule::DiskLow,
            action,
        })
    };
    for (action, tail) in [
        (
            HealthAlertAction::Acknowledge {
                duration_ms: 600_000,
            },
            "action=acknowledge\tduration_ms=600000",
        ),
        (
            HealthAlertAction::Silence {
                duration_ms: MAX_HEALTH_ALERT_HOLD_MS,
            },
            "action=silence\tduration_ms=86400000",
        ),
        (HealthAlertAction::Unsilence, "action=unsilence"),
    ] {
        let full = request(action);
        let line = encode_line(&full).unwrap();
        assert_eq!(
            line,
            format!(
                "health_alert_request\tprotocol={CURRENT_PROTOCOL_VERSION}\trequest_id=ack-1\t\
                 rule=disk_low\t{tail}\n"
            )
        );
        assert_eq!(decode_line(&line).unwrap(), full);
    }
    let line = encode_line(&request(HealthAlertAction::Acknowledge { duration_ms: 1 })).unwrap();
    let refused = |from: &str, to: &str| {
        let changed = line.replacen(from, to, 1);
        assert_ne!(changed, line, "{from} not found");
        decode_line(&changed).unwrap_err()
    };
    let field_of = |error: CodecError| match error {
        CodecError::InvalidField { field, .. } => field,
        error => panic!("{error:?}"),
    };
    assert_eq!(
        field_of(refused("duration_ms=1", "duration_ms=0")),
        "action"
    );
    assert_eq!(
        field_of(refused("duration_ms=1", "duration_ms=86400001")),
        "action"
    );
    assert_eq!(
        field_of(refused("duration_ms=1", "duration_ms=01")),
        "duration_ms"
    );
    assert_eq!(field_of(refused("\tduration_ms=1", "")), "action");
    assert_eq!(
        field_of(refused(
            "action=acknowledge\tduration_ms=1",
            "action=unsilence\tduration_ms=1"
        )),
        "action"
    );
    assert_eq!(
        field_of(refused("action=acknowledge", "action=snooze")),
        "action"
    );
    assert_eq!(field_of(refused("rule=disk_low", "rule=disk_full")), "rule");
    assert_eq!(
        decode_line(&line.replacen("\trule=disk_low", "", 1)),
        Err(CodecError::MissingField("rule"))
    );
    // An action outside the contract is never encoded.
    assert!(encode_line(&request(HealthAlertAction::Silence { duration_ms: 0 })).is_err());
}

/// Protocol 2.112: `alert_rule_set` carries one rule's setting and
/// `alert_rules_changed` the show's whole list, both exact and strict.
#[test]
#[allow(clippy::too_many_lines)] // the command, then the event, each strict
fn alert_rule_settings_have_an_exact_wire_form() {
    use fm_protocol::{
        AlertRuleSetting, CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload,
        EditHistoryStatus, EngineIdentity, EventCursor, EventMessage, EventPayload,
    };

    let cpu = AlertRuleSetting {
        trigger_milli: 800,
        clear_milli: 600,
        hold_ms: 5_000,
        ..AlertRuleSetting::default_for(HealthAlertRule::CpuSaturated)
    };
    let off = AlertRuleSetting {
        enabled: false,
        ..AlertRuleSetting::default_for(HealthAlertRule::OutputFailed)
    };
    let command = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".into(),
        idempotency_key: "k".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SetAlertRule(cpu),
    });
    let line = encode_line(&command).unwrap();
    assert_eq!(
        line,
        format!(
            "command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid=a\tidempotency_key=k\t\
             payload=alert_rule_set\trule=cpu_saturated\tenabled=1\ttrigger=800\tclear=600\t\
             hold_ms=5000\n"
        )
    );
    assert_eq!(decode_line(&line).unwrap(), command);
    // The disk rule's thresholds are signed milli bytes.
    let disk = AlertRuleSetting {
        trigger_milli: 5_368_709_120_000,
        clear_milli: 10_737_418_240_000,
        ..AlertRuleSetting::default_for(HealthAlertRule::DiskLow)
    };
    let disk_line = encode_line(&WireMessage::Command(CommandMessage {
        payload: CommandPayload::SetAlertRule(disk),
        ..match &command {
            WireMessage::Command(message) => message.clone(),
            _ => unreachable!(),
        }
    }))
    .unwrap();
    assert!(
        disk_line.contains("\ttrigger=5368709120000\t"),
        "{disk_line}"
    );
    for (from, to) in [
        ("rule=cpu_saturated", "rule=cpu_hot"),
        ("enabled=1", "enabled=yes"),
        ("trigger=800", "trigger=0800"),
        ("trigger=800", "trigger=+800"),
        ("clear=600", "clear=900"),
        ("trigger=800", "trigger=1001"),
        ("hold_ms=5000", "hold_ms=3600001"),
        ("\thold_ms=5000", ""),
    ] {
        assert!(line.contains(from), "{from}");
        assert!(
            decode_line(&line.replacen(from, to, 1)).is_err(),
            "{from} -> {to}"
        );
    }
    // A setting without a band is refused at encode time too.
    assert!(
        encode_line(&WireMessage::Command(CommandMessage {
            payload: CommandPayload::SetAlertRule(AlertRuleSetting {
                clear_milli: 900,
                ..cpu
            }),
            ..match &command {
                WireMessage::Command(message) => message.clone(),
                _ => unreachable!(),
            }
        }))
        .is_err()
    );

    let event = |settings: Vec<AlertRuleSetting>| {
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
            payload: EventPayload::AlertRulesChanged(settings),
        })
    };
    let changed = event(vec![cpu, off]);
    let line = encode_line(&changed).unwrap();
    assert!(
        line.contains(
            "\tevent=alert_rules_changed\talert_rules=cpu_saturated%3A1%3A800%3A600%3A5000%3B\
             output_failed%3A0%3A1000%3A0%3A0\t"
        ),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), changed);
    let empty = encode_line(&event(Vec::new())).unwrap();
    assert!(empty.contains("\talert_rules=\t"), "{empty}");
    assert_eq!(decode_line(&empty).unwrap(), event(Vec::new()));
    // Out of order, repeated, or default entries are refused both ways.
    for settings in [
        vec![off, cpu],
        vec![cpu, cpu],
        vec![AlertRuleSetting::default_for(HealthAlertRule::GpuSaturated)],
    ] {
        assert!(encode_line(&event(settings)).is_err());
    }
    for (from, to) in [
        (
            "alert_rules=cpu_saturated%3A1%3A800%3A600%3A5000%3B",
            "alert_rules=",
        ),
        (
            "output_failed%3A0%3A1000%3A0%3A0",
            "output_failed%3A1%3A1000%3A0%3A0",
        ),
        ("%3A5000%3B", "%3A5000%3A1%3B"),
    ] {
        let edited = line.replacen(from, to, 1);
        if from.ends_with("%3B") && to == "alert_rules=" {
            // Dropping the first entry still leaves a valid list.
            assert!(decode_line(&edited).is_ok());
        } else {
            assert!(decode_line(&edited).is_err(), "{from} -> {to}");
        }
    }
}
