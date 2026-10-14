use std::fmt::Write as _;

use fm_grade::{CubeError, CubeKind, CubeLut, CubeWarning, MAX_CUBE_INPUT_BYTES, parse_cube};

/// Resolve-style export: comment header, quoted title, input-range alias,
/// blank lines, an extension keyword, and CRLF line endings.
const RESOLVE_3D: &str = "# Created by: DaVinci Resolve\r\n\
TITLE \"Resolve Warm\"\r\n\
LUT_3D_SIZE 2\r\n\
LUT_3D_INPUT_RANGE 0.0000000000 1.0000000000\r\n\
LUT_IN_VIDEO_RANGE\r\n\
\r\n\
0.000000 0.000000 0.000000\r\n\
1.000000 0.000000 0.000000\r\n\
0.000000 1.000000 0.000000\r\n\
1.000000 1.000000 0.000000\r\n\
0.000000 0.000000 0.800000\r\n\
1.000000 0.000000 0.800000\r\n\
0.000000 1.000000 0.800000\r\n\
1.000000 1.000000 0.800000\r\n";

/// Adobe Cube LUT Specification 1.0 style 1D LUT with explicit domain.
const ADOBE_1D: &str = "TITLE \"Adobe Invert\"\n\
# comment line\n\
DOMAIN_MIN 0.0 0.0 0.0\n\
DOMAIN_MAX 2.0 2.0 2.0\n\
LUT_1D_SIZE 3\n\
1.0 1.0 1.0   # inline comment\n\
0.5 0.5 0.5\n\
0.0 0.0 0.0\n";

fn header_3d(edge: usize) -> String {
    format!("LUT_3D_SIZE {edge}\n")
}

fn identity_rows(edge: usize) -> String {
    let mut rows = String::new();
    #[allow(clippy::cast_precision_loss)]
    let value = |index: usize| index as f32 / (edge - 1) as f32;
    for b in 0..edge {
        for g in 0..edge {
            for r in 0..edge {
                writeln!(rows, "{} {} {}", value(r), value(g), value(b)).unwrap();
            }
        }
    }
    rows
}

fn error(source: &str) -> CubeError {
    parse_cube(source).unwrap_err()
}

#[test]
fn parses_a_resolve_style_3d_lut() {
    let (lut, warnings) = parse_cube(RESOLVE_3D).unwrap();
    assert_eq!(lut.title(), Some("Resolve Warm"));
    assert_eq!(lut.kind(), CubeKind::ThreeD);
    assert_eq!(lut.size(), 2);
    assert_eq!(lut.domain_min(), [0.0; 3]);
    assert_eq!(lut.domain_max(), [1.0; 3]);
    assert_eq!(lut.entries().len(), 8);
    assert_eq!(
        warnings,
        [CubeWarning::UnknownKeyword {
            line: 5,
            keyword: "LUT_IN_VIDEO_RANGE".to_owned(),
        }]
    );
    assert_ne!(warnings[0].to_string(), "");
    assert_eq!(lut.sample([1.0, 1.0, 1.0]), [1.0, 1.0, 0.8]);
    assert_eq!(lut.sample([0.5, 0.25, 0.5]), [0.5, 0.25, 0.4]);
    // Inputs are clamped to the domain.
    assert_eq!(lut.sample([-1.0, 2.0, 9.0]), [0.0, 1.0, 0.8]);
}

#[test]
fn parses_an_adobe_style_1d_lut_with_domain() {
    let (lut, warnings) = parse_cube(ADOBE_1D).unwrap();
    assert_eq!(warnings, []);
    assert_eq!(lut.title(), Some("Adobe Invert"));
    assert_eq!(lut.kind(), CubeKind::OneD);
    assert_eq!(lut.size(), 3);
    assert_eq!(lut.domain_max(), [2.0; 3]);
    assert_eq!(lut.sample([0.0, 1.0, 2.0]), [1.0, 0.5, 0.0]);
    assert_eq!(lut.sample([0.5, 1.5, 4.0]), [0.75, 0.25, 0.0]);
}

#[test]
fn input_range_alias_sets_every_channel_and_1d_alias_is_accepted() {
    let source = "LUT_1D_SIZE 2\nLUT_1D_INPUT_RANGE -0.5 1.5\n0 0 0\n1 1 1\n";
    let (lut, _) = parse_cube(source).unwrap();
    assert_eq!(lut.domain_min(), [-0.5; 3]);
    assert_eq!(lut.domain_max(), [1.5; 3]);
}

#[test]
fn accepts_bounds_inclusively() {
    let (lut, _) = parse_cube(&(header_3d(2) + &identity_rows(2))).unwrap();
    assert_eq!(lut.size(), 2);
    let large = header_3d(65) + &identity_rows(65);
    assert!(large.len() <= MAX_CUBE_INPUT_BYTES);
    let (lut, _) = parse_cube(&large).unwrap();
    assert_eq!(lut.entries().len(), 65 * 65 * 65);
    assert_eq!(
        lut.sample([0.3, 0.6, 0.9]).map(|v| (v * 1e4).round()),
        [3000.0, 6000.0, 9000.0]
    );

    let mut one_d = String::from("LUT_1D_SIZE 65536\n");
    for _ in 0..65_536 {
        one_d.push_str("0 0 0\n");
    }
    assert_eq!(parse_cube(&one_d).unwrap().0.size(), 65_536);
}

#[test]
fn rejects_oversized_input() {
    let mut source = String::from("LUT_1D_SIZE 2\n#");
    source.push_str(&" ".repeat(MAX_CUBE_INPUT_BYTES));
    assert_eq!(
        error(&source),
        CubeError::InputTooLarge {
            bytes: source.len()
        }
    );
}

#[test]
fn rejects_sizes_outside_bounds() {
    assert_eq!(
        error("LUT_3D_SIZE 1\n0 0 0\n"),
        CubeError::SizeOutOfRange {
            kind: CubeKind::ThreeD,
            size: 1
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 66\n"),
        CubeError::SizeOutOfRange {
            kind: CubeKind::ThreeD,
            size: 66
        }
    );
    assert_eq!(
        error("LUT_1D_SIZE 1\n"),
        CubeError::SizeOutOfRange {
            kind: CubeKind::OneD,
            size: 1
        }
    );
    assert_eq!(
        error("LUT_1D_SIZE 65537\n"),
        CubeError::SizeOutOfRange {
            kind: CubeKind::OneD,
            size: 65_537
        }
    );
}

#[test]
fn rejects_malformed_and_duplicate_headers() {
    assert_eq!(
        error("LUT_3D_SIZE two\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "LUT_3D_SIZE"
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 2 2\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "LUT_3D_SIZE"
        }
    );
    assert_eq!(
        error("LUT_1D_SIZE 99999999999999999999999\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "LUT_1D_SIZE"
        }
    );
    assert_eq!(
        error("TITLE unquoted\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "TITLE"
        }
    );
    assert_eq!(
        error("TITLE \"a\"\nTITLE \"b\"\n"),
        CubeError::DuplicateKeyword {
            line: 2,
            keyword: "TITLE"
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 2\nLUT_3D_SIZE 2\n"),
        CubeError::DuplicateKeyword {
            line: 2,
            keyword: "LUT_3D_SIZE"
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 2\nLUT_1D_SIZE 2\n"),
        CubeError::ConflictingSize { line: 2 }
    );
    assert_eq!(
        error("DOMAIN_MIN 0 0\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "DOMAIN_MIN"
        }
    );
    assert_eq!(
        error("DOMAIN_MAX 1 1 1 1\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "DOMAIN_MAX"
        }
    );
    assert_eq!(
        error("LUT_3D_INPUT_RANGE 0\n"),
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "LUT_3D_INPUT_RANGE"
        }
    );
    assert_eq!(
        error("DOMAIN_MIN 0 0 0\nDOMAIN_MIN 0 0 0\n"),
        CubeError::DuplicateKeyword {
            line: 2,
            keyword: "DOMAIN_MIN/DOMAIN_MAX/LUT_*_INPUT_RANGE"
        }
    );
    assert_eq!(
        error("DOMAIN_MAX 1 1 1\nLUT_3D_INPUT_RANGE 0 1\n"),
        CubeError::DuplicateKeyword {
            line: 2,
            keyword: "DOMAIN_MIN/DOMAIN_MAX/LUT_*_INPUT_RANGE"
        }
    );
    assert_eq!(
        error("LUT_1D_INPUT_RANGE 0 1\nDOMAIN_MIN 0 0 0\n"),
        CubeError::DuplicateKeyword {
            line: 2,
            keyword: "DOMAIN_MIN/DOMAIN_MAX/LUT_*_INPUT_RANGE"
        }
    );
}

#[test]
fn rejects_invalid_domains() {
    let rows = identity_rows(2);
    assert_eq!(
        error(&format!(
            "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0.5 0\nDOMAIN_MAX 1 0.5 1\n{rows}"
        )),
        CubeError::InvalidDomain
    );
    assert_eq!(
        error(&format!("LUT_3D_SIZE 2\nDOMAIN_MIN 2 0 0\n{rows}")),
        CubeError::InvalidDomain
    );
    assert_eq!(
        error(&format!("LUT_3D_SIZE 2\nDOMAIN_MAX inf 1 1\n{rows}")),
        CubeError::InvalidDomain
    );
    assert_eq!(
        error(&format!("LUT_3D_SIZE 2\nLUT_3D_INPUT_RANGE 1 0\n{rows}")),
        CubeError::InvalidDomain
    );
}

#[test]
fn rejects_malformed_data() {
    assert_eq!(error("0 0 0\n"), CubeError::DataBeforeSize { line: 1 });
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 0\n1 1 1\n"),
        CubeError::MalformedRow { line: 2 }
    );
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 0 0 0\n1 1 1\n"),
        CubeError::MalformedRow { line: 2 }
    );
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 zero 0\n1 1 1\n"),
        CubeError::MalformedRow { line: 2 }
    );
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 NaN 0\n1 1 1\n"),
        CubeError::NonFiniteValue { line: Some(2) }
    );
    // A non-finite first token is still a data row, never an unknown keyword.
    assert_eq!(
        error("LUT_1D_SIZE 2\ninf 0 0\n1 1 1\n"),
        CubeError::NonFiniteValue { line: Some(2) }
    );
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 0 0\nTITLE \"late\"\n1 1 1\n"),
        CubeError::KeywordAfterData { line: 3 }
    );
    assert_eq!(
        error("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n1 1 1\n"),
        CubeError::TooManyEntries {
            line: 4,
            expected: 2
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 2\n0 0 0\n"),
        CubeError::EntryCount {
            expected: 8,
            actual: 1
        }
    );
    assert_eq!(
        error("LUT_3D_SIZE 2\n"),
        CubeError::EntryCount {
            expected: 8,
            actual: 0
        }
    );
    assert_eq!(error("TITLE \"x\"\n# no size\n"), CubeError::MissingSize);
    assert_eq!(error(""), CubeError::MissingSize);
}

#[test]
fn every_error_has_a_message() {
    let errors = [
        CubeError::InputTooLarge { bytes: 1 },
        CubeError::MalformedKeyword {
            line: 1,
            keyword: "TITLE",
        },
        CubeError::DuplicateKeyword {
            line: 1,
            keyword: "TITLE",
        },
        CubeError::ConflictingSize { line: 1 },
        CubeError::KeywordAfterData { line: 1 },
        CubeError::DataBeforeSize { line: 1 },
        CubeError::MalformedRow { line: 1 },
        CubeError::NonFiniteValue { line: Some(1) },
        CubeError::NonFiniteValue { line: None },
        CubeError::TooManyEntries {
            line: 1,
            expected: 2,
        },
        CubeError::MissingSize,
        CubeError::SizeOutOfRange {
            kind: CubeKind::OneD,
            size: 1,
        },
        CubeError::InvalidDomain,
        CubeError::EntryCount {
            expected: 1,
            actual: 2,
        },
    ];
    for error in errors {
        assert_ne!(error.to_string(), "");
    }
}

#[test]
fn programmatic_constructors_validate() {
    assert_eq!(
        CubeLut::new_3d(2, vec![[0.0; 3]; 7], [0.0; 3], [1.0; 3]).unwrap_err(),
        CubeError::EntryCount {
            expected: 8,
            actual: 7
        }
    );
    assert_eq!(
        CubeLut::new_3d(2, vec![[f32::NAN; 3]; 8], [0.0; 3], [1.0; 3]).unwrap_err(),
        CubeError::NonFiniteValue { line: None }
    );
    assert_eq!(
        CubeLut::new_1d(vec![[0.0; 3]], [0.0; 3], [1.0; 3]).unwrap_err(),
        CubeError::SizeOutOfRange {
            kind: CubeKind::OneD,
            size: 1
        }
    );
    assert_eq!(
        CubeLut::new_1d(vec![[0.0; 3]; 2], [1.0; 3], [1.0; 3]).unwrap_err(),
        CubeError::InvalidDomain
    );
    let lut = CubeLut::new_1d(vec![[0.0; 3], [1.0; 3]], [0.0; 3], [1.0; 3]).unwrap();
    assert_eq!(lut.title(), None);
    assert_eq!(lut.clone(), lut);
}
