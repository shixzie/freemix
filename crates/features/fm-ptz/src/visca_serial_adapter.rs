//! A VISCA camera adapter over a serial port (IN-028).
//!
//! [`ViscaSerialAdapter`] implements the transport-neutral [`PtzAdapter`]
//! contract with one [`ViscaSerialTransport`] per camera. It validates and
//! clamps an intent against the camera's capabilities, encodes the VISCA
//! commands the intent needs with the shared codec, writes each frame, and
//! updates telemetry and presets only after the camera replies.
//!
//! A failed transaction marks that camera disconnected with the transport's
//! typed reason; `begin_recovery` and `complete_recovery` drive the observable
//! reconnect lifecycle.

use std::{collections::BTreeMap, io, time::Duration};

use crate::visca::{ViscaAddress, ViscaEncodeError, ViscaMoveSpeeds, visca_commands_for_intent};
use crate::visca_serial::{SerialPort, ViscaSerialError, ViscaSerialTransport};
use crate::{
    AdapterError, Axis, CameraCapabilities, CameraDescriptor, CameraId, CameraPosition,
    CommandOutcome, ConnectionState, DisconnectReason, MovementId, MovementKind, MovementState,
    Preset, PresetId, PtzAdapter, PtzIntent, RecoveryTelemetry, RelativeMove, Telemetry,
};

/// One camera's serial port, identity, and capabilities.
pub struct ViscaSerialCamera {
    pub id: CameraId,
    pub name: String,
    pub port: Box<dyn SerialPort>,
    pub capabilities: CameraCapabilities,
    pub initial_position: CameraPosition,
}

struct CameraState {
    descriptor: CameraDescriptor,
    transport: ViscaSerialTransport<Box<dyn SerialPort>>,
    telemetry: Telemetry,
    presets: BTreeMap<PresetId, Preset>,
}

/// A synchronous `PtzAdapter` over VISCA serial links.
pub struct ViscaSerialAdapter {
    cameras: BTreeMap<CameraId, CameraState>,
    next_movement_id: u64,
    address: ViscaAddress,
    speeds: ViscaMoveSpeeds,
}

impl ViscaSerialAdapter {
    /// Opens one serial transport per camera with `timeout` on blocking reads.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate camera or when a port refuses its read
    /// timeout.
    pub fn connect(
        cameras: impl IntoIterator<Item = ViscaSerialCamera>,
        timeout: Duration,
    ) -> io::Result<Self> {
        let mut connected = BTreeMap::new();
        for camera in cameras {
            if connected.contains_key(&camera.id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("camera {} is configured twice", camera.id),
                ));
            }
            let transport = ViscaSerialTransport::new(camera.port, timeout)?;
            let position = camera.initial_position.clamped(&camera.capabilities);
            connected.insert(
                camera.id.clone(),
                CameraState {
                    descriptor: CameraDescriptor {
                        id: camera.id,
                        name: camera.name,
                        capabilities: camera.capabilities,
                    },
                    transport,
                    telemetry: Telemetry {
                        connection: ConnectionState::Connected { generation: 1 },
                        position,
                        movement: MovementState::Idle,
                        commands_accepted: 0,
                        recovery: RecoveryTelemetry::default(),
                        last_error: None,
                    },
                    presets: BTreeMap::new(),
                },
            );
        }
        Ok(Self {
            cameras: connected,
            next_movement_id: 1,
            address: ViscaAddress::DEFAULT,
            speeds: ViscaMoveSpeeds::default(),
        })
    }

    /// Opens one camera's serial link.
    ///
    /// # Errors
    ///
    /// Returns the port's timeout-configuration error.
    pub fn connect_one(camera: ViscaSerialCamera, timeout: Duration) -> io::Result<Self> {
        Self::connect([camera], timeout)
    }

    /// The VISCA address every command uses (default 1).
    #[must_use]
    pub const fn address(&self) -> ViscaAddress {
        self.address
    }

    /// Changes the VISCA address every command uses.
    pub fn set_address(&mut self, address: ViscaAddress) {
        self.address = address;
    }

    /// The pan/tilt speed pair every absolute and relative move uses.
    #[must_use]
    pub const fn speeds(&self) -> ViscaMoveSpeeds {
        self.speeds
    }

    /// Changes the pan/tilt speed pair.
    pub fn set_speeds(&mut self, speeds: ViscaMoveSpeeds) {
        self.speeds = speeds;
    }

    fn camera(&self, camera_id: &CameraId) -> Result<&CameraState, AdapterError> {
        self.cameras
            .get(camera_id)
            .ok_or_else(|| AdapterError::CameraNotFound(camera_id.clone()))
    }

    fn camera_mut(&mut self, camera_id: &CameraId) -> Result<&mut CameraState, AdapterError> {
        self.cameras
            .get_mut(camera_id)
            .ok_or_else(|| AdapterError::CameraNotFound(camera_id.clone()))
    }

    fn movement_id(&mut self) -> Result<MovementId, AdapterError> {
        let id = self.next_movement_id;
        self.next_movement_id = id
            .checked_add(1)
            .ok_or(AdapterError::MovementCounterExhausted)?;
        Ok(MovementId::new(id))
    }

    fn validate_intent(
        capabilities: &CameraCapabilities,
        intent: &PtzIntent,
    ) -> Result<(), AdapterError> {
        match intent {
            PtzIntent::MoveAbsolute(movement) => {
                if !capabilities.absolute_movement {
                    return Err(AdapterError::UnsupportedIntent("absolute movement"));
                }
                validate_optional_axes(capabilities, movement.zoom, movement.focus)
            }
            PtzIntent::MoveRelative(movement) => {
                if !capabilities.relative_movement {
                    return Err(AdapterError::UnsupportedIntent("relative movement"));
                }
                validate_optional_axes(capabilities, movement.zoom, movement.focus)
            }
            PtzIntent::MoveContinuous(movement) => validate_optional_axes(
                capabilities,
                (movement.zoom != 0).then_some(movement.zoom),
                (movement.focus != 0).then_some(movement.focus),
            ),
            PtzIntent::Home if !capabilities.home => Err(AdapterError::UnsupportedIntent("home")),
            PtzIntent::SavePreset { id, .. }
            | PtzIntent::RecallPreset(id)
            | PtzIntent::DeletePreset(id)
                if !capabilities.supports_preset(*id) =>
            {
                Err(AdapterError::PresetOutOfRange(*id))
            }
            _ => Ok(()),
        }
    }

    fn send(
        camera_id: &CameraId,
        camera: &mut CameraState,
        bytes: &[u8],
    ) -> Result<(), AdapterError> {
        match camera.transport.transaction(bytes) {
            Ok(_) => Ok(()),
            Err(error) => {
                camera.telemetry.connection = ConnectionState::Disconnected {
                    reason: disconnect_reason(&error),
                };
                camera.telemetry.last_error = Some(error.to_string());
                Err(AdapterError::Disconnected(camera_id.clone()))
            }
        }
    }

    fn send_intent(
        camera_id: &CameraId,
        camera: &mut CameraState,
        address: ViscaAddress,
        speeds: ViscaMoveSpeeds,
        intent: &PtzIntent,
    ) -> Result<(), AdapterError> {
        let commands = visca_commands_for_intent(intent, speeds).map_err(encode_error)?;
        for command in commands {
            let bytes = command.encode(address).map_err(encode_error)?;
            Self::send(camera_id, camera, &bytes)?;
        }
        Ok(())
    }

    fn apply_movement(
        camera: &mut CameraState,
        movement_id: MovementId,
        intent: &PtzIntent,
    ) -> Result<MovementState, AdapterError> {
        let movement = match intent {
            PtzIntent::MoveAbsolute(movement) => {
                camera.telemetry.position = CameraPosition {
                    pan: movement.pan,
                    tilt: movement.tilt,
                    zoom: movement.zoom,
                    focus: movement.focus,
                };
                MovementState::Completed {
                    id: movement_id,
                    kind: MovementKind::Absolute,
                }
            }
            PtzIntent::MoveRelative(movement) => {
                apply_relative(&mut camera.telemetry.position, *movement);
                MovementState::Completed {
                    id: movement_id,
                    kind: MovementKind::Relative,
                }
            }
            PtzIntent::MoveContinuous(_) => MovementState::Moving {
                id: movement_id,
                kind: MovementKind::Continuous,
            },
            PtzIntent::Home => {
                let capabilities = &camera.descriptor.capabilities;
                camera.telemetry.position = CameraPosition {
                    pan: 0,
                    tilt: 0,
                    zoom: capabilities.zoom.map(|axis| axis.position.min),
                    focus: capabilities.focus.map(|axis| axis.position.min),
                }
                .clamped(capabilities);
                MovementState::Completed {
                    id: movement_id,
                    kind: MovementKind::Home,
                }
            }
            PtzIntent::RecallPreset(id) => {
                let preset = camera
                    .presets
                    .get(id)
                    .ok_or(AdapterError::PresetNotFound(*id))?;
                camera.telemetry.position = preset.position;
                MovementState::Completed {
                    id: movement_id,
                    kind: MovementKind::Preset,
                }
            }
            _ => return Ok(camera.telemetry.movement),
        };
        Ok(movement)
    }
}

impl PtzAdapter for ViscaSerialAdapter {
    fn cameras(&self) -> Vec<CameraDescriptor> {
        self.cameras
            .values()
            .map(|camera| camera.descriptor.clone())
            .collect()
    }

    fn telemetry(&self, camera_id: &CameraId) -> Result<Telemetry, AdapterError> {
        Ok(self.camera(camera_id)?.telemetry.clone())
    }

    fn presets(&self, camera_id: &CameraId) -> Result<Vec<Preset>, AdapterError> {
        Ok(self.camera(camera_id)?.presets.values().cloned().collect())
    }

    fn execute(
        &mut self,
        camera_id: &CameraId,
        intent: PtzIntent,
    ) -> Result<CommandOutcome, AdapterError> {
        let (capabilities, connection, position) = {
            let camera = self.camera(camera_id)?;
            (
                camera.descriptor.capabilities.clone(),
                camera.telemetry.connection,
                camera.telemetry.position,
            )
        };
        if !matches!(connection, ConnectionState::Connected { .. }) {
            return Err(AdapterError::Disconnected(camera_id.clone()));
        }
        Self::validate_intent(&capabilities, &intent)?;
        let applied = intent.clamped(&capabilities, position);

        let needs_movement_id = matches!(
            applied,
            PtzIntent::MoveAbsolute(_)
                | PtzIntent::MoveRelative(_)
                | PtzIntent::MoveContinuous(_)
                | PtzIntent::Home
                | PtzIntent::RecallPreset(_)
        );
        let movement_id = needs_movement_id.then(|| self.movement_id()).transpose()?;

        if let PtzIntent::RecallPreset(id) = &applied
            && !self.camera(camera_id)?.presets.contains_key(id)
        {
            return Err(AdapterError::PresetNotFound(*id));
        }

        let address = self.address;
        let speeds = self.speeds;
        let camera = self.camera_mut(camera_id)?;
        Self::send_intent(camera_id, camera, address, speeds, &applied)?;

        let movement = match &applied {
            PtzIntent::Stop => match camera.telemetry.movement {
                MovementState::Moving { id, kind } => MovementState::Stopped { id, kind },
                existing => existing,
            },
            PtzIntent::SavePreset { id, name } => {
                camera.presets.insert(
                    *id,
                    Preset {
                        id: *id,
                        name: name.clone(),
                        position: camera.telemetry.position,
                    },
                );
                camera.telemetry.movement
            }
            PtzIntent::DeletePreset(id) => {
                if camera.presets.remove(id).is_none() {
                    return Err(AdapterError::PresetNotFound(*id));
                }
                camera.telemetry.movement
            }
            _ => Self::apply_movement(
                camera,
                movement_id.expect("movement intent has an allocated ID"),
                &applied,
            )?,
        };

        camera.telemetry.movement = movement;
        camera.telemetry.commands_accepted = camera.telemetry.commands_accepted.saturating_add(1);
        camera.telemetry.last_error = None;
        Ok(CommandOutcome {
            applied,
            movement,
            telemetry: camera.telemetry.clone(),
        })
    }

    fn begin_recovery(&mut self, camera_id: &CameraId) -> Result<Telemetry, AdapterError> {
        let camera = self.camera_mut(camera_id)?;
        camera.telemetry.recovery.attempts = camera.telemetry.recovery.attempts.saturating_add(1);
        camera.telemetry.connection = ConnectionState::Recovering {
            attempt: camera.telemetry.recovery.attempts,
        };
        Ok(camera.telemetry.clone())
    }

    fn complete_recovery(&mut self, camera_id: &CameraId) -> Result<Telemetry, AdapterError> {
        let camera = self.camera_mut(camera_id)?;
        if !matches!(
            camera.telemetry.connection,
            ConnectionState::Recovering { .. }
        ) {
            return Err(AdapterError::RecoveryNotStarted(camera_id.clone()));
        }
        camera.telemetry.recovery.successes = camera.telemetry.recovery.successes.saturating_add(1);
        camera.telemetry.connection = ConnectionState::Connected {
            generation: u64::from(camera.telemetry.recovery.successes) + 1,
        };
        camera.telemetry.last_error = None;
        Ok(camera.telemetry.clone())
    }
}

fn encode_error(error: ViscaEncodeError) -> AdapterError {
    match error {
        ViscaEncodeError::UnsupportedRelativeAxis(axis) => AdapterError::UnsupportedAxis(axis),
        _ => AdapterError::UnsupportedIntent("VISCA command range"),
    }
}

const fn disconnect_reason(error: &ViscaSerialError) -> DisconnectReason {
    match error {
        ViscaSerialError::Timeout => DisconnectReason::Timeout,
        _ => DisconnectReason::TransportLost,
    }
}

fn validate_optional_axes(
    capabilities: &CameraCapabilities,
    zoom: Option<i32>,
    focus: Option<i32>,
) -> Result<(), AdapterError> {
    if zoom.is_some() && capabilities.zoom.is_none() {
        return Err(AdapterError::UnsupportedAxis(Axis::Zoom));
    }
    if focus.is_some() && capabilities.focus.is_none() {
        return Err(AdapterError::UnsupportedAxis(Axis::Focus));
    }
    Ok(())
}

fn apply_relative(position: &mut CameraPosition, movement: RelativeMove) {
    position.pan += movement.pan;
    position.tilt += movement.tilt;
    if let (Some(current), Some(delta)) = (&mut position.zoom, movement.zoom) {
        *current += delta;
    }
    if let (Some(current), Some(delta)) = (&mut position.focus, movement.focus) {
        *current += delta;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbsoluteMove, ContinuousMove, ContinuousSource};
    use std::{
        io::{Read, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    struct AckPort {
        written: Arc<Mutex<Vec<u8>>>,
        fail: Arc<AtomicBool>,
    }

    impl Read for AckPort {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let ack = [0x90, 0x41, 0xFF];
            let take = buffer.len().min(ack.len());
            buffer[..take].copy_from_slice(&ack[..take]);
            Ok(take)
        }
    }

    impl Write for AckPort {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for AckPort {
        fn set_read_timeout(&mut self, _timeout: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    fn capabilities() -> CameraCapabilities {
        let limits = |min, max| crate::AxisLimits::new(min, max).unwrap();
        CameraCapabilities {
            pan: crate::AxisCapabilities::new(limits(-2_448, 2_448), 24).unwrap(),
            tilt: crate::AxisCapabilities::new(limits(-432, 1_296), 20).unwrap(),
            zoom: Some(crate::AxisCapabilities::new(limits(0, 16_384), 7).unwrap()),
            focus: Some(crate::AxisCapabilities::new(limits(0, 4_095), 7).unwrap()),
            absolute_movement: true,
            relative_movement: true,
            home: true,
            preset_slots: 16,
        }
    }

    fn camera(fail: Arc<AtomicBool>, written: Arc<Mutex<Vec<u8>>>) -> ViscaSerialCamera {
        ViscaSerialCamera {
            id: CameraId::new("cam-1").unwrap(),
            name: "Camera 1".to_owned(),
            port: Box::new(AckPort { written, fail }),
            capabilities: capabilities(),
            initial_position: CameraPosition {
                pan: 0,
                tilt: 0,
                zoom: Some(0),
                focus: Some(0),
            },
        }
    }

    fn adapter() -> (ViscaSerialAdapter, Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let fail = Arc::new(AtomicBool::new(false));
        let adapter = ViscaSerialAdapter::connect_one(
            camera(fail.clone(), written.clone()),
            Duration::from_millis(5),
        )
        .unwrap();
        (adapter, written, fail)
    }

    fn id() -> CameraId {
        CameraId::new("cam-1").unwrap()
    }

    #[test]
    fn an_absolute_move_and_presets_use_the_serial_port() {
        let (mut adapter, written, _fail) = adapter();
        let outcome = adapter
            .execute(
                &id(),
                PtzIntent::MoveAbsolute(AbsoluteMove {
                    pan: 100,
                    tilt: -50,
                    zoom: Some(2),
                    focus: None,
                }),
            )
            .unwrap();
        assert_eq!(outcome.telemetry.position.pan, 100);
        assert_eq!(outcome.telemetry.position.tilt, -50);
        assert_eq!(outcome.telemetry.position.zoom, Some(2));
        assert!(matches!(
            outcome.movement,
            MovementState::Completed {
                kind: MovementKind::Absolute,
                ..
            }
        ));
        assert_eq!(outcome.telemetry.commands_accepted, 1);
        let bytes = written.lock().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.last(), Some(&0xFF));
        drop(bytes);

        let preset = PresetId::new(3);
        adapter
            .execute(
                &id(),
                PtzIntent::SavePreset {
                    id: preset,
                    name: "Wide".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(adapter.presets(&id()).unwrap().len(), 1);
        adapter
            .execute(
                &id(),
                PtzIntent::MoveAbsolute(AbsoluteMove {
                    pan: 0,
                    tilt: 0,
                    zoom: Some(0),
                    focus: Some(0),
                }),
            )
            .unwrap();
        let outcome = adapter
            .execute(&id(), PtzIntent::RecallPreset(preset))
            .unwrap();
        assert_eq!(outcome.telemetry.position.pan, 100);
        assert!(matches!(
            outcome.movement,
            MovementState::Completed {
                kind: MovementKind::Preset,
                ..
            }
        ));
    }

    #[test]
    fn a_timeout_disconnects_the_camera_and_recovery_reconnects_it() {
        let (mut adapter, _written, fail) = adapter();
        fail.store(true, Ordering::Relaxed);
        assert_eq!(
            adapter.execute(
                &id(),
                PtzIntent::MoveContinuous(ContinuousMove {
                    pan: 1,
                    tilt: 0,
                    zoom: 0,
                    focus: 0,
                    source: ContinuousSource::Joystick,
                }),
            ),
            Err(AdapterError::Disconnected(id()))
        );
        let telemetry = adapter.telemetry(&id()).unwrap();
        assert_eq!(
            telemetry.connection,
            ConnectionState::Disconnected {
                reason: DisconnectReason::Timeout
            }
        );
        assert_eq!(
            adapter.execute(&id(), PtzIntent::Stop),
            Err(AdapterError::Disconnected(id()))
        );
        assert!(matches!(
            adapter.begin_recovery(&id()).unwrap().connection,
            ConnectionState::Recovering { attempt: 1 }
        ));
        fail.store(false, Ordering::Relaxed);
        assert_eq!(
            adapter.complete_recovery(&id()).unwrap().connection,
            ConnectionState::Connected { generation: 2 }
        );
    }

    #[test]
    fn an_unsupported_intent_is_refused_before_the_wire() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let fail = Arc::new(AtomicBool::new(false));
        let mut adapter = ViscaSerialAdapter::connect_one(
            ViscaSerialCamera {
                id: id(),
                name: "Camera 1".to_owned(),
                port: Box::new(AckPort {
                    written: written.clone(),
                    fail,
                }),
                capabilities: CameraCapabilities {
                    home: false,
                    ..capabilities()
                },
                initial_position: CameraPosition {
                    pan: 0,
                    tilt: 0,
                    zoom: Some(0),
                    focus: Some(0),
                },
            },
            Duration::from_millis(5),
        )
        .unwrap();
        assert_eq!(
            adapter.execute(&id(), PtzIntent::Home),
            Err(AdapterError::UnsupportedIntent("home"))
        );
        assert!(written.lock().unwrap().is_empty());
    }
}
