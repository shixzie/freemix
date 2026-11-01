//! A real VISCA-over-IP camera adapter (IN-028).
//!
//! [`ViscaUdpAdapter`] implements the transport-neutral [`PtzAdapter`]
//! contract over one UDP endpoint per camera. It validates and clamps an
//! intent against the camera's capabilities, encodes the VISCA commands the
//! intent needs, sends them sequence-numbered through
//! [`ViscaUdpTransport`], and updates the last-known telemetry only after
//! every command was acknowledged.
//!
//! A failed transaction leaves the camera visibly disconnected with the
//! transport's message in `last_error`; `begin_recovery` and
//! `complete_recovery` drive the observable reconnect lifecycle.

use std::{collections::BTreeMap, io, net::SocketAddr};

use crate::visca::{ViscaAddress, ViscaEncodeError, ViscaMoveSpeeds, visca_commands_for_intent};
use crate::visca_ip::ViscaIpPayloadType;
use crate::visca_ip_transport::{ViscaUdpConfig, ViscaUdpError, ViscaUdpTransport};
use crate::{
    AdapterError, Axis, CameraCapabilities, CameraDescriptor, CameraId, CameraPosition,
    CommandOutcome, ConnectionState, DisconnectReason, MovementId, MovementKind, MovementState,
    Preset, PresetId, PtzAdapter, PtzIntent, RecoveryTelemetry, RelativeMove, Telemetry,
};

/// One camera's UDP endpoint, identity, and capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViscaUdpCamera {
    pub id: CameraId,
    pub name: String,
    pub address: SocketAddr,
    pub capabilities: CameraCapabilities,
    pub initial_position: CameraPosition,
}

struct CameraState {
    descriptor: CameraDescriptor,
    transport: ViscaUdpTransport,
    telemetry: Telemetry,
    presets: BTreeMap<PresetId, Preset>,
}

/// A synchronous `PtzAdapter` over VISCA-over-IP UDP.
pub struct ViscaUdpAdapter {
    cameras: BTreeMap<CameraId, CameraState>,
    next_movement_id: u64,
    address: ViscaAddress,
    speeds: ViscaMoveSpeeds,
}

impl ViscaUdpAdapter {
    /// Binds one UDP endpoint per camera to `config`.
    ///
    /// # Errors
    ///
    /// Returns the first socket bind or address-resolution error.
    pub fn bind(
        cameras: impl IntoIterator<Item = ViscaUdpCamera>,
        config: ViscaUdpConfig,
    ) -> io::Result<Self> {
        let mut bound = BTreeMap::new();
        for camera in cameras {
            if bound.contains_key(&camera.id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("camera {} is configured twice", camera.id),
                ));
            }
            let transport = ViscaUdpTransport::bind(("0.0.0.0", 0), camera.address, config)?;
            let position = camera.initial_position.clamped(&camera.capabilities);
            bound.insert(
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
            cameras: bound,
            next_movement_id: 1,
            address: ViscaAddress::DEFAULT,
            speeds: ViscaMoveSpeeds::default(),
        })
    }

    /// Binds one camera to `remote` using its address directly.
    ///
    /// # Errors
    ///
    /// Returns a socket bind or address-resolution error.
    pub fn bind_one(camera: ViscaUdpCamera, config: ViscaUdpConfig) -> io::Result<Self> {
        Self::bind([camera], config)
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
        match camera
            .transport
            .transact(ViscaIpPayloadType::Command, bytes)
        {
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

impl PtzAdapter for ViscaUdpAdapter {
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

        // Refuse a preset the camera never stored before touching the wire,
        // so a failed recall does not leave a half-applied move.
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

const fn disconnect_reason(error: &ViscaUdpError) -> DisconnectReason {
    match error {
        ViscaUdpError::Timeout => DisconnectReason::Timeout,
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
    use crate::visca::{ViscaCommand, ViscaMoveSpeeds};
    use crate::visca_ip::ViscaIpPacket;
    use crate::{AbsoluteMove, ContinuousMove, ContinuousSource};
    use std::{
        net::UdpSocket,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

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
            preset_slots: 255,
        }
    }

    fn cid(value: u128) -> CameraId {
        CameraId::new(value.to_string()).unwrap()
    }

    fn camera(id: u128, address: SocketAddr) -> ViscaUdpCamera {
        ViscaUdpCamera {
            id: cid(id),
            name: format!("Camera {id}"),
            address,
            capabilities: capabilities(),
            initial_position: CameraPosition {
                pan: 0,
                tilt: 0,
                zoom: Some(0),
                focus: Some(0),
            },
        }
    }

    /// A camera that acknowledges every datagram and reports the command
    /// payloads it saw.
    fn fake_camera(
        socket: UdpSocket,
        stop: Arc<AtomicBool>,
    ) -> (thread::JoinHandle<()>, mpsc::Receiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            let mut buffer = [0_u8; 64];
            while !stop.load(Ordering::Acquire) {
                let Ok((len, source)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let Ok(packet) = ViscaIpPacket::decode(&buffer[..len]) else {
                    continue;
                };
                let _ = sender.send(packet.payload.clone());
                let reply = ViscaIpPacket {
                    payload_type: ViscaIpPayloadType::Reply,
                    sequence: packet.sequence,
                    payload: vec![0x90, 0x50, 0xFF],
                };
                let _ = socket.send_to(&reply.encode().unwrap(), source);
            }
        });
        (worker, receiver)
    }

    fn next_commands(receiver: &mpsc::Receiver<Vec<u8>>, count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|_| {
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("the camera received every command")
            })
            .collect()
    }

    #[test]
    fn a_move_sends_every_visa_command_and_updates_telemetry() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (worker, receiver) = fake_camera(socket, Arc::clone(&stop));
        let mut adapter = ViscaUdpAdapter::bind_one(
            camera(1, address),
            ViscaUdpConfig {
                timeout: Duration::from_millis(500),
                attempts: 2,
                sequence_resets: 2,
            },
        )
        .unwrap();
        let outcome = adapter
            .execute(
                &cid(1),
                PtzIntent::MoveAbsolute(AbsoluteMove {
                    pan: 100,
                    tilt: 50,
                    zoom: Some(1_000),
                    focus: Some(200),
                }),
            )
            .unwrap();
        assert_eq!(outcome.telemetry.position.pan, 100);
        assert_eq!(outcome.telemetry.position.zoom, Some(1_000));
        assert!(matches!(
            outcome.movement,
            MovementState::Completed {
                kind: MovementKind::Absolute,
                ..
            }
        ));

        let commands = next_commands(&receiver, 3);
        let address = adapter.address();
        assert_eq!(
            commands[0],
            ViscaCommand::PanTiltAbsolute {
                speeds: ViscaMoveSpeeds::default(),
                pan: 100,
                tilt: 50,
            }
            .encode(address)
            .unwrap()
        );
        assert_eq!(
            commands[1],
            ViscaCommand::ZoomDirect { position: 1_000 }
                .encode(address)
                .unwrap()
        );
        assert_eq!(
            commands[2],
            ViscaCommand::FocusDirect { position: 200 }
                .encode(address)
                .unwrap()
        );

        adapter
            .execute(
                &cid(1),
                PtzIntent::SavePreset {
                    id: PresetId::new(3),
                    name: "Wide".into(),
                },
            )
            .unwrap();
        let _ = next_commands(&receiver, 1);
        adapter
            .execute(
                &cid(1),
                PtzIntent::MoveAbsolute(AbsoluteMove {
                    pan: 0,
                    tilt: 0,
                    zoom: Some(0),
                    focus: Some(0),
                }),
            )
            .unwrap();
        let _ = next_commands(&receiver, 3);
        let recalled = adapter
            .execute(&cid(1), PtzIntent::RecallPreset(PresetId::new(3)))
            .unwrap();
        assert_eq!(recalled.telemetry.position.pan, 100);
        assert_eq!(adapter.presets(&cid(1)).unwrap()[0].name, "Wide");

        stop.store(true, Ordering::Release);
        worker.join().unwrap();
    }

    #[test]
    fn a_missing_acknowledgement_disconnects_and_recovery_reconnects() {
        // A socket that never reads or replies: every transaction times out.
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let mut adapter = ViscaUdpAdapter::bind_one(
            camera(1, address),
            ViscaUdpConfig {
                timeout: Duration::from_millis(20),
                attempts: 1,
                sequence_resets: 0,
            },
        )
        .unwrap();
        assert_eq!(
            adapter.execute(
                &cid(1),
                PtzIntent::MoveContinuous(ContinuousMove {
                    source: ContinuousSource::Joystick,
                    pan: 1,
                    tilt: 0,
                    zoom: 0,
                    focus: 0,
                }),
            ),
            Err(AdapterError::Disconnected(cid(1)))
        );
        let telemetry = adapter.telemetry(&cid(1)).unwrap();
        assert!(matches!(
            telemetry.connection,
            ConnectionState::Disconnected {
                reason: DisconnectReason::Timeout
            }
        ));
        assert!(telemetry.last_error.is_some());

        let recovering = adapter.begin_recovery(&cid(1)).unwrap();
        assert!(matches!(
            recovering.connection,
            ConnectionState::Recovering { attempt: 1 }
        ));
        let recovered = adapter.complete_recovery(&cid(1)).unwrap();
        assert!(matches!(
            recovered.connection,
            ConnectionState::Connected { generation: 2 }
        ));
        assert!(recovered.last_error.is_none());
    }
}
