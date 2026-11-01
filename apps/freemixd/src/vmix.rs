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
    LiveInputSource, NetworkStreamConfig, SnapshotMessage, TitleAirRequirement, TitleFieldUpdate,
    TitleFieldValue, TransitionFavoriteEffect, WireInputId, WireOverlayChannelId, WireRecorderSlot,
};
use fm_vmix_compat::{
    CompatInput, CompatIntent, CompatState, TextFieldSelector, Translation, VmixTcpServer,
};

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
        let snapshot = control.snapshot().snapshot.clone();
        let Some(mut state) =
            compat_state(&snapshot, self.transition_millis, self.transition_effect)
        else {
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
                &snapshot,
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute(
    control: &mut ControlService<Policy>,
    principal: &fm_auth::Principal,
    now_millis: u64,
    state: &mut CompatState,
    snapshot: &SnapshotMessage,
    translation: Translation,
    transition_millis: &mut u32,
    transition_effect: &mut TransitionFavoriteEffect,
    next_command: &mut u64,
) -> Result<(), String> {
    match translation {
        Translation::Protocol(payload) => {
            submit(control, principal, now_millis, payload, next_command)
        }
        Translation::Compat(intent) => execute_compat(
            control,
            principal,
            now_millis,
            state,
            snapshot,
            intent,
            transition_millis,
            transition_effect,
            next_command,
        ),
        Translation::Unsupported(report) => Err(report.reason.to_owned()),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_compat(
    control: &mut ControlService<Policy>,
    principal: &fm_auth::Principal,
    now_millis: u64,
    state: &mut CompatState,
    snapshot: &SnapshotMessage,
    intent: CompatIntent,
    transition_millis: &mut u32,
    transition_effect: &mut TransitionFavoriteEffect,
    next_command: &mut u64,
) -> Result<(), String> {
    match intent {
        CompatIntent::SetTransitionDuration { millis } => {
            *transition_millis = millis;
            Ok(())
        }
        CompatIntent::SetTransitionEffect { effect } => {
            *transition_effect = effect;
            Ok(())
        }
        CompatIntent::CutTo { input } => {
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::SelectPreview { input },
                next_command,
            )?;
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::Cut,
                next_command,
            )
        }
        CompatIntent::Fade {
            input,
            effect,
            duration_millis,
        } => {
            if let Some(input) = input {
                submit(
                    control,
                    principal,
                    now_millis,
                    CommandPayload::SelectPreview { input },
                    next_command,
                )?;
            }
            let duration_frames = millis_to_frames(snapshot.frame_rate, duration_millis);
            let payload = match effect {
                TransitionFavoriteEffect::Cut => CommandPayload::Cut,
                TransitionFavoriteEffect::Mix(kind) => CommandPayload::Transition {
                    kind,
                    duration_frames,
                },
                TransitionFavoriteEffect::Stinger(slot) => CommandPayload::Stinger {
                    slot,
                    duration_frames,
                },
            };
            submit(control, principal, now_millis, payload, next_command)
        }
        CompatIntent::ToggleOverlay { channel, input } => {
            let on =
                state.overlays.get(usize::from(channel).saturating_sub(1)) != Some(&Some(input));
            let channel = wire_channel(channel)?;
            let payload = if on {
                CommandPayload::TakeOverlay {
                    channel,
                    source: input,
                }
            } else {
                CommandPayload::OverlayOff { channel }
            };
            submit(control, principal, now_millis, payload, next_command)?;
            let _ = state.set_overlay(channel.number(), if on { Some(input) } else { None });
            Ok(())
        }
        CompatIntent::RestoreOverlay { channel } => {
            let restored = state
                .restore_overlay(channel)
                .map_err(|error| error.to_string())?;
            let channel = wire_channel(channel)?;
            let payload = match restored {
                Some(source) => CommandPayload::TakeOverlay { channel, source },
                None => CommandPayload::OverlayOff { channel },
            };
            submit(control, principal, now_millis, payload, next_command)
        }
        CompatIntent::AllOverlaysOff => {
            for channel in 1..=8_u8 {
                let index = usize::from(channel) - 1;
                if state.overlays.get(index).is_some_and(Option::is_some) {
                    submit(
                        control,
                        principal,
                        now_millis,
                        CommandPayload::OverlayOff {
                            channel: wire_channel(channel)?,
                        },
                        next_command,
                    )?;
                    if let Some(slot) = state.overlays.get_mut(index) {
                        *slot = None;
                    }
                }
            }
            Ok(())
        }
        CompatIntent::SetRecording { enabled } => submit(
            control,
            principal,
            now_millis,
            recording_payload(enabled),
            next_command,
        ),
        CompatIntent::ToggleRecording => submit(
            control,
            principal,
            now_millis,
            recording_payload(!state.recording),
            next_command,
        ),
        CompatIntent::SetStreaming { enabled, stream } => submit(
            control,
            principal,
            now_millis,
            streaming_payload(snapshot, enabled, stream)?,
            next_command,
        ),
        CompatIntent::ToggleStreaming { stream } => submit(
            control,
            principal,
            now_millis,
            streaming_payload(snapshot, !state.streaming, stream)?,
            next_command,
        ),
        CompatIntent::RenameInput { input, name } => submit(
            control,
            principal,
            now_millis,
            CommandPayload::RenameInput { input, name },
            next_command,
        ),
        CompatIntent::SetInputVolume { input, percent } => {
            let strip = snapshot
                .input_audio_strips
                .iter()
                .find(|strip| strip.input == input)
                .ok_or_else(|| format!("input {} has no audio strip", input.get()))?;
            submit(
                control,
                principal,
                now_millis,
                audio_strip_payload(strip, Some(percent_to_millidb(percent)), None),
                next_command,
            )
        }
        CompatIntent::SetInputMuted { input, muted } => {
            let strip = snapshot
                .input_audio_strips
                .iter()
                .find(|strip| strip.input == input)
                .ok_or_else(|| format!("input {} has no audio strip", input.get()))?;
            submit(
                control,
                principal,
                now_millis,
                audio_strip_payload(strip, None, Some(muted)),
                next_command,
            )
        }
        CompatIntent::SetText {
            input,
            field,
            value,
        } => {            let title = control
                .title_states()
                .into_iter()
                .find(|title| title.input == input)
                .ok_or_else(|| format!("input {} is not a title", input.get()))?;
            let field = match field {
                TextFieldSelector::Name(name) => {
                    title.fields.iter().find(|field| field.name == name)
                }
                TextFieldSelector::Index(index) => title.fields.get(index),
            }
            .ok_or_else(|| format!("title field not found on input {}", input.get()))?;
            let require = if title.timeline.is_some() {
                TitleAirRequirement::OnAir
            } else {
                TitleAirRequirement::OffAir
            };
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::SetTitleFields {
                    input,
                    base_revision: title.field_revision,
                    require,
                    fields: vec![TitleFieldUpdate {
                        field: field.field,
                        value: TitleFieldValue::Text(value),
                    }],
                },
                next_command,
            )
        }
        CompatIntent::ToggleFadeToBlack => {
            let active = !snapshot.desired_fade_to_black.target_active;
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::FadeToBlack {
                    active,
                    duration_frames: millis_to_frames(snapshot.frame_rate, *transition_millis),
                },
                next_command,
            )
        }
        CompatIntent::AddNetworkStream { url, name } => {
            let input = free_input_id(snapshot)
                .ok_or_else(|| "no free input identifier".to_owned())?;
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::AddInput {
                    input,
                    name: name.unwrap_or_else(|| "Network stream".to_owned()),
                    source: LiveInputSource::NetworkStream(Box::new(
                        NetworkStreamConfig::new(url),
                    )),
                },
                next_command,
            )
        }
        other => Err(format!(
            "vMix function is not supported by this daemon yet: {other:?}"
        )),
    }
}

fn wire_channel(channel: u8) -> Result<WireOverlayChannelId, String> {
    WireOverlayChannelId::new(channel).ok_or_else(|| format!("invalid overlay channel: {channel}"))
}

/// The lowest positive input identifier the snapshot does not use.
fn free_input_id(snapshot: &SnapshotMessage) -> Option<WireInputId> {
    free_input_id_from(snapshot.inputs.iter().map(|input| input.input.get().get()))
}

fn free_input_id_from(used: impl IntoIterator<Item = u128>) -> Option<WireInputId> {
    let used = used
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut candidate = 1_u128;
    loop {
        if !used.contains(&candidate) {
            return std::num::NonZeroU128::new(candidate).map(WireInputId::new);
        }
        candidate = candidate.checked_add(1)?;
    }
}

/// vMix's 0 through 100 volume as a fader curve: 100 is unity and 0 is the
/// engine's minimum, with 50 percent at about −6 dB.
fn percent_to_millidb(percent: u8) -> i32 {
    if percent == 0 {
        return -96_000;
    }
    let gain = 20.0 * f64::from(percent).log10() - 40.0;
    (gain * 1_000.0).round() as i32
}

fn audio_strip_payload(
    strip: &fm_protocol::InputAudioStripStatus,
    gain_millidb: Option<i32>,
    muted: Option<bool>,
) -> CommandPayload {
    CommandPayload::SetInputAudioStrip {
        input: strip.input,
        gain_millidb: gain_millidb.unwrap_or(strip.gain_millidb),
        balance_basis_points: strip.balance_basis_points,
        pan_basis_points: strip.pan_basis_points,
        muted: muted.unwrap_or(strip.muted),
        soloed: strip.soloed,
        follow_video: strip.follow_video,
        delay_samples: strip.delay_samples,
        sends: strip.sends,
    }
}

fn millis_to_frames(frame_rate: fm_types::FrameRate, millis: u32) -> u32 {
    let numerator = u64::from(frame_rate.numerator());
    let denominator = u64::from(frame_rate.denominator());
    let frames = u64::from(millis) * numerator / (1_000 * denominator);
    u32::try_from(frames.max(1)).unwrap_or(u32::MAX)
}

fn recording_payload(enabled: bool) -> CommandPayload {
    let recorder = WireRecorderSlot::FIRST;
    if enabled {
        CommandPayload::RecordStart { recorder }
    } else {
        CommandPayload::RecordStop { recorder }
    }
}

fn streaming_payload(
    snapshot: &SnapshotMessage,
    enabled: bool,
    stream: Option<u16>,
) -> Result<CommandPayload, String> {
    match stream {
        Some(index) => {
            let target = snapshot
                .streams
                .get(usize::from(index))
                .ok_or_else(|| format!("stream index {index} is not configured"))?
                .target;
            Ok(if enabled {
                CommandPayload::StreamStart { target }
            } else {
                CommandPayload::StreamStop { target }
            })
        }
        None => Ok(if enabled {
            CommandPayload::StreamStartAll
        } else {
            CommandPayload::StreamStopAll
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_to_frames_uses_the_snapshot_rate_and_never_returns_zero() {
        let thirty = fm_types::FrameRate::new(30, 1).unwrap();
        assert_eq!(millis_to_frames(thirty, 1_000), 30);
        assert_eq!(millis_to_frames(thirty, 33), 1);
        assert_eq!(millis_to_frames(thirty, 0), 1);
        let ntsc = fm_types::FrameRate::new(30_000, 1_001).unwrap();
        assert_eq!(millis_to_frames(ntsc, 1_000), 29);
    }

    #[test]
    fn recording_payload_targets_the_first_slot() {
        assert!(matches!(
            recording_payload(true),
            CommandPayload::RecordStart { recorder } if recorder == WireRecorderSlot::FIRST
        ));
        assert!(matches!(
            recording_payload(false),
            CommandPayload::RecordStop { recorder } if recorder == WireRecorderSlot::FIRST
        ));
    }

    #[test]
    fn wire_channels_are_one_through_eight() {
        assert_eq!(wire_channel(1).unwrap().number(), 1);
        assert_eq!(wire_channel(8).unwrap().number(), 8);
        assert!(wire_channel(0).is_err());
        assert!(wire_channel(9).is_err());
    }

    #[test]
    fn free_input_ids_skip_used_identifiers() {
        assert_eq!(
            free_input_id_from(std::iter::empty::<u128>())
                .unwrap()
                .get()
                .get(),
            1
        );
        assert_eq!(
            free_input_id_from([1_u128, 2, 4]).unwrap().get().get(),
            3
        );
    }

    #[test]
    fn percent_to_millidb_follows_a_fader_curve() {
        assert_eq!(percent_to_millidb(100), 0);
        assert_eq!(percent_to_millidb(0), -96_000);
        assert_eq!(percent_to_millidb(50), -6_021);
        assert_eq!(percent_to_millidb(10), -20_000);
    }

    #[test]
    fn audio_strip_payload_preserves_the_other_strip_fields() {
        let strip = fm_protocol::InputAudioStripStatus {
            input: fm_protocol::WireInputId::new(std::num::NonZeroU128::new(3).unwrap()),
            gain_millidb: -1_000,
            balance_basis_points: 100,
            pan_basis_points: -200,
            group: Some(2),
            muted: false,
            soloed: true,
            follow_video: true,
            delay_samples: 48,
            sends: fm_types::audio_bus::AudioSends::MASTER,
        };
        let CommandPayload::SetInputAudioStrip {
            input,
            gain_millidb,
            balance_basis_points,
            pan_basis_points,
            muted,
            soloed,
            follow_video,
            delay_samples,
            ..
        } = audio_strip_payload(&strip, Some(-6_021), Some(true))
        else {
            panic!("an audio strip payload");
        };
        assert_eq!(input, strip.input);
        assert_eq!(gain_millidb, -6_021);
        assert_eq!(balance_basis_points, 100);
        assert_eq!(pan_basis_points, -200);
        assert!(muted);
        assert!(soloed);
        assert!(follow_video);
        assert_eq!(delay_samples, 48);
    }
}
