//! Bounded per-control coalescing of high-rate inbound control events.
//!
//! A moving fader can deliver hundreds of MIDI CC, 14-bit CC pairs, pitch
//! bends, or OSC floats per second, far more than the engine needs per control
//! tick. [`ControlCoalescer`] sits between the transport decoders and
//! [`crate::ControlMap::process`]: within one tick it keeps only the latest
//! absolute value per `(port, control)`, sums relative deltas, and preserves
//! every button edge in arrival order. Draining a tick yields at most one
//! event per continuous control, so the engine applies one target per control
//! per tick and the audio gain smoother interpolates between them.
//!
//! Coalescing only drops intermediate positions, never the final one, so soft
//! takeover in the control map still sees the hardware cross the target.
//! Nothing reads a clock; callers pass event timestamps and decide when a
//! tick ends by calling [`ControlCoalescer::drain`].

use crate::control::{ControlEvent, ControlId, ControlValue};
use core::fmt;
use fm_types::MediaTimestamp;
use std::collections::{BTreeMap, HashMap};

/// Largest pending-event capacity of one [`ControlCoalescer`].
pub const MAX_COALESCER_CAPACITY: usize = 4096;
/// Largest number of per-control policy overrides.
pub const MAX_COALESCE_POLICIES: usize = 4096;
/// Largest port name accepted by the coalescer, in bytes.
pub const MAX_COALESCE_PORT_BYTES: usize = 256;

/// How events of one control combine within a tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoalescePolicy {
    /// Keep only the newest value (absolute faders, knobs, OSC floats).
    Latest,
    /// Sum [`ControlValue::Delta`] steps (jog wheels); other values are kept
    /// in order.
    Sum,
    /// Keep every event in arrival order (buttons, relative 7-bit encoders
    /// whose steps arrive as absolute CC values).
    Ordered,
}

impl CoalescePolicy {
    /// The default policy for a value: absolute, float, and integer values
    /// coalesce to the latest, deltas sum, buttons stay ordered.
    #[must_use]
    pub const fn default_for(value: ControlValue) -> Self {
        match value {
            ControlValue::Absolute { .. } | ControlValue::Float(_) | ControlValue::Integer(_) => {
                Self::Latest
            }
            ControlValue::Delta(_) => Self::Sum,
            ControlValue::Button { .. } => Self::Ordered,
        }
    }
}

/// One drained event: the surviving value and how much it absorbed.
#[derive(Clone, Debug, PartialEq)]
pub struct CoalescedEvent {
    pub port: String,
    pub event: ControlEvent,
    /// Timestamp of the first event merged into this one.
    pub first_at: MediaTimestamp,
    /// Timestamp of the newest event merged into this one.
    pub last_at: MediaTimestamp,
    /// Number of earlier events this one superseded or summed.
    pub merged: u32,
}

/// What [`ControlCoalescer::offer`] did with an event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoalesceOutcome {
    /// The event occupies a new pending slot.
    Queued,
    /// The event replaced (latest) or was added to (sum) a pending value.
    Merged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoalesceError {
    InvalidCapacity(usize),
    InvalidPort(String),
    NonFiniteValue,
    /// Every pending slot is used; drain before offering more new controls.
    /// Updates to already-pending latest/sum controls still succeed.
    Full {
        capacity: usize,
    },
    TooManyPolicies {
        limit: usize,
    },
}

impl fmt::Display for CoalesceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity(capacity) => {
                write!(formatter, "coalescer capacity {capacity} is out of range")
            }
            Self::InvalidPort(port) => write!(formatter, "coalescer port {port:?} is invalid"),
            Self::NonFiniteValue => formatter.write_str("control value must be finite"),
            Self::Full { capacity } => {
                write!(formatter, "coalescer holds {capacity} pending events")
            }
            Self::TooManyPolicies { limit } => {
                write!(formatter, "coalesce policies exceed {limit}")
            }
        }
    }
}

impl std::error::Error for CoalesceError {}

/// Running counters since construction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoalescerTelemetry {
    pub offered: u64,
    pub merged: u64,
    pub rejected: u64,
    pub drained: u64,
    pub ticks: u64,
}

type SlotKey = (String, ControlId);

/// Bounded per-control, per-tick coalescer for inbound control events.
#[derive(Clone, Debug)]
pub struct ControlCoalescer {
    capacity: usize,
    pending: Vec<CoalescedEvent>,
    /// Pending slot of each mergeable `(port, control)` in this tick.
    mergeable: HashMap<SlotKey, usize>,
    policies: BTreeMap<SlotKey, CoalescePolicy>,
    telemetry: CoalescerTelemetry,
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= MAX_COALESCE_PORT_BYTES && !port.chars().any(char::is_control)
}

impl ControlCoalescer {
    /// Creates a coalescer holding at most `capacity` pending events per tick.
    ///
    /// # Errors
    ///
    /// Returns an error unless `1 <= capacity <= MAX_COALESCER_CAPACITY`.
    pub fn new(capacity: usize) -> Result<Self, CoalesceError> {
        if capacity == 0 || capacity > MAX_COALESCER_CAPACITY {
            return Err(CoalesceError::InvalidCapacity(capacity));
        }
        Ok(Self {
            capacity,
            pending: Vec::new(),
            mergeable: HashMap::new(),
            policies: BTreeMap::new(),
            telemetry: CoalescerTelemetry::default(),
        })
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of events the next drain returns.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub const fn telemetry(&self) -> CoalescerTelemetry {
        self.telemetry
    }

    /// Overrides the policy of one control on one port, e.g. [`CoalescePolicy::Ordered`]
    /// for a 7-bit relative encoder whose steps arrive as absolute CC values.
    /// Takes effect for events offered afterwards.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid port or the policy bound.
    pub fn set_policy(
        &mut self,
        port: &str,
        control: ControlId,
        policy: CoalescePolicy,
    ) -> Result<(), CoalesceError> {
        if !valid_port(port) {
            return Err(CoalesceError::InvalidPort(port.to_owned()));
        }
        let key = (port.to_owned(), control);
        if !self.policies.contains_key(&key) && self.policies.len() >= MAX_COALESCE_POLICIES {
            return Err(CoalesceError::TooManyPolicies {
                limit: MAX_COALESCE_POLICIES,
            });
        }
        self.policies.insert(key, policy);
        Ok(())
    }

    /// Removes a policy override, returning it.
    pub fn clear_policy(&mut self, port: &str, control: &ControlId) -> Option<CoalescePolicy> {
        self.policies.remove(&(port.to_owned(), control.clone()))
    }

    fn policy(&self, key: &SlotKey, value: ControlValue) -> CoalescePolicy {
        let policy = self
            .policies
            .get(key)
            .copied()
            .unwrap_or_else(|| CoalescePolicy::default_for(value));
        match (policy, value) {
            // Edges are never merged, whatever the override says, and only
            // deltas can be summed.
            (_, ControlValue::Button { .. }) | (CoalescePolicy::Sum, _)
                if !matches!(value, ControlValue::Delta(_)) =>
            {
                CoalescePolicy::Ordered
            }
            (policy, _) => policy,
        }
    }

    /// Offers one decoded event from `port` at `timestamp`.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid port, a non-finite float, or a full
    /// coalescer (the event is not retained and is counted as rejected).
    pub fn offer(
        &mut self,
        port: &str,
        event: ControlEvent,
        timestamp: MediaTimestamp,
    ) -> Result<CoalesceOutcome, CoalesceError> {
        if !valid_port(port) {
            return Err(CoalesceError::InvalidPort(port.to_owned()));
        }
        if matches!(event.value, ControlValue::Float(value) if !value.is_finite()) {
            return Err(CoalesceError::NonFiniteValue);
        }
        self.telemetry.offered += 1;
        let key = (port.to_owned(), event.control.clone());
        let policy = self.policy(&key, event.value);
        if policy != CoalescePolicy::Ordered
            && let Some(&index) = self.mergeable.get(&key)
        {
            let slot = &mut self.pending[index];
            let value = match (policy, slot.event.value, event.value) {
                (CoalescePolicy::Sum, ControlValue::Delta(pending), ControlValue::Delta(next)) => {
                    ControlValue::Delta(pending.saturating_add(next))
                }
                _ => event.value,
            };
            slot.event.value = value;
            slot.last_at = timestamp;
            slot.merged = slot.merged.saturating_add(1);
            self.telemetry.merged += 1;
            return Ok(CoalesceOutcome::Merged);
        }
        if self.pending.len() >= self.capacity {
            self.telemetry.rejected += 1;
            return Err(CoalesceError::Full {
                capacity: self.capacity,
            });
        }
        if policy != CoalescePolicy::Ordered {
            self.mergeable.insert(key, self.pending.len());
        }
        self.pending.push(CoalescedEvent {
            port: port.to_owned(),
            event,
            first_at: timestamp,
            last_at: timestamp,
            merged: 0,
        });
        Ok(CoalesceOutcome::Queued)
    }

    /// Ends the tick: returns pending events ordered by the arrival of each
    /// slot's first event, each carrying its newest (or summed) value.
    pub fn drain(&mut self) -> Vec<CoalescedEvent> {
        self.mergeable.clear();
        self.telemetry.ticks += 1;
        let drained = std::mem::take(&mut self.pending);
        self.telemetry.drained += u64::try_from(drained.len()).unwrap_or(u64::MAX);
        drained
    }

    /// Discards pending events from `port` (e.g. on detach), returning how
    /// many were dropped. Policies are kept.
    pub fn discard_port(&mut self, port: &str) -> usize {
        let before = self.pending.len();
        self.pending.retain(|pending| pending.port != port);
        self.mergeable.clear();
        for (index, pending) in self.pending.iter().enumerate() {
            let key = (pending.port.clone(), pending.event.control.clone());
            if self.policy(&key, pending.event.value) != CoalescePolicy::Ordered {
                self.mergeable.insert(key, index);
            }
        }
        before - self.pending.len()
    }
}
