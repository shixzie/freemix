//! Typed per-input trigger catalog with edge detection over caller-supplied
//! input observations and ordered delayed actions on a caller-owned clock.

use crate::{AutomationEvent, CommandIntent, Value};
use core::fmt;
use fm_scheduler::{ActionError, ActionId, ActionQueue, FrameNumber};
use fm_types::InputId;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum ordered actions carried by one input trigger binding.
pub const MAX_INPUT_TRIGGER_ACTIONS: usize = 32;
/// Maximum input trigger bindings held by one [`InputTriggerSet`].
pub const MAX_INPUT_TRIGGER_BINDINGS: usize = 1024;

/// Every per-input trigger the engine can raise.
///
/// Threshold variants fire once when the observed timeline crosses the
/// threshold moving forward between two consecutive observations. Overlay
/// variants with `channel: None` match any overlay channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputTrigger {
    TransitionIn,
    TransitionOut,
    OverlayIn { channel: Option<u8> },
    OverlayOut { channel: Option<u8> },
    Completion,
    CountdownCompleted,
    CountdownTime { elapsed_ms: u64 },
    CountdownRemaining { remaining_ms: u64 },
    PlaybackTime { position_ms: u64 },
    PlaybackRemaining { remaining_ms: u64 },
    CallConnected,
    CallDisconnected,
    ZoomActiveSpeaker,
    ReplayEventsCompleted,
}

impl InputTrigger {
    /// Stable event kind used when bridging to [`AutomationEvent`].
    #[must_use]
    pub const fn kind_name(self) -> &'static str {
        match self {
            Self::TransitionIn => "input.transition_in",
            Self::TransitionOut => "input.transition_out",
            Self::OverlayIn { .. } => "input.overlay_in",
            Self::OverlayOut { .. } => "input.overlay_out",
            Self::Completion => "input.completion",
            Self::CountdownCompleted => "input.countdown_completed",
            Self::CountdownTime { .. } => "input.countdown_time",
            Self::CountdownRemaining { .. } => "input.countdown_remaining",
            Self::PlaybackTime { .. } => "input.playback_time",
            Self::PlaybackRemaining { .. } => "input.playback_remaining",
            Self::CallConnected => "input.call_connected",
            Self::CallDisconnected => "input.call_disconnected",
            Self::ZoomActiveSpeaker => "input.zoom_active_speaker",
            Self::ReplayEventsCompleted => "input.replay_events_completed",
        }
    }

    const fn is_timeline(self) -> bool {
        matches!(
            self,
            Self::CountdownCompleted
                | Self::CountdownTime { .. }
                | Self::CountdownRemaining { .. }
                | Self::PlaybackTime { .. }
                | Self::PlaybackRemaining { .. }
        )
    }
}

/// Position within a bounded timeline (playback or countdown).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimelineObservation {
    pub position_ms: u64,
    pub duration_ms: u64,
}

impl TimelineObservation {
    #[must_use]
    pub const fn new(position_ms: u64, duration_ms: u64) -> Self {
        Self {
            position_ms,
            duration_ms,
        }
    }

    #[must_use]
    pub const fn remaining_ms(self) -> u64 {
        self.duration_ms.saturating_sub(self.position_ms)
    }
}

/// Replicated state of one input at a caller timestamp.
///
/// The flags are independent replicated facts, not an encoded state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputObservation {
    pub on_program: bool,
    pub overlays: BTreeSet<u8>,
    pub playback: Option<TimelineObservation>,
    pub countdown: Option<TimelineObservation>,
    /// Monotonic count of reached-end events, including loop wraps.
    pub completions: u64,
    /// Monotonic count of completed replay event playlists.
    pub replay_events_completed: u64,
    pub call_connected: bool,
    pub zoom_active_speaker: bool,
    /// Set for a seek, load, or reset: timeline triggers re-baseline instead
    /// of firing for the jump.
    pub discontinuity: bool,
}

/// One ordered action. `delay_ms` is relative to the previous action (or to
/// the trigger event for the first action).
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerAction<C> {
    pub delay_ms: u64,
    pub intent: CommandIntent<C>,
}

impl<C> TriggerAction<C> {
    #[must_use]
    pub const fn new(delay_ms: u64, intent: CommandIntent<C>) -> Self {
        Self { delay_ms, intent }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputTriggerBinding<C> {
    pub id: String,
    pub input: InputId,
    pub trigger: InputTrigger,
    pub actions: Vec<TriggerAction<C>>,
}

/// A detected trigger edge for one binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputTriggerMatch<'a> {
    pub binding_id: &'a str,
    pub input: InputId,
    /// The concrete trigger; overlay channels are always `Some`.
    pub trigger: InputTrigger,
    pub at_ms: u64,
}

impl InputTriggerMatch<'_> {
    /// Bridges the edge into a generic [`AutomationEvent`] whose source is the
    /// decimal input identifier.
    #[must_use]
    pub fn to_automation_event(&self) -> AutomationEvent {
        let mut event = AutomationEvent::new(self.trigger.kind_name(), self.at_ms);
        event.source = Some(self.input.to_string());
        let number = |value: u64| Value::Number(i64::try_from(value).unwrap_or(i64::MAX));
        match self.trigger {
            InputTrigger::OverlayIn { channel: Some(ch) }
            | InputTrigger::OverlayOut { channel: Some(ch) } => {
                event.fields.insert("channel".into(), number(u64::from(ch)));
            }
            InputTrigger::CountdownTime { elapsed_ms: ms }
            | InputTrigger::CountdownRemaining { remaining_ms: ms }
            | InputTrigger::PlaybackTime { position_ms: ms }
            | InputTrigger::PlaybackRemaining { remaining_ms: ms } => {
                event.fields.insert("threshold_ms".into(), number(ms));
            }
            _ => {}
        }
        event
    }
}

/// One due action from a fired binding.
#[derive(Clone, Debug, PartialEq)]
pub struct InputTriggerFire<C> {
    pub binding_id: String,
    pub input: InputId,
    pub trigger: InputTrigger,
    pub event_ms: u64,
    pub due_ms: u64,
    /// Zero-based position of the action in its binding.
    pub step: usize,
    pub intent: CommandIntent<C>,
}

/// Result of one observation: matched edges and the scheduled action IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputObserveReport {
    pub matches: Vec<(String, InputTrigger)>,
    pub scheduled: Vec<ActionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputTriggerError {
    EmptyId,
    DuplicateId(String),
    UnknownId(String),
    EmptyActions(String),
    TooManyActions { id: String, maximum: usize },
    TooManyBindings { maximum: usize },
    ClockRegression { last_ms: u64, now_ms: u64 },
    TimestampOverflow { binding_id: String },
    Scheduler(ActionError),
}

impl fmt::Display for InputTriggerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => formatter.write_str("input trigger id must not be empty"),
            Self::DuplicateId(id) => write!(formatter, "input trigger {id} already exists"),
            Self::UnknownId(id) => write!(formatter, "input trigger {id} does not exist"),
            Self::EmptyActions(id) => write!(formatter, "input trigger {id} has no actions"),
            Self::TooManyActions { id, maximum } => {
                write!(formatter, "input trigger {id} exceeds {maximum} actions")
            }
            Self::TooManyBindings { maximum } => {
                write!(formatter, "at most {maximum} input triggers may be bound")
            }
            Self::ClockRegression { last_ms, now_ms } => write!(
                formatter,
                "input observation at {now_ms}ms precedes previous {last_ms}ms"
            ),
            Self::TimestampOverflow { binding_id } => {
                write!(formatter, "input trigger {binding_id} delay overflowed")
            }
            Self::Scheduler(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for InputTriggerError {}

#[derive(Clone, Debug, PartialEq)]
struct PendingInputAction<C> {
    binding_id: String,
    input: InputId,
    trigger: InputTrigger,
    event_ms: u64,
    step: usize,
    intent: CommandIntent<C>,
}

#[derive(Clone, Debug)]
pub struct InputTriggerSet<C> {
    bindings: Vec<InputTriggerBinding<C>>,
    baselines: BTreeMap<InputId, InputObservation>,
    last_observed_ms: Option<u64>,
    pending: ActionQueue<PendingInputAction<C>>,
    pending_index: BTreeMap<ActionId, (String, InputId)>,
}

impl<C> Default for InputTriggerSet<C> {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
            baselines: BTreeMap::new(),
            last_observed_ms: None,
            pending: ActionQueue::default(),
            pending_index: BTreeMap::new(),
        }
    }
}

impl<C: Clone> InputTriggerSet<C> {
    /// Registers a binding. Registration order breaks ties between bindings
    /// whose actions become due at the same timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/duplicate ID, an empty or oversized action
    /// list, or when the binding bound is reached.
    pub fn bind(&mut self, binding: InputTriggerBinding<C>) -> Result<(), InputTriggerError> {
        if binding.id.is_empty() {
            return Err(InputTriggerError::EmptyId);
        }
        if self.bindings.iter().any(|entry| entry.id == binding.id) {
            return Err(InputTriggerError::DuplicateId(binding.id));
        }
        if binding.actions.is_empty() {
            return Err(InputTriggerError::EmptyActions(binding.id));
        }
        if binding.actions.len() > MAX_INPUT_TRIGGER_ACTIONS {
            return Err(InputTriggerError::TooManyActions {
                id: binding.id,
                maximum: MAX_INPUT_TRIGGER_ACTIONS,
            });
        }
        if self.bindings.len() >= MAX_INPUT_TRIGGER_BINDINGS {
            return Err(InputTriggerError::TooManyBindings {
                maximum: MAX_INPUT_TRIGGER_BINDINGS,
            });
        }
        self.bindings.push(binding);
        Ok(())
    }

    /// Removes a binding and cancels every action it already armed.
    ///
    /// # Errors
    ///
    /// Returns an error when no binding has `id`.
    pub fn unbind(&mut self, id: &str) -> Result<usize, InputTriggerError> {
        let before = self.bindings.len();
        self.bindings.retain(|binding| binding.id != id);
        if self.bindings.len() == before {
            return Err(InputTriggerError::UnknownId(id.to_owned()));
        }
        Ok(self.cancel_where(|binding, _| binding == id))
    }

    /// Cancels armed actions of one binding without unbinding it.
    pub fn cancel_binding(&mut self, id: &str) -> usize {
        self.cancel_where(|binding, _| binding == id)
    }

    /// Forgets an input that left the project: cancels its armed actions and
    /// drops its baseline so a later observation starts fresh.
    pub fn forget_input(&mut self, input: InputId) -> usize {
        self.baselines.remove(&input);
        self.cancel_where(|_, pending_input| pending_input == input)
    }

    /// Compares an observation with the previous one for the same input,
    /// detects edges for every binding on that input, and arms their ordered
    /// actions. The first observation of an input only establishes a baseline.
    /// Scheduling is all-or-nothing: on error no action from this call stays
    /// armed and the baseline is not advanced.
    ///
    /// # Errors
    ///
    /// Returns an error for a regressing clock, delay overflow, or exhausted
    /// scheduler IDs.
    pub fn observe(
        &mut self,
        input: InputId,
        observation: InputObservation,
        now_ms: u64,
    ) -> Result<InputObserveReport, InputTriggerError> {
        if let Some(last_ms) = self.last_observed_ms
            && now_ms < last_ms
        {
            return Err(InputTriggerError::ClockRegression { last_ms, now_ms });
        }
        let mut report = InputObserveReport::default();
        if let Some(previous) = self.baselines.get(&input) {
            let mut planned = Vec::new();
            for binding in self.bindings.iter().filter(|b| b.input == input) {
                for trigger in detect(binding.trigger, previous, &observation) {
                    let mut due = now_ms;
                    for (step, action) in binding.actions.iter().enumerate() {
                        due = due.checked_add(action.delay_ms).ok_or_else(|| {
                            InputTriggerError::TimestampOverflow {
                                binding_id: binding.id.clone(),
                            }
                        })?;
                        planned.push((
                            due,
                            PendingInputAction {
                                binding_id: binding.id.clone(),
                                input,
                                trigger,
                                event_ms: now_ms,
                                step,
                                intent: action.intent.clone(),
                            },
                        ));
                    }
                    report.matches.push((binding.id.clone(), trigger));
                }
            }
            for (due, action) in planned {
                let key = (action.binding_id.clone(), input);
                match self.pending.schedule(FrameNumber::new(due), action) {
                    Ok(id) => {
                        self.pending_index.insert(id, key);
                        report.scheduled.push(id);
                    }
                    Err(error) => {
                        for id in report.scheduled {
                            self.pending.cancel(id);
                            self.pending_index.remove(&id);
                        }
                        return Err(InputTriggerError::Scheduler(error));
                    }
                }
            }
        }
        self.baselines.insert(input, observation);
        self.last_observed_ms = Some(now_ms);
        Ok(report)
    }

    /// Lists edges an observation would produce without arming anything.
    #[must_use]
    pub fn preview(
        &self,
        input: InputId,
        observation: &InputObservation,
        now_ms: u64,
    ) -> Vec<InputTriggerMatch<'_>> {
        let Some(previous) = self.baselines.get(&input) else {
            return Vec::new();
        };
        self.bindings
            .iter()
            .filter(|binding| binding.input == input)
            .flat_map(|binding| {
                detect(binding.trigger, previous, observation)
                    .into_iter()
                    .map(move |trigger| InputTriggerMatch {
                        binding_id: &binding.id,
                        input,
                        trigger,
                        at_ms: now_ms,
                    })
            })
            .collect()
    }

    /// Drains actions due at or before `now_ms` in due time, then arming order.
    pub fn poll(&mut self, now_ms: u64) -> Vec<InputTriggerFire<C>> {
        self.pending
            .drain_due(FrameNumber::new(now_ms))
            .into_iter()
            .map(|scheduled| {
                self.pending_index.remove(&scheduled.id);
                let action = scheduled.action;
                InputTriggerFire {
                    binding_id: action.binding_id,
                    input: action.input,
                    trigger: action.trigger,
                    event_ms: action.event_ms,
                    due_ms: scheduled.target_frame.get(),
                    step: action.step,
                    intent: action.intent,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn bindings(&self) -> &[InputTriggerBinding<C>] {
        &self.bindings
    }

    fn cancel_where(&mut self, mut predicate: impl FnMut(&str, InputId) -> bool) -> usize {
        let ids: Vec<_> = self
            .pending_index
            .iter()
            .filter(|(_, (binding, input))| predicate(binding, *input))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.pending_index.remove(id);
            self.pending.cancel(*id);
        }
        ids.len()
    }
}

fn detect(
    trigger: InputTrigger,
    previous: &InputObservation,
    current: &InputObservation,
) -> Vec<InputTrigger> {
    if trigger.is_timeline() && current.discontinuity {
        return Vec::new();
    }
    let fired = match trigger {
        InputTrigger::TransitionIn => !previous.on_program && current.on_program,
        InputTrigger::TransitionOut => previous.on_program && !current.on_program,
        InputTrigger::OverlayIn { channel } => {
            return overlay_edges(&current.overlays, &previous.overlays, channel)
                .map(|ch| InputTrigger::OverlayIn { channel: Some(ch) })
                .collect();
        }
        InputTrigger::OverlayOut { channel } => {
            return overlay_edges(&previous.overlays, &current.overlays, channel)
                .map(|ch| InputTrigger::OverlayOut { channel: Some(ch) })
                .collect();
        }
        InputTrigger::Completion => current.completions > previous.completions,
        InputTrigger::ReplayEventsCompleted => {
            current.replay_events_completed > previous.replay_events_completed
        }
        InputTrigger::CallConnected => !previous.call_connected && current.call_connected,
        InputTrigger::CallDisconnected => previous.call_connected && !current.call_connected,
        InputTrigger::ZoomActiveSpeaker => {
            !previous.zoom_active_speaker && current.zoom_active_speaker
        }
        InputTrigger::CountdownCompleted => timelines(previous.countdown, current.countdown)
            .is_some_and(|(before, after)| before.remaining_ms() > 0 && after.remaining_ms() == 0),
        InputTrigger::CountdownTime { elapsed_ms } => {
            crossed_forward(previous.countdown, current.countdown, elapsed_ms)
        }
        InputTrigger::PlaybackTime { position_ms } => {
            crossed_forward(previous.playback, current.playback, position_ms)
        }
        InputTrigger::CountdownRemaining { remaining_ms } => {
            crossed_remaining(previous.countdown, current.countdown, remaining_ms)
        }
        InputTrigger::PlaybackRemaining { remaining_ms } => {
            crossed_remaining(previous.playback, current.playback, remaining_ms)
        }
    };
    if fired { vec![trigger] } else { Vec::new() }
}

fn overlay_edges<'a>(
    now: &'a BTreeSet<u8>,
    before: &'a BTreeSet<u8>,
    channel: Option<u8>,
) -> impl Iterator<Item = u8> + 'a {
    now.difference(before)
        .copied()
        .filter(move |ch| channel.is_none_or(|wanted| wanted == *ch))
}

fn timelines(
    previous: Option<TimelineObservation>,
    current: Option<TimelineObservation>,
) -> Option<(TimelineObservation, TimelineObservation)> {
    previous.zip(current)
}

fn crossed_forward(
    previous: Option<TimelineObservation>,
    current: Option<TimelineObservation>,
    threshold: u64,
) -> bool {
    timelines(previous, current).is_some_and(|(before, after)| {
        before.position_ms < threshold && threshold <= after.position_ms
    })
}

fn crossed_remaining(
    previous: Option<TimelineObservation>,
    current: Option<TimelineObservation>,
    threshold: u64,
) -> bool {
    timelines(previous, current).is_some_and(|(before, after)| {
        before.remaining_ms() > threshold && after.remaining_ms() <= threshold
    })
}
