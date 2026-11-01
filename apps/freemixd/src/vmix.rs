//! The vMix-compatible TCP API served by the daemon (RC-009).
//!
//! Every pass builds a [`CompatState`] from the control snapshot, dispatches
//! translated protocol commands through [`ControlService`] under the daemon's
//! development principal, and keeps the compat-only transition settings
//! between passes. A function with no native translation is answered with the
//! bridge's refusal rather than silently dropped.

use fm_auth::Policy;
use fm_control::ControlService;
use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult,
    SnapshotMessage, TransitionFavoriteEffect,
};
use fm_vmix_compat::{CompatInput, CompatIntent, CompatState, Translation, VmixTcpServer};

/// A bound vMix listener and the compat-only state between passes.
pub(crate) struct VmixBridge {
    server: VmixTcpServer,
    transition_millis: u32,
    transition_effect: TransitionFavoriteEffect,
    next_command: u64,
}

impl VmixBridge {
    pub(crate) fn new(server: VmixTcpServer) -> Self {
        Self {
            server,
            transition_millis: 500,
            transition_effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            next_command: 0,
        }
    }

    /// The bound address.
    ///
    /// # Errors
    ///
    /// Returns the listener's address-lookup failure.
    pub(crate) fn address(&self) -> std::io::Result<std::net::SocketAddr> {
        self.server.local_addr()
    }

    /// Accepts, reads, dispatches, writes, and publishes one readiness pass.
    pub(crate) fn poll(
        &mut self,
        control: &mut ControlService<Policy>,
        principal: &fm_auth::Principal,
        now_millis: u64,
    ) {
        let snapshot = control.snapshot();
        let Some(mut state) = compat_state(
            &snapshot.snapshot,
            self.transition_millis,
            self.transition_effect,
        ) else {
            return;
        };
        let Self {
            server,
            transition_millis,
            transition_effect,
            next_command,
        } = self;
        let result = server.poll(&mut state, &mut |state, translation| {
            execute(
                control,
                principal,
                now_millis,
                state,
                translation,
                transition_millis,
                transition_effect,
                next_command,
            )
        });
        if let Err(error) = result {
            eprintln!("FREEMIXD_VMIX\tv=1\terror={error}");
        }
    }
}

/// Builds the vMix-shaped document from the authoritative snapshot.
fn compat_state(
    snapshot: &SnapshotMessage,
    transition_millis: u32,
    transition_effect: TransitionFavoriteEffect,
) -> Option<CompatState> {
    let inputs = snapshot
        .inputs
        .iter()
        .map(|input| CompatInput::new(input.input, input.name.clone(), "Input"))
        .collect();
    let mut state = CompatState::new(
        "29.0.0.0",
        inputs,
        snapshot.realized_program,
        snapshot.realized_preview,
    )
    .ok()?;
    state.transition_millis = transition_millis;
    state.transition_effect = transition_effect;
    for overlay in &snapshot.realized_overlays {
        if overlay.active {
            let _ = state.set_overlay(overlay.channel.number(), overlay.source);
        }
    }
    state.recording = snapshot
        .recordings
        .iter()
        .any(|recording| recording.desired_active);
    state.streaming = snapshot.streams.iter().any(|stream| stream.desired_running);
    Some(state)
}

#[allow(clippy::too_many_arguments)]
fn execute(
    control: &mut ControlService<Policy>,
    principal: &fm_auth::Principal,
    now_millis: u64,
    state: &mut CompatState,
    translation: Translation,
    transition_millis: &mut u32,
    transition_effect: &mut TransitionFavoriteEffect,
    next_command: &mut u64,
) -> Result<(), String> {
    match translation {
        Translation::Protocol(payload) => {
            submit(control, principal, now_millis, payload, next_command)
        }
        Translation::Compat(CompatIntent::SetTransitionDuration { millis }) => {
            *transition_millis = millis;
            Ok(())
        }
        Translation::Compat(CompatIntent::SetTransitionEffect { effect }) => {
            *transition_effect = effect;
            Ok(())
        }
        Translation::Compat(other) => {
            let _ = state;
            Err(format!(
                "vMix function is not supported by this daemon yet: {other:?}"
            ))
        }
        Translation::Unsupported(report) => Err(report.reason.to_owned()),
    }
}

fn submit(
    control: &mut ControlService<Policy>,
    principal: &fm_auth::Principal,
    now_millis: u64,
    payload: CommandPayload,
    next_command: &mut u64,
) -> Result<(), String> {
    let id = format!("vmix-{}", *next_command);
    *next_command = next_command.wrapping_add(1);
    let message = CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: id.clone(),
        idempotency_key: id,
        expected_revision: None,
        deadline_ms: None,
        payload,
    };
    let submission = control
        .submit(principal, message, now_millis)
        .map_err(|error| error.to_string())?;
    match submission.output.result {
        CommandResult::Accepted { .. } => Ok(()),
        CommandResult::Rejected { message, .. } => Err(message),
    }
}
