//! The strict `.fmpreset` and `.fmgrade` files that share animation presets
//! and grades between projects at schema 34 (GX-011, SW-019): exact
//! documents, embedded and sibling LUTs, and refusals.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_animation::{
    Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Rgba, Timebase,
    Track, TrackKey, Value,
};
use fm_grade::{Grade, GradeLut};
use fm_preset_file::{
    AnimationPresetFile, ExportedGradeLut, GradeExportLut, GradeFileLut, GradeLutSource,
    GradePresetFile, MAX_EMBEDDED_LUT_BYTES, PresetFileError, decode_animation_preset_file,
    decode_grade_preset_file, encode_animation_preset_file, encode_grade_preset_file,
    read_animation_preset_file, read_grade_preset_file, sibling_lut_name,
    write_animation_preset_file, write_grade_preset_file,
};
use fm_types::FrameRate;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-preset-file-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const INVERT_CUBE: &str = "TITLE \"Invert\"\nLUT_3D_SIZE 2\n\
1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

/// A 15-frame slide from x -400 with a fading colour, against 400x100.
fn motion() -> PhaseMotion {
    let track = |property| TrackKey::new(MotionTarget, Phase::In, property);
    let clip = MotionClip::from_tracks(
        Timebase::new(FrameRate::new(30, 1).unwrap()),
        15,
        0,
        vec![
            Track::new(
                track(Property::PositionX),
                vec![
                    Keyframe::new(0, Value::Scalar(-400), Easing::CubicOut),
                    Keyframe::new(15, Value::Scalar(0), Easing::Linear),
                ],
            ),
            Track::new(
                track(Property::Color),
                vec![Keyframe::new(
                    0,
                    Value::Color(Rgba::new(255, 0, 16, 255)),
                    Easing::Step,
                )],
            ),
        ],
    )
    .unwrap();
    PhaseMotion::new(clip, 400, 100).unwrap()
}

const MOTION_BODY: &str = "\"phase\": \"out\", \"timebase\": \"30/1\", \"frames\": 15, \
\"width\": 400, \"height\": 100, \"tracks\": [{\"property\": \"position_x\", \"keys\": \
[{\"frame\": 0, \"value\": -400, \"easing\": \"cubic-out\"}, {\"frame\": 15, \"value\": 0, \
\"easing\": \"linear\"}]}, {\"property\": \"color\", \"keys\": [{\"frame\": 0, \"value\": \
\"#ff0010ff\", \"easing\": \"step\"}]}]";

#[test]
fn animation_preset_files_are_exact_and_strict() {
    let directory = TestDirectory::new("fmpreset");
    let file = AnimationPresetFile {
        name: "Swoosh".to_owned(),
        phase: Phase::Out,
        motion: motion(),
    };
    let encoded = encode_animation_preset_file(&file).unwrap();
    assert_eq!(
        encoded,
        format!(
            "{{\"schema_version\": 39, \"kind\": \"animation_preset\", \"name\": \"Swoosh\", \
             {MOTION_BODY}}}\n"
        )
    );
    assert_eq!(decode_animation_preset_file(&encoded).unwrap(), file);
    let path = directory.path("swoosh.fmpreset");
    write_animation_preset_file(&path, &file).unwrap();
    assert_eq!(read_animation_preset_file(&path).unwrap(), file);

    for schema in [38, 40] {
        let other = encoded.replacen(
            "\"schema_version\": 39",
            &format!("\"schema_version\": {schema}"),
            1,
        );
        assert!(matches!(
            decode_animation_preset_file(&other),
            Err(PresetFileError::UnsupportedSchema {
                found,
                supported: 39
            }) if found == schema
        ));
    }
    assert!(matches!(
        decode_animation_preset_file(&encoded.replacen("animation_preset", "grade", 1)),
        Err(PresetFileError::WrongKind {
            expected: "animation_preset",
            ..
        })
    ));
    for (from, to) in [
        ("\"frames\": 15,", "\"frames\": 15, \"id\": 1,"),
        ("\"name\": \"Swoosh\"", "\"name\": \" \""),
        ("\"phase\": \"out\"", "\"phase\": \"hold\""),
        ("\"width\": 400, ", ""),
    ] {
        assert!(
            decode_animation_preset_file(&encoded.replacen(from, to, 1)).is_err(),
            "{from} -> {to}"
        );
    }
    assert!(
        encode_animation_preset_file(&AnimationPresetFile {
            name: String::new(),
            ..file.clone()
        })
        .is_err()
    );
    let large = directory.path("large.fmpreset");
    fs::write(&large, " ".repeat(256 * 1024 + 1)).unwrap();
    assert!(matches!(
        read_animation_preset_file(&large),
        Err(PresetFileError::TooLarge { .. })
    ));
}

fn warm() -> Grade {
    let mut grade = Grade::IDENTITY;
    grade.saturation_permille = 1_300;
    grade
}

#[test]
#[allow(clippy::too_many_lines)] // one golden: embedded, sibling, missing, refusals
fn grade_files_embed_small_luts_and_place_large_ones_beside() {
    let directory = TestDirectory::new("fmgrade");
    let mut referencing = warm();
    referencing.lut = Some(GradeLut::new("asset://luts/7.cube", 600));

    // A small LUT travels inside the file; the project reference does not.
    let path = directory.path("warm.fmgrade");
    let exported = write_grade_preset_file(
        &path,
        "Warm",
        &referencing,
        Some(GradeExportLut {
            name: "Invert",
            strength_permille: 600,
            text: Some(INVERT_CUBE),
        }),
    )
    .unwrap();
    assert_eq!(exported, Some(ExportedGradeLut::Embedded));
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("{\"schema_version\": 39, \"kind\": \"grade\", \"name\": \"Warm\""));
    let decoded = decode_grade_preset_file(&text).unwrap();
    assert_eq!(
        decoded,
        GradePresetFile {
            name: "Warm".to_owned(),
            grade: warm(),
            lut: Some(GradeFileLut {
                name: "Invert".to_owned(),
                strength_permille: 600,
                source: GradeLutSource::Embedded(INVERT_CUBE.to_owned()),
            }),
        }
    );
    assert_eq!(encode_grade_preset_file(&decoded).unwrap(), text);
    let loaded = read_grade_preset_file(&path).unwrap();
    assert_eq!(loaded.grade, warm());
    assert_eq!(loaded.lut.unwrap().text, INVERT_CUBE);

    // A large LUT goes to the sibling `.cube`, which the reader loads.
    let mut large = INVERT_CUBE.to_owned();
    while large.len() <= MAX_EMBEDDED_LUT_BYTES {
        large.insert_str(
            0,
            "# padding comment to grow the file past the embed limit\n",
        );
    }
    let big = directory.path("big look.fmgrade");
    assert_eq!(sibling_lut_name(&big), "big look.cube");
    let exported = write_grade_preset_file(
        &big,
        "Big",
        &referencing,
        Some(GradeExportLut {
            name: "Invert",
            strength_permille: 600,
            text: Some(&large),
        }),
    )
    .unwrap();
    assert_eq!(
        exported,
        Some(ExportedGradeLut::Sibling {
            path: directory.path("big look.cube"),
            written: true,
        })
    );
    assert!(
        fs::read_to_string(&big)
            .unwrap()
            .contains("\"file\": \"big look.cube\"")
    );
    assert_eq!(
        read_grade_preset_file(&big).unwrap().lut.unwrap().text,
        large
    );

    // An unreadable source names the sibling without writing it; reading
    // then fails until the operator supplies it.
    let missing = directory.path("missing.fmgrade");
    let exported = write_grade_preset_file(
        &missing,
        "Missing",
        &warm(),
        Some(GradeExportLut {
            name: "Gone",
            strength_permille: 1_000,
            text: None,
        }),
    )
    .unwrap();
    assert_eq!(
        exported,
        Some(ExportedGradeLut::Sibling {
            path: directory.path("missing.cube"),
            written: false,
        })
    );
    assert!(read_grade_preset_file(&missing).is_err());
    fs::write(directory.path("missing.cube"), INVERT_CUBE).unwrap();
    assert_eq!(
        read_grade_preset_file(&missing).unwrap().lut.unwrap().text,
        INVERT_CUBE
    );

    // Strict refusals: path-like sibling names, a LUT reference in the
    // grade, both or neither LUT sources, an embedded LUT that does not
    // parse, and the wrong kind or schema.
    let sibling = fs::read_to_string(&missing).unwrap();
    for name in ["../escape.cube", "dir/x.cube", ".cube", "x.txt"] {
        let refused = sibling.replacen("missing.cube", name, 1);
        assert!(decode_grade_preset_file(&refused).is_err(), "{name}");
    }
    let lut_null = "\"lut\": null";
    assert!(text.contains(lut_null));
    for (from, to) in [
        (
            lut_null,
            "\"lut\": {\"asset_uri\": \"asset://luts/7.cube\", \"strength_permille\": 600}",
        ),
        ("\"cube\": \"", "\"file\": \"x.cube\", \"cube\": \""),
        ("LUT_3D_SIZE 2", "LUT_3D_SIZE 9"),
        ("\"kind\": \"grade\"", "\"kind\": \"animation_preset\""),
        ("\"schema_version\": 39", "\"schema_version\": 38"),
        ("\"schema_version\": 39", "\"schema_version\": 40"),
    ] {
        assert!(
            decode_grade_preset_file(&text.replacen(from, to, 1)).is_err(),
            "{from} -> {to}"
        );
    }
}

/// A path that names something other than a regular file (here a FIFO, as
/// a hostile sibling `.cube` could) is refused without opening it, so a
/// read never blocks waiting for a writer.
#[cfg(unix)]
#[test]
fn non_regular_files_are_refused_without_blocking() {
    let directory = TestDirectory::new("fifo");
    let path = directory.path("pipe.fmpreset");
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    let (sender, receiver) = std::sync::mpsc::channel();
    let read = path.clone();
    std::thread::spawn(move || {
        let _ = sender.send(read_animation_preset_file(&read).is_err());
    });
    let refused = receiver.recv_timeout(std::time::Duration::from_secs(5));
    if refused.is_err() {
        // Unblock the stuck reader before failing.
        let _ = fs::OpenOptions::new().write(true).open(&path);
    }
    assert_eq!(refused, Ok(true), "reading a FIFO must not block");
}
