//! The vMix-compatible TCP and HTTP APIs served by the daemon (RC-009).
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
    LiveInputSource, ManualTransitionKind, ManualTransitionPosition, ManualTransitionStatus,
    NetworkStreamConfig, SnapshotMessage, StingerAudioPolicy, StingerMissingMediaFallback,
    StingerStatus, TitleAirRequirement, TitleFieldUpdate, TitleFieldValue,
    TransitionFavoriteEffect, WireInputId, WireOverlayChannelId, WireRecorderSlot,
    WireStingerSlotId,
};
use fm_vmix_compat::{
    CompatAudioBus, CompatInput, CompatIntent, CompatState, TextFieldSelector, Translation,
    VmixHttpServer, VmixTcpServer,
};

/// A bound vMix listener and the compat-only state between passes.
pub(crate) struct VmixBridge {
    server: Option<VmixTcpServer>,
    http: Option<VmixHttpServer>,
    transition_millis: u32,
    transition_effect: TransitionFavoriteEffect,
    next_command: u64,
}

impl VmixBridge {
    pub(crate) fn new(server: Option<VmixTcpServer>, http: Option<VmixHttpServer>) -> Self {
        Self {
            server,
            http,
            transition_millis: 500,
            transition_effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            next_command: 0,
        }
    }

    /// The TCP listener's bound address, if one was requested.
    ///
    /// # Errors
    ///
    /// Returns the listener's address-lookup failure.
    pub(crate) fn tcp_address(&self) -> std::io::Result<Option<std::net::SocketAddr>> {
        self.server
            .as_ref()
            .map(VmixTcpServer::local_addr)
            .transpose()
    }

    /// The HTTP listener's bound address, if one was requested.
    ///
    /// # Errors
    ///
    /// Returns the listener's address-lookup failure.
    pub(crate) fn http_address(&self) -> std::io::Result<Option<std::net::SocketAddr>> {
        self.http
            .as_ref()
            .map(VmixHttpServer::local_addr)
            .transpose()
    }

    /// Accepts, reads, dispatches, writes, and publishes one readiness pass.
    pub(crate) fn poll(
        &mut self,
        control: &mut ControlService<Policy>,
        principal: &fm_auth::Principal,
        now_millis: u64,
    ) {
        if self.server.is_some() {
            self.poll_tcp(control, principal, now_millis);
        }
        if self.http.is_some() {
            self.poll_http(control, principal, now_millis);
        }
    }

    fn poll_tcp(
        &mut self,
        control: &mut ControlService<Policy>,
        principal: &fm_auth::Principal,
        now_millis: u64,
    ) {
        let snapshot = control.snapshot().snapshot.clone();
        let Self {
            server,
            http: _,
            transition_millis,
            transition_effect,
            next_command,
        } = self;
        let Some(server) = server.as_mut() else {
            return;
        };
        let Some(mut state) = compat_state(&snapshot, *transition_millis, *transition_effect)
        else {
            return;
        };
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

    fn poll_http(
        &mut self,
        control: &mut ControlService<Policy>,
        principal: &fm_auth::Principal,
        now_millis: u64,
    ) {
        let snapshot = control.snapshot().snapshot.clone();
        let Self {
            server: _,
            http,
            transition_millis,
            transition_effect,
            next_command,
        } = self;
        let Some(http) = http.as_mut() else {
            return;
        };
        let Some(mut state) = compat_state(&snapshot, *transition_millis, *transition_effect)
        else {
            return;
        };
        let result = http.poll(&mut state, &mut |state, translation| {
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
            eprintln!("FREEMIXD_VMIX_HTTP\tv=1\terror={error}");
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
                audio_strip_payload(strip, Some(percent_to_millidb(percent)), None, None, None),
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
                audio_strip_payload(strip, None, Some(muted), None, None),
                next_command,
            )
        }
        CompatIntent::SetInputBalance {
            input,
            balance_basis_points,
        } => {
            let strip = snapshot
                .input_audio_strips
                .iter()
                .find(|strip| strip.input == input)
                .ok_or_else(|| format!("input {} has no audio strip", input.get()))?;
            submit(
                control,
                principal,
                now_millis,
                audio_strip_payload(strip, None, None, Some(balance_basis_points), None),
                next_command,
            )
        }
        CompatIntent::SetText {
            input,
            field,
            value,
        } => {
            let title = control
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
        CompatIntent::SetFader { basis_points } => {
            let position = ManualTransitionPosition::new(basis_points)
                .ok_or_else(|| format!("fader position {basis_points} is outside 0..=10000"))?;
            let active = matches!(
                snapshot.desired_manual_transition,
                ManualTransitionStatus::Active(_)
            );
            if basis_points == 0 {
                if active {
                    submit(
                        control,
                        principal,
                        now_millis,
                        CommandPayload::CancelManualTransition,
                        next_command,
                    )?;
                }
                return Ok(());
            }
            if !active {
                let kind = match *transition_effect {
                    TransitionFavoriteEffect::Mix(mix) => ManualTransitionKind::from_mix(mix),
                    TransitionFavoriteEffect::Stinger(slot) => {
                        let duration = snapshot
                            .stingers
                            .iter()
                            .find(|stinger| stinger.slot == slot)
                            .map_or(1, |stinger| stinger.cut_point_frames.max(1));
                        ManualTransitionKind::Stinger {
                            slot,
                            duration_frames: duration,
                        }
                    }
                    TransitionFavoriteEffect::Cut => {
                        return Err("SetFader needs a mix or stinger transition effect".into());
                    }
                };
                submit(
                    control,
                    principal,
                    now_millis,
                    CommandPayload::StartManualTransition { kind },
                    next_command,
                )?;
            }
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::SetManualTransitionPosition { position },
                next_command,
            )?;
            if basis_points == ManualTransitionPosition::MAX {
                submit(
                    control,
                    principal,
                    now_millis,
                    CommandPayload::CommitManualTransition,
                    next_command,
                )?;
            }
            Ok(())
        }
        CompatIntent::SetAudioBus { input, bus, on } => {
            let strip = snapshot
                .input_audio_strips
                .iter()
                .find(|strip| strip.input == input)
                .ok_or_else(|| format!("input {} has no audio strip", input.get()))?;
            let sends = match bus {
                CompatAudioBus::Master => strip.sends.with_master(on),
                CompatAudioBus::Aux(number) => {
                    let bus = fm_types::audio_bus::AuxBusNumber::new(number)
                        .ok_or_else(|| format!("invalid aux bus: {number}"))?;
                    strip.sends.with_aux(bus, on)
                }
            };
            submit(
                control,
                principal,
                now_millis,
                audio_strip_payload(strip, None, None, None, Some(sends)),
                next_command,
            )
        }
        CompatIntent::SetStinger { slot, input } => {
            let slot = WireStingerSlotId::new(slot)
                .ok_or_else(|| format!("invalid stinger slot: {slot}"))?;
            let current = snapshot
                .stingers
                .iter()
                .find(|stinger| stinger.slot == slot);
            submit(
                control,
                principal,
                now_millis,
                stinger_payload(slot, input, current),
                next_command,
            )
        }
        CompatIntent::AddNetworkStream { url, name } => {
            let input =
                free_input_id(snapshot).ok_or_else(|| "no free input identifier".to_owned())?;
            submit(
                control,
                principal,
                now_millis,
                CommandPayload::AddInput {
                    input,
                    name: name.unwrap_or_else(|| "Network stream".to_owned()),
                    source: LiveInputSource::NetworkStream(Box::new(NetworkStreamConfig::new(url))),
                },
                next_command,
            )
        }
        other => Err(format!(
            "vMix function is not supported by this daemon yet: {other:?}"
        )),
    }
}

fn stinger_payload(
    slot: WireStingerSlotId,
    media_input: WireInputId,
    current: Option<&StingerStatus>,
) -> CommandPayload {
    CommandPayload::ConfigureStinger {
        slot,
        media_input,
        preload: current.is_none_or(|stinger| stinger.preload),
        cut_point_frames: current.map_or(0, |stinger| stinger.cut_point_frames),
        audio_policy: current.map_or(StingerAudioPolicy::StingerOnly, |stinger| {
            stinger.audio_policy
        }),
        missing_media_fallback: current.map_or(StingerMissingMediaFallback::Cut, |stinger| {
            stinger.missing_media_fallback
        }),
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
    let used = used.into_iter().collect::<std::collections::BTreeSet<_>>();
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
#[allow(clippy::cast_possible_truncation)] // the result is bounded to about -96..=0 dB
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
    balance_basis_points: Option<i32>,
    sends: Option<fm_types::audio_bus::AudioSends>,
) -> CommandPayload {
    CommandPayload::SetInputAudioStrip {
        input: strip.input,
        gain_millidb: gain_millidb.unwrap_or(strip.gain_millidb),
        balance_basis_points: balance_basis_points.unwrap_or(strip.balance_basis_points),
        pan_basis_points: strip.pan_basis_points,
        muted: muted.unwrap_or(strip.muted),
        soloed: strip.soloed,
        follow_video: strip.follow_video,
        delay_samples: strip.delay_samples,
        sends: sends.unwrap_or(strip.sends),
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
    fn audio_strip_payload_applies_a_bus_send_override() {
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
        let aux = fm_types::audio_bus::AuxBusNumber::new(2).unwrap();
        let sends = strip.sends.with_master(false).with_aux(aux, true);
        let CommandPayload::SetInputAudioStrip {
            input,
            gain_millidb,
            sends: got,
            ..
        } = audio_strip_payload(&strip, None, None, None, Some(sends))
        else {
            panic!("an audio strip payload");
        };
        assert_eq!(input, strip.input);
        assert_eq!(gain_millidb, strip.gain_millidb, "the gain is preserved");
        assert_eq!(got, sends);
        assert!(!got.master());
        assert!(got.aux(aux).enabled);
    }

    #[test]
    fn stinger_payload_keeps_an_existing_slots_configuration() {
        let slot = WireStingerSlotId::new(2).unwrap();
        let input = WireInputId::new(std::num::NonZeroU128::new(7).unwrap());
        let CommandPayload::ConfigureStinger {
            slot: got,
            media_input,
            preload,
            cut_point_frames,
            audio_policy,
            missing_media_fallback,
        } = stinger_payload(slot, input, None)
        else {
            panic!("a stinger payload");
        };
        assert_eq!(got, slot);
        assert_eq!(media_input, input);
        assert!(preload);
        assert_eq!(cut_point_frames, 0);
        assert_eq!(audio_policy, StingerAudioPolicy::StingerOnly);
        assert_eq!(missing_media_fallback, StingerMissingMediaFallback::Cut);

        let current = StingerStatus {
            slot,
            media_input: input,
            preload: false,
            cut_point_frames: 12,
            audio_policy: StingerAudioPolicy::MixWithProgram,
            missing_media_fallback: StingerMissingMediaFallback::Fade,
            readiness: fm_protocol::StingerReadiness::Ready,
        };
        let CommandPayload::ConfigureStinger {
            preload,
            cut_point_frames,
            audio_policy,
            missing_media_fallback,
            ..
        } = stinger_payload(slot, input, Some(&current))
        else {
            panic!("a stinger payload");
        };
        assert!(!preload);
        assert_eq!(cut_point_frames, 12);
        assert_eq!(audio_policy, StingerAudioPolicy::MixWithProgram);
        assert_eq!(missing_media_fallback, StingerMissingMediaFallback::Fade);
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
        assert_eq!(free_input_id_from([1_u128, 2, 4]).unwrap().get().get(), 3);
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
        } = audio_strip_payload(&strip, Some(-6_021), Some(true), Some(-2_500), None)
        else {
            panic!("an audio strip payload");
        };
        assert_eq!(input, strip.input);
        assert_eq!(gain_millidb, -6_021);
        assert_eq!(balance_basis_points, -2_500);
        assert_eq!(pan_basis_points, -200);
        assert!(muted);
        assert!(soloed);
        assert!(follow_video);
        assert_eq!(delay_samples, 48);
    }
}
