use fm_ptz::{
    AbsoluteMove, Axis, CameraId, ContinuousMove, ContinuousSource, ControlVector,
    FakeViscaAdapter, FakeViscaCamera, FocusMode, MouseDragMapping, PresetAction, PresetId,
    PtzAdapter, PtzIntent, RelativeMove, ResponseCurve, SpeedMapping, ViscaAddress, ViscaCommand,
    ViscaEncodeError, ViscaErrorCode, ViscaInquiry, ViscaIpControl, ViscaIpError, ViscaIpPacket,
    ViscaIpPayloadType, ViscaIpSequencer, ViscaMoveSpeeds, ViscaParseError, ViscaReply,
    ViscaReplyDecoder, decode_lens_position, decode_pan_tilt_position, visca_commands_for_intent,
};

fn cam1() -> ViscaAddress {
    ViscaAddress::new(1).unwrap()
}

fn enc(command: ViscaCommand) -> Vec<u8> {
    command.encode(cam1()).unwrap()
}

#[test]
fn addresses_are_one_through_seven_with_exact_headers() {
    assert_eq!(ViscaAddress::new(0), None);
    assert_eq!(ViscaAddress::new(8), None);
    let seven = ViscaAddress::new(7).unwrap();
    assert_eq!(seven.command_header(), 0x87);
    assert_eq!(seven.reply_header(), 0xF0);
    assert_eq!(cam1().reply_header(), 0x90);
}

#[test]
fn pan_tilt_drive_encodes_direction_and_speed_bytes() {
    let drive = |pan, tilt| enc(ViscaCommand::PanTiltDrive { pan, tilt });
    assert_eq!(
        drive(-5, 3),
        [0x81, 0x01, 0x06, 0x01, 0x05, 0x03, 0x01, 0x01, 0xFF]
    );
    assert_eq!(
        drive(24, -20),
        [0x81, 0x01, 0x06, 0x01, 0x18, 0x14, 0x02, 0x02, 0xFF]
    );
    assert_eq!(
        drive(0, 0),
        [0x81, 0x01, 0x06, 0x01, 0x01, 0x01, 0x03, 0x03, 0xFF]
    );
    assert_eq!(
        drive(7, 0),
        [0x81, 0x01, 0x06, 0x01, 0x07, 0x01, 0x02, 0x03, 0xFF]
    );
    assert_eq!(
        ViscaCommand::PanTiltDrive { pan: 25, tilt: 0 }.encode(cam1()),
        Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Pan,
            value: 25
        })
    );
    assert_eq!(
        ViscaCommand::PanTiltDrive { pan: 0, tilt: -21 }.encode(cam1()),
        Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Tilt,
            value: -21
        })
    );
}

#[test]
fn absolute_and_relative_positions_use_twos_complement_nibbles() {
    let speeds = ViscaMoveSpeeds {
        pan: 0x10,
        tilt: 0x0E,
    };
    assert_eq!(
        enc(ViscaCommand::PanTiltAbsolute {
            speeds,
            pan: 2_448,
            tilt: -432,
        }),
        [
            0x81, 0x01, 0x06, 0x02, 0x10, 0x0E, 0x00, 0x09, 0x09, 0x00, 0x0F, 0x0E, 0x05, 0x00,
            0xFF
        ]
    );
    assert_eq!(
        enc(ViscaCommand::PanTiltRelative {
            speeds: ViscaMoveSpeeds::default(),
            pan: -1,
            tilt: 0,
        }),
        [
            0x81, 0x01, 0x06, 0x03, 0x18, 0x14, 0x0F, 0x0F, 0x0F, 0x0F, 0x00, 0x00, 0x00, 0x00,
            0xFF
        ]
    );
    assert_eq!(
        ViscaCommand::PanTiltAbsolute {
            speeds,
            pan: 40_000,
            tilt: 0
        }
        .encode(cam1()),
        Err(ViscaEncodeError::PositionOutOfRange {
            axis: Axis::Pan,
            value: 40_000
        })
    );
    assert_eq!(
        ViscaCommand::PanTiltAbsolute {
            speeds: ViscaMoveSpeeds { pan: 0, tilt: 1 },
            pan: 0,
            tilt: 0
        }
        .encode(cam1()),
        Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Pan,
            value: 0
        })
    );
}

#[test]
fn lens_preset_and_system_commands_have_exact_bytes() {
    assert_eq!(
        enc(ViscaCommand::PanTiltHome),
        [0x81, 0x01, 0x06, 0x04, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::PanTiltReset),
        [0x81, 0x01, 0x06, 0x05, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::ZoomDrive { speed: 3 }),
        [0x81, 0x01, 0x04, 0x07, 0x23, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::ZoomDrive { speed: -7 }),
        [0x81, 0x01, 0x04, 0x07, 0x37, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::ZoomDrive { speed: 0 }),
        [0x81, 0x01, 0x04, 0x07, 0x00, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::ZoomDirect { position: 0x4000 }),
        [0x81, 0x01, 0x04, 0x47, 0x04, 0x00, 0x00, 0x00, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::FocusDrive { speed: -2 }),
        [0x81, 0x01, 0x04, 0x08, 0x32, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::FocusDirect { position: 0x0ABC }),
        [0x81, 0x01, 0x04, 0x48, 0x00, 0x0A, 0x0B, 0x0C, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::FocusMode(FocusMode::Auto)),
        [0x81, 0x01, 0x04, 0x38, 0x02, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::FocusMode(FocusMode::Manual)),
        [0x81, 0x01, 0x04, 0x38, 0x03, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::Preset {
            action: PresetAction::Recall,
            slot: 0x7F,
        }),
        [0x81, 0x01, 0x04, 0x3F, 0x02, 0x7F, 0xFF]
    );
    assert_eq!(
        ViscaCommand::Preset {
            action: PresetAction::Set,
            slot: 3
        }
        .encode(ViscaAddress::new(2).unwrap())
        .unwrap(),
        [0x82, 0x01, 0x04, 0x3F, 0x01, 0x03, 0xFF]
    );
    assert_eq!(
        ViscaCommand::Preset {
            action: PresetAction::Set,
            slot: 0xFF
        }
        .encode(cam1()),
        Err(ViscaEncodeError::PresetOutOfRange(0xFF))
    );
    assert_eq!(
        enc(ViscaCommand::IfClearBroadcast),
        [0x88, 0x01, 0x00, 0x01, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::AddressSetBroadcast),
        [0x88, 0x30, 0x01, 0xFF]
    );
    assert_eq!(
        enc(ViscaCommand::CommandCancel { socket: 2 }),
        [0x81, 0x22, 0xFF]
    );
    assert_eq!(
        ViscaCommand::CommandCancel { socket: 3 }.encode(cam1()),
        Err(ViscaEncodeError::SocketOutOfRange(3))
    );
    assert_eq!(
        ViscaCommand::ZoomDrive { speed: 8 }.encode(cam1()),
        Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Zoom,
            value: 8
        })
    );
}

#[test]
fn inquiries_have_exact_bytes() {
    let cam3 = ViscaAddress::new(3).unwrap();
    assert_eq!(
        ViscaInquiry::PanTiltPosition.encode(cam3),
        [0x83, 0x09, 0x06, 0x12, 0xFF]
    );
    assert_eq!(
        ViscaInquiry::ZoomPosition.encode(cam3),
        [0x83, 0x09, 0x04, 0x47, 0xFF]
    );
    assert_eq!(
        ViscaInquiry::FocusPosition.encode(cam3),
        [0x83, 0x09, 0x04, 0x48, 0xFF]
    );
}

#[test]
fn replies_parse_ack_completion_error_and_broadcasts() {
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x41, 0xFF]),
        Ok(ViscaReply::Ack {
            address: 1,
            socket: 1
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0xA0, 0x52, 0xFF]),
        Ok(ViscaReply::Completion {
            address: 2,
            socket: 2,
            data: vec![]
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x60, 0x02, 0xFF]),
        Ok(ViscaReply::Error {
            address: 1,
            socket: 0,
            code: ViscaErrorCode::Syntax
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x61, 0x41, 0xFF]),
        Ok(ViscaReply::Error {
            address: 1,
            socket: 1,
            code: ViscaErrorCode::CommandNotExecutable
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x60, 0x03, 0xFF]).unwrap(),
        ViscaReply::Error {
            address: 1,
            socket: 0,
            code: ViscaErrorCode::CommandBufferFull
        }
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x62, 0x04, 0xFF]).unwrap(),
        ViscaReply::Error {
            address: 1,
            socket: 2,
            code: ViscaErrorCode::CommandCancelled
        }
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x60, 0x77, 0xFF]).unwrap(),
        ViscaReply::Error {
            address: 1,
            socket: 0,
            code: ViscaErrorCode::Other(0x77)
        }
    );
    assert_eq!(
        ViscaReply::parse(&[0x88, 0x30, 0x02, 0xFF]),
        Ok(ViscaReply::AddressSet { next_address: 2 })
    );
    assert_eq!(
        ViscaReply::parse(&[0xF0, 0x38, 0xFF]),
        Ok(ViscaReply::NetworkChange { address: 7 })
    );
}

#[test]
fn malformed_replies_are_rejected() {
    assert_eq!(
        ViscaReply::parse(&[0x90, 0xFF]),
        Err(ViscaParseError::TooShort)
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x41, 0x00]),
        Err(ViscaParseError::MissingTerminator)
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0xFF, 0xFF]),
        Err(ViscaParseError::EmbeddedTerminator)
    );
    assert_eq!(
        ViscaReply::parse(&[0x81, 0x41, 0xFF]),
        Err(ViscaParseError::InvalidHeader(0x81))
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x41, 0x00, 0xFF]),
        Err(ViscaParseError::InvalidLength {
            expected: 3,
            actual: 4
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x60, 0xFF]),
        Err(ViscaParseError::InvalidLength {
            expected: 4,
            actual: 3
        })
    );
    assert_eq!(
        ViscaReply::parse(&[0x90, 0x70, 0xFF]),
        Err(ViscaParseError::UnknownMessage(0x70))
    );
    assert_eq!(
        ViscaReply::parse(&[0x90; 17]),
        Err(ViscaParseError::TooLong)
    );
}

#[test]
fn inquiry_completions_decode_positions() {
    let reply = ViscaReply::parse(&[
        0x90, 0x50, 0x0F, 0x06, 0x07, 0x00, 0x00, 0x01, 0x0B, 0x00, 0xFF,
    ])
    .unwrap();
    let ViscaReply::Completion { data, .. } = reply else {
        panic!("expected completion");
    };
    assert_eq!(decode_pan_tilt_position(&data), Ok((-2_448, 432)));

    assert_eq!(decode_lens_position(&[0x04, 0x00, 0x00, 0x00]), Ok(16_384));
    assert_eq!(
        decode_lens_position(&[0x04, 0x10, 0x00, 0x00]),
        Err(ViscaParseError::InvalidNibble(0x10))
    );
    assert_eq!(
        decode_pan_tilt_position(&[0; 4]),
        Err(ViscaParseError::InvalidLength {
            expected: 8,
            actual: 4
        })
    );
}

#[test]
fn stream_decoder_splits_fragments_and_resyncs_after_overlong_input() {
    let mut decoder = ViscaReplyDecoder::new();
    assert_eq!(decoder.push(&[0x90, 0x41]), Vec::new());
    assert_eq!(decoder.pending(), 2);
    assert_eq!(
        decoder.push(&[0xFF, 0x90, 0x51, 0xFF, 0x90]),
        vec![
            Ok(ViscaReply::Ack {
                address: 1,
                socket: 1
            }),
            Ok(ViscaReply::Completion {
                address: 1,
                socket: 1,
                data: vec![]
            }),
        ]
    );
    assert_eq!(decoder.pending(), 1);

    let mut overlong = vec![0x00; 20];
    overlong.extend_from_slice(&[0xFF, 0x90, 0x42, 0xFF]);
    assert_eq!(
        decoder.push(&overlong),
        vec![
            Err(ViscaParseError::TooLong),
            Ok(ViscaReply::Ack {
                address: 1,
                socket: 2
            }),
        ]
    );
    assert_eq!(decoder.pending(), 0);
}

#[test]
fn intents_translate_to_visca_command_sequences() {
    let speeds = ViscaMoveSpeeds::default();
    let continuous = PtzIntent::MoveContinuous(ContinuousMove {
        source: ContinuousSource::Joystick,
        pan: -12,
        tilt: 4,
        zoom: 2,
        focus: 0,
    });
    let bytes: Vec<Vec<u8>> = visca_commands_for_intent(&continuous, speeds)
        .unwrap()
        .into_iter()
        .map(enc)
        .collect();
    assert_eq!(
        bytes,
        vec![
            vec![0x81, 0x01, 0x06, 0x01, 0x0C, 0x04, 0x01, 0x01, 0xFF],
            vec![0x81, 0x01, 0x04, 0x07, 0x22, 0xFF],
            vec![0x81, 0x01, 0x04, 0x08, 0x00, 0xFF],
        ]
    );

    let absolute = PtzIntent::MoveAbsolute(AbsoluteMove {
        pan: 0,
        tilt: 0,
        zoom: Some(0x1000),
        focus: None,
    });
    assert_eq!(
        visca_commands_for_intent(&absolute, speeds).unwrap(),
        vec![
            ViscaCommand::PanTiltAbsolute {
                speeds,
                pan: 0,
                tilt: 0
            },
            ViscaCommand::ZoomDirect { position: 0x1000 },
        ]
    );

    assert_eq!(
        visca_commands_for_intent(
            &PtzIntent::SavePreset {
                id: PresetId::new(9),
                name: "Wide".into()
            },
            speeds
        )
        .unwrap(),
        vec![ViscaCommand::Preset {
            action: PresetAction::Set,
            slot: 9
        }]
    );
    assert_eq!(
        visca_commands_for_intent(&PtzIntent::DeletePreset(PresetId::new(9)), speeds).unwrap(),
        vec![ViscaCommand::Preset {
            action: PresetAction::Reset,
            slot: 9
        }]
    );
    assert_eq!(
        visca_commands_for_intent(&PtzIntent::Stop, speeds)
            .unwrap()
            .into_iter()
            .map(enc)
            .collect::<Vec<_>>(),
        vec![
            vec![0x81, 0x01, 0x06, 0x01, 0x01, 0x01, 0x03, 0x03, 0xFF],
            vec![0x81, 0x01, 0x04, 0x07, 0x00, 0xFF],
            vec![0x81, 0x01, 0x04, 0x08, 0x00, 0xFF],
        ]
    );
    assert_eq!(
        visca_commands_for_intent(
            &PtzIntent::MoveRelative(RelativeMove {
                pan: 1,
                tilt: 1,
                zoom: Some(5),
                focus: None
            }),
            speeds
        ),
        Err(ViscaEncodeError::UnsupportedRelativeAxis(Axis::Zoom))
    );
    assert_eq!(
        visca_commands_for_intent(
            &PtzIntent::MoveContinuous(ContinuousMove {
                source: ContinuousSource::Mouse,
                pan: 300,
                tilt: 0,
                zoom: 0,
                focus: 0
            }),
            speeds
        ),
        Err(ViscaEncodeError::SpeedOutOfRange {
            axis: Axis::Pan,
            value: 300
        })
    );
}

#[test]
fn clamped_fake_visca_intents_always_encode() {
    let camera_id = CameraId::new("cam").unwrap();
    let mut adapter = FakeViscaAdapter::new([FakeViscaCamera::new(camera_id.clone(), "Cam")]);
    let outcome = adapter
        .execute(
            &camera_id,
            PtzIntent::MoveContinuous(ContinuousMove {
                source: ContinuousSource::Joystick,
                pan: 1_000,
                tilt: -1_000,
                zoom: 50,
                focus: -50,
            }),
        )
        .unwrap();
    let commands = visca_commands_for_intent(&outcome.applied, ViscaMoveSpeeds::default()).unwrap();
    assert_eq!(
        commands.into_iter().map(enc).collect::<Vec<_>>(),
        vec![
            vec![0x81, 0x01, 0x06, 0x01, 0x18, 0x14, 0x02, 0x02, 0xFF],
            vec![0x81, 0x01, 0x04, 0x07, 0x27, 0xFF],
            vec![0x81, 0x01, 0x04, 0x08, 0x37, 0xFF],
        ]
    );
}

#[test]
fn visca_over_ip_packets_have_exact_headers() {
    let payload = enc(ViscaCommand::Preset {
        action: PresetAction::Recall,
        slot: 1,
    });
    let packet = ViscaIpPacket {
        payload_type: ViscaIpPayloadType::Command,
        sequence: 0x0102_0304,
        payload,
    };
    let bytes = packet.encode().unwrap();
    assert_eq!(
        bytes,
        [
            0x01, 0x00, 0x00, 0x07, 0x01, 0x02, 0x03, 0x04, 0x81, 0x01, 0x04, 0x3F, 0x02, 0x01,
            0xFF
        ]
    );
    assert_eq!(ViscaIpPacket::decode(&bytes), Ok(packet));

    let reply = ViscaIpPacket::decode(&[
        0x01, 0x11, 0x00, 0x03, 0xFF, 0xFF, 0xFF, 0xFF, 0x90, 0x41, 0xFF,
    ])
    .unwrap();
    assert_eq!(reply.payload_type, ViscaIpPayloadType::Reply);
    assert_eq!(reply.sequence, u32::MAX);
    assert_eq!(
        ViscaReply::parse(&reply.payload),
        Ok(ViscaReply::Ack {
            address: 1,
            socket: 1
        })
    );
}

#[test]
fn visca_over_ip_rejects_bad_framing() {
    assert_eq!(
        ViscaIpPacket::decode(&[0x01, 0x00, 0x00]),
        Err(ViscaIpError::TruncatedHeader(3))
    );
    assert_eq!(
        ViscaIpPacket::decode(&[0x03, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x01]),
        Err(ViscaIpError::UnknownPayloadType(0x0300))
    );
    assert_eq!(
        ViscaIpPacket::decode(&[0x01, 0x00, 0x00, 0x05, 0, 0, 0, 0, 0x81, 0xFF]),
        Err(ViscaIpError::LengthMismatch {
            declared: 5,
            actual: 2
        })
    );
    assert_eq!(
        ViscaIpPacket::decode(&[0x01, 0x00, 0x00, 0x00, 0, 0, 0, 0]),
        Err(ViscaIpError::EmptyPayload)
    );
    assert_eq!(
        ViscaIpPacket {
            payload_type: ViscaIpPayloadType::Command,
            sequence: 0,
            payload: vec![0; 17],
        }
        .encode(),
        Err(ViscaIpError::PayloadTooLong(17))
    );
}

#[test]
fn sequencer_wraps_and_reset_emits_control_packet() {
    let mut sequencer = ViscaIpSequencer::starting_at(u32::MAX);
    let last = sequencer.wrap(
        ViscaIpPayloadType::Inquiry,
        vec![0x81, 0x09, 0x04, 0x47, 0xFF],
    );
    assert_eq!(last.sequence, u32::MAX);
    assert_eq!(sequencer.peek(), 0);

    let mut sequencer = ViscaIpSequencer::starting_at(41);
    let reset = sequencer.reset();
    assert_eq!(
        reset.encode().unwrap(),
        [0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01]
    );
    assert_eq!(sequencer.peek(), 1);
    assert_eq!(
        ViscaIpControl::decode(reset.payload_type, &reset.payload),
        Some(ViscaIpControl::Reset)
    );
    let error =
        ViscaIpPacket::decode(&[0x02, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x07, 0x0F, 0x01])
            .unwrap();
    assert_eq!(
        ViscaIpControl::decode(error.payload_type, &error.payload),
        Some(ViscaIpControl::SequenceNumberError)
    );
    assert_eq!(
        ViscaIpControl::decode(ViscaIpPayloadType::ControlReply, &[0x01]),
        Some(ViscaIpControl::ResetAck)
    );
    assert_eq!(ViscaIpControl::MessageTypeError.payload(), &[0x0F, 0x02]);
}

#[test]
fn speed_mapping_applies_deadzone_curve_and_rounding() {
    let linear = SpeedMapping::default();
    assert_eq!(linear.map_axis(0.0, 24), 0);
    assert_eq!(linear.map_axis(1.0, 24), 24);
    assert_eq!(linear.map_axis(-1.0, 24), -24);
    assert_eq!(linear.map_axis(0.5, 24), 13); // 1 + round(11.5)
    assert_eq!(linear.map_axis(1e-9, 24), 1);
    assert_eq!(linear.map_axis(4.0, 24), 24);
    assert_eq!(linear.map_axis(f64::NAN, 24), 0);
    assert_eq!(linear.map_axis(1.0, 0), 0);

    let shaped = SpeedMapping::new(0.2, ResponseCurve::Quadratic).unwrap();
    assert_eq!(shaped.map_axis(0.2, 24), 0);
    assert_eq!(shaped.map_axis(-0.19, 24), 0);
    assert_eq!(shaped.map_axis(0.6, 24), 7); // t=0.5, 0.25*23=5.75 -> 1+6
    assert_eq!(shaped.map_axis(-1.0, 20), -20);

    let cubic = SpeedMapping::new(0.0, ResponseCurve::Cubic).unwrap();
    assert_eq!(cubic.map_axis(0.5, 24), 4); // 0.125*23=2.875 -> 1+3

    assert_eq!(SpeedMapping::new(1.0, ResponseCurve::Linear), None);
    assert_eq!(SpeedMapping::new(-0.1, ResponseCurve::Linear), None);
    assert_eq!(SpeedMapping::new(f64::NAN, ResponseCurve::Linear), None);
}

#[test]
fn joystick_and_mouse_vectors_map_to_encodable_continuous_moves() {
    let capabilities = FakeViscaCamera::new(CameraId::new("cam").unwrap(), "Cam").capabilities;
    let mapping = SpeedMapping::new(0.1, ResponseCurve::Linear).unwrap();
    let movement = mapping.map(
        ContinuousSource::Joystick,
        ControlVector {
            pan: 1.0,
            tilt: -0.55,
            zoom: 0.05,
            focus: -1.0,
        },
        &capabilities,
    );
    // tilt: t=0.5, 1+round(0.5*19)=11; zoom inside deadzone.
    assert_eq!(
        movement,
        ContinuousMove {
            source: ContinuousSource::Joystick,
            pan: 24,
            tilt: -11,
            zoom: 0,
            focus: -7,
        }
    );
    assert_eq!(
        visca_commands_for_intent(
            &PtzIntent::MoveContinuous(movement),
            ViscaMoveSpeeds::default()
        )
        .unwrap()
        .into_iter()
        .map(enc)
        .collect::<Vec<_>>(),
        vec![
            vec![0x81, 0x01, 0x06, 0x01, 0x18, 0x0B, 0x02, 0x02, 0xFF],
            vec![0x81, 0x01, 0x04, 0x07, 0x00, 0xFF],
            vec![0x81, 0x01, 0x04, 0x08, 0x37, 0xFF],
        ]
    );

    assert_eq!(MouseDragMapping::new(0, 4), None);
    let mouse = MouseDragMapping::new(200, 4).unwrap();
    let vector = mouse.vector(100, -400, -2);
    assert_eq!(
        vector,
        ControlVector {
            pan: 0.5,
            tilt: 1.0,
            zoom: -0.5,
            focus: 0.0,
        }
    );
    assert_eq!(
        SpeedMapping::default().map(ContinuousSource::Mouse, vector, &capabilities),
        ContinuousMove {
            source: ContinuousSource::Mouse,
            pan: 13,
            tilt: 20,
            zoom: -4,
            focus: 0,
        }
    );
}
