//! Ticker crawl/roll and scoreboard tests (GX-003 slice).
//!
//! Timing, splicing, and scoreboard tests use exact integer expectations and
//! need no font. Pixel tests need a real font file: they look for
//! `FM_TITLES_FONT`, then well known system font paths, then scan the system
//! font roots, and skip when none is found unless `FM_REQUIRE_FONT=1`.

use fm_titles::{
    Alignment, AssetCatalog, Bounds, ClockDirection, ClockFormat, Color, Crawl, CrawlConfig,
    CrawlError, CrawlItem, CrawlLayer, CrawlMeasure, CrawlMode, CrawlSegment, CrawlSegmentKind,
    CrawlSpeed, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
    FontCrawlMeasure, FontStyle, GameClockConfig, HorizontalAlignment, MAX_CRAWL_ITEMS,
    MAX_ROSTER_PLAYERS, Player, ReferenceRenderer, Scoreboard, ScoreboardAction, ScoreboardBinding,
    ScoreboardError, ScoreboardEvent, ScoreboardPreset, ScoreboardRules, Style, TeamSide,
    TeamState, TemplateId, TitleId, TitleScene, TitleTemplate, UpdateError, VerticalAlignment,
    actions_from_external,
};
use fm_types::FrameRate;
use std::fs;
use std::num::NonZeroU128;
use std::path::{Path, PathBuf};

const FAMILY: &str = "crawl-face";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn field(value: u128) -> FieldId {
    FieldId::new(nz(value))
}

/// Ten pixels per character: a font-free measure for timing tests.
struct FixedMeasure(u32);

impl CrawlMeasure for FixedMeasure {
    fn measure(&self, text: &str) -> u32 {
        u32::try_from(text.chars().count()).unwrap() * self.0
    }
}

fn crawl_config(mode: CrawlMode, bounds: Bounds, pixels: u32, fps: (u32, u32)) -> CrawlConfig {
    CrawlConfig {
        mode,
        bounds,
        speed: CrawlSpeed::new(pixels, 1).unwrap(),
        frame_rate: FrameRate::new(fps.0, fps.1).unwrap(),
        start_frame: 10,
        separator: "|".into(),
        gap_px: 5,
        font: FontStyle {
            family: FAMILY.into(),
            size_px: 20,
        },
        fill: Color::new(255, 255, 255, 255),
        background: None,
        opacity: 255,
        alignment: Alignment::default(),
    }
}

/// 200 px wide crawl, 4 px/frame, items "AAAA" (40) and "BB" (20) with a
/// `5 | 5` separator group (20): steps 60 and 40, cycle 100.
fn sample_crawl() -> Crawl {
    let config = crawl_config(
        CrawlMode::Crawl,
        Bounds {
            x: 0,
            y: 0,
            width: 200,
            height: 20,
        },
        120,
        (30, 1),
    );
    Crawl::new(
        ElementId::new(nz(7)),
        config,
        &["AAAA", "BB"],
        &FixedMeasure(10),
    )
    .unwrap()
}

fn summary(segments: &[CrawlSegment<'_>]) -> Vec<(String, i64, u64)> {
    segments
        .iter()
        .map(|segment| (segment.text.to_owned(), segment.offset, segment.revision))
        .collect()
}

// ---------------------------------------------------------------------------
// Crawl timing
// ---------------------------------------------------------------------------

#[test]
fn distance_is_an_exact_rational_function_of_the_frame_index() {
    let crawl = sample_crawl();
    let config = crawl.config();
    assert_eq!(config.distance_at(0), 0);
    assert_eq!(config.distance_at(10), 0);
    assert_eq!(config.distance_at(11), 4);
    assert_eq!(config.distance_at(60), 200);

    // 25 px/s at 29.97 fps: floor(n * 25 * 1001 / 30000).
    let mut ntsc = config.clone();
    ntsc.speed = CrawlSpeed::new(25, 1).unwrap();
    ntsc.frame_rate = FrameRate::new(30_000, 1_001).unwrap();
    assert_eq!(ntsc.distance_at(11), 0);
    assert_eq!(ntsc.distance_at(12), 1);
    assert_eq!(ntsc.distance_at(40), 25);
    assert_eq!(ntsc.distance_at(10 + 30_000), 25_025);
    assert_eq!(ntsc.first_frame_reaching(1), 12);
    for distance in [1, 2, 25, 26, 999, 25_025] {
        let frame = ntsc.first_frame_reaching(distance);
        assert!(ntsc.distance_at(frame) >= distance);
        assert!(ntsc.distance_at(frame - 1) < distance);
    }

    // Speeds are normalized rationals.
    let half = CrawlSpeed::new(90, 2).unwrap();
    assert_eq!((half.pixels(), half.seconds()), (45, 1));
    assert_eq!(CrawlSpeed::new(0, 1), Err(CrawlError::ZeroSpeed));
}

#[test]
fn crawl_right_enters_from_the_left_edge_and_moves_right() {
    let config = crawl_config(
        CrawlMode::CrawlRight,
        Bounds {
            x: 0,
            y: 0,
            width: 200,
            height: 20,
        },
        120,
        (30, 1),
    );
    let crawl = Crawl::new(
        ElementId::new(nz(7)),
        config,
        &["AAAA", "BB"],
        &FixedMeasure(10),
    )
    .unwrap();
    assert_eq!(
        summary(&crawl.view(10).unwrap().segments),
        vec![("AAAA".into(), 0, 0)]
    );
    assert_eq!(
        summary(&crawl.view(11).unwrap().segments),
        vec![("AAAA".into(), 4, 0)]
    );
    // At distance 40 the separator's right edge enters the left edge.
    assert_eq!(
        summary(&crawl.view(20).unwrap().segments),
        vec![("|".into(), -5, 0), ("AAAA".into(), 40, 0)]
    );
}

#[test]
fn roll_down_moves_items_top_to_bottom() {
    let config = crawl_config(
        CrawlMode::RollDown,
        Bounds {
            x: 0,
            y: 0,
            width: 100,
            height: 200,
        },
        120,
        (30, 1),
    );
    let crawl = Crawl::new(
        ElementId::new(nz(7)),
        config,
        &["AAAA", "BB"],
        &FixedMeasure(10),
    )
    .unwrap();
    assert_eq!(
        summary(&crawl.view(10).unwrap().segments),
        vec![("AAAA".into(), 0, 0)]
    );
    assert_eq!(crawl.view(11).unwrap().segments[0].offset, 4);
}

#[test]
fn styled_crawl_items_keep_their_own_fill() {
    let config = crawl_config(
        CrawlMode::CrawlRight,
        Bounds {
            x: 0,
            y: 0,
            width: 200,
            height: 20,
        },
        120,
        (30, 1),
    );
    let red = Color::new(255, 0, 0, 255);
    let blue = Color::new(0, 0, 255, 255);
    let mut crawl = Crawl::new_styled(
        ElementId::new(nz(7)),
        config,
        &[
            CrawlItem::with_fill("AAAA", red),
            CrawlItem::with_fill("BB", blue),
        ],
        &FixedMeasure(10),
    )
    .unwrap();
    let view = crawl.view(10).unwrap();
    assert_eq!(view.segments.len(), 1);
    assert_eq!(view.segments[0].fill, Some(red));
    // The second item enters at distance 60 (frame 25) with its own fill.
    let view = crawl.view(30).unwrap();
    assert!(
        view.segments
            .iter()
            .any(|segment| segment.fill == Some(blue))
    );
    // An update accepts styled items and advances the revision.
    let schedule = crawl
        .set_styled_items(
            0,
            31,
            &[CrawlItem::text("CC"), CrawlItem::with_fill("DD", blue)],
            &FixedMeasure(10),
        )
        .unwrap();
    assert_eq!(schedule.revision, 1);
}

#[test]
fn crawl_enters_from_the_right_edge_and_wraps_seamlessly_with_separators() {
    let crawl = sample_crawl();
    assert_eq!(crawl.view(10).unwrap().segments, Vec::new());
    assert_eq!(
        summary(&crawl.view(11).unwrap().segments),
        vec![("AAAA".into(), 196, 0)]
    );
    assert_eq!(crawl.cycle_length(), 100);

    let view = crawl.view(60).unwrap();
    assert_eq!(view.distance, 200);
    let kinds: Vec<_> = view.segments.iter().map(|segment| segment.kind).collect();
    assert_eq!(
        kinds,
        vec![
            CrawlSegmentKind::Item { index: 0 },
            CrawlSegmentKind::Separator,
            CrawlSegmentKind::Item { index: 1 },
            CrawlSegmentKind::Separator,
            CrawlSegmentKind::Item { index: 0 },
            CrawlSegmentKind::Separator,
            CrawlSegmentKind::Item { index: 1 },
            CrawlSegmentKind::Separator,
        ]
    );
    let offsets: Vec<_> = view.segments.iter().map(|segment| segment.offset).collect();
    assert_eq!(offsets, vec![0, 45, 60, 85, 100, 145, 160, 185]);

    // Every frame moves everything exactly 4 px; a partly visible item keeps
    // its leading edge off screen.
    let next = crawl.view(61).unwrap();
    assert_eq!(next.segments[0].offset, -4);
    assert_eq!(next.segments[0].text, "AAAA");
    // One full cycle later the picture repeats exactly.
    assert_eq!(
        summary(&crawl.view(61).unwrap().segments),
        summary(&crawl.view(61 + 25).unwrap().segments)
    );
}

#[test]
fn live_updates_splice_at_the_next_item_boundary_without_moving_visible_text() {
    let original = sample_crawl();
    let mut crawl = original.clone();
    let before = summary(&crawl.view(61).unwrap().segments);

    // d(61) = 204. The next item start at or beyond 204 is "BB" at 260.
    let schedule = crawl
        .set_items(0, 61, &["CCC", "DD", "E"], &FixedMeasure(10))
        .unwrap();
    assert_eq!(schedule.revision, 1);
    assert_eq!(schedule.boundary, 260);
    assert_eq!(schedule.start_index, 1, "the list continues at position 1");
    assert_eq!(schedule.enters_at_frame, 76);

    // Nothing on screen changes until the new list's first item enters.
    assert_eq!(summary(&crawl.view(61).unwrap().segments), before);
    for frame in 61..76 {
        assert_eq!(
            crawl.view(frame).unwrap(),
            original.view(frame).unwrap(),
            "frame {frame} jumped"
        );
    }

    let entered = crawl.view(76).unwrap();
    let last = entered.segments.last().unwrap();
    assert_eq!(last.kind, CrawlSegmentKind::Item { index: 1 });
    assert_eq!((last.text, last.offset, last.revision), ("DD", 196, 1));
    let unchanged = original.view(76).unwrap();
    assert_eq!(
        summary(&entered.segments[..entered.segments.len() - 1]),
        summary(&unchanged.segments[..unchanged.segments.len() - 1])
    );

    // Once the old list has left, the new one wraps on its own.
    let later = crawl.view(136).unwrap();
    let texts: Vec<_> = later
        .segments
        .iter()
        .filter(|segment| segment.kind != CrawlSegmentKind::Separator)
        .map(|segment| segment.text)
        .collect();
    assert_eq!(texts, vec!["E", "CCC", "DD", "E", "CCC", "DD"]);
}

#[test]
fn updates_before_the_boundary_replace_the_pending_list_and_old_lists_are_pruned() {
    let mut crawl = sample_crawl();
    crawl.set_items(0, 61, &["CCC"], &FixedMeasure(10)).unwrap();
    let schedule = crawl
        .set_items(1, 62, &["XY", "Z"], &FixedMeasure(10))
        .unwrap();
    assert_eq!((schedule.boundary, schedule.start_index), (260, 1));
    assert_eq!(crawl.live_epochs(), 2);
    assert_eq!(crawl.view(76).unwrap().segments.last().unwrap().text, "Z");

    // By d(200) = 760 the original list has left the 200 px viewport.
    crawl.set_items(2, 200, &["Q"], &FixedMeasure(10)).unwrap();
    assert_eq!(crawl.live_epochs(), 2);

    // Stale revisions and earlier frames are refused without changes.
    assert_eq!(
        crawl.set_items(2, 200, &["R"], &FixedMeasure(10)),
        Err(CrawlError::RevisionConflict {
            expected: 2,
            actual: 3
        })
    );
    assert_eq!(
        crawl.set_items(3, 199, &["R"], &FixedMeasure(10)),
        Err(CrawlError::FrameRegressed {
            frame: 199,
            last_frame: 200
        })
    );
    assert_eq!(
        crawl.view(150),
        Err(CrawlError::FrameRegressed {
            frame: 150,
            last_frame: 200
        })
    );
}

#[test]
fn an_emptied_crawl_runs_out_and_a_refill_enters_at_the_edge() {
    let mut crawl = sample_crawl();
    crawl.set_items(0, 61, &[], &FixedMeasure(10)).unwrap();
    assert_eq!(crawl.view(200).unwrap().segments, Vec::new());

    // d(200) = 760: new content's leading edge starts on the right edge.
    let schedule = crawl
        .set_items(1, 200, &["NEW"], &FixedMeasure(10))
        .unwrap();
    assert_eq!((schedule.boundary, schedule.enters_at_frame), (760, 201));
    assert_eq!(crawl.view(200).unwrap().segments, Vec::new());
    assert_eq!(
        summary(&crawl.view(201).unwrap().segments),
        vec![("NEW".into(), 196, 2)]
    );
}

#[test]
fn roll_moves_items_bottom_to_top() {
    let mut config = crawl_config(
        CrawlMode::Roll,
        Bounds {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        },
        30,
        (30, 1),
    );
    config.start_frame = 0;
    config.separator = String::new();
    config.gap_px = 3;
    // Every item measures one 12 px line: steps 15, cycle 30.
    let crawl = Crawl::new(
        ElementId::new(nz(1)),
        config,
        &["X", "Y"],
        &FixedMeasure(12),
    )
    .unwrap();
    assert_eq!(crawl.cycle_length(), 30);
    assert_eq!(
        summary(&crawl.view(20).unwrap().segments),
        vec![("X".into(), 30, 0), ("Y".into(), 45, 0)]
    );
    assert_eq!(
        summary(&crawl.view(80).unwrap().segments),
        vec![
            ("X".into(), 0, 0),
            ("Y".into(), 15, 0),
            ("X".into(), 30, 0),
            ("Y".into(), 45, 0),
        ]
    );
}

#[test]
fn crawl_inputs_are_bounded_and_validated() {
    let base = sample_crawl().config().clone();
    let measure = FixedMeasure(10);
    let id = ElementId::new(nz(1));
    assert_eq!(
        Crawl::new(id, base.clone(), &["ok", "  "], &measure).unwrap_err(),
        CrawlError::BlankItem { index: 1 }
    );
    assert_eq!(
        Crawl::new(id, base.clone(), &["line\nbreak"], &measure).unwrap_err(),
        CrawlError::ControlCharacter { index: Some(0) }
    );
    let too_many = vec!["x"; MAX_CRAWL_ITEMS + 1];
    assert!(matches!(
        Crawl::new(id, base.clone(), &too_many, &measure),
        Err(CrawlError::TooManyItems { .. })
    ));
    let long = "x".repeat(fm_titles::MAX_CRAWL_ITEM_CHARS + 1);
    assert!(matches!(
        Crawl::new(id, base.clone(), &[long.as_str()], &measure),
        Err(CrawlError::ItemTooLong { index: 0, .. })
    ));
    let mut empty = base.clone();
    empty.bounds.width = 0;
    assert_eq!(
        Crawl::new(id, empty, &["x"], &measure).unwrap_err(),
        CrawlError::EmptyViewport
    );
    let mut gap = base;
    gap.gap_px = fm_titles::MAX_CRAWL_GAP_PX + 1;
    assert!(matches!(
        Crawl::new(id, gap, &["x"], &measure),
        Err(CrawlError::GapTooLarge { .. })
    ));
    // A zero-width measurement still advances the tape by one pixel.
    let crawl = Crawl::new(
        id,
        sample_crawl().config().clone(),
        &["x"],
        &FixedMeasure(0),
    )
    .unwrap();
    // Item 1 px, separator 0 px, two 5 px gaps.
    assert_eq!(crawl.cycle_length(), 11);
}

// ---------------------------------------------------------------------------
// Crawl rendering
// ---------------------------------------------------------------------------

const DIRECT_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "C:\\Windows\\Fonts\\arial.ttf",
];

const FONT_ROOTS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/Library/Fonts",
    "/System/Library/Fonts",
    "C:\\Windows\\Fonts",
];

fn catalog() -> Option<AssetCatalog> {
    let bytes = discover_font();
    assert!(
        bytes.is_some() || std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
        "FM_REQUIRE_FONT=1 but no usable font file was found"
    );
    let Some(bytes) = bytes else {
        eprintln!("skipping crawl rendering: no font file available");
        return None;
    };
    Some(
        AssetCatalog::new()
            .with_font_bytes(FAMILY, bytes)
            .expect("system font parses"),
    )
}

fn discover_font() -> Option<Vec<u8>> {
    if let Ok(path) = std::env::var("FM_TITLES_FONT") {
        return fs::read(path).ok();
    }
    DIRECT_FONTS
        .iter()
        .find_map(|path| fs::read(path).ok())
        .or_else(|| {
            FONT_ROOTS
                .iter()
                .find_map(|root| scan_for_font(Path::new(root), 0))
        })
}

fn scan_for_font(directory: &Path, depth: u32) -> Option<Vec<u8>> {
    if depth > 4 {
        return None;
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries.iter().filter(|path| path.is_file()) {
        let usable = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("ttf") || extension.eq_ignore_ascii_case("otf")
            });
        if usable && let Ok(bytes) = fs::read(path) {
            return Some(bytes);
        }
    }
    entries
        .iter()
        .filter(|path| path.is_dir())
        .find_map(|path| scan_for_font(path, depth + 1))
}

fn template(
    width: u32,
    height: u32,
    fields: Vec<FieldDefinition>,
    elements: Vec<Element>,
) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(nz(1)),
        name: "crawl template".into(),
        width,
        height,
        background: Color::TRANSPARENT,
        fields,
        elements,
        animation: fm_titles::TitleAnimation::default(),
    }
}

fn scene(template: &TitleTemplate) -> TitleScene {
    template
        .instantiate(TitleId::new(nz(1)))
        .expect("valid template")
}

/// A scene holding one text element drawing `text` exactly where a crawl or
/// roll segment would be drawn.
fn reference_scene(
    width: u32,
    height: u32,
    text: &str,
    bounds: Bounds,
    alignment: Alignment,
) -> TitleScene {
    scene(&template(
        width,
        height,
        vec![FieldDefinition {
            id: field(1),
            name: "text".into(),
            default: FieldValue::Text(text.into()),
            exposed: true,
        }],
        vec![Element {
            id: ElementId::new(nz(1)),
            name: "reference".into(),
            bounds,
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::new(255, 255, 255, 255),
                background: None,
                opacity: 255,
                font: Some(FontStyle {
                    family: FAMILY.into(),
                    size_px: 20,
                }),
                alignment,
            },
            color_field: None,
            kind: ElementKind::Text { field: field(1) },
        }],
    ))
}

fn render_crawl(
    crawl: &Crawl,
    frame: u64,
    assets: &AssetCatalog,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let empty = scene(&template(width, height, Vec::new(), Vec::new()));
    let output = ReferenceRenderer
        .render_with_crawls(&empty, 0, assets, &[CrawlLayer { crawl, frame }])
        .unwrap();
    assert_eq!(output.report.missing_fonts, Vec::new());
    assert_eq!(output.report.degraded, Vec::new());
    output.frame.pixels().to_vec()
}

fn render_crawl_config(assets: &AssetCatalog) -> CrawlConfig {
    let mut config = crawl_config(
        CrawlMode::Crawl,
        Bounds {
            x: 10,
            y: 8,
            width: 300,
            height: 40,
        },
        120,
        (30, 1),
    );
    config.start_frame = 0;
    config.separator = String::new();
    // Wide enough that only one copy of the item is ever on screen.
    config.gap_px = 400;
    config.alignment.vertical = VerticalAlignment::Center;
    assert!(assets.font(FAMILY).is_some());
    config
}

#[test]
fn crawl_text_lands_on_the_exact_pixel_position() {
    let Some(assets) = catalog() else { return };
    let config = render_crawl_config(&assets);
    let measure = FontCrawlMeasure::new(&config, &assets).unwrap();
    let crawl = Crawl::new(ElementId::new(nz(9)), config.clone(), &["HELLO"], &measure).unwrap();
    let width = measure.measure("HELLO");
    assert!(width > 20 && width < 140, "unexpected width {width}");
    assert_eq!(crawl.cycle_length(), u64::from(width) + 400);

    // Frame 40: distance 160, leading edge at viewport x 140 = canvas x 150.
    let view = crawl.view(40).unwrap();
    assert_eq!(summary(&view.segments), vec![("HELLO".into(), 140, 0)]);
    let crawl_pixels = render_crawl(&crawl, 40, &assets, 320, 56);
    let reference = reference_scene(
        320,
        56,
        "HELLO",
        Bounds {
            x: 150,
            y: 8,
            width: 100_000,
            height: 40,
        },
        config.alignment,
    );
    let expected = ReferenceRenderer.render(&reference, 0, &assets).unwrap();
    assert!(crawl_pixels.iter().any(|channel| *channel != 0));
    assert_eq!(crawl_pixels, expected.frame.pixels());

    // Five frames later every pixel has moved exactly 20 px left.
    let later = render_crawl(&crawl, 45, &assets, 320, 56);
    let stride = 320 * 4;
    for y in 0..56 {
        for x in 10..280 {
            let moved = &later[y * stride + x * 4..][..4];
            let source = &crawl_pixels[y * stride + (x + 20) * 4..][..4];
            assert_eq!(moved, source, "pixel ({x}, {y})");
        }
    }

    // Rendering is a pure function of the frame index.
    assert_eq!(render_crawl(&crawl, 40, &assets, 320, 56), crawl_pixels);
}

#[test]
fn crawl_is_clipped_to_its_viewport() {
    let Some(assets) = catalog() else { return };
    let config = render_crawl_config(&assets);
    let measure = FontCrawlMeasure::new(&config, &assets).unwrap();
    let crawl = Crawl::new(ElementId::new(nz(9)), config, &["HELLO"], &measure).unwrap();
    // Frame 80: leading edge at viewport x -20, straddling the left edge.
    assert_eq!(crawl.view(80).unwrap().segments[0].offset, -20);
    let pixels = render_crawl(&crawl, 80, &assets, 320, 56);
    let stride = 320 * 4;
    let mut inked = false;
    for y in 0..56 {
        for x in 0..320 {
            let alpha = pixels[y * stride + x * 4 + 3];
            let inside = (10..310).contains(&x) && (8..48).contains(&y);
            assert!(inside || alpha == 0, "ink outside viewport at ({x}, {y})");
            inked |= alpha != 0;
        }
    }
    assert!(inked);
}

#[test]
fn roll_text_lands_on_the_exact_pixel_position() {
    let Some(assets) = catalog() else { return };
    let mut config = crawl_config(
        CrawlMode::Roll,
        Bounds {
            x: 10,
            y: 0,
            width: 200,
            height: 100,
        },
        30,
        (30, 1),
    );
    config.start_frame = 0;
    config.separator = String::new();
    config.gap_px = 200;
    config.alignment.horizontal = HorizontalAlignment::Center;
    let measure = FontCrawlMeasure::new(&config, &assets).unwrap();
    let crawl = Crawl::new(ElementId::new(nz(3)), config.clone(), &["HELLO"], &measure).unwrap();
    // Frame 50: distance 50, line box top at y 50.
    assert_eq!(crawl.view(50).unwrap().segments[0].offset, 50);
    let pixels = render_crawl(&crawl, 50, &assets, 220, 100);
    let reference = reference_scene(
        220,
        100,
        "HELLO",
        Bounds {
            x: 10,
            y: 50,
            width: 200,
            height: 100,
        },
        config.alignment,
    );
    let expected = ReferenceRenderer.render(&reference, 0, &assets).unwrap();
    assert!(pixels.iter().any(|channel| *channel != 0));
    assert_eq!(pixels, expected.frame.pixels());
}

#[test]
fn a_crawl_without_its_font_reports_it_and_draws_only_its_background() {
    let mut config = sample_crawl().config().clone();
    config.background = Some(Color::new(0, 0, 255, 255));
    config.bounds = Bounds {
        x: 0,
        y: 0,
        width: 4,
        height: 2,
    };
    let crawl = Crawl::new(ElementId::new(nz(5)), config, &["A"], &FixedMeasure(10)).unwrap();
    let empty = scene(&template(4, 4, Vec::new(), Vec::new()));
    let output = ReferenceRenderer
        .render_with_crawls(
            &empty,
            0,
            &AssetCatalog::new(),
            &[CrawlLayer {
                crawl: &crawl,
                frame: 11,
            }],
        )
        .unwrap();
    assert_eq!(output.report.missing_fonts.len(), 1);
    assert_eq!(output.report.missing_fonts[0].family, FAMILY);
    assert_eq!(output.frame.pixels()[..4], [0, 0, 255, 255]);
    assert_eq!(output.frame.pixels()[4 * 4 * 2..][..4], [0, 0, 0, 0]);
    assert!(matches!(
        FontCrawlMeasure::new(crawl.config(), &AssetCatalog::new()),
        Err(CrawlError::MissingFont(_))
    ));
}

// ---------------------------------------------------------------------------
// Scoreboard
// ---------------------------------------------------------------------------

fn clock_config(direction: ClockDirection, fps: (u32, u32), initial_ms: u64) -> GameClockConfig {
    GameClockConfig {
        direction,
        format: ClockFormat::MinutesSeconds,
        frame_rate: FrameRate::new(fps.0, fps.1).unwrap(),
        initial_ms,
        limit_ms: None,
    }
}

fn board(clock: GameClockConfig) -> Scoreboard {
    Scoreboard::new(
        TeamState::new("Harbor City", "HBC"),
        TeamState::new("Valley United", "VAL"),
        ScoreboardRules {
            max_score: 99,
            max_period: 4,
            shot_clock_ms: Some(24_000),
        },
        clock,
    )
    .unwrap()
}

#[test]
fn score_and_period_shortcuts_saturate_and_sets_are_bounded() {
    let mut board = board(clock_config(ClockDirection::CountUp, (30, 1), 0));
    let outcome = board
        .apply(
            0,
            0,
            &[
                ScoreboardAction::SetScore {
                    side: TeamSide::Home,
                    score: 98,
                },
                ScoreboardAction::AdjustScore {
                    side: TeamSide::Home,
                    delta: 3,
                },
                ScoreboardAction::AdjustScore {
                    side: TeamSide::Away,
                    delta: 2,
                },
                ScoreboardAction::AdjustScore {
                    side: TeamSide::Away,
                    delta: -5,
                },
                ScoreboardAction::AdjustPeriod { delta: 2 },
                ScoreboardAction::AdjustPeriod { delta: 9 },
            ],
        )
        .unwrap();
    assert_eq!(outcome.revision, 1);
    assert_eq!(outcome.saturated, vec![1, 3, 5]);
    assert_eq!(board.team(TeamSide::Home).score, 99);
    assert_eq!(board.team(TeamSide::Away).score, 0);
    assert_eq!(board.period(), 4);

    // An out-of-range set fails the whole batch.
    let before = board.clone();
    assert_eq!(
        board.apply(
            1,
            5,
            &[
                ScoreboardAction::ResetScores,
                ScoreboardAction::SetScore {
                    side: TeamSide::Away,
                    score: 100,
                },
            ],
        ),
        Err(ScoreboardError::ScoreOutOfRange {
            side: TeamSide::Away,
            score: 100,
            maximum: 99
        })
    );
    assert_eq!(
        board.apply(1, 5, &[ScoreboardAction::SetPeriod { period: 0 }]),
        Err(ScoreboardError::PeriodOutOfRange {
            period: 0,
            maximum: 4
        })
    );
    assert_eq!(
        board.apply(0, 5, &[ScoreboardAction::ResetScores]),
        Err(ScoreboardError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    );
    assert_eq!(board, before);

    board.apply(1, 5, &[ScoreboardAction::ResetScores]).unwrap();
    assert_eq!(board.team(TeamSide::Home).score, 0);
    assert_eq!(
        board.apply(2, 4, &[]),
        Err(ScoreboardError::FrameRegressed {
            frame: 4,
            last_frame: 5
        })
    );
}

#[test]
fn countdown_expires_on_the_exact_frame_and_reports_once() {
    // 10 s at 29.97 fps: ceil(10000 * 30000 / 1001000) = 300 frames.
    let mut board = board(clock_config(
        ClockDirection::CountDown,
        (30_000, 1_001),
        10_000,
    ));
    board
        .apply(0, 100, &[ScoreboardAction::StartClock])
        .unwrap();
    assert_eq!(board.clock().expiry_frame(), Some(400));
    // 299 frames = floor(299 * 1001 / 30) = 9976 ms elapsed.
    assert_eq!(board.clock().value_ms(399), 24);
    assert_eq!(board.clock().display(399), "00:01");
    assert_eq!(board.clock().display(100), "00:10");
    assert_eq!(board.advance(399).unwrap(), Vec::new());
    assert_eq!(
        board.advance(400).unwrap(),
        vec![ScoreboardEvent::ClockExpired { frame: 400 }]
    );
    assert_eq!(board.advance(500).unwrap(), Vec::new());
    assert!(!board.clock().is_running());
    assert_eq!(board.clock().value_ms(500), 0);
    assert_eq!(board.clock().display(500), "00:00");

    assert_eq!(
        board.apply(1, 500, &[ScoreboardAction::StartClock]),
        Err(ScoreboardError::ClockExpired)
    );
    // Setting re-arms; a skipped-over expiry is reported by the next apply.
    board
        .apply(
            1,
            500,
            &[
                ScoreboardAction::SetClock { value_ms: 1_000 },
                ScoreboardAction::StartClock,
            ],
        )
        .unwrap();
    assert_eq!(board.clock().expiry_frame(), Some(530));
    let outcome = board
        .apply(
            2,
            900,
            &[ScoreboardAction::AdjustScore {
                side: TeamSide::Home,
                delta: 1,
            }],
        )
        .unwrap();
    assert_eq!(
        outcome.events,
        vec![ScoreboardEvent::ClockExpired { frame: 530 }]
    );
    assert_eq!(board.advance(901).unwrap(), Vec::new());
}

#[test]
fn start_stop_cycles_accumulate_exact_frames_without_drift() {
    let rate = (30_000, 1_001);
    let mut stuttered = board(clock_config(ClockDirection::CountUp, rate, 0));
    let mut revision = 0;
    // 500 one-frame runs separated by one stopped frame each.
    for run in 0..500_u64 {
        let start = run * 2;
        revision = stuttered
            .apply(revision, start, &[ScoreboardAction::StartClock])
            .unwrap()
            .revision;
        revision = stuttered
            .apply(revision, start + 1, &[ScoreboardAction::StopClock])
            .unwrap()
            .revision;
    }
    // floor(500 * 1001 / 30) = 16683 ms; per-run flooring would give 0.
    assert_eq!(stuttered.clock().value_ms(5_000), 16_683);
    assert_eq!(stuttered.clock().display(5_000), "00:16");
}

#[test]
fn count_up_clock_stops_at_its_limit() {
    let mut config = clock_config(ClockDirection::CountUp, (25, 1), 0);
    config.limit_ms = Some(5_000);
    config.format = ClockFormat::HoursMinutesSeconds;
    let mut board = board(config);
    board.apply(0, 0, &[ScoreboardAction::StartClock]).unwrap();
    assert_eq!(board.clock().expiry_frame(), Some(125));
    assert_eq!(board.clock().value_ms(124), 4_960);
    assert_eq!(
        board.advance(1_000).unwrap(),
        vec![ScoreboardEvent::ClockExpired { frame: 125 }]
    );
    assert_eq!(board.clock().value_ms(1_000), 5_000);
    assert_eq!(board.clock().display(1_000), "00:00:05");
}

#[test]
fn the_shot_clock_counts_down_and_reports_its_expiry_once() {
    let mut board = board(clock_config(ClockDirection::CountDown, (25, 1), 60_000));
    let outcome = board
        .apply(
            0,
            10,
            &[
                ScoreboardAction::SetShotClock { value_ms: 24_000 },
                ScoreboardAction::StartShotClock,
            ],
        )
        .unwrap();
    assert_eq!(outcome.revision, 1);
    // 24 s at 25 fps is 600 frames, so it expires at frame 610.
    assert_eq!(board.shot_clock().unwrap().expiry_frame(), Some(610));
    assert_eq!(board.shot_clock().unwrap().value_ms(10), 24_000);
    assert_eq!(board.shot_clock().unwrap().display(10), "00:24");
    assert_eq!(board.advance(609).unwrap(), Vec::new());
    assert_eq!(
        board.advance(610).unwrap(),
        vec![ScoreboardEvent::ShotClockExpired { frame: 610 }]
    );
    assert_eq!(board.advance(611).unwrap(), Vec::new());
    // The external feed re-arms and starts it.
    let outcome = board
        .apply_external(
            1,
            620,
            &[("shot_clock.ms", "14000"), ("shot_clock.running", "true")],
        )
        .unwrap();
    assert_eq!(outcome.revision, 2);
    assert_eq!(board.shot_clock().unwrap().value_ms(620), 14_000);
}

#[test]
fn clock_formats_include_tenths() {
    let mut config = clock_config(ClockDirection::CountUp, (30, 1), 0);
    config.format = ClockFormat::MinutesSecondsTenths;
    let mut count_up = board(config);
    count_up
        .apply(0, 0, &[ScoreboardAction::StartClock])
        .unwrap();
    assert_eq!(count_up.clock().display(0), "00:00.0");
    assert_eq!(count_up.clock().display(3), "00:00.1");
    assert_eq!(count_up.clock().display(30), "00:01.0");
    assert_eq!(count_up.clock().display(35), "00:01.1");

    let mut config = clock_config(ClockDirection::CountDown, (30, 1), 90_000);
    config.format = ClockFormat::HoursMinutesSecondsTenths;
    let mut count_down = board(config);
    count_down
        .apply(0, 0, &[ScoreboardAction::StartClock])
        .unwrap();
    assert_eq!(count_down.clock().display(0), "00:01:30.0");
    assert_eq!(count_down.clock().display(30), "00:01:29.0");
    assert_eq!(count_down.clock().display(33), "00:01:28.9");
}

#[test]
#[allow(clippy::too_many_lines)] // roster add, duplicate, remove, and external keys
fn rosters_add_select_and_remove_players_atomically() {
    let mut board = board(GameClockConfig {
        direction: ClockDirection::CountDown,
        format: ClockFormat::MinutesSeconds,
        frame_rate: FrameRate::new(30, 1).unwrap(),
        initial_ms: 90_000,
        limit_ms: None,
    });
    let jane = Player {
        number: Some(23),
        name: "Jane Doe".into(),
    };
    let sam = Player {
        number: Some(7),
        name: "Sam Roe".into(),
    };
    let outcome = board
        .apply(
            0,
            0,
            &[
                ScoreboardAction::AddRosterPlayer {
                    side: TeamSide::Home,
                    player: jane.clone(),
                },
                ScoreboardAction::AddRosterPlayer {
                    side: TeamSide::Home,
                    player: sam.clone(),
                },
                ScoreboardAction::SelectRosterPlayer {
                    side: TeamSide::Home,
                    index: Some(1),
                },
            ],
        )
        .unwrap();
    assert_eq!(outcome.revision, 1);
    assert_eq!(
        board.team(TeamSide::Home).roster,
        vec![jane.clone(), sam.clone()]
    );
    assert_eq!(board.team(TeamSide::Home).player, Some(sam.clone()));

    // A duplicate jersey number is refused and the batch changes nothing.
    let before = board.clone();
    assert_eq!(
        board.apply(
            1,
            1,
            &[ScoreboardAction::AddRosterPlayer {
                side: TeamSide::Home,
                player: Player {
                    number: Some(23),
                    name: "Other".into(),
                },
            }],
        ),
        Err(ScoreboardError::DuplicateRosterPlayer {
            side: TeamSide::Home,
            player: "23 Other".into(),
        })
    );
    assert_eq!(board, before);

    // Removing the featured player clears the featured slot; bad indices fail.
    assert_eq!(
        board.apply(
            1,
            2,
            &[ScoreboardAction::RemoveRosterPlayer {
                side: TeamSide::Home,
                index: 9,
            }],
        ),
        Err(ScoreboardError::RosterIndexOutOfRange {
            side: TeamSide::Home,
            index: 9,
            len: 2,
        })
    );
    board
        .apply(
            1,
            2,
            &[ScoreboardAction::RemoveRosterPlayer {
                side: TeamSide::Home,
                index: 1,
            }],
        )
        .unwrap();
    assert_eq!(board.team(TeamSide::Home).roster, vec![jane]);
    assert_eq!(board.team(TeamSide::Home).player, None);

    // Selecting by index and clearing from the external feed.
    board
        .apply_external(
            2,
            3,
            &[("home.roster.select", "0"), ("away.roster.add", "9 Alex")],
        )
        .unwrap();
    assert_eq!(
        board.team(TeamSide::Home).player.as_ref().unwrap().number,
        Some(23)
    );
    assert_eq!(board.team(TeamSide::Away).roster.len(), 1);
    board
        .apply_external(3, 4, &[("home.roster.select", "")])
        .unwrap();
    assert_eq!(board.team(TeamSide::Home).player, None);
    assert_eq!(
        actions_from_external(&[("home.roster.add", "  ")]),
        Err(ScoreboardError::InvalidExternalValue { index: 0 })
    );
    assert_eq!(
        actions_from_external(&[("home.roster.select", "x")]),
        Err(ScoreboardError::InvalidExternalValue { index: 0 })
    );
}

#[test]
fn rosters_refuse_a_full_one() {
    // A full roster is refused.
    let mut full = TeamState::new("Home", "HOM");
    for number in 0..u16::try_from(MAX_ROSTER_PLAYERS).unwrap() {
        full.roster.push(Player {
            number: Some(number),
            name: format!("Player {number}"),
        });
    }
    let mut board = Scoreboard::new(
        full,
        TeamState::new("Away", "AWY"),
        ScoreboardRules {
            max_score: 99,
            max_period: 4,
            shot_clock_ms: Some(24_000),
        },
        GameClockConfig {
            direction: ClockDirection::CountDown,
            format: ClockFormat::MinutesSeconds,
            frame_rate: FrameRate::new(30, 1).unwrap(),
            initial_ms: 90_000,
            limit_ms: None,
        },
    )
    .unwrap();
    assert_eq!(
        board.apply(
            0,
            0,
            &[ScoreboardAction::AddRosterPlayer {
                side: TeamSide::Home,
                player: Player {
                    number: Some(99),
                    name: "Overflow".into(),
                },
            }],
        ),
        Err(ScoreboardError::RosterFull {
            side: TeamSide::Home,
            maximum: MAX_ROSTER_PLAYERS,
        })
    );
}

#[test]
fn a_player_can_be_featured_by_jersey_number() {
    let mut board = board(GameClockConfig {
        direction: ClockDirection::CountDown,
        format: ClockFormat::MinutesSeconds,
        frame_rate: FrameRate::new(30, 1).unwrap(),
        initial_ms: 90_000,
        limit_ms: None,
    });
    let jane = Player {
        number: Some(23),
        name: "Jane Doe".into(),
    };
    let sam = Player {
        number: Some(7),
        name: "Sam Roe".into(),
    };
    board
        .apply(
            0,
            0,
            &[
                ScoreboardAction::AddRosterPlayer {
                    side: TeamSide::Home,
                    player: jane,
                },
                ScoreboardAction::AddRosterPlayer {
                    side: TeamSide::Home,
                    player: sam.clone(),
                },
                ScoreboardAction::SelectPlayerByNumber {
                    side: TeamSide::Home,
                    number: 7,
                },
            ],
        )
        .unwrap();
    assert_eq!(board.team(TeamSide::Home).player, Some(sam));
    assert_eq!(
        board.apply(
            1,
            1,
            &[ScoreboardAction::SelectPlayerByNumber {
                side: TeamSide::Home,
                number: 99,
            }],
        ),
        Err(ScoreboardError::RosterNumberNotFound {
            side: TeamSide::Home,
            number: 99,
        })
    );
    board
        .apply_external(1, 2, &[("home.roster.number", "23")])
        .unwrap();
    assert_eq!(
        board.team(TeamSide::Home).player.as_ref().unwrap().number,
        Some(23)
    );
    board
        .apply(
            2,
            3,
            &[ScoreboardAction::ClearRoster {
                side: TeamSide::Home,
            }],
        )
        .unwrap();
    assert!(board.team(TeamSide::Home).roster.is_empty());
    assert_eq!(board.team(TeamSide::Home).player, None);
}

#[test]
fn scoreboard_snapshots_restore_exactly() {
    let mut board = board(GameClockConfig {
        direction: ClockDirection::CountDown,
        format: ClockFormat::MinutesSeconds,
        frame_rate: FrameRate::new(30, 1).unwrap(),
        initial_ms: 90_000,
        limit_ms: None,
    });
    board
        .apply(
            0,
            0,
            &[
                ScoreboardAction::SetScore {
                    side: TeamSide::Home,
                    score: 3,
                },
                ScoreboardAction::SetShotClock { value_ms: 14_000 },
                ScoreboardAction::StartShotClock,
                ScoreboardAction::StartClock,
            ],
        )
        .unwrap();
    board.advance(30).unwrap();
    let snapshot = board.snapshot();
    let restored = Scoreboard::restore(snapshot.clone()).unwrap();
    assert_eq!(restored, board);
    assert_eq!(restored.clock().display(30), "01:29");
    assert_eq!(
        restored.clock().expiry_frame(),
        board.clock().expiry_frame()
    );
    assert_eq!(
        restored.shot_clock().unwrap().value_ms(30),
        board.shot_clock().unwrap().value_ms(30)
    );

    // A running clock that starts after the last frame is refused.
    let mut broken = snapshot;
    broken.clock.running_since = Some(99);
    broken.last_frame = 30;
    assert_eq!(
        Scoreboard::restore(broken),
        Err(ScoreboardError::ClockSnapshotRunningAfterLastFrame {
            running_since: 99,
            last_frame: 30,
        })
    );
}

#[test]
fn the_shot_clock_duration_can_be_replaced() {
    let mut board = board(GameClockConfig {
        direction: ClockDirection::CountDown,
        format: ClockFormat::MinutesSeconds,
        frame_rate: FrameRate::new(30, 1).unwrap(),
        initial_ms: 90_000,
        limit_ms: None,
    });
    board
        .apply(
            0,
            0,
            &[ScoreboardAction::SetShotClockDuration { value_ms: 30_000 }],
        )
        .unwrap();
    assert_eq!(board.rules().shot_clock_ms, Some(30_000));
    assert_eq!(board.shot_clock().unwrap().config().initial_ms, 30_000);
    assert_eq!(board.shot_clock().unwrap().display(0), "00:30");
    board
        .apply(1, 1, &[ScoreboardAction::StartShotClock])
        .unwrap();
    assert_eq!(board.shot_clock().unwrap().expiry_frame(), Some(901));
    board
        .apply_external(2, 2, &[("shot_clock.duration", "24000")])
        .unwrap();
    assert_eq!(board.shot_clock().unwrap().display(2), "00:24");
    assert!(!board.shot_clock().unwrap().is_running());
}

#[test]
fn sport_presets_carry_their_rules_and_clocks() {
    let rate = FrameRate::new(25, 1).unwrap();
    let basketball = ScoreboardPreset::Basketball
        .board(
            TeamState::new("Home", "HOM"),
            TeamState::new("Away", "AWY"),
            rate,
        )
        .unwrap();
    assert_eq!(basketball.rules().max_period, 4);
    assert_eq!(basketball.rules().max_score, 199);
    assert_eq!(basketball.rules().shot_clock_ms, Some(24_000));
    assert_eq!(basketball.shot_clock().unwrap().config().initial_ms, 24_000);
    assert_eq!(basketball.clock().config().initial_ms, 600_000);
    assert_eq!(
        basketball.clock().config().direction,
        ClockDirection::CountDown
    );
    let soccer = ScoreboardPreset::Soccer
        .board(
            TeamState::new("Home", "HOM"),
            TeamState::new("Away", "AWY"),
            rate,
        )
        .unwrap();
    assert_eq!(soccer.rules().max_period, 2);
    assert_eq!(soccer.rules().shot_clock_ms, None);
    assert_eq!(soccer.shot_clock(), None);
    assert_eq!(soccer.clock().config().direction, ClockDirection::CountUp);
    assert_eq!(soccer.clock().config().limit_ms, Some(2_700_000));
    assert_eq!(
        soccer
            .clone()
            .apply(0, 0, &[ScoreboardAction::StartShotClock]),
        Err(ScoreboardError::NoShotClock)
    );
}

fn scoreboard_template() -> TitleTemplate {
    let text = |id, name: &str| FieldDefinition {
        id: field(id),
        name: name.into(),
        default: FieldValue::Text(String::new()),
        exposed: true,
    };
    template(
        64,
        16,
        vec![
            text(1, "home name"),
            text(2, "away name"),
            FieldDefinition {
                id: field(3),
                name: "home score".into(),
                default: FieldValue::Number(0),
                exposed: true,
            },
            text(4, "away score"),
            FieldDefinition {
                id: field(5),
                name: "period".into(),
                default: FieldValue::Number(1),
                exposed: true,
            },
            text(6, "clock"),
            text(7, "home player"),
        ],
        Vec::new(),
    )
}

fn binding() -> ScoreboardBinding {
    ScoreboardBinding {
        home_name: Some(field(1)),
        away_name: Some(field(2)),
        home_score: Some(field(3)),
        away_score: Some(field(4)),
        period: Some(field(5)),
        clock: Some(field(6)),
        home_player: Some(field(7)),
        ..ScoreboardBinding::default()
    }
}

#[test]
fn publishing_maps_typed_values_onto_template_fields_atomically() {
    let mut scene = scene(&scoreboard_template());
    let mut board = board(clock_config(ClockDirection::CountDown, (30, 1), 90_000));
    board
        .apply(
            0,
            0,
            &[
                ScoreboardAction::SetScore {
                    side: TeamSide::Home,
                    score: 3,
                },
                ScoreboardAction::SetScore {
                    side: TeamSide::Away,
                    score: 12,
                },
                ScoreboardAction::SetPlayer {
                    side: TeamSide::Home,
                    player: Some(Player {
                        number: Some(23),
                        name: "Jane Doe".into(),
                    }),
                },
                ScoreboardAction::StartClock,
            ],
        )
        .unwrap();

    assert_eq!(board.publish(&binding(), &mut scene, 0, 0), Ok(1));
    let value = |id| scene.field_value(field(id)).cloned().unwrap();
    assert_eq!(value(1), FieldValue::Text("Harbor City".into()));
    assert_eq!(value(2), FieldValue::Text("Valley United".into()));
    assert_eq!(value(3), FieldValue::Number(3));
    assert_eq!(value(4), FieldValue::Text("12".into()));
    assert_eq!(value(5), FieldValue::Number(1));
    assert_eq!(value(6), FieldValue::Text("01:30".into()));
    assert_eq!(value(7), FieldValue::Text("23 Jane Doe".into()));

    // Nothing changed within the same displayed second: no new revision.
    assert_eq!(board.publish(&binding(), &mut scene, 1, 29), Ok(1));
    // One second later only the clock field changes, in one revision.
    assert_eq!(
        board.field_updates(&binding(), &scene, 30),
        vec![(field(6), FieldValue::Text("01:29".into()))]
    );
    assert_eq!(board.publish(&binding(), &mut scene, 1, 30), Ok(2));

    // A bad binding fails atomically and leaves every field as it was.
    let snapshot = scene.clone();
    let mut wrong = binding();
    wrong.away_name = Some(field(3));
    wrong.home_score = None;
    board
        .apply(1, 60, &[ScoreboardAction::ResetScores])
        .unwrap();
    assert_eq!(
        board.publish(&wrong, &mut scene, 2, 60),
        Err(UpdateError::TypeMismatch {
            field: field(3),
            expected: fm_titles::FieldType::Number,
            actual: fm_titles::FieldType::Text,
        })
    );
    assert_eq!(scene, snapshot);
    assert_eq!(
        board.publish(&binding(), &mut scene, 1, 60),
        Err(UpdateError::StaleField {
            field: field(6),
            expected: 1,
            changed: 2,
        })
    );
}

#[test]
fn external_key_values_map_onto_typed_actions() {
    let mut board = board(clock_config(ClockDirection::CountDown, (30, 1), 0));
    let outcome = board
        .apply_external(
            0,
            10,
            &[
                ("home.score", "7"),
                ("away.score.delta", "-1"),
                ("away.score.delta", "4"),
                ("home.player", "23 Jane Doe"),
                ("away.player", "Sam"),
                ("away.abbreviation", "VLY"),
                ("period", "2"),
                ("clock.ms", "90000"),
                ("clock.running", "true"),
            ],
        )
        .unwrap();
    assert_eq!(outcome.saturated, vec![1]);
    assert_eq!(board.team(TeamSide::Home).score, 7);
    assert_eq!(board.team(TeamSide::Away).score, 4);
    assert_eq!(
        board.team(TeamSide::Home).player,
        Some(Player {
            number: Some(23),
            name: "Jane Doe".into()
        })
    );
    assert_eq!(
        board.team(TeamSide::Away).player.as_ref().unwrap().number,
        None
    );
    assert_eq!(board.team(TeamSide::Away).abbreviation, "VLY");
    assert_eq!(board.period(), 2);
    assert!(board.clock().is_running());
    assert_eq!(board.clock().display(40), "01:29");

    assert_eq!(
        actions_from_external(&[("home.score", "1"), ("home.score", "+7")]),
        Err(ScoreboardError::InvalidExternalValue { index: 1 })
    );
    assert_eq!(
        actions_from_external(&[("home.score", " 7")]),
        Err(ScoreboardError::InvalidExternalValue { index: 0 })
    );
    assert_eq!(
        actions_from_external(&[("referee", "x")]),
        Err(ScoreboardError::UnknownExternalKey { index: 0 })
    );
    assert_eq!(
        actions_from_external(&[("clock.running", "yes")]),
        Err(ScoreboardError::InvalidExternalValue { index: 0 })
    );
    // A failed external batch changes nothing.
    let before = board.clone();
    assert!(
        board
            .apply_external(1, 50, &[("home.score", "8"), ("period", "9")])
            .is_err()
    );
    assert_eq!(board, before);
}
