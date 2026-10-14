use std::num::{NonZeroU64, NonZeroU128};

use fm_social::{
    AccountId, Actor, AuditAction, AuditResult, AuthorId, ContentPolicy, DeskCommand, DeskConfig,
    DeskConfigError, DeskError, IncomingMessage, MessageId, MessageKey, ModerationDesk,
    ModerationState, OnAirSource, OperatorId, ProviderId, Role, SocialAggregator, SocialError,
    Timestamp, TitleMappingError, TitleSourceField, TitleSyncError, TypedTitleBinding,
    TypedTitleMapping,
};
use fm_titles::{
    FieldDefinition, FieldId, FieldValue, TemplateId, TitleId, TitleTemplate, UpdateError,
};

const ADMIN: &str = "admin";
const PRODUCER: &str = "producer";
const MODERATOR: &str = "moderator";
const VIEWER: &str = "viewer";

fn op(id: &str) -> OperatorId {
    OperatorId::from(id)
}

fn message(provider: &str, id: &str, author: &str, body: &str, at: u64) -> IncomingMessage {
    IncomingMessage {
        provider_id: ProviderId::from(provider),
        account_id: AccountId::from("channel"),
        message_id: MessageId::from(id),
        author_id: AuthorId::from(author),
        author_name: format!("Name {author}"),
        author_handle: Some(format!("@{author}")),
        body: body.to_owned(),
        created_at: Timestamp(at),
        expires_at: None,
    }
}

fn config(dwell: u64) -> DeskConfig {
    DeskConfig {
        audit_capacity: 64,
        rotation_capacity: 3,
        max_operators: 4,
        rotation_dwell: NonZeroU64::new(dwell).unwrap(),
    }
}

/// A desk with admin, producer, moderator, and viewer operators.
fn staffed_desk(config: DeskConfig) -> ModerationDesk {
    let social = SocialAggregator::new(8, ContentPolicy::default());
    let mut desk = ModerationDesk::new(social, config, ADMIN).unwrap();
    for (id, role) in [
        (PRODUCER, Role::Producer),
        (MODERATOR, Role::Moderator),
        (VIEWER, Role::Viewer),
    ] {
        desk.execute(
            &op(ADMIN),
            DeskCommand::SetRole {
                operator: op(id),
                role,
            },
            Timestamp(0),
        )
        .unwrap();
    }
    desk
}

fn ingest_approved(desk: &mut ModerationDesk, incoming: IncomingMessage, now: u64) -> MessageKey {
    let key = incoming.key();
    desk.ingest(incoming, Timestamp(now)).unwrap();
    desk.execute(
        &op(MODERATOR),
        DeskCommand::Approve(key.clone()),
        Timestamp(now),
    )
    .unwrap();
    key
}

fn field(value: u128) -> FieldId {
    FieldId::new(NonZeroU128::new(value).unwrap())
}

#[test]
fn config_capacities_must_be_non_zero() {
    let social = || SocialAggregator::new(1, ContentPolicy::default());
    let mut zero_audit = config(1);
    zero_audit.audit_capacity = 0;
    assert_eq!(
        ModerationDesk::new(social(), zero_audit, ADMIN).err(),
        Some(DeskConfigError::ZeroAuditCapacity)
    );
    let mut zero_rotation = config(1);
    zero_rotation.rotation_capacity = 0;
    assert_eq!(
        ModerationDesk::new(social(), zero_rotation, ADMIN).err(),
        Some(DeskConfigError::ZeroRotationCapacity)
    );
    let mut zero_operators = config(1);
    zero_operators.max_operators = 0;
    assert_eq!(
        ModerationDesk::new(social(), zero_operators, ADMIN).err(),
        Some(DeskConfigError::ZeroOperatorCapacity)
    );
}

#[test]
fn roles_gate_commands_and_denials_are_audited_without_effect() {
    let mut desk = staffed_desk(config(1_000));
    let incoming = message("twitch", "m1", "alice", "hello", 1);
    let key = incoming.key();
    desk.ingest(incoming, Timestamp(1)).unwrap();

    let denied = desk.execute(&op(VIEWER), DeskCommand::Approve(key.clone()), Timestamp(2));
    assert_eq!(
        denied,
        Err(DeskError::PermissionDenied {
            operator: op(VIEWER),
            role: Role::Viewer,
            required: Role::Moderator,
        })
    );
    assert_eq!(
        desk.execute(&op("stranger"), DeskCommand::ClearOnAir, Timestamp(2)),
        Err(DeskError::UnknownOperator(op("stranger")))
    );
    assert_eq!(
        desk.social().message(&key).unwrap().moderation,
        ModerationState::Pending
    );

    desk.execute(
        &op(MODERATOR),
        DeskCommand::Approve(key.clone()),
        Timestamp(3),
    )
    .unwrap();
    assert_eq!(
        desk.execute(
            &op(MODERATOR),
            DeskCommand::Feature(key.clone()),
            Timestamp(3)
        ),
        Err(DeskError::PermissionDenied {
            operator: op(MODERATOR),
            role: Role::Moderator,
            required: Role::Producer,
        })
    );
    desk.execute(
        &op(PRODUCER),
        DeskCommand::Feature(key.clone()),
        Timestamp(4),
    )
    .unwrap();

    let tail: Vec<_> = desk.audit_after(3).collect();
    assert_eq!(tail.len(), 5);
    assert_eq!(tail[0].sequence, 4);
    assert_eq!(tail[0].actor, Actor::Operator(op(VIEWER)));
    assert!(matches!(tail[0].result, AuditResult::Denied(_)));
    assert!(matches!(tail[1].result, AuditResult::Denied(_)));
    assert_eq!(
        tail[2].action,
        AuditAction::Command(DeskCommand::Approve(key.clone()))
    );
    assert_eq!(tail[2].result, AuditResult::Applied);
    assert!(matches!(tail[3].result, AuditResult::Denied(_)));
    assert_eq!(tail[4].at, Timestamp(4));
    assert_eq!(tail[4].result, AuditResult::Applied);
}

#[test]
fn invalid_authorized_commands_fail_and_are_audited() {
    let mut desk = staffed_desk(config(1_000));
    let incoming = message("youtube", "m1", "bob", "pending", 1);
    let key = incoming.key();
    desk.ingest(incoming, Timestamp(1)).unwrap();

    assert_eq!(
        desk.execute(
            &op(PRODUCER),
            DeskCommand::Feature(key.clone()),
            Timestamp(2)
        ),
        Err(DeskError::NotApproved(key.clone()))
    );
    let missing = MessageKey::new("youtube", "channel", "missing");
    assert_eq!(
        desk.execute(
            &op(MODERATOR),
            DeskCommand::Approve(missing.clone()),
            Timestamp(2)
        ),
        Err(DeskError::Social(SocialError::MessageNotFound(missing)))
    );
    let last = desk.audit().last().unwrap();
    assert!(matches!(
        last.result,
        AuditResult::Failed(DeskError::Social(_))
    ));
    assert!(desk.on_air().is_none());
}

#[test]
fn hide_takes_message_off_air_and_out_of_rotation() {
    let mut desk = staffed_desk(config(1_000));
    let first = ingest_approved(&mut desk, message("bluesky", "a", "ann", "one", 1), 1);
    let second = ingest_approved(&mut desk, message("bluesky", "b", "ben", "two", 2), 2);
    for key in [&first, &second] {
        desk.execute(
            &op(PRODUCER),
            DeskCommand::AddToRotation(key.clone()),
            Timestamp(3),
        )
        .unwrap();
    }
    assert_eq!(desk.tick(Timestamp(3)).unwrap(), Some(first.clone()));
    let generation = desk.on_air_generation();

    desk.execute(
        &op(MODERATOR),
        DeskCommand::Hide(first.clone()),
        Timestamp(4),
    )
    .unwrap();
    assert!(desk.on_air().is_none());
    assert!(desk.is_hidden(&first));
    assert_eq!(desk.rotation(), std::slice::from_ref(&second));
    assert_eq!(desk.on_air_generation(), generation + 1);

    for command in [
        DeskCommand::Feature(first.clone()),
        DeskCommand::AddToRotation(first.clone()),
    ] {
        assert_eq!(
            desk.execute(&op(PRODUCER), command, Timestamp(5)),
            Err(DeskError::Hidden(first.clone()))
        );
    }
    desk.execute(
        &op(MODERATOR),
        DeskCommand::Unhide(first.clone()),
        Timestamp(6),
    )
    .unwrap();
    assert_eq!(
        desk.execute(
            &op(MODERATOR),
            DeskCommand::Unhide(first.clone()),
            Timestamp(6)
        ),
        Err(DeskError::NotHidden(first.clone()))
    );
    desk.execute(
        &op(PRODUCER),
        DeskCommand::Feature(first.clone()),
        Timestamp(7),
    )
    .unwrap();
    assert_eq!(desk.on_air().unwrap().key, first);
}

#[test]
fn rotation_is_bounded_round_robin_and_manual_feature_pins() {
    let mut desk = staffed_desk(config(100));
    let keys: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .enumerate()
        .map(|(index, id)| {
            ingest_approved(
                &mut desk,
                message("irc", id, id, id, index as u64),
                index as u64,
            )
        })
        .collect();
    for key in &keys[..3] {
        desk.execute(
            &op(PRODUCER),
            DeskCommand::AddToRotation(key.clone()),
            Timestamp(10),
        )
        .unwrap();
    }
    assert_eq!(
        desk.execute(
            &op(PRODUCER),
            DeskCommand::AddToRotation(keys[3].clone()),
            Timestamp(10)
        ),
        Err(DeskError::RotationFull { capacity: 3 })
    );
    assert_eq!(
        desk.execute(
            &op(PRODUCER),
            DeskCommand::AddToRotation(keys[0].clone()),
            Timestamp(10)
        ),
        Err(DeskError::AlreadyInRotation(keys[0].clone()))
    );

    assert_eq!(desk.tick(Timestamp(10)).unwrap(), Some(keys[0].clone()));
    assert_eq!(desk.tick(Timestamp(109)).unwrap(), None);
    assert_eq!(desk.tick(Timestamp(110)).unwrap(), Some(keys[1].clone()));

    // Removing an entry before the cursor keeps the same next entry.
    desk.execute(
        &op(PRODUCER),
        DeskCommand::RemoveFromRotation(keys[0].clone()),
        Timestamp(120),
    )
    .unwrap();
    assert_eq!(desk.tick(Timestamp(210)).unwrap(), Some(keys[2].clone()));
    assert_eq!(desk.tick(Timestamp(310)).unwrap(), Some(keys[1].clone()));

    desk.execute(
        &op(PRODUCER),
        DeskCommand::Feature(keys[3].clone()),
        Timestamp(320),
    )
    .unwrap();
    assert_eq!(desk.on_air().unwrap().source, OnAirSource::Manual);
    assert_eq!(desk.tick(Timestamp(10_000)).unwrap(), None);
    assert_eq!(desk.on_air().unwrap().key, keys[3]);

    desk.execute(&op(PRODUCER), DeskCommand::ClearOnAir, Timestamp(10_001))
        .unwrap();
    assert_eq!(desk.tick(Timestamp(10_001)).unwrap(), Some(keys[2].clone()));
    assert_eq!(desk.on_air().unwrap().source, OnAirSource::Rotation);

    let system_entries = desk
        .audit()
        .filter(|entry| entry.actor == Actor::System)
        .count();
    assert_eq!(system_entries, 5);
}

#[test]
fn block_and_admin_only_unblock() {
    let mut desk = staffed_desk(config(100));
    let first = message("facebook", "m1", "troll", "spam", 1);
    let first_key = first.key();
    desk.ingest(first, Timestamp(1)).unwrap();
    desk.execute(&op(MODERATOR), DeskCommand::Block(first_key), Timestamp(2))
        .unwrap();

    let later = message("facebook", "m2", "troll", "more", 3);
    let later_key = later.key();
    desk.ingest(later, Timestamp(3)).unwrap();
    assert_eq!(
        desk.social().message(&later_key).unwrap().moderation,
        ModerationState::Blocked
    );

    let unblock = DeskCommand::Unblock {
        provider_id: ProviderId::from("facebook"),
        account_id: AccountId::from("channel"),
        author_id: AuthorId::from("troll"),
    };
    assert!(matches!(
        desk.execute(&op(PRODUCER), unblock.clone(), Timestamp(4)),
        Err(DeskError::PermissionDenied { .. })
    ));
    desk.execute(&op(ADMIN), unblock.clone(), Timestamp(4))
        .unwrap();
    assert_eq!(
        desk.execute(&op(ADMIN), unblock, Timestamp(4)),
        Err(DeskError::AuthorNotBlocked)
    );

    let after = message("facebook", "m3", "troll", "sorry", 5);
    let after_key = after.key();
    desk.ingest(after, Timestamp(5)).unwrap();
    assert_eq!(
        desk.social().message(&after_key).unwrap().moderation,
        ModerationState::Pending
    );
}

#[test]
fn operator_management_is_bounded_and_keeps_an_admin() {
    let mut desk = staffed_desk(config(100));
    assert_eq!(
        desk.execute(
            &op(ADMIN),
            DeskCommand::SetRole {
                operator: op("fifth"),
                role: Role::Viewer,
            },
            Timestamp(1),
        ),
        Err(DeskError::OperatorLimit { capacity: 4 })
    );
    assert_eq!(
        desk.execute(
            &op(ADMIN),
            DeskCommand::SetRole {
                operator: op(ADMIN),
                role: Role::Producer,
            },
            Timestamp(1),
        ),
        Err(DeskError::LastAdmin(op(ADMIN)))
    );
    assert_eq!(
        desk.execute(
            &op(ADMIN),
            DeskCommand::RemoveOperator(op(ADMIN)),
            Timestamp(1)
        ),
        Err(DeskError::LastAdmin(op(ADMIN)))
    );

    desk.execute(
        &op(ADMIN),
        DeskCommand::RemoveOperator(op(VIEWER)),
        Timestamp(2),
    )
    .unwrap();
    desk.execute(
        &op(ADMIN),
        DeskCommand::SetRole {
            operator: op(PRODUCER),
            role: Role::Admin,
        },
        Timestamp(2),
    )
    .unwrap();
    desk.execute(
        &op(PRODUCER),
        DeskCommand::RemoveOperator(op(ADMIN)),
        Timestamp(3),
    )
    .unwrap();
    assert_eq!(desk.role(&op(ADMIN)), None);
    assert_eq!(desk.role(&op(PRODUCER)), Some(Role::Admin));
    assert_eq!(desk.operators().len(), 2);
}

#[test]
fn audit_log_is_a_bounded_ring_with_stable_sequences() {
    let mut cfg = config(100);
    cfg.audit_capacity = 4;
    let mut desk = staffed_desk(cfg);
    for now in 1..=5 {
        desk.execute(&op(PRODUCER), DeskCommand::ClearOnAir, Timestamp(now))
            .unwrap();
    }
    // Three role grants plus five commands.
    let sequences: Vec<_> = desk.audit().map(|entry| entry.sequence).collect();
    assert_eq!(sequences, [5, 6, 7, 8]);
    assert_eq!(desk.evicted_audit_entries(), 4);
    assert_eq!(desk.audit_after(6).count(), 2);
}

#[test]
fn clock_regression_is_rejected_without_audit() {
    let mut desk = staffed_desk(config(100));
    desk.execute(&op(PRODUCER), DeskCommand::ClearOnAir, Timestamp(50))
        .unwrap();
    let before = desk.audit().len();
    assert_eq!(
        desk.execute(&op(PRODUCER), DeskCommand::ClearOnAir, Timestamp(49)),
        Err(DeskError::ClockRegression {
            last: Timestamp(50),
            now: Timestamp(49),
        })
    );
    assert_eq!(
        desk.tick(Timestamp(10)),
        Err(DeskError::ClockRegression {
            last: Timestamp(50),
            now: Timestamp(10),
        })
    );
    assert_eq!(desk.audit().len(), before);
}

fn lower_third() -> TitleTemplate {
    let text = |id, name: &str| FieldDefinition {
        id: field(id),
        name: name.to_owned(),
        default: FieldValue::Text(String::new()),
    };
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "social lower third".to_owned(),
        width: 640,
        height: 120,
        background: fm_titles::Color::TRANSPARENT,
        fields: vec![
            text(1, "name"),
            text(2, "body"),
            text(3, "provider"),
            FieldDefinition {
                id: field(4),
                name: "created".to_owned(),
                default: FieldValue::Number(0),
            },
        ],
        elements: Vec::new(),
    }
}

fn mapping() -> TypedTitleMapping {
    let bind = |id, source, max_chars| TypedTitleBinding {
        field: field(id),
        source,
        max_chars,
    };
    TypedTitleMapping {
        bindings: vec![
            bind(1, TitleSourceField::AuthorName, None),
            bind(2, TitleSourceField::Body, Some(5)),
            bind(3, TitleSourceField::ProviderId, None),
            bind(4, TitleSourceField::CreatedAt, None),
        ],
    }
}

#[test]
fn on_air_message_drives_typed_title_fields() {
    let mut desk = staffed_desk(config(100));
    let mut scene = lower_third()
        .instantiate(TitleId::new(NonZeroU128::new(9).unwrap()))
        .unwrap();
    let key = ingest_approved(
        &mut desk,
        message("zoom", "z1", "zoe", "h\u{e9}llo world", 42),
        42,
    );
    desk.execute(&op(PRODUCER), DeskCommand::Feature(key), Timestamp(43))
        .unwrap();

    let revision = desk.apply_on_air_to_scene(&mapping(), &mut scene).unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        scene.field_value(field(1)),
        Some(&FieldValue::Text("Name zoe".to_owned()))
    );
    assert_eq!(
        scene.field_value(field(2)),
        Some(&FieldValue::Text("h\u{e9}llo".to_owned()))
    );
    assert_eq!(
        scene.field_value(field(3)),
        Some(&FieldValue::Text("zoom".to_owned()))
    );
    assert_eq!(scene.field_value(field(4)), Some(&FieldValue::Number(42)));

    desk.execute(&op(PRODUCER), DeskCommand::ClearOnAir, Timestamp(44))
        .unwrap();
    assert_eq!(desk.apply_on_air_to_scene(&mapping(), &mut scene), Ok(2));
    assert_eq!(
        scene.field_value(field(1)),
        Some(&FieldValue::Text(String::new()))
    );
    assert_eq!(scene.field_value(field(4)), Some(&FieldValue::Number(0)));
}

#[test]
fn title_sync_reports_mapping_and_type_errors_without_partial_writes() {
    let mut desk = staffed_desk(config(100));
    let mut scene = lower_third()
        .instantiate(TitleId::new(NonZeroU128::new(9).unwrap()))
        .unwrap();
    let key = ingest_approved(&mut desk, message("irc", "late", "x", "y", u64::MAX), 1);
    desk.execute(&op(PRODUCER), DeskCommand::Feature(key), Timestamp(2))
        .unwrap();
    assert_eq!(
        desk.apply_on_air_to_scene(&mapping(), &mut scene),
        Err(TitleSyncError::Mapping(
            TitleMappingError::TimestampOutOfRange {
                field: field(4),
                value: Timestamp(u64::MAX),
            }
        ))
    );

    let wrong_type = TypedTitleMapping {
        bindings: vec![
            TypedTitleBinding {
                field: field(1),
                source: TitleSourceField::Body,
                max_chars: None,
            },
            TypedTitleBinding {
                field: field(4),
                source: TitleSourceField::AuthorName,
                max_chars: None,
            },
        ],
    };
    assert!(matches!(
        desk.apply_on_air_to_scene(&wrong_type, &mut scene),
        Err(TitleSyncError::Update(UpdateError::TypeMismatch { .. }))
    ));
    assert_eq!(scene.revision(), 0);
    assert_eq!(
        scene.field_value(field(1)),
        Some(&FieldValue::Text(String::new()))
    );
}
