use super::*;

const ES: Locale = Locale::new(Language::Spanish);

#[test]
fn every_catalog_has_every_key_with_englishs_arguments() {
    let issues = validate_catalogs();
    assert!(
        issues.is_empty(),
        "catalog problems (fix the catalog, not this test):\n{issues:#?}"
    );
}

#[test]
fn validation_reports_missing_duplicate_and_mismatched_entries() {
    // The validator itself must catch each defect class, or the test above
    // proves nothing.
    assert_eq!(
        placeholders("{a} and {b} and {a}").unwrap(),
        ["a".to_owned(), "b".to_owned()]
    );
    assert_eq!(placeholders("{{literal}}").unwrap(), Vec::<String>::new());
    assert_eq!(placeholders("{open"), Err(TemplateError::Unclosed));
    assert_eq!(placeholders("close}"), Err(TemplateError::StrayClose));
    assert_eq!(
        placeholders("{Bad Name}"),
        Err(TemplateError::BadName("Bad Name".to_owned()))
    );
    let broken = [
        (Msg::MenuFile, t("Archivo")),
        (Msg::MenuFile, t("Fichero")),
        (Msg::Unavailable, t("No disponible: {motivo}")),
        (Msg::HistorySteps, t("{count} pasos")),
        (
            Msg::QueuedCommands,
            plural("un comando", "{count} comandos"),
        ),
        (Msg::MenuEdit, t("Editar {")),
    ];
    let issues = validate_table(Language::Spanish, &broken);
    let language = Language::Spanish;
    assert!(issues.contains(&CatalogIssue::Duplicate {
        language,
        key: "MenuFile"
    }));
    assert!(issues.contains(&CatalogIssue::Missing {
        language,
        key: "MenuView"
    }));
    assert!(issues.contains(&CatalogIssue::ArgumentMismatch {
        language,
        key: "Unavailable",
        expected: vec!["reason".to_owned()],
        found: vec!["motivo".to_owned()],
    }));
    assert!(issues.contains(&CatalogIssue::PluralShape {
        language,
        key: "HistorySteps"
    }));
    assert!(issues.contains(&CatalogIssue::BadTemplate {
        language,
        key: "MenuEdit",
        error: TemplateError::Unclosed,
    }));
    // A `one` form may spell its count out; nothing else is reported for it.
    assert!(!issues.iter().any(|issue| matches!(
        issue,
        CatalogIssue::ArgumentMismatch {
            key: "QueuedCommands",
            ..
        }
    )));
    // A partial table indexes what it has; lookups fall back to English.
    let index = index(&broken);
    assert_eq!(index[Msg::MenuFile as usize], Some(t("Archivo")));
    assert_eq!(index[Msg::MenuView as usize], None);
}

#[test]
fn spanish_lookups_and_english_fallback() {
    assert_eq!(Locale::ENGLISH.text(Msg::MenuFile), "File");
    assert_eq!(ES.text(Msg::MenuFile), "Archivo");
    assert_eq!(ES.text(Msg::Undo), "Deshacer");
    // A key missing from a catalog falls back to English.
    let only_file = index(&[(Msg::MenuFile, t("Archivo"))]);
    let fallback = only_file[Msg::MenuEdit as usize].unwrap_or_else(|| Msg::MenuEdit.source());
    assert_eq!(fallback.other(), "Edit");
}

#[test]
fn named_arguments_fill_templates_in_either_order() {
    assert_eq!(
        Locale::ENGLISH.format(
            Msg::ToastAdded,
            &[
                ("position", Arg::from(3_usize)),
                ("input", Arg::Str("7")),
                ("name", Arg::Str("Bars")),
            ],
        ),
        "Added \"Bars\" as input 7 at position 3. Undo removes it again."
    );
    assert_eq!(
        ES.format(
            Msg::ToastAdding,
            &[("name", "Cam 1".into()), ("input", "9".into())]
        ),
        "Añadiendo «Cam 1» como entrada 9…"
    );
    // Plain numbers are never grouped; counts are.
    assert_eq!(
        Locale::ENGLISH.format(Msg::RevisionShort, &[("revision", Arg::Plain(12_345))]),
        "REV 12345"
    );
    assert_eq!(
        ES.format(Msg::PendingCount, &[("count", Arg::Count(12_345))]),
        "12.345 pendientes"
    );
}

#[test]
fn plural_rules_pick_one_only_for_exactly_one() {
    for language in Language::ALL {
        assert_eq!(language.plural(1), PluralCategory::One);
        for count in [0, 2, 5, 11, 21, 101, 1_000_000] {
            assert_eq!(language.plural(count), PluralCategory::Other);
        }
    }
    let steps = |locale: Locale, count: u64| {
        locale.format(Msg::HistorySteps, &[("count", Arg::Count(count))])
    };
    assert_eq!(steps(Locale::ENGLISH, 1), "1 step in the shared history");
    assert_eq!(steps(Locale::ENGLISH, 0), "0 steps in the shared history");
    assert_eq!(steps(ES, 1), "1 paso en el historial compartido");
    assert_eq!(steps(ES, 2), "2 pasos en el historial compartido");
}

#[test]
fn english_bridge_translates_catalog_text_and_passes_other_text_through() {
    let reason = Msg::ReasonNotReady.en();
    assert_eq!(Locale::ENGLISH.tr(reason), reason);
    assert_eq!(ES.tr(reason), "la conexión de control aún no está lista");
    assert_eq!(ES.tr("LOCKED"), "BLOQUEADO");
    assert_eq!(ES.tr("disk full"), "disk full");
    assert_eq!(ES.tr(""), "");
}

#[test]
fn language_tags_and_posix_locales_parse() {
    assert_eq!(Language::from_tag("es"), Some(Language::Spanish));
    assert_eq!(Language::from_tag("es-MX"), Some(Language::Spanish));
    assert_eq!(Language::from_tag("es_ES.UTF-8"), Some(Language::Spanish));
    assert_eq!(Language::from_tag("EN"), Some(Language::English));
    assert_eq!(Language::from_tag("en_US.UTF-8"), Some(Language::English));
    assert_eq!(Language::from_tag("C.UTF-8"), None);
    assert_eq!(Language::from_tag("POSIX"), None);
    assert_eq!(Language::from_tag("fr_FR"), None);
    assert_eq!(Language::from_tag(""), None);
    assert_eq!(HourCycle::from_flag("12h"), Some(HourCycle::H12));
    assert_eq!(HourCycle::from_flag("24"), Some(HourCycle::H23));
    assert_eq!(HourCycle::from_flag("25h"), None);
}

#[test]
fn shipped_languages_read_left_to_right() {
    for language in Language::ALL {
        assert_eq!(language.direction(), TextDirection::LeftToRight);
    }
    assert_eq!(Locale::default(), Locale::ENGLISH);
    assert_eq!(Language::Spanish.native_name(), "Español");
}

#[test]
fn integers_group_by_locale() {
    let en = Locale::ENGLISH;
    assert_eq!(en.integer(0), "0");
    assert_eq!(en.integer(999), "999");
    assert_eq!(en.integer(1_234), "1,234");
    assert_eq!(en.integer(1_234_567), "1,234,567");
    // Spanish groups only from five digits (CLDR minimum grouping 2).
    assert_eq!(ES.integer(1_234), "1234");
    assert_eq!(ES.integer(12_345), "12.345");
    assert_eq!(ES.integer(1_234_567), "1.234.567");
    assert_eq!(en.signed_integer(-1_234), "-1,234");
    assert_eq!(ES.signed_integer(i64::MIN), "-9.223.372.036.854.775.808");
}

#[test]
fn decimals_use_the_locale_mark_and_keep_signs_ascii() {
    let en = Locale::ENGLISH;
    assert_eq!(en.decimal(1_234.56, 1), "1,234.6");
    assert_eq!(ES.decimal(1_234.56, 1), "1234,6");
    assert_eq!(ES.decimal(12_345.5, 2), "12.345,50");
    assert_eq!(en.signed_decimal(0.0, 1), "+0.0");
    assert_eq!(ES.signed_decimal(-6.5, 1), "-6,5");
    // Rounding to zero never prints a negative zero.
    assert_eq!(en.signed_decimal(-0.04, 1), "+0.0");
    assert_eq!(en.decimal(f64::NEG_INFINITY, 1), "-inf");
    assert_eq!(en.decimal(f64::NAN, 1), "NaN");
    assert_eq!(en.decimal(3.0, 0), "3");
}

#[test]
fn percentages_levels_bytes_and_durations() {
    let en = Locale::ENGLISH;
    assert_eq!(en.percent(45), "45%");
    assert_eq!(ES.percent(45), "45\u{a0}%");
    assert_eq!(en.decibels(-6.02, -60.0), "-6.0 dB");
    assert_eq!(ES.decibels(12.0, -60.0), "+12,0 dB");
    assert_eq!(ES.decibels(-60.0, -60.0), "-inf dB");
    assert_eq!(en.decibels(f64::NAN, -60.0), "-inf dB");
    assert_eq!(en.bytes(0), "0 B");
    assert_eq!(en.bytes(1_023), "1023 B");
    assert_eq!(en.bytes(2_048), "2.0 KB");
    assert_eq!(en.bytes(1_048_575), "1.0 MB");
    assert_eq!(en.bytes(1_048_520), "1023.9 KB");
    assert_eq!(en.bytes(u64::MAX), "16777216.0 TB");
    assert_eq!(ES.bytes(3 * 1_024 * 1_024 / 2), "1,5 MB");
    assert_eq!(en.seconds_tenths(1_549), "1.5 s");
    assert_eq!(ES.seconds_tenths(12_380), "12,3 s");
}

#[test]
fn typed_decimals_parse_in_the_locale() {
    let en = Locale::ENGLISH;
    assert_eq!(en.parse_decimal(" -6.5 "), Some(-6.5));
    assert_eq!(en.parse_decimal("-6,5"), None);
    assert_eq!(ES.parse_decimal("-6,5"), Some(-6.5));
    assert_eq!(ES.parse_decimal("-6.5"), Some(-6.5));
    assert_eq!(ES.parse_decimal("1.234,5"), None);
    assert_eq!(ES.parse_decimal("-inf"), Some(f64::NEG_INFINITY));
    assert_eq!(en.parse_decimal("NaN"), None);
    assert_eq!(en.parse_decimal("loud"), None);
}

#[test]
fn times_follow_the_hour_cycle() {
    let english_12h = Locale::ENGLISH.with_hour_cycle(HourCycle::H12);
    let spanish_12h = ES.with_hour_cycle(HourCycle::H12);
    assert_eq!(Locale::ENGLISH.time_of_day(0), "00:00:00");
    assert_eq!(ES.time_of_day(86_399), "23:59:59");
    assert_eq!(Locale::ENGLISH.time_of_day(86_400 + 3_661), "01:01:01");
    assert_eq!(english_12h.time_of_day(0), "12:00:00\u{a0}AM");
    assert_eq!(english_12h.time_of_day(12 * 3_600), "12:00:00\u{a0}PM");
    assert_eq!(
        english_12h.time_of_day(13 * 3_600 + 5 * 60 + 9),
        "1:05:09\u{a0}PM"
    );
    assert_eq!(spanish_12h.time_of_day(9 * 3_600), "9:00:00\u{a0}a. m.");
    assert_eq!(spanish_12h.time_of_day(23 * 3_600), "11:00:00\u{a0}p. m.");
}

#[test]
fn dates_follow_the_language() {
    // 2026-10-14 is day 20_740 of the Unix epoch.
    assert_eq!(Locale::ENGLISH.date(20_740), "Oct 14, 2026");
    assert_eq!(ES.date(20_740), "14 oct 2026");
    assert_eq!(Locale::ENGLISH.date(0), "Jan 1, 1970");
    // Leap day and the day before the epoch.
    assert_eq!(Locale::ENGLISH.date(11_016), "Feb 29, 2000");
    assert_eq!(ES.date(-1), "31 dic 1969");
}
