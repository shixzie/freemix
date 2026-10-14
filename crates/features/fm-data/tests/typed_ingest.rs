//! Bounded CSV/JSON ingestion, row navigation, and typed title field mapping.

use std::str::FromStr;

use fm_data::{
    CoercionError, CoercionPolicy, CsvOptions, DataAdapter, DataPath, DataValue, Decimal,
    EdgeBehavior, NumberRounding, ParseErrorKind, ParseLimits, RowCursor, RowNavigation,
    RowSelectionError, TitleColor, TitleFieldBinding, TitleFieldKind, TitleFieldStatus,
    TitleFieldValue, TitleMapper, TitleMapperError, Transform, ValueType, coerce_title_value,
    parse_csv, parse_json, rows_at,
};

fn path(text: &str) -> DataPath {
    DataPath::from_str(text).unwrap()
}

fn string(text: &str) -> DataValue {
    DataValue::from(text)
}

fn decimal(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap()
}

// ---------------------------------------------------------------- CSV

#[test]
fn csv_parses_quotes_crlf_bom_and_blank_lines() {
    let input = "\u{feff}name,team,score\r\n\"Lovelace, Ada\",\"The \"\"Engines\"\"\",42\r\n\r\nBabbage,\"multi\nline\",7\n";
    let table = parse_csv(input.as_bytes(), &CsvOptions::default()).unwrap();
    assert_eq!(table.headers, ["name", "team", "score"]);
    assert_eq!(
        table.rows,
        vec![
            vec!["Lovelace, Ada", "The \"Engines\"", "42"],
            vec!["Babbage", "multi\nline", "7"],
        ]
    );
    let rows = table.representation().unwrap();
    assert_eq!(
        path("[1].team").extract(&rows).unwrap(),
        &string("multi\nline")
    );
}

#[test]
fn csv_handles_empty_fields_custom_delimiter_and_no_header() {
    let options = CsvOptions {
        delimiter: b';',
        has_header: false,
        ..CsvOptions::default()
    };
    let table = parse_csv(b"a;;\n\"\";b;c", &options).unwrap();
    assert_eq!(table.headers, ["column_1", "column_2", "column_3"]);
    assert_eq!(table.rows, vec![vec!["a", "", ""], vec!["", "b", "c"]]);
    // Header-only input yields zero rows; empty input yields nothing.
    let header_only = parse_csv(b"x,y\n", &CsvOptions::default()).unwrap();
    assert_eq!(header_only.headers, ["x", "y"]);
    assert_eq!(header_only.rows, Vec::<Vec<String>>::new());
    let empty = parse_csv(b"", &CsvOptions::default()).unwrap();
    assert!(empty.headers.is_empty() && empty.rows.is_empty());
}

#[test]
fn csv_reports_located_structural_errors() {
    let defaults = CsvOptions::default();
    let error = parse_csv(b"a,b\n1,2\n3\n", &defaults).unwrap_err();
    assert_eq!(
        error.kind,
        ParseErrorKind::RowWidth {
            row: 1,
            expected: 2,
            actual: 1
        }
    );
    assert_eq!((error.line, error.column, error.offset), (3, 1, 8));

    let error = parse_csv(b"a,a\n", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::DuplicateHeader("a".into()));
    let error = parse_csv(b"a,,c\n", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::EmptyHeader { column: 1 });

    let error = parse_csv(b"a\nx\"y\n", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::UnexpectedQuote);
    assert_eq!((error.line, error.column), (2, 2));

    let error = parse_csv(b"a\n\"ok\"tail\n", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::TextAfterQuote);

    let error = parse_csv(b"a\n\"open\nnever closed", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::UnterminatedString);
    assert_eq!(error.line, 2);

    let error = parse_csv(b"a\n\xff\n", &defaults).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::InvalidUtf8);
    assert_eq!((error.offset, error.line), (2, 2));

    let bad = CsvOptions {
        delimiter: b'"',
        ..defaults
    };
    assert_eq!(
        parse_csv(b"a", &bad).unwrap_err().kind,
        ParseErrorKind::InvalidDelimiter(b'"')
    );
}

#[test]
fn csv_enforces_every_limit() {
    let limits = ParseLimits {
        max_input_bytes: 64,
        max_rows: 2,
        max_columns: 3,
        max_field_bytes: 5,
        ..ParseLimits::default()
    };
    let options = CsvOptions {
        limits,
        ..CsvOptions::default()
    };
    assert_eq!(
        parse_csv(&[b'a'; 65], &options).unwrap_err().kind,
        ParseErrorKind::InputTooLarge {
            limit: 64,
            actual: 65
        }
    );
    assert_eq!(
        parse_csv(b"h\n1\n2\n3\n", &options).unwrap_err().kind,
        ParseErrorKind::TooManyRows { limit: 2 }
    );
    assert!(parse_csv(b"h\n1\n2\n", &options).is_ok());
    assert_eq!(
        parse_csv(b"a,b,c,d\n", &options).unwrap_err().kind,
        ParseErrorKind::TooManyColumns { limit: 3 }
    );
    assert_eq!(
        parse_csv(b"h\n123456\n", &options).unwrap_err().kind,
        ParseErrorKind::FieldTooLarge { limit: 5 }
    );
    assert_eq!(
        parse_csv(b"h\n\"12\"\"345\"\n", &options).unwrap_err().kind,
        ParseErrorKind::FieldTooLarge { limit: 5 }
    );
    let no_header_zero_rows = CsvOptions {
        has_header: false,
        limits: ParseLimits {
            max_rows: 0,
            ..ParseLimits::default()
        },
        ..CsvOptions::default()
    };
    assert_eq!(
        parse_csv(b"x\n", &no_header_zero_rows).unwrap_err().kind,
        ParseErrorKind::TooManyRows { limit: 0 }
    );
}

// ---------------------------------------------------------------- JSON

#[test]
fn json_parses_exact_typed_values() {
    let input = br#" {
        "title": "Final \u00e9\ud83d\ude00\n",
        "live": true, "note": null,
        "score": -42, "ratio": 0.250, "big": 1.5e3, "tiny": 25e-3, "zero": -0.0e10,
        "players": [ {"name": "Ada"}, {"name": "Grace"} ], "empty": {}, "none": []
    } "#;
    let value = parse_json(input, &ParseLimits::default()).unwrap();
    let get = |text: &str| path(text).extract(&value).unwrap().clone();
    assert_eq!(get("title"), string("Final \u{e9}\u{1f600}\n"));
    assert_eq!(get("live"), DataValue::Bool(true));
    assert_eq!(get("note"), DataValue::Null);
    assert_eq!(get("score"), DataValue::Integer(-42));
    assert_eq!(get("ratio"), DataValue::Decimal(decimal("0.25")));
    assert_eq!(get("big"), DataValue::Decimal(decimal("1500")));
    assert_eq!(get("tiny"), DataValue::Decimal(decimal("0.025")));
    assert_eq!(get("zero"), DataValue::Decimal(decimal("0")));
    assert_eq!(get("players[1].name"), string("Grace"));
    assert_eq!(get("empty").value_type(), ValueType::Object);
    assert_eq!(get("none"), DataValue::List(Vec::new()));
    assert_eq!(
        parse_json(b"-9223372036854775808", &ParseLimits::default()).unwrap(),
        DataValue::Integer(i64::MIN)
    );
}

#[test]
fn json_rejects_malformed_and_unrepresentable_input() {
    let limits = ParseLimits::default();
    let kind = |input: &[u8]| parse_json(input, &limits).unwrap_err().kind;
    assert_eq!(kind(b""), ParseErrorKind::UnexpectedEnd);
    assert_eq!(kind(b"[1,]"), ParseErrorKind::UnexpectedCharacter(']'));
    assert_eq!(
        kind(b"{\"a\":1,}"),
        ParseErrorKind::UnexpectedCharacter('}')
    );
    assert_eq!(kind(b"{\"a\" 1}"), ParseErrorKind::UnexpectedCharacter('1'));
    assert_eq!(kind(b"tru"), ParseErrorKind::UnexpectedCharacter('t'));
    assert_eq!(kind(b"1 2"), ParseErrorKind::TrailingData);
    assert_eq!(kind(b"01"), ParseErrorKind::InvalidNumber);
    assert_eq!(kind(b"1."), ParseErrorKind::InvalidNumber);
    assert_eq!(kind(b"1e"), ParseErrorKind::InvalidNumber);
    assert_eq!(kind(b"-"), ParseErrorKind::InvalidNumber);
    assert_eq!(
        kind(b"9223372036854775808"),
        ParseErrorKind::NumberOutOfRange
    );
    assert_eq!(kind(b"1e19"), ParseErrorKind::NumberOutOfRange);
    assert_eq!(kind(b"1e-19"), ParseErrorKind::NumberOutOfRange);
    assert_eq!(kind(b"1e99999"), ParseErrorKind::NumberOutOfRange);
    assert_eq!(kind(b"\"\\x\""), ParseErrorKind::InvalidEscape);
    assert_eq!(kind(b"\"\\ud83d\""), ParseErrorKind::InvalidEscape);
    assert_eq!(kind(b"\"\\u12g4\""), ParseErrorKind::InvalidEscape);
    assert_eq!(kind(b"\"a\nb\""), ParseErrorKind::ControlCharacterInString);
    assert_eq!(kind(b"\"abc"), ParseErrorKind::UnterminatedString);

    let error = parse_json(b"{\n  \"a\": 1,\n  \"a\": 2\n}", &limits).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::DuplicateKey("a".into()));
    assert_eq!((error.line, error.column), (3, 3));
}

#[test]
fn json_enforces_depth_node_and_string_limits() {
    let limits = ParseLimits {
        max_depth: 3,
        max_nodes: 5,
        max_field_bytes: 4,
        ..ParseLimits::default()
    };
    assert!(parse_json(b"[[[]]]", &limits).is_ok());
    assert_eq!(
        parse_json(b"[[[[]]]]", &limits).unwrap_err().kind,
        ParseErrorKind::DepthExceeded { limit: 3 }
    );
    assert!(parse_json(b"[1,2,3,4]", &limits).is_ok());
    assert_eq!(
        parse_json(b"[1,2,3,4,5]", &limits).unwrap_err().kind,
        ParseErrorKind::TooManyNodes { limit: 5 }
    );
    assert!(parse_json(b"\"abcd\"", &limits).is_ok());
    assert_eq!(
        parse_json(b"\"abcde\"", &limits).unwrap_err().kind,
        ParseErrorKind::FieldTooLarge { limit: 4 }
    );
    assert_eq!(
        parse_json(b"{\"long-key\":1}", &limits).unwrap_err().kind,
        ParseErrorKind::FieldTooLarge { limit: 4 }
    );
    let small = ParseLimits {
        max_input_bytes: 3,
        ..ParseLimits::default()
    };
    assert_eq!(
        parse_json(b"1234", &small).unwrap_err().kind,
        ParseErrorKind::InputTooLarge {
            limit: 3,
            actual: 4
        }
    );
}

#[test]
fn parsing_is_deterministic() {
    let input = br#"{"b":[1,2.5,"x"],"a":{"z":null,"y":false}}"#;
    let first = parse_json(input, &ParseLimits::default()).unwrap();
    let second = parse_json(input, &ParseLimits::default()).unwrap();
    assert_eq!(first, second);
    let csv = b"k,v\n1,2\n";
    assert_eq!(
        parse_csv(csv, &CsvOptions::default()).unwrap(),
        parse_csv(csv, &CsvOptions::default()).unwrap()
    );
}

// ---------------------------------------------------------------- Rows

#[test]
fn cursor_navigates_with_stop_and_wrap_edges() {
    let mut stop = RowCursor::new(3, EdgeBehavior::Stop);
    assert_eq!(stop.selected(), None);
    assert_eq!(stop.navigate(RowNavigation::Next).unwrap(), 0);
    assert_eq!(stop.navigate(RowNavigation::Previous).unwrap(), 0);
    assert_eq!(stop.navigate(RowNavigation::Last).unwrap(), 2);
    assert_eq!(stop.navigate(RowNavigation::Next).unwrap(), 2);
    assert_eq!(stop.navigate(RowNavigation::Previous).unwrap(), 1);
    assert_eq!(
        stop.navigate(RowNavigation::Index(3)).unwrap_err(),
        RowSelectionError::IndexOutOfRange { index: 3, len: 3 }
    );
    assert_eq!(stop.selected(), Some(1));

    let mut wrap = RowCursor::new(3, EdgeBehavior::Wrap);
    assert_eq!(wrap.navigate(RowNavigation::Previous).unwrap(), 2);
    assert_eq!(wrap.navigate(RowNavigation::Next).unwrap(), 0);
    assert_eq!(wrap.navigate(RowNavigation::Previous).unwrap(), 2);
    assert_eq!(wrap.navigate(RowNavigation::Index(1)).unwrap(), 1);

    let mut empty = RowCursor::new(0, EdgeBehavior::Wrap);
    assert_eq!(
        empty.navigate(RowNavigation::First).unwrap_err(),
        RowSelectionError::Empty
    );
}

#[test]
fn cursor_survives_refresh_and_selects_by_key() {
    let doc = parse_json(
        br#"{"rows":[{"id":"a","n":1},{"id":"b","n":2},{"n":3},{"id":"d","n":4}]}"#,
        &ParseLimits::default(),
    )
    .unwrap();
    let rows = rows_at(&doc, &path("rows")).unwrap();
    let mut cursor = RowCursor::new(rows.len(), EdgeBehavior::Stop);
    assert_eq!(cursor.select_where(rows, &path("id"), &string("d")), Ok(3));
    assert_eq!(
        cursor
            .current(rows)
            .and_then(|row| path("n").extract(row).ok()),
        Some(&DataValue::Integer(4))
    );
    // Exact matching: Integer 2 is not the string "2".
    assert_eq!(
        cursor.select_where(rows, &path("n"), &string("2")),
        Err(RowSelectionError::NoMatch)
    );
    assert_eq!(cursor.selected(), Some(3));
    assert_eq!(
        cursor.select_where(&rows[..2], &path("id"), &string("a")),
        Err(RowSelectionError::LengthMismatch { cursor: 4, rows: 2 })
    );

    cursor.set_len(2);
    assert_eq!(cursor.selected(), Some(1));
    cursor.set_len(5);
    assert_eq!(cursor.selected(), Some(1));
    cursor.set_len(0);
    assert_eq!(cursor.selected(), None);

    assert_eq!(
        rows_at(&doc, &path("rows[0]")).unwrap_err(),
        RowSelectionError::ExpectedList {
            actual: ValueType::Object
        }
    );
    assert!(matches!(
        rows_at(&doc, &path("missing")),
        Err(RowSelectionError::Path(_))
    ));
}

// ---------------------------------------------------------------- Coercion

#[test]
fn coercion_rules_are_explicit() {
    let policy = CoercionPolicy::default();
    let coerce = |value: DataValue, kind| coerce_title_value(&value, kind, &policy);
    assert_eq!(
        coerce(DataValue::Integer(7), TitleFieldKind::Text),
        Ok(TitleFieldValue::Text("7".into()))
    );
    assert_eq!(
        coerce(DataValue::Decimal(decimal("-1.50")), TitleFieldKind::Text),
        Ok(TitleFieldValue::Text("-1.5".into()))
    );
    assert_eq!(
        coerce(DataValue::Bool(false), TitleFieldKind::Text),
        Ok(TitleFieldValue::Text("false".into()))
    );
    assert_eq!(
        coerce(string(" +12 "), TitleFieldKind::Number),
        Ok(TitleFieldValue::Number(12))
    );
    assert_eq!(
        coerce(string("3.0"), TitleFieldKind::Number),
        Ok(TitleFieldValue::Number(3))
    );
    assert_eq!(
        coerce(string("+-3"), TitleFieldKind::Number),
        Err(CoercionError::InvalidNumber("+-3".into()))
    );
    assert_eq!(
        coerce(string("12abc"), TitleFieldKind::Number),
        Err(CoercionError::InvalidNumber("12abc".into()))
    );
    assert_eq!(
        coerce(DataValue::Decimal(decimal("2.5")), TitleFieldKind::Number),
        Err(CoercionError::FractionalNumber(decimal("2.5")))
    );
    assert_eq!(
        coerce(DataValue::Bool(true), TitleFieldKind::Number),
        Err(CoercionError::Unsupported {
            kind: TitleFieldKind::Number,
            actual: ValueType::Bool
        })
    );
    assert_eq!(
        coerce(DataValue::List(Vec::new()), TitleFieldKind::Text),
        Err(CoercionError::Unsupported {
            kind: TitleFieldKind::Text,
            actual: ValueType::List
        })
    );
}

#[test]
fn color_and_image_coercion_rules() {
    let policy = CoercionPolicy::default();
    let coerce = |value: DataValue, kind| coerce_title_value(&value, kind, &policy);
    assert_eq!(
        coerce(string("#f80"), TitleFieldKind::Color),
        Ok(TitleFieldValue::Color(TitleColor {
            r: 0xff,
            g: 0x88,
            b: 0x00,
            a: 0xff
        }))
    );
    assert_eq!(
        coerce(string("#102030"), TitleFieldKind::Color),
        Ok(TitleFieldValue::Color(TitleColor {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 0xff
        }))
    );
    assert_eq!(
        coerce(string("#10203040"), TitleFieldKind::Color),
        Ok(TitleFieldValue::Color(TitleColor {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 0x40
        }))
    );
    for bad in ["102030", "#12", "#1234", "#gg0000", "#+12345"] {
        assert_eq!(
            coerce(string(bad), TitleFieldKind::Color),
            Err(CoercionError::InvalidColor(bad.into()))
        );
    }
    assert_eq!(
        coerce(string(" logo.png "), TitleFieldKind::Image),
        Ok(TitleFieldValue::Image("logo.png".into()))
    );
    assert_eq!(
        coerce(string("a\u{7}b"), TitleFieldKind::Image),
        Err(CoercionError::ControlCharacter)
    );
    assert_eq!(
        coerce(string("   "), TitleFieldKind::Image),
        Err(CoercionError::EmptyImageReference)
    );
    let tight = CoercionPolicy {
        max_text_bytes: 3,
        ..policy
    };
    assert_eq!(
        coerce_title_value(&string("abcd"), TitleFieldKind::Text, &tight),
        Err(CoercionError::TextTooLong {
            limit: 3,
            actual: 4
        })
    );
}

#[test]
fn number_rounding_modes() {
    let round = |text: &str, rounding| {
        coerce_title_value(
            &DataValue::Decimal(decimal(text)),
            TitleFieldKind::Number,
            &CoercionPolicy {
                rounding,
                ..CoercionPolicy::default()
            },
        )
    };
    let half = NumberRounding::HalfAwayFromZero;
    let trunc = NumberRounding::Truncate;
    assert_eq!(round("2.5", half), Ok(TitleFieldValue::Number(3)));
    assert_eq!(round("-2.5", half), Ok(TitleFieldValue::Number(-3)));
    assert_eq!(round("2.49", half), Ok(TitleFieldValue::Number(2)));
    assert_eq!(round("-2.51", half), Ok(TitleFieldValue::Number(-3)));
    assert_eq!(round("2.99", trunc), Ok(TitleFieldValue::Number(2)));
    assert_eq!(round("-2.99", trunc), Ok(TitleFieldValue::Number(-2)));
    assert_eq!(
        round("0.000000000000000001", half),
        Ok(TitleFieldValue::Number(0))
    );
}

// ---------------------------------------------------------------- Mapping

fn scoreboard_mapper() -> TitleMapper {
    TitleMapper::new(
        vec![
            TitleFieldBinding::new("home", path("home"), TitleFieldKind::Text),
            TitleFieldBinding::new("home_score", path("home_score"), TitleFieldKind::Number),
            TitleFieldBinding::new("accent", path("color"), TitleFieldKind::Color).with_fallback(
                TitleFieldValue::Color(TitleColor {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
            ),
            TitleFieldBinding::new("logo", path("logo"), TitleFieldKind::Image).optional(),
            TitleFieldBinding::new("label", path("home"), TitleFieldKind::Text)
                .with_transform(Transform::Format("HOME: {value}".into())),
        ],
        CoercionPolicy::default(),
    )
    .unwrap()
}

#[test]
fn csv_rows_map_onto_title_fields_with_typed_diagnostics() {
    let csv = b"home,home_score,color,logo\nLions,3,#ff0000,lions.png\nBears,,,\nHawks,2.5,red,\n";
    let rows = parse_csv(csv, &CsvOptions::default())
        .unwrap()
        .representation()
        .unwrap();
    let DataValue::List(rows) = rows else {
        panic!("CSV representation must be a list");
    };
    let mapper = scoreboard_mapper();
    let mut cursor = RowCursor::new(rows.len(), EdgeBehavior::Wrap);

    cursor.navigate(RowNavigation::First).unwrap();
    let first = mapper.map_row(cursor.current(&rows).unwrap());
    assert!(first.is_complete());
    assert_eq!(
        first.values.get("home_score"),
        Some(&TitleFieldValue::Number(3))
    );
    assert_eq!(
        first.values.get("logo"),
        Some(&TitleFieldValue::Image("lions.png".into()))
    );
    assert_eq!(
        first.values.get("label"),
        Some(&TitleFieldValue::Text("HOME: Lions".into()))
    );

    cursor.navigate(RowNavigation::Next).unwrap();
    let second = mapper.map_row(cursor.current(&rows).unwrap());
    let statuses: Vec<_> = second
        .fields
        .iter()
        .map(|field| (field.field.as_str(), field.status.clone()))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("home", TitleFieldStatus::Mapped),
            ("home_score", TitleFieldStatus::Missing),
            ("accent", TitleFieldStatus::Fallback),
            ("logo", TitleFieldStatus::Omitted),
            ("label", TitleFieldStatus::Mapped),
        ]
    );
    assert!(!second.is_complete());
    assert_eq!(second.errors().count(), 1);
    assert!(!second.values.contains_key("logo"));

    cursor.navigate(RowNavigation::Next).unwrap();
    let third = mapper.map_row(cursor.current(&rows).unwrap());
    let errors: Vec<_> = third.errors().map(|field| field.field.as_str()).collect();
    assert_eq!(errors, ["home_score", "accent"]);
    assert_eq!(
        third.fields[1].status,
        TitleFieldStatus::Coercion(CoercionError::FractionalNumber(decimal("2.5")))
    );

    // Wrap back to the first row deterministically.
    assert_eq!(cursor.navigate(RowNavigation::Next).unwrap(), 0);
    assert_eq!(mapper.map_row(cursor.current(&rows).unwrap()), first);
}

#[test]
fn json_rows_map_with_path_and_transform_errors() {
    let doc = parse_json(
        br#"{"games":[{"home":"Owls","home_score":4,"color":null},{"home":{"x":1},"home_score":"7"}]}"#,
        &ParseLimits::default(),
    )
    .unwrap();
    let rows = rows_at(&doc, &path("games")).unwrap();
    let mapper = scoreboard_mapper();
    let report = mapper.map_row(&rows[0]);
    assert!(report.is_complete());
    assert_eq!(
        report.fields[2].status,
        TitleFieldStatus::Fallback,
        "null is treated as missing"
    );
    let report = mapper.map_row(&rows[1]);
    assert_eq!(
        report.values.get("home_score"),
        Some(&TitleFieldValue::Number(7))
    );
    assert!(matches!(
        report.fields[0].status,
        TitleFieldStatus::Coercion(CoercionError::Unsupported {
            actual: ValueType::Object,
            ..
        })
    ));
    assert!(matches!(
        report.fields[4].status,
        TitleFieldStatus::Transform(_)
    ));

    let nested = TitleMapper::new(
        vec![TitleFieldBinding::new(
            "x",
            path("home.x"),
            TitleFieldKind::Text,
        )],
        CoercionPolicy::default(),
    )
    .unwrap();
    assert!(matches!(
        nested.map_row(&rows[0]).fields[0].status,
        TitleFieldStatus::Path(_)
    ));
}

#[test]
fn mapper_configuration_is_validated_and_bounded() {
    let policy = CoercionPolicy::default();
    let text = |name: &str| TitleFieldBinding::new(name, path("a"), TitleFieldKind::Text);
    assert_eq!(
        TitleMapper::new(vec![text("a"), text("a")], policy).unwrap_err(),
        TitleMapperError::DuplicateField("a".into())
    );
    assert_eq!(
        TitleMapper::new(vec![text("")], policy).unwrap_err(),
        TitleMapperError::EmptyFieldName
    );
    assert_eq!(
        TitleMapper::new(
            vec![text("a").with_fallback(TitleFieldValue::Number(1))],
            policy
        )
        .unwrap_err(),
        TitleMapperError::FallbackKindMismatch {
            field: "a".into(),
            expected: TitleFieldKind::Text,
            actual: TitleFieldKind::Number
        }
    );
    assert!(matches!(
        TitleMapper::new(
            vec![
                TitleFieldBinding::new("i", path("a"), TitleFieldKind::Image)
                    .with_fallback(TitleFieldValue::Image(String::new()))
            ],
            policy
        ),
        Err(TitleMapperError::InvalidFallback { .. })
    ));
    let too_many: Vec<_> = (0..=TitleMapper::MAX_BINDINGS)
        .map(|index| text(&format!("f{index}")))
        .collect();
    assert_eq!(
        TitleMapper::new(too_many, policy).unwrap_err(),
        TitleMapperError::TooManyBindings {
            limit: TitleMapper::MAX_BINDINGS,
            actual: TitleMapper::MAX_BINDINGS + 1
        }
    );
    let mut busy = text("a");
    busy.transforms = vec![Transform::Fallback(DataValue::Null); 17];
    assert!(matches!(
        TitleMapper::new(vec![busy], policy),
        Err(TitleMapperError::TooManyTransforms { .. })
    ));
}

#[test]
fn empty_strings_follow_policy() {
    let bindings = vec![
        TitleFieldBinding::new("n", path("n"), TitleFieldKind::Number).optional(),
        TitleFieldBinding::new("t", path("t"), TitleFieldKind::Text),
    ];
    let row = parse_json(br#"{"n":"  ","t":""}"#, &ParseLimits::default()).unwrap();
    let lenient = TitleMapper::new(bindings.clone(), CoercionPolicy::default()).unwrap();
    let report = lenient.map_row(&row);
    assert_eq!(report.fields[0].status, TitleFieldStatus::Omitted);
    assert_eq!(
        report.values.get("t"),
        Some(&TitleFieldValue::Text(String::new()))
    );
    let strict = TitleMapper::new(
        bindings,
        CoercionPolicy {
            empty_string_is_missing: false,
            ..CoercionPolicy::default()
        },
    )
    .unwrap();
    assert_eq!(
        strict.map_row(&row).fields[0].status,
        TitleFieldStatus::Coercion(CoercionError::InvalidNumber("  ".into()))
    );
}
