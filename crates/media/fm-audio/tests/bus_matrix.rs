#![allow(
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::too_many_lines
)]

use std::num::NonZeroU128;

use fm_audio::{
    BUS_COUNT, BusDestination, BusMatrix, BusMatrixError, BusSend, BusStripInput, ChannelMeter,
    DestinationId, Gain, MAX_BUS_CHANNELS, MAX_BUS_DESTINATIONS, MAX_BUS_INPUT_MAGNITUDE,
    MAX_BUS_NAME_BYTES, MAX_BUS_STRIPS, MAX_SAMPLES_PER_BLOCK, MixBus, SendTap, StripDsp,
};
use fm_types::{InputId, SampleRate};

const RATE: u32 = 48_000;

fn rate() -> SampleRate {
    SampleRate::new(RATE).unwrap()
}

fn strip(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn gain(linear: f32) -> Gain {
    Gain::from_linear(linear).unwrap()
}

fn send(level: f32, tap: SendTap) -> BusSend {
    BusSend {
        enabled: true,
        level: gain(level),
        tap,
    }
}

/// Caller-owned output storage for one render.
struct Outputs {
    buses: Vec<Vec<f32>>,
    destinations: Vec<Vec<f32>>,
}

impl Outputs {
    fn new(matrix: &BusMatrix, samples: usize, fill: f32) -> Self {
        let channels = matrix.channels();
        Self {
            buses: vec![vec![fill; samples]; BUS_COUNT * channels],
            destinations: vec![vec![fill; samples]; matrix.destinations().count() * channels],
        }
    }

    fn bus(&self, matrix: &BusMatrix, bus: MixBus, channel: usize) -> &[f32] {
        &self.buses[bus.index() * matrix.channels() + channel]
    }

    fn destination(&self, matrix: &BusMatrix, index: usize, channel: usize) -> &[f32] {
        &self.destinations[index * matrix.channels() + channel]
    }
}

fn render(
    matrix: &mut BusMatrix,
    inputs: &[(InputId, &[Vec<f32>])],
    outputs: &mut Outputs,
) -> Result<fm_audio::BusMatrixTelemetry, BusMatrixError> {
    let plane_refs: Vec<Vec<&[f32]>> = inputs
        .iter()
        .map(|(_, planes)| planes.iter().map(Vec::as_slice).collect())
        .collect();
    let strip_inputs: Vec<BusStripInput<'_>> = inputs
        .iter()
        .zip(&plane_refs)
        .map(|((id, _), planes)| BusStripInput { strip: *id, planes })
        .collect();
    let mut bus_refs: Vec<&mut [f32]> = outputs.buses.iter_mut().map(Vec::as_mut_slice).collect();
    let mut destination_refs: Vec<&mut [f32]> = outputs
        .destinations
        .iter_mut()
        .map(Vec::as_mut_slice)
        .collect();
    matrix.render(&strip_inputs, &mut bus_refs, &mut destination_refs)
}

#[test]
fn defaults_route_strips_to_master_only() {
    let mut matrix = BusMatrix::new(rate(), 2).unwrap();
    let names: Vec<&str> = MixBus::ALL
        .iter()
        .map(|bus| matrix.bus_name(*bus))
        .collect();
    assert_eq!(names, ["Master", "A", "B", "C", "D", "E", "F", "G"]);
    assert_eq!(MixBus::G.index(), 7);
    assert_eq!(MixBus::from_index(1), Some(MixBus::A));
    assert_eq!(MixBus::from_index(BUS_COUNT), None);

    matrix.add_strip(strip(1)).unwrap();
    assert_eq!(matrix.send(strip(1), MixBus::Master), Some(BusSend::UNITY));
    for bus in &MixBus::ALL[1..] {
        assert_eq!(matrix.send(strip(1), *bus), Some(BusSend::DISABLED));
    }

    let planes = vec![vec![0.5, -0.25, 0.75], vec![-1.0, 0.0, 0.125]];
    let mut outputs = Outputs::new(&matrix, 3, 9.0);
    let telemetry = render(&mut matrix, &[(strip(1), &planes)], &mut outputs).unwrap();
    assert_eq!(telemetry.samples, 3);
    assert_eq!(telemetry.strips_present, 1);
    assert_eq!(outputs.bus(&matrix, MixBus::Master, 0), [0.5, -0.25, 0.75]);
    assert_eq!(outputs.bus(&matrix, MixBus::Master, 1), [-1.0, 0.0, 0.125]);
    for bus in &MixBus::ALL[1..] {
        for channel in 0..2 {
            assert_eq!(outputs.bus(&matrix, *bus, channel), [0.0; 3]);
        }
        assert_eq!(matrix.bus_meters(*bus), [ChannelMeter::default(); 2]);
    }
}

#[test]
fn independent_mixes_with_pre_and_post_fader_sends_and_bus_controls() {
    let mut matrix = BusMatrix::new(rate(), 2).unwrap();
    let (s1, s2) = (strip(1), strip(2));
    matrix.add_strip(s1).unwrap();
    matrix.add_strip(s2).unwrap();
    matrix.set_strip_fader(s1, gain(0.5)).unwrap();
    // A: s1 pre-fader at 0.25 only.
    matrix
        .set_send(s1, MixBus::A, send(0.25, SendTap::PreFader))
        .unwrap();
    // B: s2 post-fader at 0.5, bus fader 0.5.
    matrix
        .set_send(s2, MixBus::B, send(0.5, SendTap::PostFader))
        .unwrap();
    matrix.set_bus_fader(MixBus::B, gain(0.5));
    // C: s1 post-fader at unity but the bus is muted.
    matrix.set_send(s1, MixBus::C, BusSend::UNITY).unwrap();
    matrix.set_bus_mute(MixBus::C, true);
    // D: both strips post-fader; s1 fader applies, s2 at 2x.
    matrix.set_send(s1, MixBus::D, BusSend::UNITY).unwrap();
    matrix
        .set_send(s2, MixBus::D, send(2.0, SendTap::PostFader))
        .unwrap();

    let p1 = vec![vec![0.5, 0.25, -0.5, 1.0], vec![0.25; 4]];
    let p2 = vec![vec![0.125; 4], vec![-0.25, 0.5, 0.0, 0.75]];
    let mut outputs = Outputs::new(&matrix, 4, 0.0);
    render(&mut matrix, &[(s1, &p1), (s2, &p2)], &mut outputs).unwrap();

    // Master = s1 * 0.5 + s2.
    assert_eq!(
        outputs.bus(&matrix, MixBus::Master, 0),
        [0.375, 0.25, -0.125, 0.625]
    );
    assert_eq!(
        outputs.bus(&matrix, MixBus::Master, 1),
        [-0.125, 0.625, 0.125, 0.875]
    );
    // A = s1 * 0.25 (pre-fader ignores the 0.5 strip fader).
    assert_eq!(
        outputs.bus(&matrix, MixBus::A, 0),
        [0.125, 0.0625, -0.125, 0.25]
    );
    assert_eq!(outputs.bus(&matrix, MixBus::A, 1), [0.0625; 4]);
    // B = s2 * 0.5 * 0.5.
    assert_eq!(outputs.bus(&matrix, MixBus::B, 0), [0.03125; 4]);
    assert_eq!(
        outputs.bus(&matrix, MixBus::B, 1),
        [-0.0625, 0.125, 0.0, 0.1875]
    );
    // C muted.
    assert_eq!(outputs.bus(&matrix, MixBus::C, 0), [0.0; 4]);
    assert_eq!(outputs.bus(&matrix, MixBus::C, 1), [0.0; 4]);
    // D = s1 * 0.5 + s2 * 2.
    assert_eq!(outputs.bus(&matrix, MixBus::D, 0), [0.5, 0.375, 0.0, 0.75]);
    assert_eq!(
        outputs.bus(&matrix, MixBus::D, 1),
        [-0.375, 1.125, 0.125, 1.625]
    );
    for bus in [MixBus::E, MixBus::F, MixBus::G] {
        assert_eq!(outputs.bus(&matrix, bus, 0), [0.0; 4]);
    }

    // Meters: B right = [-0.0625, 0.125, 0, 0.1875].
    let meters = matrix.bus_meters(MixBus::B);
    assert_eq!(meters[0].peak, 0.03125);
    assert_eq!(meters[0].rms, 0.03125);
    assert_eq!(meters[1].peak, 0.1875);
    let expected_rms =
        ((0.0625_f64.powi(2) + 0.125_f64.powi(2) + 0.1875_f64.powi(2)) / 4.0).sqrt() as f32;
    assert_eq!(meters[1].rms, expected_rms);
    assert_eq!(matrix.bus_meters(MixBus::C), [ChannelMeter::default(); 2]);

    // Strip mute silences pre-fader sends too.
    matrix.set_strip_mute(s1, true).unwrap();
    render(&mut matrix, &[(s1, &p1), (s2, &p2)], &mut outputs).unwrap();
    assert_eq!(outputs.bus(&matrix, MixBus::A, 0), [0.0; 4]);
    assert_eq!(outputs.bus(&matrix, MixBus::Master, 0), [0.125; 4]);
    // Disabling a send removes only that send.
    matrix.set_strip_mute(s1, false).unwrap();
    matrix
        .set_send(
            s1,
            MixBus::A,
            BusSend {
                enabled: false,
                ..send(0.25, SendTap::PreFader)
            },
        )
        .unwrap();
    render(&mut matrix, &[(s1, &p1), (s2, &p2)], &mut outputs).unwrap();
    assert_eq!(outputs.bus(&matrix, MixBus::A, 0), [0.0; 4]);
    assert_eq!(
        outputs.bus(&matrix, MixBus::Master, 0),
        [0.375, 0.25, -0.125, 0.625]
    );
    // A configured strip without input contributes silence.
    let telemetry = render(&mut matrix, &[(s2, &p2)], &mut outputs).unwrap();
    assert_eq!(telemetry.strips_present, 1);
    assert_eq!(outputs.bus(&matrix, MixBus::Master, 0), [0.125; 4]);
}

#[test]
fn mix_minus_is_exact_where_subtraction_would_not_be() {
    let mut matrix = BusMatrix::new(rate(), 1).unwrap();
    let (s1, s2, s3) = (strip(1), strip(2), strip(3));
    for id in [s1, s2, s3] {
        matrix.add_strip(id).unwrap();
    }
    matrix
        .add_destination(BusDestination {
            id: DestinationId::Network(7),
            bus: MixBus::Master,
            mix_minus: Some(s1),
        })
        .unwrap();
    matrix
        .add_destination(BusDestination {
            id: DestinationId::Record(7),
            bus: MixBus::Master,
            mix_minus: None,
        })
        .unwrap();
    let p1 = vec![vec![1.0, 1.0]];
    let p2 = vec![vec![1.0e-8, 0.5]];
    let p3 = vec![vec![0.0, 0.25]];
    let mut outputs = Outputs::new(&matrix, 2, 0.0);
    let telemetry = render(
        &mut matrix,
        &[(s3, &p3), (s1, &p1), (s2, &p2)],
        &mut outputs,
    )
    .unwrap();
    assert_eq!(telemetry.destinations_rendered, 2);
    assert_eq!(telemetry.mix_minus_rendered, 1);

    let master = outputs.bus(&matrix, MixBus::Master, 0);
    assert_eq!(master, [1.0, 1.75]);
    // Subtracting s1 from the bus would lose the 1e-8 contribution.
    assert_eq!(master[0] - 1.0, 0.0);
    assert_eq!(outputs.destination(&matrix, 0, 0), [1.0e-8, 0.75]);
    // Plain destination is a copy of the bus.
    assert_eq!(outputs.destination(&matrix, 1, 0), master);
}

/// Deterministic pseudo-random planes without any clock or RNG dependency.
fn signal(seed: u32, channels: usize, samples: usize) -> Vec<Vec<f32>> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..channels)
        .map(|_| {
            (0..samples)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    #[allow(clippy::cast_precision_loss)]
                    let unit = (state >> 8) as f32 / (1_u32 << 24) as f32;
                    unit * 1.6 - 0.8
                })
                .collect()
        })
        .collect()
}

fn processed_matrix(exclude_send: bool) -> BusMatrix {
    let mut matrix = BusMatrix::new(rate(), 2).unwrap();
    for (value, fader, level, tap) in [
        (1, 0.75, 0.5, SendTap::PostFader),
        (2, 0.5, 1.0, SendTap::PreFader),
        (3, 1.0, 0.25, SendTap::PostFader),
    ] {
        matrix.add_strip(strip(value)).unwrap();
        matrix.set_strip_fader(strip(value), gain(fader)).unwrap();
        matrix
            .set_send(strip(value), MixBus::E, send(level, tap))
            .unwrap();
    }
    if exclude_send {
        matrix
            .set_send(strip(2), MixBus::E, BusSend::DISABLED)
            .unwrap();
    }
    let mut dsp = StripDsp::new(48_000.0, 2).unwrap();
    dsp.set_high_pass(80.0, 0.707).unwrap();
    dsp.set_compressor(-18.0, 4.0, 5.0, 50.0, 6.0, 3.0).unwrap();
    dsp.set_limiter(-1.0, 50.0).unwrap();
    matrix.set_bus_processing(MixBus::E, Some(dsp)).unwrap();
    matrix.set_bus_fader(MixBus::E, gain(0.5));
    matrix
}

#[test]
fn mix_minus_through_bus_processing_matches_bus_without_the_strip_bit_for_bit() {
    let mut matrix = processed_matrix(false);
    let mut reference = processed_matrix(true);
    matrix
        .add_destination(BusDestination {
            id: DestinationId::Device(1),
            bus: MixBus::E,
            mix_minus: Some(strip(2)),
        })
        .unwrap();
    let samples = 256;
    let mut outputs = Outputs::new(&matrix, samples, 0.0);
    let mut reference_outputs = Outputs::new(&reference, samples, 0.0);
    for block in 0..6 {
        let p1 = signal(block * 3 + 1, 2, samples);
        let p2 = signal(block * 3 + 2, 2, samples);
        let p3 = signal(block * 3 + 3, 2, samples);
        let inputs = [
            (strip(1), &p1[..]),
            (strip(2), &p2[..]),
            (strip(3), &p3[..]),
        ];
        let inputs: Vec<(InputId, &[Vec<f32>])> = inputs.to_vec();
        render(&mut matrix, &inputs, &mut outputs).unwrap();
        render(&mut reference, &inputs, &mut reference_outputs).unwrap();
        for channel in 0..2 {
            let feed = outputs.destination(&matrix, 0, channel);
            let expected = reference_outputs.bus(&reference, MixBus::E, channel);
            assert!(
                feed.iter()
                    .zip(expected)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            // And it genuinely differs from the full processed bus.
            assert_ne!(feed, outputs.bus(&matrix, MixBus::E, channel));
        }
    }
}

#[test]
fn render_validates_everything_before_writing() {
    let mut matrix = processed_matrix(false);
    matrix
        .add_destination(BusDestination {
            id: DestinationId::Record(3),
            bus: MixBus::E,
            mix_minus: Some(strip(1)),
        })
        .unwrap();
    let samples = 64;
    let p1 = signal(11, 2, samples);
    let p2 = signal(12, 2, samples);
    let r1: Vec<&[f32]> = p1.iter().map(Vec::as_slice).collect();
    let r2: Vec<&[f32]> = p2.iter().map(Vec::as_slice).collect();
    let inputs = [
        BusStripInput {
            strip: strip(1),
            planes: &r1,
        },
        BusStripInput {
            strip: strip(2),
            planes: &r2,
        },
    ];
    let mut buses = vec![vec![0.0_f32; samples]; BUS_COUNT * 2];
    let mut destinations = vec![vec![0.0_f32; samples]; 2];
    let mut bus_refs: Vec<&mut [f32]> = buses.iter_mut().map(Vec::as_mut_slice).collect();
    let mut destination_refs: Vec<&mut [f32]> =
        destinations.iter_mut().map(Vec::as_mut_slice).collect();

    // Steady-state renders reuse the caller's buffers.
    for _ in 0..4 {
        matrix
            .render(&inputs, &mut bus_refs, &mut destination_refs)
            .unwrap();
    }

    // A rejected render leaves outputs, meters, and processing untouched.
    let mut twin = matrix.clone();
    let meters = matrix.bus_meters(MixBus::E).to_vec();
    for plane in bus_refs.iter_mut().chain(destination_refs.iter_mut()) {
        plane.fill(7.0);
    }
    let mut bad = p2.clone();
    bad[1][5] = f32::NAN;
    let rb: Vec<&[f32]> = bad.iter().map(Vec::as_slice).collect();
    let bad_inputs = [
        inputs[0],
        BusStripInput {
            strip: strip(2),
            planes: &rb,
        },
    ];
    assert_eq!(
        matrix.render(&bad_inputs, &mut bus_refs, &mut destination_refs),
        Err(BusMatrixError::SampleOutOfRange {
            strip: strip(2),
            channel: 1,
            sample: 5,
        })
    );
    let mut loud = p2.clone();
    loud[0][0] = MAX_BUS_INPUT_MAGNITUDE * 2.0;
    let rl: Vec<&[f32]> = loud.iter().map(Vec::as_slice).collect();
    assert_eq!(
        matrix.render(
            &[BusStripInput {
                strip: strip(2),
                planes: &rl,
            }],
            &mut bus_refs,
            &mut destination_refs
        ),
        Err(BusMatrixError::SampleOutOfRange {
            strip: strip(2),
            channel: 0,
            sample: 0,
        })
    );
    assert_eq!(
        matrix.render(
            &[inputs[0], inputs[0]],
            &mut bus_refs,
            &mut destination_refs
        ),
        Err(BusMatrixError::DuplicateStripInput(strip(1)))
    );
    assert_eq!(
        matrix.render(
            &[BusStripInput {
                strip: strip(99),
                planes: &r1,
            }],
            &mut bus_refs,
            &mut destination_refs
        ),
        Err(BusMatrixError::UnknownStrip(strip(99)))
    );
    assert_eq!(
        matrix.render(
            &[BusStripInput {
                strip: strip(1),
                planes: &r1[..1],
            }],
            &mut bus_refs,
            &mut destination_refs
        ),
        Err(BusMatrixError::PlaneCountMismatch {
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        matrix.render(&inputs, &mut bus_refs, &mut destination_refs[..1]),
        Err(BusMatrixError::PlaneCountMismatch {
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        matrix.render(&inputs, &mut bus_refs[..15], &mut destination_refs),
        Err(BusMatrixError::PlaneCountMismatch {
            expected: 16,
            actual: 15,
        })
    );
    let short = &r1[0][..samples - 1];
    let shorts = [short, short];
    assert_eq!(
        matrix.render(
            &[BusStripInput {
                strip: strip(1),
                planes: &shorts,
            }],
            &mut bus_refs,
            &mut destination_refs
        ),
        Err(BusMatrixError::PlaneLengthMismatch {
            expected: samples,
            actual: samples - 1,
        })
    );
    assert!(
        bus_refs
            .iter()
            .chain(destination_refs.iter())
            .all(|plane| plane.iter().all(|value| *value == 7.0))
    );
    assert_eq!(matrix.bus_meters(MixBus::E), meters);

    // Processing state did not advance: both continue identically.
    let mut twin_buses = vec![vec![0.0_f32; samples]; BUS_COUNT * 2];
    let mut twin_destinations = vec![vec![0.0_f32; samples]; 2];
    let mut twin_bus_refs: Vec<&mut [f32]> = twin_buses.iter_mut().map(Vec::as_mut_slice).collect();
    let mut twin_destination_refs: Vec<&mut [f32]> = twin_destinations
        .iter_mut()
        .map(Vec::as_mut_slice)
        .collect();
    matrix
        .render(&inputs, &mut bus_refs, &mut destination_refs)
        .unwrap();
    twin.render(&inputs, &mut twin_bus_refs, &mut twin_destination_refs)
        .unwrap();
    assert_eq!(bus_refs, twin_bus_refs);
    assert_eq!(destination_refs, twin_destination_refs);

    let mut empty: Vec<&mut [f32]> = (0..BUS_COUNT * 2).map(|_| &mut [][..]).collect();
    assert_eq!(
        matrix.render(&[], &mut empty, &mut []),
        Err(BusMatrixError::BlockSamplesOutOfRange {
            actual: 0,
            maximum: MAX_SAMPLES_PER_BLOCK,
        })
    );
}

#[test]
fn control_plane_is_bounded_and_atomic() {
    assert_eq!(
        BusMatrix::new(rate(), 0).unwrap_err(),
        BusMatrixError::ChannelCountOutOfRange {
            actual: 0,
            maximum: MAX_BUS_CHANNELS,
        }
    );
    assert!(BusMatrix::new(rate(), MAX_BUS_CHANNELS + 1).is_err());

    let mut matrix = BusMatrix::new(rate(), 2).unwrap();
    for value in 1..=MAX_BUS_STRIPS as u128 {
        matrix.add_strip(strip(value)).unwrap();
    }
    assert_eq!(
        matrix.add_strip(strip(1)),
        Err(BusMatrixError::DuplicateStrip(strip(1)))
    );
    assert_eq!(
        matrix.add_strip(strip(1_000)),
        Err(BusMatrixError::StripLimit {
            maximum: MAX_BUS_STRIPS,
        })
    );
    assert_eq!(
        matrix.set_send(strip(1_000), MixBus::A, BusSend::UNITY),
        Err(BusMatrixError::UnknownStrip(strip(1_000)))
    );

    // Destinations: typed IDs are unique per kind and number.
    let destination = |id, mix_minus| BusDestination {
        id,
        bus: MixBus::B,
        mix_minus,
    };
    matrix
        .add_destination(destination(DestinationId::Device(1), Some(strip(5))))
        .unwrap();
    matrix
        .add_destination(destination(DestinationId::Network(1), None))
        .unwrap();
    matrix
        .add_destination(destination(DestinationId::Record(1), None))
        .unwrap();
    assert_eq!(
        matrix.add_destination(destination(DestinationId::Device(1), None)),
        Err(BusMatrixError::DuplicateDestination(DestinationId::Device(
            1
        )))
    );
    assert_eq!(
        matrix.add_destination(destination(DestinationId::Device(2), Some(strip(1_000)))),
        Err(BusMatrixError::UnknownStrip(strip(1_000)))
    );
    for number in 2..=(MAX_BUS_DESTINATIONS as u32 - 2) {
        matrix
            .add_destination(destination(DestinationId::Device(number), None))
            .unwrap();
    }
    assert_eq!(matrix.destinations().count(), MAX_BUS_DESTINATIONS);
    assert_eq!(
        matrix.add_destination(destination(DestinationId::Device(999), None)),
        Err(BusMatrixError::DestinationLimit {
            maximum: MAX_BUS_DESTINATIONS,
        })
    );
    assert_eq!(
        matrix.remove_strip(strip(5)),
        Err(BusMatrixError::StripInUse {
            strip: strip(5),
            destination: DestinationId::Device(1),
        })
    );
    assert_eq!(matrix.strips().count(), MAX_BUS_STRIPS);
    assert_eq!(
        matrix.remove_destination(DestinationId::Device(1)),
        Ok(destination(DestinationId::Device(1), Some(strip(5))))
    );
    assert_eq!(
        matrix.remove_destination(DestinationId::Device(1)),
        Err(BusMatrixError::UnknownDestination(DestinationId::Device(1)))
    );
    matrix.remove_strip(strip(5)).unwrap();
    assert_eq!(matrix.strips().count(), MAX_BUS_STRIPS - 1);

    // Named routing.
    matrix.set_bus_name(MixBus::A, "Stream Return").unwrap();
    assert_eq!(matrix.bus_by_name("stream return"), Some(MixBus::A));
    assert_eq!(matrix.bus_by_name("A"), None);
    assert_eq!(
        matrix.set_bus_name(MixBus::B, "STREAM RETURN"),
        Err(BusMatrixError::DuplicateBusName {
            existing: MixBus::A,
        })
    );
    matrix.set_bus_name(MixBus::A, "stream return").unwrap();
    assert_eq!(
        matrix.set_bus_name(MixBus::B, "  "),
        Err(BusMatrixError::EmptyBusName)
    );
    assert_eq!(
        matrix.set_bus_name(MixBus::B, "bad\nname"),
        Err(BusMatrixError::BusNameControlCharacter)
    );
    let long = "x".repeat(MAX_BUS_NAME_BYTES + 1);
    assert_eq!(
        matrix.set_bus_name(MixBus::B, &long),
        Err(BusMatrixError::BusNameTooLong {
            bytes: MAX_BUS_NAME_BYTES + 1,
            maximum: MAX_BUS_NAME_BYTES,
        })
    );
    assert_eq!(matrix.bus_name(MixBus::B), "B");
    assert_eq!(matrix.bus_name(MixBus::A), "stream return");

    // Processing must match the matrix.
    assert_eq!(
        matrix.set_bus_processing(MixBus::C, Some(StripDsp::new(48_000.0, 1).unwrap())),
        Err(BusMatrixError::ProcessingChannelMismatch {
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        matrix.set_bus_processing(MixBus::C, Some(StripDsp::new(44_100.0, 2).unwrap())),
        Err(BusMatrixError::ProcessingSampleRateMismatch {
            expected_hz: RATE,
            actual_hz: 44_100.0,
        })
    );
    assert!(matrix.bus_processing(MixBus::C).is_none());
    matrix
        .set_bus_processing(MixBus::C, Some(StripDsp::new(48_000.0, 2).unwrap()))
        .unwrap();
    assert!(matrix.bus_processing(MixBus::C).is_some());
    matrix.set_bus_processing(MixBus::C, None).unwrap();
    assert!(matrix.bus_processing(MixBus::C).is_none());

    matrix.set_bus_mute(MixBus::D, true);
    matrix.set_bus_fader(MixBus::D, gain(0.25));
    assert!(matrix.bus_muted(MixBus::D));
    assert_eq!(matrix.bus_fader(MixBus::D), gain(0.25));
    matrix.set_strip_mute(strip(1), true).unwrap();
    assert_eq!(matrix.strip_muted(strip(1)), Some(true));
    assert_eq!(matrix.strip_fader(strip(1)), Some(Gain::UNITY));
}
