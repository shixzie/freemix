#![allow(clippy::float_cmp, clippy::too_many_lines)]

use fm_audio::{
    DiscreteChannel, DownmixNormalization, DownmixOptions, EMBEDDED_CHANNELS, EmbeddedAudioMap,
    EmbeddedPair, EmbeddedSlot, GainMatrix, LfeHandling, MAX_GAIN_MATRIX_ROUTES,
    MAX_SAMPLES_PER_BLOCK, MINUS_3_DB, MatrixRoute, MultichannelError, MultichannelLayout,
    MultichannelSide, SpeakerLabel, StandardLayout, deembed_group, deembed_mono, deembed_pair,
};
use fm_types::{Channel, ChannelLayout};

const G: f32 = MINUS_3_DB;

fn route(input: usize, output: usize, gain: f32) -> MatrixRoute {
    MatrixRoute {
        input,
        output,
        gain,
    }
}

fn outputs(channels: usize, samples: usize) -> Vec<Vec<f32>> {
    vec![vec![9.0; samples]; channels]
}

fn bits(planes: &[Vec<f32>]) -> Vec<Vec<u32>> {
    planes
        .iter()
        .map(|plane| plane.iter().map(|value| value.to_bits()).collect())
        .collect()
}

fn labels(layout: StandardLayout) -> Vec<&'static str> {
    layout.layout().labels().iter().map(|l| l.label()).collect()
}

#[test]
fn catalog_has_exact_orders_labels_and_fm_types_interop() {
    assert_eq!(labels(StandardLayout::Mono), ["M"]);
    assert_eq!(labels(StandardLayout::Stereo), ["L", "R"]);
    assert_eq!(labels(StandardLayout::TwoPointOne), ["L", "R", "LFE"]);
    assert_eq!(
        labels(StandardLayout::FivePointOne),
        ["L", "R", "C", "LFE", "Ls", "Rs"]
    );
    assert_eq!(
        labels(StandardLayout::SevenPointOne),
        ["L", "R", "C", "LFE", "Ls", "Rs", "Lrs", "Rrs"]
    );
    let discrete = labels(StandardLayout::Discrete16);
    assert_eq!(discrete.len(), 16);
    assert_eq!(discrete[0], "Ch1");
    assert_eq!(discrete[15], "Ch16");

    let names: Vec<_> = StandardLayout::ALL.iter().map(|l| l.name()).collect();
    assert_eq!(names, ["1.0", "2.0", "2.1", "5.1", "7.1", "16.0"]);
    for layout in StandardLayout::ALL {
        assert_eq!(layout.layout().channel_count(), layout.channel_count());
        assert_eq!(layout.layout().standard(), Some(layout));
    }

    // Custom construction equals the catalogue entry with the same labels.
    let custom = MultichannelLayout::new(StandardLayout::FivePointOne.layout().labels()).unwrap();
    assert_eq!(custom, StandardLayout::FivePointOne.layout());
    assert_eq!(custom.standard(), Some(StandardLayout::FivePointOne));
    let reordered = MultichannelLayout::new(&[SpeakerLabel::Right, SpeakerLabel::Left]).unwrap();
    assert_eq!(reordered.standard(), None);
    assert_eq!(reordered.index_of(SpeakerLabel::Left), Some(1));

    // fm-types round trip preserves count and order.
    let five_one = ChannelLayout::new(vec![
        Channel::Left,
        Channel::Right,
        Channel::Center,
        Channel::LowFrequency,
        Channel::LeftSurround,
        Channel::RightSurround,
    ])
    .unwrap();
    let converted = MultichannelLayout::from_channel_layout(&five_one).unwrap();
    assert_eq!(converted.standard(), Some(StandardLayout::FivePointOne));
    assert_eq!(converted.to_channel_layout(), five_one);
    // Every catalogue layout, including 7.1 rear surrounds and 16 discrete
    // channels, round-trips through fm-types without loss.
    for standard in StandardLayout::ALL {
        let layout = standard.layout();
        let channels = layout.to_channel_layout();
        assert_eq!(channels.channels().len(), standard.channel_count());
        assert_eq!(
            MultichannelLayout::from_channel_layout(&channels).unwrap(),
            layout
        );
    }
    assert_eq!(
        &StandardLayout::SevenPointOne
            .layout()
            .to_channel_layout()
            .channels()[6..],
        &[Channel::LeftRearSurround, Channel::RightRearSurround]
    );
    let ch16 = DiscreteChannel::new(16).unwrap();
    assert_eq!(
        StandardLayout::Discrete16
            .layout()
            .to_channel_layout()
            .channels()[15],
        Channel::Discrete(ch16)
    );
    assert_eq!(
        SpeakerLabel::from_channel(Channel::Discrete(ch16)),
        SpeakerLabel::Discrete(ch16)
    );
    assert_eq!(
        SpeakerLabel::from_channel(Channel::RightRearSurround).to_channel(),
        Channel::RightRearSurround
    );

    // Validation.
    assert_eq!(
        MultichannelLayout::new(&[]),
        Err(MultichannelError::ChannelCountOutOfRange {
            actual: 0,
            maximum: 16
        })
    );
    assert_eq!(
        MultichannelLayout::new(&[SpeakerLabel::Left, SpeakerLabel::Left]),
        Err(MultichannelError::DuplicateLabel(SpeakerLabel::Left))
    );
    let seventeen = [SpeakerLabel::Mono; 17];
    assert_eq!(
        MultichannelLayout::new(&seventeen),
        Err(MultichannelError::ChannelCountOutOfRange {
            actual: 17,
            maximum: 16
        })
    );
    assert_eq!(DiscreteChannel::new(0), None);
    assert_eq!(DiscreteChannel::new(17), None);
    assert_eq!(DiscreteChannel::new(16).unwrap().number(), 16);
}

#[test]
fn identity_and_unity_routes_are_bit_exact() {
    let layout = StandardLayout::FivePointOne.layout();
    let matrix = GainMatrix::identity(layout);
    let input: Vec<Vec<f32>> = vec![
        vec![-0.0, 1.0e-40, 1024.0],
        vec![0.1, -0.2, 0.3],
        vec![f32::MIN_POSITIVE, -1024.0, 0.0],
        vec![0.333_333_34, 0.0, -0.0],
        vec![1.0, -1.0, 0.5],
        vec![0.7, 0.8, 0.9],
    ];
    let mut output = outputs(6, 3);
    assert_eq!(matrix.process(&input, &mut output), Ok(3));
    assert_eq!(bits(&output), bits(&input));

    // A reordering router with an unused (silent) output.
    let swap = GainMatrix::new(
        StandardLayout::Stereo.layout(),
        StandardLayout::TwoPointOne.layout(),
        &[route(1, 0, 1.0), route(0, 1, 1.0), route(0, 2, 0.0)],
    )
    .unwrap();
    assert_eq!(swap.routes().len(), 2, "zero-gain routes are dropped");
    assert_eq!(swap.gain(0, 1), 1.0);
    assert_eq!(swap.gain(0, 2), 0.0);
    let input = vec![vec![-0.0, 0.25], vec![0.5, -0.75]];
    let mut output = outputs(3, 2);
    swap.process(&input, &mut output).unwrap();
    assert_eq!(output[0], [0.5, -0.75]);
    assert_eq!(output[1][0].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(output[1][1], 0.25);
    assert_eq!(bits(&output[2..]), [[0_u32, 0]]);

    // Label matching reorders 5.1 into a custom order and silences gaps.
    let custom = MultichannelLayout::new(&[
        SpeakerLabel::Center,
        SpeakerLabel::LeftRearSurround,
        SpeakerLabel::Left,
    ])
    .unwrap();
    let matching = GainMatrix::matching(layout, custom);
    assert_eq!(
        matching.routes(),
        [route(2, 0, 1.0), route(0, 2, 1.0)].as_slice()
    );
}

#[test]
fn sparse_matrix_accumulates_in_deterministic_order() {
    let matrix = GainMatrix::new(
        StandardLayout::TwoPointOne.layout(),
        StandardLayout::Stereo.layout(),
        // Submitted out of order; stored as (output, input).
        &[
            route(2, 1, -0.5),
            route(1, 1, 2.0),
            route(0, 0, 0.25),
            route(2, 0, 0.5),
        ],
    )
    .unwrap();
    assert_eq!(
        matrix.routes(),
        [
            route(0, 0, 0.25),
            route(2, 0, 0.5),
            route(1, 1, 2.0),
            route(2, 1, -0.5)
        ]
        .as_slice()
    );
    let input = vec![vec![1.0, -4.0], vec![0.5, 3.0], vec![2.0, 8.0]];
    let mut output = outputs(2, 2);
    matrix.process(&input, &mut output).unwrap();
    assert_eq!(output[0], [1.25, 3.0]);
    assert_eq!(output[1], [0.0, 2.0]);
}

#[test]
fn matrix_validation_is_explicit_and_transactional() {
    let stereo = StandardLayout::Stereo.layout();
    let mono = StandardLayout::Mono.layout();
    assert_eq!(
        GainMatrix::new(stereo, mono, &[route(2, 0, 1.0)]),
        Err(MultichannelError::RouteChannelOutOfRange {
            route: 0,
            side: MultichannelSide::Input,
            channel: 2,
            channels: 2
        })
    );
    assert_eq!(
        GainMatrix::new(stereo, mono, &[route(0, 1, 1.0)]),
        Err(MultichannelError::RouteChannelOutOfRange {
            route: 0,
            side: MultichannelSide::Output,
            channel: 1,
            channels: 1
        })
    );
    assert_eq!(
        GainMatrix::new(stereo, mono, &[route(0, 0, 1.0), route(0, 0, 0.5)]),
        Err(MultichannelError::DuplicateRoute {
            input: 0,
            output: 0
        })
    );
    assert!(matches!(
        GainMatrix::new(stereo, mono, &[route(0, 0, f32::NAN)]),
        Err(MultichannelError::InvalidGain { route: 0, .. })
    ));
    assert_eq!(
        GainMatrix::new(stereo, mono, &[route(1, 0, 16.5)]),
        Err(MultichannelError::InvalidGain {
            route: 0,
            gain: 16.5
        })
    );
    assert!(GainMatrix::new(stereo, mono, &[route(1, 0, -16.0)]).is_ok());
    let many = vec![route(0, 0, 1.0); MAX_GAIN_MATRIX_ROUTES + 1];
    assert_eq!(
        GainMatrix::new(stereo, mono, &many),
        Err(MultichannelError::TooManyRoutes {
            actual: 257,
            maximum: 256
        })
    );

    let matrix = GainMatrix::downmix(
        StandardLayout::Stereo,
        StandardLayout::Mono,
        DownmixOptions::default(),
    )
    .unwrap();
    let mut output = outputs(1, 2);
    let pristine = output.clone();
    assert_eq!(
        matrix.process(&[vec![0.0; 2]], &mut output),
        Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Input,
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(
        matrix.process(&[vec![0.0; 2], vec![0.0; 3]], &mut output),
        Err(MultichannelError::PlaneLengthMismatch {
            side: MultichannelSide::Input,
            plane: 1,
            expected: 2,
            actual: 3
        })
    );
    assert_eq!(
        matrix.process(&[vec![0.0; 3], vec![0.0; 3]], &mut output),
        Err(MultichannelError::PlaneLengthMismatch {
            side: MultichannelSide::Output,
            plane: 0,
            expected: 3,
            actual: 2
        })
    );
    assert_eq!(
        matrix.process(&[vec![0.0; 2], vec![0.0; 2]], &mut outputs(2, 2)),
        Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Output,
            expected: 1,
            actual: 2
        })
    );
    assert_eq!(
        matrix.process(&[vec![0.0, 1.0], vec![1024.5, 0.0]], &mut output),
        Err(MultichannelError::InputOutOfRange {
            channel: 1,
            sample: 0
        })
    );
    assert_eq!(
        matrix.process(&[vec![0.0, f32::INFINITY], vec![0.0; 2]], &mut output),
        Err(MultichannelError::InputOutOfRange {
            channel: 0,
            sample: 1
        })
    );
    let too_long = vec![vec![0.0; MAX_SAMPLES_PER_BLOCK + 1]; 2];
    let mut long_output = outputs(1, MAX_SAMPLES_PER_BLOCK + 1);
    assert_eq!(
        matrix.process(&too_long, &mut long_output),
        Err(MultichannelError::SampleCountOutOfRange {
            actual: MAX_SAMPLES_PER_BLOCK + 1,
            maximum: MAX_SAMPLES_PER_BLOCK
        })
    );
    assert_eq!(output, pristine, "failed operations never write output");

    // Empty blocks are accepted.
    let mut empty = outputs(1, 0);
    assert_eq!(matrix.process(&[vec![], vec![]], &mut empty), Ok(0));
}

#[test]
fn five_one_to_stereo_uses_exact_bs775_coefficients() {
    let nominal = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions::default(),
    )
    .unwrap();
    assert_eq!(G, 0.707_106_77);
    assert_eq!(
        nominal.routes(),
        [
            route(0, 0, 1.0),
            route(2, 0, G),
            route(4, 0, G),
            route(1, 1, 1.0),
            route(2, 1, G),
            route(5, 1, G),
        ]
        .as_slice(),
        "LFE discarded by default"
    );

    let (l, r, c, lfe, ls, rs) = (0.5_f32, -0.25_f32, 0.75_f32, 1.0_f32, 0.125_f32, -0.5_f32);
    let input = vec![vec![l], vec![r], vec![c], vec![lfe], vec![ls], vec![rs]];
    let mut output = outputs(2, 1);
    nominal.process(&input, &mut output).unwrap();
    assert_eq!(output[0][0], l + c * G + ls * G);
    assert_eq!(output[1][0], r + c * G + rs * G);

    let with_lfe = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions {
            lfe: LfeHandling::MinusThreeDb,
            normalization: DownmixNormalization::None,
        },
    )
    .unwrap();
    assert_eq!(with_lfe.gain(3, 0), G);
    assert_eq!(with_lfe.gain(3, 1), G);
    with_lfe.process(&input, &mut output).unwrap();
    assert_eq!(output[0][0], l + c * G + lfe * G + ls * G);

    let unity_lfe = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions {
            lfe: LfeHandling::Unity,
            normalization: DownmixNormalization::None,
        },
    )
    .unwrap();
    assert_eq!(unity_lfe.gain(3, 1), 1.0);

    // Normalized: 1/(1+sqrt2) = sqrt2-1 and g/(1+sqrt2) = 1-g.
    let normalized = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions {
            lfe: LfeHandling::Discard,
            normalization: DownmixNormalization::PreventClipping,
        },
    )
    .unwrap();
    assert_eq!(normalized.gain(0, 0), 0.414_213_57);
    assert_eq!(normalized.gain(2, 0), 0.292_893_23);
    assert_eq!(normalized.gain(4, 0), 0.292_893_23);
    assert_eq!(normalized.gain(1, 1), 0.414_213_57);
    assert_eq!(normalized.gain(5, 1), 0.292_893_23);
    assert_eq!(normalized.gain(3, 0), 0.0);
    let full_scale = vec![vec![1.0]; 6];
    normalized.process(&full_scale, &mut output).unwrap();
    assert!((output[0][0] - 1.0).abs() <= f32::EPSILON);

    // Normalized with -3 dB LFE: sum = 1 + 3g.
    let normalized_lfe = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions {
            lfe: LfeHandling::MinusThreeDb,
            normalization: DownmixNormalization::PreventClipping,
        },
    )
    .unwrap();
    assert_eq!(normalized_lfe.gain(0, 0), 0.320_377_23);
    assert_eq!(normalized_lfe.gain(2, 0), 0.226_540_92);
    assert_eq!(normalized_lfe.gain(3, 0), 0.226_540_92);

    // Normalized with unity LFE: sum = 2 + 2g.
    let normalized_unity = GainMatrix::downmix(
        StandardLayout::FivePointOne,
        StandardLayout::Stereo,
        DownmixOptions {
            lfe: LfeHandling::Unity,
            normalization: DownmixNormalization::PreventClipping,
        },
    )
    .unwrap();
    assert_eq!(normalized_unity.gain(0, 0), 0.292_893_23);
    assert_eq!(normalized_unity.gain(3, 0), 0.292_893_23);
    assert_eq!(normalized_unity.gain(2, 0), 0.207_106_78);
}

#[test]
fn seven_one_to_five_one_and_stereo_to_mono_are_exact() {
    let nominal = GainMatrix::downmix(
        StandardLayout::SevenPointOne,
        StandardLayout::FivePointOne,
        DownmixOptions::default(),
    )
    .unwrap();
    assert_eq!(
        nominal.routes(),
        [
            route(0, 0, 1.0),
            route(1, 1, 1.0),
            route(2, 2, 1.0),
            route(3, 3, 1.0),
            route(4, 4, G),
            route(6, 4, G),
            route(5, 5, G),
            route(7, 5, G),
        ]
        .as_slice()
    );
    let normalized = GainMatrix::downmix(
        StandardLayout::SevenPointOne,
        StandardLayout::FivePointOne,
        DownmixOptions {
            lfe: LfeHandling::Unity,
            normalization: DownmixNormalization::PreventClipping,
        },
    )
    .unwrap();
    assert_eq!(normalized.gain(4, 4), 0.5);
    assert_eq!(normalized.gain(6, 4), 0.5);
    assert_eq!(normalized.gain(3, 3), 1.0, "LFE option does not apply");
    let input: Vec<Vec<f32>> = (0..8_u8)
        .map(|channel| vec![f32::from(channel) * 0.125 - 0.5, -0.0])
        .collect();
    let mut output = outputs(6, 2);
    normalized.process(&input, &mut output).unwrap();
    assert_eq!(
        bits(&output[..4]),
        bits(&input[..4]),
        "front and LFE pass exactly"
    );
    assert_eq!(output[4], [0.5 * 0.0 + 0.5 * 0.25, 0.0]);
    assert_eq!(output[5], [0.5 * 0.125 + 0.5 * 0.375, 0.0]);

    let mono = GainMatrix::downmix(
        StandardLayout::Stereo,
        StandardLayout::Mono,
        DownmixOptions {
            lfe: LfeHandling::Discard,
            normalization: DownmixNormalization::PreventClipping,
        },
    )
    .unwrap();
    assert_eq!(
        mono.routes(),
        [route(0, 0, 0.5), route(1, 0, 0.5)].as_slice()
    );
    let mut output = outputs(1, 3);
    mono.process(&[vec![0.25, 1.0, -1.0], vec![-0.75, 1.0, 1.0]], &mut output)
        .unwrap();
    assert_eq!(output[0], [-0.25, 1.0, 0.0]);

    assert_eq!(
        GainMatrix::downmix(
            StandardLayout::SevenPointOne,
            StandardLayout::Stereo,
            DownmixOptions::default()
        ),
        Err(MultichannelError::UnsupportedConversion {
            from: StandardLayout::SevenPointOne.layout(),
            to: StandardLayout::Stereo.layout()
        })
    );
}

#[test]
fn upmix_duplicates_by_documented_rules() {
    let pairs = |from, to| -> Vec<(usize, usize)> {
        GainMatrix::upmix_duplicate(from, to)
            .unwrap()
            .routes()
            .iter()
            .inspect(|route| assert_eq!(route.gain, 1.0))
            .map(|route| (route.input, route.output))
            .collect()
    };
    assert_eq!(
        pairs(StandardLayout::Mono, StandardLayout::Stereo),
        [(0, 0), (0, 1)]
    );
    assert_eq!(
        pairs(StandardLayout::Mono, StandardLayout::FivePointOne),
        [(0, 0), (0, 1), (0, 2), (0, 4), (0, 5)],
        "mono never feeds LFE"
    );
    assert_eq!(
        pairs(StandardLayout::Mono, StandardLayout::Discrete16),
        (0..16).map(|output| (0, output)).collect::<Vec<_>>()
    );
    assert_eq!(
        pairs(StandardLayout::Stereo, StandardLayout::TwoPointOne),
        [(0, 0), (1, 1)]
    );
    assert_eq!(
        pairs(StandardLayout::Stereo, StandardLayout::FivePointOne),
        [(0, 0), (1, 1), (0, 4), (1, 5)]
    );
    assert_eq!(
        pairs(StandardLayout::TwoPointOne, StandardLayout::FivePointOne),
        [(0, 0), (1, 1), (2, 3), (0, 4), (1, 5)]
    );
    assert_eq!(
        pairs(StandardLayout::FivePointOne, StandardLayout::SevenPointOne),
        [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 5),
            (4, 6),
            (5, 7)
        ]
    );
    assert_eq!(
        pairs(StandardLayout::Stereo, StandardLayout::SevenPointOne),
        [(0, 0), (1, 1), (0, 4), (1, 5), (0, 6), (1, 7)]
    );

    let matrix =
        GainMatrix::upmix_duplicate(StandardLayout::Stereo, StandardLayout::FivePointOne).unwrap();
    let input = vec![vec![0.1, -0.0], vec![-0.3, 0.7]];
    let mut output = outputs(6, 2);
    matrix.process(&input, &mut output).unwrap();
    assert_eq!(
        bits(&output),
        bits(&[
            input[0].clone(),
            input[1].clone(),
            vec![0.0; 2],
            vec![0.0; 2],
            input[0].clone(),
            input[1].clone()
        ])
    );

    for (from, to) in [
        (StandardLayout::FivePointOne, StandardLayout::Stereo),
        (StandardLayout::Stereo, StandardLayout::Stereo),
        (StandardLayout::Stereo, StandardLayout::Discrete16),
    ] {
        assert_eq!(
            GainMatrix::upmix_duplicate(from, to),
            Err(MultichannelError::UnsupportedConversion {
                from: from.layout(),
                to: to.layout()
            })
        );
    }
}

fn embedded_planes() -> Vec<Vec<f32>> {
    (0..EMBEDDED_CHANNELS)
        .map(|slot| {
            let base = f32::from(u8::try_from(slot).unwrap());
            vec![base + 0.5, -base, base * 0.25]
        })
        .collect()
}

#[test]
fn deembeds_mono_pairs_and_groups_from_sixteen_channels() {
    let embedded = embedded_planes();
    let slot = EmbeddedSlot::new(3, 2).unwrap();
    assert_eq!(slot.index(), 9);
    assert_eq!(EmbeddedSlot::from_index(9), Some(slot));
    assert_eq!(EmbeddedSlot::from_index(16), None);
    let mut mono = vec![0.0; 3];
    deembed_mono(&embedded, slot, &mut mono).unwrap();
    assert_eq!(mono, embedded[9]);

    let pair = EmbeddedPair::new(2, 2).unwrap();
    assert_eq!((pair.left().index(), pair.right().index()), (6, 7));
    let mut stereo = outputs(2, 3);
    deembed_pair(&embedded, pair, &mut stereo).unwrap();
    assert_eq!(stereo, embedded[6..8]);

    let mut group = outputs(4, 3);
    deembed_group(&embedded, 4, &mut group).unwrap();
    assert_eq!(group, embedded[12..16]);

    assert_eq!(
        EmbeddedSlot::new(0, 1),
        Err(MultichannelError::InvalidEmbeddedGroup(0))
    );
    assert_eq!(
        EmbeddedSlot::new(4, 5),
        Err(MultichannelError::InvalidEmbeddedChannel(5))
    );
    assert_eq!(
        EmbeddedPair::new(1, 3),
        Err(MultichannelError::InvalidEmbeddedPair(3))
    );
    assert_eq!(
        deembed_group(&embedded, 5, &mut group),
        Err(MultichannelError::InvalidEmbeddedGroup(5))
    );
    assert_eq!(
        deembed_pair(&embedded[..15], pair, &mut stereo),
        Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Input,
            expected: 16,
            actual: 15
        })
    );
    let mut short = vec![0.0; 2];
    assert_eq!(
        deembed_mono(&embedded, slot, &mut short),
        Err(MultichannelError::PlaneLengthMismatch {
            side: MultichannelSide::Output,
            plane: 0,
            expected: 3,
            actual: 2
        })
    );
}

#[test]
fn embedding_map_preserves_layouts_through_a_round_trip() {
    let stereo = StandardLayout::Stereo.layout();
    let five_one = StandardLayout::FivePointOne.layout();
    let mono = StandardLayout::Mono.layout();
    let mut map = EmbeddedAudioMap::new();
    assert_eq!(map.assign(stereo, EmbeddedSlot::new(1, 1).unwrap()), Ok(0));
    assert_eq!(
        map.assign(five_one, EmbeddedSlot::new(1, 3).unwrap()),
        Ok(1)
    );
    assert_eq!(map.assign(mono, EmbeddedSlot::new(3, 2).unwrap()), Ok(2));
    assert_eq!(map.assignment(1).unwrap().slots(), 2..8);
    assert_eq!(map.label_at(4), Some(SpeakerLabel::Center));
    assert_eq!(map.label_at(5), Some(SpeakerLabel::LowFrequencyEffects));
    assert_eq!(map.label_at(9), Some(SpeakerLabel::Mono));
    assert_eq!(map.label_at(8), None);

    assert_eq!(
        map.assign(stereo, EmbeddedSlot::new(3, 4).unwrap()),
        Err(MultichannelError::EmbeddedMisaligned { first_slot: 11 })
    );
    assert_eq!(
        map.assign(stereo, EmbeddedSlot::new(2, 3).unwrap()),
        Err(MultichannelError::EmbeddedOverlap { slot: 6 })
    );
    assert_eq!(
        map.assign(mono, EmbeddedSlot::new(3, 2).unwrap()),
        Err(MultichannelError::EmbeddedOverlap { slot: 9 })
    );
    assert_eq!(
        map.assign(
            StandardLayout::SevenPointOne.layout(),
            EmbeddedSlot::new(3, 3).unwrap()
        ),
        Err(MultichannelError::EmbeddedOverflow {
            first_slot: 10,
            channels: 8
        })
    );
    assert_eq!(map.assignments().count(), 3);

    let surround: Vec<Vec<f32>> = (0..6_u8)
        .map(|channel| vec![f32::from(channel) + 0.25, -0.0])
        .collect();
    let mut embedded = vec![vec![7.0_f32; 2]; EMBEDDED_CHANNELS];
    map.embed(1, &surround, &mut embedded).unwrap();
    assert_eq!(bits(&embedded[2..8]), bits(&surround));
    assert!(
        embedded[..2]
            .iter()
            .chain(&embedded[8..])
            .all(|p| p == &[7.0, 7.0])
    );

    let mut restored = outputs(6, 2);
    assert_eq!(map.deembed(1, &embedded, &mut restored), Ok(five_one));
    assert_eq!(bits(&restored), bits(&surround));

    map.silence_unassigned(&mut embedded).unwrap();
    assert_eq!(embedded[8], [0.0, 0.0]);
    assert_eq!(embedded[15], [0.0, 0.0]);
    assert_eq!(embedded[0], [7.0, 7.0], "assigned slots untouched");
    assert_eq!(embedded[9], [7.0, 7.0], "assigned slots untouched");

    assert_eq!(
        map.embed(3, &surround, &mut embedded),
        Err(MultichannelError::UnknownEmbeddedAssignment(3))
    );
    assert_eq!(
        map.embed(0, &surround, &mut embedded),
        Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Input,
            expected: 2,
            actual: 6
        })
    );

    // Full 16-channel discrete passthrough occupies every slot.
    let mut full = EmbeddedAudioMap::new();
    full.assign(
        StandardLayout::Discrete16.layout(),
        EmbeddedSlot::new(1, 1).unwrap(),
    )
    .unwrap();
    let source = embedded_planes();
    let mut embedded = outputs(16, 3);
    full.embed(0, &source, &mut embedded).unwrap();
    let mut back = outputs(16, 3);
    assert_eq!(
        full.deembed(0, &embedded, &mut back),
        Ok(StandardLayout::Discrete16.layout())
    );
    assert_eq!(bits(&back), bits(&source));
}
