//! Wording for the colour panel from the shared Studio catalog ([`Msg`]):
//! control names and hover help, values in operator units with the
//! locale's number format, and refusals keyed by
//! [`GradeEditorError::msg_key`].

use fm_grade::{CurveChannel, HueCurveKind, WheelKind};
use fm_ui_model::grade_editor::{
    CompareMode, CurveTarget, GradeEditorError, GradeParam, ScopeKind,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

/// The sentence for a core error id in `locale`, or `None` for an unknown
/// id.
#[must_use]
pub fn error_message(locale: Locale, msg_key: &str) -> Option<&'static str> {
    let msg = match msg_key {
        "grade-editor-invalid" => Msg::GradeErrInvalid,
        "grade-editor-nothing-to-undo" => Msg::AnimErrNothingToUndo,
        "grade-editor-nothing-to-redo" => Msg::AnimErrNothingToRedo,
        "grade-editor-empty-clipboard" => Msg::GradeErrEmptyClipboard,
        "grade-editor-empty-slot" => Msg::GradeErrEmptySlot,
        "grade-editor-gesture-active" => Msg::AnimErrDragActive,
        "grade-editor-no-gesture" => Msg::AnimErrNoDrag,
        "grade-editor-curve-full" => Msg::GradeErrCurveFull,
        "grade-editor-point-taken" => Msg::GradeErrPointTaken,
        "grade-editor-endpoint-locked" => Msg::GradeErrEndpointLocked,
        "grade-editor-unknown-point" => Msg::GradeErrUnknownPoint,
        _ => return None,
    };
    Some(locale.text(msg))
}

/// The full inline message for `error`.
#[must_use]
pub fn describe_error(locale: Locale, error: &GradeEditorError) -> String {
    let message =
        error_message(locale, error.msg_key()).unwrap_or_else(|| locale.text(Msg::AnimErrRefused));
    let message = ("message", Arg::Str(message));
    match error {
        GradeEditorError::CurveFull { max } => locale.format(
            Msg::GradeErrMaxPoints,
            &[
                message,
                ("max", Arg::from(u64::try_from(*max).unwrap_or(0))),
            ],
        ),
        _ => locale.format(Msg::AnimErrSentence, &[message]),
    }
}

/// Display name of a scalar control.
#[must_use]
pub fn param_name(locale: Locale, param: GradeParam) -> &'static str {
    locale.text(match param {
        GradeParam::Temperature => Msg::GradeTemperature,
        GradeParam::Tint => Msg::GradeTint,
        GradeParam::Exposure => Msg::GradeExposure,
        GradeParam::Contrast => Msg::GradeContrast,
        GradeParam::Pivot => Msg::GradePivot,
        GradeParam::Saturation => Msg::GradeSaturation,
        GradeParam::WheelMaster(_) => Msg::GradeMaster,
        GradeParam::LutStrength => Msg::GradeStrength,
    })
}

/// What a scalar control does.
#[must_use]
pub fn param_hover(locale: Locale, param: GradeParam) -> &'static str {
    locale.text(match param {
        GradeParam::Temperature => Msg::GradeTemperatureHover,
        GradeParam::Tint => Msg::GradeTintHover,
        GradeParam::Exposure => Msg::GradeExposureHover,
        GradeParam::Contrast => Msg::GradeContrastHover,
        GradeParam::Pivot => Msg::GradePivotHover,
        GradeParam::Saturation => Msg::GradeSaturationHover,
        GradeParam::WheelMaster(WheelKind::Lift) => Msg::GradeLiftMasterHover,
        GradeParam::WheelMaster(WheelKind::Gamma) => Msg::GradeGammaMasterHover,
        GradeParam::WheelMaster(WheelKind::Gain) => Msg::GradeGainMasterHover,
        GradeParam::WheelMaster(WheelKind::Offset) => Msg::GradeOffsetMasterHover,
        GradeParam::LutStrength => Msg::GradeStrengthHover,
    })
}

/// A stored control value in operator units: stops for exposure,
/// percentages for contrast, pivot, saturation, and strength, and `±100`
/// steps for white balance and wheel masters.
#[must_use]
pub fn format_param(locale: Locale, param: GradeParam, value: i32) -> String {
    let signed_percent = |value: i32| {
        let text = locale.percent(i64::from(value));
        if value > 0 { format!("+{text}") } else { text }
    };
    // Neutral reads without a sign; changes always carry one.
    let signed = |raw: i32, shown: f64, places: usize| {
        if raw == 0 {
            locale.decimal(shown, places)
        } else {
            locale.signed_decimal(shown, places)
        }
    };
    match param {
        GradeParam::Exposure => format!("{} EV", signed(value, f64::from(value) / 1000.0, 2)),
        GradeParam::Contrast => signed_percent(round_div(value, 10)),
        GradeParam::Pivot => locale.percent(i64::from(round_div(value, 100))),
        GradeParam::Saturation | GradeParam::LutStrength => {
            locale.percent(i64::from(round_div(value, 10)))
        }
        GradeParam::Temperature | GradeParam::Tint | GradeParam::WheelMaster(_) => {
            let steps = round_div(value, 10);
            signed(steps, f64::from(steps), 0)
        }
    }
}

const fn round_div(value: i32, divisor: i32) -> i32 {
    let half = divisor / 2;
    if value >= 0 {
        (value + half) / divisor
    } else {
        (value - half) / divisor
    }
}

/// The hover text of a slider: what it does, its value, and its gestures.
#[must_use]
pub fn slider_hover(locale: Locale, param: GradeParam, value: i32) -> String {
    format!(
        "{}\n{}",
        param_hover(locale, param),
        locale.format(
            Msg::GradeSliderHelp,
            &[
                ("value", Arg::Str(&format_param(locale, param, value))),
                (
                    "default",
                    Arg::Str(&format_param(locale, param, param.default_value())),
                ),
            ],
        )
    )
}

/// Display name of a wheel.
#[must_use]
pub fn wheel_name(locale: Locale, kind: WheelKind) -> &'static str {
    locale.text(match kind {
        WheelKind::Lift => Msg::GradeLift,
        WheelKind::Gamma => Msg::GradeGamma,
        WheelKind::Gain => Msg::GradeGain,
        WheelKind::Offset => Msg::GradeOffset,
    })
}

/// What a wheel does.
#[must_use]
pub fn wheel_hover(locale: Locale, kind: WheelKind) -> &'static str {
    locale.text(match kind {
        WheelKind::Lift => Msg::GradeLiftHover,
        WheelKind::Gamma => Msg::GradeGammaHover,
        WheelKind::Gain => Msg::GradeGainHover,
        WheelKind::Offset => Msg::GradeOffsetHover,
    })
}

/// A wheel's direction as an angle (degrees, counter-clockwise from the
/// `+Cb` axis) and a strength percentage.
#[must_use]
pub fn wheel_reading(x: i16, y: i16) -> Option<(i64, i64)> {
    if x == 0 && y == 0 {
        return None;
    }
    let (x, y) = (f64::from(x), f64::from(y));
    let angle = y.atan2(x).to_degrees().rem_euclid(360.0);
    #[allow(clippy::cast_possible_truncation)] // Both are small and finite.
    Some((
        angle.round() as i64 % 360,
        (x.hypot(y) / 10.0).round() as i64,
    ))
}

/// The help line under a wheel's description.
#[must_use]
pub fn wheel_help(locale: Locale, x: i16, y: i16) -> String {
    match wheel_reading(x, y) {
        None => locale.text(Msg::GradeWheelNeutralHelp).to_owned(),
        Some((angle, amount)) => locale.format(
            Msg::GradeWheelHelp,
            &[
                ("angle", Arg::Str(&locale.signed_integer(angle))),
                ("amount", Arg::Str(&locale.percent(amount))),
            ],
        ),
    }
}

/// Tab label of a curve.
#[must_use]
pub fn curve_name(locale: Locale, target: CurveTarget) -> &'static str {
    locale.text(match target {
        CurveTarget::Rgb(CurveChannel::Master) => Msg::GradeCurveMaster,
        CurveTarget::Rgb(CurveChannel::Red) => Msg::ColourRed,
        CurveTarget::Rgb(CurveChannel::Green) => Msg::ColourGreen,
        CurveTarget::Rgb(CurveChannel::Blue) => Msg::ColourBlue,
        CurveTarget::Hue(HueCurveKind::HueVsHue) => Msg::GradeHueVsHue,
        CurveTarget::Hue(HueCurveKind::HueVsSaturation) => Msg::GradeHueVsSat,
        CurveTarget::Hue(HueCurveKind::LumaVsSaturation) => Msg::GradeLumaVsSat,
    })
}

/// What a curve does.
#[must_use]
pub fn curve_hover(locale: Locale, target: CurveTarget) -> &'static str {
    locale.text(match target {
        CurveTarget::Rgb(CurveChannel::Master) => Msg::GradeCurveMasterHover,
        CurveTarget::Rgb(CurveChannel::Red) => Msg::GradeCurveRedHover,
        CurveTarget::Rgb(CurveChannel::Green) => Msg::GradeCurveGreenHover,
        CurveTarget::Rgb(CurveChannel::Blue) => Msg::GradeCurveBlueHover,
        CurveTarget::Hue(HueCurveKind::HueVsHue) => Msg::GradeHueVsHueHover,
        CurveTarget::Hue(HueCurveKind::HueVsSaturation) => Msg::GradeHueVsSatHover,
        CurveTarget::Hue(HueCurveKind::LumaVsSaturation) => Msg::GradeLumaVsSatHover,
    })
}

/// Display name of a built-in look by its stable id.
#[must_use]
pub fn preset_name(locale: Locale, id: &str) -> Option<&'static str> {
    preset_msgs(id).map(|(name, _)| locale.text(name))
}

/// What a built-in look does, by its stable id.
#[must_use]
pub fn preset_hover(locale: Locale, id: &str) -> Option<&'static str> {
    preset_msgs(id).map(|(_, hover)| locale.text(hover))
}

fn preset_msgs(id: &str) -> Option<(Msg, Msg)> {
    Some(match id {
        "neutral" => (Msg::GradePresetNeutral, Msg::GradePresetNeutralHover),
        "warm" => (Msg::GradePresetWarm, Msg::GradePresetWarmHover),
        "cool" => (Msg::GradePresetCool, Msg::GradePresetCoolHover),
        "punchy" => (Msg::GradePresetPunchy, Msg::GradePresetPunchyHover),
        "faded" => (Msg::GradePresetFaded, Msg::GradePresetFadedHover),
        "bw" => (Msg::GradePresetBw, Msg::GradePresetBwHover),
        _ => return None,
    })
}

/// Tab label of a scope.
#[must_use]
pub fn scope_name(locale: Locale, kind: ScopeKind) -> &'static str {
    locale.text(match kind {
        ScopeKind::Waveform => Msg::GradeScopeWaveform,
        ScopeKind::Parade => Msg::GradeScopeParade,
        ScopeKind::Vectorscope => Msg::GradeScopeVectorscope,
        ScopeKind::Histogram => Msg::GradeScopeHistogram,
    })
}

/// How to read a scope.
#[must_use]
pub fn scope_hover(locale: Locale, kind: ScopeKind) -> &'static str {
    locale.text(match kind {
        ScopeKind::Waveform => Msg::GradeScopeWaveformHover,
        ScopeKind::Parade => Msg::GradeScopeParadeHover,
        ScopeKind::Vectorscope => Msg::GradeScopeVectorscopeHover,
        ScopeKind::Histogram => Msg::GradeScopeHistogramHover,
    })
}

/// Tab label of a compare mode.
#[must_use]
pub fn compare_name(locale: Locale, mode: CompareMode) -> &'static str {
    locale.text(match mode {
        CompareMode::Off => Msg::GradeCompareOff,
        CompareMode::Wipe => Msg::TransitionWipe,
        CompareMode::SideBySide => Msg::GradeCompareSide,
    })
}

/// What a compare mode shows.
#[must_use]
pub fn compare_hover(locale: Locale, mode: CompareMode) -> &'static str {
    locale.text(match mode {
        CompareMode::Off => Msg::GradeCompareOffHover,
        CompareMode::Wipe => Msg::GradeCompareWipeHover,
        CompareMode::SideBySide => Msg::GradeCompareSideHover,
    })
}
