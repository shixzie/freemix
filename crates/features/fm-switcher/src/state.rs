use std::collections::BTreeSet;

use fm_types::{InputId, InputOrderError, OutputId, validate_input_order};

use crate::stream::{DesiredStream, StreamTargetId, collect_desired_streams};
use crate::{
    FadeToBlackAdvance, FadeToBlackController, FadeToBlackError, FadeToBlackFrame,
    FadeToBlackPosition, FadeToBlackRequest, FadeToBlackTarget, InputUsage,
    MAX_OVERLAY_TRANSITION_DURATION_FRAMES, MissingMediaFallback, OVERLAY_CHANNEL_COUNT,
    OverlayBorderPreset, OverlayChannelId, OverlayChannelState, OverlayPositionPreset,
    OverlayTransitionKind, RECORDER_SLOT_COUNT, RecorderSlot, STINGER_SLOT_COUNT,
    StingerDescriptor, StingerPlaybackDecision, StingerPreloadState, StingerSlotId,
    StingerSlotState, SwitcherCommand, SwitcherError, SwitcherEvent, TBarPosition, TBarState,
    TransitionKind, TransitionState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgramFrame {
    pub primary: InputId,
    pub secondary: Option<InputId>,
    pub transition_kind: Option<TransitionKind>,
    /// Point mix used to compose the video frame.
    pub mix_numerator: u32,
    pub mix_denominator: u32,
    /// Transition mix at the start of this frame's media interval.
    pub mix_start_numerator: u32,
    /// Transition mix at the end of this frame's media interval.
    pub mix_end_numerator: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwitcherState {
    inputs: Vec<InputId>,
    preview: InputId,
    program: InputId,
    transition: Option<TransitionState>,
    t_bar: Option<TBarState>,
    fade_to_black: FadeToBlackController,
    overlays: [OverlayChannelState; OVERLAY_CHANNEL_COUNT],
    stingers: [StingerSlotState; STINGER_SLOT_COUNT],
    streams: Vec<DesiredStream>,
    streams_running: BTreeSet<StreamTargetId>,
    recording_desired: [bool; RECORDER_SLOT_COUNT],
}

impl SwitcherState {
    /// Creates a mix with distinct initial program and preview inputs.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError`] if either selected input is unavailable or
    /// the two selections are the same input.
    pub fn new(
        inputs: Vec<InputId>,
        program: InputId,
        preview: InputId,
    ) -> Result<Self, SwitcherError> {
        let state = Self {
            inputs,
            preview,
            program,
            transition: None,
            t_bar: None,
            fade_to_black: FadeToBlackController::default(),
            overlays: std::array::from_fn(|_| OverlayChannelState::empty()),
            stingers: std::array::from_fn(|_| StingerSlotState::empty()),
            streams: Vec::new(),
            streams_running: BTreeSet::new(),
            recording_desired: [false; RECORDER_SLOT_COUNT],
        };
        state.require_input(program)?;
        state.require_input(preview)?;
        if program == preview {
            return Err(SwitcherError::PreviewEqualsProgram(preview));
        }
        Ok(state)
    }

    /// Replaces the bounded desired stream inventory with validated entries.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError`] when more than [`crate::MAX_STREAM_COUNT`]
    /// targets are supplied, an identifier repeats, or a name is blank or
    /// exceeds [`crate::MAX_STREAM_NAME_BYTES`] bytes. The inventory remains
    /// unchanged on error.
    pub fn with_streams(
        mut self,
        streams: impl IntoIterator<Item = (StreamTargetId, String)>,
    ) -> Result<Self, SwitcherError> {
        let desired = collect_desired_streams(streams)?;
        self.streams = desired;
        Ok(self)
    }

    #[must_use]
    pub fn inputs(&self) -> &[InputId] {
        &self.inputs
    }

    /// Replaces the input order with a permutation of the current inputs.
    ///
    /// # Errors
    ///
    /// Returns the [`InputOrderError`] from
    /// [`fm_types::validate_input_order`] and leaves the order unchanged.
    pub fn reorder_inputs(&mut self, inputs: Vec<InputId>) -> Result<(), InputOrderError> {
        validate_input_order(&self.inputs, &inputs)?;
        self.inputs = inputs;
        Ok(())
    }

    /// Appends a new input to the end of the mix's input order.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::DuplicateInput`] when the input already
    /// belongs to this mix; the order is unchanged.
    pub fn add_input(&mut self, input: InputId) -> Result<(), SwitcherError> {
        if self.inputs.contains(&input) {
            return Err(SwitcherError::DuplicateInput(input));
        }
        self.inputs.push(input);
        Ok(())
    }

    /// Returns the first reference that puts `input` on air, if any: Program,
    /// an automatic or manual transition endpoint, the media of the Stinger
    /// transition in flight, or the source of an overlay channel that is on
    /// air or fading.
    ///
    /// Unlike [`Self::input_usage`], Preview, overlay queues, and the media of
    /// a Stinger slot that is not playing are not on air and are ignored.
    #[must_use]
    pub fn on_air_usage(&self, input: InputId) -> Option<InputUsage> {
        if let Some(usage) = self.input_usage_on_bus(input) {
            return Some(usage);
        }
        if let Some(TransitionKind::Stinger(slot)) = self.transition.map(TransitionState::kind)
            && self
                .stinger(slot)
                .descriptor()
                .is_some_and(|descriptor| descriptor.media_input == input)
        {
            return Some(InputUsage::Stinger(slot));
        }
        self.overlay_channels_showing(input)
            .next()
            .map(InputUsage::Overlay)
    }

    /// Returns, in channel order, the overlay channels whose source is
    /// `input` while on air or fading.
    pub fn overlay_channels_showing(
        &self,
        input: InputId,
    ) -> impl Iterator<Item = OverlayChannelId> + '_ {
        OverlayChannelId::ALL
            .into_iter()
            .zip(&self.overlays)
            .filter(move |(_, overlay)| overlay.shows(input))
            .map(|(channel, _)| channel)
    }

    /// Returns the Program-bus reference to `input`: Program or a transition
    /// endpoint.
    fn input_usage_on_bus(&self, input: InputId) -> Option<InputUsage> {
        if self.program == input {
            return Some(InputUsage::Program);
        }
        if self
            .transition
            .is_some_and(|transition| transition.from() == input || transition.to() == input)
        {
            return Some(InputUsage::Transition);
        }
        if self
            .t_bar
            .is_some_and(|t_bar| t_bar.from() == input || t_bar.to() == input)
        {
            return Some(InputUsage::ManualTransition);
        }
        None
    }

    /// Returns the first switcher reference to `input`, if any: Program,
    /// Preview, a transition endpoint, the source of an overlay that is on
    /// air or fading, an overlay queue entry, or a Stinger slot's media.
    ///
    /// The source a settled-off overlay retains is not a use.
    #[must_use]
    pub fn input_usage(&self, input: InputId) -> Option<InputUsage> {
        if self.program == input {
            return Some(InputUsage::Program);
        }
        if self.preview == input {
            return Some(InputUsage::Preview);
        }
        if self
            .transition
            .is_some_and(|transition| transition.from() == input || transition.to() == input)
        {
            return Some(InputUsage::Transition);
        }
        if self
            .t_bar
            .is_some_and(|t_bar| t_bar.from() == input || t_bar.to() == input)
        {
            return Some(InputUsage::ManualTransition);
        }
        for (channel, overlay) in OverlayChannelId::ALL.into_iter().zip(&self.overlays) {
            if overlay.shows(input) {
                return Some(InputUsage::Overlay(channel));
            }
            if overlay.queued_sources().contains(&input) {
                return Some(InputUsage::OverlayQueue(channel));
            }
        }
        self.stingers
            .iter()
            .enumerate()
            .find(|(_, stinger)| {
                stinger
                    .descriptor()
                    .is_some_and(|descriptor| descriptor.media_input == input)
            })
            .and_then(|(index, _)| StingerSlotId::from_index(index))
            .map(InputUsage::Stinger)
    }

    /// Removes an input that no switcher state references, releasing it as
    /// the retained source of any settled-off overlay channel.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnknownInput`] for an input outside this mix
    /// and [`SwitcherError::InputInUse`] while [`Self::input_usage`] reports a
    /// reference; the order is unchanged.
    pub fn remove_input(&mut self, input: InputId) -> Result<(), SwitcherError> {
        self.require_input(input)?;
        if let Some(usage) = self.input_usage(input) {
            return Err(SwitcherError::InputInUse { input, usage });
        }
        self.inputs.retain(|candidate| *candidate != input);
        for overlay in &mut self.overlays {
            overlay.release(input);
        }
        Ok(())
    }

    #[must_use]
    pub const fn preview(&self) -> InputId {
        self.preview
    }

    #[must_use]
    pub const fn program(&self) -> InputId {
        self.program
    }

    #[must_use]
    pub const fn transition(&self) -> Option<TransitionState> {
        self.transition
    }

    #[must_use]
    pub const fn t_bar(&self) -> Option<TBarState> {
        self.t_bar
    }

    /// Restores an active manual transition into an otherwise idle switcher.
    ///
    /// # Errors
    ///
    /// Returns an error when a transition is already active, an endpoint is
    /// unavailable, the transition kind is not Fade, Wipe, `AlphaFade`, or Slide, or the
    /// saved endpoints do not match Program and Preview.
    pub fn restore_t_bar(&mut self, t_bar: TBarState) -> Result<(), SwitcherError> {
        self.require_idle()?;
        if !manual_transition_supported(t_bar.kind()) {
            return Err(SwitcherError::UnsupportedManualTransitionKind);
        }
        self.require_input(t_bar.from())?;
        self.require_input(t_bar.to())?;
        if t_bar.from() != self.program || t_bar.to() != self.preview {
            return Err(SwitcherError::InvalidManualTransitionRoute);
        }
        self.t_bar = Some(t_bar);
        Ok(())
    }

    #[must_use]
    pub const fn fade_to_black(&self) -> bool {
        self.fade_to_black.target().active()
    }

    #[must_use]
    pub const fn fade_to_black_position(&self) -> FadeToBlackPosition {
        self.fade_to_black.position()
    }

    /// Replaces Fade-to-Black state with a settled current-contract endpoint.
    pub fn restore_settled_fade_to_black(&mut self, active: bool) {
        self.fade_to_black = FadeToBlackController::settled(FadeToBlackTarget::from_active(active));
    }

    #[must_use]
    pub const fn fade_to_black_is_automatic(&self) -> bool {
        self.fade_to_black.is_automatic()
    }

    #[must_use]
    pub fn fade_to_black_frame(&self) -> FadeToBlackFrame {
        self.fade_to_black.frame()
    }

    #[must_use]
    pub fn overlays(&self) -> &[OverlayChannelState; OVERLAY_CHANNEL_COUNT] {
        &self.overlays
    }

    #[must_use]
    pub fn overlay(&self, channel: OverlayChannelId) -> &OverlayChannelState {
        &self.overlays[channel.index()]
    }

    #[must_use]
    pub fn stingers(&self) -> &[StingerSlotState; STINGER_SLOT_COUNT] {
        &self.stingers
    }

    #[must_use]
    pub fn stinger(&self, slot: StingerSlotId) -> &StingerSlotState {
        &self.stingers[slot.index()]
    }

    #[must_use]
    pub fn streams(&self) -> &[DesiredStream] {
        &self.streams
    }

    #[must_use]
    pub fn stream_running(&self, target: StreamTargetId) -> Option<bool> {
        self.streams
            .iter()
            .any(|stream| stream.id() == target)
            .then(|| self.streams_running.contains(&target))
    }

    #[must_use]
    pub fn running_stream_targets(&self) -> &BTreeSet<StreamTargetId> {
        &self.streams_running
    }

    /// Sets the desired running flag for one inventoried stream target.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnknownStreamTarget`] when the target is not
    /// part of this mix. Setting the current value again is accepted.
    pub fn set_stream_running(
        &mut self,
        target: StreamTargetId,
        running: bool,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        if !self.streams.iter().any(|stream| stream.id() == target) {
            return Err(SwitcherError::UnknownStreamTarget(target));
        }
        if running {
            self.streams_running.insert(target);
        } else {
            self.streams_running.remove(&target);
        }
        Ok(vec![SwitcherEvent::StreamRunningChanged {
            target,
            running,
        }])
    }

    #[must_use]
    pub const fn recording_desired(&self, slot: RecorderSlot) -> bool {
        self.recording_desired[slot.index()]
    }

    /// Sets the engine-owned desired recording flag for one recorder slot.
    ///
    /// Repeating the current value is accepted and emits no events. This
    /// operation is instant and never conflicts with transition state.
    ///
    /// # Errors
    ///
    /// Never fails; the [`SwitcherError`] result mirrors sibling desired-state
    /// setters for uniform call sites.
    pub fn set_recording_desired(
        &mut self,
        slot: RecorderSlot,
        active: bool,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        if self.recording_desired(slot) == active {
            return Ok(Vec::new());
        }
        self.recording_desired[slot.index()] = active;
        Ok(vec![SwitcherEvent::RecordingDesiredChanged {
            slot,
            active,
        }])
    }

    /// Applies one operator command atomically to desired switcher state.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError`] for unknown inputs or invalid transition state.
    pub fn apply(&mut self, command: SwitcherCommand) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        match command {
            SwitcherCommand::SelectPreview(input) => {
                self.require_idle()?;
                self.require_input(input)?;
                if input == self.program {
                    return Err(SwitcherError::PreviewEqualsProgram(input));
                }
                self.preview = input;
                Ok(vec![SwitcherEvent::PreviewSelected { input }])
            }
            SwitcherCommand::Cut => {
                self.require_idle()?;
                Ok(vec![self.complete_take(TransitionKind::Fade)])
            }
            SwitcherCommand::Transition {
                kind,
                duration_frames,
            } => {
                self.require_idle()?;
                if duration_frames == 0 {
                    return Err(SwitcherError::ZeroDuration);
                }
                self.start_transition(kind, duration_frames)
            }
            SwitcherCommand::Wipe { duration_frames } => {
                self.require_idle()?;
                if duration_frames == 0 {
                    return Err(SwitcherError::ZeroDuration);
                }
                self.start_transition(TransitionKind::Wipe, duration_frames)
            }
            SwitcherCommand::StartTBar { kind } => self.start_t_bar(kind),
            SwitcherCommand::SetTBarPosition(position) => self.set_t_bar_position(position),
            SwitcherCommand::CommitTBar => self.commit_t_bar(),
            SwitcherCommand::CancelTBar => self.cancel_t_bar(),
            SwitcherCommand::TakeOverlay { channel, source } => self.take_overlay(channel, source),
            SwitcherCommand::UpdateOverlay { channel, source } => {
                self.update_overlay(channel, source)
            }
            SwitcherCommand::OverlayOff(channel) => self.overlay_off(channel),
            SwitcherCommand::ConfigureOverlayTransition {
                channel,
                transition,
                duration_frames,
            } => self.configure_overlay_transition(channel, transition, duration_frames),
            SwitcherCommand::ConfigureOverlayAppearance {
                channel,
                position,
                border,
            } => Ok(self.configure_overlay_appearance(channel, position, border)),
            SwitcherCommand::QueueOverlay { channel, source } => {
                self.queue_overlay(channel, source)
            }
            SwitcherCommand::TakeNextOverlay(channel) => self.take_next_overlay(channel),
            SwitcherCommand::SetOverlayOutputInclusion {
                channel,
                output,
                included,
            } => Ok(self.set_overlay_output_inclusion(channel, output, included)),
            SwitcherCommand::SetStreamRunning { target, running } => {
                self.set_stream_running(target, running)
            }
            SwitcherCommand::SetRecordingDesired { slot, active } => {
                self.set_recording_desired(slot, active)
            }
        }
    }

    #[must_use]
    pub fn program_frame(&self) -> ProgramFrame {
        if let Some(transition) = self.transition {
            ProgramFrame {
                primary: transition.from(),
                secondary: Some(transition.to()),
                transition_kind: Some(transition.kind()),
                mix_numerator: transition.mix_numerator(),
                mix_denominator: transition.mix_denominator(),
                mix_start_numerator: transition.mix_numerator(),
                mix_end_numerator: transition.mix_end_numerator(),
            }
        } else if let Some(t_bar) = self.t_bar {
            ProgramFrame {
                primary: t_bar.from(),
                secondary: Some(t_bar.to()),
                transition_kind: Some(t_bar.kind()),
                mix_numerator: u32::from(t_bar.position().basis_points()),
                mix_denominator: u32::from(TBarPosition::MAX),
                mix_start_numerator: u32::from(t_bar.interval_start().basis_points()),
                mix_end_numerator: u32::from(t_bar.position().basis_points()),
            }
        } else {
            ProgramFrame {
                primary: self.program,
                secondary: None,
                transition_kind: None,
                mix_numerator: 0,
                mix_denominator: 1,
                mix_start_numerator: 0,
                mix_end_numerator: 0,
            }
        }
    }

    /// Advances one frame and reports all Program/Preview and FTB control events.
    #[must_use]
    pub fn advance_frame_events(&mut self) -> Vec<SwitcherEvent> {
        let fade_to_black = self.advance_fade_to_black();
        if let Some(t_bar) = &mut self.t_bar {
            t_bar.settle_frame();
        }
        let mut events = Vec::new();
        if let Some(mut transition) = self.transition {
            if transition.advance() {
                let kind = transition.kind();
                self.transition = None;
                events.push(self.complete_take(kind));
                events.push(SwitcherEvent::TransitionCompleted {
                    kind,
                    program: self.program,
                });
            } else {
                self.transition = Some(transition);
            }
        }
        append_fade_to_black_events(&mut events, fade_to_black);
        for (channel, overlay) in OverlayChannelId::ALL
            .into_iter()
            .zip(self.overlays.iter_mut())
        {
            let advance = overlay.advance();
            if let Some(opacity) = advance.opacity_changed {
                events.push(SwitcherEvent::OverlayOpacityChanged { channel, opacity });
            }
            if let Some(active) = advance.completed {
                events.push(SwitcherEvent::OverlayTransitionCompleted { channel, active });
            }
        }
        events
    }

    /// Configures a stinger slot and clears any readiness result for its previous media.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnknownInput`] when the media input is not part
    /// of this mix.
    pub fn configure_stinger(
        &mut self,
        slot: StingerSlotId,
        descriptor: StingerDescriptor,
    ) -> Result<(), SwitcherError> {
        self.require_input(descriptor.media_input)?;
        self.stingers[slot.index()].configure(descriptor);
        Ok(())
    }

    /// Removes one Stinger slot and its readiness state.
    pub fn remove_stinger(&mut self, slot: StingerSlotId) {
        self.stingers[slot.index()].clear();
    }

    /// Records the deterministic result of preloading a stinger's media.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnconfiguredStinger`] when the slot has no
    /// retained media descriptor.
    pub fn preload_stinger(
        &mut self,
        slot: StingerSlotId,
        media_available: bool,
    ) -> Result<SwitcherEvent, SwitcherError> {
        if self.stinger(slot).descriptor().is_none() {
            return Err(SwitcherError::UnconfiguredStinger(slot));
        }
        let state = if media_available {
            StingerPreloadState::Ready
        } else {
            StingerPreloadState::Missing
        };
        self.stingers[slot.index()].set_preload_state(state);
        Ok(SwitcherEvent::StingerPreloadChanged { slot, state })
    }

    #[must_use]
    pub fn stinger_playback_decision(&self, slot: StingerSlotId) -> StingerPlaybackDecision {
        let stinger = self.stinger(slot);
        let Some(descriptor) = stinger.descriptor() else {
            return StingerPlaybackDecision::Unconfigured;
        };
        if stinger.preload_state() == StingerPreloadState::Ready {
            StingerPlaybackDecision::Play
        } else {
            StingerPlaybackDecision::Fallback(descriptor.missing_media_fallback)
        }
    }

    /// Starts a reversible manual transition at the zero position.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::TransitionInProgress`] when another transition
    /// is active, or [`SwitcherError::UnsupportedManualTransitionKind`] when
    /// `kind` is not Fade, Wipe, `AlphaFade`, or Slide.
    pub fn start_t_bar(
        &mut self,
        kind: TransitionKind,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_idle()?;
        if !manual_transition_supported(kind) {
            return Err(SwitcherError::UnsupportedManualTransitionKind);
        }
        self.t_bar = Some(TBarState::new(kind, self.program, self.preview));
        Ok(vec![SwitcherEvent::TBarStarted {
            kind,
            from: self.program,
            to: self.preview,
        }])
    }

    /// Moves an active T-bar to an absolute position; lower positions reverse its progress.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::TransitionInProgress`] when no manual transition is active.
    pub fn set_t_bar_position(
        &mut self,
        position: TBarPosition,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        let t_bar = self
            .t_bar
            .as_mut()
            .ok_or(SwitcherError::TransitionInProgress)?;
        t_bar.set_position(position);
        Ok(vec![SwitcherEvent::TBarPositionChanged { position }])
    }

    /// Completes the active manual transition and swaps program with preview.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::TransitionInProgress`] when no manual transition is active.
    pub fn commit_t_bar(&mut self) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        let t_bar = self
            .t_bar
            .take()
            .ok_or(SwitcherError::TransitionInProgress)?;
        let program_changed = self.complete_take(t_bar.kind());
        Ok(vec![
            program_changed,
            SwitcherEvent::TransitionCompleted {
                kind: t_bar.kind(),
                program: self.program,
            },
        ])
    }

    /// Cancels the active manual transition without changing program or preview.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::TransitionInProgress`] when no manual transition is active.
    pub fn cancel_t_bar(&mut self) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.t_bar
            .take()
            .ok_or(SwitcherError::TransitionInProgress)?;
        Ok(vec![SwitcherEvent::TBarCancelled])
    }

    /// Starts or reverses an automatic FTB move independently of Program/Preview.
    ///
    /// Repeating the current target is idempotent and does not restart progress.
    ///
    /// # Errors
    ///
    /// Returns [`FadeToBlackError`] for a zero or oversized duration.
    pub fn request_fade_to_black(
        &mut self,
        active: bool,
        duration_frames: u32,
    ) -> Result<Vec<SwitcherEvent>, FadeToBlackError> {
        let target = FadeToBlackTarget::from_active(active);
        Ok(match self.fade_to_black.request(target, duration_frames)? {
            FadeToBlackRequest::Unchanged => Vec::new(),
            FadeToBlackRequest::Started(started) => {
                vec![SwitcherEvent::FadeToBlackStarted {
                    from: started.from,
                    target: started.target,
                    duration_frames: started.duration_frames,
                }]
            }
            FadeToBlackRequest::Completed(target) => {
                vec![SwitcherEvent::FadeToBlackCompleted {
                    active: target.active(),
                }]
            }
        })
    }

    /// Configures and activates an overlay channel.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnknownInput`] when the source is unavailable.
    pub fn take_overlay(
        &mut self,
        channel: OverlayChannelId,
        source: InputId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_input(source)?;
        self.overlays[channel.index()].take(source);
        Ok(vec![SwitcherEvent::OverlayTaken { channel, source }])
    }

    /// Replaces the source of a configured overlay without changing its on/off state.
    ///
    /// # Errors
    ///
    /// Returns [`SwitcherError::UnknownInput`] when the source is unavailable.
    pub fn update_overlay(
        &mut self,
        channel: OverlayChannelId,
        source: InputId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_input(source)?;
        self.overlays[channel.index()].update(source);
        Ok(vec![SwitcherEvent::OverlayUpdated { channel, source }])
    }

    /// Turns off a configured overlay while retaining its source and output routing.
    ///
    /// # Errors
    ///
    /// This operation is idempotent, including for an unconfigured channel.
    pub fn overlay_off(
        &mut self,
        channel: OverlayChannelId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.overlays[channel.index()].off();
        Ok(vec![SwitcherEvent::OverlayTurnedOff { channel }])
    }

    /// Configures the transition used by subsequent Take and Off operations.
    ///
    /// # Errors
    ///
    /// Returns an invalid-duration error unless `duration_frames` is in the
    /// inclusive supported range.
    pub fn configure_overlay_transition(
        &mut self,
        channel: OverlayChannelId,
        transition: OverlayTransitionKind,
        duration_frames: u32,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        if duration_frames == 0 || duration_frames > MAX_OVERLAY_TRANSITION_DURATION_FRAMES {
            return Err(SwitcherError::InvalidOverlayTransitionDuration {
                duration_frames,
                maximum: MAX_OVERLAY_TRANSITION_DURATION_FRAMES,
            });
        }
        self.overlays[channel.index()].configure_transition(transition, duration_frames);
        Ok(vec![SwitcherEvent::OverlayTransitionConfigured {
            channel,
            transition,
            duration_frames,
        }])
    }

    /// Starts the configured realized Take motion for one overlay channel.
    ///
    /// # Errors
    ///
    /// Returns an unknown-input error when `source` is not part of this show.
    pub fn request_overlay_take(
        &mut self,
        channel: OverlayChannelId,
        source: InputId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_input(source)?;
        self.overlays[channel.index()].request_take(source);
        Ok(vec![SwitcherEvent::OverlayTaken { channel, source }])
    }

    /// Configures the downstream layout presets for one overlay channel.
    #[must_use]
    pub fn configure_overlay_appearance(
        &mut self,
        channel: OverlayChannelId,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    ) -> Vec<SwitcherEvent> {
        self.overlays[channel.index()].configure_appearance(position, border);
        vec![SwitcherEvent::OverlayAppearanceConfigured {
            channel,
            position,
            border,
        }]
    }

    /// Appends a source to one bounded overlay queue.
    ///
    /// # Errors
    ///
    /// Returns an unknown-input error or a queue-full error.
    pub fn queue_overlay(
        &mut self,
        channel: OverlayChannelId,
        source: InputId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_input(source)?;
        if !self.overlays[channel.index()].enqueue(source) {
            return Err(SwitcherError::OverlayQueueFull {
                channel,
                maximum: crate::MAX_OVERLAY_QUEUE_DEPTH,
            });
        }
        Ok(vec![SwitcherEvent::OverlayQueued {
            channel,
            source,
            depth: self.overlays[channel.index()].queued_sources().len(),
        }])
    }

    /// Pops and immediately takes the next queued source in desired state.
    ///
    /// # Errors
    ///
    /// Returns an empty-queue error when no source is queued.
    pub fn take_next_overlay(
        &mut self,
        channel: OverlayChannelId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        let source = self.overlays[channel.index()]
            .take_next()
            .ok_or(SwitcherError::OverlayQueueEmpty(channel))?;
        Ok(vec![SwitcherEvent::OverlayQueueAdvanced {
            channel,
            source,
            remaining: self.overlays[channel.index()].queued_sources().len(),
        }])
    }

    /// Pops and takes the next source using the configured realized transition.
    ///
    /// # Errors
    ///
    /// Returns an empty-queue error when no source is queued.
    pub fn request_overlay_take_next(
        &mut self,
        channel: OverlayChannelId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        let source = self.overlays[channel.index()]
            .request_take_next()
            .ok_or(SwitcherError::OverlayQueueEmpty(channel))?;
        Ok(vec![SwitcherEvent::OverlayQueueAdvanced {
            channel,
            source,
            remaining: self.overlays[channel.index()].queued_sources().len(),
        }])
    }

    /// Takes `source` onto one overlay channel at full opacity on this frame,
    /// whatever its configured transition: the source animates its own
    /// entrance.
    ///
    /// # Errors
    ///
    /// Returns an unknown-input error when `source` is not part of this show.
    pub fn request_overlay_cut_take(
        &mut self,
        channel: OverlayChannelId,
        source: InputId,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        self.require_input(source)?;
        self.overlays[channel.index()].take(source);
        Ok(vec![SwitcherEvent::OverlayTaken { channel, source }])
    }

    /// Holds one overlay channel on air at its current opacity for exactly
    /// `frames` frames (this one included), then turns it off, whatever its
    /// configured transition: the source animates its own exit. While held
    /// the channel is transitioning, so its source stays on air.
    #[must_use]
    pub fn request_overlay_off_after(
        &mut self,
        channel: OverlayChannelId,
        frames: u32,
    ) -> Vec<SwitcherEvent> {
        self.overlays[channel.index()].request_off_after(frames);
        vec![SwitcherEvent::OverlayTurnedOff { channel }]
    }

    /// Starts the configured realized Off motion for one overlay channel.
    #[must_use]
    pub fn request_overlay_off(&mut self, channel: OverlayChannelId) -> Vec<SwitcherEvent> {
        self.overlays[channel.index()].request_off();
        vec![SwitcherEvent::OverlayTurnedOff { channel }]
    }

    #[must_use]
    pub fn overlays_in_motion(&self) -> bool {
        self.overlays
            .iter()
            .any(OverlayChannelState::is_transitioning)
    }

    #[must_use]
    pub fn set_overlay_output_inclusion(
        &mut self,
        channel: OverlayChannelId,
        output: OutputId,
        included: bool,
    ) -> Vec<SwitcherEvent> {
        self.overlays[channel.index()].set_output_inclusion(output, included);
        vec![SwitcherEvent::OverlayOutputInclusionChanged {
            channel,
            output,
            included,
        }]
    }

    fn start_transition(
        &mut self,
        kind: TransitionKind,
        duration_frames: u32,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        if let TransitionKind::Stinger(slot) = kind {
            match self.stinger_playback_decision(slot) {
                StingerPlaybackDecision::Fallback(fallback) => {
                    return self.apply_stinger_fallback(slot, fallback, duration_frames);
                }
                StingerPlaybackDecision::Play => {
                    let cut_point_frames = self
                        .stinger(slot)
                        .descriptor()
                        .expect("playable stinger has a descriptor")
                        .cut_point_frames;
                    if cut_point_frames > duration_frames {
                        return Err(SwitcherError::StingerCutPointOutOfRange {
                            slot,
                            cut_point_frames,
                            duration_frames,
                        });
                    }
                }
                StingerPlaybackDecision::Unconfigured => {
                    return Err(SwitcherError::UnconfiguredStinger(slot));
                }
            }
        }
        self.transition = Some(TransitionState::new(
            kind,
            self.program,
            self.preview,
            duration_frames,
        ));
        Ok(vec![SwitcherEvent::TransitionStarted {
            kind,
            from: self.program,
            to: self.preview,
            duration_frames,
        }])
    }

    fn apply_stinger_fallback(
        &mut self,
        slot: StingerSlotId,
        fallback: MissingMediaFallback,
        duration_frames: u32,
    ) -> Result<Vec<SwitcherEvent>, SwitcherError> {
        let fallback_event = SwitcherEvent::StingerFallbackApplied { slot, fallback };
        match fallback {
            MissingMediaFallback::Cut => Ok(vec![
                fallback_event,
                self.complete_take(TransitionKind::Fade),
            ]),
            MissingMediaFallback::Fade => {
                let mut events = vec![fallback_event];
                events.extend(self.start_transition(TransitionKind::Fade, duration_frames)?);
                Ok(events)
            }
            MissingMediaFallback::KeepProgram => Ok(vec![fallback_event]),
        }
    }

    fn complete_take(&mut self, _kind: TransitionKind) -> SwitcherEvent {
        let previous = self.program;
        std::mem::swap(&mut self.program, &mut self.preview);
        SwitcherEvent::ProgramChanged {
            previous,
            program: self.program,
        }
    }

    fn require_input(&self, input: InputId) -> Result<(), SwitcherError> {
        if self.inputs.contains(&input) {
            Ok(())
        } else {
            Err(SwitcherError::UnknownInput(input))
        }
    }

    fn require_idle(&self) -> Result<(), SwitcherError> {
        if self.transition.is_none() && self.t_bar.is_none() {
            Ok(())
        } else {
            Err(SwitcherError::TransitionInProgress)
        }
    }

    fn advance_fade_to_black(&mut self) -> FadeToBlackAdvance {
        self.fade_to_black.advance()
    }
}

const fn manual_transition_supported(kind: TransitionKind) -> bool {
    matches!(
        kind,
        TransitionKind::Fade
            | TransitionKind::Wipe
            | TransitionKind::AlphaFade
            | TransitionKind::Slide
    )
}

fn append_fade_to_black_events(events: &mut Vec<SwitcherEvent>, advance: FadeToBlackAdvance) {
    if let Some(position) = advance.position_changed {
        events.push(SwitcherEvent::FadeToBlackPositionChanged { position });
    }
    if let Some(target) = advance.completed {
        events.push(SwitcherEvent::FadeToBlackCompleted {
            active: target.active(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StingerAudioPolicy;

    fn input(value: u128) -> InputId {
        InputId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    fn channel(number: u8) -> OverlayChannelId {
        OverlayChannelId::new(number).unwrap()
    }

    fn switcher() -> SwitcherState {
        SwitcherState::new((1..=6).map(input).collect(), input(1), input(2)).unwrap()
    }

    #[test]
    fn program_is_on_air_but_preview_is_not() {
        let state = switcher();
        assert_eq!(state.on_air_usage(input(1)), Some(InputUsage::Program));
        assert_eq!(state.input_usage(input(2)), Some(InputUsage::Preview));
        assert_eq!(state.on_air_usage(input(2)), None);
        assert_eq!(state.on_air_usage(input(3)), None);
    }

    #[test]
    fn transition_endpoints_are_on_air() {
        let mut state = switcher();
        state
            .apply(SwitcherCommand::Transition {
                kind: TransitionKind::Fade,
                duration_frames: 4,
            })
            .unwrap();
        // Program outranks the transition endpoint it also is.
        assert_eq!(state.on_air_usage(input(1)), Some(InputUsage::Program));
        assert_eq!(state.on_air_usage(input(2)), Some(InputUsage::Transition));

        let mut manual = switcher();
        manual
            .apply(SwitcherCommand::StartTBar {
                kind: TransitionKind::Fade,
            })
            .unwrap();
        manual
            .apply(SwitcherCommand::SetTBarPosition(
                TBarPosition::new(5_000).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            manual.on_air_usage(input(2)),
            Some(InputUsage::ManualTransition)
        );
    }

    #[test]
    fn overlays_are_on_air_while_shown_or_fading_but_not_queued() {
        let mut state = switcher();
        state
            .apply(SwitcherCommand::ConfigureOverlayTransition {
                channel: channel(1),
                transition: OverlayTransitionKind::Fade,
                duration_frames: 2,
            })
            .unwrap();
        state.request_overlay_take(channel(1), input(3)).unwrap();
        state.request_overlay_take(channel(3), input(3)).unwrap();
        state
            .apply(SwitcherCommand::QueueOverlay {
                channel: channel(2),
                source: input(4),
            })
            .unwrap();
        assert_eq!(
            state.on_air_usage(input(3)),
            Some(InputUsage::Overlay(channel(1)))
        );
        assert_eq!(
            state.overlay_channels_showing(input(3)).collect::<Vec<_>>(),
            vec![channel(1), channel(3)]
        );
        assert_eq!(
            state.input_usage(input(4)),
            Some(InputUsage::OverlayQueue(channel(2)))
        );
        assert_eq!(state.on_air_usage(input(4)), None);

        // A fade-out keeps the source on air until it completes.
        let _ = state.request_overlay_off(channel(1));
        let _ = state.request_overlay_off(channel(3));
        assert_eq!(
            state.on_air_usage(input(3)),
            Some(InputUsage::Overlay(channel(1)))
        );
        for _ in 0..2 {
            let _ = state.advance_frame_events();
        }
        assert_eq!(state.on_air_usage(input(3)), None);
        assert_eq!(state.overlay_channels_showing(input(3)).count(), 0);
    }

    #[test]
    fn stinger_media_is_not_on_air() {
        let mut state = switcher();
        let slot = StingerSlotId::new(1).unwrap();
        state
            .configure_stinger(
                slot,
                StingerDescriptor {
                    media_input: input(5),
                    preload: false,
                    cut_point_frames: 1,
                    audio_policy: StingerAudioPolicy::Muted,
                    missing_media_fallback: MissingMediaFallback::Cut,
                },
            )
            .unwrap();
        assert_eq!(state.input_usage(input(5)), Some(InputUsage::Stinger(slot)));
        assert_eq!(state.on_air_usage(input(5)), None);
    }

    #[test]
    fn playing_stinger_media_is_on_air_until_the_transition_completes() {
        let mut state = switcher();
        let slot = StingerSlotId::new(1).unwrap();
        state
            .configure_stinger(
                slot,
                StingerDescriptor {
                    media_input: input(5),
                    preload: true,
                    cut_point_frames: 1,
                    audio_policy: StingerAudioPolicy::Muted,
                    missing_media_fallback: MissingMediaFallback::Cut,
                },
            )
            .unwrap();
        let _ = state.preload_stinger(slot, true).unwrap();
        state
            .apply(SwitcherCommand::Transition {
                kind: TransitionKind::Stinger(slot),
                duration_frames: 3,
            })
            .unwrap();
        assert_eq!(
            state.on_air_usage(input(5)),
            Some(InputUsage::Stinger(slot))
        );
        // Other media and other inputs stay off air.
        assert_eq!(state.on_air_usage(input(6)), None);
        for _ in 0..3 {
            let _ = state.advance_frame_events();
        }
        assert!(state.transition().is_none());
        assert_eq!(state.on_air_usage(input(5)), None);
    }
}
