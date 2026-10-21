//! Browser title editor (`GX-006`): exposed typed fields, per-field drafts
//! and validation, preview-before-take, role and air-state gating, and
//! optimistic concurrency against a fake authority backed by real
//! `fm_titles::TitleScene` revisions. The editor is keyed on protocol types
//! (title inputs, wire field ids, and wire values); the fake authority
//! converts at its boundary.

use std::collections::BTreeMap;
use std::num::NonZeroU128;

use fm_client::{ConnectionState, Session};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, Role, ServerIdentity, TitleFieldValue, WireTitleFieldId,
};
use fm_titles::{
    Color, FieldDefinition, FieldId, FieldValue, ImageSource, TemplateId, TitleId, TitleScene,
    TitleTemplate, UpdateError,
};
use fm_types::InputId;
use freemix_web::{
    ExposedField, FieldError, FieldType, MAX_EXPOSED_FIELDS, MAX_TITLE_TEXT_CHARS, PreviewSource,
    ReplyOutcome, SnapshotError, TitleAccess, TitleBlocked, TitleCommand, TitleCommandBody,
    TitleControlPort, TitleEditor, TitleEditorError, TitleNotice, TitleOperation, TitleRejection,
    TitleReply, TitleSnapshot, UnexpectedReply, domain_field_id, domain_field_value,
    format_field_value, parse_field_input, scene_snapshot, title_access, wire_field_id,
    wire_field_type, wire_field_value,
};

const LOWER_THIRD: u128 = 1;
const SCOREBUG: u128 = 2;
const NAME: u128 = 10;
const ROLE: u128 = 11;
const SCORE: u128 = 12;
const ACCENT: u128 = 13;
const LOGO: u128 = 14;

/// The input hosting a title.
fn title(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn field(value: u128) -> WireTitleFieldId {
    WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
}

fn text(value: &str) -> TitleFieldValue {
    TitleFieldValue::Text(value.to_owned())
}

fn color(red: u8, green: u8, blue: u8, alpha: u8) -> TitleFieldValue {
    TitleFieldValue::Color {
        red,
        green,
        blue,
        alpha,
    }
}

fn image(asset: &str) -> TitleFieldValue {
    TitleFieldValue::Image {
        asset_uri: asset.to_owned(),
    }
}

fn session(permissions: &[&str]) -> Session {
    Session {
        protocol: CURRENT_PROTOCOL_VERSION,
        granted_role: Role::Graphics,
        permissions: permissions
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        server: ServerIdentity {
            engine_id: "engine-titles".into(),
            project_id: "project-titles".into(),
            state_epoch: 1,
            log_id: "log-titles".into(),
        },
        capabilities_digest: "titles".into(),
    }
}

fn access(permissions: &[&str]) -> TitleAccess {
    title_access(&ConnectionState::Ready, Some(&session(permissions)))
}

/// Admin-like access: may edit and take.
fn producer() -> TitleAccess {
    access(&["view_status", "edit_project", "transition"])
}

fn template(id: u128, fields: Vec<FieldDefinition>) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(id).unwrap()),
        name: format!("template-{id}"),
        width: 1920,
        height: 1080,
        background: Color::TRANSPARENT,
        fields,
        elements: Vec::new(),
        animation: fm_titles::TitleAnimation::default(),
    }
}

fn definition(id: u128, name: &str, default: FieldValue) -> FieldDefinition {
    FieldDefinition {
        id: FieldId::new(NonZeroU128::new(id).unwrap()),
        name: name.to_owned(),
        default,
        exposed: true,
    }
}

struct Hosted {
    name: String,
    template: TitleTemplate,
    scene: TitleScene,
    on_air: bool,
}

/// A deterministic in-process title authority implementing the port
/// contract with the real `fm_titles` optimistic field update.
struct FakeTitleServer {
    titles: BTreeMap<InputId, Hosted>,
    commands: Vec<TitleCommand>,
}

impl FakeTitleServer {
    fn new() -> Self {
        let mut server = Self {
            titles: BTreeMap::new(),
            commands: Vec::new(),
        };
        server.host(
            LOWER_THIRD,
            "Lower third",
            template(
                1,
                vec![
                    definition(NAME, "Name", FieldValue::Text("Ada Lovelace".into())),
                    definition(ROLE, "Role", FieldValue::Text("Analyst".into())),
                    definition(
                        ACCENT,
                        "Accent",
                        FieldValue::Color(Color::new(0, 0, 255, 255)),
                    ),
                    definition(
                        LOGO,
                        "Logo",
                        FieldValue::Image(ImageSource::new("logo.png")),
                    ),
                ],
            ),
        );
        server.host(
            SCOREBUG,
            "Score bug",
            template(
                2,
                vec![definition(SCORE, "Home score", FieldValue::Number(0))],
            ),
        );
        server
    }

    fn host(&mut self, id: u128, name: &str, template: TitleTemplate) {
        let scene = template
            .instantiate(TitleId::new(NonZeroU128::new(id).unwrap()))
            .unwrap();
        self.titles.insert(
            title(id),
            Hosted {
                name: name.to_owned(),
                template,
                scene,
                on_air: false,
            },
        );
    }

    fn snapshot(&self, id: u128) -> TitleSnapshot {
        let hosted = &self.titles[&title(id)];
        scene_snapshot(
            title(id),
            &hosted.name,
            &hosted.template,
            &hosted.scene,
            hosted.on_air,
        )
    }

    fn value(&self, id: u128, field_id: u128) -> TitleFieldValue {
        wire_field_value(
            self.titles[&title(id)]
                .scene
                .field_value(domain_field_id(field(field_id)))
                .unwrap(),
        )
    }

    fn on_air(&self, id: u128) -> bool {
        self.titles[&title(id)].on_air
    }

    fn revision(&self, id: u128) -> u64 {
        self.titles[&title(id)].scene.revision()
    }

    /// An editor that has observed every hosted title.
    fn editor(&self, access: TitleAccess) -> TitleEditor {
        let mut editor = TitleEditor::new(access);
        for id in self.titles.keys() {
            editor.observe(self.snapshot(id.get().get())).unwrap();
        }
        editor
    }
}

impl TitleControlPort for FakeTitleServer {
    fn submit(&mut self, command: TitleCommand) -> TitleReply {
        self.commands.push(command.clone());
        let request = command.request;
        let Some(hosted) = self.titles.get_mut(&command.title) else {
            return TitleReply::Rejected {
                request,
                reason: TitleRejection::UnknownTitle,
                current: None,
            };
        };
        let snapshot = |hosted: &Hosted| {
            scene_snapshot(
                command.title,
                &hosted.name,
                &hosted.template,
                &hosted.scene,
                hosted.on_air,
            )
        };
        let reject = |hosted: &Hosted, reason| TitleReply::Rejected {
            request,
            reason,
            current: Some(snapshot(hosted)),
        };
        let (base_revision, fields, goes_on_air) = match command.body {
            TitleCommandBody::OffAir => {
                if !hosted.on_air {
                    return reject(hosted, TitleRejection::NotOnAir);
                }
                hosted.on_air = false;
                return TitleReply::Applied {
                    request,
                    snapshot: snapshot(hosted),
                };
            }
            TitleCommandBody::Save {
                base_revision,
                fields,
            } => {
                if hosted.on_air {
                    return reject(hosted, TitleRejection::AlreadyOnAir);
                }
                (base_revision, fields, false)
            }
            TitleCommandBody::Take {
                base_revision,
                fields,
                ..
            } => {
                if hosted.on_air {
                    return reject(hosted, TitleRejection::AlreadyOnAir);
                }
                (base_revision, fields, true)
            }
            TitleCommandBody::Update {
                base_revision,
                fields,
            } => {
                if !hosted.on_air {
                    return reject(hosted, TitleRejection::NotOnAir);
                }
                (base_revision, fields, false)
            }
        };
        let domain: Vec<(FieldId, FieldValue)> = fields
            .iter()
            .map(|(field, value)| (domain_field_id(*field), domain_field_value(value)))
            .collect();
        match hosted.scene.update_fields(base_revision, &domain) {
            Ok(_) => {
                if goes_on_air {
                    hosted.on_air = true;
                }
                TitleReply::Applied {
                    request,
                    snapshot: snapshot(hosted),
                }
            }
            Err(UpdateError::RevisionConflict { .. } | UpdateError::StaleField { .. }) => {
                TitleReply::Conflict {
                    request,
                    current: snapshot(hosted),
                }
            }
            Err(error) => reject(hosted, rejection(&error, &fields)),
        }
    }
}

/// The port rejection for a refused `fm_titles` field update.
fn rejection(
    error: &UpdateError,
    fields: &[(WireTitleFieldId, TitleFieldValue)],
) -> TitleRejection {
    match *error {
        UpdateError::UnknownField(field) => TitleRejection::UnknownField(wire_field_id(field)),
        UpdateError::DuplicateField(field) => TitleRejection::DuplicateField(wire_field_id(field)),
        UpdateError::TypeMismatch {
            field,
            expected,
            actual,
        } => TitleRejection::TypeMismatch {
            field: wire_field_id(field),
            expected: wire_field_type(expected),
            actual: wire_field_type(actual),
        },
        UpdateError::EmptyImageAsset(field) | UpdateError::TextTooLong { field, .. } => {
            TitleRejection::InvalidValue(wire_field_id(field))
        }
        UpdateError::ContentTooLarge { .. } => TitleRejection::InvalidValue(fields[0].0),
        UpdateError::RevisionExhausted
        | UpdateError::RevisionConflict { .. }
        | UpdateError::StaleField { .. } => TitleRejection::RevisionExhausted,
    }
}

fn state(editor: &TitleEditor, id: u128, operation: TitleOperation) -> Result<(), TitleBlocked> {
    editor
        .presentation(title(id))
        .unwrap()
        .operations
        .into_iter()
        .find(|presented| presented.operation == operation)
        .unwrap()
        .state
}

fn blocked(reason: TitleBlocked) -> Result<(), TitleBlocked> {
    Err(reason)
}

#[test]
fn lists_titles_with_exposed_typed_fields() {
    let server = FakeTitleServer::new();
    let editor = server.editor(producer());

    let list = editor.list();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "Lower third");
    assert!(!list[0].on_air && !list[0].dirty && !list[0].stale);

    let view = editor.presentation(title(LOWER_THIRD)).unwrap();
    let summary: Vec<_> = view
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.field_type, field.input.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            ("Name", FieldType::Text, "Ada Lovelace"),
            ("Role", FieldType::Text, "Analyst"),
            ("Accent", FieldType::Color, "#0000FFFF"),
            ("Logo", FieldType::Image, "logo.png"),
        ]
    );
    assert!(
        view.fields
            .iter()
            .all(|field| !field.dirty && field.error.is_none())
    );
    assert_eq!(view.live_revision, 0);
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::NoChanges)
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::NotPreviewed)
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::OffAir),
        blocked(TitleBlocked::OffAir)
    );
}

#[test]
fn drafts_validate_per_field_and_track_dirty_state() {
    let server = FakeTitleServer::new();
    let mut editor = server.editor(producer());
    let lt = title(LOWER_THIRD);

    editor.set_input(lt, field(ACCENT), "blue").unwrap();
    editor.set_input(lt, field(LOGO), "   ").unwrap();
    editor
        .set_input(lt, field(NAME), &"x".repeat(MAX_TITLE_TEXT_CHARS + 1))
        .unwrap();
    editor
        .set_input(title(SCOREBUG), field(SCORE), "3 goals")
        .unwrap();

    let view = editor.presentation(lt).unwrap();
    let errors: Vec<_> = view.fields.iter().map(|field| field.error).collect();
    assert_eq!(
        errors,
        [
            Some(FieldError::TextTooLong {
                max_chars: MAX_TITLE_TEXT_CHARS
            }),
            None,
            Some(FieldError::InvalidColor),
            Some(FieldError::EmptyImageAsset),
        ]
    );
    assert!(view.dirty);
    assert_eq!(
        view.fields[2].input, "blue",
        "invalid input is kept as typed"
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::InvalidDraft)
    );
    assert_eq!(
        editor.preview(lt),
        Err(TitleEditorError::Blocked(TitleBlocked::InvalidDraft))
    );
    assert_eq!(
        editor.presentation(title(SCOREBUG)).unwrap().fields[0].error,
        Some(FieldError::InvalidNumber)
    );

    // Fixing every field makes the draft sendable; typing the base value
    // back clears that field's draft.
    editor.set_input(lt, field(ACCENT), "#ff000080").unwrap();
    editor.set_input(lt, field(LOGO), " sponsor.png ").unwrap();
    editor.set_input(lt, field(NAME), "Ada Lovelace").unwrap();
    let view = editor.presentation(lt).unwrap();
    assert!(!view.fields[0].dirty, "base value clears the draft");
    assert!(view.fields[2].dirty && view.fields[2].error.is_none());
    assert_eq!(state(&editor, LOWER_THIRD, TitleOperation::Save), Ok(()));
    editor
        .set_input(title(SCOREBUG), field(SCORE), " 3 ")
        .unwrap();
    assert!(
        editor.presentation(title(SCOREBUG)).unwrap().fields[0]
            .error
            .is_none()
    );

    let preview = editor.preview(lt).unwrap();
    assert_eq!(preview.fields[2].value, color(255, 0, 0, 128));
    assert_eq!(preview.fields[2].source, PreviewSource::Draft);
    assert_eq!(preview.fields[3].value, image("sponsor.png"));
    assert_eq!(preview.fields[0].source, PreviewSource::Live);

    editor.discard_all(lt).unwrap();
    assert!(!editor.presentation(lt).unwrap().dirty);
}

#[test]
fn role_permissions_and_connection_gate_editing_and_air_control() {
    let mut server = FakeTitleServer::new();
    let lt = title(LOWER_THIRD);

    let mut viewer = server.editor(access(&["view_status"]));
    assert_eq!(
        viewer.set_input(lt, field(NAME), "Grace"),
        Err(TitleEditorError::Blocked(TitleBlocked::NotPermitted))
    );
    viewer.preview(lt).unwrap();
    for operation in [TitleOperation::Save, TitleOperation::Take] {
        assert_eq!(
            state(&viewer, LOWER_THIRD, operation),
            blocked(TitleBlocked::NotPermitted)
        );
    }
    assert!(!viewer.presentation(lt).unwrap().can_edit);

    // The production Graphics grant (view_status + edit_project) edits and
    // saves cues but cannot put them on program.
    let mut graphics = server.editor(access(&["view_status", "edit_project"]));
    graphics.set_input(lt, field(NAME), "Grace Hopper").unwrap();
    graphics.preview(lt).unwrap();
    assert_eq!(
        state(&graphics, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::NotPermitted)
    );
    assert_eq!(
        graphics.dispatch(&mut server, lt, TitleOperation::Save),
        Ok(ReplyOutcome::Applied(TitleOperation::Save))
    );
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Grace Hopper"));
    assert!(!server.on_air(LOWER_THIRD));

    // The production Operator grant takes the saved cue but cannot edit,
    // and so cannot take with changes either.
    let mut operator = server.editor(access(&["view_status", "transition"]));
    assert_eq!(
        operator.set_input(lt, field(NAME), "Someone"),
        Err(TitleEditorError::Blocked(TitleBlocked::NotPermitted))
    );
    operator.preview(lt).unwrap();
    assert_eq!(
        operator.dispatch(&mut server, lt, TitleOperation::Take),
        Ok(ReplyOutcome::Applied(TitleOperation::Take))
    );
    assert!(server.on_air(LOWER_THIRD));
    assert_eq!(
        state(&operator, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::NotPermitted)
    );
    assert_eq!(
        state(&operator, LOWER_THIRD, TitleOperation::OffAir),
        Ok(())
    );

    // Disconnected: drafting is kept locally, nothing can be sent.
    let mut offline = server.editor(title_access(
        &ConnectionState::Disconnected,
        Some(&session(&["edit_project", "transition"])),
    ));
    offline.set_input(lt, field(ROLE), "Admiral").unwrap();
    for operation in TitleOperation::ALL {
        assert_eq!(
            state(&offline, LOWER_THIRD, operation),
            blocked(TitleBlocked::NotReady)
        );
    }
    offline.set_access(access(&["edit_project", "transition"]));
    offline.preview(lt).unwrap();
    assert_eq!(state(&offline, LOWER_THIRD, TitleOperation::Update), Ok(()));
    assert_eq!(server.commands.len(), 2, "blocked actions sent nothing");
}

#[test]
fn take_airs_exactly_what_was_previewed() {
    let mut server = FakeTitleServer::new();
    let mut editor = server.editor(producer());
    let lt = title(LOWER_THIRD);

    editor.set_input(lt, field(NAME), "Grace Hopper").unwrap();
    assert_eq!(
        editor.request(lt, TitleOperation::Take),
        Err(TitleEditorError::Blocked(TitleBlocked::NotPreviewed))
    );
    let preview = editor.preview(lt).unwrap();
    assert_eq!(preview.base_revision, 0);
    assert_eq!(preview.fields[0].value, text("Grace Hopper"));
    assert!(editor.presentation(lt).unwrap().previewed);

    // Any edit after preview disarms Take until the new values are seen.
    editor.set_input(lt, field(ROLE), "Rear Admiral").unwrap();
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::NotPreviewed)
    );
    editor.preview(lt).unwrap();

    let command = editor.request(lt, TitleOperation::Take).unwrap();
    assert_eq!(
        command.body,
        TitleCommandBody::Take {
            channel: 1,
            base_revision: 0,
            fields: vec![
                (field(NAME), text("Grace Hopper")),
                (field(ROLE), text("Rear Admiral")),
            ],
        }
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::OffAir),
        blocked(TitleBlocked::Pending)
    );
    assert!(editor.presentation(lt).unwrap().pending);
    let reply = server.submit(command);
    assert_eq!(
        editor.apply_reply(reply),
        Ok(ReplyOutcome::Applied(TitleOperation::Take))
    );

    assert!(server.on_air(LOWER_THIRD));
    assert_eq!(server.value(LOWER_THIRD, ROLE), text("Rear Admiral"));
    let view = editor.presentation(lt).unwrap();
    assert!(view.on_air && !view.dirty && !view.pending && !view.previewed);
    assert_eq!((view.live_revision, view.base_revision), (1, 1));
    assert!(editor.list()[0].on_air);
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::OnAir)
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::OnAir)
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::NoChanges)
    );
    assert_eq!(state(&editor, LOWER_THIRD, TitleOperation::OffAir), Ok(()));
}

#[test]
fn edits_on_air_go_live_only_through_previewed_update() {
    let mut server = FakeTitleServer::new();
    let mut editor = server.editor(producer());
    let lt = title(LOWER_THIRD);
    editor.preview(lt).unwrap();
    editor
        .dispatch(&mut server, lt, TitleOperation::Take)
        .unwrap();
    assert!(server.on_air(LOWER_THIRD));

    // Drafting on air leaves program untouched.
    editor
        .set_input(lt, field(NAME), "Live Correction")
        .unwrap();
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Ada Lovelace"));
    let view = editor.presentation(lt).unwrap();
    assert_eq!(view.fields[0].live_value, Some(text("Ada Lovelace")));
    assert_eq!(view.fields[0].input, "Live Correction");
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::OnAir)
    );
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::NotPreviewed)
    );

    editor.preview(lt).unwrap();
    assert_eq!(
        editor.dispatch(&mut server, lt, TitleOperation::Update),
        Ok(ReplyOutcome::Applied(TitleOperation::Update))
    );
    assert!(server.on_air(LOWER_THIRD));
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Live Correction"));

    assert_eq!(
        editor.dispatch(&mut server, lt, TitleOperation::OffAir),
        Ok(ReplyOutcome::Applied(TitleOperation::OffAir))
    );
    assert!(!server.on_air(LOWER_THIRD));

    // Off air: Save stores the cue without airing it and Update is refused.
    editor.set_input(lt, field(NAME), "Next Guest").unwrap();
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::OffAir)
    );
    assert_eq!(
        editor.dispatch(&mut server, lt, TitleOperation::Save),
        Ok(ReplyOutcome::Applied(TitleOperation::Save))
    );
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Next Guest"));
    assert!(!server.on_air(LOWER_THIRD));
    assert_eq!(server.revision(LOWER_THIRD), 2);
}

#[test]
fn two_editors_with_a_stale_base_conflict_and_rebase_explicitly() {
    let mut server = FakeTitleServer::new();
    let mut alice = server.editor(producer());
    let mut bob = server.editor(producer());
    let lt = title(LOWER_THIRD);

    alice.set_input(lt, field(NAME), "Alice's Name").unwrap();
    bob.set_input(lt, field(NAME), "Bob's Name").unwrap();
    bob.set_input(lt, field(ROLE), "Bob's Role").unwrap();

    alice
        .dispatch(&mut server, lt, TitleOperation::Save)
        .unwrap();
    assert_eq!(server.revision(LOWER_THIRD), 1);

    // Bob still believes revision 0 is current; the server refuses.
    assert_eq!(
        bob.dispatch(&mut server, lt, TitleOperation::Save),
        Ok(ReplyOutcome::Conflict)
    );
    assert_eq!(
        server.value(LOWER_THIRD, NAME),
        text("Alice's Name"),
        "no overwrite"
    );
    assert_eq!(server.revision(LOWER_THIRD), 1);

    let view = bob.presentation(lt).unwrap();
    assert!(view.stale && view.dirty);
    assert_eq!(view.notice, Some(TitleNotice::Conflict));
    assert_eq!((view.base_revision, view.live_revision), (0, 1));
    let name = &view.fields[0];
    assert_eq!(name.input, "Bob's Name", "draft survives the conflict");
    assert_eq!(name.live_value, Some(text("Alice's Name")));
    let conflict = name.conflict.as_ref().unwrap();
    assert_eq!(conflict.base, text("Ada Lovelace"));
    assert_eq!(conflict.theirs, Some(text("Alice's Name")));
    assert!(view.fields[1].dirty && view.fields[1].conflict.is_none());
    for operation in [TitleOperation::Save, TitleOperation::Take] {
        assert_eq!(
            state(&bob, LOWER_THIRD, operation),
            blocked(TitleBlocked::Stale)
        );
    }
    assert_eq!(
        bob.preview(lt),
        Err(TitleEditorError::Blocked(TitleBlocked::Stale))
    );
    assert_eq!(server.commands.len(), 2);

    // Bob explicitly keeps his values on top of Alice's revision.
    bob.rebase(lt).unwrap();
    let view = bob.presentation(lt).unwrap();
    assert!(!view.stale && view.dirty && view.notice.is_none());
    assert_eq!(view.base_revision, 1);
    assert!(view.fields[0].conflict.is_none());
    let command = bob.request(lt, TitleOperation::Save).unwrap();
    assert!(matches!(
        command.body,
        TitleCommandBody::Save {
            base_revision: 1,
            ..
        }
    ));
    let reply = server.submit(command);
    bob.apply_reply(reply).unwrap();
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Bob's Name"));
    assert_eq!(server.value(LOWER_THIRD, ROLE), text("Bob's Role"));

    // Alice, still on revision 1, now conflicts and discards instead.
    alice.set_input(lt, field(ROLE), "Alice's Role").unwrap();
    assert_eq!(
        alice.dispatch(&mut server, lt, TitleOperation::Save),
        Ok(ReplyOutcome::Conflict)
    );
    alice.discard_all(lt).unwrap();
    let view = alice.presentation(lt).unwrap();
    assert!(!view.stale && !view.dirty);
    assert_eq!(view.fields[0].input, "Bob's Name");
    assert_eq!(view.fields[1].input, "Bob's Role");
    assert_eq!(server.value(LOWER_THIRD, ROLE), text("Bob's Role"));
}

#[test]
fn pushed_snapshots_follow_clean_titles_and_stale_dirty_ones() {
    let mut server = FakeTitleServer::new();
    let mut alice = server.editor(producer());
    let mut bob = server.editor(producer());
    let lt = title(LOWER_THIRD);

    // Bob previews the clean title, then Alice changes it.
    bob.preview(lt).unwrap();
    alice.set_input(lt, field(NAME), "Changed").unwrap();
    alice
        .dispatch(&mut server, lt, TitleOperation::Save)
        .unwrap();
    bob.observe(server.snapshot(LOWER_THIRD)).unwrap();
    let view = bob.presentation(lt).unwrap();
    assert!(!view.stale, "a title without drafts follows the server");
    assert_eq!(view.fields[0].input, "Changed");
    assert_eq!(
        state(&bob, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::NotPreviewed),
        "new content must be previewed before it airs"
    );

    // With a draft on another field, the push moves Bob's base along
    // (Protocol 2.96): fields are revisioned one by one, so his draft is
    // not stale and may be saved; airing it needs a fresh preview.
    bob.set_input(lt, field(ROLE), "Bob's Role").unwrap();
    bob.preview(lt).unwrap();
    alice.set_input(lt, field(NAME), "Changed Again").unwrap();
    alice
        .dispatch(&mut server, lt, TitleOperation::Save)
        .unwrap();
    bob.observe(server.snapshot(LOWER_THIRD)).unwrap();
    let view = bob.presentation(lt).unwrap();
    assert!(!view.stale);
    assert!(!view.fields[0].changed_upstream && !view.fields[0].dirty);
    assert_eq!(view.fields[0].input, "Changed Again");
    assert!(view.fields[1].dirty && view.fields[1].conflict.is_none());
    assert_eq!(state(&bob, LOWER_THIRD, TitleOperation::Save), Ok(()));
    assert_eq!(
        state(&bob, LOWER_THIRD, TitleOperation::Take),
        blocked(TitleBlocked::NotPreviewed)
    );

    // A change to the drafted field itself makes Bob stale at once.
    alice.set_input(lt, field(ROLE), "Alice's Role").unwrap();
    alice
        .dispatch(&mut server, lt, TitleOperation::Save)
        .unwrap();
    bob.observe(server.snapshot(LOWER_THIRD)).unwrap();
    let view = bob.presentation(lt).unwrap();
    assert!(view.stale);
    assert!(view.fields[1].changed_upstream && view.fields[1].dirty);
    assert!(view.fields[1].conflict.is_some());
    assert_eq!(
        state(&bob, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::Stale)
    );
    assert_eq!(server.value(LOWER_THIRD, ROLE), text("Alice's Role"));

    // An out-of-order older snapshot is ignored.
    let mut old = server.snapshot(LOWER_THIRD);
    old.revision = 0;
    old.fields[0].value = text("Ancient");
    bob.observe(old).unwrap();
    assert_eq!(
        bob.presentation(lt).unwrap().fields[0].live_value,
        Some(text("Changed Again"))
    );

    // Discarding the last draft catches the base up.
    bob.discard_field(lt, field(ROLE)).unwrap();
    let view = bob.presentation(lt).unwrap();
    assert!(!view.stale && !view.dirty);
    assert_eq!(view.fields[0].input, "Changed Again");
}

#[test]
fn field_type_mismatches_are_caught_locally_and_by_the_authority() {
    let mut server = FakeTitleServer::new();
    let mut editor = server.editor(producer());
    let lt = title(LOWER_THIRD);

    editor
        .set_value(lt, field(NAME), &TitleFieldValue::Number(42))
        .unwrap();
    let view = editor.presentation(lt).unwrap();
    assert_eq!(
        view.fields[0].error,
        Some(FieldError::TypeMismatch {
            expected: FieldType::Text,
            actual: FieldType::Number,
        })
    );
    assert_eq!(view.fields[0].input, "42");
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::InvalidDraft)
    );
    editor.set_value(lt, field(NAME), &text("42")).unwrap();
    assert_eq!(state(&editor, LOWER_THIRD, TitleOperation::Save), Ok(()));
    assert_eq!(
        editor.set_input(lt, field(SCORE), "1"),
        Err(TitleEditorError::UnknownField(field(SCORE)))
    );

    // A forged command bypassing the model changes nothing on the server.
    let forged = TitleCommand {
        request: 900,
        title: lt,
        body: TitleCommandBody::Save {
            base_revision: 0,
            fields: vec![(field(ACCENT), text("red"))],
        },
    };
    let reply = server.submit(forged);
    assert!(matches!(
        reply,
        TitleReply::Rejected {
            reason: TitleRejection::TypeMismatch {
                expected: FieldType::Color,
                actual: FieldType::Text,
                ..
            },
            ..
        }
    ));
    assert_eq!(server.revision(LOWER_THIRD), 0);

    // A type mismatch reported by the authority invalidates that draft.
    let command = editor.request(lt, TitleOperation::Save).unwrap();
    let reply = TitleReply::Rejected {
        request: command.request,
        reason: TitleRejection::TypeMismatch {
            field: field(NAME),
            expected: FieldType::Number,
            actual: FieldType::Text,
        },
        current: None,
    };
    assert_eq!(
        editor.apply_reply(reply),
        Ok(ReplyOutcome::Rejected(TitleRejection::TypeMismatch {
            field: field(NAME),
            expected: FieldType::Number,
            actual: FieldType::Text,
        }))
    );
    let view = editor.presentation(lt).unwrap();
    assert!(!view.pending && view.dirty);
    assert!(matches!(
        view.fields[0].error,
        Some(FieldError::TypeMismatch { .. })
    ));
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Ada Lovelace"));
}

#[test]
fn rebase_revalidates_drafts_against_changed_field_types() {
    let lt = title(LOWER_THIRD);
    let base = TitleSnapshot {
        title: lt,
        name: "Lower third".into(),
        revision: 4,
        on_air: false,
        fields: vec![
            ExposedField {
                id: field(NAME),
                name: "Name".into(),
                value: text("Ada"),
                revision: 0,
            },
            ExposedField {
                id: field(ROLE),
                name: "Role".into(),
                value: text("Analyst"),
                revision: 0,
            },
            ExposedField {
                id: field(SCORE),
                name: "Score".into(),
                value: text("zero"),
                revision: 0,
            },
        ],
    };
    let mut editor = TitleEditor::new(producer());
    editor.observe(base.clone()).unwrap();
    editor.set_input(lt, field(NAME), "seven").unwrap();
    editor.set_input(lt, field(ROLE), "Host").unwrap();
    editor.set_input(lt, field(SCORE), "7").unwrap();

    // The server swaps the template: Name and Score become numbers and
    // Role is removed.
    let mut swapped = base;
    swapped.revision = 5;
    swapped.fields = vec![
        ExposedField {
            id: field(NAME),
            name: "Name".into(),
            value: TitleFieldValue::Number(1),
            revision: 0,
        },
        ExposedField {
            id: field(SCORE),
            name: "Score".into(),
            value: TitleFieldValue::Number(0),
            revision: 0,
        },
    ];
    editor.observe(swapped).unwrap();
    let view = editor.presentation(lt).unwrap();
    assert!(view.stale);
    let role = view.fields.iter().find(|f| f.id == field(ROLE)).unwrap();
    assert_eq!(role.live_value, None);
    assert_eq!(role.conflict.as_ref().unwrap().theirs, None);

    editor.rebase(lt).unwrap();
    let view = editor.presentation(lt).unwrap();
    let errors: BTreeMap<_, _> = view.fields.iter().map(|f| (f.id, f.error)).collect();
    assert_eq!(errors[&field(NAME)], Some(FieldError::InvalidNumber));
    assert_eq!(errors[&field(ROLE)], Some(FieldError::FieldRemoved));
    assert_eq!(errors[&field(SCORE)], None);
    assert_eq!(view.fields[1].field_type, FieldType::Number);
    assert_eq!(
        state(&editor, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::InvalidDraft)
    );

    editor.discard_field(lt, field(ROLE)).unwrap();
    editor.set_input(lt, field(NAME), "1").unwrap();
    let command = editor.request(lt, TitleOperation::Save).unwrap();
    assert_eq!(
        command.body,
        TitleCommandBody::Save {
            base_revision: 5,
            fields: vec![(field(SCORE), TitleFieldValue::Number(7))],
        }
    );
}

#[test]
fn air_state_races_are_refused_and_off_air_stays_available() {
    let mut server = FakeTitleServer::new();
    let mut alice = server.editor(producer());
    let mut bob = server.editor(producer());
    let lt = title(LOWER_THIRD);

    // Alice prepares a cue edit; Bob takes the title before she saves.
    alice.set_input(lt, field(NAME), "Cue Only").unwrap();
    bob.preview(lt).unwrap();
    bob.dispatch(&mut server, lt, TitleOperation::Take).unwrap();
    assert_eq!(
        alice.dispatch(&mut server, lt, TitleOperation::Save),
        Ok(ReplyOutcome::Rejected(TitleRejection::AlreadyOnAir))
    );
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Ada Lovelace"));
    let view = alice.presentation(lt).unwrap();
    assert!(view.on_air && view.dirty);
    assert_eq!(
        view.notice,
        Some(TitleNotice::Rejected(TitleRejection::AlreadyOnAir))
    );
    assert_eq!(
        state(&alice, LOWER_THIRD, TitleOperation::Save),
        blocked(TitleBlocked::OnAir)
    );
    assert_eq!(
        state(&alice, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::NotPreviewed)
    );

    // Bob updates the live title's name, so Alice's draft of it goes
    // stale; she can still take it off air.
    bob.set_input(lt, field(NAME), "Live").unwrap();
    bob.preview(lt).unwrap();
    bob.dispatch(&mut server, lt, TitleOperation::Update)
        .unwrap();
    alice.observe(server.snapshot(LOWER_THIRD)).unwrap();
    assert!(alice.presentation(lt).unwrap().stale);
    assert_eq!(
        state(&alice, LOWER_THIRD, TitleOperation::Update),
        blocked(TitleBlocked::Stale)
    );
    assert_eq!(state(&alice, LOWER_THIRD, TitleOperation::OffAir), Ok(()));
    assert_eq!(
        alice.dispatch(&mut server, lt, TitleOperation::OffAir),
        Ok(ReplyOutcome::Applied(TitleOperation::OffAir))
    );
    assert!(!server.on_air(LOWER_THIRD));
    let view = alice.presentation(lt).unwrap();
    assert!(
        !view.on_air && view.stale && view.dirty,
        "off-air keeps drafts"
    );

    // Bob's view still thinks it is on air; his off-air is refused.
    assert_eq!(
        bob.dispatch(&mut server, lt, TitleOperation::OffAir),
        Ok(ReplyOutcome::Rejected(TitleRejection::NotOnAir))
    );
    assert!(!bob.presentation(lt).unwrap().on_air);
}

#[test]
fn drafts_edited_while_a_command_is_pending_stay_dirty() {
    let mut server = FakeTitleServer::new();
    let mut editor = server.editor(producer());
    let lt = title(LOWER_THIRD);

    editor.set_input(lt, field(NAME), "First").unwrap();
    let command = editor.request(lt, TitleOperation::Save).unwrap();
    assert_eq!(
        editor.rebase(lt),
        Err(TitleEditorError::Blocked(TitleBlocked::Pending))
    );
    editor.set_input(lt, field(NAME), "Second").unwrap();
    editor.set_input(lt, field(ROLE), "Late").unwrap();

    assert_eq!(
        editor.apply_reply(TitleReply::Conflict {
            request: command.request + 1,
            current: server.snapshot(LOWER_THIRD),
        }),
        Err(UnexpectedReply {
            request: command.request + 1
        })
    );
    let reply = server.submit(command);
    editor.apply_reply(reply).unwrap();
    assert_eq!(server.value(LOWER_THIRD, NAME), text("First"));

    let view = editor.presentation(lt).unwrap();
    assert!(!view.stale && !view.pending);
    assert_eq!(view.base_revision, 1);
    assert_eq!(view.fields[0].input, "Second");
    assert!(view.fields[0].dirty && view.fields[1].dirty);
    editor
        .dispatch(&mut server, lt, TitleOperation::Save)
        .unwrap();
    assert_eq!(server.value(LOWER_THIRD, NAME), text("Second"));
    assert!(!editor.presentation(lt).unwrap().dirty);
}

#[test]
fn malformed_snapshots_are_refused() {
    let server = FakeTitleServer::new();
    let mut editor = TitleEditor::new(producer());
    let mut duplicate = server.snapshot(LOWER_THIRD);
    duplicate.fields.push(duplicate.fields[0].clone());
    assert_eq!(
        editor.observe(duplicate),
        Err(SnapshotError::DuplicateField(field(NAME)))
    );
    let mut oversized = server.snapshot(SCOREBUG);
    oversized.fields = (1..=u128::try_from(MAX_EXPOSED_FIELDS).unwrap() + 1)
        .map(|id| ExposedField {
            id: field(id),
            name: format!("f{id}"),
            value: TitleFieldValue::Number(0),
            revision: 0,
        })
        .collect();
    assert_eq!(editor.observe(oversized), Err(SnapshotError::TooManyFields));
    assert_eq!(editor.list(), Vec::new());
    assert_eq!(
        editor.presentation(title(LOWER_THIRD)),
        Err(TitleEditorError::UnknownTitle(title(LOWER_THIRD)))
    );
}

#[test]
fn field_text_round_trips_through_parse_and_format() {
    for value in [
        text("Line one\nLine two"),
        TitleFieldValue::Number(-12),
        color(1, 2, 254, 128),
        image("assets/logo.png"),
    ] {
        let input = format_field_value(&value);
        assert_eq!(parse_field_input(FieldType::of(&value), &input), Ok(value));
    }
    assert_eq!(
        parse_field_input(FieldType::Color, "#abc"),
        Err(FieldError::InvalidColor)
    );
    assert_eq!(
        parse_field_input(FieldType::Color, "#00ff00"),
        Ok(color(0, 255, 0, 255))
    );
    assert_eq!(
        parse_field_input(FieldType::Number, "99999999999999999999"),
        Err(FieldError::InvalidNumber)
    );
}
