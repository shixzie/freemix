//! GX-005 slice: visibility, image URI, number/date formatting, list-row
//! binding, and preview/commit state.

use std::{collections::BTreeMap, str::FromStr};

use fm_data::{
    BindingConfigError, BindingError, BindingKind, BindingReport, BindingSet, BindingStatus,
    BoundValue, BoundValueKind, CoercionPolicy, CompareOp, DataBinding, DataPath, DataValue,
    DateInput, DatePattern, DateTimeFormat, Decimal, DecimalRounding, EdgeBehavior, FieldChange,
    FormatError, ImageUriError, ImageUriPolicy, NumberFormat, ParseLimits, PreviewError,
    PreviewState, RowBindingError, RowCursor, RowNavigation, SignStyle, StagePolicy, Timestamp,
    TitleColor, TruthyRule, UtcOffset, VisibilityError, VisibilityRule, parse_json, rows_at,
};

fn path(text: &str) -> DataPath {
    DataPath::from_str(text).unwrap()
}

fn decimal(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap()
}

fn json(text: &str) -> DataValue {
    parse_json(text.as_bytes(), &ParseLimits::default()).unwrap()
}

// ---------------------------------------------------------------- numbers

#[test]
fn number_format_rounds_exactly_and_groups_thousands() {
    let money = NumberFormat::fixed(2)
        .with_thousands(',')
        .with_prefix("$")
        .with_sign(SignStyle::Parentheses);
    assert_eq!(
        money.format_decimal(decimal("1234567.005")).unwrap(),
        "$1,234,567.01"
    );
    assert_eq!(
        money.format_decimal(decimal("-1234.5")).unwrap(),
        "($1,234.50)"
    );
    assert_eq!(
        money.format_decimal(decimal("999.999")).unwrap(),
        "$1,000.00"
    );
    // Negative values that round to zero are never signed.
    assert_eq!(money.format_decimal(decimal("-0.004")).unwrap(), "$0.00");

    let european = NumberFormat::fixed(1)
        .with_thousands('.')
        .with_decimal_separator(',')
        .with_suffix(" km")
        .with_sign(SignStyle::Always);
    assert_eq!(
        european.format_value(&DataValue::Integer(12_345)).unwrap(),
        "+12.345,0 km"
    );
    assert_eq!(european.format_value(&"-0.25".into()).unwrap(), "-0,3 km");
    assert_eq!(
        european.format_value(&DataValue::Integer(0)).unwrap(),
        "0,0 km"
    );
}

#[test]
fn number_rounding_modes_and_extremes_are_exact() {
    let even = NumberFormat::fixed(0).with_rounding(DecimalRounding::HalfEven);
    let away = NumberFormat::fixed(0);
    let truncate = NumberFormat::fixed(0).with_rounding(DecimalRounding::Truncate);
    for (input, half_even, half_away, truncated) in [
        ("2.5", "2", "3", "2"),
        ("3.5", "4", "4", "3"),
        ("-2.5", "-2", "-3", "-2"),
        ("2.51", "3", "3", "2"),
        ("-0.5", "0", "-1", "0"),
    ] {
        assert_eq!(
            even.format_decimal(decimal(input)).unwrap(),
            half_even,
            "{input}"
        );
        assert_eq!(
            away.format_decimal(decimal(input)).unwrap(),
            half_away,
            "{input}"
        );
        assert_eq!(
            truncate.format_decimal(decimal(input)).unwrap(),
            truncated,
            "{input}"
        );
    }
    // Largest coefficient at the maximum number of decimals stays exact.
    let wide = NumberFormat::fixed(18).with_thousands(',');
    assert_eq!(
        wide.format_value(&DataValue::Integer(i64::MIN)).unwrap(),
        "-9,223,372,036,854,775,808.000000000000000000"
    );
    assert_eq!(
        NumberFormat::fixed(3)
            .format_decimal(decimal("0.1"))
            .unwrap(),
        "0.100"
    );
}

#[test]
fn number_format_rejects_bad_configuration_and_input() {
    assert_eq!(
        NumberFormat::fixed(19).validate(),
        Err(FormatError::DecimalsOutOfRange {
            limit: 18,
            actual: 19
        })
    );
    assert_eq!(
        NumberFormat::fixed(0).with_thousands('.').validate(),
        Err(FormatError::SeparatorConflict('.'))
    );
    assert_eq!(
        NumberFormat::fixed(0).with_thousands('5').validate(),
        Err(FormatError::InvalidSeparator('5'))
    );
    assert_eq!(
        NumberFormat::fixed(0).with_prefix("\n").validate(),
        Err(FormatError::AffixControlCharacter)
    );
    assert_eq!(
        NumberFormat::fixed(0).format_value(&"12abc".into()),
        Err(FormatError::NotANumber("12abc".into()))
    );
    assert!(matches!(
        NumberFormat::fixed(0).format_value(&DataValue::Bool(true)),
        Err(FormatError::UnsupportedValue { .. })
    ));
}

// ---------------------------------------------------------------- dates

#[test]
fn iso_and_unix_inputs_render_at_caller_offset() {
    let pattern = DatePattern::parse("%a %d %b %Y %H:%M:%S.%L %z").unwrap();
    let utc = DateTimeFormat::new(pattern.clone());
    assert_eq!(
        utc.format_value(&"2024-02-29T23:30:15.25+01:00".into())
            .unwrap(),
        "Thu 29 Feb 2024 22:30:15.250 +0000"
    );
    let eastern =
        DateTimeFormat::new(pattern).with_output_offset(UtcOffset::from_minutes(-300).unwrap());
    assert_eq!(
        eastern.format_value(&DataValue::Integer(0)).unwrap(),
        "Wed 31 Dec 1969 19:00:00.000 -0500"
    );
    assert_eq!(
        eastern
            .format_value(&DataValue::Decimal(decimal("1700000000.987")))
            .unwrap(),
        "Tue 14 Nov 2023 17:13:20.987 -0500"
    );

    let clock = DateTimeFormat::new(DatePattern::parse("%A %B %e, %I:%M %p (%j) %Z %%").unwrap())
        .with_output_offset(UtcOffset::from_hours_minutes(5, 30).unwrap());
    assert_eq!(
        clock.format_value(&"2023-12-31T20:00:00Z".into()).unwrap(),
        "Monday January  1, 01:30 AM (001) +05:30 %"
    );
}

#[test]
fn iso_parsing_is_strict_and_naive_text_needs_an_offset() {
    let pattern = DatePattern::parse("%Y-%m-%dT%H:%M:%S").unwrap();
    let strict = DateTimeFormat::new(pattern.clone());
    assert_eq!(
        strict.format_value(&"2024-06-01 12:00".into()),
        Err(FormatError::MissingOffset("2024-06-01 12:00".into()))
    );
    let naive =
        DateTimeFormat::new(pattern).with_naive_offset(UtcOffset::from_minutes(120).unwrap());
    assert_eq!(
        naive.format_value(&"2024-06-01".into()).unwrap(),
        "2024-05-31T22:00:00"
    );
    assert_eq!(
        naive
            .format_value(&"2024-06-01T12:00:00-0330".into())
            .unwrap(),
        "2024-06-01T15:30:00"
    );
    for bad in [
        "2023-02-29T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:00:60Z",
        "2024-01-01T00:00Z junk",
        "2024-01-01Z",
        "2024-01-01T00:00:00.1234567890Z",
        "24-01-01",
    ] {
        assert!(
            matches!(
                naive.format_value(&bad.into()),
                Err(FormatError::InvalidDateTime(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        Timestamp::parse_iso8601("1970-01-01T00:00:00.5Z", None).unwrap(),
        Timestamp {
            seconds: 0,
            nanos: 500_000_000
        }
    );
    assert_eq!(
        Timestamp::from_unix(decimal("-1.25")).unwrap(),
        Timestamp {
            seconds: -2,
            nanos: 750_000_000
        }
    );
}

#[test]
fn date_input_modes_ranges_and_pattern_validation() {
    let pattern = DatePattern::parse("%Y").unwrap();
    let unix = DateTimeFormat::new(pattern.clone()).with_input(DateInput::UnixSeconds);
    assert_eq!(unix.format_value(&"86400".into()).unwrap(), "1970");
    assert!(matches!(
        DateTimeFormat::new(pattern.clone())
            .with_input(DateInput::Iso8601)
            .format_value(&DataValue::Integer(0)),
        Err(FormatError::UnsupportedValue { .. })
    ));
    assert_eq!(
        unix.format_value(&DataValue::Integer(Timestamp::MAX_SECONDS + 1)),
        Err(FormatError::DateOutOfRange)
    );
    let shifted =
        DateTimeFormat::new(pattern).with_output_offset(UtcOffset::from_minutes(60).unwrap());
    assert_eq!(
        shifted.format_value(&"9999-12-31T23:30:00Z".into()),
        Err(FormatError::DateOutOfRange)
    );
    assert_eq!(
        shifted
            .format_value(&"0000-01-01T00:00:00Z".into())
            .unwrap(),
        "0000"
    );

    assert_eq!(
        DatePattern::parse("%Q"),
        Err(FormatError::UnknownDirective('Q'))
    );
    assert_eq!(
        DatePattern::parse("100%"),
        Err(FormatError::DanglingPercent)
    );
    assert!(matches!(
        DatePattern::parse(&"x".repeat(129)),
        Err(FormatError::PatternTooLong { .. })
    ));
    assert!(matches!(
        DatePattern::parse(&"%Y".repeat(33)),
        Err(FormatError::TooManyDirectives { .. })
    ));
    assert!(UtcOffset::from_minutes(18 * 60 + 1).is_err());
    assert!(UtcOffset::from_hours_minutes(1, 60).is_err());
}

// ---------------------------------------------------------------- visibility

#[test]
fn visibility_rules_cover_explicit_non_empty_and_compare() {
    let explicit = VisibilityRule::new(TruthyRule::Explicit);
    for (value, expected) in [
        (DataValue::Bool(true), true),
        (DataValue::Integer(0), false),
        (" Yes ".into(), true),
        ("OFF".into(), false),
        ("1".into(), true),
    ] {
        assert_eq!(explicit.evaluate(&value), Ok(expected), "{value:?}");
    }
    assert_eq!(
        explicit.evaluate(&"maybe".into()),
        Err(VisibilityError::NotBoolean("maybe".into()))
    );
    assert!(explicit.evaluate(&DataValue::Integer(2)).is_err());

    let non_empty = VisibilityRule::new(TruthyRule::NonEmpty);
    assert_eq!(non_empty.evaluate(&"  ".into()), Ok(false));
    assert_eq!(non_empty.evaluate(&DataValue::List(vec![])), Ok(false));
    assert_eq!(non_empty.evaluate(&DataValue::Integer(0)), Ok(true));
    assert_eq!(
        non_empty.clone().inverted().evaluate(&"x".into()),
        Ok(false)
    );

    let leading = VisibilityRule::new(TruthyRule::Compare {
        op: CompareOp::Gt,
        operand: decimal("2.5"),
    });
    assert_eq!(leading.evaluate(&DataValue::Integer(3)), Ok(true));
    assert_eq!(leading.evaluate(&"2.50".into()), Ok(false));
    assert_eq!(
        leading.evaluate(&DataValue::Decimal(decimal("2.500001"))),
        Ok(true)
    );
    assert!(matches!(
        leading.evaluate(&DataValue::Bool(true)),
        Err(VisibilityError::NotNumeric { .. })
    ));
    for (op, expected) in [
        (CompareOp::Eq, true),
        (CompareOp::Ne, false),
        (CompareOp::Lt, false),
        (CompareOp::Le, true),
        (CompareOp::Ge, true),
    ] {
        let rule = VisibilityRule::new(TruthyRule::Compare {
            op,
            operand: decimal("10"),
        });
        assert_eq!(
            rule.evaluate(&DataValue::Integer(10)),
            Ok(expected),
            "{op:?}"
        );
    }
}

// ---------------------------------------------------------------- image URIs

#[test]
fn image_uris_validate_scheme_length_and_characters() {
    let policy = ImageUriPolicy::default();
    assert_eq!(
        policy.check("  HTTPS://cdn.example/a.png?v=1 ").unwrap(),
        "https://cdn.example/a.png?v=1"
    );
    assert_eq!(
        policy.check("file:///srv/logo.png").unwrap(),
        "file:///srv/logo.png"
    );
    assert_eq!(
        policy.check("ftp://host/x.png"),
        Err(ImageUriError::SchemeNotAllowed("ftp".into()))
    );
    assert_eq!(
        policy.check("https:///x.png"),
        Err(ImageUriError::MissingAuthority)
    );
    assert_eq!(
        policy.check("http:x.png"),
        Err(ImageUriError::MissingAuthority)
    );
    assert_eq!(
        policy.check("logos/a.png"),
        Err(ImageUriError::MissingScheme)
    );
    assert_eq!(
        policy.check(r"C:\logos\a.png"),
        Err(ImageUriError::MissingScheme)
    );
    assert_eq!(
        policy.check("https://a b/x"),
        Err(ImageUriError::InvalidCharacter)
    );
    assert_eq!(policy.check("   "), Err(ImageUriError::Empty));

    let paths = ImageUriPolicy {
        allowed_schemes: vec!["asset".into()],
        allow_paths: true,
        max_bytes: 16,
    };
    assert_eq!(paths.check(r"C:\logos\a.png").unwrap(), r"C:\logos\a.png");
    assert_eq!(paths.check("asset:team/7").unwrap(), "asset:team/7");
    assert_eq!(
        paths.check("asset:team/7/logo.png"),
        Err(ImageUriError::TooLong {
            limit: 16,
            actual: 21
        })
    );
    let bad_policy = ImageUriPolicy {
        allowed_schemes: vec!["HTTPS".into()],
        ..ImageUriPolicy::default()
    };
    assert_eq!(
        bad_policy.check("https://a/b"),
        Err(ImageUriError::InvalidPolicy)
    );
}

// ---------------------------------------------------------------- binding set

fn scoreboard_bindings() -> BindingSet {
    BindingSet::new(
        vec![
            DataBinding::new("Name.Text", path("name"), BindingKind::Text),
            DataBinding::new(
                "Score.Text",
                path("score"),
                BindingKind::FormattedNumber(NumberFormat::fixed(1).with_thousands(',')),
            ),
            DataBinding::new("Goals", path("goals"), BindingKind::Number),
            DataBinding::new("Accent", path("color"), BindingKind::Color).with_fallback(
                BoundValue::Color(TitleColor {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
            ),
            DataBinding::new(
                "Logo.Source",
                path("logo"),
                BindingKind::Image(ImageUriPolicy::default()),
            )
            .optional(),
            DataBinding::new(
                "Kickoff.Text",
                path("kickoff"),
                BindingKind::FormattedDateTime(
                    DateTimeFormat::new(DatePattern::parse("%H:%M").unwrap())
                        .with_output_offset(UtcOffset::from_minutes(60).unwrap()),
                ),
            ),
            DataBinding::new(
                "Badge.Visible",
                path("badge"),
                BindingKind::Visibility(VisibilityRule::new(TruthyRule::NonEmpty)),
            ),
            DataBinding::new(
                "Leader.Visible",
                path("rank"),
                BindingKind::Visibility(VisibilityRule::new(TruthyRule::Compare {
                    op: CompareOp::Eq,
                    operand: decimal("1"),
                })),
            )
            .with_fallback(BoundValue::Visible(false)),
        ],
        CoercionPolicy::default(),
    )
    .unwrap()
}

#[test]
fn binding_set_maps_every_kind_with_statuses() {
    let row = json(
        r#"{"name":"Ada","score":12345.65,"goals":3,"color":"","logo":"https://img.example/ada.png",
            "kickoff":"2024-05-01T18:45:00Z","badge":"","rank":1}"#,
    );
    let report = scoreboard_bindings().map_row(&row);
    assert!(report.is_complete(), "{:?}", report.fields);
    let expected: BTreeMap<String, BoundValue> = [
        ("Name.Text", BoundValue::Text("Ada".into())),
        ("Score.Text", BoundValue::Text("12,345.7".into())),
        ("Goals", BoundValue::Number(3)),
        (
            "Accent",
            BoundValue::Color(TitleColor {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }),
        ),
        (
            "Logo.Source",
            BoundValue::Image("https://img.example/ada.png".into()),
        ),
        ("Kickoff.Text", BoundValue::Text("19:45".into())),
        ("Badge.Visible", BoundValue::Visible(false)),
        ("Leader.Visible", BoundValue::Visible(true)),
    ]
    .into_iter()
    .map(|(field, value)| (field.to_owned(), value))
    .collect();
    assert_eq!(report.values, expected);
    let status = |field: &str| {
        report
            .fields
            .iter()
            .find(|entry| entry.field == field)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(status("Accent"), BindingStatus::Fallback);
    assert_eq!(status("Badge.Visible"), BindingStatus::Mapped);

    let sparse = scoreboard_bindings().map_row(&json(
        r#"{"name":"Bo","score":"n/a","goals":1,"logo":"ftp://x/y","kickoff":0}"#,
    ));
    assert!(!sparse.is_complete());
    let errors: Vec<_> = sparse.errors().map(|entry| entry.field.as_str()).collect();
    assert_eq!(errors, ["Score.Text", "Logo.Source"]);
    assert_eq!(
        sparse.values["Kickoff.Text"],
        BoundValue::Text("01:00".into())
    );
    assert_eq!(sparse.values["Badge.Visible"], BoundValue::Visible(false));
    assert_eq!(sparse.values["Leader.Visible"], BoundValue::Visible(false));
    assert!(matches!(
        &sparse.fields[1].status,
        BindingStatus::Failed(BindingError::Format(FormatError::NotANumber(_)))
    ));
}

#[test]
fn binding_set_validates_configuration() {
    let text = |field: &str| DataBinding::new(field, path("a"), BindingKind::Text);
    assert_eq!(
        BindingSet::new(vec![text("A"), text("A")], CoercionPolicy::default()),
        Err(BindingConfigError::DuplicateField("A".into()))
    );
    assert!(matches!(
        BindingSet::new(
            vec![DataBinding::new(
                "N",
                path("a"),
                BindingKind::FormattedNumber(NumberFormat::fixed(30))
            )],
            CoercionPolicy::default()
        ),
        Err(BindingConfigError::Format { .. })
    ));
    assert_eq!(
        BindingSet::new(
            vec![text("A").with_fallback(BoundValue::Visible(true))],
            CoercionPolicy::default()
        ),
        Err(BindingConfigError::FallbackKindMismatch {
            field: "A".into(),
            expected: BoundValueKind::Text,
            actual: BoundValueKind::Visibility,
        })
    );
    assert!(matches!(
        BindingSet::new(
            vec![
                DataBinding::new(
                    "L",
                    path("a"),
                    BindingKind::Image(ImageUriPolicy::default())
                )
                .with_fallback(BoundValue::Image("relative.png".into()))
            ],
            CoercionPolicy::default()
        ),
        Err(BindingConfigError::InvalidFallback {
            error: BindingError::Image(ImageUriError::MissingScheme),
            ..
        })
    ));
}

#[test]
fn list_row_binding_follows_the_cursor() {
    let document = json(r#"{"rows":[{"name":"Ada"},{"name":"Bo"},{"name":"Cy"}]}"#);
    let rows = rows_at(&document, &path("rows")).unwrap();
    let set = BindingSet::new(
        vec![DataBinding::new("Name", path("name"), BindingKind::Text)],
        CoercionPolicy::default(),
    )
    .unwrap();
    let mut cursor = RowCursor::new(rows.len(), EdgeBehavior::Wrap);
    assert_eq!(
        set.map_selected(rows, &cursor),
        Err(RowBindingError::NoSelection)
    );
    cursor.navigate(RowNavigation::Last).unwrap();
    cursor.navigate(RowNavigation::Next).unwrap();
    assert_eq!(
        set.map_selected(rows, &cursor).unwrap().values["Name"],
        BoundValue::Text("Ada".into())
    );
    assert!(matches!(
        set.map_selected(&rows[..2], &cursor),
        Err(RowBindingError::Selection(_))
    ));
}

// ---------------------------------------------------------------- preview

fn name_score_set() -> BindingSet {
    BindingSet::new(
        vec![
            DataBinding::new("Name", path("name"), BindingKind::Text),
            DataBinding::new(
                "Score",
                path("score"),
                BindingKind::FormattedNumber(NumberFormat::fixed(0)),
            ),
            DataBinding::new(
                "Flag",
                path("flag"),
                BindingKind::Visibility(VisibilityRule::new(TruthyRule::Explicit)),
            )
            .optional(),
        ],
        CoercionPolicy::default(),
    )
    .unwrap()
}

#[test]
fn preview_stages_diffs_and_commits_atomically() {
    let set = name_score_set();
    let mut state = PreviewState::new(StagePolicy::RequireComplete);
    assert_eq!(state.revision(), 0);
    assert_eq!(state.commit(), Err(PreviewError::NothingStaged));

    let staged = state
        .stage(set.map_row(&json(r#"{"name":"Ada","score":1,"flag":true}"#)))
        .unwrap();
    assert_eq!(staged.base_revision, 0);
    assert_eq!(staged.changes.len(), 3);
    assert!(state.live().is_empty(), "staging never touches live");

    let outcome = state.commit().unwrap();
    assert_eq!(outcome.revision, 1);
    assert_eq!(state.revision(), 1);
    assert!(state.staged().is_none());
    assert_eq!(state.live()["Score"], BoundValue::Text("1".into()));

    let staged = state
        .stage(set.map_row(&json(r#"{"name":"Ada","score":2}"#)))
        .unwrap();
    assert_eq!(
        staged.changes,
        vec![
            FieldChange::Removed {
                field: "Flag".into(),
                previous: BoundValue::Visible(true)
            },
            FieldChange::Changed {
                field: "Score".into(),
                previous: BoundValue::Text("1".into()),
                value: BoundValue::Text("2".into()),
            },
        ]
    );
    assert_eq!(staged.changes[1].field(), "Score");
    assert_eq!(state.commit().unwrap().revision, 2);

    // Identical results commit as a no-op without bumping the revision.
    state
        .stage(set.map_row(&json(r#"{"name":"Ada","score":2}"#)))
        .unwrap();
    let outcome = state.commit().unwrap();
    assert_eq!((outcome.revision, outcome.changes.len()), (2, 0));

    // An incomplete report is refused and does not replace the existing stage.
    state
        .stage(set.map_row(&json(r#"{"name":"Bo","score":3}"#)))
        .unwrap();
    let error = state
        .stage(set.map_row(&json(r#"{"score":"x"}"#)))
        .unwrap_err();
    assert_eq!(
        error,
        PreviewError::Incomplete {
            errors: vec!["Name".into(), "Score".into()]
        }
    );
    assert_eq!(
        state.staged().unwrap().next["Name"],
        BoundValue::Text("Bo".into())
    );
    assert!(state.discard().is_some());
    assert_eq!(state.live()["Name"], BoundValue::Text("Ada".into()));
}

#[test]
fn preview_detects_stale_stage_and_keeps_live_on_error() {
    let set = name_score_set();
    let mut state = PreviewState::new(StagePolicy::KeepLiveOnError);
    state
        .apply_direct(&set.map_row(&json(r#"{"name":"Ada","score":5,"flag":"on"}"#)))
        .unwrap();
    assert_eq!(state.revision(), 1);

    state
        .stage(set.map_row(&json(r#"{"name":"Bo","score":6}"#)))
        .unwrap();
    state
        .apply_direct(&set.map_row(&json(r#"{"name":"Ada","score":7,"flag":"on"}"#)))
        .unwrap();
    assert!(state.is_stale());
    assert_eq!(
        state.commit(),
        Err(PreviewError::Stale { staged: 1, live: 2 })
    );
    assert!(
        state.staged().is_some(),
        "stale stage is kept for inspection"
    );
    assert_eq!(state.live()["Score"], BoundValue::Text("7".into()));

    // Errored fields keep their live values; valid fields still update.
    let staged = state
        .stage(set.map_row(&json(r#"{"name":"Cy","score":"bad","flag":"maybe"}"#)))
        .unwrap();
    assert_eq!(
        staged.changes,
        vec![FieldChange::Changed {
            field: "Name".into(),
            previous: BoundValue::Text("Ada".into()),
            value: BoundValue::Text("Cy".into()),
        }]
    );
    assert_eq!(staged.report.errors().count(), 2);
    let outcome = state.commit().unwrap();
    assert_eq!(outcome.revision, 3);
    assert_eq!(state.live()["Score"], BoundValue::Text("7".into()));
    assert_eq!(state.live()["Flag"], BoundValue::Visible(true));
    assert_eq!(state.live()["Name"], BoundValue::Text("Cy".into()));

    // An empty report under KeepLiveOnError removes live fields (all Omitted is not an error).
    let empty = BindingReport::default();
    let outcome = state.apply_direct(&empty).unwrap();
    assert_eq!(outcome.changes.len(), 3);
    assert!(state.live().is_empty());
}
