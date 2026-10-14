use std::sync::Arc;

use fm_playback::{
    CacheInsert, CacheRejection, FolderListingError, GapPolicy, ImageSequence, ImageSequenceError,
    MAX_SEQUENCE_FRAMES, PrefetchBudget, PrefetchBudgetError, PrefetchCache, SequenceClock,
    SequenceClockError, SequenceEndMode, SequencePadding, SequencePosition, parse_sequence_name,
    prefetch_order,
};
use fm_types::{FrameRate, TimeBase};

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn names(numbers: &[&str]) -> Vec<String> {
    numbers
        .iter()
        .map(|digits| format!("sting_{digits}.png"))
        .collect()
}

fn frame_names(sequence: &ImageSequence) -> Vec<String> {
    (0..sequence.frame_count())
        .map(|frame| sequence.file(frame).unwrap().name().to_owned())
        .collect()
}

#[test]
fn parses_the_last_digit_run_of_the_stem() {
    let parsed = parse_sequence_name("shot_v2_0042.matte.PNG").unwrap();
    assert_eq!(parsed.prefix, "shot_v2_");
    assert_eq!(parsed.digits, "0042");
    assert_eq!(parsed.infix, ".matte");
    assert_eq!(parsed.extension, "PNG");
    assert_eq!(parsed.number, 42);
    assert_eq!(parse_sequence_name("0007.jpg").unwrap().prefix, "");
    assert_eq!(parse_sequence_name("still.png"), None);
    assert_eq!(parse_sequence_name("frame_0001.txt"), None);
    assert_eq!(
        parse_sequence_name(&format!("f{}.png", "1".repeat(19))),
        None
    );
    assert_eq!(
        parse_sequence_name(&format!("f{}.png", "9".repeat(18)))
            .unwrap()
            .number,
        999_999_999_999_999_999
    );
}

#[test]
fn discovers_fixed_padding_and_ignores_unrelated_files() {
    let mut listing = names(&["0003", "0001", "0002", "0004"]);
    listing.extend(
        [
            "sting_0006.jpg",
            "other_0001.png",
            "sting_0001.matte.png",
            "notes.txt",
            "sting_0005.PNG",
        ]
        .map(str::to_owned),
    );
    let sequence = ImageSequence::discover(&listing, "sting_0002.png", GapPolicy::Reject).unwrap();
    assert_eq!(sequence.padding(), SequencePadding::Fixed(4));
    assert_eq!(sequence.frame_count(), 5);
    assert_eq!(
        frame_names(&sequence),
        [
            "sting_0001.png",
            "sting_0002.png",
            "sting_0003.png",
            "sting_0004.png",
            "sting_0005.PNG",
        ]
    );
    // jpg, other prefix, and other infix are supported images but not members.
    assert_eq!(sequence.unrelated_images(), 3);
    assert_eq!(sequence.missing_numbers(), 0);
    assert_eq!(sequence.files()[4].number(), 5);
    assert_eq!(sequence.gap_policy(), GapPolicy::Reject);
}

#[test]
fn orders_unpadded_and_overflowing_padding_numerically() {
    let unpadded = ImageSequence::discover(
        &names(&["10", "9", "11", "8"]),
        "sting_9.png",
        GapPolicy::Reject,
    )
    .unwrap();
    assert_eq!(unpadded.padding(), SequencePadding::Unpadded);
    assert_eq!(
        frame_names(&unpadded),
        ["sting_8.png", "sting_9.png", "sting_10.png", "sting_11.png"]
    );

    let overflow = ImageSequence::discover(
        &names(&["1000", "998", "999", "1001"]),
        "sting_1000.png",
        GapPolicy::Reject,
    )
    .unwrap();
    assert_eq!(overflow.padding(), SequencePadding::Unpadded);
    let mixed = ImageSequence::discover(
        &names(&["0998", "0999", "1000", "01001"]),
        "sting_0998.png",
        GapPolicy::Reject,
    )
    .unwrap();
    assert_eq!(mixed.padding(), SequencePadding::Mixed);
    assert_eq!(mixed.files()[3].name(), "sting_01001.png");
}

#[test]
fn rejects_ambiguous_numbers_and_bad_seeds() {
    assert_eq!(
        ImageSequence::discover(&names(&["01", "001"]), "sting_01.png", GapPolicy::Skip),
        Err(ImageSequenceError::DuplicateNumber {
            number: 1,
            first: "sting_01.png".to_owned(),
            second: "sting_001.png".to_owned(),
        })
    );
    assert_eq!(
        ImageSequence::discover(&names(&["1"]), "sting_2.png", GapPolicy::Skip),
        Err(ImageSequenceError::SeedNotFound("sting_2.png".to_owned()))
    );
    assert_eq!(
        ImageSequence::discover(&["logo.png"], "logo.png", GapPolicy::Skip),
        Err(ImageSequenceError::SeedNotNumbered("logo.png".to_owned()))
    );
    assert!(matches!(
        ImageSequence::discover(&["a/1.png"], "a/1.png", GapPolicy::Skip),
        Err(ImageSequenceError::Listing(
            FolderListingError::InvalidName { index: 0 }
        ))
    ));
}

#[test]
fn gap_policies_are_exact() {
    let listing = names(&["001", "002", "005", "006"]);
    assert_eq!(
        ImageSequence::discover(&listing, "sting_001.png", GapPolicy::Reject),
        Err(ImageSequenceError::Gap {
            first_missing: 3,
            last_missing: 4,
        })
    );

    let held = ImageSequence::discover(&listing, "sting_001.png", GapPolicy::HoldPrevious).unwrap();
    assert_eq!(held.frame_count(), 6);
    assert_eq!(held.missing_numbers(), 2);
    assert_eq!(
        frame_names(&held),
        [
            "sting_001.png",
            "sting_002.png",
            "sting_002.png",
            "sting_002.png",
            "sting_005.png",
            "sting_006.png",
        ]
    );
    assert_eq!(held.file_index(6), None);

    let skipped = ImageSequence::discover(&listing, "sting_006.png", GapPolicy::Skip).unwrap();
    assert_eq!(skipped.frame_count(), 4);
    assert_eq!(
        frame_names(&skipped),
        [
            "sting_001.png",
            "sting_002.png",
            "sting_005.png",
            "sting_006.png",
        ]
    );

    let huge = names(&["0", &MAX_SEQUENCE_FRAMES.to_string()]);
    assert_eq!(
        ImageSequence::discover(&huge, "sting_0.png", GapPolicy::HoldPrevious),
        Err(ImageSequenceError::TooManyFrames {
            frames: u64::from(MAX_SEQUENCE_FRAMES) + 1,
            maximum: MAX_SEQUENCE_FRAMES,
        })
    );
    assert_eq!(
        ImageSequence::discover(&huge, "sting_0.png", GapPolicy::Skip)
            .unwrap()
            .frame_count(),
        2
    );
}

#[test]
fn clock_maps_output_frames_exactly_at_fractional_rates() {
    // 24 fps sequence on a 60 fps output: 2:3 cadence, 5 output frames per 2.
    let clock = SequenceClock::new(rate(24, 1), 48, SequenceEndMode::Once).unwrap();
    let cadence: Vec<SequencePosition> = (0..10)
        .map(|frame| clock.position_at_output_frame(frame, rate(60, 1)))
        .collect();
    let expected: Vec<SequencePosition> = [0, 0, 0, 1, 1, 2, 2, 2, 3, 3]
        .into_iter()
        .map(SequencePosition::Frame)
        .collect();
    assert_eq!(cadence, expected);

    // A 30000/1001 sequence on a 30000/1001 output is 1:1 for a long run.
    let ntsc = rate(30_000, 1_001);
    let looped = SequenceClock::new(ntsc, 1_000, SequenceEndMode::Loop).unwrap();
    for output_frame in (0..5_000_000_u64).step_by(997) {
        assert_eq!(
            looped.position_at_output_frame(output_frame, ntsc),
            SequencePosition::Frame(u32::try_from(output_frame % 1_000).unwrap())
        );
    }

    // A 24000/1001 sequence on a 60000/1001 output keeps the exact 2:3 cadence.
    let film = SequenceClock::new(rate(24_000, 1_001), 1_000_000, SequenceEndMode::Once).unwrap();
    for output_frame in [0_u64, 1, 2, 3, 4, 5, 999_995, 1_000_000, 2_499_999] {
        assert_eq!(
            film.position_at_output_frame(output_frame, rate(60_000, 1_001)),
            SequencePosition::Frame(u32::try_from(output_frame * 2 / 5).unwrap())
        );
    }
    assert_eq!(
        film.position_at_output_frame(2_500_000, rate(60_000, 1_001)),
        SequencePosition::Ended
    );
    assert_eq!(film.output_frames_per_pass(rate(60_000, 1_001)), 2_500_000);
    assert_eq!(clock.output_frames_per_pass(rate(60, 1)), 120);
    assert_eq!(
        SequenceClock::new(ntsc, 10, SequenceEndMode::Once)
            .unwrap()
            .output_frames_per_pass(rate(25, 1)),
        9
    );
    // Extreme inputs saturate instead of overflowing.
    assert_eq!(
        film.position_at_output_frame(u64::MAX, rate(1, u32::MAX)),
        SequencePosition::Ended
    );
}

#[test]
fn clock_maps_media_time_with_exact_frame_boundaries() {
    // At 30000/1001 with a 90 kHz time base, frame k starts at tick 3003k.
    let clock = SequenceClock::new(rate(30_000, 1_001), 100_000, SequenceEndMode::Once).unwrap();
    let time_base = TimeBase::new(1, 90_000).unwrap();
    for frame in [1_i64, 2, 999, 54_321, 99_999] {
        let start = frame * 3_003;
        assert_eq!(
            clock.position_at_time(start, time_base),
            SequencePosition::Frame(u32::try_from(frame).unwrap())
        );
        assert_eq!(
            clock.position_at_time(start - 1, time_base),
            SequencePosition::Frame(u32::try_from(frame - 1).unwrap())
        );
    }
    assert_eq!(
        clock.position_at_time(-1, time_base),
        SequencePosition::BeforeStart
    );
    assert_eq!(
        clock.position_at_time(100_000 * 3_003, time_base),
        SequencePosition::Ended
    );
    assert_eq!(
        clock.position_at_time(i64::MAX, TimeBase::new(u32::MAX, 1).unwrap()),
        SequencePosition::Ended
    );
}

#[test]
fn end_modes_resolve_after_the_last_frame() {
    use SequencePosition::{Ended, Frame};

    let time_base = TimeBase::new(1, 25).unwrap();
    let positions = |mode| {
        let clock = SequenceClock::new(rate(25, 1), 3, mode).unwrap();
        (0..7)
            .map(|tick| clock.position_at_time(tick, time_base))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        positions(SequenceEndMode::Once),
        [Frame(0), Frame(1), Frame(2), Ended, Ended, Ended, Ended]
    );
    assert_eq!(
        positions(SequenceEndMode::Hold),
        [
            Frame(0),
            Frame(1),
            Frame(2),
            Frame(2),
            Frame(2),
            Frame(2),
            Frame(2)
        ]
    );
    assert_eq!(
        positions(SequenceEndMode::Loop),
        [
            Frame(0),
            Frame(1),
            Frame(2),
            Frame(0),
            Frame(1),
            Frame(2),
            Frame(0)
        ]
    );
    assert_eq!(
        SequenceClock::new(rate(25, 1), 0, SequenceEndMode::Loop),
        Err(SequenceClockError::EmptySequence)
    );
    let clock = SequenceClock::new(rate(25, 1), 3, SequenceEndMode::Hold).unwrap();
    assert_eq!(clock.rate(), rate(25, 1));
    assert_eq!(clock.frame_count(), 3);
    assert_eq!(clock.end_mode(), SequenceEndMode::Hold);
}

#[test]
fn upcoming_frames_and_prefetch_order_follow_the_end_mode() {
    let loop_clock = SequenceClock::new(rate(25, 1), 5, SequenceEndMode::Loop).unwrap();
    assert_eq!(loop_clock.upcoming_frames(3, 3), [3, 4, 0, 1]);
    assert_eq!(loop_clock.upcoming_frames(3, 100), [3, 4, 0, 1, 2]);
    let once = SequenceClock::new(rate(25, 1), 5, SequenceEndMode::Once).unwrap();
    assert_eq!(once.upcoming_frames(3, 3), [3, 4]);
    assert_eq!(once.upcoming_frames(5, 3), Vec::<u32>::new());

    let held = ImageSequence::discover(
        &names(&["1", "2", "5"]),
        "sting_1.png",
        GapPolicy::HoldPrevious,
    )
    .unwrap();
    // Frames 1..=3 all show file 1 (sting_2).
    assert_eq!(prefetch_order(&held, &[0, 1, 2, 3, 4]), [0, 1, 2]);
    assert_eq!(prefetch_order(&held, &[4, 0, 9]), [2, 0]);
}

#[test]
fn cache_budget_is_validated() {
    assert_eq!(
        PrefetchCache::<u8>::new(PrefetchBudget {
            max_bytes: 0,
            max_entries: 1
        })
        .err(),
        Some(PrefetchBudgetError::ZeroBytes)
    );
    assert_eq!(
        PrefetchCache::<u8>::new(PrefetchBudget {
            max_bytes: 1,
            max_entries: 0
        })
        .err(),
        Some(PrefetchBudgetError::ZeroEntries)
    );
}

#[test]
fn cache_evicts_furthest_and_never_exceeds_budget() {
    let budget = PrefetchBudget {
        max_bytes: 300,
        max_entries: 8,
    };
    let mut cache = PrefetchCache::new(budget).unwrap();
    cache.set_window(&[0, 1, 2, 3]);
    assert_eq!(cache.missing(), [0, 1, 2, 3]);
    for index in [0, 1, 2] {
        assert_eq!(
            cache.insert(index, Arc::new(index), 100),
            CacheInsert::Inserted { evicted: 0 }
        );
    }
    assert_eq!(cache.resident_bytes(), 300);
    // Index 3 is further than every resident, so nothing nearer is evicted.
    assert_eq!(
        cache.insert(3, Arc::new(3), 100),
        CacheInsert::Rejected(CacheRejection::WouldEvictNearer)
    );
    assert_eq!(
        cache.insert(0, Arc::new(0), 100),
        CacheInsert::AlreadyResident
    );
    assert_eq!(
        cache.insert(9, Arc::new(9), 301),
        CacheInsert::Rejected(CacheRejection::LargerThanBudget {
            bytes: 301,
            maximum: 300,
        })
    );

    // Advance: the window moves to [1, 2, 3, 4]; file 0 leaves immediately.
    assert_eq!(cache.set_window(&[1, 2, 3, 4]), 1);
    assert!(!cache.contains(0));
    assert_eq!(cache.missing(), [3, 4]);
    assert_eq!(
        cache.insert(3, Arc::new(3), 100),
        CacheInsert::Inserted { evicted: 0 }
    );

    // A nearer value displaces the furthest resident.
    cache.set_window(&[5, 1, 2, 3]);
    assert_eq!(
        cache.insert(5, Arc::new(5), 150),
        CacheInsert::Inserted { evicted: 2 }
    );
    assert!(cache.contains(5) && cache.contains(1));
    assert!(!cache.contains(2) && !cache.contains(3));
    assert_eq!(cache.resident_bytes(), 250);

    assert_eq!(cache.get(5).as_deref(), Some(&5));
    assert_eq!(cache.get(2), None);
    let stats = cache.stats();
    assert_eq!((stats.hits, stats.misses), (1, 1));
    assert_eq!(stats.inserts, 5);
    assert_eq!(stats.evictions, 3);
    assert_eq!(stats.rejections, 2);
    assert_eq!(stats.peak_resident_bytes, 300);
    assert_eq!(cache.peek(1).map(|value| **value), Some(1));
    assert_eq!(cache.len(), 2);
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.resident_bytes(), 0);
    assert_eq!(cache.budget(), budget);
}

#[test]
fn simulated_looping_playback_stays_within_entry_and_byte_budget() {
    let listing: Vec<String> = (0..40)
        .map(|number| format!("loop_{number:03}.png"))
        .collect();
    let sequence = ImageSequence::discover(&listing, "loop_000.png", GapPolicy::Reject).unwrap();
    let clock = SequenceClock::new(
        rate(30_000, 1_001),
        sequence.frame_count(),
        SequenceEndMode::Loop,
    )
    .unwrap();
    let budget = PrefetchBudget {
        max_bytes: 1_000,
        max_entries: 6,
    };
    let mut cache = PrefetchCache::new(budget).unwrap();
    let mut decodes = 0_u64;
    for output_frame in 0..400 {
        let SequencePosition::Frame(frame) =
            clock.position_at_output_frame(output_frame, rate(60_000, 1_001))
        else {
            panic!("looping sequence always has a frame");
        };
        cache.set_window(&prefetch_order(&sequence, &clock.upcoming_frames(frame, 7)));
        for index in cache.missing() {
            // Each decoded file is accounted as 150 bytes.
            if matches!(
                cache.insert(index, Arc::new(index), 150),
                CacheInsert::Inserted { .. }
            ) {
                decodes += 1;
            }
        }
        let current = sequence.file_index(frame).unwrap();
        assert_eq!(cache.get(current).as_deref(), Some(&current));
        assert!(cache.resident_bytes() <= budget.max_bytes);
        assert!(cache.len() <= budget.max_entries);
    }
    // 400 output frames at 2x cover 200 sequence frames: each decoded once.
    assert_eq!(decodes, 200 + 5);
    assert_eq!(cache.stats().misses, 0);
    assert!(cache.stats().peak_resident_bytes <= budget.max_bytes);
}
