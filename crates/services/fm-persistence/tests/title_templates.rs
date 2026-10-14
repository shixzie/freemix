//! Stored title templates: the strict `.fmtitle` codec, template and font
//! import, title validation against templates, asset audit, and title assets
//! in portable export and append.
//!
//! Tests that import a real font discover one: `FM_TITLES_FONT`, then
//! well-known system paths, then a scan of the system font roots. They skip
//! when none is found unless `FM_REQUIRE_FONT=1`. Every other test uses
//! rectangle, color, and number fields, or font files that are only copied
//! and never parsed.

use std::{
    fmt::Write as _,
    fs,
    num::NonZeroU128,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_clock::production::{
    ClockName, DisplayFormat, DurationFormat, OverrunPolicy, TimeOfDayFormat, TimeZone, UtcInstant,
};
use fm_model::{
    Input, InputKind, Project, ProjectSettings, Rgba8, TitleFieldEntry, TitleFieldId,
    TitleFieldValue, TitleInput,
};
use fm_persistence::{
    AssetAuditIssue, AssetAuditReason, AssetDisposition, BundleError, ProjectPosition,
    ProjectStore, RuntimeRouting, StoredProject, StoredTitleTemplate, TitleFontAsset,
    TitleImportError, TitleIssue, TitleProblem, TitleTemplateError, decode_title_template,
    encode_title_template, title_field_defaults,
};
use fm_titles::{
    Alignment, AnimatedProperty, AnimationTrack, Bounds, ClockDirection, ClockFormat, ClockSpec,
    Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldType, FieldValue,
    FontError, FontStyle, HorizontalAlignment, ImageFilter, ImageFit, ImageSource, Interpolation,
    Keyframe, ProductionClockSpec, Style, TemplateId, TickerDirection, TickerSpec, TitleClock,
    TitleClockError, TitleClockKind, TitleTemplate, VerticalAlignment,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-titles-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(name)).unwrap()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn element_id(value: u128) -> ElementId {
    ElementId::new(nz(value))
}

fn field_id(value: u128) -> FieldId {
    FieldId::new(nz(value))
}

fn input_id(value: u128) -> InputId {
    InputId::new(nz(value))
}

fn style(font: Option<&str>) -> Style {
    Style {
        fill: Color::new(255, 255, 255, 255),
        background: None,
        opacity: 255,
        font: font.map(|family| FontStyle {
            family: family.into(),
            size_px: 48,
        }),
        alignment: Alignment::default(),
    }
}

fn element(id: u128, kind: ElementKind, font: Option<&str>) -> Element {
    Element {
        id: element_id(id),
        name: format!("Element {id}"),
        bounds: Bounds {
            x: 10,
            y: 20,
            width: 300,
            height: 60,
        },
        z_index: 0,
        visible: true,
        style: style(font),
        color_field: None,
        kind,
        animations: Vec::new(),
    }
}

/// A template that needs no font: a rectangle tinted by a color field, and a
/// number field.
fn shape_template(name: &str) -> TitleTemplate {
    let mut bar = element(1, ElementKind::Rectangle, None);
    bar.color_field = Some(field_id(2));
    TitleTemplate {
        id: TemplateId::new(nz(7)),
        name: name.into(),
        width: 1920,
        height: 1080,
        background: Color::TRANSPARENT,
        fields: vec![
            FieldDefinition {
                id: field_id(2),
                name: "Bar color".into(),
                default: FieldValue::Color(Color::new(0x11, 0x22, 0x33, 0xff)),
            },
            FieldDefinition {
                id: field_id(1),
                name: "Score".into(),
                default: FieldValue::Number(-4),
            },
        ],
        elements: vec![bar],
    }
}

/// A template with one text element in `family`.
fn text_template(family: &str) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(nz(8)),
        name: "Name strap".into(),
        width: 1920,
        height: 1080,
        background: Color::TRANSPARENT,
        fields: vec![FieldDefinition {
            id: field_id(1),
            name: "Name".into(),
            default: FieldValue::Text("Jane Doe".into()),
        }],
        elements: vec![element(
            1,
            ElementKind::Text { field: field_id(1) },
            Some(family),
        )],
    }
}

/// Every storable element kind, style option, and animation track.
#[allow(clippy::too_many_lines)]
fn rich_template() -> StoredTitleTemplate {
    let mut text = element(
        1,
        ElementKind::Text { field: field_id(1) },
        Some("Inter Bold"),
    );
    text.style.background = Some(Color::new(0, 0, 0, 128));
    text.style.opacity = 200;
    text.style.alignment = Alignment {
        horizontal: HorizontalAlignment::Center,
        vertical: VerticalAlignment::End,
    };
    text.z_index = -3;
    text.visible = false;
    text.bounds.x = -400;
    text.animations = vec![
        AnimationTrack {
            property: AnimatedProperty::X,
            keyframes: vec![
                Keyframe {
                    at_ms: 0,
                    value: -400,
                    interpolation: Interpolation::Linear,
                },
                Keyframe {
                    at_ms: 500,
                    value: 40,
                    interpolation: Interpolation::Hold,
                },
            ],
        },
        AnimationTrack {
            property: AnimatedProperty::Opacity,
            keyframes: vec![Keyframe {
                at_ms: 0,
                value: 255,
                interpolation: Interpolation::Hold,
            }],
        },
    ];
    let mut rectangle = element(2, ElementKind::Rectangle, None);
    rectangle.color_field = Some(field_id(3));
    rectangle.animations = vec![AnimationTrack {
        property: AnimatedProperty::Width,
        keyframes: vec![Keyframe {
            at_ms: 10,
            value: 300,
            interpolation: Interpolation::Linear,
        }],
    }];
    let mut image = element(
        3,
        ElementKind::Image {
            field: field_id(4),
            fit: ImageFit::Contain,
            filter: ImageFilter::Bilinear,
        },
        None,
    );
    image.style.alignment = Alignment {
        horizontal: HorizontalAlignment::End,
        vertical: VerticalAlignment::Start,
    };
    let clock = element(
        4,
        ElementKind::Clock(ClockSpec {
            direction: ClockDirection::CountDown,
            start_value_ms: 90_000,
            starts_at_ms: 250,
            format: ClockFormat::HoursMinutesSeconds,
        }),
        Some("Mono"),
    );
    let ticker = element(
        5,
        ElementKind::Ticker(TickerSpec {
            field: field_id(1),
            direction: TickerDirection::Right,
            pixels_per_second: 120,
            gap_px: 40,
            starts_at_ms: 1_000,
        }),
        Some("Inter Bold"),
    );
    let plain_image = element(
        6,
        ElementKind::Image {
            field: field_id(4),
            fit: ImageFit::None,
            filter: ImageFilter::Nearest,
        },
        None,
    );
    let cover = element(
        7,
        ElementKind::Image {
            field: field_id(4),
            fit: ImageFit::Cover,
            filter: ImageFilter::Nearest,
        },
        None,
    );
    let stretch = element(
        8,
        ElementKind::Image {
            field: field_id(4),
            fit: ImageFit::Stretch,
            filter: ImageFilter::Nearest,
        },
        None,
    );
    let count_up = element(
        9,
        ElementKind::Clock(ClockSpec {
            direction: ClockDirection::CountUp,
            start_value_ms: 0,
            starts_at_ms: 0,
            format: ClockFormat::MinutesSeconds,
        }),
        Some("Mono"),
    );
    let mut left_ticker = element(
        10,
        ElementKind::Ticker(TickerSpec {
            field: field_id(2),
            direction: TickerDirection::Left,
            pixels_per_second: 60,
            gap_px: 0,
            starts_at_ms: 0,
        }),
        Some("Mono"),
    );
    left_ticker.style.alignment.vertical = VerticalAlignment::Center;
    left_ticker.animations = vec![
        AnimationTrack {
            property: AnimatedProperty::Y,
            keyframes: vec![Keyframe {
                at_ms: 0,
                value: i64::MIN,
                interpolation: Interpolation::Hold,
            }],
        },
        AnimationTrack {
            property: AnimatedProperty::Height,
            keyframes: vec![Keyframe {
                at_ms: u64::MAX,
                value: i64::from(u32::MAX),
                interpolation: Interpolation::Hold,
            }],
        },
    ];
    StoredTitleTemplate {
        template: TitleTemplate {
            id: TemplateId::new(NonZeroU128::MAX),
            name: "Lower \"third\"\n".into(),
            width: 1920,
            height: 1080,
            background: Color::new(1, 2, 3, 4),
            fields: vec![
                FieldDefinition {
                    id: field_id(1),
                    name: "Name".into(),
                    default: FieldValue::Text("Jane Doe".into()),
                },
                FieldDefinition {
                    id: field_id(2),
                    name: "Count".into(),
                    default: FieldValue::Number(i64::MIN),
                },
                FieldDefinition {
                    id: field_id(3),
                    name: "Bar".into(),
                    default: FieldValue::Color(Color::new(0xab, 0xcd, 0xef, 0x01)),
                },
                FieldDefinition {
                    id: field_id(4),
                    name: "Logo".into(),
                    default: FieldValue::Image(ImageSource::new("asset://images/logo.png")),
                },
            ],
            elements: vec![
                text,
                rectangle,
                image,
                clock,
                ticker,
                plain_image,
                cover,
                stretch,
                count_up,
                left_ticker,
            ],
        },
        fonts: vec![
            TitleFontAsset {
                family: "Inter Bold".into(),
                asset_uri: "asset://fonts/Inter-Bold.ttf".into(),
            },
            TitleFontAsset {
                family: "Mono".into(),
                asset_uri: "asset://fonts/Mono.ttf".into(),
            },
        ],
        clocks: Vec::new(),
    }
}

/// A source document for import: the stored format with an empty font table
/// and no clocks.
fn source_document(template: &TitleTemplate) -> String {
    source_document_with_clocks(template, Vec::new())
}

/// A source document for import: the stored format with an empty font table
/// and `clocks`.
fn source_document_with_clocks(template: &TitleTemplate, clocks: Vec<TitleClock>) -> String {
    // Encode with a placeholder font table, then empty it: the encoder
    // refuses a font table that does not cover the template.
    let families = template
        .elements
        .iter()
        .filter_map(|element| element.style.font.as_ref())
        .map(|font| font.family.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let fonts = families
        .iter()
        .enumerate()
        .map(|(index, family)| TitleFontAsset {
            family: family.clone(),
            asset_uri: format!("asset://fonts/placeholder-{index}.ttf"),
        })
        .collect::<Vec<_>>();
    let encoded = encode_title_template(&StoredTitleTemplate {
        template: template.clone(),
        fonts,
        clocks,
    })
    .unwrap();
    let start = encoded.find("  \"fonts\": [").unwrap();
    let end = encoded.find("  \"clocks\": [").unwrap();
    format!("{}  \"fonts\": [],\n{}", &encoded[..start], &encoded[end..])
}

fn write_source(temp: &TestDirectory, name: &str, template: &TitleTemplate) -> PathBuf {
    let path = temp.path(name);
    write(&path, source_document(template).as_bytes());
    path
}

fn empty_project(id: u128) -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(nz(id)),
        "Show",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 9).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    )
}

fn title_input(id: u128, template_uri: &str, fields: Vec<TitleFieldEntry>) -> Input {
    Input {
        id: input_id(id),
        name: format!("Title {id}"),
        kind: InputKind::Title(TitleInput {
            template_uri: template_uri.into(),
            field_revision: 0,
            fields,
        }),
        required_capabilities: Vec::new(),
    }
}

fn stored(project: Project) -> StoredProject {
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn entry(id: u128, value: TitleFieldValue) -> TitleFieldEntry {
    TitleFieldEntry {
        id: TitleFieldId::new(nz(id)),
        value,
    }
}

/// Writes `template` with `fonts` straight into the bundle, bypassing import.
fn store_template(
    store: &ProjectStore,
    key: &str,
    template: TitleTemplate,
    fonts: Vec<TitleFontAsset>,
) -> String {
    let document = encode_title_template(&StoredTitleTemplate {
        template,
        fonts,
        clocks: Vec::new(),
    })
    .unwrap();
    write(&store.assets_root().join(key), document.as_bytes());
    format!("asset://{key}")
}

fn assets_listing(store: &ProjectStore) -> Vec<String> {
    fn walk(root: &Path, directory: &Path, output: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, output);
            } else {
                output.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut output = Vec::new();
    walk(&store.assets_root(), &store.assets_root(), &mut output);
    output.sort();
    output
}

// ---------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------

#[test]
fn every_element_kind_round_trips_exactly() {
    let expected = rich_template();
    let encoded = encode_title_template(&expected).unwrap();
    assert!(encoded.starts_with("{\n  \"schema_version\": 29,\n  \"template\": {\n"));
    assert!(encoded.contains(
        "\"kind\": {\"type\": \"image\", \"field\": 4, \"fit\": \"contain\", \"filter\": \"bilinear\"}"
    ));
    assert!(encoded.contains(
        "\"kind\": {\"type\": \"clock\", \"direction\": \"count_down\", \"start_value_ms\": 90000, \"starts_at_ms\": 250, \"format\": \"hours_minutes_seconds\"}"
    ));
    assert!(encoded.contains(
        "\"kind\": {\"type\": \"ticker\", \"field\": 1, \"direction\": \"right\", \"pixels_per_second\": 120, \"gap_px\": 40, \"starts_at_ms\": 1000}"
    ));
    assert!(encoded.contains("\"alignment\": {\"horizontal\": \"center\", \"vertical\": \"end\"}"));
    assert!(encoded.contains("\"background\": \"#00000080\""));
    assert!(encoded.contains("{\"at_ms\": 0, \"value\": -400, \"interpolation\": \"linear\"}"));
    assert!(
        encoded.contains(
            "{\"family\": \"Inter Bold\", \"asset_uri\": \"asset://fonts/Inter-Bold.ttf\"}"
        )
    );

    let decoded = decode_title_template(&encoded).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(encode_title_template(&decoded).unwrap(), encoded);

    // Fonts are written sorted by family whatever the in-memory order.
    let mut reversed = expected.clone();
    reversed.fonts.reverse();
    assert_eq!(encode_title_template(&reversed).unwrap(), encoded);

    assert_eq!(
        title_field_defaults(&expected.template),
        vec![
            entry(1, TitleFieldValue::Text("Jane Doe".into())),
            entry(2, TitleFieldValue::Number(i64::MIN)),
            entry(
                3,
                TitleFieldValue::Color(Rgba8::new(0xab, 0xcd, 0xef, 0x01))
            ),
            entry(
                4,
                TitleFieldValue::Image {
                    asset_uri: "asset://images/logo.png".into()
                }
            ),
        ]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn strict_decoder_refuses_unknown_missing_and_out_of_contract_content() {
    let encoded = encode_title_template(&rich_template()).unwrap();
    let malformed = [
        encoded.replacen("\"z_index\": -3, ", "", 1),
        encoded.replacen("\"z_index\": -3, ", "\"z_index\": -3, \"rotation\": 0, ", 1),
        encoded.replacen("\"width\": 1920,", "\"width\": 1920, \"dpi\": 72,", 1),
        encoded.replacen("\"fonts\": [", "\"extra\": null,\n  \"fonts\": [", 1),
        encoded.replacen(
            "\"horizontal\": \"center\"",
            "\"horizontal\": \"middle\"",
            1,
        ),
        encoded.replacen("\"fit\": \"contain\"", "\"fit\": \"fill\"", 1),
        encoded.replacen("\"filter\": \"bilinear\"", "\"filter\": \"cubic\"", 1),
        encoded.replacen(
            "\"direction\": \"count_down\"",
            "\"direction\": \"down\"",
            1,
        ),
        encoded.replacen(
            "\"format\": \"hours_minutes_seconds\"",
            "\"format\": \"hms\"",
            1,
        ),
        encoded.replacen("\"direction\": \"right\"", "\"direction\": \"up\"", 1),
        encoded.replacen(
            "\"interpolation\": \"linear\"",
            "\"interpolation\": \"ease\"",
            1,
        ),
        encoded.replacen("\"property\": \"x\"", "\"property\": \"rotation\"", 1),
        encoded.replacen("{\"type\": \"rectangle\"}", "{\"type\": \"ellipse\"}", 1),
        encoded.replacen(
            "{\"type\": \"rectangle\"}",
            "{\"type\": \"rectangle\", \"radius\": 2}",
            1,
        ),
        encoded.replacen(
            "\"background\": \"#01020304\"",
            "\"background\": \"#010203\"",
            1,
        ),
        encoded.replacen("\"background\": \"#01020304\"", "\"background\": null", 1),
        encoded.replacen("\"opacity\": 200", "\"opacity\": 256", 1),
        encoded.replacen("\"color_field\": 3", "\"color_field\": 0", 1),
        encoded.replacen("\"color_field\": 3", "\"color_field\": \"3\"", 1),
        encoded.replacen(
            "\"id\": 1, \"name\": \"Name\"",
            "\"id\": 0, \"name\": \"Name\"",
            1,
        ),
        format!("{encoded} {{}}"),
        encoded.replacen("{\n", "", 1),
    ];
    for document in malformed {
        assert_ne!(document, encoded);
        let error = decode_title_template(&document).unwrap_err();
        assert!(
            matches!(error, TitleTemplateError::Malformed { .. }),
            "got {error:?} for {document}"
        );
    }

    for (schema, found) in [("28", 28), ("30", 30)] {
        let document = encoded.replacen(
            "\"schema_version\": 29",
            &format!("\"schema_version\": {schema}"),
            1,
        );
        assert!(matches!(
            decode_title_template(&document),
            Err(TitleTemplateError::UnsupportedSchema { found: f, supported: 29 }) if f == found
        ));
    }

    // Font table mismatches are typed.
    let mono = ",\n    {\"family\": \"Mono\", \"asset_uri\": \"asset://fonts/Mono.ttf\"}";
    assert!(matches!(
        decode_title_template(&encoded.replacen(mono, "", 1)),
        Err(TitleTemplateError::MissingFontFamily(family)) if family == "Mono"
    ));
    assert!(matches!(
        decode_title_template(&encoded.replacen(
            mono,
            &format!("{mono},\n    {{\"family\": \"Unused\", \"asset_uri\": \"asset://fonts/u.ttf\"}}"),
            1
        )),
        Err(TitleTemplateError::UnusedFontFamily(family)) if family == "Unused"
    ));
    assert!(matches!(
        decode_title_template(&encoded.replacen(mono, &format!("{mono}{mono}"), 1)),
        Err(TitleTemplateError::DuplicateFontFamily(family)) if family == "Mono"
    ));
    assert!(matches!(
        decode_title_template(&encoded.replacen(
            "\"asset://fonts/Mono.ttf\"",
            "\"/usr/share/fonts/Mono.ttf\"",
            1
        )),
        Err(TitleTemplateError::InvalidFontUri { family }) if family == "Mono"
    ));
    // Unsorted font tables are not canonical.
    let inter =
        "\n    {\"family\": \"Inter Bold\", \"asset_uri\": \"asset://fonts/Inter-Bold.ttf\"}";
    let swapped = encoded.replacen(
        &format!("{inter}{mono}"),
        &format!("\n    {}, {}", &mono[6..], &inter[5..]),
        1,
    );
    assert_ne!(swapped, encoded);
    assert!(matches!(
        decode_title_template(&swapped),
        Err(TitleTemplateError::Malformed { .. })
    ));

    // Structure and bounds are validated.
    assert!(matches!(
        decode_title_template(&encoded.replacen("\"width\": 1920,", "\"width\": 0,", 1)),
        Err(TitleTemplateError::InvalidTemplate(_))
    ));
    assert!(matches!(
        decode_title_template(&encoded.replacen(
            "\"asset_uri\": \"asset://images/logo.png\"",
            "\"asset_uri\": \"logo.png\"",
            1
        )),
        Err(TitleTemplateError::InvalidImageUri { field }) if field == field_id(4)
    ));
    assert!(matches!(
        decode_title_template(&encoded.replacen(
            "\"value\": \"Jane Doe\"",
            &format!("\"value\": \"{}\"", "x".repeat(4_097)),
            1
        )),
        Err(TitleTemplateError::InvalidFieldDefaults(_))
    ));
}

fn clock_name(text: &str) -> ClockName {
    ClockName::new(text).unwrap()
}

/// Every production clock kind, every preset, and both overrun policies.
#[allow(clippy::too_many_lines)]
fn clock_template() -> StoredTitleTemplate {
    let rate = FrameRate::new(30_000, 1_001).unwrap();
    let durations = [
        DurationFormat::HoursMinutesSeconds,
        DurationFormat::MinutesSeconds,
        DurationFormat::Seconds,
        DurationFormat::HoursMinutesSecondsTenths,
        DurationFormat::MinutesSecondsTenths,
        DurationFormat::HoursMinutesSecondsMillis,
        DurationFormat::HoursMinutesSecondsFrames(rate),
        DurationFormat::Compact,
    ];
    let times = [
        TimeOfDayFormat::Hours24,
        TimeOfDayFormat::Hours24Minutes,
        TimeOfDayFormat::Hours12,
        TimeOfDayFormat::Hours12Minutes,
        TimeOfDayFormat::Date,
        TimeOfDayFormat::Rfc3339,
    ];
    let target = UtcInstant::from_utc_civil(2026, 10, 14, 23, 0, 0, 0).unwrap();
    let mut clocks = Vec::new();
    for (index, format) in durations.into_iter().enumerate() {
        clocks.push(TitleClock {
            name: clock_name(&format!("Since-{index}")),
            kind: TitleClockKind::SinceTake { format },
        });
        clocks.push(TitleClock {
            name: clock_name(&format!("To_{index}")),
            kind: TitleClockKind::CountdownTo {
                target,
                overrun: if index % 2 == 0 {
                    OverrunPolicy::StopAtTarget
                } else {
                    OverrunPolicy::Continue
                },
                format,
            },
        });
    }
    let zones = [
        "EST5EDT,M3.2.0,M11.1.0",
        "CET-1CEST,M3.5.0,M10.5.0/3",
        "UTC0",
        "<+0530>-5:30",
        "AEST-10AEDT,M10.1.0,M4.1.0/3",
        "NZST-12NZDT,J60,300",
    ];
    for (index, (format, zone)) in times.into_iter().zip(zones).enumerate() {
        clocks.push(TitleClock {
            name: clock_name(&format!("Local{index}")),
            kind: TitleClockKind::TimeOfDay {
                zone: TimeZone::parse_posix(zone).unwrap(),
                format,
            },
        });
    }
    let mut elements = Vec::new();
    for (index, clock) in clocks.iter().enumerate() {
        let id = u128::try_from(index).unwrap() + 1;
        elements.push(element(
            id,
            ElementKind::ProductionClock(ProductionClockSpec::new(clock.name.clone())),
            Some("Mono"),
        ));
    }
    // Element presets of both kinds, including the frames preset.
    let first_duration = elements.len() + 1;
    for (offset, format) in durations.into_iter().enumerate() {
        elements.push(element(
            u128::try_from(first_duration + offset).unwrap(),
            ElementKind::ProductionClock(ProductionClockSpec::with_format(
                clock_name("Since-0"),
                DisplayFormat::Duration(format),
            )),
            Some("Mono"),
        ));
    }
    let first_time = elements.len() + 1;
    for (offset, format) in times.into_iter().enumerate() {
        elements.push(element(
            u128::try_from(first_time + offset).unwrap(),
            ElementKind::ProductionClock(ProductionClockSpec::with_format(
                clock_name("Local0"),
                DisplayFormat::TimeOfDay(format),
            )),
            Some("Mono"),
        ));
    }
    clocks.sort_by(|left, right| left.name.cmp(&right.name));
    StoredTitleTemplate {
        template: TitleTemplate {
            id: TemplateId::new(nz(12)),
            name: "Clocks".into(),
            width: 1920,
            height: 1080,
            background: Color::TRANSPARENT,
            fields: Vec::new(),
            elements,
        },
        fonts: vec![TitleFontAsset {
            family: "Mono".into(),
            asset_uri: "asset://fonts/Mono.ttf".into(),
        }],
        clocks,
    }
}

#[test]
fn production_clocks_of_every_kind_and_preset_round_trip_exactly() {
    let expected = clock_template();
    let encoded = encode_title_template(&expected).unwrap();
    for fragment in [
        "{\"name\": \"Local0\", \"kind\": \"time_of_day\", \"zone\": \"EST5EDT,M3.2.0,M11.1.0\", \"format\": {\"type\": \"time_of_day\", \"preset\": \"hours_24\"}}",
        "{\"name\": \"Local5\", \"kind\": \"time_of_day\", \"zone\": \"NZST-12NZDT,J60,300\", \"format\": {\"type\": \"time_of_day\", \"preset\": \"rfc3339\"}}",
        "{\"name\": \"To_0\", \"kind\": \"countdown_to\", \"target_utc\": \"2026-10-14T23:00:00Z\", \"overrun\": \"stop_at_target\", \"format\": {\"type\": \"duration\", \"preset\": \"hours_minutes_seconds\"}}",
        "{\"name\": \"To_1\", \"kind\": \"countdown_to\", \"target_utc\": \"2026-10-14T23:00:00Z\", \"overrun\": \"continue\", \"format\": {\"type\": \"duration\", \"preset\": \"minutes_seconds\"}}",
        "{\"name\": \"Since-6\", \"kind\": \"since_take\", \"format\": {\"type\": \"duration\", \"preset\": \"hours_minutes_seconds_frames\", \"frame_rate\": {\"numerator\": 30000, \"denominator\": 1001}}}",
        "\"kind\": {\"type\": \"production_clock\", \"clock\": \"Local0\", \"format\": null}",
        "\"kind\": {\"type\": \"production_clock\", \"clock\": \"Since-0\", \"format\": {\"type\": \"duration\", \"preset\": \"compact\"}}",
        "\"kind\": {\"type\": \"production_clock\", \"clock\": \"Local0\", \"format\": {\"type\": \"time_of_day\", \"preset\": \"hours_12_minutes\"}}",
    ] {
        assert!(
            encoded.contains(fragment),
            "missing {fragment} in {encoded}"
        );
    }
    let decoded = decode_title_template(&encoded).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(encode_title_template(&decoded).unwrap(), encoded);

    // Clocks are written sorted by name whatever the in-memory order.
    let mut reversed = expected.clone();
    reversed.clocks.reverse();
    assert_eq!(encode_title_template(&reversed).unwrap(), encoded);

    // A document with unsorted clocks is refused.
    let local0 = encoded.find("    {\"name\": \"Local0\"").unwrap();
    let local1 = encoded.find("    {\"name\": \"Local1\"").unwrap();
    let mut swapped = encoded.clone();
    let first = encoded[local0..local1].to_owned();
    let second_end = local1 + encoded[local1..].find('\n').unwrap() + 1;
    let second = encoded[local1..second_end].to_owned();
    swapped.replace_range(local0..second_end, &format!("{second}{first}"));
    assert!(matches!(
        decode_title_template(&swapped),
        Err(TitleTemplateError::Malformed { message, .. }) if message.contains("sorted")
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn clock_tables_and_clock_content_are_strict() {
    let stored = clock_template();
    let encoded = encode_title_template(&stored).unwrap();

    // The table covers exactly the clocks elements show.
    let mut missing = stored.clone();
    missing.clocks.retain(|clock| clock.name.as_str() != "To_3");
    assert!(matches!(
        encode_title_template(&missing),
        Err(TitleTemplateError::InvalidClocks(TitleClockError::UndefinedClock { clock, .. }))
            if clock.as_str() == "To_3"
    ));
    let mut unused = stored.clone();
    unused.clocks.push(TitleClock {
        name: clock_name("Spare"),
        kind: TitleClockKind::SinceTake {
            format: DurationFormat::Seconds,
        },
    });
    assert!(matches!(
        encode_title_template(&unused),
        Err(TitleTemplateError::InvalidClocks(TitleClockError::UnusedClock(clock)))
            if clock.as_str() == "Spare"
    ));
    let mut duplicate = stored.clone();
    duplicate.clocks.push(stored.clocks[0].clone());
    assert!(matches!(
        encode_title_template(&duplicate),
        Err(TitleTemplateError::InvalidClocks(
            TitleClockError::DuplicateClock(_)
        ))
    ));
    // An element preset must be of its clock's kind.
    let mut mismatch = stored.clone();
    mismatch.template.elements.push(element(
        999,
        ElementKind::ProductionClock(ProductionClockSpec::with_format(
            clock_name("Local0"),
            DisplayFormat::Duration(DurationFormat::Seconds),
        )),
        Some("Mono"),
    ));
    assert!(matches!(
        encode_title_template(&mismatch),
        Err(TitleTemplateError::InvalidClocks(TitleClockError::FormatMismatch { element, .. }))
            if element == element_id(999)
    ));
    // Only exactly representable clocks are stored.
    let mut fractional = stored.clone();
    for clock in &mut fractional.clocks {
        if let TitleClockKind::CountdownTo { target, .. } = &mut clock.kind {
            *target = UtcInstant::new(target.unix_seconds(), 1).unwrap();
        }
    }
    assert!(matches!(
        encode_title_template(&fractional),
        Err(TitleTemplateError::UnencodableClock { .. })
    ));
    let mut short_zone = stored.clone();
    for clock in &mut short_zone.clocks {
        if let TitleClockKind::TimeOfDay { zone, .. } = &mut clock.kind {
            *zone = TimeZone::fixed("Z", 0).unwrap();
        }
    }
    assert!(matches!(
        encode_title_template(&short_zone),
        Err(TitleTemplateError::UnencodableClock { .. })
    ));

    // Strict decoding of clock content.
    for (from, to) in [
        // Non-canonical, invalid, or absent zones.
        ("\"zone\": \"UTC0\"", "\"zone\": \"UTC+0\""),
        ("\"zone\": \"UTC0\"", "\"zone\": \"Europe/Paris\""),
        ("\"zone\": \"UTC0\", ", ""),
        // Targets are exactly YYYY-MM-DDTHH:MM:SSZ.
        ("2026-10-14T23:00:00Z", "2026-10-14T23:00:00.5Z"),
        ("2026-10-14T23:00:00Z", "2026-10-14 23:00:00Z"),
        ("2026-10-14T23:00:00Z", "2026-02-30T23:00:00Z"),
        ("2026-10-14T23:00:00Z", "2026-10-14T24:00:00Z"),
        ("2026-10-14T23:00:00Z", "+026-10-14T23:00:00Z"),
        ("\"overrun\": \"continue\"", "\"overrun\": \"wrap\""),
        ("\"kind\": \"since_take\"", "\"kind\": \"stopwatch\""),
        // Formats of the wrong kind, unknown presets, stray or missing rates.
        (
            "\"kind\": \"time_of_day\", \"zone\": \"UTC0\", \"format\": {\"type\": \"time_of_day\"",
            "\"kind\": \"time_of_day\", \"zone\": \"UTC0\", \"format\": {\"type\": \"duration\"",
        ),
        (
            "\"kind\": \"since_take\", \"format\": {\"type\": \"duration\"",
            "\"kind\": \"since_take\", \"format\": {\"type\": \"time_of_day\"",
        ),
        ("\"preset\": \"compact\"", "\"preset\": \"tiny\""),
        (
            "\"type\": \"duration\", \"preset\": \"seconds\"",
            "\"type\": \"span\", \"preset\": \"seconds\"",
        ),
        (
            "\"preset\": \"seconds\"}",
            "\"preset\": \"seconds\", \"frame_rate\": {\"numerator\": 25, \"denominator\": 1}}",
        ),
        (
            ", \"frame_rate\": {\"numerator\": 30000, \"denominator\": 1001}",
            "",
        ),
        (
            "\"numerator\": 30000, \"denominator\": 1001",
            "\"numerator\": 30000, \"denominator\": 0",
        ),
        // Clock names follow the registry rules.
        ("\"name\": \"To_0\"", "\"name\": \"To 0\""),
        (
            "\"clock\": \"Local0\", \"format\": null",
            "\"clock\": \"\", \"format\": null",
        ),
        // Elements need both keys.
        (
            "\"clock\": \"Local0\", \"format\": null",
            "\"clock\": \"Local0\"",
        ),
        (
            "\"name\": \"Since-0\", ",
            "\"name\": \"Since-0\", \"extra\": 1, ",
        ),
    ] {
        assert!(encoded.contains(from), "fixture lacks {from}");
        let document = encoded.replacen(from, to, 1);
        assert!(
            matches!(
                decode_title_template(&document),
                Err(TitleTemplateError::Malformed { .. })
            ),
            "{from} -> {to} decoded: {:?}",
            decode_title_template(&document)
        );
    }
    // The clocks key itself is required.
    let without = encoded.replacen(
        &encoded[encoded.find(",\n  \"clocks\"").unwrap()..encoded.len() - 2],
        "",
        1,
    );
    assert!(matches!(
        decode_title_template(&without),
        Err(TitleTemplateError::Malformed { .. })
    ));
}

#[test]
fn imports_keep_the_source_clock_table() {
    let temp = TestDirectory::new("import-clocks");
    let store = temp.store("show.freemix");
    let mut template = shape_template("Clock");
    let mut clock = element(
        5,
        ElementKind::ProductionClock(ProductionClockSpec::new(clock_name("Doors"))),
        None,
    );
    clock.style.font = None;
    template.elements.push(clock);
    let clocks = vec![TitleClock {
        name: clock_name("Doors"),
        kind: TitleClockKind::CountdownTo {
            target: UtcInstant::from_utc_civil(2026, 10, 14, 23, 0, 0, 0).unwrap(),
            overrun: OverrunPolicy::Continue,
            format: DurationFormat::MinutesSeconds,
        },
    }];
    // A production clock draws text, so it needs a font family; without one
    // the template is structurally invalid.
    let path = temp.path("doors.fmtitle");
    let no_font = source_document_with_clocks(&shape_template("Clock"), Vec::new()).replacen(
        "\"elements\": [",
        "\"elements\": [\n      {\"id\": 5, \"name\": \"Doors\", \"bounds\": {\"x\": 0, \"y\": 0, \"width\": 10, \"height\": 10}, \"z_index\": 0, \"visible\": true, \"style\": {\"fill\": \"#ffffffff\", \"background\": null, \"opacity\": 255, \"font\": null, \"alignment\": {\"horizontal\": \"start\", \"vertical\": \"start\"}}, \"color_field\": null, \"kind\": {\"type\": \"production_clock\", \"clock\": \"Doors\", \"format\": null}, \"animations\": []},",
        1,
    );
    write(&path, no_font.as_bytes());
    assert!(store.import_title_template(&path, &[]).is_err());
    assert_eq!(assets_listing(&store), Vec::<String>::new());

    // With a font family the clock table travels into the stored template.
    template.elements[1].style = style(Some("Sans"));
    let path = temp.path("doors-font.fmtitle");
    write(
        &path,
        source_document_with_clocks(&template, clocks.clone()).as_bytes(),
    );
    let Some(font) = discover_font() else {
        return;
    };
    let imported = store
        .import_title_template(&path, &[("Sans".into(), font)])
        .unwrap();
    assert_eq!(imported.stored.clocks, clocks);
    let loaded = store.load_title_template(&imported.template_uri).unwrap();
    assert_eq!(loaded, imported.stored);
}

#[test]
fn documents_over_the_size_cap_are_refused() {
    let encoded = encode_title_template(&rich_template()).unwrap();
    let cap = usize::try_from(fm_persistence::MAX_TITLE_TEMPLATE_BYTES).unwrap();
    let padded = format!("{encoded}{}", " ".repeat(cap - encoded.len()));
    assert_eq!(padded.len(), cap);
    decode_title_template(&padded).unwrap();
    let oversized = format!("{padded} ");
    assert!(matches!(
        decode_title_template(&oversized),
        Err(TitleTemplateError::TooLarge { .. })
    ));

    let temp = TestDirectory::new("size-cap");
    let store = temp.store("show.freemix");
    write(
        &store.assets_root().join("titles/big.fmtitle"),
        oversized.as_bytes(),
    );
    assert!(matches!(
        store.load_title_template("asset://titles/big.fmtitle"),
        Err(TitleTemplateError::TooLarge { .. })
    ));
    let source = temp.path("big.fmtitle");
    write(&source, oversized.as_bytes());
    assert!(matches!(
        store.import_title_template(&source, &[]),
        Err(TitleImportError::Template(
            TitleTemplateError::TooLarge { .. }
        ))
    ));
}

#[test]
fn a_compact_source_whose_stored_form_exceeds_the_cap_is_refused() {
    // The stored layout spends more bytes per element than a compact source,
    // so a source under the cap can encode over it; storing that document
    // would leave a template that never loads again.
    let cap = usize::try_from(fm_persistence::MAX_TITLE_TEMPLATE_BYTES).unwrap();
    let mut source = String::from(
        r##"{"schema_version":29,"template":{"id":1,"name":"Big","width":16,"height":9,"background":"#00000000","fields":[],"elements":["##,
    );
    let mut id = 1_u32;
    while source.len() < cap - 1024 {
        if id > 1 {
            source.push(',');
        }
        write!(
            source,
            r##"{{"id":{id},"name":"r","bounds":{{"x":0,"y":0,"width":1,"height":1}},"z_index":0,"visible":true,"style":{{"fill":"#ffffffff","background":null,"opacity":255,"font":null,"alignment":{{"horizontal":"start","vertical":"start"}}}},"color_field":null,"kind":{{"type":"rectangle"}},"animations":[]}}"##
        )
        .unwrap();
        id += 1;
    }
    source.push_str(r#"]},"fonts":[],"clocks":[]}"#);
    assert!(source.len() <= cap);

    let temp = TestDirectory::new("encoded-cap");
    let store = temp.store("show.freemix");
    let path = temp.path("big.fmtitle");
    write(&path, source.as_bytes());
    assert!(
        matches!(
            store.import_title_template(&path, &[]),
            Err(TitleImportError::Template(
                TitleTemplateError::TooLarge { bytes, .. }
            )) if bytes > fm_persistence::MAX_TITLE_TEMPLATE_BYTES
        ),
        "an oversized stored form must be refused"
    );
    assert_eq!(assets_listing(&store), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[test]
fn imports_store_the_template_and_dedupe_identical_documents() {
    let temp = TestDirectory::new("import-shape");
    let store = temp.store("show.freemix");
    let source = write_source(&temp, "lower third.fmtitle", &shape_template("Bar"));

    let imported = store.import_title_template(&source, &[]).unwrap();
    assert_eq!(imported.template_uri, "asset://titles/lower third.fmtitle");
    assert!(matches!(
        imported.disposition,
        AssetDisposition::Copied { .. }
    ));
    assert_eq!(imported.fonts, Vec::new());
    assert_eq!(imported.stored.template, shape_template("Bar"));
    assert_eq!(
        store.load_title_template(&imported.template_uri).unwrap(),
        imported.stored
    );

    // The same document again reuses the stored file.
    let again = store.import_title_template(&source, &[]).unwrap();
    assert_eq!(again.template_uri, imported.template_uri);
    assert_eq!(again.disposition, AssetDisposition::Reused);

    // A different document with the same name gets a fresh key.
    let other = write_source(&temp, "other/lower third.fmtitle", &shape_template("Other"));
    let other = store.import_title_template(&other, &[]).unwrap();
    assert_eq!(other.template_uri, "asset://titles/lower third-2.fmtitle");
    assert_eq!(
        assets_listing(&store),
        ["titles/lower third-2.fmtitle", "titles/lower third.fmtitle"]
    );
}

#[test]
fn imports_refuse_bad_sources_and_fonts_before_copying_anything() {
    let temp = TestDirectory::new("import-refusals");
    let store = temp.store("show.freemix");

    // The source must leave fonts to the caller.
    let declared = temp.path("declared.fmtitle");
    write(
        &declared,
        encode_title_template(&StoredTitleTemplate {
            template: text_template("Sans"),
            fonts: vec![TitleFontAsset {
                family: "Sans".into(),
                asset_uri: "asset://fonts/Sans.ttf".into(),
            }],
            clocks: Vec::new(),
        })
        .unwrap()
        .as_bytes(),
    );
    assert!(matches!(
        store.import_title_template(&declared, &[]),
        Err(TitleImportError::SourceDeclaresFonts)
    ));

    let text = write_source(&temp, "strap.fmtitle", &text_template("Sans"));
    let garbage = temp.path("garbage.ttf");
    write(&garbage, b"not a font at all");
    assert!(matches!(
        store.import_title_template(&text, &[]),
        Err(TitleImportError::Template(TitleTemplateError::MissingFontFamily(family)))
            if family == "Sans"
    ));
    assert!(matches!(
        store.import_title_template(&text, &[("Serif".into(), garbage.clone())]),
        Err(TitleImportError::Template(TitleTemplateError::MissingFontFamily(family)))
            if family == "Sans"
    ));
    assert!(matches!(
        store.import_title_template(
            &text,
            &[
                ("Sans".into(), garbage.clone()),
                ("Sans".into(), garbage.clone())
            ]
        ),
        Err(TitleImportError::Template(
            TitleTemplateError::DuplicateFontFamily(_)
        ))
    ));
    assert!(matches!(
        store.import_title_template(
            &text,
            &[("Sans".into(), garbage.clone()), ("Extra".into(), garbage.clone())]
        ),
        Err(TitleImportError::Template(TitleTemplateError::UnusedFontFamily(family)))
            if family == "Extra"
    ));
    assert!(matches!(
        store.import_title_template(&text, &[("Sans".into(), garbage.clone())]),
        Err(TitleImportError::UnparsableFont { family, error: FontError::Unparsable })
            if family == "Sans"
    ));
    let empty = temp.path("empty.ttf");
    write(&empty, b"");
    assert!(matches!(
        store.import_title_template(&text, &[("Sans".into(), empty)]),
        Err(TitleImportError::UnparsableFont {
            error: FontError::Empty,
            ..
        })
    ));
    assert!(matches!(
        store.import_title_template(&text, &[("Sans".into(), temp.path("missing.ttf"))]),
        Err(TitleImportError::FontUnreadable { .. })
    ));

    // Image defaults must already be in the bundle.
    let mut logo = shape_template("Logo");
    logo.fields.push(FieldDefinition {
        id: field_id(9),
        name: "Logo".into(),
        default: FieldValue::Image(ImageSource::new("asset://images/logo.png")),
    });
    let logo_source = write_source(&temp, "logo.fmtitle", &logo);
    assert!(matches!(
        store.import_title_template(&logo_source, &[]),
        Err(TitleImportError::UnresolvedImage { field, .. }) if field == field_id(9)
    ));

    // A production clock element draws text, so it needs a font.
    let clock_source = temp.path("clock.fmtitle");
    let document = source_document(&shape_template("Clock")).replacen(
        "{\"type\": \"rectangle\"}",
        "{\"type\": \"production_clock\", \"clock\": \"Show\", \"format\": null}",
        1,
    );
    write(&clock_source, document.as_bytes());
    assert!(matches!(
        store.import_title_template(&clock_source, &[]),
        Err(TitleImportError::Template(
            TitleTemplateError::InvalidTemplate(_)
        ))
    ));

    assert_eq!(assets_listing(&store), Vec::<String>::new());

    // With the image in place the import succeeds.
    write(&store.assets_root().join("images/logo.png"), b"png");
    store.import_title_template(&logo_source, &[]).unwrap();
}

#[test]
fn fonts_are_validated_copied_and_deduplicated() {
    let Some(font) = discover_font() else {
        return;
    };
    let temp = TestDirectory::new("import-fonts");
    let store = temp.store("show.freemix");
    let copy = temp.path("fonts/Brand Sans.ttf");
    write(&copy, &fs::read(&font).unwrap());

    let first = write_source(&temp, "strap.fmtitle", &text_template("Brand Sans"));
    let imported = store
        .import_title_template(&first, &[("Brand Sans".into(), copy.clone())])
        .unwrap();
    assert_eq!(imported.fonts.len(), 1);
    assert_eq!(imported.fonts[0].family, "Brand Sans");
    assert_eq!(imported.fonts[0].asset_uri, "asset://fonts/Brand Sans.ttf");
    assert!(matches!(
        imported.fonts[0].disposition,
        AssetDisposition::Copied { .. }
    ));
    assert_eq!(
        imported.stored.fonts,
        vec![TitleFontAsset {
            family: "Brand Sans".into(),
            asset_uri: "asset://fonts/Brand Sans.ttf".into(),
        }]
    );

    // A second template sharing the same file reuses it.
    let mut second_template = text_template("Brand Sans");
    second_template.name = "Second".into();
    let second = write_source(&temp, "second.fmtitle", &second_template);
    let reimported = store
        .import_title_template(&second, &[("Brand Sans".into(), copy.clone())])
        .unwrap();
    assert_eq!(reimported.fonts[0].asset_uri, imported.fonts[0].asset_uri);
    assert_eq!(reimported.fonts[0].disposition, AssetDisposition::Reused);

    // A different file under the same name gets a fresh key.
    let mut altered = fs::read(&font).unwrap();
    altered.extend_from_slice(&[0; 4]);
    let other = temp.path("other/Brand Sans.ttf");
    write(&other, &altered);
    if fm_titles::FontFace::from_bytes(altered).is_ok() {
        let third = store
            .import_title_template(&first, &[("Brand Sans".into(), other)])
            .unwrap();
        assert_eq!(third.fonts[0].asset_uri, "asset://fonts/Brand Sans-2.ttf");
        assert_eq!(third.template_uri, "asset://titles/strap-2.fmtitle");
    }
    assert_eq!(store.audit_assets(&stored(empty_project(1))), Vec::new());
}

// ---------------------------------------------------------------------------
// Validation and audit
// ---------------------------------------------------------------------------

#[test]
fn title_fields_must_exactly_cover_the_template() {
    let temp = TestDirectory::new("validate-titles");
    let store = temp.store("show.freemix");
    let uri = store_template(
        &store,
        "titles/bar.fmtitle",
        shape_template("Bar"),
        Vec::new(),
    );
    let defaults = title_field_defaults(&shape_template("Bar"));

    let mut project = empty_project(1);
    project.add_input(title_input(1, &uri, defaults.clone()));
    project.add_input(title_input(
        2,
        &uri,
        vec![
            entry(1, TitleFieldValue::Text("not a number".into())),
            entry(3, TitleFieldValue::Number(1)),
        ],
    ));
    project.add_input(title_input(3, "asset://titles/missing.fmtitle", Vec::new()));
    let issues = store.validate_titles(&stored(project));
    assert_eq!(issues.len(), 4, "{issues:?}");
    assert_eq!(
        issues[..3],
        [
            TitleIssue {
                input_id: input_id(2),
                problem: TitleProblem::TypeMismatch {
                    field: TitleFieldId::new(nz(1)),
                    expected: FieldType::Number,
                    actual: FieldType::Text,
                },
            },
            TitleIssue {
                input_id: input_id(2),
                problem: TitleProblem::MissingField(TitleFieldId::new(nz(2))),
            },
            TitleIssue {
                input_id: input_id(2),
                problem: TitleProblem::UnknownField(TitleFieldId::new(nz(3))),
            },
        ]
    );
    assert!(matches!(
        &issues[3],
        TitleIssue {
            input_id: id,
            problem: TitleProblem::UnreadableTemplate(_),
        } if *id == input_id(3)
    ));

    let mut clean = empty_project(1);
    clean.add_input(title_input(1, &uri, defaults));
    assert_eq!(store.validate_titles(&stored(clean)), Vec::new());
}

#[test]
fn audit_reports_missing_templates_fonts_and_images() {
    let temp = TestDirectory::new("audit-titles");
    let store = temp.store("show.freemix");
    let mut logo = text_template("Sans");
    logo.fields.push(FieldDefinition {
        id: field_id(2),
        name: "Logo".into(),
        default: FieldValue::Image(ImageSource::new("asset://images/default.png")),
    });
    let font = TitleFontAsset {
        family: "Sans".into(),
        asset_uri: "asset://fonts/Sans.ttf".into(),
    };
    let uri = store_template(&store, "titles/strap.fmtitle", logo, vec![font]);
    write(&store.assets_root().join("titles/corrupt.fmtitle"), b"{");
    write(&store.assets_root().join("images/live.png"), b"png");

    let fields = |image: &str| {
        vec![
            entry(1, TitleFieldValue::Text("Ana".into())),
            entry(
                2,
                TitleFieldValue::Image {
                    asset_uri: image.into(),
                },
            ),
        ]
    };
    let mut project = empty_project(1);
    project.add_input(title_input(1, &uri, fields("asset://images/live.png")));
    project.add_input(title_input(2, "asset://titles/missing.fmtitle", Vec::new()));
    project.add_input(title_input(3, "asset://titles/corrupt.fmtitle", Vec::new()));
    project.add_input(title_input(4, &uri, fields("asset://images/gone.png")));
    project.add_input(title_input(5, "titles/strap.fmtitle", Vec::new()));
    let project = stored(project);

    let issue = |input, reason| AssetAuditIssue {
        input_id: input_id(input),
        reason,
    };
    // Input 1: the font and the default image are missing; its own image is
    // present. Input 4 additionally misses its own image.
    assert_eq!(
        store.audit_assets(&project),
        [
            issue(1, AssetAuditReason::MissingAsset),
            issue(1, AssetAuditReason::MissingAsset),
            issue(2, AssetAuditReason::MissingAsset),
            issue(3, AssetAuditReason::UnreadableTitleTemplate),
            issue(4, AssetAuditReason::MissingAsset),
            issue(4, AssetAuditReason::MissingAsset),
            issue(4, AssetAuditReason::MissingAsset),
            issue(5, AssetAuditReason::InvalidUri),
        ]
    );

    write(&store.assets_root().join("fonts/Sans.ttf"), b"font bytes");
    write(&store.assets_root().join("images/default.png"), b"png");
    write(&store.assets_root().join("images/gone.png"), b"png");
    assert_eq!(
        store.audit_assets(&project),
        [
            issue(2, AssetAuditReason::MissingAsset),
            issue(3, AssetAuditReason::UnreadableTitleTemplate),
            issue(5, AssetAuditReason::InvalidUri),
        ]
    );
    // Titles are never offered for media relink.
    let plan = store
        .plan_media_relink(&project, &fm_persistence::RelinkSearch::new(Vec::new()))
        .unwrap();
    assert_eq!(plan.entries, Vec::new());
}

// ---------------------------------------------------------------------------
// Export and append
// ---------------------------------------------------------------------------

/// A bundle with one title input whose template, font, and images resolve.
fn titled_bundle(temp: &TestDirectory, name: &str, template_key: &str) -> ProjectStore {
    let store = temp.store(name);
    let mut template = text_template("Sans");
    template.fields.push(FieldDefinition {
        id: field_id(2),
        name: "Logo".into(),
        default: FieldValue::Image(ImageSource::new("asset://images/default.png")),
    });
    let uri = store_template(
        &store,
        template_key,
        template,
        vec![TitleFontAsset {
            family: "Sans".into(),
            asset_uri: "asset://fonts/Sans.ttf".into(),
        }],
    );
    write(&store.assets_root().join("fonts/Sans.ttf"), b"font bytes");
    write(&store.assets_root().join("images/default.png"), b"default");
    write(&store.assets_root().join("images/live.png"), b"live");
    let mut project = empty_project(1);
    project.add_input(title_input(
        1,
        &uri,
        vec![
            entry(1, TitleFieldValue::Text("Ana".into())),
            entry(
                2,
                TitleFieldValue::Image {
                    asset_uri: "asset://images/live.png".into(),
                },
            ),
        ],
    ));
    store.save(&stored(project)).unwrap();
    store
}

#[test]
fn portable_export_copies_every_title_asset_verbatim() {
    let temp = TestDirectory::new("export-titles");
    let source = titled_bundle(&temp, "show.freemix", "titles/strap.fmtitle");
    let destination = temp.store("copy.freemix");

    let export = source.export_portable_bundle(&destination).unwrap();
    assert_eq!(export.rewritten_inputs, Vec::new());
    assert_eq!(
        export
            .assets
            .iter()
            .map(|asset| (asset.source_uri.as_str(), asset.bundled_uri.as_str()))
            .collect::<Vec<_>>(),
        [
            ("asset://fonts/Sans.ttf", "asset://fonts/Sans.ttf"),
            ("asset://images/default.png", "asset://images/default.png"),
            ("asset://images/live.png", "asset://images/live.png"),
            (
                "asset://titles/strap.fmtitle",
                "asset://titles/strap.fmtitle"
            ),
        ]
    );
    let loaded = destination.load().unwrap();
    assert_eq!(loaded.project(), export.project.project());
    assert_eq!(destination.audit_assets(&loaded), Vec::new());
    assert_eq!(destination.validate_titles(&loaded), Vec::new());
    assert_eq!(
        fs::read(destination.assets_root().join("fonts/Sans.ttf")).unwrap(),
        b"font bytes"
    );

    // A missing title asset refuses the export.
    fs::remove_file(source.assets_root().join("fonts/Sans.ttf")).unwrap();
    let error = source
        .export_portable_bundle(&temp.store("missing.freemix"))
        .unwrap_err();
    assert!(
        matches!(&error, BundleError::UnresolvedAssets(issues) if issues == &[AssetAuditIssue {
            input_id: input_id(1),
            reason: AssetAuditReason::MissingAsset,
        }]),
        "got {error:?}"
    );
    assert!(!temp.path("missing.freemix").exists());
}

#[test]
fn portable_export_refuses_a_title_key_that_would_need_rewriting() {
    let temp = TestDirectory::new("export-rewrite");
    let source = titled_bundle(&temp, "show.freemix", "titles/CON.fmtitle");
    let destination = temp.store("copy.freemix");
    let error = source.export_portable_bundle(&destination).unwrap_err();
    assert!(
        matches!(&error, BundleError::TitleAssetRewriteUnsupported { uri }
            if uri == "asset://titles/CON.fmtitle"),
        "got {error:?}"
    );
    assert!(!destination.root().exists());
}

#[test]
#[cfg(unix)]
fn portable_export_renames_media_rather_than_a_title_key_it_folds_onto() {
    // `images/live*.png` is portable as `images/live_.png` and sorts before
    // the title image `images/live_.png`; the title key must still win.
    let temp = TestDirectory::new("export-title-priority");
    let source = titled_bundle(&temp, "show.freemix", "titles/strap.fmtitle");
    write(
        &source.assets_root().join("images/live_.png"),
        b"title image",
    );
    write(&source.assets_root().join("images/live*.png"), b"media");
    let loaded = source.load().unwrap();
    let mut project = loaded.project().clone();
    project
        .set_title_fields(
            input_id(1),
            1,
            vec![
                entry(1, TitleFieldValue::Text("Ana".into())),
                entry(
                    2,
                    TitleFieldValue::Image {
                        asset_uri: "asset://images/live_.png".into(),
                    },
                ),
            ],
        )
        .unwrap();
    project.add_input(Input {
        id: input_id(2),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://images/live*.png".into(),
        },
        required_capabilities: Vec::new(),
    });
    source.save(&loaded.with_project(project).unwrap()).unwrap();

    let destination = temp.store("copy.freemix");
    let export = source.export_portable_bundle(&destination).unwrap();
    assert_eq!(export.rewritten_inputs, [input_id(2)]);
    let bundled = |uri: &str| {
        export
            .assets
            .iter()
            .find(|asset| asset.source_uri == uri)
            .map(|asset| asset.bundled_uri.clone())
    };
    assert_eq!(
        bundled("asset://images/live_.png").as_deref(),
        Some("asset://images/live_.png")
    );
    assert_eq!(
        bundled("asset://images/live*.png").as_deref(),
        Some("asset://images/live_-2.png")
    );
    let loaded = destination.load().unwrap();
    assert_eq!(destination.audit_assets(&loaded), Vec::new());
    assert_eq!(
        fs::read(destination.assets_root().join("images/live_.png")).unwrap(),
        b"title image"
    );
}

#[test]
fn append_carries_title_assets_and_refuses_key_clashes() {
    let temp = TestDirectory::new("append-titles");
    let source = titled_bundle(&temp, "source.freemix", "titles/strap.fmtitle");

    let target = temp.store("target.freemix");
    target.save(&stored(empty_project(2))).unwrap();
    // An identical file already at a title key is reused.
    write(&target.assets_root().join("fonts/Sans.ttf"), b"font bytes");
    let (appended, report) = target
        .append_project(&target.load().unwrap(), &source)
        .unwrap();
    assert_eq!(report.assets.len(), 4);
    assert!(
        report
            .assets
            .iter()
            .all(|asset| asset.source_uri == asset.target_uri)
    );
    let InputKind::Title(title) = &appended.project().inputs()[0].kind else {
        panic!("appended input is not a title");
    };
    assert_eq!(title.template_uri, "asset://titles/strap.fmtitle");
    assert_eq!(target.audit_assets(&appended), Vec::new());
    assert_eq!(target.validate_titles(&appended), Vec::new());

    // A different file at a title key refuses the append before copying.
    let clashing = temp.store("clash.freemix");
    clashing.save(&stored(empty_project(3))).unwrap();
    write(&clashing.assets_root().join("images/live.png"), b"other");
    let before = assets_listing(&clashing);
    let error = clashing
        .append_project(&clashing.load().unwrap(), &source)
        .unwrap_err();
    assert!(
        matches!(&error, BundleError::TitleAssetRewriteUnsupported { uri }
            if uri == "asset://images/live.png"),
        "got {error:?}"
    );
    assert_eq!(assets_listing(&clashing), before);

    // A title asset missing in the source refuses the append too.
    fs::remove_file(source.assets_root().join("images/default.png")).unwrap();
    let fresh = temp.store("fresh.freemix");
    fresh.save(&stored(empty_project(4))).unwrap();
    assert!(matches!(
        fresh.append_project(&fresh.load().unwrap(), &source),
        Err(BundleError::UnresolvedAssets(_))
    ));
    assert_eq!(assets_listing(&fresh), Vec::<String>::new());
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

fn discover_font() -> Option<PathBuf> {
    let found = std::env::var_os("FM_TITLES_FONT")
        .map(PathBuf::from)
        .or_else(|| {
            DIRECT_FONTS
                .iter()
                .map(PathBuf::from)
                .find(|path| usable_font(path))
        })
        .or_else(|| {
            FONT_ROOTS
                .iter()
                .find_map(|root| scan_for_font(Path::new(root), 0))
        });
    if found.is_none() {
        assert!(
            std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
            "FM_REQUIRE_FONT=1 but no usable font file was found"
        );
        eprintln!("skipping font import test: no font file available");
    }
    found
}

fn usable_font(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| fm_titles::FontFace::from_bytes(bytes).is_ok())
}

fn scan_for_font(directory: &Path, depth: u32) -> Option<PathBuf> {
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
        let candidate = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("ttf") || extension.eq_ignore_ascii_case("otf")
            });
        if candidate && usable_font(path) {
            return Some(path.clone());
        }
    }
    entries
        .iter()
        .filter(|path| path.is_dir())
        .find_map(|path| scan_for_font(path, depth + 1))
}
