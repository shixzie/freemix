//! Integration coverage for the photo slideshow input: per-slide durations,
//! cut/crossfade output, exact Ken Burns crops, ordered and seeded-random
//! order, live list edits, large-set thumbnail suppression, and folder
//! listing with extension filtering and natural sort.

use std::collections::BTreeSet;

use fm_playback::{
    FULL_MIX_BASIS_POINTS, FolderListingError, ImageFolderListing, KenBurns, MAX_FOLDER_ENTRIES,
    MAX_SLIDE_BROWSER_PAGE, MAX_SLIDE_TRANSITION_FRAMES, PixelRect, RectEdge, Slide, SlideId,
    SlideOrder, SlideTransition, Slideshow, SlideshowConfig, SlideshowEditError, SlideshowError,
    is_supported_image, natural_cmp,
};

const A: SlideId = SlideId::new(1);
const B: SlideId = SlideId::new(2);
const C: SlideId = SlideId::new(3);
const D: SlideId = SlideId::new(4);

fn slide(id: SlideId) -> Slide {
    Slide::new(id, format!("photo{}.jpg", id.get()), 100, 100)
}

fn abc() -> Vec<Slide> {
    vec![slide(A), slide(B), slide(C)]
}

fn ids(show: &mut Slideshow, frames: usize) -> Vec<SlideId> {
    (0..frames).map(|_| show.tick().incoming.id).collect()
}

/// Returns the sequence of distinct slides started, one per hold.
fn starts(show: &mut Slideshow, frames: usize) -> Vec<SlideId> {
    let mut out: Vec<SlideId> = Vec::new();
    let mut last = None;
    for _ in 0..frames {
        let frame = show.tick();
        if frame.outgoing.is_none() && last != Some((frame.incoming.id, frame.cycle)) {
            out.push(frame.incoming.id);
            last = Some((frame.incoming.id, frame.cycle));
        }
    }
    out
}

#[test]
fn ordered_cut_honours_per_slide_durations_and_holds_at_end() {
    let slides = vec![slide(A), slide(B).with_duration(3), slide(C)];
    let mut show = Slideshow::new(slides, SlideshowConfig::new(2)).unwrap();
    assert_eq!(ids(&mut show, 7), [A, A, B, B, B, C, C]);
    let frame = show.tick();
    assert_eq!(frame.incoming.id, C);
    assert!(frame.at_end);
    assert_eq!(frame.frame, 7);
    assert_eq!(frame.mix_basis_points, FULL_MIX_BASIS_POINTS);
    assert_eq!(show.peek_next(), None);
}

#[test]
fn looping_ordered_show_wraps_and_counts_cycles() {
    let config = SlideshowConfig {
        loop_show: true,
        ..SlideshowConfig::new(1)
    };
    let mut show = Slideshow::new(abc(), config).unwrap();
    let frames: Vec<_> = (0..7).map(|_| show.tick()).collect();
    let seq: Vec<_> = frames.iter().map(|f| (f.incoming.id, f.cycle)).collect();
    assert_eq!(
        seq,
        [(A, 0), (B, 0), (C, 0), (A, 1), (B, 1), (C, 1), (A, 2)]
    );
}

#[test]
fn crossfade_emits_exact_mixed_frames_between_holds() {
    let config = SlideshowConfig {
        transition: SlideTransition::Crossfade { frames: 3 },
        ..SlideshowConfig::new(2)
    };
    let mut show = Slideshow::new(vec![slide(A), slide(B)], config).unwrap();
    let frames: Vec<_> = (0..8).map(|_| show.tick()).collect();
    let summary: Vec<_> = frames
        .iter()
        .map(|f| {
            (
                f.outgoing.map(|o| (o.id, o.local_frame)),
                f.incoming.id,
                f.incoming.local_frame,
                f.mix_basis_points,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            // The first slide starts at local frame T (no incoming fade).
            (None, A, 3, 10_000),
            (None, A, 4, 10_000),
            (Some((A, 5)), B, 0, 2_500),
            (Some((A, 6)), B, 1, 5_000),
            (Some((A, 7)), B, 2, 7_500),
            (None, B, 3, 10_000),
            (None, B, 4, 10_000),
            (None, B, 5, 10_000),
        ]
    );
    assert!(frames[7].at_end);
    assert_eq!(show.on_air(), [B]);
}

#[test]
fn ken_burns_crop_is_exact_rational_interpolation() {
    let motion = KenBurns {
        start: PixelRect::new(0, 0, 100, 100),
        end: PixelRect::new(50, 50, 50, 50),
    };
    let only = slide(A).with_duration(4).with_ken_burns(motion);
    let mut show = Slideshow::new(vec![only], SlideshowConfig::new(4)).unwrap();
    let crops: Vec<_> = (0..6).map(|_| show.tick().incoming.crop).collect();
    let parts = |i: usize| {
        let c = crops[i];
        (
            c.x_numerator(),
            c.y_numerator(),
            c.width_numerator(),
            c.height_numerator(),
            c.denominator(),
        )
    };
    assert_eq!(parts(0), (0, 0, 100, 100, 1));
    assert_eq!(parts(1), (50, 50, 250, 250, 3));
    assert_eq!(parts(2), (100, 100, 200, 200, 3));
    assert_eq!(
        crops[3].as_pixel_rect(),
        Some(PixelRect::new(50, 50, 50, 50))
    );
    // Holding at the end clamps to the end rectangle.
    assert_eq!(crops[4], crops[3]);
    assert_eq!(crops[5], crops[3]);
    assert_eq!(crops[1].as_pixel_rect(), None);
}

#[test]
fn ken_burns_spans_incoming_hold_and_outgoing_frames() {
    let motion = KenBurns {
        start: PixelRect::new(0, 0, 80, 80),
        end: PixelRect::new(20, 20, 80, 80),
    };
    let config = SlideshowConfig {
        transition: SlideTransition::Crossfade { frames: 2 },
        loop_show: true,
        ..SlideshowConfig::new(1)
    };
    // Span = T + D + T = 5 local frames, so x moves 20 * k / 4 = 5 * k.
    let slides = vec![
        slide(A).with_ken_burns(motion),
        slide(B).with_ken_burns(motion),
    ];
    let mut show = Slideshow::new(slides, config).unwrap();
    let mut b_frames = Vec::new();
    for _ in 0..6 {
        let frame = show.tick();
        for view in frame.outgoing.into_iter().chain([frame.incoming]) {
            if view.id == B {
                b_frames.push((view.local_frame, view.crop.as_pixel_rect().unwrap().x));
            }
        }
    }
    assert_eq!(b_frames, [(0, 0), (1, 5), (2, 10), (3, 15), (4, 20)]);
}

#[test]
fn full_frame_crop_without_motion() {
    let mut show = Slideshow::new(
        vec![Slide::new(A, "a.png", 1920, 1080)],
        SlideshowConfig::new(3),
    )
    .unwrap();
    for _ in 0..4 {
        assert_eq!(
            show.tick().incoming.crop.as_pixel_rect(),
            Some(PixelRect::new(0, 0, 1920, 1080))
        );
    }
}

fn random_config(seed: u64) -> SlideshowConfig {
    SlideshowConfig {
        order: SlideOrder::Random { seed },
        loop_show: true,
        ..SlideshowConfig::new(1)
    }
}

fn many(count: u128) -> Vec<Slide> {
    (1..=count).map(|id| slide(SlideId::new(id))).collect()
}

#[test]
fn random_order_is_deterministic_permutation_per_cycle() {
    let run = |seed| {
        let mut show = Slideshow::new(many(8), random_config(seed)).unwrap();
        ids(&mut show, 40)
    };
    let first = run(42);
    assert_eq!(first, run(42));
    assert_ne!(first, run(43));
    let all: BTreeSet<_> = (1..=8).map(SlideId::new).collect();
    for cycle in first.chunks(8) {
        assert_eq!(cycle.iter().copied().collect::<BTreeSet<_>>(), all);
    }
    assert_ne!(&first[..8], &first[8..16], "each cycle reshuffles");
    for pair in first.windows(2) {
        assert_ne!(pair[0], pair[1], "no back-to-back repeat across cycles");
    }
    let ordered: Vec<_> = (1..=8).map(SlideId::new).collect();
    assert_ne!(&first[..8], ordered.as_slice());
}

#[test]
fn non_looping_random_show_plays_each_slide_once() {
    let config = SlideshowConfig {
        loop_show: false,
        ..random_config(7)
    };
    let mut show = Slideshow::new(many(5), config).unwrap();
    let seq = starts(&mut show, 12);
    assert_eq!(seq.len(), 5);
    assert_eq!(seq.iter().collect::<BTreeSet<_>>().len(), 5);
    assert!(show.tick().at_end);
}

#[test]
fn on_air_slides_cannot_be_removed() {
    let config = SlideshowConfig {
        transition: SlideTransition::Crossfade { frames: 2 },
        ..SlideshowConfig::new(1)
    };
    let mut show = Slideshow::new(abc(), config).unwrap();
    assert_eq!(show.remove(A), Err(SlideshowEditError::OnAir(A)));
    show.tick(); // A hold
    let frame = show.tick(); // first transition frame A -> B
    assert_eq!(frame.outgoing.map(|o| o.id), Some(A));
    assert_eq!(show.on_air(), [A, B]);
    assert_eq!(show.remove(A), Err(SlideshowEditError::OnAir(A)));
    assert_eq!(show.remove(B), Err(SlideshowEditError::OnAir(B)));
    assert_eq!(show.remove(D), Err(SlideshowEditError::UnknownSlide(D)));
    // Removing an off-air slide does not disturb the transition.
    assert_eq!(show.remove(C).unwrap().id(), C);
    let frame = show.tick();
    assert_eq!(
        (
            frame.outgoing.map(|o| o.id),
            frame.incoming.id,
            frame.mix_basis_points
        ),
        (Some(A), B, 6_666)
    );
    assert_eq!(show.tick().incoming.id, B);
    assert!(show.tick().at_end);
}

#[test]
fn ordered_edits_change_only_what_follows() {
    let mut show = Slideshow::new(abc(), SlideshowConfig::new(2)).unwrap();
    show.tick(); // A, 1 of 2
    show.insert(1, slide(D)).unwrap();
    assert_eq!(show.peek_next(), Some(D));
    show.move_slide(C, 0).unwrap(); // C, A, D, B
    assert_eq!(show.peek_next(), Some(D));
    let order: Vec<_> = show.slides().iter().map(Slide::id).collect();
    assert_eq!(order, [C, A, D, B]);
    // The current slide keeps its remaining frame.
    assert_eq!(ids(&mut show, 5), [A, D, D, B, B]);
    assert!(show.tick().at_end);
    // Appending to a finished non-looping show resumes it after the
    // already-due end-hold frame.
    show.insert(4, slide(SlideId::new(9))).unwrap();
    assert_eq!(show.tick().incoming.id, B);
    let frame = show.tick();
    assert_eq!(frame.incoming.id, SlideId::new(9));
    assert!(!frame.at_end);
}

#[test]
fn edit_errors_are_explicit() {
    let mut show = Slideshow::new(abc(), SlideshowConfig::new(2)).unwrap();
    assert_eq!(
        show.insert(4, slide(D)),
        Err(SlideshowEditError::IndexOutOfRange { index: 4, len: 3 })
    );
    assert_eq!(
        show.insert(0, slide(B)),
        Err(SlideshowEditError::DuplicateSlide(B))
    );
    assert_eq!(
        show.insert(0, Slide::new(D, "", 10, 10)),
        Err(SlideshowEditError::Invalid(
            SlideshowError::InvalidFilename(D)
        ))
    );
    assert_eq!(
        show.move_slide(B, 3),
        Err(SlideshowEditError::IndexOutOfRange { index: 3, len: 3 })
    );
    assert_eq!(
        show.move_slide(D, 0),
        Err(SlideshowEditError::UnknownSlide(D))
    );
}

#[test]
fn random_edits_respect_the_current_cycle() {
    let config = SlideshowConfig {
        loop_show: false,
        ..random_config(99)
    };
    let config = SlideshowConfig {
        default_duration_frames: 2,
        ..config
    };
    let mut show = Slideshow::new(many(4), config).unwrap();
    let first = show.tick().incoming.id;
    let next = show.peek_next().unwrap();
    // Removing the pending next slide drops it from this cycle.
    show.remove(next).unwrap();
    assert_ne!(show.peek_next(), Some(next));
    // A new slide joins the end of the current cycle.
    let added = SlideId::new(50);
    show.insert(0, slide(added)).unwrap();
    let seq = starts(&mut show, 12);
    assert_eq!(seq.len(), 4);
    assert_eq!(seq[0], first);
    assert_eq!(seq.last(), Some(&added));
    assert_eq!(seq.iter().collect::<BTreeSet<_>>().len(), 4);
    assert!(!seq.contains(&next));

    // Re-inserting an already played slide does not repeat it this cycle.
    let mut show = Slideshow::new(many(3), config).unwrap();
    let first = show.tick().incoming.id;
    let removed = show.peek_next().unwrap();
    show.remove(removed).unwrap();
    assert_eq!(show.tick().incoming.id, first);
    let played = show.tick().incoming.id;
    assert_ne!(played, first);
    show.remove(first).unwrap();
    show.insert(0, slide(first)).unwrap();
    assert_eq!(show.peek_next(), None);
}

#[test]
fn random_seed_and_cycle_are_reproducible_after_edits() {
    let script = || {
        let mut show = Slideshow::new(many(6), random_config(5)).unwrap();
        let mut seq = ids(&mut show, 3);
        show.insert(2, slide(SlideId::new(77))).unwrap();
        show.move_slide(SlideId::new(1), 5).unwrap();
        seq.extend(ids(&mut show, 20));
        seq
    };
    assert_eq!(script(), script());
}

#[test]
fn large_sets_suppress_thumbnails_and_fall_back_to_filenames() {
    let config = SlideshowConfig {
        large_set_threshold: 3,
        ..SlideshowConfig::new(1)
    };
    let mut show = Slideshow::new(abc(), config).unwrap();
    assert!(!show.is_large_set());
    assert!(!show.thumbnails_suppressed());
    assert!(
        show.browser_page(0, 10)
            .unwrap()
            .iter()
            .all(|e| e.show_thumbnail)
    );
    show.insert(3, slide(D)).unwrap();
    assert!(show.is_large_set());
    assert!(show.thumbnails_suppressed());
    let page = show.browser_page(1, 2).unwrap();
    let labels: Vec<_> = page
        .iter()
        .map(|e| (e.id, e.filename.as_str(), e.show_thumbnail))
        .collect();
    assert_eq!(labels, [(B, "photo2.jpg", false), (C, "photo3.jpg", false)]);
    show.remove(D).unwrap();
    assert!(!show.thumbnails_suppressed());
    assert_eq!(show.browser_page(3, 5).unwrap().len(), 0);
    assert_eq!(
        show.browser_page(4, 1),
        Err(SlideshowEditError::IndexOutOfRange { index: 4, len: 3 })
    );

    let forced = SlideshowConfig {
        suppress_thumbnails: true,
        ..SlideshowConfig::new(1)
    };
    let show = Slideshow::new(abc(), forced).unwrap();
    assert!(!show.is_large_set());
    assert!(show.thumbnails_suppressed());

    let show = Slideshow::new(many(2_000), SlideshowConfig::new(1)).unwrap();
    assert!(show.thumbnails_suppressed());
    assert_eq!(
        show.browser_page(0, usize::MAX).unwrap().len(),
        MAX_SLIDE_BROWSER_PAGE
    );
}

#[test]
fn construction_validates_slides_and_configuration() {
    let config = SlideshowConfig::new(1);
    assert_eq!(
        Slideshow::new(Vec::new(), config),
        Err(SlideshowError::Empty)
    );
    assert_eq!(
        Slideshow::new(vec![slide(A), slide(A)], config),
        Err(SlideshowError::DuplicateSlide(A))
    );
    assert_eq!(
        Slideshow::new(vec![slide(A).with_duration(0)], config),
        Err(SlideshowError::InvalidDuration(A))
    );
    assert_eq!(
        Slideshow::new(vec![Slide::new(A, "a.png", 0, 10)], config),
        Err(SlideshowError::InvalidSourceSize(A))
    );
    let outside = KenBurns {
        start: PixelRect::new(0, 0, 100, 100),
        end: PixelRect::new(60, 0, 50, 50),
    };
    assert_eq!(
        Slideshow::new(vec![slide(A).with_ken_burns(outside)], config),
        Err(SlideshowError::CropOutOfBounds {
            slide: A,
            edge: RectEdge::End
        })
    );
    let empty = KenBurns {
        start: PixelRect::new(0, 0, 0, 10),
        end: PixelRect::new(0, 0, 10, 10),
    };
    assert_eq!(
        Slideshow::new(vec![slide(A).with_ken_burns(empty)], config),
        Err(SlideshowError::CropOutOfBounds {
            slide: A,
            edge: RectEdge::Start
        })
    );
    assert_eq!(
        Slideshow::new(abc(), SlideshowConfig::new(0)),
        Err(SlideshowError::InvalidDefaultDuration)
    );
    for frames in [0, MAX_SLIDE_TRANSITION_FRAMES + 1] {
        let config = SlideshowConfig {
            transition: SlideTransition::Crossfade { frames },
            ..SlideshowConfig::new(1)
        };
        assert_eq!(
            Slideshow::new(abc(), config),
            Err(SlideshowError::InvalidTransition { frames })
        );
    }
    let config = SlideshowConfig {
        large_set_threshold: 0,
        ..SlideshowConfig::new(1)
    };
    assert_eq!(
        Slideshow::new(abc(), config),
        Err(SlideshowError::ZeroLargeSetThreshold)
    );
}

#[test]
fn folder_listing_filters_extensions_and_sorts_naturally() {
    let names = [
        "IMG10.JPG",
        "img2.jpeg",
        "notes.txt",
        "img1.png",
        "Img02.webp",
        "scan.TIFF",
        "a.Gif",
        "b.bmp",
        "c.tif",
        ".jpg",
        "noext",
        "movie.mp4",
    ];
    let listing = ImageFolderListing::from_names(&names).unwrap();
    assert_eq!(
        listing.images(),
        [
            "a.Gif",
            "b.bmp",
            "c.tif",
            "img1.png",
            "img2.jpeg",
            "Img02.webp",
            "IMG10.JPG",
            "scan.TIFF"
        ]
    );
    assert_eq!(listing.skipped(), 4);
    assert!(is_supported_image("x.JpEg"));
    assert!(!is_supported_image("x.jpg.txt"));
    assert_eq!(
        natural_cmp("frame9.png", "frame10.png"),
        core::cmp::Ordering::Less
    );
    assert_eq!(natural_cmp("a.png", "A.png"), "a.png".cmp("A.png"));
    assert_eq!(
        natural_cmp(
            "x99999999999999999999999.png",
            "x100000000000000000000000.png"
        ),
        core::cmp::Ordering::Less
    );
}

#[test]
fn folder_listing_is_independent_of_input_order() {
    let mut names: Vec<String> = (0..200).map(|i| format!("shot{i}.jpg")).collect();
    let sorted = ImageFolderListing::from_names(&names).unwrap();
    names.reverse();
    assert_eq!(ImageFolderListing::from_names(&names).unwrap(), sorted);
    assert_eq!(sorted.images()[0], "shot0.jpg");
    assert_eq!(sorted.images()[10], "shot10.jpg");
}

#[test]
fn folder_listing_rejects_invalid_names() {
    assert_eq!(
        ImageFolderListing::from_names(&["a.jpg", ""]),
        Err(FolderListingError::EmptyName { index: 1 })
    );
    assert_eq!(
        ImageFolderListing::from_names(&["dir/a.jpg"]),
        Err(FolderListingError::InvalidName { index: 0 })
    );
    assert_eq!(
        ImageFolderListing::from_names(&["a\\b.png"]),
        Err(FolderListingError::InvalidName { index: 0 })
    );
    assert_eq!(
        ImageFolderListing::from_names(&["a.jpg", "a.jpg"]),
        Err(FolderListingError::DuplicateName("a.jpg".to_owned()))
    );
    let long = format!("{}.jpg", "x".repeat(1_100));
    assert_eq!(
        ImageFolderListing::from_names(&[long.as_str()]),
        Err(FolderListingError::NameTooLong {
            index: 0,
            bytes: 1_104
        })
    );
    let too_many = vec!["x"; MAX_FOLDER_ENTRIES + 1];
    assert_eq!(
        ImageFolderListing::from_names(&too_many),
        Err(FolderListingError::TooManyEntries {
            count: MAX_FOLDER_ENTRIES + 1,
            max: MAX_FOLDER_ENTRIES
        })
    );
}

#[test]
fn folder_listing_feeds_an_ordered_slideshow() {
    let listing =
        ImageFolderListing::from_names(&["b10.png", "readme.md", "b9.png", "b1.png"]).unwrap();
    let slides: Vec<_> = listing
        .images()
        .iter()
        .zip(1..)
        .map(|(name, id)| Slide::new(SlideId::new(id), name.clone(), 640, 480))
        .collect();
    let mut show = Slideshow::new(slides, SlideshowConfig::new(1)).unwrap();
    let names: Vec<_> = ids(&mut show, 3)
        .into_iter()
        .map(|id| {
            show.slides()
                .iter()
                .find(|s| s.id() == id)
                .unwrap()
                .filename()
                .to_owned()
        })
        .collect();
    assert_eq!(names, ["b1.png", "b9.png", "b10.png"]);
}
