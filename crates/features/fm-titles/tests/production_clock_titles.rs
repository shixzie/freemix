//! Production clocks on titles: a title element bound to a named
//! `fm_clock` production clock shows exactly the registry's text at injected
//! monotonic and UTC instants.
//!
//! Text tests need no font. Frame tests render the clock element and, as the
//! oracle, a plain text element holding the expected string with the same
//! bounds and style; equal frames prove the clock drew exactly that text.
//! They need a real font file: `FM_TITLES_FONT`, then well-known system
//! paths, then a scan of the system font roots. They skip when none is found
//! unless `FM_REQUIRE_FONT=1`.

use std::fs;
use std::num::NonZeroU128;
use std::path::{Path, PathBuf};

use fm_clock::ClockTime;
use fm_clock::production::{
    ClockCommand, ClockName, ClockValue, DisplayFormat, DurationFormat, OverrunPolicy,
    ProductionClock, ProductionClocks, TimeOfDayFormat, TimeReading, TimeZone, TimerConfig,
    UtcInstant,
};
use fm_titles::{
    Alignment, AssetCatalog, Bounds, ClockDirection, ClockFormat, ClockSource, ClockSpec,
    ClockUnresolved, Color, Degradation, DegradedElement, Element, ElementId, ElementKind,
    FieldDefinition, FieldId, FieldValue, FontStyle, ProductionClockSpec, ReferenceRenderer, Style,
    TemplateId, TitleClock, TitleClockError, TitleClockKind, TitleClockTime, TitleId,
    TitleTemplate, ValidationError, evaluate_clock, evaluate_production_clock,
    title_clock_registry, validate_template, validate_title_clocks,
};

const FAMILY: &str = "test-face";
const WIDTH: u32 = 320;
const HEIGHT: u32 = 64;

fn name(text: &str) -> ClockName {
    ClockName::new(text).unwrap()
}

fn mono_ms(millis: u64) -> ClockTime {
    ClockTime::from_nanos(millis * 1_000_000)
}

fn utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> UtcInstant {
    UtcInstant::from_utc_civil(year, month, day, hour, minute, second, 0).unwrap()
}

/// A reading whose wall clock is irrelevant (timer tests).
fn at_ms(millis: u64) -> TimeReading {
    TimeReading::new(mono_ms(millis), utc(2026, 1, 1, 0, 0, 0))
}

/// A reading whose monotonic clock is irrelevant (time-of-day tests).
fn at_utc(instant: UtcInstant) -> TimeReading {
    TimeReading::new(mono_ms(0), instant)
}

/// `countdown` (10 s, overrun allowed, `MM:SS`), `hold` (10 s, stops at
/// target), and `eastern` (US Eastern by POSIX rule, 24-hour), with both
/// timers started at monotonic 0.
fn registry() -> ProductionClocks {
    let mut clocks = ProductionClocks::new();
    let ten = ClockValue::from_secs(10).unwrap();
    clocks
        .insert(
            name("countdown"),
            ProductionClock::timer(
                TimerConfig::countdown(ten, OverrunPolicy::Continue),
                DurationFormat::MinutesSeconds,
            )
            .unwrap(),
        )
        .unwrap();
    clocks
        .insert(
            name("hold"),
            ProductionClock::timer(
                TimerConfig::countdown(ten, OverrunPolicy::StopAtTarget),
                DurationFormat::MinutesSeconds,
            )
            .unwrap(),
        )
        .unwrap();
    clocks
        .insert(
            name("eastern"),
            ProductionClock::time_of_day(
                TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
                TimeOfDayFormat::Hours24,
            ),
        )
        .unwrap();
    clocks
        .apply(&name("countdown"), ClockCommand::Start, mono_ms(0))
        .unwrap();
    clocks
        .apply(&name("hold"), ClockCommand::Start, mono_ms(0))
        .unwrap();
    clocks
}

fn text(clocks: &ProductionClocks, spec: &ProductionClockSpec, at: TimeReading) -> String {
    evaluate_production_clock(spec, ClockSource::new(clocks, at)).unwrap()
}

// ---------------------------------------------------------------------------
// Text (font-free)
// ---------------------------------------------------------------------------

#[test]
fn countdown_text_counts_to_zero_at_expiry_then_overruns_negative() {
    let mut clocks = registry();
    let spec = ProductionClockSpec::new(name("countdown"));
    let cases = [
        (0, "00:10"),
        (1, "00:10"),
        (999, "00:10"),
        (1_000, "00:09"),
        (9_000, "00:01"),
        (9_999, "00:01"),
        (10_000, "00:00"),
        (10_999, "00:00"),
        (11_000, "-00:01"),
        (75_000, "-01:05"),
    ];
    for (millis, expected) in cases {
        assert_eq!(text(&clocks, &spec, at_ms(millis)), expected, "{millis}ms");
    }
    // Expiry is a reading state too: the same instant the text reaches zero.
    let reading = clocks.reading(&spec.clock, at_ms(10_000)).unwrap();
    assert!(reading.expired);
    assert!(!clocks.reading(&spec.clock, at_ms(9_999)).unwrap().expired);

    // Operator commands flow through: pausing at 4.5 s freezes `00:06`.
    clocks
        .apply(&spec.clock, ClockCommand::Pause, mono_ms(4_500))
        .unwrap();
    assert_eq!(text(&clocks, &spec, at_ms(4_500)), "00:06");
    assert_eq!(text(&clocks, &spec, at_ms(60_000)), "00:06");
}

#[test]
fn stop_at_target_countdown_holds_zero_after_expiry() {
    let clocks = registry();
    let spec = ProductionClockSpec::new(name("hold"));
    for (millis, expected) in [(9_001, "00:01"), (10_000, "00:00"), (3_600_000, "00:00")] {
        assert_eq!(text(&clocks, &spec, at_ms(millis)), expected, "{millis}ms");
    }
}

#[test]
fn an_element_preset_reformats_without_touching_the_shared_clock() {
    let clocks = registry();
    let tenths = ProductionClockSpec::with_format(
        name("countdown"),
        DisplayFormat::Duration(DurationFormat::HoursMinutesSecondsTenths),
    );
    assert_eq!(text(&clocks, &tenths, at_ms(250)), "00:00:09.8");
    assert_eq!(text(&clocks, &tenths, at_ms(10_000)), "00:00:00.0");
    // Under a tenth past zero still rounds to zero, which carries no sign.
    assert_eq!(text(&clocks, &tenths, at_ms(10_050)), "00:00:00.0");
    assert_eq!(text(&clocks, &tenths, at_ms(10_100)), "-00:00:00.1");
    // The clock's own format, used by other consumers, is unchanged.
    let plain = ProductionClockSpec::new(name("countdown"));
    assert_eq!(text(&clocks, &plain, at_ms(250)), "00:10");
    assert_eq!(
        clocks.reading(&plain.clock, at_ms(250)).unwrap().text,
        "00:10"
    );
}

#[test]
fn time_of_day_in_a_posix_zone_crosses_both_dst_transitions() {
    let clocks = registry();
    let spec = ProductionClockSpec::new(name("eastern"));
    let rfc = ProductionClockSpec::with_format(
        name("eastern"),
        DisplayFormat::TimeOfDay(TimeOfDayFormat::Rfc3339),
    );
    let twelve = ProductionClockSpec::with_format(
        name("eastern"),
        DisplayFormat::TimeOfDay(TimeOfDayFormat::Hours12),
    );
    // Spring forward, Sunday 2026-03-08 at 02:00 EST: 01:59:59 is followed
    // by 03:00:00 EDT.
    let before = at_utc(utc(2026, 3, 8, 6, 59, 59));
    let after = at_utc(utc(2026, 3, 8, 7, 0, 0));
    assert_eq!(text(&clocks, &spec, before), "01:59:59");
    assert_eq!(text(&clocks, &spec, after), "03:00:00");
    assert_eq!(text(&clocks, &rfc, before), "2026-03-08T01:59:59-05:00");
    assert_eq!(text(&clocks, &rfc, after), "2026-03-08T03:00:00-04:00");
    assert_eq!(text(&clocks, &twelve, after), "3:00:00 AM");

    // Fall back, Sunday 2026-11-01 at 02:00 EDT: 01:59:59 EDT is followed by
    // 01:00:00 EST, and the hour repeats.
    let before = at_utc(utc(2026, 11, 1, 5, 59, 59));
    let after = at_utc(utc(2026, 11, 1, 6, 0, 0));
    assert_eq!(text(&clocks, &spec, before), "01:59:59");
    assert_eq!(text(&clocks, &spec, after), "01:00:00");
    assert_eq!(text(&clocks, &rfc, before), "2026-11-01T01:59:59-04:00");
    assert_eq!(text(&clocks, &rfc, after), "2026-11-01T01:00:00-05:00");
    let reading = clocks.reading(&spec.clock, after).unwrap();
    assert_eq!(reading.zone.as_deref(), Some("EST"));
}

#[test]
fn unknown_clocks_and_mismatched_presets_are_typed_errors() {
    let clocks = registry();
    let source = ClockSource::new(&clocks, at_ms(0));
    assert_eq!(
        evaluate_production_clock(&ProductionClockSpec::new(name("missing")), source),
        Err(ClockUnresolved::UnknownClock)
    );
    assert_eq!(
        evaluate_production_clock(
            &ProductionClockSpec::with_format(
                name("missing"),
                DisplayFormat::Duration(DurationFormat::Seconds)
            ),
            source
        ),
        Err(ClockUnresolved::UnknownClock)
    );
    assert_eq!(
        evaluate_production_clock(
            &ProductionClockSpec::with_format(
                name("countdown"),
                DisplayFormat::TimeOfDay(TimeOfDayFormat::Hours24)
            ),
            source
        ),
        Err(ClockUnresolved::FormatMismatch)
    );
    assert_eq!(
        evaluate_production_clock(
            &ProductionClockSpec::with_format(
                name("eastern"),
                DisplayFormat::Duration(DurationFormat::Seconds)
            ),
            source
        ),
        Err(ClockUnresolved::FormatMismatch)
    );
}

#[test]
fn scene_clocks_share_the_production_formatter_and_rounding() {
    // A scene countdown rounds up like a production countdown: 9.5 s left
    // shows `00:10`, and `00:00` appears only at zero.
    let countdown = ClockSpec {
        direction: ClockDirection::CountDown,
        start_value_ms: 10_000,
        starts_at_ms: 0,
        format: ClockFormat::MinutesSeconds,
    };
    let clocks = registry();
    let production = ProductionClockSpec::new(name("hold"));
    for millis in [0, 500, 999, 1_000, 9_001, 9_999, 10_000, 20_000] {
        assert_eq!(
            evaluate_clock(countdown, millis),
            text(&clocks, &production, at_ms(millis)),
            "{millis}ms"
        );
    }
    // Hours and minutes are never wrapped.
    let long = ClockSpec {
        direction: ClockDirection::CountUp,
        start_value_ms: 100 * 3_600_000,
        starts_at_ms: 0,
        format: ClockFormat::HoursMinutesSeconds,
    };
    assert_eq!(evaluate_clock(long, 0), "100:00:00");
    assert_eq!(
        evaluate_clock(
            ClockSpec {
                format: ClockFormat::MinutesSeconds,
                ..long
            },
            0
        ),
        "6000:00"
    );
}

// ---------------------------------------------------------------------------
// Template structure
// ---------------------------------------------------------------------------

#[test]
fn a_production_clock_element_needs_a_font_style() {
    let mut element = clock_element(ProductionClockSpec::new(name("countdown")));
    element.style.font = None;
    let template = template(Vec::new(), element);
    let report = validate_template(&template, &AssetCatalog::new());
    assert!(
        report
            .errors
            .contains(&ValidationError::MissingFontStyle(element_id())),
        "{report:?}"
    );
}

// ---------------------------------------------------------------------------
// Frames (font required)
// ---------------------------------------------------------------------------

fn element_id() -> ElementId {
    ElementId::new(NonZeroU128::new(1).unwrap())
}

fn field_id() -> FieldId {
    FieldId::new(NonZeroU128::new(1).unwrap())
}

fn style() -> Style {
    Style {
        fill: Color::new(255, 255, 255, 255),
        background: Some(Color::new(0, 0, 64, 255)),
        opacity: 255,
        font: Some(FontStyle {
            family: FAMILY.into(),
            size_px: 32,
        }),
        alignment: Alignment::default(),
    }
}

fn element(kind: ElementKind) -> Element {
    Element {
        id: element_id(),
        name: "clock".into(),
        bounds: Bounds {
            x: 8,
            y: 8,
            width: WIDTH - 16,
            height: HEIGHT - 16,
        },
        z_index: 0,
        visible: true,
        style: style(),
        color_field: None,
        kind,
        animations: Vec::new(),
    }
}

fn clock_element(spec: ProductionClockSpec) -> Element {
    element(ElementKind::ProductionClock(spec))
}

fn template(fields: Vec<FieldDefinition>, element: Element) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "clock".into(),
        width: WIDTH,
        height: HEIGHT,
        background: Color::TRANSPARENT,
        fields,
        elements: vec![element],
    }
}

/// The frame a plain text element holding `expected` draws.
fn oracle(assets: &AssetCatalog, expected: &str) -> Vec<u8> {
    let fields = vec![FieldDefinition {
        id: field_id(),
        name: "text".into(),
        default: FieldValue::Text(expected.into()),
    }];
    let scene = template(fields, element(ElementKind::Text { field: field_id() }))
        .instantiate(TitleId::new(NonZeroU128::new(1).unwrap()))
        .unwrap();
    let output = ReferenceRenderer.render(&scene, 0, assets).unwrap();
    assert_eq!(output.report.degraded, Vec::new());
    output.frame.pixels().to_vec()
}

fn render_clock(
    assets: &AssetCatalog,
    spec: ProductionClockSpec,
    source: Option<ClockSource<'_>>,
) -> (Vec<u8>, Vec<DegradedElement>) {
    let scene = template(Vec::new(), clock_element(spec))
        .instantiate(TitleId::new(NonZeroU128::new(2).unwrap()))
        .unwrap();
    let output = match source {
        Some(source) => ReferenceRenderer.render_with_clocks(&scene, 0, assets, source),
        None => ReferenceRenderer.render(&scene, 0, assets),
    }
    .unwrap();
    assert_eq!(output.report.missing_fonts, Vec::new());
    (output.frame.pixels().to_vec(), output.report.degraded)
}

#[test]
fn rendered_countdown_frames_match_the_expected_text_through_expiry_and_overrun() {
    let Some(assets) = catalog() else { return };
    let clocks = registry();
    let tenths = ProductionClockSpec::with_format(
        name("countdown"),
        DisplayFormat::Duration(DurationFormat::MinutesSecondsTenths),
    );
    let cases = [
        (ProductionClockSpec::new(name("countdown")), 0, "00:10"),
        (ProductionClockSpec::new(name("countdown")), 9_500, "00:01"),
        (ProductionClockSpec::new(name("countdown")), 10_000, "00:00"),
        (
            ProductionClockSpec::new(name("countdown")),
            12_000,
            "-00:02",
        ),
        (ProductionClockSpec::new(name("hold")), 12_000, "00:00"),
        (tenths, 10_300, "-00:00.3"),
    ];
    let mut frames = Vec::new();
    for (spec, millis, expected) in cases {
        let (pixels, degraded) = render_clock(
            &assets,
            spec,
            Some(ClockSource::new(&clocks, at_ms(millis))),
        );
        assert_eq!(degraded, Vec::new());
        assert_eq!(
            pixels,
            oracle(&assets, expected),
            "{millis}ms frame is not the text {expected:?}"
        );
        frames.push(pixels);
    }
    // The oracle is discriminating: different texts draw different frames.
    assert_ne!(frames[0], frames[1]);
    assert_ne!(frames[2], frames[3]);
}

#[test]
fn rendered_time_of_day_frames_follow_the_posix_zone_across_dst() {
    let Some(assets) = catalog() else { return };
    let clocks = registry();
    let cases = [
        (utc(2026, 3, 8, 6, 59, 59), "01:59:59"),
        (utc(2026, 3, 8, 7, 0, 0), "03:00:00"),
        (utc(2026, 11, 1, 5, 59, 59), "01:59:59"),
        (utc(2026, 11, 1, 6, 0, 0), "01:00:00"),
    ];
    for (instant, expected) in cases {
        let (pixels, degraded) = render_clock(
            &assets,
            ProductionClockSpec::new(name("eastern")),
            Some(ClockSource::new(&clocks, at_utc(instant))),
        );
        assert_eq!(degraded, Vec::new());
        assert_eq!(
            pixels,
            oracle(&assets, expected),
            "{instant:?} frame is not the text {expected:?}"
        );
    }
    let (pixels, _) = render_clock(
        &assets,
        ProductionClockSpec::with_format(
            name("eastern"),
            DisplayFormat::TimeOfDay(TimeOfDayFormat::Hours12Minutes),
        ),
        Some(ClockSource::new(&clocks, at_utc(utc(2026, 3, 8, 7, 0, 0)))),
    );
    assert_eq!(pixels, oracle(&assets, "3:00 AM"));
}

#[test]
fn unresolved_clocks_draw_only_their_background_and_are_reported() {
    let Some(assets) = catalog() else { return };
    let clocks = registry();
    let background = oracle(&assets, "");
    let degraded = |reason| {
        vec![DegradedElement {
            element: element_id(),
            reason: Degradation::ClockUnresolved(reason),
        }]
    };

    let (pixels, report) = render_clock(&assets, ProductionClockSpec::new(name("countdown")), None);
    assert_eq!(pixels, background);
    assert_eq!(report, degraded(ClockUnresolved::NoClockSource));

    let source = ClockSource::new(&clocks, at_ms(0));
    let (pixels, report) = render_clock(
        &assets,
        ProductionClockSpec::new(name("missing")),
        Some(source),
    );
    assert_eq!(pixels, background);
    assert_eq!(report, degraded(ClockUnresolved::UnknownClock));

    let (pixels, report) = render_clock(
        &assets,
        ProductionClockSpec::with_format(
            name("eastern"),
            DisplayFormat::Duration(DurationFormat::Seconds),
        ),
        Some(source),
    );
    assert_eq!(pixels, background);
    assert_eq!(report, degraded(ClockUnresolved::FormatMismatch));
}

// ---------------------------------------------------------------------------
// Font discovery
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
    let Some(bytes) = discover_font() else {
        assert!(
            std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
            "FM_REQUIRE_FONT=1 but no usable font file was found"
        );
        eprintln!("skipping production clock frames: no font file available");
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
    for path in DIRECT_FONTS {
        if let Ok(bytes) = fs::read(path) {
            return Some(bytes);
        }
    }
    FONT_ROOTS
        .iter()
        .find_map(|root| scan_for_font(Path::new(root), 0))
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

// ---------------------------------------------------------------------------
// Template-defined clocks (font-free)
// ---------------------------------------------------------------------------

fn title_clocks() -> Vec<TitleClock> {
    vec![
        TitleClock {
            name: name("doors"),
            kind: TitleClockKind::CountdownTo {
                target: utc(2026, 10, 14, 23, 0, 0),
                overrun: OverrunPolicy::StopAtTarget,
                format: DurationFormat::HoursMinutesSeconds,
            },
        },
        TitleClock {
            name: name("late"),
            kind: TitleClockKind::CountdownTo {
                target: utc(2026, 10, 14, 23, 0, 0),
                overrun: OverrunPolicy::Continue,
                format: DurationFormat::MinutesSeconds,
            },
        },
        TitleClock {
            name: name("on-air"),
            kind: TitleClockKind::SinceTake {
                format: DurationFormat::MinutesSecondsTenths,
            },
        },
        TitleClock {
            name: name("eastern"),
            kind: TitleClockKind::TimeOfDay {
                zone: TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
                format: TimeOfDayFormat::Hours24,
            },
        },
    ]
}

fn title_clock_text(
    clock: &str,
    format: Option<DisplayFormat>,
    at: TimeReading,
    taken_at: Option<ClockTime>,
) -> String {
    let registry = title_clock_registry(&title_clocks(), TitleClockTime { at, taken_at }).unwrap();
    let spec = ProductionClockSpec {
        clock: name(clock),
        format,
    };
    text(&registry, &spec, at)
}

#[test]
fn countdown_to_counts_the_wall_clock_down_to_its_target() {
    let reading = |hour, minute, second, nanos| {
        TimeReading::new(
            mono_ms(5_000),
            UtcInstant::from_utc_civil(2026, 10, 14, hour, minute, second, nanos).unwrap(),
        )
    };
    for (at, doors, late) in [
        (reading(21, 30, 0, 0), "01:30:00", "90:00"),
        // Countdowns round up: half a second before the target still shows 1.
        (reading(22, 59, 59, 500_000_000), "00:00:01", "00:01"),
        (reading(23, 0, 0, 0), "00:00:00", "00:00"),
        // Past the target one holds zero and the other runs negative.
        (reading(23, 0, 1, 0), "00:00:00", "-00:01"),
        (reading(23, 2, 5, 0), "00:00:00", "-02:05"),
    ] {
        assert_eq!(title_clock_text("doors", None, at, None), doors);
        assert_eq!(title_clock_text("late", None, at, None), late);
    }
    // An element preset reformats the same value.
    assert_eq!(
        title_clock_text(
            "doors",
            Some(DisplayFormat::Duration(DurationFormat::Seconds)),
            reading(22, 58, 20, 0),
            None
        ),
        "100"
    );
    // A target beyond the clock range saturates instead of failing.
    let far = TimeReading::new(mono_ms(0), utc(1, 1, 1, 0, 0, 0));
    let registry = title_clock_registry(
        &title_clocks(),
        TitleClockTime {
            at: far,
            taken_at: None,
        },
    )
    .unwrap();
    assert_eq!(
        registry.reading(&name("doors"), far).unwrap().value,
        Some(ClockValue::MAX)
    );
}

#[test]
fn since_take_runs_from_the_take_instant_and_rests_at_zero_off_air() {
    let at = TimeReading::new(mono_ms(12_345), utc(2026, 1, 1, 0, 0, 0));
    assert_eq!(title_clock_text("on-air", None, at, None), "00:00.0");
    assert_eq!(
        title_clock_text("on-air", None, at, Some(mono_ms(2_000))),
        "00:10.3"
    );
    assert_eq!(
        title_clock_text("on-air", None, at, Some(mono_ms(12_345))),
        "00:00.0"
    );
    // A take after the reading is not on air yet.
    assert_eq!(
        title_clock_text("on-air", None, at, Some(mono_ms(20_000))),
        "00:00.0"
    );
    assert_eq!(
        title_clock_text(
            "on-air",
            Some(DisplayFormat::Duration(DurationFormat::HoursMinutesSeconds)),
            TimeReading::new(mono_ms(3_725_000), utc(2026, 1, 1, 0, 0, 0)),
            Some(mono_ms(0))
        ),
        "01:02:05"
    );
}

#[test]
fn template_time_of_day_follows_its_posix_zone_across_dst() {
    for (instant, expected) in [
        // 2026-03-08 02:00 EST springs forward to 03:00 EDT (07:00 UTC).
        (utc(2026, 3, 8, 6, 59, 59), "01:59:59"),
        (utc(2026, 3, 8, 7, 0, 0), "03:00:00"),
        // 2026-11-01 02:00 EDT falls back to 01:00 EST (06:00 UTC).
        (utc(2026, 11, 1, 5, 59, 59), "01:59:59"),
        (utc(2026, 11, 1, 6, 0, 0), "01:00:00"),
    ] {
        assert_eq!(
            title_clock_text("eastern", None, at_utc(instant), None),
            expected
        );
    }
    assert_eq!(
        title_clock_text(
            "eastern",
            Some(DisplayFormat::TimeOfDay(TimeOfDayFormat::Rfc3339)),
            at_utc(utc(2026, 7, 4, 16, 30, 0)),
            None
        ),
        "2026-07-04T12:30:00-04:00"
    );
}

#[test]
fn template_clock_tables_must_match_the_elements_exactly() {
    let element_with_id = |index: u128, spec: ProductionClockSpec| {
        let mut element = clock_element(spec);
        element.id = ElementId::new(NonZeroU128::new(index).unwrap());
        element
    };
    let mut elements = title_clocks()
        .into_iter()
        .zip(1..)
        .map(|(clock, index)| element_with_id(index, ProductionClockSpec::new(clock.name)))
        .collect::<Vec<_>>();
    let build = |elements: &[Element]| TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "clocks".into(),
        width: WIDTH,
        height: HEIGHT,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: elements.to_vec(),
    };
    assert_eq!(
        validate_title_clocks(&build(&elements), &title_clocks()),
        Ok(())
    );

    let mut clocks = title_clocks();
    clocks.push(clocks[0].clone());
    assert_eq!(
        validate_title_clocks(&build(&elements), &clocks),
        Err(TitleClockError::DuplicateClock(name("doors")))
    );
    assert_eq!(
        validate_title_clocks(&build(&elements[1..]), &title_clocks()),
        Err(TitleClockError::UnusedClock(name("doors")))
    );
    assert_eq!(
        validate_title_clocks(&build(&elements), &title_clocks()[1..]),
        Err(TitleClockError::UndefinedClock {
            element: ElementId::new(NonZeroU128::new(1).unwrap()),
            clock: name("doors"),
        })
    );
    elements[3] = element_with_id(
        4,
        ProductionClockSpec::with_format(
            name("eastern"),
            DisplayFormat::Duration(DurationFormat::Seconds),
        ),
    );
    assert_eq!(
        validate_title_clocks(&build(&elements), &title_clocks()),
        Err(TitleClockError::FormatMismatch {
            element: ElementId::new(NonZeroU128::new(4).unwrap()),
            clock: name("eastern"),
        })
    );
}
