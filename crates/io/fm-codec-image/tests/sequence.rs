use std::num::NonZeroU128;
use std::path::Path;

use fm_codec_image::{
    ImageSequenceConfig, ImageSequenceSource, ImageSequenceSourceError, SequenceSample,
    StillDecodeError, StillFormat, alpha_coverage,
};
use fm_frame::{
    AlphaMode, ClockDomainId, CpuVideoFrame, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SequenceNumber, TimeBase,
};
use fm_playback::{
    GapPolicy, ImageSequenceError, PrefetchBudget, SequenceEndMode, SequencePadding,
};
use fm_types::FrameRate;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const FRAME_BYTES: usize = (WIDTH * HEIGHT * 4) as usize;

fn timing() -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), TimeBase::new(1, 90_000).unwrap()),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(40_000_000).unwrap(),
        ClockDomainId::new(NonZeroU128::new(3).unwrap()),
        SequenceNumber::new(0),
    )
    .unwrap()
}

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

/// RGBA for frame `id`: red encodes the id, column `x` has alpha
/// `alpha(x)`, blue encodes the column.
fn rgba(id: u8, alpha: impl Fn(u32) -> u8) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(FRAME_BYTES);
    for _ in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels.extend_from_slice(&[id, 200, u8::try_from(x).unwrap() * 10, alpha(x)]);
        }
    }
    pixels
}

fn write_png(directory: &Path, name: &str, width: u32, height: u32, pixels: &[u8]) {
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .unwrap();
    std::fs::write(directory.join(name), encoded).unwrap();
}

fn write_frame(directory: &Path, name: &str, id: u8, alpha: impl Fn(u32) -> u8) {
    write_png(directory, name, WIDTH, HEIGHT, &rgba(id, alpha));
}

/// A six-frame stinger: wipe in (frames 0..=2 partially transparent), fully
/// opaque frame 3, then wipe out (frames 4, 5).
fn stinger_folder() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let coverage = [2_u32, 4, 6, 8, 5, 0];
    for (index, covered) in coverage.into_iter().enumerate() {
        let id = u8::try_from(index).unwrap() * 40 + 1;
        write_frame(
            path,
            &format!("sting_{:04}.png", index + 1),
            id,
            |x| match x.cmp(&covered) {
                std::cmp::Ordering::Less => 255,
                std::cmp::Ordering::Equal => 128,
                std::cmp::Ordering::Greater => 0,
            },
        );
    }
    std::fs::write(path.join("readme.txt"), b"not an image").unwrap();
    write_frame(path, "logo.png", 99, |_| 255);
    std::fs::create_dir(path.join("sting_0007.png")).unwrap();
    directory
}

fn id_of(sample: &SequenceSample) -> Option<u8> {
    match sample {
        SequenceSample::Frame { frame, .. } => Some(frame.payload().plane(0).unwrap().bytes()[0]),
        _ => None,
    }
}

fn config(end_mode: SequenceEndMode) -> ImageSequenceConfig {
    ImageSequenceConfig::new(rate(25, 1), end_mode, GapPolicy::Reject)
}

#[test]
fn alpha_stinger_plays_once_with_exact_straight_alpha() {
    let folder = stinger_folder();
    let mut source = ImageSequenceSource::open(
        folder.path(),
        "sting_0003.png",
        config(SequenceEndMode::Once),
        timing(),
    )
    .unwrap();
    let sequence = source.sequence();
    assert_eq!(sequence.frame_count(), 6);
    assert_eq!(sequence.padding(), SequencePadding::Fixed(4));
    assert_eq!(sequence.unrelated_images(), 1);

    // 25 fps sequence on a 50 fps output: every frame twice, then ended.
    let ids: Vec<Option<u8>> = (0..14)
        .map(|output_frame| {
            id_of(
                &source
                    .sample_output_frame(output_frame, rate(50, 1))
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ids,
        [
            Some(1),
            Some(1),
            Some(41),
            Some(41),
            Some(81),
            Some(81),
            Some(121),
            Some(121),
            Some(161),
            Some(161),
            Some(201),
            Some(201),
            None,
            None,
        ]
    );
    assert_eq!(
        source.sample_output_frame(12, rate(50, 1)).unwrap(),
        SequenceSample::Ended
    );
    assert_eq!(source.clock().output_frames_per_pass(rate(50, 1)), 12);

    let SequenceSample::Frame {
        frame,
        timeline_frame,
        file_index,
        ..
    } = source.sample_output_frame(4, rate(50, 1)).unwrap()
    else {
        panic!("frame expected");
    };
    assert_eq!((timeline_frame, file_index), (2, 2));
    // PNG is lossless: straight (unpremultiplied) colour survives under
    // partial and zero alpha.
    assert_eq!(
        frame.payload().plane(0).unwrap().bytes(),
        rgba(81, |x| match x {
            0..6 => 255,
            6 => 128,
            _ => 0,
        })
    );
    assert_eq!(
        frame.metadata().unwrap().alpha_mode(),
        Some(AlphaMode::Straight)
    );
    assert_eq!(frame.timing(), timing());
    let coverage = alpha_coverage(&frame).unwrap();
    assert_eq!(
        (coverage.opaque, coverage.transparent, coverage.total),
        (24, 4, 32)
    );
    assert!(!coverage.is_fully_opaque());

    // The first fully opaque frame is the invisible cut point.
    assert_eq!(source.suggest_cut_point().unwrap(), Some(3));
    assert!(source.suggest_cut_point().unwrap().unwrap() < source.sequence().frame_count());
    assert_eq!(
        source.dimensions().map(|d| (d.width(), d.height())),
        Some((WIDTH, HEIGHT))
    );
}

#[test]
fn preloaded_stinger_plays_without_further_decodes() {
    let folder = stinger_folder();
    let mut source = ImageSequenceSource::open(
        folder.path(),
        "sting_0001.png",
        ImageSequenceConfig {
            lookahead: 1,
            ..config(SequenceEndMode::Once)
        },
        timing(),
    )
    .unwrap();
    assert!(!source.is_fully_resident());
    source.preload_all().unwrap();
    assert!(source.is_fully_resident());
    assert_eq!(source.decodes(), 6);
    assert_eq!(source.resident_bytes(), 6 * FRAME_BYTES);

    for output_frame in 0..12 {
        let sample = source
            .sample_output_frame(output_frame, rate(50, 1))
            .unwrap();
        assert!(matches!(
            sample,
            SequenceSample::Frame {
                cache_hit: true,
                ..
            }
        ));
    }
    // A narrow lookahead does not evict a pinned preload.
    assert_eq!(source.resident_files(), 6);
    assert_eq!(source.decodes(), 6);
    assert_eq!(source.prefetch_from(0).unwrap(), 0);
    assert_eq!(source.suggest_cut_point().unwrap(), Some(3));
    assert_eq!(source.decodes(), 6);
    assert_eq!(source.cache_stats().misses, 0);

    source.release_preload();
    source.sample_output_frame(0, rate(50, 1)).unwrap();
    assert_eq!(source.resident_files(), 2);
}

#[test]
fn preload_reports_budgets_that_cannot_hold_the_stinger() {
    let folder = stinger_folder();
    let open = |prefetch| {
        ImageSequenceSource::open(
            folder.path(),
            "sting_0001.png",
            ImageSequenceConfig {
                prefetch,
                ..config(SequenceEndMode::Once)
            },
            timing(),
        )
        .unwrap()
    };
    let few_entries = PrefetchBudget {
        max_bytes: 1 << 20,
        max_entries: 5,
    };
    let mut source = open(few_entries);
    assert!(matches!(
        source.preload_all(),
        Err(ImageSequenceSourceError::PreloadExceedsBudget { files: 6, budget }) if budget == few_entries
    ));
    assert_eq!(source.decodes(), 0);

    let few_bytes = PrefetchBudget {
        max_bytes: 5 * FRAME_BYTES,
        max_entries: 64,
    };
    let mut source = open(few_bytes);
    assert!(matches!(
        source.preload_all(),
        Err(ImageSequenceSourceError::PreloadExceedsBudget { files: 6, .. })
    ));
    assert!(source.resident_bytes() <= few_bytes.max_bytes);
}

#[test]
fn looping_playback_prefetches_within_the_memory_budget() {
    let directory = tempfile::tempdir().unwrap();
    for number in 0..20_u8 {
        write_frame(
            directory.path(),
            &format!("loop.{number}.png"),
            number,
            |_| 255,
        );
    }
    let budget = PrefetchBudget {
        max_bytes: 5 * FRAME_BYTES,
        max_entries: 10,
    };
    let mut source = ImageSequenceSource::open(
        directory.path(),
        "loop.7.png",
        ImageSequenceConfig {
            rate: rate(30_000, 1_001),
            prefetch: budget,
            lookahead: 4,
            ..config(SequenceEndMode::Loop)
        },
        timing(),
    )
    .unwrap();
    assert_eq!(source.sequence().padding(), SequencePadding::Unpadded);
    let output_rate = rate(60_000, 1_001);
    for output_frame in 0..200_u64 {
        let expected = u8::try_from((output_frame / 2) % 20).unwrap();
        let fm_playback::SequencePosition::Frame(timeline) = source
            .clock()
            .position_at_output_frame(output_frame, output_rate)
        else {
            panic!("loop always has a frame");
        };
        source.prefetch_from(timeline).unwrap();
        let sample = source
            .sample_output_frame(output_frame, output_rate)
            .unwrap();
        assert_eq!(
            id_of(&sample),
            Some(expected),
            "output frame {output_frame}"
        );
        assert!(matches!(
            sample,
            SequenceSample::Frame {
                cache_hit: true,
                ..
            }
        ));
        assert!(source.resident_bytes() <= budget.max_bytes);
        assert!(source.resident_files() <= 5);
    }
    // 100 sequence frames shown, plus the 4-frame lookahead, each decoded once.
    assert_eq!(source.decodes(), 104);
    assert_eq!(source.cache_stats().misses, 0);
}

#[test]
fn media_time_maps_to_files_at_fractional_rates() {
    let directory = tempfile::tempdir().unwrap();
    for number in 1..=3_u8 {
        write_frame(
            directory.path(),
            &format!("t_{number:02}.png"),
            number,
            |_| 255,
        );
    }
    let mut source = ImageSequenceSource::open(
        directory.path(),
        "t_01.png",
        ImageSequenceConfig {
            rate: rate(30_000, 1_001),
            ..config(SequenceEndMode::Hold)
        },
        timing(),
    )
    .unwrap();
    let time_base = TimeBase::new(1, 90_000).unwrap();
    let ids: Vec<Option<u8>> = [-1_i64, 0, 3_002, 3_003, 6_005, 6_006, 1_000_000]
        .into_iter()
        .map(|tick| id_of(&source.sample_at_time(tick, time_base).unwrap()))
        .collect();
    assert_eq!(
        ids,
        [None, Some(1), Some(1), Some(2), Some(2), Some(3), Some(3)]
    );
    assert_eq!(
        source.sample_at_time(-1, time_base).unwrap(),
        SequenceSample::BeforeStart
    );
}

#[test]
fn held_gaps_show_the_previous_file() {
    let directory = tempfile::tempdir().unwrap();
    for number in [1_u8, 2, 5] {
        write_frame(directory.path(), &format!("g_{number}.png"), number, |_| {
            255
        });
    }
    let open = |gap_policy| {
        ImageSequenceSource::open(
            directory.path(),
            "g_1.png",
            ImageSequenceConfig {
                gap_policy,
                ..config(SequenceEndMode::Once)
            },
            timing(),
        )
    };
    assert!(matches!(
        open(GapPolicy::Reject),
        Err(ImageSequenceSourceError::Discover(
            ImageSequenceError::Gap {
                first_missing: 3,
                last_missing: 4,
            }
        ))
    ));
    let mut source = open(GapPolicy::HoldPrevious).unwrap();
    let ids: Vec<Option<u8>> = (0..6)
        .map(|frame| id_of(&source.sample_output_frame(frame, rate(25, 1)).unwrap()))
        .collect();
    assert_eq!(ids, [Some(1), Some(2), Some(2), Some(2), Some(5), None]);
    assert_eq!(source.decodes(), 3);
}

#[test]
fn inconsistent_or_invalid_files_fail_with_their_name() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    write_frame(path, "m_1.png", 1, |_| 255);
    write_png(path, "m_2.png", 4, 4, &[255; 64]);
    std::fs::write(path.join("m_3.png"), b"\x89PNG\r\n\x1a\ncorrupt").unwrap();
    let mut source =
        ImageSequenceSource::open(path, "m_1.png", config(SequenceEndMode::Once), timing())
            .unwrap();
    source.sample_output_frame(0, rate(25, 1)).unwrap();
    match source.sample_output_frame(1, rate(25, 1)) {
        Err(ImageSequenceSourceError::DimensionMismatch {
            name,
            expected,
            actual,
        }) => {
            assert_eq!(name, "m_2.png");
            assert_eq!((expected.width(), expected.height()), (WIDTH, HEIGHT));
            assert_eq!((actual.width(), actual.height()), (4, 4));
        }
        other => panic!("unexpected {other:?}"),
    }
    match source.sample_output_frame(2, rate(25, 1)) {
        Err(ImageSequenceSourceError::Decode { name, error }) => {
            assert_eq!(name, "m_3.png");
            assert_eq!(
                error,
                StillDecodeError::CorruptInput {
                    format: StillFormat::Png
                }
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut limited = ImageSequenceSource::open(
        path,
        "m_1.png",
        ImageSequenceConfig {
            decode_limits: fm_codec_image::StillDecodeLimits {
                max_encoded_bytes: 16,
                ..fm_codec_image::StillDecodeLimits::default()
            },
            ..config(SequenceEndMode::Once)
        },
        timing(),
    )
    .unwrap();
    assert!(matches!(
        limited.sample_output_frame(0, rate(25, 1)),
        Err(ImageSequenceSourceError::EncodedFileTooLarge { maximum: 16, .. })
    ));
}

#[test]
fn directory_errors_are_typed_and_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("absent");
    assert!(matches!(
        ImageSequenceSource::open(&missing, "a_1.png", config(SequenceEndMode::Once), timing()),
        Err(ImageSequenceSourceError::Directory {
            kind: std::io::ErrorKind::NotFound,
            ..
        })
    ));
    for number in 1..=5 {
        write_frame(directory.path(), &format!("a_{number}.png"), 0, |_| 255);
    }
    assert!(matches!(
        ImageSequenceSource::open(
            directory.path(),
            "a_1.png",
            ImageSequenceConfig {
                max_directory_entries: 3,
                ..config(SequenceEndMode::Once)
            },
            timing()
        ),
        Err(ImageSequenceSourceError::TooManyDirectoryEntries { maximum: 3 })
    ));
    assert!(matches!(
        ImageSequenceSource::open(
            directory.path(),
            "b_1.png",
            config(SequenceEndMode::Once),
            timing()
        ),
        Err(ImageSequenceSourceError::Discover(
            ImageSequenceError::SeedNotFound(_)
        ))
    ));
    assert!(matches!(
        ImageSequenceSource::open(
            directory.path(),
            "a_1.png",
            ImageSequenceConfig {
                prefetch: PrefetchBudget {
                    max_bytes: 0,
                    max_entries: 1
                },
                ..config(SequenceEndMode::Once)
            },
            timing()
        ),
        Err(ImageSequenceSourceError::Budget(_))
    ));
}

#[test]
fn alpha_coverage_ignores_non_rgba_payloads() {
    let payload = fm_frame::CpuVideoPayload::allocate(
        fm_frame::PixelFormat::Nv12,
        fm_frame::VideoDimensions::new(2, 2).unwrap(),
    )
    .unwrap();
    let frame = CpuVideoFrame::new(timing(), payload);
    assert_eq!(alpha_coverage(&frame), None);
}
