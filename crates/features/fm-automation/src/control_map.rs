//! Device profiles, control bindings with range mapping, learn mode, and
//! reconnect re-association for MIDI/OSC/HID control surfaces.
//!
//! Devices are identified by a stable [`DeviceKey`] (for example
//! `vendor:product:serial`) and reach the map through a caller-named port.
//! Ports may change between connections; bindings follow the device key.
//! Every operation takes caller timestamps; nothing reads a clock or touches
//! a device.

use crate::CommandIntent;
use crate::control::{ControlEvent, ControlId, ControlValue};
use crate::osc::{osc_address_matches, validate_osc_address};
use core::fmt;
use fm_types::MediaTimestamp;
use std::collections::{BTreeMap, HashMap};

/// Maximum device profiles held by one [`ControlMap`].
pub const MAX_DEVICE_PROFILES: usize = 64;
/// Maximum bindings held by one [`ControlMap`].
pub const MAX_CONTROL_BINDINGS: usize = 1024;
/// Maximum bytes in a device key or binding identifier.
pub const MAX_CONTROL_KEY_BYTES: usize = 128;
/// Maximum bytes in a port or display name.
pub const MAX_PORT_NAME_BYTES: usize = 256;

fn valid_key(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CONTROL_KEY_BYTES
        && text.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_PORT_NAME_BYTES && !text.chars().any(char::is_control)
}

/// Stable identity of a physical device across reconnects and port renames.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceKey(String);

impl DeviceKey {
    /// # Errors
    ///
    /// Returns an error unless the key is 1..=128 printable, non-space ASCII
    /// bytes.
    pub fn new(key: impl Into<String>) -> Result<Self, ControlMapError> {
        let key = key.into();
        if valid_key(&key) {
            Ok(Self(key))
        } else {
            Err(ControlMapError::InvalidKey(key))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier of a control or feedback binding.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BindingId(String);

impl BindingId {
    /// # Errors
    ///
    /// Returns an error unless the identifier is 1..=128 printable, non-space
    /// ASCII bytes.
    pub fn new(id: impl Into<String>) -> Result<Self, ControlMapError> {
        let id = id.into();
        if valid_key(&id) {
            Ok(Self(id))
        } else {
            Err(ControlMapError::InvalidKey(id))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DeviceTransport {
    Midi,
    Osc,
    Hid,
}

impl DeviceTransport {
    fn carries(self, control: &ControlId) -> bool {
        match control {
            ControlId::Osc(_) => self == Self::Osc,
            ControlId::Hid { .. } => self == Self::Hid,
            _ => self == Self::Midi,
        }
    }
}

/// A known device: its stable key, display name, transport, and the port
/// name it was last seen on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    pub key: DeviceKey,
    pub name: String,
    pub transport: DeviceTransport,
    pub port_name: String,
}

/// A finite, increasing numeric span.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    minimum: f64,
    maximum: f64,
}

impl Span {
    /// # Errors
    ///
    /// Returns an error for non-finite or non-increasing bounds.
    pub fn new(minimum: f64, maximum: f64) -> Result<Self, ControlMapError> {
        if minimum.is_finite() && maximum.is_finite() && minimum < maximum {
            Ok(Self { minimum, maximum })
        } else {
            Err(ControlMapError::InvalidSpan)
        }
    }

    #[must_use]
    pub const fn minimum(self) -> f64 {
        self.minimum
    }

    #[must_use]
    pub const fn maximum(self) -> f64 {
        self.maximum
    }

    /// Maps a value into `0..=1`, clamping outside values.
    #[must_use]
    pub fn normalize(self, value: f64) -> f64 {
        ((value - self.minimum) / (self.maximum - self.minimum)).clamp(0.0, 1.0)
    }

    /// Maps `0..=1` back into the span.
    #[must_use]
    pub fn denormalize(self, normalized: f64) -> f64 {
        self.minimum + normalized.clamp(0.0, 1.0) * (self.maximum - self.minimum)
    }
}

/// Raw MIDI velocity window mapped onto `0..=1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VelocityRange {
    pub minimum: u8,
    pub maximum: u8,
}

/// Which button edges produce actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonEdge {
    Press,
    Release,
    PressAndRelease,
    /// Each press flips a latched state.
    Toggle,
}

/// Fader pickup behaviour for absolute controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pickup {
    Immediate,
    /// Ignore the hardware until it comes within `window` (normalized) of the
    /// target value or crosses it.
    SoftTakeover {
        window: f64,
    },
}

/// Relative (endless) encoder encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativeEncoding {
    /// 7-bit: `1..=63` = +n, `65..=127` = n - 128.
    TwosComplement,
    /// 7-bit: bit 6 is the sign, bits 0..=5 the magnitude.
    SignedBit,
    /// 7-bit: value minus 64.
    BinaryOffset,
    /// Signed `Delta`/`Integer` values (HID jog wheels).
    Signed,
}

impl RelativeEncoding {
    fn delta(self, value: ControlValue) -> Option<i32> {
        match (self, value) {
            (Self::Signed, ControlValue::Delta(delta) | ControlValue::Integer(delta)) => {
                Some(delta)
            }
            (Self::Signed, _) => None,
            (encoding, ControlValue::Absolute { raw, max: 127 }) => {
                let raw = i32::from(raw);
                Some(match encoding {
                    Self::TwosComplement if raw >= 64 => raw - 128,
                    Self::SignedBit if raw & 0x40 != 0 => -(raw & 0x3F),
                    Self::SignedBit => raw & 0x3F,
                    Self::BinaryOffset => raw - 64,
                    _ => raw,
                })
            }
            _ => None,
        }
    }
}

/// How a control value becomes an action value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Response {
    Button {
        edge: ButtonEdge,
        velocity: Option<VelocityRange>,
    },
    Absolute {
        /// Raw input span; `None` uses the natural range (`0..=max` for
        /// absolute values, `0..=1` for floats). Integers require a span.
        input: Option<Span>,
        output: Span,
        invert: bool,
        /// Minimum normalized change to emit (`0.0..0.5`); endpoints always
        /// pass.
        deadband: f64,
        pickup: Pickup,
    },
    Relative {
        encoding: RelativeEncoding,
        /// Normalized change per encoder tick (`0 < step <= 1`).
        step: f64,
        output: Span,
    },
}

impl Response {
    fn validate(&self) -> Result<(), ControlMapError> {
        let ok = match *self {
            Self::Button { velocity, .. } => {
                velocity.is_none_or(|range| range.minimum < range.maximum && range.maximum <= 127)
            }
            Self::Absolute {
                deadband, pickup, ..
            } => {
                (0.0..0.5).contains(&deadband)
                    && match pickup {
                        Pickup::Immediate => true,
                        Pickup::SoftTakeover { window } => window > 0.0 && window <= 0.5,
                    }
            }
            Self::Relative { step, .. } => step > 0.0 && step <= 1.0,
        };
        if ok {
            Ok(())
        } else {
            Err(ControlMapError::InvalidResponse)
        }
    }

    const fn output(&self) -> Option<Span> {
        match *self {
            Self::Button { .. } => None,
            Self::Absolute { output, .. } | Self::Relative { output, .. } => Some(output),
        }
    }

    /// Whether a value can drive this response (used to qualify learn).
    fn accepts(&self, value: ControlValue) -> bool {
        match *self {
            Self::Button { .. } => matches!(value, ControlValue::Button { pressed: true, .. }),
            Self::Absolute { input, .. } => match value {
                ControlValue::Absolute { max, .. } => max > 0,
                ControlValue::Float(_) => true,
                ControlValue::Integer(_) => input.is_some(),
                _ => false,
            },
            Self::Relative { encoding, .. } => encoding.delta(value).is_some(),
        }
    }
}

/// A binding from one device control to a typed action.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlBinding<A> {
    pub id: BindingId,
    pub device: DeviceKey,
    pub control: ControlId,
    pub action: A,
    pub response: Response,
}

/// The value carried with a mapped action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActionValue {
    /// Normalized velocity `0..=1` (`1.0` for non-velocity sources).
    Press {
        velocity: f64,
    },
    Release,
    Toggled(bool),
    /// A value in the binding's output span.
    Level(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlOutput<A> {
    pub binding: BindingId,
    pub action: A,
    pub value: ActionValue,
    pub timestamp: MediaTimestamp,
}

/// Why a matching binding emitted nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Suppressed {
    AwaitingPickup { hardware: f64, target: f64 },
    Deadband,
    NoChange,
    IncompatibleValue,
}

/// What learn mode did with an event.
#[derive(Clone, Debug, PartialEq)]
pub enum LearnEffect {
    /// Learn was idle; the event was dispatched to bindings.
    Inactive,
    /// The event was captured as the learn candidate.
    Captured,
    /// Learn swallowed a non-qualifying event (or one after capture).
    Ignored,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessReport<A> {
    pub device: DeviceKey,
    pub outputs: Vec<CommandIntent<ControlOutput<A>>>,
    pub suppressed: Vec<(BindingId, Suppressed)>,
    pub learn: LearnEffect,
}

/// Arms learn for one new binding.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnRequest<A> {
    pub binding: BindingId,
    /// Restrict capture to one device; `None` accepts any attached device.
    pub device: Option<DeviceKey>,
    pub action: A,
    pub response: Response,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LearnCandidate<A> {
    pub binding: ControlBinding<A>,
    /// Existing bindings on the same device control.
    pub conflicts: Vec<BindingId>,
    pub captured_at: MediaTimestamp,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum LearnState<A> {
    #[default]
    Idle,
    Armed(LearnRequest<A>),
    Captured(LearnCandidate<A>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictPolicy {
    /// Fail with [`ControlMapError::LearnConflict`] when conflicts exist.
    Reject,
    /// Remove conflicting bindings before inserting the learned one.
    ReplaceExisting,
    /// Keep conflicting bindings; the control drives several actions.
    KeepBoth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearnConfirmation {
    pub binding: BindingId,
    pub replaced: Vec<BindingId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachReport {
    pub device: DeviceKey,
    /// Increments on every attach of the device key.
    pub generation: u64,
    /// The port name remembered from the previous connection.
    pub previous_port: String,
    pub port_changed: bool,
    /// Bindings re-associated with the device.
    pub bindings: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ControlMapError {
    InvalidKey(String),
    InvalidName(String),
    InvalidSpan,
    InvalidResponse,
    InvalidControl(ControlId),
    TransportMismatch {
        device: DeviceKey,
        control: ControlId,
    },
    NonFiniteValue,
    DuplicateProfile(DeviceKey),
    TooManyProfiles {
        limit: usize,
    },
    UnknownDevice(DeviceKey),
    DeviceHasBindings(DeviceKey),
    DuplicateBinding(BindingId),
    TooManyBindings {
        limit: usize,
    },
    UnknownBinding(BindingId),
    /// The binding has no output span to receive a target value.
    NoTargetValue(BindingId),
    UnknownPort(String),
    AmbiguousPort(String),
    PortInUse {
        port: String,
        device: DeviceKey,
    },
    GenerationExhausted,
    LearnActive,
    LearnNotArmed,
    NothingCaptured,
    LearnConflict(Vec<BindingId>),
}

impl fmt::Display for ControlMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => write!(formatter, "control key {key:?} is invalid"),
            Self::InvalidName(name) => write!(formatter, "control name {name:?} is invalid"),
            Self::InvalidSpan => formatter.write_str("control span must be finite and increasing"),
            Self::InvalidResponse => formatter.write_str("control response is out of range"),
            Self::InvalidControl(control) => write!(formatter, "control {control:?} is invalid"),
            Self::TransportMismatch { device, control } => write!(
                formatter,
                "control {control:?} is not carried by device {}",
                device.as_str()
            ),
            Self::NonFiniteValue => formatter.write_str("control value must be finite"),
            Self::DuplicateProfile(key) => {
                write!(formatter, "device profile {} exists", key.as_str())
            }
            Self::TooManyProfiles { limit } => {
                write!(formatter, "device profiles exceed {limit}")
            }
            Self::UnknownDevice(key) => write!(formatter, "device {} is unknown", key.as_str()),
            Self::DeviceHasBindings(key) => {
                write!(formatter, "device {} still has bindings", key.as_str())
            }
            Self::DuplicateBinding(id) => write!(formatter, "binding {} exists", id.as_str()),
            Self::TooManyBindings { limit } => write!(formatter, "bindings exceed {limit}"),
            Self::UnknownBinding(id) => write!(formatter, "binding {} is unknown", id.as_str()),
            Self::NoTargetValue(id) => {
                write!(formatter, "binding {} has no target value", id.as_str())
            }
            Self::UnknownPort(port) => write!(formatter, "port {port:?} is not attached"),
            Self::AmbiguousPort(port) => {
                write!(formatter, "port {port:?} matches several profiles")
            }
            Self::PortInUse { port, device } => {
                write!(
                    formatter,
                    "port {port:?} is attached to {}",
                    device.as_str()
                )
            }
            Self::GenerationExhausted => formatter.write_str("device generation exhausted"),
            Self::LearnActive => formatter.write_str("control learn is already active"),
            Self::LearnNotArmed => formatter.write_str("control learn is not armed"),
            Self::NothingCaptured => formatter.write_str("control learn captured nothing"),
            Self::LearnConflict(ids) => {
                write!(
                    formatter,
                    "learned control conflicts with {} bindings",
                    ids.len()
                )
            }
        }
    }
}

impl std::error::Error for ControlMapError {}

fn validate_control(control: &ControlId) -> Result<(), ControlMapError> {
    let ok = match control {
        ControlId::MidiNote { note, .. } | ControlId::MidiPolyPressure { note, .. } => *note <= 127,
        ControlId::MidiCc { controller, .. } => *controller <= 127,
        ControlId::MidiCc14 { controller, .. } => *controller <= 31,
        ControlId::MidiProgram { program, .. } => *program <= 127,
        ControlId::Osc(address) => validate_osc_address(address).is_ok(),
        ControlId::MidiPitchBend { .. }
        | ControlId::MidiChannelPressure { .. }
        | ControlId::Hid { .. } => true,
    };
    if ok {
        Ok(())
    } else {
        Err(ControlMapError::InvalidControl(control.clone()))
    }
}

/// Binding control vs. event control. OSC events may carry patterns.
fn controls_match(binding: &ControlId, event: &ControlId) -> bool {
    match (binding, event) {
        (ControlId::Osc(address), ControlId::Osc(pattern)) => {
            osc_address_matches(pattern, address).unwrap_or(false)
        }
        _ => binding == event,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BindingState {
    pressed: bool,
    toggled: bool,
    picked_up: bool,
    last_hardware: Option<f64>,
    last_normalized: Option<f64>,
    target: Option<f64>,
    relative: Option<f64>,
}

impl BindingState {
    fn reset_transient(&mut self) {
        self.pressed = false;
        self.picked_up = false;
        self.last_hardware = None;
        self.last_normalized = None;
    }
}

#[derive(Clone, Debug)]
struct Attached {
    port: String,
    generation: u64,
}

/// Device profiles, bindings, learn mode, and live port associations.
#[derive(Clone, Debug)]
pub struct ControlMap<A> {
    profiles: BTreeMap<DeviceKey, DeviceProfile>,
    bindings: Vec<ControlBinding<A>>,
    states: HashMap<BindingId, BindingState>,
    ports: BTreeMap<String, DeviceKey>,
    attached: BTreeMap<DeviceKey, Attached>,
    generations: BTreeMap<DeviceKey, u64>,
    learn: LearnState<A>,
}

impl<A> Default for ControlMap<A> {
    fn default() -> Self {
        Self {
            profiles: BTreeMap::new(),
            bindings: Vec::new(),
            states: HashMap::new(),
            ports: BTreeMap::new(),
            attached: BTreeMap::new(),
            generations: BTreeMap::new(),
            learn: LearnState::Idle,
        }
    }
}

impl<A: Clone> ControlMap<A> {
    // ------------------------------------------------------------ profiles

    /// Registers a device profile.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid names, duplicates, or the profile bound.
    pub fn add_profile(&mut self, profile: DeviceProfile) -> Result<(), ControlMapError> {
        for name in [&profile.name, &profile.port_name] {
            if !valid_name(name) {
                return Err(ControlMapError::InvalidName(name.clone()));
            }
        }
        if self.profiles.contains_key(&profile.key) {
            return Err(ControlMapError::DuplicateProfile(profile.key));
        }
        if self.profiles.len() >= MAX_DEVICE_PROFILES {
            return Err(ControlMapError::TooManyProfiles {
                limit: MAX_DEVICE_PROFILES,
            });
        }
        self.profiles.insert(profile.key.clone(), profile);
        Ok(())
    }

    /// Removes a profile with no bindings, detaching it first.
    ///
    /// # Errors
    ///
    /// Returns an error when the device is unknown or still has bindings.
    pub fn remove_profile(&mut self, key: &DeviceKey) -> Result<DeviceProfile, ControlMapError> {
        if !self.profiles.contains_key(key) {
            return Err(ControlMapError::UnknownDevice(key.clone()));
        }
        if self.bindings.iter().any(|binding| &binding.device == key) {
            return Err(ControlMapError::DeviceHasBindings(key.clone()));
        }
        if let Some(attached) = self.attached.remove(key) {
            self.ports.remove(&attached.port);
        }
        self.generations.remove(key);
        self.profiles
            .remove(key)
            .ok_or_else(|| ControlMapError::UnknownDevice(key.clone()))
    }

    #[must_use]
    pub fn profile(&self, key: &DeviceKey) -> Option<&DeviceProfile> {
        self.profiles.get(key)
    }

    /// Profiles in device-key order.
    pub fn profiles(&self) -> impl Iterator<Item = &DeviceProfile> {
        self.profiles.values()
    }

    // ------------------------------------------------------------ bindings

    /// Adds a binding for a known device.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown devices, transport mismatch, invalid
    /// controls or responses, duplicate identifiers, or the binding bound.
    pub fn add_binding(&mut self, binding: ControlBinding<A>) -> Result<(), ControlMapError> {
        self.check_binding(&binding)?;
        if self.bindings.len() >= MAX_CONTROL_BINDINGS {
            return Err(ControlMapError::TooManyBindings {
                limit: MAX_CONTROL_BINDINGS,
            });
        }
        self.states
            .insert(binding.id.clone(), BindingState::default());
        self.bindings.push(binding);
        Ok(())
    }

    fn check_binding(&self, binding: &ControlBinding<A>) -> Result<(), ControlMapError> {
        let profile = self
            .profiles
            .get(&binding.device)
            .ok_or_else(|| ControlMapError::UnknownDevice(binding.device.clone()))?;
        validate_control(&binding.control)?;
        if !profile.transport.carries(&binding.control) {
            return Err(ControlMapError::TransportMismatch {
                device: binding.device.clone(),
                control: binding.control.clone(),
            });
        }
        binding.response.validate()?;
        if self.states.contains_key(&binding.id) {
            return Err(ControlMapError::DuplicateBinding(binding.id.clone()));
        }
        Ok(())
    }

    /// Removes a binding by identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is unknown.
    pub fn remove_binding(&mut self, id: &BindingId) -> Result<ControlBinding<A>, ControlMapError> {
        let index = self
            .bindings
            .iter()
            .position(|binding| &binding.id == id)
            .ok_or_else(|| ControlMapError::UnknownBinding(id.clone()))?;
        self.states.remove(id);
        Ok(self.bindings.remove(index))
    }

    /// Bindings in insertion (dispatch) order.
    #[must_use]
    pub fn bindings(&self) -> &[ControlBinding<A>] {
        &self.bindings
    }

    /// Bindings on `device` whose control equals `control`.
    #[must_use]
    pub fn conflicts(&self, device: &DeviceKey, control: &ControlId) -> Vec<BindingId> {
        self.bindings
            .iter()
            .filter(|binding| &binding.device == device && &binding.control == control)
            .map(|binding| binding.id.clone())
            .collect()
    }

    /// Records the current value of a bound target in output units. Used as
    /// the soft-takeover pickup point and as the base for relative encoders.
    /// If the target moves away from a picked-up fader by more than the
    /// window, pickup is required again.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown/button bindings or non-finite values.
    pub fn set_target_value(&mut self, id: &BindingId, value: f64) -> Result<(), ControlMapError> {
        if !value.is_finite() {
            return Err(ControlMapError::NonFiniteValue);
        }
        let binding = self
            .bindings
            .iter()
            .find(|binding| &binding.id == id)
            .ok_or_else(|| ControlMapError::UnknownBinding(id.clone()))?;
        let output = binding
            .response
            .output()
            .ok_or_else(|| ControlMapError::NoTargetValue(id.clone()))?;
        let window = match binding.response {
            Response::Absolute {
                pickup: Pickup::SoftTakeover { window },
                ..
            } => Some(window),
            _ => None,
        };
        let normalized = output.normalize(value);
        let state = self.states.entry(id.clone()).or_default();
        state.target = Some(normalized);
        state.relative = Some(normalized);
        if let Some(window) = window
            && state.picked_up
            && state
                .last_normalized
                .is_none_or(|last| (last - normalized).abs() > window)
        {
            state.picked_up = false;
        }
        Ok(())
    }

    // --------------------------------------------------------- connection

    /// Attaches `device` on `port`, re-associating its bindings. The port
    /// name is remembered on the profile; a device that moves ports detaches
    /// from its old port. Transient per-binding state (button edges, pickup,
    /// last values) is reset; targets, toggles, and encoder positions persist.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown devices, invalid names, a port held by
    /// another device, or an exhausted generation.
    pub fn attach(
        &mut self,
        port: &str,
        device: &DeviceKey,
    ) -> Result<AttachReport, ControlMapError> {
        if !valid_name(port) {
            return Err(ControlMapError::InvalidName(port.to_owned()));
        }
        let profile = self
            .profiles
            .get_mut(device)
            .ok_or_else(|| ControlMapError::UnknownDevice(device.clone()))?;
        if let Some(holder) = self.ports.get(port)
            && holder != device
        {
            return Err(ControlMapError::PortInUse {
                port: port.to_owned(),
                device: holder.clone(),
            });
        }
        let generation = self
            .generations
            .get(device)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(ControlMapError::GenerationExhausted)?;
        let previous_port = core::mem::replace(&mut profile.port_name, port.to_owned());
        if let Some(previous) = self.attached.remove(device) {
            self.ports.remove(&previous.port);
        }
        self.generations.insert(device.clone(), generation);
        self.ports.insert(port.to_owned(), device.clone());
        self.attached.insert(
            device.clone(),
            Attached {
                port: port.to_owned(),
                generation,
            },
        );
        let bindings = self.reset_device(device);
        Ok(AttachReport {
            device: device.clone(),
            generation,
            port_changed: previous_port != port,
            previous_port,
            bindings,
        })
    }

    /// Attaches whichever single profile remembers `port` as its port name,
    /// for transports that cannot report a stable device key.
    ///
    /// # Errors
    ///
    /// Returns an error when no or several profiles remember the port.
    pub fn attach_remembered(&mut self, port: &str) -> Result<AttachReport, ControlMapError> {
        let mut matches = self
            .profiles
            .values()
            .filter(|profile| profile.port_name == port)
            .map(|profile| profile.key.clone());
        let device = matches
            .next()
            .ok_or_else(|| ControlMapError::UnknownPort(port.to_owned()))?;
        if matches.next().is_some() {
            return Err(ControlMapError::AmbiguousPort(port.to_owned()));
        }
        self.attach(port, &device)
    }

    /// Detaches the device on `port`, keeping its profile and bindings.
    ///
    /// # Errors
    ///
    /// Returns an error when the port is not attached.
    pub fn detach(&mut self, port: &str) -> Result<DeviceKey, ControlMapError> {
        let device = self
            .ports
            .remove(port)
            .ok_or_else(|| ControlMapError::UnknownPort(port.to_owned()))?;
        self.attached.remove(&device);
        self.reset_device(&device);
        if let LearnState::Captured(candidate) = &self.learn
            && candidate.binding.device == device
        {
            // A captured control from a vanished device cannot be confirmed
            // safely; return learn to armed.
            let LearnState::Captured(candidate) = core::mem::take(&mut self.learn) else {
                unreachable!("checked above");
            };
            self.learn = LearnState::Armed(LearnRequest {
                binding: candidate.binding.id,
                device: None,
                action: candidate.binding.action,
                response: candidate.binding.response,
            });
        }
        Ok(device)
    }

    fn reset_device(&mut self, device: &DeviceKey) -> usize {
        let mut count = 0;
        for binding in self.bindings.iter().filter(|b| &b.device == device) {
            if let Some(state) = self.states.get_mut(&binding.id) {
                state.reset_transient();
            }
            count += 1;
        }
        count
    }

    #[must_use]
    pub fn is_attached(&self, device: &DeviceKey) -> bool {
        self.attached.contains_key(device)
    }

    /// Current generation of an attached device.
    #[must_use]
    pub fn generation(&self, device: &DeviceKey) -> Option<u64> {
        self.attached
            .get(device)
            .map(|attached| attached.generation)
    }

    #[must_use]
    pub fn device_on_port(&self, port: &str) -> Option<&DeviceKey> {
        self.ports.get(port)
    }

    // --------------------------------------------------------------- learn

    #[must_use]
    pub fn learn_state(&self) -> &LearnState<A> {
        &self.learn
    }

    /// Arms learn: the next qualifying event (a press for button responses,
    /// a compatible continuous value otherwise) from an attached device is
    /// captured instead of dispatched.
    ///
    /// # Errors
    ///
    /// Returns an error when learn is active, the binding exists, the
    /// response is invalid, or a restricted device is unknown.
    pub fn arm_learn(&mut self, request: LearnRequest<A>) -> Result<(), ControlMapError> {
        if !matches!(self.learn, LearnState::Idle) {
            return Err(ControlMapError::LearnActive);
        }
        if self.states.contains_key(&request.binding) {
            return Err(ControlMapError::DuplicateBinding(request.binding));
        }
        request.response.validate()?;
        if let Some(device) = &request.device
            && !self.profiles.contains_key(device)
        {
            return Err(ControlMapError::UnknownDevice(device.clone()));
        }
        self.learn = LearnState::Armed(request);
        Ok(())
    }

    /// Cancels learn from either armed or captured state.
    ///
    /// # Errors
    ///
    /// Returns an error when learn is idle.
    pub fn cancel_learn(&mut self) -> Result<LearnState<A>, ControlMapError> {
        match core::mem::take(&mut self.learn) {
            LearnState::Idle => Err(ControlMapError::LearnNotArmed),
            previous => Ok(previous),
        }
    }

    /// Confirms a captured candidate under a conflict policy.
    ///
    /// # Errors
    ///
    /// Returns an error when nothing is captured, conflicts are rejected, or
    /// the binding no longer validates. Learn stays captured on error.
    pub fn confirm_learn(
        &mut self,
        policy: ConflictPolicy,
    ) -> Result<LearnConfirmation, ControlMapError> {
        let LearnState::Captured(candidate) = &self.learn else {
            return Err(match self.learn {
                LearnState::Idle => ControlMapError::LearnNotArmed,
                _ => ControlMapError::NothingCaptured,
            });
        };
        let conflicts = self.conflicts(&candidate.binding.device, &candidate.binding.control);
        if policy == ConflictPolicy::Reject && !conflicts.is_empty() {
            return Err(ControlMapError::LearnConflict(conflicts));
        }
        self.check_binding(&candidate.binding)?;
        let replaced = if policy == ConflictPolicy::ReplaceExisting {
            conflicts
        } else {
            Vec::new()
        };
        if self.bindings.len() - replaced.len() >= MAX_CONTROL_BINDINGS {
            return Err(ControlMapError::TooManyBindings {
                limit: MAX_CONTROL_BINDINGS,
            });
        }
        let LearnState::Captured(candidate) = core::mem::take(&mut self.learn) else {
            unreachable!("checked above");
        };
        for id in &replaced {
            self.remove_binding(id)?;
        }
        let binding = candidate.binding.id.clone();
        self.add_binding(candidate.binding)?;
        Ok(LearnConfirmation { binding, replaced })
    }

    fn try_capture(
        &mut self,
        device: &DeviceKey,
        event: &ControlEvent,
        timestamp: MediaTimestamp,
    ) -> LearnEffect {
        let LearnState::Armed(request) = &self.learn else {
            return LearnEffect::Ignored;
        };
        let transport_ok = self
            .profiles
            .get(device)
            .is_some_and(|profile| profile.transport.carries(&event.control));
        let qualifies = transport_ok
            && request
                .device
                .as_ref()
                .is_none_or(|wanted| wanted == device)
            && validate_control(&event.control).is_ok()
            && request.response.accepts(event.value);
        if !qualifies {
            return LearnEffect::Ignored;
        }
        let LearnState::Armed(request) = core::mem::take(&mut self.learn) else {
            unreachable!("checked above");
        };
        let conflicts = self.conflicts(device, &event.control);
        self.learn = LearnState::Captured(LearnCandidate {
            binding: ControlBinding {
                id: request.binding,
                device: device.clone(),
                control: event.control.clone(),
                action: request.action,
                response: request.response,
            },
            conflicts,
            captured_at: timestamp,
        });
        LearnEffect::Captured
    }

    // ------------------------------------------------------------ dispatch

    /// Maps one decoded event from `port` into typed action intents. While
    /// learn is armed or captured, events go to learn and are not dispatched.
    ///
    /// # Errors
    ///
    /// Returns an error when the port is not attached or a float value is
    /// not finite.
    pub fn process(
        &mut self,
        port: &str,
        event: &ControlEvent,
        timestamp: MediaTimestamp,
    ) -> Result<ProcessReport<A>, ControlMapError> {
        if matches!(event.value, ControlValue::Float(value) if !value.is_finite()) {
            return Err(ControlMapError::NonFiniteValue);
        }
        let device = self
            .ports
            .get(port)
            .cloned()
            .ok_or_else(|| ControlMapError::UnknownPort(port.to_owned()))?;
        let mut report = ProcessReport {
            device: device.clone(),
            outputs: Vec::new(),
            suppressed: Vec::new(),
            learn: LearnEffect::Inactive,
        };
        if !matches!(self.learn, LearnState::Idle) {
            report.learn = self.try_capture(&device, event, timestamp);
            return Ok(report);
        }
        for binding in &self.bindings {
            if binding.device != device || !controls_match(&binding.control, &event.control) {
                continue;
            }
            let state = self.states.entry(binding.id.clone()).or_default();
            match apply(binding, state, event.value, timestamp) {
                Ok(Some(output)) => report.outputs.push(output),
                Ok(None) => {}
                Err(reason) => report.suppressed.push((binding.id.clone(), reason)),
            }
        }
        Ok(report)
    }
}

fn pressed_of(value: ControlValue) -> Option<bool> {
    match value {
        ControlValue::Button { pressed, .. } => Some(pressed),
        ControlValue::Absolute { raw, max } if max > 0 => {
            Some(u32::from(raw) * 2 >= u32::from(max))
        }
        ControlValue::Float(value) => Some(value >= 0.5),
        ControlValue::Integer(value) => Some(value != 0),
        _ => None,
    }
}

fn normalized_of(value: ControlValue, input: Option<Span>) -> Option<f64> {
    match (value, input) {
        (ControlValue::Absolute { raw, max }, None) if max > 0 => {
            Some(f64::from(raw) / f64::from(max))
        }
        (ControlValue::Absolute { raw, .. }, Some(span)) => Some(span.normalize(f64::from(raw))),
        (ControlValue::Float(value), None) => Some(f64::from(value).clamp(0.0, 1.0)),
        (ControlValue::Float(value), Some(span)) => Some(span.normalize(f64::from(value))),
        (ControlValue::Integer(value), Some(span)) => Some(span.normalize(f64::from(value))),
        (ControlValue::Button { pressed, .. }, _) => Some(if pressed { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn same(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits()
}

fn button_action(
    edge: ButtonEdge,
    velocity: Option<VelocityRange>,
    value: ControlValue,
    state: &mut BindingState,
) -> Result<Option<ActionValue>, Suppressed> {
    let pressed = pressed_of(value).ok_or(Suppressed::IncompatibleValue)?;
    // Explicit button events are trusted as edges; continuous values are
    // edge-detected against the previous state.
    let explicit = matches!(value, ControlValue::Button { .. });
    let rising = pressed && (explicit || !state.pressed);
    let falling = !pressed && (explicit || state.pressed);
    state.pressed = pressed;
    Ok(match edge {
        ButtonEdge::Press | ButtonEdge::PressAndRelease if rising => {
            let raw = match value {
                ControlValue::Button { velocity, .. } => Some(velocity),
                _ => None,
            };
            let velocity = match (raw, velocity) {
                (Some(raw), Some(range)) => {
                    (f64::from(raw) - f64::from(range.minimum))
                        / (f64::from(range.maximum) - f64::from(range.minimum))
                }
                (Some(raw), None) => f64::from(raw) / 127.0,
                (None, _) => 1.0,
            };
            Some(ActionValue::Press {
                velocity: velocity.clamp(0.0, 1.0),
            })
        }
        ButtonEdge::Release | ButtonEdge::PressAndRelease if falling => Some(ActionValue::Release),
        ButtonEdge::Toggle if rising => {
            state.toggled = !state.toggled;
            Some(ActionValue::Toggled(state.toggled))
        }
        _ => None,
    })
}

fn apply<A: Clone>(
    binding: &ControlBinding<A>,
    state: &mut BindingState,
    value: ControlValue,
    timestamp: MediaTimestamp,
) -> Result<Option<CommandIntent<ControlOutput<A>>>, Suppressed> {
    let output = |value| ControlOutput {
        binding: binding.id.clone(),
        action: binding.action.clone(),
        value,
        timestamp,
    };
    match binding.response {
        Response::Button { edge, velocity } => Ok(button_action(edge, velocity, value, state)?
            .map(|action| CommandIntent::discrete(output(action)))),
        Response::Absolute {
            input,
            output: span,
            invert,
            deadband,
            pickup,
        } => {
            let mut normalized =
                normalized_of(value, input).ok_or(Suppressed::IncompatibleValue)?;
            if invert {
                normalized = 1.0 - normalized;
            }
            let previous_hardware = state.last_hardware.replace(normalized);
            if let Pickup::SoftTakeover { window } = pickup
                && !state.picked_up
            {
                match state.target {
                    None => state.picked_up = true,
                    Some(target) => {
                        let near = (normalized - target).abs() <= window;
                        let crossed = previous_hardware.is_some_and(|previous| {
                            (previous - target).signum() != (normalized - target).signum()
                        });
                        if near || crossed {
                            state.picked_up = true;
                        } else {
                            return Err(Suppressed::AwaitingPickup {
                                hardware: normalized,
                                target,
                            });
                        }
                    }
                }
            }
            if let Some(last) = state.last_normalized {
                let endpoint = same(normalized, 0.0) || same(normalized, 1.0);
                if (normalized - last).abs() < deadband && !endpoint {
                    return Err(Suppressed::Deadband);
                }
            }
            state.last_normalized = Some(normalized);
            state.target = Some(normalized);
            Ok(Some(CommandIntent::continuous(
                binding.id.as_str(),
                output(ActionValue::Level(span.denormalize(normalized))),
            )))
        }
        Response::Relative {
            encoding,
            step,
            output: span,
        } => {
            let delta = encoding.delta(value).ok_or(Suppressed::IncompatibleValue)?;
            let base = state.relative.or(state.target).unwrap_or(0.0);
            let next = (base + f64::from(delta) * step).clamp(0.0, 1.0);
            if delta == 0 || same(next, base) {
                return Err(Suppressed::NoChange);
            }
            state.relative = Some(next);
            state.last_normalized = Some(next);
            state.target = Some(next);
            Ok(Some(CommandIntent::continuous(
                binding.id.as_str(),
                output(ActionValue::Level(span.denormalize(next))),
            )))
        }
    }
}
