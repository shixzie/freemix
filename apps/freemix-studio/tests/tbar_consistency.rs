//! T-bar consistency (SW-001, SW-004): the console lever, the console keys,
//! an external (OSC) controller, and the protocol API drive the same manual
//! transition, so each sends the same commands and the authority renders
//! the same Program frames, frame for frame, for every timed mix: through a
//! reversal and a commit, then through a cancel.
//!
//! Each path reads the replicated state a console would hold (a
//! `ClientModel` installed from the authority's snapshot) before every
//! frame, turns its gestures into Studio intents with the production code
//! (the real `StudioShell` for keys, `t_bar_intents` behind the lever,
//! `external_intent` for the controller), maps them with the worker's
//! `studio_intent_payload`, and submits them to an in-process
//! `ControlService` that ticks one frame per step. A `SimulatedPipeline`
//! renders each Program frame.

use core::num::NonZeroU128;

use eframe::egui;
use fm_auth::{Policy, Principal, Role, SessionId, UserId};
use fm_clock::ClockDomainId;
use fm_control::{ControlLimits, ControlService};
use fm_engine::{Engine, ShowState};
use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult,
    ManualTransitionKind, ManualTransitionPosition, ServerIdentity, TransitionFavoriteEffect,
};
use fm_sim::{ImageFrame, Rgba8, SimulatedPipeline, SimulatedSource, SourcePattern};
use fm_types::{FrameRate, InputId, ProjectId};
use fm_ui_egui::{
    ExternalStudioAction, ManualTransitionAvailability, ManualTransitionGate,
    StudioConnectionStatus, StudioIntent, StudioShell, StudioUiState, TBarGesture, external_intent,
    manual_transition_availability, t_bar_intents,
};
use fm_ui_model::{ClientModel, ManualTransitionStatus, ProjectSnapshot};
use freemix_studio::studio_intent_payload;

const TRANSITION_FRAMES: u32 = 30;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project() -> ProjectId {
    ProjectId::new(NonZeroU128::new(5).unwrap())
}

fn at(basis_points: u16) -> ManualTransitionPosition {
    ManualTransitionPosition::new(basis_points).unwrap()
}

/// One operator move, worded the same for every path.
#[derive(Clone, Copy, Debug)]
enum Move {
    Start,
    /// Up (positive) or down one 10 % step.
    Step(i32),
    Commit,
    Cancel,
    /// No gesture this frame.
    Hold,
}

/// Start, three steps up, one back (reversing), two up, commit; hold; then
/// start, two up, cancel; hold.
const SCRIPT: [Move; 14] = [
    Move::Start,
    Move::Step(1),
    Move::Step(1),
    Move::Step(1),
    Move::Step(-1),
    Move::Step(1),
    Move::Step(1),
    Move::Commit,
    Move::Hold,
    Move::Start,
    Move::Step(1),
    Move::Step(1),
    Move::Cancel,
    Move::Hold,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Path {
    Lever,
    Keys,
    Controller,
    Api,
}

/// The authority, the frames it rendered, and the commands it accepted.
struct Rig {
    control: ControlService,
    server: ServerIdentity,
    pipeline: SimulatedPipeline,
    principal: Principal,
    sent: u64,
    frames: Vec<ImageFrame>,
    commands: Vec<CommandPayload>,
}

impl Rig {
    fn new() -> Self {
        let show = ShowState::new(
            "t-bar",
            vec![(input(1), "Camera".into()), (input(2), "Slides".into())],
            input(1),
            input(2),
        )
        .unwrap();
        let engine = Engine::new(
            show,
            FrameRate::new(30, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        );
        let control = ControlService::new(
            engine,
            Policy::production(),
            "engine-t-bar",
            "log-t-bar",
            ControlLimits::default(),
        );
        let engine = control.diagnostics().engine;
        let server = ServerIdentity {
            engine_id: engine.engine_id,
            project_id: "5".into(),
            state_epoch: engine.state_epoch,
            log_id: engine.log_id,
        };
        let mut pipeline = SimulatedPipeline::new(32, 18).unwrap();
        pipeline
            .register(SimulatedSource::new(
                input(1),
                SourcePattern::Solid(Rgba8::new(200, 40, 20, 255)),
            ))
            .unwrap();
        pipeline
            .register(SimulatedSource::new(input(2), SourcePattern::Bars))
            .unwrap();
        Self {
            control,
            server,
            pipeline,
            principal: Principal::authenticated(
                UserId::new("operator").unwrap(),
                SessionId::new("operator").unwrap(),
                [Role::Operator],
            ),
            sent: 0,
            frames: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// The console's replicated state, installed from the latest snapshot.
    fn state(&self) -> StudioUiState {
        let mut model = ClientModel::new(project());
        model
            .install_snapshot(ProjectSnapshot::from_protocol(
                project(),
                self.control.snapshot().snapshot.clone(),
            ))
            .unwrap();
        StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(model.view().unwrap())
            .with_switcher_permissions(true, true)
    }

    /// Submits this frame's intents through the worker's payload mapping,
    /// then advances and renders one frame.
    fn frame(&mut self, state: &StudioUiState, intents: Vec<StudioIntent>) {
        for intent in intents {
            let payload = studio_intent_payload(intent, state.view.as_ref()).unwrap();
            self.submit(payload);
        }
        let frame = self.control.tick(&self.server).unwrap().frame;
        self.frames.push(
            self.pipeline
                .render(frame.frame.get(), frame.program)
                .unwrap(),
        );
    }

    fn submit(&mut self, payload: CommandPayload) {
        self.sent += 1;
        let id = format!("t-bar-{}", self.sent);
        let result = self
            .control
            .submit(
                &self.principal,
                CommandMessage {
                    protocol: CURRENT_PROTOCOL_VERSION,
                    id: id.clone(),
                    idempotency_key: id,
                    expected_revision: None,
                    deadline_ms: None,
                    payload: payload.clone(),
                },
                0,
            )
            .unwrap()
            .output
            .result;
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{payload:?}: {result:?}"
        );
        self.commands.push(payload);
    }
}

fn desired(state: &StudioUiState) -> ManualTransitionStatus {
    state
        .view
        .as_ref()
        .map_or(ManualTransitionStatus::Inactive, |view| {
            view.switcher.desired_manual_transition
        })
}

fn availability(state: &StudioUiState) -> ManualTransitionAvailability {
    manual_transition_availability(
        ManualTransitionGate::from_state(state),
        matches!(desired(state), ManualTransitionStatus::Active(_)),
    )
}

fn position(state: &StudioUiState) -> u16 {
    match desired(state) {
        ManualTransitionStatus::Active(active) => active.position.basis_points(),
        ManualTransitionStatus::Inactive => 0,
    }
}

/// The lever's position one step from the replicated one.
fn stepped(state: &StudioUiState, steps: i32) -> ManualTransitionPosition {
    let target = i32::from(position(state)) + steps * fm_ui_egui::T_BAR_KEY_STEP_BASIS_POINTS;
    at(u16::try_from(target).unwrap())
}

/// Presses `key` on a headless console showing `state` and returns its
/// manual-transition intents.
fn press(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    key: egui::Key,
) -> Vec<StudioIntent> {
    let mut intents = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            // Pressed and released, so the next press is not a repeat.
            events: [true, false]
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .to_vec(),
            ..Default::default()
        },
        |ui| intents = shell.draw(ui, state),
    );
    intents.retain(|intent| {
        matches!(
            intent,
            StudioIntent::StartManualTransition { .. }
                | StudioIntent::SetManualTransitionPosition { .. }
                | StudioIntent::CommitManualTransition
                | StudioIntent::CancelManualTransition
        )
    });
    intents
}

/// Runs the script on `path` with manual `kind` and returns the rendered
/// frames and accepted commands.
fn run(path: Path, kind: ManualTransitionKind) -> (Vec<ImageFrame>, Vec<CommandPayload>) {
    let mut rig = Rig::new();
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.set_transition_duration_frames(TRANSITION_FRAMES);
    shell.set_t_bar_kind(kind);
    let effect = TransitionFavoriteEffect::Mix(kind.mix().unwrap());
    for step in SCRIPT {
        let state = rig.state();
        let intents = match path {
            Path::Lever => {
                let gesture = match step {
                    // The operator grabs the idle lever at the bottom,
                    // then drags it a step at a time.
                    Move::Start => Some(TBarGesture::MoveTo {
                        kind,
                        position: ManualTransitionPosition::START,
                    }),
                    Move::Step(steps) => Some(TBarGesture::MoveTo {
                        kind,
                        position: stepped(&state, steps),
                    }),
                    // The COMMIT and CANCEL keys.
                    Move::Commit => Some(TBarGesture::Commit),
                    Move::Cancel => Some(TBarGesture::Cancel),
                    Move::Hold => None,
                };
                gesture
                    .map(|gesture| t_bar_intents(desired(&state), availability(&state), gesture))
                    .unwrap_or_default()
            }
            Path::Keys => {
                let key = match step {
                    Move::Start => Some(egui::Key::T),
                    Move::Step(1) => Some(egui::Key::PageUp),
                    Move::Step(_) => Some(egui::Key::PageDown),
                    Move::Commit => Some(egui::Key::End),
                    Move::Cancel => Some(egui::Key::Escape),
                    Move::Hold => None,
                };
                key.map(|key| press(&context, &mut shell, &state, key))
                    .unwrap_or_default()
            }
            Path::Controller => {
                let action = match step {
                    Move::Start => Some(ExternalStudioAction::StartManualTransition(effect)),
                    Move::Step(steps) => Some(ExternalStudioAction::SetManualTransitionPosition(
                        stepped(&state, steps),
                    )),
                    Move::Commit => Some(ExternalStudioAction::CommitManualTransition),
                    Move::Cancel => Some(ExternalStudioAction::CancelManualTransition),
                    Move::Hold => None,
                };
                action
                    .and_then(|action| external_intent(&state, action, TRANSITION_FRAMES, 30))
                    .into_iter()
                    .collect()
            }
            Path::Api => {
                let payload = match step {
                    Move::Start => Some(CommandPayload::StartManualTransition { kind }),
                    Move::Step(steps) => Some(CommandPayload::SetManualTransitionPosition {
                        position: stepped(&state, steps),
                    }),
                    Move::Commit => Some(CommandPayload::CommitManualTransition),
                    Move::Cancel => Some(CommandPayload::CancelManualTransition),
                    Move::Hold => None,
                };
                if let Some(payload) = payload {
                    rig.submit(payload);
                }
                Vec::new()
            }
        };
        if !matches!(path, Path::Api) {
            assert_eq!(
                intents.len(),
                usize::from(!matches!(step, Move::Hold)),
                "{path:?} {kind:?} {step:?}: {intents:?}"
            );
        }
        rig.frame(&state, intents);
    }
    (rig.frames, rig.commands)
}

#[test]
fn lever_keys_controller_and_api_render_identical_frames_for_every_mix() {
    for mix in AutoTransitionKind::ALL {
        let kind = ManualTransitionKind::from_mix(mix);
        let (frames, commands) = run(Path::Api, kind);
        assert_eq!(frames.len(), SCRIPT.len());
        assert_eq!(commands.len(), 12, "{kind:?}");
        // The lever moved: partway frames differ from both endpoints, a
        // reversal repeats an earlier picture, and the commit lands on
        // the incoming source while the cancel returns to the outgoing.
        // (Frames are compared with `assert!` to keep pixels out of the
        // failure message.)
        assert!(
            frames[1..7].iter().any(|frame| *frame != frames[0]),
            "{kind:?} moves"
        );
        assert!(frames[4] == frames[2], "{kind:?} reverses exactly");
        assert!(frames[12] == frames[8], "{kind:?} cancels at the boundary");
        assert!(frames[8] != frames[0], "{kind:?} commits at the boundary");
        assert!(frames[7] == frames[8], "{kind:?} commits on its frame");
        for path in [Path::Lever, Path::Keys, Path::Controller] {
            let (path_frames, path_commands) = run(path, kind);
            assert_eq!(path_commands, commands, "{path:?} {kind:?}");
            let first_difference = path_frames
                .iter()
                .zip(&frames)
                .position(|(path_frame, frame)| path_frame != frame);
            assert_eq!(first_difference, None, "{path:?} {kind:?}");
        }
    }
}
