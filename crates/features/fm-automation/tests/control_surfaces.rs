use fm_automation::{
    ActionValue, ActivatorRule, BindingId, ButtonEdge, CommandIntent, ConflictPolicy,
    ControlBinding, ControlEvent, ControlId, ControlLearnRequest, ControlMap, ControlMapError,
    ControlOutput, ControlValue, DeviceKey, DeviceProfile, DeviceTransport, FeedbackBinding,
    FeedbackError, FeedbackPayload, FeedbackRouter, FeedbackSource, FeedbackTarget, LearnEffect,
    LearnState, MAX_CONTROL_BINDINGS, MidiChannel, MidiControlDecoder, MidiError, MidiMessage,
    MidiParser, MidiRealTime, OSC_TIME_TAG_IMMEDIATE, OscArg, OscBundle, OscError,
    OscFeedbackFormat, OscLimits, OscMessage, OscPacket, Pickup, RelativeEncoding, Response, Span,
    Suppressed, SysExPolicy, TallySnapshot, VelocityRange, decode_osc_packet, encode_osc_packet,
    osc_address_matches, osc_control_events,
};
use fm_types::{InputId, MediaTimestamp};
use std::num::NonZeroU128;

fn ch(index: u8) -> MidiChannel {
    MidiChannel::new(index).unwrap()
}

fn ok(results: Vec<Result<MidiMessage, MidiError>>) -> Vec<MidiMessage> {
    results.into_iter().map(Result::unwrap).collect()
}

fn ts(ticks: i64) -> MediaTimestamp {
    MediaTimestamp::new(ticks)
}

fn key(text: &str) -> DeviceKey {
    DeviceKey::new(text).unwrap()
}

fn id(text: &str) -> BindingId {
    BindingId::new(text).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

// ------------------------------------------------------------------ MIDI

#[test]
fn midi_running_status_with_interleaved_real_time() {
    let mut parser = MidiParser::default();
    let messages = ok(parser.feed(&[
        0x90, 0x3C, 0xF8, 0x64, // Note On with a clock between data bytes
        0x3E, 0x00, // running status: Note On 62 vel 0
        0xB1, 0x07, 0xFE, 0x7F, // CC on channel 2, active sensing inside
        0x0A, 0x40, // running status CC 10
        0xE0, 0x00, 0x40, // pitch bend centre
        0xC2, 0x05, 0x06, // program change with running status
    ]));
    assert_eq!(
        messages,
        vec![
            MidiMessage::RealTime(MidiRealTime::TimingClock),
            MidiMessage::NoteOn {
                channel: ch(0),
                note: 0x3C,
                velocity: 0x64
            },
            MidiMessage::NoteOn {
                channel: ch(0),
                note: 0x3E,
                velocity: 0
            },
            MidiMessage::RealTime(MidiRealTime::ActiveSensing),
            MidiMessage::ControlChange {
                channel: ch(1),
                controller: 7,
                value: 127
            },
            MidiMessage::ControlChange {
                channel: ch(1),
                controller: 10,
                value: 64
            },
            MidiMessage::PitchBend {
                channel: ch(0),
                value: 0x2000
            },
            MidiMessage::ProgramChange {
                channel: ch(2),
                program: 5
            },
            MidiMessage::ProgramChange {
                channel: ch(2),
                program: 6
            },
        ]
    );
    // Byte-at-a-time feeding produces the same stream.
    let mut split = MidiParser::default();
    let mut stepwise = Vec::new();
    for byte in [0x90, 0x3C, 0xF8, 0x64, 0x3E, 0x00] {
        stepwise.extend(ok(split.feed(&[byte])));
    }
    assert_eq!(stepwise, messages[..3]);
}

#[test]
fn midi_system_common_clears_running_status() {
    let mut parser = MidiParser::default();
    let results = parser.feed(&[0x90, 0x40, 0x7F, 0xF3, 0x02, 0x41, 0x10]);
    assert_eq!(
        results,
        vec![
            Ok(MidiMessage::NoteOn {
                channel: ch(0),
                note: 0x40,
                velocity: 0x7F
            }),
            Ok(MidiMessage::SongSelect(2)),
            Err(MidiError::UnexpectedDataByte(0x41)),
            Err(MidiError::UnexpectedDataByte(0x10)),
        ]
    );
    assert_eq!(
        parser.feed(&[0xF2, 0x01, 0x02, 0xF1, 0x35, 0xF6]),
        vec![
            Ok(MidiMessage::SongPosition(0x0101)),
            Ok(MidiMessage::TimeCodeQuarterFrame(0x35)),
            Ok(MidiMessage::TuneRequest),
        ]
    );
}

#[test]
fn midi_sysex_capture_skip_overflow_and_errors() {
    let mut capture = MidiParser::new(SysExPolicy::Capture { max_bytes: 4 }).unwrap();
    assert_eq!(
        capture.feed(&[0xF0, 0x7E, 0xF8, 0x01, 0x02, 0xF7, 0x90, 0x01, 0x02]),
        vec![
            Ok(MidiMessage::RealTime(MidiRealTime::TimingClock)),
            Ok(MidiMessage::SysEx(vec![0x7E, 0x01, 0x02])),
            Ok(MidiMessage::NoteOn {
                channel: ch(0),
                note: 1,
                velocity: 2
            }),
        ]
    );
    // Overflow is reported once and the rest of the message is discarded.
    assert_eq!(
        capture.feed(&[0xF0, 1, 2, 3, 4, 5, 6, 0xF7]),
        vec![Err(MidiError::SysExOverflow { limit: 4 })]
    );
    // SysEx cancels running status.
    assert_eq!(
        capture.feed(&[0x05]),
        vec![Err(MidiError::UnexpectedDataByte(5))]
    );
    // A status byte terminates SysEx without F7 and starts its own message.
    assert_eq!(
        capture.feed(&[0xF0, 0x01, 0xB0, 0x07, 0x10]),
        vec![
            Err(MidiError::UnterminatedSysEx),
            Ok(MidiMessage::ControlChange {
                channel: ch(0),
                controller: 7,
                value: 0x10
            }),
        ]
    );

    let mut skip = MidiParser::default();
    let long: Vec<u8> = std::iter::once(0xF0)
        .chain(std::iter::repeat_n(0x11, 10_000))
        .chain([0xF7, 0xF7, 0xF4, 0xFD, 0x80, 0x10, 0x90])
        .collect();
    assert_eq!(
        skip.feed(&long),
        vec![
            Err(MidiError::StrayEndOfExclusive),
            Err(MidiError::UndefinedStatus(0xF4)),
            Err(MidiError::UndefinedStatus(0xFD)),
            Err(MidiError::IncompleteMessage { status: 0x80 }),
        ]
    );
    skip.reset();
    assert_eq!(
        skip.feed(&[0x10]),
        vec![Err(MidiError::UnexpectedDataByte(0x10))]
    );

    assert_eq!(
        MidiParser::new(SysExPolicy::Capture { max_bytes: 0 }).unwrap_err(),
        MidiError::InvalidSysExLimit(0)
    );
    assert_eq!(MidiChannel::new(16), Err(MidiError::InvalidChannel(16)));
}

#[test]
fn midi_encode_round_trips_exact_bytes() {
    let messages = vec![
        MidiMessage::NoteOff {
            channel: ch(15),
            note: 1,
            velocity: 2,
        },
        MidiMessage::PolyPressure {
            channel: ch(3),
            note: 60,
            pressure: 9,
        },
        MidiMessage::ChannelPressure {
            channel: ch(4),
            pressure: 99,
        },
        MidiMessage::PitchBend {
            channel: ch(1),
            value: 0x3FFF,
        },
        MidiMessage::SysEx(vec![0x7F, 0x00]),
        MidiMessage::SongPosition(300),
        MidiMessage::RealTime(MidiRealTime::Stop),
    ];
    let mut bytes = Vec::new();
    for message in &messages {
        bytes.extend(message.encode().unwrap());
    }
    assert_eq!(
        bytes,
        [
            0x8F, 1, 2, 0xA3, 60, 9, 0xD4, 99, 0xE1, 0x7F, 0x7F, 0xF0, 0x7F, 0x00, 0xF7, 0xF2,
            0x2C, 0x02, 0xFC
        ]
    );
    let mut parser = MidiParser::new(SysExPolicy::Capture { max_bytes: 16 }).unwrap();
    assert_eq!(ok(parser.feed(&bytes)), messages);
    assert_eq!(
        MidiMessage::ControlChange {
            channel: ch(0),
            controller: 0x80,
            value: 0
        }
        .encode(),
        Err(MidiError::InvalidDataByte(0x80))
    );
    assert_eq!(
        MidiMessage::PitchBend {
            channel: ch(0),
            value: 0x4000
        }
        .encode(),
        Err(MidiError::Invalid14BitValue(0x4000))
    );
}

#[test]
fn midi_control_decoder_notes_14bit_and_pitch_bend() {
    let mut decoder = MidiControlDecoder::default();
    decoder.enable_14bit(ch(0), 7).unwrap();
    assert_eq!(
        decoder.enable_14bit(ch(0), 32),
        Err(MidiError::Invalid14BitController(32))
    );
    let mut parser = MidiParser::default();
    let events: Vec<ControlEvent> = ok(parser.feed(&[
        0x90, 0x24, 0x50, // press
        0x24, 0x00, // Note On vel 0 = release
        0x80, 0x24, 0x40, // Note Off
        0xB0, 0x07, 0x40, // 14-bit MSB
        0x27, 0x05, // paired LSB (running status)
        0x27, 0x06, // LSB only: reuses latest MSB
        0x08, 0x10, // plain 7-bit controller
        0xB1, 0x27, 0x01, // channel 2 has no 14-bit pair: plain CC 39
        0xE3, 0x7F, 0x7F, // pitch bend maximum
        0xF8, 0xC0, 0x03,
    ]))
    .iter()
    .filter_map(|message| decoder.decode(message))
    .collect();
    assert_eq!(events, decoder_expected());
}

fn decoder_expected() -> Vec<ControlEvent> {
    let note = ControlId::MidiNote {
        channel: ch(0),
        note: 0x24,
    };
    let fader = ControlId::MidiCc14 {
        channel: ch(0),
        controller: 7,
    };
    let expected = vec![
        ControlEvent {
            control: note.clone(),
            value: ControlValue::Button {
                pressed: true,
                velocity: 0x50,
            },
        },
        ControlEvent {
            control: note.clone(),
            value: ControlValue::Button {
                pressed: false,
                velocity: 0,
            },
        },
        ControlEvent {
            control: note,
            value: ControlValue::Button {
                pressed: false,
                velocity: 0x40,
            },
        },
        ControlEvent {
            control: fader.clone(),
            value: ControlValue::Absolute {
                raw: 0x40 << 7,
                max: 0x3FFF,
            },
        },
        ControlEvent {
            control: fader.clone(),
            value: ControlValue::Absolute {
                raw: (0x40 << 7) | 5,
                max: 0x3FFF,
            },
        },
        ControlEvent {
            control: fader,
            value: ControlValue::Absolute {
                raw: (0x40 << 7) | 6,
                max: 0x3FFF,
            },
        },
        ControlEvent {
            control: ControlId::MidiCc {
                channel: ch(0),
                controller: 8,
            },
            value: ControlValue::Absolute { raw: 16, max: 127 },
        },
        ControlEvent {
            control: ControlId::MidiCc {
                channel: ch(1),
                controller: 39,
            },
            value: ControlValue::Absolute { raw: 1, max: 127 },
        },
        ControlEvent {
            control: ControlId::MidiPitchBend { channel: ch(3) },
            value: ControlValue::Absolute {
                raw: 0x3FFF,
                max: 0x3FFF,
            },
        },
        ControlEvent {
            control: ControlId::MidiProgram {
                channel: ch(0),
                program: 3,
            },
            value: ControlValue::Button {
                pressed: true,
                velocity: 127,
            },
        },
    ];
    expected
}

// ------------------------------------------------------------------- OSC

const FADER_FIXTURE: [u8; 20] = [
    b'/', b'f', b'a', b'd', b'e', b'r', b'/', b'1', 0, 0, 0, 0, b',', b'f', 0, 0, 0x3F, 0x00, 0x00,
    0x00,
];

#[test]
fn osc_message_exact_fixture() {
    let limits = OscLimits::default();
    let packet = OscPacket::Message(OscMessage::new("/fader/1", vec![OscArg::Float(0.5)]));
    assert_eq!(encode_osc_packet(&packet, &limits).unwrap(), FADER_FIXTURE);
    assert_eq!(decode_osc_packet(&FADER_FIXTURE, &limits).unwrap(), packet);

    let all = OscPacket::Message(OscMessage::new(
        "/a",
        vec![
            OscArg::Int(-2),
            OscArg::String("hi".into()),
            OscArg::Blob(vec![1, 2, 3, 4, 5]),
            OscArg::True,
            OscArg::False,
        ],
    ));
    let bytes = encode_osc_packet(&all, &limits).unwrap();
    let expected: Vec<u8> = [
        &b"/a\0\0"[..],
        b",isbTF\0\0",
        &[0xFF, 0xFF, 0xFF, 0xFE],
        b"hi\0\0",
        &[0, 0, 0, 5, 1, 2, 3, 4, 5, 0, 0, 0],
    ]
    .concat();
    assert_eq!(bytes, expected);
    assert_eq!(decode_osc_packet(&bytes, &limits).unwrap(), all);

    // Exactly four characters needs a full padding word.
    let four = OscPacket::Message(OscMessage::new("/abc", vec![]));
    assert_eq!(
        encode_osc_packet(&four, &limits).unwrap(),
        b"/abc\0\0\0\0,\0\0\0"
    );
}

#[test]
fn osc_bundle_fixture_and_depth_bound() {
    let limits = OscLimits::default();
    let inner = OscPacket::Bundle(OscBundle {
        time_tag: 7,
        elements: vec![OscPacket::Message(OscMessage::new(
            "/fader/1",
            vec![OscArg::Float(0.5)],
        ))],
    });
    let outer = OscPacket::Bundle(OscBundle {
        time_tag: OSC_TIME_TAG_IMMEDIATE,
        elements: vec![OscPacket::Message(OscMessage::new("/go", vec![])), inner],
    });
    let bytes = encode_osc_packet(&outer, &limits).unwrap();
    let expected: Vec<u8> = [
        &b"#bundle\0"[..],
        &[0, 0, 0, 0, 0, 0, 0, 1],
        &[0, 0, 0, 8],
        b"/go\0,\0\0\0",
        &[0, 0, 0, 40],
        b"#bundle\0",
        &[0, 0, 0, 0, 0, 0, 0, 7],
        &[0, 0, 0, 20],
        &FADER_FIXTURE,
    ]
    .concat();
    assert_eq!(bytes, expected);
    assert_eq!(decode_osc_packet(&bytes, &limits).unwrap(), outer);

    let shallow = OscLimits {
        max_bundle_depth: 1,
        ..limits
    };
    assert_eq!(
        decode_osc_packet(&bytes, &shallow),
        Err(OscError::BundleTooDeep { limit: 1 })
    );
    assert_eq!(
        encode_osc_packet(&outer, &shallow),
        Err(OscError::BundleTooDeep { limit: 1 })
    );
    let one_message = OscLimits {
        max_messages: 1,
        ..limits
    };
    assert_eq!(
        decode_osc_packet(&bytes, &one_message),
        Err(OscError::TooManyMessages { limit: 1 })
    );

    let mut bad_size = expected.clone();
    bad_size[16..20].copy_from_slice(&[0, 0, 0, 6]);
    assert_eq!(
        decode_osc_packet(&bad_size, &limits),
        Err(OscError::InvalidElementSize(6))
    );
    let mut negative = expected;
    negative[16..20].copy_from_slice(&(-4_i32).to_be_bytes());
    assert_eq!(
        decode_osc_packet(&negative, &limits),
        Err(OscError::InvalidElementSize(-4))
    );

    let events = osc_control_events(&outer).unwrap();
    assert_eq!(
        events,
        vec![
            ControlEvent {
                control: ControlId::Osc("/go".into()),
                value: ControlValue::Button {
                    pressed: true,
                    velocity: 127
                },
            },
            ControlEvent {
                control: ControlId::Osc("/fader/1".into()),
                value: ControlValue::Float(0.5),
            },
        ]
    );
}

#[test]
fn osc_decode_rejects_malformed_packets() {
    let limits = OscLimits::default();
    assert_eq!(decode_osc_packet(&[], &limits), Err(OscError::EmptyPacket));
    assert_eq!(
        decode_osc_packet(&FADER_FIXTURE[..18], &limits),
        Err(OscError::Misaligned { len: 18 })
    );
    assert_eq!(
        decode_osc_packet(&FADER_FIXTURE[..16], &limits),
        Err(OscError::Truncated)
    );
    let mut padding = FADER_FIXTURE;
    padding[10] = b'x';
    assert_eq!(
        decode_osc_packet(&padding, &limits),
        Err(OscError::InvalidString)
    );
    assert_eq!(
        decode_osc_packet(b"/abc\0\0\0\0", &limits),
        Err(OscError::MissingTypeTags)
    );
    assert_eq!(
        decode_osc_packet(b"/a\0\0,d\0\0\0\0\0\0\0\0\0\0", &limits),
        Err(OscError::UnsupportedTypeTag('d'))
    );
    assert_eq!(
        decode_osc_packet(b"/a\0\0,\0\0\0\0\0\0\0", &limits),
        Err(OscError::TrailingBytes)
    );
    assert_eq!(
        decode_osc_packet(b"/a\0\0,b\0\0\0\0\0\x01\x07\x01\0\0", &limits),
        Err(OscError::InvalidBlobPadding)
    );
    assert_eq!(
        decode_osc_packet(b"a\0\0\0,\0\0\0", &limits),
        Err(OscError::InvalidPattern("a".into()))
    );
    let few_args = OscLimits {
        max_arguments: 1,
        ..limits
    };
    assert_eq!(
        decode_osc_packet(b"/a\0\0,TF\0", &few_args),
        Err(OscError::TooManyArguments { limit: 1 })
    );
    assert_eq!(
        encode_osc_packet(
            &OscPacket::Message(OscMessage::new("/a", vec![OscArg::String("é".into())])),
            &limits
        ),
        Err(OscError::InvalidString)
    );
    assert_eq!(
        osc_control_events(&OscPacket::Message(OscMessage::new(
            "/a",
            vec![OscArg::String("x".into())]
        ))),
        Err(OscError::UnsupportedControlArgument('s'))
    );
    assert_eq!(
        osc_control_events(&OscPacket::Message(OscMessage::new(
            "/a",
            vec![OscArg::Float(f32::NAN)]
        ))),
        Err(OscError::NonFiniteArgument)
    );
}

#[test]
fn osc_address_patterns() {
    let cases = [
        ("/mixer/*/fader", "/mixer/3/fader", true),
        ("/mixer/*", "/mixer/3/fader", false),
        ("/mixer/ch?", "/mixer/ch7", true),
        ("/mixer/ch?", "/mixer/ch10", false),
        ("/ch[1-4]/mute", "/ch3/mute", true),
        ("/ch[1-4]/mute", "/ch5/mute", false),
        ("/ch[!1-4]/mute", "/ch5/mute", true),
        ("/ch/{mute,solo}", "/ch/solo", true),
        ("/ch/{mute,solo}", "/ch/rec", false),
        ("/a*c*", "/abcxc", true),
        ("/a*c", "/abcx", false),
        ("/exact", "/exact", true),
    ];
    for (pattern, address, expected) in cases {
        assert_eq!(
            osc_address_matches(pattern, address),
            Ok(expected),
            "{pattern} vs {address}"
        );
    }
    assert_eq!(
        osc_address_matches("/ch[1-4", "/ch1"),
        Err(OscError::InvalidPattern("/ch[1-4".into()))
    );
    assert_eq!(
        osc_address_matches("/ch/*", "/ch/*"),
        Err(OscError::InvalidAddress("/ch/*".into()))
    );
}

// ---------------------------------------------------------------- mapping

#[derive(Clone, Debug, PartialEq)]
enum Act {
    Cut,
    Volume(u8),
    Shuttle,
    Overlay,
}

fn profile(device: &str, transport: DeviceTransport, port: &str) -> DeviceProfile {
    DeviceProfile {
        key: key(device),
        name: format!("{device} surface"),
        transport,
        port_name: port.into(),
    }
}

fn binding(
    name: &str,
    device: &str,
    control: ControlId,
    action: Act,
    response: Response,
) -> ControlBinding<Act> {
    ControlBinding {
        id: id(name),
        device: key(device),
        control,
        action,
        response,
    }
}

fn absolute(invert: bool, deadband: f64, pickup: Pickup) -> Response {
    Response::Absolute {
        input: None,
        output: Span::new(0.0, 100.0).unwrap(),
        invert,
        deadband,
        pickup,
    }
}

fn cc(controller: u8, raw: u16) -> ControlEvent {
    ControlEvent {
        control: ControlId::MidiCc {
            channel: ch(0),
            controller,
        },
        value: ControlValue::Absolute { raw, max: 127 },
    }
}

fn note(number: u8, pressed: bool, velocity: u8) -> ControlEvent {
    ControlEvent {
        control: ControlId::MidiNote {
            channel: ch(0),
            note: number,
        },
        value: ControlValue::Button { pressed, velocity },
    }
}

fn values(outputs: &[CommandIntent<ControlOutput<Act>>]) -> Vec<ActionValue> {
    outputs
        .iter()
        .map(|intent| intent.clone().into_command().value)
        .collect()
}

fn midi_map() -> ControlMap<Act> {
    let mut map = ControlMap::default();
    map.add_profile(profile("akai:apc:01", DeviceTransport::Midi, "APC MINI"))
        .unwrap();
    map.attach("APC MINI", &key("akai:apc:01")).unwrap();
    map
}

#[test]
fn button_bindings_map_velocity_edges_and_toggle() {
    let mut map = midi_map();
    let device = "akai:apc:01";
    map.add_binding(binding(
        "cut",
        device,
        note(1, true, 0).control,
        Act::Cut,
        Response::Button {
            edge: ButtonEdge::PressAndRelease,
            velocity: Some(VelocityRange {
                minimum: 27,
                maximum: 127,
            }),
        },
    ))
    .unwrap();
    map.add_binding(binding(
        "overlay",
        device,
        note(2, true, 0).control,
        Act::Overlay,
        Response::Button {
            edge: ButtonEdge::Toggle,
            velocity: None,
        },
    ))
    .unwrap();

    let report = map.process("APC MINI", &note(1, true, 77), ts(1)).unwrap();
    assert_eq!(report.device, key(device));
    assert_eq!(report.learn, LearnEffect::Inactive);
    assert_eq!(
        report.outputs,
        vec![CommandIntent::discrete(ControlOutput {
            binding: id("cut"),
            action: Act::Cut,
            value: ActionValue::Press { velocity: 0.5 },
            timestamp: ts(1),
        })]
    );
    let release = map.process("APC MINI", &note(1, false, 0), ts(2)).unwrap();
    assert_eq!(values(&release.outputs), vec![ActionValue::Release]);

    let mut toggles = Vec::new();
    for tick in 0..4 {
        let pressed = tick % 2 == 0;
        toggles.extend(values(
            &map.process("APC MINI", &note(2, pressed, 127), ts(tick))
                .unwrap()
                .outputs,
        ));
    }
    assert_eq!(
        toggles,
        vec![ActionValue::Toggled(true), ActionValue::Toggled(false)]
    );
    // Unbound controls produce nothing.
    let none = map.process("APC MINI", &note(9, true, 1), ts(5)).unwrap();
    assert_eq!(none.outputs.len() + none.suppressed.len(), 0);
}

#[test]
fn absolute_faders_linear_inverted_and_deadband() {
    let mut map = midi_map();
    map.add_binding(binding(
        "vol",
        "akai:apc:01",
        cc(7, 0).control,
        Act::Volume(1),
        absolute(false, 0.05, Pickup::Immediate),
    ))
    .unwrap();
    map.add_binding(binding(
        "inv",
        "akai:apc:01",
        cc(8, 0).control,
        Act::Volume(2),
        absolute(true, 0.0, Pickup::Immediate),
    ))
    .unwrap();

    let first = map.process("APC MINI", &cc(7, 127), ts(1)).unwrap();
    assert_eq!(
        first.outputs,
        vec![CommandIntent::continuous(
            "vol",
            ControlOutput {
                binding: id("vol"),
                action: Act::Volume(1),
                value: ActionValue::Level(100.0),
                timestamp: ts(1),
            }
        )]
    );
    // 127 -> 124 is a 2.4% move, inside the 5% deadband.
    let small = map.process("APC MINI", &cc(7, 124), ts(2)).unwrap();
    assert_eq!(small.suppressed, vec![(id("vol"), Suppressed::Deadband)]);
    let large = map.process("APC MINI", &cc(7, 0), ts(3)).unwrap();
    assert_eq!(values(&large.outputs), vec![ActionValue::Level(0.0)]);

    let inverted = map.process("APC MINI", &cc(8, 127), ts(4)).unwrap();
    assert_eq!(values(&inverted.outputs), vec![ActionValue::Level(0.0)]);

    // Button values cannot drive a relative encoder.
    map.add_binding(binding(
        "jog",
        "akai:apc:01",
        note(5, true, 0).control,
        Act::Shuttle,
        Response::Relative {
            encoding: RelativeEncoding::TwosComplement,
            step: 0.1,
            output: Span::new(0.0, 1.0).unwrap(),
        },
    ))
    .unwrap();
    let wrong = map.process("APC MINI", &note(5, true, 9), ts(5)).unwrap();
    assert_eq!(
        wrong.suppressed,
        vec![(id("jog"), Suppressed::IncompatibleValue)]
    );
}

#[test]
fn soft_takeover_waits_for_pickup_and_rearms_on_external_change() {
    let mut map = midi_map();
    map.add_binding(binding(
        "vol",
        "akai:apc:01",
        cc(7, 0).control,
        Act::Volume(1),
        absolute(false, 0.0, Pickup::SoftTakeover { window: 0.02 }),
    ))
    .unwrap();
    map.set_target_value(&id("vol"), 50.0).unwrap();

    let low = map.process("APC MINI", &cc(7, 0), ts(1)).unwrap();
    assert_eq!(
        low.suppressed,
        vec![(
            id("vol"),
            Suppressed::AwaitingPickup {
                hardware: 0.0,
                target: 0.5
            }
        )]
    );
    assert_eq!(low.outputs.len(), 0);
    // Jumping across the target picks up even without landing in the window.
    let crossed = map.process("APC MINI", &cc(7, 127), ts(2)).unwrap();
    assert_eq!(values(&crossed.outputs), vec![ActionValue::Level(100.0)]);
    // The fader is now picked up and tracks freely.
    let tracked = map.process("APC MINI", &cc(7, 0), ts(3)).unwrap();
    assert_eq!(values(&tracked.outputs), vec![ActionValue::Level(0.0)]);

    // An external change away from the fader requires pickup again; landing
    // within the window picks it up.
    map.set_target_value(&id("vol"), 80.0).unwrap();
    let away = map.process("APC MINI", &cc(7, 10), ts(4)).unwrap();
    assert_eq!(away.outputs.len(), 0);
    let near = map.process("APC MINI", &cc(7, 102), ts(5)).unwrap();
    assert_eq!(near.outputs.len(), 1);

    assert_eq!(
        map.set_target_value(&id("vol"), f64::NAN),
        Err(ControlMapError::NonFiniteValue)
    );
}

#[test]
fn relative_encoders_and_hid_jog() {
    let mut map = midi_map();
    let unit = Span::new(0.0, 1.0).unwrap();
    for (name, controller, encoding) in [
        ("twos", 20, RelativeEncoding::TwosComplement),
        ("signed", 21, RelativeEncoding::SignedBit),
        ("offset", 22, RelativeEncoding::BinaryOffset),
    ] {
        map.add_binding(binding(
            name,
            "akai:apc:01",
            cc(controller, 0).control,
            Act::Volume(controller),
            Response::Relative {
                encoding,
                step: 0.25,
                output: unit,
            },
        ))
        .unwrap();
    }
    let mut levels = Vec::new();
    for (controller, raw) in [
        (20, 0x01),
        (20, 0x02),
        (20, 0x7F),
        (21, 0x02),
        (21, 0x41),
        (22, 0x42),
        (22, 0x3F),
    ] {
        levels.extend(values(
            &map.process("APC MINI", &cc(controller, raw), ts(1))
                .unwrap()
                .outputs,
        ));
    }
    assert_eq!(
        levels,
        [0.25, 0.75, 0.5, 0.5, 0.25, 0.5, 0.25]
            .map(ActionValue::Level)
            .to_vec()
    );
    let clamp = map.process("APC MINI", &cc(20, 0x70), ts(2)).unwrap();
    assert_eq!(
        values(&clamp.outputs),
        vec![ActionValue::Level(0.0)],
        "large negative steps clamp at the floor"
    );
    let floor = map.process("APC MINI", &cc(20, 0x7F), ts(3)).unwrap();
    assert_eq!(floor.suppressed, vec![(id("twos"), Suppressed::NoChange)]);

    // HID jog wheel with signed deltas.
    map.add_profile(profile(
        "contour:shuttlepro:9",
        DeviceTransport::Hid,
        "hid-3",
    ))
    .unwrap();
    map.attach("hid-3", &key("contour:shuttlepro:9")).unwrap();
    let jog = ControlId::Hid {
        usage_page: 0x01,
        usage: 0x37,
    };
    map.add_binding(binding(
        "jog",
        "contour:shuttlepro:9",
        jog.clone(),
        Act::Shuttle,
        Response::Relative {
            encoding: RelativeEncoding::Signed,
            step: 0.01,
            output: Span::new(0.0, 1000.0).unwrap(),
        },
    ))
    .unwrap();
    map.set_target_value(&id("jog"), 500.0).unwrap();
    let report = map
        .process(
            "hid-3",
            &ControlEvent {
                control: jog,
                value: ControlValue::Delta(-3),
            },
            ts(4),
        )
        .unwrap();
    assert_eq!(values(&report.outputs), vec![ActionValue::Level(470.0)]);
}

#[test]
fn osc_bindings_match_incoming_patterns() {
    let mut map: ControlMap<Act> = ControlMap::default();
    map.add_profile(profile("touchosc:ipad", DeviceTransport::Osc, "udp:9000"))
        .unwrap();
    map.attach("udp:9000", &key("touchosc:ipad")).unwrap();
    for (index, name) in ["a", "b"].into_iter().enumerate() {
        map.add_binding(binding(
            name,
            "touchosc:ipad",
            ControlId::Osc(format!("/mix/{}/fader", index + 1)),
            Act::Volume(u8::try_from(index).unwrap()),
            absolute(false, 0.0, Pickup::Immediate),
        ))
        .unwrap();
    }
    let packet = decode_osc_packet(
        &encode_osc_packet(
            &OscPacket::Message(OscMessage::new("/mix/*/fader", vec![OscArg::Float(0.25)])),
            &OscLimits::default(),
        )
        .unwrap(),
        &OscLimits::default(),
    )
    .unwrap();
    let event = osc_control_events(&packet).unwrap().remove(0);
    let report = map.process("udp:9000", &event, ts(1)).unwrap();
    assert_eq!(
        values(&report.outputs),
        vec![ActionValue::Level(25.0), ActionValue::Level(25.0)]
    );
    // OSC controls cannot be bound to MIDI devices.
    assert_eq!(
        map.add_binding(binding(
            "bad",
            "touchosc:ipad",
            cc(1, 0).control,
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::TransportMismatch {
            device: key("touchosc:ipad"),
            control: cc(1, 0).control,
        })
    );
    assert_eq!(
        map.add_binding(binding(
            "pattern",
            "touchosc:ipad",
            ControlId::Osc("/mix/*".into()),
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::InvalidControl(ControlId::Osc(
            "/mix/*".into()
        )))
    );
}

#[test]
fn learn_mode_arm_capture_conflict_confirm_cancel() {
    let mut map = midi_map();
    map.add_binding(binding(
        "existing",
        "akai:apc:01",
        note(10, true, 0).control,
        Act::Overlay,
        Response::Button {
            edge: ButtonEdge::Press,
            velocity: None,
        },
    ))
    .unwrap();
    let request = ControlLearnRequest {
        binding: id("learned-cut"),
        device: None,
        action: Act::Cut,
        response: Response::Button {
            edge: ButtonEdge::Press,
            velocity: None,
        },
    };
    map.arm_learn(request.clone()).unwrap();
    assert_eq!(
        map.arm_learn(request.clone()),
        Err(ControlMapError::LearnActive)
    );

    // Releases and continuous values do not qualify for a button response,
    // and nothing is dispatched while learning.
    let release = map.process("APC MINI", &note(10, false, 0), ts(1)).unwrap();
    assert_eq!(release.learn, LearnEffect::Ignored);
    let fader = map.process("APC MINI", &cc(7, 3), ts(2)).unwrap();
    assert_eq!(fader.learn, LearnEffect::Ignored);
    let press = map.process("APC MINI", &note(10, true, 90), ts(3)).unwrap();
    assert_eq!(press.learn, LearnEffect::Captured);
    assert_eq!(press.outputs.len(), 0);
    let LearnState::Captured(candidate) = map.learn_state() else {
        panic!("expected a captured candidate");
    };
    assert_eq!(candidate.conflicts, vec![id("existing")]);
    assert_eq!(candidate.binding.control, note(10, true, 0).control);
    assert_eq!(candidate.captured_at, ts(3));
    // Later events are swallowed until confirm/cancel.
    let after = map.process("APC MINI", &note(11, true, 90), ts(4)).unwrap();
    assert_eq!(after.learn, LearnEffect::Ignored);

    assert_eq!(
        map.confirm_learn(ConflictPolicy::Reject),
        Err(ControlMapError::LearnConflict(vec![id("existing")]))
    );
    let confirmation = map.confirm_learn(ConflictPolicy::ReplaceExisting).unwrap();
    assert_eq!(confirmation.binding, id("learned-cut"));
    assert_eq!(confirmation.replaced, vec![id("existing")]);
    assert_eq!(map.bindings().len(), 1);
    assert_eq!(map.learn_state(), &LearnState::Idle);
    let dispatched = map
        .process("APC MINI", &note(10, true, 127), ts(5))
        .unwrap();
    assert_eq!(
        values(&dispatched.outputs),
        vec![ActionValue::Press { velocity: 1.0 }]
    );

    // Cancel from armed; confirm without capture is explicit.
    map.arm_learn(ControlLearnRequest {
        binding: id("fader"),
        device: Some(key("akai:apc:01")),
        action: Act::Volume(1),
        response: absolute(false, 0.0, Pickup::Immediate),
    })
    .unwrap();
    assert_eq!(
        map.confirm_learn(ConflictPolicy::KeepBoth),
        Err(ControlMapError::NothingCaptured)
    );
    assert!(matches!(map.cancel_learn(), Ok(LearnState::Armed(_))));
    assert_eq!(map.cancel_learn(), Err(ControlMapError::LearnNotArmed));
    assert_eq!(
        map.arm_learn(ControlLearnRequest {
            binding: id("learned-cut"),
            ..request
        }),
        Err(ControlMapError::DuplicateBinding(id("learned-cut")))
    );
}

#[test]
fn reconnect_reassociates_bindings_by_device_key() {
    let mut map = midi_map();
    let device = key("akai:apc:01");
    map.add_binding(binding(
        "vol",
        "akai:apc:01",
        cc(7, 0).control,
        Act::Volume(1),
        absolute(false, 0.0, Pickup::SoftTakeover { window: 0.05 }),
    ))
    .unwrap();
    map.process("APC MINI", &cc(7, 64), ts(1)).unwrap();
    assert_eq!(map.generation(&device), Some(1));

    assert_eq!(map.detach("APC MINI"), Ok(device.clone()));
    assert!(!map.is_attached(&device));
    assert_eq!(
        map.process("APC MINI", &cc(7, 64), ts(2)),
        Err(ControlMapError::UnknownPort("APC MINI".into()))
    );

    // The OS renamed the port; the stable key re-associates the bindings.
    let report = map.attach("APC MINI #2", &device).unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(report.previous_port, "APC MINI");
    assert!(report.port_changed);
    assert_eq!(report.bindings, 1);
    assert_eq!(map.profile(&device).unwrap().port_name, "APC MINI #2");
    assert_eq!(map.device_on_port("APC MINI #2"), Some(&device));
    // Pickup restarts after reconnect: the last value (0.504) is the target.
    let far = map.process("APC MINI #2", &cc(7, 0), ts(3)).unwrap();
    assert_eq!(far.outputs.len(), 0);
    let near = map.process("APC MINI #2", &cc(7, 65), ts(4)).unwrap();
    assert_eq!(near.outputs.len(), 1);

    // Another device cannot claim an attached port.
    map.add_profile(profile("akai:apc:02", DeviceTransport::Midi, "APC MINI"))
        .unwrap();
    assert_eq!(
        map.attach("APC MINI #2", &key("akai:apc:02")),
        Err(ControlMapError::PortInUse {
            port: "APC MINI #2".into(),
            device: device.clone(),
        })
    );
    // Remembered port names attach transports without stable keys.
    let remembered = map.attach_remembered("APC MINI").unwrap();
    assert_eq!(remembered.device, key("akai:apc:02"));
    assert_eq!(
        map.attach_remembered("nowhere"),
        Err(ControlMapError::UnknownPort("nowhere".into()))
    );
    assert_eq!(
        map.remove_profile(&device),
        Err(ControlMapError::DeviceHasBindings(device.clone()))
    );
}

#[test]
fn control_map_validation_and_bounds() {
    let mut map = midi_map();
    assert!(matches!(
        DeviceKey::new("has space"),
        Err(ControlMapError::InvalidKey(_))
    ));
    assert!(matches!(
        BindingId::new(""),
        Err(ControlMapError::InvalidKey(_))
    ));
    assert_eq!(Span::new(1.0, 1.0), Err(ControlMapError::InvalidSpan));
    assert_eq!(
        map.add_binding(binding(
            "x",
            "akai:apc:01",
            cc(7, 0).control,
            Act::Cut,
            absolute(false, 0.5, Pickup::Immediate),
        )),
        Err(ControlMapError::InvalidResponse)
    );
    assert_eq!(
        map.add_binding(binding(
            "x",
            "missing",
            cc(7, 0).control,
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::UnknownDevice(key("missing")))
    );
    assert_eq!(
        map.add_binding(binding(
            "x",
            "akai:apc:01",
            ControlId::MidiCc14 {
                channel: ch(0),
                controller: 40
            },
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::InvalidControl(ControlId::MidiCc14 {
            channel: ch(0),
            controller: 40
        }))
    );
    for index in 0..MAX_CONTROL_BINDINGS {
        map.add_binding(binding(
            &format!("b{index}"),
            "akai:apc:01",
            cc(7, 0).control,
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        ))
        .unwrap();
    }
    assert_eq!(
        map.add_binding(binding(
            "overflow",
            "akai:apc:01",
            cc(7, 0).control,
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::TooManyBindings {
            limit: MAX_CONTROL_BINDINGS
        })
    );
    assert_eq!(
        map.add_binding(binding(
            "b0",
            "akai:apc:01",
            cc(7, 0).control,
            Act::Cut,
            absolute(false, 0.0, Pickup::Immediate),
        )),
        Err(ControlMapError::DuplicateBinding(id("b0")))
    );
}

// --------------------------------------------------------------- feedback

fn midi(payload: &FeedbackPayload) -> &[u8] {
    match payload {
        FeedbackPayload::Midi(bytes) => bytes,
        FeedbackPayload::Osc(_) => panic!("expected MIDI"),
    }
}

fn feedback_router(device: &DeviceKey, surface: &DeviceKey) -> FeedbackRouter {
    let mut router = FeedbackRouter::default();
    router
        .add(FeedbackBinding {
            id: id("pgm-1"),
            device: device.clone(),
            source: FeedbackSource::Activator(ActivatorRule::Program(input(1))),
            target: FeedbackTarget::MidiNote {
                channel: ch(0),
                note: 0x20,
                on_velocity: 0x03,
                off_velocity: 0x00,
            },
        })
        .unwrap();
    router
        .add(FeedbackBinding {
            id: id("vol-motor"),
            device: device.clone(),
            source: FeedbackSource::Level {
                key: "input-1.volume".into(),
                range: Span::new(0.0, 100.0).unwrap(),
            },
            target: FeedbackTarget::MidiPitchBend { channel: ch(0) },
        })
        .unwrap();
    router
        .add(FeedbackBinding {
            id: id("vol-14"),
            device: device.clone(),
            source: FeedbackSource::Level {
                key: "input-1.volume".into(),
                range: Span::new(0.0, 100.0).unwrap(),
            },
            target: FeedbackTarget::MidiCc14 {
                channel: ch(1),
                controller: 7,
            },
        })
        .unwrap();
    router
        .add(FeedbackBinding {
            id: id("rec"),
            device: surface.clone(),
            source: FeedbackSource::Activator(ActivatorRule::Recording),
            target: FeedbackTarget::Osc {
                address: "/rec/led".into(),
                format: OscFeedbackFormat::Float,
            },
        })
        .unwrap();
    assert_eq!(
        router.add(FeedbackBinding {
            id: id("clash"),
            device: device.clone(),
            source: FeedbackSource::Activator(ActivatorRule::Streaming),
            target: FeedbackTarget::MidiCc {
                channel: ch(1),
                controller: 39,
                on_value: 127,
                off_value: 0,
            },
        }),
        Err(FeedbackError::TargetConflict(id("vol-14")))
    );

    router
}

#[test]
fn feedback_tally_leds_motor_faders_and_osc_bytes() {
    let device = key("akai:apc:01");
    let surface = key("touchosc:ipad");
    let mut router = feedback_router(&device, &surface);
    router.attach(&device);
    let mut tally = TallySnapshot {
        program: Some(input(1)),
        recording: true,
        ..TallySnapshot::default()
    };
    router.observe_tally(&tally).unwrap();
    assert_eq!(router.observe_level("input-1.volume", 50.0), Ok(2));
    // Surface not connected yet: only the MIDI device is due.
    assert_eq!(router.pending(), 3);
    let first = router.drain(usize::MAX);
    assert_eq!(
        first
            .iter()
            .map(|message| (message.binding.as_str(), midi(&message.payload)))
            .collect::<Vec<_>>(),
        vec![
            ("pgm-1", &[0x90, 0x20, 0x03][..]),
            ("vol-motor", &[0xE0, 0x00, 0x40][..]),
            ("vol-14", &[0xB1, 0x07, 0x40, 0xB1, 0x27, 0x00][..]),
        ]
    );

    // Coalescing: off then on again before a drain sends nothing new; a
    // net change sends only the latest value.
    tally.program = None;
    router.observe_tally(&tally).unwrap();
    tally.program = Some(input(1));
    router.observe_tally(&tally).unwrap();
    router.observe_level("input-1.volume", 10.0).unwrap();
    router.observe_level("input-1.volume", 100.0).unwrap();
    let second = router.drain(1);
    assert_eq!(second.len(), 1);
    assert_eq!(midi(&second[0].payload), [0xE0, 0x7F, 0x7F]);
    assert_eq!(router.pending(), 1);
    assert_eq!(
        midi(&router.drain(8)[0].payload),
        [0xB1, 0x07, 0x7F, 0xB1, 0x27, 0x7F]
    );
    assert_eq!(router.pending(), 0);

    // Attaching the OSC surface sends its pending state as exact OSC bytes.
    router.attach(&surface);
    let osc = router.drain(8);
    assert_eq!(osc.len(), 1);
    assert_eq!(
        osc[0].payload,
        FeedbackPayload::Osc([&b"/rec/led\0\0\0\0,f\0\0"[..], &1.0_f32.to_be_bytes()].concat())
    );

    // Reconnect forces a full resend for that device only.
    router.detach(&device);
    tally.program = None;
    router.observe_tally(&tally).unwrap();
    assert_eq!(router.pending(), 0);
    router.attach(&device);
    let resent = router.drain(8);
    assert_eq!(
        resent
            .iter()
            .map(|message| message.binding.as_str())
            .collect::<Vec<_>>(),
        vec!["pgm-1", "vol-motor", "vol-14"]
    );
    assert_eq!(midi(&resent[0].payload), [0x90, 0x20, 0x00]);

    assert_eq!(
        router.observe_level("input-1.volume", f64::INFINITY),
        Err(FeedbackError::NonFiniteValue)
    );
    assert_eq!(
        router.add(FeedbackBinding {
            id: id("bad"),
            device,
            source: FeedbackSource::Activator(ActivatorRule::Streaming),
            target: FeedbackTarget::Osc {
                address: "/bad/*".into(),
                format: OscFeedbackFormat::Bool,
            },
        }),
        Err(FeedbackError::InvalidTarget)
    );
}
