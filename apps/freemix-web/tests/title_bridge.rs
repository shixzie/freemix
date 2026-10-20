//! The shared title editor on the real protocol: each desk owns a
//! replicated `ClientModel`, a `TitleEditor`, and a `TitleProtocolBridge`,
//! and drives an in-process `fm_control::ControlService` with the exact
//! `title_fields` / `title_take` / `title_off_air` payloads. Command results
//! are delivered before their events, as on the wire, so every acceptance
//! is held until the desk's replicated state has applied it.

use core::num::NonZeroU128;

use fm_auth::{Policy, Principal, Role, SessionId, UserId};
use fm_clock::ClockDomainId;
use fm_control::{ControlLimits, ControlService};
use fm_engine::{Engine, EngineTitle, ShowState};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandResult, EventMessage, TitleFieldValue,
    WireTitleFieldId,
};
use fm_titles::{
    Color, FieldDefinition, FieldId, FieldValue, TemplateId, TitleFieldSet, TitleTemplate,
};
use fm_types::{FrameRate, InputId, ProjectId};
use fm_ui_model::{ClientModel, DurableProjectEvent, ProjectSnapshot};
use freemix_web::{
    BridgeIntake, ReplyOutcome, TitleAccess, TitleBlocked, TitleEditor, TitleEditorError,
    TitleNotice, TitleOperation, TitleProtocolBridge, TitleRejection, TitleReply, title_payload,
};

const CAMERA: u128 = 1;
const SLIDES: u128 = 2;
const LOWER_THIRD: u128 = 3;
const NAME: u128 = 1;
const ROLE: u128 = 2;
const SCORE: u128 = 3;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn field(value: u128) -> WireTitleFieldId {
    WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
}

fn text(value: &str) -> TitleFieldValue {
    TitleFieldValue::Text(value.to_owned())
}

fn project() -> ProjectId {
    ProjectId::new(NonZeroU128::new(7).unwrap())
}

fn template() -> TitleTemplate {
    let definition = |id: u128, name: &str, default: FieldValue| FieldDefinition {
        id: FieldId::new(NonZeroU128::new(id).unwrap()),
        name: name.to_owned(),
        default,
        exposed: true,
    };
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower third".into(),
        width: 64,
        height: 16,
        background: Color::TRANSPARENT,
        fields: vec![
            definition(NAME, "Name", FieldValue::Text("Ada Lovelace".into())),
            definition(ROLE, "Role", FieldValue::Text("Analyst".into())),
            definition(SCORE, "Score", FieldValue::Number(0)),
        ],
        elements: Vec::new(),
        animation: fm_titles::TitleAnimation::default(),
    }
}

/// Session permissions the production policy grants each role, as the
/// server reports them in the handshake.
fn permissions(role: Role) -> &'static [&'static str] {
    match role {
        Role::Graphics => &["view_status", "edit_project"],
        Role::Operator => &["view_status", "select_preview", "transition"],
        Role::Admin => &["view_status", "edit_project", "transition"],
        _ => unreachable!("tests use graphics, operator, and admin"),
    }
}

/// The authority plus every event it published, in revision order.
struct Server {
    control: ControlService,
    log: Vec<EventMessage>,
}

impl Server {
    fn new() -> Self {
        let mut show = ShowState::new(
            "titles",
            vec![
                (input(CAMERA), "Camera".into()),
                (input(SLIDES), "Slides".into()),
                (input(LOWER_THIRD), "Lower third".into()),
            ],
            input(CAMERA),
            input(SLIDES),
        )
        .unwrap();
        show.set_title(
            input(LOWER_THIRD),
            EngineTitle::new(
                "Lower third",
                TitleFieldSet::from_template(&template()).unwrap(),
            ),
        )
        .unwrap();
        let engine = Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        );
        Self {
            control: ControlService::new(
                engine,
                Policy::production(),
                "engine-titles",
                "log-titles",
                ControlLimits::default(),
            ),
            log: Vec::new(),
        }
    }

    fn submit(&mut self, principal: &Principal, message: CommandMessage) -> CommandResult {
        let submission = self.control.submit(principal, message, 0).unwrap();
        self.log.extend(submission.output.events);
        submission.output.result
    }

    fn title_value(&self, id: u128) -> TitleFieldValue {
        let state = self
            .control
            .title_states()
            .into_iter()
            .find(|state| state.input.to_domain() == input(LOWER_THIRD))
            .unwrap();
        state
            .fields
            .into_iter()
            .find(|status| status.field == field(id))
            .unwrap()
            .value
    }

    fn field_revision(&self) -> u64 {
        self.control.title_states()[0].field_revision
    }
}

/// One client: replicated state, editor, and bridge.
struct Desk {
    name: &'static str,
    principal: Principal,
    model: ClientModel,
    editor: TitleEditor,
    bridge: TitleProtocolBridge,
    applied: usize,
    sent: u64,
}

impl Desk {
    /// Joins as `role`, installing the snapshot and its title states.
    fn join(server: &Server, name: &'static str, role: Role) -> Self {
        let mut model = ClientModel::new(project());
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project(),
                server.control.snapshot().snapshot.clone(),
            ))
            .unwrap();
        for state in server.control.title_states() {
            model.apply_title_state(state).unwrap();
        }
        let mut desk = Self {
            name,
            principal: Principal::authenticated(
                UserId::new(name).unwrap(),
                SessionId::new(name).unwrap(),
                [role],
            ),
            model,
            editor: TitleEditor::new(TitleAccess::from_permissions(true, permissions(role))),
            bridge: TitleProtocolBridge::new(),
            applied: server.log.len(),
            sent: 0,
        };
        desk.editor.sync(desk.model.state().unwrap()).unwrap();
        desk
    }

    /// Applies every event this desk has not seen, then hands released
    /// replies to the editor and refreshes its snapshots.
    fn catch_up(&mut self, server: &Server) -> Vec<ReplyOutcome> {
        for event in &server.log[self.applied..] {
            self.model
                .apply_event(DurableProjectEvent::from_protocol(project(), event.clone()))
                .unwrap();
        }
        self.applied = server.log.len();
        let outcomes = self
            .bridge
            .poll(&self.model)
            .into_iter()
            .map(|reply| self.editor.apply_reply(reply).unwrap())
            .collect();
        self.editor.sync(self.model.state().unwrap()).unwrap();
        outcomes
    }

    /// Sends `operation` over the protocol path. The result arrives before
    /// this desk applies the events, so acceptances are held first.
    fn send(&mut self, server: &mut Server, operation: TitleOperation) -> BridgeIntake {
        let command = self.editor.request(input(LOWER_THIRD), operation).unwrap();
        self.sent += 1;
        let id = format!("{}:{}", self.name, self.sent);
        let payload = title_payload(&command).unwrap();
        self.bridge.track(id.clone(), &command);
        let result = server.submit(
            &self.principal,
            CommandMessage {
                protocol: CURRENT_PROTOCOL_VERSION,
                id: id.clone(),
                idempotency_key: id,
                expected_revision: None,
                deadline_ms: None,
                payload,
            },
        );
        let intake = self.bridge.apply_result(&result, &self.model);
        if let BridgeIntake::Reply(reply) = &intake {
            self.editor.apply_reply(reply.clone()).unwrap();
        }
        intake
    }

    /// Sends `operation` and catches up, returning the editor outcome.
    fn dispatch(&mut self, server: &mut Server, operation: TitleOperation) -> ReplyOutcome {
        let intake = self.send(server, operation);
        let mut outcomes = self.catch_up(server);
        match intake {
            BridgeIntake::Held => {
                assert_eq!(outcomes.len(), 1, "{}: one held reply", self.name);
                outcomes.remove(0)
            }
            BridgeIntake::Reply(reply) => {
                assert_eq!(outcomes, []);
                outcome_of(&reply)
            }
            BridgeIntake::NotTitle => panic!("{}: result was not tracked", self.name),
        }
    }

    fn view(&self) -> freemix_web::TitleEditorPresentation {
        self.editor.presentation(input(LOWER_THIRD)).unwrap()
    }

    fn state(&self, operation: TitleOperation) -> Result<(), TitleBlocked> {
        self.view()
            .operations
            .into_iter()
            .find(|presented| presented.operation == operation)
            .unwrap()
            .state
    }
}

fn outcome_of(reply: &TitleReply) -> ReplyOutcome {
    match reply {
        TitleReply::Applied { .. } => panic!("an applied reply is delivered through the editor"),
        TitleReply::Conflict { .. } => ReplyOutcome::Conflict,
        TitleReply::Rejected { reason, .. } => ReplyOutcome::Rejected(*reason),
    }
}

#[test]
fn snapshot_titles_reach_the_editor_with_typed_fields() {
    let server = Server::new();
    let desk = Desk::join(&server, "producer", Role::Admin);

    let list = desk.editor.list();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].title, list[0].name.as_str(), list[0].on_air),
        (input(LOWER_THIRD), "Lower third", false)
    );
    let view = desk.view();
    let inputs: Vec<_> = view
        .fields
        .iter()
        .map(|field| (field.id, field.input.as_str()))
        .collect();
    assert_eq!(
        inputs,
        [
            (field(NAME), "Ada Lovelace"),
            (field(ROLE), "Analyst"),
            (field(SCORE), "0")
        ]
    );
    assert_eq!(view.take_channel, 1);
}

#[test]
fn two_editors_race_and_the_loser_conflicts_with_drafts_kept() {
    let mut server = Server::new();
    let mut alice = Desk::join(&server, "alice", Role::Graphics);
    let mut bob = Desk::join(&server, "bob", Role::Graphics);
    let lt = input(LOWER_THIRD);

    alice
        .editor
        .set_input(lt, field(NAME), "Alice's Name")
        .unwrap();
    bob.editor.set_input(lt, field(NAME), "Bob's Name").unwrap();
    bob.editor.set_input(lt, field(ROLE), "Bob's Role").unwrap();

    // Alice's acceptance is held until her replica applies its revision.
    assert_eq!(
        alice.send(&mut server, TitleOperation::Save),
        BridgeIntake::Held
    );
    assert!(alice.view().pending);
    assert_eq!(
        alice.catch_up(&server),
        [ReplyOutcome::Applied(TitleOperation::Save)]
    );
    let view = alice.view();
    assert!(!view.pending && !view.dirty);
    assert_eq!((view.live_revision, view.base_revision), (1, 1));
    assert_eq!(server.title_value(NAME), text("Alice's Name"));

    // Bob has not applied Alice's event: his save names revision 0, the
    // server refuses it, and the conflict waits until his replica shows
    // what he lost against.
    assert_eq!(
        bob.send(&mut server, TitleOperation::Save),
        BridgeIntake::Held
    );
    assert_eq!(bob.catch_up(&server), [ReplyOutcome::Conflict]);
    assert_eq!(
        server.title_value(NAME),
        text("Alice's Name"),
        "no overwrite"
    );
    assert_eq!(server.field_revision(), 1);

    let view = bob.view();
    assert!(view.stale && view.dirty && !view.pending);
    assert_eq!(view.notice, Some(TitleNotice::Conflict));
    assert_eq!((view.base_revision, view.live_revision), (0, 1));
    assert_eq!(view.fields[0].input, "Bob's Name", "draft survives");
    assert_eq!(view.fields[0].live_value, Some(text("Alice's Name")));
    let conflict = view.fields[0].conflict.as_ref().unwrap();
    assert_eq!(conflict.base, text("Ada Lovelace"));
    assert_eq!(conflict.theirs, Some(text("Alice's Name")));
    assert_eq!(view.fields[1].input, "Bob's Role");
    assert_eq!(bob.state(TitleOperation::Save), Err(TitleBlocked::Stale));

    // Bob explicitly keeps his values on Alice's revision and wins.
    bob.editor.rebase(lt).unwrap();
    assert_eq!(
        bob.dispatch(&mut server, TitleOperation::Save),
        ReplyOutcome::Applied(TitleOperation::Save)
    );
    assert_eq!(server.title_value(NAME), text("Bob's Name"));
    assert_eq!(server.title_value(ROLE), text("Bob's Role"));
    assert!(!bob.view().dirty);

    // Alice follows the pushed change without drafts of her own.
    alice.catch_up(&server);
    let view = alice.view();
    assert!(!view.stale && !view.dirty);
    assert_eq!(view.fields[0].input, "Bob's Name");
}

#[test]
fn graphics_session_is_not_permitted_to_take() {
    let mut server = Server::new();
    let mut graphics = Desk::join(&server, "graphics", Role::Graphics);
    let lt = input(LOWER_THIRD);
    graphics.editor.preview(lt).unwrap();
    assert_eq!(
        graphics.state(TitleOperation::Take),
        Err(TitleBlocked::NotPermitted)
    );
    assert_eq!(
        graphics.editor.request(lt, TitleOperation::Take),
        Err(TitleEditorError::Blocked(TitleBlocked::NotPermitted))
    );

    // The server is the authority: a desk that wrongly believes it may air
    // (overstated permissions) is refused with permission_denied.
    graphics.editor.set_access(TitleAccess::from_permissions(
        true,
        &["edit_project", "transition"],
    ));
    graphics.editor.preview(lt).unwrap();
    let revision = server.control.diagnostics().current_revision;
    assert_eq!(
        graphics.dispatch(&mut server, TitleOperation::Take),
        ReplyOutcome::Rejected(TitleRejection::NotPermitted)
    );
    assert_eq!(server.control.diagnostics().current_revision, revision);
    let view = graphics.view();
    assert!(!view.on_air && !view.pending);
    assert_eq!(
        view.notice,
        Some(TitleNotice::Rejected(TitleRejection::NotPermitted))
    );
}

#[test]
fn take_and_off_air_reach_the_replicated_snapshots() {
    let mut server = Server::new();
    let mut producer = Desk::join(&server, "producer", Role::Admin);
    let mut operator = Desk::join(&server, "operator", Role::Operator);
    let lt = input(LOWER_THIRD);

    // Take with an edit onto overlay channel 2.
    producer.editor.set_input(lt, field(SCORE), "3").unwrap();
    producer.editor.set_take_channel(lt, 2).unwrap();
    assert_eq!(
        producer.editor.set_take_channel(lt, 0),
        Err(TitleEditorError::InvalidTakeChannel(0))
    );
    producer.editor.preview(lt).unwrap();
    assert_eq!(
        producer.dispatch(&mut server, TitleOperation::Take),
        ReplyOutcome::Applied(TitleOperation::Take)
    );
    let state = producer.model.state().unwrap();
    assert!(state.title_on_air(lt));
    assert_eq!(state.title(lt).unwrap().field_revision, 1);
    let view = producer.view();
    assert!(view.on_air && !view.dirty && !view.pending);
    assert_eq!(view.fields[2].live_value, Some(TitleFieldValue::Number(3)));
    assert_eq!(server.title_value(SCORE), TitleFieldValue::Number(3));

    // Another desk sees the title on air through its own replica.
    assert_eq!(operator.catch_up(&server), []);
    let view = operator.view();
    assert!(view.on_air);
    assert_eq!(view.fields[2].input, "3");
    assert_eq!(operator.state(TitleOperation::OffAir), Ok(()));
    assert_eq!(
        operator.state(TitleOperation::Take),
        Err(TitleBlocked::OnAir)
    );

    // Off air from the operator desk reaches both replicas.
    assert_eq!(
        operator.dispatch(&mut server, TitleOperation::OffAir),
        ReplyOutcome::Applied(TitleOperation::OffAir)
    );
    assert!(!operator.model.state().unwrap().title_on_air(lt));
    assert!(!operator.view().on_air);
    producer.catch_up(&server);
    assert!(!producer.view().on_air);
    assert_eq!(
        producer.state(TitleOperation::OffAir),
        Err(TitleBlocked::OffAir)
    );

    // The producer takes the title again; the operator takes it off before
    // the producer's replica sees that, so a live update is refused.
    producer.editor.preview(lt).unwrap();
    assert_eq!(
        producer.dispatch(&mut server, TitleOperation::Take),
        ReplyOutcome::Applied(TitleOperation::Take)
    );
    operator.catch_up(&server);
    assert_eq!(
        operator.dispatch(&mut server, TitleOperation::OffAir),
        ReplyOutcome::Applied(TitleOperation::OffAir)
    );
    producer
        .editor
        .set_input(lt, field(NAME), "Live fix")
        .unwrap();
    producer.editor.preview(lt).unwrap();
    assert_eq!(producer.state(TitleOperation::Update), Ok(()));
    assert_eq!(
        producer.dispatch(&mut server, TitleOperation::Update),
        ReplyOutcome::Rejected(TitleRejection::NotOnAir)
    );
    let view = producer.view();
    assert!(!view.on_air && view.dirty, "refusal keeps the draft");
    assert_eq!(server.title_value(NAME), text("Ada Lovelace"));
}
