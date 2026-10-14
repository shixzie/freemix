use crate::model::{
    Contrast, Curve, CurvePoint, Grade, HueCurves, RgbCurves, Wheel, Wheels, WhiteBalance,
};

/// A built-in grade with a stable identifier.
///
/// Identifiers are persisted by callers and never change meaning; display
/// names may be localized by the UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub grade: Grade,
}

const FADED_MASTER: [CurvePoint; 2] = [CurvePoint::new(0, 600), CurvePoint::new(10_000, 9400)];

/// Built-in presets in display order.
pub static BUILTIN_PRESETS: [GradePreset; 6] = [
    GradePreset {
        id: "neutral",
        name: "Neutral",
        grade: Grade::IDENTITY,
    },
    GradePreset {
        id: "warm",
        name: "Warm",
        grade: Grade {
            white_balance: WhiteBalance::new(250, -50),
            saturation_permille: 1050,
            ..Grade::IDENTITY
        },
    },
    GradePreset {
        id: "cool",
        name: "Cool",
        grade: Grade {
            white_balance: WhiteBalance::new(-250, 50),
            ..Grade::IDENTITY
        },
    },
    GradePreset {
        id: "punchy",
        name: "Punchy",
        grade: Grade {
            contrast: Contrast::new(250, 5000),
            saturation_permille: 1300,
            ..Grade::IDENTITY
        },
    },
    GradePreset {
        id: "faded",
        name: "Faded",
        grade: Grade {
            enabled: true,
            white_balance: WhiteBalance::NEUTRAL,
            exposure_millistops: 0,
            contrast: Contrast::new(-150, 5000),
            saturation_permille: 750,
            wheels: Wheels {
                lift: Wheel::new(0, 0, 100),
                ..Wheels::NEUTRAL
            },
            curves: RgbCurves {
                master: Curve::from_static(&FADED_MASTER),
                red: Curve::IDENTITY,
                green: Curve::IDENTITY,
                blue: Curve::IDENTITY,
            },
            hue_curves: HueCurves::IDENTITY,
            lut: None,
        },
    },
    GradePreset {
        id: "bw",
        name: "B&W",
        grade: Grade {
            contrast: Contrast::new(100, 5000),
            saturation_permille: 0,
            ..Grade::IDENTITY
        },
    },
];

/// Looks up a built-in preset by its stable identifier.
#[must_use]
pub fn builtin_preset(id: &str) -> Option<&'static GradePreset> {
    BUILTIN_PRESETS.iter().find(|preset| preset.id == id)
}
